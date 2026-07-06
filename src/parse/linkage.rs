//! Резолвер: сматчить субагентский файл agent-<id>.jsonl с subagent_type.
//!
//! ОТКРЫТЫЙ ВОПРОС (см. ARCHITECTURE.md §1). В одном promptId может быть несколько
//! Task tool_use → надо понять какой agent-<id>.jsonl соответствует какому типу.
//!
//! Гипотеза: <id> в имени файла связан с tool_use.id из главной сессии либо с
//! sourceToolAssistantUUID первой строки субагентского файла. Требует проверки на
//! данных перед реализацией.
//!
//! Fallback (пока не подтверждено): агрегировать субагентов по promptId без точного
//! типа; распределение типов брать из Task-вызовов в главной сессии.

/// Тип агента (subagent_type) из Task tool_use блока, если это он.
pub fn subagent_type_of_task(block: &serde_json::Value) -> Option<String> {
    if block.get("type")?.as_str()? != "tool_use" {
        return None;
    }
    if block.get("name")?.as_str()? != "Task" {
        return None;
    }
    Some(block.get("input")?.get("subagent_type")?.as_str()?.to_string())
}

/// TODO: реализовать точную линковку после проверки гипотезы про <id>.
/// Вход: имя файла (agent-<id>), первая строка субагентского файла.
/// Выход: ключ, по которому матчить Task tool_use в главной сессии.
pub fn agent_file_link_key(_file_stem: &str, _first_line: &serde_json::Value) -> Option<String> {
    None
}
