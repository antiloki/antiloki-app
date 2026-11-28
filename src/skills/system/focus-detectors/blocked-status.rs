//! blocked-status.rs — twin of skills/system/focus-detectors/blocked-status.mjs.
//! NAMES ARE CONTRACT: the export is `detect` (the JS named default), exact signature intent:
//! a pure decision on the card row; the db argument is deliberately unused.
// QUALITY: 84/100 — wire key-presence semantics explicit; Map-building a bit verbose

use serde_json::{json, Map, Value};

/// JS: `detect(card, _db)` — fires only on status='blocked'. Key-presence mirrors JS:
/// a MISSING card.driver_intent stays missing (JSON.stringify drops undefined); an explicit
/// null stays null.
pub fn detect(card: &Value, _db: &Value) -> Value {
    if card.get("status").and_then(|v| v.as_str()) != Some("blocked") {
        return Value::Null;
    }
    let mut detail = Map::new();
    detail.insert("status".into(), card.get("status").cloned().unwrap_or(Value::Null));
    if let Some(di) = card.get("driver_intent") {
        detail.insert("driver_intent".into(), di.clone());
    }
    let reason_md = concat!(
        "Card status is 'blocked' — the scheduler treats this as terminal and ",
        "will never advance it, regardless of driver_intent. Investigate why ",
        "the card was blocked (recent QA fail, escalation, or operator action) ",
        "and resolve the cause, then move it back to backlog.",
    );
    let mut opt_a = Map::new();
    opt_a.insert("id".into(), json!("A"));
    opt_a.insert("label".into(), json!("Move card back to backlog (operator unblocks)"));
    opt_a.insert("action".into(), json!("set_status"));
    if let Some(slug) = card.get("slug") {
        opt_a.insert("card_slug".into(), slug.clone());
    }
    opt_a.insert("status".into(), json!("backlog"));
    let mut opt_b = Map::new();
    opt_b.insert("id".into(), json!("B"));
    opt_b.insert("label".into(), json!("Mark intent=stopped (give up on this card)"));
    opt_b.insert("action".into(), json!("set_intent"));
    if let Some(slug) = card.get("slug") {
        opt_b.insert("card_slug".into(), slug.clone());
    }
    opt_b.insert("intent".into(), json!("stopped"));
    json!({
        "kind": "blocked_status",
        "detail": detail,
        "reason_md": reason_md,
        "surfaces_in": ["board:red-badge", "pipeline:blocked-pane", "fixit:finding"],
        "fix_options": [opt_a, opt_b],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_only_on_blocked() {
        assert_eq!(detect(&json!({"status":"backlog"}), &Value::Null), Value::Null);
        let f = detect(&json!({"status":"blocked","driver_intent":"play","slug":"C9"}), &Value::Null);
        assert_eq!(f["kind"], json!("blocked_status"));
        assert_eq!(f["fix_options"][0]["card_slug"], json!("C9"));
        assert_eq!(f["fix_options"][1]["intent"], json!("stopped"));
    }

    #[test]
    fn missing_driver_intent_stays_missing() {
        let f = detect(&json!({"status":"blocked","slug":"X"}), &Value::Null);
        assert!(f["detail"].get("driver_intent").is_none(), "stringify-drop semantics");
    }
}
