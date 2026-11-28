//! byok-test.rs — twin of skills/system/byok-test.mjs (db READ lane, attached `brain` db).
//! NAMES ARE CONTRACT: `byokTest` (default export).
//!
//! For each requested tier this skill resolves the tier's active provider row from
//! `brain.ai_providers` (deterministic, real db read — ported below), then `import()`s the
//! provider's adapter module and makes ONE live network completion call ("reply pong") to
//! measure ok/latency/sample. That adapter dispatch — a dynamic `import()` of an arbitrary
//! module path drawn from a db column, followed by an awaited real HTTP round-trip — has no
//! faithful Rust equivalent (same class as ai-call.rs's `aiCall`: genuinely non-deterministic,
//! non-portable, not fixturable). It is the PARITY-DEFERRED boundary.
//!
//! Everything deterministic around that boundary is ported faithfully: the per-tier list
//! resolution (`input.tier` singular vs the `['low','mid','high']` default), the `brain.ai_providers`
//! lookup (`WHERE tier = ? AND active = 1 ORDER BY failover_rank ASC, id ASC LIMIT 1`), and both
//! guard branches that never reach the network (no active provider row; provider row with no
//! `adapter_module`) — these produce the EXACT same `{tier, ok, error}` / `{tier, provider,
//! model_id, ok, error}` shapes and error strings as the JS. Only once a provider AND its
//! adapter_module are both resolved does the twin hit the deferred boundary, reported as a
//! `{tier, provider, model_id, ok:false, error:"PARITY-DEFERRED: ..."}` entry instead of the
//! real network attempt.
//!
//! BESPOKE NATIVE TESTING only (no fixture/frontier — dynamic adapter import + a real HTTP call
//! sit on both success paths, so there is nothing conform-db could freeze). Proven below via
//! `#[cfg(test)]` against a real in-memory db with `brain.ai_providers` attached, per the
//! `db0()`/`seed()` convention in byok-strategy-set.rs.
//!
//! The `if (!db) throw err.contextMissing('byok-test', 'db')` guard cannot fire here — the db
//! lane always hands the twin a live connection (same note as byok-strategy-set.rs).

use rusqlite::Connection;
use serde_json::{json, Map, Value};

/// Mirrors the JS `resolveAdapter` + live-call step. There is no faithful Rust equivalent for a
/// dynamic `import()` of a db-supplied module path followed by an awaited HTTP round-trip — see
/// module doc. Always returns the deferred marker; the caller folds it into the tier's result.
fn deferred_adapter_call_error(adapter_module: &str) -> String {
    format!(
        "PARITY-DEFERRED: live adapter dispatch not portable to a native test (dynamic import of '{adapter_module}' + an awaited real HTTP completion call)"
    )
}

pub fn byokTest(input: &Value, db: &Connection) -> Result<Value, String> {
    let one = input.as_object().and_then(|o| o.get("tier")).and_then(Value::as_str);
    let tiers: Vec<&str> = match one {
        Some(t) => vec![t],
        None => vec!["low", "mid", "high"],
    };

    let mut results: Vec<Value> = Vec::new();
    for tier in tiers {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT provider, model_id, adapter_module FROM brain.ai_providers \
             WHERE tier = ? AND active = 1 ORDER BY failover_rank ASC, id ASC LIMIT 1",
            &[json!(tier)],
        )?;
        let prov = rows.as_array().and_then(|a| a.first());

        let prov = match prov {
            None => {
                let mut o = Map::new();
                o.insert("tier".into(), json!(tier));
                o.insert("ok".into(), json!(false));
                o.insert(
                    "error".into(),
                    json!("no active model set for this tier (Cost screen → tier routing)"),
                );
                results.push(Value::Object(o));
                continue;
            }
            Some(p) => p,
        };

        let provider = prov.get("provider").cloned().unwrap_or(Value::Null);
        let model_id = prov.get("model_id").cloned().unwrap_or(Value::Null);
        let adapter_module = prov.get("adapter_module").and_then(|v| v.as_str());

        let adapter_module = match adapter_module {
            None => {
                let mut o = Map::new();
                o.insert("tier".into(), json!(tier));
                o.insert("provider".into(), provider);
                o.insert("model_id".into(), model_id);
                o.insert("ok".into(), json!(false));
                o.insert("error".into(), json!("provider has no adapter_module"));
                results.push(Value::Object(o));
                continue;
            }
            Some(m) => m,
        };

        let mut o = Map::new();
        o.insert("tier".into(), json!(tier));
        o.insert("provider".into(), provider);
        o.insert("model_id".into(), model_id);
        o.insert("ok".into(), json!(false));
        o.insert("error".into(), json!(deferred_adapter_call_error(adapter_module)));
        results.push(Value::Object(o));
    }

    Ok(json!({ "results": results }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "CREATE TABLE brain.ai_providers ( \
        id INTEGER PRIMARY KEY AUTOINCREMENT, \
        tier TEXT NOT NULL, \
        provider TEXT NOT NULL, \
        model_id TEXT, \
        adapter_module TEXT, \
        active INTEGER NOT NULL DEFAULT 1, \
        failover_rank INTEGER NOT NULL DEFAULT 0 \
    )";

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    fn seed(
        c: &Connection,
        id: i64,
        tier: &str,
        provider: &str,
        model_id: Option<&str>,
        adapter_module: Option<&str>,
        active: i64,
        failover_rank: i64,
    ) {
        c.execute(
            "INSERT INTO brain.ai_providers (id, tier, provider, model_id, adapter_module, active, failover_rank) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![id, tier, provider, model_id, adapter_module, active, failover_rank],
        )
        .unwrap();
    }

    #[test]
    fn no_active_provider_reports_the_exact_js_error() {
        let c = db0();
        let r = byokTest(&json!({ "tier": "low" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "results": [ { "tier": "low", "ok": false, "error": "no active model set for this tier (Cost screen → tier routing)" } ] })
        );
        let keys: Vec<&String> = r["results"][0].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["tier", "ok", "error"]);
    }

    #[test]
    fn provider_without_adapter_module_reports_the_exact_js_error() {
        let c = db0();
        seed(&c, 1, "mid", "openrouter", Some("anthropic/claude-3.5"), None, 1, 0);
        let r = byokTest(&json!({ "tier": "mid" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "results": [ { "tier": "mid", "provider": "openrouter", "model_id": "anthropic/claude-3.5", "ok": false, "error": "provider has no adapter_module" } ] })
        );
        let keys: Vec<&String> = r["results"][0].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["tier", "provider", "model_id", "ok", "error"]);
    }

    #[test]
    fn resolved_provider_hits_the_documented_deferred_boundary() {
        let c = db0();
        seed(&c, 1, "high", "openrouter", Some("openai/gpt-5"), Some("skills/external/openrouter-call.mjs"), 1, 0);
        let r = byokTest(&json!({ "tier": "high" }), &c).unwrap();
        let row = &r["results"][0];
        assert_eq!(row["tier"], json!("high"));
        assert_eq!(row["provider"], json!("openrouter"));
        assert_eq!(row["model_id"], json!("openai/gpt-5"));
        assert_eq!(row["ok"], json!(false));
        let err = row["error"].as_str().unwrap();
        assert!(err.starts_with("PARITY-DEFERRED:"));
        assert!(err.contains("skills/external/openrouter-call.mjs"));
    }

    #[test]
    fn no_tier_input_defaults_to_all_three_in_order_and_picks_lowest_failover_rank() {
        let c = db0();
        // two rows for "low": rank 1 and rank 0 — the lowest rank wins.
        seed(&c, 1, "low", "openrouter", Some("m1"), None, 1, 1);
        seed(&c, 2, "low", "ollama", Some("m2"), None, 1, 0);
        let r = byokTest(&json!({}), &c).unwrap();
        let results = r["results"].as_array().unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0]["tier"], json!("low"));
        assert_eq!(results[0]["provider"], json!("ollama")); // failover_rank 0 wins over 1
        assert_eq!(results[1]["tier"], json!("mid"));
        assert_eq!(results[1]["ok"], json!(false));
        assert_eq!(results[2]["tier"], json!("high"));
        assert_eq!(results[2]["ok"], json!(false));
    }
}
