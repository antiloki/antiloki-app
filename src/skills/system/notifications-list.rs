//! notifications-list.rs — twin of skills/system/notifications-list.mjs (db READ lane). NAMES ARE
//! CONTRACT: `notificationsList` (named default). Paginated notifications, normalized to the FE shape.
// QUALITY: 82/100 — dynamic WHERE, table-absent → [], FE-shape normalize (message_md, contract_slug)
use rusqlite::Connection;
use serde_json::{json, Value};

fn number_or(v: Option<&Value>, dflt: f64) -> f64 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(dflt),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(dflt),
        _ => dflt,
    }
}

pub fn notificationsList(input: &Value, db: &Connection) -> Result<Value, String> {
    let include_dismissed = input.get("include_dismissed") == Some(&Value::Bool(true));
    let severity = input.get("severity").and_then(|v| v.as_str()).unwrap_or("all").to_string();
    let card_slug = input.get("card_slug").filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false));
    // Number(limit)||50 then clamp [1,200]
    let raw = number_or(input.get("limit"), 50.0);
    let limit = (if raw == 0.0 || raw.is_nan() { 50.0 } else { raw }).max(1.0).min(200.0) as i64;

    let mut wh: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if !include_dismissed { wh.push("dismissed_at IS NULL"); }
    if severity != "all" && !severity.is_empty() { wh.push("severity = ?"); params.push(json!(severity)); }
    if let Some(cs) = card_slug { wh.push("related_card_slug = ?"); params.push(json!(cs.as_str().unwrap())); }
    let where_clause = if wh.is_empty() { String::new() } else { format!("WHERE {}", wh.join(" AND ")) };

    let sql = format!(
        "SELECT id, ts, kind, severity, source, message, related_card_slug, payload_json, dismissed_at \
         FROM notifications {where_clause} ORDER BY id DESC LIMIT ?"
    );
    params.push(json!(limit));
    let rows = match crate::db_json::query_rows(db, &sql, &params) {
        Ok(r) => r,
        Err(_) => return Ok(json!([])), // table may not exist on a fresh DB
    };

    let out: Vec<Value> = rows.as_array().unwrap().iter().map(|r| {
        let o = r.as_object().unwrap();
        let g = |k: &str| o.get(k).cloned().unwrap_or(Value::Null);
        let contract_slug = o.get("payload_json").and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .and_then(|p| p.get("contract_slug").cloned())
            .unwrap_or(Value::Null);
        json!({
            "id": g("id"), "ts": g("ts"), "kind": g("kind"), "severity": g("severity"),
            "message_md": g("message"), "card_slug": g("related_card_slug"),
            "contract_slug": contract_slug, "payload_json": g("payload_json"), "dismissed_at": g("dismissed_at"),
        })
    }).collect();
    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lists_normalized() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE notifications (id INTEGER PRIMARY KEY, ts TEXT, kind TEXT, severity TEXT, source TEXT, message TEXT, related_card_slug TEXT, payload_json TEXT, dismissed_at TEXT); \
          INSERT INTO notifications (id,kind,severity,message,related_card_slug,payload_json) VALUES (1,'k','error','boom','C-1','{\"contract_slug\":\"CT-1\"}'),(2,'k','info','hi',NULL,NULL);").unwrap();
        let r = notificationsList(&json!({}), &c).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 2);
        assert_eq!(r[0]["id"], json!(2)); // id DESC
        assert_eq!(r[1]["message_md"], json!("boom"));
        assert_eq!(r[1]["contract_slug"], json!("CT-1"));
        assert_eq!(notificationsList(&json!({ "severity": "error" }), &c).as_ref().unwrap().as_array().unwrap().len(), 1);
    }
}
