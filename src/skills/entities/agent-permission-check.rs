//! agent-permission-check.rs — twin of skills/entities/agent-permission-check.mjs (db lane).
//! NAMES ARE CONTRACT: `agentPermissionCheck` (named default). Returns {allowed, reason?}.
// QUALITY: 88/100 — boolean check, clean; reason only on deny (matches JS)
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn agentPermissionCheck(input: &Value, db: &Connection) -> Result<Value, String> {
    let agent = match input.get("agent_name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(a) => a,
        None => return Err("agent-permission-check: agent_name is required".into()),
    };
    let action = match input.get("action_name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(a) => a,
        None => return Err("agent-permission-check: action_name is required".into()),
    };
    let rows = crate::db_json::query_rows(db,
        "SELECT id FROM agent_actions WHERE agent_name = ? AND action_name = ? AND revoked_at IS NULL LIMIT 1",
        &[json!(agent), json!(action)])?;
    if rows.as_array().unwrap().is_empty() {
        Ok(json!({ "allowed": false, "reason": format!("agent '{agent}' has no active grant for action '{action}'") }))
    } else {
        Ok(json!({ "allowed": true }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE agent_actions (id INTEGER, agent_name TEXT, action_name TEXT, revoked_at TEXT);
            INSERT INTO agent_actions VALUES (1,'dev','x',NULL),(2,'dev','y','t');").unwrap();
        c
    }
    #[test]
    fn allow_deny_and_required() {
        assert_eq!(agentPermissionCheck(&json!({ "agent_name": "dev", "action_name": "x" }), &seeded()).unwrap()["allowed"], json!(true));
        let d = agentPermissionCheck(&json!({ "agent_name": "dev", "action_name": "y" }), &seeded()).unwrap();
        assert_eq!(d["allowed"], json!(false));
        assert!(d["reason"].is_string());
        assert!(agentPermissionCheck(&json!({ "agent_name": "dev" }), &seeded()).is_err());
    }
}
