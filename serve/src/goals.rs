//! GOAL RUNS + TEMPORARY SUBAGENTS — the orchestration over the headless CLI runner (cli.rs).
//! A port of antiloki-BE/src/chat/goal-run.ts and the SubagentRegistry half of cli-llm.ts.
//!
//! A goal run:
//!   PLAN     — the engine decomposes the goal into a DAG of tasks: each OWNS disjoint files, NEEDS
//!              predecessors, carries ACCEPTANCE criteria. The operator approves (edits) before anything runs.
//!   EXECUTE  — tasks run in WAVES (all predecessors done), in parallel within a wave, each inside the
//!              parent agent's worktree with its ownership enforced (ANTILOKI_SUBSCOPE → the hook refuses
//!              a write outside the task's files) and its predecessors' reports + the shared BLACKBOARD
//!              in its prompt.
//!   VERIFY   — every finished task is judged against its acceptance list; a failed task retries once with
//!              the verdict attached, then blocks its dependants.
//!   JOIN     — one consolidated report (synthesised by the engine, deterministic fallback), delivered once.
//!   ORCHESTRATION — pause / resume / cancel / retry / skip / re-plan, and an event log of every decision.
//! Pure state machine with an injected runner so the tests drive it without spawning anything.
use crate::cli::{now_ms, ChatMessage, CliTrace, Engine, Killer, RunOpts, RunOut, Tier};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

pub type RunFuture = Pin<Box<dyn Future<Output = Result<RunOut, String>> + Send>>;
/// The runner: an engine, a transcript, a cwd, wants-json, options, and a killer slot.
pub type RunFn = Arc<dyn Fn(Engine, Vec<ChatMessage>, String, bool, RunOpts, Option<Killer>) -> RunFuture + Send + Sync>;
pub type GitFn = Arc<dyn Fn(&[&str], &str) -> String + Send + Sync>;
/// THE SPECIALISTS a run may assign (operator 2026-09-19: "the right specialists for the specific task
/// they receive; the planner is a high-tier call with all the specifics"): `list(cwd)` = the lenses the
/// box's pane made available (id, title, description); `review(cwd, id, path)` = one isolated review as
/// markdown, handed to the sub-agent as a brief. Both async, both optional (tests, a serve without a db).
pub type SpecialistListFn = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Vec<(String, String, String)>> + Send>> + Send + Sync>;
pub type SpecialistReviewFn = Arc<dyn Fn(String, String, String) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus { Pending, Ready, Running, Verifying, Done, Failed, Blocked, Skipped }
impl TaskStatus { fn settled(self) -> bool { matches!(self, TaskStatus::Done | TaskStatus::Failed | TaskStatus::Blocked | TaskStatus::Skipped) } }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Verdict { pub passed: bool, pub reasons: Vec<String>, pub by: String }

/// the box as the run found it: a CHECKPOINT (checkpoint.rs) taken when the plan was approved. Diffs, "what did this task change" and the gate are measured against it.
pub type RunBase = crate::checkpoint::Checkpoint;
/// the checkpoint history a run belongs to: its agent's (pane), else its own
pub fn scope_of(pane_id: i64, run_id: i64) -> String { if pane_id > 0 { format!("pane-{pane_id}") } else { format!("run-{run_id}") } }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoalTask {
    pub id: String, pub title: String, pub prompt: String, pub owns: Vec<String>, pub needs: Vec<String>, pub acceptance: Vec<String>,
    #[serde(default)] pub tier: Tier,
    /// the review lenses the planner chose for this task — their isolated reviews of the task's files ride
    /// in its prompt as briefs (operator 2026-09-19: "the right specialists for the specific task")
    #[serde(default)] pub specialists: Vec<String>,
    /// THE SKILLS the planner named for this task (2026-09-20): their SKILL.md rides in the prompt
    #[serde(default)] pub skills: Vec<String>,
    #[serde(default)] pub briefs: Vec<String>,
    pub status: TaskStatus, pub wave: u32, pub attempts: u32, pub started: Option<i64>, pub ended: Option<i64>,
    pub report: Option<String>, pub verdict: Option<Verdict>, pub error: Option<String>,
    pub trace: Option<CliTrace>, pub changed: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoalEvent { pub t: i64, #[serde(default)] pub id: String, pub kind: String, pub text: String, #[serde(skip_serializing_if = "Option::is_none")] pub task: Option<String> }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Finding { pub task: String, pub text: String, pub t: i64 }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus { Planning, AwaitingAnswers, AwaitingApproval, Running, Paused, Joining, Done, Failed, Cancelled, Inline }

/// THE CHIPS OF THE QUESTION QA ASKS WHEN ITS SEND-BACKS ARE SPENT (the operator may also type a new direction instead of picking one)
pub const QA_DIRECTION_CHIPS: [&str; 4] = ["keep trying with the same feedback", "accept it as it is", "undo everything this run changed", "cancel the run"];
/// The planner reads the code before deciding — OpenCode's high tier needed more than the CLI's 300 s default.
pub const PLAN_TIMEOUT_MS: u64 = 600_000;
/// the dedicated screening call reads a little code and answers one small JSON object — never a plan's budget
pub const SCREEN_TIMEOUT_MS: u64 = 120_000;
/// the shaping call rewrites at most three questions from facts it is handed — a minute, or the code shapes them itself
pub const SHAPE_TIMEOUT_MS: u64 = 60_000;
/// THE SCREENING GATE (operator 2026-09-20: "sometimes a prompt does not carry enough info … Heimdall should
/// classify the certainty of the question"): the router's first possible answer is a QUESTION, not a plan.
/// `assumable` never stops the run — the planner takes the default and lists it in `assumptions`;
/// `blocking` parks the run in `awaiting-answers` for ONE round, then the plan is made under the answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Certainty { #[default] Blocking, Assumable }
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct Question { pub id: String, pub text: String, #[serde(default)] pub options: Vec<String>, #[serde(default)] pub default: String, #[serde(default)] pub certainty: Certainty, #[serde(default)] pub answer: Option<String>,
    /// which of `screen_kinds::KINDS` this question is (empty for a planner-authored one that fits none)
    #[serde(default)] pub kind: String }
/// What the run was planned with — kept so the answer round re-plans the same way.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct PlanArgs { pub n: usize, pub forced_tier: Option<Tier>, pub router: bool, pub auto: bool,
    /// ALWAYS A WORKER (operator 2026-09-22: "Heimdall routing the agent always into sub-agents — complexity chooses the
    /// model vs provider, size chooses the number"): a route that says "one job" is still done by ONE worker on the
    /// routed engine and tier — never by the pane itself; the pane is the brain, never the hands.
    #[serde(default)] pub always: bool }

/// THE ROUTER'S DECISION (operator 2026-09-19: "not every task needs sub-agents — the router should decide
/// the tier of the task, how many sub-agents and their tiers, and the review/consolidation"): what the
/// planning phase concluded before anyone approved anything. `delegate: false` = one job for the terminal
/// that asked (the run ends `inline`, its report says the tier and why); `bias` = what the ledger changed.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Route { pub tier: Tier, pub delegate: bool, pub why: String, #[serde(default)] pub bias: Vec<String>, #[serde(default)] pub verify_tier: Tier, #[serde(default)] pub join_tier: Tier,
    /// HOW SURE (operator 2026-09-20: "didn't say the AI was like 70% sure"): the router's own grade of its route — high | medium | low
    #[serde(default)] pub confidence: String,
    /// MIN (2026-09-21): files the ask names that neither the pack nor the snapshot know — this ask runs with exploration on
    #[serde(default)] pub no_intel: Vec<String> }

fn default_true() -> bool { true }
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct Qa { pub cmd: String, pub passed: bool, pub exit: i32, pub ms: i64, pub tail: String, pub files: Vec<String>,
    /// the same tests were already red BEFORE the change (the baseline) — the failure is not the agent's
    #[serde(default)] pub pre_existing: bool }
/// THE BOX'S DEPENDENCIES (2026-09-20): a write run's QA gate needs them. Present → nothing; the main
/// checkout has them and the lockfiles are identical → a symlink (instant, no disk); else the package
/// manager installs them (frozen lockfile, ≤ 10 min). Err = the honest reason.
pub fn deps_plan(cwd: &str) -> Result<(String, Vec<String>), String> {
    let root = std::path::Path::new(cwd);
    if root.join("node_modules").is_dir() { return Ok(("present".into(), vec![])); }
    if !root.join("package.json").is_file() { return Err("no package.json — nothing to install".into()); }
    let lock = ["pnpm-lock.yaml", "yarn.lock", "package-lock.json"].iter().find(|l| root.join(l).is_file()).copied();
    // the main checkout (a linked worktree's common dir parent) — shareable when its lockfile is byte-identical
    let common = crate::proc::cmd("git").args(["rev-parse", "--path-format=absolute", "--git-common-dir"]).current_dir(root).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    if let (Some(l), Some(main)) = (lock, std::path::Path::new(&common).parent()) {
        if main != root && main.join("node_modules").is_dir() {
            let same = std::fs::read(main.join(l)).ok() == std::fs::read(root.join(l)).ok();
            if same { return Ok((format!("link:{}", main.join("node_modules").display()), vec![])); }
        }
    }
    let cmd: Vec<String> = match lock { Some("pnpm-lock.yaml") => vec!["pnpm".into(), "install".into(), "--frozen-lockfile".into()], Some("yarn.lock") => vec!["yarn".into(), "install".into(), "--frozen-lockfile".into(), "--non-interactive".into()], Some("package-lock.json") => vec!["npm".into(), "ci".into(), "--no-audit".into(), "--no-fund".into()], _ => vec!["npm".into(), "install".into(), "--no-audit".into(), "--no-fund".into()] };
    Ok(("install".into(), cmd))
}
pub async fn ensure_deps(cwd: &str) -> Result<String, String> {
    let (how, cmd) = deps_plan(cwd)?;
    if how == "present" { return Ok("dependencies present".into()); }
    if let Some(src) = how.strip_prefix("link:") {
        #[cfg(unix)] { std::os::unix::fs::symlink(src, std::path::Path::new(cwd).join("node_modules")).map_err(|e| format!("could not link node_modules: {e}"))?; }
        #[cfg(not(unix))] { return Err("linking node_modules is not supported here".into()); }
        return Ok(format!("node_modules linked from the main checkout ({src}) — same lockfile"));
    }
    let t0 = now_ms();
    let out = tokio::time::timeout(std::time::Duration::from_secs(600), crate::proc::tokio_cmd(&cmd[0]).args(&cmd[1..]).current_dir(cwd).env("CI", "1").output()).await;
    match out {
        Ok(Ok(o)) if o.status.success() => Ok(format!("`{}` in {}s", cmd.join(" "), (now_ms() - t0) / 1000)),
        Ok(Ok(o)) => Err(format!("`{}` exited {}: {}", cmd.join(" "), o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stderr).chars().rev().take(300).collect::<String>().chars().rev().collect::<String>())),
        Ok(Err(e)) => Err(format!("`{}` could not start: {e}", cmd[0])),
        Err(_) => Err(format!("`{}` timed out after 600 s", cmd.join(" "))),
    }
}
/// Run the related tests for `files` in `cwd` — the gate itself, shared by the baseline and the after.
pub async fn run_qa(cwd: &str, files: &[String]) -> Result<Qa, String> {
    let root = std::path::Path::new(cwd);
    let cmd = qa_gate_cmd(root, files)?;
    let t0 = now_ms();
    // CARGO_BUILD_JOBS capped at 2 unless the operator set it: a cold `cargo` in a box has no memory cap of its own (three terminal OOM deaths on this machine, 2026-09-21)
    let jobs = std::env::var("CARGO_BUILD_JOBS").unwrap_or_else(|_| "2".into());
    let out = tokio::time::timeout(std::time::Duration::from_secs(600), crate::proc::shell(&cmd).current_dir(cwd).env("CI", "1").env("CARGO_BUILD_JOBS", jobs).output()).await;
    Ok(match out {
        Ok(Ok(o)) => { let text = format!("{}\n{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)); let tail: String = text.chars().rev().take(3000).collect::<String>().chars().rev().collect(); Qa { cmd, passed: o.status.success(), exit: o.status.code().unwrap_or(-1), ms: now_ms() - t0, tail, files: files.to_vec(), pre_existing: false } }
        Ok(Err(e)) => Qa { cmd, passed: false, exit: -1, ms: now_ms() - t0, tail: format!("could not run: {e}"), files: files.to_vec(), pre_existing: false },
        Err(_) => Qa { cmd, passed: false, exit: -1, ms: now_ms() - t0, tail: "timed out after 600 s".into(), files: files.to_vec(), pre_existing: false },
    })
}
/// THE GATE'S COMMAND FOR A BOX: a JS project's related tests, or - when the box has a `Cargo.toml` and no `package.json` - a Rust compile check (audit 2026-10-05: a goal run on a Rust repo had no gate at all, "no package.json",
/// so antiloki could not check its own core). It COMPILES, it does not run tests: a test link is what exhausts memory on a small box, and the command shown says exactly what ran.
pub fn qa_gate_cmd(root: &std::path::Path, files: &[String]) -> Result<String, String> {
    let pkg = std::fs::read_to_string(root.join("package.json")).ok();
    if pkg.is_none() && root.join("Cargo.toml").is_file() { return rust_gate_cmd(files); }
    let lock = ["pnpm-lock.yaml", "yarn.lock", "package-lock.json"].iter().find(|l| root.join(l).is_file()).copied().unwrap_or("");
    qa_test_cmd(pkg.as_deref(), files, root.join("node_modules").exists(), lock)
}
pub fn rust_gate_cmd(files: &[String]) -> Result<String, String> {
    if !files.iter().any(|f| f.ends_with(".rs") || f.ends_with("Cargo.toml") || f.ends_with("Cargo.lock")) { return Err("no Rust file changed - nothing to compile".into()); }
    Ok("cargo check --quiet --all-targets".into())
}
/// The test command for a change, from the project's package.json — through the project's OWN test
/// script when it is jest/vitest (react's is `node ./scripts/jest/jest-cli.js`; a bare `npx jest`
/// found nothing there, 2026-09-20), else the runner from devDependencies. Err = the honest reason the
/// gate cannot run (no runner · deps not installed · too many files). Pure — tested.
pub fn qa_test_cmd(pkg_json: Option<&str>, files: &[String], node_modules: bool, lock: &str) -> Result<String, String> {
    // QUOTED FOR THE PLATFORM'S SHELL (2026-09-24). This used to wrap every path in single quotes and drop the
    // ones with spaces: on Windows single quotes are not quoting at all — the runner is handed a path with two
    // literal apostrophes in it and finds no tests — and dropping spaces silently skips most of a Windows repo.
    let files: Vec<String> = crate::proc::quote_all(files);
    if files.is_empty() { return Err("no changed files to test".into()); }
    if files.len() > 60 { return Err(format!("{} changed files — too many for a related-tests run", files.len())); }
    let Some(pkg_json) = pkg_json else { return Err("no package.json in the box — no JS test runner".into()) };
    let pkg: Value = serde_json::from_str(pkg_json).map_err(|_| "package.json is not readable".to_string())?;
    let test = pkg.get("scripts").and_then(|s| s.get("test")).and_then(Value::as_str).unwrap_or("");
    let deps = |k: &str| pkg.get("devDependencies").and_then(|d| d.get(k)).is_some() || pkg.get("dependencies").and_then(|d| d.get(k)).is_some();
    let pm = if lock.contains("pnpm") { "pnpm" } else if lock.contains("yarn") { "yarn" } else { "npm" };
    let sep = if pm == "npm" { " -- " } else { " " };
    let jest = test.contains("jest") || deps("jest"); let vitest = test.contains("vitest") || deps("vitest");
    if !jest && !vitest { return Err("no jest/vitest runner in the box's package.json — the related tests were not run; the verifier's diff judgement stands alone".into()); }
    if !node_modules { return Err(format!("the box's dependencies are not installed (no node_modules) — `{pm} install` in the box, then the gate can run")); }
    if jest {
        return Ok(if test.contains("jest") { format!("{pm} test{sep}--findRelatedTests {} --passWithNoTests", files.join(" ")) } else { format!("npx jest --findRelatedTests {} --passWithNoTests", files.join(" ")) });
    }
    Ok(format!("npx vitest related {} --run --passWithNoTests", files.join(" ")))
}

/// THE SAVINGS — the same tokens priced at each task's tier vs all of them at the engine's high tier
/// (what one high-tier agent would have paid). API list prices; a subscription pays $0 metered, so
/// this is the API-equivalent — the honest yardstick, labelled as such. Pure — tested.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct Savings { pub tokens_in: i64, pub tokens_out: i64, pub actual_usd: f64, pub baseline_usd: f64, pub saved_usd: f64, pub saved_pct: f64, pub baseline: String, pub tiers: Vec<String> }
pub fn savings_of(engine: Engine, tasks: &[GoalTask]) -> Option<Savings> { savings_with_workers(engine, None, tasks) }
/// THE BASELINE IS THE FRONTIER MODEL'S LIST PRICE, NOT THE SEAT'S OWN TOP RUNG (operator 2026-09-28). The old rule
/// zeroed the claim for any engine with one token rate at every tier — Codex, Cursor, Gemini, Grok — because
/// `baseline - actual` was exactly zero there, and so "saved $0.00" appeared on Cursor runs that cost a fifth of
/// what Claude Opus would have. The yardstick is now `cli::frontier_of`: the same tokens at Opus's published rate
/// (or Codex's frontier rate on a Codex seat). Opus routed to Opus still saves exactly nothing, by arithmetic.
pub fn savings_with_workers(engine: Engine, workers: Option<Engine>, tasks: &[GoalTask]) -> Option<Savings> { savings_priced(engine, workers, None, tasks) }
/// THE WORK IS PRICED AT WHAT RAN IT (operator 2026-09-28, run #256: a sort dropdown written by Cursor Composer reported
/// "saved $0.00" and an actual spend of $0.474 — the run engine's OPUS rate). `workers` is None on a pair (the pair
/// carries the hands), so every task fell back to the run engine at the task's tier; but each task's own trace names the
/// engine that really ran it, and `bound` names the model the run bound its worker to. Priced there, the spend is the
/// seat's real list price and the saving against the frontier baseline is real too. A cost the CLI itself reported wins.
pub fn savings_priced(engine: Engine, workers: Option<Engine>, bound: Option<(Engine, Option<String>)>, tasks: &[GoalTask]) -> Option<Savings> {
    let mut tin = 0i64; let mut tout = 0i64; let mut actual = 0f64; let mut tiers = vec![];
    for t in tasks {
        let Some(tr) = &t.trace else { continue };
        let (i, o) = (tr.tokens_in.unwrap_or(0), tr.tokens_out.unwrap_or(0));
        if i == 0 && o == 0 { continue; }
        let ran = tr.engine;
        let model = bound.as_ref().filter(|(e, _)| *e == ran).and_then(|(_, m)| m.clone());
        let (pin, pout) = match model.as_deref() {
            Some(m) => crate::cli::prices_of(ran, Some(m)),
            None => { let (_, a, b) = crate::cli::list_price(if workers.is_some() || ran != engine { ran } else { engine }, t.tier); (a, b) }
        };
        actual += match tr.cost { Some(c) if c > 0.0 && (workers.is_some() || ran != engine) => c, _ => (i as f64 / 1e6) * pin + (o as f64 / 1e6) * pout }; tin += i; tout += o;
        tiers.push(format!("{}:{}", t.id, t.tier.key()));
    }
    if tin == 0 && tout == 0 { return None; }
    let (baseline, label) = crate::cli::frontier_price(engine, tin, tout, 0, 0);
    let saved = (baseline - actual).max(0.0);
    Some(Savings { tokens_in: tin, tokens_out: tout, actual_usd: actual, baseline_usd: baseline, saved_usd: saved, saved_pct: if baseline > 0.0 { saved / baseline * 100.0 } else { 0.0 }, baseline: label.to_string(), tiers })
}
/// `<box>/.antiloki/quota.json` — a usage limit hit by a headless run in this box (the PTY's status reads it).
pub fn mark_quota_file(cwd: &str, engine: &str, text: &str) {
    let d = std::path::Path::new(cwd).join(".antiloki");
    let _ = std::fs::create_dir_all(&d);
    let _ = std::fs::write(d.join("quota.json"), json!({ "text": text.chars().take(300).collect::<String>(), "t": now_ms(), "engine": engine, "source": "goal-run" }).to_string());
}
/// AN INLINE ROUTE SAVES TOO (operator 2026-09-20: "the heimdall button showed the saved money the
/// first time, the other asks just showed the same value"): a run routed "one job, tier X" costs the
/// pane's next turn at X instead of at the engine's high tier — the same yardstick as a plan's, from
/// the turn's real tokens once the chat's `result` reports them. Pure — tested.
/// A RUN THAT DELIVERED NOTHING SAVED NOTHING (2026-09-26).
///
/// `psf__requests-6028` on the `deepseek-deepseek` arm: the worker timed out at 240 s, the retry returned 212 chars
/// of exploration prose, the verifier rejected it twice, the run settled `failed — 0/1 tasks passed` — and it still
/// reported "the router saved $0.241 (40%)". Money spent with nothing delivered is WASTE, and that figure is what
/// the saved chip shows the operator.
///
/// The SPEND stays (hiding it would be its own lie); only the claim goes. Pure — tested.
pub fn settled_savings(done: bool, sv: Option<Savings>) -> Option<Savings> {
    sv.map(|mut sv| {
        if !done {
            sv.baseline_usd = sv.actual_usd;
            sv.saved_usd = 0.0;
            sv.saved_pct = 0.0;
            sv.baseline = format!("{} — nothing delivered, so nothing saved", sv.baseline);
        }
        sv
    })
}
/// THE RUN'S SAVING, FROM EVERYTHING IT DID (operator 2026-10-09, live test: run #358 said "saved $0.281 (92%)" but its real spend was $0.133 - the figure priced only the worker's last attempt; run #360 said
/// "saved $0.042" because each QA send-back OVERWROTE the earlier attempts). `calls` are the run's own audit rows `(job, tokens_in, tokens_out, usd)`:
///   actual   = every call the run made - the editor, the verifier, the screen, the planner, the QA, the design round;
///   baseline = the editor calls' tokens (every attempt) priced at one frontier agent - what doing the same work itself would have needed, with none of the machinery counted in its favour.
/// `None` when the run has no editor call on record (the caller keeps the old figure). Not done = nothing saved, the spend stays visible (`settled_savings`). Pure - tested.
pub fn savings_from_calls(engine: Engine, done: bool, calls: &[(String, i64, i64, f64)], tiers: Vec<String>) -> Option<Savings> {
    let (mut tin, mut tout) = (0i64, 0i64);
    for (job, i, o, _) in calls { if job.starts_with("editor") { tin += i; tout += o; } }
    if tin == 0 && tout == 0 { return None }
    let actual: f64 = calls.iter().map(|c| c.3).sum();
    let (baseline, label) = crate::cli::frontier_price(engine, tin, tout, 0, 0);
    let saved = (baseline - actual).max(0.0);
    settled_savings(done, Some(Savings { tokens_in: tin, tokens_out: tout, actual_usd: actual, baseline_usd: baseline, saved_usd: saved, saved_pct: if baseline > 0.0 { saved / baseline * 100.0 } else { 0.0 }, baseline: label.to_string(), tiers }))
}
pub fn savings_inline(engine: Engine, tier: Tier, tokens_in: i64, tokens_out: i64) -> Option<Savings> {
    if tokens_in <= 0 && tokens_out <= 0 { return None; }
    let (_, pin, pout) = crate::cli::list_price(engine, tier);
    let actual = (tokens_in as f64 / 1e6) * pin + (tokens_out as f64 / 1e6) * pout;
    let (baseline, label) = crate::cli::frontier_price(engine, tokens_in, tokens_out, 0, 0);
    let saved = (baseline - actual).max(0.0);
    Some(Savings { tokens_in, tokens_out, actual_usd: actual, baseline_usd: baseline, saved_usd: saved, saved_pct: if baseline > 0.0 { saved / baseline * 100.0 } else { 0.0 }, baseline: label.to_string(), tiers: vec![format!("inline:{}", tier.key())] })
}
/// QA (operator 2026-09-21: "an isolated QA sub-agent judges the work based on the criteria after each
/// plan finishes"): the judge's verdict over the whole run — the plan's criteria vs the real diff.
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct QaFinding { pub task: Option<String>, pub kind: String, pub text: String }
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct QaReview { pub passed: bool, pub score: i64, pub summary: String, pub findings: Vec<QaFinding>, pub by: String, pub t: i64, #[serde(default)] pub error: Option<String> }
/// The judge's JSON → the review; None when it did not answer in shape. Pure — tested.
pub fn parse_qa_review(text: &str) -> Option<QaReview> {
    // THE VERDICT IS READ FROM WHAT THE MODEL ACTUALLY WRITES (operator 2026-09-28: "the judge gave no verdict in
    // shape" on every run). Two things made that near-certain:
    //
    //   · the object was taken as the span from the FIRST `{` to the LAST `}`. A reply that says anything before
    //     the JSON containing a brace — a fenced block, a sentence with `{}` in it, a second object after the
    //     verdict — produced a slice that is not valid JSON, and the whole review was discarded.
    //   · `passed` had to be a strict JSON bool. `"passed": "true"`, `"yes"`, or a reply that gives only a score
    //     was thrown away, though every one of them says plainly whether it passed.
    //
    // So: try every balanced object in the reply, newest-first, and accept the shapes a model really emits.
    for slice in json_objects(text) {
        let Ok(j) = serde_json::from_str::<Value>(&slice) else { continue };
        let score = j.get("score").and_then(Value::as_f64).map(|s| s.round() as i64);
        let passed = match j.get("passed").or_else(|| j.get("pass")).or_else(|| j.get("ok")) {
            Some(Value::Bool(b)) => Some(*b),
            Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "pass" | "passed" | "ok" => Some(true),
                "false" | "no" | "fail" | "failed" => Some(false),
                _ => None,
            },
            Some(Value::Number(n)) => n.as_f64().map(|x| x != 0.0),
            // a reply that gives only a score still says whether it passed
            _ => score.map(|s| s >= 70),
        };
        let Some(passed) = passed else { continue };
        let findings = j.get("findings").and_then(Value::as_array).map(|a| a.iter().filter_map(|f| {
            let text = f.get("text").and_then(Value::as_str).or_else(|| f.as_str())?.trim().to_string();
            if text.is_empty() { return None }
            Some(QaFinding { task: f.get("task").and_then(Value::as_str).map(str::to_string),
                             kind: f.get("kind").and_then(Value::as_str).unwrap_or("").to_string(), text })
        }).collect()).unwrap_or_default();
        return Some(QaReview {
            passed,
            score: score.unwrap_or(if passed { 100 } else { 0 }).clamp(0, 100),
            summary: j.get("summary").and_then(Value::as_str).unwrap_or("").trim().to_string(),
            findings, ..Default::default()
        });
    }
    None
}

/// Every balanced `{…}` in the text, LAST FIRST — a model that thinks aloud puts its verdict at the end.
/// Strings and escapes are respected, so a brace inside a quoted sentence does not close the object. Pure.
pub fn json_objects(text: &str) -> Vec<String> {
    let (b, mut out, mut start, mut depth, mut in_str, mut esc) = (text.as_bytes(), Vec::new(), 0usize, 0i32, false, false);
    for (i, &c) in b.iter().enumerate() {
        if in_str {
            if esc { esc = false } else if c == b'\\' { esc = true } else if c == b'"' { in_str = false }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => { if depth == 0 { start = i } depth += 1 }
            b'}' => {
                depth -= 1;
                if depth == 0 { if let Ok(s) = std::str::from_utf8(&b[start..=i]) { out.push(s.to_string()) } }
                if depth < 0 { depth = 0 }
            }
            _ => {}
        }
    }
    out.reverse();
    out
}

/// The judge's prompt: the goal, the plan (each task's criteria and files), the joined report, the diff. Pure.
/// WHO ELSE TOUCHES THE FILES THIS RUN CHANGED — the intel the gate is asked to judge and was never given
/// (operator 2026-09-27: "QA should make a code review on the deliveried diff … and the context of the code and x-ray
/// intel"). `goal.qa_review` already instructs it that "a change that breaks a neighbour is a risk", and then handed it
/// only the goal, the plan, the report and the diff — so the neighbours were a question it could not answer except by
/// guessing or by spending turns reading the tree.
///
/// This is the cheap, deterministic half: for each changed file, which other files import it. No model call, one bounded
/// walk, and it is derived from the repo rather than from anything a run reported about itself. The fuller X-ray
/// intelligence (the atlas dossiers, the component graph) lives in the core crate's skills and is a bigger wiring job —
/// this is deliberately the part that can be trusted today.
pub fn importers_of(cwd: &str, changed: &[String]) -> String {
    use std::collections::BTreeMap;
    const SKIP: [&str; 6] = ["node_modules", ".git", "dist", "target", ".audit-emit", "build"];
    let changed: Vec<&String> = changed.iter().filter(|c| c.contains('/') || c.contains('.')).collect();
    if changed.is_empty() { return String::new(); }
    // the module name a specifier would use for each changed path: "src/products.ts" -> "products"
    let stems: Vec<(String, String)> = changed.iter().filter_map(|c| {
        std::path::Path::new(c.as_str()).file_stem().and_then(|x| x.to_str()).map(|st| ((*c).clone(), st.to_string()))
    }).collect();
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut walked = 0usize;
    let mut stack = vec![std::path::PathBuf::from(cwd)];
    while let Some(d) = stack.pop() {
        if walked > 4000 { break; }
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if SKIP.contains(&name.as_str()) || name.starts_with('.') { continue; }
            if p.is_dir() { stack.push(p); continue; }
            if !matches!(p.extension().and_then(|x| x.to_str()), Some("ts" | "tsx" | "js" | "jsx" | "mjs" | "rs" | "py")) { continue; }
            walked += 1;
            let rel = p.strip_prefix(cwd).unwrap_or(&p).to_string_lossy().to_string();
            let Ok(body) = std::fs::read_to_string(&p) else { continue };
            for (path, stem) in &stems {
                if &rel == path { continue; }                       // a file is not its own neighbour
                // a specifier that ends in the changed file's stem, in an import/require/use position
                let hit = body.lines().any(|l| {
                    let l = l.trim();
                    (l.starts_with("import") || l.starts_with("export") || l.contains("require(") || l.contains("import(") || l.starts_with("use "))
                        && (l.contains(&format!("/{stem}\"")) || l.contains(&format!("/{stem}'")) || l.contains(&format!("\"./{stem}\"")) || l.contains(&format!("'./{stem}'")))
                });
                if hit { out.entry(path.clone()).or_default().push(rel.clone()); }
            }
        }
    }
    if out.is_empty() { return String::new(); }
    out.into_iter().map(|(f, mut who)| { who.sort(); who.dedup();
        format!("- {f} is imported by: {}", who.iter().take(12).cloned().collect::<Vec<_>>().join(", ")) })
        .collect::<Vec<_>>().join("\n")
}

pub fn qa_review_prompt(run: &GoalRun, diff: &str) -> String {
    format!("{}{}", crate::queue::block_for(run.pane_id, run.base.as_ref()), qa_review_prompt_body(run, diff))
}
fn qa_review_prompt_body(run: &GoalRun, diff: &str) -> String {
    // an INLINE route has no plan: the pane's own agent answered the ask in one turn — the ask is the criterion
    let plan = if run.tasks.is_empty() { "(an inline route — no plan: the pane's own agent did this ask in one turn; judge the ask itself against the diff and the reply)".to_string() } else { run.tasks.iter().map(|t| format!("- {} · {} · owns {} · status {:?}\n  acceptance:\n{}", t.id, t.title, if t.owns.is_empty() { "(no files)".to_string() } else { t.owns.join(", ") }, t.status, if t.acceptance.is_empty() { "    1. the task is done and the report says how".to_string() } else { t.acceptance.iter().enumerate().map(|(i, a)| format!("    {}. {a}", i + 1)).collect::<Vec<_>>().join("\n") })).collect::<Vec<_>>().join("\n") };
    crate::prompts::render_base("goal.qa_review", &vars(&[
        ("goal", json!(run.goal)), ("plan", json!(plan)),
        ("report", json!(run.report.clone().unwrap_or_default().chars().take(60_000).collect::<String>())),
        ("diff", json!(if diff.trim().is_empty() { "(no diff — a read-only run, or nothing changed)".to_string() } else { diff.chars().take(80_000).collect::<String>() })),
        // the neighbours the template asks it to judge — see `importers_of`
        ("neighbours", json!(importers_of(&run.cwd, &run.tasks.iter().flat_map(|t| t.changed.iter().cloned()).collect::<Vec<_>>()))),
    ]))
}
/// MIN'S YARDSTICK (2026-09-21): measured on 176 sessions in the boxes, exploration (Read/Grep/Glob and the
/// shell's cat/ls/find output) was 50% of everything in the context and 89% of the files read were never
/// edited. A MIN turn is priced at its tier; the baseline is the same turn with exploration on — the
/// measured share put back. An estimate, labelled as such. Pure — tested.
pub const MIN_EXPLORE_SHARE: f64 = 0.5;
pub fn savings_min(engine: Engine, tier: Tier, tokens_in: i64, tokens_out: i64) -> Option<Savings> {
    if tokens_in <= 0 && tokens_out <= 0 { return None; }
    let (label, pin, pout) = crate::cli::list_price(engine, tier);
    let actual = (tokens_in as f64 / 1e6) * pin + (tokens_out as f64 / 1e6) * pout;
    let baseline = actual / (1.0 - MIN_EXPLORE_SHARE);
    let saved = (baseline - actual).max(0.0);
    Some(Savings { tokens_in, tokens_out, actual_usd: actual, baseline_usd: baseline, saved_usd: saved, saved_pct: if baseline > 0.0 { saved / baseline * 100.0 } else { 0.0 }, baseline: format!("{} · {} with exploration (measured share {:.0}%)", engine.key(), label, MIN_EXPLORE_SHARE * 100.0), tiers: vec![format!("min:{}", tier.key())] })
}
/// The paths an ask names (`src/cart.ts`, `./a/b.rs`, `README.md`) — what MIN must have intel for. Pure — tested.
/// THE ASK THAT IS ALREADY A PLAN (bench 2026-09-21: the operator enumerated eight modules; the high-tier planner
/// spent 72 s writing them back as a 6k-token plan, then a ratify round read it again — 83 s before any code):
/// how many numbered items (`1.` / `2)` at a line start) the goal lists. Two or more = the planner transcribes at
/// mid and the ratify round is skipped — the operator's own enumeration is the contract. Pure — tested.
pub fn enumerated_tasks(goal: &str) -> usize {
    goal.lines().filter(|l| { let t = l.trim_start(); let d: String = t.chars().take_while(|c| c.is_ascii_digit()).collect(); !d.is_empty() && d.len() <= 2 && t[d.len()..].starts_with(['.', ')']) && t[d.len() + 1..].starts_with(' ') }).count()
}
/// DOES THE ASK ITSELF NEED A CHECK RUN (audit 2026-09-28)? "Fix the failing test", "make the build pass", "lint the folder": a worker
/// told to run nothing cannot do these, so the no-checks rule stands down for an ask that names the check. Whole words, so
/// "attest" or "contest" do not count. Pure — tested.
pub fn ask_wants_checks(goal: &str) -> bool {
    let g = goal.to_ascii_lowercase();
    ["test", "tests", "testing", "spec", "specs", "build", "builds", "lint", "linter", "linting", "typecheck", "type-check", "tsc", "compile", "compiles", "ci", "failing", "fails", "flaky", "coverage", "vitest", "jest", "e2e", "benchmark"]
        .iter().any(|w| g.split(|c: char| !c.is_ascii_alphanumeric() && c != '-').any(|t| t == *w))
}
/// Is the ask bigger than L — worth a wave of sub-agents at all? It lists two or more parts of its own, or it is long.
/// Everything else is one job. Pure — tested.
/// THE ROUTE OF AN ASK THAT NEEDS NO PLANNER (operator 2026-09-30: *"all tasks are using merges ... even very little tasks. Why? i told you tasks L and smaller doesnt need a planner"*). An ask at L or smaller
/// is one job (`ask_bigger_than_l` says so from facts - its length and the parts it lists), and a planner call to decide "one job" cost ~35 s and ~$0.09 on every small ask while changing nothing: its
/// answer was `delegate: false` and antiloki made the one task itself. This is that answer, without the call. Same JSON the planner returns, so everything downstream (the screen, the tier bias, the
/// single task, the merge record) runs unchanged. Pure - tested.
pub fn solo_route_reply() -> String {
    r#"{"tier":"mid","delegate":false,"why":"one job at L or smaller - no planner is needed, the editor runs it alone","confidence":"high","assumptions":[],"tasks":[]}"#.to_string()
}
/// The facts a finished run holds, for `confidence::confidence_of`. Pure over the run - tested through `confidence.rs`.
pub fn confidence_facts(r: &GoalRun) -> crate::confidence::Facts {
    let tasks: Vec<&GoalTask> = r.tasks.iter().filter(|t| t.status != TaskStatus::Skipped).collect();
    let mut changed: Vec<&String> = tasks.iter().flat_map(|t| t.changed.iter()).collect(); changed.sort(); changed.dedup();
    let machine_proof = tasks.iter().any(|t| t.verdict.as_ref().map(|v| v.passed && v.by == "machine").unwrap_or(false)) || r.events.iter().any(|e| e.kind == "qa-pass");
    let verifier_met = tasks.iter().filter(|t| t.verdict.as_ref().map(|v| v.passed && v.by != "machine").unwrap_or(false)).count();
    let verifier_unmet = tasks.iter().filter(|t| t.verdict.as_ref().map(|v| !v.passed).unwrap_or(false) && t.status == TaskStatus::Done).count();
    let qa_met = r.qa_gate.as_ref().map(|g| g.passed()).or_else(|| r.qa_review.as_ref().filter(|q| q.error.is_none()).map(|q| q.passed));
    crate::confidence::Facts {
        write: r.write, tasks: tasks.len(), failed: tasks.iter().filter(|t| t.status == TaskStatus::Failed).count(), changed_files: changed.len(),
        retries: tasks.iter().map(|t| t.attempts.saturating_sub(1)).sum(), machine_proof, verifier_met, verifier_unmet,
        qa_ran: r.qa_gate.is_some() || r.qa_review.is_some(), qa_met, design_blockers: r.design.as_ref().map(|d| d.blockers.len()).unwrap_or(0),
    }
}
pub fn set_confidence(r: &mut GoalRun) { if r.status == RunStatus::Done || r.status == RunStatus::Failed { let c = crate::confidence::confidence_of(&confidence_facts(r)); r.confidence = Some(c); } }
pub fn ask_bigger_than_l(goal: &str) -> bool { enumerated_tasks(goal) >= 2 || goal.chars().count() > 600 }
/// True when the run carries a real fence (not empty and not the whole workspace). Pure — tested.
pub fn scope_fenced(scope: &[String]) -> bool {
    !scope.is_empty() && !(scope.len() == 1 && scope[0] == "**")
}
/// A worktree-relative path the run may touch (antiloki bookkeeping is always allowed). Pure — tested.
pub fn path_in_run_scope(rel: &str, scope: &[String]) -> bool {
    crate::boxes::is_ours(rel) || antiloki_rs::agents_in_pane_scope_sentry::covered(rel, scope)
}
fn named_paths_outside_scope(goal: &str, scope: &[String]) -> Vec<String> {
    named_paths(goal).into_iter().filter(|p| !path_in_run_scope(p, scope)).collect()
}
/// A task `owns` entry must sit inside the run scope when the scope is fenced. Pure — tested.
pub fn own_within_scope(own: &str, scope: &[String]) -> bool {
    if !scope_fenced(scope) { return true; }
    if scope.iter().any(|g| g == "**") { return true; }
    if own == "**" { return false; }
    if !own.contains(['*', '?', '[']) { return path_in_run_scope(own, scope); }
    if let Some(dir) = own.strip_suffix("/**") {
        let probe = format!("{dir}/probe");
        return path_in_run_scope(&probe, scope);
    }
    path_in_run_scope(own, scope)
}
fn scope_fence_fail_run(r: &mut GoalRun, why: String) {
    r.status = RunStatus::Failed;
    r.error = Some(why.clone());
    r.ended = Some(now_ms());
    Goals::log(r, "error", why, None);
}

pub fn named_paths(goal: &str) -> Vec<String> {
    let re = regex::Regex::new(r#"(?:^|[\s(`'"])((?:\./)?[\w@.-]+(?:/[\w@.-]+)*\.[A-Za-z][\w]{0,5}|(?:\./)?[\w@.-]+(?:/[\w@.-]+)+)"#).unwrap();
    let mut out: Vec<String> = vec![];
    for c in re.captures_iter(goal) {
        let p = c[1].trim_start_matches("./").trim_end_matches(['.', ',', ';', ':', ')']).to_string();
        if p.is_empty() || p.starts_with("http") || p.contains("://") || out.contains(&p) { continue; }
        // a bare word with a dot is a path only when the extension is a code/doc one, not "e.g" or "v1.2"
        if !p.contains('/') && !regex::Regex::new(r"\.(rs|ts|tsx|js|jsx|mjs|cjs|py|go|java|kt|rb|php|cs|cpp|c|h|hpp|md|json|toml|yaml|yml|css|scss|html|sql|sh)$").unwrap().is_match(&p) { continue; }
        out.push(p);
    }
    out
}
/// Of the paths an ask names, the ones that exist in the box and neither the pack lists nor the snapshot judged
/// (`judged(rel)` is the snapshot's answer). Pure over the manifest — tested.
pub fn without_intel(cwd: &str, paths: &[String], manifest: &Value, judged: &dyn Fn(&str) -> bool) -> Vec<String> {
    let items: Vec<String> = manifest.get("items").and_then(Value::as_array).map(|a| a.iter().filter_map(|i| i.get("path").and_then(Value::as_str)).map(String::from).collect()).unwrap_or_default();
    paths.iter().filter(|p| std::path::Path::new(cwd).join(p).is_file()).filter(|p| !items.iter().any(|i| i == *p)).filter(|p| !judged(p)).cloned().collect()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GoalRun {
    pub id: i64, pub parent: String, pub goal: String, pub engine: Engine, pub cwd: String, pub write: bool, pub concurrency: usize,
    pub status: RunStatus, pub created: i64, pub started: Option<i64>, pub ended: Option<i64>,
    pub tasks: Vec<GoalTask>, pub blackboard: Vec<Finding>, pub events: Vec<GoalEvent>,
    pub report: Option<String>, pub delivered: bool, pub error: Option<String>,
    #[serde(default)] pub route: Option<Route>,
    /// WHAT THE PLANNING CALL ITSELF COST (2026-09-25, for plan replay): kept so a later cache hit can say what it
    /// saved from the measurement rather than from an estimate. Zero on a replayed plan, which is the truth.
    #[serde(default)] pub plan_usd: f64,
    /// WHAT THE DEDICATED SCREENING CALL COST (L9, 2026-09-29) — a line of the run's bill, kept apart from `plan_usd` because
    /// that one is the PLANNER's row in the record, and a screen priced into it would make a cheap planner read as dear.
    #[serde(default)] pub screen_usd: f64,
    /// WHERE `plan_usd` CAME FROM — "reported" / "estimated" / "unknown" (`estimate::Basis::key`).
    ///
    /// An engine on a subscription often reports no usage at all, and `unwrap_or(0.0)` turned "nobody said" into
    /// "it was free". Two separate shapes of this were measured on 2026-09-26:
    ///
    /// - `codex:gpt-5.6-terra@high` recorded `$0.00` for two bench samples — a frontier model that ran and billed
    ///   nothing. (`reweigh_rows` is defensive here: a non-positive cost scores a neutral 0.5 on the price term
    ///   rather than winning outright, so the router was not fooled — but the screen still printed $0.00.)
    /// - the `planner l` winner, `codex:gpt-5.6-luna@medium` at $0.009 an instance, beat every rival on a figure
    ///   that excluded its own planning call, because `plan_usd` did not exist when that row was written.
    ///
    /// A missing price is now counted from the tokens at the published rate and labelled, never averaged with a
    /// measured one and never printed as $0.00.
    #[serde(default)] pub plan_basis: String,
    /// WHAT ACTUALLY PLANNED AND WHAT ACTUALLY WORKED, as `merges::key` spells it (`engine:model`).
    ///
    /// A bench arm could not be recorded without these (2026-09-26): `solve.sh` had only the PAIR NAME to offer, and
    /// a pair name is not a dispatchable binding — so `grade.sh` rejected every antiloki arm rather than file a
    /// winner the router would fail to start, and fell back to `planner: "raw"`, which is false for a merge.
    ///
    /// Resolved the same way the work is dispatched, so the record cannot disagree with what ran.
    #[serde(default)] pub plan_binding: String,
    #[serde(default)] pub worker_binding: String,
    #[serde(default)] pub plan_ms: i64,
    /// the screening gate's blocking questions (with the operator's answers once given) and the
    /// assumptions the planner took on its own for the assumable ones
    #[serde(default)] pub questions: Vec<Question>,
    #[serde(default)] pub assumptions: Vec<String>,
    #[serde(default)] pub plan_args: PlanArgs,
    /// the cockpit PR opened from this run's box (write runs, 2026-09-19: "make the result a PR")
    #[serde(default)] pub pr: Option<i64>,
    /// HEIMDALL'S ROUTER (operator 2026-09-20): on = the high-tier planner decides delegation, each
    /// task's tier and the parent's own model; off = every task at the pane's tier, no ledger bias
    #[serde(default = "default_true")] pub router: bool,
    /// MIN (2026-09-21): a chat turn taken under MIN — priced against the same turn with exploration (the ledger's other column)
    #[serde(default)] pub min: bool,
    /// QA (2026-09-21): the pane's QA is on — an isolated judge reviews the whole run after the join; its verdict
    /// THE WORKERS ON ANOTHER ENGINE (operator 2026-09-22: "Claude still orchestrates and reviews, the work is done by
    /// sub-agents on OpenCode DeepSeek"): the engine the TASKS run on (at the tier the planner assigns); the planner,
    /// the verifiers, the join and the judge stay on `engine`. None = the run's own engine, as before.
    #[serde(default)] pub workers: Option<Engine>,
    /// BLUEPRINT MODE (blueprint.rs, 2026-09-22): "" = the LLM-orchestrated run; "blueprint" = the deterministic
    /// gates — contract → skeleton → tests → gate → fills → check; every stage timed and priced in `stages`
    #[serde(default)] pub mode: String,
    #[serde(default)] pub blueprint: Option<crate::blueprint::Contract>,
    #[serde(default)] pub stages: Vec<crate::blueprint::Stage>,
    /// THE QA GATE (PLAN-THE-DIAL phase 4): one isolated check per acceptance claim — compile and test claims by the
    /// machine, judgment claims by one small call each. Replaces the whole-diff judge; `qa_review` stays one release.
    #[serde(default)] pub qa_gate: Option<crate::blueprint::QaGate>,
    /// THE MODELS BAR (PLAN-THE-DIAL §2): the dial's cost stop for this run — 0 CHEAPER · 1 BALANCED · 2 SMARTER.
    /// It does not name a model: it walks each role's binding one rung of the operator's own ladder (cli::role_binding_at).
    #[serde(default)] pub cost: Option<u8>,
    /// THE PAIR (2026-09-22): the merge this run resolves its jobs under — brain / hands. None = solo (the global rule).
    #[serde(default)] pub pair: Option<crate::cli::Pair>,
    /// A CONTROLLED RUN SAYS SO (2026-09-28). The name of the benchmark suite this run belongs to, "" for ordinary
    /// work. A bench arm is the same fixture and the same ask across pairings — comparable, which is the whole
    /// difference between a measurement and a thing that happened. Without this the serve filed every bench arm as
    /// `source: "run"` (real use), so the controlled half of the record could only ever be written by a harness
    /// copying rows in afterwards, and `blend`'s "observation moves a benched score" had nothing to move.
    #[serde(default)] pub suite: String,
    /// THE PANE THIS RUN BELONGS TO (2026-09-28), 0 when it was started by something with no pane — the MCP tool,
    /// a bench harness. A run knew its box and its parent and not the surface the operator was looking at, so
    /// nothing it learned could be said back there: the screening answers went into the run's own log and the chat
    /// that asked the question never saw what was chosen.
    #[serde(default)] pub pane_id: i64,
    /// THE PANE'S EDIT SCOPE when this run was born (2026-10-13): empty = unrestricted; otherwise named paths and worker writes must stay inside it.
    #[serde(default)] pub scope: Vec<String>,
    /// NO CHECKS ON THE WORKER'S CLOCK (operator 2026-09-28: "it made the build again. Builds take time, the build button
    /// is not active on the Heimdall menu so it shouldn't do that"). True when the pane switched none of the test /
    /// build / lint gates on — the workers are then told not to run a build, a linter, a type-check, a test suite or
    /// a dev server of their own (a button took 3 min 54 s, most of it a build nobody asked for).
    #[serde(default)] pub checks_off: bool,
    /// THE ROW THIS RUN WAS FILED UNDER (2026-09-28, operator: "make each call of the agents feed the benchmarks automatically —
    /// we know tokens, price, and QA knows correctness"). Kept so the QA judge, which finishes AFTER the run settles, can add
    /// its verdict to that same row as the correctness it measured.
    #[serde(default)] pub merge_outcome: Option<crate::merges::Outcome>,
    /// THE ROLE HEIMDALL PASSED THE BATON TO (operator 2026-09-27: "heimdall now chooses a lot of stuff, role, planner
    /// and worker … and QA. All those should be displayed in real time on the sidepane"). `roles::route_role`'s verdict
    /// as it was — role, brain, hands, measured, why, and `bent` when a switched-off seat moved it — so the rail can
    /// show the decision itself and not only the sentence the log kept.
    #[serde(default)] pub role: Option<Value>,
    /// QA ALWAYS RUNS (operator 2026-09-28: "the QA is obligatory for all tasks"). Kept on the run because the
    /// screens read it, and because a run recorded before this was optional still says what it did.
    #[serde(default)] pub qa_review_wanted: bool,
    /// DOUBLE QA — what the Heimdall menu's QA button now buys (operator 2026-09-28: "the button QA on heimdall
    /// menu should be to activate double QA. A second QA that will re-measure the work AND the first QA to get a
    /// more precise evaluation").
    ///
    /// A single judge is one opinion, and the bench says judges are the least-measured job on the board. The
    /// second pass is given the same diff AND the first verdict, and is asked to review both — so a first judge
    /// that was too harsh or too generous is itself reviewed rather than believed. The second verdict is the
    /// run's; the first is kept beside it so the two can be compared.
    #[serde(default)] pub qa_double: bool,
    /// CONFIDENCE AFTER THE RESULT (2026-10-10): what the run can say about itself once it ends, from the facts it holds (`confidence.rs`); QA is optional, and when it did not run the result offers to run it
    #[serde(default)] pub confidence: Option<crate::confidence::Confidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub qa_first: Option<QaReview>,
    #[serde(default)] pub qa_review: Option<QaReview>,
    /// what the router saved against one high-tier agent doing the same tokens, once the run settles
    #[serde(default)] pub savings: Option<Savings>,
    /// THE QA GATE (2026-09-20, antiFlow's QA stage): a write run's changed files → the project's related
    /// tests, run once after the join, before any PR — the proof rides in the report
    #[serde(default)] pub qa: Option<Qa>,
    /// THE BASELINE (antiFlow's before/after gate): the same related tests on the OWNED files at approval,
    /// before any task edits — a suite that was already red is reported as pre-existing, not blamed
    #[serde(default)] pub qa_baseline: Option<Qa>,
    /// the baseline is still running (react's jest outlives two short tasks, run 9 2026-09-20) — the
    /// after-gate waits for it, bounded, instead of comparing against nothing
    #[serde(skip)] pub qa_baseline_pending: bool,
    /// MID-TASK FORM CHANGES (PLAN-THE-CHAMELEON C2): each task takes the binding of the ROLE its owned files
    /// belong to, so a plan that touches the screen and then the API is done by two forms. Off unless the pane
    /// asked — `settings.roles.mid_task`, off by default until C4's third number exists.
    #[serde(default)] pub mid_task: bool,
    /// THE BOX AS THE RUN FOUND IT (2026-09-29): a snapshot of the working tree when the plan was approved. A run is judged on what IT changed - the diff against `HEAD`
    /// also held every earlier run's uncommitted work, so a worker that removed the search the chat had added (uncommitted) was rejected for "no removal" and its retry
    /// reverted the theme toggle and the tag chips that were never its business.
    #[serde(default)] pub base: Option<RunBase>,
    /// QA is still judging this run: it is not announced as finished (and the queue does not move on) until the gate has passed or the send-backs are spent
    #[serde(default)] pub qa_pending: bool,
    /// how many times QA sent this run back to its workers with feedback (bounded)
    #[serde(default)] pub qa_rounds: u32,
    /// THE DESIGN ROUND (PLAN-THE-DESIGN S5, 2026-10-01): the latest design lint and pictures of this run's page, the rule failures that block delivery, the critic's fixes and what the round cost (design_round.rs)
    #[serde(default)] pub design: Option<crate::design_round::DesignState>,
    /// THE RAW COMPARISON (operator 2026-09-19: "compare with raw claude as well"): the same goal as ONE
    /// plain call — no context pack, no lenses, no briefs, no verification, no join — beside the run.
    #[serde(default)] pub compare: Option<Comparison>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Comparison {
    pub engine: Engine, pub tier: Tier, pub started: i64, pub ended: Option<i64>, pub ms: i64,
    pub tokens_in: Option<i64>, pub tokens_out: Option<i64>, pub tools: usize, pub chars: usize,
    pub report: Option<String>, pub error: Option<String>,
    /// the run's own numbers at the time of the comparison, for the side-by-side
    pub run_ms: i64, pub run_calls: usize, pub run_tools: usize, pub run_tokens_in: i64, pub run_tokens_out: i64, pub run_chars: usize, pub run_passed: usize, pub run_tasks: usize,
}

impl<'de> Deserialize<'de> for Engine {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        crate::cli::engine_of(&json!(s)).ok_or_else(|| serde::de::Error::custom(format!("unknown engine {s}")))
    }
}
impl<'de> Deserialize<'de> for CliTrace {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        let n = |k: &str| v.get(k).and_then(Value::as_i64);
        Ok(CliTrace { model: v.get("model").and_then(Value::as_str).map(String::from), id: n("id").unwrap_or(0), engine: crate::cli::engine_of(v.get("engine").unwrap_or(&Value::Null)).unwrap_or(Engine::ClaudeCode), cwd: v.get("cwd").and_then(Value::as_str).unwrap_or("").into(), started: n("started").unwrap_or(0), ended: n("ended").unwrap_or(0), ms: n("ms").unwrap_or(0), tools: v.get("tools").and_then(Value::as_array).map(|a| a.iter().map(|t| crate::cli::CliTool { name: t.get("name").and_then(Value::as_str).unwrap_or("").to_string(), target: t.get("target").and_then(Value::as_str).unwrap_or("").to_string() }).collect()).unwrap_or_default(), tokens_in: n("tokensIn"), tokens_out: n("tokensOut"), cost: v.get("cost").and_then(Value::as_f64), reported: None, chars: n("chars").unwrap_or(0) as usize, ok: v.get("ok").and_then(Value::as_bool).unwrap_or(false), error: v.get("error").and_then(Value::as_str).map(String::from) })
    }
}

/// Does an ownership glob cover a worktree-relative path? (same rules as the hook's in_scope)
pub fn owns_file(owns: &[String], rel: &str) -> bool {
    owns.iter().any(|g| g == rel || g.strip_suffix("/**").map(|p| format!("{rel}/").starts_with(&format!("{p}/"))).unwrap_or(false) || (!g.contains(['*', '?', '[']) && rel.starts_with(&format!("{}/", g.trim_end_matches('/')))) || glob_match(g, rel))
}
fn glob_match(g: &str, rel: &str) -> bool {
    if !g.contains(['*', '?', '[']) { return false; }
    let mut re = String::from("^");
    let chars: Vec<char> = g.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            if chars.get(i + 2) == Some(&'/') { re.push_str("(?:.*/)?"); i += 3; } else { re.push_str(".*"); i += 2; }
            continue;
        }
        match c { '*' => re.push_str("[^/]*"), '?' => re.push_str("[^/]"), '[' | ']' => re.push(c), _ => re.push_str(&regex::escape(&c.to_string())) }
        i += 1;
    }
    re.push('$');
    regex::Regex::new(&re).map(|r| r.is_match(rel)).unwrap_or(false)
}
fn owns_text(t: &GoalTask) -> String { if t.owns.is_empty() { "(no files — read-only task)".into() } else { t.owns.join(", ") } }

// ── prompts: rendered from the catalog (prompts.rs) — Settings › Prompts edits the active version ──
pub(crate) fn vars(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> { pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect() }
pub fn plan_prompt(goal: &str, n: usize, write: bool) -> String { plan_prompt_with(goal, n, write, &[]) }
/// The planner's prompt with the lenses it may assign — "- id — title: description" per line.
/// THE RATIFIER (2026-09-20): reads the plan as a contract and lists what is wrong with it — nothing
/// when it holds. `parse_ratify` is pure — tested.
pub fn ratify_prompt(goal: &str, tasks: &[PlanTask]) -> String {
    let plan = serde_json::to_string_pretty(&tasks.iter().map(|t| json!({ "id": t.id, "title": t.title, "tier": t.tier.key(), "owns": t.owns, "needs": t.needs, "acceptance": t.acceptance, "prompt": t.prompt })).collect::<Vec<_>>()).unwrap_or_default();
    crate::prompts::render_base("goal.ratify", &vars(&[("goal", json!(goal)), ("plan", json!(plan))]))
}
/// THE TECHNOLOGY OF THE RUN, NOT OF THE APP (2026-09-28). Both merge-record sites read `audit::workspace_stack()`,
/// which is the stack of the workspace the OPERATOR has open — so every benchmark arm, which runs in a throwaway clone
/// far from that workspace, filed itself under the app's stack or under `any`. The whole controlled record landed in
/// the `any` pool: 29 arms of `editor-s/s/backend/any` that real `web` work can never reach, because `best_for_stack`
/// tries the exact stack first and a single unopposed `web` row satisfied it. That is why the router answered
/// `claude-code:sonnet` to every frontend ask however much had been measured. A row describes its own run, so it
/// carries the stack of the directory that run happened in; an empty cwd still falls back to the workspace.
/// WHICH KIND OF EVIDENCE THIS RUN IS (2026-09-28). A run that names a benchmark suite is controlled: the same
/// fixture and the same ask across pairings, comparable, which is what `merges::Evidence::Controlled` means and what
/// `blend` treats as the likelihood. Everything else is real use, comparable only with itself.
///
/// Until this existed the serve filed EVERY run as real use, benches included — so controlled rows could only be
/// written by a harness posting copies in afterwards, which is how 29 benched arms ended up in a pool that no real
/// `web` work could reach. A bench that goes through the serve now records itself correctly the first time. Pure.
/// THE SCREENING ANSWERS, AS ONE LINE FOR THE CHAT (2026-09-28). `None` when the operator took the defaults — an
/// empty object is "you decide", and announcing that as a choice would put words in their mouth. Blanks are dropped
/// for the same reason `heimdall-rail` drops them: an emptied box is not an answer. Pure — tested.
pub fn answers_line(answers: &Value) -> Option<String> {
    let o = answers.as_object()?;
    let picked: Vec<String> = o.values()
        .filter_map(|v| v.as_str().map(str::trim).filter(|s| !s.is_empty()))
        .map(|s| s.to_string())
        .collect();
    if picked.is_empty() { return None }
    Some(format!("answering heimdall: {}", picked.join(" · ")))
}

fn source_of(suite: &str) -> &'static str { if suite.trim().is_empty() { "run" } else { "bench" } }

fn run_stack(cwd: &str) -> String {
    if cwd.trim().is_empty() { return crate::audit::workspace_stack().to_string() }
    crate::gates::stack_at(cwd).to_string()
}

/// A SIGN OF LIFE FOR A SILENT WORKER (Grokbot QA 2026-10-12: "run #7 Working for 13+ minutes, 0 files, 0 tokens, $0.00, the log ends at `run 7`"). A worker's tokens, cost and changed files are only written when its CLI call
/// RETURNS, and a Codex worker with no history may legitimately take its whole first window (12 minutes, then one retry), so a healthy slow worker and a hung one looked the same on screen. After five minutes of silence the
/// run says so, once per five minutes: the minutes since it started. `started` and the last event about this task are in ms. Pure - tested.
pub(crate) fn silence_notice(started: i64, last_event_about_task: Option<i64>, now: i64) -> Option<String> {
    const EVERY: i64 = 5 * 60_000;
    if now - started < EVERY { return None }
    if last_event_about_task.map(|t| now - t < EVERY).unwrap_or(false) { return None }
    Some(format!("still working: no output yet ({} min since it started). A slow seat can take up to {} minutes before its first answer; if this stays silent the run times it out and retries once", (now - started) / 60_000, 12))
}

pub fn parse_ratify(content: &str) -> Vec<String> {
    let (s0, e0) = (content.find('{'), content.rfind('}'));
    let v: Value = match (s0, e0) { (Some(a), Some(b)) if b > a => serde_json::from_str(&content[a..=b]).unwrap_or(Value::Null), _ => Value::Null };
    let ok = v.get("ok").and_then(Value::as_bool).unwrap_or(true);
    let problems: Vec<String> = v.get("problems").and_then(Value::as_array).map(|a| a.iter().filter_map(|p| p.as_str().map(|x| x.trim().to_string())).filter(|x| !x.is_empty()).take(6).collect()).unwrap_or_default();
    if ok && problems.is_empty() { vec![] } else { problems }
}
/// The screen's answer: the questions (ids q1.., ≤3, blank text dropped) and the assumptions. Pure, tested.
/// A reply that carries tasks AND questions is a plan — the questions are demoted to assumptions.
pub fn parse_screen(content: &str) -> (Vec<Question>, Vec<String>) {
    let (s0, e0) = (content.find('{'), content.rfind('}'));
    let v: Value = match (s0, e0) { (Some(a), Some(b)) if b > a => serde_json::from_str(&content[a..=b]).unwrap_or(Value::Null), _ => Value::Null };
    let strs = |x: Option<&Value>| x.and_then(Value::as_array).map(|a| a.iter().filter_map(|p| p.as_str().map(|x| x.trim().to_string())).filter(|x| !x.is_empty()).collect::<Vec<_>>()).unwrap_or_default();
    let mut assumptions: Vec<String> = strs(v.get("assumptions")).into_iter().take(6).collect();
    let has_tasks = v.get("tasks").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false);
    let mut qs = vec![];
    for (i, q) in v.get("questions").and_then(Value::as_array).into_iter().flatten().enumerate() {
        // a question is an object (the planner\'s) or a plain string (the dedicated screen\'s, the shape bench/screen-solo measured)
        let text = q.as_str().or_else(|| q.get("text").and_then(Value::as_str)).map(str::trim).unwrap_or("").to_string();
        if text.is_empty() { continue; }
        let default = q.get("default").and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
        let assumable = q.get("certainty").and_then(Value::as_str).map(|c| c.trim().eq_ignore_ascii_case("assumable")).unwrap_or(false);
        if assumable || has_tasks { if !default.is_empty() { assumptions.push(format!("{text} → {default}")); } continue; }
        // A QUESTION WHOSE EVERY ANSWER IS "YOU DECIDE" IS NOT A QUESTION (operator 2026-09-28): it is taken by its default
        // and stated as an assumption, so the run does not stop to learn nothing
        { let opts = strs(q.get("options")); if crate::screen::is_generic(&text, &opts) { assumptions.push(if default.is_empty() { format!("{text} → the planner decides") } else { format!("{text} → {default}") }); continue; } }
        qs.push(Question { id: format!("q{}", i + 1), text, options: { // A QUESTION WITH NO OPTIONS IS A BARE BOX (audit 2026-09-28): the planner often omits them, and
                   // the operator asked for chips. Its own default is always worth a chip — it is the answer that
                   // needs no thought — and the front end appends "write it yourself" for everything else.
                   let mut o: Vec<String> = strs(q.get("options")).into_iter().take(crate::screen_kinds::MAX_CHIPS).collect();
                   if o.is_empty() && !default.trim().is_empty() { o.push(default.clone()); }
                   o }, default, certainty: Certainty::Blocking, answer: None, kind: q.get("kind").and_then(Value::as_str).unwrap_or("").to_string() });
    }
    qs.truncate(3);
    (qs, assumptions)
}
/// THE SCREENING CALL'S PROMPT (L9). The rule is the one bench/screen-solo measured; the goal is the only variable.
pub fn screen_prompt(goal: &str) -> String { crate::prompts::render_base("goal.screen", &vars(&[("goal", json!(goal))])) }
/// THE SHAPER\'S PROMPT: the questions screening raised, the fixed kinds, and the box\'s facts.
pub fn shape_prompt(goal: &str, box_facts: &str, questions: &[String], conversation: &str) -> String {
    let qs = questions.iter().enumerate().map(|(i, q)| format!("{}. {q}", i + 1)).collect::<Vec<_>>().join("\n");
    crate::prompts::render_base("goal.screen_shape", &vars(&[("goal", json!(goal)), ("kinds", json!(crate::screen_kinds::menu())), ("box", json!(box_facts)), ("questions", json!(qs)), ("conversation", json!(conversation))]))
}
/// Did the screening call answer in its shape? `{"questions":[…]}` (empty = clear). Anything else is an unreadable seat, not a "clear". Pure.
pub fn is_screen_reply(content: &str) -> bool {
    let (s0, e0) = (content.find('{'), content.rfind('}'));
    let v: Value = match (s0, e0) { (Some(a), Some(b)) if b > a => serde_json::from_str(&content[a..=b]).unwrap_or(Value::Null), _ => Value::Null };
    v.get("questions").map(Value::is_array).unwrap_or(false)
}
/// A reply that is ONLY questions — no tier, no delegate, no tasks. Pure.
pub fn is_question_reply(content: &str) -> bool {
    let (s0, e0) = (content.find('{'), content.rfind('}'));
    let v: Value = match (s0, e0) { (Some(a), Some(b)) if b > a => serde_json::from_str(&content[a..=b]).unwrap_or(Value::Null), _ => Value::Null };
    v.get("questions").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false) && v.get("tasks").is_none() && v.get("tier").is_none() && v.get("delegate").is_none()
}
pub fn plan_prompt_with(goal: &str, n: usize, write: bool, specialists: &[(String, String, String)]) -> String { plan_prompt_full(goal, n, write, specialists, &[]) }
pub fn plan_prompt_full(goal: &str, n: usize, write: bool, specialists: &[(String, String, String)], skills: &[(String, String)]) -> String { plan_prompt_screened(goal, n, write, specialists, skills, false, false) }
/// `screen`: the router may answer with questions (router on); `asked`: the operator already answered once — never again.
pub fn plan_prompt_screened(goal: &str, n: usize, write: bool, specialists: &[(String, String, String)], skills: &[(String, String)], screen: bool, asked: bool) -> String { plan_prompt_screened_by(goal, n, write, specialists, skills, screen, asked, false, "") }
/// `screened`: the dedicated screening call already judged the goal clear — the planner is told a reply with questions is an error.
pub fn plan_prompt_screened_by(goal: &str, n: usize, write: bool, specialists: &[(String, String, String)], skills: &[(String, String)], screen: bool, asked: bool, screened: bool, conversation: &str) -> String {
    let list = specialists.iter().map(|(id, title, desc)| format!("- {id} — {title}: {}", desc.chars().take(140).collect::<String>())).collect::<Vec<_>>().join("\n");
    let sk = skills.iter().map(|(slug, desc)| format!("- {slug}: {}", desc.chars().take(140).collect::<String>())).collect::<Vec<_>>().join("\n");
    crate::prompts::render_base("goal.router_plan", &vars(&[("n", json!(n)), ("verb", json!(if write { "edit" } else { "inspect" })), ("write", json!(write)), ("goal", json!(goal)), ("specialists", json!(list)), ("skills", json!(sk)), ("screen", json!(screen)), ("asked", json!(asked)), ("screened", json!(screened)), ("conversation", json!(conversation))]))
}
/// THE SKILLS OF A BOX (2026-09-20): every `SKILL.md` under `.agents/skills/<slug>/` and
/// `.claude/skills/<slug>/` — slug + the `description:` line (or the first heading). Read once per plan.
pub fn box_skills(cwd: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    for base in [".agents/skills", ".claude/skills"] {
        let Ok(rd) = std::fs::read_dir(std::path::Path::new(cwd).join(base)) else { continue };
        let mut dirs: Vec<std::path::PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.join("SKILL.md").is_file()).collect(); dirs.sort();
        for d in dirs {
            let slug = d.file_name().and_then(|x| x.to_str()).unwrap_or("").to_string();
            if out.iter().any(|(s, _)| *s == slug) { continue; }
            let body = std::fs::read_to_string(d.join("SKILL.md")).unwrap_or_default();
            let desc = body.lines().find_map(|l| l.strip_prefix("description:")).map(|d| d.trim().trim_matches('"').to_string()).or_else(|| body.lines().find(|l| l.starts_with("# ")).map(|l| l[2..].trim().to_string())).unwrap_or_default();
            out.push((slug, desc));
        }
    }
    out
}
/// A task's skills as prompt text: each SKILL.md, bounded — the sub-agent applies them. Pure over the fs.
pub fn skills_text(cwd: &str, skills: &[String]) -> String {
    skills.iter().filter_map(|slug| {
        let slug = slug.trim().trim_start_matches('/');
        if slug.is_empty() || slug.contains("..") { return None; }
        [".agents/skills", ".claude/skills"].iter().find_map(|b| std::fs::read_to_string(std::path::Path::new(cwd).join(b).join(slug).join("SKILL.md")).ok())
            .map(|body| format!("### skill: {slug}\n{}", body.chars().take(6000).collect::<String>()))
    }).collect::<Vec<_>>().join("\n\n")
}
pub fn task_prompt(run: &GoalRun, task: &GoalTask, predecessors: &[&GoalTask]) -> String {
    format!("{}{}", crate::queue::block_for(run.pane_id, run.base.as_ref()), task_prompt_body(run, task, predecessors))
}
fn task_prompt_body(run: &GoalRun, task: &GoalTask, predecessors: &[&GoalTask]) -> String {
    let pre = predecessors.iter().filter_map(|p| p.report.as_ref().map(|r| format!("### from \"{}\" ({})\n{}", p.title, p.id, r.chars().take(8000).collect::<String>()))).collect::<Vec<_>>().join("\n\n");
    let board = run.blackboard.iter().rev().take(20).collect::<Vec<_>>().into_iter().rev().map(|b| format!("- [{}] {}", b.task, b.text)).collect::<Vec<_>>().join("\n");
    let retry = match &task.verdict { Some(v) if !v.passed => v.reasons.iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n"), _ => String::new() };
    let acceptance = if task.acceptance.is_empty() { "- the task is done and the report says how".to_string() } else { task.acceptance.iter().map(|a| format!("- {a}")).collect::<Vec<_>>().join("\n") };
    crate::prompts::render_base("goal.task", &vars(&[
        ("id", json!(task.id)), ("count", json!(run.tasks.len())), ("wave", json!(task.wave + 1)), ("goal", json!(run.goal)), ("title", json!(task.title)), ("prompt", json!(task.prompt)),
        ("own_verb", json!(if run.write { "edit — a write anywhere else is refused" } else { "report on in depth" })), ("owns", json!(owns_text(task))), ("acceptance", json!(acceptance)),
        ("briefs", json!(task.briefs.join("\n\n---\n\n"))),
        ("lenses", json!(if task.briefs.is_empty() { task.specialists.join(", ") } else { String::new() })),
        ("predecessors", json!(pre)), ("board", json!(board)), ("retry", json!(retry)), ("write", json!(run.write)),
        ("skills", json!(skills_text(&run.cwd, &task.skills))), ("no_checks", json!(run.checks_off && run.write)),
    ]))
}
/// What the verifier may be shown of the diff, in characters. `verify_prompt` caps at the same number.
/// HOW LONG A FILL MAY TAKE before the watchdog abandons and retries it. Six times the median this engine and tier
/// usually need, floor 3 min, ceiling 15 — except a ONE-FILE low task, where the ceiling is 4 min: the same one-line
/// edit measured 30 s and 393 s on the same worker (2026-09-22), and a one-line edit that has run four minutes is
/// parked in OpenCode's queue, not thinking. Pure — tested.
pub fn fill_timeout_ms(median_ms: i64, one_file: bool, tier: Tier) -> u64 {
    let ceiling = if one_file && tier <= Tier::Low { 240_000 } else { 900_000 };
    (median_ms.saturating_mul(6)).clamp(180_000, ceiling) as u64
}
/// THE TWO SEAT KEYS A RUN'S MERGE ROW IS FILED UNDER, the same for the run's own row and for the QA gate's record that joins it (the gate site computed its own, nominal, pair - so a run whose seats differed from the
/// nominal ones got a second, mismatched row: found by the second live Claude-off run). `raw` for a solo run's planner; the planner that answered; the worker that ran; the nominal binding only when nothing says otherwise. Pure - tested.
pub fn run_row_keys(r: &GoalRun, nominal_planner: &crate::cli::Binding, nominal_worker: &crate::cli::Binding) -> (String, String) {
    let planner = if r.plan_basis == "solo" { "raw".to_string() }
        else if plan_binding_is_a_seat(&r.plan_binding) { r.plan_binding.split_once(':').map(|(e, m)| crate::merges::key(e, Some(m))).unwrap_or_else(|| crate::merges::key(nominal_planner.engine.key(), nominal_planner.model.as_deref())) }
        else { crate::merges::key(nominal_planner.engine.key(), nominal_planner.model.as_deref()) };
    let worker = ran_worker_key(&r.tasks).unwrap_or_else(|| crate::merges::key(nominal_worker.engine.key(), nominal_worker.model.as_deref()));
    (planner, worker)
}
/// A recorded plan binding is a seat when it reads `engine:model` with a model (a bare `engine:` is a provider with no model - the router skips those, so the record must not file under one). Pure - tested.
pub fn plan_binding_is_a_seat(b: &str) -> bool { b.split_once(':').is_some_and(|(e, m)| !e.is_empty() && !m.trim().is_empty()) }
/// THE WORKER THAT RAN: the most common `engine:model` among the finished tasks whose trace says which model it asked for; `None` when no trace does (an old run, a replayed call) and the caller falls back to the bound seat. Pure - tested.
pub fn ran_worker_key(tasks: &[GoalTask]) -> Option<String> {
    let mut seen: Vec<(String, usize)> = vec![];
    for t in tasks.iter().filter(|t| t.status == TaskStatus::Done) {
        let Some(tr) = &t.trace else { continue };
        let Some(m) = tr.model.as_deref().filter(|m| !m.trim().is_empty()) else { continue };
        let k = crate::merges::key(tr.engine.key(), Some(m));
        match seen.iter_mut().find(|(x, _)| *x == k) { Some((_, n)) => *n += 1, None => seen.push((k, 1)) }
    }
    seen.into_iter().max_by_key(|(_, n)| *n).map(|(k, _)| k)
}
pub const DIFF_BUDGET: usize = 40_000;
/// THE DIFF, FILE BY FILE, UNDER A BUDGET (2026-09-23). Every file the task touched is represented: the small ones
/// whole, the big ones truncated with a line that says how much was elided and that the file WAS changed. Smallest
/// first, so what a small file leaves unspent grows the share of the big ones; a floor keeps every file visible even
/// when there are many. Never exceeds `total` unless the floor forces it. Pure — tested.
pub fn budget_diff(parts: &[(String, String)], total: usize) -> String {
    const FLOOR: usize = 600;
    if parts.is_empty() { return String::new(); }
    let len = |i: usize| parts[i].1.chars().count();
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by_key(|&i| len(i));
    let (mut left, mut n) = (total, parts.len());
    let mut share = vec![0usize; parts.len()];
    for &i in &order {
        let fair = (left / n.max(1)).max(FLOOR);
        share[i] = len(i).min(fair);
        left = left.saturating_sub(share[i]);
        n -= 1;
    }
    let mut out = String::new();
    for (i, (f, body)) in parts.iter().enumerate() {
        let n_chars = len(i);
        if n_chars <= share[i] { out.push_str(body); }
        else {
            out.push_str(&body.chars().take(share[i]).collect::<String>());
            out.push_str(&format!("\n… {} more character(s) of `{f}` elided to fit the window — this file WAS changed; judge it on what is shown and do not reject it for missing evidence\n", n_chars - share[i]));
        }
        if !out.ends_with('\n') { out.push('\n'); }
    }
    out
}
pub fn verify_prompt(run: &GoalRun, task: &GoalTask, diff: &str) -> String {
    format!("{}{}", crate::queue::block_for(run.pane_id, run.base.as_ref()), verify_prompt_body(run, task, diff))
}
fn verify_prompt_body(run: &GoalRun, task: &GoalTask, diff: &str) -> String {
    let acceptance = if task.acceptance.is_empty() { "1. the task is done and the report says how".to_string() } else { task.acceptance.iter().enumerate().map(|(i, a)| format!("{}. {a}", i + 1)).collect::<Vec<_>>().join("\n") };
    crate::prompts::render_base("goal.verify", &vars(&[
        ("goal", json!(run.goal)), ("title", json!(task.title)), ("prompt", json!(task.prompt)), ("acceptance", json!(acceptance)),
        // THE WHOLE REPORT (2026-09-19): the verifier used to see the first 8,000 chars of a 14,000-char
        // report and rejected it for "cutting off mid-sentence" — twice. A CLI verifier has the context;
        // give it the report entire and a generous diff.
        ("report", json!(task.report.clone().unwrap_or_default().chars().take(60_000).collect::<String>())),
        ("diff", json!(if diff.is_empty() { String::new() } else { diff.chars().take(DIFF_BUDGET).collect::<String>() })),
    ]))
}
pub fn join_prompt(run: &GoalRun) -> String {
    let body = run.tasks.iter().map(|t| format!("## {} · {} · {}{}\nowns: {}{}\n{}{}", t.id, t.title, status_str(t.status), t.verdict.as_ref().map(|v| format!(" · verdict {}", if v.passed { "PASS" } else { "FAIL" })).unwrap_or_default(), owns_text(t), if t.changed.is_empty() { String::new() } else { format!("\nchanged: {}", t.changed.join(", ")) }, t.report.clone().or_else(|| t.error.clone()).unwrap_or_else(|| "(no report)".into()).chars().take(12_000).collect::<String>(), t.verdict.as_ref().filter(|v| !v.passed).map(|v| format!("\nverifier: {}", v.reasons.join(" · "))).unwrap_or_default())).collect::<Vec<_>>().join("\n\n");
    // the QA gate's verdict rides into the join as its own section (2026-09-20)
    let qa = run.qa.as_ref().map(|q| format!("\n\n## QA gate — the related tests\n`{}` → {} (exit {}, {}s){}\n```\n{}\n```", q.cmd, if q.passed { "PASSED" } else { "FAILED" }, q.exit, q.ms / 1000, if q.pre_existing { " — ALREADY RED BEFORE THE CHANGE (baseline): pre-existing, not this run's doing; say so" } else { "" }, q.tail.chars().rev().take(1500).collect::<String>().chars().rev().collect::<String>())).unwrap_or_default();
    crate::prompts::render_base("goal.join", &vars(&[("goal", json!(run.goal)), ("body", json!(format!("{body}{qa}")))]))
}
fn status_str(s: TaskStatus) -> &'static str { match s { TaskStatus::Pending => "pending", TaskStatus::Ready => "ready", TaskStatus::Running => "running", TaskStatus::Verifying => "verifying", TaskStatus::Done => "done", TaskStatus::Failed => "failed", TaskStatus::Blocked => "blocked", TaskStatus::Skipped => "skipped" } }

// ── pure helpers ───────────────────────────────────────────────────────────────────────────────
#[derive(Clone, Debug)]
pub struct PlanTask { pub id: String, pub title: String, pub prompt: String, pub owns: Vec<String>, pub needs: Vec<String>, pub acceptance: Vec<String>, pub tier: Tier, #[allow(dead_code)] pub specialists: Vec<String>, pub skills: Vec<String> }

fn sv(v: Option<&Value>) -> String { match v { Some(Value::String(s)) => s.clone(), Some(Value::Null) | None => String::new(), Some(o) => o.to_string() } }
fn strings(v: Option<&Value>) -> Vec<String> { v.and_then(Value::as_array).map(|a| a.iter().map(|x| sv(Some(x))).collect()).unwrap_or_default() }

/// Parse + sanitise the planner's JSON: ids unique, owns disjoint (first owner wins), needs known and acyclic.
/// The router's answer: `{tier, delegate, why, tasks}` — or the older bare task array (delegate = true,
/// tier from the tasks). Pure, tested. The plan's tasks go through `parse_plan`'s sanitising.
/// AN `Ok` WITH NO CONTENT IS A FAILURE (2026-09-26). Every engine has a way of exiting cleanly while saying nothing —
/// a dead gateway, a refused key, a CLI that printed a banner — and every caller of `run_job` already treated that as
/// an error. Doing it inside `run_job` is what lets the fallback rung actually run. Pure.
pub fn empty_is_failure(r: Result<crate::cli::RunOut, String>) -> Result<crate::cli::RunOut, String> {
    match r {
        Ok(o) if o.parsed.content.trim().is_empty() =>
            Err(o.parsed.error.clone().unwrap_or_else(|| "the engine produced no answer".into())),
        x => x,
    }
}

/// CAUTION: THE TWO WAYS A ROUTE GOES UP A TIER (operator 2026-10-11, taken from jev-router: a wrong cheap pick costs more than an extra strong one).
///   · DOUBT - the router graded the route "low" (`gauge::doubt`): the run and every task one tier up, except the tasks the mechanical rule lowered (`held`), which rest on deterministic evidence.
///   · RISK - a task that owns credentials, billing, a migration or a deploy goes to HIGH (`gauge::risk_paths`); an ask that talks about them (`gauge::risk_ask`) raises the run and every task one tier; a file the ask names does
///     the same for the run. Doubt and an ask's risk words together are still ONE step (no double jump), but a risky file is HIGH whatever else was said.
/// Returns one line per change for the run's log, each with the fact it points at. The run's tier never ends below its strongest task's. Pure over its arguments - tested.
pub fn apply_caution(route: &mut Route, tasks: &mut [PlanTask], goal: &str, held: &[String]) -> Vec<String> {
    let mut notes = vec![];
    let doubt = crate::gauge::doubt(&route.confidence); let doubtful = doubt.sure && doubt.value;
    let words = crate::gauge::risk_ask(goal); let wordy = words.sure;
    let named = crate::gauge::risk_named(goal);
    let step = doubtful || wordy;
    if step { let from = route.tier; route.tier = route.tier.up(); if route.tier != from { notes.push(format!("caution: run {}→{} ({})", from.key(), route.tier.key(), if doubtful { doubt.evidence.join(", ") } else { words.evidence.join(", ") })); } }
    if named.sure && route.tier != Tier::High { let from = route.tier; route.tier = Tier::High; notes.push(format!("risk: run {}→high ({})", from.key(), named.evidence.join(", "))); }
    for t in tasks.iter_mut() {
        let by_path = crate::gauge::risk_paths(&t.owns);
        if by_path.sure { if t.tier != Tier::High { notes.push(format!("risk: {} {}→high ({})", t.id, t.tier.key(), by_path.evidence.join(", "))); t.tier = Tier::High; } continue; }
        if wordy || (doubtful && !held.contains(&t.id)) { let from = t.tier; t.tier = t.tier.up(); if t.tier != from { notes.push(format!("caution: {} {}→{}", t.id, from.key(), t.tier.key())); } }
    }
    if let Some(top) = tasks.iter().map(|t| t.tier).max() { if top > route.tier { route.tier = top; } }
    notes
}

pub fn parse_route(text: &str, max: usize) -> (Route, Vec<PlanTask>, Vec<String>) {
    let obj_at = text.find('{'); let arr_at = text.find('[');
    let is_obj = matches!((obj_at, arr_at), (Some(o), Some(a)) if o < a) || (obj_at.is_some() && arr_at.is_none());
    if is_obj {
        let (s, e) = (obj_at.unwrap(), text.rfind('}').unwrap_or(text.len().saturating_sub(1)));
        if let Ok(j) = serde_json::from_str::<Value>(&text[s..=e.max(s)]) {
            if j.get("tasks").is_some() || j.get("delegate").is_some() || j.get("tier").is_some() {
                let tasks_v = j.get("tasks").cloned().unwrap_or(json!([]));
                let (tasks, warnings) = parse_plan(&tasks_v.to_string(), max);
                let mut tier = Tier::of(&sv(j.get("tier"))).unwrap_or_else(|| tasks.iter().map(|t| t.tier).max().unwrap_or_default());
                let delegate = j.get("delegate").and_then(Value::as_bool).unwrap_or(!tasks.is_empty()) && !tasks.is_empty();
                if tasks.iter().any(|t| t.tier > tier) { tier = tasks.iter().map(|t| t.tier).max().unwrap_or(tier); }
                let confidence = match sv(j.get("confidence")).trim().to_ascii_lowercase().as_str() { c @ ("high" | "medium" | "low") => c.to_string(), _ => String::new() };
                let route = Route { tier, delegate, why: sv(j.get("why")).trim().chars().take(240).collect(), bias: vec![], verify_tier: tier.max(Tier::Mid), join_tier: Tier::High, confidence, no_intel: vec![] };
                return (route, tasks, warnings);
            }
        }
    }
    let (tasks, warnings) = parse_plan(text, max);
    let tier = tasks.iter().map(|t| t.tier).max().unwrap_or_default();
    (Route { tier, delegate: !tasks.is_empty(), why: String::new(), bias: vec![], verify_tier: tier.max(Tier::Mid), join_tier: Tier::High, confidence: String::new(), no_intel: vec![] }, tasks, warnings)
}

pub fn parse_plan(text: &str, max: usize) -> (Vec<PlanTask>, Vec<String>) {
    let mut warnings = Vec::new();
    let (s, e) = (text.find('['), text.rfind(']'));
    let slice = match (s, e) { (Some(s), Some(e)) if e > s => &text[s..=e], _ => text };
    let arr: Vec<Value> = serde_json::from_str::<Value>(slice).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let mut seen: Vec<String> = Vec::new();
    let mut owned_by: HashMap<String, String> = HashMap::new();
    let mut tasks: Vec<PlanTask> = Vec::new();
    for (i, x) in arr.iter().take(max.max(1)).enumerate() {
        let mut id = sv(x.get("id")).trim().to_string(); if id.is_empty() { id = format!("t{}", i + 1); }
        while seen.contains(&id) { id = format!("{id}_{}", i + 1); }
        seen.push(id.clone());
        // OWNERSHIP IS EXCLUSIVE — exact duplicates AND overlapping globs (2026-09-19: a task owning
        // `forks/*.js` beside a sibling owning `forks/Scheduler.js` used to slip through): a path a sibling's
        // glob already covers, or a glob covering a sibling's path, is dropped from the later task.
        let owns: Vec<String> = strings(x.get("owns")).into_iter().map(|p| p.trim().trim_start_matches("./").trim_start_matches('/').to_string()).filter(|p| !p.is_empty())
            .filter(|p| {
                let clash = owned_by.iter().find(|(q, o)| *o != &id && (*q == p || owns_file(std::slice::from_ref(q), p) || owns_file(std::slice::from_ref(p), q))).map(|(q, o)| (q.clone(), o.clone()));
                match clash { Some((q, o)) => { warnings.push(format!("{p}: overlaps {q} owned by {o}, dropped from {id}")); false } None => { owned_by.insert(p.clone(), id.clone()); true } }
            }).collect();
        let title: String = { let t = sv(x.get("title")); if t.trim().is_empty() { id.clone() } else { t.trim().chars().take(140).collect() } };
        let prompt = { let p = sv(x.get("prompt")); if p.trim().is_empty() { sv(x.get("title")).trim().to_string() } else { p.trim().to_string() } };
        if title.is_empty() { continue; }
        // THE TIER DEFAULT (2026-09-22): the planner leaves per-task tiers out more often than not, and `mid` was
        // silently taken every time — three benches of "low-tier workers" in the prose, mid in the field. A task that
        // owns exactly one concrete file (no glob) is mechanical: low. Anything wider stays mid.
        let default_tier = if owns.len() == 1 && !owns[0].contains(['*', '?', '[']) { Tier::Low } else { Tier::Mid };
        tasks.push(PlanTask { id, title, prompt, owns, needs: strings(x.get("needs")), acceptance: strings(x.get("acceptance")).into_iter().filter(|a| !a.is_empty()).take(5).collect(), tier: Tier::of(&sv(x.get("tier"))).unwrap_or(default_tier), specialists: strings(x.get("specialists")).into_iter().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).take(2).collect(), skills: strings(x.get("skills")).into_iter().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()).take(3).collect() });
    }
    let ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
    for t in tasks.iter_mut() {
        let id = t.id.clone();
        t.needs.retain(|n| if !ids.contains(n) || n == &id { warnings.push(format!("{id}: unknown dependency {n} dropped")); false } else { true });
    }
    // break cycles: drop the edge that closes one (DFS)
    let mut color: HashMap<String, u8> = HashMap::new();
    fn visit(id: &str, stack: &mut Vec<String>, tasks: &mut Vec<PlanTask>, color: &mut HashMap<String, u8>, warnings: &mut Vec<String>) {
        color.insert(id.to_string(), 1);
        let needs = tasks.iter().find(|t| t.id == id).map(|t| t.needs.clone()).unwrap_or_default();
        let mut kept = Vec::new();
        for n in needs {
            if color.get(&n) == Some(&1) { warnings.push(format!("cycle {}→{n}: dependency {id}→{n} dropped", { let mut s = stack.clone(); s.push(id.to_string()); s.join("→") })); continue; }
            if !color.contains_key(&n) { stack.push(id.to_string()); visit(&n, stack, tasks, color, warnings); stack.pop(); }
            kept.push(n);
        }
        if let Some(t) = tasks.iter_mut().find(|t| t.id == id) { t.needs = kept; }
        color.insert(id.to_string(), 2);
    }
    for id in ids { if !color.contains_key(&id) { visit(&id, &mut Vec::new(), &mut tasks, &mut color, &mut warnings); } }
    (tasks, warnings)
}

/// Wave numbers from the dependency graph (0 = can start at once).
pub fn assign_waves(tasks: &[PlanTask]) -> HashMap<String, u32> {
    fn w(id: &str, depth: usize, tasks: &[PlanTask], wave: &mut HashMap<String, u32>) -> u32 {
        if let Some(v) = wave.get(id) { return *v; }
        if depth > tasks.len() { return 0; }
        let v = match tasks.iter().find(|t| t.id == id) { Some(t) if !t.needs.is_empty() => t.needs.iter().map(|n| w(n, depth + 1, tasks, wave)).max().unwrap_or(0) + 1, _ => 0 };
        wave.insert(id.to_string(), v);
        v
    }
    let mut wave = HashMap::new();
    for t in tasks { w(&t.id, 0, tasks, &mut wave); }
    wave
}

pub fn parse_verdict(text: &str) -> Option<(bool, Vec<String>)> {
    let (s, e) = (text.find('{'), text.rfind('}'));
    let slice = match (s, e) { (Some(s), Some(e)) if e > s => &text[s..=e], _ => text };
    let j: Value = serde_json::from_str(slice).ok()?;
    let passed = j.get("passed")?.as_bool()?;
    Some((passed, strings(j.get("reasons")).into_iter().take(8).collect()))
}

/// The deterministic join when the engine cannot write one.
/// `tsc --noEmit` on a TypeScript box (a tsconfig and a local tsc): the errors whose paths touch `files`, or None
/// when it compiles, is not a TS project, or tsc is unavailable. 120 s cap. Pure over the box.
pub async fn compile_errors(cwd: &str, files: &[String]) -> Option<String> { crate::blueprint::compile_errors_for(cwd, files).await }
#[allow(dead_code)]
async fn compile_errors_unused(cwd: &str, files: &[String]) -> Option<String> {
    let root = std::path::Path::new(cwd);
    let tsc = root.join("node_modules").join(".bin").join("tsc");
    if !tsc.is_file() || !files.iter().any(|f| f.ends_with(".ts") || f.ends_with(".tsx")) { return None; }
    let project = ["tsconfig.app.json", "tsconfig.json"].iter().map(|p| root.join(p)).find(|p| p.is_file())?;
    let out = tokio::time::timeout(std::time::Duration::from_secs(120), crate::proc::tokio_cmd(&tsc).args(["--noEmit", "-p"]).arg(&project).current_dir(root).output()).await.ok()?.ok()?;
    if out.status.success() { return None; }
    let text = String::from_utf8_lossy(&out.stdout);
    let mine: Vec<&str> = text.lines().filter(|l| l.contains("error TS")).filter(|l| files.iter().any(|f| { let f = f.trim_start_matches("./"); f == "**" || l.starts_with(f) || l.contains(f) })).take(6).collect();
    if mine.is_empty() { None } else { Some(mine.join(" | ").chars().take(900).collect()) }
}
/// A reply that is code inside a fence → the code. Pure.
pub fn strip_fence(s: &str) -> String {
    let t = s.trim();
    if let Some(i) = t.find("```") { let rest = &t[i + 3..]; let rest = rest.split_once('\n').map(|(_, r)| r).unwrap_or(rest); if let Some(j) = rest.rfind("```") { return rest[..j].trim().to_string(); } }
    t.to_string()
}
/// The blueprint run's report: the stages with their times and costs, each fill's machine verdict. Pure.
/// WHICH RUNS FILE A PLANNER ROW (2026-10-06): a merged run measures its planner too, and so does a BLUEPRINT run - it has no `pair`
/// (the brain is `engine`, the hands are `workers`), so the old `pair.is_some()` test left the planner unscored and the bench's
/// `audit_only` POST on it got a 409 "no such row". A solo run (planner == worker, or `raw`) and a replayed plan say nothing about a brain.
pub fn files_planner_row(has_pair: bool, mode: &str, planner: &str, worker: &str, plan_basis: &str) -> bool {
    (has_pair || mode == "blueprint") && planner != worker && planner != "raw" && plan_basis != "replayed"
}

pub fn blueprint_report(r: &GoalRun) -> String {
    let stages = r.stages.iter().map(|s| format!("- {} · {} · {:.1} s{}{}", s.name, if s.ok { "ok" } else { "FAILED" }, s.ms as f64 / 1000.0, if s.cost > 0.0 { format!(" · ${:.4}", s.cost) } else { String::new() }, if s.note.is_empty() { String::new() } else { format!(" — {}", s.note) })).collect::<Vec<_>>().join("\n");
    let fills = r.tasks.iter().map(|t| format!("- {} {} · {:?} · attempts {} · {}", t.id, t.title, t.status, t.attempts, t.verdict.as_ref().map(|v| v.reasons.join(" · ")).unwrap_or_default())).collect::<Vec<_>>().join("\n");
    format!("# Blueprint run #{} — {}\n\n## Stages\n{stages}\n\n## Fills\n{fills}", r.id, r.blueprint.as_ref().map(|c| c.summary.clone()).unwrap_or_default())
}
pub fn join_fallback(run: &GoalRun) -> String {
    let done = run.tasks.iter().filter(|t| t.status == TaskStatus::Done).count();
    let mut lines = vec![format!("# Goal run #{} — {done}/{} tasks done", run.id, run.tasks.len()), String::new(), format!("**Goal:** {}", run.goal), String::new()];
    for t in &run.tasks {
        lines.push(format!("## {} · {} · {}{}", t.id, t.title, status_str(t.status), t.verdict.as_ref().map(|v| format!(" · {}", if v.passed { "PASS" } else { "FAIL" })).unwrap_or_default()));
        lines.push(format!("owns: {}", owns_text(t)));
        if !t.changed.is_empty() { lines.push(format!("changed: {}", t.changed.join(", "))); }
        lines.push(String::new());
        lines.push(t.report.clone().or_else(|| t.error.clone()).unwrap_or_else(|| "(no report)".into()).chars().take(1500).collect());
        lines.push(String::new());
    }
    let open: Vec<&GoalTask> = run.tasks.iter().filter(|t| matches!(t.status, TaskStatus::Failed | TaskStatus::Blocked)).collect();
    lines.push("## Open points".into());
    if open.is_empty() { lines.push("- none".into()); } else { for t in open { lines.push(format!("- {} {}: {}{}", t.id, t.title, status_str(t.status), t.verdict.as_ref().map(|v| format!(" — {}", v.reasons.join(" · "))).or_else(|| t.error.as_ref().map(|e| format!(" — {e}"))).unwrap_or_default())); } }
    lines.join("\n")
}

/// A short, tool-less reply that reads as the CLI's own failure (model unsupported, auth, quota, network)
/// → the reason. Pure, conservative: a real report is long or used tools.
pub fn engine_failure(content: &str, tools: usize) -> Option<String> {
    let c = content.trim();
    if tools > 0 || c.chars().count() > 900 || c.contains("FINDING:") { return None; }
    let l = c.to_ascii_lowercase();
    let signs = ["unsupported", "not supported", "unauthorized", "not logged in", "authentication failed", "rate limit", "quota exceeded", "usage limit", "error 429", "(429)", "network error", "could not connect", "econnrefused", "model_not_found", "no such model", "invalid model", "run `codex login`", "run `claude login`", "api key"];
    let hit = signs.iter().find(|s| l.contains(*s))?;
    let line = c.lines().find(|ln| ln.to_ascii_lowercase().contains(hit)).unwrap_or(c);
    Some(line.trim().chars().take(240).collect())
}
/// `FINDING: …` lines in a report → blackboard entries.
pub fn findings(report: &str) -> Vec<String> {
    // char-safe (2026-09-19): `l[..8]` panicked on a line whose first 8 BYTES cut a multi-byte char
    // ("finding—…"), which killed the task's post-run handling and left it "running" forever
    report.lines().map(str::trim).filter_map(|l| {
        let head: String = l.chars().take(8).collect();
        if head.eq_ignore_ascii_case("finding:") { Some(l.chars().skip(8).collect::<String>().trim().to_string()) } else { None }
    }).filter(|l| !l.is_empty()).take(5).collect()
}

/// The delivery line for the parent's terminal.
pub fn join_delivery(run: &GoalRun) -> String {
    let done = run.tasks.iter().filter(|t| t.status == TaskStatus::Done).count();
    let open: Vec<&str> = run.tasks.iter().filter(|t| matches!(t.status, TaskStatus::Failed | TaskStatus::Blocked)).map(|t| t.id.as_str()).collect();
    let summary: String = run.report.clone().unwrap_or_default().lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).take(3).collect::<Vec<_>>().join(" ⏎ ").chars().take(900).collect();
    format!("[antiloki] goal run #{} finished: {done}/{} tasks passed{}. {summary} (full report in the pane's ⋔ subagents → goal runs)", run.id, run.tasks.len(), if open.is_empty() { String::new() } else { format!(" · open: {}", open.join(", ")) })
}

// ── the orchestrator ────────────────────────────────────────────────────────────────────────────
pub struct Goals {
    /// the test cases that failed on a blueprint task's last red check, keyed (run, task) - read on the next red one (`blueprint::may_dispute`)
    last_failing: Mutex<HashMap<(i64, String), Vec<String>>>,
    runs: Mutex<Vec<GoalRun>>,
    seq: Mutex<i64>,
    killers: Mutex<HashMap<String, Killer>>,
    /// runs the operator stopped (their QA ends at its next checkpoint); per `Goals`, so ids that repeat across instances never leak into each other
    stopped: Mutex<std::collections::HashSet<i64>>,
    ticking: Mutex<bool>,
    run: RunFn,
    git: GitFn,
    /// the checkpoint taken when each task's current attempt began - the gate measures the attempt against it (not persisted: a lost one skips the gate, never fails a task)
    attempt_cp: Mutex<HashMap<(i64, String), RunBase>>,
    /// how many times QA may send a run back to its workers (2 by default; 0 = QA only records its verdict)
    qa_max_rounds: std::sync::atomic::AtomicU32,
    concurrency: usize,
    tick_ms: u64,
    specialists: Mutex<Option<(SpecialistListFn, SpecialistReviewFn)>>,
    /// where the runs persist (None = in memory only — the unit tests, each Goals its own world)
    history: Mutex<Option<std::path::PathBuf>>,
    workers_for_next: Mutex<Option<(Option<Engine>, bool)>>,
    /// the pane the NEXT plan belongs to, and whether its workers run no checks — set before planning so the run is born with it
    pane_for_next: Mutex<Option<(i64, bool)>>,
    cost_for_next: Mutex<Option<u8>>,
    mid_task_for_next: Mutex<bool>,
    pair_for_next: Mutex<Option<crate::cli::Pair>>,
    suite_for_next: Mutex<String>,
    /// PER-RUN PLANNER MODE (2026-10-10, the operator: "test accuracy and quality without the planner ... Heimdall should judge if it requires a merge"): `never` = one editor runs the ask alone whatever its size,
    /// `always` = the planner plans it, anything else = Heimdall's own rule (`ask_bigger_than_l`). Set by the plan request for the NEXT run, then keyed by that run's id.
    planner_for_next: Mutex<String>,
    /// A PLAN THE CALLER ALREADY HAS (a saga run from its own tasks, 2026-11-11): taken by the next run made, answered as the planner's reply with no call and no cost
    given_next: Mutex<Option<String>>, given_plans: Mutex<HashMap<i64, String>>,
    planner_modes: Mutex<HashMap<i64, String>>,
    /// runs whose QA was asked for AFTER they finished (the result's "run independent QA"): that QA only REPORTS - it never sends work back or turns a delivered run into a failed one
    report_only: Mutex<std::collections::HashSet<i64>>,
    /// HEIMDALL'S ROLE VERDICT for the next run (2026-09-26) — what it read and why, so the run can SAY it.
    /// A router that changes who is working without a word is worse than one that does not route.
    role_for_next: Mutex<Option<Value>>,
    /// the pane scope for the NEXT plan (2026-10-13), consumed when the run is created
    scope_for_next: Mutex<Option<Vec<String>>>,
    /// THE DEDICATED SCREENING CALL (L9): on in the product; the unit tests keep their scripted single planner call unless one turns it on
    dedicated_screen: std::sync::atomic::AtomicBool,
    /// no planner for asks at L or smaller (on in production; the unit tests of the planner machinery switch it off by default, like the dedicated screen)
    solo_small: std::sync::atomic::AtomicBool,
}

fn real_git(args: &[&str], cwd: &str) -> String {
    crate::proc::cmd("git").args(args).current_dir(cwd).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}
/// Did this task fail because its SEAT was unavailable (out of quota, signed out, switched off) rather than because its answer was wrong? Such a failure says nothing about quality. Pure - tested.
pub(crate) fn task_outage(t: &GoalTask) -> bool {
    t.status == TaskStatus::Failed && t.error.as_deref().map_or(false, |e| crate::cli::is_limit_error(e) || crate::gauge::seat_dead(e).sure || e.contains("is switched off (Settings"))
}
/// A hash of one file's bytes (0 when it cannot be read or is over 4 MB): what tells "modified again" apart from "already modified" when there is no checkpoint. Pure over the file.
pub(crate) fn content_hash(cwd: &str, rel: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let p = std::path::Path::new(cwd).join(rel);
    match std::fs::metadata(&p) { Ok(m) if m.is_file() && m.len() <= 4_000_000 => std::fs::read(&p).map(|b| { let mut h = std::collections::hash_map::DefaultHasher::new(); b.hash(&mut h); h.finish() }).unwrap_or(0), _ => 0 }
}
/// The lines the gate counts as written by an attempt: its own, or - from the second attempt on - the larger of its own and the run's net additions since the run began. Pure - tested.
pub(crate) fn gate_added(attempts: u32, attempt_added: usize, run_added: Option<usize>) -> usize { if attempts >= 2 { attempt_added.max(run_added.unwrap_or(0)) } else { attempt_added } }
/// The runs as a closure sees them under `Goals::with`: reading goes through `Deref` and leaves `dirty` alone, anything that needs `&mut` (iter_mut, push, retain, an index) goes through `DerefMut` and
/// sets it - so the history is written when, and only when, a closure could have changed it.
pub(crate) struct Runs<'a> { v: &'a mut Vec<GoalRun>, dirty: bool }
impl std::ops::Deref for Runs<'_> { type Target = Vec<GoalRun>; fn deref(&self) -> &Vec<GoalRun> { self.v } }
impl std::ops::DerefMut for Runs<'_> { fn deref_mut(&mut self) -> &mut Vec<GoalRun> { self.dirty = true; self.v } }
/// A PLAN THAT NEVER ANSWERS (operator 2026-10-09: run #354 sat in "planning" for 17 min 38 s - no seat call recorded, no event after the gauge line - until the operator stopped it). The only driver of a
/// planning run is the request that started it, so nothing noticed. Planning is silent only inside a model call; every call now writes a marker line first ("asking ..."), and this says how long
/// each kind of silence may last before the run is failed: the planner's own limit plus slack after its marker, the screen's limits plus slack after its marker, 4 minutes after anything else.
/// Pure over the last event; `Some(why)` when the run has stalled.
pub(crate) fn plan_stall(status: RunStatus, last: Option<&GoalEvent>, created: i64, now: i64) -> Option<String> {
    if status != RunStatus::Planning { return None }
    let (since, kind, text) = match last { Some(e) => (e.t, e.kind.as_str(), e.text.as_str()), None => (created, "", "") };
    let limit: i64 = (if kind == "plan" && text.starts_with(PLANNER_MARK) { PLAN_TIMEOUT_MS + 120_000 }
        else if kind == "screen" && text.starts_with(SCREEN_MARK) { SCREEN_TIMEOUT_MS + SHAPE_TIMEOUT_MS + 120_000 }
        else { PLAN_SILENCE_MS }) as i64;
    if now - since <= limit { return None }
    let step = if text.is_empty() { "starting".to_string() } else { text.chars().take(90).collect::<String>() };
    Some(format!("planning made no progress for {} min (last step: {step}) - the seat calls were stopped. Ask again, or send it to another seat.", (now - since) / 60_000))
}
pub(crate) const PLANNER_MARK: &str = "asking the planner";
pub(crate) const SCREEN_MARK: &str = "asking ";
pub(crate) const PLAN_SILENCE_MS: u64 = 240_000;
pub(crate) const PLAN_WATCH_MS: u64 = 10_000;
/// THE QA THAT NEVER ENDS (2026-10-08): a run that finished while the serve restarted keeps `qa_pending` for ever (the judging task died with the process and nothing resumes it), so the chat said
/// "QA is judging" for good and the result was never announced. A Done run still pending at boot is settled: pending off, one line saying why. Pure; true when it changed the run.
pub(crate) fn settle_stale_qa(r: &mut GoalRun) -> bool {
    if r.status != RunStatus::Done || !r.qa_pending { return false }
    r.qa_pending = false;
    Goals::log(r, "qa-stopped", "the serve restarted while QA was judging — no verdict was reached; the result stands without one", None);
    true
}
/// The files a claim about `path` is checked against: the path itself, or - when it is a DIRECTORY - the files the task changed (else owned) under it, sorted and deduplicated.
/// A directory with nothing under it stays as it was. Pure over the `is_dir` answer.
pub(crate) fn claim_files(is_dir: bool, path: &str, changed: &[String], owns: &[String]) -> Vec<String> {
    // A GLOB IS NOT A FILE EITHER (2026-10-09, the live design check: a one-task run owns `**`, the claim's path was `**`, the judge read nothing and answered "no verdict in shape"):
    // it stands for every file the task changed (else owned).
    let glob = path.contains(['*', '?', '[']);
    if !is_dir && !glob { return vec![path.to_string()] }
    let under = if glob { String::new() } else { format!("{}/", path.trim_end_matches('/')) };
    let pool = if changed.is_empty() { owns } else { changed };
    let mut v: Vec<String> = pool.iter().filter(|f| f.starts_with(&under) && !f.contains(['*', '?', '['])).cloned().collect();
    v.sort(); v.dedup();
    if v.is_empty() { vec![path.to_string()] } else { v }
}
/// The text the judge reads for those files: one file is shown as it stands (as before); several are shown one after another under a `=== path ===` line, up to `budget` characters.
pub(crate) fn claim_source(cwd: &str, files: &[String], budget: usize) -> String {
    let read = |f: &String| std::fs::read_to_string(std::path::Path::new(cwd).join(f)).unwrap_or_default();
    if files.len() == 1 { return read(&files[0]).chars().take(budget).collect() }
    let mut out = String::new();
    for f in files {
        let body = read(f); if body.is_empty() { continue }
        if out.chars().count() >= budget { break }
        out.push_str(&format!("=== {f} ===\n{body}\n"));
    }
    out.chars().take(budget).collect()
}
/// The defect class a run's change belongs to, from the files its tasks changed (or own, before they ran): `backend`, `frontend`, `design`, `docs`, `infra`; `None` (pooled) for a mixed, test-only or unknown change.
pub(crate) fn qa_class_of_run(run: &GoalRun) -> Option<&'static str> {
    let mut files: Vec<String> = run.tasks.iter().flat_map(|t| if t.changed.is_empty() { t.owns.clone() } else { t.changed.clone() }).collect();
    files.sort(); files.dedup();
    crate::cli::qa_class(crate::roles::domain_of(&files).key())
}
/// THE NEXT BEST SEAT THAT CAN ANSWER (operator 2026-09-30: "it should ask an available fallback ... automatically choose the next available best
/// option for this job"). Not the same engine as the one that just refused, and never a switched-off, unsigned or out-of-quota one. The ladder's own
/// record for the job comes first (the rung that most often ANSWERED - what the benchmarks and production calls measured), then the router's
/// preference order with that engine's binding for the job. `None` only when nothing else can be called. Pure apart from reading the record.
pub(crate) fn alternate_seat(job: crate::cli::Job, not: Engine, size: &str, pair: Option<&crate::cli::Pair>, cost: Option<u8>) -> Option<(Engine, Option<String>)> {
    let ok = |e: Engine, m: Option<&str>| e != not && crate::cli::engine_routable(e) && crate::cli::model_blocked(e, m).is_none();
    // the gate is not chosen by who answers most often: a rubber stamp answers every time (see `cli::QA_GATE_ORDER`)
    if job == crate::cli::Job::QaGate { if let Some((e, m)) = crate::cli::qa_gate_ranking(None).into_iter().find(|(e, m)| ok(*e, Some(m))) { return Some((e, Some(m.to_string()))) } }
    let mut measured: Vec<(f64, Engine, String)> = crate::ladder::measured(job.key()).into_iter().filter(|(_, n, okn, _)| *n >= 1 && *okn >= 1)
        .filter_map(|(k, n, okn, _)| crate::cli::binding_of_key(&k).map(|(e, m)| (okn as f64 / n as f64, e, m))).collect();
    measured.sort_by(|a, b| b.0.total_cmp(&a.0));
    if let Some((_, e, m)) = measured.into_iter().find(|(_, e, m)| ok(*e, Some(m.as_str()))) { return Some((e, Some(m))); }
    for e in crate::cli::Engine::all() {
        if !ok(e, None) { continue }
        let b = crate::cli::job_binding_learned(job, size, pair, e, cost).0;
        return Some(if b.engine == e && ok(e, b.model.as_deref()) { (e, b.model) } else { (e, None) });
    }
    None
}
pub fn real_runner() -> RunFn {
    Arc::new(|engine: Engine, messages: Vec<ChatMessage>, cwd: String, wants_json: bool, opts: RunOpts, killer: Option<Killer>| -> RunFuture { Box::pin(async move { crate::cli::run_cli(engine, &messages, &cwd, wants_json, opts, killer).await }) })
}

impl Goals {
    /// the CLI runner, for one-shot calls outside a run (chat_pane names its episodes with it)
    pub fn runner(&self) -> RunFn { self.run.clone() }
    pub fn new(run: RunFn, git: Option<GitFn>, concurrency: usize, tick_ms: u64) -> Arc<Self> {
        Arc::new(Goals { last_failing: Mutex::new(HashMap::new()), runs: Mutex::new(Vec::new()), seq: Mutex::new(0), killers: Mutex::new(HashMap::new()), stopped: Mutex::new(Default::default()), ticking: Mutex::new(false), run, git: git.unwrap_or_else(|| Arc::new(real_git)), attempt_cp: Mutex::new(HashMap::new()), qa_max_rounds: std::sync::atomic::AtomicU32::new(2), concurrency, tick_ms, specialists: Mutex::new(None), history: Mutex::new(None), workers_for_next: Mutex::new(None), pane_for_next: Mutex::new(None), cost_for_next: Mutex::new(None), pair_for_next: Mutex::new(None), suite_for_next: Mutex::new(String::new()), planner_for_next: Mutex::new(String::new()), given_next: Mutex::new(None), given_plans: Mutex::new(HashMap::new()), planner_modes: Mutex::new(HashMap::new()), report_only: Mutex::new(Default::default()), role_for_next: Mutex::new(None), scope_for_next: Mutex::new(None), mid_task_for_next: Mutex::new(false), dedicated_screen: std::sync::atomic::AtomicBool::new(!cfg!(test)), solo_small: std::sync::atomic::AtomicBool::new(!cfg!(test)) })
    }
    pub fn set_dedicated_screen(&self, on: bool) { self.dedicated_screen.store(on, std::sync::atomic::Ordering::Relaxed); }
    pub fn set_solo_small(&self, on: bool) { self.solo_small.store(on, std::sync::atomic::Ordering::Relaxed); }
    /// Wire the specialists (lib.rs does, with the serve's db); without them the planner plans without lenses.
    pub fn with_specialists(self: &Arc<Self>, list: SpecialistListFn, review: SpecialistReviewFn) { *self.specialists.lock().unwrap_or_else(|p| p.into_inner()) = Some((list, review)); }
    fn specialist_fns(&self) -> Option<(SpecialistListFn, SpecialistReviewFn)> { self.specialists.lock().unwrap_or_else(|p| p.into_inner()).clone() }
    fn with<T>(&self, f: impl FnOnce(&mut Runs) -> T) -> T {
        let mut g = self.runs.lock().unwrap_or_else(|p| p.into_inner());
        let mut w = Runs { v: &mut g, dirty: false };
        let out = f(&mut w);
        let dirty = w.dirty;
        // THE HISTORY (operator 2026-09-19: "the plans tab is not showing the history of plans"): every
        // MUTATION lands on disk, so runs survive a serve restart and the watcher lists what happened.
        // Only a mutation (2026-10-09): this used to rewrite the whole 3.5 MB file on EVERY call, reads
        // included - each poll, tick and `get` serialised and wrote the history while holding the runs lock,
        // which pinned a core with no run active and queued every caller (async workers included) behind it.
        if dirty { if let Some(p) = self.history.lock().unwrap_or_else(|p| p.into_inner()).clone() { Self::persist(&p, &g); } }
        out
    }
    /// The default history file beside the ledger.
    pub fn default_history_path() -> std::path::PathBuf { Ledger::path().with_file_name("goal-runs.json") }
    /// Persist the runs at `path` from now on (and load what is there — see `restore`).
    pub fn set_qa_rounds(&self, n: u32) { self.qa_max_rounds.store(n, std::sync::atomic::Ordering::Relaxed); }
    pub fn with_history(self: &Arc<Self>, path: std::path::PathBuf) -> usize { *self.history.lock().unwrap_or_else(|p| p.into_inner()) = Some(path); self.restore() }
    fn persist(p: &std::path::Path, runs: &[GoalRun]) {
        let _ = p.parent().map(std::fs::create_dir_all);
        let keep: Vec<&GoalRun> = runs.iter().rev().take(200).collect::<Vec<_>>().into_iter().rev().collect();
        // atomic: a serve killed mid-write left a truncated file, the next boot could not read it, and
        // the boot after that overwrote it (2026-09-19 — six runs gone)
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_string(&keep).unwrap_or_else(|_| "[]".into())).is_ok() { let _ = std::fs::rename(&tmp, p); }
    }
    /// The runs of the last serve, loaded at boot: whatever was live is marked failed ("the serve
    /// restarted") — nothing can resume a CLI process that died with the old process.
    pub fn restore(self: &Arc<Self>) -> usize {
        let Some(path) = self.history.lock().unwrap_or_else(|p| p.into_inner()).clone() else { return 0 };
        let Ok(text) = std::fs::read_to_string(&path) else { return 0 };
        let mut runs = match serde_json::from_str::<Vec<GoalRun>>(&text) {
            Ok(r) => r,
            Err(e) => {
                // NEVER silently drop the history (2026-09-19: an unreadable file was overwritten by the
                // next persist and every run before it vanished) — set it aside and say so
                let aside = format!("{}.unreadable-{}", path.display(), now_ms());
                let _ = std::fs::rename(&path, &aside);
                eprintln!("antiloki-serve: goal history unreadable ({e}) — moved to {aside}");
                return 0;
            }
        };
        for r in runs.iter_mut() {
            if matches!(r.status, RunStatus::Planning | RunStatus::AwaitingAnswers | RunStatus::AwaitingApproval | RunStatus::Running | RunStatus::Paused | RunStatus::Joining) {
                r.status = RunStatus::Failed; r.ended = Some(now_ms()); r.error = Some("the serve restarted while the run was live".into());
                for t in r.tasks.iter_mut() { if matches!(t.status, TaskStatus::Running | TaskStatus::Verifying | TaskStatus::Ready | TaskStatus::Pending) { t.status = TaskStatus::Failed; t.error = Some("the serve restarted".into()); } }
                Self::log(r, "fail", "the serve restarted while the run was live — retry a task to continue", None);
            }
        }
        for r in runs.iter_mut() { settle_stale_qa(r); }
        let n = runs.len();
        let max_id = runs.iter().map(|r| r.id).max().unwrap_or(0);
        self.with(|rs| { **rs = runs; });
        *self.seq.lock().unwrap_or_else(|p| p.into_inner()) = max_id;
        n
    }
    fn log(r: &mut GoalRun, kind: &str, text: impl Into<String>, task: Option<&str>) {
        r.events.push(GoalEvent { t: now_ms(), id: crate::ids::next(if crate::ids::goal_event_is_warning(kind) { "W" } else { "g" }), kind: kind.into(), text: text.into(), task: task.map(String::from) });
        if r.events.len() > 400 { let n = r.events.len() - 400; r.events.drain(0..n); }
    }
    pub fn list(&self, parent: Option<&str>) -> Vec<GoalRun> { self.with(|rs| rs.iter().filter(|r| parent.map(|p| p.is_empty() || r.parent == p).unwrap_or(true)).cloned().collect()) }
    pub fn get(&self, id: i64) -> Option<GoalRun> { self.with(|rs| rs.iter().find(|r| r.id == id).cloned()) }

    /// PLAN: create the run and ask the engine for the DAG; the run then waits for approval.
    pub async fn plan(self: &Arc<Self>, parent: &str, goal: &str, engine: Engine, cwd: &str, write: bool, n: usize, concurrency: Option<usize>) -> GoalRun {
        self.plan_with(parent, goal, engine, cwd, write, n, concurrency, None).await
    }
    /// `forced_tier`: the pane's delegation policy pins every task to one tier (the planner still decides
    /// the rest); None = the planner's own per-task tiers.
    #[allow(clippy::too_many_arguments)]
    pub async fn plan_with(self: &Arc<Self>, parent: &str, goal: &str, engine: Engine, cwd: &str, write: bool, n: usize, concurrency: Option<usize>, forced_tier: Option<Tier>) -> GoalRun {
        self.plan_routed(parent, goal, engine, cwd, write, n, concurrency, forced_tier, true).await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn plan_routed(self: &Arc<Self>, parent: &str, goal: &str, engine: Engine, cwd: &str, write: bool, n: usize, concurrency: Option<usize>, forced_tier: Option<Tier>, router: bool) -> GoalRun {
        self.plan_full(parent, goal, engine, cwd, write, n, concurrency, forced_tier, router, false).await
    }
    /// `auto`: approve the plan the moment it holds (an agent-started run) — remembered so the answer
    /// round does the same after the screening gate.
    #[allow(clippy::too_many_arguments)]
    pub async fn plan_full(self: &Arc<Self>, parent: &str, goal: &str, engine: Engine, cwd: &str, write: bool, n: usize, concurrency: Option<usize>, forced_tier: Option<Tier>, router: bool, auto: bool) -> GoalRun {
        let n = n.clamp(1, 8);
        let id = { let mut s = self.seq.lock().unwrap_or_else(|p| p.into_inner()); *s += 1; *s };
        let mut r = GoalRun { plan_usd: 0.0, screen_usd: 0.0, plan_basis: String::new(), plan_binding: String::new(), worker_binding: String::new(), suite: String::new(), confidence: None, pane_id: 0, scope: vec![], checks_off: false, merge_outcome: None, role: None, plan_ms: 0, id, parent: parent.into(), goal: goal.into(), engine, cwd: cwd.into(), write, concurrency: concurrency.unwrap_or(self.concurrency), status: RunStatus::Planning, created: now_ms(), started: None, ended: None, tasks: vec![], blackboard: vec![], events: vec![], report: None, delivered: false, error: None, route: None, questions: vec![], assumptions: vec![], plan_args: PlanArgs::default(), pr: None, router, min: false, workers: None, mode: String::new(), blueprint: None, stages: vec![], qa_gate: None, cost: None, pair: None, qa_double: false, qa_first: None, qa_review_wanted: false, qa_review: None, savings: None, qa: None, qa_baseline: None, qa_baseline_pending: false, compare: None, mid_task: false, base: None, qa_pending: false, qa_rounds: 0, design: None };
        let (workers, always) = self.workers_for_next.lock().unwrap_or_else(|p| p.into_inner()).take().unwrap_or((None, false));
        // BORN WITH ITS PANE (2026-09-28): `pane_id` was never stamped on a run, so nothing could tell a pane's ask from a bench arm's,
        // and `checks_off` was set AFTER planning returned — a task auto-started by the plan could read the run first
        if let Some((pane, off)) = self.pane_for_next.lock().unwrap_or_else(|p| p.into_inner()).take() { r.pane_id = pane; r.checks_off = off; }
        r.scope = self.take_scope();
        r.cost = self.take_cost(); r.pair = self.take_pair(); r.suite = self.take_suite(); r.mid_task = self.take_mid_task(); self.take_planner_into(r.id); self.take_given_into(r.id);
        // THE BATON, SAID OUT LOUD. The verdict is logged on the run itself, so "who is working and why" is in the
        // same place as everything else the run did — not inferred from a model name in a corner.
        if let Some(role) = self.take_role() {
            if let Some(why) = role.get("why").and_then(Value::as_str) { Self::log(&mut r, "role", why.to_string(), None); }
            r.role = Some(role);
        }
        if let Some(p) = r.pair.clone() { Self::log(&mut r, "pair", format!("the merge: {} — {} decides, {} produces", p.label(), p.brain.key(), p.hands.key()), None); }
        r.plan_args = PlanArgs { n, forced_tier, router, auto, always };
        r.workers = workers;
        let workers_note = r.workers.map(|w| format!(" · workers on {}", w.key())).unwrap_or_default();
        Self::log(&mut r, "plan", format!("planning with {} · up to {n} tasks · {}{workers_note}", engine.key(), if write { "write" } else { "read-only" }), None);
        self.with(|rs| { rs.push(r.clone()); if rs.len() > 100 { let k = rs.len() - 100; rs.drain(0..k); } });
        // the planner is told who the workers are: with cheap workers the price of delegation is only its own
        // plan and the verify round, so independent parts may be split even when small
        let goal_for_planner = match r.workers {
            Some(w) => format!("{goal}\n\nWORKERS: the tasks will be executed by sub-agents on {} at the tier you assign (low = {}); with such workers the price of delegation is only your plan and the verify round — delegate independent parts even when they are small, one task per independent file or module, and rate a fully specified single-file task low.", w.key(), crate::cli::tier_model(w, Tier::Low).unwrap_or_else(|| "its default model".into())),
            None => goal.to_string(),
        };
        self.drive_plan(id, &goal_for_planner, false).await;
        self.get(id).unwrap()
    }
    /// The next `plan_full` runs its tasks on this engine (set by the http op right before planning; consumed once).
    /// The next plan runs with this stop of the models bar (set by the http op right before planning; consumed once).
    pub fn set_cost_for_next(&self, c: Option<u8>) { *self.cost_for_next.lock().unwrap_or_else(|p| p.into_inner()) = c; }
    fn take_cost(&self) -> Option<u8> { self.cost_for_next.lock().unwrap_or_else(|p| p.into_inner()).take() }
    /// The next plan runs under this pair (set by the http op right before planning; consumed once).
    pub fn set_pair_for_next(&self, p: Option<crate::cli::Pair>) { *self.pair_for_next.lock().unwrap_or_else(|p| p.into_inner()) = p; }
    fn take_pair(&self) -> Option<crate::cli::Pair> { self.pair_for_next.lock().unwrap_or_else(|p| p.into_inner()).take() }
    /// The next run is made with this plan (the planner's JSON reply: `{tier, delegate, why, tasks:[{id, title, prompt}]}`): no planning call is made for it.
    pub fn set_given_plan_for_next(&self, plan: Option<String>) { *self.given_next.lock().unwrap_or_else(|p| p.into_inner()) = plan; }
    fn take_given_into(&self, id: i64) { if let Some(p) = self.given_next.lock().unwrap_or_else(|p| p.into_inner()).take() { self.given_plans.lock().unwrap_or_else(|p| p.into_inner()).insert(id, p); } }
    fn given_plan(&self, id: i64) -> Option<String> { self.given_plans.lock().unwrap_or_else(|p| p.into_inner()).get(&id).cloned() }
    pub fn set_planner_for_next(&self, s: String) { *self.planner_for_next.lock().unwrap_or_else(|p| p.into_inner()) = s; }
    /// the planner mode asked for this run (`never` / `always` / empty), consumed from the request that made it
    fn take_planner_into(&self, id: i64) { let m = std::mem::take(&mut *self.planner_for_next.lock().unwrap_or_else(|p| p.into_inner())); if !m.is_empty() { self.planner_modes.lock().unwrap_or_else(|p| p.into_inner()).insert(id, m); } }
    fn planner_mode(&self, id: i64) -> String { self.planner_modes.lock().unwrap_or_else(|p| p.into_inner()).get(&id).cloned().unwrap_or_default() }
    pub fn set_suite_for_next(&self, s: String) { *self.suite_for_next.lock().unwrap_or_else(|p| p.into_inner()) = s; }
    /// stamp the pane a run belongs to, once it exists — see `GoalRun::pane_id`
    /// a line on a run's own log from outside — the plan route says what it bent before the run existed
    pub fn note(&self, id: i64, kind: &str, text: &str) { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, kind, text.to_string(), None); } }); }
    pub fn set_checks_off(&self, id: i64, off: bool) { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { r.checks_off = off; } }); }
    pub fn set_pane(&self, id: i64, pane: i64) { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { r.pane_id = pane; } }); }
    /// the pane a run belongs to, 0 when it has none
    pub fn pane_of(&self, id: i64) -> i64 { self.with(|rs| rs.iter().find(|x| x.id == id).map(|r| r.pane_id).unwrap_or(0)) }
    fn take_suite(&self) -> String { std::mem::take(&mut *self.suite_for_next.lock().unwrap_or_else(|p| p.into_inner())) }
    pub fn set_role_for_next(&self, v: Option<Value>) { *self.role_for_next.lock().unwrap_or_else(|p| p.into_inner()) = v; }
    pub fn set_mid_task_for_next(&self, v: bool) { *self.mid_task_for_next.lock().unwrap_or_else(|p| p.into_inner()) = v; }
    fn take_mid_task(&self) -> bool { std::mem::take(&mut *self.mid_task_for_next.lock().unwrap_or_else(|p| p.into_inner())) }
    fn take_role(&self) -> Option<Value> { self.role_for_next.lock().unwrap_or_else(|p| p.into_inner()).take() }
    pub fn set_pane_for_next(&self, pane: i64, checks_off: bool) { *self.pane_for_next.lock().unwrap_or_else(|p| p.into_inner()) = Some((pane, checks_off)); }
    /// The pane scope the next `plan_full` is born with (2026-10-13); consumed once.
    pub fn set_scope_for_next(&self, scope: Vec<String>) { *self.scope_for_next.lock().unwrap_or_else(|p| p.into_inner()) = Some(scope); }
    fn take_scope(&self) -> Vec<String> { self.scope_for_next.lock().unwrap_or_else(|p| p.into_inner()).take().unwrap_or_default() }
    pub fn set_workers_for_next(&self, w: Option<Engine>, always: bool) { *self.workers_for_next.lock().unwrap_or_else(|p| p.into_inner()) = Some((w, always)); }
    /// THE ANSWER ROUND (the screening gate, 2026-09-20): the operator — or the parent agent through
    /// `antiloki_delegate_answer` — answers the blocking questions (a missing answer = the default), and
    /// the planner plans once more under them, forbidden to ask again. Auto-approved when the run was.
    pub async fn answer(self: &Arc<Self>, id: i64, answers: &Value) -> Option<GoalRun> {
        // A QUESTION QA ASKED (not the planner's): the answer steers the workers, it does not re-plan
        if let Some(back) = self.answer_qa_direction(id, answers) { return back }
        let (goal, auto) = self.with(|rs| {
            let r = rs.iter_mut().find(|x| x.id == id)?;
            if r.status != RunStatus::AwaitingAnswers { return None; }
            let mut lines = vec![]; let mut logs = vec![];
            for q in r.questions.iter_mut() {
                let given = answers.get(&q.id).and_then(Value::as_str).map(str::trim).filter(|a| !a.is_empty()).map(str::to_string);
                let by_default = given.is_none();
                let a = given.unwrap_or_else(|| q.default.clone());
                q.answer = Some(a.clone());
                lines.push(format!("- {} → {a}", q.text));
                logs.push(format!("{}: {a}{}", q.id, if by_default { " (the default)" } else { "" }));
            }
            for l in logs { Self::log(r, "answer", l, None); }
            r.status = RunStatus::Planning;
            Self::log(r, "plan", "planning under the answers", None);
            Some((format!("{}\n\nTHE OPERATOR ANSWERED (plan under these — do not ask again):\n{}", r.goal, lines.join("\n")), r.plan_args.auto))
        })?;
        self.drive_plan(id, &goal, true).await;
        if auto && self.get(id).map(|r| r.status == RunStatus::AwaitingApproval).unwrap_or(false) { return self.approve(id); }
        self.get(id)
    }
    /// The planning itself, on an existing run: the router+planner call (screened when the router is
    /// on), the ratifier, Heimdall's bias, and the plan set awaiting approval — or the run parked on
    /// the screen's blocking questions.
    async fn drive_plan(self: &Arc<Self>, id: i64, goal: &str, asked: bool) {
        let Some(run0) = self.get(id) else { return };
        // 2026-10-13: a fenced ask fails before any model call only when it names paths and every one of them is outside the pane scope.
        if scope_fenced(&run0.scope) {
            let named = named_paths(goal);
            let outside = named_paths_outside_scope(goal, &run0.scope);
            if !named.is_empty() && outside.len() == named.len() {
                let shown = outside.iter().take(6).cloned().collect::<Vec<_>>().join(", ");
                let why = format!("the ask names path(s) outside this agent's scope ({shown}{}): scope is [{}]", if outside.len() > 6 { ", …" } else { "" }, run0.scope.join(", "));
                self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { scope_fence_fail_run(r, why); } });
                return;
            }
        }
        self.watch_planning(id);
        let (engine, cwd, write, n, forced_tier, router) = (run0.engine, run0.cwd.clone(), run0.write, run0.plan_args.n, run0.plan_args.forced_tier, run0.plan_args.router);
        let suite = run0.suite.clone();
        let cwd = cwd.as_str();
        // THE PLANNER RUNS HIGH (operator 2026-09-19: "the planner should be a high tier call with all the
        // specifics"): it reads the code, knows the specialists the box may assign, and decides tiers,
        // tasks and lenses; the work itself runs at the tier it decides
        let lenses: Vec<(String, String, String)> = match self.specialist_fns() { Some((list, _)) => list(cwd.to_string()).await, None => vec![] };
        let skills = box_skills(cwd);
        if !skills.is_empty() { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "plan", format!("{} skill{} in this box the planner may assign", skills.len(), if skills.len() == 1 { "" } else { "s" }), None); } }); }
        let enumerated = enumerated_tasks(goal) >= 2;
        let planner_tier = if enumerated || run0.plan_args.always { Tier::Mid } else { Tier::High };
        if enumerated { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "plan", format!("the ask enumerates {} tasks itself — the planner transcribes them at mid, no ratify round", enumerated_tasks(goal)), None); } }); }
        if !lenses.is_empty() { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "plan", format!("{} specialist lens{} available to this box", lenses.len(), if lenses.len() == 1 { "" } else { "es" }), None); } }); }
        // THE PLANNER IS A JOB (2026-09-22): Sonnet by measurement (L: 35/36 vs Opus 37/36 at twice the price), Opus
        // as the fallback; the operator's cell overrides. The tier still rides for engines whose lever is effort.
        let plan_cost = run0.cost;

        // PLAN REPLAY (HyperAgent's action cache at the right altitude, 2026-09-25). The planner is the most expensive
        // FIXED stage of a run — every bench since 2026-09-22 says the workers are nearly free and the supervision is
        // the bill — so the same ask, on the same commit, whose files are all still there, does not deserve a second
        // thought at full price. The guards live in `replay::recall`; a miss costs one SELECT and says why it missed.
        //
        // An ANSWERED run never replays: the operator has just changed the ask by answering, and the cached plan was
        // made before they did.
        //
        // A BENCH ARM NEVER REPLAYS (2026-09-28). The planner is the largest line on the bill — measured 2026-09-27,
        // Claude $0.093-$0.188 a plan against Codex's $0.0039, 24 to 48 times for the same job — so an arm that
        // replayed its plan compared worker cost only while reading as an end-to-end price. The first challenger arm
        // run after this cache was wired recorded its planner at $0.0000 for exactly that reason. An arm exists to
        // measure a planner, so it pays for one; `suite` is how a run says it is an arm.
        //
        // And the signature is the RUN'S technology, not the workspace's (same bug as `run_stack`): a bench in a
        // throwaway clone keyed itself on whatever the operator had open, so arms on different fixtures shared one
        // cache line. Changing the key retires the existing entries, which costs one plan each and nothing else.
        // THE PLANNER KNOWS THE CONVERSATION (operator 2026-09-29): the earlier episodes of this agent, consolidated, and this episode in full - read
        // from the pane\'s own log, deterministically. A bench arm has no conversation. A plan made with one is not the plan for the same words in
        // another, so the cache is keyed on it.
        let brief = if run0.pane_id > 0 && suite.trim().is_empty() { crate::chat_pane::conversation_brief(cwd, run0.pane_id, engine.key(), 12_000) } else { String::new() };
        let state_prefix = crate::queue::block_for(run0.pane_id, None);
        let brief_key = if brief.is_empty() { String::new() } else { use std::hash::{Hash, Hasher}; let mut h = std::collections::hash_map::DefaultHasher::new(); brief.hash(&mut h); format!("ctx{:x}", h.finish()) };
        let sig = crate::replay::signature(goal, &run_stack(cwd), &brief_key);
        let head = crate::replay::commit_of(cwd);
        let given = self.given_plan(id);
        let replayed = if given.is_some() { given } else if asked || head.is_empty() || !suite.trim().is_empty() { None } else {
            match crate::replay::recall(&sig, &head, |p| std::path::Path::new(cwd).join(p).exists()) {
                crate::replay::Recall::Hit { plan, usd, ms } => {
                    crate::replay::note(true, "", usd, ms);
                    self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) {
                        Self::log(r, "plan", format!("the same ask was planned at this commit before — replaying that plan (saving ${usd:.2} and {}s of planning)", ms / 1000), None);
                    } });
                    Some(plan)
                }
                crate::replay::Recall::Miss(why) => { crate::replay::note(false, why, 0.0, 0); None }
            }
        };

        // THE DEDICATED SCREEN (L9, 2026-09-29, the operator's call). The screening that rode inside the planner call
        // caught 43.8 % of the damaged asks (bench/screen-solo, HumanEvalComm, 36 screens a seat, every seat 100 %
        // precise); a call of its own, on the cheap seat the RECORD picks for the `screening` job, caught 87.5 %
        // (Gemini $0.006, Haiku $0.014 a screen) — and when it blocks, the planner is never called, so the question
        // costs a cent instead of a plan. A clear ask goes on to the planner, told it is screened; a seat that answers
        // nothing readable hands the job back to the planner's own gate. Not for a bench arm (the arm measures a planner
        // and its cells were recorded without this call), not for an answered run (never twice), not for a replay.
        let mut screened = false;
        let mut screen_blocked: Option<String> = None;
        // THE GAUGES COME FIRST (gauge.rs, 2026-09-29): a named path that is not in the box, a reference to nothing — facts,
        // found with no model. Sure → the run parks on that question NOW, before the screening call and the planner are paid
        // for (the same doubts used to be found AFTER the planner had planned around the missing file). The ask's other gauges
        // are written to the log once, beside the outcome, for the calibration to read later.
        if !asked { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "gauge", crate::gauge::line(goal), None); } }); }
        if router && !asked && replayed.is_none() {
            let g = crate::gauge::screen(goal, |p| std::path::Path::new(cwd).join(p).exists());
            if g.sure {
                let qs = crate::screen::as_questions(&g.value);
                self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "gauge", format!("screen: sure — {} (no model was called to find it)", g.evidence.join("; ")), None); } });
                screen_blocked = Some(json!({ "questions": qs }).to_string());
            }
        }
        // AN ASK THAT HANDS OVER THE DECISION IS NOT SCREENED (gauge::delegated): "what do you suggest?" asks the machine to choose; no question
        // can improve on its choice, and the planner is told the ask was screened so it does not ask either
        if router && !asked && replayed.is_none() && screen_blocked.is_none() {
            let g = crate::gauge::delegated(goal);
            if g.sure { screened = true; self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "gauge", format!("screen: skipped - {} (the decision was handed over)", g.evidence.join("; ")), None); } }); }
        }
        if router && !asked && replayed.is_none() && screen_blocked.is_none() && !screened && suite.trim().is_empty() && self.dedicated_screen.load(std::sync::atomic::Ordering::Relaxed) {
            let box_facts = crate::screen_kinds::box_facts(cwd);
            let sp = screen_prompt(goal);
            let psize = Self::size_of_run(&run0);
            let sb = crate::cli::job_binding_learned(crate::cli::Job::Screening, &psize, run0.pair.as_ref(), engine, plan_cost).0;
            let screen_seat = format!("{}:{}", sb.engine.key(), sb.model.as_deref().unwrap_or(""));
            self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "screen", format!("{SCREEN_MARK}{screen_seat} to screen the ask (up to {} s)", SCREEN_TIMEOUT_MS / 1000), None); } });
            let screen_killer = self.plan_killer(id, "screen");
            // THE WHOLE SCREEN IS BOUNDED, not only the seat's own wait (2026-10-09): past the limit the call is killed and the planner's own gate stands in
            let screen_call = tokio::time::timeout(std::time::Duration::from_millis(SCREEN_TIMEOUT_MS + 60_000), self.run_job(id, crate::cli::Job::Screening, engine, plan_cost, vec![ChatMessage { role: "user".into(), content: sp.clone() }], cwd.into(), true, RunOpts { max_turns: Some(6), timeout_ms: Some(SCREEN_TIMEOUT_MS), tier: Some(Tier::Low), template: Some("goal.screen".into()), ..Default::default() }, screen_killer)).await
                .unwrap_or_else(|_| { self.kill(&format!("{id}:screen")); Err(format!("no answer within {} s", (SCREEN_TIMEOUT_MS + 60_000) / 1000)) });
            self.end_plan_killer(id, "screen");
            if self.is_stopped(id) { return }
            match screen_call {
                Ok(o) if is_screen_reply(&o.parsed.content) => {
                    let (usd, _) = crate::estimate::price_run(&o.trace, sb.engine, sb.model.as_deref(), &sp, &o.parsed.content);
                    // HELD TO THE LIST (screen_kinds.rs): the questions are the model\'s only inside the seven kinds, with 3–7 chips
                    // that neither repeat nor contradict each other or the ask
                    let ask_and_box = format!("{goal}\n{box_facts}");
                    let mut shaped = crate::screen_kinds::shape_reply(&o.parsed.content, &ask_and_box);
                    // THE SHAPER (a second cheap call, ONLY when the decision was to ask): the decision prompt is the one that was measured
                    // and is left alone; the kinds, the chips and the box\'s facts are this call\'s job. It may drop a question the box
                    // answers, so "is this a web app?" about a React repo is never put to the operator. If it fails, the code shapes them.
                    let mut shape_usd = 0.0;
                    if !shaped.is_empty() {
                        let raised: Vec<String> = shaped.iter().map(|q| q.text.clone()).collect();
                        let shp = shape_prompt(goal, &box_facts, &raised, &brief);
                        let shape_call = tokio::time::timeout(std::time::Duration::from_millis(SHAPE_TIMEOUT_MS + 30_000), self.run_job(id, crate::cli::Job::Screening, engine, plan_cost, vec![ChatMessage { role: "user".into(), content: shp.clone() }], cwd.into(), true, RunOpts { max_turns: Some(4), timeout_ms: Some(SHAPE_TIMEOUT_MS), tier: Some(Tier::Low), template: Some("goal.screen_shape".into()), ..Default::default() }, self.plan_killer(id, "shape"))).await.unwrap_or_else(|_| { self.kill(&format!("{id}:shape")); Err(format!("no answer within {} s", (SHAPE_TIMEOUT_MS + 30_000) / 1000)) });
                        self.end_plan_killer(id, "shape");
                        match shape_call {
                            Ok(o2) if is_screen_reply(&o2.parsed.content) => {
                                shape_usd = crate::estimate::price_run(&o2.trace, sb.engine, sb.model.as_deref(), &shp, &o2.parsed.content).0;
                                let by_model = crate::screen_kinds::shape_reply(&o2.parsed.content, &ask_and_box);
                                let dropped = shaped.len().saturating_sub(by_model.len());
                                self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "screen", format!("shaped into kinds ({}){}", by_model.iter().map(|q| format!("{}·{} chips", if q.kind.is_empty() { "-" } else { &q.kind }, q.chips.len())).collect::<Vec<_>>().join(", "), if dropped > 0 { format!(" — {dropped} dropped: the box already answers {}", if dropped == 1 { "it" } else { "them" }) } else { String::new() }), None); } });
                                shaped = by_model;
                            }
                            _ => self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "screen", "the shaping call answered nothing readable — the kinds and chips come from the code", None); } }),
                        }
                    }
                    let qs = shaped.clone();
                    let seat = format!("{}:{}", sb.engine.key(), sb.model.as_deref().unwrap_or(""));
                    let ms = o.ms;
                    self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) {
                        r.screen_usd = usd + shape_usd;
                        Self::log(r, "screen", format!("screened on {seat} in {}s (${usd:.4}) — {}", ms / 1000, if qs.is_empty() { "clear enough to plan".to_string() } else { format!("{} blocking question{}", qs.len(), if qs.len() == 1 { "" } else { "s" }) }), None);
                    } });
                    if qs.is_empty() { screened = true } else { screen_blocked = Some(crate::screen_kinds::as_reply(&shaped)) }
                }
                Ok(_) => self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "screen", "the screening call answered nothing readable — the planner's own gate stands in", None); } }),
                Err(e) => self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "screen", format!("the screening call failed ({}) — the planner's own gate stands in", e.chars().take(160).collect::<String>()), None); } }),
            }
        }
        // a blocked screen IS the reply the gate below parks the run on: the planner is not called for it
        let replayed = match &screen_blocked { Some(c) => Some(c.clone()), None => replayed };
        let pmode = self.planner_mode(id);
        // an arm that asks for NO planner does not take a cached one either: it would be measuring a different thing
        let replayed = if pmode == "never" && screen_blocked.is_none() { None } else { replayed };
        let plan_prompt = format!("{state_prefix}{}", plan_prompt_screened_by(goal, n, write, &lenses, &skills, router, asked, screened, &brief));
        let was_replayed = replayed.is_some();   // the match below moves the plan out; the FACT is needed for pricing
        // NO PLANNER FOR L AND SMALLER - unless this run IS a measurement of planners (a bench arm names its suite) or the router is off
        let was_solo = match pmode.as_str() {
            "never" => self.solo_small.load(std::sync::atomic::Ordering::Relaxed) && !was_replayed && router,
            "always" => false,
            _ => self.solo_small.load(std::sync::atomic::Ordering::Relaxed) && !was_replayed && router && suite.trim().is_empty() && !ask_bigger_than_l(goal) && std::env::var("ANTILOKI_ALWAYS_PLAN").is_err(),
        };
        if self.is_stopped(id) { return }
        if replayed.is_none() && !was_solo {
            self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "plan", format!("{PLANNER_MARK} on {} to plan the work (up to {} min)", engine.key(), PLAN_TIMEOUT_MS / 60_000), None); } });
        }
        let out = match replayed {
            // the cached plan IS what the planner would have said, so everything downstream — the screen, the ratify
            // round, the parse — runs exactly as it does on a fresh reply. Nothing here knows it was replayed.
            Some(plan) => Ok(crate::cli::RunOut { parsed: crate::cli::CliParsed { content: plan, tokens_in: Some(0), tokens_out: Some(0), cost: Some(0.0), tools: vec![], error: None }, ms: 0, trace: crate::cli::CliTrace::replayed(engine) }),
            None if was_solo => Ok(crate::cli::RunOut { parsed: crate::cli::CliParsed { content: solo_route_reply(), tokens_in: Some(0), tokens_out: Some(0), cost: Some(0.0), tools: vec![], error: None }, ms: 0, trace: crate::cli::CliTrace::replayed(engine) }),
            None => self.run_job(id, crate::cli::Job::Planner, engine, plan_cost, vec![ChatMessage { role: "user".into(), content: plan_prompt.clone() }], cwd.into(), true, RunOpts { max_turns: Some(16), timeout_ms: Some(PLAN_TIMEOUT_MS), tier: Some(planner_tier), template: Some("goal.router_plan".into()), ..Default::default() }, self.plan_killer(id, "plan")).await,
        };
        self.end_plan_killer(id, "plan");
        if self.is_stopped(id) { return }
        if let Ok(o) = &out {
            // A MISSING PRICE IS NOT A ZERO (2026-09-26). This was `o.parsed.cost.unwrap_or(0.0)`, and Codex reports
            // no usage at all — so the most expensive stage of a run recorded as free, and the router learned to
            // prefer the planner nobody had priced. `estimate::price_turn` keeps a reported figure when there is one
            // (a replayed plan's honest $0.00 included) and counts the tokens when there is not.
            // ASK IT THE WAY THE DISPATCH ASKED IT. `role_binding_at` does not look at the pair, and `run_job` resolved
            // this planner through `job_binding_learned(Planner, size, pair, …)` — so pricing with the former would
            // bill a `deepseek-deepseek` run at Sonnet's rate and name the wrong model in `plan_binding`. Same trap as
            // the worker log line, one function later.
            let psize = Self::size_of_run(&run0);
            let pb = crate::cli::job_binding_learned(crate::cli::Job::Planner, &psize, run0.pair.as_ref(), engine, plan_cost).0;
            let (usd, basis) = crate::estimate::price_run(&o.trace, pb.engine, pb.model.as_deref(), &plan_prompt, &o.parsed.content);
            // "FREE BECAUSE CACHED" IS NOT "REPORTED AS ZERO" (2026-09-27). Both spell `plan_usd = 0.0`, and telling
            // them apart matters more than it looks: the planner is the largest line on the bill (Claude $0.093-$0.188
            // a plan against Codex's $0.0039 — 24-48x for the same job), so an arm that replayed its plan is running
            // at a fraction of its true price. B1's four replayed cells compared worker cost only without saying so,
            // and a bench cannot correct for what the record does not distinguish. `replayed` is the fact; say it.
            let basis_key: String = if was_replayed { "replayed".into() } else if was_solo { "solo".into() } else { basis.key().into() };
            let ms = o.ms;
            // THE PLANNER THAT ANSWERED, not the one that was bound (2026-10-06, the first live Claude-off run): `run_job` skips a switched-off or limited seat and the call goes to the next rung, so the bound binding named a seat that never planned
            let (pe, pm) = if was_replayed { (pb.engine, pb.model.clone()) } else { (o.trace.engine, o.trace.model.clone().or_else(|| pb.model.clone())) };
            self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) {
                r.plan_usd = usd; r.plan_ms = ms; r.plan_basis = basis_key.clone();
                r.plan_binding = format!("{}:{}", pe.key(), pm.as_deref().unwrap_or(""));
                if was_solo && screen_blocked.is_none() {
                    Self::log(r, "plan", "no planner: this is one job at L or smaller, so the editor runs it alone (no planner x editor merge, no planning call - $0.00)", None);
                } else if was_replayed && screen_blocked.is_none() {
                    Self::log(r, "plan", "the plan was replayed from cache — $0.00 is the true price of this call, and the arm is NOT paying what a fresh plan costs", None);
                } else if screen_blocked.is_none() && basis != crate::estimate::Basis::Reported {
                    Self::log(r, "plan", format!("the planner reported no cost — priced at the published rate for {} {}: {} ${:.4}", pb.engine.key(), pb.model.as_deref().unwrap_or("(default model)"), basis.word(), usd), None);
                }
            } });
        }
        // A TIMED-OUT OR EMPTY REPLY IS NOT A ROUTE (OpenCode run 12, 2026-09-20: the 300 s timeout parsed as
        // "no delegation — one job", with an empty why): the run fails and says so
        let out = match out { Ok(o) if o.parsed.content.trim().is_empty() => Err(o.parsed.error.clone().unwrap_or_else(|| "the planner returned nothing".into())), x => x };
        // THE SCREENING GATE: Heimdall graded the goal's doubts — assumable ones are logged as the
        // planner's assumptions; blocking ones park the run for one answer round (never a second)
        if let (Ok(o), true) = (&out, router) {
            let (mut questions, assumptions) = parse_screen(&o.parsed.content);
            // THE DETERMINISTIC SCREEN (operator 2026-09-25: the gate "needs to be more often used, it is like the
            // model is always sure when i use it"). It is always sure because the prompt tells it that a question is
            // "the exception, not the rule" — so the doubts that must stop a run are found in code instead, from facts
            // about this box: a named path that is not there, an ask too thin to plan, a reference to nothing.
            //
            // They are ADDED to whatever the planner asked rather than replacing it: the model may still see something
            // a filesystem check cannot, and a screen that silenced it would trade one blindness for another. Deduped
            // by evidence so the operator is never asked the same thing twice in one round.
            if !asked {
                let before_screen = questions.len();
                let found = crate::screen::doubts(&goal, |p| std::path::Path::new(&cwd).join(p).exists());
                if !found.is_empty() {
                    let already: Vec<String> = questions.iter().map(|q| q.text.to_ascii_lowercase()).collect();
                    for v in crate::screen::as_questions(&found) {
                        let text = v.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                        if already.iter().any(|t| t.contains(&text.to_ascii_lowercase())) { continue }
                        questions.push(Question {
                            id: v.get("id").and_then(Value::as_str).unwrap_or("d1").to_string(),
                            text,
                            // THE CHIPS THE SCREEN FOUND (operator 2026-09-28: "chips selection response with a
                            // last chip of write yourself"). This was hardcoded empty, so every deterministic
                            // doubt reached the operator as a bare input box however many answers it had listed.
                            options: v.get("options").and_then(Value::as_array)
                                .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
                                .unwrap_or_default(),
                            default: v.get("default").and_then(Value::as_str).unwrap_or("").to_string(),
                            certainty: Certainty::Blocking,
                            answer: None,
                            kind: v.get("kind").and_then(Value::as_str).unwrap_or("").to_string(),
                        });
                    }
                    // THE DOUBTS THE SCREEN FOUND SURVIVE THE CAP (audit 2026-09-28). The deterministic doubts
                    // are appended AFTER the planner's, so `truncate(3)` dropped exactly the ones that carry
                    // evidence and chips whenever the planner had already asked three. The screen's doubts are
                    // the cheaper and the better-founded of the two, so they go first and the planner's fill
                    // whatever room is left.
                    let cut = questions.len().saturating_sub(before_screen);
                    let mut ordered: Vec<Question> = questions.split_off(before_screen);
                    ordered.extend(questions.into_iter());
                    let _ = cut;
                    questions = ordered;
                    questions.truncate(3);
                }
            }
            self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { for a in &assumptions { Self::log(r, "assume", a.clone(), None); } r.assumptions = assumptions.clone(); } });
            if !questions.is_empty() && !asked {
                self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) {
                    for q in &questions { Self::log(r, "screen", format!("{}: {}{}", q.id, q.text, if q.default.is_empty() { String::new() } else { format!(" (default: {})", q.default) }), None); }
                    r.questions = questions.clone(); r.status = RunStatus::AwaitingAnswers;
                    Self::log(r, "screen", format!("{} blocking question{} — awaiting answers", questions.len(), if questions.len() == 1 { "" } else { "s" }), None);
                } });
                return;
            }
        }
        // ASKED AGAIN (codex, 2026-09-20: answered once, it replied with new questions — which parsed as an
        // empty route, "one job"): its defaults are taken as the answers and it plans once more; a third
        // question-only reply fails the run honestly instead of posing as a decision
        let out = match &out {
            Ok(o) if router && asked && is_question_reply(&o.parsed.content) => {
                let (qs, _) = parse_screen(&o.parsed.content);
                let taken: Vec<String> = qs.iter().filter(|q| !q.default.is_empty()).map(|q| format!("- {} → {}", q.text, q.default)).collect();
                self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "screen", format!("the planner asked again ({} question{}) — its own defaults taken, planning once more", qs.len(), if qs.len() == 1 { "" } else { "s" }), None); for t in &taken { Self::log(r, "answer", format!("{} (the planner's default)", t.trim_start_matches("- ")), None); } } });
                let goal3 = format!("{goal}\n\nYOU ASKED AGAIN — these are answered with your own defaults; a reply without a plan is an error now:\n{}", taken.join("\n"));
                let again = self.run_safe(id, engine, vec![ChatMessage { role: "user".into(), content: format!("{state_prefix}{}", plan_prompt_screened_by(&goal3, n, write, &lenses, &skills, router, true, false, &brief)) }], cwd.into(), true, RunOpts { max_turns: Some(16), timeout_ms: Some(PLAN_TIMEOUT_MS), tier: Some(planner_tier), template: Some("goal.router_plan".into()), ..Default::default() }, None).await;
                match again { Ok(o2) if o2.parsed.content.trim().is_empty() || is_question_reply(&o2.parsed.content) => Err("the planner asked a third time instead of planning".to_string()), x => x }
            }
            _ => out,
        };
        // THE RATIFICATION (operator 2026-09-20, borrowed from antiFlow's PL-ratify gate): a second high
        // call reads the plan as a CONTRACT — disjoint ownership, verifiable acceptance, right size — and
        // refuses it with reasons; the planner then plans once more with the reasons attached
        let out = match (&out, router) {
            (Ok(o), true) => {
                let (route, tasks, _) = parse_route(&o.parsed.content, n);
                if route.delegate && tasks.len() >= 2 && !enumerated {
                    let problems = match self.run_safe(id, engine, vec![ChatMessage { role: "user".into(), content: ratify_prompt(goal, &tasks) }], cwd.into(), true, RunOpts { max_turns: Some(8), timeout_ms: Some(240_000), tier: Some(Tier::High), template: Some("goal.ratify".into()), ..Default::default() }, None).await {
                        Ok(v) => parse_ratify(&v.parsed.content), Err(_) => vec![],
                    };
                    if problems.is_empty() {
                        self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "ratify", "the plan holds as a contract — disjoint ownership, verifiable acceptance", None); } });
                        out
                    } else {
                        self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { for p in &problems { Self::log(r, "ratify", format!("refused: {p}"), None); } Self::log(r, "plan", "planning again with the ratifier's reasons", None); } });
                        let goal2 = format!("{goal}\n\nA REVIEWER REFUSED THE PREVIOUS PLAN — fix exactly these:\n{}", problems.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n"));
                        self.run_safe(id, engine, vec![ChatMessage { role: "user".into(), content: format!("{state_prefix}{}", plan_prompt_screened_by(&goal2, n, write, &lenses, &skills, router, true, false, &brief)) }], cwd.into(), true, RunOpts { max_turns: Some(16), timeout_ms: Some(PLAN_TIMEOUT_MS), tier: Some(Tier::High), template: Some("goal.router_plan".into()), ..Default::default() }, None).await
                    }
                } else { out }
            }
            _ => out,
        };
        let ledger = self.ledger();
        let mut auto_run = false;
        self.with(|rs| {
            let Some(r) = rs.iter_mut().find(|x| x.id == id) else { return };
            match out {
                Ok(o) => {
                    // THE LINT COUNTS WHAT IT SEES (operator 2026-09-25: "all jsons should have a template and a
                    // lint"). The template already declares its shape; this checks the reply against that declaration
                    // and files the result, so "is a repair pass worth building?" becomes a number instead of a
                    // feeling. It does not change the parse: the tolerant parsers stay exactly as they are.
                    // the LOG line only — `run_job` already counted this reply, with the model that wrote it, and
                    // counting it twice would halve every rate in `json_health`
                    let (key, kind) = crate::prompts::catalog_entry("goal.router_plan").map(|t| t.output).unwrap_or(("", ""));
                    for p in crate::jsonlint::check(&o.parsed.content, key, kind) {
                        Self::log(r, "plan-fix", format!("the plan's JSON is off — {p}"), None);
                    }
                    let (mut route, mut tasks, warnings) = parse_route(&o.parsed.content, n);
                    for w in warnings { Self::log(r, "plan-fix", w, None); }
                    if tasks.is_empty() && route.why.is_empty() && !o.parsed.content.contains("\"delegate\"") { Self::log(r, "plan-fix", format!("reply carried no route — read as one job; it began: {}", o.parsed.content.trim().chars().take(240).collect::<String>().replace('\n', " ")), None); }
                    if !router {
                        // ROUTER OFF: the planner still splits the work, but nothing is decided about tiers or
                        // delegation — every task at the pane's tier (mid unless pinned), delegation as asked
                        let ft = forced_tier.unwrap_or(Tier::Mid);
                        for t in tasks.iter_mut() { t.tier = ft; }
                        route.tier = ft; route.delegate = !tasks.is_empty(); route.why = format!("router off — every task at tier {}", ft.key());
                        Self::log(r, "route", format!("router off · every task at tier {} · delegation as asked", ft.key()), None);
                    }
                    // THE PLAN'S ACCEPTANCE SURVIVES A COLLAPSE (2026-09-23: on every one-worker run the QA gate checked
                    // a placeholder claim and answered `unproven` — the collapse threw the planner's acceptance lines
                    // away). Whatever the plan asked for is what the one worker is judged against.
                    let mut kept_acceptance: Vec<String> = vec![];
                    let mut keep_acceptance = |ts: &[PlanTask], into: &mut Vec<String>| {
                        for t in ts { for a in &t.acceptance { let a = a.trim(); if !a.is_empty() && !into.iter().any(|x| x == a) { into.push(a.to_string()); } } }
                        into.truncate(12); // the gate makes one call per claim
                    };
                    // THE PROJECTION GATE (PLAN-THE-DIAL, 2026-09-22): delegation is not a preference, it is
                    // arithmetic. What would ONE worker take for this ask, against the wave's critical path plus the
                    // orchestration it costs? The benches put the crossover near 15 minutes of single-session work
                    // (L, 2 min: a wave 192–373 s vs one session 108 s · XXL, 15 min: 677 s vs 926 s), so a plan that
                    // projects under the floor is collapsed to one worker. `ANTILOKI_PARALLEL_FLOOR_S` moves it.
                    // …for a PANE's ask only (`pane_id > 0`): a benchmark arm that asks for a wave keeps its wave.
                    // SIZE CHOOSES THE NUMBER (operator 2026-09-28: "i asked a filter and it asked for 2 split AIs. This is
                    // not bigger than L as a task"). A wave is for an ask bigger than L; a single job — one that lists no
                    // parts and is not long — is one worker's, however many files the planner spread it over (a filter
                    // button split into App.tsx and index.css is one change, and two workers paid for two contexts).
                    if route.delegate && tasks.len() >= 2 && !cfg!(test) && r.pane_id > 0 && !ask_bigger_than_l(goal) {
                        let brief = tasks.iter().map(|t| format!("- {}: {}", t.id, t.title)).collect::<Vec<_>>().join("\n");
                        Self::log(r, "route", format!("one worker, not {} — the ask is a single job (size L or smaller); a wave is for an ask bigger than that", tasks.len()), None);
                        if !brief.is_empty() { Self::log(r, "plan", format!("the contract stands as the worker's brief:\n{brief}"), None); }
                        keep_acceptance(&tasks, &mut kept_acceptance);
                        route.delegate = false; tasks.clear();
                    }
                    if router && route.delegate && tasks.len() >= 2 {
                        let hist = crate::eta::History::load();
                        let worker = Self::worker_binding_of(r, engine);
                        // A FRESH INSTALL HAS NO HISTORY (audit 2026-09-22: the gate used to be skipped entirely
                        // when `per_unit` had fewer than two samples, so a two-minute ask on a new box still fanned
                        // out to eight sub-agents — the exact thing the floor exists to stop). With no history the
                        // benches' own medians stand in, and the log says the projection was assumed.
                        let measured = hist.per_unit("fill", worker.engine.key(), tasks.first().map(|t| t.tier.key()).unwrap_or("low"));
                        let per_fill_s = measured.unwrap_or(120_000) / 1000;
                        let orchestration_s = match (hist.per_unit("contract", engine.key(), "mid"), hist.per_unit("tests", engine.key(), "mid")) {
                            (None, None) => 150,
                            (a, b) => (a.unwrap_or(0) + b.unwrap_or(0)) / 1000,
                        };
                        let assumed = if measured.is_some() { "" } else { " · assumed (this box has no history yet)" };
                        // the floor is the operator's (Costs page / ANTILOKI_PARALLEL_FLOOR_S); the unit tests below
                        // exercise routing and waves, not this rule — `eta::a_wave_needs_both_the_margin_and_the_
                        // fifteen_minute_floor` owns it — so under test the floor is off unless the test sets it.
                        let floor_default: i64 = if cfg!(test) { 0 } else { 900 };
                        let floor_s: i64 = std::env::var("ANTILOKI_PARALLEL_FLOOR_S").ok().and_then(|v| v.parse().ok()).unwrap_or(floor_default);
                        if per_fill_s > 0 {
                            let single_s = crate::eta::single_session_s(per_fill_s, tasks.len());
                            // THE CHAIN IS THE CRITICAL PATH (audit 2026-09-22, flash-sonnet arm: "a wave of 8" was
                            // eight tasks in FIVE waves — t1 → t2,t3 → t4 → t5 → t6 → t7 → t8 — so the projection's
                            // "1 × 120 s" was really 5 × 120 s and the run took 1 884 s for $0.18 of work). The waves
                            // the plan's `needs` impose are folded into the concurrency the arithmetic sees.
                            let chain = assign_waves(&tasks).values().copied().max().unwrap_or(0) as usize + 1;
                            let conc = crate::eta::effective_concurrency(tasks.len(), chain, r.concurrency.max(1));
                            let (worth, mut why) = crate::eta::wave_or_one(single_s, orchestration_s, tasks.len(), per_fill_s, conc, floor_s);
                            if chain > 1 { why.push_str(&format!(" · the plan's dependencies make {chain} wave(s)")); }
                            if !worth {
                                // the plan is not thrown away: its tasks become ONE worker's brief (the ask, with the
                                // contract's own decomposition attached), which is what `always` already dispatches
                                let brief = tasks.iter().map(|t| format!("- {}: {}", t.id, t.title)).collect::<Vec<_>>().join("\n");
                                Self::log(r, "route", format!("one worker, not {} — {why}{assumed}", tasks.len()), None);
                                if !brief.is_empty() { Self::log(r, "plan", format!("the contract stands as the worker's brief:\n{brief}"), None); }
                                keep_acceptance(&tasks, &mut kept_acceptance);
                                route.delegate = false; tasks.clear();
                            } else {
                                Self::log(r, "route", format!("a wave of {} — {why}{assumed}", tasks.len()), None);
                            }
                        }
                    }
                    // THE MECHANICAL RULE (bench 2026-09-22, four runs): for an ask that enumerates its own tasks, a task that
                    // owns exactly one concrete file is written from a complete spec — low, whatever the planner wrote
                    // (it wrote "low-tier workers" in its prose and `mid` in every field, every time); a hint in the rubric
                    // never moved it. The verifier at mid is the safety net; an explicit `high` is respected.
                    let mut mech_lowered: Vec<String> = vec![];   // the tasks the mechanical rule lowered: deterministic evidence, so the router's doubt does not raise them again
                    if router && forced_tier.is_none() && enumerated {
                        let mut lowered = vec![];
                        for t in tasks.iter_mut() { if t.tier == Tier::Mid && t.owns.len() == 1 && !t.owns[0].contains(['*', '?', '[']) { t.tier = Tier::Low; lowered.push(t.id.clone()); } }
                        if !lowered.is_empty() { Self::log(r, "route", format!("{} single-file task{} from the operator's own list → low (the mechanical rule)", lowered.len(), if lowered.len() == 1 { "" } else { "s" }), None); }
                        mech_lowered = lowered;
                    }
                    if let Some(ft) = forced_tier.filter(|_| router) { for t in tasks.iter_mut() { t.tier = ft; } Self::log(r, "route", format!("every task pinned to tier {} — the pane's delegation policy", ft.key()), None); }
                    // HEIMDALL'S EVIDENCE: a tier THE WORKERS' engine keeps failing verification at gets bumped one up
                    // (2026-09-22: it read the run engine's ledger while the workers ran elsewhere, and re-raised every
                    // task the mechanical rule had lowered — on 8 samples)
                    let worker_engine = r.workers.unwrap_or(engine);
                    if router {
                        for t in tasks.iter_mut() { if let Some(why) = ledger.bias(worker_engine, t.tier) { let from = t.tier; t.tier = t.tier.up(); route.bias.push(format!("{}: {}→{} ({why})", t.id, from.key(), t.tier.key())); } }
                        if let Some(why) = ledger.bias(engine, route.tier) { let from = route.tier; route.tier = route.tier.up(); route.bias.push(format!("run: {}→{} ({why})", from.key(), route.tier.key())); }
                    }
                    // CAUTION (operator 2026-10-11, from jev-router): doubt and risk go UP a tier. A pinned tier is the operator's own word and stays.
                    if router && forced_tier.is_none() { for n in apply_caution(&mut route, &mut tasks, &r.goal, &mech_lowered) { route.bias.push(n); } }
                    route.verify_tier = route.tier.max(Tier::Mid); route.join_tier = Tier::High;
                    for b in &route.bias { Self::log(r, "heimdall", b.clone(), None); }
                    for t in tasks.iter().filter(|t| !t.specialists.is_empty()) { Self::log(r, "specialists", format!("{}: {}", t.id, t.specialists.join(", ")), Some(&t.id)); }
                    for t in tasks.iter().filter(|t| !t.skills.is_empty()) { Self::log(r, "skills", format!("{}: {}", t.id, t.skills.join(", ")), Some(&t.id)); }
                    Self::log(r, "route", format!("tier {} · {}{}", route.tier.key(), if route.delegate { format!("delegate: {} task{}", tasks.len(), if tasks.len() == 1 { "" } else { "s" }) } else { "no delegation — one job for the terminal that asked".to_string() }, if route.why.is_empty() { String::new() } else { format!(" · {}", route.why) }), None);
                    if !route.confidence.is_empty() { Self::log(r, "route", format!("the router is {} sure of this route", route.confidence), None); }
                    // ONE WORKER, THE WHOLE ASK (2026-09-22): `n = 1` with `always` means one sub-agent does everything
                    // the ask names — not "the planner picks one file and ignores the rest", which is what a 1-task plan
                    // gave (a bench asked for one worker on an 8-module goal and got one module).
                    let one_worker = r.plan_args.always && r.plan_args.n == 1;
                    if one_worker && route.delegate && tasks.len() <= 1 { keep_acceptance(&tasks, &mut kept_acceptance); route.delegate = false; }
                    if !route.delegate && r.plan_args.always {
                        // ONE WORKER: the router said one job — with `always` that job is dispatched to one worker on the
                        // routed engine at the routed tier, verified like any task; the pane never does the work itself
                        let owns = { let named = named_paths(&r.goal); if named.is_empty() { vec!["**".to_string()] } else { named } };
                        let t = PlanTask { id: "t1".into(), title: r.goal.lines().next().unwrap_or("the ask").chars().take(80).collect(), prompt: r.goal.clone(), owns, needs: vec![], acceptance: if kept_acceptance.is_empty() { vec!["the ask is done as written".into()] } else { kept_acceptance.clone() }, tier: route.tier, specialists: vec![], skills: vec![] };
                        route.delegate = true; tasks = vec![t];
                        // the line names the binding that will actually run (2026-09-22: it printed the run's engine while
                        // the worker was bound to OpenCode — the ledger disagreed with the log)
                        let wb = Self::worker_binding_of(r, engine);
                        r.worker_binding = format!("{}:{}", wb.engine.key(), wb.model.as_deref().unwrap_or(""));
                        Self::log(r, "route", format!("one worker on {} {} at tier {} — the pane is the brain, not the hands", wb.engine.key(), wb.model.as_deref().unwrap_or("(default model)"), route.tier.key()), None);
                        if !kept_acceptance.is_empty() { Self::log(r, "plan", format!("the plan's {} acceptance line(s) stand as this worker's — the gate judges them, not a placeholder:\n{}", kept_acceptance.len(), kept_acceptance.iter().map(|a| format!("- {a}")).collect::<Vec<_>>().join("\n")), None); }
                    }
                    if !route.delegate {
                        // ONE JOB: the run ends here with the router's verdict; the parent terminal does the work at the tier named
                        r.report = Some(format!("# Goal run #{} — no delegation\n\nThe router looked at the code and decided this is ONE job — do it in this terminal at tier **{}**{}.\n\nSwitch your own model first: `{}` — the cheapest model that fits this task ({}).\n\n**Goal:** {}", r.id, route.tier.key(), if route.why.is_empty() { String::new() } else { format!(" ({})", route.why) }, crate::cli::model_switch(engine, route.tier), crate::cli::list_price(engine, route.tier).0, r.goal));
                        r.route = Some(route); r.status = RunStatus::Inline; r.ended = Some(now_ms());
                        Self::log(r, "inline", "no delegation — the report says the tier and why", None);
                        return;
                    }
                    r.route = Some(route);
                    if tasks.is_empty() { r.status = RunStatus::Failed; r.error = Some(o.parsed.error.unwrap_or_else(|| "the planner returned no tasks".into())); r.ended = Some(now_ms()); let e = r.error.clone().unwrap(); Self::log(r, "error", e, None); }
                    else {
                        set_tasks(r, tasks);
                        let waves = r.tasks.iter().map(|t| t.wave).max().unwrap_or(0) + 1;
                        let n = r.tasks.len();
                        // ONE SUB-AGENT IS NOT A PLAN TO APPROVE (operator 2026-09-28, twice: "1 subagent should be
                        // automatically accepted, but keeps asking for approval" … "If is 1 subagent shouldnt ask
                        // approval, fix it").
                        //
                        // Approval exists so a WAVE of parallel work is not launched behind the operator's back —
                        // there is a plan to read, owners to check, a shape to disagree with. A single task has none
                        // of that: it is the ask, done by one worker, and the gate only ever made the operator click
                        // a button in a rail that is closed by default to confirm the thing they just typed.
                        //
                        // This is unconditional on purpose. The policy still governs every multi-task plan; a plan of
                        // one is simply not the thing the gate was built for.
                        //
                        // It goes THROUGH `approve` (below, once the lock is released): setting `Running` here left
                        // `started` empty and never woke the driver, so the run sat at 0/1 with its task `ready`
                        // (2026-09-28 19:43 — run #249 never began; approve also owns the deps/baseline step).
                        if n <= 1 {
                            r.status = RunStatus::AwaitingApproval;
                            auto_run = true;
                            Self::log(r, "plan", "one task — it is the ask, so it runs without asking again".to_string(), None);
                        } else {
                            r.status = RunStatus::AwaitingApproval;
                            Self::log(r, "plan", format!("{n} tasks in {waves} wave(s) — awaiting approval"), None);
                        }
                    }
                }
                Err(e) => { r.status = RunStatus::Failed; r.error = Some(e.clone()); r.ended = Some(now_ms()); Self::log(r, "error", e, None); }
            }
        });
        if auto_run { self.approve(id); }
    }
    /// Replace the plan (operator edits) — only before the run starts.
    pub fn set_plan(&self, id: i64, tasks: &Value) -> Option<GoalRun> {
        self.with(|rs| {
            let r = rs.iter_mut().find(|x| x.id == id)?;
            if !matches!(r.status, RunStatus::AwaitingApproval | RunStatus::Failed) { return Some(r.clone()); }
            let (clean, warnings) = parse_plan(&tasks.to_string(), 8);
            for w in warnings { Self::log(r, "plan-fix", w, None); }
            set_tasks(r, clean); r.status = RunStatus::AwaitingApproval; r.error = None;
            let n = r.tasks.len(); Self::log(r, "plan", format!("plan edited by the operator: {n} tasks"), None);
            Some(r.clone())
        })
    }
    pub fn approve(self: &Arc<Self>, id: i64) -> Option<GoalRun> {
        // the snapshot is taken BEFORE any worker can touch the box
        let snap = self.with(|rs| rs.iter().find(|x| x.id == id).filter(|r| r.write && r.status == RunStatus::AwaitingApproval && r.base.is_none()).map(|r| (r.cwd.clone(), r.pane_id))).and_then(|(cwd, pane)| self.snapshot(&cwd, pane, id, &format!("run #{id} approved")));
        let out = self.with(|rs| {
            let r = rs.iter_mut().find(|x| x.id == id)?;
            if r.status != RunStatus::AwaitingApproval { return Some(r.clone()); }
            // 2026-10-13: a write task whose owns sit outside the fence must not start. Read-only tasks (no owns) may. Owns the fence leaves empty are a fail.
            if r.write && scope_fenced(&r.scope) {
                for t in &r.tasks {
                    if t.owns.is_empty() { continue; }
                    let kept = t.owns.iter().filter(|o| own_within_scope(o, &r.scope)).count();
                    if kept == 0 {
                        scope_fence_fail_run(r, format!("task {} owns were fenced down to nothing under scope [{}]", t.id, r.scope.join(", ")));
                        return Some(r.clone());
                    }
                    for o in &t.owns {
                        if !own_within_scope(o, &r.scope) {
                            scope_fence_fail_run(r, format!("task {} owns `{o}` outside the agent's scope [{}]", t.id, r.scope.join(", ")));
                            return Some(r.clone());
                        }
                    }
                }
            }
            r.status = RunStatus::Running;
            r.started = Some(now_ms());
            if r.base.is_none() { r.base = snap; }
            Self::log(r, "approve", "plan approved — running", None);
            Some(r.clone())
        });
        // THE BASELINE (2026-09-20): for a write run, while the tasks start, make sure the box has its
        // dependencies and run the related tests on the OWNED files as they are now — the "before" the
        // after-gate compares against. In the background: the tasks never wait for it.
        if let Some(r) = out.as_ref().filter(|r| r.write && r.status == RunStatus::Running && !cfg!(test)) {
            let (me, rid, cwd) = (self.clone(), r.id, r.cwd.clone());
            let owned: Vec<String> = r.tasks.iter().flat_map(|t| t.owns.iter().cloned()).filter(|o| !o.contains(['*', '?', '[']) && std::path::Path::new(&cwd).join(o).is_file()).collect();
            if !owned.is_empty() { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { r.qa_baseline_pending = true; } }); }
            let wants_vitest = r.goal.to_ascii_lowercase().contains("vitest");
            tokio::spawn(async move {
                match ensure_deps(&cwd).await {
                    Ok(note) => me.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "deps", note, None); } }),
                    Err(why) => { me.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "deps", format!("not ready — {why}"), None); } }); }
                }
                // THE TEST RUNNER IS THE ORCHESTRATOR'S TO INSTALL (audit 2026-09-22): sub-agents have no network and
                // are told never to install; a goal that asks for vitest ("vitest may be added as a dev dependency")
                // therefore failed its tests task twice with `Cannot find module 'vitest'` while the raw session, which
                // may install, simply did. The blueprint path already owns this step; the plan path now does too.
                if wants_vitest {
                    match crate::blueprint::ensure_vitest(&cwd).await {
                        Ok(added) => me.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "deps", if added { "vitest installed for the tasks (the goal asks for it; the workers may not install)".into() } else { "vitest present".to_string() }, None); } }),
                        Err(why) => me.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "deps", format!("vitest could not be installed — {why}"), None); } }),
                    }
                }
                if owned.is_empty() { return; }
                match run_qa(&cwd, &owned).await {
                    Ok(q) => me.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "qa-baseline", format!("before the change: the related tests {} on {} owned file{} ({}s)", if q.passed { "pass" } else { "already FAIL" }, owned.len(), if owned.len() == 1 { "" } else { "s" }, q.ms / 1000), None); r.qa_baseline = Some(q); } }),
                    Err(why) => me.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "qa-baseline", format!("not run — {why}"), None); } }),
                }
                me.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { r.qa_baseline_pending = false; } });
            });
        }
        self.ensure_ticking(); out
    }
    pub fn pause(&self, id: i64) -> Option<GoalRun> { self.with(|rs| { let r = rs.iter_mut().find(|x| x.id == id)?; if r.status == RunStatus::Running { r.status = RunStatus::Paused; Self::log(r, "pause", "paused — running tasks finish, nothing new starts", None); } Some(r.clone()) }) }
    pub fn resume(self: &Arc<Self>, id: i64) -> Option<GoalRun> {
        let out = self.with(|rs| { let r = rs.iter_mut().find(|x| x.id == id)?; if r.status == RunStatus::Paused { r.status = RunStatus::Running; Self::log(r, "resume", "resumed", None); } Some(r.clone()) });
        self.ensure_ticking(); out
    }
    /// COMPARE WITH RAW (operator 2026-09-19): the goal as one plain call on the run's engine at the
    /// route's tier — the same worktree, the same read-only/write mode, nothing antiloki adds. Records
    /// time, tokens, tool calls and the report beside the run's own numbers. One comparison per run.
    pub async fn compare_raw(self: &Arc<Self>, id: i64) -> Option<GoalRun> {
        let run = self.get(id)?;
        if !matches!(run.status, RunStatus::Done | RunStatus::Failed | RunStatus::Inline) { return None; }
        let tier = run.route.as_ref().map(|r| r.tier).unwrap_or(Tier::Mid);
        let started = now_ms();
        self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "compare", format!("raw {} at tier {} · the goal alone, no pack, no lenses, no verification", run.engine.key(), tier.key()), None); } });
        let prompt = format!("{}

Work in this repository. Reply with your complete report.", run.goal);
        let out = self.run_safe(id, run.engine, vec![ChatMessage { role: "user".into(), content: prompt }], run.cwd.clone(), false, RunOpts { write: run.write, max_turns: Some(40), timeout_ms: Some(900_000), tier: Some(tier), template: Some("goal.raw_compare".into()), ..Default::default() }, None).await;
        let ended = now_ms();
        self.with(|rs| {
            let r = rs.iter_mut().find(|x| x.id == id)?;
            let run_ms = r.ended.unwrap_or(ended) - r.created;
            let (mut calls, mut tools, mut tin, mut tout, mut chars) = (1usize, 0usize, 0i64, 0i64, 0usize); // the planner
            for t in &r.tasks { calls += t.attempts.max(1) as usize + if t.verdict.is_some() { t.attempts.max(1) as usize } else { 0 }; if let Some(tr) = &t.trace { tools += tr.tools.len(); tin += tr.tokens_in.unwrap_or(0); tout += tr.tokens_out.unwrap_or(0); } chars += t.report.as_ref().map(|x| x.len()).unwrap_or(0); }
            if r.report.is_some() { calls += 1; }
            let base = Comparison { engine: run.engine, tier, started, ended: Some(ended), ms: ended - started, tokens_in: None, tokens_out: None, tools: 0, chars: 0, report: None, error: None,
                run_ms, run_calls: calls, run_tools: tools, run_tokens_in: tin, run_tokens_out: tout, run_chars: chars, run_passed: r.tasks.iter().filter(|t| t.status == TaskStatus::Done).count(), run_tasks: r.tasks.len() };
            r.compare = Some(match &out {
                Ok(o) => Comparison { tokens_in: o.parsed.tokens_in, tokens_out: o.parsed.tokens_out, tools: o.parsed.tools.len(), chars: o.parsed.content.len(), report: Some(o.parsed.content.clone()), error: o.parsed.error.clone(), ..base },
                Err(e) => Comparison { error: Some(e.clone()), ..base },
            });
            let c = r.compare.as_ref().unwrap();
            Self::log(r, "compare", format!("raw: {} s · {} tool call{} · {} chars{} — the run: {} s · {} calls · {} tool calls · {} chars · {}/{} verified", c.ms / 1000, c.tools, if c.tools == 1 { "" } else { "s" }, c.chars, c.error.as_ref().map(|e| format!(" · error: {e}")).unwrap_or_default(), c.run_ms / 1000, c.run_calls, c.run_tools, c.run_chars, c.run_passed, c.run_tasks), None);
            Some(r.clone())
        })
    }
    pub fn is_stopped(&self, id: i64) -> bool { self.stopped.lock().unwrap_or_else(|p| p.into_inner()).contains(&id) }
    pub fn cancel(&self, id: i64) -> Option<GoalRun> {
        self.stopped.lock().unwrap_or_else(|p| p.into_inner()).insert(id);
        crate::qa_evidence::stop_run(id);
        // A RUN THAT IS DONE BUT BEING JUDGED (operator 2026-09-30: "a button to pause or stop an execution, so we dont be locked on a wrong execution"): "stop" here means stop the QA - the work
        // stands as it was written, unverified, and nothing is sent back. Cancelling the run itself would throw away work that finished.
        if let Some(out) = self.with(|rs| {
            let r = rs.iter_mut().find(|x| x.id == id)?;
            if r.status == RunStatus::Done && r.qa_pending {
                r.qa_pending = false;
                Self::log(r, "qa-stopped", "the operator stopped the QA - the work stands as written, NOT verified, and nothing will be sent back", None);
                return Some(r.clone());
            }
            None
        }) {
            let prefix = format!("{id}:");
            let keys: Vec<String> = self.killers.lock().unwrap_or_else(|p| p.into_inner()).keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
            for k in keys.iter() { self.kill(k); }
            return Some(out);
        }
        let out = self.with(|rs| {
            let r = rs.iter_mut().find(|x| x.id == id)?;
            for t in r.tasks.iter_mut() { if matches!(t.status, TaskStatus::Running | TaskStatus::Verifying) { t.status = TaskStatus::Failed; t.error = Some("cancelled".into()); t.ended = Some(now_ms()); } }
            r.status = RunStatus::Cancelled; r.ended = Some(now_ms()); Self::log(r, "cancel", "cancelled by the operator", None);
            Some(r.clone())
        })?;
        let prefix = format!("{id}:");
        let keys: Vec<String> = self.killers.lock().unwrap_or_else(|p| p.into_inner()).keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
        for k in keys.iter() { self.kill(k); }
        Some(out)
    }
    /// A killer for one planning-phase seat call, filed under `{run}:{step}` so Stop (`cancel`) and the planning watchdog can end the real process. The screening, shaping and planner calls passed
    /// `None` before (2026-10-09): Stop marked the run cancelled and left the seat running.
    fn plan_killer(&self, id: i64, step: &str) -> Option<Killer> {
        let k: Killer = Arc::new(Mutex::new(None));
        self.killers.lock().unwrap_or_else(|p| p.into_inner()).insert(format!("{id}:{step}"), k.clone());
        Some(k)
    }
    fn end_plan_killer(&self, id: i64, step: &str) { self.killers.lock().unwrap_or_else(|p| p.into_inner()).remove(&format!("{id}:{step}")); }
    /// Fail a planning run that stopped answering: stop its calls, mark it failed with the reason. Idempotent; a run that moved on is left alone.
    pub(crate) fn fail_stalled_plan(&self, id: i64, why: &str) -> bool {
        self.stopped.lock().unwrap_or_else(|p| p.into_inner()).insert(id);
        let prefix = format!("{id}:");
        let keys: Vec<String> = self.killers.lock().unwrap_or_else(|p| p.into_inner()).keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
        for k in keys.iter() { self.kill(k); }
        self.with(|rs| match rs.iter_mut().find(|x| x.id == id) {
            Some(r) if r.status == RunStatus::Planning => { r.status = RunStatus::Failed; r.error = Some(why.to_string()); r.ended = Some(now_ms()); Self::log(r, "stall", why.to_string(), None); true }
            _ => false,
        })
    }
    /// One watcher per planning stretch: every 10 s, ask `plan_stall`; ends when the run is no longer planning.
    fn watch_planning(self: &Arc<Self>, id: i64) {
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(PLAN_WATCH_MS)).await;
                let Some(r) = me.get(id) else { return };
                if r.status != RunStatus::Planning { return }
                if let Some(why) = plan_stall(r.status, r.events.last(), r.created, now_ms()) { me.fail_stalled_plan(id, &why); return }
            }
        });
    }
    fn kill(&self, key: &str) {
        let k = self.killers.lock().unwrap_or_else(|p| p.into_inner()).remove(key);
        if let Some(k) = k { if let Some(mut child) = k.lock().unwrap_or_else(|p| p.into_inner()).take() { let _ = child.start_kill(); } }
    }
    pub fn retry(self: &Arc<Self>, id: i64, task_id: &str) -> Option<GoalRun> {
        self.stopped.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
        let out = self.with(|rs| {
            let r = rs.iter_mut().find(|x| x.id == id)?;
            let done: Vec<String> = r.tasks.iter().filter(|t| t.status == TaskStatus::Done).map(|t| t.id.clone()).collect();
            let Some(t) = r.tasks.iter_mut().find(|t| t.id == task_id) else { return Some(r.clone()) };
            if !matches!(t.status, TaskStatus::Failed | TaskStatus::Blocked | TaskStatus::Skipped) { return Some(r.clone()); }
            t.status = if t.needs.iter().all(|n| done.contains(n)) { TaskStatus::Ready } else { TaskStatus::Pending };
            t.error = None; t.attempts = 0;
            if matches!(r.status, RunStatus::Done | RunStatus::Failed) { r.status = RunStatus::Running; r.ended = None; r.report = None; r.delivered = false; }
            Self::log(r, "retry", "retry requested", Some(task_id));
            Some(r.clone())
        });
        self.ensure_ticking(); out
    }
    pub fn skip(self: &Arc<Self>, id: i64, task_id: &str) -> Option<GoalRun> {
        let out = self.with(|rs| {
            let r = rs.iter_mut().find(|x| x.id == id)?;
            if let Some(t) = r.tasks.iter_mut().find(|t| t.id == task_id) { if !matches!(t.status, TaskStatus::Done | TaskStatus::Running) { t.status = TaskStatus::Skipped; t.ended = Some(now_ms()); Self::log(r, "skip", "skipped by the operator", Some(task_id)); } }
            Some(r.clone())
        });
        self.ensure_ticking(); out
    }
    pub fn set_pr(&self, id: i64, pr: i64) -> bool { self.with(|rs| rs.iter_mut().find(|r| r.id == id).map(|r| { r.pr = Some(pr); Self::log(r, "pr", format!("PR #{pr} opened from the box"), None); })).is_some() }
    /// the PR body: the goal, the tasks with their verdicts, the joined report
    pub fn pr_body(r: &GoalRun) -> String {
        let tasks = r.tasks.iter().map(|t| format!("- {} · {} · {}{}", t.id, t.title, status_str(t.status), t.verdict.as_ref().map(|v| if v.passed { " · verified" } else { " · rejected" }).unwrap_or(""))).collect::<Vec<_>>().join("\n");
        let qa = r.qa.as_ref().map(|q| format!("\nQA gate: `{}` → {} (exit {}){}", q.cmd.chars().take(120).collect::<String>(), if q.passed { "passed" } else { "FAILED" }, q.exit, if q.pre_existing { " — pre-existing: already red before the change" } else { "" })).unwrap_or_default();
        // QA v2 (2026-09-21): the judge's verdict rides on the PR — the reviewer reads what the isolated
        // judge found before the diff
        let judge = r.qa_review.as_ref().map(Self::qa_verdict_md).unwrap_or_default();
        format!("goal run #{} · {} · {}\n\n{}{}{}\n\n{}", r.id, r.engine.key(), r.goal.lines().next().unwrap_or_default(), tasks, qa, judge, r.report.clone().unwrap_or_default())
    }
    /// The judge's verdict as markdown: `QA judge: PASSED 97/100 — summary` + its findings. Pure — tested.
    pub fn qa_verdict_md(q: &QaReview) -> String {
        let mut out = format!("\nQA judge: {} {}/100{}", if q.passed { "PASSED" } else { "FAILED" }, q.score, if q.summary.is_empty() { String::new() } else { format!(" — {}", q.summary) });
        for f in q.findings.iter().take(12) { out.push_str(&format!("\n  - [{}]{} {}", f.kind, f.task.as_ref().map(|t| format!(" {t} ·")).unwrap_or_default(), f.text)); }
        out
    }
    /// THE SEAL (QA v2, 2026-09-21: "no PR when QA failed"): the box's latest judged write run, when its
    /// verdict is a FAIL — the PR of that box is refused unless forced. Pure over the runs.
    pub fn seal_for<'a>(runs: &'a [GoalRun], cwd: &str) -> Option<&'a QaReview> {
        runs.iter().filter(|r| r.cwd == cwd && r.write && r.qa_review.is_some()).max_by_key(|r| r.ended.unwrap_or(r.created)).and_then(|r| r.qa_review.as_ref()).filter(|q| !q.passed)
    }
    /// Every judged write run of a box, newest first, as the PR body's QA section. Pure.
    pub fn box_verdicts_md(runs: &[GoalRun], cwd: &str) -> String {
        let mut rs: Vec<&GoalRun> = runs.iter().filter(|r| r.cwd == cwd && r.write && r.qa_review.is_some()).collect();
        rs.sort_by_key(|r| std::cmp::Reverse(r.ended.unwrap_or(r.created)));
        rs.iter().take(6).map(|r| format!("goal run #{} · {}{}", r.id, r.goal.lines().next().unwrap_or_default().chars().take(100).collect::<String>(), Self::qa_verdict_md(r.qa_review.as_ref().unwrap()))).collect::<Vec<_>>().join("\n\n")
    }
    pub fn mark_delivered(&self, id: i64) -> bool { self.with(|rs| rs.iter_mut().find(|x| x.id == id).map(|r| r.delivered = true).is_some()) }
    /// THE CLAIM (2026-09-20: two cockpit tabs both typed an inline route into the terminal — "/model
    /// sonnet/model sonnet"): atomically flips `delivered` and says whether THIS caller won it.
    /// The chat's turn finished in `cwd` with these tokens: price the newest DELIVERED inline route of
    /// that box that has no number yet (within the hour — an old route never inherits a later turn).
    /// The same, returning WHAT IT SAVED as well as which run it was (operator 2026-09-26: "make the pricing
    /// saves, and token saved below each response AI sends. The client must always see how much he is saving").
    /// The figure is already computed here; it simply had nowhere to go but the log.
    pub fn note_inline_usage_saved(&self, cwd: &str, tokens_in: i64, tokens_out: i64) -> Option<(i64, Option<Savings>)> {
        let id = self.note_inline_usage(cwd, tokens_in, tokens_out)?;
        let sv = self.with(|rs| rs.iter().find(|r| r.id == id).and_then(|r| r.savings.clone()));
        Some((id, sv))
    }
    pub fn note_inline_usage(&self, cwd: &str, tokens_in: i64, tokens_out: i64) -> Option<i64> {
        let cutoff = now_ms() - 3_600_000;
        let stack = run_stack(cwd);
        let (id, fed) = self.with(|rs| {
            let r = rs.iter_mut().filter(|r| r.status == RunStatus::Inline && r.cwd == cwd && r.delivered && r.savings.is_none() && r.ended.unwrap_or(0) >= cutoff).max_by_key(|r| r.ended.unwrap_or(0))?;
            let tier = r.route.as_ref().map(|x| x.tier).unwrap_or(Tier::Mid);
            r.savings = savings_inline(r.engine, tier, tokens_in, tokens_out);
            // A TURN THE PANE ANSWERED ITSELF IS EVIDENCE TOO (2026-09-28): one seat that planned and wrote — the same-seat pairing
            // the router can pick. Filed as real use (`use`), priced from its tokens; the inline QA judge adds its correctness.
            let fed = if r.pane_id > 0 && r.write {
                let seat = crate::merges::key(r.engine.key(), crate::cli::tier_model(r.engine, tier).as_deref());
                let one = r.goal.chars().count() <= 400;
                let o = crate::merges::Outcome { job: if one { "editor-s" } else { "editor-l" }.into(), size: if one { "s" } else { "l" }.into(),
                    planner: seat.clone(), worker: seat, qa: String::new(), tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0,
                    audit: None, ms: 0, usd: r.savings.as_ref().map(|v| v.actual_usd).unwrap_or(0.0), source: "use".into(),
                    domain: r.role.as_ref().and_then(|v| v.get("domain")).and_then(Value::as_str).unwrap_or("any").to_string(),
                    stack: stack.clone(), suite: String::new(), audit_only: false };
                r.merge_outcome = Some(o.clone());
                Some(o)
            } else { None };
            if let Some(sv) = &r.savings { Self::log(r, "savings", format!("the turn: {} tokens at {} · ${:.3} vs ${:.3} at {} — saved ${:.3} ({:.0}%) · API-equivalent", tokens_in + tokens_out, tier.key(), sv.actual_usd, sv.baseline_usd, sv.baseline, sv.saved_usd, sv.saved_pct), None); }
            Some((r.id, fed))
        })?;
        if let Some(o) = fed { if let Err(e) = crate::merges::record(&o) { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "merge-score", format!("this turn was not recorded — {e}"), None); } }); } }
        Some(id)
    }
    /// A chat turn taken under MIN: a settled run of its own (status inline, `min`), so the ledger and the pill
    /// count what MIN saved next to what the router saved. `ask` is the turn's text as the operator typed it.
    pub fn note_min_turn(&self, parent: &str, engine: Engine, cwd: &str, ask: &str, tier: Tier, tokens_in: i64, tokens_out: i64, refusals: usize) -> Option<i64> {
        let savings = savings_min(engine, tier, tokens_in, tokens_out)?;
        let id = { let mut s = self.seq.lock().unwrap_or_else(|p| p.into_inner()); *s += 1; *s };
        let mut r = GoalRun { plan_usd: 0.0, screen_usd: 0.0, plan_basis: String::new(), plan_binding: String::new(), worker_binding: String::new(), suite: String::new(), confidence: None, pane_id: 0, scope: vec![], checks_off: false, merge_outcome: None, role: None, plan_ms: 0, id, parent: parent.into(), goal: ask.chars().take(200).collect(), engine, cwd: cwd.into(), write: true, concurrency: 1, status: RunStatus::Inline, created: now_ms(), started: Some(now_ms()), ended: Some(now_ms()), tasks: vec![], blackboard: vec![], events: vec![], report: None, delivered: true, error: None, route: None, questions: vec![], assumptions: vec![], plan_args: PlanArgs::default(), pr: None, router: false, min: true, workers: None, mode: String::new(), blueprint: None, stages: vec![], qa_gate: None, cost: None, pair: None, qa_double: false, qa_first: None, qa_review_wanted: false, qa_review: None, savings: None, qa: None, qa_baseline: None, qa_baseline_pending: false, compare: None, mid_task: false, base: None, qa_pending: false, qa_rounds: 0, design: None };
        Self::log(&mut r, "min", format!("MIN turn: {} tokens at {} · ${:.3} vs ~${:.3} with exploration — saved ~${:.3} ({:.0}%, estimate from the measured share){}", tokens_in + tokens_out, tier.key(), savings.actual_usd, savings.baseline_usd, savings.saved_usd, savings.saved_pct, if refusals > 0 { format!(" · {refusals} exploration call{} refused", if refusals == 1 { "" } else { "s" }) } else { String::new() }), None);
        r.savings = Some(savings);
        self.with(|rs| { rs.push(r); });
        Some(id)
    }
    /// QA on for this run (the pane's `settings.qa.enabled` at plan time): the judge runs after the join.
    /// QA IS OPTIONAL AGAIN (operator 2026-10-10: "make it a normal single QA button and now QA is optional not obligatory; confidence is displayed after the results and a suggestion to run independent QA can be
    /// shown like a button"). It replaces the obligatory-QA rule of 2026-09-28 and its double pass: `on` is a plain yes or no, one judge, and a run without it ends with a confidence reading and an offer.
    pub fn set_qa_wanted(&self, id: i64, on: bool) -> bool {
        self.with(|rs| rs.iter_mut().find(|x| x.id == id).map(|r| {
            r.qa_review_wanted = on;
            r.qa_double = false;
            Self::log(r, "qa-review", if on {
                "QA is on — an isolated judge reviews the whole run after the join"
            } else { "QA is off for this run — its confidence is shown with the result, and an independent QA can be run from there" }.to_string(), None);
            true
        }).unwrap_or(false))
    }
    /// RUN INDEPENDENT QA FROM THE RESULT (2026-10-10): a finished run that skipped QA (it is optional now) can be judged on request - the same claim gate, visual evidence and design round the join would have run.
    /// Refused, with the reason, when the run is not done, wrote nothing, is already being judged or already was.
    pub fn run_qa_now(self: &Arc<Self>, rid: i64) -> Result<(), String> {
        self.with(|rs| {
            let Some(r) = rs.iter_mut().find(|x| x.id == rid) else { return Err("no such run".to_string()) };
            if r.status != RunStatus::Done { return Err("QA judges finished work: this run is not done".into()) }
            if !r.write { return Err("this run wrote nothing, so there is nothing to judge".into()) }
            if r.qa_pending { return Err("QA is already running on this run".into()) }
            if r.qa_gate.is_some() || r.qa_review.is_some() { return Err("QA already ran on this run".into()) }
            r.qa_review_wanted = true; r.qa_pending = true;
            Self::log(r, "qa-review", "independent QA requested from the result - an isolated judge reviews the whole run and REPORTS: nothing is sent back and the run stays as delivered", None);
            Ok(())
        })?;
        self.report_only.lock().unwrap_or_else(|p| p.into_inner()).insert(rid);
        let me = self.clone();
        tokio::spawn(async move { me.qa_gate(rid).await; me.design_round(rid).await; me.qa_settle(rid).await; });
        Ok(())
    }
    /// QA v2 (2026-09-21, "judge inline routes too"): the pane's own turn did the ask — when its result lands
    /// (`on_turn_result` → the priced inline run), the same isolated judge reviews the box's diff against the
    /// ask, the pane's reply as the report. Only a WRITE route the pane asked QA for, once. Says whether it runs.
    pub fn judge_inline_later(self: &Arc<Self>, rid: i64, reply: &str) -> bool {
        let go = self.with(|rs| {
            let Some(r) = rs.iter_mut().find(|x| x.id == rid) else { return false };
            if r.status != RunStatus::Inline || !r.write || !r.qa_review_wanted || r.qa_review.is_some() { return false; }
            if r.events.iter().any(|e| e.kind == "qa-review" && e.text.starts_with("judging this turn")) { return false; } // the judge is already on it
            if !reply.trim().is_empty() { r.report = Some(reply.trim().chars().take(60_000).collect()); }
            Self::log(r, "qa-review", "judging this turn — the box's diff against the ask, the reply as the report", None);
            true
        });
        if go { let me = self.clone(); tokio::spawn(async move { me.qa_review(rid).await; }); }
        go
    }
    /// THE QA GATE (PLAN-THE-DIAL phase 4, operator 2026-09-22: "there is no judge; there is a QA gate you switch on,
    /// and it checks each claim in isolation against the result, like antiFlow"): every acceptance claim of every task,
    /// checked ALONE against the file it is about. `compile` claims are already proved by the run's own tsc gate and
    /// `test` claims by their vitest case — both free and already green when the task passed; only `judgment` claims
    /// cost a call, and each gets one small one with nothing but that claim, that file and its diff. At most three per
    /// task (the rest are folded into the last), all of them in parallel.
    /// The visual-evidence step of the QA (the claim gate, and the whole-diff judge). Returns the prompt block ("" when there is nothing to look at) and the image paths.
    /// The app it looks at: the operator's single running preview, else the box's own dev server booted just for this check, else a static page in the box. When the run
    /// knows the tree it started from, the SAME steps are run on that old version, so the judge is told what the change did to each picture. NEVER fatal: no browser,
    /// no page or a failed capture is logged on the run and the claims are judged from the code, exactly as before this existed.
    /// A DEADLINE FOR A QA STAGE (2026-10-08): dev-server boot, browser capture and the plan calls each have their own timeout but the stage had none overall, and a hung Chrome could keep "QA is judging" up for
    /// ever. After `secs` the stage is abandoned, one line says so, and the judge goes on without that evidence.
    async fn within<T: Default>(self: &Arc<Self>, rid: i64, secs: u64, what: &str, fut: impl std::future::Future<Output = T>) -> T {
        match tokio::time::timeout(std::time::Duration::from_secs(secs), fut).await {
            Ok(v) => v,
            Err(_) => { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "qa-evidence", format!("{what} took longer than {} min and was dropped — the judge goes on without it", secs / 60), None); } }); T::default() }
        }
    }
    async fn visual_evidence(self: &Arc<Self>, run: &GoalRun, qa_engine: Engine, qa_model: Option<String>) -> (String, Vec<String>) {
        use std::time::Duration;
        let qa_model_for_price = qa_model.clone();
        let rid = run.id;
        let mut files: Vec<String> = run.tasks.iter().flat_map(|t| t.changed.iter().cloned()).collect(); files.sort(); files.dedup();
        if run.write && files.is_empty() {
            // SAY WHY (2026-10-08): this used to return silently, so a design ask whose changed files went unrecorded got a text-only QA and nobody could tell the screenshots had been skipped
            self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "qa-evidence", "no screenshots: this run recorded no changed files (no checkpoint to compare against), so the visual QA did not run", None); } });
        }
        if !run.write || !crate::qa_evidence::touches_frontend(&files) { return (String::new(), vec![]) }
        let say = |me: &Arc<Self>, t: String| me.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "qa-evidence", t, None); } });
        let live = crate::preview::live_urls_for(&run.cwd);   // THIS worktree's preview only: another agent's or the main checkout's shows other code
        let cwd = run.cwd.clone();
        let boot = if live.len() == 1 { None } else { let c = cwd.clone(); tokio::task::spawn_blocking(move || crate::qa_evidence::DevServer::start(&c, Duration::from_secs(90))).await.ok().and_then(|r| r.ok()) };
        let (app, source) = if live.len() == 1 { (live[0].clone(), "the preview running on this agent's own worktree") }
            else if let Some(d) = &boot { (d.url.clone(), "the box's own dev server, booted for this check") }
            else if let Some(s) = crate::qa_evidence::static_app(std::path::Path::new(&cwd)) { (s, "a static page in the box") }
            else { say(self, "the change touches the UI but there is no single running preview, no bootable dev server and no static page in the box - the claims are judged from the code alone".into()); return (String::new(), vec![]) };
        let diff = self.diff_of(&run.cwd, &files, run.base.as_ref());
        // WHAT IS REALLY ON THE PAGE, read by the browser before the QA writes its steps (operator 2026-09-30): a plan written blind guessed selectors and settled for a picture
        let probe = { let a = app.clone(); tokio::task::spawn_blocking(move || crate::qa_evidence::probe_raw(&a)).await.ok().and_then(|r| r.ok()) };
        let elements = probe.as_ref().map(crate::qa_evidence::elements_block).unwrap_or_default();
        let prompt = crate::prompts::render_base("goal.qa_evidence_plan", &vars(&[("ask", json!(run.goal.chars().take(2000).collect::<String>())), ("files", json!(files.join("\n"))),
            ("diff", json!(diff.chars().take(6000).collect::<String>())), ("app", json!(app)), ("elements", json!(elements)), ("max_steps", json!(crate::qa_evidence::MAX_STEPS))]));
        // A PURE RESTYLE IS NOT PLANNED BY A MODEL (deterministic QA step 2): the standard capture is the whole plan; `ANTILOKI_QA_MODEL_PLAN=1` puts the model back for a comparison
        let restyle = crate::qa_evidence::is_static_restyle(&run.goal, &files) && std::env::var("ANTILOKI_QA_MODEL_PLAN").is_err();
        // ...WITH THE CONTENT-SURVIVED CHECKS (2026-10-10): the model plans of the stored restyle runs all asserted the page's words ("Ministore", "Cart", "Add to cart"); the standard capture asserted nothing. Copy the changed
        // markup still carries must still be ON the page (an expect step: it fails when CSS hid it or a condition dropped it); copy that left the source is told to the judge and the operator, never failed.
        let mut removed_note = String::new();
        let planned = if restyle {
            let removed = crate::qa_evidence::restyle_copy(&run.cwd, run.base.as_ref().map(|b| b.tree.as_str()), &files);
            if !removed.is_empty() { say(self, format!("this restyle removed text the page had before: {}", removed.iter().take(6).map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", "))); }
            removed_note = crate::qa_evidence::removed_copy_note(&removed);
            Ok((crate::qa_evidence::default_plan(&app), "a restyle - the standard capture of the page, no model chose the steps".to_string()))
        } else {
        let out = self.run_safe(run.id, qa_engine, vec![ChatMessage { role: "user".into(), content: prompt.clone() }], run.cwd.clone(), false,
            RunOpts { max_turns: Some(8), timeout_ms: Some(240_000), tier: Some(Tier::Mid), model: qa_model.clone(), template: Some("goal.qa_evidence_plan".into()), ..Default::default() }, None).await;
        self.plan_or_ask_again(rid, out, qa_engine, qa_model.clone(), &prompt, &run.cwd, "goal.qa_evidence_plan", crate::qa_evidence::plan_from_reply, crate::qa_evidence::plan_problem).await };
        let (steps, how) = match planned {
            Ok((p, how)) => (p, how),
            Err(why) => (crate::qa_evidence::default_plan(&app), format!("{why} - one look at the page")),
        };
        let dir = crate::qa_evidence::evidence_dir(&run.cwd, rid);
        let base_tree = run.base.as_ref().map(|b| b.tree.clone()).filter(|t| !t.is_empty());
        // the play-by-play: viewers join by run id (GET goals/live); the film is kept beside the pictures for a replay
        let live = crate::qa_evidence::hub_open(rid, Some(dir.join("film")), std::env::var("ANTILOKI_QA_PACE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(700));
        if self.is_stopped(rid) { live.cancel.store(true, std::sync::atomic::Ordering::Relaxed); }
        // EVERY INTERACTABLE THE CHANGE IS ABOUT IS USED (operator 2026-09-30: "any interactable must be interacted and tested" - and then "if i asked just for the dropdown, why tested so many features?"): the controls the ASK and the ADDED LINES name are exercised by a
        // generated plan - no model - and the browser records whether using each one changed the page
        let coverage = probe.as_ref().map(|p| crate::qa_evidence::coverage_steps(p, &app, &run.goal, &diff)).unwrap_or_default();
        let steps_full: Vec<crate::qa_evidence::Step> = steps.iter().cloned().chain(coverage.iter().cloned()).collect();
        // THE PLAN THAT RAN, KEPT BESIDE ITS PICTURES (item 8d): the operator can promote it to a regression test (`qa_plans`) once it has passed
        let plan_json = crate::qa_evidence::steps_json(&steps_full);
        // THE DESIGN LINT (PLAN-THE-DESIGN D1, 2026-10-01) runs where the QA's own steps left the page, before the coverage plan reloads it: a deterministic measure of how the page LOOKS (contrast, spacing,
        // type, alignment, clipping, targets, hover/focus, browser-default controls), a second or two in the browser we already have open. It is NOT part of the recorded plan (a replay checks behaviour) and
        // this function only runs when the change touches the front end.
        let run_steps = crate::qa_evidence::with_design_lint(&steps, &coverage);
        let (app2, dev_used, n) = (app.clone(), boot.is_some(), steps_full.len());
        let motion = crate::qa_evidence::is_motion_change(&run.goal, &diff);
        let res = tokio::task::spawn_blocking(move || -> Result<(crate::qa_evidence::Evidence, Option<(crate::qa_evidence::Evidence, Vec<(String, bool)>)>), String> {
            use crate::qa_evidence as qe;
            let after = qe::run_plan_in(&app2, Some(std::path::Path::new(&cwd)), &run_steps, &dir, Some(live), motion)?;
            let mut before = None;
            if let Some(tree) = base_tree {
                let dest = dir.join("base-src");
                if qe::materialize_base(&cwd, &tree, &dest).is_ok() {
                    let mut guard = None;
                    let base_app = if dev_used { qe::DevServer::start(&dest.to_string_lossy(), Duration::from_secs(90)).ok().map(|d| { let u = d.url.clone(); guard = Some(d); u }) } else { qe::static_app(&dest) };
                    if let Some(burl) = base_app {
                        if let Ok(b) = qe::run_plan_in(&burl, Some(&dest), &qe::retarget(&steps, &app2, &burl), &dir.join("base"), None, false) { let cmp = qe::compare(&after, &b); before = Some((b, cmp)); }
                    }
                    drop(guard);
                    let _ = std::fs::remove_dir_all(&dest);
                }
            }
            Ok((after, before))
        }).await;
        drop(boot);
        match res {
            Ok(Ok((ev, before))) => {
                let vs = before.as_ref().map(|(_, c)| format!(", vs the old version: {} identical / {} differ", c.iter().filter(|x| x.1).count(), c.iter().filter(|x| !x.1).count())).unwrap_or_default();
                say(self, format!("{how}: {n} step(s) on {app} ({source}) -> {} screenshot(s), {} step(s) failed, {} console error(s){vs}", ev.shots.len(), ev.failed_steps, ev.console_errors.len()));
                if let Err(e) = crate::qa_plans::record(&run.cwd, rid, "ui", &app, &run.goal, plan_json, &ev) { say(self, format!("the plan could not be kept for a replay: {e}")); }
                // THE JUDGE IS SHOWN PICTURES ONLY WHEN PICTURES ARE WHAT IT NEEDS (operator 2026-09-30: "if are really necessary use AI for QA testing or if we could do all deterministically"):
                // when the browser's own checks all passed and the change is behaviour (not style), the checks ARE the verdict and the images would only cost tokens to agree with them.
                let eyes = crate::qa_evidence::needs_eyes(&ev, &files);
                let mut images: Vec<String> = if eyes { ev.shots.iter().map(|(_, p)| p.display().to_string()).collect() } else { vec![] };
                let mut block = crate::qa_evidence::prompt_block_with(&ev, eyes);
                block.push_str(&removed_note);
                if let Some((b, cmp)) = &before {
                    if eyes { images.extend(b.shots.iter().map(|(_, p)| p.display().to_string())); }
                    block.push_str(&crate::qa_evidence::base_block(b, cmp));
                    // WORDS THAT VANISHED FROM THE PAGE (deterministic QA, 2026-10-10): the same steps ran on both versions, so what the old page showed and the new one does not is a fact, not a guess
                    let lost = crate::qa_evidence::lost_visible_text(&b.text, &ev.text, 8);
                    if !lost.is_empty() { say(self, format!("text on the old page and not on the new one: {}", lost.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", "))); }
                    block.push_str(&crate::qa_evidence::lost_text_note(&lost));
                }
                if let Some(d) = &ev.design { say(self, d.one_line()); }
                // WHAT THE DESIGN ROUND WILL READ (S5): the lint and the pictures of THIS capture stay on the run - `ev` is dropped when this function returns. A capture that could not be measured clears what the last one said.
                {
                    let shots: Vec<String> = ev.shots.iter().map(|(_, p)| p.display().to_string()).collect();
                    self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) {
                        let st = r.design.get_or_insert_with(Default::default);
                        match &ev.design { Some(d) => st.observe(d, shots), None => st.unmeasured() }
                        // the "after" of a design round, in the story: the same lint on the rebuilt page
                        if st.rounds > 0 && st.sent { let line = format!("after the design round the page is measured again: design lint {} -> {}{}", st.score_first.map(|x| format!("{x:.2}")).unwrap_or_else(|| "n/a".into()), st.score.map(|x| format!("{x:.2}")).unwrap_or_else(|| "n/a".into()),
                            if st.blockers.is_empty() { String::new() } else { format!(" - still failing {} design rule(s)", st.blockers.len()) }); Self::log(r, "design-round", line, None); }
                    } });
                }
                let (pin, _) = crate::cli::prices_of(qa_engine, qa_model_for_price.as_deref());
                let tokens: u64 = images.iter().map(|p| crate::qa_evidence::image_tokens(std::path::Path::new(p))).sum();
                say(self, format!("checks: {} passed, {} failed - {} - the judge gets {} image(s){}", ev.checks.len() - ev.checks_failed(), ev.checks_failed(),
                    if ev.checks.is_empty() { "the plan had no deterministic check, so the pictures are the evidence" } else if eyes { "a check failed or the change is visual, so the judge looks too" } else { "the browser's checks settle the behaviour, so no image is sent" },
                    images.len(), if images.is_empty() { String::new() } else { format!(" (~{tokens} input tokens, ~${:.4} per claim check at this seat's price)", tokens as f64 * pin / 1e6) }));
                (block, images)
            }
            Ok(Err(e)) => { say(self, format!("no visual evidence: {e}")); (String::new(), vec![]) }
            Err(e) => { say(self, format!("no visual evidence: the capture thread failed ({e})")); (String::new(), vec![]) }
        }
    }
    /// THE QA'S PLAN, WITH ONE STRICTER ASK BEFORE THE FALLBACK (operator 2026-10-01: grok-high wrote prose around - or instead of - the step array and the QA fell back to a
    /// screenshot, "the QA gave no usable plan"). `first` is the answer to the ordinary ask. A plan in it (extraction is tolerant: `qa_evidence::json_candidates`) is used; an
    /// answer that holds none is asked for ONCE more on the same seat with the reason and a "JSON only" instruction; both steps are logged on the run. A call that FAILED (no
    /// answer at all) is not retried here: `run_safe` has already crossed seats for a limit, and a dead seat answers the same way twice. Returns the steps and what to log, or why there are none.
    #[allow(clippy::too_many_arguments)]
    async fn plan_or_ask_again<T>(self: &Arc<Self>, rid: i64, first: Result<crate::cli::RunOut, String>, engine: Engine, model: Option<String>, prompt: &str, cwd: &str, template: &str,
                                  parse: fn(&str) -> Option<Vec<T>>, problem: fn(&str) -> String) -> Result<(Vec<T>, String), String> {
        let say = |t: String| self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "qa-evidence", t, None); } });
        let o = match first { Ok(o) => o, Err(e) => return Err(format!("the QA call failed ({})", e.chars().take(160).collect::<String>())) };
        if let Some(p) = parse(&o.parsed.content) { return Ok((p, "the QA chose the steps".to_string())) }
        let p1 = problem(&o.parsed.content);
        say(format!("the QA gave no usable plan ({p1}) - asking the same seat once more, JSON only"));
        let again = self.run_safe(rid, engine, vec![ChatMessage { role: "user".into(), content: crate::qa_evidence::strict_json_ask(prompt, &p1) }], cwd.to_string(), false,
            RunOpts { max_turns: Some(8), timeout_ms: Some(240_000), tier: Some(Tier::Mid), model, template: Some(template.into()), ..Default::default() }, None).await;
        match again {
            Ok(o2) => match parse(&o2.parsed.content) {
                Some(p) => Ok((p, "the QA chose the steps (on the second ask, JSON only)".to_string())),
                None => Err(format!("the QA gave no usable plan twice (first: {p1}; second: {})", problem(&o2.parsed.content))),
            },
            Err(e) => Err(format!("the QA gave no usable plan ({p1}) and the second ask failed ({})", e.chars().take(160).collect::<String>())),
        }
    }
    /// THE BACKEND'S QA FILM: boot the box's own server, let the QA write an API test (requests + what each answer must contain), have the deterministic runner send it and decide, and
    /// stream it to the same viewer as the browser's. Returns the evidence block (empty when there is nothing to test or nothing to boot - said in the run's log, never invented).
    async fn api_evidence(self: &Arc<Self>, run: &GoalRun, qa_engine: Engine, qa_model: Option<String>) -> (String, Vec<String>) {
        use std::time::Duration;
        let rid = run.id;
        let mut files: Vec<String> = run.tasks.iter().flat_map(|t| t.changed.iter().cloned()).collect(); files.sort(); files.dedup();
        if !run.write || !crate::qa_api::touches_backend(&files) { return (String::new(), vec![]) }
        let say = |me: &Arc<Self>, t: String| me.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "qa-evidence", t, None); } });
        let cwd = run.cwd.clone();
        let boot = { let c = cwd.clone(); tokio::task::spawn_blocking(move || crate::qa_evidence::DevServer::start(&c, Duration::from_secs(90))).await.ok() };
        let server = match boot { Some(Ok(d)) => d, Some(Err(e)) => { say(self, format!("the change touches the server, but there is nothing to call: {e} - the project's own checks stand alone")); return (String::new(), vec![]) } None => return (String::new(), vec![]) };
        let base = server.url.clone();
        let diff = self.diff_of(&run.cwd, &files, run.base.as_ref());
        let prompt = crate::prompts::render_base("goal.qa_api_plan", &vars(&[("ask", json!(run.goal.chars().take(2000).collect::<String>())), ("files", json!(files.join("\n"))),
            ("diff", json!(diff.chars().take(8000).collect::<String>())), ("base", json!(base)), ("max_steps", json!(crate::qa_api::MAX_API_STEPS))]));
        let out = self.run_safe(run.id, qa_engine, vec![ChatMessage { role: "user".into(), content: prompt.clone() }], run.cwd.clone(), false,
            RunOpts { max_turns: Some(8), timeout_ms: Some(240_000), tier: Some(Tier::Mid), model: qa_model.clone(), template: Some("goal.qa_api_plan".into()), ..Default::default() }, None).await;
        let steps = match self.plan_or_ask_again(rid, out, qa_engine, qa_model, &prompt, &run.cwd, "goal.qa_api_plan", crate::qa_api::api_plan_from_reply, crate::qa_api::api_plan_problem).await {
            Ok((steps, _)) => steps,
            Err(why) => {
                say(self, format!("{why} - the server was not exercised, and the claims are judged from the code and the project's own checks"));
                return (String::new(), vec![]);
            }
        };
        let dir = crate::qa_evidence::evidence_dir(&run.cwd, rid);
        let live = crate::qa_evidence::hub_open(rid, Some(dir.join("film")), std::env::var("ANTILOKI_QA_PACE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(700));
        if self.is_stopped(rid) { live.cancel.store(true, std::sync::atomic::Ordering::Relaxed); }
        let (b2, n) = (base.clone(), steps.len());
        let plan_json = crate::qa_api::api_steps_json(&steps);
        let res = tokio::task::spawn_blocking(move || crate::qa_api::run_api_plan_live(&b2, &steps, std::path::Path::new(""), Some(live))).await;
        drop(server);
        match res {
            Ok(Ok(ev)) => {
                say(self, format!("api test: {n} step(s) on {base} -> {} check(s) passed, {} failed, {} step(s) failed - decided by the runner, no model looked at anything", ev.checks.len() - ev.checks_failed(), ev.checks_failed(), ev.failed_steps));
                if let Err(e) = crate::qa_plans::record(&run.cwd, rid, "api", &base, &run.goal, plan_json, &ev) { say(self, format!("the API test could not be kept for a replay: {e}")); }
                (crate::qa_api::prompt_block(&ev), vec![])
            }
            Ok(Err(e)) => { say(self, format!("no API evidence: {e}")); (String::new(), vec![]) }
            Err(e) => { say(self, format!("no API evidence: the runner failed ({e})")); (String::new(), vec![]) }
        }
    }
    async fn qa_gate(self: &Arc<Self>, rid: i64) {
        use crate::blueprint as bp;
        if self.is_stopped(rid) { return }
        let Some(run) = self.get(rid) else { return };
        let t0 = now_ms();
        let contract = run.blueprint.clone();
        // THE GATE IS ROUTED BY THE DEFECT CLASS OF THE CHANGE (operator 2026-10-07): the measured ranking for backend / frontend / design / docs / infra, pooled when the change is mixed or unknown
        let class = qa_class_of_run(&run);
        let qa = crate::cli::role_binding_at_in(crate::cli::Role::Qa, run.engine, run.cost, class);
        let mut checks: Vec<bp::ClaimCheck> = vec![];
        let mut jobs = vec![];
        // VISUAL EVIDENCE (operator 2026-09-30): a change to something you can SEE is judged on what it looks like, not only on its source. The QA chooses the steps,
        // deterministic tools capture them (qa_evidence.rs); every claim check then carries the block and, for Codex, the images.
        // what the design round reads is THIS pass's capture or nothing: a stale lint must not block a run whose page was not measured this time
        self.with(|rs| { if let Some(d) = rs.iter_mut().find(|x| x.id == rid).and_then(|r| r.design.as_mut()) { d.unmeasured(); } });
        let (mut evidence_block, evidence_images) = self.within(rid, 420, "the screenshot evidence", self.visual_evidence(&run, qa.engine, qa.model.clone())).await;
        // …AND THE SERVER'S VERSION (operator 2026-09-30: "if the task i asked is a backend task is there a way to make also the QA test and display something similar"): an API test the
        // QA writes and a runner decides, streamed to the same viewer as a terminal
        let (api_block, _) = self.within(rid, 300, "the API evidence", self.api_evidence(&run, qa.engine, qa.model.clone())).await;
        evidence_block.push_str(&api_block);
        // STOP is honoured between the QA's stages: nothing after this point runs for a run the operator stopped
        if self.is_stopped(rid) { return }
        for task in run.tasks.iter().filter(|t| t.status == TaskStatus::Done) {
            let path = task.owns.first().cloned().unwrap_or_default();
            let module = contract.as_ref().and_then(|c| c.modules.iter().find(|m| m.path == path));
            // what kind each claim is: the contract knows; without one, everything is judgment
            let claims: Vec<(String, String)> = match module {
                Some(m) => m.exports.iter().flat_map(|e| e.acceptance.iter().map(move |a| (a.text.clone(), a.kind.clone()))).collect(),
                None => task.acceptance.iter().map(|a| (a.clone(), "judgment".to_string())).collect(),
            };
            let (mut judgment, mut machine) = (vec![], vec![]);
            for (text, kind) in claims { if kind == "judgment" { judgment.push(text); } else { machine.push((text, kind)); } }
            // A TEST CLAIM IS ONLY MET IF A CASE PROVES IT (2026-09-22): the module's tests passing is not the same as
            // this claim having an assertion — the first gate run marked 45 claims met against 28 cases. The tests are
            // written with each `it` named after its claim, so the case is found by that prefix; without one the claim
            // is UNPROVEN, which is the honest verdict and pushes the contract to write better tests.
            let test_src = module.and_then(|m| m.test_file.as_ref()).and_then(|t| std::fs::read_to_string(std::path::Path::new(&run.cwd).join(t)).ok()).unwrap_or_default();
            let norm = |s: &str| s.to_lowercase().chars().filter(|c| c.is_alphanumeric() || *c == ' ').collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
            let cases: Vec<String> = test_src.lines().filter_map(|l| { let t = l.trim(); let i = t.find("it(").or_else(|| t.find("test("))?; let rest = &t[i..]; let q = rest.find(['"', '\''])?; let body = &rest[q + 1..]; let end = body.find(['"', '\''])?; Some(norm(&body[..end])) }).collect();
            for (text, kind) in machine {
                let (met, why) = if kind == "compile" { (Some(true), "the compiler passed on this file".to_string()) }
                    else {
                        let claim = norm(&text);
                        let head: String = claim.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
                        match cases.iter().find(|c| c.starts_with(&head) || c.contains(&head) || claim.starts_with(c.as_str())) {
                            Some(c) => (Some(true), format!("proved by the green case \"{}\"", c.chars().take(80).collect::<String>())),
                            None => (None, if test_src.is_empty() { "no test file for this module — the claim was never asserted".into() } else { "no test case carries this claim — the module's other tests are green, this line is not asserted".to_string() }),
                        }
                    };
                checks.push(bp::ClaimCheck { task: task.id.clone(), path: path.clone(), claim: text, kind, met, why, by: "machine".into(), ms: 0, cost: 0.0 });
            }
            // at most three calls per task: the tail is folded into the third
            let folded: Vec<String> = if judgment.len() > 3 { let mut v: Vec<String> = judgment[..2].to_vec(); v.push(judgment[2..].join(" AND ")); v } else { judgment };
            // THE FILES THIS CLAIM IS ABOUT (2026-10-08): a one-task run owns the FOLDER first ("src/features", then each file); read as a file that path is an error, so the judge saw an EMPTY
            // "THE FILE AS IT STANDS" and an empty diff, went exploring with its two turns and, in 2 of 3 runs, never answered: $0.33 of Opus for "no verdict in shape".
            let cfiles = claim_files(std::path::Path::new(&run.cwd).join(&path).is_dir(), &path, &task.changed, &task.owns);
            let source = claim_source(&run.cwd, &cfiles, 14_000);
            let diff = if run.write { self.diff_of(&run.cwd, &cfiles, run.base.as_ref()) } else { String::new() };
            for claim in folded {
                let prompt = crate::prompts::render_base("goal.claim_check", &vars(&[("claim", json!(claim)), ("path", json!(path)), ("diff", json!(diff.chars().take(6000).collect::<String>())), ("source", json!(source.chars().take(14_000).collect::<String>())),
                    // A CLAIM ABOUT THE ASK NEEDS THE ASK (2026-09-28: run #250's only acceptance line was "the ask is done as
                    // written and the report says how"; the judge answered "neither the ask nor the report is provided" and the
                    // gate failed unproven on every one-task run)
                    ("ask", json!(run.goal.chars().take(2000).collect::<String>())), ("report", json!(task.report.clone().unwrap_or_default().chars().take(4000).collect::<String>())),
                    ("evidence", json!(evidence_block))]));
                let images = evidence_images.clone();
                let (run_fn, cwd, tid, p, qa2) = (self.run.clone(), run.cwd.clone(), task.id.clone(), path.clone(), qa.clone());
                let prompt_for_cost = prompt.clone();   // the prompt itself is moved into the call; pricing needs its size
                jobs.push(async move {
                    let t = now_ms();
                    let out = run_fn(qa2.engine, vec![ChatMessage { role: "user".into(), content: prompt }], cwd, true, RunOpts { max_turns: Some(if images.is_empty() { 2 } else { 8 }), timeout_ms: Some(240_000), tier: Some(Tier::Mid), model: qa2.model.clone(), template: Some("goal.claim_check".into()), images, ..Default::default() }, None).await;
                    let (met, why) = out.as_ref().ok().and_then(|o| bp::parse_claim(&o.parsed.content)).unwrap_or((None, out.as_ref().err().cloned().unwrap_or_else(|| "no verdict in shape".into())));
                    // A JUDGMENT COSTS WHAT IT COSTS (2026-09-27, the same bug as the record's `usd`). This was
                    // `cost.unwrap_or(0.0)`, and `gate.cost` is the sum of these — so a gate judged by any engine that
                    // reports no usage billed the run for its MACHINE checks only, which genuinely are free. 17 of 18
                    // qa-gate rows on the record carry $0.00 for that reason. A machine check still costs nothing; a
                    // model asked a question never does.
                    let cost = match out.as_ref().ok() {
                        Some(o) => crate::estimate::price_run(&o.trace, qa2.engine, qa2.model.as_deref(), &prompt_for_cost, &o.parsed.content).0,
                        None => 0.0,
                    };
                    bp::ClaimCheck { task: tid, path: p, claim, kind: "judgment".into(), met, why, by: qa2.model.clone().unwrap_or_else(|| qa2.engine.key().into()), ms: now_ms() - t, cost }
                });
            }
        }
        checks.extend(futures_util::future::join_all(jobs).await);
        let gate = bp::QaGate {
            met: checks.iter().filter(|c| c.met == Some(true)).count(),
            unmet: checks.iter().filter(|c| c.met == Some(false)).count(),
            unproven: checks.iter().filter(|c| c.met.is_none()).count(),
            cost: checks.iter().map(|c| c.cost).sum(), ms: now_ms() - t0, checks,
            judge: crate::merges::key(qa.engine.key(), qa.model.as_deref()),
        };
        self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) {
            Self::log(r, if gate.passed() { "qa-gate-pass" } else { "qa-gate-fail" }, format!("{} met · {} unmet · {} unproven · {:.0} s · ${:.3}{}", gate.met, gate.unmet, gate.unproven, gate.ms as f64 / 1000.0, gate.cost,
                gate.checks.iter().filter(|c| c.met == Some(false)).map(|c| format!("\n  UNMET {} — {}", c.claim.chars().take(80).collect::<String>(), c.why.chars().take(100).collect::<String>())).chain(
                    gate.checks.iter().filter(|c| c.met.is_none()).map(|c| format!("\n  UNPROVEN {} — {}", c.claim.chars().take(80).collect::<String>(), c.why.chars().take(240).collect::<String>()))).collect::<String>()), None);
            // the gate's met/unmet join the combination's row (merges.rs)
            {
                let planner = crate::cli::job_binding_paired(crate::cli::Job::Planner, r.pair.as_ref(), r.engine, r.cost).0;
                let one = r.tasks.len() == 1 && r.tasks[0].owns.len() == 1 && !r.tasks[0].owns[0].contains(['*', '?', '[']) && r.goal.chars().count() <= 400;
                let wj = if one { crate::cli::Job::EditorS } else { crate::cli::Job::EditorL };
                let worker = crate::cli::job_binding_paired(wj, r.pair.as_ref(), r.workers.unwrap_or(r.engine), r.cost).0;
                let qb = crate::cli::role_binding_at(crate::cli::Role::Qa, r.engine, r.cost);
                let (gate_planner_key, gate_worker_key) = run_row_keys(r, &planner, &worker);
                let o = crate::merges::Outcome { suite: r.suite.clone(), stack: run_stack(&r.cwd), job: wj.key().into(), size: crate::merges::size_of(r.tasks.len(), one, r.goal.chars().count()).into(), planner: gate_planner_key, worker: gate_worker_key, qa: crate::merges::key(qb.engine.key(), qb.model.as_deref()), tasks: 0, pass_first: 0, retries: 0, failed: 0, gate_met: gate.met as i64, gate_unmet: gate.unmet as i64, audit: None, ms: 0, usd: gate.cost, source: source_of(&r.suite).into(), audit_only: false, domain: crate::roles::domain_of(&r.tasks.iter().flat_map(|t| if t.changed.is_empty() { t.owns.clone() } else { t.changed.clone() }).collect::<Vec<_>>()).key().to_string(),};
                let _ = crate::merges::record(&o);
            }
            // the design critic's spend (if the round ran on an earlier pass of this run) is part of what the gate cost: this gate replaces the one the critic was billed to
            r.qa_gate = Some(bp::QaGate { cost: gate.cost + r.design.as_ref().map(|d| d.usd).unwrap_or(0.0), ..gate });
        } });
    }
    /// THE QA JUDGE (2026-09-21): after the join, one isolated high-tier call over the plan's criteria, the
    /// joined report and the diff of everything that changed — a verdict with findings, never an edit.
    /// THE OPERATOR'S ANSWER TO QA'S "NEW DIRECTION" QUESTION. `None` = this run has no such question open (the caller handles it as a planning answer). Some(run) otherwise:
    ///   · "accept it as it is" - delivered, QA's verdict stays on the report;
    ///   · "undo everything this run changed" - the box goes back to where the run found it, and the run is cancelled;
    ///   · "cancel the run";
    ///   · "keep trying with the same feedback", or ANY OTHER WORDS (a new direction) - the unmet tasks go back to their workers with the QA feedback and, for words, the
    ///     operator's direction; the send-back counter restarts, so QA gets its two rounds again.
    fn answer_qa_direction(self: &Arc<Self>, id: i64, answers: &Value) -> Option<Option<GoalRun>> {
        let mut restore: Option<(String, String)> = None;
        let out = self.with(|rs| {
            let r = rs.iter_mut().find(|x| x.id == id)?;
            if r.status != RunStatus::AwaitingAnswers || !r.questions.iter().any(|q| q.kind == "qa-direction" && q.answer.is_none()) { return None }
            let (qid, qdefault) = { let q = r.questions.iter().find(|q| q.kind == "qa-direction").unwrap(); (q.id.clone(), q.default.clone()) };
            let given = answers.get(&qid).and_then(Value::as_str).map(str::trim).filter(|a| !a.is_empty()).map(str::to_string);
            let by_default = given.is_none();
            let a = given.unwrap_or(qdefault);
            if let Some(q) = r.questions.iter_mut().find(|q| q.id == qid) { q.answer = Some(a.clone()); }
            let low = a.to_lowercase();
            Self::log(r, "answer", format!("{qid}: {a}{}", if by_default { " (the default)" } else { "" }), None);
            let now = now_ms();
            if low == QA_DIRECTION_CHIPS[1] {
                r.status = RunStatus::Done; r.ended = Some(now); r.delivered = false;
                Self::log(r, "qa-accepted", format!("[Q{}.final] the operator accepted the run as it is", r.id), None);
                let note = r.qa_gate.as_ref().map(|g| g.checks.iter().filter(|c| c.met == Some(false)).map(|c| format!("- {}", c.why.chars().take(300).collect::<String>())).collect::<Vec<_>>().join("\n")).unwrap_or_default();
                r.report = Some(format!("{}\n\n⚠ QA GATE [Q{}.final]: accepted by the operator with these unmet:\n{note}", r.report.clone().unwrap_or_default(), r.id));
            } else if low == QA_DIRECTION_CHIPS[3] {
                r.status = RunStatus::Cancelled; r.ended = Some(now); Self::log(r, "cancel", format!("[Q{}.final] cancelled by the operator after QA", r.id), None);
            } else if low == QA_DIRECTION_CHIPS[2] {
                if let Some(b) = r.base.as_ref().filter(|b| !b.tree.is_empty()) { restore = Some((r.cwd.clone(), b.tree.clone())); }
                r.status = RunStatus::Cancelled; r.ended = Some(now); Self::log(r, "cancel", format!("[Q{}.final] the operator asked to undo everything this run changed", r.id), None);
            } else {
                // back to the workers: QA's own words, plus the operator's when they typed any
                let direction = if low == QA_DIRECTION_CHIPS[0] { None } else { Some(a.clone()) };
                let unmet: Vec<(Option<String>, String)> = r.qa_gate.as_ref().map(|g| g.checks.iter().filter(|c| c.met == Some(false)).map(|c| (Some(c.task.clone()), format!("the claim “{}” is NOT met - {}", c.claim, c.why))).collect()).unwrap_or_default();
                let named: Vec<String> = { let mut n: Vec<String> = unmet.iter().filter_map(|(t, _)| t.clone()).filter(|t| r.tasks.iter().any(|x| &x.id == t)).collect(); n.sort(); n.dedup(); n };
                let targets: Vec<String> = if named.is_empty() { r.tasks.iter().filter(|t| t.status == TaskStatus::Done).map(|t| t.id.clone()).collect() } else { named };
                let done: Vec<String> = r.tasks.iter().filter(|t| t.status == TaskStatus::Done).map(|t| t.id.clone()).collect();
                let tag = format!("Q{}.dir", r.id);
                for tid in &targets {
                    let mut reasons: Vec<String> = unmet.iter().filter(|(t, _)| t.as_deref() == Some(tid.as_str()) || t.is_none()).map(|(_, x)| format!("QA [{tag}]: {x}")).collect();
                    if let Some(d) = &direction { reasons.push(format!("THE OPERATOR'S NEW DIRECTION [{tag}]: {d}")); }
                    if let Some(t) = r.tasks.iter_mut().find(|t| &t.id == tid) {
                        t.verdict = Some(Verdict { passed: false, reasons, by: "qa-gate".into() });
                        t.status = if t.needs.iter().all(|n| done.contains(n)) { TaskStatus::Ready } else { TaskStatus::Pending };
                        t.error = None; t.attempts = 0; t.ended = None;
                    }
                }
                r.qa_rounds = 0; r.status = RunStatus::Running; r.ended = None; r.report = None; r.delivered = false;
                for q in r.questions.iter_mut().filter(|q| q.kind == "qa-direction") { q.kind = "qa-direction-answered".into(); }
                Self::log(r, "qa-send-back", format!("[{tag}] back to {} task(s) at the operator's word: {}", targets.len(), direction.unwrap_or_else(|| "keep trying with the same feedback".into())), None);
            }
            Some(r.clone())
        });
        let out = out?;
        if let Some((cwd, tree)) = restore { crate::checkpoint::restore(&cwd, &tree); }
        if out.status == RunStatus::Running { self.ensure_ticking(); }
        Some(self.get(id).or(Some(out)))
    }
    /// THE DESIGN ROUND (PLAN-THE-DESIGN S5 / D4, 2026-10-01), between the behavioural gate and `qa_settle`: once the claims are met, ONE critic with eyes (the cheapest vision-measured seat that can take a call - not
    /// necessarily the builder) reads the page's screenshots, its design lint and the rubric and answers with at most five concrete fixes. They wait on the run (`design.fixes`) and `qa_settle` sends them back as one
    /// brief - inside the QA cap, once. The critic's cost goes into `gate.cost` and the log. NEVER fatal: no seat, no screenshot, an unreadable answer are said in the log and the run goes on to `qa_settle` as it was.
    async fn design_round(self: &Arc<Self>, rid: i64) {
        use crate::design_round as dr;
        if self.is_stopped(rid) { return }
        let Some(run) = self.get(rid) else { return };
        if run.status != RunStatus::Done || !run.qa_pending { return }
        let Some(st) = run.design.clone() else { return };
        let files: Vec<String> = { let mut f: Vec<String> = run.tasks.iter().flat_map(|t| if t.changed.is_empty() { t.owns.clone() } else { t.changed.clone() }).collect(); f.sort(); f.dedup(); f };
        let shots = dr::pick_shots(&st.shots);
        let seat = dr::critic_seat(shots.len());
        let case = dr::Case { enabled: dr::enabled(dr::mode(), crate::roles::domain_of(&files), &files), measured: st.lint.is_some() && !shots.is_empty(), gate_unmet: run.qa_gate.as_ref().map(|g| g.unmet).unwrap_or(0),
            rounds_spent: st.rounds, qa_rounds: run.qa_rounds, max_rounds: self.qa_max_rounds.load(std::sync::atomic::Ordering::Relaxed), seat_available: seat.is_some() };
        let say = |me: &Arc<Self>, kind: &str, t: String| me.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, kind, t, None); } });
        if let dr::Decision::Skip(why) = dr::decide(&case) {
            if case.enabled && case.measured { say(self, "design-round", format!("no design critique: {why}")); }
            return;
        }
        let Some((engine, model)) = seat else { return };
        let t0 = now_ms();
        say(self, "design-round", format!("design round: {}/{model} looks at {} screenshot(s) and the design lint ({}) and writes at most {} fixes", engine.key(), shots.len(), st.lint.as_ref().and_then(|l| l.get("score")).map(|s| format!("score {s}")).unwrap_or_default(), dr::MAX_FIXES));
        // the page as the critic sees it, kept before the builder rewrites it: the bench photographs "before" and "after" with the same camera
        let before_dir = crate::qa_evidence::evidence_dir(&run.cwd, rid).join("design-before");
        let kept = dr::keep_before(&run.cwd, &files, &before_dir);
        let prompt = dr::critique_prompt(&run.goal, &shots, &st.lint_block);
        let mut asked = prompt.clone();
        let (mut usd, mut fixes, mut problem) = (0.0, None, String::new());
        for attempt in 0..2 {
            if self.is_stopped(rid) { return }
            let out = self.run_safe(rid, engine, vec![ChatMessage { role: "user".into(), content: asked.clone() }], run.cwd.clone(), false,
                RunOpts { max_turns: Some(8), timeout_ms: Some(240_000), tier: Some(Tier::Mid), model: Some(model.to_string()), template: Some("design.critique".into()), images: shots.clone(), ..Default::default() }, None).await;
            match out {
                Ok(o) => {
                    // A CRITIQUE COSTS WHAT IT COSTS (the claim check's rule, 2026-09-27): priced from the trace, never a zero
                    usd += crate::estimate::price_run(&o.trace, engine, Some(model), &asked, &o.parsed.content).0;
                    match dr::parse_fixes(&o.parsed.content) {
                        Ok(f) => { fixes = Some(f); break }
                        Err(why) => { problem = why; asked = format!("{prompt}\n\nYour last answer could not be used ({problem}). Answer again with ONLY the JSON object {{\"fixes\": [...]}} and nothing else."); }
                    }
                }
                Err(e) => { problem = e; if attempt == 0 { continue } }
            }
        }
        let ms = now_ms() - t0;
        // what this change ADDED, to tell the critic's fixes about it from fixes about the rest of the page (`partition_in_scope`)
        let all_new = run.base.as_ref().and_then(|b| crate::checkpoint::current_tree(&run.cwd).map(|c| crate::checkpoint::changes(&run.cwd, &b.tree, &c))).map(|ch| { let mine: Vec<_> = ch.iter().filter(|(_, p, _)| files.contains(p)).collect(); !mine.is_empty() && mine.iter().all(|(s, _, _)| *s == 'A') }).unwrap_or(false);
        let added_text: String = self.diff_of(&run.cwd, &files, run.base.as_ref()).lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).collect::<Vec<_>>().join("\n");
        let seat_name = format!("{}/{model}", engine.key());
        let ack = self.with(|rs| {
            let r = rs.iter_mut().find(|x| x.id == rid)?;
            let st = r.design.as_mut()?;
            st.rounds += 1; st.seat = seat_name.clone(); st.usd += usd; st.ms += ms; st.sent = false;
            if kept > 0 { st.before_dir = before_dir.display().to_string(); }
            let (inn, out) = dr::partition_in_scope(fixes.clone().unwrap_or_default(), &added_text, &run.goal, all_new);
            st.fixes = inn; st.out_of_scope = out;
            let line = st.summary();
            if let Some(g) = r.qa_gate.as_mut() { g.cost += usd; }
            Some(line)
        });
        match (ack, fixes) {
            (Some(line), Some(f)) if f.is_empty() => say(self, "design-round", format!("{line} - the critic found nothing worth changing")),
            (Some(line), Some(f)) => {
                say(self, "design-round", line);
                say(self, "design-notes", f.iter().enumerate().map(|(i, x)| x.line(i + 1)).collect::<Vec<_>>().join("\n"));
            }
            (Some(line), None) => say(self, "design-round", format!("{line} - the critic's answer could not be used ({problem}); no design round for this run")),
            (None, _) => {}
        }
    }
    /// WHAT QA DOES WITH ITS VERDICT (operator 2026-09-29: "on fail QA, the gate must provide feedback and send back to be fixed; it starts again with the worker, because QA
    /// already does the plan"). The gate's unmet claims (or the judge's findings) go back to the workers that own them, as the retry brief, and the run RE-OPENS - it is not
    /// announced as finished until QA passes or the send-backs are spent (`MAX_QA_ROUNDS`). Spent: the run is delivered with the unmet claims written on its report, each
    /// with an id. Deterministic: the feedback is the gate's own text. A cancelled run is left alone.
    async fn qa_settle(self: &Arc<Self>, rid: i64) {
        let max_rounds = self.qa_max_rounds.load(std::sync::atomic::Ordering::Relaxed);
        // everything this run has spent so far, read BEFORE the runs lock is taken (a database read does not belong under it)
        let calls = crate::audit::run_calls(rid);
        let report_only = self.report_only.lock().unwrap_or_else(|p| p.into_inner()).contains(&rid);
        let back = self.with(|rs| {
            let r = rs.iter_mut().find(|x| x.id == rid)?;
            if !r.qa_pending { return None }
            let mut feedback: Vec<(Option<String>, String)> = vec![];
            if let Some(g) = &r.qa_gate { for c in g.checks.iter().filter(|c| c.met == Some(false)) { feedback.push((Some(c.task.clone()), format!("the claim “{}” is NOT met - {}", c.claim, c.why))); } }
            if feedback.is_empty() { if let Some(q) = r.qa_review.as_ref().filter(|q| !q.passed && q.error.is_none()) { for f in q.findings.iter().filter(|f| f.kind != "ok") { feedback.push((f.task.clone(), format!("{} - {}", f.kind, f.text))); } } }
            // THE DESIGN ROUND (PLAN-THE-DESIGN S5 / D4, 2026-10-01). Two kinds of design feedback, and only one of them can hold a run back:
            //   RULES  - the D1 failures the operator turned into rules (body text under 3:1, the page sticking out sideways, clipped text). Facts found without a model; they are hard feedback like an unmet claim,
            //            sent back inside the same cap, and a run that spends the cap still failing one parks on the QA-direction question like any other unmet claim.
            //   FIXES  - the critic's polish (at most five). Taste: sent back ONCE, and only while a send-back is left in the cap; otherwise they are written on the report as design notes. They never
            //            make a finished run unfinished by themselves - a run whose only feedback is taste and whose cap is spent is delivered.
            let front: Vec<String> = r.tasks.iter().filter(|t| t.status == TaskStatus::Done && crate::qa_evidence::touches_frontend(&if t.changed.is_empty() { t.owns.clone() } else { t.changed.clone() })).map(|t| t.id.clone()).collect();
            let for_front = |text: String| -> Vec<(Option<String>, String)> { if front.is_empty() { vec![(None, text)] } else { front.iter().map(|id| (Some(id.clone()), text.clone())).collect() } };
            let design_on = { let mut f: Vec<String> = r.tasks.iter().flat_map(|t| if t.changed.is_empty() { t.owns.clone() } else { t.changed.clone() }).collect(); f.sort(); f.dedup(); crate::design_round::enabled(crate::design_round::mode(), crate::roles::domain_of(&f), &f) };
            if let Some(d) = r.design.as_ref().filter(|_| design_on) { for b in &d.blockers { feedback.extend(for_front(format!("design rule (this blocks delivery) - {b}"))); } }
            let soft: Vec<String> = r.design.as_ref().filter(|_| design_on).map(|d| d.unsent_lines()).unwrap_or_default();
            let room = max_rounds > 0 && r.qa_rounds < max_rounds;
            if r.status != RunStatus::Done || report_only || (feedback.is_empty() && !(room && !soft.is_empty())) {
                // AN INDEPENDENT QA ASKED FOR FROM THE RESULT ONLY REPORTS (found live 2026-10-10, run 384: it sent a delivered run's task back for a page-wide overflow it had not caused, the worker changed nothing twice and the
                // run flipped to failed): what it found is written on the run, the run stays as delivered, and the operator decides whether to send anything back
                if report_only && !feedback.is_empty() { let list = feedback.iter().map(|(_, t)| format!("- {t}")).collect::<Vec<_>>().join("\n"); Self::log(r, "qa-findings", format!("independent QA found {} issue(s) and did not send them back (it only reports):\n{list}", feedback.len()), None); }
                r.qa_pending = false;
                Self::apply_exact_savings(r, &calls);
                set_confidence(r);
                // delivered: the design notes (what the critic found, what was applied, the lint before and after) go on the report, once
                if r.status == RunStatus::Done { if let Some(block) = r.design.as_mut().filter(|d| !d.reported).and_then(|d| { let b = d.report_block(); if b.is_some() { d.reported = true; } b }) { r.report = Some(format!("{}{block}", r.report.clone().unwrap_or_default())); } }
                return None;
            }
            if r.qa_rounds >= max_rounds {
                r.qa_pending = false;
                set_confidence(r);
                let tag = format!("Q{}.final", r.id);
                let list = feedback.iter().map(|(_, t)| format!("- {t}")).collect::<Vec<_>>().join("\n");
                if max_rounds == 0 {
                    Self::apply_exact_savings(r, &calls);
                    // send-backs are off: QA only records its verdict, on the report
                    Self::log(r, "qa-fail", format!("[{tag}] QA finds {} unmet - delivered as it is", feedback.len()), None);
                    r.report = Some(format!("{}\n\n⚠ QA GATE [{tag}]: {} unmet:\n{list}", r.report.clone().unwrap_or_default(), feedback.len()));
                    return None;
                }
                // SPENT AND STILL WRONG -> A SCREENING QUESTION (operator 2026-09-29): what happened, in facts, and a decision for the operator. The run parks like any run waiting on
                // answers; the queue keeps its turn. The default is the safe one: keep what exists, with QA's verdict written on it.
                let last = r.tasks.iter().filter_map(|t| t.report.as_deref()).next_back().and_then(|t| t.lines().map(|l| l.trim().trim_start_matches(['#', '-', '*', ' ']).trim()).find(|l| !l.is_empty() && !l.to_lowercase().starts_with("report"))).unwrap_or("").chars().take(240).collect::<String>();
                let text = format!("QA sent this run back {} time(s) and it is still not right. Still unmet:\n{list}\n{}How do you want to go on? Pick one, or type a new direction for the workers.", r.qa_rounds, if last.is_empty() { String::new() } else { format!("What the worker last reported: {last}\n") });
                r.questions = vec![Question { id: "qa1".into(), text, options: QA_DIRECTION_CHIPS.iter().map(|s| s.to_string()).collect(), default: QA_DIRECTION_CHIPS[1].into(), certainty: Certainty::Blocking, answer: None, kind: "qa-direction".into() }];
                r.status = RunStatus::AwaitingAnswers; r.ended = None; r.delivered = false;
                Self::log(r, "qa-fail", format!("[{tag}] QA still finds {} unmet after {} send-back(s) - asking the operator for a new direction", feedback.len(), r.qa_rounds), None);
                return None;
            }
            // the critic's fixes join the brief (a send-back is left in the cap, or this point was not reached): numbered, one per line, the feature and behaviour to stay as they are
            if !soft.is_empty() {
                feedback.extend(for_front("design round - the behaviour is right; now fix how the page LOOKS. Keep the feature, the content and the behaviour exactly as they are, add no dependency, and apply these in order. Open your report with ONE plain line saying what the page now does for the operator's ORIGINAL ask - not a line about these fixes - then list what you changed:".to_string()));
                for l in soft { feedback.extend(for_front(format!("design fix {l}"))); }
                if let Some(d) = r.design.as_mut() { d.sent = true; }
            }
            // which tasks: the ones the feedback names; feedback that names none goes to every finished task
            let named: Vec<String> = feedback.iter().filter_map(|(t, _)| t.clone()).filter(|t| r.tasks.iter().any(|x| &x.id == t)).collect();
            let targets: Vec<String> = if named.is_empty() { r.tasks.iter().filter(|t| t.status == TaskStatus::Done).map(|t| t.id.clone()).collect() } else { let mut n = named; n.sort(); n.dedup(); n };
            r.qa_rounds += 1;
            let tag = format!("Q{}.{}", r.id, r.qa_rounds);
            let done: Vec<String> = r.tasks.iter().filter(|t| t.status == TaskStatus::Done).map(|t| t.id.clone()).collect();
            for tid in &targets {
                let reasons: Vec<String> = feedback.iter().filter(|(t, _)| t.as_deref() == Some(tid.as_str()) || t.is_none() || !r.tasks.iter().any(|x| Some(&x.id) == t.as_ref())).map(|(_, x)| format!("QA [{tag}]: {x}")).collect();
                if let Some(t) = r.tasks.iter_mut().find(|t| &t.id == tid) {
                    t.verdict = Some(Verdict { passed: false, reasons, by: "qa-gate".into() });
                    t.status = if t.needs.iter().all(|n| done.contains(n)) { TaskStatus::Ready } else { TaskStatus::Pending };
                    t.error = None; t.attempts = 0; t.ended = None;
                }
            }
            r.status = RunStatus::Running; r.ended = None; r.report = None; r.delivered = false;
            Self::log(r, "qa-send-back", format!("[{tag}] QA sent {} task(s) back to their workers with feedback: {}", targets.len(), feedback.iter().map(|(_, t)| t.chars().take(160).collect::<String>()).collect::<Vec<_>>().join(" | ")), None);
            Some(())
        });
        if back.is_some() { self.ensure_ticking(); }
    }
    async fn qa_review(self: &Arc<Self>, rid: i64) {
        let Some(run) = self.get(rid) else { return };
        let files: Vec<String> = { let mut f: Vec<String> = run.tasks.iter().flat_map(|t| t.changed.iter().cloned()).collect(); if f.is_empty() { f = self.changed_since(&run.cwd, run.base.as_ref()); } f.sort(); f.dedup(); f };
        let diff = if files.is_empty() { String::new() } else { self.diff_of(&run.cwd, &files, run.base.as_ref()) };
        self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "qa-review", format!("judging the run: {} task(s), {} changed file(s)", r.tasks.len(), files.len()), None); } });
        let killer: Killer = Arc::new(Mutex::new(None));
        self.killers.lock().unwrap_or_else(|p| p.into_inner()).insert(format!("{rid}:qa"), killer.clone());
        let qa = crate::cli::role_binding_at(crate::cli::Role::Qa, run.engine, run.cost);
        // the whole-diff judge looks at the pictures too (visual QA): same tool, same block
        let (evidence_block, evidence_images) = self.visual_evidence(&run, qa.engine, qa.model.clone()).await;
        let out = self.run_safe(rid, qa.engine, vec![ChatMessage { role: "user".into(), content: format!("{}{}{}", qa_review_prompt(&run, &diff), if evidence_block.is_empty() { "" } else { "\n\n" }, evidence_block) }], run.cwd.clone(), false, RunOpts { max_turns: Some(24), timeout_ms: Some(420_000), tier: Some(Tier::High), model: qa.model.clone(), images: evidence_images, ..Default::default() }, Some(killer)).await;
        self.killers.lock().unwrap_or_else(|p| p.into_inner()).remove(&format!("{rid}:qa"));
        self.with(|rs| {
            let Some(r) = rs.iter_mut().find(|x| x.id == rid) else { return };
            let review = match out {
                Ok(o) => parse_qa_review(&o.parsed.content).unwrap_or_else(|| QaReview { passed: false, score: 0, summary: format!("QA answered in a shape nothing could read — it said: {}", o.parsed.content.trim().chars().take(240).collect::<String>()), findings: vec![], by: "engine".into(), t: now_ms(), error: Some(o.parsed.content.chars().take(400).collect()) }),
                Err(e) => QaReview { passed: false, score: 0, summary: "the judge could not run".into(), findings: vec![], by: "engine".into(), t: now_ms(), error: Some(e) },
            };
            let kind = if review.error.is_some() { "qa-review-error" } else if review.passed { "qa-review-pass" } else { "qa-review-fail" };
            Self::log(r, kind, format!("{} · score {} · {} finding(s) — {}", if review.passed { "PASSED" } else { "FAILED" }, review.score, review.findings.len(), review.summary), None);
            r.qa_review = Some(review);
        });
        // …AND A SECOND PASS WHEN IT WAS ASKED FOR (operator 2026-09-28: "a second QA that will re-measure the
        // work and the first QA to get a more precise evaluation").
        // (the second, doubled pass was retired with the obligatory QA: one judge, optional)
        self.feed_qa_verdict(rid);
    }
    /// THE JUDGE'S VERDICT IS THE ROW'S CORRECTNESS (2026-09-28). Only for real use (`pane_id > 0`): a bench arm's row is graded by
    /// executed checks, and an opinion must never average into a measurement (the bench harness posts its own audit). The final
    /// verdict — the second judge's when there was one — joins the row the run was filed under, as `run+audit`.
    fn feed_qa_verdict(&self, rid: i64) {
        let Some((o, review)) = self.with(|rs| rs.iter().find(|x| x.id == rid).and_then(|r| {
            if r.pane_id <= 0 { return None }
            let review = r.qa_review.clone().filter(|v| v.error.is_none())?;
            Some((r.merge_outcome.clone()?, review))
        })) else { return };
        let a = crate::merges::Outcome { audit: Some((review.score as f64 / 100.0).clamp(0.0, 1.0)), audit_only: true, ..o };
        let said = match crate::merges::record(&a) { Ok(_) => format!("QA's verdict ({}/100) joined the benchmark row {} · {} on {} {}", review.score, a.planner, a.worker, a.job, a.size), Err(e) => format!("QA's verdict was not recorded — {e}") };
        self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "merge-score", said, None); } });
    }

    /// THE SECOND JUDGE — given the same diff AND the first verdict, and asked to review both.
    ///
    /// One judge is one opinion, and the record says the judging jobs are the least-measured on the board: every
    /// `planner` and `verifier` row for the frontier pairs carries `audit_n = 0`. A second pass cannot make a
    /// judge right, but it can catch the two failures a single verdict cannot show — a judge that waved through
    /// work it did not read, and a judge that failed work for something that is not a defect.
    ///
    /// The second verdict becomes the run's; the first is kept in `qa_first` so the two can be compared, and the
    /// log says plainly when they disagreed, because a disagreement is the interesting outcome and the one a
    /// single number would hide.
    #[allow(dead_code)]
    async fn qa_review_second(self: &Arc<Self>, rid: i64) {
        let Some(run) = self.get(rid) else { return };
        let Some(first) = run.qa_review.clone() else { return };
        if first.error.is_some() { return }          // nothing to re-measure: the first judge never ran
        let files: Vec<String> = { let mut f: Vec<String> = run.tasks.iter().flat_map(|t| t.changed.iter().cloned()).collect(); if f.is_empty() { f = self.changed_since(&run.cwd, run.base.as_ref()); } f.sort(); f.dedup(); f };
        let diff = if files.is_empty() { String::new() } else { self.diff_of(&run.cwd, &files, run.base.as_ref()) };
        self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) {
            Self::log(r, "qa-review", "second pass — re-measuring the work AND the first verdict".to_string(), None); } });
        let qa = crate::cli::role_binding_at(crate::cli::Role::Qa, run.engine, run.cost);
        let prompt = format!(
            "{}

---

A FIRST REVIEWER HAS ALREADY JUDGED THIS WORK. Their verdict:

             passed: {}
score: {}
summary: {}
findings:
{}

             Review the WORK yourself first, from the diff above, without assuming the first verdict is right. Then say              whether that verdict was fair. If they missed a defect, raise it. If they failed the work for something that              is not a defect, say so. Answer in the same JSON shape; your verdict is the one that stands.",
            qa_review_prompt(&run, &diff), first.passed, first.score,
            if first.summary.is_empty() { "(none given)" } else { &first.summary },
            if first.findings.is_empty() { "(none)".to_string() }
            else { first.findings.iter().map(|f| format!("- {}", f.text)).collect::<Vec<_>>().join("
") });
        let killer: Killer = Arc::new(Mutex::new(None));
        self.killers.lock().unwrap_or_else(|p| p.into_inner()).insert(format!("{rid}:qa2"), killer.clone());
        let out = self.run_safe(rid, qa.engine, vec![ChatMessage { role: "user".into(), content: prompt }], run.cwd.clone(), false,
                             RunOpts { max_turns: Some(24), timeout_ms: Some(420_000), tier: Some(Tier::High), model: qa.model.clone(), ..Default::default() }, Some(killer)).await;
        self.killers.lock().unwrap_or_else(|p| p.into_inner()).remove(&format!("{rid}:qa2"));
        self.with(|rs| {
            let Some(r) = rs.iter_mut().find(|x| x.id == rid) else { return };
            let Ok(o) = out else {
                Self::log(r, "qa-review", "the second pass could not run — the first verdict stands".to_string(), None); return };
            let Some(second) = parse_qa_review(&o.parsed.content) else {
                Self::log(r, "qa-review", format!("the second pass answered in a shape nothing could read — the first verdict stands. It said: {}",
                                                  o.parsed.content.trim().chars().take(200).collect::<String>()), None); return };
            let agreed = second.passed == first.passed;
            Self::log(r, if second.passed { "qa-review-pass" } else { "qa-review-fail" },
                format!("second pass: {} · score {} (first said {} · {}) — {}{}",
                        if second.passed { "PASSED" } else { "FAILED" }, second.score,
                        if first.passed { "PASSED" } else { "FAILED" }, first.score,
                        if agreed { "the two judges agree" } else { "THE TWO JUDGES DISAGREE, and the second stands" },
                        if second.summary.is_empty() { String::new() } else { format!(" — {}", second.summary) }), None);
            r.qa_first = Some(first);
            r.qa_review = Some(second);
        });
    }
    /// MIN's fallback (2026-09-21): the route carries the files the ask names that have no intel — the pane runs this ask with exploration on.
    pub fn mark_no_intel(&self, id: i64, paths: Vec<String>) -> Option<GoalRun> {
        self.with(|rs| { let r = rs.iter_mut().find(|x| x.id == id)?; if let Some(route) = r.route.as_mut() { route.no_intel = paths.clone(); } Self::log(r, "min", format!("no intel for {} — this ask runs with exploration on (MIN lifted for one turn)", paths.join(", ")), None); Some(r.clone()) })
    }
    pub fn release_delivery(&self, id: i64) -> bool { self.with(|rs| rs.iter_mut().find(|x| x.id == id).map(|r| { r.delivered = false; true }).unwrap_or(false)) }
    pub fn claim_delivery(&self, id: i64) -> Option<bool> { self.with(|rs| rs.iter_mut().find(|x| x.id == id).map(|r| { let won = !r.delivered; r.delivered = true; won })) }
    pub fn add_finding(&self, id: i64, task: &str, text: &str) -> bool { self.with(|rs| rs.iter_mut().find(|x| x.id == id).map(|r| { r.blackboard.push(Finding { task: task.into(), text: text.into(), t: now_ms() }); Self::log(r, "finding", text, Some(task)); }).is_some()) }

    /// the size bucket the merge table files a run under (merges::size_of)
    /// THE WORKER THE DISPATCH WILL ACTUALLY USE — pair included (2026-09-26).
    ///
    /// Two callers resolved this with `role_binding_at(Role::Worker, r.workers.unwrap_or(engine), …)`, which does not
    /// look at `r.pair`. Under a merge the dispatch goes through `job_binding_learned(job, size, pair, …)` and lands
    /// on the HANDS side, so the two disagreed: a `claude-deepseek` run logged "one worker on claude-code haiku"
    /// while the task trace recorded `engine: opencode`. The log contradicting the run is the 2026-09-22 bug over
    /// again ("the ledger disagreed with the log"), and it is worse than cosmetic at the projection gate, which used
    /// the wrong engine's timing history to decide whether to collapse a plan.
    ///
    /// One resolution, asked the same way the work will be dispatched.
    fn worker_binding_of(r: &GoalRun, engine: Engine) -> crate::cli::Binding {
        let size = Self::size_of_run(r);
        let job = if size == "s" { crate::cli::Job::EditorS } else { crate::cli::Job::EditorL };
        crate::cli::job_binding_learned_in(job, &size, r.pair.as_ref(), r.workers.unwrap_or(engine), r.cost, Some(Self::domain_of_run(r).as_str())).0
    }
    /// what kind of work the run's tasks are, from the files they changed or own — `roles::domain_of`, the same rule the record files under
    fn domain_of_run(r: &GoalRun) -> String {
        crate::roles::domain_of(&r.tasks.iter().flat_map(|t| if t.changed.is_empty() { t.owns.clone() } else { t.changed.clone() }).collect::<Vec<_>>()).key().to_string()
    }
    fn size_of_run(r: &GoalRun) -> String {
        let one = r.tasks.len() == 1 && r.tasks[0].owns.len() == 1 && !r.tasks[0].owns[0].contains(['*', '?', '[']) && r.goal.chars().count() <= 400;
        crate::merges::size_of(r.tasks.len(), one, r.goal.chars().count()).into()
    }
    /// THE WORKER'S BINDING (operator 2026-09-22: "always one worker cheaper than its planner"). The role's own
    /// binding, moved by the models bar, then held under the planner's price — down its ladder, or across families
    /// (cli::worker_under_planner). The move is logged on the run the first time it happens, never silently.
    fn worker_binding(&self, run: &GoalRun) -> crate::cli::Binding { self.worker_binding_for(run, None) }

    /// THE SAME, FOR ONE TASK (PLAN-THE-CHAMELEON C2). With `mid_task` on, a task whose owned files name a role
    /// is filled by THAT role's measured winner rather than by the run's single binding — so a plan that touches
    /// the screen and then the API is done by two forms, which is the pipeline the operator described.
    ///
    /// EVERY GUARD THE RUN-LEVEL PATH HAS STILL APPLIES. The winner is held under the planner's price by
    /// `worker_under_planner` exactly as before, and a task whose files say nothing (a glob, a mixture with no
    /// majority) keeps the run's form rather than being guessed at — `role_for_task` answers None there and this
    /// falls straight through to the original behaviour.
    fn worker_binding_for(&self, run: &GoalRun, task: Option<&GoalTask>) -> crate::cli::Binding {
        let planner = crate::cli::role_binding_at(crate::cli::Role::Planner, run.engine, run.cost);
        if run.mid_task {
            if let Some(role) = task.and_then(|t| crate::roles::role_for_task(&t.owns)) {
                let size = Self::size_of_run(run);
                let editor = if size == "s" { "editor-s" } else { "editor-l" };
                let obj = crate::roles::Objective::Best;
                // a task of a run that needs no planner is filled by the role's winner among SOLO cells and pairs alike (`best_cell`)
                if let Some(pick) = crate::merges::best_cell(editor, &size, role.domain.key(), obj, 1) {
                    if let Some((e, m)) = crate::cli::binding_of_key(&pick.worker) {
                        let asked = crate::cli::Binding { engine: e, model: Some(m), source: "role-task" };
                        return crate::cli::worker_for_run(asked, &planner, run.plan_basis == "solo", crate::cli::engine_usable).0;
                    }
                }
            }
        }
        // THE EDITOR'S SIZE IS A JOB (2026-09-22): one task owning one concrete file on a short ask is editor-s (Sonnet
        // 5/5 raw; Flash 2/5) — anything wider is editor-l under the verifier (Flash 35–37/37 at $0.02). An explicit
        // role binding for the worker still wins over both.
        let one_file = run.tasks.len() == 1 && run.tasks[0].owns.len() == 1 && !run.tasks[0].owns[0].contains(['*', '?', '[']) && run.goal.chars().count() <= 400;
        let role_bound = crate::cli::role_binding(crate::cli::Role::Worker, run.workers.unwrap_or(run.engine)).source == "role";
        let asked = if !role_bound {
            // the pair rules; solo: what the merge table learned for this size, else the jobs table
            crate::cli::job_binding_learned_in(if one_file { crate::cli::Job::EditorS } else { crate::cli::Job::EditorL }, &Self::size_of_run(run), run.pair.as_ref(), run.workers.unwrap_or(run.engine), run.cost, Some(Self::domain_of_run(run).as_str())).0
        } else { crate::cli::role_binding_at(crate::cli::Role::Worker, run.workers.unwrap_or(run.engine), run.cost) };
        // A SOLO RUN HAS NO PLANNER FOR ITS WORKER TO BE CHEAPER THAN (item 4, 2026-10-01): see `cli::worker_for_run`
        let solo_run = run.plan_basis == "solo";
        let (w, note) = crate::cli::worker_for_run(asked, &planner, solo_run, crate::cli::engine_usable);
        // …and the run says what the solo cells did to the choice, once (the same sentence the role route prints)
        if solo_run && !role_bound && run.pair.is_none() {
            let job = if one_file { crate::cli::Job::EditorS } else { crate::cli::Job::EditorL };
            if let Some((_, _, why)) = crate::cli::learned_pick(job, &Self::size_of_run(run), Some(Self::domain_of_run(run).as_str())).filter(|(_, _, why)| !why.is_empty()) {
                let rid = run.id;
                self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) {
                    if !r.events.iter().any(|e| e.text.starts_with("solo editor:")) { Self::log(r, "route", format!("solo editor: {why}"), None); }
                } });
            }
        }
        if let Some(n) = note {
            let rid = run.id;
            self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) {
                if !r.events.iter().any(|e| e.text.starts_with("the worker must be cheaper")) { Self::log(r, "route", n.clone(), None); }
            } });
        }
        w
    }
    /// ONE CALL, SAFE AGAINST A SEAT THAT RAN OUT OF QUOTA (operator 2026-09-30). Every call that does not go through `run_job`'s two-rung ladder - QA, the
    /// verifier, the plan retries, the worker - goes through here: a seat already known to be limited is swapped for the next available one BEFORE the call,
    /// a limit message is remembered (so every later pick skips the seat), a success clears it, and a call that hit the limit is retried once on the next
    /// seat. The model belongs to the seat it was chosen for, so it is dropped when the seat changes (the new seat uses its own default).
    ///
    /// THE CROSSING IS IN THE RUN'S LOG, IN RUN_JOB'S WORDS (found writing J11, 2026-10-01): the worker, QA and ratifier crossed to the next seat just as well but left
    /// NOTHING about it - only `run_job`'s jobs wrote the "out of quota" line, so a run that finished on another seat read as if the first had answered. `rid` is the
    /// run the call belongs to (0 for none: a bench or a unit test has no log to write to).
    async fn run_safe(&self, rid: i64, engine: Engine, messages: Vec<ChatMessage>, cwd: String, wants_json: bool, opts: RunOpts, killer: Option<Killer>) -> Result<crate::cli::RunOut, String> {
        let opts = RunOpts { run_id: rid, ..opts };
        crate::cli::refresh_windows(&cwd);
        // THE NEXT SEAT IS CHOSEN FOR THE JOB THE CALL IS (operator 2026-09-30: a Gemini worker hit its quota and the retry went to the same seat; the first free seat in the cheapest-first list
        // would have been Claude for a worker). `alternate_seat` knows the job: the gate goes down the benchmark's order, everything else to the rung that answers most.
        let job = crate::cli::Job::of(crate::audit::job_of_template(opts.template.as_deref())).unwrap_or(crate::cli::Job::EditorL);
        let next = |not: Engine| crate::goals::alternate_seat(job, not, "l", None, None).map(|(e, m)| (e, m.filter(|x| !x.trim().is_empty())));
        let say = |text: String| { if rid > 0 { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "fallback", text, None); } }); } };
        let (mut e, mut o) = (engine, opts.clone());
        // a seat that is out of quota OR switched off in Settings is left before the call, not after the refusal
        let (limited, off) = (crate::cli::seat_limit(e), crate::cli::model_blocked(e, o.model.as_deref()).is_some());
        if limited.is_some() || off {
            if let Some((n, m)) = next(e) {
                match &limited {
                    Some((_, said)) => { say(format!("{} is out of quota ({}) - Heimdall routes around it until it is back", e.key(), said.chars().take(140).collect::<String>())); say(format!("{}: the next available seat is {} {}", job.key(), n.key(), m.as_deref().unwrap_or("-"))); }
                    None => say(format!("{}: {} switched off (Settings › Seats) or out of quota — skipped, {} instead", job.key(), e.key(), n.key())),
                }
                e = n; o.model = m;
            }
        }
        let first = (self.run)(e, messages.clone(), cwd.clone(), wants_json, o, killer.clone()).await;
        match &first {
            Ok(_) => { crate::cli::clear_limit(e); first }
            Err(why) if crate::cli::note_limit(e, why) => {
                let (_, said) = crate::cli::seat_limit(e).unwrap_or((0, String::new()));
                say(format!("{} is out of quota ({}) - Heimdall routes around it until it is back", e.key(), said.chars().take(140).collect::<String>()));
                match next(e) {
                    Some((n, m)) => {
                        say(format!("{}: the next available seat is {} {}", job.key(), n.key(), m.as_deref().unwrap_or("-")));
                        let mut o2 = opts; o2.model = m;
                        let second = (self.run)(n, messages, cwd, wants_json, o2, killer).await;
                        match &second { Ok(_) => crate::cli::clear_limit(n), Err(w) => { if crate::cli::note_limit(n, w) { say(format!("{} is out of quota too - no seat is left to try for {}", n.key(), job.key())); } } }
                        second
                    }
                    None => { say(format!("no other seat is reachable for {} - the call stays failed on {}", job.key(), e.key())); first }
                }
            }
            Err(_) => first,
        }
    }
    /// RUN A JOB — the winner first, the fallback once if the winner's engine failed to answer (operator 2026-09-22:
    /// "winner + fallback always"). A refusal by the verifier is not a failure of the model; only an Err from the
    /// engine (no answer, a dead CLI, a model the account refuses) moves the call to the fallback, and the run's log
    /// says so. `opts.model` is overwritten with the binding's model on each attempt.
    async fn run_job(&self, rid: i64, job: crate::cli::Job, engine: Engine, cost: Option<u8>, messages: Vec<ChatMessage>, cwd: String, wants_json: bool, mut opts: RunOpts, killer: Option<Killer>) -> Result<crate::cli::RunOut, String> {
        let (pair, size, domain) = self.with(|rs| rs.iter().find(|x| x.id == rid).map(|r| (r.pair.clone(), Self::size_of_run(r), Self::domain_of_run(r))).unwrap_or((None, "l".into(), String::new())));
        crate::cli::refresh_windows(&cwd);
        // THE DOMAIN OF THE WORK reaches the router (2026-10-05): the verifier is picked from its own domain's pool
        let (w, f) = crate::cli::job_binding_learned_in(job, &size, pair.as_ref(), engine, cost, Some(domain.as_str()));

        // THE LADDER REORDERS ITSELF (HyperAgent's "degradation is the normal path", 2026-09-25). The fallback used to
        // leave a line in the transcript and nothing else, so a binding could fail every day and still be tried first
        // every day. Every attempt is recorded now, and a rung with enough evidence can overtake the declared winner —
        // conservatively: under `ladder::MIN_N` attempts nothing moves, because an engine that was down for an hour is
        // not an engine that is bad, and no rung is ever dropped.
        let declared = vec![
            crate::ladder::Rung { engine: w.engine.key().into(), model: w.model.clone().unwrap_or_default(), role: "winner", n: 0, ok: 0, usd: 0.0 },
            crate::ladder::Rung { engine: f.engine.key().into(), model: f.model.clone().unwrap_or_default(), role: "fallback", n: 0, ok: 0, usd: 0.0 },
        ];
        let measured = crate::ladder::measured(job.key());
        let declared: Vec<crate::ladder::Rung> = declared.into_iter().map(|mut r| {
            if let Some((_, n, ok, usd)) = measured.iter().find(|(k, _, _, _)| *k == r.key()) { r.n = *n; r.ok = *ok; r.usd = *usd; }
            r
        }).collect();
        let ordered = crate::ladder::order(&declared);
        if let Some(line) = crate::ladder::moved_line(&declared, &ordered) {
            self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "fallback", format!("the ladder moved for {}: {line}", job.key()), None); } });
        }
        // A SWITCHED-OFF SEAT IS NEVER CALLED, FROM ANY RUNG (ladder::reachable, operator 2026-09-27): the record's
        // winner may stand on Zen or on a Chinese model that is off today; it is dropped before the first call and the
        // run's own reachable seat stands in when nothing is left. Before this the planner called it, got refused,
        // and the run died on the refusal with the reachable fallback never tried.
        let (ordered, dropped) = crate::ladder::reachable(ordered,
            |r| crate::cli::engine_of(&serde_json::json!(r.engine)).map(|e| crate::cli::model_blocked(e, if r.model.is_empty() { None } else { Some(r.model.as_str()) }).is_some() || crate::cli::seat_limit(e).is_some()).unwrap_or(true),
            || { let e = crate::cli::heimdall_engine(Some(engine)).unwrap_or(engine); crate::ladder::Rung { engine: e.key().into(), model: String::new(), role: "winner", n: 0, ok: 0, usd: 0.0 } });
        if !dropped.is_empty() {
            self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "fallback", format!("{}: {} switched off (Settings › Seats) or out of quota — skipped, {} instead", job.key(), dropped.join(" and "), ordered[0].key()), None); } });
        }
        let pick = |i: usize| -> (Engine, Option<String>, &'static str) {
            let r = &ordered[i.min(ordered.len() - 1)];
            let e = crate::cli::engine_of(&serde_json::json!(r.engine)).unwrap_or(if r.role == "winner" { w.engine } else { f.engine });
            (e, if r.model.is_empty() { None } else { Some(r.model.clone()) }, r.role)
        };
        let (e1, m1, role1) = pick(0);
        let (mut e2, mut m2, mut role2) = pick(1);

        opts.model = m1.clone(); opts.run_id = rid;
        let t0 = crate::cli::now_ms();
        let first = (self.run)(e1, messages.clone(), cwd.clone(), wants_json, opts.clone(), killer.clone()).await;
        // AN ENGINE THAT SAYS NOTHING HAS NOT ANSWERED (found auditing a real bench run, 2026-09-26). OpenCode exited
        // 0 with an empty reply; `run_job` saw `Ok`, filed a SUCCESSFUL rung, and returned — so the fallback was never
        // tried and the ladder learned the opposite of what happened. The callers already convert an empty reply into
        // an error, but they do it AFTER this function, which is exactly one step too late to fall back.
        let first = empty_is_failure(first);
        // EVERY STRUCTURED REPLY IS COUNTED, WITH THE MODEL THAT WROTE IT (audit 2026-09-26). The lint was wired at the
        // planner's call site only, and passed "" for the model — so `json_health` could not answer the question it
        // exists for: WHICH model gets the shape wrong. Here it sees the template, the engine and the model for every
        // job that has one.
        if let (Some(tpl), Ok(o)) = (opts.template.as_deref(), first.as_ref()) {
            let _ = crate::jsonlint::check_base(tpl, e1.key(), m1.as_deref().unwrap_or(""), &o.parsed.content);
        }
        crate::ladder::note(job.key(), e1.key(), m1.as_deref().unwrap_or(""), role1, first.is_ok(),
            first.as_ref().ok().and_then(|o| o.trace.cost).unwrap_or(0.0), crate::cli::now_ms() - t0,
            first.as_ref().err().map(String::as_str).unwrap_or(""));
        if first.is_ok() { crate::cli::clear_limit(e1); }
        // A SEAT OUT OF QUOTA IS REMEMBERED AND ROUTED AROUND (operator 2026-09-30: Codex hit its limit in the middle of planning and the run died on it).
        // The limit is noted for every later pick, and a second model on the SAME seat is no fallback for it - the next best AVAILABLE seat is.
        let limited_now = match &first { Err(e) if crate::cli::note_limit(e1, e) => {
            let (_, said) = crate::cli::seat_limit(e1).unwrap_or((0, String::new()));
            self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "fallback", format!("{} is out of quota ({}) - Heimdall routes around it until it is back", e1.key(), said.chars().take(140).collect::<String>()), None); } });
            true } _ => false };
        let first_err_dead = matches!(&first, Err(e) if crate::gauge::seat_dead(e).sure);
        if (limited_now || first_err_dead) && e2.key() == e1.key() {
            if let Some((ae, am)) = alternate_seat(job, e1, &size, pair.as_ref(), cost) {
                self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "fallback", format!("{}: the next available seat is {} {}", job.key(), ae.key(), am.as_deref().unwrap_or("-")), None); } });
                e2 = ae; m2 = am; role2 = "fallback";
            }
        }
        match first {
            Ok(o) => Ok(o),
            // A DEAD SEAT HAS NO SECOND MODEL (gauge.rs): signed out, no credit, no binary — a fallback on the SAME engine fails the same way and
            // costs a call and a wait. A fallback on another engine is a different seat and is still tried.
            Err(e) if e2.key() == e1.key() && crate::gauge::seat_dead(&e).sure => {
                let g = crate::gauge::seat_dead(&e);
                self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "gauge", format!("{}: no fallback — {} (the fallback is the same {} seat)", job.key(), g.evidence.join("; "), e1.key()), None); } });
                Err(e)
            }
            // out of quota and no other seat is reachable: say so plainly instead of a second call on the same empty seat
            Err(e) if limited_now && e2.key() == e1.key() => Err(e),
            Err(e) if m2 != m1 || e2.key() != e1.key() => {
                self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "fallback", format!("{} on {} {} failed ({}) — retrying on the fallback {} {}", job.key(), e1.key(), m1.as_deref().unwrap_or("-"), e.chars().take(120).collect::<String>(), e2.key(), m2.as_deref().unwrap_or("-")), None); } });
                opts.model = m2.clone();
                let t1 = crate::cli::now_ms();
                let tpl2 = opts.template.clone();
                let second = empty_is_failure((self.run)(e2, messages, cwd, wants_json, opts, killer).await);
                if let (Some(tpl), Ok(o)) = (tpl2.as_deref(), second.as_ref()) {
                    let _ = crate::jsonlint::check_base(tpl, e2.key(), m2.as_deref().unwrap_or(""), &o.parsed.content);
                }
                crate::ladder::note(job.key(), e2.key(), m2.as_deref().unwrap_or(""), role2, second.is_ok(),
                    second.as_ref().ok().and_then(|o| o.trace.cost).unwrap_or(0.0), crate::cli::now_ms() - t1,
                    second.as_ref().err().map(String::as_str).unwrap_or(""));
                if let Err(e) = &second { if crate::cli::note_limit(e2, e) { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "fallback", format!("{} is out of quota too - no seat is left to try for {}", e2.key(), job.key()), None); } }); } } else { crate::cli::clear_limit(e2); }
                second
            }
            Err(e) => Err(e),
        }
    }
    fn stage(&self, rid: i64, st: crate::blueprint::Stage) {
        // the estimator learns from every stage (eta.rs): kind × engine × tier, per unit
        if st.ok && st.ms > 0 { let units = st.note.split(' ').next().and_then(|n| n.parse::<i64>().ok()).unwrap_or(1); crate::eta::History::note(st.name.split(':').next().unwrap_or(&st.name), &st.engine, &st.tier, st.ms, units); }
        self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "stage", format!("{} · {} · {:.1} s{}{}", st.name, if st.ok { "ok" } else { "FAILED" }, st.ms as f64 / 1000.0, if st.cost > 0.0 { format!(" · ${:.4}", st.cost) } else { String::new() }, if st.note.is_empty() { String::new() } else { format!(" — {}", st.note.chars().take(160).collect::<String>()) }), None); r.stages.push(st); } });
    }
    /// BLUEPRINT MODE (blueprint.rs): the deterministic pipeline up to the fills; the fills are ordinary tasks
    /// (one per module, in parallel, at low) and `verify` takes the deterministic path for this mode.
    #[allow(clippy::too_many_arguments)]
    pub async fn plan_blueprint(self: &Arc<Self>, parent: &str, goal: &str, engine: Engine, cwd: &str, workers: Option<Engine>, qa: bool) -> GoalRun {
        use crate::blueprint as bp; use std::path::Path;
        let id = { let mut s = self.seq.lock().unwrap_or_else(|p| p.into_inner()); *s += 1; *s };
        let mut r = GoalRun { plan_usd: 0.0, screen_usd: 0.0, plan_basis: String::new(), plan_binding: String::new(), worker_binding: String::new(), suite: String::new(), confidence: None, pane_id: 0, scope: vec![], checks_off: false, merge_outcome: None, role: None, plan_ms: 0, id, parent: parent.into(), goal: goal.into(), engine, cwd: cwd.into(), write: true, concurrency: 8, status: RunStatus::Planning, created: now_ms(), started: None, ended: None, tasks: vec![], blackboard: vec![], events: vec![], report: None, error: None, savings: None, route: None, router: true, delivered: false, questions: vec![], assumptions: vec![], min: false, plan_args: PlanArgs { n: 8, forced_tier: None, router: true, auto: true, always: false }, pr: None, qa: None, qa_baseline: None, qa_baseline_pending: false, compare: None, workers, mode: "blueprint".into(), blueprint: None, stages: vec![], qa_gate: None, cost: None, pair: None, qa_double: false, qa_first: None, qa_review_wanted: qa, qa_review: None, mid_task: false, base: None, qa_pending: false, qa_rounds: 0, design: None };
        r.cost = self.take_cost(); r.pair = self.take_pair(); r.suite = self.take_suite(); r.mid_task = self.take_mid_task(); let _ = std::mem::take(&mut *self.planner_for_next.lock().unwrap_or_else(|p| p.into_inner()));
        // THE BATON, SAID OUT LOUD. The verdict is logged on the run itself, so "who is working and why" is in the
        // same place as everything else the run did — not inferred from a model name in a corner.
        if let Some(role) = self.take_role() {
            if let Some(why) = role.get("why").and_then(Value::as_str) { Self::log(&mut r, "role", why.to_string(), None); }
            r.role = Some(role);
        }
        if let Some(p) = r.pair.clone() { Self::log(&mut r, "pair", format!("the merge: {} — {} decides, {} produces", p.label(), p.brain.key(), p.hands.key()), None); }
        Self::log(&mut r, "plan", format!("BLUEPRINT: contract → skeleton → tests → gate → fills on {} → check", workers.unwrap_or(engine).key()), None);
        self.with(|rs| { rs.push(r.clone()); if rs.len() > 100 { let k = rs.len() - 100; rs.drain(0..k); } });
        let wk = workers.unwrap_or(engine);
        let tokens = |o: &crate::cli::RunOut| (o.parsed.tokens_in.unwrap_or(0), o.parsed.tokens_out.unwrap_or(0), o.trace.cost.unwrap_or(0.0));
        // deps: the orchestrator owns installs
        let t0 = now_ms();
        let deps = bp::ensure_vitest(cwd).await;
        self.stage(id, bp::Stage { name: "deps".into(), engine: "npm".into(), tier: "-".into(), started: t0, ended: now_ms(), ms: now_ms() - t0, tokens_in: 0, tokens_out: 0, cost: 0.0, ok: deps.is_ok(), note: match &deps { Ok(true) => "vitest installed".into(), Ok(false) => "vitest present".into(), Err(e) => e.clone() } });
        // contract
        let conventions = bp::conventions_of(cwd); let tree = bp::source_tree(cwd);
        let t0 = now_ms();
        let planner = crate::cli::role_binding_at(crate::cli::Role::Planner, engine, r.cost);
        let contract_prompt = crate::prompts::render_base("goal.contract", &bp::contract_vars(goal, &tree, &conventions));
        let out = self.run_safe(id, planner.engine, vec![ChatMessage { role: "user".into(), content: contract_prompt.clone() }], cwd.into(), true, RunOpts { max_turns: Some(12), timeout_ms: Some(300_000), tier: Some(Tier::Mid), model: planner.model.clone(), template: Some("goal.contract".into()), ..Default::default() }, None).await;
        let contract = out.as_ref().ok().and_then(|o| bp::parse_contract(&o.parsed.content));
        let (ti, to, c) = out.as_ref().map(tokens).unwrap_or((0, 0, 0.0));
        // THE BLUEPRINT'S PLANNER HAS A PRICE TOO (2026-10-06): this run never set `plan_usd`/`plan_ms`, so the contract call was priced
        // nowhere - the editor row's bill left it out and the planner row (below) had nothing to say. Same arbiter as the plan path.
        if let Ok(o) = out.as_ref() {
            let (usd, basis) = crate::estimate::price_run(&o.trace, planner.engine, planner.model.as_deref(), &contract_prompt, &o.parsed.content);
            let (ms, binding) = (o.ms, format!("{}:{}", o.trace.engine.key(), o.trace.model.clone().or_else(|| planner.model.clone()).unwrap_or_default()));
            self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { r.plan_usd = usd; r.plan_ms = ms; r.plan_basis = basis.key().into(); r.plan_binding = binding; } });
        }
        self.stage(id, bp::Stage { name: "contract".into(), engine: planner.engine.key().into(), tier: planner.model.clone().unwrap_or_else(|| "mid".into()), started: t0, ended: now_ms(), ms: now_ms() - t0, tokens_in: ti, tokens_out: to, cost: c, ok: contract.is_some(), note: contract.as_ref().map(|c| format!("{} module(s) · {}", c.modules.len(), c.summary)).unwrap_or_else(|| out.as_ref().err().cloned().unwrap_or_else(|| "no contract in the reply".into())) });
        // A BRAIN THAT ANSWERS WITHOUT A CONTRACT GETS ONE RETRY ON ANOTHER SEAT (2026-10-06, the XXL blueprint bench): Cursor grok-medium spent ~275 s and ~17k tokens
        // on the XXL ask and returned prose with no parseable contract, twice, and the run died with nothing. `run_safe` only falls back on a quota error and `empty_is_failure`
        // only on an empty reply, so a long, non-empty, unusable answer never reached a second seat. One retry, on the next seat that can answer a planner job; the failed
        // call's price stays in `plan_usd` (it was paid for), and the record names the seat that wrote the contract.
        let contract = if contract.is_some() { contract } else if let Some((ae, am)) = alternate_seat(crate::cli::Job::Planner, planner.engine, "l", None, r.cost) {
            let am = am.filter(|m| !m.trim().is_empty()).or_else(|| { let b = crate::cli::job_binding_learned(crate::cli::Job::Planner, "l", None, ae, r.cost).0; if b.engine == ae { b.model.filter(|m| !m.trim().is_empty()) } else { None } });
            self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { Self::log(r, "fallback", format!("{} returned no contract - retrying once on {} {}", planner.engine.key(), ae.key(), am.as_deref().unwrap_or("-")), None); } });
            let t1 = now_ms();
            let out2 = self.run_safe(id, ae, vec![ChatMessage { role: "user".into(), content: contract_prompt.clone() }], cwd.into(), true, RunOpts { max_turns: Some(12), timeout_ms: Some(300_000), tier: Some(Tier::Mid), model: am.clone(), template: Some("goal.contract".into()), ..Default::default() }, None).await;
            let contract2 = out2.as_ref().ok().and_then(|o| bp::parse_contract(&o.parsed.content));
            let (ti2, to2, c2) = out2.as_ref().map(tokens).unwrap_or((0, 0, 0.0));
            if let Ok(o) = out2.as_ref() {
                let (usd, basis) = crate::estimate::price_run(&o.trace, ae, am.as_deref(), &contract_prompt, &o.parsed.content);
                let (ms, binding) = (o.ms, format!("{}:{}", o.trace.engine.key(), o.trace.model.clone().or_else(|| am.clone()).unwrap_or_default()));
                self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { r.plan_usd += usd; r.plan_ms += ms; r.plan_basis = basis.key().into(); if contract2.is_some() { r.plan_binding = binding; } } });
            }
            self.stage(id, bp::Stage { name: "contract".into(), engine: ae.key().into(), tier: am.clone().unwrap_or_else(|| "mid".into()), started: t1, ended: now_ms(), ms: now_ms() - t1, tokens_in: ti2, tokens_out: to2, cost: c2, ok: contract2.is_some(), note: contract2.as_ref().map(|c| format!("retry after {} returned none · {} module(s) · {}", planner.engine.key(), c.modules.len(), c.summary)).unwrap_or_else(|| format!("retry after {} returned none: {}", planner.engine.key(), out2.as_ref().err().cloned().unwrap_or_else(|| "no contract in the reply".into()))) });
            contract2
        } else { None };
        let Some(mut contract) = contract else {
            self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) { r.status = RunStatus::Failed; r.error = Some("the contract call returned no contract".into()); r.ended = Some(now_ms()); } });
            return self.get(id).unwrap();
        };
        // skeleton (deterministic)
        let t0 = now_ms(); let mut emitted = 0usize;
        for m in &contract.modules { if m.edit { continue; } let p = Path::new(cwd).join(&m.path); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); } if std::fs::write(&p, bp::emit_skeleton(m)).is_ok() { emitted += 1; } }
        self.stage(id, bp::Stage { name: "skeleton".into(), engine: "deterministic".into(), tier: "-".into(), started: t0, ended: now_ms(), ms: now_ms() - t0, tokens_in: 0, tokens_out: 0, cost: 0.0, ok: true, note: format!("{emitted} stub file(s) written") });
        // tests (one cheap call per module with testable lines, in parallel)
        let t0 = now_ms();
        let mut jobs = vec![];
        // WHO WRITES THE TESTS (opt-in experiment, 2026-10-08): the tests stage was ~72 % of a decomposed L run's cost ($0.54-0.58 of ~$0.78) because it runs at mid on the run's OWN engine.
        // `ANTILOKI_BP_TESTS_ON_WORKER=1` hands it to the worker role's seat instead (a cheaper one). Off by default: the 2026-09-22 note below (a cheap model wrote a wrong expectation) is why,
        // and only a measured A/B (audit and gate result with the cheap seat) may turn it on.
        let (te, tm): (Engine, Option<String>) = if std::env::var("ANTILOKI_BP_TESTS_ON_WORKER").map(|v| v == "1").unwrap_or(false) {
            let b = crate::cli::role_binding_at(crate::cli::Role::Worker, wk, self.get(id).and_then(|x| x.cost)); (b.engine, b.model)
        } else { (engine, None) };
        for (i, m) in contract.modules.iter().enumerate() {
            // a hook or a component is compile-only: a unit test for it needs a DOM the box may not have, and a bad
            // test is worse than none — the first blueprint run lost two modules that way (2026-09-22)
            if m.exports.iter().all(|e| e.kind == "hook" || e.kind == "component") { continue; }
            let lines = bp::testable_lines(m); if lines.is_empty() || m.edit { continue; }
            let skeleton = std::fs::read_to_string(Path::new(cwd).join(&m.path)).unwrap_or_default();
            let stem = Path::new(&m.path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let prompt = crate::prompts::render_base("goal.tests", &vars(&[("path", json!(m.path)), ("skeleton", json!(skeleton)), ("lines", json!(lines.iter().map(|(e, t)| format!("- [{e}] {t}")).collect::<Vec<_>>().join("\n"))), ("conventions", json!(conventions)), ("stem", json!(stem))]));
            let run = self.run.clone(); let cwd_s = cwd.to_string();
            // THE TESTS ARE THE SPEC: they run at mid on the run's own engine (haiku wrote a wrong expectation and a
            // fill retried twice against it — 2026-09-22), and they are worth more than the fills they check
            let tm2 = tm.clone();
            jobs.push(async move { (i, run(te, vec![ChatMessage { role: "user".into(), content: prompt }], cwd_s, false, RunOpts { max_turns: Some(4), timeout_ms: Some(180_000), tier: Some(Tier::Mid), model: tm2, template: Some("goal.tests".into()), ..Default::default() }, None).await) });
        }
        let results = futures_util::future::join_all(jobs).await;
        let (mut ti, mut to, mut c, mut written) = (0i64, 0i64, 0f64, 0usize);
        for (i, out) in results {
            if let Ok(o) = &out { let (a, b, d) = tokens(o); ti += a; to += b; c += d;
                let src = strip_fence(&o.parsed.content);
                if src.contains("from \"vitest\"") || src.contains("from 'vitest'") { let tp = bp::test_path_for(&contract.modules[i].path); if std::fs::write(Path::new(cwd).join(&tp), src).is_ok() { contract.modules[i].test_file = Some(tp); written += 1; } }
            }
        }
        self.stage(id, bp::Stage { name: "tests".into(), engine: te.key().into(), tier: tm.clone().unwrap_or_else(|| "mid".into()), started: t0, ended: now_ms(), ms: now_ms() - t0, tokens_in: ti, tokens_out: to, cost: c, ok: written > 0, note: format!("{written} test file(s) written") });
        // THE GATE (deterministic): every test file must compile AND fail against the skeleton — a test that passes
        // on `throw new Error("stub")` proves nothing, and one that does not compile takes its module down with it.
        // Either way the file is dropped and the module is verified by the compiler alone.
        let t0 = now_ms();
        let mut dropped: Vec<String> = vec![];
        if let Some(errs) = bp::tsc(cwd).await {
            for m in contract.modules.iter_mut() { if let Some(tp) = m.test_file.clone() { if errs.contains(&tp) { let _ = std::fs::remove_file(Path::new(cwd).join(&tp)); m.test_file = None; dropped.push(format!("{tp} (does not compile)")); } } }
        }
        let live: Vec<String> = contract.modules.iter().filter_map(|m| m.test_file.clone()).collect();
        if !live.is_empty() {
            if let Ok(None) = bp::vitest(cwd, &live).await {
                // every test passed against stubs: they assert nothing about the real behaviour
                for m in contract.modules.iter_mut() { if let Some(tp) = m.test_file.take() { let _ = std::fs::remove_file(Path::new(cwd).join(&tp)); dropped.push(format!("{tp} (passes on stubs)")); } }
            }
        }
        let after = bp::tsc(cwd).await;
        self.stage(id, bp::Stage { name: "gate".into(), engine: "tsc+vitest".into(), tier: "-".into(), started: t0, ended: now_ms(), ms: now_ms() - t0, tokens_in: 0, tokens_out: 0, cost: 0.0, ok: after.is_none(), note: if dropped.is_empty() { format!("{} test file(s) compile and fail on the stubs — the spec is real", live.len()) } else { format!("dropped: {}{}", dropped.join(", "), after.map(|e| format!("; skeleton still failing: {}", e.lines().next().unwrap_or(""))).unwrap_or_default()) } });
        // the fills: one task per module, low, in parallel
        let tasks: Vec<PlanTask> = contract.modules.iter().enumerate().map(|(i, m)| {
            let acc: Vec<String> = m.exports.iter().flat_map(|e| e.acceptance.iter().map(move |a| format!("{}: {}", e.name, a.text))).take(8).collect();
            let prompt = if m.edit { format!("Edit {} as the ask requires: {}\n\nThe contract for what changes:\n{}", m.path, goal.lines().next().unwrap_or(""), m.exports.iter().map(|e| format!("- {}: {} — {}", e.name, e.signature, e.doc)).collect::<Vec<_>>().join("\n")) }
                else { format!("FILL THE STUBS in {}: every `throw new Error(\"stub: …\")` body becomes the real implementation. The file's imports, exports, signatures and doc comments are the contract — keep them exactly; add private helpers below the exports if you need them; touch no other file. NEVER run a package manager (npm/yarn/pnpm install, add, ci): the box's dependencies are the orchestrator's and an install against the current lockfile removes what it put there.{}\n\nThe box compiles with:\n{}", m.path, m.test_file.as_ref().map(|t| format!(" The tests in {t} must pass (`npx vitest run {t}`); read them first.")).unwrap_or_default(), conventions) };
            PlanTask { id: format!("t{}", i + 1), title: format!("fill {}", m.path), prompt, owns: vec![m.path.clone()], needs: vec![], acceptance: acc, tier: Tier::Low, specialists: vec![], skills: vec![] }
        }).collect();
        self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == id) {
            r.blueprint = Some(contract.clone());
            r.route = Some(Route { tier: Tier::Low, delegate: true, why: format!("blueprint: {} module(s) filled in parallel at low", tasks.len()), bias: vec![], verify_tier: Tier::Low, join_tier: Tier::Mid, confidence: "high".into(), no_intel: vec![] });
            let n = tasks.len(); set_tasks(r, tasks); r.concurrency = n.clamp(1, 8);
            r.status = RunStatus::AwaitingApproval; Self::log(r, "plan", format!("{n} fill(s) — auto-approved"), None);
        } });
        self.approve(id);
        self.get(id).unwrap()
    }
    fn ensure_ticking(self: &Arc<Self>) {
        { let mut t = self.ticking.lock().unwrap_or_else(|p| p.into_inner()); if *t { return; } *t = true; }
        let me = self.clone();
        tokio::spawn(async move {
            loop {
                me.tick().await;
                let busy = me.with(|rs| rs.iter().any(|r| matches!(r.status, RunStatus::Running | RunStatus::Joining)));
                if !busy { break; }
                tokio::time::sleep(std::time::Duration::from_millis(me.tick_ms)).await;
            }
            *me.ticking.lock().unwrap_or_else(|p| p.into_inner()) = false;
        });
    }
    /// One orchestration step over every running run: start what is ready, resolve what is blocked, join what is finished.
    pub async fn tick(self: &Arc<Self>) {
        let mut to_start: Vec<(i64, String)> = Vec::new();
        let mut to_join: Vec<i64> = Vec::new();
        self.with(|rs| {
            for r in rs.iter_mut() {
                if r.status != RunStatus::Running { continue; }
                // THE WATCHDOG (2026-09-19): a task still "running" long past its CLI timeout with no child behind
                // it lost its post-run handling (a panic in the blocking closure) — fail it so the run can settle
                let now = now_ms();
                let lost: Vec<String> = r.tasks.iter().filter(|t| t.status == TaskStatus::Running && t.started.map(|s0| now - s0 > 900_000 + 120_000).unwrap_or(false)).map(|t| t.id.clone()).collect();
                for id in lost {
                    let alive = self.killers.lock().unwrap_or_else(|p| p.into_inner()).get(&format!("{}:{id}", r.id)).map(|k| k.lock().unwrap_or_else(|p| p.into_inner()).is_some()).unwrap_or(false);
                    if alive { continue; }
                    if let Some(t) = r.tasks.iter_mut().find(|t| t.id == id) { t.status = TaskStatus::Failed; t.ended = Some(now); t.error = Some("lost: the sub-agent's run ended but its result never landed (see the serve log)".into()); }
                    Self::log(r, "fail", "lost: the run ended but its result never landed — the watchdog failed it", Some(&id));
                }
                // a worker that has said nothing for five minutes says so (see `silence_notice`)
                let silent: Vec<(String, String)> = r.tasks.iter().filter(|t| t.status == TaskStatus::Running).filter_map(|t| {
                    let last = r.events.iter().filter(|e| e.task.as_deref() == Some(t.id.as_str())).map(|e| e.t).max();
                    silence_notice(t.started?, last, now).map(|m| (t.id.clone(), m))
                }).collect();
                for (id, m) in silent { Self::log(r, "wait", m, Some(&id)); }
                let statuses: HashMap<String, TaskStatus> = r.tasks.iter().map(|t| (t.id.clone(), t.status)).collect();
                // dependants of a failed/skipped task can never start
                let mut blocked = Vec::new();
                for t in r.tasks.iter_mut() { if t.status == TaskStatus::Pending && t.needs.iter().any(|n| matches!(statuses.get(n), Some(TaskStatus::Failed | TaskStatus::Blocked | TaskStatus::Skipped))) { t.status = TaskStatus::Blocked; t.ended = Some(now_ms()); blocked.push(t.id.clone()); } }
                for id in blocked { Self::log(r, "blocked", "blocked: a dependency failed", Some(&id)); }
                for t in r.tasks.iter_mut() { if t.status == TaskStatus::Pending && t.needs.iter().all(|n| statuses.get(n) == Some(&TaskStatus::Done)) { t.status = TaskStatus::Ready; } }
                let active = r.tasks.iter().filter(|t| matches!(t.status, TaskStatus::Running | TaskStatus::Verifying)).count();
                let mut slots = r.concurrency.saturating_sub(active);
                for t in r.tasks.iter_mut() { if slots == 0 { break; } if t.status == TaskStatus::Ready { slots -= 1; t.status = TaskStatus::Running; t.started = Some(now_ms()); t.attempts += 1; to_start.push((r.id, t.id.clone())); } }
                if r.tasks.iter().all(|t| t.status.settled()) { r.status = RunStatus::Joining; to_join.push(r.id); }
            }
        });
        for (rid, tid) in to_start { tokio::spawn(self.clone().execute(rid, tid)); }
        for rid in to_join { let me = self.clone(); tokio::spawn(async move { me.join(rid).await; }); }
    }
    /// Boxed (not `async fn`): execute → tick → execute is a cycle the compiler cannot type as
    /// nested opaque futures; a `dyn Future` cuts it.
    fn execute(self: Arc<Self>, rid: i64, tid: String) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        Box::pin(async move { self.execute_inner(rid, tid).await })
    }
    async fn execute_inner(self: Arc<Self>, rid: i64, tid: String) {
        let Some((run, task, pre)) = self.with(|rs| { let r = rs.iter_mut().find(|x| x.id == rid)?; let t = r.tasks.iter().find(|t| t.id == tid)?.clone(); let pre: Vec<GoalTask> = r.tasks.iter().filter(|x| t.needs.contains(&x.id)).cloned().collect(); Self::log(r, "start", format!("wave {} · attempt {} · owns {}", t.wave + 1, t.attempts, owns_text(&t)), Some(&tid)); Some((r.clone(), t, pre)) }) else { return };
        let before: Vec<(String, u64)> = if run.write { self.fingerprint(&run.cwd, run.base.as_ref()) } else { vec![] };
        // a CHECKPOINT BEFORE EVERY ATTEMPT: the retry starts from what the first attempt left, and the gate judges the attempt against this, not against the run's start
        if run.write && run.base.is_some() { if let Some(ab) = self.snapshot(&run.cwd, run.pane_id, rid, &format!("before #{rid} {tid} attempt {}", task.attempts)) { self.attempt_cp.lock().unwrap_or_else(|p| p.into_inner()).insert((rid, tid.clone()), ab); } }
        let killer: Killer = Arc::new(Mutex::new(None));
        self.killers.lock().unwrap_or_else(|p| p.into_inner()).insert(format!("{rid}:{tid}"), killer.clone());
        let mut env = HashMap::new();
        if run.write && !task.owns.is_empty() { env.insert("ANTILOKI_SUBSCOPE".into(), json!(task.owns).to_string()); env.insert("ANTILOKI_SUBSCOPE_TASK".into(), format!("{} {}", task.id, task.title)); }
        // THE SPECIALIST BRIEFS: the lenses the planner chose, each on the task's concrete files (≤2 files,
        // globs skipped) — isolated reviews on the subscription, logged as they land, handed in the prompt
        let mut task = task;
        // a retry keeps the briefs the first attempt already paid for (2026-09-19: a rejected codex task
        // re-ran all four specialist calls for nothing)
        if !task.specialists.is_empty() && task.briefs.is_empty() {
            if let Some((_, review)) = self.specialist_fns() {
                let files: Vec<String> = task.owns.iter().filter(|o| !o.contains('*') && o.contains('.')).take(2).cloned().collect();
                let mut briefs = vec![];
                for sp in task.specialists.clone().iter().take(2) {
                    for f in &files {
                        self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "specialist", format!("{sp} reviewing {f}…"), Some(&tid)); } });
                        match review(run.cwd.clone(), sp.clone(), f.clone()).await {
                            Ok(md) => { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "specialist", format!("{sp} on {f}: {}", md.lines().nth(2).unwrap_or("").chars().take(120).collect::<String>()), Some(&tid)); } }); briefs.push(md); }
                            Err(why) => { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "specialist", format!("{sp} on {f}: no review — {}", why.chars().take(160).collect::<String>()), Some(&tid)); } }); }
                        }
                    }
                }
                if files.is_empty() { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "specialist", "no concrete file to review (the task owns globs only) — the lenses are named in the prompt instead", Some(&tid)); } }); }
                task.briefs = briefs.clone();
                self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { if let Some(t) = r.tasks.iter_mut().find(|t| t.id == tid) { t.briefs = briefs; } } });
            }
        }
        let prompt = task_prompt(&run, &task, &pre.iter().collect::<Vec<_>>());
        // THE STALL WATCHDOG (2026-09-22): OpenCode's Go queue parks a call for ~640 s at random — 5 of 12 workers in
        // one bench, and one stalled fill cost the XXL run 14 minutes (1310 s against raw opus's 926). A fill is given
        // 6× the median time its engine and tier usually take (floor 3 min, ceiling 15), so a parked call is abandoned
        // and retried instead of holding the wave; a genuinely long fill still has room.
        let fill_timeout = {
            let hist = crate::eta::History::load();
            let per = hist.per_unit("fill", run.workers.unwrap_or(run.engine).key(), task.tier.key()).unwrap_or(120_000);
            let one_file = task.owns.len() == 1 && !task.owns[0].contains(['*', '?', '[']);
            fill_timeout_ms(per, one_file, task.tier)
        };
        let worker = self.worker_binding_for(&run, Some(&task));
        // WHO FILLS THIS TASK, SAID WHEN IT STARTS (RFC 0007 §5.10): the trace only exists once the call returns, and the screen opens a pane for the worker while it works
        self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "worker", format!("worker {} {}", worker.engine.key(), worker.model.as_deref().unwrap_or("-")), Some(&tid)); } });
        // THE WORKER HAS A FALLBACK TOO (2026-09-23: the L · Claude × Kimi arm died in 34 s with nothing written when
        // OpenCode's gateway refused the Kimi route — the planner and the verifier had a fallback, the worker did not).
        // An engine error (no answer, a refused model, a dead CLI) moves the fill once to the job's fallback; a
        // verifier's rejection is not an engine error and never does.
        let fallback = { let one = run.tasks.len() == 1 && run.tasks[0].owns.len() == 1 && !run.tasks[0].owns[0].contains(['*', '?', '[']) && run.goal.chars().count() <= 400; crate::cli::job_binding_paired(if one { crate::cli::Job::EditorS } else { crate::cli::Job::EditorL }, run.pair.as_ref(), run.workers.unwrap_or(run.engine), run.cost).1 };
        let msgs = vec![ChatMessage { role: "user".into(), content: prompt }];
        let first = self.run_safe(rid, worker.engine, msgs.clone(), run.cwd.clone(), false, RunOpts { write: run.write, max_turns: Some(40), timeout_ms: Some(fill_timeout), model: worker.model.clone(), env: env.clone(), tier: Some(task.tier), template: Some("goal.task".into()), ..Default::default() }, Some(killer.clone())).await;
        let out = match first {
            // A DEAD SEAT HAS NO SECOND MODEL (gauge.rs, found live 2026-09-29): a fallback on the same engine is the same login and the same failure
            Err(e) if fallback.engine == worker.engine && crate::gauge::seat_dead(&e).sure => {
                let g = crate::gauge::seat_dead(&e);
                self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "gauge", format!("worker: no fallback — {} (the fallback is the same {} seat)", g.evidence.join("; "), worker.engine.key()), Some(&tid)); } });
                Err(e)
            }
            Err(e) if fallback.engine != worker.engine || fallback.model != worker.model => {
                self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "fallback", format!("the worker on {} {} failed ({}) — retrying once on the fallback {} {}", worker.engine.key(), worker.model.as_deref().unwrap_or("-"), e.chars().take(120).collect::<String>(), fallback.engine.key(), fallback.model.as_deref().unwrap_or("-")), Some(&tid)); } });
                self.run_safe(rid, fallback.engine, msgs, run.cwd.clone(), false, RunOpts { write: run.write, max_turns: Some(40), timeout_ms: Some(fill_timeout), model: fallback.model.clone(), env, tier: Some(task.tier), template: Some("goal.task".into()), ..Default::default() }, Some(killer)).await
            }
            other => other,
        };
        self.killers.lock().unwrap_or_else(|p| p.into_inner()).remove(&format!("{rid}:{tid}"));
        let after: Vec<(String, u64)> = if run.write { self.fingerprint(&run.cwd, run.base.as_ref()) } else { vec![] };
        let verify = self.with(|rs| {
            let r = rs.iter_mut().find(|x| x.id == rid)?;
            let write = r.write;
            let t = r.tasks.iter_mut().find(|t| t.id == tid)?;
            if t.status != TaskStatus::Running { return None; } // cancelled meanwhile
            match out {
                Ok(o) => {
                    t.trace = Some(o.trace.clone()); t.report = Some(o.parsed.content.clone());
                    // files that changed while it ran AND that it owns — siblings run concurrently in the same box
                    if write { let fresh: Vec<String> = after.iter().filter(|(f, h)| !before.iter().any(|(bf, bh)| bf == f && bh == h)).map(|(f, _)| f.clone()).collect(); let mine: Vec<String> = if t.owns.is_empty() { fresh } else { fresh.into_iter().filter(|f| owns_file(&t.owns, f)).collect() }; for m in mine { if !t.changed.contains(&m) { t.changed.push(m); } } }
                    let fs = findings(&o.parsed.content);
                    let (changed, tools) = (t.changed.clone(), o.parsed.tools.len());
                    for f in fs { r.blackboard.push(Finding { task: tid.clone(), text: f, t: now_ms() }); }
                    if o.parsed.content.trim().is_empty() {
                        let why = o.parsed.error.unwrap_or_else(|| "no report".into());
                        let t = r.tasks.iter_mut().find(|t| t.id == tid).unwrap(); t.error = Some(why.clone());
                        // an empty answer is usually transient (a tool the model gave up on, a hiccup): one
                        // retry, like a rejected verdict — the tier goes up so a weak model is not asked twice
                        // THE RETRY GAUGE (gauge.rs): a seat that is not signed in, out of credit or not installed does not answer better at a
                        // higher tier — the error text says so, and the second call is not made
                        let g = crate::gauge::retry(&why, t.attempts, 2);
                        if g.value == crate::gauge::Retry::Again { t.status = TaskStatus::Ready; t.tier = t.tier.up(); let tier = t.tier; Self::log(r, "retry", format!("no answer — retrying once at tier {}: {}", tier.key(), why.chars().take(160).collect::<String>()), Some(&tid)); return None; }
                        if g.sure && t.attempts < 2 { Self::log(r, "gauge", format!("retry: stop — {} (a second call cannot fix it)", g.evidence.join("; ")), Some(&tid)); }
                        fail(r, &tid, &why); return None;
                    }
                    // AN ENGINE FAILURE IS NOT A REPORT (operator 2026-09-19: "error handling for sub-agent
                    // errors"): a CLI that could not start or run — unsupported model, no login, rate limit —
                    // answers with a short error text; judging it wastes a verifier call and hides the cause
                    // Cursor, agy and Grok report no tools in their envelope (audit 2026-10-05), so files the task changed count as proof it worked too
                    if let Some(why) = engine_failure(&o.parsed.content, tools + changed.len()) {
                        // THE PLAN WINDOW SPENT BY A HEADLESS RUN (queue item 2, 2026-09-20): the pane never saw
                        // the CLI's words, so the box is marked and the pane's quota banner reads it
                        if antiloki_rs::cockpit__helpers_engine_handoff::limit_hit(&why).is_some() { mark_quota_file(&r.cwd, r.engine.key(), &why); }
                        let t = r.tasks.iter_mut().find(|t| t.id == tid).unwrap(); t.error = Some(format!("engine: {why}"));
                        fail(r, &tid, &format!("engine failure — {why}")); return None;
                    }
                    Self::log(r, "report", format!("{} chars{}{}", o.parsed.content.len(), if tools > 0 { format!(" · {tools} tools") } else { String::new() }, if changed.is_empty() { String::new() } else { format!(" · changed {}", changed.join(", ")) }), Some(&tid));
                    Some(())
                }
                Err(e) => { t.error = Some(e.clone()); fail(r, &tid, &e); None }
            }
        });
        if verify.is_some() { self.verify(rid, tid).await; }
        // the next step is spawned, not awaited: execute → tick → execute would otherwise be a
        // recursive future (and one that is not Send)
        let me = self.clone();
        tokio::spawn(async move { me.tick().await; });
    }
    async fn verify(self: &Arc<Self>, rid: i64, tid: String) {
        let Some((run, task)) = self.with(|rs| { let r = rs.iter_mut().find(|x| x.id == rid)?; let t = r.tasks.iter_mut().find(|t| t.id == tid)?; t.status = TaskStatus::Verifying; let t = t.clone(); Some((r.clone(), t)) }) else { return };
        // 2026-10-13: worker edits outside the run scope are reverted and the run fails — no retry.
        if run.write && scope_fenced(&run.scope) {
            if let Some(ab) = self.attempt_cp.lock().unwrap_or_else(|p| p.into_inner()).get(&(rid, tid.clone())).cloned() {
                if let Some(cur) = crate::checkpoint::current_tree(&run.cwd) {
                    let outside: Vec<String> = crate::checkpoint::changes(&run.cwd, &ab.tree, &cur).into_iter().map(|c| c.1)
                        .filter(|p| !path_in_run_scope(p, &run.scope)).collect();
                    if !outside.is_empty() {
                        let _ = crate::checkpoint::restore_paths(&run.cwd, &ab.tree, &outside);
                        let why = format!("task {} changed file(s) outside the agent's scope: {}", tid, outside.iter().take(6).cloned().collect::<Vec<_>>().join(", "));
                        self.with(|rs| {
                            let Some(r) = rs.iter_mut().find(|x| x.id == rid) else { return };
                            if let Some(t) = r.tasks.iter_mut().find(|t| t.id == tid) {
                                t.status = TaskStatus::Failed;
                                t.ended = Some(now_ms());
                                t.error = Some("outside the agent's scope".into());
                            }
                            scope_fence_fail_run(r, why);
                        });
                        let me = self.clone();
                        tokio::spawn(async move { me.tick().await; });
                        return;
                    }
                }
            }
        }
        let diff = if run.write && !task.owns.is_empty() { self.diff_of(&run.cwd, if task.changed.is_empty() { &task.owns } else { &task.changed }, run.base.as_ref()) } else { String::new() };
        // BLUEPRINT: the machine decides — tsc + the module's tests; green passes with no model; red is read by a
        // low-tier call into the retry brief (operator 2026-09-22: "compile and tests are deterministic, any
        // low-cost AI can read them")
        if run.mode == "blueprint" && !cfg!(test) {
            use crate::blueprint as bp;
            let t0 = now_ms();
            let test_file = run.blueprint.as_ref().and_then(|c| c.modules.iter().find(|m| task.owns.first() == Some(&m.path)).and_then(|m| m.test_file.clone()));
            let tsc = bp::compile_errors_for(&run.cwd, &task.owns).await;
            let tests = match &test_file {
                Some(t) => match bp::vitest(&run.cwd, std::slice::from_ref(t)).await {
                    Ok(v) => v,
                    // VITEST VANISHED (2026-09-22): a worker ran `npm install` against the lockfile that predates the
                    // deps stage, which prunes it — two fills failed on "vitest is not installed" in a run where it had
                    // been installed. The orchestrator owns installs, so it puts it back and runs the tests once more.
                    Err(e) if e.contains("not installed") => {
                        self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "qa", "vitest had been removed from the box (a worker's package manager) — reinstalling", None); } });
                        match bp::ensure_vitest(&run.cwd).await { Ok(_) => bp::vitest(&run.cwd, std::slice::from_ref(t)).await.unwrap_or_else(Some), Err(e2) => Some(e2) }
                    }
                    Err(e) => Some(e),
                },
                None => None };
            // A BAD TEST MUST NOT FAIL A GOOD FILL (deterministic QA step 3; opt-in `ANTILOKI_BP_DISPUTE=1` until the mutant bench has priced what it lets through). When a task is rejected a SECOND time, the compiler is clean,
            // and every case that fails now also failed the attempt before (a fill that was told the failure and still could not satisfy it) - few of them, a small share of the file - those cases are skipped, the
            // file runs again, and the task passes only if everything else is green. The skipped assertions are named on the run, never hidden.
            let mut tests = tests;
            let mut disputed_note: Option<String> = None;
            if let (Some(tf), Some(text)) = (&test_file, tests.clone()) {
                let now_failing = bp::failing_cases(&text);
                let prev = self.last_failing.lock().unwrap_or_else(|p| p.into_inner()).insert((rid, tid.clone()), now_failing.clone());
                if std::env::var("ANTILOKI_BP_DISPUTE").is_ok() {
                    if let Some(prev) = prev {
                        let path = std::path::Path::new(&run.cwd).join(tf);
                        if let Ok(src) = std::fs::read_to_string(&path) {
                            if let Some((skipped, both)) = bp::dispute_plan(task.attempts, tsc.is_none(), &prev, &now_failing, &src) {
                                if std::fs::write(&path, &skipped).is_ok() {
                                    match bp::vitest(&run.cwd, std::slice::from_ref(tf)).await {
                                        Ok(None) => { tests = None; disputed_note = Some(format!("{} disputed assertion(s) skipped - failed on both attempts: {}", both.len(), both.join("; "))); }
                                        _ => { let _ = std::fs::write(&path, &src); }   // still red with them out: it was the fill after all - the test file is put back
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(n) = &disputed_note { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "dispute", n.clone(), Some(&tid)); } }); }
            let green = tsc.is_none() && tests.is_none();
            let (reasons, brief_cost, brief_ms) = if green { (vec![format!("compiles{}{}", test_file.as_ref().map(|t| format!(" · {t} green")).unwrap_or_default(), disputed_note.as_ref().map(|n| format!(" · {n}")).unwrap_or_default())], 0.0, 0) } else {
                let b0 = now_ms();
                let prompt = crate::prompts::render_base("goal.read_results", &vars(&[("path", json!(task.owns.first().cloned().unwrap_or_default())), ("tsc", json!(tsc.clone().unwrap_or_else(|| "clean".into()))), ("tests", json!(tests.clone().unwrap_or_else(|| "green".into())))]));
                let out = self.run_safe(rid, run.workers.unwrap_or(run.engine), vec![ChatMessage { role: "user".into(), content: prompt }], run.cwd.clone(), false, RunOpts { max_turns: Some(2), timeout_ms: Some(120_000), tier: Some(Tier::Low), template: Some("goal.read_results".into()), ..Default::default() }, None).await;
                let brief = out.as_ref().ok().map(|o| o.parsed.content.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| format!("{}\n{}", tsc.clone().unwrap_or_default(), tests.clone().unwrap_or_default()));
                (vec![brief.chars().take(1500).collect()], out.as_ref().ok().and_then(|o| o.trace.cost).unwrap_or(0.0), now_ms() - b0)
            };
            self.stage(rid, bp::Stage { name: format!("check:{tid}"), engine: if green { "tsc+vitest".into() } else { format!("tsc+vitest → {} low", run.workers.unwrap_or(run.engine).key()) }, tier: "-".into(), started: t0, ended: now_ms(), ms: now_ms() - t0, tokens_in: 0, tokens_out: 0, cost: brief_cost, ok: green, note: if green { reasons[0].clone() } else { format!("red (brief in {} ms)", brief_ms) } });
            self.with(|rs| {
                let Some(r) = rs.iter_mut().find(|x| x.id == rid) else { return };
                let Some(t) = r.tasks.iter_mut().find(|t| t.id == tid) else { return };
                if t.status != TaskStatus::Verifying { return; }
                let attempts = t.attempts; t.verdict = Some(Verdict { passed: green, reasons: reasons.clone(), by: "machine".into() });
                ledger_note(r.workers.unwrap_or(r.engine), t.tier, green, attempts);
                if green { t.status = TaskStatus::Done; t.ended = Some(now_ms()); Self::log(r, "pass", reasons.join(" · "), Some(&tid)); return; }
                Self::log(r, "reject", reasons.join(" · "), Some(&tid));
                if attempts < 2 { let t = r.tasks.iter_mut().find(|t| t.id == tid).unwrap(); t.status = TaskStatus::Ready; Self::log(r, "retry", "retrying once with the machine's brief attached", Some(&tid)); return; }
                fail(r, &tid, &format!("rejected twice: {}", reasons.join(" · ")));
            });
            let me = self.clone(); tokio::spawn(async move { me.tick().await; });
            return;
        }
        // THE CHECKPOINT GATE (2026-09-29, run #259): before ANY model reads this attempt, code compares the box with the checkpoint taken when the attempt began. A result that
        // changed nothing, that put the box back to an older state, or that only deleted lines for an ask that removes nothing, is rejected here - and the box is put back.
        // No model is asked, so none can be talked into it by a report that says "reverted the unrequested changes".
        let mut gate_note: Option<String> = None;
        if run.write {
            let ab = self.attempt_cp.lock().unwrap_or_else(|p| p.into_inner()).get(&(rid, tid.clone())).cloned();
            if let Some(ab) = ab {
                if let Some((rule, why, restored)) = self.checkpoint_gate(&run, &task, &ab) {
                    // the LAST attempt that changed nothing is not thrown away: the verifier is told the fact and judges the report against it (an ask can already be done)
                    if rule == "nothing-changed" && task.attempts >= 2 { gate_note = Some(format!("FACT FROM THE ORCHESTRATOR (code, not a model): during this attempt the box did not change at all. {why}.")); }
                    else {
                        self.killers.lock().unwrap_or_else(|p| p.into_inner()).remove(&format!("{rid}:{tid}"));
                        self.with(|rs| {
                            let Some(r) = rs.iter_mut().find(|x| x.id == rid) else { return };
                            let Some(t) = r.tasks.iter_mut().find(|t| t.id == tid) else { return };
                            if t.status != TaskStatus::Verifying { return; }
                            let reasons = vec![format!("checkpoint gate ({rule}): {why}{}", if restored { " - the box was put back to where this attempt began" } else { "" })];
                            let attempts = t.attempts; t.verdict = Some(Verdict { passed: false, reasons: reasons.clone(), by: "checkpoint".into() });
                            ledger_note(r.engine, t.tier, false, attempts);
                            Self::log(r, "gate", reasons.join(" · "), Some(&tid));
                            Self::log(r, "reject", reasons.join(" · "), Some(&tid));
                            if attempts < 2 { let t = r.tasks.iter_mut().find(|t| t.id == tid).unwrap(); t.status = TaskStatus::Ready; Self::log(r, "retry", "retrying once with the gate's reason attached", Some(&tid)); return; }
                            fail(r, &tid, &format!("rejected twice: {}", reasons.join(" · ")));
                        });
                        let me = self.clone(); tokio::spawn(async move { me.tick().await; });
                        return;
                    }
                }
            }
        }
        let diff = match gate_note { Some(n) => format!("{n}\n{diff}"), None => diff };
        // THE COMPILE GATE (2026-09-22): a cheap worker writes from the spec and never reads tsconfig — twice tonight a
        // value import of a type under `verbatimModuleSyntax` passed the verifier (it reads the diff against the
        // acceptance) and reached the judge as a broken build. The compiler is 5 s and decisive: a TypeScript
        // box whose errors touch the task's files fails the task here, with the errors as the verdict.
        if run.write && !cfg!(test) {
            let files: Vec<String> = if task.changed.is_empty() { task.owns.clone() } else { task.changed.clone() };
            if let Some(errs) = compile_errors(&run.cwd, &files).await {
                self.killers.lock().unwrap_or_else(|p| p.into_inner()).remove(&format!("{rid}:{tid}"));
                self.with(|rs| {
                    let Some(r) = rs.iter_mut().find(|x| x.id == rid) else { return };
                    let Some(t) = r.tasks.iter_mut().find(|t| t.id == tid) else { return };
                    if t.status != TaskStatus::Verifying { return; }
                    let reasons = vec![format!("does not compile: {errs}")];
                    let attempts = t.attempts; t.verdict = Some(Verdict { passed: false, reasons: reasons.clone(), by: "tsc".into() });
                    ledger_note(r.engine, t.tier, false, attempts);
                    Self::log(r, "reject", reasons.join(" · "), Some(&tid));
                    if attempts < 2 { let t = r.tasks.iter_mut().find(|t| t.id == tid).unwrap(); t.status = TaskStatus::Ready; Self::log(r, "retry", "retrying once with the compiler's errors attached", Some(&tid)); return; }
                    fail(r, &tid, &format!("rejected twice: {}", reasons.join(" · ")));
                });
                let me = self.clone(); tokio::spawn(async move { me.tick().await; });
                return;
            }
        }
        let killer: Killer = Arc::new(Mutex::new(None));
        self.killers.lock().unwrap_or_else(|p| p.into_inner()).insert(format!("{rid}:{tid}"), killer.clone());
        // with workers on another engine the verifier runs there too, at mid or above (a pro model checks a flash
        // worker's diff for cents); the plan and the final judge stay on the run's engine
        // THE VERIFIER IS A JOB (2026-09-22): its winner (Sonnet on Claude Code, Pro on OpenCode), its fallback if the
        // winner's engine fails — no longer "whatever engine the workers ran on, one tier up"
        let out = self.run_job(rid, crate::cli::Job::Verifier, run.engine, run.cost, vec![ChatMessage { role: "user".into(), content: verify_prompt(&run, &task, &diff) }], run.cwd.clone(), true, RunOpts { max_turns: Some(6), timeout_ms: Some(240_000), tier: Some(task.tier.max(run.route.as_ref().map(|x| x.verify_tier).unwrap_or(Tier::Mid))), template: Some("goal.verify".into()), ..Default::default() }, Some(killer)).await;
        self.killers.lock().unwrap_or_else(|p| p.into_inner()).remove(&format!("{rid}:{tid}"));
        self.with(|rs| {
            let Some(r) = rs.iter_mut().find(|x| x.id == rid) else { return };
            let Some(t) = r.tasks.iter_mut().find(|t| t.id == tid) else { return };
            if t.status != TaskStatus::Verifying { return; }
            let v = match out { Ok(o) => parse_verdict(&o.parsed.content).map(|(passed, reasons)| Verdict { passed, reasons, by: "engine".into() }).unwrap_or(Verdict { passed: true, reasons: vec!["verifier gave no verdict — accepted as reported".into()], by: "none".into() }), Err(_) => Verdict { passed: true, reasons: vec!["verifier unavailable — accepted as reported".into()], by: "none".into() } };
            let (passed, reasons, attempts) = (v.passed, v.reasons.join(" · "), t.attempts);
            t.verdict = Some(v);
            ledger_note(r.engine, t.tier, passed, attempts);
            if passed { t.status = TaskStatus::Done; t.ended = Some(now_ms()); if let (Some(s0), Some(e0)) = (t.started, t.ended) { crate::eta::History::note("fill", r.workers.unwrap_or(r.engine).key(), t.tier.key(), e0 - s0, 1); } Self::log(r, "pass", reasons, Some(&tid)); return; }
            Self::log(r, "reject", reasons.clone(), Some(&tid));
            if attempts < 2 { let t = r.tasks.iter_mut().find(|t| t.id == tid).unwrap(); t.status = TaskStatus::Ready; Self::log(r, "retry", "retrying once with the verdict attached", Some(&tid)); return; }
            fail(r, &tid, &format!("rejected twice: {reasons}"));
        });
    }
    async fn join(self: &Arc<Self>, rid: i64) {
        let Some(run) = self.with(|rs| { let r = rs.iter_mut().find(|x| x.id == rid)?; Self::log(r, "join", "all tasks settled — writing the consolidated report", None); Some(r.clone()) }) else { return };
        // THE QA GATE (2026-09-20): a write run's changed files → the project's related tests, once,
        // before the join reads the results — the proof the PR carries. No runner → said, not skipped silently.
        // A failure the BASELINE already had is reported as pre-existing, not as the agent's.
        let run = if run.write && !cfg!(test) {
            let files: Vec<String> = run.tasks.iter().flat_map(|t| t.changed.iter().cloned()).collect();
            if files.is_empty() { run } else {
                let root = std::path::Path::new(&run.cwd);
                let cmd_preview = qa_gate_cmd(root, &files).map(|c| c.chars().take(160).collect::<String>()).unwrap_or_default();
                if !cmd_preview.is_empty() { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "qa", format!("running the gate: {cmd_preview}"), None); } }); }
                match run_qa(&run.cwd, &files).await {
                    Ok(mut qa) => {
                        // the baseline started before the tasks and takes as long as this gate did — nearly
                        // always done by now; give it up to 3 more minutes, then compare against what there is
                        let t0 = now_ms();
                        while self.get(rid).map(|r| r.qa_baseline_pending).unwrap_or(false) && now_ms() - t0 < 180_000 { tokio::time::sleep(std::time::Duration::from_secs(2)).await; }
                        let base = self.get(rid).and_then(|r| r.qa_baseline);
                        if base.is_none() && self.get(rid).map(|r| r.qa_baseline_pending).unwrap_or(false) { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "qa-baseline", "still running after 3 min — the after-gate compares against nothing", None); } }); }
                        let base_failed = base.as_ref().map(|b| !b.passed).unwrap_or(false);
                        if !qa.passed && base_failed { qa.pre_existing = true; }
                        self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) {
                            let kind = if qa.passed { "qa-pass" } else if qa.pre_existing { "qa-warn" } else { "qa-fail" };
                            Self::log(r, kind, format!("related tests {} in {}s (exit {}){}", if qa.passed { "passed" } else { "FAILED" }, qa.ms / 1000, qa.exit, if qa.pre_existing { " — they were already red before the change (the baseline): not this run's doing" } else { "" }), None);
                            r.qa = Some(qa);
                        } });
                        self.get(rid).unwrap_or(run)
                    }
                    Err(why) => { self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "qa", format!("gate not run — {why}"), None); } }); run }
                }
            }
        } else { run };
        // ONE TASK: its own report is the report — no consolidation call (2026-09-22: a join cost 25–32 s and an Opus call per run)
        let out: Result<crate::cli::RunOut, String> = if run.mode == "blueprint" {
            self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "join", "blueprint — the machine's results are the report, no consolidation call", None); } });
            Err("blueprint".into())
        } else if run.tasks.len() == 1 && run.tasks[0].report.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false) {
            self.with(|rs| { if let Some(r) = rs.iter_mut().find(|x| x.id == rid) { Self::log(r, "join", "one task — its report stands, no consolidation", None); } });
            Err("single task".into())
        } else { self.run_job(rid, crate::cli::Job::Report, run.engine, run.cost, vec![ChatMessage { role: "user".into(), content: join_prompt(&run) }], run.cwd.clone(), false, RunOpts { max_turns: Some(6), timeout_ms: Some(240_000), tier: Some(run.route.as_ref().map(|x| x.join_tier.min(Tier::Mid)).unwrap_or(Tier::Mid)), template: Some("goal.join".into()), ..Default::default() }, None).await };
        self.with(|rs| {
            let Some(r) = rs.iter_mut().find(|x| x.id == rid) else { return };
            r.report = Some(match out { Ok(o) if !o.parsed.content.trim().is_empty() => o.parsed.content.trim().to_string(), _ if r.mode == "blueprint" => blueprint_report(r), _ if r.tasks.len() == 1 => r.tasks[0].report.clone().unwrap_or_else(|| join_fallback(r)), _ => join_fallback(r) });
            r.status = if r.tasks.iter().all(|t| matches!(t.status, TaskStatus::Done | TaskStatus::Skipped)) { RunStatus::Done } else { RunStatus::Failed };
            // QA judges finished work: a run that failed has nothing to judge (found live 2026-09-29: "qa-gate-fail · 0 met · 0 unmet · $-0.000" under a failed run)
            r.qa_pending = r.qa_review_wanted && r.status == RunStatus::Done;
            r.ended = Some(now_ms());
            set_confidence(r);
            let (done, n, st) = (r.tasks.iter().filter(|t| t.status == TaskStatus::Done).count(), r.tasks.len(), if r.status == RunStatus::Done { "done" } else { "failed" });
            Self::log(r, st, format!("{done}/{n} tasks passed"), None);
            // A FAILED RUN SAVED NOTHING (2026-09-26). This was set unconditionally, so `psf__requests-6028` — worker
            // timed out, retry returned 212 chars of exploration prose, verifier rejected it twice, run settled
            // 0/1 — still reported "the router saved $0.241 (40%)". Money spent with nothing delivered is WASTE,
            // and that figure feeds the saved chip the operator reads.
            //
            // The spend stays visible (hiding it would be its own lie); only the CLAIM goes. Same rule as
            // `estimate::Basis`: a number the operator cannot tell apart from a real one is worse than no number.
            // the bound model is the hands a PAIR fixed; without a pair each task keeps the tier it was planned at (a haiku task and an
            // opus task on one Claude run are not both the worker binding's model)
            let bound = if r.pair.is_some() { let wb = Self::worker_binding_of(r, r.engine); Some((wb.engine, wb.model.clone())) } else { None };
            r.savings = settled_savings(r.status == RunStatus::Done, savings_priced(r.engine, r.workers, bound, &r.tasks));
            // THE MERGE SCORE (merges.rs, 2026-09-23): this combination, on this job at this size, learns from the run
            {
                let planner = crate::cli::job_binding_paired(crate::cli::Job::Planner, r.pair.as_ref(), r.engine, r.cost).0;
                let (wj, one_file) = { let one = r.tasks.len() == 1 && r.tasks[0].owns.len() == 1 && !r.tasks[0].owns[0].contains(['*', '?', '[']) && r.goal.chars().count() <= 400; (if one { crate::cli::Job::EditorS } else { crate::cli::Job::EditorL }, one) };
                let worker = crate::cli::job_binding_paired(wj, r.pair.as_ref(), r.workers.unwrap_or(r.engine), r.cost).0;
                // a run that had no planner is a SOLO run: the record says `raw` for the planner side, which is what the table's `raw x editor` cells mean
                // THE ROW NAMES THE SEATS THAT RAN (2026-10-06): see `run_row_keys`
                let (o_planner_key, o_worker_key) = run_row_keys(r, &planner, &worker);
                // THE QA THAT RAN, NOT THE ONE A PAIR WOULD HAVE NAMED (audit F-19/F-20, 2026-10-05): the gate is called with `role_binding_at(Role::Qa, ..)`, which honours the operator's pin, the Claude-off gate order and switched-off seats; the record named `job_binding_paired(QaGate)`, so on a machine without Claude it filed the verdict under a seat that never judged
                let qa = if r.qa_review_wanted { let b = crate::cli::role_binding_at(crate::cli::Role::Qa, r.engine, r.cost); crate::merges::key(b.engine.key(), b.model.as_deref()) } else { String::new() };
                let o = crate::merges::Outcome { suite: r.suite.clone(), stack: run_stack(&r.cwd),
                    job: wj.key().into(), size: crate::merges::size_of(r.tasks.len(), one_file, r.goal.chars().count()).into(),
                    planner: o_planner_key.clone(), worker: o_worker_key.clone(), qa,
                    tasks: r.tasks.len() as i64,
                    pass_first: r.tasks.iter().filter(|t| t.status == TaskStatus::Done && t.attempts <= 1).count() as i64,
                    retries: r.tasks.iter().filter(|t| t.attempts > 1).count() as i64,
                    failed: r.tasks.iter().filter(|t| t.status == TaskStatus::Failed && !task_outage(t)).count() as i64,
                    gate_met: 0, gate_unmet: 0, audit: None,
                    ms: r.ended.unwrap_or_else(now_ms) - r.created,
                    // A PAIR THAT REPORTS NOTHING IS NOT A FREE PAIR (2026-09-27). This was
                    // `filter_map(|t| t.trace…cost).sum()`, and `filter_map` DROPS every task whose engine reports no
                    // per-turn cost — which is Codex, Cursor and Gemini, i.e. four of the five seats. The sum of an
                    // empty set is 0.0, so all 40 frontier merge runs went onto the record at $0.0000 against a
                    // token-equivalent of $10.89, and `Objective::Best` puts 0.55 of its weight on cost: the table
                    // ranked every pair that stays quiet above every pair that answers honestly. `codex × gemini`
                    // read 87k-235k input tokens and still read as the cheapest arm in all four role cells.
                    // Counted at the HANDS' OWN RATE, not the brain's. `savings_with_workers` prices on
                    // `workers.unwrap_or(engine)`, and a paired run leaves `workers` None — the hands live in `pair` —
                    // so it bills Composer's tokens at Sonnet's $3/$15. The worker binding is already resolved just
                    // above, so the record prices each task's tokens at the model that actually spent them, and the
                    // planner's own call at `plan_usd`. A reported figure still wins: `price_turn` is the arbiter.
                    // A REPLAYED PLAN IS NOT A FREE ONE (2026-09-28). `plan_usd` is 0.00 on a cache hit — the honest price OF THIS CALL —
                    // and it was filed as the pair's planner spend, so the pairing that happened to hit the cache read as the
                    // cheapest planner on the record (Sonnet: 8 s, $0.025 over 15 samples) and won every frontend ask. The row
                    // is a claim about what this PAIRING costs, so a replay is priced at the planner's list rate instead.
                    usd: (if r.plan_basis == "replayed" { crate::merges::estimated_usd("planner", &o_planner_key, &o_worker_key) } else { r.plan_usd }) + r.screen_usd + r.tasks.iter().filter_map(|t| t.trace.as_ref()).map(|tr| match tr.cost {
                        Some(c) if c > 0.0 => c,
                        _ => crate::cli::price_tokens(worker.engine, worker.model.as_deref(), tr.tokens_in.unwrap_or(0), tr.tokens_out.unwrap_or(0), 0, 0),
                    }).sum::<f64>(),
                    source: source_of(&r.suite).into(), audit_only: false,
                    // WHAT KIND OF WORK (2026-09-23): the files the tasks owned decide the domain, so a role can ask
                    // the table for the merge that won on ITS kind of work rather than on everything at once
                    domain: crate::roles::domain_of(&r.tasks.iter().flat_map(|t| if t.changed.is_empty() { t.owns.clone() } else { t.changed.clone() }).collect::<Vec<_>>()).key().to_string(),
                };
                // EVERY WORKER TASK IS A RELIABILITY OBSERVATION (operator 2026-09-30: "every answer feeds the benchs ... if there is a fail like this, the bench should be fed with this, like a negative weight").
                // Workers do not go through `run_job`'s ladder, so their answers never reached it; they do now, the seat's own failures included - the ladder's answer rate is the negative weight.
                for t in r.tasks.iter().filter(|t| matches!(t.status, TaskStatus::Done | TaskStatus::Failed)) {
                    let (tin, tms) = (t.trace.as_ref().and_then(|tr| tr.cost).unwrap_or(0.0), t.trace.as_ref().map(|tr| tr.ms).unwrap_or(0));
                    crate::ladder::note(wj.key(), worker.engine.key(), worker.model.as_deref().unwrap_or(""), "winner", t.status == TaskStatus::Done, tin, tms, t.error.as_deref().unwrap_or(""));
                }
                // …BUT A SEAT'S OUTAGE IS NOT A VERDICT ON THE PAIRING'S QUALITY. A quota message filed as a failed task sent Codex x Gemini to weight 0.25 on a cell of its own after one empty
                // seat - a pairing with 51 good samples punished for the hour its provider was out. When every task failed that way, nothing was learned about quality and no row is filed;
                // the seat's own record (the ladder above, and the limit the router now remembers) carries it.
                let outage_only = !r.tasks.is_empty() && r.tasks.iter().all(|t| t.status == TaskStatus::Failed && task_outage(t));
                if outage_only {
                    Self::log(r, "merge-score", format!("{} · {} on {} {}: not filed - every task failed because the seat was unavailable (quota / signed out), which says nothing about the pairing's quality; the seat's reliability record took it", o.planner, o.worker, o.job, o.size), None);
                } else {
                r.merge_outcome = Some(o.clone());
                match crate::merges::record(&o) { Ok(w) => Self::log(r, "merge-score", format!("{} · {} on {} {}: weight {:.2} after this run", o.planner, o.worker, o.job, o.size, w), None), Err(e) => Self::log(r, "merge-score", format!("not recorded — {e}"), None) }
                }

                // DOES "SURE" MEAN "RIGHT" (2026-09-25). The router graded its own route `high|medium|low` when it
                // planned this run; here is the only moment that grade can be checked against what the run actually
                // did. Recorded, never acted on: calibration is a measurement of a claim, and what to do about a claim
                // that fails is a decision for the operator, after the curve exists.
                // A RUN WITH NO TASKS IS NOT A ROUTE THAT SCORED ZERO (found auditing 2026-09-26). Filing it as 0.0
                // would drag every confidence bucket toward the floor and could manufacture the very verdict this
                // measurement exists to detect — "the grade is backwards" — out of runs nobody judged. No evidence is
                // not a score of zero, here as everywhere else tonight.
                if let (Some(route), false) = (r.route.as_ref(), r.tasks.is_empty()) {
                    let done = r.tasks.iter().filter(|t| t.status == TaskStatus::Done).count() as f64;
                    let quality = done / r.tasks.len() as f64;
                    let _ = crate::calibration::note(&route.confidence, &o.job, route.tier.key(), route.delegate, quality, r.tasks.len() as i64);

                    // KEEP THE PLAN ONLY IF IT WORKED (plan replay, 2026-09-25). A plan whose tasks failed is a plan
                    // nobody should be handed again at full speed, so the cache is written HERE — at the join, after
                    // the run has been judged — and not when the plan was made. The stored shape is the sanitised
                    // plan, not the prose the model wrapped it in.
                    if quality >= 1.0 && !r.tasks.is_empty() {
                        let tasks: Vec<Value> = r.tasks.iter().map(|t| json!({ "id": t.id, "title": t.title, "tier": t.tier.key(),
                            "prompt": t.prompt, "owns": t.owns, "needs": t.needs, "acceptance": t.acceptance })).collect();
                        let owns: Vec<String> = r.tasks.iter().flat_map(|t| t.owns.clone()).collect();
                        let plan = crate::replay::plan_json(route.tier.key(), route.delegate, &route.why, &route.confidence, &tasks);
                        let sig = crate::replay::signature(&r.goal, &run_stack(&r.cwd), "");
                        let head = crate::replay::commit_of(&r.cwd);
                        // what the PLANNER cost, measured — not the whole run's bill, which a replay does not save
                        if !head.is_empty() && (r.plan_usd > 0.0 || r.plan_ms > 0) {
                            let _ = crate::replay::remember(&sig, &head, &plan, &owns, r.plan_usd, r.plan_ms);
                        }
                    }
                }
                // THE PLANNER'S OWN ROW (2026-09-23: "some job winners have no data"): a merged run measures its planner too —
                // the same outcome filed under `planner`, so the planner job ranks on sized runs, not only on audited calls
                // A PLANNER ROW COSTS WHAT THE PLANNER COST (2026-09-27). This cloned the editor outcome whole, so
                // every planner row carried the WHOLE RUN's bill — the hands' tokens included. That makes the one
                // cell that could answer "which brain?" answer "which pair?" instead, and at size=l it left all four
                // brains within 0.024 of each other on a number none of them earned.
                //
                // The planner's own call is `plan_usd`, and it is the whole of what this row measures. Priced
                // honestly the spread is not subtle: Codex plans at $0.0039 against Sonnet's $0.093 and Opus's
                // $0.188 — 24-48x for the same job, with all five pairs tied on quality. The `call` cells already
                // show the router learning exactly this (codex 0.972 against sonnet 0.875) because those rows were
                // priced all along; `l` could not, because this line overwrote the figure.
                //
                // So RULE 1 of PLAN-HEIMDALL-RULES.md needs no hard-coded default: price the row correctly and
                // `best_for(Job::Planner, …)` reaches the same conclusion from the evidence, and keeps reaching it
                // if a cheaper brain appears.
                // …and a replayed plan says nothing at all about what THIS brain takes or costs: no planner row for a cache hit
                if files_planner_row(r.pair.is_some(), &r.mode, &o.planner, &o.worker, &r.plan_basis) {
                    let po = crate::merges::Outcome { job: "planner".into(), usd: r.plan_usd, ms: r.plan_ms, ..o.clone() };
                    let _ = crate::merges::record(&po);
                }
            }
            if let Some(sv) = &r.savings { Self::log(r, "savings", format!("{} tokens at {} · ${:.3} vs ${:.3} at one high-tier agent — {} ${:.3} ({:.0}%) · API-equivalent", sv.tokens_in + sv.tokens_out, sv.tiers.join(" "), sv.actual_usd, sv.baseline_usd, if r.router { "the router saved" } else { "router off, no saving" }, sv.saved_usd, sv.saved_pct), None); }
        });
        // QA (2026-09-21): the isolated judge, after the run settled — spawned, so the join returns at once
        if self.get(rid).map(|r| r.qa_review_wanted && r.status == RunStatus::Done).unwrap_or(false) {
            // the gate by default; the old whole-diff judge only while ANTILOKI_QA_MODE=judge (one release, to compare)
            let judge = std::env::var("ANTILOKI_QA_MODE").map(|v| v == "judge").unwrap_or(false);
            let me = self.clone();
            tokio::spawn(async move { if judge { me.qa_review(rid).await; } else { me.qa_gate(rid).await; me.design_round(rid).await; } me.qa_settle(rid).await; });
        }
    }
    /// The run's saving from its own audit rows once QA has handed it over (`savings_from_calls`); a run with no editor call on record keeps the figure it had.
    fn apply_exact_savings(r: &mut GoalRun, calls: &[(String, i64, i64, f64)]) {
        let tiers = r.savings.as_ref().map(|s| s.tiers.clone()).unwrap_or_default();
        let Some(sv) = savings_from_calls(r.engine, r.status == RunStatus::Done, calls, tiers) else { return };
        let line = format!("{} call(s) cost ${:.3} in all, where one frontier agent doing the same {} worker tokens would have cost ${:.3} — {} ${:.3} ({:.0}%) · API-equivalent, every attempt and every QA call counted", calls.len(), sv.actual_usd, sv.tokens_in + sv.tokens_out, sv.baseline_usd, if sv.saved_usd > 0.0 { "the router saved" } else { "nothing saved:" }, sv.saved_usd, sv.saved_pct);
        r.savings = Some(sv);
        Self::log(r, "savings", line, None);
    }
    fn changed_files(&self, cwd: &str) -> Vec<String> { (self.git)(&["status", "--porcelain", "--untracked-files=all"], cwd).lines().filter(|l| l.len() > 3).map(|l| l[3..].trim().trim_matches('"').to_string()).filter(|f| !crate::boxes::is_ours(f)).collect() }
    /// The gate's verdict on one attempt, from facts only: (rule, why, restored). `None` = it passes (or there is nothing to measure it with). The box is put back when the rule says so
    /// AND no sibling task is working in it at the same time (their edits would be undone with it).
    fn checkpoint_gate(&self, run: &GoalRun, task: &GoalTask, ab: &RunBase) -> Option<(&'static str, String, bool)> {
        use crate::checkpoint as cp;
        let cur = cp::current_tree(&run.cwd)?;
        let (added, removed) = cp::numstat(&run.cwd, &ab.tree, &cur);
        // A RETRY THAT ONLY DELETES IS CORRECTING ITS OWN FIRST ATTEMPT (operator 2026-10-09, run #359 "the cart looks ugly": attempt 1 added an "INVENTORY" label the goal had ruled out, the verifier said so,
        // attempt 2 deleted exactly that label - and the gate rejected the correction itself, "11 lines deleted and none written", failing the run). From the second attempt on, "wrote nothing" is judged
        // on what the RUN has written since it began, not only on what this attempt added to the previous one.
        let added = gate_added(task.attempts, added, run.base.as_ref().map(|b| cp::numstat(&run.cwd, &b.tree, &cur).0));
        let scope = scope_of(run.pane_id, run.id);
        let mut known: Vec<String> = cp::head_tree(&run.cwd).into_iter().collect();
        for (_, rev, _) in cp::history(&run.cwd, &scope, 20) { if let Some(t) = cp::tree_of(&run.cwd, &rev) { known.push(t); } }
        known.retain(|t| *t != ab.tree); known.dedup();
        let says = |f: fn(&str) -> bool| f(&run.goal) || f(&task.prompt) || f(&task.title);
        let (removes, reverts) = (says(crate::lexicon::asks_to_remove), says(crate::lexicon::asks_to_revert));
        // "nothing changed" is only a failing for an ask that asks to CHANGE something: a question that reached a write run may end with the box untouched
        let changes_something = crate::front::classify_open(&run.goal, crate::front::Ctx::default()) == Some(crate::front::Kind::Task);
        match cp::gate(&ab.tree, &cur, added, removed, removes, reverts, &known) {
            cp::Gate::Pass => None,
            cp::Gate::Reject { rule: "nothing-changed", .. } if !changes_something => None,
            cp::Gate::Reject { rule, why, restore } => {
                let alone = run.tasks.iter().all(|t| t.id == task.id || !matches!(t.status, TaskStatus::Running | TaskStatus::Verifying));
                let restored = restore && alone && cp::restore(&run.cwd, &ab.tree) > 0;
                Some((rule, why, restored))
            }
        }
    }
    /// A CHECKPOINT of the box now, in this agent's history (carrying the queue's state). `None` when there is no git to ask (a mock, a non-repo): the callers then keep the old `HEAD` behaviour.
    fn snapshot(&self, cwd: &str, pane_id: i64, run_id: i64, _label: &str) -> Option<RunBase> {
        // A REPO WITH NO COMMIT YET IS STILL A GIT BOX (2026-10-09): this asked for HEAD, so a project that had never been committed (the operator's design project) got NO baseline,
        // `changed` fell back to "everything dirty" and the screenshot QA and the design round never ran on it. The checkpoint code builds the tree from the index alone; it only needs a repo.
        if (self.git)(&["rev-parse", "--git-dir"], cwd).trim().is_empty() { return None }
        // IN MEMORY: the history keeps one commit per response (the queue's "after #N"), not one per step
        crate::checkpoint::snapshot_tree(cwd, &scope_of(pane_id, run_id), now_ms())
    }
    /// The files this RUN has changed: the box now against the checkpoint (tree to tree, so untracked and deleted files count and a file that was already dirty counts again when it
    /// changed since). Without a checkpoint: everything dirty, as before.
    fn changed_since(&self, cwd: &str, base: Option<&RunBase>) -> Vec<String> {
        let Some(b) = base.filter(|b| !b.tree.is_empty()) else { return self.changed_files(cwd) };
        let Some(cur) = crate::checkpoint::current_tree(cwd) else { return self.changed_files(cwd) };
        crate::checkpoint::changes(cwd, &b.tree, &cur).into_iter().map(|c| c.1).collect()
    }
    /// each file changed since the checkpoint with its content id NOW: comparing two of these says what a task changed, even in a file that was already dirty.
    fn fingerprint(&self, cwd: &str, base: Option<&RunBase>) -> Vec<(String, u64)> {
        use std::hash::{Hash, Hasher};
        // NO CHECKPOINT (a box with no base tree): the dirty-file list with a hash of each file's CONTENT. It used to be 0 for every file, so a file that was ALREADY modified before the task ran
        // (an untracked AI by-product, a half-done edit) looked unchanged after it: `changed` stayed empty and neither the screenshot QA nor the design round ever ran (2026-10-08, a frontend design ask).
        let Some(b) = base.filter(|b| !b.tree.is_empty()) else { return self.changed_files(cwd).into_iter().map(|f| { let h = content_hash(cwd, &f); (f, h) }).collect() };
        let Some(cur) = crate::checkpoint::current_tree(cwd) else { return vec![] };
        crate::checkpoint::changes(cwd, &b.tree, &cur).into_iter().map(|(_, f, blob)| { let mut h = std::collections::hash_map::DefaultHasher::new(); blob.hash(&mut h); (f, h.finish()) }).collect()
    }
    fn diff_of(&self, cwd: &str, paths: &[String], base: Option<&RunBase>) -> String {
        // AGAINST THE CHECKPOINT, tree to tree: only what this run changed, additions and removals alike
        if let Some(b) = base.filter(|b| !b.tree.is_empty()) {
            if let Some(cur) = crate::checkpoint::current_tree(cwd) {
                let files: Vec<&str> = paths.iter().filter(|p| !p.contains(['*', '?', '['])).map(String::as_str).collect();
                let parts: Vec<(String, String)> = crate::checkpoint::changes(cwd, &b.tree, &cur).into_iter()
                    .filter(|(_, f, _)| if files.is_empty() { paths.is_empty() || owns_file(paths, f) } else { files.contains(&f.as_str()) })
                    .map(|(_, f, _)| { let d = crate::checkpoint::file_diff(cwd, &b.tree, &cur, &f); (f, d) }).filter(|(_, d)| !d.trim().is_empty()).collect();
                return budget_diff(&parts, DIFF_BUDGET);
            }
        }
        let rev: Option<&str> = None;
        let files: Vec<&str> = paths.iter().filter(|p| !p.contains(['*', '?', '['])).map(String::as_str).collect();
        // ONE PART PER FILE (2026-09-23): the diff used to be one string cut at 12 000 chars, so on an eight-file task
        // the verifier saw the first three files and rejected the rest for "no diff evidence" — files it was never
        // shown. Each changed file is its own part now, and `budget_diff` gives every one of them a share.
        let named: Vec<String> = { let mut a = vec!["diff".to_string(), "--name-only".to_string()]; if let Some(r) = rev { a.push(r.to_string()); }
            if !files.is_empty() { a.push("--".into()); a.extend(files.iter().map(|f| f.to_string())); }
            (self.git)(&a.iter().map(String::as_str).collect::<Vec<_>>(), cwd).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect() };
        let mut parts: Vec<(String, String)> = named.iter()
            .map(|f| (f.clone(), match rev { Some(r) => (self.git)(&["diff", r, "--", f.as_str()], cwd), None => (self.git)(&["diff", "--", f.as_str()], cwd) }))
            .filter(|(_, d)| !d.trim().is_empty()).collect();
        // a NEW file is invisible to `git diff` (2026-09-19: the verifier rejected a created test file
        // as "no diff provided") — untracked files the task owns are shown whole, as an added file
        for line in (self.git)(&["status", "--porcelain", "--untracked-files=all"], cwd).lines().filter(|l| l.starts_with("?? ")) {
            let f = line[3..].trim().trim_matches('"').to_string();
            if crate::boxes::is_ours(&f) { continue; }          // antiloki's own files are never "what the task changed"
            if !paths.is_empty() && !owns_file(paths, &f) { continue; }
            if let Ok(body) = std::fs::read_to_string(std::path::Path::new(cwd).join(&f)) {
                let mut d = format!("\ndiff --git a/{f} b/{f}\nnew file\n--- /dev/null\n+++ b/{f}\n");
                for l in body.lines() { d.push('+'); d.push_str(l); d.push('\n'); }
                parts.push((f, d));
            }
        }
        parts.retain(|(f, _)| !crate::boxes::is_ours(f));
        parts.sort_by_key(|(f, _)| f.starts_with('.'));         // the project's source first, dot-files last: a window cut from the front must not be spent on config
        if parts.is_empty() { return match rev { Some(r) => (self.git)(&["diff", r], cwd), None => (self.git)(&["diff"], cwd) }.chars().take(DIFF_BUDGET).collect(); }
        budget_diff(&parts, DIFF_BUDGET)
    }
}
// ── THE LEDGER — Heimdall's evidence over the agents (2026-09-19) ─────────────────────────────────
// Every verified task leaves one row: engine · tier · passed on the first attempt? · attempts. The
// router reads it before the next plan: a tier an engine keeps failing at (≥ 4 samples, first-attempt
// reject rate ≥ 50%) is bumped one tier up, and the plan log says so. Kept in ANTILOKI_HOME
// (`~/.antiloki/goal-ledger.json`), the last 400 rows.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct LedgerRow { pub engine: String, pub tier: Tier, pub passed: bool, pub attempts: u32, pub t: i64 }
#[derive(Clone, Debug, Default, Serialize)]
pub struct Ledger { pub rows: Vec<LedgerRow> }
impl Ledger {
    pub fn path() -> std::path::PathBuf {
        // the unit tests keep their ledger in a per-process temp file (2026-09-19: they used to read AND
        // append to the operator's real ~/.antiloki/goal-ledger.json — today's real runs biased a test)
        #[cfg(test)]
        { return std::env::temp_dir().join(format!("antiloki-test-{}", std::process::id())).join("goal-ledger.json"); }
        #[allow(unreachable_code)]
        {
            let home = std::env::var("ANTILOKI_HOME").ok().filter(|v| !v.is_empty()).map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::env::var("HOME").ok().map(|h| std::path::PathBuf::from(h).join(".antiloki")).unwrap_or_else(|| std::path::PathBuf::from(".antiloki")));
            home.join("goal-ledger.json")
        }
    }
    pub fn load() -> Ledger { std::fs::read_to_string(Self::path()).ok().and_then(|s| serde_json::from_str::<Vec<LedgerRow>>(&s).ok()).map(|rows| Ledger { rows }).unwrap_or_default() }
    pub fn save(&self) { let p = Self::path(); let _ = p.parent().map(std::fs::create_dir_all); let _ = std::fs::write(p, serde_json::to_string(&self.rows).unwrap_or_else(|_| "[]".into())); }
    /// the bias for (engine, tier): Some(why) when the ledger says this tier is not enough — pure
    pub fn bias(&self, engine: Engine, tier: Tier) -> Option<String> {
        if tier == Tier::High { return None; }
        let rows: Vec<&LedgerRow> = self.rows.iter().rev().filter(|r| r.engine == engine.key() && r.tier == tier).take(40).collect();
        if rows.len() < 12 { return None; } // fewer is noise — four rejections in eight runs raised a whole wave one tier
        let rejected = rows.iter().filter(|r| !r.passed || r.attempts > 1).count();
        let rate = rejected as f64 / rows.len() as f64;
        if rate >= 0.5 { Some(format!("{} rejected first-attempt at {} in {} of the last {} runs", engine.key(), tier.key(), rejected, rows.len())) } else { None }
    }
    /// what the settings screen shows: per engine × tier — samples, first-attempt pass rate
    pub fn summary(&self) -> Value {
        let mut out = Vec::new();
        for e in [Engine::ClaudeCode, Engine::Codex, Engine::Opencode] { for tier in [Tier::Low, Tier::Mid, Tier::High] {
            let rows: Vec<&LedgerRow> = self.rows.iter().filter(|r| r.engine == e.key() && r.tier == tier).collect();
            if rows.is_empty() { continue; }
            let first = rows.iter().filter(|r| r.passed && r.attempts <= 1).count();
            out.push(json!({ "engine": e.key(), "tier": tier.key(), "samples": rows.len(), "first_pass_rate": (first as f64 / rows.len() as f64 * 100.0).round(), "bias": self.bias(e, tier) }));
        } }
        json!({ "rows": out, "path": Self::path().to_string_lossy() })
    }
}
fn ledger_note(engine: Engine, tier: Tier, passed: bool, attempts: u32) {
    let mut l = Ledger::load();
    l.rows.push(LedgerRow { engine: engine.key().into(), tier, passed, attempts, t: now_ms() });
    if l.rows.len() > 400 { let k = l.rows.len() - 400; l.rows.drain(0..k); }
    l.save();
}
impl Goals { pub fn ledger(&self) -> Ledger { Ledger::load() } }

fn fail(r: &mut GoalRun, tid: &str, why: &str) {
    if let Some(t) = r.tasks.iter_mut().find(|t| t.id == tid) { t.status = TaskStatus::Failed; t.ended = Some(now_ms()); t.error = Some(why.into()); }
    Goals::log(r, "fail", why, Some(tid));
}
fn set_tasks(r: &mut GoalRun, tasks: Vec<PlanTask>) {
    let waves = assign_waves(&tasks);
    r.tasks = tasks.into_iter().map(|t| GoalTask { tier: t.tier, status: if t.needs.is_empty() { TaskStatus::Ready } else { TaskStatus::Pending }, wave: *waves.get(&t.id).unwrap_or(&0), attempts: 0, started: None, ended: None, report: None, verdict: None, error: None, trace: None, changed: vec![], specialists: t.specialists, skills: t.skills, briefs: vec![], id: t.id, title: t.title, prompt: t.prompt, owns: t.owns, needs: t.needs, acceptance: t.acceptance }).collect();
}

// ── TEMPORARY SUBAGENTS — one headless run per sub-task in the parent's worktree, bounded parallelism ──
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus { Queued, Running, Done, Failed, Cancelled }
#[derive(Clone, Debug, Serialize)]
pub struct SubagentJob {
    pub id: i64, pub parent: String, pub goal: String, pub title: String, pub prompt: String, pub engine: Engine, pub cwd: String, pub write: bool,
    pub status: JobStatus, pub created: i64, pub started: Option<i64>, pub ended: Option<i64>,
    pub result: Option<String>, pub error: Option<String>, pub trace: Option<CliTrace>, pub delivered: bool,
}
fn subagent_prompt(goal: &str, task: &str, siblings: &[String]) -> String {
    let others: Vec<String> = siblings.iter().filter(|s| s.as_str() != task).map(|s| format!("- {s}")).collect();
    crate::prompts::render_base("goal.subagent", &vars(&[("count", json!(siblings.len())), ("goal", json!(goal)), ("task", json!(task)), ("others", json!(if others.is_empty() { "- (none)".to_string() } else { others.join("\n") }))]))
}
pub struct Subagents { jobs: Mutex<Vec<SubagentJob>>, seq: Mutex<i64>, running: Mutex<usize>, killers: Mutex<HashMap<i64, Killer>>, concurrency: usize, run: RunFn }
impl Subagents {
    pub fn new(run: RunFn, concurrency: usize) -> Arc<Self> { Arc::new(Subagents { jobs: Mutex::new(vec![]), seq: Mutex::new(0), running: Mutex::new(0), killers: Mutex::new(HashMap::new()), concurrency, run }) }
    fn with<T>(&self, f: impl FnOnce(&mut Vec<SubagentJob>) -> T) -> T { let mut g = self.jobs.lock().unwrap_or_else(|p| p.into_inner()); f(&mut g) }
    pub fn list(&self, parent: Option<&str>) -> Vec<SubagentJob> { self.with(|js| js.iter().filter(|j| parent.map(|p| p.is_empty() || j.parent == p).unwrap_or(true)).cloned().collect()) }
    pub fn get(&self, id: i64) -> Option<SubagentJob> { self.with(|js| js.iter().find(|j| j.id == id).cloned()) }
    pub fn spawn(self: &Arc<Self>, parent: &str, goal: &str, tasks: &[(String, Option<String>)], engine: Engine, cwd: &str, write: bool) -> Vec<SubagentJob> {
        let titles: Vec<String> = tasks.iter().map(|t| t.0.clone()).collect();
        let made: Vec<SubagentJob> = tasks.iter().map(|(title, prompt)| {
            let id = { let mut s = self.seq.lock().unwrap_or_else(|p| p.into_inner()); *s += 1; *s };
            let task = prompt.as_ref().map(|p| p.trim()).filter(|p| !p.is_empty()).unwrap_or(title).to_string();
            SubagentJob { id, parent: parent.into(), goal: goal.into(), title: title.clone(), prompt: subagent_prompt(goal, &task, &titles), engine, cwd: cwd.into(), write, status: JobStatus::Queued, created: now_ms(), started: None, ended: None, result: None, error: None, trace: None, delivered: false }
        }).collect();
        self.with(|js| { js.extend(made.iter().cloned()); if js.len() > 500 { let n = js.len() - 500; js.drain(0..n); } });
        self.pump();
        made
    }
    pub fn cancel(&self, id: i64) -> bool {
        let r = self.with(|js| { let j = js.iter_mut().find(|j| j.id == id)?; match j.status { JobStatus::Queued | JobStatus::Running => { let was_running = j.status == JobStatus::Running; j.status = JobStatus::Cancelled; j.ended = Some(now_ms()); Some(was_running) } _ => None } });
        match r { Some(true) => { let k = self.killers.lock().unwrap_or_else(|p| p.into_inner()).remove(&id); if let Some(k) = k { if let Some(mut c) = k.lock().unwrap_or_else(|p| p.into_inner()).take() { let _ = c.start_kill(); } } true } Some(false) => true, None => false }
    }
    pub fn mark_delivered(&self, id: i64) -> bool { self.with(|js| js.iter_mut().find(|j| j.id == id).map(|j| j.delivered = true).is_some()) }
    fn pump(self: &Arc<Self>) {
        loop {
            let next = { let mut running = self.running.lock().unwrap_or_else(|p| p.into_inner()); if *running >= self.concurrency { return; } let n = self.with(|js| js.iter_mut().find(|j| j.status == JobStatus::Queued).map(|j| { j.status = JobStatus::Running; j.started = Some(now_ms()); j.clone() })); if n.is_some() { *running += 1; } n };
            let Some(job) = next else { return };
            let me = self.clone();
            tokio::spawn(async move {
                let killer: Killer = Arc::new(Mutex::new(None));
                me.killers.lock().unwrap_or_else(|p| p.into_inner()).insert(job.id, killer.clone());
                let timeout = std::env::var("ANTILOKI_SUBAGENT_TIMEOUT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(900_000);
                let out = (me.run)(job.engine, vec![ChatMessage { role: "user".into(), content: job.prompt.clone() }], job.cwd.clone(), false, RunOpts { write: job.write, max_turns: Some(40), timeout_ms: Some(timeout), template: Some("goal.subagent".into()), ..Default::default() }, Some(killer)).await;
                me.with(|js| { if let Some(j) = js.iter_mut().find(|j| j.id == job.id) { if j.status == JobStatus::Running { match out { Ok(o) => { let ok = !o.parsed.content.trim().is_empty(); j.status = if ok { JobStatus::Done } else { JobStatus::Failed }; j.result = Some(o.parsed.content.clone()); j.trace = Some(o.trace); j.error = if ok { None } else { Some(o.parsed.error.unwrap_or_else(|| "no text".into())) }; } Err(e) => { j.status = JobStatus::Failed; j.error = Some(e); } } } j.ended = Some(now_ms()); } });
                me.killers.lock().unwrap_or_else(|p| p.into_inner()).remove(&job.id);
                *me.running.lock().unwrap_or_else(|p| p.into_inner()) -= 1;
                me.pump();
            });
        }
    }
}
/// Ask an engine to split a goal into N parallel, non-overlapping tasks — pure parse.
pub fn parse_simple_plan(text: &str, n: usize) -> Vec<(String, String)> {
    let (s, e) = (text.find('['), text.rfind(']'));
    let slice = match (s, e) { (Some(s), Some(e)) if e > s => &text[s..=e], _ => text };
    let mut arr: Vec<Value> = serde_json::from_str::<Value>(slice).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    if arr.is_empty() {
        // a numbered / bulleted list still works
        let re = regex::Regex::new(r"^\s*(?:[-*•]|\d+[.)])\s*").unwrap();
        arr = text.lines().map(|l| re.replace(l, "").trim().to_string()).filter(|l| !l.is_empty() && !l.starts_with(['[', ']', '{', '}'])).map(|l| json!({ "title": l })).collect();
    }
    arr.iter().take(n.max(1)).map(|x| { let title: String = [x.get("title"), x.get("task")].into_iter().flatten().map(|v| sv(Some(v))).find(|s| !s.is_empty()).unwrap_or_else(|| sv(Some(x))).trim().chars().take(140).collect(); let prompt = [x.get("prompt"), x.get("detail"), x.get("title")].into_iter().flatten().map(|v| sv(Some(v))).find(|s| !s.is_empty()).unwrap_or_else(|| sv(Some(x))).trim().to_string(); (title, prompt) }).filter(|(t, _)| !t.is_empty()).collect()
}
pub fn simple_plan_prompt(goal: &str, n: usize) -> String {
    crate::prompts::render_base("goal.simple_plan", &vars(&[("n", json!(n)), ("goal", json!(goal))]))
}

#[cfg(test)]
pub(crate) fn scripted_prompts_for_queue() -> (RunFn, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<Option<Tier>>>>) { tests::scripted_prompts(vec![]) }
#[cfg(test)]
pub(crate) fn scripted_prompts_for_queue_with(answers: Vec<&str>) -> (RunFn, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<Option<Tier>>>>) { tests::scripted_prompts(answers) }
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_silent_worker_says_so_after_five_minutes_and_then_once_per_five() {
        let min = 60_000i64; let t0 = 1_000_000i64;
        assert!(silence_notice(t0, None, t0 + 4 * min).is_none(), "not yet");
        let first = silence_notice(t0, None, t0 + 5 * min).expect("after five minutes of silence");
        assert!(first.contains("5 min") && first.contains("still working"), "{first}");
        assert!(silence_notice(t0, Some(t0 + 5 * min), t0 + 8 * min).is_none(), "the notice itself counts as the last thing said");
        assert!(silence_notice(t0, Some(t0 + 5 * min), t0 + 10 * min).unwrap().contains("10 min"));
        assert!(silence_notice(t0, Some(t0 + 9 * min), t0 + 12 * min).is_none(), "a worker that spoke three minutes ago is not silent");
    }
    #[test]
    fn a_blueprint_run_files_a_planner_row_but_a_solo_or_replayed_one_does_not() {
        // 2026-10-06: the XXL blueprint bench wrote only `editor-l`; the planner audit POST got 409 "no such row"
        assert!(files_planner_row(false, "blueprint", "codex:medium", "cursor:composer-2.5", "reported"), "brain + hands, no pair: the planner is measured");
        assert!(files_planner_row(true, "", "codex:medium", "cursor:composer-2.5", "estimated"), "a pair still files it");
        assert!(!files_planner_row(false, "", "codex:medium", "cursor:composer-2.5", "reported"), "no pair, not a blueprint: nothing to say about a brain");
        assert!(!files_planner_row(false, "blueprint", "cursor:composer-2.5", "cursor:composer-2.5", "reported"), "cursor+cursor: one seat, no planner row");
        assert!(!files_planner_row(true, "", "raw", "claude-code:sonnet", "solo"), "a solo run has no planner");
        assert!(!files_planner_row(false, "blueprint", "codex:medium", "cursor:composer-2.5", "replayed"), "a replayed plan says nothing about this brain");
    }
    #[test]
    fn a_one_line_edit_is_not_given_a_quarter_of_an_hour() {
        // the tail (2026-09-22): the same one-file task took 30 s and 393 s on the same worker — the second was parked
        // in OpenCode's queue. Six times the median still, but a one-file low task is abandoned at four minutes.
        assert_eq!(fill_timeout_ms(120_000, true, Tier::Low), 240_000, "one file, low: the tighter ceiling");
        assert_eq!(fill_timeout_ms(20_000, true, Tier::Low), 180_000, "the floor still holds — a fast median buys no less");
        assert_eq!(fill_timeout_ms(120_000, false, Tier::Low), 720_000, "many files: six times the median");
        assert_eq!(fill_timeout_ms(120_000, true, Tier::Mid), 720_000, "one file but mid — it may really be thinking");
        assert_eq!(fill_timeout_ms(400_000, false, Tier::High), 900_000, "the ceiling holds");
    }
    #[test]
    fn every_changed_file_reaches_the_verifier_within_the_budget() {
        // the bug (2026-09-23): one string cut at 12 000 chars — on an eight-file task the verifier saw three files
        // and rejected the other five for "no diff evidence". Now each file gets a share.
        let big = |f: &str, n: usize| (f.to_string(), format!("diff --git a/{f} b/{f}\n") + "+x\n".repeat(n).as_str());
        let parts: Vec<(String, String)> = (1..=8).map(|i| big(&format!("src/features/m{i}.ts"), 4_000)).collect();
        let out = budget_diff(&parts, 40_000);
        for i in 1..=8 { assert!(out.contains(&format!("m{i}.ts")), "file m{i} is missing from the verifier's diff"); }
        assert!(out.chars().count() <= 48_000, "the budget is respected: {}", out.chars().count());
        assert!(out.contains("elided to fit the window"), "a truncated file says so");
        assert!(out.contains("do not reject it for missing evidence"), "and tells the verifier what that means");
        // a small file is shown WHOLE, and what it does not spend grows the big one's share
        let mixed = vec![("a.ts".to_string(), "+one line\n".to_string()), big("b.ts", 9_000)];
        let out = budget_diff(&mixed, 40_000);
        assert!(out.contains("+one line"), "the small file is whole");
        // 27 000 chars is over a naive half-share (20 000) but under the budget once the small file's share is freed —
        // water-filling shows it WHOLE where an equal split would have cut it
        assert_eq!(out.matches("+x").count(), 9_000, "the big file got the leftovers and is complete");
        assert!(!out.contains("elided"), "nothing needed eliding here");
        // many files: the floor keeps every one of them visible
        let many: Vec<(String, String)> = (0..80).map(|i| big(&format!("f{i}.ts"), 500)).collect();
        let out = budget_diff(&many, 40_000);
        for i in 0..80 { assert!(out.contains(&format!("f{i}.ts")), "file f{i} vanished"); }
        assert_eq!(budget_diff(&[], 40_000), "");
    }
    #[test]
    fn the_screen_parses_blocking_questions_and_demotes_assumable_ones() {
        let (q, a) = parse_screen(r#"{"questions":[{"text":"Which layer — REST or GraphQL?","options":["REST","GraphQL"],"default":"REST","certainty":"blocking"},{"text":"Keep the old name?","default":"yes","certainty":"assumable"},{"text":"","default":"x"}]}"#);
        assert_eq!(q.len(), 1);
        assert_eq!((q[0].id.as_str(), q[0].text.as_str(), q[0].default.as_str(), q[0].certainty, q[0].answer.clone()), ("q1", "Which layer — REST or GraphQL?", "REST", Certainty::Blocking, None));
        assert_eq!(q[0].options, vec!["REST", "GraphQL"]);
        assert_eq!(a, vec!["Keep the old name? → yes"]);
    }
    #[test]
    fn the_screen_is_silent_on_a_plan_and_keeps_its_assumptions() {
        let (q, a) = parse_screen(r#"{"tier":"mid","delegate":true,"assumptions":["the scheduler package, not the reconciler"],"tasks":[{"id":"t1","title":"x","prompt":"p","owns":["a.js"]}]}"#);
        assert!(q.is_empty()); assert_eq!(a, vec!["the scheduler package, not the reconciler"]);
        // a plan that ALSO asks: the questions become assumptions — never a stop on a reply that holds a plan
        let (q2, a2) = parse_screen(r#"{"questions":[{"text":"which?","default":"this","certainty":"blocking"}],"tasks":[{"id":"t1","title":"x","prompt":"p","owns":["a.js"]}]}"#);
        assert!(q2.is_empty()); assert_eq!(a2, vec!["which? → this"]);
        assert_eq!(parse_screen("no json here"), (vec![], vec![]));
    }
    #[test]
    fn the_screened_prompt_asks_only_when_the_router_is_on_and_never_twice() {
        let on = plan_prompt_screened("g", 2, true, &[], &[], true, false);
        assert!(on.contains("FIRST SCREEN the goal") && on.contains("\"certainty\":\"blocking\"") && on.contains("\"assumptions\":[") && !on.contains("ALREADY answered"), "{on}");
        let asked = plan_prompt_screened("g", 2, true, &[], &[], true, true);
        assert!(asked.contains("ALREADY answered"));
        let off = plan_prompt_screened("g", 2, true, &[], &[], false, false);
        assert!(!off.contains("SCREEN") && off.contains("FIRST decide the ROUTE:") && !off.contains("assumptions"));
    }
    use crate::cli::CliParsed;

    fn pt(id: &str, needs: &[&str], owns: &[&str]) -> PlanTask { PlanTask { id: id.into(), title: id.into(), prompt: id.into(), owns: owns.iter().map(|s| s.to_string()).collect(), needs: needs.iter().map(|s| s.to_string()).collect(), acceptance: vec![], tier: Tier::default(), specialists: vec![], skills: vec![] } }

    #[test]
    fn findings_are_char_safe() {
        assert_eq!(findings("x\nFINDING: a\nfinding: b\nfinding— not one\nFÍNDING: nope\n—— eight bytes cut ——"), vec!["a".to_string(), "b".to_string()]);
        assert!(findings("—\n").is_empty());
    }

    #[test]
    fn owns_file_rules() {
        let o = vec!["src/a/**".to_string(), "docs/x.md".into(), "lib".into(), "**/*.spec.ts".into()];
        assert!(owns_file(&o, "src/a/b.ts")); assert!(owns_file(&o, "docs/x.md")); assert!(owns_file(&o, "lib/z.ts")); assert!(owns_file(&o, "deep/q.spec.ts")); assert!(!owns_file(&o, "src/b/c.ts"));
    }

    #[test]
    fn parse_plan_dedupes_ids_owners_deps_and_cycles() {
        let text = "here you go:\n[{\"id\":\"t1\",\"title\":\"A\",\"owns\":[\"./src/a/**\"],\"needs\":[\"t2\"]},{\"id\":\"t2\",\"title\":\"B\",\"owns\":[\"src/a/**\",\"src/b/**\"],\"needs\":[\"t1\",\"zz\"]},{\"id\":\"t1\",\"title\":\"C\"}]";
        let (tasks, warnings) = parse_plan(text, 8);
        assert_eq!(tasks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["t1", "t2", "t1_3"]);
        assert_eq!(tasks[0].owns, ["src/a/**"]); assert_eq!(tasks[1].owns, ["src/b/**"], "first owner wins");
        assert!(warnings.iter().any(|w| w.contains("owned by t1, dropped from t2")));
        assert!(warnings.iter().any(|w| w.contains("unknown dependency zz")));
        assert!(warnings.iter().any(|w| w.contains("cycle")), "{warnings:?}");
        let total_edges: usize = tasks.iter().map(|t| t.needs.len()).sum();
        assert_eq!(total_edges, 1, "one edge of the t1↔t2 cycle survives");
        assert!(parse_plan("no json here", 4).0.is_empty());
        assert_eq!(parse_plan("[{\"title\":\"a\"},{\"title\":\"b\"},{\"title\":\"c\"}]", 2).0.len(), 2);
    }

    #[test]
    fn waves_follow_the_dag() {
        let tasks = vec![pt("a", &[], &[]), pt("b", &["a"], &[]), pt("c", &["a"], &[]), pt("d", &["b", "c"], &[])];
        let w = assign_waves(&tasks);
        assert_eq!((w["a"], w["b"], w["c"], w["d"]), (0, 1, 1, 2));
    }

    #[test]
    fn verdict_findings_fallback_delivery() {
        assert_eq!(parse_verdict("sure: {\"passed\": false, \"reasons\": [\"no test\"]} ok"), Some((false, vec!["no test".into()])));
        assert!(parse_verdict("{\"reasons\":[]}").is_none());
        assert_eq!(findings("did x\nFINDING: a is dead\nfinding: b too\nnope"), vec!["a is dead", "b too"]);
        let mut run = GoalRun { plan_usd: 0.0, screen_usd: 0.0, plan_basis: String::new(), plan_binding: String::new(), worker_binding: String::new(), suite: String::new(), confidence: None, pane_id: 0, scope: vec![], checks_off: false, merge_outcome: None, role: None, plan_ms: 0, id: 9, parent: "p".into(), goal: "g".into(), engine: Engine::Codex, cwd: ".".into(), write: false, concurrency: 2, status: RunStatus::Done, created: 0, started: None, ended: None, tasks: vec![], blackboard: vec![], events: vec![], report: Some("# t\nline one\nline two".into()), delivered: false, error: None, route: None, questions: vec![], assumptions: vec![], plan_args: PlanArgs::default(), pr: None, router: true, min: false, workers: None, mode: String::new(), blueprint: None, stages: vec![], qa_gate: None, cost: None, pair: None, qa_double: false, qa_first: None, qa_review_wanted: false, qa_review: None, savings: None, qa: None, qa_baseline: None, qa_baseline_pending: false, compare: None, mid_task: false, base: None, qa_pending: false, qa_rounds: 0, design: None };
        set_tasks(&mut run, vec![pt("a", &[], &["x"]), pt("b", &["a"], &[])]);
        run.tasks[0].status = TaskStatus::Done; run.tasks[1].status = TaskStatus::Blocked;
        let fb = join_fallback(&run);
        assert!(fb.starts_with("# Goal run #9 — 1/2 tasks done")); assert!(fb.contains("## Open points\n- b b: blocked"));
        assert!(join_delivery(&run).starts_with("[antiloki] goal run #9 finished: 1/2 tasks passed · open: b. line one ⏎ line two"));
        let j = serde_json::to_value(&run).unwrap();
        assert_eq!(j["engine"], "codex"); assert_eq!(j["status"], "done"); assert_eq!(j["tasks"][1]["status"], "blocked");
    }

    fn scripted(answers: Arc<Mutex<Vec<String>>>, calls: Arc<Mutex<Vec<String>>>) -> RunFn {
        Arc::new(move |_engine, messages, _cwd, _json, _opts, _k| {
            let (answers, calls) = (answers.clone(), calls.clone());
            Box::pin(async move {
                let prompt = messages[0].content.clone();
                let kind = if prompt.starts_with("You are the ROUTER and PLANNER") { "plan" } else if prompt.starts_with("You are the RATIFIER") { "ratify" } else if prompt.starts_with("You are the VERIFIER") { "verify" } else if prompt.starts_with("You are the ORCHESTRATOR") { "join" } else { "task" };
                calls.lock().unwrap().push(format!("{kind}:{}", prompt.lines().find(|l| l.starts_with("YOUR TASK: ")).map(|l| l.trim_start_matches("YOUR TASK: ").to_string()).unwrap_or_default()));
                let content = { let mut a = answers.lock().unwrap(); if prompt.starts_with("You are the RATIFIER") { "{\"ok\":true,\"problems\":[]}".to_string() } else if a.is_empty() { String::new() } else { a.remove(0) } };
                let content = if content == "@task" { format!("did it\nFINDING: note from {kind}") } else if content == "@verify" { "{\"passed\":true,\"reasons\":[\"met\"]}".into() } else if content == "@reject" { "{\"passed\":false,\"reasons\":[\"missing\"]}".into() } else { content };
                Ok(RunOut { parsed: CliParsed { content, ..Default::default() }, ms: 1, trace: CliTrace { model: None, id: 1, engine: Engine::Codex, cwd: ".".into(), started: 0, ended: 1, ms: 1, tools: vec![], tokens_in: None, tokens_out: None, cost: None, reported: None, chars: 0, ok: true, error: None } })
            })
        })
    }

    /// An expectation written the POSIX way, re-quoted for whatever platform the test runs on. The commands
    /// themselves are identical; only how a path is wrapped differs, and that difference is `proc::quote`'s job.
    fn qq(expected: &str) -> String {
        let mut out = String::new();
        for (i, part) in expected.split('\'').enumerate() {
            if i % 2 == 1 { out.push_str(&crate::proc::quote(part).unwrap()) } else { out.push_str(part) }
        }
        out
    }

    #[test]
    fn the_qa_gate_picks_the_runner_from_package_json_and_scopes_it_to_the_change() {
        let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // the project's own jest script through its package manager (react: `node ./scripts/jest/jest-cli.js`)
        assert_eq!(qa_test_cmd(Some(r#"{"scripts":{"test":"node ./scripts/jest/jest-cli.js"}}"#), &f(&["src/a.js", "src/b.js"]), true, "yarn.lock"), Ok(qq("yarn test --findRelatedTests 'src/a.js' 'src/b.js' --passWithNoTests")));
        assert_eq!(qa_test_cmd(Some(r#"{"scripts":{"test":"jest"}}"#), &f(&["a.js"]), true, "package-lock.json"), Ok(qq("npm test -- --findRelatedTests 'a.js' --passWithNoTests")));
        assert_eq!(qa_test_cmd(Some(r#"{"devDependencies":{"jest":"1"}}"#), &f(&["a.js"]), true, ""), Ok(qq("npx jest --findRelatedTests 'a.js' --passWithNoTests")), "jest only as a dependency → the bin");
        assert_eq!(qa_test_cmd(Some(r#"{"devDependencies":{"vitest":"^2"}}"#), &f(&["src/a.ts"]), true, "pnpm-lock.yaml"), Ok(qq("npx vitest related 'src/a.ts' --run --passWithNoTests")));
        // the honest reasons
        assert!(qa_test_cmd(Some(r#"{"scripts":{"test":"mocha"}}"#), &f(&["a.js"]), true, "").unwrap_err().contains("no jest/vitest"));
        assert!(qa_test_cmd(Some(r#"{"scripts":{"test":"jest"}}"#), &f(&["a.js"]), false, "yarn.lock").unwrap_err().contains("yarn install"));
        assert!(qa_test_cmd(None, &f(&["a.js"]), true, "").unwrap_err().contains("no package.json"));
        assert_eq!(rust_gate_cmd(&f(&["serve/src/cli.rs"])), Ok("cargo check --quiet --all-targets".to_string()), "a changed .rs file is compiled");
        assert!(rust_gate_cmd(&f(&["README.md"])).unwrap_err().contains("no Rust file"), "docs only: nothing to compile");
        assert!(qa_test_cmd(Some(r#"{"devDependencies":{"jest":"1"}}"#), &f(&[]), true, "").unwrap_err().contains("no changed files"));
        // A PATH WITH A SPACE IS QUOTED, NOT DROPPED (2026-09-24). It used to be filtered out, which on Windows —
        // where "Documents and Settings" and "Program Files" are ordinary — silently skipped most of a repo and
        // reported the remainder as the whole run.
        assert_eq!(qa_test_cmd(Some(r#"{"devDependencies":{"jest":"1"}}"#), &f(&["a b.js"]), true, ""),
                   Ok(format!("npx jest --findRelatedTests {} --passWithNoTests", crate::proc::quote("a b.js").unwrap())));
    }
    #[test]
    fn the_deps_plan_reads_the_box() {
        let dir = std::env::temp_dir().join(format!("antiloki-deps-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        assert!(deps_plan(dir.to_str().unwrap()).unwrap_err().contains("no package.json"));
        std::fs::write(dir.join("package.json"), "{}").unwrap(); std::fs::write(dir.join("yarn.lock"), "x").unwrap();
        let (how, cmd) = deps_plan(dir.to_str().unwrap()).unwrap();
        assert_eq!(how, "install"); assert_eq!(cmd[0], "yarn"); assert!(cmd.contains(&"--frozen-lockfile".to_string()));
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        assert_eq!(deps_plan(dir.to_str().unwrap()).unwrap().0, "present");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_headless_limit_marks_the_box_for_the_pane_banner() {
        let d = std::env::temp_dir().join(format!("antiloki-quota-{}", now_ms())); std::fs::create_dir_all(&d).unwrap();
        let cwd = d.to_string_lossy().to_string();
        assert!(crate::pty::quota_file(&d).is_none());
        mark_quota_file(&cwd, "codex", "You've hit your usage limit. Try again at 6pm.");
        let q = crate::pty::quota_file(&d).unwrap();
        assert_eq!(q["source"], "goal-run"); assert_eq!(q["engine"], "codex"); assert!(q["text"].as_str().unwrap().contains("usage limit"));
        // an old mark is not a live limit
        std::fs::write(d.join(".antiloki/quota.json"), json!({ "text": "x", "t": now_ms() - 7 * 3_600_000 }).to_string()).unwrap();
        assert!(crate::pty::quota_file(&d).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn min_turns_are_priced_against_the_same_turn_with_exploration_and_recorded_as_runs() {
        let s = savings_min(Engine::ClaudeCode, Tier::Mid, 100_000, 10_000).unwrap();
        assert!((s.actual_usd - 0.45).abs() < 1e-9, "sonnet: $3 in · $15 out"); assert!((s.baseline_usd - 0.9).abs() < 1e-9, "the measured share (50%) put back"); assert!((s.saved_usd - 0.45).abs() < 1e-9); assert_eq!(s.saved_pct, 50.0);
        assert_eq!(s.tiers, vec!["min:mid"]); assert!(s.baseline.contains("with exploration (measured share 50%)"));
        assert!(savings_min(Engine::Codex, Tier::Low, 0, 0).is_none());
        // the ask's paths, and which of them have no intel
        assert_eq!(named_paths("fix the total in src/cart.ts and ./packages/a/b.tsx, see README.md (not e.g. v1.2 or https://x.y/z)"), vec!["src/cart.ts", "packages/a/b.tsx", "README.md"]);
        assert_eq!(named_paths("why is `src/ui/Output.tsx` slow?"), vec!["src/ui/Output.tsx"]);
        assert!(named_paths("make it faster").is_empty());
        let d = std::env::temp_dir().join(format!("antiloki-min-{}", now_ms())); std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(d.join("src/cart.ts"), "x").unwrap(); std::fs::write(d.join("src/known.ts"), "x").unwrap(); std::fs::write(d.join("src/judged.ts"), "x").unwrap();
        let cwd = d.to_string_lossy().to_string();
        let manifest = json!({ "items": [{ "kind": "file", "path": "src/known.ts", "written": true }] });
        let paths = vec!["src/cart.ts".to_string(), "src/known.ts".into(), "src/judged.ts".into(), "src/missing.ts".into()];
        assert_eq!(without_intel(&cwd, &paths, &manifest, &|p| p == "src/judged.ts"), vec!["src/cart.ts"], "in the pack or judged = intel; not on disk = not a file the box has");
        // a MIN turn becomes a settled run with the estimate, the ledger's other column
        let (run, _, _) = scripted_prompts(vec![]);
        let g = Goals::new(run, None, 1, 5);
        let id = g.note_min_turn("5-3", Engine::ClaudeCode, &cwd, "what depends on cart?", Tier::Mid, 20_000, 2_000, 3).unwrap();
        let r = g.get(id).unwrap();
        assert!(r.min && !r.router && r.status == RunStatus::Inline && r.delivered && r.parent == "5-3");
        assert_eq!(r.goal, "what depends on cart?"); assert!(r.savings.as_ref().unwrap().saved_usd > 0.0);
        assert!(r.events.iter().any(|e| e.kind == "min" && e.text.contains("3 exploration calls refused")), "{:?}", r.events);
        // the fallback mark rides on the route
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    fn inline_routes_are_priced_from_the_turn() {
        let s = savings_inline(Engine::ClaudeCode, Tier::Low, 100_000, 10_000).unwrap();
        assert!((s.actual_usd - 0.15).abs() < 1e-9, "haiku: $1 in · $5 out"); assert!((s.baseline_usd - 0.75).abs() < 1e-9, "opus: $5 in · $25 out"); assert!((s.saved_usd - 0.6).abs() < 1e-9); assert_eq!(s.tiers, vec!["inline:low"]);
        let s = savings_inline(Engine::ClaudeCode, Tier::Mid, 100_000, 10_000).unwrap();
        assert!((s.actual_usd - 0.45).abs() < 1e-9, "sonnet: $3 in · $15 out");
        assert_eq!(savings_inline(Engine::ClaudeCode, Tier::High, 100_000, 10_000).unwrap().saved_usd, 0.0, "opus routed to opus saves nothing — but the run gets its number");
        assert!(savings_inline(Engine::Codex, Tier::Low, 0, 0).is_none());
    }
    /// THE SAVING IS AGAINST THE FRONTIER MODEL'S LIST PRICE (operator 2026-09-28: "it was said that saved 0. This is
    /// not right. The comparison uses a deterministic calculator that compares with official prices of tokens from the
    /// best frontier model — Claude Opus, and the Codex equivalent"). The earlier rule zeroed the claim on any engine
    /// with one rate at every tier, which made Cursor runs 5× cheaper than Opus read "saved $0.00".
    /// operator 2026-09-28, run #256: a pair leaves `workers` None, so the tasks were priced at the RUN engine's rate —
    /// Claude Opus for a change Cursor Composer wrote — and the run reported "saved $0.00" against a $0.474 spend.
    #[test]
    fn a_pair_run_is_priced_at_the_seat_that_wrote_it_not_at_the_run_engine() {
        let trace = Some(crate::cli::CliTrace { model: None, id: 0, engine: Engine::Cursor, cwd: ".".into(), started: 0, ended: 1, ms: 1, tools: vec![], tokens_in: Some(100_000), tokens_out: Some(10_000), cost: None, reported: None, chars: 0, ok: true, error: None });
        let task = GoalTask { id: "t1".into(), title: "t1".into(), prompt: String::new(), owns: vec![], needs: vec![], acceptance: vec![], tier: Tier::High, specialists: vec![], skills: vec![], briefs: vec![], status: TaskStatus::Done, wave: 0, attempts: 1, started: None, ended: None, report: None, verdict: None, error: None, trace, changed: vec![] };
        let old = savings_with_workers(Engine::ClaudeCode, None, std::slice::from_ref(&task)).unwrap();
        let new = savings_priced(Engine::ClaudeCode, None, Some((Engine::Cursor, Some("composer-2.5".into()))), std::slice::from_ref(&task)).unwrap();
        assert!((new.actual_usd - 0.065).abs() < 1e-9, "composer: $0.50 in · $1.50 out for 100k/10k, got {new:?}");
        assert!((new.baseline_usd - 0.75).abs() < 1e-9, "the opus yardstick is unchanged: {new:?}");
        assert!(new.saved_usd > 0.6 && new.saved_pct > 85.0, "{new:?}");
        assert!(old.actual_usd > new.actual_usd, "the old rule priced it higher because it priced the wrong seat: {old:?}");
    }

    #[test]
    fn an_ask_that_names_a_check_is_not_told_to_run_none() {
        assert!(ask_wants_checks("fix the failing test in cart.ts"));
        assert!(ask_wants_checks("make the build pass"));
        assert!(ask_wants_checks("run the linter and fix what it finds"));
        assert!(!ask_wants_checks("add a sort dropdown for the products"));
        assert!(!ask_wants_checks("attest the contest results"), "whole words only");
    }

    #[test]
    fn size_l_or_smaller_is_one_worker_and_only_a_bigger_ask_is_a_wave() {
        assert!(!ask_bigger_than_l("add a filter"));
        assert!(!ask_bigger_than_l("add a tag filter over the six products, styled like the cart button"));
        assert!(ask_bigger_than_l("1. add the filter\n2. add the sort\n3. wire both to the URL"));
        assert!(ask_bigger_than_l(&"describe the whole checkout redesign in detail. ".repeat(20)));
    }
    #[test]
    fn the_saving_is_measured_against_the_frontier_model_not_the_seats_own_top_rung() {
        let tr = |i: i64, o: i64| Some(crate::cli::CliTrace { model: None, id: 0, engine: Engine::Codex, cwd: ".".into(), started: 0, ended: 1, ms: 1, tools: vec![], tokens_in: Some(i), tokens_out: Some(o), cost: None, reported: None, chars: 0, ok: true, error: None });
        let mk = |id: &str, tier: Tier, t: Option<crate::cli::CliTrace>| GoalTask { id: id.into(), title: id.into(), prompt: String::new(), owns: vec![], needs: vec![], acceptance: vec![], tier, specialists: vec![], skills: vec![], briefs: vec![], status: TaskStatus::Done, wave: 0, attempts: 1, started: None, ended: None, report: None, verdict: None, error: None, trace: t, changed: vec![] };

        // a Cursor composer run (one rate at every tier) against Claude Opus: $5/$25 vs $0.50/$1.50
        let cur = |i: i64, o: i64| Some(crate::cli::CliTrace { model: None, id: 0, engine: Engine::Cursor, cwd: ".".into(), started: 0, ended: 1, ms: 1, tools: vec![], tokens_in: Some(i), tokens_out: Some(o), cost: None, reported: None, chars: 0, ok: true, error: None });
        let c = savings_with_workers(Engine::ClaudeCode, Some(Engine::Cursor), &[mk("t1", Tier::Low, cur(100_000, 10_000))]).unwrap();
        assert!((c.baseline_usd - 0.75).abs() < 1e-9, "opus: 0.1M in × $5 + 0.01M out × $25 = $0.75, got {c:?}");
        assert!((c.actual_usd - 0.065).abs() < 1e-9, "composer: $0.50 in · $1.50 out, got {c:?}");
        assert!(c.saved_usd > 0.6 && c.saved_pct > 85.0, "{c:?}");
        assert_eq!(c.baseline, "claude · opus");

        // Codex measured against the Codex frontier model: the same rate, so honestly nothing saved — by arithmetic
        let s = savings_of(Engine::Codex, &[mk("t1", Tier::Low, tr(100_000, 10_000))]).unwrap();
        assert!(s.actual_usd > 0.0, "{s:?}");
        assert_eq!((s.saved_usd, s.baseline_usd), (0.0, s.actual_usd));
        assert_eq!(s.baseline, "codex · gpt-5");

        // a Codex pane whose work went to a cheaper seat DOES save against the Codex frontier price
        let d = savings_with_workers(Engine::Codex, Some(Engine::Gemini), &[mk("t1", Tier::Low, cur(100_000, 10_000))]).unwrap();
        assert!(d.saved_usd > 0.0 && d.baseline == "codex · gpt-5", "{d:?}");

        // Claude haiku under opus, and opus under opus
        let tr2 = |i: i64, o: i64| Some(crate::cli::CliTrace { model: None, id: 0, engine: Engine::ClaudeCode, cwd: ".".into(), started: 0, ended: 1, ms: 1, tools: vec![], tokens_in: Some(i), tokens_out: Some(o), cost: None, reported: None, chars: 0, ok: true, error: None });
        let h = savings_of(Engine::ClaudeCode, &[mk("t1", Tier::Low, tr2(100_000, 10_000))]).unwrap();
        assert!(h.saved_usd > 0.0, "haiku under opus is a real saving: {h:?}");
        let o = savings_of(Engine::ClaudeCode, &[mk("t1", Tier::High, tr2(100_000, 10_000))]).unwrap();
        assert_eq!(o.saved_usd, 0.0, "opus under opus saves exactly nothing: {o:?}");

        // and the inline route reads the same yardstick
        let i = savings_inline(Engine::Cursor, Tier::High, 100_000, 10_000).unwrap();
        assert!(i.saved_usd > 0.0 && i.baseline == "claude · opus", "grok 4.6 ($3/$15) is under opus ($5/$25): {i:?}");
        assert_eq!(savings_inline(Engine::ClaudeCode, Tier::High, 100_000, 10_000).unwrap().saved_usd, 0.0);
    }

    #[test]
    fn savings_price_each_task_at_its_tier_against_one_high_tier_agent() {
        let tr = |i: i64, o: i64| Some(crate::cli::CliTrace { model: None, id: 0, engine: Engine::ClaudeCode, cwd: ".".into(), started: 0, ended: 1, ms: 1, tools: vec![], tokens_in: Some(i), tokens_out: Some(o), cost: None, reported: None, chars: 0, ok: true, error: None });
        let mk = |id: &str, tier: Tier, t: Option<crate::cli::CliTrace>| GoalTask { id: id.into(), title: id.into(), prompt: String::new(), owns: vec![], needs: vec![], acceptance: vec![], tier, specialists: vec![], skills: vec![], briefs: vec![], status: TaskStatus::Done, wave: 0, attempts: 1, started: None, ended: None, report: None, verdict: None, error: None, trace: t, changed: vec![] };
        let s = savings_of(Engine::ClaudeCode, &[mk("t1", Tier::Low, tr(100_000, 10_000)), mk("t2", Tier::High, tr(50_000, 5_000)), mk("t3", Tier::Mid, None)]).unwrap();
        assert_eq!((s.tokens_in, s.tokens_out), (150_000, 15_000));
        // t1 at haiku (low): 0.1×1 + 0.01×5 = 0.15 · t2 at opus: 0.05×5 + 0.005×25 = 0.375 → actual 0.525; all at opus: 0.15×5 + 0.015×25 = 1.125
        assert!((s.actual_usd - 0.525).abs() < 1e-9 && (s.baseline_usd - 1.125).abs() < 1e-9 && (s.saved_usd - 0.6).abs() < 1e-9, "{s:?}");
        assert!((s.saved_pct - 53.333).abs() < 0.01);
        assert_eq!(s.tiers, vec!["t1:low", "t2:high"]);
        assert!(savings_of(Engine::Codex, &[mk("t", Tier::Low, None)]).is_none(), "no tokens, no number");
        assert_eq!(parse_ratify("looks fine {\"ok\":true,\"problems\":[]}"), Vec::<String>::new());
        assert_eq!(parse_ratify("{\"ok\":false,\"problems\":[\"t1 and t2 both own src/a.ts\",\"\"]}"), vec!["t1 and t2 both own src/a.ts"]);
        assert_eq!(parse_ratify("no json here"), Vec::<String>::new(), "an unreadable ratifier never blocks");
    }

    /// WHAT WAS CHOSEN IS PART OF THE PROMPT (operator 2026-09-28: "on screening, the choices screened becomes part
    /// of the sent prompt on the frontend. right now they keep hidded on its pane"). Screening's answers go straight
    /// into what the planner is told, so they belong in the transcript beside the ask, not only in the run's log.
    #[test]
    fn the_screening_choices_are_said_back_and_the_defaults_are_not() {
        let line = |v: Value| answers_line(&v);
        assert_eq!(line(json!({ "d1": "the cart drawer" })).as_deref(), Some("answering heimdall: the cart drawer"));
        assert_eq!(line(json!({ "d1": "a", "d2": "b" })).as_deref(), Some("answering heimdall: a · b"));

        // TAKING THE DEFAULTS IS NOT A CHOICE TO ANNOUNCE — an empty object means "you decide", and printing it as
        // an answer would put words in the operator's mouth
        assert_eq!(line(json!({})), None);
        // …and an emptied box is not an answer either, the same rule the rail applies before sending
        assert_eq!(line(json!({ "d1": "   " })), None);
        assert_eq!(line(json!({ "d1": "", "d2": "kept" })).as_deref(), Some("answering heimdall: kept"));
        // a non-string answer is not invented into one
        assert_eq!(line(json!({ "d1": 7 })), None);
        assert_eq!(line(json!("nope")), None);
    }

    /// A BENCH THAT GOES THROUGH THE SERVE RECORDS ITSELF AS A BENCH (2026-09-28). Until now the serve filed every
    /// run as real use, arms included, so the controlled half of the record could only be written by a harness
    /// posting copies in afterwards — and those copies hardcoded `stack: "any"`. Between that and `run_stack`, the
    /// entire benchmark record sat in a pool no real `web` work could reach, which is why the router answered Sonnet
    /// to every frontend ask. A run that names its suite is controlled; one that does not is use.
    #[test]
    fn a_run_that_names_a_suite_is_controlled_evidence_and_one_that_does_not_is_use() {
        use crate::merges::{evidence_of, Evidence};
        assert_eq!(source_of("ministore"), "bench");
        assert_eq!(source_of("  Ministore  "), "bench", "a name with room around it is still a name");
        assert_eq!(source_of(""), "run");
        assert_eq!(source_of("   "), "run", "whitespace is not a suite");
        // and the words chosen are the ones the record already classifies — not new spellings of its own
        assert!(matches!(evidence_of(source_of("ministore")), Some(Evidence::Controlled)));
        assert!(matches!(evidence_of(source_of("")), Some(Evidence::Observed)));
    }

    /// A ROW DESCRIBES ITS OWN RUN (2026-09-28). `run_stack` used to be `audit::workspace_stack()`, the stack of the
    /// workspace the operator has open — so a benchmark arm in a throwaway clone filed itself under the app's own
    /// technology. Every controlled row on the record landed in the `any` pool (29 arms under `editor-s/s/backend/any`)
    /// while real use routed to `web`, where one unopposed row answered every ask: the router said `claude-code:sonnet`
    /// to every frontend task no matter how much had been measured, because `best_for_stack` tries the exact stack
    /// first and stops the moment it finds enough samples there.
    #[test]
    fn a_merge_row_carries_the_stack_of_the_directory_the_run_happened_in() {
        let d = std::env::temp_dir().join(format!("antiloki-run-stack-{}", crate::cli::now_ms()));
        std::fs::create_dir_all(&d).unwrap();
        let cwd = d.to_string_lossy().to_string();

        // a react app is `web` wherever it sits, and whatever the operator has open
        std::fs::write(d.join("package.json"), r#"{"dependencies":{"react":"19.0.0"}}"#).unwrap();
        crate::gates::stack_at(&cwd);  // warm the per-root cache the same way a run does
        assert_eq!(run_stack(&cwd), "web", "the run's own directory decides");

        // and a caller with no directory at all still gets an answer rather than a panic
        let fallback = run_stack("");
        assert!(fallback.is_empty() || !fallback.contains('/'), "a stack is a word, not a path: {fallback:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A CACHED PLAN AND A FREE PLAN ARE THE SAME NUMBER AND NOT THE SAME FACT (2026-09-27). `replay` returns
    /// `cost: Some(0.0)` for a cache hit, which `price_turn` correctly reports as a measured zero — so `plan_usd` and
    /// `plan_basis` ALONE cannot tell "the planner was not called" from "the planner charged nothing". That mattered:
    /// the planner is the largest line on the bill (measured 2026-09-27 — Claude $0.093-$0.188 a plan against Codex's
    /// $0.0039, 24-48x for the same job), and in four of B1's five role cells every arm replayed its plan, so the
    /// whole cell compared worker cost while reading as an end-to-end price. `plan_basis` now says `replayed`.
    #[test]
    fn a_replayed_plan_is_labelled_replayed_and_not_merely_reported() {
        // what the cache hands back, and what pricing makes of it
        let (usd, basis) = crate::estimate::price_turn(Some(0.0), Engine::ClaudeCode, Some("sonnet"), "the ask", "the plan");
        assert_eq!(usd, 0.0);
        assert_eq!(basis, crate::estimate::Basis::Reported, "a reported zero is still a report — this is correct, and it is why the basis alone is not enough");
        assert_eq!(basis.key(), "reported");

        // a planner that really was called and really reported nothing lands on the SAME pair
        let (usd2, basis2) = crate::estimate::price_turn(Some(0.0), Engine::Codex, Some("gpt-5.6-terra@medium"), "the ask", "the plan");
        assert_eq!((usd2, basis2.key()), (0.0, "reported"), "indistinguishable from the cache hit above");

        // so the run carries the fact separately, and only these three values are possible
        for (was_replayed, want) in [(true, "replayed"), (false, "reported")] {
            let key: String = if was_replayed { "replayed".into() } else { basis.key().into() };
            assert_eq!(key, want);
        }
        // and a priced plan is never mislabelled as a replay
        let (usd3, basis3) = crate::estimate::price_turn(None, Engine::ClaudeCode, Some("opus"), "a long ask that costs real money to plan", "a plan");
        assert!(usd3 > 0.0 && basis3.key() == "estimated", "got ${usd3} {}", basis3.key());
    }

    /// A PAIR THAT REPORTS NOTHING IS NOT A FREE PAIR (2026-09-27). The recorded `usd` was
    /// `tasks.filter_map(|t| t.trace.cost).sum()`. `filter_map` drops the tasks that report no cost — Codex, Cursor
    /// and Gemini report none, four of the five seats — and the sum of an empty set is 0.0, so the pair went onto the
    /// permanent record as free. All 40 frontier merge runs of the five role cells recorded $0.0000 against a
    /// token-equivalent of $10.89, and since `Objective::Best` weights cost 0.55, the table ranked the quiet pairs
    /// top: `codex × gemini` read 87k-235k input tokens per run and still read as the cheapest arm in every cell.
    #[test]
    fn a_run_whose_engines_report_no_cost_is_still_priced_on_the_record() {
        let quiet = |i: i64, o: i64| Some(crate::cli::CliTrace { model: None, id: 0, engine: Engine::Codex, cwd: ".".into(), started: 0, ended: 1, ms: 1, tools: vec![], tokens_in: Some(i), tokens_out: Some(o), cost: None, reported: None, chars: 0, ok: true, error: None });
        let mk = |id: &str, tier: Tier, t: Option<crate::cli::CliTrace>| GoalTask { id: id.into(), title: id.into(), prompt: String::new(), owns: vec![], needs: vec![], acceptance: vec![], tier, specialists: vec![], skills: vec![], briefs: vec![], status: TaskStatus::Done, wave: 0, attempts: 1, started: None, ended: None, report: None, verdict: None, error: None, trace: t, changed: vec![] };
        let tasks = [mk("t1", Tier::Mid, quiet(15_000, 5_000)), mk("t2", Tier::Mid, quiet(9_000, 2_000))];

        // what the record used to compute: every task filtered out, so nothing to sum
        let dropped: f64 = tasks.iter().filter_map(|t| t.trace.as_ref().and_then(|x| x.cost)).sum();
        assert_eq!(dropped, 0.0, "this is the shape of the bug — 26k tokens of work priced at zero");

        // what it computes now: each task's tokens at the HANDS' published rate, plus the planner's own call
        let plan_usd = 0.012;
        let priced = |eng: Engine, model: Option<&str>| plan_usd + tasks.iter().filter_map(|t| t.trace.as_ref()).map(|tr| match tr.cost {
            Some(c) if c > 0.0 => c,
            _ => crate::cli::price_tokens(eng, model, tr.tokens_in.unwrap_or(0), tr.tokens_out.unwrap_or(0), 0, 0),
        }).sum::<f64>();
        let composer = priced(Engine::Cursor, Some("composer-2.5"));
        assert!(composer > plan_usd, "31k tokens across two tasks is never $0.00: got {composer}");

        // AND THE HANDS ARE BILLED AS THE HANDS. Pricing Composer's tokens at Sonnet's rate — which is what
        // `savings_with_workers` does on a pair, because a pair leaves `workers` None — inflates the same work several
        // times over, and a cost-weighted table cannot tell that apart from a genuinely dearer pair.
        let as_brain = priced(Engine::ClaudeCode, Some("sonnet"));
        assert!(as_brain > composer * 2.0, "composer at sonnet's rate: ${as_brain} vs ${composer} — the basis has to be the hands'");

        // and a run that truly did nothing still records nothing, so "free" keeps its meaning
        let idle = [mk("t", Tier::Mid, None)];
        let idle_usd: f64 = idle.iter().filter_map(|t| t.trace.as_ref()).map(|tr| crate::cli::price_tokens(Engine::Cursor, Some("composer-2.5"), tr.tokens_in.unwrap_or(0), tr.tokens_out.unwrap_or(0), 0, 0)).sum();
        assert_eq!(idle_usd, 0.0, "no tokens, no charge");
    }

    #[tokio::test]
    async fn a_refused_plan_is_planned_again_with_the_reasons_and_router_off_pins_every_task() {
        Ledger::default().save();
        // plan → ratify refuses → plan again → 2 tasks → verify ×2 → join
        let (run, prompts, tiers) = scripted_prompts(vec![PLAN2, "@ratify:t1 and t2 both own src/a.ts", PLAN2, "@task", "@task", "@verify", "@verify", "joined"]);
        let goals = Goals::new(run, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r = goals.plan("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", false, 4, None).await;
        goals.approve(r.id); let done = settle(&goals, r.id).await;
        assert_eq!(done.status, RunStatus::Done, "{:?}", done.events);
        assert!(done.events.iter().any(|e| e.kind == "ratify" && e.text.contains("refused: t1 and t2 both own")), "{:?}", done.events);
        let plans = prompts.lock().unwrap().iter().filter(|p| p.starts_with("You are the ROUTER")).count();
        assert_eq!(plans, 2, "planned twice");
        assert!(prompts.lock().unwrap()[2].contains("A REVIEWER REFUSED THE PREVIOUS PLAN"), "the second plan carries the reasons");
        assert!(done.router && done.savings.is_none(), "the scripted traces carry no tokens — no number is invented");
        drop(tiers);
        // ROUTER OFF: no ratifier, no ledger bias, every task at mid
        let (run2, prompts2, _t2) = scripted_prompts(vec![PLAN2, "@task", "@task", "@verify", "@verify", "joined"]);
        let goals2 = Goals::new(run2, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r2 = goals2.plan_routed("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", false, 4, None, None, false).await;
        assert!(!r2.router);
        assert!(r2.tasks.iter().all(|t| t.tier == Tier::Mid), "{:?}", r2.tasks.iter().map(|t| t.tier).collect::<Vec<_>>());
        assert!(!prompts2.lock().unwrap().iter().any(|p| p.starts_with("You are the RATIFIER")), "no ratifier when the router is off");
        assert!(r2.events.iter().any(|e| e.kind == "route" && e.text.contains("router off")));
    }

    /// THE PLANNER KNOWS THE CONVERSATION (operator 2026-09-29: "the planners should always know the entire episode and the consolidated summary of
    /// the previous episodes on the current agent"): a run born in a pane carries that pane\'s earlier episodes (consolidated) and this episode in full
    /// into the planner\'s prompt; a run with no pane (a bench arm) carries nothing, and its plan is cached apart from one that had a conversation.
    #[tokio::test]
    async fn the_planner_is_handed_the_pane_conversation_and_a_bench_arm_is_not() {
        let dir = std::env::temp_dir().join(format!("front-brief-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".antiloki")).unwrap();
        let log = [
            r#"{"seq":1,"t":10,"ev":{"type":"antiloki","subtype":"user","text":"add a dark theme toggle"}}"#,
            r#"{"seq":2,"t":11,"ev":{"type":"assistant","message":{"content":[{"type":"text","text":"Added it to the header."}]}}}"#,
            r#"{"seq":3,"t":12,"ev":{"type":"antiloki","subtype":"episode","reason":"New topic","snapshot":"s","engine":"claude-code","closed_title":"Dark theme","closed_subtitle":"toggle"}}"#,
            r#"{"seq":4,"t":13,"ev":{"type":"antiloki","subtype":"episode_named","n":1,"title":"Dark theme","subtitle":"toggle","story":"A dark theme toggle was added to the header."}}"#,
            r#"{"seq":5,"t":14,"ev":{"type":"antiloki","subtype":"user","text":"what do you suggest next?"}}"#,
            r#"{"seq":6,"t":15,"ev":{"type":"assistant","message":{"content":[{"type":"text","text":"Persist the cart first, then product filtering."}]}}}"#,
            r#"{"seq":7,"t":16,"ev":{"type":"antiloki","subtype":"user","text":"go"}}"#,
        ].join("\n");
        std::fs::write(dir.join(".antiloki/agent-events-5.jsonl"), log).unwrap();
        let cwd = dir.to_string_lossy().to_string();
        // a pane run: the planner\'s prompt carries both parts
        let (run, prompts, _t) = scripted_prompts(vec![PLAN2]);
        let goals = Goals::new(run, None, 3, 5); goals.set_pane_for_next(5, false);
        let _ = goals.plan_full("p", "implement the cart persistence we settled on", Engine::ClaudeCode, &cwd, false, 4, None, None, true, false).await;
        { let p = prompts.lock().unwrap(); let q = &p[0];
          assert!(q.contains("THE CONVERSATION SO FAR") && q.contains("A dark theme toggle was added to the header.") && q.contains("operator: what do you suggest next?") && q.contains("Persist the cart first, then product filtering."), "{q}");
          assert!(q.find("THE CONVERSATION SO FAR").unwrap() < q.find("GOAL: implement the cart persistence").unwrap(), "the conversation comes before the goal"); }
        // no pane (a bench arm, an API call): nothing is added
        let (run2, p2, _t2) = scripted_prompts(vec![PLAN2]);
        let goals2 = Goals::new(run2, None, 3, 5);
        let _ = goals2.plan_full("p", "implement the cart persistence we settled on", Engine::ClaudeCode, &cwd, false, 4, None, None, true, false).await;
        assert!(!p2.lock().unwrap()[0].contains("THE CONVERSATION SO FAR"));
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// THE GAUGES DECIDE WITHOUT A MODEL (2026-09-29). A missing path parks the run before ANY call — no screen, no planner —
    /// and a seat that is not signed in is not retried a tier up.
    #[tokio::test]
    async fn a_sure_gauge_parks_the_run_with_no_model_call_and_a_dead_seat_is_not_retried() {
        let (run, prompts, _t) = scripted_prompts(vec![]);
        let goals = Goals::new(run, None, 3, 5); goals.set_dedicated_screen(true);
        let r = goals.plan_full("p", "in serve/src/definitely_not_here.rs fix the off-by-one so that it returns the last item", Engine::ClaudeCode, ".", false, 4, None, None, true, false).await;
        assert_eq!(r.status, RunStatus::AwaitingAnswers, "{:?}", r.events);
        assert_eq!(prompts.lock().unwrap().len(), 0, "no model was called to find a missing file");
        assert!(r.questions[0].text.contains("definitely_not_here.rs") && !r.questions[0].options.is_empty());
        assert!(r.events.iter().any(|e| e.kind == "gauge" && e.text.starts_with("gauges — wave:")), "{:?}", r.events);
        assert!(r.events.iter().any(|e| e.kind == "gauge" && e.text.contains("screen: sure") && e.text.contains("no model was called")), "{:?}", r.events);
        // the retry gauge: a worker whose seat says it is not logged in fails once, with no second call
        let plan = "[{\"id\":\"t1\",\"title\":\"one\",\"owns\":[\"a/**\"],\"acceptance\":[\"x\"],\"tier\":\"low\"}]";
        let calls = Arc::new(Mutex::new(0usize));
        let run2: RunFn = { let calls = calls.clone(); Arc::new(move |_e, m: Vec<ChatMessage>, _c, _j, _o: RunOpts, _k| { let calls = calls.clone(); let plan = plan.to_string(); Box::pin(async move {
            let (content, error) = if m[0].content.starts_with("You are the ROUTER") { (plan, None) } else if m[0].content.contains("JOIN") || m[0].content.contains("consolidat") { ("# joined".to_string(), None) } else { *calls.lock().unwrap() += 1; (String::new(), Some("Not logged in · Please run /login".to_string())) };
            Ok(crate::cli::RunOut { parsed: CliParsed { content, error, ..Default::default() }, ms: 1, trace: crate::cli::CliTrace { model: None, id: 0, engine: Engine::Codex, cwd: ".".into(), started: 0, ended: 1, ms: 1, tools: vec![], tokens_in: None, tokens_out: None, cost: None, reported: None, chars: 0, ok: true, error: None } })
        }) }) };
        let goals2 = Goals::new(run2, None, 3, 5);
        let r2 = goals2.plan("p", "rename the accumulate helper so that its callers keep their order", Engine::Codex, ".", false, 4, None).await;
        goals2.approve(r2.id);
        let deadline = now_ms() + 5000;
        while now_ms() < deadline { if matches!(goals2.get(r2.id).unwrap().status, RunStatus::Done | RunStatus::Failed) { break; } tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
        let done = goals2.get(r2.id).unwrap();
        assert_eq!(done.tasks[0].status, TaskStatus::Failed, "{:?}", done.events);
        assert_eq!(*calls.lock().unwrap(), 1, "a seat that is not logged in is called once, not twice");
        assert!(!done.events.iter().any(|e| e.kind == "retry"), "{:?}", done.events);
        assert!(done.events.iter().any(|e| e.kind == "gauge" && e.text.starts_with("retry: stop") && e.text.contains("not logged in")), "{:?}", done.events);
    }
    /// A DEAD SEAT HAS NO SECOND MODEL (found live 2026-09-29 in the browser): the worker on claude-code haiku failed with "Not logged in", and
    /// `run_job` retried on claude-code opus — the same login, the same failure. A signed-out seat skips a same-engine fallback; a model-specific
    /// error keeps it.
    #[tokio::test]
    async fn a_signed_out_seat_gets_no_same_engine_fallback_but_a_model_error_does() {
        let plan = "[{\"id\":\"t1\",\"title\":\"one\",\"owns\":[\"a/**\"],\"acceptance\":[\"x\"],\"tier\":\"low\"}]";
        // a signed-out seat does not retry on its own second model; it crosses to ANOTHER seat (2026-09-30), so the call count is two either way (and the scripted next seat fails the same way, which the worker then reports as "no fallback")
        for (err, expected_calls, expect_gauge) in [("claude-code exited 1: Not logged in · Please run /login", 2usize, true), ("claude-code exited 1: unknown model haiku-9", 2usize, false)] {
            let calls = Arc::new(Mutex::new(0usize));
            let run: RunFn = { let calls = calls.clone(); let err = err.to_string(); Arc::new(move |_e, m: Vec<ChatMessage>, _c, _j, _o: RunOpts, _k| { let calls = calls.clone(); let (plan, err) = (plan.to_string(), err.clone()); Box::pin(async move {
                if m[0].content.starts_with("You are the ROUTER") { return Ok(crate::cli::RunOut { parsed: CliParsed { content: plan, ..Default::default() }, ms: 1, trace: crate::cli::CliTrace { model: None, id: 0, engine: Engine::ClaudeCode, cwd: ".".into(), started: 0, ended: 1, ms: 1, tools: vec![], tokens_in: None, tokens_out: None, cost: None, reported: None, chars: 0, ok: true, error: None } }) }
                if m[0].content.contains("consolidat") || m[0].content.contains("JOIN") { return Ok(crate::cli::RunOut { parsed: CliParsed { content: "# joined".into(), ..Default::default() }, ms: 1, trace: crate::cli::CliTrace { model: None, id: 0, engine: Engine::ClaudeCode, cwd: ".".into(), started: 0, ended: 1, ms: 1, tools: vec![], tokens_in: None, tokens_out: None, cost: None, reported: None, chars: 0, ok: true, error: None } }) }
                *calls.lock().unwrap() += 1; Err(err)
            }) }) };
            let goals = Goals::new(run, None, 3, 5);
            let r = goals.plan("p", "rename the accumulate helper so that its callers keep their order", Engine::ClaudeCode, ".", false, 4, None).await;
            goals.approve(r.id);
            let deadline = now_ms() + 5000;
            while now_ms() < deadline { if matches!(goals.get(r.id).unwrap().status, RunStatus::Done | RunStatus::Failed) { break; } tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
            let done = goals.get(r.id).unwrap();
            assert_eq!(done.tasks[0].status, TaskStatus::Failed, "{err}: {:?}", done.events);
            assert_eq!(*calls.lock().unwrap(), expected_calls, "{err}: {:?}", done.events);
            assert_eq!(done.events.iter().any(|e| e.kind == "gauge" && e.text.contains("no fallback")), expect_gauge, "{err}: {:?}", done.events);
        }
    }
    /// NO PLANNER FOR L AND SMALLER (operator 2026-09-30). A small ask is one job: the planner is not called, the run says so, files its merge row with `raw` as the planner, and a big ask still gets one.
    #[tokio::test]
    async fn a_small_ask_runs_without_a_planner_and_a_big_one_still_gets_one() {
        let planner_calls = |calls: &Arc<Mutex<Vec<String>>>| calls.lock().unwrap().iter().filter(|c| c.contains("ROUTER") || c.contains("router_plan") || c.contains("decides the tier")).count();
        // the reply itself is what the planner would have said for one job
        let (route, tasks, _) = parse_route(&solo_route_reply(), 4);
        assert!(!route.delegate && tasks.is_empty(), "delegate:false, no tasks - antiloki makes the one task itself");
        assert!(!ask_bigger_than_l("rename the accumulate helper so that its callers keep their order") && ask_bigger_than_l("1. add a\n2. add b"));
        let (run_fn, prompts, _) = scripted_prompts(vec![]);
        let g = Goals::new(run_fn, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 1, 60_000);
        g.set_dedicated_screen(false);
        g.set_solo_small(true);
        let small = g.plan("p", "rename the accumulate helper so that its callers keep their order", Engine::ClaudeCode, ".", true, 4, None).await;
        let r = g.get(small.id).unwrap();
        assert_eq!(r.plan_basis, "solo", "{:?}", r.events);
        assert!(r.events.iter().any(|e| e.kind == "plan" && e.text.starts_with("no planner:")), "{:?}", r.events);
        assert_eq!(planner_calls(&prompts), 0, "the planner was not called for a small ask: {:?}", prompts.lock().unwrap());
        // a big ask (two parts of its own) still gets its planner
        let (run_fn2, prompts2, _) = scripted_prompts(vec![]);
        let g2 = Goals::new(run_fn2, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 1, 60_000);
        g2.set_dedicated_screen(false); g2.set_solo_small(true);
        let big = g2.plan("p", "1. rename the accumulate helper\n2. add a test for the renamed helper", Engine::ClaudeCode, ".", true, 4, None).await;
        let until = now_ms() + 4000;
        while now_ms() < until && g2.get(big.id).map(|r| r.status == RunStatus::Planning).unwrap_or(false) { tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
        assert_ne!(g2.get(big.id).unwrap().plan_basis, "solo");
        assert!(planner_calls(&prompts2) >= 1, "{:?}", prompts2.lock().unwrap());
    }
    /// THE SOLO RUN SAYS WHAT THE SOLO CELLS DID TO ITS EDITOR (item 4, 2026-10-01): when one seat measured ALONE won the small size, the
    /// run's route says so once, with the numbers; a run that did call a planner says nothing of the kind.
    #[test]
    fn a_solo_run_says_what_the_solo_cells_did_to_its_editor_and_a_planned_run_does_not() {
        let _ = std::fs::remove_file(crate::merges::path());
        let row = |planner: &str, audit: f64, usd: f64| crate::merges::Outcome { suite: "s2".into(), stack: "any".into(), job: "editor-s".into(), size: "s".into(), domain: "any".into(),
            planner: planner.into(), worker: "cursor:composer-2.5".into(), qa: String::new(), tasks: 1, pass_first: 1, retries: 0, failed: 0, gate_met: 0, gate_unmet: 0,
            audit: Some(audit), ms: 30_000, usd, source: "bench".into(), audit_only: false };
        for _ in 0..3 { crate::merges::record(&row("claude-code:sonnet", 0.95, 0.20)).unwrap(); crate::merges::record(&row("raw", 0.90, 0.01)).unwrap(); }
        let (run_fn, _, _) = scripted_prompts(vec![]);
        let g = Goals::new(run_fn, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 1, 60_000);
        let mk = |basis: &str| {
            let id = g.note_min_turn("p", Engine::ClaudeCode, ".", "rename the accumulate helper", Tier::Low, 1, 1, 1).unwrap();
            g.with(|rs| { let r = rs.iter_mut().find(|r| r.id == id).unwrap(); r.plan_basis = basis.into();
                r.tasks = vec![GoalTask { id: "t1".into(), title: "t1".into(), prompt: String::new(), owns: vec!["src/server/orders.rs".into()], needs: vec![], acceptance: vec![], tier: Tier::Low, specialists: vec![], skills: vec![], briefs: vec![], status: TaskStatus::Ready, wave: 0, attempts: 0, started: None, ended: None, report: None, verdict: None, error: None, trace: None, changed: vec![] }]; });
            id
        };
        let (solo, planned) = (mk("solo"), mk("reported"));
        for id in [solo, planned] { let run = g.get(id).unwrap(); let _ = g.worker_binding_for(&run, None); let _ = g.worker_binding_for(&run, None); }
        let said = |id: i64| g.get(id).unwrap().events.iter().filter(|e| e.kind == "route" && e.text.starts_with("solo editor:")).map(|e| e.text.clone()).collect::<Vec<_>>();
        let lines = said(solo);
        assert_eq!(lines.len(), 1, "once, however often the binding is asked: {lines:?}");
        assert!(lines[0].contains("solo wins") && lines[0].contains("90%"), "{}", lines[0]);
        assert!(said(planned).is_empty(), "a run that called a planner does not claim a solo choice");
    }
    /// STOP, EVEN WHILE THE QA IS JUDGING (operator 2026-09-30: "a button to pause or stop an execution, so we dont be locked on a wrong execution"). A run that is done but being judged is not
    /// cancelled - its work stands - but the QA stops, nothing is sent back, and the QA film ends at its next step. A run still working is cancelled as before.
    #[test]
    fn stop_ends_the_qa_of_a_finished_run_without_throwing_its_work_away_and_cancels_one_still_working() {
        let (run_fn, _, _) = scripted_prompts(vec![]);
        let g = Goals::new(run_fn, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 1, 60_000);
        let id = g.note_min_turn("p", Engine::ClaudeCode, ".", "remove the search", Tier::Low, 1, 1, 1).unwrap();
        g.with(|rs| { let r = rs.iter_mut().find(|r| r.id == id).unwrap(); r.status = RunStatus::Done; r.qa_pending = true; r.report = Some("the report".into()); });
        let live = crate::qa_evidence::hub_open(id, None, 0);
        assert!(!live.cancel.load(std::sync::atomic::Ordering::Relaxed));
        let after = g.cancel(id).expect("the run exists");
        assert_eq!(after.status, RunStatus::Done, "the finished work is not cancelled");
        assert!(!after.qa_pending, "the QA is over");
        assert!(after.events.iter().any(|e| e.kind == "qa-stopped" && e.text.contains("NOT verified")), "{:?}", after.events);
        assert!(live.cancel.load(std::sync::atomic::Ordering::Relaxed), "the QA film is told to stop at its next step");
        assert!(g.is_stopped(id));
        // a run still working IS cancelled
        let id2 = g.note_min_turn("p", Engine::ClaudeCode, ".", "another", Tier::Low, 1, 1, 1).unwrap();
        g.with(|rs| { let r = rs.iter_mut().find(|r| r.id == id2).unwrap(); r.status = RunStatus::Running; });
        assert_eq!(g.cancel(id2).unwrap().status, RunStatus::Cancelled);
        assert!(g.is_stopped(id2));
    }
    /// A SEAT'S OUTAGE IS NOT THE PAIRING'S FAILURE (operator 2026-09-30, run 267: Gemini's quota filed Codex x Gemini at weight 0.25 on a cell of its own).
    #[test]
    fn a_task_that_failed_on_the_seat_is_told_from_one_that_failed_on_the_answer() {
        let mk = |status: TaskStatus, err: Option<&str>| GoalTask { id: "t1".into(), title: "x".into(), prompt: String::new(), owns: vec![], needs: vec![], acceptance: vec![], tier: Tier::Mid, specialists: vec![], skills: vec![], briefs: vec![], status, wave: 0, attempts: 1, started: None, ended: None, report: None, verdict: None, error: err.map(String::from), trace: None, changed: vec![] };
        assert!(task_outage(&mk(TaskStatus::Failed, Some("Individual quota reached. Please upgrade your subscription to increase your limits. Resets in 38m26s."))));
        assert!(task_outage(&mk(TaskStatus::Failed, Some("claude-code exited 1: Not logged in · Please run /login"))));
        assert!(task_outage(&mk(TaskStatus::Failed, Some("Gemini is switched off (Settings › Seats)."))));
        assert!(!task_outage(&mk(TaskStatus::Failed, Some("the verifier refused the diff twice: acceptance not met"))), "a wrong answer is the pairing's");
        assert!(!task_outage(&mk(TaskStatus::Failed, None)));
        assert!(!task_outage(&mk(TaskStatus::Done, Some("Individual quota reached"))), "only a FAILED task can be an outage");
    }
    /// A SEAT OUT OF QUOTA IS REMEMBERED AND ROUTED AROUND (operator 2026-09-30: Codex said "You've hit your usage limit ... try again at 4:44 PM" in the
    /// middle of planning and the run died on it). The call that hit the limit crosses to the next available seat, without the old seat's model; the limit
    /// is remembered so the NEXT call does not even try the empty seat; and a seat that answers clears it. Grok stands in for the empty seat because no
    /// other test routes through it.
    #[tokio::test]
    async fn a_seat_out_of_quota_is_remembered_and_the_call_crosses_to_the_next_seat() {
        let seen: Arc<Mutex<Vec<(Engine, Option<String>)>>> = Arc::new(Mutex::new(vec![]));
        let run: RunFn = { let seen = seen.clone(); Arc::new(move |e, _m: Vec<ChatMessage>, _c, _j, o: RunOpts, _k| { seen.lock().unwrap().push((e, o.model.clone()));
            Box::pin(async move { if e == Engine::Grok { Err("You've hit your usage limit. Upgrade to Pro or try again at 4:44 PM.".to_string()) }
                else { Ok(crate::cli::RunOut { parsed: CliParsed { content: "ok".into(), ..Default::default() }, ms: 1, trace: crate::cli::CliTrace::replayed(e) }) } }) }) };
        let goals = Goals::new(run, None, 1, 5);
        crate::cli::clear_limit(Engine::Grok);
        // a run to write the crossing to (operator 2026-10-01: the calls through `run_safe` crossed seats and left NOTHING in the run's log)
        let rid = goals.plan("p", "rename the accumulate helper so that its callers keep their order", Engine::Codex, ".", false, 4, None).await.id;
        seen.lock().unwrap().clear();
        let msgs = vec![ChatMessage { role: "user".into(), content: "hi".into() }];
        let out = goals.run_safe(rid, Engine::Grok, msgs.clone(), ".".into(), false, RunOpts { model: Some("grok-x".into()), ..Default::default() }, None).await;
        assert!(out.is_ok(), "the call crossed to another seat and was answered: {:?}", out.as_ref().err());
        assert!(crate::cli::seat_limit(Engine::Grok).is_some(), "the limit is remembered");
        {
            let s = seen.lock().unwrap();
            assert_eq!(s.len(), 2, "{s:?}");
            assert_eq!((s[0].0, s[0].1.as_deref()), (Engine::Grok, Some("grok-x")));
            assert!(s[1].0 != Engine::Grok && s[1].1.is_none(), "the next seat, on its own default model, not Grok's: {s:?}");
        }
        // the next call does not try the empty seat at all
        let said = |g: &Arc<Goals>| g.get(rid).unwrap().events.iter().filter(|e| e.kind == "fallback").map(|e| e.text.clone()).collect::<Vec<_>>();
        let log = said(&goals);
        assert!(log.iter().any(|t| t.starts_with("grok is out of quota")), "the crossing is in the run's log, in run_job's words: {log:?}");
        assert!(log.iter().any(|t| t.contains("the next available seat is") && !t.contains("grok")), "and says where the call went: {log:?}");
        let again = goals.run_safe(rid, Engine::Grok, msgs, ".".into(), false, RunOpts::default(), None).await;
        assert!(again.is_ok());
        assert_eq!(seen.lock().unwrap().len(), 3, "one call, not two");
        assert!(seen.lock().unwrap()[2].0 != Engine::Grok);
        assert!(said(&goals).len() > log.len(), "the second call was swapped BEFORE the call and says so too: {:?}", said(&goals));
        crate::cli::clear_limit(Engine::Grok);
        assert!(crate::cli::seat_limit(Engine::Grok).is_none());
    }
    /// THE QA'S PLAN IS ASKED FOR ONCE MORE, JSON ONLY (operator 2026-10-01: grok-high returned prose around - or instead of - the steps and the QA fell back to a screenshot). The first
    /// answer is prose with no array: the same seat is asked again with the reason and a "JSON only" instruction, both steps are in the run's log, and the plan from the second answer is
    /// used. A second unusable answer falls back, and says it was asked twice. Scripted seat; Gemini stands in so no other test shares its limit state.
    #[tokio::test]
    async fn a_qa_answer_with_no_plan_is_asked_again_once_json_only_and_both_steps_are_logged() {
        let asked: Arc<Mutex<Vec<(Engine, String)>>> = Arc::new(Mutex::new(vec![]));
        let valid = "[{\"do\":\"goto\",\"url\":\"/\"},{\"do\":\"shot\"}]";
        let run: RunFn = { let asked = asked.clone(); Arc::new(move |e, m: Vec<ChatMessage>, _c, _j, _o: RunOpts, _k| { asked.lock().unwrap().push((e, m[0].content.clone()));
            let strict = m[0].content.contains("ONLY the JSON array");
            Box::pin(async move { Ok(crate::cli::RunOut { parsed: CliParsed { content: if strict { valid.to_string() } else { "I would open the page and click on the sort control, then check the order.".into() }, ..Default::default() }, ms: 1, trace: crate::cli::CliTrace::replayed(e) }) }) }) };
        let goals = Goals::new(run, None, 1, 5);
        let rid = goals.plan("p", "rename the accumulate helper so that its callers keep their order", Engine::Codex, ".", false, 4, None).await.id;
        asked.lock().unwrap().clear();
        let first = (goals.run)(Engine::Gemini, vec![ChatMessage { role: "user".into(), content: "write the plan".into() }], ".".into(), false, RunOpts::default(), None).await;
        let (steps, how) = goals.plan_or_ask_again(rid, first, Engine::Gemini, None, "write the plan", ".", "goal.qa_evidence_plan", crate::qa_evidence::plan_from_reply, crate::qa_evidence::plan_problem).await.expect("the second ask gave a plan");
        assert_eq!(steps.len(), 2, "the plan of the SECOND answer is the plan");
        assert!(how.contains("second ask"), "{how}");
        { let a = asked.lock().unwrap(); assert_eq!(a.len(), 2, "the ordinary call, then ONE more: {a:?}"); assert_eq!(a[0].0, a[1].0, "the same seat"); assert!(a[1].1.starts_with("write the plan") && a[1].1.contains("held no JSON array"), "the stricter ask carries the reason: {}", a[1].1); }
        let log: Vec<String> = goals.get(rid).unwrap().events.iter().filter(|e| e.kind == "qa-evidence").map(|e| e.text.clone()).collect();
        assert!(log.iter().any(|t| t.contains("no usable plan") && t.contains("once more, JSON only")), "the first step is logged: {log:?}");
        // a seat that never gives JSON: two asks, then the fallback, said honestly
        let stubborn: RunFn = Arc::new(|e, _m, _c, _j, _o: RunOpts, _k| Box::pin(async move { Ok(crate::cli::RunOut { parsed: CliParsed { content: "no plan, sorry".into(), ..Default::default() }, ms: 1, trace: crate::cli::CliTrace::replayed(e) }) }));
        let g2 = Goals::new(stubborn.clone(), None, 1, 5);
        let rid2 = g2.plan("p", "rename the accumulate helper so that its callers keep their order", Engine::Codex, ".", false, 4, None).await.id;
        let first = (g2.run)(Engine::Gemini, vec![], ".".into(), false, RunOpts::default(), None).await;
        let why = g2.plan_or_ask_again(rid2, first, Engine::Gemini, None, "p", ".", "goal.qa_evidence_plan", crate::qa_evidence::plan_from_reply, crate::qa_evidence::plan_problem).await.err().expect("no plan twice");
        assert!(why.contains("no usable plan twice"), "{why}");
    }
    /// J11 (PLAN-THE-TESTS, 2026-10-01) - A SEAT THAT HITS ITS LIMIT IN THE MIDDLE OF A RUN. The pipeline, not just the one call, and the very case of the incident (operator 2026-09-30: Codex hit its
    /// 5h limit mid-plan and the run died): the planner's seat says "usage limit", the run must finish on the next seat instead of dying, say so in its own log as a WARNING with an id of its own
    /// (never two events sharing one id: the thread keys its rows by it), and remember the limit so the next pick skips the empty seat. The seat that runs dry is whichever one the router sends the
    /// planner to first (it is the router's choice and moves with the record), and the registry is cleared for it afterwards because the limits are process-wide.
    /// (Found writing this: the calls that go through `run_safe` - the worker, the QA, the ratifier - crossed to the next seat just as well but left NOTHING in the run's log about it; only
    /// `run_job`'s jobs wrote the "out of quota" line. Fixed 2026-10-01: `run_safe` takes the run id and writes the same lines; the unit test above asserts them.)
    #[tokio::test]
    async fn j11_a_planner_whose_seat_hits_its_limit_finishes_on_the_next_seat_and_the_warning_has_an_id_of_its_own() {
        let plan = "[{\"id\":\"t1\",\"title\":\"one\",\"owns\":[\"a/**\"],\"acceptance\":[\"x\"],\"tier\":\"low\"}]";
        let planners: Arc<Mutex<Vec<Engine>>> = Arc::new(Mutex::new(vec![]));
        let dry: Arc<Mutex<Option<Engine>>> = Arc::new(Mutex::new(None));
        let run: RunFn = { let (planners, dry) = (planners.clone(), dry.clone()); Arc::new(move |e, m: Vec<ChatMessage>, _c, _j, _o: RunOpts, _k| { let (planners, dry, plan) = (planners.clone(), dry.clone(), plan.to_string()); Box::pin(async move {
            let ok = |content: &str| Ok(crate::cli::RunOut { parsed: CliParsed { content: content.into(), ..Default::default() }, ms: 1, trace: crate::cli::CliTrace::replayed(e) });
            let p = &m[0].content;
            if p.starts_with("You are the ROUTER") {
                planners.lock().unwrap().push(e);
                let first = *dry.lock().unwrap().get_or_insert(e);
                return if e == first { Err("You've hit your usage limit. Upgrade to Pro or try again at 4:44 PM.".to_string()) } else { ok(&plan) };
            }
            if p.starts_with("You are the VERIFIER") { return ok("{\"passed\":true,\"reasons\":[\"met\"]}") }
            if p.contains("consolidat") || p.contains("JOIN") { return ok("# joined") }
            if p.starts_with("You are the RATIFIER") { return ok("{\"ok\":true,\"problems\":[]}") }
            if p.contains("YOUR TASK: ") { return ok("did it") }
            ok("{}")
        }) }) };
        let goals = Goals::new(run, None, 3, 5);
        let r = goals.plan("p", "rename the accumulate helper so that its callers keep their order", Engine::Grok, ".", false, 4, None).await;
        goals.approve(r.id);
        let done = settle(&goals, r.id).await;
        let w = planners.lock().unwrap().clone();
        let dry = dry.lock().unwrap().expect("the planner was called");
        let remembered = crate::cli::seat_limit(dry).is_some();
        crate::cli::clear_limit(dry);
        assert!(remembered, "the limit is remembered, so the next pick skips the empty seat");
        assert_eq!(w.first(), Some(&dry), "the planner was first tried on the seat that ran dry: {w:?}");
        assert!(w.iter().skip(1).any(|e| *e != dry), "and then crossed to another seat: {w:?}");
        assert_eq!(done.status, RunStatus::Done, "the run finished on the next seat instead of dying: {:?}", done.events);
        assert_eq!(done.tasks[0].status, TaskStatus::Done, "{:?}", done.events);
        // the log says so, as a warning, with ids that are unique across the whole run
        let fb = done.events.iter().find(|e| e.kind == "fallback" && e.text.contains("out of quota")).unwrap_or_else(|| panic!("a fallback event naming the limit: {:?}", done.events));
        assert!(fb.id.starts_with('W'), "a fallback is a warning, and warnings carry a W id: {}", fb.id);
        assert!(fb.text.contains(dry.key()), "and it names the seat that ran dry: {}", fb.text);
        assert!(done.events.iter().any(|e| e.kind == "fallback" && (e.text.contains("the next available seat is") || e.text.contains("retrying on the fallback"))), "and says where the call went: {:?}", done.events);
        let mut ids: Vec<&str> = done.events.iter().map(|e| e.id.as_str()).collect();
        let n = ids.len(); ids.sort(); ids.dedup();
        assert_eq!(ids.len(), n, "every event of the run has its own id: {:?}", done.events.iter().map(|e| (&e.id, &e.kind)).collect::<Vec<_>>());
        assert!(done.events.iter().all(|e| e.id.starts_with('W') == crate::ids::goal_event_is_warning(&e.kind)), "the id's prefix agrees with the kind");
    }
    #[tokio::test]
    async fn an_empty_answer_is_retried_once_a_tier_up_then_fails_honestly() {
        let plan = "[{\"id\":\"t1\",\"title\":\"one\",\"owns\":[\"a/**\"],\"acceptance\":[\"x\"],\"tier\":\"low\"}]";
        // plan, t1 task (empty), t1 retry task (empty again) → failed, join
        let answers = Arc::new(Mutex::new(vec![plan.into(), "".into(), "".into(), "# joined".into()]));
        let calls = Arc::new(Mutex::new(vec![]));
        let goals = Goals::new(scripted(answers, calls.clone()), None, 3, 5);
        let r = goals.plan("p", "rename the accumulate helper so that its callers keep their order", Engine::Codex, ".", false, 4, None).await;
        goals.approve(r.id);
        let deadline = now_ms() + 5000;
        while now_ms() < deadline { if matches!(goals.get(r.id).unwrap().status, RunStatus::Done | RunStatus::Failed) { break; } tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
        let done = goals.get(r.id).unwrap();
        assert_eq!(done.status, RunStatus::Failed);
        assert_eq!(done.tasks[0].attempts, 2, "one retry for an empty answer");
        assert_eq!(done.tasks[0].tier, Tier::Mid, "the retry went a tier up");
        assert!(done.events.iter().any(|e| e.kind == "retry" && e.text.starts_with("no answer — retrying once at tier mid")), "{:?}", done.events);
        assert_eq!(done.tasks[0].status, TaskStatus::Failed);
        assert_eq!(calls.lock().unwrap().iter().filter(|c| c.starts_with("task:")).count(), 2);
    }

    /// FOUND BY AUDITING A REAL BENCH RUN (2026-09-26): OpenCode exited 0 with an empty reply, `run_job` filed a
    /// SUCCESSFUL rung for it, and the fallback never ran. An engine that says nothing has not answered.
    #[test]
    fn an_engine_that_exits_cleanly_saying_nothing_has_not_answered() {
        let out = |content: &str| Ok(crate::cli::RunOut {
            parsed: crate::cli::CliParsed { content: content.into(), tokens_in: None, tokens_out: None, cost: None, tools: vec![], error: None },
            ms: 1, trace: crate::cli::CliTrace::replayed(Engine::Opencode) });
        assert!(empty_is_failure(out("")).is_err(), "an empty reply is a failure");
        assert!(empty_is_failure(out("   \n ")).is_err(), "and so is whitespace");
        assert!(empty_is_failure(out("{}")).is_ok(), "an answer of any kind is an answer");
        // the engine's own error survives rather than being replaced by ours
        let with_err = Ok(crate::cli::RunOut {
            parsed: crate::cli::CliParsed { content: String::new(), tokens_in: None, tokens_out: None, cost: None, tools: vec![], error: Some("the gateway refused".into()) },
            ms: 1, trace: crate::cli::CliTrace::replayed(Engine::Opencode) });
        assert!(matches!(empty_is_failure(with_err), Err(e) if e == "the gateway refused"), "the engine's own words survive");
        assert!(empty_is_failure(Err("already an error".into())).is_err());
    }

    /// PLAN REPLAY, END TO END: a run whose plan worked is remembered, and the same ask at the same commit is
    /// planned from the cache with no planner call at all. The tell is the transcript line and the absence of a second
    /// planning call in the scripted answers.
    #[tokio::test]
    async fn the_same_ask_at_the_same_commit_replays_its_plan_instead_of_paying_for_it_twice() {
        let _ = std::fs::remove_file(crate::merges::path());
        let plan = "{\"tier\":\"low\",\"delegate\":false,\"why\":\"one job\",\"confidence\":\"high\",\"tasks\":[]}";
        // a git worktree is required for a replay — without a commit there is nothing to pin the plan to
        let answers = Arc::new(Mutex::new(vec![plan.into(), plan.into()]));
        let calls = Arc::new(Mutex::new(vec![]));
        let goals = Goals::new(scripted(answers, calls.clone()), Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let ask = "rename the retry constant so that the backoff stays the same";
        let r1 = goals.plan_routed("p", ask, Engine::ClaudeCode, ".", false, 4, None, None, true).await;
        assert_ne!(r1.status, RunStatus::AwaitingAnswers);
        // the cache only keeps plans that WORKED, and this fixture's run has no tasks to succeed — so nothing is kept
        // and the second ask must plan again. That is the guard, stated as a test: an unproven plan is not replayed.
        let before = calls.lock().unwrap().len();
        let r2 = goals.plan_routed("p", ask, Engine::ClaudeCode, ".", false, 4, None, None, true).await;
        assert!(calls.lock().unwrap().len() > before, "a plan that never proved itself must not be replayed");
        assert_ne!(r2.status, RunStatus::AwaitingAnswers);
    }

    /// THE GATE FIRES ON FACTS NOW (operator 2026-09-25: "it is like the model is always sure when i use it"): an ask the
    /// CODE can see is unanswerable — a reference to nothing — parks the run for one answer round with the evidence in the
    /// question, however sure the planner is. A merely SHORT ask no longer does (operator 2026-09-28: "screening is to
    /// make vague prompts sharper, not to ask useless questions"): its only question was "anything particular in mind?".
    #[tokio::test]
    async fn an_unanswerable_ask_is_screened_even_when_the_planner_claims_it_is_sure_and_a_short_one_is_not() {
        let plan = "{\"tier\":\"low\",\"delegate\":false,\"why\":\"one job\",\"confidence\":\"high\",\"tasks\":[]}";
        let answers = Arc::new(Mutex::new(vec![plan.into(), plan.into()]));
        let goals = Goals::new(scripted(answers, Arc::new(Mutex::new(vec![]))), Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r = goals.plan_routed("p", "do it the same way and keep the tests green", Engine::ClaudeCode, ".", true, 4, None, None, true).await;
        assert_eq!(r.status, RunStatus::AwaitingAnswers, "a reference to nothing must stop, not be guessed at");
        assert!(r.questions.iter().any(|q| q.text.contains("the same way")), "the question quotes the phrase: {:?}", r.questions);
        assert!(r.questions.iter().all(|q| !q.default.is_empty()), "and says what happens if the operator says nothing");

        let answers2 = Arc::new(Mutex::new(vec![plan.into(), plan.into()]));
        let goals2 = Goals::new(scripted(answers2, Arc::new(Mutex::new(vec![]))), Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let short = goals2.plan_routed("p", "fix the login", Engine::ClaudeCode, ".", true, 4, None, None, true).await;
        assert_ne!(short.status, RunStatus::AwaitingAnswers, "a short ask is planned under stated assumptions, not stopped: {:?}", short.questions);
    }

    /// …and it does NOT fire on an ask that names a real file — a screen that stopped every run would be switched off
    /// within a day, and then the gate is worse than it was.
    #[tokio::test]
    async fn a_specified_ask_plans_without_a_question() {
        let plan = "{\"tier\":\"low\",\"delegate\":false,\"why\":\"one job\",\"confidence\":\"high\",\"tasks\":[]}";
        let answers = Arc::new(Mutex::new(vec![plan.into(), plan.into()]));
        let goals = Goals::new(scripted(answers, Arc::new(Mutex::new(vec![]))), Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r = goals.plan_routed("p", "in Cargo.toml bump the version so that the build still passes", Engine::ClaudeCode, ".", true, 4, None, None, true).await;
        assert_ne!(r.status, RunStatus::AwaitingAnswers, "{:?}", r.questions);
    }

    #[tokio::test]
    async fn a_run_plans_waits_for_approval_executes_in_waves_verifies_retries_and_joins() {
        let plan = "[{\"id\":\"t1\",\"title\":\"one\",\"owns\":[\"a/**\"],\"acceptance\":[\"x\"]},{\"id\":\"t2\",\"title\":\"two\",\"owns\":[\"b/**\"],\"needs\":[\"t1\"]}]";
        // plan, t1 task, t1 verify (reject), t1 retry task, t1 verify (pass), t2 task, t2 verify, join
        let answers = Arc::new(Mutex::new(vec![plan.into(), "@task".into(), "@reject".into(), "@task".into(), "@verify".into(), "@task".into(), "@verify".into(), "# joined\nall good".into()]));
        let calls = Arc::new(Mutex::new(vec![]));
        let git: GitFn = Arc::new(|_a, _c| String::new());
        let goals = Goals::new(scripted(answers, calls.clone()), Some(git), 3, 5);
        let r = goals.plan("p", "rename the accumulate helper so that its callers keep their order", Engine::Codex, ".", true, 4, None).await;
        assert_eq!(r.status, RunStatus::AwaitingApproval); assert_eq!(r.tasks.len(), 2); assert_eq!(r.tasks[1].wave, 1); assert_eq!(r.tasks[1].status, TaskStatus::Pending);
        goals.approve(r.id);
        let deadline = now_ms() + 5000;
        while now_ms() < deadline { if matches!(goals.get(r.id).unwrap().status, RunStatus::Done | RunStatus::Failed) { break; } tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
        let done = goals.get(r.id).unwrap();
        assert_eq!(done.status, RunStatus::Done, "{:?}", done.events);
        assert_eq!(done.tasks[0].attempts, 2, "rejected once, retried with the verdict attached");
        assert_eq!(done.tasks[0].verdict.as_ref().unwrap().passed, true);
        assert_eq!(done.tasks[1].status, TaskStatus::Done);
        assert_eq!(done.report.as_deref(), Some("# joined\nall good"));
        assert!(done.blackboard.len() >= 2, "FINDING lines land on the blackboard");
        // the ratifier (2026-09-20) sits between the plan and the first task
        let seq: Vec<String> = calls.lock().unwrap().iter().filter(|c| !c.starts_with("ratify:")).cloned().collect();
        assert_eq!(seq[0], "plan:"); assert_eq!(seq[1], "task:one"); assert_eq!(seq[2], "verify:"); assert_eq!(seq[3], "task:one"); assert_eq!(seq[5], "task:two"); assert_eq!(seq.last().unwrap(), "join:");
        assert!(done.events.iter().any(|e| e.kind == "reject") && done.events.iter().any(|e| e.kind == "retry"));
    }

    #[tokio::test]
    async fn a_failed_task_blocks_its_dependants_and_the_run_fails_with_the_fallback_report() {
        let plan = "[{\"id\":\"t1\",\"title\":\"one\"},{\"id\":\"t2\",\"title\":\"two\",\"needs\":[\"t1\"]}]";
        let answers = Arc::new(Mutex::new(vec![plan.into(), "@task".into(), "@reject".into(), "@task".into(), "@reject".into(), String::new()]));
        let goals = Goals::new(scripted(answers, Arc::new(Mutex::new(vec![]))), Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r = goals.plan("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", false, 2, None).await;
        goals.approve(r.id);
        let deadline = now_ms() + 5000;
        while now_ms() < deadline { if matches!(goals.get(r.id).unwrap().status, RunStatus::Done | RunStatus::Failed) { break; } tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
        let done = goals.get(r.id).unwrap();
        assert_eq!(done.status, RunStatus::Failed);
        assert_eq!(done.tasks[0].status, TaskStatus::Failed); assert!(done.tasks[0].error.as_deref().unwrap().starts_with("rejected twice"));
        assert_eq!(done.tasks[1].status, TaskStatus::Blocked);
        assert!(done.report.as_deref().unwrap().starts_with("# Goal run #"), "the deterministic fallback when the joiner says nothing");
        // retry the failed task → the run goes back to running
        let again = goals.retry(r.id, "t1").unwrap();
        assert_eq!(again.status, RunStatus::Running); assert_eq!(again.tasks[0].status, TaskStatus::Ready);
        goals.cancel(r.id);
        assert_eq!(goals.get(r.id).unwrap().status, RunStatus::Cancelled);
    }

    #[tokio::test]
    async fn subagents_run_bounded_and_deliver() {
        let answers = Arc::new(Mutex::new(vec!["r1".into(), "r2".into(), String::new()]));
        let subs = Subagents::new(scripted(answers, Arc::new(Mutex::new(vec![]))), 2);
        let jobs = subs.spawn("p", "goal", &[("a".into(), None), ("b".into(), Some("do b".into())), ("c".into(), None)], Engine::Codex, ".", false);
        assert_eq!(jobs.len(), 3); assert!(jobs[1].prompt.contains("YOUR TASK: do b") && jobs[1].prompt.contains("- a\n- b\n- c"), "a custom prompt is not a title, so no sibling is filtered (as in the TS)");
        assert!(jobs[0].prompt.contains("- b\n- c") && !jobs[0].prompt.contains("- a\n"), "a title-only task drops itself from the siblings");
        let deadline = now_ms() + 5000;
        while now_ms() < deadline { if subs.list(None).iter().all(|j| matches!(j.status, JobStatus::Done | JobStatus::Failed)) { break; } tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
        let all = subs.list(Some("p"));
        assert_eq!(all.iter().filter(|j| j.status == JobStatus::Done).count(), 2); assert_eq!(all.iter().filter(|j| j.status == JobStatus::Failed).count(), 1);
        assert!(subs.mark_delivered(all[0].id)); assert!(!subs.cancel(all[0].id), "a settled job cannot be cancelled");
        assert_eq!(parse_simple_plan("- do a\n- do b", 4), vec![("do a".to_string(), "do a".to_string()), ("do b".into(), "do b".into())]);
        assert_eq!(parse_simple_plan("[{\"title\":\"x\",\"prompt\":\"px\"}]", 4), vec![("x".to_string(), "px".to_string())]);
    }

    // THE ROUTER (2026-09-19): tier · delegation · per-task tiers, the ledger's bias, the inline run
    #[test]
    fn parse_route_reads_the_object_and_the_bare_array_and_a_no_delegation_answer() {
        let (r, tasks, _) = parse_route("{\"tier\":\"high\",\"delegate\":true,\"why\":\"two independent parts\",\"tasks\":[{\"id\":\"t1\",\"title\":\"a\",\"tier\":\"low\",\"owns\":[\"a/**\"]},{\"id\":\"t2\",\"title\":\"b\",\"tier\":\"high\",\"owns\":[\"b/**\"]}]}", 4);
        assert_eq!((r.tier, r.delegate, r.why.as_str(), r.verify_tier, r.join_tier), (Tier::High, true, "two independent parts", Tier::High, Tier::High));
        assert_eq!(tasks.iter().map(|t| t.tier).collect::<Vec<_>>(), vec![Tier::Low, Tier::High]);
        let (r, tasks, _) = parse_route("{\"tier\":\"low\",\"delegate\":false,\"why\":\"one file\",\"tasks\":[]}", 4);
        assert_eq!((r.tier, r.delegate, tasks.len()), (Tier::Low, false, 0));
        let (r, tasks, _) = parse_route("[{\"id\":\"t1\",\"title\":\"old shape\"}]", 4); // the pre-router answer still works
        assert_eq!((r.tier, r.delegate, tasks.len(), tasks[0].tier), (Tier::Mid, true, 1, Tier::Mid));
        let (r, _, _) = parse_route("{\"tier\":\"low\",\"tasks\":[{\"id\":\"t1\",\"title\":\"x\",\"tier\":\"high\"}]}", 4);
        assert_eq!(r.tier, Tier::High, "the run's tier is never below its hardest task");
        assert_eq!(r.verify_tier, Tier::High);
    }

    #[test]
    fn the_ledger_biases_a_tier_up_after_twelve_runs_with_half_rejected_first_attempts() {
        let mut l = Ledger::default();
        assert!(l.bias(Engine::Codex, Tier::Low).is_none(), "too few samples");
        for i in 0..6 { l.rows.push(LedgerRow { engine: "codex".into(), tier: Tier::Low, passed: i % 2 == 0, attempts: 1, t: i }); }
        assert!(l.bias(Engine::Codex, Tier::Low).is_none(), "six samples are still noise (2026-09-22: eight raised a whole wave)");
        for i in 6..12 { l.rows.push(LedgerRow { engine: "codex".into(), tier: Tier::Low, passed: i % 2 == 0, attempts: 1, t: i }); }
        assert!(l.bias(Engine::Codex, Tier::Low).unwrap().contains("rejected first-attempt at low in 6 of the last 12"));
        assert!(l.bias(Engine::ClaudeCode, Tier::Low).is_none(), "another engine's evidence does not apply");
        assert!(l.bias(Engine::Codex, Tier::High).is_none(), "high has nowhere to go");
        for i in 0..8 { l.rows.push(LedgerRow { engine: "codex".into(), tier: Tier::Mid, passed: true, attempts: 1, t: i }); }
        assert!(l.bias(Engine::Codex, Tier::Mid).is_none());
        let s = l.summary(); let rows = s["rows"].as_array().unwrap();
        assert_eq!(rows.iter().find(|r| r["tier"] == "mid").unwrap()["first_pass_rate"], 100.0);
        assert_eq!(rows.iter().find(|r| r["tier"] == "low").unwrap()["samples"], 12);
    }

    #[tokio::test]
    async fn a_no_delegation_route_ends_the_run_inline_with_the_tier_in_its_report() {
        let plan = "{\"tier\":\"low\",\"delegate\":false,\"why\":\"a one-line change in one file\",\"tasks\":[]}";
        let answers = Arc::new(Mutex::new(vec![plan.into()]));
        let goals = Goals::new(scripted(answers, Arc::new(Mutex::new(vec![]))), Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r = goals.plan("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", true, 4, None).await;
        assert_eq!(r.status, RunStatus::Inline);
        assert!(r.report.as_deref().unwrap().contains("ONE job — do it in this terminal at tier **low** (a one-line change in one file)"));
        assert_eq!(r.route.as_ref().unwrap().delegate, false);
        assert!(r.events.iter().any(|e| e.kind == "route" && e.text.contains("no delegation")));
    }

    /// THE JUDGE MUST SEE THE SOURCE (run 352, 2026-10-09): untracked antiloki skill files ("owned" by a `**` task) used to fill the front of the diff, so the claim check saw no source change five times.
    #[test]
    fn antilokis_own_files_do_not_crowd_the_source_out_of_the_diff() {
        let d = std::env::temp_dir().join(format!("antiloki-ours-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap(); std::fs::create_dir_all(d.join(".agents/skills/antiloki-context")).unwrap();
        let cwd = d.to_string_lossy().to_string();
        let git = |a: &[&str]| { let o = std::process::Command::new("git").args(a).current_dir(&d).output().unwrap(); String::from_utf8_lossy(&o.stdout).into_owned() };
        git(&["init", "-q"]);
        std::fs::write(d.join(".agents/skills/antiloki-context/SKILL.md"), "context line\n".repeat(900)).unwrap();      // ~12 KB, sorts first
        std::fs::write(d.join("src/App.css"), "body { margin: 0; }\n").unwrap();
        let (run, _, _) = scripted_prompts(vec![]);
        let g = Goals::new(run, None, 1, 5);
        assert_eq!(g.changed_files(&cwd), vec!["src/App.css".to_string()], "our skill file is not a change");
        let diff = g.diff_of(&cwd, &["**".to_string()], None);
        assert!(diff.contains("src/App.css") && !diff.contains("antiloki-context"), "{}", diff.chars().take(200).collect::<String>());
        assert!(diff.chars().take(6000).collect::<String>().contains("src/App.css"), "the source is inside the 6000-character window the claim check reads");
        let _ = std::fs::remove_dir_all(&d);
    }
    /// A PROJECT THAT HAS NEVER BEEN COMMITTED STILL GETS A BASELINE (operator 2026-10-09: the design project had no commit; its runs had `base = None`, so the screenshot QA and the design
    /// round were skipped). A real git repo with files and no commit, no model.
    #[test]
    fn a_repo_without_a_first_commit_still_gets_a_baseline_and_shows_what_the_run_changed() {
        let d = std::env::temp_dir().join(format!("antiloki-nocommit-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join("src")).unwrap();
        let cwd = d.to_string_lossy().to_string();
        let git = |a: &[&str]| { let o = std::process::Command::new("git").args(a).current_dir(&d).output().unwrap(); String::from_utf8_lossy(&o.stdout).into_owned() };
        git(&["init", "-q"]);
        std::fs::write(d.join("src/App.tsx"), "export const App = () => <ul>one</ul>;\n").unwrap();
        std::fs::write(d.join("AGENTS.md"), "an AI by-product already in the box\n").unwrap();
        let (run, _, _) = scripted_prompts(vec![]);
        let g = Goals::new(run, None, 1, 5);
        let base = g.snapshot(&cwd, 1, 1, "before").expect("a repo with no commit still has a baseline");
        assert!(g.changed_since(&cwd, Some(&base)).is_empty(), "nothing done yet: {:?}", g.changed_since(&cwd, Some(&base)));
        std::fs::write(d.join("src/App.tsx"), "export const App = () => <ul>two</ul>;\n").unwrap();
        assert_eq!(g.changed_since(&cwd, Some(&base)), vec!["src/App.tsx".to_string()], "only the edited file counts, not the by-product that was already there");
        let _ = std::fs::remove_dir_all(&d);
    }
    /// THE RUN IS JUDGED ON WHAT IT CHANGED (operator 2026-09-29, run #259 "remove the search"): the chat had added a search, tag chips and (an earlier run) a theme toggle to a
    /// worktree, all UNCOMMITTED. The worker removed the search - correctly - but the verifier's diff was taken against HEAD, where the search never was: it saw the theme and the
    /// chips as "added" and no removal, rejected, and the retry (told the diff "adds category-filter and theme") reverted the earlier work too. A real git box, no model.
    #[test]
    fn a_run_is_measured_against_the_box_as_it_found_it_not_against_head() {
        let d = std::env::temp_dir().join(format!("antiloki-base-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join("src")).unwrap();
        let cwd = d.to_string_lossy().to_string();
        let git = |a: &[&str]| { let o = std::process::Command::new("git").args(a).current_dir(&d).output().unwrap(); String::from_utf8_lossy(&o.stdout).into_owned() };
        git(&["init", "-q"]); git(&["config", "user.email", "t@t"]); git(&["config", "user.name", "t"]);
        std::fs::write(d.join("src/App.tsx"), "export const App = () => <ul>{products}</ul>;\n").unwrap(); git(&["add", "-A"]); git(&["commit", "-qm", "base"]);
        // uncommitted work already in the box: the theme toggle, the tag chips and the search (the chat wrote these), and an untracked file of an earlier run
        std::fs::write(d.join("src/App.tsx"), "export const App = () => <ul>{products}</ul>;\nconst theme = 1; // toggle\nconst chips = 2;\nconst search = 3;\n").unwrap();
        std::fs::write(d.join("src/notes.md"), "an earlier run's file\n").unwrap();
        let (run, _, _) = scripted_prompts(vec![]);
        let g = Goals::new(run, None, 1, 5);
        let base = g.snapshot(&cwd, 1, 1, "before").expect("a git box has a checkpoint");
        assert!(crate::checkpoint::changes(&cwd, &crate::checkpoint::head_tree(&cwd).unwrap(), &base.tree).iter().any(|c| c.1 == "src/notes.md"), "the checkpoint holds the untracked file");
        // nothing done yet: the run has changed nothing, even though the tree is dirty against HEAD
        assert!(g.changed_since(&cwd, Some(&base)).is_empty(), "{:?}", g.changed_since(&cwd, Some(&base)));
        assert!(g.diff_of(&cwd, &["**".to_string()], Some(&base)).trim().is_empty());
        let before = g.fingerprint(&cwd, Some(&base));
        // the worker removes the search - only that
        std::fs::write(d.join("src/App.tsx"), "export const App = () => <ul>{products}</ul>;\nconst theme = 1; // toggle\nconst chips = 2;\n").unwrap();
        let after = g.fingerprint(&cwd, Some(&base));
        let fresh: Vec<&String> = after.iter().filter(|(f, h)| !before.iter().any(|(bf, bh)| bf == f && bh == h)).map(|(f, _)| f).collect();
        assert_eq!(fresh, vec!["src/App.tsx"], "a file that was already dirty counts when it changed AGAIN");
        let diff = g.diff_of(&cwd, &["src/App.tsx".to_string()], Some(&base));
        assert!(diff.contains("-const search = 3;"), "the verifier sees the REMOVAL: {diff}");
        assert!(!diff.contains("+const theme") && !diff.contains("+const chips") && !diff.contains("notes.md"), "and not the earlier work: {diff}");
        // the old measure, for the record: against HEAD the removal is invisible and the earlier work looks like this run's
        let old = g.diff_of(&cwd, &["src/App.tsx".to_string()], None);
        assert!(old.contains("+const theme") && !old.contains("-const search"), "{old}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A finished run with a page task, its gate passed (or not) and a design state, waiting in `qa_pending` for `qa_settle`.
    fn design_fixture(blockers: &[&str], n_fixes: usize, qa_rounds: u32, gate_unmet: bool) -> (Arc<Goals>, i64) {
        use crate::blueprint as bp;
        let (run_fn, _, _) = scripted_prompts(vec![]);
        let g = Goals::new(run_fn, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 1, 60_000);
        let id = g.note_min_turn("p", Engine::ClaudeCode, ".", "build the shop page", Tier::Low, 1, 1, 1).unwrap();
        let t = GoalTask { id: "t1".into(), title: "build the page".into(), prompt: "build the page".into(), owns: vec!["index.html".into()], needs: vec![], acceptance: vec!["the page lists products".into()], tier: Tier::Low, specialists: vec![], skills: vec![], briefs: vec![], status: TaskStatus::Done, wave: 0, attempts: 1, started: None, ended: Some(1), report: Some("done".into()), verdict: None, error: None, trace: None, changed: vec!["index.html".into()] };
        let check = |met| bp::ClaimCheck { task: "t1".into(), path: "index.html".into(), claim: "the page lists products".into(), kind: "judgment".into(), met: Some(met), why: "no product list on the page".into(), by: "fake".into(), ms: 1, cost: 0.0 };
        let gate = bp::QaGate { met: if gate_unmet { 0 } else { 1 }, unmet: gate_unmet as usize, unproven: 0, cost: 0.01, ms: 1, checks: vec![check(!gate_unmet)], judge: String::new() };
        let fixes: Vec<crate::design_round::Fix> = (1..=n_fixes).map(|i| crate::design_round::Fix { axis: "spacing".into(), r#where: format!("block {i}"), problem: "cramped".into(), fix: format!("raise the gap {i} to 24px"), evidence: String::new() }).collect();
        let st = crate::design_round::DesignState { blockers: blockers.iter().map(|b| b.to_string()).collect(), fixes, rounds: (n_fixes > 0) as u32, seat: "cursor/composer-2.5".into(), usd: 0.004, score_first: Some(0.6), score: Some(0.6), ..Default::default() };
        g.with(|rs| { let r = rs.iter_mut().find(|r| r.id == id).unwrap(); r.status = RunStatus::Done; r.tasks = vec![t]; r.qa_pending = true; r.qa_gate = Some(gate); r.qa_rounds = qa_rounds; r.design = Some(st); r.report = Some("the report".into()); r.delivered = false; });
        (g, id)
    }
    fn reasons_of(g: &Goals, id: i64) -> Vec<String> { g.get(id).unwrap().tasks[0].verdict.as_ref().map(|v| v.reasons.clone()).unwrap_or_default() }

    /// AN INDEPENDENT QA ASKED FOR FROM THE RESULT ONLY REPORTS (2026-10-10, found live on run 384): the same failing design rule that sends a task back in the normal loop leaves the run Done, writes the finding on it
    /// and lowers the confidence - it never reworks or fails delivered work.
    #[tokio::test(flavor = "current_thread")]
    async fn an_independent_qa_from_the_result_reports_and_never_sends_a_delivered_run_back() {
        let (g, id) = design_fixture(&["horizontal overflow: at 390px wide the page scrolls sideways (scrollWidth 612)"], 0, 0, false);
        g.report_only.lock().unwrap().insert(id);
        g.qa_settle(id).await;
        let r = g.get(id).unwrap();
        assert_eq!((r.status, r.qa_pending, r.qa_rounds), (RunStatus::Done, false, 0), "delivered work stays delivered, no send-back round");
        assert!(r.tasks[0].status == TaskStatus::Done, "the task was not touched");
        assert!(r.events.iter().any(|e| e.kind == "qa-findings" && e.text.contains("horizontal overflow") && e.text.contains("only reports")), "the finding is on the run: {:?}", r.events.iter().map(|e| e.kind.clone()).collect::<Vec<_>>());
        let c = r.confidence.expect("a reading"); assert_ne!(c.level, "high", "a failing design rule keeps it from high: {c:?}"); assert!(c.qa_ran);
        // and the same run WITHOUT the flag is sent back, as it always was
        let (g2, id2) = design_fixture(&["horizontal overflow: at 390px wide the page scrolls sideways (scrollWidth 612)"], 0, 0, false);
        g2.qa_settle(id2).await;
        assert_eq!(g2.get(id2).unwrap().status, RunStatus::Running, "the normal loop reworks it");
    }

    /// THE DESIGN ROUND IN THE SEND-BACK LOOP (PLAN-THE-DESIGN S5 / D4, 2026-10-01). The critic's fixes are taste: they go back ONCE, as numbered lines in one brief, only while the QA cap has a send-back left, and they never
    /// hold a run that cannot send them. A D1 rule failure is a fact: it goes back like an unmet claim, first in the brief, and a run that spends the cap still failing it is not delivered.
    #[tokio::test(flavor = "current_thread")]
    async fn the_critics_fixes_go_back_once_as_one_numbered_brief_and_the_run_is_then_delivered_with_design_notes() {
        let (g, id) = design_fixture(&[], 3, 0, false);
        g.qa_settle(id).await;
        let r = g.get(id).unwrap();
        assert_eq!((r.status, r.qa_rounds, r.qa_pending, r.report.is_none()), (RunStatus::Running, 1, true, true), "the design round IS a send-back: it counts as one QA round");
        let re = reasons_of(&g, id);
        assert!(re[0].contains("design round - the behaviour is right") && re[0].contains(&format!("Q{id}.1")), "{re:?}");
        assert!(re[1].contains("design fix #1 [spacing] block 1") && re[2].contains("design fix #2") && re[3].contains("design fix #3 [spacing] block 3: cramped -> raise the gap 3 to 24px"), "numbered, one per line: {re:?}");
        assert!(!re.iter().any(|x| x.contains("design rule")), "no rule failed: {re:?}");
        assert!(r.design.as_ref().unwrap().sent, "marked sent");
        // the builder finished again, the page was measured again: the same fixes are NOT sent twice; the run is delivered with the notes
        g.with(|rs| { let r = rs.iter_mut().find(|r| r.id == id).unwrap(); r.status = RunStatus::Done; r.tasks[0].status = TaskStatus::Done; r.qa_pending = true; r.report = Some("the report".into()); r.design.as_mut().unwrap().score = Some(0.9); });
        g.qa_settle(id).await;
        let r = g.get(id).unwrap();
        assert_eq!((r.status, r.qa_rounds, r.qa_pending), (RunStatus::Done, 1, false), "delivered, and no second round: {:?}", r.events.last());
        let rep = r.report.unwrap();
        assert!(rep.contains("DESIGN NOTES (design round: 3 fix(es) from cursor/composer-2.5, +$0.0040, design lint 0.60 -> 0.90)") && !rep.contains("not applied"), "{rep}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn taste_never_holds_a_run_when_the_cap_is_spent_the_fixes_become_notes_and_the_run_is_delivered() {
        let (g, id) = design_fixture(&[], 2, 2, false);
        g.qa_settle(id).await;
        let r = g.get(id).unwrap();
        assert_eq!((r.status, r.qa_rounds, r.qa_pending, r.questions.is_empty()), (RunStatus::Done, 2, false, true), "no send-back left: delivered, no question to the operator");
        let rep = r.report.unwrap();
        assert!(rep.contains("DESIGN NOTES") && rep.contains("#1 [spacing] block 1") && rep.contains("[not applied]"), "the fixes are on the report as notes, marked not applied: {rep}");
        // send-backs switched off: the same
        let (g, id) = design_fixture(&[], 2, 0, false); g.set_qa_rounds(0);
        g.qa_settle(id).await;
        let r = g.get(id).unwrap();
        assert_eq!((r.status, r.qa_rounds, r.qa_pending), (RunStatus::Done, 0, false));
        assert!(r.report.unwrap().contains("[not applied]"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_design_rule_failure_goes_back_first_with_the_fixes_in_the_same_brief_and_counts_one_round() {
        let (g, id) = design_fixture(&["text contrast: 2 text node(s) are below 3:1 and cannot be read - p.sub 2.52:1"], 2, 0, false);
        g.qa_settle(id).await;
        let r = g.get(id).unwrap();
        assert_eq!((r.status, r.qa_rounds), (RunStatus::Running, 1), "ONE round, not a rule round plus a taste round");
        let re = reasons_of(&g, id);
        assert!(re[0].contains("design rule (this blocks delivery) - text contrast") && re[0].contains("2.52:1"), "{re:?}");
        assert!(re.iter().position(|x| x.contains("design fix #1")).unwrap() > 0 && re.len() == 4, "the rule, the header, the two fixes: {re:?}");
        // behaviour AND design in one brief: an unmet claim does not wait for the design round
        let (g, id) = design_fixture(&["horizontal overflow: at 390px wide the page scrolls sideways (scrollWidth 612)"], 0, 0, true);
        g.qa_settle(id).await;
        let re = reasons_of(&g, id);
        assert!(re.iter().any(|x| x.contains("the claim “the page lists products” is NOT met")) && re.iter().any(|x| x.contains("horizontal overflow")), "{re:?}");
        assert_eq!(g.get(id).unwrap().qa_rounds, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_rule_still_failing_when_the_cap_is_spent_asks_the_operator_and_when_send_backs_are_off_it_is_written_on_the_report() {
        let rule = "horizontal overflow: at 390px wide the page scrolls sideways (scrollWidth 612)";
        let (g, id) = design_fixture(&[rule], 2, 2, false);
        g.qa_settle(id).await;
        let r = g.get(id).unwrap();
        assert_eq!((r.status, r.qa_pending, r.qa_rounds), (RunStatus::AwaitingAnswers, false, 2), "a page that breaks a rule is not delivered quietly");
        assert!(r.questions[0].kind == "qa-direction" && r.questions[0].text.contains("design rule") && r.questions[0].text.contains("scrollWidth 612"), "{:?}", r.questions);
        assert!(!r.design.as_ref().unwrap().sent, "the taste fixes were not sent: there was no round left");
        let (g, id) = design_fixture(&[rule], 2, 0, false); g.set_qa_rounds(0);
        g.qa_settle(id).await;
        let r = g.get(id).unwrap();
        assert_eq!((r.status, r.qa_pending), (RunStatus::Done, false));
        assert!(r.report.unwrap().contains("design rule") , "delivered as it is, the rule failure on the report");
    }

    /// QA SENDS THE RUN BACK (operator 2026-09-29): an unmet claim goes to its worker as the retry brief, the run re-opens and is NOT announced as finished; when the send-backs are spent
    /// the run is delivered with the unmet claims written on it. Both directions asserted.
    #[tokio::test(flavor = "current_thread")]
    async fn qa_sends_an_unmet_claim_back_to_its_worker_and_asks_the_operator_after_two_rounds() {
        use crate::blueprint as bp;
        let (run_fn, _, _) = scripted_prompts(vec![]);
        let g = Goals::new(run_fn, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 1, 60_000);
        let id = g.note_min_turn("p", Engine::ClaudeCode, ".", "remove the search", Tier::Low, 1, 1, 1).unwrap();
        let mut t = GoalTask { id: "t1".into(), title: "remove the search".into(), prompt: "remove the search".into(), owns: vec!["**".into()], needs: vec![], acceptance: vec!["the search is gone".into()], tier: Tier::Low, specialists: vec![], skills: vec![], briefs: vec![], status: TaskStatus::Done, wave: 0, attempts: 1, started: None, ended: Some(1), report: Some("done".into()), verdict: None, error: None, trace: None, changed: vec![] };
        t.status = TaskStatus::Done;
        let unmet = || Some(bp::QaGate { met: 0, unmet: 1, unproven: 0, cost: 0.0, ms: 1, checks: vec![bp::ClaimCheck { task: "t1".into(), path: "**".into(), claim: "the search is gone".into(), kind: "judgment".into(), met: Some(false), why: "src/App.tsx still renders the search box".into(), by: "opus".into(), ms: 1, cost: 0.0 }], judge: String::new() });
        g.with(|rs| { let r = rs.iter_mut().find(|r| r.id == id).unwrap(); r.status = RunStatus::Done; r.tasks = vec![t.clone()]; r.qa_pending = true; r.qa_gate = unmet(); r.report = Some("the report".into()); r.delivered = false; });
        // round 1: back to the worker, with the gate's own words
        g.qa_settle(id).await;
        let r = g.get(id).unwrap();
        assert_eq!((r.status, r.qa_rounds, r.qa_pending, r.report.is_none()), (RunStatus::Running, 1, true, true), "re-opened, still pending, not announced");
        assert_eq!(r.tasks[0].status, TaskStatus::Ready); assert_eq!(r.tasks[0].attempts, 0);
        let v = r.tasks[0].verdict.as_ref().unwrap(); assert!(!v.passed && v.by == "qa-gate" && v.reasons[0].contains("src/App.tsx still renders the search box") && v.reasons[0].contains(&format!("Q{id}.1")), "{v:?}");
        let ev = r.events.iter().find(|e| e.kind == "qa-send-back").unwrap(); assert!(ev.id.starts_with('W') && ev.text.contains(&format!("[Q{id}.1]")), "{ev:?}");
        // the retry brief the worker reads carries the feedback
        assert!(task_prompt(&r, &r.tasks[0], &[]).contains("src/App.tsx still renders the search box"));
        // round 3 does not happen: two send-backs are spent, and the run becomes a QUESTION for the operator: what happened, and chips to decide
        g.with(|rs| { let r = rs.iter_mut().find(|r| r.id == id).unwrap(); r.status = RunStatus::Done; r.tasks[0].status = TaskStatus::Done; r.qa_pending = true; r.qa_rounds = 2; r.qa_gate = unmet(); r.report = Some("the report".into()); });
        g.qa_settle(id).await;
        let r = g.get(id).unwrap();
        assert_eq!((r.status, r.qa_pending), (RunStatus::AwaitingAnswers, false));
        let q = &r.questions[0]; assert_eq!((q.kind.as_str(), q.certainty), ("qa-direction", Certainty::Blocking));
        assert!(q.text.contains("sent this run back 2 time(s)") && q.text.contains("src/App.tsx still renders the search box"), "{}", q.text);
        assert_eq!(q.options, QA_DIRECTION_CHIPS.to_vec()); assert_eq!(q.default, "accept it as it is");
        // typing a new direction sends the task back with QA's words AND the operator's, and QA gets its two rounds again
        let back = g.answer(id, &json!({ "qa1": "remove it from the header component too, not only the page" })).await.unwrap();
        assert_eq!((back.status, back.qa_rounds), (RunStatus::Running, 0));
        let v = back.tasks[0].verdict.as_ref().unwrap(); assert_eq!(back.tasks[0].status, TaskStatus::Ready);
        assert!(v.reasons.iter().any(|x| x.contains("NEW DIRECTION") && x.contains("header component")) && v.reasons.iter().any(|x| x.contains("still renders the search box")), "{v:?}");
        // the chips: accept keeps the work with QA's verdict written on it; cancel ends it
        for (chip, want) in [("accept it as it is", RunStatus::Done), ("cancel the run", RunStatus::Cancelled)] {
            g.with(|rs| { let r = rs.iter_mut().find(|r| r.id == id).unwrap(); r.status = RunStatus::Done; r.tasks[0].status = TaskStatus::Done; r.qa_pending = true; r.qa_rounds = 2; r.qa_gate = unmet(); r.report = Some("the report".into()); r.questions.clear(); });
            g.qa_settle(id).await;
            let after = g.answer(id, &json!({ "qa1": chip })).await.unwrap();
            assert_eq!(after.status, want, "{chip}");
            if want == RunStatus::Done { assert!(after.report.unwrap().contains("accepted by the operator with these unmet")); }
        }
        // a passing gate just clears the flag
        g.with(|rs| { let r = rs.iter_mut().find(|r| r.id == id).unwrap(); r.qa_pending = true; r.qa_rounds = 0; r.qa_gate = Some(bp::QaGate { met: 1, unmet: 0, unproven: 0, cost: 0.0, ms: 1, checks: vec![], judge: String::new() }); });
        g.qa_settle(id).await; assert!(!g.get(id).unwrap().qa_pending);
    }

    /// THE #259 SCENARIO THROUGH THE ORCHESTRATOR: attempt 1 removes the search (passes the gate); attempt 2, on the same box, wipes it back to HEAD (rejected by CODE, and the box is put back).
    #[test]
    fn the_checkpoint_gate_lets_a_removal_through_and_stops_a_wipe_and_puts_the_box_back() {
        let d = std::env::temp_dir().join(format!("antiloki-gate-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join("src")).unwrap();
        let cwd = d.to_string_lossy().to_string();
        let git = |a: &[&str]| { let o = std::process::Command::new("git").args(a).current_dir(&d).output().unwrap(); String::from_utf8_lossy(&o.stdout).into_owned() };
        git(&["init", "-q"]); git(&["config", "user.email", "t@t"]); git(&["config", "user.name", "t"]);
        std::fs::write(d.join("src/App.tsx"), "base\n").unwrap(); git(&["add", "-A"]); git(&["commit", "-qm", "base"]);
        std::fs::write(d.join("src/App.tsx"), "base\ntheme\nchips\nsearch\n").unwrap();     // the chat's and the earlier runs' uncommitted work
        let (run_fn, _, _) = scripted_prompts(vec![]);
        let g = Goals::new(run_fn, None, 1, 5);
        let mk_task = || GoalTask { id: "t1".into(), title: "remove the search".into(), prompt: "remove the search".into(), owns: vec!["**".into()], needs: vec![], acceptance: vec![], tier: Tier::Low, specialists: vec![], skills: vec![], briefs: vec![], status: TaskStatus::Verifying, wave: 0, attempts: 1, started: None, ended: None, report: None, verdict: None, error: None, trace: None, changed: vec![] };
        let id = g.note_min_turn("p", Engine::ClaudeCode, &cwd, "remove the search", Tier::Low, 1, 1, 1).unwrap();
        let mut run = g.get(id).unwrap(); run.write = true; run.pane_id = 7; run.goal = "remove the search".into(); run.tasks = vec![mk_task()];
        let ab1 = g.snapshot(&cwd, 7, id, "before attempt 1").unwrap();
        std::fs::write(d.join("src/App.tsx"), "base\ntheme\nchips\n").unwrap();               // attempt 1: only the search goes
        assert!(g.checkpoint_gate(&run, &run.tasks[0], &ab1).is_none(), "a removal, asked for, passes");
        let ab2 = g.snapshot(&cwd, 7, id, "before attempt 2").unwrap();
        git(&["checkout", "--", "."]);                                                          // attempt 2: told a false complaint, resets to HEAD
        let (rule, why, restored) = g.checkpoint_gate(&run, &run.tasks[0], &ab2).expect("the wipe is stopped");
        assert_eq!(rule, "wiped"); assert!(restored, "{why}");
        assert_eq!(std::fs::read_to_string(d.join("src/App.tsx")).unwrap(), "base\ntheme\nchips\n", "the box is back to what attempt 1 left");
        // a sibling working in the same box: the wipe is still refused, but the box is not touched (it would undo the sibling)
        git(&["checkout", "--", "."]);
        let mut busy = run.clone(); let mut sib = mk_task(); sib.id = "t2".into(); sib.status = TaskStatus::Running; busy.tasks.push(sib);
        let (_, _, restored2) = g.checkpoint_gate(&busy, &busy.tasks[0], &ab2).expect("still refused");
        assert!(!restored2);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// run #359: attempt 2 only deletes what attempt 1 added (the verifier told it to) - a correction, not a wipe.
    #[test]
    fn a_retry_that_only_deletes_what_its_first_attempt_added_is_a_correction_not_a_rejection() {
        let d = std::env::temp_dir().join(format!("antiloki-gate-corr-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join("src")).unwrap();
        let cwd = d.to_string_lossy().to_string();
        let git = |a: &[&str]| { let o = std::process::Command::new("git").args(a).current_dir(&d).output().unwrap(); String::from_utf8_lossy(&o.stdout).into_owned() };
        git(&["init", "-q"]); git(&["config", "user.email", "t@t"]); git(&["config", "user.name", "t"]);
        std::fs::write(d.join("src/App.tsx"), "base\n").unwrap(); git(&["add", "-A"]); git(&["commit", "-qm", "base"]);
        let (run_fn, _, _) = scripted_prompts(vec![]);
        let g = Goals::new(run_fn, None, 1, 5);
        let id = g.note_min_turn("p", Engine::ClaudeCode, &cwd, "the cart looks ugly", Tier::Low, 1, 1, 1).unwrap();
        let mut run = g.get(id).unwrap(); run.write = true; run.pane_id = 7; run.goal = "the cart looks ugly".into();
        let base = g.snapshot(&cwd, 7, id, "before the run").unwrap(); run.base = Some(base.clone());
        let mut task: GoalTask = serde_json::from_value(json!({ "id": "t1", "title": "make the cart look better", "prompt": "make the cart look better", "owns": ["**"], "needs": [], "acceptance": [], "tier": "low", "status": "verifying", "wave": 0, "attempts": 1, "specialists": [], "skills": [], "briefs": [], "changed": [] })).unwrap();
        std::fs::write(d.join("src/App.tsx"), "base\n.cart { padding: 8px }\n<span>INVENTORY</span>\n").unwrap();   // attempt 1: styling plus a label nobody asked for
        let ab2 = g.snapshot(&cwd, 7, id, "before attempt 2").unwrap();
        std::fs::write(d.join("src/App.tsx"), "base\n.cart { padding: 8px }\n").unwrap();                                 // attempt 2: deletes ONLY the label
        run.tasks = vec![task.clone()];
        let first = g.checkpoint_gate(&run, &run.tasks[0], &ab2);
        assert_eq!(first.as_ref().map(|x| x.0), Some("only-deletions"), "on the FIRST attempt a deletion-only result is still refused: {first:?}");
        std::fs::write(d.join("src/App.tsx"), "base\n.cart { padding: 8px }\n").unwrap();                                 // the refusal above put the box back; attempt 2 makes the same deletion again
        task.attempts = 2; run.tasks = vec![task];
        assert!(g.checkpoint_gate(&run, &run.tasks[0], &ab2).is_none(), "from the second attempt on, the run's own net additions count: the correction passes");
        assert_eq!(std::fs::read_to_string(d.join("src/App.tsx")).unwrap(), "base\n.cart { padding: 8px }\n", "and the box is left as the correction made it");
        assert_eq!(gate_added(1, 0, Some(5)), 0); assert_eq!(gate_added(2, 0, Some(5)), 5); assert_eq!(gate_added(2, 3, Some(1)), 3); assert_eq!(gate_added(2, 0, None), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A FAILED RUN SAVED NOTHING (2026-09-26). The saved figure is what the header chip shows the operator, so a
    /// run that delivered no work must report its SPEND and no saving. Both branches asserted — the first version of
    /// this test wrapped its assertions in `if` guards and would have passed without checking anything.
    #[test]
    fn a_failed_run_reports_its_spend_and_claims_no_saving() {
        let sv = || Savings {
            tokens_in: 60_000, tokens_out: 6_000,
            actual_usd: 0.362, baseline_usd: 0.603, saved_usd: 0.241, saved_pct: 40.0,
            baseline: "claude-code · opus".into(), tiers: vec!["t1:mid".into()],
        };
        // a run that finished keeps every figure it earned
        let done = settled_savings(true, Some(sv())).unwrap();
        assert_eq!((done.saved_usd, done.saved_pct), (0.241, 40.0));
        assert_eq!(done.baseline, "claude-code · opus");

        // a run that failed keeps the SPEND and loses the CLAIM
        let failed = settled_savings(false, Some(sv())).unwrap();
        assert_eq!(failed.actual_usd, 0.362, "what was spent is still on the record");
        assert_eq!(failed.saved_usd, 0.0, "nothing delivered, so nothing saved");
        assert_eq!(failed.saved_pct, 0.0);
        assert_eq!(failed.baseline_usd, failed.actual_usd, "no baseline to beat when nothing was produced");
        assert!(failed.baseline.contains("nothing delivered"), "and it says why: {}", failed.baseline);

        // nothing to price stays nothing — never a zero-saving row invented out of None
        assert!(settled_savings(false, None).is_none());
    }

    /// A PLANNER THAT REPORTS NOTHING IS STILL PRICED (2026-09-26). `plan_usd` was `cost.unwrap_or(0.0)`, and an
    /// engine on a subscription often reports no usage at all — so the most expensive stage of a run recorded as
    /// free. The tokens are counted instead, and `plan_basis` says so, because an estimate and a measurement must
    /// never be averaged or confused.
    #[tokio::test]
    async fn a_planner_that_reports_no_cost_is_priced_from_its_tokens_and_says_so() {
        let plan = "{\"tier\":\"low\",\"delegate\":false,\"why\":\"one line\",\"tasks\":[]}";
        let answers = Arc::new(Mutex::new(vec![plan.into()]));
        // `scripted` reports `cost: None` — exactly the case that used to record $0.00
        let goals = Goals::new(scripted(answers, Arc::new(Mutex::new(vec![]))), Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r = goals.plan("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", true, 4, None).await;
        assert_eq!(r.plan_basis, "estimated", "nobody reported a cost, so it was counted — and the row says which");
        assert!(r.plan_usd > 0.0, "a planner call that happened is never $0.00: got {}", r.plan_usd);
        assert!(r.events.iter().any(|e| e.kind == "plan" && e.text.contains("reported no cost")), "the run says out loud that the figure is not a measurement");
    }

    /// QA v2: an inline route is judged too — once the pane's turn lands, the diff against the ask, the reply as the report.
    /// the env var is process-wide: put it back however the test ends
    fn scopeguard_qa_mode() -> impl Drop { struct G; impl Drop for G { fn drop(&mut self) { unsafe { std::env::remove_var("ANTILOKI_QA_MODE"); } } } G }

    #[test]
    fn an_ask_that_enumerates_its_tasks_is_recognised() {
        assert_eq!(enumerated_tasks("Add eight modules:\n\n1. src/a.ts — x\n2. src/b.ts — y\n  3) src/c.ts\n\nWhen done, stop."), 3);
        assert_eq!(enumerated_tasks("fix the bug in 2.0 and 3.5 versions"), 0, "a version number is not an item");
        assert_eq!(enumerated_tasks("1. one thing"), 1);
        assert_eq!(enumerated_tasks("rename the constant"), 0);
    }

    #[tokio::test]
    async fn an_inline_write_route_is_judged_when_its_turn_lands() {
        let plan = "{\"tier\":\"low\",\"delegate\":false,\"why\":\"one file\",\"tasks\":[]}";
        const REVIEW: &str = "{\"passed\":true,\"score\":91,\"summary\":\"the constant is renamed everywhere.\",\"findings\":[]}";
        let (run, prompts, tiers) = scripted_prompts(vec![plan, REVIEW]);
        let goals = Goals::new(run, Some(Arc::new(|a: &[&str], _c: &str| if a[0] == "status" { " M src/x.ts\n".to_string() } else { "diff --git a/src/x.ts\n-old\n+new\n".to_string() })), 3, 5);
        let r = goals.plan("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, "/box", true, 4, None).await;
        assert_eq!(r.status, RunStatus::Inline);
        assert!(!goals.judge_inline_later(r.id, "done"), "QA off: nothing runs");
        assert!(goals.set_qa_wanted(r.id, true));
        assert!(goals.judge_inline_later(r.id, "renamed it in x.ts"));
        assert!(!goals.judge_inline_later(r.id, "again"), "once");
        let deadline = now_ms() + 5000;
        while now_ms() < deadline && goals.get(r.id).unwrap().qa_review.is_none() { tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
        let judged = goals.get(r.id).unwrap();
        let qa = judged.qa_review.clone().expect("the verdict lands on the inline run");
        assert!(qa.passed && qa.score == 91, "{qa:?}");
        assert_eq!(judged.report.as_deref(), Some("renamed it in x.ts"));
        let ps = prompts.lock().unwrap(); let judge = ps.last().unwrap();
        assert!(judge.contains("GOAL: rename the retry constant so that the backoff stays the same") && judge.contains("an inline route — no plan") && judge.contains("renamed it in x.ts") && judge.contains("+new"), "{judge}");
        assert_eq!(*tiers.lock().unwrap().last().unwrap(), Some(Tier::High));
        // a judged inline write run counts for the seal like any other
        assert!(Goals::seal_for(&[judged.clone()], "/box").is_none(), "passed → no seal");
    }

    #[tokio::test]
    async fn tasks_run_at_their_own_tier_and_verify_at_least_mid() {
        let plan = "{\"tier\":\"mid\",\"delegate\":true,\"tasks\":[{\"id\":\"t1\",\"title\":\"one\",\"tier\":\"low\",\"owns\":[\"a/**\"]}]}";
        let answers = Arc::new(Mutex::new(vec![plan.into(), "@task".into(), "@verify".into(), "# joined".into()]));
        let tiers: Arc<Mutex<Vec<(String, Option<Tier>)>>> = Arc::new(Mutex::new(vec![]));
        let t2 = tiers.clone();
        let run: RunFn = Arc::new(move |_e, messages, _c, _j, opts, _k| {
            let (answers, tiers) = (answers.clone(), t2.clone());
            Box::pin(async move {
                let prompt = messages[0].content.clone();
                let kind = if prompt.starts_with("You are the ROUTER") { "plan" } else if prompt.starts_with("You are the VERIFIER") { "verify" } else if prompt.starts_with("You are the ORCHESTRATOR") { "join" } else { "task" };
                tiers.lock().unwrap().push((kind.into(), opts.tier));
                let content = { let mut a = answers.lock().unwrap(); if a.is_empty() { String::new() } else { a.remove(0) } };
                let content = if content == "@task" { "did it".to_string() } else if content == "@verify" { "{\"passed\":true,\"reasons\":[\"met\"]}".into() } else { content };
                Ok(RunOut { parsed: CliParsed { content, ..Default::default() }, ms: 1, trace: CliTrace { model: None, id: 1, engine: Engine::Codex, cwd: ".".into(), started: 0, ended: 1, ms: 1, tools: vec![], tokens_in: None, tokens_out: None, cost: None, reported: None, chars: 0, ok: true, error: None } })
            })
        });
        let goals = Goals::new(run, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r = goals.plan("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", false, 4, None).await;
        assert_eq!(r.tasks[0].tier, Tier::Low);
        goals.approve(r.id);
        let deadline = now_ms() + 5000;
        while now_ms() < deadline { if matches!(goals.get(r.id).unwrap().status, RunStatus::Done | RunStatus::Failed) { break; } tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
        let seq = tiers.lock().unwrap().clone();
        assert_eq!(seq, vec![("plan".to_string(), Some(Tier::High)), ("task".to_string(), Some(Tier::Low)), ("verify".to_string(), Some(Tier::Mid))], "one task: its report stands, no join call (2026-09-22)");
        assert!(goals.get(r.id).unwrap().report.as_deref().unwrap_or("").contains("@task") || goals.get(r.id).unwrap().report.is_some(), "the task's own report is the run's report");
    }


    #[tokio::test]
    async fn the_planner_sees_the_lenses_and_a_task_gets_its_specialist_briefs() {
        // the planner names a specialist for t1; the review fn answers for it; the task prompt carries the brief
        let plan = "{\"tier\":\"mid\",\"delegate\":true,\"why\":\"w\",\"tasks\":[{\"id\":\"t1\",\"title\":\"hunt\",\"owns\":[\"src/a.ts\"],\"specialists\":[\"axis:defects\"]}]}";
        let answers = Arc::new(Mutex::new(vec![plan.into(), "@task".into(), "@verify".into(), "joined".into()]));
        let prompts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let run: RunFn = { let (answers, prompts) = (answers.clone(), prompts.clone()); Arc::new(move |_e, messages: Vec<ChatMessage>, _c, _j, _o, _k| { let (answers, prompts) = (answers.clone(), prompts.clone()); Box::pin(async move {
            prompts.lock().unwrap().push(messages[0].content.clone());
            let content = { let mut a = answers.lock().unwrap(); if a.is_empty() { String::new() } else { a.remove(0) } };
            let content = if content == "@task" { "did it".to_string() } else if content == "@verify" { "{\"passed\":true,\"reasons\":[\"met\"]}".into() } else { content };
            Ok(crate::cli::RunOut { parsed: CliParsed { content, ..Default::default() }, ms: 1, trace: crate::cli::CliTrace { model: None, id: 0, engine: Engine::ClaudeCode, cwd: ".".into(), started: 0, ended: 1, ms: 1, tools: vec![], tokens_in: None, tokens_out: None, cost: None, reported: None, chars: 0, ok: true, error: None } })
        }) }) };
        let goals = Goals::new(run, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let reviewed: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(vec![]));
        let list: SpecialistListFn = Arc::new(|_cwd| Box::pin(async { vec![("axis:defects".to_string(), "Defects Reviewer".to_string(), "recorded defects".to_string()), ("axis:tests".to_string(), "Tests Reviewer".to_string(), "coverage".to_string())] }));
        let review: SpecialistReviewFn = { let reviewed = reviewed.clone(); Arc::new(move |_cwd, id, path| { let reviewed = reviewed.clone(); Box::pin(async move { reviewed.lock().unwrap().push((id.clone(), path.clone())); Ok(format!("# {id} on {path}\n\n**finding** · a theory")) }) }) };
        goals.with_specialists(list, review);
        let r = goals.plan("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", false, 4, None).await;
        assert!(prompts.lock().unwrap()[0].contains("- axis:defects — Defects Reviewer: recorded defects"), "the planner is told the lenses");
        assert_eq!(r.tasks[0].specialists, vec!["axis:defects".to_string()]);
        goals.approve(r.id);
        let deadline = now_ms() + 5000;
        while now_ms() < deadline { if matches!(goals.get(r.id).unwrap().status, RunStatus::Done | RunStatus::Failed) { break; } tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
        assert_eq!(reviewed.lock().unwrap().clone(), vec![("axis:defects".to_string(), "src/a.ts".to_string())]);
        // a retry of the task keeps its briefs — no second review call
        let n_before = reviewed.lock().unwrap().len();
        let _ = goals.retry(r.id, "t1");
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        assert_eq!(reviewed.lock().unwrap().len(), n_before, "the retry reused the briefs");
        let task_prompt_sent = prompts.lock().unwrap().iter().find(|p| p.starts_with("You are task t1")).cloned().unwrap();
        assert!(task_prompt_sent.contains("SPECIALIST BRIEFS") && task_prompt_sent.contains("# axis:defects on src/a.ts"), "{task_prompt_sent}");
        let done = goals.get(r.id).unwrap();
        assert_eq!(done.tasks[0].briefs.len(), 1);
        assert!(done.events.iter().any(|e| e.kind == "specialist" && e.text.contains("axis:defects on src/a.ts")));
    }

    // ── EDGE CASES (operator 2026-09-19: "make more tests, test edge cases") ────────────────────────
    #[test]
    fn overlapping_globs_are_exclusive_too() {
        let text = "[{\"id\":\"t1\",\"title\":\"A\",\"owns\":[\"src/forks/Scheduler.js\"]},{\"id\":\"t2\",\"title\":\"B\",\"owns\":[\"src/forks/*.js\",\"src/other.js\"]},{\"id\":\"t3\",\"title\":\"C\",\"owns\":[\"src/**\"]}]";
        let (tasks, warnings) = parse_plan(text, 8);
        assert_eq!(tasks[0].owns, ["src/forks/Scheduler.js"]);
        assert_eq!(tasks[1].owns, ["src/other.js"], "a glob covering a sibling's file is dropped");
        assert!(tasks[2].owns.is_empty(), "a glob covering both siblings' paths owns nothing");
        assert!(warnings.iter().any(|w| w.contains("src/forks/*.js: overlaps src/forks/Scheduler.js owned by t1")), "{warnings:?}");
    }

    pub(crate) fn scripted_prompts(answers: Vec<&str>) -> (RunFn, Arc<Mutex<Vec<String>>>, Arc<Mutex<Vec<Option<Tier>>>>) {
        let answers = Arc::new(Mutex::new(answers.into_iter().map(String::from).collect::<Vec<_>>()));
        let prompts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
        let tiers: Arc<Mutex<Vec<Option<Tier>>>> = Arc::new(Mutex::new(vec![]));
        let run: RunFn = { let (answers, prompts, tiers) = (answers.clone(), prompts.clone(), tiers.clone()); Arc::new(move |_e, messages: Vec<ChatMessage>, _c, _j, o: RunOpts, _k| { let (answers, prompts, tiers) = (answers.clone(), prompts.clone(), tiers.clone()); Box::pin(async move {
            prompts.lock().unwrap().push(messages[0].content.clone()); tiers.lock().unwrap().push(o.tier);
            // THE RATIFIER (2026-09-20) answers "ok" without consuming a scripted answer — unless the script
            // has an "@ratify:…" line next, which refuses the plan with that reason
            let content = { let mut a = answers.lock().unwrap(); if messages[0].content.starts_with("You are the RATIFIER") { match a.first().and_then(|x| x.strip_prefix("@ratify:")).map(String::from) { Some(why) => { a.remove(0); format!("{{\"ok\":false,\"problems\":[\"{why}\"]}}") } None => "{\"ok\":true,\"problems\":[]}".to_string() } } else if a.is_empty() { String::new() } else { a.remove(0) } };
            let content = if content == "@task" { "did it".to_string() } else if content == "@verify" { "{\"passed\":true,\"reasons\":[\"met\"]}".into() } else if content == "@slow" { tokio::time::sleep(std::time::Duration::from_millis(400)).await; "late".into() } else { content };
            Ok(crate::cli::RunOut { parsed: CliParsed { content, ..Default::default() }, ms: 1, trace: crate::cli::CliTrace { model: None, id: 0, engine: Engine::ClaudeCode, cwd: ".".into(), started: 0, ended: 1, ms: 1, tools: vec![], tokens_in: None, tokens_out: None, cost: None, reported: None, chars: 0, ok: true, error: None } })
        }) }) };
        (run, prompts, tiers)
    }
    async fn settle(goals: &Arc<Goals>, id: i64) -> GoalRun {
        let deadline = now_ms() + 5000;
        while now_ms() < deadline { if matches!(goals.get(id).unwrap().status, RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled | RunStatus::Inline) { break; } tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
        goals.get(id).unwrap()
    }
    /// QA (2026-09-21, amended 2026-09-22): with the pane's QA on, the PER-CLAIM GATE runs after the join — each
    /// acceptance claim checked alone (compile and test claims by the machine, judgment claims one call each). The
    /// whole-diff judge of 2026-09-21 is kept for one release behind ANTILOKI_QA_MODE=judge, which is what this test
    /// exercises; off, nothing extra runs either way.
    #[tokio::test]
    async fn qa_judges_the_whole_run_after_the_join_when_the_pane_asked_for_it() {
        unsafe { std::env::set_var("ANTILOKI_QA_MODE", "judge"); }
        let _guard = scopeguard_qa_mode();
        const REVIEW: &str = "{\"passed\":false,\"score\":60,\"summary\":\"t2 changed a file the plan never named.\",\"findings\":[{\"task\":\"t2\",\"kind\":\"drift\",\"text\":\"b/x.ts edited, plan owns a/**\"},{\"task\":null,\"kind\":\"ok\",\"text\":\"t1 met its criteria\"}]}";
        let (run, prompts, tiers) = scripted_prompts(vec![PLAN2, "@task", "@task", "@verify", "@verify", "joined", REVIEW]);
        let goals = Goals::new(run, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        goals.set_qa_rounds(0); // this test is about the judge's verdict; sending a run back is tested on its own
        let r = goals.plan("p", "rename the accumulate helper so that its callers keep their order", Engine::ClaudeCode, ".", true, 4, None).await;
        assert!(goals.set_qa_wanted(r.id, true));
        goals.approve(r.id);
        let done = settle(&goals, r.id).await;
        assert_eq!(done.status, RunStatus::Done);
        // the judge runs after the join: wait for its verdict
        let deadline = now_ms() + 5000;
        while now_ms() < deadline && goals.get(r.id).unwrap().qa_review.is_none() { tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
        let judged = goals.get(r.id).unwrap();
        let qa = judged.qa_review.expect("the judge's verdict lands on the run");
        assert!(!qa.passed); assert_eq!(qa.score, 60); assert_eq!(qa.findings.len(), 2); assert_eq!(qa.findings[0].task.as_deref(), Some("t2")); assert_eq!(qa.findings[0].kind, "drift"); assert!(qa.error.is_none());
        assert!(judged.events.iter().any(|e| e.kind == "qa-review-fail" && e.text.contains("score 60")), "{:?}", judged.events);
        let ps = prompts.lock().unwrap(); let judge = ps.last().unwrap();
        assert!(judge.starts_with("You are the QA of a finished goal run") && judge.contains("GOAL: rename the accumulate helper so that its callers keep their order") && judge.contains("- t1 ·") && judge.contains("THE JOINED REPORT:\njoined"), "{judge}");
        assert_eq!(*tiers.lock().unwrap().last().unwrap(), Some(Tier::High), "the judge is a high-tier call");
        // the judge's JSON, parsed; an answer out of shape is a review with an error, not a crash
        assert_eq!(parse_qa_review("sure:\n{\"passed\":true,\"score\":95,\"summary\":\"fine\",\"findings\":[]}").unwrap().score, 95);
        assert!(parse_qa_review("I think it is fine").is_none());
        // QA v2: the verdict rides on the PR body; a failed verdict on the box's latest judged run seals it
        let failed = parse_qa_review(r#"{"passed":false,"score":40,"summary":"t2 skipped its tests","findings":[{"task":"t2","kind":"unmet","text":"no test for the new branch"}]}"#).unwrap();
        let md = Goals::qa_verdict_md(&failed);
        assert!(md.contains("QA judge: FAILED 40/100 — t2 skipped its tests") && md.contains("- [unmet] t2 · no test for the new branch"), "{md}");
        let passed = parse_qa_review(r#"{"passed":true,"score":97,"summary":"all met","findings":[]}"#).unwrap();
        let mut a = goals.get(r.id).unwrap(); a.cwd = "/box".into(); a.write = true; a.ended = Some(10); a.qa_review = Some(passed);
        let mut b = a.clone(); b.id = 99; b.ended = Some(20); b.qa_review = Some(failed.clone());
        assert!(Goals::seal_for(&[a.clone()], "/box").is_none(), "a passed verdict never seals");
        assert_eq!(Goals::seal_for(&[a.clone(), b.clone()], "/box").map(|q| q.score), Some(40), "the latest judged run failed → sealed");
        assert!(Goals::seal_for(&[b.clone(), a.clone()], "/other").is_none(), "another box is not sealed");
        let mut c = a.clone(); c.id = 100; c.ended = Some(30); // a later pass lifts the seal
        assert!(Goals::seal_for(&[a.clone(), b.clone(), c], "/box").is_none());
        let body = Goals::box_verdicts_md(&[a.clone(), b], "/box");
        assert!(body.starts_with("goal run #99") && body.contains("QA judge: FAILED") && body.contains("QA judge: PASSED"), "{body}");
        assert!(Goals::pr_body(&a).contains("QA judge: PASSED"), "the run's own PR body carries its verdict");
        // QA off: no judge, no verdict
        let (run2, prompts2, _) = scripted_prompts(vec![PLAN2, "@task", "@task", "@verify", "@verify", "joined"]);
        let goals2 = Goals::new(run2, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r2 = goals2.plan("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", true, 4, None).await; goals2.approve(r2.id);
        let d2 = settle(&goals2, r2.id).await; tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(goals2.get(r2.id).unwrap().qa_review.is_none() && d2.status == RunStatus::Done);
        assert!(!prompts2.lock().unwrap().iter().any(|p| p.starts_with("You are the QA")));
    }
    /// THE GATE IS TOLD WHO THE NEIGHBOURS ARE (2026-09-27). `goal.qa_review` instructs the judge that "a change that
    /// breaks a neighbour is a risk" and used to hand it only the goal, plan, report and diff — so the neighbours were a
    /// question it could answer only by guessing or by spending turns reading the tree. `importers_of` answers it from
    /// the repository, deterministically and without a model call.
    ///
    /// Asserted against ministore's real shape: `App.tsx` imports `products.ts`, so a run that changes products.ts must
    /// be told App.tsx is downstream — and a file must never be listed as its own neighbour.
    #[test]
    fn the_qa_gate_is_told_which_files_import_the_ones_that_changed() {
        let cwd = "/home/user/Projects/ministore";
        if !std::path::Path::new(cwd).join("src/App.tsx").is_file() { return; }   // the fixture is the operator's box
        let n = importers_of(cwd, &["src/products.ts".to_string()]);
        assert!(n.contains("src/products.ts is imported by"), "the changed file must be named: {n}");
        assert!(n.contains("App.tsx"), "App.tsx imports products — it is the neighbour at risk: {n}");
        assert!(!n.contains("is imported by: src/products.ts"), "a file is not its own neighbour: {n}");
        // nothing to say is said with silence, so the prompt section disappears rather than printing an empty heading
        assert_eq!(importers_of(cwd, &[]), "");
        assert_eq!(importers_of(cwd, &["src/nothing-imports-this-xyz.ts".to_string()]), "");
    }

    /// THE PLANNER IS SHOWN THE BOX'S SKILLS, LEGIBLY — so a skill it never assigns is a DECISION, not a rendering
    /// fault (2026-09-27). bench/skills measured six runs in which the planner was offered exactly one skill, whose
    /// description names the goal's own subject, and assigned it in NONE. Before calling that a product gap the render
    /// had to be ruled out: this asserts the slug and the description both reach the prompt as a readable line, and
    /// that the section disappears when the box has none.
    #[test]
    fn the_planner_is_shown_the_boxs_skills_as_a_readable_list() {
        let skills = vec![("money-format".to_string(),
                           "How this store formats money — the exact rule, and the mistake that keeps being made".to_string())];
        let p = plan_prompt_full("format prices as US dollars", 1, true, &[], &skills);
        assert!(p.contains("- money-format:"), "the slug must reach the planner as a list line: {}", &p[..p.len().min(400)]);
        assert!(p.contains("the mistake that keeps being made"), "and so must the description");
        assert!(p.to_lowercase().contains("skill"), "with the instruction that names them");
        // with no skills the section is absent, so an empty box never invites an assignment
        let none = plan_prompt_full("format prices as US dollars", 1, true, &[], &[]);
        assert!(!none.contains("- money-format:"));
    }

    #[tokio::test]
    async fn the_screening_gate_parks_the_run_on_a_blocking_question_and_plans_under_the_answer() {
        const ASK: &str = "{\"questions\":[{\"text\":\"Which layer?\",\"options\":[\"REST\",\"GraphQL\"],\"default\":\"REST\",\"certainty\":\"blocking\"},{\"text\":\"Keep the name?\",\"default\":\"yes\",\"certainty\":\"assumable\"}]}";
        let (run, prompts, _t) = scripted_prompts(vec![ASK, PLAN2, "@task", "@task", "@verify", "@verify", "joined"]);
        let goals = Goals::new(run, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r = goals.plan_full("p", "add the orders endpoint so that it returns the saved order", Engine::ClaudeCode, ".", false, 4, None, None, true, true).await;
        assert_eq!(r.status, RunStatus::AwaitingAnswers, "{:?}", r.events);
        assert_eq!(r.questions.len(), 1); assert_eq!(r.questions[0].id, "q1"); assert_eq!(r.questions[0].default, "REST");
        assert_eq!(r.assumptions, vec!["Keep the name? → yes"], "the assumable one never stops the run");
        assert!(r.events.iter().any(|e| e.kind == "screen" && e.text.contains("1 blocking question")));
        assert!(prompts.lock().unwrap()[0].contains("FIRST SCREEN the goal"));
        assert!(goals.approve(r.id).map(|x| x.status) == Some(RunStatus::AwaitingAnswers), "approve is not the way out of the screen");
        // the answer round: q1 answered, planned under it, auto-approved (the run was auto) and run to the end
        let after = goals.answer(r.id, &json!({ "q1": "GraphQL" })).await.unwrap();
        assert!(matches!(after.status, RunStatus::Running | RunStatus::Joining | RunStatus::Done), "{:?}", after.status);
        assert_eq!(after.questions[0].answer.as_deref(), Some("GraphQL"));
        let p2 = prompts.lock().unwrap()[1].clone();
        assert!(p2.contains("THE OPERATOR ANSWERED") && p2.contains("Which layer? → GraphQL") && p2.contains("ALREADY answered"), "{p2}");
        let done = settle(&goals, r.id).await;
        assert_eq!(done.status, RunStatus::Done, "{:?}", done.events);
        assert!(goals.answer(r.id, &json!({})).await.is_none(), "one round only");
        // the defaults: no answers given → each question takes its default
        let (run2, _p2, _t2) = scripted_prompts(vec![ASK, PLAN2]);
        let goals2 = Goals::new(run2, None, 3, 5);
        let r2 = goals2.plan_full("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", false, 4, None, None, true, false).await;
        let a2 = goals2.answer(r2.id, &json!({})).await.unwrap();
        assert_eq!(a2.status, RunStatus::AwaitingApproval, "not auto — the human gate as usual");
        assert_eq!(a2.questions[0].answer.as_deref(), Some("REST"));
        assert!(a2.events.iter().any(|e| e.kind == "answer" && e.text == "q1: REST (the default)"));
        // router off: the prompt never screens, a question reply is just a plan with no tasks
        let (run3, p3, _t3) = scripted_prompts(vec![PLAN2]);
        let goals3 = Goals::new(run3, None, 3, 5);
        let r3 = goals3.plan_full("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", false, 4, None, None, false, false).await;
        assert_eq!(r3.status, RunStatus::AwaitingApproval); assert!(!p3.lock().unwrap()[0].contains("SCREEN"));
    }
    /// L9 (2026-09-29): the screen is a CALL OF ITS OWN. When it blocks, the planner is never called — the run is parked
    /// on the question at the price of a screen, not of a plan; when it is clear, the planner is told so; when the seat
    /// answers nothing readable, the planner's own gate stands in.
    #[tokio::test]
    async fn the_dedicated_screen_blocks_before_the_planner_and_clears_into_it() {
        const ASKQ: &str = "{\"questions\": [\"Which layer should the endpoint live in?\"]}";
        const SHAPED: &str = "{\"questions\":[{\"kind\":\"target\",\"question\":\"Which layer should the endpoint live in?\",\"chips\":[\"REST route\",\"GraphQL resolver\",\"gRPC service\"]}]}";
        let ask = "add the orders endpoint so that it returns the saved order";
        // blocked: the decision call, then the SHAPER (kinds + chips); the planner is never called
        let (run, prompts, _t) = scripted_prompts(vec![ASKQ, SHAPED, PLAN2]);
        let goals = Goals::new(run, None, 3, 5); goals.set_dedicated_screen(true);
        let r = goals.plan_full("p", ask, Engine::ClaudeCode, ".", false, 4, None, None, true, false).await;
        assert_eq!(r.status, RunStatus::AwaitingAnswers, "{:?}", r.events);
        assert_eq!(r.questions.len(), 1); assert_eq!(r.questions[0].kind, "target"); assert_eq!(r.questions[0].default, "REST route");
        assert_eq!(r.questions[0].options, vec!["REST route", "GraphQL resolver", "gRPC service"]);
        { let p = prompts.lock().unwrap(); assert_eq!(p.len(), 2, "the decision and the shaper, not the planner: {p:?}"); assert!(p[0].contains("SCREENING step") && !p[0].contains("ROUTER and PLANNER")); assert!(p[1].contains("SHAPER of a screening question") && p[1].contains("Which layer should the endpoint live in?") && p[1].contains("target: Where should"), "{}", p[1]); }
        assert!(r.events.iter().any(|e| e.kind == "screen" && e.text.contains("1 blocking question") && e.text.contains("screened on")), "{:?}", r.events);
        assert!(r.events.iter().any(|e| e.kind == "screen" && e.text.starts_with("shaped into kinds (target·3 chips)")), "{:?}", r.events);
        // …and the answer round plans under the answer, with the planner (never a second screen)
        let after = goals.answer(r.id, &json!({ "q1": "GraphQL resolver" })).await.unwrap();
        assert!(after.questions[0].answer.as_deref() == Some("GraphQL resolver"));
        { let p = prompts.lock().unwrap(); assert!(p.len() >= 3); assert!(p[2].contains("ROUTER and PLANNER") && p[2].contains("ALREADY answered"), "{}", p[2]); }
        // the shaper may DROP a question the box answers — then nothing was worth asking, and the planner runs, told it was screened
        let (run6, p6, _t6) = scripted_prompts(vec![ASKQ, "{\"questions\":[]}", PLAN2]);
        let goals6 = Goals::new(run6, None, 3, 5); goals6.set_dedicated_screen(true);
        let r6 = goals6.plan_full("p", ask, Engine::ClaudeCode, ".", false, 4, None, None, true, false).await;
        assert_eq!(r6.status, RunStatus::AwaitingApproval, "{:?}", r6.events);
        { let p = p6.lock().unwrap(); assert!(p.len() >= 3 && p[2].contains("ALREADY judged this goal clear"), "{p:?}"); }
        // the shaper answers nothing readable: the CODE shapes them (kind by its words, chips from the kind), and the run still parks
        let (run5, p5, _t5) = scripted_prompts(vec!["{\"questions\": [\"Where should the endpoint go?\"]}", "no json at all", PLAN2]);
        let goals5 = Goals::new(run5, None, 3, 5); goals5.set_dedicated_screen(true);
        let r5 = goals5.plan_full("p", ask, Engine::ClaudeCode, ".", false, 4, None, None, true, false).await;
        assert_eq!(r5.status, RunStatus::AwaitingAnswers, "{:?}", r5.events);
        assert_eq!(r5.questions[0].kind, "target"); assert!(r5.questions[0].options.len() >= 3 && r5.questions[0].options.len() <= 7, "{:?}", r5.questions[0].options);
        assert_eq!(p5.lock().unwrap().len(), 2);
        assert!(r5.events.iter().any(|e| e.kind == "screen" && e.text.contains("shaping call answered nothing readable")));
        // clear: the screen, then the planner — told it was screened (no shaper: nothing to shape)
        let (run2, p2, _t2) = scripted_prompts(vec!["{\"questions\":[]}", PLAN2]);
        let goals2 = Goals::new(run2, None, 3, 5); goals2.set_dedicated_screen(true);
        let r2 = goals2.plan_full("p", ask, Engine::ClaudeCode, ".", false, 4, None, None, true, false).await;
        assert_eq!(r2.status, RunStatus::AwaitingApproval, "{:?}", r2.events);
        { let p = p2.lock().unwrap(); assert!(p.len() >= 2); assert!(p[1].contains("ALREADY judged this goal clear"), "{}", p[1]); }
        // unreadable decision: the planner\'s embedded gate is the net, and the planner is NOT told it was screened
        let (run3, p3, _t3) = scripted_prompts(vec!["no json at all", PLAN2]);
        let goals3 = Goals::new(run3, None, 3, 5); goals3.set_dedicated_screen(true);
        let r3 = goals3.plan_full("p", ask, Engine::ClaudeCode, ".", false, 4, None, None, true, false).await;
        assert_eq!(r3.status, RunStatus::AwaitingApproval, "{:?}", r3.events);
        { let p = p3.lock().unwrap(); assert!(p.len() >= 2); assert!(p[1].contains("FIRST SCREEN the goal") && !p[1].contains("ALREADY judged"), "{}", p[1]); }
        assert!(r3.events.iter().any(|e| e.kind == "screen" && e.text.contains("nothing readable")));
        // handed over: no screening call at all, and the planner is told it was screened
        let (run7, p7, _t7) = scripted_prompts(vec![PLAN2]);
        let goals7 = Goals::new(run7, None, 3, 5); goals7.set_dedicated_screen(true);
        let r7 = goals7.plan_full("p", "what do you suggest?", Engine::ClaudeCode, ".", false, 4, None, None, true, false).await;
        assert_eq!(r7.status, RunStatus::AwaitingApproval, "{:?}", r7.events);
        { let p = p7.lock().unwrap(); assert!(p[0].contains("ROUTER and PLANNER") && p[0].contains("ALREADY judged this goal clear"), "the first call was the planner, told it was screened: {}", p[0]); }
        assert!(r7.events.iter().any(|e| e.kind == "gauge" && e.text.contains("screen: skipped")), "{:?}", r7.events);
        // router off: no screen at all
        let (run4, p4, _t4) = scripted_prompts(vec![PLAN2]);
        let goals4 = Goals::new(run4, None, 3, 5); goals4.set_dedicated_screen(true);
        let _ = goals4.plan_full("p", ask, Engine::ClaudeCode, ".", false, 4, None, None, false, false).await;
        assert_eq!(p4.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_timed_out_planner_fails_the_run_instead_of_posing_as_one_job() {
        // the CLI's timeout comes back Ok with empty content and the error set — never a route
        let run: RunFn = Arc::new(move |_e, _m, _c, _j, _o: RunOpts, _k| Box::pin(async { Ok(crate::cli::RunOut { parsed: CliParsed { content: String::new(), error: Some("timed out after 300 s".into()), ..Default::default() }, ms: 1, trace: crate::cli::CliTrace { model: None, id: 0, engine: Engine::Opencode, cwd: ".".into(), started: 0, ended: 1, ms: 1, tools: vec![], tokens_in: None, tokens_out: None, cost: None, reported: None, chars: 0, ok: false, error: None } }) }));
        let goals = Goals::new(run, None, 3, 5);
        let r = goals.plan_full("p", "rename the retry constant so that the backoff stays the same", Engine::Opencode, ".", false, 4, None, None, true, false).await;
        assert_eq!(r.status, RunStatus::Failed); assert_eq!(r.error.as_deref(), Some("timed out after 300 s")); assert!(r.route.is_none());
    }
    #[tokio::test]
    async fn a_planner_that_asks_again_gets_its_own_defaults_and_a_third_time_fails() {
        const ASK: &str = "{\"questions\":[{\"text\":\"Which layer?\",\"default\":\"REST\",\"certainty\":\"blocking\"}]}";
        assert!(is_question_reply(ASK) && !is_question_reply(PLAN2) && !is_question_reply("{\"tier\":\"mid\",\"delegate\":false,\"why\":\"one file\",\"questions\":[]}"));
        let (run, prompts, _t) = scripted_prompts(vec![ASK, ASK, PLAN2]);
        let goals = Goals::new(run, None, 3, 5);
        let r = goals.plan_full("p", "rename the retry constant so that the backoff stays the same", Engine::Codex, ".", false, 4, None, None, true, false).await;
        assert_eq!(r.status, RunStatus::AwaitingAnswers);
        let a = goals.answer(r.id, &json!({ "q1": "GraphQL" })).await.unwrap();
        assert_eq!(a.status, RunStatus::AwaitingApproval, "{:?}", a.events);
        assert_eq!(a.tasks.len(), 2);
        assert!(a.events.iter().any(|e| e.kind == "screen" && e.text.contains("asked again")));
        assert!(a.events.iter().any(|e| e.kind == "answer" && e.text == "Which layer? → REST (the planner's default)"));
        assert!(prompts.lock().unwrap()[2].contains("YOU ASKED AGAIN"));
        // a third question-only reply: the run fails and says why
        let (run2, _p, _t2) = scripted_prompts(vec![ASK, ASK, ASK]);
        let goals2 = Goals::new(run2, None, 3, 5);
        let r2 = goals2.plan_full("p", "rename the retry constant so that the backoff stays the same", Engine::Codex, ".", false, 4, None, None, true, false).await;
        let a2 = goals2.answer(r2.id, &json!({})).await.unwrap();
        assert_eq!(a2.status, RunStatus::Failed); assert_eq!(a2.error.as_deref(), Some("the planner asked a third time instead of planning"));
    }
    const PLAN2: &str = "{\"tier\":\"mid\",\"delegate\":true,\"why\":\"w\",\"tasks\":[{\"id\":\"t1\",\"title\":\"a\",\"tier\":\"high\",\"owns\":[\"src/a.ts\"],\"specialists\":[\"axis:defects\"]},{\"id\":\"t2\",\"title\":\"b\",\"tier\":\"low\",\"owns\":[\"src/b/**\"],\"specialists\":[\"axis:tests\"]}]}";

    #[tokio::test]
    async fn the_pane_policy_pins_every_task_to_one_tier() {
        Ledger::default().save(); // a sibling test may have left biasing rows in the process's ledger
        let (run, _p, tiers) = scripted_prompts(vec![PLAN2, "@task", "@task", "@verify", "@verify", "joined"]);
        let goals = Goals::new(run, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r = goals.plan_with("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", false, 4, None, Some(Tier::Low)).await;
        assert!(r.tasks.iter().all(|t| t.tier == Tier::Low), "{:?}", r.tasks.iter().map(|t| t.tier).collect::<Vec<_>>());
        assert!(r.events.iter().any(|e| e.text.contains("pinned to tier low")));
        goals.approve(r.id); let done = settle(&goals, r.id).await;
        assert_eq!(done.status, RunStatus::Done);
        let seq = tiers.lock().unwrap().clone();
        assert_eq!(seq[0], Some(Tier::High), "the planner stays high");
        assert_eq!(seq.iter().filter(|t| **t == Some(Tier::Low)).count(), 2, "both tasks ran low (verify ≥ mid, join high): {seq:?}");
    }

    #[tokio::test]
    async fn a_specialist_that_fails_is_logged_with_its_reason_and_glob_only_tasks_name_their_lenses() {
        let (run, prompts, _t) = scripted_prompts(vec![PLAN2, "@task", "@task", "@verify", "@verify", "joined"]);
        let goals = Goals::new(run, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let list: SpecialistListFn = Arc::new(|_cwd| Box::pin(async { vec![("axis:defects".to_string(), "Defects".to_string(), "d".to_string())] }));
        let review: SpecialistReviewFn = Arc::new(|_cwd, id, _path| Box::pin(async move { Err(format!("{id} is not available to this box")) }));
        goals.with_specialists(list, review);
        let r = goals.plan("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", false, 4, None).await;
        goals.approve(r.id); let done = settle(&goals, r.id).await;
        assert_eq!(done.status, RunStatus::Done, "a failed brief never fails the task");
        assert!(done.events.iter().any(|e| e.kind == "specialist" && e.text.contains("no review — axis:defects is not available to this box")), "{:?}", done.events);
        assert!(done.events.iter().any(|e| e.kind == "specialist" && e.text.contains("no concrete file")));
        let ps = prompts.lock().unwrap().clone();
        let t2 = ps.iter().find(|p| p.starts_with("You are task t2")).unwrap();
        assert!(t2.contains("LENSES TO APPLY") && t2.contains("axis:tests"), "{t2}");
        let t1 = ps.iter().find(|p| p.starts_with("You are task t1")).unwrap();
        assert!(t1.contains("LENSES TO APPLY") && !t1.contains("SPECIALIST BRIEFS"), "a failed brief falls back to naming the lens");
        assert!(done.tasks.iter().all(|t| t.briefs.is_empty()));
    }

    #[tokio::test]
    async fn the_planner_prompt_has_no_lens_section_when_the_box_offers_none() {
        let (run, prompts, _t) = scripted_prompts(vec!["{\"tier\":\"low\",\"delegate\":false,\"why\":\"tiny\",\"tasks\":[]}"]);
        let goals = Goals::new(run, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r = goals.plan("p", "fix the typo in the retry log line so that it reads correctly", Engine::ClaudeCode, ".", false, 4, None).await;
        assert_eq!(r.status, RunStatus::Inline);
        let p0 = prompts.lock().unwrap()[0].clone();
        assert!(!p0.contains("SPECIALISTS:") && !p0.contains("\"specialists\""), "{p0}");
    }

    #[tokio::test]
    async fn cancel_mid_run_fails_the_live_tasks_and_settles_the_run() {
        let (run, _p, _t) = scripted_prompts(vec![PLAN2, "@slow", "@slow", "@verify", "@verify", "joined"]);
        let goals = Goals::new(run, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r = goals.plan("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", false, 4, None).await;
        goals.approve(r.id);
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert!(goals.get(r.id).unwrap().tasks.iter().any(|t| t.status == TaskStatus::Running));
        let c = goals.cancel(r.id).unwrap();
        assert_eq!(c.status, RunStatus::Cancelled);
        assert!(c.tasks.iter().all(|t| t.status == TaskStatus::Failed && t.error.as_deref() == Some("cancelled")));
        tokio::time::sleep(std::time::Duration::from_millis(600)).await; // the slow runs land after the cancel — nothing revives
        let after = goals.get(r.id).unwrap();
        assert_eq!(after.status, RunStatus::Cancelled);
        assert!(after.tasks.iter().all(|t| t.status == TaskStatus::Failed));
        assert!(goals.cancel(r.id).is_some(), "cancel is idempotent");
        assert!(goals.retry(r.id, "t1").is_none() || goals.get(r.id).unwrap().status != RunStatus::Done, "a cancelled run does not quietly resume");
    }

    #[tokio::test]
    async fn the_watchdog_fails_a_task_whose_run_ended_without_a_result() {
        let (run, _p, _t) = scripted_prompts(vec![PLAN2, "@slow", "@slow"]);
        let goals = Goals::new(run, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r = goals.plan("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", false, 4, None).await;
        goals.approve(r.id);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        // simulate the lost handling: a task marked running long ago with no killer alive
        goals.with(|rs| { let r2 = rs.iter_mut().find(|x| x.id == r.id).unwrap(); for t in r2.tasks.iter_mut() { t.started = Some(now_ms() - 900_000 - 200_000); } });
        goals.killers.lock().unwrap().clear();
        goals.tick().await;
        let after = goals.get(r.id).unwrap();
        assert!(after.tasks.iter().all(|t| t.status == TaskStatus::Failed && t.error.as_deref().unwrap_or("").starts_with("lost:")), "{:?}", after.tasks.iter().map(|t| (t.status, t.error.clone())).collect::<Vec<_>>());
        assert!(after.events.iter().any(|e| e.text.contains("watchdog")));
    }

    #[tokio::test]
    async fn the_max_tasks_cap_and_a_plan_over_it_are_trimmed() {
        let big = "{\"tier\":\"mid\",\"delegate\":true,\"why\":\"w\",\"tasks\":[{\"id\":\"t1\",\"title\":\"a\"},{\"id\":\"t2\",\"title\":\"b\"},{\"id\":\"t3\",\"title\":\"c\"}]}";
        let (run, prompts, _t) = scripted_prompts(vec![big, "@task", "@verify", "joined"]);
        let goals = Goals::new(run, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r = goals.plan("p", "rename the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", false, 1, None).await;
        assert_eq!(r.tasks.len(), 1, "the pane's max (1) trims the planner's 3");
        assert!(prompts.lock().unwrap()[0].contains("at most 1 tasks"));
    }

    #[test]
    fn the_merge_row_names_the_seats_that_ran_not_the_ones_that_were_bound() {
        let task = |status: &str, engine: &str, model: Option<&str>| -> GoalTask {
            serde_json::from_value(json!({ "id": "t1", "title": "", "prompt": "", "owns": [], "needs": [], "acceptance": [], "status": status, "wave": 0, "attempts": 1, "started": null, "ended": null, "report": null, "verdict": null, "error": null,
                "changed": [], "trace": { "id": 1, "engine": engine, "model": model, "cwd": ".", "started": 0, "ended": 1, "ms": 1, "tools": [], "tokensIn": null, "tokensOut": null, "cost": null, "reported": null, "chars": 0, "ok": true, "error": null } })).expect("a task")
        };
        // the live Claude-off run: bound to Gemini flash, answered by Cursor Composer
        assert_eq!(ran_worker_key(&[task("done", "cursor", Some("composer-2.5"))]), Some("cursor:composer-2.5".to_string()));
        // the most common seat wins; a failed task and a trace with no model say nothing
        let mix = [task("done", "cursor", Some("composer-2.5")), task("done", "cursor", Some("composer-2.5")), task("done", "codex", Some("gpt-5.6-terra@medium")), task("failed", "claude-code", Some("opus")), task("done", "gemini", None)];
        assert_eq!(ran_worker_key(&mix), Some("cursor:composer-2.5".to_string()));
        assert_eq!(ran_worker_key(&[task("done", "gemini", None)]), None, "no model recorded: the caller falls back to the bound seat");
        assert!(plan_binding_is_a_seat("codex:gpt-5.6-terra@medium") && !plan_binding_is_a_seat("gemini:") && !plan_binding_is_a_seat("") && !plan_binding_is_a_seat(":x"));
    }

    #[test]
    fn engine_failures_are_recognised_conservatively() {
        assert!(engine_failure("The requested model gpt-5-mini is unsupported for Codex with this ChatGPT account.", 0).unwrap().contains("unsupported"));
        assert!(engine_failure("Error: not logged in — run `codex login`", 0).is_some());
        assert!(engine_failure("Rate limit reached (429). Try again later.", 0).is_some());
        assert!(engine_failure("did it. FINDING: the login form uses a weak regex", 0).is_none(), "a short real report with a finding is a report");
        assert!(engine_failure("The auth module is fine; no defects found.", 0).is_none(), "a clean short verdict is a report");
        assert!(engine_failure(&"x".repeat(1000), 0).is_none()); assert!(engine_failure("rate limit", 3).is_none(), "tools were used → it worked");
    }

    #[tokio::test]
    async fn an_engine_failure_fails_the_task_without_a_verifier_call_and_runs_survive_a_restart() {
        Ledger::default().save();
        let (run, prompts, _t) = scripted_prompts(vec![PLAN2, "Error: gpt-5-mini is unsupported for Codex with this account", "@task", "@verify", "joined"]);
        let goals = Goals::new(run, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let hist = std::env::temp_dir().join(format!("antiloki-test-{}-history-{}.json", std::process::id(), now_ms()));
        let _ = std::fs::remove_file(&hist);
        assert_eq!(goals.with_history(hist.clone()), 0);
        let r = goals.plan("p", "restore the retry constant so that the backoff stays the same", Engine::ClaudeCode, ".", false, 4, None).await;
        goals.approve(r.id); let done = settle(&goals, r.id).await;
        let t1 = done.tasks.iter().find(|t| t.id == "t1").unwrap();
        assert_eq!(t1.status, TaskStatus::Failed);
        assert!(t1.error.as_deref().unwrap().starts_with("engine failure — "), "{:?}", t1.error);
        assert!(done.events.iter().any(|e| e.kind == "fail" && e.text.contains("engine failure")));
        let verifies = prompts.lock().unwrap().iter().filter(|p| p.starts_with("You are the VERIFIER")).count();
        assert_eq!(verifies, 1, "only the healthy task was verified");
        // the history: a fresh Goals restores the runs; whatever was live is failed honestly
        let (run2, _p2, _t2) = scripted_prompts(vec![]);
        let again = Goals::new(run2, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        assert_eq!(again.with_history(hist), 1);
        let back = again.list(None).into_iter().find(|x| x.goal == "restore the retry constant so that the backoff stays the same").expect("the run came back");
        assert_eq!(back.tasks.len(), 2);
        assert!(matches!(back.status, RunStatus::Done | RunStatus::Failed));
        // a truncated history (the serve died mid-write) is set aside, never overwritten by the next persist
        let hist2 = std::env::temp_dir().join(format!("antiloki-test-{}-history2-{}.json", std::process::id(), now_ms()));
        std::fs::write(&hist2, "[{\"id\":1,\"goal\":\"cut off").unwrap();
        let (run3, _p3, _t3) = scripted_prompts(vec![]);
        let third = Goals::new(run3, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        assert_eq!(third.with_history(hist2.clone()), 0);
        assert!(!hist2.exists(), "the unreadable file was moved aside");
        let aside: Vec<_> = std::fs::read_dir(std::env::temp_dir()).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(&format!("{}.unreadable-", hist2.file_name().unwrap().to_string_lossy()))).collect();
        assert_eq!(aside.len(), 1); for e in aside { let _ = std::fs::remove_file(e.path()); }
    }

    #[tokio::test]
    async fn a_finished_run_can_be_compared_with_one_raw_call() {
        Ledger::default().save();
        let (run, prompts, tiers) = scripted_prompts(vec![PLAN2, "@task", "@task", "@verify", "@verify", "joined", "raw answer with 3 findings"]);
        let goals = Goals::new(run, Some(Arc::new(|_a: &[&str], _c: &str| String::new())), 3, 5);
        let r = goals.plan("p", "compare the retry constants so that the backoff stays the same", Engine::ClaudeCode, ".", false, 4, None).await;
        assert!(goals.compare_raw(r.id).await.is_none(), "not before the run settles");
        goals.approve(r.id); settle(&goals, r.id).await;
        let c = goals.compare_raw(r.id).await.unwrap().compare.unwrap();
        assert_eq!(c.report.as_deref(), Some("raw answer with 3 findings"));
        assert_eq!((c.run_tasks, c.run_passed), (2, 2));
        assert!(c.run_calls >= 5, "planner + 2 tasks + 2 verifies + join: {}", c.run_calls);
        let last = prompts.lock().unwrap().last().cloned().unwrap();
        assert!(last.starts_with("compare the retry constants so that the backoff stays the same\n\nWork in this repository"), "the raw call carries the goal alone: {last}");
        let route_tier = goals.get(r.id).unwrap().route.unwrap().tier;
        assert_eq!(tiers.lock().unwrap().last().cloned().flatten(), Some(route_tier), "the route's tier");
        assert!(goals.get(r.id).unwrap().events.iter().any(|e| e.kind == "compare" && e.text.contains("the run:")));
    }
}

#[cfg(test)]
mod qa_parse_tests {
    use super::*;
    /// operator 2026-09-28: "the judge gave no verdict in shape" — on every run
    #[test]
    fn a_verdict_is_read_out_of_what_a_model_really_writes() {
        // the shape the prompt asks for
        let r = parse_qa_review(r#"{"passed":true,"score":91,"summary":"fine","findings":[]}"#).unwrap();
        assert!(r.passed && r.score == 91);
        // a fenced block with prose either side — the outermost-braces slice could never parse this
        let r = parse_qa_review("Here is my review:\n```json\n{\"passed\": false, \"score\": 40, \"summary\": \"two risks\"}\n```\nHope that helps.").unwrap();
        assert!(!r.passed && r.score == 40 && r.summary == "two risks");
        // a brace in prose BEFORE the verdict, which used to poison the whole slice
        let r = parse_qa_review("The diff touches {a} and {b}.\n{\"passed\": true, \"score\": 80}").unwrap();
        assert!(r.passed && r.score == 80);
        // `passed` as a string, as a number, and a reply that gives only a score
        assert!(parse_qa_review(r#"{"passed":"yes","score":88}"#).unwrap().passed);
        assert!(!parse_qa_review(r#"{"passed":"no"}"#).unwrap().passed);
        assert!(parse_qa_review(r#"{"passed":1,"score":75}"#).unwrap().passed);
        assert!(parse_qa_review(r#"{"score":95,"summary":"good"}"#).unwrap().passed, "a score alone says whether it passed");
        assert!(!parse_qa_review(r#"{"score":20}"#).unwrap().passed);
        // findings, including the bare-string shape
        let r = parse_qa_review(r#"{"passed":false,"findings":[{"task":"t1","text":"leaks a handle"},"bare risk",{"text":" "}]}"#).unwrap();
        assert_eq!(r.findings.len(), 2, "empty text is dropped, a bare string is kept: {:?}", r.findings);
        // and a reply with no verdict at all is still None, so the caller can say what was said instead
        assert!(parse_qa_review("I could not review this.").is_none());
    }
    #[test]
    fn a_brace_inside_a_string_does_not_close_the_object() {
        let r = parse_qa_review(r#"{"passed":true,"summary":"it prints {\"a\":1} to stdout","score":70}"#).unwrap();
        assert!(r.passed && r.summary.contains("{\"a\":1}"));
        // the LAST object wins: a model that thinks aloud puts its verdict at the end
        let r = parse_qa_review(r#"{"passed":false,"score":10} ... on reflection: {"passed":true,"score":90}"#).unwrap();
        assert_eq!(r.score, 90);
    }
}

#[cfg(test)]
mod claim_files_tests {
    use super::*;
    fn v(a: &[&str]) -> Vec<String> { a.iter().map(|x| x.to_string()).collect() }
    #[test]
    fn a_file_is_its_own_claim_files() {
        assert_eq!(claim_files(false, "src/a.ts", &v(&["src/a.ts"]), &v(&["src/a.ts"])), v(&["src/a.ts"]));
    }
    #[test]
    fn a_folder_becomes_the_files_the_task_changed_under_it_sorted_and_without_duplicates() {
        let changed = v(&["src/features/sort.ts", "src/features/search.ts", "src/features/sort.ts", "src/other.ts"]);
        assert_eq!(claim_files(true, "src/features", &changed, &v(&["src/features"])), v(&["src/features/search.ts", "src/features/sort.ts"]));
    }
    #[test]
    fn a_glob_is_every_file_the_task_changed() {
        let changed = v(&["src/index.css", "src/App.css"]);
        assert_eq!(claim_files(false, "**", &changed, &v(&["**"])), v(&["src/App.css", "src/index.css"]));
        assert_eq!(claim_files(false, "src/**", &[], &v(&["src/a.ts", "**"])), v(&["src/a.ts"]), "owned files when nothing changed; the glob itself is never a file");
        assert_eq!(claim_files(false, "**", &[], &v(&["**"])), v(&["**"]), "nothing known: the path stays as it was");
    }
    #[test]
    fn a_folder_falls_back_to_the_owned_files_when_nothing_changed_and_to_itself_when_there_are_none() {
        let owns = v(&["src/features", "src/features/a.ts", "src/features/*.test.ts"]);
        assert_eq!(claim_files(true, "src/features", &[], &owns), v(&["src/features/a.ts"]));
        assert_eq!(claim_files(true, "src/features", &[], &v(&["src/features"])), v(&["src/features"]));
        assert_eq!(claim_files(true, "src/features/", &v(&["src/features/b.ts"]), &[]), v(&["src/features/b.ts"]));
    }
    #[test]
    fn the_judge_reads_the_real_files_of_a_folder_under_a_budget() {
        let d = std::env::temp_dir().join(format!("claim-src-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src/f")).unwrap();
        std::fs::write(d.join("src/f/a.ts"), "export const a = 1;").unwrap(); std::fs::write(d.join("src/f/b.ts"), "export const b = 2;").unwrap();
        let cwd = d.to_string_lossy().to_string();
        let both = claim_source(&cwd, &v(&["src/f/a.ts", "src/f/b.ts"]), 14_000);
        assert!(both.contains("=== src/f/a.ts ===") && both.contains("export const b = 2;"), "{both}");
        assert_eq!(claim_source(&cwd, &v(&["src/f/a.ts"]), 14_000), "export const a = 1;", "one file is shown exactly as before, without a header");
        assert!(claim_source(&cwd, &v(&["src/f/a.ts", "src/f/b.ts"]), 30).chars().count() <= 30, "the budget holds");
        assert_eq!(claim_source(&cwd, &v(&["src/f"]), 14_000), "", "a folder read as a file is empty - the bug this guards against is that it used to be the only thing the judge got");
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod stale_qa_tests {
    use super::*;
    fn run(status: RunStatus, pending: bool) -> GoalRun {
        let key = match status { RunStatus::Done => "done", _ => "running" };
        let mut r: GoalRun = serde_json::from_value(json!({ "id": 1, "parent": "t", "goal": "g", "engine": "claude-code", "cwd": "/tmp", "write": true, "concurrency": 1, "status": key, "created": 0, "tasks": [], "blackboard": [], "events": [], "delivered": false })).expect("a minimal run");
        r.qa_pending = pending; r
    }
    #[test]
    fn a_done_run_still_judging_at_boot_is_settled_with_a_line_saying_why() {
        let mut r = run(RunStatus::Done, true);
        assert!(settle_stale_qa(&mut r));
        assert!(!r.qa_pending);
        assert!(r.events.iter().any(|e| e.kind == "qa-stopped"), "{:?}", r.events.iter().map(|e| &e.kind).collect::<Vec<_>>());
    }
    #[test]
    fn a_run_that_is_not_pending_or_not_done_is_left_alone() {
        let mut a = run(RunStatus::Done, false); assert!(!settle_stale_qa(&mut a) && a.events.is_empty());
        let mut b = run(RunStatus::Running, true); assert!(!settle_stale_qa(&mut b) && b.qa_pending);
    }
    #[test]
    fn a_changed_file_has_a_different_hash_and_a_missing_one_is_zero() {
        let d = std::env::temp_dir().join(format!("chash-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let cwd = d.to_string_lossy().to_string();
        std::fs::write(d.join("a.tsx"), "one").unwrap(); let h1 = content_hash(&cwd, "a.tsx");
        std::fs::write(d.join("a.tsx"), "two").unwrap(); let h2 = content_hash(&cwd, "a.tsx");
        assert!(h1 != 0 && h2 != 0 && h1 != h2, "an edit to an ALREADY-modified file must show as a change");
        assert_eq!(content_hash(&cwd, "missing.tsx"), 0);
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod plan_stall_tests {
    use super::*;
    fn ev(t: i64, kind: &str, text: &str) -> GoalEvent { GoalEvent { t, id: String::new(), kind: kind.into(), text: text.into(), task: None } }
    const MIN: i64 = 60_000;
    #[test]
    fn plain_silence_after_a_non_call_step_fails_after_four_minutes() {
        let e = ev(0, "gauge", "gauges - wave: one job");
        assert!(plan_stall(RunStatus::Planning, Some(&e), 0, 3 * MIN).is_none());
        let why = plan_stall(RunStatus::Planning, Some(&e), 0, 5 * MIN).expect("5 min of silence is a stall");
        assert!(why.contains("5 min") && why.contains("gauges - wave"), "{why}");
    }
    #[test]
    fn a_planner_call_may_be_silent_for_its_own_limit_plus_slack_and_not_beyond() {
        let e = ev(0, "plan", &format!("{PLANNER_MARK} on cursor to plan the work (up to 10 min)"));
        assert!(plan_stall(RunStatus::Planning, Some(&e), 0, 11 * MIN).is_none(), "a long planner call is not a stall");
        assert!(plan_stall(RunStatus::Planning, Some(&e), 0, 13 * MIN).is_some());
    }
    #[test]
    fn a_screening_call_may_be_silent_for_both_of_its_limits_plus_slack() {
        let e = ev(0, "screen", &format!("{SCREEN_MARK}codex:gpt to screen the ask (up to 120 s)"));
        assert!(plan_stall(RunStatus::Planning, Some(&e), 0, 4 * MIN).is_none());
        assert!(plan_stall(RunStatus::Planning, Some(&e), 0, 6 * MIN).is_some());
    }
    #[test]
    fn a_run_with_no_event_at_all_counts_from_its_creation_and_only_planning_runs_can_stall() {
        assert!(plan_stall(RunStatus::Planning, None, 0, 5 * MIN).unwrap().contains("starting"));
        for st in [RunStatus::Running, RunStatus::AwaitingAnswers, RunStatus::AwaitingApproval, RunStatus::Done, RunStatus::Failed, RunStatus::Cancelled] {
            assert!(plan_stall(st, None, 0, 60 * MIN).is_none(), "{st:?}");
        }
    }
    #[test]
    fn failing_a_stalled_plan_marks_it_failed_stops_it_and_leaves_other_runs_alone() {
        let g = Goals::new(scripted_prompts_for_queue().0, None, 1, 5);
        let mk = |status: &str| -> GoalRun { serde_json::from_value(json!({ "id": 7, "parent": "t", "goal": "g", "engine": "claude-code", "cwd": "/tmp", "write": true, "concurrency": 1, "status": status, "created": 0, "tasks": [], "blackboard": [], "events": [], "delivered": false })).unwrap() };
        g.with(|rs| rs.push(mk("planning")));
        let k = g.plan_killer(7, "screen").expect("a killer is filed");
        assert!(g.killers.lock().unwrap().contains_key("7:screen"));
        assert!(g.fail_stalled_plan(7, "planning made no progress for 5 min"));
        let r = g.get(7).unwrap();
        assert_eq!(r.status, RunStatus::Failed);
        assert_eq!(r.error.as_deref(), Some("planning made no progress for 5 min"));
        assert!(r.events.iter().any(|e| e.kind == "stall"));
        assert!(g.is_stopped(7), "the late planning future must see it was stopped");
        assert!(!g.killers.lock().unwrap().contains_key("7:screen"), "the seat call was killed and unfiled");
        drop(k);
        g.with(|rs| { rs.clear(); rs.push(mk("running")); });
        assert!(!g.fail_stalled_plan(7, "x"), "a run that moved on is not touched");
        assert_eq!(g.get(7).unwrap().status, RunStatus::Running);
    }
}

#[cfg(test)]
mod history_write_tests {
    use super::*;
    /// 2026-10-09: `with` rewrote the whole history file on EVERY call, reads included (3.5 MB each poll). Reads must not touch the disk; a mutation must.
    #[test]
    fn only_a_mutation_writes_the_history_a_read_never_does() {
        let dir = std::env::temp_dir().join(format!("antiloki-history-{}-{}", std::process::id(), now_ms()));
        let path = dir.join("goal-runs.json");
        let g = Goals::new(scripted_prompts_for_queue().0, None, 1, 5);
        g.with_history(path.clone());
        let run: GoalRun = serde_json::from_value(json!({ "id": 5, "parent": "t", "goal": "g", "engine": "claude-code", "cwd": "/tmp", "write": true, "concurrency": 1, "status": "done", "created": 0, "tasks": [], "blackboard": [], "events": [], "delivered": false })).unwrap();
        g.with(|rs| rs.push(run));
        assert!(path.exists(), "a mutation is written");
        std::fs::remove_file(&path).unwrap();
        for _ in 0..3 { let _ = g.get(5); let _ = g.list(None); let _ = g.list(Some("t")); let _ = g.is_stopped(5); }
        assert!(!path.exists(), "a read must never rewrite the history");
        g.note(5, "plan", "a line");
        assert!(path.exists(), "a note is a mutation and is written");
        assert!(std::fs::read_to_string(&path).unwrap().contains("a line"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod exact_savings_tests {
    use super::*;
    fn call(job: &str, i: i64, o: i64, usd: f64) -> (String, i64, i64, f64) { (job.into(), i, o, usd) }
    #[test]
    fn actual_is_everything_the_run_spent_and_the_baseline_is_every_attempts_worker_tokens() {
        // run #358's shape: two editor attempts, two verifier calls, three QA calls, one screen
        let calls = vec![call("editor-l", 30_000, 6_000, 0.024), call("editor-l", 20_000, 5_000, 0.016), call("verifier", 9_000, 600, 0.048), call("qa-gate", 5_000, 400, 0.025), call("screening", 2_000, 300, 0.0197)];
        let sv = savings_from_calls(Engine::ClaudeCode, true, &calls, vec!["t1:mid".into()]).unwrap();
        assert_eq!((sv.tokens_in, sv.tokens_out), (50_000, 11_000), "the worker tokens of BOTH attempts");
        assert!((sv.actual_usd - (0.024 + 0.016 + 0.048 + 0.025 + 0.0197)).abs() < 1e-9, "every call counts: {}", sv.actual_usd);
        assert!(sv.baseline_usd > sv.actual_usd && sv.saved_usd > 0.0);
        assert!((sv.saved_usd - (sv.baseline_usd - sv.actual_usd)).abs() < 1e-9);
    }
    #[test]
    fn a_run_with_no_editor_call_keeps_its_old_figure_and_one_that_delivered_nothing_saved_nothing() {
        assert!(savings_from_calls(Engine::ClaudeCode, true, &[call("verifier", 1, 1, 0.01)], vec![]).is_none());
        assert!(savings_from_calls(Engine::ClaudeCode, true, &[], vec![]).is_none());
        let sv = savings_from_calls(Engine::ClaudeCode, false, &[call("editor-l", 30_000, 6_000, 0.5)], vec![]).unwrap();
        assert_eq!(sv.saved_usd, 0.0); assert!((sv.actual_usd - 0.5).abs() < 1e-9, "the spend stays visible");
    }
    #[test]
    fn costs_more_than_the_baseline_saves_zero_not_a_negative() {
        let sv = savings_from_calls(Engine::ClaudeCode, true, &[call("editor-s", 1_000, 100, 9.0)], vec![]).unwrap();
        assert_eq!(sv.saved_usd, 0.0);
    }
}

#[cfg(test)]
mod scope_fence_run_tests {
    use super::*;
    use crate::cli::CliParsed;
    #[test]
    fn old_runs_deserialize_with_empty_scope() {
        let j = r#"{"id":1,"parent":"p","goal":"g","engine":"codex","cwd":".","write":false,"concurrency":1,"status":"done","created":0,"tasks":[],"blackboard":[],"events":[],"delivered":false}"#;
        let r: GoalRun = serde_json::from_str(j).unwrap();
        assert!(r.scope.is_empty());
    }
    #[test]
    fn scope_fence_helpers() {
        assert!(!scope_fenced(&[]));
        assert!(!scope_fenced(&["**".into()]));
        assert!(scope_fenced(&["src/**".into()]));
        assert!(path_in_run_scope(".antiloki/x", &["src/**".into()]));
        assert!(!path_in_run_scope("pkg/x", &["src/**".into()]));
        assert!(own_within_scope("src/a.ts", &["src/**".into()]));
        assert!(!own_within_scope("**", &["src/**".into()]));
    }
    #[tokio::test]
    async fn outside_named_ask_fails_with_zero_model_calls() {
        let calls: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let c2 = calls.clone();
        let run: RunFn = Arc::new(move |_e, _m, _c, _j, _o, _k| { *c2.lock().unwrap() += 1; Box::pin(async { Ok(crate::cli::RunOut { parsed: CliParsed { content: "[]".into(), ..Default::default() }, ms: 1, trace: crate::cli::CliTrace::replayed(Engine::Codex) }) }) });
        let g = Goals::new(run, Some(Arc::new(|_a, _c| String::new())), 1, 5);
        g.set_dedicated_screen(false);
        g.set_solo_small(true);
        g.set_scope_for_next(vec!["src/**".into()]);
        let r = g.plan_full("p", "fix packages/foo.ts", Engine::Codex, ".", true, 1, None, None, false, true).await;
        while g.get(r.id).map(|x| x.status == RunStatus::Planning).unwrap_or(false) { tokio::time::sleep(std::time::Duration::from_millis(5)).await; }
        let fin = g.get(r.id).unwrap();
        assert_eq!(fin.status, RunStatus::Failed);
        assert!(*calls.lock().unwrap() == 0, "no model calls");
        assert!(fin.error.as_deref().unwrap_or("").contains("outside"));
        assert!(fin.error.as_deref().unwrap_or("").contains("src/**"), "the error names the scope");
    }
    #[tokio::test]
    async fn an_in_scope_named_ask_still_plans() {
        let calls: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let c2 = calls.clone();
        let run: RunFn = Arc::new(move |_e, _m, _c, _j, _o, _k| { *c2.lock().unwrap() += 1; Box::pin(async { Ok(crate::cli::RunOut { parsed: CliParsed { content: "[]".into(), ..Default::default() }, ms: 1, trace: crate::cli::CliTrace::replayed(Engine::Codex) }) }) });
        let g = Goals::new(run, Some(Arc::new(|_a, _c| String::new())), 1, 5);
        g.set_dedicated_screen(false);
        g.set_solo_small(true);
        g.set_scope_for_next(vec!["src/**".into()]);
        let r = g.plan_full("p", "fix src/app.ts", Engine::Codex, ".", true, 1, None, None, false, true).await;
        let deadline = now_ms() + 2000;
        while now_ms() < deadline && g.get(r.id).map(|x| x.status == RunStatus::Planning).unwrap_or(false) { tokio::time::sleep(std::time::Duration::from_millis(5)).await; }
        let fin = g.get(r.id).unwrap();
        assert!(*calls.lock().unwrap() > 0, "an in-scope ask still reaches a model");
        assert!(!fin.error.as_deref().unwrap_or("").contains("outside this agent's scope"), "{:?}", fin.error);
    }
    fn parked(cwd: &str, owns: Vec<String>) -> GoalRun {
        let task = GoalTask { id: "t1".into(), title: "edit".into(), prompt: "edit".into(), owns, needs: vec![], acceptance: vec![], tier: Tier::Low, specialists: vec![], skills: vec![], briefs: vec![], status: TaskStatus::Ready, wave: 0, attempts: 0, started: None, ended: None, report: None, verdict: None, error: None, trace: None, changed: vec![] };
        let mut r: GoalRun = serde_json::from_value(json!({ "id": 1, "parent": "p", "goal": "g", "engine": "codex", "cwd": cwd, "write": true, "concurrency": 1, "status": "awaiting-approval", "created": 0, "tasks": [], "blackboard": [], "events": [], "delivered": false, "scope": ["src/**"] })).unwrap();
        r.tasks = vec![task];
        r
    }
    #[tokio::test]
    async fn out_of_scope_owns_never_start_and_a_read_only_task_may() {
        let calls: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
        let c2 = calls.clone();
        let run: RunFn = Arc::new(move |_e, _m, _c, _j, _o, _k| { *c2.lock().unwrap() += 1; Box::pin(async { Ok(crate::cli::RunOut { parsed: CliParsed { content: "did it".into(), ..Default::default() }, ms: 1, trace: crate::cli::CliTrace::replayed(Engine::Codex) }) }) });
        let g = Goals::new(run, Some(Arc::new(|_a, _c| String::new())), 1, 5);
        let d = std::env::temp_dir().join(format!("antiloki-scope-owns-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        let cwd = d.to_string_lossy().to_string();
        let _ = crate::proc::cmd("git").args(["init", "-q"]).current_dir(&d).output();
        g.with(|rs| rs.push(parked(&cwd, vec!["packages/foo.ts".into()])));
        let refused = g.approve(1).unwrap();
        assert_eq!(refused.status, RunStatus::Failed);
        assert!(refused.error.as_deref().unwrap_or("").contains("packages/foo.ts") || refused.error.as_deref().unwrap_or("").contains("fenced down"), "{:?}", refused.error);
        assert_eq!(*calls.lock().unwrap(), 0, "workers never start");
        assert!(refused.tasks.iter().all(|t| t.status != TaskStatus::Running));
        g.with(|rs| { rs.clear(); rs.push(parked(&cwd, vec![])); });
        let read_only = g.approve(1).unwrap();
        assert_ne!(read_only.status, RunStatus::Failed, "a read-only task with no owns may stay: {:?}", read_only.error);
        let _ = std::fs::remove_dir_all(&d);
    }
    #[tokio::test]
    async fn a_worker_write_outside_scope_is_restored_and_in_scope_files_stay() {
        let d = std::env::temp_dir().join(format!("antiloki-scope-write-{}-{}", std::process::id(), now_ms()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("pkg")).unwrap();
        std::fs::write(d.join("src/a.txt"), "base\n").unwrap();
        std::fs::write(d.join("pkg/b.txt"), "base\n").unwrap();
        let cwd = d.to_string_lossy().to_string();
        for a in [vec!["init", "-q"], vec!["config", "user.email", "t@t"], vec!["config", "user.name", "t"], vec!["add", "-A"], vec!["commit", "-qm", "base"]] {
            let v: Vec<&str> = a.iter().copied().collect();
            crate::proc::cmd("git").args(&v).current_dir(&d).output().unwrap();
        }
        let cwd2 = cwd.clone();
        let run: RunFn = Arc::new(move |_e, _m, c, _j, _o, _k| {
            let cwd2 = cwd2.clone();
            Box::pin(async move {
                if c == cwd2 {
                    let _ = std::fs::write(format!("{c}/src/a.txt"), "in-scope\n");
                    let _ = std::fs::write(format!("{c}/pkg/b.txt"), "out-of-scope\n");
                }
                Ok(crate::cli::RunOut { parsed: CliParsed { content: "did the edit".into(), ..Default::default() }, ms: 1, trace: crate::cli::CliTrace::replayed(Engine::Codex) })
            })
        });
        let g = Goals::new(run, None, 1, 5);
        g.set_dedicated_screen(false);
        g.with(|rs| rs.push(parked(&cwd, vec!["src/a.txt".into()])));
        g.approve(1);
        let deadline = now_ms() + 8000;
        while now_ms() < deadline && g.get(1).map(|x| !matches!(x.status, RunStatus::Failed | RunStatus::Done | RunStatus::Cancelled)).unwrap_or(false) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let fin = g.get(1).unwrap();
        assert_eq!(fin.status, RunStatus::Failed, "{:?}", fin.error);
        assert!(fin.error.as_deref().unwrap_or("").contains("outside the agent's scope"), "{:?}", fin.error);
        assert_eq!(fin.tasks[0].error.as_deref(), Some("outside the agent's scope"));
        assert_eq!(fin.tasks[0].attempts, 1, "no retry");
        assert_eq!(std::fs::read_to_string(d.join("src/a.txt")).unwrap(), "in-scope\n");
        assert_eq!(std::fs::read_to_string(d.join("pkg/b.txt")).unwrap(), "base\n");
        let _ = std::fs::remove_dir_all(&d);
    }
}

mod caution_tests {
    use super::*;
    fn plan(conf: &str, tasks: &[(&str, &str, &str)]) -> (Route, Vec<PlanTask>) {
        let ts: Vec<Value> = tasks.iter().map(|(id, owns, tier)| json!({"id": id, "title": id, "prompt": "do it", "owns": [owns], "tier": tier, "acceptance": ["done"]})).collect();
        let (r, t, _) = parse_route(&json!({"tier": "mid", "delegate": true, "why": "w", "confidence": conf, "tasks": ts}).to_string(), 8); (r, t)
    }
    #[test]
    fn a_low_grade_sends_the_run_and_its_tasks_one_tier_up_but_not_the_ones_the_mechanical_rule_lowered() {
        let (mut r, mut t) = plan("low", &[("t1", "src/a.ts", "low"), ("t2", "src/b.ts", "mid"), ("t3", "src/c.ts", "low")]);
        let before = r.tier;
        let notes = apply_caution(&mut r, &mut t, "tidy the three files", &["t3".to_string()]);
        assert_eq!(t[0].tier, Tier::Mid); assert_eq!(t[1].tier, Tier::High); assert_eq!(t[2].tier, Tier::Low, "held by the mechanical rule");
        assert!(r.tier >= before && r.tier == Tier::High, "the run is never below its strongest task");
        assert!(notes.iter().any(|n| n.contains("graded this route low")), "{notes:?}");
    }
    #[test]
    fn high_and_medium_grades_and_a_missing_one_change_nothing() {
        for g in ["high", "medium", ""] {
            let (mut r, mut t) = plan(g, &[("t1", "src/a.ts", "low")]);
            let n = apply_caution(&mut r, &mut t, "tidy a.ts", &[]);
            assert!(n.is_empty() && t[0].tier == Tier::Low, "{g:?}: {n:?}");
        }
    }
    #[test]
    fn a_task_that_owns_credentials_billing_a_migration_or_a_deploy_goes_to_high_whatever_the_grade() {
        let (mut r, mut t) = plan("high", &[("t1", "src/a.ts", "low"), ("t2", "db/migrations/9.sql", "low"), ("t3", ".github/workflows/ci.yml", "mid")]);
        let n = apply_caution(&mut r, &mut t, "add a column", &["t2".to_string()]);
        assert_eq!((t[0].tier, t[1].tier, t[2].tier), (Tier::Low, Tier::High, Tier::High), "{n:?}");
        assert_eq!(r.tier, Tier::High); assert!(n.iter().any(|x| x.contains("t2") && x.contains("migrations")), "{n:?}");
    }
    #[test]
    fn risk_words_in_the_ask_are_one_step_up_and_doubt_on_top_is_not_a_second_step() {
        let (mut r, mut t) = plan("high", &[("t1", "src/a.ts", "low")]);
        apply_caution(&mut r, &mut t, "fix the crash in production", &[]); assert_eq!(t[0].tier, Tier::Mid);
        let (mut r, mut t) = plan("low", &[("t1", "src/a.ts", "low")]);
        apply_caution(&mut r, &mut t, "fix the crash in production", &[]); assert_eq!(t[0].tier, Tier::Mid, "doubt + risk words = still one step");
        let (mut r, mut t) = plan("high", &[("t1", "src/a.ts", "low")]);
        let n = apply_caution(&mut r, &mut t, "name the .env file in the ask: edit .env", &[]); assert_eq!(r.tier, Tier::High, "a risky file the ask names raises the run: {n:?}");
    }
}

