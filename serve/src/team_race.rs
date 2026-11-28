//! THE RACE: the run swarm of RFC 0019 (named `race` because `team_swarm` is the data swarm, one node over many items) (RFC 0019, step 1-3): several theories tested AT THE SAME TIME, a cap on how many run at once, one winner.
//! The sequential loop (`team_evolve::run_with`) tries one theory after another; here the improver proposes a batch of up to `width` theories, their experiments run in parallel (at most `concurrency` at once, the rest wait),
//! and when the batch is done the best admitted one is applied. Two that both pass are never merged: the second is refused with the reason, and a later batch can test it again on top. Same hands as the sequential loop, so the
//! same fake can test it. The session is written after every step (`swarm-session.json`) so the grid on the screen follows it: a trial is `waiting`, `running`, then its outcome.
use crate::team_evolve::{judge, kind_of, report_text, Cfg, Hands, Session, Trial};
use crate::team_experiment::Goal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SwarmCfg { /** theories proposed per batch */ pub width: u32, /** experiments running at once */ pub concurrency: u32 }
impl Default for SwarmCfg { fn default() -> Self { SwarmCfg { width: 4, concurrency: 3 } } }
impl SwarmCfg { pub fn tidy(mut self) -> SwarmCfg { self.width = self.width.clamp(1, 8); self.concurrency = self.concurrency.clamp(1, 6); self } }
fn dir(root: &str, team: &str) -> PathBuf { crate::team_graph_run::team_files(root, team) }
pub fn cfg(root: &str, team: &str) -> SwarmCfg { std::fs::read_to_string(dir(root, team).join("swarm.json")).ok().and_then(|t| serde_json::from_str::<SwarmCfg>(&t).ok()).map(SwarmCfg::tidy).unwrap_or_default() }
pub fn set_cfg(root: &str, team: &str, patch: &Value) -> Result<SwarmCfg, String> {
    let mut c = cfg(root, team); if let Some(x) = patch["width"].as_u64() { c.width = x as u32; } if let Some(x) = patch["concurrency"].as_u64() { c.concurrency = x as u32; } let c = c.tidy();
    let d = dir(root, team); std::fs::create_dir_all(&d).map_err(|e| e.to_string())?; std::fs::write(d.join("swarm.json"), serde_json::to_string_pretty(&c).unwrap_or_default()).map_err(|e| e.to_string())?; Ok(c)
}
pub fn session(root: &str, team: &str) -> Session { std::fs::read_to_string(dir(root, team).join("swarm-session.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| Session { status: "idle".into(), ..Default::default() }) }
fn save(root: &str, team: &str, s: &Session) { let d = dir(root, team); let _ = std::fs::create_dir_all(&d); let _ = std::fs::write(d.join("swarm-session.json"), serde_json::to_string_pretty(s).unwrap_or_default()); }
static RUNNING: Mutex<Vec<String>> = Mutex::new(Vec::new());
pub fn running(root: &str, team: &str) -> bool { RUNNING.lock().map(|v| v.contains(&format!("{root}|{team}"))).unwrap_or(false) }

/// How much one theory may spend: an even share of what is left across the batch (so the batch cannot overshoot the purse together), raised by half because a theory that is screened out spends far less. Pure.
pub fn share(left: f64, in_batch: usize, theories_left: usize) -> f64 { let n = in_batch.max(1) as f64; (left / theories_left.max(1) as f64 * 1.5).min(left / n).max(0.05) }
/// Of the admitted theories (index, gain %), the one to apply: the largest gain, the first on a tie. Pure.
pub fn pick_winner(admitted: &[(usize, f64)]) -> Option<usize> { admitted.iter().fold(None, |b: Option<(usize, f64)>, &(i, g)| match b { Some((_, bg)) if bg >= g => b, _ => Some((i, g)) }).map(|(i, _)| i) }

/// The swarm. Blocking. Returns the finished session.
pub fn run_with(h: &(dyn Hands + Sync), root: &str, team: &str, c: &Cfg, sc: &SwarmCfg) -> Session {
    let sc = sc.clone().tidy(); let goal = Goal::parse(&c.goal).unwrap_or_default(); let (tune, held) = h.tasks();
    let mut s0 = Session { status: "running".into(), goal: c.goal.clone(), min_gain_pct: c.min_gain_pct, max_trials: c.max_trials, budget_usd: c.budget_usd, started_ms: h.now(), ..Default::default() };
    if held == 0 || tune == 0 { s0.status = "needs_tasks".into(); s0.note = format!("The test bank has {tune} practice and {held} held-out tasks. An experiment needs at least 1 of each: save a few passed runs as tests first."); s0.ended_ms = h.now(); s0.report = report_text(team, &s0); save(root, team, &s0); h.tell(&s0.report); return s0; }
    save(root, team, &s0);
    let shared = Mutex::new(s0); let sync = |f: &dyn Fn(&mut Session)| { let mut g = shared.lock().unwrap_or_else(|p| p.into_inner()); f(&mut g); save(root, team, &g); };
    let mut made: u32 = 0;
    'batches: while made < c.max_trials {
        if h.stopped() { sync(&|s| s.status = "stopped".into()); break; }
        let (left, snapshot) = { let g = shared.lock().unwrap_or_else(|p| p.into_inner()); (c.budget_usd - g.spent_usd, g.trials.clone()) };
        if left < 0.05 { sync(&|s| s.status = "budget".into()); break; }
        // 1. the batch is proposed one at a time (each one sees the ones before it, so they differ)
        let want = sc.width.min(c.max_trials - made); let mut batch: Vec<usize> = vec![];
        for _ in 0..want {
            made += 1; let n = made; let trials_now = shared.lock().unwrap_or_else(|p| p.into_inner()).trials.clone(); let _ = &snapshot;
            let mut t = Trial { n, ..Default::default() };
            let reply = match h.propose(&h.brief(c, &trials_now, n)) { Ok((r, usd)) => { t.spent_usd += usd; sync(&|s| s.spent_usd += usd); r } Err(e) => { t.outcome = "error".into(); t.why = format!("the improver could not answer: {e}"); sync(&|s| { s.trials.push(t.clone()); s.status = "error".into(); s.note = "the improver seat did not answer".into(); }); break 'batches; } };
            match h.open(&reply) {
                Ok((req, hyp, patch)) => { t.request = Some(req); t.kind = kind_of(&hyp, &patch); t.hypothesis = hyp; t.outcome = "waiting".into(); let idx = { let mut g = shared.lock().unwrap_or_else(|p| p.into_inner()); g.trials.push(t.clone()); save(root, team, &g); g.trials.len() - 1 }; batch.push(idx); }
                Err(e) => { t.outcome = "no_proposal".into(); t.why = e; sync(&|s| s.trials.push(t.clone())); }
            }
        }
        if batch.is_empty() { continue; }
        // 2. the batch runs in parallel: `concurrency` workers take the next waiting theory until none is left
        let next = AtomicUsize::new(0); let theories_left = (c.max_trials - made) as usize + batch.len();
        std::thread::scope(|sc_| { for _ in 0..sc.concurrency.min(batch.len() as u32) { sc_.spawn(|| loop {
            let k = next.fetch_add(1, Ordering::SeqCst); if k >= batch.len() { break; } let idx = batch[k];
            let req = shared.lock().unwrap_or_else(|p| p.into_inner()).trials[idx].request.unwrap_or(0);
            if h.stopped() { sync(&|s| { s.trials[idx].outcome = "error".into(); s.trials[idx].why = "stopped before it ran".into(); }); let _ = h.decide(req, false, "stopped before it ran"); continue; }
            // THE MACHINE COMES FIRST: a run does not start while the person's computer is short of RAM or CPU (the same thresholds as the watcher that pauses runs already going)
            let mut told = false; while let Some(why) = h.busy() { if h.stopped() { break; } if !told { told = true; sync(&|s| { s.trials[idx].outcome = "waiting".into(); s.trials[idx].why = format!("waiting: {why}"); }); } std::thread::sleep(std::time::Duration::from_millis(h.busy_wait_ms())); }
            if told { sync(&|s| s.trials[idx].why.clear()); }
            let per = { let g = shared.lock().unwrap_or_else(|p| p.into_inner()); share(c.budget_usd - g.spent_usd, batch.len(), theories_left) };
            sync(&|s| s.trials[idx].outcome = "running".into());
            match h.experiment(req, per) {
                Ok((exp, prog)) => { let j = judge(goal, &exp, &prog); sync(&|s| { let t = &mut s.trials[idx]; t.spent_usd += j.spent; t.gain_pct = j.gain_pct; t.sure_pct = j.sure_pct; t.why = j.why.clone(); t.experiment = exp.clone(); t.outcome = j.outcome.into(); s.spent_usd += j.spent; }); if !j.admitted { let _ = h.decide(req, false, &j.why); } }
                Err(e) => { sync(&|s| { s.trials[idx].outcome = "error".into(); s.trials[idx].why = e.clone(); }); let _ = h.decide(req, false, "the experiment could not run"); }
            }
        }); } });
        // 3. the best admitted theory is applied; the others that also passed are refused with the reason (never merged blind)
        let (adm, rows): (Vec<(usize, f64)>, Vec<(usize, Option<u32>)>) = { let g = shared.lock().unwrap_or_else(|p| p.into_inner()); (batch.iter().filter(|&&i| g.trials[i].outcome == "admitted").map(|&i| (i, g.trials[i].gain_pct)).collect(), batch.iter().map(|&i| (i, g.trials[i].request)).collect()) };
        if let Some(w) = pick_winner(&adm) {
            for (i, req) in rows { if !adm.iter().any(|(a, _)| *a == i) { continue; } let req = req.unwrap_or(0);
                if i == w { let why = shared.lock().unwrap_or_else(|p| p.into_inner()).trials[i].why.clone(); let _ = h.decide(req, true, &format!("the swarm's winner: {why}")); }
                else { let why = "it passed too, but a better theory of the same batch was applied; two changes are never merged blind: test this one again on top of the winner".to_string(); let _ = h.decide(req, false, &why); sync(&|s| { s.trials[i].outcome = "passed_not_applied".into(); s.trials[i].why = why.clone(); }); } }
            sync(&|s| s.status = "goal_met".into()); break;
        }
    }
    let mut s = shared.into_inner().unwrap_or_else(|p| p.into_inner()); if s.status == "running" { s.status = "exhausted".into(); }
    s.ended_ms = h.now(); s.report = report_text(team, &s); save(root, team, &s); h.tell(&s.report); s
}

/// Start a swarm in the background, with the sequential loop's real hands. One at a time per team (and never with an evolve session on the same team).
pub fn start(root: &str, team: &str) -> Result<Session, String> {
    if crate::team_evolve::running(root, team) { return Err("a self-evolution session is already running for this team".into()); }
    let key = format!("{root}|{team}"); { let mut r = RUNNING.lock().map_err(|e| e.to_string())?; if r.contains(&key) { return Err("a swarm is already running for this team".into()); } r.push(key.clone()); }
    let c = crate::team_evolve::cfg(root, team); let sc = cfg(root, team); crate::team_evolve::clear_stop(root, team);
    if crate::team_graph_run::load_graph(root, team).is_err() { if let Ok(mut r) = RUNNING.lock() { r.retain(|k| k != &key); } return Err("no such team".into()); }
    let s0 = Session { status: "running".into(), goal: c.goal.clone(), min_gain_pct: c.min_gain_pct, max_trials: c.max_trials, budget_usd: c.budget_usd, started_ms: crate::cli::now_ms(), ..Default::default() }; save(root, team, &s0);
    let (r, t, rt) = (root.to_string(), team.to_string(), tokio::runtime::Handle::current());
    tokio::task::spawn_blocking(move || { let hands = crate::team_evolve::Real { root: r.clone(), team: t.clone(), cfg: c.clone(), rt }; let _ = run_with(&hands, &r, &t, &c, &sc); if let Ok(mut v) = RUNNING.lock() { v.retain(|k| k != &format!("{r}|{t}")); } });
    Ok(s0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};
    /// Hands for a swarm: every experiment sleeps a little and records how many were running together; theory n gains `gains[n-1]` and is admitted when the gain is at least 20.
    struct Fake { gains: Vec<f64>, now: AtomicUsize, peak: AtomicUsize, opened: AtomicUsize, decided: Mutex<Vec<(u32, bool)>>, told: Mutex<Vec<String>>, tasks: (usize, usize), spend: f64, nap: u64 }
    impl Hands for Fake {
        fn propose(&self, _p: &str) -> Result<(String, f64), String> { Ok(("{}".into(), 0.01)) }
        fn open(&self, _r: &str) -> Result<(u32, String, Value), String> { let n = self.opened.fetch_add(1, Ordering::SeqCst) as u32 + 1; Ok((n, format!("hypothesis {n}: cheaper model"), Value::Null)) }
        fn experiment(&self, n: u32, _b: f64) -> Result<(Value, Value), String> {
            let now = self.now.fetch_add(1, Ordering::SeqCst) + 1; self.peak.fetch_max(now, Ordering::SeqCst); std::thread::sleep(Duration::from_millis(self.nap)); self.now.fetch_sub(1, Ordering::SeqCst);
            let g = self.gains.get(n as usize - 1).copied().unwrap_or(0.0); let arm = |c: f64| json!({ "n": 9, "passed": 9, "rate": 1.0, "lower": 0.8, "avg_cost_usd": c, "avg_wall_ms": 1000.0, "gate_stops": 0 });
            Ok((json!({ "stage": "full", "current": arm(1.0), "candidate": arm(1.0 - g / 100.0), "admitted": g >= 20.0, "why": format!("gain {g}%"), "spent_usd": self.spend }), Value::Null))
        }
        fn decide(&self, n: u32, a: bool, _w: &str) -> Result<(), String> { self.decided.lock().unwrap().push((n, a)); Ok(()) }
        fn tasks(&self) -> (usize, usize) { self.tasks } fn stopped(&self) -> bool { false } fn now(&self) -> i64 { 1000 }
        fn brief(&self, c: &Cfg, t: &[Trial], n: u32) -> String { crate::team_evolve::trial_brief(c, t, n) } fn tell(&self, r: &str) { self.told.lock().unwrap().push(r.to_string()); }
    }
    fn fake(gains: Vec<f64>, nap: u64) -> Fake { Fake { gains, now: AtomicUsize::new(0), peak: AtomicUsize::new(0), opened: AtomicUsize::new(0), decided: Mutex::new(vec![]), told: Mutex::new(vec![]), tasks: (2, 3), spend: 0.05, nap } }
    fn root() -> String { let d = std::env::temp_dir().join(format!("antiloki-swarm-{}-{}", std::process::id(), crate::cli::now_ms())); std::fs::create_dir_all(&d).unwrap(); d.to_string_lossy().to_string() }
    fn cfg4() -> Cfg { Cfg { max_trials: 4, budget_usd: 5.0, ..Default::default() } }
    #[test] fn theories_run_together_up_to_the_cap_and_no_more() {
        let h = fake(vec![0.0; 4], 80); let t0 = Instant::now(); let s = run_with(&h, &root(), "t", &cfg4(), &SwarmCfg { width: 4, concurrency: 2 });
        assert_eq!(h.peak.load(Ordering::SeqCst), 2, "never more than the cap, and the cap is reached"); assert_eq!(s.trials.len(), 4); assert_eq!(s.status, "exhausted");
        assert!(t0.elapsed() < Duration::from_millis(4 * 80), "two at a time is faster than one after another");
    }
    #[test] fn a_run_waits_while_the_machine_is_busy_and_goes_when_it_is_not() {
        struct Busy { inner: Fake, looks: AtomicUsize } impl Hands for Busy {
            fn propose(&self, p: &str) -> Result<(String, f64), String> { self.inner.propose(p) } fn open(&self, r: &str) -> Result<(u32, String, Value), String> { self.inner.open(r) } fn experiment(&self, n: u32, b: f64) -> Result<(Value, Value), String> { assert!(self.looks.load(Ordering::SeqCst) >= 3, "it started before the machine had room"); self.inner.experiment(n, b) }
            fn decide(&self, n: u32, a: bool, w: &str) -> Result<(), String> { self.inner.decide(n, a, w) } fn tasks(&self) -> (usize, usize) { self.inner.tasks() } fn stopped(&self) -> bool { false } fn now(&self) -> i64 { 1 } fn brief(&self, c: &Cfg, t: &[Trial], n: u32) -> String { self.inner.brief(c, t, n) } fn tell(&self, r: &str) { self.inner.tell(r) }
            fn busy(&self) -> Option<String> { if self.looks.fetch_add(1, Ordering::SeqCst) < 3 { Some("only 1.2 GB of memory is free".into()) } else { None } } fn busy_wait_ms(&self) -> u64 { 5 } }
        let h = Busy { inner: fake(vec![0.0], 1), looks: AtomicUsize::new(0) }; let r = root(); let s = run_with(&h, &r, "t", &Cfg { max_trials: 1, ..cfg4() }, &SwarmCfg { width: 1, concurrency: 1 });
        assert_eq!(s.trials.len(), 1); assert_eq!(s.trials[0].outcome, "not_admitted"); assert!(h.looks.load(Ordering::SeqCst) >= 4);
    }
    #[test] fn one_at_a_time_when_the_cap_is_one() { let h = fake(vec![0.0; 3], 20); run_with(&h, &root(), "t", &Cfg { max_trials: 3, ..cfg4() }, &SwarmCfg { width: 3, concurrency: 1 }); assert_eq!(h.peak.load(Ordering::SeqCst), 1); }
    #[test] fn the_best_admitted_theory_is_applied_and_the_other_that_passed_is_refused_not_merged() {
        let h = fake(vec![25.0, 40.0, 5.0, 0.0], 5); let s = run_with(&h, &root(), "t", &cfg4(), &SwarmCfg { width: 4, concurrency: 3 });
        assert_eq!(s.status, "goal_met"); let d = h.decided.lock().unwrap().clone();
        assert!(d.contains(&(2, true)) && d.contains(&(1, false)) && d.contains(&(3, false)) && d.contains(&(4, false)), "{d:?}"); assert_eq!(d.iter().filter(|(_, a)| *a).count(), 1, "exactly one theory applied");
        assert_eq!(s.trials[1].outcome, "admitted"); assert_eq!(s.trials[0].outcome, "passed_not_applied"); assert!(s.trials[0].why.contains("never merged"));
        assert!(s.trials[1].why.contains("gain 40") && !s.trials[1].experiment.is_null(), "a trial keeps why and the experiment"); assert_eq!(h.told.lock().unwrap().len(), 1, "one report to the Leader");
    }
    #[test] fn a_second_batch_runs_when_the_first_had_no_winner_and_the_theory_cap_holds() {
        let h = fake(vec![0.0, 0.0, 0.0, 0.0, 30.0, 0.0], 2); let s = run_with(&h, &root(), "t", &Cfg { max_trials: 6, ..cfg4() }, &SwarmCfg { width: 4, concurrency: 3 });
        assert_eq!(s.trials.len(), 6, "a batch of 4 then a batch of 2"); assert_eq!(s.status, "goal_met"); assert!(h.decided.lock().unwrap().contains(&(5, true)));
    }
    #[test] fn the_purse_is_shared_so_a_batch_cannot_overshoot_it_and_an_empty_bank_waits() {
        assert!(share(1.0, 4, 4) <= 0.25 + 1e-9, "a quarter each at most"); assert!(share(0.04, 4, 4) >= 0.05); assert_eq!(pick_winner(&[(0, 25.0), (2, 25.0), (1, 40.0)]), Some(1)); assert_eq!(pick_winner(&[(3, 20.0), (1, 20.0)]), Some(3)); assert_eq!(pick_winner(&[]), None);
        let mut h = fake(vec![50.0], 1); h.tasks = (0, 0); let s = run_with(&h, &root(), "t", &cfg4(), &SwarmCfg::default()); assert_eq!(s.status, "needs_tasks"); assert!(s.trials.is_empty());
        assert_eq!(SwarmCfg { width: 99, concurrency: 0 }.tidy(), SwarmCfg { width: 8, concurrency: 1 });
    }
}
