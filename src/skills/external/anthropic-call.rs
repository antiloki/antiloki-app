//! anthropic-call.rs — twin of skills/external/anthropic-call.mjs. NAMES ARE CONTRACT:
//! `anthropicCall` (default export).
//!
//! NETWORK PROVIDER ADAPTER lane (crossing playbook lane A): BESPOKE-NATIVE. The real HTTP
//! `fetch(...)` call against `https://api.anthropic.com/v1/messages` is the DEFERRED boundary —
//! this crate has no HTTP client dependency (zero-new-Cargo-deps discipline, same as every prior
//! network-adjacent twin). `anthropicCall` below assembles the request deterministically (key
//! read, message-building, URL/headers/body) and then returns `Err("anthropic-call:
//! PARITY-DEFERRED — real HTTP fetch is not implemented in this twin")` at the exact point the JS
//! would call `fetchFn(...)`. ai-call's own JS test harness never imports this file (it injects a
//! provider_fn), so no real call-site depends on the network branch actually completing.
//!
//! Everything DETERMINISTIC around the fetch boundary is ported faithfully as pure/near-pure fns,
//! each covered by `#[cfg(test)]` with canned values:
//! - `build_messages(template_md, input_kv)` — pure. Mirrors `buildMessages`: system prompt is
//!   `template_md` + the fixed JSON-only instruction suffix; user message joins
//!   `"key: value"` pairs (`JSON.stringify` for object-typed values, raw string otherwise),
//!   falling back to the literal `"<no input>"` when `input_kv` is empty/absent.
//! - `build_request(model_id, max_tokens, temperature_value, system, messages, api_key)` — pure.
//!   Mirrors the `fetchFn(url, { method, headers, body })` call shape: returns
//!   `{url, headers, body}` as JSON so both headers and the exact request body (model,
//!   max_tokens, temperature, system, messages) are assertable without a network stack.
//! - `parse_error_response(status, text)` — pure. Mirrors the `!res.ok` branch:
//!   `"Anthropic API {status}: {text.slice(0,200)}"` (AntilokiError code IO_ERROR upstream; this
//!   fn reproduces the `cause` string, matching the Result<_, String> convention used by every
//!   other twin in this crate).
//! - `parse_success_response(body_json)` — pure. Mirrors the 200-path: pulls
//!   `body.content[0].text` (default `""`), `JSON.parse`s it (VALIDATION_LOGIC error with a
//!   200-char `text_preview` on failure — reproduced as `Err(String)`), and reads
//!   `body.usage.input_tokens` / `body.usage.output_tokens` (both default `0`).
//! - `read_key(env_override, key_path)` — a REAL fs read (matches JS `readKey`'s
//!   `existsSync`/`readFileSync` against `AUTH_DIR/anthropic.key`, or the `ANTHROPIC_API_KEY` env
//!   var when set+non-empty, trimmed either way). Not injected via `context.fetch_fn` — JS reads
//!   `process.env` / the real fs directly too, so this is the same "real effect, bespoke-native
//!   tested" class as auth-sign.rs's `authSign`, not a conform-db unit.
//!
//! `text.slice(0, 200)` (JS, UTF-16 code units) is reproduced via `.chars().take(200)` — a
//! documented narrow gap for any text containing astral-plane codepoints (surrogate pairs count
//! as 2 JS units but 1 Rust char), matching the em-dash-class disclosure convention used
//! throughout this crate. `text.slice(0, 200)` in the success-path preview uses the same
//! approach.
//!
//! `keyPath('anthropic')` is NOT re-derived from scratch here — it is byte-identical to the
//! already-converted `external_auth_init::keyPath` twin (`resolve(AUTH_DIR, "anthropic.key")`),
//! reused per the crossing playbook's cross-module-call convention (same pattern as
//! auth-sign.rs's `crate::external_auth_init::keyPath` call).
//!
//! `context.fetch_fn` (JS test-injection hook) has no Rust equivalent since the fetch boundary is
//! never actually invoked here — `anthropicCall`'s `context` parameter is accepted (contract
//! shape) but unused, mirrored by an underscore-prefixed binding, same convention as
//! auth-sign.rs's unused `_context`/`_db`.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::PathBuf;

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }

fn preview_200(text: &str) -> String {
    text.chars().take(200).collect()
}

/// `function buildMessages(template_md, input_kv)` — pure.
pub fn build_messages(template_md: &Value, input_kv: &Value) -> Value {
    let template_md_str = str_qq(Some(template_md));
    let system = format!(
        "{template_md_str}\n\nReturn ONLY a JSON object matching the documented output schema. No prose, no markdown fences."
    );

    let empty: Vec<Value> = Vec::new();
    let kv_list = input_kv.as_array().unwrap_or(&empty);
    let user_parts: Vec<String> = kv_list
        .iter()
        .map(|kv| {
            let key = str_qq(kv.get("key"));
            let value = kv.get("value");
            let value_str = match value {
                Some(v) if v.is_object() || v.is_array() => v.to_string(),
                other => str_qq(other),
            };
            format!("{key}: {value_str}")
        })
        .collect();
    let joined = user_parts.join("\n");
    let user_content = if joined.is_empty() { "<no input>".to_string() } else { joined };

    json!({
        "system": system,
        "messages": [{ "role": "user", "content": user_content }],
    })
}

/// Mirrors the `fetchFn(url, { method, headers, body })` call site — pure, no network I/O.
/// `system` / `messages` come from `build_messages` above; `api_key` from `read_key` below.
pub fn build_request(
    model_id: &Value,
    max_tokens: &Value,
    temperature_value: &Value,
    system: &Value,
    messages: &Value,
    api_key: &str,
) -> Value {
    let body = json!({
        "model": model_id,
        "max_tokens": max_tokens,
        "temperature": temperature_value,
        "system": system,
        "messages": messages,
    });
    json!({
        "url": "https://api.anthropic.com/v1/messages",
        "method": "POST",
        "headers": {
            "content-type": "application/json",
            "x-api-key": api_key,
            "anthropic-version": "2023-06-01",
        },
        "body": body,
    })
}

/// `if (!res.ok) { ... AntilokiError(IO_ERROR, "Anthropic API {status}: {text.slice(0,200)}") }`
/// — reproduces the `cause` string as `Err(String)`.
pub fn parse_error_response(status: i64, text: &str) -> String {
    format!("Anthropic API {status}: {}", preview_200(text))
}

/// `const body = await res.json(); const text = body?.content?.[0]?.text ?? ''; JSON.parse(text)`
/// — success path. Returns `{output, tokens_in, tokens_out}` or the VALIDATION_LOGIC `cause`
/// string (`"model output was not valid JSON: {e.message}"`) reproduced as `Err(String)`.
pub fn parse_success_response(body_json: &Value) -> Result<Value, String> {
    let text = body_json
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let parsed: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            return Err(format!(
                "model output was not valid JSON: {e}: {}",
                preview_200(&text)
            ));
        }
    };

    let tokens_in = body_json
        .get("usage")
        .and_then(|u| u.get("input_tokens"))
        .cloned()
        .unwrap_or(json!(0));
    let tokens_out = body_json
        .get("usage")
        .and_then(|u| u.get("output_tokens"))
        .cloned()
        .unwrap_or(json!(0));

    Ok(json!({
        "output": parsed,
        "tokens_in": tokens_in,
        "tokens_out": tokens_out,
    }))
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
    // Byte-identical to external_auth_init::keyPath("anthropic") — kept local so this file has
    // no hard runtime dependency ordering on that module beyond what's already documented above.
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("auth").join("anthropic.key")
}

/// `function readKey()` — real fs/env effect (not conform-db-testable). Checks
/// `ANTHROPIC_API_KEY` env first (trimmed, when set+non-empty), else reads+trims
/// `AUTH_DIR/anthropic.key`, else the NOT_FOUND cause string reproduced as `Err(String)`.
pub fn read_key() -> Result<String, String> {
    if let Ok(v) = std::env::var("ANTHROPIC_API_KEY") {
        if !v.is_empty() {
            return Ok(v.trim().to_string());
        }
    }
    let p = key_path();
    if !p.exists() {
        return Err(format!(
            "Anthropic API key not found. Set ANTHROPIC_API_KEY env or write key to {}",
            p.to_string_lossy()
        ));
    }
    std::fs::read_to_string(&p)
        .map(|s| s.trim().to_string())
        .map_err(|e| e.to_string())
}

/// `export default async function anthropicCall(input, context = {})` — `context` is accepted
/// for contract-shape parity (`context.fetch_fn` in JS has no Rust equivalent, see module doc)
/// but unused, mirrored by the underscore-prefixed binding below.
pub fn anthropicCall(input: &Value, _context: &Value, _db: &Connection) -> Result<Value, String> {
    let model_id = input.get("model_id").cloned().unwrap_or(Value::Null);
    let template_md = input.get("template_md").cloned().unwrap_or(Value::Null);
    let input_kv = input.get("input_kv").cloned().unwrap_or(Value::Null);
    let temperature_value = input.get("temperature_value").cloned().unwrap_or(Value::Null);
    let max_tokens = input.get("max_tokens").cloned().unwrap_or(Value::Null);

    let api_key = read_key()?;
    let built = build_messages(&template_md, &input_kv);
    let system = built.get("system").cloned().unwrap_or(Value::Null);
    let messages = built.get("messages").cloned().unwrap_or(Value::Null);

    let _request = build_request(&model_id, &max_tokens, &temperature_value, &system, &messages, &api_key);

    // PARITY-DEFERRED: real HTTP fetch against api.anthropic.com is not implemented in this
    // crate (no HTTP client dependency). Everything up to this point (key resolution, message
    // assembly, request shape) is real and deterministic; the network call itself is the
    // documented deferred boundary, matching the crossing playbook's lane-A contract for network
    // provider adapters.
    Err("anthropic-call: PARITY-DEFERRED — real HTTP fetch is not implemented in this twin".to_string())
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
            std::env::remove_var("ANTHROPIC_API_KEY");
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
            std::env::set_var("ANTHROPIC_API_KEY", key);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-anthropic-call-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn build_messages_joins_kv_pairs_and_stringifies_objects() {
        let template_md = json!("You are a helper.");
        let input_kv = json!([
            { "key": "name", "value": "dani" },
            { "key": "meta", "value": { "a": 1 } },
        ]);
        let r = build_messages(&template_md, &input_kv);
        assert_eq!(
            r["system"],
            json!("You are a helper.\n\nReturn ONLY a JSON object matching the documented output schema. No prose, no markdown fences.")
        );
        assert_eq!(r["messages"][0]["role"], json!("user"));
        assert_eq!(r["messages"][0]["content"], json!("name: dani\nmeta: {\"a\":1}"));
    }

    #[test]
    fn build_messages_empty_input_kv_falls_back_to_placeholder() {
        let r = build_messages(&json!("sys"), &json!([]));
        assert_eq!(r["messages"][0]["content"], json!("<no input>"));
        let r2 = build_messages(&json!("sys"), &Value::Null);
        assert_eq!(r2["messages"][0]["content"], json!("<no input>"));
    }

    #[test]
    fn build_request_shapes_url_headers_and_body() {
        let built = build_messages(&json!("sys"), &json!([]));
        let req = build_request(
            &json!("claude-x"),
            &json!(512),
            &json!(0.2),
            &built["system"],
            &built["messages"],
            "sk-test-key",
        );
        assert_eq!(req["url"], json!("https://api.anthropic.com/v1/messages"));
        assert_eq!(req["method"], json!("POST"));
        assert_eq!(req["headers"]["x-api-key"], json!("sk-test-key"));
        assert_eq!(req["headers"]["anthropic-version"], json!("2023-06-01"));
        assert_eq!(req["headers"]["content-type"], json!("application/json"));
        assert_eq!(req["body"]["model"], json!("claude-x"));
        assert_eq!(req["body"]["max_tokens"], json!(512));
        assert_eq!(req["body"]["temperature"], json!(0.2));
        assert_eq!(req["body"]["system"], built["system"]);
        assert_eq!(req["body"]["messages"], built["messages"]);
    }

    #[test]
    fn parse_error_response_matches_js_message_shape() {
        let e = parse_error_response(429, "rate limited, please retry later");
        assert_eq!(e, "Anthropic API 429: rate limited, please retry later");
    }

    #[test]
    fn parse_error_response_truncates_to_200_chars() {
        let long = "x".repeat(500);
        let e = parse_error_response(500, &long);
        assert_eq!(e, format!("Anthropic API 500: {}", "x".repeat(200)));
    }

    #[test]
    fn parse_success_response_extracts_output_and_token_counts() {
        let body = json!({
            "content": [{ "text": "{\"ok\":true}" }],
            "usage": { "input_tokens": 42, "output_tokens": 7 },
        });
        let r = parse_success_response(&body).unwrap();
        assert_eq!(r["output"], json!({"ok": true}));
        assert_eq!(r["tokens_in"], json!(42));
        assert_eq!(r["tokens_out"], json!(7));
    }

    #[test]
    fn parse_success_response_defaults_missing_usage_to_zero() {
        let body = json!({ "content": [{ "text": "{}" }] });
        let r = parse_success_response(&body).unwrap();
        assert_eq!(r["tokens_in"], json!(0));
        assert_eq!(r["tokens_out"], json!(0));
    }

    #[test]
    fn parse_success_response_missing_content_defaults_text_to_empty() {
        let body = json!({});
        let e = parse_success_response(&body).unwrap_err();
        assert!(e.starts_with("model output was not valid JSON:"));
    }

    #[test]
    fn parse_success_response_invalid_json_returns_validation_error() {
        let body = json!({ "content": [{ "text": "not json" }] });
        let e = parse_success_response(&body).unwrap_err();
        assert!(e.starts_with("model output was not valid JSON:"));
        assert!(e.ends_with("not json"));
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
        fs::write(auth_dir.join("anthropic.key"), "  sk-file-key\n").unwrap();
        let r = with_home(dir.to_str().unwrap(), || read_key()).unwrap();
        assert_eq!(r, "sk-file-key");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_key_missing_returns_not_found_message() {
        let dir = temp_dir("read-key-missing");
        let r = with_home(dir.to_str().unwrap(), || read_key()).unwrap_err();
        assert!(r.starts_with("Anthropic API key not found."));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn anthropic_call_returns_parity_deferred_after_real_key_resolution() {
        let dir = temp_dir("call-deferred");
        let auth_dir = dir.join("auth");
        fs::create_dir_all(&auth_dir).unwrap();
        fs::write(auth_dir.join("anthropic.key"), "sk-real").unwrap();
        let input = json!({
            "model_id": "claude-x",
            "template_md": "sys",
            "input_kv": [],
            "temperature_value": 0.5,
            "max_tokens": 100,
        });
        let e = with_home(dir.to_str().unwrap(), || {
            anthropicCall(&input, &json!({}), &db0())
        })
        .unwrap_err();
        assert_eq!(e, "anthropic-call: PARITY-DEFERRED — real HTTP fetch is not implemented in this twin");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn anthropic_call_propagates_missing_key_error() {
        let dir = temp_dir("call-no-key");
        let e = with_home(dir.to_str().unwrap(), || {
            anthropicCall(&json!({}), &json!({}), &db0())
        })
        .unwrap_err();
        assert!(e.starts_with("Anthropic API key not found."));
        let _ = fs::remove_dir_all(&dir);
    }
}
