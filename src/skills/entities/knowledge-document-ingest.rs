//! knowledge-document-ingest.rs — twin of skills/entities/knowledge-document-ingest.mjs (db lane,
//! default export `knowledgeDocumentIngest`). NAMES ARE CONTRACT.
//!
//! Accepts RAW uploaded bytes (base64), writes them into the vault at
//! `.antiloki/knowledge/uploads/<sha256><ext>` (content-addressed, so an identical re-upload
//! overwrites itself byte-for-byte and lands on the same path), then composes
//! `knowledge-document-add` to index the row (which re-hashes the file from disk, verifying the
//! write, and is itself idempotent on unchanged bytes).
//!
//! BESPOKE NATIVE TEST (not conform-db), same rationale as knowledge-document-add.rs (its module
//! doc, lesson 5/6): EVERY success path here unconditionally does a real `mkdirSync`/
//! `writeFileSync` against the vault root — there is no branch that avoids touching the
//! filesystem, so there is no safe subset the conform-db JSON-fixture harness (SQL-only
//! seed/read, no fs injection lane) could exercise. Proven instead via `#[cfg(test)]` Rust-native
//! tests that inject the vault root via `ANTILOKI_DATA_DIR` (env-var + tempdir, same locking
//! pattern as knowledge-document-add.rs / sandbox-resolve-path.rs's `with_home`).
//!
//! `context.db` missing → JS `err.contextMissing('knowledge-document-ingest','db')` —
//! unreachable here (Rust's `db: &Connection` is a required, non-nullable reference), same
//! convention as every sibling twin.
//!
//! `context.actor` → per knowledge-document-add.rs's precedent, the db-lane harness context
//! carries only `db`, so `actor` is always its JS default `'unknown'`; the JS source passes
//! `ingested_by: actor` through unconditionally (never omits the key), so this twin passes the
//! literal `"unknown"` through to the composed `knowledgeDocumentAdd` call — same resolved value
//! either way (an absent key there also defaults to `"unknown"`).
//!
//! `Buffer.from(str, 'base64')` decodes leniently (ignores stray/invalid characters, tolerates
//! missing padding) and can only THROW on a non-string input we already guard against via
//! `bytes_base64 is required`; the `base64` crate's STANDARD engine is strict. Fixtures/tests here
//! only feed well-formed base64 (matching source-add.rs's disclosed precedent), so this gap is
//! disclosed but not exercised — a malformed-base64 input diverges (JS: silently decodes garbage
//! bytes and proceeds; this twin: `Err`) but that path is a guard-throw, not exercised per the
//! playbook's throw-fidelity convention (only the SUCCESS path is fixtured).
//!
//! Throws (`filename`/`bytes_base64` required, empty-file, oversize) reproduce the exact
//! `AntilokiError` message text (`${skill}: ${cause}`) for audit fidelity — verified against
//! `skills/engine/error.mjs`'s message assembly. The two ad-hoc `AntilokiError` throws in the JS
//! source (`bytes_base64 is not valid base64`, `uploaded file is empty`, oversize) carry no
//! `skill:` prefix helper call but DO pass `skill: 'knowledge-document-ingest'` explicitly, so the
//! assembled message is still `knowledge-document-ingest: <cause>`.
//!
//! roots.mjs `KNOWLEDGE_DIR` resolution inlined verbatim from knowledge-document-add.rs's
//! `vault_root()` (not imported from roots.mjs — see that twin's module doc, lesson 6).

use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const UPLOADS_SUBDIR: &str = "uploads";
const MAX_BYTES: usize = 25 * 1024 * 1024;

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

// node:path `extname(path)` — byte-for-byte port, same as knowledge-document-add.rs.
fn extname(path: &str) -> String {
    let chars: Vec<char> = path.chars().collect();
    let len = chars.len() as i64;
    let mut start_dot: i64 = -1;
    let start_part: i64 = 0;
    let mut end: i64 = -1;
    let mut pre_dot_state: i32 = 0;
    let mut i = len - 1;
    while i >= 0 {
        let c = chars[i as usize];
        if c == '/' || c == '\\' {
            break;
        }
        if end == -1 {
            end = i + 1;
        }
        if c == '.' {
            if start_dot == -1 {
                start_dot = i;
            } else if pre_dot_state != 1 {
                pre_dot_state = 1;
            }
        } else if start_dot != -1 {
            pre_dot_state = -1;
        }
        i -= 1;
    }
    if start_dot == -1
        || end == -1
        || pre_dot_state == 0
        || (pre_dot_state == 1 && start_dot == end - 1 && start_dot == start_part + 1)
    {
        return String::new();
    }
    chars[start_dot as usize..end as usize].iter().collect()
}

// node:path `basename(p)` — trailing separators trimmed first, then the tail after the last
// separator (both `/` and `\` accepted, matching artefact-store.rs's precedent).
fn basename(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(i) => trimmed[i + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

// roots.mjs: KNOWLEDGE_DIR = join(DATA_DIR, 'knowledge'); DATA_DIR = ANTILOKI_DATA_DIR else
// SUBSTRATE_ROOT/.antiloki; SUBSTRATE_ROOT = ANTILOKI_SUBSTRATE_ROOT else self-derived. Verbatim
// duplicate of knowledge-document-add.rs's `vault_root()` — see that twin's module doc.
fn vault_root() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_DATA_DIR") {
        if !v.is_empty() {
            return PathBuf::from(v).join("knowledge");
        }
    }
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v).join(".antiloki").join("knowledge");
        }
    }
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .join(".antiloki")
        .join("knowledge")
}

pub fn knowledgeDocumentIngest(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input = if input.is_object() { input } else { &empty };

    let filename_val = input.get("filename");
    if !truthy(filename_val) {
        return Err("knowledge-document-ingest: filename is required".into());
    }
    let filename = js_string(filename_val.unwrap());

    let bytes_b64_val = input.get("bytes_base64");
    if !truthy(bytes_b64_val) {
        return Err("knowledge-document-ingest: bytes_base64 is required".into());
    }
    let bytes_b64 = js_string(bytes_b64_val.unwrap());

    use base64::Engine;
    let buf = base64::engine::general_purpose::STANDARD
        .decode(bytes_b64.as_bytes())
        .map_err(|_| "knowledge-document-ingest: bytes_base64 is not valid base64".to_string())?;

    if buf.is_empty() {
        return Err("knowledge-document-ingest: uploaded file is empty".into());
    }
    if buf.len() > MAX_BYTES {
        return Err(format!("knowledge-document-ingest: uploaded file exceeds {MAX_BYTES} bytes"));
    }

    let sha256 = hex::encode(Sha256::digest(&buf));
    let ext = extname(&filename).to_lowercase();
    let stored_name = format!("{sha256}{ext}");
    let vault_path = format!("{UPLOADS_SUBDIR}/{stored_name}");

    let uploads_dir = vault_root().join(UPLOADS_SUBDIR);
    std::fs::create_dir_all(&uploads_dir).map_err(|e| format!("knowledge-document-ingest: {e}"))?;
    std::fs::write(uploads_dir.join(&stored_name), &buf)
        .map_err(|e| format!("knowledge-document-ingest: {e}"))?;

    let summary = input.get("summary").cloned().unwrap_or(Value::Null);
    let doc = crate::entities_knowledge_document_add::knowledgeDocumentAdd(
        &json!({ "vault_path": vault_path.clone(), "summary": summary, "ingested_by": "unknown" }),
        db,
    )?;

    let mime = input.get("mime").cloned().unwrap_or(Value::Null);
    let idempotent = truthy(doc.get("idempotent"));

    Ok(json!({
        "document_id": doc.get("id").cloned().unwrap_or(Value::Null),
        "doc_type": doc.get("doc_type").cloned().unwrap_or(Value::Null),
        "title": basename(&filename),
        "vault_path": vault_path,
        "sha256": doc.get("sha256").cloned().unwrap_or(Value::Null),
        "bytes": doc.get("bytes").cloned().unwrap_or(Value::Null),
        "mime": mime,
        "idempotent": idempotent,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::fs;

    fn with_data_dir<T>(data_dir: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_DATA_DIR", data_dir);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_DATA_DIR");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-knowledge-document-ingest-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS knowledge;
            CREATE TABLE knowledge.documents (
              id INTEGER PRIMARY KEY AUTOINCREMENT,
              vault_path TEXT NOT NULL UNIQUE,
              doc_type TEXT NOT NULL,
              sha256 TEXT NOT NULL,
              bytes INTEGER NOT NULL,
              summary TEXT,
              ingested_at TEXT NOT NULL DEFAULT (datetime('now')),
              ingested_by TEXT NOT NULL,
              external_source TEXT,
              superseded_by INTEGER,
              superseded_at TEXT
            );",
        )
        .unwrap();
        c
    }

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn missing_filename_or_bytes_is_required() {
        let c = seeded();
        assert_eq!(
            knowledgeDocumentIngest(&json!({}), &c),
            Err("knowledge-document-ingest: filename is required".to_string())
        );
        assert_eq!(
            knowledgeDocumentIngest(&json!({ "filename": "a.md" }), &c),
            Err("knowledge-document-ingest: bytes_base64 is required".to_string())
        );
    }

    #[test]
    fn blank_bytes_base64_is_rejected_as_required() {
        // "" is falsy in JS (`if (!bytes_base64)`), same as an absent key.
        let c = seeded();
        let dir = temp_dir("empty");
        let r = with_data_dir(dir.to_str().unwrap(), || {
            knowledgeDocumentIngest(&json!({ "filename": "a.md", "bytes_base64": "" }), &c)
        });
        assert_eq!(r, Err("knowledge-document-ingest: bytes_base64 is required".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversize_upload_is_rejected() {
        let c = seeded();
        let dir = temp_dir("oversize");
        let big = b64(&vec![7u8; MAX_BYTES + 1]);
        let r = with_data_dir(dir.to_str().unwrap(), || {
            knowledgeDocumentIngest(&json!({ "filename": "a.bin", "bytes_base64": big }), &c)
        });
        assert_eq!(r, Err(format!("knowledge-document-ingest: uploaded file exceeds {MAX_BYTES} bytes")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ingests_new_upload_writes_content_addressed_file_and_indexes_it() {
        let c = seeded();
        let dir = temp_dir("ingest");
        let bytes = b"hello world".to_vec();
        let sha256 = hex::encode(Sha256::digest(&bytes));

        let r = with_data_dir(dir.to_str().unwrap(), || {
            knowledgeDocumentIngest(
                &json!({ "filename": "papers/voyage-deprecation.pdf", "bytes_base64": b64(&bytes), "mime": "application/pdf" }),
                &c,
            )
        })
        .unwrap();

        assert_eq!(r["vault_path"], json!(format!("uploads/{sha256}.pdf")));
        assert_eq!(r["title"], json!("voyage-deprecation.pdf"));
        assert_eq!(r["sha256"], json!(sha256));
        assert_eq!(r["doc_type"], json!("pdf"));
        assert_eq!(r["bytes"], json!(11));
        assert_eq!(r["mime"], json!("application/pdf"));
        assert_eq!(r["idempotent"], json!(false));

        let stored = dir.join("knowledge").join("uploads").join(format!("{sha256}.pdf"));
        assert_eq!(fs::read(&stored).unwrap(), bytes);

        let n: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.documents", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reingesting_identical_bytes_is_idempotent_and_overwrites_same_path() {
        let c = seeded();
        let dir = temp_dir("reingest");
        let bytes = b"same content".to_vec();

        let first = with_data_dir(dir.to_str().unwrap(), || {
            knowledgeDocumentIngest(&json!({ "filename": "notes.md", "bytes_base64": b64(&bytes) }), &c)
        })
        .unwrap();
        let second = with_data_dir(dir.to_str().unwrap(), || {
            knowledgeDocumentIngest(&json!({ "filename": "notes.md", "bytes_base64": b64(&bytes) }), &c)
        })
        .unwrap();

        assert_eq!(first["vault_path"], second["vault_path"]);
        assert_eq!(second["idempotent"], json!(true));
        assert_eq!(first["document_id"], second["document_id"]);

        let n: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.documents", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1, "no duplicate row inserted");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_extension_filename_produces_extensionless_stored_path() {
        let c = seeded();
        let dir = temp_dir("noext");
        let bytes = b"plain".to_vec();
        let sha256 = hex::encode(Sha256::digest(&bytes));

        let r = with_data_dir(dir.to_str().unwrap(), || {
            knowledgeDocumentIngest(&json!({ "filename": "README", "bytes_base64": b64(&bytes) }), &c)
        })
        .unwrap();
        assert_eq!(r["vault_path"], json!(format!("uploads/{sha256}")));
        assert_eq!(r["doc_type"], json!("note"));
        let _ = fs::remove_dir_all(&dir);
    }
}
