//! card-tags-list.rs — twin of skills/entities/card-tags-list.mjs (db lane). NAMES ARE CONTRACT:
//! `cardTagsList` (named default). Three modes: by card_slug, by tag_slug, else the tag dict.
// QUALITY: 86/100 — branch selection is the port; each query runs through db_json
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn cardTagsList(input: &Value, db: &Connection) -> Result<Value, String> {
    let card_slug = input.get("card_slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let tag_slug = input.get("tag_slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    if let Some(cs) = card_slug {
        return crate::db_json::query_rows(db,
            "SELECT t.slug, t.kind, t.description, ct.joined_at
             FROM card_tags ct JOIN tags t ON t.slug = ct.tag_slug
             WHERE ct.card_slug = ? ORDER BY t.slug",
            &[json!(cs)]);
    }
    if let Some(ts) = tag_slug {
        return crate::db_json::query_rows(db,
            "SELECT c.slug, c.title, c.status, ct.joined_at
             FROM card_tags ct JOIN cards c ON c.slug = ct.card_slug
             WHERE ct.tag_slug = ? ORDER BY c.slug",
            &[json!(ts)]);
    }
    crate::db_json::query_rows(db, "SELECT slug, kind, description FROM tags ORDER BY slug", &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("
            CREATE TABLE tags (slug TEXT, kind TEXT, description TEXT);
            CREATE TABLE cards (slug TEXT, title TEXT, status TEXT);
            CREATE TABLE card_tags (card_slug TEXT, tag_slug TEXT, joined_at TEXT);
            INSERT INTO tags VALUES ('urgent','pri','p'),('bug','kind','b');
            INSERT INTO cards VALUES ('C1','one','doing');
            INSERT INTO card_tags VALUES ('C1','urgent','t1');
        ").unwrap();
        c
    }
    #[test]
    fn three_modes() {
        assert_eq!(cardTagsList(&json!({}), &seeded()).unwrap().as_array().unwrap().len(), 2); // tag dict
        assert_eq!(cardTagsList(&json!({ "card_slug": "C1" }), &seeded()).unwrap()[0]["slug"], json!("urgent"));
        assert_eq!(cardTagsList(&json!({ "tag_slug": "urgent" }), &seeded()).unwrap()[0]["slug"], json!("C1"));
    }
}
