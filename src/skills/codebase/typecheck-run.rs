//! typecheck-run.rs — twin of skills/codebase/typecheck-run.mjs (default export, db-first sig:
//! `async function typecheckRun(input, context)`). NAMES ARE CONTRACT: `typecheckRun`.
//!
//! BESPOKE-NATIVE (no conform-db fixture/frontier entry): a THIN dispatcher over the already-
//! converted checker adapters — `crate::codebase_typecheck_project::typecheckProject` (scope:'all'
//! whole-project mode), `crate::codebase_typecheck_ts::typecheckTs` / `crate::codebase_typecheck_py::
//! typecheckPy` (per-file mode, keyed by `crate::codebase_typecheck_detect::checkerForExt`'s `id`).
//! None of those twins are conform-db fixture-testable themselves (each shells a real subprocess
//! over real files) — the routing/merge/cap logic here is pure and IS testable, so this twin's
//! `#[cfg(test)]` exercises the real dispatch against the real (not-installed-in-CI) adapters: the
//! "not installed" skip path is the one deterministic outcome guaranteed on any machine, plus the
//! pure no-adapter / cap / scope-all-not-installed-project branches.
//!
//! `ADAPTERS = { typescript: typecheckTs, python: typecheckPy }` (a plain object literal, not a
//! census unit) is modeled as a private `run_adapter(id, ...)` match, same convention as sibling
//! twins inlining a JS module-scope const map.
//!
//! `SCOPE_ALL_CAP = 400` is a plain const, transcribed as `SCOPE_ALL_CAP: usize`.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

const SCOPE_ALL_CAP: usize = 400;

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

// `String(f?.path ?? '').match(/\.[^./\\]+$/)?.[0]` — the extension including the leading dot,
// or None if the path has no '.' or the tail after the final '.' contains '/'/'\\'.
fn ext_of(path: &str) -> Option<String> {
    let i = path.rfind('.')?;
    let tail = &path[i + 1..];
    if tail.is_empty() || tail.contains('/') || tail.contains('\\') {
        return None;
    }
    Some(path[i..].to_string())
}

/// `export default async function typecheckRun(input, context)`.
pub fn typecheckRun(input: &Value, db: &Connection) -> Result<Value, String> {
    if input.get("scope").and_then(Value::as_str) == Some("all") {
        let root = input.get("root").cloned().unwrap_or(Value::Null);
        let r = crate::codebase_typecheck_project::typecheckProject(&json!({ "root": root }), db)?;

        let findings = r.get("findings").cloned().unwrap_or(json!([]));
        let findings_len = findings.as_array().map(|a| a.len()).unwrap_or(0) as i64;
        let ran = r.get("ran").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut by_checker = Map::new();
        for id in &ran {
            if let Some(id_s) = id.as_str() {
                by_checker.insert(id_s.to_string(), json!({ "findings": findings_len }));
            }
        }

        let mut o = Map::new();
        o.insert("ok".into(), r.get("ok").cloned().unwrap_or(Value::Null));
        o.insert("project".into(), json!(true));
        o.insert("checked".into(), json!([r.get("root").cloned().unwrap_or(Value::Null)]));
        o.insert("skipped".into(), r.get("skipped").cloned().unwrap_or(json!([])));
        o.insert("capped".into(), json!(0));
        o.insert("findings".into(), findings);
        o.insert("by_checker".into(), Value::Object(by_checker));
        return Ok(Value::Object(o));
    }

    let files_in = match input.get("files").and_then(Value::as_array) {
        Some(a) => a.clone(),
        None => return Err("typecheck-run: files is required".into()),
    };

    let mut capped: i64 = 0;
    let files: Vec<Value> = if files_in.len() > SCOPE_ALL_CAP {
        capped = (files_in.len() - SCOPE_ALL_CAP) as i64;
        files_in[..SCOPE_ALL_CAP].to_vec()
    } else {
        files_in
    };

    // bucket by checker id, preserving first-seen order (JS Object.create(null) key insertion order).
    let mut order: Vec<String> = Vec::new();
    let mut by_checker_files: std::collections::HashMap<String, Vec<Value>> = std::collections::HashMap::new();
    let mut skipped: Vec<Value> = Vec::new();
    for f in &files {
        let path_v = f.get("path").cloned();
        let path_s = str_qq(path_v.as_ref());
        let ext = ext_of(&path_s);
        let checker = ext.and_then(|e| {
            let c = crate::codebase_typecheck_detect::checkerForExt(&json!(e));
            if c.is_null() { None } else { Some(c) }
        });
        match checker {
            None => skipped.push(json!({ "path": f.get("path").cloned().unwrap_or(Value::Null), "reason": "no adapter" })),
            Some(c) => {
                let id = c.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                if !by_checker_files.contains_key(&id) {
                    order.push(id.clone());
                }
                by_checker_files.entry(id).or_default().push(f.clone());
            }
        }
    }

    let mut findings: Vec<Value> = Vec::new();
    let mut checked: Vec<Value> = Vec::new();
    let mut by_checker = Map::new();
    for id in &order {
        let fs = by_checker_files.get(id).cloned().unwrap_or_default();
        let res = run_adapter(id, &json!({ "files": fs }), db)?;
        if res.get("installed") == Some(&Value::Bool(false)) {
            for f in &fs {
                skipped.push(json!({ "path": f.get("path").cloned().unwrap_or(Value::Null), "reason": format!("{id} not installed") }));
            }
            continue;
        }
        let res_findings = res.get("findings").and_then(Value::as_array).cloned().unwrap_or_default();
        let res_checked = res.get("checked").and_then(Value::as_array).cloned().unwrap_or_default();
        by_checker.insert(id.clone(), json!({ "checked": res_checked.len() as i64, "findings": res_findings.len() as i64 }));
        findings.extend(res_findings);
        checked.extend(res_checked);
    }

    let mut o = Map::new();
    o.insert("ok".into(), json!(findings.is_empty()));
    o.insert("checked".into(), json!(checked));
    o.insert("skipped".into(), json!(skipped));
    o.insert("capped".into(), json!(capped));
    o.insert("findings".into(), json!(findings));
    o.insert("by_checker".into(), Value::Object(by_checker));
    Ok(Value::Object(o))
}

fn run_adapter(id: &str, input: &Value, db: &Connection) -> Result<Value, String> {
    match id {
        "typescript" => crate::codebase_typecheck_ts::typecheckTs(input, db),
        "python" => crate::codebase_typecheck_py::typecheckPy(input, db),
        _ => Ok(json!({ "installed": false, "findings": [], "checked": [] })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00'); \
             CREATE TABLE code_nodes (id INTEGER PRIMARY KEY, origin TEXT, type TEXT, path TEXT)",
        )
        .unwrap();
        c
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-typecheck-run-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ANTILOKI_HOME is process-global — serialize via the crate-wide lock, same as sibling
    // typecheck-*.rs twins, so a real machine install never leaks into these "not installed"
    // assertions.
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

    #[test]
    fn requires_files_when_no_scope_all() {
        let c = db();
        let err = typecheckRun(&json!({}), &c).unwrap_err();
        assert!(err.contains("files is required"), "got: {err}");
    }

    #[test]
    fn files_with_no_adapter_extension_are_skipped_with_no_adapter_reason() {
        let dir = temp_dir("no-adapter");
        let c = db();
        let r = with_home(dir.to_str().unwrap(), || {
            typecheckRun(&json!({ "files": [{ "path": "readme.md" }, { "path": "noext" }] }), &c).unwrap()
        });
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["checked"], json!([]));
        assert_eq!(r["capped"], json!(0));
        let skipped = r["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 2);
        assert_eq!(skipped[0]["path"], json!("readme.md"));
        assert_eq!(skipped[0]["reason"], json!("no adapter"));
        assert_eq!(skipped[1]["path"], json!("noext"));
        assert_eq!(r["by_checker"], json!({}));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ts_and_py_files_route_to_their_checker_and_skip_when_not_installed() {
        let dir = temp_dir("route-not-installed");
        let c = db();
        let r = with_home(dir.to_str().unwrap(), || {
            typecheckRun(
                &json!({ "files": [{ "path": "src/app.ts" }, { "path": "lib/mod.py" }, { "path": "readme.md" }] }),
                &c,
            )
            .unwrap()
        });
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["checked"], json!([]));
        let skipped = r["skipped"].as_array().unwrap();
        // 1 no-adapter (readme.md) + 1 typescript-not-installed + 1 python-not-installed
        assert_eq!(skipped.len(), 3);
        assert!(skipped.iter().any(|s| s["path"] == json!("readme.md") && s["reason"] == json!("no adapter")));
        assert!(skipped.iter().any(|s| s["path"] == json!("src/app.ts") && s["reason"] == json!("typescript not installed")));
        assert!(skipped.iter().any(|s| s["path"] == json!("lib/mod.py") && s["reason"] == json!("python not installed")));
        assert_eq!(r["by_checker"], json!({}));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn caps_files_over_scope_all_cap() {
        let dir = temp_dir("cap");
        let c = db();
        let files: Vec<Value> = (0..(SCOPE_ALL_CAP + 5)).map(|i| json!({ "path": format!("f{i}.md") })).collect();
        let r = with_home(dir.to_str().unwrap(), || typecheckRun(&json!({ "files": files }), &c).unwrap());
        assert_eq!(r["capped"], json!(5));
        assert_eq!(r["skipped"].as_array().unwrap().len(), SCOPE_ALL_CAP);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scope_all_dispatches_to_typecheck_project_and_wraps_result() {
        let dir = temp_dir("scope-all");
        let home = temp_dir("scope-all-home");
        let c = db();
        let r = with_home(home.to_str().unwrap(), || {
            typecheckRun(&json!({ "scope": "all", "root": dir.to_string_lossy() }), &c).unwrap()
        });
        assert_eq!(r["project"], json!(true));
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["capped"], json!(0));
        // typecheckProject normalizes `root` to forward slashes (JS `norm(root)`, mirrored by
        // typecheck-project.rs's `norm`) before returning it, and typecheck-run wraps that
        // normalized value into `checked` — not the raw Windows-backslash path passed in.
        let normalized_dir = dir.to_string_lossy().replace('\\', "/");
        assert_eq!(r["checked"], json!([normalized_dir]));
        assert_eq!(r["findings"], json!([]));
        assert_eq!(r["by_checker"], json!({})); // neither tsc nor pyright installed → `ran` empty
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&home);
    }
}
