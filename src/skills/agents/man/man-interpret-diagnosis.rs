//! man-interpret-diagnosis.rs — twin of skills/agents/man/man-interpret-diagnosis.mjs (db lane).
//! NAMES ARE CONTRACT: `manInterpretDiagnosis` (default). Thin wrapper over the already-converted
//! `agents_man_man_diagnose_failure::interpretPending` twin — on-demand interpretation of a still-
//! pending (P1-only) diagnosis, selectable by `diagnosis_id` or `card_slug`. Idempotent + cached
//! (see interpretPending's own doc): an already-classified row returns as-is.
//!
//! `provider_fn` (JS `context.provider_fn`) has NO Rust equivalent — `interpretPending` calls
//! `interpret()` internally, which dispatches to `crate::ai_ai_call::aiCall`, itself a PARITY-
//! DEFERRED always-`Err` stub (see man-diagnose-failure.rs's module doc). This wrapper is fully
//! ported; the deferred boundary lives one layer down, in the AI dispatch it calls into.
//!
//! `context.db` missing → JS throws `err.contextMissing('man-interpret-diagnosis', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin).
//!
//! Throw fidelity: neither `diagnosis_id` nor `card_slug` given →
//! `"man-interpret-diagnosis: diagnosis_id|card_slug is required"` (verified against
//! `err.required`'s message-join format in engine/error.mjs: `${skill}: ${field} is required`).
// QUALITY: thin dispatcher — fully ported around the already-converted interpretPending twin;
// no fixture/frontier/arm (bespoke-native, per CONVERSION-PLAYBOOK's dispatcher-over-nondet-dep class).

use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

#[allow(non_snake_case)]
pub fn manInterpretDiagnosis(input: &Value, db: &Connection) -> Result<Value, String> {
    let diagnosis_id = input.get("diagnosis_id").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null);
    let card_slug = input.get("card_slug").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null);
    if diagnosis_id.is_null() && !truthy(Some(&card_slug)) {
        return Err("man-interpret-diagnosis: diagnosis_id|card_slug is required".to_string());
    }

    let sel = json!({ "diagnosis_id": diagnosis_id, "card_slug": card_slug });
    let diagnosis = crate::agents_man_man_diagnose_failure::interpretPending(db, &sel)?;
    let interpreted = !diagnosis.is_null() && truthy(diagnosis.get("classification"));
    Ok(json!({ "diagnosis": diagnosis, "interpreted": interpreted }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, started_at TEXT, ended_at TEXT, \
             card_slug TEXT, decided_agent TEXT, decided_action TEXT, decided_reason TEXT, outcome TEXT, \
             duration_ms INTEGER, error_message TEXT, cost_usd REAL, facts_json TEXT);
             CREATE TABLE brain.driver_diagnoses ( id INTEGER PRIMARY KEY AUTOINCREMENT, signature TEXT NOT NULL UNIQUE, tick_id INTEGER, card_slug TEXT, classification TEXT, root_cause_md TEXT, suggested_fix_md TEXT, confidence REAL, gather_json TEXT, recurrences INTEGER NOT NULL DEFAULT 1, model_id TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), updated_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')) );
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE inbox_messages ( id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), kind TEXT, subject TEXT, body_md TEXT, severity TEXT, related_card_slug TEXT, related_contract_slug TEXT, suggested_reconciler TEXT, suggested_card_action TEXT, payload_json TEXT );"
        ).unwrap();
        c
    }

    fn seed_tick(c: &Connection, card_slug: &str, action: &str, outcome: &str, error_message: Option<&str>) -> i64 {
        c.execute(
            "INSERT INTO brain.driver_ticks (started_at, ended_at, card_slug, decided_action, outcome, error_message) VALUES ('2026-01-01 10:00:00','2026-01-01 10:00:05', ?, ?, ?, ?)",
            rusqlite::params![card_slug, action, outcome, error_message],
        ).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn missing_both_selectors_returns_exact_message() {
        let c = db();
        let e = manInterpretDiagnosis(&json!({}), &c).unwrap_err();
        assert_eq!(e, "man-interpret-diagnosis: diagnosis_id|card_slug is required");
    }

    #[test]
    fn empty_card_slug_string_is_treated_as_missing() {
        let c = db();
        let e = manInterpretDiagnosis(&json!({ "card_slug": "" }), &c).unwrap_err();
        assert_eq!(e, "man-interpret-diagnosis: diagnosis_id|card_slug is required");
    }

    #[test]
    fn no_matching_diagnosis_returns_null_diagnosis_and_not_interpreted() {
        let c = db();
        let r = manInterpretDiagnosis(&json!({ "card_slug": "NOPE" }), &c).unwrap();
        assert_eq!(r["diagnosis"], Value::Null);
        assert_eq!(r["interpreted"], json!(false));
    }

    #[test]
    fn pending_diagnosis_reinterprets_and_stays_p1_only_deferred_ai() {
        let c = db();
        let tick_id = seed_tick(&c, "CARD-X", "blueprint.apply", "failed", Some("boom"));
        crate::agents_man_man_diagnose_failure::manDiagnoseFailure(
            &json!({ "tick_id": tick_id, "gather_only": true }),
            &c,
        ).unwrap();
        let r = manInterpretDiagnosis(&json!({ "card_slug": "CARD-X" }), &c).unwrap();
        assert_eq!(r["diagnosis"]["classification"], Value::Null); // aiCall deferred stub → still pending
        assert_eq!(r["interpreted"], json!(false));
    }

    #[test]
    fn already_classified_diagnosis_is_cached_and_reported_interpreted() {
        let c = db();
        let tick_id = seed_tick(&c, "CARD-X", "blueprint.apply", "failed", Some("boom"));
        let rec = crate::agents_man_man_diagnose_failure::manDiagnoseFailure(
            &json!({ "tick_id": tick_id, "gather_only": true }),
            &c,
        ).unwrap();
        c.execute(
            "UPDATE brain.driver_diagnoses SET classification = 'card-bug' WHERE id = ?",
            [rec["diagnosis_id"].as_i64().unwrap()],
        ).unwrap();
        let r = manInterpretDiagnosis(&json!({ "diagnosis_id": rec["diagnosis_id"] }), &c).unwrap();
        assert_eq!(r["diagnosis"]["classification"], json!("card-bug"));
        assert_eq!(r["interpreted"], json!(true));
    }
}
