//! THE SIDE QUESTION (operator 2026-11-10: "a button on the agents' panes where we open a side pane bound to the current pane, which doesn't change code, just answers side questions about the parent pane").
//!
//! `POST /api/v1/cockpit/side/open {parent_pane_id}` opens a second agent of the parent's engine, in a box of its own, bound to the parent (`settings.side_of`). It is held READ-ONLY the way a team's planner is: the hook refuses its writes, the serve's
//! gate puts its box back if anything got past (the engines that have no hooks), and it is told. It starts with a briefing in its guidance and a context file of what the parent has done so far (its last turns and its current diff), and it has the live view
//! through `antiloki_pane_read`. The person types the questions into it like into any agent; the parent is never interrupted.
use crate::dispatch::{engine_call, AppState};
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

const TURNS: usize = 6;
const TURN_CHARS: usize = 900;
const DIFF_CAP: usize = 60_000;

fn cut(s: &str, n: usize) -> String { let t: String = s.chars().take(n).collect(); if s.chars().count() > n { format!("{t} …") } else { t } }

/// What the helper is told it is. Pure.
pub fn guidance(parent_title: &str, parent_pane: i64, parent_dir: &str) -> String {
    format!("You are a SIDE-QUESTION helper for the agent \"{parent_title}\" (pane {parent_pane}). The person asks you questions about what that agent is doing and has done; you answer them. You NEVER change code: every write you make is refused and reverted, so do not try; if something should change, say what, and the person will tell \"{parent_title}\".\n\nWhat to read, in this order: `.antiloki/side-context.md` in this folder (its last turns and its current diff, as of when you were opened); then, for the live state, the antiloki tool `antiloki_pane_read` with pane {parent_pane} (what its terminal shows now), and its folder `{parent_dir}` (read-only: `git -C {parent_dir} diff`, `git -C {parent_dir} log`). Answer short and concrete, and say when you are going by the context file and it may be out of date. Do not start work on your own: wait for the person's question.")
}

/// The context file: the parent's last turns (what was asked, what it answered), the files it changed and its diff. Pure over its inputs.
pub fn context_md(title: &str, pane: i64, turns: &[(String, String)], files: &[String], diff: &str) -> String {
    let mut o = format!("# What \"{title}\" (pane {pane}) has done so far\n\n");
    if turns.is_empty() { o += "It has not been asked anything yet.\n\n"; }
    for (i, (ask, ans)) in turns.iter().enumerate() { o += &format!("## Turn {} of the last {}\n**asked:** {}\n\n**answered:** {}\n\n", i + 1, turns.len(), cut(ask, TURN_CHARS), cut(ans, TURN_CHARS)); }
    o += "## Files it changed (against its starting commit)\n";
    if files.is_empty() { o += "none yet\n\n"; } else { for f in files.iter().take(80) { o += &format!("- {f}\n"); } o += "\n"; }
    if !diff.trim().is_empty() { o += &format!("## Its diff{}\n```diff\n{}\n```\n", if diff.len() > DIFF_CAP { " (cut: it is longer)" } else { "" }, cut(diff, DIFF_CAP)); }
    o
}

fn reply(code: StatusCode, v: Value) -> Response { (code, Json(v)).into_response() }
async fn pane_of(state: &Arc<AppState>, id: i64) -> Option<Value> { engine_call(state, "system.cockpit.pane.get", json!({ "id": id }), json!({ "actor": "web-fe", "source": "web-fe" })).await.ok().map(|(v, _)| v).filter(|v| !v.is_null()) }

/// The side question this agent already has, as the answer that opens it on a screen. Pure over the session.
pub fn open_side_of(session: &Value, parent: i64) -> Option<Value> {
    let settings = |x: &Value| serde_json::from_str::<Value>(x["settings_json"].as_str().unwrap_or("{}")).unwrap_or(Value::Null);
    let x = session["panes"].as_array()?.iter().find(|x| x["status"] != json!("closed") && settings(x)["side_of"].as_i64() == Some(parent))?;
    Some(json!({ "ok": true, "pane_id": x["id"], "session_id": x["session_id"], "title": settings(x)["title"].as_str().unwrap_or("Side question"), "engine": x["pane_kind"], "side_of": parent, "again": true }))
}

/// `POST /api/v1/cockpit/side/open {parent_pane_id}`
pub async fn open(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); let parent = b["parent_pane_id"].as_i64().unwrap_or(0);
    let bad = |e: &str| reply(StatusCode::OK, json!({ "ok": false, "error": e }));
    let Some(p) = pane_of(&state, parent).await else { return bad("no such pane") };
    let engine = p["pane_kind"].as_str().unwrap_or("").to_string();
    if crate::cli::engine_of(&json!(engine)).is_none() { return bad("a side question is asked of an agent: this pane is not one"); }
    let settings: Value = serde_json::from_str(p["settings_json"].as_str().unwrap_or("{}")).unwrap_or(json!({}));
    if settings["side_of"].is_i64() { return bad("this is already a side question: open it from the agent it is about"); }
    let project = crate::agents_http::active_root(&state).await;
    let trails = crate::work::trails_of_project(&state, &project);
    let Some(pbox) = trails.iter().find(|x| x.pane_id == parent).cloned() else { return bad("that agent has nothing to ask about yet: it has not started") };
    // the name the person sees on the agent (renamed on the screen: the layout holds it, so the screen says it), else the pane's own title, else the box's agent
    let title = b["parent_title"].as_str().map(|t| t.trim()).filter(|t| !t.is_empty()).map(|t| t.chars().take(80).collect::<String>()).or_else(|| settings["title"].as_str().filter(|t| !t.is_empty()).map(String::from)).unwrap_or_else(|| pbox.agent.clone());
    let sid = p["session_id"].as_i64().unwrap_or(0);
    // ONE SIDE QUESTION PER AGENT (operator 2026-11-11): a second is refused, whoever asks (the screen's ? closes the open one instead)
    let sess = engine_call(&state, "system.cockpit.session.get", json!({ "id": sid }), json!({ "actor": "web-fe", "source": "web-fe" })).await.ok().map(|(v, _)| v).unwrap_or(Value::Null);
    // The screen asks only when it shows none, so one the serve still holds is one the screen LOST (a reload while it opened): it is handed back. A refusal left the ? stuck, with nothing on screen to close (found 2026-11-28)
    if let Some(had) = open_side_of(&sess, parent) { return reply(StatusCode::OK, had); }
    let side_title = format!("Side · {}", cut(&title, 40));
    let mut extra = guidance(&title, parent, &pbox.dir.to_string_lossy());
    if pbox.shared { extra += "\n\nThat folder is shared: the changes listed for it may come from other agents or from the person too."; }
    let added = engine_call(&state, "system.cockpit.pane.add", json!({ "session_id": sid, "base_agent": "dev", "subagent_name": format!("side-{engine}-{}", crate::cli::now_ms()), "pane_kind": engine, "scope": [], "grants": ["file.read", "file.write"], "provision": true, "tier": "mid", "extra_prompt_md": extra, "settings": { "title": side_title, "side_of": parent } }), json!({ "actor": "web-fe", "source": "web-fe" })).await;
    let new_id = match added { Ok((v, _)) => v["pane"]["id"].as_i64().or_else(|| v["pane_id"].as_i64()).unwrap_or(0), Err(e) => return bad(&e) };
    if new_id <= 0 { return bad("the side pane could not be made"); }
    // the new box is prepared now: only some engines get their `.antiloki` folder when the pane is added, and without it there is no box to hold (found 2026-11-26: the side question of an Antigravity agent failed)
    let _ = engine_call(&state, "system.cockpit.pane.context.bake", json!({ "pane_id": new_id }), json!({ "actor": "web-fe", "source": "web-fe" })).await;
    // its box: held read-only, with the context file written into it
    let (st, pdir, ptitle, np, parent_box) = (state.clone(), pbox.dir.clone(), title.clone(), new_id, pbox.clone());
    let held = tokio::task::spawn_blocking(move || -> Result<String, String> {
        let nb = crate::work::boxes_of_project(&st).into_iter().find(|x| x.pane_id == np).ok_or("the side agent's box is not ready: try again")?;
        crate::team_guard::hold(&nb.dir, "side", &format!("side:{parent}"))?;
        let eps = crate::work_boxes::box_episodes(&parent_box);
        let turns: Vec<(String, String)> = eps.iter().flat_map(|e| e.turns.iter()).filter(|t| t.end_ms.is_some()).map(|t| (t.prompt.clone(), t.answer.clone())).collect::<Vec<_>>().into_iter().rev().take(TURNS).rev().collect();
        // a folder the parent shares is only READ here: git's index is not touched in it, and what is listed is the folder's uncommitted work, whoever did it
        let (patch, files) = if parent_box.shared { (crate::thread_tree::git(&["diff", "HEAD", "--no-ext-diff"], &pdir).unwrap_or_default(), crate::thread_tree::dirty_files(&pdir)) } else { (crate::team_run::patch_of(&pdir).unwrap_or_default(), crate::team_run::names_of(&pdir).unwrap_or_default()) };
        std::fs::create_dir_all(nb.dir.join(".antiloki")).map_err(|e| e.to_string())?;
        std::fs::write(nb.dir.join(".antiloki/side-context.md"), context_md(&ptitle, parent, &turns, &files, &patch)).map_err(|e| e.to_string())?;
        Ok(nb.dir.to_string_lossy().to_string())
    }).await.unwrap_or_else(|e| Err(e.to_string()));
    match held { Ok(dir) => reply(StatusCode::OK, json!({ "ok": true, "pane_id": new_id, "session_id": sid, "title": side_title, "engine": engine, "side_of": parent, "dir": dir })), Err(e) => {
        // a side pane that cannot be held is closed again, or it would refuse every later ask ("already open")
        let _ = engine_call(&state, "system.cockpit.pane.update", json!({ "pane_id": new_id, "status": "closed" }), json!({ "actor": "web-fe", "source": "web-fe" })).await;
        bad(&format!("the side question could not be opened read-only, so it was not opened: {e}")) } }
}

/// Every held side box in this project, for the gate: (pane, box dir).
pub fn held_boxes(boxes: &[crate::work_boxes::BoxRef]) -> Vec<&crate::work_boxes::BoxRef> { boxes.iter().filter(|b| crate::team_guard::role_of(Path::new(&b.dir)).as_deref() == Some("side")).collect() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_side_question_the_screen_lost_is_handed_back_and_a_closed_one_is_not() {
        let pane = |id: i64, status: &str, of: i64| json!({ "id": id, "session_id": 1, "status": status, "pane_kind": "opencode", "settings_json": json!({ "side_of": of, "title": "Side · A" }).to_string() });
        let s = json!({ "panes": [pane(5, "closed", 2), pane(6, "idle", 2), pane(7, "idle", 3)] });
        let back = open_side_of(&s, 2).unwrap(); assert_eq!((back["pane_id"].as_i64(), back["title"].as_str(), back["again"].as_bool()), (Some(6), Some("Side · A"), Some(true)));
        assert!(open_side_of(&s, 9).is_none() && open_side_of(&json!({ "panes": [pane(5, "closed", 2)] }), 2).is_none());
    }
    #[test]
    fn the_helper_is_told_what_it_is_what_to_read_and_that_it_never_writes() {
        let g = guidance("Cart refactor", 7, "/w/box7");
        assert!(g.contains("SIDE-QUESTION helper") && g.contains("pane 7") && g.contains("NEVER change code") && g.contains("side-context.md") && g.contains("antiloki_pane_read") && g.contains("git -C /w/box7 diff") && g.contains("wait for the person's question"), "{g}");
    }
    #[test]
    fn the_context_file_holds_the_last_turns_the_files_and_the_diff_and_cuts_what_is_long() {
        let turns = vec![("add a greeting".to_string(), "done, it says hello".to_string()), ("x".repeat(2000), "y".repeat(2000))];
        let m = context_md("Cart", 7, &turns, &["a.txt".into(), "b/c.rs".into()], "diff --git a/a.txt b/a.txt\n+hello\n");
        assert!(m.contains("\"Cart\" (pane 7)") && m.contains("**asked:** add a greeting") && m.contains("- a.txt") && m.contains("- b/c.rs") && m.contains("```diff") && m.contains("+hello"), "{m}");
        assert!(m.contains(" …") && !m.contains(&"x".repeat(1000)), "a long turn is cut");
        let empty = context_md("Cart", 7, &[], &[], ""); assert!(empty.contains("not been asked anything yet") && empty.contains("none yet") && !empty.contains("```diff"));
    }
}
