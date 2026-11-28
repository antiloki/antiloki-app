//! interviewer-present.rs — twin of skills/agents/in/interviewer-present.mjs (db READ+WRITE,
//! brain-attached; LLM-PARTIAL). Phase 2 of the two-phase interviewer: takes phase-1's raw
//! dispatch_results + the original user_text/intent and produces a JARVIS-style human-facing
//! reply. NAMES ARE CONTRACT: `interviewerPresent` (default), `PROMPT_NAME`, `PROMPT_TEMPLATE_MD`,
//! `PROMPT_OUTPUT_FIELDS`, `ensurePrompt`, `clipResults`, `clipValue`.
//!
//! PARITY-DEFERRED: the `aiCall(...)` leg is a real network call to a BYOK LLM provider —
//! genuinely non-deterministic, unfixturable in principle (see `crate::ai_ai_call::aiCall`'s own
//! doc). This twin calls that deferred stub, which always returns `Err(..)`, so the real happy
//! path (an actual model response reaching the output-shaping code below) cannot be exercised by
//! a native test — same limitation as every other LLM-PARTIAL twin (interviewer-ai-fallback.rs).
//! Everything else is ported faithfully: the `user_text` guard, the stub-mode short-circuit
//! (`ANTILOKI_INTERVIEWER_PRESENT_STUB=1`), prompt upsert idempotency (`ensurePrompt`: drift-aware
//! find-or-recreate via `crate::ai_prompt_find`/`crate::ai_prompt_add`), the dispatch_results
//! clipping (`clipResults`/`clipValue`), and the `output_kv` → response shaping (factored into
//! `collapse_output_kv`/`parse_view_data`/`build_ai_telemetry`, pure helpers independently testable
//! against a canned AI response, mirroring interviewer-ai-fallback.rs's `collapse_output_kv`).
//!
//! `provider_fn` (the JS test-injection hook for a mock provider) has no Rust equivalent — it only
//! ever reaches the deferred `aiCall` body, so it is dropped from this twin's signature the same
//! way it was dropped from interviewer-ai-fallback.rs.
// QUALITY: mixed unit — guard + stub-mode + prompt-upsert + clip + response-shaping ported/fixture-
// tested; aiCall leg PARITY-DEFERRED (network, unfixturable)

use rusqlite::Connection;
use serde_json::{json, Map, Value};

const PROMPT_NAME: &str = "interviewer.present.v2";

const PROMPT_TEMPLATE_MD: &str = "You are the PRESENTER stage of a two-phase interviewer. The TRIAGE stage already classified the user's intent and dispatched the appropriate substrate actions. You are given:\n  - The user's original message\n  - The triage's intent_summary + classification kind\n  - The raw dispatch results (what the substrate actually returned)\n\nYour ONLY job: produce a JARVIS-grade human-facing reply. Think senior\nengineer explaining clearly — calm, COMPLETE, well-structured. Be thorough\nfor substantive questions (operator feedback: answers were too short and flat);\nreserve terseness for trivial lookups. Never narrate retrieval (\"Pulling the\ncards…\"). Never expose raw arrays or JSON in your prose — that goes in reasoning.\n\nOUTPUT FIELDS:\n  presentation_md  — the answer the user reads, as RICH, well-structured Markdown.\n                     Be COMPLETE, not terse: for any substantive question give a\n                     thorough answer the reader needn't ask twice. Structure it:\n                       • lead with a one-line **takeaway**, then the detail\n                       • ## / ### headings to organize a multi-part answer\n                       • **bold** key terms; `inline code` for identifiers / files /\n                         actions / flags\n                       • fenced code blocks WITH a language tag (```ts, ```js, ```py …)\n                         whenever you show code — it is syntax-highlighted\n                       • bullet / numbered lists for steps or sets; a table for comparisons\n                       • [link](path) any file or source you reference\n                     Only trivial lookups (\"yes, that action exists\") stay one sentence —\n                     match depth to the question (\"how does X work\" deserves real prose).\n  view_intent      — closed enum, see below.\n  view_data_json   — optional JSON-stringified override for the view.\n                     Empty string when the dispatch_results already\n                     carry the right shape.\n  notes_to_self    — one short line: what you summarized, edge cases\n                     you handled, why you chose the view_intent. Shown\n                     in the reasoning block only.\n\nVIEW INTENT — pick ONE:\n  'inline'      — prose-only answer. The dispatch_results bubble below\n                  will still render if useful, but the prose IS the\n                  framing. Use for: question answers, summaries, status\n                  checks, explanations.\n  'board'       — user asked to SEE the board (list/show cards). The\n                  FE will render a kanban from dispatch_results.\n                  Keep presentation_md to ONE short framing line\n                  (\"Doing-impl heavy; backlog is the long tail.\").\n  'card_detail' — user asked about ONE card. view_data_json={\"slug\":\"X\"}.\n                  FE deep-dives that card.\n  'table'       — rows + columns of structured data not card-shaped.\n                  view_data_json={\"columns\":[\"a\",\"b\"],\"rows\":[[..],..]}\n  'code'        — user EXPLICITLY asked to see raw/code/JSON. Don't\n                  filter; let it through with syntax highlighting.\n  'none'        — presentation_md is the whole answer; suppress the\n                  raw dispatch bubble. Use when the data was just a\n                  lookup (\"yes, the action exists at board-keeper.\n                  card.list\").\n\nCONTEXT:\n  user_text       : {{user_text}}\n  intent_summary  : {{intent_summary}}\n  classification  : {{classification_kind}}\n  dispatch_results: {{dispatch_results_json}}\n\nSESSION INTELLIGENCE — widgets & artefacts this session generated (always available to you). The user may reference one with #name. Ground your reply in their data when relevant, and never claim you can't see one:\n{{session_intelligence}}\n\nREGENERATION NONCE (cache buster): {{nonce}}\n\nRespond with JSON of this exact shape:\n{\n  \"presentation_md\": \"<the answer>\",\n  \"view_intent\":     \"<one of: inline|board|card_detail|table|code|none>\",\n  \"view_data_json\":  \"<JSON string OR empty string>\",\n  \"notes_to_self\":   \"<one short line of presenter reasoning>\"\n}";

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "presentation_md", "field_type": "string", "required": true },
        { "field_name": "view_intent",     "field_type": "enum", "enum_values": ["inline","board","card_detail","table","code","none"], "required": true },
        { "field_name": "view_data_json",  "field_type": "string", "required": false },
        { "field_name": "notes_to_self",   "field_type": "string", "required": false },
    ])
}

/// `function ensurePrompt(db)` — drift-aware (find-or-only would pin the OLD template forever):
/// re-registers when the stored template_md differs from PROMPT_TEMPLATE_MD, so richer-response
/// edits actually take effect. Mirrors the JS try/catch: a `promptFind` error is swallowed
/// (treated as "not found yet").
pub fn ensurePrompt(db: &Connection) -> Result<Value, String> {
    let find_input = json!({ "name": PROMPT_NAME });
    let existing = crate::ai_prompt_find::promptFind(&find_input, db).ok();
    if let Some(found) = &existing {
        if found.get("template_md").and_then(Value::as_str) == Some(PROMPT_TEMPLATE_MD) {
            return Ok(found.clone());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "mid",
            "default_temperature": "cold",
            "max_output_tokens": 3000,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&find_input, db)
}

/// `function clipValue(v, maxChars)` — pure. null/undefined pass through; a string is char-sliced;
/// otherwise JSON.stringify + slice, with the JS's inner try/catch fallback (`JSON.parse(sliced +
/// '"…')?.toString?.() ?? sliced`) collapsed to the always-taken `String(v).slice(...)` branch,
/// since `JSON.parse(sliced + '"…')` never yields valid JSON for a truncated non-string value in
/// practice (the appended `"…` only closes a string literal) — the outer catch's plain slice is
/// what actually returns. `v` here is a Value, so `String(v)` is rendered as `js_string`.
pub fn clipValue(v: &Value, max_chars: i64) -> Value {
    if v.is_null() { return v.clone(); }
    let max = max_chars.max(0) as usize;
    if let Value::String(s) = v {
        let len = s.chars().count();
        if len > max {
            let take = max.saturating_sub(1);
            return json!(format!("{}…", s.chars().take(take).collect::<String>()));
        }
        return v.clone();
    }
    let stringified = serde_json::to_string(v).unwrap_or_default();
    if stringified.chars().count() <= max { return v.clone(); }
    let take = max.saturating_sub(1);
    json!(format!("{}…", js_string(v).chars().take(take).collect::<String>()))
}

// = JS String(v) for the clipValue fallback path.
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

/// `function clipResults(dispatch_results, budgetChars = 8000)` — pure. Empty/absent array →
/// `"[]"`. Otherwise projects `{action, ok, error, result}` per entry (error omitted when
/// undefined/null, matching JSON.stringify dropping it), clipping `result` to an even share of the
/// budget, then pretty-prints with 2-space indent (JS `JSON.stringify(clipped, null, 2)`).
pub fn clipResults(dispatch_results: &Value, budget_chars: i64) -> Value {
    let arr = dispatch_results.as_array().cloned().unwrap_or_default();
    if arr.is_empty() { return json!("[]"); }
    let per_item = (budget_chars as f64 / arr.len() as f64).floor() as i64;
    let clipped: Vec<Value> = arr.iter().map(|dr| {
        let mut o = Map::new();
        o.insert("action".into(), dr.get("action").cloned().unwrap_or(Value::Null));
        o.insert("ok".into(), dr.get("ok").cloned().unwrap_or(Value::Null));
        // `error: dr.error ?? undefined` — an absent/null error key is OMITTED by JSON.stringify.
        match dr.get("error") {
            Some(e) if !e.is_null() => { o.insert("error".into(), e.clone()); }
            _ => {}
        }
        let result = dr.get("result").cloned().unwrap_or(Value::Null);
        o.insert("result".into(), clipValue(&result, per_item));
        Value::Object(o)
    }).collect();
    json!(serde_json::to_string_pretty(&Value::Array(clipped)).unwrap_or_default())
}

/// `output_kv` → flat-object collapse (mirrors the JS inline `for (const row of aiResult?.output_kv
/// ?? [])` loop). Pure — independently testable against a canned AI response, since the real
/// `aiCall` leg is PARITY-DEFERRED.
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
    Value::Object(out)
}

/// `typeof out.view_data_json === 'string' && out.view_data_json.trim() ? JSON.parse(...) : null`
/// with a try/catch leaving `view_data` null on parse failure. Pure.
pub fn parse_view_data(out: &Value) -> Value {
    match out.get("view_data_json").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => serde_json::from_str(s).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// Final `ai_telemetry` shape, matching the JS object literal's defaults (`?? null` / `?? 'mid'` /
/// `?? 'cold'` / `?? 0` / `?? false`). Pure — `tokens_in`/`tokens_out` are the caller's post-hoc
/// `brain.ai_calls` read result (may be absent, matching the JS try/catch leaving them `null`).
pub fn build_ai_telemetry(ai_result: &Value, tokens_in: Option<&Value>, tokens_out: Option<&Value>) -> Value {
    json!({
        "phase": "presenter",
        "prompt_name": PROMPT_NAME,
        "model_id": ai_result.get("model_id").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null),
        "provider": ai_result.get("provider").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null),
        "tier": ai_result.get("tier_used").filter(|v| !v.is_null()).cloned().unwrap_or(json!("mid")),
        "temperature": ai_result.get("temperature_used").filter(|v| !v.is_null()).cloned().unwrap_or(json!("cold")),
        "tokens_in": tokens_in.filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null),
        "tokens_out": tokens_out.filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null),
        "cost_usd": ai_result.get("cost_usd").filter(|v| !v.is_null()).cloned().unwrap_or(json!(0)),
        "cached": ai_result.get("cached").filter(|v| !v.is_null()).cloned().unwrap_or(json!(false)),
        "duration_ms": ai_result.get("duration_ms").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null),
    })
}

/// Final response assembly (the function's return statement), given the collapsed `output_kv` map
/// and the resolved `ai_telemetry`. Pure.
pub fn build_present_output(out: &Value, ai_result: &Value, tokens_in: Option<&Value>, tokens_out: Option<&Value>) -> Value {
    json!({
        "presentation_md": out.get("presentation_md").filter(|v| !v.is_null()).cloned().unwrap_or(json!("")),
        "view_intent": out.get("view_intent").filter(|v| !v.is_null()).cloned().unwrap_or(json!("inline")),
        "view_data": parse_view_data(out),
        "notes_to_self": out.get("notes_to_self").filter(|v| !v.is_null()).cloned().unwrap_or(json!("")),
        "ai_call_id": ai_result.get("ai_call_id").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null),
        "ai_telemetry": build_ai_telemetry(ai_result, tokens_in, tokens_out),
    })
}

/// stub-mode response (`ANTILOKI_INTERVIEWER_PRESENT_STUB=1`) — deterministic canned presentation,
/// no AI call. `hasCardArray` mirrors the JS `dispatch_results.some(d => d.ok &&
/// Array.isArray(d.result) && d.result.length && 'slug' in d.result[0])`.
fn stub_response(dispatch_results: &Value) -> Value {
    let has_card_array = dispatch_results.as_array().map_or(false, |arr| {
        arr.iter().any(|d| {
            let ok = d.get("ok").map_or(false, |v| v.as_bool().unwrap_or(false));
            let result_arr = d.get("result").and_then(Value::as_array);
            ok && result_arr.map_or(false, |r| !r.is_empty() && r[0].get("slug").is_some())
        })
    });
    json!({
        "presentation_md": if has_card_array { "Stub presentation — board grouped below." } else { "Stub presentation." },
        "view_intent": if has_card_array { "board" } else { "inline" },
        "view_data": Value::Null,
        "notes_to_self": "stub mode active",
        "ai_call_id": Value::Null,
        "ai_telemetry": {
            "phase": "presenter", "prompt_name": PROMPT_NAME,
            "model_id": "stub", "provider": "stub",
            "tier": "low", "temperature": "cold",
            "tokens_in": 0, "tokens_out": 0,
            "cost_usd": 0, "cached": false, "duration_ms": 0,
        },
    })
}

/// `export default async function interviewerPresent(input, context)`. `context.db` →
/// `db: &Connection`; `context.provider_fn` has no Rust equivalent (see module doc) and is dropped.
pub fn interviewerPresent(input: &Value, db: &Connection) -> Result<Value, String> {
    let user_text = input.get("user_text").cloned().unwrap_or(Value::Null);
    let has_text = match &user_text {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        _ => true,
    };
    if !has_text {
        return Err("interviewer-present: user_text is required".to_string());
    }
    let dispatch_results = input.get("dispatch_results").cloned().unwrap_or(Value::Null);
    let intent_summary = input.get("intent_summary").cloned().unwrap_or(json!(""));
    let classification_kind = input.get("classification_kind").cloned().unwrap_or(json!("read"));
    let nonce = input.get("nonce").cloned().unwrap_or(json!(""));
    let session_id = input.get("session_id").cloned().unwrap_or(Value::Null);

    if std::env::var("ANTILOKI_INTERVIEWER_PRESENT_STUB").as_deref() == Ok("1") {
        return Ok(stub_response(&dispatch_results));
    }

    ensurePrompt(db)?;

    let dispatch_results_json = clipResults(&dispatch_results, 8000);
    let session_intelligence = crate::agents_in__helpers_session_intelligence::sessionIntelligenceDigest(db, &session_id, &json!({}));

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "user_text", "value": user_text },
            { "key": "intent_summary", "value": intent_summary },
            { "key": "classification_kind", "value": classification_kind },
            { "key": "dispatch_results_json", "value": dispatch_results_json },
            { "key": "session_intelligence", "value": session_intelligence },
            { "key": "nonce", "value": js_string(&nonce) },
        ],
    });
    // PARITY-DEFERRED boundary: crate::ai_ai_call::aiCall is a network stub that always returns
    // Err — see module doc. On a real provider response this would feed build_present_output.
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let out = collapse_output_kv(&ai_result);

    let mut tokens_in: Option<Value> = None;
    let mut tokens_out: Option<Value> = None;
    if let Some(ai_call_id) = ai_result.get("ai_call_id").filter(|v| !v.is_null()) {
        if let Ok(rows) = crate::db_json::query_rows(
            db,
            "SELECT tokens_in, tokens_out FROM brain.ai_calls WHERE id = ?",
            &[ai_call_id.clone()],
        ) {
            if let Some(row) = rows.as_array().and_then(|a| a.first()) {
                tokens_in = row.get("tokens_in").cloned();
                tokens_out = row.get("tokens_out").cloned();
            }
        }
    }

    Ok(build_present_output(&out, &ai_result, tokens_in.as_ref(), tokens_out.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env_lock::ENV_LOCK;

    fn open_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("ATTACH DATABASE ':memory:' AS brain").unwrap();
        db.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE interviewer_session_widgets (id INTEGER, session_id INTEGER, type TEXT, kind TEXT, payload_json TEXT);
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, tokens_in INTEGER, tokens_out INTEGER);",
        )
        .unwrap();
        db
    }

    #[test]
    fn missing_user_text_errors() {
        let _g = ENV_LOCK.lock().unwrap();
        let db = open_db();
        let e = interviewerPresent(&json!({}), &db).unwrap_err();
        assert!(e.contains("user_text is required"));
        let e2 = interviewerPresent(&json!({ "user_text": "" }), &db).unwrap_err();
        assert!(e2.contains("user_text is required"));
    }

    #[test]
    fn stub_mode_inline_and_board() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("ANTILOKI_INTERVIEWER_PRESENT_STUB", "1");
        let db = open_db();
        let r = interviewerPresent(&json!({ "user_text": "hi" }), &db).unwrap();
        assert_eq!(r["view_intent"], json!("inline"));
        assert_eq!(r["presentation_md"], json!("Stub presentation."));

        let r2 = interviewerPresent(&json!({
            "user_text": "hi",
            "dispatch_results": [{ "ok": true, "result": [{ "slug": "CARD-1" }] }]
        }), &db).unwrap();
        assert_eq!(r2["view_intent"], json!("board"));
        assert_eq!(r2["presentation_md"], json!("Stub presentation — board grouped below."));
        std::env::remove_var("ANTILOKI_INTERVIEWER_PRESENT_STUB");
    }

    #[test]
    fn ensure_prompt_creates_then_finds_idempotently_and_redrifts() {
        let _g = ENV_LOCK.lock().unwrap();
        let db = open_db();
        let first = ensurePrompt(&db).unwrap();
        assert_eq!(first["name"], json!(PROMPT_NAME));
        assert_eq!(first["version"], json!(1));
        let second = ensurePrompt(&db).unwrap();
        assert_eq!(second["id"], first["id"], "same template_md → finds, does not re-add");

        // simulate drift: mutate the stored template_md, ensurePrompt must re-register.
        db.execute("UPDATE brain.prompt_templates SET template_md = 'stale' WHERE id = ?", [first["id"].as_i64().unwrap()]).unwrap();
        let third = ensurePrompt(&db).unwrap();
        assert_eq!(third["version"], json!(2), "drift triggers a re-add bumping the version");
    }

    #[test]
    fn deferred_ai_call_leg_propagates_the_stub_error() {
        let _g = ENV_LOCK.lock().unwrap();
        let db = open_db();
        let e = interviewerPresent(&json!({ "user_text": "hi" }), &db).unwrap_err();
        assert!(e.contains("ai-call"), "propagates ai-call.rs's deferred-stub error: {e}");
    }

    // ── clipValue / clipResults ─────────────────────────────────────────
    #[test]
    fn clip_value_passes_short_strings_and_truncates_long_ones() {
        assert_eq!(clipValue(&json!("short"), 100), json!("short"));
        assert_eq!(clipValue(&Value::Null, 100), Value::Null);
        let long = "a".repeat(20);
        let clipped = clipValue(&json!(long), 10);
        assert_eq!(clipped.as_str().unwrap().chars().count(), 10);
        assert!(clipped.as_str().unwrap().ends_with('…'));
    }

    #[test]
    fn clip_value_non_string_passthrough_and_truncation() {
        assert_eq!(clipValue(&json!({"a":1}), 100), json!({"a":1}));
        let big = json!({ "x": "a".repeat(50) });
        let clipped = clipValue(&big, 10);
        assert!(clipped.as_str().unwrap().ends_with('…'));
    }

    #[test]
    fn clip_results_empty_and_omits_null_error() {
        assert_eq!(clipResults(&Value::Null, 8000), json!("[]"));
        assert_eq!(clipResults(&json!([]), 8000), json!("[]"));

        let r = clipResults(&json!([{ "action": "a", "ok": true, "result": {"n":1} }]), 8000);
        let parsed: Value = serde_json::from_str(r.as_str().unwrap()).unwrap();
        assert!(parsed[0].get("error").is_none(), "null/absent error key omitted");
        assert_eq!(parsed[0]["action"], json!("a"));
    }

    #[test]
    fn clip_results_keeps_present_error() {
        let r = clipResults(&json!([{ "action": "a", "ok": false, "error": "boom", "result": null }]), 8000);
        let parsed: Value = serde_json::from_str(r.as_str().unwrap()).unwrap();
        assert_eq!(parsed[0]["error"], json!("boom"));
    }

    // ── collapse_output_kv / parse_view_data / build_present_output ─────
    #[test]
    fn collapse_flattens_output_kv_rows_by_value_column() {
        let ai_result = json!({
            "ai_call_id": "call-1",
            "output_kv": [
                { "field_name": "presentation_md", "value_text": "**hi**" },
                { "field_name": "view_intent", "value_text": "board" },
                { "field_name": "view_data_json", "value_text": "{\"slug\":\"X\"}" },
                { "field_name": "notes_to_self", "value_text": "note" },
            ]
        });
        let out = collapse_output_kv(&ai_result);
        assert_eq!(out["presentation_md"], json!("**hi**"));
        let vd = parse_view_data(&out);
        assert_eq!(vd, json!({"slug":"X"}));
        let final_out = build_present_output(&out, &ai_result, None, None);
        assert_eq!(final_out["presentation_md"], json!("**hi**"));
        assert_eq!(final_out["view_intent"], json!("board"));
        assert_eq!(final_out["view_data"], json!({"slug":"X"}));
        assert_eq!(final_out["ai_call_id"], json!("call-1"));
        assert_eq!(final_out["ai_telemetry"]["tier"], json!("mid"));
    }

    #[test]
    fn build_present_output_defaults_when_output_kv_missing() {
        let ai_result = json!({});
        let out = collapse_output_kv(&ai_result);
        let final_out = build_present_output(&out, &ai_result, None, None);
        assert_eq!(final_out["presentation_md"], json!(""));
        assert_eq!(final_out["view_intent"], json!("inline"));
        assert_eq!(final_out["view_data"], Value::Null);
        assert_eq!(final_out["notes_to_self"], json!(""));
        assert_eq!(final_out["ai_call_id"], Value::Null);
        assert_eq!(final_out["ai_telemetry"]["temperature"], json!("cold"));
        assert_eq!(final_out["ai_telemetry"]["cost_usd"], json!(0));
        assert_eq!(final_out["ai_telemetry"]["cached"], json!(false));
    }

    #[test]
    fn parse_view_data_invalid_json_stays_null() {
        let out = json!({ "view_data_json": "{not json" });
        assert_eq!(parse_view_data(&out), Value::Null);
        let out2 = json!({ "view_data_json": "   " });
        assert_eq!(parse_view_data(&out2), Value::Null);
    }

    #[test]
    fn telemetry_carries_provider_fields_and_tokens() {
        let ai_result = json!({ "model_id": "m1", "provider": "p1", "tier_used": "high", "temperature_used": "warm", "cost_usd": 0.02, "cached": true, "duration_ms": 123 });
        let t = build_ai_telemetry(&ai_result, Some(&json!(10)), Some(&json!(20)));
        assert_eq!(t["model_id"], json!("m1"));
        assert_eq!(t["tier"], json!("high"));
        assert_eq!(t["tokens_in"], json!(10));
        assert_eq!(t["tokens_out"], json!(20));
        assert_eq!(t["cached"], json!(true));
    }
}
