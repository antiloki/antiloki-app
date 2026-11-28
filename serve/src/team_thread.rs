//! THREADS OF A TEAM (operator 2026-11-16): the same team forked, with a recipe of its own (the same steps on Cursor instead of Claude, say). A thread is a branch `thread/<name>` of the team's repository.
//! It runs as a team of its own (`<team>--t-<name>`, graph file kept) so its runs, costs and benchmark stay apart from `main`'s and can be compared on the same replay bank.
use crate::team_graph::{Graph, Kind};
use serde_json::{json, Value};

const PREFIX: &str = "thread/";
pub fn name_ok(n: &str) -> bool { !n.is_empty() && n.len() <= 40 && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !n.starts_with('-') }
/// The team a thread's runs belong to.
pub fn run_team(team: &str, name: &str) -> String { format!("{team}--t-{name}") }

/// The same graph on another engine: every agent step is bound to it (the model is left to the engine's own default for the tier: model names do not travel between engines) and the triggers use it.
/// Typed model calls (`llm` seats) are left alone and counted, so the person knows what did not move.
pub fn with_engine(g: &Graph, engine: &str) -> Result<(Graph, usize), String> {
    let e = crate::cli::engine_of(&json!(engine)).ok_or_else(|| format!("no engine called {engine}"))?;
    let mut c = g.clone(); let mut left = 0;
    for n in c.nodes.values_mut() { match &mut n.kind { Kind::Role { model, .. } => *model = Some(format!("{}:", e.key())), Kind::Llm(_) => left += 1, _ => {} } }
    for t in c.triggers.iter_mut() { t.engine = e.key().to_string(); }
    Ok((c, left))
}
/// Fork `main` into a thread, on another engine when one is named. Returns the thread and how many typed model calls stayed as they were.
pub fn fork(root: &str, team: &str, name: &str, engine: Option<&str>) -> Result<Value, String> {
    if !name_ok(name) { return Err("a thread name is lowercase letters, digits and dashes (up to 40)".into()); }
    let branch = format!("{PREFIX}{name}");
    if crate::team_repo::branches(root, team, PREFIX).iter().any(|(b, _)| *b == branch) { return Err(format!("the team already has a thread called {name}")); }
    // a team just made has no saved version yet (the save is in the background): the live graph becomes its first one
    let live = match crate::team_repo::recipe_on(root, team, "main") { Ok(g) => g, Err(_) => { let g = crate::team_graph_run::load_graph(root, team)?; crate::team_repo::commit_graph(root, &g, "first version")?; crate::team_repo::recipe_on(root, team, "main")? } };
    let (g, left) = match engine.filter(|e| !e.is_empty()) { Some(e) => with_engine(&live, e)?, None => (live, 0) };
    let id = crate::team_repo::open_branch(root, &g, &branch, &format!("thread {name}{}", engine.map(|e| format!(": same team on {e}")).unwrap_or_default()))?;
    Ok(json!({ "name": name, "head": id, "engine": engine.unwrap_or(""), "llm_left": left }))
}
/// The threads of a team, each with the engine its agent steps are bound to (empty = unchanged).
pub fn list(root: &str, team: &str) -> Vec<Value> {
    crate::team_repo::branches(root, team, PREFIX).into_iter().map(|(b, head)| {
        let name = b.trim_start_matches(PREFIX).to_string();
        let engine = crate::team_repo::recipe_on(root, team, &b).ok().and_then(|g| g.nodes.values().find_map(|n| match &n.kind { Kind::Role { model: Some(m), .. } => m.split_once(':').map(|(e, _)| e.to_string()), _ => None })).unwrap_or_default();
        json!({ "name": name, "head": head, "engine": engine, "team": run_team(team, &name) })
    }).collect()
}
/// Write the thread's graph as the team it runs as, so a run can start on it. The graph is the thread's own; `main` is not touched.
pub fn materialize(root: &str, team: &str, name: &str) -> Result<String, String> {
    let mut g = crate::team_repo::recipe_on(root, team, &format!("{PREFIX}{name}")).map_err(|_| format!("no thread called {name}"))?;
    let t = run_team(team, name); g.team = t.clone(); crate::team_graph_run::save_graph(root, &g)?; Ok(t)
}
/// Save a changed graph on the thread as a new version.
pub fn save(root: &str, team: &str, name: &str, g: &Graph, message: &str) -> Result<Option<String>, String> {
    let mut c = g.clone(); c.team = team.to_string(); crate::team_repo::commit_on(root, &c, &format!("{PREFIX}{name}"), message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_graph::{template, Node, RoleKind};
    fn tmp(n: &str) -> String { let p = std::env::temp_dir().join(format!("antiloki-thread-{n}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&p); std::fs::create_dir_all(&p).unwrap(); p.to_string_lossy().to_string() }
    #[test] fn a_thread_is_the_same_team_on_another_engine_with_its_own_versions_and_main_is_untouched() {
        let root = tmp("a"); let mut g = template("blank", "Demo", 2.0).unwrap();
        g.nodes.insert("w".into(), Node::role(RoleKind::Worker));
        crate::team_repo::commit_graph(&root, &g, "first").unwrap();
        let t = fork(&root, "Demo", "cursor", Some("cursor")).unwrap(); assert_eq!(t["engine"], json!("cursor"));
        assert!(fork(&root, "Demo", "cursor", None).is_err(), "a name is used once"); assert!(fork(&root, "Demo", "Bad Name", None).is_err()); assert!(fork(&root, "Demo", "x", Some("nope")).is_err());
        let l = list(&root, "Demo"); assert_eq!(l.len(), 1); assert_eq!(l[0]["engine"], json!("cursor")); assert_eq!(l[0]["team"], json!("Demo--t-cursor"));
        let main = crate::team_repo::recipe_on(&root, "Demo", "main").unwrap();
        assert!(matches!(&main.nodes["w"].kind, Kind::Role { model: None, .. }), "main keeps its own engine");
        let th = crate::team_repo::recipe_on(&root, "Demo", "thread/cursor").unwrap();
        assert!(matches!(&th.nodes["w"].kind, Kind::Role { model: Some(m), .. } if m == "cursor:"), "the thread binds the step to cursor");
        let mut g2 = th.clone(); g2.budget_usd = 9.0; assert!(save(&root, "Demo", "cursor", &g2, "more").unwrap().is_some());
        assert_eq!(crate::team_repo::recipe_on(&root, "Demo", "main").unwrap().budget_usd, 2.0, "main did not move");
        assert_eq!(crate::team_repo::recipe_on(&root, "Demo", "thread/cursor").unwrap().budget_usd, 9.0);
        let run = materialize(&root, "Demo", "cursor").unwrap(); assert_eq!(run, "Demo--t-cursor");
        assert_eq!(crate::team_graph_run::load_graph(&root, &run).unwrap().budget_usd, 9.0);
    }
    #[test] fn a_team_with_no_saved_version_yet_can_still_be_forked() {
        let root = tmp("b"); let g = template("blank", "Fresh", 1.0).unwrap(); crate::team_graph_run::save_graph(&root, &g).unwrap();
        assert!(crate::team_repo::recipe_on(&root, "Fresh", "main").is_err(), "no version yet");
        assert_eq!(fork(&root, "Fresh", "codex", Some("codex")).unwrap()["engine"], json!("codex")); assert_eq!(list(&root, "Fresh").len(), 1);
    }
}
