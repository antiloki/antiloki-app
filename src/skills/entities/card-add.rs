//! card-add.rs — twin of skills/entities/card-add.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `cardAdd` (named default). INSERT a card, normalizing body to exactly one level of JSON encoding.
// QUALITY: 86/100 — double-encode defense (parse-then-restringify), parent NOT_FOUND, SELECT * back
use rusqlite::Connection;
use serde_json::{json, Value};

// FIX-CARD-ADD-DOUBLE-ENCODE-DEFENSE: store body as exactly one level of JSON encoding.
fn normalize_body(body: Option<&Value>) -> Value {
    match body {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if !trimmed.is_empty() && (trimmed.starts_with('{') || trimmed.starts_with('[')) {
                if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                    return Value::String(serde_json::to_string(&parsed).unwrap()); // re-serialize once
                }
            }
            Value::String(serde_json::to_string(s).unwrap()) // plain text → one level of encoding
        }
        Some(v) => Value::String(serde_json::to_string(v).unwrap()), // object/array → stringify once
    }
}

pub fn cardAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = input.get("slug").cloned().unwrap_or(Value::Null);
    if slug.as_str().map(|s| s.is_empty()).unwrap_or(true) { return Err("card-add: slug is required".into()); }
    let title = input.get("title").cloned().unwrap_or(Value::Null);
    if title.as_str().map(|s| s.is_empty()).unwrap_or(true) { return Err("card-add: title is required".into()); }
    let status = input.get("status").cloned().unwrap_or(json!("backlog"));
    let parent = input.get("parent_slug").cloned().unwrap_or(Value::Null);

    if parent.as_str().map(|s| !s.is_empty()).unwrap_or(false) {
        let p = crate::db_json::query_rows(db, "SELECT slug FROM cards WHERE slug = ?", &[parent.clone()])?;
        if p.as_array().map(|a| a.is_empty()).unwrap_or(true) {
            return Err(format!("card-add: parent card '{}' not found", parent.as_str().unwrap()));
        }
    }

    crate::db_json::run_write(db,
        "INSERT INTO cards (slug, title, status, body_json, parent_slug) VALUES (?, ?, ?, ?, ?)",
        &[slug.clone(), title, status, normalize_body(input.get("body")), parent])?;
    let rows = crate::db_json::query_rows(db, "SELECT * FROM cards WHERE slug = ?", &[slug])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inserts_and_normalizes_body() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT UNIQUE, title TEXT, status TEXT, body_json TEXT, parent_slug TEXT)").unwrap();
        let r = cardAdd(&json!({ "slug": "a", "title": "A", "body": { "description": "hi", "tags": ["x"] } }), &c).unwrap();
        assert_eq!(r["status"], json!("backlog"));
        assert_eq!(r["body_json"], json!(r#"{"description":"hi","tags":["x"]}"#));
        assert!(cardAdd(&json!({ "slug": "a", "title": "dup" }), &c).unwrap_err().contains("UNIQUE"));
        assert!(cardAdd(&json!({ "title": "no slug" }), &c).is_err());
    }
}
