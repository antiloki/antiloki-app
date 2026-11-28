//! THE DRIVER OF THE TEAM GRAPH (RFC 0008 §4.6 to §4.9). `team_graph.rs` says what runs next; this carries it out: each role node is a HEADLESS call in a worktree of its own (the engine's CLI, one shot, the
//! tier's model), each `useTool` node a deterministic call with an allow-list, and only the Leader is a terminal. Everything a run does is written, as it happens, into the TEAM SAGA (one saga per team: an episode
//! for every pass of its main loop, an event for every node) and into the pack's sagas (one per saga the planner made: an episode per pass, each pointing back at the team's). The run state is checkpointed after
//! every step, so a run RESUMES after a serve restart and the whole run can be replayed from what the saga recorded.
//!
//!   graph files   `<project>/.antiloki/teams/<team>.json`  (committed, reviewable; the editor and the Leader's MCP write the same file)
//!   run files     `<project>/.antiloki/team/<run>/state.json` + `transcripts/` + `artifacts/`  (not committed)
//!   the team      is a THREAD of the space (`threads.json`, `"team": <id>`): its tree and branch are the thread's (thread_tree.rs); each saga of the pack works in a tree of its own cut from it and is integrated into it
//!                 when it passes; the pack lands on the workspace only when the person approves the merge node.
use crate::team_graph::*;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::mpsc;

fn s(v: &Value) -> String { v.as_str().unwrap_or("").trim().to_string() }
fn now() -> i64 { crate::cli::now_ms() }

fn other_memory_op(op: &str) -> &str { op.strip_prefix("memory_").unwrap_or(op) }

fn ledger_ctx(init: &RunInit, node_id: &str) -> crate::ledger::Ctx {
    let cfg = init.graph.to_json().get("nodes").and_then(|n| n.get(node_id)).map(crate::team_nodebench::cfg_of);
    let task_class = match init.graph.node(node_id).map(|n| &n.kind) { Some(Kind::Llm(sp)) => sp.examples_from.as_ref().map(|e| e.task_class.clone()), _ => None };
    crate::ledger::Ctx { root: init.root.clone(), team: init.team.clone(), run: Some(init.id.clone()), node: Some(node_id.to_string()), pane: None, caller: "team-node".into(), task_class, cfg, bench: true, ..Default::default() }
}

// ── the graph files ───────────────────────────────────────────────────────────────────────────────────────────────────────

pub fn teams_dir(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("teams") }
pub fn graph_path(root: &str, team: &str) -> PathBuf { teams_dir(root).join(format!("{}.json", slug(team))) }
pub fn run_dir(root: &str, run: &str) -> PathBuf { Path::new(root).join(".antiloki").join("team").join(run) }
pub fn load_graph(root: &str, team: &str) -> Result<Graph, String> {
    let t = std::fs::read_to_string(graph_path(root, team)).map_err(|_| format!("no team graph called {team}"))?;
    Graph::from_json(&serde_json::from_str::<Value>(&t).map_err(|e| format!("the graph file is torn: {e}"))?)
}
pub fn save_graph(root: &str, g: &Graph) -> Result<(), String> { save_graph_as(root, g, None) }
/// A save that says WHY: `change` = {reason, by, evidence_run, cost_usd, tokens, seconds, cycle}. The commit message is the reason, its body the numbers, and the same record goes to the team's `improvements.jsonl` with the commit id, so a version, its reason, its cost and the runs it made are one trail.
pub fn save_graph_as(root: &str, g: &Graph, change: Option<&Value>) -> Result<(), String> {
    let p = graph_path(root, &g.team); std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("json.tmp"); std::fs::write(&tmp, serde_json::to_string_pretty(&g.to_json()).unwrap_or_default()).map_err(|e| e.to_string())?; let r = std::fs::rename(&tmp, &p).map_err(|e| e.to_string());
    // EVERY SAVE IS A VERSION (RFC 0013 §2): the recipe is committed to the team's own git, off the request path and one at a time. A failure here never fails the save.
    if r.is_ok() { let (root, g) = (root.to_string(), g.clone()); let change = change.cloned(); std::thread::spawn(move || {
        let (msg, rec) = match &change { Some(c) if c.is_object() => { let f = |k: &str| c[k].as_str().map(String::from).unwrap_or_else(|| c[k].to_string()); let reason: String = f("reason").chars().take(200).collect(); let subject: String = reason.lines().next().unwrap_or("").chars().take(72).collect();
            (format!("improve: {subject}\n\nreason: {reason}\nby: {}\nevidence-run: {}\ncycle: {}\nimprovement-cost-usd: {}\nimprovement-tokens: {}\nimprovement-seconds: {}", f("by"), f("evidence_run"), f("cycle"), f("cost_usd"), f("tokens"), f("seconds")), Some(c.clone())) } _ => ("save".to_string(), None) };
        if let Ok(Some(id)) = crate::team_repo::commit_graph(&root, &g, &msg) { if let Some(mut c) = rec { c["commit"] = json!(id); c["t_ms"] = json!(now()); append_jsonl(&team_files(&root, &g.team).join("improvements.jsonl"), &c); } } }); }
    r
}
pub fn list_graphs(root: &str) -> Vec<Graph> {
    let mut v: Vec<Graph> = std::fs::read_dir(teams_dir(root)).into_iter().flatten().flatten().filter(|e| e.path().extension().map_or(false, |x| x == "json")).filter_map(|e| Graph::from_json(&serde_json::from_str::<Value>(&std::fs::read_to_string(e.path()).ok()?).ok()?).ok()).collect();
    v.sort_by(|a, b| a.team.cmp(&b.team)); v
}
pub fn delete_graph(root: &str, team: &str) -> bool { std::fs::remove_file(graph_path(root, team)).is_ok() }

// ── what a node is told, and what it answers (pure) ───────────────────────────────────────────────────────────────────────

/// The piece of work a role node is on: one saga of the pack (an item of the for-each), or the whole ask when the graph has no for-each. Pure.
#[derive(Clone, Debug, PartialEq)]
pub struct Unit { pub id: String, pub title: String, pub objective: String, pub criteria: Vec<crate::team::Criterion> }
pub fn criteria_of(v: &Value) -> Vec<crate::team::Criterion> {
    v.as_array().into_iter().flatten().enumerate().filter_map(|(i, c)| { let text = c["text"].as_str().or_else(|| c.as_str())?.trim().to_string(); if text.is_empty() { return None; }
        Some(crate::team::Criterion { id: c["id"].as_str().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).unwrap_or_else(|| format!("c{}", i + 1)), text, check: c["check"].as_str().unwrap_or("").trim().to_string() }) }).collect()
}
/// The unit a node is working on: the item of the for-each it is inside (the first one with an item), else the ask itself with the criteria the planner wrote. Pure.
/// The name a unit's working tree goes by: the run's own (`<run number>-<unit>`), so a second run of the team starts from the team's branch and not in what the first one left (its half-finished files, its documents). Pure.
pub fn tree_key(run: &str, unit: &str) -> String { format!("{}-{}", run.rsplit('-').next().unwrap_or(run), unit) }
pub fn unit_of(g: &Graph, values: &Map<String, Value>, ask: &str) -> Unit {
    for (id, n) in &g.nodes { if matches!(n.kind, Kind::ForEach { .. }) { if let Some(item) = values.get(id).and_then(|v| v.get("item")) {
        let uid = item["id"].as_str().map(String::from).unwrap_or_else(|| format!("item-{}", values[id]["index"].as_u64().unwrap_or(0) + 1));
        return Unit { id: uid.clone(), title: item["title"].as_str().unwrap_or(&uid).to_string(), objective: item["objective"].as_str().unwrap_or(ask).to_string(), criteria: criteria_of(&item["criteria"]) };
    } } }
    let crit = values.values().find_map(|v| v.get("criteria").filter(|c| c.is_array())).map(criteria_of).unwrap_or_default();
    Unit { id: "run".into(), title: g.team.clone(), objective: ask.to_string(), criteria: crit }
}
fn json_in(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) { if v.is_object() { return Some(v); } }
    let a = t.find('{')?; let b = t.rfind('}')?; if b <= a { return None; }
    serde_json::from_str::<Value>(&t[a..=b]).ok().filter(Value::is_object)
}
/// The planner's pack: sagas with an order (`needs`) and acceptance criteria each. A reply with no saga, or a saga with no criterion, is not a plan (the QA would have nothing to hold the work to). Pure.
pub fn parse_pack(text: &str) -> Result<Value, String> {
    let v = json_in(text).ok_or("the plan is not JSON: write {\"summary\", \"sagas\": [{\"id\", \"title\", \"objective\", \"needs\": [ids], \"criteria\": [{\"id\", \"text\", \"check\"?}]}]}")?;
    let mut sagas = vec![]; let mut seen: Vec<String> = vec![];
    for (i, x) in v["sagas"].as_array().into_iter().flatten().enumerate() {
        let id = x["id"].as_str().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).unwrap_or_else(|| format!("saga-{}", i + 1));
        if seen.contains(&id) { return Err(format!("two sagas share the id {id}")); }
        let crit = criteria_of(&x["criteria"]); if crit.is_empty() { return Err(format!("the saga {id} has no acceptance criteria: the QA could not judge it against anything")); }
        let title = s(&x["title"]); let needs: Vec<String> = x["needs"].as_array().into_iter().flatten().filter_map(|n| n.as_str().map(String::from)).collect();
        seen.push(id.clone()); sagas.push(json!({ "id": id, "title": if title.is_empty() { id.clone() } else { title }, "objective": s(&x["objective"]), "needs": needs, "criteria": crit.iter().map(|c| json!({ "id": c.id, "text": c.text, "check": c.check })).collect::<Vec<_>>() }));
    }
    if sagas.is_empty() { return Err("the plan has no sagas".into()); }
    for sg in &sagas { for n in sg["needs"].as_array().into_iter().flatten() { if !seen.iter().any(|i| Some(i.as_str()) == n.as_str()) { return Err(format!("the saga {} needs {}, which is not in the plan", sg["id"], n)); } } }
    // an order the for-each can follow: a saga after the ones it needs
    let mut ordered: Vec<Value> = vec![]; let mut left = sagas.clone();
    while !left.is_empty() { let n0 = left.len(); let mut i = 0; while i < left.len() { if left[i]["needs"].as_array().into_iter().flatten().all(|n| ordered.iter().any(|o| o["id"] == *n)) { ordered.push(left.remove(i)); } else { i += 1; } } if left.len() == n0 { return Err("the sagas need each other in a circle".into()); } }
    let criteria: Vec<Value> = ordered.iter().flat_map(|sg| sg["criteria"].as_array().cloned().unwrap_or_default()).collect();
    Ok(json!({ "plan": { "summary": s(&v["summary"]) }, "sagas": ordered, "criteria": criteria }))
}
/// The QA's answer against the unit's criteria, as the values a condition reads. Pure.
pub fn parse_qa(text: &str, criteria: &[crate::team::Criterion]) -> Result<Value, String> {
    let plan = crate::team::Plan { summary: String::new(), steps: vec![], criteria: criteria.to_vec() };
    let v = crate::team::parse_verdict(text, &plan)?;
    let failed: Vec<Value> = v.results.iter().filter(|r| !r.ok).map(|r| json!(r.id)).collect();
    Ok(json!({ "pass": v.passed, "failed": failed, "evidence": v.results.iter().map(|r| json!({ "id": r.id, "ok": r.ok, "evidence": r.evidence })).collect::<Vec<_>>(), "summary": v.summary }))
}
/// The audit's verdict: pass, flag or block, with what it flagged. Pure.
pub fn parse_audit(text: &str) -> Result<Value, String> {
    let v = json_in(text).ok_or("the audit is not JSON: write {\"verdict\": \"pass\"|\"flag\"|\"block\", \"flags\": [what you found], \"summary\"}")?;
    let verdict = s(&v["verdict"]).to_ascii_lowercase();
    if !["pass", "flag", "block"].contains(&verdict.as_str()) { return Err("the verdict is one of pass, flag, block".into()); }
    let flags: Vec<Value> = v["flags"].as_array().into_iter().flatten().filter_map(|f| f.as_str().map(|x| json!(x))).collect();
    if verdict != "pass" && flags.is_empty() { return Err("a flag or a block says what it found".into()); }
    Ok(json!({ "verdict": verdict, "flags": flags, "summary": s(&v["summary"]) }))
}
fn crit_list(c: &[crate::team::Criterion]) -> String { c.iter().map(|c| format!("- {}: {}{}", c.id, c.text, if c.check.is_empty() { String::new() } else { format!(" (check: `{}`)", c.check) })).collect::<Vec<_>>().join("\n") }
/// What changed since the node last ran that the role should act on (the failed criteria, the tests that failed). Pure.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Delta { pub qa_failed: Vec<String>, pub qa_evidence: Vec<String>, pub tests_failed: Vec<String>, pub tests_log: String, pub attempt: u32 }
pub fn delta_of(values: &Map<String, Value>) -> Delta {
    let mut d = Delta::default();
    for (_, v) in values { if let Some(ev) = v.get("evidence").and_then(Value::as_array) { for e in ev { if e["ok"] == json!(false) { d.qa_failed.push(s(&e["id"])); d.qa_evidence.push(format!("{}: {}", s(&e["id"]), s(&e["evidence"]))); } } }
        if v.get("exit").and_then(Value::as_i64).map_or(false, |x| x != 0) { for f in v["failed"].as_array().into_iter().flatten() { if let Some(t) = f.as_str() { d.tests_failed.push(t.to_string()); } } d.tests_log = s(&v["tail"]); } }
    d
}
/// The prompt for a role node. Pure. `tag` lets a test seat know which node is speaking.
pub fn role_prompt(role: RoleKind, tag: &str, ask: &str, unit: &Unit, owns: &[String], delta: &Delta) -> String {
    let work = format!("THE WORK ({}):\n{}\n\n{}", unit.title, if unit.objective.is_empty() { ask } else { unit.objective.as_str() }, if unit.criteria.is_empty() { String::new() } else { format!("It will be judged by these criteria:\n{}\n", crit_list(&unit.criteria)) });
    let scope = if owns.is_empty() { String::new() } else { format!("You own only these files: {}. A change outside them is put back.\n", owns.join(", ")) };
    let fix = { let mut f = String::new(); if !delta.qa_evidence.is_empty() { f.push_str(&format!("The QA rejected the last delivery. What is still wrong:\n- {}\nFix exactly these and do not undo what passed.\n", delta.qa_evidence.join("\n- "))); } if !delta.tests_failed.is_empty() { f.push_str(&format!("These tests failed:\n- {}\n{}\n", delta.tests_failed.join("\n- "), if delta.tests_log.is_empty() { String::new() } else { format!("The end of the log:\n{}\n", delta.tests_log) })); } f };
    match role {
        RoleKind::Planner => format!("{tag}\nYou are the PLANNER of a team. Do not edit any file. Split this ask into SAGAS (units of work that can be delivered one after another) and write the acceptance criteria of each BEFORE any work starts:\n\n{ask}\n\nReply with ONE JSON object and nothing else: {{\"summary\": one sentence, \"sagas\": [{{\"id\": \"s1\", \"title\": short, \"objective\": what must be done, \"needs\": [ids of sagas that come first], \"criteria\": [{{\"id\": \"c1\", \"text\": what must be true when it is done, \"check\": a command that proves it, if there is one}}]}}]}}. Each criterion must be observable."),
        RoleKind::Worker => format!("{tag}\nYou are the WORKER of a team. Do this work in your box.\n\n{work}\n{scope}{fix}When you are done, say what you changed."),
        RoleKind::Debugger => format!("{tag}\nYou are the DEBUGGER of a team. Something failed; you get only what failed, not the whole story. Patch it.\n\n{work}\n{scope}{fix}Change as little as you can, then say what you patched."),
        RoleKind::Qa => format!("{tag}\nYou are the QA of a team. Do NOT edit any file: read, run and test only. Judge the worker's delivery of:\n\n{work}\nAnswer EVERY criterion.\n\nReply with ONE JSON object and nothing else: {{\"results\": [{{\"id\": \"c1\", \"ok\": true|false, \"evidence\": what you saw or ran; required when ok is false}}], \"summary\": one sentence}}."),
        RoleKind::Agent => String::new(),   // a custom agent's prompt is `agent_prompt`
        RoleKind::Audit => format!("{tag}\nYou are the AUDIT of a team. Do NOT edit any file. Check this delivery against the project's laws, its scope (only the files it owns), its cost and the plan, and flag what the QA does not look at:\n\n{work}\nReply with ONE JSON object and nothing else: {{\"verdict\": \"pass\"|\"flag\"|\"block\", \"flags\": [what you found; required unless pass], \"summary\": one sentence}}. `block` is for something that must not be delivered."),
    }
}
/// A CUSTOM AGENT'S PROMPT (RFC 0008 §4.16): its one responsibility, its brief, the playbooks of its skills, the context files it should read first, the tools it may use and the files it owns.
/// THE SEAT'S OWN WEB SEARCH (§4.4): a short headless call that may use only WebSearch and answers in JSON, so a team has real search with no key. It costs a few cents and is priced like any call (the node's cap applies). Claude Code only.
async fn seat_search(init: &RunInit, node: &Node, query: &str, max: usize) -> Result<(Value, f64), String> {
    let engine = crate::cli::Engine::ClaudeCode; if !crate::cli::engine_available(engine) { return Err("no Claude Code seat here to search with".into()); }
    if let Some((_, why)) = crate::cli::seat_limit(engine) { return Err(format!("the Claude Code seat is out of quota ({}): the next search way is tried", why.chars().take(80).collect::<String>())); }   // do not spend the node's time on a seat that will refuse (the first timed Leader run stalled at its searches)
    let dir = run_dir(&init.root, &init.id).join("web"); std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let prompt = format!("Use the WebSearch tool to search for: {query}\nReply with ONLY a JSON object {{\"results\":[{{\"title\":\"...\",\"url\":\"https://...\",\"snippet\":\"one or two sentences from the page\"}}]}} with at most {max} results, the most relevant first. No other words, no other tool.");
    let opts = crate::cli::RunOpts { write: false, max_turns: Some(5), timeout_ms: Some(node.timeout_s.map(|t| t * 1000).unwrap_or(100_000)), tier: Some(crate::cli::Tier::Low), extra_tools: vec!["WebSearch".into()], ledger: Some(crate::ledger::Ctx { node: None, caller: "team-search".into(), ..ledger_ctx(init, "") }), ..Default::default() };
    let msgs = vec![crate::cli::ChatMessage { role: "user".into(), content: prompt }];
    let out = crate::cli::run_cli(engine, &msgs, &dir.to_string_lossy(), false, opts, None).await?;
    if let Some(e) = &out.parsed.error { return Err(e.clone()); }
    let cost = out.parsed.cost.or_else(|| crate::cli::priced_from_tokens(engine, None, out.parsed.tokens_in, out.parsed.tokens_out)).unwrap_or(0.0);
    let hits = crate::team_web::hits_in_reply(&out.parsed.content, max); if hits.is_empty() { return Err("the search came back with no results it could read".into()); }
    Ok((crate::team_web::shape(query, "seat", &hits), cost))
}
/// WHAT THE NODES BEFORE A CUSTOM AGENT FOUND OR SAID (§4.4): the report of an agent that leads into it, the text a web node brought back, the name of an artifact: each cut to a size, so an agent works from what the web gave and not from nothing. Pure.
pub fn inputs_for(g: &Graph, values: &Map<String, Value>, id: &str) -> String {
    let mut seen: Vec<&str> = vec![]; let mut out = String::new();
    // A JOIN passes on what it joined: an agent after `join(page, copy)` is told the page's text AND the copy, not nothing (the join has no words of its own). The sources are the join's own predecessors, one step deep per join, never in a loop.
    let mut froms: Vec<(&str, &str)> = vec![];
    for e in g.edges.iter().filter(|e| e.to == id && !FAILURE_PORTS.contains(&e.port.as_str())) {
        if matches!(g.node(&e.from).map(|n| &n.kind), Some(Kind::Join { .. })) { for e2 in g.edges.iter().filter(|x| x.to == e.from && !FAILURE_PORTS.contains(&x.port.as_str())) { if !froms.iter().any(|f| f.0 == e2.from.as_str()) { froms.push((e2.from.as_str(), e2.port.as_str())); } } }
        else if !froms.iter().any(|f| f.0 == e.from.as_str()) { froms.push((e.from.as_str(), e.port.as_str())); }
    }
    for (from, _port) in froms {
        if seen.contains(&from) { continue; } seen.push(from); let (Some(n), Some(v)) = (g.node(from), values.get(from)) else { continue };
        let name = n.label.clone().unwrap_or_else(|| from.to_string());
        let body = match &n.kind { Kind::Role { role: RoleKind::Agent, .. } => s(&v["report"]), Kind::UseTool { tool, .. } if tool.kind == "web" => s(&v["text"]), Kind::Llm(sp) => { let mut parts: Vec<String> = sp.answer.keys().filter_map(|k| v.get(k).map(|x| format!("{k}: {}", match x { Value::String(t) => t.clone(), o => o.to_string() }))).collect(); if let Some(f) = v["escalated_from"].as_str().filter(|f| !f.is_empty()) { parts.push(format!("(a stronger seat answered after {f} failed)")); } parts.join(", ") }, Kind::Check { .. } => { let e = crate::team_check::evidence(v); if e.is_empty() { String::new() } else { format!("the checks found these wrong; fix exactly these:\n{e}") } }, Kind::Artifact { .. } => format!("the document {} was made (artifacts/{})", s(&v["name"]), s(&v["name"]) + "." + s(&v["format"]).as_str()), _ => String::new() };
        if body.trim().is_empty() { continue; }
        let cut: String = body.chars().take(6000).collect(); out.push_str(&format!("FROM {name}:\n{cut}{}\n\n", if body.chars().count() > 6000 { "\n[cut]" } else { "" }));
        if out.len() > 16_000 { break; }
    }
    out
}
/// THE ANTILOKI MCP TOOLS AN AGENT NODE LISTS (`mcp`), as the names the CLI wants (`mcp__antiloki__<tool>`): without them the CLI refuses the call, "requested permissions ... you haven't granted it" (found 2026-11-21 running the pane teams). Only names that are plain tool names and not the banned ones (those start agents or steer teams). Pure.
pub fn antiloki_mcp_tools(a: Option<&crate::team_graph::AgentSpec>) -> Vec<String> {
    a.map(|a| a.mcp.iter().filter(|t| t.starts_with("antiloki_") && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !crate::team_graph::mcp_banned(t)).map(|t| format!("mcp__antiloki__{t}")).collect()).unwrap_or_default()
}
pub fn agent_prompt(tag: &str, ask: &str, unit: &Unit, a: &AgentSpec, owns: &[String], playbooks: &str, delta: &Delta, inputs: &str) -> String {
    let work = format!("THE TEAM'S ASK:\n{}\n\nTHE PART YOU ARE ON: {}\n{}", ask, unit.title, if unit.objective.is_empty() || unit.objective == ask { String::new() } else { format!("{}\n", unit.objective) });
    let ctx = if a.context.is_empty() { String::new() } else { format!("Read these first: {}.\n", a.context.join(", ")) };
    let web = if a.has("web") { "You may search and read the web with your own web tools; say where a fact came from.\n" } else { "" };
    let tools = format!("{}{web}", if a.mcp.is_empty() { "You may use no MCP tool beyond reading.\n".to_string() } else { format!("The MCP tools you may use: {}. No others.\n", a.mcp.join(", ")) });
    let found = if inputs.trim().is_empty() { String::new() } else { format!("\nWHAT THE NODES BEFORE YOU FOUND OR SAID (use it; do not make it up again):\n{inputs}") };
    let scope = if owns.is_empty() { "You edit no file.\n".to_string() } else { format!("You own only these files: {}. A change outside them is put back.\n", owns.join(", ")) };
    let acts = if a.has("outward") || a.has("money") { "You may act in the world only as your brief says; the person approves before the team reaches you.\n" } else { "" };
    let fix = if delta.qa_evidence.is_empty() { String::new() } else { format!("The last pass was rejected. What is still wrong:\n- {}\n", delta.qa_evidence.join("\n- ")) };
    let skills = if playbooks.is_empty() { String::new() } else { format!("\n{playbooks}\n") };
    format!("{tag}\nYou are {} of a team: {}.\n\n{}\n{work}{found}{ctx}{tools}{scope}{acts}{fix}{skills}\nDocuments that are not code (a report, a dashboard, a draft) go in `.antiloki/artifacts/<name>.md` (or `.html`, `.csv`, `.json`, `.svg`) in your folder, never among the files you own: they are kept with the run, not in the code. Say what each one is in `report`.\n\nWhen you are done reply with ONE JSON object and nothing else: {{\"done\": true|false, \"report\": \"what you did, and what you could not do\", \"files\": [\"paths you changed\"], \"pass\": true|false}} (`pass` only when your job is to judge: true when what you judged is acceptable; leave it out otherwise)", a.title, a.responsibility, a.brief)
}
/// A custom agent's reply: the JSON asked for, or, when it only talked, its words as the report. Pure.
pub fn parse_agent(text: &str, changed: Vec<String>) -> Value {
    let t = text.trim(); let from = t.find('{'); let to = t.rfind('}');
    if let (Some(a), Some(b)) = (from, to) { if b > a { if let Ok(v) = serde_json::from_str::<Value>(&t[a..=b]) { if v.is_object() && (v.get("done").is_some() || v.get("report").is_some()) { let done = v["done"].as_bool().unwrap_or(true); return json!({ "done": done, "pass": v["pass"].as_bool().unwrap_or(done), "report": s(&v["report"]).chars().take(2000).collect::<String>(), "files": changed, "said": s(&v["report"]).chars().take(400).collect::<String>() }); } } } }
    json!({ "done": !t.is_empty(), "pass": !t.is_empty(), "report": t.chars().take(2000).collect::<String>(), "files": changed, "said": t.chars().take(400).collect::<String>() })
}
/// WHAT THE LEADER IS TOLD when the person makes a team from their own words (RFC 0008 §4.16): screen first, design the whole team, write the skills, check, and wait for the person before any run. Pure.
pub fn leader_brief(team: &str, goal: &str, budget_usd: f64) -> String {
    format!("You are the LEADER of a new team called {team}. THE PERSON'S GOAL, in their own words:\n\n{goal}\n\n\
YOUR MEMORY IS THE TEAM\'S SAGA. You start every session knowing nothing of this team except what you are handed: a message that starts `[team memory]` (what it has done, what the person decided, what went wrong) arrives in this terminal when it opens, and you can read it again at any time with antiloki_team_audit op `recall`. CONTINUE from it; never start the team over or ask what it already answers.\n\nYOU ARE THE FRONTIER MODEL OF THIS TEAM, BY DESIGN: you screen, design and judge it, and you are never swapped for a small or local model. Small and local models are for STEPS (classify, extract, a mechanical check), chosen only where a measurement says they are good enough; you stay the strongest seat the person has.\n\
1. SCREEN FIRST, WITH A FEW QUESTIONS, NOT AN INTERVIEW. If something that matters is unclear, ask the person at most four short questions at once, here in this terminal (who the result is for, the one thing that must be true of it, what it may spend or publish, what it must never touch), and wait. If it is clear enough, say so and go on. Build nothing before that.\n\
0. THE WORLD IS THE PERSON'S APPS. A team may use the web apps they installed from the store (search the web, research, Meta Business to post, Notion...): give an agent `app-<id>` in its `mcp`. Before you finish designing, call antiloki_team_apps `preflight` {{team}} and TELL THE PERSON, before the first run, everything only they can do: install an app, sign in (you can open the page: `login`), a business page, two-step codes; anything that posts or spends stays behind a human gate. You are not limited to the node kinds you have seen: build any block for any purpose; teams may be large, and only the nodes that are working cost anything. Run independent work in parallel (fork, for_each with `parallel`, `parallel` on workers) and keep a strict budget: per agent (`cap_usd`), per run (`budget_usd`), and for the whole result (antiloki_team_campaign: parallel runs and A/B tests of two seats or two briefs, inside the machine's capacity), never above what the person agreed.\n\
1a. THE TEAM IMPROVES ITSELF, THROUGH YOU. Every run is audited when it ends (free, no model: status, checks, loops, small-model climbs, cost, gates). When the result is not good you receive a message starting `[team audit]` in this terminal, and it is also kept in your inbox (antiloki_team_audit `inbox`; `history` shows the scores by graph so you can see whether your last change helped). It is yours to decide: change the team (nodes, edges, agents, llm nodes with guards, checks, gates, briefs, seats, budgets) or leave it and say why; tell the person what you found and changed, and ask before running it again unless they asked you to iterate on your own. A team with NO check, QA or gate cannot be audited for quality: give every team at least one measurement, and say so when you cannot.\n\
1b. IS A TEAM WORTH IT? A team earns its place only if it delivers MORE than one raw agent would (an independent check that catches errors, parallel work, specialists, a result the person can trust) or costs LESS (cheap models for the mechanical steps, a strong one only where it matters). Tell the person in two plain sentences which it is for THIS goal. If one raw agent would do as well (a short, one-skill, one-shot task), SUGGEST that instead, or a PAIR (two agents dragged together: one plans and also judges, the other works), and say what they would save. Suggest, never force: if they still want the team, build it. Never build more than the goal needs.\n\
2. DESIGN the whole team with antiloki_team_design: one agent per responsibility (role \"agent\": a title, ONE sentence of responsibility, a brief, the context files it reads, its skills, the MCP tools it may use, its effects: `read`, `document` for an agent that only makes documents, `write` only for one that edits files it owns), control flow where it helps (loops with limits and cost caps, for_each, if, fork and join; a review loop ends on `until` an agent's `pass`, e.g. `reviewer.pass == true`), artifact nodes for the documents that are not code, web nodes (`use_tool` of type `web`, name `search` or `fetch`, effect `network`) when the team should look at the web before it writes (the agents after one are told what it found; an agent that must browse itself has the effect `web`), validated first with `dry_run`, tool nodes for steps that need no model, and a human node on every way to anything that posts, sends, publishes or spends. The code roles (planner, worker, QA, debugger, audit) are there when the team writes code. The budget is under ${budget_usd:.2}, the ceiling the person set.\n\
1c. YOU ARE THE MANAGER OF THE SEATS. Before you design, call antiloki_team_seats `list`: it shows what this machine has (CLI seats, Ollama models, endpoints, the hardware, what could be downloaded and whether it would fit) and the measured evidence per kind of step. Then JUDGE, as the frontier model you are: is a team needed at all; can a small or local seat take this step, which one, with which guards; use `fit` {{stage, seat}} for the facts of one step. If the step is not in the evidence it is UNMEASURED: write cases yourself, a third of them TRAPS (inputs the seat is likely to get wrong), and `trial` it; a benchmark that makes no mistakes proves nothing, and you report every mistake to the person. If a local model would help and is not here, say which, how big, why, and whether it fits; ask the person; only on their yes call `pull` (or give them the command), then `register` the endpoint so a node can use it. If the person insists on a seat you advise against, say the risk plainly, require the guards (a fallback to a stronger seat, a second seat in `agree_with`, `verify.in_source`: the validator refuses a small seat with none), trial it first, and do it: the choice is theirs. Never move yourself down, and never put a small seat on code, planning, creative writing or anything no check can tell was wrong.\n\
A STEP A SMALL MODEL CAN TAKE (classify, extract, a cross-checkable judgement) IS AN `llm` NODE, NOT AN AGENT: one call, no tools, a typed `answer`, a `seat` (`endpoint:lfm3`, `local:qwen3:4b`, `claude-code:haiku`), and ALWAYS its safeguards: `verify.in_source` (an extracted field must appear in the source text), `agree_with` (a second seat must give the same answer) and a `fallback` ladder to a stronger seat. Never a small seat for code, creative writing, tone judgement or planning.\n\
EVERY NODE SAYS WHY (`why`: {{kind, note}}; kinds: parallel, independent_check, tool, cost, gate, schedule, memory, sense). IF A RULE CAN BE WRITTEN, WRITE A CHECK: a `check` node (forbid or require a pattern, count words, require sections, valid JSON, clean HTML, reading ease) costs nothing and gives the same answer every time; use an agent only for what cannot be written as a rule, and remove a node you cannot justify. A team that is just a chain of agents is warned that it is one prompt with extra steps.\n\
END WITH ONE PAGE. The person should read ONE result, not a pile of files: finish the team with an `artifact` node that carries `compose` (name \"result\", format \"html\"): sections of widgets, data and never code. Widgets: hero, summary (from `report:<node>`), markdown (from `artifact:<file>.md`), kpis (from `run:cost`, `run:time`, `node:<id>.<field>`), table, checklist, timeline, cards, cost, links, callout, bar, embed. Place it after the last agent and before the merge gate; it costs nothing and uses no model. antiloki_team_design checks it and says what is wrong.\n\
2a. DOCS THE TEAM NEEDS (a tool, a library, an API) ARE READ INTO FILES, NEVER LEFT AS A LINK: put a `use_tool` node of type `web`, name `docs` (args: `url` of the docs start page, `max` pages up to 30; effects `network` and `read`) BEFORE the agents that need it; it saves the real pages as Markdown under the team's `docs/` folder with an INDEX.md, and the agents after it read those files with Read/Grep. A web `search` returns links, not docs.\n\
2d. TELL THE PERSON ON TELEGRAM: a `use_tool` node of type `telegram` (tool `send`; args `chat`, `text`, `token` = the NAME the person stored their bot token under) lists `network` and `outward` and must come AFTER a human gate; send a short summary with links, never documents.\n\
2c. GIT STEPS: a `use_tool` node of type `git` (the tool name is the operation: status, diff, log, branch, worktree_add, worktree_remove, commit, merge; `args.name` for a branch/unit, `args.message` for a commit, `args.unit` to work in a unit's tree) works inside the team's OWN branch and worktrees; reads list `read`, the rest `write`. It never pushes or touches the person's branch: what reaches the workspace is their merge gate.\n\
2b. ANY SERVICE, NOT ONLY INSTALLED APPS: a `use_tool` node of type `mcp_url` calls any MCP server by its address (args: `url`, `arguments`, optional `token` = a name the person stored; the tool name is the node's tool name), and one of type `api` calls any http API (the tool name is the method GET/POST/PUT/PATCH; args: `url`, `headers`, `body`, `token`). Both list `network` plus the risk they take (`read`, or `write` for a POST/PUT/PATCH or a tool that writes). A secret is NEVER written into a graph: ask the person to store it under a name. DELETE and destructive tools are never a team's.\n\
3. WRITE THE SKILLS the agents need and that do not exist, with antiloki_team_skill: a purpose, the effects, a playbook of steps. A skill never gives an agent more than its own effects. You approve them yourself: they are active at once, and the person can remove any of them from the graph, where each shows as a support node in the team pane.\n\
4. CHECK: read the validation antiloki_team_design returns and fix every error, run antiloki_team_simulate, then tell the person in plain words what the team is, who does what, what it costs in the worst case, and which skills you wrote.\n\
5. Do not start it (antiloki_team_run) until the person says so. You may edit the team afterwards (antiloki_team_design, antiloki_team_node, antiloki_team_skill) to fix it or when the person asks, and say what you changed. You cannot approve anything: skills, merges and posts are the person's.\n\n\
Start now with step 1.")
}
/// The loop (or retry) whose passes are the episodes of the team saga: the first loop that is not inside another's body; failing that, the first retry; else none (the whole run is one episode). Pure.
pub fn main_pass_node(g: &Graph) -> Option<String> {
    // the first loop the run reaches (a breadth-first walk from where it starts): the outer one comes before the loops inside it
    let mut seen: Vec<String> = vec![]; let mut q: std::collections::VecDeque<String> = g.entries().into_iter().collect(); let mut first_retry: Option<String> = None;
    while let Some(n) = q.pop_front() { if seen.contains(&n) { continue; } seen.push(n.clone()); match g.node(&n).map(|x| &x.kind) { Some(Kind::Loop { .. }) => return Some(n), Some(Kind::Retry { .. }) if first_retry.is_none() => first_retry = Some(n.clone()), _ => {} } for e in g.out_edges(&n) { q.push_back(e.to.clone()); } }
    first_retry
}
/// The command a built-in skill runs in a folder: fixed by antiloki, never by the graph. `None` when the project has nothing of that kind. Pure over what is in the folder.
pub fn skill_command(skill: &str, dir: &Path) -> Option<String> {
    let has = |f: &str| dir.join(f).is_file();
    if let Ok(over) = std::env::var(format!("ANTILOKI_TEAM_{}_CMD", skill.to_ascii_uppercase().replace('-', "_"))) { if !over.trim().is_empty() { return Some(over); } }
    let pkg = std::fs::read_to_string(dir.join("package.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let script = |n: &str| pkg.as_ref().and_then(|p| p["scripts"][n].as_str()).is_some();
    match skill {
        "run-tests" => if script("test") { Some("npm test --silent".into()) } else if has("Cargo.toml") { Some("cargo test".into()) } else if has("pytest.ini") || has("pyproject.toml") { Some("python3 -m pytest -q".into()) } else { None },
        "run-build" => if script("build") { Some("npm run build --silent".into()) } else if has("Cargo.toml") { Some("cargo build".into()) } else { None },
        "lint" => if script("lint") { Some("npm run lint --silent".into()) } else if has("Cargo.toml") { Some("cargo clippy".into()) } else { None },
        _ => None,
    }
}
/// The effects a built-in skill needs: a node may call it only when its allow-list names them. Pure.
pub fn skill_effects(skill: &str) -> Option<&'static [&'static str]> { match skill { "run-tests" | "run-build" | "lint" | "preview" => Some(&["read", "exec"]), "law-gate" => Some(&["read"]), "diff-stats" => Some(&["read"]), _ => None } }
/// The branch of a fork a node works in (`<fork>:<first node of the branch>`), when it is inside one: its tree is the branch's own until the join. Pure.
pub fn branch_label(g: &Graph, node: &str) -> Option<String> {
    for (fid, f) in &g.nodes { if f.kind != Kind::Fork { continue; }
        for e in g.out_edges(fid).filter(|e| e.port == "branch") {
            let mut seen: Vec<&str> = vec![]; let mut q: Vec<&str> = vec![e.to.as_str()];
            while let Some(n) = q.pop() { if seen.contains(&n) { continue; } if matches!(g.node(n).map(|x| &x.kind), Some(Kind::Join { .. })) { continue; } seen.push(n); if n == node { return Some(format!("{fid}-{}", e.to)); } for o in g.out_edges(n) { q.push(o.to.as_str()); } }
        } }
    None
}
/// The effects an MCP tool node needs for the antiloki tools it may call (read-only looks, or acting on a pane). Pure.
pub fn mcp_effects(tool: &str) -> Option<&'static [&'static str]> { match tool { "antiloki_panes" | "antiloki_app_tools" | "antiloki_machine" | "antiloki_cost" | "antiloki_free_pane_list" | "antiloki_pane_view" | "antiloki_pane_read" | "antiloki_worktrees" | "antiloki_spaces" | "antiloki_themes" | "antiloki_xray" | "antiloki_rules" => Some(&["read"]), "antiloki_query" | "antiloki_sources" | "antiloki_file_review" | "antiloki_folder_review" | "antiloki_blast_radius" | "antiloki_tests_for" | "antiloki_check" | "antiloki_laws" => Some(&["read"]), "antiloki_pane_act" | "antiloki_free_pane_write" | "antiloki_free_pane_open" | "antiloki_pane_open" | "antiloki_pane_arrange" | "antiloki_screen" => Some(&["read", "write"]), _ => None } }
/// The tests that failed, from a log (best effort over the common runners). Pure.
pub fn failed_tests(log: &str) -> Vec<String> {
    let mut v: Vec<String> = vec![];
    for l in log.lines() { let t = l.trim(); let hit = t.strip_prefix("FAIL ").or_else(|| t.strip_prefix("✗ ")).or_else(|| t.strip_prefix("✕ ")).or_else(|| t.strip_prefix("--- FAIL: ")).or_else(|| t.strip_prefix("FAILED ")).or_else(|| t.strip_prefix("not ok ")).or_else(|| if t.starts_with("test ") && t.ends_with("... FAILED") { Some(t.trim_start_matches("test ").trim_end_matches("... FAILED").trim()) } else { None });
        if let Some(h) = hit { let h = h.trim(); if !h.is_empty() && !v.iter().any(|x| x == h) && v.len() < 30 { v.push(h.chars().take(200).collect()); } } }
    v
}

// ── the sagas (the team's own, and the pack's) ────────────────────────────────────────────────────────────────────────────

/// The team's own saga: one per team, made the first time it is needed. Blocking.
pub fn ensure_team_saga(c: &rusqlite::Connection, team: &str, name: &str) -> Result<i64, String> {
    let key = format!("team:{}", slug(team));
    if let Ok(id) = c.query_row("SELECT id FROM sagas WHERE plan_ref = ?1", [&key], |r| r.get::<_, i64>(0)) { return Ok(id); }
    let t = now(); let meta = json!({ "kind": "team", "team": slug(team) }).to_string();
    c.execute("INSERT INTO sagas (title, objective, origin, plan_ref, meta, created_ms, updated_ms) VALUES (?1, ?2, 'team', ?3, ?4, ?5, ?5)", rusqlite::params![format!("{name} · team"), "The team's own record: an episode for every pass of its main loop, an event for every node.", key, meta, t]).map_err(|e| e.to_string())?;
    let id = c.last_insert_rowid(); crate::work::event(c, id, None, "team", "created", &format!("the team {name} was made"))?; Ok(id)
}
/// A saga of the pack (one the planner made): its own saga, grouped with the others by `pack`, in the team's words. Blocking.
pub fn ensure_pack_saga(c: &rusqlite::Connection, team: &str, run: &str, unit: &Value) -> Result<i64, String> {
    let key = format!("pack:{}:{}:{}", slug(team), run, s(&unit["id"]));
    if let Ok(id) = c.query_row("SELECT id FROM sagas WHERE plan_ref = ?1", [&key], |r| r.get::<_, i64>(0)) { return Ok(id); }
    let t = now(); let meta = json!({ "kind": "pack", "team": slug(team), "run": run, "pack": format!("{}:{}", slug(team), run), "unit": s(&unit["id"]), "needs": unit["needs"], "criteria": unit["criteria"] }).to_string();
    c.execute("INSERT INTO sagas (title, objective, origin, plan_ref, meta, created_ms, updated_ms) VALUES (?1, ?2, 'team', ?3, ?4, ?5, ?5)", rusqlite::params![s(&unit["title"]), s(&unit["objective"]), key, meta, t]).map_err(|e| e.to_string())?;
    let id = c.last_insert_rowid(); crate::work::event(c, id, None, "team", "created", &format!("planned by the team ({run})"))?; Ok(id)
}
/// A new episode (a pass): `Pass n`, `doing`, with what made it. `retry_of` points at the pass it tries again. Blocking.
pub fn open_episode(c: &rusqlite::Connection, saga: i64, title: &str, ext: &str, meta: &Value) -> Result<i64, String> {
    if let Ok(id) = c.query_row("SELECT id FROM tasks WHERE ext = ?1", [ext], |r| r.get::<_, i64>(0)) { return Ok(id); }
    let pos: i64 = c.query_row("SELECT COALESCE(MAX(position), 0) + 1 FROM tasks WHERE saga_id = ?1 AND parent_id IS NULL", [saga], |r| r.get(0)).unwrap_or(1); let t = now();
    c.execute("INSERT INTO tasks (saga_id, title, status, assignee, created_by, position, ext, meta, created_ms, updated_ms, started_ms) VALUES (?1, ?2, 'doing', 'team', 'team', ?3, ?4, ?5, ?6, ?6, ?6)", rusqlite::params![saga, title, pos, ext, meta.to_string(), t]).map_err(|e| e.to_string())?;
    let id = c.last_insert_rowid(); crate::work::event(c, saga, Some(id), "team", "created", &format!("started {title}"))?; let _ = c.execute("UPDATE sagas SET updated_ms = ?1 WHERE id = ?2", rusqlite::params![t, saga]); Ok(id)
}
pub fn close_episode(c: &rusqlite::Connection, ep: i64, status: &str, usd: f64, ms: i64) -> Result<(), String> {
    let t = now(); c.execute("UPDATE tasks SET status = ?1, done_ms = ?2, updated_ms = ?2, usd_total = usd_total + ?3, ms_total = ?4 WHERE id = ?5", rusqlite::params![status, t, usd, ms, ep]).map_err(|e| e.to_string())?; Ok(())
}
/// One line of the trail, once (the key is the run and its number, so replaying a run changes nothing). Blocking.
pub fn trace_event(c: &rusqlite::Connection, saga: i64, ep: Option<i64>, run: &str, seq: usize, ev: &TraceEv) -> Result<(), String> {
    let actor = if ev.node.is_empty() { "team".to_string() } else { ev.node.clone() };
    c.execute("INSERT OR IGNORE INTO events (saga_id, task_id, t_ms, actor, kind, text, ext) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", rusqlite::params![saga, ep, ev.t_ms, actor, ev.kind, ev.text, format!("tg:{run}:{saga}:{seq}")]).map_err(|e| e.to_string())?; Ok(())
}

// ── a run ─────────────────────────────────────────────────────────────────────────────────────────────────────────────────

pub enum Ctl { Pause, Resume, Stop, Answer { node: String, approve: bool, reply: tokio::sync::oneshot::Sender<Result<(), String>> }, Edit { node: String, patch: Value, reply: tokio::sync::oneshot::Sender<Result<Value, String>> }, Alloc { moves: Vec<Value>, ceiling: Option<f64>, by_person: bool, reply: tokio::sync::oneshot::Sender<Result<Value, String>> }, Note { node: String, kind: String, text: String }, Restart { node: String, reply: tokio::sync::oneshot::Sender<Result<(), String>> }, Done(NodeMsg) }
pub struct NodeMsg { pub node: String, pub result: Result<NodeOut, NodeFail> }
pub struct NodeOut { pub outputs: Value, pub cost_usd: f64, pub transcript: String, pub ms: i64 }
pub struct NodeFail { pub kind: FailKind, pub error: String, pub cost_usd: f64, pub transcript: String }

#[derive(Clone)]
pub struct RunHandle { pub id: String, pub team: String, pub root: String, pub ctl: mpsc::UnboundedSender<Ctl>, pub snap: Arc<Mutex<Value>> }
fn runs() -> &'static Mutex<HashMap<String, RunHandle>> { static R: OnceLock<Mutex<HashMap<String, RunHandle>>> = OnceLock::new(); R.get_or_init(|| Mutex::new(HashMap::new())) }
pub fn handle(run: &str) -> Option<RunHandle> { runs().lock().unwrap_or_else(|p| p.into_inner()).get(run).cloned() }
pub fn live_runs(root: &str) -> Vec<RunHandle> { runs().lock().unwrap_or_else(|p| p.into_inner()).values().filter(|h| h.root == root).cloned().collect() }

/// How many headless calls one seat serves at once (a subscription has a rate limit): the rest wait, and show as waiting.
fn seat_gate(engine: &str) -> Arc<tokio::sync::Semaphore> {
    static G: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Semaphore>>>> = OnceLock::new();
    let m = G.get_or_init(|| Mutex::new(HashMap::new())); let n = std::env::var("ANTILOKI_TEAM_SEAT_PARALLEL").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(3).max(1);
    m.lock().unwrap_or_else(|p| p.into_inner()).entry(engine.to_string()).or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(n))).clone()
}

/// What a run is made of, on disk and in memory.
#[derive(Clone)]
pub struct RunInit { pub id: String, pub team: String, pub root: String, pub thread: String, pub graph: Graph, pub ask: String, pub engine: String, pub state: RunState, pub team_saga: i64, pub pack: BTreeMap<String, i64>, pub episodes: BTreeMap<String, i64>, pub trace_seq: usize, pub transcripts: BTreeMap<String, Vec<String>> }

fn tier_of(t: Tier) -> crate::cli::Tier { match t { Tier::Low => crate::cli::Tier::Low, Tier::Mid => crate::cli::Tier::Mid, Tier::High => crate::cli::Tier::High } }
/// The engine and model a role node's call gets: the node's own choice (`engine:model` or a model of the run's engine), else the tier's model on the run's engine. Pure over the tables.
pub fn binding_of(node: &Node, run_engine: crate::cli::Engine) -> (crate::cli::Engine, Option<String>) {
    let tier = node.tier_of().unwrap_or(Tier::Low);
    if let Kind::Role { model: Some(m), .. } = &node.kind { let m = m.trim(); if !m.is_empty() { if let Some((e, mm)) = m.split_once(':') { if let Some(en) = crate::cli::engine_of(&json!(e)) { return (en, Some(mm.to_string()).filter(|x| !x.is_empty())); } } return (run_engine, Some(m.to_string())); } }
    (run_engine, crate::cli::tier_model(run_engine, tier_of(tier)))
}
/// The files a step changed. `git diff HEAD` lists a brand-new file only once it is intent-added, so that comes first: without it a step that only CREATED files reported none (found by the real run of 2026-11-15; the Ledger said "nothing changed").
/// WHAT ONE STEP CHANGED (the operator's real run 2026-11-16: the Ledger said the reviewer, who only reads, changed 20 files: they were the builder's, still uncommitted in the shared folder): a file counts for a step when it is new or its size or time differs from what it was when the step began, and tool caches (a path with a hidden folder in it, node_modules) are not work. `before` is `file_marks` taken at the step's start. Pure over the marks.
pub fn file_marks(dir: &Path) -> std::collections::HashMap<String, (u64, u128)> {
    changed_names(dir).into_iter().filter_map(|n| { let m = std::fs::metadata(dir.join(&n)).ok()?; let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos()); Some((n, (m.len(), t))) }).collect()
}
pub fn is_tool_cache(path: &str) -> bool { path.split('/').any(|s| (s.starts_with('.') && s.len() > 1 && s != ".github" && s != ".gitignore" && s != ".env.example") || s == "node_modules") }
/// Lines in each file the folder holds that is text and not large (a step's net change is read from two of these: before and after). Capped so a big tree costs nothing. Blocking.
pub fn line_counts(dir: &Path, marks: &std::collections::HashMap<String, (u64, u128)>) -> std::collections::HashMap<String, usize> {
    marks.iter().filter(|(n, (sz, _))| *sz <= 1_000_000 && !is_tool_cache(n)).take(3000).filter_map(|(n, _)| std::fs::read(dir.join(n)).ok().filter(|b| !b.contains(&0)).map(|b| (n.clone(), b.iter().filter(|c| **c == b'\n').count() + usize::from(!b.is_empty() && b.last() != Some(&b'\n'))))).collect()
}
/// The net lines a step added and removed across the files it changed: per file the difference of its line count before and after. Pure over the two counts.
pub fn net_lines(before: &std::collections::HashMap<String, usize>, after: &std::collections::HashMap<String, usize>, files: &[String]) -> (usize, usize) {
    files.iter().fold((0, 0), |(a, r), f| { let (b, n) = (before.get(f).copied().unwrap_or(0), after.get(f).copied().unwrap_or(0)); (a + n.saturating_sub(b), r + b.saturating_sub(n)) })
}
pub fn step_files(dir: &Path, before: &std::collections::HashMap<String, (u64, u128)>) -> Vec<String> {
    let mut v: Vec<String> = file_marks(dir).into_iter().filter(|(n, m)| !is_tool_cache(n) && before.get(n) != Some(m)).map(|(n, _)| n).collect(); v.sort(); v
}
fn changed_names(dir: &Path) -> Vec<String> { let _ = crate::team_run::patch_of(dir); crate::team_run::names_of(dir).unwrap_or_default() }
/// Put back every change outside what a node owns (and every change at all of a read-only role): returns the files it put back. Blocking.
/// Put back what a step changed outside what it may touch. `before` is what was already changed when the step began (the work of the steps before it, in the same folder): that is never this step's to undo (found on the operator's video team 2026-11-15: a document-only reviewer owns no file, and its step put back the builder's whole `video/` folder). A file already changed before the step AND changed again by it is left as it is. Blocking.
fn put_back(dir: &Path, owns: &[String], allow_any: bool, before: &std::collections::HashSet<String>, arch: &crate::team_archive::Ctx) -> Vec<String> {
    let _ = crate::team_run::patch_of(dir);   // `git diff HEAD` lists a brand-new file only once it is intent-added: without this a new file outside the scope was never seen
    let names = changed_names(dir); let mut back = vec![];
    for f in names { if before.contains(&f) || crate::team_run::NOT_WORK.iter().any(|n| f == *n || f.starts_with(&format!("{n}/"))) { continue; } if allow_any || (!owns.is_empty() && crate::goals::owns_file(owns, &f)) { continue; }
        // NOTHING IS DELETED (the operator 2026-11-15): the file goes into the archive first, and if it cannot be saved it is left where it is
        if !crate::team_archive::store_before_removal(arch, &f, &dir.join(&f), "put back: outside what the step may touch") { continue; }
        let tracked = crate::boxes::git(&["ls-files", "--error-unmatch", &f], &dir.to_string_lossy()).is_ok();
        // a file that is in HEAD goes back to it; one that is not (new, or only intent-added) is taken out of the index and off the disk
        if tracked && crate::boxes::git(&["checkout", "HEAD", "--", &f], &dir.to_string_lossy()).is_ok() {} else { let _ = crate::boxes::git(&["rm", "-f", "-q", "--cached", "--ignore-unmatch", "--", &f], &dir.to_string_lossy()); let _ = std::fs::remove_file(dir.join(&f)); }
        back.push(f); }
    back
}

/// "Resets in 167h21m26s" as a person says it: `6 days 23 h`, `2 h 5 min`, `38 min`. Pure.
pub fn reset_words(ms: i64) -> String {
    let mins = (ms.max(0) + 59_999) / 60_000;
    if mins >= 2 * 24 * 60 { format!("{} days {} h", mins / (24 * 60), mins % (24 * 60) / 60) } else if mins >= 60 { format!("{} h {} min", mins / 60, mins % 60) } else { format!("{mins} min") }
}
/// What the person is told when a node's seat is out of quota and no other seat can take the call. It says WHOSE allowance ran out, when it comes back, and that the team's own budget is fine: they are different things. Pure.
pub fn limit_failure(engine_key: &str, why: &str, spent_usd: f64, cap_usd: Option<f64>) -> String {
    let back = crate::cli::limit_reset_in_ms(why).map(|m| format!(", and it comes back in {}", reset_words(m))).unwrap_or_default();
    let cap = cap_usd.map(|c| format!(" of its ${c:.2} cap")).unwrap_or_default();
    format!("the {engine_key} seat is out of quota{back}. That is the seat's own allowance on its subscription, not this team's budget (this node spent ${spent_usd:.2}{cap}). No other seat was available to take the call: sign another one in under Settings > Seats, or wait. The seat said: {}", why.chars().take(200).collect::<String>())
}

/// A SEAT THAT IS OUT OF QUOTA IS LEFT BEFORE THE CALL, AND A CALL THAT COMES BACK WITH A QUOTA MESSAGE IS TRIED ONCE ON THE NEXT SEAT (the goal runner has always done this; a team's role node did not: the operator's first real team
/// run died on "Individual quota reached ... Resets in 167h" with the budget barely touched). Each move is a note in the node's transcript; `engine` and `model` are left on the seat that answered.
async fn call_with_fallback(engine: &mut crate::cli::Engine, model: &mut Option<String>, msgs: &[crate::cli::ChatMessage], dir: &str, opts: crate::cli::RunOpts, notes: &mut Vec<String>, spent: f64, cap: Option<f64>) -> Result<crate::cli::RunOut, String> {
    use crate::cli::{clear_limit, model_blocked, note_limit, refresh_windows, run_cli, seat_limit, Job};
    refresh_windows(dir);
    let next = |not: crate::cli::Engine| crate::goals::alternate_seat(Job::EditorL, not, "l", None, None).map(|(e, m)| (e, m.filter(|x| !x.trim().is_empty())));
    let mut o = opts.clone(); o.model = model.clone();
    let (limited, off) = (seat_limit(*engine), model_blocked(*engine, o.model.as_deref()).is_some());
    if limited.is_some() || off {
        if let Some((n, m)) = next(*engine) {
            notes.push(match &limited { Some((_, said)) => format!("{} is out of quota ({}): this call goes to {} instead", engine.key(), said.chars().take(140).collect::<String>(), n.key()), None => format!("{} is switched off (Settings > Seats): this call goes to {} instead", engine.key(), n.key()) });
            *engine = n; *model = m.clone(); o.model = m;
        }
    }
    // a seat can also report its quota as an ERROR INSIDE a call that exited normally (Cursor: "Individual quota reached", found by the first timed Leader runs): that is the same as a refused call
    let first = run_cli(*engine, msgs, dir, false, o, None).await.and_then(|r| match r.parsed.error.as_deref() { Some(e) if crate::cli::is_limit_error(e) => Err(e.to_string()), _ => Ok(r) });
    match first {
        Ok(_) => { clear_limit(*engine); first }
        Err(why) if note_limit(*engine, &why) => {
            match next(*engine) {
                Some((n, m)) => {
                    notes.push(format!("{} said it is out of quota: this call goes to {} instead", engine.key(), n.key()));
                    *engine = n; *model = m.clone(); let mut o2 = opts; o2.model = m;
                    let second = run_cli(n, msgs, dir, false, o2, None).await.and_then(|r| match r.parsed.error.as_deref() { Some(e) if crate::cli::is_limit_error(e) => Err(e.to_string()), _ => Ok(r) });
                    match &second { Ok(_) => clear_limit(n), Err(w) => { if note_limit(n, w) { notes.push(format!("{} is out of quota too", n.key())); } } }
                    second.map_err(|w| if crate::cli::is_limit_error(&w) { limit_failure(n.key(), &w, spent, cap) } else { w })
                }
                None => Err(limit_failure(engine.key(), &why, spent, cap)),
            }
        }
        Err(e) => Err(e),
    }
}

async fn run_role(init: Arc<RunInit>, node_id: String, node: Node, values: Map<String, Value>, dir: PathBuf, n_try: u32) -> Result<NodeOut, NodeFail> {
    let Kind::Role { role, owns, agent, .. } = &node.kind else { return Err(NodeFail { kind: FailKind::Error, error: "not a role".into(), cost_usd: 0.0, transcript: String::new() }) };
    let role = *role; let run_engine = crate::cli::engine_of(&json!(init.engine)).unwrap_or_else(crate::cli::auto_engine);
    let (mut engine, mut model) = binding_of(&node, run_engine);
    let tag = format!("[team-graph {} · {} · {}]", init.id, node_id, n_try);
    let unit = unit_of(&init.graph, &values, &init.ask); let delta = delta_of(&values);
    let gate = seat_gate(engine.key()); let _permit = gate.acquire_owned().await.ok();
    let start = now(); let art_before = snapshot_artifacts(&dir); let mut cost = 0.0; let mut transcript = String::new();
    let write = node.is_writer() || agent.as_ref().map_or(false, |a| a.has("document"));   // a document agent may write in its box; whatever it changes outside antiloki's own folder is put back
    let before_marks = file_marks(&dir); let before_lines = line_counts(&dir, &before_marks); let before_names: std::collections::HashSet<String> = before_marks.keys().cloned().collect();   // what the steps before this one left in the folder: not this step's to put back
    let before = if write { String::new() } else { crate::team_run::patch_of(&dir).map(|p| crate::team_run::fingerprint(&p)).unwrap_or_default() };
    let mut prompt = match (role, agent) {
        (RoleKind::Agent, Some(a)) => { let (r2, t2, names) = (init.root.clone(), init.graph.team.clone(), a.skills.clone()); let pb = blocking(move || crate::team_skills::playbooks(&r2, &t2, &names)).await.unwrap_or_default(); agent_prompt(&tag, &init.ask, &unit, a, owns, &pb, &delta, &inputs_for(&init.graph, &values, &node_id)) }
        (RoleKind::Agent, None) => return Err(NodeFail { kind: FailKind::Error, error: "a custom agent has no brief".into(), cost_usd: 0.0, transcript }),
        _ => role_prompt(role, &tag, &init.ask, &unit, owns, &delta) };
    // A SPECIALIST NODE: the agent starts from the specialist's brief (persona, memory, tools) and its micro MCP gate; an approved specialist only
    let specialist: Option<(String, Vec<String>)> = match agent.as_ref().map(|a| a.specialist.clone()).filter(|x| !x.is_empty()) {
        Some(id) => { let (r2, i2, ask) = (init.root.clone(), id.clone(), init.ask.clone()); blocking(move || { let s = crate::specialists::get(&r2, &i2)?; if !crate::specialists::usable(&s) { return None; } crate::specialists::brief(&r2, &i2, &ask).ok().map(|b| (b, s.tools.clone())) }).await.flatten().map(|(b, tools)| { prompt = format!("{b}\n\n{prompt}"); (id, tools) }) }
        None => None };
    // STEERING (the person's notes while the run goes): added to every agent call that starts after the note was written
    { let notes = run_notes(&init.root, &init.id); if !notes.is_empty() { prompt = format!("{prompt}\n\n{}", notes_block(&notes)); } }
    let mut last_err = String::new();
    for attempt in 0..2 {
        if attempt == 1 { prompt = format!("{prompt}\n\nYour last reply could not be used ({last_err}). Answer again, in the form asked.") }
        let live: crate::cli::LiveTap = { let (r2, id2, n2) = (init.root.clone(), init.id.clone(), node_id.clone()); Arc::new(move |tool: &str, target: &str| live_push(&r2, &id2, &n2, tool, target)) };
        let line_tap: crate::cli::LineTap = { let (r2, id2, n2, d2) = (init.root.clone(), init.id.clone(), node_id.clone(), dir.display().to_string()); Arc::new(move |l: &str| crate::run_edits::append(&r2, &id2, &n2, &d2, l)) };
        let opts = crate::cli::RunOpts { live: Some(live), line_tap: Some(line_tap), write, max_turns: Some(40), timeout_ms: Some(node.timeout_s.map(|t| t * 1000).unwrap_or(1_800_000)), model: model.clone(), tier: Some(tier_of(node.tier_of().unwrap_or(Tier::Low))), extra_tools: { let mut t: Vec<String> = if agent.as_ref().map_or(false, |a| a.has("web")) { vec!["WebSearch".into(), "WebFetch".into()] } else { vec![] }; t.extend(crate::team_apps::allowed_tools(&app_ids_of(agent.as_ref()))); t.extend(antiloki_mcp_tools(agent.as_ref())); t }, mcp_config: app_mcp_config(&init.id, &init.root, &node_id, &app_ids_of(agent.as_ref()), !antiloki_mcp_tools(agent.as_ref()).is_empty() || specialist.is_some(), specialist.as_ref()), ledger: Some(ledger_ctx(&init, &node_id)), ..Default::default() };
        let msgs = vec![crate::cli::ChatMessage { role: "user".into(), content: prompt.clone() }];
        let mut notes: Vec<String> = vec![];
        let r = call_with_fallback(&mut engine, &mut model, &msgs, &dir.to_string_lossy(), opts, &mut notes, cost, node.cap_usd).await;
        if !notes.is_empty() { transcript.push_str(&format!("── seat ──\n{}\n\n", notes.join("\n"))); }
        let out = match r { Ok(o) => o, Err(e) => return Err(NodeFail { kind: FailKind::Error, error: e, cost_usd: cost, transcript }) };
        let c = out.parsed.cost.or_else(|| crate::cli::priced_from_tokens(engine, model.as_deref(), out.parsed.tokens_in, out.parsed.tokens_out)).unwrap_or(0.0); cost += c;
        if let Some(e) = &out.parsed.error { return Err(NodeFail { kind: FailKind::Error, error: e.clone(), cost_usd: cost, transcript }); }
        transcript.push_str(&format!("── prompt ──\n{prompt}\n\n── reply ({:.0}s, ${c:.4}) ──\n{}\n", out.ms as f64 / 1000.0, out.parsed.content));
        if !out.parsed.tools.is_empty() { transcript.push_str(&format!("\n── tools ──\n{}\n", out.parsed.tools.iter().map(|t| format!("{} {}", t.name, t.target)).collect::<Vec<_>>().join("\n"))); }
        let text = out.parsed.content.clone();
        let parsed: Result<Value, String> = match role {
            RoleKind::Planner => parse_pack(&text),
            RoleKind::Qa => parse_qa(&text, &unit.criteria),
            RoleKind::Audit => parse_audit(&text),
            // AN AGENT THAT SAYS IT COULD NOT FINISH HAS FAILED (found 2026-11-21: a pane designer blocked on a permission still let the run reach the person's approval gate with nothing to approve): the step fails with what the agent said, and the run stops where a failure stops
            RoleKind::Agent => { let v = parse_agent(&text, step_files(&dir, &before_marks)); if v["done"] == json!(false) { Err(format!("the agent said it could not finish: {}", v["report"].as_str().filter(|r| !r.is_empty()).unwrap_or("(it gave no reason)").chars().take(400).collect::<String>())) } else { Ok(v) } }
            RoleKind::Worker | RoleKind::Debugger => { let files = step_files(&dir, &before_marks); Ok(if role == RoleKind::Worker { json!({ "done": true, "files": files, "said": text.chars().take(400).collect::<String>() }) } else { json!({ "patched": !files.is_empty(), "files": files, "said": text.chars().take(400).collect::<String>() }) }) }
        };
        match parsed {
            Ok(mut outputs) => {
                // the gate: a role never leaves its mark outside what it may touch
                let arch = crate::team_archive::Ctx { root: init.root.clone(), run: init.id.clone(), team: init.graph.team.clone(), node: node_id.clone() }; let back = if write { put_back(&dir, owns, false, &before_names, &arch) } else { let after = crate::team_run::patch_of(&dir).map(|p| crate::team_run::fingerprint(&p)).unwrap_or_default(); if after != before { put_back(&dir, &[], false, &before_names, &arch) } else { vec![] } };
                if !back.is_empty() { transcript.push_str(&format!("\n── put back ──\n{}\n", back.join("\n"))); outputs["put_back"] = json!(back); }
                if let Some(fs) = outputs["files"].as_array() { let names: Vec<String> = fs.iter().filter_map(|x| x.as_str().map(|t| t.to_string())).collect(); if !names.is_empty() { let (a, r) = net_lines(&before_lines, &line_counts(&dir, &file_marks(&dir)), &names); outputs["lines"] = json!({ "added": a, "removed": r }); } }
                crate::team_media::keep(&init.root, &init.id, &dir); crate::team_archive::store_media(&arch, &dir); saga_tools(&init.root, &init.id, &node_id, init.team_saga, start);   // what the step made to watch or hear is the run's own from now on, whatever happens to the folder
                if role == RoleKind::Agent { let kept = collect_artifacts(&dir, &run_dir(&init.root, &init.id).join("artifacts").join(&node_id), &art_before); if !kept.is_empty() { transcript.push_str(&format!("\n── artifacts ──\n{}\n", kept.join("\n"))); outputs["artifacts"] = json!(kept); } }
                return Ok(NodeOut { outputs, cost_usd: cost, transcript, ms: now() - start });
            }
            Err(e) => { last_err = e; }
        }
    }
    Err(NodeFail { kind: FailKind::Verdict, error: format!("{} gave no usable answer twice: {last_err}", role.key()), cost_usd: cost, transcript })
}

/// WHAT AN AGENT WROTE THAT IS NOT CODE (§4.17): every plain file it wrote, during this call, in `.antiloki/artifacts/` of its folder is kept with the run (`artifacts/<node>/`), outside the code and the branch. Small files only; returns their names.
type ArtSnap = HashMap<String, (Option<std::time::SystemTime>, u64)>;
/// What is in an agent's artifacts folder now (name → modified, size), so that what a call wrote is what differs afterwards. Blocking.
fn snapshot_artifacts(from_dir: &Path) -> ArtSnap { std::fs::read_dir(from_dir.join(".antiloki").join("artifacts")).into_iter().flatten().flatten().filter_map(|e| { let m = e.metadata().ok()?; m.is_file().then(|| (e.file_name().to_string_lossy().to_string(), (m.modified().ok(), m.len()))) }).collect() }
fn collect_artifacts(from_dir: &Path, to: &Path, before: &ArtSnap) -> Vec<String> {
    let mut kept = vec![]; let src = from_dir.join(".antiloki").join("artifacts");
    for e in std::fs::read_dir(&src).into_iter().flatten().flatten() {
        let p = e.path(); let name = e.file_name().to_string_lossy().to_string();
        if !p.is_file() || artifact_ext(&name).is_none() || name.starts_with('.') { continue; }
        if std::fs::metadata(&p).map_or(true, |m| m.len() > ARTIFACT_MAX) { continue; }
        if before.get(&name) == std::fs::metadata(&p).ok().map(|m| (m.modified().ok(), m.len())).as_ref() { continue; }   // only what this call wrote or changed: agents of one saga share a folder
        if std::fs::create_dir_all(to).is_ok() && std::fs::copy(&p, to.join(&name)).is_ok() { kept.push(name); }
    }
    kept.sort(); kept
}
pub const ARTIFACT_MAX: u64 = 4_000_000;
/// The format a file name carries, if it is one of the artifact formats. Pure.
pub fn artifact_ext(name: &str) -> Option<&'static str> { let ext = name.rsplit_once('.')?.1.to_ascii_lowercase(); ARTIFACT_FORMATS.iter().copied().find(|f| *f == ext) }
/// A name that stays inside the artifacts folder: plain characters, no path. Pure.
pub fn plain_name(n: &str) -> bool { !n.is_empty() && n.len() <= 120 && !n.starts_with('.') && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') && !n.contains("..") }

/// The file a step wrote in its own folder, whatever its name, when it has the wanted format: the newest one if there are several. Pure over the folder.
pub fn written_file(dir: &std::path::Path, format: &str) -> Option<(std::path::PathBuf, String)> {
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf, String)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path(); if !p.is_file() { continue; }
        let n = e.file_name().to_string_lossy().to_string();
        if !n.to_ascii_lowercase().ends_with(&format!(".{format}")) { continue; }
        let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().map_or(true, |b| t > b.0) { best = Some((t, p, n)); }
    }
    best.map(|(_, p, n)| (p, n))
}
/// THE ARTIFACT NODE (§4.17): the document is what the node it comes from wrote, kept with the run. No model, no cost. Its own file (`<name>.<format>` among that node's artifacts) comes first; failing that, its `report`.
async fn run_artifact(init: Arc<RunInit>, node_id: String, node: Node, values: Map<String, Value>) -> Result<NodeOut, NodeFail> {
    let fail = |e: String| NodeFail { kind: FailKind::Error, error: e, cost_usd: 0.0, transcript: String::new() };
    let Kind::Artifact { name, format, from, title, compose } = &node.kind else { return Err(fail("not an artifact".into())) };
    if let Some(spec) = compose { return run_compose(init.clone(), spec.clone(), name.clone(), values).await; }
    if crate::team_graph::MEDIA_FORMATS.contains(&format.as_str()) {
        // A VIDEO, A PICTURE OR A SOUND: the file a step made, kept with the run so the app can play it
        let (root, run, name, format) = (init.root.clone(), init.id.clone(), name.clone(), format.clone()); let start = now();
        let r = blocking(move || -> Result<Value, String> {
            let file = format!("{name}.{format}"); let Some(from_path) = crate::team_media::find_named(&root, &run, &name, &format) else { return Err(format!("no file called {file} (or any .{format}) in the folders this run worked in: the step that makes it has not written it")) };
            let dest = crate::team_media::kept_dir(&root, &run); std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?; let to = dest.join(&file); let bytes = std::fs::copy(&from_path, &to).map_err(|e| format!("could not keep {file}: {e}"))?;
            Ok(json!({ "ok": true, "name": name, "format": format, "path": format!("media/{file}"), "bytes": bytes, "made_from": "the file in the working folder", "media": true }))
        }).await;
        return match r { Some(Ok(v)) => Ok(NodeOut { transcript: format!("kept {} ({} bytes)\n", v["path"].as_str().unwrap_or(""), v["bytes"]), outputs: v, cost_usd: 0.0, ms: now() - start }), Some(Err(e)) => Err(fail(e)), None => Err(fail("the artifact was cut off".into())) };
    }
    let Some(src) = from.clone() else { return Err(fail(format!("{node_id} says no node it is made from"))) };
    let (root, run, name, format, title) = (init.root.clone(), init.id.clone(), name.clone(), format.clone(), title.clone()); let start = now(); let file = format!("{name}.{format}");
    let reported = values.get(&src).and_then(|v| v["report"].as_str().filter(|t| !t.trim().is_empty()).or_else(|| v["said"].as_str().filter(|t| !t.trim().is_empty())).or_else(|| v["text"].as_str().filter(|t| !t.trim().is_empty()))).map(String::from);
    let r = blocking(move || -> Result<Value, String> {
        let art = run_dir(&root, &run).join("artifacts"); std::fs::create_dir_all(&art).map_err(|e| e.to_string())?;
        // THE FILE IT WROTE, UNDER ANY NAME (found 2026-11-25: a writer saved promo-script.md, the artifact asked for promo.md, and the team kept the writer's report instead of the script)
        let (text, how) = match std::fs::read_to_string(art.join(&src).join(&file)) { Ok(t) if !t.trim().is_empty() => (t, "the file it wrote".to_string()), _ => match written_file(&art.join(&src), &format) { Some((p, n)) => (std::fs::read_to_string(&p).map_err(|e| e.to_string())?, format!("the file it wrote ({n})")), None => match reported { Some(t) => (t, "its report".to_string()), None => return Err(format!("{src} wrote neither a file called {file} nor a report: there is nothing to make the artifact from")) } } };
        if text.len() as u64 > ARTIFACT_MAX { return Err(format!("{file} is over {} MB", ARTIFACT_MAX / 1_000_000)); }
        // an html artifact made from words alone is still a page
        let text = if format == "html" && !text.contains('<') { format!("<!doctype html><meta charset=\"utf-8\"><title>{}</title><pre style=\"font:14px/1.5 system-ui;white-space:pre-wrap;max-width:70ch;margin:2rem auto\">{}</pre>", title.clone().unwrap_or_else(|| name.clone()), text.replace('&', "&amp;").replace('<', "&lt;")) } else { text };
        std::fs::write(art.join(&file), &text).map_err(|e| e.to_string())?;
        Ok(json!({ "ok": true, "name": name, "format": format, "path": format!("artifacts/{file}"), "bytes": text.len(), "made_from": how, "title": title }))
    }).await;
    match r { Some(Ok(v)) => Ok(NodeOut { transcript: format!("artifact {} ({} bytes) made from {}\n", v["path"].as_str().unwrap_or(""), v["bytes"], v["made_from"].as_str().unwrap_or("")), outputs: v, cost_usd: 0.0, ms: now() - start }), Some(Err(e)) => Err(fail(e)), None => Err(fail("the artifact was cut off".into())) }
}
/// THE MODEL NODE (§4.4): fill the prompt from the run, one call (or a ladder of them), the typed answer as the node's outputs. A node that no seat could answer fails (its failure port, or the person).
async fn run_llm(init: Arc<RunInit>, node_id: String, node: Node, values: Map<String, Value>) -> Result<NodeOut, NodeFail> {
    let fail = |e: String| NodeFail { kind: FailKind::Error, error: e, cost_usd: 0.0, transcript: String::new() };
    let Kind::Llm(spec) = &node.kind else { return Err(fail("not a model node".into())) };
    let (spec, root, run, ask) = (spec.clone(), init.root.clone(), init.id.clone(), init.ask.clone()); let sp = spec.clone();
    let filled = blocking(move || -> Result<(String, String), String> {
        let get = |k: &str| -> Option<String> {
            if k == "ask" { return Some(ask.clone()); }
            if let Some(a) = k.strip_prefix("artifact:") { let (n, f) = match a.split_once('/') { Some((n, f)) => (Some(n), f), None => (None, a) }; return artifact_text(&root, &run, n, f).ok().map(|v| s(&v["text"])); }
            let (n, f) = k.split_once('.')?; let v = values.get(n)?.get(f)?; Some(match v { Value::String(t) => t.clone(), o => o.to_string() })
        };
        Ok((crate::team_llm::fill(&sp.prompt, &get)?, if sp.verify.source.is_empty() { String::new() } else { crate::team_llm::fill(&sp.verify.source, &get)? }))
    }).await;
    let (prompt, source) = match filled { Some(Ok(p)) => p, Some(Err(e)) => return Err(fail(format!("{node_id}: {e}"))), None => return Err(fail("the prompt was cut off".into())) };
    let llm = crate::team_llm::run(&spec, &prompt, &source, Some(ledger_ctx(&init, &node_id))).await;
    match llm {
        Ok(o) => {
            let mut out = o.fields.clone(); out.insert("ok".into(), json!(true)); out.insert("schema_ok".into(), json!(true)); out.insert("seat_used".into(), json!(o.seat_used)); out.insert("escalated_from".into(), json!(o.escalated_from.clone().unwrap_or_default()));
            if let Some(w) = &o.why_escalated { out.insert("why_escalated".into(), json!(w)); } out.insert("verified".into(), json!(o.verified)); if let Some(a) = o.agreed { out.insert("agreed".into(), json!(a)); } out.insert("calls".into(), json!(o.calls));
            let mut tr = format!("{} answered ({} call{}, {} ms)", o.seat_used, o.calls, if o.calls == 1 { "" } else { "s" }, o.ms); if let Some(f) = &o.escalated_from { tr.push_str(&format!("; climbed from {f} because {}", o.why_escalated.clone().unwrap_or_default())); } tr.push('\n');
            Ok(NodeOut { transcript: tr, outputs: Value::Object(out), cost_usd: o.usd, ms: o.ms as i64 })
        }
        Err(e) => Err(fail(e)),
    }
}
/// The installed apps an agent asked for (`app-<id>` in its `mcp`). The CLI is allowed those servers and given their addresses: sign-in stays the CLI's own.
fn app_ids_of(a: Option<&AgentSpec>) -> Vec<String> { let mut v: Vec<String> = a.map(|a| a.mcp.iter().filter_map(|m| crate::team_apps::app_of(m)).collect()).unwrap_or_default(); v.sort(); v.dedup(); v }
fn app_mcp_config(run: &str, root: &str, node: &str, ids: &[String], antiloki_tools: bool, specialist: Option<&(String, Vec<String>)>) -> Option<String> {
    let mut cfg = if ids.is_empty() { json!({ "mcpServers": {} }) } else { crate::team_apps::mcp_config_for(ids, &antiloki_rs::cockpit__helpers_webapps::load()) };
    // THE ANTILOKI SERVER FOR A TEAM AGENT (found 2026-11-21: "ANTILOKI_PANE_ID is not set — this server must be spawned from a pane worktree config"): a team agent has no pane, so it is given a number of its own, above every real pane's. It is an agent to the serve (it cannot approve, write to an app or open a link on its own).
    if antiloki_tools { let pid = 910_000 + (run.bytes().chain(node.bytes()).fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32)) % 80_000); if let Some(m) = cfg["mcpServers"].as_object_mut() { m.insert("antiloki".into(), json!({ "command": crate::mcp_install::agent_bin(), "args": ["mcp"], "env": { "ANTILOKI_BE_URL": format!("http://127.0.0.1:{}", serve_port()), "ANTILOKI_PANE_ID": pid.to_string(), "ANTILOKI_SUBAGENT": "0" } })); if let (Some((id, tools)), Some(e)) = (specialist, m.get_mut("antiloki").and_then(|x| x.get_mut("env")).and_then(|e| e.as_object_mut())) { e.insert("ANTILOKI_SPECIALIST".into(), json!(id)); e.insert("ANTILOKI_SPECIALIST_TOOLS".into(), json!(tools.join(","))); } } }
    if cfg["mcpServers"].as_object().map_or(true, |m| m.is_empty()) { return None; }
    let p = run_dir(root, run).join(format!("mcp-{node}.json")); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); } std::fs::write(&p, cfg.to_string()).ok()?; Some(p.display().to_string())
}
/// THE CHECK NODE (§4.6): the rules against the text the node names. No model, no cost. A source that cannot be read is a FAILED check saying so (never a crash): the graph decides what that means.
/// The contract node a decompose node names (written by the leader): its spec, read from the graph.
fn contract_of(init: &RunInit, spec: &crate::team_decompose::DecomposeSpec) -> Option<crate::team_contract::ContractSpec> { spec.contract.as_ref().and_then(|id| init.graph.node(id)).and_then(|n| match &n.kind { Kind::Contract(c) => Some(c.clone()), _ => None }) }
/// The review node a decompose node names (rules written by the leader): its spec, read from the graph.
fn review_of(init: &RunInit, spec: &crate::team_decompose::DecomposeSpec) -> Option<crate::team_review::ReviewSpec> { spec.review.as_ref().and_then(|id| init.graph.node(id)).and_then(|n| match &n.kind { Kind::Review(c) => Some(c.clone()), _ => None }) }
/// The test node a decompose node names: its spec (the coder sees the files unless it is a holdout).
fn tests_of(init: &RunInit, spec: &crate::team_decompose::DecomposeSpec) -> Option<crate::team_test::TestSpec> { spec.tests.as_ref().and_then(|id| init.graph.node(id)).and_then(|n| match &n.kind { Kind::Test(c) => Some(c.clone()), _ => None }) }
fn serve_port() -> u16 { std::env::var("ANTILOKI_SERVE_PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(41998) }
fn sub_of(line: &str) -> Option<crate::team_decompose::Subtask> { line.split_once(" :: ").map(|(p, w)| crate::team_decompose::Subtask { path: p.trim().to_string(), what: w.trim().to_string() }) }
/// THE DECOMPOSE NODE (RFC 0014): the planner seat plans, the coder seat writes each file, code does the rest. Some files failing is an `ok: false` answer (a recurse node reads it); no files at all, or no plan, is a failure.
async fn run_decompose(init: Arc<RunInit>, node_id: String, node: Node, dir: PathBuf) -> Result<NodeOut, NodeFail> {
    let fail = |e: String, cost: f64| NodeFail { kind: FailKind::Error, error: e, cost_usd: cost, transcript: String::new() };
    let Kind::Decompose(spec) = &node.kind else { return Err(fail("not a decompose node".into(), 0.0)) }; let start = now();
    let d = crate::team_decompose::run(serve_port(), &dir, &init.ask, spec, contract_of(&init, spec).as_ref(), review_of(&init, spec).as_ref(), tests_of(&init, spec).as_ref(), 0, None, Some(ledger_ctx(&init, &node_id))).await.map_err(|e| fail(e, 0.0))?;
    if d.files.is_empty() { return Err(NodeFail { kind: FailKind::Verdict, error: format!("no file was written: {}", d.failed.iter().map(|f| format!("{}: {}", f["path"].as_str().unwrap_or(""), f["why"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("; ")), cost_usd: d.usd, transcript: format!("{}\n", d.subtasks.join("\n")) }); }
    let text = format!("{} of {} files written: {}", d.files.len(), d.subtasks.len(), d.files.join(", "));
    Ok(NodeOut { outputs: json!({ "ok": d.failed.is_empty(), "subtasks": d.subtasks, "files": d.files, "failed": d.failed, "text": text, "seats": d.seats, "checks": d.checks }), cost_usd: d.usd, transcript: format!("{text}\n{}\n", d.checks.join("\n")), ms: now() - start })
}
/// THE RECURSE NODE (RFC 0014): decompose, judge, and on a fail redo only the files the failure points at with the failure as input. Every decision here is code: which lines say what broke, which files they name, whether it is the same failure again. It ends when the judge passes, at `max_depth`, or when the failure repeats.
async fn run_recurse(init: Arc<RunInit>, node_id: String, node: Node, values: Map<String, Value>, dir: PathBuf) -> Result<NodeOut, NodeFail> {
    let fail = |kind: FailKind, e: String, cost: f64, tr: String| NodeFail { kind, error: e, cost_usd: cost, transcript: tr };
    let Kind::Recurse { worker, judge, max_depth } = &node.kind else { return Err(fail(FailKind::Error, "not a recurse node".into(), 0.0, String::new())) }; let start = now();
    let (Some(wn), Some(jn)) = (init.graph.node(worker).cloned(), init.graph.node(judge).cloned()) else { return Err(fail(FailKind::Error, "the worker or the judge is not there".into(), 0.0, String::new())) };
    let Kind::Decompose(spec) = &wn.kind else { return Err(fail(FailKind::Error, "the worker is not a decompose node".into(), 0.0, String::new())) };
    let (mut cost, mut tr) = (0.0, String::new()); let (mut redo, mut last_sig, mut stopped) = (None::<(Vec<crate::team_decompose::Subtask>, String)>, String::new(), "depth".to_string());
    let (mut plan, mut files, mut judge_out, mut attempts): (Vec<crate::team_decompose::Subtask>, Vec<String>, Value, u32) = (vec![], vec![], json!({}), 0); let mut rung = 0usize; let mut eff_contract = contract_of(&init, spec);
    for depth in 1..=*max_depth {
        attempts = depth;
        let d = crate::team_decompose::run(serve_port(), &dir, &init.ask, spec, eff_contract.as_ref(), review_of(&init, spec).as_ref(), tests_of(&init, spec).as_ref(), rung, redo.clone(), Some(ledger_ctx(&init, worker))).await.map_err(|e| fail(FailKind::Error, e, cost, tr.clone()))?;
        cost += d.usd; if eff_contract.is_none() { eff_contract = d.contract.clone(); } if redo.is_none() { plan = d.subtasks.iter().filter_map(|l| sub_of(l)).collect(); } for f in &d.files { if !files.contains(f) { files.push(f.clone()); } }
        tr.push_str(&format!("pass {depth}: plan [{}] · {} files written, {} failed\n{}", d.subtasks.join(" | "), d.files.len(), d.failed.len(), d.failed.iter().map(|f| format!("  failed {}: {}\n", f["path"].as_str().unwrap_or(""), f["why"].as_str().unwrap_or(""))).collect::<String>()));
        // NOTHING WRITTEN IS NOT A PASS: the tests that were already there would pass on an untouched tree. The failures are the feedback for the next pass.
        if d.files.is_empty() {
            let why = d.failed.iter().map(|f| format!("{}: {}", f["path"].as_str().unwrap_or(""), f["why"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n");
            let sig = crate::team_recurse::signature_of(&why); judge_out = json!({ "ok": false, "tail": why });
            if !sig.is_empty() && sig == last_sig { stopped = "no progress: the same failure again".into(); break; }
            last_sig = sig; if depth == *max_depth { stopped = "depth".into(); break; }
            redo = Some((plan.clone(), format!("Nothing could be written last time:\n{why}"))); continue;
        }
        let mut v = values.clone(); v.insert(worker.clone(), json!({ "ok": d.failed.is_empty(), "files": files, "text": d.files.join(", ") }));
        let jr = match &jn.kind { Kind::UseTool { .. } => run_tool(init.clone(), judge.clone(), jn.clone(), dir.clone(), v).await, Kind::Test(sp) => match crate::team_test::run(&dir, sp).await { Ok(res) => { let out = json!({ "ok": res.ok, "exit": res.exit, "total": res.total, "passed": res.passed, "failed": res.failed, "tail": res.tail, "blocks": crate::team_recurse::blocks_for(&sp.files, &res.failed) }); if res.ok { Ok(NodeOut { outputs: out, cost_usd: 0.0, transcript: String::new(), ms: 0 }) } else { Ok(NodeOut { outputs: out, cost_usd: 0.0, transcript: String::new(), ms: 0 }) } } Err(e) => Err(NodeFail { kind: FailKind::Error, error: e, cost_usd: 0.0, transcript: String::new() }) }, Kind::Contract(_) => run_contract(init.clone(), judge.clone(), jn.clone(), dir.clone()).await, Kind::Review(_) => run_review(init.clone(), judge.clone(), jn.clone(), dir.clone()).await, Kind::Skeleton(_) => run_skeleton(init.clone(), judge.clone(), jn.clone(), dir.clone()).await, _ => run_check(init.clone(), judge.clone(), jn.clone(), v).await };
        let (out, jcost) = match jr { Ok(o) => (o.outputs, o.cost_usd), Err(f) => (json!({ "ok": false, "tail": format!("{}\n{}", f.error, f.transcript) }), f.cost_usd) }; cost += jcost;
        if crate::team_recurse::judged_ok(&out) { stopped = "passed".into(); judge_out = out; break; }
        let tail = out["tail"].as_str().or(out["text"].as_str()).unwrap_or("").to_string(); judge_out = out;
        let sig = crate::team_recurse::signature_of(&tail); tr.push_str(&format!("judge failed (signature {}):\n{}\n", if sig.is_empty() { "-" } else { &sig }, crate::team_recurse::feedback_of(&tail)));
        // THE SAME FAILURE AGAIN: climb to the next seat of the node's ladder; stop only when there is none left
        if !sig.is_empty() && sig == last_sig { if rung < spec.fallback.len() { rung += 1; tr.push_str(&format!("the same failure again: climbing to {}\n", spec.fallback[rung - 1])); } else { stopped = "no progress: the same failure again".into(); break; } }
        last_sig = sig; if depth == *max_depth { stopped = "depth".into(); break; }
        let blamed = crate::team_recurse::blame(&tail, &files); let subs: Vec<_> = if blamed.is_empty() { plan.clone() } else { plan.iter().filter(|s| blamed.contains(&s.path)).cloned().collect() };
        let blocks = judge_out["blocks"].as_str().unwrap_or("").to_string(); let sym_note = { let d2 = dir.clone(); crate::team_recurse::symbol_note(&crate::team_recurse::blame_symbols(&tail, &files, &move |p| std::fs::read_to_string(d2.join(p)).ok())) };
        redo = Some((if subs.is_empty() { plan.clone() } else { subs }, format!("{}{}{}{}", crate::team_recurse::feedback_of(&tail), if sym_note.is_empty() { String::new() } else { format!("\n{sym_note}") }, { let h = crate::team_recurse::hints_of(&tail); if h.is_empty() { String::new() } else { format!("\n\nWhat these errors mean:\n{}", h.iter().map(|x| format!("- {x}")).collect::<Vec<_>>().join("\n")) } }, if blocks.is_empty() { String::new() } else { format!("\n\nThe failing tests (the behaviour expected):\n{blocks}") })));
    }
    if stopped != "passed" { return Err(fail(FailKind::Verdict, format!("the judge did not pass ({stopped}) after {attempts} pass(es)"), cost, tr)); }
    Ok(NodeOut { outputs: json!({ "ok": true, "depth": attempts, "attempts": attempts, "stopped": stopped, "files": files, "judge": judge_out }), cost_usd: cost, transcript: tr, ms: now() - start })
}
/// THE EVOLVE NODE (RFC 0014): reached when a run fails in its section. Counts the failures in a row (this one and the runs before it) and, at `after_failures`, tells the leader the improver is due for THAT SECTION only. Nothing here calls a model; the improver proposes, the replay bank judges, the person approves.
async fn run_evolve(init: Arc<RunInit>, node_id: String, node: Node) -> Result<NodeOut, NodeFail> {
    let Kind::Evolve { scope, after_failures } = &node.kind else { return Err(NodeFail { kind: FailKind::Error, error: "not an evolve node".into(), cost_usd: 0.0, transcript: String::new() }) }; let start = now();
    let (root, team, run) = (init.root.clone(), init.graph.team.clone(), init.id.clone());
    let before = blocking(move || runs_of(&root, &team).into_iter().filter(|r| r["id"] != json!(run)).take_while(|r| matches!(r["status"]["status"].as_str(), Some("blocked") | Some("stopped"))).count()).await.unwrap_or(0) as u32;
    let streak = before + 1; let due = streak >= *after_failures;
    if due { append_jsonl(&team_files(&init.root, &init.graph.team).join("leader-inbox.jsonl"), &json!({ "id": format!("i-{}", now()), "t_ms": now(), "status": "unread", "kind": "improver_due", "scope": scope, "message": format!("[improver due: {streak} failure(s) in a row in the section {}] Read `antiloki_team_audit` op `improve_prompt` with `scope` {}, propose ONE small change to those nodes only, and send it with op `improve_request` and the same `scope`.", scope.join(", "), json!(scope)) })); }
    Ok(NodeOut { outputs: json!({ "ok": true, "due": due, "scope": scope, "failures_in_a_row": streak }), cost_usd: 0.0, transcript: format!("evolve {node_id}: {streak} failure(s) in a row; {}\n", if due { "the improver is due for the section" } else { "not due yet" }), ms: now() - start })
}
/// THE TEST NODE: the leader's tests over a sandbox copy of the tree. A failing test is a `failed` verdict (the guard says no) carrying the names and the tail, which is what a recurse node feeds back.
async fn run_test(_init: Arc<RunInit>, node_id: String, node: Node, dir: PathBuf) -> Result<NodeOut, NodeFail> {
    let Kind::Test(sp) = &node.kind else { return Err(NodeFail { kind: FailKind::Error, error: "not a test node".into(), cost_usd: 0.0, transcript: String::new() }) }; let start = now();
    let res = crate::team_test::run(&dir, sp).await.map_err(|e| NodeFail { kind: FailKind::Error, error: e, cost_usd: 0.0, transcript: String::new() })?;
    let out = json!({ "ok": res.ok, "exit": res.exit, "total": res.total, "passed": res.passed, "failed": res.failed, "tail": res.tail });
    let tr = format!("test {node_id}: {} of {} passed{}\n{}\n", res.passed, res.total, if res.failed.is_empty() { String::new() } else { format!(" · failed: {}", res.failed.join(", ")) }, res.tail);
    // THE TESTS A TEAM PASSED BELONG TO THE CODEBASE (the operator 2026-11-19: "any code they do should be on the code basis of this work tree"): a judge stays out of the tree while the coder works, and once the work has passed it, the (visible) test files are kept in the tree, where X-ray and git see them. A hidden holdout is never written.
    let kept = if res.ok && !sp.hidden { keep_tests(&dir, &sp.files) } else { vec![] }; let tr = if kept.is_empty() { tr } else { format!("{tr}\nkept in the tree: {}\n", kept.join(", ")) };
    if res.ok { Ok(NodeOut { outputs: out, cost_usd: 0.0, transcript: tr, ms: now() - start }) } else { Err(NodeFail { kind: FailKind::Verdict, error: format!("the tests disapprove: {}", if res.failed.is_empty() { format!("exit {}", res.exit) } else { res.failed.join(", ") }), cost_usd: 0.0, transcript: tr }) }
}
/// Write the test files a passing test node ran into the work tree, unless the tree already holds a file there. Returns what it wrote. Blocking, small.
pub fn keep_tests(dir: &Path, files: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    let mut out = vec![]; for (p, c) in files { if crate::team_apply::clean(p).is_err() { continue; } let to = dir.join(p); if to.exists() { continue; } if let Some(d) = to.parent() { if std::fs::create_dir_all(d).is_err() { continue; } } if std::fs::write(&to, c).is_ok() { out.push(p.clone()); } } out
}
/// THE CONTRACT NODE: the leader's contract checked over the tree. A violation is a `failed` verdict carrying one line per problem, path first (a recurse node blames those files).
async fn run_contract(_init: Arc<RunInit>, node_id: String, node: Node, dir: PathBuf) -> Result<NodeOut, NodeFail> {
    let Kind::Contract(sp) = &node.kind else { return Err(NodeFail { kind: FailKind::Error, error: "not a contract node".into(), cost_usd: 0.0, transcript: String::new() }) }; let start = now();
    let (d2, sp2) = (dir.clone(), sp.clone()); let issues = blocking(move || crate::team_contract::enforce_tree(&d2, &sp2)).await.unwrap_or_default();
    let tail = issues.join("\n");
    if issues.is_empty() { Ok(NodeOut { outputs: json!({ "ok": true, "issues": [], "tail": "" }), cost_usd: 0.0, transcript: format!("contract {node_id}: every file conforms\n"), ms: now() - start }) }
    else { Err(NodeFail { kind: FailKind::Verdict, error: format!("the contract is not kept: {} problem(s)", issues.len()), cost_usd: 0.0, transcript: format!("contract {node_id}:\n{tail}\n") }) }
}
/// THE STUBS NODE: the skeleton of the code, written before any logic, from the leader's contract.
async fn run_stubs(init: Arc<RunInit>, node_id: String, node: Node, dir: PathBuf) -> Result<NodeOut, NodeFail> {
    let fail = |e: String, cost: f64| NodeFail { kind: FailKind::Error, error: e, cost_usd: cost, transcript: String::new() };
    let Kind::Stubs(sp) = &node.kind else { return Err(fail("not a stubs node".into(), 0.0)) }; let start = now();
    let Some(Kind::Contract(c)) = init.graph.node(&sp.contract).map(|n| n.kind.clone()) else { return Err(fail("the contract node is not there".into(), 0.0)) };
    let m = crate::team_stubs::run(serve_port(), &dir, &init.ask, sp, &c, Some(ledger_ctx(&init, &node_id))).await.map_err(|e| fail(e, 0.0))?;
    if m.files.is_empty() { return Err(NodeFail { kind: FailKind::Verdict, error: format!("no stub was written: {}", m.failed.iter().map(|f| format!("{}: {}", f["path"].as_str().unwrap_or(""), f["why"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("; ")), cost_usd: m.usd, transcript: String::new() }); }
    let text = format!("{} stub file(s): {}", m.files.len(), m.files.join(", "));
    Ok(NodeOut { outputs: json!({ "ok": m.failed.is_empty(), "files": m.files, "functions": m.functions, "failed": m.failed, "deterministic": m.deterministic, "text": text }), cost_usd: m.usd, transcript: format!("{text}\n{}\n", m.functions.join("\n")), ms: now() - start })
}
/// THE REVIEW NODE: X-ray's laws and the leader's rules over the files this node's tree changed. Errors fail the node with one line per issue (path:line [rule] message); warnings are reported and do not fail it.
async fn run_review(_init: Arc<RunInit>, node_id: String, node: Node, dir: PathBuf) -> Result<NodeOut, NodeFail> {
    let Kind::Review(sp) = &node.kind else { return Err(NodeFail { kind: FailKind::Error, error: "not a review node".into(), cost_usd: 0.0, transcript: String::new() }) }; let start = now();
    let issues = crate::team_review::run(serve_port(), &dir, sp).await.map_err(|e| NodeFail { kind: FailKind::Error, error: e, cost_usd: 0.0, transcript: String::new() })?;
    let errors = issues.iter().filter(|i| i.severity == "error").count(); let tail = issues.iter().map(|i| i.line_text()).collect::<Vec<_>>().join("\n");
    if errors == 0 { Ok(NodeOut { outputs: json!({ "ok": true, "issues": issues, "errors": 0, "tail": tail }), cost_usd: 0.0, transcript: format!("review {node_id}: {} warning(s), no error\n{tail}\n", issues.len()), ms: now() - start }) }
    else { Err(NodeFail { kind: FailKind::Verdict, error: format!("the review found {errors} error(s)"), cost_usd: 0.0, transcript: format!("review {node_id}:\n{tail}\n") }) }
}
/// THE ENFORCE NODE: the raw text from `from`, repaired by code, restructured by a tiny model only if code could not, validated by code.
async fn run_enforce(init: Arc<RunInit>, node_id: String, node: Node, values: Map<String, Value>) -> Result<NodeOut, NodeFail> {
    let Kind::Enforce(sp) = &node.kind else { return Err(NodeFail { kind: FailKind::Error, error: "not an enforce node".into(), cost_usd: 0.0, transcript: String::new() }) }; let start = now();
    let raw = if sp.from == "ask" { init.ask.clone() } else { sp.from.split_once('.').and_then(|(n, f)| values.get(n)?.get(f)).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).unwrap_or_default() };
    match crate::team_enforce::run(sp, &raw, Some(ledger_ctx(&init, &node_id))).await {
        Ok(e) => { let mut o = e.fields.clone(); o.insert("ok".into(), json!(true)); o.insert("repaired_by".into(), json!(e.by)); Ok(NodeOut { outputs: Value::Object(o), cost_usd: e.usd, transcript: format!("enforce {node_id}: valid, repaired by {}\n", e.by), ms: now() - start }) }
        Err(e) => Err(NodeFail { kind: FailKind::Verdict, error: format!("the enforcer could not make a valid answer: {e}"), cost_usd: 0.0, transcript: format!("raw text:\n{}\n", raw.chars().take(1500).collect::<String>()) }),
    }
}
/// THE SKELETON NODE: the plan views and the conformance of the tree to the leader's function-level contract. Issues fail the node with one line each (path first).
async fn run_skeleton(init: Arc<RunInit>, node_id: String, node: Node, dir: PathBuf) -> Result<NodeOut, NodeFail> {
    let Kind::Skeleton(sp) = &node.kind else { return Err(NodeFail { kind: FailKind::Error, error: "not a skeleton node".into(), cost_usd: 0.0, transcript: String::new() }) }; let start = now();
    let Some(Kind::Contract(c)) = init.graph.node(&sp.contract).map(|n| n.kind.clone()) else { return Err(NodeFail { kind: FailKind::Error, error: "the contract node is not there".into(), cost_usd: 0.0, transcript: String::new() }) };
    let (plan, issues) = blocking(move || crate::team_skeleton::run(&dir, &c)).await.unwrap_or_default();
    let (new_only, merged, diff) = crate::team_skeleton::views(&plan); let tail = issues.join("\n");
    if issues.is_empty() { Ok(NodeOut { outputs: json!({ "ok": true, "issues": [], "diff": diff, "merged": merged, "new_only": new_only, "tail": "" }), cost_usd: 0.0, transcript: format!("skeleton {node_id}: the code conforms to the plan\n{diff}\n"), ms: now() - start }) }
    else { Err(NodeFail { kind: FailKind::Verdict, error: format!("the code does not conform to the plan: {} issue(s)", issues.len()), cost_usd: 0.0, transcript: format!("skeleton {node_id}:\n{tail}\n") }) }
}
/// A CELL (one task of the family): the input from `from`, the model proposes, the task's guards judge.
async fn run_cell(init: Arc<RunInit>, node_id: String, node: Node, values: Map<String, Value>) -> Result<NodeOut, NodeFail> {
    let Kind::Cell(sp) = &node.kind else { return Err(NodeFail { kind: FailKind::Error, error: "not a cell node".into(), cost_usd: 0.0, transcript: String::new() }) }; let start = now();
    let input = if sp.from == "ask" { init.ask.clone() } else { sp.from.split_once('.').and_then(|(n, f)| values.get(n)?.get(f)).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).unwrap_or_default() };
    match crate::team_cell::run(sp, &input, Some(ledger_ctx(&init, &node_id))).await {
        Ok(d) => Ok(NodeOut { outputs: json!({ "ok": true, "output": d.output, "tries": d.tries }), cost_usd: d.usd, transcript: format!("{} {node_id}: accepted after {} try(ies)\n", sp.task, d.tries), ms: now() - start }),
        Err(e) => Err(NodeFail { kind: FailKind::Verdict, error: format!("the {} cell: {e}", sp.task), cost_usd: 0.0, transcript: String::new() }),
    }
}
async fn run_check(init: Arc<RunInit>, node_id: String, node: Node, values: Map<String, Value>) -> Result<NodeOut, NodeFail> {
    let Kind::Check { on, rules } = &node.kind else { return Err(NodeFail { kind: FailKind::Error, error: "not a check".into(), cost_usd: 0.0, transcript: String::new() }) };
    let (on, rules, root, run) = (on.clone(), rules.clone(), init.root.clone(), init.id.clone()); let start = now();
    let out = blocking(move || -> Value {
        let files: Vec<(String, u64)> = artifacts_of(&root, &run).iter().map(|a| (match a["node"].as_str() { Some(n) => format!("{n}/{}", s(&a["name"])), None => s(&a["name"]) }, a["bytes"].as_u64().unwrap_or(0))).collect();
        let (kind, rest) = on.split_once(':').unwrap_or(("", on.as_str()));
        let text: Result<String, String> = match kind {
            "artifact" => { let (n, f) = match rest.split_once('/') { Some((n, f)) => (Some(n), f), None => (None, rest) }; artifact_text(&root, &run, n, f).map(|v| s(&v["text"])) }
            "agent" | "web" | "values" => { let (n, f) = rest.split_once('.').unwrap_or((rest, "report")); values.get(n).ok_or_else(|| format!("{n} has not run")).and_then(|v| match &v[f] { Value::Null => Err(format!("{n} wrote no `{f}`")), Value::String(t) => Ok(t.clone()), o => Ok(o.to_string()) }) }
            _ => Err(format!("`{on}` is not something to check")),
        };
        match text { Ok(t) => crate::team_check::evaluate(&rules, &t, &files).to_json(), Err(e) => json!({ "ok": true, "pass": false, "failed": [{ "rule": "source", "message": format!("there is nothing to check: {e}") }], "passed": [], "score": 0.0 }) }
    }).await.unwrap_or_else(|| json!({ "ok": true, "pass": false, "failed": [{ "rule": "source", "message": "the check was cut off" }], "passed": [], "score": 0.0 }));
    let (pass, n) = (out["pass"] == json!(true), out["failed"].as_array().map_or(0, |a| a.len()));
    Ok(NodeOut { transcript: format!("check {}: {}\n", if pass { "passed" } else { "failed" }, if pass { "every rule holds".to_string() } else { format!("{n} rule(s) not met\n{}", crate::team_check::evidence(&out)) }), outputs: out, cost_usd: 0.0, ms: now() - start })
}

fn value_ref(values: &Map<String, Value>, from: &Option<String>) -> Result<Value, String> {
    let Some(f) = from.as_ref().filter(|s| !s.is_empty()) else { return Ok(json!({})); };
    let (kind, rest) = f.split_once(':').ok_or_else(|| format!("`{f}` is not a value reference"))?;
    match kind {
        "values" => {
            // `values:<node>` = the node's whole answer; a dotted path walks into it (`values:rules.output.title`)
            let mut parts = rest.split('.'); let n = parts.next().unwrap_or("");
            let mut cur = values.get(n).ok_or_else(|| format!("{n} has not run"))?;
            for field in parts { cur = cur.get(field).ok_or_else(|| format!("{n} wrote no `{field}`"))?; }
            Ok(cur.clone())
        }
        _ => Err(format!("`{f}` must be values:<node>.<field>")),
    }
}

async fn run_code(_init: Arc<RunInit>, node_id: String, node: Node, values: Map<String, Value>) -> Result<NodeOut, NodeFail> {
    let Kind::Code { source, from, limits, .. } = &node.kind else { return Err(NodeFail { kind: FailKind::Error, error: "not a code node".into(), cost_usd: 0.0, transcript: String::new() }) };
    let start = now();
    let (source, from, limits) = (source.clone(), from.clone(), limits.clone().unwrap_or_default());
    let out = blocking(move || {
        let input = value_ref(&values, &from)?;
        let output = crate::team_code::run(&source, &input, &limits)?;
        Ok(json!({ "ok": true, "output": output }))
    }).await;
    match out {
        Some(Ok(v)) => Ok(NodeOut { transcript: format!("code {node_id} finished\n"), outputs: v, cost_usd: 0.0, ms: now() - start }),
        Some(Err(e)) => Err(NodeFail { kind: FailKind::Error, error: e, cost_usd: 0.0, transcript: String::new() }),
        None => Err(NodeFail { kind: FailKind::Error, error: "the code node was cut off".into(), cost_usd: 0.0, transcript: String::new() }),
    }
}

/// SUB-TEAM: start the inner team as its own run, wait for it, map its values out. The inner run keeps its ledger rows and audit under the inner team; this node records its id and takes its cost as its own, so the budgets add up.
async fn run_subteam(init: Arc<RunInit>, node_id: String, node: Node, values: Map<String, Value>) -> Result<NodeOut, NodeFail> {
    let fail = |e: String, cost: f64| NodeFail { kind: FailKind::Error, error: e, cost_usd: cost, transcript: String::new() };
    let Kind::SubTeam { team, ask, outputs, budget_share, .. } = &node.kind else { return Err(fail("not a sub-team node".into(), 0.0)) };
    let start_ms = now();
    // the static check once more, at the door of the child: no cycle, within the depth
    { let (r, g) = (init.root.clone(), init.graph.clone()); if let Err(e) = blocking(move || crate::team_subteam::check(&r, &g)).await.unwrap_or(Err("cut off".into())) { return Err(fail(e, 0.0)); } }
    let child = match load_graph(&init.root, team) { Ok(g) => g, Err(e) => return Err(fail(format!("{node_id}: {e}"), 0.0)) };
    let get = |k: &str| -> Option<String> { if k == "ask" { return Some(init.ask.clone()); } let (n, f) = k.split_once('.')?; values.get(n)?.get(f).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())) };
    let inner_ask = match crate::team_llm::fill(ask, &get) { Ok(a) => a, Err(e) => return Err(fail(format!("{node_id}: {e}"), 0.0)) };
    let mut g2 = child.clone(); let share = budget_share.unwrap_or(0.5); let mine = node.cap_usd.unwrap_or(init.graph.budget_usd) * share;
    if mine > 0.0 { g2.budget_usd = if g2.budget_usd > 0.0 { g2.budget_usd.min(mine) } else { mine }; }
    let h = match start(&init.root, g2, &inner_ask, &init.engine).await { Ok(h) => h, Err(e) => return Err(fail(format!("{node_id}: the inner team could not start: {e}"), 0.0)) };
    let child_id = h.id.clone();
    // whose child this is, so the person finds the child's approvals from the parent's team and the parent can say it waits on one
    { let d = run_dir(&init.root, &child_id); let _ = std::fs::create_dir_all(&d); let _ = std::fs::write(d.join("parent.json"), json!({ "run": init.id, "team": init.graph.team, "node": node_id }).to_string()); }
    let limit = std::time::Duration::from_secs(node.timeout_s.unwrap_or(1800));
    let mut began = std::time::Instant::now();
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let Some(st) = status_of(&init.root, &child_id) else { continue };
        // the child waits for the PERSON (an approval): that time is theirs, not the child's, so the limit starts again
        if st["park_info"].as_object().map_or(false, |o| !o.is_empty()) { began = std::time::Instant::now(); }
        let status = st["state"]["status"]["status"].as_str().unwrap_or("").to_string();
        let cost = st["state"]["spent_usd"].as_f64().unwrap_or(0.0);
        if status == "done" {
            let vals = &st["state"]["values"];
            return Ok(NodeOut { transcript: format!("sub-team {team}: run {child_id} done\n"), outputs: crate::team_subteam::result(&child_id, team, outputs, vals), cost_usd: cost, ms: now() - start_ms });
        }
        if matches!(status.as_str(), "blocked" | "stopped") { return Err(fail(format!("{node_id}: the inner team's run {child_id} ended {status}: {}", st["state"]["status"]["why"].as_str().unwrap_or("")), cost)); }
        if began.elapsed() > limit { return Err(fail(format!("{node_id}: the inner team's run {child_id} did not finish in {} s", limit.as_secs()), cost)); }
    }
}
async fn run_transform(_init: Arc<RunInit>, node_id: String, node: Node, values: Map<String, Value>) -> Result<NodeOut, NodeFail> {
    let Kind::Transform { mode, from, config } = &node.kind else { return Err(NodeFail { kind: FailKind::Error, error: "not a transform node".into(), cost_usd: 0.0, transcript: String::new() }) };
    let start = now();
    let (mode_c, from, config) = (mode.clone(), from.clone(), config.clone());
    let out = blocking(move || {
        let input = value_ref(&values, &from)?;
        let output = crate::team_transform::apply(&mode_c, input, &config)?;
        Ok(json!({ "ok": true, "output": output }))
    }).await;
    match out {
        Some(Ok(v)) => Ok(NodeOut { transcript: format!("transform {node_id} ({mode})\n"), outputs: v, cost_usd: 0.0, ms: now() - start }),
        Some(Err(e)) => Err(NodeFail { kind: FailKind::Error, error: e, cost_usd: 0.0, transcript: String::new() }),
        None => Err(NodeFail { kind: FailKind::Error, error: "the transform node was cut off".into(), cost_usd: 0.0, transcript: String::new() }),
    }
}
/// What the run has, gathered for the master page: node outputs, the documents (text where readable), costs and events. Blocking.
fn compose_sources(root: &str, run: &str, graph: &Graph, ask: &str, values: &Map<String, Value>, team: &str) -> crate::team_compose::Sources {
    let snap = run_on_disk(root, run).unwrap_or(Value::Null); let st = &snap["state"];
    let art = run_dir(root, run).join("artifacts");
    let mut artifacts = vec![]; for a in artifacts_of(root, run) {
        let (name, node) = (s(&a["name"]), a["node"].as_str().map(String::from)); let bytes = a["bytes"].as_u64().unwrap_or(0);
        let text = if bytes <= ARTIFACT_MAX { let p = match &node { Some(n) => art.join(n).join(&name), None => art.join(&name) }; std::fs::read_to_string(p).ok() } else { None };
        artifacts.push(crate::team_compose::ArtSrc { node, name, bytes, text });
    }
    let mut events: Vec<(i64, String, String)> = events_of(root, run, 400).into_iter().map(|e| (e["t_ms"].as_i64().unwrap_or(0), s(&e["actor"]), s(&e["text"]))).collect(); events.reverse();
    let labels = graph.nodes.iter().map(|(k, n)| (k.clone(), title_of(k, n))).collect();
    let node_cost = st["node_cost"].as_object().map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_f64()?))).collect()).unwrap_or_default();
    crate::team_compose::Sources { team: team.into(), ask: ask.into(), status: st["status"].as_str().map(|x| if x == "running" { "done" } else { x }).unwrap_or("done").to_string(), cost_usd: st["spent_usd"].as_f64().unwrap_or(0.0), started_ms: st["started_ms"].as_i64().unwrap_or(0), ended_ms: now(),
        values: values.clone(), labels, node_cost, artifacts, events, media: crate::team_media::media_for_page(root, run) }
}
/// THE MASTER PAGE NODE (§4.16): the run's one result page, built from the spec and what the run has. No model, no cost.
async fn run_compose(init: Arc<RunInit>, spec: crate::team_compose::Spec, name: String, values: Map<String, Value>) -> Result<NodeOut, NodeFail> {
    let fail = |e: String| NodeFail { kind: FailKind::Error, error: e, cost_usd: 0.0, transcript: String::new() }; let start = now();
    let (root, run, graph, ask, team) = (init.root.clone(), init.id.clone(), init.graph.clone(), init.ask.clone(), init.team.clone());
    let r = blocking(move || -> Result<Value, String> {
        let src = compose_sources(&root, &run, &graph, &ask, &values, &team); let html = crate::team_compose::render(&spec, &src)?;
        let art = run_dir(&root, &run).join("artifacts"); std::fs::create_dir_all(&art).map_err(|e| e.to_string())?;
        let file = format!("{name}.html"); std::fs::write(art.join(&file), &html).map_err(|e| e.to_string())?;
        Ok(json!({ "ok": true, "name": name, "format": "html", "path": format!("artifacts/{file}"), "bytes": html.len(), "made_from": "the master page spec", "result": true }))
    }).await;
    match r { Some(Ok(v)) => Ok(NodeOut { transcript: format!("the master page {} ({} bytes)\n", v["path"].as_str().unwrap_or(""), v["bytes"]), outputs: v, cost_usd: 0.0, ms: now() - start }), Some(Err(e)) => Err(fail(format!("the master page could not be built: {e}"))), None => Err(fail("the master page was cut off".into())) }
}
/// The run's result page, if it has one: the page of its `compose` node, else `result.html` (made by "Make the master page"). Blocking.
fn result_of(root: &str, run: &str) -> Result<Value, String> {
    let snap = run_on_disk(root, run).ok_or("no such run")?;
    let name = snap["graph"]["nodes"].as_object().and_then(|n| n.values().find_map(|x| (x["node"] == "artifact" && x["compose"].is_object()).then(|| s(&x["name"]) + ".html"))).unwrap_or_else(|| "result.html".into());
    let mut v = artifact_text(root, run, None, &name).map_err(|_| "this run has no result page yet".to_string())?; v["result"] = json!(true); Ok(v)
}
/// "Make the master page": a default page from what the run has, kept as `result.html`. Blocking.
fn compose_auto(root: &str, run: &str) -> Result<Value, String> {
    let snap = run_on_disk(root, run).ok_or("no such run")?;
    let graph: Graph = serde_json::from_value(snap["graph"].clone()).map_err(|e| format!("the run's graph cannot be read: {e}"))?;
    let values = snap["state"]["values"].as_object().cloned().unwrap_or_default();
    let src = compose_sources(root, run, &graph, &s(&snap["ask"]), &values, &s(&snap["team"])); let spec = crate::team_compose::auto_spec(&src);
    let html = crate::team_compose::render(&spec, &src)?;
    let art = run_dir(root, run).join("artifacts"); std::fs::create_dir_all(&art).map_err(|e| e.to_string())?; std::fs::write(art.join("result.html"), &html).map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true, "name": "result.html", "bytes": html.len(), "result": true }))
}
// ── THE SELF-IMPROVING ENGINE: audit every run, tell the Leader when the result is not good (team_audit.rs) ─────────────────────────────────────────────────────────────────────────
static PTY: OnceLock<Arc<crate::pty::PtyRegistry>> = OnceLock::new();
/// End a pane's terminal (a thread that is deleted takes its terminals with it).
pub fn pty_kill(pane: i64) { if let Some(r) = PTY.get() { let _ = r.kill(pane); } }
/// The terminals' registry, so the audit can type into the Leader's pane. Set once when the router is built.
pub fn set_pty(r: Arc<crate::pty::PtyRegistry>) { let _ = PTY.set(r); }
pub fn team_files(root: &str, team: &str) -> PathBuf { teams_dir(root).join(slug(team)) }
/// What a graph is, for telling a changed team from the same one: its nodes and edges without where they were drawn.
pub fn graph_hash(g: &Graph) -> String { use sha2::{Digest, Sha256}; let mut v = g.to_json(); if let Some(n) = v["nodes"].as_object_mut() { for x in n.values_mut() { if let Some(o) = x.as_object_mut() { o.remove("pos"); } } } hex::encode(Sha256::digest(serde_json::to_string(&json!({ "n": v["nodes"], "e": v["edges"] })).unwrap_or_default().as_bytes()))[..12].to_string() }
/// The thread (not this team's) whose layout holds this pane, if any. Pure over the threads view: `threads[].team` and `threads[].panes[].paneId`.
pub fn other_thread_of(view: &Value, team: &str, pane: i64) -> Option<String> {
    // a fork of a team ("<team>--t-<name>") stands in its team's thread, with a Leader of its own: the team and its forks are one family
    let fam = |x: &str| slug(x.split("--t-").next().unwrap_or(x)); let me = fam(team);   // (the slug of the whole name would fold the two dashes into one)
    view["threads"].as_array()?.iter().find(|t| t["team"].as_str().map_or(true, |x| fam(x) != me) && t["panes"].as_array().map_or(false, |ps| ps.iter().any(|p| p["paneId"].as_i64() == Some(pane)))).map(|t| t["id"].as_str().unwrap_or("?").to_string())
}
fn read_jsonl(p: &Path) -> Vec<Value> { std::fs::read_to_string(p).ok().map(|t| t.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default() }
fn write_jsonl(p: &Path, rows: &[Value]) { if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); } let t: String = rows.iter().map(|r| r.to_string() + "\n").collect(); let _ = std::fs::write(p, t); }
fn append_jsonl(p: &Path, row: &Value) { if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); } if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) { use std::io::Write; let _ = writeln!(f, "{row}"); } }
pub fn audits_of(root: &str, team: &str) -> Vec<crate::team_audit::Audit> { read_jsonl(&team_files(root, team).join("audits.jsonl")).into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect() }
/// Types the unread messages into the Leader's terminal (the pane the screen bound to this team), and marks them delivered. Whatever cannot be delivered now stays unread, in the inbox, for the Leader's next look.
pub async fn deliver_pending(root: &str, team: &str) -> usize {
    let dir = team_files(root, team); let pane = read_jsonl(&dir.join("leader.json")).first().and_then(|v| v["pane"].as_i64()).or_else(|| std::fs::read_to_string(dir.join("leader.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["pane"].as_i64()));
    let (Some(pane), Some(reg)) = (pane, PTY.get().cloned()) else { return 0 };
    let mut rows = read_jsonl(&dir.join("leader-inbox.jsonl")); let mut sent = 0;
    for r in rows.iter_mut() { if r["status"] != json!("unread") { continue; } let msg = s(&r["message"]); if crate::cowork_mcp::send_to(&reg, pane, &msg).await.is_ok() { r["status"] = json!(if r["kind"] == json!("memory") { "handled" } else { "delivered" });   /* the memory needs no decision once it is in the terminal */ r["delivered_ms"] = json!(now()); sent += 1; } else { break; } }
    if sent > 0 { write_jsonl(&dir.join("leader-inbox.jsonl"), &rows); } sent
}
/// AUTOFIX (the operator 2026-11-18: "a run failed and the Leader did not receive the report; it should be automatic so it can correct it and try again, on by default, with an on/off button"): whether the Leader is told to fix a failed run and run it again on its own.
pub const AUTOFIX_TRIES: usize = 3;
pub fn autofix_on(root: &str, team: &str) -> bool { std::fs::read_to_string(team_files(root, team).join("autofix.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["on"].as_bool()).unwrap_or(true) }
pub fn set_autofix(root: &str, team: &str, on: bool) { let d = team_files(root, team); let _ = std::fs::create_dir_all(&d); let _ = std::fs::write(d.join("autofix.json"), json!({ "on": on }).to_string()); }
/// What the failure message adds for the Leader: with autofix on, to fix the cause and run the team again with the same ask (up to `AUTOFIX_TRIES` failures in a row, then stop and tell the person); with it off, to wait for the person. Pure.
pub fn autofix_tail(on: bool, streak: usize, ask: &str) -> String {
    if !on { return "\nAUTOFIX IS OFF for this team: do not run it again yourself; tell the person what failed and what you would change, and wait.\n".into(); }
    if streak > AUTOFIX_TRIES { return format!("\nAUTOFIX IS ON, but this is failure {streak} in a row (the limit is {AUTOFIX_TRIES}): do NOT run it again. Stop, tell the person what you tried and what you learned, and ask what they want.\n"); }
    format!("\nAUTOFIX IS ON (failure {streak} of at most {AUTOFIX_TRIES} in a row): the person wants you to fix this on your own. Find the cause (antiloki_team_audit op `get` and antiloki_team_status), change the team with antiloki_team_design or antiloki_team_node, then run it again with the same ask with antiloki_team_run: {}. The budget the person set still holds. Say in one line what you changed.\n", if ask.trim().is_empty() { "(the same ask as the failed run)".to_string() } else { format!("\"{}\"", ask.trim().chars().take(240).collect::<String>()) })
}
/// The audit of a finished run: kept with the run and in the team's history; when the result is not good, the Leader is told (always in the inbox, in its terminal when it is open).
pub async fn audit_and_notify(root: &str, run: &str) -> Option<crate::team_audit::Audit> {
    let (root2, run2) = (root.to_string(), run.to_string());
    let (a, msg, team, wake) = blocking(move || -> Option<(crate::team_audit::Audit, Option<String>, String, Option<crate::team_improve::Reason>)> {
        let snap = run_on_disk(&root2, &run2)?; let g: Graph = serde_json::from_value(snap["graph"].clone()).ok()?; let team = s(&snap["team"]); let prev = audits_of(&root2, &team);
        // THE BENCHMARK PER NODE (team_nodebench.rs): this run's rows join the team's, the gate reads them all
        let gh = graph_hash(&g); let nb_path = team_files(&root2, &team).join("node-records.jsonl"); let mut rows: Vec<crate::team_nodebench::NodeRecord> = read_jsonl(&nb_path).into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect();
        let mine = crate::team_nodebench::records_of(&snap, &gh); rows.extend(mine);         let rows = crate::team_nodebench::trim(rows); write_jsonl(&nb_path, &rows.iter().filter_map(|r| serde_json::to_value(r).ok()).collect::<Vec<_>>());
        crate::ledger::sync_node_records(&root2, &team, &nb_path, &rows);
        crate::ledger::consolidate_async(&root2, &team);
        crate::ledger::flush();
        // the regulator: compare each node's statistics with its setpoints, act inside the envelope (an overlay file), and say what it saw. The graph is only read.
        let reg: Vec<crate::team_audit::Finding> = crate::team_regulate::after_run(&root2, &team, &g, now()).into_iter().filter_map(|x| match x { crate::team_regulate::Act::Finding { node, message } => Some(crate::team_audit::Finding { code: "setpoint_breached".into(), severity: "warn".into(), node: Some(node), text: message, suggestion: "the router has been told to leave a failing seat alone for a while; read the benchmark block of the node".into() }), _ => None }).collect();
        let (mut nf, mut attention) = crate::team_nodebench::findings(&rows); if !reg.is_empty() { attention = true; nf.extend(reg); }
        let mut a = crate::team_audit::audit(&snap, &gh, &prev, now()); a.findings.extend(nf); a.attention = attention; let mut snap2 = snap.clone(); snap2["audit"] = serde_json::to_value(&a).ok()?; let _ = std::fs::write(run_dir(&root2, &run2).join("state.json"), serde_json::to_string(&snap2).unwrap_or_default());
        append_jsonl(&team_files(&root2, &team).join("audits.jsonl"), &serde_json::to_value(&a).ok()?);
        let notify = (!a.good || a.attention) && a.status != "stopped";
        // a FAILED run (blocked) is always told, with the autofix instruction: how many failures in a row (this one and the runs before it)
        let failed = a.status == "blocked"; let streak = if failed { 1 + runs_of(&root2, &team).into_iter().filter(|r| r["id"] != json!(run2)).take_while(|r| r["status"]["status"].as_str() == Some("blocked")).count() } else { 0 };
        let (fix_on, ask) = (autofix_on(&root2, &team), s(&snap["ask"]));
        let notify = notify || failed; let msg = notify.then(|| { let mut m = crate::team_audit::leader_message(&a, &g.team); if failed { m.push_str(&autofix_tail(fix_on, streak, &ask)); } m });
        if let Some(m) = &msg { append_jsonl(&team_files(&root2, &team).join("leader-inbox.jsonl"), &json!({ "id": format!("a-{}", a.t_ms), "run": a.run, "t_ms": a.t_ms, "status": "unread", "score": a.score, "message": m })); }
        // EVERY RUN REPORTS TO THE LEADER IN FULL (the operator 2026-11-22), good or bad, so it can reshape the team from the numbers
        { let recent = runs_of(&root2, &team).into_iter().filter(|r| r["id"] != json!(run2)).take(8).collect::<Vec<_>>(); append_jsonl(&team_files(&root2, &team).join("leader-inbox.jsonl"), &json!({ "id": format!("r-{}", a.t_ms), "run": a.run, "t_ms": a.t_ms, "status": "unread", "kind": "run_report", "message": crate::team_evolve::run_report(&team, &snap, &recent) })); }
        // the improver wakes only on a trigger (a gate finding, N runs), never on every run; it is off until the person turns it on
        let wake = crate::team_improve::after_run(&root2, &team, &a.findings.iter().map(|f| f.code.clone()).collect::<Vec<_>>(), now());
        Some((a, msg, team, wake))
    }).await.flatten()?;
    // RFC 0013: the same trigger also tells the Leader a request is due (the person decides; nothing is spent until they start an experiment)
    if let Some(reason) = &wake { append_jsonl(&team_files(root, &team).join("leader-inbox.jsonl"), &json!({ "id": format!("i-{}", now()), "t_ms": now(), "status": "unread", "kind": "improver_due", "message": format!("[improver due: {reason:?}] The team is due for a review. Read `antiloki_team_audit` op `improve_prompt`, propose ONE small change, and send it with op `improve_request` (it opens a request the person decides; it spends nothing).") })); }
    if let Some(reason) = wake { let (r, t) = (root.to_string(), team.clone()); tokio::spawn(async move { let _ = crate::team_improve::run_review(&r, &t, reason).await; }); }
    if msg.is_some() { deliver_pending(root, &team).await; } Some(a)
}
// ── CAMPAIGNS (team_campaign.rs): runs toward one result, in parallel within the machine and the money ─────────────────────────────────────────────────────────────────────────────
fn campaigns_dir(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("campaigns") }
fn save_campaign(root: &str, c: &crate::team_campaign::Campaign) { let d = campaigns_dir(root); let _ = std::fs::create_dir_all(&d); let _ = std::fs::write(d.join(format!("{}.json", c.id)), serde_json::to_string_pretty(c).unwrap_or_default()); }
pub fn load_campaign(root: &str, id: &str) -> Option<crate::team_campaign::Campaign> { if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return None; } std::fs::read_to_string(campaigns_dir(root).join(format!("{id}.json"))).ok().and_then(|t| serde_json::from_str(&t).ok()) }
fn campaign_stops() -> &'static Mutex<HashMap<String, Arc<std::sync::atomic::AtomicBool>>> { static S: OnceLock<Mutex<HashMap<String, Arc<std::sync::atomic::AtomicBool>>>> = OnceLock::new(); S.get_or_init(Default::default) }
/// Start a campaign: the saved team, an ask, arms (each a patch of nodes), a budget for the RESULT and how many may run at once (at most what the machine carries). Returns at once; the campaign drives itself.
pub async fn campaign_start(root: &str, b: &Value) -> Result<Value, String> {
    use crate::team_campaign::*;
    let team = s(&b["team"]); let ask = s(&b["ask"]); let engine = { let e = s(&b["engine"]); if e.is_empty() { crate::cli::agent_seat("", None).key().to_string() } else { e } }; let budget = b["budget_usd"].as_f64().unwrap_or(0.0);
    let arms_in = b["arms"].as_array().cloned().unwrap_or_default(); let cap = { let c = crate::local::memory_available_mb(); slots(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1), c) };
    let parallel = b["parallel"].as_u64().map(|p| p as usize).unwrap_or(cap.min(arms_in.len().max(1))); let bad = check(&arms_in, budget, parallel); if !bad.is_empty() { return Err(bad.join("; ")); }
    if ask.trim().is_empty() { return Err("say what the team is to do (`ask`)".into()); }
    let base = load_graph(root, &team).map_err(|e| format!("no such team: {e}"))?; let mut arms = vec![]; let mut graphs = vec![];
    for a in &arms_in {
        let mut g = base.clone(); let patch = a["patch"].as_object().cloned().unwrap_or_default();
        for (node, p) in &patch { apply_edit(&mut g, node, p).map_err(|e| format!("arm {}: {e}", s(&a["name"])))?; }
        let rep = validate(&g); if !rep.errors.is_empty() { return Err(format!("arm {} cannot run: {}", s(&a["name"]), rep.words().join("; "))); }
        let worst = if g.budget_usd > 0.0 { rep.worst_usd.min(g.budget_usd) } else { rep.worst_usd };
        arms.push(Arm { name: s(&a["name"]), patch, state: "waiting".into(), worst_usd: (worst * 1000.0).round() / 1000.0, ..Default::default() }); graphs.push(g);
    }
    let id = format!("c-{}", now() % 1_000_000_000);
    let c = Campaign { id: id.clone(), team: slug(&team), ask, engine, budget_usd: budget, parallel: parallel.min(cap.max(1)).max(1), arms, status: "running".into(), started_ms: now(), capacity: json!({ "slots": cap, "cpu_threads": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1), "ram_available_mb": crate::local::memory_available_mb() }), ..Default::default() };
    save_campaign(root, &c); let stop = Arc::new(std::sync::atomic::AtomicBool::new(false)); campaign_stops().lock().unwrap_or_else(|p| p.into_inner()).insert(id.clone(), stop.clone());
    let (r2, c2) = (root.to_string(), c.clone()); tokio::spawn(async move { campaign_drive(r2, c2, graphs, stop).await; });
    Ok(json!({ "ok": true, "campaign": id, "arms": c.arms.len(), "parallel": c.parallel, "capacity": c.capacity, "budget_usd": budget, "note": "the arms start as the machine and the budget allow; read it with antiloki_team_campaign status" }))
}
async fn campaign_drive(root: String, mut c: crate::team_campaign::Campaign, graphs: Vec<Graph>, stop: Arc<std::sync::atomic::AtomicBool>) {
    use crate::team_campaign::*;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        // 1. where the running arms are: spent so far; finished when the run has been audited
        for a in c.arms.iter_mut().filter(|a| a.state == "running") {
            let run = a.run.clone().unwrap_or_default(); let r2 = root.clone(); let run2 = run.clone(); let snap = blocking(move || run_on_disk(&r2, &run2)).await.flatten();
            if let Some(h) = handle(&run) { let sp = h.snap.lock().map(|g| g["spent_usd"].as_f64().unwrap_or(0.0)).unwrap_or(0.0); a.spent_usd = a.spent_usd.max(sp); }
            if let Some(v) = &snap { a.spent_usd = a.spent_usd.max(v["state"]["spent_usd"].as_f64().unwrap_or(0.0)); if v["audit"].is_object() { a.state = "done".into(); a.score = v["audit"]["score"].as_f64(); a.good = v["audit"]["good"].as_bool(); a.spent_usd = v["state"]["spent_usd"].as_f64().unwrap_or(a.spent_usd); a.note = v["audit"]["findings"].as_array().and_then(|f| f.iter().find(|x| x["severity"] == "blocker")).map(|x| s(&x["code"])).unwrap_or_default(); } }
            if a.state == "running" && handle(&run).is_none() && snap.as_ref().map_or(true, |v| !v["audit"].is_object()) && now() - c.started_ms > 5_000 { a.state = "lost".into(); a.note = "the run ended without an audit".into(); }
        }
        c.spent_usd = c.arms.iter().map(|a| a.spent_usd).sum(); let running = c.arms.iter().filter(|a| a.state == "running").count(); c.peak_parallel = c.peak_parallel.max(running);
        // 2. THE RESULT'S BUDGET: at the ceiling everything stops, whatever is still waiting is given up
        let stopped = stop.load(std::sync::atomic::Ordering::Relaxed);
        if stopped || c.spent_usd >= c.budget_usd - 1e-9 {
            for a in c.arms.iter_mut() { if a.state == "running" { if let Some(h) = a.run.as_ref().and_then(|r| handle(r)) { let _ = h.ctl.send(Ctl::Stop); } a.state = "stopped".into(); a.note = if stopped { "the campaign was stopped".into() } else { "the result's budget was reached".into() }; } else if a.state == "waiting" { a.state = "skipped".into(); a.note = if stopped { "the campaign was stopped".into() } else { "the result's budget was reached".into() }; } }
            c.status = if stopped { "stopped".into() } else { "budget_stopped".into() };
        } else {
            // 3. start what fits: the machine's slots and the budget's room
            let waiting: Vec<(usize, f64)> = c.arms.iter().enumerate().filter(|(_, a)| a.state == "waiting").map(|(i, a)| (i, a.worst_usd)).collect(); let left: f64 = c.arms.iter().filter(|a| a.state == "running").map(|a| (a.worst_usd - a.spent_usd).max(0.0)).sum();
            let (go, skip) = next_starts(&waiting, running, c.parallel, c.spent_usd, left, c.budget_usd);
            for (i, why) in skip { c.arms[i].state = "skipped".into(); c.arms[i].note = why; }
            for i in go { match start(&root, graphs[i].clone(), &c.ask, &c.engine).await { Ok(h) => { c.arms[i].state = "running".into(); c.arms[i].run = Some(h.id); } Err(e) => { c.arms[i].state = "skipped".into(); c.arms[i].note = e.chars().take(160).collect(); } } }
            let running = c.arms.iter().filter(|a| a.state == "running").count(); c.peak_parallel = c.peak_parallel.max(running);
        }
        let open = c.arms.iter().any(|a| a.state == "waiting" || a.state == "running");
        if !open { c.ended_ms = now(); c.winner = pick_winner(&c.arms); if c.status == "running" { c.status = "done".into(); } c.reading = summary(&c); save_campaign(&root, &c); campaign_stops().lock().unwrap_or_else(|p| p.into_inner()).remove(&c.id); return; }
        save_campaign(&root, &c);
    }
}
/// The artifacts of a run, for the door: the documents made (top level) and the drafts each node left (one folder per node). Blocking.
fn artifacts_of(root: &str, run: &str) -> Vec<Value> {
    let art = run_dir(root, run).join("artifacts"); let mut out = vec![];
    let entry = |p: &Path, node: Option<&str>| -> Option<Value> { let name = p.file_name()?.to_string_lossy().to_string(); let fmt = artifact_ext(&name)?; let m = std::fs::metadata(p).ok()?; Some(json!({ "name": name, "format": fmt, "bytes": m.len(), "node": node, "kind": if node.is_some() { "draft" } else { "made" }, "t_ms": m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64) })) };
    for e in std::fs::read_dir(&art).into_iter().flatten().flatten() { let p = e.path(); if p.is_file() { if let Some(v) = entry(&p, None) { out.push(v); } } else if p.is_dir() { let node = e.file_name().to_string_lossy().to_string(); for f in std::fs::read_dir(&p).into_iter().flatten().flatten() { if f.path().is_file() { if let Some(v) = entry(&f.path(), Some(&node)) { out.push(v); } } } } }
    out.sort_by(|a, b| a["kind"].as_str().cmp(&b["kind"].as_str()).then(a["name"].as_str().cmp(&b["name"].as_str()))); out
}
fn artifact_text(root: &str, run: &str, node: Option<&str>, name: &str) -> Result<Value, String> {
    if !plain_name(name) || node.map_or(false, |n| !plain_name(n)) { return Err("that is not an artifact name".into()); }
    let mut p = run_dir(root, run).join("artifacts"); if let Some(n) = node { p = p.join(n); } p = p.join(name);
    let m = std::fs::metadata(&p).map_err(|_| format!("no artifact called {name}"))?; if !m.is_file() || m.len() > ARTIFACT_MAX { return Err("that artifact is not readable here".into()); }
    let text = std::fs::read_to_string(&p).map_err(|e| e.to_string())?; Ok(json!({ "ok": true, "name": name, "format": artifact_ext(name), "text": text }))
}
/// THE PACKAGE STEP (N14, second slice): fetch a pinned git source into the run's folder, build a venv with exactly pinned wheels, run ONE registry entry. Refuses unless the person approved exactly this commit.
async fn run_package(init: &Arc<RunInit>, node_id: &str, node: &Node, dir: &Path, tool: &ToolRef) -> Result<NodeOut, NodeFail> {
    let fail = |e: String, t: String| NodeFail { kind: FailKind::Error, error: e, cost_usd: 0.0, transcript: t };
    let start = now();
    let r = crate::team_package::check_run(&tool.name, &tool.args).map_err(|e| fail(e.join("; "), String::new()))?;
    let plan = crate::team_package::plan(&r.spec, &init.root, &init.id, node_id).map_err(|e| fail(e.join("; "), String::new()))?;
    let list: Value = std::fs::read_to_string(team_files(&init.root, &init.team).join("package-approvals.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!([]));
    if !crate::team_package::approved(&list, &plan.approval_key) { return Err(fail(format!("waiting for the person: approve the package {} (the key is {}). It lands in {} and runs {} only", r.spec.name, plan.approval_key, plan.dir, r.entry), String::new())); }
    let (sandbox, tree) = (plan.dir.clone(), dir.to_string_lossy().to_string());
    let _ = std::fs::create_dir_all(&sandbox);
    if let Some(parent) = Path::new(&tree).join(&r.out).parent() { let _ = std::fs::create_dir_all(parent); }
    let steps = crate::team_package::steps(&r, &sandbox, &tree, cfg!(windows));
    let mut tr = String::new();
    for (i, (prog, args)) in steps.iter().enumerate() {
        if i == 0 && Path::new(&sandbox).join("src").join(".git").exists() { tr.push_str("clone: already there\n"); continue; }
        let mut c = crate::proc_async::tokio_cmd(prog); c.args(args).current_dir(dir).env("PYTHONPATH", format!("{sandbox}/src")).env("PYTHONDONTWRITEBYTECODE", "1");
        let o = tokio::time::timeout(std::time::Duration::from_secs(node.timeout_s.unwrap_or(1800)), c.output()).await;
        let o = match o { Ok(Ok(o)) => o, Ok(Err(e)) => return Err(fail(format!("step {} could not start `{prog}`: {e}", i + 1), tr)), Err(_) => return Err(NodeFail { kind: FailKind::Timeout, error: format!("step {} (`{prog}`) ran out of time", i + 1), cost_usd: 0.0, transcript: tr }) };
        let tail: String = { let t = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)); let l: Vec<&str> = t.lines().collect(); l[l.len().saturating_sub(6)..].join("\n") };
        tr.push_str(&format!("step {} `{}`: exit {}\n{}\n", i + 1, prog.rsplit('/').next().unwrap_or(prog), o.status.code().unwrap_or(-1), tail));
        if !o.status.success() { return Err(fail(format!("step {} (`{}`) failed: exit {}", i + 1, prog.rsplit('/').next().unwrap_or(prog), o.status.code().unwrap_or(-1)), tr)); }
        if i == 1 { let head = crate::proc_async::tokio_cmd("git").args(["-C", &format!("{sandbox}/src"), "rev-parse", "HEAD"]).output().await.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default(); if head != r.spec.version { return Err(fail(format!("the checkout is {head}, not the pinned {}", r.spec.version), tr)); } }
    }
    let out = Path::new(&tree).join(&r.out); let rd = |f: &str| { let t = std::fs::read_to_string(out.join(f)).unwrap_or_default(); t.chars().take(6000).collect::<String>() };
    let files = walk_count(&out);
    if files == 0 { return Err(fail("the entry ran and wrote nothing".into(), tr)); }
    Ok(NodeOut { outputs: json!({ "ok": true, "out": r.out, "files": files, "video_md": rd("VIDEO.md"), "timeline": rd("video/timeline.md"), "packaging": rd("video/packaging.md"), "sound": rd("video/sound.md"), "look": rd("video/look.md") }), cost_usd: 0.0, transcript: tr, ms: now() - start })
}
fn walk_count(d: &Path) -> usize { std::fs::read_dir(d).map(|rd| rd.flatten().map(|e| { let p = e.path(); if p.is_dir() { walk_count(&p) } else { 1 } }).sum()).unwrap_or(0) }

/// THE SOUND STEP (VID-27): `cue_sheet` reads and statically checks the template's beats; `mix` runs the shipped script with the person's sound runtime and fails with the exact failed checks.
async fn run_sound(init: &Arc<RunInit>, node: &Node, dir: &Path, tool: &ToolRef, values: &Map<String, Value>) -> Result<NodeOut, NodeFail> {
    let fail = |kind: FailKind, e: String, t: String| NodeFail { kind, error: e, cost_usd: 0.0, transcript: t };
    let start = now(); let errs = crate::team_sound::check_args(&tool.name, &tool.args); if !errs.is_empty() { return Err(fail(FailKind::Error, errs.join("; "), String::new())); }
    let rel = |k: &str| tool.args[k].as_str().unwrap_or("").to_string();
    let js = std::fs::read_to_string(dir.join(rel("file"))).map_err(|e| fail(FailKind::Error, format!("cannot read {}: {e}", rel("file")), String::new()))?;
    let extra: Vec<String> = tool.args["consts"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).map(|f| std::fs::read_to_string(dir.join(f)).unwrap_or_default()).collect()).unwrap_or_default();
    let mut srcs: Vec<&str> = vec![js.as_str()]; srcs.extend(extra.iter().map(|x| x.as_str())); let (cues, issues) = crate::team_sound::cues_with(&srcs);
    if !issues.is_empty() { return Err(fail(FailKind::Verdict, format!("the cue sheet has {} problem(s): {}", issues.len(), issues.join("; ")), String::new())); }
    if cues.is_empty() { return Err(fail(FailKind::Verdict, "the template has no `cue:` labels".into(), String::new())); }
    let line = cues.iter().map(|c| format!("{}@{}", c.id, c.t)).collect::<Vec<_>>().join(", ");
    if tool.name == "cue_sheet" { return Ok(NodeOut { outputs: json!({ "ok": true, "count": cues.len(), "cues": cues }), cost_usd: 0.0, transcript: format!("cue sheet: {line}\n"), ms: now() - start }); }
    let text = value_ref(values, &tool.args["text_from"].as_str().map(String::from)).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).map_err(|e| fail(FailKind::Error, e, String::new()))?;
    let (py, voice) = (crate::team_sound::python_path().map_err(|e| fail(FailKind::Error, e, String::new()))?, crate::team_sound::voice_path().map_err(|e| fail(FailKind::Error, e, String::new()))?);
    let work = run_dir(&init.root, &init.id).join("sound"); let _ = std::fs::create_dir_all(&work); let out_abs = dir.join(rel("out")); if let Some(d) = out_abs.parent() { let _ = std::fs::create_dir_all(d); }
    // the shot's length may come from an earlier node (`duration_from`); the narration window may then end `end_margin_s` before its end
    let duration = match tool.args["duration_from"].as_str() { Some(f) => value_ref(values, &Some(f.to_string())).ok().and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|x| x.parse().ok()))).ok_or_else(|| fail(FailKind::Error, format!("`duration_from` ({f}) gave no number"), String::new()))?, None => tool.args["duration_s"].as_f64().unwrap_or(5.0) };
    let mut spec = tool.args["spec"].clone(); if let Some(m) = spec["narration"]["end_margin_s"].as_f64() { spec["narration"]["end_s"] = json!(duration - m); }
    let (script, jobp) = (work.join("team_sound_mix.py"), work.join("job.json"));
    // the kit lives next to the runtime (generated on first use); the music is chosen for the topic's own text
    let kit_dir = py.parent().and_then(|p| p.parent()).and_then(|p| p.parent()).map(|p| p.join("kit")).unwrap_or_else(|| work.join("kit"));
    let music_text = tool.args["spec"]["music"]["want_from"].as_str().and_then(|f| value_ref(values, &Some(f.to_string())).ok()).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).unwrap_or_default();
    let job = json!({ "kit_dir": kit_dir.to_string_lossy(), "music_text": music_text, "embed_url": crate::local::Config::from_env().base, "video": dir.join(rel("video")).to_string_lossy(), "out": out_abs.to_string_lossy(), "cues": cues.iter().map(|c| json!({ "id": c.id, "t": c.t })).collect::<Vec<_>>(), "text": text, "voice": voice.to_string_lossy(), "spec": spec, "duration_s": duration });
    std::fs::write(&script, crate::team_sound::MIX_PY).and_then(|_| std::fs::write(&jobp, job.to_string())).map_err(|e| fail(FailKind::Error, e.to_string(), String::new()))?;
    let mut c = crate::proc_async::tokio_cmd(&py); c.arg(&script).arg(&jobp).current_dir(dir);
    let o = match tokio::time::timeout(std::time::Duration::from_secs(node.timeout_s.unwrap_or(300)), c.output()).await { Ok(Ok(o)) => o, Ok(Err(e)) => return Err(fail(FailKind::Error, format!("the sound script did not start: {e}"), String::new())), Err(_) => return Err(NodeFail { kind: FailKind::Timeout, error: "the sound mix ran out of time".into(), cost_usd: 0.0, transcript: String::new() }) };
    let all = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    let Some(rep) = crate::team_sound::report_of(&all) else { let t: Vec<&str> = all.lines().rev().take(6).collect(); return Err(fail(FailKind::Error, format!("the sound script gave no report: {}", t.into_iter().rev().collect::<Vec<_>>().join(" | ")), String::new())) };
    let tr = rep["checks"].as_array().map(|a| a.iter().map(|c| format!("{} {}: {}", if c["ok"] == json!(true) { "ok  " } else { "FAIL" }, c["check"].as_str().unwrap_or(""), c["detail"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n")).unwrap_or_default();
    let bad = crate::team_sound::failures_of(&rep);
    if !bad.is_empty() { return Err(fail(FailKind::Verdict, format!("the sound failed {} check(s): {}", bad.len(), bad.join("; ")), format!("{tr}\n"))); }
    Ok(NodeOut { outputs: json!({ "ok": true, "out": rel("out"), "script": rep["script"], "seconds": rep["total_s"], "report": rep }), cost_usd: 0.0, transcript: format!("sound mix {line}\n{tr}\n"), ms: now() - start })
}
async fn run_tool(init: Arc<RunInit>, node_id: String, node: Node, dir: PathBuf, values: Map<String, Value>) -> Result<NodeOut, NodeFail> {
    let fail = |kind: FailKind, e: String, t: String| NodeFail { kind, error: e, cost_usd: 0.0, transcript: t };
    let Kind::UseTool { tool, allow, outputs: _, .. } = &node.kind else { return Err(fail(FailKind::Error, "not a tool".into(), String::new())) };
    let start = now(); let art = run_dir(&init.root, &init.id).join("artifacts"); let _ = std::fs::create_dir_all(&art);
    // the allow-list is the node's whole world: a path it was not given, an effect it did not list, a tool it does not name are refused here, whatever the graph says
    let need = |effects: &[&str]| -> Result<(), String> { for e in effects { if !allow.effects.iter().any(|a| a == e) { return Err(format!("{node_id} may not do `{e}`: its allow-list is {}", allow.effects.join(", "))); } } Ok(()) };
    match tool.kind.as_str() {
        "skill" => {
            let Some(effects) = skill_effects(&tool.name) else { return Err(fail(FailKind::Error, format!("there is no built-in skill called {}", tool.name), String::new())) };
            need(effects).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            if !allow.skills.is_empty() && !allow.skills.contains(&tool.name) { return Err(fail(FailKind::Error, format!("the skill {} is not in the allow-list", tool.name), String::new())); }
            // THE PREVIEW NODE (2026-11-21): the project is started and kept up for the person to look at, until the run ends
            if tool.name == "preview" {
                let (run, nid, d2, ld) = (init.id.clone(), node_id.clone(), dir.clone(), art.clone()); let wait = node.timeout_s.unwrap_or(90).min(300);
                return match crate::run_preview::start(&run, &nid, &d2, &ld, wait).await {
                    Ok(v) => Ok(NodeOut { outputs: json!({ "ok": true, "url": v["url"], "cmd": v["cmd"], "log": v["log"] }), cost_usd: 0.0, transcript: format!("preview up at {} ({})\n", v["url"].as_str().unwrap_or(""), v["cmd"].as_str().unwrap_or("")), ms: now() - start }),
                    Err(e) => Err(fail(FailKind::Error, e, String::new())),
                };
            }
            // THE LAW-GATE (RFC 0018, 2026-11-21): a deterministic lane: the ratified laws of the book judge what the team made (code, copy, a report, a plan, a table, a page, images), no model, the same answer twice
            if tool.name == "law-gate" {
                let a = &tool.args; let packs: Vec<String> = a["packs"].as_array().map(|x| x.iter().filter_map(|y| y.as_str().map(String::from)).collect()).unwrap_or_default(); let globs: Vec<String> = a["paths"].as_array().map(|x| x.iter().filter_map(|y| y.as_str().map(String::from)).collect()).unwrap_or_default();
                let fail_on = { let f = s(&a["fail_on"]); if crate::lanes::SEVERITIES.contains(&f.as_str()) { f } else { "error".to_string() } }; let (root, d2) = (init.root.clone(), dir.clone());
                let rep = blocking(move || { let (book, _) = crate::lanes::load_book(&root); let arts = crate::lanes::collect(&d2, &globs, 2000); crate::lanes::judge(&book, &arts, &crate::lanes::Opts { packs, candidates: false, fail_on }) }).await;
                let Some(rep) = rep else { return Err(fail(FailKind::Error, "the law-gate was cut off".into(), String::new())) };
                let log = art.join(format!("{node_id}-{}.json", now())); let _ = std::fs::write(&log, serde_json::to_string_pretty(&rep).unwrap_or_default());
                let skipped = rep["gate"]["skipped"] == json!(true); let passed = skipped || rep["gate"]["passed"] == json!(true); let tail = crate::lanes::render_findings(&rep, 25);
                let failed: Vec<Value> = rep["findings"].as_array().map(|f| f.iter().filter(|x| x["advisory"] != json!(true)).take(40).map(|x| json!(format!("{}:{} {} ({})", x["path"].as_str().unwrap_or(""), x["line"], x["message"].as_str().unwrap_or(""), x["law"].as_str().unwrap_or("")))).collect()).unwrap_or_default();
                return Ok(NodeOut { outputs: json!({ "ok": passed, "exit": if passed { 0 } else { 1 }, "failed": failed, "skipped": skipped, "score": rep["score"], "axes": rep["axes"], "counts": rep["counts"], "digest": rep["digest"], "log": log.to_string_lossy(), "tail": tail, "note": if skipped { "no law of the book applies to these files" } else { "" } }), cost_usd: 0.0, transcript: format!("law-gate {}\n{tail}", if skipped { "skipped: no law applies" } else if passed { "passed" } else { "failed" }), ms: now() - start });
            }
            if tool.name == "diff-stats" { let names = changed_names(&dir); return Ok(NodeOut { outputs: json!({ "ok": true, "files": names.len(), "names": names }), cost_usd: 0.0, transcript: format!("diff-stats: {} file(s)\n", names.len()), ms: now() - start }); }
            // THE LINT NODE (2026-11-21): the checkers are downloaded once, pinned, and run by antiloki itself: no model, the same verdict everywhere
            if tool.name == "lint" && std::env::var("ANTILOKI_TEAM_LINT_CMD").map_or(true, |v| v.trim().is_empty()) {
                let d2 = dir.clone(); let r = tokio::time::timeout(std::time::Duration::from_secs(node.timeout_s.unwrap_or(1800)), tokio::task::spawn_blocking(move || crate::lint_tools::run(&d2, &crate::lint_tools::tools_dir()))).await;
                let (code, text) = match r { Ok(Ok(x)) => x, Ok(Err(e)) => return Err(fail(FailKind::Error, format!("the lint step was cut off: {e}"), String::new())), Err(_) => return Err(fail(FailKind::Timeout, "lint ran out of time".into(), String::new())) };
                let log = art.join(format!("{node_id}-{}.log", now())); let _ = std::fs::write(&log, &text); let tail: String = text.lines().rev().take(25).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
                return Ok(NodeOut { outputs: json!({ "ok": code == 0, "exit": code, "failed": failed_tests(&text), "log": log.to_string_lossy(), "tail": tail }), cost_usd: 0.0, transcript: format!("lint (exit {code})\n{tail}\n"), ms: now() - start });
            }
            let Some(cmd) = skill_command(&tool.name, &dir) else { return Ok(NodeOut { outputs: json!({ "ok": true, "exit": 0, "failed": [], "note": "the project has nothing of that kind to run" }), cost_usd: 0.0, transcript: format!("{}: nothing to run in this project\n", tool.name), ms: now() - start }) };
            let log = art.join(format!("{node_id}-{}.log", now()));
            let out = tokio::time::timeout(std::time::Duration::from_secs(node.timeout_s.unwrap_or(1800)), crate::proc_async::shell(&cmd).current_dir(&dir).output()).await;
            let o = match out { Ok(Ok(o)) => o, Ok(Err(e)) => return Err(fail(FailKind::Error, format!("could not run {cmd}: {e}"), String::new())), Err(_) => return Err(fail(FailKind::Timeout, format!("{cmd} ran out of time"), String::new())) };
            let mut text = String::from_utf8_lossy(&o.stdout).to_string(); text.push_str(&String::from_utf8_lossy(&o.stderr)); if text.len() > 2_000_000 { text.truncate(2_000_000); }
            let _ = std::fs::write(&log, &text); let code = o.status.code().unwrap_or(-1) as i64; let tail: String = text.lines().rev().take(25).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            Ok(NodeOut { outputs: json!({ "ok": code == 0, "exit": code, "failed": failed_tests(&text), "log": log.to_string_lossy(), "tail": tail }), cost_usd: 0.0, transcript: format!("$ {cmd}\n(exit {code}, {} bytes)\n{tail}\n", text.len()), ms: now() - start })
        }
        // THE DATA TYPES (RFC 0011 §13, §9): a db (sqlite, csv, the team's own bench.sqlite), a file inside the run's folder, the workspace's sagas. Safe by default, the person's for a write.
        "db" => {
            need(&["read"]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            let a = &tool.args; let kind = { let k = s(&a["kind"]); if k.is_empty() { "sqlite".to_string() } else { k } };
            let name = if tool.name == "bench" { String::new() } else { tool.name.clone() }; let kind = if tool.name == "bench" { "bench".to_string() } else { kind };
            let op = { let o = s(&a["op"]); if o.is_empty() { "query".to_string() } else { o } };
            let (root, team, sql) = (init.root.clone(), init.team.clone(), s(&a["sql"])); let params: Vec<Value> = a["params"].as_array().cloned().unwrap_or_default();
            let lim = crate::team_db::Limits { max_rows: a["max_rows"].as_u64().unwrap_or(1000).clamp(1, 10_000) as usize, timeout_ms: a["timeout_ms"].as_u64().unwrap_or(5000).clamp(50, 30_000) };
            let write_op = op == "write";
            if write_op { need(&["write"]).map_err(|e| fail(FailKind::Error, e, String::new()))?; }
            let dry = a["dry_run"].as_bool().unwrap_or(false); let (k2, n2) = (kind.clone(), name.clone());
            let out = blocking(move || -> Result<Value, String> {
                let consent = write_op && crate::team_db::has_consent(&root, &team, &k2, &n2);
                let c = crate::team_db::open_source(&root, &team, &k2, &n2, write_op && consent)?;
                match op.as_str() {
                    "schema" => crate::team_db::schema(&c).map(|v| json!({ "ok": true, "schema": v })),
                    "query" | "export" => { let t = crate::team_db::query(&c, &sql, &params, lim)?;
                        let rows: Vec<Value> = t.rows.iter().map(|r| Value::Object(t.columns.iter().cloned().zip(r.iter().cloned()).collect())).collect();
                        Ok(json!({ "ok": true, "columns": t.columns, "rows": rows, "count": t.rows.len(), "truncated": t.truncated, "csv": if op == "export" { Value::String(crate::team_db::to_csv(&t)) } else { Value::Null } })) }
                    // a graph run never says the person's yes to a call: a schema change is refused here, a write needs the consent on file
                    "write" => crate::team_db::execute(&c, &sql, &params, lim, consent, false, dry).map(|w| json!({ "ok": true, "affected": w.affected, "dry_run": w.dry_run })),
                    other => Err(format!("a db op is schema, query, export or write (not `{other}`)")),
                }
            }).await;
            match out { Some(Ok(v)) => return Ok(NodeOut { transcript: format!("db {}\n", tool.name), outputs: v, cost_usd: 0.0, ms: now() - start }), Some(Err(e)) => return Err(fail(FailKind::Error, e, String::new())), None => return Err(fail(FailKind::Error, "the db call was cut off".into(), String::new())) }
        }
        // A WEB APP'S TOOL (RFC 0009): the serve's own MCP client calls it with the token the person gave; a read needs `read`, a write `write`, a destructive one `destructive` (which stands behind a human node)
        "webapp" => {
            let app = tool.name.trim().trim_start_matches("app-").to_string(); let tname = s(&tool.args["tool"]); let targs = tool.args["arguments"].clone();
            let apps = antiloki_rs::cockpit__helpers_webapps::load(); let Some(entry) = apps.iter().find(|a| a["id"] == json!(app)) else { return Err(fail(FailKind::Error, format!("the app `{app}` is not installed"), String::new())) };
            let url = s(&entry["mcp_url"]); if url.is_empty() { return Err(fail(FailKind::Error, format!("`{app}` has no MCP address"), String::new())); }
            crate::mcp_oauth::refresh_if_due(&app).await; let client = crate::mcp_client::Client::connect(&url, crate::app_tokens::get(&app), Default::default()).await.map_err(|e| fail(FailKind::Error, e.to_string(), String::new()))?;
            let tools = client.tools().await.map_err(|e| fail(FailKind::Error, e.to_string(), String::new()))?;
            let Some(t) = tools.iter().find(|t| t.name == tname) else { return Err(fail(FailKind::Error, format!("the app `{app}` has no tool `{tname}`"), String::new())) };
            need(&[match t.risk { crate::mcp_client::Risk::Read => "read", crate::mcp_client::Risk::Write => "write", crate::mcp_client::Risk::Destructive => "destructive" }]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            let r = client.call(&tname, targs).await.map_err(|e| fail(if e == crate::mcp_client::McpError::NeedsSignin { FailKind::Error } else { FailKind::Error }, e.to_string(), String::new()))?;
            let out = json!({ "ok": !r.is_error, "text": r.text, "structured": r.structured, "app": app, "tool": tname, "risk": t.risk, "ms": r.ms });
            return Ok(NodeOut { transcript: format!("{app}.{tname}: {}\n", if r.is_error { "the app said error" } else { "ok" }), outputs: out, cost_usd: 0.0, ms: now() - start });
        }
        // BL-18: an MCP server by ADDRESS (not only an installed app). Unattended, so: a read is free, a write only when the node lists `write`, a destructive tool never (the person calls those).
        "mcp_url" => {
            need(&["network"]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            let url = crate::team_web::fill(&s(&tool.args["url"]), &init.ask, &values); let u = crate::team_web::allowed_url(&url).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            let token = tool.args["token"].as_str().filter(|t| !t.is_empty()).and_then(crate::app_tokens::get);
            let client = crate::mcp_client::Client::connect(u.as_str(), token, Default::default()).await.map_err(|e| fail(FailKind::Error, e.to_string(), String::new()))?;
            let tools = client.tools().await.map_err(|e| fail(FailKind::Error, e.to_string(), String::new()))?;
            let Some(t) = tools.iter().find(|t| t.name == tool.name) else { return Err(fail(FailKind::Error, format!("the server has no tool `{}`", tool.name), String::new())) };
            if t.risk == crate::mcp_client::Risk::Destructive { return Err(fail(FailKind::Error, format!("`{}` destroys data: a team never calls it, the person does", tool.name), String::new())); }
            need(&[if t.risk == crate::mcp_client::Risk::Read { "read" } else { "write" }]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            let r = client.call(&tool.name, tool.args["arguments"].clone()).await.map_err(|e| fail(FailKind::Error, e.to_string(), String::new()))?;
            return Ok(NodeOut { transcript: format!("mcp {} {}: {}\n", u.host_str().unwrap_or(""), tool.name, if r.is_error { "the server said error" } else { "ok" }), outputs: json!({ "ok": !r.is_error, "text": r.text, "structured": r.structured, "tool": tool.name, "risk": t.risk, "ms": r.ms }), cost_usd: 0.0, ms: now() - start });
        }
        // BL-18: any http API. The method decides the risk; a destructive one is never a team's.
        "api" => {
            need(&["network"]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            let risk = crate::team_web::api_risk(&tool.name); if risk == "destructive" { return Err(fail(FailKind::Error, "DELETE destroys data: a team never calls it, the person does".into(), String::new())); }
            need(&[risk]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            let url = crate::team_web::fill(&s(&tool.args["url"]), &init.ask, &values); let headers = crate::team_web::api_headers_ok(&tool.args["headers"]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            let body = match &tool.args["body"] { Value::Null => None, Value::String(t) => Some(crate::team_web::fill(t, &init.ask, &values)), other => Some(crate::team_web::fill(&other.to_string(), &init.ask, &values)) };
            let bearer = tool.args["token"].as_str().filter(|t| !t.is_empty()).and_then(crate::app_tokens::get);
            let m = tool.name.to_ascii_uppercase(); let mut v = crate::team_web::api_call(&m, &url, &headers, body, bearer).await.map_err(|e| fail(FailKind::Error, e, format!("api {m} {url}\n")))?;
            let host = crate::team_web::allowed_url(&url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default(); v["risk"] = json!(risk);
            return Ok(NodeOut { transcript: format!("api {m} {host} -> {}\n{}\n", v["status"], v["text"].as_str().unwrap_or("").chars().take(1500).collect::<String>()), outputs: v, cost_usd: 0.0, ms: now() - start });
        }
        // BL-19: a Telegram message. Outward: it needs `network` and `outward` listed, and a human gate before it (the validator's rule). The token is read from the store by its name and goes nowhere else.
        "telegram" => {
            need(&["network", "outward"]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            let chat = crate::team_web::fill(&s(&tool.args["chat"]), &init.ask, &values); let text = crate::team_web::fill(&s(&tool.args["text"]), &init.ask, &values);
            let token = crate::app_tokens::get(&s(&tool.args["token"])).ok_or_else(|| fail(FailKind::Error, format!("no bot token is stored under `{}`: the person stores it once", s(&tool.args["token"])), String::new()))?;
            return match crate::team_telegram::send(&token, &chat, &text).await { Ok(v) => Ok(NodeOut { transcript: format!("telegram send {chat}: {} characters\n", v["chars"]), outputs: v, cost_usd: 0.0, ms: now() - start }), Err(e) => Err(fail(FailKind::Error, e, format!("telegram send {chat}\n"))) };
        }
        // BL-20: git steps inside the team's own tree (no model, no network)
        "git" => {
            need(&[crate::team_git::risk(&tool.name)]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            let (r2, team, op, args) = (init.root.clone(), init.graph.team.clone(), tool.name.clone(), { let mut a = tool.args.clone(); if let Some(m) = a["message"].as_str() { a["message"] = json!(crate::team_web::fill(m, &init.ask, &values)); } a });
            let out = blocking(move || crate::team_git::run(&r2, &team, &op, &args)).await.unwrap_or_else(|| Err("the git step did not run".into()));
            return match out { Ok(v) => Ok(NodeOut { transcript: format!("git {}\n{}\n", tool.name, v["text"].as_str().unwrap_or("").chars().take(1500).collect::<String>()), outputs: v, cost_usd: 0.0, ms: now() - start }), Err(e) => Err(fail(FailKind::Error, e, format!("git {}\n", tool.name))) };
        }
        "file" => {
            let a = &tool.args; let op = s(&a["op"]); let base = run_dir(&init.root, &init.id).join("files");
            let rel = if tool.name.is_empty() { s(&a["path"]) } else { tool.name.clone() };
            let out: Result<Value, String> = if op == "write" {
                need(&["write"]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
                let content = match a["content_from"].as_str().filter(|x| !x.is_empty()) { Some(f) => value_ref(&values, &Some(f.to_string())).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())), None => Ok(s(&a["content"])) };
                content.and_then(|c| crate::team_file::write(&base, &rel, &c)).map(|n| json!({ "ok": true, "bytes": n, "path": rel }))
            } else { need(&["read"]).map_err(|e| fail(FailKind::Error, e, String::new()))?; crate::team_file::read(&base, &rel).map(|t| json!({ "ok": true, "text": t, "path": rel })) };
            return match out { Ok(v) => Ok(NodeOut { transcript: format!("file {op} {rel}\n"), outputs: v, cost_usd: 0.0, ms: now() - start }), Err(e) => Err(fail(FailKind::Error, e, String::new())) };
        }
        "package" => {
            need(&["network", "write", "exec"]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            return run_package(&init, &node_id, &node, &dir, tool).await;
        }
        "sound" => {
            need(if tool.name == "cue_sheet" { &["read"][..] } else { &["read", "write", "exec"][..] }).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            return run_sound(&init, &node, &dir, tool, &values).await;
        }
        "apply" => {
            need(&["write"]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            let get = |f: &str| value_ref(&values, &Some(format!("values:{f}"))).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string()));
            let files = crate::team_apply::plan(&tool.args["files"], &get).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            let owns = |p: &str| allow.paths.is_empty() || crate::goals::owns_file(&allow.paths, p);
            let done = crate::team_apply::write(&dir, &files, &owns).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            return Ok(NodeOut { transcript: format!("apply {}\n", done.join(", ")), outputs: json!({ "ok": true, "files": done }), cost_usd: 0.0, ms: now() - start });
        }
        "saga" => {
            need(&["read"]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            let mut a = tool.args.clone(); if tool.name != "" && a.get("op").is_none() { a["op"] = json!(tool.name); }
            if matches!(s(&a["op"]).as_str(), "write" | "sync") { need(&["write"]).map_err(|e| fail(FailKind::Error, e, String::new()))?; }
            let (root, actor) = (init.root.clone(), format!("team:{}", crate::team_graph::slug(&init.team)));
            let out = blocking(move || crate::team_saga_node::op(&root, &actor, &a)).await;
            return match out { Some(Ok(v)) => Ok(NodeOut { transcript: "saga\n".into(), outputs: v, cost_usd: 0.0, ms: now() - start }), Some(Err(e)) => Err(fail(FailKind::Error, e, String::new())), None => Err(fail(FailKind::Error, "the saga call was cut off".into(), String::new())) };
        }
        "source" => {
            need(&["read"]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            let rel = tool.name.trim_start_matches('/'); if rel.contains("..") { return Err(fail(FailKind::Error, "a source stays inside the project".into(), String::new())); }
            if !allow.paths.is_empty() && !crate::goals::owns_file(&allow.paths, rel) { return Err(fail(FailKind::Error, format!("{rel} is not in the allow-list"), String::new())); }
            let p = dir.join(rel); let bytes = std::fs::metadata(&p).map(|m| m.len()).map_err(|e| fail(FailKind::Error, format!("{rel}: {e}"), String::new()))?;
            Ok(NodeOut { outputs: json!({ "ok": true, "path": p.to_string_lossy(), "bytes": bytes }), cost_usd: 0.0, transcript: format!("source {rel} ({bytes} bytes)\n"), ms: now() - start })
        }
        "pane" => {
            need(&["read"]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            if !allow.panes.is_empty() && !allow.panes.contains(&tool.name) { return Err(fail(FailKind::Error, format!("the pane {} is not in the allow-list", tool.name), String::new())); }
            let r = crate::cowork_mcp::run_on_screen("open", &json!({ "kind": "studio", "tool": tool.name, "side": "right" }), 6_000).await;
            match r { Ok(v) => Ok(NodeOut { outputs: json!({ "ok": true, "pane": v["pane"] }), cost_usd: 0.0, transcript: format!("opened the {} pane\n", tool.name), ms: now() - start }), Err(e) => Err(fail(FailKind::Error, format!("no screen opened the pane: {e}"), String::new())) }
        }
        // THE BROWSE TOOL: `login` hands a window to the person (the node after it is theirs to press Done on); `capture` brings back screenshots and a reading of each page, signed in, read-only
        "browse" => {
            need(&["network"]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            // a node that names a TARGET reads the app's file now (the name comes from the run's ask: one team, any app)
            let args = if let Some(t) = tool.args["target"].as_str() {
                let name = crate::team_web::fill(t, &init.ask, &values).trim().to_ascii_lowercase();
                if !crate::team_browse::target_name_ok(&name) { return Err(fail(FailKind::Error, format!("the target `{name}` is not a name: say the app (for example `notion`) as the run's ask"), String::new())); }
                let p = crate::team_browse::target_path(&init.root, &name);
                let text = std::fs::read_to_string(&p).map_err(|_| fail(FailKind::Error, format!("no target file for `{name}`: write {} (profile, domains, url, urls, expect)", p.display()), String::new()))?;
                let file: Value = serde_json::from_str(&text).map_err(|e| fail(FailKind::Error, format!("the target file of `{name}` is not valid JSON: {e}"), String::new()))?;
                crate::team_browse::target_output(&file).map_err(|e| fail(FailKind::Error, format!("the target file of `{name}`: {e}"), String::new()))?;
                crate::team_browse::with_target(&tool.args, &file)
            } else { tool.args.clone() };
            let tool = &ToolRef { kind: tool.kind.clone(), name: tool.name.clone(), args };
            let errs = crate::team_browse::check_args(&tool.name, &tool.args); if !errs.is_empty() { return Err(fail(FailKind::Error, errs.join("; "), String::new())); }
            let (profile, domains) = (s(&tool.args["profile"]), tool.args["domains"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default());
            let out = if tool.name == "login" {
                let (url, wait) = (s(&tool.args["url"]), tool.args["wait_s"].as_u64().unwrap_or(600).min(1800));
                let mark = tool.args["mark"].as_bool().unwrap_or(true);
                tokio::task::spawn_blocking(move || crate::team_browse::run_login(&url, &profile, wait, mark)).await
            } else {
                let urls: Vec<String> = tool.args["urls"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                let widths = crate::team_browse::widths_of(&tool.args); let expect = crate::team_browse::expect_of(&tool.args); let o = dir.join("browse");
                tokio::task::spawn_blocking(move || crate::team_browse::run_capture(&urls, &domains, &widths, &profile, &expect, &o)).await
            };
            return match out { Ok(Ok(v)) => { if v["ok"] == json!(false) && v["signed_out"].as_array().map_or(false, |a| !a.is_empty()) { Err(fail(FailKind::Error, format!("signed out: {} Sign in in the window the login step opens, then approve.", v["signed_out"].as_array().map(|a| a.iter().map(|x| format!("{} ({})", x["url"].as_str().unwrap_or(""), x["why"].as_str().unwrap_or(""))).collect::<Vec<_>>().join("; ")).unwrap_or_default()), String::new())) } else { Ok(NodeOut { transcript: format!("browse {}: {}{}\n", tool.name, v["count"].as_u64().map_or("done".to_string(), |n| format!("{n} captures")), tool.args["output"].as_str().map_or(String::new(), |o| format!(" (the pane to build is a `{o}` pane)"))), outputs: { let mut v = v; if let Some(o) = tool.args["output"].as_str() { v["output"] = json!(o); } v }, cost_usd: 0.0, ms: now() - start }) } } Ok(Err(e)) => Err(fail(FailKind::Error, e, String::new())), Err(e) => Err(fail(FailKind::Error, format!("the browse was cut off: {e}"), String::new())) };
        }
        "web" => {
            // THE WEB (§4.4): search or read one page, no model; the effect `network` must be listed; what comes back is plain text the nodes after it are told
            need(&["network"]).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            let key = if tool.name == "fetch" || tool.name == "docs" { "url" } else { "query" }; let want = crate::team_web::fill(tool.args.get(key).and_then(Value::as_str).unwrap_or(""), &init.ask, &values); let max = tool.args.get("max").and_then(Value::as_u64).unwrap_or(5) as usize; let name = tool.name.clone();
            let mut cost = 0.0_f64; let prefer = tool.args.get("engine").and_then(Value::as_str).unwrap_or("auto").to_string(); let init2 = init.clone(); let node2 = node.clone();
            let r = tokio::time::timeout(std::time::Duration::from_secs(node.timeout_s.unwrap_or(120)), async {
                if name == "fetch" { return crate::team_web::fetch(&want, crate::team_web::MAX_TEXT_CHARS).await; }
                if name == "docs" { let dir = Path::new(&init2.root).join(".antiloki").join("teams").join(slug(&init2.graph.team)).join("docs").join(slug(&crate::team_web::allowed_url(&want).map(|u| u.host_str().unwrap_or("site").to_string()).unwrap_or_else(|_| "site".into()))); return crate::team_web::crawl_docs(&want, max, &dir).await; }
                // SEARCH: a key of the person's (Brave) first, then the seat's own web search (a small model call, priced), then the keyless pages as a last resort. A search URL set in the environment is the only engine (a test's, or a self-hosted one).
                let custom = std::env::var("ANTILOKI_WEB_SEARCH_URL").map_or(false, |t| t.contains("{q}")); let mut why: Vec<String> = vec![];
                if !custom && (prefer == "auto" || prefer == "brave") { match crate::team_web::brave(&want, max).await { Some(Ok(v)) => return Ok(v), Some(Err(e)) => why.push(e), None => {} } }
                if !custom && (prefer == "auto" || prefer == "seat") { match seat_search(&init2, &node2, &want, max).await { Ok((v, c)) => { cost += c; return Ok(v); } Err(e) => why.push(format!("seat: {e}")) } }
                if prefer == "auto" || prefer == "scrape" || custom { match crate::team_web::search(&want, max).await { Ok(v) => return Ok(v), Err(e) => why.push(e) } }
                Err(why.join("; "))
            }).await;
            return match r {
                Ok(Ok(v)) => Ok(NodeOut { transcript: format!("web {} {}{}\n{}\n", tool.name, want, v["engine"].as_str().map(|e| format!(" (by {e})")).unwrap_or_default(), v["text"].as_str().unwrap_or("").chars().take(4000).collect::<String>()), outputs: v, cost_usd: cost, ms: now() - start }),
                Ok(Err(e)) => Err(fail(FailKind::Error, e, format!("web {} {}\n", tool.name, want))),
                Err(_) => Err(fail(FailKind::Timeout, format!("web {} {} ran out of time", tool.name, want), String::new())) };
        }
        "mcp" => {
            // an antiloki MCP tool of the screen-control family, called through the serve's own door: only the ones named here, only with the effects the allow-list grants
            let Some(effects) = mcp_effects(&tool.name) else { return Err(fail(FailKind::Error, format!("the MCP tool {} is not one a tool node may call", tool.name), String::new())) };
            need(effects).map_err(|e| fail(FailKind::Error, e, String::new()))?;
            if !allow.mcp.is_empty() && !allow.mcp.contains(&tool.name) { return Err(fail(FailKind::Error, format!("the MCP tool {} is not in the allow-list", tool.name), String::new())); }
            // THE X-RAY NODE (operator 2026-11-16: "anything the agent needs is handed by this node"): the MCP's X-ray tools, run in-process against the project, with `{ask}` and `{node.field}` filled in the arguments.
            // A source that costs a model call is refused unless the node lists `money`.
            if crate::team_xray::is_xray(&tool.name) {
                let args = crate::team_xray::fill_args(&tool.args, &init.ask, &values);
                if let Some(src) = crate::team_xray::ai_source(&tool.name, &args) { if !allow.effects.iter().any(|e| e == "money") { return Err(fail(FailKind::Error, format!("the X-ray source `{src}` costs a model call: list `money` on the node, or use a deterministic source"), String::new())); } }
                let port = std::env::var("ANTILOKI_SERVE_PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(41998);
                let out = Some(crate::team_xray::run(port, &tool.name, &args).await);
                return match out { Some(Ok(text)) => Ok(NodeOut { outputs: json!({ "ok": true, "text": text }), cost_usd: 0.0, transcript: format!("{}\n{}\n", tool.name, text.chars().take(1500).collect::<String>()), ms: now() - start }), Some(Err(e)) => Err(fail(FailKind::Error, e, String::new())), None => Err(fail(FailKind::Error, "the X-ray call was cut off".into(), String::new())) };
            }
            let port = std::env::var("ANTILOKI_SERVE_PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(41998);
            let r = reqwest::Client::new().post(format!("http://127.0.0.1:{port}/api/v1/cockpit/cowork/mcp")).json(&json!({ "tool": tool.name, "args": if tool.args.is_object() { tool.args.clone() } else { json!({}) }, "pane_id": 0 })).timeout(std::time::Duration::from_secs(node.timeout_s.unwrap_or(120))).send().await;
            let v: Value = match r { Ok(x) => x.json().await.unwrap_or(Value::Null), Err(e) => return Err(fail(FailKind::Error, format!("the serve did not answer: {e}"), String::new())) };
            let ok = v["ok"] == json!(true); let text = s(&v["text"]);
            if !ok { return Err(fail(FailKind::Error, text.chars().take(300).collect(), String::new())); }
            Ok(NodeOut { outputs: json!({ "ok": true, "text": text.chars().take(4000).collect::<String>() }), cost_usd: 0.0, transcript: format!("{}\n{}\n", tool.name, text), ms: now() - start })
        }
        "webapp" => Err(fail(FailKind::Error, "web app tools arrive with RFC 0009".into(), String::new())),
        other => Err(fail(FailKind::Error, format!("tool type {other}"), String::new())),
    }
}

/// One edit of one node (by the person in the editor or the drawer, or by the Leader through MCP): the model, the tier, a cap, the scope, the retries. The result must still be a valid graph, or nothing changes. Pure over the graph.
pub fn apply_edit(g: &mut Graph, node: &str, patch: &Value) -> Result<Value, String> {
    let before = g.clone();
    let r = (|| -> Result<(), String> {
        let n = g.nodes.get_mut(node).ok_or_else(|| format!("no node called {node}"))?;
        if let Some(c) = patch.get("cap_usd") { n.cap_usd = c.as_f64().filter(|x| *x > 0.0); }
        if let Some(t) = patch.get("timeout_s") { n.timeout_s = t.as_u64().filter(|x| *x > 0); }
        if let Some(l) = patch.get("label").and_then(Value::as_str) { n.label = Some(l.to_string()).filter(|x| !x.is_empty()); }
        if let Some(p) = patch.get("pos") { n.pos = p.as_array().filter(|a| a.len() == 2).and_then(|a| Some([a[0].as_f64()?, a[1].as_f64()?])); }
        match &mut n.kind {
            Kind::Role { tier, model, owns, readonly, parallel, role, .. } => {
                if let Some(t) = patch.get("tier").and_then(Value::as_str) { *tier = match t { "low" => Some(Tier::Low), "mid" => Some(Tier::Mid), "high" => Some(Tier::High), _ => return Err("tier is low, mid or high".into()) }; }
                if let Some(m) = patch.get("model") { *model = m.as_str().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()); }
                if let Some(o) = patch.get("owns").and_then(Value::as_array) { *owns = o.iter().filter_map(|x| x.as_str().map(String::from)).collect(); }
                if let Some(r) = patch.get("readonly").and_then(Value::as_bool) { if !role.may_edit() && !r { return Err(format!("a {} never edits", role.key())); } *readonly = Some(r); }
                if let Some(p) = patch.get("parallel").and_then(Value::as_u64) { *parallel = Some(p as u32); }
            }
            Kind::Loop { max_iterations, until } => { if let Some(m) = patch.get("max_iterations").and_then(Value::as_u64) { *max_iterations = m as u32; } if let Some(u) = patch.get("until") { *until = u.as_str().map(String::from).filter(|x| !x.is_empty()); } }
            Kind::Retry { max, .. } => { if let Some(m) = patch.get("max").and_then(Value::as_u64) { *max = m as u32; } }
            Kind::If { cond } => { if let Some(c) = patch.get("cond").and_then(Value::as_str) { *cond = c.to_string(); } }
            Kind::Switch { on } => { if let Some(c) = patch.get("on").and_then(Value::as_str) { *on = c.to_string(); } }
            Kind::UseTool { allow, tool, .. } => { if let Some(a) = patch.get("allow") { *allow = serde_json::from_value::<Allow>(a.clone()).map_err(|e| format!("allow: {e}"))?; } if let Some(a) = patch.get("args") { tool.args = a.clone(); } }
            _ => {}
        }
        Ok(())
    })();
    if let Err(e) = r { *g = before; return Err(e); }
    let rep = validate(g); if !rep.ok() { let words = rep.words().join("; "); *g = before; return Err(format!("that edit makes the graph invalid: {words}")); }
    Ok(json!({ "ok": true, "node": g.nodes[node], "report": rep }))
}
/// Move budget between nodes' caps (the LEADER'S allocation, which stays under the CEILING the person set): `moves` `[{from, to, usd}]`; `ceiling` only with `by_person`. Pure over the graph.
pub fn apply_budget(g: &mut Graph, moves: &[Value], ceiling: Option<f64>, by_person: bool) -> Result<Value, String> {
    let before = g.clone();
    if let Some(c) = ceiling { if !by_person { return Err("only the person sets the ceiling: the leader allocates under it".into()); } if c <= 0.0 { return Err("a ceiling is more than zero".into()); } g.budget_usd = c; }
    for m in moves {
        let (from, to, usd) = (s(&m["from"]), s(&m["to"]), m["usd"].as_f64().unwrap_or(0.0)); if usd <= 0.0 { *g = before; return Err("move more than zero dollars".into()); }
        let have = g.nodes.get(&from).ok_or_else(|| format!("no node called {from}")).and_then(|n| n.cap_usd.ok_or_else(|| format!("{from} has no cap to take from")))?;
        if have < usd { *g = before; return Err(format!("{from} has only ${have:.2} of cap")); }
        if !g.nodes.contains_key(&to) { *g = before; return Err(format!("no node called {to}")); }
        g.nodes.get_mut(&from).unwrap().cap_usd = Some(((have - usd) * 10_000.0).round() / 10_000.0); let n = g.nodes.get_mut(&to).unwrap(); n.cap_usd = Some(((n.cap_usd.unwrap_or(0.0) + usd) * 10_000.0).round() / 10_000.0);
    }
    let worst = worst_case_usd(g);
    if g.budget_usd > 0.0 && worst > g.budget_usd * 1.5 { let w = worst; let b = g.budget_usd; *g = before; return Err(format!("that allocation could cost ${w:.2}, far over the ${b:.2} ceiling the person set")); }
    Ok(json!({ "ok": true, "budget_usd": g.budget_usd, "worst_usd": worst, "caps": g.nodes.iter().filter_map(|(k, n)| n.cap_usd.map(|c| (k.clone(), c))).collect::<BTreeMap<_, _>>() }))
}

// ── the driver ────────────────────────────────────────────────────────────────────────────────────────────────────────

struct Driver { init: RunInit, state: RunState, graph: Graph, rx: mpsc::UnboundedReceiver<Ctl>, tx: mpsc::UnboundedSender<Ctl>, tasks: HashMap<String, tokio::task::JoinHandle<()>>, snap: Arc<Mutex<Value>>, tries: BTreeMap<String, u32>, pass_ep: BTreeMap<String, i64>, last_pass_ep: Option<i64>, main_pass: Option<String>, park_info: BTreeMap<String, String>, unit_of_node: BTreeMap<String, String>, cost_ms: i64, pass_seq: u32 }

fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> impl std::future::Future<Output = Option<T>> { async move { tokio::task::spawn_blocking(f).await.ok() } }

impl Driver {
    fn dir_of_run(&self) -> PathBuf { run_dir(&self.init.root, &self.init.id) }
    fn persist(&self) {
        let d = self.dir_of_run(); let _ = std::fs::create_dir_all(&d);
        let v = json!({ "id": self.init.id, "team": self.init.team, "thread": self.init.thread, "ask": self.init.ask, "engine": self.init.engine, "graph": self.graph.to_json(), "state": serde_json::to_value(&self.state).unwrap_or(Value::Null),
            "team_saga": self.init.team_saga, "pack": self.init.pack, "episodes": self.init.episodes, "trace_seq": self.init.trace_seq, "transcripts": self.init.transcripts, "tries": self.tries, "pass_ep": self.pass_ep, "last_pass_ep": self.last_pass_ep, "park_info": self.park_info, "pass_seq": self.pass_seq });
        keep_checkpoint(&self.init.root, &self.init.id, self.state.values.len(), &v);
        let tmp = d.join("state.json.tmp"); if std::fs::write(&tmp, serde_json::to_string(&v).unwrap_or_default()).is_ok() { let _ = std::fs::rename(&tmp, d.join("state.json")); }
        let mut snap = v.clone(); let tr: Vec<Value> = self.state.trace.iter().rev().take(0).map(|t| json!(t)).collect(); let _ = tr; snap["now"] = json!(now()); snap["worst_usd"] = json!(worst_case_usd(&self.graph));
        snap["budget_usd"] = json!(self.graph.budget_usd); snap["spent_usd"] = json!(self.state.spent_usd); snap["status"] = serde_json::to_value(&self.state.status).unwrap_or(Value::Null);
        *self.snap.lock().unwrap_or_else(|p| p.into_inner()) = snap;
    }
    /// Write what the graph said since the last step into the sagas: the trail of the episode it belongs to, and, for a pass, the episode itself. Blocking work in a thread.
    async fn record(&mut self, passes: Vec<(String, u32)>) {
        let trace: Vec<TraceEv> = std::mem::take(&mut self.state.trace); if trace.is_empty() && passes.is_empty() { return; }
        // A TEAM WITHOUT A MAIN LOOP STILL LEAVES A SAGA (the operator 2026-11-15: "sagas are not written when running"): the run itself is the one episode, opened with its first step and closed when the run ends.
        let mut passes = passes; if self.main_pass.is_none() && self.pass_seq == 0 && passes.is_empty() { passes.push(("run".to_string(), 1)); }
        let ask_title: String = self.init.ask.chars().take(80).collect();
        let (root, team, run, saga, seq0) = (self.init.root.clone(), self.init.team.clone(), self.init.id.clone(), self.init.team_saga, self.init.trace_seq);
        let main = self.main_pass.clone(); let last = self.last_pass_ep; let mut seq_pass = self.pass_seq; let units: BTreeMap<String, i64> = self.init.pack.clone(); let unit_now = unit_of(&self.graph, &self.state.values, &self.init.ask).id;
        let cur_unit_saga = units.get(&unit_now).copied(); let mut eps = self.init.episodes.clone();
        let out = blocking(move || -> Result<(usize, Option<i64>, BTreeMap<String, i64>, u32), String> {
            let c = crate::work::open(&root)?; let mut last = last; let mut seq = seq0;
            for (node, n) in &passes {
                if Some(node) == main.as_ref() || main.is_none() {
                    // a pass of the main loop is a new episode of the team saga, numbered through the whole run (every saga of the pack has a pass 1); a pass after the first of the same saga retries the one before it
                    seq_pass += 1; let sq = seq_pass; let prev = last; let retry = *n > 1; let ext = format!("tg:{run}:pass:{sq}");
                    let meta = json!({ "run": run, "path": format!("{unit_now} › {node} pass {n}"), "pass": sq, "unit": unit_now, "unit_pass": n, "retry_of": if retry { prev } else { None }, "node": node });
                    if let Some(p) = last { let _ = close_episode(&c, p, if retry { "failed" } else { "done" }, 0.0, 0); }
                    let title = if node == "run" { format!("Run · {}", if ask_title.trim().is_empty() { "no ask" } else { ask_title.trim() }) } else { format!("Pass {sq} · {unit_now}") }; let id = open_episode(&c, saga, &title, &ext, &meta)?; last = Some(id); eps.insert(format!("{node}:{sq}"), id);
                    if let Some(us) = cur_unit_saga { let _ = open_episode(&c, us, &format!("Pass {n}"), &format!("tg:{run}:{us}:pass:{n}"), &json!({ "run": run, "pass": n, "produced_by": id, "path": format!("{node} › pass {n}"), "retry_of": if retry { Some(format!("pass {}", n - 1)) } else { None } })); }
                }
            }
            for ev in &trace { seq += 1; trace_event(&c, saga, last, &run, seq, ev)?; if let Some(us) = cur_unit_saga { if !ev.node.is_empty() { let _ = trace_event(&c, us, None, &run, seq, ev); } } }
            let _ = team; Ok((seq, last, eps, seq_pass))
        }).await;
        if let Some(Ok((seq, last, eps, sp))) = out { self.init.trace_seq = seq; self.last_pass_ep = last; self.init.episodes = eps; self.pass_seq = sp; }
    }
    async fn finish_sagas(&mut self) {
        let (root, ok) = (self.init.root.clone(), matches!(self.state.status, RunStatus::Done)); let last = self.last_pass_ep; let packs: Vec<i64> = self.init.pack.values().copied().collect(); let spent = self.state.spent_usd; let ms = now() - self.state.started_ms;
        let _ = blocking(move || { if let Ok(c) = crate::work::open(&root) { if let Some(p) = last { let _ = close_episode(&c, p, if ok { "done" } else { "failed" }, spent, ms); } if ok { for sg in packs { let _ = c.execute("UPDATE tasks SET status = 'done', done_ms = ?1 WHERE saga_id = ?2 AND status = 'doing'", rusqlite::params![now(), sg]); } } } }).await;
    }
    fn spawn_node(&mut self, node_id: &str) {
        let Some(node) = self.graph.node(node_id).cloned() else { return };
        let init = Arc::new(self.init.clone()); let values = self.state.values.clone(); let tx = self.tx.clone(); let id = node_id.to_string();
        let n_try = { let t = self.tries.entry(id.clone()).or_insert(0); *t += 1; *t };
        let unit = unit_of(&self.graph, &self.state.values, &self.init.ask); let (root, thread) = (self.init.root.clone(), self.init.thread.clone());
        self.unit_of_node.insert(id.clone(), unit.id.clone());
        let h = tokio::spawn(async move {
            // the node's own folder: the saga's tree (cut from the team's), made the first time it is needed
            let uid = tree_key(&init.id, &unit.id); let (r2, t2) = (root.clone(), thread.clone()); let sub = branch_label(&init.graph, &id);
            let dir = match blocking(move || match sub { Some(b) => crate::thread_tree::ensure_subunit(&r2, &t2, &uid, &b), None => crate::thread_tree::ensure_unit(&r2, &t2, &uid) }).await { Some(Ok((d, _))) => d, Some(Err(e)) => { let _ = tx.send(Ctl::Done(NodeMsg { node: id, result: Err(NodeFail { kind: FailKind::Error, error: format!("no folder to work in: {e}"), cost_usd: 0.0, transcript: String::new() }) })); return; } None => return };
            let result = match &node.kind { Kind::Role { .. } => run_role(init.clone(), id.clone(), node.clone(), values, dir, n_try).await, Kind::UseTool { .. } => run_tool(init.clone(), id.clone(), node.clone(), dir, values).await, Kind::Artifact { .. } => run_artifact(init.clone(), id.clone(), node.clone(), values).await, Kind::Check { .. } => run_check(init.clone(), id.clone(), node.clone(), values).await, Kind::Llm(_) => run_llm(init.clone(), id.clone(), node.clone(), values).await, Kind::Decompose(_) => run_decompose(init.clone(), id.clone(), node.clone(), dir).await, Kind::Test(_) => run_test(init.clone(), id.clone(), node.clone(), dir).await, Kind::Contract(_) => run_contract(init.clone(), id.clone(), node.clone(), dir).await, Kind::Stubs(_) => run_stubs(init.clone(), id.clone(), node.clone(), dir).await, Kind::Review(_) => run_review(init.clone(), id.clone(), node.clone(), dir).await, Kind::Enforce(_) => run_enforce(init.clone(), id.clone(), node.clone(), values).await, Kind::Skeleton(_) => run_skeleton(init.clone(), id.clone(), node.clone(), dir).await, Kind::Cell(_) => run_cell(init.clone(), id.clone(), node.clone(), values).await, Kind::Recurse { .. } => run_recurse(init.clone(), id.clone(), node.clone(), values, dir).await, Kind::Evolve { .. } => run_evolve(init.clone(), id.clone(), node.clone()).await, Kind::Code { .. } => run_code(init.clone(), id.clone(), node.clone(), values).await, Kind::Transform { .. } => run_transform(init.clone(), id.clone(), node.clone(), values).await, Kind::SubTeam { .. } => run_subteam(init.clone(), id.clone(), node.clone(), values).await, _ => Err(NodeFail { kind: FailKind::Error, error: "not a runnable node".into(), cost_usd: 0.0, transcript: String::new() }) };
            let _ = tx.send(Ctl::Done(NodeMsg { node: id, result }));
        });
        self.tasks.insert(node_id.to_string(), h);
    }
    async fn save_transcript(&mut self, node: &str, text: &str) {
        if text.is_empty() { return; }
        let d = self.dir_of_run().join("transcripts"); let _ = std::fs::create_dir_all(&d); let k = self.init.transcripts.entry(node.to_string()).or_default().len() + 1;
        let f = format!("{node}-{k}.txt"); let _ = std::fs::write(d.join(&f), text); self.init.transcripts.entry(node.to_string()).or_default().push(f);
    }
    /// Carry out what the graph asked for. Returns the follow-up events the actions imply (none, today: nodes answer by their own messages).
    async fn apply(&mut self, actions: Vec<Action>) {
        let mut passes = vec![];
        for a in &actions { if let Action::Pass { node, n } = a { passes.push((node.clone(), *n)); } }
        // the planner's sagas become sagas of the pack the moment the plan is written (the runs after it write into them)
        let joined: Vec<String> = self.state.trace.iter().filter(|t| t.kind == "join").map(|t| t.node.clone()).collect();
        self.record(passes).await;
        for j in joined { self.integrate_branches(&j).await; }
        for a in actions {
            match a {
                Action::Run { node } => self.spawn_node(&node),
                Action::Cancel { node } => { if let Some(h) = self.tasks.remove(&node) { h.abort(); } }
                Action::Park { node, gate } => { self.park_info.insert(node, gate); }
                Action::Deliver | Action::Handoff { .. } | Action::Pass { .. } => {}
            }
        }
        self.persist();
    }
    async fn step(&mut self, ev: Event) { match graph_step(&self.graph, &mut self.state, ev) { Ok(a) => self.apply(a).await, Err(e) => { self.state.trace.push(TraceEv { t_ms: now(), node: String::new(), kind: "error".into(), text: e }); self.record(vec![]).await; self.persist(); } } }
    /// After the planner answers: make the pack's sagas, so the Sagas screen shows them before any work.
    async fn make_pack(&mut self, outputs: &Value) {
        let units: Vec<Value> = outputs["sagas"].as_array().cloned().unwrap_or_default(); if units.is_empty() { return; }
        let (root, team, run) = (self.init.root.clone(), self.init.team.clone(), self.init.id.clone());
        let ids = blocking(move || -> Vec<(String, i64)> { let Ok(c) = crate::work::open(&root) else { return vec![] }; units.iter().filter_map(|u| ensure_pack_saga(&c, &team, &run, u).ok().map(|id| (s(&u["id"]), id))).collect() }).await.unwrap_or_default();
        for (u, id) in ids { self.init.pack.insert(u, id); }
    }
    async fn on_done(&mut self, m: NodeMsg) {
        if !self.state.active.contains_key(&m.node) { return; }   // cancelled while it finished
        self.tasks.remove(&m.node);
        match m.result {
            Ok(o) => { self.save_transcript(&m.node, &o.transcript).await; self.cost_ms += o.ms; if matches!(self.graph.node(&m.node).map(|n| &n.kind), Some(Kind::Role { role: RoleKind::Planner, .. })) { self.make_pack(&o.outputs).await; } self.step(Event::NodeDone { node: m.node.clone(), outputs: o.outputs, cost_usd: o.cost_usd }).await; self.after_unit_step(&m.node).await; }
            Err(f) => { let said = if f.transcript.trim().is_empty() { format!("FAILED ({:?}): {}\n", f.kind, f.error) } else { format!("{}\n\nFAILED ({:?}): {}\n", f.transcript.trim_end(), f.kind, f.error) }; self.save_transcript(&m.node, &said).await; self.step(Event::NodeFailed { node: m.node, kind: f.kind, error: f.error, cost_usd: f.cost_usd }).await; }
        }
    }
    /// A pack saga that has passed is brought into the team's branch when the for-each moves on (or the run is delivered): the team's branch holds the work, the workspace does not until the merge is approved.
    async fn after_unit_step(&mut self, _node: &str) {
        let Some(each) = self.graph.nodes.iter().find(|(_, n)| matches!(n.kind, Kind::ForEach { .. })).map(|(k, _)| k.clone()) else { return };
        let idx = self.state.values.get(&each).and_then(|v| v["index"].as_u64()).unwrap_or(0) as usize; let done_units: Vec<String> = self.init.pack.keys().cloned().collect(); let _ = (idx, done_units);
        self.integrate_finished().await;
    }
    async fn integrate_finished(&mut self) {
        // a unit is finished when the for-each has moved past it, or the run is done
        let each = self.graph.nodes.iter().find(|(_, n)| matches!(n.kind, Kind::ForEach { .. })).map(|(k, _)| k.clone());
        let items: Vec<String> = each.as_ref().and_then(|e| self.state.values.get(e)).map(|_| vec![]).unwrap_or_default(); let _ = items;
        let cur = unit_of(&self.graph, &self.state.values, &self.init.ask).id; let finished = matches!(self.state.status, RunStatus::Done);
        let plan_units: Vec<String> = self.state.values.values().find_map(|v| v.get("sagas").and_then(Value::as_array)).map(|a| a.iter().map(|u| s(&u["id"])).collect()).unwrap_or_default();
        let mut to_integrate: Vec<String> = vec![];
        for (i, u) in plan_units.iter().enumerate() { let cur_i = plan_units.iter().position(|x| *x == cur).unwrap_or(0); if (i < cur_i || finished) && self.unit_of_node.values().any(|x| x == u) && !self.integrated().contains(u) { to_integrate.push(u.clone()); } }
        if plan_units.is_empty() && finished && !self.integrated().iter().any(|u| u == "run") { to_integrate.push("run".into()); }
        for u in to_integrate { self.integrate_unit(&u).await; }
    }
    /// At a join the branches that arrived are brought into the saga's own tree (a conflict is told and the run goes on to the debugger or the person, never forced).
    async fn integrate_branches(&mut self, join: &str) {
        let arrived: Vec<String> = self.state.arrivals.get(join).map(|a| a.iter().cloned().collect()).unwrap_or_default();
        let mut labels: Vec<String> = arrived.iter().filter_map(|n| branch_label(&self.graph, n)).collect(); labels.sort(); labels.dedup(); if labels.is_empty() { return; }
        let unit = tree_key(&self.init.id, &unit_of(&self.graph, &self.state.values, &self.init.ask).id); let (root, thread) = (self.init.root.clone(), self.init.thread.clone());
        let r = blocking(move || -> Result<Value, String> { let (utree, ubranch) = crate::thread_tree::ensure_unit(&root, &thread, &unit)?; let mut boxes = vec![]; for l in &labels { let (d, _) = crate::thread_tree::ensure_subunit(&root, &thread, &unit, l)?; boxes.push((l.clone(), 0, d)); } let out = crate::thread_tree::integrate_tree(&utree, &ubranch, &thread, &boxes); if out.as_ref().map_or(false, |v| v["ok"] == json!(true)) { for l in &labels { crate::thread_tree::discard_unit(&root, &thread, &format!("{unit}--{l}")); } } out }).await;
        let (kind, text) = match r { Some(Ok(v)) if v["ok"] == json!(true) => ("integrate", format!("the branches of {join} were brought together: {} merged", v["merged"].as_array().map_or(0, |a| a.len()))), Some(Ok(v)) => ("conflict", format!("the branches of {join} conflict: {}", v["conflicts"])), Some(Err(e)) => ("conflict", format!("the branches of {join} could not be brought together: {e}")), None => ("conflict", "integrate was cut off".into()) };
        self.state.trace.push(TraceEv { t_ms: now(), node: join.to_string(), kind: kind.into(), text }); self.record(vec![]).await;
    }
    fn integrated(&self) -> Vec<String> { self.state.values.get("_integrated").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() }
    async fn integrate_unit(&mut self, unit: &str) {
        let (root, thread, u, key) = (self.init.root.clone(), self.init.thread.clone(), unit.to_string(), tree_key(&self.init.id, unit)); let run_id = self.init.id.clone(); let team_name = self.init.graph.team.clone();
        // the unit's tree is this run's own; once its work is in the team's branch (no conflict) the tree and its branch are taken away, so runs do not pile up trees (and the documents their agents left in them)
        let r = blocking(move || -> Result<Value, String> { let (dir, _) = crate::thread_tree::ensure_unit(&root, &thread, &key)?; crate::team_media::keep(&root, &run_id, &dir); crate::team_archive::store_tree(&crate::team_archive::Ctx { root: root.clone(), run: run_id.clone(), team: team_name.clone(), node: u.clone() }, &dir, "the folder is removed once its work is in the team's branch");   /* the video outlives the folder (found on the operator's video team: the first approve took the folder away and the player had nothing to play) */ let out = crate::thread_tree::integrate(&root, &thread, &[(u.clone(), 0, dir)]); if out.as_ref().map_or(false, |v| v["ok"] == json!(true) && v["conflicts"].as_array().map_or(true, |c| c.is_empty())) { crate::thread_tree::discard_unit(&root, &thread, &key); } out }).await;
        let (ok, text) = match r { Some(Ok(v)) => (v["ok"] == json!(true), format!("{unit} brought into the team's branch: {} merged, {} conflict(s)", v["merged"].as_array().map_or(0, |a| a.len()), v["conflicts"].as_array().map_or(0, |a| a.len()))), Some(Err(e)) => (false, format!("{unit} could not be brought into the team's branch: {e}")), None => (false, "integrate was cut off".into()) };
        let mut list = self.integrated(); list.push(unit.to_string()); self.state.values.insert("_integrated".into(), json!(list));
        self.state.trace.push(TraceEv { t_ms: now(), node: String::new(), kind: if ok { "integrate".into() } else { "conflict".into() }, text }); self.record(vec![]).await; self.persist();
    }
    async fn answer(&mut self, node: String, approve: bool) -> Result<(), String> {
        let gate = self.park_info.get(&node).cloned().unwrap_or_default();
        if approve && gate == "merge" {
            // the pack lands on the workspace only now, and only by the person's word
            let mut units: Vec<String> = self.init.pack.keys().cloned().collect(); if units.is_empty() { units.push("run".into()); }   // a team with no planner works in one tree of its own, "run"
            for u in units { if !self.integrated().contains(&u) { self.integrate_unit(&u).await; } }
            let (root, thread) = (self.init.root.clone(), self.init.thread.clone());
            let r = blocking(move || crate::thread_tree::land(&root, &thread)).await;
            match r { Some(Ok(v)) if v["ok"] == json!(true) => { self.state.trace.push(TraceEv { t_ms: now(), node: node.clone(), kind: "land".into(), text: "the pack landed on the workspace".into() });
                    // the person's own work, set aside by their click before the land, comes back now
                    let (r2, run2) = (self.init.root.clone(), self.init.id.clone()); if let Some(res) = blocking(move || crate::team_land::restore(&r2, &run2)).await { match res { Ok(v) if v["restored"] == json!(true) => self.state.trace.push(TraceEv { t_ms: now(), node: node.clone(), kind: "restored".into(), text: "your own changes are back in the workspace".into() }), Err(e) => self.state.trace.push(TraceEv { t_ms: now(), node: node.clone(), kind: "restore_failed".into(), text: e }), _ => {} } } }
                Some(Ok(v)) => { let e = s(&v["error"]); self.state.trace.push(TraceEv { t_ms: now(), node: node.clone(), kind: "land_failed".into(), text: e.clone() }); self.record(vec![]).await; self.persist(); return Err(e); }
                Some(Err(e)) => { self.state.trace.push(TraceEv { t_ms: now(), node: node.clone(), kind: "land_failed".into(), text: e.clone() }); self.record(vec![]).await; self.persist(); return Err(e); }
                None => return Err("landing was cut off".into()) }
        }
        // A MERGE THAT IS NOT APPROVED LEAVES THE WORK WHERE IT IS, AND SAVES IT TOO: whatever the folders hold goes into the archive, so nothing a team made can be lost whatever happens to the folders later
        if !approve && gate == "merge" { let (root, thread, id, team) = (self.init.root.clone(), self.init.thread.clone(), self.init.id.clone(), self.init.graph.team.clone()); let mut units: Vec<String> = self.init.pack.keys().cloned().collect(); if units.is_empty() { units.push("run".into()); }
            let _ = blocking(move || { for u in units { if let Ok((dir, _)) = crate::thread_tree::ensure_unit(&root, &thread, &tree_key(&id, &u)) { crate::team_archive::store_tree(&crate::team_archive::Ctx { root: root.clone(), run: id.clone(), team: team.clone(), node: u }, &dir, "the merge was not approved"); } } }).await; }
        self.park_info.remove(&node); self.step(Event::Answer { node, approve }).await; Ok(())
    }
    fn edit_node(&mut self, node: &str, patch: &Value) -> Result<Value, String> {
        let r = apply_edit(&mut self.graph, node, patch)?; let _ = save_graph(&self.init.root, &self.graph);
        self.state.trace.push(TraceEv { t_ms: now(), node: node.to_string(), kind: "edit".into(), text: format!("changed: {}", patch.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>().join(", ")).unwrap_or_default()) });
        Ok(r)
    }
    async fn main(mut self) {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(1000));
        if self.state.status == RunStatus::Idle { let seed: Map<String, Value> = Map::new(); self.step(Event::Start { now_ms: now(), seed }).await; }
        else { // a run found on disk: what was working when the serve stopped is started again
            let active: Vec<String> = self.state.active.keys().cloned().collect(); for n in active { if matches!(self.graph.node(&n).map(|x| &x.kind), Some(Kind::Role { .. }) | Some(Kind::UseTool { .. }) | Some(Kind::Artifact { .. }) | Some(Kind::Check { .. }) | Some(Kind::Llm(_)) | Some(Kind::Decompose(_)) | Some(Kind::Test(_)) | Some(Kind::Contract(_)) | Some(Kind::Stubs(_)) | Some(Kind::Review(_)) | Some(Kind::Enforce(_)) | Some(Kind::Skeleton(_)) | Some(Kind::Cell(_)) | Some(Kind::Recurse { .. }) | Some(Kind::Evolve { .. }) | Some(Kind::Code { .. }) | Some(Kind::Transform { .. }) | Some(Kind::SubTeam { .. })) { self.spawn_node(&n); } }
            self.state.trace.push(TraceEv { t_ms: now(), node: String::new(), kind: "resume".into(), text: "the serve restarted: the run goes on from its last checkpoint".into() }); self.record(vec![]).await; self.persist(); }
        loop {
            if self.state.finished() { break; }
            tokio::select! {
                m = self.rx.recv() => { let Some(m) = m else { break }; match m {
                    Ctl::Done(d) => self.on_done(d).await,
                    Ctl::Pause => self.step(Event::Pause).await, Ctl::Resume => self.step(Event::Resume).await,
                    Ctl::Stop => { let ids: Vec<String> = self.tasks.keys().cloned().collect(); for i in ids { if let Some(h) = self.tasks.remove(&i) { h.abort(); } } self.step(Event::Stop).await; }
                    Ctl::Answer { node, approve, reply } => { let r = self.answer(node, approve).await; let _ = reply.send(r); }
                    Ctl::Edit { node, patch, reply } => { let r = self.edit_node(&node, &patch); let _ = reply.send(r); self.persist(); }
                    Ctl::Alloc { moves, ceiling, by_person, reply } => { let r = apply_budget(&mut self.graph, &moves, ceiling, by_person); if r.is_ok() { let _ = save_graph(&self.init.root, &self.graph); self.state.trace.push(TraceEv { t_ms: now(), node: String::new(), kind: "budget".into(), text: format!("the budget was moved ({} move(s), ceiling ${:.2})", moves.len(), self.graph.budget_usd) }); self.record(vec![]).await; self.persist(); } let _ = reply.send(r); }
                    Ctl::Restart { node, reply } => { if !self.state.active.contains_key(&node) { let _ = reply.send(Err(format!("{node} is not working"))); } else { if let Some(h) = self.tasks.remove(&node) { h.abort(); } self.state.trace.push(TraceEv { t_ms: now(), node: node.clone(), kind: "restart".into(), text: "started again with what it was changed to".into() }); self.spawn_node(&node); self.record(vec![]).await; self.persist(); let _ = reply.send(Ok(())); } }
                    Ctl::Note { node, kind, text } => { self.state.trace.push(TraceEv { t_ms: now(), node, kind, text }); self.record(vec![]).await; self.persist(); }
                } }
                _ = tick.tick() => { self.step(Event::Tick { now_ms: now() }).await; }
            }
        }
        for (_, h) in self.tasks.drain() { h.abort(); }
        self.finish_sagas().await; self.integrate_finished().await; self.persist();
        // THE MASTER PAGE IS MADE BY ITSELF (the operator 2026-11-20: "deliver should just get the final report, it should be automatic"): a run that finished and has no result page of its own gets the default one from what it made, so Deliver and Result and files show it without a click. A failure here is not the run's failure.
        if matches!(self.state.status, RunStatus::Done) { let (r, id) = (self.init.root.clone(), self.init.id.clone()); let _ = blocking(move || { if result_of(&r, &id).is_err() { let _ = compose_auto(&r, &id); } }).await; }
        let _ = audit_and_notify(&self.init.root, &self.init.id).await;   // the self-improving engine: every run is audited; a result that is not good is told to the Leader
        runs().lock().unwrap_or_else(|p| p.into_inner()).retain(|_, h| h.id != self.init.id || false);
        crate::run_preview::stop_run(&self.init.id);   // a preview lives as long as its run
    }
}

/// Start a run of a team's graph over an ask. Returns the run's handle. The team thread, its saga and the run's folders are made here.
pub async fn start(root: &str, g: Graph, ask: &str, engine: &str) -> Result<RunHandle, String> {
    if !crate::team_subteam::subteams(&g).is_empty() { crate::team_subteam::check(root, &g)?; }
    let rep = crate::team_skills::report_for(root, &g, true); if !rep.ok() { return Err(format!("this graph cannot run: {}", rep.words().join("; "))); }
    if ask.trim().is_empty() { return Err("say what the team is to do".into()); }
    let run = format!("{}-{}", slug(&g.team), now() % 1_000_000_000);   // milliseconds: runs started in the same second (a campaign's arms) must not share an id
    let (r2, t2) = (root.to_string(), slug(&g.team)); let g2 = g.clone();
    let (team_saga, thread) = blocking(move || -> Result<(i64, String), String> {
        crate::thread_tree::ensure(&r2, &t2).map_err(|e| format!("the team's tree: {e}"))?;
        let c = crate::work::open(&r2)?; Ok((ensure_team_saga(&c, &g2.team, &g2.team)?, t2))
    }).await.ok_or("cut off")??;
    let (tx, rx) = mpsc::unbounded_channel(); let snap = Arc::new(Mutex::new(Value::Null));
    let init = RunInit { id: run.clone(), team: g.team.clone(), root: root.to_string(), thread, graph: g.clone(), ask: ask.to_string(), engine: engine.to_string(), state: RunState::new(), team_saga, pack: BTreeMap::new(), episodes: BTreeMap::new(), trace_seq: 0, transcripts: BTreeMap::new() };
    let h = RunHandle { id: run.clone(), team: g.team.clone(), root: root.to_string(), ctl: tx.clone(), snap: snap.clone() };
    runs().lock().unwrap_or_else(|p| p.into_inner()).insert(run, h.clone());
    let d = Driver { state: RunState::new(), graph: g.clone(), main_pass: main_pass_node(&g), init, rx, tx, tasks: HashMap::new(), snap, tries: BTreeMap::new(), pass_ep: BTreeMap::new(), last_pass_ep: None, park_info: BTreeMap::new(), unit_of_node: BTreeMap::new(), cost_ms: 0, pass_seq: 0 };
    d.persist(); tokio::spawn(d.main());
    Ok(h)
}
/// Runs that were going when the serve stopped are taken up again from their last checkpoint. Called once at start.
pub async fn resume_all(root: &str) -> usize {
    let dir = Path::new(root).join(".antiloki").join("team"); let mut n = 0;
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() { if resume_dir(root, &e.path()) { n += 1; } }
    n
}
/// Take one run up again from the state file in `dir`. False when it is finished, idle, unreadable or already going.
fn resume_dir(root: &str, dir: &Path) -> bool {
    let Ok(t) = std::fs::read_to_string(dir.join("state.json")) else { return false }; let Ok(v) = serde_json::from_str::<Value>(&t) else { return false };
    let Ok(state) = serde_json::from_value::<RunState>(v["state"].clone()) else { return false }; if state.finished() || state.status == RunStatus::Idle { return false; }
    let Ok(graph) = Graph::from_json(&v["graph"]) else { return false }; let id = s(&v["id"]); if handle(&id).is_some() { return false; }
    let (tx, rx) = mpsc::unbounded_channel(); let snap = Arc::new(Mutex::new(v.clone()));
    let init = RunInit { id: id.clone(), team: s(&v["team"]), root: root.to_string(), thread: s(&v["thread"]), graph: graph.clone(), ask: s(&v["ask"]), engine: s(&v["engine"]), state: state.clone(), team_saga: v["team_saga"].as_i64().unwrap_or(0), pack: serde_json::from_value(v["pack"].clone()).unwrap_or_default(), episodes: serde_json::from_value(v["episodes"].clone()).unwrap_or_default(), trace_seq: v["trace_seq"].as_u64().unwrap_or(0) as usize, transcripts: serde_json::from_value(v["transcripts"].clone()).unwrap_or_default() };
    let h = RunHandle { id: id.clone(), team: init.team.clone(), root: root.to_string(), ctl: tx.clone(), snap: snap.clone() };
    runs().lock().unwrap_or_else(|p| p.into_inner()).insert(id, h);
    let d = Driver { state, main_pass: main_pass_node(&graph), graph, init, rx, tx, tasks: HashMap::new(), snap, tries: serde_json::from_value(v["tries"].clone()).unwrap_or_default(), pass_ep: serde_json::from_value(v["pass_ep"].clone()).unwrap_or_default(), last_pass_ep: v["last_pass_ep"].as_i64(), park_info: serde_json::from_value(v["park_info"].clone()).unwrap_or_default(), unit_of_node: BTreeMap::new(), cost_ms: 0, pass_seq: v["pass_seq"].as_u64().unwrap_or(0) as u32 };
    tokio::spawn(d.main()); true
}
/// CHECKPOINTS (UI-18): the run's state as it was each time a node finished, kept beside the run (`checkpoints/<k>.json`, k = how many nodes had written a value).
/// A rewind starts a NEW run from one of them (the old run is left as it is, so the old attempt stays); the new run's file says which run and checkpoint it came from.
pub fn checkpoint_dir(root: &str, run: &str) -> PathBuf { run_dir(root, run).join("checkpoints") }
/// Keep the state in `v` as checkpoint k when it is not kept yet (k = number of nodes that have written a value; 0 is not a checkpoint).
pub fn keep_checkpoint(root: &str, run: &str, k: usize, v: &Value) { if k == 0 { return; } let d = checkpoint_dir(root, run); let p = d.join(format!("{k:03}.json")); if p.exists() { return; } let _ = std::fs::create_dir_all(&d); let _ = std::fs::write(p, v.to_string()); }
/// The checkpoints of a run, oldest first: what finished at each (the value keys the checkpoint has that the one before did not), the spend so far, and the runs that were started from it.
pub fn checkpoints(root: &str, run: &str) -> Vec<Value> {
    let mut files: Vec<(usize, PathBuf)> = std::fs::read_dir(checkpoint_dir(root, run)).into_iter().flatten().flatten().filter_map(|e| { let n = e.file_name().to_string_lossy().trim_end_matches(".json").parse::<usize>().ok()?; Some((n, e.path())) }).collect(); files.sort();
    let kids: Vec<(String, usize)> = runs_of(root, "").iter().filter_map(|r| { let st = run_on_disk(root, r["id"].as_str()?)?; if st["parent"]["run"].as_str()? == run { Some((s(&r["id"]), st["parent"]["k"].as_u64()? as usize)) } else { None } }).collect();
    let mut prev: std::collections::BTreeSet<String> = Default::default();
    files.into_iter().filter_map(|(k, p)| { let v: Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?; let keys: std::collections::BTreeSet<String> = v["state"]["values"].as_object()?.keys().cloned().collect();
        let new: Vec<String> = keys.difference(&prev).cloned().collect(); prev = keys;
        Some(json!({ "k": k, "nodes": new, "spent_usd": v["state"]["spent_usd"], "t_ms": v["state"]["now_ms"], "runs": kids.iter().filter(|(_, kk)| *kk == k).map(|(r, _)| r.clone()).collect::<Vec<_>>() })) }).collect()
}
/// Start a new run from checkpoint k of `run`: the same graph, the state as it was then (what ran after is not carried), taken up the way a run is after a restart. Returns the new run's id.
pub fn rewind(root: &str, run: &str, k: usize) -> Result<String, String> {
    if !run.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return Err("that is not a run id".into()); }
    let t = std::fs::read_to_string(checkpoint_dir(root, run).join(format!("{k:03}.json"))).map_err(|_| format!("run {run} has no checkpoint {k}"))?;
    let mut v: Value = serde_json::from_str(&t).map_err(|e| e.to_string())?;
    let n = (1..).find(|n| !run_dir(root, &format!("{run}-r{n}")).exists()).unwrap_or(1); let id = format!("{run}-r{n}");
    v["id"] = json!(id); v["parent"] = json!({ "run": run, "k": k });
    let d = run_dir(root, &id); std::fs::create_dir_all(&d).map_err(|e| e.to_string())?; std::fs::write(d.join("state.json"), v.to_string()).map_err(|e| e.to_string())?;
    if !resume_dir(root, &d) { return Err("the checkpoint is of a run that had already ended: there is nothing to take up".into()); }
    Ok(id)
}
/// The run on disk (for one that is not live any more): its last checkpoint.
pub fn run_on_disk(root: &str, run: &str) -> Option<Value> { serde_json::from_str(&std::fs::read_to_string(run_dir(root, run).join("state.json")).ok()?).ok() }
/// The summary of every run of a team, newest first. A run's `state.json` is read and parsed only when it has changed since the last time (its size or modification time differ): this is asked on every open of a team's pane, and reading every run of the workspace each time made it slower with every run ever made.
pub fn runs_of(root: &str, team: &str) -> Vec<Value> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, (std::time::SystemTime, u64, Value)>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default); let dir = Path::new(root).join(".antiloki").join("team"); let mut v: Vec<Value> = vec![];
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let f = e.path().join("state.json"); let Ok(md) = std::fs::metadata(&f) else { continue }; let (mt, len) = (md.modified().unwrap_or(std::time::UNIX_EPOCH), md.len());
        let hit = cache.lock().unwrap_or_else(|p| p.into_inner()).get(&f).filter(|(m, l, _)| *m == mt && *l == len).map(|(_, _, s)| s.clone());
        let sum = match hit { Some(s) => s, None => {
            let Some(x) = std::fs::read_to_string(&f).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { continue };
            let sum = json!({ "id": x["id"], "team": x["team"], "ask": x["ask"], "status": x["state"]["status"], "needs_you": x["state"]["active"].as_object().map_or(false, |a| a.keys().any(|k| x["graph"]["nodes"][k]["node"] == json!("human") || x["park_info"][k].is_string())), "spent_usd": x["state"]["spent_usd"], "started_ms": x["state"]["started_ms"] });
            cache.lock().unwrap_or_else(|p| p.into_inner()).insert(f, (mt, len, sum.clone())); sum } };
        // a run an INNER-TEAM node started says whose child it is (written by `run_subteam`), so the parent can show that it waits on the person inside its child
        let mut sum = sum; if let Some(p) = std::fs::read_to_string(e.path().join("parent.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) { sum["parent"] = p; }
        v.push(sum);
    }
    mark_waiting_parents(&mut v);
    v.retain(|r| s(&r["team"]) == slug(team) || team.is_empty());
    v.sort_by(|a, b| b["started_ms"].as_i64().cmp(&a["started_ms"].as_i64())); v
}
/// A run whose inner team is parked at a human step is itself waiting for the person: `waiting_on_child` names the child run and team (the parent's own `needs_you` stays false, so the person is counted once, at the child's gate). Pure.
pub fn mark_waiting_parents(runs: &mut Vec<Value>) {
    let waiting: Vec<(String, String, String)> = runs.iter().filter(|r| r["needs_you"] == json!(true) && r["parent"]["run"].is_string()).map(|r| (s(&r["parent"]["run"]), s(&r["id"]), s(&r["team"]))).collect();
    for r in runs.iter_mut() { let id = s(&r["id"]); if let Some((_, child, team)) = waiting.iter().find(|w| w.0 == id) { r["waiting_on_child"] = json!({ "run": child, "team": team }); } }
}
/// A node's transcripts of a run (every attempt, oldest first) and the latest text. Reads files, never traverses out of the run's folder.
pub fn node_view(root: &str, run: &str, node: &str) -> Value {
    let st = handle(run).map(|h| h.snap.lock().unwrap_or_else(|p| p.into_inner()).clone()).or_else(|| run_on_disk(root, run)).unwrap_or(Value::Null);
    let files: Vec<String> = st["transcripts"][node].as_array().into_iter().flatten().filter_map(|f| f.as_str().map(String::from)).filter(|f| !f.contains('/') && !f.contains("..")).collect();
    let attempts: Vec<Value> = files.iter().map(|f| json!({ "file": f, "text": std::fs::read_to_string(run_dir(root, run).join("transcripts").join(f)).unwrap_or_default().chars().take(60_000).collect::<String>() })).collect();
    json!({ "ok": true, "node": node, "attempts": attempts, "output": st["state"]["values"][node], "cost_usd": st["state"]["node_cost"][node], "active": st["state"]["active"].get(node).is_some() })
}


// ── the door (the screen's and the Leader's) ──────────────────────────────────────────────────────────────────────────────

use crate::dispatch::AppState;
use axum::{body::Bytes, response::{IntoResponse, Response}, Json};

pub async fn http(state: Arc<AppState>, body: Bytes) -> Response { let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); Json(handle_op(&state, b).await).into_response() }
fn fail(e: impl Into<String>) -> Value { json!({ "ok": false, "error": e.into() }) }
/// The recent events of a run, from the team's saga (newest first).
pub fn events_of(root: &str, run: &str, limit: i64) -> Vec<Value> {
    let Ok(c) = crate::work::open(root) else { return vec![] };
    antiloki_rs::db_json::query_rows(&c, "SELECT id, saga_id, task_id, t_ms, actor, kind, text FROM events WHERE ext LIKE ?1 ORDER BY id DESC LIMIT ?2", &[json!(format!("tg:{run}:%")), json!(limit)]).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}
fn status_of(root: &str, run: &str) -> Option<Value> {
    let st = handle(run).map(|h| h.snap.lock().unwrap_or_else(|p| p.into_inner()).clone()).or_else(|| run_on_disk(root, run))?;
    let live = handle(run).is_some();
    let mut v = st; v["live"] = json!(live); v["events"] = json!(events_of(root, run, 200)); v["tools_live"] = live_of(&std::fs::read_to_string(run_dir(root, run).join("live.jsonl")).unwrap_or_default(), 6); Some(v)
}
async fn ctl_call<T>(h: &RunHandle, make: impl FnOnce(tokio::sync::oneshot::Sender<Result<T, String>>) -> Ctl) -> Result<T, String> {
    let (tx, rx) = tokio::sync::oneshot::channel(); h.ctl.send(make(tx)).map_err(|_| "the run has ended".to_string())?; rx.await.map_err(|_| "the run has ended".to_string())?
}
/// Is the serve running as the background service (so that closing a window changes nothing)? systemd sets INVOCATION_ID for a unit; `antiloki-main --service` also sets ANTILOKI_BACKGROUND.
fn background_service() -> bool { std::env::var("ANTILOKI_BACKGROUND").is_ok() || std::env::var("INVOCATION_ID").is_ok() }

/// ONE LOOK AT A TEAM'S SCHEDULES (team_triggers.rs): decide, start what is due, write down what was done and why not. `at_ms` is now, except in a test that names a moment.
pub async fn tick_triggers(root: &str, g: &Graph, at_ms: i64) -> Vec<crate::team_triggers::Decision> {
    use crate::team_triggers as tt;
    if g.triggers.is_empty() { return vec![]; }
    let off = tt::local_offset_min(at_ms); let mut st = tt::load_state(root, &g.team); let today = tt::date_of(at_ms, off);
    if st.day != today { st.day = today; st.day_runs.clear(); }
    if st.consent.is_some() && st.checked_ms == 0 { st.checked_ms = at_ms; tt::save_state(root, &g.team, &st); return vec![]; }   // the first look only starts the watch
    let live = live_runs(root).iter().any(|h| h.team == slug(&g.team));
    let usd: f64 = st.day_runs.iter().filter_map(|r| status_of(root, r)).map(|v| v["state"]["spent_usd"].as_f64().unwrap_or(0.0)).sum();
    let mut decisions = tt::decide(g, &st, at_ms, live, usd, off);
    // FILE WATCH and ON ERROR (RFC 0011 §16): the same consent, the same caps; a run one of them starts counts as live for the next
    let live2 = live || decisions.iter().any(|d| matches!(d, tt::Decision::Fire { .. }));
    if g.triggers.iter().any(|t| t.kind == "file_watch") && st.consent.is_some() {
        let (r2, gg) = (root.to_string(), g.clone());
        let snaps = blocking(move || gg.triggers.iter().enumerate().filter(|(_, t)| t.kind == "file_watch").map(|(i, t)| (tt::id_of(t, i), tt::snapshot(&r2, &t.watch))).collect::<BTreeMap<_, _>>()).await.unwrap_or_default();
        let (wd, wstate) = tt::decide_watch(g, &st, &snaps, at_ms, live2, usd); st.watch = wstate; decisions.extend(wd);
    }
    let live3 = live2 || decisions.iter().any(|d| matches!(d, tt::Decision::Fire { .. }));
    if g.triggers.iter().any(|t| t.kind == "on_error") && st.consent.is_some() {
        let (r2, team) = (root.to_string(), g.team.clone());
        let fails = blocking(move || tt::failures_since(&r2, &team, at_ms)).await.unwrap_or_default();
        let (ed, seen) = tt::decide_errors(g, &st, &fails, at_ms, live3, usd); st.errors_seen.extend(seen); let n = st.errors_seen.len(); if n > 500 { st.errors_seen.drain(0..n - 500); } decisions.extend(ed);
    }
    if g.triggers.iter().any(|t| t.kind == "webhook") {
        let (r2, team) = (root.to_string(), g.team.clone()); let hooks = blocking(move || tt::hooks_take(&r2, &team)).await.unwrap_or_default();
        if !hooks.is_empty() { let live4 = live3 || decisions.iter().any(|d| matches!(d, tt::Decision::Fire { .. })); decisions.extend(tt::decide_hooks(g, &st, &hooks, at_ms, live4, usd)); }
    }
    for d in &decisions { match d {
        tt::Decision::Fire { trigger, scheduled_ms, ask, budget_usd, engine } => {
            let mut g2 = g.clone(); if let Some(b) = budget_usd { g2.budget_usd = if g2.budget_usd > 0.0 { g2.budget_usd.min(*b) } else { *b }; }
            match start(root, g2, ask, engine).await {
                Ok(h) => { st.day_runs.push(h.id.clone()); st.last.insert(trigger.clone(), tt::Fired { at_ms, scheduled_ms: *scheduled_ms, run: h.id.clone() }); st.log.push(tt::LogRow { t_ms: at_ms, trigger: trigger.clone(), what: "fired".into(), why: match g.triggers.iter().enumerate().find(|(i, t)| tt::id_of(t, *i) == *trigger).map(|(_, t)| t.kind.as_str()) { Some("file_watch") => "the watched files changed and were quiet".to_string(), Some("on_error") => "another team failed".to_string(), _ => format!("scheduled for {}", tt::date_of(*scheduled_ms, off)) }, run: Some(h.id) }); }
                Err(e) => st.log.push(tt::LogRow { t_ms: at_ms, trigger: trigger.clone(), what: "skipped".into(), why: format!("could not start: {e}"), run: None }) } }
        tt::Decision::Skip { trigger, why, .. } => st.log.push(tt::LogRow { t_ms: at_ms, trigger: trigger.clone(), what: "skipped".into(), why: why.clone(), run: None }) } }
    st.checked_ms = at_ms; tt::save_state(root, &g.team, &st); decisions
}
/// THE WEBHOOK DOOR (BL-7): `POST /api/v1/hooks/team/<team>/<trigger>` with the trigger's token in `X-Antiloki-Token`. It checks the token and queues the call; nothing starts here (the scheduler's tick does, under the person's consent and the daily caps).
pub async fn webhook(state: Arc<AppState>, team: String, trigger: String, headers: axum::http::HeaderMap, body: axum::body::Bytes) -> axum::response::Response {
    use axum::{http::StatusCode, response::IntoResponse, Json};
    let reply = |c: StatusCode, v: Value| (c, Json(v)).into_response();
    if body.len() > crate::team_triggers::HOOK_BODY_MAX { return reply(StatusCode::PAYLOAD_TOO_LARGE, json!({ "ok": false, "error": "the body is over 64 KB" })); }
    let root = crate::chat_pane::workspace_root_of(&state); if root.is_empty() { return reply(StatusCode::SERVICE_UNAVAILABLE, json!({ "ok": false, "error": "no workspace is open" })); }
    // the same answer for "no such team", "no such trigger" and "wrong token": a stranger learns nothing about what exists
    let denied = || reply(StatusCode::UNAUTHORIZED, json!({ "ok": false, "error": "not accepted" }));
    let Ok(g) = load_graph(&root, &team) else { return denied() };
    let Some((i, t)) = g.triggers.iter().enumerate().find(|(i, t)| t.kind == "webhook" && crate::team_triggers::id_of(t, *i) == trigger) else { return denied() };
    let presented = headers.get("x-antiloki-token").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !crate::team_triggers::token_ok(t, presented) { return denied(); }
    let st = crate::team_triggers::load_state(&root, &g.team); if st.consent.as_deref() != Some(crate::team_triggers::consent_hash(&g).as_str()) { return reply(StatusCode::CONFLICT, json!({ "ok": false, "error": "the person has not approved this trigger yet" })); }
    let (r2, tm, id, payload) = (root.clone(), g.team.clone(), crate::team_triggers::id_of(t, i), String::from_utf8_lossy(&body).to_string());
    match blocking(move || crate::team_triggers::hooks_push(&r2, &tm, &id, now(), &payload)).await { Some(Ok(())) => reply(StatusCode::ACCEPTED, json!({ "ok": true, "queued": true })), Some(Err(e)) => reply(StatusCode::TOO_MANY_REQUESTS, json!({ "ok": false, "error": e })), None => reply(StatusCode::INTERNAL_SERVER_ERROR, json!({ "ok": false, "error": "could not queue the call" })) }
}
/// THE LIVE TOOL USE OF A RUNNING NODE (UI-31): each tool an agent starts, as the CLI streams it, is one line of `<run>/live.jsonl` (`{t_ms, node, tool, target}`); the file is cut to its last 200 lines. The run status reads the last ones per node. Best effort: a
/// failed write never touches the call.
/// EVERY TOOL CALL OF A STEP IS IN THE SAGA (the operator 2026-11-15: "sagas are our memory: every agent, every tool call"): the calls the step made (kept as they streamed, in the run's `live.jsonl`) become events of the team saga, oldest first, so the team's record
/// says what each agent actually did, not only that it finished. At most 80 a step. Blocking.
pub fn saga_tools(root: &str, run: &str, node: &str, saga: i64, since_ms: i64) {
    let Ok(text) = std::fs::read_to_string(run_dir(root, run).join("live.jsonl")) else { return }; let Ok(c) = crate::work::open(root) else { return }; let mut n = 0;
    for l in text.lines() { let Ok(v) = serde_json::from_str::<Value>(l) else { continue }; if s(&v["node"]) != node || v["t_ms"].as_i64().unwrap_or(0) < since_ms { continue; } if n >= 80 { break; }
        let (tool, target) = (s(&v["tool"]), s(&v["target"])); let said = if target.is_empty() { tool } else { format!("{tool} · {target}") }; if crate::work::event(&c, saga, None, node, "tool", &said).is_ok() { n += 1; } }
}
pub fn live_push(root: &str, run: &str, node: &str, tool: &str, target: &str) {
    use std::io::Write; let d = run_dir(root, run); if !d.exists() { return; } let p = d.join("live.jsonl");
    let line = json!({ "t_ms": now(), "node": node, "tool": tool.chars().take(40).collect::<String>(), "target": target.chars().take(120).collect::<String>() }).to_string();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) { let _ = writeln!(f, "{line}"); }
    if let Ok(t) = std::fs::read_to_string(&p) { let n = t.lines().count(); if n > 400 { let keep: Vec<&str> = t.lines().skip(n - 200).collect(); let _ = std::fs::write(&p, keep.join("\n") + "\n"); } }
}
/// The last `per_node` tool uses of every node of a run, newest first, by node. Pure over the file's text.
pub fn live_of(text: &str, per_node: usize) -> Value {
    let mut by: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for l in text.lines().rev() { let Ok(v) = serde_json::from_str::<Value>(l) else { continue }; let node = s(&v["node"]); let e = by.entry(node).or_default(); if e.len() < per_node { e.push(json!({ "t_ms": v["t_ms"], "tool": v["tool"], "target": v["target"] })); } }
    json!(by)
}
/// THE PERSON'S NOTES TO A RUNNING TEAM (AffectUI's Steering idea, our build): `<run>/notes.jsonl`, one line each. Only the person writes them (an agent that could would be a way to inject instructions); every agent call that starts
/// afterwards is told them, labelled as the person's, so a run can be steered without stopping it.
pub fn run_notes(root: &str, run: &str) -> Vec<String> { std::fs::read_to_string(run_dir(root, run).join("notes.jsonl")).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter_map(|v| v["text"].as_str().map(String::from)).collect() }
pub fn add_run_note(root: &str, run: &str, text: &str) -> Result<usize, String> {
    let t: String = text.trim().chars().take(600).collect(); if t.is_empty() { return Err("a note needs text".into()); }
    let n = run_notes(root, run); if n.len() >= 20 { return Err("a run takes at most 20 notes".into()); }
    let d = run_dir(root, run); if !d.exists() { return Err("that run does not exist".into()); } use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(d.join("notes.jsonl")).map_err(|e| e.to_string())?; writeln!(f, "{}", json!({ "t_ms": now(), "text": t })).map_err(|e| e.to_string())?; Ok(n.len() + 1)
}
/// How the notes read to an agent. Pure.
pub fn notes_block(notes: &[String]) -> String { format!("THE PERSON ADDED THESE NOTES WHILE THE TEAM WAS RUNNING (they are the person's own words; follow them where they apply to your work):\n{}", notes.iter().map(|n| format!("- {n}")).collect::<Vec<_>>().join("\n")) }
/// The scheduler: every few seconds, every team of the open space that has schedules. Off with the driver tick switch (a probe, a test) unless `ANTILOKI_TEAM_TRIGGERS=1`.
pub async fn scheduler(state: Arc<AppState>) {
    if std::env::var("ANTILOKI_BE_TICK_DISABLED").map_or(false, |v| v == "true") && std::env::var("ANTILOKI_TEAM_TRIGGERS").map_or(true, |v| v != "1") { return; }
    let every = std::env::var("ANTILOKI_TRIGGER_TICK_S").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(20).max(1);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(every)).await;
        if crate::tick::halt_file().exists() { continue; }   // the operator's one switch for everything that runs by itself: no schedule fires while it is there
        let root = crate::chat_pane::workspace_root_of(&state); if root.is_empty() { continue; }
        let r2 = root.clone(); let gs = blocking(move || list_graphs(&r2)).await.unwrap_or_default();
        for g in gs.iter().filter(|g| !g.triggers.is_empty()) { let _ = tick_triggers(&root, g, now()).await; }
    }
}
/// THE LEADER WORKS IN THE TEAM THE PERSON OPENED (the operator's first real team: asked to make a video team inside the team they had just made, the Leader made ANOTHER team, then the right one: two teams, and the person's own left empty).
/// An agent that is about to make a NEW team while the person's open thread is a team of another name is told to put the design in that team; `new_team: true` says it really wants a second one. Pure over the files.
pub fn second_team_refusal(root: &str, name: &str, by_agent: bool, new_team: bool) -> Option<String> {
    if !by_agent || new_team { return None; }
    let active = crate::cowork_threads::active(root);
    if active.is_empty() || active == slug(name) || !graph_path(root, &active).exists() { return None; }
    Some(format!("you are working inside the team `{active}`, and `{name}` is not that team. Put the design there (a `graph` whose `team` is `{active}`), or the person ends up with two teams and their own one empty. If you really mean a second, separate team, say so with new_team: true."))
}

pub async fn handle_op(state: &Arc<AppState>, b: Value) -> Value {
    let root = crate::chat_pane::workspace_root_of(state); if root.is_empty() { return fail("no project is open"); }
    let op = s(&b["op"]); let by_agent = b["pane_id"].as_i64().unwrap_or(0) > 0;
    let team_of = |b: &Value| -> Result<Graph, String> { if b["graph"].is_object() { Graph::from_json(&b["graph"]) } else { load_graph(&root, &s(&b["team"])) } };
    match op.as_str() {
        "rubric" => json!({ "ok": true, "text": crate::team_rubric::text() }),
        "catalog" => { let mut c = catalog(); c["ok"] = json!(true); c }
        "node_list" => json!({ "ok": true, "text": node_list_md(&s(&b["category"])) }),
        // SCHEDULES (RFC 0010 §4.9): the status is for anyone; setting them, consenting and firing by hand are the PERSON's
        // BL-8: what the teams did since `since_ms` (default: the last 24 hours), from the run files and the trigger logs; read-only
        "away_digest" => { let n = now(); let since = b["since_ms"].as_i64().unwrap_or(n - 24 * 3_600_000); let r2 = root.clone(); let only_team = s(&b["team"]);
            let (runs, skips) = blocking(move || {
                let only = slug(&only_team); let mut runs = vec![]; for e in std::fs::read_dir(Path::new(&r2).join(".antiloki").join("team")).into_iter().flatten().flatten() { if let Some(l) = std::fs::read_to_string(e.path().join("state.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).as_ref().and_then(crate::team_digest::line_of) { if only_team.is_empty() || slug(&l.team) == only { runs.push(l); } } }
                let mut skips = vec![]; for g in list_graphs(&r2).into_iter().filter(|g| only_team.is_empty() || slug(&g.team) == only) { for l in crate::team_triggers::load_state(&r2, &g.team).log { if l.what == "skipped" { skips.push(crate::team_digest::SkipLine { team: g.team.clone(), trigger: l.trigger, t_ms: l.t_ms, why: l.why }); } } }
                (runs, skips) }).await.unwrap_or_default();
            crate::team_digest::digest(&runs, &skips, since, n) }
        // B2: the proof card for a team: the pre-registered verdict over the pairs a bench run recorded (`.antiloki/team/_proof/<team>.json`); none recorded = a card that says so
        "proof" => { let g = match load_graph(&root, &s(&b["team"])) { Ok(g) => g, Err(e) => return fail(e) };
            let text = std::fs::read_to_string(crate::team_proof::proof_path(&root, &g.team)).ok();
            match text { None => json!({ "ok": true, "recorded": false, "verdict": "none", "words": "No proof yet: nobody has run this team against one agent. The bench writes the pairs here; see Step C.", "n": 0 }),
                Some(t) => match serde_json::from_str::<crate::team_proof::Proof>(&t) { Ok(p) => { let mut v = crate::team_proof::card(&p, &crate::team_proof::graph_hash(&g), &p.fixtures_hash); v["ok"] = json!(true); v["recorded"] = json!(true); v }, Err(e) => fail(format!("the proof file cannot be read: {e}")) } } }
        "trigger_status" => match team_of(&b) { Ok(g) => crate::team_triggers::status_json(&root, &g, now(), background_service()), Err(e) => fail(e) },
        "triggers_set" => { if by_agent { return fail("schedules are set by the person: an agent can propose one, not set it"); }
            let mut g = match load_graph(&root, &s(&b["team"])) { Ok(g) => g, Err(e) => return fail(e) };
            match serde_json::from_value::<Vec<crate::team_graph::Trigger>>(b["triggers"].clone()) { Ok(t) => g.triggers = t, Err(e) => return fail(format!("triggers: {e}")) }
            g.limits = if b["limits"].is_object() { serde_json::from_value(b["limits"].clone()).ok() } else { None };
            let rep = crate::team_skills::report_for(&root, &g, false); if !rep.ok() { return json!({ "ok": false, "error": format!("those schedules are not valid: {}", rep.words().join("; ")), "report": rep }); }
            if let Err(e) = save_graph(&root, &g) { return fail(e); }
            let mut st = crate::team_triggers::load_state(&root, &g.team); st.consent = None; st.consent_ms = 0; crate::team_triggers::save_state(&root, &g.team, &st);   // changing a schedule takes the consent back
            crate::team_triggers::status_json(&root, &g, now(), background_service()) }
        // BL-7: a new token for a webhook trigger. It is shown ONCE (only its hash is kept in the graph); making one takes the person's consent back, like any change to a trigger.
        "webhook_token" => { if by_agent { return fail("only the person makes a webhook's token"); }
            let mut g = match load_graph(&root, &s(&b["team"])) { Ok(g) => g, Err(e) => return fail(e) }; let want = s(&b["trigger"]);
            let Some(i) = g.triggers.iter().enumerate().position(|(i, t)| t.kind == "webhook" && crate::team_triggers::id_of(t, i) == want) else { return fail(format!("this team has no webhook trigger called {want}")) };
            let (token, hash) = crate::team_triggers::new_token(); g.triggers[i].token_hash = hash;
            if let Err(e) = save_graph(&root, &g) { return fail(e); }
            let mut st = crate::team_triggers::load_state(&root, &g.team); st.consent = None; st.consent_ms = 0; crate::team_triggers::save_state(&root, &g.team, &st);
            json!({ "ok": true, "token": token, "path": format!("/api/v1/hooks/team/{}/{}", slug(&g.team), want), "note": "shown once: keep it somewhere safe. Approve the trigger again before it can start a run." }) }
        "trigger_consent" => { if by_agent { return fail("only the person says a team may run on its own"); }
            let g = match load_graph(&root, &s(&b["team"])) { Ok(g) => g, Err(e) => return fail(e) }; let mut st = crate::team_triggers::load_state(&root, &g.team);
            if b["on"].as_bool().unwrap_or(false) { if g.triggers.is_empty() { return fail("this team has no schedule to consent to"); } let rep = crate::team_skills::report_for(&root, &g, true); if !rep.ok() { return fail(format!("it cannot run unattended yet: {}", rep.words().join("; "))); }
                st.consent = Some(crate::team_triggers::consent_hash(&g)); st.consent_ms = now(); st.checked_ms = now(); } else { st.consent = None; st.consent_ms = 0; }
            crate::team_triggers::save_state(&root, &g.team, &st); crate::team_triggers::status_json(&root, &g, now(), background_service()) }
        "trigger_fire" => { if by_agent { return fail("only the person starts a scheduled run by hand"); }
            let g = match load_graph(&root, &s(&b["team"])) { Ok(g) => g, Err(e) => return fail(e) }; let want = s(&b["trigger"]);
            let Some((i, t)) = g.triggers.iter().enumerate().find(|(i, t)| crate::team_triggers::id_of(t, *i) == want || want.is_empty()) else { return fail("no such schedule") };
            let at = now(); let ask = crate::team_triggers::fill_ask(&t.ask, at, crate::team_triggers::local_offset_min(at)); let mut g2 = g.clone(); if let Some(bu) = t.budget_usd { g2.budget_usd = if g2.budget_usd > 0.0 { g2.budget_usd.min(bu) } else { bu }; }
            match start(&root, g2, &ask, &t.engine).await { Ok(h) => json!({ "ok": true, "run": h.id, "thread": slug(&h.team) }), Err(e) => fail(e) } }
        // a test's way to look at the schedules at a moment it names (only when the serve is started for it)
        "trigger_tick" => { if by_agent || std::env::var("ANTILOKI_TEAM_TRIGGERS_TEST").map_or(true, |v| v != "1") { return fail("not available"); }
            let g = match load_graph(&root, &s(&b["team"])) { Ok(g) => g, Err(e) => return fail(e) }; let d = tick_triggers(&root, &g, b["now_ms"].as_i64().unwrap_or_else(now)).await;
            json!({ "ok": true, "decisions": d.iter().map(|x| match x { crate::team_triggers::Decision::Fire { trigger, ask, .. } => json!({ "do": "fire", "trigger": trigger, "ask": ask }), crate::team_triggers::Decision::Skip { trigger, why, .. } => json!({ "do": "skip", "trigger": trigger, "why": why }) }).collect::<Vec<_>>() }) }
        // THE TEAM'S OWN SKILLS (§4.16): the Leader writes them (always as drafts); only the person approves
        "skill_write" => { let effects: Vec<String> = b["effects"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            match crate::team_skills::write(&root, &s(&b["team"]), &s(&b["name"]), &s(&b["purpose"]), &effects, &s(&b["body"])) { Ok(sk) => json!({ "ok": true, "skill": sk, "note": if sk.status == "draft" { "saved as a draft: the person reads and approves it in the team pane before any run uses it" } else { "unchanged, still approved" } }), Err(e) => fail(e) } }
        "audit" => { let (r, run) = (root.clone(), s(&b["run"])); match blocking(move || run_on_disk(&r, &run)).await.flatten() { Some(v) if v["audit"].is_object() => json!({ "ok": true, "audit": v["audit"] }), Some(_) => fail("this run has not been audited yet (it is audited when it ends)"), None => fail("no such run") } }
        "node_bench" => { let (r, team) = (root.clone(), s(&b["team"])); let rows = blocking(move || read_jsonl(&team_files(&r, &team).join("node-records.jsonl")).into_iter().filter_map(|v| serde_json::from_value::<crate::team_nodebench::NodeRecord>(v).ok()).collect::<Vec<_>>()).await.unwrap_or_default(); let mut t = crate::team_nodebench::table(&rows); t["ok"] = json!(true); t }
        "audits" => { let (r, team) = (root.clone(), s(&b["team"])); let a = blocking(move || audits_of(&r, &team)).await.unwrap_or_default(); let lim = b["limit"].as_u64().unwrap_or(10) as usize; let rows: Vec<Value> = a.iter().rev().take(lim).map(|x| json!({ "run": x.run, "graph": x.graph_hash, "score": x.score, "good": x.good, "status": x.status, "t_ms": x.t_ms, "attention": x.attention, "findings": x.findings.iter().map(|f| format!("{} {}", f.severity, f.code)).collect::<Vec<_>>() })).collect(); json!({ "ok": true, "audits": rows, "reading": "newest first; a change of `graph` between rows is a change of the team: compare the scores before and after it" }) }
        "autofix" => { let (r, team) = (root.clone(), s(&b["team"])); let on = blocking(move || autofix_on(&r, &team)).await.unwrap_or(true); json!({ "ok": true, "on": on, "tries": AUTOFIX_TRIES }) }
        "autofix_set" => { if by_agent { return fail("only the person turns autofix on or off"); } let (r, team, on) = (root.clone(), s(&b["team"]), b["on"].as_bool().unwrap_or(true)); blocking(move || set_autofix(&r, &team, on)).await; json!({ "ok": true, "on": on }) }
        "leader_bind" => { let (r, team, pane) = (root.clone(), s(&b["team"]), b["pane"].as_i64()); let Some(pane) = pane else { return fail("`pane` is the Leader terminal's pane number") }; let (r2, t2) = (r.clone(), team.clone());
            // A TERMINAL THAT STANDS IN ANOTHER THREAD IS NOT THIS TEAM'S LEADER (2026-11-18: switching threads bound the team to the agent of the thread that was leaving, and the team's memory was typed into it, 44 times in 5 hours)
            let (r3, t3) = (r.clone(), team.clone()); if let Some(other) = blocking(move || other_thread_of(&crate::cowork_threads::view(&r3), &t3, pane)).await.flatten() { return fail(format!("terminal {pane} is in the thread {other}, not in this team's: it is not bound as its Leader")); }
            // A LEADER THAT STARTS IS HANDED THE TEAM'S MEMORY: a terminal that is new (a different pane than the one on file) knows nothing of this team; its saga is put in its inbox, and goes into the terminal with whatever else waits there
            let _ = blocking(move || { let d = team_files(&r2, &t2); let _ = std::fs::create_dir_all(&d); let prev = std::fs::read_to_string(d.join("leader.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v["pane"].as_i64());
                if prev != Some(pane) { let memo = crate::team_recall::recall(&r2, &t2); append_jsonl(&d.join("leader-inbox.jsonl"), &json!({ "id": format!("m-{}", now()), "t_ms": now(), "status": "unread", "kind": "memory", "message": memo })); }
                let _ = std::fs::write(d.join("leader.json"), json!({ "pane": pane, "t_ms": now() }).to_string()); }).await; let n = deliver_pending(&r, &team).await; json!({ "ok": true, "bound": pane, "delivered": n }) }
        // THE TEAM'S MEMORY, ON REQUEST: the Leader (or the person) reads it whenever; it is built from the saga, free, no model
        "versions" => { let (r, team) = (root.clone(), s(&b["team"])); let v = blocking(move || crate::team_repo::history(&r, &team)).await.unwrap_or_default(); json!({ "ok": true, "versions": v }) }
        "recipe" => { let (r, team, id) = (root.clone(), s(&b["team"]), s(&b["id"])); match blocking(move || { let g = if id.is_empty() { load_graph(&r, &team)? } else { crate::team_repo::recipe_at(&r, &team, &id)? }; crate::team_recipe::to_recipe(&g) }).await { Some(Ok(t)) => json!({ "ok": true, "recipe": t }), Some(Err(e)) => fail(e), None => fail("the recipe was cut off") } }
        "restore" => { if by_agent { return fail("only the person goes back to an older version"); } let (r, team, id) = (root.clone(), s(&b["team"]), s(&b["id"])); match blocking(move || { let g = crate::team_repo::recipe_at(&r, &team, &id)?; save_graph(&r, &g)?; Ok::<_, String>(g.team) }).await { Some(Ok(t)) => json!({ "ok": true, "team": t }), Some(Err(e)) => fail(e), None => fail("the restore was cut off") } }
        // THE TEAM'S REPLAY BANK, EXPERIMENTS, REQUESTS AND TRAVEL (RFC 0013 steps 4-9). Only the person adds tasks, decides requests and imports; an agent may read and propose.
        "replay_add" => { if by_agent { return fail("only the person saves a task"); } let (r, team, ask, run) = (root.clone(), s(&b["team"]), s(&b["ask"]), s(&b["run"])); let checks: Vec<crate::team_check::Rule> = match serde_json::from_value(b["checks"].clone()) { Ok(c) => c, Err(e) => return fail(format!("the checks are not rules: {e}")) };
            match blocking(move || crate::team_replay::add(&r, &team, &ask, checks, &run, now() as i64)).await { Some(Ok(t)) => json!({ "ok": true, "task": t }), Some(Err(e)) => fail(e), None => fail("cut off") } }
        "thread_list" => { let (r, team) = (root.clone(), s(&b["team"])); let t = blocking(move || crate::team_thread::list(&r, &team)).await.unwrap_or_default(); json!({ "ok": true, "threads": t }) }
        "thread_fork" => { if by_agent { return fail("only the person forks a team into a thread"); }
            let (r, team, name, engine) = (root.clone(), s(&b["team"]), s(&b["name"]), s(&b["engine"])); match blocking(move || crate::team_thread::fork(&r, &team, &name, Some(engine.as_str()).filter(|e| !e.is_empty()))).await { Some(Ok(t)) => json!({ "ok": true, "thread": t }), Some(Err(e)) => fail(e), None => fail("cut off") } }
        "thread_open" => { if by_agent { return fail("only the person opens a thread to run"); }
            let (r, team, name) = (root.clone(), s(&b["team"]), s(&b["name"])); match blocking(move || crate::team_thread::materialize(&r, &team, &name)).await { Some(Ok(t)) => json!({ "ok": true, "team": t }), Some(Err(e)) => fail(e), None => fail("cut off") } }
        "run_checkpoints" => { let (r, run) = (root.clone(), s(&b["run"])); let c = blocking(move || checkpoints(&r, &run)).await.unwrap_or_default(); json!({ "ok": true, "checkpoints": c }) }
        "run_rewind" => { if by_agent { return fail("only the person rewinds a run"); } if b["confirmed"] != json!(true) { return fail("a rewind starts a new run that spends from the budget: confirm it"); }
            let (r, run, k) = (root.clone(), s(&b["run"]), b["k"].as_u64().unwrap_or(0) as usize); match blocking(move || rewind(&r, &run, k)).await { Some(Ok(id)) => json!({ "ok": true, "run": id }), Some(Err(e)) => fail(e), None => fail("cut off") } }
        "conflicts" => { let r = root.clone(); let base = { let b = s(&b["base"]); if b.is_empty() { "main".to_string() } else { b } }; match blocking(move || crate::team_conflict::scan(&r, &base).map(|u| (u.iter().map(|x| x.name.clone()).collect::<Vec<_>>(), crate::team_conflict::overlaps(&u)))).await { Some(Ok((trees, c))) => json!({ "ok": true, "trees": trees, "conflicts": c }), Some(Err(e)) => fail(e), None => fail("cut off") } }
        "root_cause" => { let (r, run) = (root.clone(), s(&b["run"])); match blocking(move || status_of(&r, &run).map(|v| { let ev: Vec<Value> = v["events"].as_array().cloned().unwrap_or_default(); let g = v["graph"].clone(); crate::team_rootcause::investigate(&s(&v["id"]), &ev, &|n| g["nodes"][n]["label"].as_str().unwrap_or(n).to_string()) })).await { Some(Some(f)) => json!({ "ok": true, "root_cause": f }), Some(None) => fail("no run with that id"), None => fail("cut off") } }
        "package_approve" => { if by_agent { return fail("only the person approves a package"); } let spec: Result<crate::team_package::Spec, _> = serde_json::from_value(b["spec"].clone()); match spec { Ok(sp) => match crate::team_package::plan(&sp, &root, "r", "n") { Ok(p) => { let f = team_files(&root, &s(&b["team"])).join("package-approvals.json"); let mut l: Vec<Value> = std::fs::read_to_string(&f).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default(); if !l.iter().any(|k| k.as_str() == Some(p.approval_key.as_str())) { l.push(json!(p.approval_key)); } if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); } let _ = std::fs::write(&f, serde_json::to_string_pretty(&l).unwrap_or_default()); { let (r2, t2, key) = (root.clone(), s(&b["team"]), p.approval_key.clone()); std::thread::spawn(move || { let _ = crate::team_repo::commit_settings(&r2, &t2, &format!("settings: approved package {key}\n\nby: person")); }); } json!({ "ok": true, "approved": p.approval_key, "notes": p.notes }) }, Err(e) => json!({ "ok": false, "error": e.join("; ") }) }, Err(e) => fail(format!("the package request is malformed: {e}")) } }
        "branch_use" => { // VID-37: expand a branch from the library into a team, as a LINK (tracked against its file) or a CLONE (the team's own); the file is vendored into the team's repo and the use is recorded in `branches.lock.json`
            let (team, name, mode, prefix) = (s(&b["team"]), s(&b["name"]), s(&b["mode"]), s(&b["prefix"]));
            if mode != "link" && mode != "clone" { return fail("`mode` is link or clone"); }
            if let Err(e) = crate::team_branch::library_path(&root, &name) { return fail(e); }
            let text = match crate::team_branch::library_text(&root, &name) { Some(t) => t, None => return fail(format!("no branch called {name} in the library")) };
            let br = match crate::team_branch::from_text(&text) { Ok(v) => v, Err(e) => return fail(e) };
            let bind = b["bind"].as_object().cloned().unwrap_or_default();
            // THE EXAMPLES RUN AT USE TOO: a branch whose own examples fail is not brought into a team (an older file with none is allowed, with a note)
            let ex = crate::team_branch::run_examples(&br); let failing: Vec<String> = ex.iter().filter(|x| !x.ok).map(|x| format!("\"{}\": {}", x.name, x.detail)).collect();
            if !failing.is_empty() { return fail(format!("the branch's own examples fail, so it was not used: {}", failing.join("; "))); }
            let (params, seats) = (b["params"].as_object().cloned().unwrap_or_default(), b["seats"].as_object().cloned().unwrap_or_default());
            let resolved = match crate::team_branch::resolve(&br, &params, &seats) { Ok(x) => x, Err(e) => return fail(e) };
            let inst = match crate::team_branch::instantiate(&br, &prefix, &bind, &params, &seats) { Ok(i) => i, Err(e) => return fail(e) };
            let mut gj = match load_graph(&root, &team) { Ok(g) => g.to_json(), Err(e) => return fail(e) };
            let clash: Vec<&String> = inst.nodes.keys().filter(|k| gj["nodes"].get(k.as_str()).is_some()).collect(); if !clash.is_empty() { return fail(format!("these node ids already exist in the team: {}; give a `prefix`", clash.iter().map(|x| x.as_str()).collect::<Vec<_>>().join(", "))); }
            let same = |e: &Value, r: &Value| e["from"] == r["from"] && e["port"] == r["port"] && e["to"] == r["to"];
            let drop: Vec<Value> = b["disconnect"].as_array().cloned().unwrap_or_default();
            let mut edges: Vec<Value> = gj["edges"].as_array().cloned().unwrap_or_default().into_iter().filter(|e| !drop.iter().any(|r| same(e, r))).collect();
            edges.extend(inst.edges.iter().cloned()); edges.extend(b["connect"].as_array().cloned().unwrap_or_default());
            for (k, v) in &inst.nodes { gj["nodes"][k.as_str()] = v.clone(); } gj["edges"] = json!(edges);
            let g2 = match Graph::from_json(&gj) { Ok(g) => g, Err(e) => return fail(e) };
            let rep = validate(&g2); if !rep.errors.is_empty() { return fail(format!("using the branch would make an invalid team: {}", rep.errors.iter().map(|i| i.message.clone()).collect::<Vec<_>>().join("; "))); }
            let hash = s(&br["hash"]); let short: String = hash.trim_start_matches("sha256:").chars().take(8).collect();
            let needs_approval: Vec<String> = inst.nodes.iter().filter(|(_, v)| v["tool"]["type"] == json!("package")).map(|(k, _)| k.clone()).collect();
            let reason = if s(&b["reason"]).is_empty() { format!("use branch {name} {} ({short}) as a {mode}, prefix \"{prefix}\"", s(&br["version"])) } else { s(&b["reason"]) };
            let change = json!({ "reason": reason, "by": if by_agent { "leader" } else { "person" }, "cycle": 0 });
            if let Err(e) = save_graph_as(&root, &g2, Some(&change)) { return fail(e); }
            let tf = team_files(&root, &team); let _ = std::fs::create_dir_all(tf.join("branches")); let _ = std::fs::write(tf.join("branches").join(format!("{name}@{short}.yaml")), &text);
            let lockp = tf.join("branches.lock.json"); let mut lock: Vec<Value> = std::fs::read_to_string(&lockp).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            lock.push(json!({ "instance": if prefix.is_empty() { name.clone() } else { prefix.clone() }, "branch": name, "version": br["version"], "hash": hash, "mode": mode, "prefix": prefix, "bind": bind, "params": params, "seats": seats, "nodes": inst.nodes.keys().collect::<Vec<_>>(), "used_at_ms": now() }));
            let _ = std::fs::write(&lockp, serde_json::to_string_pretty(&lock).unwrap_or_default());
            json!({ "ok": true, "mode": mode, "hash": hash, "nodes": inst.nodes.keys().collect::<Vec<_>>(), "start": inst.start, "inputs": br["inputs"], "outputs": br["outputs"], "requires": br["requires"], "needs_approval": needs_approval, "examples": ex.len(), "params": resolved.0, "seats": resolved.1, "warnings": rep.warnings.iter().map(|i| i.message.clone()).chain(if ex.is_empty() { Some("this branch has no examples (an older file): nothing proves it still works".to_string()) } else { None }).collect::<Vec<_>>() }) }
        "branch_status" => { // is each LINK still what its file says? a changed node makes it drifted (treated as a clone)
            let team = s(&b["team"]); let tf = team_files(&root, &team); let lock: Vec<Value> = std::fs::read_to_string(tf.join("branches.lock.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            let nodes = match load_graph(&root, &team) { Ok(g) => g.to_json()["nodes"].clone(), Err(e) => return fail(e) }; let mut rows = vec![];
            for l in &lock { let short: String = s(&l["hash"]).trim_start_matches("sha256:").chars().take(8).collect();
                let state = if l["mode"] == json!("clone") { json!({ "state": "clone (the team's own)" }) } else { match std::fs::read_to_string(tf.join("branches").join(format!("{}@{short}.yaml", s(&l["branch"])))).ok().and_then(|t| crate::team_branch::from_text(&t).ok()) {
                    Some(br) => { let d = crate::team_branch::drift(&br, &s(&l["prefix"]), &l["bind"].as_object().cloned().unwrap_or_default(), &l["params"].as_object().cloned().unwrap_or_default(), &l["seats"].as_object().cloned().unwrap_or_default(), &nodes); if d.is_empty() { json!({ "state": "linked, unchanged" }) } else { json!({ "state": "drifted: now a clone", "changed": d }) } }
                    None => json!({ "state": "the vendored file is missing or torn" }) } };
                rows.push(json!({ "instance": l["instance"], "branch": l["branch"], "version": l["version"], "hash": l["hash"], "mode": l["mode"], "status": state })); }
            json!({ "ok": true, "instances": rows }) }
        "branch_export" => { // a set of a team's nodes as a standalone branch file (YAML, strict subset), written to the library `.antiloki/branches/`
            let team = s(&b["team"]); let ids: Vec<String> = b["nodes"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            match load_graph(&root, &team).and_then(|g| crate::team_branch::export(&g, &ids, &b["meta"])) {
                Ok(v) => { let text = crate::team_branch::to_text(&v); let dir = Path::new(&root).join(".antiloki").join("branches"); let _ = std::fs::create_dir_all(&dir); let path = dir.join(format!("{}.yaml", s(&v["name"])));
                    match std::fs::write(&path, &text) { Ok(_) => json!({ "ok": true, "path": path.to_string_lossy(), "hash": crate::team_branch::hash_of(&v), "inputs": v["inputs"], "outputs": v["outputs"], "examples": crate::team_branch::run_examples(&v), "yaml": text }), Err(e) => fail(e.to_string()) } }
                Err(e) => fail(e) } }
        "branch_load" => { // read a branch file (given as text or by name from the library), refusing a bad hash or a bad content
            let text = if !s(&b["yaml"]).is_empty() { s(&b["yaml"]) } else { match crate::team_branch::library_path(&root, &s(&b["name"])) { Ok(_) => crate::team_branch::library_text(&root, &s(&b["name"])).unwrap_or_default(), Err(e) => return fail(e) } };
            match crate::team_branch::from_text(&text) { Ok(v) => json!({ "ok": true, "name": v["name"], "version": v["version"], "hash": v["hash"], "nodes": v["nodes"].as_object().map_or(0, |m| m.len()), "inputs": v["inputs"], "outputs": v["outputs"], "params": v["params"], "requires": v["requires"] }), Err(e) => fail(e) } }
        "branch_test" => { // run a branch's examples (from the library by name, or given as text)
            let text = if !s(&b["yaml"]).is_empty() { s(&b["yaml"]) } else { match crate::team_branch::library_path(&root, &s(&b["name"])) { Ok(_) => crate::team_branch::library_text(&root, &s(&b["name"])).unwrap_or_default(), Err(e) => return fail(e) } };
            match crate::team_branch::from_text(&text) { Ok(v) => { let r = crate::team_branch::run_examples(&v); json!({ "ok": true, "name": v["name"], "passed": r.iter().filter(|x| x.ok).count(), "total": r.len(), "results": r }) } Err(e) => fail(e) } }
        // TEMPLATES (RFC 0017): whole teams as YAML. List both sources; start a team from one; turn one of the person's teams into one (checked first).
        "template_list" => { let r = root.clone(); let lib = blocking(move || crate::team_template::read_library(&r)).await.unwrap_or_default(); json!({ "ok": true, "templates": crate::team_template::list(crate::team_template::SHIPPED, &lib) }) }
        "template_use" => {
            if by_agent { return fail("only the person starts a team from a template"); }
            let (name, tpl) = (s(&b["team"]), s(&b["template"])); if name.trim().is_empty() { return fail("a team needs a name"); }
            let r = root.clone(); let lib = blocking(move || crate::team_template::read_library(&r)).await.unwrap_or_default();
            let Some(text) = crate::team_template::find(crate::team_template::SHIPPED, &lib, &tpl) else { return fail(format!("no template called {tpl}")) };
            let v = match crate::team_branch::from_text(&text) { Ok(v) => v, Err(e) => return fail(format!("the template does not read: {e}")) };
            if v["template"]["kind"] != json!("team") { return fail("that file is a branch, not a team template"); }
            if let Some(m) = second_team_refusal(&root, &name, false, b["new_team"].as_bool().unwrap_or(false)) { return fail(m); }
            if graph_path(&root, &name).exists() { return fail(format!("a team called {name} exists")); }
            let budget = b["budget_usd"].as_f64().filter(|x| *x > 0.0).unwrap_or_else(|| v["settings"]["budget_usd"].as_f64().unwrap_or(6.0));
            let mut gj = json!({ "team": name, "schema": 1, "budget_usd": budget, "start": v["start"], "nodes": v["nodes"], "edges": v["edges"] });
            let mut g = match Graph::from_json(&gj) { Ok(g) => g, Err(e) => return fail(format!("the template is not a team: {e}")) };
            let rep = validate(&g); if !rep.errors.is_empty() { return fail(format!("the template would make an invalid team: {}", rep.errors.iter().map(|i| i.message.clone()).collect::<Vec<_>>().join("; "))); }
            let rc = crate::cowork_threads::apply(&root, &json!({ "op": "create_team", "name": name })); let tv = match rc { Ok(x) => x, Err(e) => return fail(e) };
            let (r2, t2) = (root.clone(), slug(&name)); if let Some(Err(e)) = blocking(move || crate::thread_tree::ensure(&r2, &t2)).await { let _ = crate::cowork_threads::apply(&root, &json!({ "op": "delete", "thread": slug(&name) })); return fail(format!("the team's tree could not be made: {e}")); }
            g.branch = crate::thread_tree::slug(&root, &slug(&name)); gj["budget_usd"] = json!(budget);
            let short: String = s(&v["hash"]).trim_start_matches("sha256:").chars().take(8).collect();
            let change = json!({ "reason": format!("started from the template {} {} ({short})", s(&v["name"]), s(&v["version"])), "by": "person", "cycle": 0 });
            if let Err(e) = save_graph_as(&root, &g, Some(&change)) { return fail(e); }
            json!({ "ok": true, "team": name, "graph": g.to_json(), "report": crate::team_skills::report_for(&root, &g, false), "thread": slug(&name), "threads": tv, "hint": v["template"]["ask"], "brief": Value::Null })
        }
        "template_share" => {
            if by_agent { return fail("only the person shares a team as a template"); }
            let (team, name, desc, cat) = (s(&b["team"]), s(&b["name"]), s(&b["description"]), s(&b["category"]));
            if desc.trim().is_empty() { return fail("say what the team is for (the description)"); }
            if crate::team_template::SHIPPED.iter().any(|(n, _)| *n == name) { return fail(format!("`{name}` is the name of a template that ships with antiloki: choose another")); }
            let path = match crate::team_template::library_path(&root, &name) { Ok(p) => p, Err(e) => return fail(e) };
            let g = match load_graph(&root, &team) { Ok(g) => g, Err(e) => return fail(e) };
            let report = validate(&g); if !report.errors.is_empty() { return fail(format!("the team does not validate yet: {}", report.errors.iter().map(|i| format!("{}{}", i.node.clone().map(|n| format!("{n}: ")).unwrap_or_default(), i.message)).collect::<Vec<_>>().join("; "))); }
            let gj = g.to_json();
            let secrets = crate::team_template::secrets_in(&gj); if !secrets.is_empty() { return fail(format!("not shared: it holds what looks like a secret ({}). Take it out of the team first.", secrets.join("; "))); }
            let (clean, reset) = crate::team_template::strip_powers(&gj);
            let paths = crate::team_template::private_paths_in(&clean); if !paths.is_empty() { return fail(format!("not shared: it names places on this machine ({}). Use a path inside the project.", paths.join("; "))); }
            let proven = runs_of(&root, &team).into_iter().find(|r| r["status"]["status"] == json!("done")).and_then(|r| r["id"].as_str().map(String::from));
            let needs = crate::team_template::needs_of(&gj);
            let v = crate::team_template::build(&gj, &name, desc.trim(), &cat, proven.as_deref(), &team, &needs, &s(&b["ask"]), &s(&b["cost_note"]));
            let e = crate::team_branch::check(&v); if !e.is_empty() { return fail(format!("the template does not check out: {}", e.join("; "))); }
            let text = crate::team_branch::to_text(&v);
            if let Err(e) = crate::team_branch::from_text(&text) { return fail(format!("the file does not read back: {e}")); }
            let (r2, n2, t2) = (root.clone(), name.clone(), text.clone()); let wrote = blocking(move || crate::docstore::put_text(&r2, "template", &n2, &t2)).await;
            match wrote { Some(Ok(())) => json!({ "ok": true, "name": name, "status": v["template"]["status"], "reset": reset, "scanned": ["secrets", "private paths"], "path": format!("antiloki.db: template {name}"), "proven_by_run": proven }), Some(Err(e)) => fail(e), None => fail("cut off") }
        }
        "branch_list" => { let dir = Path::new(&root).join(".antiloki").join("branches"); let mut rows = vec![];
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() { let p = e.path(); if p.extension().map_or(false, |x| x == "yaml") { let t = std::fs::read_to_string(&p).unwrap_or_default();
                match crate::team_branch::from_text(&t) { Ok(v) => { let r = crate::team_branch::run_examples(&v); rows.push(json!({ "name": v["name"], "version": v["version"], "hash": v["hash"], "description": v["description"], "valid": true, "examples": r.len(), "examples_pass": r.iter().all(|x| x.ok) })) } Err(e) => rows.push(json!({ "file": p.file_name().map(|f| f.to_string_lossy().to_string()), "valid": false, "error": e })) } } }
            json!({ "ok": true, "branches": rows }) }
        "settings" => { let (r, team, id) = (root.clone(), s(&b["team"]), s(&b["id"])); match blocking(move || crate::team_repo::settings_at(&r, &team, &id)).await { Some(Ok(v)) => json!({ "ok": true, "settings": v }), Some(Err(e)) => fail(e), None => fail("cut off") } }
        "package_approvals" => { let f = team_files(&root, &s(&b["team"])).join("package-approvals.json"); let l: Value = std::fs::read_to_string(&f).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!([])); json!({ "ok": true, "approved": l }) }
        "package_plan" => { let spec: Result<crate::team_package::Spec, _> = serde_json::from_value(b["spec"].clone()); match spec { Ok(sp) => match crate::team_package::plan(&sp, &root, &s(&b["run"]), &s(&b["node"])) { Ok(p) => json!({ "ok": true, "plan": p }), Err(e) => json!({ "ok": false, "error": e.join("; "), "reasons": e }) }, Err(e) => fail(format!("the package request is malformed: {e}")) } }
        "replay_list" => { let (r, team) = (root.clone(), s(&b["team"])); let t = blocking(move || crate::team_replay::list(&r, &team)).await.unwrap_or_default(); let tune = t.iter().filter(|x| x.split == "tune").count(); json!({ "ok": true, "tasks": t, "tune": tune, "heldout": t.len() - tune }) }
        "replay_oracle" => { if by_agent { return fail("only the person labels a text"); } let (r, team, id, text, pass) = (root.clone(), s(&b["team"]), s(&b["id"]), s(&b["text"]), b["pass"].as_bool().unwrap_or(true)); match blocking(move || crate::team_replay::add_oracle(&r, &team, &id, &text, pass)).await { Some(Ok(())) => json!({ "ok": true }), Some(Err(e)) => fail(e), None => fail("cut off") } }
        "replay_remove" => { if by_agent { return fail("only the person removes a task"); } let (r, team, id) = (root.clone(), s(&b["team"]), s(&b["id"])); match blocking(move || crate::team_replay::remove(&r, &team, &id)).await { Some(Ok(())) => json!({ "ok": true }), Some(Err(e)) => fail(e), None => fail("cut off") } }
        // the runs of both versions are made by the normal run machinery (they cost the person's seats and need their budget); this scores what they recorded
        "experiment_score" => { let (r, team, n) = (root.clone(), s(&b["team"]), b["n"].as_u64().unwrap_or(0) as u32); let budget = b["budget_usd"].as_f64().unwrap_or(0.0); let repeats = b["repeats"].as_u64().unwrap_or(3) as u32; let results = b["results"].clone();
            match blocking(move || -> Result<Value, String> {
                let mut q: std::collections::BTreeMap<(String, String), std::collections::VecDeque<crate::team_experiment::Outcome>> = Default::default();
                for x in results.as_array().cloned().unwrap_or_default() { q.entry((x["arm"].as_str().unwrap_or("").into(), x["task"].as_str().unwrap_or("").into())).or_default().push_back(crate::team_experiment::Outcome { pass: x["pass"].as_bool().unwrap_or(false), cost_usd: x["cost_usd"].as_f64().unwrap_or(0.0), wall_ms: x["wall_ms"].as_i64().unwrap_or(0), gate_stop: x["gate_stop"].as_bool().unwrap_or(false) }); }
                let plan = crate::team_experiment::Plan { repeats, budget_usd: budget, regression_left: crate::team_request::regression_left(&r, &team), goal: crate::team_experiment::Goal::Quality, min_gain: 0.0 };
                let rep = crate::team_experiment::run(&crate::team_replay::tasks(&r, &team, "tune"), &crate::team_replay::tasks(&r, &team, "heldout"), &plan, &mut |arm, t| q.get_mut(&(arm.to_string(), t.id.clone())).and_then(|v| v.pop_front()).ok_or_else(|| "no recorded run".to_string()))?;
                let m = crate::team_request::attach_experiment(&r, &team, n, serde_json::to_value(&rep).unwrap_or_default())?; let auto = crate::team_request::maybe_auto_merge(&r, &team, n, now() as i64); Ok(json!({ "ok": true, "report": rep, "request": auto.clone().unwrap_or(m), "auto_merged": auto.is_some() })) }).await { Some(Ok(v)) => v, Some(Err(e)) => fail(e), None => fail("cut off") } }
        // the real runs of both versions: spends the person's seats, so only the person starts it, with a budget; it goes on in the background
        "experiment_run" => { if by_agent { return fail("only the person starts an experiment: it spends your seats"); } let (r, team, n) = (root.clone(), s(&b["team"]), b["n"].as_u64().unwrap_or(0) as u32);
            match crate::team_executor::start(&r, &team, n, b["repeats"].as_u64().unwrap_or(3) as u32, b["budget_usd"].as_f64().unwrap_or(0.0)) { Ok(()) => json!({ "ok": true, "started": true }), Err(e) => fail(e) } }
        "experiment_status" => { let (r, team, n) = (root.clone(), s(&b["team"]), b["n"].as_u64().unwrap_or(0) as u32); let mut v = crate::team_executor::progress(&r, &team, n); v["ok"] = json!(true); v }
        "request_open" => { let (r, team, problem) = (root.clone(), s(&b["team"]), s(&b["problem"])); let runs: Vec<String> = b["runs"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            let ops: Result<Vec<crate::team_patch::PatchOp>, _> = serde_json::from_value(b["patch"].clone()); let ops = match ops { Ok(o) => o, Err(e) => return fail(format!("the patch is not a list of ops: {e}")) };
            // a request about ONE SECTION (an evolve node's scope): nothing outside it may change
            let scope: Vec<String> = b["scope"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            match blocking(move || { let live = load_graph(&r, &team)?; let cand = crate::team_patch::apply_ops(&live, &ops)?;
                if !scope.is_empty() { let out = crate::team_request::out_of_scope(&crate::team_request::diff(&live, &cand), &scope); if !out.is_empty() { return Err(format!("this request is about the section {}: it also changes {}", scope.join(", "), out.join("; "))); } }
                crate::team_request::open(&r, &live, &cand, &problem, &runs, now() as i64) }).await { Some(Ok(m)) => json!({ "ok": true, "request": m }), Some(Err(e)) => fail(e), None => fail("cut off") } }
        "request_list" => { let (r, team) = (root.clone(), s(&b["team"])); let v = blocking(move || crate::team_request::list(&r, &team)).await.unwrap_or_default(); json!({ "ok": true, "requests": v }) }
        "request_get" => { let (r, team, n) = (root.clone(), s(&b["team"]), b["n"].as_u64().unwrap_or(0) as u32); match blocking(move || crate::team_request::get(&r, &team, n)).await.flatten() { Some(m) => json!({ "ok": true, "request": m, "page": crate::team_request::page(&m) }), None => fail("no such request") } }
        "request_decide" => { let (r, team, n, act, why) = (root.clone(), s(&b["team"]), b["n"].as_u64().unwrap_or(0) as u32, s(&b["action"]), s(&b["reason"])); match blocking(move || crate::team_request::decide(&r, &team, n, &act, &why, "the person", by_agent, now() as i64)).await { Some(Ok(m)) => json!({ "ok": true, "request": m }), Some(Err(e)) => fail(e), None => fail("cut off") } }
        "request_config" => { let (r, team) = (root.clone(), s(&b["team"])); if let Some(on) = b["auto_merge_low"].as_bool() { if by_agent { return fail("only the person turns on a standing yes"); } let (r2, t2) = (r.clone(), team.clone()); if let Some(Err(e)) = blocking(move || crate::team_request::set_auto_merge(&r2, &t2, on)).await { return fail(e); } } json!({ "ok": true, "auto_merge_low": crate::team_request::auto_merge_on(&r, &team) }) }
        "benchmark" => { let (r, team) = (root.clone(), s(&b["team"])); let mut v = blocking(move || crate::team_request::benchmark(&r, &team)).await.unwrap_or_default(); v["ok"] = json!(true); v }
        // SELF-EVOLUTION (the operator 2026-11-22): a goal, a least gain, a most number of trials and a purse; the person starts it (it spends their seats), the Leader may change the numbers when asked
        "evolve_config" => { let (r, team, patch) = (root.clone(), s(&b["team"]), b["patch"].clone()); if by_agent && patch["budget_usd"].as_f64().map_or(false, |x| x > 10.0) { return fail("the Leader may set a budget of at most $10 for self-evolution; more is the person's decision"); } match blocking(move || { let base = crate::team_evolve::cfg(&r, &team); if patch.is_object() && !patch.as_object().map_or(true, |o| o.is_empty()) { crate::team_evolve::set_cfg(&r, &team, crate::team_evolve::merge_cfg(&base, &patch)) } else { Ok(base) } }).await { Some(Ok(c)) => json!({ "ok": true, "config": c }), Some(Err(e)) => fail(e), None => fail("cut off") } }
        "evolve_start" => { let (r, team) = (root.clone(), s(&b["team"])); match crate::team_evolve::start(&r, &team) { Ok(sess) => json!({ "ok": true, "session": sess }), Err(e) => fail(e) } }
        "evolve_status" => { let (r, team) = (root.clone(), s(&b["team"])); let (c, sess, run) = (crate::team_evolve::cfg(&r, &team), crate::team_evolve::session(&r, &team), crate::team_evolve::running(&r, &team)); let (tune, held) = (crate::team_replay::tasks(&r, &team, "tune").len(), crate::team_replay::tasks(&r, &team, "heldout").len()); { let (mr, mp, mw) = crate::mem::headless_status(); json!({ "ok": true, "config": c, "session": sess, "running": run, "tune": tune, "heldout": held, "machine": { "running": mr, "paused": mp, "why": mw, "view": crate::mem::machine_now() } }) } }
        "swarm_config" => { let (r, team, patch) = (root.clone(), s(&b["team"]), b["patch"].clone()); match blocking(move || crate::team_race::set_cfg(&r, &team, &patch)).await { Some(Ok(c)) => json!({ "ok": true, "config": c }), Some(Err(e)) => fail(e), None => fail("cut off") } }
        "swarm_start" => { let (r, team) = (root.clone(), s(&b["team"])); match crate::team_race::start(&r, &team) { Ok(sess) => json!({ "ok": true, "session": sess }), Err(e) => fail(e) } }
        "swarm_status" => { let (r, team) = (root.clone(), s(&b["team"])); let (c, e, sess, run) = (crate::team_race::cfg(&r, &team), crate::team_evolve::cfg(&r, &team), crate::team_race::session(&r, &team), crate::team_race::running(&r, &team)); { let (mr, mp, mw) = crate::mem::headless_status(); let (tune, held) = (crate::team_replay::tasks(&r, &team, "tune").len(), crate::team_replay::tasks(&r, &team, "heldout").len()); json!({ "ok": true, "swarm": c, "config": e, "session": sess, "running": run, "tune": tune, "heldout": held, "machine": { "running": mr, "paused": mp, "why": mw, "view": crate::mem::machine_now() } }) } }
        "swarm_stop" => { let (r, team) = (root.clone(), s(&b["team"])); crate::team_evolve::request_stop(&r, &team); json!({ "ok": true, "stopping": true }) }
        "evolve_stop" => { let (r, team) = (root.clone(), s(&b["team"])); crate::team_evolve::request_stop(&r, &team); json!({ "ok": true, "stopping": true }) }
        "improve_prompt" => { let (r, team) = (root.clone(), s(&b["team"])); let scope: Vec<String> = b["scope"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(); match blocking(move || load_graph(&r, &team).map(|g| { let p = crate::team_improver::prompt(&r, &team, &g); if scope.is_empty() { p } else { format!("{p}\n\nTHIS REQUEST IS ABOUT ONE SECTION OF THE TEAM: you may change ONLY these nodes: {}. A change to any other node, to the order of the steps or to the budget is refused.", scope.join(", ")) } })).await { Some(Ok(t)) => json!({ "ok": true, "prompt": t }), Some(Err(e)) => fail(e), None => fail("cut off") } }
        "improve_request" => { let (r, team, reply) = (root.clone(), s(&b["team"]), s(&b["reply"])); let runs: Vec<String> = b["runs"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            match blocking(move || crate::team_improver::request_from_reply(&r, &team, &reply, &runs, now() as i64)).await { Some(Ok(m)) => json!({ "ok": true, "request": m }), Some(Err(e)) => fail(e), None => fail("cut off") } }
        "export" => { let (r, team) = (root.clone(), s(&b["team"])); let ids: Vec<String> = b["tasks"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(); match blocking(move || crate::team_export::export(&r, &team, &ids)).await { Some(Ok(t)) => json!({ "ok": true, "file": t }), Some(Err(e)) => fail(e), None => fail("cut off") } }
        "import_review" => match crate::team_export::review(&s(&b["file"])) { Ok(v) => v, Err(e) => fail(e) },
        "import" => { if by_agent { return fail("only the person imports a team"); } let (r, file, name, ok) = (root.clone(), s(&b["file"]), s(&b["name"]), b["confirmed"].as_bool().unwrap_or(false)); match blocking(move || crate::team_export::import(&r, &file, &name, ok, now() as i64)).await { Some(Ok(v)) => v, Some(Err(e)) => fail(e), None => fail("cut off") } }
        "recall" => { let (r, team) = (root.clone(), s(&b["team"])); let text = blocking(move || crate::team_recall::recall(&r, &team)).await.unwrap_or_default(); json!({ "ok": true, "text": text }) }
        "leader_inbox" => { let (r, team) = (root.clone(), s(&b["team"])); let rows = blocking(move || read_jsonl(&team_files(&r, &team).join("leader-inbox.jsonl"))).await.unwrap_or_default(); let unread = rows.iter().filter(|x| x["status"] != json!("handled")).count(); json!({ "ok": true, "unread_or_undecided": unread, "messages": rows.into_iter().rev().take(10).collect::<Vec<_>>() }) }
        "leader_ack" => { let (r, team, id) = (root.clone(), s(&b["team"]), s(&b["id"])); let ok = blocking(move || { let p = team_files(&r, &team).join("leader-inbox.jsonl"); let mut rows = read_jsonl(&p); let mut hit = false; for x in rows.iter_mut() { if s(&x["id"]) == id || (id.is_empty() && x["status"] != json!("handled")) { x["status"] = json!("handled"); x["handled_ms"] = json!(now()); hit = true; } } write_jsonl(&p, &rows); hit }).await.unwrap_or(false); if ok { json!({ "ok": true }) } else { fail("no such message") } }
        "campaign_start" => { let r = root.clone(); match campaign_start(&r, &b).await { Ok(v) => v, Err(e) => fail(e) } }
        "campaign_status" => { let (r, id) = (root.clone(), s(&b["campaign"])); match blocking(move || load_campaign(&r, &id)).await.flatten() { Some(c) => json!({ "ok": true, "campaign": c, "reading": c.reading }), None => fail("no such campaign") } }
        "campaign_stop" => { let id = s(&b["campaign"]); match campaign_stops().lock().unwrap_or_else(|p| p.into_inner()).get(&id) { Some(f) => { f.store(true, std::sync::atomic::Ordering::Relaxed); json!({ "ok": true, "stopping": id }) } None => fail("that campaign is not running") } }
        "campaign_list" => { let r = root.clone(); let rows: Vec<Value> = blocking(move || std::fs::read_dir(campaigns_dir(&r)).into_iter().flatten().flatten().filter_map(|e| serde_json::from_str::<crate::team_campaign::Campaign>(&std::fs::read_to_string(e.path()).ok()?).ok()).map(|c| json!({ "campaign": c.id, "team": c.team, "status": c.status, "winner": c.winner, "spent_usd": c.spent_usd, "budget_usd": c.budget_usd, "started_ms": c.started_ms })).collect()).await.unwrap_or_default(); json!({ "ok": true, "campaigns": rows }) }
        "apps_list" => crate::team_apps::list().await,
        "apps_preflight" => { let (r, team) = (root.clone(), s(&b["team"])); let g = blocking(move || load_graph(&r, &team).ok()).await.flatten(); match g { Some(g) => crate::team_apps::preflight_of(&g.to_json()).await, None => fail("no such team") } }
        "apps_login" => match crate::team_apps::login(&s(&b["app"])) { Ok(v) => v, Err(e) => fail(e) },
        "seats_list" => crate::team_seats::inventory().await,
        "seats_fit" => { let inv = crate::team_seats::inventory().await; crate::team_seats::fit(&s(&b["stage"]), &s(&b["seat"]), &inv) }
        "seats_register" => match crate::team_seats::register(&s(&b["alias"]), &s(&b["url"]), &s(&b["model"]), b["ctx"].as_u64(), b["max_tokens"].as_u64()) { Ok(v) => v, Err(e) => fail(e) },
        "seats_pull" => match crate::team_seats::pull(&s(&b["tag"]), b["approved"].as_bool().unwrap_or(false)) { Ok(v) => v, Err(e) => fail(e) },
        "seats_trial" => match crate::team_seats::trial(&b).await { Ok(v) => v, Err(e) => fail(e) },
        "result" => { let (r, run) = (root.clone(), s(&b["run"])); match blocking(move || result_of(&r, &run)).await { Some(Ok(v)) => v, Some(Err(e)) => fail(e), None => fail("cut off") } }
        "compose_auto" => { let (r, run) = (root.clone(), s(&b["run"])); match blocking(move || compose_auto(&r, &run)).await { Some(Ok(v)) => v, Some(Err(e)) => fail(e), None => fail("cut off") } }
        // THE WORKSPACE AT A MERGE GATE: what would stop the land, set aside by the person's click, put back after the land
        "workspace" => { let (r, run) = (root.clone(), s(&b["run"])); let mut v = blocking(move || crate::team_land::state(&r, &run)).await.unwrap_or_else(|| json!({ "dirty": [], "stashed": false })); v["ok"] = json!(true); v }
        "stash" => { if by_agent { return fail("only the person sets their own work aside"); } let (r, run) = (root.clone(), s(&b["run"])); match blocking(move || crate::team_land::stash(&r, &run)).await { Some(Ok(v)) => v, Some(Err(e)) => fail(e), None => fail("the stash was cut off") } }
        "unstash" => { if by_agent { return fail("only the person puts their own work back"); } let (r, run) = (root.clone(), s(&b["run"])); match blocking(move || crate::team_land::restore(&r, &run)).await { Some(Ok(v)) => v, Some(Err(e)) => fail(e), None => fail("the restore was cut off") } }
        "archive" => { let (r, run) = (root.clone(), s(&b["run"])); let v = blocking(move || crate::team_archive::list(&r, &run)).await.unwrap_or_default(); json!({ "ok": true, "files": v, "reading": "what a run made, saved before anything could remove it; the bytes are in .antiloki/team/archive.sqlite and are never deleted" }) }
        "media" => { let (r, run) = (root.clone(), s(&b["run"])); let v = blocking(move || crate::team_media::media_of(&r, &run)).await.unwrap_or_default(); json!({ "ok": true, "media": v }) }
        "artifacts" => { let (r, run) = (root.clone(), s(&b["run"])); let v = blocking(move || artifacts_of(&r, &run)).await.unwrap_or_default(); json!({ "ok": true, "artifacts": v }) }
        "artifact" => { let (r, run, name) = (root.clone(), s(&b["run"]), s(&b["name"])); let node = if s(&b["node"]).is_empty() { None } else { Some(s(&b["node"])) }; match blocking(move || artifact_text(&r, &run, node.as_deref(), &name)).await { Some(Ok(v)) => v, Some(Err(e)) => fail(e), None => fail("cut off") } }
        "skill_list" => json!({ "ok": true, "skills": crate::team_skills::list(&root, &s(&b["team"])) }),
        "skill_get" => match crate::team_skills::get(&root, &s(&b["team"]), &s(&b["name"])) { Some(sk) => json!({ "ok": true, "skill": sk }), None => fail(format!("there is no skill called {}", s(&b["name"]))) },
        "skill_approve" => { if by_agent { return fail("only the person approves a skill: an agent writes it and the person reads it"); } match crate::team_skills::approve(&root, &s(&b["team"]), &s(&b["name"])) { Ok(sk) => json!({ "ok": true, "skill": sk }), Err(e) => fail(e) } }
        "skill_delete" => json!({ "ok": crate::team_skills::remove(&root, &s(&b["team"]), &s(&b["name"])) }),
        "list" => { let gs: Vec<Value> = list_graphs(&root).iter().map(|g| { let r = crate::team_skills::report_for(&root, g, false); json!({ "team": g.team, "id": slug(&g.team), "nodes": g.nodes.len(), "budget_usd": g.budget_usd, "valid": r.ok(), "worst_usd": r.worst_usd }) }).collect();
            json!({ "ok": true, "graphs": gs, "runs": runs_of(&root, ""), "live": live_runs(&root).iter().map(|h| h.id.clone()).collect::<Vec<_>>() }) }
        "get" => match team_of(&b) { Ok(g) => json!({ "ok": true, "graph": g.to_json(), "report": crate::team_skills::report_for(&root, &g, false), "runs": runs_of(&root, &g.team) }), Err(e) => fail(e) },
        "validate" => match team_of(&b) { Ok(g) => json!({ "ok": true, "report": crate::team_skills::report_for(&root, &g, false), "words": crate::team_skills::report_for(&root, &g, false).words() }), Err(e) => fail(e) },
        "simulate" => match team_of(&b) { Ok(g) => { let stubs: BTreeMap<String, Vec<Value>> = b["stubs"].as_object().map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_array().cloned().unwrap_or_else(|| vec![v.clone()]))).collect()).unwrap_or_default();
            let rep = validate(&g); if !rep.ok() { return json!({ "ok": false, "error": format!("this graph is not valid: {}", rep.words().join("; ")), "report": rep }); }
            match simulate(&g, &stubs, b["seed"].as_object().cloned().unwrap_or_default()) { Ok(sim) => json!({ "ok": true, "sim": sim, "report": rep }), Err(e) => fail(e) } } Err(e) => fail(e) },
        "save" => match Graph::from_json(&b["graph"]) { Ok(g) => {
            // a schedule is the person's: a save from a pane may not add, change or remove one
            if by_agent { let old = load_graph(&root, &g.team).ok(); if crate::team_triggers::triggers_changed(old.as_ref(), &g) { return fail("schedules are set by the person: leave `triggers` and `limits` as they are"); } }
            if g.team.trim().is_empty() { return fail("the graph has no team name"); }
            if by_agent && !graph_path(&root, &g.team).exists() { if let Some(m) = second_team_refusal(&root, &g.team, by_agent, b["new_team"].as_bool().unwrap_or(false)) { return fail(m); } }
            // a sub-team node must not make a cycle or nest too deep (the teams on disk are looked at)
            if !crate::team_subteam::subteams(&g).is_empty() { if let Err(e) = crate::team_subteam::check(&root, &g) { return fail(e); } }
            let mut g = g; g.branch = crate::thread_tree::slug(&root, &slug(&g.team));
            let old = load_graph(&root, &g.team).ok(); let d = old.as_ref().map(|o| diff(o, &g));
            // a team is a thread: a graph for a team that has none gets one (and its tree), so it can be seen and run
            let known = crate::cowork_threads::load(&root)["threads"].as_array().map_or(false, |a| a.iter().any(|t| t["id"] == json!(slug(&g.team))));
            if !known { let _ = crate::cowork_threads::apply(&root, &json!({ "op": "create_team", "name": g.team, "select": false })); let (r2, t2) = (root.clone(), slug(&g.team)); let _ = blocking(move || crate::thread_tree::ensure(&r2, &t2)).await; }
            match save_graph_as(&root, &g, b.get("change").filter(|c| c.is_object())) { Ok(_) => json!({ "ok": true, "graph": g.to_json(), "report": crate::team_skills::report_for(&root, &g, false), "diff": d, "created": old.is_none() }), Err(e) => fail(e) } } Err(e) => fail(e) },
        "create" => {
            let name = s(&b["name"]); if name.is_empty() { return fail("a team needs a name"); } let goal = s(&b["goal"]); let key = if s(&b["template"]).is_empty() { if goal.is_empty() && !by_agent { "feature".to_string() } else { "blank".to_string() } } else { s(&b["template"]) };
            if let Some(m) = second_team_refusal(&root, &name, by_agent, b["new_team"].as_bool().unwrap_or(false)) { return fail(m); }
            let Some(g) = template(&key, &name, b["budget_usd"].as_f64().unwrap_or(6.0)) else { return fail(format!("no template called {key}: one of {}", TEMPLATES.join(", "))) };
            if graph_path(&root, &name).exists() { return fail(format!("a team called {name} exists")); }
            // `adopt` is the "New team" thread its leader is talking in: it becomes the team's (the tree is made first, so a refusal leaves that thread as it was, with the chat on it)
            let adopt = s(&b["adopt"]);
            if !adopt.is_empty() { let (r2, t2) = (root.clone(), slug(&name)); if let Some(Err(e)) = blocking(move || crate::thread_tree::ensure(&r2, &t2)).await { return fail(format!("the team's tree could not be made: {e}")); } }
            let op = if adopt.is_empty() { json!({ "op": "create_team", "name": name }) } else { json!({ "op": "adopt_team", "thread": adopt, "name": name, "leader": b["leader"] }) };
            let r = crate::cowork_threads::apply(&root, &op); let v = match r { Ok(v) => v, Err(e) => return fail(e) };
            if adopt.is_empty() { let (r2, t2) = (root.clone(), slug(&name)); if let Some(Err(e)) = blocking(move || crate::thread_tree::ensure(&r2, &t2)).await { let _ = crate::cowork_threads::apply(&root, &json!({ "op": "delete", "thread": slug(&name) })); return fail(format!("the team's tree could not be made: {e}")); } }
            // the leader is already talking: it is on file, so it is not typed the team's (empty) memory as if it had just been started
            if let (false, Some(p)) = (adopt.is_empty(), b["leader"].as_i64()) { let d = team_files(&root, &slug(&name)); let _ = std::fs::create_dir_all(&d); let _ = std::fs::write(d.join("leader.json"), json!({ "pane": p, "t_ms": now() }).to_string()); }
            let mut g = g; g.branch = crate::thread_tree::slug(&root, &slug(&name));   // the branch it really has: the thread tree's
            if let Err(e) = save_graph(&root, &g) { return fail(e); }
            json!({ "ok": true, "graph": g.to_json(), "report": crate::team_skills::report_for(&root, &g, false), "thread": slug(&name), "threads": v, "brief": if goal.is_empty() { Value::Null } else { json!(leader_brief(&name, &goal, g.budget_usd)) } })
        }
        "template" => { let key = s(&b["template"]); let name = if s(&b["name"]).is_empty() { s(&b["team"]) } else { s(&b["name"]) }; match template(&key, &name, b["budget_usd"].as_f64().unwrap_or(6.0)) { Some(g) => json!({ "ok": true, "graph": g.to_json(), "report": validate(&g) }), None => fail(format!("no template called {key}: one of {}", TEMPLATES.join(", "))) } }
        "delete" => { let g = match team_of(&b) { Ok(g) => g, Err(e) => return fail(e) }; if live_runs(&root).iter().any(|h| h.team == slug(&g.team)) { return fail("a run is going: stop it first"); }
            // NOTHING IS LOST: the graph is saved to the archive before it goes; the team's thread goes with it, and its two panes (the team's own and the Leader's terminal, which cannot be closed by hand on a team thread) are closed here
            let (r2, g2) = (root.clone(), g.clone()); let _ = blocking(move || { let ctx = crate::team_archive::Ctx { root: r2.clone(), run: format!("team:{}", slug(&g2.team)), team: g2.team.clone(), node: "delete".into() }; let tmp = std::env::temp_dir().join(format!("antiloki-team-{}.json", slug(&g2.team))); if std::fs::write(&tmp, serde_json::to_string_pretty(&g2.to_json()).unwrap_or_default()).is_ok() { crate::team_archive::store_file(&ctx, &format!("{}.team.json", slug(&g2.team)), &tmp, "the team was deleted"); let _ = std::fs::remove_file(&tmp); } }).await;
            crate::cowork::release_thread_panes(state, &root, &slug(&g.team)).await;
            let ok = delete_graph(&root, &g.team); let _ = crate::cowork_threads::apply(&root, &json!({ "op": "delete", "thread": slug(&g.team) })); json!({ "ok": ok }) }
        "run" => {
            let mut g = match team_of(&b) { Ok(g) => g, Err(e) => return fail(e) };
            if live_runs(&root).iter().any(|h| h.team == slug(&g.team)) { return fail("this team is running already: stop it, or wait"); }
            if let Some(u) = b["budget_usd"].as_f64().filter(|x| *x > 0.0) { if by_agent && u > g.budget_usd && g.budget_usd > 0.0 { return fail("the leader cannot raise the ceiling the person set"); } g.budget_usd = u; let _ = save_graph(&root, &g); }
            let engine = s(&b["engine"]);
            let recipe = crate::team_repo::head(&root, &g.team); match start(&root, g, &s(&b["ask"]), &engine).await { Ok(h) => { append_jsonl(&team_files(&root, &h.team).join("run-recipes.jsonl"), &json!({ "run": h.id, "recipe": recipe, "t_ms": now() })); json!({ "ok": true, "run": h.id, "thread": slug(&h.team), "recipe": recipe }) }, Err(e) => fail(e) }
        }
        "status" => { let run = s(&b["run"]); match status_of(&root, &run) { Some(v) => json!({ "ok": true, "run": v }), None => fail(format!("no run called {run}")) } }
        "runs" => json!({ "ok": true, "runs": runs_of(&root, &s(&b["team"])) }),
        // THE EDITOR VIEW'S EVENTS (2026-11-21): what the agents of a run wrote, from event number `since` on; and a file as the run started
        "edits" => { let (r, run, since) = (root.clone(), s(&b["run"]), b["since"].as_u64().unwrap_or(0) as usize); let (ev, total) = blocking(move || crate::run_edits::read_since(&r, &run, since, 400)).await.unwrap_or((vec![], 0)); json!({ "ok": true, "events": ev, "total": total, "live": handle(&s(&b["run"])).is_some(), "now_ms": now() }) }
        // THE PERSON EDITS A FILE OF A RUN THAT IS NOT GOING (the Editor view is read-only while the agents work): only inside the run's folder, only text, at most 1 MB
        "file_save" => {
            if by_agent { return fail("only the person edits a file here"); }
            let (r, run, path, text) = (root.clone(), s(&b["run"]), s(&b["path"]), s(&b["text"])); if handle(&run).is_some() { return fail("the agents are working: the editor is read-only until the run stops"); }
            if text.len() > 1_000_000 { return fail("the file is over 1 MB"); } if path.split('/').any(|p| p == ".." || p.is_empty()) || path.starts_with('/') || path.contains('\\') { return fail("a path inside the project, like src/a.ts"); }
            let Some(dir) = crate::run_edits::work_dir(&r, &run) else { return fail("no agent has written in this run: there is no folder to save into") };
            let target = std::path::Path::new(&dir).join(&path); if !target.starts_with(&dir) { return fail("a path inside the project"); }
            match blocking(move || { if let Some(d) = target.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; } std::fs::write(&target, text).map_err(|e| e.to_string()) }).await { Some(Ok(())) => json!({ "ok": true, "path": path }), Some(Err(e)) => fail(e), None => fail("cut off") }
        }
        // A FILE AS IT IS NOW in the run's folder (what the editor opens when the agents have stopped)
        "file_now" => {
            let (r, run, path) = (root.clone(), s(&b["run"]), s(&b["path"])); if path.split('/').any(|p| p == ".." || p.is_empty()) || path.starts_with('/') { return fail("a path inside the project"); }
            let Some(dir) = crate::run_edits::work_dir(&r, &run) else { return json!({ "ok": true, "path": path, "text": "", "exists": false }) };
            match blocking(move || std::fs::read_to_string(std::path::Path::new(&dir).join(&path)).map(|t| (path, t.chars().take(1_000_000).collect::<String>()))).await { Some(Ok((p, t))) => json!({ "ok": true, "path": p, "text": t, "exists": true }), _ => json!({ "ok": true, "path": s(&b["path"]), "text": "", "exists": false }) }
        }
        // THE PREVIEWS A RUN HAS UP (the Preview node and the Editor view's dock), and ending them by hand
        "previews" => json!({ "ok": true, "previews": crate::run_preview::list(&s(&b["run"])), "live": handle(&s(&b["run"])).is_some() }),
        "preview_stop" => { if by_agent { return fail("only the person ends a preview"); } json!({ "ok": true, "stopped": crate::run_preview::stop_run(&s(&b["run"])) }) }
        // THE WHOLE PROJECT FOR THE EDITOR VIEW'S EXPLORER: every file of the run's folder (not just the ones agents touched), so any of them can be opened
        "file_tree" => { let (r, run) = (root.clone(), s(&b["run"])); match crate::run_edits::work_dir(&r, &run) { Some(w) => { let w2 = w.clone(); json!({ "ok": true, "dir": w, "files": blocking(move || crate::run_edits::tree_of(std::path::Path::new(&w2))).await.unwrap_or_default() }) } None => json!({ "ok": true, "files": [] }) } }
        // THE BOOK (RFC 0018): the laws of any kind of work. The person's and the Leader's way in; the engine judges, only a person ratifies
        // SPECIALISTS (RFC 0017 §2.4): a named agent with a memory of its own and deterministic tools. An agent may write a DRAFT, add notes and recall; approving is the person's
        "specialist_list" => { let r = root.clone(); match blocking(move || crate::specialists::list(&r).into_iter().map(|x| { let n = crate::specialists::note_count(&r, &x.id); json!({ "specialist": x, "notes": n, "usable": crate::specialists::usable(&x) }) }).collect::<Vec<_>>()).await { Some(v) => json!({ "ok": true, "specialists": v }), None => fail("cut off") } }
        "specialist_get" => { let (r, id) = (root.clone(), s(&b["id"]));
            // opening a specialist keeps what its open panes have said so far (terminal or chat)
            { let (r0, id0) = (r.clone(), id.clone()); let panes = blocking(move || crate::specialists::panes_of(&r0, &id0)).await.unwrap_or_default(); for pid in panes.into_iter().take(4) { let _ = crate::cowork::transcript_value(state, pid, 400).await; } } match blocking(move || crate::specialists::get(&r, &id).map(|x| json!({ "specialist": x, "usable": crate::specialists::usable(&x), "notes": crate::specialists::notes(&r, &id, "", 200), "memory": crate::specialists::memory_health(&r, &id), "warnings": if x.kind == "leader" { vec![] } else { crate::specialists::lint_specialist(&r, &x) } }))).await.flatten() { Some(v) => json!({ "ok": true, "specialist": v["specialist"], "usable": v["usable"], "notes": v["notes"], "memory": v["memory"], "warnings": v["warnings"], "allowed": { "tools": crate::specialists::ALLOWED_TOOLS, "shapes": crate::specialists::SHAPES, "colors": crate::specialists::COLORS } }), None => fail("no such specialist") } }
        "specialist_save" => { let (r, def) = (root.clone(), b["specialist"].clone()); match blocking(move || serde_json::from_value::<crate::specialists::Specialist>(def).map_err(|e| e.to_string()).and_then(|x| crate::specialists::save(&r, x, by_agent))).await { Some(Ok(x)) => json!({ "ok": true, "specialist": x, "usable": crate::specialists::usable(&x) }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_retire" => { if by_agent { return fail("only the person removes a specialist"); } let (r, id) = (root.clone(), s(&b["id"])); match blocking(move || crate::specialists::retire(&r, &id)).await { Some(Ok(())) => json!({ "ok": true }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_approve" => { if by_agent { return fail("only the person approves a specialist: the Leader drafts it, you look, you decide"); } let (r, id) = (root.clone(), s(&b["id"])); match blocking(move || crate::specialists::approve(&r, &id)).await { Some(Ok(x)) => json!({ "ok": true, "specialist": x }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_note" => { let (r, id, kind, text, why) = (root.clone(), s(&b["id"]), s(&b["kind"]), s(&b["text"]), s(&b["reason"])); let src: Vec<i64> = b["sources"].as_array().map(|a| a.iter().filter_map(|x| x.as_i64()).collect()).unwrap_or_default(); let who = if by_agent { id.clone() } else { "you".to_string() }; match blocking(move || crate::specialists::add_note(&r, &id, if kind.is_empty() { "note" } else { &kind }, &text, &why, &who, &src)).await { Some(Ok(n)) => json!({ "ok": true, "note": n }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_notes" => { let (r, id, q) = (root.clone(), s(&b["id"]), s(&b["query"])); let limit = b["limit"].as_u64().unwrap_or(20) as usize; match blocking(move || { let n = crate::specialists::notes(&r, &id, &q, limit); if n.is_empty() && crate::specialists::get(&r, &id).is_none() { Err(format!("no specialist `{id}`: antiloki_remember op `list` shows them")) } else { Ok(n) } }).await { Some(Ok(n)) => json!({ "ok": true, "notes": n }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_verify" => { let (r, id, n) = (root.clone(), s(&b["id"]), b["n"].as_i64().unwrap_or(0)); match blocking(move || crate::specialists::verify_note(&r, &id, n)).await { Some(Ok(v)) => json!({ "ok": true, "verified": v }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_yaml" => { let (r, id) = (root.clone(), s(&b["id"])); match blocking(move || crate::specialists::get(&r, &id).map(|x| crate::specialists::export_yaml(&x))).await.flatten() { Some(y) => json!({ "ok": true, "yaml": y }), None => fail("no such specialist") } }
        "specialist_import" => { let (r, y) = (root.clone(), s(&b["yaml"])); match blocking(move || crate::specialists::import_yaml(&r, &y)).await { Some(Ok(x)) => json!({ "ok": true, "specialist": x }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_consolidate" => { let (r, id, text, seat) = (root.clone(), s(&b["id"]), s(&b["text"]), s(&b["seat"])); let to = b["to"].as_i64().unwrap_or(0); match blocking(move || crate::specialists::consolidate(&r, &id, &text, to, if seat.is_empty() { "unknown" } else { &seat })).await { Some(Ok(v)) => json!({ "ok": true, "consolidation": v }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_chunk" => { let (r, id) = (root.clone(), s(&b["id"])); let (seat, sess, role, text) = (s(&b["seat"]), s(&b["session"]), s(&b["role"]), s(&b["text"])); let (ti, to, cost) = (b["tokens_in"].as_u64().unwrap_or(0), b["tokens_out"].as_u64().unwrap_or(0), b["cost_usd"].as_f64().unwrap_or(0.0)); match blocking(move || crate::specialists::add_chunk(&r, &id, &seat, &sess, &role, &text, ti, to, cost)).await { Some(Ok(n)) => json!({ "ok": true, "chunk": n }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_suggest" => { let (r, a, n, w) = (root.clone(), s(&b["agent"]), s(&b["name"]), s(&b["why"])); match blocking(move || crate::specialists::suggest(&r, &a, &n, &w)).await { Some(Ok(v)) => json!({ "ok": true, "suggestion": v }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_suggestions" => { let r = root.clone(); match blocking(move || crate::specialists::suggestions(&r)).await { Some(v) => json!({ "ok": true, "suggestions": v }), None => fail("cut off") } }
        "specialist_sql" => { let (r, id, q) = (root.clone(), s(&b["id"]), s(&b["sql"])); match blocking(move || crate::specialists::query(&r, &id, &q)).await { Some(Ok(v)) => json!({ "ok": true, "result": v }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_fork" => { let (r, id) = (root.clone(), s(&b["id"])); match blocking(move || crate::specialists::fork(&r, &id, by_agent)).await { Some(Ok(x)) => json!({ "ok": true, "specialist": x }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_bind_leader" => { let (r, team, pid) = (root.clone(), s(&b["team"]), b["bind_pane"].as_i64().unwrap_or(0)); if team.is_empty() { return fail("say which team"); } match blocking(move || crate::specialists::ensure_leader(&r, &team).map(|x| { if pid > 0 { crate::specialists::bind_pane(pid, &r, &x.id); } x })).await { Some(Ok(x)) => json!({ "ok": true, "specialist": x }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_ask" => { let (r, c, a, q) = (root.clone(), s(&b["id"]), s(&b["assistant"]), s(&b["question"])); match blocking(move || crate::specialists::ask(&r, &c, &a, &q)).await { Some(Ok(v)) => json!({ "ok": true, "answer": v }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_contract_open" => { let (r, from, to, task) = (root.clone(), s(&b["id"]), s(&b["to"]), s(&b["task"])); let dl: Vec<String> = b["deliverables"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(); let ck: Vec<Value> = b["checks"].as_array().cloned().unwrap_or_default(); match blocking(move || crate::specialists::contract_open(&r, &from, &to, &task, &dl, &ck)).await { Some(Ok(c)) => json!({ "ok": true, "contract": c }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_contract_deliver" => { let (r, id, by, note) = (root.clone(), s(&b["contract"]), s(&b["id"]), s(&b["note"])); match blocking(move || crate::specialists::contract_deliver(&r, &id, &by, &note)).await { Some(Ok(c)) => json!({ "ok": true, "contract": c }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_contract_check" => { let (r, id, by) = (root.clone(), s(&b["contract"]), if by_agent { s(&b["id"]) } else { "you".to_string() });
            // where the two work: their open panes' worktrees
            let extra: Vec<std::path::PathBuf> = { let (r2, id2) = (r.clone(), id.clone()); let parties = blocking(move || crate::specialists::contract_get(&r2, &id2).map(|c| (c["from"].as_str().unwrap_or("").to_string(), c["to"].as_str().unwrap_or("").to_string()))).await.flatten(); match parties { Some((f, t)) => { let r3 = r.clone(); let panes = blocking(move || { let mut v = crate::specialists::panes_of(&r3, &f); v.extend(crate::specialists::panes_of(&r3, &t)); v }).await.unwrap_or_default(); panes.into_iter().filter_map(|p| crate::cowork_mcp::pane_cwd(state, p)).map(std::path::PathBuf::from).collect() } None => vec![] } };
            match blocking(move || crate::specialists::contract_check(&r, &id, &by, &extra)).await { Some(Ok(c)) => json!({ "ok": true, "contract": c }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_contract_cancel" => { let (r, id, by) = (root.clone(), s(&b["contract"]), if by_agent { s(&b["id"]) } else { "you".to_string() }); match blocking(move || crate::specialists::contract_cancel(&r, &id, &by)).await { Some(Ok(c)) => json!({ "ok": true, "contract": c }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_contract_lint" => { let c = if b["contract"].is_object() { b["contract"].clone() } else { json!({ "to": b["to"], "task": b["task"], "deliverables": b["deliverables"], "checks": b["checks"] }) }; let issues = crate::specialists::lint_contract(&c); json!({ "ok": true, "valid": issues.is_empty(), "issues": issues }) }
        "specialist_contracts" => { let (r, who, open) = (root.clone(), s(&b["id"]), b["open"].as_bool().unwrap_or(true)); match blocking(move || crate::specialists::contracts_of(&r, &who, open)).await { Some(v) => json!({ "ok": true, "contracts": v }), None => fail("cut off") } }
        "specialist_tool" => { let (r, id, name, input) = (root.clone(), s(&b["id"]), s(&b["name"]), b["input"].clone()); match blocking(move || crate::specialists::run_tool_by_name(&r, &id, &name, &input)).await { Some(Ok(v)) => json!({ "ok": true, "result": v }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "specialist_brief" => { let (r, id, w) = (root.clone(), s(&b["id"]), s(&b["work"])); match blocking(move || crate::specialists::brief(&r, &id, &w)).await { Some(Ok(t)) => json!({ "ok": true, "brief": t }), Some(Err(e)) => fail(&e), None => fail("cut off") } }
        "book_list" => { let r = root.clone(); match blocking(move || { let (p, problems) = crate::lanes::load_book(&r); let ws = crate::lanes::workspace_ids(&r); (p.iter().map(|x| crate::lanes::summary(x, ws.contains(&x.id))).collect::<Vec<_>>(), problems) }).await { Some((packs, problems)) => json!({ "ok": true, "packs": packs, "problems": problems }), None => fail("cut off") } }
        "book_show" => { let (r, pid, key) = (root.clone(), s(&b["pack"]), s(&b["law"])); match blocking(move || crate::lanes::load_book(&r).0.into_iter().find(|p| p.id == pid)).await.flatten() { Some(p) => if key.is_empty() { json!({ "ok": true, "pack": p }) } else { match p.laws.iter().find(|l| l.key == key) { Some(l) => json!({ "ok": true, "law": l }), None => fail("no such law in that pack") } }, None => fail("no such pack") } }
        "book_run" => {
            let (r, run) = (root.clone(), s(&b["run"])); let dir = if run.is_empty() { s(&b["dir"]) } else { crate::run_edits::work_dir(&r, &run).unwrap_or_default() }; let dir = if dir.is_empty() { r.clone() } else { dir };
            let packs: Vec<String> = b["packs"].as_array().map(|x| x.iter().filter_map(|y| y.as_str().map(String::from)).collect()).unwrap_or_default(); let globs: Vec<String> = b["paths"].as_array().map(|x| x.iter().filter_map(|y| y.as_str().map(String::from)).collect()).unwrap_or_default();
            let (candidates, fail_on, limit) = (b["candidates"].as_bool().unwrap_or(false), { let f = s(&b["fail_on"]); if crate::lanes::SEVERITIES.contains(&f.as_str()) { f } else { "error".to_string() } }, b["limit"].as_u64().unwrap_or(300).clamp(1, 2000) as usize);
            match blocking(move || { let (book, problems) = crate::lanes::load_book(&r); let arts = crate::lanes::collect(std::path::Path::new(&dir), &globs, 2000); let mut rep = crate::lanes::judge(&book, &arts, &crate::lanes::Opts { packs, candidates, fail_on }); if let Some(f) = rep["findings"].as_array_mut() { let n = f.len(); f.truncate(limit); rep["findings_total"] = json!(n); } rep["book_problems"] = json!(problems); rep }).await { Some(rep) => rep, None => fail("cut off") }
        }
        "book_test" => { let law: Option<crate::lanes::Law> = if b["law"].is_object() { serde_json::from_value(b["law"].clone()).ok() } else { let (r, pid, key) = (root.clone(), s(&b["pack"]), s(&b["key"])); blocking(move || crate::lanes::load_book(&r).0.into_iter().find(|p| p.id == pid).and_then(|p| p.laws.into_iter().find(|l| l.key == key))).await.flatten() }; match law { Some(l) => { let mut o = crate::lanes::test_law(&l); o["key"] = json!(l.key); o["passed"] = o["ok"].clone(); o["ok"] = json!(true); o } None => fail("give a `law` object, or a saved `pack` and `key`") } }
        "book_save" => {
            let text = if b["pack"].is_string() { s(&b["pack"]) } else { b["pack"].to_string() }; let (r, agent) = (root.clone(), by_agent);
            match blocking(move || -> Result<Value, String> { let mut p = crate::lanes::parse_pack(&text)?; let existing = crate::lanes::load_book(&r).0.into_iter().find(|x| x.id == p.id); if agent { p = crate::lanes::as_agent_proposal(p, existing.as_ref()); } else { for l in p.laws.iter_mut() { if l.status == "ratified" && crate::lanes::ratify(l).is_err() { l.status = "candidate".into(); } } } let path = crate::lanes::save_pack(&r, &p)?; Ok(json!({ "ok": true, "path": path, "pack": crate::lanes::summary(&p, true) })) }).await { Some(Ok(v)) => v, Some(Err(e)) => fail(e), None => fail("cut off") }
        }
        "book_ratify" => {
            if by_agent { return fail("only the person ratifies a law: the Leader proposes, the engine tests, you decide"); }
            let (r, pid, key) = (root.clone(), s(&b["pack"]), s(&b["key"]));
            match blocking(move || -> Result<Value, String> { let mut p = crate::lanes::load_book(&r).0.into_iter().find(|p| p.id == pid).ok_or("no such pack")?; let i = p.laws.iter().position(|l| l.key == key).ok_or("no such law")?; let was = p.laws[i].clone(); let mut n = crate::lanes::ratify(&was)?; if was.status == "ratified" { n.version = was.version; } p.laws[i] = n; crate::lanes::save_pack(&r, &p)?; Ok(json!({ "ok": true, "pack": crate::lanes::summary(&p, true) })) }).await { Some(Ok(v)) => v, Some(Err(e)) => fail(e), None => fail("cut off") }
        }
        // THE DOCUMENTATION OF A FILE, from the file (the Editor view's docs icon): any kind of file, deterministic; the run's own folder
        "doc_of" => {
            let (r, run, path) = (root.clone(), s(&b["run"]), s(&b["path"])); if path.split('/').any(|p| p == ".." || p.is_empty()) || path.starts_with('/') { return fail("a path inside the project"); }
            // the folder: a run's own, or a folder given by the page that frames this one (X-ray's docs asks for a file it has no page for): only antiloki's own trees or this workspace, never an arbitrary folder
            let given = s(&b["dir"]); let dir = if !given.is_empty() && (given == r || crate::thread_tree::is_tree_dir(&given)) { given } else { crate::run_edits::work_dir(&r, &run).unwrap_or(r.clone()) };
            let r2 = r.clone();
            match blocking(move || { let p = std::path::Path::new(&dir).join(&path); std::fs::read(&p).map(|b| { let (book, _) = crate::lanes::load_book(&r2); let files: std::collections::BTreeSet<String> = [path.clone()].into_iter().collect(); crate::lanes::document(&book, &path, &b, &files) }) }).await { Some(Ok(v)) => v, Some(Err(e)) => fail(format!("could not read the file: {e}")), None => fail("cut off") }
        }
        "book_suspend" => {
            if by_agent { return fail("only the person suspends a law"); } let (r, pid, key, on) = (root.clone(), s(&b["pack"]), s(&b["key"]), b["on"].as_bool().unwrap_or(true));
            match blocking(move || -> Result<Value, String> { let mut p = crate::lanes::load_book(&r).0.into_iter().find(|p| p.id == pid).ok_or("no such pack")?; let l = p.laws.iter_mut().find(|l| l.key == key).ok_or("no such law")?; if on { l.status = "suspended".into(); } else { *l = crate::lanes::ratify(l)?; } crate::lanes::save_pack(&r, &p)?; Ok(json!({ "ok": true, "pack": crate::lanes::summary(&p, true) })) }).await { Some(Ok(v)) => v, Some(Err(e)) => fail(e), None => fail("cut off") }
        }
        "file_base" => { let (r, run, path) = (root.clone(), s(&b["run"]), s(&b["path"])); let dir = crate::team_graph_run::run_dir(&r, &run); let _ = &dir; let work = crate::run_edits::work_dir(&r, &run); json!({ "ok": true, "path": path, "text": match work { Some(w) => blocking(move || crate::run_edits::base_of(std::path::Path::new(&w), &path)).await.unwrap_or_default(), None => String::new() } }) }
        "node" => node_view(&root, &s(&b["run"]), &s(&b["node"])),
        // STEERING: a note to the running team, written by the person only
        "run_note" => { if by_agent { return fail("only the person steers a running team"); } let run = s(&b["run"]);
            match blocking({ let (r, t) = (root.clone(), s(&b["text"])); let run = run.clone(); move || add_run_note(&r, &run, &t) }).await { Some(Ok(n)) => json!({ "ok": true, "notes": n }), Some(Err(e)) => fail(e), None => fail("could not write the note") } }
        "control" => { let Some(h) = handle(&s(&b["run"])) else { return fail("that run is not going") }; let c = match s(&b["action"]).as_str() { "pause" => Ctl::Pause, "resume" => Ctl::Resume, "stop" => Ctl::Stop, other => return fail(format!("action is pause, resume or stop (not `{other}`)")) }; match h.ctl.send(c) { Ok(_) => json!({ "ok": true }), Err(_) => fail("the run has ended") } }
        "answer" => {
            // ONLY A PERSON PASSES A HUMAN NODE: a call that comes from an agent's pane is refused, here as well as in the graph
            if by_agent { return fail("only the person approves: an agent can ask for it (request_approval) and cannot give it"); }
            let Some(h) = handle(&s(&b["run"])) else { return fail("that run is not going") }; let (node, approve) = (s(&b["node"]), b["approve"].as_bool().unwrap_or(false));
            match ctl_call(&h, |reply| Ctl::Answer { node, approve, reply }).await { Ok(_) => json!({ "ok": true }), Err(e) => fail(e) }
        }
        "restart" => { let Some(h) = handle(&s(&b["run"])) else { return fail("that run is not going") }; let node = s(&b["node"]); match ctl_call(&h, |reply| Ctl::Restart { node, reply }).await { Ok(_) => json!({ "ok": true }), Err(e) => fail(e) } }
        "request_approval" => { let Some(h) = handle(&s(&b["run"])) else { return fail("that run is not going") }; let node = s(&b["node"]); let _ = h.ctl.send(Ctl::Note { node: node.clone(), kind: "approval_requested".into(), text: format!("the leader asks you to decide at {node}: {}", s(&b["note"])) }); json!({ "ok": true, "note": "the person has been asked; only they can answer" }) }
        "edit" => {
            let (node, patch) = (s(&b["node"]), b["patch"].clone());
            if let Some(h) = handle(&s(&b["run"])) { return match ctl_call(&h, |reply| Ctl::Edit { node, patch, reply }).await { Ok(v) => v, Err(e) => fail(e) }; }
            let mut g = match team_of(&b) { Ok(g) => g, Err(e) => return fail(e) };
            match apply_edit(&mut g, &node, &patch) { Ok(v) => { let _ = save_graph(&root, &g); v } Err(e) => fail(e) }
        }
        "budget" => {
            let moves: Vec<Value> = b["moves"].as_array().cloned().unwrap_or_default(); let ceiling = b["ceiling_usd"].as_f64();
            if let Some(h) = handle(&s(&b["run"])) { return match ctl_call(&h, |reply| Ctl::Alloc { moves, ceiling, by_person: !by_agent, reply }).await { Ok(v) => v, Err(e) => fail(e) }; }
            let mut g = match team_of(&b) { Ok(g) => g, Err(e) => return fail(e) };
            match apply_budget(&mut g, &moves, ceiling, !by_agent) { Ok(v) => { let _ = save_graph(&root, &g); v } Err(e) => fail(e) }
        }
        "graph_versions" => {
            let team = s(&b["team"]);
            match crate::team_patch::versions_list(&root, &team) { Ok(rows) => json!({ "ok": true, "versions": rows }), Err(e) => fail(e) }
        }
        "graph_inspect" => {
            let team = s(&b["team"]);
            let vid = b["version"].as_i64().or_else(|| b["version_id"].as_i64());
            match crate::team_patch::inspect_version(&root, &team, vid) { Ok(v) => v, Err(e) => fail(e) }
        }
        "graph_diff" => {
            let team = s(&b["team"]);
            let g0 = match load_graph(&root, &team) { Ok(g) => g, Err(e) => return fail(e) };
            let ops: Vec<crate::team_patch::PatchOp> = match serde_json::from_value(b["ops"].clone()) { Ok(o) => o, Err(e) => return fail(format!("ops: {e}")) };
            match crate::team_patch::apply_ops(&g0, &ops) {
                Ok(g1) => json!({ "ok": true, "diff": crate::team_patch::diff_nodes(&g0, &g1), "requires_person": crate::team_patch::patch_requires_person(&ops, &g0) }),
                Err(e) => fail(e),
            }
        }
        "graph_propose" => {
            let team = s(&b["team"]);
            let ops: Vec<crate::team_patch::PatchOp> = match serde_json::from_value(b["ops"].clone()) { Ok(o) => o, Err(e) => return fail(format!("ops: {e}")) };
            let author = if by_agent { format!("agent:{}", b["pane_id"].as_i64().unwrap_or(0)) } else { "person".into() };
            match crate::team_patch::propose(&root, &team, &ops, &author, &s(&b["reason"])) {
                Ok(p) => json!({ "ok": true, "proposal_id": p.proposal_id, "requires_person": p.requires_person }),
                Err(e) => fail(e),
            }
        }
        "graph_apply" => {
            let team = s(&b["team"]);
            let ops: Vec<crate::team_patch::PatchOp> = match serde_json::from_value(b["ops"].clone()) { Ok(o) => o, Err(e) => return fail(format!("ops: {e}")) };
            let person_ok = b["person_ok"].as_bool().unwrap_or(!by_agent);
            let applied_by = if by_agent { format!("agent:{}", b["pane_id"].as_i64().unwrap_or(0)) } else { "person".into() };
            let proposal_id = b["proposal_id"].as_i64();
            match crate::team_patch::apply(&root, &team, &ops, person_ok, &applied_by, &s(&b["reason"]), proposal_id, by_agent, &crate::team_patch::CheckpointHooks::default()) {
                Ok(a) => match load_graph(&root, &team) {
                    Ok(g) => json!({ "ok": true, "version_id": a.version_id, "graph_hash": a.graph_hash, "requires_person": a.requires_person, "graph": g.to_json(), "report": crate::team_skills::report_for(&root, &g, false) }),
                    Err(e) => fail(e),
                },
                Err(e) => fail(e),
            }
        }
        "memory_recall" | "memory_write" | "memory_forget" => {
            let which = other_memory_op(&op);
            if which == "forget" && by_agent { return fail("forgetting is the person's: an agent can recall and write, not delete"); }
            let mut body = b.clone(); body["op"] = json!(which);
            match crate::team_memory::node_op(&root, &s(&b["team"]), &body) { Ok(v) => v, Err(e) => fail(e) }
        }
        "db_consent" => {
            if by_agent { return fail("a write to a db is the person's to allow: an agent cannot give the consent"); }
            let (kind, name) = (s(&b["kind"]), s(&b["name"]));
            match crate::team_db::grant_consent(&root, &s(&b["team"]), &kind, &name) { Ok(()) => json!({ "ok": true, "kind": kind, "name": name, "bound_to": s(&b["team"]) }), Err(e) => fail(e) }
        }
        "db_schema" | "db_query" | "db_write" => {
            let (team, kind, name) = (s(&b["team"]), { let k = s(&b["kind"]); if k.is_empty() { "sqlite".to_string() } else { k } }, s(&b["name"]));
            let params: Vec<Value> = b["params"].as_array().cloned().unwrap_or_default(); let sql = s(&b["sql"]);
            let lim = crate::team_db::Limits { max_rows: b["max_rows"].as_u64().unwrap_or(1000).clamp(1, 10_000) as usize, timeout_ms: b["timeout_ms"].as_u64().unwrap_or(5000).clamp(50, 30_000) };
            let (person_ok, dry) = (!by_agent && b["person_ok"].as_bool().unwrap_or(false), b["dry_run"].as_bool().unwrap_or(false)); let (r2, t2, k2, n2, o2) = (root.clone(), team, kind, name, op.clone());
            match blocking(move || -> Result<Value, String> {
                let consent = o2 == "db_write" && crate::team_db::has_consent(&r2, &t2, &k2, &n2);
                let c = crate::team_db::open_source(&r2, &t2, &k2, &n2, o2 == "db_write" && consent)?;
                match o2.as_str() {
                    "db_schema" => crate::team_db::schema(&c).map(|v| json!({ "ok": true, "schema": v })),
                    "db_query" => crate::team_db::query(&c, &sql, &params, lim).map(|t| json!({ "ok": true, "table": t })),
                    _ => crate::team_db::execute(&c, &sql, &params, lim, consent, person_ok, dry).map(|w| json!({ "ok": true, "affected": w.affected, "dry_run": w.dry_run })),
                }
            }).await { Some(Ok(v)) => v, Some(Err(e)) => fail(e), None => fail("the db call was cut off") }
        }
        "saga_read" | "saga_write" | "saga_run" | "saga_sync" => {
            let mut body = b.clone(); body["op"] = json!(op.strip_prefix("saga_").unwrap_or(&op));
            let actor = if by_agent { format!("agent:{}", b["pane_id"].as_i64().unwrap_or(0)) } else { "person".to_string() };
            let r2 = root.clone();
            match blocking(move || crate::team_saga_node::op(&r2, &actor, &body)).await { Some(Ok(v)) => v, Some(Err(e)) => fail(e), None => fail("the saga call was cut off") }
        }
        "improve_config" => {
            let team = s(&b["team"]); let mut c = crate::team_improve::load_config(&root, &team);
            let changing = ["mode", "freeze", "frozen_nodes", "seat", "cooldown_s", "every_n_runs", "budget_usd", "max_auto_per_cycle"].iter().any(|k| b.get(*k).is_some());
            if changing {
                if by_agent { return fail("the improver's settings are the person's: an agent can read them, not change them"); }
                let mut v = serde_json::to_value(&c).unwrap_or(json!({})); for k in ["mode", "freeze", "frozen_nodes", "seat", "cooldown_s", "every_n_runs", "budget_usd", "max_auto_per_cycle"] { if let Some(x) = b.get(k) { v[k] = x.clone(); } }
                c = match serde_json::from_value(v) { Ok(c) => c, Err(e) => return fail(format!("settings: {e}")) };
                if let Err(e) = crate::team_improve::save_config(&root, &team, &c) { return fail(e); }
                // THE SETTINGS ARE VERSIONED (VID-16): the change is a commit in the team's own git, with who and what
                let (r2, t2) = (root.clone(), team.clone()); let what: Vec<String> = ["mode", "freeze", "frozen_nodes", "seat", "cooldown_s", "every_n_runs", "budget_usd", "max_auto_per_cycle"].iter().filter_map(|k| b.get(*k).map(|x| format!("{k}={x}"))).collect();
                std::thread::spawn(move || { let _ = crate::team_repo::commit_settings(&r2, &t2, &format!("settings: improver {}\n\nby: person", what.join(" "))); });
            }
            let st = crate::team_improve::load_state(&root, &team);
            json!({ "ok": true, "config": c, "hit_rate": st.hit_rate(), "state": st })
        }
        "improve_review" => {
            let team = s(&b["team"]);
            let why = match s(&b["reason"]).as_str() { "marked_bad" => crate::team_improve::Reason::MarkedBad, _ => crate::team_improve::Reason::Asked };
            match crate::team_improve::run_review(&root, &team, why).await { Ok(v) => v, Err(e) => fail(e) }
        }
        "improve_proposals" => { let team = s(&b["team"]); let st = b["status"].as_str().map(String::from); match crate::team_improve::list(&root, &team, st.as_deref()) { Ok(v) => json!({ "ok": true, "proposals": v }), Err(e) => fail(e) } }
        "improve_decide" => {
            let team = s(&b["team"]); let id = b["id"].as_i64().unwrap_or(0);
            let decided_by = if by_agent { format!("agent:{}", b["pane_id"].as_i64().unwrap_or(0)) } else { "person".to_string() };
            let person_ok = b["person_ok"].as_bool().unwrap_or(!by_agent);
            match crate::team_improve::decide(&root, &team, id, b["accept"].as_bool().unwrap_or(false), &decided_by, by_agent, person_ok, now()) { Ok(v) => v, Err(e) => fail(e) }
        }
        "bench_stats" => {
            let team = s(&b["team"]); let node = s(&b["node"]);
            json!({ "ok": true, "node": node, "seats": crate::team_bench::stats_json(&crate::team_bench::node_stats(&root, &team, &node)) })
        }
        "bench_trial" | "bench_compare" => {
            // run a labelled set WITH TRAPS through a cell's seat (trial) or through several seats (compare); every call is a ledger row with bench = 1
            let team = s(&b["team"]); let node = s(&b["node"]);
            let g = match load_graph(&root, &team) { Ok(g) => g, Err(e) => return fail(e) };
            let spec = match g.node(&node).map(|n| &n.kind) { Some(Kind::Llm(sp)) => sp.clone(), _ => return fail(format!("{node} is not a model call (`llm`): only a cell can be benchmarked")) };
            let cases = match crate::team_bench::cases_from_json(&b["cases"]) { Ok(c) => c, Err(e) => return fail(e) };
            let ctx = crate::ledger::Ctx { root: root.clone(), team: team.clone(), node: Some(node.clone()), task_class: spec.examples_from.as_ref().map(|e| e.task_class.clone()), caller: "benchmark".into(), bench: true, cfg: g.to_json().get("nodes").and_then(|n| n.get(&node)).map(crate::team_nodebench::cfg_of), ..Default::default() };
            let seats: Vec<String> = if op == "bench_trial" { vec![b["seat"].as_str().map(String::from).unwrap_or_else(|| spec.seat.clone())] } else { b["arms"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
            if op == "bench_compare" && seats.len() < 2 { return fail("a compare needs `arms`: at least two seats"); }
            let mut reports = vec![];
            for seat in &seats { match crate::team_bench::trial_llm(&spec, seat, &cases, ctx.clone()).await { Ok(r) => reports.push(r), Err(e) => return fail(e.join("; ")) } }
            if op == "bench_trial" { json!({ "ok": true, "report": reports[0] }) } else { json!({ "ok": true, "compare": crate::team_bench::decide(reports) }) }
        }
        "graph_swarm" => {
            // the Leader instantiates a swarm: for-each -> llm(preset, ladder) -> check -> join, wired in after `wire_in` and out to `wire_out` (RFC 0011 §10.3)
            let team = s(&b["team"]);
            let params: crate::team_swarm::SwarmParams = match serde_json::from_value(b["params"].clone()) { Ok(p) => p, Err(e) => return fail(format!("params: {e}")) };
            let mut g = match load_graph(&root, &team) { Ok(g) => g, Err(e) => return fail(e) };
            let exp = match crate::team_swarm::expand(&params) { Ok(e) => e, Err(e) => return fail(e.join("; ")) };
            if let Err(e) = crate::team_swarm::apply(&mut g, &exp) { return fail(e); }
            if let Some(from) = b["wire_in"].as_str().filter(|x| !x.is_empty()) { g.edges.push(crate::team_graph::Edge { from: from.into(), port: "out".into(), to: exp.entry.clone() }); }
            if let Some(to) = b["wire_out"].as_str().filter(|x| !x.is_empty()) { g.edges.push(crate::team_graph::Edge { from: exp.entry.clone(), port: "done".into(), to: to.into() }); }
            let rep = crate::team_graph::validate(&g);
            if !rep.ok() { return fail(format!("the swarm does not fit this graph: {}", rep.words().join("; "))); }
            match save_graph(&root, &g) { Ok(_) => json!({ "ok": true, "entry": exp.entry, "nodes": exp.nodes.iter().map(|(i, _)| i.clone()).collect::<Vec<_>>(), "report": crate::team_skills::report_for(&root, &g, false) }), Err(e) => fail(e) }
        }
        "ledger_off" => {
            if by_agent { return fail("the call ledger's off switch is the person's: an agent cannot change it"); }
            let team = s(&b["team"]); let off = b["off"].as_bool().unwrap_or(true);
            match crate::ledger::set_off(&root, &team, off) { Ok(()) => json!({ "ok": true, "team": team, "off": off }), Err(e) => fail(e) }
        }
        "graph_rollback" => {
            let team = s(&b["team"]);
            let applied_by = if by_agent { format!("agent:{}", b["pane_id"].as_i64().unwrap_or(0)) } else { "person".into() };
            let person_ok = b["person_ok"].as_bool().unwrap_or(!by_agent);
            match crate::team_patch::rollback(&root, &team, &applied_by, by_agent, person_ok) {
                Ok(g) => json!({ "ok": true, "graph": g.to_json(), "report": crate::team_skills::report_for(&root, &g, false) }),
                Err(e) => fail(e),
            }
        }
        other => fail(format!("op is one of: catalog, list, get, validate, simulate, save, create, template, delete, run, status, runs, node, control, answer, restart, request_approval, edit, budget, graph_versions, graph_inspect, graph_diff, graph_propose, graph_apply, graph_rollback, ledger_off, graph_swarm, db_consent, db_schema, db_query, db_write, saga_read, saga_write, saga_run, saga_sync, improve_config, improve_review, improve_proposals, improve_decide, bench_stats, bench_trial, bench_compare, memory_recall, memory_write, memory_forget (not `{other}`)")),
    }
}

// ── tests ────────────────────────────────────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #[test]
    fn an_artifact_takes_the_file_its_step_wrote_under_any_name_before_it_falls_back_to_the_report() {
        let d = std::env::temp_dir().join(format!("art-written-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        assert!(written_file(&d, "md").is_none(), "nothing written: nothing found");
        std::fs::write(d.join("promo-script.md"), "# Roteiro\nCena 1").unwrap();
        let (p, n) = written_file(&d, "md").unwrap(); assert_eq!(n, "promo-script.md"); assert!(std::fs::read_to_string(p).unwrap().contains("Cena 1"));
        assert!(written_file(&d, "html").is_none(), "a file of another format is not the one wanted");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_tests_a_team_passed_are_kept_in_the_tree_without_overwriting_anything() {
        let tmp = std::env::temp_dir().join(format!("antiloki-keeptests-{}", std::process::id())); let _ = std::fs::remove_dir_all(&tmp); std::fs::create_dir_all(&tmp).unwrap(); std::fs::write(tmp.join("a.test.js"), "mine").unwrap();
        let files: std::collections::BTreeMap<String, String> = [("a.test.js".to_string(), "theirs".to_string()), ("test/b.test.js".to_string(), "ok".to_string()), ("../escape.js".to_string(), "x".to_string())].into();
        let kept = keep_tests(&tmp, &files); assert_eq!(kept, vec!["test/b.test.js".to_string()], "an existing file stays, a path that climbs out is refused"); assert_eq!(std::fs::read_to_string(tmp.join("a.test.js")).unwrap(), "mine"); assert_eq!(std::fs::read_to_string(tmp.join("test/b.test.js")).unwrap(), "ok"); assert!(!tmp.parent().unwrap().join("escape.js").exists()); let _ = std::fs::remove_dir_all(&tmp);
    }
    #[test]
    fn a_steps_net_lines_are_the_difference_of_its_files_line_counts() {
        let b: std::collections::HashMap<String, usize> = [("a.rs".to_string(), 10), ("b.rs".to_string(), 5)].into(); let a: std::collections::HashMap<String, usize> = [("a.rs".to_string(), 14), ("b.rs".to_string(), 2), ("c.rs".to_string(), 7)].into();
        assert_eq!(net_lines(&b, &a, &["a.rs".into(), "b.rs".into(), "c.rs".into()]), (11, 3)); assert_eq!(net_lines(&b, &a, &[]), (0, 0));
        let tmp = std::env::temp_dir().join(format!("antiloki-lines-{}", std::process::id())); let _ = std::fs::remove_dir_all(&tmp); std::fs::create_dir_all(&tmp).unwrap(); std::fs::write(tmp.join("x.txt"), "a\nb\nc").unwrap(); std::fs::write(tmp.join("bin.dat"), [0u8, 1, 2]).unwrap();
        let marks: std::collections::HashMap<String, (u64, u128)> = [("x.txt".to_string(), (5, 0)), ("bin.dat".to_string(), (3, 0))].into(); let lc = line_counts(&tmp, &marks); assert_eq!(lc.get("x.txt"), Some(&3)); assert!(!lc.contains_key("bin.dat")); let _ = std::fs::remove_dir_all(&tmp);
    }
    #[test]
    fn autofix_tells_the_leader_to_fix_and_run_again_up_to_three_failures_in_a_row_and_is_on_by_default() {
        let tmp = std::env::temp_dir().join(format!("antiloki-autofix-{}", std::process::id())); let _ = std::fs::remove_dir_all(&tmp); let root = tmp.to_string_lossy().to_string();
        assert!(autofix_on(&root, "Video editing"), "on by default"); set_autofix(&root, "Video editing", false); assert!(!autofix_on(&root, "Video editing")); set_autofix(&root, "Video editing", true); assert!(autofix_on(&root, "Video editing")); let _ = std::fs::remove_dir_all(&tmp);
        let on = autofix_tail(true, 1, "make the promo"); assert!(on.contains("AUTOFIX IS ON") && on.contains("antiloki_team_run") && on.contains("make the promo") && on.contains("failure 1 of at most 3"));
        assert!(autofix_tail(true, 4, "x").contains("do NOT run it again"), "after the third failure in a row it stops and tells the person"); assert!(autofix_tail(true, 3, "x").contains("run it again"));
        assert!(autofix_tail(false, 1, "x").contains("AUTOFIX IS OFF") && !autofix_tail(false, 1, "x").contains("antiloki_team_run"));
    }
    #[test]
    fn a_terminal_of_another_thread_is_never_bound_as_a_teams_leader() {
        let view = json!({ "threads": [{ "id": "main", "team": null, "panes": [{ "paneId": 7 }] }, { "id": "video-editing", "team": "video-editing", "panes": [{ "paneId": 9 }, { "paneId": 11 }] }, { "id": "other", "team": "other", "panes": [{ "paneId": 13 }] }] });
        assert_eq!(other_thread_of(&view, "Video editing", 7).as_deref(), Some("main")); assert_eq!(other_thread_of(&view, "Video editing", 13).as_deref(), Some("other"));
        assert_eq!(other_thread_of(&view, "Video editing", 9), None, "its own thread's terminal"); assert_eq!(other_thread_of(&view, "Video editing--t-try", 9), None, "a fork's Leader stands in its team's thread"); assert_eq!(other_thread_of(&view, "Video editing--t-try", 7).as_deref(), Some("main")); assert_eq!(other_thread_of(&view, "Video editing", 99), None, "a terminal in no thread yet (just made) is allowed");
    }
    use std::collections::HashMap;
    #[test]
    fn a_step_is_credited_only_with_the_files_it_changed_and_never_with_tool_caches() {
        let dir = std::env::temp_dir().join(format!("antiloki-stepfiles-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let g = |a: &[&str]| { let o = std::process::Command::new("git").args(a).current_dir(&dir).output().unwrap(); String::from_utf8_lossy(&o.stdout).to_string() };
        g(&["init", "-q", "-b", "main"]); g(&["config", "user.email", "t@t"]); g(&["config", "user.name", "t"]); std::fs::write(dir.join("a.txt"), "one\n").unwrap(); g(&["add", "-A"]); g(&["commit", "-q", "-m", "init"]);
        // the builder's work, left uncommitted, and a tool cache
        std::fs::create_dir_all(dir.join("video/.cache")).unwrap(); std::fs::write(dir.join("video/index.html"), "<p>x</p>").unwrap(); std::fs::write(dir.join("video/.cache/c.json"), "{}").unwrap(); std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        let before = file_marks(&dir); assert!(step_files(&dir, &HashMap::new()).contains(&"video/index.html".to_string()) && !step_files(&dir, &HashMap::new()).iter().any(|f| f.contains(".cache")), "caches are not work");
        assert!(step_files(&dir, &before).is_empty(), "a step that changed nothing is credited with nothing: {:?}", step_files(&dir, &before));
        std::thread::sleep(std::time::Duration::from_millis(20)); std::fs::write(dir.join("a.txt"), "three more bytes\n").unwrap(); std::fs::write(dir.join("new.md"), "n").unwrap();
        assert_eq!(step_files(&dir, &before), vec!["a.txt".to_string(), "new.md".to_string()], "an earlier file the step rewrote counts, and a new one"); assert!(is_tool_cache("a/.hyperframes/x.json") && is_tool_cache("node_modules/x") && !is_tool_cache(".github/ci.yml") && !is_tool_cache("src/a.ts")); let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_step_puts_back_only_what_it_changed_never_the_work_of_the_steps_before_it() {
        let dir = std::env::temp_dir().join(format!("antiloki-putback-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let g = |a: &[&str]| { let o = std::process::Command::new("git").args(a).current_dir(&dir).output().unwrap(); String::from_utf8_lossy(&o.stdout).to_string() };
        g(&["init", "-q", "-b", "main"]); g(&["config", "user.email", "t@t"]); g(&["config", "user.name", "t"]); std::fs::write(dir.join("a.txt"), "one\n").unwrap(); g(&["add", "-A"]); g(&["commit", "-q", "-m", "init"]);
        // the builder's work, not committed: a video and an edit
        std::fs::create_dir_all(dir.join("video/out")).unwrap(); std::fs::write(dir.join("video/out/test.mp4"), "mp4").unwrap(); std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        let before: std::collections::HashSet<String> = changed_names(&dir).into_iter().collect(); assert!(before.contains("video/out/test.mp4") && before.contains("a.txt"), "{before:?}");
        // the next step owns nothing and writes a stray file
        std::fs::write(dir.join("stray.txt"), "x").unwrap();
        let arch = crate::team_archive::Ctx { root: dir.join("ws").to_string_lossy().to_string(), run: "r".into(), team: "t".into(), node: "n".into() }; let back = put_back(&dir, &[], false, &before, &arch); assert_eq!(back, vec!["stray.txt".to_string()], "only what this step changed: {back:?}");
        assert!(dir.join("video/out/test.mp4").exists() && std::fs::read_to_string(dir.join("a.txt")).unwrap() == "two\n" && !dir.join("stray.txt").exists()); let saved = crate::team_archive::list(&arch.root, "r"); assert!(saved.iter().any(|f| f["path"] == serde_json::json!("stray.txt")), "what was put back is in the archive: {saved:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
    use super::*;
    fn g() -> Graph { template("feature", "Checkout pack", 6.0).unwrap() }

    #[test]
    fn a_unit_works_in_a_tree_of_its_own_run() { assert_eq!(tree_key("reports-529562", "run"), "529562-run"); assert_ne!(tree_key("reports-529562", "s1"), tree_key("reports-529623", "s1"), "two runs never share a tree"); assert_eq!(tree_key("plain", "s1"), "plain-s1"); }
    #[test]
    fn the_planners_pack_is_read_ordered_and_refused_when_it_cannot_be_judged() {
        let ok = parse_pack("Here you go:\n```json\n{\"summary\":\"x\",\"sagas\":[{\"id\":\"b\",\"title\":\"B\",\"needs\":[\"a\"],\"criteria\":[{\"id\":\"c1\",\"text\":\"t\"}]},{\"id\":\"a\",\"title\":\"A\",\"criteria\":[\"it works\"]}]}\n```").unwrap();
        assert_eq!(ok["sagas"][0]["id"], json!("a"), "a saga comes after the ones it needs"); assert_eq!(ok["criteria"].as_array().unwrap().len(), 2); assert_eq!(ok["sagas"][1]["needs"], json!(["a"]));
        assert!(parse_pack("not json").unwrap_err().contains("not JSON")); assert!(parse_pack("{\"sagas\":[]}").unwrap_err().contains("no sagas"));
        assert!(parse_pack("{\"sagas\":[{\"id\":\"a\",\"criteria\":[]}]}").unwrap_err().contains("no acceptance criteria"));
        assert!(parse_pack("{\"sagas\":[{\"id\":\"a\",\"needs\":[\"z\"],\"criteria\":[\"t\"]}]}").unwrap_err().contains("not in the plan"));
        assert!(parse_pack("{\"sagas\":[{\"id\":\"a\",\"needs\":[\"b\"],\"criteria\":[\"t\"]},{\"id\":\"b\",\"needs\":[\"a\"],\"criteria\":[\"t\"]}]}").unwrap_err().contains("in a circle"));
        assert!(parse_pack("{\"sagas\":[{\"id\":\"a\",\"criteria\":[\"t\"]},{\"id\":\"a\",\"criteria\":[\"t\"]}]}").unwrap_err().contains("share the id"));
    }

    #[test]
    fn a_qa_verdict_must_answer_every_criterion_and_an_audit_verdict_must_say_what_it_found() {
        let crit = criteria_of(&json!([{ "id": "c1", "text": "a" }, { "id": "c2", "text": "b" }]));
        let v = parse_qa("{\"results\":[{\"id\":\"c1\",\"ok\":true},{\"id\":\"c2\",\"ok\":false,\"evidence\":\"it does not say world\"}],\"summary\":\"one failed\"}", &crit).unwrap();
        assert_eq!(v["pass"], json!(false)); assert_eq!(v["failed"], json!(["c2"])); assert_eq!(v["evidence"][1]["evidence"], json!("it does not say world"));
        assert!(parse_qa("{\"results\":[{\"id\":\"c1\",\"ok\":true}]}", &crit).unwrap_err().contains("does not answer criterion `c2`"));
        assert!(parse_qa("{\"results\":[{\"id\":\"c1\",\"ok\":true},{\"id\":\"c2\",\"ok\":false}]}", &crit).unwrap_err().contains("no evidence"));
        assert_eq!(parse_audit("{\"verdict\":\"PASS\"}").unwrap()["verdict"], json!("pass")); assert_eq!(parse_audit("{\"verdict\":\"flag\",\"flags\":[\"touches package.json\"]}").unwrap()["flags"], json!(["touches package.json"]));
        assert!(parse_audit("{\"verdict\":\"block\"}").unwrap_err().contains("says what it found")); assert!(parse_audit("{\"verdict\":\"maybe\"}").is_err());
    }

    #[test]
    fn the_unit_is_the_saga_of_the_for_each_or_the_ask() {
        let g = g(); let mut vals = Map::new();
        let u = unit_of(&g, &vals, "build it"); assert_eq!((u.id.as_str(), u.objective.as_str()), ("run", "build it"));
        vals.insert("each".into(), json!({ "item": { "id": "s2", "title": "Receipt", "objective": "email it", "criteria": [{ "id": "c1", "text": "sent" }] }, "index": 1, "count": 3 }));
        let u = unit_of(&g, &vals, "build it"); assert_eq!((u.id.as_str(), u.title.as_str(), u.criteria.len()), ("s2", "Receipt", 1));
    }

    #[test]
    fn a_role_is_told_only_what_it_needs_and_a_retry_gets_only_what_failed() {
        let u = Unit { id: "s1".into(), title: "Cart".into(), objective: "show the cart".into(), criteria: criteria_of(&json!([{ "id": "c1", "text": "it shows" }])) };
        let w = role_prompt(RoleKind::Worker, "[tg]", "ask", &u, &["src/**".into()], &Delta::default()); assert!(w.contains("[tg]") && w.contains("show the cart") && w.contains("c1: it shows") && w.contains("You own only these files: src/**") && !w.contains("QA rejected"));
        let mut vals = Map::new(); vals.insert("qa".into(), json!({ "pass": false, "evidence": [{ "id": "c1", "ok": false, "evidence": "the cart is empty" }] })); vals.insert("tests".into(), json!({ "exit": 1, "failed": ["cart renders"], "tail": "1 failed" }));
        let d = delta_of(&vals); assert_eq!((d.qa_failed.clone(), d.tests_failed.clone()), (vec!["c1".to_string()], vec!["cart renders".to_string()]));
        let f = role_prompt(RoleKind::Debugger, "[tg]", "ask", &u, &[], &d); assert!(f.contains("the cart is empty") && f.contains("cart renders") && f.contains("1 failed"));
        assert!(role_prompt(RoleKind::Qa, "[tg]", "ask", &u, &[], &Delta::default()).contains("Do NOT edit any file")); assert!(role_prompt(RoleKind::Planner, "[tg]", "ask", &u, &[], &Delta::default()).contains("Do not edit any file"));
    }

    #[test]
    fn the_team_sagas_episodes_are_the_passes_of_the_outermost_loop() {
        assert_eq!(main_pass_node(&g()), Some("loop".into())); assert_eq!(main_pass_node(&template("bugfix", "b", 2.0).unwrap()), Some("loop".into()));
        assert_eq!(main_pass_node(&template("review", "r", 2.0).unwrap()), None, "no loop, one episode");
        let mut nested = g(); nested.nodes.insert("inner".into(), Node::new(Kind::Loop { max_iterations: 2, until: None }).cap(1.0)); nested.edges.retain(|e| !(e.from == "loop" && e.port == "body")); nested.edges.push(Edge { from: "loop".into(), port: "body".into(), to: "inner".into() }); nested.edges.push(Edge { from: "inner".into(), port: "body".into(), to: "work".into() });
        assert_eq!(main_pass_node(&nested), Some("loop".into()), "an inner loop is not the main one");
    }

    #[test]
    fn a_node_inside_a_fork_works_in_its_branch_and_one_outside_does_not() {
        let r = template("review", "r", 3.0).unwrap(); assert_eq!(branch_label(&r, "qa_a").as_deref(), Some("fork-qa_a")); assert_eq!(branch_label(&r, "audit").as_deref(), Some("fork-audit")); assert_eq!(branch_label(&r, "join"), None); assert_eq!(branch_label(&r, "me"), None, "after the join the work is the saga's again");
        assert_eq!(branch_label(&g(), "work"), None);
    }

    #[test]
    fn a_skill_runs_a_command_antiloki_chose_and_needs_the_effects_it_has() {
        let d = std::env::temp_dir().join(format!("tg-skill-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        assert_eq!(skill_command("run-tests", &d), None); std::fs::write(d.join("package.json"), "{\"scripts\":{\"test\":\"x\",\"build\":\"y\"}}").unwrap();
        assert_eq!(skill_command("run-tests", &d).as_deref(), Some("npm test --silent")); assert_eq!(skill_command("run-build", &d).as_deref(), Some("npm run build --silent")); assert_eq!(skill_command("lint", &d), None); assert_eq!(skill_command("rm-rf", &d), None);
        assert_eq!(skill_effects("run-tests"), Some(&["read", "exec"][..])); assert_eq!(skill_effects("nope"), None);
        assert_eq!(mcp_effects("antiloki_panes"), Some(&["read"][..])); assert_eq!(mcp_effects("antiloki_pane_act"), Some(&["read", "write"][..])); assert_eq!(mcp_effects("antiloki_delegate"), None, "a tool node cannot start more agents");
        assert_eq!(failed_tests("PASS a\nFAIL cart renders\n✗ total is wrong\ntest it::works ... FAILED\nok b"), vec!["cart renders".to_string(), "total is wrong".into(), "it::works".into()]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_team_has_one_saga_and_its_passes_are_episodes_that_point_at_the_pass_they_retry() {
        let c = rusqlite::Connection::open_in_memory().unwrap(); crate::work::init_for_tests(&c).unwrap();
        let team = ensure_team_saga(&c, "Checkout pack", "Checkout pack").unwrap(); assert_eq!(ensure_team_saga(&c, "checkout pack", "x").unwrap(), team, "one saga per team, found again");
        let e1 = open_episode(&c, team, "Pass 1", "tg:r:pass:1", &json!({ "run": "r", "pass": 1 })).unwrap(); close_episode(&c, e1, "failed", 0.2, 5000).unwrap();
        let e2 = open_episode(&c, team, "Pass 2", "tg:r:pass:2", &json!({ "run": "r", "pass": 2, "retry_of": e1 })).unwrap(); assert_eq!(open_episode(&c, team, "Pass 2", "tg:r:pass:2", &json!({})).unwrap(), e2, "the same pass is never made twice");
        trace_event(&c, team, Some(e2), "r", 1, &TraceEv { t_ms: 1, node: "tests".into(), kind: "done".into(), text: "tests finished".into() }).unwrap(); trace_event(&c, team, Some(e2), "r", 1, &TraceEv { t_ms: 1, node: "tests".into(), kind: "done".into(), text: "tests finished".into() }).unwrap();
        let d = crate::work::saga_detail(&c, team).unwrap(); assert_eq!(d["episodes"].as_array().unwrap().len(), 2); assert_eq!(d["episodes"][1]["meta"]["retry_of"], json!(e1)); assert_eq!(d["episodes"][0]["status"], json!("failed"));
        assert_eq!(d["events"].as_array().unwrap().iter().filter(|e| e["text"] == "tests finished").count(), 1, "replaying a run adds nothing");
        assert_eq!(d["saga"]["meta"]["kind"], json!("team"));
        let pack = ensure_pack_saga(&c, "Checkout pack", "r", &json!({ "id": "s1", "title": "Cart", "objective": "show it", "needs": [], "criteria": [] })).unwrap(); let pe = open_episode(&c, pack, "Pass 1", "tg:r:p:1", &json!({ "produced_by": e1 })).unwrap();
        assert_eq!(crate::work::episode_detail(&c, pe).unwrap()["episode"]["meta"]["produced_by"], json!(e1));
    }

    #[test]
    fn a_graph_is_kept_as_a_file_the_leader_and_the_editor_both_write() {
        let d = std::env::temp_dir().join(format!("tg-files-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); let root = d.to_string_lossy().to_string();
        let g = g(); save_graph(&root, &g).unwrap(); assert_eq!(load_graph(&root, "checkout-pack").unwrap(), g); assert_eq!(load_graph(&root, "Checkout pack").unwrap(), g, "found by name or by id");
        assert_eq!(list_graphs(&root).len(), 1); assert!(load_graph(&root, "nope").is_err()); assert!(delete_graph(&root, "checkout-pack") && list_graphs(&root).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_leaders_brief_screens_first_designs_with_agents_writes_skills_and_waits_for_the_person() {
        let b = leader_brief("Marketing", "a marketing team for our shop", 5.0); assert!(b.contains("2a. DOCS") && b.contains("`docs`"), "the brief teaches the docs tool");
        assert!(b.contains("a marketing team for our shop") && b.contains("SCREEN FIRST") && b.contains("antiloki_team_design") && b.contains("antiloki_team_skill") && b.contains("antiloki_team_simulate") && b.contains("$5.00"));
        assert!(b.contains("Do not start it") && b.contains("You cannot approve anything"), "the Leader proposes and the person approves");
        assert!(b.contains("at most four short questions") && b.contains("IS A TEAM WORTH IT") && b.contains("SUGGEST") && b.contains("PAIR") && b.contains("never force") && b.contains("IF A RULE CAN BE WRITTEN, WRITE A CHECK") && b.contains("FRONTIER MODEL OF THIS TEAM") && b.contains("`llm` NODE") && b.contains("MANAGER OF THE SEATS") && b.contains("antiloki_team_seats") && b.contains("THE TEAM IMPROVES ITSELF") && b.contains("THE WORLD IS THE PERSON") && b.contains("antiloki_team_audit"), "a few questions, and a raw agent or a pair is suggested when a team is not needed, never forced");
    }
    #[test]
    fn a_custom_agents_prompt_carries_its_brief_skills_tools_scope_and_the_json_it_answers_in_and_its_reply_is_read_leniently() {
        let a = AgentSpec { specialist: String::new(), title: "the Copywriter".into(), responsibility: "write the posts".into(), brief: "plain and warm".into(), context: vec!["brand.md".into()], skills: vec!["voice".into()], mcp: vec!["antiloki_panes".into()], effects: vec!["read".into(), "write".into()] };
        let unit = Unit { id: "run".into(), title: "Marketing".into(), objective: "the launch".into(), criteria: vec![] };
        let p = agent_prompt("[team-graph r · copy · 1]", "launch the shop", &unit, &a, &["content/**".to_string()], "SKILL voice (how we sound):\nbe plain", &Delta::default(), "FROM research:\nsingle origin is trending");
        for want in ["[team-graph r · copy · 1]", "the Copywriter", "write the posts", "plain and warm", "brand.md", "antiloki_panes", "content/**", "SKILL voice", "\"done\"", "WHAT THE NODES BEFORE YOU FOUND", "single origin is trending"] { assert!(p.contains(want), "{want}: {p}"); }
        let v = parse_agent("here you go\n{\"done\": true, \"report\": \"3 posts\", \"files\": [\"x\"]}", vec!["content/a.md".into()]); assert_eq!(v["done"], json!(true)); assert_eq!(v["report"], json!("3 posts")); assert_eq!(v["pass"], json!(true), "pass follows done when the agent does not judge"); let j = parse_agent("{\"done\": true, \"pass\": false, \"report\": \"fix the tone\"}", vec![]); assert_eq!(j["pass"], json!(false), "a judge can say no"); assert_eq!(v["files"], json!(["content/a.md"]), "the files are the ones it really changed");
        let v = parse_agent("I could not do it", vec![]); assert_eq!(v["done"], json!(true)); assert_eq!(v["report"], json!("I could not do it"));
        assert_eq!(parse_agent("  ", vec![])["done"], json!(false), "no words is not done");
    }

    #[test]
    fn an_agents_documents_are_kept_with_the_run_outside_the_code_and_only_plain_names_are_read() {
        let root = std::env::temp_dir().join(format!("tg-art-{}", std::process::id())); let _ = std::fs::remove_dir_all(&root); let boxd = root.join("box"); std::fs::create_dir_all(boxd.join(".antiloki/artifacts")).unwrap();
        std::fs::write(boxd.join(".antiloki/artifacts/report.md"), "# Report\nall good").unwrap(); std::fs::write(boxd.join(".antiloki/artifacts/dash.html"), "<h1>dash</h1>").unwrap(); std::fs::write(boxd.join(".antiloki/artifacts/run.sh"), "rm -rf /").unwrap(); std::fs::write(boxd.join(".antiloki/artifacts/.hidden.md"), "x").unwrap();
        let r = root.to_string_lossy().to_string(); let to = run_dir(&r, "run1").join("artifacts").join("analyst");
        assert_eq!(collect_artifacts(&boxd, &to, &ArtSnap::new()), vec!["dash.html", "report.md"], "a script is no document and a hidden file is not kept");
        let list = artifacts_of(&r, "run1"); assert_eq!(list.len(), 2); assert!(list.iter().all(|a| a["kind"] == json!("draft") && a["node"] == json!("analyst")));
        let t = artifact_text(&r, "run1", Some("analyst"), "report.md").unwrap(); assert_eq!(t["text"], json!("# Report\nall good")); assert_eq!(t["format"], json!("md"));
        for bad in ["../x", "a/b.md", ".hidden.md", "", "x..md"] { assert!(artifact_text(&r, "run1", Some("analyst"), bad).is_err(), "{bad}"); }
        assert!(artifact_text(&r, "run1", Some("../.."), "report.md").is_err(), "a node name cannot leave the folder either"); assert!(artifact_text(&r, "run1", None, "report.md").is_err(), "that one was never made, only drafted");
        assert!(artifact_ext("a.MD") == Some("md") && artifact_ext("a.exe").is_none() && artifact_ext("noext").is_none() && plain_name("launch-plan_2.md") && !plain_name("a b.md"));
        // a file older than the call is another agent's
        assert!(collect_artifacts(&boxd, &root.join("other"), &snapshot_artifacts(&boxd)).is_empty(), "nothing changed since the snapshot");
        std::fs::write(boxd.join(".antiloki/artifacts/report.md"), "# Report\nchanged").unwrap(); assert_eq!(collect_artifacts(&boxd, &root.join("other2"), &snapshot_artifacts(&boxd).into_iter().map(|(k, v)| (k, (v.0, 0))).collect()), vec!["dash.html", "report.md"].into_iter().map(String::from).collect::<Vec<_>>(), "a changed size reads as changed");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_seat_out_of_quota_is_told_apart_from_the_teams_budget_and_says_when_it_comes_back() {
        assert_eq!(reset_words(167 * 3_600_000 + 21 * 60_000 + 26_000), "6 days 23 h"); assert_eq!(reset_words(2 * 3_600_000 + 5 * 60_000), "2 h 5 min"); assert_eq!(reset_words(38 * 60_000 + 26_000), "39 min"); assert_eq!(reset_words(-5), "0 min");
        let why = "Individual quota reached. Please upgrade your subscription to increase your limits. Resets in 167h21m26s.";
        let t = limit_failure("cursor", why, 0.022, Some(0.9));
        assert!(t.contains("the cursor seat is out of quota") && t.contains("comes back in 6 days 23 h"), "{t}");
        assert!(t.contains("not this team's budget") && t.contains("$0.02 of its $0.90 cap"), "the budget is said to be fine: {t}");
        assert!(t.contains("Settings > Seats") && t.contains("Individual quota reached"), "{t}");
        // no reset time in the message, no cap on the node: still a clear sentence
        let t2 = limit_failure("codex", "You've reached your usage limit.", 0.0, None); assert!(t2.contains("out of quota.") && !t2.contains("comes back"), "{t2}");
        assert!(crate::cli::is_limit_error(why), "the message of the real run is recognised as a quota message");
    }

    #[test]
    fn the_persons_notes_are_kept_per_run_capped_and_read_to_the_agents_as_the_persons_own_words() {
        let d = std::env::temp_dir().join(format!("antiloki-notes-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); let root = d.to_string_lossy().to_string();
        assert!(add_run_note(&root, "r-1", "use the blue theme").is_err(), "a run that does not exist takes no note"); std::fs::create_dir_all(run_dir(&root, "r-1")).unwrap();
        assert_eq!(add_run_note(&root, "r-1", "  use the blue theme  ").unwrap(), 1); assert_eq!(add_run_note(&root, "r-1", "and keep it short").unwrap(), 2); assert!(add_run_note(&root, "r-1", "   ").is_err());
        assert_eq!(run_notes(&root, "r-1"), vec!["use the blue theme".to_string(), "and keep it short".to_string()]); assert!(run_notes(&root, "r-2").is_empty());
        let b = notes_block(&run_notes(&root, "r-1")); assert!(b.contains("THE PERSON ADDED THESE NOTES") && b.contains("- use the blue theme") && b.contains("- and keep it short"), "{b}");
        for k in 0..30 { let _ = add_run_note(&root, "r-1", &format!("n{k}")); } assert_eq!(run_notes(&root, "r-1").len(), 20, "capped"); assert_eq!(add_run_note(&root, "r-1", &"x".repeat(900)).is_err(), true); let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_tools_an_agent_starts_are_kept_per_node_newest_first_and_the_file_stays_small() {
        let d = std::env::temp_dir().join(format!("antiloki-live-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); let root = d.to_string_lossy().to_string(); std::fs::create_dir_all(run_dir(&root, "r-1")).unwrap();
        live_push(&root, "r-1", "work", "Read", "src/a.rs"); live_push(&root, "r-1", "work", "Grep", "fn main"); live_push(&root, "r-1", "qa", "Bash", "cargo test"); live_push(&root, "nope", "work", "Read", "x");
        let v = live_of(&std::fs::read_to_string(run_dir(&root, "r-1").join("live.jsonl")).unwrap(), 6); assert_eq!(v["work"].as_array().unwrap().len(), 2); assert_eq!(v["work"][0]["tool"], json!("Grep"), "newest first"); assert_eq!(v["qa"][0]["target"], json!("cargo test")); assert!(!run_dir(&root, "nope").exists(), "a run that does not exist gets no file");
        assert_eq!(live_of(&std::fs::read_to_string(run_dir(&root, "r-1").join("live.jsonl")).unwrap(), 1)["work"].as_array().unwrap().len(), 1);
        for k in 0..450 { live_push(&root, "r-1", "work", "Read", &format!("f{k}")); } let lines = std::fs::read_to_string(run_dir(&root, "r-1").join("live.jsonl")).unwrap().lines().count(); assert!(lines <= 400 && lines >= 200, "{lines}"); let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn a_streamed_line_names_the_tools_it_starts() {
        let l = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"},{"type":"tool_use","name":"Read","input":{"file_path":"src/a.rs"}},{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#;
        assert_eq!(crate::cli::live_tools_of_line(l), vec![("Read".to_string(), "src/a.rs".to_string()), ("Bash".to_string(), "cargo test".to_string())]);
        for none in ["", "plain text", r#"{"type":"user","message":{"content":[]}}"#, r#"{"type":"assistant","message":{"content":[{"type":"text","text":"tool_use"}]}}"#, "{broken tool_use"] { assert!(crate::cli::live_tools_of_line(none).is_empty(), "{none}"); }
    }
    #[test] fn a_run_keeps_a_checkpoint_each_time_a_node_finishes_and_a_rewind_refuses_what_it_cannot_take_up() {
        let root = std::env::temp_dir().join(format!("antiloki-cp-{}", std::process::id())); let _ = std::fs::remove_dir_all(&root); let root = root.to_string_lossy().to_string();
        let st = |vals: serde_json::Value, status: &str| json!({ "id": "r1", "state": { "status": status, "values": vals, "spent_usd": 0.5, "now_ms": 9 } });
        keep_checkpoint(&root, "r1", 0, &st(json!({}), "running")); assert!(checkpoints(&root, "r1").is_empty(), "0 nodes done is not a checkpoint");
        keep_checkpoint(&root, "r1", 1, &st(json!({ "plan": {} }), "running")); keep_checkpoint(&root, "r1", 2, &st(json!({ "plan": {}, "build": {} }), "done")); keep_checkpoint(&root, "r1", 2, &st(json!({ "plan": {}, "build": {}, "x": 1 }), "done"));
        let c = checkpoints(&root, "r1"); assert_eq!(c.len(), 2); assert_eq!(c[0]["nodes"], json!(["plan"])); assert_eq!(c[1]["nodes"], json!(["build"]), "a checkpoint is kept once");
        assert!(rewind(&root, "r1", 7).unwrap_err().contains("no checkpoint 7")); assert!(rewind(&root, "../x", 1).is_err());
    }

    #[test]
    fn the_antiloki_tools_an_agent_lists_become_the_tools_the_cli_is_allowed_to_call() {
        let a = crate::team_graph::AgentSpec { mcp: vec!["antiloki_app_tools".into(), "antiloki_delegate".into(), "evil;rm".into(), "other_tool".into(), "antiloki_team_run".into()], ..Default::default() };
        assert_eq!(antiloki_mcp_tools(Some(&a)), vec!["mcp__antiloki__antiloki_app_tools"], "only antiloki's own, never the ones that start agents or steer teams"); assert!(antiloki_mcp_tools(None).is_empty());
    }
    #[test]
    fn a_parent_run_says_it_waits_for_the_person_inside_its_child() {
        let mut runs = vec![json!({ "id": "p1", "team": "outer", "needs_you": false }), json!({ "id": "c1", "team": "inner", "needs_you": true, "parent": { "run": "p1", "team": "outer", "node": "sub" } }), json!({ "id": "c2", "team": "inner", "needs_you": false, "parent": { "run": "p1", "team": "outer", "node": "sub2" } }), json!({ "id": "x", "team": "other", "needs_you": false })];
        mark_waiting_parents(&mut runs); assert_eq!(runs[0]["waiting_on_child"], json!({ "run": "c1", "team": "inner" })); assert!(runs[0]["needs_you"] == json!(false), "counted once, at the child's gate"); assert!(runs[3].get("waiting_on_child").is_none() && runs[1].get("waiting_on_child").is_none());
    }
    #[test]
    fn an_agent_after_a_join_is_told_what_the_join_waited_for() {
        let agent = |t: &str| json!({ "node": "role", "role": "agent", "owns": [], "agent": { "title": t, "responsibility": "r", "brief": "b" } });
        let g = Graph::from_json(&json!({ "team": "t", "schema": 1, "budget_usd": 1.0, "start": ["page"], "nodes": {
            "page": { "node": "use_tool", "tool": { "type": "web", "name": "fetch", "args": { "url": "{ask}" } }, "allow": { "paths": [], "panes": [], "skills": [], "mcp": [], "effects": ["network"] }, "starts_model": false, "outputs": {} },
            "copy": agent("Copywriter"), "both": { "node": "join", "policy": "all" }, "check": agent("Checker") },
            "edges": [{ "from": "page", "port": "out", "to": "copy" }, { "from": "copy", "port": "out", "to": "both" }, { "from": "page", "port": "out", "to": "both" }, { "from": "both", "port": "out", "to": "check" }] })).unwrap();
        let mut v = Map::new(); v.insert("page".into(), json!({ "text": "THE PAGE SAYS HELLO" })); v.insert("copy".into(), json!({ "report": "THE COPY CLAIMS X" }));
        let seen = inputs_for(&g, &v, "check"); assert!(seen.contains("THE PAGE SAYS HELLO") && seen.contains("THE COPY CLAIMS X"), "{seen}");
        let direct = inputs_for(&g, &v, "copy"); assert!(direct.contains("THE PAGE SAYS HELLO") && !direct.contains("THE COPY CLAIMS X"), "a direct edge is read as before: {direct}");
    }
    #[test] fn a_specialist_node_gets_the_micro_mcp_env_and_only_an_approved_specialist_is_used() {
        let run = "r1"; let root = std::env::temp_dir().join(format!("antiloki-specnode-{}", std::process::id())); std::fs::create_dir_all(&root).unwrap(); let r = root.to_string_lossy().to_string();
        let spec = ("copy-chief".to_string(), vec!["antiloki_book".to_string()]); let p = app_mcp_config(run, &r, "n1", &[], true, Some(&spec)).unwrap(); let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["antiloki"]["env"]["ANTILOKI_SPECIALIST"], json!("copy-chief")); assert_eq!(v["mcpServers"]["antiloki"]["env"]["ANTILOKI_SPECIALIST_TOOLS"], json!("antiloki_book"));
        let plain = app_mcp_config(run, &r, "n2", &[], true, None).unwrap(); let pv: Value = serde_json::from_str(&std::fs::read_to_string(&plain).unwrap()).unwrap(); assert!(pv["mcpServers"]["antiloki"]["env"].get("ANTILOKI_SPECIALIST").is_none(), "an ordinary agent is not gated as a specialist"); let _ = std::fs::remove_dir_all(&root);
    }
}
