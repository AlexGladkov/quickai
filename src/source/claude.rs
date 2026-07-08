//! Адаптер источника Claude Code — транскрипты `~/.claude/projects/**/*.jsonl`.
//!
//! Вся Claude-специфика (JSONL-схема `RawLine`, линковка субагентов через `agentId`,
//! файлы `agent-<id>.jsonl`, шум харнесса) живёт здесь, за границей [`DataSource`].

use super::{DataSource, LineEvent, TurnEvent};
use crate::parse::{self, record::RawLine};
use std::path::{Path, PathBuf};

pub struct ClaudeSource;

impl DataSource for ClaudeSource {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn root(&self) -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".claude/projects")
    }

    fn owns(&self, path: &Path) -> bool {
        path.extension().map(|e| e == "jsonl").unwrap_or(false)
    }

    /// slug проекта = имя каталога под projects/.
    fn project_of(&self, path: &Path, root: &Path) -> String {
        path.strip_prefix(root)
            .ok()
            .and_then(|p| p.components().next())
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_default()
    }

    /// id субагентского прогона из имени файла agent-<id>.jsonl.
    fn agent_run_id_of(&self, path: &Path) -> Option<String> {
        let stem = path.file_stem()?.to_str()?;
        stem.strip_prefix("agent-").map(|s| s.to_string())
    }

    fn parse_line(&self, line: &str) -> Option<LineEvent> {
        let rec: RawLine = serde_json::from_str(line).ok()?;
        let mut ev = LineEvent::default();

        // Линковка субагента: agentId ↔ subagent_type из toolUseResult.
        ev.agent_link = rec.agent_link();

        // tool_result (user-строки) → is_error по tool_use_id.
        if rec.kind.as_deref() == Some("user") {
            if let Some(content) = rec.message.as_ref().and_then(|m| m.content.as_ref()) {
                ev.tool_results = parse::tool_results(content);
            }
        }

        // user-строка задаёт promptId; заодно ловим текст промпта (без шума).
        if let Some(pid) = &rec.prompt_id {
            ev.prompt_id = Some(pid.clone());
            if let Some(text) = rec
                .message
                .as_ref()
                .and_then(|m| m.content.as_ref())
                .and_then(parse::user_text)
            {
                let clean: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
                let is_noise = clean.is_empty()
                    || clean.starts_with("<task-notification")
                    || clean.starts_with("<local-command")
                    || clean.starts_with("Caveman mode");
                if !is_noise {
                    ev.prompt_text = Some(clean);
                }
            }
        }

        // assistant-turn с валидной usage и реальной моделью.
        if rec.kind.as_deref() == Some("assistant") {
            if let Some(msg) = &rec.message {
                if let Some(u) = &msg.usage {
                    let model = msg.model.clone().unwrap_or_default();
                    if model != "<synthetic>" && !model.is_empty() {
                        let ts = rec.timestamp.as_deref().map(parse::ts_to_ms).unwrap_or(0);
                        let tool_uses = msg
                            .content
                            .as_ref()
                            .map(parse::tool_uses)
                            .unwrap_or_default();
                        ev.turn = Some(TurnEvent {
                            session_id: rec.session_id.clone(),
                            is_sidechain: rec.is_sidechain.unwrap_or(false),
                            model,
                            ts,
                            usage: parse::usage_from_raw(u),
                            stop_reason: msg.stop_reason.clone(),
                            tool_uses,
                            cwd: rec.cwd.clone().unwrap_or_default(),
                            git_branch: rec.git_branch.clone().unwrap_or_default(),
                            version: rec.version.clone().unwrap_or_default(),
                        });
                    }
                }
            }
        }

        Some(ev)
    }
}
