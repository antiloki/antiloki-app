//! agent-permissions.rs — twin of skills/entities/agent-permissions.mjs (db lane). NAMES ARE
//! CONTRACT: `agentPermissions` (named default). SELECT * over agent_actions with filters.
// QUALITY: 86/100 — SELECT * handled generically by db_json (dynamic columns); logic is the port
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn agentPermissions(input: &Value, db: &Connection) -> Result<Value, String> {
    let mut sql = String::from("SELECT * FROM agent_actions WHERE 1=1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(a) = input.get("agent_name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        sql.push_str(" AND agent_name = ?");
        params.push(json!(a));
    }
    if let Some(a) = input.get("action_name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        sql.push_str(" AND action_name = ?");
        params.push(json!(a));
    }
    // JS default includeRevoked=false → filter to active grants
    let include_revoked = input.get("includeRevoked").and_then(Value::as_bool).unwrap_or(false);
    if !include_revoked {
        sql.push_str(" AND revoked_at IS NULL");
    }
    sql.push_str(" ORDER BY agent_name, action_name, granted_at DESC");
    crate::db_json::query_rows(db, &sql, &params)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE agent_actions (agent_name TEXT, action_name TEXT, granted_at TEXT, revoked_at TEXT);
            INSERT INTO agent_actions VALUES ('dev','x','t1',NULL),('dev','y','t2','t3'),('qa','z','t4',NULL);").unwrap();
        c
    }
    #[test]
    fn active_only_by_default_and_agent_filter() {
        let active = agentPermissions(&json!({}), &seeded()).unwrap();
        assert_eq!(active.as_array().unwrap().len(), 2); // the revoked one excluded
        let all = agentPermissions(&json!({ "includeRevoked": true }), &seeded()).unwrap();
        assert_eq!(all.as_array().unwrap().len(), 3);
        let dev = agentPermissions(&json!({ "agent_name": "dev" }), &seeded()).unwrap();
        assert_eq!(dev.as_array().unwrap().len(), 1);
    }
}
