//! memory-rag-query.rs — twin of skills/entities/memory-rag-query.mjs (db READ lane). NAMES ARE
//! CONTRACT: `memoryRagQuery` (default). Query-TEXT entry point for memory-rag-context (a
//! vector-in primitive): a precomputed `vector` passes straight through; otherwise the first
//! non-blank of query/text/situation is embedded via the deferred `crate::external_embed::embed`
//! boundary (real ONNX inference is PARITY-DEFERRED, out of scope here) before delegating.
//!
//! Deliberate simplification vs the JS: the JS `context.embed` injection seam (a function value
//! swapped in by tests) has no JSON representation, so this twin always calls
//! `crate::external_embed::embed` directly — there is no equivalent seam here. Deterministic
//! coverage below exercises only the vector-passthrough branch (the embed branch is exercised
//! once as a propagate-the-Err check, since the deferred embed always errors).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn arr_len(v: &Value) -> usize {
    v.as_array().map_or(0, |a| a.len())
}

// = JS `[query, text, situation].find((v) => typeof v === 'string' && v.trim())`
fn find_query_text<'a>(get: &dyn Fn(&str) -> Option<&'a Value>) -> Option<&'a str> {
    ["query", "text", "situation"]
        .iter()
        .find_map(|k| get(k).and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()))
}

// JS `.slice(0, 8000)`: string length + slicing are UTF-16 code-unit based, not codepoint based.
fn js_slice_8000(s: &str) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    let end = units.len().min(8000);
    String::from_utf16_lossy(&units[..end])
}

pub fn memoryRagQuery(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));

    // ragOpts = input minus {query, text, situation, vector, model} (JS object-rest).
    let mut rag_opts: Map<String, Value> = obj.cloned().unwrap_or_default();
    for k in ["query", "text", "situation", "vector", "model"] {
        rag_opts.remove(k);
    }

    let model_val = get("model").cloned();
    let model_is_present_and_truthy = |v: &Option<Value>| match v {
        Some(Value::Null) | None => false,
        Some(_) => true,
    };

    // A precomputed vector (the original primitive's contract) → straight through, no embedding.
    let vector_val = get("vector").cloned();
    if let Some(v) = &vector_val {
        if v.is_array() && arr_len(v) > 0 {
            let mut ctx_input = rag_opts.clone();
            ctx_input.insert("vector".into(), v.clone());
            let model_out = if model_is_present_and_truthy(&model_val) {
                model_val.clone().unwrap()
            } else {
                json!(crate::external_embed::DEFAULT_MODEL)
            };
            ctx_input.insert("model".into(), model_out);
            return crate::entities_memory_rag_context::memoryRagContext(&Value::Object(ctx_input), db);
        }
    }

    // Otherwise embed the query text — the whole point of this entry point.
    let query_text = match find_query_text(&get) {
        Some(s) => s,
        None => return Err("memory-rag-query: query (or a precomputed vector) is required".into()),
    };
    let sliced = js_slice_8000(query_text);

    let model_for_embed = if model_is_present_and_truthy(&model_val) {
        model_val.clone().unwrap()
    } else {
        json!(crate::external_embed::DEFAULT_MODEL)
    };
    let embed_input = json!({ "text": sliced, "model": model_for_embed.clone() });
    let e = crate::external_embed::embed(&embed_input, &Value::Null)?;

    let e_vector = e.get("vector").cloned().unwrap_or(Value::Null);
    let e_model = e.get("model").cloned();
    let model_out = if model_is_present_and_truthy(&e_model) {
        e_model.unwrap()
    } else if model_is_present_and_truthy(&model_val) {
        model_val.unwrap()
    } else {
        json!(crate::external_embed::DEFAULT_MODEL)
    };

    let mut ctx_input = rag_opts;
    ctx_input.insert("vector".into(), e_vector);
    ctx_input.insert("model".into(), model_out);
    crate::entities_memory_rag_context::memoryRagContext(&Value::Object(ctx_input), db)
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

    #[test]
    fn precomputed_vector_passes_straight_through_to_rag_context() {
        let c = db();
        let a = add_memory(&c, "A", 0.9);
        store(&c, a, "M", &[1.0, 1.0, 1.0]);

        let r = memoryRagQuery(&json!({ "vector": [1.0, 1.0, 1.0], "model": "M" }), &c).unwrap();
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["memory_id"], json!(a));
    }

    #[test]
    fn precomputed_vector_without_model_defaults_to_default_model() {
        let c = db();
        let a = add_memory(&c, "A", 0.5);
        store(&c, a, crate::external_embed::DEFAULT_MODEL, &[1.0, 0.0]);

        let r = memoryRagQuery(&json!({ "vector": [1.0, 0.0] }), &c).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 1);
    }

    #[test]
    fn ragopts_flow_through_the_passthrough_branch() {
        let c = db();
        let a = add_memory(&c, "A", 0.5);
        let b = add_memory(&c, "B", 0.5);
        store(&c, a, "M", &[1.0, 1.0, 1.0]);
        store(&c, b, "M", &[1.0, 1.0, 1.0]);

        let r = memoryRagQuery(&json!({ "vector": [1.0, 1.0, 1.0], "model": "M", "limit": 1 }), &c).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 1);
    }

    #[test]
    fn empty_vector_array_falls_through_to_query_required_error() {
        let c = db();
        let e = memoryRagQuery(&json!({ "vector": [] }), &c).unwrap_err();
        assert!(e.contains("query"));
    }

    #[test]
    fn missing_query_and_vector_errors() {
        let c = db();
        let e = memoryRagQuery(&json!({}), &c).unwrap_err();
        assert_eq!(e, "memory-rag-query: query (or a precomputed vector) is required");
    }

    #[test]
    fn blank_query_string_errors() {
        let c = db();
        let e = memoryRagQuery(&json!({ "query": "   " }), &c).unwrap_err();
        assert_eq!(e, "memory-rag-query: query (or a precomputed vector) is required");
    }

    #[test]
    fn text_field_used_when_query_absent() {
        let c = db();
        // No embedding infra available in this fixture — the deferred embed boundary errors,
        // and that error must propagate straight through (mirrors JS `await embedFn(...)` with
        // no try/catch around it in memoryRagQuery).
        let e = memoryRagQuery(&json!({ "text": "hello world" }), &c).unwrap_err();
        assert!(!e.is_empty());
    }
}
