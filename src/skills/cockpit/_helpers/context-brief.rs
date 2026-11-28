//! context-brief.rs — twin of skills/cockpit/_helpers/context-brief.mjs (no-db + db-first
//! helpers). NAMES ARE CONTRACT: estTokens · resolveSourceText · parseExcludes ·
//! resolveContextDocs · missionValue. The ONE place a pane's readable context is assembled:
//! `resolveContextDocs` classifies each session context doc, embedding LIVE source text (via
//! the already-converted `notebooks_notebook_corpus::gather` twin) for `CONTEXT SOURCE — <label>:`
//! docs and passing everything else through verbatim; `missionValue` composes the final
//! 24k-capped mission_md the pane prompt carries.
// QUALITY: 80/100 — thin db-first wrapper over an existing twin, deterministic classification
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse::<f64>().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() } // JS .slice(0,n) (BMP-equivalent)
fn number_val(n: f64) -> Value { if n.is_finite() && n.fract() == 0.0 { json!(n as i64) } else { json!(n) } }

// = SOURCE_KIND_BY_LABEL: the studio's source-chip labels → notebook-corpus gather kinds
fn source_kind_by_label(label: &str) -> Option<&'static str> {
    match label {
        "AST" => Some("ast"),
        "Descriptions" => Some("descriptions"),
        "Digest" => Some("digest"),
        "Entry points" => Some("entrypoints"),
        "Data stories" => Some("data_stories"),
        "Design" => Some("design"),
        "Tests" => Some("tests"),
        "Git" => Some("git"),
        "Docs" => Some("docs"),
        "Cards" => Some("cards"),
        "Contracts" => Some("contracts"),
        "Failures" => Some("failures"),
        "Decisions" => Some("decisions"),
        "Timeline" => Some("timeline"),
        "Memory" => Some("memory"),
        "Stack Gazette" => Some("faculty_arch"),
        "Market Herald" => Some("faculty_product"),
        _ => None,
    }
}

fn context_source_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new("^CONTEXT SOURCE — (.+?):").unwrap())
}

pub fn estTokens(s: &Value) -> Value {
    let len = str_qq(Some(s)).chars().count() as f64;
    number_val((len / 4.0).ceil())
}

// resolve ONE source kind to its live grounded text. ast + descriptions gather per-file —
// preview them over the atlas's own files (empty scope on a missing/absent atlas table).
pub fn resolveSourceText(db: &Connection, kind: &Value, context: &Value) -> Value {
    let kind_str = str_qq(Some(kind));
    if let Some(slug) = kind_str.strip_prefix("pack:") { return json!(crate::context_context_library::pack_body(db, slug).unwrap_or_default()); }
    let mut paths: Vec<Value> = Vec::new();
    if kind_str == "ast" || kind_str == "descriptions" {
        if let Ok(rows) = crate::db_json::query_rows(db, "SELECT path FROM code_nodes WHERE origin = 'atlas' AND type = 'file' ORDER BY path LIMIT 60", &[]) {
            if let Some(arr) = rows.as_array() {
                paths = arr.iter().filter_map(|r| r.get("path").cloned()).collect();
            }
        }
    }
    let scope = json!({ "paths": paths });
    let s = json!({ "kind": kind_str });
    let result = crate::notebooks_notebook_corpus::gather(db, &s, &scope, context);
    json!(str_qq(Some(&result)))
}

pub fn parseExcludes(json_v: &Value) -> Value {
    let raw = match json_v {
        Value::Null => "[]".to_string(),
        Value::String(s) => s.clone(),
        other => tmpl(Some(other)), // JSON.parse(x) on a non-string coerces x via ToString first
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Array(arr)) => {
            let nums: Vec<Value> = arr.iter()
                .map(|x| js_number(Some(x)))
                .filter(|n| *n != 0.0 && !n.is_nan())
                .map(number_val)
                .collect();
            json!(nums)
        }
        _ => json!([]),
    }
}

// Classify + resolve the session's shared context docs for one pane. A `CONTEXT SOURCE —
// <label>:` doc EMBEDS its live content; other docs pass through verbatim. Excluded ids are
// returned flagged (not silently dropped) so the transparency surfaces can show the full picture.
pub fn resolveContextDocs(db: &Connection, docs: &Value, excludes: &Value, context: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let docs_arr = docs.as_array().unwrap_or(&empty);
    let excl_nums: Vec<f64> = excludes.as_array().map(|a| a.iter().map(|v| js_number(Some(v))).collect()).unwrap_or_default();
    let re = context_source_re();
    let out: Vec<Value> = docs_arr.iter().map(|d| {
        let id_num = js_number(d.get("id"));
        let excluded = excl_nums.iter().any(|&e| e == id_num);
        let body_md = str_qq(d.get("body_md"));
        let caps = re.captures(&body_md);
        let label_text = caps.as_ref().and_then(|c| c.get(1)).map(|m| m.as_str().to_string());
        let kind = label_text.as_deref().and_then(source_kind_by_label);
        match kind {
            None => {
                let first_line = body_md.split('\n').next().unwrap_or("");
                let label = take_chars(first_line, 60);
                json!({
                    "id": d.get("id").cloned().unwrap_or(Value::Null),
                    "label": label,
                    "source": Value::Null,
                    "body": body_md,
                    "tokens": estTokens(&json!(body_md)),
                    "excluded": excluded,
                })
            }
            Some(k) => {
                let label_text = label_text.unwrap();
                let text = if excluded { String::new() } else {
                    let v = resolveSourceText(db, &json!(k), context);
                    str_qq(Some(&v))
                };
                let body_inner = if !text.is_empty() { take_chars(&text, 6000) } else { "(this source is empty right now)".to_string() };
                let body = format!("── LIVE SOURCE: {label_text} (embedded at turn time) ──\n{body_inner}");
                json!({
                    "id": d.get("id").cloned().unwrap_or(Value::Null),
                    "label": label_text,
                    "source": k,
                    "body": body,
                    "tokens": estTokens(&json!(body)),
                    "excluded": excluded,
                })
            }
        }
    }).collect();
    json!(out)
}

// The exact `mission_md` value the pane prompt carries: mission + every INCLUDED context doc,
// sources embedded. Same 24k cap the prompt always had.
pub fn missionValue(mission_md: &Value, resolved_docs: &Value) -> Value {
    let mut parts: Vec<String> = vec![str_qq(Some(mission_md))];
    if let Some(arr) = resolved_docs.as_array() {
        for d in arr {
            if truthy(d.get("excluded")) { continue; }
            let id = tmpl(d.get("id"));
            let body = tmpl(d.get("body"));
            parts.push(format!("--- CONTEXT DOC #{id} ---\n{body}"));
        }
    }
    let joined = parts.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n");
    json!(take_chars(&joined, 24000))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE cards (slug TEXT, title TEXT, status TEXT, updated_at TEXT, created_at TEXT);
             CREATE TABLE code_nodes (path TEXT, origin TEXT, type TEXT, name TEXT, signature TEXT);",
        ).unwrap();
        db
    }

    #[test]
    fn est_tokens_ceils_quarter_length() {
        assert_eq!(estTokens(&json!("hello")), json!(2)); // ceil(5/4)
        assert_eq!(estTokens(&Value::Null), json!(0));
        assert_eq!(estTokens(&json!("12345678")), json!(2)); // ceil(8/4)
    }

    #[test]
    fn resolve_source_text_cards_and_ast() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, title, status, updated_at, created_at) VALUES ('CARD-1','First card','backlog','2026-01-01 00:00:00','2025-12-01 00:00:00')", []).unwrap();
        let cards_text = resolveSourceText(&db, &json!("cards"), &json!({}));
        assert!(cards_text.as_str().unwrap().contains("CARD-1"));

        db.execute("INSERT INTO code_nodes (path, origin, type, name, signature) VALUES ('src/foo.mjs','atlas','file',NULL,NULL)", []).unwrap();
        db.execute("INSERT INTO code_nodes (path, origin, type, name, signature) VALUES ('src/foo.mjs','atlas','function','foo','function foo()')", []).unwrap();
        let ast_text = resolveSourceText(&db, &json!("ast"), &json!({}));
        assert!(ast_text.as_str().unwrap().contains("src/foo.mjs"));

        let digest_text = resolveSourceText(&db, &json!("digest"), &json!({}));
        assert_eq!(digest_text, json!(""));
    }

    #[test]
    fn parse_excludes_filters_falsy_and_catches_bad_json() {
        assert_eq!(parseExcludes(&json!("[1,2,0,3]")), json!([1, 2, 3]));
        assert_eq!(parseExcludes(&Value::Null), json!([]));
        assert_eq!(parseExcludes(&json!("not json")), json!([]));
        assert_eq!(parseExcludes(&json!("{\"a\":1}")), json!([]));
    }

    #[test]
    fn resolve_context_docs_embeds_live_source_and_passes_through() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, title, status, updated_at, created_at) VALUES ('CARD-1','First card','backlog','2026-01-01 00:00:00','2025-12-01 00:00:00')", []).unwrap();
        let docs = json!([
            { "id": 1, "body_md": "CONTEXT SOURCE — Cards:\nstale placeholder" },
            { "id": 2, "body_md": "CONTEXT SOURCE — Cards:\nstale placeholder2" },
            { "id": 3, "body_md": "Just a regular doc\nSecond line" },
        ]);
        let out = resolveContextDocs(&db, &docs, &json!([1]), &json!({}));
        let arr = out.as_array().unwrap();
        assert_eq!(arr[0]["excluded"], json!(true));
        assert!(arr[0]["body"].as_str().unwrap().contains("(this source is empty right now)"));
        assert_eq!(arr[1]["excluded"], json!(false));
        assert!(arr[1]["body"].as_str().unwrap().contains("CARD-1"));
        assert_eq!(arr[1]["source"], json!("cards"));
        assert_eq!(arr[2]["label"], json!("Just a regular doc"));
        assert_eq!(arr[2]["source"], Value::Null);
    }

    #[test]
    fn mission_value_joins_included_docs_and_caps() {
        let docs = json!([
            { "id": 1, "body": "body one", "excluded": false },
            { "id": 2, "body": "body two", "excluded": true },
        ]);
        assert_eq!(missionValue(&json!("Do the thing"), &docs), json!("Do the thing\n\n--- CONTEXT DOC #1 ---\nbody one"));
        assert_eq!(missionValue(&Value::Null, &json!([])), json!(""));
    }
}
