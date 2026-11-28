//! code-checks.rs — twin of skills/blueprint/_helpers/code-checks.mjs, shared DETERMINISTIC
//! check primitives for the blueprint linters. Pure, no-db helper module. NAMES ARE CONTRACT:
//! isDegenerateSource · extractRawCode · splitCodeArtifact · parseDecls · gradeNames · isStubBody.
//!
//! `parseDecls` is the JS module's only async fn — per the async-lane rule it stays an ordinary
//! sync `pub fn` here. It calls the already-converted `codebase_ast_parse::astParse` twin (a
//! standard db-lane fn) with a throwaway in-memory `Connection`: the JS source passes an empty
//! `{}` context object too, and `astParse` never reads its `_db` parameter, so the connection is
//! never touched — only present to satisfy the twin's signature.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
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
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

fn decl_keyword_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| {
        regex::Regex::new(r"\b(function|class|const|let|var|def|fn|func|type|interface|export|import|struct)\b").unwrap()
    })
}

#[allow(non_snake_case)]
pub fn isDegenerateSource(source: &Value) -> Value {
    let t = str_qq(Some(source));
    let t = t.trim();
    json!(t.chars().count() < 80 && !decl_keyword_re().is_match(t))
}

fn fence_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"(?s)^```[a-zA-Z]*\r?\n(.*?)\r?\n?```$").unwrap())
}

#[allow(non_snake_case)]
pub fn extractRawCode(text: &Value) -> Value {
    json!(extract_raw_code_str(text))
}

fn extract_raw_code_str(text: &Value) -> String {
    let mut t = str_qq(Some(text)).trim().to_string();
    if let Some(caps) = fence_re().captures(&t) {
        t = caps.get(1).map(|m| m.as_str()).unwrap_or("").trim().to_string();
    }
    if t.chars().count() > 2 && t.starts_with('"') && t.ends_with('"') {
        if let Ok(Value::String(inner)) = serde_json::from_str::<Value>(&t) {
            let inner_trimmed = inner.trim();
            if !inner_trimmed.is_empty() {
                t = inner_trimmed.to_string();
            }
        }
    }
    t
}

fn opens_unclosed_brace_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"\bimport\b[^}]*\{[^}]*$").unwrap())
}
fn import_line_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"^\s*import\b").unwrap())
}
fn export_name_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"\bexport\s+(?:default\s+)?(?:const|function|class|let|var)?\s*([A-Za-z_$][\w$]*)").unwrap())
}

#[allow(non_snake_case)]
pub fn splitCodeArtifact(rawText: &Value) -> Value {
    let src = extract_raw_code_str(rawText);
    let mut imports: Vec<Value> = Vec::new();
    let mut body: Vec<&str> = Vec::new();
    for ln in src.split('\n') {
        let opens_unclosed_brace = opens_unclosed_brace_re().is_match(ln);
        let body_has_content = body.iter().any(|b| !b.trim().is_empty());
        if import_line_re().is_match(ln) && !opens_unclosed_brace && !body_has_content {
            imports.push(json!(ln.trim()));
        } else {
            body.push(ln);
        }
    }
    let joined = body.join("\n");
    // JS: .replace(/^\s*\n/, '') — \s* is greedy and \s includes \n, so this strips the WHOLE
    // leading run of blank lines (backtracks to the last \n in that run), not just one line.
    let leading_blank_re = {
        static R: OnceLock<regex::Regex> = OnceLock::new();
        R.get_or_init(|| regex::Regex::new(r"^\s*\n").unwrap())
    };
    let joined = leading_blank_re.replacen(&joined, 1, "");
    let code = joined.trim_end_matches(|c: char| c.is_whitespace()).to_string();

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut exports: Vec<Value> = Vec::new();
    for caps in export_name_re().captures_iter(&code) {
        let name = caps.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
        if name.is_empty() || name == "default" {
            continue;
        }
        if seen.insert(name.clone()) {
            exports.push(json!(name));
        }
    }

    json!({ "code": code, "imports": imports, "exports": exports })
}

#[allow(non_snake_case)]
pub fn parseDecls(path: &Value, source: &Value) -> Value {
    let input = json!({ "path": path, "source": source });
    let db = Connection::open_in_memory().unwrap();
    match crate::codebase_ast_parse::astParse(&input, &db) {
        Ok(r) => {
            let nodes = nz(r.get("nodes")).cloned().unwrap_or_else(|| json!([]));
            let decls: Vec<Value> = nodes
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter(|n| n.get("type").and_then(|t| t.as_str()) == Some("function"))
                        .map(|n| {
                            json!({
                                "name": str_qq(n.get("name")),
                                "signature": nz(n.get("signature")).cloned().unwrap_or_else(|| json!("")),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            if truthy(r.get("has_error")) {
                json!({
                    "parses": false,
                    "skipped": false,
                    "decls": decls,
                    "error": "tree-sitter reported syntax ERROR node(s) — the composed file is not valid code",
                    "loc": Value::Null,
                })
            } else {
                json!({ "parses": true, "skipped": false, "decls": decls })
            }
        }
        Err(e) => {
            if e.contains("language adapter") {
                json!({ "parses": true, "skipped": true, "decls": [] })
            } else {
                let msg: String = e.chars().take(200).collect();
                json!({ "parses": false, "skipped": false, "decls": [], "error": msg, "loc": Value::Null })
            }
        }
    }
}

fn valid_ident_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*$").unwrap())
}

#[allow(non_snake_case)]
pub fn gradeNames(got_names: &Value, expected_names: &Value) -> Value {
    let got_vec: Vec<String> = got_names
        .as_array()
        .map(|a| a.iter().map(|v| str_qq(Some(v))).collect())
        .unwrap_or_default();

    let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for n in &got_vec {
        if !counts.contains_key(n) {
            order.push(n.clone());
        }
        *counts.entry(n.clone()).or_insert(0) += 1;
    }
    let duplicate: Vec<Value> = order
        .iter()
        .filter(|n| !n.is_empty() && counts[*n] > 1)
        .map(|n| json!({ "name": n, "count": counts[n] }))
        .collect();

    let mut got_set_order: Vec<String> = Vec::new();
    let mut got_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    for n in &got_vec {
        if !n.is_empty() && got_set.insert(n.clone()) {
            got_set_order.push(n.clone());
        }
    }

    let exp_arr = nz(Some(expected_names)).and_then(|v| v.as_array());
    let (missing, extra) = match exp_arr {
        Some(exp) => {
            let mut exp_set_order: Vec<String> = Vec::new();
            let mut exp_set: std::collections::HashSet<String> = std::collections::HashSet::new();
            for v in exp {
                let n = str_qq(Some(v));
                if !n.is_empty() && exp_set.insert(n.clone()) {
                    exp_set_order.push(n);
                }
            }
            let missing: Vec<Value> = exp_set_order
                .iter()
                .filter(|n| !got_set.contains(*n))
                .map(|n| json!(n))
                .collect();
            let extra: Vec<Value> = got_set_order
                .iter()
                .filter(|n| !exp_set.contains(*n))
                .map(|n| json!(n))
                .collect();
            (missing, extra)
        }
        None => (Vec::new(), Vec::new()),
    };

    let bad_identifier: Vec<Value> = got_set_order
        .iter()
        .filter(|n| !valid_ident_re().is_match(n))
        .map(|n| json!(n))
        .collect();

    json!({ "duplicate": duplicate, "missing": missing, "extra": extra, "badIdentifier": bad_identifier })
}

fn stub_body_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| {
        regex::Regex::new(r#"(?i)(//\s*TODO\b|/\*\s*TODO\b|throw new \w*Error\(\s*[`'"][^`'"]*\b(TODO|not[ _-]?implemented|unimplemented|stub)\b)"#).unwrap()
    })
}

#[allow(non_snake_case)]
pub fn isStubBody(source: &Value) -> Value {
    json!(stub_body_re().is_match(&str_qq(Some(source))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degenerate_source_short_no_keyword() {
        assert_eq!(isDegenerateSource(&json!("stub_source")), json!(true));
        assert_eq!(isDegenerateSource(&json!("function foo() {}")), json!(false));
        assert_eq!(isDegenerateSource(&Value::Null), json!(true));
    }

    #[test]
    fn extract_raw_code_strips_fence_and_double_json() {
        assert_eq!(extractRawCode(&json!("```js\nconst x = 1;\n```")), json!("const x = 1;"));
        assert_eq!(extractRawCode(&json!("\"const x = 1;\"")), json!("const x = 1;"));
        assert_eq!(extractRawCode(&json!("plain code")), json!("plain code"));
    }

    #[test]
    fn split_code_artifact_hoists_leading_imports_and_collects_exports() {
        let src = "import { a } from 'x';\nimport b from 'y';\n\nexport function foo() {}\nexport const bar = 1;\n";
        let r = splitCodeArtifact(&json!(src));
        assert_eq!(r["imports"], json!(["import { a } from 'x';", "import b from 'y';"]));
        assert_eq!(r["exports"], json!(["foo", "bar"]));
        assert!(r["code"].as_str().unwrap().starts_with("export function foo"));
    }

    #[test]
    fn split_code_artifact_keeps_multiline_import_in_body() {
        let src = "import {\n  a,\n  b\n} from 'x';\nexport const c = 1;\n";
        let r = splitCodeArtifact(&json!(src));
        assert_eq!(r["imports"], json!([]));
        assert!(r["code"].as_str().unwrap().contains("import {"));
    }

    #[test]
    fn parse_decls_js_function() {
        let r = parseDecls(&json!("foo.js"), &json!("function add(a, b) {\n  return a + b;\n}\n"));
        assert_eq!(r["parses"], json!(true));
        assert_eq!(r["skipped"], json!(false));
        assert_eq!(r["decls"][0]["name"], json!("add"));
    }

    #[test]
    fn parse_decls_unknown_extension_is_skipped_not_a_failure() {
        let r = parseDecls(&json!("readme.xyz"), &json!("hi"));
        assert_eq!(r["parses"], json!(true));
        assert_eq!(r["skipped"], json!(true));
        assert_eq!(r["decls"], json!([]));
    }

    #[test]
    fn grade_names_flags_duplicate_missing_extra_bad_identifier() {
        let r = gradeNames(&json!(["foo", "foo", "1bad"]), &json!(["foo", "bar"]));
        assert_eq!(r["duplicate"], json!([{ "name": "foo", "count": 2 }]));
        assert_eq!(r["missing"], json!(["bar"]));
        assert_eq!(r["extra"], json!(["1bad"]));
        assert_eq!(r["badIdentifier"], json!(["1bad"]));
    }

    #[test]
    fn grade_names_null_expected_skips_missing_extra() {
        let r = gradeNames(&json!(["a"]), &Value::Null);
        assert_eq!(r["missing"], json!([]));
        assert_eq!(r["extra"], json!([]));
    }

    #[test]
    fn is_stub_body_detects_todo_and_stub_throw() {
        assert_eq!(isStubBody(&json!("// TODO: implement")), json!(true));
        assert_eq!(isStubBody(&json!("throw new Error('not implemented')")), json!(true));
        assert_eq!(isStubBody(&json!("return 1;")), json!(false));
    }
}
