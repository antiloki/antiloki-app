//! WHAT AN AGENT HAS DONE, IS DOING AND STILL HAS TO DO (operator 2026-11-26: "I make a big ask and the agent is on the first item, but I forget the list: an icon that shows a queue with done, doing and todo; not
//! kanban, a simple list"). Two lists per pane, nothing guessed by a model:
//!   plan  the agent's own task list: the one it keeps with `antiloki_queue` (every provider), else the CLI's native one (Claude Code's tasks, Codex's plan, OpenCode's todos);
//!   asks  what the person asked, from the engine's transcript: answered = done, the one being worked on = doing, typed while it was busy or not yet answered = todo.
//!
//!   antiloki_queue {items: [{text, state: todo|doing|done}]} | {op: get}      GET /api/v1/cockpit/pane/queue?pane_id=
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use crate::dispatch::{engine_call, AppState};

const MAX_ITEMS: usize = 80;
const MAX_TEXT: usize = 240;
#[derive(Clone, Debug, PartialEq)]
pub struct Item { pub text: String, pub state: &'static str }
fn item(text: &str, state: &'static str) -> Item { let line = text.split_whitespace().collect::<Vec<_>>().join(" "); let mut t: String = line.chars().take(MAX_TEXT).collect(); if line.chars().count() > MAX_TEXT { t.push('\u{2026}'); } Item { text: t, state } }
/// The words each CLI uses for a state. Pure.
pub fn state_of(s: &str) -> &'static str { match s.trim().to_ascii_lowercase().as_str() { "done" | "completed" | "complete" => "done", "doing" | "in_progress" | "in-progress" | "active" => "doing", _ => "todo" } }

/// A list of `{text|content|step|subject, state|status}`; cancelled ones are left out. Pure.
pub fn items_of(list: &Value) -> Vec<Item> {
    list.as_array().into_iter().flatten().filter_map(|x| {
        let text = ["text", "content", "step", "subject"].iter().find_map(|k| x[*k].as_str()).or_else(|| x.as_str())?.trim();
        let st = x["state"].as_str().or(x["status"].as_str()).unwrap_or("todo"); if text.is_empty() || st.eq_ignore_ascii_case("cancelled") || st.eq_ignore_ascii_case("deleted") { return None; }
        Some(item(text, state_of(st)))
    }).take(MAX_ITEMS).collect()
}
/// Claude Code's own list in a session file: the last `TodoWrite`, or the tasks made by `TaskCreate` and moved by `TaskUpdate`. Pure.
pub fn claude_plan<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<Item> {
    let mut todo: Vec<Item> = vec![]; let mut tasks: Vec<(String, Option<Item>)> = vec![];
    for l in lines {
        if !l.contains("TodoWrite") && !l.contains("TaskCreate") && !l.contains("TaskUpdate") { continue; }
        let Ok(v) = serde_json::from_str::<Value>(l) else { continue }; if v["isSidechain"] == json!(true) { continue; }
        for c in v["message"]["content"].as_array().into_iter().flatten().filter(|c| c["type"] == json!("tool_use")) {
            let i = &c["input"];
            match c["name"].as_str() {
                Some("TodoWrite") => todo = items_of(&i["todos"]),
                Some("TaskCreate") => { if let Some(t) = i["subject"].as_str() { tasks.push(((tasks.len() + 1).to_string(), Some(item(t, "todo")))); } }
                Some("TaskUpdate") => { let id = i["taskId"].as_str().map(String::from).unwrap_or_else(|| i["taskId"].to_string()); if let Some(t) = tasks.iter_mut().find(|t| t.0 == id) { match i["status"].as_str() { Some("deleted") => t.1 = None, s => { if let Some(x) = t.1.as_mut() { if let Some(s) = s { x.state = state_of(s); } if let Some(n) = i["subject"].as_str() { x.text = item(n, "todo").text; } } } } } }
                _ => {}
            }
        }
    }
    let made: Vec<Item> = tasks.into_iter().filter_map(|t| t.1).take(MAX_ITEMS).collect();
    if made.is_empty() { todo } else { made }
}
/// What the person typed while Claude Code was busy and it has not taken yet. Pure.
pub fn claude_waiting<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut q: Vec<String> = vec![];
    for l in lines {
        if !l.contains("\"queue-operation\"") { continue; }
        let Ok(v) = serde_json::from_str::<Value>(l) else { continue }; if v["type"] != json!("queue-operation") { continue; }
        let content = v["content"].as_str().unwrap_or("").to_string();
        match v["operation"].as_str() {
            Some("enqueue") => q.push(content),
            Some("dequeue") => { if !q.is_empty() { q.remove(0); } }
            Some("remove") => { if let Some(i) = q.iter().position(|x| *x == content) { q.remove(i); } else if !q.is_empty() { q.remove(0); } }
            Some("popAll") => q.clear(),
            _ => {}
        }
    }
    q.into_iter().filter(|t| !t.trim().is_empty() && !t.trim_start().starts_with('<')).collect()
}
/// Codex's last `update_plan` in a rollout file. Pure.
pub fn codex_plan<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<Item> {
    let mut plan = vec![];
    for l in lines {
        if !l.contains("update_plan") { continue; }
        let Ok(v) = serde_json::from_str::<Value>(l) else { continue }; let p = &v["payload"]; if p["name"] != json!("update_plan") { continue; }
        if let Some(a) = p["arguments"].as_str().or(p["input"].as_str()).and_then(|a| serde_json::from_str::<Value>(a).ok()) { plan = items_of(&a["plan"]); }
    }
    plan
}
/// Is this turn something the person (or another agent) asked, and not the engine's own text? Pure.
fn is_ask(text: &str) -> bool { let t = text.trim(); !t.is_empty() && !t.starts_with('<') && !t.starts_with('/') && !t.starts_with("[Request interrupted") && !t.starts_with("Caveat:") && !crate::chat_pane::is_internal_brief(t) }
/// The asks of a conversation with their state, oldest first. Pure.
pub fn asks_of(turns: &[Value], working: bool, waiting: &[String]) -> Vec<Item> {
    let mut out: Vec<(String, bool)> = vec![];   // (ask, answered)
    for t in turns {
        if crate::pane_provider::typed_by_antiloki(t) { continue; }   // not an ask of the person's (#28)
        if t["role"] == json!("user") { let text = crate::work::strip_paste_markup(t["text"].as_str().unwrap_or("")); if is_ask(&text) { out.push((text, false)); } }
        else if let Some(last) = out.last_mut() { if t["text"].as_str().is_some_and(|x| !x.trim().is_empty()) { last.1 = true; } }
    }
    let n = out.len();
    let mut items: Vec<Item> = out.into_iter().enumerate().map(|(i, (text, answered))| item(&text, if i + 1 == n && working { "doing" } else if answered || i + 1 < n { "done" } else { "todo" })).collect();
    for w in waiting { let w = item(&crate::work::strip_paste_markup(w), "todo"); if !items.iter().any(|x| x.text == w.text && x.state != "done") { items.push(w); } }
    let skip = items.len().saturating_sub(MAX_ITEMS); items.split_off(skip)
}

fn file_of(pane_id: i64) -> PathBuf { std::env::var_os("ANTILOKI_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".antiloki"))).unwrap_or_else(std::env::temp_dir).join("queue").join(format!("pane-{pane_id}.json")) }
fn kept(pane_id: i64) -> Vec<Item> { std::fs::read_to_string(file_of(pane_id)).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map(|v| items_of(&v)).unwrap_or_default() }
fn json_of(v: &[Item]) -> Vec<Value> { v.iter().map(|i| json!({ "text": i.text, "state": i.state })).collect() }

/// The agent's tool: replace its list, or read it. Blocking.
pub fn run_tool(pane_id: i64, args: &Value) -> Result<String, String> {
    if pane_id <= 0 { return Err("only an agent in a pane keeps a queue".into()); }
    let show = |v: &[Item]| v.iter().map(|i| format!("[{}] {}", i.state, i.text)).collect::<Vec<_>>().join("\n");
    if args["op"] == json!("get") || args["items"].is_null() { let v = kept(pane_id); return Ok(if v.is_empty() { "your queue is empty".into() } else { show(&v) }); }
    if !args["items"].is_array() { return Err("`items` is the whole list: [{text, state: todo|doing|done}]".into()); }
    if args["items"].as_array().map_or(0, |a| a.len()) > MAX_ITEMS { return Err(format!("at most {MAX_ITEMS} items")); }
    let v = items_of(&args["items"]); let f = file_of(pane_id); if let Some(d) = f.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    std::fs::write(&f, json!(json_of(&v)).to_string()).map_err(|e| e.to_string())?;
    Ok(format!("the person sees your queue: {} done, {} doing, {} to do", v.iter().filter(|i| i.state == "done").count(), v.iter().filter(|i| i.state == "doing").count(), v.iter().filter(|i| i.state == "todo").count()))
}

fn opencode_plan(root: &str) -> Vec<Item> {
    let Some(db) = antiloki_rs::cockpit__helpers_engine_usage::opencode_db().and_then(|p| rusqlite::Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()) else { return vec![] };
    let Ok(mut st) = db.prepare("SELECT content, status FROM todo WHERE session_id = (SELECT id FROM session_v2 WHERE directory = ?1 ORDER BY time_updated DESC LIMIT 1) ORDER BY position") else { return vec![] };
    let rows: Vec<Value> = st.query_map([root], |r| Ok(json!({ "content": r.get::<_, String>(0)?, "status": r.get::<_, String>(1)? }))).map(|it| it.flatten().collect()).unwrap_or_default();
    items_of(&json!(rows))
}

/// `GET /api/v1/cockpit/pane/queue?pane_id=`
pub async fn view(st: &Arc<AppState>, pane_id: i64) -> Value {
    let pane = engine_call(st, "system.cockpit.pane.get", json!({ "id": pane_id }), json!({ "actor": "web-fe", "source": "web-fe" })).await.map(|(v, _)| v).unwrap_or(Value::Null);
    let kind = pane["pane_kind"].as_str().unwrap_or("").to_string(); if kind.is_empty() { return json!({ "ok": false, "error": "no such pane" }); }
    let root = crate::cowork::pane_facts(st, pane_id).await.0;
    let t = crate::cowork::transcript_value(st, pane_id, 400).await;
    let turns: Vec<Value> = t["turns"].as_array().cloned().unwrap_or_default(); let session = t["session"].as_str().unwrap_or("").to_string();
    tokio::task::spawn_blocking(move || {
        let rows = crate::pty_remote::screen_rows(pane_id);
        let working = crate::cowork::usage_for_pane(Path::new(&root), pane_id, &kind, rows.as_deref())["working"] == true;
        let file = crate::cowork::session_file(&root, &kind, &session);
        let waiting = match &file { Some(p) if kind == "claude-code" => crate::cowork::parsed("queue-waiting", p, |t| claude_waiting(t.lines())), _ => vec![] };
        let mut plan = kept(pane_id); let mut source = "antiloki";
        if plan.is_empty() { source = "engine"; plan = match (kind.as_str(), &file) { ("claude-code", Some(p)) => crate::cowork::parsed("queue-plan", p, |t| claude_plan(t.lines())), ("codex", Some(p)) => crate::cowork::parsed("queue-plan", p, |t| codex_plan(t.lines())), ("opencode", _) => opencode_plan(&root), _ => vec![] }; }
        json!({ "ok": true, "working": working, "plan": json_of(&plan), "plan_from": source, "asks": json_of(&asks_of(&turns, working, &waiting)) })
    }).await.unwrap_or_else(|_| json!({ "ok": false, "error": "cut off" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn use_(name: &str, input: Value) -> String { json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": name, "input": input }] } }).to_string() }
    #[test]
    fn a_list_is_read_in_every_dialect() {
        let v = items_of(&json!([{ "text": "a", "state": "done" }, { "content": "b", "status": "in_progress" }, { "step": "c", "status": "pending" }, { "content": "x", "status": "cancelled" }, "d", { "text": " " }]));
        assert_eq!(v.iter().map(|i| (i.text.as_str(), i.state)).collect::<Vec<_>>(), vec![("a", "done"), ("b", "doing"), ("c", "todo"), ("d", "todo")]);
    }
    #[test]
    fn claudes_tasks_follow_their_updates_and_its_waiting_asks_leave_when_taken() {
        let l = [use_("TaskCreate", json!({ "subject": "one" })), use_("TaskCreate", json!({ "subject": "two" })), use_("TaskUpdate", json!({ "taskId": "1", "status": "completed" })), use_("TaskUpdate", json!({ "taskId": "2", "status": "in_progress" }))];
        assert_eq!(claude_plan(l.iter().map(String::as_str)), vec![item("one", "done"), item("two", "doing")]);
        let t = use_("TodoWrite", json!({ "todos": [{ "content": "z", "status": "pending" }] })); assert_eq!(claude_plan([t.as_str()].into_iter()), vec![item("z", "todo")]);
        let q = |op: &str, c: &str| json!({ "type": "queue-operation", "operation": op, "content": c }).to_string();
        let l = [q("enqueue", "<task-notification>x"), q("enqueue", "first"), q("enqueue", "second"), q("remove", "<task-notification>x"), q("enqueue", "third"), q("dequeue", "")];
        assert_eq!(claude_waiting(l.iter().map(String::as_str)), vec!["second".to_string(), "third".to_string()]);
    }
    #[test]
    fn asks_are_done_doing_or_waiting_and_the_engines_own_text_is_not_an_ask() {
        let u = |t: &str| json!({ "role": "user", "text": t }); let a = |t: &str| json!({ "role": "assistant", "text": t });
        let turns = [u("You are CC, a specialist.\nYour purpose: x"), a("ok"), u("fix the login"), a("done"), u("/model haiku"), u("<system-reminder>x"), u("add tests"), a("working on it")];
        let v = asks_of(&turns, true, &["then deploy".to_string()]);
        assert_eq!(v, vec![item("fix the login", "done"), item("add tests", "doing"), item("then deploy", "todo")]);
        assert_eq!(asks_of(&[u("hi")], false, &[]), vec![item("hi", "todo")], "not answered and not working");
        assert!(item(&"y".repeat(300), "todo").text.ends_with('\u{2026}'));
    }
}
