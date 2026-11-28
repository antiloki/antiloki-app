//! card-update.rs — twin of skills/entities/card-update.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `cardUpdate` (default). Partial title/body/parent update with no-op guard; UPDATE + return row.
// QUALITY: 84/100 — provided-vs-existing merge, JSON.stringify body, no-op guard; datetime('now') frozen 2026-01-01
use rusqlite::Connection;
use serde_json::{json, Value};

// JS strict `===` for the primitive shapes that actually occur here (String / Null; Number/Bool
// for completeness). Strict: no cross-type coercion, so String vs Null is false.
fn js_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Bool(x), Value::Bool(y)) => x == y,
        _ => false,
    }
}

pub fn cardUpdate(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = match input.get("slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Err("card-update: slug is required".into()),
    };

    let title = input.get("title"); // Some(&Value) when the key exists (even if null) = JS "provided"
    let body = input.get("body");
    let parent = input.get("parent_slug");

    let title_provided = title.is_some();
    let body_provided = body.is_some();
    let parent_provided = parent.is_some();

    if !title_provided && !body_provided && !parent_provided {
        return Err("card-update: must provide at least one of: title, body, parent_slug".into());
    }
    if title_provided {
        // typeof title !== 'string' || title.length === 0
        let ok = title.unwrap().as_str().map_or(false, |s| !s.is_empty());
        if !ok {
            return Err("card-update: title must be a non-empty string when provided".into());
        }
    }

    let existing_rows = crate::db_json::query_rows(db, "SELECT * FROM cards WHERE slug = ?", &[json!(slug)])?;
    let existing_rows = existing_rows.as_array().unwrap();
    let existing = match existing_rows.first() {
        Some(r) => r,
        None => return Err(format!("card-update: card '{slug}' not found")),
    };

    // parent validation only when parent provided AND non-null (JS: parent_slug != null)
    if parent_provided {
        if let Some(p) = parent.filter(|v| !v.is_null()) {
            if p.as_str() == Some(slug) {
                return Err("card-update: card cannot be its own parent".into());
            }
            let prows = crate::db_json::query_rows(db, "SELECT slug FROM cards WHERE slug = ?", &[p.clone()])?;
            if prows.as_array().unwrap().is_empty() {
                let ps = p.as_str().map(|s| s.to_string()).unwrap_or_else(|| p.to_string());
                return Err(format!("card-update: parent card '{ps}' not found"));
            }
        }
    }

    let ex_title = existing.get("title").cloned().unwrap_or(Value::Null);
    let ex_body = existing.get("body_json").cloned().unwrap_or(Value::Null);
    let ex_parent = existing.get("parent_slug").cloned().unwrap_or(Value::Null);

    let new_title = if title_provided { title.unwrap().clone() } else { ex_title.clone() };
    let new_body_json = if body_provided {
        let b = body.unwrap();
        if b.is_null() { Value::Null } else { Value::String(serde_json::to_string(b).map_err(|e| e.to_string())?) }
    } else {
        ex_body.clone()
    };
    let new_parent = if parent_provided { parent.unwrap().clone() } else { ex_parent.clone() };

    if js_eq(&new_title, &ex_title) && js_eq(&new_body_json, &ex_body) && js_eq(&new_parent, &ex_parent) {
        return Err(format!("card-update: card '{slug}' update is a no-op (no fields changed)"));
    }

    // JS: UPDATE ... updated_at = datetime('now') — frozen-clock lane pins 'now' to the instant below.
    crate::db_json::run_write(
        db,
        &format!("UPDATE cards SET title = ?, body_json = ?, parent_slug = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
        &[new_title, new_body_json, new_parent, json!(slug)],
    )?;

    let rows = crate::db_json::query_rows(db, "SELECT * FROM cards WHERE slug = ?", &[json!(slug)])?;
    Ok(rows.as_array().unwrap().first().cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT, parent_slug TEXT, status TEXT, updated_at TEXT); \
             INSERT INTO cards VALUES ('C-1','old title','{\"kind\":\"feature\",\"size\":\"m\",\"why\":\"old reason\"}',NULL,'backlog','2025-12-01 00:00:00'); \
             INSERT INTO cards VALUES ('P-1','a parent',NULL,NULL,'backlog','2025-12-01 00:00:00');",
        ).unwrap();
        c
    }

    #[test]
    fn title_only_preserves_body_and_stamps() {
        let c = seeded();
        let r = cardUpdate(&json!({ "slug": "C-1", "title": "new title" }), &c).unwrap();
        assert_eq!(r["title"], json!("new title"));
        assert_eq!(r["body_json"], json!("{\"kind\":\"feature\",\"size\":\"m\",\"why\":\"old reason\"}"));
        assert_eq!(r["parent_slug"], Value::Null);
        assert_eq!(r["updated_at"], json!("2026-01-01 00:00:00"));
    }

    #[test]
    fn body_object_is_json_stringified() {
        let c = seeded();
        let r = cardUpdate(&json!({ "slug": "C-1", "body": { "kind": "bug", "size": "s", "why": "new reason" } }), &c).unwrap();
        assert_eq!(r["title"], json!("old title")); // preserved
        assert_eq!(r["body_json"], json!("{\"kind\":\"bug\",\"size\":\"s\",\"why\":\"new reason\"}"));
    }

    #[test]
    fn body_null_clears() {
        let c = seeded();
        let r = cardUpdate(&json!({ "slug": "C-1", "body": null }), &c).unwrap();
        assert_eq!(r["body_json"], Value::Null);
    }

    #[test]
    fn parent_set_after_existence_check() {
        let c = seeded();
        let r = cardUpdate(&json!({ "slug": "C-1", "parent_slug": "P-1" }), &c).unwrap();
        assert_eq!(r["parent_slug"], json!("P-1"));
        assert_eq!(r["title"], json!("old title"));
    }

    #[test]
    fn combined_update() {
        let c = seeded();
        let r = cardUpdate(&json!({ "slug": "C-1", "title": "combo", "body": { "kind": "epic", "size": "l", "why": "combo" }, "parent_slug": "P-1" }), &c).unwrap();
        assert_eq!(r["title"], json!("combo"));
        assert_eq!(r["body_json"], json!("{\"kind\":\"epic\",\"size\":\"l\",\"why\":\"combo\"}"));
        assert_eq!(r["parent_slug"], json!("P-1"));
    }

    #[test]
    fn guards() {
        let c = seeded();
        assert!(cardUpdate(&json!({ "title": "x" }), &c).is_err()); // missing slug
        assert!(cardUpdate(&json!({ "slug": "C-1" }), &c).is_err()); // nothing provided
        assert!(cardUpdate(&json!({ "slug": "C-1", "title": "" }), &c).is_err()); // empty title
        assert!(cardUpdate(&json!({ "slug": "NOPE", "title": "x" }), &c).is_err()); // not found
        assert!(cardUpdate(&json!({ "slug": "C-1", "title": "old title" }), &c).is_err()); // no-op
        assert!(cardUpdate(&json!({ "slug": "C-1", "parent_slug": "C-1" }), &c).is_err()); // self parent
        assert!(cardUpdate(&json!({ "slug": "C-1", "parent_slug": "GHOST" }), &c).is_err()); // parent not found
    }
}
