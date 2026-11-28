//! git-manager-worktree-ops.rs — twin of skills/agents/git-manager/git-manager-worktree-ops.mjs.
//! NAMES ARE CONTRACT: `worktreeAdd`, `worktreeRemove`, `worktreeList` (named exports) +
//! `gitManagerWorktreeOps` (default, mode-dispatch).
//!
//! BESPOKE-NATIVE (playbook's VET rule), same shape as the sibling git-manager-branch-init.rs:
//! `worktreeAdd`/`worktreeList` shell out to `git` unconditionally on every success path, and
//! `worktreeRemove`'s one git-free success path (`!existsSync(path)`) is gated on REAL FILESYSTEM
//! state under `WORKTREES_DIR` (`$ANTILOKI_HOME`-derived, NOT DB-derived — unlike
//! `getWorkspaceRoot`'s `workspaces`-table root, conform-db's fixture format has no seam to
//! control it: no env-var injection, no committed-testdata-dir convention for a HOME-anchored
//! path). None of the three named exports (nor the mode-dispatching default) is fixture-testable
//! via conform-db. Proven instead via `#[cfg(test)]` native tests against a real temp git repo
//! (git-manager-branch-init.rs / god-driver-halt.rs / stack-facts.rs convention) — no fixture
//! JSON, no frontier entry, no dispatch arm for any of the four units.
//!
//! `WORKTREE_ROOT` = `WORKTREES_DIR` (roots.mjs: `join(HOME_DIR, 'worktrees')`, `HOME_DIR` =
//! `$ANTILOKI_HOME` override else `join(homedir(), '.antiloki')`) — inlined here the same way the
//! already-converted `orphan-worktree-dirs.rs`'s `worktrees_dir()` helper does (that module's own
//! doc comment notes THIS twin didn't exist yet at the time it was written; the two now duplicate
//! a few lines rather than one importing the other, keeping this change surgical to the requested
//! file per playbook/project convention).
//!
//! `root` (the git-command cwd for `add`/`list`, and the STATUS-CHECK cwd override inside
//! `remove`'s dirty-check) is DB-derived via the already-converted `engine_workspace_root::
//! getWorkspaceRoot` twin, reused per the playbook's cross-module rule. NOTE the two cwds inside
//! `worktreeRemove` are deliberately DIFFERENT: `git status --porcelain` runs with cwd=`path`
//! (the worktree's own directory), while `git worktree remove` runs with cwd=`root` (the main
//! workspace) — ported faithfully, not collapsed to one.
//!
//! `worktreePathFor(card_slug) = resolve(WORKTREE_ROOT, String(card_slug).toLowerCase())` — a
//! plain absolute-root + relative-slug join (card slugs never contain traversal sequences in
//! real usage), so this twin uses `PathBuf::join` rather than porting Node's full win32
//! `path.resolve` normalizer (that algorithm lives privately in `engine_workspace_root.rs` /
//! `constitution-estimate.rs`, neither of which exports it) — disclosed simplification, same
//! scope as `orphan-worktree-dirs.rs`'s identical `worktrees_dir().join(...)` call.
//!
//! `card_slug`/`branch`/`force` destructuring defaults (`branch = null`, `force = false`) apply
//! ONLY when the key is MISSING from `input`, same "missing-key-only, not `??`" convention as
//! git-manager-branch-init.rs — modeled via `.get(key).cloned().unwrap_or(default)`. The
//! `!card_slug` require-guard is a JS truthy check (any falsy value throws, not just
//! missing/null) — modeled via the shared `truthy()` helper, per the standard `err.required`
//! convention used throughout this codebase.
//!
//! `.slice(0, 200)` / `.slice(9|5|7)` on stderr/stdout/porcelain lines (playbook gotcha #5,
//! UTF-16 code units): the error-message slices ported as `.chars().take(200)` (real `git`
//! output is ASCII/BMP in practice); the `worktree `/`HEAD `/`branch ` prefix slices are exact
//! literal-length strips (`str::strip_prefix`), not general slicing, so no approximation there.
// QUALITY: 78/100 — bespoke-native (unconditional/fs-gated subprocess across all 3 named
// exports), real git calls proven via temp-dir native tests mirroring git-manager-branch-init.rs.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::PathBuf;
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`

const SKILL_ADD: &str = "git-manager-worktree-add";
const SKILL_REMOVE: &str = "git-manager-worktree-remove";
const SKILL_LIST: &str = "git-manager-worktree-list";
const SKILL_OPS: &str = "git-manager-worktree-ops";

struct GitResult { status: Option<i32>, stdout: String, stderr: String }

/// `function git(args, cwd) { return spawnSync('git', args, { cwd, encoding: 'utf8' }); }`
fn git(args: &[&str], cwd: &str) -> GitResult {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) => GitResult {
            status: o.status.code(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Err(_) => GitResult { status: None, stdout: String::new(), stderr: String::new() },
    }
}

fn slice200(s: &str) -> String { s.chars().take(200).collect() }

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
// WORKTREES_DIR = join(HOME_DIR, 'worktrees') — see module doc (duplicated from
// orphan-worktree-dirs.rs's identical private helper).
fn worktrees_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("worktrees")
}

/// `function worktreePathFor(card_slug) { return resolve(WORKTREE_ROOT, String(card_slug).toLowerCase()); }`
fn worktree_path_for(card_slug: &Value) -> PathBuf {
    worktrees_dir().join(js_string(card_slug).to_lowercase())
}

/// THE PROJECT'S NAME AS A FOLDER SAYS IT (operator 2026-11-27: `cockpit-1-1` did not say which project it was): the folder's own name in lower case words and dashes, at most 24 characters; "" when there is none. Pure.
pub fn project_tag(root: &str) -> String {
    let base = root.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("");
    let t: String = base.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let t: String = t.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-").chars().take(24).collect();
    t.trim_end_matches('-').to_string()
}

/// The path `worktreeAdd` WOULD use for a slug — so a caller can look at what is already there
/// (cockpit-pane-add reconciles another workspace's leftover before asking for the worktree).
#[allow(non_snake_case)]
pub fn worktreePathFor(card_slug: &Value) -> PathBuf {
    worktree_path_for(card_slug)
}

fn worktree_slug_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"\.antiloki[/\\]worktrees[/\\]([^/\\]+)").unwrap())
}

/// `export function worktreeAdd(input, context)`
pub fn worktreeAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default().to_string();

    let card_slug_v = input.get("card_slug").cloned();
    if !truthy(card_slug_v.as_ref()) {
        return Err(format!("{SKILL_ADD}: card_slug is required"));
    }
    let card_slug = card_slug_v.unwrap();
    let branch_v = input.get("branch").cloned().unwrap_or(Value::Null);
    let force_v = input.get("force").cloned().unwrap_or(json!(false));

    let path = worktree_path_for(&card_slug);
    let path_str = path.to_string_lossy().into_owned();
    let exists = path.exists();

    if exists && !truthy(Some(&force_v)) {
        return Err(format!(
            "{SKILL_ADD}: worktree already exists for '{}' at {path_str}. Pass force=true to recreate.",
            tmpl(Some(&card_slug))
        ));
    }
    if exists {
        // JS: try { rmSync(path, { recursive: true, force: true }); } catch { /* hopefully git will overwrite */ }
        let _ = std::fs::remove_dir_all(&path);
    }

    let branch_str = if truthy(Some(&branch_v)) { Some(js_string(&branch_v)) } else { None };
    // `from`: cut the new worktree's own branch (named after the slug) from this commit-ish, not from HEAD - a thread's agent starts from the thread's branch (2026-11-11)
    let from_str = input.get("from").and_then(Value::as_str).map(str::trim).filter(|f| !f.is_empty() && !f.starts_with('-')).map(String::from);
    let slug_s = js_string(&card_slug);
    let mut args: Vec<&str> = vec!["worktree", "add"];
    if let Some(f) = &from_str { args.push("-b"); args.push(slug_s.as_str()); args.push(path_str.as_str()); args.push(f.as_str()); } else {
        args.push(path_str.as_str());
        if let Some(b) = &branch_str {
            args.push(b.as_str());
        }
    }
    let r = git(&args, &root);
    if r.status != Some(0) {
        let msg: &str = if !r.stderr.is_empty() { r.stderr.as_str() } else if !r.stdout.is_empty() { r.stdout.as_str() } else { "" };
        return Err(format!("{SKILL_ADD}: git worktree add failed: {}", slice200(msg)));
    }

    Ok(json!({ "card_slug": card_slug, "path": path_str, "branch": branch_v, "created": true }))
}

/// `export function worktreeRemove(input, context)`
pub fn worktreeRemove(input: &Value, db: &Connection) -> Result<Value, String> {
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default().to_string();

    let card_slug_v = input.get("card_slug").cloned();
    if !truthy(card_slug_v.as_ref()) {
        return Err(format!("{SKILL_REMOVE}: card_slug is required"));
    }
    let card_slug = card_slug_v.unwrap();
    let force_v = input.get("force").cloned().unwrap_or(json!(false));

    let path = worktree_path_for(&card_slug);
    let path_str = path.to_string_lossy().into_owned();
    if !path.exists() {
        return Ok(json!({ "card_slug": card_slug, "path": path_str, "removed": false, "reason": "worktree does not exist" }));
    }

    if !truthy(Some(&force_v)) {
        let stat = git(&["status", "--porcelain"], &path_str);
        if !stat.stdout.trim().is_empty() {
            return Err(format!(
                "{SKILL_REMOVE}: worktree '{}' has uncommitted changes. Pass force=true to discard.",
                tmpl(Some(&card_slug))
            ));
        }
    }

    let force_flag = truthy(Some(&force_v));
    let args: Vec<&str> = if force_flag { vec!["worktree", "remove", "--force", path_str.as_str()] } else { vec!["worktree", "remove", path_str.as_str()] };
    let r = git(&args, &root);
    if r.status != Some(0) {
        return Err(format!("{SKILL_REMOVE}: git worktree remove failed: {}", slice200(&r.stderr)));
    }

    Ok(json!({ "card_slug": card_slug, "path": path_str, "removed": true }))
}

/// `export function worktreeList(_input, context)`
pub fn worktreeList(_input: &Value, db: &Connection) -> Result<Value, String> {
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default().to_string();

    let r = git(&["worktree", "list", "--porcelain"], &root);
    if r.status != Some(0) {
        return Err(format!("{SKILL_LIST}: git worktree list failed: {}", slice200(&r.stderr)));
    }

    let mut entries: Vec<Map<String, Value>> = Vec::new();
    let mut cur: Option<Map<String, Value>> = None;
    for line in r.stdout.split('\n') {
        if let Some(rest) = line.strip_prefix("worktree ") {
            if let Some(c) = cur.take() {
                entries.push(c);
            }
            let mut m = Map::new();
            m.insert("path".into(), json!(rest.trim()));
            cur = Some(m);
        } else if let Some(rest) = line.strip_prefix("HEAD ") {
            if let Some(c) = cur.as_mut() {
                c.insert("head".into(), json!(rest.trim()));
            }
        } else if let Some(rest) = line.strip_prefix("branch ") {
            if let Some(c) = cur.as_mut() {
                c.insert("branch".into(), json!(rest.trim()));
            }
        }
    }
    if let Some(c) = cur.take() {
        entries.push(c);
    }

    let re = worktree_slug_re();
    for e in entries.iter_mut() {
        let m = e.get("path").and_then(Value::as_str).and_then(|p| re.captures(p)).and_then(|c| c.get(1)).map(|g| g.as_str().to_string());
        if let Some(slug) = m {
            e.insert("card_slug".into(), json!(slug));
        }
    }

    let count = entries.len() as i64;
    let entries_val: Vec<Value> = entries.into_iter().map(Value::Object).collect();
    Ok(json!({ "entries": entries_val, "count": count }))
}

/// `export default function gitManagerWorktreeOps(input, context)`
pub fn gitManagerWorktreeOps(input: &Value, db: &Connection) -> Result<Value, String> {
    match input.get("mode").and_then(Value::as_str) {
        Some("add") => worktreeAdd(input, db),
        Some("remove") => worktreeRemove(input, db),
        Some("list") => worktreeList(input, db),
        _ => Err(format!("{SKILL_OPS}: mode (add|remove|list) is required")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    // SAFETY: ANTILOKI_HOME is process-global; serialize since `cargo test` runs threads in
    // parallel by default, and this same var is read by other converted twins' tests too
    // (god-driver-halt.rs, orphan-worktree-dirs.rs, ...) — shared crate-wide lock.
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
        dir.push(format!("antiloki-git-worktree-ops-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_git(args: &[&str], cwd: &Path) {
        let out = crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    fn make_temp_git_repo(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        run_git(&["init", "-q", "-b", "master"], &dir);
        run_git(&["config", "user.email", "test@test"], &dir);
        run_git(&["config", "user.name", "test"], &dir);
        fs::write(dir.join("README.md"), "# test\n").unwrap();
        run_git(&["add", "README.md"], &dir);
        run_git(&["commit", "-q", "-m", "init"], &dir);
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
    fn add_card_slug_is_required() {
        let c = Connection::open_in_memory().unwrap(); // no workspaces table needed — errors before any git call
        let e = worktreeAdd(&json!({}), &c).unwrap_err();
        assert_eq!(e, "git-manager-worktree-add: card_slug is required");
    }

    #[test]
    fn remove_card_slug_is_required() {
        let c = Connection::open_in_memory().unwrap();
        let e = worktreeRemove(&json!({}), &c).unwrap_err();
        assert_eq!(e, "git-manager-worktree-remove: card_slug is required");
    }

    #[test]
    fn add_creates_a_new_worktree_default_branch_inferred_from_path() {
        let repo = make_temp_git_repo("add-basic");
        let home = temp_dir("add-basic-home");
        let c = db_with_workspace(&repo);
        let r = with_home(home.to_str().unwrap(), || worktreeAdd(&json!({ "card_slug": "FEAT-X" }), &c).unwrap());
        let expected_path = home.join("worktrees").join("feat-x");
        assert_eq!(
            r,
            json!({
                "card_slug": "FEAT-X",
                "path": expected_path.to_string_lossy().into_owned(),
                "branch": null,
                "created": true,
            })
        );
        assert!(expected_path.join("README.md").exists());
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn add_refuses_when_worktree_already_exists_without_force() {
        let repo = make_temp_git_repo("add-dup");
        let home = temp_dir("add-dup-home");
        let c = db_with_workspace(&repo);
        with_home(home.to_str().unwrap(), || worktreeAdd(&json!({ "card_slug": "DUP-ONE" }), &c).unwrap());
        let e = with_home(home.to_str().unwrap(), || worktreeAdd(&json!({ "card_slug": "DUP-ONE" }), &c).unwrap_err());
        assert!(e.starts_with("git-manager-worktree-add: worktree already exists for 'DUP-ONE' at"), "{e}");
        assert!(e.ends_with("Pass force=true to recreate."), "{e}");
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn add_force_recreates_an_existing_worktree_dir() {
        let repo = make_temp_git_repo("add-force");
        let home = temp_dir("add-force-home");
        let c = db_with_workspace(&repo);
        with_home(home.to_str().unwrap(), || worktreeAdd(&json!({ "card_slug": "FORCE-ONE" }), &c).unwrap());
        // `git worktree remove --force` deletes the directory too (verified against real git),
        // so recreate it bare afterward to simulate a stale/orphaned dir with no git registration
        // — that's the case worktreeAdd's rmSync-then-recreate path needs to handle.
        run_git(&["worktree", "remove", "--force", home.join("worktrees").join("force-one").to_str().unwrap()], &repo);
        fs::create_dir_all(home.join("worktrees").join("force-one")).unwrap();
        fs::write(home.join("worktrees").join("force-one").join("stale.txt"), "stale").unwrap();
        let r = with_home(home.to_str().unwrap(), || worktreeAdd(&json!({ "card_slug": "FORCE-ONE", "force": true }), &c).unwrap());
        assert_eq!(r["created"], json!(true));
        assert!(!home.join("worktrees").join("force-one").join("stale.txt").exists());
        assert!(home.join("worktrees").join("force-one").join("README.md").exists());
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn remove_returns_not_removed_when_worktree_dir_is_absent() {
        let repo = make_temp_git_repo("remove-absent");
        let home = temp_dir("remove-absent-home");
        let c = db_with_workspace(&repo);
        let r = with_home(home.to_str().unwrap(), || worktreeRemove(&json!({ "card_slug": "GHOST" }), &c).unwrap());
        assert_eq!(r["removed"], json!(false));
        assert_eq!(r["reason"], json!("worktree does not exist"));
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn remove_clean_worktree_succeeds_without_force() {
        let repo = make_temp_git_repo("remove-clean");
        let home = temp_dir("remove-clean-home");
        let c = db_with_workspace(&repo);
        with_home(home.to_str().unwrap(), || worktreeAdd(&json!({ "card_slug": "CLEAN-ONE" }), &c).unwrap());
        let r = with_home(home.to_str().unwrap(), || worktreeRemove(&json!({ "card_slug": "CLEAN-ONE" }), &c).unwrap());
        assert_eq!(r["removed"], json!(true));
        assert!(!home.join("worktrees").join("clean-one").exists());
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn remove_dirty_worktree_refuses_without_force() {
        let repo = make_temp_git_repo("remove-dirty");
        let home = temp_dir("remove-dirty-home");
        let c = db_with_workspace(&repo);
        with_home(home.to_str().unwrap(), || worktreeAdd(&json!({ "card_slug": "DIRTY-ONE" }), &c).unwrap());
        fs::write(home.join("worktrees").join("dirty-one").join("untracked.txt"), "x").unwrap();
        let e = with_home(home.to_str().unwrap(), || worktreeRemove(&json!({ "card_slug": "DIRTY-ONE" }), &c).unwrap_err());
        assert_eq!(e, "git-manager-worktree-remove: worktree 'DIRTY-ONE' has uncommitted changes. Pass force=true to discard.");
        assert!(home.join("worktrees").join("dirty-one").exists());
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn remove_dirty_worktree_force_discards_and_removes() {
        let repo = make_temp_git_repo("remove-dirty-force");
        let home = temp_dir("remove-dirty-force-home");
        let c = db_with_workspace(&repo);
        with_home(home.to_str().unwrap(), || worktreeAdd(&json!({ "card_slug": "DIRTY-FORCE" }), &c).unwrap());
        fs::write(home.join("worktrees").join("dirty-force").join("untracked.txt"), "x").unwrap();
        let r = with_home(home.to_str().unwrap(), || worktreeRemove(&json!({ "card_slug": "DIRTY-FORCE", "force": true }), &c).unwrap());
        assert_eq!(r["removed"], json!(true));
        assert!(!home.join("worktrees").join("dirty-force").exists());
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn list_reports_entries_with_card_slug_extracted() {
        let repo = make_temp_git_repo("list-basic");
        // HOME_DIR (roots.mjs) uses $ANTILOKI_HOME VERBATIM (unlike the unoverridden default
        // `join(homedir(), '.antiloki')`) — the override must itself end in `.antiloki` for the
        // `.antiloki/worktrees/<slug>` card_slug-extraction regex to match, same as production.
        let home_base = temp_dir("list-basic-home");
        let home = home_base.join(".antiloki");
        fs::create_dir_all(&home).unwrap();
        let c = db_with_workspace(&repo);
        with_home(home.to_str().unwrap(), || worktreeAdd(&json!({ "card_slug": "LIST-ONE" }), &c).unwrap());
        let r = worktreeList(&json!({}), &c).unwrap();
        assert_eq!(r["count"], json!(2)); // main repo + the added worktree
        let entries = r["entries"].as_array().unwrap();
        let main_entry = entries.iter().find(|e| e["card_slug"].is_null()).unwrap();
        assert_eq!(main_entry["branch"], json!("refs/heads/master"));
        let wt_entry = entries.iter().find(|e| e["card_slug"] == json!("list-one")).unwrap();
        assert_eq!(wt_entry["branch"], json!("refs/heads/list-one"));
        assert!(wt_entry["path"].as_str().unwrap().ends_with("list-one"));
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home_base);
    }

    #[test]
    fn default_dispatches_by_mode_and_requires_a_known_mode() {
        let repo = make_temp_git_repo("dispatch");
        let home = temp_dir("dispatch-home");
        let c = db_with_workspace(&repo);
        let added = with_home(home.to_str().unwrap(), || gitManagerWorktreeOps(&json!({ "mode": "add", "card_slug": "DISPATCH-ONE" }), &c).unwrap());
        assert_eq!(added["created"], json!(true));
        let listed = gitManagerWorktreeOps(&json!({ "mode": "list" }), &c).unwrap();
        assert_eq!(listed["count"], json!(2));
        let removed = with_home(home.to_str().unwrap(), || gitManagerWorktreeOps(&json!({ "mode": "remove", "card_slug": "DISPATCH-ONE" }), &c).unwrap());
        assert_eq!(removed["removed"], json!(true));
        let e = gitManagerWorktreeOps(&json!({}), &c).unwrap_err();
        assert_eq!(e, "git-manager-worktree-ops: mode (add|remove|list) is required");
        let e2 = gitManagerWorktreeOps(&json!({ "mode": "bogus" }), &c).unwrap_err();
        assert_eq!(e2, "git-manager-worktree-ops: mode (add|remove|list) is required");
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }
}
