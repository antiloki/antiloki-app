//! source-remove.rs — twin of skills/sources/source-remove.mjs (default export `sourceRemove`,
//! standard db lane, cross-db: reads/writes `knowledge.sources`, best-effort deletes from
//! `memory.rag_chunks`). NAMES ARE CONTRACT.
//!
//! Deletes a registered source: the knowledge.sources row, a best-effort unlink of the real file
//! under .antiloki/sources/, and a best-effort drop of its RAG chunk in memory.rag_chunks.
//!
//! FS SIDE EFFECT NOTE: the real-file unlink is wrapped in try/catch in the JS and never
//! influences the returned value or any DB table — `db_after` (what conform actually compares)
//! cannot see it. The crossing db-lane fixture context carries only `db` (source-list.rs
//! precedent: no `context.sourcesDir` injection), so both lanes resolve the SAME on-disk default
//! path (SOURCES_DIR / rel_path) and, since fixtures never plant a matching real file there, both
//! lanes always take the `existsSync` false branch — the unlink is faithfully reproduced but
//! provably inert under conform.
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
// JS String(v) (no ??/|| involved once the truthy branch already picked a value)
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

// JS: SOURCES_DIR = join(SUBSTRATE_ROOT, '.antiloki', 'sources'). Same anchor as source-list.rs's
// sources_dir(): antiloki-core-rust and antiloki-core are siblings under the same SUBSTRATE_ROOT
// on this checkout layout, so CARGO_MANIFEST_DIR's parent resolves identically.
fn sources_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .join(".antiloki")
        .join("sources")
}

pub fn sourceRemove(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::sources_sources_schema::ensureSourcesSchema(db, &json!("knowledge."));

    // const id = String(input?.id || '').trim();
    let id_field = input.get("id");
    let id_base: Value = if truthy(id_field) { id_field.unwrap().clone() } else { json!("") };
    let id = js_string(&id_base).trim().to_string();
    if id.is_empty() {
        return Ok(json!({ "ok": false, "error": "id required" }));
    }

    let rows = crate::db_json::query_rows(db, "SELECT rel_path FROM knowledge.sources WHERE id = ?", &[json!(id)])?;
    let rel_path = match rows.as_array().and_then(|a| a.first()) {
        None => return Ok(json!({ "ok": false, "error": format!("source '{}' not found", id) })),
        Some(r) => r.get("rel_path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    };

    // try { if (existsSync(abs)) unlinkSync(abs); } catch { /* best-effort */ }
    let abs = sources_dir().join(&rel_path);
    if abs.exists() {
        let _ = std::fs::remove_file(&abs);
    }

    crate::db_json::run_write(db, "DELETE FROM knowledge.sources WHERE id = ?", &[json!(id)])?;
    // try { db.prepare(...).run(id) } catch { /* rag may be absent */ }
    let _ = crate::db_json::run_write(
        db,
        "DELETE FROM memory.rag_chunks WHERE source_db='knowledge' AND source_table='sources' AND source_row=?",
        &[json!(id)],
    );

    Ok(json!({ "ok": true, "removed": true, "id": id }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS knowledge; ATTACH ':memory:' AS memory;").unwrap();
        c.execute_batch(
            "CREATE TABLE knowledge.sources (id TEXT PRIMARY KEY, filename TEXT NOT NULL, rel_path TEXT NOT NULL UNIQUE, \
             ext TEXT, mime TEXT, sha256 TEXT NOT NULL, bytes INTEGER NOT NULL, text TEXT, title TEXT, \
             added_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', added_by TEXT NOT NULL DEFAULT 'operator');
             CREATE TABLE memory.rag_chunks (id INTEGER PRIMARY KEY AUTOINCREMENT, source_db TEXT NOT NULL, \
             source_table TEXT NOT NULL, source_row TEXT NOT NULL, chunk_ix INTEGER NOT NULL DEFAULT 0, \
             text TEXT NOT NULL, content_hash TEXT NOT NULL, vector BLOB NOT NULL, model TEXT NOT NULL, \
             dim INTEGER NOT NULL, updated_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');",
        )
        .unwrap();
        c
    }

    #[test]
    fn missing_id_returns_required_error() {
        let c = open_db();
        let r = sourceRemove(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "id required" }));
    }

    #[test]
    fn whitespace_only_id_trims_to_empty_and_is_required() {
        let c = open_db();
        let r = sourceRemove(&json!({ "id": "   " }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "id required" }));
    }

    #[test]
    fn falsy_numeric_id_zero_is_treated_as_required() {
        // String(0 || '') === '' — 0 is falsy, so the || substitutes ''
        let c = open_db();
        let r = sourceRemove(&json!({ "id": 0 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "id required" }));
    }

    #[test]
    fn unknown_id_returns_not_found() {
        let c = open_db();
        let r = sourceRemove(&json!({ "id": "ghost" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "source 'ghost' not found" }));
    }

    #[test]
    fn removes_source_row_and_its_own_rag_chunk_only() {
        let c = open_db();
        c.execute_batch(
            "INSERT INTO knowledge.sources (id, filename, rel_path, sha256, bytes) VALUES \
             ('src-1', 'a.md', 'sources/a.md', 'sha-a', 10), \
             ('src-2', 'b.md', 'sources/b.md', 'sha-b', 20);
             INSERT INTO memory.rag_chunks (source_db, source_table, source_row, text, content_hash, vector, model, dim) VALUES \
             ('knowledge', 'sources', 'src-1', 'chunk one', 'hash1', x'00', 'ollama:nomic-embed-text', 1), \
             ('knowledge', 'sources', 'src-2', 'chunk two', 'hash2', x'00', 'ollama:nomic-embed-text', 1);",
        )
        .unwrap();

        let r = sourceRemove(&json!({ "id": " src-1 " }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "removed": true, "id": "src-1" }));

        let remaining_sources: Vec<String> = c
            .prepare("SELECT id FROM knowledge.sources ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|x| x.unwrap())
            .collect();
        assert_eq!(remaining_sources, vec!["src-2".to_string()]);

        let remaining_chunks: Vec<String> = c
            .prepare("SELECT source_row FROM memory.rag_chunks ORDER BY source_row")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|x| x.unwrap())
            .collect();
        assert_eq!(remaining_chunks, vec!["src-2".to_string()]);
    }

    #[test]
    fn removes_source_with_no_matching_rag_chunk_still_ok() {
        let c = open_db();
        c.execute_batch(
            "INSERT INTO knowledge.sources (id, filename, rel_path, sha256, bytes) VALUES ('src-9', 'z.md', 'sources/z.md', 'sha-z', 1);",
        )
        .unwrap();
        let r = sourceRemove(&json!({ "id": "src-9" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "removed": true, "id": "src-9" }));
    }
}
