//! agent-grant.rs — twin of skills/entities/agent-grant.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `agentGrant` (default). Idempotent grant of an action to an agent; enforces child.grants ⊆ parent.
// QUALITY: 85/100 — agent/action existence, narrowing via grant-within-parent twin, idempotent hit, INSERT + SELECT * back
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS String(v) — used only in the (un-fixtured) not-found error interpolation.
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

pub fn agentGrant(input: &Value, db: &Connection) -> Result<Value, String> {
    // context.actor defaults to 'unknown'; the db-lane harness passes context = { db } only,
    // so actor is always 'unknown' (matches the JS `const { actor = 'unknown' } = context`).
    let actor = json!("unknown");

    let agent_name = input.get("agent_name").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&agent_name)) { return Err("agent-grant: agent_name is required".into()); }
    let action_name = input.get("action_name").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&action_name)) { return Err("agent-grant: action_name is required".into()); }

    // granted_by = actor (default fires only when the key is absent/undefined, NOT on null).
    let granted_by = match input.get("granted_by") { None => actor.clone(), Some(v) => v.clone() };
    // conditions = null (default fires only when the key is absent).
    let conditions = input.get("conditions").cloned().unwrap_or(Value::Null);

    // Verify agent + action exist (FK would catch it but we want a cleaner error).
    let agent_row = crate::db_json::query_rows(db, "SELECT 1 FROM agents WHERE name = ?", &[agent_name.clone()])?;
    if agent_row.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return Err(format!("agent-grant: agent '{}' not found", js_string(&agent_name)));
    }
    let skill_row = crate::db_json::query_rows(db, "SELECT 1 FROM actions WHERE name = ?", &[action_name.clone()])?;
    if skill_row.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return Err(format!("agent-grant: action '{}' not found", js_string(&action_name)));
    }

    // EPIC-SUBAGENT narrowing: child.grants ⊆ parent.grants (inert for root agents).
    let narrowing = crate::entities_agent_grants_check::grantWithinParent(
        db,
        agent_name.as_str().unwrap_or(""),
        action_name.as_str().unwrap_or(""),
    );
    if !truthy(narrowing.get("ok")) {
        let reason = narrowing.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        return Err(format!("agent-grant: {reason}"));
    }

    // Idempotent: return existing active grant if any (spread + idempotent:true).
    let existing = crate::db_json::query_rows(db,
        "SELECT * FROM agent_actions WHERE agent_name = ? AND action_name = ? AND revoked_at IS NULL",
        &[agent_name.clone(), action_name.clone()])?;
    if let Some(row) = existing.as_array().and_then(|a| a.first()) {
        let mut o = row.as_object().cloned().unwrap_or_default();
        o.insert("idempotent".into(), json!(true));
        return Ok(Value::Object(o));
    }

    // conditions == null ? null : JSON.stringify(conditions)
    let conditions_json = if conditions.is_null() { Value::Null } else { json!(serde_json::to_string(&conditions).map_err(|e| e.to_string())?) };

    let (_c, id) = crate::db_json::run_write(db,
        "INSERT INTO agent_actions (agent_name, action_name, granted_by, conditions_json) VALUES (?, ?, ?, ?)",
        &[agent_name.clone(), action_name.clone(), granted_by, conditions_json])?;
    let row = crate::db_json::query_rows(db, "SELECT * FROM agent_actions WHERE id = ?", &[json!(id)])?;
    Ok(row.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE agents (name TEXT PRIMARY KEY, parent_agent TEXT); \
             CREATE TABLE actions (name TEXT PRIMARY KEY); \
             CREATE TABLE agent_actions (id INTEGER PRIMARY KEY, agent_name TEXT NOT NULL, action_name TEXT NOT NULL, granted_at TEXT, granted_by TEXT NOT NULL, conditions_json TEXT, revoked_at TEXT, revoked_by TEXT); \
             INSERT INTO agents (name, parent_agent) VALUES ('dev', NULL); \
             INSERT INTO actions (name) VALUES ('card.add');").unwrap();
        c
    }
    #[test]
    fn grants_idempotently() {
        let c = seeded();
        // fresh grant: default granted_by=unknown, null conditions, id=1
        let r = agentGrant(&json!({ "agent_name": "dev", "action_name": "card.add" }), &c).unwrap();
        assert_eq!(r["id"], json!(1));
        assert_eq!(r["granted_by"], json!("unknown"));
        assert_eq!(r["conditions_json"], Value::Null);
        assert_eq!(r.get("idempotent"), None);
        // second call is idempotent — returns the existing row + idempotent:true, no new insert
        let r2 = agentGrant(&json!({ "agent_name": "dev", "action_name": "card.add" }), &c).unwrap();
        assert_eq!(r2["id"], json!(1));
        assert_eq!(r2["idempotent"], json!(true));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM agent_actions", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }
    #[test]
    fn serializes_conditions_and_granted_by() {
        let c = seeded();
        let r = agentGrant(&json!({ "agent_name": "dev", "action_name": "card.add", "granted_by": "human:dani", "conditions": { "status": ["doing", "done"] } }), &c).unwrap();
        assert_eq!(r["granted_by"], json!("human:dani"));
        assert_eq!(r["conditions_json"], json!(r#"{"status":["doing","done"]}"#));
    }
    #[test]
    fn revoked_grant_does_not_block_insert() {
        let c = seeded();
        c.execute("INSERT INTO agent_actions (id, agent_name, action_name, granted_by, revoked_at) VALUES (1, 'dev', 'card.add', 'seed', '2026-01-01')", []).unwrap();
        let r = agentGrant(&json!({ "agent_name": "dev", "action_name": "card.add" }), &c).unwrap();
        assert_eq!(r["id"], json!(2));
        assert_eq!(r.get("idempotent"), None);
    }
    #[test]
    fn guards() {
        let c = seeded();
        assert!(agentGrant(&json!({ "action_name": "card.add" }), &c).unwrap_err().contains("agent_name is required"));
        assert!(agentGrant(&json!({ "agent_name": "ghost", "action_name": "card.add" }), &c).unwrap_err().contains("agent 'ghost' not found"));
        assert!(agentGrant(&json!({ "agent_name": "dev", "action_name": "ghost.x" }), &c).unwrap_err().contains("action 'ghost.x' not found"));
    }
}
