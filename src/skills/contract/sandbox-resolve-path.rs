//! sandbox-resolve-path.rs — twin of skills/contract/sandbox-resolve-path.mjs (db lane).
//! NAMES ARE CONTRACT: `sandboxResolvePath` (default). Resolves the absolute path a contract's
//! DEV agent should read/write for a given repo-relative `relpath`, enforcing that `relpath` is
//! declared in `contract.files_touched` (the single chokepoint keeping DEV inside contract scope).
//!
//! Precedence: worktree_path (git worktree, absolute) > sandbox_path (COW scratch under
//! SUBSTRATE_ROOT) > original (workspace root) — mirrors skills/engine/roots.mjs's root taxonomy.
//!
//! Cross-module: `contractGet` (already converted, `contract_contract_get::contractGet`) and
//! `getWorkspaceRoot` (same batch, `engine_workspace_root::getWorkspaceRoot` — db-first helper,
//! `fn(db) -> Value` per the playbook's db-first-helper lane; the JS call site never hits its own
//! `context.db missing` throw since `db` is always a live connection here).
//!
//! Real-fs note: the `worktree_path` branch calls `existsSync` on a path resolved against
//! WORKTREES_DIR (`HOME_DIR/worktrees`, roots.mjs — inlined below per the shared brief's lesson 3,
//! reading ANTILOKI_HOME at call time rather than importing roots.mjs). That existsSync call is a
//! REAL filesystem read with no conform-db fixture equivalent (no fs-injection lane) — but it only
//! fires when `contract.worktree_path` is a truthy DB value, which the JS docstring itself notes is
//! "NULL when ANTILOKI_WORKTREE is off → this branch is dead" in normal operation. The other two
//! success branches (sandbox / original) never touch the filesystem, so they ARE fully conform-db
//! fixturable (this unit's testing_lane); the worktree branch is additionally proven below via
//! bespoke #[cfg(test)] ANTILOKI_HOME + tempdir tests (god-driver-resume.rs `with_home()` pattern),
//! matching lesson 3's guidance for a branch conform-db structurally cannot reach.
//!
//! `.mjs` `resolve(base, ...segments)` (node:path) normalizes `/`/`\` uniformly and collapses `.`
//! components; twinned here as `node_join`, matching source-list.rs's precedent for `join()`
//! (PathBuf::push alone keeps an embedded "/" literal in one component, diverging from node:path).
//! Every segment reaching `node_join` in the fixturable branches has already been validated
//! repo-relative + traversal-free (relpath) or is a trusted DB column (sandbox_path), so a full
//! `..`-collapsing resolver is not required for byte-identical output on realistic inputs.
//!
//! Throws (NOT_FOUND / PERMISSION_DENIED / VALIDATION_LOGIC / VALIDATION_REQUIRED) are guard
//! branches — per the playbook's throw-fidelity gotcha the fixture only judges the SUCCESS path,
//! so exact V8 message text is reproduced for audit fidelity but not fixture-compared.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
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
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

// node:path `resolve(base, rel)` / `join(base, rel)` — normalizes `/` and `\` uniformly and drops
// `.` components. Segments here never carry `..` in the fixturable branches (validated upstream).
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

fn is_windows_drive_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

// node:path win32 `isAbsolute` (this checkout's host platform): root-relative (`/x`, `\x`) or
// drive-absolute (`C:/x`, `C:\x`).
fn is_absolute(s: &str) -> bool {
    s.starts_with('/') || s.starts_with('\\') || is_windows_drive_absolute(s)
}

// JS: `/^[A-Z]:/i.test(relpath)` — a bare drive-letter prefix, WITHOUT requiring a following
// separator (broader than is_windows_drive_absolute; kept as a distinct check to mirror the JS
// source's belt-and-suspenders regex verbatim).
fn starts_with_drive_letter(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

#[cfg(windows)]
fn homedir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let drive = std::env::var("HOMEDRIVE").ok().filter(|s| !s.is_empty());
    let path = std::env::var("HOMEPATH").ok().filter(|s| !s.is_empty());
    match (drive, path) {
        (Some(d), Some(p)) => Some(PathBuf::from(format!("{d}{p}"))),
        _ => None,
    }
}

#[cfg(not(windows))]
fn homedir() -> Option<PathBuf> {
    std::env::var("HOME").ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

// roots.mjs: HOME_DIR = resolve($ANTILOKI_HOME) if set+truthy else join(homedir(), '.antiloki');
// WORKTREES_DIR = join(HOME_DIR, 'worktrees') — a DIRECT child of HOME_DIR (sibling of RUN_DIR,
// NOT nested under it). Same no-`dirs`-crate convention as god-driver-resume.rs /
// orphan-sandbox-dirs.rs.
fn worktrees_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("worktrees")
}

// JS: SUBSTRATE_ROOT = resolve(ENGINE_ROOT, '..') anchored at roots.mjs's own file location
// (antiloki-core's parent), overridable via ANTILOKI_SUBSTRATE_ROOT. CARGO_MANIFEST_DIR here is
// antiloki-core-rust; its parent is that same SUBSTRATE_ROOT on this checkout layout
// (source-list.rs / source-get.rs precedent).
fn substrate_root() -> PathBuf {
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

pub fn sandboxResolvePath(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing(...)` — unreachable in
    // the db lane (a connection is always injected), mirrors contract-get.rs / source-get.rs.
    let empty = json!({});
    let input = if input.is_object() { input } else { &empty };

    // JS: `input?.contract_slug ?? input?.slug` / `input?.relpath ?? input?.path`.
    let slug = nz(input.get("contract_slug")).or_else(|| nz(input.get("slug"))).cloned();
    let relpath = nz(input.get("relpath")).or_else(|| nz(input.get("path"))).cloned();

    if !truthy(slug.as_ref()) {
        return Err("sandbox-resolve-path: contract_slug is required".into());
    }
    if !truthy(relpath.as_ref()) {
        return Err("sandbox-resolve-path: relpath is required".into());
    }
    let slug = slug.unwrap();
    let relpath = relpath.unwrap();
    let slug_str = slug.as_str().unwrap_or("").to_string();
    let relpath_str = relpath.as_str().unwrap_or("").to_string();

    // Security: reject absolute paths + path-traversal BEFORE touching the DB.
    if is_absolute(&relpath_str) || relpath_str.contains("..") || starts_with_drive_letter(&relpath_str) {
        return Err(format!(
            "sandbox-resolve-path: relpath must be repo-relative without '..': got '{relpath_str}'"
        ));
    }

    let contract = crate::contract_contract_get::contractGet(&slug, db)?;
    if contract.is_null() {
        return Err(format!("sandbox-resolve-path: contract '{slug_str}' not found"));
    }

    // JS: `Array.isArray(contract.files_touched) ? contract.files_touched : []`.
    let files_touched: Vec<String> = contract
        .get("files_touched")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    if !files_touched.iter().any(|f| f == &relpath_str) {
        return Err(format!(
            "sandbox-resolve-path: path '{relpath_str}' is not in contract '{slug_str}' files_touched (out-of-scope)"
        ));
    }

    let worktree_path = contract.get("worktree_path").cloned();
    if truthy(worktree_path.as_ref()) {
        let wt_str = worktree_path.as_ref().and_then(|v| v.as_str()).unwrap_or("").to_string();
        // JS `resolve(contract.worktree_path)`: normalizes a single path; treated as already
        // absolute here since the docstring guarantees worktree_path is absolute in practice
        // (real fs check below, not conform-db fixtured — see module doc).
        let wt = PathBuf::from(&wt_str);
        let worktrees_root = worktrees_dir();
        if !wt.starts_with(&worktrees_root) || !wt.exists() {
            return Err(format!(
                "sandbox-resolve-path: contract '{slug_str}' worktree_path is not a live worktree under .antiloki/worktrees (stale or escaped): {wt_str}"
            ));
        }
        let mut out = Map::new();
        out.insert(
            "absolute_path".into(),
            json!(node_join(&wt, &relpath_str).to_string_lossy().into_owned()),
        );
        out.insert("relpath".into(), relpath);
        out.insert("location".into(), json!("worktree"));
        out.insert(
            "sandbox_path".into(),
            contract.get("sandbox_path").cloned().unwrap_or(Value::Null),
        );
        return Ok(Value::Object(out));
    }

    let sandbox_path = contract.get("sandbox_path").cloned();
    if truthy(sandbox_path.as_ref()) {
        let sb_str = sandbox_path.as_ref().and_then(|v| v.as_str()).unwrap_or("").to_string();
        let abs = node_join(&node_join(&substrate_root(), &sb_str), &relpath_str);
        let mut out = Map::new();
        out.insert("absolute_path".into(), json!(abs.to_string_lossy().into_owned()));
        out.insert("relpath".into(), relpath);
        out.insert("location".into(), json!("sandbox"));
        out.insert("sandbox_path".into(), sandbox_path.unwrap());
        return Ok(Value::Object(out));
    }

    let workspace_root = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root_str = workspace_root.as_str().unwrap_or("").to_string();
    let abs = node_join(Path::new(&root_str), &relpath_str);
    let mut out = Map::new();
    out.insert("absolute_path".into(), json!(abs.to_string_lossy().into_owned()));
    out.insert("relpath".into(), relpath);
    out.insert("location".into(), json!("original"));
    out.insert("sandbox_path".into(), Value::Null);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

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

    #[test]
    fn guards_reject() {
        let c = db();
        assert!(sandboxResolvePath(&json!({ "relpath": "a" }), &c).is_err()); // missing slug
        assert!(sandboxResolvePath(&json!({ "contract_slug": "X" }), &c).is_err()); // missing relpath

        seed_contract(&c, "C-1", &["../../etc/passwd"], None, None);
        assert!(sandboxResolvePath(&json!({ "contract_slug": "C-1", "relpath": "../../etc/passwd" }), &c).is_err());
        assert!(sandboxResolvePath(&json!({ "contract_slug": "C-1", "relpath": "/etc/passwd" }), &c).is_err());
        assert!(sandboxResolvePath(&json!({ "contract_slug": "C-1", "relpath": "C:/Windows/system32" }), &c).is_err());

        assert!(sandboxResolvePath(&json!({ "contract_slug": "NOPE", "relpath": "a.mjs" }), &c).is_err()); // not found

        seed_contract(&c, "C-SCOPE", &["src/in-scope.mjs"], None, None);
        assert!(sandboxResolvePath(&json!({ "contract_slug": "C-SCOPE", "relpath": "src/out-of-scope.mjs" }), &c).is_err());
    }

    #[test]
    fn returns_original_path_when_no_sandbox() {
        let c = db();
        seed_contract(&c, "C-NOSB", &["src/x.mjs"], None, None);
        let r = sandboxResolvePath(&json!({ "contract_slug": "C-NOSB", "relpath": "src/x.mjs" }), &c).unwrap();
        assert_eq!(r["location"], json!("original"));
        assert_eq!(r["sandbox_path"], Value::Null);
        assert_eq!(r["relpath"], json!("src/x.mjs"));
        let expected = substrate_root().join("src").join("x.mjs");
        assert_eq!(r["absolute_path"], json!(expected.to_string_lossy().into_owned()));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, vec!["absolute_path", "relpath", "location", "sandbox_path"]);
    }

    #[test]
    fn returns_sandbox_path_when_set() {
        let c = db();
        seed_contract(&c, "C-SB", &["src/x.mjs"], Some(".antiloki/sandbox/C-SB"), None);
        let r = sandboxResolvePath(&json!({ "contract_slug": "C-SB", "relpath": "src/x.mjs" }), &c).unwrap();
        assert_eq!(r["location"], json!("sandbox"));
        assert_eq!(r["sandbox_path"], json!(".antiloki/sandbox/C-SB"));
        let expected = substrate_root().join(".antiloki").join("sandbox").join("C-SB").join("src").join("x.mjs");
        assert_eq!(r["absolute_path"], json!(expected.to_string_lossy().into_owned()));
    }

    #[test]
    fn input_accepts_relpath_and_path_aliases() {
        let c = db();
        seed_contract(&c, "C-ALIAS", &["a.mjs"], None, None);
        let r1 = sandboxResolvePath(&json!({ "contract_slug": "C-ALIAS", "relpath": "a.mjs" }), &c).unwrap();
        let r2 = sandboxResolvePath(&json!({ "contract_slug": "C-ALIAS", "path": "a.mjs" }), &c).unwrap();
        assert_eq!(r1, r2);
    }

    #[test]
    fn input_accepts_contract_slug_and_slug_aliases() {
        let c = db();
        seed_contract(&c, "C-SLUG-ALIAS", &["b.mjs"], None, None);
        let r1 = sandboxResolvePath(&json!({ "contract_slug": "C-SLUG-ALIAS", "relpath": "b.mjs" }), &c).unwrap();
        let r2 = sandboxResolvePath(&json!({ "slug": "C-SLUG-ALIAS", "relpath": "b.mjs" }), &c).unwrap();
        assert_eq!(r1, r2);
    }

    // ── worktree branch: real fs I/O (existsSync-equivalent), not conform-db fixturable — proven
    // natively here via ANTILOKI_HOME + tempdir injection (god-driver-resume.rs `with_home()`).
    // shared crate-wide lock (test_env_lock.rs) — a local mutex here would not prevent a race with OTHER files' tests over the same real env var.

    fn with_home<T>(antiloki_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", antiloki_home);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-sandbox-resolve-path-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn worktree_path_takes_precedence_when_live() {
        let dir = temp_dir("live");
        let wt = dir.join("worktrees").join("CARD-1");
        fs::create_dir_all(&wt).unwrap();

        let c = db();
        seed_contract(&c, "C-WT", &["src/x.mjs"], None, Some(wt.to_string_lossy().as_ref()));

        let r = with_home(dir.to_str().unwrap(), || {
            sandboxResolvePath(&json!({ "contract_slug": "C-WT", "relpath": "src/x.mjs" }), &c).unwrap()
        });
        assert_eq!(r["location"], json!("worktree"));
        let expected = wt.join("src").join("x.mjs");
        assert_eq!(r["absolute_path"], json!(expected.to_string_lossy().into_owned()));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn worktree_path_stale_or_escaped_errors() {
        let dir = temp_dir("stale");
        // never created on disk -> !exists()
        let missing = dir.join("worktrees").join("CARD-2");

        let c = db();
        seed_contract(&c, "C-WT2", &["src/x.mjs"], None, Some(missing.to_string_lossy().as_ref()));

        let r = with_home(dir.to_str().unwrap(), || {
            sandboxResolvePath(&json!({ "contract_slug": "C-WT2", "relpath": "src/x.mjs" }), &c)
        });
        assert!(r.is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn worktree_path_outside_worktrees_root_errors() {
        let dir = temp_dir("escaped");
        let outside = dir.join("elsewhere");
        fs::create_dir_all(&outside).unwrap();

        let c = db();
        seed_contract(&c, "C-WT3", &["src/x.mjs"], None, Some(outside.to_string_lossy().as_ref()));

        let r = with_home(dir.to_str().unwrap(), || {
            sandboxResolvePath(&json!({ "contract_slug": "C-WT3", "relpath": "src/x.mjs" }), &c)
        });
        assert!(r.is_err());

        let _ = fs::remove_dir_all(&dir);
    }
}
