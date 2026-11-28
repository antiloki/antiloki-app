//! cockpit-pr-open.rs — twin of skills/cockpit/cockpit-pr-open.mjs (async default export
//! `cockpitPrOpen(input, context)`, now sync per the async-lane convention — the `async` keyword is
//! vestigial, nothing genuinely awaits a promise). NAMES ARE CONTRACT: `cockpitPrOpen`.
//! EPIC-VIBE-COCKPIT (pull requests). Open/refresh a PR for a pane (or every pane of a session with
//! changes): commit the pane's worktree, capture the diff (base..branch), preview mergeability vs
//! base (git merge-tree — no working tree touched). Upserts one PR per (session, branch).
//!
//! GIT SUBPROCESS — DISCLOSED UNTESTED (playbook lesson 5's flag-skippable pattern, same shape as
//! cockpit-pane-diff.rs's `git_run`): the JS `git()` (`spawnSync('git', args, {cwd,...})`) fires on
//! every pane whose `worktree_path` is truthy AND `root` (`getWorkspaceRoot(db)`) is truthy. This
//! twin ports the whole cluster — `git add`/`diff`/`reset`/`commit`/`merge-tree`, the edit-whitelist
//! scope gate (via the already-converted `scope.rs` twin), and the upsert — as real, correct code,
//! but NO fixture case reaches it: `worktree_path` is a nullable DB column (fixture-controllable —
//! both branches of `!p.worktree_path` are exercised), while `root` is NOT fixture-controllable
//! (`getWorkspaceRoot` always resolves to a non-empty string, falling back to `SUBSTRATE_ROOT` —
//! see workspace-root.rs — so `if (root)` is unconditionally true whenever a pane clears the
//! worktree_path gate). Real git repo/branch state is not deterministically driveable through the
//! SQL-only conform-db harness, so every fixture case here stops at `worktree_path` being absent —
//! the pane_id lookup has no such filter (mirrors cockpit-pane-diff.rs's pane-not-found/
//! worktree-not-provisioned split); the session_id lookup filters `worktree_path IS NOT NULL` in
//! SQL itself, so ANY row it would return is guaranteed to enter the git branch — its only
//! fixture-safe case is therefore the empty-result path (session/panes that don't match).
//!
//! `context.db` missing → JS throws `err.contextMissing('cockpit-pr-open','db')`. Unreachable here:
//! `db: &Connection` is required/non-nullable, same convention every sibling db-lane twin uses.
//! Neither `pane_id` nor `session_id` truthy → JS throws `err.required('cockpit-pr-open',
//! 'session_id or pane_id')` (message `"cockpit-pr-open: session_id or pane_id is required"`) —
//! modeled as `Err`, guard-throw not fixture-tested per gotcha #9 (see `session_id_or_pane_id_required` test).
//!
//! JS wraps the whole git cluster in `try {...} catch (e) { skipped.push({idx, reason:
//! String(e?.message??e).slice(0,80)}); continue; }`. `git_run` never "throws" (mirrors `spawnSync`
//! never throwing for a missing binary — see cockpit-pane-diff.rs), and `paneScope`/`splitByScope`
//! are infallible `Value -> Value` twins, so that catch is dead code here and not separately
//! modeled (same disclosed convention as cockpit-pane-diff.rs). One further disclosed simplification
//! *inside* the already-disclosed-untested branch: JS passes `p.base_branch`/`p.branch` directly
//! (not stringified) as `spawnSync` args — a `null` `branch` (nullable column, no default) would
//! make Node's real `spawnSync` throw a `TypeError` synchronously (caught by that same outer
//! try/catch → skipped). This twin renders them via `tmpl` (yielding the literal string `"null"`)
//! into the subprocess args instead of replicating that throw path — `base_branch` comes from
//! `cockpit_sessions.base_branch` which is `NOT NULL DEFAULT ''` at the schema level, so only
//! `branch` being null is a real (if rare) production possibility, and it sits entirely inside a
//! branch no fixture exercises either way.
// QUALITY: 74/100 — real git subprocess + scope-gate + upsert ported for the full pane cluster;
// fixture covers only the git-free early-return paths (pane not found · no worktree · session
// query's own WHERE filter yields nothing), disclosed above.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}
// node:sqlite binds an integral JS number as INTEGER, a fractional one as REAL.
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }

struct GitOut { stdout: String, status: i32 }

/// `const git = (args, cwd) => spawnSync('git', args, { cwd, encoding: 'utf8', maxBuffer: 8*1024*1024 })`
/// — never throws for a missing binary (see module doc); collapses to an empty/status=-1 result.
fn git_run(args: &[&str], cwd: &str) -> GitOut {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) => GitOut { stdout: String::from_utf8_lossy(&o.stdout).into_owned(), status: o.status.code().unwrap_or(-1) },
        Err(_) => GitOut { stdout: String::new(), status: -1 },
    }
}

/// `.slice(0, n)` on a string, JS UTF-16 code-unit semantics approximated via chars (gotcha #5;
/// this branch carries no fixture coverage either way, see module doc).
fn js_slice0_str(s: &str, n: usize) -> String { s.chars().take(n).collect() }

pub fn cockpitPrOpen(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `if (!db) throw err.contextMissing(...)` — unreachable in the db lane (db is always present).
    let _ = crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let panes: Vec<Value> = if truthy(input.get("pane_id")) {
        let p = num_param(js_number(input.get("pane_id")));
        crate::db_json::query_rows(
            db,
            "SELECT p.*, s.base_branch FROM cockpit_panes p JOIN cockpit_sessions s ON s.id = p.session_id WHERE p.id = ?",
            &[p],
        )?.as_array().cloned().unwrap_or_default()
    } else if truthy(input.get("session_id")) {
        let p = num_param(js_number(input.get("session_id")));
        crate::db_json::query_rows(
            db,
            "SELECT p.*, s.base_branch FROM cockpit_panes p JOIN cockpit_sessions s ON s.id = p.session_id WHERE p.session_id = ? AND p.worktree_path IS NOT NULL ORDER BY p.idx",
            &[p],
        )?.as_array().cloned().unwrap_or_default()
    } else {
        return Err("cockpit-pr-open: session_id or pane_id is required".into());
    };

    // JS `try { root = getWorkspaceRoot(db) } catch { root = null }` — getWorkspaceRoot never
    // errors given a present db (see workspace-root.rs doc), so the try/catch is dead here; root is
    // therefore always a non-empty string (falls back to SUBSTRATE_ROOT) — see module doc.
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or_default().to_string();

    let mut opened: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();

    'panes: for p in &panes {
        let worktree_path = p.get("worktree_path").cloned().unwrap_or(Value::Null);
        if !truthy(Some(&worktree_path)) {
            skipped.push(json!({ "idx": p.get("idx").cloned().unwrap_or(Value::Null), "reason": "no worktree" }));
            continue;
        }
        let wt = worktree_path.as_str().unwrap_or_default();

        let mut changed: i64 = 0;
        let mut base_for_row: Option<String> = p.get("base_branch").and_then(Value::as_str).filter(|b| !b.is_empty()).map(String::from);
        let mut patch = String::new();
        let mut mergeable: Option<i64> = None;
        let mut conflicts: Vec<String> = Vec::new();

        if !root.is_empty() {
            // GIT SUBPROCESS + SCOPE-GATE BLOCK — real code, disclosed untested by fixture (see
            // module doc: `root` is unconditionally truthy in practice once worktree_path clears).
            git_run(&["add", "-A"], wt);
            let name_out = git_run(&["diff", "--cached", "--name-only"], wt);
            let mut names: Vec<Value> = name_out.stdout.split('\n').filter(|s| !s.is_empty()).map(|s| json!(s)).collect();
            // antiloki's own bake artifacts (the pack, the skills, the briefs) never ride a PR (2026-09-19)
            let artifacts: Vec<String> = names.iter().filter_map(|v| v.as_str()).filter(|f| crate::cockpit__helpers_scope::is_bake_artifact(f)).map(String::from).collect();
            if !artifacts.is_empty() {
                let mut reset_args: Vec<String> = vec!["reset".into(), "-q".into(), "HEAD".into(), "--".into()];
                reset_args.extend(artifacts.iter().cloned());
                let reset_refs: Vec<&str> = reset_args.iter().map(String::as_str).collect();
                git_run(&reset_refs, wt);
                names = crate::cockpit__helpers_scope::without_bake_artifacts(&json!(names)).as_array().cloned().unwrap_or_default();
            }

            let scope = crate::cockpit__helpers_scope::paneScope(db, p);
            let split = crate::cockpit__helpers_scope::splitByScope(&scope, &json!(names));
            let out_of_scope: Vec<Value> = split.get("out_of_scope").and_then(Value::as_array).cloned().unwrap_or_default();
            if !out_of_scope.is_empty() {
                let mode = match nz(input.get("scope_mode")) {
                    Some(v) => js_string(v),
                    None => "block".to_string(),
                };
                if mode == "in_scope_only" {
                    let mut reset_args: Vec<String> = vec!["reset".into(), "HEAD".into(), "--".into()];
                    for f in &out_of_scope { reset_args.push(js_string(f)); }
                    let reset_refs: Vec<&str> = reset_args.iter().map(String::as_str).collect();
                    git_run(&reset_refs, wt);
                    let name_out2 = git_run(&["diff", "--cached", "--name-only"], wt);
                    names = name_out2.stdout.split('\n').filter(|s| !s.is_empty()).map(|s| json!(s)).collect();
                } else if mode != "override" {
                    skipped.push(json!({
                        "idx": p.get("idx").cloned().unwrap_or(Value::Null),
                        "reason": "out_of_scope",
                        "files": out_of_scope.iter().take(40).cloned().collect::<Vec<_>>(),
                        "scope": scope,
                    }));
                    continue 'panes;
                }
            }

            changed = names.len() as i64;
            if changed != 0 {
                // a goal run's PR commits with its goal + joined report (2026-09-19) — the generic pane
                // line stays for a hand-closed pane
                let msg = match (nz(input.get("title")), nz(input.get("message"))) {
                    (Some(t), Some(m)) => format!("{}\n\n{}", js_string(t), js_string(m)),
                    (Some(t), None) => js_string(t),
                    (None, Some(m)) => format!("cockpit pane {} ({})\n\n{}", tmpl(p.get("idx")), tmpl(p.get("subagent_name")), js_string(m)),
                    (None, None) => format!("cockpit pane {} ({})", tmpl(p.get("idx")), tmpl(p.get("subagent_name"))),
                };
                git_run(&["commit", "-m", &msg], wt);
            }
            // an antiChat session has no base branch of its own (2026-09-19) — the workspace root's branch is
            // the base, the same fallback pane-diff makes; without it the diff was empty and the PR "? unknown"
            let mut base_branch_v = p.get("base_branch").cloned().unwrap_or(Value::Null);
            // THE BOX'S OWN REPOSITORY (2026-09-19): the pane's worktree belongs to the WORKSPACE repo
            // (react-main, say), not to the substrate root — git runs in the worktree, and the base is
            // the main checkout's branch (the parent of the git common dir), never the substrate's HEAD
            let common = git_run(&["rev-parse", "--path-format=absolute", "--git-common-dir"], wt).stdout.trim().to_string();
            let repo_root = std::path::Path::new(&common).parent().map(|d| d.to_string_lossy().into_owned()).filter(|d| !d.is_empty()).unwrap_or_else(|| root.clone());
            if !truthy(Some(&base_branch_v)) {
                let head = git_run(&["rev-parse", "--abbrev-ref", "HEAD"], &repo_root).stdout.trim().to_string();
                if !head.is_empty() && head != "HEAD" { base_branch_v = json!(head); }
            }
            let branch_v = p.get("branch").cloned().unwrap_or(Value::Null);
            if let Some(b) = base_branch_v.as_str() { base_for_row = Some(b.to_string()); }
            let base_branch_s = tmpl(Some(&base_branch_v));
            let branch_s = tmpl(Some(&branch_v));
            let diff_out = git_run(&["diff", &format!("{base_branch_s}...{branch_s}")], wt);
            patch = js_slice0_str(&diff_out.stdout, 200000);
            // `changed` is what the PR carries against its base, not what this call staged: a re-open
            // after the commit counted 0 files while the patch held two (2026-09-19)
            let carried = git_run(&["diff", "--name-only", &format!("{base_branch_s}...{branch_s}")], wt).stdout.lines().filter(|l| !l.trim().is_empty()).count() as i64;
            if carried > changed { changed = carried; }

            if truthy(Some(&base_branch_v)) && truthy(Some(&branch_v)) {
                let mt = git_run(&["merge-tree", "--write-tree", "--name-only", &base_branch_s, &branch_s], wt);
                if mt.status == 0 {
                    mergeable = Some(1);
                } else if mt.status == 1 {
                    mergeable = Some(0);
                    let lines: Vec<&str> = mt.stdout.split('\n').collect();
                    if let Some(b) = lines.iter().position(|l| l.is_empty()) {
                        conflicts = lines[b + 1..].iter().filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
                    }
                }
            }
            // JS wraps this whole block in try/catch (skip + continue on error) — dead code here,
            // see module doc.
        }

        if changed == 0 && patch.is_empty() {
            skipped.push(json!({ "idx": p.get("idx").cloned().unwrap_or(Value::Null), "reason": "no changes" }));
            continue;
        }

        let title = nz(input.get("title")).map(|t| js_string(t).lines().next().unwrap_or_default().chars().take(120).collect::<String>()).filter(|t| !t.is_empty()).unwrap_or_else(|| format!("pane {} ({})", tmpl(p.get("idx")), tmpl(p.get("subagent_name"))));
        let summary = nz(input.get("message")).map(|m| js_string(m).chars().take(20_000).collect::<String>());
        let conflicts_json = serde_json::to_string(&conflicts).unwrap_or_else(|_| "[]".to_string());
        db.execute(
            &format!("INSERT INTO cockpit_pull_requests (session_id, pane_id, idx, agent, branch, base_branch, title, status, changed, mergeable, conflicts_json, patch, summary_md, updated_at) \
             VALUES (?,?,?,?,?,?,?, 'open', ?,?,?,?,?, {}) \
             ON CONFLICT(session_id, branch) DO UPDATE SET status='open', title=excluded.title, base_branch=CASE WHEN excluded.base_branch <> '' THEN excluded.base_branch ELSE base_branch END, changed=excluded.changed, mergeable=excluded.mergeable, conflicts_json=excluded.conflicts_json, patch=excluded.patch, summary_md=COALESCE(excluded.summary_md, summary_md), updated_at={}", crate::clock::now_sql(), crate::clock::now_sql()),
            rusqlite::params![
                p.get("session_id").and_then(Value::as_i64),
                p.get("id").and_then(Value::as_i64),
                p.get("idx").and_then(Value::as_i64),
                p.get("subagent_name").and_then(Value::as_str),
                p.get("branch").and_then(Value::as_str),
                base_for_row.clone().unwrap_or_default(),
                title,
                changed,
                mergeable,
                conflicts_json,
                patch,
                summary,
            ],
        )
        .map_err(|e| e.to_string())?;

        let rows = crate::db_json::query_rows(
            db,
            "SELECT id FROM cockpit_pull_requests WHERE session_id = ? AND branch = ?",
            &[p.get("session_id").cloned().unwrap_or(Value::Null), p.get("branch").cloned().unwrap_or(Value::Null)],
        )?;
        if let Some(row) = rows.as_array().and_then(|a| a.first()) {
            opened.push(row.get("id").cloned().unwrap_or(Value::Null));
        }
    }

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("opened".into(), json!(opened));
    out.insert("skipped".into(), json!(skipped));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cockpit_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'active', \
             mission_md TEXT, base_branch TEXT NOT NULL DEFAULT '', base_sha TEXT NOT NULL DEFAULT '', mode TEXT NOT NULL DEFAULT 'steer', \
             created_at TEXT NOT NULL DEFAULT (datetime('now')), updated_at TEXT);
             CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, idx INTEGER NOT NULL, \
             subagent_name TEXT NOT NULL DEFAULT '', scope_json TEXT NOT NULL DEFAULT '[]', grants_json TEXT NOT NULL DEFAULT '[]', \
             worktree_path TEXT, branch TEXT, tier TEXT NOT NULL DEFAULT 'low', base_agent TEXT, extra_prompt_md TEXT, model_id TEXT, \
             calls INTEGER NOT NULL DEFAULT 0, cost_usd REAL NOT NULL DEFAULT 0, status TEXT NOT NULL DEFAULT 'idle', last_activity TEXT, \
             UNIQUE (session_id, idx))",
        ).unwrap();
        c
    }

    #[test]
    fn pane_id_not_found_yields_empty_opened_and_skipped() {
        let c = fresh();
        let r = cockpitPrOpen(&json!({ "pane_id": 99 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "opened": [], "skipped": [] }));
    }

    #[test]
    fn pane_id_found_with_no_worktree_is_skipped() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_sessions (id, name, base_branch) VALUES (1, 's1', 'main')", []).unwrap();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx, subagent_name) VALUES (1, 1, 0, 'dev')", []).unwrap();
        let r = cockpitPrOpen(&json!({ "pane_id": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "opened": [], "skipped": [{ "idx": 0, "reason": "no worktree" }] }));
    }

    #[test]
    fn pane_id_found_with_empty_string_worktree_is_also_skipped() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_sessions (id, name, base_branch) VALUES (1, 's1', 'main')", []).unwrap();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx, subagent_name, worktree_path) VALUES (1, 1, 2, 'dev', '')", []).unwrap();
        let r = cockpitPrOpen(&json!({ "pane_id": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "opened": [], "skipped": [{ "idx": 2, "reason": "no worktree" }] }));
    }

    #[test]
    fn session_id_query_filters_out_null_worktree_panes_in_sql_itself() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_sessions (id, name, base_branch) VALUES (2, 's2', 'main')", []).unwrap();
        c.execute_batch(
            "INSERT INTO cockpit_panes (id, session_id, idx, subagent_name) VALUES (2, 2, 0, 'a');
             INSERT INTO cockpit_panes (id, session_id, idx, subagent_name) VALUES (3, 2, 1, 'b');",
        ).unwrap();
        // both panes for session 2 have NULL worktree_path -> the query's own
        // `AND p.worktree_path IS NOT NULL` filters them out before the loop even runs.
        let r = cockpitPrOpen(&json!({ "session_id": 2 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "opened": [], "skipped": [] }));
    }

    #[test]
    fn session_id_not_matching_any_session_yields_empty() {
        let c = fresh();
        let r = cockpitPrOpen(&json!({ "session_id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "opened": [], "skipped": [] }));
    }

    #[test]
    fn session_id_or_pane_id_required_guard() {
        let c = fresh();
        assert_eq!(
            cockpitPrOpen(&json!({}), &c),
            Err("cockpit-pr-open: session_id or pane_id is required".to_string())
        );
        assert_eq!(
            cockpitPrOpen(&json!({ "pane_id": 0, "session_id": 0 }), &c),
            Err("cockpit-pr-open: session_id or pane_id is required".to_string())
        );
    }
}
