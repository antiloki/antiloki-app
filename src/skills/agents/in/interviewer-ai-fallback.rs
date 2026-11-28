//! interviewer-ai-fallback.rs — twin of skills/agents/in/interviewer-ai-fallback.mjs (db READ+WRITE,
//! brain-attached; LLM-PARTIAL). NAMES ARE CONTRACT: `interviewerAiFallback` (default),
//! `PROMPT_NAME`, `PROMPT_TEMPLATE_MD`, `ensurePrompt`.
//!
//! PARITY-DEFERRED: the `aiCall(...)` leg is a real network call to a BYOK LLM provider — genuinely
//! non-deterministic, unfixturable in principle (see `crate::ai_ai_call::aiCall`'s own doc). This
//! twin calls that deferred stub, which always returns `Err(..)`, so the real happy path (an actual
//! model response reaching `collapse_output_kv`) cannot be exercised by a native test — same
//! limitation as every other LLM-PARTIAL twin. Everything else is ported faithfully: the
//! `user_text` guard, prompt upsert idempotency (`ensurePrompt`: find-or-create via
//! `crate::ai_prompt_find`/`crate::ai_prompt_add`), and the `output_kv` → flat-object collapse +
//! `suggested_args_json` parse + default-fallback response shaping (`collapse_output_kv`, a pure
//! helper factored out of the JS's inline loop specifically so it's independently testable against
//! a canned AI response).
//!
//! `provider_fn` (the JS test-injection hook for a mock provider) has no Rust equivalent — it only
//! ever reaches the deferred `aiCall` body, so it is dropped from this twin's signature the same way
//! `db`/`opts` were dropped from `validateAndDecompose` in ai-call.rs (dead on every real contract
//! this twin can express).
// QUALITY: mixed unit — guard + prompt-upsert + response-shaping ported/fixture-tested; aiCall leg
// PARITY-DEFERRED (network, unfixturable)

use rusqlite::Connection;
use serde_json::{json, Map, Value};

pub const PROMPT_NAME: &str = "interviewer.fallback.sdr.v1";

pub const PROMPT_TEMPLATE_MD: &str = "You are the Interviewer for antiloki-v3, a deterministic AI orchestration substrate.\n\nYou sit between the user and 155 substrate actions across 9 agents\n(board-keeper, pl, tl, dev, qa, man, in, system, god). Every user\nrequest must route through you before any action dispatches.\n\nThe deterministic classifier did NOT recognize the user's intent. Your\njob — in this exact order:\n\n  1. SCREEN — Infer what the user is actually trying to accomplish\n     (the underlying need, not the literal words). Greetings, vague\n     questions, off-topic small talk are all routable to \"what would\n     you like to do?\"\n\n  2. EDUCATE — Briefly tell them what the substrate can do that\n     matches. Reference real capabilities by name: list cards, add\n     a memory, tick the driver, render docs, run a sandbox, etc.\n\n  3. DIRECT — Offer ONE concrete next step:\n     - A canonical action name (one of the 156, e.g.,\n       \"board-keeper.card.list\", \"system.driver.tick\",\n       \"keeper.memory.add\", \"system.docs.getting_started\")\n     - OR a focused clarifying question (just one — don't pile up)\n\nVoice rules:\n  - Warm but brief — 2–4 sentences total\n  - No emoji, no headers, no enthusiasm theater\n  - Speak like a sharp SDR qualifying a lead — not a chatbot pretending\n    to be a friend\n  - If the user is just saying hi, give them a friendly nudge into\n    something useful — don't get stuck mirroring small talk\n\nYou CANNOT execute actions yourself. Suggest the action name; the\nsubstrate's gate will run it after the user confirms.\n\nAvailable action namespaces (a sample):\n  board-keeper.card.*   pl.session.*   tl.sandbox.*\n  contract.*            dev.*          qa.*\n  system.driver.*       system.docs.*  system.memory.*  keeper.*\n  system.action.find    system.skill.find    system.anatomy.html\n\nOutput JSON of this exact shape:\n{\n  \"kind\": \"read\" | \"mutate\" | \"meta\" | \"clarify\",\n  \"response_md\": \"<2-4 sentences shown to the user>\",\n  \"suggested_action_name\": \"<canonical action name>\" | null,\n  \"suggested_args\": {<minimal args for that action>}\n}\n\nUser input: {{user_text}}";

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "kind", "field_type": "enum", "enum_values": ["read","mutate","meta","clarify"], "required": true },
        { "field_name": "response_md", "field_type": "string", "required": true },
        { "field_name": "suggested_action_name", "field_type": "string", "required": false },
        { "field_name": "suggested_args_json", "field_type": "string", "required": false },
    ])
}

/// `function ensurePrompt(db)` — find-or-create idempotency: `promptFind` first, and only on
/// failure (name not yet seeded) does `promptAdd` run, followed by a re-find. Mirrors the JS
/// try/catch exactly — a `promptAdd` failure (e.g. a race where another caller seeded it between
/// the find and the add) propagates as this function's own error, same as JS letting the throw
/// bubble.
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
            "max_output_tokens": 800,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&find_input, db)
}

/// `output_kv` → flat-object collapse (mirrors the JS inline `for (const row of aiResult?.output_kv
/// ?? [])` loop) followed by the `suggested_args_json` parse and the default-fallback response
/// shape. Factored out as its own pure fn so the deterministic response-shaping surface is
/// independently testable against a canned AI response, since the real `aiCall` leg (the only thing
/// that could ever produce a genuine `output_kv`) is PARITY-DEFERRED.
pub fn collapse_output_kv(ai_result: &Value) -> Value {
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

    let mut suggested_args = json!({});
    if let Some(raw) = out.get("suggested_args_json").and_then(Value::as_str) {
        if !raw.is_empty() {
            if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
                suggested_args = parsed;
            }
        }
    }

    let kind = out.get("kind").cloned().unwrap_or(json!("clarify"));
    let response_md = out
        .get("response_md")
        .cloned()
        .unwrap_or(json!("I did not understand. Could you be more specific?"));
    let suggested_action_name = out.get("suggested_action_name").cloned().unwrap_or(Value::Null);
    let ai_call_id = ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null);

    json!({
        "kind": kind,
        "response_md": response_md,
        "suggested_action_name": suggested_action_name,
        "suggested_args": suggested_args,
        "ai_call_id": ai_call_id,
        "prompt_name": PROMPT_NAME,
    })
}

/// `export default async function interviewerAiFallback(input, context)`. `context.db` →
/// `db: &Connection`; `context.provider_fn` has no Rust equivalent (see module doc) and is dropped.
pub fn interviewerAiFallback(input: &Value, db: &Connection) -> Result<Value, String> {
    let user_text = input.get("user_text").cloned().unwrap_or(Value::Null);
    let has_text = match &user_text {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        _ => true,
    };
    if !has_text {
        return Err("interviewer-ai-fallback: user_text is required".to_string());
    }

    ensurePrompt(db)?;

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [{ "key": "user_text", "value": user_text }],
    });
    // PARITY-DEFERRED boundary: crate::ai_ai_call::aiCall is a network stub that always returns
    // Err — see module doc. On a real provider response this would feed collapse_output_kv.
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;
    Ok(collapse_output_kv(&ai_result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env_lock::ENV_LOCK;

    // Schema copied verbatim from prompt-add.rs's/prompt-find.rs's own test fixtures — both twins
    // this module composes are exercised against exactly this shape.
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

    #[test]
    fn missing_user_text_errors() {
        let _g = ENV_LOCK.lock().unwrap();
        let db = open_db();
        let e = interviewerAiFallback(&json!({}), &db).unwrap_err();
        assert!(e.contains("user_text is required"));
        let e2 = interviewerAiFallback(&json!({ "user_text": "" }), &db).unwrap_err();
        assert!(e2.contains("user_text is required"));
    }

    #[test]
    fn ensure_prompt_creates_then_finds_idempotently() {
        let _g = ENV_LOCK.lock().unwrap();
        let db = open_db();
        let first = ensurePrompt(&db).unwrap();
        assert_eq!(first["name"], json!(PROMPT_NAME));
        let second = ensurePrompt(&db).unwrap();
        assert_eq!(second["id"], first["id"], "second call finds, does not re-add");
    }

    #[test]
    fn deferred_ai_call_leg_propagates_the_stub_error() {
        let _g = ENV_LOCK.lock().unwrap();
        let db = open_db();
        let e = interviewerAiFallback(&json!({ "user_text": "hi" }), &db).unwrap_err();
        assert!(e.contains("ai-call"), "propagates ai-call.rs's deferred-stub error: {e}");
    }

    // ── collapse_output_kv (pure response-shaping surface) ──────────────────────────────────
    #[test]
    fn collapse_flattens_output_kv_rows_by_value_column() {
        let ai_result = json!({
            "ai_call_id": "call-1",
            "output_kv": [
                { "field_name": "kind", "value_text": "read" },
                { "field_name": "response_md", "value_text": "Try board-keeper.card.list." },
                { "field_name": "suggested_action_name", "value_text": "board-keeper.card.list" },
                { "field_name": "suggested_args_json", "value_text": "{\"limit\":5}" },
            ]
        });
        let out = collapse_output_kv(&ai_result);
        assert_eq!(out["kind"], json!("read"));
        assert_eq!(out["response_md"], json!("Try board-keeper.card.list."));
        assert_eq!(out["suggested_action_name"], json!("board-keeper.card.list"));
        assert_eq!(out["suggested_args"], json!({"limit":5}));
        assert_eq!(out["ai_call_id"], json!("call-1"));
        assert_eq!(out["prompt_name"], json!(PROMPT_NAME));
    }

    #[test]
    fn collapse_defaults_when_output_kv_missing_or_empty() {
        let out = collapse_output_kv(&json!({}));
        assert_eq!(out["kind"], json!("clarify"));
        assert_eq!(out["response_md"], json!("I did not understand. Could you be more specific?"));
        assert_eq!(out["suggested_action_name"], Value::Null);
        assert_eq!(out["suggested_args"], json!({}));
        assert_eq!(out["ai_call_id"], Value::Null);

        let out2 = collapse_output_kv(&json!({ "output_kv": [] }));
        assert_eq!(out2["kind"], json!("clarify"));
    }

    #[test]
    fn collapse_invalid_suggested_args_json_keeps_empty_object() {
        let ai_result = json!({
            "output_kv": [
                { "field_name": "kind", "value_text": "clarify" },
                { "field_name": "suggested_args_json", "value_text": "{not json" },
            ]
        });
        let out = collapse_output_kv(&ai_result);
        assert_eq!(out["suggested_args"], json!({}));
    }

    #[test]
    fn collapse_handles_number_and_boolean_value_columns() {
        let ai_result = json!({
            "output_kv": [
                { "field_name": "confidence", "value_number": 0.42 },
                { "field_name": "flagged", "value_boolean": 1 },
            ]
        });
        let out = collapse_output_kv(&ai_result);
        assert_eq!(out["kind"], json!("clarify")); // not present in output_kv → default fallback
        // confidence/flagged aren't part of the response shape, but the loop still ran without error
        let _ = out;
    }
}
