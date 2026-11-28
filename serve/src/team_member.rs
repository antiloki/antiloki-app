//! WHAT AN AGENT BELONGS TO (the operator 2026-11-15: "the agents' sagas should show if the agent is part of a pair, a team, a thread, anything"): for one agent's pane, every group it is a member of: the thread it works in, the pair or team it is linked into (and its role there), and the team whose Leader it is.
//! Read from where those facts already live (the threads' layouts, the teams file, a team's leader file), so the saga of an agent can say it without a second copy that could go stale. Blocking.
use serde_json::{json, Value};
use std::path::Path;

/// What `member_of` reads, loaded once per list (a list asks about many panes; the threads' layouts are files, one per thread).
pub struct MemberCtx { threads: Vec<Value>, teams: Vec<crate::team::Team>, leaders: Vec<(String, i64)> }
impl MemberCtx {
    pub fn load(root: &str) -> Self {
        let threads = crate::cowork_threads::view(root)["threads"].as_array().cloned().unwrap_or_default(); let teams = crate::team_run::load_teams(root);
        let dir = Path::new(root).join(".antiloki").join("team"); let mut leaders = vec![];
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() { let p = e.path(); if !p.is_dir() { continue; } if let Some(pane) = std::fs::read_to_string(p.join("leader.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["pane"].as_i64()) { leaders.push((e.file_name().to_string_lossy().to_string(), pane)); } }
        MemberCtx { threads, teams, leaders }
    }
    pub fn of(&self, pane_id: i64) -> Vec<Value> {
        let mut out: Vec<Value> = vec![];
        for t in &self.threads { let has = t["panes"].as_array().into_iter().flatten().any(|p| p.as_i64() == Some(pane_id) || p["paneId"].as_i64() == Some(pane_id) || p["pane_id"].as_i64() == Some(pane_id));
            if has { out.push(json!({ "kind": "thread", "name": t["name"], "id": t["id"] })); if let Some(team) = t["team"].as_str().filter(|x| !x.is_empty()) { out.push(json!({ "kind": "team-thread", "name": team })); } } }
        for tm in &self.teams { if let Some((role, _)) = tm.members().into_iter().find(|(_, p)| *p == pane_id) { out.push(json!({ "kind": if tm.pair { "pair" } else { "team" }, "name": tm.name, "role": role.key() })); } }
        for (team, pane) in &self.leaders { if *pane == pane_id { out.push(json!({ "kind": "leader", "name": team, "role": "leader" })); } }
        out
    }
}
pub fn member_of(root: &str, pane_id: i64) -> Vec<Value> { MemberCtx::load(root).of(pane_id) }
