//! agent-add.rs — twin of skills/entities/agent-add.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `agentAdd` (named default). Register an agent persona (+ EPIC-SUBAGENT tree fields), SELECT * back.
// QUALITY: 84/100 — 10-col INSERT, JSON.stringify(actions/context/permissions), parent cycle+NOT_FOUND
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn agentAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    let name = input.get("name").cloned().unwrap_or(Value::Null);
    if name.as_str().map(|s| s.is_empty()).unwrap_or(true) { return Err("agent-add: name is required".into()); }
    let description = input.get("description").cloned().unwrap_or(Value::Null);
    if description.as_str().map(|s| s.is_empty()).unwrap_or(true) { return Err("agent-add: description is required".into()); }
    let name_str = name.as_str().unwrap();

    let parent = input.get("parent_agent").cloned().unwrap_or(Value::Null);
    if parent.as_str().map(|s| !s.is_empty()).unwrap_or(false) {
        let ps = parent.as_str().unwrap();
        if ps == name_str {
            return Err(format!("agent-add: agent '{name_str}' cannot be its own parent"));
        }
        let p = crate::db_json::query_rows(db, "SELECT 1 FROM agents WHERE name = ?", &[parent.clone()])?;
        if p.as_array().map(|a| a.is_empty()).unwrap_or(true) {
            return Err(format!("agent-add: parent_agent '{ps}' not found"));
        }
    }

    // JSON.stringify of the collection fields (defaults [], {}, {})
    let jstr = |k: &str, dflt_val: Value| -> Value {
        let v = input.get(k).cloned().unwrap_or(dflt_val);
        json!(serde_json::to_string(&v).unwrap())
    };
    let model = input.get("model").cloned().unwrap_or(Value::Null);
    let specialization = input.get("specialization_md").cloned().unwrap_or(Value::Null);
    // applies_to == null ? null : (string ? applies_to : JSON.stringify(applies_to))
    let applies_to = match input.get("applies_to") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) => json!(s),
        Some(v) => json!(serde_json::to_string(v).unwrap()),
    };
    let is_abstract = if truthy(input.get("is_abstract")) { 1 } else { 0 };

    crate::db_json::run_write(db,
        "INSERT INTO agents (name, description, skills_json, context_json, permissions_json, model, parent_agent, specialization_md, applies_to_json, is_abstract) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[name.clone(), description, jstr("actions", json!([])), jstr("context", json!({})), jstr("permissions", json!({})), model, parent, specialization, applies_to, json!(is_abstract)])?;
    let rows = crate::db_json::query_rows(db, "SELECT * FROM agents WHERE name = ?", &[name])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

fn truthy(v: Option<&Value>) -> bool {
    match v { Some(Value::Bool(b)) => *b, Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(), Some(Value::Null) | None => false, _ => true }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inserts_with_json_fields() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE agents (name TEXT UNIQUE, description TEXT, skills_json TEXT, context_json TEXT, permissions_json TEXT, model TEXT, parent_agent TEXT, specialization_md TEXT, applies_to_json TEXT, is_abstract INTEGER)").unwrap();
        let r = agentAdd(&json!({ "name": "pl", "description": "lead", "actions": ["x.do"], "context": { "reads": ["cards"] } }), &c).unwrap();
        assert_eq!(r["skills_json"], json!(r#"["x.do"]"#));
        assert_eq!(r["context_json"], json!(r#"{"reads":["cards"]}"#));
        assert!(agentAdd(&json!({ "name": "pl", "description": "dup" }), &c).unwrap_err().contains("UNIQUE"));
        assert!(agentAdd(&json!({ "name": "z", "description": "d", "parent_agent": "z" }), &c).unwrap_err().contains("its own parent"));
    }
}
