//! THE SWARM WITH A CONTRACT (RFC 0011 §10.3): a block template the Leader instantiates, not a runtime kind. It expands to
//! `for-each → llm(preset, ladder) → check → join` and back to the for-each; the contract is the schema (the preset's), the verification rules (the preset's guard
//! plus any `check` rules) and the ladder of seats (a failed guard climbs to the next seat). A failure the body cannot repair is collected, never fatal to the swarm.
use crate::team_graph::{Edge, Graph, JoinPolicy, Kind, Node};
use crate::team_llm::{ExamplesFrom, PresetSpec, Spec, Verify};
use serde::{Deserialize, Serialize};

/// A swarm is bounded by what the machine can run at once, not by how many nodes the graph has.
pub const MAX_CONCURRENCY: u32 = 8;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SwarmParams {
    /// the id prefix of the expanded nodes
    pub id: String,
    /// where the items come from, as a condition-language path (`plan.items`, `ask_items`)
    pub items: String,
    pub preset: PresetSpec,
    /// the ladder, cheapest first: the first seat answers, a failed guard climbs to the next
    pub seats: Vec<String>,
    pub prompt: String,
    #[serde(default)] pub concurrency: Option<u32>,
    #[serde(default)] pub budget_usd: Option<f64>,
    /// how many seats must give the same answer (2 or 3: the others cross-check the first)
    #[serde(default)] pub quorum: Option<u32>,
    /// free rules a field of the answer must also pass (`rules` as in a Check node)
    #[serde(default)] pub check_rules: Vec<crate::team_check::Rule>,
    #[serde(default)] pub examples_task_class: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Expansion { pub nodes: Vec<(String, Node)>, pub edges: Vec<Edge>, pub entry: String }

/// What is wrong with a swarm, in words. `swarm_unbounded`: no concurrency or no budget.
pub fn check(p: &SwarmParams) -> Vec<String> {
    let mut out = vec![];
    if p.id.trim().is_empty() || !p.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') { out.push("swarm: `id` is letters, digits and underscores".into()); }
    match p.concurrency { None | Some(0) => out.push("swarm_unbounded: say how many items run at once (`concurrency`)".into()), Some(c) if c > MAX_CONCURRENCY => out.push(format!("swarm_unbounded: concurrency is at most {MAX_CONCURRENCY} (the machine's model slots)")), _ => {} }
    if !p.budget_usd.map_or(false, |b| b > 0.0) { out.push("swarm_unbounded: a swarm needs a `budget_usd` above zero".into()); }
    if p.seats.is_empty() { out.push("swarm: name at least one seat".into()); }
    if p.items.trim().is_empty() { out.push("swarm: say where the items come from (`items`)".into()); }
    if p.quorum.map_or(false, |q| !(1..=3).contains(&q)) { out.push("swarm: quorum is 1 to 3".into()); }
    if p.quorum.map_or(false, |q| q as usize > p.seats.len()) { out.push("swarm: a quorum needs that many seats".into()); }
    out
}

pub fn expand(p: &SwarmParams) -> Result<Expansion, Vec<String>> {
    let errs = check(p); if !errs.is_empty() { return Err(errs); }
    let (each, llm, chk, join) = (format!("{}_each", p.id), format!("{}_llm", p.id), format!("{}_check", p.id), format!("{}_join", p.id));
    let quorum = p.quorum.unwrap_or(1).max(1) as usize;
    let spec = Spec {
        seat: p.seats[0].clone(), prompt: format!("{}\n{{{each}.item}}", p.prompt.trim_end()), answer: Default::default(),
        fallback: p.seats[quorum.min(p.seats.len())..].to_vec(),
        agree_with: p.seats[1..quorum.min(p.seats.len())].to_vec(),
        verify: Verify { in_source: vec![], source: format!("{{{each}.item}}") },
        max_tokens: None, timeout_s: None,
        examples_from: p.examples_task_class.as_ref().map(|t| ExamplesFrom { task_class: t.clone(), k: 3 }),
        preset: Some(p.preset.clone()), router: None,
    };
    let field = crate::team_llm::preset_schema(&p.preset).map_err(|e| vec![e])?.keys().next().cloned().unwrap_or_else(|| "text".into());
    let b = p.budget_usd.unwrap_or(0.0);
    let mut nodes: Vec<(String, Node)> = vec![
        (each.clone(), Node::new(Kind::ForEach { over: p.items.clone(), parallel: p.concurrency, batch: None, on_error: Some("collect".into()) })),
        (llm.clone(), Node::new(Kind::Llm(spec)).cap(b)),
    ];
    let mut edges = vec![Edge { from: each.clone(), port: "body".into(), to: llm.clone() }];
    let mut last = llm.clone();
    if !p.check_rules.is_empty() {
        nodes.push((chk.clone(), Node::new(Kind::Check { on: format!("values:{llm}.{field}"), rules: p.check_rules.clone() })));
        edges.push(Edge { from: last.clone(), port: "out".into(), to: chk.clone() }); last = chk.clone();
    }
    nodes.push((join.clone(), Node::new(Kind::Join { policy: JoinPolicy::All, quorum: None })));
    edges.push(Edge { from: last, port: "out".into(), to: join.clone() });
    edges.push(Edge { from: join, port: "out".into(), to: each.clone() });
    Ok(Expansion { nodes, edges, entry: each })
}

/// Put an expansion into a graph. The caller wires the entry in and the for-each's `done` port on.
pub fn apply(g: &mut Graph, e: &Expansion) -> Result<(), String> {
    for (id, _) in &e.nodes { if g.nodes.contains_key(id) { return Err(format!("a node called {id} is already there")); } }
    for (id, n) in &e.nodes { g.nodes.insert(id.clone(), n.clone()); }
    g.edges.extend(e.edges.iter().cloned());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_check::Rule;
    use serde_json::json;

    fn params() -> SwarmParams {
        SwarmParams { id: "tag".into(), items: "ask_items".into(), preset: PresetSpec { kind: "classify".into(), categories: vec!["bug".into(), "idea".into()], ..Default::default() },
            seats: vec!["local:qwen3:4b".into(), "endpoint:mid".into(), "claude-code:haiku".into()], prompt: "Classify this ticket.".into(),
            concurrency: Some(2), budget_usd: Some(0.5), quorum: None, check_rules: vec![Rule { kind: "forbid".into(), ..Default::default() }], examples_task_class: None }
    }

    #[test]
    fn a_swarm_expands_to_foreach_llm_check_join() {
        let e = expand(&params()).unwrap();
        let kinds: Vec<&str> = e.nodes.iter().map(|(_, n)| n.kind_key()).collect();
        assert_eq!(kinds, vec!["for_each", "llm", "check", "join"]);
        assert_eq!(e.entry, "tag_each");
        let ports: Vec<(&str, &str, &str)> = e.edges.iter().map(|x| (x.from.as_str(), x.port.as_str(), x.to.as_str())).collect();
        assert_eq!(ports, vec![("tag_each", "body", "tag_llm"), ("tag_llm", "out", "tag_check"), ("tag_check", "out", "tag_join"), ("tag_join", "out", "tag_each")]);
        match &e.nodes[0].1.kind { Kind::ForEach { parallel, on_error, .. } => { assert_eq!(*parallel, Some(2)); assert_eq!(on_error.as_deref(), Some("collect"), "one bad item never ends the swarm"); } _ => panic!() }
        match &e.nodes[1].1.kind { Kind::Llm(sp) => {
            assert_eq!(sp.seat, "local:qwen3:4b"); assert_eq!(sp.fallback, vec!["endpoint:mid".to_string(), "claude-code:haiku".to_string()], "the ladder is the contract");
            assert!(sp.preset.is_some(), "the preset brings the schema and the guard"); assert!(e.nodes[1].1.cap_usd == Some(0.5), "the budget is on the cell"); } _ => panic!() }
        // the check judges the preset's field, the join collects
        match &e.nodes[2].1.kind { Kind::Check { on, .. } => assert_eq!(on, "values:tag_llm.category"), _ => panic!() }
        // without rules there is no check node
        let mut p = params(); p.check_rules.clear(); let e = expand(&p).unwrap();
        assert_eq!(e.nodes.iter().map(|(_, n)| n.kind_key()).collect::<Vec<_>>(), vec!["for_each", "llm", "join"]);
        // a quorum of two turns the second seat into a cross-check and the third stays the ladder
        let mut p = params(); p.quorum = Some(2); let e = expand(&p).unwrap();
        match &e.nodes[1].1.kind { Kind::Llm(sp) => { assert_eq!(sp.agree_with, vec!["endpoint:mid".to_string()]); assert_eq!(sp.fallback, vec!["claude-code:haiku".to_string()]); } _ => panic!() }
        // and it goes into a graph once
        let mut g = crate::team_graph::template("blank", "Swarm", 2.0).unwrap(); let e = expand(&params()).unwrap();
        apply(&mut g, &e).unwrap(); assert!(g.nodes.contains_key("tag_llm")); assert!(apply(&mut g, &e).is_err());
    }

    #[test]
    fn a_swarm_without_concurrency_or_a_budget_is_a_validator_error() {
        let mut p = params(); p.concurrency = None; assert!(check(&p).iter().any(|e| e.contains("swarm_unbounded") && e.contains("concurrency")));
        let mut p = params(); p.concurrency = Some(0); assert!(check(&p).iter().any(|e| e.contains("swarm_unbounded")));
        let mut p = params(); p.concurrency = Some(MAX_CONCURRENCY + 1); assert!(check(&p).iter().any(|e| e.contains("model slots")));
        let mut p = params(); p.budget_usd = None; assert!(check(&p).iter().any(|e| e.contains("swarm_unbounded") && e.contains("budget")));
        let mut p = params(); p.budget_usd = Some(0.0); assert!(!check(&p).is_empty());
        assert!(expand(&{ let mut p = params(); p.budget_usd = None; p }).is_err(), "an unbounded swarm is never expanded");
        let mut p = params(); p.seats.clear(); assert!(!check(&p).is_empty());
        assert!(check(&params()).is_empty(), "{:?}", check(&params()));
        let _ = json!({});
    }
}
