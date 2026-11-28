//! rag-recall.rs — twin of skills/rag/rag-recall.mjs (db READ lane, `memory.rag_chunks`).
//! NAMES ARE CONTRACT: `ragRecall` (default). Embeds the query, cosine-ranks against pre-stored
//! chunk vectors (L2-normalized at embed time, so dot product == cosine), returns the top-k hits.
//! Degrades to `{ hits: [], empty: true }` when no sweep has run yet (empty/missing table) or the
//! scope filter drops every row.
//!
//! PARITY-DEFERRED (boundary only): `crate::external_embed::embed` — the real 768-dim ONNX call.
//! JS: `const e = await embed({ text: query.slice(0, 8000), model: DEFAULT_MODEL }, context);`
//! with NO try/catch around it — a rejection propagates uncaught out of `ragRecall`. Mirrored
//! exactly: `ragRecall` calls `crate::external_embed::embed(...)?`, so while the deferred stub
//! Errs, `ragRecall` Errs too (same as the real JS with no cached model / no network). Everything
//! else — vector unpack, db read, scope filter, scoring, sort, top-k, hit shaping — is fully
//! ported and independently testable via `rag_recall_ranked`, which takes the query vector
//! directly (bypassing embed) since embed cannot be exercised deterministically in this sandbox.
use rusqlite::types::ValueRef;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

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

fn cell(v: ValueRef) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => json!(b.iter().map(|x| json!(*x as i64)).collect::<Vec<_>>()),
    }
}

// JS `blob instanceof Uint8Array ? blob : new Uint8Array(blob)` — a real BLOB cell passes bytes
// through; anything else hits the TypedArray "length" ctor overload (a bare number allocates
// that many zeroed bytes). Mirrors memory-similar.rs::js_new_uint8array for the same reason.
fn js_new_uint8array(v: ValueRef) -> Vec<u8> {
    match v {
        ValueRef::Blob(b) => b.to_vec(),
        ValueRef::Integer(n) => vec![0u8; n.max(0) as usize],
        ValueRef::Real(f) => vec![0u8; f.max(0.0) as usize],
        _ => Vec::new(),
    }
}

/// JS `unpackVector(blob)`: `Math.floor(byteLength/4)` float32s, little-endian, via DataView.
fn unpack_vector(blob: &[u8]) -> Vec<f32> {
    let n = blob.len() / 4;
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let off = i * 4;
        v.push(f32::from_le_bytes([blob[off], blob[off + 1], blob[off + 2], blob[off + 3]]));
    }
    v
}

/// JS `dot(a, b)`: accumulate over `Math.min(a.length, b.length)` in f64 (Float32Array elements
/// read out as exact-f64 representations of their f32 value, multiplied/summed as JS numbers).
fn dot(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    let mut s = 0.0f64;
    for i in 0..n {
        s += (a[i] as f64) * (b[i] as f64);
    }
    s
}

// JS `String(text || '').replace(/\s+/g, ' ')` — collapse any whitespace run to a single space.
fn collapse_ws(s: &str) -> String {
    let mut out = String::new();
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

struct Row {
    id: Value,
    source_db: String,
    source_table: String,
    source_row: String,
    text: String,
    vector: Vec<u8>,
}

/// Fetch + scope-filter rows. `Err(early)` carries the JS early-return JSON shape
/// (`{ hits: [], empty: true }`) for: db error (JS `try { db.prepare(...).all() } catch { ... }`),
/// empty result set, or an empty-after-scope-filter result set — all three collapse to the same
/// shape in the JS source.
fn fetch_scoped_rows(input: &Value, db: &Connection) -> Result<Vec<Row>, Value> {
    let source_db = input.get("source_db").and_then(Value::as_str).filter(|s| !s.is_empty());
    let scope_paths = input.get("scope_paths").and_then(Value::as_array).filter(|a| !a.is_empty());
    let scope_set: Option<HashSet<String>> =
        scope_paths.map(|a| a.iter().map(|p| json_to_string(p).replace('\\', "/")).collect());

    let sql = if source_db.is_some() {
        "SELECT id, source_db, source_table, source_row, text, vector FROM memory.rag_chunks WHERE source_db = ?"
    } else {
        "SELECT id, source_db, source_table, source_row, text, vector FROM memory.rag_chunks"
    };

    let early = || json!({ "hits": [], "empty": true });

    let query = || -> Result<Vec<Row>, String> {
        let mut stmt = db.prepare(sql).map_err(|e| e.to_string())?;
        let mapped = if let Some(sdb) = source_db {
            stmt.query_map(rusqlite::params![sdb], row_mapper)
        } else {
            stmt.query_map([], row_mapper)
        }
        .map_err(|e| e.to_string())?;
        mapped.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    };

    let mut rows = match query() {
        Ok(r) => r,
        Err(_) => return Err(early()),
    };
    if rows.is_empty() {
        return Err(early());
    }

    // scope filter: only FILE-keyed code (code_nodes / code_descriptions) is restricted to
    // in-scope paths; everything else (docs/decisions/memory/laws + code_data_stories) stays global.
    if let Some(set) = &scope_set {
        rows.retain(|r| {
            !(r.source_db == "code" && (r.source_table == "code_nodes" || r.source_table == "code_descriptions"))
                || set.contains(r.source_row.split('@').next().unwrap_or(""))
        });
    }
    if rows.is_empty() {
        return Err(early());
    }
    Ok(rows)
}

fn row_mapper(row: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        id: cell(row.get_ref(0)?),
        source_db: row.get(1)?,
        source_table: row.get(2)?,
        source_row: row.get(3)?,
        text: row.get::<_, Option<String>>(4)?.unwrap_or_default(), // JS `r.text || ''`
        vector: js_new_uint8array(row.get_ref(5)?),
    })
}

// JS `String(p)` for a JSON value coming out of an input array (scope_paths entries).
fn json_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

// JS `Math.max(1, Math.min(50, Number(k) || 12))`.
fn k_cap(input: &Value) -> usize {
    let n = js_number(input.get("k"));
    let eff = if n != 0.0 && !n.is_nan() { n } else { 12.0 };
    eff.max(1.0).min(50.0) as usize
}

fn shape_hits(rows: Vec<Row>, qv: &[f32], input: &Value, searched: usize, model: Value) -> Value {
    let mut scored: Vec<(Row, f64)> = rows
        .into_iter()
        .map(|r| {
            let v = unpack_vector(&r.vector);
            let score = dot(qv, &v);
            (r, score)
        })
        .collect();
    // JS Array.prototype.sort is stable (ES2019+) — Rust's sort_by matches that guarantee.
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let cap = k_cap(input);
    let hits: Vec<Value> = scored
        .into_iter()
        .take(cap)
        .map(|(r, score)| {
            let text: String = collapse_ws(&r.text).chars().take(600).collect();
            json!({
                "id": r.id, "source_db": r.source_db, "source_table": r.source_table,
                "source_row": r.source_row, "text": text, "score": score,
            })
        })
        .collect();
    json!({ "hits": hits, "empty": false, "model": model, "searched": searched })
}

// JS `query.slice(0, 8000)` on the query string — approximated on chars (documented deviation
// from UTF-16 code units, matching this codebase's existing .slice(0,N) convention).
fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `export default async function ragRecall(input, context)`. `context.db` is always injected
/// by the db lane (JS `if (!db) throw err.contextMissing('rag-recall', 'db')` is unreachable here).
pub fn ragRecall(input: &Value, context: &Value, db: &Connection) -> Result<Value, String> {
    let query = input.get("query").and_then(Value::as_str).filter(|s| !s.is_empty());
    let query = match query {
        Some(q) => q,
        None => return Err("rag-recall: query is required".to_string()),
    };

    let rows = match fetch_scoped_rows(input, db) {
        Ok(rows) => rows,
        Err(early) => return Ok(early),
    };
    let searched = rows.len();

    let embed_input = json!({ "text": truncate_chars(query, 8000), "model": crate::external_embed::DEFAULT_MODEL });
    let e = crate::external_embed::embed(&embed_input, context)?;
    let qv: Vec<f32> = e
        .get("vector")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_f64).map(|f| f as f32).collect())
        .unwrap_or_default();
    let model = e.get("model").cloned().unwrap_or(Value::Null);

    Ok(shape_hits(rows, &qv, input, searched, model))
}

/// TEST-ONLY entry point (not a JS-mirrored export): identical to `ragRecall` from the
/// query-vector onward, but takes `qv`/`model` directly instead of calling the deferred
/// `crate::external_embed::embed`. This is how the deterministic ranking core gets exercised —
/// see module doc.
pub fn rag_recall_ranked(input: &Value, db: &Connection, qv: &[f32], model: Value) -> Result<Value, String> {
    if input.get("query").and_then(Value::as_str).filter(|s| !s.is_empty()).is_none() {
        return Err("rag-recall: query is required".to_string());
    }
    let rows = match fetch_scoped_rows(input, db) {
        Ok(rows) => rows,
        Err(early) => return Ok(early),
    };
    let searched = rows.len();
    Ok(shape_hits(rows, qv, input, searched, model))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(floats: &[f32]) -> Vec<u8> {
        floats.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
             CREATE TABLE memory.rag_chunks (
               id INTEGER PRIMARY KEY, source_db TEXT NOT NULL, source_table TEXT NOT NULL,
               source_row TEXT NOT NULL, text TEXT NOT NULL, vector BLOB NOT NULL
             );",
        )
        .unwrap();
        c
    }

    fn insert(c: &Connection, id: i64, source_db: &str, source_table: &str, source_row: &str, text: &str, v: &[f32]) {
        c.execute(
            "INSERT INTO memory.rag_chunks (id, source_db, source_table, source_row, text, vector) VALUES (?, ?, ?, ?, ?, ?)",
            rusqlite::params![id, source_db, source_table, source_row, text, pack(v)],
        )
        .unwrap();
    }

    #[test]
    fn ranks_by_dot_product_desc_and_respects_k() {
        let c = seeded();
        insert(&c, 1, "brain", "ai_calls", "1", "alpha  content", &[1.0, 0.0, 0.0]);
        insert(&c, 2, "brain", "ai_calls", "2", "beta content", &[0.9, 0.1, 0.0]);
        insert(&c, 3, "laws", "laws", "L1", "gamma content", &[0.0, 1.0, 0.0]);

        let r = rag_recall_ranked(&json!({ "query": "q", "k": 2 }), &c, &[1.0, 0.0, 0.0], json!("test-model")).unwrap();
        assert_eq!(r["empty"], json!(false));
        assert_eq!(r["searched"], json!(3));
        assert_eq!(r["model"], json!("test-model"));
        let hits = r["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2); // k=2 caps the result
        assert_eq!(hits[0]["id"], json!(1));
        assert!((hits[0]["score"].as_f64().unwrap() - 1.0).abs() < 1e-6);
        assert_eq!(hits[1]["id"], json!(2));
        assert!((hits[1]["score"].as_f64().unwrap() - 0.9).abs() < 1e-6);
    }

    #[test]
    fn hit_text_is_whitespace_collapsed_and_truncated_to_600() {
        let c = seeded();
        let long = "word ".repeat(200); // 1000 chars with runs of whitespace
        insert(&c, 1, "brain", "ai_calls", "1", &long, &[1.0]);
        let r = rag_recall_ranked(&json!({ "query": "q" }), &c, &[1.0], Value::Null).unwrap();
        let text = r["hits"][0]["text"].as_str().unwrap();
        assert_eq!(text.len(), 600);
        assert!(!text.contains("  ")); // no double spaces survive collapse
    }

    #[test]
    fn empty_table_returns_empty_result() {
        let c = seeded();
        let r = rag_recall_ranked(&json!({ "query": "q" }), &c, &[1.0], Value::Null).unwrap();
        assert_eq!(r, json!({ "hits": [], "empty": true }));
    }

    #[test]
    fn missing_table_returns_empty_result_not_error() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory;").unwrap();
        let r = rag_recall_ranked(&json!({ "query": "q" }), &c, &[1.0], Value::Null).unwrap();
        assert_eq!(r, json!({ "hits": [], "empty": true }));
    }

    #[test]
    fn requires_query() {
        let c = seeded();
        let e = rag_recall_ranked(&json!({}), &c, &[1.0], Value::Null).unwrap_err();
        assert_eq!(e, "rag-recall: query is required");
        let e2 = rag_recall_ranked(&json!({ "query": "" }), &c, &[1.0], Value::Null).unwrap_err();
        assert_eq!(e2, "rag-recall: query is required");
    }

    #[test]
    fn source_db_filters_the_query() {
        let c = seeded();
        insert(&c, 1, "brain", "ai_calls", "1", "a", &[1.0]);
        insert(&c, 2, "laws", "laws", "L1", "b", &[1.0]);
        let r = rag_recall_ranked(&json!({ "query": "q", "source_db": "laws" }), &c, &[1.0], Value::Null).unwrap();
        assert_eq!(r["searched"], json!(1));
        assert_eq!(r["hits"][0]["id"], json!(2));
    }

    #[test]
    fn scope_paths_restrict_code_nodes_and_descriptions_but_not_other_tables() {
        let c = seeded();
        insert(&c, 1, "code", "code_nodes", "src/a.rs@atlas", "in scope", &[1.0]);
        insert(&c, 2, "code", "code_nodes", "src/b.rs@atlas", "out of scope", &[1.0]);
        insert(&c, 3, "code", "code_descriptions", "src/a.rs", "desc in scope", &[1.0]);
        insert(&c, 4, "code", "code_data_stories", "flow-1", "never filtered", &[1.0]);
        insert(&c, 5, "laws", "laws", "L1", "never filtered global", &[1.0]);

        let r = rag_recall_ranked(
            &json!({ "query": "q", "scope_paths": ["src/a.rs"], "k": 50 }),
            &c,
            &[1.0],
            Value::Null,
        )
        .unwrap();
        let ids: Vec<i64> = r["hits"].as_array().unwrap().iter().map(|h| h["id"].as_i64().unwrap()).collect();
        assert_eq!(ids.len(), 4);
        assert!(!ids.contains(&2)); // out-of-scope code_nodes dropped
        assert!(ids.contains(&1));
        assert!(ids.contains(&3));
        assert!(ids.contains(&4)); // code_data_stories never filtered
        assert!(ids.contains(&5)); // non-code source_db never filtered
    }

    #[test]
    fn scope_paths_normalizes_backslashes_only_on_the_scope_side() {
        // Real JS (rag-recall.mjs) only backslash-normalizes scope_paths entries
        // (`String(p).replace(/\\/g, '/')`); it does NOT normalize source_row before
        // the `.split('@')[0]` comparison. So a Windows-style stored row
        // "src\\a.rs@atlas" does NOT match a scope entry "src/a.rs" — verified against
        // real JS: scopeSet.has(String(source_row).split('@')[0]) === false here.
        let c = seeded();
        insert(&c, 1, "code", "code_nodes", "src\\a.rs@atlas", "windows path row", &[1.0]);
        let r = rag_recall_ranked(&json!({ "query": "q", "scope_paths": ["src/a.rs"] }), &c, &[1.0], Value::Null).unwrap();
        assert_eq!(r, json!({ "hits": [], "empty": true }));
    }

    #[test]
    fn all_rows_dropped_by_scope_filter_returns_empty_result() {
        let c = seeded();
        insert(&c, 1, "code", "code_nodes", "src/a.rs@atlas", "x", &[1.0]);
        let r = rag_recall_ranked(&json!({ "query": "q", "scope_paths": ["nope.rs"] }), &c, &[1.0], Value::Null).unwrap();
        assert_eq!(r, json!({ "hits": [], "empty": true }));
    }

    #[test]
    fn k_defaults_to_12_and_clamps_into_1_to_50() {
        let c = seeded();
        for i in 1..=15 {
            insert(&c, i, "brain", "ai_calls", &i.to_string(), "x", &[1.0]);
        }
        let default_r = rag_recall_ranked(&json!({ "query": "q" }), &c, &[1.0], Value::Null).unwrap();
        assert_eq!(default_r["hits"].as_array().unwrap().len(), 12); // default k=12

        let zero_r = rag_recall_ranked(&json!({ "query": "q", "k": 0 }), &c, &[1.0], Value::Null).unwrap();
        assert_eq!(zero_r["hits"].as_array().unwrap().len(), 12); // 0 is falsy -> default 12

        let clamp_low = rag_recall_ranked(&json!({ "query": "q", "k": -5 }), &c, &[1.0], Value::Null).unwrap();
        assert_eq!(clamp_low["hits"].as_array().unwrap().len(), 1); // Math.max(1, ...)
    }

    #[test]
    fn unpack_vector_reads_little_endian_float32() {
        let bytes = 1.5f32.to_le_bytes();
        let v = unpack_vector(&bytes);
        assert_eq!(v, vec![1.5f32]);
        // trailing partial float32 (not a multiple of 4 bytes) is dropped, matching
        // Math.floor(byteLength / 4).
        let mut padded = bytes.to_vec();
        padded.push(0xFF);
        assert_eq!(unpack_vector(&padded).len(), 1);
    }
}
