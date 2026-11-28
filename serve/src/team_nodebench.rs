//! A BENCHMARK PER NODE (RFC 0010 §4.20 A; operator 2026-11-13: "a benchmark per node is important for the self-improving gate"). When a run ends, one row per node that took part is appended to `<team>/node-records.jsonl`, taken from what
//! the run already recorded (its trace, its values, its costs): nothing new is measured during the run and no model is called. From the rows this module computes, per node and per version of that node (`cfg`, a hash of its spec), how often it
//! succeeds, how often it needs a retry or a stronger seat, what it costs and how long it takes, where it stands in its team, whether the Leader's last change to it helped, and the findings that make the audit tell the Leader about a node that is
//! slipping EVEN WHEN THE RUN WAS GOOD. Every figure carries its `n`; under five rows it says "too few runs" and gives no verdict. Pure: snapshots and rows in, rows and findings out.
use crate::team_audit::Finding;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const MAX_ROWS: usize = 2000;
pub const WINDOW: usize = 50;
pub const MIN_N: usize = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeRecord {
    pub run: String, pub t_ms: i64, pub team: String, pub graph: String, pub node: String, pub kind: String, pub title: String,
    /** a hash of the node's own spec (not where it is drawn, not its label): a change of seat, tier, brief, cap, rules or prompt changes it */ pub cfg: String,
    pub seat: String, pub tries: u32, pub ok: bool, /** ok | retried | failed | capped | check_failed | declined | escalated */ pub outcome: String,
    pub cost_usd: f64, pub ms: i64, pub escalated: bool, #[serde(default)] pub detail: Value,
}

fn s(v: &Value) -> String { v.as_str().unwrap_or("").to_string() }
/// The version of a node: its spec without its position and its label. Pure.
pub fn cfg_of(node: &Value) -> String {
    use sha2::{Digest, Sha256}; let mut v = node.clone(); if let Some(o) = v.as_object_mut() { o.remove("pos"); o.remove("label"); }
    hex::encode(Sha256::digest(serde_json::to_string(&v).unwrap_or_default().as_bytes()))[..8].to_string()
}
fn kind_of(n: &Value) -> String { match n["node"].as_str().unwrap_or("") { "role" => n["role"].as_str().unwrap_or("role").to_string(), k => k.to_string() } }
fn seat_of(n: &Value, run_engine: &str) -> String {
    match n["node"].as_str().unwrap_or("") {
        "llm" => s(&n["seat"]),
        "role" => { let e = if run_engine.is_empty() { "claude-code" } else { run_engine }; match (n["model"].as_str().filter(|m| !m.is_empty()), n["tier"].as_str()) { (Some(m), _) => format!("{e}:{m}"), (None, Some(t)) => format!("{e}:{t}"), _ => e.to_string() } }
        "check" | "artifact" | "human" | "loop" => "none".into(),
        _ => "none".into(),
    }
}
const RECORDED: [&str; 11] = ["planner", "worker", "qa", "debugger", "audit", "agent", "check", "llm", "human", "artifact", "loop"];

/// One row per node that took part in this run. `snapshot` is the run's `state.json`. Pure.
pub fn records_of(snapshot: &Value, graph_hash: &str) -> Vec<NodeRecord> {
    let g = &snapshot["graph"]["nodes"]; let st = &snapshot["state"]; let trace = st["trace"].as_array().cloned().unwrap_or_default(); let values = st["values"].as_object().cloned().unwrap_or_default();
    let costs = st["node_cost"].as_object().cloned().unwrap_or_default(); let (run, team, engine) = (s(&snapshot["id"]), s(&snapshot["team"]), s(&snapshot["engine"]));
    let t_end = trace.iter().filter_map(|t| t["t_ms"].as_i64()).max().unwrap_or(0);
    let mut out = vec![];
    for (id, n) in g.as_object().into_iter().flatten() {
        let kind = kind_of(n); if !RECORDED.contains(&kind.as_str()) { continue; }
        let mine: Vec<&Value> = trace.iter().filter(|t| t["node"] == json!(id)).collect(); let starts = mine.iter().filter(|t| t["kind"] == "start").count() as u32; let failed = mine.iter().filter(|t| t["kind"] == "failed").count() as u32;
        let capped = mine.iter().any(|t| t["kind"] == "cap"); let v = values.get(id);
        if starts == 0 && v.is_none() && failed == 0 { continue; }   // it took no part in this run
        let first = mine.iter().filter(|t| t["kind"] == "start").filter_map(|t| t["t_ms"].as_i64()).min(); let last = mine.iter().filter(|t| t["kind"] == "done" || t["kind"] == "failed").filter_map(|t| t["t_ms"].as_i64()).max().or(if v.is_some() { Some(t_end) } else { None });
        let ms = match (first, last) { (Some(a), Some(b)) if b >= a => b - a, _ => 0 }; let tries = starts.max(1);
        let (mut ok, mut outcome, mut escalated) = (v.is_some(), "ok".to_string(), false); let mut detail = json!({});
        match kind.as_str() {
            "check" => { let pass = v.map_or(false, |x| x["pass"] == json!(true)); ok = pass; if !pass { outcome = "check_failed".into(); } detail = json!({ "failed_rules": v.map(|x| x["failed"].as_array().map_or(0, |a| a.len())).unwrap_or(0) }); }
            "llm" => { escalated = v.map_or(false, |x| x["escalated_from"].as_str().map_or(false, |e| !e.is_empty())); ok = v.map_or(false, |x| x["ok"] == json!(true)); if escalated { outcome = "escalated".into(); } detail = json!({ "calls": v.and_then(|x| x["calls"].as_u64()).unwrap_or(0) }); }
            "human" => { let approved = v.map_or(false, |x| x["approved"] == json!(true)); ok = approved; if !approved { outcome = "declined".into(); } }
            "loop" => { let (count, max) = (v.and_then(|x| x["count"].as_u64()).unwrap_or(0), n["max_iterations"].as_u64().unwrap_or(0)); ok = max == 0 || count < max; if !ok { outcome = "exhausted".into(); } detail = json!({ "loop_rounds": count }); }
            _ => { if !ok { outcome = if failed > 0 { "failed".into() } else { "unfinished".into() }; } else if starts > 1 || failed > 0 { outcome = "retried".into(); } }
        }
        if capped && outcome == "ok" { outcome = "capped".into(); }
        out.push(NodeRecord { run: run.clone(), t_ms: t_end, team: team.clone(), graph: graph_hash.into(), node: id.clone(), kind, title: n["label"].as_str().map(String::from).or_else(|| n["agent"]["title"].as_str().map(String::from)).unwrap_or_else(|| id.clone()), cfg: cfg_of(n), seat: seat_of(n, &engine), tries, ok, outcome,
            cost_usd: costs.get(id).and_then(Value::as_f64).unwrap_or(0.0), ms, escalated, detail });
    }
    out
}

// ── the figures ───────────────────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Stats { pub n: usize, pub ok_rate: f64, pub retry_rate: f64, pub climb_rate: f64, pub cost_median: f64, pub cost_mean: f64, pub ms_median: i64, pub last_t: i64 }
fn median(mut v: Vec<f64>) -> f64 { if v.is_empty() { return 0.0; } v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)); let m = v.len() / 2; if v.len() % 2 == 1 { v[m] } else { (v[m - 1] + v[m]) / 2.0 } }
fn r3(x: f64) -> f64 { (x * 1000.0).round() / 1000.0 }
/// The figures of a set of rows (newest last), over the last WINDOW. Pure.
pub fn stats(rows: &[&NodeRecord]) -> Stats {
    let w: Vec<&&NodeRecord> = rows.iter().rev().take(WINDOW).collect(); let n = w.len(); let f = |p: &dyn Fn(&NodeRecord) -> bool| if n == 0 { 0.0 } else { w.iter().filter(|r| p(r)).count() as f64 / n as f64 };
    Stats { n, ok_rate: r3(f(&|r| r.ok)), retry_rate: r3(f(&|r| r.tries > 1 || r.outcome == "retried")), climb_rate: r3(f(&|r| r.escalated)), cost_median: r3(median(w.iter().map(|r| r.cost_usd).collect())), cost_mean: r3(if n == 0 { 0.0 } else { w.iter().map(|r| r.cost_usd).sum::<f64>() / n as f64 }),
        ms_median: median(w.iter().map(|r| r.ms as f64).collect()) as i64, last_t: w.iter().map(|r| r.t_ms).max().unwrap_or(0) }
}
fn verdict(old: &Stats, new: &Stats) -> &'static str {
    if old.n < MIN_N || new.n < MIN_N { return "too few runs"; }
    let d = new.ok_rate - old.ok_rate; let dc = if old.cost_median > 0.0 { (new.cost_median - old.cost_median) / old.cost_median } else { 0.0 };
    if d >= 0.1 || (d > -0.05 && dc <= -0.25) { "better" } else if d <= -0.1 || (d > -0.05 && dc >= 0.25) { "worse" } else { "about the same" }
}
/// The whole table: per node, its versions and where it stands. Pure over the rows (oldest first) of one team.
pub fn table(rows: &[NodeRecord]) -> Value {
    let mut by_node: BTreeMap<&str, Vec<&NodeRecord>> = BTreeMap::new(); for r in rows { by_node.entry(r.node.as_str()).or_default().push(r); }
    // each node's latest version
    let mut latest: BTreeMap<&str, (String, Stats)> = BTreeMap::new(); let mut versions: BTreeMap<&str, Vec<(String, Stats, String)>> = BTreeMap::new();
    for (node, rs) in &by_node {
        let mut cfgs: Vec<String> = vec![]; for r in rs.iter() { if !cfgs.contains(&r.cfg) { cfgs.push(r.cfg.clone()); } }
        let mut vs: Vec<(String, Stats, String)> = cfgs.iter().map(|c| { let sub: Vec<&NodeRecord> = rs.iter().filter(|r| &r.cfg == c).copied().collect(); (c.clone(), stats(&sub), sub.last().map(|r| r.seat.clone()).unwrap_or_default()) }).collect();
        vs.sort_by_key(|v| v.1.last_t); if let Some(l) = vs.last() { latest.insert(node, (l.0.clone(), l.1.clone())); } versions.insert(node, vs);
    }
    let total_cost: f64 = latest.values().map(|l| l.1.cost_median).sum();
    let mut cost_order: Vec<(&str, f64)> = latest.iter().map(|(n, l)| (*n, l.1.cost_median)).collect(); cost_order.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut rel_order: Vec<(&str, f64, f64)> = latest.iter().map(|(n, l)| (*n, l.1.ok_rate, l.1.retry_rate)).collect(); rel_order.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal)));
    let mut out = vec![];
    for (node, rs) in &by_node {
        let (cfg, st) = latest[node].clone(); let vs = &versions[node]; let first = rs[rs.len() - 1];
        let prev = if vs.len() >= 2 { Some(&vs[vs.len() - 2]) } else { None };
        let vs_prev = prev.map(|p| json!({ "old_cfg": p.0, "old_seat": p.2, "old": p.1, "new": st, "verdict": verdict(&p.1, &st), "reading": format!("changed since the version run {} time(s): {}", p.1.n, verdict(&p.1, &st)) }));
        out.push(json!({ "node": node, "kind": first.kind, "title": first.title, "cfg": cfg, "seat": rs.iter().rev().find(|r| r.cfg == cfg).map(|r| r.seat.clone()).unwrap_or_default(), "stats": st, "enough_runs": st.n >= MIN_N,
            "cost_share": if total_cost > 0.0 { r3(st.cost_median / total_cost) } else { 0.0 }, "cost_rank": cost_order.iter().position(|c| c.0 == *node).map(|p| p + 1), "reliability_rank": rel_order.iter().position(|c| c.0 == *node).map(|p| p + 1),
            "cost_per_ok": if st.ok_rate > 0.0 { json!(r3(st.cost_median / st.ok_rate)) } else { Value::Null }, "vs_previous_cfg": vs_prev, "versions": vs.iter().map(|v| json!({ "cfg": v.0, "seat": v.2, "stats": v.1 })).collect::<Vec<_>>() }));
    }
    out.sort_by(|a, b| a["stats"]["ok_rate"].as_f64().partial_cmp(&b["stats"]["ok_rate"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
    json!({ "nodes": out, "rows": rows.len(), "reading": "weakest first; every figure has its n; under 5 runs it is 'too few runs', not a verdict" })
}

// ── the gate: what makes the audit tell the Leader about a node even when the run was good ──────────────────────────────────────

fn f(code: &str, sev: &str, node: &str, text: String, suggestion: &str) -> Finding { Finding { code: code.into(), severity: sev.into(), node: Some(node.into()), text, suggestion: suggestion.into() } }
/// The findings about nodes, from every row of the team including the run just ended. `(findings, attention)`: attention is true when one of them asks for the Leader's eyes. Pure.
pub fn findings(rows: &[NodeRecord]) -> (Vec<Finding>, bool) {
    let mut out = vec![]; let mut attention = false;
    let mut by_node: BTreeMap<&str, Vec<&NodeRecord>> = BTreeMap::new(); for r in rows { by_node.entry(r.node.as_str()).or_default().push(r); }
    for (node, rs) in &by_node {
        let latest_cfg = &rs[rs.len() - 1].cfg; let same: Vec<&NodeRecord> = rs.iter().filter(|r| &r.cfg == latest_cfg).copied().collect(); let st = stats(&same); let title = &rs[rs.len() - 1].title;
        if st.n >= MIN_N && st.ok_rate < 0.7 { attention = true; out.push(f("node_unreliable", "warn", node, format!("{title} succeeded in {:.0}% of its last {} runs", st.ok_rate * 100.0, st.n), "read why it fails in its transcripts; a stronger seat, a sharper brief, a split of its job, a check in front of it, or a failure edge to a fallback")); }
        if same.len() >= 2 * MIN_N { let (older, newer) = (&same[same.len() - 2 * MIN_N..same.len() - MIN_N], &same[same.len() - MIN_N..]); let (a, b) = (stats(older), stats(newer)); if a.ok_rate - b.ok_rate >= 0.2 { attention = true; out.push(f("node_regressed", "warn", node, format!("{title} fell from {:.0}% to {:.0}% success with nothing changed in it", a.ok_rate * 100.0, b.ok_rate * 100.0), "something around it moved (a model update, a changed site or tool, drifting input): look at its latest failures")); } }
        if rs[rs.len() - 1].kind == "llm" { let last: Vec<&NodeRecord> = same.iter().rev().take(10).copied().collect(); if last.len() >= MIN_N { let c = last.iter().filter(|r| r.escalated).count() as f64 / last.len() as f64; if c > 0.4 { attention = true; out.push(f("node_climbing", "warn", node, format!("{title} climbed to a stronger seat in {:.0}% of its last {} runs", c * 100.0, last.len()), "the small seat is not earning its place: trial it with traps (antiloki_team_seats trial), start higher on the ladder, or give it a better safeguard")); } } }
        // what the last change did (always shown)
        let mut cfgs: Vec<&String> = vec![]; for r in rs.iter() { if !cfgs.contains(&&r.cfg) { cfgs.push(&r.cfg); } }
        if cfgs.len() >= 2 { let (o, n) = (cfgs[cfgs.len() - 2], cfgs[cfgs.len() - 1]); let so = stats(&rs.iter().filter(|r| &r.cfg == o).copied().collect::<Vec<_>>()); let sn = stats(&rs.iter().filter(|r| &r.cfg == n).copied().collect::<Vec<_>>()); let v = verdict(&so, &sn);
            let code = match v { "better" => "change_helped", "worse" => "change_hurt", _ => "change_unclear" }; let sev = if v == "worse" { "warn" } else { "info" }; if v == "worse" { attention = true; }
            out.push(f(code, sev, node, format!("{title} was changed: {v} (success {:.0}% over {} run(s) before, {:.0}% over {} since; median cost ${:.3} then ${:.3})", so.ok_rate * 100.0, so.n, sn.ok_rate * 100.0, sn.n, so.cost_median, sn.cost_median), if v == "worse" { "revert it or try the other arm in a campaign" } else { "keep what helped" })); }
    }
    // one node taking most of the money, run after run
    let mut runs: Vec<&str> = vec![]; for r in rows { if !runs.contains(&r.run.as_str()) { runs.push(r.run.as_str()); } }
    if by_node.len() >= 3 && runs.len() >= 5 { let last5 = &runs[runs.len() - 5..]; let mut dom: BTreeMap<&str, usize> = BTreeMap::new();
        for run in last5 { let rr: Vec<&NodeRecord> = rows.iter().filter(|r| &r.run.as_str() == run).collect(); let total: f64 = rr.iter().map(|r| r.cost_usd).sum(); if total <= 0.0 { continue; } if let Some(top) = rr.iter().max_by(|a, b| a.cost_usd.partial_cmp(&b.cost_usd).unwrap_or(std::cmp::Ordering::Equal)) { if top.cost_usd / total > 0.5 { *dom.entry(top.node.as_str()).or_default() += 1; } } }
        for (node, c) in dom { if c == 5 { let title = rows.iter().rev().find(|r| r.node == node).map(|r| r.title.clone()).unwrap_or_default(); attention = true; out.push(f("node_dominates_cost", "warn", node, format!("{title} took over half of the run's cost in each of the last 5 runs"), "the most expensive node is the first to question: a cheaper seat with guards, a check instead of a reviewer, or a smaller job")); } } }
    (out, attention)
}
/// Rows kept: the newest MAX_ROWS. Pure.
pub fn trim(mut rows: Vec<NodeRecord>) -> Vec<NodeRecord> { if rows.len() > MAX_ROWS { rows.drain(..rows.len() - MAX_ROWS); } rows }

#[cfg(test)]
mod tests {
    use super::*;
    fn rec(run: &str, node: &str, kind: &str, cfg: &str, ok: bool, tries: u32, cost: f64, t: i64, esc: bool) -> NodeRecord { NodeRecord { run: run.into(), t_ms: t, team: "t".into(), graph: "g".into(), node: node.into(), kind: kind.into(), title: node.into(), cfg: cfg.into(), seat: "claude-code:low".into(), tries, ok, outcome: if ok { "ok".into() } else { "failed".into() }, cost_usd: cost, ms: 1000, escalated: esc, detail: json!({}) } }
    fn snap() -> Value { json!({ "id": "r1", "team": "t", "engine": "claude-code", "graph": { "nodes": {
        "w": { "node": "role", "role": "agent", "tier": "low", "label": "Writer", "pos": [1, 2], "agent": { "title": "the Writer" } }, "chk": { "node": "check", "on": "artifact:a.md", "rules": [] }, "k": { "node": "llm", "seat": "endpoint:lfm3" },
        "ghost": { "node": "role", "role": "agent" }, "f": { "node": "fork" }, "rl": { "node": "loop", "max_iterations": 3 } } },
        "state": { "values": { "w": { "report": "x" }, "chk": { "pass": false, "failed": [{ "message": "m" }] }, "k": { "ok": true, "escalated_from": "endpoint:lfm3", "calls": 3 }, "rl": { "count": 3 } }, "node_cost": { "w": 0.02, "k": 0.01 },
                   "trace": [ { "t_ms": 1000, "node": "w", "kind": "start" }, { "t_ms": 2000, "node": "w", "kind": "failed" }, { "t_ms": 2100, "node": "w", "kind": "start" }, { "t_ms": 5000, "node": "w", "kind": "done" }, { "t_ms": 5100, "node": "chk", "kind": "start" }, { "t_ms": 5200, "node": "chk", "kind": "done" }, { "t_ms": 5300, "node": "k", "kind": "start" }, { "t_ms": 6000, "node": "k", "kind": "done" } ] } }) }

    #[test]
    fn a_finished_run_gives_one_row_per_node_that_took_part_with_the_rule_of_its_kind() {
        let rows = records_of(&snap(), "gh"); let by = |n: &str| rows.iter().find(|r| r.node == n).cloned().unwrap();
        assert!(rows.iter().all(|r| r.node != "ghost" && r.node != "f"), "a node that did not run, and a fork, are not recorded"); assert_eq!(rows.len(), 4);
        let w = by("w"); assert!(w.ok && w.outcome == "retried" && w.tries == 2 && w.ms == 4000 && (w.cost_usd - 0.02).abs() < 1e-9 && w.seat == "claude-code:low" && w.title == "Writer", "{w:?}");
        let c = by("chk"); assert!(!c.ok && c.outcome == "check_failed" && c.detail["failed_rules"] == 1); let k = by("k"); assert!(k.ok && k.escalated && k.outcome == "escalated" && k.seat == "endpoint:lfm3"); let l = by("rl"); assert!(!l.ok && l.outcome == "exhausted");
    }
    #[test]
    fn the_version_of_a_node_changes_with_its_spec_and_not_with_where_it_is_drawn_or_what_it_is_called() {
        let a = json!({ "node": "role", "role": "agent", "tier": "low", "pos": [1, 1], "label": "A" }); let b = json!({ "node": "role", "role": "agent", "tier": "low", "pos": [9, 9], "label": "B" }); let c = json!({ "node": "role", "role": "agent", "tier": "high", "pos": [1, 1], "label": "A" });
        assert_eq!(cfg_of(&a), cfg_of(&b)); assert_ne!(cfg_of(&a), cfg_of(&c));
    }
    #[test]
    fn the_figures_are_over_the_window_and_say_too_few_runs_under_five() {
        let rows: Vec<NodeRecord> = (0..8).map(|i| rec(&format!("r{i}"), "w", "agent", "c1", i % 4 != 0, if i % 4 == 0 { 2 } else { 1 }, 0.02, i, false)).collect(); let refs: Vec<&NodeRecord> = rows.iter().collect(); let st = stats(&refs); assert_eq!((st.n, st.ok_rate, st.retry_rate), (8, 0.75, 0.25)); assert_eq!(st.cost_median, 0.02);
        let t = table(&rows); assert_eq!(t["nodes"][0]["enough_runs"], true); assert!(t["nodes"][0]["vs_previous_cfg"].is_null());
        let few = table(&rows[..3]); assert_eq!(few["nodes"][0]["enough_runs"], false);
    }
    #[test]
    fn the_table_ranks_nodes_and_compares_a_changed_node_with_its_older_version_when_both_have_enough_runs() {
        let mut rows = vec![]; for i in 0..6 { rows.push(rec(&format!("a{i}"), "w", "agent", "old", i >= 3, 1, 0.05, i, false)); rows.push(rec(&format!("a{i}"), "chk", "check", "k1", true, 1, 0.0, i, false)); }
        for i in 0..6 { rows.push(rec(&format!("b{i}"), "w", "agent", "new", true, 1, 0.03, 100 + i, false)); rows.push(rec(&format!("b{i}"), "chk", "check", "k1", true, 1, 0.0, 100 + i, false)); }
        let t = table(&rows); let w = t["nodes"].as_array().unwrap().iter().find(|n| n["node"] == "w").unwrap(); assert_eq!(w["cfg"], "new"); assert_eq!(w["vs_previous_cfg"]["verdict"], "better"); assert_eq!(w["cost_rank"], 1); assert_eq!(w["cost_share"], 1.0); assert_eq!(w["versions"].as_array().unwrap().len(), 2);
        let c = t["nodes"].as_array().unwrap().iter().find(|n| n["node"] == "chk").unwrap(); assert_eq!(c["cost_per_ok"], 0.0); assert!(c["vs_previous_cfg"].is_null());
        let few: Vec<NodeRecord> = vec![rec("a", "w", "agent", "old", false, 1, 0.1, 1, false), rec("b", "w", "agent", "new", true, 1, 0.1, 2, false)]; assert_eq!(table(&few)["nodes"][0]["vs_previous_cfg"]["verdict"], "too few runs");
    }
    #[test]
    fn the_gate_tells_the_leader_about_a_slipping_node_at_its_boundaries_and_stays_quiet_otherwise() {
        let mk = |n: usize, oks: usize| -> Vec<NodeRecord> { (0..n).map(|i| rec(&format!("r{i}"), "w", "agent", "c", i < oks, 1, 0.01, i as i64, false)).collect() };
        let (f1, a1) = findings(&mk(5, 3)); assert!(a1 && f1.iter().any(|x| x.code == "node_unreliable"), "3 of 5 is 60%"); let (f2, a2) = findings(&mk(5, 4)); assert!(!a2 && f2.is_empty(), "80% is fine"); let (_, a3) = findings(&mk(4, 0)); assert!(!a3, "four runs are too few");
        let seven: Vec<NodeRecord> = (0..10).map(|i| rec(&format!("r{i}"), "w", "agent", "c", ![1, 4, 6].contains(&i), 1, 0.01, i as i64, false)).collect(); assert!(!findings(&seven).1, "exactly 70% is not under 70%, and it did not fall");
        // regressed with nothing changed: five good, then five with two good (100% -> 40%)
        let r: Vec<NodeRecord> = (0..10).map(|i| rec(&format!("r{i}"), "w", "agent", "c", i < 5 || i == 5 || i == 6, 1, 0.01, i as i64, false)).collect(); let (f3, a3) = findings(&r); assert!(a3 && f3.iter().any(|x| x.code == "node_regressed"), "{:?}", f3.iter().map(|x| &x.code).collect::<Vec<_>>());
        // a small seat that climbs in most runs
        let l: Vec<NodeRecord> = (0..10).map(|i| rec(&format!("r{i}"), "k", "llm", "c", true, 1, 0.0, i, i < 5)).collect(); let (f4, a4) = findings(&l); assert!(a4 && f4.iter().any(|x| x.code == "node_climbing")); let l2: Vec<NodeRecord> = (0..10).map(|i| rec(&format!("r{i}"), "k", "llm", "c", true, 1, 0.0, i, i < 4)).collect(); assert!(!findings(&l2).1, "exactly 40% is not over 40%");
        // one node taking the money in five runs in a row, with at least three nodes
        let mut d = vec![]; for i in 0..5 { let run = format!("r{i}"); d.push(rec(&run, "big", "agent", "c", true, 1, 0.9, i, false)); d.push(rec(&run, "s1", "agent", "c", true, 1, 0.05, i, false)); d.push(rec(&run, "s2", "check", "c", true, 1, 0.0, i, false)); } let (f5, a5) = findings(&d); assert!(a5 && f5.iter().any(|x| x.code == "node_dominates_cost" && x.node.as_deref() == Some("big")));
    }
    #[test]
    fn a_change_that_helped_or_hurt_is_always_said_and_a_hurt_one_asks_for_the_leaders_eyes() {
        let mut up = vec![]; for i in 0..6 { up.push(rec(&format!("a{i}"), "w", "agent", "old", false, 1, 0.05, i, false)); } for i in 0..6 { up.push(rec(&format!("b{i}"), "w", "agent", "new", true, 1, 0.05, 100 + i, false)); }
        let (f1, a1) = findings(&up); let h = f1.iter().find(|x| x.code == "change_helped").unwrap(); assert!(h.severity == "info" && h.text.contains("better") && !a1, "a change that helped is shown, not a reason to interrupt");
        let mut down = vec![]; for i in 0..6 { down.push(rec(&format!("a{i}"), "w", "agent", "old", true, 1, 0.05, i, false)); } for i in 0..6 { down.push(rec(&format!("b{i}"), "w", "agent", "new", i == 0, 1, 0.05, 100 + i, false)); }
        let (f2, a2) = findings(&down); assert!(f2.iter().any(|x| x.code == "change_hurt" && x.severity == "warn") && a2);
        let few = vec![rec("a", "w", "agent", "old", true, 1, 0.1, 1, false), rec("b", "w", "agent", "new", false, 1, 0.1, 2, false)]; assert!(findings(&few).0.iter().any(|x| x.code == "change_unclear"), "too few runs to say");
        assert_eq!(trim((0..2100).map(|i| rec("r", "w", "agent", "c", true, 1, 0.0, i, false)).collect()).len(), MAX_ROWS);
    }
}
