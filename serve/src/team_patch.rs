//! Graph versioning and JSON patches (RFC 0011 §17). Propose, checkpointed apply, rollback.
use crate::team_graph::{self, diff as graph_diff, validate, Graph, Kind};
use crate::team_graph_run::{graph_hash, load_graph, save_graph};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const VERSION_KEEP: usize = 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PatchOp {
    AddNode { id: String, node: Value },
    RemoveNode { id: String },
    SetField { node: String, path: String, value: Value },
    AddEdge { from: String, #[serde(default = "out_port")] port: String, to: String },
    RemoveEdge { from: String, #[serde(default = "out_port")] port: String, to: String },
    SetGraphField { field: String, value: Value },
    Rename { from: String, to: String },
}

fn out_port() -> String { "out".into() }

#[derive(Clone, Default)]
pub struct CheckpointHooks {
    pub seat_trial: Option<fn(&Graph, &[PatchOp]) -> Result<(), String>>,
    pub cell_trial: Option<fn(&Graph, &[PatchOp]) -> Result<(), String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProposeOut {
    pub proposal_id: i64,
    pub requires_person: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyOut {
    pub version_id: i64,
    pub graph_hash: String,
    pub requires_person: bool,
}

/// Node or graph keys that need the person's yes before an agent may apply.
/// Each op is judged against the graph AS THE EARLIER OPS LEFT IT, so a node a patch adds (or renames) is judged for what it is, not as a stranger.
pub fn patch_requires_person(ops: &[PatchOp], base: &Graph) -> bool {
    let mut cur = base.clone();
    for op in ops {
        if op_requires_person(op, &cur) { return true; }
        if let Ok(next) = apply_ops(&cur, std::slice::from_ref(op)) { cur = next; }
    }
    false
}

/// Does going from `a` to `b` add, remove or change a gate (a Check, a Human gate, a budget gate), the budget, the triggers or the limits? Rollback uses it: undoing a person's gate is a gate change too.
pub fn gates_differ(a: &Graph, b: &Graph) -> bool {
    let (ja, jb) = (a.to_json(), b.to_json());
    for k in ["budget_usd", "triggers", "limits", "setpoints"] { if ja.get(k) != jb.get(k) { return true; } }
    let gates = |g: &Graph| -> std::collections::BTreeMap<String, Value> {
        let j = g.to_json();
        g.nodes.iter().filter(|(_, n)| node_kind_sensitive(&n.kind)).map(|(id, _)| (id.clone(), j["nodes"][id].clone())).collect()
    };
    gates(a) != gates(b)
}

fn op_requires_person(op: &PatchOp, base: &Graph) -> bool {
    match op {
        PatchOp::AddNode { node, .. } => value_is_sensitive(node),
        PatchOp::RemoveNode { id } => base.nodes.get(id).map(|n| node_kind_sensitive(&n.kind)).unwrap_or(false),
        PatchOp::SetField { node, path, .. } => {
            let p = path.to_ascii_lowercase();
            if p.contains("budget") || p == "triggers" || p == "limits" || p == "setpoints" || p.contains("consent") { return true; }
            base.nodes.get(node).map_or(value_path_sensitive(&p), |n| sensitive_field_on_node(&n.kind, &p))
        }
        PatchOp::SetGraphField { field, .. } => matches!(field.as_str(), "budget_usd" | "triggers" | "limits" | "setpoints"),
        PatchOp::AddEdge { from, to, .. } | PatchOp::RemoveEdge { from, to, .. } => {
            base.nodes.get(from).map_or(false, |n| node_kind_sensitive(&n.kind))
                || base.nodes.get(to).map_or(false, |n| node_kind_sensitive(&n.kind))
        }
        PatchOp::Rename { from, .. } => base.nodes.get(from).map(|n| node_kind_sensitive(&n.kind)).unwrap_or(false),
    }
}

fn value_is_sensitive(v: &Value) -> bool {
    let k = v.get("node").and_then(Value::as_str).unwrap_or("");
    matches!(k, "check" | "human" | "budget_gate") || v.get("rules").is_some() || v.get("gate").is_some()
}

fn node_kind_sensitive(kind: &Kind) -> bool {
    matches!(kind, Kind::Check { .. } | Kind::Human { .. } | Kind::BudgetGate { .. })
}

fn sensitive_field_on_node(kind: &Kind, path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    match kind {
        Kind::Check { .. } => true,
        Kind::Human { .. } => p.contains("gate") || p.contains("timeout"),
        Kind::Role { .. } => p.contains("cap_usd") || p.contains("budget"),
        Kind::Llm(_) => p.contains("seat") || p.contains("budget") || p.starts_with("verify") || p.starts_with("agree_with") || p.starts_with("fallback"),
        _ => p.contains("budget"),
    }
}

fn value_path_sensitive(path: &str) -> bool {
    path.contains("budget") || path.contains("trigger") || path.contains("limit") || path.contains("consent")
}

pub fn apply_ops(base: &Graph, ops: &[PatchOp]) -> Result<Graph, String> {
    let mut g = base.clone();
    for op in ops {
        match op {
            PatchOp::AddNode { id, node } => {
                if g.nodes.contains_key(id) { return Err(format!("a node called {id} is already there")); }
                let n: team_graph::Node = serde_json::from_value(node.clone()).map_err(|e| format!("add_node {id}: {e}"))?;
                g.nodes.insert(id.clone(), n);
            }
            PatchOp::RemoveNode { id } => {
                if !g.nodes.remove(id).is_some() { return Err(format!("no node called {id}")); }
                g.edges.retain(|e| e.from != *id && e.to != *id);
            }
            PatchOp::SetField { node, path, value } => {
                let mut v = g.to_json();
                let nodes = v.get_mut("nodes").and_then(|x| x.as_object_mut()).ok_or("nodes missing")?;
                let entry = nodes.get_mut(node).ok_or_else(|| format!("no node called {node}"))?;
                set_at_path(entry, path, value.clone())?;
                g = Graph::from_json(&v)?;
            }
            PatchOp::AddEdge { from, port, to } => {
                if !g.nodes.contains_key(from) || !g.nodes.contains_key(to) { return Err("edge: unknown node".into()); }
                let e = team_graph::Edge { from: from.clone(), port: port.clone(), to: to.clone() };
                if !g.edges.iter().any(|x| x.from == e.from && x.port == e.port && x.to == e.to) { g.edges.push(e); }
            }
            PatchOp::RemoveEdge { from, port, to } => {
                g.edges.retain(|e| !(e.from == *from && e.port == *port && e.to == *to));
            }
            PatchOp::SetGraphField { field, value } => {
                let mut v = g.to_json();
                let o = v.as_object_mut().ok_or("graph is not an object")?;
                o.insert(field.clone(), value.clone());
                g = Graph::from_json(&v)?;
            }
            PatchOp::Rename { from, to } => {
                if from == to { continue; }
                let n = g.nodes.remove(from).ok_or_else(|| format!("no node called {from}"))?;
                if g.nodes.contains_key(to) { return Err(format!("a node called {to} is already there")); }
                g.nodes.insert(to.clone(), n);
                for e in &mut g.edges {
                    if e.from == *from { e.from = to.clone(); }
                    if e.to == *from { e.to = to.clone(); }
                }
            }
        }
    }
    Ok(g)
}

fn set_at_path(v: &mut Value, path: &str, value: Value) -> Result<(), String> {
    let parts: Vec<&str> = path.split('.').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() { return Err("empty path".into()); }
    let mut cur = v;
    for (i, p) in parts.iter().enumerate() {
        if i + 1 == parts.len() {
            if let Some(o) = cur.as_object_mut() { o.insert((*p).to_string(), value); return Ok(()); }
            return Err(format!("cannot set {path}"));
        }
        let next = cur.get_mut(*p).ok_or_else(|| format!("no field {p} in {path}"))?;
        cur = next;
    }
    Err(format!("cannot set {path}"))
}

fn patch_touches_seat_or_cell(ops: &[PatchOp]) -> bool {
    ops.iter().any(|op| match op {
        PatchOp::SetField { path, .. } => {
            let p = path.to_ascii_lowercase();
            p.contains("seat") || p.contains("model") || p.contains("tier") || p.contains("cell")
        }
        PatchOp::AddNode { node, .. } => node.get("node") == Some(&json!("llm")) || node.get("seat").is_some(),
        _ => false,
    })
}

fn run_checkpoints(root: &str, g: &Graph, ops: &[PatchOp], hooks: &CheckpointHooks) -> Result<(), String> {
    let rep = validate(g);
    if !rep.ok() { return Err(format!("checkpoint validator: {}", rep.words().join("; "))); }
    let skill = crate::team_skills::report_for(root, g, false);
    if !skill.ok() { return Err(format!("checkpoint validator: {}", skill.words().join("; "))); }
    if patch_touches_seat_or_cell(ops) {
        if let Some(f) = hooks.seat_trial {
            f(g, ops).map_err(|e| format!("checkpoint seat_trial: {e}"))?;
        }
        if let Some(f) = hooks.cell_trial {
            f(g, ops).map_err(|e| format!("checkpoint cell_trial: {e}"))?;
        }
    }
    Ok(())
}

pub fn diff_nodes(old: &Graph, new: &Graph) -> Value {
    let d = graph_diff(old, new);
    json!({
        "added": d["nodes_added"],
        "removed": d["nodes_removed"],
        "changed": d["nodes_changed"],
        "wires_added": d["wires_added"],
        "wires_removed": d["wires_removed"],
        "budget": d["budget"],
    })
}

pub fn propose(root: &str, team: &str, ops: &[PatchOp], author: &str, reason: &str) -> Result<ProposeOut, String> {
    let base = load_graph(root, team)?;
    let requires_person = patch_requires_person(ops, &base);
    let patch_json = serde_json::to_string(ops).map_err(|e| e.to_string())?;
    let id = crate::ledger::write_proposal(root, team, author, &patch_json, reason)?;
    Ok(ProposeOut { proposal_id: id, requires_person })
}

/// Make the newest version row say what is live. The first time there is no row at all (so the very first patch can be undone); later a person's own save or edit may have moved the graph since the last row, and that state is kept as a row too.
fn sync_head(root: &str, team: &str, live: &Graph) -> Result<i64, String> {
    let head = crate::ledger::latest_version_id(root, team);
    if let Some(id) = head {
        if crate::ledger::read_version(root, team, id).map(|r| r.graph_hash == graph_hash(live)).unwrap_or(false) { return Ok(id); }
    }
    let reason = if head.is_none() { "initial" } else { "changed outside a patch" };
    crate::ledger::write_version(root, team, head, live, reason, None, "system")
}

pub fn apply(
    root: &str,
    team: &str,
    ops: &[PatchOp],
    person_ok: bool,
    applied_by: &str,
    reason: &str,
    proposal_id: Option<i64>,
    by_agent: bool,
    hooks: &CheckpointHooks,
) -> Result<ApplyOut, String> {
    let base = load_graph(root, team)?;
    let requires_person = patch_requires_person(ops, &base);
    if requires_person && by_agent {
        return Err("this patch needs the person's yes: an agent cannot apply it".into());
    }
    if requires_person && !person_ok {
        return Err("this patch needs the person's yes".into());
    }
    let next = apply_ops(&base, ops)?;
    run_checkpoints(root, &next, ops, hooks).map_err(|e| {
        if let Some(rest) = e.strip_prefix("checkpoint ") {
            // the name of the checkpoint AND why: the person cannot act on "the validator failed"
            if let Some((name, why)) = rest.split_once(':') { let why = why.trim(); return if why.is_empty() { format!("checkpoint failed: {name}") } else { format!("checkpoint failed: {name}: {}", why.chars().take(300).collect::<String>()) }; }
        }
        e
    })?;
    let parent = sync_head(root, team, &base)?;
    save_graph(root, &next)?;
    let vid = match crate::ledger::write_version(root, team, Some(parent), &next, reason, proposal_id, applied_by) {
        Ok(v) => v,
        Err(e) => { let _ = save_graph(root, &base); return Err(format!("the version could not be written, the graph is as it was: {e}")); }
    };
    if let Some(pid) = proposal_id { let _ = crate::ledger::mark_proposal(root, team, pid, "applied", applied_by); }
    let _ = crate::ledger::truncate_versions(root, team, VERSION_KEEP);
    Ok(ApplyOut { version_id: vid, graph_hash: graph_hash(&next), requires_person })
}

/// One step back, every time it is called: to the state before the newest change. History is kept: the rollback is a new row (`rollback to N`), never a delete, so the bad version and its proposal stay for whoever learns from them. A rollback that would remove or change a gate, the budget, the triggers or the limits is the person's to make.
pub fn rollback(root: &str, team: &str, applied_by: &str, by_agent: bool, person_ok: bool) -> Result<Graph, String> {
    let live = load_graph(root, team)?;
    let head_id = sync_head(root, team, &live)?;
    let head = crate::ledger::read_version(root, team, head_id)?;
    let current = match head.reason.strip_prefix("rollback to ").and_then(|n| n.trim().parse::<i64>().ok()) {
        Some(restored) => crate::ledger::read_version(root, team, restored).unwrap_or(head.clone()),
        None => head.clone(),
    };
    let parent = current.parent.ok_or("there is nothing earlier to go back to")?;
    let prow = crate::ledger::read_version(root, team, parent).map_err(|_| format!("the earlier version was dropped (only the last {VERSION_KEEP} are kept)"))?;
    let g: Graph = serde_json::from_str(&prow.graph).map_err(|e| format!("parent graph: {e}"))?;
    if gates_differ(&live, &g) {
        if by_agent { return Err("this rollback changes a gate, the budget, the triggers or the limits: an agent cannot do it, the person can".into()); }
        if !person_ok { return Err("this rollback changes a gate, the budget, the triggers or the limits and needs the person's yes".into()); }
    }
    save_graph(root, &g)?;
    crate::ledger::write_version(root, team, Some(head_id), &g, &format!("rollback to {parent}"), None, applied_by)?;
    let _ = crate::ledger::truncate_versions(root, team, VERSION_KEEP);
    Ok(g)
}

pub fn versions_list(root: &str, team: &str) -> Result<Vec<Value>, String> {
    Ok(crate::ledger::list_versions(root, team)?.into_iter().map(|r| json!({
        "id": r.id, "ts_ms": r.ts_ms, "parent": r.parent, "graph_hash": r.graph_hash,
        "reason": r.reason, "proposal": r.proposal, "applied_by": r.applied_by,
    })).collect())
}

pub fn inspect_version(root: &str, team: &str, version_id: Option<i64>) -> Result<Value, String> {
    if let Some(id) = version_id {
        let row = crate::ledger::read_version(root, team, id)?;
        let g: Graph = serde_json::from_str(&row.graph).map_err(|e| e.to_string())?;
        return Ok(json!({ "ok": true, "version": id, "graph": g.to_json(), "meta": row.meta_json() }));
    }
    let g = load_graph(root, team)?;
    Ok(json!({ "ok": true, "version": serde_json::Value::Null, "graph": g.to_json(), "graph_hash": graph_hash(&g) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_graph::{template, Node};
    use std::sync::atomic::{AtomicI64, Ordering};

    static NTH: AtomicI64 = AtomicI64::new(0);

    fn ws() -> (std::path::PathBuf, String, String) {
        let n = NTH.fetch_add(1, Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("antiloki-patch-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let team = "Patch team".into();
        (d.clone(), d.to_string_lossy().into_owned(), team)
    }

    fn seed(root: &str, team: &str) -> Graph {
        let g = template("blank", team, 4.0).unwrap();
        save_graph(root, &g).unwrap();
        g
    }

    fn ops(v: Value) -> Vec<PatchOp> { serde_json::from_value(v).unwrap() }

    #[test]
    fn a_patch_applies_every_op_add_node_remove_node_set_field_add_edge_remove_edge_set_graph_field_rename() {
        let (_d, root, team) = ws();
        let base = seed(&root, &team);
        let note = json!({ "node": "role", "role": "agent", "agent": { "title": "Note", "responsibility": "notes", "effects": ["document"] } });
        let patch = ops(json!([
            { "op": "add_node", "id": "note", "node": note },
            { "op": "add_edge", "from": "me", "port": "approved", "to": "note" },
            { "op": "set_field", "node": "note", "path": "agent.title", "value": "Scribe" },
            { "op": "rename", "from": "note", "to": "scribe" },
            { "op": "set_graph_field", "field": "budget_usd", "value": 5.0 },
            { "op": "remove_edge", "from": "me", "port": "approved", "to": "scribe" },
            { "op": "remove_node", "id": "scribe" },
        ]));
        let g = apply_ops(&base, &patch).unwrap();
        assert!(!g.nodes.contains_key("scribe"));
        assert!((g.budget_usd - 5.0).abs() < 1e-9);
    }

    #[test]
    fn an_applied_patch_writes_a_version_with_parent_hash_reason_and_proposal() {
        let (_d, root, team) = ws();
        let _ = seed(&root, &team);
        let patch = ops(json!([{ "op": "set_field", "node": "me", "path": "label", "value": "tagged" }]));
        let prop = propose(&root, &team, &patch, "leader", "add tag").unwrap();
        let out = apply(&root, &team, &patch, true, "person", "add tag", Some(prop.proposal_id), false, &CheckpointHooks::default()).unwrap();
        let row = crate::ledger::read_version(&root, &team, out.version_id).unwrap();
        assert_eq!(row.reason, "add tag");
        assert_eq!(row.proposal, Some(prop.proposal_id));
        assert_eq!(row.graph_hash, out.graph_hash);
        assert_eq!(row.applied_by, "person");
        let init = crate::ledger::read_version(&root, &team, row.parent.expect("the first apply has the initial state as its parent")).unwrap();
        assert_eq!(init.reason, "initial");
    }

    #[test]
    fn a_failed_validator_stops_the_apply_and_names_the_checkpoint() {
        let (_d, root, team) = ws();
        let base = template("bugfix", &team, 4.0).unwrap();
        save_graph(&root, &base).unwrap();
        let patch = ops(json!([{ "op": "set_field", "node": "loop", "path": "max_iterations", "value": 0 }]));
        let err = apply(&root, &team, &patch, true, "person", "break loop", None, false, &CheckpointHooks::default()).unwrap_err();
        assert!(err.contains("validator"), "{err}");
        assert!(err.contains("checkpoint"), "{err}");
    }

    #[test]
    fn rollback_restores_the_parent_version_exactly() {
        let (_d, root, team) = ws();
        let base = seed(&root, &team);
        let p1 = ops(json!([{ "op": "set_field", "node": "me", "path": "label", "value": "after one" }]));
        apply(&root, &team, &p1, true, "p", "one", None, false, &CheckpointHooks::default()).unwrap();
        let mid = load_graph(&root, &team).unwrap();
        let p2 = ops(json!([{ "op": "set_field", "node": "me", "path": "label", "value": "after two" }]));
        apply(&root, &team, &p2, true, "p", "two", None, false, &CheckpointHooks::default()).unwrap();
        let back = rollback(&root, &team, "p", false, true).unwrap();
        assert_eq!(serde_json::to_string(&back.to_json()).unwrap(), serde_json::to_string(&mid.to_json()).unwrap());
        assert_eq!(serde_json::to_string(&load_graph(&root, &team).unwrap().to_json()).unwrap(), serde_json::to_string(&mid.to_json()).unwrap());
        let _ = base;
    }

    #[test]
    fn a_patch_touching_a_check_a_budget_a_human_gate_triggers_or_limits_requires_the_person() {
        let (_d, root, team) = ws();
        let base = seed(&root, &team);
        assert!(patch_requires_person(&ops(json!([{ "op": "add_node", "id": "c", "node": { "node": "check", "on": "artifact:x.md", "rules": [] } }])), &base));
        assert!(patch_requires_person(&ops(json!([{ "op": "set_graph_field", "field": "budget_usd", "value": 9.0 }])), &base));
        assert!(patch_requires_person(&ops(json!([{ "op": "set_field", "node": "me", "path": "gate", "value": "spend" }])), &base));
        assert!(patch_requires_person(&ops(json!([{ "op": "set_graph_field", "field": "triggers", "value": [{ "cron": "0 9 * * *", "ask": "go" }] }])), &base));
        assert!(patch_requires_person(&ops(json!([{ "op": "set_graph_field", "field": "limits", "value": { "max_runs_per_day": 1 } }])), &base));
    }

    #[test]
    fn a_requires_person_patch_is_refused_without_the_persons_yes() {
        let (_d, root, team) = ws();
        let _ = seed(&root, &team);
        let patch = ops(json!([{ "op": "set_graph_field", "field": "budget_usd", "value": 8.0 }]));
        let err = apply(&root, &team, &patch, false, "person", "raise", None, false, &CheckpointHooks::default()).unwrap_err();
        assert!(err.contains("person"), "{err}");
    }

    #[test]
    fn an_agent_pane_cannot_apply_a_requires_person_patch() {
        let (_d, root, team) = ws();
        let _ = seed(&root, &team);
        let patch = ops(json!([{ "op": "set_graph_field", "field": "budget_usd", "value": 8.0 }]));
        let err = apply(&root, &team, &patch, true, "agent:7", "raise", None, true, &CheckpointHooks::default()).unwrap_err();
        assert!(err.contains("agent"), "{err}");
    }

    #[test]
    fn only_the_last_k_versions_are_kept() {
        let (_d, root, team) = ws();
        let _ = seed(&root, &team);
        for i in 0..25 {
            let patch = ops(json!([{ "op": "set_field", "node": "me", "path": "label", "value": format!("v{i}") }]));
            let _ = apply(&root, &team, &patch, true, "p", "fill", None, false, &CheckpointHooks::default()).unwrap();
        }
        let rows = crate::ledger::list_versions(&root, &team).unwrap();
        assert!(rows.len() <= VERSION_KEEP, "kept {} rows", rows.len());
    }

    #[test]
    fn diff_lists_added_removed_and_changed_nodes() {
        let (_d, root, team) = ws();
        let old = seed(&root, &team);
        let patch = ops(json!([
            { "op": "add_node", "id": "extra", "node": { "node": "deliver" } },
            { "op": "set_field", "node": "me", "path": "gate", "value": "ship" },
        ]));
        let new = apply_ops(&old, &patch).unwrap();
        let d = diff_nodes(&old, &new);
        assert!(d["added"].as_array().unwrap().iter().any(|x| x == "extra"));
        assert!(d["changed"].as_array().unwrap().iter().any(|x| x == "me"));
        assert_eq!(d["removed"].as_array().unwrap().len(), 0);
    }


    fn label(root: &str, team: &str) -> String { load_graph(root, team).unwrap().to_json()["nodes"]["me"]["label"].as_str().unwrap_or("").to_string() }
    fn set_label(root: &str, team: &str, v: &str) { apply(root, team, &ops(json!([{ "op": "set_field", "node": "me", "path": "label", "value": v }])), true, "p", v, None, false, &CheckpointHooks::default()).unwrap(); }

    #[test]
    fn the_first_patch_can_be_rolled_back_to_the_original() {
        let (_d, root, team) = ws();
        let base = seed(&root, &team);
        set_label(&root, &team, "one");
        let back = rollback(&root, &team, "p", false, true).unwrap();
        assert_eq!(back.to_json(), base.to_json());
    }

    #[test]
    fn rolling_back_twice_goes_two_steps_and_no_version_is_deleted() {
        let (_d, root, team) = ws();
        let base = seed(&root, &team);
        set_label(&root, &team, "one"); set_label(&root, &team, "two"); set_label(&root, &team, "three");
        let before = crate::ledger::list_versions(&root, &team).unwrap().len();
        rollback(&root, &team, "p", false, true).unwrap();
        assert_eq!(label(&root, &team), "two");
        rollback(&root, &team, "p", false, true).unwrap();
        assert_eq!(label(&root, &team), "one");
        rollback(&root, &team, "p", false, true).unwrap();
        assert_eq!(load_graph(&root, &team).unwrap().to_json(), base.to_json());
        assert!(rollback(&root, &team, "p", false, true).is_err(), "nothing is earlier than the original");
        let rows = crate::ledger::list_versions(&root, &team).unwrap();
        assert_eq!(rows.len(), before + 3, "rollbacks add rows, they delete none");
        assert!(rows.iter().any(|r| r.reason == "three"), "the undone version is still there");
    }

    #[test]
    fn a_change_after_a_rollback_can_itself_be_rolled_back() {
        let (_d, root, team) = ws();
        let _ = seed(&root, &team);
        set_label(&root, &team, "one"); set_label(&root, &team, "two");
        rollback(&root, &team, "p", false, true).unwrap();
        set_label(&root, &team, "three");
        rollback(&root, &team, "p", false, true).unwrap();
        assert_eq!(label(&root, &team), "one");
    }

    #[test]
    fn a_save_made_outside_a_patch_is_kept_as_a_version_and_rollback_does_not_lose_it() {
        let (_d, root, team) = ws();
        let _ = seed(&root, &team);
        set_label(&root, &team, "one");
        let mut g = load_graph(&root, &team).unwrap().to_json();
        g["nodes"]["me"]["label"] = json!("by hand");
        save_graph(&root, &Graph::from_json(&g).unwrap()).unwrap();
        set_label(&root, &team, "two");
        rollback(&root, &team, "p", false, true).unwrap();
        assert_eq!(label(&root, &team), "by hand", "the person's own edit is what the rollback returns to");
    }

    #[test]
    fn an_agent_cannot_roll_back_a_gate_or_the_budget_but_the_person_can() {
        let (_d, root, team) = ws();
        let _ = seed(&root, &team);
        apply(&root, &team, &ops(json!([{ "op": "set_graph_field", "field": "budget_usd", "value": 9.0 }])), true, "person", "raise", None, false, &CheckpointHooks::default()).unwrap();
        set_label(&root, &team, "x");
        rollback(&root, &team, "person", false, true).unwrap();   // label only: fine
        let err = rollback(&root, &team, "agent:3", true, true).unwrap_err();   // would lower the budget the person raised
        assert!(err.contains("agent"), "{err}");
        assert!((load_graph(&root, &team).unwrap().budget_usd - 9.0).abs() < 1e-9);
        assert!(rollback(&root, &team, "person", false, false).unwrap_err().contains("yes"));
        rollback(&root, &team, "person", false, true).unwrap();
        assert!((load_graph(&root, &team).unwrap().budget_usd - 4.0).abs() < 1e-9);
    }

    #[test]
    fn a_node_added_earlier_in_the_same_patch_is_judged_for_what_it_is() {
        let (_d, root, team) = ws();
        let base = seed(&root, &team);
        let llm = json!({ "node": "llm", "seat": "endpoint:a:b", "prompt": "x", "answer": { "a": "string" } });
        assert!(!patch_requires_person(&ops(json!([{ "op": "add_node", "id": "c", "node": llm.clone() }])), &base));
        assert!(patch_requires_person(&ops(json!([{ "op": "add_node", "id": "c", "node": llm }, { "op": "set_field", "node": "c", "path": "seat", "value": "claude-code:haiku" }])), &base));
    }

    #[test]
    fn loosening_a_cells_guard_needs_the_person() {
        let (_d, root, team) = ws();
        let base = seed(&root, &team);
        let mut g = base.to_json();
        g["nodes"]["c"] = json!({ "node": "llm", "seat": "endpoint:a:b", "prompt": "x", "answer": { "a": "string" }, "verify": { "in_source": ["a"], "source": "{ask}" } });
        let with = Graph::from_json(&g).unwrap();
        assert!(patch_requires_person(&ops(json!([{ "op": "set_field", "node": "c", "path": "verify", "value": {} }])), &with));
        assert!(patch_requires_person(&ops(json!([{ "op": "set_field", "node": "c", "path": "agree_with", "value": [] }])), &with));
    }

    #[test]
    fn an_applied_proposal_is_marked_applied() {
        let (_d, root, team) = ws();
        let _ = seed(&root, &team);
        let patch = ops(json!([{ "op": "set_field", "node": "me", "path": "label", "value": "z" }]));
        let prop = propose(&root, &team, &patch, "leader", "z").unwrap();
        apply(&root, &team, &patch, true, "person", "z", Some(prop.proposal_id), false, &CheckpointHooks::default()).unwrap();
        assert_eq!(crate::ledger::proposal_status(&root, &team, prop.proposal_id).as_deref(), Some("applied"));
    }

    #[test]
    fn a_failed_checkpoint_says_which_one_and_why() {
        let (_d, root, team) = ws();
        let base = template("bugfix", &team, 4.0).unwrap(); save_graph(&root, &base).unwrap();
        let patch = ops(json!([{ "op": "set_field", "node": "loop", "path": "max_iterations", "value": 0 }]));
        let err = apply(&root, &team, &patch, true, "person", "break loop", None, false, &CheckpointHooks::default()).unwrap_err();
        assert!(err.starts_with("checkpoint failed: validator: "), "{err}");
        assert!(err.len() > "checkpoint failed: validator: ".len() + 10, "the reason is there: {err}");
        assert_eq!(load_graph(&root, &team).unwrap().to_json(), base.to_json(), "nothing was applied");
    }
}