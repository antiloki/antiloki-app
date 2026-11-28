//! provider-test-call.rs — twin of skills/system/provider-test-call.mjs (db READ lane).
//! NAMES ARE CONTRACT: `providerTestCall` (default export).
//!
//! Resolves ONE provider row from `brain.ai_providers` by `provider_id` (deterministic, real db
//! read — ported below), then `import()`s the provider's `adapter_module` path and fires a live
//! "reply HELLO" completion call to measure ok/latency/tokens/preview. That adapter dispatch — a
//! dynamic `import()` of an arbitrary module path drawn from a db column, followed by an awaited
//! real AI completion call — has no faithful Rust equivalent (same class as byok-test.rs's
//! `byokTest` / ai-call.rs's `aiCall`: genuinely non-deterministic, non-portable, not
//! fixturable). It is the PARITY-DEFERRED boundary.
//!
//! Everything deterministic around that boundary is ported faithfully: the `provider_id`
//! required-input guard (JS `provider_id == null`, so `0`/`""` are VALID ids — only missing/null
//! trips the guard), the `brain.ai_providers` lookup by id, and both guard branches that never
//! reach the adapter (`{ok:false, error:"provider id=<id> not found", provider_id}` when no row;
//! `{ok:false, error:"provider has no adapter_module set", provider}` — echoing the FULL row —
//! when `adapter_module` is unset). Only once a provider AND its adapter_module are both resolved
//! does the twin hit the deferred boundary, reported in the same `{ok:false, latency_ms, error,
//! provider, model_id}` shape as the JS catch branch (the JS success branch additionally carries
//! `tokens_in`/`tokens_out`/`response_preview`/`tier` — none of that is knowable without a real
//! adapter call, so the deferred report mirrors the FAILURE shape, which is exactly what the JS
//! itself would produce if the (unreachable-here) adapter import/call threw).
//!
//! `latency_ms` mirrors the JS's `performance.now()` delta with a REAL `std::time::Instant`
//! measurement around the (deferred) dispatch — not a fixture-frozen value, since this whole
//! skill is bespoke-native (no conform-db comparison ever runs against it).
//!
//! BESPOKE NATIVE TESTING only (no fixture/frontier — dynamic adapter import + a real AI call sit
//! on the success path, so there is nothing conform-db could freeze). Proven below via
//! `#[cfg(test)]` against a real in-memory db with `brain.ai_providers` attached, per the
//! `db0()`/`seed()` convention in byok-test.rs.
//!
//! The `if (!db) throw err.contextMissing('provider-test-call', 'db')` guard cannot fire here —
//! the db lane always hands the twin a live connection (same note as byok-test.rs).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::time::Instant;

// = JS template-literal `${v}` coercion (undefined->"undefined", null->"null").
fn tmpl(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// Mirrors the JS `import(adapter_module)` + live adapter call step. There is no faithful Rust
/// equivalent for a dynamic `import()` of a db-supplied module path followed by an awaited AI
/// completion call — see module doc. Always returns the deferred marker.
fn deferred_adapter_call_error(adapter_module: &str) -> String {
    format!(
        "PARITY-DEFERRED: live adapter dispatch not portable to a native test (dynamic import of '{adapter_module}' + an awaited real AI completion call)"
    )
}

pub fn providerTestCall(input: &Value, db: &Connection) -> Result<Value, String> {
    let provider_id = input.as_object().and_then(|o| o.get("provider_id")).cloned();
    let provider_id = match provider_id {
        None | Some(Value::Null) => {
            return Err("provider-test-call: provider_id is required".to_string());
        }
        Some(v) => v,
    };

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, tier, provider, model_id, adapter_module FROM brain.ai_providers WHERE id = ?",
        &[provider_id.clone()],
    )?;
    let provider = rows.as_array().and_then(|a| a.first());

    let provider = match provider {
        None => {
            let mut o = Map::new();
            o.insert("ok".into(), json!(false));
            o.insert(
                "error".into(),
                json!(format!("provider id={} not found", tmpl(&provider_id))),
            );
            o.insert("provider_id".into(), provider_id);
            return Ok(Value::Object(o));
        }
        Some(p) => p,
    };

    let adapter_module = provider.get("adapter_module").and_then(|v| v.as_str());
    let adapter_module = match adapter_module {
        None => {
            let mut o = Map::new();
            o.insert("ok".into(), json!(false));
            o.insert("error".into(), json!("provider has no adapter_module set"));
            o.insert("provider".into(), provider.clone());
            return Ok(Value::Object(o));
        }
        Some(m) => m,
    };

    let provider_name = provider.get("provider").cloned().unwrap_or(Value::Null);
    let model_id = provider.get("model_id").cloned().unwrap_or(Value::Null);

    let t0 = Instant::now();
    let latency_ms = (t0.elapsed().as_secs_f64() * 1000.0 * 100.0).round() / 100.0;

    let mut o = Map::new();
    o.insert("ok".into(), json!(false));
    o.insert("latency_ms".into(), json!(latency_ms));
    o.insert("error".into(), json!(deferred_adapter_call_error(adapter_module)));
    o.insert("provider".into(), provider_name);
    o.insert("model_id".into(), model_id);
    Ok(Value::Object(o))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "CREATE TABLE brain.ai_providers ( \
        id INTEGER PRIMARY KEY AUTOINCREMENT, \
        tier TEXT NOT NULL, \
        provider TEXT NOT NULL, \
        model_id TEXT, \
        adapter_module TEXT \
    )";

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    fn seed(c: &Connection, id: i64, tier: &str, provider: &str, model_id: Option<&str>, adapter_module: Option<&str>) {
        c.execute(
            "INSERT INTO brain.ai_providers (id, tier, provider, model_id, adapter_module) VALUES (?, ?, ?, ?, ?)",
            rusqlite::params![id, tier, provider, model_id, adapter_module],
        )
        .unwrap();
    }

    #[test]
    fn missing_provider_id_returns_exact_required_message() {
        let c = db0();
        let e = providerTestCall(&json!({}), &c).unwrap_err();
        assert_eq!(e, "provider-test-call: provider_id is required");
        let e2 = providerTestCall(&json!({ "provider_id": null }), &c).unwrap_err();
        assert_eq!(e2, "provider-test-call: provider_id is required");
    }

    #[test]
    fn zero_provider_id_is_a_valid_id_not_treated_as_missing() {
        // JS `provider_id == null` is false for 0 — this must reach the db lookup, not the guard.
        let c = db0();
        let r = providerTestCall(&json!({ "provider_id": 0 }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("provider id=0 not found"));
        assert_eq!(r["provider_id"], json!(0));
    }

    #[test]
    fn unknown_provider_id_reports_the_exact_js_error_and_echoes_provider_id() {
        let c = db0();
        let r = providerTestCall(&json!({ "provider_id": 999 }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "ok": false, "error": "provider id=999 not found", "provider_id": 999 })
        );
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["ok", "error", "provider_id"]);
    }

    #[test]
    fn provider_without_adapter_module_reports_the_exact_js_error_and_echoes_full_row() {
        let c = db0();
        seed(&c, 1, "mid", "openrouter", Some("anthropic/claude-3.5"), None);
        let r = providerTestCall(&json!({ "provider_id": 1 }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("provider has no adapter_module set"));
        assert_eq!(r["provider"]["id"], json!(1));
        assert_eq!(r["provider"]["tier"], json!("mid"));
        assert_eq!(r["provider"]["provider"], json!("openrouter"));
        assert_eq!(r["provider"]["model_id"], json!("anthropic/claude-3.5"));
        assert!(r["provider"]["adapter_module"].is_null());
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["ok", "error", "provider"]);
    }

    #[test]
    fn resolved_provider_hits_the_documented_deferred_boundary() {
        let c = db0();
        seed(&c, 7, "high", "openrouter", Some("openai/gpt-5"), Some("skills/external/openrouter-call.mjs"));
        let r = providerTestCall(&json!({ "provider_id": 7 }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["latency_ms"].as_f64().is_some());
        let err = r["error"].as_str().unwrap();
        assert!(err.starts_with("PARITY-DEFERRED:"), "{err}");
        assert!(err.contains("skills/external/openrouter-call.mjs"), "{err}");
        assert_eq!(r["provider"], json!("openrouter"));
        assert_eq!(r["model_id"], json!("openai/gpt-5"));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["ok", "latency_ms", "error", "provider", "model_id"]);
    }
}
