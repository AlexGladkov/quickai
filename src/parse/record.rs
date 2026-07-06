//! Serde-схема сырой строки jsonl. Отражает только нужные поля — остальное игнор.
//! Схема подтверждена на данных ~/.claude/projects/**/*.jsonl.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct RawLine {
    #[serde(rename = "type")]
    pub kind: Option<String>, // "assistant" | "user" | "attachment" | ...

    #[serde(rename = "promptId")]
    pub prompt_id: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    #[serde(rename = "requestId")]
    pub request_id: Option<String>,
    pub uuid: Option<String>,
    #[serde(rename = "parentUuid")]
    pub parent_uuid: Option<String>,
    #[serde(rename = "isSidechain")]
    pub is_sidechain: Option<bool>,
    #[serde(rename = "sourceToolAssistantUUID")]
    pub source_tool_assistant_uuid: Option<String>,

    pub timestamp: Option<String>, // ISO-8601
    pub cwd: Option<String>,
    #[serde(rename = "gitBranch")]
    pub git_branch: Option<String>,
    pub version: Option<String>,

    pub message: Option<Message>,

    /// Результат Agent/Task tool_use. Форма непостоянна (object|array), поэтому Value —
    /// линковку agentId↔agentType достаём вручную (см. agent_link).
    #[serde(rename = "toolUseResult")]
    pub tool_use_result: Option<serde_json::Value>,
}

impl RawLine {
    /// (agentId, agentType) из toolUseResult, если это результат Agent/Task.
    pub fn agent_link(&self) -> Option<(String, String)> {
        let r = self.tool_use_result.as_ref()?;
        let id = r.get("agentId")?.as_str()?.to_string();
        let ty = r.get("agentType")?.as_str()?.to_string();
        Some((id, ty))
    }
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub role: Option<String>,
    pub model: Option<String>,
    pub usage: Option<RawUsage>,
    #[serde(rename = "stop_reason")]
    pub stop_reason: Option<String>,
    /// content: user-текст, tool_use (assistant), tool_result (user).
    pub content: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
pub struct RawUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation: Option<CacheCreation>,
    #[serde(default)]
    pub server_tool_use: Option<ServerToolUse>,
}

#[derive(Debug, Deserialize, Default)]
pub struct CacheCreation {
    #[serde(default)]
    pub ephemeral_5m_input_tokens: u64,
    #[serde(default)]
    pub ephemeral_1h_input_tokens: u64,
}

#[derive(Debug, Deserialize, Default)]
pub struct ServerToolUse {
    #[serde(default)]
    pub web_search_requests: u64,
    #[serde(default)]
    pub web_fetch_requests: u64,
}
