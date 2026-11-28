//! inbox-message-add.rs — twin of skills/entities/inbox-message-add.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `inboxMessageAdd` (named default). File a rich attention message; returns { id, ts }.
// QUALITY: 85/100 — required kind/subject/body, severity enum (join '/'), payload stringify, id+ts read-back
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn inboxMessageAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    let req = |k: &str| -> Result<Value, String> {
        match input.get(k) { Some(v) if !v.is_null() && v.as_str() != Some("") => Ok(v.clone()), _ => Err(format!("inbox-message-add: {k} is required")) }
    };
    let kind = req("kind")?;
    let subject = req("subject")?;
    let body_md = req("body_md")?;
    let severity = input.get("severity").and_then(|v| v.as_str()).unwrap_or("info").to_string();
    if !matches!(severity.as_str(), "info" | "warn" | "error") {
        return Err(format!("inbox-message-add: severity must be one of info/warn/error (got '{severity}')"));
    }
    let opt = |k: &str| input.get(k).cloned().unwrap_or(Value::Null);
    // payload_json == null ? null : (string ? itself : JSON.stringify)
    let payload = match input.get("payload_json") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(s)) => json!(s),
        Some(v) => json!(serde_json::to_string(v).unwrap()),
    };

    let (_c, id) = crate::db_json::run_write(db,
        "INSERT INTO inbox_messages (kind, subject, body_md, severity, related_card_slug, related_contract_slug, suggested_reconciler, suggested_card_action, payload_json) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[kind, subject, body_md, json!(severity), opt("related_card_slug"), opt("related_contract_slug"), opt("suggested_reconciler"), opt("suggested_card_action"), payload])?;
    let row = crate::db_json::query_rows(db, "SELECT id, ts FROM inbox_messages WHERE id = ?", &[json!(id)])?;
    let ts = row.as_array().and_then(|a| a.first()).and_then(|r| r.get("ts")).cloned().unwrap_or(Value::Null);
    Ok(json!({ "id": id, "ts": ts }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inserts_and_validates() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE inbox_messages (id INTEGER PRIMARY KEY, ts TEXT, kind TEXT, subject TEXT, body_md TEXT, severity TEXT, related_card_slug TEXT, related_contract_slug TEXT, suggested_reconciler TEXT, suggested_card_action TEXT, payload_json TEXT)").unwrap();
        let r = inboxMessageAdd(&json!({ "kind": "generic", "subject": "s", "body_md": "b", "payload_json": { "x": 1 } }), &c).unwrap();
        assert_eq!(r["id"], json!(1));
        assert_eq!(r["ts"], Value::Null); // no datetime default in this schema
        let pj: String = c.query_row("SELECT payload_json FROM inbox_messages WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(pj, r#"{"x":1}"#);
        assert!(inboxMessageAdd(&json!({ "kind": "k", "subject": "s", "body_md": "b", "severity": "warning" }), &c).unwrap_err().contains("info/warn/error"));
        assert!(inboxMessageAdd(&json!({ "subject": "s", "body_md": "b" }), &c).unwrap_err().contains("kind is required"));
    }
}
