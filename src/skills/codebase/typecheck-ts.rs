//! typecheck-ts.rs — twin of skills/codebase/typecheck-ts.mjs. NAMES ARE CONTRACT: `typecheckTs`
//! (default), `parseTsc` (named export).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: `typecheckTs` shells out to a REAL `tsc`
//! subprocess (`node <tsc-entry> --noEmit ...`) against files written into a REAL temp directory
//! (`mkdtempSync`) — subprocess + real fs, same class as lint-eslint.rs / lint-ruff.rs /
//! lint-install.rs (all reuse the same `_helpers/run-cmd.rs` twin). Proven below via `#[cfg(test)]`
//! against a real `node`/`tsc` install when available (skipped gracefully otherwise) plus a pure
//! unit test of `parseTsc` (no subprocess needed).
//!
//! Cross-module calls (already-converted twins, reused not reimplemented):
//! `crate::engine_workspace_root::resolveWorkspacePath`,
//! `crate::codebase__helpers_run_cmd::{runCmd, resolveBin}`. `TOOLS_DIR` (imported from
//! typecheck-detect.mjs, itself `roots.mjs TOOLS_DIR + '/typecheck'`) is inlined here the same
//! way `codebase/typecheck-detect.rs`'s own `tools_dir()` inlines it — that fn is a private
//! (non-`pub`) helper there (a plain const in the JS, not a census unit), so this twin recomputes
//! the identical path locally rather than reaching into another module's private fn.
//!
//! `mkdtempSync(join(tmpdir(), 'antiloki-tc-'))`: Rust has no libuv-style atomic mkdtemp, so this
//! twin generates a unique suffix (process id + a monotonic nanosecond timestamp) and creates the
//! dir — collision-free in practice for this single-process, single-call-site use, same
//! convention as every sibling twin's `#[cfg(test)]` `temp_dir()` helper (now promoted to
//! production code here since the JS original itself needs a fresh temp dir per real call, not
//! just per test).
// QUALITY: 78/100 — real subprocess dispatch via the already-proven run-cmd twin; own mkdtemp is an approximation of libuv's atomic mkdtemp (disclosed)

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::OnceLock;

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
// roots.mjs TOOLS_DIR = join(HOME_DIR, 'tools'); typecheck-detect.mjs TOOLS_DIR = join(that, 'typecheck').
fn tools_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("tools").join("typecheck")
}

// mkdtempSync(join(tmpdir(), 'antiloki-tc-')) — see module doc.
fn mkdtemp(prefix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("{prefix}{}-{nanos}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

// pure module-resolution diagnostics — noise in single-file mode (no project graph to resolve against).
fn resolution_codes() -> [&'static str; 6] {
    ["2307", "2305", "2306", "2792", "6053", "7016"]
}

// `^(.*?)\((\d+),(\d+)\):\s+error\s+TS(\d+):\s+(.*)$` evaluated per-line (JS `.split(/\r?\n/)`
// already strips the line terminator, so `$`/`.` need no DOTALL/MULTILINE flags here).
fn tsc_line_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(.*?)\((\d+),(\d+)\):\s+error\s+TS(\d+):\s+(.*)$").unwrap())
}

/// `export function parseTsc(out)` — `path/to/file.ts(line,col): error TSxxxx: message` per line.
pub fn parseTsc(out: &Value) -> Value {
    // `String(out ?? '')` — JS `??` only falls back on null/undefined, not other falsy values.
    let s = match out {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let re = tsc_line_re();
    let mut findings: Vec<Value> = Vec::new();
    for line in s.split(['\n']) {
        let line = line.strip_suffix('\r').unwrap_or(line); // `\r?\n` split leaves a trailing \r on CRLF input
        if let Some(caps) = re.captures(line) {
            let file = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let file_v = if file.is_empty() { Value::Null } else { json!(file) };
            let line_n: i64 = caps.get(2).unwrap().as_str().parse().unwrap_or(0);
            let col_n: i64 = caps.get(3).unwrap().as_str().parse().unwrap_or(0);
            let num = caps.get(4).unwrap().as_str().to_string();
            let message = caps.get(5).unwrap().as_str().trim().to_string();
            findings.push(json!({
                "file": file_v,
                "line": line_n,
                "col": col_n,
                "num": num,
                "code": format!("TS{num}"),
                "message": message,
            }));
        }
    }
    json!(findings)
}

/// `export default async function typecheckTs(input, context)`.
pub fn typecheckTs(input: &Value, db: &Connection) -> Result<Value, String> {
    let entry_v = crate::codebase__helpers_run_cmd::resolveBin(&json!(tools_dir().to_string_lossy()), &json!("typescript"), &json!("tsc"));
    let entry = match entry_v.as_str() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            let mut o = Map::new();
            o.insert("ok".into(), json!(true));
            o.insert("installed".into(), json!(false));
            o.insert("skipped".into(), json!("typescript not installed"));
            o.insert("checked".into(), json!([]));
            o.insert("findings".into(), json!([]));
            return Ok(Value::Object(o));
        }
    };

    let empty: Vec<Value> = Vec::new();
    let files = input.get("files").and_then(Value::as_array).unwrap_or(&empty).clone();
    let tmp = mkdtemp("antiloki-tc-");
    let mut findings: Vec<Value> = Vec::new();
    let mut checked: Vec<Value> = Vec::new();

    for f in &files {
        let path_v = f.get("path").cloned().unwrap_or(Value::Null);
        let path_s = path_v.as_str().unwrap_or_default().to_string();
        let tsx = path_s.to_lowercase().ends_with('x');
        let source = f.get("source");

        let target: String = if let Some(src) = source.filter(|v| !v.is_null()) {
            let name = format!("f{}{}", checked.len(), if tsx { ".tsx" } else { ".ts" });
            let target_path = tmp.join(&name);
            let content = match src {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if std::fs::write(&target_path, content).is_err() {
                continue;
            }
            target_path.to_string_lossy().into_owned()
        } else {
            match crate::engine_workspace_root::resolveWorkspacePath(db, &path_v) {
                Ok(v) => v.as_str().unwrap_or_default().to_string(),
                Err(_) => continue,
            }
        };

        // `--moduleResolution bundler` (TS >= 5.0), not `node`: TypeScript 7 (what the tools dir installs by
        // default now) REMOVED `node`/node10 and exits TS5108 without checking anything.
        let mut args: Vec<Value> = vec![
            json!(entry), json!("--noEmit"), json!("--pretty"), json!("false"),
            json!("--skipLibCheck"), json!("--target"), json!("es2020"),
            json!("--module"), json!("esnext"), json!("--moduleResolution"), json!("bundler"),
        ];
        if tsx {
            args.push(json!("--jsx"));
            args.push(json!("react-jsx"));
        }
        args.push(json!(target));

        let opts = json!({ "cwd": tmp.to_string_lossy(), "timeoutMs": 120000 });
        let r = crate::codebase__helpers_run_cmd::runCmd(&json!(node_bin()), &Value::Array(args), &opts);
        let stdout = r.get("stdout").and_then(Value::as_str).unwrap_or_default();
        let stderr = r.get("stderr").and_then(Value::as_str).unwrap_or_default();
        let combined = format!("{stdout}\n{stderr}");
        let parsed = parseTsc(&json!(combined));
        if let Some(arr) = parsed.as_array() {
            for d in arr {
                let num = d.get("num").and_then(Value::as_str).unwrap_or_default();
                if resolution_codes().contains(&num) {
                    continue;
                }
                findings.push(json!({
                    "file": path_v,
                    "line": d.get("line").cloned().unwrap_or(Value::Null),
                    "col": d.get("col").cloned().unwrap_or(Value::Null),
                    "code": d.get("code").cloned().unwrap_or(Value::Null),
                    "message": d.get("message").cloned().unwrap_or(Value::Null),
                    "severity": "error",
                }));
            }
        }
        checked.push(path_v);
    }

    let mut o = Map::new();
    o.insert("ok".into(), json!(findings.is_empty()));
    o.insert("installed".into(), json!(true));
    o.insert("checked".into(), json!(checked));
    o.insert("findings".into(), json!(findings));
    Ok(Value::Object(o))
}

// JS runs `entry` via `node <entry> args` (process.execPath). "node" resolves through PATH the
// same way the JS's own process binary would in the substrate's dev/CI environment.
fn node_bin() -> &'static str {
    "node"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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

    #[test]
    fn parse_tsc_extracts_file_line_col_code_message() {
        let out = "src/app.ts(12,5): error TS2322: Type 'string' is not assignable to type 'number'.\nnot a match line\n";
        let findings = parseTsc(&json!(out));
        let arr = findings.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["file"], json!("src/app.ts"));
        assert_eq!(arr[0]["line"], json!(12));
        assert_eq!(arr[0]["col"], json!(5));
        assert_eq!(arr[0]["num"], json!("2322"));
        assert_eq!(arr[0]["code"], json!("TS2322"));
        assert_eq!(arr[0]["message"], json!("Type 'string' is not assignable to type 'number'."));
    }

    #[test]
    fn parse_tsc_handles_multiple_lines_and_no_matches() {
        let out = "no errors here\nfoo.ts(1,1): error TS1000: first\nbar.ts(2,2): error TS1001: second";
        let findings = parseTsc(&json!(out));
        let arr = findings.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["num"], json!("1000"));
        assert_eq!(arr[1]["num"], json!("1001"));
    }

    #[test]
    fn parse_tsc_empty_and_null_input() {
        assert_eq!(parseTsc(&json!("")).as_array().unwrap().len(), 0);
        assert_eq!(parseTsc(&Value::Null).as_array().unwrap().len(), 0);
    }

    #[test]
    fn typecheck_ts_reports_not_installed_when_tsc_missing() {
        // No typescript package.json under this ANTILOKI_HOME's tools/typecheck dir.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-typecheck-ts-rs-notinstalled-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", dir.to_str().unwrap());
        }
        let c = db();
        let r = typecheckTs(&json!({ "files": [] }), &c).unwrap();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
        }
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["installed"], json!(false));
        assert_eq!(r["skipped"], json!("typescript not installed"));
        assert_eq!(r["checked"], json!([]));
        assert_eq!(r["findings"], json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn typecheck_ts_real_tsc_source_mode_finds_a_type_error_when_available() {
        // Only runs the real subprocess assertion if a real typescript install is resolvable
        // under this machine's ANTILOKI_HOME/tools/typecheck — otherwise it degrades to the
        // same not-installed assertion as the test above (proves the guard path either way).
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let c = db();
        let r = typecheckTs(
            &json!({ "files": [{ "path": "bad.ts", "source": "const x: number = 'nope';" }] }),
            &c,
        )
        .unwrap();
        if r["installed"] == json!(false) {
            assert_eq!(r["skipped"], json!("typescript not installed"));
        } else {
            assert_eq!(r["installed"], json!(true));
            assert_eq!(r["checked"], json!(["bad.ts"]));
            let findings = r["findings"].as_array().unwrap();
            assert!(!findings.is_empty(), "expected a real tsc type error for a string-to-number assignment");
            assert_eq!(findings[0]["file"], json!("bad.ts"));
            assert_eq!(findings[0]["severity"], json!("error"));
        }
    }
}
