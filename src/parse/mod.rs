//! Стриминговый парсер jsonl. Читает построчно, битые строки пропускает.

pub mod record;
pub mod linkage;

use crate::model::Usage;
use record::RawUsage;

/// ISO-8601 → epoch ms. При ошибке → 0 (turn не теряем, время неизвестно).
pub fn ts_to_ms(iso: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(0)
}

/// Разворот сырого usage в доменный.
pub fn usage_from_raw(r: &RawUsage) -> Usage {
    let (cw5m, cw1h) = r
        .cache_creation
        .as_ref()
        .map(|c| (c.ephemeral_5m_input_tokens, c.ephemeral_1h_input_tokens))
        .unwrap_or((0, 0));
    let (ws, wf) = r
        .server_tool_use
        .as_ref()
        .map(|s| (s.web_search_requests, s.web_fetch_requests))
        .unwrap_or((0, 0));
    Usage {
        input: r.input_tokens,
        output: r.output_tokens,
        cache_write_5m: cw5m,
        cache_write_1h: cw1h,
        cache_read: r.cache_read_input_tokens,
        web_search: ws,
        web_fetch: wf,
    }
}

/// tool_use блоки из assistant-content: (tool_use_id, name).
pub fn tool_uses(content: &serde_json::Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let serde_json::Value::Array(blocks) = content {
        for b in blocks {
            if b.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                if let (Some(id), Some(name)) = (
                    b.get("id").and_then(|v| v.as_str()),
                    b.get("name").and_then(|v| v.as_str()),
                ) {
                    out.push((id.to_string(), name.to_string()));
                }
            }
        }
    }
    out
}

/// tool_result блоки из user-content: (tool_use_id, is_error).
pub fn tool_results(content: &serde_json::Value) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    if let serde_json::Value::Array(blocks) = content {
        for b in blocks {
            if b.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                if let Some(id) = b.get("tool_use_id").and_then(|v| v.as_str()) {
                    let err = b.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
                    out.push((id.to_string(), err));
                }
            }
        }
    }
    out
}

/// Извлечь текст первого user-prompt из message.content (строка или массив блоков).
pub fn user_text(content: &serde_json::Value) -> Option<String> {
    match content {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Array(blocks) => blocks.iter().find_map(|b| {
            if b.get("type")?.as_str()? == "text" {
                Some(b.get("text")?.as_str()?.to_string())
            } else {
                None
            }
        }),
        _ => None,
    }
}
