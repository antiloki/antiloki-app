//! screening-gate.rs — twin of skills/blueprint/screening-gate.mjs (no-db helper module). NAMES ARE
//! CONTRACT: `screeningGate` + `parseLoose` + `pickField` (named). Deterministic PL-screening gate.
// QUALITY: 80/100 — acceptance validation, loose fenced-JSON parse (regex), output_kv field pick
use serde_json::{json, Value};
use std::sync::OnceLock;
use regex::Regex;

fn js_str(v: &Value) -> String { match v { Value::String(s) => s.clone(), Value::Null => "null".into(), o => o.to_string() } }
fn truthy(v: &Value) -> bool {
    match v { Value::Bool(b) => *b, Value::Null => false, Value::String(s) => !s.is_empty(), Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false), _ => true }
}
const VALID_KINDS: [&str; 5] = ["behavior", "file-exists", "file_exists", "grep", "test"];

pub fn screeningGate(screening: &Value) -> Value {
    let mut issues: Vec<Value> = Vec::new();
    let acc = screening.get("acceptance").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if acc.is_empty() {
        issues.push(json!({ "severity": "blocker", "kind": "no_acceptance", "ref": "", "problem": "screening has no acceptance S-tasks" }));
    }
    for (i, ac) in acc.iter().enumerate() {
        let id = match ac.get("id").filter(|v| !v.is_null()) { Some(v) => js_str(v), None => format!("#{i}") };
        let field_blank = |k: &str| ac.get(k).map(|v| !(truthy(v) && !js_str(v).trim().is_empty())).unwrap_or(true);
        if field_blank("intent") { issues.push(json!({ "severity": "blocker", "kind": "no_intent", "ref": id, "problem": format!("S-task '{id}' has no intent") })); }
        if field_blank("done_criteria") { issues.push(json!({ "severity": "blocker", "kind": "no_done_criteria", "ref": id, "problem": format!("S-task '{id}' has no done_criteria") })); }
        let kind = ac.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
        if !VALID_KINDS.contains(&kind.as_str()) {
            let raw = match ac.get("kind") { None => "undefined".to_string(), Some(v) => js_str(v) };
            issues.push(json!({ "severity": "blocker", "kind": "bad_kind", "ref": id, "problem": format!("S-task '{id}' kind '{raw}' invalid") }));
        }
    }
    let passed = issues.iter().all(|i| i.get("severity") != Some(&json!("blocker")));
    json!({ "passed": passed, "issues": issues, "acceptance_count": acc.len() })
}

pub fn parseLoose(s: &Value) -> Value {
    if s.is_null() { return Value::Null; }
    let str_ = js_str(s);
    static FENCE: OnceLock<Regex> = OnceLock::new();
    static BRACE: OnceLock<Regex> = OnceLock::new();
    let fence = FENCE.get_or_init(|| Regex::new(r"(?is)```(?:json)?\s*(.*?)```").unwrap());
    let brace = BRACE.get_or_init(|| Regex::new(r"(?s)\{.*\}").unwrap());
    let fenced = fence.captures(&str_).and_then(|c| c.get(1)).map(|m| m.as_str().to_string());
    let braced = brace.find(&str_).map(|m| m.as_str().to_string());
    for c in [fenced, Some(str_.clone()), braced].into_iter().flatten() {
        if let Ok(v) = serde_json::from_str::<Value>(c.trim()) { return v; }
    }
    Value::Null
}

pub fn pickField(r: &Value, field: &Value) -> Value {
    let fld = field.as_str().unwrap_or("");
    r.get("output_kv").and_then(|v| v.as_array()).and_then(|arr| {
        arr.iter().find(|o| o.get("field_name").and_then(|v| v.as_str()) == Some(fld)
            && o.get("parent_output_id").map(|v| v.is_null()).unwrap_or(true))
            .and_then(|o| o.get("value_text").cloned())
    }).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gate_parse_pick() {
        let good = json!({ "acceptance": [{ "id": "a1", "intent": "do x", "done_criteria": "x works", "kind": "behavior" }] });
        assert_eq!(screeningGate(&good)["passed"], json!(true));
        let bad = json!({ "acceptance": [{ "id": "a1", "intent": "", "kind": "nope" }] });
        assert_eq!(screeningGate(&bad)["passed"], json!(false));
        assert_eq!(screeningGate(&json!({}))["acceptance_count"], json!(0));
        assert_eq!(parseLoose(&json!("prefix ```json\n{\"a\":1}\n``` suffix")), json!({ "a": 1 }));
        assert_eq!(parseLoose(&json!("not json")), Value::Null);
        let r = json!({ "output_kv": [{ "field_name": "verdict", "parent_output_id": null, "value_text": "pass" }] });
        assert_eq!(pickField(&r, &json!("verdict")), json!("pass"));
        assert_eq!(pickField(&r, &json!("missing")), Value::Null);
    }
}
