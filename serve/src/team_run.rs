//! TEAMS ON THE SERVE (operator 2026-11-10): the team store, the run registry, the real panes behind the `team::Io`, the sagas that record every step, and the door the screen and the MCP use.
//!
//!   POST /api/v1/cockpit/teams   {op: list | create | link | unlink | delete | run | status | stop, ...}
//!
//! A team lives in `<project>/.antiloki/teams.json` (up to one pane per role); a run's record in `<project>/.antiloki/team-runs/<id>.json`, and in the sagas store as ONE saga with a task per step (the plan, each
//! attempt's work, each attempt's QA), each with its own price: the deterministic timeline the person reads. The loop's decisions are `team.rs`'s; this only carries them out.
use crate::team::{self, Team, Io, Outcome, Reply, Role, Run};
use crate::dispatch::AppState;
use crate::pty::PtyRegistry;
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How long one role may take over one turn before the run stops and says so.
const TURN_TIMEOUT: Duration = Duration::from_secs(25 * 60);
/// What an agent keeps in its box that is not the work: never part of a delivery, never a QA's edit.
pub(crate) const NOT_WORK: [&str; 9] = [".antiloki", ".claude", ".agents", ".opencode", ".codex", "CLAUDE.md", "AGENTS.md", ".mcp.json", "opencode.json"];

fn s(v: &Value) -> String { v.as_str().unwrap_or("").trim().to_string() }
fn reply(v: Value) -> Response { (StatusCode::OK, Json(v)).into_response() }
fn err(e: impl std::fmt::Display) -> Response { reply(json!({ "ok": false, "error": e.to_string() })) }

// ── the store ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

pub fn teams_path(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("teams.json") }
/// The file an older build called these "crews" (RFC 0008: the word is team now): read when there is no `teams.json` yet, and left where it is; the next save writes `teams.json`.
fn legacy_path(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("crews.json") }
pub fn load_teams(root: &str) -> Vec<Team> { std::fs::read_to_string(teams_path(root)).or_else(|_| std::fs::read_to_string(legacy_path(root))).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default() }
pub fn save_teams(root: &str, teams: &[Team]) -> Result<(), String> {
    let p = teams_path(root); if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    std::fs::write(&p, serde_json::to_string_pretty(teams).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}
fn runs_dir(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("team-runs") }
fn save_run(root: &str, id: &str, v: &Value) { let d = runs_dir(root); let _ = std::fs::create_dir_all(&d); let _ = std::fs::write(d.join(format!("{id}.json")), v.to_string()); }
pub fn load_runs(root: &str) -> Vec<Value> {
    let old = Path::new(root).join(".antiloki").join("crew-runs");
    let mut v: Vec<Value> = std::fs::read_dir(runs_dir(root)).into_iter().flatten().flatten().chain(std::fs::read_dir(old).into_iter().flatten().flatten()).filter_map(|e| serde_json::from_str::<Value>(&std::fs::read_to_string(e.path()).ok()?).ok()).map(in_new_words).collect();
    v.sort_by(|a, b| b["started_ms"].as_i64().cmp(&a["started_ms"].as_i64())); v
}

/// A team's id from its name: lower case words and dashes. Pure.
pub fn slug(name: &str) -> String { let t: String = name.trim().to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect(); t.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-") }

/// The ops that change teams, over a list of them. Pure: the door only loads and saves.
pub fn apply(teams: &mut Vec<Team>, b: &Value) -> Result<Value, String> {
    let find = |teams: &Vec<Team>, k: &str| teams.iter().position(|c| c.id == slug(k) || c.name.eq_ignore_ascii_case(k.trim()));
    match s(&b["op"]).as_str() {
        "create" => {
            let name = s(&b["name"]); let id = slug(&name); if id.is_empty() { return Err("a team needs a name".into()); }
            if teams.iter().any(|c| c.id == id) { return Err(format!("a team called `{id}` exists")); }
            teams.push(Team { id: id.clone(), name, ..Default::default() }); Ok(json!({ "team": id }))
        }
        // A PAIR: one agent dragged into another. The one dropped ON is the planner (and judges its own planning: the QA), the dragged one is the worker. Named `Pair`, `Pair 2`, … unless the person says.
        "pair" => {
            let (planner, worker) = (b["planner"].as_i64().filter(|p| *p > 0).ok_or("`planner` is the pane number of the agent that plans (and judges)")?, b["worker"].as_i64().filter(|p| *p > 0).ok_or("`worker` is the pane number of the agent that works")?);
            if planner == worker { return Err("a pair is two different agents".into()); }
            for p in [planner, worker] { if let Some(c) = teams.iter().find(|c| c.members().iter().any(|(_, q)| *q == p)) { return Err(format!("pane {p} belongs to `{}` already: unlink it first", c.name)); } }
            let want: String = s(&b["name"]).split_whitespace().collect::<Vec<_>>().join(" "); let mut name = if want.is_empty() { "Pair".to_string() } else { want.chars().take(60).collect() }; let mut n = 1;
            while teams.iter().any(|c| c.id == slug(&name)) { n += 1; name = format!("{} {n}", if want.is_empty() { "Pair" } else { want.as_str() }); }
            let id = slug(&name); teams.push(Team { id: id.clone(), name: name.clone(), planner: Some(planner), worker: Some(worker), qa: None, pair: true });
            Ok(json!({ "team": id, "name": name, "role": "planner", "pane": planner, "worker": worker, "pair": true }))
        }
        "link" => {
            let i = find(teams, &s(&b["team"])).ok_or("no such team: teams op list")?; let role = Role::parse(&s(&b["role"])).ok_or("role is one of: planner, worker, qa")?;
            let pane = b["pane"].as_i64().filter(|p| *p > 0).ok_or("`pane` is the cockpit pane number of the agent to link")?;
            if let Some(other) = teams.iter().enumerate().find(|(j, c)| *j != i && c.members().iter().any(|(_, p)| *p == pane)) { return Err(format!("pane {pane} belongs to the team `{}` already", other.1.id)); }
            teams[i].link(role, pane)?; Ok(json!({ "team": teams[i].id, "role": role.key(), "pane": pane }))
        }
        "unlink" => {
            let pane = b["pane"].as_i64().ok_or("`pane` is required")?;
            for i in 0..teams.len() { if let Some(r) = teams[i].unlink(pane) { let id = teams[i].id.clone(); if teams[i].pair { teams.remove(i); } /* a pair with one left is no pair */ return Ok(json!({ "team": id, "role": r.key(), "pane": pane })); } }
            Err(format!("pane {pane} is in no team"))
        }
        // THE NAME IS THE PERSON'S WORD FOR IT, THE ID IS THE TEAM'S IDENTITY: renaming changes what is shown (and what the planner is told), never which team it is, so runs, sagas and layouts that name the id stay true
        "rename" => {
            let i = find(teams, &s(&b["team"])).ok_or("no such team")?; let name: String = s(&b["name"]).split_whitespace().collect::<Vec<_>>().join(" ").chars().take(60).collect();
            if name.is_empty() { return Err("a team needs a name".into()); }
            if teams.iter().enumerate().any(|(j, c)| j != i && c.name.eq_ignore_ascii_case(&name)) { return Err(format!("another team is already called `{name}`")); }
            teams[i].name = name.clone(); Ok(json!({ "team": teams[i].id, "name": name }))
        }
        "delete" => { let i = find(teams, &s(&b["team"])).ok_or("no such team")?; let c = teams.remove(i); Ok(json!({ "deleted": c.id })) }
        other => Err(format!("op is one of: list, create, pair, link, unlink, rename, delete, run, status, stop (not `{other}`)")),
    }
}

// ── the runs ─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

/// ONE WRITER AT A TIME PER TERMINAL: the team types into a pane from several places (the next run's prompt, the last run's result, a violation notice), and two typings at once interleave their keystrokes.
fn pane_lock(pane: i64) -> Arc<tokio::sync::Mutex<()>> { static L: OnceLock<Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new(); L.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner()).entry(pane).or_default().clone() }
/// Type `text` into a pane, in turn with every other typing the team does into it. Blocking (a run's thread).
fn type_into(rt: &tokio::runtime::Handle, reg: &Arc<PtyRegistry>, pane: i64, text: &str) -> Result<(), String> {
    let (reg, text, lock) = (reg.clone(), text.to_string(), pane_lock(pane));
    rt.block_on(async move { let _turn = lock.lock().await; crate::cowork_mcp::send_to(&reg, pane, &text).await })
}

struct Handle { stop: Arc<AtomicBool>, snap: Arc<Mutex<Value>>, violations: Arc<std::sync::atomic::AtomicU32> }
fn handles() -> &'static Mutex<HashMap<String, Arc<Handle>>> { static H: OnceLock<Mutex<HashMap<String, Arc<Handle>>>> = OnceLock::new(); H.get_or_init(Default::default) }

pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let o = antiloki_rs::proc::cmd("git").args(args).current_dir(dir).output().map_err(|e| format!("git: {e}"))?;
    if !o.status.success() { return Err(format!("git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&o.stderr).trim())); }
    Ok(String::from_utf8_lossy(&o.stdout).to_string())
}
fn excludes() -> Vec<String> { NOT_WORK.iter().map(|n| format!(":(exclude){n}")).collect() }
/// A box's change as a patch: everything tracked or new, but not an agent's own files.
pub fn names_of(dir: &Path) -> Result<Vec<String>, String> {
    let ex = excludes(); let mut a: Vec<&str> = vec!["diff", "HEAD", "--name-only", "--", "."]; a.extend(ex.iter().map(String::as_str));
    let mut v: Vec<String> = git(dir, &a)?.lines().map(String::from).filter(|l| !l.is_empty()).collect();
    v.extend(untracked(dir));   // a brand-new file is not in `git diff HEAD` until it is intent-added, and the diff no longer leaves it so (see `patch_of`)
    v.sort(); v.dedup(); Ok(v)
}
/// The files git does not know yet (not ignored, not an agent's own). Never fails: an error is an empty list.
fn untracked(dir: &Path) -> Vec<String> {
    let ex = excludes(); let mut a: Vec<&str> = vec!["ls-files", "--others", "--exclude-standard", "--", "."]; a.extend(ex.iter().map(String::as_str));
    git(dir, &a).map(|o| o.lines().map(String::from).filter(|l| !l.is_empty()).collect()).unwrap_or_default()
}
/// A box's whole change as a patch, new files included. It marks the new files with `git add -N` only to let `git diff HEAD` see them and takes the mark off again: a folder that is the person's own (the workspace) is left as it was found, and a half-added file there
/// stops `git stash` ("Entry not uptodate"), as it did on the operator's workspace on 2026-11-15.
pub fn patch_of(dir: &Path) -> Result<String, String> {
    let ex = excludes(); let fresh = untracked(dir);
    let mut add: Vec<&str> = vec!["add", "-N", "--", "."]; add.extend(ex.iter().map(String::as_str)); let _ = git(dir, &add);
    let mut diff: Vec<&str> = vec!["diff", "HEAD", "--binary", "--no-ext-diff", "--src-prefix=a/", "--dst-prefix=b/", "--", "."]; diff.extend(ex.iter().map(String::as_str)); let out = git(dir, &diff);
    for chunk in fresh.chunks(200) { let mut r: Vec<&str> = vec!["reset", "-q", "--"]; r.extend(chunk.iter().map(String::as_str)); let _ = git(dir, &r); }
    out
}
/// A patch as a short fingerprint; empty for no change. Pure.
pub fn fingerprint(patch: &str) -> String { if patch.trim().is_empty() { String::new() } else { hex::encode(Sha256::digest(patch.as_bytes()))[..16].to_string() } }

struct PaneIo { st: Arc<AppState>, reg: Arc<PtyRegistry>, rt: tokio::runtime::Handle, team: Team, run_id: String, root: String, saga: i64, stop: Arc<AtomicBool>, snap: Arc<Mutex<Value>>, violations: Arc<std::sync::atomic::AtomicU32>, step: u32, patch: String, qa_base: String, started_ms: i64 }

impl PaneIo {
    fn box_of(&self, role: Role) -> Result<crate::work_boxes::BoxRef, String> {
        let pane = self.team.pane(role).ok_or_else(|| format!("the team has no {}", role.key()))?;
        crate::work::boxes_of_project(&self.st).into_iter().find(|b| b.pane_id == pane).ok_or_else(|| format!("the {} (pane {pane}) has no agent box here: open it as an agent first", role.key()))
    }
    fn with_db<T>(&self, f: impl FnOnce(&rusqlite::Connection) -> Result<T, String>) -> Option<T> { crate::work::open(&self.root).ok().and_then(|c| f(&c).ok()) }
    fn step_title(&self, role: Role, attempt: u8) -> String { match role { Role::Planner => "Plan".into(), Role::Worker => format!("Work · attempt {attempt}"), Role::Qa => format!("QA · attempt {attempt}") } }
}

impl PaneIo {
    /// A planner's (or QA's) box after its turn: back to its baseline if it was written to.
    fn guard_box(&mut self, role: Role, dir: &Path, pane: i64) {
        if let Ok(Some(v)) = crate::team_guard::check(dir) { let _ = crate::team_guard::restore(dir); self.violate(role, pane, &v); }
    }
    /// Record a violation by a read-only role: the guard's log, the saga's trail, the run's count, and a word to the agent that made it.
    fn violate(&mut self, role: Role, pane: i64, v: &crate::team_guard::Violation) {
        self.violations.fetch_add(1, Ordering::Relaxed);
        let text = format!("the {} changed code ({}) and it was reverted", role.key(), if v.files.is_empty() { "the box".to_string() } else { v.files.join(", ") });
        crate::team_guard::append_log(&self.root, json!({ "t": crate::cli::now_ms(), "team": self.team.id, "role": role.key(), "pane": pane, "files": v.files, "moved_head": v.moved_head, "run": self.run_id }));
        self.with_db(|c| crate::work::event(c, self.saga, None, "team", "violation", &text));
        let _ = type_into(&self.rt, &self.reg, pane, &crate::team_guard::notice(role.key(), v));
    }
}

impl Io for PaneIo {
    fn ask(&mut self, role: Role, prompt: &str) -> Result<Reply, String> {
        if self.stop.load(Ordering::Relaxed) { return Err("the person stopped the run".into()); }
        if self.violations.load(Ordering::Relaxed) >= 2 { return Err("a read-only role (the planner or the QA) changed code twice in this run: it is stopped, and both were reverted".into()); }
        let b = self.box_of(role)?; self.step += 1;
        let tag = format!("[team {} · step {}]", self.run_id, self.step);
        let attempt = self.snap.lock().map(|g| g["run"]["attempt"].as_u64().unwrap_or(1) as u8).unwrap_or(1);
        let title = self.step_title(role, attempt.max(1));
        let task = self.with_db(|c| { let t = crate::work::create_episode(c, self.saga, None, &title, "", &b.agent, "team")?; crate::work::update_episode(c, t, &json!({ "status": "doing" }), "team")?; Ok(t) });
        let sent = type_into(&self.rt, &self.reg, b.pane_id, &format!("{tag}\n{prompt}"));
        if let Err(e) = sent { if let Some(t) = task { self.with_db(|c| crate::work::update_episode(c, t, &json!({ "status": "failed" }), "team")); } return Err(format!("{}: {e}", role.key())); }
        let t0 = Instant::now();
        loop {
            if self.stop.load(Ordering::Relaxed) { if let Some(t) = task { self.with_db(|c| crate::work::update_episode(c, t, &json!({ "status": "canceled" }), "team")); } return Err("the person stopped the run".into()); }
            let turn = crate::work_boxes::box_episodes(&b).into_iter().flat_map(|e| e.turns).find(|t| t.prompt.contains(&tag) && t.end_ms.is_some());
            if let Some(t) = turn {
                if let Some(id) = task { let (usd, tokens, ans) = (t.usd, t.tokens, t.answer.chars().take(1500).collect::<String>());
                    self.with_db(|c| { crate::work::update_episode(c, id, &json!({ "status": "done", "body": ans }), "team")?; c.execute("UPDATE tasks SET usd_total = ?1, tokens = ?2, engine = ?3 WHERE id = ?4", rusqlite::params![usd, tokens, b.engine, id]).map_err(|e| e.to_string())?; Ok(()) }); }
                // a planner or a QA that wrote to its box during this turn is reverted, and the turn's answer still counts (it is words)
                if matches!(role, Role::Planner) { self.guard_box(role, &b.dir, b.pane_id); }
                return Ok(Reply { text: t.answer, usd: t.usd });
            }
            if t0.elapsed() > TURN_TIMEOUT { if let Some(t) = task { self.with_db(|c| crate::work::update_episode(c, t, &json!({ "status": "failed" }), "team")); } return Err(format!("the {} did not finish its turn in {} minutes", role.key(), TURN_TIMEOUT.as_secs() / 60)); }
            std::thread::sleep(Duration::from_millis(800));
        }
    }
    fn delivery(&mut self) -> Result<String, String> {
        let b = self.box_of(Role::Worker)?; self.patch = patch_of(&b.dir)?;
        // THE SAGA KEEPS WHAT THE WORKER CHANGED (under the work step that made it), in its own sqlite
        let (saga, patch, agent) = (self.saga, self.patch.clone(), b.agent.clone());
        self.with_db(|c| { let task: Option<i64> = c.query_row("SELECT id FROM tasks WHERE saga_id = ?1 AND title LIKE 'Work%' ORDER BY id DESC LIMIT 1", [saga], |r| r.get(0)).ok(); crate::work_changes::record(c, saga, task, &agent, &patch) });
        Ok(fingerprint(&self.patch))
    }
    fn hand_over(&mut self) -> Result<(), String> {
        let Some(_) = self.team.pane(Role::Qa) else { return Ok(()) };
        let q = self.box_of(Role::Qa)?;
        // the QA's box is made to hold exactly the delivery (on top of its baseline): whatever a last round left there goes first
        if crate::team_guard::load_baseline(&q.dir).is_some() { crate::team_guard::restore(&q.dir)?; } else {
            git(&q.dir, &["reset", "--hard", "-q"])?;
            let mut clean: Vec<String> = vec!["clean".into(), "-fdq".into()]; for n in NOT_WORK { clean.push("-e".into()); clean.push(n.into()); }
            git(&q.dir, &clean.iter().map(String::as_str).collect::<Vec<_>>())?;
        }
        if !self.patch.trim().is_empty() {
            let mut child = antiloki_rs::proc::cmd("git").args(["apply", "--whitespace=nowarn", "-"]).current_dir(&q.dir).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn().map_err(|e| e.to_string())?;
            use std::io::Write; child.stdin.take().ok_or("no stdin")?.write_all(self.patch.as_bytes()).map_err(|e| e.to_string())?;
            let o = child.wait_with_output().map_err(|e| e.to_string())?; if !o.status.success() { return Err(format!("the delivery does not apply to the QA's box: {}", String::from_utf8_lossy(&o.stderr).trim())); }
        }
        self.qa_base = fingerprint(&patch_of(&q.dir)?); Ok(())
    }
    fn qa_clean(&mut self) -> Result<bool, String> {
        let q = self.box_of(Role::Qa)?; let clean = fingerprint(&patch_of(&q.dir)?) == self.qa_base;
        if !clean { let v = crate::team_guard::Violation { files: crate::team_guard::changed_files(&self.patch, &patch_of(&q.dir)?), moved_head: false }; self.violate(Role::Qa, q.pane_id, &v); }
        // the judging is over: the QA's box goes back to its baseline (the delivery was only lent to it)
        if crate::team_guard::load_baseline(&q.dir).is_some() { let _ = crate::team_guard::restore(&q.dir); }
        Ok(clean)
    }
    fn note(&mut self, line: &str) { let l = line.to_string(); self.with_db(|c| crate::work::event(c, self.saga, None, "team", "team", &l)); }
    fn progress(&mut self, run: &Run) {
        let v = json!({ "run_id": self.run_id, "team": self.team.id, "task": run.task, "saga": self.saga, "started_ms": self.started_ms, "run": run.summary(), "verdicts": run.verdicts, "plan": run.plan, "ended": false });
        if let Ok(mut g) = self.snap.lock() { *g = v.clone(); } save_run(&self.root, &self.run_id, &v);
    }
}

/// Start a run in the background. The returned value is what the screen shows at once; the rest arrives in the saga and `status`.
pub async fn start_run(state: Arc<AppState>, reg: Arc<PtyRegistry>, root: String, team: Team, task: String, budget: Option<f64>, first_plan: Option<String>, report_back: bool) -> Result<Value, String> {
    team.ready()?; if task.trim().is_empty() { return Err("a run needs the task".into()); }
    let run_id = format!("{}-{}", team.id, crate::cli::now_ms() / 1000 % 1_000_000);
    // every linked pane must be an agent with a box here, now, not halfway through
    let probe = crate::work::boxes_of_project(&state);
    for (role, pane) in team.members() { if !probe.iter().any(|b| b.pane_id == pane) { return Err(format!("the {} (pane {pane}) has no agent box: open it as an agent first", role.key())); } }
    let title: String = task.trim().chars().take(80).collect();
    let (saga, started) = { let (root, run_id, team, title, task) = (root.clone(), run_id.clone(), team.clone(), title.clone(), task.clone());
        tokio::task::spawn_blocking(move || -> Result<(i64, i64), String> {
            let c = crate::work::open(&root)?; let id = crate::work::create_saga(&c, &format!("Team · {title}"), &task, "team")?;
            let meta = json!({ "kind": "team", "team": team.id, "run_id": run_id, "members": team.members().iter().map(|(r, p)| json!({ "role": r.key(), "pane": p })).collect::<Vec<_>>(), "max_attempts": team::MAX_ATTEMPTS });
            c.execute("UPDATE sagas SET plan_ref = ?1, meta = ?2 WHERE id = ?3", rusqlite::params![format!("team:{run_id}"), meta.to_string(), id]).map_err(|e| e.to_string())?;
            Ok((id, crate::cli::now_ms()))
        }).await.map_err(|e| e.to_string())?? };
    let stop = Arc::new(AtomicBool::new(false)); let snap = Arc::new(Mutex::new(json!({ "run_id": run_id, "team": team.id, "saga": saga, "started_ms": started, "ended": false, "run": { "phase": "planning", "attempt": 0 } })));
    let violations = Arc::new(std::sync::atomic::AtomicU32::new(0));
    handles().lock().unwrap_or_else(|p| p.into_inner()).insert(run_id.clone(), Arc::new(Handle { stop: stop.clone(), snap: snap.clone(), violations: violations.clone() }));
    let rt = tokio::runtime::Handle::current(); let out = json!({ "ok": true, "run_id": run_id, "saga": saga, "team": team.id });
    let (rid, root2) = (run_id.clone(), root.clone());
    tokio::task::spawn_blocking(move || {
        let (has_planner, has_qa) = (team.planner.is_some(), team.pane(Role::Qa).is_some());
        let mut run = Run::new(&task, has_planner, has_qa, budget);
        let mut io = PaneIo { st: state, reg, rt, team: team.clone(), run_id: rid.clone(), root: root2.clone(), saga, stop, snap: snap.clone(), violations, step: 0, patch: String::new(), qa_base: String::new(), started_ms: started };
        let outcome = team::drive_with(&mut run, &mut io, first_plan);
        let (kind, why) = match &outcome { Outcome::Done => ("done", String::new()), Outcome::Blocked(w) => ("blocked", w.clone()) };
        io.with_db(|c| { crate::work::event(c, saga, None, "team", kind, &if kind == "done" { format!("the QA accepted the work after {} attempt{}", run.attempt, if run.attempt == 1 { "" } else { "s" }) } else { format!("needs you: {why}") })?;
            c.execute("UPDATE sagas SET meta = json_set(CASE WHEN meta = '' THEN '{}' ELSE meta END, '$.outcome', ?1, '$.blocked', ?2, '$.attempts', ?3, '$.spent_usd', ?4) WHERE id = ?5", rusqlite::params![kind, why, run.attempt as i64, run.spent_usd, saga]).map_err(|e| e.to_string())?; Ok(()) });
        let v = json!({ "run_id": rid, "team": team.id, "task": run.task, "saga": saga, "started_ms": started, "ended": true, "ended_ms": crate::cli::now_ms(), "outcome": kind, "blocked": why, "run": run.summary(), "verdicts": run.verdicts, "plan": run.plan });
        if let Ok(mut g) = snap.lock() { *g = v.clone(); } save_run(&root2, &rid, &v);
        // THE RESULT GOES BACK TO THE PLANNER, the one the person talks to, so it can tell them (a run the person started from the bar reports there too)
        if report_back { if let Some(pane) = team.planner { let msg = result_message(&run, &outcome); let _ = type_into(&io.rt, &io.reg, pane, &msg); } }
    });
    Ok(out)
}

/// The teams door, as a value: the screen's POST and the MCP's tool both come through here.
pub async fn handle(state: Arc<AppState>, reg: Arc<PtyRegistry>, b: Value) -> Value {
    let b = in_new_words(b);
    let root = crate::chat_pane::workspace_root_of(&state); if root.is_empty() { return json!({ "ok": false, "error": "no project is open" }); }
    let fail = |e: String| json!({ "ok": false, "error": e });
    match s(&b["op"]).as_str() {
        "" | "list" => {
            let teams = load_teams(&root); let boxes = crate::work::boxes_of_project(&state);
            let rows: Vec<Value> = teams.iter().map(|c| json!({ "id": c.id, "name": c.name, "pair": c.pair, "members": c.members().iter().map(|(r, p)| json!({ "role": r.key(), "pane": p, "agent": boxes.iter().find(|x| x.pane_id == *p).map(|x| x.agent.clone()), "engine": boxes.iter().find(|x| x.pane_id == *p).map(|x| x.engine.clone()), "held": boxes.iter().find(|x| x.pane_id == *p).map(|x| crate::team_guard::is_held(&x.dir)).unwrap_or(false), "baseline_dirty": boxes.iter().find(|x| x.pane_id == *p).and_then(|x| crate::team_guard::load_baseline(&x.dir)).map(|b| !b.fp.is_empty()).unwrap_or(false) })).collect::<Vec<_>>(), "ready": c.ready().is_ok(), "violations": crate::team_guard::read_log(&root).iter().filter(|e| e["team"] == json!(c.id)).count(), "last_violation": crate::team_guard::read_log(&root).iter().rev().find(|e| e["team"] == json!(c.id)).cloned() })).collect();
            let live: Vec<Value> = handles().lock().unwrap_or_else(|p| p.into_inner()).values().map(|h| h.snap.lock().map(|g| g.clone()).unwrap_or(Value::Null)).collect();
            json!({ "ok": true, "teams": rows, "runs": load_runs(&root).into_iter().take(20).collect::<Vec<_>>(), "live": live })
        }
        "run" => {
            let teams = load_teams(&root); let want = s(&b["team"]);
            let Some(c) = teams.iter().find(|c| c.id == slug(&want) || c.name.eq_ignore_ascii_case(&want)).cloned() else { return fail(format!("no such team `{want}`: teams op list")) };
            match start_run(state, reg, root, c, s(&b["task"]), b["budget_usd"].as_f64().filter(|x| *x > 0.0), None, true).await { Ok(v) => v, Err(e) => fail(e) }
        }
        "status" => {
            let id = s(&b["run_id"]);
            if let Some(h) = handles().lock().unwrap_or_else(|p| p.into_inner()).get(&id) { return json!({ "ok": true, "run": h.snap.lock().map(|g| g.clone()).unwrap_or(Value::Null) }); }
            match load_runs(&root).into_iter().find(|r| r["run_id"] == json!(id)) { Some(r) => json!({ "ok": true, "run": r }), None => fail(format!("no run `{id}`")) }
        }
        "stop" => { let id = s(&b["run_id"]); match handles().lock().unwrap_or_else(|p| p.into_inner()).get(&id) { Some(h) => { h.stop.store(true, Ordering::Relaxed); json!({ "ok": true, "stopping": id }) } None => fail(format!("no run `{id}` is going")) } }
        _ => {
            let mut teams = load_teams(&root);
            let doomed: Vec<(Role, i64)> = if s(&b["op"]) == "delete" { teams.iter().find(|c| c.id == slug(&s(&b["team"])) || c.name.eq_ignore_ascii_case(&s(&b["team"]))).map(|c| c.members()).unwrap_or_default() }
                else if s(&b["op"]) == "unlink" { teams.iter().find(|c| c.pair && c.members().iter().any(|(_, p)| Some(*p) == b["pane"].as_i64())).map(|c| c.members()).unwrap_or_default() }   // a pair with one left is dissolved: both are let go
                else { vec![] };
            match apply(&mut teams, &b) {
                Ok(mut v) => {
                    if let Err(e) = save_teams(&root, &teams) { return fail(e); }
                    // THE GATE FOLLOWS THE LINK: a planner or a QA is held read-only (the hook refuses its writes, the guard reverts what gets past), a worker or anyone leaving is let go
                    let st = state.clone(); let r2 = root.clone(); let op = s(&b["op"]); let (role, pane, team) = (s(&v["role"]), v["pane"].as_i64().unwrap_or(0), s(&v["team"])); let team_id = team.clone();
                    let guarded = tokio::task::spawn_blocking(move || -> Result<(), String> {
                        let boxes = crate::work::boxes_of_project(&st); let dir_of = |p: i64| boxes.iter().find(|x| x.pane_id == p).map(|x| x.dir.clone());
                        match op.as_str() {
                            "link" | "pair" => if let Some(dir) = dir_of(pane) {
                                if role == "worker" { crate::team_guard::release(&dir); }
                                else {
                                    if crate::cowork::same_root(&dir.to_string_lossy(), &r2) { return Err("this agent works in your project folder itself: a team's planner and QA need a box of their own, so that anything they change can be rolled back".into()); }
                                    crate::team_guard::hold(&dir, &role, &team).map_err(|e| format!("this agent's box cannot be held read-only (a team's planner and QA need a git checkout, so their changes can be rolled back): {e}"))?;
                                }
                            },
                            "unlink" => { if let Some(dir) = dir_of(pane) { crate::team_guard::release(&dir); } for (_, p) in &doomed { if let Some(dir) = dir_of(*p) { crate::team_guard::release(&dir); } } }
                            "delete" => for (_, p) in &doomed { if let Some(dir) = dir_of(*p) { crate::team_guard::release(&dir); } },
                            _ => {}
                        }
                        Ok(())
                    }).await.map_err(|e| e.to_string()).and_then(|r| r);
                    if let Err(e) = guarded { if s(&b["op"]) == "link" { let mut again = load_teams(&root); for c in again.iter_mut() { c.unlink(pane); } let _ = save_teams(&root, &again); } else if s(&b["op"]) == "pair" { let mut again = load_teams(&root); again.retain(|c| c.id != team_id); let _ = save_teams(&root, &again); } return fail(e); }
                    v["ok"] = json!(true); v
                }
                Err(e) => fail(e),
            }
        }
    }
}

/// `POST /api/v1/cockpit/teams`
pub async fn http(state: Arc<AppState>, reg: Arc<PtyRegistry>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    // pairing or linking from the SCREEN gives each member a copy of its own first; the screen starts those terminals again (an agent asking through its tool cannot have its own terminal restarted under it)
    let members: Vec<i64> = match s(&b["op"]).as_str() { "pair" => vec![b["planner"].as_i64().unwrap_or(0), b["worker"].as_i64().unwrap_or(0)], "link" => vec![b["pane"].as_i64().unwrap_or(0)], _ => vec![] };
    let root = crate::chat_pane::workspace_root_of(&state);
    let mut restart = vec![];
    for pane in members.into_iter().filter(|p| *p > 0 && !root.is_empty()) {
        match crate::thread_worktree::give_own_copy(&state, &root, pane).await { Ok(true) => restart.push(pane), Ok(false) => {} Err(e) => return reply(json!({ "ok": false, "error": e, "restart": restart })) }
    }
    let mut v = handle(state, reg, b).await;
    if !restart.is_empty() { v["restart"] = json!(restart); }
    reply(v)
}
/// A request in the old words (`crew`) is read as the new (`team`): an older screen, agent binary or script keeps working. Pure - tested.
pub fn in_new_words(mut b: Value) -> Value {
    if let Some(o) = b.as_object_mut() { if !o.contains_key("team") { if let Some(v) = o.remove("crew") { o.insert("team".into(), v); } } }
    b
}

/// A team's state in words, for an agent that asked. Pure.
pub fn render(v: &Value) -> String {
    if v["ok"] == json!(false) { return s(&v["error"]); }
    if let Some(teams) = v["teams"].as_array() {
        let mut out = vec![format!("{} team{}", teams.len(), if teams.len() == 1 { "" } else { "s" })];
        for c in teams { out.push(format!("- **{}**{}{} — {}", s(&c["id"]), if s(&c["name"]) != s(&c["id"]) && !s(&c["name"]).is_empty() { format!(" \"{}\"", s(&c["name"])) } else { String::new() }, if c["ready"] == json!(true) { "" } else { " (not ready)" }, c["members"].as_array().map(|m| m.iter().map(|x| format!("{} = pane {} ({})", s(&x["role"]), x["pane"], x["agent"].as_str().unwrap_or("?"))).collect::<Vec<_>>().join(", ")).unwrap_or_default())); }
        for r in v["live"].as_array().into_iter().flatten().filter(|r| r["ended"] != json!(true)) { out.push(format!("running {}: {} · {} attempt {}", s(&r["run_id"]), s(&r["task"]), s(&r["run"]["phase"]), r["run"]["attempt"])); }
        for r in v["runs"].as_array().into_iter().flatten().take(5) { out.push(format!("run {} — {}{}", s(&r["run_id"]), s(&r["outcome"]), if s(&r["blocked"]).is_empty() { String::new() } else { format!(" ({})", s(&r["blocked"])) })); }
        return out.join("\n");
    }
    if let Some(r) = v.get("run").filter(|r| r.is_object()) {
        let run = &r["run"]; let ended = r["ended"] == json!(true);
        let mut out = vec![format!("run {} — {} · attempt {} of {}{}", s(&r["run_id"]), if ended { s(&r["outcome"]) } else { s(&run["phase"]) }, run["attempt"], run["max_attempts"], if run["spent_usd"].as_f64().unwrap_or(0.0) > 0.0 { format!(" · ${:.2}", run["spent_usd"].as_f64().unwrap_or(0.0)) } else { String::new() })];
        if !s(&r["blocked"]).is_empty() { out.push(format!("NEEDS YOU: {}", s(&r["blocked"]))); }
        for (i, vd) in r["verdicts"].as_array().into_iter().flatten().enumerate() { out.push(format!("QA verdict {}: {} — {}", i + 1, if vd["passed"] == json!(true) { "pass" } else { "fail" }, vd["results"].as_array().map(|rs| rs.iter().filter(|x| x["ok"] == json!(false)).map(|x| format!("{}: {}", s(&x["id"]), s(&x["evidence"]))).collect::<Vec<_>>().join("; ")).unwrap_or_default())); }
        if r["saga"].is_number() { out.push(format!("saga {} holds every step", r["saga"])); }
        return out.join("\n");
    }
    if s(&v["run_id"]) != "" { return format!("started run {} (saga {}). Watch it with teams op status.", s(&v["run_id"]), v["saga"]); }
    serde_json::to_string(v).unwrap_or_default()
}


/// What the planner is told when the run ends: the outcome, and what the QA found. It starts with `[team result]` so the watcher never mistakes it for the person talking. Pure.
pub fn result_message(run: &Run, outcome: &Outcome) -> String {
    let found = run.verdicts.last().map(|v| { let bad: Vec<String> = v.results.iter().filter(|r| !r.ok).map(|r| format!("{} — {}", r.id, r.evidence)).collect(); if bad.is_empty() { format!("The QA said: {}", if v.summary.is_empty() { "all criteria pass".to_string() } else { v.summary.clone() }) } else { format!("Still failing: {}", bad.join("; ")) } }).unwrap_or_default();
    match outcome {
        Outcome::Done => format!("[team result] The work is done after {} attempt{}. {found} Tell the person what was done.", run.attempt, if run.attempt == 1 { "" } else { "s" }),
        Outcome::Blocked(why) => format!("[team result] The team stopped and needs the person: {why}. {found} Tell the person what happened and what they can decide."),
    }
}

/// The note the planner's prompt hook adds to what the person types: set when this pane is some team's planner and the text is the person's own (a prompt the team typed starts with a `[team` tag). Pure over the teams.
pub fn planner_context(teams: &[Team], pane_id: i64, text: &str) -> Option<String> {
    if text.trim_start().starts_with("[team") { return None; }
    teams.iter().find(|c| c.planner == Some(pane_id)).map(|c| { let n = team::planner_note(&c.name, c.pane(Role::Qa).is_some()); if c.pair { format!("{n}\nThis team is a PAIR: you are also its QA. When the worker delivers you are asked to judge the work against YOUR OWN criteria, so write criteria you can check.") } else { n } })
}

/// The newest finished turn of a box and whether it is the person's (not a team-typed prompt): `(end_ms, prompt, answer)`. Pure over turns.
fn latest_turn(turns: &[crate::work_boxes::Turn]) -> Option<(i64, String, String)> { turns.iter().filter_map(|t| t.end_ms.map(|e| (e, t.prompt.clone(), t.answer.clone()))).max_by_key(|x| x.0) }

/// What a new planner answer asks of the team: a plan to run (with the person's own words as the task), or nothing. `seen` is the end of the last turn looked at. Pure.
pub fn plan_from_answer(seen: i64, turn: &(i64, String, String)) -> Option<(String, String)> {
    let (end, prompt, answer) = turn; if *end <= seen || prompt.trim_start().starts_with("[team") { return None; }
    let block = team::extract_plan_block(answer)?; team::parse_plan(&block).ok()?;
    Some((prompt.trim().to_string(), block))
}

/// THE WATCHER: the person talks to a team's planner like to any agent; when the planner answers with a plan block, the team carries it out. Checked every couple of seconds, for the open workspace's teams.
pub async fn watch(state: Arc<AppState>, reg: Arc<PtyRegistry>) {
    let mut seen: HashMap<i64, i64> = HashMap::new();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let root = crate::chat_pane::workspace_root_of(&state); if root.is_empty() { continue; }
        let all = load_teams(&root);
        // THE GATE FOLLOWS THE TEAM: a planner or QA linked before the gate existed (or whose marker was lost) is held now; and every held box is checked against its baseline, a violation reverted, recorded and told
        if !all.is_empty() { let (st, all, rt_reg, root2) = (state.clone(), all.clone(), reg.clone(), root.clone());
          let found: Vec<(String, i64, Role, crate::team_guard::Violation)> = tokio::task::spawn_blocking(move || {
            let boxes = crate::work::boxes_of_project(&st); let mut out = vec![];
            let verifying: std::collections::HashSet<String> = handles().lock().unwrap_or_else(|p| p.into_inner()).values().filter_map(|h| h.snap.lock().ok().and_then(|g| (g["run"]["phase"] == json!("verifying") && g["ended"] != json!(true)).then(|| s(&g["team"])))).collect();
            for c in &all { for (role, pane) in c.members() {
                if role == Role::Worker { continue; }
                let Some(b) = boxes.iter().find(|x| x.pane_id == pane) else { continue };
                if crate::cowork::same_root(&b.dir.to_string_lossy(), &root2) { continue; }
                if !crate::team_guard::is_held(&b.dir) { let _ = crate::team_guard::hold(&b.dir, role.key(), &c.id); continue; }
                // the QA's box holds the delivery while it judges: the run's own check covers that
                if role == Role::Qa && verifying.contains(&c.id) { continue; }
                if let Ok(Some(v)) = crate::team_guard::check(&b.dir) { let _ = crate::team_guard::restore(&b.dir); out.push((c.id.clone(), pane, role, v)); }
            } }
            out }).await.unwrap_or_default();
          for (team_id, pane, role, v) in found {
            crate::team_guard::append_log(&root, json!({ "t": crate::cli::now_ms(), "team": team_id, "role": role.key(), "pane": pane, "files": v.files, "moved_head": v.moved_head, "run": Value::Null }));
            // a run that is going carries the count (two violations stop it) and the saga's trail says so
            let live: Option<(Arc<Handle>, i64)> = handles().lock().unwrap_or_else(|p| p.into_inner()).values().find_map(|h| h.snap.lock().ok().and_then(|g| (g["team"] == json!(team_id) && g["ended"] != json!(true)).then(|| (h.clone(), g["saga"].as_i64().unwrap_or(0)))));
            if let Some((h, saga)) = live { h.violations.fetch_add(1, Ordering::Relaxed); let (r2, text) = (root.clone(), format!("the {} changed code ({}) and it was reverted", role.key(), if v.files.is_empty() { "the box".to_string() } else { v.files.join(", ") })); let _ = tokio::task::spawn_blocking(move || { if let Ok(c) = crate::work::open(&r2) { let _ = crate::work::event(&c, saga, None, "team", "violation", &text); } }).await; }
            let (rt, reg2, msg) = (tokio::runtime::Handle::current(), rt_reg.clone(), crate::team_guard::notice(role.key(), &v));
            let _ = tokio::task::spawn_blocking(move || type_into(&rt, &reg2, pane, &msg)).await;
          } }
        // THE SIDE-QUESTION HELPERS ARE HELD THE SAME WAY: a change in one's box is put back, logged and told to it
        { let (st, rt_reg, root2) = (state.clone(), reg.clone(), root.clone());
          let found: Vec<(i64, crate::team_guard::Violation)> = tokio::task::spawn_blocking(move || {
            let boxes = crate::work::boxes_of_project(&st); let mut out = vec![];
            for b in crate::side::held_boxes(&boxes) { if crate::cowork::same_root(&b.dir.to_string_lossy(), &root2) { continue; } if let Ok(Some(v)) = crate::team_guard::check(&b.dir) { let _ = crate::team_guard::restore(&b.dir); out.push((b.pane_id, v)); } }
            out }).await.unwrap_or_default();
          for (pane, v) in found {
            crate::team_guard::append_log(&root, json!({ "t": crate::cli::now_ms(), "team": "side", "role": "side", "pane": pane, "files": v.files, "moved_head": v.moved_head, "run": Value::Null }));
            let (rt, reg2, msg) = (tokio::runtime::Handle::current(), rt_reg.clone(), crate::team_guard::notice("side", &v));
            let _ = tokio::task::spawn_blocking(move || type_into(&rt, &reg2, pane, &msg)).await;
          } }
        let teams: Vec<Team> = all.into_iter().filter(|c| c.ready().is_ok()).collect(); if teams.is_empty() { continue; }
        let st = state.clone();
        let turns: Vec<(Team, Option<(i64, String, String)>)> = tokio::task::spawn_blocking(move || { let boxes = crate::work::boxes_of_project(&st); teams.into_iter().map(|c| { let t = c.planner.and_then(|p| boxes.iter().find(|b| b.pane_id == p)).and_then(|b| latest_turn(&crate::work_boxes::box_episodes(b).into_iter().flat_map(|e| e.turns).collect::<Vec<_>>())); (c, t) }).collect() }).await.unwrap_or_default();
        for (team, turn) in turns {
            let Some(pane) = team.planner else { continue };
            // the first sight of a planner is not news: what it already said stays said (a planner with nothing said yet has nothing to leave: anything it says next is new)
            let Some(&was) = seen.get(&pane) else { seen.insert(pane, turn.as_ref().map_or(0, |t| t.0)); continue };
            let Some(turn) = turn else { continue };
            if let Some((task, block)) = plan_from_answer(was, &turn) {
                let busy = handles().lock().unwrap_or_else(|p| p.into_inner()).values().any(|h| h.snap.lock().map(|g| g["team"] == json!(team.id) && g["ended"] != json!(true)).unwrap_or(false));
                if !busy { let _ = start_run(state.clone(), reg.clone(), root.clone(), team.clone(), task, None, Some(block), true).await; }
            }
            if turn.0 > was { seen.insert(pane, turn.0); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_diff_sees_a_new_file_and_leaves_the_folder_as_it_found_it() {
        let dir = std::env::temp_dir().join(format!("antiloki-patch-{}", std::process::id())); let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let g = |a: &[&str]| { let o = std::process::Command::new("git").args(a).current_dir(&dir).output().unwrap(); String::from_utf8_lossy(&o.stdout).to_string() };
        g(&["init", "-q", "-b", "main"]); g(&["config", "user.email", "t@t"]); g(&["config", "user.name", "t"]); std::fs::write(dir.join("a.txt"), "one\n").unwrap(); g(&["add", "-A"]); g(&["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("a.txt"), "two\n").unwrap(); std::fs::write(dir.join("new.md"), "hello\n").unwrap();
        let patch = patch_of(&dir).unwrap(); assert!(patch.contains("+hello") && patch.contains("new file") && patch.contains("+two"), "the new file is in the patch: {patch}");
        let st = g(&["status", "--porcelain"]); assert!(st.contains("?? new.md") && !st.contains(" A new.md") && st.contains(" M a.txt"), "no half-added mark is left behind: {st}");
        let names = names_of(&dir).unwrap(); assert!(names.contains(&"a.txt".to_string()) && names.contains(&"new.md".to_string()), "{names:?}");
        let stash = g(&["stash", "push", "-u", "-m", "x"]); assert!(stash.contains("Saved"), "a stash works on it: {stash}"); let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pair_is_two_agents_the_one_dropped_on_plans_and_judges_and_a_pair_with_one_left_is_no_pair() {
        let mut teams: Vec<Team> = vec![];
        let v = apply(&mut teams, &json!({ "op": "pair", "planner": 2, "worker": 4 })).unwrap(); assert_eq!((v["name"].as_str().unwrap(), v["role"].as_str().unwrap(), v["pane"].as_i64().unwrap()), ("Pair", "planner", 2));
        let t = &teams[0]; assert!(t.pair && t.ready().is_ok()); assert_eq!(t.members().iter().map(|(r, p)| (r.key(), *p)).collect::<Vec<_>>(), vec![("planner", 2), ("worker", 4)], "listed as the two it is");
        assert_eq!(t.pane(Role::Qa), Some(2), "the planner is the QA"); assert!(t.to_graph(2.0).nodes.values().any(|n| matches!(&n.kind, crate::team_graph::Kind::Role { role: crate::team_graph::RoleKind::Qa, .. })), "the pair's run has a QA step");
        assert!(planner_context(&teams, 2, "do it").unwrap().contains("PAIR: you are also its QA")); assert!(planner_context(&teams, 4, "do it").is_none());
        // a second pair is named on, a pane is in one place only, a pair takes no third member, a pair is two different agents
        assert_eq!(apply(&mut teams, &json!({ "op": "pair", "planner": 6, "worker": 8 })).unwrap()["name"], "Pair 2");
        assert!(apply(&mut teams, &json!({ "op": "pair", "planner": 2, "worker": 10 })).unwrap_err().contains("belongs to `Pair` already")); assert!(apply(&mut teams, &json!({ "op": "pair", "planner": 12, "worker": 12 })).unwrap_err().contains("two different"));
        assert!(apply(&mut teams, &json!({ "op": "link", "team": "pair", "role": "qa", "pane": 10 })).unwrap_err().contains("this is a pair"));
        assert_eq!(apply(&mut teams, &json!({ "op": "unlink", "pane": 4 })).unwrap()["role"], "worker"); assert_eq!(teams.len(), 1, "the pair is dissolved, the other is let go"); assert_eq!(teams[0].name, "Pair 2");
        assert_eq!(apply(&mut teams, &json!({ "op": "pair", "planner": 20, "worker": 22, "name": "Night shift" })).unwrap()["name"], "Night shift");
    }
    #[test]
    fn teams_are_made_linked_and_unlinked_one_pane_to_one_team() {
        let mut teams: Vec<Team> = vec![];
        assert_eq!(apply(&mut teams, &json!({ "op": "create", "name": "Picker Team" })).unwrap()["team"], "picker-team");
        assert!(apply(&mut teams, &json!({ "op": "create", "name": "picker team" })).unwrap_err().contains("exists"));
        assert!(apply(&mut teams, &json!({ "op": "create", "name": "  " })).unwrap_err().contains("needs a name"));
        apply(&mut teams, &json!({ "op": "link", "team": "Picker Team", "role": "worker", "pane": 4 })).unwrap();
        apply(&mut teams, &json!({ "op": "create", "name": "other" })).unwrap();
        assert!(apply(&mut teams, &json!({ "op": "link", "team": "other", "role": "qa", "pane": 4 })).unwrap_err().contains("belongs to the team `picker-team`"));
        assert!(apply(&mut teams, &json!({ "op": "link", "team": "other", "role": "boss", "pane": 5 })).unwrap_err().contains("planner, worker, qa"));
        assert!(apply(&mut teams, &json!({ "op": "link", "team": "nope", "role": "qa", "pane": 5 })).unwrap_err().contains("no such team"));
        assert!(apply(&mut teams, &json!({ "op": "link", "team": "picker-team", "role": "worker", "pane": 9 })).unwrap_err().contains("one per role"));
        // a rename changes the name and never the id; two teams cannot share a name; an empty one is refused
        let id = teams[0].id.clone(); assert_eq!(apply(&mut teams, &json!({ "op": "rename", "team": id.clone(), "name": "  The   night   shift " })).unwrap()["name"], "The night shift");
        assert_eq!((teams[0].id.clone(), teams[0].name.clone()), (id.clone(), "The night shift".to_string())); assert!(apply(&mut teams, &json!({ "op": "rename", "team": "The night shift", "name": " " })).is_err());
        assert_eq!(apply(&mut teams, &json!({ "op": "unlink", "pane": 4 })).unwrap()["role"], "worker");
        assert!(apply(&mut teams, &json!({ "op": "unlink", "pane": 4 })).unwrap_err().contains("in no team"));
        assert_eq!(apply(&mut teams, &json!({ "op": "delete", "team": "other" })).unwrap()["deleted"], "other"); assert_eq!(teams.len(), 1);
        assert!(apply(&mut teams, &json!({ "op": "dance" })).unwrap_err().contains("op is one of"));
    }

    #[test]
    fn teams_survive_in_the_project_folder() {
        let d = std::env::temp_dir().join(format!("antiloki-teams-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); let root = d.to_string_lossy().to_string();
        assert!(load_teams(&root).is_empty());
        let mut c = Team { id: "k".into(), name: "K".into(), ..Default::default() }; c.link(Role::Worker, 3).unwrap(); c.link(Role::Qa, 5).unwrap();
        save_teams(&root, &[c.clone()]).unwrap(); assert_eq!(load_teams(&root), vec![c]);
        save_run(&root, "k-1", &json!({ "run_id": "k-1", "started_ms": 5 })); save_run(&root, "k-2", &json!({ "run_id": "k-2", "started_ms": 9 }));
        assert_eq!(load_runs(&root).iter().map(|r| r["run_id"].as_str().unwrap().to_string()).collect::<Vec<_>>(), vec!["k-2", "k-1"], "newest first");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_delivery_is_the_boxes_change_without_the_agents_own_files_and_the_qa_is_clean_only_while_it_holds_exactly_that() {
        let d = std::env::temp_dir().join(format!("antiloki-team-git-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let g = |a: &[&str]| { let o = std::process::Command::new("git").args(["-c", "user.email=e@x", "-c", "user.name=e"]).args(a).current_dir(&d).output().unwrap(); assert!(o.status.success(), "{a:?}: {}", String::from_utf8_lossy(&o.stderr)); };
        g(&["init", "-q"]); std::fs::write(d.join("a.txt"), "one\n").unwrap(); g(&["add", "-A"]); g(&["commit", "-q", "-m", "init"]);
        assert_eq!(fingerprint(&patch_of(&d).unwrap()), "", "nothing changed: no delivery");
        // an agent's own files are not work
        std::fs::create_dir_all(d.join(".antiloki")).unwrap(); std::fs::write(d.join(".antiloki/prompts.jsonl"), "x").unwrap(); std::fs::write(d.join("CLAUDE.md"), "brief").unwrap();
        assert_eq!(fingerprint(&patch_of(&d).unwrap()), "", "the box's own files are not a change");
        std::fs::write(d.join("a.txt"), "two\n").unwrap(); std::fs::write(d.join("new.txt"), "n\n").unwrap();
        let p1 = patch_of(&d).unwrap(); assert!(p1.contains("a.txt") && p1.contains("new.txt") && !p1.contains(".antiloki") && !p1.contains("CLAUDE.md"), "{p1}");
        let f1 = fingerprint(&p1); assert_eq!(f1.len(), 16); assert_eq!(f1, fingerprint(&patch_of(&d).unwrap()), "the same change is the same fingerprint");
        std::fs::write(d.join("a.txt"), "three\n").unwrap(); assert_ne!(f1, fingerprint(&patch_of(&d).unwrap()), "another change, another fingerprint");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_slug_is_words_and_dashes() { assert_eq!((slug("Picker Team!"), slug("  --a__b "), slug("")), ("picker-team".to_string(), "a-b".to_string(), String::new())); }

    fn turn(end: i64, prompt: &str, answer: &str) -> (i64, String, String) { (end, prompt.into(), answer.into()) }
    const BLOCK: &str = "```team-plan\n{\"summary\":\"s\",\"criteria\":[{\"id\":\"c1\",\"text\":\"it works\"}]}\n```";

    #[test]
    fn a_planners_answer_with_a_plan_block_starts_the_team_with_the_persons_own_words_as_the_task() {
        let t = turn(10, " add a greeting to the page ", &format!("Plan below.\n{BLOCK}"));
        let (task, block) = plan_from_answer(5, &t).unwrap(); assert_eq!(task, "add a greeting to the page"); assert!(block.contains("c1"));
        assert!(plan_from_answer(10, &t).is_none(), "an answer already seen starts nothing twice");
        assert!(plan_from_answer(5, &turn(10, "hi", "just talk")).is_none(), "talk is not a plan");
        assert!(plan_from_answer(5, &turn(10, "x", "```team-plan\n{\"criteria\":[]}\n```")).is_none(), "a plan with no criteria is not one");
        assert!(plan_from_answer(5, &turn(10, "[team result] done", &format!("{BLOCK}"))).is_none(), "what the team typed to the planner is not the person asking");
    }

    #[test]
    fn only_the_planner_is_told_it_is_one_and_only_for_what_the_person_typed() {
        let mut c = Team { id: "k".into(), name: "K".into(), ..Default::default() }; c.link(Role::Planner, 2).unwrap(); c.link(Role::Worker, 4).unwrap();
        let teams = vec![c];
        assert!(planner_context(&teams, 2, "add a greeting").unwrap().contains("PLANNER of the team \"K\""));
        assert!(planner_context(&teams, 4, "add a greeting").is_none() && planner_context(&teams, 9, "x").is_none(), "the worker and strangers are told nothing");
        assert!(planner_context(&teams, 2, "[team k-1 · step 1]\nYou are the PLANNER").is_none() && planner_context(&teams, 2, "[team result] done").is_none(), "the team's own prompts carry no note");
    }

    #[test]
    fn the_planner_is_told_how_it_ended_and_what_the_qa_found() {
        let mut run = Run::new("t", true, true, None); run.attempt = 2; run.verdicts.push(team::Verdict { passed: true, results: vec![], summary: "fine".into() });
        let m = result_message(&run, &Outcome::Done); assert!(m.starts_with("[team result] The work is done after 2 attempts") && m.contains("The QA said: fine"), "{m}");
        run.verdicts.push(team::Verdict { passed: false, results: vec![team::CriterionResult { id: "c2".into(), ok: false, evidence: "no world".into() }], summary: String::new() });
        let m = result_message(&run, &Outcome::Blocked("the QA rejected the work 3 times".into())); assert!(m.starts_with("[team result] The team stopped") && m.contains("c2 — no world") && m.contains("3 times"), "{m}");
    }
}
