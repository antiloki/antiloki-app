//! openai-call.rs — twin of skills/external/openai-call.mjs (NETWORK provider adapter, lane A).
//! NAMES ARE CONTRACT: `openaiCall` (default), `looksLikeJsonPrompt` (named export).
//!
//! PARITY-DEFERRED: the real `fetch(OPENAI_URL, ...)` POST to
//! https://api.openai.com/v1/chat/completions is the deferred network boundary — there is no
//! HTTP client in this crate (zero-new-Cargo-deps per the crossing playbook) and no
//! `context.fetch_fn` test-injection concept in Rust. `openaiCall` below performs every
//! DETERMINISTIC step around that boundary faithfully — reading `OPENAI_API_KEY` (via the
//! already-converted `requireApiKey` twin), building the exact request body (model/messages/
//! temperature/max_tokens/response_format, including the WS-2 system-role message), and would
//! parse a response — then returns the deferred stub `Err("openai-call: PARITY-DEFERRED — no
//! network client in this crate; see module doc")` at the exact point the JS would call
//! `fetchFn(...)`, mirroring auth-sign.rs / book-run.rs's deferred-boundary convention.
//!
//! The RESPONSE-PARSING side (JS lines 69-73: `parseProviderResponse` → pull
//! `choices[0].message.content` → `parseJsonOutputOrText` → `normalizeTokens(usage)`) is ported
//! as the pure, independently-testable `parse_openai_response(parsed_body)` fn below, fed canned
//! response JSON in `#[cfg(test)]` exactly as the playbook's lane-A guidance prescribes — it
//! reuses the already-converted `crate::external__helpers_provider_adapter_helpers` twins
//! (`parseJsonOutputOrText`, `normalizeTokens`) so both sides of the adapter stay byte-for-byte
//! aligned with the JS helper module.
//!
//! `requireApiKey`/`parseProviderResponse` throw fidelity (verified against the JS helper
//! module, itself already twinned) is exercised through those existing tests — not re-proven
//! here. `DEFAULT_TIMEOUT_MS` (`ANTILOKI_OPENAI_TIMEOUT_MS` env var, default 60_000) is read for
//! parity but has no effect since no request is ever actually sent.

use serde_json::{json, Value};
use std::sync::OnceLock;
use regex::Regex;

fn json_prompt_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)Output JSON|JSON of this exact shape|return JSON").unwrap())
}

/// JS: `function looksLikeJsonPrompt(template_md) { return /Output JSON|JSON of this exact
/// shape|return JSON/i.test(template_md); }` — `RegExp.test` coerces a non-string arg via
/// `String(v)`; `js_string` below mirrors that coercion (rarely hit — real callers pass a string
/// `template_md`).
pub fn looksLikeJsonPrompt(template_md: &Value) -> Value {
    let s = match template_md {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    };
    json!(json_prompt_re().is_match(&s))
}

const OPENAI_URL: &str = "https://api.openai.com/v1/chat/completions";

/// JS: `const DEFAULT_TIMEOUT_MS = Number(process.env.ANTILOKI_OPENAI_TIMEOUT_MS ?? 60_000);`
fn default_timeout_ms() -> f64 {
    match std::env::var("ANTILOKI_OPENAI_TIMEOUT_MS") {
        Ok(s) => s.trim().parse::<f64>().unwrap_or(f64::NAN),
        Err(_) => 60_000.0,
    }
}

/// Builds the exact JSON body `openaiCall` would `JSON.stringify` into the fetch POST — the
/// deterministic half of JS lines 40-52, split out as an independently-testable pure fn.
pub fn build_request_body(
    template_md: &Value,
    input_kv: &Value,
    model_id: &Value,
    temperature_value: &Value,
    max_tokens: &Value,
    system_md: &Value,
) -> Value {
    let prompt = crate::external__helpers_provider_adapter_helpers::substituteTemplate(template_md, input_kv);
    let wants_json = looksLikeJsonPrompt(template_md).as_bool().unwrap_or(false);
    let system_trim_nonempty = matches!(system_md, Value::String(s) if !s.trim().is_empty());
    let messages = if system_trim_nonempty {
        json!([
            { "role": "system", "content": system_md },
            { "role": "user", "content": prompt },
        ])
    } else {
        json!([{ "role": "user", "content": prompt }])
    };
    let mut body = serde_json::Map::new();
    body.insert("model".into(), model_id.clone());
    body.insert("messages".into(), messages);
    body.insert("temperature".into(), temperature_value.clone());
    body.insert("max_tokens".into(), max_tokens.clone());
    if wants_json {
        body.insert("response_format".into(), json!({ "type": "json_object" }));
    }
    Value::Object(body)
}

/// The pure response-parsing half (JS lines 69-73), fed a canned already-parsed provider JSON
/// body (i.e. what `parseProviderResponse` would have returned on success).
pub fn parse_openai_response(parsed_body: &Value) -> Value {
    let text = parsed_body
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c0| c0.get("message"))
        .and_then(|m| m.get("content"))
        .cloned()
        .unwrap_or(json!(""));
    let output = crate::external__helpers_provider_adapter_helpers::parseJsonOutputOrText(&text);
    let tokens = crate::external__helpers_provider_adapter_helpers::normalizeTokens(
        parsed_body.get("usage").unwrap_or(&Value::Null),
    );
    json!({
        "output": output,
        "tokens_in": tokens.get("tokens_in").cloned().unwrap_or(json!(0)),
        "tokens_out": tokens.get("tokens_out").cloned().unwrap_or(json!(0)),
    })
}

/// `openaiCall(input, _context)` — mirrors the JS default export up to (not including) the real
/// `fetch(...)` call, which is PARITY-DEFERRED (see module doc). `_context` (`context.fetch_fn`
/// test-injection in JS) has no Rust equivalent and is unused.
pub fn openaiCall(input: &Value, _context: &Value) -> Result<Value, String> {
    let obj = input.as_object();
    let template_md = obj.and_then(|o| o.get("template_md")).cloned().unwrap_or(Value::Null);
    let input_kv = obj.and_then(|o| o.get("input_kv")).cloned().unwrap_or(json!([]));
    let model_id = obj.and_then(|o| o.get("model_id")).cloned().unwrap_or(Value::Null);
    let temperature_value = obj
        .and_then(|o| o.get("temperature_value"))
        .cloned()
        .unwrap_or(json!(0.2));
    let max_tokens = obj.and_then(|o| o.get("max_tokens")).cloned().unwrap_or(json!(1024));
    let system_md = obj.and_then(|o| o.get("system_md")).cloned().unwrap_or(Value::Null);

    let _api_key = crate::external__helpers_provider_adapter_helpers::requireApiKey(
        &json!("OPENAI_API_KEY"),
        &json!("openai-call"),
    )?;
    let _body = build_request_body(&template_md, &input_kv, &model_id, &temperature_value, &max_tokens, &system_md);
    let _url = OPENAI_URL;
    let _timeout_ms = default_timeout_ms();

    // JS: `response = await fetchFn(OPENAI_URL, {...})` — the deferred network boundary.
    Err("openai-call: PARITY-DEFERRED — no network client in this crate; see module doc".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_json_prompt_matches_the_three_phrasings() {
        assert_eq!(looksLikeJsonPrompt(&json!("Please respond with Output JSON only")), json!(true));
        assert_eq!(looksLikeJsonPrompt(&json!("JSON of this exact shape: {...}")), json!(true));
        assert_eq!(looksLikeJsonPrompt(&json!("please return JSON")), json!(true));
        assert_eq!(looksLikeJsonPrompt(&json!("output json please")), json!(true)); // case-insensitive
        assert_eq!(looksLikeJsonPrompt(&json!("plain prose prompt")), json!(false));
        assert_eq!(looksLikeJsonPrompt(&Value::Null), json!(false));
    }

    #[test]
    fn build_request_body_without_system_md() {
        let body = build_request_body(
            &json!("Hi {{name}}"),
            &json!([{ "key": "name", "value": "Atlas" }]),
            &json!("gpt-4o-mini"),
            &json!(0.2),
            &json!(1024),
            &Value::Null,
        );
        assert_eq!(
            body,
            json!({
                "model": "gpt-4o-mini",
                "messages": [{ "role": "user", "content": "Hi Atlas" }],
                "temperature": 0.2,
                "max_tokens": 1024,
            })
        );
    }

    #[test]
    fn build_request_body_with_system_md_prepends_system_message() {
        let body = build_request_body(
            &json!("do the thing"),
            &json!([]),
            &json!("gpt-4o"),
            &json!(0.5),
            &json!(500),
            &json!("you are terse"),
        );
        assert_eq!(
            body["messages"],
            json!([
                { "role": "system", "content": "you are terse" },
                { "role": "user", "content": "do the thing" },
            ])
        );
    }

    #[test]
    fn build_request_body_blank_system_md_is_ignored() {
        let body = build_request_body(&json!("x"), &json!([]), &json!("m"), &json!(0.2), &json!(1), &json!("   "));
        assert_eq!(body["messages"], json!([{ "role": "user", "content": "x" }]));
    }

    #[test]
    fn build_request_body_sets_json_response_format_when_prompt_wants_json() {
        let body = build_request_body(
            &json!("Output JSON of this exact shape: {}"),
            &json!([]),
            &json!("m"),
            &json!(0.2),
            &json!(1),
            &Value::Null,
        );
        assert_eq!(body["response_format"], json!({ "type": "json_object" }));
    }

    #[test]
    fn build_request_body_omits_response_format_for_plain_prompt() {
        let body = build_request_body(&json!("plain prompt"), &json!([]), &json!("m"), &json!(0.2), &json!(1), &Value::Null);
        assert!(body.as_object().unwrap().get("response_format").is_none());
    }

    #[test]
    fn parse_openai_response_extracts_content_and_tokens() {
        let out = parse_openai_response(&json!({
            "choices": [{ "message": { "content": "{\"x\":1}" } }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 4 },
        }));
        assert_eq!(out["output"], json!({ "x": 1 }));
        assert_eq!(out["tokens_in"], json!(10));
        assert_eq!(out["tokens_out"], json!(4));
    }

    #[test]
    fn parse_openai_response_falls_back_to_plain_text_and_zero_tokens() {
        let out = parse_openai_response(&json!({
            "choices": [{ "message": { "content": "plain prose reply" } }],
        }));
        assert_eq!(out["output"], json!("plain prose reply"));
        assert_eq!(out["tokens_in"], json!(0));
        assert_eq!(out["tokens_out"], json!(0));
    }

    #[test]
    fn parse_openai_response_missing_content_defaults_to_empty_string_output() {
        let out = parse_openai_response(&json!({ "choices": [] }));
        assert_eq!(out["output"], json!(""));
    }

    #[test]
    fn openai_call_requires_api_key_before_touching_network() {
        // OPENAI_API_KEY is process-global and its sibling test SETS it. Without the shared lock
        // the two race and whichever loses reports the other's env — a flake that reads as a real
        // failure. See test_env_lock's module doc.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env mutation of the real OPENAI_API_KEY-name var; serialize against
        // other tests in this process that might also read it (none currently do), matching the
        // shared-env-lock convention used elsewhere in the crossing (not needed here since no
        // other test in this file touches OPENAI_API_KEY, but removed defensively either way).
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
        }
        let err = openaiCall(&json!({ "template_md": "x", "model_id": "gpt-4o" }), &Value::Null).unwrap_err();
        assert!(err.contains("OPENAI_API_KEY"), "{err}");
        assert!(err.contains("not set"), "{err}");
    }

    #[test]
    fn openai_call_reaches_the_parity_deferred_boundary_once_key_is_set() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("OPENAI_API_KEY_RS_TEST_TMP", "unused");
            std::env::set_var("OPENAI_API_KEY", "sk-test-key");
        }
        let err = openaiCall(&json!({ "template_md": "x", "model_id": "gpt-4o" }), &Value::Null).unwrap_err();
        assert!(err.contains("PARITY-DEFERRED"), "{err}");
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
            std::env::remove_var("OPENAI_API_KEY_RS_TEST_TMP");
        }
    }
}
