//! RUN A SAGA (operator 2026-11-10: "a saga created by hand can choose model, total to spend, subtasks, description, and has a Run button").
//!
//!   POST /api/v1/sagas/run {id, workspace?}           run the saga's open tasks: a goal run made FROM them (the same machinery agents' delegation uses: workers, verify, join, judge), tied to this saga
//!   POST /api/v1/sagas/run {op:"stop", id, workspace?} stop its run
//!
//! The saga keeps its own title, objective and tasks; the run is linked in (`work::link_run`) so the saga's tasks take the run's states, cost and seat as it goes, and no second saga appears. The settings (`run`:
//! engine, model, budget_usd) are the saga's own (`update`). The budget is a hard stop: a watcher adds up what the run has cost and cancels it when it reaches the limit.
use crate::dispatch::AppState;
use crate::goals::{Goals, RunStatus};
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// A run holds this many tasks at most (the plan's own limit).
pub const MAX_TASKS: usize = 8;
fn reply(v: Value) -> Response { (StatusCode::OK, Json(v)).into_response() }
fn fail(m: &str) -> Response { reply(json!({ "ok": false, "error": m })) }
fn s(v: &Value) -> String { v.as_str().unwrap_or("").trim().to_string() }

/// What the run is asked, from the saga: the title and objective, then its open tasks as a numbered list (the planner reads them as the tasks). Pure - tested.
pub fn goal_of(title: &str, objective: &str, tasks: &[(String, String)]) -> String {
    let mut g = title.trim().to_string(); if !objective.trim().is_empty() { g.push_str(&format!("\n\n{}", objective.trim())); }
    g.push_str("\n\nThe tasks, in order:\n");
    for (i, (t, _)) in tasks.iter().enumerate() { g.push_str(&format!("{}. {}\n", i + 1, t.trim())); }
    g
}
/// The open episodes of a saga detail (read under either word), as (id, title, prompt): done and canceled ones are not run again; an episode's body and its steps are its prompt. Pure - tested.
pub fn open_episodes(detail: &Value) -> Vec<(i64, String, String)> {
    detail.get("episodes").or_else(|| detail.get("tasks")).and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().filter(|t| !matches!(t["status"].as_str(), Some("done") | Some("canceled"))).map(|t| {
        let title = s(&t["title"]); let mut prompt = title.clone();
        if !s(&t["body"]).is_empty() { prompt.push_str(&format!("\n\n{}", s(&t["body"]))); }
        let kids: Vec<String> = t.get("steps").or_else(|| t.get("subtasks")).and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().filter(|k| !matches!(k["status"].as_str(), Some("done") | Some("canceled"))).map(|k| format!("- {}{}", s(&k["title"]), if s(&k["body"]).is_empty() { String::new() } else { format!(": {}", s(&k["body"])) })).collect();
        if !kids.is_empty() { prompt.push_str(&format!("\n\nDo these parts of it:\n{}", kids.join("\n"))); }
        (t["id"].as_i64().unwrap_or(0), title, prompt)
    }).collect()
}
/// Has the run spent what the saga allows? `spent` and `budget` in USD; no budget means no limit. Pure - tested.
pub fn over_budget(spent: f64, budget: Option<f64>) -> bool { budget.map_or(false, |b| b > 0.0 && spent + 1e-9 >= b) }
fn active(st: &RunStatus) -> bool { matches!(st, RunStatus::Planning | RunStatus::AwaitingAnswers | RunStatus::AwaitingApproval | RunStatus::Running | RunStatus::Paused | RunStatus::Joining) }
fn run_number(plan_ref: &Value) -> Option<i64> { plan_ref.as_str()?.strip_prefix("run:")?.parse().ok() }

async fn call(state: &Arc<AppState>, goals: &Arc<Goals>, reg: &Arc<crate::agent::AgentRegistry>, op: &str, body: Value) -> Value {
    let r = crate::agents_http::goals_op(state.clone(), goals.clone(), reg.clone(), op.to_string(), Bytes::from(body.to_string())).await;
    match axum::body::to_bytes(r.into_body(), 16 * 1024 * 1024).await { Ok(b) => serde_json::from_slice(&b).unwrap_or(Value::Null), Err(_) => Value::Null }
}
/// The error a goals call answered with, as a line.
fn why(v: &Value) -> String { let e = &v["error"]; if let Some(m) = e["message"].as_str() { return m.to_string(); } e.as_str().map(String::from).unwrap_or_else(|| "the run was not accepted".to_string()) }

/// What a run has cost so far: its planning plus every task's call.
fn spent(r: &crate::goals::GoalRun) -> f64 { let v = crate::work::view_of(r); v.plan_usd + v.tasks.iter().map(|t| t.usd).sum::<f64>() }

/// Stops the run when it has cost what the saga allows. Polls; ends with the run.
fn watch(goals: Arc<Goals>, run: i64, budget: f64) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let Some(r) = goals.get(run) else { return };
            if !active(&r.status) { return; }
            let used = spent(&r);
            if over_budget(used, Some(budget)) {
                goals.note(run, "budget", &format!("the saga's budget is spent (${used:.2} of ${budget:.2}): stopping the run"));
                goals.cancel(run); return;
            }
        }
    });
}

pub async fn post(state: Arc<AppState>, goals: Arc<Goals>, reg: Arc<crate::agent::AgentRegistry>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (id, ws) = (b["id"].as_i64().unwrap_or(0), s(&b["workspace"]));
    let detail = match crate::work::with_db_in(&state, ws.clone(), move |c, root| crate::work::saga_detail(c, id).map(|d| (d, root.to_string()))).await { Ok(d) => d, Err(e) => return fail(&e) };
    let (detail, root) = detail;
    let linked = run_number(&detail["saga"]["plan_ref"]).and_then(|n| goals.get(n));
    if s(&b["op"]) == "stop" {
        let Some(r) = linked.filter(|r| active(&r.status)) else { return fail("this saga has no run going") };
        goals.cancel(r.id); return reply(json!({ "ok": true, "run": r.id }));
    }
    if let Some(r) = &linked { if active(&r.status) { return fail("this saga is already running"); } }
    if detail["saga"]["plan_ref"].as_str().map_or(false, |p| !p.starts_with("run:")) { return fail("this saga is not one you can run"); }
    let open = open_episodes(&detail);
    if open.is_empty() { return fail("there is nothing to run: add an episode first (done and canceled ones are not run again)"); }
    if open.len() > MAX_TASKS { return fail(&format!("a run holds {MAX_TASKS} episodes at most and this saga has {} open: split it into two sagas, or finish some first", open.len())); }
    let cfg = &detail["saga"]["run"];
    let pairs_in: Vec<(String, String)> = open.iter().map(|(_, t, p)| (t.clone(), p.clone())).collect();
    let goal = goal_of(&s(&detail["saga"]["title"]), &s(&detail["saga"]["objective"]), &pairs_in);
    let engine = s(&cfg["engine"]); let model = s(&cfg["model"]);
    // 1. the run, parked for approval (so its tasks can be set to exactly the saga's)
    let mut plan = json!({ "goal": goal, "cwd": root, "write": true, "n": open.len(), "auto": false, "router": false });
    if !engine.is_empty() { plan["engine"] = json!(engine); }
    if !model.is_empty() && !engine.is_empty() { if let Some(e) = crate::cli::engine_of(&json!(engine)) { plan["pair"] = json!({ "name": "saga", "brain": e.key(), "hands": e.key(), "brain_model": model, "hands_model": model }); } }
    // the plan is the saga's own tasks, one for one (an id each, so the sync can tie them back): the planner is not asked, and costs nothing
    let tasks: Vec<Value> = open.iter().map(|(tid, title, prompt)| json!({ "id": format!("s{tid}"), "title": title, "prompt": prompt })).collect();
    goals.set_given_plan_for_next(Some(json!({ "tier": "mid", "delegate": true, "why": "the saga's own tasks", "confidence": "high", "assumptions": [], "tasks": tasks }).to_string()));
    let waited = std::time::Instant::now();
    let planned = call(&state, &goals, &reg, "plan", plan).await;
    goals.set_given_plan_for_next(None);
    let Some(run) = planned["run"]["id"].as_i64() else { return fail(&why(&planned)) };
    // the run is made in the background (the screen, then a plan that is one job): wait until it is parked for approval
    let mut answered = false;
    let parked = loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let Some(r) = goals.get(run) else { return fail("the run vanished") };
        match r.status {
            RunStatus::AwaitingApproval => break true,
            RunStatus::Planning => {}
            RunStatus::AwaitingAnswers if !answered => { answered = true; call(&state, &goals, &reg, "answer", json!({ "id": run, "answers": {} })).await; }
            _ => { let e = r.error.clone().unwrap_or_else(|| format!("the run ended as {:?} before it could start", r.status)); goals.cancel(run); return fail(&e); }
        }
        if waited.elapsed() > Duration::from_secs(120) { goals.cancel(run); return fail("the run took too long to get ready"); }
    };
    if !parked { return fail("the run could not be parked"); }
    // 2. the plan must be exactly the saga's tasks (an id each): anything else means the saga must not take this run
    let ids: Vec<String> = goals.get(run).map(|r| r.tasks.iter().map(|t| t.id.clone()).collect()).unwrap_or_default();
    if ids != open.iter().map(|(tid, _, _)| format!("s{tid}")).collect::<Vec<_>>() { goals.cancel(run); return fail("the run did not take the saga's tasks: try again"); }
    // 3. the saga takes the run as its own, then it starts
    let pairs: Vec<(i64, String)> = open.iter().map(|(tid, _, _)| (*tid, format!("s{tid}"))).collect();
    if let Err(e) = crate::work::with_db_in(&state, ws, move |c, _| crate::work::link_run(c, id, run, &pairs, "you")).await { goals.cancel(run); return fail(&e); }
    let go = call(&state, &goals, &reg, "approve", json!({ "id": run })).await;
    if go["ok"] != json!(true) { goals.cancel(run); return fail(&why(&go)); }
    if let Some(b) = cfg["budget_usd"].as_f64().filter(|b| *b > 0.0) { watch(goals.clone(), run, b); }
    reply(json!({ "ok": true, "run": run, "episodes": open.len(), "tasks": open.len() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_run_is_asked_the_sagas_title_and_objective_and_its_open_tasks_in_order_and_done_ones_are_left_out() {
        let d = json!({ "saga": {}, "tasks": [
            { "id": 1, "title": "the form", "body": "with a card field", "status": "todo", "subtasks": [{ "title": "validate", "body": "", "status": "todo" }, { "title": "style", "status": "done" }] },
            { "id": 2, "title": "old", "status": "done", "subtasks": [] },
            { "id": 3, "title": "the webhook", "body": "", "status": "in-progress", "subtasks": [] } ] });
        let open = open_episodes(&d);
        assert_eq!(open.iter().map(|t| t.0).collect::<Vec<_>>(), vec![1, 3]);
        assert!(open[0].2.starts_with("the form\n\nwith a card field") && open[0].2.contains("- validate") && !open[0].2.contains("style"), "{:?}", open[0]);
        assert_eq!(open[1].2, "the webhook");
        let g = goal_of(" Add checkout ", "Stripe, test mode", &open.iter().map(|t| (t.1.clone(), t.2.clone())).collect::<Vec<_>>());
        assert_eq!(g, "Add checkout\n\nStripe, test mode\n\nThe tasks, in order:\n1. the form\n2. the webhook\n");
        assert_eq!(crate::goals::enumerated_tasks(&g), 2, "the planner counts them as two tasks");
    }
    #[test]
    fn a_budget_stops_at_the_limit_and_no_budget_never_does() {
        assert!(!over_budget(0.49, Some(0.5)) && over_budget(0.5, Some(0.5)) && over_budget(3.0, Some(0.5)));
        assert!(!over_budget(100.0, None) && !over_budget(100.0, Some(0.0)));
        assert_eq!(run_number(&json!("run:12")), Some(12)); assert_eq!(run_number(&json!(null)), None); assert_eq!(run_number(&json!("x")), None);
    }
}
