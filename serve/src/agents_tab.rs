//! THE AGENTS TAB (operator 2026-09-22: "transform the chats tab into agents tab — all custom agents created under
//! Studio be on there, using the episode architecture we have on the chats but under each agent has its episodes,
//! same logic, same design, inside a custom agent"). A ROSTER: antiChat (the default, the chat pane as it is) and
//! one row per Studio recipe of the active workspace — a face, a name, the pair it runs as, a status line from its
//! runs, what Heimdall saved on it, and its episodes. An agent's pane is made on first open, in the chats session,
//! keyed `agent-<slug>`, with the recipe's provider, scope, specialists and skills; unlike a chat it works in the
//! WORKSPACE and may write — its Heimdall menu keeps the full set of switches.
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::agent::AgentRegistry;
use crate::chat_pane::{chats_session, ctx, episodes_of, events_text, pane_row, set_worktree, settings_of};
use crate::dispatch::{engine_call, AppState};
use crate::goals::{Goals, GoalRun, RunStatus};

fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }
fn num(v: Option<&Value>) -> i64 { v.and_then(Value::as_i64).unwrap_or(0) }

/// The provider an agent's pane opens on: the recipe's own, unless that seat is switched off - then the one Heimdall would plan on (the recipe's own when nothing can run, so the refusal is honest). Pure - tested.
pub fn provider_when_off(provider: &str, blocked: impl Fn(crate::cli::Engine) -> bool, heimdall: impl Fn() -> Option<crate::cli::Engine>) -> String {
    match crate::cli::engine_of(&json!(provider)) {
        Some(e) if blocked(e) => heimdall().map(|h| h.key().to_string()).unwrap_or_else(|| provider.to_string()),
        _ => provider.to_string(),
    }
}
/// THE STATUS LINE — what the roster says under the name, from the agent's runs: where it needs the operator
/// first, then what it is doing, then what it last did. Pure — tested.
pub fn status_of(runs: &[&GoalRun], live: bool) -> (String, &'static str) {
    if let Some(r) = runs.iter().find(|r| r.status == RunStatus::AwaitingAnswers) { return (format!("needs an answer on “{}”", first_line(&r.goal)), "needs"); }
    if let Some(r) = runs.iter().find(|r| r.status == RunStatus::AwaitingApproval) { return (format!("needs a yes on “{}”", first_line(&r.goal)), "needs"); }
    if let Some(r) = runs.iter().find(|r| matches!(r.status, RunStatus::Running | RunStatus::Joining | RunStatus::Planning)) {
        let done = r.tasks.iter().filter(|t| t.status == crate::goals::TaskStatus::Done).count();
        return (if r.tasks.is_empty() { format!("planning “{}”", first_line(&r.goal)) } else { format!("working — {done} of {} on “{}”", r.tasks.len(), first_line(&r.goal)) }, "working");
    }
    if live { return ("live — waiting for an ask".into(), "live"); }
    if let Some(r) = runs.iter().max_by_key(|r| r.ended.unwrap_or(r.created)) {
        return (match r.status { RunStatus::Done | RunStatus::Inline => format!("done: “{}”", first_line(&r.goal)), RunStatus::Failed => format!("failed: “{}”", first_line(&r.goal)), _ => first_line(&r.goal) }, "idle");
    }
    ("idle".into(), "idle")
}
fn first_line(goal: &str) -> String { goal.lines().next().unwrap_or("").trim().chars().take(56).collect() }
/// what the router saved on this agent's runs — the arithmetic (routed runs) and the estimate (MIN turns), apart
pub fn saved_of(runs: &[&GoalRun]) -> (f64, f64) {
    let mut a = 0.0; let mut e = 0.0;
    for r in runs { if let Some(sv) = &r.savings { if r.min { e += sv.saved_usd; } else if r.router { a += sv.saved_usd; } } }
    (a, e)
}

/// a `*_json` column as the array it holds — the roster hands the agent's skills, specialists and scope to the
/// sidebar, which reads them as lists and not as strings of JSON. Pure.
fn arr_col(r: &Value, col: &str) -> Value {
    r.get(col).and_then(Value::as_str).and_then(|t| serde_json::from_str::<Value>(t).ok()).filter(Value::is_array).unwrap_or(json!([]))
}

/// `GET cockpit/agents/roster` — antiChat first, then every recipe of the active workspace with its pane (if opened),
/// pair, status, savings and episodes.
pub async fn roster(state: Arc<AppState>, reg: Arc<AgentRegistry>, goals: Arc<Goals>) -> Response {
    let conn = state.pick();
    let (recipes, panes, root, unpin) = tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        // the roster reads `role` and `archived`, which only the recipes module creates — without this a serve that
        // has not yet served a recipe request queries columns that do not exist and hands back an empty roster
        crate::recipes::ensure(&c);
        // …and DECIDE what main's panes still need unpinning: `open_pane` no longer forces chat + readonly, but a
        // pane created before that still carries them and would stay a read-only chat for ever (operator
        // 2026-09-27). The write is dispatched below, outside this lock — `system.cockpit.pane.update` owns the
        // table and re-bakes the brief, and `engine_call` re-enters the pool, which would deadlock in here.
        let unpin = crate::recipes::unpin_main_panes(&c);
        let root = antiloki_rs::engine_workspace_root::getWorkspaceRoot(&c).as_str().unwrap_or("").to_string();
        // the audit log's technology axis is told the root here, where a connection is already in hand
        // (PLAN-THE-LEAN-YEARS II.7) — it must never take one of its own from a call path
        crate::audit::set_workspace_root(&root);
        let recipes = antiloki_rs::db_json::query_rows(&c, "SELECT slug, name, icon, provider, theme, description, role, objective, scope_json, specialists_json, skills_json, pair_json, box FROM agent_recipes WHERE (workspace = ? OR workspace = '') AND archived = 0 ORDER BY name", &[json!(root)]).unwrap_or(json!([]));
        let panes = antiloki_rs::db_json::query_rows(&c, "SELECT p.id, p.session_id, p.subagent_name, p.pane_kind, p.worktree_path, p.settings_json, p.status FROM cockpit_panes p JOIN cockpit_sessions s ON s.id = p.session_id WHERE s.name = 'chats' AND s.status != 'closed' AND p.subagent_name LIKE 'agent-%'", &[]).unwrap_or(json!([]));
        (recipes, panes, root, unpin)
    }).await.unwrap_or((json!([]), json!([]), String::new(), vec![]));
    // THE WRITE, OUTSIDE THE LOCK (audit 2026-09-28). `cockpit_panes` belongs to `system.cockpit.pane.update`,
    // which also re-bakes the on-disk brief — the half a direct UPDATE silently skipped, and the half the
    // read-only bug was made of. Dispatched here, where no substrate connection is held.
    if !unpin.is_empty() { crate::recipes::apply_unpin(&state, unpin).await; }
    let runs = goals.list(None);
    let mut rows = vec![];
    for r in recipes.as_array().cloned().unwrap_or_default() {
        let slug = s(r.get("slug")).rsplit("::").next().unwrap_or("").to_string();
        let key = format!("agent-{slug}");
        let pane = panes.as_array().and_then(|a| a.iter().find(|p| s(p.get("subagent_name")) == key)).cloned();
        let (pane_id, engine, pair, episodes, live) = match &pane {
            Some(p) => {
                let id = num(p.get("id")); let engine = s(p.get("pane_kind")); let settings = settings_of(p);
                let pair = crate::cli::Pair::of_settings(&settings).map(|x| x.label()).unwrap_or_else(|| "solo".into());
                let eps = episodes_of(events_text(&s(p.get("worktree_path")), id).lines(), &engine);
                // …AND THE EPISODES THAT CAME BACK FROM A FORK (operator 2026-09-27: "only on merge with the master
                // they merge with master episodes (but showing wich episodes came from outside)").
                //
                // A fork's closed episode is already filed into the WORKSPACE's memory.sqlite by `file_episode`, so
                // it outlives the box it ran in — but the list here reads the pane's own event log, which a fork's
                // episodes were never in. Master's timeline was therefore missing exactly the work that was merged
                // into it. Filed episodes belonging to OTHER agents are folded in, carrying the agent that did them
                // so the row can say it came from outside; an episode is never quietly absorbed.
                let mut eps = serde_json::to_value(eps).unwrap_or(json!([])).as_array().cloned().unwrap_or_default();
                let slug_here = { let a = s(settings.get("agent")); if a.is_empty() { s(p.get("subagent_name")).trim_start_matches("agent-").to_string() } else { a } };
                if !root.is_empty() {
                    let mut n = eps.len() as i64;
                    for f in crate::episodes_db::list(&root, None, 50) {
                        let from = s(f.get("agent"));
                        // only what ANOTHER agent did, and only once it has been merged somewhere
                        if from.is_empty() || from == slug_here || s(f.get("merged_into")).is_empty() { continue }
                        if !from.starts_with(&format!("{slug_here}-")) { continue }   // its own forks, not the whole workspace
                        n += 1;
                        eps.push(json!({ "n": n, "seq": 0, "t": f.get("t").cloned().unwrap_or(json!(0)),
                            "reason": "merged in", "engine": f.get("engine").cloned().unwrap_or(json!("")),
                            "turns": f.get("turns").cloned().unwrap_or(json!(0)), "last_t": f.get("t").cloned().unwrap_or(json!(0)),
                            "first_ask": "", "snapshot": "", "title": f.get("title").cloned().unwrap_or(json!("")),
                            "subtitle": f.get("subtitle").cloned().unwrap_or(json!("")), "replies": 0, "first_reply": "",
                            "story": f.get("story").cloned().unwrap_or(json!("")),
                            "box": "", "agent": from, "forked_from": f.get("pane_id").cloned().unwrap_or(json!(0)) }));
                    }
                }
                (id, engine, pair, Value::Array(eps), reg.status(id).get("live").and_then(Value::as_bool).unwrap_or(false))
            }
            None => (0, s(r.get("provider")), r.get("pair_json").and_then(Value::as_str).filter(|t| !t.is_empty()).and_then(|t| serde_json::from_str::<crate::cli::Pair>(t).ok()).map(|p| p.label()).unwrap_or_else(|| "solo".into()), json!([]), false),
        };
        let mine: Vec<&GoalRun> = runs.iter().filter(|x| x.parent == key).collect();
        let (status, tone) = status_of(&mine, live);
        let (saved, est) = saved_of(&mine);
        // the ROLE and the OBJECTIVE ride on the row: the roster reads as "who this is and what it is for", which is
        // what the operator hires (2026-09-23: "an agent carries role, skills, scope, specialists, avatar and its
        // episodes"). `recipes::row` fills the role in for agents made before the column existed.
        // THE MAIN AGENT (2026-09-24, bug 9) is read-only and has no role, no merge and nothing to configure —
        // the roster says so rather than showing it empty chrome that does not apply to it.
        let is_main = slug == crate::recipes::MAIN;
        // MAIN'S NAME IS ITS PANE'S TITLE (operator 2026-09-28: double-click a tab to rename it). Main has no recipe the wizard can
        // save, so `cockpit/chat/rename` writes `settings.title` on its pane and the roster reads it back — only for Main, because
        // every other agent's name is its recipe's and a stale pane title must never outrank an edited recipe
        let name = if is_main { pane.as_ref().map(|p| s(settings_of(p).get("title"))).filter(|t| !t.trim().is_empty()).map(Value::from).unwrap_or_else(|| r.get("name").cloned().unwrap_or(Value::Null)) } else { r.get("name").cloned().unwrap_or(Value::Null) };
        rows.push(json!({ "slug": slug, "name": name, "icon": r.get("icon"), "provider": r.get("provider"), "theme": r.get("theme"), "description": r.get("description"),
            "main": is_main, "readonly": false, "box": r.get("box"),
            "role": crate::recipes::role_of(&r), "objective": r.get("objective"),
            "skills": arr_col(&r, "skills_json"), "specialists": arr_col(&r, "specialists_json"), "scope": arr_col(&r, "scope_json"),
            "pane_id": pane_id, "engine": engine, "pair": pair, "status": status, "tone": tone, "saved": saved, "saved_est": est, "episodes": episodes, "live": live, "runs": mine.len() }));
    }
    // Main first, always — it is the way in, and a roster sorted by name would file it under M
    rows.sort_by_key(|r| if r.get("main") == Some(&json!(true)) { 0 } else { 1 });
    Json(json!({ "ok": true, "workspace": root, "agents": rows })).into_response()
}

/// `POST cockpit/agents/open {slug}` — the recipe's pane, made on first open: the chats session, `agent-<slug>`, the
/// recipe's provider, scope, specialists and skills, the WORKSPACE as its worktree, and the recipe's pair if it has one.
pub async fn open(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let slug = s(b.get("slug")).trim().to_string();
    if slug.is_empty() { return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "slug required" }))).into_response(); }
    match open_pane(&state, &slug).await {
        Ok((v, made)) => (if made { StatusCode::CREATED } else { StatusCode::OK }, Json(v)).into_response(),
        Err((code, e)) => (code, Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}
/// The agent's pane — found, or made (see `open`). `(response body, made)`; the error carries its status.
pub async fn open_pane(state: &Arc<AppState>, slug: &str) -> Result<(Value, bool), (StatusCode, String)> {
    let slug = slug.to_string();
    let key = format!("agent-{slug}");
    let conn = state.pick(); let slug2 = slug.clone(); let key2 = key.clone();
    let (recipe, existing, root) = tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        let root = antiloki_rs::engine_workspace_root::getWorkspaceRoot(&c).as_str().unwrap_or("").to_string();
        let recipe = antiloki_rs::db_json::query_rows(&c, "SELECT slug, name, icon, provider, theme, description, scope_json, specialists_json, skills_json, pair_json, box FROM agent_recipes WHERE (slug = ? OR slug LIKE ?) AND (workspace = ? OR workspace = '') LIMIT 1", &[json!(slug2), json!(format!("%::{slug2}")), json!(root)]).ok().and_then(|r| r.as_array().and_then(|a| a.first()).cloned());
        let existing = antiloki_rs::db_json::query_rows(&c, "SELECT p.id FROM cockpit_panes p JOIN cockpit_sessions s ON s.id = p.session_id WHERE s.name = 'chats' AND s.status != 'closed' AND p.subagent_name = ? AND p.status != 'closed' LIMIT 1", &[json!(key2)]).ok().and_then(|r| r.as_array().and_then(|a| a.first()).map(|p| num(p.get("id")))).unwrap_or(0);
        (recipe, existing, root)
    }).await.unwrap_or((None, 0, String::new()));
    let Some(recipe) = recipe else { return Err((StatusCode::NOT_FOUND, format!("no recipe '{slug}' in this workspace"))) };
    if existing > 0 { let pane = pane_row(state, existing).await.unwrap_or(json!({ "id": existing })); return Ok((json!({ "ok": true, "pane": pane, "made": false }), false)); }
    let r: Result<Value, String> = async {
        let sid = chats_session(state).await?;
        let arr = |k: &str| recipe.get(k).and_then(Value::as_str).and_then(|t| serde_json::from_str::<Value>(t).ok()).unwrap_or(json!([]));
        // EVERY SEAT THE RECIPE MAY NAME (audit 2026-09-28). This list was three engines while `recipes.rs`
        // accepts all six, so a Cursor, Gemini or Grok agent was silently rewritten to `claude-code` and opened
        // as a Claude pane — the seat the operator chose was discarded without a word. One source of truth:
        // `chat_pane::ENGINES`, which is `cli::Engine` in string form.
        let provider = { let p = s(recipe.get("provider"));
            let p = if crate::chat_pane::ENGINES.contains(&p.as_str()) { p } else { "claude-code".into() };
            // …AND A SEAT THAT IS SWITCHED OFF IS NOT OPENED (2026-10-01, the Claude-off audit): Main is seeded on `claude-code`, so with Claude off its pane was born on the seat that refuses
            // every call and only moved on its first send. It opens on the seat Heimdall plans on instead.
            // …AND A SEAT THAT IS NOT INSTALLED, OR NOT SIGNED IN, IS NOT OPENED EITHER (Grokbot QA 2026-10-12, item 5: Main was born on `claude-code` on a machine with no Claude and sat on "the agent is starting again" for ever).
            provider_when_off(&p, |e| crate::cli::model_blocked(e, None).is_some() || !crate::cli::engine_usable(e), || crate::cli::heimdall_engine(None))
        };
        let pair = recipe.get("pair_json").and_then(Value::as_str).filter(|t| !t.is_empty()).and_then(|t| serde_json::from_str::<Value>(t).ok()).unwrap_or(Value::Null);
        let mut settings = json!({ "agent": slug, "title": s(recipe.get("name")), "made": crate::cli::now_ms(), "specialists": arr("specialists_json"), "skills": arr("skills_json"), "pair": pair });
        // MAIN IS NO LONGER A READ-ONLY CHAT (operator 2026-09-27: "first is not suppose to be read-only, we dont
        // have this anymore"; and earlier: "keep just one agent, not specific role, a master agent").
        //
        // Bug 9's main was "the regular chat with full access and no worktrees" — something to ASK, never something
        // that works, which is why it was pinned `chat + readonly + delegate off`. That premise is gone: there is one
        // agent now and it does the work, so pinning it read-only left the operator with an assistant that could not
        // act and a Heimdall that could not route. Three of the six bugs reported tonight came from these three
        // flags, including a plain "hi" answered by opus at $0.0797 because nothing was routing it.
        //
        // The recipe decides now, like every other agent's. Main still ships with scope ["**"] — the whole workspace
        // — which is what "master agent" means.
        //
        // AND IT STARTS ON THE CHEAP RUNG. Heimdall is skipped for a trivial ask on purpose, and the fallback used to
        // be whatever the CLI defaults to. A pane with no model of its own is pinned to its engine's low tier; the
        // router raises it whenever the ask warrants.
        if slug == crate::recipes::MAIN { settings["chat"] = json!(true); settings["readonly"] = json!(false); }
        if slug == crate::recipes::MAIN && settings.get("model").is_none() {
            if let Some(low) = crate::cli::tier_model(crate::cli::engine_of(&json!(provider)).unwrap_or(crate::cli::Engine::ClaudeCode), crate::cli::Tier::Low) {
                settings["model"] = json!(low);
            }
        }
        // A BOX OF ITS OWN (operator 2026-09-24), opt-in per agent. `settings.box == "own"` gives the agent a
        // worktree it does not share, with the workspace's node_modules linked in so the gates still run; anything
        // else keeps the shared tree, which is what makes a change visible in the running preview the moment it is
        // written. Main never gets one: it cannot write, so there is nothing to isolate.
        // …and it may have a box like anyone else; it is no longer "cannot write, so nothing to isolate"
        let own_box = s(recipe.get("box")) == "own";
        let (added, _) = engine_call(state, "system.cockpit.pane.add", json!({ "session_id": sid, "subagent_name": key, "pane_kind": provider, "provision": false, "scope": arr("scope_json"), "extra_prompt_md": s(recipe.get("description")), "settings": settings }), ctx()).await?;
        let pane_id = num(added.get("pane").and_then(|p| p.get("id")));
        if pane_id <= 0 { return Err(format!("pane/add: {}", s(added.get("error")))); }
        let cwd = if own_box && !root.is_empty() {
            match crate::boxes::ensure_box_for(&root, &slug, 1) {
                Ok(p) => p.to_string_lossy().to_string(),
                // a box that cannot be made is reported, not silently swapped for the shared tree: an agent the
                // operator believes is fenced off and is not is worse than an agent that failed to start
                Err(e) => return Err(format!("this agent asked for a box of its own and git refused: {e}")),
            }
        } else { root.clone() };
        if !cwd.is_empty() { set_worktree(state, pane_id, &cwd).await; }
        let pane = pane_row(state, pane_id).await.unwrap_or(added["pane"].clone());
        Ok(json!({ "ok": true, "pane": pane, "session_id": sid, "made": true }))
    }.await;
    r.map(|v| (v, true)).map_err(|e| (StatusCode::BAD_REQUEST, e))
}

/// `POST cockpit/agents/episode/close {pane_id, n, title, to_workspace}` — an episode ends as a MERGE.
///
/// operator 2026-09-24: *"the episodes could be just like new branches in their worktrees… When a episode is
/// closed is like a merge in the code."* For an agent in a shared tree there is nothing to merge and it says so.
pub async fn episode_close(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let pane_id = num(b.get("pane_id"));
    let n = b.get("n").and_then(Value::as_u64).unwrap_or(1) as usize;
    let title = s(b.get("title"));
    let to_workspace = b.get("to_workspace").and_then(Value::as_bool).unwrap_or(true);
    let Some(pane) = pane_row(&state, pane_id).await else { return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "no such pane" }))).into_response() };
    let cwd = s(pane.get("worktree_path"));
    let slug = { let st = settings_of(&pane); let a = s(st.get("agent")); if a.is_empty() { s(pane.get("subagent_name")).trim_start_matches("agent-").to_string() } else { a } };
    let root = workspace_root(&state).await;
    if cwd.is_empty() || cwd == root {
        return Json(json!({ "ok": true, "merged": false,
            "note": "this agent works in the workspace itself, so its episode has nothing to merge — the changes are already where you can see them. Give it a box of its own to make closing an episode a merge." })).into_response();
    }
    let boxp = std::path::PathBuf::from(&cwd);
    let sealed = match crate::boxes::seal_episode(&boxp, &slug, n, &title) { Ok(v) => v, Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response() };
    match crate::boxes::merge_episode(&root, &boxp, &slug, n, to_workspace) {
        Ok(m) => Json(json!({ "ok": m.get("ok").cloned().unwrap_or(json!(true)), "merged": true, "sealed": sealed, "merge": m })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "sealed": sealed, "error": e }))).into_response(),
    }
}
/// `POST cockpit/agents/episode/open {pane_id, n}` — the next episode gets a branch of its own.
/// `GET cockpit/agents/episode/turns?pane_id=&n=` — THE CONVERSATION THAT HAPPENED IN ONE EPISODE (operator
/// 2026-09-27: "the episodes are clickable they carry the history of the conversation, we can see the conversation
/// that happened on that episode onclick").
///
/// The roster already sends each episode's `seq`, because `episodes_of` cut the log there. So an episode's turns are
/// the log between its own `seq` and the NEXT one's — asked of the same function that did the cutting, so the two
/// cannot drift. The last episode has no successor and runs to the end of the log, which is the live one.
pub async fn episode_turns(state: Arc<AppState>, q: std::collections::HashMap<String, String>) -> Response {
    let pane_id: i64 = q.get("pane_id").and_then(|v| v.parse().ok()).unwrap_or(0);
    let n: usize = q.get("n").and_then(|v| v.parse().ok()).unwrap_or(0);
    if n == 0 { return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "n is 1-based and required" }))).into_response() }
    let Some(pane) = pane_row(&state, pane_id).await else { return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "no such pane" }))).into_response() };
    let cwd = s(pane.get("worktree_path"));
    let engine = s(pane.get("pane_kind"));
    let text = tokio::task::spawn_blocking(move || crate::chat_pane::events_text(&cwd, pane_id)).await.unwrap_or_default();
    let eps = crate::chat_pane::episodes_of(text.lines(), &engine);
    let Some(ep) = eps.iter().find(|e| e.n == n) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": format!("this agent has no episode {n}") }))).into_response()
    };
    // the half-open range [this episode's seq, the next episode's seq) — None for the live one
    let to = eps.iter().find(|e| e.n == n + 1).map(|e| e.seq);
    let turns = crate::chat_pane::turns_between(text.lines(), ep.seq, to);
    Json(json!({ "ok": true, "n": n, "title": ep.title, "subtitle": ep.subtitle, "story": ep.story,
                 "engine": ep.engine, "t": ep.t, "last_t": ep.last_t, "live": to.is_none(),
                 "turns": turns })).into_response()
}

/// WHERE EACH FORK'S BOX HAS GOT TO. Keyed by pane, read by `fork_status`, written by the task cutting the box.
///
/// In memory on purpose: it describes work happening in THIS process right now, and a serve that restarts mid-cut
/// has no half-made box to report on — `ensure_box` is idempotent and the next open re-cuts it.
static FORK_PREP: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<i64, Value>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// WHAT THE SCREEN SAYS WHILE THE BOX IS CUT (operator 2026-09-27: "a loading should show with real time info of
/// creating the worktree and funny messages like claude does till the worktree is ready and safe to work on").
///
/// The step is the truth — `git worktree add`, the node_modules link, the excludes — and the line beside it is the
/// product talking. Both come from here so the two can never say different things, and so the funny half can be
/// changed without touching the half that is a fact.
pub fn fork_step_line(step: &str) -> &'static str {
    match step {
        "branch"   => "cutting a branch for your double…",
        "deps"     => "lending it your node_modules — it travels light",
        "excludes" => "teaching it to ignore our paperwork",
        "ready"    => "ready — it has its own box, and its own opinions",
        _          => "getting a box together…",
    }
}
pub fn fork_prep_set(pane_id: i64, step: &str, err: Option<String>) {
    let v = json!({ "step": step, "line": fork_step_line(step), "ready": step == "ready", "error": err, "t": crate::cli::now_ms() });
    FORK_PREP.lock().unwrap_or_else(|p| p.into_inner()).insert(pane_id, v);
}

/// `GET cockpit/pane/fork/status?pane_id=` — how far the box has got. A pane nobody is preparing answers `ready`,
/// because every pane that is not mid-cut has a box already: absence of news is good news here, and a screen that
/// waits forever for a status nobody is writing is worse than one that starts.
pub async fn fork_status(q: std::collections::HashMap<String, String>) -> Response {
    let pane_id: i64 = q.get("pane_id").and_then(|v| v.parse().ok()).unwrap_or(0);
    let v = FORK_PREP.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id).cloned();
    Json(v.unwrap_or_else(|| json!({ "step": "ready", "line": fork_step_line("ready"), "ready": true, "error": Value::Null })))
        .into_response()
}

/// THE FORK GETS A NAME, NOT A TIMESTAMP (operator 2026-09-27: "a name should be assigned to the new agents
/// worktrees. A funny name").
///
/// `main-fork-71119` is a number wearing a name. You cannot say it out loud, you cannot tell two of them apart at a
/// glance, and a worktree you cannot name is a worktree you will not remember to close. So a fork is named the way a
/// container or a dyno is — two words, pronounceable, and in this product's own register: a fork is a DOUBLE of an
/// agent, which is Loki's whole trick, so the words are about mischief and copies rather than colours and animals.
///
/// `taken` decides collisions, injected so the choice is pure and testable. Pure — tested.
pub fn fork_name(base: &str, free: impl Fn(&str) -> bool) -> (String, String) {
    // the double, the trick, the stand-in — a fork is all three
    const FIRST: [&str; 16] = ["sly", "spare", "shadow", "second", "other", "cheeky", "quiet", "restless",
                               "borrowed", "parallel", "wayward", "spry", "double", "crooked", "errant", "stray"];
    const SECOND: [&str; 16] = ["twin", "double", "shapeshifter", "understudy", "changeling", "stunt-double",
                                "decoy", "doppel", "echo", "mimic", "impostor", "stand-in", "duplicate",
                                "counterpart", "alibi", "sidekick"];
    let seed = crate::cli::now_ms() as usize;
    for n in 0..(FIRST.len() * SECOND.len()) {
        let i = (seed + n * 7) % FIRST.len();
        let j = (seed / 3 + n * 11) % SECOND.len();
        let nick = format!("{} {}", FIRST[i], SECOND[j]);
        let slug = format!("{base}-{}-{}", FIRST[i], SECOND[j]);
        if free(&slug) { return (nick, slug) }
    }
    // sixteen times sixteen names all taken is not a naming problem any more
    let slug = format!("{base}-fork-{}", seed % 100_000);
    (format!("fork {}", seed % 100_000), slug)
}

/// Is a box of this name already cut? The fork's name has to be free on DISK, not merely unused in this process.
fn box_taken(root: &str, slug: &str) -> bool {
    !root.is_empty() && crate::boxes::box_of(root, slug).join(".git").exists()
}

/// `POST cockpit/pane/fork {pane_id, name?}` — FORK THIS AGENT (operator 2026-09-27: "the tab will be a new agent,
/// using the same config as the previous agent but in a new worktree and have its own episodes … is temporary
/// because the user can close the agent").
///
/// The same agent, twice, on two worktrees. Everything that makes it what it is — the pair, the roles, the scope,
/// QA, delegation — lives in `settings_json`, so the fork copies that WHOLE object rather than naming the fields it
/// knows about: a setting added next month rides along without this function learning about it.
///
/// WHAT IT DELIBERATELY DOES NOT COPY: the episodes. An episode is a consolidation of ONE session, and handing the
/// fork its parent's would claim work this agent has not done. Its history begins now.
///
/// `forked_from` is recorded in the fork's settings so closing it knows where the work should go back to, and so a
/// roster can say which agents are temporary.
pub async fn pane_fork(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let src_id = num(b.get("pane_id"));
    let Some(src) = pane_row(&state, src_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "no such pane" }))).into_response()
    };
    let mut settings = settings_of(&src);
    let parent_slug = { let a = s(settings.get("agent")); if a.is_empty() { s(src.get("subagent_name")).trim_start_matches("agent-").to_string() } else { a } };
    // a fork of a fork forks the ORIGINAL's name, not `x-fork-fork-fork`
    let base = parent_slug.split("-fork-").next().unwrap_or(&parent_slug).to_string();
    let src_engine = crate::cli::engine_of(&json!(s(src.get("pane_kind")))).unwrap_or(crate::cli::Engine::ClaudeCode);
    let root_now = workspace_root(&state).await;
    let (nick, slug) = fork_name(&base, |s| !box_taken(&root_now, s));
    let name = { let n = s(b.get("name")); if n.trim().is_empty() { nick } else { n } };

    let root = root_now.clone();
    if root.is_empty() { return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "no workspace to fork in" }))).into_response() }
    // the path the box WILL have; it is cut below, after the pane exists to report progress against
    let box_path = crate::boxes::box_of(&root, &slug).to_string_lossy().to_string();

    if let Some(m) = settings.as_object_mut() {
        m.insert("agent".into(), json!(slug));
        m.insert("forked_from".into(), json!(src_id));
        m.insert("forked_at".into(), json!(crate::cli::now_ms()));
        m.insert("temporary".into(), json!(true));
        m.insert("title".into(), json!(name.clone()));      // the tab and the pane agree on what this is

        // WHAT A FORK MUST NOT INHERIT (audited 2026-09-27, after the first real use).
        //
        // "the same config as the previous agent" was the spec and copying the whole settings object was the right
        // way to honour it — but MAIN is a read-only chat, and a faithful copy of a read-only chat is a fork that
        // cannot do the work it was forked to do. The operator's report: no loading, no router, and a process that
        // exited. Three of those come from here.
        //
        //   readonly  — a fork exists to open a SECOND line of work. Read-only makes that impossible.
        //   chat      — a chat pane has no scope, no gates and no status row; the fork is an agent, not a chat.
        //
        // `pair` and `delegate` are NOT forced: those are genuine choices of the parent, and a fork that quietly
        // turned on sub-agents the parent had off would be a different agent, not a double. If Heimdall is off on
        // the parent it is off on the fork, and that is the parent's setting to change.
        m.insert("readonly".into(), json!(false));
        m.insert("chat".into(), json!(true));   // antiChat's chrome and its episodes list — the writable kind

        // THE BASELINE IS THE CHEAP RUNG, NOT THE DEFAULT ONE (operator 2026-09-27: "the chat is running on claude
        // opus. Why? before heimdall, just for activate heimdall it shouldnt run on something expensive … i just
        // said Hi and opus answered this is absurd").
        //
        // Heimdall is skipped for a trivial ask on purpose — a router call costs more than the answer — but the ask
        // then fell through to whatever the CLI defaults to, which on this machine is opus. Measured: "hi" cost
        // $0.0797 for 21,599 tokens. The guard was right and the fallback was backwards.
        //
        // So a pane with no model of its own is PINNED to its engine's low rung. Heimdall still raises it whenever
        // the ask warrants — that is what the router is for — and nothing forces a cheap model onto real work.
        if !m.contains_key("model") {
            if let Some(low) = crate::cli::tier_model(src_engine, crate::cli::Tier::Low) {
                m.insert("model".into(), json!(low));
            }
        }
    }
    let key = format!("agent-{slug}");
    let kind = s(src.get("pane_kind"));
    let sid = num(src.get("session_id"));
    let added = match crate::dispatch::engine_call(&state, "system.cockpit.pane.add",
        json!({ "session_id": sid, "subagent_name": key, "pane_kind": kind, "provision": false, "settings": settings, "scope": crate::chat_pane::pane_scope_globs(&src) }),   // A FORK KEEPS ITS PARENT'S FENCE (2026-09-30: cheeky-double forked from Main with scope ["**"] and got [] - unrestricted)
        crate::dispatch::attribution_ctx(&axum::http::HeaderMap::new())).await {
        Ok((v, _)) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": format!("pane/add: {e}") }))).into_response(),
    };
    let pane_id = num(added.get("pane").and_then(|p| p.get("id")));
    if pane_id <= 0 { return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": s(added.get("error")) }))).into_response() }
    crate::chat_pane::set_worktree(&state, pane_id, &box_path).await;

    // THE TAB OPENS FIRST, THE BOX IS CUT BEHIND IT. Returning only when the worktree is ready means ＋ stalls with
    // nothing on screen; opening the tab first means the operator watches it being made. The composer is gated on
    // `ready` by the screen — a box half-cut is not a box you may write in, and that is the whole reason the status
    // exists rather than a spinner.
    fork_prep_set(pane_id, "start", None);
    let (r2, s2) = (root.clone(), slug.clone());
    tokio::task::spawn_blocking(move || {
        match crate::boxes::ensure_box_with(&r2, &s2, |step| fork_prep_set(pane_id, step, None)) {
            Ok(_) => fork_prep_set(pane_id, "ready", None),
            Err(e) => fork_prep_set(pane_id, "failed", Some(e)),
        }
    });

    let pane = pane_row(&state, pane_id).await.unwrap_or_else(|| added["pane"].clone());
    (StatusCode::CREATED, Json(json!({ "ok": true, "pane": pane, "slug": slug, "name": name, "preparing": true,
                                       "worktree": box_path, "forked_from": src_id, "episodes": [] }))).into_response()
}

pub async fn episode_open(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let pane_id = num(b.get("pane_id"));
    let n = b.get("n").and_then(Value::as_u64).unwrap_or(1) as usize;
    let Some(pane) = pane_row(&state, pane_id).await else { return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "no such pane" }))).into_response() };
    let cwd = s(pane.get("worktree_path"));
    let slug = { let st = settings_of(&pane); let a = s(st.get("agent")); if a.is_empty() { s(pane.get("subagent_name")).trim_start_matches("agent-").to_string() } else { a } };
    if cwd.is_empty() || cwd == workspace_root(&state).await { return Json(json!({ "ok": true, "branch": Value::Null })).into_response() }
    match crate::boxes::start_episode(std::path::Path::new(&cwd), &slug, n) {
        Ok(branch) => Json(json!({ "ok": true, "branch": branch })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}
async fn workspace_root(state: &Arc<AppState>) -> String {
    let conn = state.pick();
    tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        antiloki_rs::engine_workspace_root::getWorkspaceRoot(&c).as_str().unwrap_or("").to_string()
    }).await.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_agent_opens_on_the_heimdall_seat_when_its_own_is_switched_off() {
        use crate::cli::Engine::*;
        let claude_off = |e: crate::cli::Engine| e == ClaudeCode;
        // Main is seeded on claude-code; with Claude off its pane used to be born there and move on the first send
        assert_eq!(provider_when_off("claude-code", claude_off, || Some(Codex)), "codex");
        assert_eq!(provider_when_off("cursor", claude_off, || Some(Codex)), "cursor", "a seat that is on is the recipe's own choice");
        assert_eq!(provider_when_off("claude-code", |_| false, || Some(Codex)), "claude-code");
        assert_eq!(provider_when_off("claude-code", claude_off, || None), "claude-code", "nothing can run: keep the recipe's, the refusal is then honest");
    }
    fn run(goal: &str, status: RunStatus, done: usize, total: usize, saved: Option<(f64, bool)>) -> GoalRun {
        let mut r: GoalRun = serde_json::from_value(json!({ "id": 1, "parent": "agent-x", "goal": goal, "engine": "claude-code", "cwd": "/", "write": true, "concurrency": 1, "status": status, "created": 10, "started": null, "ended": null, "tasks": [], "blackboard": [], "events": [], "report": null, "delivered": false, "error": null })).unwrap();
        for i in 0..total { let mut t: crate::goals::GoalTask = serde_json::from_value(json!({ "id": format!("t{i}"), "title": "t", "prompt": "p", "owns": [], "needs": [], "acceptance": [], "status": if i < done { "done" } else { "pending" }, "wave": 0, "attempts": 1, "started": null, "ended": null, "report": null, "verdict": null, "error": null, "trace": null, "changed": [] })).unwrap(); t.status = if i < done { crate::goals::TaskStatus::Done } else { crate::goals::TaskStatus::Pending }; r.tasks.push(t); }
        if let Some((usd, min)) = saved { r.min = min; r.router = true; r.savings = Some(crate::goals::Savings { tokens_in: 0, tokens_out: 0, actual_usd: 0.0, baseline_usd: usd, saved_usd: usd, saved_pct: 0.0, baseline: String::new(), tiers: vec![] }); }
        r
    }
    #[test]
    fn the_status_line_says_where_the_operator_is_needed_first_then_the_work_then_the_last_thing() {
        let a = run("Ship the 0.1.26 release notes\nmore", RunStatus::AwaitingApproval, 0, 0, None);
        let w = run("Find leads for the launch", RunStatus::Running, 3, 8, None);
        let d = run("Rewrite the authority section", RunStatus::Done, 1, 1, None);
        assert_eq!(status_of(&[&d, &w, &a], true), ("needs a yes on “Ship the 0.1.26 release notes”".into(), "needs"));
        assert_eq!(status_of(&[&d, &w], false), ("working — 3 of 8 on “Find leads for the launch”".into(), "working"));
        assert_eq!(status_of(&[&d], false), ("done: “Rewrite the authority section”".into(), "idle"));
        assert_eq!(status_of(&[], true), ("live — waiting for an ask".into(), "live"));
        assert_eq!(status_of(&[], false), ("idle".into(), "idle"));
        // the money under the name: routed runs are arithmetic, MIN turns an estimate — kept apart
        let r1 = run("x", RunStatus::Done, 1, 1, Some((1.5, false))); let r2 = run("y", RunStatus::Inline, 0, 0, Some((0.4, true)));
        assert_eq!(saved_of(&[&r1, &r2]), (1.5, 0.4));
    }
}
