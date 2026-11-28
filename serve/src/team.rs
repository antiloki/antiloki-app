//! TEAMS: A PLANNER, A WORKER AND A QA, LINKED (operator 2026-11-10: "I want to be able to link agents with roles, planner and worker, even if they are from different providers. Plan, worker, QA ... if the QA agent disapproves a
//! delivery it is needed to have a fallback, retry loop, with 3 times maximum, and feedbacks each loop").
//!
//! This is the DECISION half and nothing else: given what has happened in a run, what happens next. It is pure, so every rule of the loop is tested without a terminal, a model or a clock. The driving half (typing a prompt into
//! a pane, hearing that its turn ended, reading its diff) only carries these decisions out, and the sagas store records each one.
//!
//! THE FLOW
//!   plan      the planner writes a plan: steps, and the ACCEPTANCE CRITERIA the work will be judged by. Fixed before any work starts, so QA cannot move the goalposts.
//!   work      the worker does it (attempt 1), in its own box.
//!   verify    the QA, who may read and run but never edit, answers every criterion pass or fail WITH EVIDENCE.
//!   fail      the worker gets only what failed (the delta, not the whole transcript) and tries again - at most MAX_ATTEMPTS times in all.
//!   stop      a pass is done. Three rejections, an attempt that changed nothing, an invalid verdict twice, a spent budget: BLOCKED, with every verdict attached, for the person. It never loops silently.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// How many times the work is tried in all (the first attempt and its retries).
pub const MAX_ATTEMPTS: u8 = 3;
/// A QA verdict that does not cover the criteria is asked again this many times before the run stops (the QA's own mistake is not the worker's).
pub const MAX_VERDICT_RETRIES: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role { Planner, Worker, Qa }
impl Role {
    pub fn key(self) -> &'static str { match self { Role::Planner => "planner", Role::Worker => "worker", Role::Qa => "qa" } }
    pub fn parse(s: &str) -> Option<Role> { match s.trim().to_ascii_lowercase().as_str() { "planner" | "plan" => Some(Role::Planner), "worker" | "work" => Some(Role::Worker), "qa" | "verifier" | "review" => Some(Role::Qa), _ => None } }
    /// What the role may do in its box: the contract that makes the roles worth having.
    pub fn may_edit(self) -> bool { matches!(self, Role::Worker) }
}

/// A team: up to one pane per role (for now). A pane belongs to at most one team.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Team { pub id: String, pub name: String, pub planner: Option<i64>, pub worker: Option<i64>, pub qa: Option<i64>,
    /// A PAIR (operator 2026-11-13): not a team and not a single agent. Two agents, one dragged into the other: one plans, the other works, and the PLANNER ALSO JUDGES (it is its own QA). No third member.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub pair: bool }
impl Team {
    /// This team as a graph (RFC 0008 M0): plan → loop {work → qa → if pass} → deliver, three attempts, then the person. Pure.
    pub fn to_graph(&self, budget_usd: f64) -> crate::team_graph::Graph { crate::team_graph::from_team_v1(&self.name, self.planner.is_some(), self.pane(Role::Qa).is_some(), budget_usd) }
    fn raw(&self, r: Role) -> Option<i64> { match r { Role::Planner => self.planner, Role::Worker => self.worker, Role::Qa => self.qa } }
    /// The pane that does a role. In a pair the QA is the planner.
    pub fn pane(&self, r: Role) -> Option<i64> { match r { Role::Qa if self.pair => self.planner, _ => self.raw(r) } }
    /// The members as linked (a pair's planner is listed once, as the planner).
    pub fn members(&self) -> Vec<(Role, i64)> { [Role::Planner, Role::Worker, Role::Qa].into_iter().filter_map(|r| self.raw(r).map(|p| (r, p))).collect() }
    /// Link a pane into a role. One per role: a role that is taken is refused, and so is a pane that already has a role here.
    pub fn link(&mut self, r: Role, pane: i64) -> Result<(), String> {
        if let Some((have, _)) = self.members().into_iter().find(|(_, p)| *p == pane) { return Err(format!("pane {pane} is already this team's {}", have.key())); }
        if self.pair { return Err("this is a pair: two agents, the planner judges: unlink one first to make something else".into()); }
        if let Some(p) = self.raw(r) { return Err(format!("this team's {} is pane {p} already (one per role)", r.key())); }
        match r { Role::Planner => self.planner = Some(pane), Role::Worker => self.worker = Some(pane), Role::Qa => self.qa = Some(pane) }
        Ok(())
    }
    pub fn unlink(&mut self, pane: i64) -> Option<Role> {
        let r = self.members().into_iter().find(|(_, p)| *p == pane).map(|(r, _)| r)?;
        match r { Role::Planner => self.planner = None, Role::Worker => self.worker = None, Role::Qa => self.qa = None }
        Some(r)
    }
    /// A team needs a PLANNER and a WORKER (operator 2026-11-10); the QA is optional (with none, the worker's delivery is accepted as it comes). Says what is missing.
    pub fn ready(&self) -> Result<(), String> {
        if self.worker.is_none() { return Err("the team has no worker: link a pane as the worker".into()); }
        if self.planner.is_none() { return Err("the team has no planner: link a pane as the planner (the QA is the optional one)".into()); }
        Ok(())
    }
}

/// One thing the work is judged by: an id, the sentence, and (optionally) a command that proves it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Criterion { pub id: String, pub text: String, #[serde(default)] pub check: String }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Plan { pub summary: String, #[serde(default)] pub steps: Vec<String>, pub criteria: Vec<Criterion> }

/// Read a plan from what the planner wrote (JSON, as a file or in a fenced block). A plan with no criteria is not a plan: QA would have nothing to hold the work to.
pub fn parse_plan(text: &str) -> Result<Plan, String> {
    let v = json_in(text).ok_or("the plan is not JSON: write {\"summary\", \"steps\": [...], \"criteria\": [{\"id\", \"text\", \"check\"?}]}")?;
    let criteria: Vec<Criterion> = v["criteria"].as_array().into_iter().flatten().enumerate().filter_map(|(i, c)| {
        let text = c["text"].as_str().or_else(|| c.as_str())?.trim().to_string(); if text.is_empty() { return None; }
        Some(Criterion { id: c["id"].as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("c{}", i + 1)), text, check: c["check"].as_str().unwrap_or("").trim().to_string() })
    }).collect();
    if criteria.is_empty() { return Err("the plan has no acceptance criteria: the QA could not judge the work against anything".into()); }
    let mut ids: Vec<&str> = criteria.iter().map(|c| c.id.as_str()).collect(); ids.sort(); let n = ids.len(); ids.dedup();
    if ids.len() != n { return Err("two criteria share an id".into()); }
    Ok(Plan { summary: v["summary"].as_str().unwrap_or("").trim().to_string(), steps: v["steps"].as_array().into_iter().flatten().filter_map(|s| s.as_str().map(|x| x.trim().to_string())).filter(|s| !s.is_empty()).collect(), criteria })
}

/// The QA's answer: every criterion, pass or fail, with the evidence.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Verdict { pub passed: bool, pub results: Vec<CriterionResult>, pub summary: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CriterionResult { pub id: String, pub ok: bool, #[serde(default)] pub evidence: String }

/// Read a verdict and check it against the plan's criteria. The verdict is the plan's criteria answered, so it is PASS only when every one is ok: whatever the QA wrote in `passed` is not trusted over its own results.
/// A criterion left unanswered, or one the plan never had, makes the verdict invalid (the QA is asked again). A failed criterion with no evidence is invalid too: "no" has to say why, or the worker has nothing to fix.
pub fn parse_verdict(text: &str, plan: &Plan) -> Result<Verdict, String> {
    let v = json_in(text).ok_or("the verdict is not JSON: write {\"results\": [{\"id\", \"ok\", \"evidence\"}], \"summary\"}")?;
    let results: Vec<CriterionResult> = v["results"].as_array().into_iter().flatten().filter_map(|r| Some(CriterionResult { id: r["id"].as_str()?.trim().to_string(), ok: r["ok"].as_bool().or_else(|| r["pass"].as_bool())?, evidence: r["evidence"].as_str().unwrap_or("").trim().to_string() })).collect();
    for c in &plan.criteria { if !results.iter().any(|r| r.id == c.id) { return Err(format!("the verdict does not answer criterion `{}`", c.id)); } }
    if let Some(r) = results.iter().find(|r| !plan.criteria.iter().any(|c| c.id == r.id)) { return Err(format!("the verdict answers `{}`, which is not one of the plan's criteria", r.id)); }
    if let Some(r) = results.iter().find(|r| !r.ok && r.evidence.is_empty()) { return Err(format!("criterion `{}` failed with no evidence: say what was seen", r.id)); }
    let passed = results.iter().all(|r| r.ok);
    Ok(Verdict { passed, results, summary: v["summary"].as_str().unwrap_or("").trim().to_string() })
}

/// The plan a planner puts at the end of a reply to the person: a fenced block labelled `team-plan`. `None` when the reply has none (it was only talk). Pure.
pub fn extract_plan_block(answer: &str) -> Option<String> {
    let start = answer.find("```team-plan")?; let rest = &answer[start + "```team-plan".len()..];
    let end = rest.find("```")?; let body = rest[..end].trim(); (!body.is_empty()).then(|| body.to_string())
}
/// What the planner is told alongside whatever the person types to it (the prompt hook adds it as context). Pure.
pub fn planner_note(team_name: &str, has_qa: bool) -> String {
    format!("antiloki team: you are the PLANNER of the team \"{team_name}\". The person talks to you; the worker{} only act on what the team sends them, and you never edit files yourself. If the person's message asks for work, answer in plain words and END your reply with ONE fenced block labelled team-plan:\n```team-plan\n{{\"summary\": \"one sentence\", \"steps\": [\"short steps\"], \"criteria\": [{{\"id\": \"c1\", \"text\": \"what must be true when it is done\", \"check\": \"a command that proves it, if any\"}}]}}\n```\nThe team then carries it out: the worker does it{}. The result comes back to you as a message starting with [team]; tell the person what happened. If the message is only a question or chat, just answer, with no block.", if has_qa { " and the QA" } else { "" }, if has_qa { ", the QA judges every criterion, and a rejection sends the worker only what failed (three attempts at most)" } else { " and its delivery is accepted as it comes" })
}

fn json_in(text: &str) -> Option<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str::<Value>(t) { if v.is_object() { return Some(v); } }
    // a fenced block, or the outermost braces of a reply that talks around the JSON
    let start = t.find('{')?; let end = t.rfind('}')?; if end <= start { return None; }
    serde_json::from_str::<Value>(&t[start..=end]).ok().filter(Value::is_object)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase { Planning, Working, Verifying, Done, Blocked }

/// The run: what was asked, where it is, and everything that has happened (each verdict kept whole).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub task: String, pub phase: Phase,
    /// the attempt the worker is on or has just finished (1-based; 0 before the work starts)
    pub attempt: u8,
    pub plan: Option<Plan>, pub verdicts: Vec<Verdict>, pub diffs: Vec<String>,
    pub verdict_retries: u8, pub spent_usd: f64, pub budget_usd: Option<f64>,
    pub blocked: Option<String>,
    /// roles that are not linked are skipped: no planner means the task is the plan, no QA means the worker's delivery is accepted as it comes
    pub has_planner: bool, pub has_qa: bool,
}

/// What the driver does next.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// ask the planner for a plan
    Plan { prompt: String },
    /// give the worker the work (the first time) or the feedback to fix it (a retry)
    Work { prompt: String, attempt: u8 },
    /// ask the QA to judge the delivery against the criteria
    Verify { prompt: String, attempt: u8 },
    /// ask the QA again: its last answer was not a usable verdict
    VerifyAgain { prompt: String, why: String },
    /// the work is accepted
    Finish,
    /// stop and tell the person why, with everything that was tried
    Block { why: String },
}

/// What happened, as the driver hears it.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    PlanWritten(String),
    /// the worker's turn ended; `diff` is a fingerprint of what it changed in its box (empty when nothing)
    WorkEnded { diff: String },
    VerdictWritten(String),
    Cost(f64),
}

impl Run {
    pub fn new(task: &str, has_planner: bool, has_qa: bool, budget_usd: Option<f64>) -> Run {
        Run { task: task.trim().to_string(), phase: if has_planner { Phase::Planning } else { Phase::Working }, attempt: 0, plan: None, verdicts: vec![], diffs: vec![], verdict_retries: 0, spent_usd: 0.0, budget_usd, blocked: None, has_planner, has_qa }
    }
    /// The first thing to do.
    pub fn start(&mut self) -> Action {
        if self.has_planner { return Action::Plan { prompt: plan_prompt(&self.task) }; }
        self.plan = Some(Plan { summary: self.task.clone(), steps: vec![], criteria: vec![Criterion { id: "c1".into(), text: self.task.clone(), check: String::new() }] });
        self.work_action()
    }
    fn work_action(&mut self) -> Action { self.phase = Phase::Working; self.attempt += 1; let plan = self.plan.clone().unwrap_or_default(); Action::Work { prompt: if self.attempt == 1 { work_prompt(&self.task, &plan) } else { retry_prompt(&self.task, &plan, self.attempt, self.verdicts.last()) }, attempt: self.attempt } }
    fn block(&mut self, why: String) -> Action { self.phase = Phase::Blocked; self.blocked = Some(why.clone()); Action::Block { why } }
    fn over_budget(&self) -> bool { self.budget_usd.map_or(false, |b| self.spent_usd >= b) }

    /// Take one event and say what to do next. A run that is over (done or blocked) answers every event with its own end.
    pub fn on(&mut self, e: Event) -> Action {
        if self.phase == Phase::Done { return Action::Finish; }
        if self.phase == Phase::Blocked { return Action::Block { why: self.blocked.clone().unwrap_or_default() }; }
        match e {
            Event::Cost(c) => { self.spent_usd += c.max(0.0); if self.over_budget() { return self.block(format!("the budget of ${:.2} is spent (${:.2}) before the work was accepted", self.budget_usd.unwrap_or(0.0), self.spent_usd)); } self.pending() }
            Event::PlanWritten(text) if self.phase == Phase::Planning => match parse_plan(&text) {
                Ok(p) => { self.plan = Some(p); self.work_action() }
                Err(e) => { if self.verdict_retries >= MAX_VERDICT_RETRIES { return self.block(format!("the planner twice gave no usable plan: {e}")); } self.verdict_retries += 1; Action::Plan { prompt: format!("{}\n\nYour last plan could not be used: {e}", plan_prompt(&self.task)) } }
            },
            Event::WorkEnded { diff } if self.phase == Phase::Working => {
                if diff.is_empty() { return self.block(format!("attempt {} changed nothing in the worker's box", self.attempt)); }
                // NO PROGRESS: the same change as the attempt before is the same answer to the same feedback
                if self.diffs.last() == Some(&diff) { return self.block(format!("attempt {} made the very same change as attempt {}: asking again will not help", self.attempt, self.attempt - 1)); }
                self.diffs.push(diff);
                if !self.has_qa { self.phase = Phase::Done; return Action::Finish; }
                self.phase = Phase::Verifying; self.verdict_retries = 0;
                Action::Verify { prompt: verify_prompt(&self.task, self.plan.as_ref().unwrap_or(&Plan::default()), self.attempt), attempt: self.attempt }
            }
            Event::VerdictWritten(text) if self.phase == Phase::Verifying => {
                let plan = self.plan.clone().unwrap_or_default();
                match parse_verdict(&text, &plan) {
                    Err(why) => { if self.verdict_retries >= MAX_VERDICT_RETRIES { return self.block(format!("the QA twice gave no usable verdict: {why}")); } self.verdict_retries += 1; Action::VerifyAgain { prompt: format!("{}\n\nYour last verdict could not be used: {why}", verify_prompt(&self.task, &plan, self.attempt)), why } }
                    Ok(v) => {
                        let passed = v.passed; self.verdicts.push(v);
                        if passed { self.phase = Phase::Done; return Action::Finish; }
                        if self.attempt >= MAX_ATTEMPTS { return self.block(format!("the QA rejected the work {MAX_ATTEMPTS} times; what was still wrong: {}", failed_ids(self.verdicts.last()))); }
                        if self.over_budget() { return self.block(format!("the QA rejected attempt {} and the budget is spent", self.attempt)); }
                        self.work_action()
                    }
                }
            }
            // an event that does not belong to this phase is ignored: the same action stays pending
            _ => self.pending(),
        }
    }
    /// The action that is waiting on the current phase (what to do again if asked, e.g. after a restart).
    pub fn pending(&self) -> Action {
        let plan = self.plan.clone().unwrap_or_default();
        match self.phase {
            Phase::Planning => Action::Plan { prompt: plan_prompt(&self.task) },
            Phase::Working => Action::Work { prompt: if self.attempt <= 1 { work_prompt(&self.task, &plan) } else { retry_prompt(&self.task, &plan, self.attempt, self.verdicts.last()) }, attempt: self.attempt.max(1) },
            Phase::Verifying => Action::Verify { prompt: verify_prompt(&self.task, &plan, self.attempt), attempt: self.attempt },
            Phase::Done => Action::Finish,
            Phase::Blocked => Action::Block { why: self.blocked.clone().unwrap_or_default() },
        }
    }
    /// The run as the sagas store and the screen read it.
    pub fn summary(&self) -> Value {
        json!({ "phase": self.phase, "attempt": self.attempt, "max_attempts": MAX_ATTEMPTS, "criteria": self.plan.as_ref().map(|p| p.criteria.len()).unwrap_or(0), "verdicts": self.verdicts.len(), "blocked": self.blocked, "spent_usd": self.spent_usd, "budget_usd": self.budget_usd, "has_planner": self.has_planner, "has_qa": self.has_qa })
    }
}

fn failed_ids(v: Option<&Verdict>) -> String { v.map(|v| v.results.iter().filter(|r| !r.ok).map(|r| r.id.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default() }

pub fn plan_prompt(task: &str) -> String {
    format!("You are the PLANNER of a team. Do not edit any file. Plan this work:\n\n{task}\n\nReply with ONE JSON object and nothing else: {{\"summary\": one sentence, \"steps\": [short strings], \"criteria\": [{{\"id\": \"c1\", \"text\": what must be true when it is done, \"check\": a command that proves it, if there is one}}]}}. The criteria are what the QA will judge the work by, so make each one observable.")
}
pub fn work_prompt(task: &str, plan: &Plan) -> String {
    let steps = if plan.steps.is_empty() { String::new() } else { format!("\n\nThe plan:\n{}", plan.steps.iter().enumerate().map(|(i, s)| format!("{}. {s}", i + 1)).collect::<Vec<_>>().join("\n")) };
    format!("You are the WORKER of a team. Do this work in your box:\n\n{task}{steps}\n\nIt will be judged by a QA against these criteria:\n{}\n\nWhen you are done, say what you changed.", criteria_list(plan))
}
/// The feedback for a retry: only what failed, with the QA's evidence. Not the whole transcript.
pub fn retry_prompt(task: &str, plan: &Plan, attempt: u8, last: Option<&Verdict>) -> String {
    let failed: Vec<String> = last.map(|v| v.results.iter().filter(|r| !r.ok).map(|r| { let text = plan.criteria.iter().find(|c| c.id == r.id).map(|c| c.text.as_str()).unwrap_or(""); format!("- {} ({text}): {}", r.id, r.evidence) }).collect()).unwrap_or_default();
    format!("You are the WORKER of a team. The QA rejected your last delivery (attempt {} of {MAX_ATTEMPTS} comes next). The task was:\n\n{task}\n\nWhat is still wrong:\n{}\n\n{}Fix exactly these. Do not undo what already passed.", attempt, failed.join("\n"), last.filter(|v| !v.summary.is_empty()).map(|v| format!("The QA said: {}\n\n", v.summary)).unwrap_or_default())
}
pub fn verify_prompt(task: &str, plan: &Plan, attempt: u8) -> String {
    format!("You are the QA of a team. Do NOT edit any file: read, run and test only. Judge the worker's delivery (attempt {attempt}) of:\n\n{task}\n\nAnswer EVERY criterion:\n{}\n\nReply with ONE JSON object and nothing else: {{\"results\": [{{\"id\": \"c1\", \"ok\": true|false, \"evidence\": what you saw or ran; required when ok is false}}], \"summary\": one sentence}}.", criteria_list(plan))
}
fn criteria_list(plan: &Plan) -> String { plan.criteria.iter().map(|c| format!("- {}: {}{}", c.id, c.text, if c.check.is_empty() { String::new() } else { format!(" (check: `{}`)", c.check) })).collect::<Vec<_>>().join("\n") }

// ── THE DRIVER: carries the decisions out, through whatever can talk to the three panes ─────────────────────────────────────────────────────────────────────────────────────────────────────────────

/// One reply from a role's pane: the model's final message of its turn, and what the turn cost.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply { pub text: String, pub usd: f64 }

/// What the driver needs of the world. The real one types into panes and reads their boxes; the tests' one is a script.
pub trait Io {
    /// Type `prompt` into the role's pane and wait for its turn to end; its last message. Errors: no such pane, the turn never ended, the person stopped the run.
    fn ask(&mut self, role: Role, prompt: &str) -> Result<Reply, String>;
    /// What the worker's box holds now, as a fingerprint of the change (empty when it changed nothing).
    fn delivery(&mut self) -> Result<String, String>;
    /// Put the worker's delivery into the QA's box (replacing what a last round left there), so the QA reads and runs exactly what was delivered.
    fn hand_over(&mut self) -> Result<(), String>;
    /// Whether the QA's box still holds exactly the delivery: false means the QA EDITED something, which a QA never does.
    fn qa_clean(&mut self) -> Result<bool, String>;
    /// A line for the trail (the sagas store records it).
    fn note(&mut self, line: &str);
    /// The run as it stands, after every decision (the screen and the saga read it). Nothing to do by default.
    fn progress(&mut self, _run: &Run) {}
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome { Done, Blocked(String) }

/// Drive a run from its start to its end. Every decision is the `Run`'s; every effect is the `Io`'s.
pub fn drive(run: &mut Run, io: &mut impl Io) -> Outcome { drive_with(run, io, None) }
/// The same, for a run that starts with the plan already in hand (the planner wrote it in conversation with the person): the planner is not asked for it again.
pub fn drive_with(run: &mut Run, io: &mut impl Io, first_plan: Option<String>) -> Outcome {
    let mut action = run.start();
    if let (Some(text), Action::Plan { .. }) = (first_plan, &action) { io.note("the planner's plan, from the conversation"); action = run.on(Event::PlanWritten(text)); }
    let mut handed_over_for: u8 = 0;
    loop {
        io.note(&describe(&action)); io.progress(run);
        action = match action {
            Action::Finish => return Outcome::Done,
            Action::Block { why } => return Outcome::Blocked(why),
            Action::Plan { prompt } => match io.ask(Role::Planner, &prompt) {
                Err(e) => return stop(run, format!("the planner could not be asked: {e}")),
                Ok(r) => { if let Some(o) = spend(run, r.usd) { return o; } run.on(Event::PlanWritten(r.text)) }
            },
            Action::Work { prompt, .. } => match io.ask(Role::Worker, &prompt) {
                Err(e) => return stop(run, format!("the worker could not be asked: {e}")),
                Ok(r) => { if let Some(o) = spend(run, r.usd) { return o; }
                    match io.delivery() { Ok(diff) => run.on(Event::WorkEnded { diff }), Err(e) => return stop(run, format!("the worker's box could not be read: {e}")) } }
            },
            // a first judging and a second one (the QA's last answer was not a verdict) are the same ask: the delivery is handed over once per attempt
            Action::Verify { prompt, .. } | Action::VerifyAgain { prompt, .. } => {
                if handed_over_for != run.attempt { if let Err(e) = io.hand_over() { return stop(run, format!("the delivery could not be handed to the QA: {e}")); } handed_over_for = run.attempt; }
                match io.ask(Role::Qa, &prompt) {
                    Err(e) => return stop(run, format!("the QA could not be asked: {e}")),
                    Ok(r) => { if let Some(o) = spend(run, r.usd) { return o; }
                        match io.qa_clean() { Ok(true) => run.on(Event::VerdictWritten(r.text)), Ok(false) => return stop(run, "the QA edited files: a QA reads and runs, it never edits, so its verdict is not trusted".into()), Err(e) => return stop(run, format!("the QA's box could not be read: {e}")) } }
                }
            }
        };
    }
}
/// Count what a turn cost; `Some` when that spent the budget (the run is then blocked).
fn spend(run: &mut Run, usd: f64) -> Option<Outcome> { if let Action::Block { why } = run.on(Event::Cost(usd)) { Some(Outcome::Blocked(why)) } else { None } }
fn stop(run: &mut Run, why: String) -> Outcome { run.phase = Phase::Blocked; run.blocked = Some(why.clone()); Outcome::Blocked(why) }
fn describe(a: &Action) -> String {
    match a {
        Action::Plan { .. } => "asked the planner for a plan".into(), Action::Work { attempt, .. } => format!("gave the worker the work (attempt {attempt} of {MAX_ATTEMPTS})"),
        Action::Verify { attempt, .. } => format!("asked the QA to judge attempt {attempt}"), Action::VerifyAgain { why, .. } => format!("asked the QA again: {why}"),
        Action::Finish => "the QA accepted the work".into(), Action::Block { why } => format!("blocked: {why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN_JSON: &str = r#"{"summary":"fix the picker","steps":["read it","fix it"],"criteria":[{"id":"c1","text":"the picker opens","check":"npm test"},{"id":"c2","text":"no console errors"}]}"#;
    const PLAN: &str = r#"Here is my plan: {"summary":"fix the picker","steps":["read it","fix it"],"criteria":[{"id":"c1","text":"the picker opens","check":"npm test"},{"id":"c2","text":"no console errors"}]}"#;
    fn ok_verdict() -> String { r#"{"results":[{"id":"c1","ok":true},{"id":"c2","ok":true}],"summary":"fine"}"#.into() }
    fn bad_verdict(why: &str) -> String { format!(r#"{{"results":[{{"id":"c1","ok":true}},{{"id":"c2","ok":false,"evidence":"{why}"}}],"summary":"c2 fails"}}"#) }
    fn act(a: &Action) -> &'static str { match a { Action::Plan { .. } => "plan", Action::Work { .. } => "work", Action::Verify { .. } => "verify", Action::VerifyAgain { .. } => "verify-again", Action::Finish => "finish", Action::Block { .. } => "block" } }

    #[test]
    fn a_team_has_one_pane_per_role_and_a_pane_has_one_role() {
        let mut c = Team { id: "k".into(), name: "k".into(), ..Default::default() };
        assert!(c.ready().unwrap_err().contains("no worker"));
        c.link(Role::Worker, 4).unwrap(); assert!(c.ready().unwrap_err().contains("no planner"), "a worker alone is not a team");
        c.link(Role::Qa, 6).unwrap(); assert!(c.ready().unwrap_err().contains("no planner"), "a worker and a QA still need the one who plans");
        c.link(Role::Planner, 2).unwrap(); assert!(c.ready().is_ok());
        assert!(c.link(Role::Worker, 9).unwrap_err().contains("one per role")); assert!(c.link(Role::Planner, 4).unwrap_err().contains("already this team's worker"));
        assert_eq!(c.members().iter().map(|(r, p)| (r.key(), *p)).collect::<Vec<_>>(), vec![("planner", 2), ("worker", 4), ("qa", 6)]);
        assert_eq!(c.unlink(6), Some(Role::Qa)); assert!(c.ready().is_ok(), "the QA is optional");
        assert_eq!(c.unlink(4), Some(Role::Worker)); assert_eq!(c.unlink(4), None); assert!(c.ready().is_err());
        assert!(Role::Worker.may_edit() && !Role::Planner.may_edit() && !Role::Qa.may_edit(), "only the worker edits");
        assert_eq!((Role::parse("Plan"), Role::parse("verifier"), Role::parse("boss")), (Some(Role::Planner), Some(Role::Qa), None));
    }

    #[test]
    fn a_plan_needs_criteria_and_is_read_from_a_reply_that_talks_around_it() {
        let p = parse_plan(PLAN).unwrap(); assert_eq!((p.summary.as_str(), p.steps.len(), p.criteria.len(), p.criteria[0].check.as_str()), ("fix the picker", 2, 2, "npm test"));
        assert!(parse_plan("no json here").unwrap_err().contains("not JSON"));
        assert!(parse_plan(r#"{"summary":"x","criteria":[]}"#).unwrap_err().contains("no acceptance criteria"));
        assert!(parse_plan(r#"{"criteria":[{"id":"a","text":"x"},{"id":"a","text":"y"}]}"#).unwrap_err().contains("share an id"));
        assert_eq!(parse_plan(r#"{"criteria":["it works","it is fast"]}"#).unwrap().criteria.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec!["c1", "c2"], "bare sentences get ids");
    }

    #[test]
    fn a_verdict_must_answer_every_criterion_and_a_no_must_say_why_and_passing_is_the_results_not_the_claim() {
        let plan = parse_plan(PLAN).unwrap();
        assert!(parse_verdict(&ok_verdict(), &plan).unwrap().passed);
        let v = parse_verdict(&bad_verdict("console shows TypeError"), &plan).unwrap(); assert!(!v.passed && v.results[1].evidence.contains("TypeError"));
        assert!(parse_verdict(r#"{"results":[{"id":"c1","ok":true}]}"#, &plan).unwrap_err().contains("does not answer criterion `c2`"));
        assert!(parse_verdict(r#"{"results":[{"id":"c1","ok":true},{"id":"c2","ok":true},{"id":"zz","ok":true}]}"#, &plan).unwrap_err().contains("not one of the plan's criteria"));
        assert!(parse_verdict(r#"{"results":[{"id":"c1","ok":true},{"id":"c2","ok":false}]}"#, &plan).unwrap_err().contains("no evidence"));
        let claimed = parse_verdict(r#"{"passed":true,"results":[{"id":"c1","ok":true},{"id":"c2","ok":false,"evidence":"x"}]}"#, &plan).unwrap(); assert!(!claimed.passed, "the QA's own claim does not outrank its results");
        assert!(parse_verdict("lgtm", &plan).unwrap_err().contains("not JSON"));
    }

    #[test]
    fn the_happy_path_plans_works_verifies_and_finishes() {
        let mut r = Run::new("fix the picker", true, true, None);
        assert_eq!(act(&r.start()), "plan");
        assert_eq!(act(&r.on(Event::PlanWritten(PLAN.into()))), "work"); assert_eq!((r.phase, r.attempt), (Phase::Working, 1));
        let v = r.on(Event::WorkEnded { diff: "d1".into() }); assert_eq!(act(&v), "verify"); assert_eq!(r.phase, Phase::Verifying);
        assert_eq!(act(&r.on(Event::VerdictWritten(ok_verdict()))), "finish"); assert_eq!(r.phase, Phase::Done);
        assert_eq!(act(&r.on(Event::WorkEnded { diff: "zz".into() })), "finish", "a finished run stays finished");
    }

    #[test]
    fn a_rejection_sends_the_worker_only_what_failed_and_a_second_attempt_can_pass() {
        let mut r = Run::new("fix the picker", true, true, None); r.start(); r.on(Event::PlanWritten(PLAN.into())); r.on(Event::WorkEnded { diff: "d1".into() });
        let a = r.on(Event::VerdictWritten(bad_verdict("console shows TypeError")));
        let Action::Work { prompt, attempt } = a else { panic!("{a:?}") };
        assert_eq!(attempt, 2); assert!(prompt.contains("c2 (no console errors): console shows TypeError") && !prompt.contains("c1 ("), "only the failed criterion, with its evidence: {prompt}");
        assert!(prompt.contains("attempt 2 of 3"));
        assert_eq!(act(&r.on(Event::WorkEnded { diff: "d2".into() })), "verify");
        assert_eq!(act(&r.on(Event::VerdictWritten(ok_verdict()))), "finish"); assert_eq!(r.verdicts.len(), 2);
    }

    #[test]
    fn three_rejections_block_the_run_for_the_person_with_every_verdict() {
        let mut r = Run::new("t", true, true, None); r.start(); r.on(Event::PlanWritten(PLAN.into()));
        for n in 1..=3u8 {
            assert_eq!(act(&r.on(Event::WorkEnded { diff: format!("d{n}") })), "verify", "attempt {n}");
            let a = r.on(Event::VerdictWritten(bad_verdict("still broken")));
            if n < 3 { assert_eq!(act(&a), "work", "attempt {n} is retried"); } else { let Action::Block { why } = a else { panic!("{a:?}") }; assert!(why.contains("3 times") && why.contains("c2"), "{why}"); }
        }
        assert_eq!((r.phase, r.attempt, r.verdicts.len()), (Phase::Blocked, 3, 3)); assert_eq!(act(&r.on(Event::WorkEnded { diff: "d9".into() })), "block", "a blocked run stays blocked");
    }

    #[test]
    fn an_attempt_that_changes_nothing_or_repeats_the_last_change_blocks_at_once() {
        let mut r = Run::new("t", true, true, None); r.start(); r.on(Event::PlanWritten(PLAN.into()));
        let Action::Block { why } = r.on(Event::WorkEnded { diff: String::new() }) else { panic!() }; assert!(why.contains("changed nothing"));
        let mut r = Run::new("t", true, true, None); r.start(); r.on(Event::PlanWritten(PLAN.into())); r.on(Event::WorkEnded { diff: "same".into() }); r.on(Event::VerdictWritten(bad_verdict("x")));
        let Action::Block { why } = r.on(Event::WorkEnded { diff: "same".into() }) else { panic!() }; assert!(why.contains("very same change"), "{why}");
        assert_eq!(r.verdicts.len(), 1, "no second round of QA for a change that is not a change");
    }

    #[test]
    fn an_unusable_verdict_or_plan_is_asked_for_again_once_and_then_the_run_stops() {
        let mut r = Run::new("t", true, true, None); r.start(); r.on(Event::PlanWritten(PLAN.into())); r.on(Event::WorkEnded { diff: "d1".into() });
        let a = r.on(Event::VerdictWritten("looks good to me".into())); assert_eq!(act(&a), "verify-again"); assert_eq!(r.attempt, 1, "the worker is not asked again for the QA's mistake");
        let Action::Block { why } = r.on(Event::VerdictWritten("still prose".into())) else { panic!() }; assert!(why.contains("twice gave no usable verdict"));
        let mut r = Run::new("t", true, true, None); r.start();
        assert_eq!(act(&r.on(Event::PlanWritten("{\"criteria\":[]}".into()))), "plan"); let Action::Block { why } = r.on(Event::PlanWritten("nope".into())) else { panic!() }; assert!(why.contains("no usable plan"));
        // a good verdict after one bad one still counts, and the retry counter is per verification
        let mut r = Run::new("t", true, true, None); r.start(); r.on(Event::PlanWritten(PLAN.into())); r.on(Event::WorkEnded { diff: "d1".into() }); r.on(Event::VerdictWritten("prose".into()));
        assert_eq!(act(&r.on(Event::VerdictWritten(ok_verdict()))), "finish");
    }

    #[test]
    fn a_spent_budget_stops_the_loop_before_another_attempt() {
        let mut r = Run::new("t", true, true, Some(1.0)); r.start(); r.on(Event::PlanWritten(PLAN.into())); r.on(Event::Cost(0.6)); r.on(Event::WorkEnded { diff: "d1".into() });
        r.on(Event::Cost(0.5)); assert_eq!(r.phase, Phase::Blocked, "spent {} of 1.00", r.spent_usd);
        let mut r = Run::new("t", true, true, Some(1.0)); r.start(); r.on(Event::PlanWritten(PLAN.into())); r.on(Event::WorkEnded { diff: "d1".into() }); r.spent_usd = 1.2;
        let Action::Block { why } = r.on(Event::VerdictWritten(bad_verdict("x"))) else { panic!() }; assert!(why.contains("budget is spent"), "{why}");
    }

    #[test]
    fn a_team_without_a_planner_or_a_qa_skips_that_role() {
        let mut r = Run::new("fix it", false, true, None); let a = r.start(); assert_eq!(act(&a), "work", "no planner: the task is the plan");
        assert_eq!(r.plan.as_ref().unwrap().criteria[0].text, "fix it");
        assert_eq!(act(&r.on(Event::WorkEnded { diff: "d".into() })), "verify");
        let mut r = Run::new("fix it", true, false, None); r.start(); r.on(Event::PlanWritten(PLAN.into()));
        assert_eq!(act(&r.on(Event::WorkEnded { diff: "d".into() })), "finish", "no QA: the delivery is accepted as it comes");
    }

    #[test]
    fn events_out_of_phase_change_nothing() {
        let mut r = Run::new("t", true, true, None); r.start();
        assert_eq!(act(&r.on(Event::VerdictWritten(ok_verdict()))), "plan"); assert_eq!(act(&r.on(Event::WorkEnded { diff: "x".into() })), "plan"); assert_eq!(r.phase, Phase::Planning);
        assert_eq!(r.summary()["max_attempts"], json!(3));
    }

    // ── the driver, against a scripted world ──
    #[derive(Default)]
    struct Fake { plans: Vec<String>, work_diffs: Vec<String>, verdicts: Vec<String>, log: Vec<String>, qa_dirty: bool, fail_ask: Option<Role>, usd: f64, handovers: usize }
    impl Io for Fake {
        fn ask(&mut self, role: Role, prompt: &str) -> Result<Reply, String> {
            if self.fail_ask == Some(role) { return Err("pane 7 is gone".into()); }
            self.log.push(format!("{}:{}", role.key(), prompt.lines().next().unwrap_or("").chars().take(24).collect::<String>()));
            let text = match role { Role::Planner => self.plans.remove(0), Role::Worker => "done".into(), Role::Qa => self.verdicts.remove(0) };
            Ok(Reply { text, usd: self.usd })
        }
        fn delivery(&mut self) -> Result<String, String> { Ok(self.work_diffs.remove(0)) }
        fn hand_over(&mut self) -> Result<(), String> { self.handovers += 1; Ok(()) }
        fn qa_clean(&mut self) -> Result<bool, String> { Ok(!self.qa_dirty) }
        fn note(&mut self, line: &str) { self.log.push(format!("# {line}")); }
    }

    #[test]
    fn the_driver_runs_a_team_that_needs_two_rounds_and_hands_each_delivery_to_the_qa() {
        let mut io = Fake { plans: vec![PLAN.into()], work_diffs: vec!["d1".into(), "d2".into()], verdicts: vec![bad_verdict("TypeError"), ok_verdict()], ..Default::default() };
        let mut run = Run::new("fix the picker", true, true, None);
        assert_eq!(drive(&mut run, &mut io), Outcome::Done);
        assert_eq!((run.attempt, run.verdicts.len(), io.handovers), (2, 2, 2), "each attempt is handed to the QA");
        let roles: Vec<&str> = io.log.iter().filter(|l| !l.starts_with('#')).map(|l| l.split(':').next().unwrap()).collect(); assert_eq!(roles, vec!["planner", "worker", "qa", "worker", "qa"]);
        assert!(io.log.iter().any(|l| l.contains("gave the worker the work (attempt 2 of 3)")) && io.log.iter().any(|l| l.contains("the QA accepted")));
    }

    #[test]
    fn the_driver_stops_after_three_rejections_and_when_the_qa_edits_and_when_a_pane_is_gone() {
        let mut io = Fake { plans: vec![PLAN.into()], work_diffs: vec!["a".into(), "b".into(), "c".into()], verdicts: vec![bad_verdict("x"), bad_verdict("y"), bad_verdict("z")], ..Default::default() };
        let mut run = Run::new("t", true, true, None);
        let Outcome::Blocked(why) = drive(&mut run, &mut io) else { panic!() }; assert!(why.contains("3 times"), "{why}"); assert_eq!((run.verdicts.len(), io.handovers), (3, 3));
        let mut io = Fake { plans: vec![PLAN.into()], work_diffs: vec!["a".into()], verdicts: vec![ok_verdict()], qa_dirty: true, ..Default::default() };
        let Outcome::Blocked(why) = drive(&mut Run::new("t", true, true, None), &mut io) else { panic!() }; assert!(why.contains("QA edited files"), "a QA that edits is not trusted, even with a pass: {why}");
        let mut io = Fake { plans: vec![PLAN.into()], fail_ask: Some(Role::Worker), ..Default::default() };
        let Outcome::Blocked(why) = drive(&mut Run::new("t", true, true, None), &mut io) else { panic!() }; assert!(why.contains("worker could not be asked") && why.contains("pane 7 is gone"), "{why}");
        let mut io = Fake { fail_ask: Some(Role::Planner), ..Default::default() };
        let Outcome::Blocked(why) = drive(&mut Run::new("t", true, true, None), &mut io) else { panic!() }; assert!(why.contains("planner could not be asked"), "{why}");
    }

    #[test]
    fn the_driver_stops_on_a_spent_budget_and_on_an_unchanged_retry() {
        let mut io = Fake { plans: vec![PLAN.into()], work_diffs: vec!["a".into(), "b".into()], verdicts: vec![bad_verdict("x")], usd: 0.5, ..Default::default() };
        let mut run = Run::new("t", true, true, Some(1.2));
        let Outcome::Blocked(why) = drive(&mut run, &mut io) else { panic!() }; assert!(why.contains("budget"), "{why}"); assert!(run.spent_usd >= 1.2);
        let mut io = Fake { plans: vec![PLAN.into()], work_diffs: vec!["same".into(), "same".into()], verdicts: vec![bad_verdict("x")], ..Default::default() };
        let Outcome::Blocked(why) = drive(&mut Run::new("t", true, true, None), &mut io) else { panic!() }; assert!(why.contains("very same change"), "{why}");
    }

    #[test]
    fn a_team_of_a_worker_and_a_qa_needs_no_plan() {
        let mut io = Fake { work_diffs: vec!["a".into()], verdicts: vec![r#"{"results":[{"id":"c1","ok":true}]}"#.into()], ..Default::default() };
        let mut run = Run::new("fix it", false, true, None); assert_eq!(drive(&mut run, &mut io), Outcome::Done);
        assert!(io.log.iter().all(|l| l.starts_with('#') || !l.starts_with("planner")));
    }

    #[test]
    fn a_plan_is_taken_from_the_fenced_block_at_the_end_of_the_planners_reply_and_only_from_there() {
        let reply = format!("Sure, here is how I would do it.\n\n```team-plan\n{PLAN_JSON}\n```\nTell me if you want changes.");
        let block = extract_plan_block(&reply).unwrap(); assert!(parse_plan(&block).is_ok());
        assert_eq!(extract_plan_block("just talk, and a ```json {\"criteria\": []} ``` that is not it"), None);
        assert_eq!(extract_plan_block("```team-plan\n```"), None, "an empty block is no plan"); assert_eq!(extract_plan_block("```team-plan\n{\"x\":1}"), None, "unclosed");
        assert!(planner_note("k", true).contains("QA judges") && !planner_note("k", false).contains("QA judges") && planner_note("k", false).contains("accepted as it comes"));
        assert!(planner_note("k", true).contains("team-plan") && planner_note("k", true).contains("never edit files"));
    }

    #[test]
    fn a_run_that_starts_with_the_plan_in_hand_does_not_ask_the_planner_again() {
        let mut io = Fake { work_diffs: vec!["d1".into()], verdicts: vec![ok_verdict()], ..Default::default() };
        let mut run = Run::new("fix the picker", true, true, None);
        assert_eq!(drive_with(&mut run, &mut io, Some(PLAN.into())), Outcome::Done);
        assert!(io.log.iter().all(|l| l.starts_with('#') || !l.starts_with("planner")), "{:?}", io.log); assert!(io.log.iter().any(|l| l.contains("from the conversation")));
        let mut run = Run::new("t", true, true, None); let mut io = Fake { plans: vec![PLAN.into()], work_diffs: vec!["d".into()], verdicts: vec![ok_verdict()], ..Default::default() };
        // a chat plan that cannot be used is the planner's to redo, once: it is asked, and the run goes on
        assert_eq!(drive_with(&mut run, &mut io, Some("not a plan".into())), Outcome::Done); assert_eq!(io.log.iter().filter(|l| l.starts_with("planner")).count(), 1);
    }
}
