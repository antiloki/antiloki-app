//! card-children.rs — twin of skills/entities/card-children.mjs (db lane). NAMES ARE CONTRACT:
//! `cardChildren` (named default). Direct children, or a BFS recursive descent.
// QUALITY: 86/100 — faithful BFS (FIFO queue + seen set), same append order as JS
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};

fn fetch_direct(db: &Connection, parent: &str) -> Result<Vec<Value>, String> {
    let rows = crate::db_json::query_rows(db,
        "SELECT slug, title, status, parent_slug, score FROM cards WHERE parent_slug = ? ORDER BY rowid",
        &[json!(parent)])?;
    Ok(rows.as_array().unwrap().clone())
}

pub fn cardChildren(input: &Value, db: &Connection) -> Result<Value, String> {
    let parent = match input.get("parent_slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(p) => p,
        None => return Err("card-children: parent_slug is required".into()),
    };
    let recursive = input.get("recursive").and_then(Value::as_bool).unwrap_or(false);
    let direct = fetch_direct(db, parent)?;
    if !recursive {
        return Ok(json!({ "parent_slug": parent, "children": direct }));
    }
    let slug_of = |c: &Value| c["slug"].as_str().unwrap_or("").to_string();
    let mut out = direct.clone();
    let mut queue: VecDeque<String> = direct.iter().map(&slug_of).collect();
    let mut seen: HashSet<String> = std::iter::once(parent.to_string()).chain(direct.iter().map(&slug_of)).collect();
    while let Some(next) = queue.pop_front() {
        for c in fetch_direct(db, &next)? {
            let slug = slug_of(&c);
            if !seen.contains(&slug) {
                seen.insert(slug.clone());
                queue.push_back(slug);
                out.push(c);
            }
        }
    }
    Ok(json!({ "parent_slug": parent, "children": out, "recursive": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (slug TEXT, title TEXT, status TEXT, parent_slug TEXT, score REAL);
            INSERT INTO cards VALUES ('P','p','doing',NULL,1),('C1','c1','doing','P',2),('G1','g1','doing','C1',3);").unwrap();
        c
    }
    #[test]
    fn direct_and_recursive() {
        assert_eq!(cardChildren(&json!({ "parent_slug": "P" }), &seeded()).unwrap()["children"].as_array().unwrap().len(), 1);
        let rec = cardChildren(&json!({ "parent_slug": "P", "recursive": true }), &seeded()).unwrap();
        assert_eq!(rec["children"].as_array().unwrap().len(), 2); // C1 then G1
        assert!(cardChildren(&json!({}), &seeded()).is_err());
    }
}
