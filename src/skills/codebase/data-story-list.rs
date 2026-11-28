//! data-story-list.rs — twin of skills/codebase/data-story-list.mjs (db lane). NAMES ARE
//! CONTRACT: `dataStoryList` (named default). List + hop_count from hops_json.
// QUALITY: 85/100 — row remap with a JSON-column length; faithful to `?? '[]'` defaulting
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn dataStoryList(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas");
    let rows = crate::db_json::query_rows(db,
        "SELECT id, name, description, direction, hops_json, linked_test, updated_at FROM code_data_stories WHERE origin=? ORDER BY id DESC",
        &[json!(origin)])?;
    let stories: Vec<Value> = rows.as_array().unwrap().iter().map(|r| {
        let o = r.as_object().unwrap();
        // JS: JSON.parse(r.hops_json ?? '[]') then .length; invalid → []
        let hop_count = o.get("hops_json").and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .and_then(|v| v.as_array().map(|a| a.len()))
            .unwrap_or(0);
        json!({
            "id": o["id"].clone(), "name": o["name"].clone(), "description": o["description"].clone(),
            "direction": o["direction"].clone(), "hop_count": hop_count,
            "linked_test": o["linked_test"].clone(), "updated_at": o["updated_at"].clone(),
        })
    }).collect();
    Ok(json!({ "stories": stories, "count": stories.len() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_data_stories (id INTEGER, name TEXT, description TEXT, direction TEXT, hops_json TEXT, linked_test TEXT, updated_at TEXT, origin TEXT);
            INSERT INTO code_data_stories VALUES (1,'s1','d','fwd','[1,2,3]','t.js','u1','atlas'),(2,'s2','e','rev',NULL,NULL,'u2','atlas');").unwrap();
        c
    }
    #[test]
    fn lists_with_hop_count() {
        let r = dataStoryList(&json!({}), &seeded()).unwrap();
        assert_eq!(r["count"], json!(2));
        assert_eq!(r["stories"][0]["id"], json!(2)); // id DESC
        assert_eq!(r["stories"][0]["hop_count"], json!(0)); // null hops
        assert_eq!(r["stories"][1]["hop_count"], json!(3));
    }
}
