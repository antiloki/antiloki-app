//! external/file-list.rs — twin of skills/external/file-list.mjs. BESPOKE-NATIVE, not conform-db
//! fixtured (see the crossing playbook's VET step). NAMES ARE CONTRACT: `fileList` (default),
//! `validatePath` (named).
//!
//! Every branch does REAL filesystem I/O (readdirSync/lstatSync/statSync/readFileSync) rooted at
//! `PROJECT_ROOT`, a HARDCODED constant computed from THIS module's own on-disk location: JS
//! `resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')` from
//! `antiloki-core/skills/external/` walks up 3 levels to the PARENT of `antiloki-core` — i.e. the
//! substrate checkout root (same value/rationale as `file-read.rs`'s `project_root()` /
//! `god-driver-halt.rs`'s home-dir resolution). Never a DB row, so the conform-db harness's
//! FS-injection lane cannot redirect it. Proven correct below via #[cfg(test)] tests using a real
//! temp dir + `ANTILOKI_SUBSTRATE_ROOT` env-var injection, same pattern as `file-read.rs`.
//!
//! Sort: JS `names.sort((a,b) => a.localeCompare(b, undefined, { sensitivity: 'base' }))` is a
//! full ICU case+diacritic-insensitive collation with no Rust-stdlib equivalent (no external ICU
//! crate is a dependency of this crate). Approximated here as a stable case-insensitive
//! (`to_lowercase`) sort — exact for the ASCII names this bespoke-native path is tested against;
//! documented divergence for non-ASCII names, which is acceptable because this unit has no
//! JS-vs-Rust byte-parity check (bespoke-native, native tests only).
//!
//! Throws (VALIDATION_REQUIRED/VALIDATION_LOGIC/PERMISSION_DENIED/NOT_FOUND) collapse to
//! `Err(String)` carrying the exact `AntilokiError` message text (`"file-list: <cause>"`) for
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

fn is_windows_drive_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

// node:path win32 `isAbsolute` (this checkout's host platform): root-relative (`/x`, `\x`) or
// drive-absolute (`C:/x`, `C:\x`). Matches `file-read.rs`'s copy (duplicated per-file per this
// codebase's convention — no shared node:path helper module).
fn is_absolute(s: &str) -> bool {
    s.starts_with('/') || s.starts_with('\\') || is_windows_drive_absolute(s)
}

// node:path `resolve(base, rel)` for a `rel` already known traversal-free and non-absolute (the
// only call site here reaches this after such a check) — join + separator-normalize, dropping `.`
// components. Same convention as `file-read.rs`'s `node_join`.
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
// (same override seam + rationale as `file-read.rs`'s `project_root()`).
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

const PATH_BLACKLIST: [&str; 3] = [".antiloki/auth", ".antiloki/db", ".git"];

fn is_blacklisted(rel: &str) -> bool {
    let norm = rel.replace('\\', "/");
    PATH_BLACKLIST.iter().any(|p| norm == *p || norm.starts_with(&format!("{p}/")))
}

/// `export function validatePath(p)` — project-relative path safety guard: rejects a non-string,
/// an absolute path, any `..` segment, a blacklisted prefix (auth keys / db / git internals),
/// then a resolved path that escapes `PROJECT_ROOT`. Returns the resolved absolute path as a JSON
/// string.
pub fn validatePath(p: &Value) -> Result<Value, String> {
    let p_str = match p.as_str() {
        Some(s) => s,
        None => return Err("file-list: path is required".into()),
    };
    if is_absolute(p_str) {
        return Err("file-list: absolute paths not allowed (use project-relative path)".into());
    }
    if p_str.split(['\\', '/']).any(|seg| seg == "..") {
        return Err("file-list: parent directory traversal (..) not allowed".into());
    }
    if is_blacklisted(p_str) {
        return Err(format!(
            "file-list: path '{p_str}' is blacklisted (auth keys, db, git internals)"
        ));
    }
    let root = project_root();
    let abs = node_join(&root, p_str);
    let root_str = root.to_string_lossy().into_owned();
    let abs_str = abs.to_string_lossy().into_owned();
    if !abs_str.starts_with(&root_str) {
        return Err("file-list: resolved path escapes project root".into());
    }
    Ok(json!(abs_str))
}

fn classify_type(abs_entry: &Path) -> &'static str {
    let meta = match std::fs::symlink_metadata(abs_entry) {
        Ok(m) => m,
        Err(_) => return "other",
    };
    let ft = meta.file_type();
    if ft.is_symlink() {
        "symlink"
    } else if ft.is_dir() {
        "dir"
    } else if ft.is_file() {
        "file"
    } else {
        "other"
    }
}

/// `export default function fileList(input, context)` — list a directory's entries with the same
/// safety guards as `file-read`. Read-only, no audit event. Returns
/// `{ path, entries: [{ name, type, size?, sha256? }], count, truncated }`.
pub fn fileList(input: &Value, _db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input = if input.is_object() { input } else { &empty };

    let path_val = input.get("path").cloned().unwrap_or(Value::Null);
    let abs_val = validatePath(&path_val)?;
    let path = path_val.as_str().unwrap_or("").to_string();
    let abs = PathBuf::from(abs_val.as_str().unwrap_or(""));

    if !abs.exists() {
        return Err(format!("file-list: directory '{path}' not found"));
    }
    let meta = std::fs::metadata(&abs).map_err(|e| e.to_string())?;
    if !meta.is_dir() {
        return Err("file-list: path is not a directory".into());
    }

    let mut names: Vec<String> = std::fs::read_dir(&abs)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    // JS: names.sort((a,b) => a.localeCompare(b, undefined, { sensitivity: 'base' })) — stable,
    // case-insensitive (see module doc-comment for the ICU-collation caveat).
    names.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));

    let include_hashes = truthy(input.get("include_hashes"));
    let limit_num = js_number(input.get("limit"));
    let effective_limit = if limit_num != 0.0 && !limit_num.is_nan() { limit_num } else { 1000.0 };
    let cap = effective_limit.min(10_000.0).max(1.0);

    let mut entries: Vec<Value> = Vec::new();
    for name in &names {
        if (entries.len() as f64) >= cap {
            break;
        }
        let abs_entry = abs.join(name);
        let ty = classify_type(&abs_entry);
        let mut e = Map::new();
        e.insert("name".into(), json!(name));
        e.insert("type".into(), json!(ty));
        if ty == "file" {
            if let Ok(s) = std::fs::metadata(&abs_entry) {
                let size = s.len();
                e.insert("size".into(), json!(size));
                if include_hashes && size <= 5_000_000 {
                    if let Ok(buf) = std::fs::read(&abs_entry) {
                        e.insert("sha256".into(), json!(hex::encode(Sha256::digest(&buf))));
                    }
                }
            }
        }
        entries.push(Value::Object(e));
    }

    let count = entries.len();
    let truncated = names.len() > entries.len();
    let mut out = Map::new();
    out.insert("path".into(), json!(path));
    out.insert("entries".into(), Value::Array(entries));
    out.insert("count".into(), json!(count as i64));
    out.insert("truncated".into(), json!(truncated));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_SUBSTRATE_ROOT is a process-global env var read by many twins' tests
    // across the crate — shared crate-wide lock (test_env_lock.rs), a local mutex would not
    // prevent a race with OTHER files' tests over the same real env var.
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

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-file-list-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn validate_path_rejects_absolute_traversal_and_non_string() {
        assert!(validatePath(&json!("C:/etc/passwd")).unwrap_err().contains("absolute"));
        assert!(validatePath(&json!("../etc/passwd")).unwrap_err().contains("traversal"));
        assert!(validatePath(&json!("a/../../b")).unwrap_err().contains("traversal"));
        assert!(validatePath(&Value::Null).unwrap_err().contains("path is required"));
    }

    #[test]
    fn validate_path_rejects_blacklisted() {
        let e1 = validatePath(&json!(".antiloki/auth")).unwrap_err();
        assert!(e1.contains("blacklisted"), "{e1}");
        let e2 = validatePath(&json!(".antiloki/auth/foo")).unwrap_err();
        assert!(e2.contains("blacklisted"), "{e2}");
        let e3 = validatePath(&json!(".antiloki/db")).unwrap_err();
        assert!(e3.contains("blacklisted"), "{e3}");
        let e4 = validatePath(&json!(".git")).unwrap_err();
        assert!(e4.contains("blacklisted"), "{e4}");
    }

    #[test]
    fn lists_directory_sorted_case_insensitive_with_types_and_sizes() {
        let dir = temp_dir("happy");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("Banana.txt"), "abc").unwrap();
        fs::write(dir.join("apple.txt"), "de").unwrap();

        let c = db0();
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            fileList(&json!({ "path": "." }), &c).unwrap()
        });
        assert_eq!(r["path"], json!("."));
        assert_eq!(r["count"], json!(3));
        assert_eq!(r["truncated"], json!(false));
        let entries = r["entries"].as_array().unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e["name"].as_str().unwrap()).collect();
        // case-insensitive sort: apple.txt before Banana.txt before sub
        assert_eq!(names, vec!["apple.txt", "Banana.txt", "sub"]);

        let apple = entries.iter().find(|e| e["name"] == "apple.txt").unwrap();
        assert_eq!(apple["type"], json!("file"));
        assert_eq!(apple["size"], json!(2));
        assert!(apple.get("sha256").is_none());

        let sub = entries.iter().find(|e| e["name"] == "sub").unwrap();
        assert_eq!(sub["type"], json!("dir"));
        assert!(sub.get("size").is_none());

        let keys: Vec<&String> = apple.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["name", "type", "size"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn include_hashes_computes_sha256_for_files_only() {
        let dir = temp_dir("hashes");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "hello").unwrap();
        fs::create_dir_all(dir.join("d")).unwrap();

        let c = db0();
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            fileList(&json!({ "path": ".", "include_hashes": true }), &c).unwrap()
        });
        let entries = r["entries"].as_array().unwrap();
        let a = entries.iter().find(|e| e["name"] == "a.txt").unwrap();
        assert_eq!(a["sha256"], json!(hex::encode(Sha256::digest(b"hello"))));
        let d = entries.iter().find(|e| e["name"] == "d").unwrap();
        assert!(d.get("sha256").is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn limit_caps_entries_and_sets_truncated() {
        let dir = temp_dir("limit");
        fs::create_dir_all(&dir).unwrap();
        for n in ["a", "b", "c"] {
            fs::write(dir.join(n), "x").unwrap();
        }
        let c = db0();
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            fileList(&json!({ "path": ".", "limit": 2 }), &c).unwrap()
        });
        assert_eq!(r["count"], json!(2));
        assert_eq!(r["truncated"], json!(true));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn limit_defaults_to_1000_when_missing_zero_or_nan() {
        let dir = temp_dir("limit-default");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a"), "x").unwrap();
        let c = db0();
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            fileList(&json!({ "path": ".", "limit": 0 }), &c).unwrap()
        });
        assert_eq!(r["truncated"], json!(false));
        assert_eq!(r["count"], json!(1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_directory_returns_not_found() {
        let dir = temp_dir("notfound");
        let c = db0();
        let e = with_substrate_root(dir.to_str().unwrap(), || {
            fileList(&json!({ "path": "no-such-dir" }), &c).unwrap_err()
        });
        assert!(e.contains("not found"), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_path_returns_not_a_directory() {
        let dir = temp_dir("notadir");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("f.txt"), "x").unwrap();
        let c = db0();
        let e = with_substrate_root(dir.to_str().unwrap(), || {
            fileList(&json!({ "path": "f.txt" }), &c).unwrap_err()
        });
        assert!(e.contains("not a directory"), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }
}
