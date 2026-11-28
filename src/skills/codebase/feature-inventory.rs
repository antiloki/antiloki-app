//! feature-inventory.rs — twin of skills/codebase/feature-inventory.mjs (db READ lane). NAMES ARE
//! CONTRACT: `featureInventory` (named default). Features DERIVED from the atlas (UI/non-UI/gaps).
// QUALITY: 79/100 — page-route↔story match (hop-walk or name-token overlap), data grouping, gaps
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

fn js_str(v: &Value) -> String { match v { Value::String(s) => s.clone(), Value::Null => String::new(), o => o.to_string() } }
fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase().split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit()))
        .filter(|t| t.chars().count() > 2).map(|t| t.to_string()).collect()
}

struct Story { id: Value, name: Value, linked_test: Value, hop_paths: Vec<String>, toks: HashSet<String> }

pub fn featureInventory(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
    let eps = crate::db_json::query_rows(db, "SELECT kind, subtype, path, label, http_method, detail FROM code_entry_points WHERE origin = ?", &[json!(origin)])
        .ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
    let stories = crate::db_json::query_rows(db, "SELECT id, name, description, hops_json, linked_test FROM code_data_stories WHERE origin = ?", &[json!(origin)])
        .ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();

    let parsed: Vec<Story> = stories.iter().map(|s| {
        let hops: Vec<Value> = s.get("hops_json").and_then(|v| v.as_str()).and_then(|x| serde_json::from_str::<Value>(x).ok()).and_then(|v| v.as_array().cloned()).unwrap_or_default();
        let mut hop_paths: Vec<String> = Vec::new();
        for h in &hops {
            let p = js_str(h.get("path").filter(|v| !v.is_null()).or_else(|| h.get("file").filter(|v| !v.is_null())).unwrap_or(&Value::Null));
            if !p.is_empty() && !hop_paths.contains(&p) { hop_paths.push(p); }
        }
        let toks: HashSet<String> = tokens(&format!("{} {}", js_str(s.get("name").unwrap_or(&Value::Null)), js_str(s.get("description").unwrap_or(&Value::Null)))).into_iter().collect();
        Story { id: s.get("id").cloned().unwrap_or(Value::Null), name: s.get("name").cloned().unwrap_or(Value::Null), linked_test: s.get("linked_test").cloned().unwrap_or(Value::Null), hop_paths, toks }
    }).collect();

    // UI: page-routes; a story matches by hop-walk of its file OR name-token overlap.
    let mut claimed: HashSet<String> = HashSet::new();
    let sub_is = |e: &Value, v: &str| e.get("subtype").and_then(|x| x.as_str()) == Some(v);
    let mut pages: Vec<Value> = Vec::new();
    for e in eps.iter().filter(|e| sub_is(e, "page-route")) {
        let epath = js_str(e.get("path").unwrap_or(&Value::Null));
        let label = e.get("label").cloned().unwrap_or(Value::Null);
        let lab_toks = tokens(&format!("{} {}", js_str(&label), epath));
        let matched: Vec<&Story> = parsed.iter().filter(|s| s.hop_paths.contains(&epath) || lab_toks.iter().any(|t| s.toks.contains(t))).collect();
        for s in &matched { claimed.insert(js_str(&s.id)); }
        pages.push(json!({ "key": format!("page:{}", js_str(&label)), "name": label, "kind": "ui", "source": "page-route", "path": e.get("path").cloned().unwrap_or(Value::Null),
            "stories": matched.iter().map(|s| json!({ "id": s.id, "name": s.name, "linked_test": s.linked_test })).collect::<Vec<_>>() }));
    }
    let mut ui: Vec<Value> = pages.iter().filter(|f| !f["stories"].as_array().unwrap().is_empty()).cloned().collect();
    for s in &parsed {
        if !claimed.contains(&js_str(&s.id)) {
            ui.push(json!({ "key": format!("story:{}", js_str(&s.id)), "name": s.name, "kind": "ui", "source": "data-story",
                "path": s.hop_paths.first().map(|p| json!(p)).unwrap_or(Value::Null),
                "stories": [json!({ "id": s.id, "name": s.name, "linked_test": s.linked_test })] }));
        }
    }

    let mut nonui: Vec<Value> = Vec::new();
    for e in eps.iter().filter(|e| sub_is(e, "http-route")) {
        let method = e.get("http_method").and_then(|v| v.as_str()).unwrap_or("");
        let label = js_str(e.get("label").unwrap_or(&Value::Null));
        nonui.push(json!({ "key": format!("http:{} {}", method, label), "name": format!("{} {}", method, label).trim(),
            "kind": "nonui", "source": "http-route", "path": e.get("path").cloned().unwrap_or(Value::Null), "detail": e.get("detail").cloned().unwrap_or(Value::Null) }));
    }
    // data touchpoints grouped by table label (insertion order), ops sorted
    let mut order: Vec<String> = Vec::new();
    let mut by_table: std::collections::HashMap<String, (Value, Value, HashSet<String>)> = std::collections::HashMap::new();
    for e in eps.iter().filter(|e| e.get("kind").and_then(|v| v.as_str()) == Some("data")) {
        let label = js_str(e.get("label").unwrap_or(&Value::Null));
        if !by_table.contains_key(&label) { order.push(label.clone()); by_table.insert(label.clone(), (e.get("label").cloned().unwrap_or(Value::Null), e.get("path").cloned().unwrap_or(Value::Null), HashSet::new())); }
        by_table.get_mut(&label).unwrap().2.insert(js_str(e.get("subtype").unwrap_or(&Value::Null)));
    }
    for label in &order {
        let (name, path, ops) = &by_table[label];
        let mut ops_v: Vec<String> = ops.iter().cloned().collect(); ops_v.sort();
        nonui.push(json!({ "key": format!("data:{label}"), "name": name, "kind": "nonui", "source": "data", "path": path, "ops": ops_v }));
    }

    let gaps: Vec<Value> = pages.iter().filter(|f| f["stories"].as_array().unwrap().is_empty()).map(|f| {
        let mut o = f.as_object().unwrap().clone(); o.insert("kind".into(), json!("gap")); Value::Object(o)
    }).collect();

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("origin".into(), json!(origin));
    out.insert("ui".into(), json!(ui));
    out.insert("nonui".into(), json!(nonui));
    out.insert("gaps".into(), json!(gaps));
    out.insert("sources".into(), json!({ "entry_points": eps.len(), "stories": stories.len() }));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inventories() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_entry_points (id INTEGER PRIMARY KEY, origin TEXT, kind TEXT, subtype TEXT, path TEXT, label TEXT, http_method TEXT, detail TEXT); \
          CREATE TABLE code_data_stories (id INTEGER PRIMARY KEY, origin TEXT, name TEXT, description TEXT, hops_json TEXT, linked_test TEXT); \
          INSERT INTO code_entry_points (origin,kind,subtype,path,label,http_method) VALUES ('atlas','ui','page-route','src/pages/checkout.jsx','/checkout',NULL),('atlas','ui','page-route','src/pages/orders.jsx','/orders/:id',NULL),('atlas','route','http-route','src/api/checkout.js','/api/checkout','POST'),('atlas','data','db-read','src/db/orders.js','orders',NULL),('atlas','data','db-write','src/db/orders.js','orders',NULL); \
          INSERT INTO code_data_stories (origin,name,description,hops_json,linked_test) VALUES ('atlas','checkout happy path','cart to paid','[{\"path\":\"src/pages/checkout.jsx\"}]','e2e/checkout.test.js');").unwrap();
        let r = featureInventory(&json!({}), &c).unwrap();
        assert!(r["ui"].as_array().unwrap().iter().any(|f| f["name"] == json!("/checkout")));
        assert!(r["nonui"].as_array().unwrap().iter().any(|f| f["name"] == json!("POST /api/checkout")));
        let orders = r["nonui"].as_array().unwrap().iter().find(|f| f["source"] == json!("data")).unwrap();
        assert_eq!(orders["ops"], json!(["db-read", "db-write"]));
        assert_eq!(r["gaps"].as_array().unwrap().len(), 1); // /orders/:id
        assert_eq!(r["sources"], json!({ "entry_points": 5, "stories": 1 }));
    }
}
