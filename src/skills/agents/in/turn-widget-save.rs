//! turn-widget-save.rs — twin of skills/agents/in/turn-widget-save.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `turnWidgetSave` (named default). Persist a widget/artifact to its chat session.
// QUALITY: 80/100 — session-from-turn derive, type/kind/payload validation, JSON.stringify + size cap
use rusqlite::Connection;
use serde_json::{json, Value};

const MAX_PAYLOAD_CHARS: usize = 200_000;

// String() coercion for "(got '…')" messages.
fn jval(v: Option<&Value>) -> String {
    match v { Some(Value::String(s)) => s.clone(), Some(Value::Null) | None => "undefined".into(), Some(o) => o.to_string() }
}
// Number(): f64, NaN when non-numeric.
fn number_of(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) } }
        Value::Bool(b) => if *b { 1.0 } else { 0.0 },
        Value::Null => 0.0,
        _ => f64::NAN,
    }
}
fn positive_int(f: f64) -> Option<i64> { if f.is_finite() && f.fract() == 0.0 && f > 0.0 { Some(f as i64) } else { None } }

pub fn turnWidgetSave(input: &Value, db: &Connection) -> Result<Value, String> {
    // turn_id: optional positive integer
    let turn_id: Option<i64> = match input.get("turn_id") {
        Some(v) if !v.is_null() => match positive_int(number_of(v)) {
            Some(i) => Some(i),
            None => return Err(format!("turn-widget-save: turn_id must be one of: positive integer (got '{}')", jval(input.get("turn_id")))),
        },
        _ => None,
    };

    // session_id: given, else derived from turn
    let mut session_num: Option<f64> = match input.get("session_id") {
        Some(v) if !v.is_null() => Some(number_of(v)),
        _ => None,
    };
    if session_num.is_none() {
        if let Some(tid) = turn_id {
            let t = crate::db_json::query_rows(db, "SELECT session_id FROM interviewer_turns WHERE id = ?", &[json!(tid)])?;
            match t.as_array().and_then(|a| a.first()) {
                Some(r) => session_num = r.get("session_id").and_then(|v| v.as_f64()),
                None => return Err(format!("turn-widget-save: turn '{tid}' not found")),
            }
        }
    }
    let session_id = match session_num.and_then(positive_int) {
        Some(i) => i,
        None => return Err("turn-widget-save: session_id (or a turn_id to derive it) is required".into()),
    };
    let sess = crate::db_json::query_rows(db, "SELECT id FROM interviewer_sessions WHERE id = ?", &[json!(session_id)])?;
    if sess.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        return Err(format!("turn-widget-save: session '{session_id}' not found"));
    }

    // type ∈ {widget, artifact}
    let type_str = input.get("type").and_then(|v| v.as_str());
    if !matches!(type_str, Some("widget") | Some("artifact")) {
        return Err(format!("turn-widget-save: type must be one of: widget, artifact (got '{}')", jval(input.get("type"))));
    }
    let type_str = type_str.unwrap();

    let kind = input.get("kind").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if kind.is_empty() { return Err("turn-widget-save: kind is required".into()); }
    match input.get("payload") {
        None | Some(Value::Null) => return Err("turn-widget-save: payload is required".into()),
        _ => {}
    }
    let payload_json = serde_json::to_string(input.get("payload").unwrap()).map_err(|_| "turn-widget-save: payload (must be JSON-serializable) is required".to_string())?;
    if payload_json.chars().count() > MAX_PAYLOAD_CHARS {
        return Err(format!("turn-widget-save: payload must be one of: <= {MAX_PAYLOAD_CHARS} chars (got '{} chars')", payload_json.chars().count()));
    }

    let kind60: String = kind.chars().take(60).collect();
    let (_c, id) = crate::db_json::run_write(db,
        "INSERT INTO interviewer_session_widgets (session_id, turn_id, type, kind, payload_json) VALUES (?, ?, ?, ?, ?)",
        &[json!(session_id), turn_id.map(|i| json!(i)).unwrap_or(Value::Null), json!(type_str), json!(kind60), json!(payload_json)])?;

    Ok(json!({ "ok": true, "id": id, "session_id": session_id, "turn_id": turn_id.map(|i| json!(i)).unwrap_or(Value::Null) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seed() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE interviewer_sessions (id INTEGER PRIMARY KEY, user_identifier TEXT); \
          CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY, session_id INTEGER); \
          CREATE TABLE interviewer_session_widgets (id INTEGER PRIMARY KEY, session_id INTEGER, turn_id INTEGER, type TEXT, kind TEXT, payload_json TEXT); \
          INSERT INTO interviewer_sessions (id) VALUES (1); INSERT INTO interviewer_turns (id, session_id) VALUES (7, 1);").unwrap();
        c
    }
    #[test]
    fn saves_via_turn_derive_and_guards() {
        let c = seed();
        let r = turnWidgetSave(&json!({ "turn_id": 7, "type": "widget", "kind": "diagram", "payload": { "nodes": [] } }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["session_id"], json!(1));
        assert_eq!(r["turn_id"], json!(7));
        assert!(turnWidgetSave(&json!({ "turn_id": 999, "type": "widget", "kind": "d", "payload": {} }), &c).unwrap_err().contains("turn '999' not found"));
        assert!(turnWidgetSave(&json!({ "turn_id": 7, "type": "sticker", "kind": "d", "payload": {} }), &c).unwrap_err().contains("type must be one of"));
        assert!(turnWidgetSave(&json!({ "turn_id": 7, "type": "widget", "kind": "d" }), &c).unwrap_err().contains("payload is required"));
    }
}
