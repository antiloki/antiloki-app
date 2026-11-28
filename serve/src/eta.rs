//! THE ESTIMATOR (operator 2026-09-22: "how good is time estimation on task completion? a real live time remaining
//! stream — this could also help to decide on sub-agents").
//!
//! Every CLI call this serve makes is already timed (`cli::CliTrace`) and every blueprint stage is timed on its run
//! (`blueprint::Stage`). The estimator turns that history into two numbers:
//!
//!   * `remaining` — seconds left in a run: the stages not yet done, plus the WAVE'S CRITICAL PATH (the slowest
//!     unfinished fill, not their sum — they run together), each from the median of what that kind of call took
//!     before on that engine and tier.
//!   * `single_session_s` — what ONE session would take for the same work. The delegate rule is arithmetic from
//!     there: parallel is worth it when the wave's path is materially shorter than that.
//!
//! Durations are per (kind, engine, tier), scaled by the unit a stage repeats over (modules, files). Medians, not
//! means: one stalled OpenCode call (640 s, five of twelve on 2026-09-22) must not move the estimate — that is a
//! watchdog's job, not an estimator's.
use serde::{Deserialize, Serialize};

/// One remembered duration.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Sample { pub kind: String, pub engine: String, pub tier: String, pub ms: i64, #[serde(default)] pub units: i64, pub t: i64 }

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct History { #[serde(default)] pub samples: Vec<Sample> }

/// The median of a slice. Pure.
pub fn median(mut v: Vec<i64>) -> Option<i64> {
    if v.is_empty() { return None; }
    v.sort_unstable();
    Some(v[v.len() / 2])
}

impl History {
    pub fn path() -> std::path::PathBuf {
        #[cfg(test)]
        { return std::env::temp_dir().join(format!("antiloki-eta-test-{}", std::process::id())).join("eta.json"); }
        #[cfg(not(test))]
        {
            let home = std::env::var("ANTILOKI_HOME").ok().filter(|v| !v.is_empty()).map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from(std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| ".".into())).join(".antiloki"));
            home.join("eta.json")
        }
    }
    pub fn load() -> Self { std::fs::read_to_string(Self::path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default() }
    pub fn save(&self) { let p = Self::path(); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); } let _ = std::fs::write(p, serde_json::to_string(self).unwrap_or_default()); }
    pub fn note(kind: &str, engine: &str, tier: &str, ms: i64, units: i64) {
        if ms <= 0 { return; }
        let mut h = Self::load();
        h.samples.push(Sample { kind: kind.into(), engine: engine.into(), tier: tier.into(), ms, units: units.max(1), t: crate::cli::now_ms() });
        if h.samples.len() > 600 { let k = h.samples.len() - 600; h.samples.drain(0..k); }
        h.save();
    }
    /// The median ms PER UNIT for this kind of call, most specific first: (kind, engine, tier) → (kind, engine) →
    /// (kind). None when this serve has never made such a call. Pure over the history.
    pub fn per_unit(&self, kind: &str, engine: &str, tier: &str) -> Option<i64> {
        let pick = |f: &dyn Fn(&Sample) -> bool| -> Option<i64> {
            let v: Vec<i64> = self.samples.iter().rev().take(200).filter(|s| f(s)).map(|s| s.ms / s.units.max(1)).collect();
            if v.len() < 2 { None } else { median(v) }
        };
        pick(&|s: &Sample| s.kind == kind && s.engine == engine && s.tier == tier)
            .or_else(|| pick(&|s: &Sample| s.kind == kind && s.engine == engine))
            .or_else(|| pick(&|s: &Sample| s.kind == kind))
    }
}

/// What a run still owes, in seconds, and how sure that is.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Eta { pub remaining_s: i64, pub of_s: i64, pub confidence: String, pub basis: String }

/// The estimate for a stage pipeline plus a wave: finished stages are facts, the rest are medians, and the
/// unfinished fills count ONCE (the critical path), not N times. `fills_left` is how many are still running or
/// queued, `concurrency` how many run at a time. Pure — tested.
pub fn estimate(done_ms: i64, stages_left: &[(String, i64)], fills_left: usize, fill_ms: i64, concurrency: usize) -> Eta {
    let stages: i64 = stages_left.iter().map(|(_, ms)| *ms).sum();
    let waves = if concurrency == 0 { fills_left as i64 } else { (fills_left as f64 / concurrency as f64).ceil() as i64 };
    let wave = waves * fill_ms;
    let remaining = stages + wave;
    let known = stages_left.iter().filter(|(_, ms)| *ms > 0).count() + usize::from(fill_ms > 0);
    let confidence = if known == 0 { "none" } else if stages_left.len() + usize::from(fills_left > 0) == known { "measured" } else { "partial" };
    Eta { remaining_s: remaining / 1000, of_s: (done_ms + remaining) / 1000, confidence: confidence.into(),
        basis: format!("{} stage(s) {} s + {} fill wave(s) × {} s", stages_left.len(), stages / 1000, waves, fill_ms / 1000) }
}

/// WOULD ONE SESSION BE FASTER? The wave's critical path (the slowest fill × the number of waves) plus the fixed
/// orchestration, against one session's predicted time. `parallel_worth_it` is the rule the router can use instead
/// of a prompt hint. Pure — tested.
pub fn parallel_worth_it(single_session_s: i64, orchestration_s: i64, fills: usize, fill_s: i64, concurrency: usize) -> (bool, String) {
    let waves = if concurrency == 0 { fills as i64 } else { (fills as f64 / concurrency.max(1) as f64).ceil() as i64 };
    let parallel = orchestration_s + waves * fill_s;
    let worth = parallel + parallel / 5 < single_session_s; // a fifth of margin: an estimate is not a promise
    (worth, format!("one session ≈ {single_session_s} s · parallel ≈ {parallel} s ({orchestration_s} s of orchestration + {waves} × {fill_s} s)"))
}

/// WHAT ONE SESSION REALLY COSTS (measured 2026-09-22 on the XXL ORDERS goal). The naive projection — the median
/// PARALLEL fill times the number of parts — under-counts, because a single session also reads the tree, wires the
/// parts together, writes the tests and the README, all serially, and carries one growing context instead of N
/// fresh ones. On that goal the naive number was 488 s (8 × 61 s) while the measured single sessions took 795 s
/// (raw Opus) and 724 s (one Sonnet worker under a contract) — 1.5× and 1.5×. One goal, two sessions: a thin
/// measurement, so it is a named constant with its evidence rather than a formula pretending to more.
pub const SERIAL_FACTOR: f64 = 1.5;
/// The projection for ONE session doing `parts` parts that a parallel worker does in `per_part_s` each.
pub fn single_session_s(per_part_s: i64, parts: usize) -> i64 { ((per_part_s * parts as i64) as f64 * SERIAL_FACTOR).round() as i64 }

/// A PLAN WITH A CHAIN IS NOT ONE WAVE. `chain` is the depth the tasks' `needs` impose (1 = all independent); the
/// concurrency the wave arithmetic may assume is the smaller of the operator's cap and what the chain allows —
/// `parallel_worth_it` then counts the right number of waves. Pure — tested.
pub fn effective_concurrency(fills: usize, chain: usize, cap: usize) -> usize {
    let per_wave = (fills as f64 / chain.max(1) as f64).ceil() as usize;
    per_wave.clamp(1, cap.max(1))
}

/// THE DELEGATION DECISION, WHOLE (operator 2026-09-22: "for 15+ min tasks complexity we use sub-agents"). A wave
/// is kept only when it beats one session with margin AND the single-session projection clears the operator's
/// floor — 15 minutes by default. A floor of 0 or less is the operator saying "never collapse on my behalf" and
/// turns the whole gate off. Pure — tested, and the one place the rule lives.
pub fn wave_or_one(single_session_s: i64, orchestration_s: i64, fills: usize, fill_s: i64, concurrency: usize, floor_s: i64) -> (bool, String) {
    if floor_s <= 0 { return (true, "the floor is off — the plan's own decision stands".into()); }
    let (worth, why) = parallel_worth_it(single_session_s, orchestration_s, fills, fill_s, concurrency);
    if single_session_s < floor_s {
        return (false, format!("{why} · projected {single_session_s} s is under the {floor_s} s floor for a wave"));
    }
    (worth, why)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_estimate_counts_a_wave_once_not_n_times() {
        let e = estimate(60_000, &[("contract".into(), 90_000), ("tests".into(), 40_000)], 8, 50_000, 8);
        assert_eq!(e.remaining_s, 90 + 40 + 50, "one wave of eight at 50 s, not 400 s");
        assert_eq!(e.of_s, 60 + 180);
        assert_eq!(e.confidence, "measured");
        // sixteen fills at a concurrency of eight are two waves
        assert_eq!(estimate(0, &[], 16, 50_000, 8).remaining_s, 100);
        assert_eq!(estimate(0, &[], 0, 0, 8).confidence, "none");
    }
    #[test]
    fn parallel_is_worth_it_only_when_it_beats_one_session_with_margin() {
        // L on 2026-09-22: one haiku session 75 s; the wave was 100 s of orchestration + 8 × 50 s at 8 wide
        assert_eq!(parallel_worth_it(75, 100, 8, 50, 8).0, false, "the wave loses on small work");
        // a big job: one session 20 min, the same orchestration, eight fills of 3 min
        let (worth, why) = parallel_worth_it(1200, 100, 8, 180, 8);
        assert!(worth, "{why}");
        // 120 + 4×90 at 4 wide = 210; 210 + 42 = 252 < 600 → worth it
        assert!(parallel_worth_it(600, 120, 4, 90, 4).0, "clearly shorter");
        // 120 + 60 = 180; 180 + 36 = 216 < 300 → still worth it, just
        assert!(parallel_worth_it(300, 120, 4, 60, 4).0, "shorter with margin");
        // 200 + 4×20 at 1 wide = 280; 280 + 56 = 336 > 300 → not worth the risk
        assert!(!parallel_worth_it(300, 200, 4, 20, 1).0, "serial fills lose the race");
    }
    #[test]
    fn one_session_is_projected_longer_than_the_sum_of_its_parallel_parts() {
        // the XXL of 2026-09-22: eight parts at 61 s of parallel fill projected 488 s naively; the two sessions
        // actually measured on that goal took 795 s and 724 s, which is what the factor is calibrated to
        assert_eq!(single_session_s(61, 8), 732);
        assert!(single_session_s(61, 8) > 61 * 8, "a single session is never merely the sum of the parts");
        assert_eq!(single_session_s(0, 8), 0, "no history, no projection");
    }
    #[test]
    fn a_serial_chain_costs_its_waves_not_one() {
        // eight independent parts at 8 wide: one wave
        assert_eq!(effective_concurrency(8, 1, 8), 8);
        // the flash-sonnet arm of 2026-09-22: eight parts in a chain five deep → at most two at a time → five waves
        let c = effective_concurrency(8, 5, 8); assert_eq!(c, 2);
        let (_, why) = parallel_worth_it(1200, 150, 8, 120, c); assert!(why.contains("4 × 120 s") || why.contains("5 × 120"), "{why}");
        // the chain decides even when the cap is generous, and never below one
        assert_eq!(effective_concurrency(3, 3, 8), 1);
        assert_eq!(effective_concurrency(8, 1, 2), 2, "the operator's cap still holds");
    }
    #[test]
    fn a_wave_needs_both_the_margin_and_the_fifteen_minute_floor() {
        // the XXL of 2026-09-22: one session ≈ 15 min, eight fills of 90 s at 8 wide — a wave, and the benches agree
        assert!(wave_or_one(900, 150, 8, 90, 8, 900).0);
        // the same shape, one minute short of the floor: one worker, and the reason says which rule stopped it
        let (worth, why) = wave_or_one(840, 150, 8, 90, 8, 900);
        assert!(!worth); assert!(why.contains("under the 900 s floor"), "{why}");
        // over the floor but the wave is not actually faster: still one worker
        assert!(!wave_or_one(1000, 800, 4, 200, 1, 900).0, "serial fills lose the race even over the floor");
        // the floor switched off is the operator's own override, not a silent default
        assert_eq!(wave_or_one(60, 150, 2, 30, 2, 0), (true, "the floor is off — the plan's own decision stands".to_string()));
    }
    #[test]
    fn the_median_ignores_a_stall() {
        let mut h = History::default();
        for ms in [40_000, 45_000, 42_000, 640_000, 41_000] { h.samples.push(Sample { kind: "fill".into(), engine: "opencode".into(), tier: "low".into(), ms, units: 1, t: 0 }); }
        assert_eq!(h.per_unit("fill", "opencode", "low"), Some(42_000), "the 640 s stall does not move the median");
        assert_eq!(h.per_unit("fill", "claude-code", "low"), Some(42_000), "falls back to the kind when the engine is new");
        assert_eq!(h.per_unit("contract", "x", "y"), None);
    }
}
