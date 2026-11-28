//! cockpit-session-close.rs — twin of skills/cockpit/cockpit-session-close.mjs (async default →
//! sync twin). NAMES ARE CONTRACT: `cockpitSessionClose(input, context)`. Close a vibe session:
//! mark it merged/abandoned/archived and best-effort tear down the panes' worktrees.
//!
//! BESPOKE-NATIVE (playbook's VET rule), same reasoning as the already-converted sibling
//! `agents_git_manager_git_manager_worktree_ops::worktreeRemove` this skill calls unconditionally
//! (per pane, when `input.cleanup !== false`, which is the DEFAULT): that call shells out to a
//! REAL `git` subprocess against a REAL filesystem path under `WORKTREES_DIR`
//! (`$ANTILOKI_HOME`-derived, not DB-derived), and — critically — `git worktree remove` is
//! DESTRUCTIVE and non-idempotent against real on-disk state. conform-db's fixture model only
//! captures DB rows (schema/seed/db_after); it has no seam to create/reset the real worktree
//! directory `worktreeRemove` inspects, and re-running the SAME fixture through both lanes would
//! mutate shared real git state on the second lane's turn (first lane's removal makes the second
//! lane's `existsSync` check diverge), which is exactly the "real subprocess/filesystem work
//! outside the fixture model" blocker the playbook calls out. `worktreeRemove` itself already
//! carries zero fixture/frontier/dispatch-arm entries for this exact reason (see that module's
//! doc comment) — this caller inherits the same unfixturable shape rather than working around it
//! by only ever exercising `cleanup:false` in a fixture (which would silently never test the
//! skill's default/real behavior).
//!
//! Proven instead via `#[cfg(test)]` native tests: DB-only branches (guards, not-found, status/
//! mode transitions, the pane-status UPDATE's `status != 'error'` filter, the `cleanup:false`
//! skip) against an in-memory `Connection`; the real-worktree-removal branch against a temp git
//! repo + `$ANTILOKI_HOME` override, mirroring `git-manager-worktree-ops.rs`'s test harness
//! (`with_home`/`make_temp_git_repo`/`db_with_workspace`, reusing the shared `test_env_lock`). No
//! fixture JSON, no frontier entry, no dispatch arm.
//!
//! `Number(input?.session_id)` + `if (!sessionId) throw` → `js_number` + the "0 or NaN" falsy
//! check, same convention as `cockpit-session-audit.rs`/`cockpit-git-graph.rs`. `status` derives
//! from `input.mode` via a plain string match (`'merged'` / `'archived'` / else `'abandoned'`) —
//! no `??`/truthy subtlety. The removal loop's `card_slug` is COMPUTED as
//! `` `cockpit-${sessionId}-${idx}` `` — it does NOT read the pane's stored `worktree_path`
//! column; that column only gates the loop's `continue` (skip panes with no worktree_path at
//! all), matching the JS verbatim (`worktree_path` from the SELECT is otherwise unused past the
//! guard). `force: status === 'abandoned'` is passed straight through to `worktreeRemove`.
//! `datetime('now')` in the session UPDATE is pinned to the frozen instant per the playbook's
//! datetime rule (this fn does mutate real DB state, unlike the worktree side effect).
// QUALITY: 76/100 — DB logic is straightforward CRUD; the interesting/risky part (best-effort
// real worktree teardown) is proven by native tests only, same class as its worktree-ops dependency.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }

// = JS Number(v)
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

// JS `${v}` for a Number: whole-valued finite numbers render without a decimal point.
fn num_tmpl(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 { format!("{}", n as i64) } else { n.to_string() }
}

// node:sqlite binds an integer-valued JS number as INTEGER, else REAL — mirrors that bind type.
fn id_param(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 { json!(n as i64) } else { json!(n) }
}

fn js_num_json(n: f64) -> Value { if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 { json!(n as i64) } else { json!(n) } }

/// `export default async function cockpitSessionClose(input, context)`
pub fn cockpitSessionClose(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let session_id_n = js_number(input.get("session_id"));
    if session_id_n == 0.0 || session_id_n.is_nan() {
        return Err("cockpit-session-close: session_id is required".into());
    }
    let session_id_p = id_param(session_id_n);

    let session_rows = crate::db_json::query_rows(db, "SELECT id FROM cockpit_sessions WHERE id = ?", &[session_id_p.clone()])?;
    if session_rows.as_array().map(|a| a.is_empty()).unwrap_or(true) {
        let mut out = Map::new();
        out.insert("ok".into(), json!(false));
        out.insert("error".into(), json!(format!("session {} not found", num_tmpl(session_id_n))));
        return Ok(Value::Object(out));
    }

    let mode = input.get("mode").and_then(Value::as_str);
    let status = match mode {
        Some("merged") => "merged",
        Some("archived") => "archived",
        _ => "abandoned",
    };

    let mut removed: i64 = 0;
    if !matches!(input.get("cleanup"), Some(Value::Bool(false))) {
        let panes = crate::db_json::query_rows(db, "SELECT idx, worktree_path FROM cockpit_panes WHERE session_id = ?", &[session_id_p.clone()])?;
        if let Some(arr) = panes.as_array() {
            for p in arr {
                if !truthy(p.get("worktree_path")) {
                    continue;
                }
                let idx = p.get("idx").cloned().unwrap_or(Value::Null);
                let card_slug = format!("cockpit-{}-{}", num_tmpl(session_id_n), js_number_or_tmpl(&idx));
                let wt_input = json!({ "card_slug": card_slug, "force": status == "abandoned" });
                if crate::agents_git_manager_git_manager_worktree_ops::worktreeRemove(&wt_input, db).is_ok() {
                    removed += 1;
                }
            }
        }
    }

    db.execute(
        "UPDATE cockpit_sessions SET status = ?, updated_at = datetime('2026-01-01 00:00:00') WHERE id = ?",
        rusqlite::params![status, id_param_str(&session_id_p)],
    ).map_err(|e| e.to_string())?;
    if status != "archived" {
        db.execute(
            "UPDATE cockpit_panes SET status = 'merged' WHERE session_id = ? AND status != 'error'",
            rusqlite::params![id_param_str(&session_id_p)],
        ).map_err(|e| e.to_string())?;
    }

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("session_id".into(), js_num_json(session_id_n));
    out.insert("status".into(), json!(status));
    out.insert("worktrees_removed".into(), json!(removed));
    Ok(Value::Object(out))
}

// The pane `idx` column is INTEGER; render it the way JS template-literal interpolation would
// (`${p.idx}` — an integer Number renders without a decimal point).
fn js_number_or_tmpl(v: &Value) -> String {
    match v {
        Value::Number(n) => n.as_i64().map(|i| i.to_string()).unwrap_or_else(|| n.to_string()),
        other => num_tmpl(js_number(Some(other))),
    }
}

// rusqlite params! needs a concrete bind value; id_param already picked Int vs Real to mirror
// node:sqlite's bind type, so thread that same choice through as a rusqlite ToSql-able type.
fn id_param_str(v: &Value) -> rusqlite::types::Value {
    match v {
        Value::Number(n) => n.as_i64().map(rusqlite::types::Value::Integer).unwrap_or_else(|| rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0))),
        _ => rusqlite::types::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn seeded_schema_only() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c
    }

    #[test]
    fn missing_session_id_errors() {
        let c = seeded_schema_only();
        assert_eq!(cockpitSessionClose(&json!({}), &c), Err("cockpit-session-close: session_id is required".to_string()));
    }

    #[test]
    fn zero_session_id_errors() {
        let c = seeded_schema_only();
        assert_eq!(cockpitSessionClose(&json!({ "session_id": 0 }), &c), Err("cockpit-session-close: session_id is required".to_string()));
    }

    #[test]
    fn unknown_session_not_found() {
        let c = seeded_schema_only();
        let r = cockpitSessionClose(&json!({ "session_id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "session 999 not found" }));
    }

    #[test]
    fn default_mode_abandoned_updates_non_error_panes() {
        let c = seeded_schema_only();
        c.execute_batch(
            "INSERT INTO cockpit_sessions (id,name,status,created_at) VALUES (1,'alpha','active','2026-01-01 00:00:01'); \
             INSERT INTO cockpit_panes (id,session_id,idx,status) VALUES (10,1,0,'working'),(11,1,1,'error')",
        ).unwrap();
        let r = cockpitSessionClose(&json!({ "session_id": 1, "cleanup": false }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "session_id": 1, "status": "abandoned", "worktrees_removed": 0 }));
        let status: String = c.query_row("SELECT status FROM cockpit_sessions WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "abandoned");
        let s10: String = c.query_row("SELECT status FROM cockpit_panes WHERE id = 10", [], |r| r.get(0)).unwrap();
        let s11: String = c.query_row("SELECT status FROM cockpit_panes WHERE id = 11", [], |r| r.get(0)).unwrap();
        assert_eq!(s10, "merged");
        assert_eq!(s11, "error"); // untouched
    }

    #[test]
    fn mode_merged_updates_status_and_panes() {
        let c = seeded_schema_only();
        c.execute_batch(
            "INSERT INTO cockpit_sessions (id,name,status,created_at) VALUES (2,'beta','active','2026-01-01 00:00:01'); \
             INSERT INTO cockpit_panes (id,session_id,idx,status) VALUES (20,2,0,'idle')",
        ).unwrap();
        let r = cockpitSessionClose(&json!({ "session_id": 2, "mode": "merged", "cleanup": false }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "session_id": 2, "status": "merged", "worktrees_removed": 0 }));
        let s: String = c.query_row("SELECT status FROM cockpit_panes WHERE id = 20", [], |r| r.get(0)).unwrap();
        assert_eq!(s, "merged");
    }

    #[test]
    fn mode_archived_skips_pane_status_update() {
        let c = seeded_schema_only();
        c.execute_batch(
            "INSERT INTO cockpit_sessions (id,name,status,created_at) VALUES (3,'gamma','active','2026-01-01 00:00:01'); \
             INSERT INTO cockpit_panes (id,session_id,idx,status,worktree_path) VALUES (30,3,0,'working','/tmp/whatever')",
        ).unwrap();
        let r = cockpitSessionClose(&json!({ "session_id": 3, "mode": "archived", "cleanup": false }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "session_id": 3, "status": "archived", "worktrees_removed": 0 }));
        let s: String = c.query_row("SELECT status FROM cockpit_panes WHERE id = 30", [], |r| r.get(0)).unwrap();
        assert_eq!(s, "working"); // untouched
    }

    #[test]
    fn unrecognized_mode_falls_back_to_abandoned() {
        let c = seeded_schema_only();
        c.execute_batch("INSERT INTO cockpit_sessions (id,name,status,created_at) VALUES (4,'delta','active','2026-01-01 00:00:01')").unwrap();
        let r = cockpitSessionClose(&json!({ "session_id": 4, "mode": "bogus", "cleanup": false }), &c).unwrap();
        assert_eq!(r["status"], json!("abandoned"));
    }

    #[test]
    fn cleanup_false_skips_removal_even_with_worktree_path() {
        let c = seeded_schema_only();
        c.execute_batch(
            "INSERT INTO cockpit_sessions (id,name,status,created_at) VALUES (5,'epsilon','active','2026-01-01 00:00:01'); \
             INSERT INTO cockpit_panes (id,session_id,idx,status,worktree_path) VALUES (50,5,0,'working','/some/path')",
        ).unwrap();
        let r = cockpitSessionClose(&json!({ "session_id": 5, "cleanup": false }), &c).unwrap();
        assert_eq!(r["worktrees_removed"], json!(0));
    }

    #[test]
    fn panes_without_worktree_path_are_skipped() {
        let c = seeded_schema_only();
        c.execute_batch(
            "INSERT INTO cockpit_sessions (id,name,status,created_at) VALUES (6,'zeta','active','2026-01-01 00:00:01'); \
             INSERT INTO cockpit_panes (id,session_id,idx,status) VALUES (60,6,0,'working')",
        ).unwrap();
        let r = cockpitSessionClose(&json!({ "session_id": 6 }), &c).unwrap();
        assert_eq!(r["worktrees_removed"], json!(0));
    }

    // --- real-worktree integration, mirroring git-manager-worktree-ops.rs's test harness ---

    fn with_home<T>(antiloki_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_HOME", antiloki_home); }
        let result = f();
        unsafe { std::env::remove_var("ANTILOKI_HOME"); }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-cockpit-session-close-rs-{tag}-{}", std::process::id()));
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
        let c = seeded_schema_only();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        ).unwrap();
        c.execute(
            "INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)",
            rusqlite::params![root.to_string_lossy().into_owned()],
        ).unwrap();
        c
    }

    #[test]
    fn default_cleanup_removes_real_worktree_by_computed_slug() {
        let repo = make_temp_git_repo("close-real");
        let home = temp_dir("close-real-home");
        with_home(&home.to_string_lossy(), || {
            let c = db_with_workspace(&repo);
            c.execute_batch("INSERT INTO cockpit_sessions (id,name,status,created_at) VALUES (7,'eta','active','2026-01-01 00:00:01')").unwrap();
            let add_input = json!({ "card_slug": "cockpit-7-0" });
            crate::agents_git_manager_git_manager_worktree_ops::worktreeAdd(&add_input, &c).unwrap();
            let wt_path = home.join("worktrees").join("cockpit-7-0");
            assert!(wt_path.exists());
            c.execute(
                "INSERT INTO cockpit_panes (id,session_id,idx,status,worktree_path) VALUES (70,7,0,'working',?1)",
                rusqlite::params![wt_path.to_string_lossy().into_owned()],
            ).unwrap();
            let r = cockpitSessionClose(&json!({ "session_id": 7 }), &c).unwrap();
            assert_eq!(r["worktrees_removed"], json!(1));
            assert!(!wt_path.exists());
        });
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn dirty_worktree_removal_failure_is_swallowed_status_still_updates() {
        let repo = make_temp_git_repo("close-dirty");
        let home = temp_dir("close-dirty-home");
        with_home(&home.to_string_lossy(), || {
            let c = db_with_workspace(&repo);
            c.execute_batch("INSERT INTO cockpit_sessions (id,name,status,created_at) VALUES (8,'theta','active','2026-01-01 00:00:01')").unwrap();
            crate::agents_git_manager_git_manager_worktree_ops::worktreeAdd(&json!({ "card_slug": "cockpit-8-0" }), &c).unwrap();
            let wt_path = home.join("worktrees").join("cockpit-8-0");
            fs::write(wt_path.join("untracked.txt"), "x").unwrap();
            c.execute(
                "INSERT INTO cockpit_panes (id,session_id,idx,status,worktree_path) VALUES (80,8,0,'working',?1)",
                rusqlite::params![wt_path.to_string_lossy().into_owned()],
            ).unwrap();
            let r = cockpitSessionClose(&json!({ "session_id": 8, "mode": "merged" }), &c).unwrap();
            assert_eq!(r["status"], json!("merged"));
            assert_eq!(r["worktrees_removed"], json!(0));
            assert!(wt_path.exists());
            let status: String = c.query_row("SELECT status FROM cockpit_sessions WHERE id = 8", [], |row| row.get(0)).unwrap();
            assert_eq!(status, "merged");
        });
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn abandoned_mode_forces_removal_of_dirty_worktree() {
        let repo = make_temp_git_repo("close-force");
        let home = temp_dir("close-force-home");
        with_home(&home.to_string_lossy(), || {
            let c = db_with_workspace(&repo);
            c.execute_batch("INSERT INTO cockpit_sessions (id,name,status,created_at) VALUES (9,'iota','active','2026-01-01 00:00:01')").unwrap();
            crate::agents_git_manager_git_manager_worktree_ops::worktreeAdd(&json!({ "card_slug": "cockpit-9-0" }), &c).unwrap();
            let wt_path = home.join("worktrees").join("cockpit-9-0");
            fs::write(wt_path.join("untracked.txt"), "x").unwrap();
            c.execute(
                "INSERT INTO cockpit_panes (id,session_id,idx,status,worktree_path) VALUES (90,9,0,'working',?1)",
                rusqlite::params![wt_path.to_string_lossy().into_owned()],
            ).unwrap();
            let r = cockpitSessionClose(&json!({ "session_id": 9 }), &c).unwrap();
            assert_eq!(r["status"], json!("abandoned"));
            assert_eq!(r["worktrees_removed"], json!(1));
            assert!(!wt_path.exists());
        });
        let _ = fs::remove_dir_all(&repo);
        let _ = fs::remove_dir_all(&home);
    }
}
