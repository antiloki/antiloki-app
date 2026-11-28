//! THE IMPROVER (RFC 0011 §18): a read-only proposer, never the Leader. The free audit and the benchmark MEASURE every run; the paid Improver wakes only on a trigger
//! (a gate finding, N finished runs, the person marks a result bad, the person asks), under a cooldown, and PROPOSES structural changes, one hypothesis each, with the
//! rows it cites. It applies NOTHING: the Leader (or the person) decides through `decide`, and a patch that touches a gate, a budget, a human gate, triggers, limits or
//! setpoints is the person's. Per team: a mode (off, suggest, auto-apply for low-risk kinds), a freeze flag, a record of how often its proposals helped.
use crate::team_bench::{Case, Expect, Stats, TrialReport, MIN_N};
use crate::team_graph::{Edge, Graph};
use crate::team_patch::{self, CheckpointHooks, PatchOp};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The findings of the free audit that wake the Improver (RFC §18).
pub const TRIGGER_CODES: [&str; 4] = ["node_unreliable", "node_regressed", "node_dominates_cost", "node_climbing"];
/// Kinds of change that auto-apply may take, never a gate, a budget, a trigger, a limit or a setpoint.
pub const LOW_RISK_KINDS: [&str; 2] = ["seat_cheaper_with_trial", "prompt_wording"];
const WEEK_S: i64 = 7 * 24 * 3600;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode { Off, Suggest, AutoLowRisk }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// no file = off: the paid Improver never wakes on its own unless the person turned it on (a person's ask always works)
    #[serde(default = "mode_off")] pub mode: Mode,
    /// no proposals at all while frozen (the whole team, or the listed nodes)
    #[serde(default)] pub freeze: bool, #[serde(default)] pub frozen_nodes: Vec<String>,
    #[serde(default = "d_cooldown")] pub cooldown_s: i64,
    #[serde(default = "d_every")] pub every_n_runs: u32,
    /// the seat that proposes (Sonnet or below; an endpoint seat works too)
    #[serde(default = "d_seat")] pub seat: String,
    #[serde(default = "d_budget")] pub budget_usd: f64,
    #[serde(default = "d_auto")] pub max_auto_per_cycle: u32,
}
fn mode_off() -> Mode { Mode::Off } fn d_cooldown() -> i64 { 6 * 3600 } fn d_every() -> u32 { 10 } fn d_seat() -> String { "claude-code:sonnet".into() } fn d_budget() -> f64 { 0.5 } fn d_auto() -> u32 { 1 }
impl Default for Config { fn default() -> Config { serde_json::from_value(json!({})).unwrap() } }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Change { pub proposal: i64, pub at_ms: i64, /** the node's ok rate before the change */ pub before_ok_rate: f64, /** calls of the node when it was made: the new version is judged after MIN_N more */ pub calls_at_change: u32, pub judged: Option<String> }

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub last_review_ms: i64, pub runs_since_review: u32,
    /// nodes that already took an accepted change in this cycle (since the last review)
    pub cycle_nodes: Vec<String>, pub auto_this_cycle: u32,
    pub proposed: u32, pub accepted: u32, pub helped: u32, pub hurt: u32,
    pub changes: BTreeMap<String, Change>,
}
impl State { pub fn hit_rate(&self) -> f64 { if self.proposed == 0 { 0.0 } else { self.helped as f64 / self.proposed as f64 } } }

fn dir(root: &str, team: &str) -> PathBuf { crate::team_graph_run::team_files(root, team) }
pub fn load_config(root: &str, team: &str) -> Config { std::fs::read_to_string(dir(root, team).join("improver.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default() }
pub fn save_config(root: &str, team: &str, c: &Config) -> Result<(), String> { let d = dir(root, team); std::fs::create_dir_all(&d).map_err(|e| e.to_string())?; std::fs::write(d.join("improver.json"), serde_json::to_string_pretty(c).map_err(|e| e.to_string())?).map_err(|e| e.to_string()) }
pub fn load_state(root: &str, team: &str) -> State { std::fs::read_to_string(dir(root, team).join("improver-state.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default() }
pub fn save_state(root: &str, team: &str, s: &State) -> Result<(), String> { let d = dir(root, team); std::fs::create_dir_all(&d).map_err(|e| e.to_string())?; std::fs::write(d.join("improver-state.json"), serde_json::to_string_pretty(s).map_err(|e| e.to_string())?).map_err(|e| e.to_string()) }

// ── when it wakes ─────────────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum Reason { GateFinding(String), EveryNRuns, MarkedBad, Asked }
impl Reason { pub fn label(&self) -> String { match self { Reason::GateFinding(c) => format!("gate finding {c}"), Reason::EveryNRuns => "N runs since the last review".into(), Reason::MarkedBad => "the person marked a result bad".into(), Reason::Asked => "the person asked".into() } } }

/// May a review start now for this reason? Off, frozen, or inside the cooldown: no (the person's own ask only skips the cooldown).
pub fn should_review(cfg: &Config, st: &State, why: &Reason, now_ms: i64) -> Result<(), String> {
    if cfg.freeze && *why != Reason::Asked { return Err("the team is frozen".into()); }
    if cfg.mode == Mode::Off && *why != Reason::Asked { return Err("the improver is off for this team".into()); }
    let since = (now_ms - st.last_review_ms) / 1000;
    if st.last_review_ms > 0 && *why != Reason::Asked && since < cfg.cooldown_s { return Err(format!("cooldown: the last review was {since} s ago, the wait is {} s", cfg.cooldown_s)); }
    match why {
        Reason::EveryNRuns => { if st.runs_since_review < cfg.every_n_runs { return Err(format!("{} of {} runs", st.runs_since_review, cfg.every_n_runs)); } if st.last_review_ms > 0 && since < WEEK_S { return Err("never more than weekly on a run count".into()); } Ok(()) }
        Reason::GateFinding(c) => if TRIGGER_CODES.contains(&c.as_str()) { Ok(()) } else { Err(format!("`{c}` is not a finding that wakes the improver")) },
        Reason::MarkedBad | Reason::Asked => Ok(()),
    }
}

/// After a run: which reason (if any) wakes the improver? Not every run: a finding of the free audit, or the Nth run.
pub fn triggers_of(cfg: &Config, st: &State, finding_codes: &[String], now_ms: i64) -> Option<Reason> {
    for c in finding_codes { let r = Reason::GateFinding(c.clone()); if TRIGGER_CODES.contains(&c.as_str()) && should_review(cfg, st, &r, now_ms).is_ok() { return Some(r); } }
    if should_review(cfg, st, &Reason::EveryNRuns, now_ms).is_ok() { return Some(Reason::EveryNRuns); }
    None
}

// ── what it proposes ──────────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    #[serde(default)] pub id: Option<i64>,
    /// the node the change is about (one accepted change per node per cycle)
    pub node: String,
    pub kind: String,
    pub patch: Vec<PatchOp>,
    pub hypothesis: String, pub metric: String, #[serde(default)] pub expected: String,
    /// the rows it cites, with n
    pub evidence: String, #[serde(default)] pub risk: String,
}

/// The proposals in a model's reply: an array, or `{"proposals": [...]}`, in prose or fenced.
pub fn parse_proposals(text: &str) -> Result<Vec<Proposal>, String> {
    let try_parse = |s: &str| -> Option<Vec<Proposal>> { serde_json::from_str::<Vec<Proposal>>(s).ok().or_else(|| serde_json::from_str::<Value>(s).ok().and_then(|v| serde_json::from_value(v.get("proposals")?.clone()).ok())) };
    if let Some(p) = try_parse(text.trim()) { return Ok(p); }
    if let (Some(a), Some(b)) = (text.find('{'), text.rfind('}')) { if a < b { if let Some(p) = try_parse(&text[a..=b]) { return Ok(p); } } }
    if let (Some(a), Some(b)) = (text.find('['), text.rfind(']')) { if a < b { if let Some(p) = try_parse(&text[a..=b]) { return Ok(p); } } }
    Err("the reply holds no proposals in the form asked".into())
}

/// Why a proposal may not be listed, or None. Frozen, unknown node, no evidence, no patch, a node still being judged after its last change.
pub fn vet(cfg: &Config, st: &State, g: &Graph, node_calls: &BTreeMap<String, u32>, p: &Proposal) -> Option<String> {
    if cfg.freeze || cfg.frozen_nodes.iter().any(|n| *n == p.node) { return Some("frozen: no proposals for this node".into()); }
    if !p.node.is_empty() && !g.nodes.contains_key(&p.node) { return Some(format!("{} is not in the graph", p.node)); }
    if p.patch.is_empty() || p.patch.len() > 6 { return Some("a proposal is one to six patch ops".into()); }
    if p.hypothesis.trim().is_empty() || p.metric.trim().is_empty() { return Some("say the hypothesis and the metric it should move".into()); }
    if p.evidence.trim().is_empty() { return Some("cite the rows it rests on, with n".into()); }
    if let Some(ch) = st.changes.get(&p.node) { if ch.judged.is_none() { let since = node_calls.get(&p.node).copied().unwrap_or(0).saturating_sub(ch.calls_at_change); if since < MIN_N { return Some(format!("{} changed recently: its new version needs {MIN_N} more calls before it is judged or touched again ({since} so far)", p.node)); } } }
    None
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ReviewOut { pub proposed: Vec<i64>, pub rejected: Vec<(String, String)> }

/// A review's outcome from the model's reply: list what survives the vetting. NOTHING is applied and the graph is only read.
pub fn review_with(root: &str, team: &str, g: &Graph, reply: &str, node_calls: &BTreeMap<String, u32>, now_ms: i64) -> Result<ReviewOut, String> {
    let cfg = load_config(root, team); let mut st = load_state(root, team);
    let props = parse_proposals(reply)?;
    let mut out = ReviewOut::default(); let mut nodes_taken: Vec<String> = vec![];
    for p in props {
        if let Some(why) = vet(&cfg, &st, g, node_calls, &p) { out.rejected.push((p.node.clone(), why)); continue; }
        // one hypothesis per cycle and per node: the first stands, the rest wait for the next review
        if nodes_taken.contains(&p.node) { out.rejected.push((p.node.clone(), "one change per node per cycle".into())); continue; }
        // the same change already waits for the person: a second review does not pile it up again
        let patch_json = serde_json::to_string(&p.patch).unwrap_or_default(); let kind_key = format!("{}:{}", p.node, p.kind);
        let waiting: i64 = crate::ledger::with_db(root, team, |c| Ok(c.query_row("SELECT COUNT(*) FROM proposals WHERE author = 'improver' AND status = 'pending' AND kind = ?1 AND patch = ?2", rusqlite::params![kind_key, patch_json], |r| r.get(0)).unwrap_or(0))).unwrap_or(0);
        if waiting > 0 { out.rejected.push((p.node.clone(), "the same change is already waiting for your decision".into())); continue; }
        let id = crate::ledger::with_db(root, team, |c| {
            c.execute("INSERT INTO proposals (ts_ms, author, kind, patch, hypothesis, metric, expected, evidence, risk, status) VALUES (?1,'improver',?2,?3,?4,?5,?6,?7,?8,'pending')",
                rusqlite::params![now_ms, format!("{}:{}", p.node, p.kind), serde_json::to_string(&p.patch).unwrap_or_default(), p.hypothesis, p.metric, p.expected, p.evidence, p.risk]).map_err(|e| e.to_string())?;
            Ok(c.last_insert_rowid())
        })?;
        nodes_taken.push(p.node.clone()); out.proposed.push(id); st.proposed += 1;
    }
    st.last_review_ms = now_ms; st.runs_since_review = 0; st.cycle_nodes.clear(); st.auto_this_cycle = 0;
    save_state(root, team, &st)?;
    Ok(out)
}

#[derive(Clone, Debug, Serialize)]
pub struct Stored { pub id: i64, pub node: String, pub kind: String, pub patch: Vec<PatchOp>, pub hypothesis: String, pub metric: String, pub expected: String, pub evidence: String, pub risk: String, pub status: String, pub requires_person: bool }

pub fn list(root: &str, team: &str, status: Option<&str>) -> Result<Vec<Stored>, String> {
    let g = crate::team_graph_run::load_graph(root, team)?;
    crate::ledger::with_db(root, team, |c| {
        let mut st = c.prepare("SELECT id, kind, patch, hypothesis, metric, expected, evidence, risk, status FROM proposals WHERE author = 'improver' AND (?1 IS NULL OR status = ?1) ORDER BY id").map_err(|e| e.to_string())?;
        let rows = st.query_map([status], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?, r.get::<_, Option<String>>(4)?, r.get::<_, Option<String>>(5)?, r.get::<_, Option<String>>(6)?, r.get::<_, Option<String>>(7)?, r.get::<_, Option<String>>(8)?))).map_err(|e| e.to_string())?;
        let mut out = vec![];
        for r in rows { let (id, kind, patch, h, m, e, ev, risk, status) = r.map_err(|e| e.to_string())?;
            let ops: Vec<PatchOp> = serde_json::from_str(&patch).unwrap_or_default();
            let (node, kind) = kind.split_once(':').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((String::new(), kind));
            out.push(Stored { id, node, kind, requires_person: team_patch::patch_requires_person(&ops, &g), patch: ops, hypothesis: h.unwrap_or_default(), metric: m.unwrap_or_default(), expected: e.unwrap_or_default(), evidence: ev.unwrap_or_default(), risk: risk.unwrap_or_default(), status: status.unwrap_or_default() }); }
        Ok(out)
    })
}

/// The decision on a proposal: `accept` (apply through the checkpointed patch path), `reject`. The improver itself can never decide; a patch that is the person's (a gate, a budget, triggers, limits, setpoints) needs the person.
pub fn decide(root: &str, team: &str, id: i64, accept: bool, decided_by: &str, by_agent: bool, person_ok: bool, now_ms: i64) -> Result<Value, String> {
    if decided_by.trim().is_empty() || decided_by == "improver" || decided_by.starts_with("improver:") { return Err("the improver proposes; it cannot decide or apply".into()); }
    let p = list(root, team, None)?.into_iter().find(|x| x.id == id).ok_or_else(|| format!("no proposal {id}"))?;
    if p.status != "pending" { return Err(format!("proposal {id} is {} already", p.status)); }
    let mut st = load_state(root, team);
    if !accept { crate::ledger::mark_proposal(root, team, id, "rejected", decided_by)?; return Ok(json!({ "ok": true, "status": "rejected" })); }
    if st.cycle_nodes.iter().any(|n| *n == p.node) { return Err(format!("{} already took a change in this cycle: one accepted change per node per cycle", p.node)); }
    let cfg = load_config(root, team);
    if cfg.freeze || cfg.frozen_nodes.iter().any(|n| *n == p.node) { return Err("frozen: nothing is applied to this node".into()); }
    let calls = crate::team_bench::node_stats(root, team, &p.node).values().map(|s| s.n).sum::<u32>();
    let before_ok = { let m = crate::team_bench::node_stats(root, team, &p.node); let (n, ok) = m.values().fold((0u32, 0u32), |a, s| (a.0 + s.n, a.1 + s.ok)); if n == 0 { 0.0 } else { ok as f64 / n as f64 } };
    let out = team_patch::apply(root, team, &p.patch, person_ok, decided_by, &format!("improver proposal {id}: {}", p.hypothesis), Some(id), by_agent, &CheckpointHooks::default())?;
    st.cycle_nodes.push(p.node.clone()); st.accepted += 1;
    st.changes.insert(p.node.clone(), Change { proposal: id, at_ms: now_ms, before_ok_rate: before_ok, calls_at_change: calls, judged: None });
    save_state(root, team, &st)?;
    Ok(json!({ "ok": true, "status": "applied", "version_id": out.version_id, "graph_hash": out.graph_hash }))
}

/// Auto-apply mode: low-risk kinds only, never a patch that is the person's, at most `max_auto_per_cycle` a cycle. The engine decides, not the improver.
pub fn auto_apply(root: &str, team: &str, now_ms: i64) -> Vec<i64> {
    let cfg = load_config(root, team);
    if cfg.mode != Mode::AutoLowRisk || cfg.freeze { return vec![]; }
    let mut done = vec![];
    for p in list(root, team, Some("pending")).unwrap_or_default() {
        if load_state(root, team).auto_this_cycle >= cfg.max_auto_per_cycle { break; }
        if !LOW_RISK_KINDS.contains(&p.kind.as_str()) || p.requires_person { continue; }
        if decide(root, team, p.id, true, "engine:auto", true, false, now_ms).is_ok() { let mut st = load_state(root, team); st.auto_this_cycle += 1; let _ = save_state(root, team, &st); done.push(p.id); }
    }
    done
}

/// After enough calls on a changed node: did the change help? Records the verdict and the hit rate. Hurt = the Leader should roll back.
pub fn judge_change(root: &str, team: &str, node: &str, after: &Stats) -> Option<String> {
    let mut st = load_state(root, team);
    let ch = st.changes.get(node)?.clone(); if ch.judged.is_some() || after.n < MIN_N { return None; }
    let verdict = if after.ok_rate >= ch.before_ok_rate + 0.1 { "helped" } else if after.ok_rate <= ch.before_ok_rate - 0.1 { "hurt" } else { "no_change" };
    match verdict { "helped" => st.helped += 1, "hurt" => st.hurt += 1, _ => {} }
    if let Some(c) = st.changes.get_mut(node) { c.judged = Some(verdict.into()); }
    save_state(root, team, &st).ok(); Some(verdict.into())
}

/// Judge every change that has had MIN_N calls since it was made, from the calls AFTER it (the ledger rows newer than the change).
pub fn judge_pending(root: &str, team: &str) {
    let st = load_state(root, team);
    for (node, ch) in st.changes.iter().filter(|(_, c)| c.judged.is_none()) {
        let calls: Vec<crate::team_bench::CallStat> = crate::ledger::with_db(root, team, |c| {
            let mut q = c.prepare("SELECT outcome, COALESCE(verified, 1), COALESCE(wall_ms, 0), COALESCE(cost_usd, 0) FROM calls WHERE node = ?1 AND ts_ms > ?2 AND bench = 0").map_err(|e| e.to_string())?;
            let it = q.query_map(rusqlite::params![node, ch.at_ms], |r| { let o: String = r.get(0)?; let v: i64 = r.get(1)?; Ok(crate::team_bench::CallStat { ok: o == "ok" && v != 0, wall_ms: r.get(2)?, cost_usd: r.get(3)?, retried: false, climbed: false }) }).map_err(|e| e.to_string())?;
            it.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
        }).unwrap_or_default();
        if !calls.is_empty() { judge_change(root, team, node, &crate::team_bench::stats_of(&calls)); }
    }
}

/// A review, end to end: ask the improver's seat (read-only: it only answers), list what survives the vetting, and in auto mode let the ENGINE take the low-risk ones. Nothing is applied by the improver.
pub async fn run_review(root: &str, team: &str, why: Reason) -> Result<Value, String> {
    let cfg = load_config(root, team); let st = load_state(root, team); let now = crate::cli::now_ms();
    should_review(&cfg, &st, &why, now)?;
    let g = crate::team_graph_run::load_graph(root, team)?;
    let p = prompt(&brief(root, team, &g), &why);
    let seat = crate::team_llm::parse_seat(&cfg.seat)?;
    let ctx = crate::ledger::Ctx { root: root.to_string(), team: team.to_string(), caller: "improver".into(), ..Default::default() };
    let (text, cost, _) = crate::team_llm::call(&seat, &p, 1500, 180, Some(ctx)).await?;
    crate::ledger::flush();
    let calls: BTreeMap<String, u32> = g.nodes.keys().map(|n| (n.clone(), crate::team_bench::node_stats(root, team, n).values().map(|s| s.n).sum())).collect();
    let out = review_with(root, team, &g, &text, &calls, now)?;
    let auto = auto_apply(root, team, now);
    Ok(json!({ "ok": true, "reason": why.label(), "proposed": out.proposed, "rejected": out.rejected, "auto_applied": auto, "cost_usd": cost, "over_budget": cost > cfg.budget_usd }))
}

/// After a run: count it, and say whether the improver should wake.
pub fn after_run(root: &str, team: &str, finding_codes: &[String], now_ms: i64) -> Option<Reason> {
    let cfg = load_config(root, team); let mut st = load_state(root, team);
    st.runs_since_review += 1; let _ = save_state(root, team, &st);
    judge_pending(root, team);
    let st = load_state(root, team);
    if cfg.mode == Mode::Off { return None; }
    triggers_of(&cfg, &st, finding_codes, now_ms)
}

// ── the brief it reads (deterministic: the same team, the same text) ─────────────────────────────────────────────────────

/// Everything the improver may read, as text: the graph's nodes, each node's benchmark per seat, the recent audits' findings, the setpoints, the memory sizes. No tools, no writes.
pub fn brief(root: &str, team: &str, g: &Graph) -> String {
    let mut s = format!("TEAM {} (budget ${:.2})\n\nNODES\n", g.team, g.budget_usd);
    for (id, n) in &g.nodes { s.push_str(&format!("- {id}: {}\n", n.kind_key())); }
    s.push_str("\nBENCHMARK PER NODE AND SEAT (n, ok rate, verdict)\n");
    for id in g.nodes.keys() { let st = crate::team_bench::node_stats(root, team, id); for (seat, x) in st { s.push_str(&format!("- {id} on {seat}: n={} ok={:.0}% p95={} ms {}\n", x.n, x.ok_rate * 100.0, x.p95_ms, x.verdict)); } }
    if !g.setpoints.is_empty() { s.push_str(&format!("\nSETPOINTS {}\n", serde_json::to_string(&g.setpoints).unwrap_or_default())); }
    let mem = crate::ledger::with_db(root, team, |c| Ok((c.query_row::<i64, _, _>("SELECT COUNT(*) FROM examples WHERE kind = 'golden'", [], |r| r.get(0)).unwrap_or(0), c.query_row::<i64, _, _>("SELECT COUNT(*) FROM examples WHERE kind = 'regression'", [], |r| r.get(0)).unwrap_or(0)))).unwrap_or((0, 0));
    s.push_str(&format!("\nMEMORY golden={} regression={}\n", mem.0, mem.1));
    s
}

pub fn prompt(brief: &str, why: &Reason) -> String {
    format!("You are the IMPROVER of an AI team. You are read-only: you apply nothing. Reason for this review: {}.\n\n{brief}\n\nPropose AT MOST three structural changes, one hypothesis each, as JSON only: {{\"proposals\": [{{\"node\": \"<node id>\", \"kind\": \"prompt_wording|seat_cheaper_with_trial|add_check|split|merge|other\", \"patch\": [<ops>], \"hypothesis\": \"...\", \"metric\": \"which number should move\", \"expected\": \"how much\", \"evidence\": \"the rows you cite, with n\", \"risk\": \"...\"}}]}}. Patch ops: add_node, remove_node, set_field {{node, path, value}}, add_edge, remove_edge, set_graph_field {{field, value}}, rename. Never weaken a Check, a budget or a Human gate. If nothing is worth changing, answer {{\"proposals\": []}}.", why.label())
}

// ── ablation (RFC §18, the improver's tool) ───────────────────────────────────────────────────────────────────────────────

/// The graph without node `x`: every edge into it is joined to every edge out of it (the same ports kept), so the rest still runs. The Leader and delivery are never removed.
pub fn ablate(g: &Graph, x: &str) -> Result<Graph, String> {
    let n = g.nodes.get(x).ok_or_else(|| format!("no node called {x}"))?;
    if matches!(n.kind, crate::team_graph::Kind::Leader | crate::team_graph::Kind::Deliver) { return Err("the Leader and the delivery are never removed".into()); }
    let mut out = g.clone();
    let ins: Vec<Edge> = g.edges.iter().filter(|e| e.to == x).cloned().collect(); let outs: Vec<Edge> = g.edges.iter().filter(|e| e.from == x).cloned().collect();
    out.nodes.remove(x); out.edges.retain(|e| e.from != x && e.to != x);
    for i in &ins { for o in &outs { let e = Edge { from: i.from.clone(), port: i.port.clone(), to: o.to.clone() }; if !out.edges.contains(&e) { out.edges.push(e); } } }
    out.start.retain(|s| s != x); if g.start.iter().any(|s| s == x) { for o in &outs { if !out.start.contains(&o.to) { out.start.push(o.to.clone()); } } }
    out.setpoints.remove(x);
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Ablation { pub propose_removal: bool, pub why: String }

/// With and without the node on the same held-out set: remove it only if quality is not lower, no trap is newly failed, and both ran enough cases.
pub fn ablation_verdict(with: &TrialReport, without: &TrialReport) -> Ablation {
    if with.n < MIN_N || without.n < MIN_N { return Ablation { propose_removal: false, why: format!("too few held-out cases (with n={}, without n={}; the minimum is {MIN_N})", with.n, without.n) }; }
    if without.trap_failures > with.trap_failures { return Ablation { propose_removal: false, why: "without it a trap that was caught now fails".into() }; }
    if without.pass_rate + 1e-9 < with.pass_rate { return Ablation { propose_removal: false, why: format!("quality is lower without it ({:.0}% against {:.0}%)", without.pass_rate * 100.0, with.pass_rate * 100.0) }; }
    Ablation { propose_removal: true, why: format!("quality is not lower without it ({:.0}% against {:.0}%) and no trap is lost: propose removing it", without.pass_rate * 100.0, with.pass_rate * 100.0) }
}

/// The held-out set of a task class: its held-out golden examples (never used as prompt examples) plus every trap.
pub fn heldout_cases(root: &str, team: &str, task_class: &str) -> Vec<Case> {
    crate::ledger::with_db(root, team, |c| {
        let mut st = c.prepare("SELECT input, output, kind FROM examples WHERE task_class = ?1 AND ((kind = 'golden' AND split = 'heldout') OR kind = 'trap') ORDER BY id").map_err(|e| e.to_string())?;
        let rows = st.query_map([task_class], |r| Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get::<_, Option<String>>(1)?.unwrap_or_default(), r.get::<_, String>(2)?))).map_err(|e| e.to_string())?;
        let mut out = vec![];
        for r in rows { let (input, output, kind) = r.map_err(|e| e.to_string())?; if let Some(m) = serde_json::from_str::<Value>(&output).ok().and_then(|v| v.as_object().cloned()) { out.push(Case { input, expect: Expect::Fields(m), trap: kind == "trap" }); } }
        Ok(out)
    }).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_graph::{template, Kind, Node};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NTH: AtomicU64 = AtomicU64::new(0);
    fn ws() -> (String, String) { let n = NTH.fetch_add(1, Ordering::SeqCst); let d = std::env::temp_dir().join(format!("antiloki-improve-{}-{n}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); (d.to_string_lossy().into_owned(), "imp team".into()) }
    fn on(root: &str, team: &str, mode: Mode) -> Config { let c = Config { mode, ..Default::default() }; save_config(root, team, &c).unwrap(); c }
    fn seed(root: &str, team: &str) -> Graph { let g = template("blank", team, 4.0).unwrap(); crate::team_graph_run::save_graph(root, &g).unwrap(); g }
    fn prop(node: &str, kind: &str, ops: Value) -> Value { json!({ "node": node, "kind": kind, "patch": ops, "hypothesis": "a clearer label reads better", "metric": "ok rate", "expected": "+5 points", "evidence": "me on endpoint:a: n=30, ok 70%", "risk": "low" }) }
    fn label(v: &str) -> Value { json!([{ "op": "set_field", "node": "me", "path": "label", "value": v }]) }
    fn reply(ps: Vec<Value>) -> String { json!({ "proposals": ps }).to_string() }

    #[test]
    fn a_review_is_triggered_by_a_gate_finding_or_n_runs_or_a_request_and_not_by_every_run() {
        let cfg = Config { mode: Mode::Suggest, ..Default::default() }; let mut st = State::default(); let t = 100 * WEEK_S * 1000;
        assert_eq!(triggers_of(&cfg, &st, &[], t), None, "a quiet run wakes nothing");
        st.runs_since_review = 3; assert_eq!(triggers_of(&cfg, &st, &["silent_failure_edge".into()], t), None, "a finding that is not a trigger wakes nothing");
        assert_eq!(triggers_of(&cfg, &st, &["node_unreliable".into()], t), Some(Reason::GateFinding("node_unreliable".into())));
        for c in TRIGGER_CODES { assert!(should_review(&cfg, &st, &Reason::GateFinding(c.into()), t).is_ok(), "{c}"); }
        st.runs_since_review = 10; assert_eq!(triggers_of(&cfg, &st, &[], t), Some(Reason::EveryNRuns));
        st.runs_since_review = 9; assert!(should_review(&cfg, &st, &Reason::EveryNRuns, t).is_err());
        assert!(should_review(&cfg, &st, &Reason::Asked, t).is_ok() && should_review(&cfg, &st, &Reason::MarkedBad, t).is_ok());
        // off: only the person's ask works; frozen: the same
        let off = Config::default(); assert_eq!(off.mode, Mode::Off); assert!(should_review(&off, &st, &Reason::GateFinding("node_unreliable".into()), t).is_err()); assert!(should_review(&off, &st, &Reason::Asked, t).is_ok());
        let frozen = Config { mode: Mode::Suggest, freeze: true, ..Default::default() }; assert!(should_review(&frozen, &st, &Reason::MarkedBad, t).is_err());
        // and a run counted through the files
        let (root, team) = ws(); on(&root, &team, Mode::Suggest);
        assert_eq!(after_run(&root, &team, &[], t), None); assert_eq!(load_state(&root, &team).runs_since_review, 1);
        assert_eq!(after_run(&root, &team, &["node_regressed".into()], t), Some(Reason::GateFinding("node_regressed".into())));
    }

    #[test]
    fn the_cooldown_blocks_a_second_review() {
        let cfg = Config { mode: Mode::Suggest, ..Default::default() }; let t0 = 1_000_000_000_000i64;
        let st = State { last_review_ms: t0, runs_since_review: 50, ..Default::default() };
        let f = Reason::GateFinding("node_unreliable".into());
        assert!(should_review(&cfg, &st, &f, t0 + 3600 * 1000).unwrap_err().contains("cooldown"));
        assert!(should_review(&cfg, &st, &Reason::EveryNRuns, t0 + 3600 * 1000).is_err());
        assert!(should_review(&cfg, &st, &f, t0 + 7 * 3600 * 1000).is_ok(), "after the cooldown it may wake");
        assert!(should_review(&cfg, &st, &Reason::EveryNRuns, t0 + 7 * 3600 * 1000).is_err(), "a run count never wakes it more than weekly");
        assert!(should_review(&cfg, &st, &Reason::EveryNRuns, t0 + WEEK_S * 1000 + 1).is_ok());
        assert!(should_review(&cfg, &st, &Reason::Asked, t0 + 1000).is_ok(), "the person's own ask is never made to wait");
        // a finished review sets the clock
        let (root, team) = ws(); on(&root, &team, Mode::Suggest); let g = seed(&root, &team);
        review_with(&root, &team, &g, &reply(vec![]), &BTreeMap::new(), t0).unwrap();
        assert_eq!(load_state(&root, &team).last_review_ms, t0);
        assert!(should_review(&load_config(&root, &team), &load_state(&root, &team), &f, t0 + 1000).is_err());
    }

    #[test]
    fn one_accepted_change_per_node_per_cycle() {
        let (root, team) = ws(); on(&root, &team, Mode::Suggest); let g = seed(&root, &team);
        // two proposals for the same node in one review: only the first is listed
        let out = review_with(&root, &team, &g, &reply(vec![prop("me", "prompt_wording", label("one")), prop("me", "prompt_wording", label("two"))]), &BTreeMap::new(), 1000).unwrap();
        assert_eq!(out.proposed.len(), 1); assert!(out.rejected.iter().any(|(n, w)| n == "me" && w.contains("one change per node per cycle")), "{:?}", out.rejected);
        let id = out.proposed[0];
        decide(&root, &team, id, true, "leader", true, false, 2000).unwrap();
        // a second proposal for the same node, decided in the same cycle, cannot be accepted
        crate::ledger::with_db(&root, &team, |c| { c.execute("INSERT INTO proposals (ts_ms, author, kind, patch, hypothesis, metric, evidence, status) VALUES (3000,'improver','me:prompt_wording',?1,'h','m','e','pending')", [serde_json::to_string(&serde_json::from_value::<Vec<PatchOp>>(label("three")).unwrap()).unwrap()]).unwrap(); Ok(()) }).unwrap();
        let second = list(&root, &team, Some("pending")).unwrap()[0].id;
        assert!(decide(&root, &team, second, true, "leader", true, false, 3000).unwrap_err().contains("one accepted change per node per cycle"));
        // the changed node is not touched again until its new version has MIN_N calls
        let g2 = crate::team_graph_run::load_graph(&root, &team).unwrap();
        let again = review_with(&root, &team, &g2, &reply(vec![prop("me", "prompt_wording", label("four"))]), &BTreeMap::new(), 9000).unwrap();
        assert!(again.proposed.is_empty() && again.rejected[0].1.contains("changed recently"), "{:?}", again.rejected);
        let calls: BTreeMap<String, u32> = [("me".to_string(), MIN_N + 1)].into_iter().collect();
        assert_eq!(review_with(&root, &team, &g2, &reply(vec![prop("me", "prompt_wording", label("four"))]), &calls, 10_000).unwrap().proposed.len(), 1, "and then it may be proposed again, in a new cycle");
    }

    #[test]
    fn the_improver_is_read_only_and_cannot_apply() {
        let (root, team) = ws(); on(&root, &team, Mode::Suggest); let g = seed(&root, &team);
        let before = crate::team_graph_run::graph_hash(&g);
        let out = review_with(&root, &team, &g, &reply(vec![prop("me", "prompt_wording", label("x"))]), &BTreeMap::new(), 1000).unwrap();
        assert_eq!(crate::team_graph_run::graph_hash(&crate::team_graph_run::load_graph(&root, &team).unwrap()), before, "a review changes nothing");
        assert_eq!(list(&root, &team, Some("pending")).unwrap().len(), 1);
        for who in ["improver", "improver:7", ""] { assert!(decide(&root, &team, out.proposed[0], true, who, false, true, 2000).unwrap_err().contains("cannot decide"), "{who}"); }
        assert_eq!(crate::team_graph_run::graph_hash(&crate::team_graph_run::load_graph(&root, &team).unwrap()), before);
        assert!(brief(&root, &team, &g).contains("NODES") && prompt(&brief(&root, &team, &g), &Reason::Asked).contains("you apply nothing"));
    }

    #[test]
    fn a_proposal_that_weakens_a_gate_a_budget_or_a_human_gate_requires_the_person() {
        let (root, team) = ws(); on(&root, &team, Mode::Suggest); let g = seed(&root, &team);
        let out = review_with(&root, &team, &g, &reply(vec![prop("me", "other", json!([{ "op": "set_field", "node": "me", "path": "gate", "value": "none" }])), prop("deliver", "other", json!([{ "op": "set_graph_field", "field": "budget_usd", "value": 50.0 }]))]), &BTreeMap::new(), 1000).unwrap();
        assert_eq!(out.proposed.len(), 2); let listed = list(&root, &team, Some("pending")).unwrap(); assert!(listed.iter().all(|p| p.requires_person), "{listed:?}");
        for id in &out.proposed {
            assert!(decide(&root, &team, *id, true, "leader", true, true, 2000).unwrap_err().contains("agent"), "an agent never accepts it, even claiming the person said yes");
            assert!(decide(&root, &team, *id, true, "person", false, false, 2000).unwrap_err().contains("person"), "without the person's yes it is refused");
        }
        assert_eq!(decide(&root, &team, out.proposed[0], true, "person", false, true, 2000).unwrap()["status"], json!("applied"));
    }

    #[test]
    fn auto_apply_never_touches_gates_budgets_or_triggers() {
        let (root, team) = ws(); on(&root, &team, Mode::AutoLowRisk); let g = seed(&root, &team);
        let out = review_with(&root, &team, &g, &reply(vec![prop("me", "prompt_wording", json!([{ "op": "set_field", "node": "me", "path": "gate", "value": "none" }])), prop("deliver", "seat_cheaper_with_trial", json!([{ "op": "set_graph_field", "field": "triggers", "value": [{ "cron": "0 9 * * *", "ask": "go" }] }])), prop("me", "other", label("kind not low risk"))]), &BTreeMap::new(), 1000).unwrap();
        assert!(!out.proposed.is_empty());
        assert!(auto_apply(&root, &team, 2000).is_empty(), "a gate, a trigger, a kind that is not low risk: none is taken");
        assert_eq!(list(&root, &team, Some("pending")).unwrap().len(), out.proposed.len());
        // a plain wording change is taken, once a cycle, by the engine
        let (root, team) = ws(); on(&root, &team, Mode::AutoLowRisk); let g = seed(&root, &team);
        review_with(&root, &team, &g, &reply(vec![prop("me", "prompt_wording", label("clearer"))]), &BTreeMap::new(), 1000).unwrap();
        assert_eq!(auto_apply(&root, &team, 2000).len(), 1);
        assert_eq!(crate::team_graph_run::load_graph(&root, &team).unwrap().to_json()["nodes"]["me"]["label"], json!("clearer"));
        // not in suggest mode
        let (root, team) = ws(); on(&root, &team, Mode::Suggest); let g = seed(&root, &team);
        review_with(&root, &team, &g, &reply(vec![prop("me", "prompt_wording", label("clearer"))]), &BTreeMap::new(), 1000).unwrap();
        assert!(auto_apply(&root, &team, 2000).is_empty());
    }

    #[test]
    fn the_freeze_flag_blocks_proposals() {
        let (root, team) = ws(); let g = seed(&root, &team);
        save_config(&root, &team, &Config { mode: Mode::Suggest, freeze: true, ..Default::default() }).unwrap();
        let out = review_with(&root, &team, &g, &reply(vec![prop("me", "prompt_wording", label("x"))]), &BTreeMap::new(), 1000).unwrap();
        assert!(out.proposed.is_empty() && out.rejected[0].1.contains("frozen"), "{:?}", out.rejected);
        save_config(&root, &team, &Config { mode: Mode::Suggest, frozen_nodes: vec!["me".into()], ..Default::default() }).unwrap();
        let out = review_with(&root, &team, &g, &reply(vec![prop("me", "prompt_wording", label("x")), prop("deliver", "prompt_wording", json!([{ "op": "set_field", "node": "deliver", "path": "label", "value": "y" }]))]), &BTreeMap::new(), 2000).unwrap();
        assert_eq!(out.proposed.len(), 1, "only the node that is not frozen: {:?}", out.rejected);
        // a proposal that was listed before the freeze cannot be applied after it
        save_config(&root, &team, &Config { mode: Mode::Suggest, freeze: true, ..Default::default() }).unwrap();
        assert!(decide(&root, &team, out.proposed[0], true, "leader", true, false, 3000).unwrap_err().contains("frozen"));
        // bad replies and bad proposals are said, not applied
        assert!(parse_proposals("no json here").is_err());
        let g = crate::team_graph_run::load_graph(&root, &team).unwrap(); save_config(&root, &team, &Config { mode: Mode::Suggest, ..Default::default() }).unwrap();
        let out = review_with(&root, &team, &g, &reply(vec![prop("ghost", "other", label("x")), json!({ "node": "me", "kind": "other", "patch": label("z"), "hypothesis": "h", "metric": "m", "evidence": "" })]), &BTreeMap::new(), 9000).unwrap();
        assert!(out.proposed.is_empty() && out.rejected.len() == 2, "{:?}", out.rejected);
    }

    #[test]
    fn the_hit_rate_is_recorded() {
        let (root, team) = ws(); on(&root, &team, Mode::Suggest); let g = seed(&root, &team);
        assert_eq!(load_state(&root, &team).hit_rate(), 0.0);
        let mut st = load_state(&root, &team); st.proposed = 4; st.accepted = 3;
        st.changes.insert("a".into(), Change { proposal: 1, before_ok_rate: 0.6, calls_at_change: 0, ..Default::default() }); st.changes.insert("b".into(), Change { proposal: 2, before_ok_rate: 0.6, ..Default::default() }); st.changes.insert("c".into(), Change { proposal: 3, before_ok_rate: 0.9, ..Default::default() });
        save_state(&root, &team, &st).unwrap();
        let s = |ok: u32, n: u32| crate::team_bench::stats_of(&(0..n).map(|i| crate::team_bench::CallStat { ok: i < ok, wall_ms: 1, cost_usd: 0.0, retried: false, climbed: false }).collect::<Vec<_>>());
        assert_eq!(judge_change(&root, &team, "a", &s(9, 10)).as_deref(), Some("helped"));
        assert_eq!(judge_change(&root, &team, "b", &s(6, 10)).as_deref(), Some("no_change"));
        assert_eq!(judge_change(&root, &team, "c", &s(5, 10)).as_deref(), Some("hurt"));
        assert_eq!(judge_change(&root, &team, "a", &s(9, 10)), None, "a change is judged once");
        assert_eq!(judge_change(&root, &team, "c", &s(1, 2)), None);
        let st = load_state(&root, &team); assert_eq!((st.proposed, st.accepted, st.helped, st.hurt), (4, 3, 1, 1)); assert!((st.hit_rate() - 0.25).abs() < 1e-9, "accepted and helped over proposed");
        let _ = g;
    }

    #[test]
    fn the_ablation_runner_compares_without_a_node_on_the_held_out_set() {
        // the graph without the node: its edges are joined, the Leader and the delivery stay
        let mut g = template("blank", "Abl", 2.0).unwrap(); g.edges.clear(); g.start = vec!["a".into()];
        for id in ["a", "x", "b"] { g.nodes.insert(id.into(), Node::new(Kind::Fork)); }
        for e in [Edge { from: "a".into(), port: "branch".into(), to: "x".into() }, Edge { from: "x".into(), port: "branch".into(), to: "b".into() }] { g.edges.push(e); }
        let h = ablate(&g, "x").unwrap(); assert!(!h.nodes.contains_key("x")); assert_eq!(h.edges, vec![Edge { from: "a".into(), port: "branch".into(), to: "b".into() }]);
        assert!(ablate(&g, "me").is_ok() && ablate(&g, "deliver").is_err() && ablate(&g, "ghost").is_err());
        // the held-out set: held-out golden examples and traps, nothing else
        let (root, team) = ws();
        crate::ledger::with_db(&root, &team, |c| { for (i, (kind, split)) in [("golden", "train"), ("golden", "heldout"), ("trap", "none"), ("regression", "none"), ("golden", "heldout")].iter().enumerate() {
            c.execute("INSERT INTO examples (cell, task_class, input, output, kind, split, hash) VALUES ('c','shelf',?1,?2,?3,?4,?5)", rusqlite::params![format!("in{i}"), json!({ "price": i }).to_string(), kind, split, format!("h{i}")]).unwrap(); } Ok(()) }).unwrap();
        let cases = heldout_cases(&root, &team, "shelf"); assert_eq!(cases.len(), 3); assert_eq!(cases.iter().filter(|c| c.trap).count(), 1); assert!(heldout_cases(&root, &team, "other").is_empty());
        // the verdict on scripted trial results
        let rep = |arm: &str, passed: u32, n: u32, trap_fail: u32| TrialReport { arm: arm.into(), n, passed, pass_rate: passed as f64 / n as f64, traps: 2, trap_failures: trap_fail, results: vec![] };
        assert!(ablation_verdict(&rep("with", 9, 10, 0), &rep("without", 9, 10, 0)).propose_removal, "not lower: propose removing it");
        assert!(ablation_verdict(&rep("with", 9, 10, 0), &rep("without", 10, 10, 0)).propose_removal);
        let lower = ablation_verdict(&rep("with", 9, 10, 0), &rep("without", 7, 10, 0)); assert!(!lower.propose_removal && lower.why.contains("lower"));
        let trap = ablation_verdict(&rep("with", 9, 10, 0), &rep("without", 9, 10, 1)); assert!(!trap.propose_removal && trap.why.contains("trap"));
        let few = ablation_verdict(&rep("with", 3, 3, 0), &rep("without", 3, 3, 0)); assert!(!few.propose_removal && few.why.contains("too few"));
    }

    #[test]
    fn a_second_review_does_not_list_a_change_that_is_already_waiting() {
        let (root, team) = ws(); on(&root, &team, Mode::Suggest); let g = seed(&root, &team);
        let r = reply(vec![prop("me", "prompt_wording", label("clearer"))]);
        assert_eq!(review_with(&root, &team, &g, &r, &BTreeMap::new(), 1000).unwrap().proposed.len(), 1);
        let again = review_with(&root, &team, &g, &r, &BTreeMap::new(), 2000).unwrap();
        assert!(again.proposed.is_empty() && again.rejected[0].1.contains("already waiting"), "{:?}", again.rejected);
        assert_eq!(list(&root, &team, Some("pending")).unwrap().len(), 1, "one waits, not two");
        // a different change for the same node is a new proposal
        assert_eq!(review_with(&root, &team, &g, &reply(vec![prop("me", "prompt_wording", label("other wording"))]), &BTreeMap::new(), 3000).unwrap().proposed.len(), 1);
    }
}
