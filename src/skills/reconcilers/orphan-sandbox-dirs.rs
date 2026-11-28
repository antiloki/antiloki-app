//! orphan-sandbox-dirs.rs — twin of skills/reconcilers/orphan-sandbox-dirs.mjs
//! (reconciler-object lane). NAMES ARE CONTRACT: the units are the default object's
//! METHODS `detect(db)` and `fix(finding)`.
//!
//! Sweeps `.antiloki/run/sandboxes/<slug>/` for directories whose contract row is
//! either absent or in a terminal status (done/rejected/superseded), and removes
//! them on fix.
//!
//! REAL FILESYSTEM SIDE EFFECT — not fixture-testable through the conform-db two-lane
//! harness (no real-fs-mutation assertion there, and unlike the orphan-worktree-dirs
//! sibling this JS does not expose an injectable `root` param for detect()). Per the
//! shared brief lesson 3, this twin is proven via bespoke #[cfg(test)] Rust-native
//! tests using ANTILOKI_HOME env-var injection + a real tempdir, copying the
//! `with_home()` pattern from god-driver-resume.rs exactly.
//!
//! HOME_DIR resolution mirrors god-driver-resume.rs / skills/external/_helpers/
//! ollama-host.rs (no `dirs`-crate dependency): `ANTILOKI_HOME` env override (plain
//! path join) else Node `os.homedir()` semantics (POSIX `$HOME`; Windows
//! `%USERPROFILE%` falling back to `%HOMEDRIVE%%HOMEPATH%`). roots.mjs:
//! `HOME_DIR = resolve($ANTILOKI_HOME) if set+truthy else join(homedir(), '.antiloki')`,
//! `RUN_DIR = join(HOME_DIR, 'run')`, and this module's own
//! `SANDBOX_ROOT = resolve(RUN_DIR, 'sandboxes')`.
//!
//! JS `detect(db)` iterates `readdirSync(SANDBOX_ROOT, { withFileTypes: true })` in
//! OS directory order (unspecified, not guaranteed sorted). This twin sorts entries
//! by name for deterministic tests — a safe divergence since this file is proven via
//! bespoke native tests, not a cross-language byte-for-byte comparison.
//!
//! JS `db.prepare(...).get(slug)` inside a try/catch: a real prepare/query error
//! (e.g. the `contracts` table absent) hits `catch { continue }` — the entry is
//! skipped with NO finding. A query that runs cleanly but returns no row is the
//! `!row` branch — DOES produce a finding. Rust mirrors this: `QueryReturnedNoRows`
//! maps to `None` (finding path); any other `rusqlite::Error` maps to `continue`
//! (skip, no finding).
//!
//! JS `fix(finding)` calls `rmSync(path, { recursive: true, force: true })` —
//! `force: true` means a missing path does NOT throw. Rust's `remove_dir_all`
//! DOES error with `NotFound` for a missing path, so this twin explicitly treats
//! `NotFound` as a successful no-op removal to match the JS force semantics.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

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

// roots.mjs: HOME_DIR = resolve($ANTILOKI_HOME) if set+truthy, else join(homedir(), '.antiloki');
// RUN_DIR = join(HOME_DIR, 'run'). orphan-sandbox-dirs.mjs: SANDBOX_ROOT = resolve(RUN_DIR, 'sandboxes').
fn sandbox_root() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("run").join("sandboxes")
}

const TERMINAL_STATUSES: [&str; 3] = ["done", "rejected", "superseded"];

/// JS: `async detect(db)`.
pub fn detect(db: &Connection) -> Value {
    let root = sandbox_root();
    if !root.exists() {
        return json!([]);
    }
    let mut entries: Vec<(String, PathBuf)> = match std::fs::read_dir(&root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect(),
        Err(_) => return json!([]),
    };
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut findings: Vec<Value> = Vec::new();
    for (slug, path) in entries {
        let path_str = path.to_string_lossy().into_owned();
        let status: Option<String> = match db.query_row(
            "SELECT status FROM contracts WHERE slug = ?1",
            rusqlite::params![slug],
            |r| r.get::<usize, String>(0),
        ) {
            Ok(s) => Some(s),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(_) => continue, // JS: catch { continue } — table absent / real query error
        };

        match status {
            None => {
                let mut detail = Map::new();
                detail.insert("path".into(), json!(path_str));
                detail.insert("reason".into(), json!("no contract row"));

                let mut f = Map::new();
                f.insert("kind".into(), json!("orphan-sandbox-dir"));
                f.insert("severity".into(), json!("cleanup"));
                f.insert(
                    "message".into(),
                    json!(format!("sandbox dir '{slug}' has no matching contract row")),
                );
                f.insert("contract_slug".into(), json!(slug));
                f.insert("detail".into(), Value::Object(detail));
                f.insert("suggested_action".into(), json!("rmdir"));
                findings.push(Value::Object(f));
            }
            Some(status) if TERMINAL_STATUSES.contains(&status.as_str()) => {
                let mut detail = Map::new();
                detail.insert("path".into(), json!(path_str));
                detail.insert("reason".into(), json!(format!("contract status={status}")));

                let mut f = Map::new();
                f.insert("kind".into(), json!("orphan-sandbox-dir"));
                f.insert("severity".into(), json!("cleanup"));
                f.insert(
                    "message".into(),
                    json!(format!(
                        "sandbox dir '{slug}' belongs to contract in terminal status='{status}'"
                    )),
                );
                f.insert("contract_slug".into(), json!(slug));
                f.insert("detail".into(), Value::Object(detail));
                f.insert("suggested_action".into(), json!("rmdir"));
                findings.push(Value::Object(f));
            }
            Some(_) => {} // active status — left alone
        }
    }
    json!(findings)
}

/// JS: `async fix(finding)` — ignores `db` (not even declared as a param); this twin
/// keeps the uniform (finding, db) call shape used across the reconciler registry
/// (see stuck-contracts.rs's `fix(_finding, _db)`) but never touches `_db`.
pub fn fix(finding: &Value, _db: &Connection) -> Value {
    let path = finding
        .get("detail")
        .and_then(|d| d.get("path"))
        .and_then(|p| p.as_str())
        .filter(|p| !p.is_empty());

    let path = match path {
        Some(p) => p,
        None => return json!({ "applied": false, "reason": "missing path" }),
    };

    match std::fs::remove_dir_all(path) {
        Ok(_) => json!({ "applied": true, "changes": { "files_removed": [path] } }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // JS rmSync(path, { force: true }) does not throw when the path is already gone.
            json!({ "applied": true, "changes": { "files_removed": [path] } })
        }
        Err(e) => json!({ "applied": false, "reason": e.to_string() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;

    // SAFETY: ANTILOKI_HOME is process-global; serialize since `cargo test` runs threads in
    // parallel by default, and this same var is read by other converted twins' tests too.
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
        dir.push(format!("antiloki-orphan-sandbox-dirs-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE contracts (slug TEXT, status TEXT)").unwrap();
        c
    }

    #[test]
    fn no_sandbox_root_yields_empty_array() {
        let dir = temp_dir("noroot");
        let r = with_home(dir.to_str().unwrap(), || detect(&db()));
        assert_eq!(r, json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn table_absent_skips_every_entry_with_no_findings() {
        let dir = temp_dir("noTable");
        let sandboxes = dir.join("run").join("sandboxes");
        fs::create_dir_all(sandboxes.join("some-slug")).unwrap();
        let empty_db = Connection::open_in_memory().unwrap(); // no contracts table
        let r = with_home(dir.to_str().unwrap(), || detect(&empty_db));
        assert_eq!(r, json!([])); // JS: catch { continue } — no finding, not a crash
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn flags_missing_row_and_terminal_status_leaves_active_alone() {
        let dir = temp_dir("mixed");
        let sandboxes = dir.join("run").join("sandboxes");
        fs::create_dir_all(sandboxes.join("no-row-slug")).unwrap();
        fs::create_dir_all(sandboxes.join("done-slug")).unwrap();
        fs::create_dir_all(sandboxes.join("active-slug")).unwrap();
        // a stray FILE (not a dir) inside sandboxes must be ignored (JS filters isDirectory())
        fs::write(sandboxes.join("stray-file.txt"), "x").unwrap();

        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, status) VALUES ('done-slug', 'done')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO contracts (slug, status) VALUES ('active-slug', 'impl')",
            [],
        )
        .unwrap();

        let out = with_home(dir.to_str().unwrap(), || detect(&c));
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2); // active-slug excluded; stray file excluded

        // sorted by slug name: active-slug excluded, so order is done-slug, no-row-slug
        assert_eq!(arr[0]["contract_slug"], json!("done-slug"));
        assert_eq!(arr[0]["kind"], json!("orphan-sandbox-dir"));
        assert_eq!(arr[0]["severity"], json!("cleanup"));
        assert_eq!(
            arr[0]["message"],
            json!("sandbox dir 'done-slug' belongs to contract in terminal status='done'")
        );
        assert_eq!(arr[0]["detail"]["reason"], json!("contract status=done"));
        assert_eq!(arr[0]["suggested_action"], json!("rmdir"));
        let expected_path = sandboxes.join("done-slug").to_string_lossy().into_owned();
        assert_eq!(arr[0]["detail"]["path"], json!(expected_path));

        assert_eq!(arr[1]["contract_slug"], json!("no-row-slug"));
        assert_eq!(
            arr[1]["message"],
            json!("sandbox dir 'no-row-slug' has no matching contract row")
        );
        assert_eq!(arr[1]["detail"]["reason"], json!("no contract row"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejected_and_superseded_are_also_terminal() {
        let dir = temp_dir("terminals");
        let sandboxes = dir.join("run").join("sandboxes");
        fs::create_dir_all(sandboxes.join("rej-slug")).unwrap();
        fs::create_dir_all(sandboxes.join("sup-slug")).unwrap();
        let c = db();
        c.execute("INSERT INTO contracts (slug, status) VALUES ('rej-slug', 'rejected')", []).unwrap();
        c.execute("INSERT INTO contracts (slug, status) VALUES ('sup-slug', 'superseded')", []).unwrap();
        let out = with_home(dir.to_str().unwrap(), || detect(&c));
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fix_missing_path_is_a_noop_failure() {
        let r = fix(&json!({}), &Connection::open_in_memory().unwrap());
        assert_eq!(r, json!({ "applied": false, "reason": "missing path" }));

        let r2 = fix(&json!({ "detail": {} }), &Connection::open_in_memory().unwrap());
        assert_eq!(r2, json!({ "applied": false, "reason": "missing path" }));
    }

    #[test]
    fn fix_removes_an_existing_directory_recursively() {
        let dir = temp_dir("fix-existing");
        let target = dir.join("some-slug");
        fs::create_dir_all(target.join("nested")).unwrap();
        fs::write(target.join("nested").join("f.txt"), "x").unwrap();

        let path_str = target.to_string_lossy().into_owned();
        let r = fix(
            &json!({ "detail": { "path": path_str } }),
            &Connection::open_in_memory().unwrap(),
        );
        assert_eq!(r["applied"], json!(true));
        assert_eq!(r["changes"]["files_removed"], json!([path_str]));
        assert!(!target.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fix_on_already_missing_path_is_still_applied_true() {
        // JS rmSync(path, { force: true }) does not throw for a missing path.
        let dir = temp_dir("fix-missing");
        let ghost = dir.join("never-existed");
        let path_str = ghost.to_string_lossy().into_owned();
        let r = fix(
            &json!({ "detail": { "path": path_str } }),
            &Connection::open_in_memory().unwrap(),
        );
        assert_eq!(r["applied"], json!(true));
        let _ = fs::remove_dir_all(&dir);
    }
}
