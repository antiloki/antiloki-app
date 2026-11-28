//! THE SAGA NODE (RFC 0011 §9): a thin, typed door over `work.rs` and `saga_run.rs`. NO NEW STORAGE: the sagas and episodes are the workspace's own (`work.sqlite`).
//! Ops: `read` (status, rollup, detail, episodes, list), `write` (create or update a saga or an episode, with an optional Check on the shape of its words),
//! `run` (the goal a saga's open episodes make, for the goal-run machinery to start; or stop), `sync` (link a team run's result back as an episode's state).
//! Scope is the team's own workspace: `root` is the workspace the team lives in, never a parameter of the call.
use crate::work;
use serde_json::{json, Value};

fn s(v: &Value) -> String { v.as_str().unwrap_or("").trim().to_string() }

/// The optional Check on a saga's or an episode's words (the same rule kinds as a Check node): why it may not be written, or None.
fn shape(op: &Value, text: &str) -> Option<String> {
    let rules: Vec<crate::team_check::Rule> = serde_json::from_value(op["check"].clone()).ok()?;
    if rules.is_empty() { return None; }
    let r = crate::team_check::evaluate(&rules, text, &[]);
    if r.pass { None } else { Some(format!("it does not pass its check: {}", r.failed.iter().filter_map(|f| f["message"].as_str().map(String::from)).collect::<Vec<_>>().join("; "))) }
}

pub fn op(root: &str, actor: &str, o: &Value) -> Result<Value, String> {
    let c = work::open(root)?;
    match s(&o["op"]).as_str() {
        "read" => {
            let what = { let w = s(&o["what"]); if w.is_empty() { "detail".to_string() } else { w } };
            if what == "list" { return Ok(json!({ "ok": true, "sagas": work::list(&c)? })); }
            if what == "episode" { return work::episode_detail(&c, o["episode"].as_i64().ok_or("say which `episode`")?); }
            let id = o["saga"].as_i64().ok_or("say which `saga` (its id)")?;
            match what.as_str() {
                "status" => Ok(json!({ "ok": true, "saga": id, "status": work::effective_status(&c, id)? })),
                "rollup" => { let d = work::saga_detail(&c, id)?; Ok(json!({ "ok": true, "saga": id, "rollup": d["rollup"] })) }
                "detail" => work::saga_detail(&c, id),
                "episodes" => { let d = work::saga_detail(&c, id)?; Ok(json!({ "ok": true, "saga": id, "episodes": d["episodes"] })) }
                other => Err(format!("read: `what` is status, rollup, detail, episodes, episode or list (not `{other}`)")),
            }
        }
        "write" => {
            if let Some(e) = o.get("episode").filter(|e| e.is_object()) {
                let title = s(&e["title"]); let body = s(&e["body"]);
                if let Some(w) = shape(o, &format!("{title}\n{body}")) { return Err(format!("episode: {w}")); }
                if let Some(id) = e["id"].as_i64() { work::update_episode(&c, id, e, actor)?; return Ok(json!({ "ok": true, "episode": id, "updated": true })); }
                let saga = o["saga"].as_i64().ok_or("say which `saga` the episode is in")?;
                let id = work::create_episode(&c, saga, e["parent"].as_i64(), &title, &body, &s(&e["assignee"]), actor)?;
                return Ok(json!({ "ok": true, "episode": id, "created": true }));
            }
            let title = s(&o["title"]); let objective = s(&o["objective"]);
            if let Some(w) = shape(o, &format!("{title}\n{objective}")) { return Err(format!("saga: {w}")); }
            if let Some(id) = o["saga"].as_i64() { work::update_saga(&c, id, &json!({ "title": title, "objective": objective }), actor)?; return Ok(json!({ "ok": true, "saga": id, "updated": true })); }
            let id = work::create_saga(&c, &title, &objective, actor)?; Ok(json!({ "ok": true, "saga": id, "created": true }))
        }
        "run" => {
            // the goal the machinery runs: the saga's title and objective, then its open episodes in order. Starting (and stopping) it is `saga_run.rs`'s, with the saga's own budget.
            let id = o["saga"].as_i64().ok_or("say which `saga`")?;
            let d = work::saga_detail(&c, id)?;
            let open = crate::saga_run::open_episodes(&d);
            if open.is_empty() { return Err("the saga has no open episodes to run".into()); }
            let tasks: Vec<(String, String)> = open.iter().map(|(_, t, p)| (t.clone(), p.clone())).collect();
            let goal = crate::saga_run::goal_of(&s(&d["saga"]["title"]), &s(&d["saga"]["objective"]), &tasks);
            Ok(json!({ "ok": true, "saga": id, "goal": goal, "episodes": open.iter().map(|(i, t, _)| json!({ "id": i, "title": t })).collect::<Vec<_>>(), "stop": o["stop"].as_bool().unwrap_or(false), "budget_usd": o["budget_usd"] }))
        }
        "sync" => {
            // a team run's result, linked back as the state of the episode it was for
            let ep = o["episode"].as_i64().ok_or("say which `episode`")?;
            let state = s(&o["state"]);
            if !["todo", "doing", "done", "failed", "canceled"].contains(&state.as_str()) { return Err("state is todo, doing, done, failed or canceled".into()); }
            work::update_episode(&c, ep, &json!({ "status": state }), actor)?;
            let run = s(&o["team_run"]);
            Ok(json!({ "ok": true, "episode": ep, "status": state, "team_run": run }))
        }
        other => Err(format!("saga op is read, write, run or sync (not `{other}`)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ws() -> String { let d = std::env::temp_dir().join(format!("antiloki-saga-node-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d.to_string_lossy().into_owned() }

    #[test]
    fn the_saga_node_reads_writes_runs_and_syncs() {
        let root = ws(); let me = "team:shelf";
        // write: a saga, then episodes in it
        let w = op(&root, me, &json!({ "op": "write", "title": "Restock the shelf", "objective": "Get every new item priced" })).unwrap(); assert_eq!(w["created"], json!(true));
        let saga = w["saga"].as_i64().unwrap();
        let e1 = op(&root, me, &json!({ "op": "write", "saga": saga, "episode": { "title": "Price Orchard Morning", "body": "from the supplier sheet" } })).unwrap()["episode"].as_i64().unwrap();
        let e2 = op(&root, me, &json!({ "op": "write", "saga": saga, "episode": { "title": "Price Winter Blend" } })).unwrap()["episode"].as_i64().unwrap();
        // read: status, rollup, detail, episodes, the list
        assert_eq!(op(&root, me, &json!({ "op": "read", "what": "status", "saga": saga })).unwrap()["status"], json!("todo"));
        assert_eq!(op(&root, me, &json!({ "op": "read", "what": "episodes", "saga": saga })).unwrap()["episodes"].as_array().unwrap().len(), 2);
        assert!(op(&root, me, &json!({ "op": "read", "what": "rollup", "saga": saga })).unwrap()["rollup"].is_object());
        assert_eq!(op(&root, me, &json!({ "op": "read", "saga": saga })).unwrap()["saga"]["title"], json!("Restock the shelf"));
        assert_eq!(op(&root, me, &json!({ "op": "read", "what": "list" })).unwrap()["sagas"].as_array().unwrap().len(), 1);
        assert_eq!(op(&root, me, &json!({ "op": "read", "what": "episode", "episode": e1 })).unwrap()["ok"], json!(true));
        // run: the goal is the saga's words and its open episodes, in order
        let r = op(&root, me, &json!({ "op": "run", "saga": saga, "budget_usd": 2.0 })).unwrap();
        let goal = r["goal"].as_str().unwrap(); assert!(goal.contains("Restock the shelf") && goal.contains("1. Price Orchard Morning") && goal.contains("2. Price Winter Blend"), "{goal}");
        // sync: the run's result is the episode's state; a done episode is no longer run
        assert_eq!(op(&root, me, &json!({ "op": "sync", "episode": e1, "state": "done", "team_run": "r-7" })).unwrap()["status"], json!("done"));
        let after = op(&root, me, &json!({ "op": "run", "saga": saga })).unwrap(); assert!(!after["goal"].as_str().unwrap().contains("Orchard"), "{after}"); assert_eq!(after["episodes"].as_array().unwrap().len(), 1);
        op(&root, me, &json!({ "op": "sync", "episode": e2, "state": "done" })).unwrap();
        assert!(op(&root, me, &json!({ "op": "run", "saga": saga })).unwrap_err().contains("no open episodes"));
        assert_eq!(op(&root, me, &json!({ "op": "read", "what": "status", "saga": saga })).unwrap()["status"], json!("done"));
        // an update, a bad state, an unknown op, and the optional check on the words
        assert_eq!(op(&root, me, &json!({ "op": "write", "saga": saga, "title": "Restock", "objective": "x" })).unwrap()["updated"], json!(true));
        assert!(op(&root, me, &json!({ "op": "sync", "episode": e1, "state": "finished" })).is_err()); assert!(op(&root, me, &json!({ "op": "nope" })).is_err());
        let strict = json!({ "op": "write", "title": "TODO fix later", "check": [ { "kind": "forbid", "pattern": "TODO" } ] });
        assert!(op(&root, me, &strict).unwrap_err().contains("check"), "{:?}", op(&root, me, &strict));
        assert_eq!(op(&root, me, &json!({ "op": "read", "what": "list" })).unwrap()["sagas"].as_array().unwrap().len(), 1, "the refused saga was not written");
    }
}
