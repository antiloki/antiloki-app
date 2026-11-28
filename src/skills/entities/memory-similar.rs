//! memory-similar.rs — twin of skills/entities/memory-similar.mjs (db READ lane, attached
//! `memory` db). NAMES ARE CONTRACT: `memorySimilar` (default), `cosineSimilarity` (named,
//! no-db). Linear-scan cosine-similarity search over memory.embeddings joined to
//! memory.memories: filter by model/embedded_field/dims/activeOnly, score, sort desc, slice top-N.
use rusqlite::types::ValueRef;
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

// Mirrors db_json::cell (private to its module): node:sqlite's `.all()` hands back a column's
// native JS type. Written locally because the row map here must EXCLUDE one column by name
// (`vector`) while every other e.*/m.* column — including whatever `m.*` happens to expand to —
// still flows through untouched.
fn cell(v: ValueRef) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => json!(b.iter().map(|x| json!(*x as i64)).collect::<Vec<_>>()),
    }
}

// Mirrors db_json::to_sql (private) for the two bound params (model, embedded_field).
fn to_sql_param(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as S;
    match v {
        Value::Null => S::Null,
        Value::Bool(b) => S::Integer(*b as i64),
        Value::Number(n) => n.as_i64().map(S::Integer).unwrap_or_else(|| S::Real(n.as_f64().unwrap())),
        Value::String(s) => S::Text(s.clone()),
        other => S::Text(other.to_string()),
    }
}

// JS `new Uint8Array(x)` where x is the raw `row.vector` cell: a real BLOB passes its bytes
// through unchanged; any other type hits the TypedArray "length" constructor overload (a plain
// number allocates that many ZEROED bytes). Fixtures seed non-blob `vector` cells as an INTEGER
// equal to the needed byte length (dims*4) to exercise that path deterministically — a genuine
// Buffer/Uint8Array isn't representable through the JSON seed mechanism (node:sqlite rejects a
// bound plain array outright; confirmed empirically against both lanes).
fn js_new_uint8array(v: ValueRef) -> Vec<u8> {
    match v {
        ValueRef::Blob(b) => b.to_vec(),
        ValueRef::Integer(n) => vec![0u8; n.max(0) as usize],
        ValueRef::Real(f) => vec![0u8; f.max(0.0) as usize],
        _ => Vec::new(),
    }
}

// JS `arr.slice(0, end)`: ToIntegerOrInfinity(end) then clamp into [0, len] (negative counts
// from the array's end).
fn array_slice_end(len: usize, end: f64) -> usize {
    let e = if end.is_nan() { 0.0 } else if end.is_infinite() { end } else { end.trunc() };
    let len_f = len as f64;
    let rel_end = if e < 0.0 { (len_f + e).max(0.0) } else { e.min(len_f) };
    rel_end as usize
}

fn arr_len(v: &Value) -> usize { v.as_array().map_or(0, |a| a.len()) }
fn arr_get(v: &Value, i: usize) -> Option<&Value> { v.as_array().and_then(|a| a.get(i)) }

// JS dot(a,b): accumulates over a.length (NOT min length — b[i] past its own end reads as
// `undefined` → NaN via ToNumber, poisoning the sum). Matches the module-private helper verbatim.
fn dot(a: &Value, b: &Value) -> f64 {
    let n = arr_len(a);
    let mut s = 0.0;
    for i in 0..n {
        s += js_number(arr_get(a, i)) * js_number(arr_get(b, i));
    }
    s
}
fn magnitude(v: &Value) -> f64 {
    let n = arr_len(v);
    let mut s = 0.0;
    for i in 0..n {
        let x = js_number(arr_get(v, i));
        s += x * x;
    }
    s.sqrt()
}

/// JS named export `cosineSimilarity(a, b)` — 0 for either zero-magnitude vector (no /0).
pub fn cosineSimilarity(a: &Value, b: &Value) -> Value {
    let ma = magnitude(a);
    let mb = magnitude(b);
    if ma == 0.0 || mb == 0.0 {
        return json!(0);
    }
    json!(dot(a, b) / (ma * mb))
}

struct MatchRow {
    map: Map<String, Value>,
    dims: i64,
    bytes: Vec<u8>,
    memory_id: Value,
    model: Value,
    embedded_field: Value,
}

pub fn memorySimilar(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('memory-similar','db') when no db — the db lane always injects one.
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));

    let vector_val = get("vector").cloned().unwrap_or(Value::Null);
    if !vector_val.is_array() || arr_len(&vector_val) == 0 {
        return Err("memory-similar: vector (non-empty array) is required".into());
    }
    if !truthy(get("model")) {
        return Err("memory-similar: model is required".into());
    }
    let model = get("model").cloned().unwrap();

    // Defaults apply ONLY when the key is ABSENT (JS destructuring default) — an explicit
    // null/0/'' present in input overrides the default and flows through as-is.
    let embedded_field = match get("embedded_field") { None => json!("situation"), Some(v) => v.clone() };
    let limit = match get("limit") { None => 10.0, Some(v) => js_number(Some(v)) };
    let min_score = match get("minScore") { None => 0.0, Some(v) => js_number(Some(v)) };
    let active_only = match get("activeOnly") { None => true, Some(v) => truthy(Some(v)) };

    let query_dims = arr_len(&vector_val) as i64;

    let sql = format!(
        "SELECT e.id AS embedding_id, e.memory_id, e.model, e.dims, e.vector, e.embedded_field, m.* \
         FROM memory.embeddings e JOIN memory.memories m ON m.id = e.memory_id \
         WHERE e.model = ? AND e.embedded_field = ?{}",
        if active_only { " AND m.superseded_by IS NULL" } else { "" }
    );

    let mut stmt = db.prepare(&sql).map_err(|e| e.to_string())?;
    let col_names: Vec<String> =
        (0..stmt.column_count()).map(|i| stmt.column_name(i).unwrap_or("").to_string()).collect();
    let idx = |name: &str| {
        col_names.iter().position(|n| n == name).ok_or_else(|| format!("memory-similar: column '{name}' missing from row"))
    };
    let vector_idx = idx("vector")?;
    let dims_idx = idx("dims")?;
    let memory_id_idx = idx("memory_id")?;
    let model_col_idx = idx("model")?;
    let embedded_field_idx = idx("embedded_field")?;

    let rows: Vec<MatchRow> = {
        let mapped = stmt
            .query_map(rusqlite::params![to_sql_param(&model), to_sql_param(&embedded_field)], |row| {
                let mut map = Map::new();
                for (i, name) in col_names.iter().enumerate() {
                    if i == vector_idx {
                        continue; // `{ vector: _, ...memory } = row` strips the blob
                    }
                    map.insert(name.clone(), cell(row.get_ref(i)?));
                }
                Ok(MatchRow {
                    map,
                    dims: row.get(dims_idx)?,
                    bytes: js_new_uint8array(row.get_ref(vector_idx)?),
                    memory_id: cell(row.get_ref(memory_id_idx)?),
                    model: cell(row.get_ref(model_col_idx)?),
                    embedded_field: cell(row.get_ref(embedded_field_idx)?),
                })
            })
            .map_err(|e| e.to_string())?;
        mapped.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
    };

    let mut scored: Vec<(f64, Value)> = Vec::new();
    for row in rows {
        if row.dims != query_dims {
            continue; // skip mismatched dims silently
        }
        let blob_json = json!(row.bytes.iter().map(|b| json!(*b as i64)).collect::<Vec<_>>());
        let stored = crate::entities_memory_embed_store::unpackFloat32(&blob_json, &json!(row.dims));
        let score_v = cosineSimilarity(&vector_val, &stored);
        let score = score_v.as_f64().unwrap_or(f64::NAN);
        if score < min_score {
            continue;
        }
        scored.push((
            score,
            json!({
                "memory_id": row.memory_id,
                "score": score_v,
                "embedded_field": row.embedded_field,
                "model": row.model,
                "memory": Value::Object(row.map),
            }),
        ));
    }
    // JS `.sort((a,b) => b.score - a.score)`: stable descending (a NaN comparator result is
    // treated as 'equal' by V8, keeping relative order — mirrored by unwrap_or(Equal)).
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let end = array_slice_end(scored.len(), limit);
    let out: Vec<Value> = scored.into_iter().take(end).map(|(_, v)| v).collect();
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
             );",
        )
        .unwrap();
        c
    }

    fn add_memory(c: &Connection, title: &str) -> i64 {
        c.execute(
            "INSERT INTO memory.memories (type, title, situation, guidance, created_by) VALUES ('lesson', ?, 's', 'g', 'tester')",
            rusqlite::params![title],
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

    #[test]
    fn ranks_by_similarity_desc_and_strips_vector() {
        let c = db();
        let a = add_memory(&c, "A");
        let b = add_memory(&c, "B");
        let cc = add_memory(&c, "C");
        store(&c, a, "M", &[1.0, 0.0, 0.0]);
        store(&c, b, "M", &[0.9, 0.1, 0.0]);
        store(&c, cc, "M", &[0.0, 0.0, 1.0]);

        let r = memorySimilar(&json!({ "vector": [1.0, 0.0, 0.0], "model": "M", "limit": 3 }), &c).unwrap();
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["memory_id"], json!(a));
        assert_eq!(arr[1]["memory_id"], json!(b));
        assert_eq!(arr[2]["memory_id"], json!(cc));
        assert!(arr[0]["score"].as_f64().unwrap() > arr[1]["score"].as_f64().unwrap());
        assert!(arr[1]["score"].as_f64().unwrap() > arr[2]["score"].as_f64().unwrap());
        assert_eq!(arr[0]["memory"]["title"], json!("A"));
        assert!(arr[0]["memory"].get("vector").is_none()); // stripped
        assert_eq!(arr[0]["memory"]["memory_id"], json!(a)); // the odd-but-real JS duplication
    }

    #[test]
    fn respects_limit() {
        let c = db();
        let a = add_memory(&c, "A");
        let b = add_memory(&c, "B");
        store(&c, a, "M", &[1.0, 0.0]);
        store(&c, b, "M", &[0.0, 1.0]);
        let r = memorySimilar(&json!({ "vector": [1.0, 0.0], "model": "M", "limit": 1 }), &c).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 1);
    }

    #[test]
    fn excludes_superseded_by_default_and_activeOnly_false_includes() {
        let c = db();
        let a = add_memory(&c, "A");
        let b = add_memory(&c, "B");
        store(&c, a, "M", &[1.0, 0.0]);
        store(&c, b, "M", &[1.0, 0.0]);
        c.execute("UPDATE memory.memories SET superseded_by = 999 WHERE id = ?", rusqlite::params![b]).unwrap();

        let r = memorySimilar(&json!({ "vector": [1.0, 0.0], "model": "M" }), &c).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 1);

        let r2 = memorySimilar(&json!({ "vector": [1.0, 0.0], "model": "M", "activeOnly": false }), &c).unwrap();
        assert_eq!(r2.as_array().unwrap().len(), 2);
    }

    #[test]
    fn respects_min_score_and_model_and_embedded_field_filters() {
        let c = db();
        let a = add_memory(&c, "A");
        store(&c, a, "M", &[1.0, 0.0]);
        assert_eq!(memorySimilar(&json!({ "vector": [1.0, 0.0], "model": "M", "minScore": 1.5 }), &c).unwrap().as_array().unwrap().len(), 0);
        assert_eq!(memorySimilar(&json!({ "vector": [1.0, 0.0], "model": "NOPE" }), &c).unwrap().as_array().unwrap().len(), 0);
        assert_eq!(memorySimilar(&json!({ "vector": [1.0, 0.0], "model": "M", "embedded_field": "guidance" }), &c).unwrap().as_array().unwrap().len(), 0);
    }

    #[test]
    fn skips_mismatched_dims_silently() {
        let c = db();
        let a = add_memory(&c, "A");
        store(&c, a, "M", &[1.0, 0.0, 0.0]); // dims 3
        let r = memorySimilar(&json!({ "vector": [1.0, 0.0], "model": "M" }), &c).unwrap(); // query dims 2
        assert_eq!(r.as_array().unwrap().len(), 0);
    }

    #[test]
    fn rejects_empty_vector_and_missing_model() {
        let c = db();
        assert!(memorySimilar(&json!({ "vector": [], "model": "M" }), &c).is_err());
        assert!(memorySimilar(&json!({ "vector": [1.0], "model": "" }), &c).is_err());
    }

    #[test]
    fn cosine_similarity_matches_js_semantics() {
        let cos = |a: Value, b: Value| cosineSimilarity(&a, &b).as_f64().unwrap();
        assert_eq!(cos(json!([1, 0, 0]), json!([1, 0, 0])), 1.0);
        assert_eq!(cos(json!([1, 0]), json!([0, 1])), 0.0);
        assert_eq!(cos(json!([1, 0]), json!([-1, 0])), -1.0);
        assert_eq!(cos(json!([0, 0]), json!([1, 0])), 0.0); // zero-magnitude avoids NaN
    }
}
