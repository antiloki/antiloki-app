//! openrouter-image-call.rs — twin of skills/external/openrouter-image-call.mjs. NAMES ARE
//! CONTRACT: default `openrouterImageCall`. Adapter for IMAGE GENERATION through OpenRouter
//! (nano-banana class models) — same endpoint as the already-converted `external_openrouter_call`
//! twin (https://openrouter.ai/api/v1/chat/completions), reused here via `OPENROUTER_URL` /
//! `read_key` (byte-identical key resolution, same provider slug "openrouter").
//!
//! LANE A (bespoke-native network adapter, crossing playbook): the real
//! `fetchFn(URL, ...)` round-trip is the DEFERRED boundary — genuinely non-deterministic (a live
//! provider's response, including OpenRouter's ACTUAL billed cost), not JSON-fixturable,
//! unconvertible IN PRINCIPLE. Everything DETERMINISTIC around it is ported faithfully as pure
//! fns and IS what this file's tests exercise:
//! - `build_request(input, api_key)`: assembles the exact request URL/headers/body the JS sends
//!   (`model`, single user-role `messages` entry carrying the raw prompt, `modalities:
//!   ['image','text']`, `usage: { include: true }`).
//! - `parse_response_body(parsed, model_id)`: mirrors the JS post-fetch tail — the `parsed?.error`
//!   early-throw, the `choices[0].message.images[0].image_url.url` extraction (`?? null` then a
//!   "no image in the response" throw when absent), and the `tokens_in`/`tokens_out`/`cost_usd`/
//!   `generation_id` extraction (`?? 0` / `?? null` per JS nullish-coalescing).
//!
//! `openrouterImageCall` (the default export) is the PARITY-DEFERRED stub: it validates `prompt`,
//! resolves the key, and builds the request (proving the deterministic surface is
//! reachable/correct) then returns Err before the network hop, same convention as
//! `external_openrouter_call::openrouterCall`. No fixture, no frontier entry — BESPOKE-NATIVE
//! TESTING only, via `#[cfg(test)]` below.
//!
//! `guardRealNetworkInTests` (JS: blocks real spend under `NODE_TEST_CONTEXT` unless
//! `ANTILOKI_ALLOW_REAL_AI=1`) has no Rust equivalent — this twin never reaches the real network
//! branch at all (PARITY-DEFERRED), so the money-guard's protected code path is unreachable here,
//! same omission as `external_openrouter_call`.

use serde_json::{json, Value};

pub const URL: &str = crate::external_openrouter_call::OPENROUTER_URL;

fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

/// JS: the request-body assembly inside `openrouterImageCall` — `model: model_id`, a single
/// user-role message carrying the raw `prompt`, `modalities: ['image', 'text']`,
/// `usage: { include: true }` (OpenRouter returns the actual cost — metered, not estimated).
/// Returns `{ "url", "method", "headers": {...}, "body": {...} }`.
pub fn build_request(prompt: &str, model_id: &str, api_key: &str) -> Value {
    json!({
        "url": URL,
        "method": "POST",
        "headers": {
            "Authorization": format!("Bearer {api_key}"),
            "Content-Type": "application/json",
        },
        "body": {
            "model": model_id,
            "messages": [{ "role": "user", "content": prompt }],
            "modalities": ["image", "text"],
            "usage": { "include": true },
        },
    })
}

/// JS: the post-fetch tail of `openrouterImageCall` — the `parsed?.error` early-throw, the
/// `choices[0].message.images[0].image_url.url ?? null` extraction with a "no image in the
/// response" throw when absent, then the `tokens_in`/`tokens_out`/`cost_usd`/`generation_id`
/// extraction (`?? 0` / `?? null`). `model_id` is threaded through purely for the error messages
/// (matches JS closure capture).
pub fn parse_response_body(parsed: &Value, model_id: &str) -> Result<Value, String> {
    if let Some(err) = nz(parsed.get("error")) {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| err.to_string());
        return Err(format!(
            "OpenRouter image error for '{model_id}': {msg}"
        ));
    }

    let msg = parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .cloned()
        .unwrap_or(json!({}));
    let img = msg
        .get("images")
        .and_then(|i| i.get(0))
        .and_then(|i| i.get("image_url"))
        .and_then(|i| i.get("url"))
        .filter(|v| !v.is_null())
        .cloned();
    let img = match img {
        Some(v) => v,
        None => {
            return Err(format!(
                "no image in the response (model '{model_id}' may not support image output)"
            ))
        }
    };

    let tokens_in = parsed
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(json!(0));
    let tokens_out = parsed
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(json!(0));
    let cost_usd = parsed
        .get("usage")
        .and_then(|u| u.get("cost"))
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(Value::Null);
    let generation_id = nz(parsed.get("id")).cloned().unwrap_or(Value::Null);

    Ok(json!({
        "image_data_url": img,
        "tokens_in": tokens_in,
        "tokens_out": tokens_out,
        "cost_usd": cost_usd,
        "generation_id": generation_id,
    }))
}

/// `export default async function openrouterImageCall(input, context = {})` — `context` is
/// accepted for contract-shape parity (`context.fetch_fn` in JS has no Rust equivalent, see
/// module doc) but unused, mirrored by the underscore-prefixed binding below.
pub fn openrouterImageCall(input: &Value, _context: &Value) -> Result<Value, String> {
    let prompt = input.get("prompt").and_then(Value::as_str).unwrap_or("");
    if prompt.is_empty() {
        return Err("openrouter-image-call: prompt required".to_string());
    }
    let model_id = nz(input.get("model_id"))
        .and_then(Value::as_str)
        .unwrap_or("google/gemini-2.5-flash-image-preview")
        .to_string();

    let api_key = crate::external_openrouter_call::read_key()?;
    let _request = build_request(prompt, &model_id, &api_key);

    // PARITY-DEFERRED: real HTTP fetch against openrouter.ai is not implemented in this crate (no
    // HTTP client dependency). Everything up to this point (key resolution, request assembly) is
    // real and deterministic; the network call itself is the documented deferred boundary,
    // matching the crossing playbook's lane-A contract for network provider adapters.
    Err("openrouter-image-call: PARITY-DEFERRED — real HTTP fetch is not implemented in this twin".to_string())
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

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-openrouter-image-call-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn build_request_assembles_url_headers_and_body() {
        let req = build_request("draw a cat", "google/gemini-2.5-flash-image-preview", "sk-test");
        assert_eq!(req["url"], json!(URL));
        assert_eq!(req["method"], json!("POST"));
        assert_eq!(req["headers"]["Authorization"], json!("Bearer sk-test"));
        assert_eq!(req["headers"]["Content-Type"], json!("application/json"));
        assert_eq!(req["body"]["model"], json!("google/gemini-2.5-flash-image-preview"));
        assert_eq!(req["body"]["messages"], json!([{ "role": "user", "content": "draw a cat" }]));
        assert_eq!(req["body"]["modalities"], json!(["image", "text"]));
        assert_eq!(req["body"]["usage"], json!({ "include": true }));
    }

    #[test]
    fn parse_response_body_extracts_image_and_usage() {
        let parsed = json!({
            "id": "gen-987",
            "choices": [{ "message": { "images": [{ "image_url": { "url": "data:image/png;base64,AAAA" } }] } }],
            "usage": { "prompt_tokens": 12, "completion_tokens": 3, "cost": 0.02 },
        });
        let out = parse_response_body(&parsed, "google/gemini-2.5-flash-image-preview").unwrap();
        assert_eq!(out["image_data_url"], json!("data:image/png;base64,AAAA"));
        assert_eq!(out["tokens_in"], json!(12));
        assert_eq!(out["tokens_out"], json!(3));
        assert_eq!(out["cost_usd"], json!(0.02));
        assert_eq!(out["generation_id"], json!("gen-987"));
    }

    #[test]
    fn parse_response_body_defaults_missing_usage_and_id_to_zero_or_null() {
        let parsed = json!({
            "choices": [{ "message": { "images": [{ "image_url": { "url": "data:image/png;base64,BBBB" } }] } }],
        });
        let out = parse_response_body(&parsed, "m").unwrap();
        assert_eq!(out["tokens_in"], json!(0));
        assert_eq!(out["tokens_out"], json!(0));
        assert_eq!(out["cost_usd"], Value::Null);
        assert_eq!(out["generation_id"], Value::Null);
    }

    #[test]
    fn parse_response_body_errors_on_provider_error_field() {
        let parsed = json!({ "error": { "message": "model deprecated" } });
        let e = parse_response_body(&parsed, "meta-llama/llama-3.3-70b").unwrap_err();
        assert!(e.contains("model deprecated"), "{e}");
        assert!(e.contains("meta-llama/llama-3.3-70b"), "{e}");
    }

    #[test]
    fn parse_response_body_errors_when_no_image_in_response() {
        let parsed = json!({ "choices": [{ "message": { "content": "sorry, text only" } }] });
        let e = parse_response_body(&parsed, "openai/gpt-4o-mini").unwrap_err();
        assert!(e.contains("no image in the response"), "{e}");
        assert!(e.contains("openai/gpt-4o-mini"), "{e}");
    }

    #[test]
    fn parse_response_body_errors_when_image_url_is_null() {
        let parsed = json!({ "choices": [{ "message": { "images": [{ "image_url": { "url": null } }] } }] });
        let e = parse_response_body(&parsed, "m").unwrap_err();
        assert!(e.contains("no image in the response"), "{e}");
    }

    #[test]
    fn openrouter_image_call_requires_prompt() {
        let e = openrouterImageCall(&json!({}), &json!({})).unwrap_err();
        assert_eq!(e, "openrouter-image-call: prompt required");
        let e2 = openrouterImageCall(&json!({ "prompt": "" }), &json!({})).unwrap_err();
        assert_eq!(e2, "openrouter-image-call: prompt required");
    }

    #[test]
    fn openrouter_image_call_returns_parity_deferred_after_real_key_resolution() {
        let dir = temp_dir("call-deferred");
        let auth_dir = dir.join("auth");
        fs::create_dir_all(&auth_dir).unwrap();
        fs::write(auth_dir.join("openrouter.key"), "sk-real").unwrap();
        let input = json!({ "prompt": "a cat riding a bike" });
        let e = with_home(dir.to_str().unwrap(), || openrouterImageCall(&input, &json!({}))).unwrap_err();
        assert_eq!(e, "openrouter-image-call: PARITY-DEFERRED — real HTTP fetch is not implemented in this twin");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn openrouter_image_call_propagates_missing_key_error() {
        let dir = temp_dir("call-no-key");
        let e = with_home(dir.to_str().unwrap(), || openrouterImageCall(&json!({ "prompt": "x" }), &json!({}))).unwrap_err();
        assert!(e.starts_with("OpenRouter API key not found."), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }
}
