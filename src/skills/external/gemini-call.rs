//! gemini-call.rs — twin of skills/external/gemini-call.mjs. NAMES ARE CONTRACT: default
//! `geminiCall`. ProviderAdapter for Google Gemini via Google AI Studio
//! (generativelanguage.googleapis.com) — a different envelope from the OpenAI-compatible
//! adapters (contents/parts body, key-as-query-param auth, candidates/usageMetadata response).
//!
//! LANE A (bespoke-native network adapter, crossing playbook): the real
//! `fetchFn(url, ...)` round-trip is the DEFERRED boundary — genuinely non-deterministic (a live
//! provider's response), not JSON-fixturable, unconvertible IN PRINCIPLE. Everything
//! DETERMINISTIC around it is ported faithfully as pure fns and IS what this file's tests
//! exercise:
//! - `temp_to_generation_config(temperature_value, max_tokens, wants_json)`: twin of
//!   `tempToGenerationConfig` — `{ temperature, maxOutputTokens }` plus `responseMimeType:
//!   "application/json"` only when `wants_json` (replay-identity preserving: field absent
//!   otherwise, matching JS's conditional `cfg.responseMimeType = ...`).
//! - `build_request(input, api_key)`: assembles the exact request URL/body the JS sends —
//!   `substituteTemplate` reused byte-for-byte from the already-converted
//!   `external__helpers_provider_adapter_helpers` twin, `looksLikeJsonPrompt` reused from the
//!   already-converted `external_openai_call` twin (JS: `import { looksLikeJsonPrompt } from
//!   './openai-call.mjs'`). `encode_uri_component` mirrors JS's `encodeURIComponent` unreserved
//!   set (`A-Z a-z 0-9 - _ . ! ~ * ' ( )`) since the model_id is interpolated raw into the URL
//!   path and the crate has no URL-encoding dependency (zero-new-Cargo-deps per the crossing
//!   playbook).
//! - `parse_response_body(parsed)`: mirrors the JS response-shape extraction —
//!   `candidates[0].content.parts[*].text` joined (missing/non-array ⇒ `""`, matching the JS
//!   optional-chain + `?? ''` fallback), then `parseJsonOutputOrText` for `output`;
//!   `usageMetadata.promptTokenCount`/`candidatesTokenCount` (JS comment: "normalizeTokens not
//!   used here — Gemini's shape doesn't match the common aliases. Inline extraction is
//!   clearer.") coerced via `Number(x) || 0` semantics for `tokens_in`/`tokens_out`.
//!
//! `requireApiKey('GEMINI_API_KEY', 'gemini-call')` is reused byte-for-byte from the helper twin
//! (unlike anthropic-call/openrouter-call's file-based BYOK key, gemini-call's JS source reads
//! only the env var — no `AUTH_DIR/gemini.key` fallback, so this twin has none either).
//!
//! `geminiCall` (the default export) is the PARITY-DEFERRED stub: it resolves the key and builds
//! the request (proving the deterministic surface is reachable/correct) then returns Err before
//! the network hop, same convention as openrouter-call.rs's `openrouterCall` / deepseek-call.rs's
//! `deepseekCall`. No fixture, no frontier entry — BESPOKE-NATIVE TESTING only, via `#[cfg(test)]`
//! below.
//!
//! `DEFAULT_TIMEOUT_MS` (`ANTILOKI_GEMINI_TIMEOUT_MS` env, default 90_000) governs the JS
//! `AbortController` timeout — inert here since no real HTTP client is wired, kept as a named
//! const for parity/documentation with the JS source.

use serde_json::{json, Value};

pub const GEMINI_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";

fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

// JS: encodeURIComponent(model_id) — unreserved set: A-Z a-z 0-9 - _ . ! ~ * ' ( )
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// JS: `function tempToGenerationConfig(temperature_value, max_tokens, wantsJson)`.
pub fn temp_to_generation_config(temperature_value: &Value, max_tokens: &Value, wants_json: bool) -> Value {
    let mut cfg = json!({
        "temperature": temperature_value,
        "maxOutputTokens": max_tokens,
    });
    if wants_json {
        cfg["responseMimeType"] = json!("application/json");
    }
    cfg
}

/// JS: the `url`/`body` assembly inside `geminiCall`, up to (not including) the fetch call.
/// `input_kv` defaults to `[]`, `temperature_value` to `0.2`, `max_tokens` to `1024` (JS
/// default-param values). Returns `{ "url", "body": {...} }`.
pub fn build_request(input: &Value, api_key: &str) -> Value {
    let template_md = input.get("template_md").cloned().unwrap_or(Value::Null);
    let input_kv = nz(input.get("input_kv")).cloned().unwrap_or(json!([]));
    let model_id = input.get("model_id").cloned().unwrap_or(Value::Null);
    let temperature_value = nz(input.get("temperature_value")).cloned().unwrap_or(json!(0.2));
    let max_tokens = nz(input.get("max_tokens")).cloned().unwrap_or(json!(1024));

    let prompt = crate::external__helpers_provider_adapter_helpers::substituteTemplate(&template_md, &input_kv);
    let wants_json = crate::external_openai_call::looksLikeJsonPrompt(&template_md).as_bool().unwrap_or(false);

    let model_id_str = model_id.as_str().map(|s| s.to_string()).unwrap_or_default();
    let url = format!(
        "{GEMINI_BASE}/{}:generateContent?key={api_key}",
        encode_uri_component(&model_id_str)
    );

    let body = json!({
        "contents": [{ "role": "user", "parts": [{ "text": prompt }] }],
        "generationConfig": temp_to_generation_config(&temperature_value, &max_tokens, wants_json),
    });

    json!({ "url": url, "body": body })
}

/// JS: `number(x) || 0`-style coercion for `tokens_in`/`tokens_out`.
fn number_or_zero(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => {
            let f = n.as_f64().unwrap_or(0.0);
            if f == 0.0 || f.is_nan() { 0 } else { f as i64 }
        }
        Some(Value::String(s)) => s.trim().parse::<f64>().ok().filter(|f| *f != 0.0 && !f.is_nan()).map(|f| f as i64).unwrap_or(0),
        _ => 0,
    }
}

/// JS: the post-`parseProviderResponse` tail of `geminiCall` — `candidates[0].content.parts[*]
/// .text` joined, `parseJsonOutputOrText`, then `usageMetadata.promptTokenCount` /
/// `candidatesTokenCount` coerced via `Number(x) || 0`.
pub fn parse_response_body(parsed: &Value) -> Value {
    let text = parsed
        .get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .map(|p| p.get("text").and_then(Value::as_str).unwrap_or(""))
                .collect::<String>()
        })
        .unwrap_or_default();

    let output = crate::external__helpers_provider_adapter_helpers::parseJsonOutputOrText(&json!(text));
    let um = parsed.get("usageMetadata");
    let tokens_in = number_or_zero(um.and_then(|u| u.get("promptTokenCount")));
    let tokens_out = number_or_zero(um.and_then(|u| u.get("candidatesTokenCount")));

    json!({
        "output": output,
        "tokens_in": tokens_in,
        "tokens_out": tokens_out,
    })
}

/// `export default async function geminiCall(input, context = {})` — `context` is accepted for
/// contract-shape parity (`context.fetch_fn` in JS has no Rust equivalent, see module doc) but
/// unused, mirrored by the underscore-prefixed binding below.
pub fn geminiCall(input: &Value, _context: &Value) -> Result<Value, String> {
    let api_key = crate::external__helpers_provider_adapter_helpers::requireApiKey(&json!("GEMINI_API_KEY"), &json!("gemini-call"))?;
    let _request = build_request(input, api_key.as_str().unwrap_or_default());

    // PARITY-DEFERRED: real HTTP fetch against generativelanguage.googleapis.com is not
    // implemented in this crate (no HTTP client dependency). Everything up to this point (key
    // resolution, request assembly) is real and deterministic; the network call itself is the
    // documented deferred boundary, matching the crossing playbook's lane-A contract for network
    // provider adapters.
    Err("gemini-call: PARITY-DEFERRED — real HTTP fetch is not implemented in this twin".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_env_key<T>(key: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("GEMINI_API_KEY", key);
        }
        let result = f();
        unsafe {
            std::env::remove_var("GEMINI_API_KEY");
        }
        result
    }

    fn without_env_key<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("GEMINI_API_KEY");
        }
        f()
    }

    #[test]
    fn temp_to_generation_config_omits_response_mime_type_when_not_json() {
        let cfg = temp_to_generation_config(&json!(0.2), &json!(1024), false);
        assert_eq!(cfg, json!({ "temperature": 0.2, "maxOutputTokens": 1024 }));
    }

    #[test]
    fn temp_to_generation_config_sets_response_mime_type_when_json() {
        let cfg = temp_to_generation_config(&json!(0.5), &json!(2048), true);
        assert_eq!(
            cfg,
            json!({ "temperature": 0.5, "maxOutputTokens": 2048, "responseMimeType": "application/json" })
        );
    }

    #[test]
    fn build_request_assembles_url_and_body_with_defaults() {
        let req = build_request(
            &json!({ "template_md": "Say {{word}}", "input_kv": [{ "key": "word", "value": "hi" }], "model_id": "gemini-2.0-flash" }),
            "sk-test",
        );
        assert_eq!(
            req["url"],
            json!("https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:generateContent?key=sk-test")
        );
        assert_eq!(
            req["body"]["contents"],
            json!([{ "role": "user", "parts": [{ "text": "Say hi" }] }])
        );
        assert_eq!(req["body"]["generationConfig"], json!({ "temperature": 0.2, "maxOutputTokens": 1024 }));
    }

    #[test]
    fn build_request_encodes_model_id_in_url() {
        let req = build_request(&json!({ "template_md": "hi", "model_id": "models/gemini pro" }), "sk");
        assert_eq!(
            req["url"],
            json!("https://generativelanguage.googleapis.com/v1beta/models/models%2Fgemini%20pro:generateContent?key=sk")
        );
    }

    #[test]
    fn build_request_sets_response_mime_type_when_prompt_looks_like_json() {
        let req = build_request(&json!({ "template_md": "Output JSON only please", "model_id": "m" }), "sk");
        assert_eq!(req["body"]["generationConfig"]["responseMimeType"], json!("application/json"));
    }

    #[test]
    fn build_request_uses_custom_temperature_and_max_tokens() {
        let req = build_request(&json!({ "template_md": "hi", "model_id": "m", "temperature_value": 0.9, "max_tokens": 256 }), "sk");
        assert_eq!(req["body"]["generationConfig"]["temperature"], json!(0.9));
        assert_eq!(req["body"]["generationConfig"]["maxOutputTokens"], json!(256));
    }

    #[test]
    fn parse_response_body_extracts_joined_text_and_tokens() {
        let parsed = json!({
            "candidates": [{ "content": { "parts": [{ "text": "Hello, " }, { "text": "world" }] } }],
            "usageMetadata": { "promptTokenCount": 12, "candidatesTokenCount": 5 },
        });
        let out = parse_response_body(&parsed);
        assert_eq!(out["output"], json!("Hello, world"));
        assert_eq!(out["tokens_in"], json!(12));
        assert_eq!(out["tokens_out"], json!(5));
    }

    #[test]
    fn parse_response_body_parses_json_output_when_shaped_like_json() {
        let parsed = json!({
            "candidates": [{ "content": { "parts": [{ "text": "{\"a\":1}" }] } }],
        });
        let out = parse_response_body(&parsed);
        assert_eq!(out["output"], json!({ "a": 1 }));
    }

    #[test]
    fn parse_response_body_missing_candidates_defaults_to_empty_text_and_zero_tokens() {
        let out = parse_response_body(&json!({}));
        assert_eq!(out["output"], json!(""));
        assert_eq!(out["tokens_in"], json!(0));
        assert_eq!(out["tokens_out"], json!(0));
    }

    #[test]
    fn parse_response_body_part_missing_text_field_treated_as_empty_string() {
        let parsed = json!({
            "candidates": [{ "content": { "parts": [{ "text": "a" }, {}, { "text": "b" }] } }],
        });
        let out = parse_response_body(&parsed);
        assert_eq!(out["output"], json!("ab"));
    }

    #[test]
    fn gemini_call_returns_parity_deferred_after_real_key_resolution() {
        let input = json!({ "template_md": "hi", "model_id": "gemini-2.0-flash" });
        let e = with_env_key("sk-real", || geminiCall(&input, &json!({}))).unwrap_err();
        assert_eq!(e, "gemini-call: PARITY-DEFERRED — real HTTP fetch is not implemented in this twin");
    }

    #[test]
    fn gemini_call_propagates_missing_key_error() {
        let e = without_env_key(|| geminiCall(&json!({}), &json!({}))).unwrap_err();
        assert!(e.contains("GEMINI_API_KEY") || e.to_lowercase().contains("key"), "{e}");
    }
}
