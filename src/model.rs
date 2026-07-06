//! Доменные типы. Отвязаны от сырой jsonl-схемы (та живёт в parse::record).

/// Сырое потребление токенов одного assistant-turn.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    pub web_search: u64,
    pub web_fetch: u64,
}

impl Usage {
    /// Суммарный «сырой» объём токенов (для сортировок по объёму).
    pub fn total_tokens(&self) -> u64 {
        self.input + self.output + self.cache_write_5m + self.cache_write_1h + self.cache_read
    }
}

/// Один assistant-turn — атом профилирования.
#[derive(Debug, Clone)]
pub struct Turn {
    pub prompt_id: String,
    pub session_id: String,
    /// None = главный агент; Some = субагент (agent-<id>).
    pub agent_run_id: Option<String>,
    pub is_sidechain: bool,
    pub model: String,
    pub ts_ms: i64,
    pub usage: Usage,
    pub cost_usd: f64,
}

/// Одна инвокация субагента (agent-<id>.jsonl).
#[derive(Debug, Clone)]
pub struct AgentRun {
    pub id: String,
    pub prompt_id: String,
    pub session_id: String,
    pub agent_type: String,
    pub file_path: String,
    pub first_ts_ms: i64,
    pub last_ts_ms: i64,
    pub turns: u32,
    pub out_tokens: u64,
    pub cost_usd: f64,
}

/// Одна пользовательская задача (promptId) — главный агент + все субагенты.
#[derive(Debug, Clone)]
pub struct Task {
    pub prompt_id: String,
    pub session_id: String,
    pub project: String,
    pub text: String,
    pub first_ts_ms: i64,
    pub last_ts_ms: i64,
    pub cost_usd: f64,
    pub out_tokens: u64,
    pub agent_count: u32,
}

impl Task {
    pub fn wall_ms(&self) -> i64 {
        (self.last_ts_ms - self.first_ts_ms).max(0)
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    pub session_id: String,
    pub project: String,
    pub cwd: String,
    pub git_branch: String,
    pub first_ts_ms: i64,
    pub last_ts_ms: i64,
    pub version: String,
}
