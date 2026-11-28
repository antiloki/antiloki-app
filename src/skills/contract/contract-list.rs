//! contract-list.rs — twin of skills/contract/contract-list.mjs (db lane). NAMES ARE CONTRACT:
//! `contractList` (named default). Filtered read with status/tier enum guards.
// QUALITY: 87/100 — enum validation + where-building + limit clamp
use rusqlite::Connection;
use serde_json::{json, Value};

const VALID_STATUSES: [&str; 8] = ["proposed", "tl_approved", "human_approved", "impl", "qa", "done", "rejected", "superseded"];
const VALID_TIERS: [&str; 3] = ["trivial", "standard", "high-risk"];

pub fn contractList(input: &Value, db: &Connection) -> Result<Value, String> {
    if let Some(s) = input.get("status").filter(|v| !v.is_null()).and_then(|v| v.as_str()) {
        if !VALID_STATUSES.contains(&s) {
            return Err(format!("contract-list: status must be one of: {} (got '{s}')", VALID_STATUSES.join(", ")));
        }
    }
    if let Some(t) = input.get("tier").filter(|v| !v.is_null()).and_then(|v| v.as_str()) {
        if !VALID_TIERS.contains(&t) {
            return Err(format!("contract-list: tier must be one of: {} (got '{t}')", VALID_TIERS.join(", ")));
        }
    }
    let mut wheres: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(s) = input.get("status").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) { wheres.push("status = ?"); params.push(json!(s)); }
    if let Some(t) = input.get("tier").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) { wheres.push("tier = ?"); params.push(json!(t)); }
    if let Some(cs) = input.get("card_slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) { wheres.push("card_slug = ?"); params.push(json!(cs)); }
    if !input.get("include_superseded").and_then(Value::as_bool).unwrap_or(false) { wheres.push("superseded_by IS NULL"); }
    let where_ = if wheres.is_empty() { String::new() } else { format!("WHERE {}", wheres.join(" AND ")) };
    // JS: Math.max(1, Math.min(1000, limit | 0)) — |0 truncates
    let limit = (input.get("limit").and_then(|v| v.as_f64()).unwrap_or(100.0) as i64).clamp(1, 1000);
    params.push(json!(limit));
    let rows = crate::db_json::query_rows(db, &format!(
        "SELECT slug, title, status, template_kind, size, complexity, risk, tier, card_slug, current_attempt, superseded_by, signed_by, signed_at, created_at, updated_at FROM contracts {where_} ORDER BY status ASC, created_at DESC LIMIT ?"),
        &params)?;
    Ok(json!({ "count": rows.as_array().unwrap().len(), "contracts": rows }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters_and_enum() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE contracts (slug TEXT, title TEXT, status TEXT, template_kind TEXT, size TEXT, complexity TEXT, risk TEXT, tier TEXT, card_slug TEXT, current_attempt INTEGER, superseded_by TEXT, signed_by TEXT, signed_at TEXT, created_at TEXT, updated_at TEXT);
            INSERT INTO contracts (slug,status,tier,created_at) VALUES ('A','impl','standard','t1'),('B','done','trivial','t2');").unwrap();
        assert_eq!(contractList(&json!({}), &c).unwrap()["count"], json!(2));
        assert_eq!(contractList(&json!({ "status": "impl" }), &c).unwrap()["count"], json!(1));
        assert!(contractList(&json!({ "status": "bogus" }), &c).is_err());
    }
}
