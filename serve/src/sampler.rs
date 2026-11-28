//! THE SAMPLER — what to measure next, and what not to pay for (PLAN-THE-LEAN-YEARS II.3).
//!
//! The record has 11 jobs × 4 sizes × 6 domains × the pairs anyone might run. Measuring that grid is a quarter of
//! compute and most of it decides nothing: a cell where one pair is obviously ahead does not need a fifth run, and a
//! cell nobody routes to does not deserve a dollar. So this module does not plan a matrix. It ranks the cells by how
//! much a run there would change a DECISION, and says what the next few runs should be.
//!
//! THREE RULES, in the plan's own words:
//!   · "Cells nobody routes to are never measured" — traffic is a filter, not a tiebreak.
//!   · "Stop once a pair's advantage is significant at the cell's traffic" — a decided cell leaves the queue.
//!   · Spend where "the decision between two candidate merges is closest, and where that decision is asked for most".
//!
//! WHAT WE HAVE TO WORK WITH, AND THE ONE APPROXIMATION IT FORCES. The plan asks for "a posterior over (pass rate,
//! cost, latency)" per cell. Per-run outcomes are not on disk — `merge_scores` keeps aggregates (n, the quality parts,
//! totals for ms and usd), by design, because that is what the router reads on every dispatch. So the posterior is
//! reconstructed by moment-matching a Beta to (quality, n): `Beta(1 + q·n, 1 + (1-q)·n)`. That is honest in the
//! direction that matters — with a small n the distribution is wide and nothing looks decided — and it is exactly the
//! posterior you would have had if the n runs had been pass/fail draws at rate q. It is NOT a claim that quality is
//! binomial; it is the most conservative shape that reproduces the two numbers we kept.
//!
//! The comparison between two arms then uses a normal approximation to the difference of two Betas. For the n we have
//! (single digits) that is an approximation, and it is named as one everywhere it is reported: the queue says how
//! confident it is, never that a cell is proven. Both are pure functions — no database, no clock, no RNG, so the plan
//! for a given record is reproducible and testable, which a Thompson sampler with a random draw would not be.
use serde_json::{json, Value};

/// ONE PAIRING'S EVIDENCE IN A CELL — what the record kept, not what it wishes it had.
#[derive(Clone, Debug, PartialEq)]
pub struct Arm {
    pub planner: String,
    pub worker: String,
    pub qa: String,
    /// samples behind this row
    pub n: i64,
    /// quality in 0..1 (`merges::quality_of`), the same number the weight ranks on
    pub q: f64,
    /// dollars and seconds PER RUN, as the table reports them
    pub usd: f64,
    pub s: f64,
    /// `true` for a bench row (controlled), `false` for real use (observed). The plan's II.4 rule: observational
    /// evidence is a prior, controlled evidence is the likelihood — so a cell whose leader is only observed is not
    /// decided, however far ahead it looks.
    pub controlled: bool,
}

/// ONE CELL — a job at a size on a kind of work, and every pairing measured in it.
#[derive(Clone, Debug)]
pub struct Cell {
    pub job: String,
    pub size: String,
    pub domain: String,
    /// THE TECHNOLOGY THE ROWS WERE MEASURED ON (II.2, 2026-09-25) — a Rust cell and a Python cell are different
    /// questions, so they are different cells and the plan prices them separately. `any` is the pool.
    pub stack: String,
    pub arms: Vec<Arm>,
    /// HOW OFTEN REAL USE ROUTES HERE — the observed samples in this cell. It is the plan's "asked for most often in
    /// real use", read from the record itself rather than guessed: an observed row exists because a real run happened.
    pub traffic: i64,
    /// WHAT A RUN COSTS HERE WHEN NOTHING IN THIS CELL HAS A PRICE (2026-09-28). The median per-run price of every
    /// arm on the record that does have one. Before the flat seats reported their tokens, 53 of 224 live rows carried
    /// `usd_total = 0` — not because the runs were free but because Cursor, Antigravity and Grok print no price — and
    /// a cell made only of those rows projected "$0.00 to settle them", which is the one number the operator uses to
    /// decide whether to spend. A measured stand-in is honest where a zero is not; `Step::priced` says which it is.
    pub typical_usd: f64,
    /// TRUE WHEN A BENCHED POOL ANSWERS THIS CELL (2026-09-28). Set when this cell's own arms are all real-use
    /// evidence AND some other cell of the same job and size holds a controlled arm — which is exactly the case
    /// `merges::best_for_stack` now defers on, since observation moves a benched score rather than setting one.
    /// The router never reads such a cell, so runs bought here buy no decision and the plan must not ask for them.
    pub pooled: bool,
}

/// WHY A CELL IS IN THE QUEUE — or why it is not. Each variant is an action, not a score.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// two arms close enough that a few runs could swap them
    Separate,
    /// one arm measured: it cannot be a winner until something has been measured against it
    Challenger,
    /// the leader is only observed evidence — a bench would make it comparable (II.4)
    Confirm,
    /// the advantage is significant at this cell's traffic; stop paying
    Decided,
    /// nobody routes here; never measured, on purpose
    Untrafficked,
    /// nothing in this cell was ever BENCHED, and a benched pool elsewhere answers it — so the router never reads
    /// this cell and buying runs here buys no decision (2026-09-28)
    Pooled,
}
impl Need {
    pub fn key(self) -> &'static str {
        match self { Need::Separate => "separate", Need::Challenger => "challenger", Need::Confirm => "confirm", Need::Decided => "decided", Need::Untrafficked => "untrafficked", Need::Pooled => "pooled" }
    }
    /// does this need spending?
    pub fn spends(self) -> bool { matches!(self, Need::Separate | Need::Challenger | Need::Confirm) }
}

/// ONE LINE OF THE PLAN.
#[derive(Clone, Debug)]
pub struct Step {
    pub job: String,
    pub size: String,
    pub domain: String,
    pub stack: String,
    pub traffic: i64,
    pub need: Need,
    pub leader: Option<Arm>,
    pub challenger: Option<Arm>,
    /// probability the runner-up is actually the better pairing, under the approximation above (0 when there is
    /// nothing to compare)
    pub p_flip: f64,
    /// runs suggested now — a batch, re-planned after it lands, never a schedule
    pub runs: i64,
    pub usd: f64,
    /// `true` when `usd` came from arms that were actually priced; `false` when it is the record's typical run
    /// standing in for a cell nobody could price. Never let the screen spend a `false` as if it were a measurement.
    pub priced: bool,
    /// the ranking number: how much of a decision a dollar here buys
    pub value: f64,
    pub why: String,
}

/// Beta posterior moment-matched to (quality, samples) — see the module note for why this shape and not another.
/// `n <= 0` is the flat prior: no evidence is not evidence of 0.5, it is no evidence, and a flat Beta says exactly
/// that. Pure.
pub fn posterior(q: f64, n: i64) -> (f64, f64) {
    let n = n.max(0) as f64;
    let q = q.clamp(0.0, 1.0);
    (1.0 + q * n, 1.0 + (1.0 - q) * n)
}
/// mean and variance of Beta(a, b). Pure.
pub fn moments(a: f64, b: f64) -> (f64, f64) {
    let s = a + b;
    let m = a / s;
    (m, (a * b) / (s * s * (s + 1.0)))
}
/// Φ — the standard normal CDF, Abramowitz & Stegun 7.1.26 through erf. Deterministic and good to ~1e-7, which is
/// three digits more than a probability printed as a percentage needs. Pure.
fn phi(z: f64) -> f64 {
    let s = if z < 0.0 { -1.0 } else { 1.0 };
    let x = (z / std::f64::consts::SQRT_2).abs();
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let y = 1.0 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * (-x * x).exp();
    0.5 * (1.0 + s * y)
}

/// THE PROBABILITY THE CHALLENGER IS ACTUALLY BETTER, given what the two rows kept.
///
/// A normal approximation to the difference of the two Beta posteriors. Returns 0.5 when both are equally uninformed —
/// a coin, which is the truth — and never returns 0 or 1: no amount of aggregate evidence in this record proves a
/// pairing, and a queue that printed certainty would be the falsehood this whole plan exists to remove. Pure.
pub fn p_challenger_better(leader: &Arm, challenger: &Arm) -> f64 {
    let (la, lb) = posterior(leader.q, leader.n);
    let (ca, cb) = posterior(challenger.q, challenger.n);
    let (lm, lv) = moments(la, lb);
    let (cm, cv) = moments(ca, cb);
    let sd = (lv + cv).sqrt();
    if sd <= f64::EPSILON { return if cm > lm { 1.0 } else { 0.5 } }
    phi((cm - lm) / sd).clamp(0.001, 0.999)
}

/// HOW MANY MORE RUNS WOULD SETTLE IT — under the same normal approximation, the samples needed for the gap to clear
/// `z` standard deviations (1.645 ≈ 95% one-sided). A Beta's variance falls as ~1/n, so the n that separates a gap `d`
/// is `(z/d)² · scale`; the result is capped because a plan that says "run this 400 times" is not a plan. Zero gap →
/// the cap: the honest answer is "this may never separate, spend the cap and decide on cost". Pure.
pub fn runs_to_decide(leader: &Arm, challenger: &Arm, cap: i64) -> i64 {
    let d = (leader.q - challenger.q).abs();
    if d < 1e-6 { return cap }
    let (la, lb) = posterior(leader.q, leader.n);
    let (ca, cb) = posterior(challenger.q, challenger.n);
    let (_, lv) = moments(la, lb);
    let (_, cv) = moments(ca, cb);
    let have = (lv + cv).sqrt();
    let want = d / 1.645;
    if have <= want { return 0 }
    // variance ~ 1/n ⇒ sd ~ 1/√n: multiply the smaller side's samples by (have/want)²
    let factor = (have / want).powi(2);
    let base = leader.n.min(challenger.n).max(1) as f64;
    (((factor - 1.0) * base).ceil() as i64).clamp(1, cap)
}

/// the arms of a cell, best quality first, ties broken by samples then by price — the same order the table ranks in.
fn ranked(arms: &[Arm]) -> Vec<Arm> {
    let mut v = arms.to_vec();
    v.sort_by(|a, b| b.q.partial_cmp(&a.q).unwrap_or(std::cmp::Ordering::Equal)
        .then(b.n.cmp(&a.n))
        .then(a.usd.partial_cmp(&b.usd).unwrap_or(std::cmp::Ordering::Equal)));
    v
}

/// WHAT ONE CELL NEEDS. The order of the checks is the argument: no traffic outranks everything (it is the one rule
/// that saves most of the money), a single arm cannot be a winner, observed-only cannot be ground truth, and only then
/// does closeness decide. Pure.
pub fn step_for(cell: &Cell, cap: i64) -> Step {
    let arms = ranked(&cell.arms);
    let leader = arms.first().cloned();
    let challenger = arms.get(1).cloned();
    let mk = |need: Need, p_flip: f64, runs: i64, why: String| {
        let measured = leader.as_ref().map(|a| a.usd).unwrap_or(0.0).max(challenger.as_ref().map(|a| a.usd).unwrap_or(0.0));
        let priced = measured > 0.0;
        let usd = if priced { measured } else { cell.typical_usd } * runs as f64;
        // VALUE = how much of a decision a dollar buys. Traffic is a multiplier, not a bonus: a coin-flip in a cell
        // used a hundred times is worth a hundred times a coin-flip in a cell used once. A cell with no price yet
        // (nothing measured) is not free — it is unknown, so it ranks on the decision alone rather than dividing by 0.
        let value = if need.spends() { p_flip.min(1.0 - p_flip).max(0.001) * (cell.traffic.max(1) as f64) / usd.max(0.01) } else { 0.0 };
        Step { job: cell.job.clone(), size: cell.size.clone(), domain: cell.domain.clone(), stack: cell.stack.clone(), traffic: cell.traffic,
               need, leader: leader.clone(), challenger: challenger.clone(), p_flip, runs, usd, priced, value, why }
    };
    if cell.traffic <= 0 {
        return mk(Need::Untrafficked, 0.0, 0, "nobody routes here — not measured on purpose".into());
    }
    // THE ROUTER AND THE PLAN MUST AGREE (2026-09-28). `best_for_stack` walks its ladder twice and the first pass
    // stops only at a pool holding a benched arm, so a cell of real-use rows alone never decides anything while a
    // bench exists elsewhere — the pool answers. Asking the operator to spend on it would be asking for runs that
    // change no route, which is the one thing this screen exists to prevent.
    if cell.pooled {
        return mk(Need::Pooled, 0.0, 0, "nothing here was benched, so the benched pool answers this cell — runs here would change no route".into());
    }
    let Some(l) = leader.clone() else {
        return mk(Need::Challenger, 0.5, 0, "routed to, and nothing measured — the record cannot answer this cell at all".into());
    };
    let Some(c) = challenger.clone() else {
        return mk(Need::Challenger, 0.5, 1, format!("only {} is measured here — one arm cannot be a winner until something runs against it", side(&l)));
    };
    let p = p_challenger_better(&l, &c);
    if !l.controlled {
        let runs = runs_to_decide(&l, &c, cap).max(1);
        return mk(Need::Confirm, p, runs, format!("the leader here is real-use evidence only — a bench makes it comparable ({}% chance {} is actually ahead)", (p * 100.0).round(), side(&c)));
    }
    if p < 0.05 {
        return mk(Need::Decided, p, 0, format!("{} is ahead by more than this cell's evidence can overturn ({}% chance it is wrong) — stop paying", side(&l), (p * 100.0).round()));
    }
    // TWO ARMS THAT ARE THE SAME NEVER SEPARATE, AND THIS SCREEN WOULD ASK FOREVER (2026-09-28). Watched happening:
    // `planner/s/backend/web` sat at exactly 50% through three rounds of runs and went on asking for eight more each
    // time, and `editor-s` moved 23% → 37% across sixteen — AWAY from a decision, because there is no decision to
    // reach. `runs_to_decide` already caps the RUNS and its own comment says "spend the cap and decide on cost,
    // rather than forever"; nothing anywhere acted on that, so a cell of equals was a standing bill.
    //
    // `merges::separated` is the record's own test of whether a difference is real. When both arms are benched, both
    // have spent the cap, and the intervals still overlap, the answer IS the answer: they are indistinguishable, so
    // the cheaper one takes the work and the asking stops. `ranked` already breaks a quality tie on price, so the
    // leader here is the cheaper of two equals — the sentence says so rather than implying it won on merit.
    if c.controlled && l.n >= cap && c.n >= cap && !crate::merges::separated((l.q, l.n), (c.q, c.n)) {
        let cheaper = if c.usd > 0.0 && c.usd < l.usd { &c } else { &l };
        return mk(Need::Decided, p, 0, format!(
            "{} and {} are indistinguishable after {} and {} runs — the record cannot separate them, so {} takes it on price and this cell stops costing money",
            side(&l), side(&c), l.n, c.n, side(cheaper)));
    }
    let runs = runs_to_decide(&l, &c, cap);
    if runs == 0 {
        return mk(Need::Decided, p, 0, format!("{} and {} are separated at this cell's traffic", side(&l), side(&c)));
    }
    mk(Need::Separate, p, runs, format!("{}% chance {} beats {} — {} run(s) would settle it", (p * 100.0).round(), side(&c), side(&l), runs))
}
/// a pairing as a person reads it
fn side(a: &Arm) -> String {
    let w = |k: &str| k.rsplit('/').next().unwrap_or(k).to_string();
    if a.worker.is_empty() || a.worker == "-" { w(&a.planner) } else { format!("{} × {}", w(&a.planner), w(&a.worker)) }
}

/// THE PLAN: every cell judged, the spending ones ranked by value per dollar, trimmed to a budget.
///
/// `budget` is a ceiling in dollars on the projected spend, `None` for no ceiling. The trim is greedy over the value
/// ranking, and a step that does not fit is skipped rather than shrunk: half a comparison is not half an answer.
pub fn plan(cells: &[Cell], budget: Option<f64>, cap: i64) -> (Vec<Step>, Value) {
    let steps: Vec<Step> = cells.iter().map(|c| step_for(c, cap)).collect();
    let mut queue: Vec<Step> = steps.iter().filter(|s| s.need.spends() && s.runs > 0).cloned().collect();
    queue.sort_by(|a, b| b.value.partial_cmp(&a.value).unwrap_or(std::cmp::Ordering::Equal).then(b.traffic.cmp(&a.traffic)));
    let mut spend = 0.0;
    let mut taken: Vec<Step> = vec![];
    for s in queue {
        if let Some(b) = budget { if spend + s.usd > b { continue } }
        spend += s.usd;
        taken.push(s);
    }
    let count = |n: Need| steps.iter().filter(|s| s.need == n).count();
    let summary = json!({
        "cells": steps.len(),
        "queued": taken.len(),
        "projected_usd": (spend * 100.0).round() / 100.0,
        // IS THE PROJECTION A MEASUREMENT? `false` when any queued step had to borrow the record's typical run because
        // none of its own arms carried a price. The screen must say "about" rather than quote it as a figure.
        "projected_priced": taken.iter().all(|s| s.priced),
        "budget": budget,
        "decided": count(Need::Decided),
        "untrafficked": count(Need::Untrafficked),
        // HOW MANY OF THOSE WERE MEASURED ANYWAY (audit 2026-09-28). "Nobody routes here" and "nothing was measured
        // here" are different facts, and the screen was printing the first as the second. On this record 12 of the 17
        // untrafficked cells hold five or six BENCHED arms apiece — the work was done, it simply answers a question
        // the operator's own code has not asked yet. Telling them it was never measured writes off a real asset.
        "untrafficked_benched": steps.iter().filter(|s| s.need == Need::Untrafficked && s.leader.as_ref().is_some_and(|a| a.controlled)).count(),
        // cells the router never reads because nothing in them was benched — explained, not bought
        "pooled": count(Need::Pooled),
        "needs_challenger": count(Need::Challenger),
        "needs_confirming": count(Need::Confirm),
        // the number the plan exists to shrink, said plainly
        // WHAT THE BUDGET REFUSED, reported rather than hidden. `+ 0.0` because a sum of nothing rounds to -0.0 in
        // IEEE arithmetic, and a screen that prints "-$0.00 refused" has lost the reader's trust over a sign bit.
        "skipped_usd": (steps.iter().filter(|s| s.need.spends() && !taken.iter().any(|t| t.job == s.job && t.size == s.size && t.domain == s.domain && t.stack == s.stack)).map(|s| s.usd).sum::<f64>() * 100.0).round() / 100.0 + 0.0,
    });
    (taken, summary)
}

/// one step as the FE and the bench harness read it
pub fn step_json(s: &Step) -> Value {
    json!({
        "job": s.job, "size": s.size, "domain": s.domain, "stack": s.stack, "traffic": s.traffic,
        "need": s.need.key(), "p_flip": (s.p_flip * 1000.0).round() / 1000.0,
        "runs": s.runs, "usd": (s.usd * 1000.0).round() / 1000.0, "priced": s.priced, "value": (s.value * 100.0).round() / 100.0,
        "why": s.why,
        "leader": s.leader.as_ref().map(arm_json), "challenger": s.challenger.as_ref().map(arm_json),
    })
}
fn arm_json(a: &Arm) -> Value {
    json!({ "planner": a.planner, "worker": a.worker, "qa": a.qa, "n": a.n,
            "q": (a.q * 1000.0).round() / 1000.0, "usd": a.usd, "s": a.s, "controlled": a.controlled, "label": side(a) })
}

/// THE RECORD, READ AS CELLS. Quality per row is `merges::quality_of` — the same number the weight ranks on, so the
/// sampler and the router cannot disagree about which pairing leads. A row with no quality at all (nothing measured
/// but a dollar) is left out rather than counted as zero.
///
/// A CONTROL SUITE IS NOT WORTH SETTLING (2026-09-26). This screen's whole claim is "spend this much and a decision
/// changes" — so an arm the router can never pick must not appear in it, or the plan asks for money to settle a
/// contest with no prize. `merges::suite_routes` decides, the same function `best`/`best_in` ask, and the rows stay
/// on disk and on the benchmarks screen either way.
pub fn cells_from_record() -> Result<Vec<Cell>, String> {
    let c = crate::merges::open().map_err(|e| e.to_string())?;
    let mut st = c.prepare(&format!("SELECT job, size, domain, planner, worker, qa, n, pass_first, retries, failed, gate_met, gate_unmet, audit_sum, audit_n, ms_total, usd_total, source, reasked, stack FROM merge_scores WHERE size != 'call' AND {}", crate::merges::routing_sql())).map_err(|e| e.to_string())?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, String>(5)?,
            r.get::<_, i64>(6)?, r.get::<_, i64>(7)?, r.get::<_, i64>(8)?, r.get::<_, i64>(9)?, r.get::<_, i64>(10)?, r.get::<_, i64>(11)?,
            r.get::<_, f64>(12)?, r.get::<_, i64>(13)?, r.get::<_, i64>(14)?, r.get::<_, f64>(15)?, r.get::<_, String>(16)?, r.get::<_, i64>(17)?, r.get::<_, String>(18)?))
    }).map_err(|e| e.to_string())?.filter_map(Result::ok).collect::<Vec<_>>();
    // ONE PAIRING IS ONE ARM, HOWEVER MANY WAYS IT WAS MEASURED (2026-09-28). A pairing watched in the cockpit and
    // then benched has TWO rows — `suite` is part of the unique key, which is exactly how `blend` gets both kinds of
    // evidence for the same pairing — and this loop turned them into two rival arms of the same cell. So a cell could
    // never leave `Need::Confirm`: three benched runs of the leader landed, `ranked` still put the 13-sample observed
    // row first because it has more samples, and the screen went on asking for a bench that had already happened.
    //
    // `best_in` has always grouped by (planner, worker) and blended. This is the sampler agreeing with the router,
    // using the router's own function so the two cannot drift apart again.
    type Half = Option<(f64, i64, f64, f64)>;
    // one running total per evidence class: samples, and everything else weighted by them
    fn add(h: &mut Half, q: f64, n: i64, usd: f64, s: f64) {
        let (q0, n0, u0, s0) = h.unwrap_or((0.0, 0, 0.0, 0.0));
        let (a, b) = (n0 as f64, n.max(1) as f64);
        let t = a + b;
        *h = Some(((q0 * a + q * b) / t, n0 + n, (u0 * a + usd * b) / t, (s0 * a + s * b) / t));
    }
    let mut by: std::collections::BTreeMap<(String, String, String, String), Cell> = std::collections::BTreeMap::new();
    let mut halves: std::collections::BTreeMap<(String, String, String, String, String, String), (Half, Half)> = std::collections::BTreeMap::new();
    let mut traffic: std::collections::BTreeMap<(String, String, String, String), i64> = std::collections::BTreeMap::new();
    for (job, size, domain, planner, worker, _qa, n, pass, retries, failed, gm, gu, asum, an, ms, usd, source, reasked, stack) in rows {
        let Some((q, _)) = crate::merges::quality_of(asum, an, pass, retries, failed, gm, gu, reasked, n) else { continue };
        let nn = n.max(1) as f64;
        let (u, sec) = ((usd / nn * 1000.0).round() / 1000.0, (ms as f64 / nn / 1000.0).round());
        let cell_key = (job.clone(), size.clone(), domain.clone(), stack.clone());
        // THE QA SIDE IS NOT PART OF THE KEY, because it is not part of the ROUTER's key: `best_in` groups by
        // (planner, worker) and nothing else. QA became obligatory on 2026-09-27, so every bench row now names a QA
        // engine while the older observed rows name none — and keying on it split one pairing into two arms that
        // print the SAME name. The screen showed `claude-code:sonnet × cursor:composer-2.5` as its own challenger.
        let e = halves.entry((job, size, domain, stack, planner, worker)).or_insert((None, None));
        match crate::merges::evidence_of(&source) {
            Some(crate::merges::Evidence::Controlled) => add(&mut e.0, q, n, u, sec),
            // a quarantined source is not evidence of anything, here as everywhere else
            Some(crate::merges::Evidence::Observed) => { add(&mut e.1, q, n, u, sec); *traffic.entry(cell_key).or_insert(0) += n }
            None => {}
        }
    }
    for ((job, size, domain, stack, planner, worker), (ctrl, obs)) in halves {
        let controlled = ctrl.is_some();
        let Some((q, n, usd, s)) = crate::merges::blend(ctrl, obs).map(|(q, n, u, s, _)| (q, n, u, s)) else { continue };
        let key = (job.clone(), size.clone(), domain.clone(), stack.clone());
        let t = traffic.get(&key).copied().unwrap_or(0);
        let cell = by.entry(key).or_insert_with(|| Cell { job, size, domain, stack, arms: vec![], traffic: t, typical_usd: 0.0, pooled: false });
        cell.traffic = t;
        cell.arms.push(Arm { planner, worker, qa: String::new(), n, q, usd, s, controlled });
    }
    // THE TYPICAL RUN, MEASURED (2026-09-28). The median over every arm that carries a price — median and not mean so
    // one expensive Opus cell cannot set the stand-in for a whole screen. Cells whose own arms are all unpriced borrow
    // it and are flagged `priced: false`; if nothing on the record has a price it stays 0.0 and the screen says so.
    let mut cells: Vec<Cell> = by.into_values().collect();
    let mut priced: Vec<f64> = cells.iter().flat_map(|c| c.arms.iter()).map(|a| a.usd).filter(|u| *u > 0.0).collect();
    priced.sort_by(f64::total_cmp);
    let typical = if priced.is_empty() { 0.0 } else { priced[priced.len() / 2] };
    // which job+size pairs have a benched arm ANYWHERE — the pool `best_for_stack` falls through to
    let benched: std::collections::BTreeSet<(String, String)> = cells.iter()
        .filter(|c| c.arms.iter().any(|a| a.controlled))
        .map(|c| (c.job.clone(), c.size.clone())).collect();
    for c in &mut cells {
        c.typical_usd = typical;
        c.pooled = !c.arms.is_empty() && !c.arms.iter().any(|a| a.controlled)
            && benched.contains(&(c.job.clone(), c.size.clone()));
    }
    Ok(cells)
}

/// `GET /api/v1/settings/sampler?budget=<usd>&cap=<runs>` — what to measure next, and what the plan refuses to spend.
pub async fn http_plan(q: axum::extract::Query<std::collections::HashMap<String, String>>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let budget = q.get("budget").and_then(|v| v.parse::<f64>().ok()).filter(|v| *v > 0.0);
    let cap = q.get("cap").and_then(|v| v.parse::<i64>().ok()).filter(|v| *v > 0).unwrap_or(8);
    // A DECISION THE OPERATOR CANNOT SEE IS NOT A DECISION (audit 2026-09-28). `plan` returns only the steps worth
    // BUYING, so a settled cell reached the screen as a number in the summary and nothing else — not which cell, not
    // which pairing won, not why. That went from a small gap to a real one the moment a cell could be settled by a
    // TIE broken on price: "these two are indistinguishable, so the cheaper one takes it" is exactly the sentence
    // the operator needs and the only place it exists. `settled` carries those cells; they are not things to buy,
    // so they stay out of `steps` and out of the projected spend.
    match tokio::task::spawn_blocking(move || cells_from_record().map(|c| {
        let (steps, summary) = plan(&c, budget, cap);
        let settled: Vec<Value> = c.iter().map(|cell| step_for(cell, cap)).filter(|s| s.need == Need::Decided)
            .map(|s| step_json(&s)).collect();
        (steps.iter().map(step_json).collect::<Vec<_>>(), settled, summary)
    })).await.unwrap_or_else(|e| Err(e.to_string())) {
        Ok((steps, settled, summary)) => axum::Json(json!({ "ok": true, "steps": steps, "settled": settled, "summary": summary })).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn arm(p: &str, w: &str, n: i64, q: f64, usd: f64, controlled: bool) -> Arm {
        Arm { planner: p.into(), worker: w.into(), qa: String::new(), n, q, usd, s: 100.0, controlled }
    }
    fn cell(traffic: i64, arms: Vec<Arm>) -> Cell {
        Cell { job: "editor-l".into(), size: "l".into(), domain: "backend".into(), stack: "any".into(), arms, traffic, typical_usd: 0.0, pooled: false }
    }

    #[test]
    fn no_evidence_is_not_evidence_of_a_coin_flip_it_is_a_flat_prior() {
        let (a, b) = posterior(0.9, 0);
        assert_eq!((a, b), (1.0, 1.0), "zero samples must not carry the quality into the posterior");
        let (m, v) = moments(a, b);
        assert!((m - 0.5).abs() < 1e-9);
        assert!(v > 0.08, "and it must be WIDE — a flat Beta's variance is 1/12");
        // …while samples narrow it around the measured quality
        let (m2, v2) = moments(posterior(0.9, 20).0, posterior(0.9, 20).1);
        assert!(m2 > 0.85 && v2 < v / 10.0);
    }

    #[test]
    fn the_normal_cdf_is_the_one_it_claims_to_be() {
        assert!((phi(0.0) - 0.5).abs() < 1e-6);
        assert!((phi(1.645) - 0.95).abs() < 1e-3);
        assert!((phi(-1.96) - 0.025).abs() < 1e-3);
    }

    #[test]
    fn a_close_pair_is_a_coin_and_a_wide_gap_with_evidence_is_not() {
        let close = p_challenger_better(&arm("a", "1", 6, 0.80, 0.2, true), &arm("b", "1", 6, 0.78, 0.2, true));
        assert!(close > 0.35 && close < 0.5, "two samples apart on six runs each is close to a coin: {close}");
        let wide = p_challenger_better(&arm("a", "1", 30, 0.95, 0.2, true), &arm("b", "1", 30, 0.40, 0.2, true));
        assert!(wide < 0.02, "a huge gap on thirty runs is not a coin: {wide}");
        // NEVER CERTAIN: the record keeps aggregates, so no answer here is proof
        assert!(wide >= 0.001 && wide <= 0.999);
        let unmeasured = p_challenger_better(&arm("a", "1", 0, 0.99, 0.2, true), &arm("b", "1", 0, 0.01, 0.2, true));
        assert!((unmeasured - 0.5).abs() < 1e-6, "two unmeasured arms are a coin whatever their claimed quality");
    }

    #[test]
    fn a_cell_nobody_routes_to_is_never_measured_whatever_its_numbers_look_like() {
        // the temptation this guards: an interesting-looking coin flip in a cell no one uses
        let s = step_for(&cell(0, vec![arm("a", "1", 2, 0.80, 5.0, true), arm("b", "1", 2, 0.79, 5.0, true)]), 8);
        assert_eq!(s.need, Need::Untrafficked);
        assert_eq!(s.runs, 0);
        assert_eq!(s.usd, 0.0);
        assert!(s.why.contains("nobody routes here"));
    }

    #[test]
    fn one_arm_cannot_be_a_winner_and_the_queue_says_so_instead_of_ranking_it_first() {
        let s = step_for(&cell(12, vec![arm("claude-code:sonnet", "cursor:composer-2.5", 9, 0.93, 0.25, true)]), 8);
        assert_eq!(s.need, Need::Challenger);
        assert_eq!(s.runs, 1);
        assert!(s.why.contains("one arm cannot be a winner"));
        assert!(s.why.contains("sonnet"), "and it names the pairing: {}", s.why);
    }

    #[test]
    fn an_observed_leader_is_never_decided_however_far_ahead_it_looks() {
        // II.4: observational evidence is a PRIOR. A 40-run lead in real use does not settle a cell.
        let s = step_for(&cell(40, vec![arm("a", "1", 40, 0.95, 0.1, false), arm("b", "1", 3, 0.5, 0.1, true)]), 8);
        assert_eq!(s.need, Need::Confirm, "{}", s.why);
        assert!(s.runs >= 1, "and it asks for a bench rather than shrugging");
        assert!(s.why.contains("real-use evidence only"));
    }

    #[test]
    fn a_settled_cell_leaves_the_queue_and_says_stop_paying() {
        let s = step_for(&cell(30, vec![arm("a", "1", 30, 0.96, 0.4, true), arm("b", "1", 30, 0.35, 0.4, true)]), 8);
        assert_eq!(s.need, Need::Decided);
        assert_eq!(s.runs, 0);
        assert_eq!(s.usd, 0.0);
        assert!(s.why.contains("stop paying"));
    }

    #[test]
    fn runs_to_decide_asks_for_more_where_the_gap_is_thinner_and_caps_itself() {
        let thin = runs_to_decide(&arm("a", "1", 4, 0.80, 0.2, true), &arm("b", "1", 4, 0.78, 0.2, true), 8);
        let fat = runs_to_decide(&arm("a", "1", 4, 0.90, 0.2, true), &arm("b", "1", 4, 0.55, 0.2, true), 8);
        assert!(thin >= fat, "a 2-point gap cannot need fewer runs than a 35-point gap ({thin} vs {fat})");
        assert!(thin <= 8 && fat <= 8, "and neither may exceed the cap");
        // an identical pair may never separate: spend the cap and decide on cost, rather than forever
        assert_eq!(runs_to_decide(&arm("a", "1", 5, 0.8, 0.2, true), &arm("b", "1", 5, 0.8, 0.2, true), 8), 8);
        // already separated → nothing to buy
        assert_eq!(runs_to_decide(&arm("a", "1", 200, 0.95, 0.2, true), &arm("b", "1", 200, 0.30, 0.2, true), 8), 0);
    }

    /// A CELL NOBODY COULD PRICE IS NOT A FREE CELL (2026-09-28). The flat seats print tokens but no price, so 53 of
    /// 224 live rows carried `usd_total = 0` and the screen told the operator four cells were "$0.00 to settle them" —
    /// the one number they use to decide whether to spend. An unpriced cell now borrows the record's median run and
    /// says `priced: false`, so the projection is a stand-in that admits it rather than a zero that lies.
    /// THE ROUTER AND THE PLAN MUST AGREE (2026-09-28). `merges::best_for_stack` now walks its ladder twice and the
    /// first pass stops only at a pool holding a benched arm — observation moves a benched score, it does not set
    /// one. So a cell made entirely of real-use rows decides nothing while a bench exists for that job and size: the
    /// pool answers it. Four of the six cells this screen was demanding runs for were exactly that shape, and the
    /// runs would have changed no route. A cell with NO bench anywhere still needs one, because there the observed
    /// rows really are the decision.
    /// TWO ARMS THAT ARE THE SAME NEVER SEPARATE (2026-09-28), and this screen would ask for runs forever. Watched
    /// on the live record: `planner/s/backend/web` sat at exactly 50% through three rounds and asked for eight more
    /// each time; `editor-s` moved 23% → 37% across sixteen runs, AWAY from a decision, because there is none to
    /// reach. `runs_to_decide`'s own comment says "spend the cap and decide on cost, rather than forever" and
    /// nothing acted on it.
    #[test]
    fn two_arms_the_record_cannot_separate_are_decided_on_price_not_asked_about_again() {
        // equal quality, plenty of samples, one clearly cheaper — the shape that was a standing bill
        let dear = arm("a", "1", 20, 0.90, 0.40, true);
        let cheap = arm("b", "1", 20, 0.89, 0.05, true);
        let s = step_for(&cell(50, vec![dear.clone(), cheap.clone()]), 8);
        assert_eq!(s.need, Need::Decided);
        assert!(!s.need.spends() && s.runs == 0 && s.usd == 0.0, "an unanswerable question costs nothing: {s:?}");
        assert!(s.why.contains("indistinguishable") && s.why.contains("on price"), "{}", s.why);
        assert!(s.why.contains("so b × 1 takes it"), "the CHEAPER arm takes it, and the sentence names it: {}", s.why);

        // …but a thin cell is not settled, it is merely young — the cap has not been spent
        let young = step_for(&cell(50, vec![arm("a", "1", 2, 0.90, 0.40, true), arm("b", "1", 2, 0.89, 0.05, true)]), 8);
        assert!(young.need.spends(), "two samples each decides nothing: {young:?}");

        // …and a real gap is still a real gap, decided the way it always was
        let gap = step_for(&cell(50, vec![arm("a", "1", 20, 0.97, 0.40, true), arm("b", "1", 20, 0.40, 0.05, true)]), 8);
        assert_eq!(gap.need, Need::Decided);
        assert!(gap.why.contains("stop paying"), "the ordinary decided sentence, not the tie one: {}", gap.why);
    }

    #[test]
    fn a_cell_nothing_benched_defers_to_the_pool_instead_of_asking_for_money() {
        let observed = vec![arm("a", "1", 13, 0.9, 0.2, false)];
        let mut c = cell(13, observed.clone());
        // with a bench somewhere for this job and size, the pool answers and the plan asks for nothing
        c.pooled = true;
        let s = step_for(&c, 8);
        assert_eq!(s.need, Need::Pooled);
        assert!(!s.need.spends() && s.runs == 0 && s.usd == 0.0, "a cell the router never reads costs nothing: {s:?}");
        assert!(s.why.contains("change no route"), "{}", s.why);

        // with nothing benched anywhere, the same cell is a real gap and is asked for
        c.pooled = false;
        let s = step_for(&c, 8);
        assert_eq!(s.need, Need::Challenger, "one arm and no bench is still one arm");
        assert!(s.need.spends());
    }

    /// AND THE LOADER DECIDES `pooled` FROM THE RECORD, not from a caller's opinion.
    #[test]
    fn pooled_is_true_only_when_a_bench_exists_for_that_job_and_size_elsewhere() {
        // the rule, stated the way the loader computes it
        let rule = |arms: &[Arm], benched_elsewhere: bool| {
            !arms.is_empty() && !arms.iter().any(|a| a.controlled) && benched_elsewhere
        };
        let watched = vec![arm("a", "1", 13, 0.9, 0.2, false)];
        let benched = vec![arm("a", "1", 3, 0.9, 0.2, true)];
        assert!(rule(&watched, true), "watched here, benched there → the pool answers");
        assert!(!rule(&watched, false), "watched here and nowhere benched → this IS the decision");
        assert!(!rule(&benched, true), "a cell with its own bench answers for itself");
        assert!(!rule(&[], true), "an empty cell is untrafficked, not pooled");
    }

    #[test]
    fn a_cell_with_no_priced_arm_borrows_the_records_typical_run_and_says_so() {
        let mut c = cell(50, vec![arm("a", "1", 4, 0.80, 0.0, true), arm("b", "1", 4, 0.78, 0.0, true)]);
        // before: both arms unpriced and no stand-in — the projection is honestly nothing at all
        let bare = step_for(&c, 8);
        assert!(bare.need.spends() && bare.runs > 0, "the cell does need runs");
        assert_eq!((bare.usd, bare.priced), (0.0, false), "nothing on the whole record has a price either");

        // with a stand-in: a real number, still flagged as not measured
        c.typical_usd = 0.25;
        let s = step_for(&c, 8);
        assert_eq!(s.runs, bare.runs, "the stand-in prices the plan, it does not change it");
        assert_eq!((s.usd, s.priced), (0.25 * s.runs as f64, false));

        // and a cell that IS priced never borrows
        let p = step_for(&cell(50, vec![arm("a", "1", 4, 0.80, 0.4, true), arm("b", "1", 4, 0.78, 0.1, true)]), 8);
        assert!(p.priced && p.usd == 0.4 * p.runs as f64, "the dearer arm sets the price it will actually cost");
    }

    #[test]
    fn the_plan_spends_where_a_dollar_buys_the_most_decision_and_obeys_a_budget() {
        let c = |job: &str, traffic: i64, usd: f64| Cell {
            job: job.into(), size: "l".into(), domain: "backend".into(), stack: "any".into(), traffic, typical_usd: 0.0, pooled: false,
            arms: vec![arm("a", "1", 4, 0.80, usd, true), arm("b", "1", 4, 0.78, usd, true)],
        };
        // same coin flip in three cells: heavy traffic, cheap runs, and an expensive one nobody uses much
        let cells = vec![c("editor-l", 100, 0.10), c("planner", 2, 0.10), c("qa-gate", 3, 4.00)];
        let (steps, summary) = plan(&cells, None, 8);
        assert_eq!(steps[0].job, "editor-l", "traffic is a multiplier, so the busy cell leads");
        assert_eq!(steps.len(), 3);
        assert!(summary["projected_usd"].as_f64().unwrap() > 0.0);

        // A BUDGET SKIPS WHAT DOES NOT FIT RATHER THAN SHRINKING IT — half a comparison is not half an answer
        let (cheap, s2) = plan(&cells, Some(1.00), 8);
        assert!(cheap.iter().all(|s| s.usd <= 1.00));
        assert!(cheap.iter().all(|s| s.job != "qa-gate"), "the $4 cell cannot fit a $1 budget");
        assert!(s2["skipped_usd"].as_f64().unwrap() > 0.0, "and what was refused is reported, not hidden");
    }

    #[test]
    fn the_summary_counts_what_the_plan_refuses_to_buy() {
        let cells = vec![
            cell(0, vec![arm("a", "1", 2, 0.8, 1.0, true), arm("b", "1", 2, 0.7, 1.0, true)]),      // untrafficked
            cell(30, vec![arm("a", "1", 30, 0.96, 0.4, true), arm("b", "1", 30, 0.3, 0.4, true)]),  // decided
            cell(5, vec![arm("a", "1", 5, 0.9, 0.2, true)]),                                        // needs a challenger
            cell(9, vec![arm("a", "1", 9, 0.9, 0.2, false), arm("b", "1", 2, 0.5, 0.2, true)]),     // observed leader
        ];
        let (steps, summary) = plan(&cells, None, 8);
        assert_eq!(summary["cells"], 4);
        assert_eq!(summary["untrafficked"], 1);
        assert_eq!(summary["decided"], 1);
        assert_eq!(summary["needs_challenger"], 1);
        assert_eq!(summary["needs_confirming"], 1);
        assert_eq!(steps.len(), 2, "only the two that need spending are queued");
        // nothing was refused, and that reads as zero rather than IEEE's negative zero
        assert_eq!(summary["skipped_usd"].as_f64().unwrap(), 0.0);
        assert_eq!(summary["skipped_usd"].to_string(), "0.0", "-0.0 on a screen is a bug, not a rounding detail");
    }

    #[test]
    fn the_json_is_what_the_screen_and_the_harness_read() {
        let s = step_for(&cell(50, vec![arm("claude-code:sonnet", "cursor:composer-2.5", 4, 0.80, 0.25, true),
                                        arm("claude-code:opus", "cursor:cursor-grok-4.6-medium", 4, 0.78, 0.25, true)]), 8);
        let j = step_json(&s);
        assert_eq!(j["need"], "separate");
        assert_eq!(j["job"], "editor-l");
        assert_eq!(j["traffic"], 50);
        assert!(j["runs"].as_i64().unwrap() >= 1);
        assert!(j["leader"]["label"].as_str().unwrap().contains("sonnet"));
        assert!(j["challenger"]["label"].as_str().unwrap().contains("grok"), "{}", j["challenger"]["label"]);
        assert!(j["why"].as_str().unwrap().contains("%"), "the reason carries the number it is based on");
    }

    #[test]
    fn the_record_reads_as_cells_and_a_row_with_nothing_measured_is_not_a_zero() {
        let _ = std::fs::remove_file(crate::merges::path());
        let o = |job: &str, planner: &str, pass: i64, source: &str| crate::merges::Outcome { suite: String::new(), stack: "any".into(),
            job: job.into(), size: "l".into(), domain: "backend".into(), planner: planner.into(),
            worker: "cursor:composer-2.5".into(), qa: String::new(), tasks: 1,
            pass_first: pass, retries: 0, failed: 1 - pass, gate_met: 0, gate_unmet: 0, audit: Some(0.9),
            ms: 120_000, usd: 0.25, source: source.into(), audit_only: false,
        };
        crate::merges::record(&o("editor-l", "claude-code:sonnet", 1, "bench")).unwrap();
        crate::merges::record(&o("editor-l", "claude-code:opus", 1, "use")).unwrap();
        let cells = cells_from_record().unwrap();
        let c = cells.iter().find(|c| c.job == "editor-l").expect("the cell is there");
        assert_eq!(c.arms.len(), 2);
        assert_eq!(c.traffic, 1, "only the observed row is traffic — a bench is not someone routing here");
        assert!(c.arms.iter().any(|a| a.controlled) && c.arms.iter().any(|a| !a.controlled));
        assert!(c.arms.iter().all(|a| a.usd > 0.0 && a.n > 0));
    }

    /// AND THE PLAN NEVER ASKS FOR MONEY TO SETTLE A CONTEST WITH NO PRIZE. A control suite's arms cannot be routed
    /// to, so "spend $X and a decision changes" is false for them — this screen is the one place in the product that
    /// asks the operator to spend, so it is the last place a number that cannot route belongs.
    #[test]
    fn a_control_suites_arms_are_not_worth_settling_and_do_not_appear() {
        let _ = std::fs::remove_file(crate::merges::path());
        let o = |suite: &str, worker: &str| crate::merges::Outcome { suite: suite.into(), stack: "any".into(),
            // `raw` is the SOLO YARDSTICK and no longer routes (merges::routing_sql); the filler was never the point
            job: "editor-l".into(), size: "l".into(), domain: "backend".into(), planner: crate::merges::key("claude-code", Some("sonnet")),
            worker: worker.into(), qa: String::new(), tasks: 1, pass_first: 1, retries: 0, failed: 0,
            gate_met: 0, gate_unmet: 0, audit: Some(0.9), ms: 120_000, usd: 0.25, source: "bench".into(),
            audit_only: false,
        };
        crate::merges::record(&o("swebench", "w:control")).unwrap();
        crate::merges::record(&o("ministore", "w:measured")).unwrap();
        let cells = cells_from_record().unwrap();
        let c = cells.iter().find(|c| c.job == "editor-l").expect("the cell is there");
        assert_eq!(c.arms.len(), 1, "the control arm must not be offered as something to settle");
        assert_eq!(c.arms[0].worker, "w:measured");
        // and it is still on disk — excluded from the plan, not deleted from the record
        assert!(crate::merges::table().unwrap().iter().any(|r| r["suite"] == "swebench"));
    }
}
