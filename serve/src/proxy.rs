//! Port of the per-pane AI proxy (`ai-proxy/pane/:id/*`).
//!
//! This is the piece that makes BYOK safe for coding-agent panes: the pane's CLI is pointed at
//! this endpoint with a PLACEHOLDER key, and the real key never leaves the server. Every request
//! is gated on the pane still being live (the kill-switch) and every response is metered.
//!
//! Streaming matters here — an agent pane consumes SSE token-by-token, so the upstream body is
//! forwarded as a stream rather than buffered. The last 8000 bytes are kept to scrape the usage
//! cost out of the final chunk, exactly as the BE does.
//!
//! ⚠️ NOT VERIFIED against the BE: proving parity needs a live provider key and real network
//! calls. This is written to match the source, not measured against it — a weaker claim than the
//! rest of the surface.

use axum::{
    body::Body,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::dispatch::{engine_call, AppState};

struct Upstream {
    url: &'static str,
    key_file: &'static str,
    env_key: &'static str,
    wants_cost_include: bool,
}

fn upstream_for(target: &str) -> Upstream {
    match target {
        "openai" => Upstream {
            url: "https://api.openai.com/v1/chat/completions",
            key_file: "openai.key",
            env_key: "OPENAI_DIRECT_KEY",
            wants_cost_include: false,
        },
        // legacy/unknown panes default to OpenRouter, unchanged from the BE
        _ => Upstream {
            url: "https://openrouter.ai/api/v1/chat/completions",
            key_file: "openrouter.key",
            env_key: "OPENROUTER_API_KEY",
            wants_cost_include: true,
        },
    }
}

/// Which proxy an engine uses; `None` means a subscription engine with no proxy at all.
fn engine_proxy(pane_kind: &str) -> Option<Option<&'static str>> {
    match pane_kind {
        "opencode" => Some(Some("openrouter")),
        "opencode-direct" => Some(Some("openai")),
        "claude-code" | "codex" => Some(None),
        _ => None,
    }
}

/// Env first, then the substrate's auth dir. The placeholder the pane itself carries is
/// explicitly rejected, otherwise the proxy would forward its own dummy key upstream.
fn proxy_key(up: &Upstream) -> Option<String> {
    if let Ok(v) = std::env::var(up.env_key) {
        let v = v.trim().to_string();
        if !v.is_empty() && v != "antiloki-pane-proxy" {
            return Some(v);
        }
    }
    let home = std::env::var("ANTILOKI_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let mut p = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .map(std::path::PathBuf::from)
                .unwrap_or_default();
            p.push(".antiloki");
            p
        });
    for candidate in [
        home.join("auth").join(up.key_file),
        std::path::PathBuf::from(".antiloki").join("auth").join(up.key_file),
    ] {
        if let Ok(k) = std::fs::read_to_string(&candidate) {
            let k = k.trim().to_string();
            if !k.is_empty() {
                return Some(k);
            }
        }
    }
    None
}

fn err(status: StatusCode, message: String) -> Response {
    (status, Json(json!({ "error": { "message": message } }))).into_response()
}

/// `GET /api/v1/ai-proxy/pane/:id/models` — a fixed one-model list; the pane only needs SOMETHING
/// to select, and the real model choice is made upstream.
pub async fn models() -> Response {
    Json(json!({
        "object": "list",
        "data": [{ "id": "antiloki-pane-proxy", "object": "model", "owned_by": "antiloki" }]
    }))
    .into_response()
}

/// `POST /api/v1/ai-proxy/pane/:id/chat/completions`
pub async fn chat_completions(state: Arc<AppState>, id: String, body: axum::body::Bytes) -> Response {
    let pane_id: i64 = id.trim().parse().unwrap_or(0);
    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });

    // The kill-switch: a pane that is no longer live cannot spend the key.
    let pane = engine_call(&state, "system.cockpit.pane.live", json!({ "id": pane_id }), ctx.clone())
        .await
        .map(|(v, _)| v)
        .unwrap_or(Value::Null);
    if pane.get("live").and_then(|v| v.as_bool()) != Some(true) {
        return err(
            StatusCode::FORBIDDEN,
            format!("pane {id} is not live — proxy refused (antiloki kill-switch)"),
        );
    }

    let pane_kind = pane.get("pane_kind").and_then(|v| v.as_str()).unwrap_or("");
    if let Some(None) = engine_proxy(pane_kind) {
        return err(
            StatusCode::BAD_REQUEST,
            format!("pane {id} is a subscription engine — it has no proxy"),
        );
    }
    let target = engine_proxy(pane_kind).flatten().unwrap_or("openrouter");
    let up = upstream_for(target);

    let Some(key) = proxy_key(&up) else {
        return err(
            StatusCode::BAD_GATEWAY,
            format!("no {target} key on the substrate (Settings → BYOK · .antiloki/auth/{})", up.key_file),
        );
    };

    let mut fwd: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
    if up.wants_cost_include {
        if let Some(o) = fwd.as_object_mut() {
            o.insert("usage".into(), json!({ "include": true }));
        }
    }

    let client = reqwest::Client::new();
    let upstream = match client
        .post(up.url)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {key}"))
        .json(&fwd)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_GATEWAY, format!("upstream unreachable: {e}")),
    };

    let status = StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let ct = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let is_sse = ct.to_lowercase().contains("event-stream");

    // Forward as a stream, keeping a rolling tail so the usage cost can be scraped after the
    // last chunk. Metering is best-effort and must never fail the operator's request.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<axum::body::Bytes, std::io::Error>>();
    let st = state.clone();
    tokio::spawn(async move {
        let mut tail = String::new();
        let mut stream = upstream.bytes_stream();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    tail.push_str(&String::from_utf8_lossy(&bytes));
                    if tail.len() > 8000 {
                        tail = tail.chars().rev().take(8000).collect::<String>().chars().rev().collect();
                    }
                    if tx.send(Ok(bytes)).is_err() {
                        break; // client disconnected — still meter what we saw
                    }
                }
                Err(_) => break,
            }
        }
        drop(tx);
        let cost = scrape_cost(&tail);
        let _ = engine_call(
            &st,
            "system.cockpit.pane.meter",
            json!({ "pane_id": pane_id, "cost_usd": cost }),
            json!({ "actor": "web-fe", "source": "web-fe" }),
        )
        .await;
    });

    let mut res = Response::builder().status(status).header(header::CONTENT_TYPE, ct);
    if is_sse {
        res = res.header(header::CACHE_CONTROL, "no-cache");
    }
    res.body(Body::from_stream(tokio_stream::wrappers::UnboundedReceiverStream::new(rx)))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// `/"usage"\s*:\s*\{[^}]*"cost"\s*:\s*([0-9.eE+-]+)/` — the BE scrapes rather than parses,
/// because the tail may be a partial SSE frame that is not valid JSON on its own.
fn scrape_cost(tail: &str) -> f64 {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r#""usage"\s*:\s*\{[^}]*"cost"\s*:\s*([0-9.eE+-]+)"#).unwrap()
    });
    re.captures(tail)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<f64>().ok())
        .filter(|f| f.is_finite())
        .unwrap_or(0.0)
}
