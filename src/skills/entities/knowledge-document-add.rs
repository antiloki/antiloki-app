//! knowledge-document-add.rs — twin of skills/entities/knowledge-document-add.mjs (db lane,
//! default export `knowledgeDocumentAdd`). NAMES ARE CONTRACT.
//!
//! Registers a vault file as a knowledge document: resolves `vault_path` against KNOWLEDGE_DIR,
//! requires the file to already exist on disk, hashes its bytes (sha256), detects `doc_type` from
//! the extension when not given, and INSERTs (or, if an unsuperseded row with the same vault_path
//! + sha256 already exists, returns it unchanged — idempotent re-add).
//!
//! BESPOKE NATIVE TEST (not conform-db), per this session's lesson 5/6: unlike a skill with a
//! *conditionally skippable* real-fs branch (e.g. db-reset.mjs's `archive:false`), EVERY success
//! path here unconditionally does a real `existsSync`/`statSync`/`readFileSync` against a
//! pre-existing file — there is no branch that avoids touching the filesystem, so there is no safe
//! subset the conform-db JSON-fixture harness (which only seeds/reads SQL table state, no fs
//! injection lane) could exercise. Proven instead via `#[cfg(test)]` Rust-native tests that write
//! real temp files and inject the vault root via `ANTILOKI_DATA_DIR` (env-var + tempdir, same
//! locking pattern as sandbox-resolve-path.rs's `with_home`).
//!
//! roots.mjs: `KNOWLEDGE_DIR = join(DATA_DIR, 'knowledge')`, `DATA_DIR = ANTILOKI_DATA_DIR` else
//! `SUBSTRATE_ROOT/.antiloki`, `SUBSTRATE_ROOT = ANTILOKI_SUBSTRATE_ROOT` else self-derived from
//! roots.mjs's own file location. Per lesson 6, roots.mjs itself is NOT imported/converted — the
//! specific root resolution is inlined below, reading the env vars at call time (self-derivation
//! fallback anchored at CARGO_MANIFEST_DIR, mirroring source-add.rs's `sources_dir()` /
//! db-reset.rs's `default_archive_dir()` precedent).
//!
//! `context.db` missing → JS `err.contextMissing('knowledge-document-add','db')` — unreachable
//! here (Rust's `db: &Connection` is a required, non-nullable reference), same convention as every
//! sibling twin. `context.actor` → per knowledge-extract-add.rs's precedent, the db-lane harness
//! context carries only `db`, so `actor` is always its JS default `'unknown'`; modeled below as the
//! `ingested_by` absent-key fallback.
//!
//! Throws (VALIDATION_REQUIRED / NOT_FOUND / VALIDATION_LOGIC / VALIDATION_ENUM / ALREADY_EXISTS)
//! reproduce the exact `AntilokiError` message text (skill-prefixed `cause`) for audit fidelity,
//! per the playbook's throw-fidelity lesson — verified against `skills/engine/error.mjs`'s message
//! assembly (`${skill}: ${cause}`).
//!
//! `vault_path` is assumed a JSON string in practice (every call site + the JS test pass a string;
//! a non-string would make node:path's `resolve()` throw a TypeError with no `AntilokiError`
//! analog to reproduce) — modeled as `.as_str().unwrap_or("")`, matching the "guard-throws are not
//! exercised" convention.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const ALLOWED_TYPES: [&str; 6] = ["md", "pdf", "image", "code", "external", "note"];

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// node:path `extname(path)` — byte-for-byte port of the real algorithm (source-add.rs precedent,
// verified against real `node:path` ground truth).
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

fn detect_type(vault_path: &str) -> &'static str {
    let ext = extname(vault_path).to_lowercase();
    match ext.as_str() {
        ".md" | ".markdown" => "md",
        ".pdf" => "pdf",
        ".png" | ".jpg" | ".jpeg" | ".gif" | ".svg" | ".webp" => "image",
        ".mjs" | ".js" | ".ts" | ".py" | ".go" | ".rs" | ".java" => "code",
        ".url" | ".link" => "external",
        _ => "note",
    }
}

fn is_windows_drive_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

// node:path `resolve(base, rel)`: an absolute `rel` replaces `base` entirely; otherwise join +
// drop `.` components (no `..`-collapsing — vault_path is a caller-supplied relative doc path in
// practice, matching source-add.rs's disclosed simplification for its own unexercised resolve()).
fn resolve_vault_path(base: &Path, rel: &str) -> PathBuf {
    if rel.starts_with('/') || rel.starts_with('\\') || is_windows_drive_absolute(rel) {
        return PathBuf::from(rel);
    }
    let mut p = base.to_path_buf();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        p.push(part);
    }
    p
}

// roots.mjs: KNOWLEDGE_DIR = join(DATA_DIR, 'knowledge'); DATA_DIR = ANTILOKI_DATA_DIR else
// SUBSTRATE_ROOT/.antiloki; SUBSTRATE_ROOT = ANTILOKI_SUBSTRATE_ROOT else self-derived. See module
// doc — not imported from roots.mjs (lesson 6), inlined + env-read at call time instead.
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
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .join(".antiloki")
        .join("knowledge")
}

pub fn knowledgeDocumentAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input = if input.is_object() { input } else { &empty };

    let vault_path_val = input.get("vault_path");
    if !truthy(vault_path_val) {
        return Err("knowledge-document-add: vault_path is required".into());
    }
    let vault_path = vault_path_val.and_then(|v| v.as_str()).unwrap_or("").to_string();

    let abs = resolve_vault_path(&vault_root(), &vault_path);
    if !abs.exists() {
        return Err(format!("knowledge-document-add: vault file '{vault_path}' not found"));
    }
    let stat = std::fs::metadata(&abs).map_err(|e| format!("knowledge-document-add: {e}"))?;
    if stat.is_dir() {
        return Err("knowledge-document-add: vault_path refers to a directory".into());
    }

    let buf = std::fs::read(&abs).map_err(|e| format!("knowledge-document-add: {e}"))?;
    let sha256 = hex::encode(Sha256::digest(&buf));

    // JS: `doc_type || detectType(vault_path)` — a falsy doc_type (missing, null, "") falls
    // through to detection, not just an absent key.
    let doc_type_val = input.get("doc_type");
    let doc_type = if truthy(doc_type_val) {
        doc_type_val.and_then(|v| v.as_str()).unwrap_or("").to_string()
    } else {
        detect_type(&vault_path).to_string()
    };
    if !ALLOWED_TYPES.contains(&doc_type.as_str()) {
        return Err(format!(
            "knowledge-document-add: doc_type must be one of: {} (got '{}')",
            ALLOWED_TYPES.join(", "),
            doc_type
        ));
    }

    let existing_rows = crate::db_json::query_rows(
        db,
        "SELECT * FROM knowledge.documents WHERE vault_path = ? AND superseded_by IS NULL",
        &[json!(vault_path.clone())],
    )?;
    if let Some(existing) = existing_rows.as_array().and_then(|a| a.first()) {
        let existing_sha = existing.get("sha256").and_then(|v| v.as_str()).unwrap_or("");
        if existing_sha == sha256 {
            let mut o: Map<String, Value> = existing.as_object().cloned().unwrap_or_default();
            o.insert("idempotent".into(), json!(true));
            return Ok(Value::Object(o));
        }
        return Err(format!(
            "knowledge-document-add: document '{vault_path}' exists with different sha256 — file was modified. Supersede explicitly to register the new version."
        ));
    }

    let summary = input.get("summary").cloned().unwrap_or(Value::Null);
    let external_source = input.get("external_source").cloned().unwrap_or(Value::Null);
    // JS: `ingested_by = actor` — destructuring default fires only on an ABSENT key; explicit null
    // stays null (knowledge-extract-add.rs `created_by` precedent). db-lane context has no `actor`
    // key, so the default is always 'unknown'.
    let ingested_by = match input.get("ingested_by") {
        None => json!("unknown"),
        Some(v) => v.clone(),
    };

    let (_changes, id) = crate::db_json::run_write(
        db,
        "INSERT INTO knowledge.documents (vault_path, doc_type, sha256, bytes, summary, ingested_by, external_source) VALUES (?, ?, ?, ?, ?, ?, ?)",
        &[
            json!(vault_path),
            json!(doc_type),
            json!(sha256),
            json!(buf.len() as i64),
            summary,
            ingested_by,
            external_source,
        ],
    )?;

    let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.documents WHERE id = ?", &[json!(id)])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    // shared crate-wide lock (test_env_lock.rs) — a local mutex here would not prevent a race with OTHER files' tests over the same real env var.

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
        dir.push(format!("antiloki-knowledge-document-add-rs-{tag}-{}", std::process::id()));
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

    #[test]
    fn extname_matches_node_ground_truth() {
        assert_eq!(extname("papers/voyage-deprecation.pdf"), ".pdf");
        assert_eq!(extname("notes/readme"), "");
        assert_eq!(extname("a/.gitignore"), "");
        assert_eq!(extname("a/archive.tar.gz"), ".gz");
    }

    #[test]
    fn detect_type_covers_every_bucket() {
        assert_eq!(detect_type("x.md"), "md");
        assert_eq!(detect_type("x.markdown"), "md");
        assert_eq!(detect_type("x.pdf"), "pdf");
        assert_eq!(detect_type("x.png"), "image");
        assert_eq!(detect_type("x.webp"), "image");
        assert_eq!(detect_type("x.rs"), "code");
        assert_eq!(detect_type("x.url"), "external");
        assert_eq!(detect_type("x.link"), "external");
        assert_eq!(detect_type("x.unknownext"), "note");
    }

    #[test]
    fn missing_vault_path_is_required() {
        let c = seeded();
        assert_eq!(
            knowledgeDocumentAdd(&json!({}), &c),
            Err("knowledge-document-add: vault_path is required".to_string())
        );
        assert_eq!(
            knowledgeDocumentAdd(&json!({ "vault_path": "" }), &c),
            Err("knowledge-document-add: vault_path is required".to_string())
        );
    }

    #[test]
    fn nonexistent_file_is_not_found() {
        let dir = temp_dir("missing");
        let c = seeded();
        let r = with_data_dir(dir.to_str().unwrap(), || {
            knowledgeDocumentAdd(&json!({ "vault_path": "papers/ghost.pdf" }), &c)
        });
        assert_eq!(r, Err("knowledge-document-add: vault file 'papers/ghost.pdf' not found".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn directory_vault_path_is_rejected() {
        let dir = temp_dir("isdir");
        fs::create_dir_all(dir.join("knowledge").join("papers").join("sub")).unwrap();
        let c = seeded();
        let r = with_data_dir(dir.to_str().unwrap(), || {
            knowledgeDocumentAdd(&json!({ "vault_path": "papers/sub" }), &c)
        });
        assert_eq!(r, Err("knowledge-document-add: vault_path refers to a directory".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn inserts_new_document_and_detects_type_from_extension() {
        let dir = temp_dir("insert");
        let vault = dir.join("knowledge");
        fs::create_dir_all(vault.join("papers")).unwrap();
        fs::write(vault.join("papers").join("voyage-deprecation.pdf"), b"hello world").unwrap();

        let c = seeded();
        let r = with_data_dir(dir.to_str().unwrap(), || {
            knowledgeDocumentAdd(&json!({ "vault_path": "papers/voyage-deprecation.pdf", "summary": "why we moved off Voyage" }), &c)
        })
        .unwrap();

        assert_eq!(r["vault_path"], json!("papers/voyage-deprecation.pdf"));
        assert_eq!(r["doc_type"], json!("pdf"));
        assert_eq!(r["sha256"], json!("b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"));
        assert_eq!(r["bytes"], json!(11));
        assert_eq!(r["summary"], json!("why we moved off Voyage"));
        assert_eq!(r["ingested_by"], json!("unknown"));
        assert_eq!(r["external_source"], Value::Null);
        assert_eq!(r["superseded_by"], Value::Null);

        let n: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.documents", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_doc_type_overrides_detection() {
        let dir = temp_dir("explicit-type");
        let vault = dir.join("knowledge");
        fs::create_dir_all(&vault).unwrap();
        fs::write(vault.join("notes.txt"), b"plain text notes").unwrap();

        let c = seeded();
        let r = with_data_dir(dir.to_str().unwrap(), || {
            knowledgeDocumentAdd(&json!({ "vault_path": "notes.txt", "doc_type": "note" }), &c)
        })
        .unwrap();
        assert_eq!(r["doc_type"], json!("note"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_doc_type_is_rejected_via_enum_error() {
        let dir = temp_dir("bad-type");
        let vault = dir.join("knowledge");
        fs::create_dir_all(&vault).unwrap();
        fs::write(vault.join("x.mjs"), b"export default 1;").unwrap();

        let c = seeded();
        let r = with_data_dir(dir.to_str().unwrap(), || {
            knowledgeDocumentAdd(&json!({ "vault_path": "x.mjs", "doc_type": "video" }), &c)
        });
        assert_eq!(
            r,
            Err("knowledge-document-add: doc_type must be one of: md, pdf, image, code, external, note (got 'video')".to_string())
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reingesting_unchanged_bytes_returns_existing_row_as_idempotent() {
        let dir = temp_dir("idempotent");
        let vault = dir.join("knowledge");
        fs::create_dir_all(&vault).unwrap();
        fs::write(vault.join("stable.md"), b"same content").unwrap();

        let c = seeded();
        let first = with_data_dir(dir.to_str().unwrap(), || {
            knowledgeDocumentAdd(&json!({ "vault_path": "stable.md" }), &c)
        })
        .unwrap();
        let second = with_data_dir(dir.to_str().unwrap(), || {
            knowledgeDocumentAdd(&json!({ "vault_path": "stable.md" }), &c)
        })
        .unwrap();

        assert_eq!(second["idempotent"], json!(true));
        assert_eq!(second["id"], first["id"]);
        assert_eq!(second["sha256"], first["sha256"]);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.documents", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1, "no duplicate row inserted");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn changed_bytes_on_existing_vault_path_is_rejected() {
        let dir = temp_dir("changed");
        let vault = dir.join("knowledge");
        fs::create_dir_all(&vault).unwrap();
        let path = vault.join("mutable.md");
        fs::write(&path, b"version one").unwrap();

        let c = seeded();
        with_data_dir(dir.to_str().unwrap(), || knowledgeDocumentAdd(&json!({ "vault_path": "mutable.md" }), &c)).unwrap();

        fs::write(&path, "version TWO — different bytes".as_bytes()).unwrap();
        let r = with_data_dir(dir.to_str().unwrap(), || knowledgeDocumentAdd(&json!({ "vault_path": "mutable.md" }), &c));
        assert!(r.is_err());
        let msg = r.unwrap_err();
        assert!(msg.contains("document 'mutable.md' exists with different sha256"));
        assert!(msg.contains("Supersede explicitly to register the new version."));

        let n: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.documents", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1, "no second row inserted on the rejected re-add");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_ingested_by_overrides_actor_default() {
        let dir = temp_dir("ingested-by");
        let vault = dir.join("knowledge");
        fs::create_dir_all(&vault).unwrap();
        fs::write(vault.join("owned.md"), b"owned content").unwrap();

        let c = seeded();
        let r = with_data_dir(dir.to_str().unwrap(), || {
            knowledgeDocumentAdd(&json!({ "vault_path": "owned.md", "ingested_by": "curator" }), &c)
        })
        .unwrap();
        assert_eq!(r["ingested_by"], json!("curator"));
        let _ = fs::remove_dir_all(&dir);
    }
}
