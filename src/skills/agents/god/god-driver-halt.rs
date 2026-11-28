//! god-driver-halt.rs — twin of skills/agents/god/god-driver-halt.mjs (db lane; ignores both
//! `input` and `context`, mirroring the JS `_input, _context` signature). NAMES ARE CONTRACT:
//! `godDriverHalt` (default), `HALT_FILE` (the roots.mjs re-export — not a unit, see census
//! rule: only functions count; modeled here as the `halt_file()` fn, same convention as the
//! sibling god-driver-resume.rs).
//!
//! Writes the daemon halt-file (roots.mjs `HALT_FILE` = `RUN_DIR/driver-halt`, `RUN_DIR` =
//! `HOME_DIR/run`) with the current timestamp as its content, creating the parent dir first.
//!
//! HOME_DIR resolution mirrors god-driver-resume.rs (no `dirs`-crate dependency): `ANTILOKI_HOME`
//! env override (plain path join, no cwd-resolve — callers never pass a relative value in
//! practice) else Node `os.homedir()` semantics (POSIX `$HOME`; Windows `%USERPROFILE%` falling
//! back to `%HOMEDRIVE%%HOMEPATH%`).
//!
//! Real fs side effect, not simulated — same precedent as the sibling resume twin. JS:
//! `mkdirSync(dirname(HALT_FILE), { recursive: true }); const ts = new Date().toISOString();
//! writeFileSync(HALT_FILE, ts);` → twinned as `std::fs::create_dir_all` + `std::fs::write`.
//! `new Date().toISOString()` → frozen-clock literal `"2026-01-01T00:00:00.000Z"` (same
//! convention as resume.rs). Output key order matches the JS literal `{ halted_at, halt_file }`
//! (god-driver-halt.mjs ~line 17).

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

pub fn godDriverHalt(_input: &Value, _db: &Connection) -> Result<Value, String> {
    let path = halt_file();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let ts = crate::clock::now_iso_nodb();
    std::fs::write(&path, &ts).map_err(|e| e.to_string())?;
    let mut out = Map::new();
    out.insert("halted_at".into(), json!(ts));
    out.insert("halt_file".into(), json!(path.to_string_lossy().into_owned()));
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
        dir.push(format!("antiloki-god-driver-halt-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn writes_halt_file_creating_parent_dir() {
        let dir = temp_dir("create-parent");
        let r = with_home(dir.to_str().unwrap(), || godDriverHalt(&json!({}), &db0()).unwrap());
        let expected_path = dir.join("run").join("driver-halt");
        assert_eq!(r["halted_at"], json!("2026-01-01T00:00:00.000Z"));
        assert_eq!(r["halt_file"], json!(expected_path.to_string_lossy().into_owned()));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["halted_at", "halt_file"]);
        assert!(expected_path.exists());
        let contents = fs::read_to_string(&expected_path).unwrap();
        assert_eq!(contents, "2026-01-01T00:00:00.000Z");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn overwrites_existing_halt_file() {
        let dir = temp_dir("overwrite");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        let halt_path = run_dir.join("driver-halt");
        fs::write(&halt_path, "stale").unwrap();
        let r = with_home(dir.to_str().unwrap(), || godDriverHalt(&json!({}), &db0()).unwrap());
        assert_eq!(r["halted_at"], json!("2026-01-01T00:00:00.000Z"));
        let contents = fs::read_to_string(&halt_path).unwrap();
        assert_eq!(contents, "2026-01-01T00:00:00.000Z");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_input_and_context() {
        let dir = temp_dir("ignore-input");
        let r = with_home(dir.to_str().unwrap(), || {
            godDriverHalt(&json!({ "anything": "goes", "arbitrary": 1 }), &db0()).unwrap()
        });
        assert_eq!(r["halted_at"], json!("2026-01-01T00:00:00.000Z"));
        let _ = fs::remove_dir_all(&dir);
    }
}
