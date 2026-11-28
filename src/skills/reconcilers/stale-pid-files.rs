//! stale-pid-files.rs — twin of skills/reconcilers/stale-pid-files.mjs (reconciler-object
//! lane). NAMES ARE CONTRACT: the units are the default object's METHODS `detect()` and
//! `fix(finding)`.
//!
//! Unlinks `.antiloki/run/*.pid` files whose recorded pid is no longer a live process (or
//! whose content is empty/unparseable).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: `detect()` sweeps a REAL directory
//! (`RUN_DIR`, a HOME_DIR-anchored constant per roots.mjs — not DB-derived, so not
//! fixture-controllable per the crossing playbook) AND makes a REAL OS process-liveness
//! probe per candidate pid (`process.kill(pid, 0)` — genuinely non-deterministic: whether
//! pid N is alive depends on the live host process table, not seedable). Same precedent as
//! the orphan-sandbox-dirs / orphan-worktree-dirs / god-driver-halt siblings: proven via
//! #[cfg(test)] Rust-native tests using ANTILOKI_HOME env-var injection + a real tempdir,
//! copying the `with_home()` pattern from god-driver-resume.rs exactly. `detect()` here
//! takes NO db argument at all (unlike every other reconciler in this folder — JS declares
//! `async detect()` with zero params); the Rust twin keeps an unused `_db: &Connection`
//! parameter anyway, matching the uniform (db) / (finding, db) call shape the reconciler
//! registry dispatches with (see orphan-sandbox-dirs.rs's `fix(finding, _db)` for the same
//! convention).
//!
//! HOME_DIR resolution mirrors god-driver-halt.rs / orphan-sandbox-dirs.rs (no `dirs`-crate
//! dependency): `ANTILOKI_HOME` env override (plain path join) else Node `os.homedir()`
//! semantics (POSIX `$HOME`; Windows `%USERPROFILE%` falling back to
//! `%HOMEDRIVE%%HOMEPATH%`). roots.mjs: `HOME_DIR = resolve($ANTILOKI_HOME) if set+truthy
//! else join(homedir(), '.antiloki')`; `RUN_DIR = join(HOME_DIR, 'run')`.
//!
//! `isPidAlive(pid)`: JS calls `process.kill(pid, 0)` — send signal 0, the standard "does
//! this pid exist" probe: throws ESRCH if not, EPERM if it exists but the caller lacks
//! permission to signal it (EPERM is treated as ALIVE). This twin makes the equivalent raw
//! OS call directly with NO new Cargo dependency: on Unix, an `extern "C"` FFI declaration
//! of libc's `kill(2)` (linked automatically on every Unix Rust target — the same call
//! Node's libuv makes under the hood) with `EPERM == 1` mapped to alive, matching JS
//! exactly. On Windows, `process.kill(pid, 0)` under libuv is an `OpenProcess` +
//! `GetExitCodeProcess` probe; this twin declares the same two Win32 functions via `extern
//! "system"` linked against `kernel32` (always available on a Windows target, exactly like
//! `std` itself already depends on) and checks `STILL_ACTIVE`.
//!
//! JS `Number(content)` parsing of pid-file text is the playbook's standard `js_number`
//! helper, String-branch only (this module only ever calls it on a trimmed pid-file
//! string). `${pid}` (template literal on the raw JS Number, e.g. the "references dead pid
//! NaN" message) uses `js_number_to_string` — NOT `str_qq`/`tmpl`, since `pid` here is
//! always a raw f64, never a JSON Value. The `detail.pid` field (a plain in-memory JS
//! number, never itself `JSON.stringify`-ed by this module) is rendered into the returned
//! `Value` via `pid_to_json`: NaN/Infinite → `null` (exactly what serde_json's own
//! `f64`→`Value` conversion already collapses to, and what `JSON.stringify(NaN)` produces
//! downstream), integer-valued → JSON integer (gotcha 2: avoid a spurious ".0"), otherwise
//! → JSON float.
//!
//! `fix(finding)`: unlike the sibling reconcilers (which force-remove and treat "already
//! gone" as a successful no-op), this one calls plain `unlinkSync` — NOT `rmSync` with
//! `force: true` — so a missing path is a REAL failure here: `{ applied: false, reason:
//! 'already gone' }`. Rust mirrors this: `NotFound` maps to `applied:false`, not
//! `applied:true`.

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
// RUN_DIR = join(HOME_DIR, 'run').
fn run_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("run")
}

// = JS Number(v), String-branch only (playbook's standard js_number helper, specialized to
// the one shape this module ever feeds it: a trimmed pid-file string).
fn js_number_str(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) }
}

// = JS Number.prototype.toString() / `${f}` for a raw f64 (playbook standard helper,
// copied verbatim).
fn js_number_to_string(f: f64) -> String {
    if f.is_nan() { return "NaN".to_string(); }
    if f == 0.0 { return "0".to_string(); } // JS: (-0).toString() === "0"
    let neg = f.is_sign_negative();
    let abs = f.abs();
    if abs.is_infinite() { return if neg { "-Infinity".into() } else { "Infinity".into() }; }
    let sci = format!("{:e}", abs);
    let (mantissa, exp_str) = sci.split_once('e').unwrap();
    let exp: i32 = exp_str.parse().unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let body = if (-6..=20).contains(&exp) {
        if exp >= 0 {
            if exp + 1 >= k { format!("{digits}{}", "0".repeat((exp + 1 - k) as usize)) }
            else { let (i, f) = digits.split_at((exp + 1) as usize); format!("{i}.{f}") }
        } else { format!("0.{}{digits}", "0".repeat((-exp - 1) as usize)) }
    } else {
        let m = if k == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{m}e{}{}", if exp >= 0 { "+" } else { "-" }, exp.abs())
    };
    if neg { format!("-{body}") } else { body }
}

// detail.pid: a raw JS number embedded into a returned object (never itself
// JSON.stringify-ed inside this module). NaN/Infinite collapse to JSON null (the same
// place serde_json's own f64->Value conversion already lands); integer-valued renders as a
// JSON integer (gotcha 2, avoids a spurious ".0"); otherwise a JSON float.
fn pid_to_json(f: f64) -> Value {
    if !f.is_finite() { return Value::Null; }
    if f.fract() == 0.0 && f.abs() < 9.0e15 { json!(f as i64) } else { json!(f) }
}

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

// JS: process.kill(pid, 0) → EPERM (exists, no permission to signal) is treated as alive;
// any other failure (ESRCH: no such process) is dead. `pid <= 0` is guarded here too (not
// just by the caller) because POSIX kill(2) gives pid<=0 special process-GROUP semantics
// (kill(0,...) signals the caller's own group) — never what "is this specific pid alive"
// should mean, so it must never reach the raw syscall.
#[cfg(unix)]
fn is_pid_alive(pid: f64) -> bool {
    if pid <= 0.0 || pid > i32::MAX as f64 { return false; }
    let ret = unsafe { kill(pid as i32, 0) };
    if ret == 0 { return true; }
    std::io::Error::last_os_error().raw_os_error() == Some(1) // EPERM
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(dw_desired_access: u32, b_inherit_handle: i32, dw_process_id: u32) -> *mut core::ffi::c_void;
    fn CloseHandle(h_object: *mut core::ffi::c_void) -> i32;
    fn GetExitCodeProcess(h_process: *mut core::ffi::c_void, lp_exit_code: *mut u32) -> i32;
}

#[cfg(windows)]
fn is_pid_alive(pid: f64) -> bool {
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    if pid <= 0.0 || pid > u32::MAX as f64 { return false; }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
        if handle.is_null() { return false; }
        let mut exit_code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut exit_code as *mut u32);
        CloseHandle(handle);
        ok != 0 && exit_code == STILL_ACTIVE
    }
}

/// JS: `async detect()` — takes no arguments at all; `_db` kept only for the uniform
/// reconciler-registry call shape (see module doc).
pub fn detect(_db: &Connection) -> Value {
    let dir = run_dir();
    if !dir.exists() {
        return json!([]);
    }
    let mut names: Vec<String> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".pid"))
            .collect(),
        Err(_) => return json!([]),
    };
    // JS readdirSync order is OS-unspecified; sorted here for deterministic tests — safe
    // since this twin is proven via bespoke native tests, not a byte-for-byte cross-lane diff.
    names.sort();

    let mut findings: Vec<Value> = Vec::new();
    for f in names {
        let path = dir.join(&f);
        let path_str = path.to_string_lossy().into_owned();
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c.trim().to_string(),
            Err(_) => continue, // JS: catch { continue } (e.g. entry is actually a directory)
        };

        if content.is_empty() {
            let mut detail = Map::new();
            detail.insert("path".into(), json!(path_str));
            detail.insert("pid".into(), Value::Null);

            let mut fnd = Map::new();
            fnd.insert("kind".into(), json!("stale-pid-file"));
            fnd.insert("severity".into(), json!("cleanup"));
            fnd.insert("message".into(), json!(format!("pid file {f} is empty")));
            fnd.insert("detail".into(), Value::Object(detail));
            fnd.insert("suggested_action".into(), json!("unlink"));
            findings.push(Value::Object(fnd));
            continue;
        }

        let pid = js_number_str(&content);
        if !pid.is_finite() || pid <= 0.0 || !is_pid_alive(pid) {
            let mut detail = Map::new();
            detail.insert("path".into(), json!(path_str));
            detail.insert("pid".into(), pid_to_json(pid));

            let mut fnd = Map::new();
            fnd.insert("kind".into(), json!("stale-pid-file"));
            fnd.insert("severity".into(), json!("cleanup"));
            fnd.insert(
                "message".into(),
                json!(format!("pid file {f} references dead pid {}", js_number_to_string(pid))),
            );
            fnd.insert("detail".into(), Value::Object(detail));
            fnd.insert("suggested_action".into(), json!("unlink"));
            findings.push(Value::Object(fnd));
        }
    }
    json!(findings)
}

/// JS: `async fix(finding)` — no `db` param either; `_db` kept for the uniform call shape.
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

    match std::fs::remove_file(path) {
        Ok(_) => json!({ "applied": true, "changes": { "files_removed": [path] } }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // JS unlinkSync (no `force`) throws ENOENT on a missing path — caught and
            // reported as a real failure, unlike the force-remove reconciler siblings.
            json!({ "applied": false, "reason": "already gone" })
        }
        Err(e) => json!({ "applied": false, "reason": e.to_string() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

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
        dir.push(format!("antiloki-stale-pid-files-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    // A pid the OS will not reuse within this test's lifetime: spawn a trivial child and
    // wait for it to exit before reading back its pid.
    fn dead_pid() -> u32 {
        let mut child = if cfg!(windows) {
            Command::new("cmd").args(["/C", "exit", "0"]).spawn().unwrap()
        } else {
            Command::new("true").spawn().unwrap()
        };
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[test]
    fn is_pid_alive_true_for_the_current_process() {
        assert!(is_pid_alive(std::process::id() as f64));
    }

    #[test]
    fn is_pid_alive_false_for_an_exited_process() {
        assert!(!is_pid_alive(dead_pid() as f64));
    }

    #[test]
    fn is_pid_alive_false_for_zero_and_negative() {
        // pid<=0 must never reach the raw kill(2)/OpenProcess syscall (group-signal footgun).
        assert!(!is_pid_alive(0.0));
        assert!(!is_pid_alive(-5.0));
    }

    #[test]
    fn detect_no_run_dir_yields_empty_array() {
        let dir = temp_dir("noroot");
        let r = with_home(dir.to_str().unwrap(), || detect(&db0()));
        assert_eq!(r, json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_flags_empty_pid_file_with_null_pid() {
        let dir = temp_dir("empty");
        let run = dir.join("run");
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("empty.pid"), "   \n").unwrap();

        let r = with_home(dir.to_str().unwrap(), || detect(&db0()));
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["kind"], json!("stale-pid-file"));
        assert_eq!(arr[0]["severity"], json!("cleanup"));
        assert_eq!(arr[0]["message"], json!("pid file empty.pid is empty"));
        assert_eq!(arr[0]["detail"]["pid"], Value::Null);
        assert_eq!(arr[0]["suggested_action"], json!("unlink"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_flags_non_numeric_content_as_dead_pid_nan() {
        let dir = temp_dir("nan");
        let run = dir.join("run");
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("garbage.pid"), "not-a-number").unwrap();

        let r = with_home(dir.to_str().unwrap(), || detect(&db0()));
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["message"], json!("pid file garbage.pid references dead pid NaN"));
        assert_eq!(arr[0]["detail"]["pid"], Value::Null);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_flags_zero_pid_ignores_live_pid_and_non_pid_files() {
        let dir = temp_dir("mixed");
        let run = dir.join("run");
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("zero.pid"), "0").unwrap();
        fs::write(run.join("alive.pid"), std::process::id().to_string()).unwrap();
        fs::write(run.join("notes.txt"), "not a pid file").unwrap();

        let r = with_home(dir.to_str().unwrap(), || detect(&db0()));
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1, "only zero.pid should be flagged");
        assert_eq!(arr[0]["detail"]["pid"], json!(0));
        assert_eq!(arr[0]["message"], json!("pid file zero.pid references dead pid 0"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_flags_dead_pid_with_its_numeric_value() {
        let dir = temp_dir("dead");
        let run = dir.join("run");
        fs::create_dir_all(&run).unwrap();
        let pid = dead_pid();
        fs::write(run.join("dead.pid"), pid.to_string()).unwrap();

        let r = with_home(dir.to_str().unwrap(), || detect(&db0()));
        let arr = r.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["detail"]["pid"], json!(pid as i64));
        assert_eq!(
            arr[0]["message"],
            json!(format!("pid file dead.pid references dead pid {pid}"))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fix_missing_path_is_a_noop_failure() {
        let r = fix(&json!({}), &db0());
        assert_eq!(r, json!({ "applied": false, "reason": "missing path" }));
        let r2 = fix(&json!({ "detail": {} }), &db0());
        assert_eq!(r2, json!({ "applied": false, "reason": "missing path" }));
    }

    #[test]
    fn fix_unlinks_an_existing_file() {
        let dir = temp_dir("fix-existing");
        let target = dir.join("some.pid");
        fs::write(&target, "1").unwrap();
        let path_str = target.to_string_lossy().into_owned();

        let r = fix(&json!({ "detail": { "path": path_str } }), &db0());
        assert_eq!(r["applied"], json!(true));
        assert_eq!(r["changes"]["files_removed"], json!([path_str]));
        assert!(!target.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fix_on_missing_file_is_a_real_failure_already_gone() {
        // Unlike the force-remove reconciler siblings (orphan-sandbox-dirs etc), a missing
        // path here is NOT a successful no-op — JS unlinkSync throws ENOENT.
        let dir = temp_dir("fix-missing");
        let ghost = dir.join("never-existed.pid");
        let path_str = ghost.to_string_lossy().into_owned();

        let r = fix(&json!({ "detail": { "path": path_str } }), &db0());
        assert_eq!(r, json!({ "applied": false, "reason": "already gone" }));
        let _ = fs::remove_dir_all(&dir);
    }
}
