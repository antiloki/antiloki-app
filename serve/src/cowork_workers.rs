//! THE TEMPORARY WORKER PANES' DATA (RFC 0007 §5.10): when the agent in a terminal pane delegates (`antiloki_delegate`), the backend
//! already plans it, picks the worker by the benchmarks (the merge), runs it headless under the pane's scope, verifies and joins.
//! None of that changes. This only READS the pane's runs and says, per task, who is working, on what, and how it went, so the screen
//! can open a pane for the worker while it works and close it when it is done.
//!
//!   GET /api/v1/cockpit/cowork/workers?pane_id=N   -> { workers: [ { run_id, task, title, status, engine, model, ... } ] }
//!
//! A worker's output is not streamed (the engine's call is collected whole), so what a worker pane shows is the run's own record: its
//! events, then its tools, files, tokens, cost and verdict once the call returns. Pure over run JSON; the handler only finds the runs.
use crate::dispatch::{engine_call, AppState};
use crate::goals::Goals;
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// A finished task stays listed this long (the screen decides how long it keeps the pane open; this only bounds the list).
const KEEP_MS: i64 = 10 * 60 * 1000;
/// The words of the task's own events the pane prints, newest last.
const EVENTS_PER_TASK: usize = 10;

fn text(v: &Value, max: usize) -> String { v.as_str().unwrap_or("").chars().take(max).collect() }

/// `worker cursor composer-2.5` (the line the run logs when a task's worker starts) -> (engine, model).
pub fn parse_worker_event(t: &str) -> Option<(String, Option<String>)> {
    let mut it = t.strip_prefix("worker ")?.split_whitespace();
    let engine = it.next()?.to_string();
    let model = it.next().filter(|m| *m != "-").map(String::from);
    Some((engine, model))
}

/// One descriptor per task that has started (running, verifying or settled), across the runs of one pane. Pure.
pub fn workers_from_runs(runs: &[Value], now_ms: i64) -> Vec<Value> {
    let mut out = vec![];
    for run in runs {
        let events = run["events"].as_array().cloned().unwrap_or_default();
        let pair = &run["pair"];
        let why = events.iter().find(|e| e["kind"] == "route").map(|e| text(&e["text"], 300));
        for t in run["tasks"].as_array().into_iter().flatten() {
            let status = t["status"].as_str().unwrap_or("");
            if !matches!(status, "running" | "verifying" | "done" | "failed" | "blocked") { continue; }
            let ended = t["ended"].as_i64().or_else(|| run["ended"].as_i64());
            if matches!(status, "done" | "failed" | "blocked") && ended.map_or(false, |e| now_ms - e > KEEP_MS) { continue; }
            let tid = t["id"].as_str().unwrap_or("");
            let mine: Vec<&Value> = events.iter().filter(|e| e["task"].as_str() == Some(tid)).collect();
            // who: the engine that answered (its trace), else the one the run logged at start, else the merge's hands
            let logged = mine.iter().rev().find_map(|e| parse_worker_event(e["text"].as_str().unwrap_or("")));
            let engine = t["trace"]["engine"].as_str().map(String::from).or_else(|| logged.as_ref().map(|l| l.0.clone())).or_else(|| pair["hands"].as_str().map(String::from));
            let model = t["trace"]["model"].as_str().map(String::from).or_else(|| logged.as_ref().and_then(|l| l.1.clone())).or_else(|| pair["hands_model"].as_str().map(String::from));
            let skip = mine.len().saturating_sub(EVENTS_PER_TASK);
            out.push(json!({
                "run_id": run["id"], "task": tid, "title": text(&t["title"], 160), "status": status, "run_status": run["status"],
                "wave": t["wave"], "attempts": t["attempts"],
                "engine": engine, "model": model,
                "brain": pair["brain"].as_str().or_else(|| run["engine"].as_str()), "merge": pair["name"],
                "why": why, "started": t["started"], "ended": t["ended"],
                "tools": t["trace"]["tools"], "tokens_in": t["trace"]["tokensIn"], "tokens_out": t["trace"]["tokensOut"], "cost": t["trace"]["cost"],
                "changed": t["changed"], "verdict": t["verdict"], "error": t["error"], "report": text(&t["report"], 600),
                "events": mine.iter().skip(skip).map(|e| json!({ "t": e["t"], "kind": e["kind"], "text": text(&e["text"], 300) })).collect::<Vec<_>>(),
            }));
        }
    }
    out
}

/// `GET /api/v1/cockpit/cowork/workers?pane_id=N`
pub async fn workers_get(state: Arc<AppState>, goals: Arc<Goals>, q: HashMap<String, String>) -> Response {
    let pane_id: i64 = q.get("pane_id").and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
    let pane = engine_call(&state, "system.cockpit.pane.get", json!({ "id": pane_id }), ctx).await.map(|(v, _)| v).unwrap_or(Value::Null);
    // the same key the delegate door files a pane's runs under (agents_http: the pane's name without its `chat-`)
    let key = pane["subagent_name"].as_str().unwrap_or("").trim_start_matches("chat-").to_string();
    if key.is_empty() { return (StatusCode::OK, Json(json!({ "ok": true, "workers": [] }))).into_response(); }
    let runs: Vec<Value> = goals.list(Some(&key)).into_iter().map(|r| json!(r)).collect();
    (StatusCode::OK, Json(json!({ "ok": true, "workers": workers_from_runs(&runs, crate::cli::now_ms()) }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> Value {
        json!({ "id": 12, "status": "running", "engine": "claude-code", "pair": { "name": "claude-cursor", "brain": "claude-code", "hands": "cursor", "hands_model": "composer-2.5" },
            "events": [
                { "t": 1, "kind": "route", "text": "one worker on cursor composer-2.5 at tier low — the pane is the brain, not the hands" },
                { "t": 2, "kind": "worker", "text": "worker cursor composer-2.5", "task": "t1" },
                { "t": 3, "kind": "verify", "text": "t1 passed", "task": "t1" },
                { "t": 4, "kind": "worker", "text": "worker opencode deepseek-v4-flash", "task": "t2" } ],
            "tasks": [
                { "id": "t1", "title": "fix the picker", "status": "done", "wave": 0, "attempts": 1, "started": 100, "ended": 9_000, "changed": ["src/picker.tsx"], "report": "done", "error": null,
                  "verdict": { "passed": true, "reasons": ["met"] }, "trace": { "engine": "cursor", "model": "composer-2.5", "tools": [{ "name": "edit", "target": "src/picker.tsx" }], "tokensIn": 11_000, "tokensOut": 400, "cost": 0.02 } },
                { "id": "t2", "title": "add a test", "status": "running", "wave": 1, "attempts": 1, "started": 9_100, "ended": null, "changed": [], "report": null, "error": null, "verdict": null, "trace": null },
                { "id": "t3", "title": "not started", "status": "pending", "wave": 2, "attempts": 0, "started": null, "ended": null, "changed": [], "report": null, "error": null, "verdict": null, "trace": null } ] })
    }

    #[test]
    fn the_worker_event_names_the_engine_and_model() {
        assert_eq!(parse_worker_event("worker cursor composer-2.5"), Some(("cursor".into(), Some("composer-2.5".into()))));
        assert_eq!(parse_worker_event("worker claude-code -"), Some(("claude-code".into(), None)));
        assert_eq!(parse_worker_event("planning with claude-code"), None);
    }

    #[test]
    fn a_started_task_is_a_worker_and_a_waiting_one_is_not() {
        let w = workers_from_runs(&[run()], 10_000);
        assert_eq!(w.iter().map(|x| x["task"].as_str().unwrap()).collect::<Vec<_>>(), vec!["t1", "t2"], "t3 has not started: no pane yet");
        let (t1, t2) = (&w[0], &w[1]);
        // a finished task reports what it did, from its own trace
        assert_eq!((t1["engine"].clone(), t1["model"].clone(), t1["status"].clone(), t1["cost"].clone()), (json!("cursor"), json!("composer-2.5"), json!("done"), json!(0.02)));
        assert_eq!((t1["tools"][0]["name"].clone(), t1["changed"][0].clone(), t1["verdict"]["passed"].clone()), (json!("edit"), json!("src/picker.tsx"), json!(true)));
        assert_eq!(t1["events"].as_array().unwrap().len(), 2, "only its own events: {t1}");
        // a running task has no trace yet: the pane still knows who it is, from the line the run logged when the worker started
        assert_eq!((t2["engine"].clone(), t2["model"].clone(), t2["status"].clone(), t2["cost"].clone()), (json!("opencode"), json!("deepseek-v4-flash"), json!("running"), Value::Null));
        // the merge: the pane is the brain
        assert_eq!((t2["brain"].clone(), t2["merge"].clone()), (json!("claude-code"), json!("claude-cursor")));
        assert!(t2["why"].as_str().unwrap().contains("the pane is the brain"));
    }

    #[test]
    fn with_no_worker_line_the_merges_hands_are_the_worker_and_old_finished_tasks_are_dropped() {
        let mut r = run();
        r["events"] = json!([]);
        r["tasks"][1]["status"] = json!("verifying");
        let w = workers_from_runs(&[r.clone()], 10_000);
        assert_eq!((w[1]["engine"].clone(), w[1]["model"].clone()), (json!("cursor"), json!("composer-2.5")));
        // t1 ended at 9_000; eleven minutes later it is gone, the one still running stays
        let later = workers_from_runs(&[r], 9_000 + 11 * 60 * 1000);
        assert_eq!(later.iter().map(|x| x["task"].as_str().unwrap()).collect::<Vec<_>>(), vec!["t2"]);
    }

    #[test]
    fn a_failed_task_keeps_its_error_and_a_solo_run_names_its_own_engine_as_the_brain() {
        let r = json!({ "id": 3, "status": "failed", "engine": "codex", "pair": null, "events": [],
            "tasks": [{ "id": "t1", "title": "x", "status": "failed", "wave": 0, "attempts": 2, "started": 1, "ended": 5, "changed": [], "report": "", "error": "rejected twice", "verdict": null, "trace": { "engine": "codex", "model": null, "tools": [] } }] });
        let w = workers_from_runs(&[r], 6);
        assert_eq!((w[0]["status"].clone(), w[0]["error"].clone(), w[0]["brain"].clone(), w[0]["merge"].clone(), w[0]["engine"].clone()), (json!("failed"), json!("rejected twice"), json!("codex"), Value::Null, json!("codex")));
        assert!(workers_from_runs(&[json!({})], 0).is_empty() && workers_from_runs(&[], 0).is_empty());
    }
}
