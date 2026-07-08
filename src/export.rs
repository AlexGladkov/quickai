//! `quickai export --json` — machine-readable aggregate dump for external ingestion.
//!
//! The CLI/report views are human-facing; this command emits a single stable JSON
//! object with the same aggregates so tools (dashboards, backends) can consume quickai
//! without scraping formatted text or coupling to the SQLite schema. Everything is
//! derived from the existing `query` aggregates — no new SQL.

use anyhow::Result;
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::query;

/// Stable contract version for consumers. Bump on breaking shape changes.
const EXPORT_SCHEMA_VERSION: u32 = 1;

fn top_rows(conn: &Connection, group: &str, proj: &str, limit: i64) -> Result<Value> {
    let rows = query::top(conn, group, "cost", limit, proj)?;
    Ok(Value::Array(
        rows.into_iter()
            .map(|r| {
                json!({
                    "key": r.key,
                    "cost_usd": r.cost_usd,
                    "out_tokens": r.out_tokens,
                    "count": r.count,
                })
            })
            .collect(),
    ))
}

/// Build the export document for the given project filter ("" = all projects).
pub fn build(conn: &Connection, proj: &str) -> Result<Value> {
    let s = query::stats(conn, proj)?;
    let tools: Vec<Value> = query::tool_profile(conn, 100, proj)?
        .into_iter()
        .map(|t| json!({"name": t.name, "calls": t.calls, "errors": t.errors, "err_pct": t.err_pct}))
        .collect();
    let waste: Vec<Value> = query::waste(conn, proj)?
        .into_iter()
        .map(|w| json!({"reason": w.reason, "count": w.count}))
        .collect();
    let latency: Vec<Value> = query::model_latency(conn, proj)?
        .into_iter()
        .map(|m| json!({"model": m.model, "turns": m.turns, "median_gap_ms": m.median_gap_ms}))
        .collect();
    let by_source: Vec<Value> = query::sources(conn, proj)?
        .into_iter()
        .map(|s| json!({"source": s.source, "turns": s.turns, "out_tokens": s.out_tokens, "cost_usd": s.cost_usd}))
        .collect();

    Ok(json!({
        "schema_version": EXPORT_SCHEMA_VERSION,
        "project_filter": proj,
        "summary": {
            "projects": s.projects,
            "sessions": s.sessions,
            "tasks": s.tasks,
            "agent_runs": s.agent_runs,
            "agent_runs_linked": s.agent_runs_linked,
            "turns": s.turns,
            "total_cost_usd": s.total_cost,
            "total_out_tokens": s.total_out,
            "total_in_tokens": s.total_in,
            "total_cache_read_tokens": s.total_cache_read,
            "total_tokens": s.total_tokens,
        },
        "by_project": top_rows(conn, "project", proj, 1000)?,
        "by_model": top_rows(conn, "model", proj, 100)?,
        "by_agent_type": top_rows(conn, "agenttype", proj, 200)?,
        "by_source": by_source,
        "tools": tools,
        "waste": waste,
        "latency": latency,
    }))
}

/// Print the export document to stdout (pretty when `pretty`, else compact one-line).
pub fn run(conn: &Connection, proj: &str, pretty: bool) -> Result<()> {
    let doc = build(conn, proj)?;
    let out = if pretty {
        serde_json::to_string_pretty(&doc)?
    } else {
        serde_json::to_string(&doc)?
    };
    println!("{out}");
    Ok(())
}
