//! pane-brief-md.rs — the PANE BRIEF block a code-agent pane reads first (AGENTS.md / CLAUDE.md),
//! and the marker-delimited upsert that keeps the repo's own file intact below it. NO JS TWIN.
//!
//! Extracted 2026-09-14 from cockpit-pane-add.rs so the same block can be rewritten from THREE
//! places instead of once at provision: `pane/add` (as before), `pane/context/bake` (the block
//! must list the pack it just wrote) and `pane/update` (a scope or guidance edit used to wait for a
//! "next boot" that never re-baked anything).
//!
//! The block gains a `CONTEXT PACK:` section: one line per materialised file with a one-line
//! summary, and the live MCP tool list when the pane has them. Blackboard context docs are inlined
//! verbatim ONLY when no pack materialises them (preset `none`) — otherwise they are paths too.
use serde_json::Value;
use std::path::Path;

pub const BRIEF_A: &str = "<!-- ANTILOKI-PANE-BRIEF -->";
pub const BRIEF_B: &str = "<!-- /ANTILOKI-PANE-BRIEF -->";

pub const MCP_TOOLS: &[(&str, &str)] = &[
    ("antiloki_context_index", "what is baked for you and how stale it is"),
    ("antiloki_file_review", "measured review of one file: weak axes, violations with lines, importers"),
    ("antiloki_folder_review", "the folder paper: roster, health, worst members, violations"),
    ("antiloki_check", "re-judge a file's current content against the laws BEFORE you finish"),
    ("antiloki_blast_radius", "who breaks transitively if a file changes, and which tests to run"),
    ("antiloki_tests_for", "which tests reach a folder's files"),
    ("antiloki_laws", "the ratified laws"),
    ("antiloki_reads", "what you have read so far: pack vs source vs tools"),
    ("antiloki_audit", "the audit trail of this box: every read / write / refused write / shell command, cross-checked against git"),
    ("antiloki_delegate", "DELEGATE: fan a goal out to parallel sub-agents in this box (a plan of tasks owning disjoint files, run in waves, each verified, one joined report) — for speed, isolation, accuracy"),
    ("antiloki_delegate_answer", "answer the screening gate: when a delegated run is awaiting-answers (the goal was under-specified), answer its blocking questions from your context — or {} for the defaults"),
    ("antiloki_delegate_status", "where a delegated run is: tasks, waves, verdicts"),
    ("antiloki_delegate_result", "wait for a delegated run and get its joined report"),
    ("antiloki_specialists", "the review specialists the operator made available to you (id · lens)"),
    ("antiloki_specialist", "one isolated review of a file or folder by a specialist lens {specialist, path, question?} — a second opinion on your subscription; cite it, don't paste it"),
    ("antiloki_usage", "your token / context / plan-window usage right now"),
    ("antiloki_handoff", "about to hit your usage limit? export this conversation + touched files to .antiloki/handoff/ so another engine continues"),
    ("antiloki_sagas", "the project's sagas (goals) and their episodes: who is on each, what it cost; read before you start, so you do not redo a done episode"),
    ("antiloki_saga_add", "start a saga: a titled goal that episodes hang under"),
    ("antiloki_episode_add", "add an episode (or a step of one) to a saga"),
    ("antiloki_episode_update", "move an episode on: status, assignee, body, title; the trail records that you did it"),
    ("antiloki_team_design", "the Leader's tool: write a team's graph (roles, control flow, tool nodes, budget) or start from a template; returns the validation, the worst-case cost and what changed"),
    ("antiloki_team_campaign", "parallel runs and A/B tests of a team toward one result, inside the machine's capacity and a budget for the whole result; compare arms by audit and cost"),
    ("antiloki_team_apps", "the web apps the person installed (MCP): which answer or need a sign-in, what a team needs from the person before its first run, open an app's page so they can sign in"),
    ("antiloki_team_audit", "the self-improving engine: every run is audited; read the audit messages sent to you, the scores by run and graph, and mark them handled once you decided"),
    ("antiloki_team_seats", "the Leader manages the models: what this machine has, whether a model fits a step (with measured evidence), register a local server, download a model with the person's yes, trial a seat on cases with traps"),
    ("antiloki_team_skill", "write the skills a team's custom agents need (drafts the person approves) or list them"),
    ("antiloki_team_artifacts", "list or read the documents a team run made that are not code: reports, dashboards"),
    ("antiloki_team_run", "start a team's pack (the ask goes to the planner); the pack lands only when the person approves"),
    ("antiloki_team_status", "a team's graph or a run: each node's state, cost and the latest events"),
    ("antiloki_team_node", "change one node of a team: model, tier, cap, scope, retries, conditions"),
    ("antiloki_team_budget", "move budget between nodes under the ceiling the person set"),
    ("antiloki_team_pause", "pause a team's run"), ("antiloki_team_resume", "resume a paused run"), ("antiloki_team_stop", "stop a run"),
    ("antiloki_team_request_approval", "ask the person to decide at a human node (nothing here approves)"),
    ("antiloki_team_simulate", "step a team's graph with stubbed outputs: the path, the passes and the worst case, at no cost"),
    ("antiloki_panes", "the panes open on the person's screen right now, which one is focused, which one is you"),
    ("antiloki_pane_open", "open a pane the person can SEE beside yours: a terminal (give `command` to run the tests or a dev server there), another agent (`engine`, `prompt`), or a Studio tool (notes, issues, preview)"),
    ("antiloki_pane_read", "what a terminal pane shows right now (its screen as text): read the output of what you ran there"),
    ("antiloki_pane_send", "type into a terminal you opened"),
    ("antiloki_pane_close", "close a pane you opened when you are done with it"),
    ("antiloki_pane_arrange", "reorganise the screen like herdr: move / swap / flip / zoom / focus / rename / resize panes (read antiloki_panes first: it shows the layout tree)"),
    ("antiloki_pane_view", "look inside any Studio pane (Sagas, Notes, Preview, Diff, Skills, Frames ...): what it says and every control in it, with refs"),
    ("antiloki_pane_act", "use any Studio pane the way a person does: click, type, select, press a key, scroll, in the pane's own UI (the effect is visible and returned)"),
    ("antiloki_look", "how the screen looks: terminal and pane opacity, the wallpaper's darkness, theme and wallpaper (no arguments reads it); never touches the desktop's own theme"),
    ("antiloki_themes", "the colour themes and wallpapers antiloki_look can wear"),
    ("antiloki_screen", "show or hide the sidebar, switch workspace (no arguments reports them)"),
    ("antiloki_worktrees", "every agent's box and the project's checkout: who is where, branch, changed files, ahead/behind: look before you touch shared files"),
    ("antiloki_ship", "ship your box: preview / commit / push / pr (the PR carries the QA verdicts); commit, push and pr only when the person asked"),
    ("antiloki_issues", "what is wrong here, measured: the latest judge run, code signals and graph smells; or one file's or folder's review: find work worth doing, check before you edit"),
    ("antiloki_qa", "test the running app like a person, in a QA pane the person watches live: steps (goto/click/type/expect/shot ...) -> failed steps, checks, console errors, screenshot paths to read; use it after a front-end change"),
    ("antiloki_threads", "a space holds threads, each a screen of its own with its own panes and agents: list them, create one, rename, switch, delete an empty one"),
    ("antiloki_teams", "link agents as a planner, a worker and a QA (any providers) and run a task through them: a plan with acceptance criteria, the work, the QA's verdict on each criterion, three attempts at most with only what failed fed back, then it stops for the person"),
    ("antiloki_spaces", "the spaces (workspaces) antiloki has, with files and health of each: every X-ray tool below takes `space` to be about another one than the open space"),
    ("antiloki_xray", "ANY X-ray feature of ANY space: reviews, the AST, graph, diagrams, and the page's other tabs (rules, system_diagram, design, data_stories, architecture_history, working_tree, telemetry ...); no `source` lists them"),
    ("antiloki_rules", "the Quality tab's deterministic rules of a space: list / show / add / edit / activate / deactivate / remove / reweight / except (detectors op says how to write one); the next rebuild applies it"),
    ("antiloki_xray_rebuild", "re-read a space's code into its atlas (the ↻ rebuild) so X-ray and a rule you just changed read current"),
    ("antiloki_xray_open", "open a pane showing exactly one part of X-ray for a space (system-diagram, import-graph, file, folder, ast, rules, constitution, authors, git-graph, design ...): the same widget X-ray draws; read it with antiloki_pane_view, operate it with antiloki_pane_act"),
    ("antiloki_sources", "the full catalog of antiIDE features you can query"),
    ("antiloki_query", "any antiIDE feature by source key: ast (full or partial: symbol / lines), git, file_history, authors, imports, subgraph, trace, neighborhood, diagram, tree, metrics, smells, signals, docs, paper, …"),
];

pub const CONTEXT_SKILL_DESCRIPTION: &str = "Use FIRST on any task in this worktree: how to read antiloki's measured context pack (.antiloki/context/) and when to call the antiloki MCP tools instead of re-reading source.";

/// `.claude/skills/antiloki-context/SKILL.md` (and `.agents/skills/…`), materialised beside the
/// library skills whenever the pane has a pack.
pub const CONTEXT_SKILL_MD: &str = "---\nname: antiloki context\ndescription: Use FIRST on any task in this worktree: how to read antiloki's measured context pack (.antiloki/context/) and when to call the antiloki MCP tools instead of re-reading source.\n---\n\n# antiloki context\n\nantiloki has already measured the code you were assigned. The results are on disk, deterministic, and cheaper than exploring:\n\n1. Read `.antiloki/context/INDEX.md` — it lists every baked file with a one-line summary and says how stale the pack is (judge snapshot vs worktree HEAD).\n2. For the files you will touch, read `.antiloki/context/files/<path with / as __>.md` — the weak axes with what drives them, every violated law with an exact line, who imports the file, whether a test reaches it, and whether the last change regressed it.\n3. `.antiloki/context/laws.md` is the ruleset your edits are judged against. Do not introduce a violation of a ratified law; fix the ones on lines you touch.\n4. `.antiloki/context/folders/<folder>.md` is the folder paper: roster, health, worst members, boundary. `graph/<folder>.json` is the runtime import graph (type-only imports excluded). `tests/<folder>.md` says which tests reach which files.\n\nWhen a pack file and the source disagree, the source is newer — say so, then trust the source.\n\n## Live tools (MCP server `antiloki`, when wired)\n\n- `antiloki_check {path}` — re-judge a file's CURRENT content against the laws. Run it on every file you edited before you report done; it tells you what you fixed and what you introduced.\n- `antiloki_file_review {path}` / `antiloki_folder_review {path}` — the same review as the pack, live.\n- `antiloki_blast_radius {paths}` — who breaks transitively if a file changes, and which tests to run first.\n- `antiloki_tests_for {path}` — the tests reaching a folder.\n- `antiloki_laws` — the ratified laws.\n- `antiloki_context_index` — the INDEX, live.\n- `antiloki_sources` — the catalog of EVERY antiIDE feature; `antiloki_query {source, args}` fetches any of them: `ast` (full, or partial with `symbol` / `lines` — includes the syntax tree), `imports`, `subgraph`, `trace`, `neighborhood`, `diagram`, `tree`, `git`, `file_history`, `authors`, `history`, `metrics`, `smells`, `entry_points`, `signals`, `coverage`, `docs`, `paper`, `source`, and the AI-backed `describe` / `explain_snippet` / `logic_spec`.\n\nPrefer the pack for what is already there; prefer the tools for a file the pack did not materialise (INDEX lists them) or after you changed something.\n\n## Delegation — you orchestrate sub-agents\n\nYou can fan work out to parallel sub-agents that run inside this same box under your scope: `antiloki_delegate {goal, tasks?: number, write?: boolean}`. antiloki plans the goal into tasks that OWN disjoint files, runs them in waves, verifies each against its acceptance, and hands you ONE joined report (`antiloki_delegate_result {id}` waits for it; `antiloki_delegate_status {id}` peeks). Decide yourself, without asking the operator, when it pays:\n- SPEED — the work splits into 2+ independent parts (several files, folders, or questions) → delegate instead of doing them one after another;\n- ISOLATION — parts must not touch each other's files, or a survey should not pollute your own context window → delegate and keep only the reports;\n- ACCURACY — you want an independent verification pass, or to compare two approaches → delegate both and judge the reports.\nDo NOT delegate a small, sequential or single-file task. While a run is live, keep working on what is yours; integrate the joined report when it arrives.\n";

fn strip_leading_ws(s: &str) -> String { s.trim_start_matches(|c: char| c.is_whitespace()).to_string() }
fn s(v: Option<&Value>) -> String { match v { Some(Value::String(x)) => x.clone(), Some(Value::Null) | None => String::new(), Some(o) => o.to_string() } }

/// NEVER LOSE THE USER'S FILE: only the marker-delimited block is rewritten; the file's own
/// content survives below it.
pub fn upsert_brief(file: &Path, brief_md: &str) {
    let mut orig = std::fs::read_to_string(file).unwrap_or_default();
    if let (Some(a), Some(b)) = (orig.find(BRIEF_A), orig.find(BRIEF_B)) {
        if b > a {
            let spliced = format!("{}{}", &orig[..a], &orig[b + BRIEF_B.len()..]);
            orig = strip_leading_ws(&spliced);
        }
    }
    let block = format!("{BRIEF_A}\n{brief_md}\n{BRIEF_B}\n");
    let content = if !orig.trim().is_empty() { format!("{block}\n{}", strip_leading_ws(&orig)) } else { block };
    let _ = std::fs::write(file, content);
}

/// Everything the block needs, gathered by the caller (add / bake / update).
pub struct BriefInput<'a> {
    pub extra_prompt_md: Option<&'a str>,
    pub scope: &'a [String],
    /// the pack's manifest (`.antiloki/context/manifest.json` shape) when one is baked
    pub manifest: Option<&'a Value>,
    /// blackboard `context` docs `{id, body_md}` — inlined only when `manifest` is None
    pub docs: &'a [Value],
    pub live_tools: bool,
    /// antiQA is wired into this pane (`settings.antiqa.enabled`, the button in the Heimdall menu): say what it is for
    pub antiqa: bool,
    /// the introduction to antiloki from the prompt catalog's active row (`agent.antiloki_intro`, SQLite, editable in Settings); `None` when it is not there yet: the short built-in rubric stands in
    pub intro: Option<&'a str>,
}

/// The CONTEXT PACK section alone — also what the Claude `--append-system-prompt` carries.
pub fn pack_section(manifest: &Value, live_tools: bool) -> String {
    let mut lines: Vec<String> = Vec::new();
    let items = manifest.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
    let written: Vec<&Value> = items.iter().filter(|i| i.get("written").and_then(Value::as_bool).unwrap_or(false)).collect();
    if !written.is_empty() {
        lines.push(format!(
            "CONTEXT PACK (generated by antiloki at {} — read .antiloki/context/INDEX.md FIRST, before exploring the tree; it is the measured view of the code you were assigned):",
            s(manifest.get("generated")),
        ));
        for it in written {
            lines.push(format!("- .antiloki/context/{} — {}", s(it.get("file")), s(it.get("summary"))));
        }
        let skipped: Vec<&Value> = items.iter().filter(|i| !i.get("written").and_then(Value::as_bool).unwrap_or(false)).collect();
        if !skipped.is_empty() {
            lines.push(format!("- not materialised (budget): {} — ask the live tools", skipped.iter().map(|i| format!("{} {}", s(i.get("kind")), s(i.get("path")))).collect::<Vec<_>>().join(", ")));
        }
        // WHERE THE INTEL STOPS (2026-10-01): the one-line version of INDEX.md's coverage section, so an agent that reads only the brief still hears it
        let cov = s(manifest.get("coverage").and_then(|c| c.get("brief_line")));
        if !cov.is_empty() { lines.push(cov); }
    }
    if live_tools {
        lines.push(format!(
            "LIVE TOOLS (MCP server `antiloki`): {}. Run antiloki_check on every file you edited before you report done.",
            MCP_TOOLS.iter().map(|(n, d)| format!("{n} ({d})")).collect::<Vec<_>>().join("; "),
        ));
        // WHERE THINGS GO (operator 2026-11-11: asked an agent for a mockup and it made a Claude artifact; the person has a Frames pane for it)
        lines.push("WHERE THINGS GO: a MOCKUP or screen design the person asks for goes into the Frames pane: antiloki_frame_create (you write ONE self-contained HTML document: inline CSS, no scripts, no network addresses) or antiloki_frame_ask (a design agent makes it). Never a Claude artifact, never only a file. Work worth tracking goes to a saga (antiloki_saga_add). To look at or click in a pane use antiloki_pane_view / antiloki_pane_act; do not drive the Frames pane's form to make a mockup, use the tools.".to_string());
    }
    lines.join("\n")
}

/// WHAT ANTIQA IS FOR, in the pane's own words (RFC 0001 §7.4/§7.5). It is an external QA tool the operator switched on: it plays user
/// stories against the running app and records evidence. The agent tests the FEATURES IT CHANGED, not the whole project, and never edits
/// the stories (the oracle): a failing story is a finding about the product, not a thing to rewrite.
pub fn antiqa_section() -> String {
    "ANTIQA (MCP server `antiqa`, switched on by the operator): after you change what a user can see or do, test it. antiqa_list_packs, then antiqa_list_stories (use `query` to find the stories that visit the screens and controls you touched), then antiqa_run_start with ONLY those story ids and the url of the running app, antiqa_run_wait, antiqa_get_report. Fix the items classified `defect`; the others are not product bugs. antiqa_rerun_story checks a fix: it counts only when the story passes there. Never edit antiQA's packs or stories to make a run pass, and say which stories ran, not that the project is clean.".to_string()
}

/// WHAT EVERY AGENT KNOWS ABOUT ANTILOKI, at creation (operator 2026-11-24: "all agents, specialists or not, should read at least a concise rubric about antiloki when created, so we do not need to explain things").
/// Found in a real run: asked to "create a calculator pane", an agent that had antiloki's tools used its own CLI's plugin-authoring skill instead, and no pane opened. Short on purpose: it says where things live and which door to use.
pub fn antiloki_rubric() -> String {
    "YOU ARE INSIDE ANTILOKI (a workspace app of threads, panes, teams and sagas; you are one agent in one pane). The person speaks in antiloki's words: when they ask for a PANE, a terminal, an agent, a team, a web app, a memory (saga) or a review, use antiloki's own tools (MCP server `antiloki`: antiloki_pane_open / antiloki_free_pane_open / antiloki_panes / antiloki_pane_send, antiloki_team_*, antiloki_sagas / antiloki_remember, antiloki_check / antiloki_file_review). Never answer with your own CLI's plugins, skills, mods, extra windows or background shells instead: those are not antiloki's panes and the person will not see them. If you are unsure which tool, call antiloki_app_tools or antiloki_sources first. Panes and teams belong to a thread: a team's thread holds only its own pane, so open others in an ordinary thread. If an antiloki tool is not available or fails, say so plainly; do not improvise a substitute. Do not ask the person to explain antiloki: look it up with the tools above.".to_string()
}

/// The whole block body (without markers).
pub fn brief_md(input: &BriefInput) -> String {
    let mut sections: Vec<String> = vec![
        "# PANE BRIEF (generated by antiloki — only this block is rewritten; the file's own content below is preserved)".to_string(),
        "".to_string(),
        "MISSION:".to_string(),
        "(none set)".to_string(), // the session row never carries mission_md into the pane brief — see cockpit-pane-add.rs module doc
        "".to_string(),
    ];
    if let Some(ep) = input.extra_prompt_md { if !ep.is_empty() { sections.push(format!("OPERATOR GUIDANCE:\n{ep}\n")); } }
    if !input.scope.is_empty() {
        sections.push(format!(
            "SCOPE (edit whitelist): you may EDIT only files matching: {}. You can READ the entire repo — use that context. Edits outside the whitelist WILL BE BLOCKED at integration.",
            input.scope.join(", ")
        ));
    }
    sections.push(match input.intro { Some(t) if !t.trim().is_empty() => t.trim().to_string(), _ => antiloki_rubric() }); sections.push(String::new());
    if input.antiqa { sections.push(String::new()); sections.push(antiqa_section()); }
    match input.manifest {
        Some(m) => {
            let sec = pack_section(m, input.live_tools);
            if !sec.is_empty() { sections.push(String::new()); sections.push(sec); }
        }
        None => {
            if input.live_tools {
                sections.push(String::new());
                sections.push(pack_section(&Value::Null, true));
            }
            for d in input.docs {
                let id = d.get("id").cloned().unwrap_or(Value::Null);
                let body = s(d.get("body_md"));
                sections.push(format!("\n--- CONTEXT DOC #{} ---\n{body}", s(Some(&id))));
            }
        }
    }
    sections.into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_brief_carries_the_short_antiloki_rubric_and_it_stays_short() {
        let r = antiloki_rubric(); assert!(r.contains("antiloki_pane_open") && r.contains("INSIDE ANTILOKI") && r.contains("Never answer with your own CLI's plugins"));
        assert!(r.len() < 1400, "a rubric every agent reads must stay concise ({} chars)", r.len());
    }
    #[test]
    fn the_catalog_introduction_replaces_the_built_in_rubric_when_there_is_one() {
        let mk = |intro| brief_md(&BriefInput { extra_prompt_md: None, scope: &[], manifest: None, docs: &[], live_tools: false, antiqa: false, intro });
        assert!(mk(Some("MY INTRO FROM SQLITE")).contains("MY INTRO FROM SQLITE") && !mk(Some("MY INTRO FROM SQLITE")).contains("INSIDE ANTILOKI"));
        assert!(mk(None).contains("INSIDE ANTILOKI") && mk(Some("  ")).contains("INSIDE ANTILOKI"));
    }
    #[test]
    fn upsert_rewrites_only_the_block() {
        let d = std::env::temp_dir().join(format!("antiloki-brief-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let f = d.join("AGENTS.md");
        std::fs::write(&f, "# my repo\nrules here\n").unwrap();
        upsert_brief(&f, "one");
        upsert_brief(&f, "two");
        let t = std::fs::read_to_string(&f).unwrap();
        assert_eq!(t, format!("{BRIEF_A}\ntwo\n{BRIEF_B}\n\n# my repo\nrules here\n"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn brief_lists_pack_paths_instead_of_inlining_docs() {
        let manifest = json!({ "generated": "2026-09-14T10:00:00Z", "items": [
            { "kind": "laws", "file": "laws.md", "summary": "17 ratified laws", "written": true },
            { "kind": "file", "path": "src/a.ts", "file": "files/src__a.ts.md", "summary": "file review 42/100", "written": true },
            { "kind": "file", "path": "src/b.ts", "file": "files/src__b.ts.md", "summary": "", "written": false },
        ]});
        let docs = vec![json!({ "id": 3, "body_md": "secret plan" })];
        let md = brief_md(&BriefInput { extra_prompt_md: Some("be nice"), scope: &["src/**".to_string()], manifest: Some(&manifest), docs: &docs, live_tools: true, antiqa: false, intro: None });
        assert!(md.contains("OPERATOR GUIDANCE:\nbe nice"));
        assert!(md.contains("SCOPE (edit whitelist): you may EDIT only files matching: src/**."));
        assert!(md.contains("CONTEXT PACK (generated by antiloki at 2026-09-14T10:00:00Z — read .antiloki/context/INDEX.md FIRST"));
        assert!(md.contains("- .antiloki/context/files/src__a.ts.md — file review 42/100"));
        assert!(md.contains("- not materialised (budget): file src/b.ts — ask the live tools"));
        assert!(md.contains("LIVE TOOLS (MCP server `antiloki`): antiloki_context_index"));
        assert!(!md.contains("secret plan"), "docs are in the pack, not inlined");

        let md = brief_md(&BriefInput { extra_prompt_md: None, scope: &[], manifest: None, docs: &docs, live_tools: false, antiqa: false, intro: None });
        assert!(md.contains("--- CONTEXT DOC #3 ---\nsecret plan"), "no pack → docs inline, as before");
        assert!(!md.contains("CONTEXT PACK"));
    }

    #[test]
    fn the_antiqa_paragraph_appears_only_when_the_pane_has_it_and_never_invites_editing_the_stories() {
        let base = |antiqa: bool| brief_md(&BriefInput { extra_prompt_md: None, scope: &[], manifest: None, docs: &[], live_tools: false, antiqa, intro: None });
        assert!(!base(false).contains("ANTIQA"));
        let md = base(true);
        assert!(md.contains("ANTIQA (MCP server `antiqa`") && md.contains("antiqa_list_stories") && md.contains("ONLY those story ids"));
        assert!(md.contains("Never edit antiQA's packs or stories"), "the oracle is not the agent's to change");
    }
}
