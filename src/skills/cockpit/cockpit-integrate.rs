//! cockpit-integrate.rs — twin of skills/cockpit/cockpit-integrate.mjs (async default export
//! `cockpitIntegrate(input, context)`, now sync per the async-lane convention — the JS only
//! `await`s sync SQLite reads/writes through `context.db`, so the resolved value is what this
//! twin returns directly). NAMES ARE CONTRACT: `cockpitIntegrate`. EPIC-VIBE-COCKPIT P4: fold the
//! panes back toward base — commit each pane's worktree, then run a NON-destructive conflict
//! preview against base via `git merge-tree` (no working tree touched, no auto-resolve).
//!
//! GIT SUBPROCESS — DISCLOSED UNTESTED BY THE FIXTURE (playbook lesson 5's flag-skippable
//! pattern, same shape as cockpit-pane-diff.rs's `git_run`): the JS `git()`
//! (`spawnSync('git', args, {cwd, encoding:'utf8'})`) is called once per pane in the `panes`
//! result set, but the SQL that builds that set already filters to `worktree_path IS NOT NULL`
//! — a session with zero such panes (or whose `pane_ids` filter matches none) makes the loop body
//! — and therefore every `git_run` call — never execute, which is a normal, deterministic,
//! fixture-controllable condition (not a flag threaded through input). This twin ports `git()` as
//! a real `git_run` subprocess wrapper (mirrors `spawnSync` field-for-field: stdout/stderr/status,
//! never throws for a missing binary) and writes real logic for the full per-pane commit +
//! merge-tree-preview flow — VERIFIED against real git output via a full JS behavior test
//! (`cockpit-integrate.test.mjs`, incl. a genuine conflicting-merge scenario) — but the conform-db
//! fixture ONLY exercises sessions with zero matching panes: real git repo/worktree state is not
//! deterministically fixture-controllable through the SQL-only conform-db harness (same reasoning
//! as cockpit-pane-diff.rs). The JS `try {...} catch(e)` wrapping the whole per-pane git cluster is
//! not separately modeled either, for the same non-throwing-subprocess reason documented there.
//!
//! QUIRK, faithfully ported (verified against real `git merge-tree --write-tree --name-only`
//! output, git 2.55): on a conflict (`status===1`), stdout is `<tree-oid>\n<changed-file-list>\n
//! \n<human messages: "Auto-merging …" / "CONFLICT …">`. The parser takes everything AFTER THE
//! FIRST BLANK LINE — which lands on the human-readable Auto-merging/CONFLICT messages, NOT the
//! `--name-only` file list (which appears BEFORE the blank line). So `r.conflicts` ends up holding
//! prose lines like `"CONFLICT (content): Merge conflict in f.txt"`, not bare filenames. This is
//! the real JS behavior (confirmed in the JS test), not a re-derivation — ported verbatim.
//!
//! `root` resolution: JS wraps `getWorkspaceRoot(db)` in try/catch, but the already-converted
//! `engine_workspace_root::getWorkspaceRoot` twin never itself throws — a missing `workspaces`
//! table or no active row falls back to `SUBSTRATE_ROOT` rather than raising. So `root` is
//! realistically ALWAYS a non-empty string; the JS `if (!root)` per-pane "no workspace root
//! resolvable" branch (and this twin's mirror of it) is dead code on both sides — ported for
//! fidelity, not separately fixture-tested.
//!
//! `context.db` missing → JS throws `err.contextMissing('cockpit-integrate','db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention every sibling
//! db-lane twin uses). `session_id` missing/falsy (`Number(input?.session_id)` NaN or 0) → JS
//! throws `err.required('cockpit-integrate','session_id')` — the one reachable throw path,
//! modeled as `Err` (guard-throws are not fixture-tested per playbook gotcha #9).
// QUALITY: 78/100 — real git subprocess ported for the full per-pane flow (verified via JS test
// incl. a real conflicting merge), fixture covers only the git-free zero-panes paths (disclosed above)

use rusqlite::Connection;
use serde_json::{json, Map, Value};

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
// JS Number → JSON output: an integer-valued number renders without a decimal point.
fn num_json(f: f64) -> Value { if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.007199254740992e15 { json!(f as i64) } else { json!(f) } }

// JS `Number.prototype.toString()` for a raw f64 embedded in a template literal (not a
// JSON-sourced Value::Number). `sessionId` is always finite/non-NaN/non-zero by the time this is
// used (guarded above), so the fixed-notation path is what's ever exercised.
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

/// `const git = (args, cwd) => spawnSync('git', args, { cwd, encoding: 'utf8' })` — never throws
/// (see module doc); a failed spawn (e.g. `git` missing from PATH) collapses to an empty/status=-1
/// result, matching JS's `result.stdout` being `null` in that case.
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

/// `.slice(0, n)` on a string, JS UTF-16 code-unit semantics approximated via chars (gotcha #5).
fn js_slice0_str(s: &str, n: usize) -> String { s.chars().take(n).collect() }

pub fn cockpitIntegrate(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `if (!db) throw err.contextMissing(...)` — unreachable in the db lane (db is always present).
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let session_id_num = js_number(input.get("session_id"));
    if !truthy_num(session_id_num) {
        return Err("cockpit-integrate: session_id is required".into());
    }

    let session_rows = crate::db_json::query_rows(
        db,
        "SELECT id, base_branch FROM cockpit_sessions WHERE id = ?",
        &[num_param(session_id_num)],
    )?;
    let session = session_rows.as_array().and_then(|a| a.first()).cloned();
    let Some(session) = session else {
        let mut out = Map::new();
        out.insert("ok".into(), json!(false));
        out.insert("error".into(), json!(format!("session {} not found", js_number_to_string(session_id_num))));
        return Ok(Value::Object(out));
    };

    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root_str = root_v.as_str().unwrap_or("").to_string();

    // JS `session.base_branch || ''`.
    let base = match session.get("base_branch") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => String::new(),
    };

    // JS `Array.isArray(input?.pane_ids) ? input.pane_ids.map(Number) : null`.
    let only: Option<Vec<f64>> = input.get("pane_ids").and_then(Value::as_array)
        .map(|arr| arr.iter().map(|x| js_number(Some(x))).collect());

    let pane_rows = crate::db_json::query_rows(
        db,
        "SELECT * FROM cockpit_panes WHERE session_id = ? AND worktree_path IS NOT NULL ORDER BY idx",
        &[num_param(session_id_num)],
    )?;
    let panes: Vec<Value> = pane_rows.as_array().cloned().unwrap_or_default().into_iter()
        .filter(|p| {
            let pid = p.get("id").and_then(Value::as_f64).unwrap_or(f64::NAN);
            only.as_ref().map_or(true, |o| o.iter().any(|n| *n == pid))
        })
        .collect();

    let _ = crate::db_json::run_write(
        db,
        &format!("UPDATE cockpit_sessions SET status = 'integrating', updated_at = {} WHERE id = ?", crate::clock::now_sql()),
        &[num_param(session_id_num)],
    )?;

    let mut results: Vec<Value> = Vec::new();
    for p in &panes {
        let mut r = Map::new();
        r.insert("pane_id".into(), p.get("id").cloned().unwrap_or(Value::Null));
        r.insert("idx".into(), p.get("idx").cloned().unwrap_or(Value::Null));
        r.insert("subagent".into(), p.get("subagent_name").cloned().unwrap_or(Value::Null));
        r.insert("committed".into(), json!(false));
        r.insert("mergeable".into(), Value::Null);
        r.insert("conflicts".into(), json!([]));
        r.insert("note".into(), Value::Null);

        if root_str.is_empty() {
            r.insert("note".into(), json!("no workspace root resolvable"));
            results.push(Value::Object(r));
            continue;
        }

        // worktree_path is guaranteed non-null by the SQL WHERE clause.
        let worktree_path = p.get("worktree_path").and_then(Value::as_str).unwrap_or("");

        // 1. commit the pane's worktree if it has changes (safe — operates only inside the worktree)
        git_run(&["add", "-A"], worktree_path);
        let st = git_run(&["status", "--porcelain"], worktree_path);
        if !st.stdout.trim().is_empty() {
            let msg = format!("cockpit pane {} ({})", tmpl(p.get("idx")), tmpl(p.get("subagent_name")));
            let c = git_run(&["commit", "-m", &msg], worktree_path);
            r.insert("committed".into(), json!(c.status == 0));
        }

        // 2. non-destructive conflict preview vs base (no working tree touched)
        let branch = p.get("branch").and_then(Value::as_str).unwrap_or("");
        if !base.is_empty() && !branch.is_empty() {
            let mt = git_run(&["merge-tree", "--write-tree", "--name-only", &base, branch], &root_str);
            if mt.status == 0 {
                r.insert("mergeable".into(), json!(true));
            } else if mt.status == 1 {
                r.insert("mergeable".into(), json!(false));
                let lines: Vec<&str> = mt.stdout.split('\n').collect();
                let blank = lines.iter().position(|l| l.is_empty());
                let conflicts: Vec<Value> = match blank {
                    Some(bi) => lines[bi + 1..].iter().filter(|l| !l.is_empty()).map(|l| json!(*l)).collect(),
                    None => Vec::new(),
                };
                r.insert("conflicts".into(), Value::Array(conflicts));
            } else {
                r.insert("note".into(), json!(format!("merge-tree unavailable: {}", js_slice0_str(&mt.stderr, 120))));
            }
        } else {
            r.insert("note".into(), json!("missing base or pane branch"));
        }

        results.push(Value::Object(r));
    }

    let _ = crate::db_json::run_write(
        db,
        &format!("UPDATE cockpit_sessions SET status = 'active', updated_at = {} WHERE id = ?", crate::clock::now_sql()),
        &[num_param(session_id_num)],
    )?;

    let clean = results.iter().filter(|r| r.get("mergeable") == Some(&json!(true))).count();
    let conflicted = results.iter().filter(|r| r.get("mergeable") == Some(&json!(false))).count();
    let total = results.len();

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("session_id".into(), num_json(session_id_num));
    out.insert("base_branch".into(), json!(base));
    out.insert("panes".into(), Value::Array(results));
    let mut summary = Map::new();
    summary.insert("clean".into(), json!(clean as i64));
    summary.insert("conflicted".into(), json!(conflicted as i64));
    summary.insert("total".into(), json!(total as i64));
    out.insert("summary".into(), Value::Object(summary));
    out.insert(
        "note".into(),
        json!("committed each pane + previewed conflicts; the final forward-merge into base is operator-driven (the main tree may be dirty)"),
    );
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cockpit_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, \
             status TEXT NOT NULL DEFAULT 'active', mission_md TEXT, base_branch TEXT NOT NULL DEFAULT '', \
             base_sha TEXT NOT NULL DEFAULT '', mode TEXT NOT NULL DEFAULT 'steer', created_at TEXT, updated_at TEXT); \
             CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, \
             idx INTEGER NOT NULL, subagent_name TEXT NOT NULL DEFAULT '', scope_json TEXT NOT NULL DEFAULT '[]', \
             grants_json TEXT NOT NULL DEFAULT '[]', worktree_path TEXT, branch TEXT, tier TEXT NOT NULL DEFAULT 'low', \
             base_agent TEXT, extra_prompt_md TEXT, model_id TEXT, calls INTEGER NOT NULL DEFAULT 0, \
             cost_usd REAL NOT NULL DEFAULT 0, status TEXT NOT NULL DEFAULT 'idle', last_activity TEXT, \
             UNIQUE (session_id, idx))",
        ).unwrap();
        c
    }

    #[test]
    fn session_not_found() {
        let c = fresh();
        let r = cockpitIntegrate(&json!({ "session_id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "session 999 not found" }));
    }

    #[test]
    fn session_id_required_guard() {
        let c = fresh();
        assert_eq!(cockpitIntegrate(&json!({}), &c), Err("cockpit-integrate: session_id is required".to_string()));
        assert_eq!(cockpitIntegrate(&json!({ "session_id": 0 }), &c), Err("cockpit-integrate: session_id is required".to_string()));
    }

    #[test]
    fn zero_worktree_provisioned_panes_flips_status_back_to_active() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_sessions (id, name, base_branch) VALUES (1, 'alpha', 'main')", []).unwrap();
        c.execute("INSERT INTO cockpit_panes (id, session_id, idx, subagent_name) VALUES (5, 1, 0, 'dev')", []).unwrap();
        let r = cockpitIntegrate(&json!({ "session_id": 1 }), &c).unwrap();
        assert_eq!(
            r,
            json!({
                "ok": true, "session_id": 1, "base_branch": "main", "panes": [],
                "summary": { "clean": 0, "conflicted": 0, "total": 0 },
                "note": "committed each pane + previewed conflicts; the final forward-merge into base is operator-driven (the main tree may be dirty)",
            })
        );
        let row = crate::db_json::query_rows(&c, "SELECT status FROM cockpit_sessions WHERE id = 1", &[]).unwrap();
        assert_eq!(row[0]["status"], json!("active"));
    }

    #[test]
    fn pane_ids_filters_out_non_matching_panes() {
        let c = fresh();
        c.execute("INSERT INTO cockpit_sessions (id, name, base_branch) VALUES (2, 'beta', 'main')", []).unwrap();
        c.execute(
            "INSERT INTO cockpit_panes (id, session_id, idx, subagent_name, worktree_path, branch) VALUES (7, 2, 0, 'ui', 'C:/somewhere', 'feature')",
            [],
        ).unwrap();
        let r = cockpitIntegrate(&json!({ "session_id": 2, "pane_ids": [999] }), &c).unwrap();
        assert_eq!(r["panes"], json!([]));
        assert_eq!(r["summary"], json!({ "clean": 0, "conflicted": 0, "total": 0 }));
    }
}
