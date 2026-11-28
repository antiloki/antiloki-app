//! investigate-present.rs — twin of skills/agents/in/investigate-present.mjs (db READ+WRITE,
//! brain-attached; LLM-PARTIAL). NAMES ARE CONTRACT: `investigatePresent` (default), `PROMPT_NAME`,
//! `PROMPT_TEMPLATE_MD`, `PROMPT_OUTPUT_FIELDS` (as `prompt_output_fields()`, a fn — array literal,
//! same convention as interviewer-clarify-questionnaire.rs's `prompt_output_fields`).
//!
//! PARITY-DEFERRED: the `aiCall(...)` leg is a real network call to a BYOK LLM provider —
//! genuinely non-deterministic, unfixturable in principle (see `crate::ai_ai_call::aiCall`'s own
//! doc). This twin calls that deferred stub, which always returns `Err(..)`, so the real happy
//! path (an actual model response reaching the output-shaping block) cannot be exercised by a
//! native test — same limitation as interviewer-clarify-questionnaire.rs's identical boundary.
//! Everything else is ported faithfully: the `db`/`question`/`answer` guards, the no-evidence
//! pass-through short-circuit, the `ANTILOKI_INVESTIGATE_PRESENT_STUB=1` test-mode short-circuit,
//! prompt upsert idempotency (`ensurePrompt`: find-or-create via `crate::ai_prompt_find`/
//! `crate::ai_prompt_add`), the `key_points_block`/`citations_block` prompt-assembly strings, the
//! `buildViewData` citation-lookup shaping, the `follow_ups_json` parse, and the telemetry token
//! readback from `brain.ai_calls`.
//!
//! `provider_fn` (the JS test-injection hook for a mock provider) has no Rust equivalent — it only
//! ever reaches the deferred `aiCall` body, so it is dropped from this twin's signature, same as
//! interviewer-clarify-questionnaire.rs drops it.
//!
//! `buildViewData` and the `follow_ups_json` parse are factored out as their own pure fns (mirrors
//! interviewer-clarify-questionnaire.rs's `shape_questionnaire`) so the deterministic
//! parsing/shaping surface is independently testable against a canned response, since the real
//! `aiCall` leg can never produce one here.
// QUALITY: mixed unit — guards + no-evidence short-circuit + stub branch + prompt-assembly +
// response-shaping ported/fixture-tested; aiCall leg PARITY-DEFERRED (network, unfixturable)

use rusqlite::Connection;
use serde_json::{json, Map, Value};

pub const PROMPT_NAME: &str = "interviewer.investigate.present.v1";

pub const PROMPT_TEMPLATE_MD: &str = "You are the PRESENTER. The investigator produced a structured answer with citations. Translate it into user-facing prose. NO new reasoning — you only re-phrase what's already in the answer.\n\nQUESTION:\n{{question}}\n\nANSWER SUMMARY:\n{{summary_md}}\n\nKEY POINTS:\n{{key_points_block}}\n\nCITATIONS:\n{{citations_block}}\n\nOutput JSON of this exact shape:\n{\n  \"presentation_md\": \"<2-4 sentence natural-language reply, keeping all citation refs visible as [ref]>\",\n  \"follow_ups_json\": \"<JSON array of 2-3 {text, suggested_action?} objects — natural next-step questions the user might ask>\"\n}\n\nRules:\n  - Preserve [ref] citations from the input verbatim. Do NOT invent new refs.\n  - presentation_md is a chat bubble — concise, no headers, no bullets.\n  - follow_ups are SHORT (5-10 words). suggested_action is optional and only set when a specific substrate action would naturally answer the follow-up.";

/// `PROMPT_OUTPUT_FIELDS` (JS `export const` array literal, 2 entries) as a fn — see module doc.
pub fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "presentation_md", "field_type": "string", "required": true },
        { "field_name": "follow_ups_json", "field_type": "string", "required": false },
    ])
}

/// `function ensurePrompt(db)` — find-or-create idempotency, identical shape to
/// interviewer-clarify-questionnaire.rs's `ensurePrompt`.
pub fn ensurePrompt(db: &Connection) -> Result<Value, String> {
    let find_input = json!({ "name": PROMPT_NAME });
    if let Ok(found) = crate::ai_prompt_find::promptFind(&find_input, db) {
        return Ok(found);
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "mid",
            "default_temperature": "cold",
            "max_output_tokens": 700,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&find_input, db)
}

/// `function parseJsonArray(s)` — array pass-through, JSON.parse of a string when it decodes to an
/// array, `[]` otherwise (non-string, parse failure, or a non-array JSON value). Pure.
pub fn parseJsonArray(s: &Value) -> Value {
    if let Some(arr) = s.as_array() { return Value::Array(arr.clone()); }
    let Some(text) = s.as_str() else { return json!([]) };
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Array(a)) => Value::Array(a),
        _ => json!([]),
    }
}

/// `function buildViewData(question, answer, citations, findings, follow_ups)` — builds the
/// `citations[]` keyed by ref for the TUI render, looking up the original finding snippet
/// (`findings` matched by `ref`) so the user sees the source content inline. Pure.
pub fn buildViewData(question: &Value, answer: &Value, citations: &Value, findings: &Value, follow_ups: &Value) -> Value {
    let findings_arr = findings.as_array().cloned().unwrap_or_default();
    let cit_arr = citations.as_array().cloned().unwrap_or_default();
    let cit: Vec<Value> = cit_arr.iter().map(|r| {
        let found = findings_arr.iter().find(|f| f.get("ref") == Some(r));
        json!({
            "ref": r.clone(),
            "source": found.and_then(|f| f.get("source")).cloned().unwrap_or(json!("unknown")),
            "snippet": found.and_then(|f| f.get("snippet")).cloned().unwrap_or(json!("")),
        })
    }).collect();

    json!({
        "question": question.clone(),
        "answer_md": answer.get("summary_md").cloned().unwrap_or(json!("")),
        "key_points": answer.get("key_points").cloned().unwrap_or(json!([])),
        "confidence": answer.get("confidence").cloned().unwrap_or(json!(0)),
        "citations": cit,
        "follow_ups": follow_ups.clone(),
    })
}

/// `export default async function investigatePresent(input, context)`. `context.db` →
/// `db: &Connection`; `context.provider_fn` has no Rust equivalent (see module doc) and is dropped.
pub fn investigatePresent(input: &Value, db: &Connection) -> Result<Value, String> {
    let question = input.get("question").cloned().unwrap_or(Value::Null);
    let has_question = match &question {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        _ => true,
    };
    if !has_question {
        return Err("investigate-present: question is required".to_string());
    }
    let answer = input.get("answer").cloned().unwrap_or(Value::Null);
    if answer.is_null() {
        return Err("investigate-present: answer is required".to_string());
    }
    let citations = input.get("citations").cloned().unwrap_or(json!([]));
    let findings = input.get("findings").cloned().unwrap_or(json!([]));

    // No-evidence pass-through — present whatever summary_md already exists (typically the
    // substrate's no-evidence default). Saves an AI call.
    let summary_md = answer.get("summary_md").and_then(Value::as_str).unwrap_or("");
    let confidence = answer.get("confidence").and_then(Value::as_f64).unwrap_or(0.0);
    if summary_md.is_empty() || confidence == 0.0 {
        let presentation_md = if summary_md.is_empty() { "(no answer)".to_string() } else { summary_md.to_string() };
        return Ok(json!({
            "presentation_md": presentation_md,
            "view_intent": "investigation",
            "view_data": buildViewData(&question, &answer, &citations, &findings, &json!([])),
            "ai_call_id": Value::Null,
            "ai_telemetry": Value::Null,
        }));
    }

    // Test-mode stub.
    if std::env::var("ANTILOKI_INVESTIGATE_PRESENT_STUB").as_deref() == Ok("1") {
        return Ok(json!({
            "presentation_md": format!("(stub) {summary_md}"),
            "view_intent": "investigation",
            "view_data": buildViewData(&question, &answer, &citations, &findings, &json!([])),
            "ai_call_id": Value::Null,
            "ai_telemetry": {
                "phase": "investigate_present", "prompt_name": PROMPT_NAME,
                "model_id": "stub", "provider": "stub",
                "tier": "mid", "temperature": "cold",
                "tokens_in": 0, "tokens_out": 0, "cost_usd": 0, "cached": false, "duration_ms": 0,
            }
        }));
    }

    ensurePrompt(db)?;

    let key_points_block = answer.get("key_points").and_then(Value::as_array).cloned().unwrap_or_default()
        .iter().enumerate()
        .map(|(i, p)| format!("{}. {}", i + 1, p.as_str().unwrap_or_default()))
        .collect::<Vec<_>>().join("\n");

    let findings_arr = findings.as_array().cloned().unwrap_or_default();
    let citations_block = citations.as_array().cloned().unwrap_or_default()
        .iter()
        .map(|r| {
            match findings_arr.iter().find(|x| x.get("ref") == Some(r)) {
                Some(f) => {
                    let source = f.get("source").and_then(Value::as_str).unwrap_or("");
                    let snippet: String = f.get("snippet").and_then(Value::as_str).unwrap_or("").chars().take(100).collect();
                    format!("[{}] ({source}) {snippet}", r.as_str().unwrap_or_default())
                }
                None => format!("[{}]", r.as_str().unwrap_or_default()),
            }
        })
        .collect::<Vec<_>>().join("\n");

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "question",         "value": question },
            { "key": "summary_md",       "value": summary_md },
            { "key": "key_points_block", "value": key_points_block },
            { "key": "citations_block",  "value": citations_block },
        ],
        "tier": "mid",
        "temperature": "cold",
    });
    // PARITY-DEFERRED boundary: crate::ai_ai_call::aiCall is a network stub that always returns
    // Err — see module doc. On a real provider response this would feed the output-shaping block
    // below, then the telemetry readback.
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

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

    let follow_ups_json = out.get("follow_ups_json").cloned().unwrap_or(Value::Null);
    let follow_ups = parseJsonArray(&follow_ups_json);

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
        "presentation_md": out.get("presentation_md").cloned().unwrap_or(json!(summary_md)),
        "view_intent": "investigation",
        "view_data": buildViewData(&question, &answer, &citations, &findings, &follow_ups),
        "ai_call_id": ai_call_id,
        "ai_telemetry": {
            "phase": "investigate_present",
            "prompt_name": PROMPT_NAME,
            "model_id": ai_result.get("model_id").cloned().unwrap_or(Value::Null),
            "provider": ai_result.get("provider").cloned().unwrap_or(Value::Null),
            "tier": ai_result.get("tier_used").cloned().unwrap_or(json!("mid")),
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

    // Schema copied verbatim from interviewer-clarify-questionnaire.rs's own test fixture — both
    // twins this module composes (prompt-find/prompt-add) are exercised against exactly this shape.
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
        std::env::remove_var("ANTILOKI_INVESTIGATE_PRESENT_STUB");
    }

    #[test]
    fn missing_question_or_answer_errors() {
        let db = open_db();
        let e = investigatePresent(&json!({}), &db).unwrap_err();
        assert!(e.contains("question is required"));
        let e2 = investigatePresent(&json!({ "question": "" }), &db).unwrap_err();
        assert!(e2.contains("question is required"));
        let e3 = investigatePresent(&json!({ "question": "why?" }), &db).unwrap_err();
        assert!(e3.contains("answer is required"));
    }

    #[test]
    fn no_evidence_pass_through_skips_ai_call_when_summary_missing() {
        let db = open_db();
        let out = investigatePresent(&json!({ "question": "why?", "answer": {} }), &db).unwrap();
        assert_eq!(out["presentation_md"], json!("(no answer)"));
        assert_eq!(out["ai_call_id"], Value::Null);
        assert_eq!(out["ai_telemetry"], Value::Null);
        assert_eq!(out["view_intent"], json!("investigation"));
        let count: i64 = db.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0, "no prompt row created on the pass-through path");
    }

    #[test]
    fn no_evidence_pass_through_skips_ai_call_when_confidence_zero() {
        let db = open_db();
        let answer = json!({ "summary_md": "No cards match.", "confidence": 0 });
        let out = investigatePresent(&json!({ "question": "why?", "answer": answer }), &db).unwrap();
        assert_eq!(out["presentation_md"], json!("No cards match."));
        assert_eq!(out["ai_call_id"], Value::Null);
    }

    #[test]
    fn stub_env_short_circuits_before_ensure_prompt() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("ANTILOKI_INVESTIGATE_PRESENT_STUB", "1");
        let db = open_db();
        let answer = json!({ "summary_md": "The card is done.", "confidence": 0.9 });
        let out = investigatePresent(&json!({ "question": "is it done?", "answer": answer }), &db).unwrap();
        assert_eq!(out["presentation_md"], json!("(stub) The card is done."));
        assert_eq!(out["ai_telemetry"]["model_id"], json!("stub"));
        let count: i64 = db.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0, "stub branch never calls ensurePrompt");
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
        let answer = json!({ "summary_md": "Real answer text.", "confidence": 0.7 });
        let e = investigatePresent(&json!({ "question": "why?", "answer": answer }), &db).unwrap_err();
        assert!(e.contains("ai-call"), "propagates ai-call.rs's deferred-stub error: {e}");
    }

    // ── buildViewData (pure shaping surface) ────────────────────────────────────────────────
    #[test]
    fn build_view_data_looks_up_citation_snippets_by_ref() {
        let answer = json!({ "summary_md": "Summary text.", "key_points": ["a", "b"], "confidence": 0.8 });
        let findings = json!([
            { "ref": "F1", "source": "CARD-A", "snippet": "some evidence" },
            { "ref": "F2", "source": "CARD-B", "snippet": "other evidence" },
        ]);
        let citations = json!(["F1", "F2", "F9"]);
        let out = buildViewData(&json!("why?"), &answer, &citations, &findings, &json!([]));
        assert_eq!(out["question"], json!("why?"));
        assert_eq!(out["answer_md"], json!("Summary text."));
        assert_eq!(out["key_points"], json!(["a", "b"]));
        assert_eq!(out["confidence"], json!(0.8));
        let cit = out["citations"].as_array().unwrap();
        assert_eq!(cit.len(), 3);
        assert_eq!(cit[0], json!({ "ref": "F1", "source": "CARD-A", "snippet": "some evidence" }));
        assert_eq!(cit[2], json!({ "ref": "F9", "source": "unknown", "snippet": "" }), "unmatched ref falls back");
    }

    // ── parseJsonArray (pure) ────────────────────────────────────────────────────────────────
    #[test]
    fn parse_json_array_branches() {
        assert_eq!(parseJsonArray(&json!([1, 2])), json!([1, 2]));
        assert_eq!(parseJsonArray(&json!("[1,2]")), json!([1, 2]));
        assert_eq!(parseJsonArray(&json!("not json")), json!([]));
        assert_eq!(parseJsonArray(&json!("{\"a\":1}")), json!([]), "object, not array");
        assert_eq!(parseJsonArray(&Value::Null), json!([]));
        assert_eq!(parseJsonArray(&json!(42)), json!([]));
    }
}
