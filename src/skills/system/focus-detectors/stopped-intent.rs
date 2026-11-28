//! stopped-intent.rs — twin of skills/system/focus-detectors/stopped-intent.mjs.
//! NAMES ARE CONTRACT: the export is `detect` (the JS named default). Simpler than
//! blocked-status: `detail` is a fixed literal, not built conditionally from card fields.
// QUALITY: 86/100 — near-identical to the already-proven blocked-status pattern; simpler (fixed detail)

use serde_json::{json, Map, Value};

/// JS: `detect(card, _db)` — fires only on driver_intent='stopped'. `card_slug` mirrors
/// JSON-wire key-presence: a MISSING card.slug stays missing in fix_options.
pub fn detect(card: &Value, _db: &Value) -> Value {
    if card.get("driver_intent").and_then(|v| v.as_str()) != Some("stopped") {
        return Value::Null;
    }
    let reason_md = concat!(
        "Operator set driver_intent=stopped on this card. The scheduler will ",
        "not pick it up. If you want work to resume, change intent to play; ",
        "if you want the card closed entirely, archive it.",
    );
    let mut opt_a = Map::new();
    opt_a.insert("id".into(), json!("A"));
    opt_a.insert("label".into(), json!("Resume play (driver_intent=play)"));
    opt_a.insert("action".into(), json!("set_intent"));
    if let Some(slug) = card.get("slug") {
        opt_a.insert("card_slug".into(), slug.clone());
    }
    opt_a.insert("intent".into(), json!("play"));
    let mut opt_b = Map::new();
    opt_b.insert("id".into(), json!("B"));
    opt_b.insert("label".into(), json!("Archive card"));
    opt_b.insert("action".into(), json!("set_status"));
    if let Some(slug) = card.get("slug") {
        opt_b.insert("card_slug".into(), slug.clone());
    }
    opt_b.insert("status".into(), json!("archived"));
    json!({
        "kind": "stopped_intent",
        "detail": { "driver_intent": "stopped" },
        "reason_md": reason_md,
        "surfaces_in": ["board:red-badge", "fixit:finding"],
        "fix_options": [opt_a, opt_b],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_only_on_stopped() {
        assert_eq!(detect(&json!({"driver_intent":"play"}), &Value::Null), Value::Null);
        assert_eq!(detect(&json!({}), &Value::Null), Value::Null);
        let f = detect(&json!({"driver_intent":"stopped","slug":"CARD-9"}), &Value::Null);
        assert_eq!(f["kind"], json!("stopped_intent"));
        assert_eq!(f["fix_options"][0]["card_slug"], json!("CARD-9"));
        assert_eq!(f["fix_options"][1]["status"], json!("archived"));
    }

    #[test]
    fn missing_slug_stays_missing() {
        let f = detect(&json!({"driver_intent":"stopped"}), &Value::Null);
        assert!(f["fix_options"][0].get("card_slug").is_none());
        assert!(f["fix_options"][1].get("card_slug").is_none());
    }
}
