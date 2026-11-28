//! THE EXECUTOR (RFC 0013 §3.3, step 5): runs the current version and a request's candidate on the replay bank with the normal run machinery, judges each run by the task's own checks, and hands the
//! outcomes to `team_experiment::run`. This SPENDS THE PERSON'S SEATS: it only starts from an explicit person's call with a budget, one experiment per team at a time, a time limit per run.
//! The candidate runs as a temporary team `<team>--try-<n>` (its graph file is removed afterwards); a run that parks at a human gate or overruns is stopped and counted as a failure.
use crate::team_experiment::{Outcome, Plan};
use serde_json::{json, Value};
use std::path::PathBuf;

pub const RUN_LIMIT_S: u64 = 20 * 60;

fn progress_path(root: &str, team: &str, n: u32) -> PathBuf { crate::team_request::dir(root, team).join(n.to_string()).join("experiment.json") }
pub fn progress(root: &str, team: &str, n: u32) -> Value { std::fs::read_to_string(progress_path(root, team, n)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({ "state": "none" })) }
fn put(root: &str, team: &str, n: u32, v: Value) { let p = progress_path(root, team, n); let _ = std::fs::create_dir_all(p.parent().unwrap()); let _ = std::fs::write(p, v.to_string()); }

/// The run's status word in the state file the driver writes: `{"state":{"status":{"status":"done"}}}` (found by the first real experiment: reading it one level up made every run a failure). Pure.
pub fn status_word(st: &Value) -> &str { st["state"]["status"]["status"].as_str().unwrap_or("") }
/// An agent node writes what it made AND a few lines about what it did (`report`, `said`): the lines about it are not what it produced, and counting them judges the narration. Pure.
fn strip_narration(v: &Value) -> Value { match v { Value::Object(o) => Value::Object(o.iter().filter(|(k, _)| !["report", "said"].contains(&k.as_str())).map(|(k, x)| (k.clone(), x.clone())).collect()), x => x.clone() } }
/// What a finished run produced, as text and file sizes for the task's checks: its artifacts, then the string values it wrote.
pub fn produced(root: &str, run: &str) -> (String, Vec<(String, u64)>) {
    let (mut text, mut files) = (String::new(), vec![]);
    fn walk(d: &std::path::Path, base: &std::path::Path, text: &mut String, files: &mut Vec<(String, u64)>) { for e in std::fs::read_dir(d).into_iter().flatten().flatten() { let p = e.path(); if p.is_dir() { walk(&p, base, text, files); } else if let Ok(m) = p.metadata() { let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().to_string(); files.push((rel, m.len())); if m.len() < 400_000 { if let Ok(t) = std::fs::read_to_string(&p) { text.push_str(&t); text.push('\n'); } } } } }
    let art = crate::team_graph_run::run_dir(root, run).join("artifacts"); walk(&art, &art, &mut text, &mut files);
    if let Some(v) = crate::team_graph_run::run_on_disk(root, run) { if let Some(m) = v["state"]["values"].as_object() { for x in m.values().map(|x| strip_narration(x)) { let x = &x; if let Some(t) = x.as_str() { text.push_str(t); text.push('\n'); } else if let Some(o) = x.as_object() { for y in o.values() { if let Some(t) = y.as_str() { text.push_str(t); text.push('\n'); } } } } } }
    (text, files)
}

/// One run of one version on one task, waited for, judged. A run that could not start is an Err (counted as a failure).
async fn one(root: &str, g: crate::team_graph::Graph, task: &crate::team_replay::Task) -> Result<Outcome, String> {
    let h = crate::team_graph_run::start(root, g, &task.ask, "").await?; let t0 = crate::cli::now_ms();
    loop {
        if crate::team_graph_run::handle(&h.id).is_none() { break; }
        if (crate::cli::now_ms() - t0) as u64 > RUN_LIMIT_S * 1000 { let _ = h.ctl.send(crate::team_graph_run::Ctl::Stop); tokio::time::sleep(std::time::Duration::from_secs(5)).await; break; }
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    }
    let st = crate::team_graph_run::run_on_disk(root, &h.id).unwrap_or(Value::Null); let status = status_word(&st); let done = status == "done";
    let (text, files) = produced(root, &h.id); let verdict = crate::team_replay::judge(task, &text, &files);
    let stopped_at_gate = matches!(status, "blocked" | "paused");
    Ok(Outcome { pass: done && verdict.pass, cost_usd: st["state"]["spent_usd"].as_f64().or(st["spent_usd"].as_f64()).unwrap_or(0.0), wall_ms: crate::cli::now_ms() - t0, gate_stop: stopped_at_gate })
}

/// Start the experiment of request `n` in the background. Returns at once; `progress` says how it goes, and the report is attached to the request (and a low-risk admitted change merges if the person's standing yes is on).
pub fn start(root: &str, team: &str, n: u32, repeats: u32, budget_usd: f64) -> Result<(), String> { start_for(root, team, n, repeats, budget_usd, crate::team_experiment::Goal::Quality, 0.0) }
/// The same, for a GOAL (cost or time must come down by `min_gain`, quality as before).
pub fn start_for(root: &str, team: &str, n: u32, repeats: u32, budget_usd: f64, goal: crate::team_experiment::Goal, min_gain: f64) -> Result<(), String> {
    if budget_usd <= 0.0 { return Err("set an improvement budget first: an experiment spends real model calls".into()); }
    let m = crate::team_request::get(root, team, n).ok_or("no such request")?; if m.status != "open" && m.status != "retry" { return Err("this request is already decided".into()); }
    if progress(root, team, n)["state"] == json!("running") { return Err("this experiment is already running".into()); }
    let live = crate::team_graph_run::load_graph(root, team)?; let mut cand = crate::team_repo::recipe_on(root, team, &m.branch)?;
    let (tune, held) = (crate::team_replay::tasks(root, team, "tune"), crate::team_replay::tasks(root, team, "heldout")); if held.is_empty() { return Err("no held-out tasks: save a few more runs as tasks first".into()); }
    let temp = format!("{}--try-{n}", live.team); cand.team = temp.clone(); crate::team_graph_run::save_graph(root, &cand)?;
    let plan = Plan { repeats: repeats.max(1), budget_usd, regression_left: crate::team_request::regression_left(root, team), goal, min_gain };
    put(root, team, n, json!({ "state": "running", "started_ms": crate::cli::now_ms(), "budget_usd": budget_usd, "repeats": repeats }));
    let (r, t) = (root.to_string(), team.to_string()); let rt = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let res = crate::team_experiment::run(&tune, &held, &plan, &mut |arm, task| { let g = if arm == "current" { live.clone() } else { cand.clone() }; rt.block_on(one(&r, g, task)) });
        let _ = std::fs::remove_file(crate::team_graph_run::graph_path(&r, &temp));
        match res.and_then(|rep| { let v = serde_json::to_value(&rep).unwrap_or_default(); crate::team_request::attach_experiment(&r, &t, n, v).map(|_| rep) }) {
            Ok(rep) => { let auto = crate::team_request::maybe_auto_merge(&r, &t, n, crate::cli::now_ms()).is_some(); put(&r, &t, n, json!({ "state": "done", "admitted": rep.admitted, "why": rep.why, "spent_usd": rep.spent_usd, "auto_merged": auto })); }
            Err(e) => put(&r, &t, n, json!({ "state": "failed", "error": e })),
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn what_a_run_made_is_read_from_its_artifacts_and_values_and_judged_by_the_tasks_own_checks() {
        let root = std::env::temp_dir().join(format!("antiloki-exec-{}", std::process::id())); let _ = std::fs::remove_dir_all(&root); let r = root.to_string_lossy().to_string();
        let run = crate::team_graph_run::run_dir(&r, "run-1"); std::fs::create_dir_all(run.join("artifacts").join("writer")).unwrap();
        std::fs::write(run.join("artifacts").join("writer").join("post.md"), "a short post about tea").unwrap(); std::fs::write(run.join("state.json"), json!({ "state": { "status": { "status": "done" }, "values": { "writer": { "report": "done well and long and chatty", "said": "narration", "text": "a line it made" } } } }).to_string()).unwrap();
        let (text, files) = produced(&r, "run-1"); assert!(text.contains("short post") && text.contains("a line it made") && !text.contains("chatty") && !text.contains("narration") && files == vec![("writer/post.md".to_string(), 22)], "{text:?} {files:?}");
        assert_eq!(status_word(&crate::team_graph_run::run_on_disk(&r, "run-1").unwrap()), "done"); assert_eq!(status_word(&json!({})), "");
        let task = |max: f64| crate::team_replay::Task { id: "t".into(), ask: "a".into(), checks: vec![crate::team_check::Rule { id: "w".into(), kind: "words".into(), max: Some(max), ..Default::default() }], from_run: String::new(), added_ms: 0, split: "tune".into(), oracle: vec![] };
        assert!(crate::team_replay::judge(&task(20.0), &text, &files).pass && !crate::team_replay::judge(&task(3.0), &text, &files).pass);
        assert!(start(&r, "Nope", 1, 3, 0.0).is_err() && start(&r, "Nope", 1, 3, 1.0).is_err(), "no budget or no such request: nothing starts");
        assert_eq!(progress(&r, "Nope", 1)["state"], json!("none"));
    }
}
