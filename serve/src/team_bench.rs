//! THE BENCHMARK (RFC 0011 §7): mostly the ledger plus statistics, with two nodes that START something: a `trial` (a labelled set WITH TRAPS through one seat)
//! and a `compare` (the same inputs to N seats, every arm recorded). Both write rows with `bench = 1`. Defaults here (MIN_N, the margin) are defaults to tune from
//! our own runs, not measured figures. A comparison that can cause a change states its rule before it runs (the margin, the metric, the minimum n).
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Under this many calls a seat gets numbers but no verdict.
pub const MIN_N: u32 = 5;
/// The pre-registered rule of a compare: an arm wins only if its pass rate leads every other arm by at least this much, and every arm has at least MIN_N cases.
pub const COMPARE_MARGIN: f64 = 0.10;

/// The Wilson score interval for `ok` of `n` (z = 1.96): honest at small n and at the edges, where the plain proportion is not.
pub fn wilson(ok: u32, n: u32) -> (f64, f64) {
    if n == 0 { return (0.0, 1.0); }
    let (n, p, z) = (n as f64, ok as f64 / n as f64, 1.96_f64);
    let d = 1.0 + z * z / n;
    let c = p + z * z / (2.0 * n);
    let m = z * ((p * (1.0 - p) + z * z / (4.0 * n)) / n).sqrt();
    (((c - m) / d).max(0.0), ((c + m) / d).min(1.0))
}

/// One call, as the statistics see it.
#[derive(Clone, Debug, PartialEq)]
pub struct CallStat { pub ok: bool, pub wall_ms: i64, pub cost_usd: f64, pub retried: bool, pub climbed: bool }

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Stats {
    pub n: u32, pub ok: u32, pub ok_rate: f64, pub ok_lo: f64, pub ok_hi: f64,
    pub retry_rate: f64, pub climb_rate: f64, pub median_ms: i64, pub p95_ms: i64, pub cost_per_ok: Option<f64>,
    /// `too few runs`, `reliable`, `unreliable` or `unclear`: said only from MIN_N calls on
    pub verdict: String,
}

fn percentile(sorted: &[i64], q: f64) -> i64 { if sorted.is_empty() { 0 } else { sorted[(((sorted.len() - 1) as f64) * q).round() as usize] } }

pub fn stats_of(calls: &[CallStat]) -> Stats {
    let n = calls.len() as u32; let ok = calls.iter().filter(|c| c.ok).count() as u32;
    let (lo, hi) = wilson(ok, n);
    let mut ms: Vec<i64> = calls.iter().map(|c| c.wall_ms).collect(); ms.sort_unstable();
    let rate = |f: &dyn Fn(&CallStat) -> bool| if n == 0 { 0.0 } else { calls.iter().filter(|c| f(c)).count() as f64 / n as f64 };
    let cost: f64 = calls.iter().map(|c| c.cost_usd).sum();
    let verdict = if n < MIN_N { format!("too few runs (n={n}, a verdict needs {MIN_N})") } else if lo >= 0.8 { "reliable".into() } else if hi < 0.5 { "unreliable".into() } else { "unclear".into() };
    Stats { n, ok, ok_rate: if n == 0 { 0.0 } else { ok as f64 / n as f64 }, ok_lo: lo, ok_hi: hi, retry_rate: rate(&|c| c.retried), climb_rate: rate(&|c| c.climbed),
        median_ms: percentile(&ms, 0.5), p95_ms: percentile(&ms, 0.95), cost_per_ok: if ok == 0 { None } else { Some(cost / ok as f64) }, verdict }
}

/// How each seat has done on one node, from the ledger: seat -> statistics. A call is ok when it answered and no guard caught it.
pub fn node_stats(root: &str, team: &str, node: &str) -> BTreeMap<String, Stats> {
    let rows: Vec<(String, CallStat)> = crate::ledger::with_db(root, team, |c| {
        let mut st = c.prepare("SELECT seat, outcome, COALESCE(verified, 1), COALESCE(wall_ms, 0), COALESCE(cost_usd, 0), COALESCE(try, 1), climbed_from FROM calls WHERE node = ?1 AND seat IS NOT NULL ORDER BY id").map_err(|e| e.to_string())?;
        let it = st.query_map([node], |r| { let outcome: String = r.get(1)?; let verified: i64 = r.get(2)?; let tryn: i64 = r.get(5)?; let climbed: Option<String> = r.get(6)?;
            Ok((r.get::<_, String>(0)?, CallStat { ok: outcome == "ok" && verified != 0, wall_ms: r.get(3)?, cost_usd: r.get(4)?, retried: tryn > 1, climbed: climbed.is_some() })) }).map_err(|e| e.to_string())?;
        it.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
    }).unwrap_or_default();
    let mut by: BTreeMap<String, Vec<CallStat>> = BTreeMap::new();
    for (seat, c) in rows { by.entry(seat).or_default().push(c); }
    by.into_iter().map(|(k, v)| (k, stats_of(&v))).collect()
}

// ── trial and compare ─────────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum Expect { /** the answer's fields equal these */ Fields(Map<String, Value>), /** the answer must NOT be this (a failure a guard caught before) */ Not(Map<String, Value>) }

#[derive(Clone, Debug, PartialEq)]
pub struct Case { pub input: String, pub expect: Expect, /** a case built to catch a seat that is wrong in a known way */ pub trap: bool }

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CaseResult { pub input: String, pub passed: bool, pub trap: bool, pub answer: Option<Map<String, Value>>, pub error: Option<String> }

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TrialReport { pub arm: String, pub n: u32, pub passed: u32, pub pass_rate: f64, pub traps: u32, pub trap_failures: u32, pub results: Vec<CaseResult> }

/// A set no seat can fail measures nothing: it must hold at least one trap, and a case must say what it expects.
pub fn check_set(cases: &[Case]) -> Vec<String> {
    let mut out = vec![];
    if cases.is_empty() { out.push("a trial needs cases".into()); }
    if !cases.iter().any(|c| c.trap) { out.push("a trial needs traps: a set no seat can fail measures nothing".into()); }
    for (i, c) in cases.iter().enumerate() { if c.input.trim().is_empty() { out.push(format!("case {}: the input is empty", i + 1)); } if matches!(&c.expect, Expect::Fields(m) | Expect::Not(m) if m.is_empty()) { out.push(format!("case {}: it says nothing about the answer", i + 1)); } }
    out
}

fn same_field(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) { (Some(x), Some(y)) => (x - y).abs() < 1e-9, _ => match (a.as_str(), b.as_str()) { (Some(x), Some(y)) => x.trim().eq_ignore_ascii_case(y.trim()), _ => a == b } }
}
fn matches_all(want: &Map<String, Value>, got: &Map<String, Value>) -> bool { want.iter().all(|(k, v)| got.get(k).map_or(false, |g| same_field(v, g))) }

pub fn judge(case: &Case, answer: &Result<Map<String, Value>, String>) -> CaseResult {
    match answer {
        Ok(a) => { let passed = match &case.expect { Expect::Fields(w) => matches_all(w, a), Expect::Not(w) => !matches_all(w, a) }; CaseResult { input: case.input.clone(), passed, trap: case.trap, answer: Some(a.clone()), error: None } }
        Err(e) => CaseResult { input: case.input.clone(), passed: false, trap: case.trap, answer: None, error: Some(e.clone()) },
    }
}

pub fn report(arm: &str, results: Vec<CaseResult>) -> TrialReport {
    let n = results.len() as u32; let passed = results.iter().filter(|r| r.passed).count() as u32;
    let traps = results.iter().filter(|r| r.trap).count() as u32; let trap_failures = results.iter().filter(|r| r.trap && !r.passed).count() as u32;
    TrialReport { arm: arm.to_string(), n, passed, pass_rate: if n == 0 { 0.0 } else { passed as f64 / n as f64 }, traps, trap_failures, results }
}

/// Run one arm over a set with the given runner (a model call, or a scripted one). The runner writes its own ledger rows (with `bench = 1`).
pub fn run_trial(arm: &str, cases: &[Case], run: &mut dyn FnMut(&Case) -> Result<Map<String, Value>, String>) -> Result<TrialReport, Vec<String>> {
    let bad = check_set(cases); if !bad.is_empty() { return Err(bad); }
    Ok(report(arm, cases.iter().map(|c| judge(c, &run(c))).collect()))
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CompareReport { pub arms: Vec<TrialReport>, /** the arm that beat every other by the margin, if one did */ pub winner: Option<String>, pub rule: String }

/// The same cases to every arm, every arm recorded, the winner named only by the rule stated here and now.
pub fn run_compare(cases: &[Case], arms: &mut [(String, Box<dyn FnMut(&Case) -> Result<Map<String, Value>, String>>)]) -> Result<CompareReport, Vec<String>> {
    let bad = check_set(cases); if !bad.is_empty() { return Err(bad); }
    if arms.len() < 2 { return Err(vec!["a compare needs at least two arms".into()]); }
    let reports: Vec<TrialReport> = arms.iter_mut().map(|(name, f)| report(name, cases.iter().map(|c| judge(c, &f(c))).collect())).collect();
    Ok(decide(reports))
}

pub fn decide(reports: Vec<TrialReport>) -> CompareReport {
    let rule = format!("an arm wins only if its pass rate leads every other arm by at least {COMPARE_MARGIN} and every arm ran at least {MIN_N} cases (defaults to tune, not measured figures)");
    let enough = reports.iter().all(|r| r.n >= MIN_N);
    let mut winner = None;
    if enough { if let Some(best) = reports.iter().max_by(|a, b| a.pass_rate.partial_cmp(&b.pass_rate).unwrap_or(std::cmp::Ordering::Equal)) {
        if reports.iter().filter(|r| r.arm != best.arm).all(|r| best.pass_rate - r.pass_rate >= COMPARE_MARGIN) { winner = Some(best.arm.clone()); } } }
    CompareReport { arms: reports, winner, rule }
}

/// The real thing: the cases through a cell's spec on one seat, every call a ledger row with `bench = 1`.
pub async fn trial_llm(spec: &crate::team_llm::Spec, seat: &str, cases: &[Case], mut ctx: crate::ledger::Ctx) -> Result<TrialReport, Vec<String>> {
    let bad = check_set(cases); if !bad.is_empty() { return Err(bad); }
    let mut sp = spec.clone(); sp.seat = seat.to_string(); sp.router = None; sp.fallback = vec![]; sp.examples_from = None;
    ctx.bench = true; ctx.caller = "benchmark".into();
    let mut results = vec![];
    for c in cases { let a = crate::team_llm::run(&sp, &c.input, &c.input, Some(ctx.clone())).await.map(|o| o.fields).map_err(|e| e.to_string()); results.push(judge(c, &a)); }
    crate::ledger::flush();
    Ok(report(seat, results))
}

pub fn cases_from_json(v: &Value) -> Result<Vec<Case>, String> {
    let arr = v.as_array().ok_or("`cases` is a list")?;
    arr.iter().enumerate().map(|(i, c)| {
        let input = c["input"].as_str().ok_or_else(|| format!("case {}: `input` is text", i + 1))?.to_string();
        let trap = c["trap"].as_bool().unwrap_or(false);
        let (key, neg) = if c.get("not").is_some() { ("not", true) } else { ("expect", false) };
        let m = c[key].as_object().cloned().ok_or_else(|| format!("case {}: say `expect` (or `not`) as an object", i + 1))?;
        Ok(Case { input, trap, expect: if neg { Expect::Not(m) } else { Expect::Fields(m) } })
    }).collect()
}

pub fn stats_json(m: &BTreeMap<String, Stats>) -> Value { json!(m) }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{self, Ctx};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NTH: AtomicU64 = AtomicU64::new(0);
    fn ws() -> String { let n = NTH.fetch_add(1, Ordering::SeqCst); let d = std::env::temp_dir().join(format!("antiloki-bench-{}-{n}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d.to_string_lossy().into_owned() }
    fn obj(v: Value) -> Map<String, Value> { v.as_object().unwrap().clone() }
    fn cases() -> Vec<Case> { vec![
        Case { input: "price 18".into(), expect: Expect::Fields(obj(json!({ "price": 18 }))), trap: false },
        Case { input: "weight 18 kg, no price".into(), expect: Expect::Fields(obj(json!({ "price": 0 }))), trap: true },
        Case { input: "price 22".into(), expect: Expect::Fields(obj(json!({ "price": 22 }))), trap: false },
        Case { input: "was 40 now 25".into(), expect: Expect::Not(obj(json!({ "price": 40 }))), trap: true },
    ] }
    /// a scripted seat that reads the first number, and writes its own ledger row, as a real call does
    fn seat(root: &str, name: &str, wrong_on_traps: bool) -> impl FnMut(&Case) -> Result<Map<String, Value>, String> {
        let (root, name) = (root.to_string(), name.to_string());
        move |c: &Case| {
            let n: i64 = c.input.split(|ch: char| !ch.is_ascii_digit()).find(|t| !t.is_empty()).and_then(|t| t.parse().ok()).unwrap_or(0);
            let price = if c.trap && wrong_on_traps { n } else if c.input.contains("no price") { 0 } else if c.input.contains("now") { 25 } else { n };
            let ctx = Ctx { root: root.clone(), team: "t".into(), caller: "benchmark".into(), node: Some("cell".into()), bench: true, cfg: Some(name.clone()), ..Default::default() };
            ledger::record_llm(Some(&ctx), &crate::team_llm::Seat::Endpoint { alias: name.clone(), model: None }, &c.input, &Ok((json!({ "price": price }).to_string(), 0.001, 20)));
            Ok(obj(json!({ "price": price })))
        }
    }

    #[test]
    fn trial_runs_a_labelled_set_with_traps_and_records_bench_rows() {
        let root = ws();
        let mut run = seat(&root, "good", false);
        let rep = run_trial("good", &cases(), &mut run).unwrap();
        assert_eq!((rep.n, rep.passed, rep.traps, rep.trap_failures), (4, 4, 2, 0), "{rep:?}");
        let bench: i64 = ledger::with_db(&root, "t", |c| Ok(c.query_row("SELECT COUNT(*) FROM calls WHERE bench = 1 AND caller = 'benchmark'", [], |r| r.get(0)).unwrap())).unwrap();
        assert_eq!(bench, 4, "every case is a ledger row marked as a benchmark call");
        // a seat that falls for the traps is caught by them
        let mut bad = seat(&root, "bad", true);
        let rep = run_trial("bad", &cases(), &mut bad).unwrap();
        assert_eq!(rep.trap_failures, 2, "{rep:?}"); assert!(rep.pass_rate < 1.0);
        // a set no seat can fail is refused; so is an empty one
        let no_traps: Vec<Case> = cases().into_iter().filter(|c| !c.trap).collect();
        assert!(run_trial("x", &no_traps, &mut seat(&root, "x", false)).unwrap_err().iter().any(|e| e.contains("traps")));
        assert!(run_trial("x", &[], &mut seat(&root, "x", false)).is_err());
        // an error from a seat is a failed case, not a crash
        let mut broken = |_: &Case| -> Result<Map<String, Value>, String> { Err("timed out".into()) };
        let rep = run_trial("broken", &cases(), &mut broken).unwrap(); assert_eq!(rep.passed, 0); assert_eq!(rep.results[0].error.as_deref(), Some("timed out"));
        assert_eq!(cases_from_json(&json!([{ "input": "a", "expect": { "x": 1 }, "trap": true }, { "input": "b", "not": { "x": 1 } }])).unwrap().len(), 2);
    }

    #[test]
    fn compare_records_every_arm() {
        let root = ws();
        let big: Vec<Case> = (0..3).flat_map(|_| cases()).collect();   // 12 cases: enough for the rule
        let mut arms: Vec<(String, Box<dyn FnMut(&Case) -> Result<Map<String, Value>, String>>)> = vec![("good".into(), Box::new(seat(&root, "good", false))), ("bad".into(), Box::new(seat(&root, "bad", true)))];
        let rep = run_compare(&big, &mut arms).unwrap();
        assert_eq!(rep.arms.len(), 2); assert!(rep.arms.iter().all(|a| a.n == 12), "every arm saw every case");
        assert_eq!(rep.winner.as_deref(), Some("good"), "{rep:?}"); assert!(rep.rule.contains("0.1"), "the rule is stated with the result");
        let per_arm: Vec<(String, i64)> = ledger::with_db(&root, "t", |c| { let mut st = c.prepare("SELECT cfg, COUNT(*) FROM calls WHERE bench = 1 GROUP BY cfg ORDER BY cfg").unwrap(); Ok(st.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(|r| r.unwrap()).collect()) }).unwrap();
        assert_eq!(per_arm, vec![("bad".to_string(), 12), ("good".to_string(), 12)], "every arm is recorded");
        // two equal arms: no winner; too few cases: no winner
        let mut same: Vec<(String, Box<dyn FnMut(&Case) -> Result<Map<String, Value>, String>>)> = vec![("a".into(), Box::new(seat(&root, "a", false))), ("b".into(), Box::new(seat(&root, "b", false)))];
        assert_eq!(run_compare(&big, &mut same).unwrap().winner, None);
        let mut few: Vec<(String, Box<dyn FnMut(&Case) -> Result<Map<String, Value>, String>>)> = vec![("good".into(), Box::new(seat(&root, "good", false))), ("bad".into(), Box::new(seat(&root, "bad", true)))];
        assert_eq!(run_compare(&cases(), &mut few).unwrap().winner, None, "n=4 is under the minimum");
        assert!(run_compare(&big, &mut [("only".into(), Box::new(seat(&root, "o", false)) as Box<dyn FnMut(&Case) -> Result<Map<String, Value>, String>>)]).is_err());
    }

    #[test]
    fn stats_use_wilson_intervals_and_say_too_few_runs_under_five() {
        let (lo, hi) = wilson(8, 10); assert!((lo - 0.490).abs() < 0.01 && (hi - 0.943).abs() < 0.01, "{lo} {hi}");
        let (lo, hi) = wilson(0, 0); assert_eq!((lo, hi), (0.0, 1.0));
        let (lo, hi) = wilson(10, 10); assert!(lo > 0.70 && hi == 1.0, "ten of ten is not certainty: {lo}");
        let call = |ok: bool, ms: i64| CallStat { ok, wall_ms: ms, cost_usd: 0.01, retried: false, climbed: false };
        let few = stats_of(&[call(true, 10), call(true, 20), call(false, 30), call(true, 40)]);
        assert_eq!(few.n, 4); assert!(few.verdict.contains("too few runs"), "{}", few.verdict);
        let many: Vec<CallStat> = (0..40).map(|i| call(i % 20 != 0, 10 + i)).collect();
        let s = stats_of(&many); assert_eq!(s.n, 40); assert_eq!(s.ok, 38); assert!((s.ok_rate - 0.95).abs() < 1e-9);
        assert_eq!(s.verdict, "reliable", "{s:?}"); assert!(s.ok_lo >= 0.8 && s.ok_hi < 0.995);
        assert_eq!(s.median_ms, 30); assert!(s.p95_ms >= 45); assert!((s.cost_per_ok.unwrap() - 0.4 / 38.0).abs() < 1e-9);
        // 36 of 40 is a good rate but not yet a proven one: the interval says so
        let ninety: Vec<CallStat> = (0..40).map(|i| call(i % 10 != 0, 10)).collect(); assert_eq!(stats_of(&ninety).verdict, "unclear");
        let bad: Vec<CallStat> = (0..30).map(|i| call(i == 0, 5)).collect(); assert_eq!(stats_of(&bad).verdict, "unreliable");
        let mid: Vec<CallStat> = (0..10).map(|i| call(i < 6, 5)).collect(); assert_eq!(stats_of(&mid).verdict, "unclear");
        // and the same figures come out of the ledger, per seat
        let root = ws();
        ledger::with_db(&root, "t", |c| { for i in 0..8 { c.execute("INSERT INTO calls (ts_ms, team, node, caller, source, outcome, bench, prompt_hash, answer_hash, seat, wall_ms, cost_usd, try) VALUES (1,'t','cell','x','endpoint',?1,0,'p','a','endpoint:a',?2,0.01,1)", rusqlite::params![if i < 7 { "ok" } else { "guard_failed" }, 10 * (i + 1)]).unwrap(); } Ok(()) }).unwrap();
        let st = node_stats(&root, "t", "cell"); assert_eq!(st["endpoint:a"].n, 8); assert_eq!(st["endpoint:a"].ok, 7);
        assert!(node_stats(&root, "t", "nothing").is_empty());
    }
}
