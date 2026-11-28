//! god-driver-resume.rs — twin of skills/agents/god/god-driver-resume.mjs (db lane; ignores both
//! `input` and `context`, mirroring the JS `_input, _context` signature). NAMES ARE CONTRACT:
//! `godDriverResume` (default), `HALT_FILE` (the roots.mjs re-export — not a unit, see census
//! rule: only functions count; modeled here as the `halt_file()` fn since Rust has no
//! module-load-time top-level constant equivalent to a `join(homedir(), ...)` computation).
//!
//! Removes the daemon halt-file (roots.mjs `HALT_FILE` = `RUN_DIR/driver-halt`, `RUN_DIR` =
//! `HOME_DIR/run`) if present, and reports whether it was halted.
//!
//! HOME_DIR resolution mirrors the already-converted skills/external/_helpers/ollama-host.rs (no
//! `dirs`-crate dependency): `ANTILOKI_HOME` env override (plain path join, no cwd-resolve —
//! callers never pass a relative value in practice) else Node `os.homedir()` semantics (POSIX
//! `$HOME`; Windows `%USERPROFILE%` falling back to `%HOMEDRIVE%%HOMEPATH%`).
//!
//! Real fs side effect, not simulated — matches the god-card-driver-intent-set.rs precedent for
//! this same HALT_FILE mechanism. JS: `try { unlinkSync(HALT_FILE); } catch { /* ignore — race
//! with another resumer */ }` → twinned as `let _ = std::fs::remove_file(...)`.
//! `new Date().toISOString()` → frozen-clock literal `"2026-01-01T00:00:00.000Z"`.
//!
//! VET / disclosed gap: unlike god-card-driver-intent-set.rs's `halt_path`, this JS's `HALT_FILE`
//! is a fixed module-level constant imported from roots.mjs — NOT input-injectable — so there is
//! no fixture-safe way to force `was_halted=true` without a real pre-existing file at the resolved
//! path (no fs-injection lane exists yet). Every practical fixture case exercises only the
//! `was_halted=false` branch (the `existsSync`/`unlinkSync` call sites ARE reached, just never
//! observed with a present file) — same class as an honestly-uncovered guard-throw. The `halt_file`
//! output STRING, however, is unconditional (present on every call, both branches), so it must
//! byte-match the JS lane's real resolved path for ANY fixture to conform — hence the faithful
//! home-dir resolution below rather than a placeholder literal. Also note: JS resolves `HALT_FILE`
//! ONCE at module import time (roots.mjs top-level `join(...)`), so a fixture harness that reuses
//! one Node process across cases with a changing `ANTILOKI_HOME` would NOT see it take effect —
//! this Rust twin re-resolves on every call, which is a behavioral no-op difference only if the
//! harness sets `ANTILOKI_HOME` once before the JS module is first imported.

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

pub fn godDriverResume(_input: &Value, _db: &Connection) -> Result<Value, String> {
    let path = halt_file();
    let was_present = path.exists();
    if was_present {
        // JS: try { unlinkSync(HALT_FILE); } catch { /* ignore — race with another resumer */ }
        let _ = std::fs::remove_file(&path);
    }
    let mut out = Map::new();
    out.insert("resumed_at".into(), json!(crate::clock::now_iso_nodb()));
    out.insert("was_halted".into(), json!(was_present));
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
        dir.push(format!("antiloki-god-driver-resume-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn no_halt_file_reports_not_halted() {
        let dir = temp_dir("nofile");
        let r = with_home(dir.to_str().unwrap(), || godDriverResume(&json!({}), &db0()).unwrap());
        assert_eq!(r["was_halted"], json!(false));
        assert_eq!(r["resumed_at"], json!("2026-01-01T00:00:00.000Z"));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["resumed_at", "was_halted", "halt_file"]);
        let expected = dir.join("run").join("driver-halt");
        assert_eq!(r["halt_file"], json!(expected.to_string_lossy().into_owned()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn present_halt_file_is_removed_and_reported() {
        let dir = temp_dir("present");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        let halt_path = run_dir.join("driver-halt");
        fs::write(&halt_path, "halt").unwrap();
        let r = with_home(dir.to_str().unwrap(), || godDriverResume(&json!({}), &db0()).unwrap());
        assert_eq!(r["was_halted"], json!(true));
        assert!(!halt_path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_input_and_context() {
        let dir = temp_dir("ignore-input");
        let r = with_home(dir.to_str().unwrap(), || {
            godDriverResume(&json!({ "anything": "goes", "arbitrary": 1 }), &db0()).unwrap()
        });
        assert_eq!(r["was_halted"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }
}
