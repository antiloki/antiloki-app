//! codebase/typecheck-py.rs — twin of skills/codebase/typecheck-py.mjs (default export, db-first
//! sig `async function typecheckPy(input, context)`). NAMES ARE CONTRACT: `typecheckPy`,
//! `parsePyright`.
//!
//! BESPOKE-NATIVE (no conform-db fixture/frontier entry), same precedent as `lint-eslint.rs`'s
//! `lintEslint` and `_helpers/run-cmd.rs`'s `runCmd`: the real work is spawning a REAL `pyright`
//! subprocess (via a resolved npm-installed bin entry) over REAL files (either written to a temp
//! dir from inline `source`, or resolved against the real workspace filesystem) and parsing its
//! stdout — not a value the conform-db two-lane harness can seed/replay. Every deterministic
//! branch (not-installed, JSON-parse of pyright's `--outputjson` output, the resolution-rule
//! filter, the per-diagnostic field mapping) is instead proven directly below with a FAKE pyright
//! entry script (a tiny `node` program under a committed testdata dir) so the full real-subprocess
//! path runs deterministically without a real pyright install.
//!
//! `TOOLS_DIR` (from typecheck-detect.mjs, a plain `export const`, not a function — not a census
//! unit) is inlined as the private `tools_dir()` fn, duplicating `typecheck-detect.rs`'s private
//! helper of the same name (that fn isn't `pub`, so it can't be imported cross-module; playbook
//! precedent for const-only / private-helper deps is to reimplement, not import).
//!
//! `process.execPath` (the JS lane's own node binary) has no meaningful Rust twin — this twin
//! invokes the plain `node` command instead (must be on PATH), same disclosed-scope precedent as
//! `lint-eslint.rs`.
//!
//! `resolveWorkspacePath`/`resolveBin`/`runCmd` are each already-converted twins, called here
//! exactly as the JS does (JS DI via `context.run_cmd`/`context.resolve_bin` has no Rust analogue
//! — this twin always uses the real functions, i.e. the un-overridden JS default path). Per-file
//! errors from `resolveWorkspacePath` (path escapes / traversal) and from a failed `source` write
//! are caught-and-skipped exactly like the JS `try { ... } catch { continue; }` / `catch { continue; }`.
//!
//! `mkdtempSync` has no exact Rust stdlib equivalent — this twin creates a uniquely-named temp
//! dir (`std::env::temp_dir()` + a pid+nanos-timestamp suffix, same uniqueness strategy as the
//! sibling twins' own `#[cfg(test)]` temp-dir helpers, e.g. `terminal-sandbox.rs`) instead of
//! porting glibc's mkdtemp algorithm — behaviorally equivalent (a fresh, collision-free directory)
//! for this substrate's single-process call sites.
// QUALITY: 76/100 — real subprocess + real fs (bespoke-native, no fixture), TOOLS_DIR duplicated per-file (disclosed).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

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

// typecheck-detect.mjs TOOLS_DIR — duplicated private helper (see module doc).
fn tools_dir() -> String {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("tools").join("typecheck").to_string_lossy().into_owned()
}

fn resolution_rules() -> [&'static str; 3] {
    ["reportMissingImports", "reportMissingModuleSource", "reportAttributeAccessIssue"]
}

// = JS String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(o) => o.to_string(),
    }
}

fn nz(v: Option<Value>) -> Option<Value> {
    v.filter(|x| !x.is_null())
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let mut dir = std::env::temp_dir();
    dir.push(format!("{prefix}{}-{}", std::process::id(), nanos));
    let _ = fs::create_dir_all(&dir);
    dir
}

/// `export function parsePyright(out)` — parses `pyright --outputjson` stdout into findings.
pub fn parsePyright(out: &Value) -> Value {
    let text = str_qq(Some(out));
    let data: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return json!([]),
    };
    let diags = data.get("generalDiagnostics").and_then(Value::as_array).cloned().unwrap_or_default();
    let out: Vec<Value> = diags
        .iter()
        .filter(|d| d.get("severity").and_then(Value::as_str) == Some("error"))
        .map(|d| {
            let line = d.get("range").and_then(|r| r.get("start")).and_then(|s| s.get("line")).and_then(Value::as_i64).unwrap_or(0) + 1;
            let col = d.get("range").and_then(|r| r.get("start")).and_then(|s| s.get("character")).and_then(Value::as_i64).unwrap_or(0) + 1;
            let rule = nz(d.get("rule").cloned());
            let message = str_qq(d.get("message"));
            let message_first_line = message.split('\n').next().unwrap_or("").to_string();
            let mut o = Map::new();
            o.insert("file".into(), nz(d.get("file").cloned()).unwrap_or(Value::Null));
            o.insert("line".into(), json!(line));
            o.insert("col".into(), json!(col));
            o.insert("rule".into(), rule.clone().unwrap_or(Value::Null));
            o.insert("code".into(), rule.unwrap_or(json!("pyright")));
            o.insert("message".into(), json!(message_first_line));
            Value::Object(o)
        })
        .collect();
    json!(out)
}

/// `export default async function typecheckPy(input, context)` — see module doc.
pub fn typecheckPy(input: &Value, db: &Connection) -> Result<Value, String> {
    let entry = nz(Some(crate::codebase__helpers_run_cmd::resolveBin(&json!(tools_dir()), &json!("pyright"), &json!("pyright"))));
    let entry_s = match entry.and_then(|v| v.as_str().map(str::to_string)) {
        Some(s) => s,
        None => {
            let mut o = Map::new();
            o.insert("ok".into(), json!(true));
            o.insert("installed".into(), json!(false));
            o.insert("skipped".into(), json!("pyright not installed"));
            o.insert("checked".into(), json!([]));
            o.insert("findings".into(), json!([]));
            return Ok(Value::Object(o));
        }
    };

    let files = input.get("files").and_then(Value::as_array).cloned().unwrap_or_default();
    let tmp = unique_temp_dir("antiloki-tcpy-");
    let rules = resolution_rules();

    let mut findings: Vec<Value> = Vec::new();
    let mut checked: Vec<Value> = Vec::new();

    for f in &files {
        let f_path = f.get("path").cloned().unwrap_or(Value::Null);
        let source = f.get("source").filter(|s| !s.is_null());

        let target: String = if let Some(src) = source {
            let name = format!("f{}.py", checked.len());
            let target_path = tmp.join(&name);
            match fs::write(&target_path, str_qq(Some(src))) {
                Ok(_) => target_path.to_string_lossy().into_owned(),
                Err(_) => continue, // catch { continue }
            }
        } else {
            match crate::engine_workspace_root::resolveWorkspacePath(db, &f_path) {
                Ok(v) => v.as_str().unwrap_or_default().to_string(),
                Err(_) => continue, // catch { continue }
            }
        };

        let args = json!([entry_s, "--outputjson", target]);
        let opts = json!({ "cwd": tmp.to_string_lossy(), "timeoutMs": 120000 });
        let r = crate::codebase__helpers_run_cmd::runCmd(&json!("node"), &args, &opts);
        let stdout = r.get("stdout").cloned().unwrap_or(json!(""));
        let diags = parsePyright(&stdout);
        if let Some(arr) = diags.as_array() {
            for d in arr {
                let rule = d.get("rule").and_then(Value::as_str);
                if let Some(rule_s) = rule {
                    if rules.contains(&rule_s) {
                        continue;
                    }
                }
                let mut fo = Map::new();
                fo.insert("file".into(), f_path.clone());
                fo.insert("line".into(), d.get("line").cloned().unwrap_or(json!(0)));
                fo.insert("col".into(), d.get("col").cloned().unwrap_or(json!(0)));
                fo.insert("code".into(), nz(d.get("rule").cloned()).unwrap_or(json!("pyright")));
                fo.insert("message".into(), d.get("message").cloned().unwrap_or(Value::Null));
                fo.insert("severity".into(), json!("error"));
                findings.push(Value::Object(fo));
            }
        }
        checked.push(f_path);
    }

    let mut o = Map::new();
    o.insert("ok".into(), json!(findings.is_empty()));
    o.insert("installed".into(), json!(true));
    o.insert("checked".into(), json!(checked));
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
        )
        .unwrap();
        c
    }
    fn seed_active(c: &Connection, root_path: &str) {
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active, created_at) VALUES ('self', ?1, 'git', 1, '2026-01-01 00:00:00')",
            rusqlite::params![root_path],
        )
        .unwrap();
    }

    fn sample_pyright_json() -> &'static str {
        r#"{"generalDiagnostics":[
            {"file":"a.py","severity":"error","rule":"reportGeneralTypeIssues","range":{"start":{"line":2,"character":4}},"message":"bad type\nmore detail"},
            {"file":"a.py","severity":"error","rule":"reportMissingImports","range":{"start":{"line":0,"character":0}},"message":"missing import"},
            {"file":"a.py","severity":"warning","rule":"reportUnusedVariable","range":{"start":{"line":1,"character":1}},"message":"unused"}
        ]}"#
    }

    #[test]
    fn parse_pyright_filters_errors_maps_fields_and_takes_first_message_line() {
        let out = parsePyright(&json!(sample_pyright_json()));
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 2); // warning dropped
        assert_eq!(arr[0]["line"], json!(3));
        assert_eq!(arr[0]["col"], json!(5));
        assert_eq!(arr[0]["code"], json!("reportGeneralTypeIssues"));
        assert_eq!(arr[0]["message"], json!("bad type"));
        assert_eq!(arr[1]["code"], json!("reportMissingImports"));
    }

    #[test]
    fn parse_pyright_returns_empty_on_malformed_json() {
        assert_eq!(parsePyright(&json!("not json")), json!([]));
        assert_eq!(parsePyright(&Value::Null), json!([]));
    }

    #[test]
    fn parse_pyright_defaults_missing_rule_to_pyright_code() {
        let text = r#"{"generalDiagnostics":[{"file":"a.py","severity":"error","range":{"start":{"line":0,"character":0}},"message":"x"}]}"#;
        let arr = parsePyright(&json!(text)).as_array().unwrap().clone();
        assert_eq!(arr[0]["code"], json!("pyright"));
        assert_eq!(arr[0]["rule"], Value::Null);
    }

    // Real resolveBin/tools_dir probe against a fresh HOME with no node_modules/pyright.
    // SAFETY: ANTILOKI_HOME is process-global — serialize via the crate-wide lock (a local mutex
    // would not prevent a race with OTHER files' tests over the same real env var).
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
        dir.push(format!("antiloki-typecheck-py-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn not_installed_when_no_pyright_anywhere() {
        let dir = temp_dir("not-installed");
        let c = db();
        seed_active(&c, dir.to_string_lossy().as_ref());
        let r = with_home(dir.to_str().unwrap(), || typecheckPy(&json!({ "files": [] }), &c).unwrap());
        assert_eq!(r["installed"], json!(false));
        assert_eq!(r["skipped"], json!("pyright not installed"));
        assert_eq!(r["checked"], json!([]));
        assert_eq!(r["findings"], json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn installed_with_no_files_returns_empty_checked() {
        // Fake pyright entry present under HOME/tools/typecheck/node_modules/pyright — never
        // actually invoked since `files` is empty (loop body never runs).
        let dir = temp_dir("no-files");
        let pkg_dir = dir.join("tools").join("typecheck").join("node_modules").join("pyright");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(pkg_dir.join("package.json"), r#"{"bin":{"pyright":"index.js"}}"#).unwrap();
        fs::write(pkg_dir.join("index.js"), "").unwrap();

        let c = db();
        seed_active(&c, dir.to_string_lossy().as_ref());
        let r = with_home(dir.to_str().unwrap(), || typecheckPy(&json!({ "files": [] }), &c).unwrap());
        assert_eq!(r["installed"], json!(true));
        assert_eq!(r["checked"], json!([]));
        assert_eq!(r["findings"], json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_a_file_whose_workspace_path_escapes_the_root() {
        // A file-mode entry (no `source`) with a path that escapes the workspace root hits the
        // `catch { continue }` branch: never counted in `checked`, never spawns pyright for it.
        let dir = temp_dir("escape");
        let pkg_dir = dir.join("tools").join("typecheck").join("node_modules").join("pyright");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(pkg_dir.join("package.json"), r#"{"bin":{"pyright":"index.js"}}"#).unwrap();
        fs::write(pkg_dir.join("index.js"), "").unwrap();

        let c = db();
        seed_active(&c, dir.to_string_lossy().as_ref());
        let r = with_home(dir.to_str().unwrap(), || typecheckPy(&json!({ "files": [{ "path": "../escape.py" }] }), &c).unwrap());
        assert_eq!(r["installed"], json!(true));
        assert_eq!(r["checked"], json!([]));
        assert_eq!(r["findings"], json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn runs_real_pyright_like_fake_entry_over_inline_source_and_filters_resolution_rules() {
        // A tiny real `node` program standing in for pyright: reads argv, always emits one
        // reportMissingImports (filtered out) and one reportGeneralTypeIssues (kept) error.
        let dir = temp_dir("run");
        let pkg_dir = dir.join("tools").join("typecheck").join("node_modules").join("pyright");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(pkg_dir.join("package.json"), r#"{"bin":{"pyright":"fake.js"}}"#).unwrap();
        let fake_js = r#"
console.log(JSON.stringify({ generalDiagnostics: [
  { file: "x.py", severity: "error", rule: "reportMissingImports", range: { start: { line: 0, character: 0 } }, message: "no module" },
  { file: "x.py", severity: "error", rule: "reportGeneralTypeIssues", range: { start: { line: 4, character: 2 } }, message: "type mismatch\nextra" }
]}));
"#;
        fs::write(pkg_dir.join("fake.js"), fake_js).unwrap();

        let c = db();
        seed_active(&c, dir.to_string_lossy().as_ref());
        let r = with_home(dir.to_str().unwrap(), || {
            typecheckPy(&json!({ "files": [{ "path": "inline.py", "source": "x = 1\n" }] }), &c).unwrap()
        });
        assert_eq!(r["installed"], json!(true));
        assert_eq!(r["checked"], json!(["inline.py"]));
        let findings = r["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["file"], json!("inline.py"));
        assert_eq!(findings[0]["code"], json!("reportGeneralTypeIssues"));
        assert_eq!(findings[0]["line"], json!(5));
        assert_eq!(findings[0]["col"], json!(3));
        assert_eq!(findings[0]["message"], json!("type mismatch"));
        assert_eq!(findings[0]["severity"], json!("error"));
        assert_eq!(r["ok"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }
}
