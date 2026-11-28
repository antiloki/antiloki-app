//! workflow-add.rs — twin of skills/workflow/workflow-add.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `workflowAdd` (named default). Register a workflow in state.workflows after validating its steps.
// QUALITY: 83/100 — required fields, on_failure enum, per-step object+action validation, SELECT * back
use rusqlite::Connection;
use serde_json::{json, Value};

// JS typeof for the "(got …)" message.
fn jstypeof(v: &Value) -> &'static str {
    match v { Value::Null => "object", Value::Bool(_) => "boolean", Value::Number(_) => "number",
        Value::String(_) => "string", Value::Array(_) | Value::Object(_) => "object" }
}

pub fn workflowAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    let req = |k: &str| -> Result<Value, String> {
        match input.get(k) { Some(v) if !v.is_null() && v.as_str() != Some("") => Ok(v.clone()), _ => Err(format!("workflow-add: {k} is required")) }
    };
    let name = req("name")?;
    let description = req("description")?;
    let owner_agent = req("owner_agent")?;

    let steps = input.get("steps");
    let steps_arr = match steps { Some(Value::Array(a)) if !a.is_empty() => a.clone(), _ => return Err("workflow-add: steps (non-empty array) is required".into()) };
    let on_failure = input.get("on_failure").and_then(|v| v.as_str()).unwrap_or("abort").to_string();
    if !matches!(on_failure.as_str(), "abort" | "continue") {
        return Err(format!("workflow-add: on_failure must be one of: abort, continue (got '{on_failure}')"));
    }
    for (i, s) in steps_arr.iter().enumerate() {
        if !s.is_object() {
            return Err(format!("workflow-add: steps[{i}] must be object (got {})", jstypeof(s)));
        }
        let action_ok = s.get("action").and_then(|a| a.as_str()).map(|a| !a.is_empty()).unwrap_or(false);
        if !action_ok {
            return Err(format!("workflow-add: steps[{i}].action is required"));
        }
    }

    let steps_json = serde_json::to_string(&Value::Array(steps_arr)).unwrap();
    let (_c, id) = crate::db_json::run_write(db,
        "INSERT INTO workflows (name, description, owner_agent, steps_json, on_failure) VALUES (?, ?, ?, ?, ?)",
        &[name, description, owner_agent, json!(steps_json), json!(on_failure)])?;
    let rows = crate::db_json::query_rows(db, "SELECT * FROM workflows WHERE id = ?", &[json!(id)])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inserts_and_validates_steps() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE workflows (id INTEGER PRIMARY KEY, name TEXT, description TEXT, owner_agent TEXT, steps_json TEXT, on_failure TEXT)").unwrap();
        let r = workflowAdd(&json!({ "name": "w", "description": "d", "owner_agent": "in", "steps": [{ "action": "a.b" }] }), &c).unwrap();
        assert_eq!(r["name"], json!("w"));
        assert!(workflowAdd(&json!({ "name": "w", "description": "d", "owner_agent": "in", "steps": [] }), &c).unwrap_err().contains("steps (non-empty array)"));
        assert!(workflowAdd(&json!({ "name": "w", "description": "d", "owner_agent": "in", "steps": ["x"] }), &c).unwrap_err().contains("steps[0] must be object (got string)"));
        assert!(workflowAdd(&json!({ "name": "w", "description": "d", "owner_agent": "in", "steps": [{}] }), &c).unwrap_err().contains("steps[0].action is required"));
    }
}
