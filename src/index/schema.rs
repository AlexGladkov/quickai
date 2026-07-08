//! DDL SQLite. Схема — производная от транскриптов (source of truth), reindex идемпотентен.

use anyhow::Result;
use rusqlite::Connection;

pub const SCHEMA_VERSION: i64 = 2;

pub fn init(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;

        CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT
        );

        -- Инкрементальный индекс: дочитываем только хвост файла.
        CREATE TABLE IF NOT EXISTS files (
            path           TEXT PRIMARY KEY,
            mtime          INTEGER,
            size           INTEGER,
            bytes_read     INTEGER,
            last_indexed   INTEGER,
            last_prompt_id TEXT     -- seed для протяжки promptId при дочитывании хвоста
        );

        -- Текст пользовательского промпта (promptId живёт только на user-строках).
        CREATE TABLE IF NOT EXISTS prompt_text (
            prompt_id TEXT PRIMARY KEY,
            text      TEXT
        );

        -- Линковка agentId ↔ subagent_type (из toolUseResult родительской сессии).
        CREATE TABLE IF NOT EXISTS agent_meta (
            id         TEXT PRIMARY KEY,
            agent_type TEXT
        );

        CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY,
            project    TEXT,
            source     TEXT,     -- источник данных (claude|codex|opencode|…)
            cwd        TEXT,
            git_branch TEXT,
            first_ts   INTEGER,
            last_ts    INTEGER,
            version    TEXT
        );

        CREATE TABLE IF NOT EXISTS tasks (
            prompt_id    TEXT PRIMARY KEY,
            session_id   TEXT,
            project      TEXT,
            source       TEXT,
            text         TEXT,
            first_ts     INTEGER,
            last_ts      INTEGER,
            wall_ms      INTEGER,
            cost_usd     REAL,
            out_tokens   INTEGER,
            total_tokens INTEGER,   -- input+output+cache(все) — суммарный throughput
            agent_count  INTEGER
        );

        CREATE TABLE IF NOT EXISTS agent_runs (
            id         TEXT PRIMARY KEY,
            prompt_id  TEXT,
            session_id TEXT,
            project    TEXT,
            source     TEXT,
            agent_type TEXT,
            file_path  TEXT,
            first_ts   INTEGER,
            last_ts    INTEGER,
            turns      INTEGER,
            out_tokens INTEGER,
            cost_usd   REAL,
            prompt     TEXT     -- первая строка промпта агента (смысл вместо hash-id)
        );

        -- Первый промпт субагента (agent-<id>) — берём из его файла.
        CREATE TABLE IF NOT EXISTS agent_prompt (
            id   TEXT PRIMARY KEY,
            text TEXT
        );

        CREATE TABLE IF NOT EXISTS turns (
            id             INTEGER PRIMARY KEY,
            prompt_id      TEXT,
            session_id     TEXT,
            project        TEXT,
            source         TEXT,
            ext_id         TEXT,     -- стабильный id turn'а из источника (дедуп иммутабельных файлов)
            agent_run_id   TEXT,
            is_sidechain   INTEGER,
            model          TEXT,
            ts             INTEGER,
            input_tokens   INTEGER,
            output_tokens  INTEGER,
            cache_write_5m INTEGER,
            cache_write_1h INTEGER,
            cache_read     INTEGER,
            web_search     INTEGER,
            web_fetch      INTEGER,
            cost_usd       REAL,
            stop_reason    TEXT
        );

        -- Вызовы тулзов: tool_use ↔ tool_result по tool_use_id (в пределах файла).
        CREATE TABLE IF NOT EXISTS tool_calls (
            tool_use_id  TEXT PRIMARY KEY,
            name         TEXT,
            project      TEXT,
            source       TEXT,
            session_id   TEXT,
            agent_run_id TEXT,
            is_error     INTEGER DEFAULT 0
        );

        CREATE INDEX IF NOT EXISTS idx_turns_prompt ON turns(prompt_id);
        CREATE INDEX IF NOT EXISTS idx_turns_agent  ON turns(agent_run_id);
        CREATE INDEX IF NOT EXISTS idx_turns_model  ON turns(model);
        CREATE INDEX IF NOT EXISTS idx_turns_ts     ON turns(ts);
        CREATE INDEX IF NOT EXISTS idx_agent_prompt ON agent_runs(prompt_id);
        CREATE INDEX IF NOT EXISTS idx_tasks_proj   ON tasks(project);
        CREATE INDEX IF NOT EXISTS idx_turns_stop   ON turns(stop_reason);
        CREATE INDEX IF NOT EXISTS idx_turns_sess   ON turns(session_id);
        CREATE INDEX IF NOT EXISTS idx_tool_name    ON tool_calls(name);
        CREATE INDEX IF NOT EXISTS idx_tool_proj    ON tool_calls(project);
        CREATE INDEX IF NOT EXISTS idx_turns_proj   ON turns(project);
        CREATE INDEX IF NOT EXISTS idx_agent_proj   ON agent_runs(project);
        "#,
    )?;

    migrate(conn)?;

    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_turns_source ON turns(source);
         -- дедуп OpenCode-turn'ов по (source, ext_id); NULL (Claude) в индекс не входят.
         CREATE UNIQUE INDEX IF NOT EXISTS idx_turns_extid ON turns(source, ext_id)
             WHERE ext_id IS NOT NULL;",
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO meta(key,value) VALUES('schema_version', ?1)",
        [SCHEMA_VERSION],
    )?;
    Ok(())
}

/// Аддитивные миграции существующих БД: доливаем недостающие колонки. Проверяем по
/// наличию колонки (а не по версии) — устойчиво к любому промежуточному состоянию.
/// До v2 всё было Claude Code, поэтому `source` доливаем с DEFAULT 'claude' без reindex.
fn migrate(conn: &Connection) -> Result<()> {
    for t in ["sessions", "tasks", "agent_runs", "turns", "tool_calls"] {
        ensure_col(conn, t, "source", "TEXT DEFAULT 'claude'")?;
    }
    ensure_col(conn, "turns", "ext_id", "TEXT")?;
    Ok(())
}

/// Добавить колонку, если её ещё нет (идемпотентно).
fn ensure_col(conn: &Connection, table: &str, col: &str, decl: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|name| name == col);
    if !exists {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {col} {decl}"), [])?;
    }
    Ok(())
}
