//! byok-strategy-set.rs — twin of skills/system/byok-strategy-set.mjs (db WRITE lane, attached
//! `brain` db). NAMES ARE CONTRACT: `byokStrategySet` (default). Flips every tier's active
//! provider to the chosen strategy's target (byok → openrouter): a tier with no row for the
//! target provider is left untouched and reported in `skipped`, never stranded with zero active
//! providers. Guard throw (bad strategy) → Err, unfixtured. The JS computes a savepoint name from
//! Date.now() — frozen-clock lane pins it to 1767225600000; the harness hands a fresh connection
//! so BEGIN IMMEDIATE succeeds and the savepoint path never fires.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

const TIERS: [&str; 3] = ["low", "mid", "high"];
// JS: const sp = `byok_strategy_${Date.now()}` — crate::clock decides wall-clock vs the pinned instant.
fn sp() -> String { format!("byok_strategy_{}", crate::clock::now_ms()) }

// = JS `${v}` (template-literal interpolation; feeds the guard message only).
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

pub fn byokStrategySet(input: &Value, db: &Connection) -> Result<Value, String> {
    // The `if (!db) throw err.contextMissing(...)` guard cannot fire: the db lane always
    // hands the twin a live connection.
    let strategy_v = input.get("strategy");
    let strategy = strategy_v.and_then(|v| v.as_str());
    // JS: TARGET_PROVIDER = { byok: 'openrouter' } — the only key.
    let target = match strategy {
        Some("byok") => "openrouter",
        _ => {
            return Err(format!(
                "byok-strategy-set: strategy must be one of: byok (got '{}')",
                tmpl(strategy_v)
            ));
        }
    };
    let strategy = strategy.unwrap();

    let mut skipped: Vec<String> = Vec::new();

    // JS: try BEGIN IMMEDIATE, on failure (already inside a transaction) nest via SAVEPOINT.
    // Bound ONCE: SAVEPOINT/RELEASE/ROLLBACK must name the same savepoint, and off the frozen
    // clock two sp() calls can straddle a millisecond.
    let sp = sp();
    let nested = db.execute_batch("BEGIN IMMEDIATE").is_err();
    if nested {
        db.execute_batch(&format!("SAVEPOINT {sp}")).map_err(|e| e.to_string())?;
    }
    let body = (|| -> Result<(), String> {
        for t in TIERS {
            let count_rows = crate::db_json::query_rows(
                db,
                "SELECT COUNT(*) AS c FROM brain.ai_providers WHERE tier = ? AND provider = ?",
                &[json!(t), json!(target)],
            )?;
            let c = count_rows
                .as_array()
                .and_then(|a| a.first())
                .and_then(|r| r.get("c"))
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            if c == 0 {
                skipped.push(t.to_string());
                continue;
            }
            crate::db_json::run_write(db, "UPDATE brain.ai_providers SET active = 0 WHERE tier = ?", &[json!(t)])?;
            crate::db_json::run_write(
                db,
                "UPDATE brain.ai_providers SET active = 1 WHERE tier = ? AND provider = ?",
                &[json!(t), json!(target)],
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

    let mut active = Map::new();
    for t in TIERS {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT provider FROM brain.ai_providers WHERE tier = ? AND active = 1 ORDER BY id LIMIT 1",
            &[json!(t)],
        )?;
        let provider = rows
            .as_array()
            .and_then(|a| a.first())
            .and_then(|r| r.get("provider"))
            .cloned()
            .unwrap_or(Value::Null);
        active.insert(t.to_string(), provider);
    }

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("strategy".into(), json!(strategy));
    out.insert("active".into(), Value::Object(active));
    out.insert("skipped".into(), json!(skipped));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "CREATE TABLE brain.ai_providers ( id INTEGER PRIMARY KEY AUTOINCREMENT, tier TEXT NOT NULL, provider TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1 )";

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }
    fn seed(c: &Connection, id: i64, tier: &str, provider: &str, active: i64) {
        c.execute(
            "INSERT INTO brain.ai_providers (id, tier, provider, active) VALUES (?, ?, ?, ?)",
            rusqlite::params![id, tier, provider, active],
        )
        .unwrap();
    }
    fn active_provider(c: &Connection, tier: &str) -> Option<String> {
        c.query_row(
            "SELECT provider FROM brain.ai_providers WHERE tier = ? AND active = 1 ORDER BY id LIMIT 1",
            [tier],
            |r| r.get(0),
        )
        .ok()
    }

    #[test]
    fn flips_tiers_with_a_target_row_and_skips_the_rest() {
        let c = db0();
        seed(&c, 1, "low", "anthropic", 1);
        seed(&c, 2, "low", "openrouter", 0);
        seed(&c, 3, "mid", "anthropic", 1); // no openrouter row for mid → skipped
        seed(&c, 4, "high", "openrouter", 0);

        let r = byokStrategySet(&json!({ "strategy": "byok" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "ok": true, "strategy": "byok", "active": { "low": "openrouter", "mid": "anthropic", "high": "openrouter" }, "skipped": ["mid"] })
        );
        // key order is the JS object-literal order
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["ok", "strategy", "active", "skipped"]);
        assert_eq!(active_provider(&c, "low").as_deref(), Some("openrouter"));
        assert_eq!(active_provider(&c, "mid").as_deref(), Some("anthropic")); // untouched
        assert_eq!(active_provider(&c, "high").as_deref(), Some("openrouter"));
    }

    #[test]
    fn a_tier_with_zero_rows_is_skipped_and_reports_active_null() {
        let c = db0();
        seed(&c, 10, "low", "openrouter", 0);
        seed(&c, 11, "mid", "openrouter", 0);
        // no rows at all for "high"

        let r = byokStrategySet(&json!({ "strategy": "byok" }), &c).unwrap();
        assert_eq!(r["skipped"], json!(["high"]));
        assert_eq!(r["active"], json!({ "low": "openrouter", "mid": "openrouter", "high": Value::Null }));
    }

    #[test]
    fn rejects_an_unknown_strategy_and_never_writes() {
        let c = db0();
        seed(&c, 1, "low", "anthropic", 1);
        let e = byokStrategySet(&json!({ "strategy": "subscription" }), &c).unwrap_err();
        assert!(e.contains("strategy must be one of: byok (got 'subscription')"));
        assert_eq!(active_provider(&c, "low").as_deref(), Some("anthropic")); // guard path never writes
    }
}
