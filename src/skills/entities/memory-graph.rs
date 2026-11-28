//! memory-graph.rs — twin of skills/entities/memory-graph.mjs (db READ lane, attached `memory`
//! db). NAMES ARE CONTRACT: `memoryGraph` (default). BFS the memory.memory_links graph from a
//! seed memory row up to `depth` hops — undirected by default (directed=true follows only
//! outgoing edges), optional `kinds` allow-list, excludes superseded neighbors unless
//! activeOnly=false. Edges are recorded even when the neighbor they point to is filtered out
//! (audit trail); nodes/visited stay in lockstep so a revisited or cyclic edge never re-queues.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn as_i64(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_i64(),
        _ => None,
    }
}

fn memory_by_id(id: i64, nodes: &[(i64, Value)], db: &Connection) -> Result<Option<Value>, String> {
    if let Some((_, row)) = nodes.iter().find(|(nid, _)| *nid == id) {
        return Ok(Some(row.clone()));
    }
    let rows = crate::db_json::query_rows(db, "SELECT * FROM memory.memories WHERE id=?", &[json!(id)])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned())
}

pub fn memoryGraph(input: &Value, db: &Connection) -> Result<Value, String> {
    let seed_memory_id = as_i64(input.get("seed_memory_id"))
        .ok_or_else(|| "memory-graph: seed_memory_id (integer) is required".to_string())?;
    let depth = match input.get("depth") {
        None => 1i64,
        Some(v) => as_i64(Some(v)).ok_or_else(|| "memory-graph: depth must be a non-negative integer".to_string())?,
    };
    if depth < 0 {
        return Err("memory-graph: depth must be a non-negative integer".to_string());
    }
    let directed = truthy(input.get("directed"));
    let active_only = match input.get("activeOnly") {
        None => true,
        Some(v) => truthy(Some(v)),
    };
    // kinds: null/absent = no filter; an array (even empty) is truthy in JS, so it DOES filter —
    // an empty array blocks every edge. Mirrors `if (kinds && !kinds.includes(link.kind))`.
    let kinds: Option<Vec<String>> = match input.get("kinds") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect()),
        Some(_) => None,
    };

    let seed_rows = crate::db_json::query_rows(db, "SELECT * FROM memory.memories WHERE id=?", &[json!(seed_memory_id)])?;
    let seed_row = match seed_rows.as_array().and_then(|a| a.first()).cloned() {
        Some(r) => r,
        None => return Err(format!("memory-graph: memory id '{}' not found", seed_memory_id)),
    };

    let mut nodes: Vec<(i64, Value)> = vec![(seed_memory_id, seed_row.clone())];
    let mut edges: Vec<Value> = Vec::new();
    let mut seen_edges: std::collections::HashSet<i64> = std::collections::HashSet::new();

    if depth == 0 {
        let mut out = Map::new();
        out.insert("nodes".into(), json!([seed_row]));
        out.insert("edges".into(), json!([]));
        return Ok(Value::Object(out));
    }

    let mut frontier: Vec<i64> = vec![seed_memory_id];
    let mut visited: std::collections::HashSet<i64> = std::collections::HashSet::from([seed_memory_id]);

    let mut d = 0i64;
    while d < depth && !frontier.is_empty() {
        let mut next: Vec<i64> = Vec::new();
        for &current_id in &frontier {
            let (sql, params): (&str, Vec<Value>) = if directed {
                ("SELECT * FROM memory.memory_links WHERE from_memory_id=?", vec![json!(current_id)])
            } else {
                (
                    "SELECT * FROM memory.memory_links WHERE from_memory_id=? OR to_memory_id=?",
                    vec![json!(current_id), json!(current_id)],
                )
            };
            let links = crate::db_json::query_rows(db, sql, &params)?;
            let links = links.as_array().cloned().unwrap_or_default();

            for link in links {
                let link_kind = link.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if let Some(ref k) = kinds {
                    if !k.iter().any(|kk| kk == &link_kind) {
                        continue;
                    }
                }
                let link_id = as_i64(link.get("id")).unwrap_or_default();
                if seen_edges.contains(&link_id) {
                    continue;
                }
                seen_edges.insert(link_id);
                edges.push(link.clone());

                let from_id = as_i64(link.get("from_memory_id")).unwrap_or_default();
                let to_id = as_i64(link.get("to_memory_id")).unwrap_or_default();
                let other_id = if from_id == current_id { to_id } else { from_id };
                if visited.contains(&other_id) {
                    continue;
                }
                let other_mem = match memory_by_id(other_id, &nodes, db)? {
                    Some(m) => m,
                    None => continue,
                };
                if active_only {
                    let superseded_by_set = other_mem.get("superseded_by").map_or(false, |v| !v.is_null());
                    if superseded_by_set {
                        continue;
                    }
                }
                nodes.push((other_id, other_mem));
                visited.insert(other_id);
                next.push(other_id);
            }
        }
        frontier = next;
        d += 1;
    }

    let mut out = Map::new();
    out.insert("nodes".into(), json!(nodes.into_iter().map(|(_, v)| v).collect::<Vec<_>>()));
    out.insert("edges".into(), json!(edges));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, title TEXT, superseded_by INTEGER);
             CREATE TABLE memory.memory_links (id INTEGER PRIMARY KEY, from_memory_id INTEGER NOT NULL, to_memory_id INTEGER NOT NULL, kind TEXT NOT NULL);",
        )
        .unwrap();
        c
    }

    #[test]
    fn depth_zero_returns_seed_only() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,title) VALUES (1,'a')", []).unwrap();
        let r = memoryGraph(&json!({ "seed_memory_id": 1, "depth": 0 }), &c).unwrap();
        assert_eq!(r["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(r["edges"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn depth_one_undirected_reaches_both_directions() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,title) VALUES (1,'a'),(2,'b'),(3,'c')", []).unwrap();
        c.execute("INSERT INTO memory.memory_links (id,from_memory_id,to_memory_id,kind) VALUES (1,1,2,'related'),(2,3,1,'extends')", []).unwrap();
        let r = memoryGraph(&json!({ "seed_memory_id": 1, "depth": 1 }), &c).unwrap();
        let mut ids: Vec<i64> = r["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_i64().unwrap()).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 3]);
        assert_eq!(r["edges"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn directed_only_follows_outgoing() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,title) VALUES (1,'a'),(2,'b'),(3,'c')", []).unwrap();
        c.execute("INSERT INTO memory.memory_links (id,from_memory_id,to_memory_id,kind) VALUES (1,1,2,'related'),(2,3,1,'extends')", []).unwrap();
        let r = memoryGraph(&json!({ "seed_memory_id": 1, "depth": 1, "directed": true }), &c).unwrap();
        let mut ids: Vec<i64> = r["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_i64().unwrap()).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn kinds_filter_restricts_traversal() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,title) VALUES (1,'a'),(2,'b'),(3,'c')", []).unwrap();
        c.execute("INSERT INTO memory.memory_links (id,from_memory_id,to_memory_id,kind) VALUES (1,1,2,'related'),(2,1,3,'extends')", []).unwrap();
        let r = memoryGraph(&json!({ "seed_memory_id": 1, "depth": 1, "kinds": ["related"] }), &c).unwrap();
        let mut ids: Vec<i64> = r["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_i64().unwrap()).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn empty_kinds_array_is_truthy_and_blocks_everything() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,title) VALUES (1,'a'),(2,'b')", []).unwrap();
        c.execute("INSERT INTO memory.memory_links (id,from_memory_id,to_memory_id,kind) VALUES (1,1,2,'related')", []).unwrap();
        let r = memoryGraph(&json!({ "seed_memory_id": 1, "depth": 1, "kinds": [] }), &c).unwrap();
        assert_eq!(r["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(r["edges"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn excludes_superseded_neighbor_but_keeps_edge() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,title,superseded_by) VALUES (1,'a',NULL),(2,'b',99)", []).unwrap();
        c.execute("INSERT INTO memory.memory_links (id,from_memory_id,to_memory_id,kind) VALUES (1,1,2,'related')", []).unwrap();
        let r = memoryGraph(&json!({ "seed_memory_id": 1, "depth": 1 }), &c).unwrap();
        assert_eq!(r["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(r["edges"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn active_only_false_includes_superseded_neighbor() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,title,superseded_by) VALUES (1,'a',NULL),(2,'b',99)", []).unwrap();
        c.execute("INSERT INTO memory.memory_links (id,from_memory_id,to_memory_id,kind) VALUES (1,1,2,'related')", []).unwrap();
        let r = memoryGraph(&json!({ "seed_memory_id": 1, "depth": 1, "activeOnly": false }), &c).unwrap();
        assert_eq!(r["nodes"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn dangling_edge_target_is_skipped_but_edge_kept() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,title) VALUES (1,'a')", []).unwrap();
        c.execute("INSERT INTO memory.memory_links (id,from_memory_id,to_memory_id,kind) VALUES (1,1,999,'related')", []).unwrap();
        let r = memoryGraph(&json!({ "seed_memory_id": 1, "depth": 1 }), &c).unwrap();
        assert_eq!(r["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(r["edges"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn cycles_do_not_cause_infinite_loop() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,title) VALUES (1,'a'),(2,'b')", []).unwrap();
        c.execute("INSERT INTO memory.memory_links (id,from_memory_id,to_memory_id,kind) VALUES (1,1,2,'related'),(2,2,1,'extends')", []).unwrap();
        let r = memoryGraph(&json!({ "seed_memory_id": 1, "depth": 10 }), &c).unwrap();
        assert_eq!(r["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(r["edges"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn depth_two_reaches_second_hop_and_default_depth_is_one() {
        let c = db();
        c.execute("INSERT INTO memory.memories (id,title) VALUES (1,'a'),(2,'b'),(3,'c')", []).unwrap();
        c.execute("INSERT INTO memory.memory_links (id,from_memory_id,to_memory_id,kind) VALUES (1,1,2,'related'),(2,2,3,'extends')", []).unwrap();
        let r2 = memoryGraph(&json!({ "seed_memory_id": 1, "depth": 2 }), &c).unwrap();
        assert_eq!(r2["nodes"].as_array().unwrap().len(), 3);
        assert_eq!(r2["edges"].as_array().unwrap().len(), 2);
        let r_default = memoryGraph(&json!({ "seed_memory_id": 1 }), &c).unwrap();
        assert_eq!(r_default["nodes"].as_array().unwrap().len(), 2); // depth defaults to 1
    }

    #[test]
    fn seed_not_found_errors() {
        let c = db();
        assert!(memoryGraph(&json!({ "seed_memory_id": 9999 }), &c).unwrap_err().contains("not found"));
    }
}
