//! data-story-trace.rs — twin of skills/codebase/data-story-trace.mjs (db READ lane). NAMES ARE
//! CONTRACT: `dataStoryTrace` (async default in JS → sync twin). DETERMINISTIC entry→sink walk over
//! the real graph (calls+imports+dispatches+uses+renders); every hop is a real edge.
// The JS `ai_fill` tail needs a `provider_fn` from context; the conform harness only passes {db}, so
// that branch is unreachable (JS lane never runs it) — the twin has no provider_fn and omits it.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};

fn truthy(v: Option<&Value>) -> bool { match v { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()), Some(Value::String(s)) => !s.is_empty(), Some(_) => true } }
fn str_qq(v: Option<&Value>) -> String { match v { None | Some(Value::Null) => String::new(), Some(Value::String(s)) => s.clone(), Some(Value::Bool(b)) => b.to_string(), Some(n @ Value::Number(_)) => n.to_string(), Some(o) => o.to_string() } }
fn js_string(v: &Value) -> String { match v { Value::Null => "null".into(), Value::String(s) => s.clone(), Value::Number(n) => n.to_string(), Value::Bool(b) => b.to_string(), Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_) => "[object Object]".into() } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn js_number(v: Option<&Value>) -> f64 { match v { None => f64::NAN, Some(Value::Null) => 0.0, Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 }, Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }, Some(_) => f64::NAN } }

// Map/Set key for a node id / path value (all strings in the atlas; SameValueZero on the string).
fn nid(v: &Value) -> String { match v { Value::String(s) => s.clone(), other => other.to_string() } }

// baseName = String(p ?? '').split(/[/\\]/).filter(Boolean).pop() || String(p) — kept for parity
// with the (unreachable-here) ai_fill tail; unused in the conform path.
#[allow(dead_code)]
fn base_name(p: &Value) -> String {
    let s = str_qq(Some(p));
    match s.split(|c| c == '/' || c == '\\').filter(|x| !x.is_empty()).last() {
        Some(last) => last.to_string(),
        None => js_string(p),
    }
}
// shortName = String(p ?? '').split(/[/\\]/).filter(Boolean).slice(-2).join('/') || String(p)
fn short_name(p: &Value) -> String {
    let s = str_qq(Some(p));
    let parts: Vec<&str> = s.split(|c| c == '/' || c == '\\').filter(|x| !x.is_empty()).collect();
    let start = if parts.len() >= 2 { parts.len() - 2 } else { 0 };
    let joined = parts[start..].join("/");
    if joined.is_empty() { js_string(p) } else { joined }
}

fn is_seed(ep_kinds: &HashMap<String, HashSet<String>>, id: &str) -> bool {
    match ep_kinds.get(id) { Some(k) => k.contains("route") || k.contains("ui"), None => false }
}
fn is_sink(ep_kinds: &HashMap<String, HashSet<String>>, dispatch_targets: &HashSet<String>, id: &str) -> bool {
    let by_kind = ep_kinds.get(id).map_or(false, |k| k.contains("data"));
    by_kind || dispatch_targets.contains(id)
}

// pathDepth: number of edges from the seed to `node` (seed has null parent → depth 0).
fn path_depth(prev: &HashMap<String, Option<String>>, node: &str) -> usize {
    let mut d = 0usize;
    let mut n = node.to_string();
    loop {
        match prev.get(&n) {
            Some(Some(parent)) => { d += 1; n = parent.clone(); }
            _ => break, // Some(None) = null OR None = undefined → JS `!= null` stops
        }
    }
    d
}

// BFS from `seed` to the NEAREST sink (≤ max_hops); returns the path spine [seed, …, sink].
fn trace_path(seed: &str, adj: &HashMap<String, Vec<String>>, ep_kinds: &HashMap<String, HashSet<String>>, dispatch_targets: &HashSet<String>, max_hops: f64) -> Option<Vec<String>> {
    let mut prev: HashMap<String, Option<String>> = HashMap::new();
    prev.insert(seed.to_string(), None);
    let mut q: VecDeque<String> = VecDeque::new();
    q.push_back(seed.to_string());
    while let Some(cur) = q.pop_front() {
        if cur != seed && is_sink(ep_kinds, dispatch_targets, &cur) {
            let mut path: Vec<String> = Vec::new();
            let mut n = Some(cur.clone());
            while let Some(node) = n {
                path.insert(0, node.clone()); // unshift
                n = prev.get(&node).cloned().flatten();
            }
            return Some(path);
        }
        if (path_depth(&prev, &cur) as f64) >= max_hops { continue; }
        if let Some(neighbors) = adj.get(&cur) {
            for nx in neighbors {
                if !prev.contains_key(nx) {
                    prev.insert(nx.clone(), Some(cur.clone()));
                    q.push_back(nx.clone());
                }
            }
        }
    }
    None
}

fn build_proposal(path: &[String], ep_kinds: &HashMap<String, HashSet<String>>, dispatch_targets: &HashSet<String>) -> Value {
    let last_i = path.len() - 1;
    let role = |id: &str, i: usize| -> String {
        if i == 0 {
            return if ep_kinds.get(id).map_or(false, |k| k.contains("route")) { "route".into() } else { "user action".into() };
        }
        if i == last_i {
            if dispatch_targets.contains(id) { return "state update".into(); }
            return if ep_kinds.get(id).map_or(false, |k| k.contains("data")) { "data".into() } else { "sink".into() };
        }
        "flows through".into()
    };
    let hops: Vec<Value> = path.iter().enumerate().map(|(i, id)| {
        json!({ "node_id": id, "label": format!("{}: {}", role(id, i), short_name(&Value::String(id.clone()))) })
    }).collect();
    let name = format!("{} \u{2192} {}", short_name(&Value::String(path[0].clone())), short_name(&Value::String(path[last_i].clone())));
    let trace: Vec<String> = path.iter().map(|id| short_name(&Value::String(id.clone()))).collect();
    let description = format!("Deterministic trace: {}", trace.join(" \u{2192} "));
    json!({ "name": name, "description": description, "direction": "forward", "hops": hops, "complete": true })
}

pub fn dataStoryTrace(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS guard `if (!db) throw` — db is always present in the twin (guard not exercised).
    let origin = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));
    let limit = f64::min(8.0, f64::max(1.0, js_number(Some(&nz(input.get("limit")).cloned().unwrap_or(json!(5))))));
    let max_hops = f64::min(16.0, f64::max(2.0, js_number(Some(&nz(input.get("max_hops")).cloned().unwrap_or(json!(8))))));
    let goal: Option<String> = if truthy(input.get("goal")) { Some(js_string(input.get("goal").unwrap()).to_lowercase()) } else { None };

    // file-level forward adjacency over the real edges.
    let node_rows = crate::db_json::query_rows(db, "SELECT id, path FROM code_nodes WHERE origin=?", &[origin.clone()]).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
    let mut file_of: HashMap<String, Value> = HashMap::new();
    for n in &node_rows { file_of.insert(nid(n.get("id").unwrap_or(&Value::Null)), n.get("path").cloned().unwrap_or(Value::Null)); }

    let file_rows = crate::db_json::query_rows(db, "SELECT id FROM code_nodes WHERE origin=? AND type='file'", &[origin.clone()]).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
    let mut file_ids: HashSet<String> = HashSet::new();
    let mut file_ids_ordered: Vec<String> = Vec::new();
    for n in &file_rows { let id = nid(n.get("id").unwrap_or(&Value::Null)); if file_ids.insert(id.clone()) { file_ids_ordered.push(id); } }

    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    let edge_rows = crate::db_json::query_rows(db, "SELECT from_node, to_node FROM code_edges WHERE origin=? AND type IN ('calls','imports','dispatches','uses','renders')", &[origin.clone()]).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
    for e in &edge_rows {
        let a = e.get("from_node").and_then(|f| file_of.get(&nid(f)));
        let b = e.get("to_node").and_then(|t| file_of.get(&nid(t)));
        if !truthy(a) || !truthy(b) { continue; }
        let (a, b) = (a.unwrap(), b.unwrap());
        if a == b { continue; }
        let (ak, bk) = (nid(a), nid(b));
        if !file_ids.contains(&ak) || !file_ids.contains(&bk) { continue; }
        let neighbors = adj.entry(ak).or_default();
        if !neighbors.contains(&bk) { neighbors.push(bk); }
    }

    // entry points: route/ui = SEEDs; data files + dispatch targets = SINKs.
    let eps = crate::db_json::query_rows(db, "SELECT path, kind FROM code_entry_points WHERE origin=?", &[origin.clone()]).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
    let mut ep_kinds: HashMap<String, HashSet<String>> = HashMap::new();
    for e in &eps { ep_kinds.entry(nid(e.get("path").unwrap_or(&Value::Null))).or_default().insert(nid(e.get("kind").unwrap_or(&Value::Null))); }
    let dt_rows = crate::db_json::query_rows(db, "SELECT DISTINCT to_node FROM code_edges WHERE origin=? AND type='dispatches'", &[origin.clone()]).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
    let dispatch_targets: HashSet<String> = dt_rows.iter().map(|r| nid(r.get("to_node").unwrap_or(&Value::Null))).collect();

    let mut seeds: Vec<String> = file_ids_ordered.iter().filter(|id| is_seed(&ep_kinds, id.as_str())).cloned().collect();
    if let Some(g) = &goal {
        let filtered: Vec<String> = seeds.iter().filter(|id| id.to_lowercase().contains(g.as_str())).cloned().collect();
        if !filtered.is_empty() { seeds = filtered; }
    }

    let mut proposals: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut _unreached: Vec<String> = Vec::new();
    for seed in &seeds {
        match trace_path(seed, &adj, &ep_kinds, &dispatch_targets, max_hops) {
            None => { _unreached.push(seed.clone()); }
            Some(path) => {
                let key = path.join(">");
                if seen.contains(&key) { continue; }
                seen.insert(key);
                proposals.push(build_proposal(&path, &ep_kinds, &dispatch_targets));
                if (proposals.len() as f64) >= limit { break; }
            }
        }
    }
    // ai_fill tail omitted: unreachable in conform (no provider_fn in context).

    let mut out = Map::new();
    out.insert("proposals".into(), json!(proposals));
    out.insert("count".into(), json!(proposals.len() as i64));
    out.insert("deterministic".into(), json!(true));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT); \
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
             CREATE TABLE code_entry_points (origin TEXT, kind TEXT, path TEXT); \
             INSERT INTO code_nodes (id,origin,path,type) VALUES ('a.js','atlas','a.js','file'),('b.js','atlas','b.js','file'),('c.js','atlas','c.js','file'),('lonely.js','atlas','lonely.js','file'); \
             INSERT INTO code_edges (origin,from_node,to_node,type) VALUES ('atlas','a.js','b.js','calls'),('atlas','b.js','c.js','dispatches'); \
             INSERT INTO code_entry_points (origin,kind,path) VALUES ('atlas','ui','a.js'),('atlas','ui','lonely.js');"
        ).unwrap();
        c
    }

    #[test]
    fn traces_ui_to_reducer() {
        let r = dataStoryTrace(&json!({}), &seed_db()).unwrap();
        assert_eq!(r["deterministic"], json!(true));
        assert_eq!(r["count"], json!(1));
        let p = &r["proposals"][0];
        let ids: Vec<&str> = p["hops"].as_array().unwrap().iter().map(|h| h["node_id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["a.js", "b.js", "c.js"]);
        assert_eq!(p["complete"], json!(true));
        assert!(p["hops"][0]["label"].as_str().unwrap().contains("user action"));
        assert!(p["hops"][2]["label"].as_str().unwrap().contains("state update"));
        assert_eq!(p["name"], json!("a.js \u{2192} c.js"));
        // lonely.js reaches no sink → dropped
        assert!(!r["proposals"].as_array().unwrap().iter().any(|pr| pr["hops"][0]["node_id"] == json!("lonely.js")));
    }

    #[test]
    fn route_role_and_data_sink_with_limit() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT); \
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
             CREATE TABLE code_entry_points (origin TEXT, kind TEXT, path TEXT); \
             INSERT INTO code_nodes (id,origin,path,type) VALUES ('src/pages/home.jsx','atlas','src/pages/home.jsx','file'),('src/pages/search.jsx','atlas','src/pages/search.jsx','file'),('src/components/view.js','atlas','src/components/view.js','file'),('src/db/store.js','atlas','src/db/store.js','file'); \
             INSERT INTO code_edges (origin,from_node,to_node,type) VALUES ('atlas','src/pages/home.jsx','src/components/view.js','renders'),('atlas','src/pages/search.jsx','src/components/view.js','renders'),('atlas','src/components/view.js','src/db/store.js','uses'); \
             INSERT INTO code_entry_points (origin,kind,path) VALUES ('atlas','route','src/pages/home.jsx'),('atlas','ui','src/pages/search.jsx'),('atlas','data','src/db/store.js');"
        ).unwrap();
        let r = dataStoryTrace(&json!({ "limit": 1 }), &c).unwrap();
        assert_eq!(r["count"], json!(1)); // limit break
        let p = &r["proposals"][0];
        assert!(p["hops"][0]["label"].as_str().unwrap().starts_with("route:"));
        assert_eq!(p["hops"][2]["label"], json!("data: db/store.js")); // data sink + shortName slice(-2)
        assert_eq!(p["name"], json!("pages/home.jsx \u{2192} db/store.js"));
    }
}
