//! Инкрементальный индексатор: walk источника → normalize (LineEvent) → SQLite.
//!
//! Ядро не знает формата транскриптов — оно ходит по файлам источника, зовёт
//! [`DataSource::parse_line`] и пишет нормализованные строки. Claude/Codex/OpenCode —
//! разные `impl DataSource` (см. [`crate::source`]).

pub mod schema;

use crate::pricing;
use crate::source::DataSource;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use walkdir::WalkDir;

pub fn db_path() -> std::path::PathBuf {
    // QUICKAI_DB переопределяет путь к БД (напр. для демо/тестов).
    if let Ok(p) = std::env::var("QUICKAI_DB") {
        return std::path::PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join(".claude/quickai.db")
}

pub fn open_db() -> Result<Connection> {
    let conn = Connection::open(db_path())?;
    schema::init(&conn)?;
    Ok(conn)
}

pub struct IndexStats {
    pub files_scanned: usize,
    pub files_indexed: usize,
    pub turns_added: u64,
}

/// Полный проход по источнику. rebuild=true → снести данные ЭТОГО источника и перечитать.
pub fn run(conn: &mut Connection, rebuild: bool, source: &dyn DataSource) -> Result<IndexStats> {
    let root = source.root();
    let src = source.name();

    if rebuild {
        // Скоуп по источнику — не трогаем данные других источников.
        conn.execute("DELETE FROM turns WHERE source=?1", [src])?;
        conn.execute("DELETE FROM sessions WHERE source=?1", [src])?;
        conn.execute("DELETE FROM tool_calls WHERE source=?1", [src])?;
        conn.execute(
            "DELETE FROM files WHERE path LIKE ?1",
            [format!("{}%", root.to_string_lossy())],
        )?;
        // agent_runs/tasks пересобираются из turns в aggregate_tasks; aux-таблицы идемпотентны.
    }

    let mut stats = IndexStats { files_scanned: 0, files_indexed: 0, turns_added: 0 };

    let tx = conn.transaction()?;
    for entry in WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !source.owns(path) {
            continue;
        }
        stats.files_scanned += 1;

        let meta = std::fs::metadata(path)?;
        let size = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Инкрементальность: сколько байт прочитали + последний promptId (seed).
        let (prev_read, seed_prompt): (i64, Option<String>) = tx
            .query_row(
                "SELECT bytes_read, last_prompt_id FROM files
                 WHERE path=?1 AND mtime=?2 AND size>=?3",
                (path.to_string_lossy(), mtime, size as i64),
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0, None));
        if prev_read as u64 >= size {
            continue; // не менялся — пропуск
        }

        let project = source.project_of(path, &root);
        let (added, last_prompt) =
            index_file(&tx, source, path, &project, prev_read as u64, seed_prompt)?;
        stats.turns_added += added;
        stats.files_indexed += 1;

        tx.execute(
            "INSERT OR REPLACE INTO files(path,mtime,size,bytes_read,last_indexed,last_prompt_id)
             VALUES(?1,?2,?3,?4,strftime('%s','now'),?5)",
            rusqlite::params![path.to_string_lossy(), mtime, size as i64, size as i64, last_prompt],
        )?;
    }
    tx.commit()?;

    aggregate_tasks(conn)?;
    Ok(stats)
}

/// Прочитать хвост файла с офсета, вставить turn'ы через нормализованные [`LineEvent`].
/// promptId живёт только на user-строках → протягиваем вперёд на assistant-turn'ы.
/// Возврат: (число turn'ов, последний виденный promptId — seed для след. дочитывания).
fn index_file(
    conn: &Connection,
    source: &dyn DataSource,
    path: &Path,
    project: &str,
    from: u64,
    seed_prompt: Option<String>,
) -> Result<(u64, Option<String>)> {
    let mut f = std::fs::File::open(path).with_context(|| format!("open {path:?}"))?;
    f.seek(SeekFrom::Start(from))?;
    let reader = BufReader::new(&mut f as &mut dyn Read);

    let src = source.name();
    let agent_run_id = source.agent_run_id_of(path); // Some для субагентских файлов
    let mut count = 0u64;
    let mut cur_prompt = seed_prompt;

    for line in reader.lines() {
        let line = match line {
            Ok(l) if !l.trim().is_empty() => l,
            _ => continue,
        };
        let ev = match source.parse_line(&line) {
            Some(e) => e,
            None => continue, // битая строка — пропуск
        };

        // Линковка субагента.
        if let Some((aid, atype)) = &ev.agent_link {
            conn.execute(
                "INSERT OR REPLACE INTO agent_meta(id,agent_type) VALUES(?1,?2)",
                rusqlite::params![aid, atype],
            )?;
        }

        // tool_result → пометить is_error у соответствующего tool_call.
        for (tuid, err) in &ev.tool_results {
            if *err {
                conn.execute(
                    "UPDATE tool_calls SET is_error=1 WHERE tool_use_id=?1",
                    rusqlite::params![tuid],
                )?;
            }
        }

        // user-строка задаёт текущий promptId; текст промпта (если не шум).
        if let Some(pid) = &ev.prompt_id {
            cur_prompt = Some(pid.clone());
            if let Some(clean) = &ev.prompt_text {
                let trimmed: String = clean.chars().take(200).collect();
                conn.execute(
                    "INSERT OR IGNORE INTO prompt_text(prompt_id,text) VALUES(?1,?2)",
                    rusqlite::params![pid, trimmed],
                )?;
                // Для субагентского файла — первый промпт агента (смысл вместо hash-id).
                if let Some(arid) = &agent_run_id {
                    let snip: String = clean.chars().take(160).collect();
                    conn.execute(
                        "INSERT OR IGNORE INTO agent_prompt(id,text) VALUES(?1,?2)",
                        rusqlite::params![arid, snip],
                    )?;
                }
            }
        }

        // assistant-turn.
        let turn = match ev.turn {
            Some(t) => t,
            None => continue,
        };
        let cost = pricing::cost_of(&turn.model, &turn.usage);

        conn.execute(
            "INSERT INTO turns(prompt_id,session_id,project,source,agent_run_id,is_sidechain,model,ts,
               input_tokens,output_tokens,cache_write_5m,cache_write_1h,cache_read,
               web_search,web_fetch,cost_usd,stop_reason)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            rusqlite::params![
                cur_prompt,
                turn.session_id,
                project,
                src,
                agent_run_id,
                turn.is_sidechain as i64,
                turn.model,
                turn.ts,
                turn.usage.input as i64,
                turn.usage.output as i64,
                turn.usage.cache_write_5m as i64,
                turn.usage.cache_write_1h as i64,
                turn.usage.cache_read as i64,
                turn.usage.web_search as i64,
                turn.usage.web_fetch as i64,
                cost,
                turn.stop_reason,
            ],
        )?;

        // tool_use блоки этого turn'а → tool_calls (is_error проставит tool_result).
        for (tuid, name) in &turn.tool_uses {
            conn.execute(
                "INSERT OR IGNORE INTO tool_calls(tool_use_id,name,project,source,session_id,agent_run_id)
                 VALUES(?1,?2,?3,?4,?5,?6)",
                rusqlite::params![tuid, name, project, src, turn.session_id, agent_run_id],
            )?;
        }

        // upsert session (контекст берём с первого попавшегося turn'а сессии).
        if let Some(sid) = &turn.session_id {
            conn.execute(
                "INSERT OR IGNORE INTO sessions(session_id,project,source,cwd,git_branch,first_ts,last_ts,version)
                 VALUES(?1,?2,?3,?4,?5,?6,?6,?7)",
                rusqlite::params![
                    sid, project, src, turn.cwd, turn.git_branch, turn.ts, turn.version,
                ],
            )?;
            conn.execute(
                "UPDATE sessions SET last_ts=MAX(last_ts,?2), first_ts=MIN(first_ts,?2)
                 WHERE session_id=?1",
                rusqlite::params![sid, turn.ts],
            )?;
        }
        count += 1;
    }
    Ok((count, cur_prompt))
}

/// Пересобрать агрегаты tasks и agent_runs из turns (денормализация вверх).
fn aggregate_tasks(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DELETE FROM agent_runs;
        INSERT INTO agent_runs(id,prompt_id,session_id,project,source,agent_type,file_path,
                               first_ts,last_ts,turns,out_tokens,cost_usd)
        SELECT agent_run_id,
               MAX(prompt_id), MAX(session_id), MAX(project), MAX(source),
               '' AS agent_type, '' AS file_path,
               MIN(ts), MAX(ts), COUNT(*),
               SUM(output_tokens), SUM(cost_usd)
        FROM turns
        WHERE agent_run_id IS NOT NULL
        GROUP BY agent_run_id;

        -- subagent_type из линковки agent_meta.
        UPDATE agent_runs SET agent_type = COALESCE(
            (SELECT m.agent_type FROM agent_meta m WHERE m.id = agent_runs.id), '');

        -- промпт-сниппет агента.
        UPDATE agent_runs SET prompt = COALESCE(
            (SELECT p.text FROM agent_prompt p WHERE p.id = agent_runs.id), '');

        DELETE FROM tasks;
        INSERT INTO tasks(prompt_id,session_id,project,source,text,first_ts,last_ts,
                          wall_ms,cost_usd,out_tokens,total_tokens,agent_count)
        SELECT t.prompt_id,
               MAX(t.session_id),
               '' AS project,
               MAX(t.source),
               '' AS text,
               MIN(t.ts), MAX(t.ts),
               MAX(t.ts)-MIN(t.ts),
               SUM(t.cost_usd), SUM(t.output_tokens),
               SUM(t.input_tokens+t.output_tokens+t.cache_write_5m+t.cache_write_1h+t.cache_read),
               COUNT(DISTINCT t.agent_run_id)
        FROM turns t
        WHERE t.prompt_id IS NOT NULL
        GROUP BY t.prompt_id;

        -- project подтягиваем отдельным UPDATE через сессию (без коррелированного MAX).
        UPDATE tasks SET project = COALESCE(
            (SELECT s.project FROM sessions s WHERE s.session_id = tasks.session_id), '');

        -- текст промпта из prompt_text.
        UPDATE tasks SET text = COALESCE(
            (SELECT p.text FROM prompt_text p WHERE p.prompt_id = tasks.prompt_id), '');
        "#,
    )?;
    Ok(())
}
