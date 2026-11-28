//! CAMPAIGNS: SEVERAL RUNS TOWARD ONE RESULT, WITHIN THE MACHINE AND THE MONEY (operator 2026-11-13: "you can do more than one run in parallel, A/B tests too; the most important thing is not to go beyond the budget for each run, for the final result
//! itself, and for each agent"). A campaign is a team, an ask, and ARMS: the same team with a change to some nodes (a different seat, another brief, another cap), run side by side as far as the machine allows, compared by the audit and the cost,
//! the winner kept. Three budgets stand on each other: an agent's `cap_usd` (the node), a run's `budget_usd` (the graph), and the campaign's `budget_usd` (the RESULT: every arm, every round, together). The scheduler never STARTS an arm that
//! could take the sum over the result's budget (its worst case is known before it runs), and stops every arm the moment the sum reaches it. Nothing here edits the saved team: an arm is a copy with a patch.
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Arm { pub name: String, /** node id -> the fields to change on it (the same patch `antiloki_team_node` takes) */ #[serde(default)] pub patch: Map<String, Value>,
    #[serde(default)] pub state: String, #[serde(default)] pub run: Option<String>, #[serde(default)] pub worst_usd: f64, #[serde(default)] pub spent_usd: f64, #[serde(default)] pub score: Option<f64>, #[serde(default)] pub good: Option<bool>, #[serde(default)] pub note: String }
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Campaign { pub id: String, pub team: String, pub ask: String, pub engine: String, pub budget_usd: f64, pub parallel: usize, pub arms: Vec<Arm>, pub status: String, pub started_ms: i64, #[serde(default)] pub ended_ms: i64,
    #[serde(default)] pub spent_usd: f64, #[serde(default)] pub peak_parallel: usize, #[serde(default)] pub winner: Option<String>, #[serde(default)] pub capacity: Value, #[serde(default)] pub reading: String }

/// How many runs the machine can carry at once: half the threads, and RAM for each (a run holds a CLI process per working node, about 600 MB), always at least one, never more than 8. Local models are one at a time elsewhere (the slot). Pure.
pub fn slots(cpu_threads: usize, ram_available_mb: Option<u64>) -> usize {
    let by_cpu = (cpu_threads / 2).max(1); let by_ram = ram_available_mb.map_or(by_cpu, |r| (r.saturating_sub(1500) / 600) as usize); by_cpu.min(by_ram.max(1)).clamp(1, 8)
}
/// What the scheduler does next: which waiting arms start now, which are given up (and why). `waiting`: (index, worst case of that arm); `running_left`: what the running arms could still spend (worst case minus spent). Pure.
pub fn next_starts(waiting: &[(usize, f64)], running: usize, limit: usize, spent_total: f64, running_left: f64, budget: f64) -> (Vec<usize>, Vec<(usize, String)>) {
    let (mut start, mut skip) = (vec![], vec![]); let mut committed = running_left; let mut n = running;
    for (i, worst) in waiting {
        let alone = spent_total + worst > budget + 1e-9;
        if n >= limit { break; }   // the machine is full: it waits
        if spent_total + committed + worst <= budget + 1e-9 { start.push(*i); committed += worst; n += 1; }
        else if n == 0 && alone { skip.push((*i, format!("its worst case ${worst:.2} would take the campaign over its ${budget:.2} budget (${spent_total:.2} spent)"))); }
        // else: running arms could still need their share of the budget: it waits to see
    }
    (start, skip)
}
/// The arm to keep: a good result before a bad one, then the higher score, then the cheaper, then the first. `None` when no arm finished. Pure.
pub fn pick_winner(arms: &[Arm]) -> Option<String> {
    arms.iter().filter(|a| a.state == "done" && a.score.is_some()).max_by(|a, b| a.good.unwrap_or(false).cmp(&b.good.unwrap_or(false)).then(a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal)).then(b.spent_usd.partial_cmp(&a.spent_usd).unwrap_or(std::cmp::Ordering::Equal)).then(std::cmp::Ordering::Greater)).map(|a| a.name.clone())
}
/// What is wrong with a campaign request, in words. Pure.
pub fn check(arms: &[Value], budget: f64, parallel: usize) -> Vec<String> {
    let mut out = vec![]; if arms.is_empty() || arms.len() > 8 { out.push("between 1 and 8 arms".into()); } if budget <= 0.0 { out.push("the campaign needs a budget above zero (the result's own, over all its runs)".into()); } if parallel == 0 || parallel > 8 { out.push("parallel is 1 to 8".into()); }
    let mut seen: Vec<&str> = vec![]; for (i, a) in arms.iter().enumerate() { let n = a["name"].as_str().unwrap_or(""); if n.is_empty() || !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { out.push(format!("arm {}: a plain name", i + 1)); } if seen.contains(&n) { out.push(format!("two arms are called {n}")); } seen.push(n); if !a["patch"].is_object() && !a["patch"].is_null() { out.push(format!("arm {n}: `patch` is {{node: {{fields}}}}")); } }
    out
}
pub fn summary(c: &Campaign) -> String {
    let w = c.winner.clone().unwrap_or_else(|| "none".into()); let parts: Vec<String> = c.arms.iter().map(|a| format!("{} {} ${:.3}{}", a.name, a.score.map_or(a.state.clone(), |s| format!("score {s:.2}{}", if a.good == Some(true) { " good" } else { "" })), a.spent_usd, if a.note.is_empty() { String::new() } else { format!(" ({})", a.note) })).collect();
    format!("{}: winner {w}; ${:.3} of ${:.2}; at most {} at once; {}", c.status, c.spent_usd, c.budget_usd, c.peak_parallel, parts.join(" | "))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn arm(n: &str, state: &str, score: Option<f64>, good: bool, usd: f64) -> Arm { Arm { name: n.into(), state: state.into(), score, good: Some(good), spent_usd: usd, ..Default::default() } }
    #[test]
    fn the_machine_carries_as_many_runs_as_its_threads_and_memory_allow_and_never_none() {
        assert_eq!(slots(8, Some(5500)), 4); assert_eq!(slots(8, Some(3000)), 2); assert_eq!(slots(8, Some(1000)), 1); assert_eq!(slots(2, Some(20000)), 1); assert_eq!(slots(64, Some(64000)), 8); assert_eq!(slots(8, None), 4); assert_eq!(slots(0, Some(0)), 1);
    }
    #[test]
    fn an_arm_starts_only_if_the_sum_of_what_could_still_be_spent_stays_inside_the_result_budget() {
        let w = [(0, 1.0), (1, 1.0), (2, 1.0)];
        let (s, k) = next_starts(&w, 0, 4, 0.0, 0.0, 2.5); assert_eq!(s, vec![0, 1], "two fit, the third would take it over"); assert!(k.is_empty(), "it waits: the running ones may not spend their worst case");
        let (s, k) = next_starts(&w, 1, 2, 0.2, 0.8, 2.5); assert_eq!(s, vec![0], "the machine holds two: one running, one more");
        let (s, k) = next_starts(&w, 0, 4, 2.0, 0.0, 2.5); assert!(s.is_empty() && k.len() == 3 && k[0].1.contains("over its $2.50 budget"), "nothing is running and nothing fits: given up, with the reason: {k:?}");
        let (s, k) = next_starts(&w, 0, 1, 0.0, 0.0, 5.0); assert_eq!(s, vec![0]); assert!(k.is_empty());
        assert_eq!(next_starts(&[], 0, 4, 0.0, 0.0, 1.0), (vec![], vec![]));
    }
    #[test]
    fn the_winner_is_a_good_result_then_the_higher_score_then_the_cheaper_and_nobody_when_nothing_finished() {
        let a = vec![arm("a", "done", Some(0.9), false, 0.1), arm("b", "done", Some(0.8), true, 0.3), arm("c", "done", Some(0.8), true, 0.2)]; assert_eq!(pick_winner(&a).as_deref(), Some("c"), "good beats a higher bad score; the cheaper of equals");
        assert_eq!(pick_winner(&[arm("a", "skipped", None, false, 0.0), arm("b", "running", None, false, 0.0)]), None); assert_eq!(pick_winner(&[arm("x", "done", Some(1.0), true, 0.0), arm("y", "done", Some(1.0), true, 0.0)]).as_deref(), Some("x"), "a tie keeps the first");
    }
    #[test]
    fn a_bad_request_is_said_in_words() {
        let ok = vec![json!({ "name": "a" }), json!({ "name": "b", "patch": { "writer": { "tier": "high" } } })]; assert!(check(&ok, 2.0, 2).is_empty());
        let bad = check(&[json!({ "name": "a" }), json!({ "name": "a" }), json!({ "name": "x y" }), json!({ "name": "p", "patch": 3 })], 0.0, 9).join(" | "); for n in ["two arms are called a", "a plain name", "`patch` is", "budget above zero", "parallel is 1 to 8"] { assert!(bad.contains(n), "{n} in {bad}"); } assert!(!check(&[], 1.0, 1).is_empty());
        let c = Campaign { status: "done".into(), winner: Some("b".into()), spent_usd: 0.4, budget_usd: 2.0, peak_parallel: 2, arms: vec![arm("a", "done", Some(0.5), false, 0.1), arm("b", "done", Some(1.0), true, 0.3)], ..Default::default() }; let s = summary(&c); assert!(s.contains("winner b") && s.contains("$0.400 of $2.00") && s.contains("at most 2 at once"), "{s}");
    }
}
