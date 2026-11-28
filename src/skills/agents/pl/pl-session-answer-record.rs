//! pl-session-answer-record.rs — twin of skills/agents/pl/pl-session-answer-record.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `plSessionAnswerRecord` (default). Records an answer to a pl_questions row
//! (selected_option_id XOR writein_md), then auto-advances the session step once the step is fully answered.
// QUALITY: INSERT + option-ownership guard + LEFT JOIN unanswered count + STEP_NEXT advance (frozen clock)
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS truthiness — used for the `if (!question_id)` guard.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// JS destructuring default: absent key → default; present (incl. null) → the value as-is.
fn destr(input: &Value, key: &str, default: Value) -> Value {
    match input.get(key) { None => default, Some(v) => v.clone() }
}

// `${v}` interpolation for the (guard-only) error messages: id numbers / strings.
fn tmpl(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        o => o.to_string(),
    }
}

pub fn plSessionAnswerRecord(input: &Value, db: &Connection) -> Result<Value, String> {
    // context.db is always present in the Rust lane (the harness passes the Connection directly).
    let question_id = destr(input, "question_id", Value::Null);
    let selected_option_id = destr(input, "selected_option_id", Value::Null);
    let writein_md = destr(input, "writein_md", Value::Null);
    // answered_by default is `context?.actor ?? 'human:cli'`; the conform context is {db} only (no
    // actor), so the default collapses to 'human:cli'.
    let answered_by = destr(input, "answered_by", json!("human:cli"));

    if !truthy(Some(&question_id)) {
        return Err("pl-session-answer-record: question_id is required".into());
    }
    // `selected_option_id == null && (!writein_md || !writein_md.trim())`
    let writein_blank = match &writein_md {
        Value::String(s) => s.trim().is_empty(),
        other => !truthy(Some(other)),
    };
    if selected_option_id.is_null() && writein_blank {
        return Err("pl-session-answer-record: selected_option_id OR writein_md is required".into());
    }

    let q_rows = crate::db_json::query_rows(db, "SELECT * FROM pl_questions WHERE id = ?", &[question_id.clone()])?;
    let q = match q_rows.as_array().and_then(|a| a.first()) {
        Some(q) => q.clone(),
        None => return Err(format!("pl-session-answer-record: question {} not found", tmpl(&question_id))),
    };
    let session_id = q.get("session_id").cloned().unwrap_or(Value::Null);
    let step = q.get("step").cloned().unwrap_or(Value::Null);

    // Validate selected_option_id belongs to this question if provided.
    if !selected_option_id.is_null() {
        let opt = crate::db_json::query_rows(db, "SELECT 1 FROM pl_question_options WHERE id = ? AND question_id = ?", &[selected_option_id.clone(), question_id.clone()])?;
        if opt.as_array().map_or(true, |a| a.is_empty()) {
            return Err(format!("pl-session-answer-record: selected_option_id {} does not belong to question {}", tmpl(&selected_option_id), tmpl(&question_id)));
        }
    }

    // Insert answer (UNIQUE on question_id → ALREADY_EXISTS on duplicate). Guard-only in the crossing.
    match crate::db_json::run_write(db, "INSERT INTO pl_answers (question_id, selected_option_id, writein_md, answered_by) VALUES (?, ?, ?, ?)", &[question_id.clone(), selected_option_id.clone(), writein_md.clone(), answered_by.clone()]) {
        Ok(_) => {}
        Err(e) => {
            if e.contains("UNIQUE") {
                return Err(format!("pl-session-answer-record: question {} already answered", tmpl(&question_id)));
            }
            return Err(e);
        }
    }

    // Auto-advance the step once every question of the current step is answered.
    let count_rows = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS c FROM pl_questions q LEFT JOIN pl_answers a ON a.question_id = q.id WHERE q.session_id = ? AND q.step = ? AND a.id IS NULL",
        &[session_id.clone(), step.clone()],
    )?;
    let unanswered = count_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("c")).and_then(|v| v.as_i64()).unwrap_or(0);

    let mut advanced_to = Value::Null;
    if unanswered == 0 {
        let next = match step.as_str() {
            Some("clarify") => Some("researching"),
            Some("research") => Some("deciding_split"),
            Some("decompose") => Some("tuning"),
            Some("tune") => Some("awaiting_approval"),
            _ => None,
        };
        if let Some(next) = next {
            crate::db_json::run_write(db, &format!("UPDATE pl_sessions SET status = ?, updated_at = {} WHERE id = ?", crate::clock::now_sql()), &[json!(next), session_id.clone()])?;
            advanced_to = json!(next);
        }
    }

    let mut out = Map::new();
    out.insert("question_id".into(), question_id);
    out.insert("session_id".into(), session_id);
    out.insert("answered_by".into(), answered_by);
    out.insert("advanced_to".into(), advanced_to);
    out.insert("remaining_questions_this_step".into(), json!(unanswered));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY, status TEXT NOT NULL, updated_at TEXT); \
             CREATE TABLE pl_questions (id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL, step TEXT NOT NULL); \
             CREATE TABLE pl_question_options (id INTEGER PRIMARY KEY, question_id INTEGER NOT NULL); \
             CREATE TABLE pl_answers (id INTEGER PRIMARY KEY, question_id INTEGER NOT NULL UNIQUE, selected_option_id INTEGER, writein_md TEXT, answered_by TEXT NOT NULL);",
        ).unwrap();
        c
    }

    #[test]
    fn records_and_holds_when_step_incomplete() {
        let c = db0();
        c.execute_batch(
            "INSERT INTO pl_sessions (id, status, updated_at) VALUES (1, 'clarifying', '2025-01-01 00:00:00'); \
             INSERT INTO pl_questions (id, session_id, step) VALUES (10, 1, 'clarify'), (11, 1, 'clarify'); \
             INSERT INTO pl_question_options (id, question_id) VALUES (100, 10);",
        ).unwrap();
        let r = plSessionAnswerRecord(&json!({ "question_id": 10, "selected_option_id": 100 }), &c).unwrap();
        assert_eq!(r, json!({ "question_id": 10, "session_id": 1, "answered_by": "human:cli", "advanced_to": null, "remaining_questions_this_step": 1 }));
        // key order is the JS object-literal order
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["question_id", "session_id", "answered_by", "advanced_to", "remaining_questions_this_step"]);
        let status: String = c.query_row("SELECT status FROM pl_sessions WHERE id=1", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "clarifying"); // step not complete → not advanced
    }

    #[test]
    fn writein_completes_step_and_advances() {
        let c = db0();
        c.execute_batch(
            "INSERT INTO pl_sessions (id, status, updated_at) VALUES (2, 'clarifying', '2025-01-01 00:00:00'); \
             INSERT INTO pl_questions (id, session_id, step) VALUES (20, 2, 'clarify');",
        ).unwrap();
        let r = plSessionAnswerRecord(&json!({ "question_id": 20, "writein_md": "my answer", "answered_by": "human:dani" }), &c).unwrap();
        assert_eq!(r["advanced_to"], json!("researching"));
        assert_eq!(r["remaining_questions_this_step"], json!(0));
        assert_eq!(r["answered_by"], json!("human:dani"));
        let status: String = c.query_row("SELECT status FROM pl_sessions WHERE id=2", [], |row| row.get(0)).unwrap();
        let updated: String = c.query_row("SELECT updated_at FROM pl_sessions WHERE id=2", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "researching");
        assert_eq!(updated, "2026-01-01 00:00:00"); // frozen clock literal
    }

    #[test]
    fn step_filter_advances_ignoring_other_steps() {
        let c = db0();
        c.execute_batch(
            "INSERT INTO pl_sessions (id, status, updated_at) VALUES (4, 'clarifying', '2025-01-01 00:00:00'); \
             INSERT INTO pl_questions (id, session_id, step) VALUES (40, 4, 'clarify'), (41, 4, 'research'); \
             INSERT INTO pl_question_options (id, question_id) VALUES (400, 40);",
        ).unwrap();
        let r = plSessionAnswerRecord(&json!({ "question_id": 40, "selected_option_id": 400 }), &c).unwrap();
        assert_eq!(r["advanced_to"], json!("researching")); // only the clarify step must be complete
        assert_eq!(r["remaining_questions_this_step"], json!(0));
    }

    #[test]
    fn tune_step_advances_to_awaiting_approval() {
        let c = db0();
        c.execute_batch(
            "INSERT INTO pl_sessions (id, status, updated_at) VALUES (3, 'tuning', '2025-01-01 00:00:00'); \
             INSERT INTO pl_questions (id, session_id, step) VALUES (30, 3, 'tune'); \
             INSERT INTO pl_question_options (id, question_id) VALUES (300, 30);",
        ).unwrap();
        let r = plSessionAnswerRecord(&json!({ "question_id": 30, "selected_option_id": 300 }), &c).unwrap();
        assert_eq!(r["advanced_to"], json!("awaiting_approval"));
    }

    #[test]
    fn guards_and_option_ownership() {
        let c = db0();
        c.execute_batch(
            "INSERT INTO pl_sessions (id, status, updated_at) VALUES (6, 'clarifying', '2025-01-01 00:00:00'); \
             INSERT INTO pl_questions (id, session_id, step) VALUES (60, 6, 'clarify'), (61, 6, 'clarify'); \
             INSERT INTO pl_question_options (id, question_id) VALUES (600, 61);",
        ).unwrap();
        assert!(plSessionAnswerRecord(&json!({ "question_id": 60 }), &c).is_err()); // neither option nor writein
        assert!(plSessionAnswerRecord(&json!({ "question_id": 60, "selected_option_id": 600 }), &c).is_err()); // option belongs to q61
        assert!(plSessionAnswerRecord(&json!({ "question_id": 999, "writein_md": "x" }), &c).is_err()); // unknown question
    }
}
