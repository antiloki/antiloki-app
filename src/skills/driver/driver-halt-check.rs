//! driver-halt-check.rs — twin of skills/driver/driver-halt-check.mjs (db lane; ignores
//! `context`, mirroring the JS `input, _context` signature — pure read-only fs check, no DB).
//! NAMES ARE CONTRACT: `driverHaltCheck` (default). Reports whether the driver halt-file
//! exists: `{ halted, path }` where `path = input?.halt_path ?? DEFAULT_HALT_PATH`.
//!
//! `DEFAULT_HALT_PATH` (JS: `HALT_FILE_PATH` = roots.mjs `HALT_FILE` = `RUN_DIR/driver-halt`,
//! `RUN_DIR` = `HOME_DIR/run`) — HOME_DIR resolution mirrors the already-converted
//! skills/agents/god/god-driver-resume.rs / skills/external/_helpers/ollama-host.rs precedent
//! for this exact mechanism (no `dirs`-crate dependency): `ANTILOKI_HOME` env override (plain
//! path join, no cwd-resolve — callers never pass a relative value in practice) else Node
//! `os.homedir()` semantics (POSIX `$HOME`; Windows `%USERPROFILE%` falling back to
//! `%HOMEDRIVE%%HOMEPATH%`).
//!
//! `halt_path` is input-injectable (unlike god-driver-resume.rs's fixed constant), so — like
//! god-card-driver-intent-set.rs's `halt_path` — BOTH branches (halted=true/false) are fully
//! fixture-testable via a real pre-existing file at an injected path; no honestly-uncovered gap.
//!
//! `fs.existsSync` never throws in Node (confirmed empirically: `existsSync(123)`,
//! `existsSync(null)`, `existsSync({})` all return `false`, only a deprecation warning) — so this
//! twin is total, never `Err`. The output `path` field echoes the raw `input.halt_path` value
//! verbatim (JS does not coerce it to a string before returning), including non-string JSON
//! types; `existsSync`-equivalence for a non-string path is `false` (mirrors the real runtime
//! behavior above), same as any non-existent path.
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
fn default_halt_path() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("run").join("driver-halt")
}

// = JS `x ?? y` right-operand selection (null/undefined treated as absent).
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

pub fn driverHaltCheck(input: &Value, _db: &Connection) -> Result<Value, String> {
    let halt_path_input = nz(input.get("halt_path"));
    let path_value: Value = halt_path_input
        .cloned()
        .unwrap_or_else(|| json!(default_halt_path().to_string_lossy().into_owned()));

    // fs.existsSync never throws; for a non-string arg it returns false (Node coerces/validates
    // internally and swallows the error) — mirrored here by treating "not a string" as absent.
    let halted = match path_value.as_str() {
        Some(s) => std::path::Path::new(s).exists(),
        None => false,
    };

    let mut out = Map::new();
    out.insert("halted".into(), json!(halted));
    out.insert("path".into(), path_value);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    fn temp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "antiloki-halt-test-rs-{}-{}-{tag}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    #[test]
    fn returns_halted_false_when_file_absent() {
        let path = temp_path("absent");
        if path.exists() {
            fs::remove_file(&path).unwrap();
        }
        let r = driverHaltCheck(&json!({ "halt_path": path.to_string_lossy() }), &db0()).unwrap();
        assert_eq!(r["halted"], json!(false));
        assert_eq!(r["path"], json!(path.to_string_lossy().into_owned()));
    }

    #[test]
    fn returns_halted_true_when_file_present() {
        let path = temp_path("present");
        fs::write(&path, "").unwrap();
        let r = driverHaltCheck(&json!({ "halt_path": path.to_string_lossy() }), &db0()).unwrap();
        assert_eq!(r["halted"], json!(true));
        assert_eq!(r["path"], json!(path.to_string_lossy().into_owned()));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn default_path_resolves_under_dot_antiloki_run() {
        let r = driverHaltCheck(&json!({}), &db0()).unwrap();
        let path = r["path"].as_str().unwrap().replace('\\', "/");
        assert!(path.ends_with(".antiloki/run/driver-halt"), "got: {path}");
    }

    #[test]
    fn null_halt_path_falls_back_to_default() {
        let r = driverHaltCheck(&json!({ "halt_path": Value::Null }), &db0()).unwrap();
        let path = r["path"].as_str().unwrap().replace('\\', "/");
        assert!(path.ends_with(".antiloki/run/driver-halt"), "got: {path}");
    }
}
