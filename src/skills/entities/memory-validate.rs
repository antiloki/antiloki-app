//! memory-validate.rs — twin of skills/entities/memory-validate.mjs (db WRITE lane, attached
//! `memory` db). NAMES ARE CONTRACT: `memoryValidate` (default). Marks a memory human-confirmed:
//! flips validated to 1, bumps confidence (default 1.0, explicit override 0..1), returns the row.
// QUALITY: 82/100 — integer-id + 0..1 confidence guards, existence check, UPDATE + SELECT * back
use rusqlite::Connection;
use serde_json::{json, Value};

// = JS `${v}` (only feeds the range error message).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

pub fn memoryValidate(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('memory-validate','db') when no db — the db lane always injects one.
    // Number.isInteger(id): integer-VALUED numbers only (strings/bools/null/absent all fail).
    let id = match input.get("id").and_then(|v| v.as_f64()).filter(|f| f.is_finite() && f.fract() == 0.0) {
        Some(f) => f as i64,
        None => return Err("memory-validate: id (integer) is required".into()),
    };
    // destructuring default: confidence = 1.0 fires only when the key is ABSENT; explicit null
    // stays null and fails the typeof-number guard below, exactly like JS.
    let confidence = match input.get("confidence") { None => json!(1.0), Some(v) => v.clone() };
    if !confidence.is_number() || confidence.as_f64().map_or(true, |f| f < 0.0 || f > 1.0) {
        return Err(format!("memory-validate: confidence must be between 0 and 1 (got {})", tmpl(Some(&confidence))));
    }

    let rows = crate::db_json::query_rows(db, "SELECT id FROM memory.memories WHERE id = ?", &[json!(id)])?;
    if rows.as_array().map_or(true, |a| a.is_empty()) {
        return Err(format!("memory-validate: memory id '{id}' not found"));
    }

    crate::db_json::run_write(
        db,
        "UPDATE memory.memories SET validated = 1, confidence = ? WHERE id = ?",
        &[confidence, json!(id)],
    )?;
    let out = crate::db_json::query_rows(db, "SELECT * FROM memory.memories WHERE id = ?", &[json!(id)])?;
    Ok(out.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Same table shape as the entities_memory_list fixture: confidence has INTEGER affinity so the
    // default 1.0 lands as integer 1 in BOTH lanes (JS binds number 1; SQLite converts a lossless
    // real to int under INTEGER affinity), while fractional overrides (0.85) stay REAL in both.
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory;
            CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, tags_json TEXT NOT NULL DEFAULT '[]', source_card TEXT, confidence INTEGER NOT NULL DEFAULT 0, validated INTEGER NOT NULL DEFAULT 0, superseded_by INTEGER, created_by TEXT NOT NULL);
            INSERT INTO memory.memories (id,type,title,tags_json,source_card,confidence,validated,superseded_by,created_by) VALUES
              (1,'lesson','T1','[]',NULL,0,0,NULL,'test'),
              (2,'decision','T2','[\"arch\"]','CARD-X',0,1,NULL,'test');").unwrap();
        c
    }

    #[test]
    fn default_confidence_flips_validated_and_bumps_to_1() {
        let c = db();
        let r = memoryValidate(&json!({ "id": 1 }), &c).unwrap();
        assert_eq!(r["validated"], json!(1));
        assert_eq!(r["confidence"], json!(1)); // 1.0 → int 1 under INTEGER affinity
        assert_eq!(r["title"], json!("T1"));
        // other row untouched
        let other: i64 = c.query_row("SELECT validated FROM memory.memories WHERE id = 2", [], |r| r.get(0)).unwrap();
        assert_eq!(other, 1);
    }

    #[test]
    fn custom_confidence_override_stays_fractional() {
        let c = db();
        let r = memoryValidate(&json!({ "id": 2, "confidence": 0.85 }), &c).unwrap();
        assert_eq!(r["validated"], json!(1));
        assert_eq!(r["confidence"], json!(0.85));
        assert_eq!(r["source_card"], json!("CARD-X"));
    }

    #[test]
    fn boundary_confidences_accepted() {
        let c = db();
        assert_eq!(memoryValidate(&json!({ "id": 1, "confidence": 0 }), &c).unwrap()["confidence"], json!(0));
        assert_eq!(memoryValidate(&json!({ "id": 1, "confidence": 1 }), &c).unwrap()["confidence"], json!(1));
    }

    #[test]
    fn nonexistent_id_errs_not_found() {
        let e = memoryValidate(&json!({ "id": 9999 }), &db()).unwrap_err();
        assert!(e.contains("memory id '9999' not found"));
    }

    #[test]
    fn non_integer_id_errs() {
        let c = db();
        for bad in [json!({ "id": "one" }), json!({ "id": 1.5 }), json!({ "id": null }), json!({}), Value::Null] {
            assert!(memoryValidate(&bad, &c).unwrap_err().contains("id (integer) is required"));
        }
    }

    #[test]
    fn out_of_range_or_non_number_confidence_errs() {
        let c = db();
        assert!(memoryValidate(&json!({ "id": 1, "confidence": 1.5 }), &c).unwrap_err().contains("between 0 and 1 (got 1.5)"));
        assert!(memoryValidate(&json!({ "id": 1, "confidence": -0.1 }), &c).unwrap_err().contains("confidence"));
        assert!(memoryValidate(&json!({ "id": 1, "confidence": null }), &c).unwrap_err().contains("confidence"));
        assert!(memoryValidate(&json!({ "id": 1, "confidence": "high" }), &c).unwrap_err().contains("confidence"));
    }
}
