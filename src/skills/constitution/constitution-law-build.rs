//! constitution-law-build.rs — twin of skills/constitution/constitution-law-build.mjs (db lane,
//! cross-db brain.* via the ai-call/prompt-find/prompt-add twins). NAMES ARE CONTRACT: default
//! `constitutionLawBuild`. AI BUILDS ONE CUSTOM LAW from a plain-English description: the user
//! describes a code-quality rule, the AI returns a law with a DETERMINISTIC detector (regex/
//! absent/path/import/metric/ast). It does NOT persist — the wizard shows the built law for review
//! and the operator creates it separately.
//!
//! `context.db` missing → JS throws `err.required('constitution-law-build', 'context.db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin). `context.provider_fn` (a JS function reference) has no Rust
//! equivalent to carry — threaded as `null` into the constructed aiCall input, same as
//! pl-research.rs's `provider_fn`.
//!
//! `ensure_prompt` mirrors the JS `ensurePrompt`'s CONTENT-EQUALITY check (not book-run.rs's
//! find-or-add pattern): a `promptFind` hit is reused as-is only when its `template_md` AND
//! `system_md` both match this module's constants byte-for-byte; any miss (not-found, or either
//! field drifted) falls through to `promptAdd`, which bumps the version. `promptFind`'s Err (name
//! not found) collapses to `None`, matching JS's `try { e = promptFind(...) } catch { e = null; }`.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`. Unlike the fire-and-
//! propagate pattern in book-run.rs/pl-research.rs, THIS module's JS wraps the aiCall await in its
//! own try/catch and returns a real `{ok:false, ...}` value on failure (not a thrown exception) —
//! ported here as a `match` on `aiCall`'s `Result`, so the deferred stub's `Err` is caught and
//! reshaped into the same `{ok:false, law:null, ai_call_id:null, cost_usd:0, error}` JSON the real
//! JS produces for a live network failure. This IS the module's one real reachable outcome given
//! the stub. Everything past a successful `aiCall` (the `extract_json`/`find_output_text` response
//! parse, the detector/regex/metric validators, and the final law-shaping) is ported byte-for-byte
//! from the JS and unit-tested directly against canned `ai_result`-shaped values — unreachable from
//! `constitutionLawBuild` itself while aiCall is stubbed, but correct by construction (same pattern
//! as book-run.rs's `parse_arr`/`parse_obj`, pl-research.rs's `build_research_report`).
// QUALITY: mixed unit — deterministic guard/prompt-ensure surface (fully ported, fixture-reachable
// up to the deferred aiCall boundary, whose Err is caught and reshaped into a real ok:false
// return) + pure response-parse/validation/shaping helpers (unreachable from the default export
// while aiCall is stubbed, unit-tested directly) + 1 bespoke/deferred aiCall dispatch (no fixture).

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

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
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
} // = JS Number(v)

// JS `.slice(0, n)` on a string — code-point based here (playbook gotcha #5: JS .length/.slice
// are UTF-16 code units; ASCII/BMP text this module handles is exact either way).
fn slice_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

const PROMPT: &str = "constitution-law-build";
const SYSTEM: &str = "You convert a plain-English code-quality rule into ONE deterministic, scannable law. Pick the SIMPLEST detector that catches it — a MATCH must mean a VIOLATION:\n- regex: a JS regex over the source; a match is the violation (style, naming, forbidden calls). config {pattern, flags?}\n- absent: a JS regex that MUST be present; MISSING it is the violation (e.g. every file carries a license header). config {pattern}\n- path: a JS regex over the file PATH (file placement / naming). config {pattern}\n- import: like regex but only import/require lines (forbidden dependencies). config {pattern}\n- metric: a threshold over an atlas metric in {loc, complexity, fan_in, fan_out}; over max is the violation. config {metric, max}\n- ast: a tree-sitter S-expression query per grammar for STRUCTURAL rules regex can't express. config {queries:{tsx?:\"(for_statement) @hit\", python?:\"...\"}}\nRules: (1) STRONGLY prefer regex/metric — they always work; use ast only for a real structural rule; (2) a regex must be a valid JS regex and flag the violation; (3) scope by language when the rule is language-specific. Output ONLY JSON: {\"title\":\"short name\",\"rule_md\":\"one sentence: the rule and why\",\"detector\":\"regex|absent|path|import|metric|ast\",\"config\":{...},\"weight\":1-10,\"severity\":\"info|warn|error|critical\",\"languages\":[\"ts\",\"js\",\"py\"]}. No prose outside the JSON.";
const TEMPLATE: &str = "Build a code-quality law for this rule:\n\n{{description}}\n\nOutput the law JSON.";

// try promptFind; a miss (Err, or content drifted from this module's constants) falls through to
// promptAdd (mirrors JS ensurePrompt's try/catch + template_md/system_md equality check).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if let Ok(e) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db) {
        let tpl_match = e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE);
        let sys_match = e.get("system_md").and_then(Value::as_str) == Some(SYSTEM);
        if tpl_match && sys_match {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "mid",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 900,
            "output_fields": [{ "field_name": "law_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

// `function extractJson(t) { const s = String(t ?? ''); const a = s.indexOf('{'), b =
// s.lastIndexOf('}'); if (a < 0 || b <= a) return null; try { return JSON.parse(s.slice(a, b+1)); }
// catch { return null; } }` — pure.
fn extract_json(t: Option<&Value>) -> Option<Value> {
    let s = str_qq(t);
    let a = s.find('{')?;
    let b = s.rfind('}')?;
    if b <= a {
        return None;
    }
    serde_json::from_str(&s[a..=b]).ok()
}

// `res?.output_kv?.find((o) => o.field_name === name && o.parent_output_id == null)?.value_text`
// (JS `?? ''` on the whole chain is applied by the caller via extract_json's own `t ?? ''`, so this
// returns Null on a miss same as the JS optional-chain would before the `??`).
fn find_output_text(res: &Value, field_name: &str) -> Value {
    res.get("output_kv")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter().find(|o| {
                o.get("field_name").and_then(Value::as_str) == Some(field_name)
                    && matches!(o.get("parent_output_id"), None | Some(Value::Null))
            })
        })
        .and_then(|o| o.get("value_text"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn detectors() -> &'static HashSet<&'static str> {
    use std::sync::OnceLock;
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| ["regex", "absent", "path", "import", "metric", "ast"].into_iter().collect())
}
fn regexy() -> &'static HashSet<&'static str> {
    use std::sync::OnceLock;
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| ["regex", "absent", "path", "import"].into_iter().collect())
}

// Validate + shape a successfully-parsed AI body into `{ok, law, ai_call_id, cost_usd}` OR an
// `{ok:false, ...error}` — the JS function body after `extractJson`, ported as its own pure fn so
// it is directly unit-testable (unreachable from `constitutionLawBuild` while `aiCall` is a
// deferred stub, per this module's doc).
fn validate_and_shape(body: Option<&Value>, ai_call_id: &Value, cost: f64) -> Value {
    let cost_r = (cost * 10000.0).round() / 10000.0; // Number(cost.toFixed(4))
    // `!body || !DETECTORS.has(body.detector)` — a missing body, a missing detector, or an
    // unrecognized detector all collapse to the same "did not return a usable detector" error.
    let detector = body.and_then(|b| b.get("detector")).and_then(Value::as_str);
    let (Some(body), Some(detector)) = (body, detector.filter(|d| detectors().contains(d))) else {
        return json!({ "ok": false, "law": Value::Null, "ai_call_id": ai_call_id, "cost_usd": cost_r, "error": "the model did not return a usable detector" });
    };
    let config = body.get("config").cloned();
    if regexy().contains(detector) {
        let pattern = str_qq(config.as_ref().and_then(|c| c.get("pattern")));
        if regex::Regex::new(&pattern).is_err() {
            return json!({ "ok": false, "law": Value::Null, "ai_call_id": ai_call_id, "cost_usd": cost_r, "error": "the built regex did not compile — reword and retry" });
        }
        if !truthy(config.as_ref().and_then(|c| c.get("pattern"))) {
            return json!({ "ok": false, "law": Value::Null, "ai_call_id": ai_call_id, "cost_usd": cost_r, "error": "the model returned an empty pattern" });
        }
    }
    if detector == "metric" {
        let metric = config.as_ref().and_then(|c| c.get("metric")).and_then(Value::as_str);
        if !matches!(metric, Some("loc") | Some("complexity") | Some("fan_in") | Some("fan_out")) {
            return json!({ "ok": false, "law": Value::Null, "ai_call_id": ai_call_id, "cost_usd": cost_r, "error": "the model chose an unknown metric" });
        }
    }
    let w = js_number(body.get("weight"));
    let w = if w.is_nan() || w == 0.0 { 5.0 } else { w };
    let weight = w.max(1.0).min(10.0);
    let severity = match body.get("severity").and_then(Value::as_str) {
        Some(s @ ("info" | "warn" | "error" | "critical")) => s,
        _ => "warn",
    };
    let languages: Vec<Value> = match body.get("languages").and_then(Value::as_array) {
        Some(a) => a
            .iter()
            .filter(|l| matches!(l.as_str(), Some("ts") | Some("js") | Some("py")))
            .cloned()
            .collect(),
        None => vec![],
    };
    // `String(body.title ?? 'custom law').slice(0, 80)` — default fires only on null/undefined.
    let title_final = if body.get("title").map_or(true, Value::is_null) { "custom law".to_string() } else { str_qq(body.get("title")) };
    let title_final = slice_chars(&title_final, 80);
    let rule_md = if body.get("rule_md").map_or(true, Value::is_null) { String::new() } else { str_qq(body.get("rule_md")) };
    let rule_md = slice_chars(&rule_md, 400);

    // `body.config ?? {}` — null or absent both default; a present non-null config (even a
    // non-object) passes through as-is, same as JS.
    let config_final = match &config {
        None | Some(Value::Null) => json!({}),
        Some(v) => v.clone(),
    };
    let law = json!({
        "title": title_final,
        "rule_md": rule_md,
        "detector": detector,
        "config": config_final,
        "weight": weight,
        "severity": severity,
        "languages": languages,
    });
    json!({ "ok": true, "law": law, "ai_call_id": ai_call_id, "cost_usd": cost_r })
}

pub fn constitutionLawBuild(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    let description = str_qq(input.get("description")).trim().to_string();
    if description.is_empty() {
        return Err("constitution-law-build: description is required".to_string());
    }
    ensure_prompt(db)?;

    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [{ "key": "description", "value": description }],
        "provider_fn": Value::Null,
    });
    match crate::ai_ai_call::aiCall(&ai_input, db) {
        Err(e) => Ok(json!({
            "ok": false,
            "law": Value::Null,
            "ai_call_id": Value::Null,
            "cost_usd": 0,
            "error": slice_chars(&e, 200),
        })),
        Ok(res) => {
            let ai_call_id = res.get("ai_call_id").cloned().unwrap_or(Value::Null);
            // `Number(res?.cost_usd ?? 0)` — null/undefined defaults to literal 0 before Number().
            let cost = match res.get("cost_usd") {
                None | Some(Value::Null) => 0.0,
                Some(v) => js_number(Some(v)),
            };
            let text = find_output_text(&res, "law_json");
            let body = extract_json(Some(&text));
            Ok(validate_and_shape(body.as_ref(), &ai_call_id, cost))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS brain;").unwrap();
        crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn extract_json_finds_first_and_last_brace() {
        let v = extract_json(Some(&json!("prefix {\"a\":1} suffix"))).unwrap();
        assert_eq!(v, json!({"a":1}));
    }

    #[test]
    fn extract_json_none_when_no_braces_or_malformed() {
        assert!(extract_json(Some(&json!("no braces here"))).is_none());
        assert!(extract_json(Some(&json!("{not json}"))).is_none());
        assert!(extract_json(None).is_none());
    }

    #[test]
    fn find_output_text_matches_top_level_field_only() {
        let res = json!({ "output_kv": [
            { "field_name": "law_json", "parent_output_id": null, "value_text": "{\"detector\":\"regex\"}" },
            { "field_name": "law_json", "parent_output_id": 3, "value_text": "nested" },
        ]});
        assert_eq!(find_output_text(&res, "law_json"), json!("{\"detector\":\"regex\"}"));
    }

    #[test]
    fn validate_and_shape_rejects_unknown_detector() {
        let body = json!({"detector": "nope"});
        let out = validate_and_shape(Some(&body), &Value::Null, 0.0);
        assert_eq!(out["ok"], json!(false));
        assert_eq!(out["error"], json!("the model did not return a usable detector"));
    }

    #[test]
    fn validate_and_shape_rejects_empty_regex_pattern() {
        let body = json!({"detector": "regex", "config": {}});
        let out = validate_and_shape(Some(&body), &Value::Null, 0.0);
        assert_eq!(out["ok"], json!(false));
        assert_eq!(out["error"], json!("the model returned an empty pattern"));
    }

    #[test]
    fn validate_and_shape_rejects_unknown_metric() {
        let body = json!({"detector": "metric", "config": {"metric": "nonsense", "max": 10}});
        let out = validate_and_shape(Some(&body), &Value::Null, 0.0);
        assert_eq!(out["ok"], json!(false));
        assert_eq!(out["error"], json!("the model chose an unknown metric"));
    }

    #[test]
    fn validate_and_shape_builds_law_with_defaults() {
        let body = json!({
            "detector": "regex",
            "config": {"pattern": "console\\.log"},
            "title": null,
            "rule_md": null,
            "severity": "bogus",
            "weight": 0,
            "languages": ["ts", "cobol", "py"],
        });
        let out = validate_and_shape(Some(&body), &json!(42), 0.0000501);
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["ai_call_id"], json!(42));
        assert_eq!(out["cost_usd"], json!(0.0001));
        let law = &out["law"];
        assert_eq!(law["title"], json!("custom law"));
        assert_eq!(law["rule_md"], json!(""));
        assert_eq!(law["severity"], json!("warn"));
        assert_eq!(law["weight"], json!(5.0));
        assert_eq!(law["languages"], json!(["ts", "py"]));
    }

    #[test]
    fn validate_and_shape_clamps_weight_and_slices_title() {
        let long_title = "x".repeat(100);
        let body = json!({
            "detector": "metric",
            "config": {"metric": "loc", "max": 300},
            "title": long_title,
            "weight": 999,
        });
        let out = validate_and_shape(Some(&body), &Value::Null, 0.0);
        assert_eq!(out["law"]["weight"], json!(10.0));
        assert_eq!(out["law"]["title"].as_str().unwrap().len(), 80);
    }

    #[test]
    fn constitution_law_build_requires_description() {
        let c = setup();
        let err = constitutionLawBuild(&json!({}), &c).unwrap_err();
        assert!(err.contains("description is required"));
    }

    #[test]
    fn constitution_law_build_returns_ok_false_on_deferred_ai_call() {
        let c = setup();
        let out = constitutionLawBuild(&json!({"description": "no console.log in prod code"}), &c).unwrap();
        assert_eq!(out["ok"], json!(false));
        assert_eq!(out["law"], Value::Null);
        assert_eq!(out["cost_usd"], json!(0));
        assert!(out["error"].as_str().unwrap().contains("ai-call"));
    }
}
