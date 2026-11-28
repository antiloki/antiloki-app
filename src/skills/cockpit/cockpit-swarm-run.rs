//! cockpit-swarm-run.rs — twin of skills/cockpit/cockpit-swarm-run.mjs. NAMES ARE CONTRACT:
//! default `cockpitSwarmRun`. The SWARM pane's FOREMAN: a task queue (rows in cockpit_blackboard,
//! kind 'swarmq', JSON {title,brief,agent,status,reply}) that op run-next drains one task at a
//! time by driving ONE cockpit-pane-message turn on the swarm pane's SHARED worktree.
//!
//! BESPOKE-NATIVE (dispatches to the already-converted `cockpit_cockpit_pane_message::cockpitPaneMessage`
//! twin, which itself carries an `aiCall` PARITY-DEFERRED boundary — see that module's doc), not
//! conform-db. Every deterministic gate around the dispatch (op validation, pane/worktree checks,
//! the single-writer guard, task marking) is ported faithfully and is what this file's own
//! `#[cfg(test)]` suite exercises against a real in-memory db (mirroring cockpit-pane-message.rs's
//! own test harness). The run-next 'done'-vs-'failed' outcome differs deterministically from the
//! reachable JS paths ONLY in that this twin's `cockpitPaneMessage` call itself only ever `Err`s at
//! the aiCall boundary for `pane_kind == 'agent'` panes with `ANTILOKI_VIBE_CHAT` unset/on (the
//! `vibeChatOn` early-Err guard in that module) — a swarm pane (`pane_kind = 'swarm'`) skips that
//! guard and instead flows into `run_pane_agent`'s own aiCall dispatch, whose error is caught
//! INSIDE `cockpitPaneMessage` (producing `Ok({ ok: false, reply_md: "agent error: ..." })`), so
//! `run-next` marks the task 'done' with that message — never 'failed' — for a real swarm pane, an
//! outcome this file's tests assert explicitly.
use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
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
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

// `{ id, ...JSON.parse(row.body_md) }`, or `{ id, title:'(bad task)', brief:'', status:'failed' }` on parse failure.
fn parse_task(id: i64, body_md: &str) -> Value {
    match serde_json::from_str::<Value>(body_md) {
        Ok(Value::Object(mut o)) => { o.insert("id".into(), json!(id)); Value::Object(o) }
        _ => json!({ "id": id, "title": "(bad task)", "brief": "", "status": "failed" }),
    }
}

pub fn cockpitSwarmRun(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));
    // JS: `String(input?.op ?? 'list')` — nullish-coalescing only substitutes for null/undefined,
    // NOT for an empty string, so `op: ''` stays '' (falling through to the final enumVal throw).
    let op = match input.get("op") {
        None | Some(Value::Null) => "list".to_string(),
        Some(v) => str_qq(Some(v)),
    };
    let pane_id_n = js_number(input.get("pane_id"));
    if !truthy_num(pane_id_n) && op != "remove" {
        return Err("cockpit-swarm-run: pane_id is required".into());
    }
    let pane_id = json!(pane_id_n as i64);

    if op == "add" {
        let session_id_n = js_number(input.get("session_id"));
        if !truthy_num(session_id_n) {
            return Err("cockpit-swarm-run: session_id is required".into());
        }
        let t = match input.get("task") { None | Some(Value::Null) => json!({}), Some(v) => v.clone() };
        let body = json!({
            "title": take_chars(&str_qq(t.get("title")), 140),
            "brief": take_chars(&str_qq(t.get("brief")), 6000),
            // JS: `String(t.agent ?? 'forge')` — nullish-coalescing, so an explicit '' stays ''.
            "agent": match t.get("agent") { None | Some(Value::Null) => "forge".to_string(), Some(v) => str_qq(Some(v)) },
            "status": "queued",
        });
        let (_, id) = crate::db_json::run_write(
            db,
            "INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (?,?,?,?)",
            &[json!(session_id_n as i64), pane_id, json!("swarmq"), json!(body.to_string())],
        )?;
        return Ok(json!({ "ok": true, "id": id }));
    }
    if op == "remove" {
        let id_n = js_number(input.get("id"));
        if !truthy_num(id_n) {
            return Err("cockpit-swarm-run: id is required".into());
        }
        crate::db_json::run_write(db, "DELETE FROM cockpit_blackboard WHERE id = ? AND kind = 'swarmq'", &[json!(id_n as i64)])?;
        return Ok(json!({ "ok": true, "id": id_n as i64, "removed": true }));
    }

    let rows = crate::db_json::query_rows(db, "SELECT id, body_md FROM cockpit_blackboard WHERE pane_id = ? AND kind = 'swarmq' ORDER BY id ASC", &[pane_id.clone()])?;
    let tasks: Vec<Value> = rows.as_array().cloned().unwrap_or_default().iter().map(|r| {
        let id = r.get("id").and_then(Value::as_i64).unwrap_or(0);
        parse_task(id, &str_qq(r.get("body_md")))
    }).collect();
    if op == "list" {
        return Ok(json!({ "ok": true, "tasks": tasks }));
    }

    if op == "run-next" {
        let pane_rows = crate::db_json::query_rows(db, "SELECT * FROM cockpit_panes WHERE id = ?", &[pane_id.clone()])?;
        let Some(pane) = pane_rows.as_array().and_then(|a| a.first()) else {
            return Ok(json!({ "ok": false, "error": format!("pane {} not found", pane_id_n as i64) }));
        };
        let worktree_path = pane.get("worktree_path").and_then(Value::as_str).unwrap_or("");
        if worktree_path.is_empty() {
            return Ok(json!({ "ok": false, "error": "the swarm has no worktree yet — provisioning may still be running" }));
        }
        if tasks.iter().any(|t| t.get("status").and_then(Value::as_str) == Some("running")) {
            return Ok(json!({ "ok": false, "error": "a task is already running — one writer at a time in a shared worktree" }));
        }
        let Some(next) = tasks.iter().find(|t| t.get("status").and_then(Value::as_str) == Some("queued")).cloned() else {
            return Ok(json!({ "ok": true, "done": true, "tasks": tasks }));
        };
        let next_id = next.get("id").and_then(Value::as_i64).unwrap_or(0);
        let mark = |db: &Connection, status: &str, extra: &Value| -> Result<(), String> {
            let mut rest = next.as_object().cloned().unwrap_or_default();
            rest.remove("id");
            if let Some(o) = extra.as_object() { for (k, v) in o { rest.insert(k.clone(), v.clone()); } }
            rest.insert("status".into(), json!(status));
            let body = take_chars(&Value::Object(rest).to_string(), 19000);
            crate::db_json::run_write(db, "UPDATE cockpit_blackboard SET body_md = ? WHERE id = ?", &[json!(body), json!(next_id)])?;
            Ok(())
        };
        mark(db, "running", &json!({}))?;

        let title = str_qq(next.get("title"));
        let brief = str_qq(next.get("brief"));
        let user_input = format!("SWARM TASK: {title}\n\n{brief}\n\nWork INSIDE this worktree. Apply your edits directly (the swarm auto-applies). Reply with a 2-line summary when done.");
        match crate::cockpit_cockpit_pane_message::cockpitPaneMessage(&json!({ "pane_id": pane_id_n as i64, "user_input": user_input }), db) {
            Ok(res) => {
                let reply = take_chars(&str_qq(res.get("reply_md")), 1500);
                let applied = res.get("applied").and_then(Value::as_array).map(|a| a.len() as i64).unwrap_or(0);
                mark(db, "done", &json!({ "reply": reply, "applied": applied }))?;
                Ok(json!({ "ok": true, "task_id": next_id, "status": "done", "reply_md": reply, "applied": applied, "ai_call_id": res.get("ai_call_id").cloned().unwrap_or(Value::Null) }))
            }
            Err(e) => {
                let msg = take_chars(&e, 400);
                mark(db, "failed", &json!({ "reply": msg }))?;
                Ok(json!({ "ok": false, "task_id": next_id, "status": "failed", "error": msg }))
            }
        }
    } else {
        Err("cockpit-swarm-run: op 'add'|'run-next'|'list'|'remove' is required".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.ai_calls ( id INTEGER PRIMARY KEY, cost_usd_actual REAL ); \
             CREATE TABLE cost_settings (key TEXT PRIMARY KEY, value TEXT);",
        ).unwrap();
        c.execute("INSERT INTO cockpit_sessions (id, name, created_at) VALUES (1, 's1', datetime('now'))", []).unwrap();
        c
    }

    fn insert_swarm_pane(c: &Connection, worktree: &str) -> i64 {
        crate::db_json::run_write(
            c,
            "INSERT INTO cockpit_panes (session_id, idx, subagent_name, worktree_path, auto_apply, pane_kind) VALUES (1, 0, 'swarm', ?, 1, 'swarm')",
            &[json!(worktree)],
        ).unwrap().1
    }

    fn add_task(c: &Connection, pane_id: i64, title: &str, brief: &str) -> i64 {
        cockpitSwarmRun(&json!({ "op": "add", "pane_id": pane_id, "session_id": 1, "task": { "title": title, "brief": brief } }), c)
            .unwrap()["id"].as_i64().unwrap()
    }

    #[test]
    fn pane_id_required_except_remove() {
        let c = seeded();
        assert_eq!(cockpitSwarmRun(&json!({ "op": "list" }), &c), Err("cockpit-swarm-run: pane_id is required".to_string()));
        assert!(cockpitSwarmRun(&json!({ "op": "remove", "id": 999 }), &c).unwrap()["ok"] == json!(true));
    }

    #[test]
    fn add_requires_session_id_and_defaults_agent_and_status() {
        let c = seeded();
        let pid = insert_swarm_pane(&c, "");
        assert_eq!(
            cockpitSwarmRun(&json!({ "op": "add", "pane_id": pid, "task": {} }), &c),
            Err("cockpit-swarm-run: session_id is required".to_string())
        );
        let tid = add_task(&c, pid, "Fix the bug", "do the thing");
        let listed = cockpitSwarmRun(&json!({ "op": "list", "pane_id": pid }), &c).unwrap();
        let tasks = listed["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0]["id"], json!(tid));
        assert_eq!(tasks[0]["title"], json!("Fix the bug"));
        assert_eq!(tasks[0]["agent"], json!("forge"));
        assert_eq!(tasks[0]["status"], json!("queued"));
    }

    #[test]
    fn add_truncates_title_and_brief() {
        let c = seeded();
        let pid = insert_swarm_pane(&c, "");
        let long_title = "x".repeat(200);
        let long_brief = "y".repeat(7000);
        add_task(&c, pid, &long_title, &long_brief);
        let listed = cockpitSwarmRun(&json!({ "op": "list", "pane_id": pid }), &c).unwrap();
        let t = &listed["tasks"].as_array().unwrap()[0];
        assert_eq!(t["title"].as_str().unwrap().chars().count(), 140);
        assert_eq!(t["brief"].as_str().unwrap().chars().count(), 6000);
    }

    #[test]
    fn remove_deletes_only_swarmq_rows_by_id() {
        let c = seeded();
        let pid = insert_swarm_pane(&c, "");
        let tid = add_task(&c, pid, "T", "B");
        // a non-swarmq row with the same id space should be untouched conceptually; here just verify delete works
        let r = cockpitSwarmRun(&json!({ "op": "remove", "id": tid }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "id": tid, "removed": true }));
        let listed = cockpitSwarmRun(&json!({ "op": "list", "pane_id": pid }), &c).unwrap();
        assert_eq!(listed["tasks"], json!([]));
    }

    #[test]
    fn parse_task_recovers_from_bad_json() {
        let c = seeded();
        let pid = insert_swarm_pane(&c, "");
        crate::db_json::run_write(&c, "INSERT INTO cockpit_blackboard (session_id, pane_id, kind, body_md) VALUES (1, ?, 'swarmq', 'not json')", &[json!(pid)]).unwrap();
        let listed = cockpitSwarmRun(&json!({ "op": "list", "pane_id": pid }), &c).unwrap();
        let t = &listed["tasks"].as_array().unwrap()[0];
        assert_eq!(t["title"], json!("(bad task)"));
        assert_eq!(t["status"], json!("failed"));
    }

    #[test]
    fn run_next_requires_pane_found_and_worktree() {
        let c = seeded();
        let r = cockpitSwarmRun(&json!({ "op": "run-next", "pane_id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "pane 999 not found" }));

        let pid = insert_swarm_pane(&c, "");
        let r2 = cockpitSwarmRun(&json!({ "op": "run-next", "pane_id": pid }), &c).unwrap();
        assert_eq!(r2["ok"], json!(false));
        assert_eq!(r2["error"], json!("the swarm has no worktree yet — provisioning may still be running"));
    }

    #[test]
    fn run_next_reports_done_true_when_queue_is_empty() {
        let c = seeded();
        let pid = insert_swarm_pane(&c, "/tmp/wt");
        let r = cockpitSwarmRun(&json!({ "op": "run-next", "pane_id": pid }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["done"], json!(true));
    }

    #[test]
    fn run_next_refuses_when_a_task_is_already_running() {
        let c = seeded();
        let pid = insert_swarm_pane(&c, "/tmp/wt");
        let tid = add_task(&c, pid, "T1", "B1");
        crate::db_json::run_write(&c, "UPDATE cockpit_blackboard SET body_md = '{\"title\":\"T1\",\"brief\":\"B1\",\"agent\":\"forge\",\"status\":\"running\"}' WHERE id = ?", &[json!(tid)]).unwrap();
        let r = cockpitSwarmRun(&json!({ "op": "run-next", "pane_id": pid }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("a task is already running — one writer at a time in a shared worktree"));
    }

    // swarm pane_kind skips cockpit-pane-message's vibeChatOn early-Err guard (agent-only), so this
    // reaches run_pane_agent's own aiCall PARITY-DEFERRED dispatch, whose Err is caught INSIDE
    // cockpitPaneMessage — meaning run-next always sees an Ok(..) here and marks 'done', never
    // 'failed', for a real swarm pane. This is the deterministic parity boundary this twin discloses.
    #[test]
    fn run_next_marks_done_with_agent_error_reply_for_a_real_swarm_pane() {
        let c = seeded();
        let pid = insert_swarm_pane(&c, "/tmp/wt");
        let tid = add_task(&c, pid, "Ship it", "make it work");
        let r = cockpitSwarmRun(&json!({ "op": "run-next", "pane_id": pid }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["task_id"], json!(tid));
        assert_eq!(r["status"], json!("done"));
        assert!(r["reply_md"].as_str().unwrap().starts_with("agent error:"));
        let listed = cockpitSwarmRun(&json!({ "op": "list", "pane_id": pid }), &c).unwrap();
        let t = &listed["tasks"].as_array().unwrap()[0];
        assert_eq!(t["status"], json!("done"));
        assert!(t["reply"].as_str().unwrap().starts_with("agent error:"));
    }

    #[test]
    fn unknown_op_errors() {
        let c = seeded();
        let pid = insert_swarm_pane(&c, "");
        assert_eq!(
            cockpitSwarmRun(&json!({ "op": "bogus", "pane_id": pid }), &c),
            Err("cockpit-swarm-run: op 'add'|'run-next'|'list'|'remove' is required".to_string())
        );
    }
}
