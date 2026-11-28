//! memory-supersede.rs — twin of skills/entities/memory-supersede.mjs (db WRITE lane, attached
//! `memory` db). NAMES ARE CONTRACT: `memorySupersede` (default). Functional "delete": marks the
//! old memory as replaced by a newer one (which must exist), then returns the updated old row.
// QUALITY: 82/100 — integer-id guards, self-supersede + double-supersede + dangling-target checks
use rusqlite::Connection;
use serde_json::{json, Value};

// Number.isInteger(v): integer-VALUED numbers only (strings/bools/null/absent all fail).
fn as_integer(v: Option<&Value>) -> Option<i64> {
    v.and_then(|x| x.as_f64()).filter(|f| f.is_finite() && f.fract() == 0.0).map(|f| f as i64)
}

pub fn memorySupersede(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('memory-supersede','db') when no db — the db lane always injects one.
    let old_id = match as_integer(input.get("old_id")) {
        Some(i) => i,
        None => return Err("memory-supersede: old_id (integer) is required".into()),
    };
    let new_id = match as_integer(input.get("new_id")) {
        Some(i) => i,
        None => return Err("memory-supersede: new_id (integer) is required".into()),
    };
    if old_id == new_id {
        return Err("memory-supersede: old_id and new_id must differ".into());
    }

    let old_rows = crate::db_json::query_rows(db, "SELECT id, superseded_by FROM memory.memories WHERE id = ?", &[json!(old_id)])?;
    let old_row = match old_rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("memory-supersede: memory id '{old_id}' not found")),
    };
    // JS `oldRow.superseded_by != null` — loose !=, so only null/undefined pass through.
    if let Some(cur) = old_row.get("superseded_by").filter(|v| !v.is_null()) {
        return Err(format!("memory-supersede: memory id {old_id} is already superseded by {cur}"));
    }

    let new_rows = crate::db_json::query_rows(db, "SELECT id FROM memory.memories WHERE id = ?", &[json!(new_id)])?;
    if new_rows.as_array().map_or(true, |a| a.is_empty()) {
        return Err(format!("memory-supersede: memory id {new_id} not found (cannot supersede with nonexistent memory)"));
    }

    crate::db_json::run_write(db, "UPDATE memory.memories SET superseded_by = ? WHERE id = ?", &[json!(new_id), json!(old_id)])?;
    let out = crate::db_json::query_rows(db, "SELECT * FROM memory.memories WHERE id = ?", &[json!(old_id)])?;
    Ok(out.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Same table shape as the entities_memory_validate fixture (attached `memory` db).
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory;
            CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, tags_json TEXT NOT NULL DEFAULT '[]', source_card TEXT, confidence INTEGER NOT NULL DEFAULT 0, validated INTEGER NOT NULL DEFAULT 0, superseded_by INTEGER, created_by TEXT NOT NULL);
            INSERT INTO memory.memories (id,type,title,tags_json,source_card,confidence,validated,superseded_by,created_by) VALUES
              (1,'lesson','T1','[\"ai\"]',NULL,0,0,NULL,'test'),
              (2,'decision','T2','[]','CARD-X',1,1,NULL,'test'),
              (3,'lesson','T3-already','[]',NULL,0,0,9,'test');").unwrap();
        c
    }

    #[test]
    fn supersedes_and_returns_the_updated_old_row() {
        let c = db();
        let r = memorySupersede(&json!({ "old_id": 1, "new_id": 2 }), &c).unwrap();
        assert_eq!(r["id"], json!(1));
        assert_eq!(r["superseded_by"], json!(2));
        assert_eq!(r["title"], json!("T1"));
        assert_eq!(r["tags_json"], json!("[\"ai\"]"));
        // the newer memory itself stays untouched
        let other: Option<i64> = c.query_row("SELECT superseded_by FROM memory.memories WHERE id = 2", [], |r| r.get(0)).unwrap();
        assert_eq!(other, None);
    }

    #[test]
    fn non_integer_ids_err() {
        let c = db();
        for bad in [json!({ "old_id": "1", "new_id": 2 }), json!({ "old_id": 1.5, "new_id": 2 }), json!({ "new_id": 2 }), json!({ "old_id": null, "new_id": 2 }), Value::Null] {
            assert!(memorySupersede(&bad, &c).unwrap_err().contains("old_id (integer) is required"));
        }
        assert!(memorySupersede(&json!({ "old_id": 1, "new_id": "2" }), &c).unwrap_err().contains("new_id (integer) is required"));
        assert!(memorySupersede(&json!({ "old_id": 1 }), &c).unwrap_err().contains("new_id (integer) is required"));
    }

    #[test]
    fn self_supersede_errs() {
        assert!(memorySupersede(&json!({ "old_id": 1, "new_id": 1 }), &db()).unwrap_err().contains("must differ"));
    }

    #[test]
    fn missing_old_or_new_errs_not_found() {
        let c = db();
        assert!(memorySupersede(&json!({ "old_id": 99, "new_id": 1 }), &c).unwrap_err().contains("memory id '99' not found"));
        assert!(memorySupersede(&json!({ "old_id": 1, "new_id": 99 }), &c).unwrap_err().contains("memory id 99 not found (cannot supersede"));
        // guard-errors leave the row untouched
        let sb: Option<i64> = c.query_row("SELECT superseded_by FROM memory.memories WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(sb, None);
    }

    #[test]
    fn already_superseded_errs() {
        let e = memorySupersede(&json!({ "old_id": 3, "new_id": 1 }), &db()).unwrap_err();
        assert!(e.contains("memory id 3 is already superseded by 9"));
    }
}
