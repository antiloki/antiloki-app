//! card-tag-add.rs — twin of skills/entities/card-tag-add.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `cardTagAdd` (named default). Validate card+tag exist, INSERT, catch UNIQUE → already-tagged.
// QUALITY: 86/100 — write with a constraint-violation branch matching JS's UNIQUE/PRIMARY KEY catch
use rusqlite::{params, Connection};
use serde_json::{json, Value};

pub fn cardTagAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    let card_slug = match input.get("card_slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Err("card-tag-add: card_slug is required".into()),
    };
    let tag_slug = match input.get("tag_slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Err("card-tag-add: tag_slug is required".into()),
    };
    if crate::db_json::query_rows(db, "SELECT slug FROM cards WHERE slug = ?", &[json!(card_slug)])?.as_array().unwrap().is_empty() {
        return Err(format!("card-tag-add: card '{card_slug}' not found"));
    }
    if crate::db_json::query_rows(db, "SELECT slug FROM tags WHERE slug = ?", &[json!(tag_slug)])?.as_array().unwrap().is_empty() {
        return Err(format!("card-tag-add: tag '{tag_slug}' not in dictionary (add it to tags table first)"));
    }
    match db.execute("INSERT INTO card_tags (card_slug, tag_slug) VALUES (?1, ?2)", params![card_slug, tag_slug]) {
        Ok(_) => Ok(json!({ "card_slug": card_slug, "tag_slug": tag_slug, "added": true })),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("UNIQUE") || msg.contains("PRIMARY KEY") {
                Ok(json!({ "card_slug": card_slug, "tag_slug": tag_slug, "added": false, "reason": "already tagged" }))
            } else {
                Err(msg)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (slug TEXT); CREATE TABLE tags (slug TEXT);
            CREATE TABLE card_tags (card_slug TEXT, tag_slug TEXT, PRIMARY KEY (card_slug, tag_slug));
            INSERT INTO cards VALUES ('C1'); INSERT INTO tags VALUES ('urgent');").unwrap();
        c
    }
    #[test]
    fn add_dupe_and_missing() {
        let c = seeded();
        assert_eq!(cardTagAdd(&json!({ "card_slug": "C1", "tag_slug": "urgent" }), &c).unwrap()["added"], json!(true));
        assert_eq!(cardTagAdd(&json!({ "card_slug": "C1", "tag_slug": "urgent" }), &c).unwrap()["added"], json!(false)); // dup
        assert!(cardTagAdd(&json!({ "card_slug": "ghost", "tag_slug": "urgent" }), &c).is_err());
        assert!(cardTagAdd(&json!({ "card_slug": "C1", "tag_slug": "nope" }), &c).is_err());
    }
}
