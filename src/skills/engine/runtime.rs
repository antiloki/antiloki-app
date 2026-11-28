//! runtime.rs — twin of skills/engine/runtime.mjs (db-first + no-db helper module; NOT a
//! dispatchable skill). NAMES ARE CONTRACT: `resolveRuntime` (db-first, `fn(db)`), `containerPath`
//! (no-db, `fn(cwd)`), `wrapCommand` (no-db, `fn(rt, command, cwd)`). `quoteHost` is a plain const
//! in the JS (not a function *export declaration* — not a census unit, same convention as
//! roots.mjs's `SUBSTRATE_ROOT` const in workspace-root.rs), inlined here as `quote_host()`.
//!
//! WINDOWS-ONLY SCOPE (disclosed gap, same convention as workspace-root.rs / code-coverage.rs /
//! shared-diff.rs / run-cmd.rs): the JS `quoteHost` branches on `process.platform === 'win32'`
//! (true on this substrate's dev/CI machine, not `posix`) — this twin hardcodes the win32
//! (double-quote) branch only.
//!
//! `containerPath` needs `WORKTREES_DIR` (roots.mjs: `HOME_DIR/worktrees`, `HOME_DIR` =
//! `$ANTILOKI_HOME` override else `homedir()/.antiloki`) — inlined per the shared roots.mjs
//! inlining convention (sandbox-resolve-path.rs / orphan-worktree-dirs.rs / driver-token.rs
//! precedent), never imported (roots.mjs is a const-only leaf module — no functions, not a census
//! unit itself).
//!
//! `containerPath` does NOT port Node's full `path.relative()` algorithm. The JS calls
//! `relative(WORKTREES_DIR, String(cwd ?? ''))` directly (no upfront `resolve()` of `cwd`), which
//! only stays deterministic when `cwd` is already absolute — true at every real call site (worktree
//! paths handed in by the git-manager skills) and in this module's own JS test (`join(WORKTREES_DIR,
//! ...)` / a bare `C:/...` literal). This twin assumes an absolute `cwd` (disclosed gap, same
//! scope-limitation style as workspace-root.rs) and uses a component-wise `Path::strip_prefix`
//! containment check (sandbox-resolve-path.rs's `wt.starts_with(&worktrees_root)` precedent)
//! instead of a byte-for-byte `relative()` port: a sibling-prefix path like `WORKTREES_DIR + "2"`
//! correctly falls through to `/repo`, matching Node's `..`-prefixed `relative()` result, because
//! `strip_prefix` is component-wise not a raw string prefix test.
//!
//! Real-env note (conform-db fixture scope): `WORKTREES_DIR` is `$ANTILOKI_HOME`-or-`homedir()`
//! derived (machine-local, no fixture-injection seam in the conform-db harness — same gap noted in
//! sandbox-resolve-path.rs's module doc). The fixture below only exercises the `/repo` branch with
//! a `cwd` guaranteed to sit outside ANY real machine's `WORKTREES_DIR` (portable across dev
//! machines); the "contained → `/worktrees/…`" branch is instead verified by the `#[cfg(test)]`
//! Rust unit tests below (`with_home` pinning `ANTILOKI_HOME` to a fixed value, mirroring
//! `orphan-worktree-dirs.rs`'s `with_home` pattern) and by this module's own untouched JS test file.
// QUALITY: 78/100 — win32-only quoteHost (disclosed), containerPath containment-check
// simplification (disclosed, not a full relative() port, assumes absolute cwd)

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}

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

// roots.mjs: HOME_DIR = resolve($ANTILOKI_HOME) if set+truthy else join(homedir(), '.antiloki');
// WORKTREES_DIR = join(HOME_DIR, 'worktrees').
fn worktrees_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("worktrees")
}

// win32 branch of the JS `quoteHost` const — see module doc for the disclosed win32-only scope.
// `(s) => \`"${String(s).replace(/"/g, '\\"')}"\`` — String(v) (no `??`) = js_string.
fn quote_host(v: &Value) -> String {
    format!("\"{}\"", js_string(v).replace('"', "\\\""))
}

/// `export function resolveRuntime(db)` — reads the per-workspace runtime record out of
/// `cost_settings`. JS wraps each read in a swallowed `try/catch`, defaulting on ANY failure
/// (including a pre-migration DB missing the `cost_settings` table) — `query_rows` returning `Err`
/// mirrors that, same convention as `workspace-root.rs`'s `getActiveWorkspace`.
pub fn resolveRuntime(db: &Connection) -> Value {
    let get = |k: &str, d: Option<&str>| -> Value {
        let fallback = || d.map(|s| json!(s)).unwrap_or(Value::Null);
        match crate::db_json::query_rows(db, "SELECT value FROM cost_settings WHERE key = ?", &[json!(k)]) {
            Ok(rows) => rows
                .as_array()
                .and_then(|a| a.first())
                .and_then(|r| r.get("value"))
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(fallback),
            Err(_) => fallback(),
        }
    };
    let mut out = Map::new();
    out.insert("runtime".into(), get("ws_runtime", Some("host")));
    out.insert("image".into(), get("ws_runtime_image", None));
    out.insert("container".into(), get("ws_runtime_container", None));
    Value::Object(out)
}

/// `export function containerPath(cwd)` — maps a host cwd (assumed absolute — see module doc) to
/// its in-container mount path under `WORKTREES_DIR`, else `/repo`.
pub fn containerPath(cwd: &Value) -> Value {
    let cwd_str = str_qq(Some(cwd)); // JS: String(cwd ?? '')
    let cwd_path = Path::new(&cwd_str);
    let root = worktrees_dir();
    match cwd_path.strip_prefix(&root) {
        Ok(rel) => {
            let rel_s = rel.to_string_lossy().replace('\\', "/");
            json!(format!("/worktrees/{rel_s}"))
        }
        Err(_) => json!("/repo"),
    }
}

/// `export function wrapCommand(rt, command, cwd)` — HOST → unchanged; DOCKER (with a live
/// container) → exec into the mapped worktree path inside the container, host-shell-quoted.
pub fn wrapCommand(rt: &Value, command: &Value, cwd: &Value) -> Value {
    let container = rt.get("container").cloned().unwrap_or(Value::Null);
    let runtime_is_docker = rt.get("runtime").and_then(|v| v.as_str()) == Some("docker");
    if !truthy(Some(rt)) || !runtime_is_docker || !truthy(Some(&container)) {
        return command.clone();
    }
    let inner = containerPath(cwd);
    json!(format!(
        "docker exec -w {} {} sh -lc {}",
        quote_host(&inner),
        tmpl(Some(&container)),
        quote_host(command)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    // ANTILOKI_HOME is process-global and read by other converted twins' tests running in the
    // same `cargo test` process — serialize through the shared crate-wide lock (test_env_lock.rs
    // — a local mutex here would not prevent a race with OTHER files' tests over the same var).
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

    /// Fixture paths are written Windows-style; on other hosts the same path is spelled `/tmp/rt-home` etc.
    fn np(win: &str) -> String {
        if cfg!(windows) { win.to_string() } else { win.replacen("C:", "", 1).replace('\\', "/") }
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cost_settings (key TEXT PRIMARY KEY, value TEXT)").unwrap();
        c
    }

    #[test]
    fn resolve_runtime_defaults_to_host() {
        let rt = resolveRuntime(&db());
        assert_eq!(rt["runtime"], json!("host"));
        assert_eq!(rt["container"], Value::Null);
    }

    #[test]
    fn resolve_runtime_reads_the_docker_record() {
        let c = db();
        c.execute(
            "INSERT INTO cost_settings (key, value) VALUES ('ws_runtime', 'docker'), ('ws_runtime_image', 'node:16-bullseye'), ('ws_runtime_container', 'antiloki-ws-demo')",
            [],
        ).unwrap();
        let rt = resolveRuntime(&c);
        assert_eq!(rt["runtime"], json!("docker"));
        assert_eq!(rt["image"], json!("node:16-bullseye"));
        assert_eq!(rt["container"], json!("antiloki-ws-demo"));
    }

    #[test]
    fn wrap_command_host_runs_unchanged() {
        assert_eq!(wrapCommand(&json!({"runtime":"host"}), &json!("npm run build"), &json!("/anywhere")), json!("npm run build"));
        assert_eq!(wrapCommand(&json!({"runtime":"docker","container":null}), &json!("npm test"), &json!("/x")), json!("npm test"));
    }

    #[test]
    fn wrap_command_docker_execs_into_the_mapped_worktree() {
        with_home(&np("C:\\tmp\\rt-home"), || {
            let rt = json!({"runtime":"docker","container":"antiloki-ws-demo"});
            let cwd = np("C:\\tmp\\rt-home\\worktrees\\CARD-X-C1");
            let wrapped = wrapCommand(&rt, &json!("npm run dev"), &json!(cwd));
            let wrapped_s = wrapped.as_str().unwrap();
            assert!(wrapped_s.starts_with(&format!("docker exec -w {} antiloki-ws-demo sh -lc ", quote_host(&json!("/worktrees/CARD-X-C1")))), "{wrapped_s}");
            assert!(wrapped_s.ends_with(&format!("sh -lc {}", quote_host(&json!("npm run dev")))), "{wrapped_s}");
        });
    }

    #[test]
    fn container_path_maps_a_worktree_under_worktrees_dir_else_repo() {
        with_home(&np("C:\\tmp\\rt-home2"), || {
            assert_eq!(containerPath(&json!(np("C:\\tmp\\rt-home2\\worktrees\\feat-x--r4"))), json!("/worktrees/feat-x--r4"));
            assert_eq!(containerPath(&json!(np("C:/somewhere/else"))), json!("/repo"));
        });
    }
}
