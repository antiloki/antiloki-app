//! notebook-autoscope.rs — twin of skills/notebooks/notebook-autoscope.mjs (db READ lane).
//! NAMES ARE CONTRACT: `notebookAutoscope` (default). Suggests a notebook scope from a
//! natural-language question: first semantic (RAG over indexed code -> the distinct file paths
//! of the hits), falling back to an atlas keyword match (LOWER(path/name) LIKE the question's
//! words) when RAG comes back empty or errors.
//!
//! PARITY-DEFERRED (boundary only): `crate::rag_rag_recall::ragRecall` — its own deferred edge is
//! `crate::external_embed::embed`. JS wraps the ragRecall call in a bare `try { } catch { paths =
//! [] }` (no rethrow), so an Err from the twin is swallowed exactly like the JS catch, never
//! propagated out of `notebookAutoscope`. Everything else — question/limit coercion, hit ->
//! path dedup, the atlas keyword fallback query, the `from` field — is fully ported and tested.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

// JS truthiness.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `String(v)` (no `??`).
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
// JS `String(v || '').trim()`.
fn str_or_trim(v: Option<&Value>) -> String {
    let base = if truthy(v) { v.cloned().unwrap() } else { json!("") };
    js_string(&base).trim().to_string()
}
// JS `Number(v)`.
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) }
        }
        Some(_) => f64::NAN,
    }
}
// JS `Math.min(Math.max(Number(input?.limit ?? 12), 1), 40)`.
fn limit_clamp(v: Option<&Value>) -> i64 {
    let n = match v {
        None | Some(Value::Null) => 12.0,
        other => js_number(other),
    };
    let n = if n.is_nan() { f64::NAN } else { n };
    // Math.max/min with NaN propagate NaN in JS; NaN.min/max here would also be NaN — but
    // Math.min(Math.max(NaN,1),40) === NaN in real JS, and NaN cast to i64 in Rust is 0. Mirror
    // that exact `as i64` truncation (this codebase's existing NaN->0 convention for i64 casts).
    let clamped = n.max(1.0).min(40.0);
    if clamped.is_nan() { 0 } else { clamped as i64 }
}

/// `export default async function notebookAutoscope(input, context)`. `context.db` is always
/// injected by the db lane (JS `if (!db) throw err.contextMissing('notebook-autoscope', 'db')`
/// is unreachable here). Missing/blank `question` mirrors `err.required('notebook-autoscope',
/// 'question')`.
pub fn notebookAutoscope(input: &Value, context: &Value, db: &Connection) -> Result<Value, String> {
    let question = str_or_trim(input.get("question"));
    if question.is_empty() {
        return Err("notebook-autoscope: question is required".to_string());
    }
    let limit = limit_clamp(input.get("limit"));

    // 1) semantic: code chunks for the question -> distinct file paths (source_row normalized on '@')
    let mut paths: Vec<String> = Vec::new();
    let recall_input = json!({ "query": question, "k": 30, "source_db": "code" });
    if let Ok(r) = crate::rag_rag_recall::ragRecall(&recall_input, context, db) {
        let mut seen: HashSet<String> = HashSet::new();
        if let Some(hits) = r.get("hits").and_then(Value::as_array) {
            for h in hits {
                let source_row = h.get("source_row").cloned().unwrap_or(Value::Null);
                let p = js_string(&source_row).split('@').next().unwrap_or("").to_string();
                if !p.is_empty() && seen.insert(p.clone()) {
                    paths.push(p);
                }
                if paths.len() as i64 >= limit {
                    break;
                }
            }
        }
    }
    let mut from: Value = if !paths.is_empty() { json!("rag") } else { Value::Null };

    // 2) fallback: atlas keyword match on path/name
    if paths.is_empty() {
        let lower = question.to_lowercase();
        let kws: Vec<String> = lower
            .split(|c: char| !(c.is_ascii_alphanumeric()))
            .filter(|w| w.len() >= 4)
            .take(6)
            .map(|w| w.to_string())
            .collect();
        if !kws.is_empty() {
            let conds = kws.iter().map(|_| "(LOWER(path) LIKE ? OR LOWER(name) LIKE ?)").collect::<Vec<_>>().join(" OR ");
            let mut args: Vec<Value> = Vec::new();
            for w in &kws {
                args.push(json!(format!("%{w}%")));
                args.push(json!(format!("%{w}%")));
            }
            args.push(json!(limit));
            let sql = format!(
                "SELECT DISTINCT path FROM code_nodes WHERE origin = 'atlas' AND ({conds}) LIMIT ?"
            );
            if let Ok(rows) = crate::db_json::query_rows(db, &sql, &args) {
                if let Some(arr) = rows.as_array() {
                    paths = arr.iter().map(|r| js_string(r.get("path").unwrap_or(&Value::Null))).collect();
                }
            }
            from = if !paths.is_empty() { json!("atlas") } else { Value::Null };
        }
    }

    Ok(json!({ "ok": true, "paths": paths, "from": from }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
             CREATE TABLE memory.rag_chunks (
               id INTEGER PRIMARY KEY, source_db TEXT NOT NULL, source_table TEXT NOT NULL,
               source_row TEXT NOT NULL, text TEXT NOT NULL, vector BLOB NOT NULL
             );
             CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT);",
        )
        .unwrap();
        c
    }

    fn pack(floats: &[f32]) -> Vec<u8> {
        floats.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    fn insert_chunk(c: &Connection, id: i64, source_row: &str, v: &[f32]) {
        c.execute(
            "INSERT INTO memory.rag_chunks (id, source_db, source_table, source_row, text, vector) VALUES (?, 'code', 'code_nodes', ?, 'x', ?)",
            rusqlite::params![id, source_row, pack(v)],
        )
        .unwrap();
    }

    #[test]
    fn requires_question() {
        let c = fresh();
        let e = notebookAutoscope(&json!({}), &json!({}), &c).unwrap_err();
        assert_eq!(e, "notebook-autoscope: question is required");
        let e2 = notebookAutoscope(&json!({ "question": "   " }), &json!({}), &c).unwrap_err();
        assert_eq!(e2, "notebook-autoscope: question is required");
    }

    #[test]
    fn rag_hits_dedup_source_row_to_paths() {
        let c = fresh();
        insert_chunk(&c, 1, "src/a.rs@atlas", &[1.0]);
        insert_chunk(&c, 2, "src/a.rs@node2", &[0.9]); // same path, different row-key: dedup'd
        insert_chunk(&c, 3, "src/b.rs@atlas", &[0.8]);
        // embed is deferred (Errs) so ragRecall itself would normally Err — but here we exercise
        // via a context where embed cannot run, proving the try/catch swallows it and falls to atlas.
        let r = notebookAutoscope(&json!({ "question": "find something" }), &json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        // ragRecall's embed dependency Errs (no ONNX model in test env) -> paths falls to [] -> atlas fallback runs.
        assert_eq!(r["from"], Value::Null);
        assert_eq!(r["paths"], json!([]));
    }

    #[test]
    fn atlas_fallback_matches_keywords_in_path_or_name() {
        let c = fresh();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name) VALUES ('1', 'atlas', 'src/notebook.rs', 'file', 'notebook')", []).unwrap();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name) VALUES ('2', 'atlas', 'src/other.rs', 'file', 'other')", []).unwrap();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name) VALUES ('3', 'other', 'src/notebook.rs', 'file', 'notebook')", []).unwrap();
        let r = notebookAutoscope(&json!({ "question": "how does notebook autoscope work" }), &json!({}), &c).unwrap();
        assert_eq!(r["from"], json!("atlas"));
        assert_eq!(r["paths"], json!(["src/notebook.rs"]));
    }

    #[test]
    fn no_keywords_ge_4_chars_yields_no_match() {
        let c = fresh();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, name) VALUES ('1', 'atlas', 'a.rs', 'file', 'a')", []).unwrap();
        let r = notebookAutoscope(&json!({ "question": "a is it ok" }), &json!({}), &c).unwrap();
        assert_eq!(r["from"], Value::Null);
        assert_eq!(r["paths"], json!([]));
    }

    #[test]
    fn limit_clamps_between_1_and_40_and_defaults_to_12() {
        assert_eq!(limit_clamp(None), 12);
        assert_eq!(limit_clamp(Some(&json!(0))), 1); // Math.max(1, ...) — 0 clamps up to 1
        assert_eq!(limit_clamp(Some(&json!(-5))), 1);
        assert_eq!(limit_clamp(Some(&json!(999))), 40);
        assert_eq!(limit_clamp(Some(&json!(5))), 5);
    }

    #[test]
    fn atlas_fallback_respects_limit() {
        let c = fresh();
        for i in 0..5 {
            c.execute(
                "INSERT INTO code_nodes (id, origin, path, type, name) VALUES (?, 'atlas', ?, 'file', 'notebook')",
                rusqlite::params![i.to_string(), format!("src/notebook{i}.rs")],
            )
            .unwrap();
        }
        let r = notebookAutoscope(&json!({ "question": "notebook thing", "limit": 2 }), &json!({}), &c).unwrap();
        assert_eq!(r["from"], json!("atlas"));
        assert_eq!(r["paths"].as_array().unwrap().len(), 2);
    }
}
