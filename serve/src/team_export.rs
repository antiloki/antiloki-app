//! EXPORT AND IMPORT A TEAM (RFC 0013 §9, step 9): a team is its recipe, so it can travel as one file. The file holds the recipe, TEAM.md and the replay tasks the person picks; never runs, sagas, costs,
//! git history, requests, keys, tokens or machine paths. Import unpacks it only after the person has seen what it asks for; triggers arrive switched off; the team starts at v1 "not measured here".
use crate::team_graph::{Graph, Kind};
use serde_json::{json, Value};

pub const FORMAT: &str = "antiloki-team/1";

pub fn export(root: &str, team: &str, task_ids: &[String]) -> Result<String, String> {
    let g = crate::team_graph_run::load_graph(root, team)?; let recipe = crate::team_recipe::to_recipe(&g)?;
    let notes = std::fs::read_to_string(crate::team_repo::repo_dir(root, team).join("TEAM.md")).unwrap_or_else(|_| crate::team_notes::team_md(&g, ""));
    for (what, text) in [("recipe", &recipe), ("TEAM.md", &notes)] { if text.contains(root) || text.contains("/home/") || text.contains(":\\Users\\") { return Err(format!("the {what} names a path on this machine: take it out before exporting")); } }
    let tasks: Vec<Value> = crate::team_replay::list(root, team).into_iter().filter(|t| task_ids.contains(&t.id)).map(|t| json!({ "ask": t.ask, "checks": t.checks, "oracle": t.oracle })).collect();
    Ok(serde_json::to_string_pretty(&json!({ "format": FORMAT, "team": g.team, "recipe": recipe, "team_md": notes, "tasks": tasks })).unwrap_or_default())
}

fn parse(text: &str) -> Result<(Value, Graph), String> {
    let v: Value = serde_json::from_str(text).map_err(|_| "this is not a team file".to_string())?;
    if v["format"] != json!(FORMAT) { return Err("this file is not a team this version knows (format antiloki-team/1)".into()); }
    let g = crate::team_recipe::from_recipe(v["recipe"].as_str().ok_or("the file holds no recipe")?)?; Ok((v, g))
}
/// What importing would ask the person to allow, in words, before anything is written.
pub fn review(text: &str) -> Result<Value, String> {
    let (v, g) = parse(text)?; let rep = crate::team_graph::validate(&g); let mut asks: Vec<String> = vec![];
    for (id, n) in &g.nodes { match &n.kind {
        Kind::Role { agent: Some(a), .. } => for e in &a.effects { if ["write", "outward", "money", "web"].contains(&e.as_str()) { asks.push(format!("`{id}` may {e}")); } },
        Kind::UseTool { tool, .. } => asks.push(format!("`{id}` uses a tool: {}", serde_json::to_string(tool).unwrap_or_default())),
        Kind::Code { .. } => asks.push(format!("`{id}` runs code in the sandbox")), Kind::SubTeam { team, .. } => asks.push(format!("`{id}` runs another team: {team}")), _ => {} } }
    Ok(json!({ "ok": true, "team": g.team, "nodes": g.nodes.len(), "budget_usd": g.budget_usd, "asks": asks, "triggers_switched_off": g.triggers.len(), "tasks": v["tasks"].as_array().map(|a| a.len()).unwrap_or(0), "valid": rep.ok(), "problems": rep.words(),
        "note": "it arrives as version 1, not measured here; its triggers are switched off; no keys or history come with it" }))
}
/// Import after the person's yes (`confirmed`). A name already taken is refused, never overwritten.
pub fn import(root: &str, text: &str, name: &str, confirmed: bool, now_ms: i64) -> Result<Value, String> {
    if !confirmed { return Err("read the review first: importing needs your yes".into()); }
    let (v, mut g) = parse(text)?; if !name.trim().is_empty() { g.team = name.trim().to_string(); }
    if crate::team_graph_run::load_graph(root, &g.team).is_ok() { return Err(format!("a team called {} already exists: pick another name", g.team)); }
    let rep = crate::team_graph::validate(&g); if !rep.ok() { return Err(format!("the team is not valid here: {}", rep.words().join("; "))); }
    g.triggers.clear(); g.limits = None;
    crate::team_repo::commit_graph(root, &g, "imported (v1, not measured here)")?; crate::team_graph_run::save_graph(root, &g)?;
    let d = crate::team_repo::repo_dir(root, &g.team); let _ = std::fs::write(d.parent().unwrap().join("imported.json"), json!({ "from": v["team"], "version": 1, "measured_here": false, "t_ms": now_ms }).to_string());
    let mut added = 0; for t in v["tasks"].as_array().cloned().unwrap_or_default() { if let (Some(ask), Ok(checks)) = (t["ask"].as_str(), serde_json::from_value(t["checks"].clone())) { if crate::team_replay::add(root, &g.team, ask, checks, "", now_ms).is_ok() { added += 1; } } }
    Ok(json!({ "ok": true, "team": g.team, "tasks_added": added }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(n: &str) -> String { let p = std::env::temp_dir().join(format!("antiloki-exp-{n}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&p); std::fs::create_dir_all(&p).unwrap(); p.to_string_lossy().to_string() }
    #[test] fn a_team_travels_as_structure_only_and_arrives_switched_off_unmeasured_and_never_over_another() {
        let a = tmp("a"); let b = tmp("b"); let g = crate::team_graph::template("blank", "Demo", 2.0).unwrap(); crate::team_graph_run::save_graph(&a, &g).unwrap();
        let t = crate::team_replay::add(&a, "Demo", "make a thing", vec![crate::team_check::Rule { id: "w".into(), kind: "words".into(), max: Some(9.0), ..Default::default() }], "run-secret-1", 1).unwrap();
        let file = export(&a, "Demo", &[t.id.clone()]).unwrap();
        assert!(!file.contains("run-secret-1") && !file.contains(&a), "no run, no machine path travels");
        let r = review(&file).unwrap(); assert_eq!(r["team"], json!("Demo")); assert_eq!(r["tasks"], json!(1));
        assert!(import(&b, &file, "", false, 5).is_err(), "no yes, no import");
        let out = import(&b, &file, "Copy", true, 5).unwrap(); assert_eq!(out["tasks_added"], json!(1));
        let got = crate::team_graph_run::load_graph(&b, "Copy").unwrap(); assert!(got.triggers.is_empty());
        assert!(crate::team_repo::history(&b, "Copy")[0]["message"].as_str().unwrap().contains("not measured here"));
        assert!(import(&b, &file, "Copy", true, 6).is_err(), "never over a team that is there");
        assert!(review("{}").is_err() && review("nope").is_err());
        assert_eq!(crate::team_request::benchmark(&b, "Copy")["imported_not_measured_here"], json!(true));
    }
}
