//! source-read.rs — twin of skills/sources/source-read.mjs (db READ lane, with an fs fallback).
//! NAMES ARE CONTRACT: `sourceRead` (default). A source's text content by id.
// QUALITY: 78/100 — SELECT by id + a disk-read fallback when the `text` column is NULL. The JS
// fallback dir is `context?.sourcesDir ?? SOURCES_DIR`; the crossing's db-lane fixture context
// carries only `db` (law-add.rs / source-list.rs precedent), so `sourcesDir` always resolves to
// the real SOURCES_DIR default here too — anchored off CARGO_MANIFEST_DIR exactly like
// source-list.rs, since antiloki-core-rust and antiloki-core are siblings under the same
// SUBSTRATE_ROOT. Both lanes therefore read the SAME real directory on the SAME machine, so the
// fallback is deterministic even though no fixture drives it directly; fixtures here only cover
// rows whose `text` column is already populated (or missing rows), matching the "fixture only the
// reliably-testable path" precedent set by extract-text.rs's PDF scoping note.
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

pub fn sourceRead(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::sources_sources_schema::ensureSourcesSchema(db, &json!("knowledge."));
    // JS: if (!db) throw err.contextMissing('source-read','db') — unreachable in the db lane (a
    // connection is always injected).

    // JS: const id = String(input?.id || '').trim();
    let raw_id = input.get("id");
    let id_str = if truthy(raw_id) { js_string(raw_id.unwrap()) } else { String::new() };
    let id = id_str.trim().to_string();
    if id.is_empty() {
        return Err("source-read: id is required".into());
    }

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, filename, rel_path, ext, mime, bytes, text FROM knowledge.sources WHERE id = ?",
        &[json!(id.clone())],
    )?;
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Ok(json!({ "ok": false, "error": format!("source '{id}' not found") })),
    };

    let row_id = row.get("id").cloned().unwrap_or(Value::Null);
    let filename = row.get("filename").cloned().unwrap_or(Value::Null);
    let mime = row.get("mime").cloned().unwrap_or(Value::Null);
    let bytes = row.get("bytes").cloned().unwrap_or(Value::Null);
    let mut text = row.get("text").cloned().unwrap_or(Value::Null);

    if text.is_null() {
        let rel_path = row.get("rel_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let abs = sources_dir().join(&rel_path);
        // JS: try { if (existsSync(abs)) text = readFileSync(abs, 'utf8'); } catch { /* binary / unreadable */ }
        if abs.exists() {
            if let Ok(raw) = std::fs::read(&abs) {
                text = json!(String::from_utf8_lossy(&raw).into_owned());
            }
        }
    }

    let is_binary = text.is_null();
    Ok(json!({
        "ok": true,
        "id": row_id,
        "filename": filename,
        "mime": mime,
        "bytes": bytes,
        "text": text,
        "is_binary": is_binary,
    }))
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
        assert_eq!(sourceRead(&json!({}), &c).unwrap_err(), "source-read: id is required");
        assert_eq!(sourceRead(&json!({ "id": "   " }), &c).unwrap_err(), "source-read: id is required");
    }

    #[test]
    fn not_found_returns_ok_false() {
        let c = open_knowledge_db();
        let r = sourceRead(&json!({ "id": "src-missing" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "source 'src-missing' not found" }));
    }

    #[test]
    fn found_with_stored_text_returns_it_directly() {
        let c = open_knowledge_db();
        c.execute_batch(
            "INSERT INTO knowledge.sources (id, filename, rel_path, ext, mime, sha256, bytes, text, title, added_at, added_by) \
             VALUES ('src-aaa','a.md','src-aaa.md','.md','text/markdown','sha-a',5,'hello','A','2026-01-01 00:00:00','operator')",
        )
        .unwrap();
        let r = sourceRead(&json!({ "id": "src-aaa" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "ok": true, "id": "src-aaa", "filename": "a.md", "mime": "text/markdown", "bytes": 5, "text": "hello", "is_binary": false })
        );
    }

    #[test]
    fn found_with_null_text_and_missing_file_reports_is_binary() {
        let c = open_knowledge_db();
        c.execute_batch(
            "INSERT INTO knowledge.sources (id, filename, rel_path, ext, mime, sha256, bytes, text, title, added_at, added_by) \
             VALUES ('src-ccc','c.png','src-ccc-does-not-exist-on-disk.png','.png','image/png','sha-c',20,NULL,'C','2026-01-01 00:00:00','operator')",
        )
        .unwrap();
        let r = sourceRead(&json!({ "id": "src-ccc" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["id"], json!("src-ccc"));
        assert_eq!(r["text"], Value::Null);
        assert_eq!(r["is_binary"], json!(true));
    }
}
