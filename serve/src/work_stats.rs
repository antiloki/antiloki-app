//! THE TIMELINE'S OWN INTELLIGENCE (operator 2026-09-24: *"stats on timeline and stats on fuel is not the same
//! thing, are different stats, on the fuel is ok, but on the timeline should be another kind of deterministic
//! inteligence be creative"*).
//!
//! Fuel answers "where did the money go". That is a real question and it is answered elsewhere. The timeline's
//! question is different: **how does the work actually go?** Not what it cost — whether it lands first time,
//! where it keeps going back to, when in the day it happens, and how much looking each change takes.
//!
//! Every number here is counted from rows the run ledger already holds. Nothing is modelled, nothing is
//! estimated, and where the evidence is too thin to say something the answer is that it is too thin.
use serde_json::{json, Value};

fn n(v: Option<&Value>) -> f64 { v.and_then(Value::as_f64).unwrap_or(0.0) }
fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }
fn arr(v: Option<&Value>) -> Vec<Value> { v.and_then(Value::as_array).cloned().unwrap_or_default() }

/// FIRST TIME RIGHT — of the tasks that finished, how many passed on their first attempt.
///
/// The single most useful number nobody shows: a model that gets there on attempt three is not the same as one
/// that gets there, and the difference is paid for twice — once in money and once in the operator's afternoon.
/// Pure — tested.
pub fn first_try(runs: &[Value]) -> Value {
    let (mut first, mut retried, mut failed) = (0usize, 0usize, 0usize);
    let mut by_engine: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for r in runs {
        let worker = { let w = s(r.get("workers")); if w.is_empty() { s(r.get("engine")) } else { w } };
        for t in arr(r.get("tasks")) {
            let status = s(t.get("status"));
            if status != "done" && status != "failed" { continue }
            let attempts = n(t.get("attempts")).max(1.0) as usize;
            if status == "failed" { failed += 1; continue }
            let e = by_engine.entry(worker.clone()).or_insert((0, 0));
            e.1 += 1;
            if attempts <= 1 { first += 1; e.0 += 1; } else { retried += 1 }
        }
    }
    let settled = first + retried;
    json!({ "first": first, "retried": retried, "failed": failed, "settled": settled,
        "pct": if settled > 0 { first as f64 / settled as f64 * 100.0 } else { 0.0 },
        "by_engine": by_engine.into_iter().filter(|(_, (_, tot))| *tot >= 3)
            .map(|(k, (ok, tot))| json!({ "engine": k, "first": ok, "tasks": tot, "pct": ok as f64 / tot as f64 * 100.0 })).collect::<Vec<_>>(),
        "note": if settled < 5 { "too few finished tasks to read anything into this yet" } else { "" } })
}

/// WHERE THE WORK KEEPS GOING BACK TO — the files that appear in the most runs. Not git churn: this is churn as
/// the AGENTS experienced it, which is the map of where your codebase is hard. Pure — tested.
pub fn hotspots(runs: &[Value], top: usize) -> Vec<Value> {
    let mut seen: std::collections::BTreeMap<String, (usize, usize)> = Default::default();   // path → (runs, retries)
    for r in runs {
        let mut in_this_run: std::collections::BTreeMap<String, usize> = Default::default();
        for t in arr(r.get("tasks")) {
            let retried = n(t.get("attempts")).max(1.0) as usize > 1;
            for f in arr(t.get("owns")).into_iter().chain(arr(t.get("changed"))) {
                let p = s(Some(&f));
                if p.is_empty() { continue }
                let e = in_this_run.entry(p).or_insert(0);
                if retried { *e += 1 }
            }
        }
        for (p, retries) in in_this_run {
            let e = seen.entry(p).or_insert((0, 0));
            e.0 += 1; e.1 += retries;
        }
    }
    let mut out: Vec<Value> = seen.into_iter().filter(|(_, (runs, _))| *runs > 1)
        .map(|(path, (runs, retries))| json!({ "path": path, "runs": runs, "retries": retries })).collect();
    out.sort_by(|a, b| (n(b.get("runs")), n(b.get("retries"))).partial_cmp(&(n(a.get("runs")), n(a.get("retries")))).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(top);
    out
}

/// WHAT THE GATES ACTUALLY CAUGHT. A gate that has never failed is either proving something or proving nothing,
/// and the count is the only way to tell. Pure — tested.
pub fn gates(runs: &[Value]) -> Value {
    let (mut qa_pass, mut qa_fail, mut base_red, mut judged_pass, mut judged_fail) = (0, 0, 0, 0, 0);
    let (mut rejects, mut retries) = (0usize, 0usize);
    for r in runs {
        if let Some(q) = r.get("qa").filter(|v| !v.is_null()) {
            if q.get("passed").and_then(Value::as_bool).unwrap_or(false) { qa_pass += 1 } else { qa_fail += 1 }
        }
        if let Some(b) = r.get("qa_baseline").filter(|v| !v.is_null()) {
            if !b.get("passed").and_then(Value::as_bool).unwrap_or(true) { base_red += 1 }
        }
        if let Some(j) = r.get("qa_review").filter(|v| !v.is_null()) {
            if j.get("passed").and_then(Value::as_bool).unwrap_or(false) { judged_pass += 1 } else { judged_fail += 1 }
        }
        for e in arr(r.get("events")) {
            match s(e.get("kind")).as_str() { "reject" => rejects += 1, "retry" => retries += 1, _ => {} }
        }
    }
    json!({ "tests": { "passed": qa_pass, "failed": qa_fail },
        "baseline_already_red": base_red,
        "judge": { "passed": judged_pass, "failed": judged_fail },
        "verifier": { "rejected": rejects, "retried": retries },
        "note": if qa_pass + qa_fail == 0 { "the test gate has not run on a settled run yet" } else if qa_fail == 0 { "the test gate has never failed — it is either proving something easy or nothing at all" } else { "" } })
}

/// WHEN THE WORK HAPPENS — 24 buckets of local hour. Your own rhythm, from the clock, not a survey. Pure — tested.
pub fn rhythm(times_ms: &[i64], tz_offset_min: i64) -> Vec<u32> {
    let mut hours = vec![0u32; 24];
    for t in times_ms {
        if *t <= 0 { continue }
        let local = t + tz_offset_min * 60_000;
        let h = local.div_euclid(3_600_000).rem_euclid(24) as usize;
        hours[h] += 1;
    }
    hours
}

/// HOW MUCH LOOKING PER CHANGE. A high ratio is an agent hunting; a low one knew where to go. It is the clearest
/// signal the context pack is working, and it is free to count. Pure — tested.
pub fn looking(runs: &[Value]) -> Value {
    let (mut reads, mut writes) = (0usize, 0usize);
    for r in runs {
        for t in arr(r.get("tasks")) {
            for c in arr(t.get("trace").and_then(|x| x.get("tools"))) {
                let name = s(c.get("name")).to_ascii_lowercase();
                if name.contains("read") || name.contains("grep") || name.contains("glob") || name.contains("search") { reads += 1 }
                else if name.contains("write") || name.contains("edit") || name.contains("patch") { writes += 1 }
            }
        }
    }
    json!({ "reads": reads, "writes": writes,
        "ratio": if writes > 0 { reads as f64 / writes as f64 } else { 0.0 },
        "note": if writes == 0 { "no writes recorded yet" } else { "" } })
}

/// The whole picture, and one sentence that says what it means. Pure — tested.
pub fn work_stats(runs: &[Value], times_ms: &[i64], tz_offset_min: i64) -> Value {
    let ft = first_try(runs);
    let g = gates(runs);
    let lk = looking(runs);
    let mut verdict = String::new();
    if n(ft.get("settled")) >= 5.0 {
        verdict.push_str(&format!("{:.0}% of finished tasks landed on the first attempt.", n(ft.get("pct"))));
        if n(ft.get("retried")) > 0.0 { verdict.push_str(&format!(" {} needed a second go.", n(ft.get("retried")) as i64)); }
    } else { verdict.push_str("Not enough finished tasks yet to say how often the work lands first time."); }
    if n(lk.get("writes")) > 0.0 { verdict.push_str(&format!(" {:.1} lookups per change.", n(lk.get("ratio")))); }
    let rej = n(g.get("verifier").and_then(|v| v.get("rejected")));
    if rej > 0.0 { verdict.push_str(&format!(" The verifier sent work back {} time(s) — that is the gate earning its place.", rej as i64)); }
    json!({ "first_try": ft, "gates": g, "looking": lk, "hotspots": hotspots(runs, 8),
        "rhythm": rhythm(times_ms, tz_offset_min), "verdict": verdict })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(status: &str, attempts: i64, owns: &[&str], tools: &[&str]) -> Value {
        json!({ "status": status, "attempts": attempts, "owns": owns, "changed": [],
                "trace": { "tools": tools.iter().map(|t| json!({ "name": t })).collect::<Vec<_>>() } })
    }

    #[test]
    fn first_time_right_is_the_number_nobody_shows() {
        let runs = vec![
            json!({ "engine": "claude-code", "workers": "opencode", "tasks": [
                task("done", 1, &["a.ts"], &[]), task("done", 2, &["b.ts"], &[]), task("done", 1, &["c.ts"], &[]),
                task("failed", 3, &["d.ts"], &[]), task("running", 1, &["e.ts"], &[]) ] }),
        ];
        let ft = first_try(&runs);
        assert_eq!(ft["first"], json!(2));
        assert_eq!(ft["retried"], json!(1), "a task that needed a second go is not a first-time pass");
        assert_eq!(ft["failed"], json!(1));
        assert_eq!(ft["settled"], json!(3), "a task still running is settled by nobody");
        assert!((ft["pct"].as_f64().unwrap() - 66.67).abs() < 0.1);
        // fewer than five finished tasks is not a statistic and says so
        assert!(ft["note"].as_str().unwrap().contains("too few"));
        // the per-engine split needs three tasks before it is shown at all
        assert_eq!(ft["by_engine"].as_array().unwrap().len(), 1);
        assert_eq!(ft["by_engine"][0]["engine"], json!("opencode"));
    }

    #[test]
    fn the_hotspots_are_churn_as_the_agents_felt_it() {
        let runs = vec![
            json!({ "tasks": [task("done", 2, &["src/cart.ts"], &[]), task("done", 1, &["src/new.ts"], &[])] }),
            json!({ "tasks": [task("done", 1, &["src/cart.ts"], &[])] }),
            json!({ "tasks": [task("done", 3, &["src/cart.ts"], &[]), task("done", 1, &["src/other.ts"], &[])] }),
        ];
        let h = hotspots(&runs, 8);
        assert_eq!(h.len(), 1, "a file touched by ONE run is not a hotspot");
        assert_eq!(h[0]["path"], json!("src/cart.ts"));
        assert_eq!(h[0]["runs"], json!(3));
        assert_eq!(h[0]["retries"], json!(2), "and it needed a second go in two of them");
    }

    #[test]
    fn a_gate_that_never_fails_is_reported_as_suspicious_not_as_perfect() {
        let green = vec![json!({ "qa": { "passed": true }, "events": [] }), json!({ "qa": { "passed": true }, "events": [] })];
        let g = gates(&green);
        assert_eq!(g["tests"], json!({ "passed": 2, "failed": 0 }));
        assert!(g["note"].as_str().unwrap().contains("proving something easy or nothing at all"));

        let real = vec![
            json!({ "qa": { "passed": false }, "qa_baseline": { "passed": false }, "qa_review": { "passed": false },
                    "events": [{ "kind": "reject" }, { "kind": "retry" }, { "kind": "pass" }] }),
            json!({ "qa": { "passed": true }, "events": [] }),
        ];
        let g2 = gates(&real);
        assert_eq!(g2["tests"], json!({ "passed": 1, "failed": 1 }));
        assert_eq!(g2["baseline_already_red"], json!(1), "a suite that was red BEFORE the change is its own fact");
        assert_eq!(g2["judge"], json!({ "passed": 0, "failed": 1 }));
        assert_eq!(g2["verifier"], json!({ "rejected": 1, "retried": 1 }));
        assert_eq!(g2["note"], json!(""));
        assert!(gates(&[])["note"].as_str().unwrap().contains("has not run"));
    }

    #[test]
    fn the_rhythm_is_the_operators_own_clock() {
        let h = |hour: i64| hour * 3_600_000;              // 1970-01-01 at that UTC hour
        let r = rhythm(&[h(14), h(14), h(9), 0, -5], 0);
        assert_eq!(r[14], 2); assert_eq!(r[9], 1);
        assert_eq!(r.iter().sum::<u32>(), 3, "a missing timestamp is not midnight");
        // at UTC-3 the same 14:00 UTC call is 11:00 for the operator
        let b = rhythm(&[h(14)], -180);
        assert_eq!(b[11], 1); assert_eq!(b[14], 0);
        // and it wraps rather than panicking
        assert_eq!(rhythm(&[h(1)], -180)[22], 1);
    }

    #[test]
    fn how_much_looking_each_change_took() {
        let runs = vec![json!({ "tasks": [
            task("done", 1, &["a.ts"], &["read", "read", "grep", "edit"]),
            task("done", 1, &["b.ts"], &["Read", "Write"]),
        ] })];
        let l = looking(&runs);
        assert_eq!(l["reads"], json!(4)); assert_eq!(l["writes"], json!(2));
        assert_eq!(l["ratio"], json!(2.0));
        assert!(looking(&[])["note"].as_str().unwrap().contains("no writes"));
    }

    #[test]
    fn the_verdict_says_what_the_numbers_mean_and_refuses_when_they_are_thin() {
        let thin = work_stats(&[json!({ "tasks": [task("done", 1, &["a.ts"], &[])] })], &[], 0);
        assert!(thin["verdict"].as_str().unwrap().starts_with("Not enough finished tasks"));

        let runs: Vec<Value> = vec![json!({ "engine": "claude-code", "tasks": (0..6).map(|i| task("done", if i == 5 { 2 } else { 1 }, &["a.ts"], &["read", "read", "edit"])).collect::<Vec<_>>(),
            "events": [{ "kind": "reject" }] })];
        let w = work_stats(&runs, &[3_600_000 * 14], -180);
        let v = w["verdict"].as_str().unwrap();
        assert!(v.contains("83% of finished tasks landed on the first attempt"), "{v}");
        assert!(v.contains("1 needed a second go"));
        assert!(v.contains("2.0 lookups per change"));
        assert!(v.contains("sent work back 1 time(s)"));
        assert_eq!(w["rhythm"][11], json!(1));
    }
}


// ── THE MERGED TIMELINE, SERVE-SIDE ─────────────────────────────────────────────────────────────
//
// operator 2026-09-24: *"graph tab on the timeline should show the graph from all active agents timeline like it
// was before, is just showing my actions it should show all paralel work like a git graph"* and *"fuel takes a
// lot of time to display the data should be instant"*.
//
// Both had one cause. The browser built this by doing a session-get per session, then an audit read and a goal
// read per pane — around sixty requests every fifteen seconds. When any of them was slow the whole tick produced
// nothing, so the graph fell back to the only frames that never need a request: the operator's own. Assembled
// here it is one call, and it cannot half-arrive.
//
// Attribution matters more than volume. The box's `reads.jsonl` is shared by every agent working in that
// worktree, so a frame taken from it belongs to nobody in particular — which is exactly how nine agents ended up
// looking like one. The per-pane event log (`agent-events-<pane>.jsonl`) and a run's `parent` are attributed, so
// those are what lanes are built from.

/// One node on the graph.
fn frame(t: i64, agent: &str, glyph: &str, op: &str, text: &str, tone: &str) -> Value {
    json!({ "t": t, "agent": agent, "glyph": glyph, "op": op, "text": text.chars().take(200).collect::<String>(), "tone": tone })
}

/// A pane's own turns, from its own log. Pure over the lines — tested.
pub fn frames_of_log<'a>(lines: impl Iterator<Item = &'a str>, agent: &str, limit: usize) -> Vec<Value> {
    let mut out = vec![];
    for line in lines {
        let Ok(w) = serde_json::from_str::<Value>(line) else { continue };
        let t = n(w.get("t")) as i64;
        let ev = w.get("ev").cloned().unwrap_or(Value::Null);
        let ty = s(ev.get("type"));
        match ty.as_str() {
            "antiloki" => {
                let sub = s(ev.get("subtype"));
                let text = s(ev.get("text"));
                match sub.as_str() {
                    "user" => out.push(frame(t, agent, "»", "ask", &s(ev.get("text")), "fg")),
                    "episode" => out.push(frame(t, agent, "↻", "episode", if text.is_empty() { "a new episode" } else { &text }, "ok")),
                    "gates" => out.push(frame(t, agent, "⛒", "gate", &text, if ev.get("ok") == Some(&json!(false)) { "bad" } else { "dim" })),
                    "paused" => out.push(frame(t, agent, "⏸", "paused", &text, "warn")),
                    _ => {}
                }
            }
            "assistant" => {
                for b in arr(ev.get("message").and_then(|m| m.get("content"))) {
                    if s(b.get("type")) == "tool_use" {
                        let name = s(b.get("name"));
                        let input = b.get("input").cloned().unwrap_or(Value::Null);
                        let target = ["file_path", "command", "pattern", "path", "query"].iter()
                            .find_map(|k| input.get(*k).and_then(Value::as_str)).unwrap_or("").to_string();
                        let low = name.to_ascii_lowercase();
                        let (glyph, op, tone) =
                            if low.contains("write") || low.contains("edit") { ("✎", "write", "warn") }
                            else if low.contains("bash") { ("$", "run", "dim") }
                            else if low.contains("grep") || low.contains("glob") { ("⌕", "search", "dim") }
                            else { ("📄", "read", "dim") };
                        out.push(frame(t, agent, glyph, op, &target, tone));
                    }
                }
            }
            "result" => {
                let err = ev.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                out.push(frame(t, agent, if err { "✗" } else { "✓" }, "turn", &s(ev.get("result")), if err { "bad" } else { "ok" }));
            }
            _ => {}
        }
        if out.len() > limit { out.drain(..out.len() - limit); }
    }
    out
}

/// A run's ledger, attributed to the agent that owns it. Pure — tested.
pub fn frames_of_run(run: &Value) -> Vec<Value> {
    let agent = { let p = s(run.get("parent")); if p.is_empty() { "an agent".into() } else { p } };
    arr(run.get("events")).into_iter().filter_map(|e| {
        let kind = s(e.get("kind"));
        let (glyph, tone) = match kind.as_str() {
            "start" => ("◆", "fg"), "pass" => ("✓", "ok"), "reject" => ("✗", "bad"), "retry" => ("↻", "warn"),
            "stage" => ("▸", "dim"), "qa" | "qa-pass" => ("⚖", "ok"), "qa-fail" | "qa-review-fail" => ("⚖", "bad"),
            "done" => ("✓", "ok"), "fail" => ("✗", "bad"), "heimdall" | "route" => ("⑂", "fg"),
            _ => return None,
        };
        Some(frame(n(e.get("t")) as i64, &agent, glyph, &kind, &s(e.get("text")), tone))
    }).collect()
}

/// Everything, newest first. Pure — tested.
pub fn merge_frames(mut all: Vec<Value>, limit: usize) -> Vec<Value> {
    all.sort_by_key(|f| std::cmp::Reverse(n(f.get("t")) as i64));
    all.truncate(limit);
    all
}

#[cfg(test)]
mod frame_tests {
    use super::*;

    #[test]
    fn a_panes_own_log_becomes_its_own_lane() {
        let line = |t: i64, ev: Value| serde_json::to_string(&json!({ "seq": 1, "t": t, "ev": ev })).unwrap();
        let log = [
            line(10, json!({ "type": "antiloki", "subtype": "user", "text": "add a product" })),
            line(20, json!({ "type": "assistant", "message": { "content": [
                { "type": "tool_use", "name": "Read", "input": { "file_path": "src/products.ts" } },
                { "type": "tool_use", "name": "Edit", "input": { "file_path": "src/products.ts" } }] } })),
            line(30, json!({ "type": "antiloki", "subtype": "gates", "ok": false, "text": "build: FAILED" })),
            line(40, json!({ "type": "result", "is_error": false, "result": "done" })),
            "not json at all".to_string(),
        ].join("\n");
        let f = frames_of_log(log.lines(), "daniboy", 100);
        assert_eq!(f.len(), 5, "a corrupt line is skipped, not fatal");
        assert!(f.iter().all(|x| x["agent"] == json!("daniboy")), "every frame belongs to the pane it came from");
        assert_eq!((f[0]["op"].as_str().unwrap(), f[0]["glyph"].as_str().unwrap()), ("ask", "»"));
        assert_eq!((f[1]["op"].as_str().unwrap(), f[1]["text"].as_str().unwrap()), ("read", "src/products.ts"));
        assert_eq!((f[2]["op"].as_str().unwrap(), f[2]["tone"].as_str().unwrap()), ("write", "warn"));
        assert_eq!((f[3]["op"].as_str().unwrap(), f[3]["tone"].as_str().unwrap()), ("gate", "bad"));
        assert_eq!(f[4]["op"], json!("turn"));
    }

    #[test]
    fn a_runs_ledger_lands_on_the_agent_that_owns_it_and_drops_the_noise() {
        let run = json!({ "parent": "backender", "events": [
            { "t": 1, "kind": "start", "text": "wave 1" },
            { "t": 2, "kind": "reject", "text": "missing JSDoc" },
            { "t": 3, "kind": "pass", "text": "compiles" },
            { "t": 4, "kind": "report", "text": "1858 chars" },
        ] });
        let f = frames_of_run(&run);
        assert_eq!(f.len(), 3, "a bookkeeping line is not a node on the graph");
        assert!(f.iter().all(|x| x["agent"] == json!("backender")));
        assert_eq!(f[1]["tone"], json!("bad"));
    }

    #[test]
    fn everything_merges_newest_first_and_several_agents_keep_their_own_lanes() {
        let a = frames_of_log(r#"{"t":100,"ev":{"type":"result","result":"a"}}"#.lines(), "alice", 10);
        let b = frames_of_log(r#"{"t":200,"ev":{"type":"result","result":"b"}}"#.lines(), "bob", 10);
        let all = merge_frames([a, b].concat(), 10);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0]["agent"], json!("bob"), "newest first");
        assert_eq!(all[1]["agent"], json!("alice"), "and alice keeps her own lane rather than being folded in");
        assert_eq!(merge_frames(vec![], 10).len(), 0);
    }
}
