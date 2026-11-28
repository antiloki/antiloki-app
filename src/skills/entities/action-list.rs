//! action-list.rs — twin of skills/entities/action-list.mjs (db lane). NAMES ARE CONTRACT: `actionList`.
// QUALITY: 87/100 — kind enum + filters + limit; {count, actions} envelope
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn actionList(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: if (kind != null && !VALID_KINDS.has(kind)) throw
    if let Some(k) = input.get("kind").filter(|v| !v.is_null()).and_then(|v| v.as_str()) {
        if !["atomic", "workflow"].contains(&k) {
            return Err(format!("action-list: kind must be one of: atomic, workflow (got '{k}')"));
        }
    }
    let mut where_parts: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(k) = input.get("kind").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        where_parts.push("kind = ?");
        params.push(json!(k));
    }
    if let Some(o) = input.get("owner_agent").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        where_parts.push("owner_agent = ?");
        params.push(json!(o));
    }
    let where_clause = if where_parts.is_empty() { String::new() } else { format!("WHERE {}", where_parts.join(" AND ")) };
    // JS: Number(limit) > 0 ? Number(limit) : 50
    let limit = input.get("limit").and_then(|v| v.as_f64()).filter(|n| *n > 0.0).unwrap_or(50.0) as i64;
    let sql = format!("SELECT name, description, owner_agent, kind FROM actions {where_clause} ORDER BY name ASC LIMIT ?");
    params.push(json!(limit));
    let rows = crate::db_json::query_rows(db, &sql, &params)?;
    Ok(json!({ "count": rows.as_array().unwrap().len(), "actions": rows }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE actions (name TEXT, description TEXT, owner_agent TEXT, kind TEXT);
            INSERT INTO actions VALUES ('b.x','X','system','atomic'),('a.y','Y','god','workflow');").unwrap();
        c
    }
    #[test]
    fn lists_filters_and_bad_kind() {
        let all = actionList(&json!({}), &seeded()).unwrap();
        assert_eq!(all["count"], json!(2));
        assert_eq!(all["actions"][0]["name"], json!("a.y")); // name ASC
        assert_eq!(actionList(&json!({ "kind": "workflow" }), &seeded()).unwrap()["count"], json!(1));
        assert!(actionList(&json!({ "kind": "bad" }), &seeded()).is_err());
    }
}
