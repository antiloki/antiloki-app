//! DOES "SURE" MEAN "RIGHT"? (operator 2026-09-25, after reading TypeSafe's JEV: "the router already says how sure it
//! is — count how often the sure ones actually worked, so the word stops being decoration").
//!
//! The planning router answers every ask with `{tier, delegate, why, confidence}` and has done since 2026-09-20, when
//! the operator asked for it: *"didn't say the AI was like 70% sure"*. That grade is printed in one transcript line
//! and compared against what happened NOWHERE. A model grading its own certainty, with nothing checking whether a
//! "high" is right more often than a "low", is a decoration with the shape of a measurement.
//!
//! What a vendor cannot do for us and we can: we hold the outcome of every route — the gates, the audit, the tasks
//! that failed. So the curve is a query over evidence we already keep, on our own traffic.
//!
//! WHAT THIS DELIBERATELY DOES NOT DO. It does not correct the router, re-weight anything, or hide a grade it distrusts.
//! Calibration is a MEASUREMENT of a claim, and the first honest use of it is to show the operator whether the claim
//! survives contact with the record. Acting on it — training the grade, or dropping it — is a decision that needs the
//! curve to exist first, and needs a person.
use serde_json::{json, Value};

/// the buckets the router actually emits, in the order a person reads them
pub const GRADES: [&str; 3] = ["high", "medium", "low"];

/// One settled route: what it claimed, and what happened.
#[derive(Clone, Debug, PartialEq)]
pub struct Graded {
    pub confidence: String,
    pub job: String,
    pub quality: f64,
    pub t: i64,
}

fn open() -> Result<rusqlite::Connection, String> {
    let c = crate::merges::open().map_err(|e| e.to_string())?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS route_grades (
        id INTEGER PRIMARY KEY AUTOINCREMENT, t INTEGER NOT NULL,
        confidence TEXT NOT NULL, job TEXT NOT NULL DEFAULT '', tier TEXT NOT NULL DEFAULT '',
        delegate INTEGER NOT NULL DEFAULT 0, quality REAL NOT NULL, tasks INTEGER NOT NULL DEFAULT 0);")
        .map_err(|e| e.to_string())?;
    Ok(c)
}

/// FILE ONE SETTLED ROUTE. Called at the join, where the run's own numbers are already in hand — the same place the
/// merge row is written, and for the same reason: it is the moment the claim can be checked.
///
/// A route with no grade is not recorded: the router did not claim anything, so there is nothing to check, and
/// inventing a bucket for it would put made-up rows in the one table whose whole purpose is honesty about a claim.
pub fn note(confidence: &str, job: &str, tier: &str, delegate: bool, quality: f64, tasks: i64) -> Result<(), String> {
    let g = confidence.trim().to_ascii_lowercase();
    if g.is_empty() { return Ok(()) }
    let c = open()?;
    c.execute("INSERT INTO route_grades (t, confidence, job, tier, delegate, quality, tasks) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        rusqlite::params![crate::cli::now_ms(), g, job, tier, i64::from(delegate), quality.clamp(0.0, 1.0), tasks])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// THE CURVE, from rows. Pure so the verdict can be tested without a database.
///
/// Per bucket: how many routes carried that grade and what they actually scored. The verdict compares the buckets in
/// the only way that matters — a grade is worth having if a higher one is RIGHT MORE OFTEN. Ordering is the test, not
/// the absolute numbers: a router that scores 0.9 on everything is not calibrated, it is lucky, and one that scores
/// 0.4 on everything is not miscalibrated, it is bad at the work.
pub fn curve(rows: &[Graded]) -> Value {
    let mut buckets: Vec<Value> = vec![];
    let mut means: Vec<(String, f64, usize)> = vec![];
    for g in GRADES {
        let mine: Vec<&Graded> = rows.iter().filter(|r| r.confidence == g).collect();
        let n = mine.len();
        let mean = if n == 0 { 0.0 } else { mine.iter().map(|r| r.quality).sum::<f64>() / n as f64 };
        if n > 0 { means.push((g.to_string(), mean, n)); }
        buckets.push(json!({ "confidence": g, "n": n, "quality": (mean * 1000.0).round() / 1000.0,
                             "measured": n > 0 }));
    }
    // THE VERDICT IS REFUSED UNTIL THERE IS SOMETHING TO JUDGE. Two buckets with a handful of runs each cannot tell
    // you whether a grade separates, and a confident answer drawn from four samples would be the very failure this
    // measurement exists to expose.
    let total: usize = means.iter().map(|(_, _, n)| n).sum();
    let named: Vec<&(String, f64, usize)> = means.iter().filter(|(_, _, n)| *n >= 3).collect();
    let verdict = if total == 0 {
        "no route has been graded yet — the curve fills as runs settle".to_string()
    } else if named.len() < 2 {
        format!("{total} graded run(s), but fewer than two grades have 3 samples — not enough to say whether the grade separates")
    } else {
        // does a higher grade actually score higher? compare the best-evidenced pair in order
        let mut ordered = named.clone();
        ordered.sort_by_key(|(g, _, _)| GRADES.iter().position(|x| x == g).unwrap_or(9));
        let top = ordered.first().unwrap();
        let bottom = ordered.last().unwrap();
        let gap = top.1 - bottom.1;
        if gap > 0.10 {
            format!("the grade separates: `{}` runs score {:.2}, `{}` {:.2} over {total} graded runs", top.0, top.1, bottom.0, bottom.1)
        } else if gap < -0.10 {
            format!("THE GRADE IS BACKWARDS: `{}` scores {:.2} but `{}` scores {:.2} — a higher grade is doing worse", top.0, top.1, bottom.0, bottom.1)
        } else {
            format!("the grade is decoration so far: `{}` {:.2} vs `{}` {:.2} over {total} runs — it does not predict the outcome",
                    top.0, top.1, bottom.0, bottom.1)
        }
    };
    json!({ "buckets": buckets, "total": total, "verdict": verdict })
}

/// the curve over everything on disk
pub fn report() -> Result<Value, String> {
    let c = open()?;
    let mut st = c.prepare("SELECT confidence, job, quality, t FROM route_grades ORDER BY t DESC LIMIT 2000").map_err(|e| e.to_string())?;
    let rows: Vec<Graded> = st.query_map([], |r| Ok(Graded { confidence: r.get(0)?, job: r.get(1)?, quality: r.get(2)?, t: r.get(3)? }))
        .map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
    let mut out = curve(&rows);
    out["ok"] = json!(true);
    Ok(out)
}

pub async fn http_report() -> axum::response::Response {
    use axum::response::IntoResponse;
    match tokio::task::spawn_blocking(report).await.unwrap_or_else(|e| Err(e.to_string())) {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn g(conf: &str, q: f64) -> Graded { Graded { confidence: conf.into(), job: "editor-l".into(), quality: q, t: 0 } }

    #[test]
    fn an_empty_record_says_so_instead_of_drawing_a_curve() {
        let c = curve(&[]);
        assert_eq!(c["total"], 0);
        assert!(c["verdict"].as_str().unwrap().contains("no route has been graded"));
        assert!(c["buckets"].as_array().unwrap().iter().all(|b| b["measured"] == json!(false)));
    }

    #[test]
    fn a_handful_of_runs_refuses_a_verdict_rather_than_guessing_one() {
        let rows = vec![g("high", 0.9), g("high", 0.9), g("low", 0.2)];
        let v = curve(&rows)["verdict"].as_str().unwrap().to_string();
        assert!(v.contains("not enough"), "{v}");
    }

    #[test]
    fn a_grade_that_predicts_the_outcome_is_named_as_separating() {
        let mut rows = vec![];
        for _ in 0..5 { rows.push(g("high", 0.92)); }
        for _ in 0..5 { rows.push(g("low", 0.40)); }
        let c = curve(&rows);
        assert!(c["verdict"].as_str().unwrap().contains("separates"), "{}", c["verdict"]);
        assert_eq!(c["buckets"][0]["n"], 5);
        assert!((c["buckets"][0]["quality"].as_f64().unwrap() - 0.92).abs() < 1e-9);
    }

    #[test]
    fn a_grade_that_predicts_nothing_is_called_decoration() {
        let mut rows = vec![];
        for _ in 0..6 { rows.push(g("high", 0.80)); }
        for _ in 0..6 { rows.push(g("low", 0.78)); }
        let v = curve(&rows)["verdict"].as_str().unwrap().to_string();
        assert!(v.contains("decoration"), "{v}");
    }

    /// The finding that would matter most, so it gets its own words rather than being folded into "decoration".
    #[test]
    fn a_grade_that_runs_backwards_is_said_out_loud() {
        let mut rows = vec![];
        for _ in 0..6 { rows.push(g("high", 0.40)); }
        for _ in 0..6 { rows.push(g("low", 0.85)); }
        let v = curve(&rows)["verdict"].as_str().unwrap().to_string();
        assert!(v.contains("BACKWARDS"), "{v}");
    }

    #[test]
    fn an_ungraded_route_is_not_recorded_as_a_bucket_of_its_own() {
        let _ = std::fs::remove_file(crate::merges::path());
        note("", "editor-l", "high", true, 0.9, 3).unwrap();
        note("high", "editor-l", "high", true, 0.9, 3).unwrap();
        let r = report().unwrap();
        assert_eq!(r["total"], 1, "the routes that claimed nothing are not in the curve");
    }
}
