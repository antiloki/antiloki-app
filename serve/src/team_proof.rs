//! THE PROOF CARD (RFC 0010 Appendix D, B2): does the team beat one agent, by the rules written BEFORE any run. Pure over the pairs a bench run recorded: nothing here calls a model. The pairs live in
//! `.antiloki/team/<team>/proof.json` (the bench script writes it; the Proof tab reads it). The verdict is one of `earns its place`, `earns it on cost`, `earns it on time`, `one agent was enough`, `inconclusive`;
//! it is withheld when the judge is not reliable, and it is `stale` the moment the graph, the fixture set or the judge seat changes. Small n is "inconclusive: too few", never a verdict.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const MIN_PAIRS: usize = 15; pub const MIN_FIXTURES: usize = 3; pub const RESAMPLES: usize = 10_000;
/// A pair is one fixture, one repetition: the team's run and the baseline's run (a raw agent or a pair), judged both ways round.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Pair {
    pub fixture: String, #[serde(default)] pub rep: u32,
    /// team | base | tie, as the judge said it with the order swapped back; `inconsistent` = the two orders disagreed (counted as a tie)
    pub winner: String, #[serde(default)] pub inconsistent: bool,
    #[serde(default)] pub q_team: f64, #[serde(default)] pub q_base: f64,
    #[serde(default)] pub cost_team: f64, #[serde(default)] pub cost_base: f64, #[serde(default)] pub secs_team: f64, #[serde(default)] pub secs_base: f64,
    #[serde(default)] pub team_failed_check: bool, #[serde(default)] pub base_failed_check: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Judge { #[serde(default)] pub seat: String, #[serde(default)] pub swap_inconsistency: f64, #[serde(default)] pub spot_agree: f64, #[serde(default)] pub spot_n: u32 }
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Proof { #[serde(default)] pub baseline: String, #[serde(default)] pub graph_hash: String, #[serde(default)] pub fixtures_hash: String, #[serde(default)] pub judge: Judge, #[serde(default)] pub pairs: Vec<Pair> }

/// What the team's score is for ONE pair: 1 = the team won, 0.5 = a tie, 0 = it lost. A mandatory check that exactly one side failed decides the pair for the other; an inconsistent judgement is a tie. Pure.
pub fn score(p: &Pair) -> f64 {
    match (p.team_failed_check, p.base_failed_check) { (true, false) => return 0.0, (false, true) => return 1.0, _ => {} }
    if p.inconsistent { return 0.5; }
    match p.winner.as_str() { "team" => 1.0, "base" => 0.0, _ => 0.5 }
}
struct Rng(u64); impl Rng { fn next(&mut self) -> u64 { self.0 ^= self.0 << 13; self.0 ^= self.0 >> 7; self.0 ^= self.0 << 17; self.0 } fn below(&mut self, n: usize) -> usize { (self.next() % n.max(1) as u64) as usize } }
fn mean(v: &[f64]) -> f64 { if v.is_empty() { 0.0 } else { v.iter().sum::<f64>() / v.len() as f64 } }
fn median(v: &[f64]) -> f64 { if v.is_empty() { return 0.0 } let mut s = v.to_vec(); s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)); let m = s.len() / 2; if s.len() % 2 == 1 { s[m] } else { (s[m - 1] + s[m]) / 2.0 } }
/// 95 % percentile bootstrap of `stat` over the pairs, resampling INSIDE each fixture (a fixture is never dropped or doubled). Deterministic: the seed is fixed so the same pairs give the same card. Pure.
fn bootstrap(pairs: &[Pair], stat: &dyn Fn(&[&Pair]) -> f64) -> (f64, f64) {
    let mut by: std::collections::BTreeMap<&str, Vec<&Pair>> = Default::default(); for p in pairs { by.entry(p.fixture.as_str()).or_default().push(p); }
    let groups: Vec<Vec<&Pair>> = by.into_values().collect(); let mut rng = Rng(0x9E37_79B9_7F4A_7C15); let mut vals = Vec::with_capacity(RESAMPLES);
    for _ in 0..RESAMPLES { let mut s: Vec<&Pair> = vec![]; for g in &groups { for _ in 0..g.len() { s.push(g[rng.below(g.len())]); } } vals.push(stat(&s)); }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)); (vals[(RESAMPLES as f64 * 0.025) as usize], vals[(RESAMPLES as f64 * 0.975) as usize - 1])
}
/// The card. `current_graph_hash`/`current_fixtures_hash` say what the graph and the fixtures are NOW: a proof made for others is stale. Pure.
pub fn card(proof: &Proof, current_graph_hash: &str, current_fixtures_hash: &str) -> Value {
    let pairs = &proof.pairs; let n = pairs.len(); let mut fx: Vec<&str> = pairs.iter().map(|p| p.fixture.as_str()).collect(); fx.sort(); fx.dedup(); let fixtures = fx.len();
    let stale = (!proof.graph_hash.is_empty() && proof.graph_hash != current_graph_hash) || (!proof.fixtures_hash.is_empty() && proof.fixtures_hash != current_fixtures_hash);
    let base = json!({ "baseline": proof.baseline, "n": n, "fixtures": fixtures, "stale": stale, "judge": proof.judge });
    let mut out = |verdict: &str, words: String, next: &str, extra: Value| { let mut v = base.clone(); v["verdict"] = json!(verdict); v["words"] = json!(words); v["next_step"] = json!(next); if let Value::Object(e) = extra { for (k, x) in e { v[k] = x; } } v };
    if n < MIN_PAIRS || fixtures < MIN_FIXTURES { return out("inconclusive", format!("Inconclusive: too few runs ({n} pairs over {fixtures} fixtures; a verdict needs at least {MIN_PAIRS} pairs over {MIN_FIXTURES} fixtures)."), "run more reps on more fixtures", json!({})); }
    let judge_bad = proof.judge.swap_inconsistency > 0.25 || (proof.judge.spot_n >= 5 && proof.judge.spot_agree < 0.70);
    if judge_bad { return out("withheld", "No verdict: the judge is not reliable (it changes its mind when the order is swapped, or it disagrees with your spot checks). Fix the judge first.".into(), "use a stronger judge seat or add your own spot checks", json!({ "judge_ok": false })); }
    let w_of = |s: &[&Pair]| mean(&s.iter().map(|p| score(p)).collect::<Vec<_>>());
    let dq_of = |s: &[&Pair]| mean(&s.iter().map(|p| p.q_team - p.q_base).collect::<Vec<_>>());
    let w = w_of(&pairs.iter().collect::<Vec<_>>()); let (w_lo, w_hi) = bootstrap(pairs, &w_of);
    let dq = dq_of(&pairs.iter().collect::<Vec<_>>()); let (dq_lo, dq_hi) = bootstrap(pairs, &dq_of);
    let ratio = |a: Vec<f64>, b: Vec<f64>| { let mb = median(&b); if mb <= 0.0 { f64::NAN } else { median(&a) / mb } };
    let rho_c = ratio(pairs.iter().map(|p| p.cost_team).collect(), pairs.iter().map(|p| p.cost_base).collect()); let rho_t = ratio(pairs.iter().map(|p| p.secs_team).collect(), pairs.iter().map(|p| p.secs_base).collect());
    let checks_no_worse = pairs.iter().filter(|p| p.team_failed_check).count() <= pairs.iter().filter(|p| p.base_failed_check).count();
    let (verdict, words, next) = if w_lo > 0.5 && dq >= 0.0 && checks_no_worse { ("earns its place", format!("The team earns its place: it beat {} in {:.0}% of {n} pairs (95% interval {:.0}-{:.0}%), with no loss in quality.", proof.baseline, w * 100.0, w_lo * 100.0, w_hi * 100.0), "keep it; re-run when the graph or the task changes")
        } else if dq_lo >= -0.03 && rho_c.is_finite() && rho_c <= 0.7 { ("earns it on cost", format!("The team is as good as {} (within 3 points) at {:.0}% of its cost.", proof.baseline, rho_c * 100.0), "it earns it on cost: say so, not that it is better")
        } else if dq_lo >= -0.03 && rho_t.is_finite() && rho_t <= 0.7 { ("earns it on time", format!("The team is as good as {} (within 3 points) in {:.0}% of its time.", proof.baseline, rho_t * 100.0), "it earns it on time: say so, not that it is better")
        } else if w_hi < 0.5 || (dq_hi < 0.03 && rho_c > 1.0 && rho_t > 1.0) { ("one agent was enough", format!("One agent was enough: {} did as well or better ({} won {:.0}% of pairs) and the team costs {:.1}x as much and takes {:.1}x as long.", proof.baseline, proof.baseline, (1.0 - w) * 100.0, rho_c, rho_t), "run it as one agent, or find what the team is for")
        } else { ("inconclusive", format!("Inconclusive: the team won {:.0}% of {n} pairs (95% interval {:.0}-{:.0}%); that does not settle it either way.", w * 100.0, w_lo * 100.0, w_hi * 100.0), "more reps, a held-out fixture, or a stronger judge") };
    out(verdict, if stale { format!("{words} (STALE: the graph or the fixtures changed since this ran)") } else { words }, next, json!({ "judge_ok": true, "w": w, "w_lo": w_lo, "w_hi": w_hi, "dq": dq, "dq_lo": dq_lo, "dq_hi": dq_hi, "rho_cost": if rho_c.is_finite() { json!(rho_c) } else { Value::Null }, "rho_time": if rho_t.is_finite() { json!(rho_t) } else { Value::Null } }))
}
/// A short stable hash of a graph's nodes and wires (not its layout, budget or branch): the proof belongs to THIS team. Pure.
pub fn graph_hash(g: &crate::team_graph::Graph) -> String {
    use sha2::{Digest, Sha256}; let v = g.to_json(); let s = serde_json::to_string(&json!({ "nodes": v["nodes"], "edges": v["edges"] })).unwrap_or_default().replace("\"pos\"", "\"_p\"");
    hex::encode(&Sha256::digest(s.as_bytes())[..8])
}
pub fn proof_path(root: &str, team: &str) -> std::path::PathBuf { std::path::Path::new(root).join(".antiloki").join("team").join("_proof").join(format!("{}.json", crate::team_graph::slug(team))) }

#[cfg(test)]
mod tests {
    use super::*;
    fn pairs(n_fx: usize, reps: usize, f: impl Fn(usize, usize) -> Pair) -> Vec<Pair> { (0..n_fx).flat_map(|i| (0..reps).map(move |r| (i, r))).map(|(i, r)| { let mut p = f(i, r); p.fixture = format!("fx{i}"); p.rep = r as u32; p }).collect() }
    fn proof(pairs: Vec<Pair>) -> Proof { Proof { baseline: "a raw agent".into(), graph_hash: "g1".into(), fixtures_hash: "f1".into(), judge: Judge { seat: "claude-code:opus".into(), swap_inconsistency: 0.05, spot_agree: 0.9, spot_n: 10 }, pairs } }
    #[test]
    fn a_pair_is_scored_by_the_mandatory_check_first_then_the_judge_and_a_swap_disagreement_is_a_tie() {
        let p = |w: &str, inc: bool, tf: bool, bf: bool| Pair { winner: w.into(), inconsistent: inc, team_failed_check: tf, base_failed_check: bf, ..Default::default() };
        assert_eq!(score(&p("team", false, false, false)), 1.0); assert_eq!(score(&p("base", false, false, false)), 0.0); assert_eq!(score(&p("tie", false, false, false)), 0.5); assert_eq!(score(&p("team", true, false, false)), 0.5, "order-inconsistent = tie");
        assert_eq!(score(&p("team", false, true, false)), 0.0, "only the team failed a mandatory check: the baseline wins whatever the judge said"); assert_eq!(score(&p("base", false, false, true)), 1.0); assert_eq!(score(&p("base", false, true, true)), 0.0, "both failed: the judge decides");
    }
    #[test]
    fn too_few_runs_is_inconclusive_never_a_verdict() {
        let c = card(&proof(pairs(2, 5, |_, _| Pair { winner: "team".into(), ..Default::default() })), "g1", "f1"); assert_eq!(c["verdict"], json!("inconclusive")); assert!(c["words"].as_str().unwrap().contains("too few"), "{c}");
        let c = card(&proof(pairs(3, 4, |_, _| Pair { winner: "team".into(), ..Default::default() })), "g1", "f1"); assert_eq!(c["verdict"], json!("inconclusive"), "12 pairs < 15: {c}");
        let c = card(&Proof::default(), "g", "f"); assert_eq!(c["verdict"], json!("inconclusive")); assert_eq!(c["n"], json!(0));
    }
    #[test]
    fn the_verdicts_follow_the_rules_written_before_the_run() {
        // the team wins nearly everything and is not worse: earns its place
        let win = card(&proof(pairs(3, 6, |i, r| Pair { winner: if (i + r) % 9 == 0 { "tie" } else { "team" }.into(), q_team: 0.9, q_base: 0.7, cost_team: 0.02, cost_base: 0.02, secs_team: 50.0, secs_base: 40.0, ..Default::default() })), "g1", "f1");
        assert_eq!(win["verdict"], json!("earns its place"), "{win}"); assert!(win["w_lo"].as_f64().unwrap() > 0.5 && win["dq"].as_f64().unwrap() > 0.0);
        // as good (all ties), at a third of the cost: earns it on cost, not "better"
        let cost = card(&proof(pairs(3, 6, |_, _| Pair { winner: "tie".into(), q_team: 0.8, q_base: 0.8, cost_team: 0.01, cost_base: 0.03, secs_team: 60.0, secs_base: 30.0, ..Default::default() })), "g1", "f1"); assert_eq!(cost["verdict"], json!("earns it on cost"), "{cost}");
        let time = card(&proof(pairs(3, 6, |_, _| Pair { winner: "tie".into(), q_team: 0.8, q_base: 0.8, cost_team: 0.05, cost_base: 0.03, secs_team: 10.0, secs_base: 30.0, ..Default::default() })), "g1", "f1"); assert_eq!(time["verdict"], json!("earns it on time"), "{time}");
        // the baseline wins and the team is dearer and slower: one agent was enough
        let one = card(&proof(pairs(3, 6, |_, _| Pair { winner: "base".into(), q_team: 0.6, q_base: 0.9, cost_team: 0.1, cost_base: 0.03, secs_team: 90.0, secs_base: 30.0, ..Default::default() })), "g1", "f1"); assert_eq!(one["verdict"], json!("one agent was enough"), "{one}"); assert!(one["words"].as_str().unwrap().contains("3.3x"), "{one}");
        // a coin flip between them, equal cost: inconclusive, with a next step
        let flip = card(&proof(pairs(3, 6, |i, r| Pair { winner: if (i + r) % 2 == 0 { "team" } else { "base" }.into(), q_team: 0.8, q_base: 0.8, cost_team: 0.03, cost_base: 0.03, secs_team: 30.0, secs_base: 30.0, ..Default::default() })), "g1", "f1"); assert_eq!(flip["verdict"], json!("inconclusive"), "{flip}"); assert!(flip["next_step"].as_str().unwrap().contains("more reps"));
        // the same pairs always give the same card (the bootstrap seed is fixed)
        assert_eq!(flip, card(&proof(pairs(3, 6, |i, r| Pair { winner: if (i + r) % 2 == 0 { "team" } else { "base" }.into(), q_team: 0.8, q_base: 0.8, cost_team: 0.03, cost_base: 0.03, secs_team: 30.0, secs_base: 30.0, ..Default::default() })), "g1", "f1"));
    }
    #[test]
    fn an_unreliable_judge_withholds_the_verdict_and_a_changed_graph_makes_it_stale() {
        let mk = |judge: Judge| { let mut p = proof(pairs(3, 6, |_, _| Pair { winner: "team".into(), q_team: 0.9, q_base: 0.5, ..Default::default() })); p.judge = judge; p };
        let bad = card(&mk(Judge { seat: "x".into(), swap_inconsistency: 0.30, spot_agree: 0.9, spot_n: 10 }), "g1", "f1"); assert_eq!(bad["verdict"], json!("withheld"), "{bad}");
        let bad2 = card(&mk(Judge { seat: "x".into(), swap_inconsistency: 0.1, spot_agree: 0.6, spot_n: 5 }), "g1", "f1"); assert_eq!(bad2["verdict"], json!("withheld"), "disagrees with the person's spot checks");
        let few = card(&mk(Judge { seat: "x".into(), swap_inconsistency: 0.1, spot_agree: 0.6, spot_n: 4 }), "g1", "f1"); assert_ne!(few["verdict"], json!("withheld"), "under 5 spot checks the agreement is not held against the judge");
        let stale = card(&mk(Judge::default()), "g2", "f1"); assert_eq!(stale["stale"], json!(true)); assert!(stale["words"].as_str().unwrap().contains("STALE"), "{stale}"); assert_eq!(card(&mk(Judge::default()), "g1", "f1")["stale"], json!(false));
        assert_eq!(card(&mk(Judge::default()), "g1", "f2")["stale"], json!(true), "a changed fixture set is stale too");
    }
    #[test]
    fn a_graph_hash_ignores_the_layout_and_follows_the_nodes() {
        let mut a = crate::team_graph::template("blank", "T", 2.0).unwrap(); let h = graph_hash(&a); assert_eq!(h, graph_hash(&a.clone())); a.budget_usd = 9.0; assert_eq!(graph_hash(&a), h, "the budget is not the team");
        let mut b = a.clone(); b.nodes.insert("x".into(), crate::team_graph::Node::new(crate::team_graph::Kind::Deliver)); assert_ne!(graph_hash(&b), h);
    }
}
