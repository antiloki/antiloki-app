//! driver-halt.rs — twin of skills/system/driver-halt.mjs (db lane; ignores `context`,
//! mirroring the JS `input, _context` signature). NAMES ARE CONTRACT: `driverHalt` (default).
//!
//! Toggles the daemon halt-file (roots.mjs `HALT_FILE` = `RUN_DIR/driver-halt`, `RUN_DIR` =
//! `HOME_DIR/run`) on/off based on `input.halt` (required boolean):
//!   - `halt: true`  → create the parent dir (recursive) + write the current ISO timestamp,
//!                     return `{ halted: true }`.
//!   - `halt: false` → remove the file if present (idempotent — ignore a missing file / a
//!                     race where it disappears between the exists-check and the unlink),
//!                     return `{ halted: false }`.
//!   - anything else → throw (ported as `Err`) — JS: `err.required('driver-halt', 'halt
//!     (boolean)')`, whose `AntilokiError` message is `"driver-halt: halt (boolean) is
//!     required"` (verified against the real JS via `node --input-type=module -e`).
//!
//! DIFFERENT from the sibling `agents/god/god-driver-halt.rs` / `god-driver-resume.rs`: those
//! are unconditional write/remove twins that ignore `input` entirely; this one is a single
//! input-driven toggle. HOME_DIR resolution mirrors that same precedent (no `dirs`-crate
//! dependency): `ANTILOKI_HOME` env override (plain path join, no cwd-resolve — callers never
//! pass a relative value in practice) else Node `os.homedir()` semantics (POSIX `$HOME`;
//! Windows `%USERPROFILE%` falling back to `%HOMEDRIVE%%HOMEPATH%`).
//!
//! `new Date().toISOString()` → frozen-clock literal `"2026-01-01T00:00:00.000Z"` (per the
//! playbook's frozen-clock lane).
//!
//! Real fs side effect (mkdir + write / unlink at a HOME-relative path resolved from an env
//! var) — not fixture-testable through the normal conform-db harness (no real-fs-mutation
//! assertion there). Per playbook lesson 3: tested via a bespoke `#[cfg(test)]` Rust-native
//! test using env-var + tempdir injection, copying the `with_home()` pattern from
//! `god-driver-resume.rs` exactly.

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
// RUN_DIR = join(HOME_DIR, 'run'); HALT_FILE = join(RUN_DIR, 'driver-halt').
fn halt_file() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("run").join("driver-halt")
}

pub fn driverHalt(input: &Value, _db: &Connection) -> Result<Value, String> {
    let halt = input.get("halt");
    let halt_bool = match halt {
        Some(Value::Bool(b)) => Some(*b),
        _ => None,
    };
    let halt_bool = match halt_bool {
        Some(b) => b,
        None => return Err("driver-halt: halt (boolean) is required".to_string()),
    };

    let path = halt_file();
    let mut out = Map::new();
    if halt_bool {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, "2026-01-01T00:00:00.000Z").map_err(|e| e.to_string())?;
        out.insert("halted".into(), json!(true));
    } else {
        if path.exists() {
            // JS: try { unlinkSync(HALT_FILE); } catch { /* race: already gone */ }
            let _ = std::fs::remove_file(&path);
        }
        out.insert("halted".into(), json!(false));
    }
    Ok(Value::Object(out))
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
        dir.push(format!("antiloki-driver-halt-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn halt_true_creates_parent_dir_and_writes_file() {
        let dir = temp_dir("halt-true");
        let expected_path = dir.join("run").join("driver-halt");
        assert!(!expected_path.exists());
        let r = with_home(dir.to_str().unwrap(), || {
            driverHalt(&json!({ "halt": true }), &db0()).unwrap()
        });
        assert_eq!(r, json!({ "halted": true }));
        assert!(expected_path.exists());
        let contents = fs::read_to_string(&expected_path).unwrap();
        assert_eq!(contents, "2026-01-01T00:00:00.000Z");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn halt_false_removes_existing_file() {
        let dir = temp_dir("halt-false-present");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        let halt_path = run_dir.join("driver-halt");
        fs::write(&halt_path, "stale").unwrap();
        let r = with_home(dir.to_str().unwrap(), || {
            driverHalt(&json!({ "halt": false }), &db0()).unwrap()
        });
        assert_eq!(r, json!({ "halted": false }));
        assert!(!halt_path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn halt_false_is_idempotent_when_file_absent() {
        let dir = temp_dir("halt-false-absent");
        let r = with_home(dir.to_str().unwrap(), || {
            driverHalt(&json!({ "halt": false }), &db0()).unwrap()
        });
        assert_eq!(r, json!({ "halted": false }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_or_non_boolean_halt_returns_required_error() {
        let dir = temp_dir("halt-invalid");
        for bad in [json!({}), json!({ "halt": "true" }), json!({ "halt": 1 }), json!({ "halt": Value::Null })] {
            let r = with_home(dir.to_str().unwrap(), || driverHalt(&bad, &db0()));
            assert_eq!(r, Err("driver-halt: halt (boolean) is required".to_string()));
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn halt_true_overwrites_existing_file_with_fresh_timestamp() {
        let dir = temp_dir("halt-true-overwrite");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        let halt_path = run_dir.join("driver-halt");
        fs::write(&halt_path, "stale").unwrap();
        let r = with_home(dir.to_str().unwrap(), || {
            driverHalt(&json!({ "halt": true }), &db0()).unwrap()
        });
        assert_eq!(r, json!({ "halted": true }));
        let contents = fs::read_to_string(&halt_path).unwrap();
        assert_eq!(contents, "2026-01-01T00:00:00.000Z");
        let _ = fs::remove_dir_all(&dir);
    }
}
