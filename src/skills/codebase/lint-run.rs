//! codebase/lint-run.rs — twin of skills/codebase/lint-run.mjs (default export, db-first sig
//! `export default async function lintRun(input, context)`). NAMES ARE CONTRACT: `lintRun`.
//!
//! BESPOKE-NATIVE: this file is a THIN DISPATCHER over the two style-lint adapters, both of which
//! already have real Rust twins that spawn REAL subprocesses (`codebase_lint_eslint::lintEslint`,
//! `codebase_lint_ruff::lintRuff`) — not something the conform-db two-lane harness can seed/replay
//! (no db writes; the "input" that matters is the real filesystem + real child processes owned by
//! the adapters it calls). This file's own logic — `scope:'all'` fan-out vs `files` routing by
//! extension, per-linter skip/aggregate, `by_linter` tally — IS deterministic and is exercised
//! directly below using a fake adapter substitute is not possible (adapters aren't DI'd in JS
//! either — `ADAPTERS` is a fixed const map), so tests exercise the real dispatch against a
//! not-installed eslint/ruff environment (both twins degrade to `installed:false`/`skipped`
//! deterministically when the tool truly isn't on PATH / in node_modules), proving the
//! aggregation/skip-bookkeeping logic end-to-end.
//!
//! `LINTERS` (lint-detect.mjs's plain `export const`, not a function) is NOT `pub` in
//! `lint-detect.rs` — same private-helper-duplication precedent as `lint-eslint.rs`'s/
//! `lint-ruff.rs`'s own `tools_dir()` duplication — so the two linter ids ("eslint","ruff", in
//! that fixed order) are inlined here as a local const array for the `scope:'all'` loop.
//! `linterForExt` IS `pub` in `lint-detect.rs` and is called directly (not reimplemented) for the
//! `files` routing branch, matching the "call the twin, don't restub it" rule.
//!
//! JS `context.run_cmd`/`context.resolve_bin` DI overrides have no Rust analogue (functions
//! aren't JSON args) — same disclosed scope as `lint-eslint.rs`/`lint-ruff.rs`: this twin's
//! adapter calls always exercise the real un-overridden JS default path.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

const LINTER_IDS: [&str; 2] = ["eslint", "ruff"];

fn call_adapter(id: &str, input: &Value, db: &Connection) -> Result<Value, String> {
    match id {
        "eslint" => crate::codebase_lint_eslint::lintEslint(input, db),
        "ruff" => crate::codebase_lint_ruff::lintRuff(input, db),
        _ => Err(format!("lint-run: unknown linter {id}")),
    }
}

/// `export default async function lintRun(input, context)` — see module doc.
pub fn lintRun(input: &Value, db: &Connection) -> Result<Value, String> {
    let mut findings: Vec<Value> = Vec::new();
    let mut by_linter: Map<String, Value> = Map::new();
    let mut ran: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();

    let scope = input.get("scope").and_then(Value::as_str);
    if scope == Some("all") {
        for id in LINTER_IDS.iter() {
            let res = call_adapter(id, &json!({ "all": true }), db)?;
            let installed_false = matches!(res.get("installed"), Some(Value::Bool(false)));
            let skipped_val = res.get("skipped").cloned().filter(|v| !v.is_null());
            if installed_false || skipped_val.is_some() {
                let reason = skipped_val.unwrap_or_else(|| json!(format!("{id} not installed")));
                let mut o = Map::new();
                o.insert("linter".into(), json!(id));
                o.insert("reason".into(), reason);
                skipped.push(Value::Object(o));
                continue;
            }
            let res_findings = res.get("findings").and_then(Value::as_array).cloned().unwrap_or_default();
            let findings_len = res_findings.len();
            findings.extend(res_findings);
            let checked_len = res.get("checked").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
            let mut bl = Map::new();
            bl.insert("checked".into(), json!(checked_len));
            bl.insert("findings".into(), json!(findings_len));
            by_linter.insert((*id).to_string(), Value::Object(bl));
            ran.push(json!(id));
        }
        // EVERY OTHER LANGUAGE (2026-09-19): the toolchain linter, when the project speaks it
        {
            let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
            let root = std::path::PathBuf::from(root_v.as_str().unwrap_or_default());
            for t in crate::codebase_toolchain_checks::tools_of_kind("lint") {
                if !crate::codebase_toolchain_checks::project_has(&root, t) { continue; }
                let r = crate::codebase_toolchain_checks::run_tool(&root, t, 300_000);
                if r.get("installed") == Some(&json!(false)) { skipped.push(json!({ "linter": t.id, "reason": r.get("reason").cloned().unwrap_or(json!("not installed")) })); continue; }
                let fs = r.get("findings").and_then(Value::as_array).cloned().unwrap_or_default();
                by_linter.insert(t.id.to_string(), json!({ "checked": 0, "findings": fs.len() }));
                findings.extend(fs);
                ran.push(json!(t.id));
            }
        }
        let mut out = Map::new();
        out.insert("ok".into(), json!(findings.is_empty()));
        out.insert("scope".into(), json!("all"));
        out.insert("ran".into(), json!(ran));
        out.insert("skipped".into(), json!(skipped));
        out.insert("findings".into(), json!(findings));
        out.insert("by_linter".into(), Value::Object(by_linter));
        out.insert("checked".into(), json!([]));
        return Ok(Value::Object(out));
    }

    let files = match input.get("files").and_then(Value::as_array) {
        Some(a) => a.clone(),
        None => return Err("lint-run: files is required".into()),
    };

    // Preserve JS `Object.entries(byLinter)` insertion order (first-seen linter id wins order).
    let mut by_linter_files: Vec<(String, Vec<Value>)> = Vec::new();
    for f in &files {
        let path = f.get("path").cloned().unwrap_or(Value::Null);
        let path_str = match &path {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => js_display(other),
        };
        let ext = ext_of(&path_str);
        let ext_v = match &ext {
            Some(e) => json!(e),
            None => Value::Null,
        };
        let linter = crate::codebase_lint_detect::linterForExt(&ext_v);
        if linter.is_null() {
            let mut o = Map::new();
            o.insert("path".into(), path);
            o.insert("reason".into(), json!("no linter"));
            skipped.push(Value::Object(o));
            continue;
        }
        let id = linter.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        match by_linter_files.iter_mut().find(|(k, _)| *k == id) {
            Some((_, v)) => v.push(f.clone()),
            None => by_linter_files.push((id, vec![f.clone()])),
        }
    }

    let mut checked: Vec<Value> = Vec::new();
    for (id, fs) in by_linter_files {
        let res = call_adapter(&id, &json!({ "files": fs }), db)?;
        let installed_false = matches!(res.get("installed"), Some(Value::Bool(false)));
        let skipped_val = res.get("skipped").cloned().filter(|v| !v.is_null());
        if installed_false || skipped_val.is_some() {
            let reason = skipped_val.unwrap_or_else(|| json!(format!("{id} not installed")));
            for f in &fs {
                let mut o = Map::new();
                o.insert("path".into(), f.get("path").cloned().unwrap_or(Value::Null));
                o.insert("reason".into(), reason.clone());
                skipped.push(Value::Object(o));
            }
            continue;
        }
        let res_findings = res.get("findings").and_then(Value::as_array).cloned().unwrap_or_default();
        let findings_len = res_findings.len();
        findings.extend(res_findings);
        let res_checked = res.get("checked").and_then(Value::as_array).cloned().unwrap_or_default();
        let checked_len = res_checked.len();
        checked.extend(res_checked);
        let mut bl = Map::new();
        bl.insert("checked".into(), json!(checked_len));
        bl.insert("findings".into(), json!(findings_len));
        by_linter.insert(id.clone(), Value::Object(bl));
        ran.push(json!(id));
    }

    let mut out = Map::new();
    out.insert("ok".into(), json!(findings.is_empty()));
    out.insert("ran".into(), json!(ran));
    out.insert("skipped".into(), json!(skipped));
    out.insert("findings".into(), json!(findings));
    out.insert("by_linter".into(), Value::Object(by_linter));
    out.insert("checked".into(), json!(checked));
    Ok(Value::Object(out))
}

// = JS `String(f?.path ?? '').match(/\.[^./\\]+$/)?.[0]`: the rightmost dot in the final path
// segment (no '/'/'\\' between the dot and end of string), mirroring lint-detect.rs's `ext_of`.
// linterForExt lowercases internally, so case is preserved here.
fn ext_of(path: &str) -> Option<String> {
    let filename = match path.rfind(['/', '\\']) {
        Some(i) => &path[i + 1..],
        None => path,
    };
    match filename.rfind('.') {
        Some(i) if i + 1 < filename.len() => Some(filename[i..].to_string()),
        _ => None,
    }
}

// = JS `String(v)` for a non-string/non-null JSON value (rare: path is normally a string).
fn js_display(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_display).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
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

    /// Run `f` with $ANTILOKI_HOME pointing at a fresh empty dir, so an eslint installed under the machine's
    /// real ~/.antiloki/tools/lint (any dev box) cannot turn a "not installed" assertion into a real run.
    fn with_empty_tools_home<T>(tag: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!("antiloki-lint-run-rs-home-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let prev = std::env::var("ANTILOKI_HOME").ok();
        unsafe { std::env::set_var("ANTILOKI_HOME", &home); }
        let out = f();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("ANTILOKI_HOME", v),
                None => std::env::remove_var("ANTILOKI_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&home);
        out
    }

    #[test]
    fn files_no_extension_match_is_skipped_no_linter() {
        let c = db();
        seed_active(&c, std::env::temp_dir().to_string_lossy().as_ref());
        let r = lintRun(&json!({ "files": [{ "path": "README.md" }] }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["ran"], json!([]));
        let skipped = r["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0]["path"], json!("README.md"));
        assert_eq!(skipped[0]["reason"], json!("no linter"));
        assert_eq!(r["findings"], json!([]));
        assert_eq!(r["checked"], json!([]));
    }

    #[test]
    fn files_missing_input_errors() {
        let c = db();
        seed_active(&c, std::env::temp_dir().to_string_lossy().as_ref());
        assert!(lintRun(&json!({}), &c).is_err());
    }

    #[test]
    fn scope_all_reports_by_linter_when_not_installed() {
        // In a workspace with no eslint node_modules and (likely) no real ruff on PATH, both
        // adapters degrade deterministically to installed:false/skipped — proving the skip
        // bookkeeping without depending on real tool presence for eslint's branch.
        let c = db();
        let dir = std::env::temp_dir().join(format!("antiloki-lint-run-rs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        seed_active(&c, dir.to_str().unwrap());
        let r = with_empty_tools_home("all", || lintRun(&json!({ "scope": "all" }), &c)).unwrap();
        assert_eq!(r["scope"], json!("all"));
        let skipped = r["skipped"].as_array().unwrap();
        let eslint_skip = skipped.iter().find(|s| s["linter"] == json!("eslint"));
        assert!(eslint_skip.is_some());
        assert_eq!(eslint_skip.unwrap()["reason"], json!("eslint not installed"));
        assert_eq!(r["checked"], json!([]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_route_by_extension_to_correct_linter_when_not_installed() {
        let c = db();
        let dir = std::env::temp_dir().join(format!("antiloki-lint-run-rs-route-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        seed_active(&c, dir.to_str().unwrap());
        let r = with_empty_tools_home("route", || lintRun(
            &json!({ "files": [{ "path": "src/app.js" }, { "path": "scripts/run.py" }, { "path": "notes.txt" }] }),
            &c,
        ))
        .unwrap();
        let skipped = r["skipped"].as_array().unwrap();
        assert!(skipped.iter().any(|s| s["path"] == json!("notes.txt") && s["reason"] == json!("no linter")));
        assert!(skipped.iter().any(|s| s["path"] == json!("src/app.js") && s["reason"] == json!("eslint not installed")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
