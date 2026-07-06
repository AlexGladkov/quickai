//! Минимальный MCP-сервер поверх stdio (JSON-RPC 2.0, newline-delimited).
//! Отдаёт те же запросы что CLB, но вызывается из диалога. Ходит в готовую БД.

use crate::{cli, index, query};
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const PROTOCOL: &str = "2024-11-05";

pub fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) if !l.trim().is_empty() => l,
            _ => continue,
        };
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");

        // Уведомления (без id) — ответ не шлём.
        let Some(id) = id else { continue };

        let result = handle(method, req.get("params"));
        let resp = match result {
            Ok(r) => json!({"jsonrpc":"2.0","id":id,"result":r}),
            Err(e) => json!({"jsonrpc":"2.0","id":id,
                "error":{"code":-32000,"message":e.to_string()}}),
        };
        writeln!(stdout, "{}", resp)?;
        stdout.flush()?;
    }
    Ok(())
}

fn handle(method: &str, params: Option<&Value>) -> Result<Value> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "quickai", "version": env!("CARGO_PKG_VERSION")}
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_list()),
        "tools/call" => tools_call(params),
        other => anyhow::bail!("неизвестный метод: {other}"),
    }
}

fn tools_list() -> Value {
    let enum_group = json!(["task", "agent", "agenttype", "project", "model"]);
    let enum_by = json!(["cost", "tokens"]);
    let proj_prop = json!({"type": "string", "description": "фильтр по проекту (подстрока, напр. имя папки); пусто = все проекты"});
    json!({"tools": [
        {
            "name": "quickai_stats",
            "description": "Сводка индекса профайлера: задачи, субагенты, turn'ы, output-токены, суммарная стоимость. project — фильтр по проекту",
            "inputSchema": {"type": "object", "properties": {"project": proj_prop.clone()}}
        },
        {
            "name": "quickai_top",
            "description": "Топ прожорливых по стоимости/токенам. Группировка: task|agent|agenttype|project|model",
            "inputSchema": {"type": "object", "properties": {
                "group": {"type": "string", "enum": enum_group, "default": "task"},
                "by": {"type": "string", "enum": enum_by, "default": "cost"},
                "limit": {"type": "integer", "default": 20},
                "project": proj_prop.clone()
            }}
        },
        {
            "name": "quickai_task",
            "description": "Разбор одной задачи (promptId): главный агент + субагенты, стоимость каждого",
            "inputSchema": {"type": "object", "properties": {
                "prompt_id": {"type": "string"}
            }, "required": ["prompt_id"]}
        },
        {
            "name": "quickai_bench",
            "description": "Бенчмарк типа агента во времени: ср. $/запуск, $/turn, дневной тренд",
            "inputSchema": {"type": "object", "properties": {
                "agent_type": {"type": "string"}
            }, "required": ["agent_type"]}
        },
        {
            "name": "quickai_tasks",
            "description": "Список поставленных задач (пользовательских промптов) с разбивкой: дата, $, агенты, wall, текст",
            "inputSchema": {"type": "object", "properties": {
                "project": {"type": "string", "description": "подстрока-фильтр по проекту"},
                "by": {"type": "string", "enum": ["time", "cost"], "default": "time"},
                "limit": {"type": "integer", "default": 30}
            }}
        },
        {
            "name": "quickai_usage",
            "description": "Цельный usage-отчёт: сводка + модели + проекты + типы агентов + топ задач. project — фильтр по проекту",
            "inputSchema": {"type": "object", "properties": {"project": proj_prop.clone()}}
        },
        {
            "name": "quickai_cache",
            "description": "Cache-hit по сессиям: где кэш ломается (низкий hit% + большой не-кэш) — кандидаты на фикс",
            "inputSchema": {"type": "object", "properties": {"limit": {"type": "integer", "default": 20}, "project": proj_prop.clone()}}
        },
        {
            "name": "quickai_latency",
            "description": "Латентность моделей (ср. gap между turn'ами) + фактор параллелизма субагентов",
            "inputSchema": {"type": "object", "properties": {"limit": {"type": "integer", "default": 15}, "project": proj_prop.clone()}}
        },
        {
            "name": "quickai_spin",
            "description": "Спиннинг: задачи с кучей turn'ов и низким output/turn (кружение впустую)",
            "inputSchema": {"type": "object", "properties": {"limit": {"type": "integer", "default": 20}, "project": proj_prop.clone()}}
        },
        {
            "name": "quickai_tools",
            "description": "Профиль тулзов: частота вызовов Bash/Read/Edit/… и error-rate",
            "inputSchema": {"type": "object", "properties": {"limit": {"type": "integer", "default": 25}, "project": proj_prop.clone()}}
        },
        {
            "name": "quickai_waste",
            "description": "Waste: распределение stop_reason (max_tokens=обрыв, refusal=отказ)",
            "inputSchema": {"type": "object", "properties": {"project": proj_prop.clone()}}
        },
        {
            "name": "quickai_slow",
            "description": "Долгие задачи по wall-времени + доля простоя (idle%): почему харнесс тормозил",
            "inputSchema": {"type": "object", "properties": {"limit": {"type": "integer", "default": 20}, "project": proj_prop.clone()}}
        }
    ]})
}

fn tools_call(params: Option<&Value>) -> Result<Value> {
    let params = params.ok_or_else(|| anyhow::anyhow!("нет params"))?;
    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let conn = index::open_db()?;
    // Фильтр по проекту ("" = все) — «профилируй ЭТОТ проект» из чата.
    let proj = args.get("project").and_then(|v| v.as_str()).unwrap_or("");

    let text = match name {
        "quickai_stats" => cli::render_stats(&query::stats(&conn, proj)?),
        "quickai_top" => {
            let group = args.get("group").and_then(|v| v.as_str()).unwrap_or("task");
            let by = args.get("by").and_then(|v| v.as_str()).unwrap_or("cost");
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
            let rows = query::top(&conn, group, by, limit, proj)?;
            cli::render_top(&rows, group, by)
        }
        "quickai_task" => {
            let pid = args.get("prompt_id").and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("нужен prompt_id"))?;
            cli::render_task(&query::task(&conn, pid)?)
        }
        "quickai_bench" => {
            let at = args.get("agent_type").and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("нужен agent_type"))?;
            cli::render_bench(&query::bench(&conn, at)?)
        }
        "quickai_tasks" => {
            let project = args.get("project").and_then(|v| v.as_str());
            let by = args.get("by").and_then(|v| v.as_str()).unwrap_or("time");
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(30);
            let rows = query::tasks_list(&conn, project, by, limit)?;
            cli::render_tasks(&rows, &format!("Задачи ({} по {})", rows.len(), by))
        }
        "quickai_usage" => {
            let stats = query::stats(&conn, proj)?;
            let models = query::top(&conn, "model", "cost", 8, proj)?;
            let projects = query::top(&conn, "project", "cost", 10, proj)?;
            let agents = query::top(&conn, "agenttype", "cost", 12, proj)?;
            let tasks = query::tasks_list(&conn, if proj.is_empty() { None } else { Some(proj) }, "cost", 15)?;
            cli::render_usage(&stats, &models, &projects, &tasks, &agents)
        }
        "quickai_cache" => {
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
            cli::render_cache(&query::cache_health(&conn, limit, proj)?)
        }
        "quickai_latency" => {
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(15);
            cli::render_latency(&query::model_latency(&conn, proj)?, &query::parallel_tasks(&conn, limit, proj)?)
        }
        "quickai_spin" => {
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
            cli::render_spinning(&query::spinning(&conn, limit, proj)?)
        }
        "quickai_tools" => {
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(25);
            cli::render_tools(&query::tool_profile(&conn, limit, proj)?)
        }
        "quickai_waste" => cli::render_waste(&query::waste(&conn, proj)?),
        "quickai_slow" => {
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
            cli::render_slow(&query::slow(&conn, limit, proj)?)
        }
        other => anyhow::bail!("неизвестный tool: {other}"),
    };

    Ok(json!({"content": [{"type": "text", "text": text}], "isError": false}))
}
