//! sprint-set-item-phase.rs — twin of skills/entities/sprint-set-item-phase.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `sprintSetItemPhase` (named default). UPDATE + NOT_FOUND on 0 changes.
// QUALITY: 87/100 — enum guard + UPDATE + changes==0 → NOT_FOUND
use rusqlite::{params, Connection};
use serde_json::{json, Value};

pub fn sprintSetItemPhase(input: &Value, db: &Connection) -> Result<Value, String> {
    let sprint = match input.get("sprint_slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s, None => return Err("sprint-set-item-phase: sprint_slug is required".into()),
    };
    let card = match input.get("card_slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s, None => return Err("sprint-set-item-phase: card_slug is required".into()),
    };
    let phase = input.get("phase").and_then(|v| v.as_str()).unwrap_or("");
    if !["impl", "qa", "done"].contains(&phase) {
        return Err(format!("sprint-set-item-phase: phase must be one of: impl, qa, done (got '{phase}')"));
    }
    let changes = db.execute("UPDATE sprint_items SET phase = ?1 WHERE sprint_slug = ?2 AND card_slug = ?3", params![phase, sprint, card])
        .map_err(|e| e.to_string())?;
    if changes == 0 {
        return Err(format!("sprint-set-item-phase: no sprint_items row for sprint='{sprint}' card='{card}'"));
    }
    Ok(json!({ "sprint_slug": sprint, "card_slug": card, "phase": phase, "updated": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn updates_or_not_found() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE sprint_items (sprint_slug TEXT, card_slug TEXT, phase TEXT); INSERT INTO sprint_items VALUES ('s','C','impl');").unwrap();
        assert_eq!(sprintSetItemPhase(&json!({ "sprint_slug": "s", "card_slug": "C", "phase": "qa" }), &c).unwrap()["updated"], json!(true));
        assert!(sprintSetItemPhase(&json!({ "sprint_slug": "s", "card_slug": "X", "phase": "qa" }), &c).is_err());
        assert!(sprintSetItemPhase(&json!({ "sprint_slug": "s", "card_slug": "C", "phase": "bad" }), &c).is_err());
    }
}
