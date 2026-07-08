//! Граница источника данных (source/adapter).
//!
//! Индексатор (`crate::index`) не знает, ГДЕ и КАК источник хранит транскрипты — это
//! знает адаптер. Он обходит свои файлы, читает инкрементально и отдаёт поток
//! нормализованных [`Event`], одинаковый для всех источников. Новый SWE-агент
//! подключается отдельным `impl DataSource` — без правок индексатора/схемы/агрегатов.
//!
//! Разные модели хранения ложатся на одну границу:
//! - Claude Code — append-JSONL, дочитывание хвоста по байтовому офсету ([`claude`])
//! - OpenCode — иммутабельные per-message JSON + части в соседних файлах ([`opencode`])

pub mod claude;
pub mod opencode;

use crate::model::Usage;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Один assistant-turn в нормализованном виде.
pub struct TurnData {
    /// promptId явно (OpenCode знает по parentID) или None → взять текущий (Claude протягивает).
    pub prompt_id: Option<String>,
    pub project: String,
    pub session_id: Option<String>,
    pub agent_run_id: Option<String>,
    /// Стабильный внешний id turn'а (id сообщения) — дедуп при повторном чтении
    /// иммутабельных файлов. None = нет (Claude, append-only — дублей не бывает).
    pub ext_id: Option<String>,
    pub is_sidechain: bool,
    pub model: String,
    pub ts: i64,
    pub usage: Usage,
    /// Готовый cost из источника (OpenCode) или None → посчитать по прайсингу (Claude).
    pub cost: Option<f64>,
    pub stop_reason: Option<String>,
    /// tool_use блоки этого turn'а: (tool_use_id, name).
    pub tool_uses: Vec<(String, String)>,
    // Контекст сессии.
    pub cwd: String,
    pub git_branch: String,
    pub version: String,
}

/// Нормализованная операция, извлечённая из транскрипта. Индексатор применяет их к БД.
pub enum Event {
    /// Линковка субагента: agentId ↔ subagent_type.
    AgentLink { id: String, agent_type: String },
    /// Результат тулзы: пометить is_error у tool_call.
    ToolResult { tool_use_id: String, is_error: bool },
    /// Пользовательский промпт: задаёт текущий promptId + (не-шумный) текст.
    Prompt {
        prompt_id: String,
        /// Очищенный текст (None = шум/пусто, promptId всё равно выставляется).
        text: Option<String>,
        /// Для субагентского файла — id прогона (первый промпт агента).
        agent_run_id: Option<String>,
    },
    /// assistant-turn.
    Turn(TurnData),
}

/// Результат дочитывания одного файла источника.
pub struct FileBatch {
    pub events: Vec<Event>,
    /// Сколько байт файла прочитано (для инкрементального дочитывания хвоста).
    pub bytes: u64,
    /// Последний виденный promptId — seed для следующего дочитывания хвоста.
    pub last_prompt: Option<String>,
}

/// Источник данных: свой обход файлов + разбор в нормализованные [`Event`].
pub trait DataSource {
    /// Стабильный id источника — колонка `source` (claude|opencode|…).
    fn name(&self) -> &'static str;
    /// Корневой каталог для обхода.
    fn root(&self) -> PathBuf;
    /// Файл принадлежит источнику (фильтр по расположению/имени/расширению).
    fn owns(&self, path: &Path) -> bool;
    /// Дочитать файл с офсета `from`, вернуть нормализованные события.
    /// `seed_prompt` — последний promptId предыдущего дочитывания (протяжка Claude).
    fn read_file(&self, path: &Path, from: u64, seed_prompt: Option<String>) -> Result<FileBatch>;
}

/// Резолв источника по имени. Новые адаптеры добавляются сюда.
pub fn resolve(name: &str) -> Result<Box<dyn DataSource>> {
    match name {
        "claude" => Ok(Box::new(claude::ClaudeSource)),
        "opencode" => Ok(Box::new(opencode::OpenCodeSource::new())),
        other => anyhow::bail!("неизвестный источник: {other} (доступно: claude, opencode)"),
    }
}
