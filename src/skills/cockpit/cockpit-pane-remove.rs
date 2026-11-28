//! cockpit-pane-remove.rs — twin of skills/cockpit/cockpit-pane-remove.mjs. NAMES ARE CONTRACT:
//! `cockpitPaneRemove` (default). EPIC-VIBE-COCKPIT + WORKTREE LIFECYCLE: delete a cockpit pane;
//! when it holds a worktree, `mode` decides its fate — `'merge'` commits the worktree's dirty
//! changes on its own branch, enforces the pane's scope on the ACTUAL diff (git-level, post-hoc,
//! not the agent's self-report), then `git merge --no-ff`s into the base branch (a conflict
//! ABORTS and keeps the pane + worktree — nothing lost); `'discard'`/omitted just tears the
//! worktree down.
//!
//! BESPOKE-NATIVE (not conform-db fixture-tested), same class as git-manager-worktree-ops.rs /
//! git-manager-branch-init.rs: EVERY code path that reaches the merge branch shells out to real
//! `git` unconditionally (status/add/commit/diff/merge/branch -d), and the non-merge path calls
//! the already-bespoke-native `worktreeRemove` twin. Proven via `#[cfg(test)]` native tests
//! against real temp git repos (make_temp_git_repo / with_home convention shared with
//! git-manager-worktree-ops.rs) — no fixture JSON, no frontier entry, no dispatch arm.
//!
//! Cross-module calls (pre-existing Rust twins, per the playbook's cross-module rule):
//! `crate::cockpit_cockpit_schema::ensureCockpitSchema`,
//! `crate::agents_git_manager_git_manager_worktree_ops::worktreeRemove`,
//! `crate::engine_workspace_root::getWorkspaceRoot`.
//!
//! `context.db` is always injected by the db lane in production, so the JS
//! `if (!db) throw err.contextMissing(...)` guard is unreachable here (mirrors every sibling
//! twin's convention, e.g. driver-queue.rs).
//!
//! The JS `console.log('[gate vibe.merge] ...')` on a successful merge is a pure logging side
//! effect (not part of the returned `Value`, not observable by any caller) — omitted here per the
//! same disclosed-simplification convention other twins use for non-observable console output.
//!
//! Glob→regex (`scope_json` scope enforcement): ported byte-for-byte from the JS
//! `globToRe` closure — escape regex metacharacters, then `**` → `.*`, then remaining `*` →
//! `[^/]*` (in that order, matching the JS `.replace` chain exactly, including the same
//! collision-avoiding `<<GLOBSTAR>>` placeholder token).
// QUALITY: 78/100 — bespoke-native (unconditional real-git subprocess chain on the merge path), scope-glob and merge/abort paths proven via temp-git-repo native tests.

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::path::Path;

// JS `Number(v)`.
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
fn truthy_num(n: f64) -> bool { n != 0.0 && !n.is_nan() }
// node:sqlite binds an integer-valued JS number as INTEGER, else REAL — mirror the bind type.
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }
// = JS `${v}` for a whole-valued Number.
fn num_tmpl(n: f64) -> String { if n.is_finite() && n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 { format!("{}", n as i64) } else { n.to_string() } }
fn take(s: &str, n: usize) -> String { s.chars().take(n).collect() }

struct GitR { ok: bool, out: String, err: String }

/// `function git(args, cwd) { try { return { ok:true, out: execFileSync(...).trim() } }
/// catch (e) { return { ok:false, out: String(e?.stdout ?? ''), err: String(e?.stderr ?? e?.message ?? '') } } }`
/// IS `path` A LINKED WORKTREE OF THE REPO AT `root` — never the main worktree, never anything else on disk.
///
/// `git worktree list --porcelain` prints the MAIN worktree first and then each linked one; a linked entry is the
/// only thing this repository may ever delete. Paths are compared after canonicalisation so a symlinked or
/// trailing-slash spelling cannot slip past. Returns false when git cannot answer, which keeps the caller's hands
/// off anything it is not certain about.
fn is_linked_worktree(path: &str, root: &str) -> bool {
    let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| std::path::PathBuf::from(p));
    let (target, main) = (canon(path), canon(root));
    if target == main { return false }                       // the repository itself is never a box to remove
    let out = git(&["worktree", "list", "--porcelain"], root);
    if !out.ok { return false }
    out.out.lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(|p| canon(p.trim()))
        .skip(1)                                             // the first entry is the main worktree
        .any(|p| p == target)
}

fn git(args: &[&str], cwd: &str) -> GitR {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) => {
            let out = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr).into_owned();
            if o.status.success() {
                GitR { ok: true, out: out.trim().to_string(), err: String::new() }
            } else {
                GitR { ok: false, out, err }
            }
        }
        Err(e) => GitR { ok: false, out: String::new(), err: e.to_string() },
    }
}

fn escape_glob_regex_literal(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// `const globToRe = (g) => new RegExp('^' + String(g).replace(<escape>).replace(/\*\*/g,
/// '<<GLOBSTAR>>').replace(/\*/g, '[^/]*').replace(/<<GLOBSTAR>>/g, '.*') + '$')`
fn glob_to_regex(g: &str) -> regex::Regex {
    let escaped = escape_glob_regex_literal(g);
    let s1 = escaped.replace("**", "<<GLOBSTAR>>");
    let s2 = s1.replace('*', "[^/]*");
    let s3 = s2.replace("<<GLOBSTAR>>", ".*");
    regex::Regex::new(&format!("^{s3}$")).unwrap_or_else(|_| regex::Regex::new("^$never$").unwrap())
}

struct Pane { session_id: i64, idx: i64, branch: Option<String>, worktree_path: Option<String> }

#[allow(non_snake_case)]
pub fn cockpitPaneRemove(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let pane_id_n = js_number(input.get("pane_id"));
    if !truthy_num(pane_id_n) {
        return Err("cockpit-pane-remove: pane_id is required".into());
    }
    let _ = num_param; // pane_id is an integer id — bind as i64, not a JSON Value
    let pane_id_param: i64 = pane_id_n as i64;

    let pane: Option<Pane> = db
        .prepare("SELECT session_id, idx, branch, worktree_path FROM cockpit_panes WHERE id = ?")
        .map_err(|e| e.to_string())?
        .query_row(rusqlite::params![pane_id_param], |row| {
            Ok(Pane {
                session_id: row.get(0)?,
                idx: row.get(1)?,
                branch: row.get(2)?,
                worktree_path: row.get(3)?,
            })
        })
        .optional()
        .map_err(|e| e.to_string())?;

    let pane = match pane {
        Some(p) => p,
        None => {
            let mut out = Map::new();
            out.insert("ok".into(), json!(false));
            out.insert("error".into(), json!(format!("pane {} not found", num_tmpl(pane_id_n))));
            return Ok(Value::Object(out));
        }
    };

    let mode_is_merge = input.get("mode").and_then(Value::as_str) == Some("merge");
    let has_worktree = pane.worktree_path.as_deref().map_or(false, |p| Path::new(p).exists());
    let mut merged = false;

    if mode_is_merge && pane.branch.is_some() && pane.worktree_path.is_some() && has_worktree {
        let branch = pane.branch.clone().unwrap();
        let worktree_path = pane.worktree_path.clone().unwrap();

        let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
        let root = root_v.as_str().unwrap_or("").to_string();
        if root.is_empty() {
            let mut out = Map::new();
            out.insert("ok".into(), json!(false));
            out.insert("error".into(), json!("no git workspace to merge into — the pane stays open"));
            return Ok(Value::Object(out));
        }

        let dirty = git(&["status", "--porcelain"], &worktree_path);
        if dirty.ok && !dirty.out.is_empty() {
            let _ = git(&["add", "-A"], &worktree_path);
            let commit_msg = format!("vibe pane #{} close (operator merge)", pane.idx + 1);
            let cm = git(&["commit", "-m", &commit_msg], &worktree_path);
            let combined = format!("{}{}", cm.err, cm.out);
            if !cm.ok && !combined.contains("nothing to commit") {
                let msg = if !cm.err.is_empty() { &cm.err } else { &cm.out };
                let mut out = Map::new();
                out.insert("ok".into(), json!(false));
                out.insert(
                    "error".into(),
                    json!(format!("commit before merge failed: {} — the pane stays open", take(msg, 200))),
                );
                return Ok(Value::Object(out));
            }
        }

        // EPIC-VIBE-OPENCODE-ACT-LAYER P3 MERGE GATE: scope is enforced on WHAT ACTUALLY
        // CHANGED (real git diff), whatever the agent inside the box claimed.
        let scope_json: Option<String> = db
            .prepare("SELECT scope_json FROM cockpit_panes WHERE id = ?")
            .map_err(|e| e.to_string())?
            .query_row(rusqlite::params![pane_id_param], |row| row.get(0))
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();
        let scope_globs: Vec<String> = scope_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .and_then(|v| v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()))
            .unwrap_or_default();

        if !scope_globs.is_empty() {
            const ALLOW: [&str; 5] = ["AGENTS.md", "CLAUDE.md", "opencode.json", "package.json", "package-lock.json"];
            let d = git(&["diff", "--name-only", &format!("HEAD...{branch}")], &root);
            let changed: Vec<&str> = if d.ok { d.out.split(['\r', '\n']).map(str::trim).filter(|s| !s.is_empty()).collect() } else { Vec::new() };
            let regexes: Vec<regex::Regex> = scope_globs.iter().map(|g| glob_to_regex(g)).collect();
            let out_of_scope: Vec<&str> = changed
                .into_iter()
                .filter(|f| !ALLOW.contains(f) && !crate::cockpit__helpers_scope::is_bake_artifact(f) && !regexes.iter().any(|re| re.is_match(f)))
                .collect();
            if !out_of_scope.is_empty() {
                let shown: Vec<&str> = out_of_scope.iter().take(6).copied().collect();
                let extra = if out_of_scope.len() > 6 { format!(" (+{} more)", out_of_scope.len() - 6) } else { String::new() };
                let mut out = Map::new();
                out.insert("ok".into(), json!(false));
                out.insert(
                    "error".into(),
                    json!(format!(
                        "merge REFUSED — out-of-scope changes: {}{}. The pane scope is the boundary; discard or re-scope.",
                        shown.join(", "),
                        extra
                    )),
                );
                return Ok(Value::Object(out));
            }
        }

        let merge_msg = format!("merge {branch} (vibe pane #{})", pane.idx + 1);
        let m = git(&["merge", "--no-ff", "-m", &merge_msg, &branch], &root);
        if !m.ok {
            let _ = git(&["merge", "--abort"], &root);
            let msg = if !m.err.is_empty() { &m.err } else { &m.out };
            let mut out = Map::new();
            out.insert("ok".into(), json!(false));
            out.insert(
                "error".into(),
                json!(format!(
                    "merge conflict — ABORTED, nothing changed: {}. Resolve in the worktree or discard.",
                    take(msg, 220)
                )),
            );
            return Ok(Value::Object(out));
        }
        merged = true;
    }

    let worktree_input = json!({
        "card_slug": format!("cockpit-{}-{}", pane.session_id, pane.idx),
        "force": true,
    });
    let _ = crate::agents_git_manager_git_manager_worktree_ops::worktreeRemove(&worktree_input, db);

    // AND THE BOX THIS PANE ACTUALLY HELD (found 2026-09-28: six orphaned worktrees in one evening).
    //
    // `worktreeRemove` above finds a box by the COCKPIT CARD SLUG, `cockpit-<session>-<idx>`. A forked agent's
    // box is not named that — it is `<agents root>/<repo>/<slug>` — so for every fork the call above matched
    // nothing, reported nothing, and the worktree and its branch stayed on disk for ever while the pane row was
    // deleted. Nothing afterwards could reach them: the pane that knew the path was gone.
    //
    // The pane's own `worktree_path` is the authority, and it is right here. `git worktree remove --force`
    // detaches it properly (a plain `rm -rf` would leave the registration behind, which is a different orphan),
    // and the prune sweeps the registration if the directory went another way.
    // ONLY A LINKED WORKTREE, AND GIT HAS TO SAY SO (the guard, added the same day this removal was).
    //
    // The first version of this deleted `pane.worktree_path` outright when `git worktree remove` did not take it.
    // A pane that SHARES the repository — main does, `worktree_path` is the workspace root itself — would have had
    // the operator's entire repository deleted by `remove_dir_all` the first time anyone closed it, because
    // `git worktree remove` refuses the MAIN worktree and the fallback did not care why it refused.
    //
    // So the path must be a LINKED worktree of this repo, confirmed by git itself, and never the main one. Nothing
    // is deleted on a guess: if git does not list it, it is left alone and the caller is none the wiser, which is
    // the right failure for an operation whose worst case is unrecoverable.
    if let Some(wt) = pane.worktree_path.as_deref().filter(|p| !p.is_empty()) {
        let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
        let root = root_v.as_str().unwrap_or("").to_string();
        if Path::new(wt).exists() && !root.is_empty() && is_linked_worktree(wt, &root) {
            let _ = git(&["worktree", "remove", "--force", wt], &root);
            // …and only once git has agreed it was a linked worktree AND has let go of it
            if Path::new(wt).exists() && !is_linked_worktree(wt, &root) { let _ = std::fs::remove_dir_all(wt); }
            let _ = git(&["worktree", "prune"], &root);
        }
    }

    // merged → the branch is folded in and can go (-d); discard (2026-09-16, the close dialog's
    // "discard") → the branch's unmerged commits go with the box (-D). Plain remove keeps the branch.
    let mode_is_discard = input.get("mode").and_then(Value::as_str) == Some("discard");
    if merged || mode_is_discard {
        if let Some(branch) = &pane.branch {
            let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
            let root = root_v.as_str().unwrap_or("").to_string();
            if !root.is_empty() { let _ = git(&[if merged { "branch" } else { "branch" }, if merged { "-d" } else { "-D" }, branch], &root); }
        }
    }

    db.execute(
        "DELETE FROM cockpit_blackboard WHERE session_id = ? AND pane_id = ?",
        rusqlite::params![pane.session_id, pane_id_param],
    )
    .map_err(|e| e.to_string())?;
    db.execute("DELETE FROM cockpit_claims WHERE pane_id = ?", rusqlite::params![pane_id_param])
        .map_err(|e| e.to_string())?;
    db.execute("DELETE FROM cockpit_panes WHERE id = ?", rusqlite::params![pane_id_param])
        .map_err(|e| e.to_string())?;

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("removed".into(), num_param(pane_id_n));
    out.insert("merged".into(), json!(merged));
    out.insert("discarded".into(), json!(mode_is_discard && !merged));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    // SAFETY: ANTILOKI_HOME is process-global; serialize since `cargo test` runs threads in
    // parallel by default and other converted twins' tests read the same var — shared
    // crate-wide lock, same convention as git-manager-worktree-ops.rs.
    fn with_home<T>(antiloki_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_HOME", antiloki_home); }
        let result = f();
        unsafe { std::env::remove_var("ANTILOKI_HOME"); }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-cockpit-pane-remove-rs-{tag}-{}", std::process::id()));
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
        run_git(&["init", "-q"], &dir);
        run_git(&["config", "user.email", "test@test"], &dir);
        run_git(&["config", "user.name", "test"], &dir);
        fs::write(dir.join("README.md"), "# test\n").unwrap();
        run_git(&["add", "README.md"], &dir);
        run_git(&["commit", "-q", "-m", "init"], &dir);
        dir
    }

    fn db_with_workspace(root: &Path) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
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

    fn insert_pane(c: &Connection, id: i64, session_id: i64, idx: i64, branch: Option<&str>, worktree_path: Option<&str>) {
        c.execute(
            "INSERT INTO cockpit_panes (id, session_id, idx, branch, worktree_path, scope_json, grants_json) VALUES (?, ?, ?, ?, ?, '[]', '[]')",
            rusqlite::params![id, session_id, idx, branch, worktree_path],
        )
        .unwrap();
    }

    #[test]
    fn missing_pane_id_errors() {
        let repo = make_temp_git_repo("missing-id");
        let c = db_with_workspace(&repo);
        let e = cockpitPaneRemove(&json!({}), &c).unwrap_err();
        assert_eq!(e, "cockpit-pane-remove: pane_id is required");
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn unknown_pane_returns_ok_false_not_found() {
        let repo = make_temp_git_repo("unknown-pane");
        let c = db_with_workspace(&repo);
        let r = cockpitPaneRemove(&json!({ "pane_id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "pane 999 not found" }));
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn discard_no_worktree_deletes_pane_rows() {
        let repo = make_temp_git_repo("discard-no-wt");
        let c = db_with_workspace(&repo);
        insert_pane(&c, 1, 5, 0, None, None);
        c.execute("INSERT INTO cockpit_claims (session_id, pane_id, path_glob) VALUES (5, 1, 'x')", []).unwrap();
        c.execute("INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (5, 1, 'note', 'x')", []).unwrap();
        let r = cockpitPaneRemove(&json!({ "pane_id": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "removed": 1, "merged": false, "discarded": false }));
        let left: i64 = c.query_row("SELECT COUNT(*) FROM cockpit_panes", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
        let claims: i64 = c.query_row("SELECT COUNT(*) FROM cockpit_claims", [], |r| r.get(0)).unwrap();
        assert_eq!(claims, 0);
        let bb: i64 = c.query_row("SELECT COUNT(*) FROM cockpit_blackboard", [], |r| r.get(0)).unwrap();
        assert_eq!(bb, 0);
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn merge_no_worktree_falls_back_to_discard() {
        // mode:'merge' but the worktree path doesn't exist on disk → the merge branch is
        // skipped entirely (JS: `existsSync(pane.worktree_path)` gates it), same as discard.
        let repo = make_temp_git_repo("merge-no-wt");
        let home = temp_dir("merge-no-wt-home");
        let c = db_with_workspace(&repo);
        insert_pane(&c, 1, 5, 0, Some("some-branch"), Some(home.join("worktrees").join("ghost").to_str().unwrap()));
        let r = with_home(home.to_str().unwrap(), || cockpitPaneRemove(&json!({ "pane_id": 1, "mode": "merge" }), &c).unwrap());
        assert_eq!(r, json!({ "ok": true, "removed": 1, "merged": false, "discarded": false }));
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn merge_commits_dirty_worktree_and_merges_no_ff() {
        let repo = make_temp_git_repo("merge-happy");
        let home = temp_dir("merge-happy-home");
        let c = db_with_workspace(&repo);
        // `git worktree add <path> <branch>` (worktreeAdd's real-JS-matching invocation, no
        // `-b`) checks out an EXISTING branch — it does not create one. Real git confirms:
        // `git worktree add path pane-branch` on a repo without that ref fails with
        // "fatal: invalid reference: pane-branch". Create the branch first, same as a real
        // caller would (e.g. git-manager-branch-init) before opening a cockpit pane on it.
        run_git(&["branch", "pane-branch"], &repo);
        with_home(home.to_str().unwrap(), || {
            crate::agents_git_manager_git_manager_worktree_ops::worktreeAdd(&json!({ "card_slug": "cockpit-5-0", "branch": "pane-branch" }), &c).unwrap()
        });
        let wt_path = home.join("worktrees").join("cockpit-5-0");
        fs::write(wt_path.join("new.txt"), "hi").unwrap();
        insert_pane(&c, 1, 5, 0, Some("pane-branch"), Some(wt_path.to_str().unwrap()));

        let r = with_home(home.to_str().unwrap(), || cockpitPaneRemove(&json!({ "pane_id": 1, "mode": "merge" }), &c).unwrap());
        assert_eq!(r, json!({ "ok": true, "removed": 1, "merged": true, "discarded": false }));
        assert!(repo.join("new.txt").exists()); // merged into base
        let left: i64 = c.query_row("SELECT COUNT(*) FROM cockpit_panes", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
        // the worktree itself is torn down by worktreeRemove; the merged branch is deleted (-d)
        let branches = git(&["branch", "--list", "pane-branch"], repo.to_str().unwrap());
        assert!(branches.out.is_empty(), "branch should be deleted: {}", branches.out);
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn merge_conflict_aborts_and_keeps_pane() {
        let repo = make_temp_git_repo("merge-conflict");
        let home = temp_dir("merge-conflict-home");
        let c = db_with_workspace(&repo);
        // branch off the ORIGINAL commit ("# test\n") before base diverges — must exist as a
        // real ref before worktreeAdd (which checks out an existing branch, not `-b`).
        run_git(&["branch", "pane-branch"], &repo);
        with_home(home.to_str().unwrap(), || {
            crate::agents_git_manager_git_manager_worktree_ops::worktreeAdd(&json!({ "card_slug": "cockpit-5-0", "branch": "pane-branch" }), &c).unwrap()
        });
        let wt_path = home.join("worktrees").join("cockpit-5-0");
        // base now diverges: edits the SAME line of the SAME file after the branch point
        fs::write(repo.join("README.md"), "# test\nbase-change\n").unwrap();
        run_git(&["commit", "-am", "base change"], &repo);
        // pane edits the same line differently (left uncommitted — cockpitPaneRemove auto-commits it)
        fs::write(wt_path.join("README.md"), "# test\npane-change\n").unwrap();
        insert_pane(&c, 1, 5, 0, Some("pane-branch"), Some(wt_path.to_str().unwrap()));

        let r = with_home(home.to_str().unwrap(), || cockpitPaneRemove(&json!({ "pane_id": 1, "mode": "merge" }), &c).unwrap());
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().starts_with("merge conflict"), "{r}");
        // pane stays open — nothing deleted
        let left: i64 = c.query_row("SELECT COUNT(*) FROM cockpit_panes", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 1);
        assert!(wt_path.exists());
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn merge_refuses_out_of_scope_changes() {
        let repo = make_temp_git_repo("merge-scope");
        let home = temp_dir("merge-scope-home");
        let c = db_with_workspace(&repo);
        run_git(&["branch", "pane-branch"], &repo);
        with_home(home.to_str().unwrap(), || {
            crate::agents_git_manager_git_manager_worktree_ops::worktreeAdd(&json!({ "card_slug": "cockpit-5-0", "branch": "pane-branch" }), &c).unwrap()
        });
        let wt_path = home.join("worktrees").join("cockpit-5-0");
        fs::write(wt_path.join("out-of-scope.txt"), "nope").unwrap();
        run_git(&["add", "-A"], &wt_path);
        run_git(&["commit", "-m", "sneaky"], &wt_path);
        insert_pane(&c, 1, 5, 0, Some("pane-branch"), Some(wt_path.to_str().unwrap()));
        c.execute("UPDATE cockpit_panes SET scope_json = '[\"src/**\"]' WHERE id = 1", []).unwrap();

        let r = with_home(home.to_str().unwrap(), || cockpitPaneRemove(&json!({ "pane_id": 1, "mode": "merge" }), &c).unwrap());
        assert_eq!(r["ok"], json!(false));
        let msg = r["error"].as_str().unwrap();
        assert!(msg.contains("merge REFUSED"), "{msg}");
        assert!(msg.contains("out-of-scope.txt"), "{msg}");
        let left: i64 = c.query_row("SELECT COUNT(*) FROM cockpit_panes", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 1);
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn merge_scope_globstar_and_allowlist_pass() {
        let repo = make_temp_git_repo("merge-scope-ok");
        let home = temp_dir("merge-scope-ok-home");
        let c = db_with_workspace(&repo);
        run_git(&["branch", "pane-branch"], &repo);
        with_home(home.to_str().unwrap(), || {
            crate::agents_git_manager_git_manager_worktree_ops::worktreeAdd(&json!({ "card_slug": "cockpit-5-0", "branch": "pane-branch" }), &c).unwrap()
        });
        let wt_path = home.join("worktrees").join("cockpit-5-0");
        fs::create_dir_all(wt_path.join("src").join("ui")).unwrap();
        fs::write(wt_path.join("src").join("ui").join("a.ts"), "x").unwrap();
        fs::write(wt_path.join("package.json"), "{}").unwrap();
        run_git(&["add", "-A"], &wt_path);
        run_git(&["commit", "-m", "in scope"], &wt_path);
        insert_pane(&c, 1, 5, 0, Some("pane-branch"), Some(wt_path.to_str().unwrap()));
        c.execute("UPDATE cockpit_panes SET scope_json = '[\"src/ui/**\"]' WHERE id = 1", []).unwrap();

        let r = with_home(home.to_str().unwrap(), || cockpitPaneRemove(&json!({ "pane_id": 1, "mode": "merge" }), &c).unwrap());
        assert_eq!(r, json!({ "ok": true, "removed": 1, "merged": true, "discarded": false }));
        assert!(repo.join("src").join("ui").join("a.ts").exists());
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn glob_to_regex_matches_globstar_and_single_star() {
        assert!(glob_to_regex("src/**").is_match("src/ui/a.ts"));
        assert!(glob_to_regex("src/*.ts").is_match("src/a.ts"));
        assert!(!glob_to_regex("src/*.ts").is_match("src/ui/a.ts"));
        assert!(glob_to_regex("a.b").is_match("a.b"));
        assert!(!glob_to_regex("a.b").is_match("aXb")); // '.' escaped, not a wildcard
    }
}

#[cfg(test)]
mod worktree_guard_tests {
    use super::*;

    fn sh(args: &[&str], cwd: &std::path::Path) {
        let _ = crate::proc::cmd(args[0]).args(&args[1..]).current_dir(cwd).output();
    }

    /// The repository must never be deletable as if it were a box (the guard's whole reason to exist).
    #[test]
    fn the_main_worktree_is_never_a_removable_box() {
        let base = std::env::temp_dir().join(format!("antiloki-wt-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        sh(&["git", "init", "-q", "."], &repo);
        sh(&["git", "config", "user.email", "t@t"], &repo);
        sh(&["git", "config", "user.name", "t"], &repo);
        sh(&["git", "commit", "-q", "--allow-empty", "-m", "base"], &repo);
        let root = repo.to_string_lossy().to_string();

        // the repository itself: refused, however it is spelled
        assert!(!is_linked_worktree(&root, &root), "the main worktree is not a box");
        assert!(!is_linked_worktree(&format!("{root}/"), &root), "…nor with a trailing slash");

        // something that is not a worktree at all: refused
        let stray = base.join("stray");
        std::fs::create_dir_all(&stray).unwrap();
        assert!(!is_linked_worktree(&stray.to_string_lossy(), &root), "a plain directory is not a box");

        // a real linked worktree: accepted
        let box_path = base.join("abox");
        sh(&["git", "worktree", "add", "-q", "-b", "abox", &box_path.to_string_lossy(), "HEAD"], &repo);
        if box_path.exists() {
            assert!(is_linked_worktree(&box_path.to_string_lossy(), &root), "a linked worktree IS a box");
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
