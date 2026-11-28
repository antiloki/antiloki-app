//! card-accuracy-persist.rs — twin of skills/entities/card-accuracy-persist.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `cardAccuracyPersist` (default). Audited write of the accuracy columns
//! (accuracy_score / accuracy_detail_json / accuracy_computed_at), then returns the fresh row.
// QUALITY: 84/100 — ?? null + JSON.stringify(detail) bind semantics; datetime('now') frozen to 2026-01-01
use rusqlite::Connection;
use serde_json::{json, Value};

// JS `x ?? null`: null AND undefined both fall through to null.
fn nz_or_null(v: Option<&Value>) -> Value {
    match v {
        None | Some(Value::Null) => Value::Null,
        Some(x) => x.clone(),
    }
}

pub fn cardAccuracyPersist(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = match input.get("slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Err("card-accuracy-persist: slug is required".into()),
    };

    // param 1: accuracy_score ?? null
    let score = nz_or_null(input.get("accuracy_score"));
    // param 2: accuracy_detail != null ? JSON.stringify(accuracy_detail) : null
    //   JS `!= null` is true only when present AND non-null → then compact-JSON it (bound as TEXT).
    let detail = match input.get("accuracy_detail") {
        None | Some(Value::Null) => Value::Null,
        Some(v) => json!(serde_json::to_string(v).map_err(|e| e.to_string())?),
    };

    // JS: accuracy_computed_at = datetime('now'), updated_at = datetime('now')
    //   the JS conform lane freezes 'now'; the twin writes the frozen literal to match byte-for-byte.
    crate::db_json::run_write(
        db,
        &format!("UPDATE cards \
         SET accuracy_score = ?, accuracy_detail_json = ?, accuracy_computed_at = {}, updated_at = {} \
         WHERE slug = ?", crate::clock::now_sql(), crate::clock::now_sql()),
        &[score, detail, json!(slug)],
    )?;

    // .get(slug): first matching row, or undefined (→ null) when no card matched.
    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, accuracy_score, accuracy_detail_json, accuracy_computed_at FROM cards WHERE slug = ?",
        &[json!(slug)],
    )?;
    Ok(rows.as_array().unwrap().first().cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, accuracy_score INTEGER, accuracy_detail_json TEXT, accuracy_computed_at TEXT, updated_at TEXT); \
             INSERT INTO cards VALUES ('C-1', NULL, NULL, NULL, '2025-12-01 00:00:00');",
        )
        .unwrap();
        c
    }

    #[test]
    fn persists_score_and_detail() {
        let c = seeded();
        let r = cardAccuracyPersist(
            &json!({ "slug": "C-1", "accuracy_score": 85, "accuracy_detail": { "model": "bge", "hits": [1, 2], "note": "ok" } }),
            &c,
        )
        .unwrap();
        assert_eq!(r["slug"], json!("C-1"));
        assert_eq!(r["accuracy_score"], json!(85));
        // JSON.stringify preserves insertion order → compact JSON string stored as TEXT.
        assert_eq!(r["accuracy_detail_json"], json!("{\"model\":\"bge\",\"hits\":[1,2],\"note\":\"ok\"}"));
        assert_eq!(r["accuracy_computed_at"], json!("2026-01-01 00:00:00"));
    }

    #[test]
    fn nulls_when_absent() {
        let c = seeded();
        let r = cardAccuracyPersist(&json!({ "slug": "C-1" }), &c).unwrap();
        assert_eq!(r["accuracy_score"], Value::Null);
        assert_eq!(r["accuracy_detail_json"], Value::Null);
        assert_eq!(r["accuracy_computed_at"], json!("2026-01-01 00:00:00"));
    }

    #[test]
    fn missing_card_returns_null() {
        let c = seeded();
        let r = cardAccuracyPersist(&json!({ "slug": "GHOST", "accuracy_score": 1 }), &c).unwrap();
        assert_eq!(r, Value::Null);
    }

    #[test]
    fn requires_slug() {
        let c = seeded();
        assert!(cardAccuracyPersist(&json!({ "accuracy_score": 1 }), &c).is_err());
    }
}
