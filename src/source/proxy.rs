//! Адаптер источника `proxy` — нативный JSONL от `quickai proxy` (live-capture).
//!
//! Append-JSONL (как Claude), поэтому дочитываем хвост по байтовому офсету. Одна
//! строка = один захваченный ответ LLM API (см. crate::proxy). cost НЕ задаём —
//! считаем по прайсингу, т.к. модели тут провайдерские (OpenAI и пр.).

use super::{DataSource, Event, FileBatch, TurnData};
use crate::model::Usage;
use anyhow::Result;
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub struct ProxySource;

impl DataSource for ProxySource {
    fn name(&self) -> &'static str {
        "proxy"
    }

    fn root(&self) -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".quickai/proxy")
    }

    fn owns(&self, path: &Path) -> bool {
        path.extension().map(|e| e == "jsonl").unwrap_or(false)
    }

    fn read_file(&self, path: &Path, from: u64, _seed: Option<String>) -> Result<FileBatch> {
        let mut f = std::fs::File::open(path)?;
        let size = f.metadata().map(|m| m.len()).unwrap_or(from);
        f.seek(SeekFrom::Start(from))?;
        let reader = BufReader::new(&mut f as &mut dyn Read);

        let mut events = Vec::new();
        for line in reader.lines() {
            let line = match line {
                Ok(l) if !l.trim().is_empty() => l,
                _ => continue,
            };
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let u = |k: &str| -> u64 { v.get(k).and_then(|x| x.as_u64()).unwrap_or(0) };
            let session = v.get("session_id").and_then(|x| x.as_str()).map(|s| s.to_string());
            let usage = Usage {
                input: u("input"),
                output: u("output") + u("reasoning"), // отдельной колонки под reasoning нет
                cache_write_5m: 0,
                cache_write_1h: 0,
                cache_read: u("cache_read"),
                web_search: 0,
                web_fetch: 0,
            };
            events.push(Event::Turn(TurnData {
                // группируем по сессии-разговору (per-user-turn у прокси нет)
                prompt_id: session.clone(),
                project: String::new(),
                session_id: session,
                agent_run_id: None,
                ext_id: v.get("ext_id").and_then(|x| x.as_str()).map(|s| s.to_string()),
                is_sidechain: false,
                model: v.get("model").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                ts: v.get("ts").and_then(|x| x.as_i64()).unwrap_or(0),
                usage,
                cost: None, // считаем по прайсингу
                stop_reason: v.get("stop_reason").and_then(|x| x.as_str()).map(|s| s.to_string()),
                tool_uses: Vec::new(),
                cwd: String::new(),
                git_branch: String::new(),
                version: String::new(),
            }));
        }

        Ok(FileBatch { events, bytes: size, last_prompt: None })
    }
}
