//! god-ai-provider-set-active.rs — twin of skills/agents/god/god-ai-provider-set-active.mjs
//! (db WRITE lane, attached `brain` db). NAMES ARE CONTRACT: `godAiProviderSetActive` (default).
//! Activates ONE provider within its tier: deactivate all rows in the target's tier, then
//! activate the target. Guard throws (missing provider_id / not found) → Err, unfixtured.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS `!!v` truthiness — used for `was_active: !!target.active` (INTEGER 0/1 → bool).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS `${v}` (template-literal interpolation; only feeds the not-found error message).
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

pub fn godAiProviderSetActive(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { provider_id } = input ?? {}` then `if (provider_id == null) throw` —
    // loose `== null` rejects ONLY undefined/null (0 and '' pass the guard).
    let provider_id = match input.get("provider_id") {
        None | Some(Value::Null) => {
            return Err("god-ai-provider-set-active: provider_id is required".into())
        }
        Some(v) => v.clone(),
    };
    // The `if (!db) throw err.contextMissing(...)` guard cannot fire here: the db lane
    // always hands the twin a live connection.

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, tier, provider, model_id, active FROM brain.ai_providers WHERE id = ?",
        &[provider_id.clone()],
    )?;
    let target = match rows.as_array().and_then(|a| a.first()).cloned() {
        Some(t) => t,
        None => {
            return Err(format!(
                "god-ai-provider-set-active: provider id={} not found",
                tmpl(Some(&provider_id))
            ))
        }
    };
    let tier = target.get("tier").cloned().unwrap_or(Value::Null);

    // Deactivate all in this tier (idempotent), then activate the target.
    crate::db_json::run_write(db, "UPDATE brain.ai_providers SET active = 0 WHERE tier = ?", &[tier.clone()])?;
    crate::db_json::run_write(db, "UPDATE brain.ai_providers SET active = 1 WHERE id = ?", &[provider_id.clone()])?;

    let mut out = Map::new();
    out.insert("provider_id".into(), provider_id);
    out.insert("tier".into(), tier);
    out.insert("provider".into(), target.get("provider").cloned().unwrap_or(Value::Null));
    out.insert("model_id".into(), target.get("model_id").cloned().unwrap_or(Value::Null));
    out.insert("was_active".into(), json!(truthy(target.get("active"))));
    out.insert("is_active".into(), json!(true));
    out.insert("deactivated_in_tier".into(), json!(true));
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
    fn seed(c: &Connection, id: i64, tier: &str, provider: &str, model_id: &str, active: i64) {
        c.execute(
            "INSERT INTO brain.ai_providers (id, tier, provider, model_id, active) VALUES (?, ?, ?, ?, ?)",
            rusqlite::params![id, tier, provider, model_id, active],
        )
        .unwrap();
    }
    fn active_of(c: &Connection, id: i64) -> i64 {
        c.query_row("SELECT active FROM brain.ai_providers WHERE id = ?", [id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn switches_active_within_tier_only() {
        let c = db0();
        seed(&c, 1, "mid", "openrouter", "anthropic/claude-sonnet-4.6", 1);
        seed(&c, 2, "mid", "anthropic", "claude-sonnet-4-6", 0);
        seed(&c, 3, "low", "openrouter", "qwen/qwen-2.5-coder-32b-instruct", 1);
        let r = godAiProviderSetActive(&json!({ "provider_id": 2 }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "provider_id": 2, "tier": "mid", "provider": "anthropic", "model_id": "claude-sonnet-4-6", "was_active": false, "is_active": true, "deactivated_in_tier": true })
        );
        // key order is the JS object-literal order
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["provider_id", "tier", "provider", "model_id", "was_active", "is_active", "deactivated_in_tier"]);
        assert_eq!(active_of(&c, 1), 0); // tier sibling deactivated
        assert_eq!(active_of(&c, 2), 1); // target activated
        assert_eq!(active_of(&c, 3), 1); // other tier untouched
    }

    #[test]
    fn already_active_is_idempotent_and_reports_was_active() {
        let c = db0();
        seed(&c, 10, "high", "openrouter", "openai/gpt-5", 1);
        seed(&c, 11, "high", "anthropic", "claude-opus-4-7", 0);
        let r = godAiProviderSetActive(&json!({ "provider_id": 10 }), &c).unwrap();
        assert_eq!(r["was_active"], json!(true));
        assert_eq!(r["is_active"], json!(true));
        assert_eq!(active_of(&c, 10), 1);
        assert_eq!(active_of(&c, 11), 0);
    }

    #[test]
    fn lone_inactive_provider_becomes_active() {
        let c = db0();
        seed(&c, 20, "image", "openrouter", "google/gemini-image", 0);
        let r = godAiProviderSetActive(&json!({ "provider_id": 20 }), &c).unwrap();
        assert_eq!(r["was_active"], json!(false));
        assert_eq!(active_of(&c, 20), 1);
    }

    #[test]
    fn guards_reject_bad_input() {
        let c = db0();
        seed(&c, 30, "low", "anthropic", "claude-haiku-4-5", 0);
        let e = godAiProviderSetActive(&json!({}), &c).unwrap_err();
        assert!(e.contains("provider_id is required"));
        let e = godAiProviderSetActive(&json!({ "provider_id": null }), &c).unwrap_err();
        assert!(e.contains("provider_id is required"));
        let e = godAiProviderSetActive(&json!({ "provider_id": 9999 }), &c).unwrap_err();
        assert!(e.contains("provider id=9999 not found"));
        assert_eq!(active_of(&c, 30), 0); // guard paths never write
    }
}
