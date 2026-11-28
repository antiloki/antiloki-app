//! interviewer-clarify-questionnaire.rs — twin of
//! skills/agents/in/interviewer-clarify-questionnaire.mjs (db READ+WRITE, brain-attached;
//! LLM-PARTIAL). NAMES ARE CONTRACT: `interviewerClarifyQuestionnaire` (default), `PROMPT_NAME`,
//! `PROMPT_TEMPLATE_MD`, `PROMPT_OUTPUT_FIELDS` (as `prompt_output_fields()`, a fn — see
//! ai-call.rs's `TEMP_TO_VALUE` precedent for why a `const` DATA export becomes a Rust fn when
//! the JS shape is an array literal, not a primitive), `SKIP_THRESHOLD`, `ensurePrompt`.
//!
//! PARITY-DEFERRED: the `aiCall(...)` leg is a real network call to a BYOK LLM provider —
//! genuinely non-deterministic, unfixturable in principle (see `crate::ai_ai_call::aiCall`'s own
//! doc). This twin calls that deferred stub, which always returns `Err(..)`, so the real happy
//! path (an actual model response reaching `shape_questionnaire`) cannot be exercised by a native
//! test — same limitation as interviewer-ai-fallback.rs's identical boundary. Everything else is
//! ported faithfully: the `user_text` guard, prompt upsert idempotency (`ensurePrompt`: find-or-
//! create via `crate::ai_prompt_find`/`crate::ai_prompt_add`), the STUB test-mode short-circuit
//! (`ANTILOKI_INTERVIEWER_QUESTIONNAIRE_STUB=1`, a deterministic canned 3-option fork with no AI
//! call), the `questions_json` string → normalized `{question_md, options:[{key,label}]}` parse
//! (capped 3 questions / 4 options, `should_render` gate at `SKIP_THRESHOLD`), and the telemetry
//! token readback from `brain.ai_calls`.
//!
//! `provider_fn` (the JS test-injection hook for a mock provider) has no Rust equivalent — it only
//! ever reaches the deferred `aiCall` body, so it is dropped from this twin's signature, same as
//! interviewer-ai-fallback.rs drops it.
//!
//! `shape_questionnaire` is factored out as its own pure fn (mirrors interviewer-ai-fallback.rs's
//! `collapse_output_kv`) so the deterministic parsing/shaping surface is independently testable
//! against a canned `output_kv`, since the real `aiCall` leg can never produce one here.
// QUALITY: mixed unit — guard + prompt-upsert + stub branch + response-shaping ported/fixture-
// tested; aiCall leg PARITY-DEFERRED (network, unfixturable)

use rusqlite::Connection;
use serde_json::{json, Map, Value};

pub const PROMPT_NAME: &str = "interviewer.clarify.questionnaire.v3";

pub const PROMPT_TEMPLATE_MD: &str = "You are the CLARIFIER. The triage phase saw the user's input but couldn't confidently give a GOOD answer. Your job: list EVERY question you need answered to close that gap — UP TO 3 — each with 3-5 concrete options. The user picks the pieces that fit (chips) to mount their idea, so give a rich, specific set — not a bare A/B. Surface ALL the gaps AT ONCE, never a back-and-forth drill-down.\n\nRules:\n  - 1 to 3 questions. Fewer is better — ask only what genuinely blocks a good answer. If one question suffices, ask one.\n  - Each question is ONE short sentence. No greetings, no \"great question\".\n  - Each question has 3-5 options. Each option is a SHORT phrase (2-3 seconds to read) and specific — a concrete building block the user can pick. They may pick several, so options need NOT be mutually exclusive.\n  - Avoid \"Other\"/\"Something else\" placeholders — the FE adds a write-in per question automatically.\n  - If you actually understand the user well enough WITHOUT asking, return own_confidence >= 0.85 and an EMPTY questions list. Honest signal — don't over-ask.\n\nUSER INPUT: {{user_text}}\nTRIAGE INTENT SUMMARY: {{intent_summary}}\nRECENT TURNS (for thread continuity):\n{{recent_turns}}\n\nOutput JSON of this exact shape:\n{\n  \"questions_json\": [\n    {\"question\": \"<one-sentence question>\", \"options\": [\"<short label>\", \"<short label>\"]}\n  ],\n  \"own_confidence\": <number 0.0..1.0, your honest confidence the user's intent is clear WITHOUT asking>\n}";

/// `PROMPT_OUTPUT_FIELDS` (JS `export const` array literal, 2 entries) as a fn — see module doc.
pub fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "questions_json", "field_type": "string", "required": true },
        { "field_name": "own_confidence", "field_type": "number", "required": true },
    ])
}

/// Confidence below this → questionnaire renders. At/above → callers (interviewer-turn) skip it;
/// the triage answer is good enough.
pub const SKIP_THRESHOLD: f64 = 0.85;

/// `function ensurePrompt(db)` — find-or-create idempotency, identical shape to
/// interviewer-ai-fallback.rs's `ensurePrompt`.
pub fn ensurePrompt(db: &Connection) -> Result<Value, String> {
    let find_input = json!({ "name": PROMPT_NAME });
    if let Ok(found) = crate::ai_prompt_find::promptFind(&find_input, db) {
        return Ok(found);
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "low",
            "default_temperature": "cold",
            "max_output_tokens": 900,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&find_input, db)
}

/// The stub branch's canned response (JS: literal object returned when
/// `ANTILOKI_INTERVIEWER_QUESTIONNAIRE_STUB=1`, no AI call). Pure, no inputs — factored out so the
/// exact shape is independently assertable.
pub fn stub_response() -> Value {
    json!({
        "should_render": true,
        "own_confidence": 0.3,
        "questions": [
            {
                "question_md": "What are you trying to do?",
                "options": [
                    { "key": "A", "label": "See the current board" },
                    { "key": "B", "label": "File a new card" },
                    { "key": "C", "label": "Look up a specific action" },
                ]
            }
        ],
        "ai_call_id": Value::Null,
        "ai_telemetry": {
            "phase": "clarify_questionnaire", "prompt_name": PROMPT_NAME,
            "model_id": "stub", "provider": "stub",
            "tier": "low", "temperature": "cold",
            "tokens_in": 0, "tokens_out": 0,
            "cost_usd": 0, "cached": false, "duration_ms": 0,
        }
    })
}

/// `output_kv` → `{own_confidence, questions}` shaping (mirrors the JS inline loop + the
/// `questions_json` JSON.parse/normalize block). Factored out as its own pure fn, same rationale
/// as interviewer-ai-fallback.rs's `collapse_output_kv`: the real `aiCall` leg is PARITY-DEFERRED,
/// so this is the only way to exercise the parsing surface natively.
///
/// Normalizes to `[{question_md, options:[{key,label}]}]`, capped 3 questions / 4 options per
/// question; options without a non-empty label are dropped; a question surviving with zero
/// options is dropped too (mirrors the JS `.filter((q) => q.question_md && q.options.length > 0)`).
/// `should_render` = `own_confidence < SKIP_THRESHOLD && questions.length > 0`; when false the
/// returned `questions` array is empty (JS: `should_render ? questions : []`).
pub fn shape_questionnaire(ai_result: &Value) -> Value {
    let mut out: Map<String, Value> = Map::new();
    if let Some(rows) = ai_result.get("output_kv").and_then(Value::as_array) {
        for row in rows {
            let Some(field_name) = row.get("field_name").and_then(Value::as_str) else { continue };
            if let Some(v) = row.get("value_text") {
                if !v.is_null() { out.insert(field_name.to_string(), v.clone()); continue; }
            }
            if let Some(v) = row.get("value_number") {
                if !v.is_null() { out.insert(field_name.to_string(), v.clone()); continue; }
            }
            if let Some(v) = row.get("value_boolean") {
                if !v.is_null() { out.insert(field_name.to_string(), json!(v.as_bool().unwrap_or(false))); continue; }
            }
        }
    }

    let own_confidence = out.get("own_confidence").and_then(Value::as_f64).unwrap_or(0.0);

    let raw_questions_json = out.get("questions_json").and_then(Value::as_str).unwrap_or("[]");
    let mut questions: Vec<Value> = Vec::new();
    if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(raw_questions_json) {
        for q in arr.iter().take(3) {
            let question_md: String = q.get("question").and_then(Value::as_str)
                .or_else(|| q.get("question_md").and_then(Value::as_str))
                .unwrap_or("")
                .chars().take(300).collect();
            let raw_options = q.get("options").and_then(Value::as_array).cloned().unwrap_or_default();
            let mut options: Vec<Value> = Vec::new();
            for (j, o) in raw_options.iter().take(4).enumerate() {
                let label: String = match o {
                    Value::String(s) => s.clone(),
                    other => other.get("label").and_then(Value::as_str).unwrap_or("").to_string(),
                }.chars().take(140).collect();
                if label.is_empty() { continue; }
                let key = char::from_u32(65 + j as u32).unwrap_or('?').to_string();
                options.push(json!({ "key": key, "label": label }));
            }
            if !question_md.is_empty() && !options.is_empty() {
                questions.push(json!({ "question_md": question_md, "options": options }));
            }
        }
    }

    let should_render = own_confidence < SKIP_THRESHOLD && !questions.is_empty();

    json!({
        "should_render": should_render,
        "own_confidence": own_confidence,
        "questions": if should_render { json!(questions) } else { json!([]) },
    })
}

/// `export default async function interviewerClarifyQuestionnaire(input, context)`. `context.db`
/// → `db: &Connection`; `context.provider_fn` has no Rust equivalent (see module doc) and is
/// dropped. The `ANTILOKI_INTERVIEWER_QUESTIONNAIRE_STUB=1` test-mode short-circuit is ported
/// verbatim (checked BEFORE `ensurePrompt`/the AI call, matching JS control flow).
pub fn interviewerClarifyQuestionnaire(input: &Value, db: &Connection) -> Result<Value, String> {
    let user_text = input.get("user_text").cloned().unwrap_or(Value::Null);
    let has_text = match &user_text {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        _ => true,
    };
    if !has_text {
        return Err("interviewer-clarify-questionnaire: user_text is required".to_string());
    }

    if std::env::var("ANTILOKI_INTERVIEWER_QUESTIONNAIRE_STUB").as_deref() == Ok("1") {
        return Ok(stub_response());
    }

    ensurePrompt(db)?;

    let intent_summary = input.get("intent_summary").cloned().unwrap_or(json!(""));
    let recent_turns = input.get("recent_turns").cloned().unwrap_or(json!(""));

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "user_text", "value": user_text },
            { "key": "intent_summary", "value": intent_summary },
            { "key": "recent_turns", "value": recent_turns },
        ],
    });
    // PARITY-DEFERRED boundary: crate::ai_ai_call::aiCall is a network stub that always returns
    // Err — see module doc. On a real provider response this would feed shape_questionnaire, then
    // the telemetry readback below.
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let shaped = shape_questionnaire(&ai_result);

    let ai_call_id = ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null);
    let mut tokens_in = Value::Null;
    let mut tokens_out = Value::Null;
    if let Some(id) = ai_call_id.as_str() {
        if let Ok(row) = db.query_row(
            "SELECT tokens_in, tokens_out FROM brain.ai_calls WHERE id = ?1",
            [id],
            |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?)),
        ) {
            tokens_in = row.0.map(Value::from).unwrap_or(Value::Null);
            tokens_out = row.1.map(Value::from).unwrap_or(Value::Null);
        }
    }

    Ok(json!({
        "should_render": shaped["should_render"],
        "own_confidence": shaped["own_confidence"],
        "questions": shaped["questions"],
        "ai_call_id": ai_call_id,
        "ai_telemetry": {
            "phase": "clarify_questionnaire",
            "prompt_name": PROMPT_NAME,
            "model_id": ai_result.get("model_id").cloned().unwrap_or(Value::Null),
            "provider": ai_result.get("provider").cloned().unwrap_or(Value::Null),
            "tier": ai_result.get("tier_used").cloned().unwrap_or(json!("low")),
            "temperature": ai_result.get("temperature_used").cloned().unwrap_or(json!("cold")),
            "tokens_in": tokens_in,
            "tokens_out": tokens_out,
            "cost_usd": ai_result.get("cost_usd").cloned().unwrap_or(json!(0)),
            "cached": ai_result.get("cached").cloned().unwrap_or(json!(false)),
            "duration_ms": ai_result.get("duration_ms").cloned().unwrap_or(Value::Null),
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env_lock::ENV_LOCK;

    // Schema copied verbatim from interviewer-ai-fallback.rs's own test fixture — both twins this
    // module composes (prompt-find/prompt-add) are exercised against exactly this shape.
    fn open_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("ATTACH DATABASE ':memory:' AS brain").unwrap();
        db.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        )
        .unwrap();
        db
    }

    fn clear_stub_env() {
        std::env::remove_var("ANTILOKI_INTERVIEWER_QUESTIONNAIRE_STUB");
    }

    #[test]
    fn missing_user_text_errors() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_stub_env();
        let db = open_db();
        let e = interviewerClarifyQuestionnaire(&json!({}), &db).unwrap_err();
        assert!(e.contains("user_text is required"));
        let e2 = interviewerClarifyQuestionnaire(&json!({ "user_text": "" }), &db).unwrap_err();
        assert!(e2.contains("user_text is required"));
    }

    #[test]
    fn stub_env_short_circuits_before_any_db_write() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("ANTILOKI_INTERVIEWER_QUESTIONNAIRE_STUB", "1");
        let db = open_db();
        let out = interviewerClarifyQuestionnaire(&json!({ "user_text": "hi" }), &db).unwrap();
        assert_eq!(out, stub_response());
        // No prompt row was created — the stub branch never calls ensurePrompt.
        let count: i64 = db.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
        clear_stub_env();
    }

    #[test]
    fn ensure_prompt_creates_then_finds_idempotently() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_stub_env();
        let db = open_db();
        let first = ensurePrompt(&db).unwrap();
        assert_eq!(first["name"], json!(PROMPT_NAME));
        let second = ensurePrompt(&db).unwrap();
        assert_eq!(second["id"], first["id"], "second call finds, does not re-add");
    }

    #[test]
    fn deferred_ai_call_leg_propagates_the_stub_error() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_stub_env();
        let db = open_db();
        let e = interviewerClarifyQuestionnaire(&json!({ "user_text": "do the thing" }), &db).unwrap_err();
        assert!(e.contains("ai-call"), "propagates ai-call.rs's deferred-stub error: {e}");
    }

    // ── shape_questionnaire (pure response-shaping surface) ─────────────────────────────────
    #[test]
    fn shape_normalizes_questions_and_caps_counts() {
        let ai_result = json!({
            "output_kv": [
                { "field_name": "own_confidence", "value_number": 0.3 },
                { "field_name": "questions_json", "value_text": serde_json::to_string(&json!([
                    { "question": "What are you trying to do?", "options": ["A", "B", "C", "D", "E"] },
                    { "question": "Q2", "options": ["x", "y"] },
                    { "question": "Q3", "options": ["z"] },
                    { "question": "Q4 dropped by cap", "options": ["w"] },
                ])).unwrap() },
            ]
        });
        let out = shape_questionnaire(&ai_result);
        assert_eq!(out["should_render"], json!(true));
        assert_eq!(out["own_confidence"], json!(0.3));
        let qs = out["questions"].as_array().unwrap();
        assert_eq!(qs.len(), 3, "capped at 3 questions");
        assert_eq!(qs[0]["question_md"], json!("What are you trying to do?"));
        let opts = qs[0]["options"].as_array().unwrap();
        assert_eq!(opts.len(), 4, "capped at 4 options");
        assert_eq!(opts[0], json!({ "key": "A", "label": "A" }));
        assert_eq!(opts[3], json!({ "key": "D", "label": "D" }));
    }

    #[test]
    fn shape_drops_options_without_a_label_and_questions_left_with_none() {
        let ai_result = json!({
            "output_kv": [
                { "field_name": "own_confidence", "value_number": 0.2 },
                { "field_name": "questions_json", "value_text": serde_json::to_string(&json!([
                    { "question": "Kept", "options": ["", "real label"] },
                    { "question": "Dropped entirely", "options": [""] },
                ])).unwrap() },
            ]
        });
        let out = shape_questionnaire(&ai_result);
        let qs = out["questions"].as_array().unwrap();
        assert_eq!(qs.len(), 1);
        assert_eq!(qs[0]["question_md"], json!("Kept"));
        assert_eq!(qs[0]["options"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn shape_should_render_false_when_confidence_at_or_above_threshold() {
        let ai_result = json!({
            "output_kv": [
                { "field_name": "own_confidence", "value_number": 0.85 },
                { "field_name": "questions_json", "value_text": serde_json::to_string(&json!([
                    { "question": "Would still be valid", "options": ["a"] },
                ])).unwrap() },
            ]
        });
        let out = shape_questionnaire(&ai_result);
        assert_eq!(out["should_render"], json!(false));
        assert_eq!(out["questions"], json!([]), "hidden when should_render is false, even though parsing succeeded");
    }

    #[test]
    fn shape_should_render_false_when_questions_empty_even_at_low_confidence() {
        let ai_result = json!({
            "output_kv": [
                { "field_name": "own_confidence", "value_number": 0.1 },
                { "field_name": "questions_json", "value_text": "[]" },
            ]
        });
        let out = shape_questionnaire(&ai_result);
        assert_eq!(out["should_render"], json!(false));
    }

    #[test]
    fn shape_malformed_questions_json_yields_no_questions_not_an_error() {
        let ai_result = json!({
            "output_kv": [
                { "field_name": "own_confidence", "value_number": 0.1 },
                { "field_name": "questions_json", "value_text": "{not json" },
            ]
        });
        let out = shape_questionnaire(&ai_result);
        assert_eq!(out["questions"], json!([]));
        assert_eq!(out["should_render"], json!(false));
    }

    #[test]
    fn shape_missing_output_kv_defaults_to_zero_confidence_and_no_render() {
        let out = shape_questionnaire(&json!({}));
        assert_eq!(out["own_confidence"], json!(0.0));
        assert_eq!(out["should_render"], json!(false));
        assert_eq!(out["questions"], json!([]));
    }
}
