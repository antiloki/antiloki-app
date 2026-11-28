//! TEAM.md (RFC 0013 §1, step 3): how a team works, written from its graph, the same text every time for the same graph (no model).
//! It sits beside `team.recipe.toml` in the team's own repository. The part under `## Known traps` is the person's (or the improver's, with approval): a rewrite keeps it.
use crate::team_graph::{Graph, Kind};

const TRAPS: &str = "## Known traps";
const TRAPS_EMPTY: &str = "_None written yet. Add what went wrong and how to avoid it; the improver reads this first._";

fn kind_of(k: &Kind) -> String { serde_json::to_value(k).ok().and_then(|v| v.get("node").and_then(|n| n.as_str().map(String::from))).unwrap_or_else(|| "node".into()) }
fn one_line(s: &str) -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") }

/// The text under `## Known traps` in an earlier TEAM.md (empty when there is none or it is the placeholder).
pub fn traps_of(old: &str) -> String {
    let Some(i) = old.find(TRAPS) else { return String::new() };
    let t = old[i + TRAPS.len()..].trim().to_string();
    if t == TRAPS_EMPTY { String::new() } else { t }
}

/// What this team is for, in what order, and what each node does; `traps` is carried over from the previous file.
pub fn team_md(g: &Graph, traps: &str) -> String {
    let mut s = format!("# {}\n\nA team of {} nodes, budget ${:.2} per run. Generated from `team.recipe.toml`; edit the recipe, not this part.\n\n## Nodes\n", g.team, g.nodes.len(), g.budget_usd);
    for (id, n) in &g.nodes {
        let name = n.label.clone().unwrap_or_else(|| id.clone());
        let mut line = format!("- **{}** (`{}`, {})", name, id, kind_of(&n.kind));
        if let Kind::Role { agent: Some(a), model, .. } = &n.kind {
            if !a.responsibility.is_empty() { line += &format!(": {}", one_line(&a.responsibility)); }
            if !a.effects.is_empty() { line += &format!(" · may {}", a.effects.join(", ")); }
            if let Some(m) = model { line += &format!(" · model {m}"); }
        }
        if let Some(w) = &n.why { line += &format!(" · why: {}{}", w.kind, if w.note.is_empty() { String::new() } else { format!(" — {}", one_line(&w.note)) }); }
        if let Some(c) = n.cap_usd { line += &format!(" · cap ${c:.2}"); }
        if let Some(a) = &n.autonomy { if a != "auto" { line += &format!(" · asks ({a})"); } }
        if n.approval.is_some() { line += " · needs your approval"; }
        s += &line; s.push('\n');
    }
    s += "\n## Order\n";
    if g.edges.is_empty() { s += "_No connections yet._\n"; }
    for e in &g.edges { s += &format!("- `{}` → `{}`{}\n", e.from, e.to, if e.port == "out" { String::new() } else { format!(" (on {})", e.port) }); }
    if !g.triggers.is_empty() { s += &format!("\n## Starts\n{} trigger(s) start this team without being asked.\n", g.triggers.len()); }
    s += &format!("\n{TRAPS}\n{}\n", if traps.trim().is_empty() { TRAPS_EMPTY } else { traps.trim() });
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_graph::template;
    #[test] fn the_notes_name_every_node_and_keep_the_traps_through_a_rewrite() {
        let g = template("blank", "Demo", 2.0).unwrap();
        let a = team_md(&g, ""); assert_eq!(a, team_md(&g, ""), "the same graph gives the same text");
        for id in g.nodes.keys() { assert!(a.contains(&format!("`{id}`")), "{id} is listed"); }
        assert!(a.contains("# Demo") && a.contains("$2.00") && a.contains(TRAPS_EMPTY) && traps_of(&a).is_empty());
        let mut with = a.replace(TRAPS_EMPTY, "- the reviewer times out on large diffs"); with.push('\n');
        assert_eq!(traps_of(&with), "- the reviewer times out on large diffs");
        let mut g2 = g.clone(); g2.budget_usd = 5.0; let b = team_md(&g2, &traps_of(&with));
        assert!(b.contains("$5.00") && b.contains("reviewer times out") && !b.contains(TRAPS_EMPTY), "the trap survives a change to the graph");
    }
}
