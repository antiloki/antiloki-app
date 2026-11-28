//! openrouter-call.rs — twin of skills/external/openrouter-call.mjs. NAMES ARE CONTRACT: default
//! `openrouterCall`. ProviderAdapter for OpenRouter's OpenAI-compatible chat-completions endpoint
//! (https://openrouter.ai/api/v1/chat/completions) — model_id encodes the upstream provider
//! (openai/gpt-4o-mini, anthropic/claude-3.5-sonnet, ...).
//!
//! LANE A (bespoke-native network adapter, crossing playbook): the real
//! `fetchFn(OPENROUTER_URL, ...)` round-trip is the DEFERRED boundary — genuinely
//! non-deterministic (a live provider's response), not JSON-fixturable, unconvertible IN
//! PRINCIPLE. Everything DETERMINISTIC around it is ported faithfully as pure fns and IS what
//! this file's tests exercise:
//! - `build_request(input, api_key)`: assembles the exact request URL/headers/body the JS sends.
//!   `substituteTemplate` reused byte-for-byte from the already-converted
//!   `external__helpers_provider_adapter_helpers` twin (cross-module-call convention). Mirrors the
//!   JS `:online` web-search suffix rule, the optional leading system message (WS-2), the optional
//!   `response_format` / `session_id` fields (only attached when present, replay-identity
//!   preserving), and the fixed headers (`HTTP-Referer`/`X-Title`, env-overridable).
//! - `parse_response_body(parsed, model_id)`: mirrors the JS post-`parseProviderResponse` tail —
//!   the `parsed?.error` early-throw, the `<think>...</think>`-stripping + `reasoning`-field
//!   fallback for reasoning models, the empty-completion throw, then
//!   `parseJsonOutputOrText`/`normalizeTokens` (both reused from the helper twin) plus
//!   `generation_id`/`cost_usd_reported` extraction.
//! `parseProviderResponse` itself (status→error-code mapping, JSON-parse-or-throw) is exactly the
//! already-converted helper twin, unit-tested there — not re-tested here.
//!
//! `readKey()` — a REAL fs/env effect (matches JS: `OPENROUTER_API_KEY` env wins when
//! set+non-empty, else `AUTH_DIR/openrouter.key` trimmed, else the PERMISSION_DENIED cause
//! string). Same "real effect, bespoke-native tested" class as anthropic-call.rs's `read_key` —
//! `key_path()` here is byte-identical to that twin's `AUTH_DIR/<provider>.key` derivation, kept
//! local to avoid a hard dependency ordering beyond what's documented.
//!
//! `openrouterCall` (the default export) is the PARITY-DEFERRED stub: it resolves the key and
//! builds the request (proving the deterministic surface is reachable/correct) then returns Err
//! before the network hop, same convention as anthropic-call.rs's `anthropicCall` /
//! deepseek-call.rs's `deepseekCall`. No fixture, no frontier entry — BESPOKE-NATIVE TESTING only,
//! via `#[cfg(test)]` below.
//!
//! `guardRealNetworkInTests` (JS: blocks real spend under `NODE_TEST_CONTEXT` unless
//! `ANTILOKI_ALLOW_REAL_AI=1`) has no Rust equivalent — this twin never reaches the real network
//! branch at all (PARITY-DEFERRED), so the money-guard's protected code path is unreachable here,
//! same omission as deepseek-call.rs.
//!
//! `DEFAULT_TIMEOUT_MS` (`ANTILOKI_OPENROUTER_TIMEOUT_MS` env, default 120_000) governs the JS
//! `AbortController` timeout — inert here since no real HTTP client is wired, kept as a named
//! const for parity/documentation with the JS source.

use regex::Regex;
use serde_json::{json, Value};
use std::path::PathBuf;

pub const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

// roots.mjs: AUTH_DIR = join(HOME_DIR, 'auth'); HOME_DIR = $ANTILOKI_HOME else homedir()/.antiloki
#[cfg(windows)]
fn homedir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let drive = std::env::var("HOMEDRIVE").ok().filter(|s| !s.is_empty());
    let path = std::env::var("HOMEPATH").ok().filter(|s| !s.is_empty());
    match (drive, path) {
        (Some(d), Some(p)) => Some(PathBuf::from(format!("{d}{p}"))),
        _ => None,
    }
}

#[cfg(not(windows))]
fn homedir() -> Option<PathBuf> {
    std::env::var("HOME").ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

fn key_path() -> PathBuf {
    // Byte-identical to anthropic-call.rs's key_path() / external_auth_init::keyPath, just for
    // the "openrouter" provider slug.
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("auth").join("openrouter.key")
}

/// `function readKey()` — real fs/env effect (not conform-db-testable). Checks
/// `OPENROUTER_API_KEY` env first (trimmed, when set+non-empty), else reads+trims
/// `AUTH_DIR/openrouter.key`, else the PERMISSION_DENIED cause string reproduced as `Err(String)`.
pub fn read_key() -> Result<String, String> {
    if let Ok(v) = std::env::var("OPENROUTER_API_KEY") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let p = key_path();
    if p.exists() {
        if let Ok(raw) = std::fs::read_to_string(&p) {
            let trimmed = raw.trim().to_string();
            if !trimmed.is_empty() {
                return Ok(trimmed);
            }
        }
    }
    Err(format!(
        "OpenRouter API key not found. Set OPENROUTER_API_KEY env or write the key to {} (Settings \u{2192} BYOK).",
        p.to_string_lossy()
    ))
}

/// JS: the `messages`/`body` assembly + fetch headers inside `openrouterCall`. `input_kv`
/// defaults to `[]`, `temperature_value` to `0.2`, `max_tokens` to `1024`, `response_format` /
/// `session_id` / `web_search` default to falsy — all matching the JS default-param values.
/// Returns `{ "url", "headers": {...}, "body": {...} }`.
pub fn build_request(input: &Value, api_key: &str) -> Value {
    let template_md = input.get("template_md").cloned().unwrap_or(Value::Null);
    let input_kv = nz(input.get("input_kv")).cloned().unwrap_or(json!([]));
    let model_id = input.get("model_id").cloned().unwrap_or(Value::Null);
    let temperature_value = nz(input.get("temperature_value")).cloned().unwrap_or(json!(0.2));
    let max_tokens = nz(input.get("max_tokens")).cloned().unwrap_or(json!(1024));
    let response_format = nz(input.get("response_format")).cloned();
    let system_md = input.get("system_md").and_then(Value::as_str);
    let session_id = nz(input.get("session_id")).cloned();
    let web_search = input.get("web_search").and_then(Value::as_bool).unwrap_or(false);

    let prompt = crate::external__helpers_provider_adapter_helpers::substituteTemplate(&template_md, &input_kv);
    let prompt_str = prompt.as_str().unwrap_or_default();

    // WS-2 (migration 128): templates may carry a system-role message; send it as a leading
    // system message. Absent/empty ⇒ body unchanged (replay identity).
    let messages = match system_md {
        Some(s) if !s.trim().is_empty() => json!([
            { "role": "system", "content": s },
            { "role": "user", "content": prompt_str },
        ]),
        _ => json!([{ "role": "user", "content": prompt_str }]),
    };

    let model_id_str = model_id.as_str().unwrap_or_default();
    let model = if web_search && !model_id_str.ends_with(":online") {
        json!(format!("{model_id_str}:online"))
    } else {
        model_id.clone()
    };

    let mut body = json!({
        "model": model,
        "messages": messages,
        "temperature": temperature_value,
        "max_tokens": max_tokens,
        "usage": { "include": true },
    });
    if let Some(rf) = response_format {
        body["response_format"] = rf;
    }
    if let Some(sid) = session_id {
        body["session_id"] = sid;
    }

    let referer = std::env::var("ANTILOKI_OPENROUTER_REFERER").unwrap_or_else(|_| "https://github.com/antiloki-v3".to_string());
    let title = std::env::var("ANTILOKI_OPENROUTER_TITLE").unwrap_or_else(|_| "antiloki-v3".to_string());

    json!({
        "url": OPENROUTER_URL,
        "method": "POST",
        "headers": {
            "Authorization": format!("Bearer {api_key}"),
            "Content-Type": "application/json",
            "HTTP-Referer": referer,
            "X-Title": title,
        },
        "body": body,
    })
}

fn strip_think(s: &str) -> String {
    // JS: /<think>[\s\S]*?<\/think>/gi (non-greedy, case-insensitive, dotall via [\s\S])
    let re = Regex::new(r"(?is)<think>.*?</think>").unwrap();
    re.replace_all(s, "").trim().to_string()
}

/// JS: the post-`parseProviderResponse` tail of `openrouterCall` — the `parsed?.error`
/// early-throw, the `<think>` strip + `reasoning` fallback, the empty-completion throw, then
/// `parseJsonOutputOrText`/`normalizeTokens` + `generation_id`/`cost_usd_reported` extraction.
/// `model_id` is threaded through purely for the error messages (matches JS closure capture).
pub fn parse_response_body(parsed: &Value, model_id: &Value) -> Result<Value, String> {
    if let Some(err) = nz(parsed.get("error")) {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| err.to_string());
        return Err(format!(
            "OpenRouter returned an error for model '{}': {msg}",
            model_id.as_str().unwrap_or_default()
        ));
    }

    let msg = parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .cloned()
        .unwrap_or(json!({}));
    let content = msg.get("content").and_then(Value::as_str).unwrap_or("");
    let mut text = strip_think(content);
    if text.is_empty() {
        if let Some(reasoning) = msg.get("reasoning").and_then(Value::as_str) {
            if !reasoning.is_empty() {
                text = strip_think(reasoning);
            }
        }
    }
    if text.trim().is_empty() {
        let finish_reason = parsed
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .cloned()
            .unwrap_or(Value::Null);
        return Err(format!(
            "OpenRouter returned an empty completion for model '{}' (no content or reasoning). The model is likely unavailable, deprecated, or rate-limited on OpenRouter — switch this tier's model (Settings / BYOK / Cost Center) or retry. finish_reason={finish_reason}",
            model_id.as_str().unwrap_or_default()
        ));
    }

    let output = crate::external__helpers_provider_adapter_helpers::parseJsonOutputOrText(&json!(text));
    let usage = parsed.get("usage").cloned().unwrap_or(Value::Null);
    let tokens = crate::external__helpers_provider_adapter_helpers::normalizeTokens(&usage);
    let generation_id = parsed.get("id").cloned().unwrap_or(Value::Null);
    let cost_usd_reported = parsed
        .get("usage")
        .and_then(|u| u.get("cost"))
        .filter(|c| c.is_number())
        .cloned()
        .unwrap_or(Value::Null);

    Ok(json!({
        "output": output,
        "tokens_in": tokens.get("tokens_in").cloned().unwrap_or(json!(0)),
        "tokens_out": tokens.get("tokens_out").cloned().unwrap_or(json!(0)),
        "tokens_cached": tokens.get("tokens_cached").cloned().unwrap_or(json!(0)),
        "generation_id": generation_id,
        "cost_usd_reported": cost_usd_reported,
    }))
}

/// `export default async function openrouterCall(input, context = {})` — `context` is accepted
/// for contract-shape parity (`context.fetch_fn` in JS has no Rust equivalent, see module doc)
/// but unused, mirrored by the underscore-prefixed binding below.
pub fn openrouterCall(input: &Value, _context: &Value) -> Result<Value, String> {
    let api_key = read_key()?;
    let _request = build_request(input, &api_key);

    // PARITY-DEFERRED: real HTTP fetch against openrouter.ai is not implemented in this crate (no
    // HTTP client dependency). Everything up to this point (key resolution, request assembly) is
    // real and deterministic; the network call itself is the documented deferred boundary,
    // matching the crossing playbook's lane-A contract for network provider adapters.
    Err("openrouter-call: PARITY-DEFERRED — real HTTP fetch is not implemented in this twin".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn with_home<T>(antiloki_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", antiloki_home);
            std::env::remove_var("OPENROUTER_API_KEY");
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
        }
        result
    }

    fn with_env_key<T>(key: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("OPENROUTER_API_KEY", key);
        }
        let result = f();
        unsafe {
            std::env::remove_var("OPENROUTER_API_KEY");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-openrouter-call-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn read_key_prefers_env_var_over_key_file() {
        let r = with_env_key("  sk-from-env  ", || read_key()).unwrap();
        assert_eq!(r, "sk-from-env");
    }

    #[test]
    fn read_key_reads_and_trims_key_file_when_no_env() {
        let dir = temp_dir("read-key");
        let auth_dir = dir.join("auth");
        fs::create_dir_all(&auth_dir).unwrap();
        fs::write(auth_dir.join("openrouter.key"), "  sk-file-key\n").unwrap();
        let r = with_home(dir.to_str().unwrap(), || read_key()).unwrap();
        assert_eq!(r, "sk-file-key");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_key_missing_returns_not_found_message() {
        let dir = temp_dir("read-key-missing");
        let r = with_home(dir.to_str().unwrap(), || read_key()).unwrap_err();
        assert!(r.starts_with("OpenRouter API key not found."), "{r}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_request_assembles_url_headers_body_with_defaults() {
        let req = build_request(
            &json!({ "template_md": "Say {{word}}", "input_kv": [{ "key": "word", "value": "hi" }], "model_id": "openai/gpt-4o-mini" }),
            "sk-test",
        );
        assert_eq!(req["url"], json!(OPENROUTER_URL));
        assert_eq!(req["headers"]["Authorization"], json!("Bearer sk-test"));
        assert_eq!(req["headers"]["Content-Type"], json!("application/json"));
        assert_eq!(req["headers"]["HTTP-Referer"], json!("https://github.com/antiloki-v3"));
        assert_eq!(req["headers"]["X-Title"], json!("antiloki-v3"));
        assert_eq!(req["body"]["model"], json!("openai/gpt-4o-mini"));
        assert_eq!(req["body"]["messages"], json!([{ "role": "user", "content": "Say hi" }]));
        assert_eq!(req["body"]["temperature"], json!(0.2));
        assert_eq!(req["body"]["max_tokens"], json!(1024));
        assert_eq!(req["body"]["usage"], json!({ "include": true }));
        assert!(req["body"].get("response_format").is_none());
        assert!(req["body"].get("session_id").is_none());
    }

    #[test]
    fn build_request_prepends_system_message_when_system_md_present() {
        let req = build_request(
            &json!({ "template_md": "hi", "model_id": "m", "system_md": "You are terse." }),
            "sk",
        );
        assert_eq!(
            req["body"]["messages"],
            json!([
                { "role": "system", "content": "You are terse." },
                { "role": "user", "content": "hi" },
            ])
        );
    }

    #[test]
    fn build_request_blank_system_md_is_dropped() {
        let req = build_request(&json!({ "template_md": "hi", "model_id": "m", "system_md": "   " }), "sk");
        assert_eq!(req["body"]["messages"], json!([{ "role": "user", "content": "hi" }]));
    }

    #[test]
    fn build_request_appends_online_suffix_for_web_search() {
        let req = build_request(&json!({ "template_md": "hi", "model_id": "openai/gpt-4o", "web_search": true }), "sk");
        assert_eq!(req["body"]["model"], json!("openai/gpt-4o:online"));
    }

    #[test]
    fn build_request_web_search_does_not_double_suffix() {
        let req = build_request(&json!({ "template_md": "hi", "model_id": "openai/gpt-4o:online", "web_search": true }), "sk");
        assert_eq!(req["body"]["model"], json!("openai/gpt-4o:online"));
    }

    #[test]
    fn build_request_attaches_response_format_and_session_id_when_present() {
        let req = build_request(
            &json!({ "template_md": "hi", "model_id": "m", "response_format": { "type": "json_object" }, "session_id": "sess-1" }),
            "sk",
        );
        assert_eq!(req["body"]["response_format"], json!({ "type": "json_object" }));
        assert_eq!(req["body"]["session_id"], json!("sess-1"));
    }

    #[test]
    fn parse_response_body_extracts_output_tokens_and_metadata() {
        let parsed = json!({
            "id": "gen-123",
            "choices": [{ "message": { "content": "{\"a\":1}" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 4, "cost": 0.0012 },
        });
        let out = parse_response_body(&parsed, &json!("openai/gpt-4o-mini")).unwrap();
        assert_eq!(out["output"], json!({ "a": 1 }));
        assert_eq!(out["tokens_in"], json!(10));
        assert_eq!(out["tokens_out"], json!(4));
        assert_eq!(out["generation_id"], json!("gen-123"));
        assert_eq!(out["cost_usd_reported"], json!(0.0012));
    }

    #[test]
    fn parse_response_body_strips_think_blocks_from_content() {
        let parsed = json!({
            "choices": [{ "message": { "content": "<think>pondering...</think>final answer" } }],
        });
        let out = parse_response_body(&parsed, &json!("m")).unwrap();
        assert_eq!(out["output"], json!("final answer"));
    }

    #[test]
    fn parse_response_body_falls_back_to_reasoning_when_content_empty() {
        let parsed = json!({
            "choices": [{ "message": { "content": "", "reasoning": "<think>x</think>the reasoning answer" } }],
        });
        let out = parse_response_body(&parsed, &json!("m")).unwrap();
        assert_eq!(out["output"], json!("the reasoning answer"));
    }

    #[test]
    fn parse_response_body_errors_on_empty_completion() {
        let parsed = json!({ "choices": [{ "message": {}, "finish_reason": "length" }] });
        let e = parse_response_body(&parsed, &json!("openai/gpt-4o-mini")).unwrap_err();
        assert!(e.contains("empty completion"), "{e}");
        assert!(e.contains("openai/gpt-4o-mini"), "{e}");
    }

    #[test]
    fn parse_response_body_errors_on_provider_error_field() {
        let parsed = json!({ "error": { "message": "model deprecated" } });
        let e = parse_response_body(&parsed, &json!("meta-llama/llama-3.3-70b")).unwrap_err();
        assert!(e.contains("model deprecated"), "{e}");
        assert!(e.contains("meta-llama/llama-3.3-70b"), "{e}");
    }

    #[test]
    fn parse_response_body_missing_cost_and_id_default_to_null() {
        let parsed = json!({ "choices": [{ "message": { "content": "hi" } }] });
        let out = parse_response_body(&parsed, &json!("m")).unwrap();
        assert_eq!(out["generation_id"], Value::Null);
        assert_eq!(out["cost_usd_reported"], Value::Null);
        assert_eq!(out["tokens_cached"], json!(0));
    }

    #[test]
    fn openrouter_call_returns_parity_deferred_after_real_key_resolution() {
        let dir = temp_dir("call-deferred");
        let auth_dir = dir.join("auth");
        fs::create_dir_all(&auth_dir).unwrap();
        fs::write(auth_dir.join("openrouter.key"), "sk-real").unwrap();
        let input = json!({ "template_md": "hi", "model_id": "openai/gpt-4o-mini" });
        let e = with_home(dir.to_str().unwrap(), || openrouterCall(&input, &json!({}))).unwrap_err();
        assert_eq!(e, "openrouter-call: PARITY-DEFERRED — real HTTP fetch is not implemented in this twin");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn openrouter_call_propagates_missing_key_error() {
        let dir = temp_dir("call-no-key");
        let e = with_home(dir.to_str().unwrap(), || openrouterCall(&json!({}), &json!({}))).unwrap_err();
        assert!(e.starts_with("OpenRouter API key not found."), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }
}
