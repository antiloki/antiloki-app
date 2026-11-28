//! blueprint-coverage.rs — twin of skills/blueprint/blueprint-coverage.mjs (no-db, pure). NAMES ARE
//! CONTRACT: `blueprintCoverage` (default). The deterministic acceptance↔blueprint coverage join.
// QUALITY: 80/100 — forward (every S-task maps to an existing block) + backward (no orphan blocks)
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn tmpl(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(Value::Null) => "null".into(), Some(x) => str_qq(Some(x)) } }
// cross-module: block-lint's normPath (String(p??'').trim().strip-leading-./)
fn norm_path(v: &Value) -> String { crate::blueprint_blueprint_lint::normPath(v).as_str().unwrap_or("").to_string() }
fn id_or_empty(ac: &Value) -> Value { ac.get("id").filter(|v| !v.is_null()).cloned().unwrap_or(json!("")) }

pub fn blueprintCoverage(input: &Value, _db: &Connection) -> Result<Value, String> {
    let acceptance: Vec<Value> = input.get("acceptance").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let blocks: Vec<Value> = input.get("blocks").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let mut issues: Vec<Value> = Vec::new();
    let path_set: HashSet<String> = blocks.iter().map(|b| norm_path(b.get("path").unwrap_or(&Value::Null))).filter(|s| !s.is_empty()).collect();
    let mut mapped: HashSet<String> = HashSet::new();

    // forward — every S-task maps to ≥1 existing file block
    let mut unmapped = 0i64;
    for ac in &acceptance {
        let maps = ac.get("maps_to_blocks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        if maps.is_empty() {
            unmapped += 1;
            let intent = match ac.get("intent") { Some(v) if !v.is_null() => str_qq(Some(v)), _ => String::new() };
            issues.push(json!({ "severity": "blocker", "kind": "acceptance_unmapped", "ref": id_or_empty(ac),
                "problem": format!("acceptance '{}' ({}) maps to no block — nothing in the blueprint builds it", tmpl(ac.get("id")), intent) }));
            continue;
        }
        for p in &maps {
            let np = norm_path(p);
            mapped.insert(np.clone());
            if !path_set.contains(&np) {
                issues.push(json!({ "severity": "blocker", "kind": "acceptance_maps_missing_block", "ref": id_or_empty(ac),
                    "problem": format!("acceptance '{}' maps to '{}', which is not a file block in the blueprint", tmpl(ac.get("id")), tmpl(Some(p))) }));
            }
        }
    }

    // backward — no orphan file blocks
    let mut orphans = 0i64;
    for b in &blocks {
        let np = norm_path(b.get("path").unwrap_or(&Value::Null));
        if !np.is_empty() && !mapped.contains(&np) {
            orphans += 1;
            issues.push(json!({ "severity": "blocker", "kind": "orphan_block", "ref": b.get("path").filter(|v| !v.is_null()).cloned().unwrap_or(json!("")),
                "problem": format!("file block '{}' is not referenced by any acceptance S-task — built but unasked-for", tmpl(b.get("path"))) }));
        }
    }

    let passed = issues.iter().all(|i| i.get("severity").and_then(|v| v.as_str()) != Some("blocker"));
    Ok(json!({ "passed": passed, "issues": issues, "checked": acceptance.len(), "blocks": blocks.len(), "unmapped": unmapped, "orphans": orphans }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Value { blueprintCoverage(&i, &Connection::open_in_memory().unwrap()).unwrap() }
    #[test]
    fn covers() {
        let clean = call(json!({ "acceptance": [{ "id": "a1", "maps_to_blocks": ["src/a.ts"] }, { "id": "a2", "maps_to_blocks": ["./src/b.ts"] }], "blocks": [{ "path": "src/a.ts" }, { "path": "src/b.ts" }] }));
        assert_eq!(clean["passed"], json!(true));
        assert_eq!(clean["checked"], json!(2));
        let bad = call(json!({ "acceptance": [{ "id": "a1", "intent": "do x" }, { "id": "a2", "maps_to_blocks": ["src/missing.ts"] }], "blocks": [{ "path": "src/orphan.ts" }] }));
        assert_eq!(bad["passed"], json!(false));
        assert_eq!(bad["unmapped"], json!(1));
        assert_eq!(bad["orphans"], json!(1));
        let kinds: Vec<&str> = bad["issues"].as_array().unwrap().iter().map(|i| i["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"acceptance_unmapped") && kinds.contains(&"acceptance_maps_missing_block") && kinds.contains(&"orphan_block"));
    }
}
