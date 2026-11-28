//! THE REGULATOR (RFC 0011 §7): INFRA, not a node. After a run the engine compares each node's windowed statistics with the node's `setpoints` and acts INSIDE
//! AN ALLOWED ENVELOPE: raise a finding, stop sending a failing seat calls for a while (the router skips it), shrink a swarm. It NEVER edits the graph: it takes the
//! graph by shared reference, returns actions as data, and the only thing it writes is an overlay file the router and the swarm read. The graph stays the person's.
use crate::team_bench::{Stats, MIN_N};
use crate::team_graph::{Graph, Kind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// What a node is expected to hold. Any part may be left out.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Setpoints {
    #[serde(default, skip_serializing_if = "Option::is_none")] pub min_ok_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub max_p95_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub max_cost_per_ok: Option<f64>,
}

/// How long a seat that fell under its setpoint is left alone before the router may try it again (and the benchmark's exploration share keeps measuring it).
pub const BLOCK_MS: i64 = 30 * 60 * 1000;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Act {
    /// a node is not holding its setpoint: the audit and the Leader hear of it
    Finding { node: String, message: String },
    /// the router leaves this seat alone for a while (a circuit breaker), staying inside the node's own candidates and floor
    Block { node: String, seat: String, until_ms: i64, reason: String },
    /// a swarm runs fewer items at once
    ResizeSwarm { node: String, concurrency: u32, reason: String },
}

/// The envelope of a node: the seats it may use at all. A node with a fixed seat has none, so the regulator can only raise a finding about it.
fn envelope(g: &Graph, node: &str) -> Vec<String> {
    match g.node(node).map(|n| &n.kind) {
        Some(Kind::Llm(sp)) if crate::team_router::is_router_seat(&sp.seat) => sp.router.as_ref().map(|r| r.candidates.iter().map(|c| c.seat.clone()).collect()).unwrap_or_default(),
        _ => vec![],
    }
}

/// Pure: the graph and the statistics in, actions out. Nothing is written; the graph is borrowed, never changed.
pub fn regulate(g: &Graph, stats: &BTreeMap<String, BTreeMap<String, Stats>>, now_ms: i64) -> Vec<Act> {
    let mut out = vec![];
    for (node, sp) in &g.setpoints {
        let Some(per_seat) = stats.get(node) else { continue };
        let env = envelope(g, node);
        for (seat, st) in per_seat {
            if st.n < MIN_N { continue; }   // under the minimum there is no verdict, so no action
            let mut why: Vec<String> = vec![];
            // a breach is confident: even the generous end of the interval is under the setpoint
            if let Some(min) = sp.min_ok_rate { if st.ok_hi < min { why.push(format!("ok rate {:.0}% (n={}, at most {:.0}% with confidence) is under {:.0}%", st.ok_rate * 100.0, st.n, st.ok_hi * 100.0, min * 100.0)); } }
            if let Some(max) = sp.max_p95_ms { if st.p95_ms > max { why.push(format!("p95 {} ms is over {} ms", st.p95_ms, max)); } }
            if let (Some(max), Some(c)) = (sp.max_cost_per_ok, st.cost_per_ok) { if c > max { why.push(format!("cost per ok answer ${c:.4} is over ${max:.4}")); } }
            if why.is_empty() { continue; }
            let message = format!("{node} on {seat}: {}", why.join("; "));
            out.push(Act::Finding { node: node.clone(), message: message.clone() });
            // inside the envelope only: a candidate of this node's own router, and only while another candidate or the floor remains
            if env.contains(seat) && st.ok_hi < sp.min_ok_rate.unwrap_or(0.0) { out.push(Act::Block { node: node.clone(), seat: seat.clone(), until_ms: now_ms + BLOCK_MS, reason: message }); }
            // a slow swarm gets narrower, never to zero
            if sp.max_p95_ms.map_or(false, |m| st.p95_ms > m) {
                for (fid, f) in &g.nodes { if let Kind::ForEach { parallel: Some(p), .. } = &f.kind { if *p > 1 && crate::team_graph::in_body(g, fid, node) { out.push(Act::ResizeSwarm { node: fid.clone(), concurrency: (*p / 2).max(1), reason: format!("{node} is slow") }); } } }
            }
        }
    }
    out
}

/// What the router and the swarm read. It is the only thing the regulator writes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Overlay {
    /// node -> seat -> until (ms)
    #[serde(default)] pub blocked: BTreeMap<String, BTreeMap<String, i64>>,
    #[serde(default)] pub concurrency: BTreeMap<String, u32>,
}

impl Overlay {
    pub fn apply(&mut self, acts: &[Act]) {
        for a in acts { match a {
            Act::Block { node, seat, until_ms, .. } => { self.blocked.entry(node.clone()).or_default().insert(seat.clone(), *until_ms); }
            Act::ResizeSwarm { node, concurrency, .. } => { self.concurrency.insert(node.clone(), *concurrency); }
            Act::Finding { .. } => {}
        } }
    }
    /// drop what has run out
    pub fn expire(&mut self, now_ms: i64) { for m in self.blocked.values_mut() { m.retain(|_, until| *until > now_ms); } self.blocked.retain(|_, m| !m.is_empty()); }
}

fn overlay_path(root: &str, team: &str) -> PathBuf { crate::team_graph_run::team_files(root, team).join("regulator.json") }

pub fn load_overlay(root: &str, team: &str) -> Overlay { std::fs::read_to_string(overlay_path(root, team)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default() }
pub fn save_overlay(root: &str, team: &str, o: &Overlay) -> Result<(), String> {
    let p = overlay_path(root, team); if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    std::fs::write(p, serde_json::to_string_pretty(o).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Seats the router must leave alone for this node right now.
pub fn blocked_seats(root: &str, team: &str, node: &str, now_ms: i64) -> Vec<String> {
    load_overlay(root, team).blocked.get(node).map(|m| m.iter().filter(|(_, u)| **u > now_ms).map(|(s, _)| s.clone()).collect()).unwrap_or_default()
}

/// After a run: read the statistics from the ledger, regulate, write the overlay. Returns the actions (the findings go to the audit). The graph is only read.
pub fn after_run(root: &str, team: &str, g: &Graph, now_ms: i64) -> Vec<Act> {
    if g.setpoints.is_empty() { return vec![]; }
    let stats: BTreeMap<String, BTreeMap<String, Stats>> = g.setpoints.keys().map(|n| (n.clone(), crate::team_bench::node_stats(root, team, n))).collect();
    let acts = regulate(g, &stats, now_ms);
    let mut ov = load_overlay(root, team); ov.expire(now_ms); ov.apply(&acts);
    let _ = save_overlay(root, team, &ov);
    acts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_bench::{stats_of, CallStat};
    use crate::team_graph::{Node, Kind};
    use crate::team_router::{Candidate, RouterSpec};

    fn st(ok: u32, n: u32, ms: i64) -> Stats { stats_of(&(0..n).map(|i| CallStat { ok: i < ok, wall_ms: ms, cost_usd: 0.01, retried: false, climbed: false }).collect::<Vec<_>>()) }
    fn routed() -> Node {
        let sp = crate::team_llm::Spec { seat: "router:cheap".into(), prompt: "p".into(), answer: [("a".to_string(), serde_json::json!("string"))].into_iter().collect(),
            router: Some(RouterSpec { candidates: vec![Candidate { seat: "local:qwen3:4b".into(), max_input_chars: None, cost_usd: Some(0.0) }, Candidate { seat: "endpoint:mid".into(), max_input_chars: None, cost_usd: Some(0.01) }], floor: "claude-code:sonnet".into(), explore: None, min_ok: None, cost_cap_usd: None }), ..Default::default() };
        Node::new(Kind::Llm(sp))
    }
    fn fixed() -> Node { Node::new(Kind::Llm(crate::team_llm::Spec { seat: "endpoint:mid".into(), prompt: "p".into(), fallback: vec!["claude-code:haiku".into()], answer: [("a".to_string(), serde_json::json!("string"))].into_iter().collect(), ..Default::default() })) }
    fn graph() -> Graph {
        let mut g = crate::team_graph::template("blank", "Reg", 2.0).unwrap();
        g.nodes.insert("routed".into(), routed()); g.nodes.insert("fixed".into(), fixed());
        for n in ["routed", "fixed"] { g.setpoints.insert(n.into(), Setpoints { min_ok_rate: Some(0.8), ..Default::default() }); }
        g
    }
    fn stats_for(node: &str, v: &[(&str, Stats)]) -> BTreeMap<String, BTreeMap<String, Stats>> { [(node.to_string(), v.iter().map(|(s, x)| (s.to_string(), x.clone())).collect())].into_iter().collect() }

    #[test]
    fn a_node_over_its_setpoints_raises_a_finding_and_reroutes_inside_the_envelope() {
        let g = graph(); let now = 1_000;
        let stats = stats_for("routed", &[("local:qwen3:4b", st(6, 30, 10)), ("endpoint:mid", st(29, 30, 10))]);
        let acts = regulate(&g, &stats, now);
        assert!(acts.iter().any(|a| matches!(a, Act::Finding { node, message } if node == "routed" && message.contains("local:qwen3:4b") && message.contains("under 80%"))), "{acts:?}");
        assert!(acts.iter().any(|a| matches!(a, Act::Block { node, seat, until_ms, .. } if node == "routed" && seat == "local:qwen3:4b" && *until_ms == now + BLOCK_MS)), "{acts:?}");
        assert!(!acts.iter().any(|a| matches!(a, Act::Block { seat, .. } if seat == "endpoint:mid")), "the seat that holds is left alone");
        // a node with a fixed seat has no envelope: a finding, never a reroute
        let acts = regulate(&g, &stats_for("fixed", &[("endpoint:mid", st(5, 30, 10))]), now);
        assert!(acts.iter().any(|a| matches!(a, Act::Finding { node, .. } if node == "fixed")), "{acts:?}"); assert!(!acts.iter().any(|a| matches!(a, Act::Block { .. })), "{acts:?}");
        // a seat the node does not list is never blocked either
        let acts = regulate(&g, &stats_for("routed", &[("claude-code:sonnet", st(1, 30, 10))]), now);
        assert!(!acts.iter().any(|a| matches!(a, Act::Block { .. })), "the floor is never blocked: {acts:?}");
        // too few runs, no verdict, no action; a node that holds, no action
        assert!(regulate(&g, &stats_for("routed", &[("local:qwen3:4b", st(0, 3, 10))]), now).is_empty());
        assert!(regulate(&g, &stats_for("routed", &[("local:qwen3:4b", st(29, 30, 10))]), now).is_empty());
        // a slow node narrows its swarm, never to zero
        let mut g2 = graph(); g2.setpoints.insert("routed".into(), Setpoints { max_p95_ms: Some(100), ..Default::default() });
        g2.nodes.insert("each".into(), Node::new(Kind::ForEach { over: "x".into(), parallel: Some(4), batch: None, on_error: None })); g2.edges.push(crate::team_graph::Edge { from: "each".into(), port: "body".into(), to: "routed".into() });
        let acts = regulate(&g2, &stats_for("routed", &[("local:qwen3:4b", st(30, 30, 900))]), now);
        assert!(acts.iter().any(|a| matches!(a, Act::ResizeSwarm { node, concurrency: 2, .. } if node == "each")), "{acts:?}");
        // the overlay keeps the block until it runs out, and the router reads it
        let mut ov = Overlay::default(); ov.apply(&regulate(&g, &stats_for("routed", &[("local:qwen3:4b", st(6, 30, 10))]), now));
        assert_eq!(ov.blocked["routed"]["local:qwen3:4b"], now + BLOCK_MS); ov.expire(now + BLOCK_MS + 1); assert!(ov.blocked.is_empty());
    }

    #[test]
    fn the_regulator_never_edits_the_graph() {
        let g = graph(); let before = crate::team_graph_run::graph_hash(&g); let json_before = g.to_json();
        let stats = stats_for("routed", &[("local:qwen3:4b", st(2, 40, 900)), ("endpoint:mid", st(40, 40, 10))]);
        let acts = regulate(&g, &stats, 5);   // the signature takes `&Graph`: it cannot change it
        assert!(!acts.is_empty());
        let mut ov = Overlay::default(); ov.apply(&acts);
        // and writing its overlay to disk leaves the graph alone as well
        let d = std::env::temp_dir().join(format!("antiloki-regulate-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let root = d.to_string_lossy().to_string();
        crate::team_graph_run::save_graph(&root, &g).unwrap();
        let acts2 = after_run(&root, "reg", &g, 5);   // no ledger rows yet: nothing to act on
        assert!(acts2.is_empty(), "{acts2:?}");
        save_overlay(&root, "reg", &ov).unwrap(); assert_eq!(load_overlay(&root, "reg"), ov);
        assert_eq!(blocked_seats(&root, "reg", "routed", 6), vec!["local:qwen3:4b".to_string()]); assert!(blocked_seats(&root, "reg", "routed", 5 + BLOCK_MS + 1).is_empty());
        assert_eq!(crate::team_graph_run::graph_hash(&crate::team_graph_run::load_graph(&root, "reg").unwrap()), before, "the saved graph is the same graph");
        assert_eq!(g.to_json(), json_before); assert_eq!(crate::team_graph_run::graph_hash(&g), before);
        let p = crate::team_graph_run::team_files(&root, "reg").join("regulator.json"); assert!(p.exists(), "the overlay is a separate file");
    }
}
