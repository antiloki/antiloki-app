//! byok-local-model-add.rs — twin of skills/system/byok-local-model-add.mjs (db WRITE lane,
//! attached `brain` db). NAMES ARE CONTRACT: `byokLocalModelAdd` (default). Upserts a LOCAL
//! Ollama model into brain.ai_providers (wiring the ollama-call adapter) and activates it on
//! (tier, rank): vacate the rank, dedupe the model off other ranks, activate. Guard throws
//! (bad tier / empty model_id / rank out of range) → Err, unfixtured. The JS computes a
//! savepoint name from Date.now() — frozen-clock lane pins it to 1767225600000; the harness
//! hands a fresh connection so BEGIN IMMEDIATE succeeds and the savepoint path never fires.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

const TIERS: [&str; 3] = ["low", "mid", "high"];
const PROVIDER: &str = "ollama";
const ADAPTER: &str = "antiloki-core/skills/external/ollama-call.mjs";
// JS: const sp = `byok_local_${Date.now()}` — crate::clock decides wall-clock vs the pinned instant.
fn sp() -> String { format!("byok_local_{}", crate::clock::now_ms()) }

// = JS `${v}` (template-literal interpolation; only feeds the tier guard message).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// = JS `typeof v` for JSON-representable values (feeds the model_id guard message).
fn type_of(v: Option<&Value>) -> &'static str {
    match v {
        None => "undefined",
        Some(Value::Null) | Some(Value::Array(_)) | Some(Value::Object(_)) => "object",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
    }
}

pub fn byokLocalModelAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    // The `if (!db) throw err.contextMissing(...)` guard cannot fire: the db lane always
    // hands the twin a live connection.
    let tier_v = input.get("tier");
    let tier = tier_v.and_then(|v| v.as_str());
    // JS: typeof input?.model_id === 'string' ? input.model_id.trim() : ''
    let model_id = match input.get("model_id") {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    };
    // JS: Number.isInteger(input?.rank) ? input.rank : 0
    let rank = input.get("rank").and_then(|v| v.as_i64()).unwrap_or(0);

    if !tier.map_or(false, |t| TIERS.contains(&t)) {
        return Err(format!(
            "byok-local-model-add: tier must be one of: low, mid, high (got '{}')",
            tmpl(tier_v)
        ));
    }
    let tier = tier.unwrap();
    if model_id.is_empty() {
        return Err(format!(
            "byok-local-model-add: model_id must be non-empty Ollama tag (e.g. deepseek-r1:8b) (got {})",
            type_of(input.get("model_id"))
        ));
    }
    if rank < 0 || rank > 2 {
        return Err(format!(
            "byok-local-model-add: rank must be one of: 0, 1, 2 (got '{}')",
            rank
        ));
    }

    // JS: try BEGIN IMMEDIATE, on failure (already inside a transaction) nest via SAVEPOINT.
    // Bound ONCE: SAVEPOINT/RELEASE/ROLLBACK must name the same savepoint, and off the frozen
    // clock two sp() calls can straddle a millisecond.
    let sp = sp();
    let nested = db.execute_batch("BEGIN IMMEDIATE").is_err();
    if nested {
        db.execute_batch(&format!("SAVEPOINT {sp}")).map_err(|e| e.to_string())?;
    }
    let body = (|| -> Result<(), String> {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT id FROM brain.ai_providers WHERE tier=? AND provider=? AND model_id=?",
            &[json!(tier), json!(PROVIDER), json!(model_id)],
        )?;
        let existing = rows.as_array().and_then(|a| a.first()).cloned();
        match existing {
            None => {
                crate::db_json::run_write(
                    db,
                    "\n        INSERT INTO brain.ai_providers (tier, provider, model_id, active, failover_rank, adapter_module, notes_md)\n        VALUES (?, ?, ?, 0, ?, ?, 'local Ollama model \u{2014} added via Settings \u{2192} Local AI')\n      ",
                    &[json!(tier), json!(PROVIDER), json!(model_id), json!(rank), json!(ADAPTER)],
                )?;
            }
            Some(row) => {
                // keep the adapter pointer correct on a pre-existing row.
                crate::db_json::run_write(
                    db,
                    "UPDATE brain.ai_providers SET adapter_module=? WHERE id=?",
                    &[json!(ADAPTER), row.get("id").cloned().unwrap_or(Value::Null)],
                )?;
            }
        }
        // activate on (tier, rank): vacate the rank, dedupe the model off other ranks, activate.
        crate::db_json::run_write(
            db,
            "UPDATE brain.ai_providers SET active=0 WHERE tier=? AND active=1 AND failover_rank=?",
            &[json!(tier), json!(rank)],
        )?;
        crate::db_json::run_write(
            db,
            "UPDATE brain.ai_providers SET active=0 WHERE tier=? AND provider=? AND model_id=?",
            &[json!(tier), json!(PROVIDER), json!(model_id)],
        )?;
        crate::db_json::run_write(
            db,
            "UPDATE brain.ai_providers SET active=1, failover_rank=? WHERE tier=? AND provider=? AND model_id=?",
            &[json!(rank), json!(tier), json!(PROVIDER), json!(model_id)],
        )?;
        Ok(())
    })();
    match body {
        Ok(()) => {
            if nested {
                db.execute_batch(&format!("RELEASE SAVEPOINT {sp}")).map_err(|e| e.to_string())?;
            } else {
                db.execute_batch("COMMIT").map_err(|e| e.to_string())?;
            }
        }
        Err(e) => {
            let _ = if nested {
                db.execute_batch(&format!("ROLLBACK TO SAVEPOINT {sp}"))
            } else {
                db.execute_batch("ROLLBACK")
            };
            return Err(e);
        }
    }

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("tier".into(), json!(tier));
    out.insert("rank".into(), json!(rank));
    out.insert("provider".into(), json!(PROVIDER));
    out.insert("model_id".into(), json!(model_id));
    out.insert("adapter_module".into(), json!(ADAPTER));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "CREATE TABLE brain.ai_providers ( id INTEGER PRIMARY KEY AUTOINCREMENT, tier TEXT NOT NULL CHECK (tier IN ('low','mid','high','image')), provider TEXT NOT NULL, model_id TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1, failover_rank INTEGER NOT NULL DEFAULT 0, adapter_module TEXT, notes_md TEXT )";

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }
    fn seed(c: &Connection, id: i64, tier: &str, provider: &str, model_id: &str, active: i64, rank: i64, adapter: &str) {
        c.execute(
            "INSERT INTO brain.ai_providers (id, tier, provider, model_id, active, failover_rank, adapter_module) VALUES (?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![id, tier, provider, model_id, active, rank, adapter],
        )
        .unwrap();
    }
    fn row(c: &Connection, where_sql: &str) -> (i64, i64, String) {
        c.query_row(
            &format!("SELECT active, failover_rank, adapter_module FROM brain.ai_providers WHERE {where_sql}"),
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<String>>(2)?.unwrap_or_default())),
        )
        .unwrap()
    }

    #[test]
    fn registers_wires_adapter_and_activates() {
        let c = db0();
        let r = byokLocalModelAdd(&json!({ "tier": "low", "model_id": "deepseek-r1:8b" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "ok": true, "tier": "low", "rank": 0, "provider": "ollama", "model_id": "deepseek-r1:8b", "adapter_module": ADAPTER })
        );
        // key order is the JS object-literal order
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["ok", "tier", "rank", "provider", "model_id", "adapter_module"]);
        let (active, rank, adapter) = row(&c, "tier='low' AND provider='ollama' AND model_id='deepseek-r1:8b'");
        assert_eq!((active, rank), (1, 0));
        assert_eq!(adapter, ADAPTER);
    }

    #[test]
    fn activating_deactivates_previous_primary_and_updates_stale_adapter() {
        let c = db0();
        seed(&c, 4, "mid", "anthropic", "claude-haiku-4-5", 1, 1, "antiloki-core/skills/external/anthropic-call.mjs");
        seed(&c, 5, "mid", "ollama", "qwen2.5-coder:32b", 0, 2, "stale/ollama-path.mjs");
        byokLocalModelAdd(&json!({ "tier": "mid", "model_id": "qwen2.5-coder:32b", "rank": 1 }), &c).unwrap();
        let (prev_active, _, _) = row(&c, "id=4");
        assert_eq!(prev_active, 0); // previous holder of (mid, rank 1) vacated
        let (active, rank, adapter) = row(&c, "id=5");
        assert_eq!((active, rank), (1, 1));
        assert_eq!(adapter, ADAPTER); // stale adapter pointer corrected
    }

    #[test]
    fn idempotent_readd_keeps_one_row_and_trims_model_id() {
        let c = db0();
        let r1 = byokLocalModelAdd(&json!({ "tier": "high", "model_id": "  llama3.1:70b  ", "rank": 2 }), &c).unwrap();
        assert_eq!(r1["model_id"], json!("llama3.1:70b")); // trimmed
        byokLocalModelAdd(&json!({ "tier": "high", "model_id": "llama3.1:70b", "rank": 2 }), &c).unwrap();
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.ai_providers WHERE tier='high' AND provider='ollama' AND model_id='llama3.1:70b'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
        let (active, rank, _) = row(&c, "model_id='llama3.1:70b'");
        assert_eq!((active, rank), (1, 2));
    }

    #[test]
    fn readd_at_new_rank_moves_the_active_row() {
        let c = db0();
        seed(&c, 7, "low", "ollama", "phi4:14b", 1, 0, ADAPTER);
        let r = byokLocalModelAdd(&json!({ "tier": "low", "model_id": "phi4:14b", "rank": 1 }), &c).unwrap();
        assert_eq!(r["rank"], json!(1));
        let (active, rank, _) = row(&c, "id=7");
        assert_eq!((active, rank), (1, 1)); // deduped off rank 0, active on rank 1
    }

    #[test]
    fn guards_reject_bad_input_and_never_write() {
        let c = db0();
        let e = byokLocalModelAdd(&json!({ "tier": "ultra", "model_id": "x" }), &c).unwrap_err();
        assert!(e.contains("tier must be one of: low, mid, high (got 'ultra')"));
        let e = byokLocalModelAdd(&json!({ "tier": "low" }), &c).unwrap_err();
        assert!(e.contains("model_id must be") && e.contains("(got undefined)"));
        let e = byokLocalModelAdd(&json!({ "tier": "low", "model_id": "   " }), &c).unwrap_err();
        assert!(e.contains("(got string)")); // whitespace-only trims to empty
        let e = byokLocalModelAdd(&json!({ "tier": "low", "model_id": "x", "rank": 3 }), &c).unwrap_err();
        assert!(e.contains("rank must be one of: 0, 1, 2 (got '3')"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.ai_providers", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0); // guard paths never write
    }
}
