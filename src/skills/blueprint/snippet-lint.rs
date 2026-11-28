//! snippet-lint.rs — twin of skills/blueprint/snippet-lint.mjs (no-db, pure). NAMES ARE CONTRACT:
//! `snippetLint` (default). The deterministic shape gate on the snippet swarm's output (pipeline v2).
// QUALITY: 80/100 — per-snippet shape checks (block_id/dup/code/imports/exports) + expected-block count
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

pub fn snippetLint(input: &Value, _db: &Connection) -> Result<Value, String> {
    let snippets: Vec<Value> = if let Some(a) = input.get("snippets").and_then(|v| v.as_array()) { a.clone() }
        else if truthy(input.get("snippet")) { vec![input.get("snippet").unwrap().clone()] }
        else { vec![] };
    let expected: Option<Vec<String>> = input.get("block_ids").and_then(|v| v.as_array()).map(|a| a.iter().map(js_string).collect());

    let mut issues: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for (i, s) in snippets.iter().enumerate() {
        let block_id = s.get("block_id");
        let id: Value = match block_id { Some(v) if !v.is_null() => v.clone(), _ => json!(format!("#{}", i)) };
        let id_str = js_string(&id);
        if !truthy(block_id) {
            issues.push(json!({ "severity": "blocker", "kind": "snippet_no_block_id", "ref": i.to_string(), "problem": format!("snippet[{}] has no block_id", i) }));
        } else {
            if seen.contains(&id_str) { issues.push(json!({ "severity": "blocker", "kind": "snippet_dup", "ref": id_str, "problem": format!("duplicate snippet for block '{}'", id_str) })); }
            seen.insert(id_str.clone());
        }
        let code_empty = match s.get("code") { None | Some(Value::Null) => true, Some(c) => js_string(c).trim().is_empty() };
        if code_empty {
            issues.push(json!({ "severity": "blocker", "kind": "snippet_no_code", "ref": id_str, "problem": format!("snippet '{}' has empty code", id_str) }));
        }
        if let Some(imp) = s.get("imports") { if !imp.is_null() && !imp.is_array() {
            issues.push(json!({ "severity": "blocker", "kind": "snippet_bad_imports", "ref": id_str, "problem": format!("snippet '{}' imports must be an array", id_str) }));
        }}
        if let Some(exp) = s.get("exports") { if !exp.is_null() && !exp.is_array() {
            issues.push(json!({ "severity": "blocker", "kind": "snippet_bad_exports", "ref": id_str, "problem": format!("snippet '{}' exports must be an array", id_str) }));
        }}
    }

    let mut missing = 0i64;
    if let Some(exp) = &expected {
        for bid in exp {
            if !seen.contains(bid) {
                missing += 1;
                issues.push(json!({ "severity": "blocker", "kind": "snippet_missing", "ref": bid, "problem": format!("block '{}' has no authored snippet", bid) }));
            }
        }
    }
    let passed = issues.iter().all(|i| i.get("severity").and_then(|v| v.as_str()) != Some("blocker"));
    Ok(json!({ "passed": passed, "issues": issues, "snippet_count": snippets.len(), "missing": missing }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Value { snippetLint(&i, &Connection::open_in_memory().unwrap()).unwrap() }
    #[test]
    fn lints() {
        let clean = call(json!({ "snippets": [{ "block_id": "b1", "code": "export const a = 1;", "imports": [], "exports": ["a"] }, { "block_id": "b2", "code": "x" }] }));
        assert_eq!(clean["passed"], json!(true));
        assert_eq!(clean["snippet_count"], json!(2));
        let dup = call(json!({ "snippets": [{ "block_id": "b1", "code": "x" }, { "block_id": "b1", "code": "" }] }));
        assert_eq!(dup["passed"], json!(false));
        let kinds: Vec<&str> = dup["issues"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"snippet_dup") && kinds.contains(&"snippet_no_code"));
        let missing = call(json!({ "snippets": [{ "block_id": "b1", "code": "x" }], "block_ids": ["b1", "b2"] }));
        assert_eq!(missing["missing"], json!(1));
        assert_eq!(missing["issues"][0]["kind"], json!("snippet_missing"));
        assert_eq!(missing["issues"][0]["ref"], json!("b2"));
    }
}
