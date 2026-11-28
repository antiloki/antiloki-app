//! byok-model-set.rs — twin of skills/system/byok-model-set.mjs (db WRITE lane, attached
//! `brain` db). NAMES ARE CONTRACT: `byokModelSet` (default). Assigns (provider, model_id) to
//! (tier, rank) among already-seeded brain.ai_providers rows: vacate the target rank, dedupe
//! the model off any other rank, then activate — UNLESS model_id is empty, in which case the
//! slot is simply CLEARED (freeSlot only; rank 0 can never be cleared, guarded below). Guard
//! throws (bad tier/rank, clearing rank 0, unseeded model) → Err, unfixtured. The JS computes a
//! savepoint name from Date.now() — frozen-clock lane pins it to 1767225600000; the harness
//! hands a fresh connection so BEGIN IMMEDIATE succeeds and the savepoint path never fires.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

const TIERS: [&str; 4] = ["low", "mid", "high", "image"];
const MAX_RANK: i64 = 9;
// JS: const sp = `byok_model_${Date.now()}` — crate::clock decides wall-clock vs the pinned instant.
fn sp() -> String { format!("byok_model_{}", crate::clock::now_ms()) }

// = JS `${v}` (template-literal interpolation; feeds guard messages only).
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

pub fn byokModelSet(input: &Value, db: &Connection) -> Result<Value, String> {
    // The `if (!db) throw err.contextMissing(...)` guard cannot fire: the db lane always
    // hands the twin a live connection.
    let tier_v = input.get("tier");
    let tier = tier_v.and_then(|v| v.as_str());
    // JS: (input?.provider ?? 'openrouter').trim()
    let provider = match input.get("provider").filter(|v| !v.is_null()) {
        None => "openrouter".to_string(),
        Some(Value::String(s)) => s.trim().to_string(),
        Some(v) => tmpl(Some(v)).trim().to_string(),
    };
    // JS: Number.isInteger(input?.rank) ? input.rank : 0
    let rank = input.get("rank").and_then(|v| v.as_i64()).unwrap_or(0);

    if !tier.map_or(false, |t| TIERS.contains(&t)) {
        return Err(format!(
            "byok-model-set: tier must be one of: low, mid, high, image (got '{}')",
            tmpl(tier_v)
        ));
    }
    let tier = tier.unwrap();
    if rank < 0 || rank > MAX_RANK {
        return Err(format!(
            "byok-model-set: rank must be one of: 0, 1, 2, 3, 4, 5, 6, 7, 8, 9 (got '{}')",
            rank
        ));
    }
    // JS: typeof input?.model_id === 'string' ? input.model_id.trim() : ''
    let model_id = match input.get("model_id") {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    };
    let clearing = model_id.is_empty();
    if clearing && rank == 0 {
        return Err(format!(
            "byok-model-set: model_id must be non-empty string (a tier primary cannot be cleared) (got {})",
            type_of(input.get("model_id"))
        ));
    }
    if !clearing {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT id FROM brain.ai_providers WHERE tier = ? AND provider = ? AND model_id = ?",
            &[json!(tier), json!(provider), json!(model_id)],
        )?;
        if rows.as_array().map_or(true, |a| a.is_empty()) {
            return Err(format!(
                "byok-model-set: ai_providers '{}/{}/{}' not found",
                tier, provider, model_id
            ));
        }
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
        // vacate the target rank
        crate::db_json::run_write(
            db,
            "UPDATE brain.ai_providers SET active = 0 WHERE tier = ? AND active = 1 AND failover_rank = ?",
            &[json!(tier), json!(rank)],
        )?;
        if !clearing {
            // never let one model occupy two ranks
            crate::db_json::run_write(
                db,
                "UPDATE brain.ai_providers SET active = 0 WHERE tier = ? AND provider = ? AND model_id = ?",
                &[json!(tier), json!(provider), json!(model_id)],
            )?;
            crate::db_json::run_write(
                db,
                "UPDATE brain.ai_providers SET active = 1, failover_rank = ? WHERE tier = ? AND provider = ? AND model_id = ?",
                &[json!(rank), json!(tier), json!(provider), json!(model_id)],
            )?;
        }
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
    out.insert("provider".into(), json!(provider));
    out.insert("model_id".into(), if clearing { Value::Null } else { json!(model_id) });
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "CREATE TABLE brain.ai_providers ( id INTEGER PRIMARY KEY AUTOINCREMENT, tier TEXT NOT NULL CHECK (tier IN ('low','mid','high','image')), provider TEXT NOT NULL, model_id TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1, failover_rank INTEGER NOT NULL DEFAULT 0 )";

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }
    fn seed(c: &Connection, id: i64, tier: &str, provider: &str, model_id: &str, active: i64, rank: i64) {
        c.execute(
            "INSERT INTO brain.ai_providers (id, tier, provider, model_id, active, failover_rank) VALUES (?, ?, ?, ?, ?, ?)",
            rusqlite::params![id, tier, provider, model_id, active, rank],
        )
        .unwrap();
    }
    fn row(c: &Connection, where_sql: &str) -> (i64, i64) {
        c.query_row(
            &format!("SELECT active, failover_rank FROM brain.ai_providers WHERE {where_sql}"),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    }

    #[test]
    fn activates_default_rank_and_vacates_previous_holder() {
        let c = db0();
        seed(&c, 1, "low", "openrouter", "qwen/qwen-2.5-7b", 0, 5);
        seed(&c, 2, "low", "anthropic", "claude-haiku-4-5", 1, 0);
        let r = byokModelSet(&json!({ "tier": "low", "model_id": "qwen/qwen-2.5-7b" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "ok": true, "tier": "low", "rank": 0, "provider": "openrouter", "model_id": "qwen/qwen-2.5-7b" })
        );
        // key order is the JS object-literal order
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["ok", "tier", "rank", "provider", "model_id"]);
        assert_eq!(row(&c, "id=1"), (1, 0)); // moved from rank 5 to rank 0, active
        assert_eq!(row(&c, "id=2"), (0, 0)); // previous rank-0 holder vacated
    }

    #[test]
    fn activating_at_nonzero_rank_dedupes_model_off_other_rank() {
        let c = db0();
        seed(&c, 10, "mid", "anthropic", "claude-haiku-4-5", 1, 1);
        seed(&c, 11, "mid", "openrouter", "qwen/qwen-2.5-coder", 1, 3);
        let r = byokModelSet(&json!({ "tier": "mid", "provider": "anthropic", "model_id": "claude-haiku-4-5", "rank": 3 }), &c).unwrap();
        assert_eq!(r["rank"], json!(3));
        assert_eq!(row(&c, "id=10"), (1, 3)); // deduped off rank 1, active on rank 3
        assert_eq!(row(&c, "id=11"), (0, 3)); // previous rank-3 holder vacated
    }

    #[test]
    fn clearing_a_nonzero_rank_only_vacates_and_returns_null_model_id() {
        let c = db0();
        seed(&c, 20, "high", "openrouter", "some-model", 1, 5);
        let r = byokModelSet(&json!({ "tier": "high", "rank": 5, "model_id": "" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "ok": true, "tier": "high", "rank": 5, "provider": "openrouter", "model_id": Value::Null })
        );
        assert_eq!(row(&c, "id=20"), (0, 5)); // vacated, not reactivated
    }

    #[test]
    fn image_tier_activates_default_rank() {
        let c = db0();
        seed(&c, 30, "image", "openrouter", "google/gemini-image", 0, 1);
        let r = byokModelSet(&json!({ "tier": "image", "model_id": "google/gemini-image" }), &c).unwrap();
        assert_eq!(r["tier"], json!("image"));
        assert_eq!(row(&c, "id=30"), (1, 0));
    }

    #[test]
    fn guards_reject_bad_input_and_never_write() {
        let c = db0();
        seed(&c, 40, "low", "openrouter", "keep-me", 1, 0);
        let e = byokModelSet(&json!({ "tier": "ultra", "model_id": "x" }), &c).unwrap_err();
        assert!(e.contains("tier must be one of: low, mid, high, image (got 'ultra')"));
        let e = byokModelSet(&json!({ "tier": "low", "model_id": "x", "rank": 10 }), &c).unwrap_err();
        assert!(e.contains("rank must be one of: 0, 1, 2, 3, 4, 5, 6, 7, 8, 9 (got '10')"));
        let e = byokModelSet(&json!({ "tier": "low", "model_id": "" }), &c).unwrap_err();
        assert!(e.contains("a tier primary cannot be cleared"));
        let e = byokModelSet(&json!({ "tier": "low", "model_id": "unseeded-model" }), &c).unwrap_err();
        assert!(e.contains("ai_providers 'low/openrouter/unseeded-model' not found"));
        assert_eq!(row(&c, "id=40"), (1, 0)); // guard paths never write
    }
}
