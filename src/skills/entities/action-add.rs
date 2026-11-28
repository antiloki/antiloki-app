//! action-add.rs — twin of skills/entities/action-add.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! the export is named `skillAdd` (not actionAdd). Register an action + validate steps/kind.
// QUALITY: 84/100 — steps type/empty checks, kind enum, workflow-requires-action-run, owner NOT_FOUND
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn skillAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    let req = |k: &str| -> Result<Value, String> {
        match input.get(k) { Some(v) if !v.is_null() && v.as_str() != Some("") => Ok(v.clone()), _ => Err(format!("action-add: {k} is required")) }
    };
    let name = req("name")?;
    let description = req("description")?;
    let owner_agent = req("owner_agent")?;

    let steps = input.get("steps");
    let steps_arr = match steps {
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return Err("action-add: steps must be an array".into()),
        None | Some(Value::Null) => return Err("action-add: steps must be an array".into()), // !steps → type error
    };
    if steps_arr.is_empty() { return Err("action-add: steps must not be empty".into()); }

    let kind = input.get("kind").and_then(|v| v.as_str()).unwrap_or("atomic").to_string();
    if !matches!(kind.as_str(), "atomic" | "workflow") {
        return Err(format!("action-add: kind must be one of: atomic, workflow (got '{kind}')"));
    }
    if kind == "workflow" {
        let has_action_run = steps_arr.iter().any(|s| s.get("skill").and_then(|v| v.as_str()) == Some("action-run"));
        if !has_action_run {
            return Err("action-add: kind='workflow' requires steps to include at least one {skill: 'action-run'} step (composing other actions). Use kind='atomic' for skill-only chains.".into());
        }
    }

    let owner = owner_agent.as_str().unwrap();
    let a = crate::db_json::query_rows(db, "SELECT 1 FROM agents WHERE name = ?", &[owner_agent.clone()])?;
    if a.as_array().map(|x| x.is_empty()).unwrap_or(true) {
        return Err(format!("action-add: owner_agent '{owner}' not found"));
    }

    let returns = input.get("returns").cloned().unwrap_or(Value::Null);
    let on_failure = input.get("on_failure").cloned().unwrap_or(json!("abort"));
    let steps_json = serde_json::to_string(&Value::Array(steps_arr)).unwrap();
    crate::db_json::run_write(db,
        "INSERT INTO actions (name, description, owner_agent, steps_json, returns, on_failure, kind) VALUES (?, ?, ?, ?, ?, ?, ?)",
        &[name.clone(), description, owner_agent, json!(steps_json), returns, on_failure, json!(kind)])?;
    let rows = crate::db_json::query_rows(db, "SELECT * FROM actions WHERE name = ?", &[name])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seed() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE agents (name TEXT); CREATE TABLE actions (name TEXT UNIQUE, description TEXT, owner_agent TEXT, steps_json TEXT, returns TEXT, on_failure TEXT, kind TEXT); INSERT INTO agents VALUES ('in');").unwrap();
        c
    }
    #[test]
    fn inserts_and_validates() {
        let c = seed();
        let r = skillAdd(&json!({ "name": "a.b", "description": "d", "owner_agent": "in", "steps": [{ "skill": "x" }] }), &c).unwrap();
        assert_eq!(r["kind"], json!("atomic"));
        assert!(skillAdd(&json!({ "name": "z", "description": "d", "owner_agent": "in", "steps": "no" }), &c).unwrap_err().contains("must be an array"));
        assert!(skillAdd(&json!({ "name": "z", "description": "d", "owner_agent": "in", "steps": [] }), &c).unwrap_err().contains("must not be empty"));
        assert!(skillAdd(&json!({ "name": "z", "description": "d", "owner_agent": "in", "steps": [{ "skill": "x" }], "kind": "workflow" }), &c).unwrap_err().contains("action-run"));
        assert!(skillAdd(&json!({ "name": "z", "description": "d", "owner_agent": "ghost", "steps": [{ "skill": "x" }] }), &c).unwrap_err().contains("owner_agent 'ghost' not found"));
    }
}
