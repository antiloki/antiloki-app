//! cockpit-pane-diff.rs — twin of skills/cockpit/cockpit-pane-diff.mjs (async default export
//! `cockpitPaneDiff(input, context)`, now sync per the async-lane convention — the JS only
//! `await`s nothing (it isn't actually async-dependent; the `async` keyword is vestigial), so the
//! resolved value is what this twin returns directly). NAMES ARE CONTRACT: `cockpitPaneDiff`.
//! EPIC-VIBE-COCKPIT: review what a pane's agent actually changed in its worktree.
//!   mode='diff'    → stage all + return the patch (incl. new files) + changed-file count.
//!   mode='commit'  → stage all + commit on the pane's branch.
//!   mode='discard' → reset the worktree to HEAD + remove untracked (destructive to worktree only).
//!
//! GIT SUBPROCESS — DISCLOSED UNTESTED (playbook lesson 5's flag-skippable pattern, same shape as
//! constitution-estimate.rs's `git_diff_paths`): the JS `git()` (`spawnSync('git', args, {cwd,...})`)
//! is called on EVERY success path that reaches a provisioned worktree (all three modes), but that
//! whole cluster is reached ONLY when `pane.worktree_path` is truthy — an optional column whose
//! ABSENCE is itself a normal, deterministic early-return (`{ ok:false, error:'worktree not
//! provisioned' }`). This twin ports `git()` as a real `git_run` subprocess wrapper (mirrors
//! `spawnSync` field-for-field: stdout/stderr/status) and writes real logic for all three modes, but
//! the fixture ONLY exercises the two pane-lookup early returns (not-found / not-provisioned) —
//! real git repo/worktree state is not deterministically fixture-controllable through the SQL-only
//! conform-db harness. Minor disclosed gap: Node's `spawnSync` never throws for a missing `git`
//! binary (it populates `result.error`/leaves `stdout` null instead) — this twin's `git_run`
//! mirrors that non-throwing shape (Err from `Command::output()` collapses to an empty/status=-1
//! result, matching `(git(...).stdout || '')`'s fallback), so the JS `try {...} catch` wrapper around
//! the git calls is realistically dead code in both lanes and is not separately modeled here.
//!
//! `context.db` missing → JS throws `err.contextMissing('cockpit-pane-diff','db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention every sibling
//! db-lane twin uses). `pane_id` missing/falsy (`Number(input?.pane_id)` NaN or 0) → JS throws
//! `err.required('cockpit-pane-diff','pane_id')` (`"cockpit-pane-diff: pane_id is required"`) — the
//! one reachable throw path, modeled as `Err` (guard-throws are not fixture-tested per gotcha #9).
// QUALITY: 74/100 — real git subprocess ported for all 3 modes; fixture covers only the two
// git-free early-return paths (pane not found / worktree not provisioned), disclosed above.

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

// JS `Number.prototype.toString()` for a raw f64 embedded directly in a template literal (not a
// JSON-sourced Value::Number). paneId is `Number(input?.pane_id)` — always finite/non-NaN/non-zero
// by the time this is used (guarded above), so the fixed-notation path is what's ever exercised.
fn js_number_to_string(f: f64) -> String {
    if f.is_nan() { return "NaN".to_string(); }
    if f == 0.0 { return "0".to_string(); }
    let neg = f.is_sign_negative();
    let abs = f.abs();
    if abs.is_infinite() { return if neg { "-Infinity".into() } else { "Infinity".into() }; }
    let sci = format!("{:e}", abs);
    let (mantissa, exp_str) = sci.split_once('e').unwrap();
    let exp: i32 = exp_str.parse().unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let body = if (-6..=20).contains(&exp) {
        if exp >= 0 {
            if exp + 1 >= k { format!("{digits}{}", "0".repeat((exp + 1 - k) as usize)) }
            else { let (i, f) = digits.split_at((exp + 1) as usize); format!("{i}.{f}") }
        } else { format!("0.{}{digits}", "0".repeat((-exp - 1) as usize)) }
    } else {
        let m = if k == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{m}e{}{}", if exp >= 0 { "+" } else { "-" }, exp.abs())
    };
    if neg { format!("-{body}") } else { body }
}

struct GitOut { stdout: String, stderr: String, status: i32 }

/// `const git = (args, cwd) => spawnSync('git', args, { cwd, encoding: 'utf8', maxBuffer: 8*1024*1024 })`
/// — never throws (see module doc); a failed spawn (e.g. `git` missing from PATH) collapses to an
/// empty/status=-1 result, matching JS's `result.stdout` being `null` in that case.
fn git_run(args: &[&str], cwd: &str) -> GitOut {
    match crate::proc::cmd("git").args(args).current_dir(cwd).output() {
        Ok(o) => GitOut {
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            status: o.status.code().unwrap_or(-1),
        },
        Err(_) => GitOut { stdout: String::new(), stderr: String::new(), status: -1 },
    }
}

/// `.slice(0, n)` on a string, JS UTF-16 code-unit semantics approximated via chars (gotcha #5;
/// fixtures/testdata for the git branches stay ASCII/BMP in practice — this branch carries no
/// fixture coverage either way, see module doc).
fn js_slice0_str(s: &str, n: usize) -> String { s.chars().take(n).collect() }

pub fn cockpitPaneDiff(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `if (!db) throw err.contextMissing(...)` — unreachable in the db lane (db is always present).
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let pane_id_num = js_number(input.get("pane_id"));
    if !truthy_num(pane_id_num) {
        return Err("cockpit-pane-diff: pane_id is required".into());
    }

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, idx, subagent_name, worktree_path, session_id, branch FROM cockpit_panes WHERE id = ?",
        &[num_param(pane_id_num)],
    )?;
    let pane = rows.as_array().and_then(|a| a.first()).cloned();

    let Some(pane) = pane else {
        let mut out = Map::new();
        out.insert("ok".into(), json!(false));
        out.insert("error".into(), json!(format!("pane {} not found", js_number_to_string(pane_id_num))));
        return Ok(Value::Object(out));
    };

    let worktree_path = pane.get("worktree_path").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&worktree_path)) {
        let mut out = Map::new();
        out.insert("ok".into(), json!(false));
        out.insert("error".into(), json!("worktree not provisioned"));
        return Ok(Value::Object(out));
    }
    let wt = worktree_path.as_str().unwrap_or_default();

    let mode: &str = match input.get("mode").and_then(Value::as_str) {
        Some(m) if m == "diff" || m == "commit" || m == "discard" || m == "status" => m,
        _ => "diff",
    };

    // ── mode='status' (2026-09-16, "closing an agent should check for changes") ─────────────
    // What the box holds that base does not: uncommitted files (nothing staged by this call) and
    // commits the pane's branch is ahead by. Read-only — the close dialog decides merge / discard.
    if mode == "status" {
        let porcelain = git_run(&["status", "--porcelain"], wt);
        let files: Vec<String> = porcelain.stdout.lines().filter(|l| l.len() > 3).map(|l| l[3..].trim().to_string()).collect();
        let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
        let root = root_v.as_str().unwrap_or("").to_string();
        let session_base: String = pane.get("session_id").and_then(Value::as_i64)
            .and_then(|sid| crate::db_json::query_rows(db, "SELECT base_branch FROM cockpit_sessions WHERE id = ?", &[json!(sid)]).ok())
            .and_then(|r| r.as_array().and_then(|a| a.first()).and_then(|s| s.get("base_branch")).and_then(Value::as_str).map(String::from))
            .unwrap_or_default();
        let base = if !session_base.is_empty() { session_base } else if !root.is_empty() { git_run(&["rev-parse", "--abbrev-ref", "HEAD"], &root).stdout.trim().to_string() } else { String::new() };
        let ahead: i64 = if base.is_empty() { 0 } else { git_run(&["rev-list", "--count", &format!("{base}..HEAD")], wt).stdout.trim().parse().unwrap_or(0) };
        let ahead_files: Vec<String> = if base.is_empty() || ahead == 0 { Vec::new() } else { git_run(&["diff", "--name-only", &format!("{base}...HEAD")], wt).stdout.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect() };
        // antiloki's own files never count as the agent's work
        let ours = |f: &String| f.starts_with(".antiloki/") || f.starts_with(".claude/") || f.starts_with(".codex/") || f.starts_with(".opencode/") || f.starts_with(".agents/") || f == "AGENTS.md" || f == "CLAUDE.md" || f == "opencode.json";
        let work: Vec<String> = files.iter().filter(|f| !ours(f)).cloned().collect();
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("mode".into(), json!("status"));
        out.insert("base".into(), json!(base));
        out.insert("branch".into(), pane.get("branch").cloned().unwrap_or(Value::Null));
        out.insert("uncommitted".into(), json!(work.len() as i64));
        out.insert("files".into(), json!(work));
        out.insert("ahead".into(), json!(ahead));
        out.insert("ahead_files".into(), json!(ahead_files));
        out.insert("dirty".into(), json!(!work.is_empty() || ahead > 0));
        return Ok(Value::Object(out));
    }

    // ── mode='discard' ───────────────────────────────────────────────────
    if mode == "discard" {
        git_run(&["checkout", "--", "."], wt);
        git_run(&["clean", "-fd"], wt);
        crate::db_json::run_write(
            db,
            &format!("UPDATE cockpit_panes SET status = 'idle', last_activity = {} WHERE id = ?", crate::clock::now_sql()),
            &[num_param(pane_id_num)],
        )?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("mode".into(), json!("discard"));
        out.insert("discarded".into(), json!(true));
        return Ok(Value::Object(out));
    }

    git_run(&["add", "-A"], wt);
    let name_out = git_run(&["diff", "--cached", "--name-only"], wt);
    let names: Vec<String> = name_out.stdout.split('\n').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();

    // ── mode='commit' ────────────────────────────────────────────────────
    if mode == "commit" {
        if names.is_empty() {
            let mut out = Map::new();
            out.insert("ok".into(), json!(true));
            out.insert("mode".into(), json!("commit"));
            out.insert("committed".into(), json!(false));
            out.insert("changed".into(), json!(0));
            out.insert("note".into(), json!("nothing to commit"));
            return Ok(Value::Object(out));
        }
        let default_msg = format!("cockpit pane {} ({})", tmpl(pane.get("idx")), tmpl(pane.get("subagent_name")));
        let msg_src = if truthy(input.get("message")) { input.get("message").unwrap().clone() } else { json!(default_msg) };
        let msg = js_slice0_str(&js_string(&msg_src), 200);
        let c = git_run(&["commit", "-m", &msg], wt);
        let committed = c.status == 0;
        let note: Value = if committed {
            Value::Null
        } else {
            let raw = if !c.stderr.is_empty() { c.stderr } else if !c.stdout.is_empty() { c.stdout } else { String::new() };
            json!(js_slice0_str(&raw, 160))
        };
        let mut out = Map::new();
        out.insert("ok".into(), json!(committed));
        out.insert("mode".into(), json!("commit"));
        out.insert("committed".into(), json!(committed));
        out.insert("changed".into(), json!(names.len() as i64));
        out.insert("note".into(), note);
        return Ok(Value::Object(out));
    }

    // ── mode='diff' (default) ────────────────────────────────────────────
    let diff_out = git_run(&["diff", "--cached"], wt);
    let patch = js_slice0_str(&diff_out.stdout, 200000);
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("mode".into(), json!("diff"));
    out.insert("changed".into(), json!(names.len() as i64));
    out.insert("patch".into(), if patch.is_empty() { json!("(no changes in this pane's worktree)") } else { json!(patch) });
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
    fn pane_not_found() {
        let c = fresh();
        let r = cockpitPaneDiff(&json!({ "pane_id": 99 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "pane 99 not found" }));
    }

    #[test]
    fn worktree_not_provisioned() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx, subagent_name) VALUES (1, 1, 0, 'dev')", []).unwrap();
        let r = cockpitPaneDiff(&json!({ "pane_id": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "worktree not provisioned" }));
    }

    #[test]
    fn worktree_empty_string_is_also_not_provisioned() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx, subagent_name, worktree_path) VALUES (1, 1, 0, 'dev', '')", []).unwrap();
        let r = cockpitPaneDiff(&json!({ "pane_id": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "worktree not provisioned" }));
    }

    #[test]
    fn pane_id_required_guard() {
        let c = fresh();
        assert_eq!(cockpitPaneDiff(&json!({}), &c), Err("cockpit-pane-diff: pane_id is required".to_string()));
        assert_eq!(cockpitPaneDiff(&json!({ "pane_id": 0 }), &c), Err("cockpit-pane-diff: pane_id is required".to_string()));
    }

    fn git(args: &[&str], cwd: &std::path::Path) {
        let out = crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn status_mode_reports_uncommitted_and_ahead_without_touching_the_tree() {
        let root = std::env::temp_dir().join(format!("antiloki-pane-diff-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        git(&["init", "-q", "-b", "main"], &root);
        git(&["config", "user.email", "t@t"], &root); git(&["config", "user.name", "t"], &root);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        git(&["add", "a.txt"], &root); git(&["commit", "-q", "-m", "init"], &root);
        let wt = root.join("box");
        git(&["worktree", "add", "-q", "-b", "pane-1", wt.to_str().unwrap()], &root);
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute_batch("CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')").unwrap();
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)", [root.to_string_lossy().to_string()]).unwrap();
        c.execute("INSERT INTO cockpit_sessions (id, name, base_branch) VALUES (1, 's', 'main')", []).unwrap();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx, branch, worktree_path) VALUES (1, 1, 0, 'pane-1', ?1)", [wt.to_string_lossy().to_string()]).unwrap();
        // clean box
        let r = cockpitPaneDiff(&json!({ "pane_id": 1, "mode": "status" }), &c).unwrap();
        assert_eq!(r["dirty"], json!(false));
        assert_eq!(r["base"], json!("main"));
        // a commit ahead + an uncommitted file + antiloki's own files (never counted)
        std::fs::write(wt.join("b.txt"), "b\n").unwrap();
        git(&["add", "b.txt"], &wt); git(&["commit", "-q", "-m", "work"], &wt);
        std::fs::write(wt.join("c.txt"), "c\n").unwrap();
        std::fs::create_dir_all(wt.join(".antiloki")).unwrap();
        std::fs::write(wt.join(".antiloki/x"), "x").unwrap();
        std::fs::write(wt.join("AGENTS.md"), "brief").unwrap();
        let r = cockpitPaneDiff(&json!({ "pane_id": 1, "mode": "status" }), &c).unwrap();
        assert_eq!(r["ahead"], json!(1));
        assert_eq!(r["ahead_files"], json!(["b.txt"]));
        assert_eq!(r["uncommitted"], json!(1));
        assert_eq!(r["files"], json!(["c.txt"]));
        assert_eq!(r["dirty"], json!(true));
        // read-only: nothing got staged
        let staged = crate::proc::cmd("git").args(["diff", "--cached", "--name-only"]).current_dir(&wt).output().unwrap();
        assert!(String::from_utf8_lossy(&staged.stdout).trim().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
