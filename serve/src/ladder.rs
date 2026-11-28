//! THE FALLBACK LADDER, WHICH REORDERS ITSELF (HyperAgent's "degradation is the normal path"; queued 2026-09-25).
//!
//! Today a failed job falls back once and leaves a line in the transcript. That line is thrown away, so the same
//! binding can fail every day of the week and still be tried first every time — the router learns nothing from the one
//! event that is purely about reliability rather than taste.
//!
//! HyperAgent's insight is that degradation should be the NORMAL path rather than the error path. Ours is better in
//! one way and worse in another: we have a record, so a rung that keeps failing can be demoted from evidence — but we
//! must be careful, because an engine that was down for an hour is not an engine that is bad.
//!
//! So: every attempt is recorded with which RUNG answered it, and the order is recomputed from that record —
//!   · a rung with fewer than `MIN_N` attempts keeps its declared place, because two failures on a Tuesday are not a
//!     verdict;
//!   · above that, rungs are ordered by how often they ANSWER, and ties go to the cheaper one;
//!   · and the declared winner is never dropped, only overtaken: a ladder that deleted rungs would eventually have
//!     nothing left to fall back to.
//!
//! The ladder is DESCRIBED here and consulted by the router; nothing in this file spends a cent or calls a model.
use serde_json::{json, Value};

/// attempts before the record is allowed to move a rung — the same "two is not a verdict" line the rest of the record
/// draws, and the number a person can argue with in one place
pub const MIN_N: i64 = 4;

/// one step of a ladder: who to ask, and what that step is FOR
#[derive(Clone, Debug, PartialEq)]
pub struct Rung {
    pub engine: String,
    pub model: String,
    /// `winner` · `fallback` · `raw` — the role the rung plays, not its rank, which the record decides
    pub role: &'static str,
    /// attempts and answers from the record, for the screen and for the ordering
    pub n: i64,
    pub ok: i64,
    pub usd: f64,
}
impl Rung {
    pub fn key(&self) -> String { format!("{}:{}", self.engine, if self.model.is_empty() { "default" } else { &self.model }) }
    /// how often this rung actually answered. An unmeasured rung is NOT scored zero — it is scored as unknown, which
    /// the ordering treats as "leave it where it was declared". Pure.
    pub fn answer_rate(&self) -> Option<f64> { if self.n >= MIN_N { Some(self.ok as f64 / self.n as f64) } else { None } }
    pub fn json(&self) -> Value {
        json!({ "engine": self.engine, "model": self.model, "role": self.role, "n": self.n, "ok": self.ok,
                "usd": (self.usd * 1000.0).round() / 1000.0, "answer_rate": self.answer_rate() })
    }
}

fn open() -> Result<rusqlite::Connection, String> {
    let c = crate::merges::open().map_err(|e| e.to_string())?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS ladder_rungs (
        job TEXT NOT NULL, rung TEXT NOT NULL, role TEXT NOT NULL DEFAULT '',
        n INTEGER NOT NULL DEFAULT 0, ok INTEGER NOT NULL DEFAULT 0,
        usd REAL NOT NULL DEFAULT 0, ms INTEGER NOT NULL DEFAULT 0, last_error TEXT NOT NULL DEFAULT '',
        last_seen INTEGER NOT NULL DEFAULT 0, UNIQUE (job, rung));").map_err(|e| e.to_string())?;
    Ok(c)
}

/// A CALL THAT NEVER REACHED A MODEL IS NOT EVIDENCE (operator 2026-09-27, the Zen 402).
///
/// `planner opencode:deepseek-v4-pro` stood in the record at 0 ok out of 5 — five refusals from
/// `cli::model_blocked`, every one of them decided here before a single token left the machine. Read back as
/// an answer rate that is a verdict on the MODEL, which it is not: turn Zen on tomorrow and those five
/// phantom failures would still be arguing against a rung nobody has ever measured.
///
/// So a refusal is not recorded, and the rows already carrying one are dropped (`forget_refusals`). It is the
/// same doctrine as the legacy archive one file over: keep the table to things that actually happened.
/// Matched on the sentences `model_blocked` itself writes — pure, tested.
pub fn is_refusal(error: &str) -> bool {
    // MATCHED AT THE HEAD OF THE MESSAGE, NOT ANYWHERE IN IT (audit 2026-09-28). The string reaching `note()` is
    // `"{engine} produced no answer (exit {code}): {up to 500 chars of stderr}"`, so a loose `contains` was a
    // search of arbitrary engine output — and THIS REPOSITORY contains the literal "(Settings › Seats)" in
    // three files. An engine that read one of them, echoed a line and then crashed had its genuine failure filed
    // as a policy refusal and dropped from the record entirely. "switched off" is plain English besides.
    //
    // A real refusal is written by `cli::model_blocked` and is the WHOLE message: it never carries an engine
    // prefix, because the call was stopped before an engine was reached. So it is anchored, and it must match one
    // of the sentences that function actually produces.
    let e = error.trim().to_ascii_lowercase();
    if e.is_empty() { return false }
    if e.contains("produced no answer") { return false }      // that shape is an engine that RAN and failed
    e.starts_with("opencode zen is off")
        || (e.ends_with(" is switched off (settings › providers).") && !e.contains("produced no answer"))
        || (e.starts_with("is a chinese model") || e.contains(" is a chinese model and they are off"))
        || e.contains(" is not one of cursor's own models")
        || e.contains(" is not one of google's own models")
}

/// Drop a rung whose whole record is refusals — nothing it holds was measured. Returns how many went.
///
/// ONLY WHEN THE WHOLE RECORD IS ONE (audit 2026-09-28). This deleted on the strength of `last_error` alone, so a
/// rung with nineteen genuine failures and one final refusal lost all twenty attempts, its cost and its time.
/// `note()` no longer records a refusal at all, so any rung written since then cannot be in that state; what is
/// left is the rows written BEFORE the guard existed, and a row is only safe to forget when every attempt it
/// counts was refused — which, for those rows, is `ok = 0` and `n` equal to the refusals we can still see. That
/// cannot be reconstructed after the fact, so the bar is deliberately conservative: a SINGLE attempt, refused.
/// A rung with real attempts mixed in keeps them and simply stops growing, which `reachable` already handles.
pub fn forget_refusals() -> usize {
    let Ok(c) = open() else { return 0 };
    let rows: Vec<(String, String, String, i64)> = c.prepare("SELECT job, rung, last_error, n FROM ladder_rungs WHERE ok = 0 AND last_error <> ''")
        .and_then(|mut st| st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map(|rs| rs.filter_map(Result::ok).collect()))
        .unwrap_or_default();
    let mut gone = 0;
    for (job, rung, err, n) in rows {
        if !is_refusal(&err) || n > 1 { continue }
        if c.execute("DELETE FROM ladder_rungs WHERE job = ?1 AND rung = ?2", rusqlite::params![job, rung]).is_ok() { gone += 1; }
    }
    if gone > 0 { eprintln!("ladder: {gone} rung(s) forgotten — their one attempt was a refusal, so nothing was measured"); }
    gone
}

/// RECORD ONE ATTEMPT — the fact the transcript used to keep to itself. Called for every rung tried, answered or not.
pub fn note(job: &str, engine: &str, model: &str, role: &str, ok: bool, usd: f64, ms: i64, error: &str) {
    if !ok && is_refusal(error) { return }        // a refusal is not an attempt — see `is_refusal`
    let Ok(c) = open() else { return };
    let rung = format!("{}:{}", engine, if model.is_empty() { "default" } else { model });
    let _ = c.execute(
        "INSERT INTO ladder_rungs (job, rung, role, n, ok, usd, ms, last_error, last_seen) VALUES (?1,?2,?3,1,?4,?5,?6,?7,?8)
         ON CONFLICT(job, rung) DO UPDATE SET n = n + 1, ok = ok + ?4, usd = usd + ?5, ms = ms + ?6,
           last_error = CASE WHEN ?4 = 0 THEN ?7 ELSE last_error END, last_seen = ?8",
        rusqlite::params![job, rung, role, i64::from(ok), usd, ms, error.chars().take(160).collect::<String>(), crate::cli::now_ms()]);
}

/// what the record knows about one job's rungs
pub fn measured(job: &str) -> Vec<(String, i64, i64, f64)> {
    let Ok(c) = open() else { return vec![] };
    let Ok(mut st) = c.prepare("SELECT rung, n, ok, usd FROM ladder_rungs WHERE job = ?1") else { return vec![] };
    st.query_map(rusqlite::params![job], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, f64>(3)?)))
        .map(|rows| rows.filter_map(Result::ok).collect()).unwrap_or_default()
}

/// THE ORDER TO TRY THEM IN. `declared` is the ladder as the jobs table and the pair define it — winner first — and
/// this returns the same rungs, possibly reordered by what the record has seen. Pure: the caller supplies the
/// measurements, so the rule can be tested without a database.
///
/// Reordering is CONSERVATIVE on purpose. A rung with fewer than MIN_N attempts keeps its declared position, because
/// an engine that was down for an hour is not an engine that is bad. Only rungs with enough evidence compete, and they
/// compete on answering — ties to the cheaper. Nothing is ever dropped: a ladder that deleted its rungs would end up
/// with nothing to fall back to.
pub fn order(declared: &[Rung]) -> Vec<Rung> {
    let mut out = declared.to_vec();
    // stable sort: everything without a verdict keeps its relative place, and a measured rung moves only against
    // other measured rungs
    out.sort_by(|a, b| match (a.answer_rate(), b.answer_rate()) {
        (Some(ra), Some(rb)) => rb.partial_cmp(&ra).unwrap_or(std::cmp::Ordering::Equal)
            .then((a.usd / a.n.max(1) as f64).partial_cmp(&(b.usd / b.n.max(1) as f64)).unwrap_or(std::cmp::Ordering::Equal)),
        _ => std::cmp::Ordering::Equal,
    });
    out
}

/// the line the transcript gets when the order is not the declared one — silence would make a reordering feel like a
/// bug the next time someone reads the log
pub fn moved_line(declared: &[Rung], ordered: &[Rung]) -> Option<String> {
    let (d, o) = (declared.first()?, ordered.first()?);
    if d.key() == o.key() { return None }
    Some(format!("{} answers {:.0}% of the time against {:.0}% for {} — trying it first",
        o.key(), o.answer_rate().unwrap_or(0.0) * 100.0, d.answer_rate().unwrap_or(0.0) * 100.0, d.key()))
}

/// every job's ladder, for the screen
pub fn report() -> Result<Value, String> {
    let c = open()?;
    let mut st = c.prepare("SELECT job, rung, role, n, ok, usd, last_error, last_seen FROM ladder_rungs ORDER BY job, ok * 1.0 / n DESC, n DESC").map_err(|e| e.to_string())?;
    let rows: Vec<Value> = st.query_map([], |r| {
        let (n, ok): (i64, i64) = (r.get(3)?, r.get(4)?);
        Ok(json!({ "job": r.get::<_, String>(0)?, "rung": r.get::<_, String>(1)?, "role": r.get::<_, String>(2)?,
                   "n": n, "ok": ok, "answer_rate": if n >= MIN_N { Some(ok as f64 / n as f64) } else { None },
                   "usd": r.get::<_, f64>(5)?, "last_error": r.get::<_, String>(6)?, "last_seen": r.get::<_, i64>(7)? }))
    }).map_err(|e| e.to_string())?.filter_map(Result::ok).collect();
    let failing: Vec<&Value> = rows.iter().filter(|r| r["answer_rate"].as_f64().map(|x| x < 0.8).unwrap_or(false)).collect();
    let verdict = if rows.is_empty() {
        "no job has fallen back yet — nothing has failed often enough to have a ladder".to_string()
    } else if failing.is_empty() {
        format!("{} rung(s) recorded, none answering below 80% — the declared order stands", rows.len())
    } else {
        format!("{} rung(s) answering below 80%: {}", failing.len(),
                failing.iter().take(3).map(|r| format!("{} ({:.0}%)", r["rung"].as_str().unwrap_or(""), r["answer_rate"].as_f64().unwrap_or(0.0) * 100.0)).collect::<Vec<_>>().join(" · "))
    };
    Ok(json!({ "ok": true, "rows": rows, "verdict": verdict, "min_n": MIN_N }))
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
    fn rung(engine: &str, model: &str, role: &'static str, n: i64, ok: i64, usd: f64) -> Rung {
        Rung { engine: engine.into(), model: model.into(), role, n, ok, usd }
    }

    #[test]
    fn a_rung_with_too_little_evidence_keeps_its_declared_place() {
        // two failures on a Tuesday are not a verdict about an engine
        let declared = vec![rung("claude-code", "sonnet", "winner", 3, 1, 0.3), rung("opencode", "qwen", "fallback", 2, 2, 0.01)];
        let ordered = order(&declared);
        assert_eq!(ordered[0].key(), "claude-code:sonnet", "the declared winner stays until there is evidence");
        assert!(moved_line(&declared, &ordered).is_none());
    }

    #[test]
    fn a_rung_that_keeps_failing_is_overtaken_once_the_evidence_is_there() {
        let declared = vec![rung("claude-code", "sonnet", "winner", 10, 4, 3.0), rung("opencode", "qwen", "fallback", 10, 10, 0.1)];
        let ordered = order(&declared);
        assert_eq!(ordered[0].key(), "opencode:qwen");
        let line = moved_line(&declared, &ordered).expect("a reordering must announce itself");
        assert!(line.contains("100%") && line.contains("40%"), "{line}");
    }

    #[test]
    fn nothing_is_ever_dropped_however_badly_it_answers() {
        let declared = vec![rung("claude-code", "sonnet", "winner", 10, 0, 3.0), rung("opencode", "qwen", "fallback", 10, 10, 0.1)];
        let ordered = order(&declared);
        assert_eq!(ordered.len(), 2, "a ladder that deletes rungs runs out of rungs");
        assert_eq!(ordered[1].key(), "claude-code:sonnet");
    }

    #[test]
    fn equal_answering_goes_to_the_cheaper_rung() {
        let declared = vec![rung("claude-code", "opus", "winner", 8, 8, 8.0), rung("opencode", "qwen", "fallback", 8, 8, 0.08)];
        assert_eq!(order(&declared)[0].key(), "opencode:qwen");
    }

    #[test]
    fn an_unmeasured_ladder_is_the_declared_ladder() {
        let declared = vec![rung("claude-code", "sonnet", "winner", 0, 0, 0.0), rung("opencode", "qwen", "fallback", 0, 0, 0.0)];
        assert_eq!(order(&declared).iter().map(|r| r.key()).collect::<Vec<_>>(),
                   declared.iter().map(|r| r.key()).collect::<Vec<_>>());
    }

    #[test]
    fn the_record_counts_attempts_and_answers_and_keeps_the_last_error() {
        let _ = std::fs::remove_file(crate::merges::path());
        note("planner", "claude-code", "sonnet", "winner", false, 0.0, 1200, "the engine timed out");
        note("planner", "claude-code", "sonnet", "winner", true, 0.30, 40_000, "");
        note("planner", "opencode", "qwen", "fallback", true, 0.01, 30_000, "");
        let m = measured("planner");
        let sonnet = m.iter().find(|(k, _, _, _)| k == "claude-code:sonnet").unwrap();
        assert_eq!((sonnet.1, sonnet.2), (2, 1), "two attempts, one answer");
        let r = report().unwrap();
        assert!(r["rows"].as_array().unwrap().iter().any(|x| x["last_error"].as_str().unwrap_or("").contains("timed out")),
            "a rung's last failure is kept — a ladder that forgets why it moved cannot be argued with");
        assert!(r["verdict"].as_str().unwrap().contains("rung(s) recorded") || r["verdict"].as_str().unwrap().contains("below 80%"), "{}", r["verdict"]);
    }
}

/// A SWITCHED-OFF SEAT IS NEVER CALLED, FROM ANY RUNG (operator 2026-09-27: "OpenCode Zen is off … even though at
/// settings is already selected another provider to heimdall"). The record's winner and fallback are what was MEASURED,
/// not what is reachable today: a rung whose seat is off (Zen) or whose model family is off (Chinese models) is dropped
/// before the first call, and `stand_in` — the run's own reachable seat — is the ladder when nothing is left. Returns
/// the rungs to try and the keys it dropped, for the run's log. Pure.
pub fn reachable(ordered: Vec<Rung>, blocked: impl Fn(&Rung) -> bool, stand_in: impl FnOnce() -> Rung) -> (Vec<Rung>, Vec<String>) {
    let mut kept = vec![]; let mut dropped = vec![];
    for r in ordered { if blocked(&r) { dropped.push(r.key()); } else { kept.push(r); } }
    if kept.is_empty() { kept.push(stand_in()); }
    (kept, dropped)
}

#[cfg(test)]
mod reachable_tests {
    use super::*;
    fn rung(e: &str, m: &str, role: &'static str) -> Rung { Rung { engine: e.into(), model: m.into(), role, n: 0, ok: 0, usd: 0.0 } }
    #[test]
    fn a_rung_on_a_switched_off_seat_is_skipped_and_the_run_engine_stands_in_when_none_is_left() {
        let off = |r: &Rung| r.engine == "opencode";
        let (kept, dropped) = reachable(vec![rung("opencode", "opencode/deepseek-v4-pro", "winner"), rung("claude-code", "sonnet", "fallback")], off, || rung("codex", "", "winner"));
        assert_eq!(kept.iter().map(|r| r.key()).collect::<Vec<_>>(), vec!["claude-code:sonnet"], "the measured winner is off today: the fallback is tried first, not after a refusal");
        assert_eq!(dropped, vec!["opencode:opencode/deepseek-v4-pro"]);
        let (kept, dropped) = reachable(vec![rung("opencode", "a", "winner"), rung("opencode", "b", "fallback")], off, || rung("codex", "", "winner"));
        assert_eq!(kept.iter().map(|r| r.key()).collect::<Vec<_>>(), vec!["codex:default"], "both rungs off: the run's own seat, never a refusal");
        assert_eq!(dropped.len(), 2);
        let (kept, dropped) = reachable(vec![rung("claude-code", "opus", "winner"), rung("codex", "", "fallback")], off, || rung("gemini", "", "winner"));
        assert_eq!(kept.len(), 2); assert!(dropped.is_empty(), "a reachable ladder is left exactly as ordered");
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::*;
    #[test]
    fn a_policy_refusal_is_never_read_as_a_model_failure() {
        assert!(is_refusal("OpenCode Zen is off (Settings › Seats). It bills metered credit, not your subscription — turn it on only if you hold Zen credit."));
        assert!(is_refusal("deepseek-v4-pro is a Chinese model and they are off (Settings › Seats)."));
        assert!(is_refusal("claude-sonnet is not one of Cursor's own models: it bills at API rates"));
        assert!(is_refusal("gpt-5.6 is not one of Google's own models: reaching it through Antigravity"));
        assert!(!is_refusal("Upstream request failed: Insufficient account funds"), "a 402 from the provider IS a real attempt — the call was made");
        // the audit's false positives: engine stderr that merely MENTIONS the words
        assert!(!is_refusal("claude-code produced no answer (exit 1): thread 'main' panicked at cli.rs: (Settings › Seats)"),
                "an engine that read our own source and crashed is a real failure, not a policy refusal");
        assert!(!is_refusal("extended thinking is switched off for this model"), "a vendor sentence is not our gate");
        assert!(!is_refusal("the model returned nothing"));
        assert!(!is_refusal(""));
    }
}
