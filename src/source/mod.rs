//! Граница источника данных (source/adapter).
//!
//! Индексатор (`crate::index`) работает НЕ с конкретным форматом транскриптов, а с
//! нормализованной моделью [`LineEvent`], которую выдаёт адаптер источника. Новый
//! SWE-агент (Codex, OpenCode, …) подключается отдельным `impl DataSource` — без правок
//! индексатора, схемы и агрегатов. Claude Code — первый адаптер (см. [`claude`]).

pub mod claude;

use crate::model::Usage;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Один assistant-turn в нормализованном виде (независимо от формата источника).
pub struct TurnEvent {
    pub session_id: Option<String>,
    pub is_sidechain: bool,
    pub model: String,
    pub ts: i64,
    pub usage: Usage,
    pub stop_reason: Option<String>,
    /// tool_use блоки этого turn'а: (tool_use_id, name).
    pub tool_uses: Vec<(String, String)>,
    // Контекст сессии (берётся с первого turn'а сессии).
    pub cwd: String,
    pub git_branch: String,
    pub version: String,
}

/// Нормализованный результат разбора одной строки транскрипта.
/// Все поля независимы: строка может нести и промпт, и линковку, и turn.
#[derive(Default)]
pub struct LineEvent {
    /// Линковка субагента: (agentId, subagent_type).
    pub agent_link: Option<(String, String)>,
    /// Результаты тулзов: (tool_use_id, is_error).
    pub tool_results: Vec<(String, bool)>,
    /// promptId, заданный этой строкой (протягивается вперёд на turn'ы).
    pub prompt_id: Option<String>,
    /// Очищенный текст промпта (whitespace схлопнут, шум отсеян). None = нет/шум.
    pub prompt_text: Option<String>,
    /// assistant-turn, если строка его несёт.
    pub turn: Option<TurnEvent>,
}

/// Источник данных: где лежат транскрипты и как их разбирать в [`LineEvent`].
pub trait DataSource {
    /// Стабильный id источника — попадает в колонку `source` (claude|codex|opencode|…).
    fn name(&self) -> &'static str;
    /// Корневой каталог транскриптов источника.
    fn root(&self) -> PathBuf;
    /// Принадлежит ли файл источнику (фильтр по расширению/имени).
    fn owns(&self, path: &Path) -> bool;
    /// slug проекта из пути транскрипта.
    fn project_of(&self, path: &Path, root: &Path) -> String;
    /// id субагентского прогона из имени файла (None = основной транскрипт).
    fn agent_run_id_of(&self, path: &Path) -> Option<String>;
    /// Разобрать одну строку в нормализованное событие (None = битая/пустая строка).
    fn parse_line(&self, line: &str) -> Option<LineEvent>;
}

/// Резолв источника по имени. Новые адаптеры добавляются сюда.
pub fn resolve(name: &str) -> Result<Box<dyn DataSource>> {
    match name {
        "claude" => Ok(Box::new(claude::ClaudeSource)),
        other => anyhow::bail!("неизвестный источник: {other} (доступно: claude)"),
    }
}
