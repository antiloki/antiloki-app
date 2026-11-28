//! THE BOARD (RFC 0025 I2 and I3, 2026-11-26): every running agent on one screen - what it is doing, what waits for it - and the tasks agents gave each other, each in one of four states: waiting, working, done, accepted.
//! It only reads; pause, continue and stop are the doors the screens already have.
//!
//!   GET /api/v1/cockpit/board
use serde_json::{json, Value};
use std::sync::Arc;

use crate::dispatch::{engine_call, AppState};

/// A contract's state as the board says it. `running`: the one asked has a pane open. A cancelled one is not a task any more (`None`). Pure.
pub fn task_state(c: &Value, running: bool) -> Option<&'static str> {
    match c["status"].as_str().unwrap_or("") { "fulfilled" => Some("accepted"), "open" if c["delivered"].is_object() => Some("done"), "open" => Some(if running { "working" } else { "waiting" }), _ => None }
}

pub async fn view(st: &Arc<AppState>) -> Value {
    let root = crate::agents_http::active_root(st).await;
    let terms = tokio::task::spawn_blocking(crate::pty_remote::list_running).await.ok().and_then(|r| r.ok()).unwrap_or_default();
    let paused: Vec<Value> = crate::pane_watch::paused_rows();
    let mail = tokio::task::spawn_blocking(crate::mailbox::waiting_rows).await.unwrap_or_default();
    let mut agents = vec![];
    for t in terms {
        let Some(pane_id) = crate::pty_remote::pane_of(&t.id) else { continue };
        let pane = engine_call(st, "system.cockpit.pane.get", json!({ "id": pane_id }), json!({ "actor": "web-fe", "source": "web-fe" })).await.map(|(v, _)| v).unwrap_or(Value::Null);
        let kind = pane["pane_kind"].as_str().unwrap_or("").to_string(); if kind.is_empty() || kind == "shell" { continue; }
        let queue = crate::task_queue::view(st, pane_id).await;
        let work = crate::cowork::pane_facts(st, pane_id).await.0; let k = kind.clone();
        let (usage, specialist) = tokio::task::spawn_blocking(move || (crate::cowork::usage_for_pane(std::path::Path::new(&work), pane_id, &k, crate::pty_remote::screen_rows(pane_id).as_deref()), crate::specialists::bound_of(pane_id))).await.unwrap_or((Value::Null, None));
        let items = |key: &str| queue[key].as_array().cloned().unwrap_or_default();
        let now: Option<Value> = items("plan").into_iter().chain(items("asks")).find(|i| i["state"] == "doing").map(|i| i["text"].clone());
        let todo = items("plan").iter().chain(items("asks").iter()).filter(|i| i["state"] == "todo").count();
        let held = paused.iter().find(|p| p["pane_id"].as_i64() == Some(pane_id));
        let state = if held.is_some() { "paused" } else if !usage["blocked"].is_null() { "needs you" } else if usage["working"] == true { "working" } else { "idle" };
        agents.push(json!({ "pane_id": pane_id, "engine": kind, "specialist": specialist.map(|s| s.1), "folder": pane["worktree_path"], "state": state, "why": held.map(|p| p["why"].clone()).unwrap_or_else(|| usage["blocked"].clone()), "model": usage["model"], "session_cost": usage["session_cost"], "doing": now, "todo": todo, "messages": mail.iter().filter(|m| m["pane_id"].as_i64() == Some(pane_id)).count() }));
    }
    let r = root.clone();
    let tasks: Vec<Value> = tokio::task::spawn_blocking(move || {
        let mut v = crate::specialists::all_contracts(&r); v.sort_by_key(|c| std::cmp::Reverse(c["t_ms"].as_u64().unwrap_or(0))); v.truncate(40);
        v.into_iter().filter_map(|c| { let to = c["to"].as_str().unwrap_or("").to_string(); let running = !crate::specialists::panes_of(&r, &to).is_empty(); let state = task_state(&c, running)?; Some(json!({ "id": c["id"], "from": c["from"], "to": to, "task": c["task"], "state": state, "t_ms": c["t_ms"], "note": c["delivered"]["note"], "checks_passed": c["last"]["passed"] })) }).collect()
    }).await.unwrap_or_default();
    json!({ "ok": true, "root": root, "agents": agents, "tasks": tasks, "groups": crate::groups::summary()["list"] })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_task_is_waiting_working_done_or_accepted() {
        let open = json!({ "status": "open" }); assert_eq!((task_state(&open, false), task_state(&open, true)), (Some("waiting"), Some("working")));
        assert_eq!(task_state(&json!({ "status": "open", "delivered": { "note": "x" } }), false), Some("done")); assert_eq!(task_state(&json!({ "status": "fulfilled", "delivered": {} }), true), Some("accepted")); assert_eq!(task_state(&json!({ "status": "cancelled" }), true), None);
    }
}
