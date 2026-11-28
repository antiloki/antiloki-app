//! memory-add.rs — twin of skills/entities/memory-add.mjs (db WRITE lane, attached `memory` db).
//! NAMES ARE CONTRACT: `memoryAdd` (default), `ALLOWED_TYPES` (data). INSERT a new memory row
//! (lesson/preference/decision/reference/correction/narrative), then SELECT * it back.
use rusqlite::Connection;
use serde_json::{json, Value};

pub const ALLOWED_TYPES: [&str; 6] = ["lesson", "preference", "decision", "reference", "correction", "narrative"];

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS `${v}` interpolation (only feeds error messages).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

pub fn memoryAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('memory-add','db') when no db — the db lane always injects one.
    let type_v = input.get("type").cloned().unwrap_or(Value::Null);
    let title = input.get("title").cloned().unwrap_or(Value::Null);
    let situation = input.get("situation").cloned().unwrap_or(Value::Null);
    let guidance = input.get("guidance").cloned().unwrap_or(Value::Null);
    // destructuring defaults fire only when the key is ABSENT; an explicit null stays null, which
    // equals the default anyway for these five fields, so a plain "present ? value : default" read
    // is byte-identical to the JS destructure.
    let naive_response = input.get("naive_response").cloned().unwrap_or(Value::Null);
    let rationale = input.get("rationale").cloned().unwrap_or(Value::Null);
    let tags = match input.get("tags") { None => json!([]), Some(v) => v.clone() };
    let source_card = input.get("source_card").cloned().unwrap_or(Value::Null);
    let source_event = input.get("source_event").cloned().unwrap_or(Value::Null);
    let source_session = input.get("source_session").cloned().unwrap_or(Value::Null);
    let confidence = match input.get("confidence") { None => json!(0.5), Some(v) => v.clone() };
    let created_by = input.get("created_by").cloned().unwrap_or(Value::Null);

    if !truthy(Some(&type_v)) { return Err("memory-add: type is required".into()); }
    let type_s = type_v.as_str().unwrap_or("");
    let is_correction = type_s == "correction";
    if !ALLOWED_TYPES.contains(&type_s) {
        return Err(format!("memory-add: type must be one of: {} (got '{}')", ALLOWED_TYPES.join(", "), tmpl(Some(&type_v))));
    }
    if !truthy(Some(&title)) { return Err("memory-add: title is required".into()); }
    if !truthy(Some(&situation)) { return Err("memory-add: situation is required".into()); }
    if !truthy(Some(&guidance)) { return Err("memory-add: guidance is required".into()); }
    if !truthy(Some(&created_by)) { return Err("memory-add: created_by is required".into()); }
    if !confidence.is_number() || confidence.as_f64().map_or(true, |f| f < 0.0 || f > 1.0) {
        return Err(format!("memory-add: confidence must be between 0 and 1 (got {})", tmpl(Some(&confidence))));
    }

    // FEAT-AGENT-SCOPED-MEMORY-NAMESPACE: JS derives an `agent:<name>` tag from context.actor when
    // the caller didn't already tag one. The db lane always calls with context == { db } (no actor
    // field — see conform-db.mjs), so `context?.actor` is always undefined there, `String(undefined
    // ?? '')` is '', and the regex `^(?:agent:)?(pl|tl|dev|qa|man|keeper)\b` can never match an empty
    // string — tags_final always equals tags unchanged under the harness. No branch to reduce.
    let tags_final = tags;

    if !tags_final.is_array() {
        return Err("memory-add: tags must be an array".into());
    }
    if is_correction && !truthy(Some(&naive_response)) {
        return Err("memory-add: type=correction requires naive_response (the wrong answer being corrected)".into());
    }

    let tags_json = serde_json::to_string(&tags_final).map_err(|e| e.to_string())?;

    let (_changes, last_id) = crate::db_json::run_write(
        db,
        "INSERT INTO memory.memories\n      (type, title, situation, guidance, naive_response, rationale, tags_json,\n       source_card, source_event, source_session, confidence, created_by)\n     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        &[type_v, title, situation, guidance, naive_response, rationale, json!(tags_json), source_card, source_event, source_session, confidence, created_by],
    )?;

    let rows = crate::db_json::query_rows(db, "SELECT * FROM memory.memories WHERE id = ?", &[json!(last_id)])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
            CREATE TABLE memory.memories (
              id              INTEGER PRIMARY KEY AUTOINCREMENT,
              type            TEXT    NOT NULL,
              title           TEXT    NOT NULL,
              situation       TEXT    NOT NULL,
              guidance        TEXT    NOT NULL,
              naive_response  TEXT,
              rationale       TEXT,
              tags_json       TEXT    NOT NULL DEFAULT '[]',
              source_card     TEXT,
              source_event    INTEGER,
              source_session  TEXT,
              confidence      INTEGER NOT NULL DEFAULT 0,
              validated       INTEGER NOT NULL DEFAULT 0,
              superseded_by   INTEGER,
              agent_slug      TEXT,
              created_at      TEXT    NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
              created_by      TEXT    NOT NULL,
              CHECK (type IN ('lesson', 'preference', 'decision', 'reference', 'correction', 'narrative')),
              CHECK (confidence >= 0 AND confidence <= 1),
              CHECK (validated IN (0, 1))
            );",
        )
        .unwrap();
        c
    }

    #[test]
    fn inserts_minimal_lesson_with_defaults() {
        let c = db();
        let r = memoryAdd(&json!({
            "type": "lesson", "title": "T1", "situation": "S1", "guidance": "G1", "created_by": "test"
        }), &c).unwrap();
        assert_eq!(r["id"], json!(1));
        assert_eq!(r["type"], json!("lesson"));
        assert_eq!(r["naive_response"], Value::Null);
        assert_eq!(r["rationale"], Value::Null);
        assert_eq!(r["tags_json"], json!("[]"));
        assert_eq!(r["source_card"], Value::Null);
        assert_eq!(r["source_event"], Value::Null);
        assert_eq!(r["source_session"], Value::Null);
        assert_eq!(r["confidence"], json!(0.5));
        assert_eq!(r["validated"], json!(0));
        assert_eq!(r["superseded_by"], Value::Null);
        assert_eq!(r["created_at"], json!("2026-01-01 00:00:00"));
        assert_eq!(r["created_by"], json!("test"));
    }

    #[test]
    fn correction_requires_and_stores_naive_response() {
        let c = db();
        let r = memoryAdd(&json!({
            "type": "correction", "title": "T2", "situation": "S2", "guidance": "G2",
            "naive_response": "I proposed 3 tables", "created_by": "test"
        }), &c).unwrap();
        assert_eq!(r["naive_response"], json!("I proposed 3 tables"));
        assert!(memoryAdd(&json!({
            "type": "correction", "title": "T2b", "situation": "S2", "guidance": "G2", "created_by": "test"
        }), &c).unwrap_err().contains("naive_response"));
    }

    #[test]
    fn stores_tags_rationale_and_fractional_confidence() {
        let c = db();
        let r = memoryAdd(&json!({
            "type": "preference", "title": "T3", "situation": "S3", "guidance": "G3",
            "rationale": "because reasons", "tags": ["schema", "ai-friendly"], "confidence": 0.9, "created_by": "test"
        }), &c).unwrap();
        assert_eq!(r["rationale"], json!("because reasons"));
        assert_eq!(r["tags_json"], json!("[\"schema\",\"ai-friendly\"]"));
        assert_eq!(r["confidence"], json!(0.9));
    }

    #[test]
    fn stores_source_links_and_integer_confidence_boundaries() {
        let c = db();
        let r = memoryAdd(&json!({
            "type": "decision", "title": "T4", "situation": "S4", "guidance": "G4",
            "source_card": "REAL-001", "source_event": 42, "source_session": "sess-abc123",
            "confidence": 1, "created_by": "test"
        }), &c).unwrap();
        assert_eq!(r["source_card"], json!("REAL-001"));
        assert_eq!(r["source_event"], json!(42));
        assert_eq!(r["source_session"], json!("sess-abc123"));
        assert_eq!(r["confidence"], json!(1));

        let r0 = memoryAdd(&json!({
            "type": "reference", "title": "T5", "situation": "S5", "guidance": "G5",
            "confidence": 0, "created_by": "test"
        }), &c).unwrap();
        assert_eq!(r0["confidence"], json!(0));
    }

    #[test]
    fn preexisting_agent_tag_passes_through_unchanged() {
        let c = db();
        let r = memoryAdd(&json!({
            "type": "narrative", "title": "T6", "situation": "S6", "guidance": "G6",
            "tags": ["agent:dev", "other"], "naive_response": "kept even though not correction", "created_by": "test"
        }), &c).unwrap();
        assert_eq!(r["tags_json"], json!("[\"agent:dev\",\"other\"]"));
        assert_eq!(r["naive_response"], json!("kept even though not correction"));
    }

    #[test]
    fn accepts_all_allowed_types() {
        for t in ALLOWED_TYPES {
            let c = db();
            let mut input = json!({ "type": t, "title": "T", "situation": "S", "guidance": "G", "created_by": "test" });
            if t == "correction" { input["naive_response"] = json!("wrong answer"); }
            assert_eq!(memoryAdd(&input, &c).unwrap()["type"], json!(t));
        }
    }

    #[test]
    fn guards_reject_invalid_input() {
        let c = db();
        assert!(memoryAdd(&json!({ "title": "T", "situation": "S", "guidance": "G", "created_by": "u" }), &c).is_err()); // type missing
        assert!(memoryAdd(&json!({ "type": "bogus", "title": "T", "situation": "S", "guidance": "G", "created_by": "u" }), &c).is_err()); // type enum
        assert!(memoryAdd(&json!({ "type": "lesson", "situation": "S", "guidance": "G", "created_by": "u" }), &c).is_err()); // title missing
        assert!(memoryAdd(&json!({ "type": "lesson", "title": "", "situation": "S", "guidance": "G", "created_by": "u" }), &c).is_err()); // title empty
        assert!(memoryAdd(&json!({ "type": "lesson", "title": "T", "guidance": "G", "created_by": "u" }), &c).is_err()); // situation missing
        assert!(memoryAdd(&json!({ "type": "lesson", "title": "T", "situation": "S", "created_by": "u" }), &c).is_err()); // guidance missing
        assert!(memoryAdd(&json!({ "type": "lesson", "title": "T", "situation": "S", "guidance": "G" }), &c).is_err()); // created_by missing
        assert!(memoryAdd(&json!({ "type": "lesson", "title": "T", "situation": "S", "guidance": "G", "created_by": "u", "confidence": -0.1 }), &c).is_err());
        assert!(memoryAdd(&json!({ "type": "lesson", "title": "T", "situation": "S", "guidance": "G", "created_by": "u", "confidence": 1.5 }), &c).is_err());
        assert!(memoryAdd(&json!({ "type": "lesson", "title": "T", "situation": "S", "guidance": "G", "created_by": "u", "confidence": "high" }), &c).is_err());
        assert!(memoryAdd(&json!({ "type": "lesson", "title": "T", "situation": "S", "guidance": "G", "created_by": "u", "tags": "not-array" }), &c).is_err());
    }
}
