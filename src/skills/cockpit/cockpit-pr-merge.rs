//! cockpit-pr-merge.rs — twin of skills/cockpit/cockpit-pr-merge.mjs (async default → sync twin).
//! NAMES ARE CONTRACT: `cockpitPrMerge`. EPIC-VIBE-COCKPIT (pull requests). Forward-merges a PR's
//! branch into its base SAFELY via git plumbing (`merge-tree --write-tree` → `commit-tree` →
//! `update-ref`, CAS'd against the old base tip) so the possibly-dirty main working tree is NEVER
//! checked out. Refuses on conflict. A `source: 'driver'` PR instead delegates to the real worktree
//! merge (`worktreeMerge`) so the driver's close chain resumes.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: EVERY non-trivial path shells out to real `git`
//! against the real workspace-root repo (`merge-tree`/`rev-parse`/`commit-tree`/`update-ref` for
//! the plumbing path, or the already-bespoke-native `worktreeMerge` — see
//! `git-manager-worktree-session.rs`'s own module doc — for the driver path). Both classes of
//! subprocess produce real git objects/refs as a side effect and depend on actual repo state, which
//! isn't deterministically fixture-controllable the way a seeded table is (same precedent as
//! `cockpit-pr-list.rs`'s `git_commits_for_pr`, `git-manager-worktree-session.rs`,
//! `git-manager-worktree-ops.rs`). Proven instead via `#[cfg(test)]` native tests against real temp
//! git repos.
//!
//! Cross-module (already-converted twins, reused not reimplemented): `ensureCockpitSchema`
//! (`cockpit_cockpit_schema::ensureCockpitSchema`), `getWorkspaceRoot`
//! (`engine_workspace_root::getWorkspaceRoot`), `worktreeMerge`
//! (`agents_git_manager_git_manager_worktree_session::worktreeMerge`).
//!
//! `context.db` missing → JS throws `err.contextMissing(...)`. Not modeled: `db: &Connection` is a
//! required, non-nullable reference here, same "guard-throws are not exercised" convention as every
//! sibling twin.
//!
//! `getWorkspaceRoot` never actually throws in the Rust twin (it always falls back to a constant
//! `SUBSTRATE_ROOT`), so the JS `try { root = getWorkspaceRoot(db) } catch { root = null }` guard
//! and the resulting `if (!root) return { ok:false, error:'no workspace root' }` branch are
//! effectively unreachable here too — transcribed faithfully (`if root.is_empty()`) but not
//! exercised by a native test, matching the sibling twins' documented convention for unreachable
//! guards.
//!
//! `e?.cause ?? e?.message ?? e` (the driver-branch catch): every error `worktreeMerge` can throw in
//! JS sets an explicit `cause` (via `err.notFound`/`new AntilokiError({cause: ...})`), so `e.cause`
//! — the RAW reason text with no `"<skill>: "` prefix — always wins over `e.message`. Our Rust
//! `worktreeMerge` returns `Err("git-manager-worktree-session: <cause>")` uniformly (see that twin's
//! own `Err(format!("{SKILL}: ..."))` call sites), so this twin recovers the JS-visible `cause` by
//! stripping that fixed prefix before formatting `worktree merge failed: <cause>.slice(0,300)`. A
//! failure of the immediately-following `UPDATE cockpit_pull_requests` (inside the SAME JS `try`)
//! would instead surface via `e.message` (no AntilokiError, no `.cause`) — modeled the same way,
//! since our `db_json::run_write` error strings carry no skill prefix to strip.
//!
//! `datetime('now')` in the `UPDATE ... SET status = 'merged', updated_at = datetime('now')` calls
//! is left UNFROZEN — same `tl-sandbox-create.rs`/`qa-sandbox-reset.rs`/
//! `git-manager-worktree-session.rs` precedent: this unit has no JS-lane counterpart run under the
//! frozen-clock conform-db harness to match against (bespoke-native, no fixture).
//!
//! `id` is a JS `Number(input?.id)`; PR ids are SQLite `INTEGER PRIMARY KEY` in every real caller,
//! so `num_to_string`/`id_value` below only bother with the integer-valued fast path (matching
//! `cockpit-pr-list.rs`'s `id_param`) plus a plain float fallback — never exercised, since a
//! fractional PR id cannot exist in production.
// QUALITY: 74/100 — every git-touching branch real, proven via temp-repo native tests; the
// unreachable getWorkspaceRoot-failure guard is disclosed, not tested (same as sibling twins).

use rusqlite::{Connection};
use serde_json::{json, Map, Value};

const SKILL: &str = "cockpit-pr-merge";

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// JS binds Number(x): an integer-valued number binds as SQLite INTEGER, otherwise REAL — same
// convention as cockpit-pr-list.rs's id_param / cockpit-session-get.rs's id_param.
fn id_value(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 { json!(n as i64) } else { json!(n) }
}

// `${id}` for the "PR {id} not found" template literal. See module doc: PR ids are always plain
// ints in production, so this only bothers with the integer fast path + a plain float fallback.
fn num_to_string(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 { (n as i64).to_string() } else { format!("{n}") }
}

fn slice(s: &str, n: usize) -> String { s.chars().take(n).collect() }

struct GitResult { status: Option<i32>, stdout: String, stderr: String }

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

fn err_out(msg: String) -> Value {
    let mut m = Map::new();
    m.insert("ok".into(), json!(false));
    m.insert("error".into(), json!(msg));
    Value::Object(m)
}

/// `export default async function cockpitPrMerge(input, context)`
#[allow(non_snake_case)]
pub fn cockpitPrMerge(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let id_num = js_number(input.get("id"));
    if !(id_num != 0.0 && !id_num.is_nan()) {
        return Err(format!("{SKILL}: id is required"));
    }
    let id_val = id_value(id_num);

    let pr_rows = crate::db_json::query_rows(db, "SELECT * FROM cockpit_pull_requests WHERE id = ?", &[id_val.clone()])?;
    let pr = match pr_rows.as_array().and_then(|a| a.first()).and_then(Value::as_object) {
        Some(m) => m.clone(),
        None => return Ok(err_out(format!("PR {} not found", num_to_string(id_num)))),
    };

    if pr.get("status").and_then(Value::as_str) == Some("merged") {
        let mut m = Map::new();
        m.insert("ok".into(), json!(true));
        m.insert("merged".into(), id_val);
        m.insert("note".into(), json!("already merged"));
        return Ok(Value::Object(m));
    }

    // FEAT-DRIVER-PR-GATE: a driver PR is a card's live WORKTREE — the merge must be the real
    // worktree merge (see module doc / git-manager-worktree-session.rs) so the driver's close
    // chain resumes on the next ticks.
    if pr.get("source").and_then(Value::as_str) == Some("driver") && truthy(pr.get("ref_slug")) {
        let ref_slug = pr.get("ref_slug").cloned().unwrap_or(Value::Null);
        let driver_result: Result<Value, String> = (|| -> Result<Value, String> {
            let r = crate::agents_git_manager_git_manager_worktree_session::worktreeMerge(
                &json!({ "contract_slug": ref_slug }),
                db,
            )
            .map_err(|e| e.strip_prefix(&format!("{}: ", "git-manager-worktree-session")).map(|s| s.to_string()).unwrap_or(e))?;

            crate::db_json::run_write(
                db,
                "UPDATE cockpit_pull_requests SET status = 'merged', updated_at = datetime('now') WHERE id = ?",
                &[id_val.clone()],
            )?;

            let mut out = Map::new();
            out.insert("ok".into(), json!(true));
            out.insert("merged".into(), id_val.clone());
            out.insert("driver".into(), json!(true));
            if let Value::Object(rm) = r {
                for (k, v) in rm {
                    out.insert(k, v);
                }
            }
            Ok(Value::Object(out))
        })();

        return match driver_result {
            Ok(v) => Ok(v),
            Err(cause) => Ok(err_out(format!("worktree merge failed: {}", slice(&cause, 300)))),
        };
    }

    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or("");
    if root.is_empty() {
        return Ok(err_out("no workspace root".to_string()));
    }

    let base_branch = str_qq(pr.get("base_branch"));
    let branch = str_qq(pr.get("branch"));

    // 1. compute the merged tree (refuse on conflict) — touches no working tree
    let mt = git(&["merge-tree", "--write-tree", &base_branch, &branch], root);
    if mt.status == Some(1) {
        let lines: Vec<&str> = mt.stdout.split('\n').collect();
        let b = lines.iter().position(|l| l.is_empty());
        let conflicts: Vec<Value> = match b {
            Some(bi) => lines[bi + 1..].iter().filter(|l| !l.is_empty()).map(|l| json!(*l)).collect(),
            None => Vec::new(),
        };
        let mut m = Map::new();
        m.insert("ok".into(), json!(false));
        m.insert("error".into(), json!("PR has conflicts — resolve first"));
        m.insert("conflicts".into(), json!(conflicts));
        return Ok(Value::Object(m));
    }
    if mt.status != Some(0) {
        return Ok(err_out(format!("merge-tree unavailable: {}", slice(&mt.stderr, 120))));
    }
    let tree = mt.stdout.split('\n').next().unwrap_or("").trim().to_string();
    if tree.is_empty() {
        return Ok(err_out("no merged tree produced".to_string()));
    }

    // 2. create a merge commit with base + branch as parents
    let base_rev = git(&["rev-parse", &base_branch], root).stdout.trim().to_string();
    let branch_rev = git(&["rev-parse", &branch], root).stdout.trim().to_string();
    if base_rev.is_empty() || branch_rev.is_empty() {
        return Ok(err_out("cannot resolve base/branch revs".to_string()));
    }
    let subject = if truthy(pr.get("title")) { tmpl(pr.get("title")) } else { branch.clone() };
    let commit_msg = format!("Merge PR: {subject} ({branch} \u{2192} {base_branch})");
    let commit = git(
        &["commit-tree", &tree, "-p", &base_rev, "-p", &branch_rev, "-m", &commit_msg],
        root,
    )
    .stdout
    .trim()
    .to_string();
    if commit.is_empty() {
        return Ok(err_out("commit-tree failed".to_string()));
    }

    // 3. advance the base ref (CAS against the old value — never force)
    let upd = git(&["update-ref", &format!("refs/heads/{base_branch}"), &commit, &base_rev], root);
    if upd.status != Some(0) {
        return Ok(err_out(format!("update-ref failed (base moved?): {}", slice(&upd.stderr, 120))));
    }

    crate::db_json::run_write(
        db,
        "UPDATE cockpit_pull_requests SET status = 'merged', updated_at = datetime('now') WHERE id = ?",
        &[id_val.clone()],
    )?;
    if truthy(pr.get("pane_id")) {
        crate::db_json::run_write(
            db,
            "UPDATE cockpit_panes SET status = 'merged' WHERE id = ?",
            &[pr.get("pane_id").cloned().unwrap_or(Value::Null)],
        )?;
    }
    let bb_msg = format!("\u{2713} merged PR {branch} \u{2192} {base_branch} ({})", slice(&commit, 8));
    let _ = crate::db_json::run_write(
        db,
        "INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (?, NULL, ?, ?)",
        &[pr.get("session_id").cloned().unwrap_or(Value::Null), json!("decision"), json!(bb_msg)],
    );

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("merged".into(), id_val);
    out.insert("commit".into(), json!(commit));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    // SAFETY: ANTILOKI_HOME is process-global; serialize since `cargo test` runs threads in
    // parallel by default and the driver path (via worktreeMerge) reads it — same shared
    // crate-wide lock convention as auth-sign.rs / git-manager-worktree-session.rs.
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
        dir.push(format!("antiloki-cockpit-pr-merge-rs-{tag}-{}", std::process::id()));
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
        run_git(&["branch", "-M", "main"], &dir);
        dir
    }

    fn db_with_schema() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
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
        c
    }

    fn seed_workspace(c: &Connection, root: &Path) {
        c.execute(
            "INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)",
            rusqlite::params![root.to_string_lossy().into_owned()],
        )
        .unwrap();
    }

    fn insert_pr(c: &Connection, id: i64, source: &str, ref_slug: Option<&str>, branch: &str, base_branch: &str, title: Option<&str>, pane_id: Option<i64>, session_id: i64, status: &str) {
        c.execute(
            "INSERT INTO cockpit_pull_requests (id, session_id, pane_id, source, ref_slug, branch, base_branch, title, status) VALUES (?,?,?,?,?,?,?,?,?)",
            rusqlite::params![id, session_id, pane_id, source, ref_slug, branch, base_branch, title, status],
        )
        .unwrap();
    }

    #[test]
    fn missing_id_is_required() {
        let c = db_with_schema();
        let e = cockpitPrMerge(&json!({}), &c).unwrap_err();
        assert_eq!(e, "cockpit-pr-merge: id is required");
    }

    #[test]
    fn zero_id_is_falsy_and_required() {
        let c = db_with_schema();
        let e = cockpitPrMerge(&json!({ "id": 0 }), &c).unwrap_err();
        assert_eq!(e, "cockpit-pr-merge: id is required");
    }

    #[test]
    fn pr_not_found() {
        let c = db_with_schema();
        let r = cockpitPrMerge(&json!({ "id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "PR 999 not found" }));
    }

    #[test]
    fn already_merged_short_circuits() {
        let c = db_with_schema();
        insert_pr(&c, 1, "vibe", None, "b1", "main", None, None, 10, "merged");
        let r = cockpitPrMerge(&json!({ "id": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "merged": 1, "note": "already merged" }));
    }

    #[test]
    fn driver_pr_merges_the_real_worktree_and_marks_merged() {
        let repo = make_repo("driver-ok");
        let home = temp_dir("driver-ok-home");
        let c = db_with_schema();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('card-1','wt card','doing')", []).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, title, status, size) VALUES ('ct-1','card-1','c','tl_approved','M')",
            [],
        )
        .unwrap();
        seed_workspace(&c, &repo);

        // author real work in the driver's worktree before merging (mirrors the round-trip test
        // in git-manager-worktree-session.rs).
        let created = with_home(home.to_str().unwrap(), || {
            crate::agents_git_manager_git_manager_worktree_session::worktreeCreate(&json!({ "contract_slug": "ct-1" }), &c).unwrap()
        });
        let wt_path = created["path"].as_str().unwrap().to_string();
        fs::write(Path::new(&wt_path).join("src.js"), "export const x = 1\n").unwrap();
        run_git(&["add", "-A"], Path::new(&wt_path));
        run_git(&["commit", "-q", "-m", "authored"], Path::new(&wt_path));

        insert_pr(&c, 1, "driver", Some("ct-1"), "irrelevant", "irrelevant", None, None, 10, "open");

        let r = with_home(home.to_str().unwrap(), || cockpitPrMerge(&json!({ "id": 1 }), &c).unwrap());
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["driver"], json!(true));
        assert_eq!(r["merged"], json!(true)); // spread overwrite: worktreeMerge's own "merged" boolean wins over the outer id
        assert_eq!(r["removed"], json!(true));
        assert!(repo.join("src.js").exists(), "authored file merged into main");
        let status: String = c.query_row("SELECT status FROM cockpit_pull_requests WHERE id=1", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "merged");

        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn driver_pr_merge_failure_reports_the_bare_cause_not_the_prefixed_message() {
        let c = db_with_schema();
        // no contracts row for 'ghost' -> worktreeMerge throws err.notFound('contract','ghost')
        // whose e.cause is "contract 'ghost' not found" (no "git-manager-worktree-session: " prefix).
        insert_pr(&c, 1, "driver", Some("ghost"), "irrelevant", "irrelevant", None, None, 10, "open");
        let r = cockpitPrMerge(&json!({ "id": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "worktree merge failed: contract 'ghost' not found" }));
        let status: String = c.query_row("SELECT status FROM cockpit_pull_requests WHERE id=1", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "open", "failed merge does not flip status");
    }

    #[test]
    fn plumbing_merge_cleanly_advances_base_and_marks_merged_and_posts_blackboard() {
        let repo = make_repo("plumbing-ok");
        let c = db_with_schema();
        seed_workspace(&c, &repo);

        run_git(&["checkout", "-b", "feature"], &repo);
        fs::write(repo.join("feature.txt"), "new file\n").unwrap();
        run_git(&["add", "-A"], &repo);
        run_git(&["commit", "-q", "-m", "feature work"], &repo);
        run_git(&["checkout", "main"], &repo);

        insert_pr(&c, 7, "vibe", None, "feature", "main", Some("My Feature"), Some(3), 10, "open");
        c.execute(
            "INSERT INTO cockpit_panes (id, session_id, idx, status) VALUES (3, 10, 0, 'working')",
            [],
        )
        .unwrap();

        let r = cockpitPrMerge(&json!({ "id": 7 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["merged"], json!(7));
        let commit = r["commit"].as_str().unwrap().to_string();
        assert_eq!(commit.len(), 40, "full sha");

        // base ref actually advanced to the new merge commit
        let head = run_git_capture(&["rev-parse", "main"], &repo);
        assert_eq!(head, commit);
        // Real git behavior (verified against a standalone temp repo): `update-ref` only moves
        // the ref — it never touches the working tree/index (that's the whole point of the
        // plumbing path per the module doc: "the possibly-dirty main working tree is NEVER
        // checked out"). Both JS and this port shell out to the exact same `git update-ref`
        // call with no follow-up `checkout`/`reset --hard`, so `feature.txt` correctly stays
        // absent from the working directory; assert it landed in the merged TREE instead.
        let tree_entry = run_git_capture(&["ls-tree", "-r", "--name-only", &commit], &repo);
        assert!(tree_entry.lines().any(|l| l == "feature.txt"), "merged tree contains feature.txt: {tree_entry}");

        let status: String = c.query_row("SELECT status FROM cockpit_pull_requests WHERE id=7", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "merged");
        let pane_status: String = c.query_row("SELECT status FROM cockpit_panes WHERE id=3", [], |row| row.get(0)).unwrap();
        assert_eq!(pane_status, "merged");

        let bb: String = c.query_row("SELECT body_md FROM cockpit_blackboard WHERE session_id=10", [], |row| row.get(0)).unwrap();
        assert!(bb.starts_with("\u{2713} merged PR feature \u{2192} main ("), "{bb}");
        assert!(bb.contains(&commit[..8]));

        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn plumbing_merge_with_conflict_is_refused_and_lists_conflicting_paths() {
        let repo = make_repo("plumbing-conflict");
        let c = db_with_schema();
        seed_workspace(&c, &repo);

        fs::write(repo.join("conflict.txt"), "line1\n").unwrap();
        run_git(&["add", "-A"], &repo);
        run_git(&["commit", "-q", "-m", "add conflict.txt"], &repo);

        run_git(&["checkout", "-b", "feature"], &repo);
        fs::write(repo.join("conflict.txt"), "feature-line\n").unwrap();
        run_git(&["add", "-A"], &repo);
        run_git(&["commit", "-q", "-m", "feature edit"], &repo);

        run_git(&["checkout", "main"], &repo);
        fs::write(repo.join("conflict.txt"), "main-line\n").unwrap();
        run_git(&["add", "-A"], &repo);
        run_git(&["commit", "-q", "-m", "main edit"], &repo);

        insert_pr(&c, 9, "vibe", None, "feature", "main", None, None, 10, "open");

        let r = cockpitPrMerge(&json!({ "id": 9 }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("PR has conflicts — resolve first"));
        let conflicts = r["conflicts"].as_array().unwrap();
        assert!(!conflicts.is_empty());

        let status: String = c.query_row("SELECT status FROM cockpit_pull_requests WHERE id=9", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "open", "refused merge does not flip status");

        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn plumbing_merge_falls_back_to_branch_name_when_title_is_absent() {
        let repo = make_repo("plumbing-notitle");
        let c = db_with_schema();
        seed_workspace(&c, &repo);
        run_git(&["checkout", "-b", "feature2"], &repo);
        fs::write(repo.join("f2.txt"), "x\n").unwrap();
        run_git(&["add", "-A"], &repo);
        run_git(&["commit", "-q", "-m", "f2"], &repo);
        run_git(&["checkout", "main"], &repo);

        insert_pr(&c, 3, "vibe", None, "feature2", "main", None, None, 10, "open");
        let r = cockpitPrMerge(&json!({ "id": 3 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));

        let commit = r["commit"].as_str().unwrap();
        let subject = run_git_capture(&["log", "-1", "--pretty=%s", commit], &repo);
        assert_eq!(subject, "Merge PR: feature2 (feature2 \u{2192} main)");

        let _ = fs::remove_dir_all(&repo);
    }

    fn run_git_capture(args: &[&str], cwd: &Path) -> String {
        let out = crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}
