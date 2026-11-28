//! ui-story-delete.rs — twin of skills/codebase/ui-story-delete.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `uiStoryDelete` (named default). DELETE + {ok, deleted}.
// QUALITY: 87/100 — Number(id) truthiness gate; table-absent → deleted:0
use rusqlite::{params, Connection};
use serde_json::{json, Value};

pub fn uiStoryDelete(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: id = Number(input?.id); if (!id) throw  — 0 / NaN / absent all fail
    let id = input.get("id").and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
    let id = match id {
        Some(n) if n != 0 => n,
        _ => return Err("ui-story-delete: id is required".into()),
    };
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas");
    let deleted = db.execute("DELETE FROM code_user_stories WHERE id = ?1 AND origin = ?2", params![id, origin]).unwrap_or(0);
    Ok(json!({ "ok": true, "deleted": deleted }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deletes_and_gates_id() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_user_stories (id INTEGER, origin TEXT); INSERT INTO code_user_stories VALUES (1,'atlas');").unwrap();
        assert_eq!(uiStoryDelete(&json!({ "id": 1 }), &c).unwrap(), json!({ "ok": true, "deleted": 1 }));
        assert!(uiStoryDelete(&json!({}), &c).is_err());
        assert!(uiStoryDelete(&json!({ "id": 0 }), &c).is_err());
    }
}
