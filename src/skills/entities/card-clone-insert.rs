//! card-clone-insert.rs — twin of skills/entities/card-clone-insert.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `cardCloneInsert` (named default). Raw INSERT of a clone card + SELECT * back.
// QUALITY: 88/100 — 10 bound params with JS destructure defaults (body_json null, acceptance_json '[]')
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn cardCloneInsert(input: &Value, db: &Connection) -> Result<Value, String> {
    let get = |k: &str, dflt: Value| input.get(k).cloned().unwrap_or(dflt);
    let slug = get("slug", Value::Null);
    if slug.as_str().map(|s| s.is_empty()).unwrap_or(true) {
        return Err("card-clone-insert: slug is required".into());
    }
    let title = get("title", Value::Null);
    if title.as_str().map(|s| s.is_empty()).unwrap_or(true) {
        return Err("card-clone-insert: title is required".into());
    }
    let params = &[
        slug.clone(),
        title.clone(),
        get("body_json", Value::Null),
        get("acceptance_json", json!("[]")),
        get("risk", Value::Null),
        get("complexity", Value::Null),
        get("priority", Value::Null),
        get("score", Value::Null),
        get("parent_slug", Value::Null),
        get("cloned_from", Value::Null),
    ];
    crate::db_json::run_write(
        db,
        "INSERT INTO cards (slug, title, status, body_json, acceptance_json, risk, complexity, priority, score, parent_slug, cloned_from) \
         VALUES (?, ?, 'backlog', ?, ?, ?, ?, ?, ?, ?, ?)",
        params,
    )?;
    let rows = crate::db_json::query_rows(db, "SELECT * FROM cards WHERE slug = ?", &[slug])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inserts_clone_with_clean_state() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT, title TEXT, status TEXT, body_json TEXT, acceptance_json TEXT, risk TEXT, complexity TEXT, priority INTEGER, score REAL, parent_slug TEXT, cloned_from TEXT)").unwrap();
        let r = cardCloneInsert(&json!({ "slug": "C-2", "title": "clone", "cloned_from": "C-1" }), &c).unwrap();
        assert_eq!(r["status"], json!("backlog"));
        assert_eq!(r["acceptance_json"], json!("[]")); // default
        assert_eq!(r["cloned_from"], json!("C-1"));
        assert!(cardCloneInsert(&json!({ "title": "x" }), &c).is_err()); // no slug
    }
}
