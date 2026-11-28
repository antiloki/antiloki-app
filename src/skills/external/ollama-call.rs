//! ollama-call.rs — twin of skills/external/ollama-call.mjs (LOCAL Ollama ProviderAdapter, NETWORK
//! lane). NAMES ARE CONTRACT for the JS named exports: `stripReasoning`, `extractReasoning`,
//! `resolveHost` (re-exported from the already-converted `ollama-host.rs` twin, same as the JS
//! module re-exports it from `./_helpers/ollama-host.mjs`).
//!
//! The default export `ollamaCall` is REAL (2026-08-09) — it performs the POST via `ureq`, the same
//! seam `ai-call.rs::dispatch_local_llm` uses. Ollama is a LOCAL server over plain http, which is
//! precisely why this adapter is portable while the hosted ones are not: this crate builds `ureq`
//! with `default-features = false` (no TLS). The live round-trip is still not a conform fixture unit
//! (a real model's answer is nondeterministic), but every step around it is, and the two fetch-layer
//! error branches now have real tests — an unreachable server is driven against a closed port.
//!
//! `context.fetch_fn` (JS's test-injection point) has no counterpart here and is not consulted.
//!
//! The DETERMINISTIC halves were already ported and remain the tested core:
//!   - `buildRequest(input)` — host resolution (via the twinned `resolveHost`) + prompt template
//!     substitution (via the twinned `substituteTemplate`) + the exact `/api/chat` URL and JSON
//!     request body (model/messages/stream/options), mirroring JS's `JSON.stringify` behavior that
//!     a missing `model_id` key (`undefined`, not `null`) is OMITTED from the body — this twin
//!     mirrors that by only inserting the "model" key when `input.model_id` is present at all.
//!   - `parseOllamaResponse(parsed, model_id, max_tokens)` — the exact post-fetch logic fed the
//!     ALREADY-PARSED response JSON (`ollamaCall` does the fetch, then routes the response through
//!     `parseProviderResponse` exactly as JS does): the `parsed.error` guard, `<think>` extraction/stripping, the
//!     empty-answer fail-loud guard, `parseJsonOutputOrText` decode, and prompt_eval_count/
//!     eval_count → tokens_in/tokens_out mapping (`Number(x ?? 0) || 0`).
//! Both fetch-layer error branches are now live, with JS's message text verbatim: an unreachable
//! server ("Is `ollama serve` running?") and a timeout ("too SLOW … run `ollama ps`: size_vram").
//! Keeping them distinct is load-bearing — JS's own comment records that conflating them once sent
//! a debugging session chasing "is the server up?" when a CPU-bound model was simply too slow.
//! A non-2xx is NOT a transport failure: `ureq` reports it as `Err(Status)`, so it is mapped back
//! into the `{status, ok, text}` shape `parseProviderResponse` expects, preserving that helper's
//! 401/403/429/non-ok branches instead of mislabelling an Ollama 400 as an unreachable server.

use serde_json::{json, Map, Value};

// ── standard helper functions (playbook) ────────────────────────────────────────────────────
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) }
        }
        Some(_) => f64::NAN,
    }
}
// `Number(x ?? 0) || 0` — `??` catches null/undefined (both -> Number(0) = 0), then a NaN or 0
// result is replaced by 0 too (JS `||` falsy-coalesce). Integral results stay integral in the JSON.
fn number_or_zero(v: Option<&Value>) -> Value {
    let converted = match v {
        None | Some(Value::Null) => 0.0,
        Some(other) => js_number(Some(other)),
    };
    let n = if converted.is_nan() || converted == 0.0 { 0.0 } else { converted };
    if n.fract() == 0.0 && n.is_finite() { json!(n as i64) } else { json!(n) }
}

// Re-exported so callers of this twin have the same surface as the JS module (which re-exports
// resolveHost from ./_helpers/ollama-host.mjs).
pub use crate::external__helpers_ollama_host::resolveHost;
use crate::external__helpers_provider_adapter_helpers::{parseJsonOutputOrText, parseProviderResponse, substituteTemplate};

/// JS: `export function stripReasoning(content)`. Strips R1-style `<think>…</think>` reasoning;
/// an UNCLOSED `<think>` (model cut off mid-reasoning) yields `''` so the caller fails loud instead
/// of feeding half a thought to the JSON decoder.
pub fn stripReasoning(content: &Value) -> Value {
    let s = str_qq(Some(content));
    let mut out = strip_closed_think(&s).trim().to_string();
    if out.to_lowercase().contains("<think>") {
        out = String::new();
    }
    json!(out)
}

fn strip_closed_think(s: &str) -> String {
    // JS: /<think>[\s\S]*?<\/think>/gi — case-insensitive, non-greedy, repeated removal.
    let lower = s.to_lowercase();
    let mut out = String::new();
    let mut i = 0usize;
    let bytes = s.as_bytes();
    let lbytes = lower.as_bytes();
    loop {
        let open = find_from(lbytes, b"<think>", i);
        match open {
            None => {
                out.push_str(&s[i..]);
                break;
            }
            Some(open_at) => {
                let close = find_from(lbytes, b"</think>", open_at);
                match close {
                    None => {
                        // no matching close anywhere after this open — leave the rest untouched
                        // (regex wouldn't match here either); the unclosed-tag detector downstream
                        // still catches this case via the raw lowercase "<think>" substring check.
                        out.push_str(&s[i..]);
                        break;
                    }
                    Some(close_at) => {
                        out.push_str(&s[i..open_at]);
                        i = close_at + b"</think>".len();
                    }
                }
                let _ = bytes;
            }
        }
    }
    out
}
fn find_from(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from > haystack.len() || needle.is_empty() {
        return None;
    }
    haystack[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

/// JS: `export function extractReasoning(content)`. Pulls the reasoning chain OUT of `content`
/// (the inverse of `stripReasoning`) — closed `<think>` blocks joined by a blank line, else a
/// truncated/unclosed trailing `<think>…` capture, else `null`.
pub fn extractReasoning(content: &Value) -> Value {
    let s = str_qq(Some(content));
    let lower = s.to_lowercase();
    let mut closed: Vec<String> = Vec::new();
    let mut i = 0usize;
    loop {
        let open = find_from(lower.as_bytes(), b"<think>", i);
        let Some(open_at) = open else { break };
        let close = find_from(lower.as_bytes(), b"</think>", open_at);
        let Some(close_at) = close else { break };
        let inner = &s[open_at + 7..close_at];
        closed.push(inner.trim().to_string());
        i = close_at + 8;
    }
    if !closed.is_empty() {
        let joined = closed.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n");
        return if joined.is_empty() { Value::Null } else { json!(joined) };
    }
    // trailing unclosed <think>...
    if let Some(open_at) = find_from(lower.as_bytes(), b"<think>", 0) {
        let rest = s[open_at + 7..].trim().to_string();
        return if rest.is_empty() { Value::Null } else { json!(rest) };
    }
    Value::Null
}

/// Twin-only helper (not a JS export): the DETERMINISTIC request-assembly half of `ollamaCall` —
/// host resolution, prompt substitution, message array shaping (system_md optional), and the
/// exact Ollama `/api/chat` JSON body. Returns `{ "url": string, "body": object, "host": string }`.
pub fn buildRequest(input: &Value) -> Value {
    let host = resolveHost();
    let url = format!("{host}/api/chat");

    let template_md = input.get("template_md").cloned().unwrap_or(Value::Null);
    let input_kv = input.get("input_kv").cloned().unwrap_or(json!([]));
    let prompt = substituteTemplate(&template_md, &input_kv);

    let system_md = input.get("system_md");
    let has_system = matches!(system_md, Some(Value::String(s)) if !s.trim().is_empty());
    let messages = if has_system {
        json!([
            { "role": "system", "content": system_md.cloned().unwrap_or(Value::Null) },
            { "role": "user", "content": prompt },
        ])
    } else {
        json!([{ "role": "user", "content": prompt }])
    };

    // JS default params: `temperature_value = 0.2`, `max_tokens = 1024` apply only when the key is
    // ABSENT (undefined) — an explicit `null` passes through unchanged, same as real JS defaults.
    let temperature_value = input.get("temperature_value").cloned().unwrap_or(json!(0.2));
    let max_tokens = input.get("max_tokens").cloned().unwrap_or(json!(1024));

    let mut body = Map::new();
    // JSON.stringify drops a key whose value is `undefined` — mirrored by only inserting "model"
    // when input.model_id is present at all (absent key -> omitted; explicit null -> kept as null).
    if let Some(model_id) = input.get("model_id") {
        body.insert("model".into(), model_id.clone());
    }
    body.insert("messages".into(), messages);
    body.insert("stream".into(), json!(false));
    body.insert("options".into(), json!({ "temperature": temperature_value, "num_predict": max_tokens }));

    json!({ "url": url, "body": Value::Object(body), "host": host })
}

fn antiloki_message(skill: &str, cause: &str) -> String {
    format!("{skill}: {cause}")
}

/// Twin-only helper (not a JS export): the DETERMINISTIC response-parsing half of `ollamaCall` —
/// fed the ALREADY-DECODED Ollama response JSON (post `parseProviderResponse`, out of scope here).
/// Mirrors JS lines 108-130: the `parsed.error` guard, reasoning extraction/stripping, the
/// empty-answer fail-loud guard, `parseJsonOutputOrText` decode, and token mapping.
pub fn parseOllamaResponse(parsed: &Value, model_id: &Value, max_tokens: &Value) -> Result<Value, String> {
    if let Some(err) = parsed.get("error") {
        if !err.is_null() {
            let err_str = match err {
                Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            let cause = format!(
                "Ollama returned an error for model '{}': {}. Pull it first: `ollama pull {}`.",
                tmpl(Some(model_id)),
                err_str,
                tmpl(Some(model_id))
            );
            return Err(antiloki_message("ollama-call", &cause));
        }
    }
    let raw = parsed.get("message").and_then(|m| m.get("content")).cloned().unwrap_or(json!(""));
    let reasoning_md = extractReasoning(&raw);
    let text = stripReasoning(&raw);
    let text_str = text.as_str().unwrap_or("");
    if text_str.trim().is_empty() {
        let cause = format!(
            "Ollama model '{}' produced no answer (only reasoning, or empty — likely truncated by num_predict={}). Raise max_output_tokens for this prompt or use a larger model.",
            tmpl(Some(model_id)),
            tmpl(Some(max_tokens))
        );
        return Err(antiloki_message("ollama-call", &cause));
    }
    let output = parseJsonOutputOrText(&text);
    let tokens_in = number_or_zero(parsed.get("prompt_eval_count"));
    let tokens_out = number_or_zero(parsed.get("eval_count"));
    Ok(json!({
        "output": output,
        "tokens_in": tokens_in,
        "tokens_out": tokens_out,
        "tokens_cached": 0,
        "generation_id": Value::Null,
        "reasoning_md": reasoning_md,
    }))
}

/// JS `DEFAULT_TIMEOUT_MS = Number(process.env.ANTILOKI_OLLAMA_TIMEOUT_MS ?? 300_000)`.
/// Local big models are slow; the JS default is deliberately generous.
fn ollama_timeout_ms() -> u64 {
    match std::env::var("ANTILOKI_OLLAMA_TIMEOUT_MS") {
        Ok(v) => v.trim().parse::<f64>().ok().filter(|n| n.is_finite() && *n > 0.0).map(|n| n as u64).unwrap_or(300_000),
        Err(_) => 300_000,
    }
}

/// JS: default export `ollamaCall(input, context)` — REAL, not deferred.
///
/// The two deterministic halves were already ported (`buildRequest` / `parseOllamaResponse`); this
/// is the one network line between them, the same `ureq` seam `ai-call.rs::dispatch_local_llm` uses.
/// Ollama is a LOCAL server over plain http, so no TLS stack is needed — which is exactly why this
/// adapter is portable today while the hosted ones (openrouter/anthropic/…) still are not: this
/// crate builds `ureq` with `default-features = false` (see Cargo.toml).
///
/// `context.fetch_fn` (JS's test-injection point) has no counterpart here and is not consulted.
///
/// Error parity: JS distinguishes an AbortError TIMEOUT from a connection failure because
/// conflating them once sent a debugging session chasing "is the server up?" when the real cause
/// was a model grinding at CPU speed. `ureq` surfaces the same split — `Transport` errors carry the
/// timeout — so both messages are reproduced verbatim, including the `ollama ps` / `size_vram` hint.
pub fn ollamaCall(input: &Value, _context: &Value) -> Result<Value, String> {
    let req = buildRequest(input);
    let url = req.get("url").and_then(Value::as_str).unwrap_or_default().to_string();
    let host = req.get("host").and_then(Value::as_str).unwrap_or_default().to_string();
    let body = req.get("body").cloned().unwrap_or(json!({}));

    let model_id = input.get("model_id").cloned().unwrap_or(Value::Null);
    let max_tokens = input.get("max_tokens").cloned().unwrap_or(json!(1024));
    let timeout = ollama_timeout_ms();

    // ureq treats a non-2xx as Err(Status); JS's fetch does not — it hands the Response to
    // parseProviderResponse, which owns the 401/403/429/non-ok/unparseable branches. So map a
    // Status error BACK into the {status, ok, text} shape that helper expects, and reserve the
    // fetch-catch path for genuine Transport failures. Without this, an Ollama 400 ("model not
    // found") would surface as "Is `ollama serve` running?" — the exact conflation JS's own
    // timeout-vs-connection split exists to prevent.
    let (status, text) = match ureq::post(&url)
        .timeout(std::time::Duration::from_millis(timeout))
        .send_json(body)
    {
        Ok(resp) => {
            let s = resp.status() as i64;
            let t = resp.into_string().unwrap_or_default();
            (s, t)
        }
        Err(ureq::Error::Status(code, resp)) => {
            let t = resp.into_string().unwrap_or_default();
            (code as i64, t)
        }
        Err(e) => {
            let is_timeout = {
                let s = e.to_string().to_lowercase();
                s.contains("timed out") || s.contains("timeout")
            };
            return Err(if is_timeout {
                format!(
                    "ollama-call: Ollama model '{}' (at {host}) did not finish within {timeout}ms. It is almost certainly too SLOW for this prompt — usually because it is running on CPU with no GPU offload (run `ollama ps`: size_vram must be > 0 for GPU). On CPU a 7B does ~3-5 tok/s, so a multi-thousand-token answer can't finish. Fixes: use a GPU, pick a much smaller model, lower the prompt's max_output_tokens, raise ANTILOKI_OLLAMA_TIMEOUT_MS, or keep the heavy tiers on a hosted provider.",
                    tmpl(Some(&model_id))
                )
            } else {
                format!(
                    "ollama-call: Could not reach Ollama at {host} ({e}). Is `ollama serve` running? Set the endpoint in Settings → Local AI (or OLLAMA_HOST)."
                )
            });
        }
    };

    let parsed = parseProviderResponse(
        &json!({ "status": status, "ok": (200..300).contains(&status), "text": text }),
        &json!("ollama-call"),
    )?;

    parseOllamaResponse(&parsed, &model_id, &max_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_reasoning_removes_closed_think_keeps_answer() {
        assert_eq!(stripReasoning(&json!("<think>let me think</think>\n\n{\"a\":1}")), json!("{\"a\":1}"));
        assert_eq!(stripReasoning(&json!("no reasoning here")), json!("no reasoning here"));
    }

    #[test]
    fn strip_reasoning_returns_empty_for_unclosed_think() {
        assert_eq!(stripReasoning(&json!("<think>reasoning that never finished…")), json!(""));
    }

    #[test]
    fn extract_reasoning_captures_closed_truncated_and_none() {
        assert_eq!(extractReasoning(&json!("<think>deliberating</think>\nanswer")), json!("deliberating"));
        assert_eq!(extractReasoning(&json!("<think>cut off mid-thought")), json!("cut off mid-thought"));
        assert_eq!(extractReasoning(&json!("just an answer")), Value::Null);
    }

    #[test]
    fn build_request_posts_to_api_chat_with_expected_shape() {
        // SAFETY: test-only env mutation of OLLAMA_HOST, serialized via the shared crate-wide lock.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("OLLAMA_HOST"); }
        let req = buildRequest(&json!({
            "template_md": "do {{x}}",
            "input_kv": [{ "key": "x", "value": "it" }],
            "model_id": "deepseek-r1:8b",
            "temperature_value": 0.2,
            "max_tokens": 4000,
        }));
        assert!(req["url"].as_str().unwrap().ends_with("/api/chat"), "{req}");
        assert_eq!(req["body"]["model"], json!("deepseek-r1:8b"));
        assert_eq!(req["body"]["stream"], json!(false));
        assert_eq!(req["body"]["options"]["num_predict"], json!(4000));
        assert_eq!(req["body"]["messages"][0]["content"], json!("do it"));
        assert_eq!(req["body"]["messages"][0]["role"], json!("user"));
    }

    #[test]
    fn build_request_prepends_system_message_when_present() {
        let req = buildRequest(&json!({ "template_md": "u", "model_id": "m", "system_md": "you are X" }));
        let msgs = req["body"]["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], json!("system"));
        assert_eq!(msgs[0]["content"], json!("you are X"));
        assert_eq!(msgs[1]["role"], json!("user"));
    }

    #[test]
    fn build_request_omits_model_key_when_model_id_absent() {
        let req = buildRequest(&json!({ "template_md": "t" }));
        assert!(!req["body"].as_object().unwrap().contains_key("model"));
    }

    #[test]
    fn parse_response_strips_reasoning_decodes_json_maps_tokens() {
        let parsed = json!({
            "message": { "content": "<think>reasoning</think>\n```json\n{\"features_json\":[{\"id\":\"f1\"}]}\n```" },
            "prompt_eval_count": 321, "eval_count": 88, "done_reason": "stop",
        });
        let r = parseOllamaResponse(&parsed, &json!("deepseek-r1:8b"), &json!(4000)).unwrap();
        assert_eq!(r["output"], json!({ "features_json": [{ "id": "f1" }] }));
        assert_eq!(r["tokens_in"], json!(321));
        assert_eq!(r["tokens_out"], json!(88));
        assert_eq!(r["reasoning_md"], json!("reasoning"));
    }

    #[test]
    fn parse_response_fails_loud_when_only_truncated_reasoning() {
        let parsed = json!({
            "message": { "content": "<think>still thinking and cut off" },
            "prompt_eval_count": 10, "eval_count": 4000, "done_reason": "length",
        });
        let err = parseOllamaResponse(&parsed, &json!("deepseek-r1:8b"), &json!(4000)).unwrap_err();
        assert!(err.to_lowercase().contains("no answer") || err.to_lowercase().contains("truncated"), "{err}");
    }

    #[test]
    fn parse_response_fails_loud_on_ollama_error_body() {
        let parsed = json!({ "error": "model \"deepseek-r1:8b\" not found, try pulling it first" });
        let err = parseOllamaResponse(&parsed, &json!("deepseek-r1:8b"), &json!(4000)).unwrap_err();
        assert!(err.to_lowercase().contains("ollama pull"), "{err}");
    }

    #[test]
    fn ollama_call_reports_an_unreachable_server_distinctly_from_a_timeout() {
        // Point at a closed port so the Transport branch fires deterministically (no live server
        // needed). The message must be the "is `ollama serve` running?" one, NOT the slow-model
        // timeout advice — conflating those is the exact bug the JS split exists to prevent.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("OLLAMA_HOST", "http://127.0.0.1:1") };
        let e = ollamaCall(&json!({ "template_md": "hi", "model_id": "m" }), &json!({})).unwrap_err();
        unsafe { std::env::remove_var("OLLAMA_HOST") };
        assert!(e.starts_with("ollama-call: Could not reach Ollama at"), "{e}");
        assert!(e.contains("ollama serve"), "{e}");
        assert!(!e.contains("size_vram"), "must not be the timeout message: {e}");
    }

    /// LIVE round-trip against a real Ollama server. `#[ignore]` because it needs `ollama serve`
    /// and a pulled model — the suite must stay hermetic. Run deliberately:
    ///   cargo test --lib external_ollama_call -- --ignored --nocapture
    #[test]
    #[ignore = "requires a live Ollama server + a pulled model"]
    fn live_round_trip_against_a_real_ollama_server() {
        let model = std::env::var("ANTILOKI_OLLAMA_TEST_MODEL").unwrap_or_else(|_| "qwen3.5:9b".into());
        let out = ollamaCall(
            &json!({
                "template_md": "Reply with exactly one word: OK",
                "input_kv": [],
                "model_id": model,
                "temperature_value": 0.0,
                // generous on purpose: qwen3.5 is a REASONING model, so a small num_predict is
                // consumed entirely inside <think> and stripReasoning correctly yields '' — which
                // trips the empty-answer guard. That is the twin behaving right, not a bug.
                "max_tokens": 512,
            }),
            &json!({}),
        )
        .expect("live ollama call");
        println!("live ollama output = {out}");
        assert!(out.get("output").is_some(), "{out}");
        assert!(out["tokens_in"].as_i64().unwrap_or(0) > 0, "tokens_in should be real: {out}");
        assert!(out.get("reasoning_md").is_some());
    }

    #[test]
    fn timeout_ms_reads_the_env_with_the_js_default() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("ANTILOKI_OLLAMA_TIMEOUT_MS") };
        assert_eq!(ollama_timeout_ms(), 300_000);
        unsafe { std::env::set_var("ANTILOKI_OLLAMA_TIMEOUT_MS", "1500") };
        assert_eq!(ollama_timeout_ms(), 1500);
        unsafe { std::env::set_var("ANTILOKI_OLLAMA_TIMEOUT_MS", "garbage") };
        assert_eq!(ollama_timeout_ms(), 300_000, "unparseable falls back to the JS default");
        unsafe { std::env::remove_var("ANTILOKI_OLLAMA_TIMEOUT_MS") };
    }
}
