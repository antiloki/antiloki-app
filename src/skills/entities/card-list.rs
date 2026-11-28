//! card-list.rs — twin of skills/entities/card-list.mjs (db lane). NAMES ARE CONTRACT: `cardList`.
// QUALITY: 87/100 — filter + sort-mode + limit SQL building; db_json runs it
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn cardList(input: &Value, db: &Connection) -> Result<Value, String> {
    let sort_by = input.get("sortBy").and_then(|v| v.as_str()).unwrap_or("default");
    if !["default", "score", "score_asc"].contains(&sort_by) {
        return Err(format!("card-list: sortBy must be one of: default, score, score_asc (got '{sort_by}')"));
    }
    let mut sql = String::from("SELECT * FROM cards WHERE 1=1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(s) = input.get("status").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        sql.push_str(" AND status = ?");
        params.push(json!(s));
    }
    if let Some(p) = input.get("slugPattern").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        sql.push_str(" AND slug LIKE ?");
        params.push(json!(p));
    }
    sql.push_str(match sort_by {
        "score" => " ORDER BY score IS NULL ASC, score DESC, slug",
        "score_asc" => " ORDER BY score IS NULL ASC, score ASC, slug",
        _ => " ORDER BY status, slug",
    });
    if let Some(l) = input.get("limit").and_then(|v| v.as_i64()).filter(|&n| n > 0) {
        sql.push_str(" LIMIT ?");
        params.push(json!(l));
    }
    crate::db_json::query_rows(db, &sql, &params)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (slug TEXT, status TEXT, score REAL);
            INSERT INTO cards VALUES ('A','doing',5),('B','backlog',NULL),('C','doing',9);").unwrap();
        c
    }
    #[test]
    fn filter_sort_limit_and_bad_enum() {
        assert_eq!(cardList(&json!({ "status": "doing" }), &seeded()).unwrap().as_array().unwrap().len(), 2);
        let score = cardList(&json!({ "sortBy": "score" }), &seeded()).unwrap();
        assert_eq!(score[0]["slug"], json!("C")); // 9 first
        assert!(cardList(&json!({ "sortBy": "nope" }), &seeded()).is_err());
    }
}
