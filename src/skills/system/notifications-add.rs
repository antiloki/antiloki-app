//! notifications-add.rs — twin of skills/system/notifications-add.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `notificationsAdd` (named default). INSERT a one-off operator notification.
// QUALITY: 87/100 — required kind/severity/source/message, severity enum (warn not warning), String() coerce
use rusqlite::Connection;
use serde_json::{json, Value};

fn sstr(v: &Value) -> Value {
    match v { Value::String(s) => json!(s), Value::Null => Value::Null, o => json!(o.to_string()) }
}

pub fn notificationsAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    let req = |k: &str| -> Result<&Value, String> {
        match input.get(k) {
            Some(v) if !v.is_null() && v.as_str() != Some("") => Ok(v),
            _ => Err(format!("notifications-add: {k} is required")),
        }
    };
    let kind = req("kind")?.clone();
    let severity = req("severity")?.clone();
    let sev = severity.as_str().unwrap_or("");
    if !matches!(sev, "error" | "warn" | "info") {
        return Err(format!("notifications-add: severity must be one of: error, warn, info (got '{sev}')"));
    }
    let source = req("source")?.clone();
    let message = req("message")?.clone();
    // optional: null unless truthy
    let opt = |k: &str| -> Value { match input.get(k) { Some(v) if !v.is_null() && v.as_str() != Some("") => sstr(v), _ => Value::Null } };

    let (_c, id) = crate::db_json::run_write(db,
        "INSERT INTO notifications (kind, severity, source, message, related_card_slug, payload_json) VALUES (?, ?, ?, ?, ?, ?)",
        &[sstr(&kind), sstr(&severity), sstr(&source), sstr(&message), opt("related_card_slug"), opt("payload_json")])?;
    Ok(json!({ "id": id }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inserts_and_validates() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE notifications (id INTEGER PRIMARY KEY, kind TEXT, severity TEXT, source TEXT, message TEXT, related_card_slug TEXT, payload_json TEXT)").unwrap();
        assert_eq!(notificationsAdd(&json!({ "kind": "k", "severity": "warn", "source": "s", "message": "m" }), &c).unwrap()["id"], json!(1));
        assert!(notificationsAdd(&json!({ "kind": "k", "severity": "warning", "source": "s", "message": "m" }), &c).unwrap_err().contains("severity must be one of"));
        assert!(notificationsAdd(&json!({ "severity": "info", "source": "s", "message": "m" }), &c).unwrap_err().contains("kind is required"));
    }
}
