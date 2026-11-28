//! typecheck-detect.rs — twin of skills/codebase/typecheck-detect.mjs (db + real-fs lane).
//! NAMES ARE CONTRACT: `typecheckDetect` (default), `checkerFor`, `isInstalled`, `checkerForExt`.
//! `CHECKERS` and this module's own `TOOLS_DIR` are plain data consts in the JS (not units per
//! the census rule — only functions count); modeled here as the `checkers()` / `tools_dir()`
//! helper fns, same convention as the sibling god-driver-halt.rs's `halt_file()`.
//!
//! Which typecheckers (TypeScript, Python via pyright) are present in the atlas (file-extension
//! counts from `code_nodes`) and already installed under `HOME_DIR/tools/typecheck/node_modules/
//! <pkg>/package.json`.
//!
//! `TOOLS_DIR` here = roots.mjs `TOOLS_DIR` (`HOME_DIR/tools`) + `/typecheck`; `HOME_DIR` has an
//! `ANTILOKI_HOME` env override (lesson 6 — portable, NOT fixed to the engine's own location), so
//! this twin resolves it exactly like the already-converted god-driver-halt.rs / driver-halt-
//! check.rs precedent (no `dirs`-crate dependency): `ANTILOKI_HOME` env override (plain path
//! join) else Node `os.homedir()` semantics (POSIX `$HOME`; Windows `%USERPROFILE%` falling back
//! to `%HOMEDRIVE%%HOMEPATH%`).
//!
//! `isInstalled` does a REAL `existsSync`-equivalent fs check (and `typecheckDetect` calls it
//! internally for every checker) — not fixture-testable through the conform-db JSON harness (it
//! has no fs support), so this file's tests are bespoke-native: a temp dir + `ANTILOKI_HOME`
//! override, mirroring god-driver-halt.rs's `with_home` pattern. `checkerFor`/`checkerForExt` are
//! pure/deterministic and are exercised the same way, just without needing the env override.
//!
//! `String(r.path)` (no `??`) on the SQL row is `js_string` (Null → the literal string "null"),
//! NOT `str_qq` — the row's `path` key is always present (selected column), so a SQL NULL comes
//! through as JSON `null`, never a missing key.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
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
// roots.mjs TOOLS_DIR = join(HOME_DIR, 'tools'); this module's TOOLS_DIR = join(that, 'typecheck').
fn tools_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("tools").join("typecheck")
}

// CHECKERS — same order/shape as the JS array literal.
fn checkers() -> Vec<Value> {
    vec![
        json!({ "id": "typescript", "label": "TypeScript", "pkg": "typescript", "exts": [".ts", ".mts", ".cts", ".tsx"] }),
        json!({ "id": "python", "label": "Python", "pkg": "pyright", "exts": [".py", ".pyi"] }),
    ]
}

// = JS String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

// = JS String(v) (no ??)
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

// JS: `String(r.path).match(/\.[^./\\]+$/)`. A string has only one "last '.'", so the regex's
// no-'.'-in-the-captured-run constraint reduces to: take the tail after the FINAL '.'; a match
// exists iff that tail is non-empty and contains no '/' or '\\' (the extension is `.` + tail).
fn ext_of(path: &str) -> Option<String> {
    let i = path.rfind('.')?;
    let tail = &path[i + 1..];
    if tail.is_empty() || tail.contains('/') || tail.contains('\\') {
        return None;
    }
    Some(path[i..].to_string())
}

pub fn checkerFor(id: &Value) -> Value {
    checkers().into_iter().find(|c| c["id"] == *id).unwrap_or(Value::Null)
}

pub fn isInstalled(id: &Value) -> Value {
    let c = checkerFor(id);
    if c.is_null() {
        return json!(false);
    }
    let pkg = c["pkg"].as_str().unwrap_or("");
    let path = tools_dir().join("node_modules").join(pkg).join("package.json");
    json!(path.exists())
}

pub fn checkerForExt(ext: &Value) -> Value {
    let e = str_qq(Some(ext)).to_lowercase();
    checkers()
        .into_iter()
        .find(|c| c["exts"].as_array().is_some_and(|a| a.iter().any(|x| x.as_str() == Some(e.as_str()))))
        .unwrap_or(Value::Null)
}

pub fn typecheckDetect(_input: &Value, db: &Connection) -> Result<Value, String> {
    let mut counts: HashMap<String, i64> = HashMap::new();
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT path FROM code_nodes WHERE origin = 'atlas' AND type = 'file'", &[]) {
        if let Some(arr) = rows.as_array() {
            for r in arr {
                let p = js_string(r.get("path").unwrap_or(&Value::Null));
                if let Some(e) = ext_of(&p) {
                    *counts.entry(e.to_lowercase()).or_insert(0) += 1;
                }
            }
        }
    }

    let mut languages: Vec<Value> = checkers()
        .into_iter()
        .map(|c| {
            let exts = c["exts"].as_array().cloned().unwrap_or_default();
            let file_count: i64 = exts.iter().map(|e| counts.get(e.as_str().unwrap_or("")).copied().unwrap_or(0)).sum();
            json!({
                "id": c["id"], "label": c["label"], "pkg": c["pkg"], "exts": c["exts"], "via": "npm",
                "file_count": file_count, "present": file_count > 0, "installed": isInstalled(&c["id"]),
            })
        })
        .collect();
    // EVERY OTHER PARSED LANGUAGE (2026-09-19): the toolchain's own check, probed on PATH — nothing to download
    let root = std::path::PathBuf::from(crate::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or_default());
    for t in crate::codebase_toolchain_checks::tools_of_kind("check") {
        let file_count: i64 = t.exts.iter().map(|e| counts.get(*e).copied().unwrap_or(0)).sum();
        languages.push(crate::codebase_toolchain_checks::describe(&root, t, file_count));
    }

    let mut out = Map::new();
    out.insert("tools_dir".into(), json!(tools_dir().to_string_lossy().into_owned()));
    out.insert("languages".into(), json!(languages));
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
        dir.push(format!("antiloki-typecheck-detect-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn checker_for_matches_known_ids_and_null_for_unknown() {
        assert_eq!(checkerFor(&json!("typescript"))["label"], json!("TypeScript"));
        assert_eq!(checkerFor(&json!("python"))["pkg"], json!("pyright"));
        assert_eq!(checkerFor(&json!("rust")), Value::Null);
        assert_eq!(checkerFor(&Value::Null), Value::Null);
    }

    #[test]
    fn checker_for_ext_lowercases_and_matches() {
        assert_eq!(checkerForExt(&json!(".TS"))["id"], json!("typescript"));
        assert_eq!(checkerForExt(&json!(".tsx"))["id"], json!("typescript"));
        assert_eq!(checkerForExt(&json!(".pyi"))["id"], json!("python"));
        assert_eq!(checkerForExt(&json!(".rs")), Value::Null);
        assert_eq!(checkerForExt(&Value::Null), Value::Null);
    }

    #[test]
    fn is_installed_reflects_real_fs_state() {
        let dir = temp_dir("installed");
        let ts_pkg_dir = dir.join("tools").join("typecheck").join("node_modules").join("typescript");
        fs::create_dir_all(&ts_pkg_dir).unwrap();
        fs::write(ts_pkg_dir.join("package.json"), "{}").unwrap();

        with_home(dir.to_str().unwrap(), || {
            assert_eq!(isInstalled(&json!("typescript")), json!(true));
            assert_eq!(isInstalled(&json!("python")), json!(false));
            assert_eq!(isInstalled(&json!("unknown")), json!(false));
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn typecheck_detect_counts_files_by_ext_and_reports_install_state() {
        let dir = temp_dir("detect");
        let ts_pkg_dir = dir.join("tools").join("typecheck").join("node_modules").join("typescript");
        fs::create_dir_all(&ts_pkg_dir).unwrap();
        fs::write(ts_pkg_dir.join("package.json"), "{}").unwrap();

        let c = db0();
        c.execute_batch(
            "CREATE TABLE code_nodes (id INTEGER PRIMARY KEY, origin TEXT, type TEXT, path TEXT); \
             INSERT INTO code_nodes (origin, type, path) VALUES \
               ('atlas','file','src/app.ts'), \
               ('atlas','file','SRC/App.TSX'), \
               ('atlas','file','lib/mod.py'), \
               ('atlas','file','nested/dir.name/file.pyi'), \
               ('atlas','file','readme.md'), \
               ('atlas','file','noext'), \
               ('other','file','ignored.ts'), \
               ('atlas','dir','src');",
        )
        .unwrap();

        let r = with_home(dir.to_str().unwrap(), || typecheckDetect(&json!({}), &c).unwrap());

        let langs = r["languages"].as_array().unwrap();
        let ts = langs.iter().find(|l| l["id"] == json!("typescript")).unwrap();
        assert_eq!(ts["file_count"], json!(2));
        assert_eq!(ts["present"], json!(true));
        assert_eq!(ts["installed"], json!(true));

        let py = langs.iter().find(|l| l["id"] == json!("python")).unwrap();
        assert_eq!(py["file_count"], json!(2));
        assert_eq!(py["present"], json!(true));
        assert_eq!(py["installed"], json!(false));

        let tools_dir_str = r["tools_dir"].as_str().unwrap().replace('\\', "/");
        assert!(tools_dir_str.ends_with("/tools/typecheck"), "got: {tools_dir_str}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn typecheck_detect_all_zero_when_code_nodes_missing() {
        let c = db0(); // no code_nodes table at all → the try/catch swallow path
        let dir = temp_dir("no-atlas");
        let r = with_home(dir.to_str().unwrap(), || typecheckDetect(&json!({}), &c).unwrap());
        for l in r["languages"].as_array().unwrap() {
            assert_eq!(l["file_count"], json!(0));
            assert_eq!(l["present"], json!(false));
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
