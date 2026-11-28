//! TEAMS (operator 2026-09-22, the moat: "name an agent, give it a brief, put it on a routine — the work runs on a
//! schedule, you review the result"; renamed 2026-09-23: "change routines name for teams, because it is teams of
//! agents working on pipelines"). A team is a BRIEF a group of agents gets every N minutes: at its time the ticker
//! opens each member's pane (agents_tab::open_pane) and sends the brief as an ask (agent::agent_send), in the order
//! the operator listed them — from there each is an ordinary turn: Heimdall routes it, the runs carry the result,
//! the roster's status line says "needs a yes on …" when the operator is needed, and the money lands under the face.
//!
//! WHAT IT IS NOT, yet: the members do not hand artefacts to one another. Each is told the team, the brief, and where
//! it sits in the line; stage gates and hand-offs are the meeting design (PLAN-THE-MEETING.md) and are not built. A
//! one-member team is the routine this used to be, unchanged.
//!
//! The wire keeps `agent` (the first member) alongside `members` so an older client and an older row both still work.
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::agent::AgentRegistry;
use crate::dispatch::AppState;

fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }
fn num(v: Option<&Value>) -> i64 { v.and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|t| t.parse().ok()))).unwrap_or(0) }
fn ensure(db: &rusqlite::Connection) {
    let _ = db.execute_batch("CREATE TABLE IF NOT EXISTS agent_routines (id INTEGER PRIMARY KEY AUTOINCREMENT, workspace TEXT NOT NULL DEFAULT '', agent TEXT NOT NULL, name TEXT NOT NULL, brief TEXT NOT NULL, every_min INTEGER NOT NULL DEFAULT 1440, enabled INTEGER NOT NULL DEFAULT 1, last_run INTEGER NOT NULL DEFAULT 0, next_run INTEGER NOT NULL DEFAULT 0, last_note TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL DEFAULT (datetime('now')))");
    // the members of the team, in order, comma-separated. Added 2026-09-23; an existing row has none and falls back
    // to its single `agent`, which is exactly the one-member team it always was.
    let _ = db.execute("ALTER TABLE agent_routines ADD COLUMN members TEXT NOT NULL DEFAULT ''", []);
}

/// The team's members, in order: the stored list when there is one, else the single agent. Pure — tested.
pub fn members_of(members: &str, agent: &str) -> Vec<String> {
    let listed: Vec<String> = members.split(',').map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).collect();
    if listed.is_empty() { if agent.trim().is_empty() { vec![] } else { vec![agent.trim().to_string()] } } else { listed }
}
fn active_root(db: &rusqlite::Connection) -> String { antiloki_rs::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or("").to_string() }

/// When a routine runs next after it ran at `now`: `every_min` minutes on, never less than one. Pure — tested.
pub fn next_after(now_ms: i64, every_min: i64) -> i64 { now_ms + every_min.max(1) * 60_000 }
/// Is it due? Enabled and its time has come (a never-run routine is due at once). Pure — tested.
pub fn is_due(enabled: bool, next_run: i64, now_ms: i64) -> bool { enabled && next_run <= now_ms }

/// `GET cockpit/agents/routines` — this workspace's routines, soonest first.
pub async fn list(state: Arc<AppState>) -> Response {
    let conn = state.pick();
    let rows = tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner()); ensure(&c);
        let root = active_root(&c);
        antiloki_rs::db_json::query_rows(&c, "SELECT id, agent, members, name, brief, every_min, enabled, last_run, next_run, last_note, created_at FROM agent_routines WHERE workspace = ? ORDER BY enabled DESC, next_run ASC", &[json!(root)]).unwrap_or(json!([]))
    }).await.unwrap_or(json!([]));
    Json(json!({ "ok": true, "routines": rows, "now": crate::cli::now_ms() })).into_response()
}
/// `POST cockpit/agents/routines {id?, agent, name, brief, every_min, enabled}` — make or change one.
pub async fn save(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    // `members` is the team, in order; `agent` alone is a one-member team (the old wire, still accepted)
    let listed: Vec<String> = match b.get("members") {
        Some(Value::Array(a)) => a.iter().map(|m| s(Some(m)).trim().to_string()).filter(|m| !m.is_empty()).collect(),
        Some(Value::String(t)) => t.split(',').map(|m| m.trim().to_string()).filter(|m| !m.is_empty()).collect(),
        _ => vec![],
    };
    let mut members = listed;
    let first = s(b.get("agent")).trim().to_string();
    if members.is_empty() && !first.is_empty() { members.push(first); }
    let agent = members.first().cloned().unwrap_or_default();
    let members_csv = members.join(",");
    let (name, brief) = (s(b.get("name")).trim().to_string(), s(b.get("brief")).trim().to_string());
    if agent.is_empty() || brief.is_empty() { return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "a team needs at least one member and a brief" }))).into_response(); }
    let every = num(b.get("every_min")).clamp(1, 60 * 24 * 30); let enabled = b.get("enabled").and_then(Value::as_bool).unwrap_or(true); let id = num(b.get("id"));
    let name = if name.is_empty() { brief.lines().next().unwrap_or("routine").chars().take(60).collect() } else { name };
    let conn = state.pick();
    let out = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner()); ensure(&c);
        let root = active_root(&c); let now = crate::cli::now_ms();
        if id > 0 {
            c.execute("UPDATE agent_routines SET agent=?1, members=?2, name=?3, brief=?4, every_min=?5, enabled=?6 WHERE id=?7 AND workspace=?8", rusqlite::params![agent, members_csv, name, brief, every, enabled as i64, id, root]).map_err(|e| e.to_string())?;
            Ok(json!({ "ok": true, "id": id }))
        } else {
            // a new routine is due at once: the first result is what tells the operator the brief was right
            c.execute("INSERT INTO agent_routines (workspace, agent, members, name, brief, every_min, enabled, next_run) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)", rusqlite::params![root, agent, members_csv, name, brief, every, enabled as i64, now]).map_err(|e| e.to_string())?;
            Ok(json!({ "ok": true, "id": c.last_insert_rowid() }))
        }
    }).await.unwrap_or_else(|e| Err(e.to_string()));
    match out { Ok(v) => Json(v).into_response(), Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response() }
}
/// `POST cockpit/agents/routines/delete {id}`
pub async fn delete(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); let id = num(b.get("id"));
    let conn = state.pick();
    let n = tokio::task::spawn_blocking(move || { let c = conn.lock().unwrap_or_else(|p| p.into_inner()); ensure(&c); c.execute("DELETE FROM agent_routines WHERE id=?1", rusqlite::params![id]).unwrap_or(0) }).await.unwrap_or(0);
    Json(json!({ "ok": n > 0 })).into_response()
}
/// `POST cockpit/agents/routines/run {id}` — now, not at its time.
pub async fn run_now(state: Arc<AppState>, reg: Arc<AgentRegistry>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); let id = num(b.get("id"));
    match fire(&state, &reg, id).await { Ok(note) => Json(json!({ "ok": true, "note": note })).into_response(), Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": e }))).into_response() }
}
/// One routine, fired: the agent's pane (made if needed), the brief sent as an ask, the row stamped.
async fn fire(state: &Arc<AppState>, reg: &Arc<AgentRegistry>, id: i64) -> Result<String, String> {
    let conn = state.pick();
    let row = tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner()); ensure(&c);
        antiloki_rs::db_json::query_rows(&c, "SELECT id, agent, members, name, brief, every_min FROM agent_routines WHERE id = ?", &[json!(id)]).ok().and_then(|r| r.as_array().and_then(|a| a.first()).cloned())
    }).await.unwrap_or(None).ok_or("no such team")?;
    let (brief, every, team) = (s(row.get("brief")), num(row.get("every_min")), s(row.get("name")));
    let members = members_of(&s(row.get("members")), &s(row.get("agent")));
    if members.is_empty() { return Err("the team has no members".into()); }
    let n = members.len();
    let (mut sent, mut failed) = (0usize, Vec::<String>::new());
    for (i, member) in members.iter().enumerate() {
        // every member gets the brief and its place in the line. They do NOT hand artefacts to one another — that is
        // the meeting design, not built; saying so in the prompt is better than letting a member assume an input.
        let line = if n == 1 { String::new() } else { format!(" You are {} of {} on this team; the others work the same brief in parallel and do not hand you anything.", i + 1, n) };
        let text = format!("TEAM “{team}” — this brief runs on a schedule (every {every} min).{line} Do it, then end with a two-line report: what changed, and what needs a yes from the operator (or “nothing”).\n\n{brief}");
        match crate::agents_tab::open_pane(state, member).await {
            Err((_, e)) => failed.push(format!("{member}: {e}")),
            Ok((opened, _)) => {
                let pane_id = num(opened.get("pane").and_then(|p| p.get("id")));
                if pane_id <= 0 { failed.push(format!("{member}: no pane")); continue; }
                let res = crate::agent::agent_send(state.clone(), reg.clone(), Bytes::from(serde_json::to_vec(&json!({ "pane_id": pane_id, "text": text })).unwrap_or_default())).await;
                if res.status().is_success() { sent += 1; } else { failed.push(format!("{member}: send failed ({})", res.status())); }
            }
        }
    }
    let ok = sent > 0;
    let now = crate::cli::now_ms();
    let note = if failed.is_empty() { format!("sent to {sent} of {n}") } else { format!("sent to {sent} of {n} — {}", failed.join("; ")) };
    let conn = state.pick(); let note2 = note.clone();
    let _ = tokio::task::spawn_blocking(move || { let c = conn.lock().unwrap_or_else(|p| p.into_inner()); let _ = c.execute("UPDATE agent_routines SET last_run=?1, next_run=?2, last_note=?3 WHERE id=?4", rusqlite::params![now, next_after(now, every), note2, id]); }).await;
    if ok { Ok(note) } else { Err(note) }
}
/// THE TICKER: every minute, every due routine of every workspace fires. Started once at boot.
pub fn start(state: Arc<AppState>, reg: Arc<AgentRegistry>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let conn = state.pick();
            let due: Vec<i64> = tokio::task::spawn_blocking(move || {
                let c = conn.lock().unwrap_or_else(|p| p.into_inner()); ensure(&c);
                let now = crate::cli::now_ms();
                antiloki_rs::db_json::query_rows(&c, "SELECT id, enabled, next_run FROM agent_routines", &[]).ok().and_then(|r| r.as_array().cloned()).unwrap_or_default().iter()
                    .filter(|r| is_due(num(r.get("enabled")) != 0, num(r.get("next_run")), now)).map(|r| num(r.get("id"))).collect()
            }).await.unwrap_or_default();
            for id in due { if let Err(e) = fire(&state, &reg, id).await { eprintln!("routine {id}: {e}"); } }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_team_is_its_listed_members_in_order_and_falls_back_to_the_single_agent() {
        assert_eq!(members_of("po,backender,tester", ""), vec!["po", "backender", "tester"], "in the order written");
        assert_eq!(members_of(" po , tester ", ""), vec!["po", "tester"], "spaces are not members");
        assert_eq!(members_of("", "scout"), vec!["scout"], "an old row is a one-member team");
        assert_eq!(members_of(",,", "scout"), vec!["scout"], "nor are empty slots");
        assert!(members_of("", "").is_empty(), "nothing at all is nothing");
        assert_eq!(members_of("a,b", "z"), vec!["a", "b"], "the list wins over the legacy column");
    }

    #[test]
    fn a_routine_is_due_when_enabled_and_its_time_has_come_and_runs_again_every_n_minutes() {
        assert!(is_due(true, 0, 1_000), "never run: due at once");
        assert!(!is_due(false, 0, 1_000), "disabled: never");
        assert!(!is_due(true, 2_000, 1_000)); assert!(is_due(true, 1_000, 1_000));
        assert_eq!(next_after(1_000, 30), 1_000 + 30 * 60_000);
        assert_eq!(next_after(1_000, 0), 1_000 + 60_000, "never less than a minute");
    }
}
