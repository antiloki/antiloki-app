//! agent-revoke.rs — twin of skills/entities/agent-revoke.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `agentRevoke` (default). Append-only revoke: stamps revoked_at/revoked_by on the active grant.
// QUALITY: 82/100 — active-grant lookup (revoked_at IS NULL), frozen-clock UPDATE, SELECT * back
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

pub fn agentRevoke(input: &Value, db: &Connection) -> Result<Value, String> {
    // context.actor defaults to 'unknown'; the db-lane harness passes context = { db } only,
    // so actor is always 'unknown' (matches the JS `const { actor = 'unknown' } = context`).
    let actor = json!("unknown");

    let agent_name = input.get("agent_name").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&agent_name)) { return Err("agent-revoke: agent_name is required".into()); }
    let action_name = input.get("action_name").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&action_name)) { return Err("agent-revoke: action_name is required".into()); }

    // revoked_by = actor (default fires only when the key is absent/undefined, NOT on null).
    let revoked_by = match input.get("revoked_by") { None => actor.clone(), Some(v) => v.clone() };

    // Active grant: the first row with revoked_at IS NULL. Append-only — never DELETEs.
    let grant = crate::db_json::query_rows(db,
        "SELECT * FROM agent_actions WHERE agent_name = ? AND action_name = ? AND revoked_at IS NULL",
        &[agent_name.clone(), action_name.clone()])?;
    let grant = match grant.as_array().and_then(|a| a.first()) {
        Some(g) => g.clone(),
        // NOT_FOUND: revoking something not granted is a logic error (guard path — not fixtured).
        None => return Err(format!(
            "agent-revoke: no active grant for agent='{}' action='{}'",
            js_string(&agent_name), js_string(&action_name))),
    };
    let id = grant.get("id").cloned().unwrap_or(Value::Null);

    // Frozen-clock lane: JS `datetime('now')` is rewritten to the frozen literal by the harness;
    // the Rust twin writes the SAME literal so both lanes run identical SQL.
    crate::db_json::run_write(db,
        &format!("UPDATE agent_actions SET revoked_at = {}, revoked_by = ? WHERE id = ?", crate::clock::now_sql()),
        &[revoked_by, id.clone()])?;

    let row = crate::db_json::query_rows(db, "SELECT * FROM agent_actions WHERE id = ?", &[id])?;
    Ok(row.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE agent_actions (id INTEGER PRIMARY KEY AUTOINCREMENT, agent_name TEXT NOT NULL, action_name TEXT NOT NULL, granted_at TEXT NOT NULL DEFAULT (datetime('now')), granted_by TEXT NOT NULL, conditions_json TEXT, revoked_at TEXT, revoked_by TEXT); \
             INSERT INTO agent_actions (id, agent_name, action_name, granted_at, granted_by, revoked_at, revoked_by) VALUES (1, 'DEV', 'card.add', '2025-11-01 00:00:00', 'human:dani', '2025-12-01 00:00:00', 'old'); \
             INSERT INTO agent_actions (id, agent_name, action_name, granted_at, granted_by) VALUES (2, 'DEV', 'card.add', '2025-12-20 00:00:00', 'human:dani');").unwrap();
        c
    }
    #[test]
    fn revokes_active_grant_skipping_already_revoked() {
        let c = seeded();
        let r = agentRevoke(&json!({ "agent_name": "DEV", "action_name": "card.add", "revoked_by": "human:danil" }), &c).unwrap();
        // picks the active row (id=2), not the already-revoked decoy (id=1)
        assert_eq!(r["id"], json!(2));
        assert_eq!(r["revoked_at"], json!("2026-01-01 00:00:00"));
        assert_eq!(r["revoked_by"], json!("human:danil"));
        // the already-revoked row is untouched
        let old: String = c.query_row("SELECT revoked_by FROM agent_actions WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(old, "old");
    }
    #[test]
    fn revoked_by_defaults_to_unknown() {
        let c = seeded();
        let r = agentRevoke(&json!({ "agent_name": "DEV", "action_name": "card.add" }), &c).unwrap();
        assert_eq!(r["id"], json!(2));
        assert_eq!(r["revoked_by"], json!("unknown"));
        assert_eq!(r["revoked_at"], json!("2026-01-01 00:00:00"));
    }
    #[test]
    fn guards_and_not_found() {
        let c = seeded();
        assert!(agentRevoke(&json!({ "action_name": "card.add" }), &c).unwrap_err().contains("agent_name is required"));
        assert!(agentRevoke(&json!({ "agent_name": "DEV" }), &c).unwrap_err().contains("action_name is required"));
        assert!(agentRevoke(&json!({ "agent_name": "GHOST", "action_name": "card.add" }), &c).unwrap_err().contains("no active grant"));
    }
}
