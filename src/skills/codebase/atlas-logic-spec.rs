//! atlas-logic-spec.rs — twin of skills/codebase/atlas-logic-spec.mjs (db lane). NAMES ARE
//! CONTRACT: default `atlasLogicSpec`. Designs the LOGIC of a change/feature as a structured,
//! language-agnostic spec (a "mini-AST") — the shape of the logic, reviewed BEFORE any code
//! exists — grounded in a named file's real source when given, via the already-converted
//! `codebase_codebase_file_read::codebaseFileRead` twin.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub — it always returns `Err`, so `atlasLogicSpec` propagates that error once the
//! (real, ported) deterministic surface above it has run: guard (db present, intent non-empty),
//! best-effort file-context assembly (JS wraps the whole block in try/catch — any failure,
//! including a missing/unreadable file, silently yields no context block, mirroring pl-research's
//! `fetch_knowledge_by_overlap`), and `ensure_prompt` (reusing the converted `ai_prompt_find`/
//! `ai_prompt_add` twins, same pattern as pl-research.rs/book-run.rs). The response-parsing
//! (`build_spec`) helper is ported as its own PURE function and unit-tested directly with a
//! canned `ai_result` — unreachable from `atlasLogicSpec` itself (the `?` on `aiCall`
//! short-circuits before it runs) but correct by construction and independently testable per the
//! crossing playbook's coverage rule.
// QUALITY: mixed unit — deterministic guard/context-assembly/db-read surface (fully ported,
// fixture-reachable up to the deferred aiCall boundary) + 1 pure response-shaping helper
// (unreachable from the default export, unit-tested directly) + 1 bespoke/deferred aiCall
// dispatch (no fixture/frontier).
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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
} // = JS String(v ?? '')

const PROMPT_NAME: &str = "atlas.logic.spec.v1";
const PROMPT_TEMPLATE_MD: &str = "Design the LOGIC of this as a structured spec a developer can review before writing any code — the data + control flow, not the syntax.

INTENT: {{intent}}

{{context}}

Return JSON only, exactly:
{\"name\":\"<the function / module name>\",\"summary\":\"<one line: what it does>\",\"inputs\":[{\"name\":\"<arg>\",\"type\":\"<type or shape>\"}],\"steps\":[{\"n\":1,\"action\":\"<short step>\",\"detail\":\"<the transform / decision / data-shape change>\"}],\"outputs\":[{\"name\":\"<result>\",\"type\":\"<type or shape>\"}],\"calls\":[\"<external fn / module it uses>\"]}";

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "spec_json", "field_type": "string", "required": true, "min_length": 10 },
    ])
}

// try promptFind; on miss, promptAdd (mirrors JS ensurePrompt's try/catch: existing &&
// existing.template_md === TEMPLATE short-circuits, else re-registers).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db) {
        if existing.get("template_md").and_then(Value::as_str) == Some(PROMPT_TEMPLATE_MD) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "system_md": "You design the LOGIC of a change or feature as a structured spec a developer reviews BEFORE any code is written — the shape of the logic, not the syntax. You output only strict JSON. Be concrete and language-agnostic: name the unit, state its inputs (name + type/shape), the ORDERED steps (each: a short action + the data transform or decision it performs), the outputs, and the external functions/modules it calls. Ground it in the provided context when given; invent no APIs. 3-8 steps — the essential logic, not line-by-line pseudocode.",
            "default_tier": "mid",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 1600,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    Ok(())
}

// = JS `Number(v)` — used only to test `Number.isFinite(Number(s.n))` below.
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}

// Pure — `d.steps.filter(s => s && (s.action || s.detail)).slice(0,12).map((s,i) => ({n, action, detail}))`.
fn build_steps(d: &Value) -> Vec<Value> {
    let arr = d.get("steps").and_then(Value::as_array).cloned().unwrap_or_default();
    arr.into_iter()
        .filter(|s| !s.is_null() && (truthy(s.get("action")) || truthy(s.get("detail"))))
        .take(12)
        .enumerate()
        .map(|(i, s)| {
            let num = js_number(s.get("n"));
            let n = if num.is_finite() { num as i64 } else { i as i64 + 1 };
            let action = str_qq(s.get("action")).trim().to_string();
            let detail = if truthy(s.get("detail")) { json!(str_qq(s.get("detail")).trim().to_string()) } else { Value::Null };
            json!({ "n": n, "action": action, "detail": detail })
        })
        .collect()
}

// Pure — `list.filter(x => x && (x.name || x.type)).slice(0,10).map(x => ({name, type}))`.
fn build_io(list: &Value) -> Vec<Value> {
    let arr = list.as_array().cloned().unwrap_or_default();
    arr.into_iter()
        .filter(|x| !x.is_null() && (truthy(x.get("name")) || truthy(x.get("type"))))
        .take(10)
        .map(|x| {
            let name = if truthy(x.get("name")) { str_qq(x.get("name")).trim().to_string() } else { "?".to_string() };
            let type_ = if truthy(x.get("type")) { json!(str_qq(x.get("type")).trim().to_string()) } else { Value::Null };
            json!({ "name": name, "type": type_ })
        })
        .collect()
}

// Pure — decomposes the parsed `spec_json` blob (`d`) into the final `spec` shape. Returns
// `Err` for `spec.steps.length < 1` (mirrors JS `{ ok:false, error:'spec has no steps' }`).
fn build_spec(d: &Value, file: Option<&Value>) -> Result<Value, String> {
    let steps = build_steps(d);
    if steps.is_empty() {
        return Err("spec has no steps".to_string());
    }
    let calls: Vec<Value> = d
        .get("calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|c| str_qq(Some(&c)).trim().to_string())
        .filter(|c| !c.is_empty())
        .take(12)
        .map(|c| json!(c))
        .collect();
    Ok(json!({
        "name": if truthy(d.get("name")) { json!(str_qq(d.get("name")).trim().to_string()) } else { Value::Null },
        "summary": if truthy(d.get("summary")) { json!(str_qq(d.get("summary")).trim().to_string()) } else { Value::Null },
        "file": file.cloned().unwrap_or(Value::Null),
        "inputs": build_io(d.get("inputs").unwrap_or(&Value::Null)),
        "steps": steps,
        "outputs": build_io(d.get("outputs").unwrap_or(&Value::Null)),
        "calls": calls,
    }))
}

pub fn atlasLogicSpec(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = if truthy(input.get("origin")) { str_qq(input.get("origin")) } else { "atlas".to_string() };
    let _ = origin; // JS reads `origin` but never uses it beyond the destructure — kept for parity readability.
    let intent: String = str_qq(input.get("intent")).chars().take(800).collect();
    if intent.trim().is_empty() {
        return Err("no intent".to_string());
    }

    // optional file context — best-effort, mirrors JS try/catch (any failure yields no block).
    let mut ctx_block = String::new();
    if truthy(input.get("file")) {
        let file = input.get("file").unwrap();
        if let Ok(res) = crate::codebase_codebase_file_read::codebaseFileRead(&json!({ "path": file }), db) {
            let content = str_qq(res.get("content"));
            if !content.trim().is_empty() {
                let truncated: String = content.chars().take(3000).collect();
                ctx_block = format!("EXISTING FILE ({}) — design to fit this:\n{}", str_qq(Some(file)), truncated);
            }
        }
    }

    ensure_prompt(db)?;

    // ── the AI call: PARITY-DEFERRED from here on (see module doc) — aiCall always returns Err.
    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "intent", "value": intent },
            { "key": "context", "value": ctx_block },
        ],
        "tier": "mid",
        "temperature": "cold",
        "phase_flow": "single_only",
        "provider_fn": Value::Null,
    });
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let raw = ai_result
        .get("output_kv")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find(|o| o.get("field_name").and_then(Value::as_str) == Some("spec_json") && o.get("parent_output_id").map_or(true, Value::is_null)))
        .and_then(|o| o.get("value_text"))
        .filter(|v| !v.is_null());
    let raw_str = match raw.and_then(Value::as_str) {
        Some(s) => s,
        None => return Err("no spec returned".to_string()),
    };
    let d: Value = match serde_json::from_str(raw_str) {
        Ok(v) => v,
        Err(_) => return Err("spec parse failed".to_string()),
    };

    let spec = build_spec(&d, input.get("file"))?;
    Ok(json!({ "ok": true, "spec": spec }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (root_path TEXT, is_active INTEGER);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn empty_intent_is_rejected() {
        let c = setup();
        assert_eq!(atlasLogicSpec(&json!({}), &c).unwrap_err(), "no intent");
        assert_eq!(atlasLogicSpec(&json!({ "intent": "   " }), &c).unwrap_err(), "no intent");
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_after_full_deterministic_surface() {
        // proves guard/ensure_prompt ran cleanly and the error is aiCall's deferred stub.
        let c = setup();
        let err = atlasLogicSpec(&json!({ "intent": "design a rate limiter" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.logic.spec.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn missing_or_unreadable_file_is_best_effort_and_still_reaches_boundary() {
        let c = setup();
        // no active workspace + no root → codebaseFileRead errors → caught, ctx_block stays empty.
        let err = atlasLogicSpec(&json!({ "intent": "fix the thing", "file": "skills/x.mjs" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
    }

    #[test]
    fn build_steps_filters_falsy_and_dedupes_n() {
        let d = json!({ "steps": [
            null,
            { "action": "", "detail": "" },
            { "n": 3, "action": "validate input", "detail": "check shape" },
            { "action": "no n given" },
        ] });
        let steps = build_steps(&d);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0], json!({ "n": 3, "action": "validate input", "detail": "check shape" }));
        assert_eq!(steps[1], json!({ "n": 2, "action": "no n given", "detail": Value::Null }));
    }

    #[test]
    fn build_steps_caps_at_12() {
        let mut arr = Vec::new();
        for i in 0..20 {
            arr.push(json!({ "n": i, "action": format!("step{i}") }));
        }
        let steps = build_steps(&json!({ "steps": arr }));
        assert_eq!(steps.len(), 12);
    }

    #[test]
    fn build_io_defaults_missing_name_to_question_mark_and_caps_at_10() {
        let mut arr = Vec::new();
        for i in 0..15 {
            arr.push(json!({ "type": format!("T{i}") }));
        }
        let io = build_io(&json!(arr));
        assert_eq!(io.len(), 10);
        assert_eq!(io[0], json!({ "name": "?", "type": "T0" }));
    }

    #[test]
    fn build_spec_errors_on_no_steps() {
        let d = json!({ "name": "foo", "steps": [] });
        assert_eq!(build_spec(&d, None).unwrap_err(), "spec has no steps");
    }

    #[test]
    fn build_spec_happy_path_shapes_and_caps_calls_at_12() {
        let mut calls = Vec::new();
        for i in 0..15 {
            calls.push(json!(format!("call{i}")));
        }
        let d = json!({
            "name": "  fooBar  ", "summary": "  does a thing  ",
            "inputs": [{ "name": "x", "type": "number" }],
            "steps": [{ "n": 1, "action": "do it", "detail": "the transform" }],
            "outputs": [{ "name": "y", "type": "string" }],
            "calls": calls,
        });
        let s = build_spec(&d, Some(&json!("skills/x.mjs"))).unwrap();
        assert_eq!(s["name"], json!("fooBar"));
        assert_eq!(s["summary"], json!("does a thing"));
        assert_eq!(s["file"], json!("skills/x.mjs"));
        assert_eq!(s["inputs"], json!([{ "name": "x", "type": "number" }]));
        assert_eq!(s["outputs"], json!([{ "name": "y", "type": "string" }]));
        assert_eq!(s["calls"].as_array().unwrap().len(), 12);
    }

    #[test]
    fn build_spec_missing_name_and_summary_are_null() {
        let d = json!({ "steps": [{ "n": 1, "action": "x" }] });
        let s = build_spec(&d, None).unwrap();
        assert_eq!(s["name"], Value::Null);
        assert_eq!(s["summary"], Value::Null);
        assert_eq!(s["file"], Value::Null);
        assert_eq!(s["calls"], json!([]));
    }
}
