//! agent-grants-check.rs — twin of skills/entities/agent-grants-check.mjs (db lane). NAMES ARE
//! CONTRACT: `agentGrantsCheck` (default) + `grantWithinParent` (named, db-first helper). Subset invariant.
// QUALITY: 84/100 — child.grants ⊆ parent.grants; root/legacy → ok; helper conformed via the sig lane
use rusqlite::Connection;
use serde_json::{json, Value};

// db-first helper: ok=true means granting `action` to `agent` does not exceed the parent.
pub fn grantWithinParent(db: &Connection, agent_name: &str, action_name: &str) -> Value {
    let row = match crate::db_json::query_rows(db, "SELECT parent_agent FROM agents WHERE name = ?", &[json!(agent_name)]) {
        Ok(r) => r,
        Err(_) => return json!({ "ok": true, "parent": null }), // legacy DB without the column
    };
    let row0 = match row.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return json!({ "ok": false, "reason": format!("agent '{agent_name}' not found"), "parent": null }),
    };
    let parent = row0.get("parent_agent").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let p = match parent { Some(p) => p, None => return json!({ "ok": true, "parent": null }) }; // root — unconstrained

    let has = crate::db_json::query_rows(db,
        "SELECT 1 FROM agent_actions WHERE agent_name = ? AND action_name = ? AND revoked_at IS NULL LIMIT 1",
        &[json!(p), json!(action_name)]).unwrap_or(json!([]));
    if !has.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return json!({ "ok": true, "parent": p });
    }
    json!({ "ok": false, "parent": p,
        "reason": format!("sub-agent '{agent_name}' cannot be granted '{action_name}' — its parent '{p}' does not hold that grant (child.grants ⊆ parent.grants)") })
}

pub fn agentGrantsCheck(input: &Value, db: &Connection) -> Result<Value, String> {
    let agent = match input.get("agent").filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false)) {
        Some(v) => v.as_str().unwrap().to_string(),
        None => return Err("agent-grants-check: agent is required".into()),
    };
    let action = match input.get("action").filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false)) {
        Some(v) => v.as_str().unwrap().to_string(),
        None => return Err("agent-grants-check: action is required".into()),
    };
    Ok(grantWithinParent(db, &agent, &action))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subset_invariant() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE agents (name TEXT, parent_agent TEXT); CREATE TABLE agent_actions (agent_name TEXT, action_name TEXT, revoked_at TEXT); \
          INSERT INTO agents VALUES ('root',NULL),('p.child','p'),('p',NULL);").unwrap();
        assert_eq!(grantWithinParent(&c, "root", "x")["ok"], json!(true)); // root unconstrained
        assert_eq!(grantWithinParent(&c, "p.child", "x")["ok"], json!(false)); // parent lacks grant
        c.execute("INSERT INTO agent_actions VALUES ('p','x',NULL)", []).unwrap();
        assert_eq!(grantWithinParent(&c, "p.child", "x")["ok"], json!(true)); // now within parent
        assert_eq!(grantWithinParent(&c, "ghost", "x")["ok"], json!(false)); // not found
        assert!(agentGrantsCheck(&json!({ "action": "x" }), &c).is_err());
    }
}
