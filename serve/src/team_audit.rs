//! THE SELF-IMPROVING ENGINE (operator 2026-11-13: "a self-audit measurement that happens every run, and if it sees the result is not good, it sends a message to the team leader for sure, and the team leader decides if it wants to alter the
//! thing: add edges, nodes, LLMs, bots, other agents, gates, to try again and see a better result"). Three parts, all here or next to it:
//!   1. `audit`     a deterministic, free measurement of a finished run from what the run itself recorded (its status, its checks, its loops, its model climbs, its cost, its gates). No model, no extra cost: it runs every time.
//!   2. the history `.antiloki/teams/<team>/audits.jsonl`, one line per run with the graph's hash, so a change the Leader makes can be judged by the runs that follow it (did the score move?).
//!   3. the message `leader_message`: when the result is not good the Leader is told, in its own terminal if it is open and in a durable inbox always (`leader-inbox.jsonl`), with the facts, the levers it has, and what is the person's to
//!      decide. The Leader decides; nothing here edits a graph. Pure: a snapshot in, an audit out.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Finding { pub code: String, /** blocker | warn | info */ pub severity: String, pub node: Option<String>, pub text: String, pub suggestion: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Audit { pub run: String, pub team: String, pub graph_hash: String, pub t_ms: i64, pub status: String, pub score: f64, pub good: bool, pub has_quality_signal: bool, pub findings: Vec<Finding>, pub signals: Value,
    /** a node is slipping (even when the run was good): the Leader is told all the same (team_nodebench) */ #[serde(default)] pub attention: bool }

fn f(code: &str, sev: &str, node: Option<&str>, text: String, suggestion: &str) -> Finding { Finding { code: code.into(), severity: sev.into(), node: node.map(String::from), text, suggestion: suggestion.into() } }
fn s(v: &Value) -> String { v.as_str().unwrap_or("").to_string() }

/// The audit of one run from its snapshot (`state.json`: `graph`, `state`) and the audits that came before it for the same team (newest last). Pure.
pub fn audit(snapshot: &Value, graph_hash: &str, previous: &[Audit], now_ms: i64) -> Audit {
    let st = &snapshot["state"]; let g = &snapshot["graph"]; let nodes = g["nodes"].as_object().cloned().unwrap_or_default(); let values = st["values"].as_object().cloned().unwrap_or_default();
    let status_key = st["status"]["status"].as_str().unwrap_or("idle").to_string(); let why = st["status"]["why"].as_str().unwrap_or("").to_string();
    let spent = st["spent_usd"].as_f64().unwrap_or(0.0); let budget = g["budget_usd"].as_f64().unwrap_or(0.0); let trace = st["trace"].as_array().cloned().unwrap_or_default();
    let mut out: Vec<Finding> = vec![]; let kind_of = |id: &str| nodes.get(id).and_then(|n| n["node"].as_str()).unwrap_or("").to_string();
    let title = |id: &str| nodes.get(id).and_then(|n| n["label"].as_str().map(String::from).or_else(|| n["agent"]["title"].as_str().map(String::from))).unwrap_or_else(|| id.to_string());

    // 1. how the run ended
    match status_key.as_str() {
        "blocked" => out.push(f("run_blocked", "blocker", None, format!("the run was handed to the person: {why}"), "find the node that could not finish and give it a way out: a failure edge to a fallback agent or a debugger, a stronger seat, a different brief; or a gate if only the person can decide this")),
        "done" => {} "stopped" => out.push(f("run_stopped", "info", None, "the person stopped the run".into(), "nothing to fix; ask them why if it keeps happening")),
        other => out.push(f("run_unfinished", "warn", None, format!("the run ended in the state `{other}`"), "look at its trace")),
    }
    // 2. the checks (the team's own measurement of quality): the last answer of each check node
    let mut checks = 0; let mut checks_failed = 0;
    for (id, v) in &values { if kind_of(id) != "check" { continue; } checks += 1;
        if v["pass"] == json!(false) { checks_failed += 1; let rules: Vec<String> = v["failed"].as_array().into_iter().flatten().filter_map(|x| x["message"].as_str().map(String::from)).take(3).collect();
            out.push(f("check_failed", "blocker", Some(id), format!("{} failed its last check: {}", title(id), rules.join("; ")), "the agent upstream cannot meet this rule: put the rule's words in its brief, give it a stronger seat, split its job, or ask whether the rule is right; a loop around it only helps if the agent can learn from what failed")); } }
    // 3. loops that ran out
    for (id, v) in &values { if kind_of(id) != "loop" { continue; } let (count, max) = (v["count"].as_u64().unwrap_or(0), nodes[id]["max_iterations"].as_u64().unwrap_or(0));
        if max > 0 && count >= max && status_key == "done" && checks_failed > 0 { out.push(f("loop_exhausted", "blocker", Some(id), format!("{} went round all {max} times and the result still failed", title(id)), "more rounds will not help by themselves: change what the body does between rounds (a different brief, a debugger, a stronger seat, a human gate)")); } }
    // 4. nodes that failed, caps, gates the person declined
    let mut failed_nodes: Vec<String> = vec![]; let mut caps = 0;
    for t in &trace { let (k, node) = (s(&t["kind"]), s(&t["node"])); if k == "failed" && !failed_nodes.contains(&node) { failed_nodes.push(node.clone()); } if k == "cap" { caps += 1; } }
    for n in &failed_nodes { let sev = if status_key == "blocked" { "blocker" } else { "warn" }; out.push(f("node_failed", sev, Some(n), format!("{} failed at least once in this run", title(n)), "add a failure edge from it (a fallback agent, a stronger seat, a retry) or read why it failed in its transcript")); }
    if caps > 0 { out.push(f("cost_cap_hit", "warn", None, format!("a node hit its cost cap {caps} time(s)"), "raise the cap if the work is worth it, or split the step; a cap that bites every run means the step is mis-sized")); }
    for (id, v) in &values { if kind_of(id) == "human" && v["approved"] == json!(false) { out.push(f("person_declined", "blocker", Some(id), format!("the person declined at {}", title(id)), "ask the person what was wrong: their reason is the best measurement there is, and it should become a rule (a check) or a change of brief")); } }
    // 5. money
    if budget > 0.0 { let ratio = spent / budget; if ratio >= 1.0 { out.push(f("over_budget", "blocker", None, format!("it spent ${spent:.2} of a ${budget:.2} budget"), "cheaper seats for mechanical steps (an llm node with guards, a check instead of a reviewer), fewer rounds, or a larger budget the person agrees to")); } else if ratio >= 0.8 { out.push(f("cost_near_budget", "warn", None, format!("it spent ${spent:.2} of ${budget:.2} ({:.0}%)", ratio * 100.0), "look at the cost by step in the result page: the most expensive node is the first to question")); } }
    // 6. the small seats: a climb is the guard working; many climbs mean the seat is not earning its place
    let (mut llm_nodes, mut climbed) = (0, 0);
    for (id, v) in &values { if kind_of(id) != "llm" { continue; } llm_nodes += 1; if v["escalated_from"].as_str().map_or(false, |x| !x.is_empty()) { climbed += 1; out.push(f("llm_climbed", "info", Some(id), format!("{} climbed from {} because {}", title(id), s(&v["escalated_from"]), s(&v["why_escalated"])), "if it climbs in most runs, trial the seat on cases with traps (antiloki_team_seats trial) and consider starting higher")); } }
    // 7. is there a measurement at all?
    let gates = values.keys().filter(|id| kind_of(id) == "human").count(); let agents = nodes.values().filter(|n| n["node"] == "role" && n["role"] == "agent").count();
    let has_signal = checks > 0 || gates > 0 || nodes.values().any(|n| n["node"] == "role" && n["role"] == "qa");
    if !has_signal && agents > 0 { out.push(f("no_quality_signal", "warn", None, "this team has no check, no QA and no gate: the audit can say whether it finished and what it cost, not whether the result is good".into(), "add a check node for what a rule can judge, or a QA or a gate for what it cannot: without a measurement the team cannot improve")); }
    // 8. the same team failing again and again: a redesign, not a tweak
    let last3: Vec<&Audit> = previous.iter().rev().take(2).collect(); let this_bad = out.iter().any(|x| x.severity == "blocker");
    if this_bad && last3.len() == 2 && last3.iter().all(|a| !a.good) { out.push(f("repeated_failure", "blocker", None, "this is the third run in a row that did not produce a good result".into(), "stop tweaking: is a team the right shape for this goal, is a different design needed, or is one raw agent enough? Tell the person plainly")); }
    // 9. did your last change help? (the self-improving loop: the score by graph)
    if let Some(prev) = previous.iter().rev().find(|a| a.graph_hash != graph_hash) { if previous.last().map_or(false, |l| l.graph_hash != graph_hash) { let now_score = score_of(&out, status_key == "done"); let d = now_score - prev.score; out.push(f("since_last_change", "info", None, format!("the graph changed since the run before: the score went from {:.2} to {:.2} ({})", prev.score, now_score, if d > 0.05 { "better" } else if d < -0.05 { "worse" } else { "about the same" }), "keep what helped, revert what did not")); } }
    let blockers = out.iter().filter(|x| x.severity == "blocker").count(); let score = score_of(&out, status_key == "done"); let good = blockers == 0 && status_key == "done";
    let escalated_rate = if llm_nodes == 0 { 0.0 } else { climbed as f64 / llm_nodes as f64 };
    Audit { run: s(&snapshot["id"]), team: s(&snapshot["team"]), graph_hash: graph_hash.into(), t_ms: now_ms, status: status_key, score, good, has_quality_signal: has_signal, findings: out, attention: false,
        signals: json!({ "spent_usd": spent, "budget_usd": budget, "checks": checks, "checks_failed": checks_failed, "failed_nodes": failed_nodes, "caps_hit": caps, "llm_nodes": llm_nodes, "llm_climbed": climbed, "llm_climb_rate": escalated_rate, "gates": gates }) }
}
fn score_of(findings: &[Finding], done: bool) -> f64 { let b = findings.iter().filter(|x| x.severity == "blocker").count() as f64; let w = findings.iter().filter(|x| x.severity == "warn").count() as f64; let base = if done { 1.0 } else { 0.4 }; ((base - 0.35 * b - 0.1 * w) * 100.0).round().clamp(0.0, 100.0) / 100.0 }

/// The message the Leader is told: the facts, the levers, and who decides what. Starts with `[team audit]` so a watcher never mistakes it for the person talking. Pure.
pub fn leader_message(a: &Audit, team_name: &str) -> String {
    let mut o = if a.good { format!("[team audit] The team \"{team_name}\" ran ({}) and the result was good (score {:.2}, ${:.2}), but a NODE IS SLIPPING:\n", a.run, a.score, a.signals["spent_usd"].as_f64().unwrap_or(0.0)) } else { format!("[team audit] The team \"{team_name}\" ran ({}) and the result is NOT good: score {:.2}, status {}, ${:.2} spent.\n", a.run, a.score, a.status, a.signals["spent_usd"].as_f64().unwrap_or(0.0)) };
    for x in a.findings.iter().filter(|x| x.severity != "info") { o.push_str(&format!("- [{}] {}{}: {}\n    what you could change: {}\n", x.severity, x.code, x.node.as_ref().map_or(String::new(), |n| format!(" at {n}")), x.text, x.suggestion)); }
    o.push_str("\nYOU ARE THE MANAGER: decide. Read the details with antiloki_team_audit (op `get` {run}; `history` {team} shows the scores by graph). You may change the team: antiloki_team_design or antiloki_team_node to add or alter nodes (agents, llm nodes with guards, checks, bots, gates), edges, briefs, seats and budgets. Judge whether a team is still the right shape or one raw agent would do. ");
    o.push_str("Tell the person what you found and what you changed and why, and ask before you run it again (cost is theirs to approve) unless they asked you to iterate on your own. You cannot approve a merge or a skill. If you decide NOT to change anything, say why, and mark this handled with antiloki_team_audit op `ack`.");
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snap(status: Value, values: Value, trace: Value, spent: f64, nodes: Value) -> Value { json!({ "id": "r-1", "team": "t", "graph": { "budget_usd": 2.0, "nodes": nodes }, "state": { "status": status, "values": values, "trace": trace, "spent_usd": spent } }) }
    fn nodes() -> Value { json!({ "w": { "node": "role", "role": "agent", "agent": { "title": "the Writer" } }, "chk": { "node": "check" }, "rl": { "node": "loop", "max_iterations": 3 }, "k": { "node": "llm" }, "me": { "node": "human" } }) }
    fn codes(a: &Audit) -> Vec<String> { a.findings.iter().map(|x| x.code.clone()).collect() }

    #[test]
    fn a_clean_run_with_a_check_is_good_and_a_run_with_no_measurement_says_so() {
        let a = audit(&snap(json!({ "status": "done" }), json!({ "chk": { "pass": true } }), json!([]), 0.3, nodes()), "h1", &[], 1); assert!(a.good && a.score == 1.0 && a.has_quality_signal && a.findings.is_empty(), "{a:?}");
        let n = json!({ "w": { "node": "role", "role": "agent" } }); let b = audit(&snap(json!({ "status": "done" }), json!({}), json!([]), 0.1, n), "h1", &[], 1); assert!(b.good && !b.has_quality_signal && codes(&b) == vec!["no_quality_signal"], "good by what it can see, and it says it cannot see quality");
    }
    #[test]
    fn a_failed_check_an_exhausted_loop_a_declined_gate_and_a_blocked_run_are_blockers_with_what_to_change() {
        let a = audit(&snap(json!({ "status": "done" }), json!({ "chk": { "pass": false, "failed": [{ "message": "at most 150 words" }] }, "rl": { "count": 3 }, "me": { "approved": false } }), json!([]), 0.5, nodes()), "h1", &[], 1);
        assert!(!a.good); for c in ["check_failed", "loop_exhausted", "person_declined"] { assert!(codes(&a).contains(&c.to_string()), "{c} in {:?}", codes(&a)); } assert!(a.findings.iter().find(|x| x.code == "check_failed").unwrap().text.contains("at most 150 words")); assert!(a.score < 0.5);
        let b = audit(&snap(json!({ "status": "blocked", "why": "writer did not go to plan" }), json!({}), json!([{ "kind": "failed", "node": "w" }]), 0.2, nodes()), "h1", &[], 1); assert!(!b.good && codes(&b).contains(&"run_blocked".to_string()) && b.findings.iter().any(|x| x.code == "node_failed" && x.severity == "blocker"));
        let c = audit(&snap(json!({ "status": "stopped" }), json!({}), json!([]), 0.0, nodes()), "h1", &[], 1); assert!(!c.good && c.findings[0].severity == "info", "a run the person stopped is not a failure to message the Leader about... but it is not good either");
    }
    #[test]
    fn money_and_small_seats_are_measured_too() {
        let a = audit(&snap(json!({ "status": "done" }), json!({ "chk": { "pass": true }, "k": { "escalated_from": "endpoint:lfm3", "why_escalated": "price not in the text" } }), json!([{ "kind": "cap", "node": "w" }]), 1.7, nodes()), "h1", &[], 1);
        assert!(a.good, "warnings are not failures"); for c in ["cost_near_budget", "cost_cap_hit", "llm_climbed"] { assert!(codes(&a).contains(&c.to_string()), "{c}"); } assert_eq!(a.signals["llm_climbed"], 1);
        let b = audit(&snap(json!({ "status": "done" }), json!({ "chk": { "pass": true } }), json!([]), 2.5, nodes()), "h1", &[], 1); assert!(!b.good && codes(&b).contains(&"over_budget".to_string()));
    }
    #[test]
    fn three_bad_runs_in_a_row_ask_for_a_redesign_and_a_changed_graph_reports_what_the_change_did() {
        let bad = |h: &str, score: f64| Audit { run: "x".into(), team: "t".into(), graph_hash: h.into(), t_ms: 0, status: "done".into(), score, good: false, has_quality_signal: true, findings: vec![], signals: json!({}), attention: false };
        let fail = snap(json!({ "status": "done" }), json!({ "chk": { "pass": false, "failed": [] } }), json!([]), 0.2, nodes());
        let a = audit(&fail, "h2", &[bad("h2", 0.3), bad("h2", 0.3)], 1); assert!(codes(&a).contains(&"repeated_failure".to_string()), "{:?}", codes(&a));
        let ok = snap(json!({ "status": "done" }), json!({ "chk": { "pass": true } }), json!([]), 0.2, nodes()); let b = audit(&ok, "h3", &[bad("h2", 0.3)], 1); let c = b.findings.iter().find(|x| x.code == "since_last_change").unwrap(); assert!(c.text.contains("0.30 to 1.00") && c.text.contains("better"), "{}", c.text);
    }
    #[test]
    fn the_leaders_message_carries_the_facts_the_levers_and_who_decides() {
        let a = audit(&snap(json!({ "status": "done" }), json!({ "chk": { "pass": false, "failed": [{ "message": "no placeholders" }] } }), json!([]), 0.5, nodes()), "h1", &[], 1); let m = leader_message(&a, "Posts");
        assert!(m.starts_with("[team audit]") && m.contains("NOT good") && m.contains("check_failed") && m.contains("no placeholders") && m.contains("what you could change") && m.contains("antiloki_team_design") && m.contains("ask before you run it again") && m.contains("cannot approve a merge"), "{m}");
    }
}
