//! card-phase-event-record.rs — twin of skills/entities/card-phase-event-record.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `cardPhaseEventRecord` (default) + `KNOWN_PHASES` (named). Appends a lifecycle event.
// QUALITY: 85/100 — NOT_FOUND guard, evidence JSON.stringify (null → null column), known-phase flag
use rusqlite::Connection;
use serde_json::{json, Value};

pub const KNOWN_PHASES: [&str; 6] = ["pl_draft", "tl_sign", "dev_impl", "qa_pass", "qa_fail", "close"];

pub fn cardPhaseEventRecord(input: &Value, db: &Connection) -> Result<Value, String> {
    let req = |k: &str| -> Result<Value, String> {
        match input.get(k) {
            Some(v) if !v.is_null() && v.as_str() != Some("") => Ok(v.clone()),
            _ => Err(format!("card-phase-event-record: {k} is required")),
        }
    };
    let card_slug = req("card_slug")?;
    let phase = req("phase")?;
    let actor = req("actor")?;

    let card = crate::db_json::query_rows(db, "SELECT slug FROM cards WHERE slug = ?", &[card_slug.clone()])?;
    if card.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        let cs = card_slug.as_str().map(|s| s.to_string()).unwrap_or_else(|| card_slug.to_string());
        return Err(format!("card-phase-event-record: card '{cs}' not found"));
    }

    // evidence == null ? null : JSON.stringify(evidence)
    let evidence_json = match input.get("evidence") {
        Some(v) if !v.is_null() => Value::String(v.to_string()),
        _ => Value::Null,
    };
    let (_c, id) = crate::db_json::run_write(
        db,
        "INSERT INTO card_phase_events (card_slug, phase, actor, evidence_json) VALUES (?, ?, ?, ?)",
        &[card_slug.clone(), phase.clone(), actor.clone(), evidence_json],
    )?;

    let known = phase.as_str().map(|p| KNOWN_PHASES.contains(&p)).unwrap_or(false);
    Ok(json!({ "id": id, "card_slug": card_slug, "phase": phase, "actor": actor, "known_phase": known }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn records_and_guards() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (slug TEXT); CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY, card_slug TEXT, phase TEXT, actor TEXT, evidence_json TEXT); INSERT INTO cards VALUES ('C-1');").unwrap();
        let r = cardPhaseEventRecord(&json!({ "card_slug": "C-1", "phase": "qa_pass", "actor": "qa", "evidence": { "ok": true } }), &c).unwrap();
        assert_eq!(r["id"], json!(1));
        assert_eq!(r["known_phase"], json!(true));
        let ev: String = c.query_row("SELECT evidence_json FROM card_phase_events WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(ev, r#"{"ok":true}"#);
        assert!(cardPhaseEventRecord(&json!({ "card_slug": "NOPE", "phase": "x", "actor": "a" }), &c).unwrap_err().contains("not found"));
        assert!(cardPhaseEventRecord(&json!({ "phase": "x", "actor": "a" }), &c).is_err()); // no card_slug
    }
}
