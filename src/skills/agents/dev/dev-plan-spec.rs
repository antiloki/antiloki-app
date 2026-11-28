//! dev-plan-spec.rs — twin of skills/agents/dev/dev-plan-spec.mjs (no-db, pure). NAMES ARE CONTRACT:
//! `devPlanSpecValidate` (default) · validateMicrotask · validatePatch. Microtask-plan JSON validator.
// QUALITY: 81/100 — per-microtask/per-patch shape validation, dup-id + depends_on refs + budget-sum
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

const VALID_DIFF_SHAPES: [&str; 4] = ["small_edit", "single_file", "multi_file", "new_module"];
const VALID_PATCH_OPS: [&str; 3] = ["create", "insert_after", "replace_section"];

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
fn nonempty_string(v: Option<&Value>) -> bool { v.and_then(|x| x.as_str()).map_or(false, |s| !s.is_empty()) }
fn is_string(v: Option<&Value>) -> bool { v.map_or(false, |x| x.is_string()) }
fn is_number(v: Option<&Value>) -> bool { v.map_or(false, |x| x.is_number()) }
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN, Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}
fn obj_or_array(v: &Value) -> bool { v.is_object() || v.is_array() } // JS typeof === 'object' (arrays too)

fn validate_patch(p: &Value, mi: &Value, pi: &Value, files_touched: &Value) -> Vec<String> {
    let at = format!("microtasks[{}].patches[{}]", str_qq(Some(mi)), str_qq(Some(pi)));
    let mut errors: Vec<String> = Vec::new();
    if !truthy(Some(p)) || !obj_or_array(p) { errors.push(format!("{} must be an object", at)); return errors; }
    let file = p.get("file");
    if !nonempty_string(file) {
        errors.push(format!("{}.file must be a non-empty string", at));
    } else if let Some(ft) = files_touched.as_array() {
        if !ft.iter().any(|x| Some(x) == file) { errors.push(format!("{}.file '{}' is not in the microtask's files_touched", at, str_qq(file))); }
    }
    let op = p.get("op").and_then(|v| v.as_str());
    if !VALID_PATCH_OPS.contains(&op.unwrap_or("")) { errors.push(format!("{}.op must be one of: {}", at, VALID_PATCH_OPS.join(", "))); }
    if !is_string(p.get("replacement")) { errors.push(format!("{}.replacement must be a string", at)); }
    if op == Some("insert_after") {
        if !nonempty_string(p.get("anchor_before")) { errors.push(format!("{}.anchor_before is required (non-empty string) for op=insert_after", at)); }
    } else if op == Some("replace_section") {
        if !nonempty_string(p.get("anchor_before")) { errors.push(format!("{}.anchor_before is required (non-empty string) for op=replace_section", at)); }
        if !nonempty_string(p.get("anchor_after")) { errors.push(format!("{}.anchor_after is required (non-empty string) for op=replace_section", at)); }
    }
    errors
}

fn validate_microtask(m: &Value, index: &Value) -> Vec<String> {
    let idx = str_qq(Some(index));
    let mut errors: Vec<String> = Vec::new();
    if !truthy(Some(m)) || !obj_or_array(m) { errors.push(format!("microtasks[{}] must be an object", idx)); return errors; }
    if !nonempty_string(m.get("microtask_id")) { errors.push(format!("microtasks[{}].microtask_id must be non-empty string", idx)); }
    if !nonempty_string(m.get("intent")) { errors.push(format!("microtasks[{}].intent must be non-empty string", idx)); }
    match m.get("files_touched").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => { if !a.iter().all(|f| nonempty_string(Some(f))) { errors.push(format!("microtasks[{}].files_touched must contain non-empty strings only", idx)); } }
        _ => errors.push(format!("microtasks[{}].files_touched must be a non-empty array", idx)),
    }
    if !m.get("acceptance").and_then(|v| v.as_array()).map_or(false, |a| !a.is_empty()) { errors.push(format!("microtasks[{}].acceptance must be a non-empty array", idx)); }
    if !is_number(m.get("max_tokens")) || js_number(m.get("max_tokens")) <= 0.0 { errors.push(format!("microtasks[{}].max_tokens must be a positive number", idx)); }
    if !is_number(m.get("max_cost_usd")) || js_number(m.get("max_cost_usd")) < 0.0 { errors.push(format!("microtasks[{}].max_cost_usd must be a non-negative number", idx)); }
    if !VALID_DIFF_SHAPES.contains(&m.get("expected_diff_shape").and_then(|v| v.as_str()).unwrap_or("")) { errors.push(format!("microtasks[{}].expected_diff_shape must be one of: {}", idx, VALID_DIFF_SHAPES.join(", "))); }
    if let Some(d) = m.get("depends_on") { if !d.is_array() { errors.push(format!("microtasks[{}].depends_on must be an array when present", idx)); } }
    if let Some(pt) = m.get("patches") {
        if !pt.is_array() { errors.push(format!("microtasks[{}].patches must be an array when present", idx)); }
        else { for (pi, patch) in pt.as_array().unwrap().iter().enumerate() { errors.extend(validate_patch(patch, index, &json!(pi), m.get("files_touched").unwrap_or(&Value::Null))); } }
    }
    errors
}

pub fn validatePatch(p: &Value, mi: &Value, pi: &Value, files_touched: &Value) -> Value { json!(validate_patch(p, mi, pi, files_touched)) }
pub fn validateMicrotask(m: &Value, index: &Value) -> Value { json!(validate_microtask(m, index)) }

pub fn devPlanSpecValidate(input: &Value, _db: &Connection) -> Result<Value, String> {
    let plan = match input.get("plan") { Some(p) if truthy(Some(p)) && obj_or_array(p) => p, _ => return Err("dev-plan-spec: 'plan' is required".into()) };
    let mut errors: Vec<String> = Vec::new();
    if !nonempty_string(plan.get("plan_id")) { errors.push("plan_id must be non-empty string".into()); }
    if !nonempty_string(plan.get("card_slug")) { errors.push("card_slug must be non-empty string".into()); }
    let microtasks = plan.get("microtasks").and_then(|v| v.as_array());
    if microtasks.map_or(true, |a| a.is_empty()) { errors.push("microtasks must be a non-empty array".into()); }
    if !is_number(plan.get("total_max_cost_usd")) || js_number(plan.get("total_max_cost_usd")) < 0.0 { errors.push("total_max_cost_usd must be a non-negative number".into()); }

    if let Some(mts) = plan.get("microtasks").and_then(|v| v.as_array()) {
        let mut ids: HashSet<String> = HashSet::new();
        for (i, m) in mts.iter().enumerate() {
            errors.extend(validate_microtask(m, &json!(i)));
            if truthy(m.get("microtask_id")) {
                let mid = str_qq(m.get("microtask_id"));
                if ids.contains(&mid) { errors.push(format!("duplicate microtask_id '{}' at index {}", mid, i)); }
                ids.insert(mid);
            }
        }
        for (i, m) in mts.iter().enumerate() {
            for dep in m.get("depends_on").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
                if !ids.contains(&str_qq(Some(&dep))) { errors.push(format!("microtasks[{}].depends_on references unknown '{}'", i, str_qq(Some(&dep)))); }
            }
        }
        let total = js_number(plan.get("total_max_cost_usd"));
        let sum: f64 = mts.iter().map(|m| { let v = js_number(m.get("max_cost_usd")); if v.is_nan() { 0.0 } else { v } }).sum();
        if sum > total + 1e-6 { errors.push(format!("sum of microtask max_cost_usd ({:.4}) exceeds plan.total_max_cost_usd ({})", sum, str_qq(plan.get("total_max_cost_usd")))); }
    }

    if !errors.is_empty() { return Err(format!("plan validation failed: {}", errors.join("; "))); }
    Ok(json!({ "ok": true, "microtask_count": microtasks.map_or(0, |a| a.len()), "total_max_cost_usd": plan.get("total_max_cost_usd").cloned().unwrap_or(Value::Null) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates() {
        let m = json!({ "microtask_id": "m1", "intent": "do", "files_touched": ["a.ts"], "acceptance": ["x"], "max_tokens": 512, "max_cost_usd": 0.05, "expected_diff_shape": "small_edit" });
        assert_eq!(validateMicrotask(&m, &json!(0)), json!([]));
        let bad = validateMicrotask(&json!({ "intent": "", "files_touched": [], "max_tokens": 0, "expected_diff_shape": "wat" }), &json!(1));
        assert!(bad.as_array().unwrap().len() >= 5);
        assert_eq!(validatePatch(&json!({ "file": "a.ts", "op": "insert_after", "replacement": "code" }), &json!(0), &json!(0), &json!(["a.ts"])), json!(["microtasks[0].patches[0].anchor_before is required (non-empty string) for op=insert_after"]));
        let plan = json!({ "plan": { "plan_id": "p1", "card_slug": "C-1", "total_max_cost_usd": 0.1, "microtasks": [m] } });
        assert_eq!(devPlanSpecValidate(&plan, &Connection::open_in_memory().unwrap()).unwrap(), json!({ "ok": true, "microtask_count": 1, "total_max_cost_usd": 0.1 }));
    }
}
