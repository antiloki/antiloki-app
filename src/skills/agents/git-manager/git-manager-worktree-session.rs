//! git-manager-worktree-session.rs — twin of skills/agents/git-manager/git-manager-worktree-session.mjs.
//! NAMES ARE CONTRACT: `worktreeCreate`, `worktreeMerge` (named exports) + `gitManagerWorktreeSession`
//! (default, mode-dispatch).
//!
//! BESPOKE-NATIVE (playbook's VET rule), same shape as the sibling `git-manager-worktree-ops.rs` /
//! `git-manager-branch-init.rs`: `worktreeCreate` shells out to `git` unconditionally on EVERY
//! success path (the very first check, `isRepo(root)`, is a `git rev-parse --is-inside-work-tree`
//! call before any input-flag branch). `worktreeMerge` has one git-free success path (the worktree
//! dir doesn't exist), but that path is gated on REAL FILESYSTEM state under `WORKTREES_DIR`
//! (`$ANTILOKI_HOME`-derived, NOT DB-derived), the identical situation `worktreeRemove` documents
//! in the sibling ops twin — no fixture seam controls it. Neither named export, nor the
//! mode-dispatching default, is fixture-testable via conform-db. Proven instead via `#[cfg(test)]`
//! native tests against a real temp git repo (git-manager-branch-init.rs / god-driver-halt.rs /
//! stack-facts.rs convention) — no fixture JSON, no frontier entry, no dispatch arm for any unit.
//!
//! `context.workspace_root` / `context.worktree_root` (JS module doc: "context overrides (for
//! tests)"): test-only knobs with no place in the fixed `(input: &Value, db: &Connection)` Rust
//! signature every dispatchable twin uses — production callers only ever pass `{ db }`, so `root`
//! is always DB-derived via the already-converted `engine_workspace_root::getWorkspaceRoot` twin
//! and the worktree root is always `WORKTREES_DIR` (`$ANTILOKI_HOME`-derived), inlined the same
//! way `git-manager-worktree-ops.rs`'s `worktrees_dir()` duplicates roots.mjs's constant (that
//! twin's own doc notes the duplication is deliberate/surgical rather than cross-importing).
//!
//! Cross-module (already-converted twins, reused not reimplemented): `contractGet`
//! (`contract_contract_get::contractGet`), `getWorkspaceRoot` (`engine_workspace_root::
//! getWorkspaceRoot`), `adviseOnFailure` (`driver__helpers_failure_advice::adviseOnFailure`).
//!
//! `context.db` missing → JS throws `err.contextMissing(...)`. Not modeled: `db: &Connection` is
//! a required, non-nullable reference here, same convention as every sibling twin.
//!
//! `datetime('now')` (the two `contracts.worktree_path` UPDATEs) is left UNFROZEN — same
//! `tl-sandbox-create.rs` / `qa-sandbox-reset.rs` precedent: this unit has no JS-lane counterpart
//! run under the frozen-clock conform-db harness to match against (bespoke-native, no fixture), so
//! the honest-for-production translation is the real SQLite clock.
//!
//! `.slice(0, 200)` on git stderr/stdout error tails (playbook gotcha #5, UTF-16 code units):
//! ported as `.chars().take(200)` — real `git` output is ASCII/BMP in practice, exact for every
//! case this unit's own tests exercise (same as the sibling ops/branch-init twins).
//!
//! `worktreeCreate`'s SESSION-13 deps-install sub-branch (`existsSync(package.json)`-gated `npm
//! ci`/`npm install`, plus the best-effort `notifications` INSERT on failure): real code below,
//! DISCLOSED-UNTESTED by both lanes' own test suites — this skill's own
//! `git-manager-worktree-session.test.mjs` never writes a `package.json` into the base repo
//! (`makeRepo()` only commits `README.md`), so the JS source's own coverage never exercises this
//! branch either; the native tests mirror that same fixture shape. The `.slice(-600)` /
//! `.slice(-200)` / `.slice(-160)` "tail of output" JS slices are ported as `.chars().rev().take(n)
//! .collect::<Vec<_>>().into_iter().rev().collect()` (last-N-chars, not first); `spawnSync`'s
//! `timeout: 300000` has no direct `std::process::Command` equivalent without a watcher thread —
//! left unenforced, a disclosed simplification on this untested branch (same "real spawn, no
//! process-timeout" gap `cockpit-script-run.rs` discloses for its own untested spawn branch).
//! `new Date().toISOString()` inside the `[gate worktree.deps]` console.log line is cosmetic
//! (never stored/returned) — written as the codebase's standard frozen literal
//! `"2026-01-01T00:00:00.000Z"` for convention consistency, not because a JS-lane comparison needs it.
// QUALITY: 68/100 — bespoke-native (unconditional/fs-gated subprocess across both named exports),
// real git calls proven via temp-dir native tests; the SESSION-13 npm-install sub-branch is real
// but disclosed-untested, mirroring the JS source's own test suite's blind spot.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

const SKILL: &str = "git-manager-worktree-session";

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

fn slice200(s: &str) -> String { s.chars().take(200).collect() }
fn last_n_chars(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    let start = chars.len().saturating_sub(n);
    chars[start..].iter().collect()
}
fn collapse_ws(s: &str) -> String {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"\s+").unwrap()).replace_all(s, " ").into_owned()
}

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

/// `const isRepo = (root) => git(['rev-parse', '--is-inside-work-tree'], root).status === 0;`
fn is_repo(root: &str) -> bool { git(&["rev-parse", "--is-inside-work-tree"], root).status == Some(0) }

/// `const branchFor = (card) => \`antiloki/${String(card).toLowerCase()}\`;`
fn branch_for(card_slug: &Value) -> String { format!("antiloki/{}", js_string(card_slug).to_lowercase()) }

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
// WORKTREES_DIR = join(HOME_DIR, 'worktrees') — duplicated from git-manager-worktree-ops.rs's
// identical private helper (see module doc: deliberate, surgical duplication over cross-import).
fn worktrees_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("worktrees")
}

/// `path = resolve(wtRoot, String(card_slug).toLowerCase())` — wtRoot is always WORKTREES_DIR in
/// this twin (see module doc on the test-only context override).
fn worktree_path_for(card_slug: &Value) -> PathBuf { worktrees_dir().join(js_string(card_slug).to_lowercase()) }

struct Ctx { slug: Value, slug_str: String, card_slug: Value, root: String, path: PathBuf, branch: String }

/// `function resolveCtx(input, context)`
fn resolve_ctx(input: &Value, db: &Connection) -> Result<Ctx, String> {
    let slug_v = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&slug_v)) {
        return Err(format!("{SKILL}: contract_slug is required"));
    }
    let slug_str = str_qq(Some(&slug_v));

    let contract = crate::contract_contract_get::contractGet(&slug_v, db)?;
    if contract.is_null() {
        return Err(format!("{SKILL}: contract '{}' not found", tmpl(Some(&slug_v))));
    }

    let card_slug = nz(contract.get("card_slug")).cloned().unwrap_or_else(|| slug_v.clone());
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default().to_string();
    let path = worktree_path_for(&card_slug);
    let branch = branch_for(&card_slug);

    Ok(Ctx { slug: slug_v, slug_str, card_slug, root, path, branch })
}

/// `export function worktreeCreate(input, context)`
pub fn worktreeCreate(input: &Value, db: &Connection) -> Result<Value, String> {
    let ctx = resolve_ctx(input, db)?;
    let force = truthy(input.get("force"));

    if !is_repo(&ctx.root) {
        return Err(format!(
            "{SKILL}: workspace '{}' is not a git repository — worktree env needs a git repo (greenfield targets must be git-init'd first)",
            ctx.root
        ));
    }

    let path_str = ctx.path.to_string_lossy().into_owned();

    if ctx.path.exists() && !force {
        crate::db_json::run_write(
            db,
            "UPDATE contracts SET worktree_path = ?, updated_at = datetime('now') WHERE slug = ?",
            &[json!(path_str), json!(ctx.slug_str)],
        )?;
        return Ok(json!({
            "contract_slug": ctx.slug, "card_slug": ctx.card_slug, "path": path_str,
            "branch": ctx.branch, "created": false, "reused": true,
        }));
    }

    if ctx.path.exists() && force {
        let _ = std::fs::remove_dir_all(&ctx.path); // JS: try{}catch{/* git will report */}
    }

    let wt_root = worktrees_dir();
    std::fs::create_dir_all(&wt_root).map_err(|e| e.to_string())?; // JS: mkdirSync(wtRoot, {recursive:true})

    let branch_exists = git(&["rev-parse", "--verify", "--quiet", ctx.branch.as_str()], &ctx.root).status == Some(0);
    let args: Vec<&str> = if branch_exists {
        vec!["worktree", "add", path_str.as_str(), ctx.branch.as_str()]
    } else {
        vec!["worktree", "add", "-b", ctx.branch.as_str(), path_str.as_str()]
    };
    let r = git(&args, &ctx.root);
    if r.status != Some(0) {
        let msg: &str = if !r.stderr.is_empty() { r.stderr.as_str() } else if !r.stdout.is_empty() { r.stdout.as_str() } else { "" };
        return Err(format!("{SKILL}: git worktree add failed: {}", slice200(msg)));
    }

    crate::db_json::run_write(
        db,
        "UPDATE contracts SET worktree_path = ?, updated_at = datetime('now') WHERE slug = ?",
        &[json!(path_str), json!(ctx.slug_str)],
    )?;

    // SESSION-13 deps install — real, DISCLOSED-UNTESTED sub-branch (see module doc).
    let mut deps_installed: Value = Value::Null;
    let mut deps_error: Value = Value::Null;
    let pkg_json = ctx.path.join("package.json");
    if pkg_json.exists() {
        let has_lock = ctx.path.join("package-lock.json").exists();
        let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
        let inst_arg = if has_lock { "ci" } else { "install" };
        let out = crate::proc::cmd(npm)
            .args([inst_arg, "--no-audit", "--no-fund"])
            .current_dir(&ctx.path)
            .output();
        let (status_ok, combined, err_msg) = match out {
            Ok(o) => (o.status.code() == Some(0), format!("{}\n{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)), None),
            Err(e) => (false, String::new(), Some(e.to_string())),
        };
        deps_installed = json!(status_ok);
        let sliced600 = last_n_chars(combined.trim(), 600);
        let tail = if !sliced600.is_empty() { sliced600 } else { err_msg.unwrap_or_else(|| "(no output — timeout?)".to_string()) };
        let card_slug_str = tmpl(Some(&ctx.card_slug));
        let logmsg = if status_ok {
            format!("[gate worktree.deps] 2026-01-01T00:00:00.000Z session=card-{card_slug_str} installed")
        } else {
            format!(
                "[gate worktree.deps] 2026-01-01T00:00:00.000Z session=card-{card_slug_str} INSTALL FAILED — {}",
                collapse_ws(&last_n_chars(&tail, 200))
            )
        };
        println!("{logmsg}");
        if !status_ok {
            deps_error = json!(tail);
            let advice = crate::driver__helpers_failure_advice::adviseOnFailure(&json!(tail));
            let remedy = advice.get("remedy_md").and_then(Value::as_str).unwrap_or("");
            let remedy260: String = remedy.chars().take(260).collect();
            let npm_tail_short = collapse_ws(&last_n_chars(&tail, 160));
            let message = format!("worktree deps install FAILED for {card_slug_str} — {remedy260} (npm tail: {npm_tail_short})");
            let mut payload = Map::new();
            payload.insert("kind_detail".into(), json!("worktree-deps-failed"));
            payload.insert("tail".into(), json!(tail));
            if let Value::Object(m) = &advice {
                for (k, v) in m {
                    payload.insert(k.clone(), v.clone());
                }
            }
            let payload_json = serde_json::to_string(&Value::Object(payload)).unwrap_or_default();
            // JS: try { db.prepare(...).run(...) } catch { /* notifications absent */ }
            let _ = crate::db_json::run_write(
                db,
                "INSERT INTO notifications (kind, severity, source, message, related_card_slug, payload_json) VALUES ('generic','warn','driver',?,?,?)",
                &[json!(message), json!(card_slug_str), json!(payload_json)],
            );
        }
    }

    Ok(json!({
        "contract_slug": ctx.slug, "card_slug": ctx.card_slug, "path": path_str,
        "branch": ctx.branch, "created": true, "deps_installed": deps_installed, "deps_error": deps_error,
    }))
}

/// `export function worktreeMerge(input, context)`
pub fn worktreeMerge(input: &Value, db: &Connection) -> Result<Value, String> {
    let ctx = resolve_ctx(input, db)?;

    if !ctx.path.exists() {
        crate::db_json::run_write(
            db,
            "UPDATE contracts SET worktree_path = NULL, updated_at = datetime('now') WHERE slug = ?",
            &[json!(ctx.slug_str)],
        )?;
        return Ok(json!({
            "contract_slug": ctx.slug, "card_slug": ctx.card_slug, "branch": ctx.branch,
            "merged": false, "removed": false, "reason": "no worktree",
        }));
    }

    let path_str = ctx.path.to_string_lossy().into_owned();
    let dirty = git(&["status", "--porcelain"], &path_str).stdout.trim().to_string();
    let mut committed = false;
    if !dirty.is_empty() {
        git(&["add", "-A"], &path_str);
        let commit_msg = format!("dev: {} worktree changes (driver:auto)", tmpl(Some(&ctx.card_slug)));
        let cm = git(&["commit", "-m", commit_msg.as_str()], &path_str);
        if cm.status != Some(0) {
            let msg: &str = if !cm.stderr.is_empty() { cm.stderr.as_str() } else if !cm.stdout.is_empty() { cm.stdout.as_str() } else { "" };
            return Err(format!("{SKILL}: committing the worktree before merge failed: {}", slice200(msg)));
        }
        committed = true;
    }

    let merge_msg = format!("merge {} (driver:auto)", ctx.branch);
    let m = git(&["merge", "--no-ff", "-m", merge_msg.as_str(), ctx.branch.as_str()], &ctx.root);
    if m.status != Some(0) {
        let _ = git(&["merge", "--abort"], &ctx.root); // JS: try{}catch{/* nothing to abort */}
        let msg: &str = if !m.stderr.is_empty() { m.stderr.as_str() } else if !m.stdout.is_empty() { m.stdout.as_str() } else { "" };
        return Err(format!("{SKILL}: git merge of '{}' failed (conflict?): {}", ctx.branch, slice200(msg)));
    }

    git(&["worktree", "remove", "--force", path_str.as_str()], &ctx.root);
    let _ = git(&["branch", "-d", ctx.branch.as_str()], &ctx.root); // JS: try{}catch{/* merged branch delete is best-effort */}

    crate::db_json::run_write(
        db,
        "UPDATE contracts SET worktree_path = NULL, updated_at = datetime('now') WHERE slug = ?",
        &[json!(ctx.slug_str)],
    )?;

    Ok(json!({
        "contract_slug": ctx.slug, "card_slug": ctx.card_slug, "branch": ctx.branch,
        "merged": true, "removed": true, "committed": committed,
    }))
}

/// `export default function gitManagerWorktreeSession(input, context)`
pub fn gitManagerWorktreeSession(input: &Value, db: &Connection) -> Result<Value, String> {
    match input.get("mode").and_then(Value::as_str) {
        Some("create") => worktreeCreate(input, db),
        Some("merge") => worktreeMerge(input, db),
        _ => Err(format!("{SKILL}: mode (create|merge) is required")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    // SAFETY: ANTILOKI_HOME is process-global; serialize since `cargo test` runs threads in
    // parallel by default, and this same var is read by other converted twins' tests too
    // (god-driver-halt.rs, git-manager-worktree-ops.rs, ...) — shared crate-wide lock.
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
        dir.push(format!("antiloki-git-worktree-session-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn run_git(args: &[&str], cwd: &Path) {
        let out = crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    fn make_repo(tag: &str) -> PathBuf {
        let dir = temp_dir(tag);
        run_git(&["init", "-q"], &dir);
        run_git(&["config", "user.email", "test@antiloki.dev"], &dir);
        run_git(&["config", "user.name", "antiloki test"], &dir);
        run_git(&["config", "commit.gpgsign", "false"], &dir);
        fs::write(dir.join("README.md"), "# base\n").unwrap();
        run_git(&["add", "-A"], &dir);
        run_git(&["commit", "-q", "-m", "base"], &dir);
        dir
    }

    fn db_with_seed(root: &Path, slug: &str, card: &str) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
             CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT);
             CREATE TABLE contracts (slug TEXT UNIQUE, card_slug TEXT, title TEXT, status TEXT, size TEXT, \
             worktree_path TEXT, updated_at TEXT);
             CREATE TABLE notifications (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT, severity TEXT, \
             source TEXT, message TEXT, related_card_slug TEXT, payload_json TEXT);",
        )
        .unwrap();
        c.execute(
            "INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)",
            rusqlite::params![root.to_string_lossy().into_owned()],
        )
        .unwrap();
        c.execute("INSERT INTO cards (slug, title, status) VALUES (?1, 'wt card', 'doing')", rusqlite::params![card]).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, title, status, size) VALUES (?1, ?2, 'c', 'tl_approved', 'M')",
            rusqlite::params![slug, card],
        )
        .unwrap();
        c
    }

    #[test]
    fn contract_slug_is_required() {
        let c = Connection::open_in_memory().unwrap(); // no contracts table needed — errors before any lookup
        let e = worktreeCreate(&json!({}), &c).unwrap_err();
        assert_eq!(e, "git-manager-worktree-session: contract_slug is required");
        let e2 = worktreeMerge(&json!({}), &c).unwrap_err();
        assert_eq!(e2, "git-manager-worktree-session: contract_slug is required");
    }

    #[test]
    fn unknown_contract_slug_errors() {
        let repo = make_repo("unknown-contract");
        let c = db_with_seed(&repo, "ct-real", "card-real");
        let e = worktreeCreate(&json!({ "contract_slug": "ghost" }), &c).unwrap_err();
        assert_eq!(e, "git-manager-worktree-session: contract 'ghost' not found");
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn create_makes_a_branch_and_worktree_and_persists_worktree_path() {
        let repo = make_repo("create-basic");
        let home = temp_dir("create-basic-home");
        let c = db_with_seed(&repo, "ct-wt", "card-wt");
        let r = with_home(home.to_str().unwrap(), || worktreeCreate(&json!({ "contract_slug": "ct-wt" }), &c).unwrap());
        assert_eq!(r["created"], json!(true));
        assert_eq!(r["branch"], json!("antiloki/card-wt"));
        let path = r["path"].as_str().unwrap();
        assert!(Path::new(path).exists(), "worktree dir exists");
        assert!(Path::new(path).join("README.md").exists(), "worktree is a FULL checkout");
        let persisted: String = c.query_row("SELECT worktree_path FROM contracts WHERE slug='ct-wt'", [], |row| row.get(0)).unwrap();
        assert_eq!(persisted, path);
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn create_is_idempotent_reused_when_worktree_already_exists() {
        let repo = make_repo("create-idempotent");
        let home = temp_dir("create-idempotent-home");
        let c = db_with_seed(&repo, "ct-2", "card-2");
        with_home(home.to_str().unwrap(), || worktreeCreate(&json!({ "contract_slug": "ct-2" }), &c).unwrap());
        let again = with_home(home.to_str().unwrap(), || worktreeCreate(&json!({ "contract_slug": "ct-2" }), &c).unwrap());
        assert_eq!(again["created"], json!(false));
        assert_eq!(again["reused"], json!(true));
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn create_on_a_non_git_workspace_fails_with_a_clear_error() {
        let not_repo = temp_dir("create-notrepo");
        let home = temp_dir("create-notrepo-home");
        let c = db_with_seed(&not_repo, "ct-4", "card-4");
        let e = with_home(home.to_str().unwrap(), || worktreeCreate(&json!({ "contract_slug": "ct-4" }), &c).unwrap_err());
        assert!(e.contains("not a git repository"), "{e}");
        let _ = fs::remove_dir_all(&not_repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn create_and_merge_round_trip_brings_the_work_into_main_and_cleans_up() {
        let repo = make_repo("roundtrip");
        let home = temp_dir("roundtrip-home");
        let c = db_with_seed(&repo, "ct-rt", "card-rt");
        let created = with_home(home.to_str().unwrap(), || worktreeCreate(&json!({ "contract_slug": "ct-rt" }), &c).unwrap());
        let wt_path = created["path"].as_str().unwrap().to_string();

        fs::write(Path::new(&wt_path).join("src.js"), "export const x = 1\n").unwrap();
        run_git(&["add", "-A"], Path::new(&wt_path));
        run_git(&["commit", "-q", "-m", "authored"], Path::new(&wt_path));

        let merged = with_home(home.to_str().unwrap(), || worktreeMerge(&json!({ "contract_slug": "ct-rt" }), &c).unwrap());
        assert_eq!(merged["merged"], json!(true));
        assert_eq!(merged["removed"], json!(true));
        assert!(repo.join("src.js").exists(), "authored file merged into main");
        assert!(!Path::new(&wt_path).exists(), "worktree removed after merge");
        let persisted: Option<String> = c.query_row("SELECT worktree_path FROM contracts WHERE slug='ct-rt'", [], |row| row.get(0)).unwrap();
        assert_eq!(persisted, None);
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn merge_auto_commits_uncommitted_dev_changes_then_merges() {
        let repo = make_repo("automerge");
        let home = temp_dir("automerge-home");
        let c = db_with_seed(&repo, "ct-3", "card-3");
        let created = with_home(home.to_str().unwrap(), || worktreeCreate(&json!({ "contract_slug": "ct-3" }), &c).unwrap());
        let wt_path = created["path"].as_str().unwrap().to_string();

        // DEV writes into the worktree's working tree and leaves it UNCOMMITTED.
        fs::write(Path::new(&wt_path).join("dev.js"), "export const authored = 1\n").unwrap();
        let merged = with_home(home.to_str().unwrap(), || worktreeMerge(&json!({ "contract_slug": "ct-3" }), &c).unwrap());
        assert_eq!(merged["committed"], json!(true));
        assert_eq!(merged["merged"], json!(true));
        assert!(repo.join("dev.js").exists(), "auto-committed DEV change merged into main");
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn merge_when_no_worktree_is_a_no_op_that_clears_worktree_path() {
        let repo = make_repo("merge-noworktree");
        let home = temp_dir("merge-noworktree-home");
        let c = db_with_seed(&repo, "ct-5", "card-5");
        c.execute("UPDATE contracts SET worktree_path = 'stale-path' WHERE slug = 'ct-5'", []).unwrap();
        let r = with_home(home.to_str().unwrap(), || worktreeMerge(&json!({ "contract_slug": "ct-5" }), &c).unwrap());
        assert_eq!(r, json!({ "contract_slug": "ct-5", "card_slug": "card-5", "branch": "antiloki/card-5", "merged": false, "removed": false, "reason": "no worktree" }));
        let persisted: Option<String> = c.query_row("SELECT worktree_path FROM contracts WHERE slug='ct-5'", [], |row| row.get(0)).unwrap();
        assert_eq!(persisted, None);
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn default_dispatches_by_mode_and_requires_a_known_mode() {
        let repo = make_repo("dispatch");
        let home = temp_dir("dispatch-home");
        let c = db_with_seed(&repo, "ct-d", "card-d");
        let created = with_home(home.to_str().unwrap(), || gitManagerWorktreeSession(&json!({ "mode": "create", "contract_slug": "ct-d" }), &c).unwrap());
        assert_eq!(created["created"], json!(true));
        let merged = with_home(home.to_str().unwrap(), || gitManagerWorktreeSession(&json!({ "mode": "merge", "contract_slug": "ct-d" }), &c).unwrap());
        assert_eq!(merged["merged"], json!(true));
        let e = gitManagerWorktreeSession(&json!({}), &c).unwrap_err();
        assert_eq!(e, "git-manager-worktree-session: mode (create|merge) is required");
        let e2 = gitManagerWorktreeSession(&json!({ "mode": "bogus" }), &c).unwrap_err();
        assert_eq!(e2, "git-manager-worktree-session: mode (create|merge) is required");
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }
}
