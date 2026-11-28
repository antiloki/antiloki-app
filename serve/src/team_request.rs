//! IMPROVEMENT REQUESTS (RFC 0013 §3, §5, steps 6-8): a request is a branch `improve/<n>` of the team's own git holding ONE small change to the recipe, plus a `REQUEST.md` that tells the person
//! the problem, the change in plain words, the experiment's numbers and the risk class. The person approves (the branch becomes `main`, the team is rebuilt), refuses (the branch is kept as `refused/<n>`
//! with the reason, which the next proposal reads) or asks for another try. The judge (checks, replay bank, scorer) is never on the branch, so a request cannot change it.
use crate::team_graph::{Graph, Kind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MAX_CHANGES: usize = 3;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Change { pub node: String, pub what: String, /** low | medium | high | forbidden */ pub risk: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub n: u32, /** open | approved | refused | retry */ pub status: String, pub branch: String, pub problem: String, pub run_ids: Vec<String>, pub changes: Vec<Change>, pub risk: String,
    #[serde(default)] pub experiment: Option<Value>, #[serde(default)] pub reason: String, pub opened_ms: i64, #[serde(default)] pub decided_ms: i64, #[serde(default)] pub decided_by: String,
}

pub fn dir(root: &str, team: &str) -> PathBuf { Path::new(root).join(".antiloki").join("team").join(crate::cowork_threads::slug(team)).join("requests") }
fn meta_path(root: &str, team: &str, n: u32) -> PathBuf { dir(root, team).join(format!("{n}.json")) }
pub fn list(root: &str, team: &str) -> Vec<Meta> {
    let mut v: Vec<Meta> = std::fs::read_dir(dir(root, team)).into_iter().flatten().flatten().filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false)).filter_map(|e| std::fs::read_to_string(e.path()).ok().and_then(|t| serde_json::from_str(&t).ok())).collect();
    v.sort_by_key(|m| std::cmp::Reverse(m.n)); v
}
pub fn get(root: &str, team: &str, n: u32) -> Option<Meta> { std::fs::read_to_string(meta_path(root, team, n)).ok().and_then(|t| serde_json::from_str(&t).ok()) }
fn put(root: &str, team: &str, m: &Meta) -> Result<(), String> { let d = dir(root, team); std::fs::create_dir_all(&d).map_err(|e| e.to_string())?; std::fs::write(meta_path(root, team, m.n), serde_json::to_string_pretty(m).unwrap_or_default()).map_err(|e| e.to_string()) }

// ── what changed, in words, and how risky ────────────────────────────────────────────────────────────────────────────────

fn flat(v: &Value, prefix: &str, out: &mut BTreeMap<String, Value>, depth: u8) {
    match v { Value::Object(m) if depth < 3 => for (k, x) in m { flat(x, &if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") }, out, depth + 1); }, _ => { out.insert(prefix.into(), v.clone()); } }
}
fn risk_of_path(p: &str) -> &'static str {
    let last = p.rsplit('.').next().unwrap_or(p);
    if p == "pos" || last == "pos" { return "ignore"; }
    if ["cap_usd", "approval", "autonomy", "consent"].contains(&last) || p.contains("budget") { return "forbidden"; }
    if ["effects", "owns", "skills", "mcp", "allow", "tool", "readonly", "starts_model", "paths", "panes"].contains(&last) { return "high"; }
    if ["brief", "responsibility", "title", "label", "why", "context", "note"].contains(&last) || p.starts_with("why.") { return "low"; }
    "medium"
}
fn is_judge(k: &Kind) -> bool { matches!(k, Kind::Check { .. } | Kind::Test(_) | Kind::Contract(_) | Kind::Review(_) | Kind::Human { .. } | Kind::BudgetGate { .. } | Kind::Stop { .. }) }
fn is_power(k: &Kind) -> bool { matches!(k, Kind::UseTool { .. } | Kind::Code { .. } | Kind::SubTeam { .. }) || matches!(k, Kind::Role { agent: Some(a), .. } if a.has("write") || a.has("outward") || a.has("money") || a.has("web")) }

/// The differences between the live graph and a candidate, each with its risk. `forbidden` = never an improver's to change (spend limits, gates, the judge, approvals, triggers, setpoints).
pub fn diff(a: &Graph, b: &Graph) -> Vec<Change> {
    let mut out = vec![]; let c = |node: &str, what: String, risk: &str| Change { node: node.into(), what, risk: risk.into() };
    if a.budget_usd != b.budget_usd { out.push(c("", format!("the budget per run changes from ${:.2} to ${:.2}", a.budget_usd, b.budget_usd), "forbidden")); }
    if a.triggers != b.triggers || a.limits != b.limits { out.push(c("", "when the team starts on its own changes".into(), "forbidden")); }
    if a.setpoints != b.setpoints { out.push(c("", "what each node is expected to hold changes".into(), "forbidden")); }
    for (id, n) in &b.nodes { if !a.nodes.contains_key(id) { let r = if is_judge(&n.kind) { "forbidden" } else if is_power(&n.kind) { "high" } else { "medium" }; out.push(c(id, format!("adds `{id}`"), r)); } }
    for (id, n) in &a.nodes { if !b.nodes.contains_key(id) { let r = if is_judge(&n.kind) { "forbidden" } else if is_power(&n.kind) { "high" } else { "medium" }; out.push(c(id, format!("removes `{id}`"), r)); } }
    for (id, nb) in &b.nodes { if let Some(na) = a.nodes.get(id) {
        let (mut fa, mut fb) = (BTreeMap::new(), BTreeMap::new()); flat(&serde_json::to_value(na).unwrap_or_default(), "", &mut fa, 0); flat(&serde_json::to_value(nb).unwrap_or_default(), "", &mut fb, 0);
        let keys: std::collections::BTreeSet<&String> = fa.keys().chain(fb.keys()).collect();
        for k in keys { if fa.get(k) != fb.get(k) { let mut r = risk_of_path(k); if r == "ignore" { continue; } if is_judge(&na.kind) && r != "low" { r = "forbidden"; }
            out.push(c(id, format!("`{id}`: {} changes", k.replace('_', " ")), r)); } }
    } }
    if a.edges != b.edges { out.push(c("", "the order the steps run in changes".into(), "medium")); }
    out
}
/// The changes a request makes OUTSIDE the section it was asked about (the evolve node's `scope`): a change to a node not in it, or to the graph as a whole. Empty = all inside. Pure.
pub fn out_of_scope(changes: &[Change], scope: &[String]) -> Vec<String> { changes.iter().filter(|c| c.node.is_empty() || !scope.iter().any(|s| s == &c.node)).map(|c| c.what.clone()).collect() }
pub fn risk_class(changes: &[Change]) -> String { ["forbidden", "high", "medium", "low"].iter().find(|r| changes.iter().any(|c| &c.risk == *r)).unwrap_or(&"low").to_string() }

// ── open, experiment, decide ─────────────────────────────────────────────────────────────────────────────────────────────

/// Open a request: the candidate graph on a branch, ONE small change, nothing forbidden. `live` is the team as it runs now.
pub fn open(root: &str, live: &Graph, candidate: &Graph, problem: &str, run_ids: &[String], now_ms: i64) -> Result<Meta, String> {
    if problem.trim().is_empty() { return Err("a request names the problem it is about, with the runs that show it".into()); }
    let team = &live.team; let changes = diff(live, candidate);
    if changes.is_empty() { return Err("the candidate is the same as the team: nothing to ask".into()); }
    if let Some(f) = changes.iter().find(|c| c.risk == "forbidden") { return Err(format!("an improver may not change this ({}): spend limits, gates, checks, approvals and triggers are the person's", f.what)); }
    if changes.len() > MAX_CHANGES { return Err(format!("{} changes in one request: one idea per request, so its effect can be told apart (at most {MAX_CHANGES})", changes.len())); }
    // only what the change itself breaks stops it: a team that already had problems is not blocked from being improved
    let before = crate::team_graph::validate(live).words(); let broken: Vec<String> = crate::team_graph::validate(candidate).words().into_iter().filter(|w| !before.contains(w)).collect();
    if !broken.is_empty() && !crate::team_graph::validate(candidate).ok() { return Err(format!("the candidate team is not valid: {}", broken.join("; "))); }
    crate::team_repo::commit_graph(root, live, "save")?;
    let n = list(root, team).first().map(|m| m.n).unwrap_or(0) + 1; let branch = format!("improve/{n}");
    crate::team_repo::open_branch(root, candidate, &branch, &format!("request {n}: {}", problem.lines().next().unwrap_or("")))?;
    let m = Meta { n, status: "open".into(), branch, problem: problem.trim().into(), run_ids: run_ids.to_vec(), risk: risk_class(&changes), changes, experiment: None, reason: String::new(), opened_ms: now_ms, decided_ms: 0, decided_by: String::new() };
    put(root, team, &m)?; write_page(root, team, &m); Ok(m)
}
/// Attach the experiment's numbers (a `team_experiment::Report` as JSON) and rewrite the page.
pub fn attach_experiment(root: &str, team: &str, n: u32, report: Value) -> Result<Meta, String> {
    let mut m = get(root, team, n).ok_or("no such request")?; if m.status != "open" && m.status != "retry" { return Err("this request is already decided".into()); }
    m.experiment = Some(report); put(root, team, &m)?; write_page(root, team, &m); Ok(m)
}
/// What earlier approved changes have already used of the regression allowance (a share of held-out tasks), and what is left.
pub fn regression_left(root: &str, team: &str) -> f64 {
    let used: f64 = list(root, team).iter().filter(|m| m.status == "approved").filter_map(|m| m.experiment.as_ref()?.get("regression_share")?.as_f64()).sum();
    (crate::team_experiment::DEFAULT_REGRESSION_BUDGET - used).max(0.0)
}
/// The person's decision. `approve`: the branch becomes main and the live team is rebuilt from it. `refuse`: the branch is kept as `refused/<n>`, with the reason. `retry`: asks for another try.
pub fn decide(root: &str, team: &str, n: u32, action: &str, reason: &str, by: &str, by_agent: bool, now_ms: i64) -> Result<Meta, String> {
    if by_agent { return Err("only the person decides a request".into()); }
    let mut m = get(root, team, n).ok_or("no such request")?; if m.status != "open" && m.status != "retry" { return Err(format!("this request is already {}", m.status)); }
    match action {
        "approve" => { let g = crate::team_repo::recipe_on(root, team, &m.branch)?; crate::team_repo::merge_branch(root, team, &m.branch)?; crate::team_graph_run::save_graph(root, &g)?; m.status = "approved".into();
            let head = crate::team_repo::head(root, team); if let Some(e) = &m.experiment { record_version(root, team, &head, e, now_ms); } }
        "refuse" => { if reason.trim().is_empty() { return Err("say why: the reason is what the next proposal learns from".into()); } crate::team_repo::rename_branch(root, team, &m.branch, &format!("refused/{n}"))?; m.branch = format!("refused/{n}"); m.status = "refused".into(); }
        "retry" => { m.status = "retry".into(); }
        _ => return Err("approve, refuse or retry".into()),
    }
    m.reason = reason.trim().into(); m.decided_by = by.into(); m.decided_ms = now_ms; put(root, team, &m)?; write_page(root, team, &m); Ok(m)
}
/// STEP 10: the person's standing yes for the lowest risk class. Off unless they turn it on; never for medium or high; only when the experiment admitted the change.
pub fn auto_merge_on(root: &str, team: &str) -> bool { std::fs::read_to_string(dir(root, team).join("config.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["auto_merge_low"].as_bool()).unwrap_or(false) }
pub fn set_auto_merge(root: &str, team: &str, on: bool) -> Result<(), String> { let d = dir(root, team); std::fs::create_dir_all(&d).map_err(|e| e.to_string())?; std::fs::write(d.join("config.json"), json!({ "auto_merge_low": on }).to_string()).map_err(|e| e.to_string()) }
/// After an experiment: merge on the person's standing yes when it is a low-risk, admitted change. Returns the decided request, or None when it waits for the person.
pub fn maybe_auto_merge(root: &str, team: &str, n: u32, now_ms: i64) -> Option<Meta> {
    let m = get(root, team, n)?; if !auto_merge_on(root, team) || m.risk != "low" || !m.experiment.as_ref().and_then(|e| e["admitted"].as_bool()).unwrap_or(false) { return None; }
    decide(root, team, n, "approve", "merged on your standing yes for low-risk, admitted changes", "standing yes", false, now_ms).ok()
}
/// The reasons the person gave for refusing, newest first: the next proposal is told them.
pub fn refusals(root: &str, team: &str) -> Vec<String> { list(root, team).into_iter().filter(|m| m.status == "refused").map(|m| format!("request {} ({}): {}", m.n, m.changes.first().map(|c| c.what.clone()).unwrap_or_default(), m.reason)).collect() }

// ── the page and the benchmark tab ───────────────────────────────────────────────────────────────────────────────────────

fn pct(x: f64) -> String { format!("{:.0}%", x * 100.0) }
pub fn page(m: &Meta) -> String {
    let icon = match m.risk.as_str() { "low" => "●", "medium" => "◆", _ => "▲" };
    let mut s = format!("# Request {} · {} risk {}\n\n**Problem.** {}\n", m.n, m.risk, icon, m.problem.lines().next().unwrap_or(""));
    if !m.run_ids.is_empty() { s += &format!("Shown by: {}\n", m.run_ids.join(", ")); }
    s += "\n## The change\n"; for c in &m.changes { s += &format!("- {} ({})\n", c.what, c.risk); }
    s += "\n## Current against proposed\n";
    match &m.experiment { None => s += "_No experiment yet. Nothing here says the change helps._\n", Some(e) => {
        let g = |arm: &str, k: &str| e[arm][k].as_f64().unwrap_or(0.0);
        s += "| | passes | lower bound | cost per run | time per run | gate stops |\n|---|---|---|---|---|---|\n";
        for (label, arm) in [("current", "current"), ("proposed", "candidate")] { s += &format!("| {label} | {} | {} | ${:.3} | {:.1}s | {} |\n", pct(g(arm, "rate")), pct(g(arm, "lower")), g(arm, "avg_cost_usd"), g(arm, "avg_wall_ms") / 1000.0, e[arm]["gate_stops"].as_u64().unwrap_or(0)); }
        let rg = e["regressions"].as_array().map(|a| a.len()).unwrap_or(0);
        s += &format!("\n{} · {} regression(s) · spent ${:.2}\n", e["why"].as_str().unwrap_or(""), rg, e["spent_usd"].as_f64().unwrap_or(0.0));
    } }
    s += &format!("\n## Status\n{}{}\n", m.status, if m.reason.is_empty() { String::new() } else { format!(" — {}", m.reason) });
    s
}
fn write_page(root: &str, team: &str, m: &Meta) { let d = dir(root, team).join(m.n.to_string()); let _ = std::fs::create_dir_all(&d); let _ = std::fs::write(d.join("REQUEST.md"), page(m)); }

fn measured_path(root: &str, team: &str) -> PathBuf { dir(root, team).join("measured.json") }
fn measured(root: &str, team: &str) -> BTreeMap<String, Value> { std::fs::read_to_string(measured_path(root, team)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default() }
/// What a version scored (the experiment that admitted it), kept by its commit id.
pub fn record_version(root: &str, team: &str, id: &str, report: &Value, now_ms: i64) {
    if id.is_empty() { return; } let mut m = measured(root, team); m.insert(id.into(), json!({ "candidate": report["candidate"], "n": report["candidate"]["n"], "last_run_ms": now_ms })); let _ = std::fs::create_dir_all(dir(root, team)); let _ = std::fs::write(measured_path(root, team), serde_json::to_string_pretty(&m).unwrap_or_default());
}
/// The Benchmark tab: per version its pass rate, lower bound, cost, time and last measured, and the model of each node (so a change of model reads down a column).
pub fn benchmark(root: &str, team: &str) -> Value {
    let m = measured(root, team); let imported = std::fs::read_to_string(crate::team_repo::repo_dir(root, team).join("..").join("imported.json")).is_ok();
    let rows: Vec<Value> = crate::team_repo::history(root, team).into_iter().take(30).map(|v| {
        let id = v["id"].as_str().unwrap_or("").to_string(); let models: BTreeMap<String, String> = crate::team_repo::recipe_at(root, team, &id).map(|g| g.nodes.iter().filter_map(|(k, n)| match &n.kind { Kind::Role { model: Some(x), .. } => Some((k.clone(), x.clone())), Kind::Llm(s) => Some((k.clone(), s.seat.clone())), _ => None }).collect()).unwrap_or_default();
        let sc = m.get(&id); json!({ "id": id, "n": v["n"], "message": v["message"], "ts": v["ts"], "models": models,
            "measured": sc.is_some(), "pass_rate": sc.map(|s| s["candidate"]["rate"].clone()).unwrap_or(Value::Null), "lower": sc.map(|s| s["candidate"]["lower"].clone()).unwrap_or(Value::Null),
            "avg_cost_usd": sc.map(|s| s["candidate"]["avg_cost_usd"].clone()).unwrap_or(Value::Null), "avg_wall_ms": sc.map(|s| s["candidate"]["avg_wall_ms"].clone()).unwrap_or(Value::Null), "last_run_ms": sc.map(|s| s["last_run_ms"].clone()).unwrap_or(Value::Null) })
    }).collect();
    json!({ "versions": rows, "imported_not_measured_here": imported && m.is_empty(), "regression_left": regression_left(root, team) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_graph::template;
    fn tmp(n: &str) -> String { let p = std::env::temp_dir().join(format!("antiloki-req-{n}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&p); std::fs::create_dir_all(&p).unwrap(); p.to_string_lossy().to_string() }
    fn with_agent(name: &str) -> (String, Graph) {
        let root = tmp(name); let mut g = template("blank", "Demo", 2.0).unwrap(); g.nodes.insert("writer".into(), { let mut n = crate::team_graph::Node::role(crate::team_graph::RoleKind::Agent); if let Kind::Role { agent, .. } = &mut n.kind { *agent = Some(crate::team_graph::AgentSpec { title: "Writer".into(), responsibility: "write the posts".into(), brief: "in our voice".into(), effects: vec!["read".into()], ..Default::default() }); } n.cap_usd = Some(0.5); n });
        std::fs::create_dir_all(crate::team_graph_run::teams_dir(&root)).unwrap(); crate::team_graph_run::save_graph(&root, &g).unwrap(); std::thread::sleep(std::time::Duration::from_millis(300)); (root, g)
    }
    fn edit(g: &Graph, node: &str, path: &str, v: Value) -> Graph { crate::team_patch::apply_ops(g, &[crate::team_patch::PatchOp::SetField { node: node.into(), path: path.into(), value: v }]).unwrap() }
    #[test] fn a_request_for_a_section_may_only_change_that_section() {
        let c = |n: &str| Change { node: n.into(), what: format!("`{n}` changes"), risk: "low".into() };
        assert!(out_of_scope(&[c("decompose")], &["decompose".into(), "tests".into()]).is_empty()); assert_eq!(out_of_scope(&[c("decompose"), c("writer")], &["decompose".into()]), vec!["`writer` changes".to_string()]);
        assert_eq!(out_of_scope(&[Change { node: String::new(), what: "the order the steps run in changes".into(), risk: "medium".into() }], &["decompose".into()]).len(), 1, "the graph as a whole is outside every section");
    }
    #[test] fn changes_are_told_in_words_with_a_risk_and_the_forbidden_never_reach_a_branch() {
        let g = template("blank", "Demo", 2.0).unwrap(); let mut b = g.clone(); b.budget_usd = 9.0;
        let d = diff(&g, &b); assert_eq!(d.len(), 1); assert_eq!(risk_class(&d), "forbidden");
        assert_eq!(risk_class(&[]), "low"); assert_eq!(risk_of_path("agent.brief"), "low"); assert_eq!(risk_of_path("model"), "medium"); assert_eq!(risk_of_path("agent.effects"), "high"); assert_eq!(risk_of_path("cap_usd"), "forbidden"); assert_eq!(risk_of_path("pos"), "ignore");
    }
    #[test] fn a_request_is_a_branch_with_a_page_and_approving_it_makes_it_the_team_while_refusing_keeps_the_reason() {
        let (root, g) = with_agent("a"); let live = crate::team_graph_run::load_graph(&root, "Demo").unwrap(); let _ = g;
        let cand = edit(&live, "writer", "agent.brief", json!("shorter, in our voice"));
        assert!(open(&root, &live, &cand, "", &[], 1).is_err(), "no problem named");
        assert!(open(&root, &live, &live, "x", &[], 1).is_err(), "no change");
        let mut rich = live.clone(); rich.budget_usd = 99.0; assert!(open(&root, &live, &rich, "x", &[], 1).unwrap_err().contains("may not change"));
        let m = open(&root, &live, &cand, "the writer rambles (run-1, run-2)", &["run-1".into(), "run-2".into()], 10).unwrap(); assert_eq!((m.n, m.branch.as_str(), m.risk.as_str()), (1, "improve/1", "low"));
        assert!(dir(&root, "Demo").join("1").join("REQUEST.md").exists() && page(&m).contains("No experiment yet"));
        let rep = json!({ "current": { "rate": 0.7, "lower": 0.4, "avg_cost_usd": 0.1, "avg_wall_ms": 2000.0, "gate_stops": 1 }, "candidate": { "n": 9, "rate": 1.0, "lower": 0.7, "avg_cost_usd": 0.08, "avg_wall_ms": 1500.0, "gate_stops": 0 }, "regressions": [], "regression_share": 0.0, "spent_usd": 0.4, "why": "admitted" });
        let m = attach_experiment(&root, "Demo", 1, rep).unwrap(); assert!(page(&m).contains("| proposed | 100% | 70%"));
        assert!(decide(&root, "Demo", 1, "approve", "", "me", true, 20).is_err(), "an agent cannot decide");
        let m = decide(&root, "Demo", 1, "approve", "", "me", false, 20).unwrap(); assert_eq!(m.status, "approved");
        let now = crate::team_graph_run::load_graph(&root, "Demo").unwrap(); assert_eq!(serde_json::to_value(&now.nodes["writer"]).unwrap()["agent"]["brief"], json!("shorter, in our voice"));
        assert!(decide(&root, "Demo", 1, "refuse", "x", "me", false, 21).is_err(), "decided once");
        // a second request, refused: the branch is kept and the reason is remembered
        let live2 = crate::team_graph_run::load_graph(&root, "Demo").unwrap(); let cand2 = edit(&live2, "writer", "agent.brief", json!("terse"));
        let m2 = open(&root, &live2, &cand2, "too long again", &[], 30).unwrap(); assert_eq!(m2.n, 2);
        assert!(decide(&root, "Demo", 2, "refuse", "", "me", false, 31).is_err(), "a refusal needs its reason");
        decide(&root, "Demo", 2, "refuse", "terse loses the tone", "me", false, 32).unwrap(); assert!(refusals(&root, "Demo")[0].contains("terse loses the tone"));
        assert!(crate::team_repo::recipe_on(&root, "Demo", "refused/2").is_ok(), "the refused branch is kept");
        // the standing yes: off by default, then a low-risk admitted change merges by itself
        assert!(!auto_merge_on(&root, "Demo") && maybe_auto_merge(&root, "Demo", 2, 40).is_none());
        let live3 = crate::team_graph_run::load_graph(&root, "Demo").unwrap(); let m3 = open(&root, &live3, &edit(&live3, "writer", "agent.brief", json!("plain")), "still wordy", &[], 41).unwrap();
        let good = json!({ "admitted": true, "current": { "rate": 0.5, "lower": 0.2 }, "candidate": { "n": 9, "rate": 1.0, "lower": 0.7 }, "regressions": [], "regression_share": 0.0, "spent_usd": 0.1, "why": "admitted" });
        attach_experiment(&root, "Demo", m3.n, good).unwrap(); assert!(maybe_auto_merge(&root, "Demo", m3.n, 42).is_none(), "off: it waits for the person");
        set_auto_merge(&root, "Demo", true).unwrap(); assert_eq!(maybe_auto_merge(&root, "Demo", m3.n, 43).unwrap().status, "approved");
        assert_eq!(list(&root, "Demo").len(), 3); assert!(benchmark(&root, "Demo")["versions"].as_array().unwrap().len() >= 2);
    }
    #[test] fn a_request_with_too_many_changes_is_one_idea_too_many() {
        let (root, _) = with_agent("b"); let live = crate::team_graph_run::load_graph(&root, "Demo").unwrap();
        let mut c = edit(&live, "writer", "agent.brief", json!("a")); c = edit(&c, "writer", "agent.responsibility", json!("b")); c = edit(&c, "writer", "agent.title", json!("c")); c = edit(&c, "writer", "model", json!("m"));
        assert!(open(&root, &live, &c, "x", &[], 1).unwrap_err().contains("one idea per request"));
    }
}
