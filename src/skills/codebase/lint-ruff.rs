//! codebase/lint-ruff.rs — twin of skills/codebase/lint-ruff.mjs (default export, db-first
//! sig `async function lintRuff(input, context)`). NAMES ARE CONTRACT: `lintRuff`.
//!
//! BESPOKE-NATIVE (no conform-db fixture/frontier entry), same precedent as `lint-eslint.rs`'s
//! `lintEslint` and `_helpers/run-cmd.rs`'s `runCmd`: the real work is spawning a REAL `ruff`
//! subprocess (a standalone Rust binary picked up off the user's PATH, not an npm-installed
//! tool — unlike eslint/tsc there is no `resolveBin`/`node_modules` probe here at all) over the
//! REAL workspace filesystem and parsing its `--output-format json` stdout — not a value the
//! conform-db two-lane harness can seed/replay.
//!   - `installed:false` (ruff missing from PATH) is the JS `r.error || r.code === -1` branch —
//!     ruff simply fails to spawn, mirrored here by `run_cmd` returning `error:true`/`code:-1`.
//!   - JSON parse failure of stdout (non-JSON output) is swallowed exactly like the JS `catch {}`
//!     → empty findings, `ok:true`.
//!   - `getWorkspaceRoot`/`runCmd` are each already-converted twins, called here exactly as the
//!     JS does (JS DI via `context.run_cmd` has no Rust analogue — this twin always uses the real
//!     functions, same disclosed scope as `lint-eslint.rs`).
//!
//! `relative(root, d.filename)` + `.replace(/\\/g,'/')` (the `norm` helper) is ported via the
//! same win32 `relative()` algorithm duplicated in `lint-eslint.rs`/`code-coverage.rs` (verified
//! byte-for-byte vs real Node output there); duplicated here privately for the same reason (no
//! `pub` cross-module helper to import).
// QUALITY: 76/100 — real subprocess + real fs (bespoke-native, no fixture), win32 relative() port duplicated per-file (disclosed).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

fn is_win_sep(c: u8) -> bool { c == b'/' || c == b'\\' }

fn normalize_string_win(path: &[u8], allow_above_root: bool) -> String {
    let mut res: Vec<u8> = Vec::new();
    let mut last_segment_length: isize = 0;
    let mut last_slash: isize = -1;
    let mut dots: isize = 0;
    let mut ch: u8 = 0;
    let n = path.len() as isize;
    let mut i: isize = 0;
    while i <= n {
        if i < n {
            ch = path[i as usize];
        } else if is_win_sep(ch) {
            break;
        } else {
            ch = b'\\';
        }
        if is_win_sep(ch) {
            if last_slash == i - 1 || dots == 1 {
                // NOOP
            } else if dots == 2 {
                if res.len() < 2 || last_segment_length != 2
                    || res[res.len() - 1] != b'.' || res[res.len() - 2] != b'.'
                {
                    if res.len() > 2 {
                        match res.iter().rposition(|&c| is_win_sep(c)) {
                            Some(lsi) => {
                                res.truncate(lsi);
                                let new_last = res.iter().rposition(|&c| is_win_sep(c)).map(|x| x as isize).unwrap_or(-1);
                                last_segment_length = res.len() as isize - 1 - new_last;
                            }
                            None => { res.clear(); last_segment_length = 0; }
                        }
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    } else if !res.is_empty() {
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i;
                        dots = 0;
                        i += 1;
                        continue;
                    }
                }
                if allow_above_root {
                    if !res.is_empty() { res.push(b'\\'); res.push(b'.'); res.push(b'.'); }
                    else { res.push(b'.'); res.push(b'.'); }
                    last_segment_length = 2;
                }
            } else {
                let seg = &path[(last_slash + 1) as usize..i as usize];
                if !res.is_empty() { res.push(b'\\'); res.extend_from_slice(seg); }
                else { res.extend_from_slice(seg); }
                last_segment_length = i - last_slash - 1;
            }
            last_slash = i;
            dots = 0;
        } else if ch == b'.' && dots != -1 {
            dots += 1;
        } else {
            dots = -1;
        }
        i += 1;
    }
    String::from_utf8(res).unwrap_or_default()
}

fn win_normalize(path: &str) -> String {
    let bytes = path.as_bytes();
    let len = bytes.len();
    if len == 0 { return ".".into(); }
    if len == 1 {
        return if is_win_sep(bytes[0]) { "\\".into() } else { path.to_string() };
    }
    let mut root_end: usize = 0;
    let mut device: Option<String> = None;
    let mut is_absolute = false;
    if is_win_sep(bytes[0]) {
        is_absolute = true;
        root_end = 1;
    } else if bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        device = Some(String::from_utf8(bytes[0..2].to_vec()).unwrap_or_default());
        root_end = 2;
        if len > 2 && is_win_sep(bytes[2]) {
            is_absolute = true;
            root_end = 3;
        }
    }
    let mut tail = if root_end < len { normalize_string_win(&bytes[root_end..], !is_absolute) } else { String::new() };
    if tail.is_empty() && !is_absolute { tail = ".".into(); }
    if !tail.is_empty() && is_win_sep(bytes[len - 1]) { tail.push('\\'); }
    match device {
        None => if is_absolute { if !tail.is_empty() { format!("\\{tail}") } else { "\\".into() } } else { tail },
        Some(d) => if is_absolute { if !tail.is_empty() { format!("{d}\\{tail}") } else { format!("{d}\\") } } else { format!("{d}{tail}") },
    }
}

/// node:path.win32.relative(from, to) — see module doc (duplicated from lint-eslint.rs).
fn win_relative(from_raw: &str, to_raw: &str) -> String {
    if from_raw == to_raw { return String::new(); }
    let from_orig = win_normalize(from_raw);
    let to_orig = win_normalize(to_raw);
    if from_orig == to_orig { return String::new(); }
    let from_lc = from_orig.to_ascii_lowercase();
    let to_lc = to_orig.to_ascii_lowercase();
    if from_lc == to_lc { return String::new(); }
    let from_b = from_lc.as_bytes();
    let to_b = to_lc.as_bytes();

    let mut from_start = 0usize;
    while from_start < from_b.len() && from_b[from_start] == b'\\' { from_start += 1; }
    let mut from_end = from_b.len();
    while from_end > from_start + 1 && from_b[from_end - 1] == b'\\' { from_end -= 1; }
    let from_len = from_end - from_start;

    let mut to_start = 0usize;
    while to_start < to_b.len() && to_b[to_start] == b'\\' { to_start += 1; }
    let mut to_end = to_b.len();
    while to_end > to_start + 1 && to_b[to_end - 1] == b'\\' { to_end -= 1; }
    let to_len = to_end - to_start;

    let length = from_len.min(to_len);
    let mut last_common_sep: i64 = -1;
    let mut i: usize = 0;
    loop {
        if i == length {
            if to_len > length {
                if to_b[to_start + i] == b'\\' {
                    return to_orig[to_start + i + 1..].to_string();
                } else if i == 2 {
                    return to_orig[to_start + i..].to_string();
                }
            }
            if from_len > length {
                if from_b[from_start + i] == b'\\' {
                    last_common_sep = i as i64;
                } else if i == 2 {
                    last_common_sep = 3;
                }
            }
            break;
        }
        let fc = from_b[from_start + i];
        let tc = to_b[to_start + i];
        if fc != tc { break; }
        if fc == b'\\' { last_common_sep = i as i64; }
        i += 1;
    }

    if i != length && last_common_sep == -1 {
        return to_orig;
    }

    let lcs = if last_common_sep < 0 { 0 } else { last_common_sep as usize };
    let mut out = String::new();
    let mut j = from_start + lcs + 1;
    while j <= from_end {
        if j == from_end || from_b[j] == b'\\' {
            if out.is_empty() { out.push_str(".."); } else { out.push_str("\\.."); }
        }
        j += 1;
    }

    let mut to_start2 = to_start + lcs;
    if !out.is_empty() {
        format!("{out}{}", &to_orig[to_start2..to_end])
    } else {
        if to_start2 < to_b.len() && to_b[to_start2] == b'\\' { to_start2 += 1; }
        to_orig[to_start2..to_end].to_string()
    }
}

// = JS `const norm = (p) => String(p ?? '').replace(/\\/g, '/')`
fn norm(p: &str) -> String { p.replace('\\', "/") }

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

// = JS `r.error || r.code === -1` (the "ruff not on PATH" detector). Factored out so it can be
// unit-tested directly against synthetic run_cmd results — on Windows, `runCmd` always shells out
// via `cmd /C` (see module doc / lintRuff below), so a *real* missing binary yields cmd's own
// "not recognized" exit code 1, not -1/error — this branch is only reachable there via a
// non-shell spawn failure (posix), hence the direct unit test below instead of an OS-level one.
fn ruff_missing(r: &Value) -> bool {
    matches!(r.get("error"), Some(Value::Bool(true))) || r.get("code").and_then(Value::as_i64) == Some(-1)
}

fn empty_result(installed: bool) -> Value {
    let mut o = Map::new();
    o.insert("ok".into(), json!(true));
    o.insert("installed".into(), json!(installed));
    o.insert("checked".into(), json!([]));
    o.insert("findings".into(), json!([]));
    Value::Object(o)
}

/// `export default async function lintRuff(input, context)` — see module doc.
pub fn lintRuff(input: &Value, db: &Connection) -> Result<Value, String> {
    let all = input.get("all").map(is_truthy).unwrap_or(false);
    let paths: Vec<String> = input
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|f| f.get("path").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string))
        .collect();
    let targets: Vec<String> = if all { vec![".".to_string()] } else { paths };
    if targets.is_empty() {
        let mut o = Map::new();
        o.insert("ok".into(), json!(true));
        o.insert("installed".into(), json!(true));
        o.insert("checked".into(), json!([]));
        o.insert("findings".into(), json!([]));
        return Ok(Value::Object(o));
    }

    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default().to_string();

    let mut args: Vec<Value> = vec![json!("check"), json!("--output-format"), json!("json")];
    args.extend(targets.iter().map(|t| json!(t)));
    let opts = json!({ "cwd": root, "shell": cfg!(windows), "timeoutMs": 240000 });
    let r = crate::codebase__helpers_run_cmd::runCmd(&json!("ruff"), &Value::Array(args), &opts);

    if ruff_missing(&r) {
        let mut o = empty_result(false);
        if let Value::Object(m) = &mut o {
            m.insert("skipped".into(), json!("ruff not on PATH"));
        }
        return Ok(o);
    }

    let stdout = r.get("stdout").and_then(Value::as_str).unwrap_or_default();
    let stdout_for_parse = if stdout.is_empty() { "[]" } else { stdout };
    let mut findings: Vec<Value> = Vec::new();
    let mut checked: BTreeSet<String> = BTreeSet::new();
    let mut checked_order: Vec<String> = Vec::new();
    if let Ok(diags) = serde_json::from_str::<Value>(stdout_for_parse) {
        if let Some(arr) = diags.as_array() {
            for d in arr {
                let filename = d.get("filename").and_then(Value::as_str).unwrap_or_default();
                let rel = norm(&win_relative(&root, filename));
                if checked.insert(rel.clone()) {
                    checked_order.push(rel.clone());
                }
                let line = d.get("location").and_then(|l| l.get("row")).and_then(Value::as_i64).unwrap_or(0);
                let col = d.get("location").and_then(|l| l.get("column")).and_then(Value::as_i64).unwrap_or(0);
                let code = d.get("code").cloned().filter(|v| !v.is_null()).unwrap_or(json!("ruff"));
                let mut fo = Map::new();
                fo.insert("file".into(), json!(rel));
                fo.insert("line".into(), json!(line));
                fo.insert("col".into(), json!(col));
                fo.insert("code".into(), code);
                if let Some(msg) = d.get("message") {
                    fo.insert("message".into(), msg.clone());
                }
                fo.insert("severity".into(), json!("error"));
                findings.push(Value::Object(fo));
            }
        }
    }

    let mut o = Map::new();
    o.insert("ok".into(), json!(findings.is_empty()));
    o.insert("installed".into(), json!(true));
    o.insert("checked".into(), json!(checked_order));
    o.insert("findings".into(), json!(findings));
    Ok(Value::Object(o))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        ).unwrap();
        c
    }
    fn seed_active(c: &Connection, root_path: &str) {
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active, created_at) VALUES ('self', ?1, 'git', 1, '2026-01-01 00:00:00')",
            rusqlite::params![root_path],
        ).unwrap();
    }

    #[test]
    fn win_relative_and_norm_match_real_node_output() {
        assert_eq!(norm(&win_relative("C:/proj", "C:\\proj\\src\\app.py")), "src/app.py");
        assert_eq!(win_relative("C:/proj", "C:/proj"), "");
    }

    #[test]
    fn no_targets_returns_early_without_spawning_ruff() {
        let c = db();
        seed_active(&c, std::env::temp_dir().to_string_lossy().as_ref());
        let r = lintRuff(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["installed"], json!(true));
        assert_eq!(r["checked"], json!([]));
        assert_eq!(r["findings"], json!([]));
        assert!(r.get("skipped").is_none());
    }

    #[test]
    fn ruff_missing_detector_matches_js_error_or_code_neg1() {
        assert!(ruff_missing(&json!({ "error": true, "code": -1 })));
        assert!(ruff_missing(&json!({ "code": -1 })));
        assert!(ruff_missing(&json!({ "error": true, "code": 0 })));
        assert!(!ruff_missing(&json!({ "code": 1, "stderr": "not recognized" })));
        assert!(!ruff_missing(&json!({ "code": 0, "stdout": "[]" })));
    }

    #[test]
    fn missing_binary_with_real_process_spawn_reports_installed_true_no_findings() {
        // Real end-to-end run against a PATH with no `ruff` binary reachable. On this platform
        // `runCmd` shells out via `cmd /C` (module doc), so the real observable JS behavior is
        // NOT `installed:false` — cmd itself always spawns fine and returns its own "not
        // recognized" exit code (1), which `ruff_missing` correctly does NOT treat as missing
        // (verified live against real skills/codebase/_helpers/run-cmd.mjs during authoring:
        // `runCmd('ruff', ['check'], {shell:true})` against an emptied PATH returns
        // `{code:1, stderr:"'ruff' is not recognized..."}`, never `error`/`code:-1`). Empty
        // stdout then parses as `[]` → zero findings, `ok:true`.
        // PATH is process-global, and emptying it makes EVERY binary unreachable for as long as
        // this test holds it — including the `git` that the code-regenerate/terminal-sandbox tests
        // spawn. Without this lock those tests failed intermittently in full-suite runs (the twin
        // saw `git` vanish and reported a sandbox error). ENV_LOCK is the same lock every
        // ANTILOKI_HOME test takes, so holding it here serialises this window against all of them.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let c = db();
        seed_active(&c, std::env::temp_dir().to_string_lossy().as_ref());
        let empty_dir = std::env::temp_dir().join(format!("antiloki-lint-ruff-emptypath-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&empty_dir);
        let old_path = std::env::var("PATH").unwrap_or_default();
        unsafe { std::env::set_var("PATH", &empty_dir); }
        let r = lintRuff(&json!({ "all": true }), &c);
        unsafe { std::env::set_var("PATH", old_path); }
        let _ = std::fs::remove_dir_all(&empty_dir);
        let r = r.unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["checked"], json!([]));
        assert_eq!(r["findings"], json!([]));
        if cfg!(windows) {
            assert_eq!(r["installed"], json!(true));
            assert!(r.get("skipped").is_none());
        } else {
            // POSIX spawns `ruff` directly (no `cmd /C` in between), so a missing binary is a spawn
            // failure -> `error:true`/`code:-1` -> the JS `installed:false` branch.
            assert_eq!(r["installed"], json!(false));
            assert_eq!(r["skipped"], json!("ruff not on PATH"));
        }
    }
}
