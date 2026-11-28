//! codebase/lint-detect.rs — twin of skills/codebase/lint-detect.mjs (db READ lane + no-db
//! helpers). NAMES ARE CONTRACT: `linterFor`, `linterForExt`, `npmInstalled`, `isInstalled`
//! (named exports) and `lintDetect` (default). `TOOLS_DIR`/`LINTERS` are plain `export const`
//! DATA (not functions — census excludes them), modeled here as the private `tools_dir()` fn
//! and the `LINTERS` array, same convention as god-driver-halt.rs's `halt_file()`.
//!
//! TOOLS_DIR = roots.mjs `HOME_DIR/tools/lint`; HOME_DIR resolution mirrors god-driver-halt.rs
//! (no `dirs`-crate dep): `ANTILOKI_HOME` env override else Node `os.homedir()` semantics.
//!
//! SCOPE LIMIT (disclosed, matches this task's file-specific note): `LINTERS` has TWO
//! distribution stories — eslint (`via:'npm'`, `existsSync`-only, deterministic given a real
//! filesystem) and ruff (`via:'path'`, a REAL subprocess probe `ruff --version`, JS
//! `run-cmd.mjs`). Unlike lesson-5's input-flag-gated skips, this branch is NOT
//! conditionally skippable via any argument — `LINTERS` is fixed module data, so both
//! `isInstalled('ruff')` and every call to `lintDetect()` always execute the real subprocess
//! probe. The Rust twin still performs the equivalent real probe (so production behavior
//! matches JS 1:1 on any given machine), but:
//!   - JS `isInstalled(id, run = runCmd)` accepts a DI `run` callback (used by
//!     `lintDetect`'s `context.run_cmd` override) that has no meaningful Rust twin (functions
//!     aren't JSON-serializable fixture args) — the Rust `isInstalled`/`lintDetect` always use
//!     the REAL probe, i.e. the un-overridden JS default path.
//!   - JS `run-cmd.mjs` enforces an 8000ms timeout via `setTimeout` + `SIGKILL`; the Rust probe
//!     has no timeout (blocks on `Command::output()`), a disclosed gap that only matters for a
//!     hung `ruff` binary — outside this file's fixture-tested scope.
//!   - Unit tests below exercise the eslint/`existsSync`-only path deterministically (seeded
//!     temp `ANTILOKI_HOME`); the ruff/subprocess path is only checked for shape (`Value::Bool`),
//!     never a specific value, since real ruff presence varies by machine.
//!
//! db read: `SELECT path FROM code_nodes WHERE origin = 'atlas' AND type = 'file'`, wrapped in a
//! try/catch in JS ("no atlas" → all-zero counts) — twinned via `.ok()` + `unwrap_or_default()`,
//! same convention as feature-inventory.rs. Extension extraction mirrors JS
//! `String(r.path).match(/\.[^./\\]+$/)` (rightmost dot in the final path segment, lowercased).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;

struct Linter {
    id: &'static str,
    label: &'static str,
    via: &'static str,
    pkg: Option<&'static str>,
    bin: &'static str,
    exts: &'static [&'static str],
}

const LINTERS: [Linter; 2] = [
    Linter {
        id: "eslint",
        label: "ESLint (JS/TS)",
        via: "npm",
        pkg: Some("eslint"),
        bin: "eslint",
        exts: &[".js", ".jsx", ".mjs", ".cjs", ".ts", ".tsx", ".mts", ".cts"],
    },
    Linter {
        id: "ruff",
        label: "Ruff (Python)",
        via: "path",
        pkg: None,
        bin: "ruff",
        exts: &[".py", ".pyi"],
    },
];

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
// TOOLS_DIR (roots.mjs) = join(HOME_DIR, 'tools'); this module's TOOLS_DIR = join(TOOLS_ROOT, 'lint').
fn tools_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("tools").join("lint")
}

fn linter_for(id: &Value) -> Option<&'static Linter> {
    let id_s = id.as_str()?;
    LINTERS.iter().find(|l| l.id == id_s)
}

// Full LINTERS-array shape (id,label,via,pkg,bin,exts), as returned verbatim by JS `linterFor`/
// `linterForExt` (`LINTERS.find(...)` returns the raw array element, `bin` included).
fn linter_full(l: &Linter) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), json!(l.id));
    o.insert("label".into(), json!(l.label));
    o.insert("via".into(), json!(l.via));
    o.insert("pkg".into(), match l.pkg { Some(p) => json!(p), None => Value::Null });
    o.insert("bin".into(), json!(l.bin));
    o.insert("exts".into(), json!(l.exts));
    Value::Object(o)
}

// = JS `String(v ?? '')`: nullish (null/undefined) coalesces to '' BEFORE String() ever runs, so
// non-primitive values (Array/Object) still need real JS String() coercion, not raw JSON text —
// delegate to js_string (below) for everything past the null/undefined case.
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(other) => js_string(other),
    }
}

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

// = JS `String(r.path).match(/\.[^./\\]+$/)` then `[0].toLowerCase()`: the rightmost dot in the
// final path segment, requiring >=1 char after it with no further '.', '/', '\\'.
fn ext_of(path: &str) -> Option<String> {
    let filename = match path.rfind(['/', '\\']) {
        Some(i) => &path[i + 1..],
        None => path,
    };
    match filename.rfind('.') {
        Some(i) if i + 1 < filename.len() => Some(filename[i..].to_lowercase()),
        _ => None,
    }
}

pub fn linterFor(id: &Value) -> Value {
    linter_for(id).map(linter_full).unwrap_or(Value::Null)
}

pub fn linterForExt(ext: &Value) -> Value {
    let e = str_qq(Some(ext)).to_lowercase();
    LINTERS.iter().find(|l| l.exts.contains(&e.as_str())).map(linter_full).unwrap_or(Value::Null)
}

pub fn npmInstalled(id: &Value) -> Value {
    let l = match linter_for(id) {
        Some(l) => l,
        None => return Value::Bool(false),
    };
    if l.via != "npm" {
        return Value::Bool(false);
    }
    let pkg = match l.pkg {
        Some(p) => p,
        None => return Value::Bool(false),
    };
    let path = tools_dir().join("node_modules").join(pkg).join("package.json");
    Value::Bool(path.exists())
}

// Real subprocess probe, mirroring run-cmd.mjs's `spawn(bin, ['--version'], { shell: win32 })`
// resolving `r.code === 0`. SCOPE LIMIT: no 8000ms timeout twin (see module doc) — disclosed,
// not exercised by fixtures.
fn probe_installed(bin: &str) -> bool {
    #[cfg(windows)]
    let output = crate::proc::cmd("cmd").arg("/C").arg(bin).arg("--version").output();
    #[cfg(not(windows))]
    let output = crate::proc::cmd(bin).arg("--version").output();
    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

pub fn isInstalled(id: &Value) -> Value {
    let l = match linter_for(id) {
        Some(l) => l,
        None => return Value::Bool(false),
    };
    if l.via == "npm" {
        return npmInstalled(id);
    }
    Value::Bool(probe_installed(l.bin))
}

pub fn lintDetect(_input: &Value, db: &Connection) -> Result<Value, String> {
    let mut counts: HashMap<String, i64> = HashMap::new();
    let rows = crate::db_json::query_rows(
        db,
        "SELECT path FROM code_nodes WHERE origin = 'atlas' AND type = 'file'",
        &[],
    )
    .ok()
    .and_then(|v| v.as_array().cloned())
    .unwrap_or_default();
    for r in &rows {
        let path = js_string(r.get("path").unwrap_or(&Value::Null));
        if let Some(ext) = ext_of(&path) {
            *counts.entry(ext).or_insert(0) += 1;
        }
    }

    let mut linters = Vec::new();
    for l in LINTERS.iter() {
        let file_count: i64 = l.exts.iter().map(|e| *counts.get(*e).unwrap_or(&0)).sum();
        let mut o = Map::new();
        o.insert("id".into(), json!(l.id));
        o.insert("label".into(), json!(l.label));
        o.insert("via".into(), json!(l.via));
        o.insert("pkg".into(), match l.pkg { Some(p) => json!(p), None => Value::Null });
        o.insert("exts".into(), json!(l.exts));
        o.insert("file_count".into(), json!(file_count));
        o.insert("present".into(), json!(file_count > 0));
        o.insert("installed".into(), isInstalled(&json!(l.id)));
        linters.push(Value::Object(o));
    }
    // EVERY OTHER PARSED LANGUAGE (2026-09-19): the community linter, probed on PATH
    let root = PathBuf::from(crate::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or_default());
    for t in crate::codebase_toolchain_checks::tools_of_kind("lint") {
        let file_count: i64 = t.exts.iter().map(|e| counts.get(*e).copied().unwrap_or(0)).sum();
        linters.push(crate::codebase_toolchain_checks::describe(&root, t, file_count));
    }

    let mut out = Map::new();
    out.insert("tools_dir".into(), json!(tools_dir().to_string_lossy().into_owned()));
    out.insert("linters".into(), json!(linters));
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
        dir.push(format!("antiloki-lint-detect-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db0() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn linter_for_and_ext_lookup() {
        let eslint = linterFor(&json!("eslint"));
        assert_eq!(eslint["id"], json!("eslint"));
        assert_eq!(eslint["via"], json!("npm"));
        assert_eq!(eslint["pkg"], json!("eslint"));
        assert_eq!(eslint["bin"], json!("eslint"));
        assert_eq!(linterFor(&json!("nope")), Value::Null);

        let ruff = linterFor(&json!("ruff"));
        assert_eq!(ruff["pkg"], Value::Null);

        assert_eq!(linterForExt(&json!(".TS"))["id"], json!("eslint"));
        assert_eq!(linterForExt(&json!(".py"))["id"], json!("ruff"));
        assert_eq!(linterForExt(&json!(".rs")), Value::Null);
        assert_eq!(linterForExt(&Value::Null), Value::Null);
    }

    #[test]
    fn npm_installed_reflects_real_fs_under_tools_dir() {
        let dir = temp_dir("npm-installed");
        with_home(dir.to_str().unwrap(), || {
            assert_eq!(npmInstalled(&json!("eslint")), Value::Bool(false));
            let pkg_dir = dir.join("tools").join("lint").join("node_modules").join("eslint");
            fs::create_dir_all(&pkg_dir).unwrap();
            fs::write(pkg_dir.join("package.json"), "{}").unwrap();
            assert_eq!(npmInstalled(&json!("eslint")), Value::Bool(true));
            // ruff is via:'path', never npm-installed regardless of fs.
            assert_eq!(npmInstalled(&json!("ruff")), Value::Bool(false));
            assert_eq!(npmInstalled(&json!("unknown")), Value::Bool(false));
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_installed_eslint_path_is_deterministic() {
        let dir = temp_dir("is-installed");
        with_home(dir.to_str().unwrap(), || {
            assert_eq!(isInstalled(&json!("eslint")), Value::Bool(false));
            let pkg_dir = dir.join("tools").join("lint").join("node_modules").join("eslint");
            fs::create_dir_all(&pkg_dir).unwrap();
            fs::write(pkg_dir.join("package.json"), "{}").unwrap();
            assert_eq!(isInstalled(&json!("eslint")), Value::Bool(true));
            assert_eq!(isInstalled(&json!("unknown")), Value::Bool(false));
            // ruff/path branch: real subprocess probe, environment-dependent — shape only.
            assert!(matches!(isInstalled(&json!("ruff")), Value::Bool(_)));
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lint_detect_counts_files_and_reports_tools_dir() {
        let dir = temp_dir("lint-detect");
        let c = db0();
        c.execute_batch(
            "CREATE TABLE code_nodes (id INTEGER PRIMARY KEY, origin TEXT, type TEXT, path TEXT); \
             INSERT INTO code_nodes (origin, type, path) VALUES \
               ('atlas','file','src/App.tsx'), \
               ('atlas','file','src/index.js'), \
               ('atlas','file','scripts/run.py'), \
               ('atlas','file','README.md'), \
               ('other','file','src/skip.ts'), \
               ('atlas','dir','src');",
        )
        .unwrap();
        with_home(dir.to_str().unwrap(), || {
            let pkg_dir = dir.join("tools").join("lint").join("node_modules").join("eslint");
            fs::create_dir_all(&pkg_dir).unwrap();
            fs::write(pkg_dir.join("package.json"), "{}").unwrap();

            let r = lintDetect(&json!({}), &c).unwrap();
            let expected_tools_dir = dir.join("tools").join("lint");
            assert_eq!(r["tools_dir"], json!(expected_tools_dir.to_string_lossy().into_owned()));

            let linters = r["linters"].as_array().unwrap();
            // eslint · ruff, then one toolchain linter per parsed language (2026-09-19)
            assert_eq!(linters.len(), 2 + crate::codebase_toolchain_checks::tools_of_kind("lint").len());
            assert!(linters[2..].iter().all(|l| l["via"] == json!("path") && l["install_hint"].is_string()));
            let eslint = &linters[0];
            assert_eq!(eslint["id"], json!("eslint"));
            assert_eq!(eslint["file_count"], json!(2)); // .tsx + .js
            assert_eq!(eslint["present"], json!(true));
            assert_eq!(eslint["installed"], json!(true));
            let keys: Vec<&String> = eslint.as_object().unwrap().keys().collect();
            assert_eq!(keys, ["id", "label", "via", "pkg", "exts", "file_count", "present", "installed"]);

            let ruff = &linters[1];
            assert_eq!(ruff["id"], json!("ruff"));
            assert_eq!(ruff["file_count"], json!(1)); // .py
            assert_eq!(ruff["present"], json!(true));
            assert!(matches!(ruff["installed"], Value::Bool(_))); // subprocess, shape only
        });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn lint_detect_no_atlas_table_yields_zero_counts() {
        let dir = temp_dir("no-atlas");
        let c = db0(); // no code_nodes table at all → the SELECT errors, caught → all zero
        with_home(dir.to_str().unwrap(), || {
            let r = lintDetect(&json!({}), &c).unwrap();
            for l in r["linters"].as_array().unwrap() {
                assert_eq!(l["file_count"], json!(0));
                assert_eq!(l["present"], json!(false));
            }
        });
        let _ = fs::remove_dir_all(&dir);
    }
}
