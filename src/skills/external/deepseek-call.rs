//! deepseek-call.rs — twin of skills/external/deepseek-call.mjs. NAMES ARE CONTRACT: default
//! `deepseekCall`. ProviderAdapter for DeepSeek's OpenAI-compatible chat-completions endpoint —
//! same shape as the (not-yet-converted) openai-call, different base URL + API-key env var.
//!
//! LANE A (bespoke-native network adapter): the real `fetch(DEEPSEEK_URL, ...)` round-trip is the
//! DEFERRED boundary — genuinely non-deterministic (a live provider's response), not
//! JSON-fixturable, unconvertible IN PRINCIPLE. Everything DETERMINISTIC around it is ported
//! faithfully as pure fns and IS what this file's tests exercise:
//! - `build_request(input)`: assembles the exact request URL/headers/body the JS sends —
//!   `substituteTemplate`/`requireApiKey` reused byte-for-byte from the already-converted
//!   `external__helpers_provider_adapter_helpers` twin (cross-module-call convention).
//! - `parse_response_body(parsed)`: mirrors the JS post-`parseProviderResponse` tail —
//!   `parsed?.choices?.[0]?.message?.content ?? ''` then `parseJsonOutputOrText` for `output`,
//!   `normalizeTokens(parsed?.usage)` for `tokens_in`/`tokens_out` (the JS return omits
//!   `tokens_cached`, so this twin does too — NAMES ARE CONTRACT extends to the output SHAPE).
//! `parseProviderResponse` itself (status→error-code mapping, JSON-parse-or-throw) is exactly the
//! already-converted helper twin, unit-tested there — not re-tested here.
//!
//! `deepseekCall` (the default export) is the PARITY-DEFERRED stub: it builds the request (proving
//! the deterministic surface is reachable/correct) then returns Err before the network hop, same
//! convention as ai-call.rs's `aiCall` / book-run.rs's faculty calls. No fixture, no frontier entry
//! — BESPOKE-NATIVE TESTING only, via `#[cfg(test)]` below.
//!
//! `DEFAULT_TIMEOUT_MS` (`ANTILOKI_DEEPSEEK_TIMEOUT_MS` env, default 90_000) governs the JS
//! `AbortController` timeout — inert here since no real HTTP client is wired, kept as a named
//! const for parity/documentation with the JS source.

use serde_json::{json, Value};

pub const DEEPSEEK_URL: &str = "https://api.deepseek.com/v1/chat/completions";

fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

/// JS: request-assembly portion of `deepseekCall` — `substituteTemplate` + the `body` object
/// literal. Returns `{ "url", "headers": {"Authorization","Content-Type"}, "body" }`.
/// `temperature_value` defaults to `0.2`, `max_tokens` to `1024` (JS default-param values).
pub fn build_request(input: &Value) -> Result<Value, String> {
    let template_md = input.get("template_md").cloned().unwrap_or(Value::Null);
    let input_kv = input.get("input_kv").cloned().unwrap_or(json!([]));
    let model_id = input.get("model_id").cloned().unwrap_or(Value::Null);
    let temperature_value = nz(input.get("temperature_value")).cloned().unwrap_or(json!(0.2));
    let max_tokens = nz(input.get("max_tokens")).cloned().unwrap_or(json!(1024));

    let api_key = crate::external__helpers_provider_adapter_helpers::requireApiKey(&json!("DEEPSEEK_API_KEY"), &json!("deepseek-call"))?;
    let prompt = crate::external__helpers_provider_adapter_helpers::substituteTemplate(&template_md, &input_kv);

    let body = json!({
        "model": model_id,
        "messages": [{ "role": "user", "content": prompt }],
        "temperature": temperature_value,
        "max_tokens": max_tokens,
    });

    Ok(json!({
        "url": DEEPSEEK_URL,
        "headers": {
            "Authorization": format!("Bearer {}", api_key.as_str().unwrap_or_default()),
            "Content-Type": "application/json",
        },
        "body": body,
    }))
}

/// JS: `const parsed = await parseProviderResponse(...); const text = parsed?.choices?.[0]?.message?.content ?? '';
/// const output = parseJsonOutputOrText(text); const { tokens_in, tokens_out } = normalizeTokens(parsed?.usage);
/// return { output, tokens_in, tokens_out };` — `parsed` here is the ALREADY-successful JSON body
/// returned by `parseProviderResponse` (see that twin for the error-status branches).
pub fn parse_response_body(parsed: &Value) -> Value {
    let text = parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .cloned()
        .unwrap_or(json!(""));
    let output = crate::external__helpers_provider_adapter_helpers::parseJsonOutputOrText(&text);
    let usage = parsed.get("usage").cloned().unwrap_or(Value::Null);
    let tokens = crate::external__helpers_provider_adapter_helpers::normalizeTokens(&usage);
    json!({
        "output": output,
        "tokens_in": tokens.get("tokens_in").cloned().unwrap_or(json!(0)),
        "tokens_out": tokens.get("tokens_out").cloned().unwrap_or(json!(0)),
    })
}

/// PARITY-DEFERRED (whole function — same convention as ai-call.rs's `aiCall`): builds the real
/// request via `build_request` (proving that deterministic surface), then stops before the network
/// hop. Never dispatched by conform-db (no frontier entry, no fixture) — exists so the module
/// compiles and any future caller resolves by name.
pub fn deepseekCall(input: &Value) -> Result<Value, String> {
    let _request = build_request(input)?;
    Err("deepseek-call twin: deepseekCall (the default export) is PARITY-DEFERRED — see the module doc. It dispatches a real network call to the DeepSeek provider (an awaited HTTP round-trip), which is genuinely non-deterministic and not fixturable. Use build_request/parse_response_body for the deterministic surface.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_key<T>(f: impl FnOnce() -> T) -> T {
        // ENV_LOCK, not "no concurrent readers": build_request_errors_without_api_key removes this
        // same process-global var, and under parallel test threads it lands inside this window.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        // SAFETY: test-only env mutation, serialized by the lock above.
        unsafe {
            std::env::set_var("DEEPSEEK_API_KEY", "sk-test-deepseek");
        }
        let out = f();
        unsafe {
            std::env::remove_var("DEEPSEEK_API_KEY");
        }
        out
    }

    #[test]
    fn build_request_assembles_url_headers_body() {
        with_key(|| {
            let req = build_request(&json!({
                "template_md": "Say {{word}}",
                "input_kv": [{ "key": "word", "value": "hi" }],
                "model_id": "deepseek-chat",
                "temperature_value": 0.5,
                "max_tokens": 256,
            }))
            .unwrap();
            assert_eq!(req["url"], json!(DEEPSEEK_URL));
            assert_eq!(req["headers"]["Authorization"], json!("Bearer sk-test-deepseek"));
            assert_eq!(req["headers"]["Content-Type"], json!("application/json"));
            assert_eq!(req["body"]["model"], json!("deepseek-chat"));
            assert_eq!(req["body"]["messages"], json!([{ "role": "user", "content": "Say hi" }]));
            assert_eq!(req["body"]["temperature"], json!(0.5));
            assert_eq!(req["body"]["max_tokens"], json!(256));
        });
    }

    #[test]
    fn build_request_applies_defaults() {
        with_key(|| {
            let req = build_request(&json!({ "template_md": "hi", "model_id": "deepseek-reasoner" })).unwrap();
            assert_eq!(req["body"]["temperature"], json!(0.2));
            assert_eq!(req["body"]["max_tokens"], json!(1024));
        });
    }

    #[test]
    fn build_request_errors_without_api_key() {
        // paired with with_key's guard — without it this removal lands inside their key window.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("DEEPSEEK_API_KEY");
        }
        let err = build_request(&json!({ "template_md": "hi" })).unwrap_err();
        assert!(err.contains("DEEPSEEK_API_KEY"), "{err}");
    }

    #[test]
    fn parse_response_body_extracts_output_and_tokens() {
        let parsed = json!({
            "choices": [{ "message": { "content": "{\"a\":1}" } }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 4 },
        });
        let out = parse_response_body(&parsed);
        assert_eq!(out, json!({ "output": { "a": 1 }, "tokens_in": 10, "tokens_out": 4 }));
    }

    #[test]
    fn parse_response_body_defaults_on_missing_fields() {
        let out = parse_response_body(&json!({}));
        assert_eq!(out, json!({ "output": "", "tokens_in": 0, "tokens_out": 0 }));
    }

    #[test]
    fn deepseek_call_is_parity_deferred() {
        with_key(|| {
            let err = deepseekCall(&json!({ "template_md": "hi", "model_id": "deepseek-chat" })).unwrap_err();
            assert!(err.contains("PARITY-DEFERRED"), "{err}");
        });
    }
}
