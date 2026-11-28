//! memory-rag-context.rs — twin of skills/entities/memory-rag-context.mjs (db READ lane, attached
//! `memory` db via memory-similar + memory-graph). NAMES ARE CONTRACT: `memoryRagContext`
//! (default). GraphRAG retrieval: pull topK anchors by cosine similarity (memory-similar), BFS
//! graph-expand each anchor (memory-graph) into a candidate union keyed by memory_id (insertion
//! order preserved like a JS Map — re-touching an existing key updates its value in place, never
//! moves its position), rerank by composite score = alpha*similarity + beta*graphProximity +
//! gamma*confidence where graphProximity = (direct anchor<->node edge weight, else 0.5) /
//! (graphDepth+1) accumulated via Math.max across every anchor path that reaches the same
//! candidate, filter by minScore, slice to limit.
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS Number(v)
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}
// = JS Math.max(a,b): either NaN poisons the result.
fn js_max(a: f64, b: f64) -> f64 { if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) } }

fn arr_len(v: &Value) -> usize { v.as_array().map_or(0, |a| a.len()) }

// JS `arr.slice(0, end)`: ToIntegerOrInfinity(end) then clamp into [0, len] (negative counts
// from the array's end).
fn array_slice_end(len: usize, end: f64) -> usize {
    let e = if end.is_nan() { 0.0 } else if end.is_infinite() { end } else { end.trunc() };
    let len_f = len as f64;
    let rel_end = if e < 0.0 { (len_f + e).max(0.0) } else { e.min(len_f) };
    rel_end as usize
}

struct Candidate {
    memory_id: Value,
    memory: Value,
    similarity: f64,
    graph_proximity: f64,
    confidence: Value,
    sources: Vec<&'static str>,
}

pub fn memoryRagContext(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));

    let vector_val = get("vector").cloned().unwrap_or(Value::Null);
    if !vector_val.is_array() || arr_len(&vector_val) == 0 {
        return Err("memory-rag-context: vector (non-empty array) is required".into());
    }
    let model_val = get("model").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&model_val)) {
        return Err("memory-rag-context: model is required".into());
    }

    // Defaults apply ONLY when the key is ABSENT (JS destructuring default) — an explicit
    // null/0/'' present in input overrides the default and flows through as-is.
    let embedded_field = match get("embedded_field") { None => json!("situation"), Some(v) => v.clone() };
    let top_k = match get("topK") { None => json!(5), Some(v) => v.clone() };
    let graph_depth = match get("graphDepth") { None => json!(1), Some(v) => v.clone() };
    let kinds = match get("kinds") { None => Value::Null, Some(v) => v.clone() };
    let alpha = match get("alpha") { None => 1.0, Some(v) => js_number(Some(v)) };
    let beta = match get("beta") { None => 0.3, Some(v) => js_number(Some(v)) };
    let gamma = match get("gamma") { None => 0.2, Some(v) => js_number(Some(v)) };
    let limit = match get("limit") { None => 10.0, Some(v) => js_number(Some(v)) };
    let min_score = match get("minScore") { None => 0.0, Some(v) => js_number(Some(v)) };
    let graph_depth_num = js_number(Some(&graph_depth));

    // 1. Semantic anchors via similarity
    let anchors_input = json!({
        "vector": vector_val, "model": model_val, "embedded_field": embedded_field,
        "limit": top_k, "minScore": 0.0, "activeOnly": true,
    });
    let anchors = crate::entities_memory_similar::memorySimilar(&anchors_input, db)?;
    let anchors_arr = anchors.as_array().cloned().unwrap_or_default();

    // Map of memory_id -> candidate, insertion-order preserved like a JS Map.
    let mut order: Vec<i64> = Vec::new();
    let mut map: std::collections::HashMap<i64, Candidate> = std::collections::HashMap::new();

    for a in &anchors_arr {
        let mid_key = a.get("memory_id").and_then(|v| v.as_i64()).unwrap_or_default();
        let memory = a.get("memory").cloned().unwrap_or(Value::Null);
        let confidence = memory.get("confidence").cloned().unwrap_or(Value::Null);
        let similarity = a.get("score").and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
        if !map.contains_key(&mid_key) { order.push(mid_key); }
        map.insert(mid_key, Candidate {
            memory_id: a.get("memory_id").cloned().unwrap_or(Value::Null),
            memory,
            similarity,
            graph_proximity: 1.0, // anchors themselves are at distance 0
            confidence,
            sources: vec!["similarity"],
        });
    }

    // 2. Graph expansion from each anchor
    for a in &anchors_arr {
        let mid_val = a.get("memory_id").cloned().unwrap_or(Value::Null);
        let graph_input = json!({
            "seed_memory_id": mid_val.clone(), "depth": graph_depth.clone(), "kinds": kinds.clone(), "activeOnly": true,
        });
        let subgraph = crate::entities_memory_graph::memoryGraph(&graph_input, db)?;
        let nodes = subgraph.get("nodes").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let edges = subgraph.get("edges").and_then(|v| v.as_array()).cloned().unwrap_or_default();

        for node in &nodes {
            let node_id_val = node.get("id").cloned().unwrap_or(Value::Null);
            if node_id_val == mid_val { continue; }
            let node_key = node_id_val.as_i64().unwrap_or_default();

            let edge = edges.iter().find(|e| {
                let from = e.get("from_memory_id").cloned().unwrap_or(Value::Null);
                let to = e.get("to_memory_id").cloned().unwrap_or(Value::Null);
                (from == mid_val && to == node_id_val) || (to == mid_val && from == node_id_val)
            });
            let link_weight = match edge { Some(e) => js_number(e.get("weight")), None => 0.5 };
            let graph_proximity = link_weight / (graph_depth_num + 1.0);

            if let Some(existing) = map.get_mut(&node_key) {
                existing.graph_proximity = js_max(existing.graph_proximity, graph_proximity);
                if !existing.sources.contains(&"graph") { existing.sources.push("graph"); }
            } else {
                let confidence = node.get("confidence").cloned().unwrap_or(Value::Null);
                order.push(node_key);
                map.insert(node_key, Candidate {
                    memory_id: node_id_val,
                    memory: node.clone(),
                    similarity: 0.0,
                    graph_proximity,
                    confidence,
                    sources: vec!["graph"],
                });
            }
        }
    }

    // 3. Rerank by composite score
    let mut scored: Vec<(f64, Candidate)> = order.into_iter().filter_map(|mid| {
        map.remove(&mid).map(|c| {
            let confidence_f = js_number(Some(&c.confidence));
            let score = alpha * c.similarity + beta * c.graph_proximity + gamma * confidence_f;
            (score, c)
        })
    }).collect();
    // JS `.sort((a,b) => b.score - a.score)`: stable descending (a NaN comparator result is
    // treated as 'equal' by V8, keeping relative order).
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let filtered: Vec<(f64, Candidate)> = scored.into_iter().filter(|(score, _)| *score >= min_score).collect();
    let end = array_slice_end(filtered.len(), limit);
    let out: Vec<Value> = filtered.into_iter().take(end).map(|(score, c)| json!({
        "memory_id": c.memory_id,
        "score": score,
        "similarity": c.similarity,
        "graph_proximity": c.graph_proximity,
        "confidence": c.confidence,
        "sources": c.sources,
        "memory": c.memory,
    })).collect();
    Ok(json!(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
             CREATE TABLE memory.memories (
               id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL,
               situation TEXT NOT NULL, guidance TEXT NOT NULL, naive_response TEXT, rationale TEXT,
               tags_json TEXT NOT NULL DEFAULT '[]', source_card TEXT, source_event INTEGER,
               source_session TEXT, confidence REAL NOT NULL DEFAULT 0.5, validated INTEGER NOT NULL DEFAULT 0,
               superseded_by INTEGER, agent_slug TEXT,
               created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), created_by TEXT NOT NULL
             );
             CREATE TABLE memory.embeddings (
               id INTEGER PRIMARY KEY AUTOINCREMENT, memory_id INTEGER NOT NULL, model TEXT NOT NULL,
               dims INTEGER NOT NULL, vector BLOB NOT NULL, embedded_field TEXT NOT NULL DEFAULT 'situation',
               created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
               UNIQUE (memory_id, model, embedded_field)
             );
             CREATE TABLE memory.memory_links (
               id INTEGER PRIMARY KEY AUTOINCREMENT, from_memory_id INTEGER NOT NULL, to_memory_id INTEGER NOT NULL,
               kind TEXT NOT NULL, weight REAL NOT NULL DEFAULT 1.0
             );",
        )
        .unwrap();
        c
    }

    fn add_memory(c: &Connection, title: &str, confidence: f64) -> i64 {
        c.execute(
            "INSERT INTO memory.memories (type, title, situation, guidance, created_by, confidence) VALUES ('lesson', ?, 's', 'g', 'tester', ?)",
            rusqlite::params![title, confidence],
        )
        .unwrap();
        c.last_insert_rowid()
    }

    fn store(c: &Connection, memory_id: i64, model: &str, vector: &[f64]) {
        crate::entities_memory_embed_store::memoryEmbedStore(
            &json!({ "memory_id": memory_id, "model": model, "vector": vector }),
            c,
        )
        .unwrap();
    }

    fn link(c: &Connection, from: i64, to: i64, kind: &str, weight: f64) {
        c.execute(
            "INSERT INTO memory.memory_links (from_memory_id, to_memory_id, kind, weight) VALUES (?, ?, ?, ?)",
            rusqlite::params![from, to, kind, weight],
        )
        .unwrap();
    }

    #[test]
    fn combines_similarity_and_graph_with_dedup_and_confidence_ranking() {
        let c = db();
        let a = add_memory(&c, "A", 0.9);
        let b = add_memory(&c, "B", 0.4);
        let n3 = add_memory(&c, "C", 0.6);
        store(&c, a, "M", &[1.0, 1.0, 1.0]);
        store(&c, b, "M", &[1.0, 1.0, 1.0]);
        link(&c, a, n3, "related", 0.3);
        link(&c, b, n3, "related", 0.9);

        let r = memoryRagContext(&json!({ "vector": [1.0, 1.0, 1.0], "model": "M" }), &c).unwrap();
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        let c3 = arr.iter().find(|x| x["memory_id"] == json!(n3)).unwrap();
        assert_eq!(c3["sources"], json!(["graph"]));
        // second anchor's edge (weight 0.9) yields a bigger proximity than the first (0.3) — max wins
        assert!((c3["graph_proximity"].as_f64().unwrap() - (0.9 / 2.0)).abs() < 1e-9);
        let ca = arr.iter().find(|x| x["memory_id"] == json!(a)).unwrap();
        assert_eq!(ca["sources"], json!(["similarity"]));
    }

    #[test]
    fn kinds_filter_restricts_graph_expansion() {
        let c = db();
        let a = add_memory(&c, "A", 0.5);
        let b = add_memory(&c, "B", 0.5);
        let n3 = add_memory(&c, "C", 0.5);
        store(&c, a, "M", &[1.0, 1.0, 1.0]);
        link(&c, a, b, "contradicts", 1.0);
        link(&c, a, n3, "related", 1.0);

        let r = memoryRagContext(&json!({ "vector": [1.0, 1.0, 1.0], "model": "M", "kinds": ["related"] }), &c).unwrap();
        let ids: Vec<i64> = r.as_array().unwrap().iter().map(|x| x["memory_id"].as_i64().unwrap()).collect();
        assert!(ids.contains(&n3));
        assert!(!ids.contains(&b));
    }

    #[test]
    fn min_score_filters_out_everything() {
        let c = db();
        let a = add_memory(&c, "A", 0.5);
        store(&c, a, "M", &[1.0, 1.0, 1.0]);
        let r = memoryRagContext(&json!({ "vector": [1.0, 1.0, 1.0], "model": "M", "minScore": 5.0 }), &c).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 0);
    }

    #[test]
    fn limit_restricts_count() {
        let c = db();
        let a = add_memory(&c, "A", 0.5);
        let b = add_memory(&c, "B", 0.5);
        let cc = add_memory(&c, "C", 0.5);
        let d = add_memory(&c, "D", 0.5);
        store(&c, a, "M", &[1.0, 1.0, 1.0]);
        link(&c, a, b, "related", 0.9);
        link(&c, a, cc, "related", 0.5);
        link(&c, a, d, "related", 0.2);

        let r = memoryRagContext(&json!({ "vector": [1.0, 1.0, 1.0], "model": "M", "limit": 2 }), &c).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 2);
    }

    #[test]
    fn top_k_restricts_anchors_pulled_via_similarity() {
        let c = db();
        let a = add_memory(&c, "A", 0.5);
        let b = add_memory(&c, "B", 0.5);
        // C has NO embedding at all — it can never become an anchor regardless of which of
        // A/B the topK=1 cut keeps (tied similarity, tie-break order unspecified here), and
        // it is not linked to anything, so it can never surface via graph expansion either.
        let n3 = add_memory(&c, "C", 0.5);
        store(&c, a, "M", &[1.0, 1.0, 1.0]);
        store(&c, b, "M", &[1.0, 1.0, 1.0]);
        link(&c, a, b, "related", 0.5); // undirected traversal reaches the other regardless of which anchor wins

        let r = memoryRagContext(&json!({ "vector": [1.0, 1.0, 1.0], "model": "M", "topK": 1 }), &c).unwrap();
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 2); // the one anchor kept by topK=1, plus its graph neighbor
        let ids: Vec<i64> = arr.iter().map(|x| x["memory_id"].as_i64().unwrap()).collect();
        assert!(!ids.contains(&n3));
    }

    #[test]
    fn embedded_field_override_restricts_anchor_match() {
        let c = db();
        let a = add_memory(&c, "A", 0.5);
        let b = add_memory(&c, "B", 0.5);
        crate::entities_memory_embed_store::memoryEmbedStore(&json!({ "memory_id": a, "model": "M", "embedded_field": "situation", "vector": [1.0, 1.0, 1.0] }), &c).unwrap();
        crate::entities_memory_embed_store::memoryEmbedStore(&json!({ "memory_id": b, "model": "M", "embedded_field": "guidance", "vector": [1.0, 1.0, 1.0] }), &c).unwrap();

        let r = memoryRagContext(&json!({ "vector": [1.0, 1.0, 1.0], "model": "M", "embedded_field": "guidance" }), &c).unwrap();
        let ids: Vec<i64> = r.as_array().unwrap().iter().map(|x| x["memory_id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![b]);
    }

    #[test]
    fn no_matching_anchors_returns_empty() {
        let c = db();
        let a = add_memory(&c, "A", 0.5);
        store(&c, a, "M", &[1.0, 1.0, 1.0]);
        let r = memoryRagContext(&json!({ "vector": [1.0, 1.0, 1.0], "model": "NOPE" }), &c).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 0);
    }

    #[test]
    fn second_hop_without_direct_anchor_edge_uses_default_weight() {
        let c = db();
        let a = add_memory(&c, "A", 0.5);
        let b = add_memory(&c, "B", 0.5);
        let n3 = add_memory(&c, "C", 0.5);
        store(&c, a, "M", &[1.0, 1.0, 1.0]);
        link(&c, a, b, "related", 0.9);
        link(&c, b, n3, "related", 0.4);

        let r = memoryRagContext(&json!({ "vector": [1.0, 1.0, 1.0], "model": "M", "graphDepth": 2 }), &c).unwrap();
        let c3 = r.as_array().unwrap().iter().find(|x| x["memory_id"] == json!(n3)).unwrap();
        // no direct a<->C edge → default weight 0.5, divided by (graphDepth+1)=3
        assert!((c3["graph_proximity"].as_f64().unwrap() - (0.5 / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn custom_alpha_beta_gamma_weight_the_composite_score() {
        let c = db();
        let a = add_memory(&c, "A", 0.8);
        store(&c, a, "M", &[1.0, 1.0, 1.0]);
        let r = memoryRagContext(&json!({ "vector": [1.0, 1.0, 1.0], "model": "M", "alpha": 2.0, "beta": 0.5, "gamma": 1.0 }), &c).unwrap();
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        // identical query/stored vector -> similarity 1.0 * 2.0 + graphProximity 1.0 (anchor default) * 0.5 + confidence 0.8 * 1.0
        assert!((arr[0]["score"].as_f64().unwrap() - 3.3).abs() < 1e-9);
    }
}
