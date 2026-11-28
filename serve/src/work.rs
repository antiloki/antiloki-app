//! SAGAS (operator 2026-10-11): a place, per project, where the user and their agents keep the work - a Linear-like list of SAGAS (a titled goal; an AI plan is one), each holding EPISODES
//! (and one level of steps: an episode's checklist), every one with an assignee, an activity trail that names WHO did each step, and telemetry (time, cost, tokens, which seat) taken from the real runs.
//!
//! The team is the user and a team of agents - not people. So there are no mentions, no notifications and no permissions; an actor is `you`, `heimdall`, `planner` or an agent's name.
//!
//! ONE SQLITE PER PROJECT at `<project>/.antiloki/work.sqlite`. It is opened per request (WAL), never through the cockpit db's writer connection, so nothing here can deadlock a dispatch.
//!
//! A PLAN BECOMES A SAGA BY SYNC, NOT BY A HOOK: `sync_runs` upserts one saga per goal run of this project (key `run:<id>`) and one task per run task, with the run's status, time, cost and seat
//! written onto them. The screen calls it whenever it reads, so it is always current, and a task the user added by hand to such a saga is never touched.
//!
//! Routes: GET /api/v1/sagas (list, rolled up) · GET /api/v1/sagas/detail?id= · GET /api/v1/sagas/task?id= (also /sagas/episode?id=) · POST /api/v1/sagas/{op} with op = create | update | delete | episode-create | episode-update | episode-delete (the old `task-*` ops answer the same).
//!
//! NAMES (operator 2026-11-11, RFC 0008 §4.10): what the store calls a TASK is an EPISODE in the product and the API, and a SUBTASK is a STEP. The tables keep their old names (`tasks`, `parent_id`), so an older
//! serve or agent binary opening the same file keeps working; the responses carry both words for one release (`episodes` and `tasks`, `steps` and `subtasks`).
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const EPISODE_STATUSES: [&str; 5] = ["todo", "doing", "done", "failed", "canceled"];
const MAX_TITLE: usize = 200;

pub fn db_path(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("work.sqlite") }

pub fn open(root: &str) -> Result<Connection, String> {
    if root.trim().is_empty() { return Err("no project is open".into()); }
    let p = db_path(root);
    if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    let c = Connection::open(p).map_err(|e| e.to_string())?;
    init(&c).map_err(|e| e.to_string())?;
    Ok(c)
}
/// The tables, for the tests of modules that write into the saga store.
#[cfg(test)]
pub(crate) fn init_for_tests(c: &Connection) -> rusqlite::Result<()> { init(c) }
fn init(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
      CREATE TABLE IF NOT EXISTS sagas (
        id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, objective TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'todo',
        origin TEXT NOT NULL DEFAULT 'you', plan_ref TEXT, confidence TEXT NOT NULL DEFAULT '', delivered INTEGER NOT NULL DEFAULT 0,
        created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL);
      CREATE UNIQUE INDEX IF NOT EXISTS sagas_plan_ref ON sagas(plan_ref) WHERE plan_ref IS NOT NULL;
      CREATE TABLE IF NOT EXISTS tasks (
        id INTEGER PRIMARY KEY AUTOINCREMENT, saga_id INTEGER NOT NULL REFERENCES sagas(id) ON DELETE CASCADE, parent_id INTEGER REFERENCES tasks(id) ON DELETE CASCADE,
        title TEXT NOT NULL, body TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'todo', assignee TEXT NOT NULL DEFAULT '', created_by TEXT NOT NULL DEFAULT 'you',
        position INTEGER NOT NULL DEFAULT 0, ext TEXT, engine TEXT NOT NULL DEFAULT '', model TEXT NOT NULL DEFAULT '',
        ms_total INTEGER NOT NULL DEFAULT 0, usd_total REAL NOT NULL DEFAULT 0, tokens INTEGER NOT NULL DEFAULT 0,
        created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL, started_ms INTEGER, done_ms INTEGER);
      CREATE UNIQUE INDEX IF NOT EXISTS tasks_ext ON tasks(ext) WHERE ext IS NOT NULL;
      CREATE INDEX IF NOT EXISTS tasks_saga ON tasks(saga_id, position, id);
      CREATE TABLE IF NOT EXISTS events (
        id INTEGER PRIMARY KEY AUTOINCREMENT, saga_id INTEGER NOT NULL REFERENCES sagas(id) ON DELETE CASCADE, task_id INTEGER REFERENCES tasks(id) ON DELETE CASCADE,
        t_ms INTEGER NOT NULL, actor TEXT NOT NULL, kind TEXT NOT NULL, text TEXT NOT NULL DEFAULT '', ext TEXT);
      CREATE UNIQUE INDEX IF NOT EXISTS events_ext ON events(ext) WHERE ext IS NOT NULL;
      CREATE INDEX IF NOT EXISTS events_saga ON events(saga_id, t_ms);")?;
    // a run's fingerprint at its last sync: an unchanged run (all 100 of a project's finished ones, on every poll) is skipped whole
    if !c.prepare("SELECT 1 FROM pragma_table_info('sagas') WHERE name = 'sig'")?.exists([])? { c.execute_batch("ALTER TABLE sagas ADD COLUMN sig TEXT NOT NULL DEFAULT ''")?; }
    // an episode's deterministic metadata (the pane, its tokens and price, the tools it used ...) as JSON; empty for a saga that is not one
    if !c.prepare("SELECT 1 FROM pragma_table_info('sagas') WHERE name = 'meta'")?.exists([])? { c.execute_batch("ALTER TABLE sagas ADD COLUMN meta TEXT NOT NULL DEFAULT ''")?; }
    // how a hand-made saga is RUN (operator 2026-11-11): the seat and model that do it and the most it may cost, as JSON; empty until the person chooses
    if !c.prepare("SELECT 1 FROM pragma_table_info('sagas') WHERE name = 'run_cfg'")?.exists([])? { c.execute_batch("ALTER TABLE sagas ADD COLUMN run_cfg TEXT NOT NULL DEFAULT ''")?; }
    // an episode's own facts as JSON (RFC 0008 §4.9: which run and which loop pass made it, the episode it retries, the team episode that produced it); empty for one that has none
    if !c.prepare("SELECT 1 FROM pragma_table_info('tasks') WHERE name = 'meta'")?.exists([])? { c.execute_batch("ALTER TABLE tasks ADD COLUMN meta TEXT NOT NULL DEFAULT ''")?; }
    crate::work_changes::init(c)?;
    Ok(())
}
fn now() -> i64 { crate::cli::now_ms() }
fn rows(c: &Connection, sql: &str, p: &[Value]) -> Result<Vec<Value>, String> { Ok(antiloki_rs::db_json::query_rows(c, sql, p)?.as_array().cloned().unwrap_or_default()) }
fn clean_title(t: &str) -> Result<String, String> {
    let t = t.trim();
    if t.is_empty() { return Err("a title is needed".into()); }
    Ok(t.chars().take(MAX_TITLE).collect())
}
pub(crate) fn event(c: &Connection, saga: i64, task: Option<i64>, actor: &str, kind: &str, text: &str) -> Result<(), String> {
    c.execute("INSERT INTO events (saga_id, task_id, t_ms, actor, kind, text) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![saga, task, now(), actor, kind, text]).map_err(|e| e.to_string())?;
    Ok(())
}
fn touch_saga(c: &Connection, id: i64) { let _ = c.execute("UPDATE sagas SET updated_ms = ?1 WHERE id = ?2", params![now(), id]); }
fn exists(c: &Connection, table: &str, id: i64) -> bool { c.query_row(&format!("SELECT 1 FROM {table} WHERE id = ?1"), [id], |_| Ok(())).is_ok() }

// ------------------------------------------------------------------------------------------------------------------------------ writes
pub fn create_saga(c: &Connection, title: &str, objective: &str, actor: &str) -> Result<i64, String> {
    let title = clean_title(title)?;
    c.execute("INSERT INTO sagas (title, objective, origin, created_ms, updated_ms) VALUES (?1, ?2, ?3, ?4, ?4)", params![title, objective.trim(), actor, now()]).map_err(|e| e.to_string())?;
    let id = c.last_insert_rowid();
    event(c, id, None, actor, "created", &format!("started the saga \"{title}\""))?;
    Ok(id)
}
/// What a saga IS right now (its stored word and its tasks, as `list` shows it).
pub fn effective_status(c: &Connection, id: i64) -> Result<&'static str, String> {
    let stored: String = c.query_row("SELECT status FROM sagas WHERE id = ?1", [id], |r| r.get(0)).map_err(|_| "no such saga".to_string())?;
    let tasks = rows(c, "SELECT status, ms_total, usd_total, tokens, engine, model FROM tasks WHERE saga_id = ?1", &[json!(id)])?;
    Ok(derive_status(&stored, &rollup(&tasks)))
}
/// A FINISHED SAGA IS PART OF THE RECORD (operator 2026-10-11: "finished sagas can't be deleted or canceled"): done and failed ones can be neither canceled nor deleted, and a canceled one can only be reopened.
/// Only a saga that is still open (todo or in progress) can be canceled or deleted. The screen hides the buttons; this is the rule they mirror.
pub fn is_open(status: &str) -> bool { matches!(status, "todo" | "doing") }

pub fn update_saga(c: &Connection, id: i64, patch: &Value, actor: &str) -> Result<(), String> {
    if !exists(c, "sagas", id) { return Err("no such saga".into()); }
    if patch.get("status").and_then(Value::as_str) == Some("canceled") && !is_open(effective_status(c, id)?) { return Err("a finished saga cannot be canceled: it is part of the record".into()); }
    if let Some(t) = patch.get("title").and_then(Value::as_str) { let t = clean_title(t)?; c.execute("UPDATE sagas SET title = ?1 WHERE id = ?2", params![t, id]).map_err(|e| e.to_string())?; event(c, id, None, actor, "edited", &format!("renamed it to \"{t}\""))?; }
    if let Some(o) = patch.get("objective").and_then(Value::as_str) { c.execute("UPDATE sagas SET objective = ?1 WHERE id = ?2", params![o.trim(), id]).map_err(|e| e.to_string())?; event(c, id, None, actor, "edited", "changed the objective")?; }
    // HOW IT RUNS: the seat, the model, the most it may cost (USD; 0 or null takes the limit away)
    if ["engine", "model", "budget_usd"].iter().any(|k| patch.get(*k).is_some()) {
        let cur: String = c.query_row("SELECT run_cfg FROM sagas WHERE id = ?1", [id], |x| x.get(0)).map_err(|e| e.to_string())?;
        let mut cfg = run_of(&json!(cur)); let o = cfg.as_object_mut().unwrap();
        for k in ["engine", "model"] { if let Some(v) = patch.get(k) { let t = v.as_str().unwrap_or("").trim(); if t.is_empty() { o.remove(k); } else { o.insert(k.into(), json!(t)); } } }
        if let Some(v) = patch.get("budget_usd") { let b = v.as_f64().or_else(|| v.as_str().and_then(|t| t.trim().parse().ok())).unwrap_or(0.0); if b < 0.0 { return Err("the budget cannot be negative".into()); } if b == 0.0 { o.remove("budget_usd"); } else { o.insert("budget_usd".into(), json!((b * 100.0).round() / 100.0)); } }
        let text = if cfg.as_object().map_or(true, |m| m.is_empty()) { String::new() } else { cfg.to_string() };
        c.execute("UPDATE sagas SET run_cfg = ?1 WHERE id = ?2", params![text, id]).map_err(|e| e.to_string())?;
        event(c, id, None, actor, "edited", &format!("set how it runs: {}", if text.is_empty() { "defaults".to_string() } else { text.clone() }))?;
    }
    if let Some(s) = patch.get("status").and_then(Value::as_str) {
        if !["todo", "canceled"].contains(&s) { return Err("a saga's status follows its tasks; only todo (reopen) or canceled can be set".into()); }
        c.execute("UPDATE sagas SET status = ?1 WHERE id = ?2", params![s, id]).map_err(|e| e.to_string())?;
        event(c, id, None, actor, "status", &if s == "canceled" { "canceled the saga".to_string() } else { "reopened the saga".to_string() })?;
    }
    touch_saga(c, id);
    Ok(())
}
pub fn delete_saga(c: &Connection, id: i64) -> Result<(), String> {
    if exists(c, "sagas", id) && !is_open(effective_status(c, id)?) { return Err("a finished saga cannot be deleted: it is part of the record".into()); }
    if c.execute("DELETE FROM sagas WHERE id = ?1", [id]).map_err(|e| e.to_string())? == 0 { return Err("no such saga".into()); }
    Ok(())
}
/// A run that RUNS this hand-made saga: the saga takes the run as its own (so the sync updates it in place and adds no second saga), and each task is tied to the run's task made from it (so the sync updates the
/// task in place and adds no copy). `pairs`: (the task's id here, the id of the run's task made from it). Clears the old run's seal so the next read writes everything.
pub fn link_run(c: &Connection, saga_id: i64, run_id: i64, pairs: &[(i64, String)], actor: &str) -> Result<(), String> {
    let key = format!("run:{run_id}");
    // a read of the sagas between the run's birth and now may already have made a saga of its own for this run (the sync does, for every run): that one is this run's copy, and goes
    c.execute("DELETE FROM sagas WHERE plan_ref = ?1 AND id != ?2", params![key, saga_id]).map_err(|e| e.to_string())?;
    c.execute("UPDATE sagas SET plan_ref = ?1, sig = '' WHERE id = ?2", params![key, saga_id]).map_err(|e| e.to_string())?;
    for (tid, pid) in pairs { c.execute("UPDATE tasks SET ext = ?1 WHERE id = ?2 AND saga_id = ?3", params![format!("{key}:{pid}"), tid, saga_id]).map_err(|e| e.to_string())?; }
    event(c, saga_id, None, actor, "run", &format!("started run {run_id} on {} episode{}", pairs.len(), if pairs.len() == 1 { "" } else { "s" }))?;
    touch_saga(c, saga_id); Ok(())
}
/// An episode, or - with `parent_id` - a step of one. Steps go one level deep: a step cannot have its own.
pub fn create_episode(c: &Connection, saga_id: i64, parent_id: Option<i64>, title: &str, body: &str, assignee: &str, actor: &str) -> Result<i64, String> {
    let title = clean_title(title)?;
    if !exists(c, "sagas", saga_id) { return Err("no such saga".into()); }
    if let Some(p) = parent_id {
        let row = rows(c, "SELECT saga_id, parent_id FROM tasks WHERE id = ?1", &[json!(p)])?;
        let Some(r) = row.first() else { return Err("no such parent episode".into()) };
        if r["saga_id"].as_i64() != Some(saga_id) { return Err("the parent episode belongs to another saga".into()); }
        if !r["parent_id"].is_null() { return Err("a step cannot have steps".into()); }
    }
    let pos: i64 = c.query_row("SELECT COALESCE(MAX(position), 0) + 1 FROM tasks WHERE saga_id = ?1 AND parent_id IS ?2", params![saga_id, parent_id], |r| r.get(0)).unwrap_or(1);
    c.execute("INSERT INTO tasks (saga_id, parent_id, title, body, assignee, created_by, position, created_ms, updated_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        params![saga_id, parent_id, title, body.trim(), assignee.trim(), actor, pos, now()]).map_err(|e| e.to_string())?;
    let id = c.last_insert_rowid();
    event(c, saga_id, Some(id), actor, "created", &format!("added {} \"{title}\"", if parent_id.is_some() { "the step" } else { "the episode" }))?;
    touch_saga(c, saga_id);
    Ok(id)
}
pub fn update_episode(c: &Connection, id: i64, patch: &Value, actor: &str) -> Result<(), String> {
    let cur = rows(c, "SELECT saga_id, title, status, assignee FROM tasks WHERE id = ?1", &[json!(id)])?;
    let Some(cur) = cur.first() else { return Err("no such episode".into()) };
    let saga = cur["saga_id"].as_i64().unwrap_or(0);
    if let Some(t) = patch.get("title").and_then(Value::as_str) { let t = clean_title(t)?; c.execute("UPDATE tasks SET title = ?1 WHERE id = ?2", params![t, id]).map_err(|e| e.to_string())?; event(c, saga, Some(id), actor, "edited", &format!("renamed it to \"{t}\""))?; }
    if let Some(b) = patch.get("body").and_then(Value::as_str) { c.execute("UPDATE tasks SET body = ?1 WHERE id = ?2", params![b.trim(), id]).map_err(|e| e.to_string())?; event(c, saga, Some(id), actor, "edited", "changed the description")?; }
    if let Some(a) = patch.get("assignee").and_then(Value::as_str) {
        let a = a.trim();
        if a != cur["assignee"].as_str().unwrap_or("") {
            c.execute("UPDATE tasks SET assignee = ?1 WHERE id = ?2", params![a, id]).map_err(|e| e.to_string())?;
            event(c, saga, Some(id), actor, "assigned", &if a.is_empty() { "unassigned it".to_string() } else { format!("assigned it to {a}") })?;
        }
    }
    if let Some(s) = patch.get("status").and_then(Value::as_str) {
        if !EPISODE_STATUSES.contains(&s) { return Err(format!("status must be one of {}", EPISODE_STATUSES.join(" · "))); }
        let from = cur["status"].as_str().unwrap_or("todo");
        if s != from {
            let t = now();
            c.execute("UPDATE tasks SET status = ?1, started_ms = CASE WHEN ?1 = 'doing' AND started_ms IS NULL THEN ?3 ELSE started_ms END, done_ms = CASE WHEN ?1 IN ('done','failed','canceled') THEN ?3 ELSE NULL END WHERE id = ?2", params![s, id, t]).map_err(|e| e.to_string())?;
            event(c, saga, Some(id), actor, "status", &format!("{from} → {s}"))?;
        }
    }
    let _ = c.execute("UPDATE tasks SET updated_ms = ?1 WHERE id = ?2", params![now(), id]);
    touch_saga(c, saga);
    Ok(())
}
pub fn delete_episode(c: &Connection, id: i64) -> Result<(), String> {
    let saga: Option<i64> = c.query_row("SELECT saga_id FROM tasks WHERE id = ?1", [id], |r| r.get(0)).ok();
    let Some(saga) = saga else { return Err("no such episode".into()) };
    c.execute("DELETE FROM tasks WHERE id = ?1", [id]).map_err(|e| e.to_string())?;
    touch_saga(c, saga);
    Ok(())
}

// ------------------------------------------------------------------------------------------------------------------------------ roll-ups
/// Counts and telemetry over a set of tasks (each `{status, ms_total, usd_total, tokens, engine}`). Pure - tested.
pub fn rollup(tasks: &[Value]) -> Value {
    let mut by = [0i64; 5];
    let (mut ms, mut usd, mut tokens) = (0i64, 0f64, 0i64);
    let mut seats: std::collections::BTreeMap<String, i64> = Default::default();
    // WHO did the work (an agent's name, or `engine:model` for a run's seat): how many tasks each, and how many of them failed - the list draws a face for each
    let mut agents: std::collections::BTreeMap<String, (i64, i64)> = Default::default();
    for t in tasks {
        let who = t["assignee"].as_str().unwrap_or("").trim();
        if !who.is_empty() { let e = agents.entry(who.to_string()).or_default(); e.0 += 1; if t["status"].as_str() == Some("failed") { e.1 += 1; } }
        if let Some(i) = EPISODE_STATUSES.iter().position(|s| Some(*s) == t["status"].as_str()) { by[i] += 1; }
        ms += t["ms_total"].as_i64().unwrap_or(0); usd += t["usd_total"].as_f64().unwrap_or(0.0); tokens += t["tokens"].as_i64().unwrap_or(0);
        // a seat is the exact model that did the task (`cursor:composer-2.5`), so the screen can draw each model's own icon
        let (e, m) = (t["engine"].as_str().unwrap_or(""), t["model"].as_str().unwrap_or(""));
        if !e.is_empty() { *seats.entry(if m.is_empty() { e.to_string() } else { format!("{e}:{m}") }).or_default() += 1; }
    }
    json!({ "tasks": tasks.len(), "episodes": tasks.len(), "todo": by[0], "doing": by[1], "done": by[2], "failed": by[3], "canceled": by[4],
            "ms": ms, "usd": (usd * 10000.0).round() / 10000.0, "tokens": tokens, "seats": seats,
            "agents": agents.into_iter().map(|(k, (n, f))| (k, json!({ "n": n, "failed": f }))).collect::<serde_json::Map<String, Value>>() })
}
/// What a saga's status IS: its tasks decide, except that canceled is the user's word. No tasks yet = todo. Pure - tested.
pub fn derive_status(stored: &str, r: &Value) -> &'static str {
    // `canceled` is the user's word; `failed` is a run's verdict (a run can fail at QA with every task done); `done` with no tasks is a run answered inline
    if stored == "canceled" { return "canceled"; }
    if stored == "failed" { return "failed"; }
    let n = r["tasks"].as_i64().unwrap_or(0);
    if stored == "done" && n == 0 { return "done"; } let c = |k: &str| r[k].as_i64().unwrap_or(0);
    if n == 0 { return "todo"; }
    if c("doing") > 0 { return "doing"; }
    if c("done") + c("canceled") == n { return if c("done") > 0 { "done" } else { "canceled" }; }
    if c("failed") > 0 && c("todo") == 0 { return "failed"; }
    if c("done") > 0 || c("failed") > 0 { return "doing"; }
    "todo"
}

// ------------------------------------------------------------------------------------------------------------------------------ reads
/// Every saga with its roll-up and derived status, newest activity first.
pub fn list(c: &Connection) -> Result<Vec<Value>, String> {
    let sagas = rows(c, "SELECT id, title, objective, status, origin, plan_ref, confidence, delivered, created_ms, updated_ms, meta, run_cfg FROM sagas ORDER BY updated_ms DESC, id DESC", &[])?;
    let tasks = rows(c, "SELECT saga_id, parent_id, status, ms_total, usd_total, tokens, engine, model, assignee FROM tasks", &[])?;
    Ok(sagas.into_iter().map(|mut s| {
        let id = s["id"].as_i64().unwrap_or(0);
        let mine: Vec<Value> = tasks.iter().filter(|t| t["saga_id"].as_i64() == Some(id)).cloned().collect();
        let r = rollup(&mine);
        s["status"] = json!(derive_status(s["status"].as_str().unwrap_or("todo"), &r));
        s["rollup"] = r;
        s["meta"] = meta_of(&s["meta"]);
        s["run"] = run_of(&s["run_cfg"]); if let Some(o) = s.as_object_mut() { o.remove("run_cfg"); }
        s
    }).collect())
}
/// A saga with its episodes nested (steps under their episode), its roll-up and its activity trail (newest 300, newest first).
/// A saga's stored metadata as JSON (`null` when it has none). Pure.
/// A saga's run settings: `{engine, model, budget_usd}`, each only when chosen (`{}` when none). Pure.
pub fn run_of(v: &Value) -> Value { let m = meta_of(v); if m.is_object() { m } else { json!({}) } }
/// The rows of episodes with their `meta` text read as JSON (`null` when an episode has none). Pure.
pub fn with_meta(mut v: Vec<Value>) -> Vec<Value> { for r in v.iter_mut() { if let Some(o) = r.as_object_mut() { let m = meta_of(o.get("meta").unwrap_or(&Value::Null)); o.insert("meta".into(), m); } } v }
pub fn meta_of(v: &Value) -> Value { v.as_str().filter(|t| !t.is_empty()).and_then(|t| serde_json::from_str::<Value>(t).ok()).unwrap_or(Value::Null) }
pub fn saga_detail(c: &Connection, id: i64) -> Result<Value, String> {
    let mut saga = rows(c, "SELECT id, title, objective, status, origin, plan_ref, confidence, delivered, created_ms, updated_ms, meta, run_cfg FROM sagas WHERE id = ?1", &[json!(id)])?.into_iter().next().ok_or("no such saga")?;
    let flat = rows(c, "SELECT id, saga_id, parent_id, title, body, status, assignee, created_by, position, engine, model, ms_total, usd_total, tokens, created_ms, updated_ms, started_ms, done_ms, meta FROM tasks WHERE saga_id = ?1 ORDER BY position, id", &[json!(id)])?;
    let flat = with_meta(flat);
    let tasks = nest(&flat);
    let r = rollup(&flat);
    saga["status"] = json!(derive_status(saga["status"].as_str().unwrap_or("todo"), &r));
    saga["meta"] = meta_of(&saga["meta"]);
    saga["run"] = run_of(&saga["run_cfg"]); if let Some(o) = saga.as_object_mut() { o.remove("run_cfg"); }
    let events = rows(c, "SELECT id, task_id, t_ms, actor, kind, text FROM events WHERE saga_id = ?1 ORDER BY t_ms DESC, id DESC LIMIT 1500", &[json!(id)])?;
    let changes = crate::work_changes::list(c, id).unwrap_or_default();
    Ok(json!({ "ok": true, "saga": saga, "episodes": tasks, "tasks": tasks, "rollup": r, "events": events, "changes": changes }))
}
/// Top-level episodes, each with `steps` (also under the old word `subtasks`) and a `rollup` over itself and them. Pure - tested.
pub fn nest(flat: &[Value]) -> Vec<Value> {
    flat.iter().filter(|t| t["parent_id"].is_null()).map(|t| {
        let kids: Vec<Value> = flat.iter().filter(|k| k["parent_id"] == t["id"]).cloned().collect();
        let mut all = vec![t.clone()]; all.extend(kids.iter().cloned());
        let mut t = t.clone(); t["steps"] = json!(kids); t["subtasks"] = json!(kids); t["rollup"] = rollup(&all); t
    }).collect()
}
pub fn episode_detail(c: &Connection, id: i64) -> Result<Value, String> {
    let t = rows(c, "SELECT id, saga_id, parent_id, title, body, status, assignee, created_by, position, engine, model, ms_total, usd_total, tokens, created_ms, updated_ms, started_ms, done_ms, meta FROM tasks WHERE id = ?1", &[json!(id)])?.into_iter().next().ok_or("no such episode")?;
    let t = with_meta(vec![t]).remove(0);
    let kids = rows(c, "SELECT id, saga_id, parent_id, title, body, status, assignee, created_by, position, engine, model, ms_total, usd_total, tokens, created_ms, updated_ms, started_ms, done_ms, meta FROM tasks WHERE parent_id = ?1 ORDER BY position, id", &[json!(id)])?;
    let kids = with_meta(kids);
    let mut all = vec![t.clone()]; all.extend(kids.iter().cloned());
    let saga = rows(c, "SELECT id, title FROM sagas WHERE id = ?1", &[json!(t["saga_id"])])?.into_iter().next().unwrap_or(json!({}));
    let events = rows(c, "SELECT id, task_id, t_ms, actor, kind, text FROM events WHERE task_id = ?1 OR task_id IN (SELECT id FROM tasks WHERE parent_id = ?1) ORDER BY t_ms DESC, id DESC LIMIT 1500", &[json!(id)])?;
    Ok(json!({ "ok": true, "episode": t, "task": t, "saga": saga, "steps": kids, "subtasks": kids, "rollup": rollup(&all), "events": events }))
}

// ------------------------------------------------------------------------------------------------------------------------------ plans become sagas
/// A goal run, as far as the sagas care (decoupled from `goals::GoalRun` so the mapping is testable without a run).
#[derive(Clone, Debug, Default)]
pub struct RunView {
    /// when the run was made and when it last did anything (its end, else its newest event) - the saga carries the RUN's times, not the time it was first read
    pub created: i64, pub updated: i64,
    pub id: i64, pub goal: String, pub status: String, pub confidence: String, pub delivered: bool, pub plan_usd: f64,
    pub tasks: Vec<RunTaskView>, pub events: Vec<(i64, String, String, String, Option<String>)>, // (t, id, kind, text, task)
}
#[derive(Clone, Debug, Default)]
pub struct RunTaskView { pub id: String, pub title: String, pub status: String, pub started: Option<i64>, pub ended: Option<i64>, pub engine: String, pub model: String, pub ms: i64, pub usd: f64, pub tokens: i64 }

/// A run task's status in the vocabulary of the sagas. Pure - tested.
pub fn task_status_of(run_task: &str, run: &str) -> &'static str {
    match run_task {
        "running" | "verifying" => "doing",
        "done" => "done",
        "failed" | "blocked" => "failed",
        "skipped" => "canceled",
        _ => if run == "cancelled" { "canceled" } else { "todo" },
    }
}
/// A run's own time, or now when it carries none.
fn stamp(t: i64) -> i64 { if t > 0 { t } else { now() } }
/// A saga's title from an ask: its first line, cut at a WORD and marked with an ellipsis when it was longer than `n` (never mid-word). Pure - tested.
pub fn short_title(goal: &str, n: usize) -> String {
    let line = goal.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if line.chars().count() <= n { return line.to_string(); }
    let cut: String = line.chars().take(n).collect();
    let at = cut.rfind(' ').filter(|i| *i > n / 2).unwrap_or(cut.len());
    format!("{}…", cut[..at].trim_end_matches([',', ';', ':', '-', ' ']))
}
fn first_line(s: &str, n: usize) -> String { s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(n).collect() }

/// The status a run gives its saga when the tasks alone would not tell the truth. Pure - tested.
pub fn saga_status_of_run(run: &str, no_tasks: bool) -> &'static str {
    match run { "failed" => "failed", "cancelled" => "canceled", "inline" => "done", "done" if no_tasks => "done", _ => "todo" }
}
/// What a run looks like to the sync: if it is the same as last time, there is nothing to write. Pure - tested.
pub fn fingerprint(r: &RunView) -> String {
    format!("v4|{}|{}|{}|{}|{}|{}", r.status, r.delivered as i64, r.confidence, r.events.len(), r.tasks.len(),
        r.tasks.iter().map(|t| format!("{}:{}:{}:{:.4}:{}", t.id, t.status, t.ms, t.usd, t.engine)).collect::<Vec<_>>().join(","))
}

/// Upsert one saga per run and one task per run task. Titles are written when a row is created (the user may rename them); status, time, cost and seat are always the run's.
/// Returns how many rows changed. Idempotent.
pub fn sync_runs(c: &Connection, runs: &[RunView]) -> Result<usize, String> {
    let mut changed = 0usize;
    for r in runs {
        let key = format!("run:{}", r.id);
        let sig = fingerprint(r); let before = changed;
        let stored = saga_status_of_run(&r.status, r.tasks.is_empty());
        let seen: Option<String> = c.query_row("SELECT sig FROM sagas WHERE plan_ref = ?1", [&key], |x| x.get(0)).ok();
        if seen.as_deref() == Some(sig.as_str()) { continue; }
        let tx = c.unchecked_transaction().map_err(|e| e.to_string())?;
        let saga: Option<i64> = tx.query_row("SELECT id FROM sagas WHERE plan_ref = ?1", [&key], |x| x.get(0)).ok();
        let saga = match saga {
            Some(id) => {
                // a title an older build cut mid-word (the first 120 characters) is made whole once; a title the user changed is never touched
                let old = first_line(&r.goal, 120);
                if old.chars().count() >= 120 { tx.execute("UPDATE sagas SET title = ?1 WHERE id = ?2 AND title = ?3", params![short_title(&r.goal, 100), id, old]).map_err(|e| e.to_string())?; }
                tx.execute("UPDATE sagas SET confidence = ?1, delivered = ?2 WHERE id = ?3 AND (confidence != ?1 OR delivered != ?2)", params![r.confidence, r.delivered as i64, id]).map_err(|e| e.to_string())?; id }
            None => {
                let title = short_title(&r.goal, 100);
                tx.execute("INSERT INTO sagas (title, objective, origin, plan_ref, confidence, delivered, created_ms, updated_ms) VALUES (?1, ?2, 'heimdall', ?3, ?4, ?5, ?6, ?7)",
                    params![if title.is_empty() { format!("run {}", r.id) } else { title }, r.goal.trim(), key, r.confidence, r.delivered as i64, stamp(r.created), stamp(r.updated.max(r.created))]).map_err(|e| e.to_string())?;
                let id = tx.last_insert_rowid();
                tx.execute("INSERT INTO events (saga_id, t_ms, actor, kind, text, ext) VALUES (?1, ?2, 'heimdall', 'created', 'planned this saga from your ask', ?3)", params![id, stamp(r.created), format!("{key}:created")]).map_err(|e| e.to_string())?;
                changed += 1; id
            }
        };
        for (i, t) in r.tasks.iter().enumerate() {
            let ext = format!("{key}:{}", t.id);
            let st = task_status_of(&t.status, &r.status);
            let assignee = if t.engine.is_empty() { String::new() } else if t.model.is_empty() { t.engine.clone() } else { format!("{}:{}", t.engine, t.model) };
            let cur: Option<(i64, String, i64, f64)> = tx.query_row("SELECT id, status, ms_total, usd_total FROM tasks WHERE ext = ?1", [&ext], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?))).ok();
            match cur {
                None => {
                    tx.execute("INSERT INTO tasks (saga_id, title, body, status, assignee, created_by, position, ext, engine, model, ms_total, usd_total, tokens, created_ms, updated_ms, started_ms, done_ms)
                                VALUES (?1, ?2, '', ?3, ?4, 'planner', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12, ?13, ?14)",
                        params![saga, first_line(&t.title, MAX_TITLE), st, assignee, i as i64 + 1, ext, t.engine, t.model, t.ms, t.usd, t.tokens, stamp(r.created), t.started, t.ended]).map_err(|e| e.to_string())?;
                    changed += 1;
                }
                Some((id, old, ms, usd)) => {
                    if old != st || ms != t.ms || (usd - t.usd).abs() > 1e-9 {
                        tx.execute("UPDATE tasks SET status = ?1, assignee = CASE WHEN ?2 != '' THEN ?2 ELSE assignee END, engine = ?3, model = ?4, ms_total = ?5, usd_total = ?6, tokens = ?7, updated_ms = ?8, started_ms = ?9, done_ms = ?10 WHERE id = ?11",
                            params![st, assignee, t.engine, t.model, t.ms, t.usd, t.tokens, stamp(r.updated), t.started, t.ended, id]).map_err(|e| e.to_string())?;
                        if old != st {
                            tx.execute("INSERT OR IGNORE INTO events (saga_id, task_id, t_ms, actor, kind, text, ext) VALUES (?1, ?2, ?3, ?4, 'status', ?5, ?6)",
                                params![saga, id, stamp(r.updated), if assignee.is_empty() { "heimdall".to_string() } else { assignee.clone() }, format!("{old} → {st}"), format!("{ext}:{old}>{st}")]).map_err(|e| e.to_string())?;
                        }
                        changed += 1;
                    }
                }
            }
        }
        // the run's own story, as the saga's trail (each line once)
        for (t, id, kind, text, task) in &r.events {
            let tid: Option<i64> = task.as_ref().and_then(|tk| tx.query_row("SELECT id FROM tasks WHERE ext = ?1", [format!("{key}:{tk}")], |x| x.get(0)).ok());
            let n = tx.execute("INSERT OR IGNORE INTO events (saga_id, task_id, t_ms, actor, kind, text, ext) VALUES (?1, ?2, ?3, 'heimdall', ?4, ?5, ?6)",
                params![saga, tid, t, kind, text.chars().take(600).collect::<String>(), format!("{key}:e:{t}:{id}:{kind}")]).map_err(|e| e.to_string())?;
            changed += n;
        }
        // the run's verdict, but never over the user's own cancel (unless the run itself was cancelled)
        tx.execute("UPDATE sagas SET status = ?1 WHERE id = ?2 AND (?1 = 'canceled' OR status != 'canceled')", params![stored, saga]).map_err(|e| e.to_string())?;
        if changed > before { tx.execute("UPDATE sagas SET updated_ms = MAX(updated_ms, ?1) WHERE id = ?2", params![stamp(r.updated), saga]).map_err(|e| e.to_string())?; }
        tx.execute("UPDATE sagas SET sig = ?1 WHERE id = ?2", params![sig, saga]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
    }
    Ok(changed)
}

/// An enum as its wire word ("awaiting-answers", "running", ...).
fn wire<T: serde::Serialize>(v: &T) -> String { serde_json::to_value(v).ok().and_then(|x| x.as_str().map(str::to_string)).unwrap_or_default() }
/// `goals::GoalRun` -> `RunView`. The seat that answered, its time, cost and tokens come from the task's own trace.
pub fn view_of(r: &crate::goals::GoalRun) -> RunView {
    RunView {
        created: r.created, updated: r.ended.or_else(|| r.events.iter().map(|e| e.t).max()).unwrap_or(r.created).max(r.created),
        id: r.id, goal: r.goal.clone(), status: wire(&r.status), confidence: r.route.as_ref().map(|x| x.confidence.clone()).unwrap_or_default(), delivered: r.delivered, plan_usd: r.plan_usd,
        tasks: r.tasks.iter().map(|t| RunTaskView {
            id: t.id.clone(), title: t.title.clone(), status: wire(&t.status), started: t.started, ended: t.ended,
            engine: t.trace.as_ref().map(|x| x.engine.key().to_string()).unwrap_or_default(), model: t.trace.as_ref().and_then(|x| x.model.clone()).unwrap_or_default(),
            ms: t.trace.as_ref().map(|x| x.ms).unwrap_or_else(|| match (t.started, t.ended) { (Some(a), Some(b)) if b >= a => b - a, _ => 0 }),
            usd: t.trace.as_ref().and_then(|x| x.cost).unwrap_or(0.0), tokens: t.trace.as_ref().map(|x| x.tokens_in.unwrap_or(0) as i64 + x.tokens_out.unwrap_or(0) as i64).unwrap_or(0),
        }).collect(),
        events: r.events.iter().map(|e| (e.t, e.id.clone(), e.kind.clone(), e.text.clone(), e.task.clone())).collect(),
    }
}

// ------------------------------------------------------------------------------------------------------------------------------ http
use crate::dispatch::AppState;
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use std::sync::{Arc, Mutex};

fn bad(m: &str) -> Response { (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": m }))).into_response() }

/// The goal runs that belong to THIS project (see `in_project`). Goal runs are kept in one file for every project, so this is what stops a project showing another's.
fn runs_of_project(state: &Arc<AppState>, goals: &crate::goals::Goals, root: &str) -> Vec<RunView> {
    let boxes: Vec<String> = {
        let conn = state.pick(); let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        antiloki_rs::db_json::query_rows(&c, "SELECT worktree_path FROM cockpit_panes WHERE worktree_path IS NOT NULL", &[]).ok()
            .and_then(|v| v.as_array().cloned()).unwrap_or_default().iter().filter_map(|r| r["worktree_path"].as_str().map(str::to_string)).collect()
    };
    goals.list(None).iter().filter(|r| in_project(&r.cwd, root, &boxes, &|c| same_repo(c, root))).map(view_of).collect()
}
/// `ministore-2` -> `ministore`: the name the project's clones carry. Pure - tested.
pub fn base_name(root: &str) -> String {
    let n = norm(root); let leaf = n.rsplit('/').next().unwrap_or("").to_string();
    match leaf.rsplit_once('-') { Some((b, tail)) if !b.is_empty() && !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => b.to_string(), _ => leaf }
}
fn norm(s: &str) -> String { s.trim().trim_end_matches(['/', '\\']).replace('\\', "/").to_lowercase() }
/// A sibling folder named after the project: `ministore-merge-L-claude-composer`, `ministore-freeze-1` next to `ministore-2`. These are the clones a benchmark or a fork works in. Pure - tested.
pub fn is_named_clone(cwd: &str, root: &str) -> bool {
    let (c, r) = (norm(cwd), norm(root));
    let (cp, rp) = (c.rsplit_once('/'), r.rsplit_once('/'));
    let (Some((cdir, cleaf)), Some((rdir, _))) = (cp, rp) else { return false };
    let base = base_name(root);
    !base.is_empty() && cdir == rdir && (cleaf == base || cleaf.starts_with(&format!("{base}-")))
}
/// Is a run's folder this project's? Yes when it is the project folder or inside it, exactly one of its panes' boxes, a sibling clone named after it, or a checkout of the SAME repository (`same_repo`: the same
/// root commit). Pure given `same_repo` - tested.
pub fn in_project(cwd: &str, root: &str, boxes: &[String], same_repo: &dyn Fn(&str) -> bool) -> bool {
    let (c, r) = (norm(cwd), norm(root));
    if c.is_empty() || r.is_empty() { return false; }
    c == r || c.starts_with(&format!("{r}/")) || boxes.iter().any(|b| norm(b) == c) || is_named_clone(cwd, root) || same_repo(cwd)
}
/// The commit a repository started from (its first), cached for a minute per folder: asking git for every run of a long history on every poll would be a lot of git.
fn root_commit(dir: &str) -> Option<String> {
    static CACHE: std::sync::OnceLock<Mutex<std::collections::HashMap<String, (std::time::Instant, Option<String>)>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some((at, v)) = cache.lock().unwrap_or_else(|p| p.into_inner()).get(dir) { if at.elapsed() < std::time::Duration::from_secs(60) { return v.clone(); } }
    let v = crate::proc::cmd("git").args(["rev-list", "--max-parents=0", "HEAD"]).current_dir(dir).output().ok().filter(|o| o.status.success())
        .and_then(|o| String::from_utf8_lossy(&o.stdout).lines().last().map(|l| l.trim().to_string())).filter(|l| !l.is_empty());
    cache.lock().unwrap_or_else(|p| p.into_inner()).insert(dir.to_string(), (std::time::Instant::now(), v.clone()));
    v
}
fn same_repo(cwd: &str, root: &str) -> bool { match (root_commit(cwd), root_commit(root)) { (Some(a), Some(b)) => a == b, _ => false } }

/// Every registered workspace as (name, root). Blocking (it reads the registry).
fn workspaces_all() -> Vec<(String, String)> {
    antiloki_rs::system_workspace_list::workspaceList(&json!({}), None).ok().and_then(|v| v["workspaces"].as_array().cloned()).unwrap_or_default().iter()
        .filter_map(|w| Some((w["name"].as_str()?.to_string(), w["root_path"].as_str()?.to_string()))).filter(|(_, r)| !r.is_empty()).collect()
}
/// EVERY TOKEN EVER (the operator 2026-11-22: "tokens spent in total from all projects in all times"): the calls of every registered workspace, summed. Blocking.
pub fn tokens_total() -> Value {
    let mut seen = std::collections::HashSet::new(); let (mut t, mut i, mut o, mut calls, mut projects) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0u32);
    for (_, root) in workspaces_all() { if !seen.insert(root.clone()) { continue; } let rows = crate::episodes_db::calls_by_model(&root, 0, 0); if rows.is_empty() { continue; } projects += 1;
        for m in rows { let g = |k: &str| m.get(k).and_then(Value::as_f64).unwrap_or(0.0); t += g("tokens"); i += g("tokens_in"); o += g("tokens_out"); calls += g("calls"); } }
    json!({ "ok": true, "tokens": t, "tokens_in": i, "tokens_out": o, "calls": calls, "projects": projects })
}
/// `GET /api/v1/cockpit/tokens-total`, cached for a minute (it opens every project's record).
pub async fn tokens_total_get() -> axum::response::Response {
    static CACHE: std::sync::Mutex<Option<(i64, Value)>> = std::sync::Mutex::new(None);
    let now = crate::cli::now_ms() as i64; if let Some((t, v)) = CACHE.lock().ok().and_then(|c| c.clone()) { if now - t < 60_000 { return axum::Json(v).into_response(); } }
    let v = tokio::task::spawn_blocking(tokens_total).await.unwrap_or_else(|_| json!({ "ok": false })); if let Ok(mut c) = CACHE.lock() { *c = Some((now, v.clone())); } axum::Json(v).into_response()
}
/// The root a request is about: the workspace it names (a registered one), else the open one. Blocking.
fn root_for(st: &Arc<AppState>, workspace: &str) -> Result<String, String> {
    let ws = workspace.trim(); if ws.is_empty() { return Ok(crate::chat_pane::workspace_root_of(st)); }
    workspaces_all().into_iter().find(|(n, _)| n == ws).map(|(_, r)| r).ok_or_else(|| format!("no workspace named \"{ws}\""))
}
/// The name of the workspace whose root this is (the open one's, when it is not found).
fn name_of(root: &str) -> String { workspaces_all().into_iter().find(|(_, r)| r == root).map(|(n, _)| n).unwrap_or_default() }

async fn with_db<T: Send + 'static>(state: &Arc<AppState>, f: impl FnOnce(&Connection, &str) -> Result<T, String> + Send + 'static) -> Result<T, String> { with_db_in(state, String::new(), f).await }
pub(crate) async fn with_db_in<T: Send + 'static>(state: &Arc<AppState>, workspace: String, f: impl FnOnce(&Connection, &str) -> Result<T, String> + Send + 'static) -> Result<T, String> {
    let st = state.clone();
    tokio::task::spawn_blocking(move || { let root = root_for(&st, &workspace)?; let c = open(&root)?; f(&c, &root) }).await.map_err(|e| e.to_string())?
}

/// Every agent box of the open project (a terminal pane's worktree with the engine's own CLI in it): who, which engine, where. What the sagas read their trails from.
pub(crate) fn boxes_of_project(st: &Arc<AppState>) -> Vec<crate::work_boxes::BoxRef> {
    let conn = st.pick();
    let rows = { let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        antiloki_rs::db_json::query_rows(&c, "SELECT p.id, p.subagent_name, p.pane_kind, p.worktree_path, p.settings_json FROM cockpit_panes p WHERE COALESCE(p.status,'') != 'closed' AND COALESCE(p.worktree_path,'') != '' AND p.pane_kind IN ('claude-code','codex','opencode','gemini','cursor','grok')", &[]).unwrap_or(json!([])) };
    rows.as_array().cloned().unwrap_or_default().iter().filter_map(|r| {
        let dir = PathBuf::from(r["worktree_path"].as_str()?); if !dir.join(".antiloki").is_dir() { return None; }
        let title = serde_json::from_str::<Value>(r["settings_json"].as_str().unwrap_or("{}")).ok().and_then(|v| v["title"].as_str().map(String::from)).filter(|t| !t.is_empty());
        Some(crate::work_boxes::BoxRef { agent: title.unwrap_or_else(|| r["pane_kind"].as_str().unwrap_or("agent").to_string()), engine: r["pane_kind"].as_str()?.to_string(), dir, pane_id: r["id"].as_i64().unwrap_or(0), shared: false })
    }).collect()
}
/// Every agent whose trail the sagas read: the boxes, and the agents that share the project's folder. ONLY for reading: what commits, restores or merges an agent's files takes `boxes_of_project`, so it can never touch a shared folder.
pub(crate) fn trails_of_project(st: &Arc<AppState>, root: &str) -> Vec<crate::work_boxes::BoxRef> {
    let mut all = boxes_of_project(st);
    let conn = st.pick();
    let rows = { let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        antiloki_rs::db_json::query_rows(&c, "SELECT p.id, p.subagent_name, p.pane_kind, p.settings_json FROM cockpit_panes p WHERE COALESCE(p.status,'') != 'closed' AND COALESCE(p.worktree_path,'') = '' AND p.pane_kind IN ('claude-code','codex','opencode','gemini','cursor','grok')", &[]).unwrap_or(json!([])) };
    all.extend(shared_trails(root, rows.as_array().map_or(&[][..], |a| a.as_slice())));
    all
}
/// The rows of agents with no box that have records under `root`. Pure over those folders.
fn shared_trails(root: &str, rows: &[Value]) -> Vec<crate::work_boxes::BoxRef> {
    rows.iter().filter_map(|r| {
        let pane_id = r["id"].as_i64()?; if root.is_empty() { return None; }
        let folder = crate::shared_folder::folder_of(root, r); if !crate::shared_folder::records(&folder, pane_id).is_dir() { return None; }
        let title = serde_json::from_str::<Value>(r["settings_json"].as_str().unwrap_or("{}")).ok().and_then(|v| v["title"].as_str().map(String::from)).filter(|t| !t.is_empty());
        Some(crate::work_boxes::BoxRef { agent: title.unwrap_or_else(|| r["pane_kind"].as_str().unwrap_or("agent").to_string()), engine: r["pane_kind"].as_str()?.to_string(), dir: PathBuf::from(folder), pane_id, shared: true })
    }).collect()
}

/// `GET /api/v1/sagas[?all=1][&workspace=name]`. The open workspace is synced from its goal runs and its agent boxes on every read; with `all=1` the other registered workspaces are listed too, from what
/// they last saved (each saga says which workspace it is in).
/// IS A SYNC DUE? The sagas of the open workspace are brought up to date with its runs and its agents' boxes (git diffs and all) on every list and every detail; with several screens asking, that was about a second of the serve's one writer connection each time (measured 2026-11-16: 1.1 s for 210 KB on the optimized build). A sync done in the last 3 seconds is as good as a new one. Per root. Pure over the clock.
pub fn sync_due(root: &str, now: std::time::Instant) -> bool {
    static LAST: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>> = std::sync::OnceLock::new();
    let mut m = LAST.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner());
    match m.get(root) { Some(t) if now.duration_since(*t) < std::time::Duration::from_secs(3) => false, _ => { m.insert(root.to_string(), now); true } }
}
/// The words of a saga with the paste markup taken out: an agent's saga is titled by its first prompt, and a prompt that was a pasted block starts with `<pasted_content id="…">` (or Claude's `[Pasted text #1 +12 lines]`), which is not a title. Pure.
pub fn strip_paste_markup(s: &str) -> String {
    let mut out = String::new(); let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]); let tail = &rest[i..];
        if tail.starts_with("<pasted_content") || tail.starts_with("</pasted_content") { match tail.find('>') { Some(j) => { rest = &tail[j + 1..]; continue } None => { rest = ""; break } } }
        out.push('<'); rest = &tail[1..];
    }
    out.push_str(rest);
    let mut clean = String::new(); let mut r = out.as_str();
    while let Some(i) = r.find("[Pasted text") { clean.push_str(&r[..i]); match r[i..].find(']') { Some(j) => r = &r[i + j + 1..], None => { r = ""; break } } }
    clean.push_str(r); clean.split_whitespace().collect::<Vec<_>>().join(" ")
}
/// A saga as the list shows it: the title and the words under it without paste markup; a title left empty by that is the first words of what is under it, else "untitled saga". Pure over the row.
pub fn clean_paste_words(sg: &mut Value) {
    for k in ["title", "summary", "objective"] { if let Some(t) = sg[k].as_str() { let c = strip_paste_markup(t); sg[k] = json!(c); } }
    if sg["title"].as_str().map_or(false, |t| t.is_empty()) {
        let from = ["summary", "objective"].iter().filter_map(|k| sg[*k].as_str()).find(|t| !t.is_empty()).map(|t| t.chars().take(60).collect::<String>());
        sg["title"] = json!(from.unwrap_or_else(|| "untitled saga".to_string()));
    }
}
pub async fn http_list(state: Arc<AppState>, goals: Arc<crate::goals::Goals>, q: std::collections::HashMap<String, String>) -> Response {
    let st = state.clone(); let all = q.get("all").map_or(false, |v| v == "1"); let want = q.get("workspace").cloned().unwrap_or_default();
    // the list of hundreds of runs is made at most every 2 s however often the page asks (swr.rs): the last answer is served at once
    let key = format!("sagas:{all}:{want}");
    let out = tokio::task::spawn_blocking(move || crate::swr::get(&key, std::time::Duration::from_secs(2), move || -> Result<Value, String> {
        let active = crate::chat_pane::workspace_root_of(&st);
        let regs = workspaces_all();
        let targets: Vec<(String, String)> = if all { let mut t = regs.clone(); if !t.iter().any(|(_, r)| *r == active) && !active.is_empty() { t.push((name_of(&active), active.clone())); } t }
            else { let r = root_for(&st, &want)?; vec![(regs.iter().find(|(_, x)| *x == r).map(|(n, _)| n.clone()).unwrap_or_default(), r)] };
        let mut sagas = vec![]; let mut names = vec![];
        for (name, root) in targets {
            names.push(json!({ "name": name, "root": root, "active": root == active }));
            let live = root == active;
            if !live && !db_path(&root).exists() { continue; }
            let c = match open(&root) { Ok(c) => c, Err(_) => continue };
            // the sync (the git diff of every agent's box: 15 to 20 s of CPU) never runs on a request: with a few pages polling, a sync per request filled every core and nothing answered (found 2026-11-18, an update that could not become healthy). One background sync at a time; the list reads what is stored.
            if live { start_background_sync(&st, &goals, &root); }
            let mut cache: std::collections::HashMap<i64, Vec<Value>> = Default::default(); let mctx = crate::team_member::MemberCtx::load(&root);
            for mut sg in list(&c)? { sg["workspace"] = json!(name);
                clean_paste_words(&mut sg);
                // an agent's own saga says what the agent belongs to (a thread, a pair, a team, the Leader of a team)
                if sg["meta"]["kind"] == json!("episode") { if let Some(pid) = sg["meta"]["pane_id"].as_i64() { let m = cache.entry(pid).or_insert_with(|| mctx.of(pid)).clone(); if !m.is_empty() { sg["meta"]["member_of"] = json!(m); } } }
                sagas.push(sg); }
        }
        sagas.sort_by(|a, b| b["updated_ms"].as_i64().cmp(&a["updated_ms"].as_i64()));
        Ok(json!({ "ok": true, "sagas": sagas, "workspaces": names }))
    })).await.unwrap_or_else(|e| Err(e.to_string()));
    match out { Ok(v) => Json(v).into_response(), Err(e) => bad(&e) }
}
static DETAIL_SYNCING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Bring the workspace's sagas up to date in a thread of its own: at most one at a time, and not more often than `sync_due` allows. Returns at once.
fn start_background_sync(st: &Arc<AppState>, goals: &Arc<crate::goals::Goals>, root: &str) {
    // the FIRST read of a workspace after the serve started syncs where it stands (there is nothing stored to show yet); every later one reads what is stored and syncs in the background
    static FIRST: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    if FIRST.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner()).insert(root.to_string()) { if let Ok(c) = open(root) { let _ = sync_runs(&c, &runs_of_project(st, goals, root)); crate::work_boxes::sync_project(&c, &trails_of_project(st, root), root); } let _ = sync_due(root, std::time::Instant::now()); return; }
    if !sync_due(root, std::time::Instant::now()) || DETAIL_SYNCING.swap(true, std::sync::atomic::Ordering::SeqCst) { return; }
    let (st2, goals2, root2) = (st.clone(), goals.clone(), root.to_string());
    std::thread::spawn(move || { if let Ok(c2) = open(&root2) { let _ = sync_runs(&c2, &runs_of_project(&st2, &goals2, &root2)); crate::work_boxes::sync_project(&c2, &trails_of_project(&st2, &root2), &root2); } DETAIL_SYNCING.store(false, std::sync::atomic::Ordering::SeqCst); });
}
pub async fn http_detail(state: Arc<AppState>, goals: Arc<crate::goals::Goals>, q: std::collections::HashMap<String, String>) -> Response {
    let id = q.get("id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0); let want = q.get("workspace").cloned().unwrap_or_default();
    let st = state.clone();
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let root = root_for(&st, &want)?; let c = open(&root)?;
        // READING A SAGA NEVER WAITS FOR THE SYNC (2026-11-18: "read a saga is very slow": 15 to 20 s a read, the git diff of every agent's box, on the request): the saga is read from what is stored, and the workspace is brought up to date in the background, at most one sync at a time
        if root == crate::chat_pane::workspace_root_of(&st) { start_background_sync(&st, &goals, &root); }
        let mut v = saga_detail(&c, id)?; v["saga"]["workspace"] = json!(name_of(&root)); Ok(v)
    }).await.unwrap_or_else(|e| Err(e.to_string()));
    match out { Ok(v) => Json(v).into_response(), Err(e) => (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": e }))).into_response() }
}
pub async fn http_episode(state: Arc<AppState>, q: std::collections::HashMap<String, String>) -> Response {
    let id = q.get("id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    let want = q.get("workspace").cloned().unwrap_or_default();
    match with_db_in(&state, want, move |c, _| episode_detail(c, id)).await { Ok(v) => Json(v).into_response(), Err(e) => (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": e }))).into_response() }
}
/// GET /api/v1/sagas/change?id=N: one snapshot of what a saga changed, with its patch.
pub async fn http_change(state: Arc<AppState>, q: std::collections::HashMap<String, String>) -> Response {
    let id = q.get("id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    let want = q.get("workspace").cloned().unwrap_or_default();
    match with_db_in(&state, want, move |c, _| crate::work_changes::get(c, id).map(|r| json!({ "ok": true, "change": r }))).await { Ok(v) => Json(v).into_response(), Err(e) => (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": e }))).into_response() }
}
/// Who a write came from. The screen sends no pane: that is the user. The agents' MCP tools send the pane they run in, and the trail then names that agent (its recipe name, else its engine).
pub fn actor_of_pane(pane: &Value) -> String {
    let settings: Value = pane.get("settings_json").and_then(Value::as_str).and_then(|t| serde_json::from_str(t).ok()).unwrap_or(json!({}));
    let agent = settings.get("agent").and_then(Value::as_str).unwrap_or("").trim();
    if !agent.is_empty() { return agent.to_string(); }
    let kind = pane.get("pane_kind").and_then(Value::as_str).unwrap_or("").trim();
    if kind.is_empty() { "agent".into() } else { kind.to_string() }
}

/// POST /api/v1/sagas/{op}. Writes made from the screen are the user's (`you`); a write that carries a `pane_id` came from that pane's agent.
pub async fn http_op(state: Arc<AppState>, op: String, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
    let pane_id = b.get("pane_id").and_then(Value::as_i64).unwrap_or(0);
    let actor = if pane_id > 0 { match crate::chat_pane::pane_row(&state, pane_id).await { Some(p) => actor_of_pane(&p), None => return bad("no such pane") } } else { "you".to_string() };
    let want = b.get("workspace").and_then(Value::as_str).unwrap_or("").to_string();
    let out = with_db_in(&state, want, move |c, _| -> Result<Value, String> {
        let s = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let n = |k: &str| b.get(k).and_then(Value::as_i64).unwrap_or(0);
        match op.as_str() {
            "create" => { let id = create_saga(c, &s("title"), &s("objective"), &actor)?; saga_detail(c, id) }
            "update" => { update_saga(c, n("id"), &b, &actor)?; saga_detail(c, n("id")) }
            "delete" => { delete_saga(c, n("id"))?; Ok(json!({ "ok": true })) }
            "task-create" | "episode-create" => { let parent = b.get("parent_id").and_then(Value::as_i64); let id = create_episode(c, n("saga_id"), parent, &s("title"), &s("body"), &s("assignee"), &actor)?; episode_detail(c, id) }
            "task-update" | "episode-update" => { update_episode(c, n("id"), &b, &actor)?; episode_detail(c, n("id")) }
            "task-delete" | "episode-delete" => { delete_episode(c, n("id"))?; Ok(json!({ "ok": true })) }
            other => Err(format!("unknown op {other}")),
        }
    }).await;
    match out { Ok(v) => Json(v).into_response(), Err(e) => bad(&e) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_timeline_gives_titles_for_all_and_details_for_the_last_only() {
        use super::{timeline_text, PastSaga};
        let s = |id: i64, title: &str, n: usize| PastSaga { id, updated_ms: 1_795_000_000_000, title: title.into(), status: "done".into(), tasks: (0..n).map(|k| (format!("episode {k}"), format!("detail of {title} {k}"))).collect() };
        assert!(timeline_text(&[]).is_empty());
        let t = timeline_text(&[s(1, "first", 1), s(2, "second", 8)]);
        assert!(t.contains("saga #1") && t.contains("saga #2") && t.contains("(3 earlier episode(s))") && t.contains("detail of second 7") && !t.contains("detail of first"), "{t}");
        let many: Vec<PastSaga> = (0..30).map(|i| s(i, "x", 1)).collect(); let t = timeline_text(&many); assert!(t.contains("18 older saga(s)") && !t.contains("saga #3 ") && t.len() < 6000);
    }

    #[test]
    fn a_saga_titled_by_a_pasted_block_shows_words_and_not_the_markup() {
        assert_eq!(strip_paste_markup("<pasted_content id=\"b667\">"), ""); assert_eq!(strip_paste_markup("<pasted_content id=\"a\">fix the login</pasted_content> now"), "fix the login now");
        assert_eq!(strip_paste_markup("[Pasted text #1 +12 lines] add dark mode"), "add dark mode"); assert_eq!(strip_paste_markup("a < b and c > d"), "a < b and c > d", "other angle brackets stay");
        let mut sg = json!({ "title": "<pasted_content id=\"b667\">", "summary": "make the build faster\n\nplease" }); clean_paste_words(&mut sg); assert_eq!(sg["title"], json!("make the build faster please")); assert_eq!(sg["summary"], json!("make the build faster please"));
        let mut e = json!({ "title": "<pasted_content id=\"x\">" }); clean_paste_words(&mut e); assert_eq!(e["title"], json!("untitled saga"));
    }
    #[test]
    fn a_sync_done_a_moment_ago_is_not_done_again_and_roots_do_not_share_it() {
        let t0 = std::time::Instant::now(); assert!(sync_due("/sync-test/a", t0), "the first is due"); assert!(!sync_due("/sync-test/a", t0 + std::time::Duration::from_secs(2)), "2 s later it is not"); assert!(sync_due("/sync-test/b", t0), "another root is its own");
        assert!(sync_due("/sync-test/a", t0 + std::time::Duration::from_secs(4)), "after 3 s it is due again"); assert!(!sync_due("/sync-test/a", t0 + std::time::Duration::from_secs(5)), "and that one restarted the wait");
    }
    use super::*;
    fn db() -> Connection { let c = Connection::open_in_memory().unwrap(); init(&c).unwrap(); c }
    fn t(c: &Connection, saga: i64, title: &str) -> i64 { create_episode(c, saga, None, title, "", "", "you").unwrap() }

    #[test]
    fn a_run_made_from_a_hand_made_saga_replaces_the_copy_the_sync_already_made_of_it() {
        let c = db(); let mine = create_saga(&c, "by hand", "", "you").unwrap(); let a = t(&c, mine, "one"); let b = t(&c, mine, "two");
        // the sync read the run before the saga took it: a saga of its own, with tasks of its own
        let rv = RunView { created: 1, updated: 2, id: 9, goal: "by hand".into(), status: "running".into(), confidence: String::new(), delivered: false, plan_usd: 0.0,
            tasks: vec![RunTaskView { id: format!("s{a}"), title: "one".into(), status: "running".into(), started: Some(1), ended: None, engine: "claude-code".into(), model: String::new(), ms: 0, usd: 0.0, tokens: 0 }, RunTaskView { id: format!("s{b}"), title: "two".into(), status: "pending".into(), started: None, ended: None, engine: String::new(), model: String::new(), ms: 0, usd: 0.0, tokens: 0 }], events: vec![] };
        sync_runs(&c, &[rv.clone()]).unwrap(); assert_eq!(list(&c).unwrap().len(), 2, "the sync made a copy");
        link_run(&c, mine, 9, &[(a, format!("s{a}")), (b, format!("s{b}"))], "you").unwrap();
        assert_eq!(list(&c).unwrap().len(), 1, "the copy is gone: one saga");
        sync_runs(&c, &[rv]).unwrap(); let d = saga_detail(&c, mine).unwrap();
        assert_eq!(d["tasks"].as_array().unwrap().len(), 2, "the tasks are the saga's own, not doubled"); assert_eq!(d["saga"]["plan_ref"], json!("run:9")); assert_eq!(d["tasks"][0]["status"], json!("doing"));
    }
    #[test]
    fn a_saga_holds_episodes_and_steps_one_level_deep() {
        let c = db(); let s = create_saga(&c, "  Add Stripe checkout ", "", "you").unwrap();
        let a = t(&c, s, "the form"); let sub = create_episode(&c, s, Some(a), "validate the card", "", "", "you").unwrap();
        assert!(create_episode(&c, s, Some(sub), "too deep", "", "", "you").unwrap_err().contains("cannot have steps"));
        let other = create_saga(&c, "another", "", "you").unwrap();
        assert!(create_episode(&c, other, Some(a), "wrong saga", "", "", "you").unwrap_err().contains("another saga"));
        assert!(create_episode(&c, s, None, "   ", "", "", "you").unwrap_err().contains("title"));
        let d = saga_detail(&c, s).unwrap();
        assert_eq!(d["saga"]["title"], json!("Add Stripe checkout"));
        assert_eq!(d["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(d["tasks"][0]["subtasks"][0]["title"], json!("validate the card"));
        assert_eq!(d["episodes"][0]["steps"][0]["title"], json!("validate the card"), "the new words carry the same rows"); assert_eq!(d["rollup"]["episodes"], json!(2));
        assert_eq!(d["rollup"]["tasks"], json!(2));
    }
    #[test]
    fn every_change_is_in_the_trail_with_who_made_it() {
        let c = db(); let s = create_saga(&c, "x", "", "you").unwrap(); let a = t(&c, s, "task");
        update_episode(&c, a, &json!({ "assignee": "frontender", "status": "doing" }), "heimdall").unwrap();
        update_episode(&c, a, &json!({ "assignee": "frontender", "status": "doing" }), "heimdall").unwrap();   // nothing changed: nothing recorded
        update_episode(&c, a, &json!({ "status": "done" }), "frontender").unwrap();
        let ev = episode_detail(&c, a).unwrap()["events"].as_array().unwrap().iter().map(|e| format!("{}: {}", e["actor"].as_str().unwrap(), e["text"].as_str().unwrap())).collect::<Vec<_>>();
        assert!(ev.iter().any(|e| e == "heimdall: assigned it to frontender"), "{ev:?}");
        assert!(ev.iter().any(|e| e == "heimdall: todo → doing"));
        assert!(ev.iter().any(|e| e == "frontender: doing → done"));
        assert_eq!(ev.iter().filter(|e| e.contains("assigned")).count(), 1, "an unchanged assignee is not a new event");
        let t = episode_detail(&c, a).unwrap()["episode"].clone();
        assert!(t["started_ms"].is_i64() && t["done_ms"].is_i64());
    }
    #[test]
    fn a_status_must_be_one_the_sagas_know() {
        let c = db(); let s = create_saga(&c, "x", "", "you").unwrap(); let a = t(&c, s, "task");
        assert!(update_episode(&c, a, &json!({ "status": "blocked-ish" }), "you").is_err());
        assert!(update_saga(&c, s, &json!({ "status": "done" }), "you").is_err(), "a saga's status follows its tasks");
    }
    #[test]
    fn a_sagas_status_follows_its_tasks() {
        let r = |todo, doing, done, failed, canceled| json!({ "tasks": todo + doing + done + failed + canceled, "todo": todo, "doing": doing, "done": done, "failed": failed, "canceled": canceled });
        assert_eq!(derive_status("todo", &r(0, 0, 0, 0, 0)), "todo");
        assert_eq!(derive_status("todo", &r(2, 0, 0, 0, 0)), "todo");
        assert_eq!(derive_status("todo", &r(1, 1, 0, 0, 0)), "doing");
        assert_eq!(derive_status("todo", &r(1, 0, 1, 0, 0)), "doing", "some done, some waiting: in progress");
        assert_eq!(derive_status("todo", &r(0, 0, 3, 0, 0)), "done");
        assert_eq!(derive_status("todo", &r(0, 0, 2, 0, 1)), "done");
        assert_eq!(derive_status("todo", &r(0, 0, 0, 1, 1)), "failed");
        assert_eq!(derive_status("todo", &r(0, 0, 0, 0, 2)), "canceled");
        assert_eq!(derive_status("canceled", &r(0, 1, 0, 0, 0)), "canceled", "canceled is the user's word");
    }
    #[test]
    fn the_rollup_sums_time_cost_and_names_the_seats() {
        let r = rollup(&[json!({ "status": "done", "ms_total": 1000, "usd_total": 0.25, "tokens": 10, "engine": "cursor" }), json!({ "status": "failed", "ms_total": 500, "usd_total": 0.125, "tokens": 5, "engine": "cursor" }), json!({ "status": "todo" })]);
        assert_eq!((r["ms"].clone(), r["usd"].clone(), r["tokens"].clone(), r["done"].clone(), r["failed"].clone(), r["todo"].clone()), (json!(1500), json!(0.375), json!(15), json!(1), json!(1), json!(1)));
        assert_eq!(r["seats"]["cursor"], json!(2));
    }
    fn run(id: i64, status: &str, tasks: Vec<(&str, &str, i64, f64)>) -> RunView {
        RunView { id, goal: "add a docs page for the queue\nand link it".into(), status: "running".into(), confidence: "high".into(),
            tasks: tasks.into_iter().map(|(i, st, ms, usd)| RunTaskView { id: i.into(), title: format!("task {i}"), status: st.into(), engine: "cursor".into(), model: "composer".into(), ms, usd, tokens: 100, started: Some(1), ended: None }).collect(),
            events: vec![(10, "e1".into(), "plan".into(), "planned 2 tasks".into(), None), (11, "e2".into(), "task".into(), "started".into(), Some("t1".into()))], ..Default::default() }
        .tap(|r| r.status = status.into())
    }
    trait Tap: Sized { fn tap(mut self, f: impl FnOnce(&mut Self)) -> Self { f(&mut self); self } } impl Tap for RunView {}
    #[test]
    fn a_plan_becomes_a_saga_and_follows_its_run() {
        let c = db();
        sync_runs(&c, &[run(7, "running", vec![("t1", "running", 0, 0.0), ("t2", "pending", 0, 0.0)])]).unwrap();
        let l = list(&c).unwrap();
        assert_eq!(l.len(), 1); assert_eq!(l[0]["title"], json!("add a docs page for the queue")); assert_eq!(l[0]["origin"], json!("heimdall"));
        assert_eq!(l[0]["status"], json!("doing")); assert_eq!(l[0]["rollup"]["tasks"], json!(2)); assert_eq!(l[0]["confidence"], json!("high"));
        // the user renames the saga and adds a task of their own; the run moves on
        let id = l[0]["id"].as_i64().unwrap();
        update_saga(&c, id, &json!({ "title": "docs: the queue" }), "you").unwrap(); t(&c, id, "my own task");
        sync_runs(&c, &[run(7, "done", vec![("t1", "done", 4000, 0.02), ("t2", "done", 6000, 0.03)])]).unwrap();
        let d = saga_detail(&c, id).unwrap();
        assert_eq!(d["saga"]["title"], json!("docs: the queue"), "a rename survives the sync");
        assert_eq!(d["tasks"].as_array().unwrap().len(), 3, "the user's own task is untouched");
        assert_eq!(d["rollup"]["ms"], json!(10000)); assert_eq!(d["rollup"]["usd"], json!(0.05)); assert_eq!(d["rollup"]["done"], json!(2));
        assert_eq!(d["tasks"][0]["assignee"], json!("cursor:composer"));
        let trail: Vec<String> = d["events"].as_array().unwrap().iter().map(|e| e["text"].as_str().unwrap().to_string()).collect();
        assert!(trail.iter().any(|t| t == "planned 2 tasks") && trail.iter().any(|t| t == "doing → done"), "{trail:?}");
    }
    #[test]
    fn syncing_twice_changes_nothing_and_adds_no_duplicates() {
        let c = db(); let r = run(3, "running", vec![("t1", "running", 5, 0.01)]);
        sync_runs(&c, &[r.clone()]).unwrap();
        let before = (rows(&c, "SELECT id FROM tasks", &[]).unwrap().len(), rows(&c, "SELECT id FROM events", &[]).unwrap().len(), rows(&c, "SELECT id FROM sagas", &[]).unwrap().len());
        assert_eq!(sync_runs(&c, &[r]).unwrap(), 0);
        assert_eq!((rows(&c, "SELECT id FROM tasks", &[]).unwrap().len(), rows(&c, "SELECT id FROM events", &[]).unwrap().len(), rows(&c, "SELECT id FROM sagas", &[]).unwrap().len()), before);
    }
    #[test]
    fn run_statuses_map_into_task_statuses() {
        assert_eq!(task_status_of("running", "running"), "doing"); assert_eq!(task_status_of("verifying", "running"), "doing");
        assert_eq!(task_status_of("failed", "failed"), "failed"); assert_eq!(task_status_of("blocked", "running"), "failed");
        assert_eq!(task_status_of("skipped", "done"), "canceled"); assert_eq!(task_status_of("pending", "running"), "todo");
        assert_eq!(task_status_of("pending", "cancelled"), "canceled", "a cancelled run cancels what never started");
    }
    #[test]
    fn a_run_belongs_to_a_project_by_its_folder() {
        let boxes = vec!["/home/u/.antiloki/worktrees/cockpit-1-0".to_string()];
        let no = |_: &str| false;
        assert!(in_project("/home/u/app", "/home/u/app/", &[], &no));
        assert!(in_project("/home/u/app/sub", "/home/u/app", &[], &no));
        assert!(!in_project("/home/u/app2", "/home/u/app", &[], &no), "a sibling that shares a prefix but not a name is another project");
        assert!(in_project("/home/u/.antiloki/worktrees/cockpit-1-0", "/home/u/app", &boxes, &no));
        assert!(!in_project("/home/u/.antiloki/worktrees/cockpit-2-0", "/home/u/app", &boxes, &no));
        assert!(in_project("C:\\Users\\u\\App", "c:/users/u/app", &[], &no));
        assert!(!in_project("", "/home/u/app", &[], &no));
        assert!(in_project("/elsewhere/checkout", "/home/u/app", &[], &|_| true), "a checkout of the same repository is the project's");
    }
    #[test]
    fn the_clones_a_benchmark_works_in_belong_to_the_project() {
        // the operator's real case: the project is ministore-2, its runs ran in ministore-merge-... and ministore-freeze-... next to it
        let root = "/home/u/Projects/ministore-2";
        assert_eq!(base_name(root), "ministore"); assert_eq!(base_name("/p/app"), "app"); assert_eq!(base_name("/p/my-app"), "my-app"); assert_eq!(base_name("/p/my-app-3"), "my-app");
        for yes in ["/home/u/Projects/ministore-merge-L-claude-composer-qatrue", "/home/u/Projects/ministore-freeze-L-backend-1", "/home/u/Projects/ministore", "/home/u/Projects/ministore-2"] {
            assert!(is_named_clone(yes, root), "{yes}");
        }
        for no in ["/home/u/Projects/ministores", "/home/u/Projects/store", "/home/u/Other/ministore-merge-1", "/home/u/Projects/app-ministore"] { assert!(!is_named_clone(no, root), "{no}"); }
    }
    #[test]
    fn a_sagas_status_follows_its_runs_verdict_not_only_its_tasks() {
        let c = db();
        // a run that failed at QA with every task done: the saga failed
        sync_runs(&c, &[run(1, "failed", vec![("t1", "done", 1, 0.0)]), run(2, "cancelled", vec![]), run(3, "inline", vec![]), run(4, "done", vec![("t1", "done", 1, 0.0)])]).unwrap();
        let by = |r: i64| list(&c).unwrap().into_iter().find(|s| s["plan_ref"] == json!(format!("run:{r}"))).unwrap()["status"].clone();
        assert_eq!((by(1), by(2), by(3), by(4)), (json!("failed"), json!("canceled"), json!("done"), json!("done")));
        // the user cancels a plan's saga while its run is still open: a later sync does not undo it (a FINISHED one cannot be canceled at all)
        sync_runs(&c, &[run(6, "running", vec![("t1", "running", 1, 0.0)])]).unwrap();
        let id = list(&c).unwrap().into_iter().find(|s| s["plan_ref"] == json!("run:6")).unwrap()["id"].as_i64().unwrap();
        update_saga(&c, id, &json!({ "status": "canceled" }), "you").unwrap();
        sync_runs(&c, &[run(6, "running", vec![("t1", "running", 9, 0.0)])]).unwrap();
        assert_eq!(by(6), json!("canceled"));
        assert!(update_saga(&c, list(&c).unwrap().into_iter().find(|s| s["plan_ref"] == json!("run:4")).unwrap()["id"].as_i64().unwrap(), &json!({ "status": "canceled" }), "you").is_err(), "run 4 is done: part of the record");
        assert_eq!((saga_status_of_run("running", false), saga_status_of_run("awaiting-approval", true), saga_status_of_run("done", true)), ("todo", "todo", "done"));
    }
    #[test]
    fn the_seats_of_a_rollup_are_the_exact_models() {
        let r = rollup(&[json!({ "status": "done", "engine": "cursor", "model": "composer-2.5" }), json!({ "status": "done", "engine": "cursor", "model": "composer-2.5" }), json!({ "status": "failed", "engine": "cursor", "model": "composer-2" }), json!({ "status": "done", "engine": "codex", "model": "" })]);
        assert_eq!(r["seats"], json!({ "cursor:composer-2.5": 2, "cursor:composer-2": 1, "codex": 1 }));
    }
    #[test]
    fn a_rollup_says_who_did_the_work_and_who_failed() {
        let r = rollup(&[json!({ "status": "done", "assignee": "frontender" }), json!({ "status": "failed", "assignee": "frontender" }), json!({ "status": "done", "assignee": "cursor:composer" }), json!({ "status": "todo", "assignee": "" })]);
        assert_eq!(r["agents"], json!({ "frontender": { "n": 2, "failed": 1 }, "cursor:composer": { "n": 1, "failed": 0 } }));
    }
    #[test]
    fn a_title_is_cut_at_a_word_with_an_ellipsis_never_mid_word() {
        assert_eq!(short_title("make it better", 100), "make it better");
        assert_eq!(short_title("\n  first line\nsecond", 100), "first line");
        let long = "Build an ORDERS feature for this Vite + React 19 + TypeScript store, wired into the existing app. No new runtime dependencies (vitest may be added)";
        let t = short_title(long, 100);
        assert!(t.ends_with('…') && t.chars().count() <= 101, "{t}");
        assert!(long.starts_with(t.trim_end_matches('…')) && !t.contains("depende"), "cut at a word: {t}");
    }
    #[test]
    fn a_title_an_older_build_cut_is_made_whole_once_and_a_renamed_one_is_left_alone() {
        let c = db();
        let goal = "Build an ORDERS feature for this Vite + React 19 + TypeScript store, wired into the existing app. No new runtime dependencies (vitest may be added) and keep going";
        let mut r = run(11, "done", vec![("t1", "done", 1, 0.0)]); r.goal = goal.into();
        sync_runs(&c, &[r.clone()]).unwrap();
        let id = list(&c).unwrap()[0]["id"].as_i64().unwrap();
        c.execute("UPDATE sagas SET title = ?1, sig = '' WHERE id = ?2", params![first_line(goal, 120), id]).unwrap();   // what the older build wrote
        sync_runs(&c, &[r.clone()]).unwrap();
        assert!(list(&c).unwrap()[0]["title"].as_str().unwrap().ends_with('…'));
        c.execute("UPDATE sagas SET title = 'my own name', sig = '' WHERE id = ?1", [id]).unwrap();
        sync_runs(&c, &[r]).unwrap();
        assert_eq!(list(&c).unwrap()[0]["title"], json!("my own name"));
    }
    #[test]
    fn a_saga_carries_its_runs_own_times_not_the_time_it_was_first_read() {
        let c = db();
        let mut r = run(5, "done", vec![("t1", "done", 1, 0.0)]); r.created = 1_700_000_000_000; r.updated = 1_700_000_090_000;
        sync_runs(&c, &[r]).unwrap();
        let l = list(&c).unwrap();
        assert_eq!((l[0]["created_ms"].clone(), l[0]["updated_ms"].clone()), (json!(1_700_000_000_000i64), json!(1_700_000_090_000i64)));
    }
    #[test]
    fn an_unchanged_run_is_recognised_so_the_sync_can_skip_it() {
        let a = run(9, "running", vec![("t1", "running", 5, 0.01)]);
        assert_eq!(fingerprint(&a), fingerprint(&a.clone()));
        let mut b = a.clone(); b.tasks[0].ms = 6;
        assert_ne!(fingerprint(&a), fingerprint(&b), "a task that moved on changes it");
        let mut c = a.clone(); c.events.push((20, "e3".into(), "task".into(), "more".into(), None));
        assert_ne!(fingerprint(&a), fingerprint(&c), "a new event changes it");
        // and a skipped run does not touch another run's saga
        let db = db();
        sync_runs(&db, &[a.clone(), run(10, "done", vec![("t1", "done", 1, 0.0)])]).unwrap();
        let before = rows(&db, "SELECT id, updated_ms FROM sagas ORDER BY id", &[]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        sync_runs(&db, &[a, run(10, "done", vec![("t1", "done", 1, 0.0)])]).unwrap();
        assert_eq!(rows(&db, "SELECT id, updated_ms FROM sagas ORDER BY id", &[]).unwrap(), before, "nothing changed, so nothing is touched");
    }
    #[test]
    fn an_agents_write_is_attributed_to_the_agent() {
        assert_eq!(actor_of_pane(&json!({ "pane_kind": "cursor", "settings_json": "{\"agent\":\"frontender\"}" })), "frontender");
        assert_eq!(actor_of_pane(&json!({ "pane_kind": "claude-code", "settings_json": "{\"chat\":true}" })), "claude-code");
        assert_eq!(actor_of_pane(&json!({})), "agent");
        // and the trail then carries that name
        let c = db(); let s = create_saga(&c, "x", "", "frontender").unwrap(); let a = t(&c, s, "task");
        update_episode(&c, a, &json!({ "status": "done" }), "frontender").unwrap();
        let ev = saga_detail(&c, s).unwrap()["events"].as_array().unwrap().iter().map(|e| e["actor"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        assert!(ev.iter().all(|a| a == "frontender" || a == "you"), "{ev:?}");
    }
    #[test]
    fn a_finished_saga_can_be_neither_canceled_nor_deleted() {
        let c = db();
        let open = create_saga(&c, "open", "", "you").unwrap(); let t1 = t(&c, open, "a");
        // an open saga can be canceled, and then reopened, and deleted
        update_saga(&c, open, &json!({ "status": "canceled" }), "you").unwrap();
        assert_eq!(effective_status(&c, open).unwrap(), "canceled");
        assert!(delete_saga(&c, open).unwrap_err().contains("finished"), "a canceled saga is finished too: it can only be reopened");
        update_saga(&c, open, &json!({ "status": "todo" }), "you").unwrap();
        // done: every task done
        update_episode(&c, t1, &json!({ "status": "done" }), "you").unwrap();
        assert_eq!(effective_status(&c, open).unwrap(), "done");
        assert!(update_saga(&c, open, &json!({ "status": "canceled" }), "you").unwrap_err().contains("cannot be canceled"));
        assert!(delete_saga(&c, open).unwrap_err().contains("cannot be deleted"));
        assert_eq!(list(&c).unwrap().len(), 1, "it is still there");
        // failed: a run's verdict
        sync_runs(&c, &[run(8, "failed", vec![("t1", "done", 1, 0.0)])]).unwrap();
        let failed = list(&c).unwrap().into_iter().find(|s| s["plan_ref"] == json!("run:8")).unwrap()["id"].as_i64().unwrap();
        assert!(update_saga(&c, failed, &json!({ "status": "canceled" }), "you").is_err() && delete_saga(&c, failed).is_err());
        // adding a task to a finished saga reopens it, and then it is open again
        t(&c, open, "one more");
        assert!(is_open(effective_status(&c, open).unwrap()));
        delete_saga(&c, open).unwrap();
    }
    #[test]
    fn deleting_a_saga_takes_its_tasks_and_trail_with_it() {
        let c = db(); let s = create_saga(&c, "x", "", "you").unwrap(); let a = t(&c, s, "a"); create_episode(&c, s, Some(a), "b", "", "", "you").unwrap();
        delete_saga(&c, s).unwrap();
        for tbl in ["sagas", "tasks", "events"] { assert_eq!(rows(&c, &format!("SELECT 1 AS x FROM {tbl}"), &[]).unwrap().len(), 0, "{tbl}"); }
        assert!(delete_saga(&c, s).is_err());
    }
    #[test]
    fn it_opens_a_file_of_its_own_inside_the_project() {
        let d = std::env::temp_dir().join(format!("antiloki-work-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let root = d.to_string_lossy().to_string();
        let c = open(&root).unwrap(); create_saga(&c, "persisted", "", "you").unwrap(); drop(c);
        assert!(db_path(&root).ends_with(".antiloki/work.sqlite") || db_path(&root).ends_with(".antiloki\\work.sqlite"));
        assert_eq!(list(&open(&root).unwrap()).unwrap().len(), 1, "it is a file, and it is still there");
        assert!(open("").is_err());
    }
}

/// BRING THE SAGAS OF A WORKSPACE UP TO DATE NOW (idle sleep, operator 2026-11-16): a sleeping agent's saga is saved BEFORE its terminal closes. Returns whether the pane has a saga row afterwards, which is the only proof the sleep may go on.
/// One saga of a timeline: its number, when it last moved, its title, state, and its tasks (title, body) oldest first.
pub struct PastSaga { pub id: i64, pub updated_ms: i64, pub title: String, pub status: String, pub tasks: Vec<(String, String)> }
const TIMELINE_SAGAS: usize = 12;
const TIMELINE_TASKS: usize = 5;
/// The words of the timeline: titles for all, details only for the newest. `sagas` oldest first. Pure.
pub fn timeline_text(sagas: &[PastSaga]) -> String {
    if sagas.is_empty() { return String::new(); }
    let cut = |t: &str, n: usize| { let t = t.split_whitespace().collect::<Vec<_>>().join(" "); if t.chars().count() > n { format!("{}…", t.chars().take(n).collect::<String>()) } else { t } };
    let mut o = String::from("\nYOUR TIMELINE (what you did before, from your sagas, oldest first; titles only except the last). Look inside one only when an ask needs it: `antiloki_sagas` op `show` {id}.\n");
    let skipped = sagas.len().saturating_sub(TIMELINE_SAGAS); if skipped > 0 { o.push_str(&format!("({skipped} older saga(s) not listed: `antiloki_sagas` op `list`)\n")); }
    let shown = &sagas[skipped..];
    for (i, s) in shown.iter().enumerate() {
        let last = i + 1 == shown.len(); let date = crate::cowork_engines::ms_to_iso(s.updated_ms).chars().take(10).collect::<String>();
        o.push_str(&format!("- saga #{} {date} [{}] {}\n", s.id, s.status, cut(&s.title, 110)));
        let from = s.tasks.len().saturating_sub(TIMELINE_TASKS); if from > 0 { o.push_str(&format!("    ({from} earlier episode(s))\n")); }
        for (title, body) in &s.tasks[from..] { o.push_str(&format!("    · {}\n", cut(title, 110))); if last && !body.trim().is_empty() { o.push_str(&format!("      {}\n", cut(body, 400))); } }
    }
    o
}
/// The timeline of the sagas made from these panes. Blocking; empty when there is none or the store cannot be read.
pub fn timeline_of(root: &str, panes: &[i64]) -> String {
    if panes.is_empty() { return String::new(); }
    let Ok(c) = open(root) else { return String::new() };
    let Ok(mut st) = c.prepare("SELECT id, updated_ms, title, status, plan_ref FROM sagas WHERE plan_ref LIKE 'ep:%' AND status != 'canceled' ORDER BY updated_ms") else { return String::new() };
    let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?))) else { return String::new() };
    let sagas: Vec<PastSaga> = rows.flatten().filter(|r| r.4.split(':').nth(1).and_then(|p| p.parse::<i64>().ok()).is_some_and(|p| panes.contains(&p))).map(|(id, updated_ms, title, status, _)| {
        let tasks = c.prepare("SELECT title, body FROM tasks WHERE saga_id = ?1 AND parent_id IS NULL ORDER BY position, id").and_then(|mut q| q.query_map([id], |r| Ok((r.get(0)?, r.get(1)?))).map(|m| m.flatten().collect())).unwrap_or_default();
        PastSaga { id, updated_ms, title, status, tasks }
    }).collect();
    timeline_text(&sagas)
}

pub fn consolidate(st: &Arc<AppState>, goals: &crate::goals::Goals, root: &str, pane_id: i64) -> Result<bool, String> {
    let c = open(root)?;
    sync_runs(&c, &runs_of_project(st, goals, root))?; crate::work_boxes::sync_project(&c, &trails_of_project(st, root), root);
    Ok(with_meta(list(&c)?).iter().any(|s| meta_of(&s["meta"])["pane_id"].as_i64() == Some(pane_id) || s["meta"]["pane_id"].as_i64() == Some(pane_id)))
}
