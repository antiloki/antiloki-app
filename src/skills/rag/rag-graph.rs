//! rag-graph.rs — twin of skills/rag/rag-graph.mjs (db READ lane, cross-db: memory.rag_chunks +
//! memory.rag_edges via ATTACH). NAMES ARE CONTRACT: `ragGraph` (default). Returns the vector
//! graph for the FE Knowledge canvas: { nodes, edges, by_db, total }.
// QUALITY: 82/100 — limit clamp (Number(limit) || 1500 → [1,4000]), edge endpoint-set filter
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// JS Set identity for db-sourced ids: serialize the JSON value (1 vs "1" stay distinct, like ===)
fn id_key(v: Option<&Value>) -> String { serde_json::to_string(v.unwrap_or(&Value::Null)).unwrap_or_default() }

pub fn ragGraph(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('rag-graph','db') when no db — the db lane always injects one.
    // const { limit = 1500 } = input ?? {}; then Number(limit) || 1500 — absent/null/0/''/NaN → 1500
    let n = js_number(input.get("limit"));
    let eff = if n != 0.0 && !n.is_nan() { n } else { 1500.0 };
    let clamped = eff.min(4000.0).max(1.0); // Math.max(1, Math.min(4000, eff))
    // node:sqlite binds integral JS numbers as INTEGER, fractional as REAL — mirror that
    let limit_param = if clamped.fract() == 0.0 { json!(clamped as i64) } else { json!(clamped) };

    let nodes: Vec<Value> = match crate::db_json::query_rows(
        db,
        "\n      SELECT id, source_db, source_table, source_row, substr(text, 1, 140) AS snippet\n      FROM memory.rag_chunks ORDER BY id LIMIT ?\n    ",
        &[limit_param],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(e) => {
            // JS catch: { nodes:[], edges:[], by_db:[], total:0, error: String(e).slice(0,140) }
            let msg: String = e.chars().take(140).collect();
            return Ok(json!({ "nodes": [], "edges": [], "by_db": [], "total": 0, "error": msg }));
        }
    };

    let ids: HashSet<String> = nodes.iter().map(|n| id_key(n.get("id"))).collect();
    let edges: Vec<Value> = match crate::db_json::query_rows(
        db,
        "SELECT from_chunk AS \"from\", to_chunk AS \"to\", kind, score FROM memory.rag_edges",
        &[],
    ) {
        Ok(v) => v
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| ids.contains(&id_key(e.get("from"))) && ids.contains(&id_key(e.get("to"))))
            .collect(),
        Err(_) => Vec::new(),
    };

    let by_db: Value = crate::db_json::query_rows(
        db,
        "SELECT source_db, COUNT(*) AS n FROM memory.rag_chunks GROUP BY source_db ORDER BY n DESC",
        &[],
    )
    .unwrap_or_else(|_| json!([]));

    Ok(json!({ "nodes": nodes, "edges": edges, "by_db": by_db, "total": nodes.len() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
             CREATE TABLE memory.rag_chunks (id INTEGER PRIMARY KEY, source_db TEXT NOT NULL, source_table TEXT NOT NULL, source_row TEXT NOT NULL, text TEXT NOT NULL);
             CREATE TABLE memory.rag_edges (from_chunk INTEGER NOT NULL, to_chunk INTEGER NOT NULL, kind TEXT NOT NULL, score REAL NOT NULL, PRIMARY KEY (from_chunk, to_chunk, kind));
             INSERT INTO memory.rag_chunks (id,source_db,source_table,source_row,text) VALUES
               (1,'brain','ai_calls','1','alpha'),
               (2,'brain','ai_calls','2','beta'),
               (3,'laws','laws','L1','gamma'),
               (4,'knowledge','sources','S1','delta');
             INSERT INTO memory.rag_edges (from_chunk,to_chunk,kind,score) VALUES
               (1,2,'knn',0.91),(2,3,'knn',0.85),(1,4,'ref',0.5),(3,9,'knn',0.7);",
        )
        .unwrap();
        c
    }

    #[test]
    fn graph_reads_nodes_edges_by_db() {
        let c = seeded();
        let r = ragGraph(&json!({}), &c).unwrap();
        assert_eq!(r["total"], json!(4));
        assert_eq!(r["nodes"].as_array().unwrap().len(), 4);
        assert_eq!(r["nodes"][0]["snippet"], json!("alpha"));
        // edge (3,9) dropped: 9 is not a node
        assert_eq!(r["edges"].as_array().unwrap().len(), 3);
        assert_eq!(r["edges"][0], json!({ "from": 1, "to": 2, "kind": "knn", "score": 0.91 }));
        // by_db ordered by count desc: brain(2), then laws(1)/knowledge(1)
        assert_eq!(r["by_db"][0], json!({ "source_db": "brain", "n": 2 }));
    }

    #[test]
    fn limit_clamps_and_filters_edges() {
        let c = seeded();
        // limit -5 → Math.max(1, ...) → 1 node; every edge references a dropped endpoint
        let r = ragGraph(&json!({ "limit": -5 }), &c).unwrap();
        assert_eq!(r["total"], json!(1));
        assert_eq!(r["edges"], json!([]));
        // by_db still counts ALL chunks (not limited)
        assert_eq!(r["by_db"].as_array().unwrap().len(), 3);
        // limit "2" → Number("2") = 2
        let r2 = ragGraph(&json!({ "limit": "2" }), &c).unwrap();
        assert_eq!(r2["total"], json!(2));
        // limit 0 → falsy → default 1500
        let r3 = ragGraph(&json!({ "limit": 0 }), &c).unwrap();
        assert_eq!(r3["total"], json!(4));
    }

    #[test]
    fn missing_tables_returns_error_shape() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory;").unwrap();
        let r = ragGraph(&json!({}), &c).unwrap();
        assert_eq!(r["nodes"], json!([]));
        assert_eq!(r["total"], json!(0));
        assert!(r["error"].as_str().unwrap().contains("rag_chunks"));
    }
}
