//! atlas-subgraph.rs — twin of skills/codebase/atlas-subgraph.mjs (db READ lane). NAMES ARE
//! CONTRACT: `atlasSubgraph` (named default). N-hop bidirectional edge slice around target nodes.
// QUALITY: 80/100 — BFS both directions, path→seed resolution, dynamic IN, edge membership filter
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

pub fn atlasSubgraph(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas").to_string();
    let hops = (input.get("hops").and_then(|v| v.as_f64()).unwrap_or(1.0) as i64).clamp(0, 5);

    let node_ids = input.get("node_ids").and_then(|v| v.as_array());
    let paths = input.get("paths").and_then(|v| v.as_array());
    let mut seeds: Vec<String> = Vec::new();
    if let Some(ids) = node_ids.filter(|a| !a.is_empty()) {
        seeds = ids.iter().map(|v| match v { Value::String(s) => s.clone(), o => o.to_string() }).collect();
    } else if let Some(ps) = paths.filter(|a| !a.is_empty()) {
        let norm: Vec<Value> = ps.iter().map(|p| json!(p.as_str().unwrap_or("").replace('\\', "/"))).collect();
        let ph = norm.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let mut args = vec![json!(origin)];
        args.extend(norm);
        let rows = crate::db_json::query_rows(db, &format!("SELECT id FROM code_nodes WHERE origin = ? AND path IN ({ph})"), &args)?;
        seeds = rows.as_array().unwrap().iter().filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(|s| s.to_string())).collect();
    }
    if seeds.is_empty() {
        if node_ids.is_none() && paths.is_none() {
            return Err("atlas-subgraph: node_ids or paths is required".into());
        }
        return Ok(json!({ "origin": origin, "hops": hops, "seeds": [], "nodes": [], "edges": [], "node_count": 0, "edge_count": 0 }));
    }

    // BFS over edges in BOTH directions up to `hops` levels.
    let mut in_set: HashSet<String> = seeds.iter().cloned().collect();
    let mut frontier: Vec<String> = seeds.clone();
    for _ in 0..hops {
        if frontier.is_empty() { break; }
        let mut next: Vec<String> = Vec::new();
        for cur in &frontier {
            let nbrs = crate::db_json::query_rows(db,
                "SELECT to_node AS n FROM code_edges WHERE origin = ? AND from_node = ? \
                 UNION SELECT from_node AS n FROM code_edges WHERE origin = ? AND to_node = ?",
                &[json!(origin), json!(cur), json!(origin), json!(cur)])?;
            for r in nbrs.as_array().unwrap() {
                if let Some(n) = r.get("n").and_then(|v| v.as_str()) {
                    if in_set.insert(n.to_string()) { next.push(n.to_string()); }
                }
            }
        }
        frontier = next;
    }

    let ids: Vec<Value> = in_set.iter().map(|s| json!(s)).collect();
    let ph = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let nodes = crate::db_json::query_rows(db, &format!("SELECT id, origin, path, type, name, signature, intent, parent_id FROM code_nodes WHERE id IN ({ph})"), &ids)?;
    let all_edges = crate::db_json::query_rows(db, "SELECT from_node, to_node, type FROM code_edges WHERE origin = ?", &[json!(origin)])?;
    let edges: Vec<Value> = all_edges.as_array().unwrap().iter().filter(|e| {
        let f = e.get("from_node").and_then(|v| v.as_str()).unwrap_or("");
        let t = e.get("to_node").and_then(|v| v.as_str()).unwrap_or("");
        in_set.contains(f) && in_set.contains(t)
    }).cloned().collect();

    let node_count = nodes.as_array().unwrap().len();
    let edge_count = edges.len();
    Ok(json!({ "origin": origin, "hops": hops, "seeds": seeds, "nodes": nodes, "edges": edges, "node_count": node_count, "edge_count": edge_count }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn n_hop_slice() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, parent_id TEXT); \
          CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
          INSERT INTO code_nodes (id,origin,path,type,name) VALUES ('a@','atlas','a.ts','file','a'),('b@','atlas','b.ts','file','b'),('af','atlas','a.ts','function','f'); \
          INSERT INTO code_edges (origin,from_node,to_node,type) VALUES ('atlas','a@','b@','imports');").unwrap();
        let r = atlasSubgraph(&json!({ "paths": ["a.ts"], "hops": 1 }), &c).unwrap();
        assert_eq!(r["node_count"], json!(3)); // a@, af (both at a.ts), + b@ (imported)
        assert_eq!(r["edge_count"], json!(1));
        assert!(atlasSubgraph(&json!({}), &c).is_err());
    }
}
