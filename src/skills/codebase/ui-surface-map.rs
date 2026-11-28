//! ui-surface-map.rs — twin of skills/codebase/ui-surface-map.mjs (db READ lane). NAMES ARE
//! CONTRACT: `uiSurfaceMap` (named default). Screens (route+controls) + flows for the test generators.
// QUALITY: 80/100 — control-by-file grouping (preserve_order), standalone screens, safeParse hops
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS: safeParse(s) = try JSON.parse(s ?? '[]') catch [] — null → [], valid → parsed, bad → [].
fn safe_len(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).ok().and_then(|x| x.as_array().map(|a| a.len() as i64)).unwrap_or(0),
        _ => 0, // null column or absent → '[]' → length 0
    }
}
fn g(o: &Map<String, Value>, k: &str) -> Value { o.get(k).cloned().unwrap_or(Value::Null) }

pub fn uiSurfaceMap(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
    let eps = crate::db_json::query_rows(db, "SELECT kind, subtype, path, label, line, framework, http_method FROM code_entry_points WHERE origin = ?", &[json!(origin)])
        .ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
    let stories = crate::db_json::query_rows(db, "SELECT id, name, direction, hops_json, linked_test FROM code_data_stories WHERE origin = ?", &[json!(origin)])
        .ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();

    let is = |e: &Value, k: &str, v: &str| e.get(k).and_then(|x| x.as_str()) == Some(v);
    let routes: Vec<&Value> = eps.iter().filter(|e| is(e, "kind", "route") || is(e, "subtype", "page-route")).collect();
    let controls: Vec<&Value> = eps.iter().filter(|e| is(e, "kind", "ui") && !is(e, "subtype", "page-route")).collect();

    // group controls by file
    let mut ctrl_by_file: Map<String, Value> = Map::new();
    for c in &controls {
        let o = c.as_object().unwrap();
        let path = o.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        ctrl_by_file.entry(path).or_insert_with(|| json!([])).as_array_mut().unwrap()
            .push(json!({ "kind": g(o, "subtype"), "label": g(o, "label"), "line": g(o, "line") }));
    }

    let mut screens: Vec<Value> = Vec::new();
    let mut routed_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    for r in &routes {
        let o = r.as_object().unwrap();
        let path = o.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        routed_paths.insert(path.clone());
        let ctrls = ctrl_by_file.get(&path).cloned().unwrap_or(json!([]));
        screens.push(json!({ "route": g(o, "label"), "path": o.get("path").cloned().unwrap_or(Value::Null), "framework": g(o, "framework"), "controls": ctrls }));
    }
    // files with controls but no route → standalone screens (Object.entries order = insertion order)
    for (path, cs) in ctrl_by_file.iter() {
        if !routed_paths.contains(path) {
            screens.push(json!({ "route": Value::Null, "path": path, "framework": Value::Null, "controls": cs }));
        }
    }

    let flows: Vec<Value> = stories.iter().map(|s| {
        let o = s.as_object().unwrap();
        json!({ "id": g(o, "id"), "name": g(o, "name"), "direction": g(o, "direction"), "hops": safe_len(o.get("hops_json")), "linked_test": g(o, "linked_test") })
    }).collect();

    let forms = controls.iter().filter(|c| is(c, "subtype", "form-submit")).count();
    Ok(json!({
        "ok": true, "origin": origin, "screens": screens, "flows": flows,
        "summary": { "screens": screens.len(), "controls": controls.len(), "forms": forms, "flows": flows.len() }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_screens_and_flows() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_entry_points (origin TEXT, kind TEXT, subtype TEXT, path TEXT, label TEXT, line INTEGER, framework TEXT, http_method TEXT); \
          CREATE TABLE code_data_stories (origin TEXT, id INTEGER, name TEXT, direction TEXT, hops_json TEXT, linked_test TEXT); \
          INSERT INTO code_entry_points (origin,kind,subtype,path,label,line) VALUES ('atlas','route','page-route','routes/foo.tsx','/foo',1),('atlas','ui','click','routes/foo.tsx','Submit',5),('atlas','ui','form-submit','routes/foo.tsx','loginForm',7),('atlas','ui','click','comp/bar.tsx','orphan',2); \
          INSERT INTO code_data_stories VALUES ('atlas',1,'search','forward','[{\"node_id\":\"a\"},{\"node_id\":\"b\"}]',null);").unwrap();
        let r = uiSurfaceMap(&json!({}), &c).unwrap();
        let foo = r["screens"].as_array().unwrap().iter().find(|s| s["route"] == json!("/foo")).unwrap();
        assert_eq!(foo["controls"].as_array().unwrap().len(), 2);
        assert!(r["screens"].as_array().unwrap().iter().any(|s| s["path"] == json!("comp/bar.tsx") && s["route"] == Value::Null));
        assert_eq!(r["flows"][0]["hops"], json!(2));
        assert_eq!(r["summary"]["forms"], json!(1));
    }
}
