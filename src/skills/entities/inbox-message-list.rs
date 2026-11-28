//! inbox-message-list.rs — twin of skills/entities/inbox-message-list.mjs (db READ lane). NAMES ARE
//! CONTRACT: `inboxMessageList` (named default). Filtered inbox read + total/unread/archived counts.
// QUALITY: 82/100 — dynamic WHERE (archived/unread/since/card), counts via SUM(CASE) coalesced to 0
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn inboxMessageList(input: &Value, db: &Connection) -> Result<Value, String> {
    let archived = input.get("archived").map(|v| v == &Value::Bool(true)).unwrap_or(false);
    let unread_only = input.get("unread_only").map(|v| v == &Value::Bool(true)).unwrap_or(false);
    let limit = input.get("limit").and_then(|v| v.as_i64()).unwrap_or(100);

    let mut wh: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if !archived { wh.push("archived_at IS NULL"); }
    if unread_only { wh.push("read_at IS NULL"); }
    if let Some(since) = input.get("since_ts").filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false)) {
        wh.push("ts >= ?"); params.push(since.clone());
    }
    if let Some(cs) = input.get("card_slug").filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false)) {
        wh.push("related_card_slug = ?"); params.push(cs.clone());
    }
    let where_clause = if wh.is_empty() { String::new() } else { format!("WHERE {}", wh.join(" AND ")) };
    let sql = format!(
        "SELECT id, ts, kind, subject, body_md, severity, related_card_slug, related_contract_slug, \
           suggested_reconciler, suggested_card_action, payload_json, read_at, archived_at \
         FROM inbox_messages {where_clause} ORDER BY id DESC LIMIT ?"
    );
    let mut ev_params = params.clone();
    ev_params.push(json!(limit));
    let messages = crate::db_json::query_rows(db, &sql, &ev_params)?;

    let counts = crate::db_json::query_rows(db,
        "SELECT COUNT(*) AS total, \
           SUM(CASE WHEN read_at IS NULL AND archived_at IS NULL THEN 1 ELSE 0 END) AS unread, \
           SUM(CASE WHEN archived_at IS NOT NULL THEN 1 ELSE 0 END) AS archived FROM inbox_messages", &[])?;
    let c0 = counts.as_array().and_then(|a| a.first()).cloned().unwrap_or(json!({}));
    let g = |k: &str| c0.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    Ok(json!({ "messages": messages, "counts": { "total": g("total"), "unread": g("unread"), "archived": g("archived") } }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lists_and_counts() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE inbox_messages (id INTEGER PRIMARY KEY, ts TEXT, kind TEXT, subject TEXT, body_md TEXT, severity TEXT, related_card_slug TEXT, related_contract_slug TEXT, suggested_reconciler TEXT, suggested_card_action TEXT, payload_json TEXT, read_at TEXT, archived_at TEXT); \
          INSERT INTO inbox_messages (id, kind, subject) VALUES (1,'k','a'),(2,'k','b'); \
          UPDATE inbox_messages SET read_at='t' WHERE id=1;").unwrap();
        let r = inboxMessageList(&json!({}), &c).unwrap();
        assert_eq!(r["messages"].as_array().unwrap().len(), 2);
        assert_eq!(r["messages"][0]["id"], json!(2)); // id DESC
        assert_eq!(r["counts"]["total"], json!(2));
        assert_eq!(r["counts"]["unread"], json!(1));
        let u = inboxMessageList(&json!({ "unread_only": true }), &c).unwrap();
        assert_eq!(u["messages"].as_array().unwrap().len(), 1);
    }
}
