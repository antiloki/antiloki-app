//! SELF-EVOLUTION AS AN EXPERIMENT (the operator 2026-11-22: "completely scientific methodology; a goal, at least 20% better, at most 5 tries, then it stops with a report; each run reports to the Leader; the 20%, the 5 tries and the budget are customizable, by the person and by the Leader when asked").
//! One SESSION = a goal (cost, time or quality), a least gain (default 20%), a most number of trials (default 5) and a purse (dollars). Each trial is a controlled experiment: the improver proposes ONE hypothesis from a short list of kinds of change (a deterministic node in place of an agent, control flow, decomposition, a cheaper or specialist agent, a specialised branch), the candidate and the current version run side by side on the replay bank (staged, repeated, held-out, a lower bound, a regression budget: `team_experiment`), and the change is kept only if the GOAL moved by the least gain with the pass rate held. The first change that does it ends the session (the goal is met); five failed trials, an empty purse or a stop ends it too. Every ending writes a report, and the Leader is sent it.
//! What is measured is always against the current version in the SAME experiment (no baseline that drifts); what failed is told to the next trial so it is not repeated (path dependence, SICA; the archive of refused ideas, DGM).
use crate::team_experiment::Goal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Cfg { pub goal: String, pub min_gain_pct: f64, pub max_trials: u32, pub budget_usd: f64, pub repeats: u32, pub seat: String }
impl Default for Cfg { fn default() -> Self { Cfg { goal: "cost".into(), min_gain_pct: 20.0, max_trials: 5, budget_usd: 2.0, repeats: 3, seat: "claude-code:haiku".into() } } }
impl Cfg {
    /// A config as given, held to what makes sense; the goal word must be one we know. Pure.
    pub fn tidy(mut self) -> Result<Cfg, String> {
        if Goal::parse(&self.goal).is_none() { return Err(format!("the goal is cost, time or quality, not `{}`", self.goal)); } self.goal = Goal::parse(&self.goal).unwrap().word().to_string();
        self.min_gain_pct = self.min_gain_pct.clamp(1.0, 90.0); self.max_trials = self.max_trials.clamp(1, 20); self.budget_usd = (self.budget_usd.clamp(0.05, 200.0) * 100.0).round() / 100.0; self.repeats = self.repeats.clamp(1, 10);
        if crate::team_llm::parse_seat(&self.seat).is_err() { return Err(format!("`{}` is not a seat for the improver (claude-code:haiku, local:<tag>, endpoint:<alias>)", self.seat)); } Ok(self)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct Trial { pub n: u32, pub kind: String, pub hypothesis: String, pub request: Option<u32>, /** proposed | no_proposal | screened_out | not_admitted | admitted | error */ pub outcome: String, pub gain_pct: f64, /** the gain once the run-to-run noise is counted (the low end of the whisker the screen draws) */ #[serde(default)] pub sure_pct: f64, pub spent_usd: f64, pub why: String, #[serde(default)] pub experiment: Value }
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct Session {
    /** idle | running | goal_met | exhausted | budget | stopped | needs_tasks | error */ pub status: String, pub goal: String, pub min_gain_pct: f64, pub max_trials: u32, pub budget_usd: f64, pub spent_usd: f64, pub started_ms: i64, pub ended_ms: i64, pub trials: Vec<Trial>, pub report: String, pub note: String,
}
fn dir(root: &str, team: &str) -> PathBuf { crate::team_graph_run::team_files(root, team) }
pub fn cfg(root: &str, team: &str) -> Cfg { std::fs::read_to_string(dir(root, team).join("evolve.json")).ok().and_then(|t| serde_json::from_str::<Cfg>(&t).ok()).and_then(|c| c.tidy().ok()).unwrap_or_default() }
pub fn set_cfg(root: &str, team: &str, c: Cfg) -> Result<Cfg, String> { let c = c.tidy()?; let d = dir(root, team); std::fs::create_dir_all(&d).map_err(|e| e.to_string())?; std::fs::write(d.join("evolve.json"), serde_json::to_string_pretty(&c).unwrap_or_default()).map_err(|e| e.to_string())?; Ok(c) }
/// What the person (or the Leader, when asked) changed, merged onto the config. Pure over the config.
pub fn merge_cfg(base: &Cfg, patch: &Value) -> Cfg { let mut c = base.clone(); if let Some(x) = patch["goal"].as_str() { c.goal = x.into(); } if let Some(x) = patch["min_gain_pct"].as_f64() { c.min_gain_pct = x; } if let Some(x) = patch["max_trials"].as_u64() { c.max_trials = x as u32; } if let Some(x) = patch["budget_usd"].as_f64() { c.budget_usd = x; } if let Some(x) = patch["repeats"].as_u64() { c.repeats = x as u32; } if let Some(x) = patch["seat"].as_str() { c.seat = x.into(); } c }
pub fn session(root: &str, team: &str) -> Session { std::fs::read_to_string(dir(root, team).join("evolve-session.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| Session { status: "idle".into(), ..Default::default() }) }
fn save(root: &str, team: &str, s: &Session) { let d = dir(root, team); let _ = std::fs::create_dir_all(&d); let _ = std::fs::write(d.join("evolve-session.json"), serde_json::to_string_pretty(s).unwrap_or_default()); }
fn stop_flag(root: &str, team: &str) -> PathBuf { dir(root, team).join("evolve.stop") }
pub fn clear_stop(root: &str, team: &str) { let _ = std::fs::remove_file(stop_flag(root, team)); }
pub fn request_stop(root: &str, team: &str) { let _ = std::fs::create_dir_all(dir(root, team)); let _ = std::fs::write(stop_flag(root, team), "stop"); }
/// `Some(session)` while one runs in this process: one at a time per team.
static RUNNING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
pub fn running(root: &str, team: &str) -> bool { RUNNING.lock().map(|v| v.contains(&format!("{root}|{team}"))).unwrap_or(false) }

/// THE KINDS OF CHANGE the improver is asked to consider, in the order that usually pays most for the goal: what an agent does that a program could do costs the most for nothing.
pub const KINDS: [(&str, &str); 6] = [
    ("deterministic", "replace an agent step with a deterministic node (a code node, a check, a tool, a `match` cell) where its output can be computed or verified by rules"),
    ("flow", "change the control flow: run independent steps in parallel, skip a step when its input is empty, stop early when a check passes, cap retries or loop limits"),
    ("decompose", "split one large step into smaller ones (or merge steps that only pass text along), so each is easier, cheaper or can run apart"),
    ("model", "give a node a cheaper or faster model or tier where the benchmark shows it holds, and keep the strong one only where it matters"),
    ("specialist", "give a step a specialist agent with a narrow prompt and only the tools it needs, instead of a general one"),
    ("branch", "add a specialised branch: a route for the common easy case that skips the heavy path, with the heavy path kept for the rest"),
];
/// What the improver is told about THIS session: the goal in numbers, the kinds of change, and every earlier trial with its result, so nothing is tried twice. Pure.
pub fn trial_brief(c: &Cfg, trials: &[Trial], n: u32) -> String {
    let mut s = format!("THIS IS A SCIENTIFIC EXPERIMENT, trial {n} of at most {}.\nGOAL: make the team at least {:.0}% better on {} ({}), with the pass rate not falling. The current version is measured in the same experiment as your candidate, side by side, on held-out tasks you have not seen.\nPropose ONE hypothesis (one idea, one patch). Say what you expect to move and why, from the evidence above, not from taste.\nKINDS OF CHANGE (think across all of them; the first usually pays most):\n", c.max_trials, c.min_gain_pct, c.goal, match c.goal.as_str() { "cost" => "dollars per run", "time" => "seconds per run", _ => "the pass rate" });
    for (k, d) in KINDS { s.push_str(&format!("- {k}: {d}\n")); }
    if trials.is_empty() { s.push_str("\nNo trial has been run yet.\n"); } else { s.push_str("\nALREADY TRIED IN THIS SESSION (do not propose these again; learn from why they failed):\n"); for t in trials { s.push_str(&format!("- trial {} [{}] {}: {} ({})\n", t.n, t.kind, t.hypothesis, t.outcome, t.why)); } }
    s
}
/// WHAT THE IMPROVER NEEDS TO PROPOSE ANYTHING REAL (found by the first real session, 2026-11-22: five trials, five empty answers, because the prompt named the nodes but not what they say, and gave no grammar for a patch): the team as it is, node by node with the prompts and seats; the exact patch operations with an example of each; what the recent runs cost and took per node; and the tests it will be judged on. Pure over its inputs.
pub fn evidence_block(g: &crate::team_graph::Graph, runs: &[Value], asks: &[String]) -> String {
    fn trim(v: &Value) -> Value { match v { Value::Object(o) => Value::Object(o.iter().filter(|(_, x)| !x.is_null() && !matches!(x, Value::Array(a) if a.is_empty())).map(|(k, x)| (k.clone(), trim(x))).collect()), x => x.clone() } }
    let gj = g.to_json(); let mut o = String::from("THE TEAM AS IT IS (nodes with their settings; `{ask}` and `{node.field}` in a prompt are filled from the run)\n");
    if let Some(ns) = gj["nodes"].as_object() { for (id, n) in ns { o.push_str(&format!("- {id}: {}\n", trim(n))); } }
    o.push_str("EDGES (from -> to, by port)\n"); for e in gj["edges"].as_array().into_iter().flatten() { o.push_str(&format!("- {} -> {} ({})\n", e["from"].as_str().unwrap_or(""), e["to"].as_str().unwrap_or(""), e["port"].as_str().unwrap_or("out"))); }
    o.push_str("\nA PATCH IS A LIST OF THESE OPERATIONS (JSON, at most 3). Examples:\n  {\"op\":\"set_field\",\"node\":\"draft\",\"path\":\"prompt\",\"value\":\"the new prompt text\"}   (any field of a node: prompt, seat, max_tokens, timeout_s, cap_usd)\n  {\"op\":\"set_field\",\"node\":\"polish\",\"path\":\"seat\",\"value\":\"claude-code:haiku\"}\n  {\"op\":\"remove_edge\",\"from\":\"draft\",\"port\":\"out\",\"to\":\"polish\"}\n  {\"op\":\"add_edge\",\"from\":\"draft\",\"port\":\"out\",\"to\":\"final\"}\n  {\"op\":\"remove_node\",\"id\":\"polish\"}   (take its edges away first, and wire its input to its output)\n  {\"op\":\"add_node\",\"id\":\"pick\",\"node\":{\"node\":\"code\",\"source\":\"...\"}}\nChecks, gates, approvals, budgets and triggers cannot be changed by you (they are the person's and the judge's).\n");
    let done: Vec<&Value> = runs.iter().filter(|r| r["status"]["status"] == json!("done")).take(8).collect();
    if done.is_empty() { o.push_str("\nRECENT RUNS: none finished yet.\n"); } else { o.push_str(&format!("\nRECENT FINISHED RUNS ({}), cost in dollars:\n", done.len())); let mut per: std::collections::BTreeMap<String, (f64, u32)> = Default::default(); for r in &done { o.push_str(&format!("- {} total ${:.4}\n", r["id"].as_str().unwrap_or(""), r["spent_usd"].as_f64().unwrap_or(0.0))); if let Some(m) = r["node_cost"].as_object() { for (k, v) in m { let e = per.entry(k.clone()).or_default(); e.0 += v.as_f64().unwrap_or(0.0); e.1 += 1; } } } if !per.is_empty() { o.push_str("Average per node:\n"); for (k, (c, n)) in &per { o.push_str(&format!("- {k}: ${:.4}\n", c / *n as f64)); } } }
    if !asks.is_empty() { o.push_str("\nTHE TESTS IT WILL BE JUDGED ON (the asks; the checks are hidden from you):\n"); for a in asks.iter().take(6) { o.push_str(&format!("- {}\n", a.chars().take(140).collect::<String>())); } }
    o
}
/// The kind of change a patch is, from what it does (for the report and so the next trial varies). Pure.
pub fn kind_of(hypothesis: &str, patch: &Value) -> String {
    let h = hypothesis.to_lowercase(); let p = patch.to_string().to_lowercase();
    let has = |w: &[&str]| w.iter().any(|x| h.contains(x) || p.contains(x));
    if has(&["\"code\"", "deterministic", "\"check\"", "\"use_tool\"", "match cell"]) { "deterministic" } else if has(&["parallel", "fork", "retry", "early", "skip", "limit", "loop", "\"if\""]) { "flow" } else if has(&["split", "decompose", "merge steps"]) { "decompose" } else if has(&["specialist", "narrow"]) { "specialist" } else if has(&["branch", "route", "\"switch\""]) { "branch" } else if has(&["model", "tier", "haiku", "sonnet", "cheaper"]) { "model" } else { "other" }.to_string()
}
/// The report of a session in plain words: the goal, each trial with its numbers, what was kept and what it means. Pure.
pub fn report_text(team: &str, s: &Session) -> String {
    let word = match s.status.as_str() { "goal_met" => "GOAL MET", "exhausted" => "STOPPED: all trials used, the goal was not reached", "budget" => "STOPPED: the budget ran out", "stopped" => "STOPPED by the person", "needs_tasks" => "NOT STARTED: the team has no tests to measure against", "error" => "STOPPED: an error", other => other };
    let mut t = format!("SELF-EVOLUTION REPORT for {team}\n{word}\nGoal: {} down/up by at least {:.0}% · trials used {} of {} · spent ${:.2} of ${:.2}\n", s.goal, s.min_gain_pct, s.trials.len(), s.max_trials, s.spent_usd, s.budget_usd);
    for x in &s.trials { t.push_str(&format!("\nTrial {} · {} · {}\n  hypothesis: {}\n  result: {}{}\n  why: {}\n  spent ${:.3}\n", x.n, x.kind, x.outcome.replace('_', " "), x.hypothesis, if x.outcome == "admitted" { "KEPT, " } else { "" }, format!("{:+.0}% on {}", x.gain_pct, s.goal), x.why, x.spent_usd)); }
    if s.trials.is_empty() { t.push_str(&format!("\n{}\n", s.note)); }
    let best = s.trials.iter().find(|x| x.outcome == "admitted").or_else(|| s.trials.iter().filter(|x| x.outcome == "not_admitted").max_by(|a, b| a.gain_pct.partial_cmp(&b.gain_pct).unwrap_or(std::cmp::Ordering::Equal)));
    match (s.status.as_str(), best) { ("goal_met", Some(b)) => t.push_str(&format!("\nThe change of trial {} was approved and is now the live version of the team ({:+.0}% on {}).\n", b.n, b.gain_pct, s.goal)), (_, Some(b)) if s.status != "needs_tasks" => t.push_str(&format!("\nNothing was changed. The closest was trial {} ({:+.0}% on {}). Reasons it fell short are above: the next session can start from them, or the goal can be eased.\n", b.n, b.gain_pct, s.goal)), _ => {} }
    t
}

/// What a session needs from the outside, so the loop can be tested without a model or a run: ask the improver, open a request from its answer, run the experiment of a request, decide it.
pub trait Hands {
    /// The improver's answer to a prompt and what the call cost.
    fn propose(&self, prompt: &str) -> Result<(String, f64), String>;
    /// Open a request from the answer; its number, hypothesis and patch.
    fn open(&self, reply: &str) -> Result<(u32, String, Value), String>;
    /// Run the request's experiment with a budget; wait for it; the experiment's report (as attached to the request) and its progress.
    fn experiment(&self, n: u32, budget: f64) -> Result<(Value, Value), String>;
    fn decide(&self, n: u32, approve: bool, why: &str) -> Result<(), String>;
    fn tasks(&self) -> (usize, usize);
    fn stopped(&self) -> bool;
    fn now(&self) -> i64;
    fn brief(&self, c: &Cfg, trials: &[Trial], n: u32) -> String;
    fn tell(&self, report: &str);
    /// Why a new run should NOT start right now (the person's machine is short of RAM or CPU), or None. The swarm waits on it before each run; the default says there is always room.
    fn busy(&self) -> Option<String> { None }
    /// How long to wait between two looks at `busy`, in milliseconds.
    fn busy_wait_ms(&self) -> u64 { 5000 }
}
/// What one finished experiment says about its trial: money spent, the gain (0 for a candidate screened out, which never ran the full test), why, the outcome word, and whether it was admitted. Pure.
pub struct Judged { pub spent: f64, pub gain_pct: f64, pub sure_pct: f64, pub why: String, pub outcome: &'static str, pub admitted: bool }
pub fn judge(goal: Goal, exp: &Value, prog: &Value) -> Judged {
    let arm = |k: &str| serde_json::from_value::<crate::team_experiment::Arm>(exp[k].clone()).unwrap_or_default();
    let gain_pct = if exp["stage"] == json!("full") { crate::team_experiment::gain(goal, &arm("current"), &arm("candidate")) * 100.0 } else { 0.0 };
    let sure_pct = if exp["stage"] == json!("full") && goal != Goal::Quality { crate::team_experiment::sure_gain(goal, &arm("current"), &arm("candidate")) * 100.0 } else { gain_pct };
    let admitted = exp["admitted"].as_bool().or(prog["admitted"].as_bool()).unwrap_or(false);
    Judged { sure_pct, spent: exp["spent_usd"].as_f64().or(prog["spent_usd"].as_f64()).unwrap_or(0.0), gain_pct, why: exp["why"].as_str().or(prog["why"].as_str()).unwrap_or("").to_string(), outcome: if admitted { "admitted" } else if exp["stage"] == json!("screened_out") { "screened_out" } else { "not_admitted" }, admitted }
}
/// The loop. Blocking. Returns the finished session (also saved after every step, so the screen can follow it).
pub fn run_with(h: &dyn Hands, root: &str, team: &str, c: &Cfg) -> Session {
    let mut s = Session { status: "running".into(), goal: c.goal.clone(), min_gain_pct: c.min_gain_pct, max_trials: c.max_trials, budget_usd: c.budget_usd, started_ms: h.now(), ..Default::default() };
    let goal = Goal::parse(&c.goal).unwrap_or_default(); let (tune, held) = h.tasks();
    if held == 0 || tune == 0 { s.status = "needs_tasks".into(); s.note = format!("The test bank has {tune} practice and {held} held-out tasks. An experiment needs at least 1 of each: save a few passed runs as tests first."); s.ended_ms = h.now(); s.report = report_text(team, &s); save(root, team, &s); h.tell(&s.report); return s; }
    save(root, team, &s);
    for n in 1..=c.max_trials {
        if h.stopped() { s.status = "stopped".into(); break; }
        let left = c.budget_usd - s.spent_usd; if left < 0.05 { s.status = "budget".into(); break; }
        let mut t = Trial { n, ..Default::default() };
        let reply = match h.propose(&h.brief(c, &s.trials, n)) { Ok((r, usd)) => { t.spent_usd += usd; s.spent_usd += usd; r } Err(e) => { t.outcome = "error".into(); t.why = format!("the improver could not answer: {e}"); s.trials.push(t); s.status = "error".into(); s.note = "the improver seat did not answer".into(); break; } };
        let (req, hyp, patch) = match h.open(&reply) { Ok(x) => x, Err(e) => { t.outcome = "no_proposal".into(); t.why = e; s.trials.push(t); save(root, team, &s); continue; } };
        t.request = Some(req); t.hypothesis = hyp.clone(); t.kind = kind_of(&hyp, &patch); t.outcome = "proposed".into(); s.trials.push(t.clone()); save(root, team, &s);
        let per = ((c.budget_usd - s.spent_usd) / (c.max_trials - n + 1) as f64 * 1.5).min(c.budget_usd - s.spent_usd).max(0.05);
        let (exp, prog) = match h.experiment(req, per) { Ok(x) => x, Err(e) => { t.outcome = "error".into(); t.why = e; *s.trials.last_mut().unwrap() = t; let _ = h.decide(req, false, "the experiment could not run"); save(root, team, &s); continue; } };
        let j = judge(goal, &exp, &prog); t.spent_usd += j.spent; s.spent_usd += j.spent; t.gain_pct = j.gain_pct; t.sure_pct = j.sure_pct; t.why = j.why; t.experiment = exp.clone(); t.outcome = j.outcome.into(); let admitted = j.admitted;
        if admitted { let _ = h.decide(req, true, &format!("the self-evolution session: {}", t.why)); } else { let _ = h.decide(req, false, &t.why); }
        let last = s.trials.len() - 1; s.trials[last] = t.clone(); save(root, team, &s);
        if admitted { s.status = "goal_met".into(); break; }
    }
    if s.status == "running" { s.status = "exhausted".into(); }
    s.ended_ms = h.now(); s.report = report_text(team, &s); save(root, team, &s); h.tell(&s.report); s
}

/// The real hands: a seat for the improver, the request machinery, the experiment executor, the leader's inbox.
pub struct Real { pub root: String, pub team: String, pub cfg: Cfg, pub rt: tokio::runtime::Handle }
impl Hands for Real {
    fn propose(&self, prompt: &str) -> Result<(String, f64), String> { let seat = crate::team_llm::parse_seat(&self.cfg.seat)?; let r = self.rt.block_on(crate::team_llm::call_plain(&seat, prompt, 6000, 420, None))?; Ok((r.0, r.1)) }
    fn open(&self, reply: &str) -> Result<(u32, String, Value), String> {
        let m = crate::team_improver::request_from_reply(&self.root, &self.team, reply, &[], crate::cli::now_ms())?;
        let ps = crate::team_improve::parse_proposals(reply).ok().and_then(|p| p.into_iter().next()); let (hyp, patch) = ps.map(|p| (p.hypothesis, serde_json::to_value(&p.patch).unwrap_or_default())).unwrap_or_else(|| (m.problem.clone(), Value::Null)); Ok((m.n, hyp, patch))
    }
    fn experiment(&self, n: u32, budget: f64) -> Result<(Value, Value), String> {
        let goal = Goal::parse(&self.cfg.goal).unwrap_or_default(); crate::team_executor::start_for(&self.root, &self.team, n, self.cfg.repeats, budget, goal, self.cfg.min_gain_pct / 100.0)?;
        let t0 = std::time::Instant::now(); loop { std::thread::sleep(std::time::Duration::from_secs(3)); let p = crate::team_executor::progress(&self.root, &self.team, n); match p["state"].as_str() { Some("done") => break, Some("failed") => return Err(p["error"].as_str().unwrap_or("the experiment failed").to_string()), _ => {} } if self.stopped() { return Err("stopped".into()); } if t0.elapsed().as_secs() > 3 * 3600 { return Err("the experiment took over three hours".into()); } }
        let exp = crate::team_request::get(&self.root, &self.team, n).and_then(|m| m.experiment).unwrap_or(Value::Null); Ok((exp, crate::team_executor::progress(&self.root, &self.team, n)))
    }
    fn decide(&self, n: u32, approve: bool, why: &str) -> Result<(), String> { crate::team_request::decide(&self.root, &self.team, n, if approve { "approve" } else { "refuse" }, why, "the self-evolution session", false, crate::cli::now_ms()).map(|_| ()) }
    fn tasks(&self) -> (usize, usize) { (crate::team_replay::tasks(&self.root, &self.team, "tune").len(), crate::team_replay::tasks(&self.root, &self.team, "heldout").len()) }
    fn stopped(&self) -> bool { stop_flag(&self.root, &self.team).exists() }
    fn now(&self) -> i64 { crate::cli::now_ms() }
    fn brief(&self, c: &Cfg, trials: &[Trial], n: u32) -> String { let live = crate::team_graph_run::load_graph(&self.root, &self.team); let base = live.as_ref().map(|g| crate::team_improver::prompt(&self.root, &self.team, g)).unwrap_or_default(); let asks: Vec<String> = ["tune", "heldout"].iter().flat_map(|sp| crate::team_replay::tasks(&self.root, &self.team, sp)).map(|t| t.ask).collect(); let ev = live.as_ref().map(|g| evidence_block(g, &crate::team_graph_run::runs_of(&self.root, &self.team).into_iter().take(10).map(|mut r| { if let Some(id) = r["id"].as_str().map(String::from) { if let Some(d) = crate::team_graph_run::run_on_disk(&self.root, &id) { r["node_cost"] = d["state"]["node_cost"].clone(); } } r }).collect::<Vec<_>>(), &asks)).unwrap_or_default(); format!("{base}\n\n{ev}\n\n{}", trial_brief(c, trials, n)) }
    fn busy(&self) -> Option<String> { let (t, a) = crate::mem::system()?; let (sw, why) = crate::mem::switch_for(t, a, crate::mem::load1(), std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2), 1, 0); if sw == crate::mem::Switch::Pause { Some(why) } else { None } }
    fn tell(&self, report: &str) { inbox(&self.root, &self.team, json!({ "id": format!("e-{}", crate::cli::now_ms()), "t_ms": crate::cli::now_ms(), "status": "unread", "kind": "evolve_report", "message": report })); }
}
/// Append a row to the Leader's inbox.
pub fn inbox(root: &str, team: &str, row: Value) { let p = dir(root, team).join("leader-inbox.jsonl"); let _ = std::fs::create_dir_all(p.parent().unwrap()); if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) { use std::io::Write; let _ = writeln!(f, "{row}"); } }
/// Start a session in the background (the person's call: it spends their seats). One at a time per team.
pub fn start(root: &str, team: &str) -> Result<Session, String> {
    let key = format!("{root}|{team}"); { let mut r = RUNNING.lock().map_err(|e| e.to_string())?; if r.contains(&key) { return Err("a self-evolution session is already running for this team".into()); } r.push(key.clone()); }
    let c = cfg(root, team); let _ = std::fs::remove_file(stop_flag(root, team));
    if crate::team_graph_run::load_graph(root, team).is_err() { if let Ok(mut r) = RUNNING.lock() { r.retain(|k| k != &key); } return Err("no such team".into()); }
    let s0 = Session { status: "running".into(), goal: c.goal.clone(), min_gain_pct: c.min_gain_pct, max_trials: c.max_trials, budget_usd: c.budget_usd, started_ms: crate::cli::now_ms(), ..Default::default() }; save(root, team, &s0);
    let (r, t, rt) = (root.to_string(), team.to_string(), tokio::runtime::Handle::current());
    tokio::task::spawn_blocking(move || { let hands = Real { root: r.clone(), team: t.clone(), cfg: c.clone(), rt }; let _ = run_with(&hands, &r, &t, &c); if let Ok(mut v) = RUNNING.lock() { v.retain(|k| k != &format!("{r}|{t}")); } });
    Ok(s0)
}
/// A FULL REPORT OF ONE RUN FOR THE LEADER (the operator 2026-11-22: "each run sends a full report to the leader so the leader can reshape the team"): how it ended, what it cost and took against the plan, where the time and money went node by node, what stopped it, and how the numbers compare to the team's recent runs. Pure over the run's state.
pub fn run_report(team: &str, snap: &Value, recent: &[Value]) -> String {
    let st = &snap["state"]; let status = st["status"]["status"].as_str().unwrap_or("?"); let cost = st["spent_usd"].as_f64().or(snap["spent_usd"].as_f64()).unwrap_or(0.0);
    let started = st["started_ms"].as_f64().unwrap_or(0.0); let now = st["now_ms"].as_f64().unwrap_or(started); let secs = ((now - started) / 1000.0).max(0.0);
    let mut o = format!("RUN REPORT for {team} · run {} · {status} · ${cost:.3} · {secs:.0} s\nAsk: {}\n", snap["id"].as_str().unwrap_or(""), snap["ask"].as_str().unwrap_or("").chars().take(160).collect::<String>());
    if let Some(m) = st["node_cost"].as_object() { let mut v: Vec<(&String, f64)> = m.iter().filter_map(|(k, x)| Some((k, x.as_f64()?))).collect(); v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)); if !v.is_empty() { o.push_str("Where the money went:\n"); for (k, c) in v.iter().take(8) { o.push_str(&format!("- {k}: ${c:.3} ({:.0}%)\n", if cost > 0.0 { c / cost * 100.0 } else { 0.0 })); } } }
    if let Some(w) = st["status"]["why"].as_str().filter(|w| !w.is_empty()) { o.push_str(&format!("Why it stopped: {w}\n")); }
    let done: Vec<&Value> = recent.iter().filter(|r| r["status"]["status"] == json!("done")).collect();
    if done.len() >= 2 { let avg = done.iter().filter_map(|r| r["spent_usd"].as_f64()).sum::<f64>() / done.len() as f64; if avg > 0.0 { o.push_str(&format!("Against the last {} finished runs: ${cost:.3} now, ${avg:.3} on average ({:+.0}%).\n", done.len(), (cost - avg) / avg * 100.0)); } }
    o.push_str("You may reshape the team from this: ask `antiloki_team_audit` for the improve prompt, or start a self-evolution session from the team's top bar.\n"); o
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    /// A scripted world: the experiments come from a list of (admitted, current cost, candidate cost, stage); nothing real runs.
    struct Fake { exps: RefCell<Vec<Value>>, held: usize, tune: usize, told: RefCell<Vec<String>>, decided: RefCell<Vec<(u32, bool)>>, answer: bool }
    impl Hands for Fake {
        fn propose(&self, _p: &str) -> Result<(String, f64), String> { if self.answer { Ok(("{}".into(), 0.01)) } else { Err("seat down".into()) } }
        fn open(&self, _r: &str) -> Result<(u32, String, Value), String> { let n = self.decided.borrow().len() as u32 + 1; Ok((n, format!("hypothesis {n}: replace the drafting agent with a code node"), json!([{ "op": "add_node", "node": { "node": "code" } }]))) }
        fn experiment(&self, _n: u32, _b: f64) -> Result<(Value, Value), String> { let mut e = self.exps.borrow_mut(); if e.is_empty() { return Err("none left".into()); } Ok((e.remove(0), json!({}))) }
        fn decide(&self, n: u32, a: bool, _w: &str) -> Result<(), String> { self.decided.borrow_mut().push((n, a)); Ok(()) }
        fn tasks(&self) -> (usize, usize) { (self.tune, self.held) } fn stopped(&self) -> bool { false } fn now(&self) -> i64 { 1000 }
        fn brief(&self, c: &Cfg, t: &[Trial], n: u32) -> String { trial_brief(c, t, n) } fn tell(&self, r: &str) { self.told.borrow_mut().push(r.to_string()); }
    }
    fn arm(cost: f64) -> Value { json!({ "n": 9, "passed": 9, "rate": 1.0, "lower": 0.8, "avg_cost_usd": cost, "avg_wall_ms": 1000.0, "gate_stops": 0 }) }
    fn exp(admitted: bool, cur: f64, cand: f64, spent: f64) -> Value { json!({ "stage": "full", "current": arm(cur), "candidate": arm(cand), "admitted": admitted, "why": if admitted { "admitted: 30% better on cost" } else { "the candidate is only 5% better on cost" }, "spent_usd": spent }) }
    fn world(exps: Vec<Value>) -> Fake { Fake { exps: RefCell::new(exps), held: 3, tune: 2, told: RefCell::new(vec![]), decided: RefCell::new(vec![]), answer: true } }
    fn root() -> String { let d = std::env::temp_dir().join(format!("antiloki-evolve-{}-{}", std::process::id(), crate::cli::now_ms())); std::fs::create_dir_all(&d).unwrap(); d.to_string_lossy().to_string() }
    #[test] fn a_session_stops_at_the_first_trial_that_meets_the_goal_approves_only_that_one_and_tells_the_leader() {
        let w = world(vec![exp(false, 0.10, 0.095, 0.2), exp(true, 0.10, 0.06, 0.2), exp(true, 0.10, 0.05, 0.2)]); let r = root();
        let s = run_with(&w, &r, "T", &Cfg::default()); assert_eq!(s.status, "goal_met"); assert_eq!(s.trials.len(), 2, "the third trial never ran"); assert_eq!(s.trials[0].outcome, "not_admitted"); assert_eq!(s.trials[1].outcome, "admitted"); assert!((s.trials[1].gain_pct - 40.0).abs() < 1e-6);
        assert_eq!(*w.decided.borrow(), vec![(1, false), (2, true)]); assert!(s.spent_usd > 0.4 && s.spent_usd < 0.5, "{}", s.spent_usd); assert_eq!(w.told.borrow().len(), 1); assert!(w.told.borrow()[0].contains("GOAL MET") && w.told.borrow()[0].contains("hypothesis 2"));
        assert_eq!(session(&r, "T").status, "goal_met"); let _ = std::fs::remove_dir_all(&r);
    }
    #[test] fn five_failed_trials_end_it_with_a_report_and_nothing_changed() {
        let w = world((0..9).map(|_| exp(false, 0.10, 0.09, 0.1)).collect()); let r = root(); let s = run_with(&w, &r, "T", &Cfg::default());
        assert_eq!(s.status, "exhausted"); assert_eq!(s.trials.len(), 5); assert!(w.decided.borrow().iter().all(|(_, a)| !*a), "nothing was approved"); assert!(s.report.contains("all trials used") && s.report.contains("Nothing was changed")); let _ = std::fs::remove_dir_all(&r);
        let mut c = Cfg::default(); c.max_trials = 2; let w = world((0..9).map(|_| exp(false, 0.10, 0.09, 0.1)).collect()); assert_eq!(run_with(&w, &root(), "T", &c).trials.len(), 2, "the number of trials is the person's");
    }
    #[test] fn the_purse_ends_it_and_so_do_an_empty_bank_a_silent_improver_and_a_stop() {
        let mut c = Cfg::default(); c.budget_usd = 0.3; let w = world((0..9).map(|_| exp(false, 0.10, 0.09, 0.25)).collect()); let s = run_with(&w, &root(), "T", &c); assert_eq!(s.status, "budget"); assert!(s.trials.len() <= 2);
        let mut w = world(vec![]); w.held = 0; let s = run_with(&w, &root(), "T", &Cfg::default()); assert_eq!(s.status, "needs_tasks"); assert!(s.report.contains("NOT STARTED") && s.trials.is_empty());
        let mut w = world(vec![]); w.answer = false; assert_eq!(run_with(&w, &root(), "T", &Cfg::default()).status, "error");
    }
    #[test] fn the_goal_the_gain_the_trials_and_the_purse_are_held_to_what_makes_sense_and_merged_from_what_is_asked() {
        let c = Cfg { goal: "faster".into(), min_gain_pct: 500.0, max_trials: 99, budget_usd: -3.0, repeats: 0, seat: "claude-code:haiku".into() }.tidy().unwrap(); assert_eq!((c.goal.as_str(), c.min_gain_pct, c.max_trials, c.budget_usd, c.repeats), ("time", 90.0, 20, 0.05, 1));
        assert!(Cfg { goal: "shinier".into(), ..Cfg::default() }.tidy().is_err() && Cfg { seat: "nonsense".into(), ..Cfg::default() }.tidy().is_err());
        let m = merge_cfg(&Cfg::default(), &json!({ "min_gain_pct": 35.0, "max_trials": 8, "goal": "quality" })); assert_eq!((m.min_gain_pct, m.max_trials, m.goal.as_str(), m.budget_usd), (35.0, 8, "quality", 2.0));
        assert_eq!((Cfg::default().min_gain_pct, Cfg::default().max_trials), (20.0, 5), "the defaults are 20% and 5 tries");
    }
    #[test] fn the_brief_tells_the_goal_the_kinds_of_change_and_what_was_already_tried() {
        let t = vec![Trial { n: 1, kind: "model".into(), hypothesis: "cheaper tier".into(), outcome: "not_admitted".into(), why: "only 4% better".into(), ..Default::default() }]; let b = trial_brief(&Cfg::default(), &t, 2);
        for k in ["deterministic", "flow", "decompose", "model", "specialist", "branch"] { assert!(b.contains(&format!("- {k}:")), "{k}"); } assert!(b.contains("at least 20% better on cost") && b.contains("do not propose these again") && b.contains("cheaper tier") && b.contains("only 4% better"));
        assert_eq!(kind_of("Replace the drafting agent with a deterministic check", &Value::Null), "deterministic"); assert_eq!(kind_of("run the two reviewers in parallel", &Value::Null), "flow"); assert_eq!(kind_of("use a cheaper tier", &Value::Null), "model");
    }
    #[test] fn the_improver_is_given_the_team_the_patch_grammar_the_costs_and_the_tests() {
        let g = crate::team_graph::Graph::from_json(&json!({ "team": "T", "schema": 1, "branch": "", "budget_usd": 0.5, "start": ["a"], "nodes": { "leader": { "node": "leader" }, "a": { "node": "llm", "seat": "claude-code:haiku", "prompt": "Write about {ask}", "answer": { "text": "string" } } }, "edges": [] })).unwrap();
        let runs = vec![json!({ "id": "r1", "status": { "status": "done" }, "spent_usd": 0.02, "node_cost": { "a": 0.02 } })]; let b = evidence_block(&g, &runs, &["TeaBox: a tea subscription".to_string()]);
        for want in ["Write about {ask}", "claude-code:haiku", "set_field", "remove_edge", "add_node", "r1 total $0.0200", "- a: $0.0200", "TeaBox"] { assert!(b.contains(want), "{want}\n{b}"); }
    }
    #[test] fn a_trial_carries_the_gain_with_the_noise_counted_for_the_whisker() {
        let a = |c: f64, sd: f64| json!({ "n": 6, "passed": 6, "rate": 1.0, "lower": 0.8, "avg_cost_usd": c, "avg_wall_ms": 1000.0, "gate_stops": 0, "sd_cost_usd": sd, "sd_wall_ms": 0.0 });
        let exp = json!({ "stage": "full", "current": a(1.0, 0.3), "candidate": a(0.76, 0.1), "admitted": false, "why": "x" }); let j = judge(Goal::Cost, &exp, &Value::Null);
        assert!((j.gain_pct - 24.0).abs() < 0.01); assert!(j.sure_pct < j.gain_pct - 5.0, "the noise takes a share off: {} vs {}", j.sure_pct, j.gain_pct);
        let out = judge(Goal::Cost, &json!({ "stage": "screened_out", "admitted": false }), &Value::Null); assert_eq!((out.gain_pct, out.sure_pct, out.outcome), (0.0, 0.0, "screened_out"));
    }
    #[test] fn a_gain_that_shows_in_only_one_half_of_the_runs_is_not_admitted() {
        use crate::team_experiment::{run, Outcome, Plan};
        let task = |id: &str| crate::team_replay::Task { oracle: vec![], id: id.into(), ask: id.into(), checks: vec![], from_run: String::new(), added_ms: 0, split: "heldout".into() };
        let held = vec![task("h1"), task("h2")]; let tune = vec![task("a")]; let p = Plan { repeats: 4, budget_usd: 10.0, regression_left: 0.1, goal: Goal::Cost, min_gain: 0.2 };
        let mut k = 0u32; let r = run(&tune, &held, &p, &mut |arm, _| { if arm == "candidate" { k += 1; } Ok(Outcome { pass: true, cost_usd: if arm == "current" { 0.10 } else if k <= 4 { 0.05 } else { 0.10 }, wall_ms: 1000, gate_stop: false }) }).unwrap();
        assert!(!r.admitted && (r.why.contains("not stable") || r.why.contains("noise")), "{}", r.why);
        let r = run(&tune, &held, &p, &mut |arm, _| Ok(Outcome { pass: true, cost_usd: if arm == "current" { 0.10 } else { 0.06 }, wall_ms: 1000, gate_stop: false })).unwrap(); assert!(r.admitted, "{}", r.why);
    }
    #[test] fn a_run_report_says_where_the_money_went_why_it_stopped_and_how_it_compares() {
        let snap = json!({ "id": "r9", "ask": "write the post", "state": { "status": { "status": "blocked", "why": "the check failed twice" }, "spent_usd": 0.5, "started_ms": 0.0, "now_ms": 60000.0, "node_cost": { "writer": 0.4, "check": 0.1 } } });
        let recent = vec![json!({ "status": { "status": "done" }, "spent_usd": 0.25 }), json!({ "status": { "status": "done" }, "spent_usd": 0.25 })]; let r = run_report("T", &snap, &recent);
        assert!(r.contains("run r9") && r.contains("blocked") && r.contains("writer: $0.400 (80%)") && r.contains("the check failed twice") && r.contains("+100%"), "{r}");
    }
}
