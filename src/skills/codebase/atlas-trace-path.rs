//! atlas-trace-path.rs — twin of skills/codebase/atlas-trace-path.mjs (db READ lane). NAMES ARE CONTRACT:
//! `atlasTracePath` (default) · bfsPath. Shortest dependency chain A→B over the atlas call/import graph.
// QUALITY: 82/100 — BFS shortest path (cross-module symbolFind resolve), directed code_edges, node hydrate
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;

fn truthy(v: Option<&Value>) -> bool {
    match v { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()), Some(Value::String(s)) => !s.is_empty(), Some(_) => true }
}
fn str_qq(v: Option<&Value>) -> String {
    match v { None | Some(Value::Null) => String::new(), Some(Value::String(s)) => s.clone(), Some(Value::Bool(b)) => b.to_string(), Some(n @ Value::Number(_)) => n.to_string(), Some(o) => o.to_string() }
}
fn tmpl(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(Value::Null) => "null".into(), Some(x) => str_qq(Some(x)) } }
fn js_number(v: Option<&Value>) -> f64 {
    match v { None => f64::NAN, Some(Value::Null) => 0.0, Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 }, Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }, Some(_) => f64::NAN }
}
// object-key coercion for ids (adj/prev keyed by String(id))
fn key(v: &Value) -> String { match v { Value::String(s) => s.clone(), Value::Number(n) => n.to_string(), Value::Bool(b) => b.to_string(), Value::Null => "null".into(), o => o.to_string() } }

pub fn bfsPath(adj: &Value, src_id: &Value, dst_id: &Value, max_hops: &Value) -> Value {
    let max = { let n = js_number(Some(max_hops)); if n.is_nan() { 8 } else { n as i64 } }; // default 8 when absent
    if src_id == dst_id { return json!({ "ids": [src_id], "edgeTypes": [] }); }
    let mut prev: HashMap<String, Value> = HashMap::new();
    prev.insert(key(src_id), Value::Null);
    let mut prev_edge: HashMap<String, Value> = HashMap::new();
    let mut queue: Vec<Value> = vec![src_id.clone()];
    let mut depth = 0i64;
    while !queue.is_empty() && depth < max {
        let mut next: Vec<Value> = Vec::new();
        for cur in &queue {
            let empty = vec![];
            let neighbors = adj.get(key(cur)).and_then(|v| v.as_array()).unwrap_or(&empty).clone();
            for edge in &neighbors {
                let to = edge.get("to").cloned().unwrap_or(Value::Null);
                let ty = edge.get("type").cloned().unwrap_or(Value::Null);
                let to_key = key(&to);
                if prev.contains_key(&to_key) { continue; }
                prev.insert(to_key.clone(), cur.clone());
                prev_edge.insert(to_key, ty);
                if to == *dst_id {
                    let mut ids: Vec<Value> = Vec::new();
                    let mut c = dst_id.clone();
                    loop {
                        ids.insert(0, c.clone());
                        match prev.get(&key(&c)) { Some(Value::Null) | None => break, Some(p) => c = p.clone() }
                    }
                    let edge_types: Vec<Value> = ids.iter().skip(1).map(|id| prev_edge.get(&key(id)).cloned().unwrap_or(Value::Null)).collect();
                    return json!({ "ids": ids, "edgeTypes": edge_types });
                }
                next.push(to);
            }
        }
        queue = next;
        depth += 1;
    }
    Value::Null
}

fn resolve_node(db: &Connection, query: &Value, origin: &str) -> Option<Value> {
    let q = str_qq(Some(query)).replace('\\', "/");
    if let Ok(r) = crate::db_json::query_rows(db, "SELECT id, name, path, type FROM code_nodes WHERE origin = ? AND path = ? ORDER BY (type='file') DESC, rowid LIMIT 1", &[json!(origin), json!(q)]) {
        if let Some(row) = r.as_array().and_then(|a| a.first()) { return Some(row.clone()); }
    }
    if let Ok(sf) = crate::codebase_symbol_find::symbolFind(&json!({ "query": query, "limit": 1, "origin": origin }), db) {
        if let Some(node) = sf.get("matches").and_then(|m| m.as_array()).and_then(|a| a.first()).and_then(|m| m.get("node")).filter(|n| !n.is_null()) {
            return Some(json!({ "id": node.get("id").cloned().unwrap_or(Value::Null), "name": node.get("name").cloned().unwrap_or(Value::Null), "path": node.get("path").cloned().unwrap_or(Value::Null), "type": node.get("type").cloned().unwrap_or(Value::Null) }));
        }
    }
    None
}

pub fn atlasTracePath(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
    let from = input.get("from");
    let to = input.get("to");
    if !truthy(from) || !truthy(to) { return Err("atlas-trace-path: from + to required".into()); }
    let max_hops = js_number(Some(&input.get("max_hops").filter(|v| !v.is_null()).cloned().unwrap_or(json!(8)))).max(1.0);

    let src = resolve_node(db, from.unwrap(), &origin);
    let dst = resolve_node(db, to.unwrap(), &origin);
    let src = match src { Some(s) => s, None => return Ok(json!({ "found": false, "path": [], "edges": [], "hops": 0, "reason": format!("no atlas node for \"{}\"", tmpl(from)) })) };
    let dst = match dst { Some(d) => d, None => return Ok(json!({ "found": false, "path": [], "edges": [], "hops": 0, "reason": format!("no atlas node for \"{}\"", tmpl(to)) })) };

    let edge_rows = match crate::db_json::query_rows(db, "SELECT from_node, to_node, type FROM code_edges WHERE origin = ?", &[json!(origin)]) {
        Ok(r) => r.as_array().cloned().unwrap_or_default(),
        Err(_) => return Ok(json!({ "found": false, "path": [], "edges": [], "hops": 0, "reason": "atlas has no code_edges" })),
    };
    let type_filter: Option<Vec<String>> = input.get("edge_types").and_then(|v| v.as_array()).filter(|a| !a.is_empty()).map(|a| a.iter().map(|t| str_qq(Some(t))).collect());
    let mut adj_map = serde_json::Map::new();
    for e in &edge_rows {
        let ty = e.get("type").cloned().unwrap_or(Value::Null);
        if let Some(tf) = &type_filter { if !tf.contains(&str_qq(Some(&ty))) { continue; } }
        let from_k = key(e.get("from_node").unwrap_or(&Value::Null));
        adj_map.entry(from_k).or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!({ "to": e.get("to_node").cloned().unwrap_or(Value::Null), "type": ty }));
    }
    let adj = Value::Object(adj_map);

    let result = bfsPath(&adj, src.get("id").unwrap_or(&Value::Null), dst.get("id").unwrap_or(&Value::Null), &json!(max_hops));
    if result.is_null() {
        return Ok(json!({ "found": false, "from_node": src, "to_node": dst, "path": [], "edges": [], "hops": 0, "reason": format!("no path within {} hops", (max_hops as i64)) }));
    }
    let empty = vec![];
    let ids = result.get("ids").and_then(|v| v.as_array()).unwrap_or(&empty).clone();
    let rows = if !ids.is_empty() {
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!("SELECT id, name, path, type FROM code_nodes WHERE origin = ? AND id IN ({})", placeholders);
        let mut params = vec![json!(origin)];
        params.extend(ids.iter().cloned());
        crate::db_json::query_rows(db, &sql, &params).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default()
    } else { vec![] };
    let mut by_id: HashMap<String, Value> = HashMap::new();
    for n in &rows { by_id.insert(key(n.get("id").unwrap_or(&Value::Null)), n.clone()); }
    let path: Vec<Value> = ids.iter().map(|id| by_id.get(&key(id)).cloned().unwrap_or_else(|| json!({ "id": id }))).collect();
    let edges: Vec<Value> = result.get("edgeTypes").and_then(|v| v.as_array()).unwrap_or(&empty).iter().map(|t| json!({ "type": t })).collect();
    Ok(json!({ "found": true, "from_node": src, "to_node": dst, "path": path, "edges": edges, "hops": ids.len() as i64 - 1 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn traces() {
        let adj = json!({ "1": [{ "to": "2", "type": "imports" }, { "to": "3", "type": "imports" }], "2": [{ "to": "4", "type": "calls" }], "3": [{ "to": "4", "type": "calls" }], "4": [{ "to": "5", "type": "calls" }] });
        let r = bfsPath(&adj, &json!("1"), &json!("5"), &json!(8));
        assert_eq!(r["ids"], json!(["1", "2", "4", "5"])); // shortest (BFS picks first-discovered)
        assert_eq!(r["edgeTypes"], json!(["imports", "calls", "calls"]));
        assert_eq!(bfsPath(&adj, &json!("1"), &json!("1"), &json!(8)), json!({ "ids": ["1"], "edgeTypes": [] }));
        assert!(bfsPath(&adj, &json!("5"), &json!("1"), &json!(8)).is_null()); // unreachable (directed)
        // db integration
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_nodes (id TEXT, origin TEXT, name TEXT, path TEXT, type TEXT, rowid_ INTEGER); \
          CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
          INSERT INTO code_nodes (id,origin,name,path,type) VALUES ('a','atlas','a.ts','a.ts','file'),('b','atlas','b.ts','b.ts','file'); \
          INSERT INTO code_edges (origin,from_node,to_node,type) VALUES ('atlas','a','b','imports');").unwrap();
        let t = atlasTracePath(&json!({ "from": "a.ts", "to": "b.ts" }), &c).unwrap();
        assert_eq!(t["found"], json!(true));
        assert_eq!(t["hops"], json!(1));
    }
}
