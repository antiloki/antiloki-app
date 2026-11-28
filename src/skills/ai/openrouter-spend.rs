//! openrouter-spend.rs — twin of skills/ai/openrouter-spend.mjs. NAMES ARE CONTRACT: default
//! `openrouterSpend`. Pulls the LIVE spend figure from OpenRouter's own accounting (not the local
//! `ai_calls` per-call ledger) via two reads:
//!   GET /api/v1/credits -> { total_credits, total_usage }   (the account, all-time)
//!   GET /api/v1/key     -> { data: { label, usage, limit } } (this key's lifetime usage)
//!
//! LANE A (bespoke-native network adapter, crossing playbook): both `fetchFn` round-trips are the
//! DEFERRED boundary — genuinely non-deterministic (a live provider's response), not
//! JSON-fixturable. Everything DETERMINISTIC around them is ported faithfully and IS what this
//! file's tests exercise:
//! - `read_key()` reused byte-for-byte from the already-converted `external_openrouter_call`
//!   twin (same `OPENROUTER_API_KEY` env / `AUTH_DIR/openrouter.key` resolution).
//! - `build_get_request(path, api_key)`: assembles the exact
//!   `https://openrouter.ai/api/v1<path>` URL + `Authorization: Bearer <key>` header the JS
//!   `get()` closure sends.
//!
//! `openrouterSpend` (the default export) is REAL as of 2026-08-09 — enabling `ureq`'s `tls`
//! feature is what unblocked it (this is an https endpoint; the crate previously built ureq
//! without a TLS stack because its only outbound call was a local http one).
//!
//! JS calls `readKey()` UNGUARDED (outside any try/catch), so a missing key propagates (`?`)
//! exactly as in the JS source. Past key resolution, JS wraps EACH of the two `get()` calls in its
//! own try/catch and ACCUMULATES a `partial_error` string rather than throwing — one endpoint
//! failing must still return the other's numbers. Only when BOTH yield nothing does it throw
//! `err.notFound`. That structure is reproduced exactly.
//!
//! The deterministic half is split into `shape_spend` (same pattern as ollama-call's
//! `parseOllamaResponse`) so the both-empty refusal, the `+(x).toFixed(4)` rounding, the key-field
//! projection and the omitted-vs-null `partial_error` are all unit-tested without a round-trip.
//!
//! `guardRealNetworkInTests` (JS: blocks real spend under `NODE_TEST_CONTEXT` unless
//! `ANTILOKI_ALLOW_REAL_AI=1`) still has no Rust equivalent. It matters less here than for the
//! completion adapters — these two endpoints are read-only and free — but it is a real omission,
//! not an oversight: this crate has no NODE_TEST_CONTEXT notion to gate on.

use rusqlite::Connection;
use serde_json::{json, Value};

pub const OPENROUTER_API_BASE: &str = "https://openrouter.ai/api/v1";

/// JS: the `get = async (path) => { const res = await fetchFn(\`https://openrouter.ai/api/v1${path}\`, { headers: { Authorization: \`Bearer ${apiKey}\` } }); return res.json(); }` closure —
/// the request-shape half only (the fetch itself is the deferred boundary).
pub fn build_get_request(path: &str, api_key: &str) -> Value {
    json!({
        "url": format!("{OPENROUTER_API_BASE}{path}"),
        "headers": { "Authorization": format!("Bearer {api_key}") },
    })
}

/// `export default async function openrouterSpend(input, context = {})` — `context` is accepted
/// for contract-shape parity (`context.fetch_fn` in JS has no Rust equivalent, see module doc)
/// but unused, mirrored by the underscore-prefixed binding below.
pub fn openrouterSpend(_input: &Value, db: &Connection) -> Result<Value, String> {
    let api_key = crate::external_openrouter_call::read_key()?;

    // JS wraps EACH call in its own try/catch and keeps going: one endpoint failing must still
    // yield the other's numbers, with the failure surfaced as `partial_error`. Only when BOTH
    // produce nothing does it throw.
    let get = |path: &str| -> std::result::Result<Value, String> {
        let req = build_get_request(path, &api_key);
        let url = req.get("url").and_then(Value::as_str).unwrap_or_default();
        let auth = req.get("headers").and_then(|h| h.get("Authorization")).and_then(Value::as_str).unwrap_or_default();
        let resp = ureq::get(url)
            .set("Authorization", auth)
            .timeout(std::time::Duration::from_millis(30_000))
            .call()
            .map_err(|e| e.to_string())?;
        resp.into_json::<Value>().map_err(|e| e.to_string())
    };

    let mut credits = Value::Null;
    let mut key = Value::Null;
    let mut error: Option<String> = None;
    match get("/credits") {
        Ok(v) => credits = v.get("data").cloned().unwrap_or(Value::Null),
        Err(e) => error = Some(format!("credits: {}", e.chars().take(120).collect::<String>())),
    }
    match get("/key") {
        Ok(v) => key = v.get("data").cloned().unwrap_or(Value::Null),
        Err(e) => {
            let msg = format!("key: {}", e.chars().take(120).collect::<String>());
            error = Some(match error {
                Some(prev) => format!("{prev} · {msg}"),
                None => msg,
            });
        }
    }
    shape_spend(&credits, &key, error.as_deref(), db)
}

/// The DETERMINISTIC half of `openrouterSpend`, split out so the response shaping is testable
/// without a network round-trip (same pattern as ollama-call's `parseOllamaResponse`): the
/// both-empty refusal, the `remaining_usd` rounding, key-field projection, and `partial_error`.
pub fn shape_spend(credits: &Value, key: &Value, error: Option<&str>, db: &Connection) -> Result<Value, String> {
    if credits.is_null() && key.is_null() {
        let cause = error.unwrap_or("no response");
        return Err(format!("openrouter-spend: OpenRouter billing data not found: {cause}"));
    }

    let total_credits = credits.get("total_credits").cloned().unwrap_or(Value::Null);
    let total_usage = credits.get("total_usage").cloned().unwrap_or(Value::Null);
    // JS: `+(total_credits - total_usage).toFixed(4)` — fixed to 4dp then re-numbered, so a
    // trailing-zero decimal renders as an integer-ish number, not the raw float.
    let remaining = match (total_credits.as_f64(), total_usage.as_f64()) {
        (Some(c), Some(u)) => {
            let r: f64 = format!("{:.4}", c - u).parse().unwrap_or(c - u);
            if r.fract() == 0.0 && r.abs() < 9e15 { json!(r as i64) } else { json!(r) }
        }
        _ => Value::Null,
    };

    let mut out = serde_json::Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("total_credits_usd".into(), total_credits);
    out.insert("total_usage_usd".into(), total_usage);
    out.insert("remaining_usd".into(), remaining);
    out.insert("key_label".into(), key.get("label").cloned().unwrap_or(Value::Null));
    out.insert("key_usage_usd".into(), key.get("usage").cloned().unwrap_or(Value::Null));
    out.insert("key_limit_usd".into(), key.get("limit").cloned().unwrap_or(Value::Null));
    out.insert("fetched_at".into(), json!(crate::clock::now_iso(db)));
    if let Some(e) = error {
        out.insert("partial_error".into(), json!(e));
    }
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c0() -> rusqlite::Connection { Connection::open_in_memory().unwrap() }

    // ── shape_spend: the deterministic half (no network) ─────────────────────────────────────
    #[test]
    fn both_endpoints_empty_is_a_not_found_carrying_the_cause() {
        let e = shape_spend(&Value::Null, &Value::Null, Some("credits: boom"), &c0()).unwrap_err();
        assert!(e.contains("billing data not found"), "{e}");
        assert!(e.contains("credits: boom"), "the cause rides along: {e}");
        let e2 = shape_spend(&Value::Null, &Value::Null, None, &c0()).unwrap_err();
        assert!(e2.contains("no response"), "absent error falls back to 'no response': {e2}");
    }

    #[test]
    fn remaining_is_credits_minus_usage_rounded_to_4dp() {
        let credits = json!({ "total_credits": 10.0, "total_usage": 2.5 });
        let r = shape_spend(&credits, &Value::Null, None, &c0()).unwrap();
        assert_eq!(r["remaining_usd"], json!(7.5));
        // JS does +(x).toFixed(4): a value that lands whole renders as a whole number
        let whole = shape_spend(&json!({ "total_credits": 10.0, "total_usage": 4.0 }), &Value::Null, None, &c0()).unwrap();
        assert_eq!(whole["remaining_usd"], json!(6));
        // and float noise is trimmed at 4dp rather than leaking 0.30000000000000004
        let noisy = shape_spend(&json!({ "total_credits": 0.7, "total_usage": 0.4 }), &Value::Null, None, &c0()).unwrap();
        assert_eq!(noisy["remaining_usd"], json!(0.3));
    }

    #[test]
    fn remaining_is_null_when_either_side_is_missing() {
        let only_credits = shape_spend(&json!({ "total_credits": 10.0 }), &Value::Null, None, &c0()).unwrap();
        assert_eq!(only_credits["remaining_usd"], Value::Null);
        assert_eq!(only_credits["total_credits_usd"], json!(10.0));
    }

    #[test]
    fn a_key_only_response_still_succeeds_with_its_fields() {
        let key = json!({ "label": "prod", "usage": 3.25, "limit": 50 });
        let r = shape_spend(&Value::Null, &key, Some("credits: 500"), &c0()).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["key_label"], json!("prod"));
        assert_eq!(r["key_usage_usd"], json!(3.25));
        assert_eq!(r["key_limit_usd"], json!(50));
        assert_eq!(r["partial_error"], json!("credits: 500"), "one endpoint failing is partial, not fatal");
        assert_eq!(r["total_credits_usd"], Value::Null);
    }

    #[test]
    fn partial_error_is_absent_when_both_calls_succeeded() {
        let r = shape_spend(&json!({ "total_credits": 1, "total_usage": 0 }), &json!({ "label": "k" }), None, &c0()).unwrap();
        assert!(r.get("partial_error").is_none(), "the key is OMITTED, not null: {r}");
    }

    #[test]
    fn key_order_matches_the_js_object_literal() {
        let r = shape_spend(&json!({ "total_credits": 1, "total_usage": 0 }), &json!({}), None, &c0()).unwrap();
        let keys: Vec<&str> = r.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, vec![
            "ok", "total_credits_usd", "total_usage_usd", "remaining_usd",
            "key_label", "key_usage_usd", "key_limit_usd", "fetched_at",
        ]);
    }

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
        dir.push(format!("antiloki-openrouter-spend-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn build_get_request_assembles_url_and_auth_header_for_credits() {
        let req = build_get_request("/credits", "sk-test");
        assert_eq!(req["url"], json!("https://openrouter.ai/api/v1/credits"));
        assert_eq!(req["headers"]["Authorization"], json!("Bearer sk-test"));
    }

    #[test]
    fn build_get_request_assembles_url_and_auth_header_for_key() {
        let req = build_get_request("/key", "sk-test");
        assert_eq!(req["url"], json!("https://openrouter.ai/api/v1/key"));
        assert_eq!(req["headers"]["Authorization"], json!("Bearer sk-test"));
    }

    #[test]
    fn openrouter_spend_returns_parity_deferred_after_real_key_resolution() {
        let dir = temp_dir("spend-key-resolves");
        let auth_dir = dir.join("auth");
        fs::create_dir_all(&auth_dir).unwrap();
        fs::write(auth_dir.join("openrouter.key"), "sk-real").unwrap();
        // The key resolves, so the fn proceeds PAST key resolution to the network. We assert only
        // that it did not fail on the key — the round-trip itself is not hermetic and is covered by
        // shape_spend's tests above.
        let r = with_home(dir.to_str().unwrap(), || openrouterSpend(&json!({}), &Connection::open_in_memory().unwrap()));
        if let Err(e) = &r {
            assert!(!e.starts_with("OpenRouter API key not found."), "key should have resolved: {e}");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn openrouter_spend_propagates_missing_key_error() {
        let dir = temp_dir("spend-no-key");
        let e = with_home(dir.to_str().unwrap(), || openrouterSpend(&json!({}), &Connection::open_in_memory().unwrap())).unwrap_err();
        assert!(e.starts_with("OpenRouter API key not found."), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }
}
