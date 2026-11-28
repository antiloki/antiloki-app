//! external/file-write.rs — twin of skills/external/file-write.mjs. BESPOKE-NATIVE, not conform-db
//! fixtured (see the crossing playbook's VET step). NAMES ARE CONTRACT: `fileWrite` (default),
//! `validatePath` (named).
//!
//! Every branch does REAL filesystem I/O (existsSync/readFileSync/mkdirSync/writeFileSync) and its
//! default root is `PROJECT_ROOT`, a HARDCODED constant computed from THIS module's own on-disk
//! location — same non-DB-derived root as `file-read.rs` (identical JS: `resolve(dirname(fileURLTo
//! Path(import.meta.url)), '..', '..', '..')`), never a DB row the conform-db fixture harness could
//! redirect. Proven correct below via #[cfg(test)] tests using a real temp dir + `ANTILOKI_SUBSTRATE
//! _ROOT` env-var injection, same pattern as `file-read.rs` / `god-driver-halt.rs`'s `with_home()`.
//!
//! `contract_slug` branch calls two already-converted twins (cross-module reuse per the playbook):
//! `sandboxResolvePath` (`contract_sandbox_resolve_path::sandboxResolvePath`) to resolve where the
//! write lands (sandbox / worktree / original), and — only when the resolved location is
//! `"original"` — `contractGet` (`contract_contract_get::contractGet`) to refuse a direct write to
//! the original file while the contract is in an active status (`impl`/`qa`/`done`) with no sandbox
//! yet. `applyHardBlacklist` (JS-private, not exported — not a census unit) is inlined here as a
//! private `apply_hard_blacklist` defense-in-depth re-check on the raw `relpath`, mirroring the JS
//! comment: even a contract's `files_touched` must never resolve through the hard blacklist.
//!
//! Throws (VALIDATION_REQUIRED/VALIDATION_LOGIC/PERMISSION_DENIED/CONTEXT_MISSING/ALREADY_EXISTS)
//! collapse to `Err(String)` carrying the exact `AntilokiError` message text (`"file-write: <cause>"`)
//! for audit-log parity; per the throw-fidelity gotcha these are not fixture-compared (no fixture
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

const HARD_BLACKLIST: &[&str] = &[".git/", "node_modules/", ".antiloki/auth/"];
const HARD_BLACKLIST_FILES: &[&str] = &["state.sqlite", "memory.sqlite", "brain.sqlite"];

fn is_windows_drive_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

// node:path win32 `isAbsolute` (this checkout's host platform): root-relative (`/x`, `\x`) or
// drive-absolute (`C:/x`, `C:\x`). Matches `file-read.rs` / `sandbox-resolve-path.rs`'s copy
// (duplicated per-file per this codebase's convention — no shared node:path helper module).
fn is_absolute(s: &str) -> bool {
    s.starts_with('/') || s.starts_with('\\') || is_windows_drive_absolute(s)
}

// node:path `resolve(base, rel)` for a `rel` already known traversal-free and non-absolute — join +
// separator-normalize, dropping `.` components. Same convention as `file-read.rs`'s `node_join`.
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
// (`antiloki-core/skills/external`) — i.e. the parent of `antiloki-core`. `CARGO_MANIFEST_DIR` here
// is `antiloki-core-rust`; its parent is the same substrate root on this checkout layout (same
// override seam as `file-read.rs` / `sandbox-resolve-path.rs` / `workspace-root.rs`).
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

fn sha256_hex(buf: &[u8]) -> String {
    hex::encode(Sha256::digest(buf))
}

// JS-private `applyHardBlacklist(p)` — not exported, not a census unit. Defense-in-depth re-check
// applied to the raw contract-relative path even when routed through the sandbox.
fn apply_hard_blacklist(p: &str) -> Result<(), String> {
    let norm = p.replace('\\', "/");
    for bl in HARD_BLACKLIST {
        if norm.starts_with(bl) {
            return Err(format!(
                "file-write: path under blacklisted prefix '{bl}' \u{2014} never writable"
            ));
        }
    }
    for bf in HARD_BLACKLIST_FILES {
        if norm == *bf || norm.ends_with(&format!("/{bf}")) {
            return Err(format!(
                "file-write: file '{bf}' is system-managed, not writable via action"
            ));
        }
    }
    Ok(())
}

/// `export function validatePath(p, allowedPrefixes = null)` — project-relative path safety guard:
/// requires a non-empty string, rejects absolute paths, rejects any `..` segment, rejects the hard
/// blacklist (prefixes + specific filenames), optionally requires an `allowedPrefixes` match, then
/// rejects a resolved path that escapes `PROJECT_ROOT`. Returns the resolved absolute path as a
/// JSON string.
pub fn validatePath(p: &Value, allowed_prefixes: &Value) -> Result<Value, String> {
    let p_str = match p.as_str().filter(|s| !s.is_empty()) {
        Some(s) => s,
        None => return Err("file-write: path is required".into()),
    };
    if is_absolute(p_str) {
        return Err("file-write: absolute paths not allowed".into());
    }
    if p_str.split(['\\', '/']).any(|seg| seg == "..") {
        return Err("file-write: parent directory traversal (..) not allowed".into());
    }

    let norm = p_str.replace('\\', "/");
    for bl in HARD_BLACKLIST {
        if norm.starts_with(bl) {
            return Err(format!(
                "file-write: path under blacklisted prefix '{bl}' \u{2014} never writable"
            ));
        }
    }
    for bf in HARD_BLACKLIST_FILES {
        if norm == *bf || norm.ends_with(&format!("/{bf}")) {
            return Err(format!(
                "file-write: file '{bf}' is system-managed, not writable via action"
            ));
        }
    }

    if let Some(prefixes) = allowed_prefixes.as_array().filter(|a| !a.is_empty()) {
        let ok = prefixes.iter().any(|pref| {
            let pref_norm = pref.as_str().unwrap_or("").replace('\\', "/");
            norm.starts_with(&pref_norm)
        });
        if !ok {
            let joined: Vec<&str> = prefixes.iter().filter_map(|v| v.as_str()).collect();
            return Err(format!(
                "file-write: path '{p_str}' not under any allowed prefix [{}]",
                joined.join(", ")
            ));
        }
    }

    let root = project_root();
    let abs = node_join(&root, p_str);
    let root_str = root.to_string_lossy().into_owned();
    let abs_str = abs.to_string_lossy().into_owned();
    if !abs_str.starts_with(&root_str) {
        return Err("file-write: resolved path escapes project root".into());
    }
    Ok(json!(abs_str))
}

/// `export default function fileWrite(input, context)` — write a project file with safety guards +
/// audit-friendly return `{ path, created, before_sha256, after_sha256, before_bytes, after_bytes,
/// lines }`. When `input.contract_slug` is set, routes through the sandbox (already-converted
/// `sandboxResolvePath` twin) instead of the plain `PROJECT_ROOT`-relative `validatePath` guard.
pub fn fileWrite(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty = json!({});
    let input = if input.is_object() { input } else { &empty };

    let path_val = input.get("path").cloned().unwrap_or(Value::Null);
    let content_val = input.get("content").cloned();
    let expected_sha256 = input.get("expected_sha256").cloned();
    let allowed_prefixes = input.get("allowed_prefixes").cloned().unwrap_or(Value::Null);
    let contract_slug = input.get("contract_slug").cloned();

    let content = match &content_val {
        Some(Value::String(s)) => s.clone(),
        _ => return Err("file-write: content is required".into()),
    };

    // CONTRACT-DRIVEN-WRITE-MANDATE (card 240): strict mode requires every write to cite a
    // contract_slug. Off by default (mirrors JS: `process.env.ANTILOKI_STRICT_CONTRACTS === 'true'`).
    if std::env::var("ANTILOKI_STRICT_CONTRACTS").ok().as_deref() == Some("true")
        && !truthy(contract_slug.as_ref())
    {
        return Err("file-write: ANTILOKI_STRICT_CONTRACTS=true: every file-write requires a contract_slug. Draft a contract via contract.draft.auto and route the write through it.".into());
    }

    let path_str = path_val.as_str().unwrap_or("").to_string();

    // JS: `const db = context?.db; if (!db) throw err.contextMissing(...)` — unreachable here,
    // `db: &Connection` is always a live connection in this signature (same convention as every
    // sibling db-lane twin, e.g. file-read.rs).
    let abs: PathBuf = if truthy(contract_slug.as_ref()) {
        let slug_val = contract_slug.unwrap_or(Value::Null);
        let slug_str = slug_val.as_str().unwrap_or("").to_string();
        let r = crate::contract_sandbox_resolve_path::sandboxResolvePath(
            &json!({ "contract_slug": slug_val.clone(), "relpath": path_val.clone() }),
            db,
        )?;
        let abs_str = r.get("absolute_path").and_then(|v| v.as_str()).unwrap_or("").to_string();

        apply_hard_blacklist(&path_str)?;

        let location = r.get("location").and_then(|v| v.as_str()).unwrap_or("");
        if location == "original" {
            let c = crate::contract_contract_get::contractGet(&slug_val, db)?;
            let status = c.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if matches!(status, "impl" | "qa" | "done") {
                return Err(format!(
                    "file-write: contract '{slug_str}' is in status='{status}' but has no sandbox_path \u{2014} refusing direct write to original. Run tl-sandbox-create first."
                ));
            }
        }
        PathBuf::from(abs_str)
    } else {
        let v = validatePath(&path_val, &allowed_prefixes)?;
        PathBuf::from(v.as_str().unwrap_or(""))
    };

    let existed = abs.exists();
    let mut before_sha256: Option<String> = None;
    let mut before_bytes: Option<i64> = None;

    if existed {
        let existing_buf = std::fs::read(&abs).map_err(|e| e.to_string())?;
        let sha = sha256_hex(&existing_buf);
        before_bytes = Some(existing_buf.len() as i64);

        if let Some(Value::String(exp)) = &expected_sha256 {
            if exp != &sha {
                let exp12: String = exp.chars().take(12).collect();
                let got12: String = sha.chars().take(12).collect();
                return Err(format!(
                    "file-write: optimistic concurrency check failed: file '{path_str}' has been modified since read (expected {exp12}, found {got12})"
                ));
            }
        }
        before_sha256 = Some(sha);
    }

    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&abs, content.as_bytes()).map_err(|e| e.to_string())?;

    let after_buf = std::fs::read(&abs).map_err(|e| e.to_string())?;
    let after_sha256 = sha256_hex(&after_buf);

    let mut out = Map::new();
    out.insert("path".into(), json!(path_str));
    out.insert("created".into(), json!(!existed));
    out.insert("before_sha256".into(), before_sha256.map_or(Value::Null, |s| json!(s)));
    out.insert("after_sha256".into(), json!(after_sha256));
    out.insert("before_bytes".into(), before_bytes.map_or(Value::Null, |n| json!(n)));
    out.insert("after_bytes".into(), json!(after_buf.len() as i64));
    out.insert("lines".into(), json!(content.split('\n').count() as i64));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_SUBSTRATE_ROOT / ANTILOKI_STRICT_CONTRACTS are process-global env vars read
    // by many twins' tests across the crate — shared crate-wide lock (test_env_lock.rs), a local
    // mutex here would not prevent a race with OTHER files' tests over the same real env var.
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

    fn with_strict_contracts<T>(f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_STRICT_CONTRACTS", "true");
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_STRICT_CONTRACTS");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-file-write-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT, status TEXT, sandbox_path TEXT, files_touched_json TEXT, worktree_path TEXT);
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT, root_path TEXT, vcs TEXT, is_active INTEGER, created_at TEXT);",
        )
        .unwrap();
        c
    }

    fn seed_contract(c: &Connection, slug: &str, status: &str, files: &[&str], sandbox_path: Option<&str>) {
        let files_json = serde_json::to_string(files).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, status, sandbox_path, files_touched_json, worktree_path) VALUES (?1, ?2, ?3, ?4, NULL)",
            rusqlite::params![slug, status, sandbox_path, files_json],
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
    fn content_required_when_missing_or_non_string() {
        let c = db();
        assert!(fileWrite(&json!({ "path": "a.txt" }), &c).unwrap_err().contains("content is required"));
        assert!(fileWrite(&json!({ "path": "a.txt", "content": 5 }), &c).unwrap_err().contains("content is required"));
    }

    #[test]
    fn validate_path_rejects_absolute_traversal_blacklist_and_prefix() {
        assert!(validatePath(&json!("C:/etc/passwd"), &Value::Null).unwrap_err().contains("absolute"));
        assert!(validatePath(&json!("../etc/passwd"), &Value::Null).unwrap_err().contains("traversal"));
        assert!(validatePath(&json!(".git/HEAD"), &Value::Null).unwrap_err().contains("blacklisted prefix"));
        assert!(validatePath(&json!(".antiloki/state.sqlite"), &Value::Null).unwrap_err().contains("system-managed"));
        assert!(validatePath(&json!("bin/evil.mjs"), &json!([".antiloki/skills/"])).unwrap_err().contains("allowed prefix"));
        assert!(validatePath(&json!(""), &Value::Null).unwrap_err().contains("path is required"));
    }

    #[test]
    fn writes_new_file_creating_parent_dir_when_no_contract_slug() {
        let dir = temp_dir("create");
        let c = db();
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            fileWrite(&json!({ "path": ".antiloki/skills/new.mjs", "content": "export default 1;\n" }), &c).unwrap()
        });
        assert_eq!(r["created"], json!(true));
        assert_eq!(r["before_sha256"], Value::Null);
        assert_eq!(r["before_bytes"], Value::Null);
        assert_eq!(r["after_bytes"], json!(18));
        assert_eq!(r["lines"], json!(2));
        assert_eq!(r["after_sha256"], json!(sha256_hex(b"export default 1;\n")));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["path", "created", "before_sha256", "after_sha256", "before_bytes", "after_bytes", "lines"]);
        assert!(dir.join(".antiloki/skills/new.mjs").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn expected_sha256_mismatch_blocks_overwrite_and_match_allows_it() {
        let dir = temp_dir("concurrency");
        let c = db();
        with_substrate_root(dir.to_str().unwrap(), || {
            let first = fileWrite(&json!({ "path": "x.txt", "content": "v1" }), &c).unwrap();
            fs::write(dir.join("x.txt"), "v2-from-other-process").unwrap();
            let mut stale_req = json!({ "path": "x.txt", "content": "v3" });
            stale_req["expected_sha256"] = first["after_sha256"].clone();
            let e = fileWrite(&stale_req, &c).unwrap_err();
            assert!(e.contains("optimistic concurrency check failed"), "{e}");
            assert_eq!(fs::read_to_string(dir.join("x.txt")).unwrap(), "v2-from-other-process");

            let cur_sha = sha256_hex(b"v2-from-other-process");
            let second = fileWrite(&json!({ "path": "x.txt", "content": "v3", "expected_sha256": cur_sha }), &c).unwrap();
            assert_eq!(second["created"], json!(false));
            assert_eq!(fs::read_to_string(dir.join("x.txt")).unwrap(), "v3");
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn contract_slug_writes_into_sandbox_and_blocks_hard_blacklist() {
        let dir = temp_dir("sandbox");
        let c = db();
        seed_contract(&c, "C-1", "impl", &["x.mjs", ".antiloki/auth/dev.key"], Some(".antiloki/sandbox/C-1"));
        with_substrate_root(dir.to_str().unwrap(), || {
            let r = fileWrite(&json!({ "path": "x.mjs", "content": "hi", "contract_slug": "C-1" }), &c).unwrap();
            assert_eq!(r["created"], json!(true));
            assert_eq!(
                fs::read_to_string(dir.join(".antiloki/sandbox/C-1/x.mjs")).unwrap(),
                "hi"
            );

            // Even though it's in files_touched, the hard blacklist still wins (defense-in-depth).
            let e = fileWrite(
                &json!({ "path": ".antiloki/auth/dev.key", "content": "forged", "contract_slug": "C-1" }),
                &c,
            )
            .unwrap_err();
            assert!(e.contains("blacklisted prefix") || e.contains("system-managed"), "{e}");
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn contract_slug_original_location_refused_when_status_active_without_sandbox() {
        let dir = temp_dir("active-refuse");
        let ws_root = dir.join("workspace");
        fs::create_dir_all(&ws_root).unwrap();
        let c = db();
        seed_contract(&c, "C-2", "impl", &["y.mjs"], None);
        seed_active_workspace(&c, ws_root.to_str().unwrap());
        let e = with_substrate_root(dir.to_str().unwrap(), || {
            fileWrite(&json!({ "path": "y.mjs", "content": "hi", "contract_slug": "C-2" }), &c).unwrap_err()
        });
        assert!(e.contains("refusing direct write to original"), "{e}");
        assert!(!ws_root.join("y.mjs").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn contract_slug_original_location_allowed_when_status_not_active() {
        let dir = temp_dir("draft-allow");
        let ws_root = dir.join("workspace");
        fs::create_dir_all(&ws_root).unwrap();
        let c = db();
        seed_contract(&c, "C-3", "draft", &["z.mjs"], None);
        seed_active_workspace(&c, ws_root.to_str().unwrap());
        let r = with_substrate_root(dir.to_str().unwrap(), || {
            fileWrite(&json!({ "path": "z.mjs", "content": "hi", "contract_slug": "C-3" }), &c).unwrap()
        });
        assert_eq!(r["created"], json!(true));
        assert_eq!(fs::read_to_string(ws_root.join("z.mjs")).unwrap(), "hi");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn strict_contracts_env_requires_contract_slug() {
        let c = db();
        let e = with_strict_contracts(|| {
            fileWrite(&json!({ "path": "a.txt", "content": "hi" }), &c).unwrap_err()
        });
        assert!(e.contains("ANTILOKI_STRICT_CONTRACTS=true"), "{e}");
    }
}
