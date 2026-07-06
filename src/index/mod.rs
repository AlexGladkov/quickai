//! Инкрементальный индексатор: walk projects → parse jsonl → SQLite.

pub mod schema;

use crate::parse::{self, record::RawLine};
use crate::pricing;
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Корень транскриптов Claude Code.
pub fn projects_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".claude/projects")
}

pub fn db_path() -> PathBuf {
    // QUICKAI_DB переопределяет путь к БД (напр. для демо/тестов).
    if let Ok(p) = std::env::var("QUICKAI_DB") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".claude/quickai.db")
}

pub fn open_db() -> Result<Connection> {
    let conn = Connection::open(db_path())?;
    schema::init(&conn)?;
    Ok(conn)
}

/// slug проекта = имя каталога под projects/.
fn project_of(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .and_then(|p| p.components().next())
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Тип агента из имени субагентского файла (agent-<id>.jsonl) — пока только сам id.
/// Точный subagent_type подставит parse::linkage после резолва (TODO).
fn agent_run_id_of(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    stem.strip_prefix("agent-").map(|s| s.to_string())
}

pub struct IndexStats {
    pub files_scanned: usize,
    pub files_indexed: usize,
    pub turns_added: u64,
}

/// Полный проход. rebuild=true → снести данные и перечитать всё с нуля.
pub fn run(conn: &mut Connection, rebuild: bool) -> Result<IndexStats> {
    if rebuild {
        conn.execute_batch(
            "DELETE FROM turns; DELETE FROM agent_runs; DELETE FROM tasks;
             DELETE FROM sessions; DELETE FROM files; DELETE FROM tool_calls;
             DELETE FROM prompt_text; DELETE FROM agent_meta; DELETE FROM agent_prompt;",
        )?;
    }
    let root = projects_root();
    let mut stats = IndexStats { files_scanned: 0, files_indexed: 0, turns_added: 0 };

    let tx = conn.transaction()?;
    for entry in WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().map(|e| e != "jsonl").unwrap_or(true) {
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

        let project = project_of(path, &root);
        let (added, last_prompt) = index_file(&tx, path, &project, prev_read as u64, seed_prompt)?;
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

/// Прочитать хвост файла с офсета, вставить turn'ы.
/// promptId живёт только на user-строках → протягиваем вперёд на assistant-turn'ы.
/// Возврат: (число turn'ов, последний виденный promptId — seed для след. дочитывания).
fn index_file(
    conn: &Connection,
    path: &Path,
    project: &str,
    from: u64,
    seed_prompt: Option<String>,
) -> Result<(u64, Option<String>)> {
    let mut f = std::fs::File::open(path).with_context(|| format!("open {path:?}"))?;
    f.seek(SeekFrom::Start(from))?;
    let reader = BufReader::new(&mut f as &mut dyn Read);

    let agent_run_id = agent_run_id_of(path); // Some для субагентских файлов
    let mut count = 0u64;
    let mut cur_prompt = seed_prompt;

    for line in reader.lines() {
        let line = match line {
            Ok(l) if !l.trim().is_empty() => l,
            _ => continue,
        };
        let rec: RawLine = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(_) => continue, // битая строка — пропуск
        };

        // Линковка субагента: agentId ↔ subagent_type из toolUseResult.
        if let Some((aid, atype)) = rec.agent_link() {
            conn.execute(
                "INSERT OR REPLACE INTO agent_meta(id,agent_type) VALUES(?1,?2)",
                rusqlite::params![aid, atype],
            )?;
        }

        // tool_result (user-строки) → пометить is_error у соответствующего tool_call.
        if rec.kind.as_deref() == Some("user") {
            if let Some(content) = rec.message.as_ref().and_then(|m| m.content.as_ref()) {
                for (tuid, err) in parse::tool_results(content) {
                    if err {
                        conn.execute(
                            "UPDATE tool_calls SET is_error=1 WHERE tool_use_id=?1",
                            rusqlite::params![tuid],
                        )?;
                    }
                }
            }
        }

        // user-строка задаёт текущий promptId; заодно ловим текст промпта.
        if let Some(pid) = &rec.prompt_id {
            cur_prompt = Some(pid.clone());
            if let Some(text) = rec.message.as_ref().and_then(|m| m.content.as_ref()).and_then(parse::user_text) {
                // Схлопнуть whitespace (текст промпта многострочный) + отсечь системный шум.
                let clean: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
                let is_noise = clean.is_empty()
                    || clean.starts_with("<task-notification")
                    || clean.starts_with("<local-command")
                    || clean.starts_with("Caveman mode");
                if !is_noise {
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
        }

        if rec.kind.as_deref() != Some("assistant") {
            continue;
        }
        let msg = match &rec.message {
            Some(m) => m,
            None => continue,
        };
        let usage = match &msg.usage {
            Some(u) => parse::usage_from_raw(u),
            None => continue,
        };
        let model = msg.model.clone().unwrap_or_default();
        if model == "<synthetic>" || model.is_empty() {
            continue;
        }
        let ts = rec.timestamp.as_deref().map(parse::ts_to_ms).unwrap_or(0);
        let cost = pricing::cost_of(&model, &usage);

        conn.execute(
            "INSERT INTO turns(prompt_id,session_id,project,agent_run_id,is_sidechain,model,ts,
               input_tokens,output_tokens,cache_write_5m,cache_write_1h,cache_read,
               web_search,web_fetch,cost_usd,stop_reason)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            rusqlite::params![
                cur_prompt,
                rec.session_id,
                project,
                agent_run_id,
                rec.is_sidechain.unwrap_or(false) as i64,
                model,
                ts,
                usage.input as i64,
                usage.output as i64,
                usage.cache_write_5m as i64,
                usage.cache_write_1h as i64,
                usage.cache_read as i64,
                usage.web_search as i64,
                usage.web_fetch as i64,
                cost,
                msg.stop_reason,
            ],
        )?;

        // tool_use блоки этого turn'а → tool_calls (is_error проставит tool_result).
        if let Some(content) = msg.content.as_ref() {
            for (tuid, name) in parse::tool_uses(content) {
                conn.execute(
                    "INSERT OR IGNORE INTO tool_calls(tool_use_id,name,project,session_id,agent_run_id)
                     VALUES(?1,?2,?3,?4,?5)",
                    rusqlite::params![tuid, name, project, rec.session_id, agent_run_id],
                )?;
            }
        }

        // upsert session (контекст берём с первого попавшегося turn'а сессии)
        if let Some(sid) = &rec.session_id {
            conn.execute(
                "INSERT OR IGNORE INTO sessions(session_id,project,cwd,git_branch,first_ts,last_ts,version)
                 VALUES(?1,?2,?3,?4,?5,?5,?6)",
                rusqlite::params![
                    sid, project,
                    rec.cwd.clone().unwrap_or_default(),
                    rec.git_branch.clone().unwrap_or_default(),
                    ts,
                    rec.version.clone().unwrap_or_default(),
                ],
            )?;
            conn.execute(
                "UPDATE sessions SET last_ts=MAX(last_ts,?2), first_ts=MIN(first_ts,?2)
                 WHERE session_id=?1",
                rusqlite::params![sid, ts],
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
        INSERT INTO agent_runs(id,prompt_id,session_id,project,agent_type,file_path,
                               first_ts,last_ts,turns,out_tokens,cost_usd)
        SELECT agent_run_id,
               MAX(prompt_id), MAX(session_id), MAX(project),
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
        INSERT INTO tasks(prompt_id,session_id,project,text,first_ts,last_ts,
                          wall_ms,cost_usd,out_tokens,total_tokens,agent_count)
        SELECT t.prompt_id,
               MAX(t.session_id),
               '' AS project,
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
