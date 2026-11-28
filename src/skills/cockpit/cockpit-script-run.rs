//! cockpit-script-run.rs — twin of skills/cockpit/cockpit-script-run.mjs (db WRITE lane, async
//! default export — the JS `await`s nothing itself, so per the async-lane convention this twin is
//! an ordinary sync `pub fn`). NAMES ARE CONTRACT: `cockpitScriptRun` (default). SCRIPT-RUNNER
//! PANES: resolves a registry command (build|dev|test, via the already-converted
//! `system_workspace_scripts::resolveScript` twin) and runs it in a sibling pane's worktree,
//! streaming output into `build_gate_live` keyed `vibe-pane-<pane_id>`.
//!
//! MODULE-SCOPE PROCESS REGISTRY — DISCLOSED, PARTIAL PARITY (self_flag, human-review-queue item).
//! The JS module holds one live child PER PANE in a process-lifetime `Map` (`RUNNING`), mutated by
//! `run`/`stop` and by async `child.on('close', ...)`/`stdout`/`stderr` event handlers that fire
//! ANY TIME AFTER the function has already returned (spawn is fire-and-forget, unlike the
//! `git diff --name-only` wait-for-exit precedent in `constitution-estimate.rs`). This twin mirrors
//! the Map with a real process-scoped `static RUNNING: OnceLock<Mutex<HashMap<i64, RunningEntry>>>`
//! (same pattern as `engine/log-decision.rs`'s dedup map) holding a REAL `std::process::Child` —
//! `status`/`stop` read/kill it for real, and `run`'s spawn itself is REAL
//! (`std::process::Command`, platform-branched shell invocation mirroring
//! `spawn(cmd, {cwd, shell:true, env})`). The RETURN VALUE and the two SYNCHRONOUS db writes that
//! precede the spawn (`DELETE FROM build_gate_live` + the first `liveWrite` banner insert) are
//! fully ported and real. NOT ported: the `stdout`/`stderr` streaming writes, the `close` handler's
//! exit-line write + Map cleanup, and the 600s cap timer — all of these need to keep writing
//! through `db` for up to 10 minutes AFTER this call returns, and `db: &Connection` here is a
//! borrowed, call-scoped reference (not an owned/`'static` handle a background OS thread could
//! soundly hold), so there is no sound, non-fake way to port them within this twin's per-call
//! signature. This is a genuine architecture gap (not a stubbed/faked implementation of the ported
//! parts), and it never surfaces in the fixture: exactly like the git-diff branch, `run`'s spawn
//! sub-branch (reachable only when BOTH the target pane has a provisioned worktree AND a script is
//! resolved) is real code, deliberately UNEXERCISED by any fixture case — spawning a real npm
//! build/dev/test process inside a byte-identical-comparison harness would itself be
//! non-deterministic. The fixture instead covers `status`/`stop` (empty registry — the only state a
//! fresh process can deterministically start from) and `run`'s two early `ok:false` guards (no
//! worktree / no resolved script), neither of which touches the registry or `build_gate_live`.
//!
//! `context.db` missing → JS throws `err.contextMissing('cockpit-script-run', 'db')`. Not modeled:
//! `db: &Connection` is a required, non-nullable reference, same convention as every sibling twin.
//! `pane_id`/`worktree_pane_id` missing/falsy and an invalid `script` enum are guard-throws, ported
//! as `Err(String)` but not fixture-tested (playbook gotcha #9).
// QUALITY: 60/100 — script-runner pane: real spawn + real process registry for status/stop, but the
// async stdout/stderr streaming past return is a disclosed, architecturally-unavoidable gap (see module doc)

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Child;
use std::sync::{Mutex, OnceLock};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
// JS Number(v): the subset that appears here (missing/null/number/string/bool).
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }
fn truthy_num(n: f64) -> bool { n != 0.0 && !n.is_nan() }

// = JS Number.prototype.toString()/`${f}` for a raw f64 not sourced from a JSON Value — used only
// for `${wtPaneId}` in the two "no worktree"/"no script" error strings, where wtPaneId is a plain
// JS Number (Number(input?.worktree_pane_id)), not a db-sourced Value.
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
fn tmpl_num(n: f64) -> String { if n.is_finite() && n == n.trunc() { (n as i64).to_string() } else { js_number_to_string(n) } }

struct RunningEntry { child: Child, script: String, started_at: String }

fn running_map() -> &'static Mutex<HashMap<i64, RunningEntry>> {
    static MAP: OnceLock<Mutex<HashMap<i64, RunningEntry>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

// `spawn(cmd, { cwd, shell: true, env: { ...process.env, BROWSER: 'none', ...(kind === 'dev' ? {}
// : { CI: '1' }) } })` — REAL, disclosed-untested (see module doc): platform shell invocation
// mirroring node's `shell: true` (cmd.exe /C on Windows, sh -c elsewhere). `Command` inherits the
// full parent environment by default (matching the JS's explicit `...process.env` spread); `.env()`
// then layers BROWSER (always) and CI (unless kind === 'dev') on top, same as the JS spread order.
fn spawn_script(cmd: &str, cwd: &str, kind: &str) -> std::io::Result<Child> {
    let mut command = if cfg!(windows) {
        let mut c = crate::proc::cmd("cmd");
        c.args(["/C", cmd]);
        c
    } else {
        let mut c = crate::proc::cmd("sh");
        c.args(["-c", cmd]);
        c
    };
    command.current_dir(cwd).env("BROWSER", "none");
    if kind != "dev" { command.env("CI", "1"); }
    command.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    command.spawn()
}

/// The main DB's file path (empty for an in-memory test db → the pump degrades to draining the pipes).
fn main_db_file(db: &Connection) -> String {
    db.query_row("SELECT file FROM pragma_database_list WHERE name = 'main'", [], |r| r.get::<_, Option<String>>(0))
        .ok().flatten().unwrap_or_default()
}

/// Reader threads for stdout + stderr → `build_gate_live` rows (seq 1..), plus a `done=1` row once
/// both pipes close. Chunks are line-batched (≤ 4000 chars, the JS cap) so a build's thousands of
/// lines become hundreds of rows, not thousands.
fn pump_output(db_file: String, live_key: String, stdout: Option<std::process::ChildStdout>, stderr: Option<std::process::ChildStderr>) {
    use std::io::Read;
    use std::sync::{mpsc, Arc};
    let (tx, rx) = mpsc::channel::<Option<String>>();
    let mut readers = 0;
    let spawn_reader = |mut r: Box<dyn Read + Send>, tx: mpsc::Sender<Option<String>>| {
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match r.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => { let _ = tx.send(Some(String::from_utf8_lossy(&buf[..n]).to_string())); }
                }
            }
            let _ = tx.send(None);
        });
    };
    if let Some(o) = stdout { readers += 1; spawn_reader(Box::new(o), tx.clone()); }
    if let Some(e) = stderr { readers += 1; spawn_reader(Box::new(e), tx.clone()); }
    drop(tx);
    let key = Arc::new(live_key);
    std::thread::spawn(move || {
        let conn = if db_file.is_empty() { None } else {
            Connection::open(&db_file).ok().map(|c| { let _ = c.busy_timeout(std::time::Duration::from_secs(5)); c })
        };
        let mut seq: i64 = 1;
        let mut open = readers;
        let mut pending = String::new();
        let mut last_flush = std::time::Instant::now();
        let flush = |conn: &Option<Connection>, seq: &mut i64, chunk: &str, done: i64| {
            if let Some(c) = conn {
                let capped: String = chunk.chars().take(4000).collect();
                let _ = c.execute(
                    "INSERT INTO build_gate_live (contract_slug, phase, seq, chunk, done) VALUES (?1, 'build', ?2, ?3, ?4)",
                    rusqlite::params![key.as_str(), *seq, capped, done],
                );
                *seq += 1;
            }
        };
        loop {
            match rx.recv_timeout(std::time::Duration::from_millis(400)) {
                Ok(Some(s)) => { pending.push_str(&s); }
                Ok(None) => { open -= 1; }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => { open = 0; }
            }
            let due = !pending.is_empty() && (pending.len() >= 3500 || last_flush.elapsed() >= std::time::Duration::from_millis(300));
            if due {
                while !pending.is_empty() {
                    let cut = if pending.len() > 3500 { pending.char_indices().map(|(i, _)| i).take_while(|&i| i < 3500).last().unwrap_or(0).max(1) } else { pending.len() };
                    let (head, tail) = pending.split_at(cut);
                    flush(&conn, &mut seq, head, 0);
                    pending = tail.to_string();
                }
                last_flush = std::time::Instant::now();
            }
            if open == 0 {
                if !pending.is_empty() { flush(&conn, &mut seq, &pending, 0); }
                flush(&conn, &mut seq, "\n[done]\n", 1);
                break;
            }
        }
    });
}

pub fn cockpitScriptRun(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `if (!db) throw err.contextMissing(...)` — unreachable, db is always injected.
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));

    let pane_id_n = js_number(input.get("pane_id"));
    if !truthy_num(pane_id_n) { return Err("cockpit-script-run: pane_id is required".into()); }
    let pane_id = pane_id_n as i64;
    let op = input.get("op").and_then(|v| v.as_str()).unwrap_or("run");

    if op == "status" {
        let mut map = running_map().lock().unwrap();
        let (running, script, started_at) = match map.get_mut(&pane_id) {
            Some(entry) => {
                let still_running = matches!(entry.child.try_wait(), Ok(None));
                (still_running, json!(entry.script.clone()), json!(entry.started_at.clone()))
            }
            None => (false, Value::Null, Value::Null),
        };
        return Ok(json!({ "ok": true, "pane_id": pane_id, "running": running, "script": script, "started_at": started_at }));
    }

    if op == "stop" {
        let mut map = running_map().lock().unwrap();
        if let Some(entry) = map.get_mut(&pane_id) {
            if matches!(entry.child.try_wait(), Ok(None)) {
                let _ = entry.child.kill();
            }
        }
        map.remove(&pane_id);
        return Ok(json!({ "ok": true, "pane_id": pane_id, "stopped": true }));
    }

    // ── run ──
    let kind = str_qq(input.get("script"));
    if kind != "build" && kind != "dev" && kind != "test" && kind != "install" {
        return Err(format!("cockpit-script-run: script must be one of: build, dev, test, install (got '{kind}')"));
    }
    // `worktree_pane_id: "root"` (operator 2026-09-16 — the merged Terminal pane's scripts tab runs
    // without an agent pane): the active workspace root is the cwd, no pane row involved.
    let wants_root = input.get("worktree_pane_id").and_then(Value::as_str) == Some("root");
    let wt_pane_id_n = if wants_root { 0.0 } else { js_number(input.get("worktree_pane_id")) };
    if !wants_root && !truthy_num(wt_pane_id_n) { return Err("cockpit-script-run: worktree_pane_id is required".into()); }
    let wt_pane_id_param: Value = if wt_pane_id_n.is_finite() && wt_pane_id_n == wt_pane_id_n.trunc() { json!(wt_pane_id_n as i64) } else { json!(wt_pane_id_n) };

    let (worktree_path_s, wt_idx) = if wants_root {
        let root = crate::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or("").to_string();
        if root.is_empty() {
            return Ok(json!({ "ok": false, "error": "no active workspace — open a repo on the Code screen first" }));
        }
        (root, Value::Null)
    } else {
        let wt_rows = crate::db_json::query_rows(db, "SELECT id, idx, worktree_path FROM cockpit_panes WHERE id = ?", &[wt_pane_id_param])?;
        let wt_pane = wt_rows.as_array().and_then(|a| a.first()).cloned();
        let worktree_path = wt_pane.as_ref().and_then(|p| p.get("worktree_path")).cloned();
        if !truthy(worktree_path.as_ref()) {
            return Ok(json!({ "ok": false, "error": format!("pane {} has no worktree provisioned — pick a pane whose worktree exists", tmpl_num(wt_pane_id_n)) }));
        }
        (worktree_path.as_ref().and_then(|v| v.as_str()).unwrap_or("").to_string(), wt_pane.as_ref().and_then(|p| p.get("idx")).cloned().unwrap_or(Value::Null))
    };

    let cmd_v = crate::system_workspace_scripts::resolveScript(db, &kind);
    if !truthy(Some(&cmd_v)) {
        return Ok(json!({ "ok": false, "error": format!("no '{kind}' script recorded or detected — record one on the Code screen (scripts registry)") }));
    }
    let cmd = cmd_v.as_str().unwrap_or("").to_string();

    // Re-run semantics: a click while running kills the previous child first.
    {
        let mut map = running_map().lock().unwrap();
        if let Some(entry) = map.get_mut(&pane_id) {
            if matches!(entry.child.try_wait(), Ok(None)) {
                let _ = entry.child.kill();
            }
        }
    }

    let live_key = format!("vibe-pane-{pane_id}");
    let _ = crate::db_json::run_write(db, "DELETE FROM build_gate_live WHERE contract_slug = ?", &[json!(live_key.clone())]);
    let banner = if wants_root { format!("[pane #{pane_id}] $ {cmd}   (workspace root: {worktree_path_s})\n") } else { format!("[pane #{pane_id}] $ {cmd}   (worktree of pane #{}: {worktree_path_s})\n", tmpl(Some(&wt_idx))) };
    let banner_chunk: String = banner.chars().take(4000).collect();
    let _ = crate::db_json::run_write(
        db,
        "INSERT INTO build_gate_live (contract_slug, phase, seq, chunk, done) VALUES (?, 'build', ?, ?, ?)",
        &[json!(live_key.clone()), json!(0), json!(banner_chunk), json!(0)],
    );

    // REAL spawn — disclosed untested by fixture (see module doc). JS always proceeds to the
    // {ok:true,...} return regardless of spawn outcome (Node defers ENOENT-style failures to an
    // async 'error' event); mirrored here by ignoring a synchronous spawn Err the same way.
    if let Ok(mut child) = spawn_script(&cmd, &worktree_path_s, &kind) {
        // Frozen-clock literal: `new Date().toISOString()` under the conform harness's frozen
        // clock always yields this instant (never exercised by fixture — see module doc).
        let started_at = crate::clock::now_iso_nodb();
        // STREAMING (operator 2026-09-16: "scripts/run is not working"): the port piped stdout/stderr
        // but never read them — the pane showed the banner and nothing else (and a chatty script
        // blocked on the full pipe). Pump both into `build_gate_live` from a reader thread on its own
        // connection to the same file (WAL + busy timeout), the same rows the JS `liveWrite` wrote,
        // then the `done` row when the child exits.
        let db_file = main_db_file(db);
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        pump_output(db_file, live_key.clone(), stdout, stderr);
        running_map().lock().unwrap().insert(pane_id, RunningEntry { child, script: kind.clone(), started_at });
    } else {
        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO build_gate_live (contract_slug, phase, seq, chunk, done) VALUES (?, 'build', ?, ?, ?)",
            &[json!(live_key.clone()), json!(1), json!(format!("could not spawn `{cmd}` in {worktree_path_s}\n")), json!(1)],
        );
    }

    Ok(json!({ "ok": true, "pane_id": pane_id, "script": kind, "cmd": cmd, "worktree_pane": wt_idx, "live_key": live_key, "running": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER NOT NULL, \
             idx INTEGER NOT NULL, subagent_name TEXT NOT NULL DEFAULT '', scope_json TEXT NOT NULL DEFAULT '[]', \
             grants_json TEXT NOT NULL DEFAULT '[]', worktree_path TEXT, branch TEXT, tier TEXT NOT NULL DEFAULT 'low', \
             status TEXT NOT NULL DEFAULT 'idle', last_activity TEXT)",
        ).unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        ).unwrap();
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active, created_at) VALUES ('w', ?1, 'git', 1, '2026-01-01 00:00:00')",
            rusqlite::params![concat!(env!("ANTILOKI_FIXTURES"), "/cockpit-script-run-no-pkg")],
        ).unwrap();
        c
    }

    #[test]
    fn status_and_stop_on_a_fresh_registry_are_deterministic_noops() {
        let c = db();
        let s = cockpitScriptRun(&json!({ "pane_id": 1, "op": "status" }), &c).unwrap();
        assert_eq!(s, json!({ "ok": true, "pane_id": 1, "running": false, "script": null, "started_at": null }));
        let t = cockpitScriptRun(&json!({ "pane_id": 2, "op": "stop" }), &c).unwrap();
        assert_eq!(t, json!({ "ok": true, "pane_id": 2, "stopped": true }));
    }

    #[test]
    fn run_without_a_provisioned_worktree_is_ok_false() {
        let c = db();
        c.execute("INSERT INTO cockpit_panes (session_id, idx, worktree_path) VALUES (1, 0, NULL)", []).unwrap();
        let r = cockpitScriptRun(&json!({ "pane_id": 100, "script": "build", "worktree_pane_id": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "pane 1 has no worktree provisioned — pick a pane whose worktree exists" }));
    }

    #[test]
    fn run_with_no_matching_worktree_pane_row_is_ok_false() {
        let c = db();
        let r = cockpitScriptRun(&json!({ "pane_id": 101, "script": "dev", "worktree_pane_id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "pane 999 has no worktree provisioned — pick a pane whose worktree exists" }));
    }

    #[test]
    fn run_with_a_worktree_but_no_resolved_script_is_ok_false() {
        let c = db();
        c.execute("INSERT INTO cockpit_panes (session_id, idx, worktree_path) VALUES (1, 1, 'C:/tmp/wt11')", []).unwrap();
        let r = cockpitScriptRun(&json!({ "pane_id": 102, "script": "test", "worktree_pane_id": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "no 'test' script recorded or detected — record one on the Code screen (scripts registry)" }));
    }

    #[test]
    fn guard_throws_are_not_swallowed() {
        let c = db();
        assert_eq!(cockpitScriptRun(&json!({}), &c).unwrap_err(), "cockpit-script-run: pane_id is required");
        assert_eq!(
            cockpitScriptRun(&json!({ "pane_id": 1, "script": "lint" }), &c).unwrap_err(),
            "cockpit-script-run: script must be one of: build, dev, test, install (got 'lint')"
        );
        assert_eq!(
            cockpitScriptRun(&json!({ "pane_id": 1, "script": "build" }), &c).unwrap_err(),
            "cockpit-script-run: worktree_pane_id is required"
        );
    }

    #[test]
    fn status_stop_key_on_pane_id_independently_of_op_casing_of_run_state() {
        // Registry is process-scoped (static) — confirm two DIFFERENT pane_ids never see each
        // other's (absent) state, matching the JS Map's per-key isolation.
        let c = db();
        let a = cockpitScriptRun(&json!({ "pane_id": 7, "op": "status" }), &c).unwrap();
        let b = cockpitScriptRun(&json!({ "pane_id": 8, "op": "status" }), &c).unwrap();
        assert_eq!(a["running"], json!(false));
        assert_eq!(b["running"], json!(false));
    }
}
