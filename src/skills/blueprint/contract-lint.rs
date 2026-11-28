//! contract-lint.rs — twin of skills/blueprint/contract-lint.mjs (no-db, pure). NAMES ARE CONTRACT:
//! `contractLint` (default). The deterministic well-formedness gate on the contract (pipeline v2).
// QUALITY: 80/100 — goal/acceptance/kind/must_obey/files_touched shape checks before the AI Ratify judge
use regex::Regex;
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
// JS `${v}` interpolation: absent → "undefined", null → "null", else String(v)
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}

fn parse_arr(v: &Value) -> Vec<Value> {
    if let Some(a) = v.as_array() { return a.clone(); }
    if let Some(s) = v.as_str() {
        if let Ok(x) = serde_json::from_str::<Value>(s) {
            if let Some(a) = x.as_array() { return a.clone(); }
        }
    }
    vec![]
}
fn bad_path(p: &Value) -> bool {
    static DRIVE: OnceLock<Regex> = OnceLock::new();
    let drive = DRIVE.get_or_init(|| Regex::new(r"^[A-Za-z]:[\\/]").unwrap());
    let s = str_qq(Some(p));
    s.is_empty() || s.starts_with('/') || drive.is_match(&s) || s.contains("..")
}
// c.a ?? c.b — first non-null, else Null
fn coalesce<'a>(c: &'a Value, a: &str, b: &str) -> &'a Value {
    match c.get(a) { Some(v) if !v.is_null() => v, _ => c.get(b).filter(|v| !v.is_null()).unwrap_or(&Value::Null) }
}

pub fn contractLint(input: &Value, _db: &Connection) -> Result<Value, String> {
    // input?.contract ?? input ?? {}
    let c: &Value = match input.get("contract") { Some(v) if !v.is_null() => v, _ => if input.is_null() { &Value::Null } else { input } };
    let mut issues: Vec<Value> = Vec::new();

    let goal = c.get("goal_md");
    if !truthy(goal) || str_qq(goal).trim().is_empty() {
        issues.push(json!({ "severity": "blocker", "kind": "missing_field", "ref": "goal_md", "problem": "contract.goal_md is missing or empty" }));
    }

    let acceptance = parse_arr(coalesce(c, "acceptance_json", "acceptance"));
    if acceptance.is_empty() {
        issues.push(json!({ "severity": "blocker", "kind": "no_acceptance", "ref": "acceptance", "problem": "contract.acceptance is empty — there is nothing to build or verify against" }));
    }
    let valid_kinds = ["behavior", "file-exists", "file_exists", "grep", "test"];
    for (i, ac) in acceptance.iter().enumerate() {
        let id: Value = match ac.get("id") { Some(v) if !v.is_null() => v.clone(), _ => json!(format!("#{}", i)) };
        let id_str = str_qq(Some(&id));
        if !truthy(ac.get("id")) { issues.push(json!({ "severity": "blocker", "kind": "acceptance_no_id", "ref": i.to_string(), "problem": format!("acceptance[{}] has no id", i) })); }
        if !truthy(ac.get("intent")) || str_qq(ac.get("intent")).trim().is_empty() { issues.push(json!({ "severity": "blocker", "kind": "acceptance_no_intent", "ref": id_str, "problem": format!("acceptance '{}' has no intent", id_str) })); }
        if !truthy(ac.get("done_criteria")) || str_qq(ac.get("done_criteria")).trim().is_empty() { issues.push(json!({ "severity": "blocker", "kind": "acceptance_no_done_criteria", "ref": id_str, "problem": format!("acceptance '{}' has no done_criteria", id_str) })); }
        let kind_l = str_qq(ac.get("kind")).to_lowercase();
        if !valid_kinds.contains(&kind_l.as_str()) {
            issues.push(json!({ "severity": "blocker", "kind": "acceptance_bad_kind", "ref": id_str, "problem": format!("acceptance '{}' kind '{}' is invalid (behavior | file-exists | grep | test)", id_str, tmpl(ac.get("kind"))) }));
        }
    }

    if parse_arr(coalesce(c, "must_obey_json", "must_obey")).is_empty() {
        issues.push(json!({ "severity": "warn", "kind": "no_must_obey", "ref": "must_obey", "problem": "contract.must_obey is empty (no hard laws asserted)" }));
    }

    static PLACEHOLDER: OnceLock<Regex> = OnceLock::new();
    let placeholder = PLACEHOLDER.get_or_init(|| Regex::new(r"<[A-Z_]+>").unwrap());
    for f in parse_arr(coalesce(c, "files_touched_json", "files_touched")) {
        let fs = str_qq(Some(&f));
        if placeholder.is_match(&fs) {
            issues.push(json!({ "severity": "blocker", "kind": "placeholder_path", "ref": fs.clone(), "problem": format!("files_touched '{}' is an unresolved placeholder", fs) }));
        } else if bad_path(&f) {
            issues.push(json!({ "severity": "blocker", "kind": "files_touched_bad_path", "ref": fs.clone(), "problem": format!("files_touched '{}' must be repo-relative (no absolute path, no '..')", fs) }));
        }
    }

    let passed = issues.iter().all(|i| i.get("severity").and_then(|v| v.as_str()) != Some("blocker"));
    Ok(json!({ "passed": passed, "issues": issues, "acceptance_count": acceptance.len() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Value { contractLint(&i, &Connection::open_in_memory().unwrap()).unwrap() }
    #[test]
    fn lints() {
        let clean = call(json!({ "goal_md": "add filter", "acceptance_json": "[{\"id\":\"ac1\",\"intent\":\"x\",\"done_criteria\":\"y\",\"kind\":\"behavior\"}]", "must_obey": ["law1"], "files_touched": ["src/f.ts"] }));
        assert_eq!(clean["passed"], json!(true));
        assert_eq!(clean["acceptance_count"], json!(1));
        let bad = call(json!({ "acceptance": [{ "intent": "x", "done_criteria": "y", "kind": "wat" }], "files_touched": ["/abs/path", "src/<PLACEHOLDER>.ts", "../escape"] }));
        assert_eq!(bad["passed"], json!(false));
        let kinds: Vec<&str> = bad["issues"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"missing_field") && kinds.contains(&"acceptance_no_id") && kinds.contains(&"acceptance_bad_kind"));
        assert!(kinds.contains(&"files_touched_bad_path") && kinds.contains(&"placeholder_path"));
        assert!(bad["issues"].as_array().unwrap().iter().any(|i| i["kind"] == json!("no_must_obey") && i["severity"] == json!("warn")));
    }
}
