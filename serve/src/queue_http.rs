//! The queue's doors: what the pane's side panel reads and does (list, add, cancel, reorder, re-prompt, undo), the router's binding for an item, and the loop that gives each
//! agent its next item. The rules live in `queue.rs`; this file is the wiring to the plan endpoint, the pane rows and the event stream.
use crate::agents_http::{goals_op, pane_delegation};
use crate::dispatch::AppState;
use crate::goals::Goals;
use crate::queue::{Added, Binding, Queues, StartFn, BindFn, Status};
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::sync::Arc;

fn ok(v: Value) -> Response { (StatusCode::OK, Json(v)).into_response() }
fn bad(msg: &str) -> Response { (StatusCode::BAD_REQUEST, Json(json!({ "error": { "message": msg } }))).into_response() }
fn pane_of(b: &Value) -> i64 { b.get("pane_id").and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0) }

/// THE BINDING FOR AN ITEM, decided with the same policy the plan door applies at execution: the pane's delegation settings, the role the words name, and - in `auto`, the mode
/// that already means "the router picks the models" - the merge the measured record names for that role. In `on` the operator's own pair stands; in `off` nothing is routed.
pub async fn bind(state: &Arc<AppState>, pane_id: i64, goal: &str) -> Binding {
    let (d, _) = pane_delegation(state, pane_id).await;
    let at = crate::cli::now_ms();
    if !d.enabled || goal.trim().is_empty() { return Binding { mode: d.mode.clone(), at, why: if d.enabled { String::new() } else { "delegation is off for this pane".into() }, ..Default::default() } }
    let mut b = Binding { mode: d.mode.clone(), at, ..Default::default() };
    if d.roles {
        let route = crate::roles::route_role(goal, "l", d.role_objective);
        b.why = route.get("why").and_then(Value::as_str).unwrap_or("").to_string();
        let name = route.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        if d.mode == "auto" {
            b.label = auto_label(&route, &name);
            if let Some(p) = crate::roles::pair_of_route(&route) { b.pair = serde_json::to_value(&p).unwrap_or(Value::Null); }
        } else { b.label = name; }
        b.role = route;
    }
    if let Some(p) = &d.pair { b.label = format!("{} (your pair)", p.name); b.pair = serde_json::to_value(p).unwrap_or(Value::Null); }
    b
}

/// WHAT THE ITEM'S CHIP SAYS ABOUT THE ROUTE in `auto`. A pair that won is named brain and hands; a SOLO seat that won (item 4, 2026-10-01: `solo: true`, no brain, `measured: true`)
/// is named as writing alone - and binds no pair, there is none to bind. Review 2026-10-01: this read only `pair_of_route`, which needs a brain, so a measured solo winner was
/// labelled `nothing measured yet`, the exact opposite of the record. Only a route nobody measured says that. Pure - tested.
pub fn auto_label(route: &Value, name: &str) -> String {
    let measured = route.get("measured").and_then(Value::as_bool).unwrap_or(false);
    let hands = route.get("hands").and_then(Value::as_str).unwrap_or("?");
    if crate::roles::pair_of_route(route).is_some() {
        format!("{name} · {} plans, {hands} writes", route.get("brain").and_then(Value::as_str).unwrap_or("?"))
    } else if measured && route.get("solo").and_then(Value::as_bool).unwrap_or(false) {
        format!("{name} · {hands} writes alone")
    } else { format!("{name} · nothing measured yet") }
}

/// the closures the queue runs its items with
pub fn runners(state: Arc<AppState>, goals: Arc<Goals>, reg: Arc<crate::agent::AgentRegistry>) -> (StartFn, BindFn) {
    let start: StartFn = { let (st, g, r) = (state.clone(), goals.clone(), reg.clone()); Arc::new(move |mut body: Value| { let (st, g, r) = (st.clone(), g.clone(), r.clone()); Box::pin(async move {
        // the operator's words were written to the thread when the item was queued
        body["announced"] = json!(true);
        let resp = goals_op(st, g, r, "plan".into(), Bytes::from(serde_json::to_vec(&body).unwrap_or_default())).await;
        let (status, bytes) = { let (parts, b) = resp.into_parts(); (parts.status, axum::body::to_bytes(b, usize::MAX).await.unwrap_or_default()) };
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if status.is_success() { Ok(v) } else { Err(v.pointer("/error/message").and_then(Value::as_str).unwrap_or("the router refused this ask").to_string()) }
    }) }) };
    let bindf: BindFn = { let st = state.clone(); Arc::new(move |pane: i64, goal: String| { let st = st.clone(); Box::pin(async move { bind(&st, pane, &goal).await }) }) };
    (start, bindf)
}

/// the loop: every agent's next item, one at a time
pub fn spawn_loop(queues: Arc<Queues>, goals: Arc<Goals>, start: StartFn, bindf: BindFn) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            queues.tick(&goals, &start, &bindf, crate::cli::now_ms()).await;
        }
    });
}

fn note(reg: &crate::agent::AgentRegistry, pane: i64, text: String) { if pane > 0 { reg.emit(pane, json!({ "type": "antiloki", "subtype": "note", "tone": "dim", "text": text }), true); } }

/// GET /cockpit/queue?pane_id= - the panel's whole story: items in order, the state, and the checkpoint history
pub fn list(queues: &Arc<Queues>, pane_id: i64) -> Response {
    let items = queues.list(pane_id, 30);
    // (an item whose request carries an EMPTY cwd - a pane with no box yet - is not a box: keep looking at older ones)
    let cwd = items.iter().rev().find_map(|i| i.request.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()).map(String::from)).unwrap_or_default();
    let cps: Vec<Value> = if cwd.is_empty() { vec![] } else { crate::checkpoint::history(&cwd, &format!("pane-{pane_id}"), 12).into_iter().map(|(n, rev, subject)| json!({ "n": n, "rev": rev.chars().take(8).collect::<String>(), "subject": subject })).collect() };
    // which queued items name a file another active item names (deterministic: the paths their words carry)
    let conflicts: Vec<Value> = items.iter().filter(|a| a.status.active()).flat_map(|a| items.iter().filter(|b| b.status.active() && b.id > a.id && b.paths.iter().any(|p| a.paths.contains(p))).map(move |b| json!([a.id, b.id]))).collect();
    ok(json!({ "ok": true, "items": items, "state": queues.state_json(pane_id), "checkpoints": cps, "conflicts": conflicts, "idle": queues.doing(pane_id).is_none() }))
}

pub async fn op(state: Arc<AppState>, goals: Arc<Goals>, reg: Arc<crate::agent::AgentRegistry>, queues: Arc<Queues>, start: StartFn, bindf: BindFn, op: String, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let now = crate::cli::now_ms();
    match op.as_str() {
        "add" => {
            let pane = pane_of(&b); let ask = b.get("goal").and_then(Value::as_str).unwrap_or("").trim().to_string();
            if pane <= 0 || ask.is_empty() { return bad("pane_id and goal required") }
            // the request the plan door will get when it is this item's turn: the pane's own body, minus the goal
            let mut request = b.clone(); if let Some(o) = request.as_object_mut() { o.remove("goal"); }
            let cwd = match request.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()) {
                Some(c) => c.to_string(),
                None => { let c = crate::chat_pane::pane_row(&state, pane).await.and_then(|p| p.get("worktree_path").and_then(Value::as_str).map(String::from)).unwrap_or_default(); request["cwd"] = json!(c); c }
            };
            let binding = bind(&state, pane, &ask).await;
            let id = match queues.add(pane, &ask, request, binding, now) {
                Added::Duplicate(d) => return ok(json!({ "ok": true, "duplicate": true, "item": queues.get(d) })),
                Added::New(id) => id,
            };
            // what the operator said goes in the thread NOW, whether the item runs now or later
            reg.emit(pane, json!({ "type": "antiloki", "subtype": "user", "text": ask }), true);
            let idle = queues.claim_if_idle(pane, id, now);
            if !idle {
                let pos = queues.list(pane, 0).iter().filter(|i| i.status == Status::Queued).position(|i| i.id == id).map(|p| p + 1).unwrap_or(1);
                note(&reg, pane, format!("▸ queued #{id} — {} ahead of it{}", pos, queues.get(id).map(|i| if i.binding.label.is_empty() { String::new() } else { format!(" · {}", i.binding.label) }).unwrap_or_default()));
                return ok(json!({ "ok": true, "queued": true, "item": queues.get(id) }));
            }
            // an idle agent starts at once, and the router's refusal comes back to the pane exactly as it did when the pane called the plan door itself
            match queues.start_item(id, &start, &bindf, cwd, now).await {
                Ok(run) => ok(json!({ "ok": true, "item": queues.get(id), "run": run.get("run").cloned().unwrap_or(run) })),
                Err(why) => bad(&why),
            }
        }
        "cancel" => {
            let id = b.get("id").and_then(Value::as_i64).unwrap_or(0);
            match queues.cancel(id, now) {
                Ok(run) => { if let Some(r) = run { goals.cancel(r); } if let Some(i) = queues.get(id) { note(&reg, i.pane_id, format!("▸ #{id} cancelled")); } ok(json!({ "ok": true, "item": queues.get(id) })) }
                Err(e) => bad(&e),
            }
        }
        "reorder" => {
            let (id, index) = (b.get("id").and_then(Value::as_i64).unwrap_or(0), b.get("index").and_then(Value::as_u64).unwrap_or(0) as usize);
            match queues.reorder(id, index) { Ok(i) => ok(json!({ "ok": true, "index": i })), Err(e) => bad(&e) }
        }
        "reprompt" => {
            let (id, text) = (b.get("id").and_then(Value::as_i64).unwrap_or(0), b.get("goal").and_then(Value::as_str).unwrap_or(""));
            match queues.reprompt(id, text) {
                Ok(it) => { let nb = bind(&state, it.pane_id, &it.goal).await; queues.update(id, |i| i.binding = nb); note(&reg, it.pane_id, format!("▸ #{id} re-prompted: {}", it.goal.chars().take(100).collect::<String>())); ok(json!({ "ok": true, "item": queues.get(id) })) }
                Err(e) => bad(&e),
            }
        }
        // UNDO A FINISHED ITEM: put the box back to the checkpoint taken before it began. Refused while the agent is working; itself checkpointed first, so it can be undone.
        "restore" => {
            let id = b.get("id").and_then(Value::as_i64).unwrap_or(0);
            let Some(it) = queues.get(id) else { return bad("no such item") };
            if it.status.active() { return bad("it is not finished - cancel it instead") }
            if queues.doing(it.pane_id).is_some() { return bad("the agent is working - undo when it is idle") }
            let (Some(cp), Some(cwd)) = (it.cp_before.clone(), it.request.get("cwd").and_then(Value::as_str).map(String::from)) else { return bad("this item has no checkpoint (the box is not a git repository)") };
            crate::checkpoint::take(&cwd, &format!("pane-{}", it.pane_id), &format!("before undoing #{id}"), &queues.state_json(it.pane_id), now);
            let n = crate::checkpoint::restore(&cwd, &cp.tree);
            note(&reg, it.pane_id, format!("▸ #{id} undone — the box is back at checkpoint {} ({n} file(s) changed)", cp.n));
            // A RUN THE QA GATE PASSED AND THE OPERATOR NOW REVERTED IS A FALSE ACCEPT: the one error nothing else catches. Count it against the judge that accepted it.
            if let Some(r) = it.run_id.and_then(|rid| goals.get(rid)) {
                // "QA accepted it" = the isolated judge passed it, OR the claim gate proved at least one claim and found none unmet
                let accepted = r.qa_review.as_ref().map_or(false, |q| q.passed && q.error.is_none()) || r.qa_gate.as_ref().map_or(false, |g| g.met > 0 && g.unmet == 0);
                if accepted {
                    // the judge is the seat the gate RAN ON, recorded on the result (`QaGate.judge`): the gate is routed per defect class now, so it is no longer the job binding. A run from before has none - then the old rule.
                    let judge = r.qa_gate.as_ref().map(|g| g.judge.clone()).filter(|j| !j.is_empty()).unwrap_or_else(|| {
                        let b = crate::cli::job_binding_paired(crate::cli::Job::QaGate, r.pair.as_ref(), r.engine, r.cost).0;
                        crate::merges::key(b.engine.key(), b.model.as_deref())
                    });
                    if crate::merges::record_override("qa-gate", &judge).unwrap_or(false) {
                        note(&reg, it.pane_id, format!("▸ QA ({judge}) had passed #{id} and you undid it - counted as a false accept against that judge"));
                    }
                }
            }
            ok(json!({ "ok": true, "files": n, "checkpoint": cp.n }))
        }
        _ => bad("unknown queue op"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::Item;

    static PANES: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(9_000_000);
    /// a pane id no other test uses (the queue store is process-global)
    fn fresh_pane() -> i64 { PANES.fetch_add(1, std::sync::atomic::Ordering::SeqCst) }
    fn scratch(tag: &str) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!("antiloki-queue-http-{tag}-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    /// an AppState whose `cockpit_panes` holds the given (id, settings) rows
    fn state_with(panes: &[(i64, Value)]) -> Arc<AppState> {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY, settings_json TEXT)").unwrap();
        for (id, settings) in panes { conn.execute("INSERT INTO cockpit_panes (id, settings_json) VALUES (?1, ?2)", rusqlite::params![id, settings.to_string()]).unwrap(); }
        crate::dispatch::test_app_state(conn)
    }
    async fn bind_for(settings: Value, goal: &str) -> Binding {
        let pane = fresh_pane();
        bind(&state_with(&[(pane, settings)]), pane, goal).await
    }

    // ── bind: the policy the plan door applies, decided when the item is queued ──
    #[tokio::test]
    async fn delegation_off_binds_nothing_and_says_why() {
        let b = bind_for(json!({ "delegate": { "enabled": false }, "router": { "mode": "on" } }), "add a footer").await;
        assert_eq!(b.why, "delegation is off for this pane");
        assert_eq!(b.mode, "on", "the pane's mode is still reported");
        assert!(b.label.is_empty() && b.role.is_null() && b.pair.is_null());
        assert!(b.at > 0);
    }

    #[tokio::test]
    async fn an_empty_ask_is_bound_to_nothing_without_an_excuse() {
        for goal in ["", "   \n\t "] {
            let b = bind_for(json!({}), goal).await;
            assert!(b.why.is_empty() && b.label.is_empty() && b.role.is_null() && b.pair.is_null(), "{goal:?}: {b:?}");
            assert_eq!(b.mode, "auto");
        }
    }

    #[tokio::test]
    async fn a_pane_the_db_has_never_heard_of_gets_the_defaults_routed_by_role() {
        // no such pane, and no such table: both read as "default delegation" - enabled, auto, roles on
        let b = bind(&state_with(&[]), fresh_pane(), "fix the login api endpoint").await;
        assert_eq!(b.mode, "auto");
        assert!(b.role["role"].is_string() && !b.why.is_empty(), "the role was decided: {b:?}");
        let bare = crate::dispatch::test_app_state(rusqlite::Connection::open_in_memory().unwrap());
        let b = bind(&bare, fresh_pane(), "fix the login api endpoint").await;
        assert_eq!(b.mode, "auto", "no cockpit_panes table at all is not an error");
        assert!(b.role["role"].is_string());
    }

    #[tokio::test]
    async fn in_auto_with_nothing_measured_the_label_says_so_and_no_pair_is_invented() {
        let goal = "restyle the css of the checkout button";
        let b = bind_for(json!({}), goal).await;
        let route = crate::roles::route_role(goal, "l", crate::roles::Objective::Best);
        let name = route["name"].as_str().unwrap();
        if route["measured"].as_bool() == Some(true) {
            assert_eq!(b.label, auto_label(&route, name), "{b:?}");
            assert_eq!(b.pair.is_null(), route["solo"].as_bool() == Some(true), "a pair is bound unless a seat works alone: {b:?}");
        } else {
            assert_eq!(b.label, format!("{name} · nothing measured yet"));
            assert!(b.pair.is_null(), "an unmeasured cell must not name a pair");
        }
        assert_eq!(b.why, route["why"].as_str().unwrap());
        assert_eq!(b.role["role"], route["role"]);
    }

    #[test]
    fn a_measured_solo_winner_is_labelled_as_writing_alone_not_as_unmeasured() {
        // the route item 4 returns when ONE seat beats every pair: no brain, solo, measured
        let solo = json!({ "role": "backender", "name": "Backender", "brain": null, "hands": "claude-code:sonnet", "solo": true, "measured": true });
        assert_eq!(auto_label(&solo, "Backender"), "Backender · claude-code:sonnet writes alone");
        assert!(crate::roles::pair_of_route(&solo).is_none(), "and no pair is bound around a seat that works alone");
        // a pair keeps its sentence
        let pair = json!({ "role": "backender", "brain": "claude-code:opus", "hands": "cursor:composer-2.5", "solo": false, "measured": true });
        assert_eq!(auto_label(&pair, "Backender"), "Backender · claude-code:opus plans, cursor:composer-2.5 writes");
        // nothing measured says so - also when `solo` is set on a route that was not measured
        let none = json!({ "role": "backender", "brain": null, "hands": null, "solo": false, "measured": false });
        assert_eq!(auto_label(&none, "Backender"), "Backender · nothing measured yet");
        let odd = json!({ "brain": null, "hands": "claude-code:sonnet", "solo": true, "measured": false });
        assert_eq!(auto_label(&odd, "Backender"), "Backender · nothing measured yet");
    }

    #[tokio::test]
    async fn in_on_and_off_the_role_is_named_but_the_router_picks_no_pair() {
        let goal = "add a sort dropdown to the table";
        let name = crate::roles::route_role(goal, "l", crate::roles::Objective::Best)["name"].as_str().unwrap().to_string();
        for mode in ["on", "off"] {
            let b = bind_for(json!({ "router": { "mode": mode } }), goal).await;
            assert_eq!(b.mode, mode);
            assert_eq!(b.label, name, "the label is the role alone: no models are chosen for the operator ({mode})");
            assert!(b.pair.is_null(), "{mode}");
        }
        // the old boolean still reads as on/off when no mode was ever set
        assert_eq!(bind_for(json!({ "router": { "enabled": false } }), goal).await.mode, "off");
    }

    #[tokio::test]
    async fn with_role_routing_switched_off_only_the_pane_settings_speak() {
        let b = bind_for(json!({ "roles": { "enabled": false } }), "add a sort dropdown").await;
        assert!(b.label.is_empty() && b.role.is_null() && b.why.is_empty() && b.pair.is_null(), "{b:?}");
        assert_eq!(b.mode, "auto");
    }

    #[tokio::test]
    async fn the_operators_own_pair_always_wins_the_label_and_the_pair() {
        let settings = json!({ "pair": { "name": "mine", "brain": { "engine": "claude-code", "model": "opus" }, "hands": { "engine": "claude-code", "model": "sonnet" } } });
        for mode in ["auto", "on", "off"] {
            let mut s = settings.clone();
            s["router"] = json!({ "mode": mode });
            let b = bind_for(s, "add a footer").await;
            assert_eq!(b.label, "mine (your pair)", "{mode}");
            assert_eq!(b.pair["name"], "mine");
            assert_eq!((b.pair["brain_model"].as_str(), b.pair["hands_model"].as_str()), (Some("opus"), Some("sonnet")));
        }
        // and with role routing off the pair still labels the item
        let mut s = settings.clone();
        s["roles"] = json!({ "enabled": false });
        assert_eq!(bind_for(s, "add a footer").await.label, "mine (your pair)");
        // `solo` is no pair
        let b = bind_for(json!({ "pair": "solo" }), "add a footer").await;
        assert!(b.pair.is_null() && !b.label.contains("your pair"));
    }

    #[tokio::test]
    async fn effort_decides_which_measured_winner_the_role_reads() {
        for (stop, want) in [(0, "cheaper"), (1, "best"), (2, "faster"), (7, "best")] {
            let b = bind_for(json!({ "dial": { "cost": stop } }), "add a sort dropdown").await;
            assert_eq!(b.role["objective"], want, "effort stop {stop}");
        }
    }

    #[tokio::test]
    async fn a_measured_winner_becomes_the_pair_in_auto_and_only_in_auto() {
        // seed this thread's throwaway record (merges::path() is per test thread under cfg(test)) with one winner per side
        let cc = |m: &str| format!("claude-code:{m}");
        let row = |job: &str, planner: String, worker: String| crate::merges::Outcome { job: job.into(), size: "l".into(), planner, worker, qa: String::new(), tasks: 4, pass_first: 4, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(1.0), ms: 40_000, usd: 0.5, source: "bench".into(), audit_only: false, domain: "any".into(), stack: "any".into(), suite: String::new() };
        crate::merges::record(&row("planner", cc("opus"), "-".into())).unwrap();
        crate::merges::record(&row("editor-l", cc("opus"), cc("sonnet"))).unwrap();
        let goal = "add a sort dropdown to the table";
        let route = crate::roles::route_role(goal, "l", crate::roles::Objective::Best);
        assert_eq!(route["measured"], true, "the seeded record is read: {route}");
        let auto = bind_for(json!({}), goal).await;
        let name = route["name"].as_str().unwrap();
        assert_eq!(auto.label, format!("{name} · claude-code:opus plans, claude-code:sonnet writes"));
        assert_eq!((auto.pair["brain"].as_str(), auto.pair["hands"].as_str()), (Some("claude-code"), Some("claude-code")));
        assert_eq!((auto.pair["brain_model"].as_str(), auto.pair["hands_model"].as_str()), (Some("opus"), Some("sonnet")));
        assert_eq!(auto.pair["name"], route["role"], "the pair is named after the role");
        let on = bind_for(json!({ "router": { "mode": "on" } }), goal).await;
        assert!(on.pair.is_null(), "in `on` the operator's own bars stand: the router names no models");
        assert_eq!(on.label, name);
    }

    // ── list: the panel's whole story ──
    fn ask(q: &Arc<Queues>, pane: i64, text: &str, now: i64) -> i64 {
        match q.add(pane, text, json!({}), Binding::default(), now) { Added::New(id) => id, Added::Duplicate(d) => d }
    }
    fn body(r: Response) -> Value {
        let (_, b) = r.into_parts();
        let bytes = futures_util::FutureExt::now_or_never(axum::body::to_bytes(b, usize::MAX)).expect("a ready body").unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn an_empty_pane_lists_nothing_and_is_idle() {
        let q = crate::queue::global();
        let v = body(list(&q, fresh_pane()));
        assert_eq!(v["ok"], true);
        assert_eq!((v["items"].clone(), v["checkpoints"].clone(), v["conflicts"].clone(), v["idle"].clone()), (json!([]), json!([]), json!([]), json!(true)));
        assert_eq!(v["state"]["queued"], json!([]));
    }

    #[test]
    fn two_active_items_that_name_the_same_file_conflict_in_id_order_and_settled_ones_never_do() {
        let q = crate::queue::global();
        let pane = fresh_pane();
        let a = ask(&q, pane, "edit src/app.rs to add logging", 1);
        let b = ask(&q, pane, "rename things in src/app.rs and src/lib.rs", 2);
        let c = ask(&q, pane, "document src/lib.rs", 3);
        let d = ask(&q, pane, "delete src/app.rs", 4);          // will be settled
        let e = ask(&q, pane, "tidy src/other.rs", 5);           // touches nothing the others do
        for (id, paths) in [(a, vec!["src/app.rs"]), (b, vec!["src/app.rs", "src/lib.rs"]), (c, vec!["src/lib.rs"]), (d, vec!["src/app.rs"]), (e, vec!["src/other.rs"])] {
            assert_eq!(q.get(id).unwrap().paths, paths, "the words carry these paths");
        }
        q.update(d, |i| i.status = Status::Done);
        q.update(a, |i| i.status = Status::Doing);
        let v = body(list(&q, pane));
        assert_eq!(v["conflicts"], json!([[a, b], [b, c]]), "a↔b share app.rs, b↔c share lib.rs; d is settled, e shares nothing");
        assert_eq!(v["idle"], false, "an item is doing");
        let order: Vec<i64> = v["items"].as_array().unwrap().iter().map(|i| i["id"].as_i64().unwrap()).collect();
        assert_eq!(order, vec![d, a, b, c, e], "settled history first, then doing, then queued in order");
    }

    #[test]
    fn a_conflict_pair_is_listed_once_and_never_with_itself() {
        let q = crate::queue::global();
        let pane = fresh_pane();
        let a = ask(&q, pane, "fix src/x.rs", 1);
        let b = ask(&q, pane, "also fix src/x.rs by hand", 2);
        let c = ask(&q, pane, "and src/x.rs a third time", 3);
        let v = body(list(&q, pane));
        assert_eq!(v["conflicts"], json!([[a, b], [a, c], [b, c]]));
    }

    #[test]
    fn checkpoints_are_read_from_the_newest_item_that_knows_its_box() {
        let dir = scratch("cps");
        let cwd = dir.to_string_lossy().to_string();
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(&dir, &["add", "-A"]); git(&dir, &["commit", "-qm", "base"]);
        let pane = fresh_pane();
        let q = crate::queue::global();
        let scope = format!("pane-{pane}");
        let c1 = crate::checkpoint::take(&cwd, &scope, "before #1", &json!({}), 10).expect("a checkpoint");
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        crate::checkpoint::take(&cwd, &scope, "after #1", &json!({}), 20).expect("a second one");
        let with_cwd = match q.add(pane, "first ask", json!({ "cwd": cwd }), Binding::default(), 1) { Added::New(i) => i, _ => panic!() };
        let without = match q.add(pane, "second ask", json!({ "cwd": "" }), Binding::default(), 2) { Added::New(i) => i, _ => panic!() };
        let v = body(list(&q, pane));
        let cps = v["checkpoints"].as_array().unwrap();
        assert_eq!(cps.len(), 2, "{v}");
        assert_eq!(cps[0]["rev"].as_str().unwrap().len(), 8, "revisions are shortened for the panel");
        assert!(cps.iter().any(|c| c["rev"].as_str().unwrap() == &c1.rev[..8]), "the first checkpoint is in the history");
        assert!(cps.iter().all(|c| c["n"].is_number() && c["subject"].is_string()));
        // an item whose request has no cwd is skipped in favour of an older one that has it (newest WITH a cwd)
        assert!(q.get(without).is_some() && q.get(with_cwd).is_some());
        // a pane none of whose items name a box has no history to show
        let bare = fresh_pane();
        ask(&q, bare, "hello there", 1);
        assert_eq!(body(list(&q, bare))["checkpoints"], json!([]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let o = crate::proc::cmd("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]).args(args).current_dir(dir).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).to_string()
    }

    // ── op: every door of the queue ──
    #[allow(dead_code)]
    struct Harness {
        state: Arc<AppState>, goals: Arc<Goals>, reg: Arc<crate::agent::AgentRegistry>, queues: Arc<Queues>,
        start: StartFn, bindf: BindFn, dir: std::path::PathBuf, pane: i64,
        started: Arc<std::sync::Mutex<Vec<Value>>>, next_start: Arc<std::sync::Mutex<Result<Value, String>>>, label: Arc<std::sync::Mutex<String>>,
    }
    fn harness() -> Harness {
        let pane = fresh_pane();
        let dir = scratch("op");
        let state = state_with(&[(pane, json!({}))]);
        let (run_fn, _, _) = crate::goals::scripted_prompts_for_queue();
        let goals = crate::goals::Goals::new(run_fn, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 1, 5);
        let reg = crate::agent::AgentRegistry::new();
        std::fs::create_dir_all(dir.join(".antiloki")).unwrap(); // the thread's event log lives here
        reg.set_root(pane, &dir.to_string_lossy());
        let started: Arc<std::sync::Mutex<Vec<Value>>> = Arc::new(std::sync::Mutex::new(vec![]));
        let next_start: Arc<std::sync::Mutex<Result<Value, String>>> = Arc::new(std::sync::Mutex::new(Ok(json!({ "run": { "id": 4242 } }))));
        let label = Arc::new(std::sync::Mutex::new("Routed".to_string()));
        let start: StartFn = { let (s, n) = (started.clone(), next_start.clone()); Arc::new(move |b: Value| { let (s, n) = (s.clone(), n.clone()); Box::pin(async move { s.lock().unwrap().push(b); n.lock().unwrap().clone() }) }) };
        let bindf: BindFn = { let l = label.clone(); Arc::new(move |_p: i64, _g: String| { let l = l.clone(); Box::pin(async move { Binding { label: l.lock().unwrap().clone(), ..Default::default() } }) }) };
        Harness { state, goals, reg, queues: crate::queue::global(), start, bindf, dir, pane, started, next_start, label }
    }
    impl Harness {
        async fn call(&self, op_name: &str, b: Value) -> (StatusCode, Value) {
            let r = op(self.state.clone(), self.goals.clone(), self.reg.clone(), self.queues.clone(), self.start.clone(), self.bindf.clone(), op_name.into(), Bytes::from(b.to_string())).await;
            let status = r.status();
            (status, body(r))
        }
        async fn add(&self, goal: &str) -> (StatusCode, Value) { self.call("add", json!({ "pane_id": self.pane, "goal": goal, "cwd": self.dir.to_string_lossy() })).await }
        /// what the pane's thread shows, in order, as (subtype, text)
        fn thread(&self) -> Vec<(String, String)> {
            let p = crate::agent::events_read_path(&self.dir.to_string_lossy(), self.pane);
            std::fs::read_to_string(p).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .map(|v| (v["ev"]["subtype"].as_str().unwrap_or("").to_string(), v["ev"]["text"].as_str().unwrap_or("").to_string())).collect()
        }
    }

    #[tokio::test]
    async fn an_unknown_op_and_a_malformed_body_are_refused_plainly() {
        let h = harness();
        let (st, v) = h.call("frobnicate", json!({})).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"]["message"], "unknown queue op");
        for b in [json!({}), json!({ "pane_id": h.pane }), json!({ "pane_id": h.pane, "goal": "   " }), json!({ "pane_id": 0, "goal": "x" }), json!({ "pane_id": -3, "goal": "x" }), json!({ "goal": "x" })] {
            let (st, v) = h.call("add", b.clone()).await;
            assert_eq!((st, v["error"]["message"].as_str()), (StatusCode::BAD_REQUEST, Some("pane_id and goal required")), "{b}");
        }
        // garbage that is not JSON at all reads as an empty body
        let r = op(h.state.clone(), h.goals.clone(), h.reg.clone(), h.queues.clone(), h.start.clone(), h.bindf.clone(), "add".into(), Bytes::from("not json")).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert!(h.started.lock().unwrap().is_empty(), "nothing started");
    }

    #[tokio::test]
    async fn the_pane_id_may_arrive_as_a_string_and_an_ask_on_an_idle_agent_starts_at_once() {
        let h = harness();
        let (st, v) = h.call("add", json!({ "pane_id": h.pane.to_string(), "goal": "  add   a footer  ", "cwd": h.dir.to_string_lossy(), "engine": "claude-code" })).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!((v["ok"].clone(), v["item"]["status"].clone(), v["run"].clone()), (json!(true), json!("doing"), json!({ "id": 4242 })));
        assert_eq!(v["item"]["run_id"], 4242);
        let sent = h.started.lock().unwrap().clone();
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0]["goal"].as_str(), sent[0]["pane_id"].as_i64(), sent[0]["engine"].as_str()), (Some("add a footer"), Some(h.pane), Some("claude-code")), "the plan door gets the pane's own body, the consolidated goal and the pane");
        assert_eq!(v["item"]["ask"], "add   a footer", "the operator's words are kept as typed (trimmed)");
        assert_eq!(h.thread(), vec![("user".to_string(), "add   a footer".to_string())], "the words are in the thread now, and no 'queued' note for an item that starts at once");
        assert_eq!(v["item"]["binding"]["label"], "Routed");
    }

    #[tokio::test]
    async fn a_second_ask_waits_behind_the_running_one_and_the_thread_says_how_many_are_ahead() {
        let h = harness();
        h.add("first thing to do").await;
        let (st, v) = h.add("second thing to do").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!((v["queued"].clone(), v["item"]["status"].clone()), (json!(true), json!("queued")));
        let (_, v3) = h.add("third thing to do").await;
        let th = h.thread();
        assert_eq!(th.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), vec!["user", "user", "note", "user", "note"]);
        // the label is the item's OWN binding, decided by the policy when it was queued (not the closure the turn re-binds with)
        let label = v["item"]["binding"]["label"].as_str().unwrap();
        assert!(!label.is_empty());
        assert_eq!(th[2].1, format!("▸ queued #{} — 1 ahead of it · {label}", v["item"]["id"]));
        assert_eq!(th[4].1, format!("▸ queued #{} — 2 ahead of it · {}", v3["item"]["id"], v3["item"]["binding"]["label"].as_str().unwrap()));
        assert_eq!(h.started.lock().unwrap().len(), 1, "only the first was started");
    }

    #[tokio::test]
    async fn the_same_ask_twice_returns_the_existing_item_and_does_not_speak_twice() {
        let h = harness();
        let (_, first) = h.add("add a footer to the page").await;
        let (st, again) = h.add("Add a footer to the page!").await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!((again["duplicate"].clone(), again["item"]["id"].clone()), (json!(true), first["item"]["id"].clone()));
        assert_eq!(h.thread().len(), 1, "the operator's words are in the thread once");
        assert_eq!(h.started.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_router_refusal_comes_back_to_the_pane_and_settles_the_item_as_failed() {
        let h = harness();
        *h.next_start.lock().unwrap() = Err("the router refused this ask".into());
        let (st, v) = h.add("do something impossible").await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"]["message"], "the router refused this ask");
        let it = h.queues.list(h.pane, 5).into_iter().next().unwrap();
        assert_eq!((it.status, it.note.as_str()), (Status::Failed, "the router refused this ask"));
        assert!(h.queues.doing(h.pane).is_none(), "and the agent is free again");
    }

    #[tokio::test]
    async fn a_missing_cwd_falls_back_to_the_pane_row_and_then_to_empty() {
        let h = harness();
        // no cwd in the body and no pane row in this db: the request carries an empty cwd rather than none
        let (st, _) = h.call("add", json!({ "pane_id": h.pane, "goal": "no box named" })).await;
        assert_eq!(st, StatusCode::OK);
        let sent = h.started.lock().unwrap().clone();
        assert_eq!(sent[0]["cwd"], "");
    }

    #[tokio::test]
    async fn cancelling_a_queued_item_is_immediate_and_a_running_one_hands_back_its_run() {
        let h = harness();
        let (_, running) = h.add("the running one").await;
        let (_, waiting) = h.add("the waiting one").await;
        let (rid, wid) = (running["item"]["id"].as_i64().unwrap(), waiting["item"]["id"].as_i64().unwrap());
        let (st, v) = h.call("cancel", json!({ "id": wid })).await;
        assert_eq!((st, v["item"]["status"].clone()), (StatusCode::OK, json!("cancelled")));
        assert!(h.thread().iter().any(|(s, t)| s == "note" && t == &format!("▸ #{wid} cancelled")));
        let (st, v) = h.call("cancel", json!({ "id": rid })).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["item"]["note"], "cancelling", "a running item is told to stop; the loop settles it when the run ends");
        // a settled one cannot be cancelled, an unknown one does not exist, and a missing id is an unknown one
        let (st, v) = h.call("cancel", json!({ "id": wid })).await;
        assert_eq!((st, v["error"]["message"].as_str()), (StatusCode::BAD_REQUEST, Some("it is already settled")));
        for b in [json!({ "id": 99_999_999 }), json!({}), json!({ "id": "x" })] {
            let (st, v) = h.call("cancel", b).await;
            assert_eq!((st, v["error"]["message"].as_str()), (StatusCode::BAD_REQUEST, Some("no such item")));
        }
    }

    #[tokio::test]
    async fn reordering_moves_only_queued_items_and_reports_where_they_landed() {
        let h = harness();
        let (_, run) = h.add("running now").await;
        let mut ids = vec![];
        for g in ["queued alpha", "queued beta", "queued gamma"] { ids.push(h.add(g).await.1["item"]["id"].as_i64().unwrap()); }
        let (st, v) = h.call("reorder", json!({ "id": ids[2], "index": 0 })).await;
        assert_eq!((st, v["index"].clone()), (StatusCode::OK, json!(0)));
        let queued: Vec<i64> = h.queues.list(h.pane, 0).iter().filter(|i| i.status == Status::Queued).map(|i| i.id).collect();
        assert_eq!(queued, vec![ids[2], ids[0], ids[1]]);
        let (_, v) = h.call("reorder", json!({ "id": ids[2], "index": 99 })).await;
        assert_eq!(v["index"], 2, "past the end is the end");
        let (st, v) = h.call("reorder", json!({ "id": run["item"]["id"] })).await;
        assert_eq!((st, v["error"]["message"].as_str()), (StatusCode::BAD_REQUEST, Some("only a queued item can be moved")), "the running one is history");
        let (st, _) = h.call("reorder", json!({ "id": 0 })).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn re_prompting_re_reads_the_words_re_binds_and_tells_the_thread() {
        let h = harness();
        h.add("the running one").await;
        let (_, q) = h.add("remove the header from src/app.rs").await;
        let id = q["item"]["id"].as_i64().unwrap();
        assert_eq!(q["item"]["removes"], true);
        let (st, v) = h.call("reprompt", json!({ "id": id, "goal": "  add a   sidebar\nto src/layout.rs " })).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        let it = &v["item"];
        assert_eq!((it["goal"].as_str(), it["removes"].clone(), it["paths"].clone(), it["note"].as_str()), (Some("add a sidebar to src/layout.rs"), json!(false), json!(["src/layout.rs"]), Some("re-prompted")));
        assert_eq!(it["binding"]["mode"], "auto", "re-bound by the policy (bind), not by the closure");
        assert!(h.thread().iter().any(|(s, t)| s == "note" && t == &format!("▸ #{id} re-prompted: add a sidebar to src/layout.rs")));
        for (b, why) in [(json!({ "id": id, "goal": "   " }), "the new prompt is empty"), (json!({ "id": 99_999_999, "goal": "x" }), "no such item"), (json!({ "id": h.queues.doing(h.pane).unwrap().id, "goal": "x" }), "only a queued item can be re-prompted")] {
            let (st, v) = h.call("reprompt", b).await;
            assert_eq!((st, v["error"]["message"].as_str()), (StatusCode::BAD_REQUEST, Some(why)));
        }
    }

    #[tokio::test]
    async fn undo_refuses_every_state_it_cannot_honour_and_names_the_reason() {
        let h = harness();
        let (st, v) = h.call("restore", json!({ "id": 99_999_999 })).await;
        assert_eq!((st, v["error"]["message"].as_str()), (StatusCode::BAD_REQUEST, Some("no such item")));
        let (_, first) = h.add("first job").await;
        let (_, second) = h.add("second job").await;
        let (fid, sid) = (first["item"]["id"].as_i64().unwrap(), second["item"]["id"].as_i64().unwrap());
        let (_, v) = h.call("restore", json!({ "id": fid })).await;
        assert_eq!(v["error"]["message"], "it is not finished - cancel it instead");
        // settle the second by hand: the first still holds the agent, so undoing is refused
        h.queues.update(sid, |i| i.status = Status::Done);
        let (_, v) = h.call("restore", json!({ "id": sid })).await;
        assert_eq!(v["error"]["message"], "the agent is working - undo when it is idle");
        h.queues.update(fid, |i| i.status = Status::Done);
        let (_, v) = h.call("restore", json!({ "id": sid })).await;
        assert_eq!(v["error"]["message"], "this item has no checkpoint (the box is not a git repository)");
    }

    #[tokio::test]
    async fn undo_puts_the_box_back_to_the_checkpoint_taken_before_the_item_and_can_itself_be_undone() {
        let h = harness();
        git(&h.dir, &["init", "-q"]);
        std::fs::write(h.dir.join("a.txt"), "one\n").unwrap();
        git(&h.dir, &["add", "-A"]); git(&h.dir, &["commit", "-qm", "base"]);
        let cwd = h.dir.to_string_lossy().to_string();
        let (st, v) = h.add("change a.txt").await;
        assert_eq!(st, StatusCode::OK, "{v}");
        let id = v["item"]["id"].as_i64().unwrap();
        let before = h.queues.get(id).unwrap().cp_before.expect("the box was checkpointed before the item began... or took_if_changed found nothing to record");
        // the item's work: edit a file and add another
        std::fs::write(h.dir.join("a.txt"), "two\n").unwrap();
        std::fs::write(h.dir.join("new.txt"), "made by the agent\n").unwrap();
        h.queues.update(id, |i| i.status = Status::Done);
        let (st, v) = h.call("restore", json!({ "id": id })).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["files"], 2, "one rewritten, one removed");
        assert_eq!(v["checkpoint"], before.n);
        assert_eq!(std::fs::read_to_string(h.dir.join("a.txt")).unwrap(), "one\n");
        assert!(!h.dir.join("new.txt").exists(), "a file the checkpoint never had is removed");
        assert!(h.thread().iter().any(|(s, t)| s == "note" && t.starts_with(&format!("▸ #{id} undone — the box is back at checkpoint {}", before.n)) && t.ends_with("(2 file(s) changed)")), "{:?}", h.thread());
        // the undo was checkpointed first, so it can be undone: its checkpoint holds the state before undoing
        let cps = crate::checkpoint::history(&cwd, &format!("pane-{}", h.pane), 12);
        assert!(cps.iter().any(|(_, _, subject)| subject.contains(&format!("before undoing #{id}"))), "{cps:?}");
    }

    /// RULE: the closure the queue re-binds with at an item's turn is the SAME policy `bind` applies when the item is queued.
    #[tokio::test]
    async fn the_rebinding_closure_is_the_queue_time_policy() {
        let h = harness();
        let (_start, bindf) = runners(h.state.clone(), h.goals.clone(), h.reg.clone());
        let goal = "add a sort dropdown to the table";
        let again = bindf(h.pane, goal.to_string()).await;
        let first = bind(&h.state, h.pane, goal).await;
        assert_eq!((again.label, again.mode, again.why), (first.label, first.mode, first.why), "same policy, same verdict");
        // and the pane's own settings travel with it
        let off = crate::dispatch::test_app_state({
            let c = rusqlite::Connection::open_in_memory().unwrap();
            c.execute_batch("CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY, settings_json TEXT)").unwrap();
            c.execute("INSERT INTO cockpit_panes VALUES (1, ?1)", [json!({ "delegate": { "enabled": false } }).to_string()]).unwrap();
            c
        });
        let (_s, b2) = runners(off, h.goals.clone(), h.reg.clone());
        assert_eq!(b2(1, goal.to_string()).await.why, "delegation is off for this pane");
    }
}
