//! THE TWO WAYS TO DO AN ASK, PRICED BEFORE YOU PICK (HyperAgent's `perform()` vs `ai()`, improved; queued
//! 2026-09-25 by the operator).
//!
//! HyperAgent gives the caller two entry points with different costs — a cheap accessibility-tree action and an
//! expensive visual one — and lets them choose. The idea worth stealing is that the CHOICE IS VISIBLE. antiloki
//! decides silently today: `classify_ask` picks a job, the jobs table picks a winner, and the operator learns what it
//! cost afterwards.
//!
//! So both ways are offered with their price attached:
//!   · THIN — one call, the job's winner, no planner and no wave. What the record calls a `raw` planner.
//!   · DEEP — planned, verified, gated, judged. What the record calls a merged run.
//!
//! And we can do something HyperAgent cannot: RECOMMEND, from measurement rather than from taste, because the record
//! holds what each way actually cost and scored ON THIS JOB AT THIS SIZE. It is deliberately not narrowed further:
//! filtering by the kind of work and the language as well would leave almost every cell unmeasured, and a
//! recommendation nobody can make is worse than one whose scope is stated. The scope IS stated — job and size — and
//! the reason line carries the sample counts, so a thin recommendation looks thin.
//!
//! THE PRICE IS NEVER INVENTED. A way nobody has measured at this size says so and shows no figure — "we have not
//! done it this way here" is a useful answer and a made-up "≈ $0.40" is not.
use serde_json::{json, Value};

/// one way of doing the ask, as the strip prints it
#[derive(Clone, Debug, PartialEq)]
pub struct Way {
    pub key: &'static str,
    pub label: &'static str,
    /// dollars and seconds per run, `None` when this way has never been measured on work like this
    pub usd: Option<f64>,
    pub s: Option<f64>,
    /// samples behind those numbers
    pub n: i64,
    /// quality 0..1 from the same rows, `None` when unmeasured
    pub quality: Option<f64>,
}
impl Way {
    pub fn json(&self) -> Value {
        json!({ "key": self.key, "label": self.label, "usd": self.usd, "s": self.s, "n": self.n, "quality": self.quality })
    }
}

/// WHICH WAY TO RECOMMEND, and the sentence that says why.
///
/// The rule is the record's, not a preference: when both ways have been measured, recommend THIN unless DEEP is
/// meaningfully better — "meaningfully" being the same 80 %-of-the-best band the weight already uses for its cost band, because
/// a routing rule that disagreed with the ranking rule would make the two screens argue. When only one way has
/// evidence, recommend it and say that is why. When neither does, recommend the cheap one and say plainly that nothing
/// has been measured — the cheap way is the one whose mistake is cheap. Pure — tested.
pub fn recommend(thin: &Way, deep: &Way) -> (&'static str, String) {
    match (thin.quality, deep.quality) {
        (Some(qt), Some(qd)) => {
            // F-03 (24-hour audit, 2026-10-01): this was `qd - qt > 0.03`, an ABSOLUTE three points, while the sentence
            // above (and the ranking) promise the one RELATIVE tolerance. THIN is recommended while it keeps at least
            // 80 % of the planned way's quality, so the recommendation and the ranking cannot argue.
            if qt < crate::roles::Objective::Best.floor(qd) {
                ("deep", format!("planned runs score {:.2} here against {:.2} for a single call — worth the extra {}",
                                 qd, qt, extra(thin, deep)))
            } else {
                ("thin", format!("a single call scores {:.2} here against {:.2} planned — the extra {} buys nothing measurable",
                                 qt, qd, extra(thin, deep)))
            }
        }
        (Some(_), None) => ("thin", format!("only the single call has been measured on this job at this size ({} run(s))", thin.n)),
        (None, Some(_)) => ("deep", format!("only the planned way has been measured on this job at this size ({} run(s))", deep.n)),
        (None, None) => ("thin", "neither way has been measured on this job at this size — the cheap one is the one whose mistake is cheap".into()),
    }
}
fn extra(thin: &Way, deep: &Way) -> String {
    match (thin.usd, deep.usd) {
        (Some(t), Some(d)) if d > t => format!("${:.2}", d - t),
        _ => "cost".into(),
    }
}

/// the measured rows behind one way. `planner_raw` selects the thin way (`planner = 'raw'` is how the record spells
/// "no planner"), its negation the deep one.
fn measure(job: &str, size: &str, planner_raw: bool) -> Way {
    let (key, label) = if planner_raw { ("thin", "one call, the job's winner") } else { ("deep", "planned · verified · gated") };
    let empty = Way { key, label, usd: None, s: None, n: 0, quality: None };
    let Ok(c) = crate::merges::open() else { return empty };
    let sql = format!("SELECT COALESCE(SUM(n),0), COALESCE(SUM(usd_total),0), COALESCE(SUM(ms_total),0), \
                       COALESCE(SUM(audit_sum),0), COALESCE(SUM(audit_n),0), COALESCE(SUM(pass_first),0), \
                       COALESCE(SUM(retries),0), COALESCE(SUM(failed),0), COALESCE(SUM(gate_met),0), COALESCE(SUM(gate_unmet),0) \
                       FROM merge_scores WHERE job = ?1 AND size = ?2 AND planner {} 'raw'", if planner_raw { "=" } else { "!=" });
    let row: Option<(i64, f64, i64, f64, i64, i64, i64, i64, i64, i64)> = c.query_row(&sql, rusqlite::params![job, size], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?))
    }).ok();
    let Some((n, usd, ms, asum, an, pass, retries, failed, gm, gu)) = row else { return empty };
    if n <= 0 { return empty }
    let nn = n as f64;
    Way {
        key, label,
        usd: Some((usd / nn * 1000.0).round() / 1000.0),
        s: Some((ms as f64 / nn / 1000.0).round()),
        n,
        quality: crate::merges::quality_of(asum, an, pass, retries, failed, gm, gu, 0, n).map(|(q, _)| q),
    }
}

/// WHICH CELL AN ASK FALLS IN — the ROUTER'S OWN classification (`cli::classify_ask`), never a second opinion: the
/// comparison is only meaningful for the cell the ask would actually route to, and a screen that guessed `editor-l`
/// while the router picked `lookup` would compare against the wrong record. The size comes from the same rule the
/// record is keyed by (`merges::size_of`), and the domain from the active workspace's work, which the ask alone
/// cannot know — `any` is the pool, and it is the honest default here. Pure. Tested.
pub fn cell_of_ask(ask: &str) -> (String, String) {
    let job = crate::cli::classify_ask(ask);
    let one_file = ask.split(|c: char| c.is_whitespace()).filter(|w| w.contains('/') || w.contains('.')).count() <= 1;
    let size = crate::merges::size_of(1, one_file, ask.chars().count()).to_string();
    (job.key().to_string(), size)
}

/// `GET /api/v1/settings/two-ways?ask=…` — both ways, priced, with the recommendation and its reason.
pub async fn http_ways(q: axum::extract::Query<std::collections::HashMap<String, String>>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let ask = q.get("ask").cloned().unwrap_or_default();
    let out = tokio::task::spawn_blocking(move || {
        let (job, size) = if ask.trim().is_empty() { ("editor-l".to_string(), "l".to_string()) } else { cell_of_ask(&ask) };
        let thin = measure(&job, &size, true);
        let deep = measure(&job, &size, false);
        let (pick, why) = recommend(&thin, &deep);
        json!({ "ok": true, "job": job, "size": size, "thin": thin.json(), "deep": deep.json(), "recommended": pick, "why": why })
    }).await.unwrap_or_else(|e| json!({ "ok": false, "error": e.to_string() }));
    axum::Json(out).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_cell_is_the_routers_own_verdict_for_the_ask_not_a_second_guess() {
        // a question is a reader's job, not an edit — comparing it against the editor rows would compare nothing
        assert_eq!(cell_of_ask("where is the workspace root read?").0, "lookup");
        assert_eq!(cell_of_ask("why does the sampler use a Beta?").0, "synthesis");
        // a one-file, short ask is S; a long one with several files is L — the same rule the record is keyed by
        let (job, size) = cell_of_ask("fix the off-by-one in serve/src/eta.rs");
        assert_eq!((job.as_str(), size.as_str()), ("editor-s", "s"));
        let long = format!("rework {} and src/b.rs and src/c.rs — {}", "src/a.rs", "x".repeat(420));
        assert_eq!(cell_of_ask(&long).1, "l");
    }

    fn way(key: &'static str, usd: f64, q: Option<f64>, n: i64) -> Way {
        Way { key, label: "", usd: Some(usd), s: Some(60.0), n, quality: q }
    }

    #[test]
    fn a_planned_run_is_recommended_only_when_it_is_measurably_better() {
        let (pick, why) = recommend(&way("thin", 0.05, Some(0.60), 6), &way("deep", 0.90, Some(0.95), 6));
        assert_eq!(pick, "deep");
        assert!(why.contains("worth the extra $0.85"), "{why}");
    }

    #[test]
    fn a_planned_run_that_buys_nothing_measurable_is_not_recommended_however_thorough_it_looks() {
        let (pick, why) = recommend(&way("thin", 0.05, Some(0.92), 6), &way("deep", 0.90, Some(0.93), 6));
        assert_eq!(pick, "thin", "one point of quality for eighteen times the price is not an improvement");
        assert!(why.contains("buys nothing measurable"), "{why}");
    }

    #[test]
    fn the_recommendation_uses_the_one_relative_tolerance_not_an_absolute_three_points() {
        // 0.80 against 0.95 is fifteen points apart but the single call keeps 84 % of the planned way: inside the band,
        // so the cheap way is recommended (the old absolute 0.03 said "deep" here).
        let (pick, _) = recommend(&way("thin", 0.05, Some(0.80), 6), &way("deep", 0.90, Some(0.95), 6));
        assert_eq!(pick, "thin", "84 % of the best is inside the 80 % band");
        // the boundary: exactly 80 % of 0.80 is 0.64, kept; a hair under is not
        let (at, _) = recommend(&way("thin", 0.05, Some(0.64), 6), &way("deep", 0.90, Some(0.80), 6));
        assert_eq!(at, "thin", "exactly 80 % is kept");
        let (under, _) = recommend(&way("thin", 0.05, Some(0.63), 6), &way("deep", 0.90, Some(0.80), 6));
        assert_eq!(under, "deep", "79 % is outside the band");
        // and a small absolute gap at a low level is NOT noise: 0.30 against 0.36 is 83 % -> thin; 0.28 against 0.36 is 78 % -> deep
        assert_eq!(recommend(&way("thin", 0.05, Some(0.28), 6), &way("deep", 0.90, Some(0.36), 6)).0, "deep");
    }

    #[test]
    fn one_measured_way_is_recommended_and_says_that_is_the_reason() {
        let unmeasured = Way { key: "deep", label: "", usd: None, s: None, n: 0, quality: None };
        let (pick, why) = recommend(&way("thin", 0.05, Some(0.80), 4), &unmeasured);
        assert_eq!(pick, "thin");
        assert!(why.contains("only the single call has been measured"), "{why}");
    }

    #[test]
    fn with_nothing_measured_the_cheap_way_wins_and_the_reason_is_not_dressed_up() {
        let none_thin = Way { key: "thin", label: "", usd: None, s: None, n: 0, quality: None };
        let none_deep = Way { key: "deep", label: "", usd: None, s: None, n: 0, quality: None };
        let (pick, why) = recommend(&none_thin, &none_deep);
        assert_eq!(pick, "thin");
        assert!(why.contains("neither way has been measured"), "{why}");
        assert!(!why.contains('$'), "and it must not print a price it does not have: {why}");
    }

    #[test]
    fn an_unmeasured_way_carries_no_figures_at_all() {
        let _ = std::fs::remove_file(crate::merges::path());
        let w = measure("editor-l", "xxl", true);
        assert_eq!((w.usd, w.s, w.n, w.quality), (None, None, 0, None));
        assert_eq!(w.json()["usd"], Value::Null, "the screen must be able to tell 'unmeasured' from 'free'");
    }

    #[test]
    fn the_two_ways_read_the_record_apart() {
        let _ = std::fs::remove_file(crate::merges::path());
        let o = |planner: &str, usd: f64, audit: f64| crate::merges::Outcome { suite: String::new(),
            job: "editor-s".into(), size: "s".into(), domain: "any".into(), stack: "any".into(),
            planner: planner.into(), worker: "w:1".into(), qa: String::new(), tasks: 1, pass_first: 1, retries: 0,
            failed: 0, gate_met: 0, gate_unmet: 0, audit: Some(audit), ms: 30_000, usd, source: "bench".into(), audit_only: false };
        crate::merges::record(&o("raw", 0.02, 0.9)).unwrap();
        crate::merges::record(&o("claude-code:sonnet", 0.60, 0.95)).unwrap();
        let thin = measure("editor-s", "s", true);
        let deep = measure("editor-s", "s", false);
        assert_eq!(thin.usd, Some(0.02));
        assert_eq!(deep.usd, Some(0.6));
        assert_eq!((thin.n, deep.n), (1, 1), "a run is counted on exactly one side");
    }
}
