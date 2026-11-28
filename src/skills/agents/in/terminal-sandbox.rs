//! terminal-sandbox.rs — twin of skills/agents/in/terminal-sandbox.mjs. NAMES ARE CONTRACT:
//! `terminalSandbox` (default), `ensure`, `sandboxPath` (named exports — `ensure` is imported
//! directly by repl-direct.mjs's JS sibling, `sandboxPath` by the module itself elsewhere in the
//! substrate). `diff`/`apply`/`discard` stay private helpers here exactly as in the JS (plain
//! `function`, no `export`), reachable only through `terminalSandbox`'s mode dispatch.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: every mode shells out to a REAL `git`
//! subprocess against a REAL worktree directory tree under `WORKTREES_DIR`
//! (`$ANTILOKI_HOME/worktrees` or `~/.antiloki/worktrees`) — `ensure` runs `git worktree add`,
//! `apply` runs `git apply --check` then `git apply` against the real workspace root, `discard`
//! runs `git worktree remove --force`. None of that is fixture-seedable through conform-db's
//! two-lane harness (same class as tl-sandbox-create.rs / git-manager-worktree-ops.rs /
//! orphan-worktree-dirs.rs — all touch this same real-git-worktree family). Proven below via
//! `#[cfg(test)]` against a real temp git repo + real `ANTILOKI_HOME` tempdir, mirroring
//! git-manager-worktree-ops.rs's `make_temp_git_repo` / `with_home` pattern.
//!
//! Cross-module calls (already-converted twins, reused not reimplemented):
//! `crate::engine_workspace_root::getWorkspaceRoot` and `crate::cockpit__helpers_scope::splitByScope`.
//!
//! `WORKTREES_DIR` (roots.mjs: `join(HOME_DIR, 'worktrees')`, `HOME_DIR` = `$ANTILOKI_HOME` env
//! override else `join(homedir(), '.antiloki')`) is inlined here rather than imported — same
//! per-file inlining convention as orphan-worktree-dirs.rs / auth-sign.rs (no shared roots crate
//! exists yet).
//!
//! Scope-persistence best-effort block (`ensure`'s `.antiloki-scope.json` + git-exclude append):
//! ported literally including its real quirk — `git rev-parse --git-path info/exclude` runs with
//! `cwd=sp` (the sandbox worktree) but the JS then calls plain `readFileSync`/`appendFileSync` on
//! that returned path using the NODE PROCESS's own cwd (spawnSync's `cwd` option only scopes the
//! subprocess, not the parent's fs calls) — so a relative git-path answer resolves against the
//! REAL PROCESS CWD, not `sp`. This twin reproduces that exact resolution (`std::env::current_dir()`
//! join) rather than "fixing" it, since the whole block is wrapped in JS's best-effort
//! try/catch (`/* scope persistence best-effort; apply falls back to unrestricted */`) and errors
//! here are equally swallowed.
//!
//! Message-slicing (`.slice(0, 200)` / `.slice(0, 240)`) uses UTF-16 code-unit semantics in JS;
//! this twin approximates with `chars().take(N)` (ASCII/BMP-safe, same disclosed gap as every
//! sibling git-subprocess twin — orphan-worktree-dirs.rs, git-manager-worktree-ops.rs).
// QUALITY: 80/100 — real git subprocess dispatch (4 modes), scope-persistence quirk ported literally (disclosed), UTF-16 slice approximation (disclosed, shared convention)

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct GitOut {
    status: i32,
    stdout: String,
    stderr: String,
}

fn run_git(args: &[&str], cwd: &Path, input: Option<&str>) -> GitOut {
    use std::io::Write;
    let mut cmd = crate::proc::cmd("git");
    cmd.args(args).current_dir(cwd).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return GitOut { status: -1, stdout: String::new(), stderr: String::new() },
    };
    let mut child = child;
    if let Some(data) = input {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(data.as_bytes());
        }
    }
    match child.wait_with_output() {
        Ok(out) => GitOut {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Err(_) => GitOut { status: -1, stdout: String::new(), stderr: String::new() },
    }
}

fn slice_n(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn is_git_repo(root: &Path) -> bool {
    let r = run_git(&["rev-parse", "--is-inside-work-tree"], root, None);
    r.status == 0 && r.stdout.trim() == "true"
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

// roots.mjs: HOME_DIR = resolve($ANTILOKI_HOME) if set+truthy, else join(homedir(), '.antiloki');
// WORKTREES_DIR = join(HOME_DIR, 'worktrees').
fn worktrees_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("worktrees")
}

// `String(f || '')`: JS-falsy (null/undefined/false/0/'') coerces to '', otherwise String(f).
fn str_or_empty(v: &Value) -> String {
    let falsy = match v {
        Value::Null => true,
        Value::Bool(b) => !*b,
        Value::Number(n) => n.as_f64().map_or(false, |f| f == 0.0),
        Value::String(s) => s.is_empty(),
        _ => false,
    };
    if falsy {
        String::new()
    } else {
        match v {
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.to_string(),
            other => other.to_string(),
        }
    }
}

/// `sandboxPath(session_id)` — `String(session_id || '').replace(/[^A-Za-z0-9_-]/g, '').slice(0, 64)`,
/// then `resolve(SANDBOX_ROOT, 'term-' + safe)`. Throws `err.required` when nothing survives.
fn sandbox_path_str(session_id: &Value) -> Result<String, String> {
    let raw = str_or_empty(session_id);
    let safe: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(64)
        .collect();
    if safe.is_empty() {
        return Err("terminal-sandbox: session_id is required".to_string());
    }
    let p = worktrees_dir().join(format!("term-{safe}"));
    Ok(p.to_string_lossy().into_owned())
}

pub fn sandboxPath(session_id: &Value) -> Result<Value, String> {
    sandbox_path_str(session_id).map(|s| json!(s))
}

/// `cleanFolders(arr)`: per-entry trim → strip leading run of `.`/`/` → strip trailing run of
/// `/`/`\` → split on every `/`/`\` and rejoin with `/` → drop entries that end up empty.
fn clean_folders(arr: &Value) -> Vec<String> {
    let empty: Vec<Value> = Vec::new();
    let a = arr.as_array().unwrap_or(&empty);
    a.iter()
        .filter_map(|f| {
            let s = str_or_empty(f);
            let trimmed = s.trim();
            let no_lead = trimmed.trim_start_matches(|c| c == '.' || c == '/');
            let no_trail = no_lead.trim_end_matches(|c| c == '/' || c == '\\');
            let joined: String = no_trail
                .split(|c| c == '/' || c == '\\')
                .collect::<Vec<_>>()
                .join("/");
            if joined.is_empty() {
                None
            } else {
                Some(joined)
            }
        })
        .collect()
}

fn read_scope(sp: &Path) -> Vec<Value> {
    match std::fs::read_to_string(sp.join(".antiloki-scope.json")) {
        Ok(content) => match serde_json::from_str::<Value>(&content) {
            Ok(v) => v
                .get("allow_folders")
                .and_then(|a| a.as_array())
                .cloned()
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    }
}

/// `export async function ensure(input, context)`.
pub fn ensure(input: &Value, db: &Connection) -> Result<Value, String> {
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default().to_string();
    let session_id = input.get("session_id").cloned().unwrap_or(Value::Null);
    let allow_folders = input.get("allow_folders").cloned().unwrap_or(json!([]));
    let sp_str = sandbox_path_str(&session_id)?;
    let sp = PathBuf::from(&sp_str);

    if !is_git_repo(Path::new(&root)) {
        return Err(format!("terminal-sandbox: workspace root is not a git repo: {root}"));
    }

    let folders = clean_folders(&allow_folders);
    let mut created = false;

    if !sp.exists() {
        let add = run_git(&["worktree", "add", "--detach", &sp_str, "HEAD"], Path::new(&root), None);
        if add.status != 0 {
            let msg = if !add.stderr.is_empty() { &add.stderr } else { &add.stdout };
            return Err(format!("terminal-sandbox: worktree add failed: {}", slice_n(msg, 200)));
        }
        created = true;
    }

    let vibe_sparse = std::env::var("ANTILOKI_VIBE_SPARSE").ok();
    if !folders.is_empty() && vibe_sparse.as_deref() == Some("1") {
        run_git(&["sparse-checkout", "init", "--cone"], &sp, None);
        let mut set_args: Vec<&str> = vec!["sparse-checkout", "set"];
        let folder_refs: Vec<&str> = folders.iter().map(|s| s.as_str()).collect();
        set_args.extend(folder_refs.iter().copied());
        let set = run_git(&set_args, &sp, None);
        if set.status != 0 {
            return Err(format!("terminal-sandbox: sparse-checkout set failed: {}", slice_n(&set.stderr, 200)));
        }
    } else {
        run_git(&["sparse-checkout", "disable"], &sp, None); // no-op if never inited
    }

    // Best-effort scope persistence (see module doc for the cwd-resolution quirk ported as-is).
    let scope_json = serde_json::to_string(&json!({ "allow_folders": folders })).unwrap_or_default();
    let _ = std::fs::write(sp.join(".antiloki-scope.json"), scope_json);
    let excl_out = run_git(&["rev-parse", "--git-path", "info/exclude"], &sp, None);
    let excl_rel = excl_out.stdout.trim().to_string();
    if !excl_rel.is_empty() {
        let excl_path = if Path::new(&excl_rel).is_absolute() {
            PathBuf::from(&excl_rel)
        } else {
            std::env::current_dir().unwrap_or_default().join(&excl_rel)
        };
        let cur = std::fs::read_to_string(&excl_path).unwrap_or_default();
        if !cur.contains(".antiloki-scope.json") {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&excl_path) {
                let _ = f.write_all(b"\n.antiloki-scope.json\n");
            }
        }
    }

    let ls = run_git(&["ls-files"], &sp, None);
    let files = ls.stdout.split('\n').filter(|s| !s.is_empty()).count() as i64;

    let mut out = Map::new();
    out.insert("session_id".into(), session_id);
    out.insert("path".into(), json!(sp_str));
    out.insert("allow_folders".into(), json!(folders));
    out.insert("files".into(), json!(files));
    out.insert("created".into(), json!(created));
    Ok(Value::Object(out))
}

/// `function diff(input, context)` — private (no JS `export`); context is unused in the JS too.
fn diff(input: &Value) -> Result<Value, String> {
    let session_id = input.get("session_id").cloned().unwrap_or(Value::Null);
    let sp_str = sandbox_path_str(&session_id)?;
    let sp = PathBuf::from(&sp_str);
    if !sp.exists() {
        return Ok(json!({
            "session_id": session_id, "exists": false, "patch": "", "files": [], "changed": 0
        }));
    }
    run_git(&["add", "-A"], &sp, None); // include untracked
    let patch = run_git(&["diff", "--cached"], &sp, None).stdout;
    let scope = read_scope(&sp);
    let scope_val = json!(scope);
    let name_status = run_git(&["diff", "--cached", "--name-status"], &sp, None).stdout;

    let mut files: Vec<Value> = Vec::new();
    for line in name_status.split('\n') {
        if line.is_empty() {
            continue;
        }
        // JS: /^(\S+)\s+(.+)$/ — status token, then the rest of the line as path.
        if let Some(pos) = line.find(char::is_whitespace) {
            let status = &line[..pos];
            let rest = line[pos..].trim_start();
            if !rest.is_empty() {
                let in_scope = if scope.is_empty() {
                    true
                } else {
                    let split = crate::cockpit__helpers_scope::splitByScope(&scope_val, &json!([rest]));
                    split
                        .get("in_scope")
                        .and_then(|a| a.as_array())
                        .map(|a| a.len() == 1)
                        .unwrap_or(false)
                };
                files.push(json!({ "status": status, "path": rest, "in_scope": in_scope }));
            }
        }
    }
    let out_of_scope: Vec<Value> = files
        .iter()
        .filter(|f| !f["in_scope"].as_bool().unwrap_or(false))
        .map(|f| f["path"].clone())
        .collect();
    let changed = files.len() as i64;
    Ok(json!({
        "session_id": session_id, "exists": true, "patch": patch, "files": files,
        "changed": changed, "scope": scope_val, "out_of_scope": out_of_scope,
    }))
}

/// `function apply(input, context)` — private (no JS `export`). The gated merge-back.
fn apply(input: &Value, db: &Connection) -> Result<Value, String> {
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default().to_string();
    let session_id = input.get("session_id").cloned().unwrap_or(Value::Null);
    let scope_mode = input.get("scope_mode").and_then(Value::as_str).unwrap_or("block").to_string();
    let sp_str = sandbox_path_str(&session_id)?;
    let sp = PathBuf::from(&sp_str);
    if !sp.exists() {
        return Ok(json!({ "applied": false, "reason": "no sandbox for this session" }));
    }
    run_git(&["add", "-A"], &sp, None);

    let scope = read_scope(&sp);
    let scope_val = json!(scope);
    let mut pathspec: Vec<String> = Vec::new();

    if !scope.is_empty() {
        let names_out = run_git(&["diff", "--cached", "--name-only"], &sp, None).stdout;
        let names: Vec<Value> = names_out.split('\n').filter(|s| !s.is_empty()).map(|s| json!(s)).collect();
        let split = crate::cockpit__helpers_scope::splitByScope(&scope_val, &json!(names));
        let in_scope: Vec<Value> = split.get("in_scope").and_then(|a| a.as_array()).cloned().unwrap_or_default();
        let out_of_scope: Vec<Value> = split.get("out_of_scope").and_then(|a| a.as_array()).cloned().unwrap_or_default();

        if !out_of_scope.is_empty() && scope_mode == "block" {
            let oos_slice: Vec<Value> = out_of_scope.iter().take(40).cloned().collect();
            return Ok(json!({
                "applied": false,
                "reason": "out_of_scope",
                "out_of_scope": oos_slice,
                "scope": scope_val,
                "choices": [
                    "re-run with scope_mode:\"in_scope_only\"",
                    "re-run with scope_mode:\"override\" (operator click)",
                    "discard the out-of-scope edits",
                ],
            }));
        }
        if !out_of_scope.is_empty() && scope_mode == "in_scope_only" {
            if in_scope.is_empty() {
                let oos_slice: Vec<Value> = out_of_scope.iter().take(40).cloned().collect();
                return Ok(json!({
                    "applied": false, "reason": "no in-scope changes to apply", "out_of_scope": oos_slice
                }));
            }
            pathspec.push("--".to_string());
            for v in &in_scope {
                pathspec.push(str_or_empty(v));
            }
        }
    }

    let pathspec_refs: Vec<&str> = pathspec.iter().map(|s| s.as_str()).collect();
    let mut diff_args: Vec<&str> = vec!["diff", "--cached"];
    diff_args.extend(pathspec_refs.iter().copied());
    let patch = run_git(&diff_args, &sp, None).stdout;
    if patch.trim().is_empty() {
        return Ok(json!({ "applied": false, "reason": "no changes to apply" }));
    }

    let check = run_git(&["apply", "--check", "-"], Path::new(&root), Some(&patch)); // GATE
    if check.status != 0 {
        return Ok(json!({
            "applied": false,
            "reason": format!("does not apply cleanly to the real tree: {}", slice_n(&check.stderr, 240)),
        }));
    }
    let ap = run_git(&["apply", "-"], Path::new(&root), Some(&patch)); // never --force
    if ap.status != 0 {
        return Ok(json!({
            "applied": false,
            "reason": format!("apply failed: {}", slice_n(&ap.stderr, 240)),
        }));
    }

    let mut name_only_args: Vec<&str> = vec!["diff", "--cached", "--name-only"];
    name_only_args.extend(pathspec_refs.iter().copied());
    let files_out = run_git(&name_only_args, &sp, None).stdout;
    let files: Vec<Value> = files_out.split('\n').filter(|s| !s.is_empty()).map(|s| json!(s)).collect();
    let count = files.len() as i64;
    Ok(json!({ "applied": true, "files": files, "count": count }))
}

/// `function discard(input, context)` — private (no JS `export`). Teardown.
fn discard(input: &Value, db: &Connection) -> Result<Value, String> {
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default().to_string();
    let session_id = input.get("session_id").cloned().unwrap_or(Value::Null);
    let sp_str = sandbox_path_str(&session_id)?;
    let sp = PathBuf::from(&sp_str);
    if !sp.exists() {
        return Ok(json!({ "removed": false, "reason": "no sandbox" }));
    }
    let r = run_git(&["worktree", "remove", "--force", &sp_str], Path::new(&root), None);
    if r.status != 0 {
        let _ = std::fs::remove_dir_all(&sp);
        run_git(&["worktree", "prune"], Path::new(&root), None);
    }
    Ok(json!({ "removed": true, "path": sp_str }))
}

/// `export default function terminalSandbox(input, context)`.
pub fn terminalSandbox(input: &Value, db: &Connection) -> Result<Value, String> {
    let mode = input.get("mode").and_then(Value::as_str).unwrap_or("");
    match mode {
        "ensure" => ensure(input, db),
        "diff" => diff(input),
        "apply" => apply(input, db),
        "discard" => discard(input, db),
        _ => Err("terminal-sandbox: mode (ensure|diff|apply|discard) is required".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_HOME is process-global; serialize via the shared crate-wide lock
    // (test_env_lock.rs) — a local mutex would not prevent a race with OTHER files' tests over
    // the same real env var (auth-sign.rs / orphan-worktree-dirs.rs also read it).
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
        dir.push(format!("antiloki-terminal-sandbox-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_git_ok(args: &[&str], cwd: &Path) {
        let out = crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    fn make_temp_git_repo(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        run_git_ok(&["init", "-q"], &dir);
        run_git_ok(&["config", "user.email", "test@test"], &dir);
        run_git_ok(&["config", "user.name", "test"], &dir);
        fs::write(dir.join("README.md"), "# test\n").unwrap();
        run_git_ok(&["add", "README.md"], &dir);
        run_git_ok(&["commit", "-q", "-m", "init"], &dir);
        dir
    }

    fn db_with_workspace(root: &Path) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        )
        .unwrap();
        c.execute(
            "INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)",
            rusqlite::params![root.to_string_lossy().into_owned()],
        )
        .unwrap();
        c
    }

    #[test]
    fn sandbox_path_rejects_empty_and_sanitizes() {
        let e = sandbox_path_str(&json!("")).unwrap_err();
        assert_eq!(e, "terminal-sandbox: session_id is required");
        let e2 = sandbox_path_str(&Value::Null).unwrap_err();
        assert_eq!(e2, "terminal-sandbox: session_id is required");

        let p = sandbox_path_str(&json!("abc../\\!! DEF-1_2")).unwrap();
        assert!(p.ends_with("term-abcDEF-1_2"), "{p}");
    }

    #[test]
    fn clean_folders_strips_leading_dot_slash_and_trailing_slash() {
        let f = clean_folders(&json!(["./src/ui/", "../foo//bar\\baz", "", "  "]));
        // Real JS: `^[./]+` strips only the leading ".." + "/" run (stops at 'f'), leaving the
        // internal "//" from "foo//bar" untouched by the split/join (split on a run of one
        // separator still yields an empty segment between two separators, so join re-inserts
        // "//"). Verified via node -e against skills/agents/in/terminal-sandbox.mjs's cleanFolders:
        // cleanFolders(["./src/ui/", "../foo//bar\\baz", "", "  "]) === ["src/ui", "foo//bar/baz"].
        assert_eq!(f, vec!["src/ui".to_string(), "foo//bar/baz".to_string()]);
    }

    #[test]
    fn terminal_sandbox_missing_mode_errs() {
        let c = Connection::open_in_memory().unwrap();
        let e = terminalSandbox(&json!({}), &c).unwrap_err();
        assert_eq!(e, "terminal-sandbox: mode (ensure|diff|apply|discard) is required");
    }

    #[test]
    fn ensure_refuses_non_git_workspace_root() {
        let root = temp_dir("not-a-repo");
        let home = temp_dir("not-a-repo-home");
        let c = db_with_workspace(&root);
        let e = with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "ensure", "session_id": "s1" }), &c).unwrap_err()
        });
        assert!(e.contains("workspace root is not a git repo"), "{e}");
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn diff_and_apply_and_discard_missing_sandbox_are_safe_noops() {
        let root = make_temp_git_repo("noop-root");
        let home = temp_dir("noop-home");
        let c = db_with_workspace(&root);

        let d = with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "diff", "session_id": "no-such-session" }), &c).unwrap()
        });
        assert_eq!(d["exists"], json!(false));
        assert_eq!(d["patch"], json!(""));
        assert_eq!(d["changed"], json!(0));

        let a = with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "apply", "session_id": "no-such-session" }), &c).unwrap()
        });
        assert_eq!(a, json!({ "applied": false, "reason": "no sandbox for this session" }));

        let disc = with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "discard", "session_id": "no-such-session" }), &c).unwrap()
        });
        assert_eq!(disc, json!({ "removed": false, "reason": "no sandbox" }));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn ensure_provisions_a_full_worktree_and_is_idempotent() {
        let root = make_temp_git_repo("ensure-root");
        let home = temp_dir("ensure-home");
        let c = db_with_workspace(&root);

        let r1 = with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "ensure", "session_id": "sess-A!!" }), &c).unwrap()
        });
        assert_eq!(r1["created"], json!(true));
        assert_eq!(r1["session_id"], json!("sess-A!!"));
        let sp = r1["path"].as_str().unwrap().to_string();
        // JS sanitize is `replace(/[^A-Za-z0-9_-]/g, '')` — hyphens survive, only the `!!` is
        // stripped, so "sess-A!!" -> "sess-A" (not "sessA"). Verified against sandboxPath in
        // skills/agents/in/terminal-sandbox.mjs.
        assert!(PathBuf::from(&sp).ends_with("term-sess-A"), "{sp}");
        assert!(PathBuf::from(&sp).join("README.md").exists(), "full checkout, not pruned");
        assert_eq!(r1["files"], json!(1));

        // Re-running ensure reuses the existing worktree (created:false).
        let r2 = with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "ensure", "session_id": "sess-A!!" }), &c).unwrap()
        });
        assert_eq!(r2["created"], json!(false));
        assert_eq!(r2["path"], r1["path"]);

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn diff_apply_discard_round_trip_lands_a_real_change() {
        let root = make_temp_git_repo("roundtrip-root");
        let home = temp_dir("roundtrip-home");
        let c = db_with_workspace(&root);

        // sp must be computed (and the file written) INSIDE with_home: sandbox_path_str reads
        // ANTILOKI_HOME, which with_home clears again on scope exit — computing sp outside would
        // resolve against the real user home instead of this test's tempdir (Os NotFound).
        let sp = with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "ensure", "session_id": "rt1" }), &c).unwrap();
            let sp = sandbox_path_str(&json!("rt1")).unwrap();
            fs::write(PathBuf::from(&sp).join("new-file.txt"), "hello from sandbox\n").unwrap();
            sp
        });

        let d = with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "diff", "session_id": "rt1" }), &c).unwrap()
        });
        assert_eq!(d["exists"], json!(true));
        assert_eq!(d["changed"], json!(1));
        assert!(d["patch"].as_str().unwrap().contains("new-file.txt"));
        assert_eq!(d["files"][0]["status"], json!("A"));
        assert_eq!(d["files"][0]["path"], json!("new-file.txt"));
        assert_eq!(d["files"][0]["in_scope"], json!(true), "empty scope => everything in scope");

        let a = with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "apply", "session_id": "rt1" }), &c).unwrap()
        });
        assert_eq!(a["applied"], json!(true));
        assert_eq!(a["count"], json!(1));
        assert!(root.join("new-file.txt").exists(), "landed on the REAL workspace root");
        // This machine's git has core.autocrlf=true, so `git apply` normalizes the LF this test
        // wrote into CRLF when landing on the real tree — a real git behavior orthogonal to the
        // skill's own logic (both the JS original and this port just shell out to the same git),
        // so compare with CRLF normalized away rather than assuming a byte-exact echo.
        let landed = fs::read_to_string(root.join("new-file.txt")).unwrap();
        assert_eq!(landed.replace("\r\n", "\n"), "hello from sandbox\n");

        let disc = with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "discard", "session_id": "rt1" }), &c).unwrap()
        });
        assert_eq!(disc["removed"], json!(true));
        assert!(!PathBuf::from(&sp).exists(), "worktree dir gone after discard");

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn apply_with_no_staged_changes_reports_no_changes() {
        let root = make_temp_git_repo("emptydiff-root");
        let home = temp_dir("emptydiff-home");
        let c = db_with_workspace(&root);

        with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "ensure", "session_id": "empty1" }), &c).unwrap()
        });
        let a = with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "apply", "session_id": "empty1" }), &c).unwrap()
        });
        assert_eq!(a, json!({ "applied": false, "reason": "no changes to apply" }));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn apply_out_of_scope_block_mode_refuses_with_choices() {
        let root = make_temp_git_repo("scope-root");
        let home = temp_dir("scope-home");
        let c = db_with_workspace(&root);

        // sp must be computed (and the file written) INSIDE with_home — see round-trip test comment.
        with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "ensure", "session_id": "scope1", "allow_folders": ["src"] }), &c).unwrap();
            let sp = sandbox_path_str(&json!("scope1")).unwrap();
            fs::write(PathBuf::from(&sp).join("outside.txt"), "nope\n").unwrap();
        });

        let a = with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "apply", "session_id": "scope1" }), &c).unwrap()
        });
        assert_eq!(a["applied"], json!(false));
        assert_eq!(a["reason"], json!("out_of_scope"));
        assert_eq!(a["out_of_scope"], json!(["outside.txt"]));
        assert_eq!(a["scope"], json!(["src"]));
        assert!(!root.join("outside.txt").exists(), "blocked — nothing landed on the real tree");

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn apply_out_of_scope_in_scope_only_mode_lands_only_the_whitelisted_subset() {
        let root = make_temp_git_repo("scope2-root");
        let home = temp_dir("scope2-home");
        let c = db_with_workspace(&root);

        // sp must be computed (and the files written) INSIDE with_home — see round-trip test comment.
        with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "ensure", "session_id": "scope2", "allow_folders": ["src"] }), &c).unwrap();
            let sp = sandbox_path_str(&json!("scope2")).unwrap();
            fs::create_dir_all(PathBuf::from(&sp).join("src")).unwrap();
            fs::write(PathBuf::from(&sp).join("src").join("in.txt"), "in scope\n").unwrap();
            fs::write(PathBuf::from(&sp).join("outside2.txt"), "out of scope\n").unwrap();
        });

        let a = with_home(home.to_str().unwrap(), || {
            terminalSandbox(&json!({ "mode": "apply", "session_id": "scope2", "scope_mode": "in_scope_only" }), &c).unwrap()
        });
        assert_eq!(a["applied"], json!(true));
        assert_eq!(a["files"], json!(["src/in.txt"]));
        assert!(root.join("src").join("in.txt").exists());
        assert!(!root.join("outside2.txt").exists(), "the out-of-scope file was NOT applied");

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&home);
    }
}
