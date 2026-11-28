//! THE RUBRIC (operator 2026-11-17: "a rubric / context doc so the AI knows all the possibilities of the team recipe; the first thing a leader reads before it creates or changes a team").
//! Two parts. THE REFERENCE is generated from the code (the node catalog, the tool types, the effects, the runners, the package entries, the failure ports), so it cannot go stale. THE RUBRIC is hand-written: the rules a team needs to work,
//! each one learned from a real run (the bible-video experiment of 2026-11-17 and the coder-team experiments of 2026-11-16). Served by the op `rubric` and the MCP tool `antiloki_team_rubric`.
use serde_json::Value;

/// The hand-written rules. Keep each one short, concrete and tied to the failure that taught it.
pub const RUBRIC: &str = r#"# HOW TO DESIGN OR CHANGE A TEAM — read this before you create or change anything

A team is a RECIPE: one file with every node's full configuration (seats, contract, tests, rules, tool arguments, budgets), kept in the team's own git. Every change you make is a version: say WHY (the reason, the evidence run), and the version keeps it with its cost, tokens and time. You change a recipe; you never edit the person's project to "fix" a team.

## The rules
1. DETERMINISM FIRST. Use a code step, a tool step or a support node wherever no model is needed. A model is for judgment and for writing, never for what code can do (counting, parsing, checking, rendering, fetching).
2. DIVIDE THE WORK BY DIFFICULTY. The leader (a frontier model) writes, ONCE and BEFORE the run: the contract, the tests, the review rules. The CODER that fills function bodies is a small local model. The plan of a task is frontier work; the typing is local work. Never put a frontier seat in the coder or in its ladder when the person asked for a local team.
3. THE CONTRACT IS FUNCTION-LEVEL. For every file: `exports`, a `purpose`, and a `functions` list, each with `signature`, `does`, `returns`, `examples` (call -> result) and `edge_cases`. A file-level contract (purpose and exports only) makes the planner invent its own API, and the run fails on a name nobody asked for. A small model needs the exact signature and examples; a sentence is not enough.
4. THE CONTRACT'S CONVENTIONS CARRY THE LESSONS. Put in `conventions` everything a small model gets wrong: the module system (a plain browser script has NO import and NO export; a global `function name(...)` at top level), the units (all times are SECONDS as plain numbers: 0.8 means 800 ms, never 800), the allowed APIs, the colours and fonts, and ONE worked example line of correct code. When a run fails, the fix usually belongs here.
5. JUDGES RUN AFTER THE WORK, NEVER BEFORE IT. `contract`, `test`, `review` and `skeleton` JUDGE files that exist: wire them as the judge of a `recurse`/`decompose` node or after the writing step. A judge placed before any code exists fails on "the file does not exist". `stubs` runs before the coder; the contract is DATA that `stubs`, `decompose` and `skeleton` read, not a step in the flow.
6. THE TESTS ARE WRITTEN BY THE LEADER, ONCE. A `test` node holds the test files; the coder never sees them and nobody weakens them during a run. Test what the person will see (structure, units, ranges, that something happens in the first second and after the last), and test with real tools for output files (ffprobe for a video, a parser for a file), never with the model's own claim.
7. EVERY FAILURE PORT IS WIRED. A node that can fail (`failed`, `timeout`, `verdict`) must lead somewhere: a `retry`, an `evolve`, a `stop` with a message, or the person. A failure with nothing wired stops the run and hands it to the person with no learning. The code-writing step (`recurse`) needs its failure wired too, not only the build and the render.
8. A RETRY LOOP NEEDS NEW INFORMATION. Feed the exact judge feedback (the failing test names and assertion text) into the next try. The same input twice gives the same failure; the ladder climbs to the next LOCAL rung only after the same failure repeats.
9. A GRAPH NAMES A TOOL, NEVER A COMMAND LINE. Tools come from a fixed list (below): a skill, a test runner by name, a package entry by name. If the thing you need is not on the list, say so; do not smuggle a shell string.
10. A PACKAGE IS PINNED AND APPROVED. A git source is pinned to its full 40-character commit, dependencies are `name==exact.version` wheels, install scripts are off, it lands in the run's folder, and the person approves each commit once. The package step runs ONE entry from the registry.
11. LOCAL MODELS: the coder is `local:qwen2.5-coder:3b`; a tiny helper is `local:qwen3:0.6b`. A model that "thinks" in its answer (Qwen3 4B) is not usable as a coder. Give a local model one file and one job at a time.
12. BUDGETS ARE CEILINGS. Set `budget_usd` for the run and `cap_usd` per step; local seats cost nothing but time, so give them generous `timeout_s` (a 3B coder takes minutes) and the paid ones tight caps.
13. THE PERSON'S GATES ARE THE PERSON'S. You cannot change a human gate, a budget ceiling, a check, a trigger or a limit by yourself: a change that touches one is a REQUEST the person approves. Everything else, you may change, with a reason.
14. CHANGE ONE THING AT A TIME AND SAY WHY. Name the failing node, quote the evidence from the run record, change the smallest part that explains it, and give the metric that should improve. A rewrite of the whole graph hides what worked.
15. MEASURE. Every run records time, tokens and price per node and per model; a change is judged by the next runs, not by how reasonable it sounds.
16. REUSE BEFORE YOU BUILD. `antiloki_team_branch` `list` first. A branch is a team with ports (a YAML file: the full settings of every node, typed inputs and outputs, requirements, a hash). LINK a proven branch (the shared, read-only file, tracked); CLONE it when you will change it (your own copy, with lineage); editing the nodes of a link turns it into a clone. When you build something that works, EXPORT it as a branch so the next team does not rebuild it.
17. A USED BRANCH IS GUEST CODE. It can only request limits, never raise the team's budget, gates or approvals; a package inside it needs the person's approval in this team; its model seats are requirements with defaults (map them with `seats` to what this machine has: a seat the machine lacks fails at run time, not at use) and its parameters are typed with defaults (set them with `params`). Connect its outputs (failure ports too) before you run: a branch has no leader and no delivery of its own.

## The shape of a recipe (read the generated reference below for every field)
- `nodes`: a map id -> node; each node has `node` (its kind) plus that kind's fields. `edges`: a list of {from, port, to}; `start`: the entry nodes. `budget_usd`: the ceiling per run.
- A tool step is `{"node":"use_tool","tool":{"type":<tool type>,"name":<name>,"args":{...}},"allow":{"effects":[...],"paths":[...],"skills":[...]},"outputs":{...}}`; it may do only the effects its allow-list names.
- Seats are written `local:<ollama tag>`, `claude-code:<model>`, `endpoint:<alias>`. A contract file looks like `{"src/a.js":{"exports":["f"],"purpose":"...","functions":[{"signature":"f(x)","does":"...","returns":"number","examples":["f(2) -> 4"],"edge_cases":["f(-1) -> 0"]}]}}`.
"#;

fn s(v: &Value) -> String { v.as_str().unwrap_or("").to_string() }

/// The generated reference: every node the palette offers (with its default form, the shape to copy), the tool types, the effects, the test runners, the package entries and the failure ports. Pure over the catalog.
pub fn reference(catalog: &Value) -> String {
    let mut out = String::from("\n# THE REFERENCE (generated from the code: every kind, field and list a recipe can use)\n\n## Nodes (id [family] what it is :: its default form: copy the shape, change the values)\n");
    for n in catalog["nodes"].as_array().cloned().unwrap_or_default() {
        let d = serde_json::to_string(&n["default"]).unwrap_or_default(); let d: String = d.chars().take(600).collect();
        let ports: Vec<String> = n["ports"].as_array().map(|a| a.iter().map(|p| p.as_str().map(String::from).unwrap_or_else(|| p["name"].as_str().unwrap_or("").to_string())).collect()).unwrap_or_default();
        out.push_str(&format!("- `{}` [{}] {}{} :: {}\n", s(&n["id"]), s(&n["family"]), s(&n["hint"]), if ports.is_empty() { String::new() } else { format!(" · ports: {}", ports.join(", ")) }, d));
    }
    let list = |k: &str| catalog[k].as_array().map(|a| a.iter().map(|x| s(x)).collect::<Vec<_>>().join(", ")).unwrap_or_default();
    out.push_str(&format!("\n## Tool types (`tool.type`)\n{}\n\n## Effects (an allow-list names the ones a step may do)\n{}\n\n## Failure ports (wire each one)\n{}\n\n## Templates\n{}\n", list("tool_types"), list("effects"), list("failure_ports"), catalog["templates"].as_array().map(|a| a.iter().map(|t| if t.is_string() { s(t) } else { s(&t["id"]) }).collect::<Vec<_>>().join(", ")).unwrap_or_default()));
    out.push_str(&format!("\n## Test runners (a `test` node names one: `runner`)\n{}\n", crate::team_test::RUNNERS.iter().map(|(n, c)| format!("- `{n}` runs `{c}`")).collect::<Vec<_>>().join("\n")));
    out.push_str(&format!("\n## Sound entries (a `sound` tool step names one: `tool.name`)\n- `cue_sheet` reads the template's `tl.addLabel(\"cue:<name>\", <number>)` beats and checks each against the tween after it\n- `mix` makes the effects, a fitted narration with a tiny local voice, a music bed and loudness, then checks the result and writes captions (.srt) from the heard words. A beat DESCRIBES its sound (`want`: the closest description in the kit wins, by similarity) or names one of {}; the music is a named `bed` ({}) or chosen for the topic's text (`want_from`). It needs the person's sound runtime and the embedding model (nomic-embed-text)\n", crate::team_sound::KIT.join(", "), crate::team_sound::BEDS.join(", ")));
    out.push_str(&format!("\n## Package entries (a `package` tool step names one: `tool.name`)\n{}\n", crate::team_package::ENTRIES.iter().map(|(n, m, a)| format!("- `{n}` runs `python -m {m} {}`", a.join(" "))).collect::<Vec<_>>().join("\n")));
    out
}

/// The whole document: the hand-written rubric, then the generated reference.
pub fn text() -> String { format!("{}{}", RUBRIC, reference(&crate::team_graph::catalog())) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_document_has_the_rules_and_a_reference_that_lists_what_the_code_offers() {
        let t = text();
        assert!(t.starts_with("# HOW TO DESIGN OR CHANGE A TEAM"), "the rules come first");
        for must in ["THE CONTRACT IS FUNCTION-LEVEL", "JUDGES RUN AFTER THE WORK", "EVERY FAILURE PORT IS WIRED", "all times are SECONDS"] { assert!(t.contains(must), "{must}"); }
        for must in ["`recurse`", "`decompose`", "`test`", "`contract`", "tool_package", "rea_video", "node_test", "Tool types", "package"] { assert!(t.contains(must), "the reference lists {must}"); }
        assert!(t.len() > 8_000 && t.len() < 80_000, "{} chars", t.len());
    }
}
