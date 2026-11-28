//! ui-stories-get.rs — twin of skills/codebase/ui-stories-get.mjs (db lane). NAMES ARE CONTRACT:
//! `uiStoriesGet` (named default). Defensive list (table-absent → []) + parsed routes/steps.
// QUALITY: 85/100 — table-absent catch + JSON-column parse-or-[] (incl. "null" → [])
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn parse_or(o: &Map<String, Value>, key: &str, default: Value) -> Value {
    // JS: parse(s) = try JSON.parse(s ?? 'null') catch null ; then `?? []`
    o.get(key).and_then(|v| v.as_str())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .filter(|v| !v.is_null())
        .unwrap_or(default)
}

pub fn uiStoriesGet(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas");
    // JS wraps the query in try/catch: table absent → []
    let rows = crate::db_json::query_rows(db,
        "SELECT id, name, url, source, step_count, routes_json, steps_json, created_at FROM code_user_stories WHERE origin = ? ORDER BY created_at DESC, id DESC",
        &[json!(origin)]).unwrap_or_else(|_| json!([]));
    let stories: Vec<Value> = rows.as_array().unwrap().iter().map(|r| {
        let o = r.as_object().unwrap();
        json!({
            "id": o["id"].clone(), "name": o["name"].clone(), "url": o["url"].clone(),
            "source": o["source"].clone(), "step_count": o["step_count"].clone(),
            "routes": parse_or(o, "routes_json", json!([])),
            "steps": parse_or(o, "steps_json", json!([])),
            "created_at": o["created_at"].clone(),
        })
    }).collect();
    Ok(json!({ "ok": true, "origin": origin, "stories": stories }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_user_stories (id INTEGER, name TEXT, url TEXT, source TEXT, step_count INTEGER, routes_json TEXT, steps_json TEXT, created_at TEXT, origin TEXT);
            INSERT INTO code_user_stories VALUES (1,'s','/u','fe',2,'[\"/a\"]',NULL,'t1','atlas');").unwrap();
        c
    }
    #[test]
    fn parses_routes_defaults_steps() {
        let r = uiStoriesGet(&json!({}), &seeded()).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["stories"][0]["routes"], json!(["/a"]));
        assert_eq!(r["stories"][0]["steps"], json!([])); // null → []
    }
}
