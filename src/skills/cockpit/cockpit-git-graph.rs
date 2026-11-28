//! cockpit-git-graph.rs — twin of skills/cockpit/cockpit-git-graph.mjs (async default export
//! `cockpitGitGraph(input, context)`, now sync per the async-lane convention — the JS only
//! `await`s sync SQLite reads through `context.db`, so the resolved value is what this twin
//! returns directly). NAMES ARE CONTRACT: `cockpitGitGraph`. The GIT GRAPH panel (operator
//! 2026-07-06: "a panel for gitgraph with all worktrees") + the close-pane merge preview +
//! open-in-Explorer:
//!   op='graph' (default)  → base branch head + one lane per pane worktree (ahead commits,
//!                            dirty file count, diffstat vs base).
//!   op='close_info'       → everything the close-warning modal shows for one pane.
//!   op='open_worktree'    → opens that pane's worktree folder in the OS file explorer.
//!
//! GIT SUBPROCESS — DISCLOSED UNTESTED BY THE FIXTURE (playbook lesson 5's flag-skippable
//! pattern, same shape as cockpit-pane-diff.rs's `git_run` / cockpit-integrate.rs): the JS `git()`
//! (`execFileSync('git', args, {cwd, encoding:'utf8', timeout:15_000, stdio:[...]})`) fires
//! unconditionally on every `op='graph'` call that has a resolvable root (realistically always —
//! see the `root` note below) for the base head/branch, AND inside `laneFor` for every pane whose
//! `worktree_path` is truthy AND exists on disk. Both call sites are genuinely reachable via a
//! deterministic, fixture-controllable condition (a pane row with `worktree_path` NULL/empty, or
//! pointing at a path that doesn't exist), which IS exercised. What is NOT — and cannot be,
//! through this SQL-only conform-db harness, which has no real git repo to point `root`/`cwd` at
//! — is the actual git-output-bearing success path (`op='graph'`'s head/branch text, and
//! `laneFor`'s ahead/dirty/diffstat for an existing worktree). This twin ports `git()` as a real
//! `git_run` subprocess wrapper (mirrors `execFileSync` field-for-field: on a nonzero exit or a
//! spawn failure it never throws in the JS's `try/catch` sense, it just returns `{ok:false,...}`)
//! and writes real logic for the full graph/laneFor flow — VERIFIED against real git output via a
//! full JS behavior test (`cockpit-git-graph.test.mjs`, incl. real `git worktree add` scenarios
//! covering ahead-commits, dirty-file count, and the diffstat/dirty_stat "last line of `git diff
//! --stat`" quirk — see below) — but the conform-db fixture only exercises the git-free early
//! returns and the worktree-missing branch of `laneFor`.
//!
//! QUIRK, verified via the JS test: `dirty_stat`/`diffstat` take the LAST line of `git diff
//! --stat`'s output (`.split('\n').slice(-1)[0]`). With exactly one changed file that output has
//! TWO lines (` g.txt | 2 +-` then ` 1 file changed, ...`), so the captured value is the SUMMARY
//! line, not the per-file line — faithfully ported (not a re-derivation).
//!
//! QUIRK, verified via the JS test: `op='close_info'` returns `{ ok: true, ...laneFor(root, pane)
//! }`. Object spread is last-write-wins on VALUE but first-position on KEY — `laneFor`'s own `ok`
//! field (its `lane` base object always carries one) OVERWRITES the outer literal's `ok: true` in
//! place. So a worktree-missing pane's close_info result has `ok: false` (from `laneFor`), not
//! `true` — ported via `Map::insert` (serde_json `preserve_order`), which has the identical
//! update-in-place-keeps-position / append-if-new semantics as JS object spread.
//!
//! `root` resolution: JS wraps `getWorkspaceRoot(db)` in try/catch, but the already-converted
//! `engine_workspace_root::getWorkspaceRoot` twin never itself throws — a missing `workspaces`
//! table or no active row falls back to `SUBSTRATE_ROOT` rather than raising. So `root` is
//! realistically ALWAYS a non-empty string; the JS `if (!root)` branches (and this twin's mirror
//! of them, in both `op='close_info'` and `op='graph'`) are dead code on both sides — ported for
//! fidelity, not separately fixture-tested (same disclosed gap as cockpit-integrate.rs).
//!
//! `op='open_worktree'`'s `spawn(explorer/open/xdg-open, ...).unref()` (fire-and-forget OS window)
//! is ported as a real, platform-selected `std::process::Command::spawn()` call but is reached
//! only when a pane's worktree BOTH is truthy AND exists on disk — same git-free/git-having split
//! as above, and untestable through conform-db for the same "no real filesystem worktree" reason.
//! It is also deliberately not exercised by the JS behavior test (it would pop a real OS file
//! browser window during test runs).
//!
//! `context.db` missing → JS throws `err.contextMissing('cockpit-git-graph','db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention every sibling
//! db-lane twin uses). `session_id` missing/falsy (`Number(input?.session_id)` NaN or 0, only
//! reachable when `op` is neither 'open_worktree' nor 'close_info') → JS throws
//! `err.required('cockpit-git-graph','session_id')` — the one reachable throw path, modeled as
//! `Err` (guard-throws are not fixture-tested per playbook gotcha #9).
// QUALITY: 78/100 — real git subprocess + real explorer-spawn ported for the full graph/close_info/
// open_worktree flow (verified via a JS test with real `git worktree add` scenarios), fixture
// covers only the git-free early-return / worktree-missing paths (disclosed above)

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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS Number(v): the subset that appears here (missing/null/bool/number/string).
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
fn truthy_num(n: f64) -> bool { n != 0.0 && !n.is_nan() }
// node:sqlite binds an integral JS number as INTEGER, a fractional one as REAL.
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }

struct GitOut { ok: bool, out: String }

/// `const git = (args, cwd) => { try { return {ok:true, out: execFileSync(...).trim()}; }
/// catch (e) { return {ok:false, out: String(e?.stdout ?? ''), err: ...}; } }`. `.err` is never
/// read by any call site in this module (only `.ok`/`.out` are), so it is not modeled as a struct
/// field. A nonzero-exit `execFileSync` throws with `e.stdout`/`e.stderr` already decoded strings
/// (since `encoding:'utf8'` was passed) — `Command::output()` never throws for a nonzero exit, so
/// that branch is `Ok(o) if !o.status.success()`; a genuine spawn failure (e.g. `git` missing from
/// PATH) is `Err(_)` from `Command::output()`, mirrored to the JS catch's `e.stdout` being absent
/// (`String(undefined ?? '') === ''`).
fn git_run(args: &[&str], cwd: &str) -> GitOut {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) if o.status.success() => GitOut { ok: true, out: String::from_utf8_lossy(&o.stdout).trim().to_string() },
        Ok(o) => GitOut { ok: false, out: String::from_utf8_lossy(&o.stdout).into_owned() },
        Err(_) => GitOut { ok: false, out: String::new() },
    }
}

/// `function laneFor(root, pane)` — one worktree's git status vs `root`'s current branch.
fn laneFor(root: &str, pane: &Value) -> Map<String, Value> {
    let wt = pane.get("worktree_path").cloned().unwrap_or(Value::Null);
    let mut lane = Map::new();
    lane.insert("pane_id".into(), pane.get("id").cloned().unwrap_or(Value::Null));
    lane.insert("idx".into(), pane.get("idx").cloned().unwrap_or(Value::Null));
    lane.insert("name".into(), pane.get("subagent_name").cloned().unwrap_or(Value::Null));
    lane.insert("branch".into(), pane.get("branch").cloned().unwrap_or(Value::Null));
    lane.insert("worktree_path".into(), wt.clone());
    lane.insert("ok".into(), json!(false));

    if !truthy(Some(&wt)) || !std::path::Path::new(wt.as_str().unwrap_or("")).exists() {
        lane.insert("reason".into(), json!("worktree missing"));
        return lane;
    }
    let wt_str = wt.as_str().unwrap_or("");

    let base = git_run(&["rev-parse", "--abbrev-ref", "HEAD"], root);
    let base_branch = if base.ok { base.out.clone() } else { "master".to_string() };
    let pane_branch = tmpl(pane.get("branch"));
    let ahead = git_run(&["log", "--oneline", &format!("{base_branch}..{pane_branch}")], root);
    let dirty = git_run(&["status", "--porcelain"], wt_str);
    let stat = git_run(&["diff", "--stat", &format!("{base_branch}...{pane_branch}")], root);
    let dirty_stat = git_run(&["diff", "--stat"], wt_str);

    lane.insert("ok".into(), json!(true));
    lane.insert("base_branch".into(), json!(base_branch));
    lane.insert("ahead_commits".into(), if ahead.ok && !ahead.out.is_empty() {
        json!(ahead.out.split('\n').take(20).collect::<Vec<_>>())
    } else { json!([]) });
    lane.insert("dirty_files".into(), if dirty.ok && !dirty.out.is_empty() {
        json!(dirty.out.split('\n').count() as i64)
    } else { json!(0) });
    lane.insert("dirty_stat".into(), json!(if dirty_stat.ok { dirty_stat.out.split('\n').last().unwrap_or("").to_string() } else { String::new() }));
    lane.insert("diffstat".into(), json!(if stat.ok { stat.out.split('\n').last().unwrap_or("").to_string() } else { String::new() }));
    lane
}

pub fn cockpitGitGraph(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `if (!db) throw err.contextMissing(...)` — unreachable in the db lane (db is always present).
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));
    let op = input.get("op").and_then(Value::as_str);
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = root_v.as_str().unwrap_or("");
    let root_truthy = !root.is_empty();

    if op == Some("open_worktree") {
        let pane_id = js_number(input.get("pane_id"));
        let rows = crate::db_json::query_rows(db, "SELECT id, worktree_path FROM cockpit_panes WHERE id = ?", &[num_param(pane_id)])?;
        let pane = rows.as_array().and_then(|a| a.first()).cloned();
        let wt = pane.as_ref().and_then(|p| p.get("worktree_path")).cloned().unwrap_or(Value::Null);
        if !truthy(Some(&wt)) || !std::path::Path::new(wt.as_str().unwrap_or("")).exists() {
            let mut out = Map::new();
            out.insert("ok".into(), json!(false));
            out.insert("error".into(), json!("no worktree on this pane"));
            return Ok(Value::Object(out));
        }
        // RISKY, real-but-untested (see module doc): fire-and-forget open of the OS file explorer.
        let wt_str = crate::engine_roots::native_reveal_path(wt.as_str().unwrap_or(""));
        let spawned = if cfg!(windows) {
            crate::proc::cmd("explorer").arg(&wt_str)
                .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()
        } else if cfg!(target_os = "macos") {
            crate::proc::cmd("open").arg(&wt_str)
                .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()
        } else {
            crate::proc::cmd("xdg-open").arg(&wt_str)
                .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()
        };
        let mut out = Map::new();
        match spawned {
            Ok(_) => {
                out.insert("ok".into(), json!(true));
                out.insert("opened".into(), wt.clone());
            }
            Err(e) => {
                out.insert("ok".into(), json!(false));
                out.insert("error".into(), json!(e.to_string()));
            }
        }
        return Ok(Value::Object(out));
    }

    if op == Some("close_info") {
        let pane_id = js_number(input.get("pane_id"));
        let rows = crate::db_json::query_rows(db, "SELECT * FROM cockpit_panes WHERE id = ?", &[num_param(pane_id)])?;
        let pane = rows.as_array().and_then(|a| a.first()).cloned();
        let Some(pane) = pane else {
            let mut out = Map::new();
            out.insert("ok".into(), json!(false));
            out.insert("error".into(), json!("pane not found"));
            return Ok(Value::Object(out));
        };
        if !root_truthy {
            let mut out = Map::new();
            out.insert("ok".into(), json!(false));
            out.insert("error".into(), json!("no git workspace"));
            return Ok(Value::Object(out));
        }
        // `{ ok: true, ...laneFor(root, pane) }` — see module doc's spread-overwrite QUIRK.
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        for (k, v) in laneFor(root, &pane) { out.insert(k, v); }
        return Ok(Value::Object(out));
    }

    // op='graph' (default / anything else) — RISKY, real-but-untested by fixture: unconditionally
    // shells to git for head/baseBranch regardless of pane count (see module doc).
    let session_id = js_number(input.get("session_id"));
    if !truthy_num(session_id) {
        return Err("cockpit-git-graph: session_id is required".into());
    }
    if !root_truthy {
        let mut out = Map::new();
        out.insert("ok".into(), json!(false));
        out.insert("error".into(), json!("no git workspace mounted"));
        return Ok(Value::Object(out));
    }
    let head = git_run(&["log", "--oneline", "-1"], root);
    let base_branch_res = git_run(&["rev-parse", "--abbrev-ref", "HEAD"], root);
    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, idx, subagent_name, branch, worktree_path FROM cockpit_panes WHERE session_id = ? AND worktree_path IS NOT NULL ORDER BY idx",
        &[num_param(session_id)],
    )?;
    let panes = rows.as_array().cloned().unwrap_or_default();

    let mut base = Map::new();
    base.insert("branch".into(), json!(if base_branch_res.ok { base_branch_res.out.clone() } else { "?".to_string() }));
    base.insert("head".into(), json!(if head.ok { head.out.clone() } else { "?".to_string() }));
    let lanes: Vec<Value> = panes.iter().map(|p| Value::Object(laneFor(root, p))).collect();

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("base".into(), Value::Object(base));
    out.insert("lanes".into(), json!(lanes));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, \
             idx INTEGER NOT NULL, subagent_name TEXT NOT NULL DEFAULT '', scope_json TEXT NOT NULL DEFAULT '[]', \
             grants_json TEXT NOT NULL DEFAULT '[]', worktree_path TEXT, branch TEXT, tier TEXT NOT NULL DEFAULT 'low', \
             base_agent TEXT, extra_prompt_md TEXT, model_id TEXT, calls INTEGER NOT NULL DEFAULT 0, \
             cost_usd REAL NOT NULL DEFAULT 0, status TEXT NOT NULL DEFAULT 'idle', last_activity TEXT, \
             UNIQUE (session_id, idx))",
        ).unwrap();
        c
    }

    #[test]
    fn graph_session_id_required_guard() {
        let c = fresh();
        assert_eq!(cockpitGitGraph(&json!({}), &c), Err("cockpit-git-graph: session_id is required".to_string()));
        assert_eq!(cockpitGitGraph(&json!({ "session_id": 0 }), &c), Err("cockpit-git-graph: session_id is required".to_string()));
    }

    #[test]
    fn graph_no_matching_panes_returns_empty_lanes() {
        let c = fresh();
        let r = cockpitGitGraph(&json!({ "op": "graph", "session_id": 1 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["lanes"], json!([]));
    }

    #[test]
    fn graph_pane_with_missing_worktree_reports_worktree_missing() {
        let c = fresh();
        c.execute(
            "INSERT INTO cockpit_panes (id, session_id, idx, subagent_name, worktree_path, branch) VALUES (11, 1, 0, 'dev', 'C:/does/not/exist', 'feature')",
            [],
        ).unwrap();
        let r = cockpitGitGraph(&json!({ "op": "graph", "session_id": 1 }), &c).unwrap();
        assert_eq!(
            r["lanes"],
            json!([{ "pane_id": 11, "idx": 0, "name": "dev", "branch": "feature", "worktree_path": "C:/does/not/exist", "ok": false, "reason": "worktree missing" }])
        );
    }

    #[test]
    fn close_info_pane_not_found() {
        let c = fresh();
        let r = cockpitGitGraph(&json!({ "op": "close_info", "pane_id": 99 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "pane not found" }));
    }

    #[test]
    fn close_info_null_worktree_path_reports_worktree_missing_ok_false() {
        // JS-verified QUIRK: `{ ok: true, ...laneFor(...) }` ends up `ok: false` because laneFor's
        // own `ok` overwrites the outer literal's value (spread is last-write-wins on value).
        let c = fresh();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx, subagent_name, branch) VALUES (1, 1, 0, 'dev', 'antiloki/cockpit/1/0')", []).unwrap();
        let r = cockpitGitGraph(&json!({ "op": "close_info", "pane_id": 1 }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "ok": false, "pane_id": 1, "idx": 0, "name": "dev", "branch": "antiloki/cockpit/1/0", "worktree_path": null, "reason": "worktree missing" })
        );
    }

    #[test]
    fn close_info_empty_string_worktree_path_also_missing() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx, subagent_name, worktree_path) VALUES (1, 1, 0, 'dev', '')", []).unwrap();
        let r = cockpitGitGraph(&json!({ "op": "close_info", "pane_id": 1 }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["reason"], json!("worktree missing"));
    }

    #[test]
    fn open_worktree_pane_not_found() {
        let c = fresh();
        let r = cockpitGitGraph(&json!({ "op": "open_worktree", "pane_id": 99 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "no worktree on this pane" }));
    }

    #[test]
    fn open_worktree_null_worktree_path() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx, subagent_name) VALUES (1, 1, 0, 'dev')", []).unwrap();
        let r = cockpitGitGraph(&json!({ "op": "open_worktree", "pane_id": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "no worktree on this pane" }));
    }

    #[test]
    fn open_worktree_path_does_not_exist_on_disk() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx, subagent_name, worktree_path) VALUES (1, 1, 0, 'dev', 'C:/does/not/exist/anywhere')", []).unwrap();
        let r = cockpitGitGraph(&json!({ "op": "open_worktree", "pane_id": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "no worktree on this pane" }));
    }
}
