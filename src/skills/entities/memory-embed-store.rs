//! memory-embed-store.rs — twin of skills/entities/memory-embed-store.mjs (db WRITE lane,
//! attached `memory` db). NAMES ARE CONTRACT: `memoryEmbedStore` (default), `unpackFloat32`
//! (named, no-db). Stores a pre-computed embedding vector for a memory as a float32 LE BLOB
//! (4 bytes/dim); UNIQUE(memory_id, model, embedded_field) — re-storing the same triple UPDATEs
//! the existing row in place instead of inserting a duplicate.
use rusqlite::Connection;
use serde_json::{json, Value};

// Number.isInteger(v): integer-VALUED numbers only (strings/bools/null/absent all fail).
fn is_js_integer(v: &Value) -> bool {
    match v {
        Value::Number(n) => {
            n.as_i64().is_some()
                || n.as_u64().is_some()
                || n.as_f64().map_or(false, |f| f.is_finite() && f.fract() == 0.0)
        }
        _ => false,
    }
}
fn as_js_i64(v: &Value) -> i64 {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).unwrap_or(0)
}
// Mirrors db_json::to_sql (that fn is private to its module) for the one bound param
// (embedded_field) that isn't already a native Rust type by the time we bind it.
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

// vector is packed as float32 LE bytes for compact storage (4 bytes/dim) — module-private
// helper in the JS source too (not exported, not a census unit).
fn pack_float32(vec: &[f64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vec.len() * 4);
    for v in vec {
        out.extend_from_slice(&(*v as f32).to_le_bytes());
    }
    out
}

/// node:sqlite hands back a BLOB column as a Uint8Array; other drivers may return a Buffer or a
/// plain byte array — the fixture models that second case as a JSON array of byte values.
pub fn unpackFloat32(blob: &Value, dims: &Value) -> Value {
    let bytes: Vec<u8> = blob
        .as_array()
        .map(|a| a.iter().map(|v| v.as_u64().unwrap_or(0) as u8).collect())
        .unwrap_or_default();
    let n = dims.as_i64().unwrap_or(0).max(0) as usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let off = i * 4;
        let f = f32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]);
        out.push(json!(f as f64));
    }
    json!(out)
}

pub fn memoryEmbedStore(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('memory-embed-store','db') when no db — the db lane always
    // injects one, so that guard is unreachable here.
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k)).cloned().unwrap_or(Value::Null);

    let memory_id = get("memory_id");
    let model = get("model");
    let vector = get("vector");
    // embedded_field defaults to 'situation' only when the key is ABSENT (JS destructuring
    // default fires on undefined, not on an explicit null — explicit null stays null).
    let embedded_field = match obj.and_then(|o| o.get("embedded_field")) {
        None => json!("situation"),
        Some(v) => v.clone(),
    };

    if !is_js_integer(&memory_id) {
        return Err("memory-embed-store: memory_id (integer) is required".into());
    }
    let model_str = match &model {
        // !model || typeof model !== 'string' — passes only for a non-empty string.
        Value::String(s) if !s.is_empty() => s.clone(),
        _ => return Err("memory-embed-store: model (string) is required".into()),
    };
    let vec_arr = match vector.as_array() {
        Some(a) if !a.is_empty() => a.clone(),
        _ => return Err("memory-embed-store: vector must be a non-empty array of numbers".into()),
    };
    let mut nums: Vec<f64> = Vec::with_capacity(vec_arr.len());
    for v in &vec_arr {
        let f = match v {
            Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
            _ => f64::NAN, // typeof v !== 'number'
        };
        if !f.is_finite() {
            return Err("memory-embed-store: vector must contain only finite numbers".into());
        }
        nums.push(f);
    }

    let memory_id_i64 = as_js_i64(&memory_id);
    let mem_rows = crate::db_json::query_rows(db, "SELECT id FROM memory.memories WHERE id = ?", &[json!(memory_id_i64)])?;
    if mem_rows.as_array().map_or(true, |a| a.is_empty()) {
        return Err(format!("memory-embed-store: memory id '{memory_id_i64}' not found"));
    }

    let dims = nums.len() as i64;
    let blob = pack_float32(&nums);

    db.execute(
        &format!("INSERT INTO memory.embeddings (memory_id, model, dims, vector, embedded_field)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(memory_id, model, embedded_field)
         DO UPDATE SET vector = excluded.vector, dims = excluded.dims, created_at = {}", crate::clock::now_sql()),
        rusqlite::params![memory_id_i64, model_str, dims, blob, to_sql_param(&embedded_field)],
    )
    .map_err(|e| e.to_string())?;

    let out = crate::db_json::query_rows(
        db,
        "SELECT id, memory_id, model, dims, embedded_field, created_at
         FROM memory.embeddings WHERE memory_id = ? AND model = ? AND embedded_field = ?",
        &[json!(memory_id_i64), json!(model_str), embedded_field],
    )?;
    Ok(out.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
             CREATE TABLE memory.memories ( id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT );
             CREATE TABLE memory.embeddings (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               memory_id INTEGER NOT NULL,
               model TEXT NOT NULL,
               dims INTEGER NOT NULL,
               vector BLOB NOT NULL,
               embedded_field TEXT NOT NULL DEFAULT 'situation',
               created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
               UNIQUE (memory_id, model, embedded_field),
               FOREIGN KEY (memory_id) REFERENCES memories(id)
             );
             INSERT INTO memory.memories (id, title) VALUES (1, 'm1');",
        )
        .unwrap();
        c
    }

    #[test]
    fn stores_a_vector_and_returns_metadata() {
        let c = db();
        let r = memoryEmbedStore(&json!({ "memory_id": 1, "model": "voyage-3", "vector": [0.1, 0.2, 0.3] }), &c).unwrap();
        assert_eq!(r["memory_id"], json!(1));
        assert_eq!(r["model"], json!("voyage-3"));
        assert_eq!(r["dims"], json!(3));
        assert_eq!(r["embedded_field"], json!("situation"));
        assert_eq!(r["created_at"], json!("2026-01-01 00:00:00"));
    }

    #[test]
    fn round_trips_vector_via_unpack_float32() {
        let c = db();
        let original = [0.1, -0.5, 0.7, 1.0, -1.0, 0.0];
        memoryEmbedStore(&json!({ "memory_id": 1, "model": "test-6d", "vector": original }), &c).unwrap();
        let (vector, dims): (Vec<u8>, i64) = c
            .query_row("SELECT vector, dims FROM memory.embeddings WHERE memory_id=1", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        let blob_json = json!(vector.iter().map(|b| json!(*b as i64)).collect::<Vec<_>>());
        let unpacked = unpackFloat32(&blob_json, &json!(dims));
        let arr = unpacked.as_array().unwrap();
        for (i, v) in original.iter().enumerate() {
            let got = arr[i].as_f64().unwrap();
            assert!((got - v).abs() < 1e-6, "index {i}: expected ~{v}, got {got}");
        }
    }

    #[test]
    fn restoring_same_triple_updates_not_inserts() {
        let c = db();
        memoryEmbedStore(&json!({ "memory_id": 1, "model": "M1", "vector": [1, 0, 0] }), &c).unwrap();
        memoryEmbedStore(&json!({ "memory_id": 1, "model": "M1", "vector": [0, 1, 0] }), &c).unwrap();
        let count: i64 = c.query_row("SELECT COUNT(*) FROM memory.embeddings WHERE memory_id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn different_embedded_field_stores_separately() {
        let c = db();
        memoryEmbedStore(&json!({ "memory_id": 1, "model": "M", "vector": [1, 0], "embedded_field": "situation" }), &c).unwrap();
        memoryEmbedStore(&json!({ "memory_id": 1, "model": "M", "vector": [0, 1], "embedded_field": "guidance" }), &c).unwrap();
        let count: i64 = c.query_row("SELECT COUNT(*) FROM memory.embeddings WHERE memory_id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn rejects_nonexistent_memory_id() {
        let c = db();
        let e = memoryEmbedStore(&json!({ "memory_id": 9999, "model": "M", "vector": [1] }), &c).unwrap_err();
        assert_eq!(e, "memory-embed-store: memory id '9999' not found");
    }

    #[test]
    fn rejects_empty_vector() {
        let c = db();
        let e = memoryEmbedStore(&json!({ "memory_id": 1, "model": "M", "vector": [] }), &c).unwrap_err();
        assert!(e.contains("non-empty"));
    }

    #[test]
    fn rejects_missing_model() {
        let c = db();
        let e = memoryEmbedStore(&json!({ "memory_id": 1, "vector": [1] }), &c).unwrap_err();
        assert_eq!(e, "memory-embed-store: model (string) is required");
    }

    #[test]
    fn rejects_non_integer_memory_id() {
        let c = db();
        let e = memoryEmbedStore(&json!({ "memory_id": "1", "model": "M", "vector": [1] }), &c).unwrap_err();
        assert_eq!(e, "memory-embed-store: memory_id (integer) is required");
    }

    #[test]
    fn unpack_float32_zero_dims_is_empty() {
        assert_eq!(unpackFloat32(&json!([]), &json!(0)), json!([]));
    }
}
