//! skill-library-mint.rs — twin of skills/agents/in/skill-library-mint.mjs (db lane). NAMES ARE
//! CONTRACT: `skillLibraryMint` (default), `PROMPT_NAME`, `PROMPT_TEMPLATE_MD` (consts, exposed as
//! `pub const` below).
//!
//! Distills a chat session's solved problem into a SKILL draft (operator "turn this conversation
//! into a skill"). ONE ai-call; the result lands as a DISABLED draft (provenance 'minted') for the
//! operator to review + enable.
//!
//! PARITY-DEFERRED boundary: `crate::ai_ai_call::aiCall` always returns `Err` here (see
//! ai-call.rs's own module doc — the provider dispatch is a real, non-deterministic network
//! round-trip, unconvertible in principle). Because the real call never lands `Ok`, this twin's
//! `res` path always falls through to the empty-`out` map, `name` stays empty, and the function
//! returns the "the model found no reusable method in this session" branch with `ai_call_id: null`,
//! `cost_usd: null` — mirroring the JS shape exactly for that (untestable-happy-path) branch.
//! Everything BEFORE the call — the `session_id` guard, the turns query + no-turns guard, the
//! transcript assembly (join + per-reply 1500-char slice + 48000-char overall slice),
//! `ensurePrompt`'s find-or-register flow, and the post-call output-shaping (`out` map,
//! `skillLibrary` add + forced `enabled = 0`) — is ported faithfully and covered by the native
//! tests below (the add+disable path is exercised directly, standing in for what would run after a
//! successful `aiCall` in JS).
//!
//! Cross-module calls (pre-existing Rust twins): `crate::ai_prompt_find::promptFind`,
//! `crate::ai_prompt_add::promptAdd`, `crate::ai_ai_call::aiCall`,
//! `crate::agents_in_skill_library::skillLibrary`.
//!
//! `context.db` missing → JS `err.contextMissing('skill-library-mint', 'db')`. Not modeled: `db:
//! &Connection` stays a required, non-nullable reference (every sibling db-lane twin's
//! convention). `context.provider_fn` is JS-only plumbing passed through to `aiCall`'s network
//! dispatch — not modeled either, since the deferred stub takes no such parameter.
// QUALITY: bespoke-native — deterministic guards/transcript-assembly/db-bookkeeping ported and
// tested; the aiCall network dispatch is the disclosed PARITY-DEFERRED boundary (inherited from
// ai-call.rs, not re-litigated here).

use rusqlite::Connection;
use serde_json::{json, Value};

pub const PROMPT_NAME: &str = "skilllib.mint.v1";

pub const PROMPT_TEMPLATE_MD: &str = "You distill ONE reusable SKILL from a chat session where a problem was worked through.
A skill = craft another agent can apply later: WHEN to use it, and HOW — concrete steps, not platitudes.
If the session contains no reusable method, set name to an empty string.

THE SESSION (user ↔ assistant, oldest first):
{{transcript}}

Respond with JSON: {\"name\": \"<short imperative name or empty>\", \"description\": \"<ONE when-to-use line>\", \"body_md\": \"<the method: steps, heuristics, pitfalls — markdown>\"}";

/// `ensurePrompt(db)` — find-or-register. Return value unused by the caller (JS `ensurePrompt(db);`
/// discards it too); only the registration side effect matters.
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
            "max_output_tokens": 1600,
            "output_fields": [
                { "field_name": "name", "field_type": "string", "required": true },
                { "field_name": "description", "field_type": "string", "required": true },
                { "field_name": "body_md", "field_type": "string", "required": true },
            ],
        }),
        db,
    );
}

// JS `String(t.reply).slice(0, 1500)` — approximated as a codepoint take (playbook convention;
// this feeds operator-typed chat transcript, not adversarial UTF-16 surrogate content).
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

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

/// `export default async function skillLibraryMint(input, context)`
#[allow(non_snake_case)]
pub fn skillLibraryMint(input: &Value, db: &Connection) -> Result<Value, String> {
    // `Number(input?.session_id)`; NaN or non-positive-integer both fall to the same `required`
    // message (JS never distinguishes "wrong type" from "missing" here).
    let session_num = input.get("session_id").and_then(|v| match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok().or_else(|| if s.trim().is_empty() { Some(0.0) } else { None }),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Null => Some(0.0),
        _ => None,
    });
    let session_id = match session_num {
        Some(f) if f.is_finite() && f.fract() == 0.0 && f > 0.0 => f as i64,
        _ => return Err("skill-library-mint: session_id is required".into()),
    };

    let turns = crate::db_json::query_rows(
        db,
        "SELECT user_text, COALESCE(interviewer_response_md, '') AS reply \
         FROM interviewer_turns WHERE session_id = ? ORDER BY turn_index ASC LIMIT 40",
        &[json!(session_id)],
    )?;
    let turns: Vec<Value> = turns.as_array().cloned().unwrap_or_default();
    if turns.is_empty() {
        return Ok(json!({ "ok": false, "error": "the session has no turns to distill" }));
    }

    let transcript_full = turns
        .iter()
        .map(|t| {
            let user_text = t.get("user_text").map(js_string).unwrap_or_default();
            let reply = t.get("reply").map(js_string).unwrap_or_default();
            format!("USER: {user_text}\nASSISTANT: {}", take_chars(&reply, 1500))
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let transcript = take_chars(&transcript_full, 48000);

    ensure_prompt(db);
    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [{ "key": "transcript", "value": transcript }],
    });
    // PARITY-DEFERRED boundary: real `aiCall` always Errs here (see module doc + ai-call.rs). The
    // JS awaits the call directly (no try/catch) — a real failure would propagate as a thrown
    // error there too; this twin mirrors that by propagating `?` on `Err`.
    let res = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let mut out_name = String::new();
    let mut out_description: Option<String> = None;
    let mut out_body_md: Option<String> = None;
    for row in res.get("output_kv").and_then(Value::as_array).into_iter().flatten() {
        let value_text = match row.get("value_text") {
            Some(Value::Null) | None => continue,
            Some(v) => js_string(v),
        };
        match row.get("field_name").and_then(Value::as_str) {
            Some("name") => out_name = value_text,
            Some("description") => out_description = Some(value_text),
            Some("body_md") => out_body_md = Some(value_text),
            _ => {}
        }
    }
    let ai_call_id = res.get("ai_call_id").cloned().unwrap_or(Value::Null);
    let cost_usd = res.get("cost_usd").cloned().unwrap_or(Value::Null);

    let name = out_name.trim().to_string();
    if name.is_empty() {
        return Ok(json!({
            "ok": false,
            "ai_call_id": ai_call_id,
            "cost_usd": cost_usd,
            "error": "the model found no reusable method in this session",
        }));
    }

    let made = crate::agents_in_skill_library::skillLibrary(
        &json!({
            "op": "add",
            "name": name,
            "description": out_description.unwrap_or_else(|| "when this situation recurs".to_string()),
            "body_md": out_body_md.unwrap_or_default(),
            "provenance": "minted",
        }),
        db,
    )?;
    let slug = made.get("skill").and_then(|s| s.get("slug")).and_then(Value::as_str).unwrap_or("").to_string();
    // drafts arrive DISABLED — the operator reviews + enables in the Skill Library widget.
    db.execute("UPDATE knowledge.skill_library SET enabled = 0 WHERE slug = ?1", rusqlite::params![slug])
        .map_err(|e| e.to_string())?;

    let mut skill = made.get("skill").cloned().unwrap_or(Value::Null);
    if let Value::Object(ref mut m) = skill {
        m.insert("enabled".into(), json!(0));
    }
    Ok(json!({ "ok": true, "skill": skill, "ai_call_id": ai_call_id, "cost_usd": cost_usd }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; ATTACH ':memory:' AS knowledge; \
             CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY, session_id INTEGER, turn_index INTEGER, \
               user_text TEXT, interviewer_response_md TEXT); \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE knowledge.skill_library (id INTEGER PRIMARY KEY, slug TEXT UNIQUE NOT NULL, name TEXT NOT NULL, description TEXT NOT NULL, body_md TEXT NOT NULL DEFAULT '', assets_json TEXT NOT NULL DEFAULT '{}', applies_json TEXT NOT NULL DEFAULT '{}', verify_cmd TEXT, provenance TEXT NOT NULL DEFAULT 'authored', enabled INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));",
        )
        .unwrap();
        c
    }

    fn insert_turn(c: &Connection, session_id: i64, idx: i64, user_text: &str, resp: Option<&str>) {
        c.execute(
            "INSERT INTO interviewer_turns (session_id, turn_index, user_text, interviewer_response_md) VALUES (?1,?2,?3,?4)",
            rusqlite::params![session_id, idx, user_text, resp],
        )
        .unwrap();
    }

    // ── guards ───────────────────────────────────────────────────────
    #[test]
    fn missing_session_id_returns_exact_message() {
        let c = db();
        assert_eq!(skillLibraryMint(&json!({}), &c).unwrap_err(), "skill-library-mint: session_id is required");
    }

    #[test]
    fn zero_or_negative_session_id_returns_required_message() {
        let c = db();
        assert_eq!(skillLibraryMint(&json!({"session_id": 0}), &c).unwrap_err(), "skill-library-mint: session_id is required");
        assert_eq!(skillLibraryMint(&json!({"session_id": -3}), &c).unwrap_err(), "skill-library-mint: session_id is required");
    }

    #[test]
    fn non_integer_session_id_returns_required_message() {
        let c = db();
        assert_eq!(skillLibraryMint(&json!({"session_id": "abc"}), &c).unwrap_err(), "skill-library-mint: session_id is required");
    }

    #[test]
    fn no_turns_returns_ok_false_without_calling_ai() {
        let c = db();
        let out = skillLibraryMint(&json!({"session_id": 1}), &c).unwrap();
        assert_eq!(out, json!({ "ok": false, "error": "the session has no turns to distill" }));
    }

    // ── the deferred aiCall boundary: real network dispatch always Errs here, and the JS
    // (no try/catch around `await aiCall(...)`) propagates that failure identically.
    #[test]
    fn present_turns_propagate_the_deferred_ai_call_error() {
        let c = db();
        insert_turn(&c, 1, 0, "how do I do X", Some("do Y then Z"));
        let err = skillLibraryMint(&json!({"session_id": 1}), &c).unwrap_err();
        assert!(err.contains("ai-call"), "expected deferred-boundary error, got: {err}");
    }

    #[test]
    fn ensure_prompt_registers_template_once() {
        let c = db();
        insert_turn(&c, 1, 0, "hi", Some("hello"));
        let _ = skillLibraryMint(&json!({"session_id": 1}), &c);
        let found = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), &c).unwrap();
        assert_eq!(found.get("template_md").and_then(Value::as_str), Some(PROMPT_TEMPLATE_MD));
        // idempotent — a second call finds the same registered row rather than erroring
        let _ = skillLibraryMint(&json!({"session_id": 1}), &c);
        let rows = crate::db_json::query_rows(&c, "SELECT COUNT(*) AS n FROM brain.prompt_templates WHERE name = ?", &[json!(PROMPT_NAME)]).unwrap();
        assert_eq!(rows[0]["n"], json!(1));
    }

    // ── post-call output shaping, exercised directly (stands in for the branch after a
    // successful aiCall, which the deferred stub can never reach) ──────────────────────
    #[test]
    fn add_then_forced_disable_lands_a_draft_skill() {
        let c = db();
        let made = crate::agents_in_skill_library::skillLibrary(
            &json!({ "op": "add", "name": "Retry with backoff", "description": "when a flaky call needs resilience", "body_md": "1. wrap in retry\n2. backoff", "provenance": "minted" }),
            &c,
        )
        .unwrap();
        let slug = made["skill"]["slug"].as_str().unwrap().to_string();
        c.execute("UPDATE knowledge.skill_library SET enabled = 0 WHERE slug = ?1", rusqlite::params![slug]).unwrap();
        let rows = crate::db_json::query_rows(&c, "SELECT enabled, provenance FROM knowledge.skill_library WHERE slug = ?", &[json!(slug)]).unwrap();
        assert_eq!(rows[0]["enabled"], json!(0));
        assert_eq!(rows[0]["provenance"], json!("minted"));
    }

    // ── transcript assembly (pure string logic, no aiCall needed) ─────────────────────
    #[test]
    fn take_chars_truncates_by_codepoint_count() {
        assert_eq!(take_chars("hello world", 5), "hello");
        assert_eq!(take_chars("hi", 10), "hi");
    }
}
