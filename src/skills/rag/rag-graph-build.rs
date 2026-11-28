//! rag-graph-build.rs — twin of skills/rag/rag-graph-build.mjs (db WRITE lane, cross-db:
//! memory.rag_chunks → memory.rag_edges via ATTACH). NAMES ARE CONTRACT: `ragGraphBuild`
//! (default). Rebuilds the graph-RAG edges from scratch: kNN cosine neighbours (vectors are
//! L2-normalized upstream, so cosine = dot) stored undirected (min,max) + deduped, plus 'ref'
//! edges where a chunk's text mentions another chunk's slug-like source_row.
use rusqlite::Connection;
use serde_json::{json, Value};

fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// JS unpackVector: a BLOB's bytes decode as little-endian f32s (widened to f64 like V8 does on
// read). Any NON-blob cell (TEXT/INTEGER/REAL/NULL) hits `new Uint8Array(x)` in JS, which
// allocates ZEROED bytes — every element is 0, so each dot() contribution is 0. An empty vec is
// observationally identical through dot() (zeros of any length dot to 0), so that's what we return.
fn unpack_vector(v: &rusqlite::types::Value) -> Vec<f64> {
    match v {
        rusqlite::types::Value::Blob(b) => b
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f64)
            .collect(),
        _ => Vec::new(),
    }
}

// JS dot(a, b): f64 accumulation over min length — same IEEE ops in the same order.
fn dot(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    let mut s = 0.0;
    for i in 0..n {
        s += a[i] * b[i];
    }
    s
}

struct Chunk {
    id: i64,
    source_row: String,
    text: String,
    vec: Vec<f64>,
}

pub fn ragGraphBuild(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('rag-graph-build','db') when no db — the db lane always injects one.
    // const { k = KNN_K } = input ?? {} — the default (6) applies only when the key is ABSENT;
    // a present null/0/'abc' flows into Math.min(k, sims.length) as ToNumber(k) (0 / 0 / NaN).
    let k: f64 = match input.get("k") {
        None => 6.0,
        Some(v) => js_number(Some(v)),
    };

    let rows: Vec<Chunk> = {
        let mut stmt = db
            .prepare("SELECT id, source_row, text, vector FROM memory.rag_chunks ORDER BY id")
            .map_err(|e| e.to_string())?;
        let mapped = stmt
            .query_map([], |r| {
                let vector: rusqlite::types::Value = r.get(3)?;
                Ok(Chunk {
                    id: r.get(0)?,
                    source_row: r.get(1)?,
                    // JS (r.text || '') — NULL falls back to ''
                    text: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    vec: unpack_vector(&vector),
                })
            })
            .map_err(|e| e.to_string())?;
        mapped.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };

    db.execute("DELETE FROM memory.rag_edges", []).map_err(|e| e.to_string())?;
    if rows.len() < 2 {
        return Ok(json!({ "nodes": rows.len(), "knn_edges": 0, "ref_edges": 0 }));
    }

    let mut ins = db
        .prepare("INSERT OR IGNORE INTO memory.rag_edges (from_chunk, to_chunk, kind, score) VALUES (?, ?, ?, ?)")
        .map_err(|e| e.to_string())?;

    // kNN: top-k cosine neighbours per chunk, stored undirected + deduped.
    for i in 0..rows.len() {
        let mut sims: Vec<(usize, f64)> = Vec::new();
        for j in 0..rows.len() {
            if i != j {
                sims.push((j, dot(&rows[i].vec, &rows[j].vec)));
            }
        }
        // JS (a,b)=>b[1]-a[1]: stable desc, NaN comparator result treated as equal
        sims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        // t < Math.min(k, sims.length) — NaN limit (k:'abc') never admits an iteration
        let lim = if k.is_nan() { f64::NAN } else { k.min(sims.len() as f64) };
        let mut t = 0usize;
        while (t as f64) < lim {
            let (j, score) = sims[t];
            let (a, b) = (rows[i].id, rows[j].id);
            ins.execute(rusqlite::params![a.min(b), a.max(b), "knn", score])
                .map_err(|e| e.to_string())?;
            t += 1;
        }
    }

    // ref: a chunk's text mentions another chunk's slug-like source_row (≥6 chars, slug/id shaped).
    let re = regex::Regex::new(r"^[A-Za-z0-9][\w./-]{5,}$").unwrap();
    let slugs: Vec<&Chunk> = rows.iter().filter(|r| re.is_match(&r.source_row)).collect();
    for r in &rows {
        let lower = r.text.to_lowercase();
        for s in &slugs {
            if s.id == r.id {
                continue;
            }
            if lower.contains(&s.source_row.to_lowercase()) {
                ins.execute(rusqlite::params![r.id.min(s.id), r.id.max(s.id), "ref", 1.0f64])
                    .map_err(|e| e.to_string())?;
            }
        }
    }

    let knn: i64 = db
        .query_row("SELECT COUNT(*) c FROM memory.rag_edges WHERE kind='knn'", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let ref_count: i64 = db
        .query_row("SELECT COUNT(*) c FROM memory.rag_edges WHERE kind='ref'", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    Ok(json!({ "nodes": rows.len(), "knn_edges": knn, "ref_edges": ref_count }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
             CREATE TABLE memory.rag_chunks (id INTEGER PRIMARY KEY, source_row TEXT NOT NULL, text TEXT NOT NULL, vector BLOB);
             CREATE TABLE memory.rag_edges (from_chunk INTEGER NOT NULL, to_chunk INTEGER NOT NULL, kind TEXT NOT NULL, score REAL NOT NULL, PRIMARY KEY (from_chunk, to_chunk, kind));",
        )
        .unwrap();
        c
    }

    fn edges(c: &Connection) -> Vec<(i64, i64, String, f64)> {
        let mut stmt = c
            .prepare("SELECT from_chunk, to_chunk, kind, score FROM memory.rag_edges ORDER BY rowid")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn knn_scores_are_f32_widened_dots_and_dedup_undirected() {
        let c = fresh();
        let mut ins = c
            .prepare("INSERT INTO memory.rag_chunks (id, source_row, text, vector) VALUES (?,?,?,?)")
            .unwrap();
        ins.execute(rusqlite::params![1i64, "r1", "a", blob(&[1.0, 0.0, 0.0])]).unwrap();
        ins.execute(rusqlite::params![2i64, "r2", "b", blob(&[0.6, 0.8, 0.0])]).unwrap();
        ins.execute(rusqlite::params![3i64, "r3", "c", blob(&[0.0, 1.0, 0.0])]).unwrap();
        drop(ins);
        let r = ragGraphBuild(&json!({ "k": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "nodes": 3, "knn_edges": 2, "ref_edges": 0 }));
        // probe against the JS original: i=1's best is j=2 (0.8), i=2's best dedups into it
        assert_eq!(
            edges(&c),
            vec![
                (1, 2, "knn".into(), 0.6f32 as f64),
                (2, 3, "knn".into(), 0.8f32 as f64),
            ]
        );
    }

    #[test]
    fn ref_edges_match_slug_mentions_case_insensitive_and_dedup() {
        let c = fresh();
        let mut ins = c
            .prepare("INSERT INTO memory.rag_chunks (id, source_row, text, vector) VALUES (?,?,?,?)")
            .unwrap();
        // mutual mention → single undirected ref edge; 'x1' is too short to be a slug
        ins.execute(rusqlite::params![5i64, "SLUG-ONE-LONG", "mentions slug-two-long", "v"]).unwrap();
        ins.execute(rusqlite::params![7i64, "SLUG-TWO-LONG", "mentions slug-one-long", "v"]).unwrap();
        ins.execute(rusqlite::params![9i64, "x1", "mentions slug-one-long too", "v"]).unwrap();
        drop(ins);
        let r = ragGraphBuild(&json!({ "k": 0 }), &c).unwrap();
        assert_eq!(r, json!({ "nodes": 3, "knn_edges": 0, "ref_edges": 2 }));
        assert_eq!(
            edges(&c),
            vec![(5, 7, "ref".into(), 1.0), (5, 9, "ref".into(), 1.0)]
        );
    }

    #[test]
    fn rebuild_deletes_stale_edges_and_under_two_rows_returns_early() {
        let c = fresh();
        c.execute_batch("INSERT INTO memory.rag_edges (from_chunk,to_chunk,kind,score) VALUES (1,2,'knn',0.5);").unwrap();
        let r = ragGraphBuild(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "nodes": 0, "knn_edges": 0, "ref_edges": 0 }));
        assert_eq!(edges(&c).len(), 0); // stale edge deleted even on the early-return path

        c.execute(
            "INSERT INTO memory.rag_chunks (id, source_row, text, vector) VALUES (1,'only','one','v')",
            [],
        )
        .unwrap();
        let r1 = ragGraphBuild(&Value::Null, &c).unwrap(); // input null → k default, unused
        assert_eq!(r1, json!({ "nodes": 1, "knn_edges": 0, "ref_edges": 0 }));
    }

    #[test]
    fn non_blob_vectors_read_as_zero_vectors_and_k_coerces() {
        let c = fresh();
        let mut ins = c
            .prepare("INSERT INTO memory.rag_chunks (id, source_row, text, vector) VALUES (?,?,?,?)")
            .unwrap();
        ins.execute(rusqlite::params![1i64, "r1", "a", rusqlite::types::Value::Null]).unwrap();
        ins.execute(rusqlite::params![2i64, "r2", "b", 5i64]).unwrap();
        ins.execute(rusqlite::params![3i64, "r3", "c", ""]).unwrap();
        drop(ins);
        // all dots 0 → stable tie order: i=0 picks j=1 → (1,2); i=1 picks j=0 → dup; i=2 picks j=0 → (1,3)
        let r = ragGraphBuild(&json!({ "k": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "nodes": 3, "knn_edges": 2, "ref_edges": 0 }));
        assert_eq!(
            edges(&c),
            vec![(1, 2, "knn".into(), 0.0), (1, 3, "knn".into(), 0.0)]
        );
        // k:'abc' → Math.min(NaN, …) admits no iterations
        let r2 = ragGraphBuild(&json!({ "k": "abc" }), &c).unwrap();
        assert_eq!(r2, json!({ "nodes": 3, "knn_edges": 0, "ref_edges": 0 }));
        // k:null → ToNumber(null)=0 → no knn either
        let r3 = ragGraphBuild(&json!({ "k": null }), &c).unwrap();
        assert_eq!(r3, json!({ "nodes": 3, "knn_edges": 0, "ref_edges": 0 }));
    }
}
