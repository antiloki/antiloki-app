//! data-story-add.rs — twin of skills/codebase/data-story-add.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `dataStoryAdd` (named default). INSERT + {id (lastInsertRowid), name, direction, hop_count}.
// QUALITY: 86/100 — INSERT with last_insert_rowid; hops_json via preserve_order = JS JSON.stringify
use rusqlite::{params, Connection};
use serde_json::{json, Value};

pub fn dataStoryAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas");
    let name_raw = input.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if name_raw.trim().is_empty() {
        return Err("data-story-add: name is required".into());
    }
    // JS: direction === 'backward' ? 'backward' : 'forward'
    let direction = if input.get("direction").and_then(|v| v.as_str()) == Some("backward") { "backward" } else { "forward" };
    // JS: Array.isArray(hops) ? hops : []
    let hops = input.get("hops").filter(|v| v.is_array()).cloned().unwrap_or_else(|| json!([]));
    let hop_count = hops.as_array().map(|a| a.len()).unwrap_or(0);
    let name_200: String = name_raw.chars().take(200).collect(); // JS String(name).slice(0,200)
    let description = input.get("description").cloned().unwrap_or(Value::Null);
    let linked_test = input.get("linked_test").cloned().unwrap_or(Value::Null);
    db.execute(
        "INSERT INTO code_data_stories (origin, name, description, direction, hops_json, linked_test) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![origin, name_200, json_param(&description), direction, serde_json::to_string(&hops).unwrap(), json_param(&linked_test)],
    ).map_err(|e| e.to_string())?;
    Ok(json!({ "id": db.last_insert_rowid(), "name": name_raw, "direction": direction, "hop_count": hop_count }))
}

// null → SQL NULL, string → text; description/linked_test are strings or null here
fn json_param(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inserts_and_returns_id() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_data_stories (id INTEGER PRIMARY KEY, origin TEXT, name TEXT, description TEXT, direction TEXT, hops_json TEXT, linked_test TEXT)").unwrap();
        let r = dataStoryAdd(&json!({ "name": "s1", "hops": [{ "node_id": 1 }] }), &c).unwrap();
        assert_eq!(r["id"], json!(1));
        assert_eq!(r["direction"], json!("forward"));
        assert_eq!(r["hop_count"], json!(1));
        let stored: String = c.query_row("SELECT hops_json FROM code_data_stories WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(stored, "[{\"node_id\":1}]");
        assert!(dataStoryAdd(&json!({}), &c).is_err());
    }
}
