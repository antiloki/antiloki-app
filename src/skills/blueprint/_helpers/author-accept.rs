//! author-accept.rs — twin of skills/blueprint/_helpers/author-accept.mjs. THE ONE accept rule
//! for any code-emitting AI call. NAMES ARE CONTRACT: isTruncatedCall · acceptAuthoredCode.
//!
//! `acceptAuthoredCode` is the JS module's only async fn — per the async-lane rule it stays an
//! ordinary sync `pub fn` here (it only awaits `parseDecls`, itself sync-over-async in its own
//! twin). Both fns cross-call the already-converted `blueprint__helpers_code_checks` twin.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

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
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}

fn export_word_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"\bexport\b").unwrap())
}

/// db-first helper: isTruncatedCall(db, ai_call_id, margin=8). `db` is a mandatory `&Connection`
/// in this twin (the harness never dispatches without one), so the `!db` half of the JS guard
/// has no Rust counterpart; the `!ai_call_id` half is `truthy`. `margin` defaults to 8 when
/// undefined/null, matching the JS default parameter.
#[allow(non_snake_case)]
pub fn isTruncatedCall(db: &Connection, ai_call_id: &Value, margin: &Value) -> Value {
    if !truthy(Some(ai_call_id)) {
        return json!(false);
    }
    let margin = nz(Some(margin)).map(|m| js_number(Some(m))).unwrap_or(8.0);
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT a.tokens_out, p.max_output_tokens FROM brain.ai_calls a JOIN brain.prompt_templates p ON p.id = a.prompt_template_id WHERE a.id = ?",
        &[ai_call_id.clone()],
    ) {
        Ok(v) => v,
        Err(_) => return json!(false),
    };
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r,
        None => return json!(false),
    };
    let max_output_tokens = row.get("max_output_tokens");
    if !truthy(max_output_tokens) {
        return json!(false);
    }
    let tokens_out = js_number(row.get("tokens_out"));
    let max_output_tokens = js_number(max_output_tokens);
    json!(tokens_out >= max_output_tokens - margin)
}

/// db-first helper: acceptAuthoredCode(db, { code, path, ai_call_id }). THE accept rule — the
/// authored code must ACTUALLY PARSE (via the already-converted `parseDecls` twin), never a
/// proxy check.
#[allow(non_snake_case)]
pub fn acceptAuthoredCode(db: &Connection, input: &Value) -> Value {
    let code = input.get("code");
    let path = nz(input.get("path")).cloned().unwrap_or(Value::Null);
    let ai_call_id = nz(input.get("ai_call_id")).cloned().unwrap_or(Value::Null);

    let s = str_qq(code).trim().to_string();
    if s.is_empty() {
        return json!({ "ok": false, "reason": "empty", "truncated": false });
    }
    if truthy(Some(&crate::blueprint__helpers_code_checks::isDegenerateSource(&json!(s)))) {
        return json!({ "ok": false, "reason": "degenerate", "truncated": false });
    }
    let truncated = truthy(Some(&isTruncatedCall(db, &ai_call_id, &json!(8))));

    let parsed = crate::blueprint__helpers_code_checks::parseDecls(&path, &json!(s));
    let parses = parsed.get("parses").map_or(true, |p| p.as_bool() != Some(false));
    if !parses {
        return json!({ "ok": false, "reason": "unparseable", "truncated": truncated });
    }
    if truncated && !export_word_re().is_match(&s) {
        return json!({ "ok": false, "reason": "truncated", "truncated": truncated });
    }
    json!({ "ok": true, "reason": "ok", "truncated": truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        db.execute_batch(
            "CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY, max_output_tokens INTEGER);
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, prompt_template_id INTEGER, tokens_out INTEGER);",
        ).unwrap();
        db
    }

    #[test]
    fn truncated_call_false_without_ai_call_id() {
        let db = setup();
        assert_eq!(isTruncatedCall(&db, &Value::Null, &json!(8)), json!(false));
    }

    #[test]
    fn truncated_call_true_when_tokens_out_reaches_cap_minus_margin() {
        let db = setup();
        db.execute("INSERT INTO brain.prompt_templates (id, max_output_tokens) VALUES (1, 1000)", []).unwrap();
        db.execute("INSERT INTO brain.ai_calls (id, prompt_template_id, tokens_out) VALUES (10, 1, 995)", []).unwrap();
        assert_eq!(isTruncatedCall(&db, &json!(10), &json!(8)), json!(true));
    }

    #[test]
    fn truncated_call_false_when_well_under_cap() {
        let db = setup();
        db.execute("INSERT INTO brain.prompt_templates (id, max_output_tokens) VALUES (1, 1000)", []).unwrap();
        db.execute("INSERT INTO brain.ai_calls (id, prompt_template_id, tokens_out) VALUES (11, 1, 200)", []).unwrap();
        assert_eq!(isTruncatedCall(&db, &json!(11), &json!(8)), json!(false));
    }

    #[test]
    fn accept_empty_code_rejected() {
        let db = setup();
        let r = acceptAuthoredCode(&db, &json!({ "code": "", "path": "foo.js" }));
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["reason"], json!("empty"));
    }

    #[test]
    fn accept_degenerate_short_echo_rejected() {
        let db = setup();
        let r = acceptAuthoredCode(&db, &json!({ "code": "stub_source", "path": "foo.js" }));
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["reason"], json!("degenerate"));
    }

    #[test]
    fn accept_valid_parsing_code_ok() {
        let db = setup();
        let r = acceptAuthoredCode(&db, &json!({ "code": "export function add(a, b) {\n  return a + b;\n}\n", "path": "foo.js" }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["reason"], json!("ok"));
        assert_eq!(r["truncated"], json!(false));
    }

    // .js goes through parse-js.rs's native oxc_parser, which (per that twin's documented
    // divergence) never fails to produce a Program for these inputs — so the unparseable branch
    // is exercised via a tree-sitter-grammar extension (.tsx) instead, matching how
    // code-checks.rs's own parse_decls_js_function test only proves the happy path for .js.
    #[test]
    fn accept_unparseable_code_rejected() {
        let db = setup();
        let r = acceptAuthoredCode(&db, &json!({ "code": "function broken(a, {\n  return <div", "path": "foo.tsx" }));
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["reason"], json!("unparseable"));
    }

    #[test]
    fn accept_truncated_without_export_rejected() {
        let db = setup();
        db.execute("INSERT INTO brain.prompt_templates (id, max_output_tokens) VALUES (1, 1000)", []).unwrap();
        db.execute("INSERT INTO brain.ai_calls (id, prompt_template_id, tokens_out) VALUES (20, 1, 995)", []).unwrap();
        let r = acceptAuthoredCode(&db, &json!({ "code": "const x = 1;\nfunction foo() {}", "path": "foo.js", "ai_call_id": 20 }));
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["reason"], json!("truncated"));
        assert_eq!(r["truncated"], json!(true));
    }

    #[test]
    fn accept_truncated_but_has_export_is_salvaged() {
        let db = setup();
        db.execute("INSERT INTO brain.prompt_templates (id, max_output_tokens) VALUES (1, 1000)", []).unwrap();
        db.execute("INSERT INTO brain.ai_calls (id, prompt_template_id, tokens_out) VALUES (21, 1, 995)", []).unwrap();
        let r = acceptAuthoredCode(&db, &json!({ "code": "export function foo() {\n  return 1;\n}\n", "path": "foo.js", "ai_call_id": 21 }));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["truncated"], json!(true));
    }
}
