//! typecheck-project.rs — twin of skills/codebase/typecheck-project.mjs (default export, db-first
//! sig: `async function typecheckProject(input, context)`). NAMES ARE CONTRACT: `typecheckProject`.
//! `norm`/`rel` stay private helpers (no JS `export`), exactly like the JS module-scope consts.
//!
//! BESPOKE-NATIVE (no conform-db fixture/frontier entry), same class as typecheck-ts.rs /
//! typecheck-py.rs / lint-eslint.rs: the whole point of this skill is shelling out to a REAL
//! `tsc`/`pyright` subprocess (via `node <entry> ...`) against the REAL project root (the active
//! workspace or an explicit `root`) and parsing its stdout — not a conform-db-seedable value.
//! Proven below via `#[cfg(test)]` against fake `node` programs standing in for tsc/pyright (same
//! precedent as typecheck-py.rs's `runs_real_pyright_like_fake_entry_over_inline_source...` test)
//! plus the deterministic skip branches (no tsc/pyright installed, no tsconfig.json).
//!
//! Cross-module calls (already-converted twins, reused not reimplemented):
//! `crate::engine_workspace_root::getWorkspaceRoot`,
//! `crate::codebase__helpers_run_cmd::{runCmd, resolveBin}`,
//! `crate::codebase_typecheck_ts::parseTsc`, `crate::codebase_typecheck_py::parsePyright`.
//! `TOOLS_DIR` (imported from typecheck-detect.mjs, itself `roots.mjs TOOLS_DIR + '/typecheck'`)
//! is inlined as a private `tools_dir()` fn duplicating the private helper of the same name in
//! typecheck-detect.rs / typecheck-ts.rs / typecheck-py.rs (not `pub` there — playbook precedent
//! for const-only / private-helper deps is to reimplement locally, not import cross-module).
//!
//! `process.execPath` (the JS lane's own node binary) has no meaningful Rust twin — invokes the
//! plain `node` command instead (must be on PATH), same disclosed-scope precedent as the sibling
//! typecheck-ts.rs/typecheck-py.rs twins.
//!
//! `rel(root, f)`: tsc prints paths relative to cwd (kept as-is), pyright prints absolute paths
//! (relativized against `root` via `node:path.relative`, win32 semantics per this crate's
//! workspace-root.rs precedent — `f` here is always a real absolute Windows path pyright itself
//! emitted, so a simple case-preserving prefix-strip suffices; unlike workspace-root.rs's
//! containment CHECK, this is a value-returning relativize, so it also strips a leading
//! separator and falls back to the raw path when `f` is not under `root`, matching Node's
//! `path.relative` "no common ancestor share" case for this substrate's always-same-drive inputs).
// QUALITY: 79/100 — real subprocess dispatch reusing 2 already-proven adapter twins; TOOLS_DIR/rel duplicated per-file (disclosed, matches sibling convention)

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
// roots.mjs TOOLS_DIR = join(HOME_DIR, 'tools'); typecheck-detect.mjs TOOLS_DIR = join(that, 'typecheck').
fn tools_dir() -> String {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("tools").join("typecheck").to_string_lossy().into_owned()
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

// `const norm = (p) => String(p ?? '').replace(/\\/g, '/');`
fn norm(p: Option<&Value>) -> String {
    str_qq(p).replace('\\', "/")
}

// `const rel = (root, f) => { if (!f) return '(project)'; ... }` — see module doc for the
// win32-relativize scope. `!f` is JS-falsy: null/undefined/''/0/false, not just missing.
fn rel(root: &str, f: Option<&Value>) -> String {
    let falsy = match f {
        None | Some(Value::Null) => true,
        Some(Value::Bool(b)) => !*b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |x| x == 0.0),
        Some(Value::String(s)) => s.is_empty(),
        _ => false,
    };
    if falsy {
        return "(project)".to_string();
    }
    let n = norm(f);
    let is_abs = n.starts_with('/') || (n.len() > 1 && n.as_bytes()[0].is_ascii_alphabetic() && n.as_bytes()[1] == b':');
    if !is_abs {
        return n;
    }
    let root_norm = norm(Some(&json!(root)));
    let root_lc = root_norm.to_ascii_lowercase();
    let n_lc = n.to_ascii_lowercase();
    let prefix = if root_lc.ends_with('/') { root_lc.clone() } else { format!("{root_lc}/") };
    let relativized = if n_lc == root_lc {
        String::new()
    } else if n_lc.starts_with(&prefix) {
        n[prefix.len()..].to_string()
    } else {
        String::new() // no common ancestor share → relative(...) === '' path never taken in practice; fall back below
    };
    if relativized.is_empty() { n } else { relativized }
}

/// `export default async function typecheckProject(input, context)`.
pub fn typecheckProject(input: &Value, db: &Connection) -> Result<Value, String> {
    let root_v = input.get("root").filter(|v| !v.is_null()).cloned().unwrap_or_else(|| crate::engine_workspace_root::getWorkspaceRoot(db));
    let root = root_v.as_str().unwrap_or_default().to_string();

    let mut findings: Vec<Value> = Vec::new();
    let mut ran: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();

    // ── TypeScript: tsc --noEmit over the project's tsconfig (cross-module). ──
    let tsc = {
        let a = crate::codebase__helpers_run_cmd::resolveBin(&json!(root), &json!("typescript"), &json!("tsc"));
        if a.as_str().map(|s| !s.is_empty()).unwrap_or(false) {
            Some(a.as_str().unwrap().to_string())
        } else {
            let b = crate::codebase__helpers_run_cmd::resolveBin(&json!(tools_dir()), &json!("typescript"), &json!("tsc"));
            b.as_str().filter(|s| !s.is_empty()).map(|s| s.to_string())
        }
    };
    match &tsc {
        None => skipped.push(json!({ "id": "typescript", "reason": "no typescript (project or downloaded)" })),
        Some(tsc_entry) => {
            let tsconfig = PathBuf::from(&root).join("tsconfig.json");
            if !tsconfig.exists() {
                skipped.push(json!({ "id": "typescript", "reason": "no tsconfig.json at the project root" }));
            } else {
                let args = json!([tsc_entry, "--noEmit", "--pretty", "false"]);
                let opts = json!({ "cwd": root, "timeoutMs": 300000 });
                let r = crate::codebase__helpers_run_cmd::runCmd(&json!("node"), &args, &opts);
                let stdout = r.get("stdout").and_then(Value::as_str).unwrap_or_default();
                let stderr = r.get("stderr").and_then(Value::as_str).unwrap_or_default();
                let combined = format!("{stdout}\n{stderr}");
                let parsed = crate::codebase_typecheck_ts::parseTsc(&json!(combined));
                if let Some(arr) = parsed.as_array() {
                    for d in arr {
                        findings.push(json!({
                            "file": rel(&root, d.get("file")),
                            "line": d.get("line").cloned().unwrap_or(Value::Null),
                            "col": d.get("col").cloned().unwrap_or(Value::Null),
                            "code": d.get("code").cloned().unwrap_or(Value::Null),
                            "message": d.get("message").cloned().unwrap_or(Value::Null),
                            "severity": "error",
                        }));
                    }
                }
                ran.push(json!("typescript"));
            }
        }
    }

    // ── Python: pyright over the project root (uses the project's pyright config / env). ──
    let pyright = {
        let a = crate::codebase__helpers_run_cmd::resolveBin(&json!(root), &json!("pyright"), &json!("pyright"));
        if a.as_str().map(|s| !s.is_empty()).unwrap_or(false) {
            Some(a.as_str().unwrap().to_string())
        } else {
            let b = crate::codebase__helpers_run_cmd::resolveBin(&json!(tools_dir()), &json!("pyright"), &json!("pyright"));
            b.as_str().filter(|s| !s.is_empty()).map(|s| s.to_string())
        }
    };
    match &pyright {
        None => skipped.push(json!({ "id": "python", "reason": "no pyright (project or downloaded)" })),
        Some(pyright_entry) => {
            let args = json!([pyright_entry, "--outputjson", root]);
            let opts = json!({ "cwd": root, "timeoutMs": 300000 });
            let r = crate::codebase__helpers_run_cmd::runCmd(&json!("node"), &args, &opts);
            let stdout = r.get("stdout").cloned().unwrap_or(json!(""));
            let diags = crate::codebase_typecheck_py::parsePyright(&stdout);
            if let Some(arr) = diags.as_array() {
                for d in arr {
                    findings.push(json!({
                        "file": rel(&root, d.get("file")),
                        "line": d.get("line").cloned().unwrap_or(Value::Null),
                        "col": d.get("col").cloned().unwrap_or(Value::Null),
                        "code": nz_or(d.get("code").cloned(), json!("pyright")),
                        "message": d.get("message").cloned().unwrap_or(Value::Null),
                        "severity": "error",
                    }));
                }
            }
            ran.push(json!("python"));
        }
    }

    // ── EVERY OTHER LANGUAGE (2026-09-19): the toolchain check, when the project speaks it ──
    {
        let rp = std::path::Path::new(&root);
        for t in crate::codebase_toolchain_checks::tools_of_kind("check") {
            if !crate::codebase_toolchain_checks::project_has(rp, t) { continue; }
            let r = crate::codebase_toolchain_checks::run_tool(rp, t, 300_000);
            if r.get("installed") == Some(&json!(false)) { skipped.push(json!({ "id": t.id, "reason": r.get("reason").cloned().unwrap_or(json!("not installed")) })); continue; }
            for d in r.get("findings").and_then(Value::as_array).cloned().unwrap_or_default() {
                findings.push(json!({ "file": rel(&root, d.get("file")), "line": d["line"], "col": d["col"], "code": d["code"], "message": d["message"], "severity": d["severity"] }));
            }
            ran.push(json!(t.id));
        }
    }

    let mut o = Map::new();
    o.insert("ok".into(), json!(findings.is_empty()));
    o.insert("root".into(), json!(norm(Some(&json!(root)))));
    o.insert("ran".into(), json!(ran));
    o.insert("skipped".into(), json!(skipped));
    o.insert("findings".into(), json!(findings));
    Ok(Value::Object(o))
}

// `d.code ?? 'pyright'` — JS `??`.
fn nz_or(v: Option<Value>, fallback: Value) -> Value {
    match v {
        Some(x) if !x.is_null() => x,
        _ => fallback,
    }
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

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-typecheck-project-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // `tools_dir()`'s fallback path reads real ANTILOKI_HOME; SAFETY: serialize via the shared
    // crate-wide lock (test_env_lock.rs) and point it at an empty temp dir so a real machine
    // install under the developer's actual ~/.antiloki/tools/typecheck never leaks into these
    // "not installed" assertions — same convention as typecheck-py.rs's `with_home`.
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
    fn rel_returns_project_placeholder_for_falsy_file() {
        assert_eq!(rel("C:/proj", None), "(project)");
        assert_eq!(rel("C:/proj", Some(&Value::Null)), "(project)");
        assert_eq!(rel("C:/proj", Some(&json!(""))), "(project)");
    }

    #[test]
    fn rel_keeps_already_relative_paths_as_is() {
        assert_eq!(rel("C:/proj", Some(&json!("src/app.ts"))), "src/app.ts");
    }

    #[test]
    fn rel_relativizes_an_absolute_path_under_root() {
        assert_eq!(rel("C:\\proj", Some(&json!("C:\\proj\\src\\app.py"))), "src/app.py");
    }

    #[test]
    fn no_tsc_no_pyright_skips_both() {
        let dir = temp_dir("neither-installed");
        let home = temp_dir("neither-installed-home");
        let c = db();
        let r = with_home(home.to_str().unwrap(), || {
            typecheckProject(&json!({ "root": dir.to_string_lossy() }), &c).unwrap()
        });
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["ran"], json!([]));
        let skipped = r["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 2);
        assert_eq!(skipped[0]["id"], json!("typescript"));
        assert_eq!(skipped[0]["reason"], json!("no typescript (project or downloaded)"));
        assert_eq!(skipped[1]["id"], json!("python"));
        assert_eq!(r["findings"], json!([]));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn tsc_installed_but_no_tsconfig_skips_typescript_only() {
        let dir = temp_dir("no-tsconfig");
        let home = temp_dir("no-tsconfig-home");
        let ts_pkg_dir = dir.join("node_modules").join("typescript");
        fs::create_dir_all(&ts_pkg_dir).unwrap();
        fs::write(ts_pkg_dir.join("package.json"), r#"{"bin":{"tsc":"bin/tsc.js"}}"#).unwrap();
        fs::create_dir_all(ts_pkg_dir.join("bin")).unwrap();
        fs::write(ts_pkg_dir.join("bin").join("tsc.js"), "").unwrap();

        let c = db();
        let r = with_home(home.to_str().unwrap(), || {
            typecheckProject(&json!({ "root": dir.to_string_lossy() }), &c).unwrap()
        });
        let skipped = r["skipped"].as_array().unwrap();
        assert_eq!(skipped.len(), 2);
        assert_eq!(skipped[0]["id"], json!("typescript"));
        assert_eq!(skipped[0]["reason"], json!("no tsconfig.json at the project root"));
        assert_eq!(skipped[1]["id"], json!("python"));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn real_fake_tsc_and_pyright_report_findings_relativized_against_root() {
        // Fake tsc: a tiny real `node` program that prints one tsc-shaped error line to stdout.
        let dir = temp_dir("run-both");
        let root = dir.join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("tsconfig.json"), "{}").unwrap();

        let ts_pkg_dir = root.join("node_modules").join("typescript");
        fs::create_dir_all(&ts_pkg_dir).unwrap();
        fs::write(ts_pkg_dir.join("package.json"), r#"{"bin":{"tsc":"fake-tsc.js"}}"#).unwrap();
        fs::write(
            ts_pkg_dir.join("fake-tsc.js"),
            "console.log(\"src/app.ts(3,7): error TS2322: bad type.\");",
        )
        .unwrap();

        let py_pkg_dir = root.join("node_modules").join("pyright");
        fs::create_dir_all(&py_pkg_dir).unwrap();
        fs::write(py_pkg_dir.join("package.json"), r#"{"bin":{"pyright":"fake-pyright.js"}}"#).unwrap();
        let root_fwd = root.to_string_lossy().replace('\\', "\\\\");
        let fake_pyright_js = format!(
            "console.log(JSON.stringify({{generalDiagnostics:[{{file:\"{root_fwd}\\\\lib\\\\mod.py\",severity:\"error\",rule:\"reportGeneralTypeIssues\",range:{{start:{{line:1,character:2}}}},message:\"bad type\"}}]}}));"
        );
        fs::write(py_pkg_dir.join("fake-pyright.js"), fake_pyright_js).unwrap();

        let c = db();
        let r = typecheckProject(&json!({ "root": root.to_string_lossy() }), &c).unwrap();
        assert_eq!(r["ran"], json!(["typescript", "python"]));
        assert_eq!(r["skipped"], json!([]));
        assert_eq!(r["ok"], json!(false));
        let findings = r["findings"].as_array().unwrap();
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0]["file"], json!("src/app.ts"));
        assert_eq!(findings[0]["line"], json!(3));
        assert_eq!(findings[0]["code"], json!("TS2322"));
        assert_eq!(findings[1]["file"], json!("lib/mod.py"));
        assert_eq!(findings[1]["line"], json!(2));
        assert_eq!(findings[1]["code"], json!("reportGeneralTypeIssues"));
        let _ = fs::remove_dir_all(&dir);
    }
}
