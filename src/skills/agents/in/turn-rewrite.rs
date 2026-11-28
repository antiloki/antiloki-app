//! turn-rewrite.rs — twin of skills/agents/in/turn-rewrite.mjs (db WRITE lane, brain-attached).
//! NAMES ARE CONTRACT: `turnRewrite` (default). Simplify/Expand ONE persisted chat reply
//! (`interviewer_turns.interviewer_response_md`) — display-only, the turn row is never mutated.
//!
//! PARITY-DEFERRED boundary: the `aiCall` dispatch (`crate::ai_ai_call::aiCall`) is the same
//! always-`Err` stub documented in `ai-call.rs` — a real network round-trip to an LLM provider,
//! genuinely non-deterministic and unfixturable in principle. JS re-throws any aiCall failure
//! whose `e.code !== 'VALIDATION_LOGIC'`; the deferred stub's error is never that code, so this
//! twin's `?` propagation after the call mirrors the JS rethrow exactly. Everything BEFORE the
//! call — turn_id/operation guards, the row fetch + truncation, `ensurePrompt` (idempotent
//! registration via the already-converted `ai_prompt_find`/`ai_prompt_add` twins), and the
//! `input_kv` assembly — is a REAL, observable, ported side effect (native tests assert the
//! prompt row lands even though the call itself always fails here). The `Ok(res)` branch below
//! (output-field extraction, the `extractAnswerFromRaw` salvage parser, the validation-failed
//! fallback query, and cost/model telemetry) is ported FAITHFULLY for when a real provider lands
//! behind `ai_ai_call::aiCall`, but is unreachable through the public API today — same convention
//! as `man-diagnose-failure.rs`'s `interpret()`. `extractAnswerFromRaw` is exercised directly by
//! native tests since it is a pure function of an already-produced response value.
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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
// = JS Number(v) (turn_id coercion).
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) }
        }
        Some(_) => f64::NAN,
    }
}
// = JS `.slice(0,n)` — chars-based approximation, exact for ASCII/BMP content (same convention as
// man-diagnose-failure.rs's js_slice).
fn js_slice(s: &str, n: usize) -> String { s.chars().take(n).collect() }
fn js_len(s: &str) -> usize { s.encode_utf16().count() }

const PROMPT_NAME: &str = "system.turn.rewrite.v1";

const PROMPT_TEMPLATE_MD: &str = "You are rewriting ONE chat answer for an operator, on demand. Rewrite it per the instruction, grounded ONLY in the original answer (the question is orientation). DO NOT invent facts, files, numbers, or claims the original answer does not contain.

RULES:
  1. When simplifying: re-express the SAME content in genuinely plainer, shorter language — noticeably clearer, every key fact kept, filler dropped.
  2. When expanding: elaborate the SAME content — unpack the reasoning, structure with headings/bullets where it helps, spell out implications the original states or directly implies. No new facts.
  3. Keep markdown. Keep code, identifiers, paths, and numbers verbatim.
  4. No preamble (\"Here is…\"), no meta-commentary — output the rewritten answer only.

OPERATOR QUESTION (orientation only):
{{user_text}}

ORIGINAL ANSWER (the source of truth — rewrite THIS):
{{original_md}}

INSTRUCTION: {{instruction}}

OUTPUT — return ONLY a JSON object of this exact shape (no prose, no markdown fences):
{
  \"answer_md\": \"<the rewritten answer in markdown>\"
}";

// A rewrite worth doing needs the whole answer, but a pathological reply shouldn't blow the prompt.
const MAX_ORIGINAL_CHARS: usize = 12000;

fn operation_instruction(op: &str) -> Option<&'static str> {
    match op {
        "simplify" => Some("Simplify this answer — genuinely plainer and shorter, keep every key fact."),
        "expand" => Some("Expand this answer — more depth and structure, grounded only in what it already says."),
        _ => None,
    }
}

fn output_fields() -> Value {
    json!([{ "field_name": "answer_md", "field_type": "string", "required": true }])
}

/// `function ensurePrompt(db)` — JS `try { promptFind(...) } catch { promptAdd(...) }`.
fn ensure_prompt(db: &Connection) {
    if crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).is_ok() {
        return;
    }
    let _ = crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "mid",
            "default_temperature": "cold",
            "max_output_tokens": 1400,
            "output_fields": output_fields(),
        }),
        db,
    );
}

/// `function extractAnswerFromRaw(raw)` — pure salvage parser for reasoning-model dumps that put
/// the real `{"answer_md": …}` payload at the END (often fenced). Candidates tried last-first:
/// every fenced block, then every `{"answer_md"` start, then the legacy whole-span match. A clean
/// answer with no extractable payload passes through unchanged (identity for normal replies).
fn extract_answer_from_raw(raw: Option<&Value>) -> String {
    let text = match raw {
        None | Some(Value::Null) => return String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => serde_json::to_string(v).unwrap_or_else(|_| str_qq(Some(v))),
    };
    let text = text.trim();

    let answer_of = |v: &Value| -> Option<String> {
        let obj = v.as_object()?;
        let a = obj.get("answer_md")?.as_str()?;
        let t = a.trim();
        // reject the prompt's own echoed placeholder ("<the rewritten answer in markdown>").
        if t.is_empty() { return None; }
        let placeholder_re = regex::Regex::new(r"^<[^>]*>$").unwrap();
        if placeholder_re.is_match(t) { return None; }
        Some(a.to_string())
    };
    let try_parse = |s: &str| -> Option<Value> { serde_json::from_str(s).ok() };

    let mut candidates: Vec<String> = Vec::new();
    let fence_re = regex::Regex::new(r"(?is)```(?:json)?\s*(.*?)```").unwrap();
    for cap in fence_re.captures_iter(text) {
        candidates.insert(0, cap[1].trim().to_string());
    }
    let start_re = regex::Regex::new(r#"\{\s*"answer_md""#).unwrap();
    for m in start_re.find_iter(text) {
        candidates.insert(0, text[m.start()..].to_string());
    }
    let whole_re = regex::Regex::new(r"(?s)\{.*\}").unwrap();
    if let Some(m) = whole_re.find(text) {
        candidates.push(m.as_str().to_string());
    }

    for cand in &candidates {
        let end = cand.rfind('}');
        let a = end
            .and_then(|e| try_parse(&cand[..=e]))
            .and_then(|v| answer_of(&v))
            .or_else(|| try_parse(cand).and_then(|v| answer_of(&v)));
        if let Some(a) = a {
            return a;
        }
    }
    // no extractable payload: a clean prose/markdown reply IS the answer — strip boundary fences only.
    let strip_lead = regex::Regex::new(r"(?i)^```(?:json)?\s*").unwrap();
    let strip_trail = regex::Regex::new(r"(?i)\s*```$").unwrap();
    let s = strip_lead.replace(text, "");
    let s = strip_trail.replace(&s, "");
    s.trim().to_string()
}

/// `original.length > MAX_ORIGINAL_CHARS` truncation → `original_md`.
fn build_original_md(original: &str) -> String {
    if js_len(original) > MAX_ORIGINAL_CHARS {
        format!("{}\n…(truncated)", js_slice(original, MAX_ORIGINAL_CHARS))
    } else {
        original.to_string()
    }
}

#[allow(non_snake_case)]
pub fn turnRewrite(input: &Value, db: &Connection) -> Result<Value, String> {
    let turn_id_f = js_number(input.get("turn_id"));
    if !turn_id_f.is_finite() || turn_id_f.fract() != 0.0 || turn_id_f <= 0.0 {
        return Err("turn-rewrite: turn_id is required".to_string());
    }
    let turn_id = turn_id_f as i64;

    let operation = input.get("operation").and_then(Value::as_str).unwrap_or("");
    let instruction = match operation_instruction(operation) {
        Some(i) => i,
        None => {
            return Err(format!(
                "turn-rewrite: operation must be one of: simplify, expand (got '{}')",
                tmpl(input.get("operation"))
            ))
        }
    };

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, user_text, interviewer_response_md FROM interviewer_turns WHERE id = ?",
        &[json!(turn_id)],
    )?;
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("turn-rewrite: turn '{turn_id}' not found")),
    };
    let original = str_qq(row.get("interviewer_response_md")).trim().to_string();
    if original.is_empty() {
        return Err(format!("turn-rewrite: reply on turn '{turn_id}' not found"));
    }
    let original_md = build_original_md(&original);

    ensure_prompt(db);
    let input_kv = json!([
        { "key": "user_text", "value": str_qq(row.get("user_text")) },
        { "key": "original_md", "value": original_md },
        { "key": "instruction", "value": instruction },
    ]);

    let template_id = crate::db_json::query_rows(db, "SELECT id FROM brain.prompt_templates WHERE name = ?", &[json!(PROMPT_NAME)])?
        .as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).cloned().unwrap_or(Value::Null);
    let pre_call_max_id = crate::db_json::query_rows(db, "SELECT COALESCE(MAX(id), 0) AS m FROM brain.ai_calls", &[])?
        .as_array().and_then(|a| a.first()).and_then(|r| r.get("m")).cloned().unwrap_or(json!(0));

    let call_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": input_kv,
        "intent": "system.turn.rewrite",
    });

    // aiCall is the PARITY-DEFERRED boundary (see module doc): its error is never
    // VALIDATION_LOGIC, so JS's `if (e.code !== 'VALIDATION_LOGIC') throw e;` always rethrows —
    // mirrored here by `?`. Below is the ported Ok(res) branch, unreachable while aiCall stubs.
    let ai_result = crate::ai_ai_call::aiCall(&call_input, db)?;

    let mut answer_md = String::new();
    let mut ai_call_id = ai_result.get("ai_call_id").cloned().filter(|v| !v.is_null());
    if let Some(outs) = ai_result.get("output_kv").and_then(Value::as_array) {
        for o in outs {
            if o.get("field_name").and_then(Value::as_str) == Some("answer_md") {
                if let Some(t) = o.get("value_text") {
                    if !t.is_null() {
                        answer_md = str_qq(Some(t));
                    }
                }
            }
        }
    }
    if !answer_md.is_empty() {
        answer_md = extract_answer_from_raw(Some(&json!(answer_md)));
    }

    if answer_md.is_empty() && !template_id.is_null() {
        let fail_row = crate::db_json::query_rows(
            db,
            "SELECT id FROM brain.ai_calls WHERE prompt_template_id = ? AND id > ? AND outcome = 'validation_failed' ORDER BY id DESC LIMIT 1",
            &[template_id.clone(), pre_call_max_id.clone()],
        )?
            .as_array().and_then(|a| a.first()).cloned();
        if let Some(fr) = fail_row {
            let fr_id = fr.get("id").cloned().unwrap_or(Value::Null);
            if ai_call_id.is_none() { ai_call_id = Some(fr_id.clone()); }
            let raw_row = crate::db_json::query_rows(
                db,
                "SELECT value_text FROM brain.ai_call_outputs WHERE ai_call_id = ? AND field_name = '__raw_output_on_validation_fail__' ORDER BY id DESC LIMIT 1",
                &[fr_id],
            )?
                .as_array().and_then(|a| a.first()).and_then(|r| r.get("value_text")).cloned();
            if let Some(rt) = raw_row {
                if truthy(Some(&rt)) {
                    answer_md = extract_answer_from_raw(Some(&rt));
                }
            }
        }
    }

    let mut cost_usd: f64 = 0.0;
    let mut model_id = Value::Null;
    if let Some(id) = &ai_call_id {
        if let Ok(rows) = crate::db_json::query_rows(db, "SELECT cost_usd_actual, model_id FROM brain.ai_calls WHERE id = ?", &[id.clone()]) {
            if let Some(r) = rows.as_array().and_then(|a| a.first()) {
                cost_usd = r.get("cost_usd_actual").and_then(Value::as_f64).unwrap_or(0.0);
                model_id = r.get("model_id").cloned().unwrap_or(Value::Null);
            }
        }
    }

    Ok(json!({
        "turn_id": turn_id,
        "operation": operation,
        "answer_md": if answer_md.is_empty() { "(no rewrite produced)".to_string() } else { answer_md },
        "ai_call_id": ai_call_id.unwrap_or(Value::Null),
        "cost_usd": cost_usd,
        "model_id": model_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY, user_text TEXT, interviewer_response_md TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, ts TEXT, prompt_template_id INTEGER, tier TEXT, temperature TEXT, provider TEXT, model_id TEXT, outcome TEXT, tokens_in INTEGER, tokens_out INTEGER, duration_ms REAL, cost_usd_actual REAL, cached INTEGER, parent_event_id INTEGER);
             CREATE TABLE brain.ai_call_outputs (id INTEGER PRIMARY KEY, ai_call_id INTEGER, field_name TEXT, value_text TEXT, value_number REAL, value_boolean INTEGER, order_index INTEGER, parent_output_id INTEGER);",
        ).unwrap();
        c
    }

    fn seed_turn(c: &Connection, user_text: &str, reply: Option<&str>) -> i64 {
        c.execute("INSERT INTO interviewer_turns (user_text, interviewer_response_md) VALUES (?, ?)", rusqlite::params![user_text, reply]).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn missing_turn_id_returns_exact_message() {
        let c = db();
        assert_eq!(turnRewrite(&json!({}), &c).unwrap_err(), "turn-rewrite: turn_id is required");
        assert_eq!(turnRewrite(&json!({ "turn_id": -1 }), &c).unwrap_err(), "turn-rewrite: turn_id is required");
        assert_eq!(turnRewrite(&json!({ "turn_id": 1.5 }), &c).unwrap_err(), "turn-rewrite: turn_id is required");
    }

    #[test]
    fn invalid_operation_returns_exact_enum_message() {
        let c = db();
        let e = turnRewrite(&json!({ "turn_id": 1, "operation": "delete" }), &c).unwrap_err();
        assert_eq!(e, "turn-rewrite: operation must be one of: simplify, expand (got 'delete')");
    }

    #[test]
    fn unknown_turn_id_returns_exact_not_found_message() {
        let c = db();
        let e = turnRewrite(&json!({ "turn_id": 999, "operation": "simplify" }), &c).unwrap_err();
        assert_eq!(e, "turn-rewrite: turn '999' not found");
    }

    #[test]
    fn empty_reply_returns_exact_not_found_message() {
        let c = db();
        let id = seed_turn(&c, "hi", None);
        let e = turnRewrite(&json!({ "turn_id": id, "operation": "simplify" }), &c).unwrap_err();
        assert_eq!(e, format!("turn-rewrite: reply on turn '{id}' not found"));
        let id2 = seed_turn(&c, "hi", Some("   "));
        let e2 = turnRewrite(&json!({ "turn_id": id2, "operation": "simplify" }), &c).unwrap_err();
        assert_eq!(e2, format!("turn-rewrite: reply on turn '{id2}' not found"));
    }

    #[test]
    fn happy_path_registers_prompt_then_propagates_deferred_ai_call_error() {
        let c = db();
        let id = seed_turn(&c, "what is X?", Some("X is Y."));
        let r = turnRewrite(&json!({ "turn_id": id, "operation": "expand" }), &c);
        assert!(r.is_err(), "aiCall is a deferred stub (always Err) — this twin propagates it");
        // the deterministic prefix (ensurePrompt) still ran as a real, observable side effect.
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT_NAME], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_is_idempotent() {
        let c = db();
        ensure_prompt(&c);
        ensure_prompt(&c);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT_NAME], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn build_original_md_passes_through_short_text() {
        assert_eq!(build_original_md("hello"), "hello");
    }

    #[test]
    fn build_original_md_truncates_long_text() {
        let long = "x".repeat(MAX_ORIGINAL_CHARS + 500);
        let out = build_original_md(&long);
        assert!(out.ends_with("\n…(truncated)"));
        assert_eq!(js_len(&out) - "\n…(truncated)".chars().count(), MAX_ORIGINAL_CHARS);
    }

    #[test]
    fn extract_answer_from_raw_direct_object() {
        let raw = json!({ "answer_md": "Simple answer." });
        assert_eq!(extract_answer_from_raw(Some(&json!(serde_json::to_string(&raw).unwrap()))), "Simple answer.");
    }

    #[test]
    fn extract_answer_from_raw_rejects_placeholder() {
        let text = r#"{"answer_md": "<the rewritten answer in markdown>"}"#;
        // no extractable payload → clean identity pass-through (fence-stripped)
        assert_eq!(extract_answer_from_raw(Some(&json!(text))), text);
    }

    #[test]
    fn extract_answer_from_raw_pulls_last_fenced_json_from_reasoning_dump() {
        let text = "Let me think about this...\nSome reasoning here.\n```json\n{\"answer_md\": \"The real answer.\"}\n```";
        assert_eq!(extract_answer_from_raw(Some(&json!(text))), "The real answer.");
    }

    #[test]
    fn extract_answer_from_raw_clean_prose_passes_through_identity() {
        let text = "Just a normal markdown reply with **bold**.";
        assert_eq!(extract_answer_from_raw(Some(&json!(text))), text);
    }

    #[test]
    fn extract_answer_from_raw_strips_boundary_fences_when_no_payload() {
        let text = "```json\nSome plain text, not JSON.\n```";
        assert_eq!(extract_answer_from_raw(Some(&json!(text))), "Some plain text, not JSON.");
    }

    #[test]
    fn extract_answer_from_raw_null_is_empty() {
        assert_eq!(extract_answer_from_raw(None), "");
        assert_eq!(extract_answer_from_raw(Some(&Value::Null)), "");
    }
}
