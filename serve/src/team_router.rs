//! THE ROUTER (RFC 0011 §10.2): a seat policy for an `llm` node (`seat: "router:<name>"` plus a `router` part), not a node of its own.
//! A rules table (input size, cost cap, whether the seat is usable and has quota) narrows the candidates; a bandit (Thompson sampling on ok/fail counts per
//! seat for this task class) picks among the cheap ones that are reliable enough; a minimum share of calls goes to a random eligible seat so the benchmark keeps
//! measuring seats the router no longer favours; a safety-floor seat is always the way out. Every choice carries its evidence. It never moves the Leader.
use crate::team_graph::Kind;
use crate::team_llm::{parse_seat, Seat};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// A default to tune from our own runs: this share of the calls goes to a random eligible seat.
pub const DEFAULT_EXPLORE: f64 = 0.10;
/// A seat is "reliable enough" when its sampled ok rate is at least this.
pub const DEFAULT_MIN_OK: f64 = 0.80;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub seat: String,
    /// the longest input (characters) this seat should be given
    #[serde(default, skip_serializing_if = "Option::is_none")] pub max_input_chars: Option<u32>,
    /// what one call is expected to cost, in dollars (0 for a local seat)
    #[serde(default, skip_serializing_if = "Option::is_none")] pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RouterSpec {
    /// cheapest first
    pub candidates: Vec<Candidate>,
    /// the seat that is always reachable and stronger than every candidate
    #[serde(default)] pub floor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub explore: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub min_ok: Option<f64>,
    /// a call is never given to a seat whose expected cost is above this
    #[serde(default, skip_serializing_if = "Option::is_none")] pub cost_cap_usd: Option<f64>,
}

/// A small, seedable generator (xorshift64*): the same seed gives the same choices, so a choice can be replayed from its evidence.
#[derive(Clone, Debug)]
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Rng { Rng(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed }) }
    pub fn next_u64(&mut self) -> u64 { let mut x = self.0; x ^= x >> 12; x ^= x << 25; x ^= x >> 27; self.0 = x; x.wrapping_mul(0x2545_F491_4F6C_DD1D) }
    /// in (0, 1)
    pub fn next_f64(&mut self) -> f64 { (((self.next_u64() >> 11) as f64) + 0.5) / ((1u64 << 53) as f64) }
}

/// Beta(a, b) for whole a, b: the ratio of two Gamma sums of exponentials. Counts are scaled down past 200 so a long history stays cheap.
fn beta_sample(ok: u32, fail: u32, rng: &mut Rng) -> f64 {
    let scale = |n: u32| -> u32 { if n > 200 { 200 } else { n } };
    let (ok, fail) = if ok.max(fail) > 200 { let k = 200.0 / ok.max(fail) as f64; (((ok as f64) * k).round() as u32, ((fail as f64) * k).round() as u32) } else { (ok, fail) };
    let gamma = |k: u32, rng: &mut Rng| -> f64 { (0..k.max(1)).map(|_| -rng.next_f64().ln()).sum() };
    let (x, y) = (gamma(scale(ok) + 1, rng), gamma(scale(fail) + 1, rng));
    x / (x + y)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Choice {
    pub seat: String,
    /// true when this seat was taken for the exploration share, not because the policy favoured it
    pub explored: bool,
    /// true when no candidate qualified and the floor was taken
    pub floor: bool,
    pub why: String,
    pub evidence: Value,
}

/// Choose the seat for one call. `stats` is seat → (ok, fail) for this task class; `usable` says whether a seat is installed, signed in and has quota.
pub fn choose(r: &RouterSpec, stats: &BTreeMap<String, (u32, u32)>, usable: &dyn Fn(&str) -> bool, input_chars: usize, rng: &mut Rng) -> Choice {
    let explore = r.explore.unwrap_or(DEFAULT_EXPLORE).clamp(0.0, 1.0);
    let min_ok = r.min_ok.unwrap_or(DEFAULT_MIN_OK);
    let mut skipped: Vec<Value> = vec![];
    let mut eligible: Vec<&Candidate> = vec![];
    for c in &r.candidates {
        let why_not = if !usable(&c.seat) { Some("not usable now (not installed, not signed in, or out of quota)") }
            else if c.max_input_chars.map_or(false, |m| input_chars > m as usize) { Some("the input is longer than it should be given") }
            else if r.cost_cap_usd.map_or(false, |cap| c.cost_usd.unwrap_or(0.0) > cap) { Some("above the cost cap") }
            else { None };
        match why_not { Some(w) => skipped.push(json!({ "seat": c.seat, "why": w })), None => eligible.push(c) }
    }
    if eligible.is_empty() {
        return Choice { seat: r.floor.clone(), explored: false, floor: true, why: "no candidate qualified: the safety floor".into(), evidence: json!({ "skipped": skipped, "input_chars": input_chars }) };
    }
    let draws: Vec<(String, f64, (u32, u32))> = eligible.iter().map(|c| { let (ok, fail) = stats.get(&c.seat).copied().unwrap_or((0, 0)); (c.seat.clone(), beta_sample(ok, fail, rng), (ok, fail)) }).collect();
    let evidence = |picked: &str, how: &str| json!({ "how": how, "picked": picked, "input_chars": input_chars, "min_ok": min_ok, "explore": explore,
        "draws": draws.iter().map(|(s, p, st)| json!({ "seat": s, "sampled_ok_rate": (p * 1000.0).round() / 1000.0, "ok": st.0, "fail": st.1 })).collect::<Vec<_>>(), "skipped": skipped });
    // the exploration share: a random eligible seat, so a seat the policy stopped favouring is still measured
    if rng.next_f64() < explore {
        let i = (rng.next_u64() % eligible.len() as u64) as usize; let seat = eligible[i].seat.clone();
        return Choice { why: format!("exploring: {seat} taken at random for the benchmark"), evidence: evidence(&seat, "explore"), seat, explored: true, floor: false };
    }
    // cheapest first: the first seat whose sampled ok rate is good enough
    for (c, (_, p, _)) in eligible.iter().zip(draws.iter()) {
        if *p >= min_ok { return Choice { seat: c.seat.clone(), explored: false, floor: false, why: format!("the cheapest seat that meets the rule ({:.0}% sampled ok rate)", p * 100.0), evidence: evidence(&c.seat, "cheapest_reliable") }; }
    }
    // none is reliable enough: the one most likely to be right
    let best = draws.iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)).map(|d| d.0.clone()).unwrap_or_else(|| r.floor.clone());
    Choice { why: "no seat meets the rule yet: the one most likely to be right".into(), evidence: evidence(&best, "best_available"), seat: best, explored: false, floor: false }
}

/// The router only decides the seat of a model call (`llm`). The Leader (the person's own terminal) and the roles are never moved by it.
pub fn router_applies(kind: &Kind) -> bool { matches!(kind, Kind::Llm(_)) }

pub fn is_router_seat(seat: &str) -> bool { seat.trim().starts_with("router:") }

fn rank(s: &Seat) -> u8 { match s { Seat::Local { .. } => 0, Seat::Endpoint { .. } => 1, Seat::Claude { .. } => 2 } }

/// What is wrong with a router, in words. `router_no_floor`: a router with no stronger fallback.
pub fn check(r: &RouterSpec) -> Vec<String> {
    let mut out = vec![];
    if r.candidates.is_empty() { out.push("router: name at least one candidate seat".into()); }
    if r.candidates.len() > 8 { out.push("router: at most eight candidates".into()); }
    let mut top = 0u8;
    for c in &r.candidates {
        match parse_seat(&c.seat) { Ok(s) => top = top.max(rank(&s)), Err(e) => out.push(format!("router: {e}")) }
        if c.cost_usd.map_or(false, |x| x < 0.0) { out.push(format!("router: `{}` has a negative cost", c.seat)); }
    }
    if r.floor.trim().is_empty() { out.push("router_no_floor: a router needs a `floor`, a stronger seat that is always reachable".into()); }
    else {
        match parse_seat(&r.floor) {
            Err(e) => out.push(format!("router_no_floor: {e}")),
            Ok(f) => {
                if r.candidates.iter().any(|c| c.seat == r.floor) { out.push("router_no_floor: the floor is one of the candidates, so there is no stronger fallback".into()); }
                else if rank(&f) < top { out.push("router_no_floor: the floor is weaker than a candidate: there is no stronger fallback".into()); }
            }
        }
    }
    if r.explore.map_or(false, |e| !(0.0..=1.0).contains(&e)) { out.push("router: explore is a share between 0 and 1".into()); }
    if r.min_ok.map_or(false, |e| !(0.0..=1.0).contains(&e)) { out.push("router: min_ok is a share between 0 and 1".into()); }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(seat: &str, cost: f64) -> Candidate { Candidate { seat: seat.into(), max_input_chars: None, cost_usd: Some(cost) } }
    fn policy() -> RouterSpec { RouterSpec { candidates: vec![cand("local:qwen3:4b", 0.0), cand("endpoint:mid", 0.002), cand("claude-code:haiku", 0.02)], floor: "claude-code:sonnet".into(), explore: Some(0.0), min_ok: None, cost_cap_usd: None } }
    fn all(_: &str) -> bool { true }
    fn stats(v: &[(&str, u32, u32)]) -> BTreeMap<String, (u32, u32)> { v.iter().map(|(s, o, f)| (s.to_string(), (*o, *f))).collect() }

    #[test]
    fn router_picks_the_cheapest_seat_that_meets_the_rule() {
        let r = policy();
        // the cheap one has a long good record: it is taken, every time
        let st = stats(&[("local:qwen3:4b", 60, 2), ("endpoint:mid", 60, 2)]);
        for seed in 1..40 { let c = choose(&r, &st, &all, 100, &mut Rng::new(seed)); assert_eq!(c.seat, "local:qwen3:4b", "{c:?}"); assert!(!c.explored && !c.floor); }
        // it started failing: the next cheapest that is reliable takes over
        let st = stats(&[("local:qwen3:4b", 2, 40), ("endpoint:mid", 60, 2)]);
        for seed in 1..40 { assert_eq!(choose(&r, &st, &all, 100, &mut Rng::new(seed)).seat, "endpoint:mid"); }
        // nothing is reliable yet: the most likely one, and the evidence says so
        let st = stats(&[("local:qwen3:4b", 1, 30), ("endpoint:mid", 1, 30), ("claude-code:haiku", 25, 1)]);
        let c = choose(&r, &st, &all, 100, &mut Rng::new(7)); assert_eq!(c.seat, "claude-code:haiku");
        assert!(c.evidence["draws"].as_array().unwrap().len() == 3 && c.evidence["how"].is_string(), "the choice carries its evidence: {c:?}");
    }

    #[test]
    fn router_respects_availability_and_quota() {
        let mut r = policy(); let st = stats(&[("local:qwen3:4b", 60, 0), ("endpoint:mid", 60, 0), ("claude-code:haiku", 60, 0)]);
        // the cheap seat is out of quota: skipped, and the evidence names why
        let c = choose(&r, &st, &|s| s != "local:qwen3:4b", 100, &mut Rng::new(3)); assert_eq!(c.seat, "endpoint:mid");
        assert!(c.evidence["skipped"][0]["why"].as_str().unwrap().contains("quota"), "{c:?}");
        // an input too long for it
        r.candidates[0].max_input_chars = Some(50); assert_eq!(choose(&r, &st, &all, 500, &mut Rng::new(3)).seat, "endpoint:mid");
        assert_eq!(choose(&r, &st, &all, 40, &mut Rng::new(3)).seat, "local:qwen3:4b");
        // a cost cap
        r.candidates[0].max_input_chars = None; r.cost_cap_usd = Some(0.001); assert_eq!(choose(&r, &st, &all, 10, &mut Rng::new(3)).seat, "local:qwen3:4b");
        r.candidates[0].cost_usd = Some(0.5); let c = choose(&r, &st, &all, 10, &mut Rng::new(3)); assert!(c.floor && c.seat == "claude-code:sonnet", "everything above the cap: the floor {c:?}");
        // nothing usable: the floor, never an error
        let c = choose(&policy(), &st, &|_| false, 10, &mut Rng::new(3)); assert!(c.floor && c.seat == "claude-code:sonnet");
    }

    #[test]
    fn bandit_keeps_a_minimum_exploration_share_and_a_seeded_rng_is_reproducible() {
        let mut r = policy(); r.explore = Some(0.2);
        let st = stats(&[("local:qwen3:4b", 200, 0), ("endpoint:mid", 200, 0), ("claude-code:haiku", 200, 0)]);
        let mut rng = Rng::new(42); let (mut explored, mut other) = (0, 0); let n = 4000;
        for _ in 0..n { let c = choose(&r, &st, &all, 100, &mut rng); if c.explored { explored += 1; } if c.seat != "local:qwen3:4b" { other += 1; } }
        let share = explored as f64 / n as f64;
        assert!((0.17..=0.23).contains(&share), "the exploration share is about 0.2, was {share}");
        assert!(other > 0, "a seat the policy no longer favours is still measured");
        // never explores when the share is zero
        r.explore = Some(0.0); let mut rng = Rng::new(42); assert!((0..500).all(|_| !choose(&r, &st, &all, 100, &mut rng).explored));
        // the same seed, the same choices; another seed, other ones
        r.explore = Some(0.5);
        let run = |seed: u64| -> Vec<String> { let mut g = Rng::new(seed); (0..60).map(|_| choose(&r, &st, &all, 100, &mut g).seat).collect() };
        assert_eq!(run(9), run(9)); assert_ne!(run(9), run(10));
        let u = Rng::new(1).next_f64(); assert!(u > 0.0 && u < 1.0);
    }

    #[test]
    fn router_never_moves_the_leader() {
        assert!(!router_applies(&Kind::Leader));
        assert!(!router_applies(&Kind::Role { role: crate::team_graph::RoleKind::Worker, tier: None, model: None, readonly: None, fresh_context: None, parallel: None, owns: vec![], agent: None }));
        assert!(!router_applies(&Kind::Deliver));
        let spec: crate::team_llm::Spec = serde_json::from_value(json!({ "seat": "router:cheap", "prompt": "p", "answer": { "x": "string" } })).unwrap();
        assert!(router_applies(&Kind::Llm(spec)));
        assert!(is_router_seat("router:cheap") && !is_router_seat("endpoint:a"));
    }

    #[test]
    fn a_router_with_no_stronger_fallback_is_a_validator_error() {
        assert!(check(&policy()).is_empty(), "{:?}", check(&policy()));
        let mut r = policy(); r.floor = String::new(); assert!(check(&r).iter().any(|e| e.contains("router_no_floor")));
        let mut r = policy(); r.floor = "claude-code:haiku".into(); assert!(check(&r).iter().any(|e| e.contains("router_no_floor") && e.contains("one of the candidates")));
        let mut r = policy(); r.floor = "local:qwen3:4b-b".into(); assert!(check(&r).iter().any(|e| e.contains("weaker")), "{:?}", check(&r));
        let mut r = policy(); r.floor = "nonsense".into(); assert!(check(&r).iter().any(|e| e.contains("router_no_floor")));
        let mut r = policy(); r.candidates.clear(); assert!(check(&r).iter().any(|e| e.contains("at least one candidate")));
        let mut r = policy(); r.explore = Some(1.5); assert!(check(&r).iter().any(|e| e.contains("explore")));
    }
}
