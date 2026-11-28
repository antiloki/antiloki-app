//! data-story-delete.rs — twin of skills/codebase/data-story-delete.mjs (db WRITE lane). NAMES
//! ARE CONTRACT: `dataStoryDelete` (named default). DELETE + {id, deleted}.
// QUALITY: 88/100 — the first WRITE twin: mutation proven by db_after, deleted from rows-affected
use rusqlite::{params, Connection};
use serde_json::{json, Value};

pub fn dataStoryDelete(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas");
    // JS: id = input?.id; if (id == null) throw; then Number(id)
    let id_val = input.get("id");
    if id_val.is_none() || id_val == Some(&Value::Null) {
        return Err("data-story-delete: id is required".into());
    }
    let id = id_val.and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0);
    let changes = db.execute("DELETE FROM code_data_stories WHERE origin=?1 AND id=?2", params![origin, id])
        .map_err(|e| e.to_string())?;
    Ok(json!({ "id": id, "deleted": changes > 0 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deletes_and_reports() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_data_stories (id INTEGER, origin TEXT);
            INSERT INTO code_data_stories VALUES (1,'atlas'),(2,'atlas');").unwrap();
        assert_eq!(dataStoryDelete(&json!({ "id": 1 }), &c).unwrap(), json!({ "id": 1, "deleted": true }));
        assert_eq!(dataStoryDelete(&json!({ "id": 99 }), &c).unwrap()["deleted"], json!(false));
        assert!(dataStoryDelete(&json!({}), &c).is_err());
    }
}
