//! THE EXPERIMENT (RFC 0013 §3.3, §8, step 5): the current version against a candidate, on the replay bank, several runs each, staged and under a budget.
//! Stage 1 is a cheap screen on the tune tasks (one run each): a candidate that is clearly worse stops there. Stage 2 runs the held-out tasks `repeats` times; the candidate is admitted only when
//! its LOWER confidence bound beats the current version's, nothing is lost beyond the regression budget, and the money held. The runner is a closure (a real team run, or a scripted one): this file spends nothing itself.
use crate::team_replay::Task;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const SCREEN_DROP: f64 = 0.25;
pub const MIN_RUNS: u32 = 5;
pub const DEFAULT_REGRESSION_BUDGET: f64 = 0.10;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outcome { pub pass: bool, pub cost_usd: f64, pub wall_ms: i64, pub gate_stop: bool }
/// WHAT A CHANGE IS FOR (the operator 2026-11-22: "get better on something by at least 20%"): the experiment used to admit on QUALITY only, so a candidate that was cheaper or faster with the same pass rate was thrown out as "could be noise". A goal says which number must move: quality (the lower bound of the pass rate, as before), cost or time (per run, down by at least `min_gain`, while the pass rate does not fall).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Goal { #[default] Quality, Cost, Time }
impl Goal { pub fn parse(s: &str) -> Option<Goal> { match s { "quality" | "pass" | "right" => Some(Goal::Quality), "cost" | "cheaper" => Some(Goal::Cost), "time" | "faster" | "speed" => Some(Goal::Time), _ => None } } pub fn word(self) -> &'static str { match self { Goal::Quality => "quality", Goal::Cost => "cost", Goal::Time => "time" } } }
/// How much better a candidate is than the current version on the goal, as a fraction (0.2 = 20% better): cost and time are lower-is-better, quality is the pass rate against a pass rate that cannot grow past 100%. Pure.
pub fn gain(goal: Goal, cur: &Arm, cand: &Arm) -> f64 {
    match goal { Goal::Cost => if cur.avg_cost_usd > 0.0 { (cur.avg_cost_usd - cand.avg_cost_usd) / cur.avg_cost_usd } else { 0.0 }, Goal::Time => if cur.avg_wall_ms > 0.0 { (cur.avg_wall_ms - cand.avg_wall_ms) / cur.avg_wall_ms } else { 0.0 }, Goal::Quality => if cur.rate > 0.0 { (cand.rate - cur.rate) / cur.rate } else if cand.rate > 0.0 { 1.0 } else { 0.0 } }
}
/// THE GAIN A RUN-TO-RUN NOISE CANNOT EXPLAIN (found by the first real session, 2026-11-22: a change admitted at 24% cheaper was measured again and was not cheaper at all, the cost of the same version moved 25% between two batches of six runs): the current version counted at its mean less a standard error, the candidate at its mean plus one. Cost and time only; quality has its lower bound. Pure.
pub fn sure_gain(goal: Goal, cur: &Arm, cand: &Arm) -> f64 {
    let se = |sd: f64, n: u32| if n == 0 { 0.0 } else { sd / (n as f64).sqrt() };
    match goal { Goal::Cost => if cur.avg_cost_usd > 0.0 { ((cur.avg_cost_usd - se(cur.sd_cost_usd, cur.n)) - (cand.avg_cost_usd + se(cand.sd_cost_usd, cand.n))) / cur.avg_cost_usd } else { 0.0 }, Goal::Time => if cur.avg_wall_ms > 0.0 { ((cur.avg_wall_ms - se(cur.sd_wall_ms, cur.n)) - (cand.avg_wall_ms + se(cand.sd_wall_ms, cand.n))) / cur.avg_wall_ms } else { 0.0 }, Goal::Quality => gain(goal, cur, cand) }
}
#[derive(Clone, Debug)]
pub struct Plan { pub repeats: u32, pub budget_usd: f64, /** what earlier accepted changes have not yet used of the allowance, as a share of held-out tasks */ pub regression_left: f64, pub goal: Goal, /** the least gain on the goal that admits a change (cost and time goals), as a fraction */ pub min_gain: f64 }
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Arm { pub n: u32, pub passed: u32, pub rate: f64, pub lower: f64, pub avg_cost_usd: f64, pub avg_wall_ms: f64, pub gate_stops: u32, /** how much a single run's cost and time vary: the noise a gain must stand out from */ #[serde(default)] pub sd_cost_usd: f64, #[serde(default)] pub sd_wall_ms: f64 }
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report { /** "screened_out" or "full" */ pub stage: String, pub current: Arm, pub candidate: Arm, /** held-out tasks the current version passes every time and the candidate does not */ pub regressions: Vec<String>, pub regression_share: f64, pub spent_usd: f64, pub budget_hit: bool, pub admitted: bool, pub why: String }

/// The spread of a list of numbers (sample standard deviation). Pure.
fn sd(v: &[f64]) -> f64 { if v.len() < 2 { return 0.0; } let m = v.iter().sum::<f64>() / v.len() as f64; (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt() }
#[derive(Default)]
struct Acc { n: u32, passed: u32, cost: f64, wall: i64, stops: u32, per: BTreeMap<String, (u32, u32)>, costs: Vec<f64>, walls: Vec<f64> }
impl Acc {
    fn merge(&mut self, o: &Acc) { self.n += o.n; self.passed += o.passed; self.cost += o.cost; self.wall += o.wall; self.stops += o.stops; self.costs.extend(&o.costs); self.walls.extend(&o.walls); for (k, (n, p)) in &o.per { let e = self.per.entry(k.clone()).or_insert((0, 0)); e.0 += n; e.1 += p; } }
    fn add(&mut self, task: &str, o: &Outcome) { self.n += 1; self.passed += o.pass as u32; self.cost += o.cost_usd; self.wall += o.wall_ms; self.stops += o.gate_stop as u32; self.costs.push(o.cost_usd); self.walls.push(o.wall_ms as f64); let e = self.per.entry(task.into()).or_default(); e.0 += 1; e.1 += o.pass as u32; }
    fn arm(&self) -> Arm { let n = self.n.max(1) as f64; Arm { n: self.n, passed: self.passed, rate: if self.n == 0 { 0.0 } else { self.passed as f64 / self.n as f64 }, lower: crate::team_bench::wilson(self.passed, self.n).0, avg_cost_usd: self.cost / n, avg_wall_ms: self.wall as f64 / n, gate_stops: self.stops, sd_cost_usd: sd(&self.costs), sd_wall_ms: sd(&self.walls) } }
}

/// Run both arms. `run(arm, task)` is "current" or "candidate"; an Err is a run that could not be made (counted as a failure and noted).
pub fn run(tune: &[Task], held: &[Task], plan: &Plan, runner: &mut dyn FnMut(&str, &Task) -> Result<Outcome, String>) -> Result<Report, String> {
    if held.is_empty() { return Err("no held-out tasks: a change cannot be admitted on tasks it was tuned on (save a few more runs as tasks)".into()); }
    let bad = crate::team_replay::disagreeing(held); if !bad.is_empty() { return Err(format!("the checks of {} task(s) contradict what you labelled by hand ({}): the judge is wrong there, so nothing can be admitted on it", bad.len(), bad.join(", "))); }
    if plan.budget_usd <= 0.0 { return Err("set an improvement budget first: an experiment spends real model calls".into()); }
    let (mut cur, mut cand, mut spent, mut hit) = (Acc::default(), Acc::default(), 0.0_f64, false);
    let mut one = |arm: &str, t: &Task, acc: &mut Acc, spent: &mut f64, hit: &mut bool| -> bool {
        if *spent >= plan.budget_usd { *hit = true; return false; }
        let o = runner(arm, t).unwrap_or(Outcome { pass: false, cost_usd: 0.0, wall_ms: 0, gate_stop: false }); *spent += o.cost_usd; acc.add(&t.id, &o); true
    };
    // stage 1: the screen
    for t in tune { if !(one("current", t, &mut cur, &mut spent, &mut hit) && one("candidate", t, &mut cand, &mut spent, &mut hit)) { break; } }
    if !hit && !tune.is_empty() && cand.arm().rate + SCREEN_DROP < cur.arm().rate {
        return Ok(Report { stage: "screened_out".into(), current: cur.arm(), candidate: cand.arm(), regressions: vec![], regression_share: 0.0, spent_usd: spent, budget_hit: false, admitted: false, why: format!("screened out: {:.0}% on the tune tasks against {:.0}% for the current version", cand.arm().rate * 100.0, cur.arm().rate * 100.0) });
    }
    // stage 2: the held-out tasks, repeated, the two arms side by side
    let (mut hc, mut hn) = (Acc::default(), Acc::default()); let (mut ec, mut en, mut oc, mut on_) = (Acc::default(), Acc::default(), Acc::default(), Acc::default());   // the same runs in two interleaved halves: a gain that is real shows in both
    'all: for rep in 0..plan.repeats.max(1) { for t in held { let (sc, sn) = if rep % 2 == 0 { (&mut ec, &mut en) } else { (&mut oc, &mut on_) }; if !(one("current", t, sc, &mut spent, &mut hit) && one("candidate", t, sn, &mut spent, &mut hit)) { break 'all; } } }
    hc.merge(&ec); hc.merge(&oc); hn.merge(&en); hn.merge(&on_);
    let (ca, na) = (hc.arm(), hn.arm());
    let regressions: Vec<String> = hc.per.iter().filter(|(_, (n, p))| *n > 0 && n == p).filter(|(id, _)| hn.per.get(*id).map(|(n, p)| p < n).unwrap_or(true)).map(|(id, _)| id.clone()).collect();
    let share = regressions.len() as f64 / held.len() as f64;
    let why = if hit { "the improvement budget ran out before the experiment finished: not admitted".to_string() }
        else if ca.n < MIN_RUNS || na.n < MIN_RUNS { format!("only {} runs per version (at least {MIN_RUNS} needed): too few to admit", ca.n.min(na.n)) }
        else if na.rate < ca.rate { format!("the candidate passes less often ({:.0}% against {:.0}%)", na.rate * 100.0, ca.rate * 100.0) }
        else if plan.goal != Goal::Quality && sure_gain(plan.goal, &ca, &na) < plan.min_gain { format!("the candidate is only {:.0}% better on {} once the run-to-run noise is counted ({:.0}% on the averages; at least {:.0}% is the goal): {} against {}", sure_gain(plan.goal, &ca, &na) * 100.0, plan.goal.word(), gain(plan.goal, &ca, &na) * 100.0, plan.min_gain * 100.0, if plan.goal == Goal::Cost { format!("${:.3} a run", na.avg_cost_usd) } else { format!("{:.0} s a run", na.avg_wall_ms / 1000.0) }, if plan.goal == Goal::Cost { format!("${:.3}", ca.avg_cost_usd) } else { format!("{:.0} s", ca.avg_wall_ms / 1000.0) }) }
        else if plan.goal != Goal::Quality && ec.n > 0 && oc.n > 0 && gain(plan.goal, &ec.arm(), &en.arm()).min(gain(plan.goal, &oc.arm(), &on_.arm())) < plan.min_gain / 2.0 { format!("the gain is not stable: {:.0}% in the first runs, {:.0}% in the later ones (each must be at least half the goal, {:.0}%): it may be noise", gain(plan.goal, &ec.arm(), &en.arm()) * 100.0, gain(plan.goal, &oc.arm(), &on_.arm()) * 100.0, plan.min_gain * 50.0) }
        else if plan.goal == Goal::Quality && na.lower <= ca.lower { format!("the candidate's lower bound ({:.0}%) does not beat the current version's ({:.0}%): the gain could be noise", na.lower * 100.0, ca.lower * 100.0) }
        else if share > plan.regression_left { format!("it would lose {} task(s) the current version always passes ({:.0}% of held-out), more than the regression budget left ({:.0}%)", regressions.len(), share * 100.0, plan.regression_left * 100.0) }
        else if plan.goal != Goal::Quality { format!("admitted: {:.0}% better on {} even counting the noise ({:.0}% on the averages; {}), the pass rate held ({:.0}% against {:.0}%), {} regression(s)", sure_gain(plan.goal, &ca, &na) * 100.0, plan.goal.word(), gain(plan.goal, &ca, &na) * 100.0, if plan.goal == Goal::Cost { format!("${:.3} against ${:.3} a run", na.avg_cost_usd, ca.avg_cost_usd) } else { format!("{:.0} s against {:.0} s a run", na.avg_wall_ms / 1000.0, ca.avg_wall_ms / 1000.0) }, na.rate * 100.0, ca.rate * 100.0, regressions.len()) }
        else { format!("admitted: lower bound {:.0}% against {:.0}%, {} regression(s)", na.lower * 100.0, ca.lower * 100.0, regressions.len()) };
    let admitted = why.starts_with("admitted");
    Ok(Report { stage: "full".into(), current: ca, candidate: na, regressions, regression_share: share, spent_usd: spent, budget_hit: hit, admitted, why })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn task(id: &str) -> Task { Task { oracle: vec![], id: id.into(), ask: id.into(), checks: vec![], from_run: String::new(), added_ms: 0, split: "heldout".into() } }
    fn plan(budget: f64) -> Plan { Plan { repeats: 3, budget_usd: budget, regression_left: 0.10, goal: Goal::Quality, min_gain: 0.2 } }
    fn out(pass: bool) -> Result<Outcome, String> { Ok(Outcome { pass, cost_usd: 0.01, wall_ms: 1000, gate_stop: false }) }
    #[test] fn a_clear_gain_is_admitted_noise_a_regression_a_screen_miss_and_an_empty_purse_are_not() {
        let held = vec![task("h1"), task("h2"), task("h3")]; let tune = vec![task("a"), task("b")];
        assert!(run(&tune, &[], &plan(1.0), &mut |_, _| out(true)).is_err() && run(&tune, &held, &plan(0.0), &mut |_, _| out(true)).is_err(), "no held-out tasks, no budget: no experiment");
        let mut wrong = held.clone(); wrong[0].checks = vec![crate::team_check::Rule { id: "w".into(), kind: "words".into(), max: Some(1.0), ..Default::default() }]; wrong[0].oracle = vec![crate::team_replay::Oracle { text: "one two three".into(), pass: true }];
        assert!(run(&tune, &wrong, &plan(1.0), &mut |_, _| out(true)).unwrap_err().contains("contradict"), "a judge that disagrees with the person's labels admits nothing");
        // current fails h1 half the time, the candidate never: a gain
        let mut k = 0; let r = run(&tune, &held, &plan(1.0), &mut |arm, t| { if arm == "current" && t.id == "h1" { k += 1; return out(k % 2 == 0); } out(true) }).unwrap();
        assert_eq!(r.stage, "full"); assert!(r.admitted, "{}", r.why); assert!(r.candidate.lower > r.current.lower && r.regressions.is_empty());
        // the candidate is the same as the current: no gain, not admitted
        let r = run(&tune, &held, &plan(1.0), &mut |_, _| out(true)).unwrap(); assert!(!r.admitted && r.why.contains("could be noise"), "{}", r.why);
        // the candidate breaks h2, which the current always passes: a regression, and a worse rate
        let r = run(&tune, &held, &plan(1.0), &mut |arm, t| out(!(arm == "candidate" && t.id == "h2"))).unwrap(); assert!(!r.admitted && r.regressions == vec!["h2".to_string()]);
        // clearly worse on tune: stops at the screen and spends little
        let r = run(&tune, &held, &plan(1.0), &mut |arm, _| out(arm == "current")).unwrap(); assert_eq!(r.stage, "screened_out"); assert!(r.spent_usd < 0.1 && !r.admitted);
        // the budget runs out half way: never admitted
        let r = run(&tune, &held, &plan(0.05), &mut |_, _| out(true)).unwrap(); assert!(r.budget_hit && !r.admitted);
    }
    #[test] fn a_cheaper_candidate_with_the_same_pass_rate_is_admitted_for_a_cost_goal_only_when_it_is_20_percent_cheaper() {
        let held = vec![task("h1"), task("h2"), task("h3")]; let tune = vec![task("a")];
        let costly = |arm: &str, c_cur: f64, c_cand: f64| -> Result<Outcome, String> { Ok(Outcome { pass: true, cost_usd: if arm == "current" { c_cur } else { c_cand }, wall_ms: 1000, gate_stop: false }) };
        let mut p = plan(10.0); p.goal = Goal::Cost; p.min_gain = 0.2;
        let r = run(&tune, &held, &p, &mut |arm, _| costly(arm, 0.10, 0.07)).unwrap(); assert!(r.admitted, "{}", r.why); assert!(r.why.contains("better on cost") && r.why.contains("30% on the averages"), "{}", r.why);
        let r = run(&tune, &held, &p, &mut |arm, _| costly(arm, 0.10, 0.09)).unwrap(); assert!(!r.admitted && r.why.contains("only 10% better on cost"), "{}", r.why);
        // cheaper but it fails more: never
        let r = run(&tune, &held, &p, &mut |arm, t| Ok(Outcome { pass: !(arm == "candidate" && t.id == "h1"), cost_usd: if arm == "current" { 0.10 } else { 0.02 }, wall_ms: 1000, gate_stop: false })).unwrap(); assert!(!r.admitted && r.why.contains("passes less often"), "{}", r.why);
        // faster for a time goal
        p.goal = Goal::Time; let r = run(&tune, &held, &p, &mut |arm, _| Ok(Outcome { pass: true, cost_usd: 0.01, wall_ms: if arm == "current" { 60_000 } else { 30_000 }, gate_stop: false })).unwrap(); assert!(r.admitted && r.why.contains("on time"), "{}", r.why);
        assert_eq!(Goal::parse("cheaper"), Some(Goal::Cost)); assert_eq!(Goal::parse("nonsense"), None);
        let a = |rate: f64, cost: f64, ms: f64| Arm { n: 5, passed: 0, rate, lower: rate, avg_cost_usd: cost, avg_wall_ms: ms, gate_stops: 0, sd_cost_usd: 0.0, sd_wall_ms: 0.0 };
        assert!((gain(Goal::Cost, &a(1.0, 0.10, 1.0), &a(1.0, 0.07, 1.0)) - 0.3).abs() < 1e-9); assert!((gain(Goal::Quality, &a(0.5, 0.1, 1.0), &a(0.75, 0.1, 1.0)) - 0.5).abs() < 1e-9); assert_eq!(gain(Goal::Cost, &a(1.0, 0.0, 1.0), &a(1.0, 0.0, 1.0)), 0.0);
    }
    #[test] fn a_cost_gain_inside_the_run_to_run_noise_is_not_admitted_the_bug_the_first_real_session_found() {
        let held = vec![task("h1"), task("h2")]; let tune = vec![task("a")]; let mut p = plan(10.0); p.goal = Goal::Cost; p.min_gain = 0.2; p.repeats = 3;
        // the candidate is 25% cheaper on average, but its runs swing from $0.02 to $0.13: noise, not a gain
        let mut k = 0u32; let r = run(&tune, &held, &p, &mut |arm, _| { let c = if arm == "current" { 0.10 } else { k += 1; if k % 2 == 0 { 0.02 } else { 0.13 } }; Ok(Outcome { pass: true, cost_usd: c, wall_ms: 1000, gate_stop: false }) }).unwrap();
        assert!(!r.admitted && r.why.contains("noise"), "{}", r.why);
        // steady runs 25% cheaper: admitted
        let r = run(&tune, &held, &p, &mut |arm, _| Ok(Outcome { pass: true, cost_usd: if arm == "current" { 0.10 } else { 0.075 }, wall_ms: 1000, gate_stop: false })).unwrap(); assert!(r.admitted, "{}", r.why);
        assert!((sd(&[1.0, 3.0]) - 2.0_f64.sqrt()).abs() < 1e-9 && sd(&[5.0]) == 0.0);
    }
}
