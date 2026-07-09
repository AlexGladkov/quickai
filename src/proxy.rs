//! `quickai proxy` — live-capture: прозрачный reverse-proxy между агентом и LLM API.
//!
//! Для харнессов, которые НЕ пишут per-turn usage на диск (Codex). Агент смотрит
//! base_url на этот прокси; прокси форвардит запрос как есть (ключ юзера — насквозь,
//! не хранится), а из ответа вытягивает `usage` и дописывает строку в нативный JSONL
//! `~/.quickai/proxy/<день>.jsonl`. Его читает адаптер `source=proxy` (см. crate::source).
//!
//! Инвариант надёжности: извлечение usage — best-effort ПОСЛЕ форварда байтов клиенту.
//! Любая ошибка разбора НЕ влияет на проксируемый ответ — агент не должен ломаться.

use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Method, Response, StatusCode, Uri},
    routing::any,
    Router,
};
use futures_util::StreamExt;
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone)]
struct ProxyState {
    upstream: String,
    client: reqwest::Client,
    sink: PathBuf,
}

/// Поднять прокси (блокирующе). upstream — базовый URL провайдера.
pub fn serve(port: u16, upstream: String) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move { serve_async(port, upstream).await })
}

async fn serve_async(port: u16, upstream: String) -> Result<()> {
    let sink = sink_dir()?;
    let state = Arc::new(ProxyState {
        upstream: upstream.trim_end_matches('/').to_string(),
        client: reqwest::Client::builder().build()?,
        sink,
    });

    let app = Router::new().fallback(any(proxy)).with_state(state.clone());
    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.with_context(|| format!("bind {addr}"))?;

    eprintln!("▶ quickai proxy на http://{addr}  (upstream: {})", state.upstream);
    eprintln!("  usage → {}", state.sink.display());
    eprintln!("  Codex:  base_url = \"http://{addr}/v1\" в ~/.codex/config.toml ([model_providers.*])");
    axum::serve(listener, app).await?;
    Ok(())
}

fn sink_dir() -> Result<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let dir = PathBuf::from(home).join(".quickai/proxy");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Catch-all: форвард любого метода/пути на upstream, tee ответа для извлечения usage.
async fn proxy(
    State(st): State<Arc<ProxyState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response<Body> {
    match proxy_inner(st, method, uri, headers, body).await {
        Ok(resp) => resp,
        Err(e) => {
            // Апстрим недоступен и т.п. — отдать 502, но не паниковать.
            Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Body::from(format!("quickai proxy: upstream error: {e}")))
                .unwrap()
        }
    }
}

async fn proxy_inner(
    st: Arc<ProxyState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response<Body>> {
    let path_q = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let url = format!("{}{}", st.upstream, path_q);

    // Тело запроса целиком (запросы небольшие) — заодно достаём model/session.
    let req_bytes = axum::body::to_bytes(body, usize::MAX).await?;
    let req_json: Option<Value> = serde_json::from_slice(&req_bytes).ok();
    let model = req_json
        .as_ref()
        .and_then(|v| v.get("model"))
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    // Группировка сессии, если провайдер её несёт (Responses: conversation / prompt_cache_key).
    let session = req_json.as_ref().and_then(|v| {
        v.get("conversation")
            .or_else(|| v.get("prompt_cache_key"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
    });

    // Форвардим заголовки как есть, кроме host (reqwest выставит свой).
    let mut fwd = reqwest::header::HeaderMap::new();
    for (k, v) in headers.iter() {
        if k == axum::http::header::HOST {
            continue;
        }
        if let Ok(name) = reqwest::header::HeaderName::from_bytes(k.as_ref()) {
            if let Ok(val) = reqwest::header::HeaderValue::from_bytes(v.as_ref()) {
                fwd.insert(name, val);
            }
        }
    }

    let start = Instant::now();
    let up = st
        .client
        .request(method, &url)
        .headers(fwd)
        .body(req_bytes.to_vec())
        .send()
        .await
        .with_context(|| format!("forward to {url}"))?;

    let status = up.status();
    let is_sse = up
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("event-stream"))
        .unwrap_or(false);

    // Копируем заголовки ответа, кроме hop-by-hop (их выставит axum/hyper).
    let mut out = Response::builder().status(status);
    for (k, v) in up.headers().iter() {
        if matches!(
            k.as_str(),
            "connection" | "transfer-encoding" | "content-length" | "keep-alive"
        ) {
            continue;
        }
        out = out.header(k, v);
    }

    // Tee-стрим: каждый чанк уходит клиенту немедленно + копится для разбора.
    let sink = st.sink.clone();
    let mut acc: Vec<u8> = Vec::new();
    let mut up_stream = up.bytes_stream();
    let tee = async_stream::stream! {
        while let Some(item) = up_stream.next().await {
            match item {
                Ok(chunk) => {
                    acc.extend_from_slice(&chunk);
                    yield Ok::<_, std::io::Error>(chunk);
                }
                Err(e) => {
                    yield Err(std::io::Error::new(std::io::ErrorKind::Other, e));
                    return;
                }
            }
        }
        // Поток закончился — best-effort извлечение (клиент уже всё получил).
        let latency_ms = start.elapsed().as_millis() as i64;
        if let Some(rec) = extract_usage(&acc, is_sse, &model, session.as_deref(), latency_ms) {
            let _ = write_record(&sink, &rec);
        }
    };

    Ok(out.body(Body::from_stream(tee))?)
}

/// Одна запись usage в нативном формате.
struct Record {
    ts: i64,
    model: String,
    session_id: Option<String>,
    ext_id: Option<String>,
    input: u64,
    output: u64,
    reasoning: u64,
    cache_read: u64,
    stop_reason: Option<String>,
    latency_ms: i64,
}

/// Достать usage из ответа. Поддержка OpenAI Chat (`usage`) и Responses
/// (`response.completed` → `response.usage`), стриминг и не-стриминг.
fn extract_usage(
    body: &[u8],
    is_sse: bool,
    req_model: &str,
    session: Option<&str>,
    latency_ms: i64,
) -> Option<Record> {
    let usage_obj: Value;
    let mut model = req_model.to_string();
    let mut ext_id = None;
    let mut stop_reason = None;

    if is_sse {
        // Собираем все data: {...} события, ищем то, где есть usage.
        let text = String::from_utf8_lossy(body);
        let mut found: Option<Value> = None;
        for line in text.lines() {
            let line = line.trim();
            let payload = line.strip_prefix("data:").map(|s| s.trim()).unwrap_or("");
            if payload.is_empty() || payload == "[DONE]" {
                continue;
            }
            let v: Value = match serde_json::from_str(payload) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // Responses API: событие с вложенным response.
            let resp = v.get("response").unwrap_or(&v);
            if resp.get("usage").is_some() {
                if let Some(m) = resp.get("model").and_then(|x| x.as_str()) {
                    model = m.to_string();
                }
                ext_id = resp.get("id").and_then(|x| x.as_str()).map(|s| s.to_string());
                stop_reason = resp
                    .get("status")
                    .or_else(|| resp.get("finish_reason"))
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string());
                found = resp.get("usage").cloned();
            }
        }
        usage_obj = found?;
    } else {
        let v: Value = serde_json::from_slice(body).ok()?;
        let resp = v.get("response").unwrap_or(&v);
        if let Some(m) = resp.get("model").and_then(|x| x.as_str()) {
            model = m.to_string();
        }
        ext_id = resp.get("id").and_then(|x| x.as_str()).map(|s| s.to_string());
        stop_reason = resp
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .or_else(|| resp.get("status"))
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        usage_obj = resp.get("usage").cloned()?;
    }

    // Нормализуем поля usage: OpenAI Chat (prompt_tokens/completion_tokens) и
    // Responses (input_tokens/output_tokens) + детали кэша/reasoning.
    let g = |keys: &[&str]| -> u64 {
        for k in keys {
            if let Some(n) = usage_obj.get(*k).and_then(|x| x.as_u64()) {
                return n;
            }
        }
        0
    };
    let input = g(&["input_tokens", "prompt_tokens"]);
    let output = g(&["output_tokens", "completion_tokens"]);
    let cache_read = usage_obj
        .get("input_tokens_details")
        .or_else(|| usage_obj.get("prompt_tokens_details"))
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let reasoning = usage_obj
        .get("output_tokens_details")
        .or_else(|| usage_obj.get("completion_tokens_details"))
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(0);

    if input == 0 && output == 0 {
        return None; // нет полезной телеметрии
    }

    Some(Record {
        ts: now_ms(),
        model,
        session_id: session.map(|s| s.to_string()),
        ext_id,
        input,
        // OpenAI: cached_tokens входят в prompt_tokens — вычитаем, чтобы не двойного счёта.
        output,
        reasoning,
        cache_read,
        stop_reason,
        latency_ms,
    })
    .map(|mut r| {
        r.input = r.input.saturating_sub(r.cache_read);
        r
    })
}

fn write_record(sink: &PathBuf, r: &Record) -> Result<()> {
    let day = chrono::Local::now().format("%Y-%m-%d");
    let path = sink.join(format!("{day}.jsonl"));
    let line = serde_json::json!({
        "ts": r.ts,
        "model": r.model,
        "session_id": r.session_id,
        "ext_id": r.ext_id,
        "input": r.input,
        "output": r.output,
        "reasoning": r.reasoning,
        "cache_read": r.cache_read,
        "stop_reason": r.stop_reason,
        "latency_ms": r.latency_ms,
    });
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
