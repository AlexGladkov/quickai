//! Адаптер источника Claude Code — транскрипты `~/.claude/projects/**/*.jsonl`.
//!
//! Append-JSONL: читаем хвост файла с байтового офсета. Вся Claude-специфика
//! (схема `RawLine`, `agentId`-линковка, `agent-<id>.jsonl`, шум харнесса,
//! протяжка promptId с user-строк на assistant-turn'ы) живёт здесь.

use super::{DataSource, Event, FileBatch, TurnData};
use crate::parse::{self, record::RawLine};
use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub struct ClaudeSource;

impl ClaudeSource {
    /// slug проекта = имя каталога под projects/.
    fn project_of(path: &Path, root: &Path) -> String {
        path.strip_prefix(root)
            .ok()
            .and_then(|p| p.components().next())
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_default()
    }

    /// id субагентского прогона из имени файла agent-<id>.jsonl.
    fn agent_run_id_of(path: &Path) -> Option<String> {
        let stem = path.file_stem()?.to_str()?;
        stem.strip_prefix("agent-").map(|s| s.to_string())
    }
}

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

    fn read_file(&self, path: &Path, from: u64, seed_prompt: Option<String>) -> Result<FileBatch> {
        let root = self.root();
        let project = Self::project_of(path, &root);
        let agent_run_id = Self::agent_run_id_of(path); // Some для субагентских файлов

        let mut f = std::fs::File::open(path).with_context(|| format!("open {path:?}"))?;
        let size = f.metadata().map(|m| m.len()).unwrap_or(from);
        f.seek(SeekFrom::Start(from))?;
        let reader = BufReader::new(&mut f as &mut dyn Read);

        let mut events = Vec::new();
        let mut last_prompt = seed_prompt;

        for line in reader.lines() {
            let line = match line {
                Ok(l) if !l.trim().is_empty() => l,
                _ => continue,
            };
            let rec: RawLine = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(_) => continue, // битая строка — пропуск
            };

            if let Some((id, agent_type)) = rec.agent_link() {
                events.push(Event::AgentLink { id, agent_type });
            }

            if rec.kind.as_deref() == Some("user") {
                if let Some(content) = rec.message.as_ref().and_then(|m| m.content.as_ref()) {
                    for (tool_use_id, is_error) in parse::tool_results(content) {
                        events.push(Event::ToolResult { tool_use_id, is_error });
                    }
                }
            }

            // user-строка задаёт promptId; текст промпта (без шума).
            if let Some(pid) = &rec.prompt_id {
                last_prompt = Some(pid.clone());
                let text = rec
                    .message
                    .as_ref()
                    .and_then(|m| m.content.as_ref())
                    .and_then(parse::user_text)
                    .and_then(clean_prompt);
                events.push(Event::Prompt {
                    prompt_id: pid.clone(),
                    text,
                    agent_run_id: agent_run_id.clone(),
                });
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
                            events.push(Event::Turn(TurnData {
                                prompt_id: last_prompt.clone(), // протяжка текущего promptId
                                project: project.clone(),
                                session_id: rec.session_id.clone(),
                                agent_run_id: agent_run_id.clone(),
                                ext_id: None, // append-only, дублей нет
                                is_sidechain: rec.is_sidechain.unwrap_or(false),
                                model,
                                ts,
                                usage: parse::usage_from_raw(u),
                                cost: None, // считаем по прайсингу
                                stop_reason: msg.stop_reason.clone(),
                                tool_uses,
                                cwd: rec.cwd.clone().unwrap_or_default(),
                                git_branch: rec.git_branch.clone().unwrap_or_default(),
                                version: rec.version.clone().unwrap_or_default(),
                            }));
                        }
                    }
                }
            }
        }

        Ok(FileBatch { events, bytes: size, last_prompt })
    }
}

/// Схлопнуть whitespace + отсечь системный шум харнесса. None = шум/пусто.
fn clean_prompt(text: String) -> Option<String> {
    let clean: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let is_noise = clean.is_empty()
        || clean.starts_with("<task-notification")
        || clean.starts_with("<local-command")
        || clean.starts_with("Caveman mode");
    if is_noise {
        None
    } else {
        Some(clean)
    }
}
