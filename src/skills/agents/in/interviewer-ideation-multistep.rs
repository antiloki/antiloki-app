//! interviewer-ideation-multistep.rs — twin of skills/agents/in/interviewer-ideation-multistep.mjs.
//! NAMES ARE CONTRACT: `interviewerIdeationMultistep` (default). Multi-step (3-step) ideation
//! wizard: each step's question/options is AI-generated from the raw intent + prior answers;
//! step 3 additionally drafts a card title/body.
//!
//! PARITY-DEFERRED: the `await aiCall(...)` leg is the real LLM provider dispatch — genuinely
//! non-deterministic, not fixturable — same convention as every other bespoke-native twin
//! (see src/skills/ai/ai-call.rs's own doc). Ported here as `crate::ai_ai_call::aiCall(&input_value,
//! db)`, which is itself a deferred stub returning `Err(...)` (no live provider, no fixture, no
//! frontier entry). Everything else is ported FAITHFULLY:
//!   - guards: `db` required (JS `err.contextMissing`; unreachable here — `db: &Connection` is a
//!     required, non-nullable reference, same convention as every sibling twin in this folder),
//!     `raw_intent` required (`err.required`), `step` enum-checked against `[1, 2, 3]`
//!     (`err.enumVal`).
//!   - `ensurePrompt`: try `promptFind` (reused twin `crate::ai_prompt_find::promptFind`); on
//!     failure, register via `crate::ai_prompt_add::promptAdd` then re-`promptFind`. Ported as
//!     `ensure_prompt`.
//!   - prior-answers prompt block assembly (`priorBlock`): ported as pure helper `build_prior_block`.
//!   - response shaping (`aiResult.output_kv` → `out` map → final return shape): ported as pure
//!     helper `shape_output`, fixture-testable by feeding it a canned `ai_call` result `Value`
//!     directly (the field only exists post-dispatch, so this is the only way to exercise it
//!     without a live provider).
//!
//! `PROMPT_NAME`/`PROMPT_TEMPLATE_MD`/`PROMPT_OUTPUT_FIELDS` are ported as `pub const`/`pub fn`
//! data matching the JS named exports (module-level string/array literals — not census units,
//! but kept for parity + reuse by `ensure_prompt`).

use rusqlite::Connection;
use serde_json::{json, Value};

pub const PROMPT_NAME: &str = "interviewer.ideation.step.v1";

pub const PROMPT_TEMPLATE_MD: &str = "You are running an IDEATION wizard. The user has a fuzzy intent and we're narrowing it across 3 steps:
  step 1: WHAT — what kind of thing is being built
  step 2: WHO  — which agent/surface owns it
  step 3: WHY  — what does success look like (1 acceptance criterion)

CURRENT STEP: {{step}}
USER RAW INTENT: {{raw_intent}}
PRIOR ANSWERS (so far):
{{prior_answers}}

Generate the next step's prompt: one focused question + 3 short options + 1 hint for writein.
For step 3 (the LAST step), also produce a draft card title + 2-sentence body summarizing the
ideation chain into a card the user can file.

Output JSON of this exact shape:
{
  \"question_md\": \"<the question for this step>\",
  \"option_a\":    \"<short A label>\",
  \"option_b\":    \"<short B label>\",
  \"option_c\":    \"<short C label>\",
  \"writein_hint\":\"<what 'other' should describe>\",
  \"is_final\":    <true if step==3 AND we can draft a card>,
  \"draft_title\": \"<card title — only when is_final=true>\",
  \"draft_body\":  \"<2-sentence body — only when is_final=true>\"
}";

pub fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "question_md",  "field_type": "string",  "required": true },
        { "field_name": "option_a",     "field_type": "string",  "required": true },
        { "field_name": "option_b",     "field_type": "string",  "required": true },
        { "field_name": "option_c",     "field_type": "string",  "required": true },
        { "field_name": "writein_hint", "field_type": "string",  "required": false },
        { "field_name": "is_final",     "field_type": "boolean", "required": true },
        { "field_name": "draft_title",  "field_type": "string",  "required": false },
        { "field_name": "draft_body",   "field_type": "string",  "required": false },
    ])
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS `${v}` (feeds the enumVal `(got '…')` interpolation only).
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

// `ensurePrompt(db)` — try promptFind; on error, promptAdd then re-promptFind. Returns the row
// (unused by the caller, same as JS) or the propagated Err.
fn ensure_prompt(db: &Connection) -> Result<Value, String> {
    let find_input = json!({ "name": PROMPT_NAME });
    if let Ok(row) = crate::ai_prompt_find::promptFind(&find_input, db) {
        return Ok(row);
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "low",
            "default_temperature": "cold",
            "max_output_tokens": 600,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&find_input, db)
}

// `priorBlock` assembly: JS `prior_answers.length === 0 ? '  (this is the first step — no prior
// answers)' : prior_answers.map((a, i) => `  step ${i + 1}: "${a}"`).join('\n')`. Pure.
pub fn build_prior_block(prior_answers: &Value) -> String {
    let arr = prior_answers.as_array().cloned().unwrap_or_default();
    if arr.is_empty() {
        return "  (this is the first step — no prior answers)".to_string();
    }
    arr.iter()
        .enumerate()
        .map(|(i, a)| format!("  step {}: \"{}\"", i + 1, tmpl(Some(a))))
        .collect::<Vec<_>>()
        .join("\n")
}

// Response shaping: JS `for (const row of aiResult.output_kv ?? [])` builds `out` keyed by
// `field_name`, preferring `value_text` (non-null) else `value_boolean` (non-null), then shapes
// the final return object. Pure — feed it a canned `ai_call`-shaped result `Value` (fixture-
// testable independent of the deferred aiCall dispatch).
pub fn shape_output(step: &Value, ai_result: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(rows) = ai_result.get("output_kv").and_then(|v| v.as_array()) {
        for row in rows {
            let field_name = match row.get("field_name").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            if let Some(vt) = row.get("value_text") {
                if !vt.is_null() {
                    out.insert(field_name, vt.clone());
                    continue;
                }
            }
            if let Some(vb) = row.get("value_boolean") {
                if !vb.is_null() {
                    out.insert(field_name, json!(truthy(Some(vb))));
                }
            }
        }
    }
    let get_str = |k: &str| -> Value {
        match out.get(k) {
            Some(Value::Null) | None => json!(""),
            Some(v) => v.clone(),
        }
    };
    let get_opt = |k: &str| -> Value {
        match out.get(k) {
            Some(Value::Null) | None => Value::Null,
            Some(v) => v.clone(),
        }
    };
    json!({
        "step": step,
        "question_md": get_str("question_md"),
        "options": [
            { "key": "A", "label": get_str("option_a") },
            { "key": "B", "label": get_str("option_b") },
            { "key": "C", "label": get_str("option_c") },
        ],
        "writein_hint": get_opt("writein_hint"),
        "is_final": truthy(out.get("is_final")),
        "draft_title": get_opt("draft_title"),
        "draft_body": get_opt("draft_body"),
        "ai_call_id": ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null),
    })
}

pub fn interviewerIdeationMultistep(input: &Value, db: &Connection) -> Result<Value, String> {
    let raw_intent = input.get("raw_intent").cloned().unwrap_or(Value::Null);
    let step = input.get("step").cloned().unwrap_or(json!(1));
    let prior_answers = input.get("prior_answers").cloned().unwrap_or(json!([]));

    if !truthy(Some(&raw_intent)) {
        return Err("interviewer-ideation-multistep: raw_intent is required".into());
    }
    let step_ok = matches!(step.as_i64(), Some(1) | Some(2) | Some(3));
    if !step_ok {
        return Err(format!(
            "interviewer-ideation-multistep: step must be one of: 1, 2, 3 (got '{}')",
            tmpl(Some(&step))
        ));
    }

    ensure_prompt(db)?;

    let prior_block = build_prior_block(&prior_answers);

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "step", "value": tmpl(Some(&step)) },
            { "key": "raw_intent", "value": raw_intent },
            { "key": "prior_answers", "value": prior_block },
        ],
        "tier": "low",
        "temperature": "cold",
    });
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

    Ok(shape_output(&step, &ai_result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    // Minimal brain.prompt_templates/* schema (mirrors ai_prompt_find.rs's own `db0()`) — the
    // guard/deferred-boundary tests below only need `ensure_prompt` to succeed via promptAdd's
    // insert path (no pre-seeded row), so no seed() call is needed.
    const SCHEMA: &str = "
        CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
        CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
    ";

    // Mirrors ai_prompt_add.rs's own `db()` helper (same schema, same convention) — the
    // guard/deferred-boundary tests below only need `ensure_prompt` to succeed via promptAdd's
    // insert path (no pre-seeded row), so no seed data is needed.
    fn open_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    // ── guards ───────────────────────────────────────────────────────────
    #[test]
    fn raw_intent_required_guard() {
        let db = open_db();
        let e = interviewerIdeationMultistep(&json!({}), &db).unwrap_err();
        assert_eq!(e, "interviewer-ideation-multistep: raw_intent is required");
    }

    #[test]
    fn raw_intent_empty_string_is_required_guard() {
        let db = open_db();
        let e = interviewerIdeationMultistep(&json!({ "raw_intent": "" }), &db).unwrap_err();
        assert_eq!(e, "interviewer-ideation-multistep: raw_intent is required");
    }

    #[test]
    fn step_enum_guard_rejects_out_of_range() {
        let db = open_db();
        let e = interviewerIdeationMultistep(&json!({ "raw_intent": "x", "step": 4 }), &db).unwrap_err();
        assert_eq!(e, "interviewer-ideation-multistep: step must be one of: 1, 2, 3 (got '4')");
    }

    #[test]
    fn step_enum_guard_rejects_non_integer() {
        let db = open_db();
        let e = interviewerIdeationMultistep(&json!({ "raw_intent": "x", "step": "2" }), &db).unwrap_err();
        assert_eq!(e, "interviewer-ideation-multistep: step must be one of: 1, 2, 3 (got '2')");
    }

    // ── deferred boundary ───────────────────────────────────────────────
    #[test]
    fn defers_to_ai_call_and_propagates_its_error() {
        let db = open_db();
        let e = interviewerIdeationMultistep(&json!({ "raw_intent": "build a thing", "step": 1 }), &db).unwrap_err();
        assert!(e.contains("ai-call"), "expected deferred aiCall error, got: {e}");
    }

    // ── build_prior_block (pure) ─────────────────────────────────────────
    #[test]
    fn prior_block_empty_says_first_step() {
        assert_eq!(build_prior_block(&json!([])), "  (this is the first step — no prior answers)");
    }

    #[test]
    fn prior_block_lists_prior_answers_1_indexed() {
        let block = build_prior_block(&json!(["dashboard", "internal"]));
        assert_eq!(block, "  step 1: \"dashboard\"\n  step 2: \"internal\"");
    }

    // ── shape_output (pure) ───────────────────────────────────────────────
    #[test]
    fn shape_output_maps_text_and_boolean_kv_rows() {
        let ai_result = json!({
            "ai_call_id": 42,
            "output_kv": [
                { "field_name": "question_md", "value_text": "What kind?" },
                { "field_name": "option_a", "value_text": "Feature" },
                { "field_name": "option_b", "value_text": "Fix" },
                { "field_name": "option_c", "value_text": "Chore" },
                { "field_name": "is_final", "value_boolean": true },
            ],
        });
        let out = shape_output(&json!(1), &ai_result);
        assert_eq!(out["step"], json!(1));
        assert_eq!(out["question_md"], json!("What kind?"));
        assert_eq!(out["options"], json!([
            { "key": "A", "label": "Feature" },
            { "key": "B", "label": "Fix" },
            { "key": "C", "label": "Chore" },
        ]));
        assert_eq!(out["is_final"], json!(true));
        assert_eq!(out["writein_hint"], Value::Null);
        assert_eq!(out["draft_title"], Value::Null);
        assert_eq!(out["draft_body"], Value::Null);
        assert_eq!(out["ai_call_id"], json!(42));
    }

    #[test]
    fn shape_output_defaults_missing_output_kv_to_empty_strings() {
        let out = shape_output(&json!(3), &json!({}));
        assert_eq!(out["question_md"], json!(""));
        assert_eq!(out["options"], json!([
            { "key": "A", "label": "" },
            { "key": "B", "label": "" },
            { "key": "C", "label": "" },
        ]));
        assert_eq!(out["is_final"], json!(false));
        assert_eq!(out["ai_call_id"], Value::Null);
    }

    #[test]
    fn shape_output_final_step_carries_draft_fields() {
        let ai_result = json!({
            "output_kv": [
                { "field_name": "is_final", "value_boolean": true },
                { "field_name": "draft_title", "value_text": "Ship the wizard" },
                { "field_name": "draft_body", "value_text": "A 2-sentence summary." },
            ],
        });
        let out = shape_output(&json!(3), &ai_result);
        assert_eq!(out["is_final"], json!(true));
        assert_eq!(out["draft_title"], json!("Ship the wizard"));
        assert_eq!(out["draft_body"], json!("A 2-sentence summary."));
    }
}
