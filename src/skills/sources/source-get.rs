//! source-get.rs — twin of skills/sources/source-get.mjs (db READ lane). NAMES ARE CONTRACT:
//! `sourceGet` (default). One source's metadata + absolute path, by id.
// QUALITY: 80/100 — SELECT by id + one row-shape decoration (abs_path), same shape as
// source-list.rs's per-row decoration but for a single row. The JS `dir` is
// `context?.sourcesDir ?? SOURCES_DIR`; the crossing's db-lane fixture context carries only `db`
// (law-add.rs / source-list.rs / source-read.rs precedent), so `dir` always resolves to the
// SOURCES_DIR default in every fixture run — reproduced below by anchoring off
// CARGO_MANIFEST_DIR exactly like source-list.rs and source-read.rs, since antiloki-core-rust and
// antiloki-core are siblings under the same SUBSTRATE_ROOT.
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

fn sources_dir() -> std::path::PathBuf {
    // JS: SOURCES_DIR = join(SUBSTRATE_ROOT, '.antiloki', 'sources'), anchored at roots.mjs's own
    // file location (antiloki-core's parent). CARGO_MANIFEST_DIR here is antiloki-core-rust; its
    // parent is that same SUBSTRATE_ROOT on this checkout layout (source-list.rs precedent).
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .join(".antiloki")
        .join("sources")
}

pub fn sourceGet(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::sources_sources_schema::ensureSourcesSchema(db, &json!("knowledge."));
    // JS: if (!db) throw err.contextMissing('source-get','db') — unreachable in the db lane (a
    // connection is always injected).

    // JS: const id = String(input?.id || '').trim();
    let raw_id = input.get("id");
    let id_str = if truthy(raw_id) { js_string(raw_id.unwrap()) } else { String::new() };
    let id = id_str.trim().to_string();
    if id.is_empty() {
        return Err("source-get: id is required".into());
    }

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, filename, rel_path, ext, mime, sha256, bytes, title, added_at FROM knowledge.sources WHERE id = ?",
        &[json!(id.clone())],
    )?;
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Ok(json!({ "ok": false, "error": format!("source '{id}' not found") })),
    };

    let mut o = row.as_object().cloned().unwrap_or_default();
    let rel = o.get("rel_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let abs = sources_dir().join(&rel).to_string_lossy().into_owned();
    o.insert("abs_path".into(), json!(abs));

    Ok(json!({ "ok": true, "source": Value::Object(o) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_knowledge_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS knowledge").unwrap();
        crate::sources_sources_schema::ensureSourcesSchema(&c, &json!("knowledge."));
        c
    }

    #[test]
    fn id_required() {
        let c = open_knowledge_db();
        assert_eq!(sourceGet(&json!({}), &c).unwrap_err(), "source-get: id is required");
        assert_eq!(sourceGet(&json!({ "id": "   " }), &c).unwrap_err(), "source-get: id is required");
    }

    #[test]
    fn not_found_returns_ok_false() {
        let c = open_knowledge_db();
        let r = sourceGet(&json!({ "id": "src-missing" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "source 'src-missing' not found" }));
    }

    #[test]
    fn found_returns_metadata_with_abs_path() {
        let c = open_knowledge_db();
        c.execute_batch(
            "INSERT INTO knowledge.sources (id, filename, rel_path, ext, mime, sha256, bytes, text, title, added_at, added_by) \
             VALUES ('src-aaa','a.md','src-aaa.md','.md','text/markdown','sha-a',5,'hello','A','2026-01-01 00:00:00','operator')",
        )
        .unwrap();
        let r = sourceGet(&json!({ "id": "src-aaa" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let s = &r["source"];
        assert_eq!(s["id"], json!("src-aaa"));
        assert_eq!(s["filename"], json!("a.md"));
        assert_eq!(s["rel_path"], json!("src-aaa.md"));
        assert_eq!(s["ext"], json!(".md"));
        assert_eq!(s["mime"], json!("text/markdown"));
        assert_eq!(s["sha256"], json!("sha-a"));
        assert_eq!(s["bytes"], json!(5));
        assert_eq!(s["title"], json!("A"));
        assert_eq!(s["added_at"], json!("2026-01-01 00:00:00"));
        assert!(s["abs_path"].as_str().unwrap().ends_with("src-aaa.md"));
        // no `text` column in this row shape — matches the JS SELECT list (no `text`)
        assert!(s.get("text").is_none());
    }
}
