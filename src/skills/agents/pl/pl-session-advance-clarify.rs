//! pl-session-advance-clarify.rs — twin of skills/agents/pl/pl-session-advance-clarify.mjs (db
//! WRITE lane, cross-db brain.*). NAMES ARE CONTRACT: default `plSessionAdvanceClarify`. IN's
//! elicitation step: ensures the 'pl.clarify.questions.v2' prompt template exists, then calls
//! aiCall for 1-3 clarifying questions; on confidence_in_understanding >= 0.85 (or an empty
//! questions array) it advances the session straight to 'researching', otherwise it decomposes
//! the AI's {questions:[{question_md, options[]}]} output into pl_questions + pl_question_options
//! rows and leaves the session in 'clarifying' awaiting human answers.
//!
//! PARITY-DEFERRED: dispatches through crate::ai_ai_call::aiCall (the deferred network-AI stub —
//! see ai-call.rs's module doc), which always returns Err. Everything around it is ported
//! faithfully and IS the deterministic surface these tests exercise: session lookup + status
//! guard, the FIX-DRIVER-PL-CLARIFY-LOOP pending-questions early-return guard, prompt-template
//! ensure/bootstrap (`ensure_prompt`, reusing the already-converted `ai_prompt_find`/
//! `ai_prompt_add` twins). The response-decomposition logic (parent/child ai_call_outputs rows →
//! pl_questions/pl_question_options, the confidence>=0.85 and empty-questions advance branches)
//! is ported byte-for-byte from the JS but — like book-run.rs's post-wave logic — is unreachable
//! by this file's own fixture because it runs only after a successful aiCall, which the deferred
//! stub never produces; correctness there is by inspection + the same shape as the reachable code.
use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn destr(input: &Value, key: &str, default: Value) -> Value {
    match input.get(key) { None => default, Some(v) => v.clone() }
}
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn first(rows: &Value) -> Option<Value> { rows.as_array().and_then(|a| a.first()).cloned() }

const PROMPT_NAME: &str = "pl.clarify.questions.v2";

// FIX-PL-CLARIFY-PROMPT-MISSING-INTENT: v1 referenced raw_intent in prose with no {{raw_intent}}
// placeholder; v2 (below) fixes that. promptAdd is name-keyed — v1 stays in DB, new sessions use v2.
const PROMPT_TEMPLATE_MD: &str = r#"You are the Interviewer (IN). Below is the user's raw_intent. Generate 1-3 high-leverage clarifying questions. Each question MUST have 2-5 multiple-choice options + always allow a write-in. Be empathetic-but-precise. Avoid yes/no questions unless the answer materially changes scope.

═══ raw_intent ═══
{{raw_intent}}
═══ end raw_intent ═══

Return JSON of this exact shape:
{
  "confidence_in_understanding": <0.0..1.0>,
  "questions": [
    { "question_md": "<one question>", "options": ["<opt1>", "<opt2>", "..."] }
  ]
}

If confidence_in_understanding >= 0.85 the questions array MAY be empty. If the raw_intent already covers motivation + strategy + acceptance (richly populated body), prefer empty questions — don't ask the user to re-state what's already there."#;

// try promptFind; on miss, promptAdd then promptFind again (mirrors JS ensurePrompt's try/catch).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).is_ok() {
        return Ok(());
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "mid",
            "default_temperature": "cold",
            "max_output_tokens": 1024,
            "output_fields": [
                { "field_name": "confidence_in_understanding", "field_type": "number" },
                { "field_name": "questions", "field_type": "array_of_object", "description": "each: {question_md, options[]}" },
            ],
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db)?;
    Ok(())
}

pub fn plSessionAdvanceClarify(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing(...)` — unreachable
    // here (Rust's `db: &Connection` is never absent).
    let session_id = destr(input, "session_id", Value::Null);
    let provider_fn = destr(input, "provider_fn", Value::Null);
    let force = truthy(input.get("force"));

    if !truthy(Some(&session_id)) {
        return Err("pl-session-advance-clarify: session_id is required".into());
    }

    let sess_rows = crate::db_json::query_rows(db, "SELECT * FROM pl_sessions WHERE id = ?", &[session_id.clone()])?;
    let Some(session) = first(&sess_rows) else {
        return Err(format!("pl-session-advance-clarify: session {} not found", js_string(&session_id)));
    };
    let status = session.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if status != "clarifying" && !force {
        return Err(format!(
            "pl-session-advance-clarify: session status='{status}', expected 'clarifying' (pass force=true to override)"
        ));
    }

    // FIX-DRIVER-PL-CLARIFY-LOOP (defense in depth): unanswered clarify questions already exist →
    // don't re-call the AI (would rack up cost every driver tick). force=true overrides.
    if !force {
        let pending_rows = crate::db_json::query_rows(
            db,
            "SELECT COUNT(*) AS c FROM pl_questions q \
             WHERE q.session_id = ? AND q.step = 'clarify' \
               AND NOT EXISTS (SELECT 1 FROM pl_answers a WHERE a.question_id = q.id)",
            &[session_id.clone()],
        )?;
        let pending = first(&pending_rows).and_then(|r| r.get("c").and_then(|v| v.as_i64())).unwrap_or(0);
        if pending > 0 {
            return Ok(json!({
                "session_id": session_id,
                "ai_call_id": Value::Null,
                "confidence_in_understanding": Value::Null,
                "questions_persisted": 0,
                "questions": [],
                "new_status": "clarifying",
                "cached": false,
                "cost_usd": 0,
                "awaiting_human": true,
                "pending_question_count": pending,
                "message": format!("{pending} clarify question(s) already pending; awaiting human answer. Pass force=true to regenerate."),
            }));
        }
    }

    ensure_prompt(db)?;

    let raw_intent = session.get("raw_intent").cloned().unwrap_or(Value::Null);
    let budget_usd = session.get("budget_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let spent_usd = session.get("spent_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);

    // PARITY-DEFERRED past this point: crate::ai_ai_call::aiCall always returns Err (see that
    // module's doc). Everything below is ported byte-for-byte from the JS but unreachable here.
    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [{ "key": "raw_intent", "value": raw_intent }],
        "budget_remaining_usd": budget_usd - spent_usd,
        "provider_fn": provider_fn,
    });
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let outputs_arr = ai_result.get("output_kv").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let confidence = outputs_arr.iter()
        .find(|o| o.get("field_name").and_then(|v| v.as_str()) == Some("confidence_in_understanding"))
        .and_then(|o| o.get("value_number"))
        .filter(|v| !v.is_null())
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let ai_call_id = ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null);
    let cost_usd = ai_result.get("cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cached = ai_result.get("cached").cloned().unwrap_or(json!(false));

    crate::db_json::run_write(
        db,
        "UPDATE pl_sessions SET spent_usd = spent_usd + ?, updated_at = datetime('now') WHERE id = ?",
        &[json!(cost_usd), session_id.clone()],
    )?;

    if confidence >= 0.85 {
        crate::db_json::run_write(
            db,
            "UPDATE pl_sessions SET status = 'researching', updated_at = datetime('now') WHERE id = ?",
            &[session_id.clone()],
        )?;
        return Ok(json!({
            "session_id": session_id,
            "ai_call_id": ai_call_id,
            "confidence_in_understanding": confidence,
            "questions_persisted": 0,
            "new_status": "researching",
            "cached": cached,
            "cost_usd": cost_usd,
        }));
    }

    // Decompose 'questions' (array_of_object): parent rows have field_name='questions' with NULL
    // value cols + order_index = i; children rows have field_name = the object key, parent_output_id
    // pointing at the parent row.
    let mut parent_rows: Vec<&Value> = outputs_arr.iter()
        .filter(|o| {
            o.get("field_name").and_then(|v| v.as_str()) == Some("questions")
                && o.get("value_text").map_or(true, |v| v.is_null())
                && o.get("value_number").map_or(true, |v| v.is_null())
                && o.get("value_boolean").map_or(true, |v| v.is_null())
        })
        .collect();
    parent_rows.sort_by(|a, b| {
        let ai = a.get("order_index").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let bi = b.get("order_index").and_then(|v| v.as_f64()).unwrap_or(0.0);
        ai.partial_cmp(&bi).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut questions_persisted: Vec<Value> = Vec::new();
    for parent in parent_rows {
        let parent_id = parent.get("id").cloned().unwrap_or(Value::Null);
        let child_rows = crate::db_json::query_rows(
            db,
            "SELECT field_name, value_text, order_index FROM brain.ai_call_outputs WHERE ai_call_id = ? AND parent_output_id = ?",
            &[ai_call_id.clone(), parent_id],
        )?;
        let children: Vec<Value> = child_rows.as_array().cloned().unwrap_or_default();

        let question_md = children.iter()
            .find(|c| c.get("field_name").and_then(|v| v.as_str()) == Some("question_md"))
            .and_then(|c| c.get("value_text"))
            .cloned()
            .filter(|v| !v.is_null());
        let Some(question_md) = question_md else { continue };

        // Options may be a JSON-stringified array stored on a single child row.
        let raw_options = children.iter()
            .find(|c| c.get("field_name").and_then(|v| v.as_str()) == Some("options"))
            .and_then(|c| c.get("value_text"))
            .cloned()
            .filter(|v| !v.is_null());
        let mut options: Vec<Value> = Vec::new();
        if let Some(Value::String(s)) = raw_options {
            if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(&s) {
                options = a;
            }
        }

        let order_index = parent.get("order_index").cloned().unwrap_or(Value::Null);
        let (_changes, qid) = crate::db_json::run_write(
            db,
            "INSERT INTO pl_questions (session_id, step, question_md, allows_writein, order_index, ai_call_id) VALUES (?, 'clarify', ?, 1, ?, ?)",
            &[session_id.clone(), question_md.clone(), order_index, ai_call_id.clone()],
        )?;

        for (i, opt) in options.iter().enumerate() {
            if let Some(s) = opt.as_str() {
                if !s.trim().is_empty() {
                    crate::db_json::run_write(
                        db,
                        "INSERT INTO pl_question_options (question_id, option_md, order_index) VALUES (?, ?, ?)",
                        &[json!(qid), json!(s), json!(i as i64)],
                    )?;
                }
            }
        }

        questions_persisted.push(json!({ "question_id": qid, "question_md": question_md, "option_count": options.len() }));
    }

    // FIX-PL-CLARIFY-ADVANCE-WHEN-NO-QUESTIONS: nothing to ask the human → advance to 'researching'
    // anyway; staying in 'clarifying' with zero questions is a dead-loop for the driver.
    if questions_persisted.is_empty() {
        crate::db_json::run_write(
            db,
            "UPDATE pl_sessions SET status = 'researching', updated_at = datetime('now') WHERE id = ?",
            &[session_id.clone()],
        )?;
        return Ok(json!({
            "session_id": session_id,
            "ai_call_id": ai_call_id,
            "confidence_in_understanding": confidence,
            "questions_persisted": 0,
            "questions": [],
            "new_status": "researching",
            "advanced_despite_no_questions": true,
            "cached": cached,
            "cost_usd": cost_usd,
        }));
    }

    Ok(json!({
        "session_id": session_id,
        "ai_call_id": ai_call_id,
        "confidence_in_understanding": confidence,
        "questions_persisted": questions_persisted.len(),
        "questions": questions_persisted,
        "new_status": "clarifying",
        "cached": cached,
        "cost_usd": cost_usd,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY, raw_intent TEXT, status TEXT NOT NULL, budget_usd REAL NOT NULL DEFAULT 0, spent_usd REAL NOT NULL DEFAULT 0, updated_at TEXT);
             CREATE TABLE pl_questions (id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL, step TEXT NOT NULL, question_md TEXT, allows_writein INTEGER, order_index INTEGER, ai_call_id INTEGER);
             CREATE TABLE pl_answers (id INTEGER PRIMARY KEY, question_id INTEGER NOT NULL);
             CREATE TABLE pl_question_options (id INTEGER PRIMARY KEY, question_id INTEGER NOT NULL, option_md TEXT, order_index INTEGER);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn a_session(c: &Connection, status: &str) -> i64 {
        c.execute("INSERT INTO pl_sessions (raw_intent, status, budget_usd, spent_usd) VALUES ('build the thing', ?, 0.5, 0.1)", [status]).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn session_id_is_required() {
        let c = setup();
        assert!(plSessionAdvanceClarify(&json!({}), &c).unwrap_err().contains("session_id is required"));
    }

    #[test]
    fn session_not_found_errors() {
        let c = setup();
        assert!(plSessionAdvanceClarify(&json!({ "session_id": 999 }), &c).unwrap_err().contains("session 999 not found"));
    }

    #[test]
    fn status_guard_blocks_unless_forced() {
        let c = setup();
        let sid = a_session(&c, "researching");
        let err = plSessionAdvanceClarify(&json!({ "session_id": sid }), &c).unwrap_err();
        assert!(err.contains("status='researching', expected 'clarifying'"), "unexpected: {err}");
        // no prompt template was ensured — the guard fired first
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn pending_questions_short_circuit_without_calling_ai() {
        let c = setup();
        let sid = a_session(&c, "clarifying");
        c.execute("INSERT INTO pl_questions (session_id, step) VALUES (?, 'clarify')", [sid]).unwrap();
        let r = plSessionAdvanceClarify(&json!({ "session_id": sid }), &c).unwrap();
        assert_eq!(r["awaiting_human"], json!(true));
        assert_eq!(r["pending_question_count"], json!(1));
        assert_eq!(r["new_status"], json!("clarifying"));
        assert_eq!(r["ai_call_id"], Value::Null);
        // no prompt template ensured — the pending guard returned before ensure_prompt
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn answered_question_does_not_count_as_pending() {
        let c = setup();
        let sid = a_session(&c, "clarifying");
        c.execute("INSERT INTO pl_questions (id, session_id, step) VALUES (1, ?, 'clarify')", [sid]).unwrap();
        c.execute("INSERT INTO pl_answers (question_id) VALUES (1)", []).unwrap();
        // fully answered → reaches ensure_prompt then the deferred aiCall boundary
        let err = plSessionAdvanceClarify(&json!({ "session_id": sid }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
    }

    #[test]
    fn force_bypasses_status_and_pending_guards_and_reaches_deferred_boundary() {
        let c = setup();
        let sid = a_session(&c, "researching");
        c.execute("INSERT INTO pl_questions (session_id, step) VALUES (?, 'clarify')", [sid]).unwrap();
        let err = plSessionAdvanceClarify(&json!({ "session_id": sid, "force": true }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        // the prompt template WAS ensured (ensure_prompt ran) before the deferred call
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'pl.clarify.questions.v2'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn clean_clarifying_session_ensures_prompt_then_reaches_deferred_boundary() {
        let c = setup();
        let sid = a_session(&c, "clarifying");
        let err = plSessionAdvanceClarify(&json!({ "session_id": sid }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'pl.clarify.questions.v2'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
        // second call reuses the already-registered template (promptFind hit, no re-insert)
        let _ = plSessionAdvanceClarify(&json!({ "session_id": sid }), &c);
        let n2: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'pl.clarify.questions.v2'", [], |row| row.get(0)).unwrap();
        assert_eq!(n2, 1);
    }
}
