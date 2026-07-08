//! Агрегирующие запросы над индексом.

use anyhow::Result;
use rusqlite::Connection;

/// LIKE-паттерн для фильтра по проекту ("" = все проекты).
fn plike(proj: &str) -> String {
    format!("%{}%", proj)
}

#[derive(Debug)]
pub struct TopRow {
    pub key: String,
    pub cost_usd: f64,
    pub out_tokens: i64,
    pub count: i64,
}

#[derive(Debug)]
pub struct DbStats {
    pub projects: i64,
    pub sessions: i64,
    pub tasks: i64,
    pub agent_runs: i64,
    pub agent_runs_linked: i64,
    pub turns: i64,
    pub total_cost: f64,
    pub total_out: i64,
    pub total_in: i64,
    pub total_cache_read: i64,
    pub total_tokens: i64,
}

#[derive(Debug)]
pub struct SourceRow {
    pub source: String,
    pub turns: i64,
    pub out_tokens: i64,
    pub cost_usd: f64,
}

/// Разбивка по источникам данных (claude|codex|opencode|…) с фильтром по проекту.
pub fn sources(conn: &Connection, proj: &str) -> Result<Vec<SourceRow>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(NULLIF(source,''),'claude') AS source,
                COUNT(*), SUM(output_tokens), SUM(cost_usd)
         FROM turns WHERE project LIKE ?1
         GROUP BY source ORDER BY SUM(cost_usd) DESC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![plike(proj)], |r| {
            Ok(SourceRow {
                source: r.get(0)?,
                turns: r.get(1)?,
                out_tokens: r.get(2)?,
                cost_usd: r.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn stats(conn: &Connection, proj: &str) -> Result<DbStats> {
    let p = plike(proj);
    let one = |sql: &str| -> Result<i64> {
        Ok(conn.query_row(sql, rusqlite::params![p], |r| r.get(0))?)
    };
    Ok(DbStats {
        projects: one("SELECT COUNT(DISTINCT project) FROM sessions WHERE project LIKE ?1")?,
        sessions: one("SELECT COUNT(*) FROM sessions WHERE project LIKE ?1")?,
        tasks: one("SELECT COUNT(*) FROM tasks WHERE project LIKE ?1")?,
        agent_runs: one("SELECT COUNT(*) FROM agent_runs WHERE project LIKE ?1")?,
        agent_runs_linked: one("SELECT COUNT(*) FROM agent_runs WHERE agent_type<>'' AND project LIKE ?1")?,
        turns: one("SELECT COUNT(*) FROM turns WHERE project LIKE ?1")?,
        total_cost: conn.query_row("SELECT COALESCE(SUM(cost_usd),0) FROM turns WHERE project LIKE ?1", rusqlite::params![p], |r| r.get(0))?,
        total_out: one("SELECT COALESCE(SUM(output_tokens),0) FROM turns WHERE project LIKE ?1")?,
        total_in: one("SELECT COALESCE(SUM(input_tokens),0) FROM turns WHERE project LIKE ?1")?,
        total_cache_read: one("SELECT COALESCE(SUM(cache_read),0) FROM turns WHERE project LIKE ?1")?,
        total_tokens: one("SELECT COALESCE(SUM(input_tokens+output_tokens+cache_write_5m+cache_write_1h+cache_read),0) FROM turns WHERE project LIKE ?1")?,
    })
}

/// group ∈ task | agent | agenttype | project | model ; by ∈ cost | tokens ; proj "" = все
pub fn top(conn: &Connection, group: &str, by: &str, limit: i64, proj: &str) -> Result<Vec<TopRow>> {
    let order = match by {
        "cost" => "cost_usd",
        "tokens" => "out_tokens",
        other => anyhow::bail!("неизвестный ключ сортировки: {other} (cost|tokens)"),
    };
    // ?1 = LIKE-проект, ?2 = limit. Каждая группа → SELECT (key,cost_usd,out_tokens,cnt).
    let sql = match group {
        "task" => format!(
            "SELECT COALESCE(NULLIF(t.text,''), t.prompt_id) AS key,
                    t.cost_usd, t.out_tokens, t.agent_count AS cnt
             FROM tasks t WHERE t.project LIKE ?1 ORDER BY {order} DESC LIMIT ?2"
        ),
        "agent" => format!(
            "SELECT (COALESCE(NULLIF(agent_type,''),'workflow/без типа')||'  '||id) AS key,
                    cost_usd, out_tokens, turns AS cnt
             FROM agent_runs WHERE project LIKE ?1 ORDER BY {order} DESC LIMIT ?2"
        ),
        "agenttype" => format!(
            "SELECT COALESCE(NULLIF(agent_type,''),'workflow/без типа') AS key,
                    SUM(cost_usd) AS cost_usd, SUM(out_tokens) AS out_tokens,
                    COUNT(*) AS cnt
             FROM agent_runs WHERE project LIKE ?1 GROUP BY agent_type ORDER BY {order} DESC LIMIT ?2"
        ),
        "model" => format!(
            "SELECT model AS key, SUM(cost_usd) AS cost_usd,
                    SUM(output_tokens) AS out_tokens, COUNT(*) AS cnt
             FROM turns WHERE project LIKE ?1 GROUP BY model ORDER BY {order} DESC LIMIT ?2"
        ),
        "project" => format!(
            "SELECT project AS key, SUM(cost_usd) AS cost_usd,
                    SUM(out_tokens) AS out_tokens, COUNT(*) AS cnt
             FROM tasks WHERE project LIKE ?1 GROUP BY project ORDER BY {order} DESC LIMIT ?2"
        ),
        other => anyhow::bail!("неизвестная группа: {other} (task|agent|agenttype|project|model)"),
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params![plike(proj), limit], |r| {
            Ok(TopRow {
                key: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                cost_usd: r.get(1)?,
                out_tokens: r.get(2)?,
                count: r.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[derive(Debug)]
pub struct TaskRow {
    pub prompt_id: String,
    pub day: String,
    pub text: String,
    pub project: String,
    pub cost_usd: f64,
    pub out_tokens: i64,
    pub total_tokens: i64,
    pub agent_count: i64,
    pub wall_ms: i64,
}

/// Список поставленных задач (пользовательских промптов) с разбивкой.
/// project — подстрока-фильтр (None = все). by ∈ time|cost.
pub fn tasks_list(
    conn: &Connection,
    project: Option<&str>,
    by: &str,
    limit: i64,
) -> Result<Vec<TaskRow>> {
    let order = match by {
        "time" => "first_ts DESC",
        "cost" => "cost_usd DESC",
        "tokens" => "total_tokens DESC",
        other => anyhow::bail!("неизвестная сортировка: {other} (time|cost|tokens)"),
    };
    let like = format!("%{}%", project.unwrap_or(""));
    let sql = format!(
        "SELECT prompt_id, date(first_ts/1000,'unixepoch') AS day,
                COALESCE(NULLIF(text,''), prompt_id) AS text,
                project, cost_usd, out_tokens, total_tokens, agent_count, wall_ms
         FROM tasks
         WHERE project LIKE ?1 AND text <> ''
         ORDER BY {order} LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params![like, limit], |r| {
            Ok(TaskRow {
                prompt_id: r.get(0)?,
                day: r.get(1)?,
                text: r.get(2)?,
                project: r.get(3)?,
                cost_usd: r.get(4)?,
                out_tokens: r.get(5)?,
                total_tokens: r.get(6)?,
                agent_count: r.get(7)?,
                wall_ms: r.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[derive(Debug)]
pub struct BenchSummary {
    pub agent_type: String,
    pub runs: i64,
    pub total_cost: f64,
    pub total_out: i64,
    pub total_turns: i64,
    pub avg_cost_per_run: f64,
    pub avg_turns_per_run: f64,
    pub avg_cost_per_turn: f64,
    pub days: Vec<BenchDay>,
}

#[derive(Debug)]
pub struct BenchDay {
    pub day: String,
    pub runs: i64,
    pub cost: f64,
}

/// Бенчмарк одного типа агента: сводка + дневной тренд.
pub fn bench(conn: &Connection, agent_type: &str) -> Result<BenchSummary> {
    let (runs, total_cost, total_out, total_turns): (i64, f64, i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(cost_usd),0), COALESCE(SUM(out_tokens),0),
                COALESCE(SUM(turns),0)
         FROM agent_runs WHERE agent_type = ?1",
        [agent_type],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    if runs == 0 {
        anyhow::bail!("нет запусков агента '{agent_type}' (см. quickai top --group agenttype)");
    }
    let mut stmt = conn.prepare(
        "SELECT date(first_ts/1000,'unixepoch') AS d, COUNT(*), SUM(cost_usd)
         FROM agent_runs WHERE agent_type=?1 AND first_ts>0
         GROUP BY d ORDER BY d DESC LIMIT 14",
    )?;
    let days = stmt
        .query_map([agent_type], |r| {
            Ok(BenchDay { day: r.get(0)?, runs: r.get(1)?, cost: r.get(2)? })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(BenchSummary {
        agent_type: agent_type.to_string(),
        runs,
        total_cost,
        total_out,
        total_turns,
        avg_cost_per_run: total_cost / runs as f64,
        avg_turns_per_run: total_turns as f64 / runs as f64,
        avg_cost_per_turn: if total_turns > 0 { total_cost / total_turns as f64 } else { 0.0 },
        days,
    })
}

#[derive(Debug)]
pub struct TaskBreakdown {
    pub prompt_id: String,
    pub project: String,
    pub main_cost: f64,
    pub total_cost: f64,
    pub agents: Vec<AgentLine>,
    pub wall_ms: i64,
    pub busy_ms: i64,    // сумма wall субагентов (сколько «работы» шло)
    pub idle_ms: i64,    // простои: сумма gap'ов >60с между turn'ами (харнесс стоял)
    pub max_gap_ms: i64, // самый большой единичный простой
}

#[derive(Debug)]
pub struct AgentLine {
    pub id: String,
    pub agent_type: String,
    pub turns: i64,
    pub out_tokens: i64,
    pub cost_usd: f64,
    pub wall_ms: i64,
    pub prompt: String,
}

// ─────────────────────────── #1 cache-hit эффективность ───────────────────────────

#[derive(Debug)]
pub struct CacheRow {
    pub session_id: String,
    pub project: String,
    pub cache_read: i64,
    pub non_cached: i64, // input + cache_write (то, что НЕ из кэша)
    pub hit_pct: i64,    // cache_read / (cache_read + non_cached)
}

/// Сессии с наибольшим НЕ-кэшированным объёмом (кандидаты на фикс кэша).
pub fn cache_health(conn: &Connection, limit: i64, proj: &str) -> Result<Vec<CacheRow>> {
    let mut stmt = conn.prepare(
        "SELECT t.session_id,
                COALESCE(s.project,''),
                SUM(t.cache_read) AS cr,
                SUM(t.input_tokens + t.cache_write_5m + t.cache_write_1h) AS nc
         FROM turns t LEFT JOIN sessions s ON s.session_id=t.session_id
         WHERE t.session_id IS NOT NULL AND t.project LIKE ?2
         GROUP BY t.session_id
         HAVING cr+nc > 1000000
         ORDER BY nc DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![limit, plike(proj)], |r| {
            let cr: i64 = r.get(2)?;
            let nc: i64 = r.get(3)?;
            let denom = (cr + nc).max(1);
            Ok(CacheRow {
                session_id: r.get(0)?,
                project: r.get(1)?,
                cache_read: cr,
                non_cached: nc,
                hit_pct: 100 * cr / denom,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ─────────────────────────── #2 латентность / параллелизм ───────────────────────────

#[derive(Debug)]
pub struct ModelLatency {
    pub model: String,
    pub turns: i64,
    pub median_gap_ms: i64, // медианный gap между turn'ами (<120с, прокси отзывчивости)
}

#[derive(Debug)]
pub struct ParallelTask {
    pub prompt_id: String,
    pub text: String,
    pub wall_ms: i64,
    pub agent_busy_ms: i64, // сумма длительностей субагентов
    pub factor: f64,        // agent_busy / wall (>1 = реально параллельно)
    pub agents: i64,
}

/// Медианный gap между turn'ами по моделям (прокси latency; idle >120с отрезан).
pub fn model_latency(conn: &Connection, proj: &str) -> Result<Vec<ModelLatency>> {
    let mut stmt = conn.prepare(
        "WITH g AS (
           SELECT model,
                  ts - LAG(ts) OVER (PARTITION BY session_id ORDER BY ts) AS gap
           FROM turns WHERE ts>0 AND project LIKE ?1
         )
         SELECT model, COUNT(*) AS n,
                CAST(AVG(gap) AS INTEGER) AS avg_gap
         FROM g WHERE gap>0 AND gap<120000
         GROUP BY model ORDER BY n DESC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![plike(proj)], |r| {
            Ok(ModelLatency {
                model: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                turns: r.get(1)?,
                median_gap_ms: r.get(2)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Топ задач по фактору параллелизма субагентов.
pub fn parallel_tasks(conn: &Connection, limit: i64, proj: &str) -> Result<Vec<ParallelTask>> {
    let mut stmt = conn.prepare(
        "SELECT t.prompt_id, COALESCE(NULLIF(t.text,''),t.prompt_id),
                t.wall_ms,
                COALESCE((SELECT SUM(ar.last_ts-ar.first_ts) FROM agent_runs ar
                          WHERE ar.prompt_id=t.prompt_id),0) AS busy,
                t.agent_count
         FROM tasks t
         WHERE t.agent_count>1 AND t.wall_ms>0 AND t.text<>'' AND t.project LIKE ?2
         ORDER BY busy DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![limit, plike(proj)], |r| {
            let wall: i64 = r.get(2)?;
            let busy: i64 = r.get(3)?;
            Ok(ParallelTask {
                prompt_id: r.get(0)?,
                text: r.get(1)?,
                wall_ms: wall,
                agent_busy_ms: busy,
                factor: if wall > 0 { busy as f64 / wall as f64 } else { 0.0 },
                agents: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ─────────────────────────── #3 turn-эффективность (спиннинг) ───────────────────────────

#[derive(Debug)]
pub struct SpinRow {
    pub prompt_id: String,
    pub text: String,
    pub turns: i64,
    pub out_tokens: i64,
    pub out_per_turn: i64,
    pub agents: i64,
}

/// Задачи с большим числом turn'ов и низким output/turn = кружение впустую.
pub fn spinning(conn: &Connection, limit: i64, proj: &str) -> Result<Vec<SpinRow>> {
    let mut stmt = conn.prepare(
        "SELECT tk.prompt_id, COALESCE(NULLIF(tk.text,''),tk.prompt_id),
                COUNT(t.id) AS turns, SUM(t.output_tokens) AS outp, tk.agent_count
         FROM tasks tk JOIN turns t ON t.prompt_id=tk.prompt_id
         WHERE tk.text<>'' AND tk.project LIKE ?2
         GROUP BY tk.prompt_id
         HAVING turns>=20
         ORDER BY (SUM(t.output_tokens)*1.0/COUNT(t.id)) ASC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![limit, plike(proj)], |r| {
            let turns: i64 = r.get(2)?;
            let outp: i64 = r.get(3)?;
            Ok(SpinRow {
                prompt_id: r.get(0)?,
                text: r.get(1)?,
                turns,
                out_tokens: outp,
                out_per_turn: if turns > 0 { outp / turns } else { 0 },
                agents: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ─────────────────────────── #4 профиль тулзов ───────────────────────────

#[derive(Debug)]
pub struct ToolRow {
    pub name: String,
    pub calls: i64,
    pub errors: i64,
    pub err_pct: i64,
}

/// Топ тулзов по числу вызовов + error-rate.
pub fn tool_profile(conn: &Connection, limit: i64, proj: &str) -> Result<Vec<ToolRow>> {
    let mut stmt = conn.prepare(
        "SELECT name, COUNT(*) AS calls, SUM(is_error) AS errs
         FROM tool_calls WHERE project LIKE ?2 GROUP BY name ORDER BY calls DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![limit, plike(proj)], |r| {
            let calls: i64 = r.get(1)?;
            let errs: i64 = r.get(2)?;
            Ok(ToolRow {
                name: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                calls,
                errors: errs,
                err_pct: if calls > 0 { 100 * errs / calls } else { 0 },
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ─────────────────────────── #5 waste (stop_reason) ───────────────────────────

#[derive(Debug)]
pub struct WasteRow {
    pub reason: String,
    pub count: i64,
}

/// Распределение stop_reason (max_tokens = truncation → перезапуск, refusal и т.п.).
pub fn waste(conn: &Connection, proj: &str) -> Result<Vec<WasteRow>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(stop_reason,'(null)') AS r, COUNT(*) AS n
         FROM turns WHERE project LIKE ?1 GROUP BY r ORDER BY n DESC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![plike(proj)], |r| Ok(WasteRow { reason: r.get(0)?, count: r.get(1)? }))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Все субагенты с их prompt_id — для группировки по задачам (отчёт).
pub fn all_agent_runs(conn: &Connection, proj: &str) -> Result<Vec<(String, AgentLine)>> {
    let mut stmt = conn.prepare(
        "SELECT prompt_id, id, agent_type, turns, out_tokens, cost_usd,
                (last_ts-first_ts) AS wall, COALESCE(prompt,'')
         FROM agent_runs WHERE prompt_id IS NOT NULL AND project LIKE ?1 ORDER BY cost_usd DESC",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![plike(proj)], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                AgentLine {
                    id: r.get(1)?,
                    agent_type: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    turns: r.get(3)?,
                    out_tokens: r.get(4)?,
                    cost_usd: r.get(5)?,
                    wall_ms: r.get(6)?,
                    prompt: r.get(7)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Разбор одной задачи: главный агент + субагенты.
pub fn task(conn: &Connection, prompt_id: &str) -> Result<TaskBreakdown> {
    let (project, wall_ms, total_cost): (String, i64, f64) = conn.query_row(
        "SELECT project, wall_ms, cost_usd FROM tasks WHERE prompt_id=?1",
        [prompt_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let main_cost: f64 = conn.query_row(
        "SELECT COALESCE(SUM(cost_usd),0) FROM turns WHERE prompt_id=?1 AND agent_run_id IS NULL",
        [prompt_id],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT id, agent_type, turns, out_tokens, cost_usd,
                (last_ts-first_ts) AS wall, COALESCE(prompt,'')
         FROM agent_runs WHERE prompt_id=?1 ORDER BY cost_usd DESC",
    )?;
    let agents = stmt
        .query_map([prompt_id], |r| {
            Ok(AgentLine {
                id: r.get(0)?,
                agent_type: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                turns: r.get(2)?,
                out_tokens: r.get(3)?,
                cost_usd: r.get(4)?,
                wall_ms: r.get(5)?,
                prompt: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let busy_ms: i64 = agents.iter().map(|a| a.wall_ms.max(0)).sum();
    // Тайминг: простои = сумма gap'ов >60с между turn'ами задачи (харнесс реально стоял).
    let (idle_ms, max_gap_ms): (i64, i64) = conn.query_row(
        "WITH o AS (SELECT ts - LAG(ts) OVER (ORDER BY ts) AS gap
                    FROM turns WHERE prompt_id=?1 AND ts>0)
         SELECT COALESCE(SUM(CASE WHEN gap>60000 THEN gap ELSE 0 END),0),
                COALESCE(MAX(gap),0)
         FROM o",
        [prompt_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(TaskBreakdown {
        prompt_id: prompt_id.to_string(), project, main_cost, total_cost, agents,
        wall_ms, busy_ms, idle_ms, max_gap_ms,
    })
}

// ─────────────────────────── Долгие задачи (почему харнесс тормозил) ───────────────────────────

#[derive(Debug)]
pub struct SlowRow {
    pub prompt_id: String,
    pub text: String,
    pub wall_ms: i64,
    pub idle_ms: i64,
    pub idle_pct: i64,
    pub max_gap_ms: i64,
    pub agents: i64,
}

/// Задачи по wall-времени + доля простоя (idle = харнесс стоял, gap>60с).
pub fn slow(conn: &Connection, limit: i64, proj: &str) -> Result<Vec<SlowRow>> {
    let mut stmt = conn.prepare(
        "WITH gaps AS (
           SELECT prompt_id,
                  ts - LAG(ts) OVER (PARTITION BY prompt_id ORDER BY ts) AS gap
           FROM turns WHERE prompt_id IS NOT NULL AND ts>0 AND project LIKE ?2
         ),
         idle AS (
           SELECT prompt_id,
                  SUM(CASE WHEN gap>60000 THEN gap ELSE 0 END) AS idle_ms,
                  MAX(gap) AS max_gap
           FROM gaps GROUP BY prompt_id
         )
         SELECT tk.prompt_id, COALESCE(NULLIF(tk.text,''),tk.prompt_id),
                tk.wall_ms, COALESCE(i.idle_ms,0), COALESCE(i.max_gap,0), tk.agent_count
         FROM tasks tk JOIN idle i ON i.prompt_id=tk.prompt_id
         WHERE tk.text<>'' AND tk.wall_ms>0
         ORDER BY tk.wall_ms DESC LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![limit, plike(proj)], |r| {
            let wall: i64 = r.get(2)?;
            let idle: i64 = r.get(3)?;
            Ok(SlowRow {
                prompt_id: r.get(0)?,
                text: r.get(1)?,
                wall_ms: wall,
                idle_ms: idle,
                idle_pct: if wall > 0 { 100 * idle / wall } else { 0 },
                max_gap_ms: r.get(4)?,
                agents: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
