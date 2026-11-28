//! NEW TEAM = THE LEADER, NO FORM (the operator 2026-11-24: "on new team instead of that form just open the leader already, and the leader can access a special MCP connection for team onboarding which will make him ask the name of the team and purpose, so he will rename and create the team once we answered").
//!
//! The pane is an ordinary agent that is marked as an onboarding leader (`settings.onboarding`), and it is told, once its terminal is up, to ask the person what the team is for. The MCP tool
//! `antiloki_team_setup` is the only way it creates the team:
//!   - `ask`: returns the question to put to the person (nothing is created);
//!   - `create` {name, purpose}: creates the team through the same path the form used (`team_graph_run::handle_op`), then the marker is cleared, so the pane can no longer create a second team.
//! Any other pane is refused: the tool answers only for a pane marked as onboarding.
use crate::dispatch::{engine_call, AppState};
use crate::pty::PtyRegistry;
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// What the leader is told when its terminal is up. Pure text.
pub const BRIEF: &str = "[antiloki] A person wants to make a new team, with you as its leader. First call the antiloki MCP tool `antiloki_team_setup` with op `ask`: it returns the question to put to the person. Put it to them in plain words, in your reply, and wait for the answer. From the answer, choose a short name for the team yourself (they can ask for another), then call `antiloki_team_setup` with op `create`, that `name`, and the `purpose` in their words. This thread is renamed to the team's name. Then build the team: call `antiloki_team_rubric`, then design its steps with `antiloki_team_design` under that same name, for the job the person described. Do not start a run. Tell them in a few lines what you built and ask what to change.";

/// A SWITCH OF PROVIDER MID-SETUP KEEPS THE SETUP (#31: the new provider got only the handover and made a specialist from the answers). While the pane is still onboarding, the handover starts with the same orders. Pure.
pub fn handover(settings_json: &str, pack: &str) -> String {
    if !is_onboarding(settings_json) { return pack.to_string(); }
    let orders = format!("## FIRST: you are still setting up a new team\n\n{} If the person has already answered, do not ask again: call `create` with their answers. Do not make a specialist for this.\n\n", BRIEF.trim_start_matches("[antiloki] "));
    match pack.split_once("\n\n") { Some((head, rest)) => format!("{head}\n\n{orders}{rest}"), None => format!("{orders}{pack}") }
}
/// The handover carries the orders, so a brief still waiting to be typed is dropped: two messages are never typed into one new terminal.
pub fn drop_waiting(pane_id: i64) { if let Ok(mut m) = pending().lock() { if m.remove(&pane_id).is_some() { keep(&m); } } }
/// The questions the `ask` op returns. Pure.
pub const QUESTIONS: &str = "Ask the person one question, in plain words: what is this team for? Do not ask for a name: choose it from the answer. Wait for the answer before you create anything.";

fn pending() -> &'static Mutex<HashMap<i64, String>> { static P: OnceLock<Mutex<HashMap<i64, String>>> = OnceLock::new(); P.get_or_init(|| Mutex::new(crate::handover_store::load("onboarding"))) }
fn keep(m: &HashMap<i64, String>) { crate::handover_store::save("onboarding", m); }

/// Is this pane's settings the marker of an onboarding leader? Pure.
pub fn is_onboarding(settings_json: &str) -> bool { serde_json::from_str::<Value>(settings_json).ok().and_then(|v| v["onboarding"].as_bool()).unwrap_or(false) }

/// The thread this pane is in, when it can become the new team's: not Main and not already a team's. Pure over the threads view.
pub fn thread_to_adopt(view: &Value, pane: i64) -> Option<String> {
    view["threads"].as_array()?.iter().find(|t| t["panes"].as_array().is_some_and(|ps| ps.iter().any(|p| p["paneId"].as_i64() == Some(pane)))).filter(|t| t["id"] != json!(crate::cowork_threads::MAIN) && !t["team"].is_string()).and_then(|t| t["id"].as_str().map(String::from))
}

fn reply(v: Value) -> Response { (StatusCode::OK, Json(v)).into_response() }

/// `POST /api/v1/cockpit/team/onboard {pane_id}`: mark an agent pane as the leader of a new team, and queue the brief for its terminal's start.
pub async fn onboard_post(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let id = b["pane_id"].as_i64().unwrap_or(0);
    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
    let Some(pane) = engine_call(&state, "system.cockpit.pane.get", json!({ "id": id }), ctx.clone()).await.ok().map(|(v, _)| v).filter(|v| !v.is_null()) else { return reply(json!({ "ok": false, "error": "no such pane" })) };
    let mut settings: Value = serde_json::from_str(pane["settings_json"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
    if !settings.is_object() { settings = json!({}); }
    settings["onboarding"] = json!(true);
    if let Err(e) = engine_call(&state, "system.cockpit.pane.update", json!({ "pane_id": id, "settings": settings }), ctx).await { return reply(json!({ "ok": false, "error": e })); }
    if let Ok(mut m) = pending().lock() { m.insert(id, BRIEF.to_string()); keep(&m); }
    reply(json!({ "ok": true, "pane_id": id }))
}

/// Called when a pane's terminal has just started: type the brief once the composer is up, if one is queued for it.
pub fn after_start(reg: Arc<PtyRegistry>, pane_id: i64) {
    let Some(text) = pending().lock().ok().and_then(|mut m| { let t = m.remove(&pane_id); keep(&m); t }) else { return };
    tokio::spawn(async move {
        let r = crate::pty::pty_send(reg, Bytes::from(json!({ "pane_id": pane_id, "text": text, "wait_s": 90 }).to_string())).await;
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap_or_default(); let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if v["ok"] != json!(true) { if let Ok(mut m) = pending().lock() { m.insert(pane_id, text); keep(&m); } }   // not typed: the next start tries again
    });
}

/// The `antiloki_team_setup` tool. Answers only for an onboarding pane; `create` makes the team and clears the marker.
pub async fn run_tool(state: &Arc<AppState>, you: i64, args: &Value) -> Result<String, String> {
    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
    let pane = engine_call(state, "system.cockpit.pane.get", json!({ "id": you }), ctx.clone()).await.ok().map(|(v, _)| v).filter(|v| !v.is_null()).ok_or("no such pane")?;
    let settings_text = pane["settings_json"].as_str().unwrap_or("{}").to_string();
    if !is_onboarding(&settings_text) { return Err("only the leader of a new team that is being set up can use this tool; a team is made from the new-team button".into()); }
    match args["op"].as_str().unwrap_or("") {
        "ask" => Ok(QUESTIONS.to_string()),
        "create" => {
            let name = args["name"].as_str().unwrap_or("").trim().to_string();
            let purpose = args["purpose"].as_str().unwrap_or("").trim().to_string();
            if name.is_empty() || purpose.is_empty() { return Err("ask the person what the team is for first, then give `name` (yours, short, from their answer) and `purpose`".into()); }
            // the thread the leader is talking in becomes the team's; a leader sitting on Main or on a team's thread gets the team a thread of its own
            let r1 = crate::agents_http::active_root(state).await;
            let own = tokio::task::spawn_blocking(move || thread_to_adopt(&crate::cowork_threads::view(&r1), you)).await.ok().flatten();
            let v = crate::team_graph_run::handle_op(state, json!({ "op": "create", "name": name, "goal": purpose, "template": "blank", "budget_usd": 6.0, "new_team": true, "adopt": own.unwrap_or_default(), "leader": you })).await;
            if v["ok"] != json!(true) { return Err(v["error"].as_str().unwrap_or("the team could not be created").to_string()); }
            // THE PURPOSE IS THE TEAM'S GOAL: a team graph has no goal field, so the purpose is kept as the objective of a saga named after the team (the product's record of a goal)
            let root = crate::agents_http::active_root(state).await;
            let (r2, n2, p2) = (root.clone(), name.clone(), purpose.clone());
            let saga = tokio::task::spawn_blocking(move || crate::work::open(&r2).and_then(|c| crate::work::create_saga(&c, &n2, &p2, "team-setup"))).await.unwrap_or_else(|e| Err(e.to_string()));
            if let Err(e) = saga { return Err(format!("the team was made, but its purpose could not be saved: {e}")); }
            // the marker goes: this pane is the leader now, and it cannot make a second team
            let mut settings: Value = serde_json::from_str(&settings_text).unwrap_or(json!({})); if !settings.is_object() { settings = json!({}); }
            settings["onboarding"] = json!(false);
            let _ = engine_call(state, "system.cockpit.pane.update", json!({ "pane_id": you, "settings": settings }), ctx).await;
            Ok(format!("the team `{name}` is created (thread `{}`), and its purpose is saved as its saga objective. Now build it: call `antiloki_team_rubric`, then `antiloki_team_design` under the name `{name}`. Do not start a run.", v["thread"].as_str().unwrap_or(&name)))
        }
        _ => Err("op must be `ask` (get the questions) or `create` (with name and purpose)".into()),
    }
}

#[cfg(test)]
mod tests {
    #[test] fn the_thread_the_leader_talks_in_is_the_one_that_becomes_the_teams() {
        let v = serde_json::json!({ "threads": [{ "id": "main", "panes": [{ "paneId": 1 }] }, { "id": "new-team", "team": null, "panes": [{ "paneId": 7 }] }, { "id": "squad", "team": "squad", "panes": [{ "paneId": 9 }] }] });
        assert_eq!(thread_to_adopt(&v, 7).as_deref(), Some("new-team")); assert_eq!((thread_to_adopt(&v, 1), thread_to_adopt(&v, 9), thread_to_adopt(&v, 5)), (None, None, None));
    }
    use super::*;
    #[test]
    fn a_handover_in_the_middle_of_a_setup_restates_it() {
        let p = handover(r#"{"onboarding":true}"#, "# Reconciliation pack\n\nwhat was said");
        assert!(p.starts_with("# Reconciliation pack\n\n## FIRST: you are still setting up a new team") && p.contains("antiloki_team_setup") && p.ends_with("what was said"), "{p}");
        assert_eq!(handover(r#"{"onboarding":false}"#, "x"), "x"); assert!(crate::pane_provider::typed_by_antiloki(&serde_json::json!({ "role": "user", "text": BRIEF })));
    }
    #[test]
    fn only_a_pane_marked_as_onboarding_is_an_onboarding_leader() {
        assert!(is_onboarding(r#"{"onboarding":true,"model":"haiku"}"#));
        assert!(!is_onboarding(r#"{"onboarding":false}"#));
        assert!(!is_onboarding(r#"{"model":"haiku"}"#));
        assert!(!is_onboarding("not json"));
    }
    #[test]
    fn the_brief_sends_the_leader_to_ask_first_and_create_only_after_the_answers() {
        assert!(BRIEF.contains("op `ask`") && BRIEF.contains("op `create`") && BRIEF.contains("name") && BRIEF.contains("purpose"));
        assert!(QUESTIONS.contains("what is this team for?") && QUESTIONS.contains("Do not ask for a name") && QUESTIONS.contains("before you create anything"));
        assert!(BRIEF.contains("antiloki_team_design") && BRIEF.contains("Do not start a run"));
    }
}
