//! Адаптер источника OpenCode — хранилище `~/.local/share/opencode/storage`.
//!
//! Не JSONL: данные — иммутабельные JSON-файлы. Одно сообщение = один файл
//! `message/<sessionID>/msg_*.json`; его содержимое (текст промпта, вызовы тулзов) —
//! в соседних `part/<messageID>/prt_*.json`; контекст сессии (каталог, версия) —
//! в `session/<projectID>/<sessionID>.json`. Токены/cost/модель лежат прямо в
//! assistant-сообщении, поэтому cost берём готовый (не пересчитываем прайсингом).

use super::{DataSource, Event, FileBatch, TurnData};
use crate::model::Usage;
use anyhow::Result;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct OpenCodeSource {
    storage: PathBuf,
    /// Кэш sessionID → (directory, version) — session-json читаем один раз.
    sessions: RefCell<HashMap<String, (String, String)>>,
}

impl OpenCodeSource {
    pub fn new() -> Self {
        let base = std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                PathBuf::from(home).join(".local/share")
            });
        OpenCodeSource {
            storage: base.join("opencode/storage"),
            sessions: RefCell::new(HashMap::new()),
        }
    }

    /// (directory, version) сессии по id. Файл называется <sessionID>.json под session/*/.
    fn session_ctx(&self, sid: &str) -> (String, String) {
        if let Some(v) = self.sessions.borrow().get(sid) {
            return v.clone();
        }
        let mut ctx = (String::new(), String::new());
        let target = format!("{sid}.json");
        for entry in walkdir::WalkDir::new(self.storage.join("session"))
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_name().to_string_lossy() == target {
                if let Ok(v) = read_json(entry.path()) {
                    let dir = v.get("directory").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let ver = v.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    ctx = (dir, ver);
                }
                break;
            }
        }
        self.sessions.borrow_mut().insert(sid.to_string(), ctx.clone());
        ctx
    }

    /// Разобрать части сообщения: собрать текст + вызовы/ошибки тулзов.
    fn parts_of(&self, msg_id: &str) -> (String, Vec<(String, String)>, Vec<(String, bool)>) {
        let dir = self.storage.join("part").join(msg_id);
        let mut text = String::new();
        let mut tool_uses = Vec::new();
        let mut tool_results = Vec::new();
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => return (text, tool_uses, tool_results),
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let v = match read_json(&entry.path()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match v.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(t) = v.get("text").and_then(|t| t.as_str()) {
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        text.push_str(t);
                    }
                }
                Some("tool") => {
                    let call_id = v.get("callID").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let name = v.get("tool").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    if !call_id.is_empty() {
                        tool_uses.push((call_id.clone(), name));
                        let status = v.get("state").and_then(|s| s.get("status")).and_then(|s| s.as_str());
                        if status == Some("error") {
                            tool_results.push((call_id, true));
                        }
                    }
                }
                _ => {}
            }
        }
        (text, tool_uses, tool_results)
    }
}

impl DataSource for OpenCodeSource {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn root(&self) -> PathBuf {
        self.storage.join("message")
    }

    fn owns(&self, path: &Path) -> bool {
        path.extension().map(|e| e == "json").unwrap_or(false)
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("msg_"))
                .unwrap_or(false)
    }

    /// Один файл = одно сообщение. Иммутабельно → офсет игнорируем, читаем целиком.
    fn read_file(&self, path: &Path, _from: u64, _seed: Option<String>) -> Result<FileBatch> {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let msg = read_json(path)?;
        let mut events = Vec::new();

        let id = msg.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let sid = msg.get("sessionID").and_then(|x| x.as_str()).map(|s| s.to_string());
        let role = msg.get("role").and_then(|x| x.as_str()).unwrap_or("");

        match role {
            "user" => {
                let (text, _, _) = self.parts_of(&id);
                let clean = clean_prompt(text);
                if !id.is_empty() {
                    events.push(Event::Prompt { prompt_id: id, text: clean, agent_run_id: None });
                }
            }
            "assistant" => {
                let tk = msg.get("tokens");
                let g = |a: &str, b: &str| -> u64 {
                    tk.and_then(|t| t.get(a)).and_then(|x| x.get(b)).and_then(|x| x.as_u64()).unwrap_or(0)
                };
                let f = |a: &str| -> u64 {
                    tk.and_then(|t| t.get(a)).and_then(|x| x.as_u64()).unwrap_or(0)
                };
                let usage = Usage {
                    input: f("input"),
                    // reasoning складываем в output — отдельной колонки нет, но токены реальны.
                    output: f("output") + f("reasoning"),
                    cache_write_5m: g("cache", "write"), // 5m/1h split у OpenCode нет
                    cache_write_1h: 0,
                    cache_read: g("cache", "read"),
                    web_search: 0,
                    web_fetch: 0,
                };
                let (_, tool_uses, tool_results) = self.parts_of(&id);
                for (tool_use_id, is_error) in tool_results {
                    events.push(Event::ToolResult { tool_use_id, is_error });
                }
                let (cwd, version) = sid
                    .as_deref()
                    .map(|s| self.session_ctx(s))
                    .unwrap_or_default();
                let model = msg.get("modelID").and_then(|x| x.as_str()).unwrap_or("").to_string();
                events.push(Event::Turn(TurnData {
                    prompt_id: msg.get("parentID").and_then(|x| x.as_str()).map(|s| s.to_string()),
                    project: cwd.clone(),
                    session_id: sid,
                    agent_run_id: None,
                    is_sidechain: false,
                    ext_id: Some(id), // дедуп при повторном чтении (иммутабельный файл)
                    model,
                    ts: msg.get("time").and_then(|t| t.get("created")).and_then(|x| x.as_i64()).unwrap_or(0),
                    usage,
                    cost: msg.get("cost").and_then(|x| x.as_f64()),
                    stop_reason: msg.get("finish").and_then(|x| x.as_str()).map(|s| s.to_string()),
                    tool_uses,
                    cwd,
                    git_branch: String::new(),
                    version,
                }));
            }
            _ => {}
        }

        Ok(FileBatch { events, bytes: size, last_prompt: None })
    }
}

fn read_json(path: &Path) -> Result<Value> {
    let s = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&s)?)
}

/// Схлопнуть whitespace. None = пусто.
fn clean_prompt(text: String) -> Option<String> {
    let clean: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.is_empty() {
        None
    } else {
        Some(clean)
    }
}
