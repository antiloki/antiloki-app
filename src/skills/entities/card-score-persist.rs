//! card-score-persist.rs — twin of skills/entities/card-score-persist.mjs (db WRITE lane). NAMES
//! ARE CONTRACT: `cardScorePersist` (default). UPDATE the 4 scoring columns + updated_at, return row.
// QUALITY: writes risk/complexity/priority/score + updated_at=datetime('now') frozen to 2026-01-01;
// returns SELECT slug,risk,complexity,priority,score row. Success-path only (throws on missing slug).
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn cardScorePersist(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = match input.get("slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => return Err("card-score-persist: slug is required".into()),
    };
    let risk = input.get("risk").cloned().unwrap_or(Value::Null);
    let complexity = input.get("complexity").cloned().unwrap_or(Value::Null);
    let priority = input.get("priority").cloned().unwrap_or(Value::Null);
    let score = input.get("score").cloned().unwrap_or(Value::Null);

    // JS: UPDATE ... updated_at = datetime('now') — frozen-clock lane pins 'now' to the instant below.
    crate::db_json::run_write(
        db,
        &format!("UPDATE cards SET risk = ?, complexity = ?, priority = ?, score = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
        &[risk, complexity, priority, score, json!(slug)],
    )?;

    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, risk, complexity, priority, score FROM cards WHERE slug = ?",
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
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, risk INTEGER, complexity INTEGER, priority INTEGER, score INTEGER, updated_at TEXT); \
             INSERT INTO cards VALUES ('C-1', NULL, NULL, NULL, NULL, '2025-12-01 00:00:00'); \
             INSERT INTO cards VALUES ('C-OTHER', 9, 9, 9, 9, '2025-01-01 00:00:00');",
        ).unwrap();
        c
    }
    #[test]
    fn writes_facets_and_returns_row() {
        let c = seeded();
        let r = cardScorePersist(&json!({ "slug": "C-1", "risk": 3, "complexity": 5, "priority": 2, "score": 30 }), &c).unwrap();
        assert_eq!(r, json!({ "slug": "C-1", "risk": 3, "complexity": 5, "priority": 2, "score": 30 }));
        // updated_at was stamped with the frozen instant (not selected into the return).
        let stamp = crate::db_json::query_rows(&c, "SELECT updated_at FROM cards WHERE slug = ?", &[json!("C-1")]).unwrap();
        assert_eq!(stamp[0]["updated_at"], json!("2026-01-01 00:00:00"));
    }
    #[test]
    fn null_score_and_untouched_sibling() {
        let c = seeded();
        let r = cardScorePersist(&json!({ "slug": "C-1", "risk": 1, "complexity": 1, "priority": 1, "score": null }), &c).unwrap();
        assert_eq!(r, json!({ "slug": "C-1", "risk": 1, "complexity": 1, "priority": 1, "score": null }));
        // the WHERE clause leaves the sibling row exactly as seeded.
        let other = crate::db_json::query_rows(&c, "SELECT score, updated_at FROM cards WHERE slug = ?", &[json!("C-OTHER")]).unwrap();
        assert_eq!(other[0]["score"], json!(9));
        assert_eq!(other[0]["updated_at"], json!("2025-01-01 00:00:00"));
    }
    #[test]
    fn missing_slug_errs() {
        let c = seeded();
        assert!(cardScorePersist(&json!({ "risk": 1 }), &c).is_err());
    }
}
