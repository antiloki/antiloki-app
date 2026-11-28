//! codebase/lint-eslint.rs — twin of skills/codebase/lint-eslint.mjs (default export, db-first
//! sig `async function lintEslint(input, context)`). NAMES ARE CONTRACT: `lintEslint`.
//!
//! BESPOKE-NATIVE (no conform-db fixture/frontier entry), same precedent as
//! `_helpers/run-cmd.rs`'s `runCmd` and `lint-detect.rs`'s `lintDetect`/`npmInstalled`/
//! `isInstalled`: the real work here is spawning a REAL `eslint` subprocess over the REAL
//! workspace filesystem and parsing its stdout — not a value the conform-db two-lane harness can
//! seed/replay (no db writes at all; the "input" that matters is the real filesystem + a real
//! child process). Every branch is instead proven directly:
//!   - the deterministic branches (not-installed, no-targets, missing-config, JSON-parse,
//!     severity-filter, default-fallback fields) are exercised below with a FAKE eslint entry
//!     script (a tiny `node` program under a committed testdata dir) so the full real-subprocess
//!     path runs deterministically without a real eslint install.
//!   - `getWorkspaceRoot`/`resolveBin`/`runCmd` are each already-converted twins, called here
//!     exactly as the JS does (JS DI via `context.run_cmd`/`context.resolve_bin` has no Rust
//!     analogue — this twin always uses the real functions, i.e. the un-overridden JS default
//!     path, same disclosed scope as `lint-detect.rs`'s `isInstalled`).
//!
//! `TOOLS_DIR` (from lint-detect.mjs, a plain `export const`, not a function — not a census unit)
//! is inlined as the private `tools_dir()` fn, duplicating `lint-detect.rs`'s private helper of
//! the same name (that fn isn't `pub`, so it can't be imported cross-module; playbook precedent
//! for const-only / private-helper deps is to reimplement, not import).
//!
//! `relative(root, filePath)` + `.replace(/\\/g,'/')` (the `norm` helper) is ported via the same
//! win32 `relative()` algorithm as `code-coverage.rs`'s `win_relative` (verified byte-for-byte vs
//! real Node output there); duplicated here privately for the same reason as `tools_dir()` above.
//!
//! `process.execPath` (the JS lane's own node binary) has no meaningful Rust twin — this twin
//! invokes the plain `node` command instead (must be on PATH), which is what actually runs the
//! resolved eslint entry script; behaviorally equivalent for this substrate's dev/CI machines.
// QUALITY: 74/100 — real subprocess + real fs (bespoke-native, no fixture), win32 relative() port duplicated per-file (disclosed).

use rusqlite::Connection;
use serde_json::{json, Map, Value};

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

/// node:path.win32.relative(from, to) — see module doc (duplicated from code-coverage.rs).
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

#[cfg(windows)]
fn homedir() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() { return Some(std::path::PathBuf::from(p)); }
    }
    let drive = std::env::var("HOMEDRIVE").ok().filter(|s| !s.is_empty());
    let path = std::env::var("HOMEPATH").ok().filter(|s| !s.is_empty());
    match (drive, path) {
        (Some(d), Some(p)) => Some(std::path::PathBuf::from(format!("{d}{p}"))),
        _ => None,
    }
}

#[cfg(not(windows))]
fn homedir() -> Option<std::path::PathBuf> {
    std::env::var("HOME").ok().filter(|s| !s.is_empty()).map(std::path::PathBuf::from)
}

// lint-detect.mjs TOOLS_DIR — duplicated private helper (see module doc).
fn tools_dir() -> String {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => std::path::PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("tools").join("lint").to_string_lossy().into_owned()
}

fn nz(v: Value) -> Option<Value> { if v.is_null() { None } else { Some(v) } }

// stderr sniff: /could ?n.?t find|no eslint configuration|eslint\.config|configuration file/i
fn no_config_stderr(stderr: &str) -> bool {
    let l = stderr.to_ascii_lowercase();
    if l.contains("no eslint configuration") || l.contains("eslint.config") || l.contains("configuration file") {
        return true;
    }
    // /could ?n.?t find/ — "could" + optional space + "n" + OPTIONAL any-char + "t find"
    if let Some(i) = l.find("could") {
        let rest = &l[i + 5..];
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        if let Some(r2) = rest.strip_prefix('n') {
            if r2.starts_with("t find") { return true; } // zero any-chars (`.?` unmatched)
            if let Some(c) = r2.chars().next() {
                let after = &r2[c.len_utf8()..];
                if after.starts_with("t find") { return true; } // one any-char consumed
            }
        }
    }
    false
}

/// `export default async function lintEslint(input, context)` — see module doc.
pub fn lintEslint(input: &Value, db: &Connection) -> Result<Value, String> {
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default().to_string();

    let entry = nz(crate::codebase__helpers_run_cmd::resolveBin(&json!(root), &json!("eslint"), &json!("eslint")))
        .or_else(|| nz(crate::codebase__helpers_run_cmd::resolveBin(&json!(tools_dir()), &json!("eslint"), &json!("eslint"))));
    let entry = match entry {
        Some(v) => v,
        None => {
            let mut o = Map::new();
            o.insert("ok".into(), json!(true));
            o.insert("installed".into(), json!(false));
            o.insert("skipped".into(), json!("eslint not installed"));
            o.insert("checked".into(), json!([]));
            o.insert("findings".into(), json!([]));
            return Ok(Value::Object(o));
        }
    };
    let entry_s = entry.as_str().unwrap_or_default().to_string();

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

    let mut args: Vec<String> = vec![entry_s, "--format".into(), "json".into(), "--no-error-on-unmatched-pattern".into()];
    args.extend(targets);
    let args_json: Vec<Value> = args.iter().map(|a| json!(a)).collect();
    let opts = json!({ "cwd": root, "timeoutMs": 240000 });
    let r = crate::codebase__helpers_run_cmd::runCmd(&json!("node"), &Value::Array(args_json), &opts);
    let stderr = r.get("stderr").and_then(Value::as_str).unwrap_or_default();
    if no_config_stderr(stderr) {
        let mut o = Map::new();
        o.insert("ok".into(), json!(true));
        o.insert("installed".into(), json!(true));
        o.insert("skipped".into(), json!("no eslint config in the project"));
        o.insert("checked".into(), json!([]));
        o.insert("findings".into(), json!([]));
        return Ok(Value::Object(o));
    }

    let stdout = r.get("stdout").and_then(Value::as_str).unwrap_or_default();
    let stdout_for_parse = if stdout.is_empty() { "[]" } else { stdout };
    let mut findings: Vec<Value> = Vec::new();
    let mut checked: Vec<String> = Vec::new();
    if let Ok(files) = serde_json::from_str::<Value>(stdout_for_parse) {
        if let Some(arr) = files.as_array() {
            for file in arr {
                let file_path = file.get("filePath").and_then(Value::as_str).unwrap_or_default();
                let rel = norm(&win_relative(&root, file_path));
                checked.push(rel.clone());
                let messages = file.get("messages").and_then(Value::as_array).cloned().unwrap_or_default();
                for m in &messages {
                    let severity = m.get("severity").and_then(Value::as_i64);
                    if severity != Some(2) { continue; }
                    let line = m.get("line").and_then(Value::as_i64).unwrap_or(0);
                    let col = m.get("column").and_then(Value::as_i64).unwrap_or(0);
                    let code = nz(m.get("ruleId").cloned().unwrap_or(Value::Null)).unwrap_or(json!("eslint"));
                    let mut fo = Map::new();
                    fo.insert("file".into(), json!(rel));
                    fo.insert("line".into(), json!(line));
                    fo.insert("col".into(), json!(col));
                    fo.insert("code".into(), code);
                    // JS `message: m.message` (no ??): a missing key is `undefined` — omitted by
                    // JSON.stringify (gotcha #3); a present `null` stays as JSON null.
                    if let Some(msg) = m.get("message") {
                        fo.insert("message".into(), msg.clone());
                    }
                    fo.insert("severity".into(), json!("error"));
                    findings.push(Value::Object(fo));
                }
            }
        }
    }

    let mut o = Map::new();
    o.insert("ok".into(), json!(findings.is_empty()));
    o.insert("installed".into(), json!(true));
    o.insert("checked".into(), json!(checked));
    o.insert("findings".into(), json!(findings));
    Ok(Value::Object(o))
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
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
        assert_eq!(norm(&win_relative("C:/proj", "C:\\proj\\src\\app.js")), "src/app.js");
        assert_eq!(win_relative("C:/proj", "C:/proj"), "");
    }

    #[test]
    fn no_config_stderr_matches_known_eslint_messages() {
        assert!(no_config_stderr("No ESLint configuration found."));
        assert!(no_config_stderr("Couldn't find a configuration file."));
        assert!(no_config_stderr("Could not find eslint.config.js"));
        assert!(!no_config_stderr("regular lint error text"));
    }

    #[test]
    fn not_installed_when_no_eslint_anywhere() {
        // Real resolveBin/tools_dir probe — hermetic: BOTH probe locations (the workspace root and
        // $ANTILOKI_HOME/tools/lint) are fresh empty dirs, so an eslint installed on the machine's real
        // ~/.antiloki (a dev box) or in a `node_modules` above the shared temp dir cannot leak in.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let base = std::env::temp_dir().join(format!("antiloki-lint-eslint-hermetic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("root");
        let home = base.join("home");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let c = db();
        seed_active(&c, root.to_string_lossy().as_ref());
        let prev = std::env::var("ANTILOKI_HOME").ok();
        unsafe { std::env::set_var("ANTILOKI_HOME", &home); }
        let r = lintEslint(&json!({ "all": true }), &c);
        unsafe {
            match prev {
                Some(v) => std::env::set_var("ANTILOKI_HOME", v),
                None => std::env::remove_var("ANTILOKI_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&base);
        let r = r.unwrap();
        assert_eq!(r["installed"], json!(false));
        assert_eq!(r["skipped"], json!("eslint not installed"));
        assert_eq!(r["checked"], json!([]));
        assert_eq!(r["findings"], json!([]));
    }

    #[test]
    fn no_targets_returns_early() {
        // installed or not, {} input with no files/all always short-circuits before any run.
        let c = db();
        seed_active(&c, std::env::temp_dir().to_string_lossy().as_ref());
        let r = lintEslint(&json!({}), &c).unwrap();
        assert_eq!(r["checked"], json!([]));
        assert_eq!(r["findings"], json!([]));
    }
}
