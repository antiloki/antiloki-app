//! SUB-TEAM (RFC 0011 §14): a node that runs another team as one step. The rules that keep nesting safe live here, as pure checks over the graphs on disk:
//! a team cannot include itself, directly or through others, and nesting has a depth limit (default 3). The inner team's runs, ledger rows and audits stay with the
//! inner team; the outer node records the child run id and takes the child's cost as its own.
use crate::team_graph::{slug, Graph, Kind};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// How deep teams may nest unless a node says less: team A includes B includes C includes D is three levels.
pub const DEFAULT_MAX_DEPTH: u32 = 3;
/// No node may ask for more than this.
pub const HARD_MAX_DEPTH: u32 = 5;

/// The sub-team nodes of a graph: (node id, team, the node's own depth limit).
pub fn subteams(g: &Graph) -> Vec<(String, String, Option<u32>)> {
    g.nodes.iter().filter_map(|(id, n)| match &n.kind { Kind::SubTeam { team, max_depth, .. } => Some((id.clone(), team.clone(), *max_depth)), _ => None }).collect()
}

fn walk(root: &str, g: &Graph, chain: &mut Vec<String>, limit: u32) -> Result<(), String> {
    chain.push(slug(&g.team));
    for (nid, team, md) in subteams(g) {
        let child = slug(&team);
        if chain.contains(&child) { return Err(format!("{}: {} would include itself ({} -> {child}): a team cannot include itself, directly or through others", g.team, nid, chain.join(" -> "))); }
        let lim = md.map_or(limit, |d| d.min(limit));
        if chain.len() as u32 > lim { return Err(format!("{}: {nid} nests {} levels deep and the limit is {lim}", g.team, chain.len())); }
        let cg = crate::team_graph_run::load_graph(root, &team).map_err(|_| format!("{}: {nid} runs a team called `{team}`, and there is none", g.team))?;
        walk(root, &cg, chain, lim)?;
    }
    chain.pop();
    Ok(())
}

/// Is this graph safe to run with the teams that exist? Names the first problem.
pub fn check(root: &str, g: &Graph) -> Result<(), String> { walk(root, g, &mut vec![], DEFAULT_MAX_DEPTH) }

/// What the outer node writes: the child run id (always), the team, and the outputs mapped out of the child run's values (`node.field`; a missing one is null).
pub fn result(child_run: &str, team: &str, outputs: &BTreeMap<String, String>, child_values: &Value) -> Value {
    let mut out = Map::new();
    for (name, path) in outputs { let (n, f) = path.split_once('.').unwrap_or((path.as_str(), "output")); out.insert(name.clone(), child_values.get(n).and_then(|v| v.get(f)).cloned().unwrap_or(Value::Null)); }
    json!({ "ok": true, "child_run": child_run, "team": team, "outputs": out })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_graph::{template, Node};
    use std::sync::atomic::{AtomicU64, Ordering};
    static NTH: AtomicU64 = AtomicU64::new(0);
    fn ws() -> String { let n = NTH.fetch_add(1, Ordering::SeqCst); let d = std::env::temp_dir().join(format!("antiloki-subteam-{}-{n}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d.to_string_lossy().into_owned() }
    fn sub(team: &str, max_depth: Option<u32>) -> Node { Node::new(Kind::SubTeam { team: team.into(), ask: "do it".into(), outputs: BTreeMap::new(), budget_share: None, max_depth }) }
    /// a team that includes the given teams
    fn team(root: &str, name: &str, includes: &[&str]) -> Graph {
        let mut g = template("blank", name, 2.0).unwrap();
        for (i, t) in includes.iter().enumerate() { g.nodes.insert(format!("s{i}"), sub(t, None)); }
        crate::team_graph_run::save_graph(root, &g).unwrap(); g
    }

    #[test]
    fn a_team_cannot_include_itself_directly_or_through_others() {
        let root = ws();
        // directly: the validator says it without looking at the disk
        let a = team(&root, "Alpha", &["Alpha"]);
        assert!(crate::team_graph::validate(&a).has("subteam_cycle"));
        assert!(check(&root, &a).unwrap_err().contains("include itself"));
        // through others: A -> B -> C -> A
        team(&root, "Beta", &["Gamma"]); team(&root, "Gamma", &["Alpha"]);
        let a = team(&root, "Alpha", &["Beta"]);
        let e = check(&root, &a).unwrap_err(); assert!(e.contains("include itself") && e.contains("alpha -> beta -> gamma -> alpha"), "{e}");
        // the same team used twice side by side is not a cycle
        let root2 = ws(); team(&root2, "Leaf", &[]); team(&root2, "Mid", &["Leaf", "Leaf"]); let top = team(&root2, "Top", &["Mid", "Leaf"]);
        assert!(check(&root2, &top).is_ok(), "{:?}", check(&root2, &top));
        // a team that is not there is said, not a panic
        let lost = team(&root2, "Lost", &["Nowhere"]); assert!(check(&root2, &lost).unwrap_err().contains("there is none"));
        assert!(crate::team_graph::validate(&team(&root2, "Empty", &[""])).has("subteam_team"));
    }

    #[test]
    fn the_depth_limit_is_enforced() {
        let root = ws();
        // T0 -> T1 -> T2 -> T3: three levels, allowed
        team(&root, "T3", &[]); team(&root, "T2", &["T3"]); team(&root, "T1", &["T2"]); let t0 = team(&root, "T0", &["T1"]);
        assert!(check(&root, &t0).is_ok(), "{:?}", check(&root, &t0));
        // one more level is too deep
        team(&root, "T4", &[]); team(&root, "T3", &["T4"]);
        let e = check(&root, &t0).unwrap_err(); assert!(e.contains("levels deep") && e.contains("limit is 3"), "{e}");
        // a node may ask for less, never more
        let root2 = ws(); team(&root2, "B2", &[]); team(&root2, "B1", &["B2"]);
        let mut top = template("blank", "B0", 2.0).unwrap(); top.nodes.insert("s".into(), sub("B1", Some(1))); crate::team_graph_run::save_graph(&root2, &top).unwrap();
        assert!(check(&root2, &top).unwrap_err().contains("limit is 1"), "{:?}", check(&root2, &top));
        let mut bad = top.clone(); bad.nodes.insert("s".into(), sub("B1", Some(HARD_MAX_DEPTH + 1))); assert!(crate::team_graph::validate(&bad).has("subteam_depth"));
        let mut zero = top.clone(); zero.nodes.insert("s".into(), sub("B1", Some(0))); assert!(crate::team_graph::validate(&zero).has("subteam_depth"));
    }

    #[test]
    fn the_child_run_id_is_recorded_on_the_outer_node() {
        let outputs: BTreeMap<String, String> = [("price".to_string(), "extract.price".to_string()), ("gone".to_string(), "ghost.x".to_string()), ("whole".to_string(), "extract".to_string())].into_iter().collect();
        let vals = json!({ "extract": { "price": 18, "output": "raw" } });
        let r = result("r-42", "Pricing", &outputs, &vals);
        assert_eq!(r["child_run"], json!("r-42"), "the outer node says which run it started"); assert_eq!(r["team"], json!("Pricing")); assert_eq!(r["ok"], json!(true));
        assert_eq!(r["outputs"]["price"], json!(18)); assert_eq!(r["outputs"]["gone"], Value::Null, "an output the inner run did not write is null, not an error"); assert_eq!(r["outputs"]["whole"], json!("raw"), "a bare node name reads its `output`");
        // and a sub-team node's values carry the id for whoever reads the run (the audit, the Improver, the person)
        let mut s = crate::team_graph::RunState::new(); let mut g = template("blank", "Outer", 2.0).unwrap(); g.edges.clear(); g.start = vec!["sub".into()]; g.nodes.insert("sub".into(), sub("Pricing", None)); g.edges.push(crate::team_graph::Edge { from: "sub".into(), port: "out".into(), to: "deliver".into() });
        let a = crate::team_graph::graph_step(&g, &mut s, crate::team_graph::Event::Start { now_ms: 0, seed: Map::new() }).unwrap(); assert!(a.contains(&crate::team_graph::Action::Run { node: "sub".into() }), "a sub-team node is a step that runs: {a:?}");
        let a = crate::team_graph::graph_step(&g, &mut s, crate::team_graph::Event::NodeDone { node: "sub".into(), outputs: r.clone(), cost_usd: 0.05 }).unwrap();
        assert_eq!(s.values["sub"]["child_run"], json!("r-42")); assert!(a.contains(&crate::team_graph::Action::Deliver), "{a:?}"); assert!((s.spent_usd - 0.05).abs() < 1e-9, "the child's cost is the node's cost");
    }
}
