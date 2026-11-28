//! WHILE YOU WERE AWAY (RFC 0010 §4.17, BL-8): what the teams did since the person last looked: runs started and finished, parked at a gate, failed, and the calls that triggers skipped. Read-only, built from the run
//! files and the trigger logs, so it is right after the window was closed, the machine restarted or a phone opened the app. Pure over the rows; the op reads the files.
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct RunLine { pub id: String, pub team: String, pub ask: String, pub status: String, pub why: String, pub started_ms: i64, pub ended_ms: i64, pub spent_usd: f64 }
#[derive(Clone, Debug, PartialEq)]
pub struct SkipLine { pub team: String, pub trigger: String, pub t_ms: i64, pub why: String }

/// One run's line from its state file (the whole `state.json`). None when it is not a run.
pub fn line_of(v: &Value) -> Option<RunLine> {
    let st = &v["state"]; let id = v["id"].as_str()?; if st.is_null() { return None }
    let status = st["status"]["status"].as_str().or_else(|| st["status"].as_str()).unwrap_or("idle").to_string();
    Some(RunLine { id: id.to_string(), team: v["team"].as_str().unwrap_or("").to_string(), ask: v["ask"].as_str().unwrap_or("").chars().take(160).collect(), why: st["status"]["why"].as_str().unwrap_or("").to_string(), status, started_ms: st["started_ms"].as_i64().unwrap_or(0), ended_ms: st["now_ms"].as_i64().unwrap_or(0), spent_usd: st["spent_usd"].as_f64().unwrap_or(0.0) })
}
fn kind_of(r: &RunLine) -> &'static str {
    match r.status.as_str() { "done" => "finished", "blocked" => "failed", "stopped" => "stopped", "paused" => "parked", "running" => "running", _ => "other" }
}
/// The digest since `since_ms`: counts, one sentence per thing that needs the person, and the total spent. Only runs that STARTED or ENDED inside the window count. Pure.
pub fn digest(runs: &[RunLine], skips: &[SkipLine], since_ms: i64, now_ms: i64) -> Value {
    let inside: Vec<&RunLine> = runs.iter().filter(|r| r.started_ms >= since_ms || (r.ended_ms >= since_ms && kind_of(r) != "running")).collect();
    let n = |k: &str| inside.iter().filter(|r| kind_of(r) == k).count();
    let (finished, failed, parked, running, stopped) = (n("finished"), n("failed"), n("parked"), n("running"), n("stopped"));
    let spent: f64 = inside.iter().map(|r| r.spent_usd).sum();
    let skipped: Vec<&SkipLine> = skips.iter().filter(|s| s.t_ms >= since_ms).collect();
    let mut needs: Vec<String> = vec![];
    for r in inside.iter().filter(|r| kind_of(r) == "failed") { needs.push(format!("{} · run {} did not finish: {}", r.team, r.id, if r.why.is_empty() { "it stopped" } else { r.why.as_str() })); }
    for r in inside.iter().filter(|r| kind_of(r) == "parked") { needs.push(format!("{} · run {} is waiting for you", r.team, r.id)); }
    let mut parts = vec![]; let started = inside.iter().filter(|r| r.started_ms >= since_ms).count();
    if started > 0 { parts.push(format!("{started} run{} started", if started == 1 { "" } else { "s" })) }
    if finished > 0 { parts.push(format!("{finished} finished")) } if failed > 0 { parts.push(format!("{failed} failed")) } if parked > 0 { parts.push(format!("{parked} waiting for you")) } if running > 0 { parts.push(format!("{running} still running")) }
    if stopped > 0 { parts.push(format!("{stopped} stopped")) } if !skipped.is_empty() { parts.push(format!("{} trigger call{} skipped", skipped.len(), if skipped.len() == 1 { "" } else { "s" })) }
    let words = if parts.is_empty() { "Nothing happened while you were away.".to_string() } else { format!("While you were away: {}. Spent ${spent:.2}.", parts.join(", ")) };
    let mut sorted: Vec<&&RunLine> = inside.iter().collect(); sorted.sort_by(|a, b| b.ended_ms.cmp(&a.ended_ms).then(a.id.cmp(&b.id)));
    let recent: Vec<Value> = sorted.into_iter().take(40).map(|r| json!({ "id": r.id, "team": r.team, "ask": r.ask, "kind": kind_of(r), "why": r.why, "started_ms": r.started_ms, "ended_ms": r.ended_ms, "spent_usd": r.spent_usd })).collect();
    json!({ "ok": true, "since_ms": since_ms, "now_ms": now_ms, "started": started, "finished": finished, "failed": failed, "parked": parked, "running": running, "stopped": stopped, "skipped": skipped.len(), "spent_usd": (spent * 10_000.0).round() / 10_000.0,
        "runs": recent, "needs_you": needs, "skips": skipped.iter().take(10).map(|s| json!({ "team": s.team, "trigger": s.trigger, "t_ms": s.t_ms, "why": s.why })).collect::<Vec<_>>(), "words": words, "quiet": parts.is_empty() })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(id: &str, team: &str, status: &str, why: &str, start: i64, end: i64, usd: f64) -> RunLine { RunLine { id: id.into(), team: team.into(), ask: String::new(), status: status.into(), why: why.into(), started_ms: start, ended_ms: end, spent_usd: usd } }
    #[test]
    fn the_digest_counts_what_happened_since_and_names_what_needs_the_person() {
        let runs = vec![run("a-1", "A", "done", "", 100, 200, 0.5), run("a-2", "A", "blocked", "composer did not go to plan", 300, 400, 0.05), run("b-1", "B", "paused", "", 500, 600, 0.1), run("c-1", "C", "running", "", 700, 800, 0.2), run("old-1", "Old", "done", "", 1, 2, 9.0)];
        let skips = vec![SkipLine { team: "A".into(), trigger: "t1".into(), t_ms: 350, why: "the day's limit of 6 scheduled runs is reached".into() }, SkipLine { team: "A".into(), trigger: "t1".into(), t_ms: 5, why: "old".into() }];
        let d = digest(&runs, &skips, 50, 1000);
        assert_eq!((d["started"].as_i64(), d["finished"].as_i64(), d["failed"].as_i64(), d["parked"].as_i64(), d["running"].as_i64(), d["skipped"].as_i64()), (Some(4), Some(1), Some(1), Some(1), Some(1), Some(1)));
        assert_eq!(d["spent_usd"], json!(0.85), "the old run is outside the window: {d}");
        let needs: Vec<&str> = d["needs_you"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect(); assert_eq!(needs.len(), 2); assert!(needs[0].contains("composer did not go to plan") && needs[1].contains("waiting for you"), "{needs:?}");
        let w = d["words"].as_str().unwrap(); assert!(w.starts_with("While you were away: 4 runs started, 1 finished, 1 failed, 1 waiting for you, 1 still running, 1 trigger call skipped") && w.contains("$0.85"), "{w}");
        // a quiet window says so
        let q = digest(&runs, &skips, 5_000, 6_000); assert_eq!(q["quiet"], json!(true)); assert_eq!(q["words"], json!("Nothing happened while you were away."));
        // a run that began before the window but ended inside it still counts as finished
        let e = digest(&[run("x", "X", "done", "", 10, 90, 1.0)], &[], 50, 100); assert_eq!(e["finished"], json!(1)); assert_eq!(e["started"], json!(0));
    }
    #[test]
    fn a_run_file_becomes_a_line_and_anything_else_does_not() {
        let v = json!({ "id": "x-1", "team": "X", "ask": "do x", "state": { "status": { "status": "blocked", "why": "stuck" }, "started_ms": 5, "now_ms": 9, "spent_usd": 0.3 } });
        let mut want = run("x-1", "X", "blocked", "stuck", 5, 9, 0.3); want.ask = "do x".into(); assert_eq!(line_of(&v), Some(want));
        assert_eq!(line_of(&json!({ "id": "y", "state": { "status": "done", "started_ms": 1, "now_ms": 2, "spent_usd": 0 } })).map(|r| r.status), Some("done".into()), "a plain status string reads too");
        assert!(line_of(&json!({ "nope": 1 })).is_none() && line_of(&json!({ "id": "z" })).is_none());
    }
}
