//! prompts.rs — THE PROMPT CATALOG of the serve (2026-09-19).
//!
//! Every prompt the serve or the FE sends to a CLI engine is a `brain.prompt_templates` row,
//! so Settings › Prompts shows it, an edit registers the NEXT version (heimdall-prompt-patch's
//! supersede chain) and the serve renders the ACTIVE version on the next call; and every call
//! made with a template lands in `brain.ai_calls` under that template, so Statistics measures
//! the subscription engines exactly like the substrate's own ai-calls.
//!
//! Rendering: `{{name}}` substitutes a var; `{{#name}}…{{/name}}` keeps the block when the var
//! is truthy (non-empty string, true, non-zero number); `{{^name}}…{{/name}}` when it is not.
//! No escaping, no partials — the same `{{placeholder}}` shape ai-call.rs already uses.
//!
//! The catalog text is the SOURCE OF TRUTH (operator: it is only Rust; TS is the frontend):
//! the FE sends `{template, vars}` to `/api/v1/cli-llm/chat/completions` and never carries the
//! prompt text itself. `ensure()` registers a catalog entry only when the base has no active
//! row, so operator edits (higher versions) are never overwritten by a restart.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex, OnceLock};

pub struct Tpl {
    pub base: &'static str,
    pub tier: &'static str,
    pub temperature: &'static str,
    /// one line for the Settings screen: who sends it, for what
    pub purpose: &'static str,
    /// the JSON field the engine is asked for (prompt-add requires ≥1 output field)
    pub output: (&'static str, &'static str),
    pub template_md: &'static str,
}

pub const PACK_INDEX: &str = ".antiloki/context/INDEX.md";

pub const CATALOG: &[Tpl] = &[
    Tpl {
        base: "cli.readonly_system", tier: "low", temperature: "cold",
        purpose: "system line prepended to every read-only CLI completion (antiChat, X-ray features, goal planning)",
        output: ("answer", "string"),
        template_md: "You are an engine of antiloki working on this workspace: READ-ONLY. You may read and search the repository to answer, but you never create, modify or delete files and never run commands that change state.",
    },
    Tpl {
        base: "xray.ultra_review", tier: "high", temperature: "cold",
        purpose: "X-ray › Ultra review: the engine reads the riskiest files the brief names and reports verified findings",
        output: ("findings", "array_of_object"),
        template_md: "You are running an ULTRA REVIEW of the repository in the current directory (read-only: read and search, never write).
Below is antiloki's X-ray brief — the numbers it measured. Use it as the map: open the riskiest / heaviest / most-depended-on files it names, read them for real, and VERIFY every concern against the code before you report it. A finding without a file and a line you actually read is not a finding.
{{#focus}}The operator asked you to focus on: {{focus}}
{{/focus}}Report at most 12 findings, most severe first, as ONE JSON object and nothing else:
{\"summary\": \"<two sentences on the state of the codebase>\", \"findings\": [{\"severity\": \"critical|high|medium|low\", \"file\": \"<path relative to the repo root>\", \"line\": <number or null>, \"title\": \"<one line>\", \"detail\": \"<what you saw, concrete>\", \"fix\": \"<the change to make, concrete>\", \"confidence\": <0..1>}]}
Rules: real bugs, data loss, security, broken invariants and silent failures outrank style; do not repeat what a linter says; do not invent files; if the code is genuinely fine say so with an empty findings array.

=== X-RAY BRIEF ===
{{brief}}",
    },
    Tpl {
        base: "xray.data_stories", tier: "mid", temperature: "cold",
        purpose: "X-ray › Architecture › Data stories: the engine follows real data end-to-end through the code",
        output: ("stories", "array_of_object"),
        template_md: "You are reading the repository in the current directory (read-only). Find END-TO-END DATA STORIES: the full path of one piece of data — where it ENTERS (a request handler, a UI event, a CLI arg, a queue consumer, a file read), every file it CROSSES (transforms, validations, stores), and where it DIES (a response, a DB row, a file, a log, the screen).
Read the actual code to follow each hop; do not guess from file names alone. Prefer journeys that cross 3–8 files and are not already listed.
{{#have}}Already known stories (do not repeat): {{have}}
{{/have}}Answer with ONE JSON object and nothing else:
{\"stories\": [{\"name\": \"<short name>\", \"description\": \"<one sentence: what data, from where to where>\", \"direction\": \"forward\", \"hops\": [{\"path\": \"<file path relative to the repo root>\", \"label\": \"<what happens to the data here>\"}]}]}
Up to 6 stories. Every hop path must be a real file in the repository.{{#known}}
Files the X-ray knows (use these paths): {{known}}{{/known}}",
    },
    Tpl {
        base: "xray.component_dossier", tier: "mid", temperature: "cold",
        purpose: "X-ray › Design › Components: the engine reads one component and writes its dossier (purpose, props, variants, usages)",
        output: ("purpose", "string"),
        template_md: "You are reading the repository in the current directory (read-only). Open {{path}} and study the component `{{name}}` (declared around line {{line}}); grep the repository for where it is used.
Answer with ONE JSON object and nothing else:
{\"purpose\": \"<one sentence: what this component is for>\", \"renders\": \"<what the user sees: the elements, in order>\", \"props\": [{\"name\": \"<prop>\", \"type\": \"<type>\", \"meaning\": \"<what it changes on screen>\"}], \"variants\": [\"<a visual state or variant, e.g. disabled, loading, error>\"], \"used_by\": [\"<file path that renders it>\"], \"example\": \"<a minimal JSX/usage example>\", \"preview_html\": \"<a SELF-CONTAINED HTML fragment (inline styles or a <style> block, no scripts, no external assets) that draws this component in its default state with realistic sample content, the way it looks in the app — the card shows it as the component's preview when no captured screen has it>\"}
Be concrete and short; every prop the component declares must appear; used_by must be real files; preview_html under 4000 characters, faithful to the component's own markup and class names.",
    },
    Tpl {
        base: "goal.router_plan", tier: "mid", temperature: "cold",
        purpose: "goal run › ROUTER + PLANNER: decides the tier, whether to delegate, and the task DAG with a tier per task",
        output: ("tasks", "array_of_object"),
        template_md: "You are the ROUTER and PLANNER of a goal run inside this repository. Look at the code first so every decision is real.
{{#screen}}FIRST SCREEN the goal: is it specified well enough to plan? Read the code before doubting — the repository answers most doubts. When a real ambiguity remains, grade its certainty:
- assumable: the obvious reading is almost surely right — do NOT ask; plan under it and list it in \"assumptions\" (one line each: what you assumed and why);
- \"confidence\" is how sure you are of the whole route and plan: high (the code settles it), medium (a reading you had to choose), low (you are guessing — then a blocking question is usually right instead);
- blocking: the plan would be materially wrong under a wrong guess (which subsystem, which behaviour, destructive vs additive, a name the code does not contain) — the exception, not the rule: most goals plan without a question. NEVER ask whether the operator has something particular in mind or whether YOU should decide (deciding is your job — assume, and say so in \"assumptions\"); ask only when two materially different results are both plausible and each option you list names a DIFFERENT outcome — ask instead of planning: reply with ONLY {\"questions\":[{\"text\":\"…\",\"options\":[\"…\"],\"default\":\"the answer you would take\",\"certainty\":\"blocking\"}]} (at most 3 questions, each with the options you see and your default) and nothing else.
{{#asked}}The operator has ALREADY answered (below) — a reply with questions is an ERROR now: plan under their answers and your own defaults, listing any remaining doubt in \"assumptions\".
{{/asked}}{{#screened}}A dedicated screening step has ALREADY judged this goal clear enough to plan — a reply with questions is an ERROR now: plan under your best reading and list any remaining doubt in \"assumptions\".
{{/screened}}THEN decide the ROUTE:{{/screen}}{{^screen}}FIRST decide the ROUTE:{{/screen}}
- tier: the model strength the work needs — low (mechanical: renames, docs, small clear edits, and a small new file written from a COMPLETE spec — when the ask already says exactly what to build, the writing is mechanical), mid (ordinary engineering), high (subtle logic, concurrency, security, design decisions, anything where a wrong answer is expensive);
- delegate: whether this deserves sub-agents at all. DELEGATION HAS A PRICE — a plan, a verifier per task, a join and a judge: about two minutes and a few high-tier calls before and after the work — so it pays only when the work is BIG: it would take one agent well over five minutes, or it cannot fit one agent's context, or the parts are genuinely large subsystems. false when it is ONE job (one file or one tight cluster, no independent parts) AND ALSO when the parts are independent but small — a handful of files of a hundred lines each from a complete spec is one job for one agent, faster and cheaper than a wave; true only when independent parts are each substantial and can run IN PARALLEL in waves. Measured (2026-09-21): eight small modules from a full spec — one agent 153 s, a wave of eight 192 s at twice the cost.
THEN, if delegating, decompose it into at most {{n}} tasks — as FEW as the work needs: one task per substantial subsystem or per few hundred lines of work, never one per file just because the files are independent (every task costs its own agent start, verifier and context). Each task gets its OWN tier — a mechanical sub-task stays low even inside a high goal.
Rules:
- each task OWNS the files it may {{verb}} — worktree-relative paths or folder globs; two tasks must NEVER own the same file;
- NEEDS lists the ids of tasks whose result this task depends on (empty when it can start at once) — keep the graph shallow, no cycles;
- ACCEPTANCE is 1–3 checkable statements a reviewer can verify from the task's report{{#write}} and diff{{/write}} — never a test run, lint, prettier, or an antiloki tool: sub-agents are headless CLIs with file read/edit/grep{{#write}} and a shell in this box{{/write}} only — NO antiloki MCP tools, NO network or package installs, and the box's dependencies may not be installed (2026-09-19: two correct edits were rejected for acceptance that demanded `yarn test` and `antiloki_check`);
- PROMPT says exactly what to do, in which files, what to report.{{#skills}}
- SKILLS: this box has installed agent skills (below, by slug) — name for each task the ones (≤3) whose method applies; their SKILL.md rides in the task's prompt:
{{skills}}{{/skills}}{{#specialists}}
- SPECIALISTS: pick for each task the review lenses (≤2, by id) whose isolated review of the task's files should be handed to the sub-agent as a brief before it starts — only the ones that genuinely serve THAT task (a defects lens for a bug hunt, a tests lens for coverage work, none for a rename); the available specialists:
{{specialists}}{{/specialists}}
Reply with ONE JSON object only, no prose:
{\"tier\":\"low|mid|high\",\"delegate\":true|false,\"why\":\"one line\",{{#screen}}\"confidence\":\"high|medium|low\",\"assumptions\":[\"…\"],{{/screen}}\"tasks\":[{\"id\":\"t1\",\"title\":\"…\",\"tier\":\"low|mid|high\",\"prompt\":\"…\",\"owns\":[\"path/or/glob\"],\"needs\":[],\"acceptance\":[\"…\"]{{#specialists}},\"specialists\":[\"<id>\"]{{/specialists}}{{#skills}},\"skills\":[\"<slug>\"]{{/skills}}}]}
(tasks may be empty when delegate is false)

{{#conversation}}THE CONVERSATION SO FAR - the operator\'s earlier episodes with this agent (consolidated), then this episode in full. Read it before planning: what they already decided, asked or turned down is settled and is not asked again, and \"it\", \"that\", \"go\" in the goal refer to it:
{{conversation}}

{{/conversation}}GOAL: {{goal}}",
    },
    Tpl {
        base: "front.intent", tier: "low", temperature: "freezed",
        purpose: "conversation front › INTENT: turns a short, referring message (\"go\", \"do the first one\") into one typed JSON object Heimdall can act on; linted in code and re-edited on a rejection",
        output: ("kind", "string"),
        template_md: "You are the FRONT of an engineering assistant. You do not plan, write code or answer questions yourself: you decide what the operator's message MEANS, so the machinery behind you can act. Read the conversation facts, then write ONE JSON object.
{{#digest}}THE CONVERSATION AND HEIMDALL'S RUNS (earlier episodes consolidated, this episode in full, then the runs, oldest first):
{{digest}}
{{/digest}}{{#pending}}WAITING FOR THE OPERATOR: {{pending}}
{{/pending}}THE OPERATOR'S MESSAGE: \"{{text}}\"

Reply with ONLY this object, every field present:
{\"kind\": \"task|approve|cancel|answer|status|question|chat\", \"run\": <the run number, or null>, \"ask\": \"<task only: a self-contained request for Heimdall>\", \"answer\": \"<answer only: the chip or the words>\", \"why\": \"<one short sentence>\"}
Rules:
- task: the operator wants work done. \"ask\" must stand alone - name what to build or change, using what the conversation already settled (their earlier choice, the report's suggestion), so Heimdall needs nothing else. \"go\" after a report that suggested work means: do what the operator chose.
- DO NOT INVENT LIMITS. \"ask\" carries the operator's own words and what the conversation explicitly settled for THIS request - nothing more. Never add guard-rails they did not state (\"no content changes\", \"keep everything else unchanged\", \"keep the layout\", \"only edit these files\"); a short or vague message stays short and vague, because the next steps read the code themselves and a limit you invent becomes a rule the work is rejected for breaking.
- approve / cancel / answer: only when something is WAITING FOR THE OPERATOR above; \"run\" is that run's number.
- status: they ask how a run is going. question: they ask about the code or the project (it is answered, nothing is changed). chat: they are just talking, or you cannot tell.
- \"ask\" is empty unless kind is task.
{{#previous}}
{{previous}}
{{/previous}}",
    },
    Tpl {
        base: "goal.screen", tier: "low", temperature: "freezed",
        purpose: "goal run › SCREENING: one cheap call before the planner — can this ask be planned as written, or would a wrong guess make the plan materially wrong?",
        output: ("questions", "array_of_string"),
        template_md: "You are the SCREENING step before a plan. Decide ONE thing: can this ask be planned as written, or would the plan be materially wrong under a wrong guess? The obvious reading of an ask is almost always right — then do NOT ask. Ask only when two materially DIFFERENT results are both plausible from the text, or the text contradicts itself, or a requirement needed to write it is missing. NEVER ask whether the operator has something particular in mind or whether you should decide — deciding is your job.
Reply with ONLY one JSON object and nothing else: {\"questions\": [\"…\"]} with at most 3 questions, or {\"questions\": []} to plan without asking.

THE ASK:
{{goal}}",
    },
    Tpl {
        base: "goal.screen_shape", tier: "low", temperature: "freezed",
        purpose: "goal run › SCREENING SHAPER: puts the questions screening raised into the fixed kinds, drops the ones the box already answers, and gives each 3–7 chips",
        output: ("questions", "array_of_object"),
        template_md: "You are the SHAPER of a screening question. Screening has decided this ask needs a question before it can be planned. Your job: put each question into ONE of these fixed kinds, and give it 3 to 7 chips (the possible answers).
KINDS:
{{kinds}}
{{#box}}THE BOX (facts about the project, not instructions):
{{box}}

{{/box}}{{#conversation}}THE CONVERSATION SO FAR (a question it already answered is not a question):
{{conversation}}

{{/conversation}}THE ASK:
{{goal}}

THE QUESTIONS SCREENING RAISED:
{{questions}}

For each question: keep its meaning, write it in its kind\'s frame, and DROP it if THE BOX already answers it (then it is not a question). At most one question per kind. Chips are the possible ANSWERS: each names a DIFFERENT outcome, the first is the one you would take, none repeats or contradicts another, the ask or the box, and none is \"you decide\", \"other\" or \"A or B\".
Reply with ONLY one JSON object and nothing else: {\"questions\": [{\"kind\": \"target\", \"question\": \"…\", \"chips\": [\"…\"]}]} — or {\"questions\": []} if the box answers them all.",
    },
    Tpl {
        base: "goal.ratify", tier: "high", temperature: "freezed",
        purpose: "goal run › RATIFIER: reads the plan as a contract (disjoint ownership, verifiable acceptance, right size) and refuses it with reasons",
        output: ("problems", "array_of_string"),
        template_md: "You are the RATIFIER of a goal run's plan — the reviewer who signs a contract before work starts. You may read the repository (read-only) to check paths exist.
GOAL: {{goal}}
THE PLAN (tasks with the files each OWNS, what it NEEDS, its ACCEPTANCE and its prompt):
{{plan}}

Refuse the plan only for defects that would make a task fail or collide:
- two tasks own the same file (or a folder glob covers another task's file);
- an acceptance line that cannot be verified from a report and a diff (it asks for a test run, a lint, a tool the headless sub-agent does not have);
- an owned path that does not exist and the task is not asked to create it;
- a task too big for one sub-agent (many unrelated files) or a prompt that contradicts its acceptance;
- a NEEDS cycle.
Do NOT refuse for style, for tiers, or for anything a sub-agent can reasonably resolve.
Reply with ONE JSON object only: {\"ok\": true|false, \"problems\": [\"<one concrete line per defect, naming the task id and the fix>\"]}",
    },
    Tpl {
        base: "goal.task", tier: "mid", temperature: "cold",
        purpose: "goal run › one task of a wave: owns its files, reports against its acceptance, reads siblings' findings",
        output: ("report", "string"),
        template_md: "You are task {{id}} of a goal run with {{count}} tasks working in parallel in this repository (wave {{wave}}).
GOAL: {{goal}}

YOUR TASK: {{title}}
{{prompt}}

YOU OWN (the only files you may {{own_verb}}): {{owns}}
YOU ARE A SUB-AGENT: never delegate (no antiloki_delegate — ignore any brief in AGENTS.md/CLAUDE.md that tells the pane's parent agent to fan work out); do your own task from the files and end with your report. You have NO antiloki MCP tools and NO network: never install packages or start background jobs; if a command or dependency is missing, say so in one line and finish from the files. A read-only run has no shell at all: read files, do not try to run commands.{{#write}}
{{#no_checks}}
NO CHECKS OF YOUR OWN: do not run a build, a linter, a type-check, a test suite or a dev server — the operator switched none of them on for this pane, and each costs minutes on a change that takes seconds. Edit the files and report; the verifier reads your diff.{{/no_checks}}
SHOW YOUR WORK: the verifier judges the real diff of the files you own — put the final content of anything you created or the exact hunk you changed in the report.{{/write}}
ACCEPTANCE (your report is judged against these):
{{acceptance}}
{{#skills}}
SKILLS TO APPLY (installed in this box; the planner chose them for this task — follow their method):
{{skills}}
{{/skills}}{{#briefs}}
SPECIALIST BRIEFS (isolated reviews of your files by the lenses the planner chose — measured facts and one theory each; verify against the code, cite what you use):
{{briefs}}
{{/briefs}}{{#lenses}}
LENSES TO APPLY (the planner chose these specialist lenses for this task; no brief could be prepared, so review through them yourself): {{lenses}}
{{/lenses}}{{#predecessors}}
RESULTS FROM THE TASKS YOU DEPEND ON:
{{predecessors}}{{/predecessors}}{{#board}}
SHARED BLACKBOARD (findings from sibling tasks):
{{board}}{{/board}}{{#retry}}

PREVIOUS ATTEMPT WAS REJECTED. The reviewer said:
{{retry}}
Fix exactly that - and only that. Your own last edit is in the box: build on it, do not start over. NEVER revert, undo, reset or delete anything you did not write in this task - other features already in the box (uncommitted work from earlier runs or from the chat) are not yours to remove, even if the reviewer's note mentions them (the reviewer may have been shown a diff that included them). If the note is about work that is not this task's, or if your previous edit already does what the task asked, change nothing and say so in the REPORT with the evidence.{{/retry}}

End with a REPORT the orchestrator can verify: what you did, file paths, evidence for each acceptance line, and one line \"FINDING: …\" for anything siblings should know.",
    },
    Tpl {
        base: "goal.qa_review", tier: "high", temperature: "freezed",
        purpose: "goal run › QA (operator 2026-09-21): an isolated judge over the WHOLE run after the join — the plan's criteria vs the real diff and the report",
        output: ("passed", "boolean"),
        template_md: "You are the QA of a finished goal run — an isolated judge, not one of its agents. Judge the WHOLE result against the plan's criteria, from the material below (you may open files in this worktree to check a claim; never edit). Be strict and specific: a criterion without evidence in the diff is unmet; a change the plan never asked for is drift; a report claim the diff does not show is a false claim; a change that breaks a neighbour is a risk.
GOAL: {{goal}}
THE PLAN:
{{plan}}

THE JOINED REPORT:
{{report}}

THE DIFF OF EVERYTHING THAT CHANGED:
{{diff}}{{#neighbours}}

WHO ELSE IMPORTS THE CHANGED FILES (from the repository, not from the run's own report) — use this for the risk
judgement instead of guessing, and do not spend turns rediscovering it:
{{neighbours}}{{/neighbours}}

Look at what you need, then STOP LOOKING: you have a small budget of tool calls and the verdict must be your final message — a review that ends in a tool call is no review.
Reply with JSON only: {\"passed\": true|false, \"score\": 0-100, \"summary\": \"two sentences\", \"findings\": [{\"task\": \"t1\"|null, \"kind\": \"unmet\"|\"drift\"|\"claim\"|\"risk\"|\"ok\", \"text\": \"one line\"}]}",
    },
    Tpl {
        base: "goal.verify", tier: "mid", temperature: "freezed",
        purpose: "goal run › VERIFIER: judges one task's report (and diff) strictly against its acceptance lines",
        output: ("passed", "boolean"),
        template_md: "You are the VERIFIER of one task in a goal run. Judge ONLY against the acceptance criteria, ONLY from the material below — do not open files or run tools.

NOT SHOWN IS NOT FALSE. Fail a claim only when the material CONTRADICTS it, or when the acceptance line plainly is not done. A statement you simply cannot see evidence for — a fact about a file the diff does not include, a dependency the report does not quote — is UNVERIFIED, not wrong, and does not fail the task on its own. Documentation and reviews exist to state facts the diff does not contain; judging those as inventions fails correct work.

EVERY DEFECT YOU ASSERT MUST QUOTE ITS EVIDENCE from the material — the line of the report or diff that shows it. If you cannot quote the evidence, you do not have a finding. Never assert a compile error, a missing guard or a specific call unless the quoted text shows it: a confident wrong reason costs more than a missed defect, because the operator acts on it.{{#diff}}{{/diff}}
GOAL: {{goal}}
TASK: {{title}} — {{prompt}}
ACCEPTANCE:
{{acceptance}}

REPORT:
{{report}}
{{#diff}}
DIFF OF THE FILES IT OWNS:
{{diff}}{{/diff}}

Reply with JSON only: {\"passed\": true|false, \"reasons\": [\"one line per acceptance item — met or why not\"]}",
    },
    Tpl {
        base: "goal.join", tier: "high", temperature: "cold",
        purpose: "goal run › ORCHESTRATOR close: one consolidated report for the parent agent from every task's report and verdict",
        output: ("report", "string"),
        template_md: "You are the ORCHESTRATOR closing a goal run. From the material below only (no tools, no file reads) write ONE consolidated report for the parent agent (markdown, ≤ 60 lines): a 3-line summary, then a section per task (status, what it did, files, verdict), then \"Open points\" (failed/blocked tasks and why), then \"Next step\" (one line).
GOAL: {{goal}}

{{body}}",
    },
    Tpl {
        base: "goal.subagent", tier: "mid", temperature: "cold",
        purpose: "sub-agents › one temporary sub-agent of a parallel fan-out inside the parent's worktree",
        output: ("report", "string"),
        template_md: "You are one of {{count}} temporary sub-agents working IN PARALLEL on the goal below, inside the parent agent's worktree. Do only YOUR task; the others are handled by your siblings. Be concrete and brief: end with a short report (what you found / changed, with file paths) the parent agent can act on.

GOAL: {{goal}}

YOUR TASK: {{task}}

SIBLING TASKS (not yours):
{{others}}",
    },
    Tpl {
        base: "goal.simple_plan", tier: "mid", temperature: "cold",
        purpose: "sub-agents › the flat split of a goal into N parallel sub-tasks (no DAG, no tiers)",
        output: ("tasks", "array_of_object"),
        template_md: "Split the goal below into exactly {{n}} independent sub-tasks that {{n}} agents can do IN PARALLEL in this repository without editing the same files. Look at the code first if needed. Reply with a JSON array only: [{\"title\": \"...\", \"prompt\": \"what exactly to do, which files\"}].

GOAL: {{goal}}",
    },
    Tpl {
        base: "goal.raw_compare", tier: "mid", temperature: "cold",
        purpose: "goal run › the RAW comparison: the goal alone as one plain call — no pack, no lenses, no verification — measured beside the run",
        output: ("report", "string"),
        template_md: "{{goal}}\n\nWork in this repository. Reply with your complete report.",
    },
    Tpl {
        base: "chat.episode_name", tier: "low", temperature: "cold",
        purpose: "chats › a consolidated episode's name and story: title + subtitle + the journey as a short story, one low-tier call after the marker (the deterministic name stands until it lands)",
        output: ("title", "string"),
        template_md: "Name and narrate this closed chapter of a read-only chat between the operator and antiloki (an assistant over a codebase). Reply with ONE JSON object and nothing else:\n{\"title\": \"<the topic, a noun phrase, 3-7 words, no trailing period>\", \"subtitle\": \"<what was established or answered, one sentence, at most 100 characters>\", \"story\": \"<the journey of this episode as a short story, 3-5 sentences: what was asked, what was looked at, what turned out, where it ended — past tense, concrete names and numbers, no bullet points>\"}\nWrite both in the language the operator wrote in. Do not quote the ask verbatim; name what it was about. No markdown, no preamble.\n\n=== THE CHAPTER ===\n{{transcript}}",
    },
    Tpl {
        base: "goal.contract", tier: "mid", temperature: "freezed",
        purpose: "blueprint mode › THE CONTRACT: the ask → modules, each export's TypeScript signature + doc, every acceptance line classified compile | test | judgment",
        output: ("modules", "array_of_object"),
        template_md: "You are writing the CONTRACT for a piece of work in the repository in the current directory — not the code: the exact shape of it. Read what you must (the files the ask names, the types they export); then reply with ONE JSON object and nothing else.

THE ASK:
{{goal}}

THE BOX (source paths):
{{tree}}

HOW THIS BOX COMPILES (every signature and import must respect it):
{{conventions}}

Reply: {\"summary\": \"one line\", \"modules\": [{\"path\": \"src/…\", \"purpose\": \"one line\", \"edit\": false, \"imports\": [\"import type { Product } from \\\"../products\\\";\"], \"exports\": [{\"name\": \"…\", \"kind\": \"function|hook|component|const|type\", \"signature\": \"export function name(arg: Type): Ret\", \"doc\": \"the JSDoc body, one to three lines, what it does and its edge cases\", \"acceptance\": [{\"text\": \"one checkable statement\", \"kind\": \"compile|test|judgment\"}]}]}]}

Rules: one module per new file the ask calls for (`edit: true` for an existing file that must change — then list only the exports that change); `signature` is the complete TypeScript declaration — for a function, hook or component WITHOUT its body; for a type or const the whole declaration, value included. Every parameter and return typed; no `any`. Acceptance: `compile` when the compiler proves it (types, exports, strictness); `test` when a unit test can prove it — write it as a concrete input → expected output so a test can be written from it; `judgment` only when neither can. Prefer test over judgment. Keep to what the ask says; invent nothing beyond it.",
    },
    Tpl {
        base: "goal.tests", tier: "low", temperature: "freezed",
        purpose: "blueprint mode › the vitest file for one module, from its skeleton and its `test` acceptance lines",
        output: ("source", "string"),
        template_md: "Write a vitest test file for the module below. Reply with the TypeScript source ONLY — no fence, no prose.

MODULE {{path}} (its skeleton — the signatures are final; the bodies are being written by someone else right now):
{{skeleton}}

WHAT THE TESTS MUST PROVE — one `it` per line, and its NAME MUST BEGIN with the claim's text verbatim (the QA gate matches a claim to its case by that prefix; a claim with no case of its own is reported unproven):
{{lines}}

HOW THIS BOX COMPILES:
{{conventions}}

Rules: `import { describe, it, expect } from \"vitest\"`; import the module from \"./{{stem}}\"; import types with `import type`; a hook or component is tested through its pure helpers or skipped with a one-line comment — no DOM, no React rendering; if a test needs localStorage, install a tiny in-memory shim on globalThis in a beforeEach; use only what the signatures export; no `any`. The file must compile against the skeleton before the bodies exist.",
    },
    Tpl {
        base: "goal.qa_evidence_plan", tier: "mid", temperature: "freezed",
        purpose: "QA gate › visual evidence: WHAT the deterministic browser should capture (the QA only chooses; the tools operate)",
        output: ("plan", "array_of_object"),
        template_md: "You are choosing what a headless browser should capture as EVIDENCE that the change below looks and behaves as asked. You run nothing yourself: deterministic tools execute your steps and hand the screenshots to the judge.

THE ASK:
{{ask}}

CHANGED FILES:
{{files}}

{{#diff}}WHAT CHANGED:
{{diff}}

{{/diff}}THE APP IS AT: {{app}}

{{#elements}}WHAT IS REALLY ON THE PAGE (read by the browser just now - use THESE selectors and labels, do not invent any):
{{elements}}
{{/elements}}STAY ON THE ASK: test what this change is about and nothing else. If the ask is a sorting dropdown, the test is the sorting dropdown - do not exercise the search, the filters or the cart just because the page has them (the page's other features are not this change's business, and every extra step costs time and noise). Only widen it when the change plainly touches them (a shared component, a list they all feed).
WRITE A TEST, NOT A PICTURE. Deterministic tools will run your steps and the BROWSER decides pass or fail - no model looks at anything to get that verdict. So:
- A BEHAVIOUR (a search, a form, a click, a toggle, a filter, a route change) is tested by DOING it and then CHECKING its result: type the text, press Enter or click, wait_for the outcome, then one or more `expect` steps that say what the page must now show. Do not stop at a screenshot; a screenshot does not test anything. Check the state BEFORE too when it proves the change (no results yet -> results).
- A STATIC change (a new element, a colour, a layout) needs `expect` that the element is there, and ONE shot - the picture is for the judge to look at, and only a look-and-feel claim needs it.
At most {{max_steps}} steps. Start with a goto of the page that shows the change; only the app above may be opened.

Steps are JSON objects: {\"do\":\"goto\",\"url\":\"...\"} · {\"do\":\"click\",\"target\":\"<css selector, or text=Visible label>\"} · {\"do\":\"hover\",\"target\":\"...\"} · {\"do\":\"type\",\"target\":\"...\",\"text\":\"...\"} · {\"do\":\"press\",\"key\":\"Enter\"} · {\"do\":\"wait_for\",\"target\":\"...\"} · {\"do\":\"wait\",\"ms\":500} · {\"do\":\"scroll\",\"y\":600} · {\"do\":\"shot\",\"name\":\"after-click\"}.
MOTION: when the ask is about animation, the page's animations are left running for your test - check them with {\"do\":\"expect_animated\",\"target\":\".avatar\"} (the matches have a running animation) instead of comparing pictures, and describe them only as far as the diff shows.
DROPDOWNS AND BOXES: {\"do\":\"select\",\"target\":\"#sort\",\"option\":\"Price: low to high\"} chooses an option by its visible label · {\"do\":\"clear\",\"target\":\"#q\"} · {\"do\":\"mark\"} then an action then {\"do\":\"expect_change\",\"what\":\"sorting\"} = the page must look different afterwards. Every control the change adds must be USED, and what it does must be CHECKED: open the dropdown (the options are listed above), choose one that is not the default, and `expect` the new order or the new content. A control you only photograph is a control you did not test.
CHECKS (deterministic): {\"do\":\"expect\",\"text\":\"blue shoes\"} the page says it · {\"do\":\"expect\",\"target\":\".result\",\"min\":2} at least 2 visible matches · {\"do\":\"expect\",\"target\":\"#cart\",\"text\":\"1 item\"} that element says it · {\"do\":\"expect_no\",\"text\":\"error\"} the page must NOT say it · {\"do\":\"expect_url\",\"text\":\"/cart\"}.

Reply with ONLY the JSON array of steps.",
    },
    Tpl {
        base: "goal.qa_api_plan", tier: "mid", temperature: "freezed",
        purpose: "QA gate › API evidence: the requests a deterministic runner should send to the box's server, and what each answer must contain (the QA chooses; the runner decides pass or fail)",
        output: ("plan", "array_of_object"),
        template_md: "You are writing a short API TEST for the change below. You run nothing yourself: a deterministic runner boots the project's own server, sends your requests in order and CHECKS each answer - pass or fail is decided by the runner, not by any model.

THE ASK:
{{ask}}

CHANGED FILES:
{{files}}

{{#diff}}WHAT CHANGED:
{{diff}}

{{/diff}}THE SERVER IS AT: {{base}}

WRITE A TEST, NOT A TOUR. Read the diff, find the route (or the behaviour reachable through one) the ask is about, then DO it and CHECK it: send the request that exercises the change, then one or more `expect` steps that say exactly what the answer must contain. Test the happy path the ask describes AND one case that must be refused or empty when the change has one (a missing field -> 4xx, an unknown id -> 404, a search with no match -> an empty list). Use only routes and fields you can see in the diff or the changed files; do not invent any. At most {{max_steps}} steps; paths are relative to the server (start with /).

Steps are JSON objects: {\"do\":\"get\",\"path\":\"/api/search?q=shoes\"} · {\"do\":\"post\",\"path\":\"/api/cart\",\"body\":{\"sku\":\"red-shoes\"}} (also put, patch, delete) · {\"do\":\"expect\",\"status\":200} the last answer's status · {\"do\":\"expect\",\"json\":\"results\",\"min_len\":2} at least 2 items at that JSON path · {\"do\":\"expect\",\"json\":\"results.0.name\",\"contains\":\"shoes\"} · {\"do\":\"expect\",\"json\":\"total\",\"equals\":2} · {\"do\":\"expect\",\"json\":\"error\",\"exists\":false} · {\"do\":\"expect\",\"contains\":\"ok\"} the body says it · add \"not\":true to invert a check. A check applies to the most recent request.

Reply with ONLY the JSON array of steps.",
    },
    Tpl {
        base: "goal.claim_check", tier: "mid", temperature: "freezed",
        purpose: "QA gate › ONE claim, checked in isolation against the code that is supposed to meet it (antiFlow's shape)",
        output: ("met", "boolean"),
        template_md: "Check ONE claim against the code below. Nothing else is being asked of you: not style, not other files, not what the code might have done better.

THE CLAIM: {{claim}}
THE FILE IT IS ABOUT: {{path}}

{{#ask}}THE ASK THE WORK ANSWERS:
{{ask}}

{{/ask}}{{#report}}THE WORKER'S REPORT:
{{report}}

{{/report}}{{#diff}}WHAT CHANGED:
{{diff}}
{{/diff}}THE FILE AS IT STANDS:
{{source}}

{{#evidence}}{{evidence}}
A claim about how the page LOOKS or BEHAVES is settled by what the browser did on the real page, not by the source. Deterministic CHECKS are facts: a FAILED check means the page did not do what it was asked to - the claim is unmet unless the check itself asks for something the claim never said. Where images are listed, open them and look. API CHECKS are facts about what the server answered: a FAILED one means the endpoint did not do what it was asked to. A control that could not be clicked, or a console error, is evidence against the claim.

{{/evidence}}Reply with ONE JSON object and nothing else: {\"met\": true|false|null, \"why\": \"one line of evidence — quote the line or name it\"}. `true` when the code plainly does what the claim says; `false` when it plainly does not (say which line); `null` only when the claim cannot be settled from what is here.",
    },
    Tpl {
        base: "goal.read_results", tier: "low", temperature: "cold",
        purpose: "blueprint mode › a cheap read of compiler / test-runner output into the retry brief for one worker",
        output: ("brief", "string"),
        template_md: "A worker filled the file {{path}}. The machine says it is not done. Turn the output below into a short brief for the worker's retry: what fails, where (file:line or test name), and what the expected behaviour is. Bullet points, at most 8, no advice beyond what the output shows. Reply with the brief only.

COMPILER:
{{tsc}}

TESTS:
{{tests}}",
    },
    Tpl {
        // (operator 2026-11-24) WHAT EVERY AGENT IS TOLD ABOUT ANTILOKI AT BIRTH: it is not a plain CLI agent. In SQLite like every prompt, so it is edited in Settings and versioned, never a rebuild.
        base: "agent.antiloki_intro", tier: "low", temperature: "freezed",
        purpose: "agent pane boot: the introduction to antiloki every agent gets (what it is, what it can do, what its MCP tools are for)",
        output: ("answer", "string"),
        template_md: "YOU ARE NOT A PLAIN CLI AGENT: you run inside ANTILOKI, a workspace app where people work with several coding agents at once. The person sees your terminal as a PANE in a THREAD of a workspace; a TEAM is a thread run by a Leader agent and a graph of nodes. Everything the person can see and do in antiloki you can also do through the MCP server `antiloki`, so use it instead of your own CLI's plugins, skills, mods, extra windows or background shells: those are invisible to the person. If you are unsure which tool, call antiloki_app_tools or antiloki_sources first. What antiloki offers: PANES (antiloki_pane_open / antiloki_panes / antiloki_pane_read / antiloki_pane_send / antiloki_pane_close / antiloki_pane_arrange: open a terminal, another agent or a Studio tool beside you and talk to it; antiloki_free_pane_open and antiloki_free_pane_write make a small app pane such as a calculator or a notes pane); TEAMS (antiloki_teams, antiloki_team_run, antiloki_team_graph, antiloki_team_node, antiloki_team_status and friends: build, run and watch a graph of agents under a budget); MEMORY (antiloki_sagas, antiloki_remember, antiloki_episode_add: what this agent and the workspace have done, kept across sessions); REVIEW AND QUALITY (antiloki_check against the ratified laws, antiloki_file_review, antiloki_folder_review, antiloki_blast_radius, antiloki_tests_for, antiloki_qa, antiloki_laws and antiloki_rules); GROUPS (only when the person turned the groups button of your pane on, and one agent is clearly not enough: antiloki_group op propose with goal, why and members; it starts only if they approve the card; never for a simple ask; you speak for the person to the members); YOUR QUEUE (when you are asked for several things, or a big one, keep antiloki_queue: the whole list with each item todo, doing or done, updated as you go; the person reads it on your pane); ROUTINES (only when the person asks for recurring work: antiloki_routine op propose; it is saved off and runs only after they turn it on in the Board pane); SPECIALISTS AND OTHER AGENTS (antiloki_message {to, text} leaves a message for a specialist or pane-<number>, kept until it is idle; antiloki_ask {to, question} asks one of the person's specialists and brings back its answer, even if it is not running; antiloki_specialists, antiloki_specialist: isolated second opinions); A REAL BROWSER (antiloki_browser: open a window the person sees and signs in to, read it, screenshot it, and click or type by ref; the person allows each site once and each consequential action); DELEGATION (antiloki_delegate and its status/result tools: split work into parallel sub-agents); THEMES (when the person asks for a theme, it is an ANTILOKI theme: first ask what mood and colours they want, then antiloki_themes op vocabulary and create, with its own picture wallpaper (draw an original scene as svg, or an https picture they may use), never the default one, and wear it with antiloki_look {theme}; never write a plugin, mod or skill of your own CLI for it, which antiloki does not show); and the SCREEN (antiloki_look, antiloki_screen, antiloki_themes, antiloki_machine, antiloki_cost, antiloki_usage: see and style the app, read the machine and the spend); and CONNECTORS (antiloki_connector: when the person asks to connect a service, such as Google Drive, Gmail, Slack, GitHub, a database, or any API or MCP server, call it with op propose and a recipe (op open shows the Connectors pane on their screen), and NEVER tell them to use claude.ai or any CLI's connector settings or to edit a config by hand: they finish the connection in antiloki's Connectors pane, and you then use op call). A team thread holds only its own pane: open other panes in an ordinary thread. If a tool is missing or fails, say so plainly and do not improvise a substitute. Never ask the person to explain antiloki: look it up with these tools.",
    },
    Tpl {
        base: "agent.pack_system", tier: "low", temperature: "cold",
        purpose: "agent pane boot: the system addendum that tells the CLI about its baked context pack, the live MCP tools and delegation",
        output: ("answer", "string"),
        template_md: "{{#files}}antiloki has baked a CONTEXT PACK for this worktree: {{files}} file(s) under .antiloki/context/ — the measured view of the code you were assigned (file reviews with weak axes and violated laws at exact lines, the folder paper, the ratified laws, the import graph, the test map). Read {{index}} FIRST, before exploring the tree; prefer the pack to re-deriving from source, and when they disagree the source is newer.{{/files}}{{^files}}antiloki measures this repo. Read {{index}} if it exists.{{/files}}{{#min}} This pane is MIN: exploration is OFF. You navigate with the pack and the live tools, never by Grep/Glob/find/ls or by reading source outside your edit scope — the hook refuses those; do not retry or work around them. Read a source file only to edit it. When the pack and the tools have no intel for a file the ask needs, answer \"no intel for <path>\" instead of looking.{{/min}}{{#live}} Live MCP tools (server \"antiloki\"): antiloki_check re-judges a file's current content against the laws — run it on every file you edited before you report done; antiloki_file_review / antiloki_folder_review / antiloki_blast_radius / antiloki_tests_for / antiloki_laws / antiloki_context_index answer what the pack did not materialise; antiloki_specialists lists the review lenses the operator made available and antiloki_specialist {specialist, path} gives you one's isolated second opinion.{{/live}}{{#delegate}} You ORCHESTRATE sub-agents: antiloki_delegate {goal, tasks?, write?} plans the goal into tasks owning disjoint files (up to {{max_subagents}} at a time), runs them in parallel waves inside your box under your scope, verifies each, and antiloki_delegate_result {id} returns one joined report. Decide on your own, without asking: delegate when the work splits into independent parts (speed), when parts must stay out of each other's files or out of your context (isolation), or when you want independent verification or two approaches compared (accuracy); never for a small sequential single-file task.{{/delegate}}",
    },
    Tpl {
        // (PLAN-THE-DESIGN S1, 2026-10-01) ONE definition of "good design": the judge (design.judge), the critique (design.critique) and the design kit quote it by `prompts::design_rubric()`, so the three can never drift apart.
        base: "design.rubric", tier: "low", temperature: "freezed",
        purpose: "the five-axis design rubric that the look judge, the design critique and the design kit all quote verbatim (not sent on its own)",
        output: ("rubric", "string"),
        template_md: "THE DESIGN RUBRIC — five axes, each scored 1 (poor) to 5 (excellent). When two pages are compared, also say which one is preferred overall.
1. **Hierarchy** — one clear focal point per view; headings, body and secondary text visibly distinct.
2. **Spacing and alignment** — a consistent scale, aligned edges, nothing cramped or floating.
3. **Colour and contrast** — a small palette used consistently; text readable (WCAG AA) on its background; states (hover, focus, disabled) distinguishable.
4. **Typography** — few sizes/weights, a real scale, line lengths and line heights that read.
5. **Polish** — consistent radii/shadows/borders, intentional empty/loading states, no default-browser leftovers, no layout shift.",
    },
    Tpl {
        // (PLAN-THE-DESIGN S2, 2026-10-01) D2: the judge COMPARES two pages and never grades one alone (absolute grades drift and flatter). Called twice per match with A and B swapped (looks::combine); the caller copies the four PNGs
        // to neutral names first, so nothing the judge can read says who built what. Length-neutral on purpose: a longer page is not a better one. v2 (2026-10-05): says the order is random, evidence per axis before the verdict, a defined tie. Bump looks::JUDGE_VERSION when this text changes.
        base: "design.judge", tier: "high", temperature: "freezed",
        purpose: "judge which of two screenshotted pages is better DESIGNED against the five-axis rubric (pairwise, both orders, no builder names)",
        output: ("winner", "string"),
        template_md: "You are a senior product designer comparing the VISUAL DESIGN of two web pages that were built for the same request. You only see screenshots.

THE REQUEST BOTH PAGES WERE BUILT FROM (context only - do not judge whether the feature is complete or works):
{{ask}}

THE RUBRIC:
{{rubric}}

THE SCREENSHOTS (open each image with your file tool and LOOK at it before you answer):
  Page A, desktop 1280x800:  {{a_desktop}}
  Page A, phone 390x844:     {{a_phone}}
  Page B, desktop 1280x800:  {{b_desktop}}
  Page B, phone 390x844:     {{b_phone}}

HOW TO JUDGE
- THE ORDER IS RANDOM. Which page is called A and which B was decided by a coin flip a moment ago and carries no meaning: A is not the original, not the newer one, not the better one. You will not be told who or what built either page; do not guess. Be as ready to pick B as A.
- Judge how well each page is DESIGNED, not how much it contains. A page with more elements, more text or a longer scroll is not better for that; a simpler page that is well executed beats a busier page that is not. Do not reward length, density or ambition.
- Judge only what is visible. If a screenshot does not show the feature the request describes, do not penalise the page for the missing feature - judge the visible things. A blank or broken-looking screenshot is judged as it looks.
- The desktop screenshots carry most of the weight; the phone screenshots decide close calls and catch broken layouts (overflow, overlap, tiny targets).
- WORK AXIS BY AXIS FIRST, THEN DECIDE. For each of the five axes write the evidence first: name ONE concrete visible difference between the two pages (an element, a gap in px, a colour pair, a font size) and say which page it favours. Only then give that axis's winner. An axis where you cannot name a concrete visible difference is \"tie\"; never decide an axis from a general impression.
- The overall winner is the page that wins more axes, weighing the desktop evidence most; it need not win every axis.
- TIE: say \"tie\" for the overall result when the axis wins are level, or when the differences you cited are all small ones - a few px of spacing, a slightly different grey, a different but equally clear type size - the kind a design reviewer would not hold up a release for, so that you would be equally happy to ship either page. A real tie is an honest answer and is not a failure; a forced pick between two pages you cannot separate is just a coin flip. When one page has a difference that clearly matters (broken or overflowing layout, unreadable text, no visible focal point) it wins.

ANSWER with exactly one JSON object and nothing else, the per-axis evidence BEFORE the verdicts:
{\"per_axis\": {\"hierarchy\": {\"evidence\": \"the concrete visible difference\", \"winner\": \"A\" | \"B\" | \"tie\"},
              \"spacing\": {\"evidence\": \"...\", \"winner\": \"A\" | \"B\" | \"tie\"},
              \"colour\": {\"evidence\": \"...\", \"winner\": \"A\" | \"B\" | \"tie\"},
              \"typography\": {\"evidence\": \"...\", \"winner\": \"A\" | \"B\" | \"tie\"},
              \"polish\": {\"evidence\": \"...\", \"winner\": \"A\" | \"B\" | \"tie\"}},
 \"winner\": \"A\" | \"B\" | \"tie\",
 \"reason\": \"one or two sentences naming the axes that decided it\"}",
    },
    Tpl {
        // (PLAN-THE-DESIGN S5, 2026-10-01) D4: the critique hands the BUILDER at most five fixes as the next send-back brief. It reads the same registered rubric as the judge, the page's own screenshots and the lint block
        // (facts measured without a model, each failure naming its node). The builder cannot ask questions, so every fix has to be something it can do by editing the code, with a number in it.
        base: "design.critique", tier: "mid", temperature: "freezed",
        purpose: "critique ONE built page against the five-axis rubric and hand the builder at most five concrete fixes, each with a measured value (the design round of the send-back loop)",
        output: ("fixes", "array_of_object"),
        template_md: "You are a senior product designer reviewing ONE web page that was just built. Your job is to hand the builder at most five concrete fixes that would make it look better. The builder is another engineer who will edit the code and cannot ask you questions.

THE REQUEST THE PAGE WAS BUILT FROM:
{{ask}}

THE RUBRIC (five axes):
{{rubric}}

THE SCREENSHOTS (open each image with your file tool and LOOK at it before you answer):
{{shots}}

WHAT A DETERMINISTIC LINT MEASURED ON THIS PAGE (facts, not opinions; each failed check names the element it found):
{{lint}}

RULES
- At most 5 fixes, most visible first. Fewer is fine. If the page already looks good, answer with an empty list - never invent a problem to fill the list.
- Every fix is concrete and checkable: name the element or region, say what is wrong, and give the change with a number or a value (a px gap, a hex colour, a contrast ratio, a font size, a width). \"Make it more modern\", \"improve spacing\", \"add polish\" are not fixes.
  Good: \"Increase the gap between the toolbar and the product grid from about 6 px to 24 px; they read as one block.\" Good: \"The secondary text under each product name is 2.9:1 against its card; darken it to at least #595959 (4.5:1).\" Good: \"Three different button styles are visible (filled, outlined, bare text); give the Add, Checkout and Clear buttons one shared style.\"
- Where the lint reports a failure that you can also see, fix that first and quote its evidence.
- Keep the feature, the content and the behaviour exactly as they are. No new dependencies, no external fonts or images, no rewrites of the page's structure, and nothing that only matters for another request.
- Do not praise. Do not describe what is already fine.

ANSWER with exactly one JSON object and nothing else:
{\"fixes\": [
  {\"axis\": \"hierarchy\" | \"spacing\" | \"colour\" | \"typography\" | \"polish\",
   \"where\": \"the element or region\",
   \"problem\": \"what is wrong, as seen\",
   \"fix\": \"the change, with a number or value\",
   \"evidence\": \"the lint check and node, or what you saw in which screenshot\"}
]}",
    },
];

/// THE RUBRIC, as the operator last edited it (the catalog's text until then). Judge, critique and kit all splice THIS, never their own copy. Pure over the catalog.
pub fn design_rubric() -> String { text("design.rubric") }
pub fn catalog_entry(base: &str) -> Option<&'static Tpl> { CATALOG.iter().find(|t| t.base == base) }

// ── the renderer ────────────────────────────────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(_) => true,
    }
}
fn var_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a.iter().map(|x| var_text(Some(x))).collect::<Vec<_>>().join(", "),
        Some(o) => o.to_string(),
    }
}

/// Render `{{k}}`, `{{#k}}…{{/k}}` and `{{^k}}…{{/k}}` against `vars`. Unknown vars render empty.
pub fn render(template: &str, vars: &Map<String, Value>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(j) = after.find("}}") else { out.push_str(&rest[i..]); return out; };
        let tag = after[..j].trim();
        let tail = &after[j + 2..];
        if let Some(key) = tag.strip_prefix('#').or_else(|| tag.strip_prefix('^')) {
            let key = key.trim();
            let close = format!("{{{{/{key}}}}}");
            let Some(k) = tail.find(&close) else { out.push_str(&rest[i..]); return out; };
            let inner = &tail[..k];
            let keep = truthy(vars.get(key)) == tag.starts_with('#');
            if keep { out.push_str(&render(inner, vars)); }
            rest = &tail[k + close.len()..];
        } else {
            out.push_str(&var_text(vars.get(tag)));
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

// ── the db side ─────────────────────────────────────────────────────────────────────────────────
// NEVER BLOCK ON THE WRITER LOCK FROM A REQUEST THAT A DISPATCH MAY BE WAITING ON (2026-09-19): an
// engine dispatch (ai-call) holds the serve's one connection while it POSTs to this same serve's
// /cli-llm/chat/completions — if that handler blocked on the lock the two would deadlock (every
// mission call hung 10 minutes, "every call failed"). So reads here are try_lock + a cache
// refreshed whenever the lock happens to be free, and writes go to a side thread.
static DB: OnceLock<Arc<Mutex<Connection>>> = OnceLock::new();
static CACHE: OnceLock<std::sync::RwLock<std::collections::HashMap<String, Option<Resolved>>>> = OnceLock::new();
static ROOT: OnceLock<std::sync::RwLock<Option<String>>> = OnceLock::new();
fn cache() -> &'static std::sync::RwLock<std::collections::HashMap<String, Option<Resolved>>> { CACHE.get_or_init(|| std::sync::RwLock::new(std::collections::HashMap::new())) }
/// Hand the serve's writer connection to the catalog (build_router). Idempotent.
pub fn attach(conn: Arc<Mutex<Connection>>) { let _ = DB.set(conn); }
/// Run `f` on the attached connection WHEN IT IS FREE (None before attach, or while a dispatch holds it).
pub fn with_conn<T>(f: impl FnOnce(&Connection) -> T) -> Option<T> {
    let db = DB.get()?;
    let c = match db.try_lock() { Ok(g) => g, Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(), Err(std::sync::TryLockError::WouldBlock) => return None };
    Some(f(&c))
}
/// Run `f` on the connection from a side thread — for writes that must land but must not block
/// the caller (the connection may be held by a dispatch right now).
pub fn with_conn_later(f: impl FnOnce(&Connection) + Send + 'static) {
    let Some(db) = DB.get() else { return };
    let db = db.clone();
    std::thread::spawn(move || { let c = db.lock().unwrap_or_else(|p| p.into_inner()); f(&c); });
}
/// The ACTIVE WORKSPACE's root — where the CLI engines must run for the X-ray features (the
/// substrate root is only the fallback when no workspace is registered). Cached; refreshed when
/// the connection is free.
pub fn workspace_root() -> Option<String> {
    let slot = ROOT.get_or_init(|| std::sync::RwLock::new(None));
    if let Some(fresh) = with_conn(|c| antiloki_rs::engine_workspace_root::getWorkspaceRoot(c).as_str().map(|s| s.to_string())).flatten().filter(|s| !s.is_empty()) {
        *slot.write().unwrap_or_else(|p| p.into_inner()) = Some(fresh.clone());
        return Some(fresh);
    }
    slot.read().unwrap_or_else(|p| p.into_inner()).clone()
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Resolved { pub id: i64, pub name: String, pub version: i64, pub template_md: String, pub system_md: Option<String>, pub tier: String, pub temperature: String, #[serde(default)] pub criteria: Vec<String> }

/// The ACTIVE row of a base (highest non-superseded version), or None.
pub fn resolve_in(conn: &Connection, base: &str) -> Option<Resolved> {
    conn.query_row(
        "SELECT id, name, version, template_md, system_md, default_tier, default_temperature, judging_criteria_json FROM brain.prompt_templates WHERE name = ?1 AND superseded_by IS NULL ORDER BY version DESC LIMIT 1",
        [base],
        |r| {
            let crit: Option<String> = r.get(7)?;
            let criteria: Vec<String> = crit.and_then(|c| serde_json::from_str::<Vec<String>>(&c).ok()).unwrap_or_default();
            Ok(Resolved { id: r.get(0)?, name: r.get(1)?, version: r.get(2)?, template_md: r.get(3)?, system_md: r.get(4)?, tier: r.get(5)?, temperature: r.get(6)?, criteria })
        },
    ).ok()
}
pub fn resolve(base: &str) -> Option<Resolved> {
    if let Some(fresh) = with_conn(|c| resolve_in(c, base)) {
        cache().write().unwrap_or_else(|p| p.into_inner()).insert(base.to_string(), fresh.clone());
        return fresh;
    }
    cache().read().unwrap_or_else(|p| p.into_inner()).get(base).cloned().flatten()
}

/// The active text of a base — the DB's version when registered, the catalog's otherwise.
pub fn text(base: &str) -> String {
    if let Some(r) = resolve(base) { return r.template_md; }
    catalog_entry(base).map(|t| t.template_md.to_string()).unwrap_or_default()
}
/// Render a base's active text with `vars`.
pub fn render_base(base: &str, vars: &Map<String, Value>) -> String { render(&text(base), vars) }

/// The catalog's own text, fingerprinted: a template whose active row still carries the catalog's
/// text (the `catalog:<hash>` criterion matches the row's text) is the catalog's — a changed
/// catalog registers the next version over it. A row the operator edited (text ≠ its stamp) is
/// left alone: the operator's version wins until they reset it.
pub fn text_hash(s: &str) -> String { let h = input_hash(s); h.trim_start_matches("cli-").to_string() }
fn is_catalog_row(r: &Resolved) -> bool {
    // a stamped row: the catalog's when the stamp matches its text; an unstamped v1 predates the
    // stamps (registered by the catalog on 2026-09-19 before they existed) — an operator edit is v2+
    let stamped = r.criteria.iter().any(|c| c.starts_with("catalog:"));
    if stamped { r.criteria.iter().any(|c| c == &format!("catalog:{}", text_hash(&r.template_md))) } else { r.version == 1 }
}

/// Register every catalog entry whose base has no active row — or whose active row is the catalog's
/// own (unedited) text and the catalog changed. Returns the bases registered.
pub fn ensure_in(conn: &Connection) -> Vec<String> {
    let mut made = vec![];
    for t in CATALOG {
        if let Some(active) = resolve_in(conn, t.base) {
            let changed = active.template_md != t.template_md;
            if !(changed && is_catalog_row(&active)) { continue; }
        }
        let input = json!({
            "name": t.base, "template_md": t.template_md, "default_tier": t.tier, "default_temperature": t.temperature,
            "max_output_tokens": 8192,
            "output_fields": [{ "field_name": t.output.0, "field_type": t.output.1, "required": true, "description": t.purpose }],
            "judging_criteria": [t.purpose, format!("catalog:{}", text_hash(t.template_md))],
        });
        match antiloki_rs::ai_prompt_add::promptAdd(&input, conn) {
            Ok(_) => made.push(t.base.to_string()),
            Err(e) => eprintln!("antiloki-serve: prompt catalog: {} not registered — {e}", t.base),
        }
    }
    made
}
pub fn ensure() -> Vec<String> {
    let Some(db) = DB.get() else { return vec![] };
    let c = db.lock().unwrap_or_else(|p| p.into_inner());
    let made = ensure_in(&c);
    // warm the cache so a call made while a dispatch holds the lock still renders the DB's version
    let mut m = cache().write().unwrap_or_else(|p| p.into_inner());
    for t in CATALOG { m.insert(t.base.to_string(), resolve_in(&c, t.base)); }
    made
}

/// One finished CLI call under a template → `brain.ai_calls` (what Statistics and the Prompts
/// screen count). `provider` is "cli", `model` the engine key. Best-effort: never fails a call.
pub struct CallRecord<'a> { pub base: &'a str, pub tier: &'a str, pub engine: &'a str, pub model: Option<&'a str>, pub tokens_in: i64, pub tokens_out: i64, pub cost_usd: f64, pub duration_ms: i64, pub ok: bool, pub error: Option<&'a str>, pub input_hash: &'a str }

pub fn record_in(conn: &Connection, r: &CallRecord) -> Option<i64> {
    let tpl = resolve_in(conn, r.base)?;
    let tier = if ["low", "mid", "high"].contains(&r.tier) { r.tier } else { tpl.tier.as_str() };
    let envelope = json!({ "engine": r.engine, "model": r.model, "template": tpl.name, "version": tpl.version, "error": r.error });
    conn.execute(
        "INSERT INTO brain.ai_calls (prompt_template_id, tier, temperature, provider, model_id, input_hash, duration_ms, cost_usd_actual, tokens_in, tokens_out, cached, envelope_json, outcome) \
         VALUES (?1, ?2, ?3, 'cli', ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?11)",
        rusqlite::params![tpl.id, tier, tpl.temperature, r.model.map(|m| format!("{}:{m}", r.engine)).unwrap_or_else(|| r.engine.to_string()), r.input_hash, r.duration_ms, r.cost_usd, r.tokens_in, r.tokens_out, envelope.to_string(), if r.ok { "success" } else { "error" }],
    ).ok()?;
    Some(conn.last_insert_rowid())
}
/// Record on a side thread (the connection may be held by the very dispatch that made this call).
pub fn record(r: &CallRecord) {
    let owned = OwnedRecord { base: r.base.into(), tier: r.tier.into(), engine: r.engine.into(), model: r.model.map(String::from), tokens_in: r.tokens_in, tokens_out: r.tokens_out, cost_usd: r.cost_usd, duration_ms: r.duration_ms, ok: r.ok, error: r.error.map(String::from), input_hash: r.input_hash.into() };
    with_conn_later(move |c| { let _ = record_in(c, &owned.borrow()); });
}
struct OwnedRecord { base: String, tier: String, engine: String, model: Option<String>, tokens_in: i64, tokens_out: i64, cost_usd: f64, duration_ms: i64, ok: bool, error: Option<String>, input_hash: String }
impl OwnedRecord {
    fn borrow(&self) -> CallRecord<'_> { CallRecord { base: &self.base, tier: &self.tier, engine: &self.engine, model: self.model.as_deref(), tokens_in: self.tokens_in, tokens_out: self.tokens_out, cost_usd: self.cost_usd, duration_ms: self.duration_ms, ok: self.ok, error: self.error.as_deref(), input_hash: &self.input_hash } }
}

/// A cheap stable hash of the rendered input (ai_calls.input_hash is NOT NULL).
pub fn input_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() { h ^= *b as u64; h = h.wrapping_mul(0x100000001b3); }
    format!("cli-{h:016x}")
}

/// What the FE shows: the catalog with the active version of each base.
pub fn list() -> Vec<Value> {
    CATALOG.iter().map(|t| {
        let active = resolve(t.base);
        json!({ "base": t.base, "purpose": t.purpose, "tier": t.tier, "temperature": t.temperature,
                "version": active.as_ref().map(|a| a.version), "name": active.as_ref().map(|a| a.name.clone()),
                "template_md": active.as_ref().map(|a| a.template_md.clone()).unwrap_or_else(|| t.template_md.to_string()),
                "registered": active.is_some() })
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn m(pairs: &[(&str, Value)]) -> Map<String, Value> { pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect() }

    #[test]
    fn render_substitutes_sections_and_inverse_sections() {
        let t = "a {{x}} {{#y}}Y={{y}} {{/y}}{{^y}}no y {{/y}}{{#z}}z{{/z}}end";
        assert_eq!(render(t, &m(&[("x", json!("1")), ("y", json!("2"))])), "a 1 Y=2 end");
        assert_eq!(render(t, &m(&[("x", json!(""))])), "a  no y end");
        assert_eq!(render("{{#n}}n={{n}}{{/n}}", &m(&[("n", json!(0))])), "");
        assert_eq!(render("{{#n}}n={{n}}{{/n}}", &m(&[("n", json!(3))])), "n=3");
        assert_eq!(render("{{list}}", &m(&[("list", json!(["a", "b"]))])), "a, b");
        assert_eq!(render("{{a}} {{/a}}", &m(&[])), " ", "a stray close renders as an unknown var — empty");
    }

    #[test]
    fn the_catalog_renders_the_goal_prompts_byte_identical_to_the_old_format_strings() {
        // the ROUTER prompt as goals.rs formatted it before the catalog existed
        let old = format!("You are the ROUTER and PLANNER of a goal run inside this repository. Look at the code first so every decision is real.
FIRST decide the ROUTE:
- tier: the model strength the work needs — low (mechanical: renames, docs, small clear edits, and a small new file written from a COMPLETE spec — when the ask already says exactly what to build, the writing is mechanical), mid (ordinary engineering), high (subtle logic, concurrency, security, design decisions, anything where a wrong answer is expensive);
- delegate: whether this deserves sub-agents at all. DELEGATION HAS A PRICE — a plan, a verifier per task, a join and a judge: about two minutes and a few high-tier calls before and after the work — so it pays only when the work is BIG: it would take one agent well over five minutes, or it cannot fit one agent's context, or the parts are genuinely large subsystems. false when it is ONE job (one file or one tight cluster, no independent parts) AND ALSO when the parts are independent but small — a handful of files of a hundred lines each from a complete spec is one job for one agent, faster and cheaper than a wave; true only when independent parts are each substantial and can run IN PARALLEL in waves. Measured (2026-09-21): eight small modules from a full spec — one agent 153 s, a wave of eight 192 s at twice the cost.
THEN, if delegating, decompose it into at most {n} tasks — as FEW as the work needs: one task per substantial subsystem or per few hundred lines of work, never one per file just because the files are independent (every task costs its own agent start, verifier and context). Each task gets its OWN tier — a mechanical sub-task stays low even inside a high goal.
Rules:
- each task OWNS the files it may {} — worktree-relative paths or folder globs; two tasks must NEVER own the same file;
- NEEDS lists the ids of tasks whose result this task depends on (empty when it can start at once) — keep the graph shallow, no cycles;
- ACCEPTANCE is 1–3 checkable statements a reviewer can verify from the task's report{} — never a test run, lint, prettier, or an antiloki tool: sub-agents are headless CLIs with file read/edit/grep and a shell in this box only — NO antiloki MCP tools, NO network or package installs, and the box's dependencies may not be installed (2026-09-19: two correct edits were rejected for acceptance that demanded `yarn test` and `antiloki_check`);
- PROMPT says exactly what to do, in which files, what to report.
Reply with ONE JSON object only, no prose:
{{\"tier\":\"low|mid|high\",\"delegate\":true|false,\"why\":\"one line\",\"tasks\":[{{\"id\":\"t1\",\"title\":\"…\",\"tier\":\"low|mid|high\",\"prompt\":\"…\",\"owns\":[\"path/or/glob\"],\"needs\":[],\"acceptance\":[\"…\"]}}]}}
(tasks may be empty when delegate is false)

GOAL: {goal}", "edit", " and diff", n = 4, goal = "fix it");
        let now = render(catalog_entry("goal.router_plan").unwrap().template_md, &m(&[("n", json!(4)), ("verb", json!("edit")), ("write", json!(true)), ("goal", json!("fix it"))]));
        assert_eq!(now, old);

        let old_v = format!("You are the VERIFIER of one task in a goal run. Judge ONLY against the acceptance criteria, ONLY from the material below — do not open files or run tools.

NOT SHOWN IS NOT FALSE. Fail a claim only when the material CONTRADICTS it, or when the acceptance line plainly is not done. A statement you simply cannot see evidence for — a fact about a file the diff does not include, a dependency the report does not quote — is UNVERIFIED, not wrong, and does not fail the task on its own. Documentation and reviews exist to state facts the diff does not contain; judging those as inventions fails correct work.

EVERY DEFECT YOU ASSERT MUST QUOTE ITS EVIDENCE from the material — the line of the report or diff that shows it. If you cannot quote the evidence, you do not have a finding. Never assert a compile error, a missing guard or a specific call unless the quoted text shows it: a confident wrong reason costs more than a missed defect, because the operator acts on it.{}
GOAL: {}
TASK: {} — {}
ACCEPTANCE:
{}

REPORT:
{}
{}

Reply with JSON only: {{\"passed\": true|false, \"reasons\": [\"one line per acceptance item — met or why not\"]}}", "", "g", "t", "p", "1. a", "rep", "");
        let now_v = render(catalog_entry("goal.verify").unwrap().template_md, &m(&[("goal", json!("g")), ("title", json!("t")), ("prompt", json!("p")), ("acceptance", json!("1. a")), ("report", json!("rep")), ("diff", json!(""))]));
        assert_eq!(now_v, old_v);
    }

    #[test]
    fn the_ultra_review_renders_with_and_without_a_focus() {
        let t = catalog_entry("xray.ultra_review").unwrap().template_md;
        let with = render(t, &m(&[("brief", json!("B")), ("focus", json!("auth"))]));
        assert!(with.contains("\nThe operator asked you to focus on: auth\nReport at most 12"));
        assert!(with.ends_with("=== X-RAY BRIEF ===\nB"));
        let without = render(t, &m(&[("brief", json!("B"))]));
        assert!(without.contains("read is not a finding.\nReport at most 12"));
        assert!(!without.contains("focus on"));
    }

    #[test]
    fn the_pack_system_prompt_has_three_shapes() {
        let t = catalog_entry("agent.pack_system").unwrap().template_md;
        let none = render(t, &m(&[("files", json!("")), ("index", json!(PACK_INDEX)), ("live", json!(false))]));
        assert_eq!(none, format!("antiloki measures this repo. Read {PACK_INDEX} if it exists."));
        let baked = render(t, &m(&[("files", json!(3)), ("index", json!(PACK_INDEX)), ("live", json!(false))]));
        assert!(baked.starts_with("antiloki has baked a CONTEXT PACK for this worktree: 3 file(s)"));
        assert!(!baked.contains("Live MCP tools"));
        let live = render(t, &m(&[("files", json!(3)), ("index", json!(PACK_INDEX)), ("live", json!(true)), ("delegate", json!(true)), ("max_subagents", json!(4))]));
        assert!(live.contains(" Live MCP tools (server \"antiloki\")") && live.contains("You ORCHESTRATE sub-agents") && live.contains("up to 4 at a time"));
        let quiet = render(t, &m(&[("files", json!(3)), ("index", json!(PACK_INDEX)), ("live", json!(true)), ("delegate", json!(false))]));
        assert!(quiet.contains("Live MCP tools") && !quiet.contains("ORCHESTRATE"), "delegation off drops the orchestration sentence");
        // MIN (2026-09-21): the pane is told exploration is off, before the tools it navigates with
        let min = render(t, &m(&[("files", json!(3)), ("index", json!(PACK_INDEX)), ("live", json!(true)), ("min", json!(true))]));
        assert!(min.contains("This pane is MIN: exploration is OFF") && min.contains("no intel for <path>"), "{min}");
        assert!(min.find("This pane is MIN").unwrap() < min.find("Live MCP tools").unwrap());
        assert!(!quiet.contains("MIN"));
    }

    // PLAN-THE-DESIGN S1: the rubric is ONE text. The judge, the critique and the kit splice `design_rubric()`; the bench copy (bench/design/judge_prompts.md) quotes the same five lines.
    #[test]
    fn the_design_rubric_is_one_registered_text_with_five_axes_and_the_bench_quotes_it_verbatim() {
        let t = catalog_entry("design.rubric").expect("the rubric is a catalog entry");
        for (i, axis) in ["Hierarchy", "Spacing and alignment", "Colour and contrast", "Typography", "Polish"].iter().enumerate() {
            assert!(t.template_md.contains(&format!("{}. **{axis}**", i + 1)), "axis {axis} missing from the rubric:\n{}", t.template_md);
        }
        assert!(t.template_md.contains("1 (poor) to 5 (excellent)") && !t.template_md.contains("{{"), "a rubric has no placeholders: it is spliced as it is");
        let md = include_str!("../../bench/design/judge_prompts.md");
        let block = md.split("<!-- RUBRIC-BEGIN -->").nth(1).and_then(|x| x.split("<!-- RUBRIC-END -->").next()).expect("the bench copy is marked").trim();
        assert!(block.lines().count() == 5 && t.template_md.contains(block), "the bench's rubric differs from the registered one:\n{block}");
        assert!(design_rubric().contains("Hierarchy"));
        // registered like every other prompt: Settings > Prompts lists it, an operator edit is never overwritten
        assert_eq!(CATALOG.iter().filter(|x| x.base == "design.rubric").count(), 1);
    }

    #[test]
    fn ensure_registers_every_base_once_and_record_lands_in_ai_calls() {
        // a minimal brain schema — the real one lives in open-db's DDL
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("ATTACH DATABASE ':memory:' AS brain;
            CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version));
            CREATE TABLE brain.prompt_output_fields (id INTEGER PRIMARY KEY AUTOINCREMENT, prompt_template_id INTEGER, field_name TEXT, field_type TEXT, enum_values TEXT, required INTEGER, description TEXT, order_index INTEGER, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT);
            CREATE TABLE brain.prompt_input_fields (id INTEGER PRIMARY KEY AUTOINCREMENT, prompt_template_id INTEGER, field_name TEXT, field_type TEXT, required INTEGER, description TEXT, order_index INTEGER);
            CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT (datetime('now')), prompt_template_id INTEGER NOT NULL, tier TEXT NOT NULL, temperature TEXT NOT NULL, provider TEXT, model_id TEXT, input_hash TEXT NOT NULL, duration_ms REAL, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER, tokens_cached INTEGER NOT NULL DEFAULT 0, cached INTEGER NOT NULL DEFAULT 0, cache_source_call_id INTEGER, parent_event_id INTEGER, envelope_json TEXT, outcome TEXT);").unwrap();
        let made = ensure_in(&conn);
        assert_eq!(made.len(), CATALOG.len(), "{made:?}");
        let again = ensure_in(&conn);
        assert!(again.is_empty(), "a second ensure registers nothing, but it registered {again:?}");
        let r = resolve_in(&conn, "xray.ultra_review").unwrap();
        assert_eq!((r.version, r.tier.as_str()), (1, "high"));
        // an operator edit (next version) wins over the catalog text
        conn.execute("UPDATE brain.prompt_templates SET superseded_by = 999 WHERE name = 'goal.join'", []).unwrap();
        conn.execute("INSERT INTO brain.prompt_templates (name, version, template_md, default_tier, default_temperature) VALUES ('goal.join', 2, 'edited {{goal}}', 'high', 'cold')", []).unwrap();
        assert_eq!(render(&resolve_in(&conn, "goal.join").unwrap().template_md, &m(&[("goal", json!("x"))])), "edited x");
        assert!(ensure_in(&conn).is_empty(), "the edited base still counts as registered");
        // the catalog's own row (unedited) follows a catalog change; the operator's edit does not
        conn.execute("UPDATE brain.prompt_templates SET template_md = 'an older catalog text' WHERE name = 'goal.verify' AND superseded_by IS NULL", []).unwrap();
        conn.execute("UPDATE brain.prompt_templates SET judging_criteria_json = ?1 WHERE name = 'goal.verify' AND superseded_by IS NULL", [serde_json::to_string(&vec!["p", &format!("catalog:{}", text_hash("an older catalog text"))]).unwrap()]).unwrap();
        assert_eq!(ensure_in(&conn), vec!["goal.verify".to_string()]);
        assert_eq!(resolve_in(&conn, "goal.verify").unwrap().version, 2);
        assert!(ensure_in(&conn).is_empty());
        let id = record_in(&conn, &CallRecord { base: "xray.ultra_review", tier: "high", engine: "claude-code", model: Some("opus"), tokens_in: 10, tokens_out: 5, cost_usd: 0.0, duration_ms: 1200, ok: true, error: None, input_hash: "h" }).unwrap();
        let (tpl, prov, model, out): (i64, String, String, String) = conn.query_row("SELECT prompt_template_id, provider, model_id, outcome FROM brain.ai_calls WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap();
        assert_eq!((tpl, prov.as_str(), model.as_str(), out.as_str()), (r.id, "cli", "claude-code:opus", "success"));
        assert!(record_in(&conn, &CallRecord { base: "nope", tier: "mid", engine: "codex", model: None, tokens_in: 0, tokens_out: 0, cost_usd: 0.0, duration_ms: 0, ok: false, error: Some("x"), input_hash: "h" }).is_none());
    }
}
