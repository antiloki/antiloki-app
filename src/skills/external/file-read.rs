//! external/file-read.rs — twin of skills/external/file-read.mjs. BESPOKE-NATIVE, not conform-db
//! fixtured (see the crossing playbook's VET step). NAMES ARE CONTRACT: `fileRead` (default),
//! `validatePath` (named).
//!
//! Every branch of this skill does REAL filesystem I/O (existsSync/statSync/readFileSync) and
//! its default root is `PROJECT_ROOT`, a HARDCODED constant computed from THIS module's own
//! on-disk location: JS `resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')`
//! from `antiloki-core/skills/external/` walks up 3 levels to the PARENT of `antiloki-core` —
//! i.e. the substrate checkout root (same value as `sandbox-resolve-path.rs`'s / `workspace-
//! root.rs`'s `substrate_root()`), never a DB row the conform-db fixture harness could redirect.
//! The harness's FS-injection lane (conform-db.mjs `isFs`) only overrides an explicit
//! `input.root` field (see `codebase-file-read`'s fixture) — file-read.mjs never reads
//! `input.root`, so that lane cannot reach it either. Proven correct below via #[cfg(test)]
//! tests using a real temp dir + `ANTILOKI_SUBSTRATE_ROOT`/`ANTILOKI_HOME` env-var injection,
//! same pattern as `god-driver-halt.rs` / `sandbox-resolve-path.rs`'s `with_home()`.
//!
//! `contract_slug` branch calls the already-converted `sandboxResolvePath` twin
//! (`contract_sandbox_resolve_path::sandboxResolvePath`) — cross-module reuse per the playbook.
//! Its `location` can be `worktree` / `sandbox` / `original`; when `sandbox` and the resolved
//! file doesn't exist there, file-read falls back to `resolve(PROJECT_ROOT, path)` — safe
//! because `sandboxResolvePath` has ALREADY rejected an absolute/traversal `path` (it validates
//! the same string as `relpath`), so no re-validation is needed for the fallback join.
//!
//! Throws (VALIDATION_REQUIRED/VALIDATION_LOGIC/CONTEXT_MISSING/NOT_FOUND/PARSE_ERROR) collapse
//! to `Err(String)` carrying the exact `AntilokiError` message text (`"file-read: <cause>"`) for
//! audit-log parity; per the throw-fidelity gotcha these are not fixture-compared (no fixture
//! exists for this bespoke-native unit).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn is_windows_drive_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

// node:path win32 `isAbsolute` (this checkout's host platform): root-relative (`/x`, `\x`) or
// drive-absolute (`C:/x`, `C:\x`). Matches `sandbox-resolve-path.rs`'s copy (duplicated per-file
// per this codebase's convention — no shared node:path helper module).
fn is_absolute(s: &str) -> bool {
    s.starts_with('/') || s.starts_with('\\') || is_windows_drive_absolute(s)
}

// node:path `resolve(base, rel)` for a `rel` already known traversal-free and non-absolute (both
// call sites here only reach this after such a check) — join + separator-normalize, dropping `.`
// components. Same convention as `sandbox-resolve-path.rs`'s `node_join`.
fn node_join(base: &Path, rel: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        p.push(part);
    }
    p
}

// JS `PROJECT_ROOT = resolve(HERE, '..', '..', '..')` where `HERE` is this file's own directory
// (`antiloki-core/skills/external`) — i.e. the parent of `antiloki-core`. `CARGO_MANIFEST_DIR`
// here is `antiloki-core-rust`; its parent is the same substrate root on this checkout layout
// (same override seam + rationale as `sandbox-resolve-path.rs` / `workspace-root.rs`).
fn project_root() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

fn looks_binary(buf: &[u8]) -> bool {
    let end = buf.len().min(8192);
    buf[..end].iter().any(|&b| b == 0)
}

/// `export function validatePath(p)` — project-relative path safety guard: rejects absolute
/// paths, rejects any `..` segment, then rejects a resolved path that escapes `PROJECT_ROOT`
/// (unreachable in practice once the first two guards hold, kept for JS fidelity). Returns the
/// resolved absolute path as a JSON string.
pub fn validatePath(p: &Value) -> Result<Value, String> {
    let p_str = p.as_str().unwrap_or("");
    if is_absolute(p_str) {
        return Err("file-read: absolute paths not allowed (use project-relative path)".into());
    }
    if p_str.split(['\\', '/']).any(|seg| seg == "..") {
        return Err("file-read: parent directory traversal (..) not allowed".into());
    }
    let root = project_root();
    let abs = node_join(&root, p_str);
    let root_str = root.to_string_lossy().into_owned();
    let abs_str = abs.to_string_lossy().into_owned();
    if !abs_str.starts_with(&root_str) {
        return Err("file-read: resolved path escapes project root".into());
    }
    Ok(json!(abs_str))
}

/// `export default function fileRead(input, context)` — read a project file with safety guards;
/// read-only, no audit event. Returns `{ path, content, sha256, lines, bytes }`.
pub fn fileRead(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input = if input.is_object() { input } else { &empty };

    let path = match input.get("path").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(p) => p.to_string(),
        None => return Err("file-read: path is required".into()),
    };
    let contract_slug = input.get("contract_slug").cloned();

    // JS: `const db = context?.db; if (!db) throw err.contextMissing(...)` — unreachable here,
    // `db: &Connection` is always a live connection in this signature (same convention as every
    // sibling db-lane twin).
    let abs: PathBuf = if truthy(contract_slug.as_ref()) {
        let slug_val = contract_slug.clone().unwrap_or(Value::Null);
        let r = crate::contract_sandbox_resolve_path::sandboxResolvePath(
            &json!({ "contract_slug": slug_val, "relpath": path }),
            db,
        )?;
        let mut abs_str = r.get("absolute_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let location = r.get("location").and_then(|v| v.as_str()).unwrap_or("");
        if location == "sandbox" && !Path::new(&abs_str).exists() {
            // Fall back to the original — sandbox might not have this file yet. `path` was
            // already validated traversal-free/non-absolute by sandboxResolvePath above (it
            // checks the same string as `relpath`), so a plain join is safe here.
            let orig_abs = node_join(&project_root(), &path);
            if orig_abs.exists() {
                abs_str = orig_abs.to_string_lossy().into_owned();
            }
        }
        PathBuf::from(abs_str)
    } else {
        let v = validatePath(&json!(path))?;
        PathBuf::from(v.as_str().unwrap_or(""))
    };

    if !abs.exists() {
        return Err(format!("file-read: file '{path}' not found"));
    }
    if abs.is_dir() {
        return Err("file-read: path refers to a directory, not a file".into());
    }

    let buf = std::fs::read(&abs).map_err(|e| e.to_string())?;
    if looks_binary(&buf) {
        return Err("file-read: file appears to be binary (NUL bytes detected)".into());
    }

    let content = String::from_utf8_lossy(&buf).into_owned();
    let sha256 = hex::encode(Sha256::digest(&buf));
    let mut out = Map::new();
    out.insert("path".into(), json!(path));
    out.insert("content".into(), json!(content));
    out.insert("sha256".into(), json!(sha256));
    out.insert("lines".into(), json!(content.split('\n').count() as i64));
    out.insert("bytes".into(), json!(buf.len() as i64));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_SUBSTRATE_ROOT / ANTILOKI_HOME are process-global env vars read by many
    // twins' tests across the crate — shared crate-wide lock (test_env_lock.rs), a local mutex
    // would not prevent a race with OTHER files' tests over the same real env var.
    fn with_substrate_root<T>(root: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }
        result
    }

    fn with_home<T>(home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", home);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-file-read-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT, sandbox_path TEXT, files_touched_json TEXT, worktree_path TEXT);
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, root_path TEXT, vcs TEXT, is_active INTEGER, created_at TEXT);",
        )
        .unwrap();
        c
    }

    fn seed_contract(c: &Connection, slug: &str, files: &[&str], sandbox_path: Option<&str>, worktree_path: Option<&str>) {
        let files_json = serde_json::to_string(files).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, sandbox_path, files_touched_json, worktree_path) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![slug, sandbox_path, files_json, worktree_path],
        )
        .unwrap();
    }

    fn seed_active_workspace(c: &Connection, root_path: &str) {
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active, created_at) VALUES ('x', ?1, 'git', 1, '2026-01-01 00:00:00')",
            rusqlite::params![root_path],
        )
        .unwrap();
    }

    #[test]
    fn validate_path_rejects_absolute_and_traversal() {
        assert!(validatePath(&json!("C:/etc/passwd")).unwrap_err().contains("absolute"));
        assert!(validatePath(&json!("../etc/passwd")).unwrap_err().contains("traversal"));
        assert!(validatePath(&json!("a/../../b")).unwrap_err().contains("traversal"));
    }

    #[test]
    fn validate_path_resolves_under_project_root() {
        let dir = temp_dir("validate");
        let r = with_substrate_root(dir.to_str().unwrap(), || validatePath(&json!("src/a.mjs")).unwrap());
        let expected = dir.join("src").join("a.mjs");
        assert_eq!(r, json!(expected.to_string_lossy().into_owned()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_real_file_under_project_root() {
        let dir = temp_dir("happy");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src").join("a.mjs"), "export const a = 1;\nexport const b = 2;\n").unwrap();

        let c = db();
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            fileRead(&json!({ "path": "src/a.mjs" }), &c).unwrap()
        });
        assert_eq!(r["path"], json!("src/a.mjs"));
        assert_eq!(r["content"], json!("export const a = 1;\nexport const b = 2;\n"));
        assert_eq!(r["lines"], json!(3));
        assert_eq!(r["bytes"], json!(40));
        assert_eq!(
            r["sha256"],
            json!(hex::encode(Sha256::digest(b"export const a = 1;\nexport const b = 2;\n")))
        );
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["path", "content", "sha256", "lines", "bytes"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_missing_or_non_string_path() {
        let c = db();
        assert!(fileRead(&json!({}), &c).unwrap_err().contains("path is required"));
        assert!(fileRead(&json!({ "path": "" }), &c).unwrap_err().contains("path is required"));
        assert!(fileRead(&json!({ "path": 123 }), &c).unwrap_err().contains("path is required"));
    }

    #[test]
    fn rejects_not_found_directory_and_binary() {
        let dir = temp_dir("guards");
        fs::create_dir_all(dir.join("adir")).unwrap();
        fs::write(dir.join("bin.dat"), [1u8, 2, 0, 3, 4]).unwrap();

        let c = db();
        with_substrate_root(dir.to_str().unwrap(), || {
            let e1 = fileRead(&json!({ "path": "nope.txt" }), &c).unwrap_err();
            assert!(e1.contains("not found"), "{e1}");
            let e2 = fileRead(&json!({ "path": "adir" }), &c).unwrap_err();
            assert!(e2.contains("directory"), "{e2}");
            let e3 = fileRead(&json!({ "path": "bin.dat" }), &c).unwrap_err();
            assert!(e3.contains("binary"), "{e3}");
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn contract_slug_reads_from_sandbox_when_present() {
        let dir = temp_dir("sandbox-hit");
        let sandbox_dir = dir.join(".antiloki").join("sandbox").join("C-1");
        fs::create_dir_all(&sandbox_dir).unwrap();
        fs::write(sandbox_dir.join("x.mjs"), "sandbox content").unwrap();

        let c = db();
        seed_contract(&c, "C-1", &["x.mjs"], Some(".antiloki/sandbox/C-1"), None);
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            fileRead(&json!({ "path": "x.mjs", "contract_slug": "C-1" }), &c).unwrap()
        });
        assert_eq!(r["content"], json!("sandbox content"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn contract_slug_falls_back_to_original_when_sandbox_missing_file() {
        let dir = temp_dir("sandbox-fallback");
        // Sandbox dir exists but does NOT contain x.mjs; original (PROJECT_ROOT-relative) does.
        fs::create_dir_all(dir.join(".antiloki").join("sandbox").join("C-2")).unwrap();
        fs::write(dir.join("x.mjs"), "original content").unwrap();

        let c = db();
        seed_contract(&c, "C-2", &["x.mjs"], Some(".antiloki/sandbox/C-2"), None);
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            fileRead(&json!({ "path": "x.mjs", "contract_slug": "C-2" }), &c).unwrap()
        });
        assert_eq!(r["content"], json!("original content"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn contract_slug_reads_via_active_workspace_root_when_no_sandbox() {
        let dir = temp_dir("workspace-root");
        let ws_root = dir.join("workspace");
        fs::create_dir_all(&ws_root).unwrap();
        fs::write(ws_root.join("y.mjs"), "workspace content").unwrap();

        let c = db();
        seed_contract(&c, "C-3", &["y.mjs"], None, None);
        seed_active_workspace(&c, ws_root.to_str().unwrap());
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            fileRead(&json!({ "path": "y.mjs", "contract_slug": "C-3" }), &c).unwrap()
        });
        assert_eq!(r["content"], json!("workspace content"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn contract_slug_worktree_takes_precedence() {
        let dir = temp_dir("worktree");
        let wt = dir.join("worktrees").join("CARD-1");
        fs::create_dir_all(&wt).unwrap();
        fs::write(wt.join("z.mjs"), "worktree content").unwrap();

        let c = db();
        seed_contract(&c, "C-4", &["z.mjs"], None, Some(wt.to_string_lossy().as_ref()));
        let r = with_home(dir.to_str().unwrap(), || {
            fileRead(&json!({ "path": "z.mjs", "contract_slug": "C-4" }), &c).unwrap()
        });
        assert_eq!(r["content"], json!("worktree content"));
        let _ = fs::remove_dir_all(&dir);
    }
}
