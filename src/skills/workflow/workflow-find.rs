//! workflow-find.rs — twin of skills/workflow/workflow-find.mjs (db lane). NAMES ARE CONTRACT:
//! `workflowFind` (named default). Single-row lookup + steps_json parse.
// QUALITY: 85/100 — establishes the JSON-column pattern: a null column mirrors JS JSON.parse(null)===null
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn workflowFind(input: &Value, db: &Connection) -> Result<Value, String> {
    let name = match input.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(n) => n,
        None => return Err("workflow-find: name is required".into()),
    };
    let rows = crate::db_json::query_rows(db, "SELECT * FROM workflows WHERE name = ? LIMIT 1", &[json!(name)])?;
    let arr = rows.as_array().unwrap();
    if arr.is_empty() {
        return Err(format!("workflow-find: workflow '{name}' not found"));
    }
    let mut row = arr[0].as_object().unwrap().clone();
    // JS: let steps=[]; try { steps = JSON.parse(row.steps_json); } catch {}
    // a valid JSON string parses; an invalid one leaves []; a NULL column → JSON.parse(null)===null
    let steps = match row.get("steps_json") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or_else(|_| json!([])),
        _ => Value::Null,
    };
    row.insert("steps".into(), steps);
    Ok(Value::Object(row))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE workflows (id INTEGER, name TEXT, steps_json TEXT);
            INSERT INTO workflows VALUES (1,'w','[{\"skill\":\"x\"}]');").unwrap();
        c
    }
    #[test]
    fn finds_and_parses_steps() {
        let r = workflowFind(&json!({ "name": "w" }), &seeded()).unwrap();
        assert_eq!(r["steps"][0]["skill"], json!("x"));
        assert!(workflowFind(&json!({ "name": "ghost" }), &seeded()).is_err());
        assert!(workflowFind(&json!({}), &seeded()).is_err());
    }
}
