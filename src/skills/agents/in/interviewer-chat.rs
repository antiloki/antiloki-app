//! interviewer-chat.rs — twin of skills/agents/in/interviewer-chat.mjs (db, cross-db
//! brain.*/memory.*/knowledge.* + dispatches to already-twinned interviewer-* helpers).
//! NAMES ARE CONTRACT: default `interviewerChat`; named `fetchTier2Memory`, `fetchActiveCards`,
//! `fetchRecentTurns` (legacy helpers — not called from `interviewerChat` itself in the JS either,
//! kept as standalone exports for parity with the JS module's export list).
//!
//! PARITY-DEFERRED: `interviewerChat` (outside the `ANTILOKI_INTERVIEWER_CHAT_STUB` branch) ends
//! in `crate::ai_ai_call::aiCall` — the deferred network-AI stub (see ai-call.rs's own doc), which
//! always returns Err. Everything BEFORE that call is real, ported, reachable code: the required-
//! field guard, tier routing (+ the "never answer on low tier" conversation floor), the bundle
//! slice, the cold-start bootstrap-context block (itself wrapped exactly like the JS's non-fatal
//! try/catch — a failure there must never block the chat), the workspace brief, the pane-overlay
//! catalog/persona assembly, the session-intelligence digest, the skills index, and the full
//! input_kv assembly fed to aiCall. All of that is exercised directly by this file's own tests.
//! The STUB branch (`ANTILOKI_INTERVIEWER_CHAT_STUB=1`, used by JS REPL subprocess tests for a
//! deterministic canned reply with no AI call) is fully real and reachable — ported byte-for-byte
//! including `stub_reply`'s classification rules.
//!
//! Past the first `aiCall`, the JS's lint/escalate retry (a SECOND aiCall call), the output-field
//! parsing (suggested_args/ui_json/next_steps), and the final response object are ported
//! byte-for-byte from the JS but — exactly like book-run.rs's post-wave staging logic — are
//! UNREACHABLE by this file's own fixture, since the very first `aiCall` inside them already
//! short-circuits via `?`. Correctness there is by inspection + shape parity with the reachable
//! code above it, following book-run.rs's precedent for this exact situation.
//!
//! PARITY-NOTE (episodic memory): the JS wraps a real embedding call (`external/embed.mjs`, a
//! non-portable ML primitive — no Rust twin exists or is planned) in a non-fatal try/catch that
//! silently degrades `episodic_memory` to `'(no episodic matches)'` on ANY failure (missing model,
//! import error, etc). This twin reproduces exactly that degraded-but-legitimate JS behavior: the
//! embed step is skipped and the fallback string is always used — not an approximation of the
//! happy path, but the exact fallback path the JS itself takes whenever embeddings are unavailable.
//!
//! `context.actor` (used by the JS to seed `interviewerBootstrapContext`'s `user_identifier`) has
//! no carrier in this twin's `(input, db)` signature — passed as `null`, mirroring book-run.rs's
//! treatment of `provider_fn` (a JS function reference with no Rust equivalent to carry, since the
//! deferred aiCall boundary is where such values would be consumed anyway).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
fn first(rows: &Value) -> Option<Value> { rows.as_array().and_then(|a| a.first()).cloned() }
// `loadJson(s, fallback)`: parse a value's string form as JSON, fallback on falsy input or parse failure.
fn load_json_value(s: Option<&Value>, fallback: Value) -> Value {
    let Some(sv) = s.filter(|v| truthy(Some(*v))) else { return fallback };
    serde_json::from_str(&js_string(sv)).unwrap_or(fallback)
}

// v19: SESSION INTELLIGENCE block — the triage is now aware of the widgets/artefacts this session
// generated (always in context; the user references one with #name).
// v20: SKILLS INDEX — the agent Skill Library's when-to-use lines (progressive disclosure).
const PROMPT_NAME: &str = "interviewer.chat.v27";
const PANE_PROMPT_NAME: &str = "interviewer.pane.v3";

const PANE_PREAMBLE_MD: &str = r#"YOU ARE A VIBE PANE AGENT — one focused sub-agent among several, working inside a scoped slice of the workspace. Everything below the PANE CONTEXT is your shared triage engine; the pane context OVERRIDES generic behavior where they conflict.

PANE CONTEXT:

YOUR PERSONA (base identity + the operator's extra guidance for this session):
{{persona}}

YOUR SCOPE (you may only touch files matching these globs — edits outside are refused by the substrate):
{{pane_scope}}

MISSION (shared, read-only — includes the session's context docs):
{{pane_mission}}

COORDINATION LOG (other panes — respect their claims):
{{pane_coordination}}

CURRENT IN-SCOPE FILES (their FULL current content — already loaded for you):
{{pane_files}}

HOW YOU CHANGE FILES (the ONLY way — a prose answer changes NOTHING):
  When the operator asks you to create, add, change, or remove ANY file content,
  set kind='mutate' and suggest cockpit.pane.file.write with args
  {"path":"<worktree-relative path>","content":"<the FULL new file content>"}.
  One file per action. The content of your in-scope files is ALREADY above — do
  NOT ask to read those or announce "I'll read the file first"; emit the write in
  THIS turn. cockpit.pane.file.read exists only for files too large to inline.
  The substrate stages your write as a pending plan the operator Applies (or
  writes immediately when the pane is in auto mode) — you never need to ask
  permission yourself, and edits outside your scope are refused.

AMBIGUITY GATE — screen BEFORE you write (operator 2026-07-07):
  An edit ask is AMBIGUOUS when it underdetermines any of: WHERE the change goes
  (which file/section/line), WHAT FORM it takes (a comment? a string literal?
  visible UI text? a variable?), or WHICH of several matching files. Example:
  "add a string to this file, Obsession" — comment vs literal vs rendered text
  are all plausible. When that happens you MUST NOT guess: set kind='clarify',
  confidence 0.5, suggested_action_name=null, and ask ONE terse question laying
  out the 2-3 interpretations — the screening widget renders it as options.
  Only write when the ask pins down where + what form (or the operator already
  answered your screening).

"#;

const PROMPT_TEMPLATE_MD: &str = r#"You are PHASE 1 (TRIAGE) of a two-phase interviewer. A separate PRESENTER phase will write the human-facing reply AFTER you decide what to dispatch.

YOUR JOB — and ONLY your job:
  1. CLASSIFY the user's intent (kind).
  2. PICK the right canonical action (or none) from the catalog.
  3. ROUTE to the correct owning agent.
  4. EMIT optional next_steps as affordances for the user.

You are NOT writing the user-facing reply. That's the presenter's job.
Keep response_md to AT MOST ONE short clause — used only as a fallback
when the presenter is disabled. Empty string is preferred.

You CANNOT execute actions yourself. The substrate's gate runs them after the user confirms. You propose; the human or the gate decides.

YOU ARE STRICTLY READ-ONLY. You NEVER change code or files — you have no file-write tool and must never claim to have edited, created, or deleted a file. Your entire job is to READ, SUGGEST, show OPTIONS (next_steps), and SCREEN when unsure. If the user asks you to change code or a file, explain what you'd change and point them at the surface that does it (a vibe pane / the cockpit) — do not pretend to do it.

SUBSTRATE FACTS (for grounding):
  - 5 SQLite DBs: state, memory, brain, laws, knowledge
  - 10 agents: system, dev, qa, man, god, in (you), pl, tl, board-keeper, keeper
  - 156 actions across namespaces:
      board-keeper.card.*   pl.session.*   tl.sandbox.*
      contract.*            dev.*          qa.*
      system.driver.*       system.docs.*  system.memory.*  keeper.*
      system.action.find    system.skill.find    system.anatomy.html
  - Driver loop ('system.driver.run' / 'system.driver.tick') is the autonomous orchestrator.
  - Memory is curated by the 'keeper' agent (keeper.memory.* mutations, keeper.review for hygiene).

THIS WORKSPACE — the project you are operating in. When the user says "this project", "the codebase", "the repo", "the workspace", or asks about its architecture / structure / what it does, they mean THIS. You ALREADY KNOW it (below) — so NEVER ask "which project?" and never run a clarify screening for it:
{{workspace_brief}}

ANSWERING ABOUT THE PROJECT / CODE — exception to "response_md empty preferred":
  Conversational / overview / explanatory questions have NO catalog action and do NOT go through the presenter — so YOU must answer them directly, grounded in THIS WORKSPACE + the memory context. For "tell me about this project", "what does X do", "explain the architecture", "how is this organized", etc.: set kind='read', suggested_action_name=null, and WRITE a concise, specific answer (3-6 sentences) in response_md using the context above. Do NOT ask a clarifying question when the workspace context already lets you answer — answer first.

CONSOLIDATED MEMORY (tier-2 narratives — your grounding context):
{{memory_context}}

ACTIVE WORK (top of mind):
{{active_cards}}

RECENT TURNS (this session, for thread continuity):
{{recent_turns}}

WHERE YOU LEFT OFF (cross-session continuity, populated only on turn 0 of a new session):
{{bootstrap_context}}

EPISODIC MEMORY (similar things the user asked across past sessions):
{{episodic_memory}}

SESSION INTELLIGENCE — the widgets & artefacts generated in THIS session (always in your context). The user can reference one by typing #name. Treat the data below as ground truth you ALREADY have: use it to answer, and NEVER say you can't see a widget or artefact the session produced:
{{session_intelligence}}

SKILLS INDEX — the agent Skill Library (name — when to use). PROGRESSIVE DISCLOSURE: this is only the index; when the user's task matches a skill's when-to-use line, suggest system.skilllib.manage with args {"op":"get","slug":"<slug>"} to load its full method before answering:
{{skills_index}}

THE OPERATOR'S SCREEN — a deterministic manifest of what the user SEES right now: the layout, the docked widgets (with live state), the instruments rail, live agent panes, the fullscreen page being read. Serialized by the app itself (no vision). Treat it as ground truth about the visible workspace: when the user says "this widget", "this pane", "this paper", "why is it red" — resolve against this list FIRST, by #id, without asking them to specify. '(the screen manifest is empty)' simply means nothing beyond the chat is on stage:
{{screen}}

SCREEN QUESTIONS ARE ANSWERED, NEVER DISPATCHED. There is NO substrate action that reads the screen — the manifest above is already the complete answer material. For ANY question answerable from THE OPERATOR'S SCREEN alone ("what do you see on my screen", "what am I reading", "what's docked", "why is this pane red", "how much has this agent spent", "describe my workspace"):
  - kind='meta', suggested_action_name=null, confidence >= 0.9
  - write the FULL answer in response_md, quoting the manifest's #ids and numbers verbatim
  - if the manifest is empty, SAY it is empty ("only the chat is on stage") — do not ask which widget they mean
  Example: user says "what do you see on my screen?" → kind='meta', response_md lists the docked widgets, agent panes (status · violations · cost), and the fullscreen page, each by #id.

THE UI VERBS — you can OPERATE the screen, in TWO TIERS. When the user asks you to open / show / navigate / dock / reorder / generate something ON SCREEN (the UI itself — not substrate data), return ui_json: a JSON array (max 4) of commands from THIS whitelist ONLY.
GLOBAL TIER — works from ANY screen:
  {"verb":"navigate","to":"<path>"} — go to a screen. Paths: "/" (antiDesk + antiVibe) · "/operations/atlas" (antiIDE Code & Design) · "/docs" (antiDocs) · "/features" (antiFlow) · "/operations" (Harness) · "/kanban" · "/reviews" · "/ultra-review" · "/settings" · "/settings/cost" · "/settings/byok" · "/settings/memory" · "/settings/safety" · "/settings/personas" · "/settings/account"
  {"verb":"open_ide_view","view":"<view>","path":"<file path, optional>"} — antiIDE's sidebar tabs (navigates there first if needed). Views: review · docs · graph · diagram · history · diff · gitgraph · constitution · books (the Import Technical Book control center) · authors · insights · structure · describe · snapshot · gravity · stories · coverage · runner · telemetry. With "path" it also opens that file; the FILE REVIEW window is view="review" + path. On antiIDE, "open the graph" means exactly this: {"verb":"open_ide_view","view":"graph"}.
LOCAL TIER — valid ONLY when THE OPERATOR'S SCREEN lists them under "local actions" (each screen publishes its own map; a local verb the manifest does not list CANNOT execute there):
  {"verb":"open_widget","uid":"<uid>"} — open a widget from THE OPERATOR'S SCREEN fullscreen (uid without the #)
  {"verb":"close_widget","uid":"<uid>"} — DESTROY a docked widget's slot (it stays in the instruments history)
  {"verb":"close_all_widgets"} — destroy EVERY docked widget on the stage
  {"verb":"dock","uid":"<uid>"} — dock an instruments widget at the END of the queue
  {"verb":"dock_move","uid":"<uid>","pos":2} — reorder: move a docked widget to position N among the docked widgets
  {"verb":"summon","command":"/graph"} — generate a widget into the chat via the slash palette: /graph /xray /design [dir] /review <path> /docs [path] /constitution [path] /authors /code /git /board /cost /queue /memory [q]
Rules:
  - ui_json ONLY when the user asked you to DO something on screen; kind='meta'.
  - TIER DISCIPLINE: GLOBAL verbs are always available. LOCAL verbs only when this screen's manifest lists them under "local actions" — if the user asks for a local action that belongs to ANOTHER screen, first navigate (global), and say the rest happens there.
  - "what can you do on this screen?" / "what can you do here?" → kind='meta', NEVER empty: list the GLOBAL tier plus THIS screen's "local actions" from the manifest, one line each, in plain words.
  - $0 ONLY. UI verbs open surfaces — screens, tabs, windows, widgets. They must NEVER start anything that costs AI: no swarm reviews, no antiDocs generation, no brainstorms, no video classes, no agent missions. If the user wants one of those, OPEN the screen where its button lives and say so ("opening the notebook — the generate button is there; generation costs AI, so it stays your click").
  - CONFIRMED BY THE USER: your commands render as a confirmation button; nothing runs until they click ✓. Phrase response_md as a ready offer naming the exact target ("ready to open the atlas graph — confirm below"), not as a done deed.
  - THE OFFER CARRIES THE COMMANDS: whenever response_md offers/announces a screen action ("ready to open X — confirm below"), ui_json in the SAME reply MUST contain the matching command(s). The confirmation button renders FROM ui_json — an offer with ui_json empty is a broken reply the user cannot accept. Never promise a screen action without its ui_json.
  - NEVER invent uids — only ones present in THE OPERATOR'S SCREEN. NEVER use ui_json for substrate reads/mutations (those go through suggested_action_name).

THE PREVIOUS EPISODE (this conversation continues from it — a consolidated snapshot):
{{prior_episode}}

PREVIOUS EPISODE rules: background memory, LOWER priority than the current turns. Use it to resolve references to recent work ("that paper", "the widget from before", "continue where we stopped") without re-asking. Never treat it as the current subject on its own.

TOPIC ANCHOR (the subject of the IMMEDIATELY PREVIOUS turn — if any):
{{topic_anchor}}

TOPIC ANCHOR — interpretation rules:
  - If TOPIC ANCHOR identifies a card slug and the user's input is a
    short follow-up (pronoun, fragment like "and the contract?", "what
    about the body?", "show it") — the user is almost certainly still
    talking about that card. Lift its slug into intent_summary and
    suggest the action that reads/operates on that slug.
  - In that case: emit kind appropriate to the question and a confidence
    >= 0.85. The substrate has already given you the anchor; don't
    over-question.
  - If the user's input is substantive (full sentence, new subject,
    starts with a new noun phrase, mentions a different slug) — IGNORE
    the topic anchor. Treat as a fresh classification.
  - If TOPIC ANCHOR is "(none)" — treat as a fresh classification too.

RESPONSE_MD — FALLBACK ONLY (presenter handles framing):
  - Empty string is the DEFAULT and PREFERRED.
  - If you must write anything, hard cap is ONE short clause (under
    60 chars). It will only render when the presenter is disabled or
    fails. Do NOT try to summarize the data the action will return —
    you haven't seen it yet.
  - DO NOT write "Here's X", "Pulling X now", "Running it" — these
    are anti-patterns from the old monolithic prompt. Forbidden.

KIND=START_WORK — when to choose it:
  Use kind='start_work' when the user names a SPECIFIC card slug AND
  wants the substrate's driver harness to start working on it. Trigger
  phrases (with a slug present in the question):
    - "do EPIC-X"
    - "start EPIC-X" / "start work on EPIC-X"
    - "ship EPIC-X"
    - "let's do EPIC-X"
    - "work on FEAT-Y"
    - "kick off X" / "begin X"
  When you pick start_work, set suggested_action_name='system.work.start'
  and suggested_args_json='{"slug": "<extracted slug>"}'.
  delegated_to_agent='system'. The substrate bumps cards.priority=5; the
  driver picks it on the next tick and routes through PL/contract/impl/qa.

  Do NOT use start_work when:
    - The user is asking for an overview ("what's next on the board") — that's 'read' or 'investigate'.
    - No specific slug is in the question ("just do something", "pick one") — that's 'clarify' (ask which one).
    - The card is already done or archived (mention it; do not start_work).

  Example:
    USER: "do EPIC-FE-TUI-VIEW-SHAPE-AUDIT"
    {
      "intent_summary": "User wants to start work on EPIC-FE-TUI-VIEW-SHAPE-AUDIT",
      "response_md": "",
      "kind": "start_work",
      "suggested_action_name": "system.work.start",
      "suggested_args_json": "{\"slug\": \"EPIC-FE-TUI-VIEW-SHAPE-AUDIT\"}",
      "delegated_to_agent": "system",
      "next_steps_json": ""
    }

KIND=INVESTIGATE — when to choose it:
  Use kind='investigate' when the user asks a NON-TRIVIAL question that
  requires reasoning across multiple substrate surfaces. Examples:
    - "why is the driver stuck?"
    - "how does the audit chain work?"
    - "what would break if I changed law X?"
    - "explain how PL sessions become contracts"
    - "where is the bottleneck in the embedding pipeline?"
  When you pick investigate, set suggested_action_name='system.investigate'
  and suggested_args_json='{"question": <verbatim user_text>}'.
  delegated_to_agent='in'. The substrate's investigate action handles
  research + reasoning + presentation deterministically.
  Do NOT use investigate for simple lookups (those are 'read') or for
  intent that requires a mutation (those are 'mutate'/'autonomous').

ACTION SELECTION — PICK THE RIGHT TOOL:
  When the user wants to SEE structured things (board, cards, memories),
  prefer actions that return STRUCTURED DATA over ones that return
  pre-rendered text:
    - board-keeper.card.list (array of {slug,title,status}) → preferred
    - board-keeper.log.board (ANSI string) → only if user explicitly
      asked for a "text/CLI/log" view
    - Same principle for *.list / *.detail / *.json over *.log.* / *.html
  The presenter + FE render rich views from structured data.

ACTION-SUGGESTION RULES — RESTRAINT BY DEFAULT:
  - Do NOT suggest system.docs.* or any other render action unless the user EXPLICITLY asked to see docs (e.g., "show me the docs", "explain X", "render Y"). For greetings, ambiguous input, or general questions: leave suggested_action_name empty and put possibilities in next_steps instead.
  - Prefer next_steps over suggested_action_name when you're not 100% sure the user wants something dispatched right now. next_steps are visible affordances; suggested_action_name triggers an immediate run.
  - When in doubt, set kind='clarify' and ask one short follow-up question.

EVERYTHING GOES THROUGH ACTIONS — COMPOSE BEFORE YOU PROPOSE:
  Every user intent must map to a CANONICAL ACTION NAME from the
  ACTION CATALOG below. You do NOT have direct database access, you
  do NOT execute scripts, you do NOT improvise.

  But your job is to REASON about composition — not to demand a new
  action for every variation of a request. Filing a new card is the
  LAST resort, not the first. Workflow, in order:

    1. Read the user intent in its PLAIN, NON-MAXIMAL form. "Summary
       of cards" most likely means "grouped overview" (which log.board
       already gives) — not "AI-synthesized prose summary of every
       card body." Don't over-interpret to manufacture a gap.

    2. Scan the ACTION CATALOG. Look for the action whose DESCRIPTION
       matches the plain intent. Many actions are explicitly named
       *.log.*, *.list, *.summary, *.html — read the descriptions, not
       just the names. board-keeper.log.board literally describes
       itself as a summary; that's a summary action.

    3. If a single action fits → suggest it. Done.

    4. If a CHAIN of existing actions fits — suggest the FIRST one,
       put the rest in next_steps so the user can drive the chain.
       Example: "summarize each card's body" → card.list now, with
       next_steps pointing at card.html per slug. The user can pick.

    5. Reframe before you give up. Ask yourself: what's the closest
       existing action that gets the user 80% there? If you find it,
       suggest it AND mention the gap in one short sentence so the
       user can decide whether 80% is enough.

    6. ONLY if (1)-(5) all fail — meaning no existing action or chain
       can reasonably serve the intent — do you propose filing a new
       action card. Use kind='clarify', suggested_action_name=
       'board-keeper.card.add', and name the missing action concretely.

  Phrases you must NEVER write in response_md:
    BAD: "Need X to do Y — that's the next step."
    BAD: "I'd have to query the DB for that."
    BAD: "Let me pull the bodies."
    BAD: "Pulling full details is the next step."
    BAD: "No bulk-X action exists yet."   (premature gap claim — did
           you actually check whether log.* or list satisfies the
           plain reading first?)
  All of these are symptoms of the same anti-pattern: narrating around
  a perceived capability gap instead of reading the catalog carefully.

OUTPUT — JSON of this exact shape:
{
  "intent_summary":      "<one-sentence paraphrase of what the user wants>",
  "response_md":         "<2-5 sentence reply shown to the user>",
  "kind":                "read" | "mutate" | "meta" | "clarify" | "autonomous" | "curate" | "investigate" | "start_work",
  "suggested_action_name": "<canonical action name from catalog, or empty string>",
  "suggested_args_json":   "<JSON-encoded args object, or empty string>",
  "delegated_to_agent":  "<agent name that owns the suggested action; 'in' for read-only intents handled by you>",
  "next_steps_json":     "<JSON array of {hint, action_name} for 1-3 follow-ups, or empty string>",
  "confidence":          <your OWN honest 0.0-1.0 confidence, as a BARE NUMBER (e.g. 0.85, not a string), that this classification + reply genuinely and fully answers the user. Be honest: rate LOW (below 0.7) when you're guessing, lack the context/source to answer well, or are unsure which interpretation is right. High only when you're sure.>
}

DELEGATION RULES (read-only enforcement):
  - You (the 'in' agent) are READ-ONLY. You can dispatch read/meta actions inline (system.docs.*, system.memory.list, board-keeper.card.list, etc.) and set delegated_to_agent='in'.
  - For ANY mutation (card add, memory add, contract draft, sandbox create, etc.) you must NOT pretend to do it yourself. Propose the canonical action name + set delegated_to_agent to the agent that owns it:
      board-keeper.card.*           → delegated_to_agent: 'board-keeper'
      keeper.memory.*, keeper.review→ delegated_to_agent: 'keeper'
      pl.session.*, card.draft.auto → delegated_to_agent: 'pl'
      contract.*, tl.sandbox.*      → delegated_to_agent: 'tl'
      dev.*                         → delegated_to_agent: 'dev'
      qa.*                          → delegated_to_agent: 'qa'
      system.driver.*               → delegated_to_agent: 'system'
  - When ambiguous and you don't know which action fits, set kind='clarify' + suggested_action_name='' + delegated_to_agent='in' and ASK a clarifying question in response_md.

FEW-SHOT EXAMPLES (right vs wrong behavior):

  Example 1 — greeting → ONE sentence + affordances (NO action dispatched)
    USER: "hi"
    OUTPUT:
    {
      "intent_summary": "User is starting a session",
      "response_md": "Hey. What are we working on?",
      "kind": "clarify",
      "suggested_action_name": "",
      "suggested_args_json": "",
      "delegated_to_agent": "in",
      "next_steps_json": "[{\"hint\":\"see the current board\",\"action_name\":\"board-keeper.card.list\"},
    { key: 'prior_episode', value: String(episode_prior_md ?? '').slice(0, 4000) || '(none — this is the first episode)' },{\"hint\":\"ask Product Lead to draft a card\",\"action_name\":\"pl.session.start\"}]"
    }
    NOTE: suggested_action_name is empty. Greetings do NOT trigger a dispatch.
    The user can pick from next_steps if they want to act.

  Example 2 — read intent → run inline (delegated_to_agent='in')
    USER: "show me the board"
    OUTPUT:
    {
      "intent_summary": "User wants to see current cards",
      "response_md": "",
      "kind": "read",
      "suggested_action_name": "board-keeper.card.list",
      "suggested_args_json": "{}",
      "delegated_to_agent": "in",
      "next_steps_json": "[{\"hint\":\"drill into a card\",\"action_name\":\"board-keeper.card.html\"}]"
    }
    NOTE: empty response_md is correct — the kanban bubble IS the
    answer. No prose needed.

  Example 3 — mutate intent → DELEGATE, do NOT execute
    USER: "add a card for RAG citation hover"
    OUTPUT:
    {
      "intent_summary": "User wants to file a card for a UI feature",
      "response_md": "Sure — type 'go' to file it. Want to tweak the title first?",
      "kind": "mutate",
      "suggested_action_name": "board-keeper.card.add",
      "suggested_args_json": "{\"title\":\"RAG citation hover\"}",
      "delegated_to_agent": "board-keeper",
      "next_steps_json": ""
    }

  Example 4 — ambiguous → clarify, NO action
    USER: "do the thing"
    OUTPUT:
    {
      "intent_summary": "Intent unclear — needs more from the user",
      "response_md": "I want to make sure I route this right — are you trying to inspect something, file work, or kick off a session?",
      "kind": "clarify",
      "suggested_action_name": "",
      "suggested_args_json": "",
      "delegated_to_agent": "in",
      "next_steps_json": ""
    }

  Example 5 — off-topic chat → gentle redirect, no action
    USER: "tell me a joke"
    OUTPUT:
    {
      "intent_summary": "User is making small talk, off-topic",
      "response_md": "Not really my lane — I route work into the substrate. Want to peek at the board or have Product Lead draft a card?",
      "kind": "clarify",
      "suggested_action_name": "",
      "suggested_args_json": "",
      "delegated_to_agent": "in",
      "next_steps_json": ""
    }

  Example 6 — "summary" looks like a gap but ISN'T: compose existing
    USER: "give me a summary of the cards"
    [Don't panic about "no bulk-bodies action." Re-read the catalog:
     card.list returns slug+title+status — the chat renders that as a
     kanban grouped by status, which IS the summary the user wants.
     Prefer card.list (structured) over log.board (ANSI text) in chat.]
    OUTPUT:
    {
      "intent_summary": "User wants a high-level overview of the board",
      "response_md": "Grouped by status.",
      "kind": "read",
      "suggested_action_name": "board-keeper.card.list",
      "suggested_args_json": "{}",
      "delegated_to_agent": "in",
      "next_steps_json": "[{\"hint\":\"drill into one card\",\"action_name\":\"board-keeper.card.html\"},{\"hint\":\"CLI-style colored summary\",\"action_name\":\"board-keeper.log.board\"}]"
    }
    NOTE: card.list satisfies "summary" because the chat renders it as
    a kanban. log.board (ANSI text) goes in next_steps for the rare
    user who actually wants the CLI view.

  Example 7 — TRUE GAP → propose the missing action (last resort)
    USER: "give me a synthesized prose summary of every card body, ranked by risk"
    [Now there really is no fit: no existing action ranks-by-risk, and
     no LLM-summarization action exists at the catalog level. This is
     a genuine new capability.]
    OUTPUT:
    {
      "intent_summary": "User wants an LLM-synthesized, risk-ranked digest of all card bodies",
      "response_md": "No action ranks-and-synthesizes card bodies today. Want me to file one?",
      "kind": "clarify",
      "suggested_action_name": "board-keeper.card.add",
      "suggested_args_json": "{\"title\":\"board-keeper.card.digest — LLM-synthesized prose digest ranked by risk\",\"kind\":\"feature\",\"priority\":3}",
      "delegated_to_agent": "board-keeper",
      "next_steps_json": "[{\"hint\":\"in the meantime: board overview\",\"action_name\":\"board-keeper.log.board\"}]"
    }
    NOTE: only NOW is card.add justified, because no composition of
    existing actions can rank-by-risk + synthesize prose.

ACTION CATALOG (full list of dispatchable actions):
{{action_catalog}}

ATTACHED REFERENCES (cards / knowledge docs / DB rows the user pulled in with "@" — treat as authoritative context for THIS turn; the user is asking ABOUT these unless they say otherwise):
{{references}}

ATTACHED FILES (documents the user uploaded — their summaries/contents are grounding for THIS turn):
{{attachments}}

USER INPUT: {{user_text}}"#;

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "intent_summary",        "field_type": "string", "required": true },
        { "field_name": "response_md",           "field_type": "string", "required": false },
        { "field_name": "kind",                  "field_type": "enum", "enum_values": ["read","mutate","meta","clarify","autonomous","curate","investigate","start_work"], "required": true },
        { "field_name": "suggested_action_name", "field_type": "string", "required": false },
        { "field_name": "suggested_args_json",   "field_type": "string", "required": false },
        { "field_name": "delegated_to_agent",    "field_type": "string", "required": true },
        { "field_name": "next_steps_json",       "field_type": "string", "required": false },
        { "field_name": "confidence",            "field_type": "number", "required": false },
        // v23 UI COMMAND BUS: whitelisted screen verbs (JSON array), executed by the FE — empty when none.
        { "field_name": "ui_json",               "field_type": "string", "required": false },
    ])
}

const TIER2_LIMIT: i64 = 8;
const ACTIVE_CARDS_LIMIT: i64 = 5;
const RECENT_TURNS_LIMIT: i64 = 4;

pub fn fetchTier2Memory(db: &Connection) -> String {
    let mut arr = crate::db_json::query_rows(
        db,
        "SELECT id, title, guidance, COALESCE(rationale, '') AS rationale FROM memory.memories \
         WHERE type = 'narrative' AND COALESCE(superseded_by, '') = '' ORDER BY id DESC LIMIT ?",
        &[json!(TIER2_LIMIT)],
    ).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    if arr.is_empty() {
        arr = crate::db_json::query_rows(
            db,
            "SELECT id, title, guidance, '' AS rationale FROM memory.memories \
             WHERE type IN ('correction','decision') AND COALESCE(validated, 0) = 1 \
               AND COALESCE(superseded_by, '') = '' ORDER BY id DESC LIMIT ?",
            &[json!(TIER2_LIMIT)],
        ).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    }
    if arr.is_empty() { return "  (no consolidated memory yet — substrate is fresh)".to_string(); }
    arr.iter().enumerate().map(|(i, r)| {
        let guidance: String = js_string(r.get("guidance").unwrap_or(&Value::Null)).chars().take(260).collect();
        format!("  {}. [#{}] {}\n     → {}", i + 1, js_string(r.get("id").unwrap_or(&Value::Null)), js_string(r.get("title").unwrap_or(&Value::Null)), guidance)
    }).collect::<Vec<_>>().join("\n")
}

pub fn fetchActiveCards(db: &Connection) -> String {
    let arr = crate::db_json::query_rows(
        db,
        "SELECT slug, title, status FROM cards WHERE status NOT IN ('done','blocked','archived') ORDER BY updated_at DESC LIMIT ?",
        &[json!(ACTIVE_CARDS_LIMIT)],
    ).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    if arr.is_empty() { return "  (board is clear — no active cards)".to_string(); }
    arr.iter().map(|c| format!(
        "  - {} [{}] — {}",
        js_string(c.get("slug").unwrap_or(&Value::Null)),
        js_string(c.get("status").unwrap_or(&Value::Null)),
        js_string(c.get("title").unwrap_or(&Value::Null)),
    )).collect::<Vec<_>>().join("\n")
}

pub fn fetchRecentTurns(db: &Connection, session_id: &Value) -> String {
    if !truthy(Some(session_id)) { return "  (this is the first turn of a fresh session)".to_string(); }
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT user_text, classified_kind FROM interviewer_turns WHERE session_id = ? ORDER BY turn_index DESC LIMIT ?",
        &[session_id.clone(), json!(RECENT_TURNS_LIMIT)],
    ) {
        Ok(v) => v,
        Err(_) => return "  (session history unavailable)".to_string(),
    };
    let mut arr = rows.as_array().cloned().unwrap_or_default();
    if arr.is_empty() { return "  (this is the first turn of a fresh session)".to_string(); }
    arr.reverse();
    arr.iter().enumerate().map(|(i, t)| {
        let text: String = js_string(t.get("user_text").unwrap_or(&Value::Null)).chars().take(140).collect();
        let kind = if truthy(t.get("classified_kind")) { js_string(t.get("classified_kind").unwrap()) } else { "n/a".to_string() };
        format!("  {}. user: \"{}\" (last classified as {})", i + 1, text, kind)
    }).collect::<Vec<_>>().join("\n")
}

// try promptFind; on miss, promptAdd then promptFind again (mirrors JS ensurePrompt's try/catch).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).is_ok() {
        return Ok(());
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "low",
            "default_temperature": "cold",
            "max_output_tokens": 1200,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db)?;
    Ok(())
}

fn ensure_pane_prompt(db: &Connection) -> Result<(), String> {
    if crate::ai_prompt_find::promptFind(&json!({ "name": PANE_PROMPT_NAME }), db).is_ok() {
        return Ok(());
    }
    let template = format!("{PANE_PREAMBLE_MD}{PROMPT_TEMPLATE_MD}");
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PANE_PROMPT_NAME,
            "template_md": template,
            "default_tier": "low",
            "default_temperature": "cold",
            "max_output_tokens": 1200,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": PANE_PROMPT_NAME }), db)?;
    Ok(())
}

// Deterministic stub mode — used by REPL subprocess tests that can't inject a provider_fn
// through the test boundary. NOT a fallback for production; only fires when
// ANTILOKI_INTERVIEWER_CHAT_STUB=1.
fn stub_reply(user_text: &str) -> Value {
    let t = user_text.to_lowercase();
    let t = t.trim();
    let base = |intent_summary: &str, response_md: &str, kind: &str, suggested_action_name: Value, delegated_to_agent: &str, next_steps: Value| json!({
        "intent_summary": intent_summary,
        "response_md": response_md,
        "kind": kind,
        "suggested_action_name": suggested_action_name,
        "suggested_args": {},
        "delegated_to_agent": delegated_to_agent,
        "next_steps": next_steps,
    });

    static LIST_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let list_re = LIST_RE.get_or_init(|| regex::Regex::new(r"^(list|show|how many|render|view|display)\b").unwrap());
    if list_re.is_match(t) {
        if t.contains("card") {
            return base(
                "User wants to see the board", "Listing cards.", "read",
                json!("board-keeper.card.list"), "in",
                json!([{ "hint": "drill into a card", "action_name": "board-keeper.card.html" }]),
            );
        }
        if t.contains("memor") {
            return base("User wants to see memories", "Listing memories.", "read", json!("system.memory.list"), "in", json!([]));
        }
        return base("User wants a read", "OK, reading.", "read", Value::Null, "in", json!([]));
    }
    static ADD_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let add_re = ADD_RE.get_or_init(|| regex::Regex::new(r"^(add|create|file|new)\b").unwrap());
    if add_re.is_match(t) {
        return base(
            "User wants to file a card",
            "OK, I will propose that — board-keeper handles card writes.",
            "mutate", json!("board-keeper.card.add"), "board-keeper", json!([]),
        );
    }
    static DRIVE_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let drive_re = DRIVE_RE.get_or_init(|| regex::Regex::new(r"^(drive|run.* driver|tick)\b").unwrap());
    if drive_re.is_match(t) {
        return base(
            "User wants the driver loop", "Want me to hand off to the driver?",
            "autonomous", json!("system.driver.run"), "system", json!([]),
        );
    }
    base("User said hello", "Hello — tell me what you want to do.", "clarify", Value::Null, "in", json!([]))
}

// ai-call returns output_kv (rows from brain.ai_call_outputs). Collapse the rows into a flat object.
fn collapse(res: &Value) -> Value {
    let mut o = Map::new();
    if let Some(arr) = res.get("output_kv").and_then(|v| v.as_array()) {
        for row in arr {
            let Some(field) = row.get("field_name").and_then(|v| v.as_str()) else { continue };
            if let Some(v) = row.get("value_text").filter(|v| !v.is_null()) {
                o.insert(field.to_string(), v.clone());
            } else if let Some(v) = row.get("value_number").filter(|v| !v.is_null()) {
                o.insert(field.to_string(), v.clone());
            } else if let Some(v) = row.get("value_boolean").filter(|v| !v.is_null()) {
                o.insert(field.to_string(), v.clone());
            }
        }
    }
    Value::Object(o)
}

const PANE_FILE_TOOLS_MD: &str = "[pane file tools — your ONLY way to change files]\n  cockpit.pane.file.read (in) — {\"path\":\"<worktree-relative>\"} → current content\n  cockpit.pane.file.write (in) — {\"path\":\"<worktree-relative>\",\"content\":\"<FULL new content>\"} → staged for Apply (or written now in auto mode)";

pub fn interviewerChat(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));

    let user_text_v = get("user_text").cloned().unwrap_or(Value::Null);
    // JS: `if (!user_text) throw err.required('interviewer-chat', 'user_text')`
    if !truthy(Some(&user_text_v)) {
        return Err("interviewer-chat: user_text is required".to_string());
    }
    let user_text = js_string(&user_text_v);

    let session_id = get("session_id").cloned().unwrap_or(Value::Null);
    let topic_anchor_str = match get("topic_anchor") {
        None | Some(Value::Null) => "(none)".to_string(),
        Some(v) => js_string(v),
    };
    let screen_md = get("screen_md").cloned().unwrap_or(Value::Null);
    let references = match get("references") { Some(v @ Value::Array(_)) => v.clone(), _ => json!([]) };
    let attachments = match get("attachments") { Some(v @ Value::Array(_)) => v.clone(), _ => json!([]) };
    let model_id = get("model_id").cloned().unwrap_or(Value::Null);
    let pane_overlay = get("pane_overlay").filter(|v| !v.is_null()).cloned();

    // Test-mode stub: deterministic canned reply (no AI call). Production never sets this env var.
    if std::env::var("ANTILOKI_INTERVIEWER_CHAT_STUB").ok().as_deref() == Some("1") {
        let s = stub_reply(&user_text);
        let routing = crate::agents_in_interviewer_route_tier::interviewerRouteTier(&json!({ "user_text": user_text }), db)?;
        return Ok(json!({
            "intent_summary": s.get("intent_summary").cloned().unwrap_or(json!("")),
            "response_md": s.get("response_md").cloned().unwrap_or(Value::Null),
            "kind": s.get("kind").cloned().unwrap_or(Value::Null),
            "suggested_action_name": s.get("suggested_action_name").cloned().unwrap_or(Value::Null),
            "suggested_args": s.get("suggested_args").cloned().unwrap_or(json!({})),
            "delegated_to_agent": s.get("delegated_to_agent").cloned().unwrap_or(json!("in")),
            "next_steps": s.get("next_steps").cloned().unwrap_or(json!([])),
            "ai_call_id": Value::Null,
            "prompt_name": PROMPT_NAME,
            "routing": routing.clone(),
            "context_bundle": {
                "memory_context": "  (stub: no narratives injected)",
                "active_cards":   "  (stub: no active cards injected)",
                "recent_turns":   "  (stub: no recent turns injected)",
                "action_catalog": "  (stub: catalog elided)",
                "bootstrap_context": "(stub)",
                "episodic_memory":   "(stub)",
            },
            "ai_telemetry": {
                "phase": "triage", "prompt_name": PROMPT_NAME,
                "model_id": "stub", "provider": "stub",
                "tier": routing.get("tier").cloned().unwrap_or(Value::Null),
                "temperature": routing.get("temperature").cloned().unwrap_or(Value::Null),
                "tokens_in": 0, "tokens_out": 0,
                "cost_usd": 0, "cached": false, "duration_ms": 0,
            },
        }));
    }

    let is_pane = pane_overlay.is_some();
    let prompt_name = if is_pane { PANE_PROMPT_NAME } else { PROMPT_NAME };
    if is_pane { ensure_pane_prompt(db)?; } else { ensure_prompt(db)?; }

    // FEAT-INTERVIEWER-SMART-V2 step 2: pick tier + temperature per turn.
    let mut routing = crate::agents_in_interviewer_route_tier::interviewerRouteTier(&json!({ "user_text": user_text.clone() }), db)?;
    // CONVERSATION FLOOR (operator 2026-06-30): never answer on the LOW coder model; floor at mid.
    if routing.get("tier").and_then(|v| v.as_str()) == Some("low") {
        routing["tier"] = json!("mid");
    }
    let tier = js_string(routing.get("tier").unwrap_or(&json!("mid")));
    let temperature = routing.get("temperature").cloned().unwrap_or(json!("cold"));

    // FEAT-BUNDLE-SLICING-RAG (Phase 2): per-tier context budgets.
    let slice = crate::agents_in_interviewer_bundle_slice::interviewerBundleSlice(
        &json!({ "user_text": user_text.clone(), "session_id": session_id.clone(), "tier": tier.clone() }),
        db,
    )?;
    let memory_context = js_string(slice.get("memory_context").unwrap_or(&Value::Null));
    let active_cards = js_string(slice.get("active_cards").unwrap_or(&Value::Null));
    let recent_turns = js_string(slice.get("recent_turns").unwrap_or(&Value::Null));
    let action_catalog = js_string(slice.get("action_catalog").unwrap_or(&Value::Null));

    // FEAT-COLD-START-BOOTSTRAP (Phase 3): on turn 0 of a NEW session, auto-end stale prior
    // sessions then pull last-session context. Bootstrap failure must NEVER block the chat —
    // mirrors the JS's non-fatal try/catch by simply ignoring any Err from this block.
    let bootstrap_context = (|| -> Result<String, String> {
        let turn_count: i64 = if truthy(Some(&session_id)) {
            let rows = crate::db_json::query_rows(db, "SELECT COUNT(*) AS c FROM interviewer_turns WHERE session_id=?", &[session_id.clone()])?;
            first(&rows).and_then(|r| r.get("c").and_then(|v| v.as_i64())).unwrap_or(0)
        } else {
            0
        };
        if turn_count <= 1 {
            let _ = crate::agents_in_interviewer_session_stale_end::interviewerSessionStaleEnd(&json!({}), db);
            let boot = crate::agents_in_interviewer_bootstrap_context::interviewerBootstrapContext(
                &json!({ "user_identifier": Value::Null, "current_session_id": session_id.clone() }),
                db,
            )?;
            Ok(js_string(boot.get("context_md").unwrap_or(&Value::Null)))
        } else {
            Ok(String::new())
        }
    })().unwrap_or_default();
    let bootstrap_context = if bootstrap_context.is_empty() { "(no prior context)".to_string() } else { bootstrap_context };

    // FEAT-CROSS-SESSION-RAG: episodic memory — see module doc's PARITY-NOTE. `embed` is a real
    // ML primitive with no Rust twin; the JS's own non-fatal catch means the deterministic
    // fallback below IS the faithful port, not an approximation of the happy path.
    let episodic_memory = "(no episodic matches)".to_string();

    // SPECIALIST CONTEXT: a deterministic brief of the active workspace.
    let workspace_brief = match crate::agents_in_interviewer_workspace_brief::interviewerWorkspaceBrief(&json!({}), db) {
        Ok(v) => {
            let b = v.get("brief_md").and_then(|x| x.as_str()).unwrap_or("");
            if b.is_empty() { "(this workspace has no atlas/git index yet)".to_string() } else { b.to_string() }
        }
        Err(_) => "(workspace context unavailable)".to_string(),
    };

    // EPIC-VIBE-CHAT-UNIFICATION M2: a pane's catalog is grant-filtered to its base agent.
    let mut pane_catalog: Option<String> = None;
    if let Some(po) = &pane_overlay {
        if let Some(base_agent) = po.get("base_agent").filter(|v| truthy(Some(*v))) {
            if let Ok(cat) = crate::agents_in_interviewer_action_catalog::interviewerActionCatalog(&json!({ "for_agent": base_agent }), db) {
                pane_catalog = cat.get("catalog").and_then(|c| c.as_str()).map(|s| s.to_string());
            }
        }
    }
    let catalog_for_prompt = if is_pane {
        format!("{}\n{}", pane_catalog.unwrap_or_else(|| action_catalog.clone()), PANE_FILE_TOOLS_MD)
    } else {
        action_catalog.clone()
    };

    // SESSION INTELLIGENCE (operator 2026-07-25): the session's own generated widgets + artefacts.
    let session_intelligence = crate::agents_in__helpers_session_intelligence::sessionIntelligenceDigest(db, &session_id, &json!({}));
    let session_intelligence = session_intelligence.as_str().unwrap_or("(session intelligence unavailable)").to_string();

    // SKILLS INDEX (operator 2026-07-25): when-to-use lines only.
    let skills_index = (|| -> Result<String, String> {
        let rows = crate::db_json::query_rows(db, "SELECT slug, name, description FROM knowledge.skill_library WHERE enabled = 1 ORDER BY name ASC LIMIT 40", &[])?;
        let arr = rows.as_array().cloned().unwrap_or_default();
        if arr.is_empty() { return Ok("(the skill library is empty)".to_string()); }
        Ok(arr.iter().map(|r| format!(
            "- {} ({}) — {}",
            js_string(r.get("name").unwrap_or(&Value::Null)),
            js_string(r.get("slug").unwrap_or(&Value::Null)),
            js_string(r.get("description").unwrap_or(&Value::Null)).replace('\n', " "),
        )).collect::<Vec<_>>().join("\n"))
    })().unwrap_or_else(|_| "(skill library unavailable)".to_string());

    let screen_str = {
        let raw = if screen_md.is_null() { String::new() } else { js_string(&screen_md) };
        // 20000, not 6000 (2026-09-19): the screen now carries PAGES — the X-ray brief, the open
        // file's dossier — so the chat knows what the operator is looking at, numbers included
        let sliced: String = raw.chars().take(20000).collect();
        let trimmed = sliced.trim().to_string();
        if trimmed.is_empty() { "(the screen manifest is empty)".to_string() } else { trimmed }
    };

    let mut input_kv: Vec<Value> = vec![
        json!({ "key": "user_text",            "value": user_text }),
        json!({ "key": "workspace_brief",      "value": workspace_brief }),
        json!({ "key": "memory_context",       "value": memory_context }),
        json!({ "key": "active_cards",         "value": active_cards }),
        json!({ "key": "recent_turns",         "value": recent_turns }),
        json!({ "key": "action_catalog",       "value": catalog_for_prompt }),
        json!({ "key": "bootstrap_context",    "value": bootstrap_context }),
        json!({ "key": "episodic_memory",      "value": episodic_memory }),
        json!({ "key": "session_intelligence", "value": session_intelligence }),
        json!({ "key": "skills_index",         "value": skills_index }),
        json!({ "key": "screen",               "value": screen_str }),
        // FIX-INTERVIEWER-CONTEXT-GROUNDING-FROM-RECENT-TURNS
        json!({ "key": "topic_anchor",         "value": topic_anchor_str }),
    ];
    if let Some(po) = &pane_overlay {
        let persona_raw = po.get("persona").filter(|v| !v.is_null()).map(js_string).unwrap_or_default();
        let persona: String = persona_raw.chars().take(8000).collect();
        let persona = if persona.is_empty() { "(a focused implementation agent working within its scope)".to_string() } else { persona };

        let scope_raw = match po.get("scope") {
            Some(Value::Array(a)) => a.iter().map(js_string).collect::<Vec<_>>().join("\n"),
            other => other.filter(|v| !v.is_null()).map(js_string).unwrap_or_default(),
        };
        let scope = if scope_raw.is_empty() { "(unrestricted)".to_string() } else { scope_raw };

        let mission: String = po.get("mission_md").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "(none)".to_string()).chars().take(24000).collect();
        let coordination: String = po.get("coordination").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "(none)".to_string()).chars().take(8000).collect();
        let pane_files: String = po.get("pane_files").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "(none)".to_string()).chars().take(120000).collect();

        input_kv.push(json!({ "key": "persona",           "value": persona }));
        input_kv.push(json!({ "key": "pane_scope",        "value": scope }));
        input_kv.push(json!({ "key": "pane_mission",      "value": mission }));
        input_kv.push(json!({ "key": "pane_coordination", "value": coordination }));
        input_kv.push(json!({ "key": "pane_files",        "value": pane_files }));
    }

    // ── PARITY-DEFERRED from here on (see module doc): `crate::ai_ai_call::aiCall` always
    // returns Err (network dispatch, no fixture). `provider_fn` (a JS function reference) has no
    // Rust carrier — passed as `null`, mirroring book-run.rs's identical treatment.
    let ai_input = json!({
        "prompt_name": prompt_name,
        "input_kv": input_kv,
        "tier": tier.clone(),
        "temperature": temperature.clone(),
        "model_id": model_id.clone(),
        "provider_fn": Value::Null,
        "references": references.clone(),
        "attachments": attachments.clone(),
        "intent": "interviewer.turn",
    });
    let mut ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;
    let mut out = collapse(&ai_result);

    // LINT + ESCALATE (operator 2026-06-22): a usable reply needs non-empty response_md OR a
    // delegated action/next_steps. Escalate once at the next tier up on a truly empty turn or
    // low self-confidence. UNREACHABLE past the `?` above — ported byte-for-byte for parity,
    // exactly like book-run.rs's post-wave staging logic (correctness by inspection).
    let next_tier = |t: &str| -> Option<&'static str> { match t { "low" => Some("mid"), "mid" => Some("high"), _ => None } };
    let has_next_steps = matches!(load_json_value(out.get("next_steps_json"), json!([])), Value::Array(a) if !a.is_empty());
    let self_conf: Option<f64> = out.get("confidence").map(|v| js_number(Some(v))).filter(|c| c.is_finite());
    let response_md_trim = js_string(out.get("response_md").unwrap_or(&Value::Null)).trim().to_string();
    let has_suggested = truthy(out.get("suggested_action_name"));
    let truly_empty = response_md_trim.is_empty() && !has_suggested && !has_next_steps;
    let low_conf = self_conf.map_or(false, |c| c < 0.7);

    let mut escalation: Option<(String, String, String)> = None;
    if !truthy(Some(&model_id)) && (truly_empty || low_conf) {
        if let Some(up_tier) = next_tier(&tier) {
            let reason = if truly_empty {
                format!(
                    "lint failed (empty response_md) at tier '{}' / model '{}' → escalated to '{}'",
                    tier, js_string(ai_result.get("model_id").unwrap_or(&json!("?"))), up_tier,
                )
            } else {
                format!(
                    "low self-confidence {:.2} (< 0.70) at tier '{}' → escalated to '{}' for a stronger answer",
                    self_conf.unwrap_or(0.0), tier, up_tier,
                )
            };
            let retry_input = json!({
                "prompt_name": prompt_name,
                "input_kv": input_kv,
                "tier": up_tier,
                "temperature": temperature.clone(),
                "provider_fn": Value::Null,
                "references": references.clone(),
                "attachments": attachments.clone(),
                "bypass_cache": true,
                "intent": format!("interviewer.turn.escalated — {reason}"),
            });
            if let Ok(retry) = crate::ai_ai_call::aiCall(&retry_input, db) {
                let retry_out = collapse(&retry);
                let retry_conf: Option<f64> = retry_out.get("confidence").map(|v| js_number(Some(v))).filter(|c| c.is_finite());
                let retry_response = js_string(retry_out.get("response_md").unwrap_or(&Value::Null)).trim().to_string();
                let accept = !retry_response.is_empty()
                    && (truly_empty || retry_conf.is_none() || self_conf.is_none() || retry_conf.unwrap() >= self_conf.unwrap());
                if accept {
                    ai_result = retry;
                    out = retry_out;
                    escalation = Some((tier.clone(), up_tier.to_string(), reason));
                }
            }
        }
    }

    let suggested_args = load_json_value(out.get("suggested_args_json"), json!({}));
    // v23: whitelisted UI commands — lenient: accept a single object with a "verb" string too.
    let ui_parsed = load_json_value(out.get("ui_json"), json!([]));
    let ui_list: Vec<Value> = match &ui_parsed {
        Value::Array(a) => a.clone(),
        Value::Object(o) if o.get("verb").and_then(|v| v.as_str()).is_some() => vec![ui_parsed.clone()],
        _ => vec![],
    };
    let ui_commands: Vec<Value> = ui_list.into_iter().filter(|c| c.get("verb").and_then(|v| v.as_str()).is_some()).take(4).collect();
    let next_steps: Vec<Value> = match load_json_value(out.get("next_steps_json"), json!([])) {
        Value::Array(a) => a.into_iter().take(3).collect(),
        _ => vec![],
    };

    let mut tokens_in = Value::Null;
    let mut tokens_out = Value::Null;
    if let Some(call_id) = ai_result.get("ai_call_id").filter(|v| !v.is_null()) {
        if let Ok(rows) = crate::db_json::query_rows(db, "SELECT tokens_in, tokens_out FROM brain.ai_calls WHERE id = ?", &[call_id.clone()]) {
            if let Some(row) = first(&rows) {
                tokens_in = row.get("tokens_in").cloned().unwrap_or(Value::Null);
                tokens_out = row.get("tokens_out").cloned().unwrap_or(Value::Null);
            }
        }
    }

    // FIX-INTERVIEWER-AUTODISPATCH-RESPONSE-RENDERING: skip the rephrase placeholder when the
    // AI actually produced a classification + suggested action.
    let ai_kind = out.get("kind").filter(|v| truthy(Some(*v))).cloned().unwrap_or(json!("clarify"));
    let ai_suggested = out.get("suggested_action_name").filter(|v| truthy(Some(*v))).cloned();
    let ai_response = out.get("response_md").cloned();
    let classification_succeeded = js_string(&ai_kind) != "clarify" || ai_suggested.is_some();
    let response_md_final = match &ai_response {
        Some(v) if !v.is_null() => v.clone(),
        _ => if classification_succeeded { json!("") } else { json!("I didn't catch that — could you rephrase?") },
    };

    Ok(json!({
        "intent_summary":        out.get("intent_summary").cloned().unwrap_or(json!("")),
        "response_md":           response_md_final,
        "kind":                  ai_kind,
        "suggested_action_name": ai_suggested.unwrap_or(Value::Null),
        "suggested_args":        suggested_args,
        "delegated_to_agent":    out.get("delegated_to_agent").filter(|v| truthy(Some(*v))).cloned().unwrap_or(json!("in")),
        "next_steps":            next_steps,
        "ui_commands":           ui_commands,
        "confidence": out.get("confidence").map(|v| js_number(Some(v))).filter(|c| c.is_finite()).map(|c| json!(c.max(0.0).min(1.0))).unwrap_or(Value::Null),
        "ai_call_id":            ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null),
        "prompt_name":           prompt_name,
        "routing":               routing,
        "context_bundle": {
            "memory_context": memory_context, "active_cards": active_cards, "recent_turns": recent_turns,
            "action_catalog": action_catalog, "bootstrap_context": bootstrap_context, "episodic_memory": episodic_memory,
        },
        "ai_telemetry": {
            "phase": "triage",
            "prompt_name": PROMPT_NAME,
            "model_id":    ai_result.get("model_id").cloned().unwrap_or(Value::Null),
            "provider":    ai_result.get("provider").cloned().unwrap_or(Value::Null),
            "tier":        ai_result.get("tier_used").cloned().unwrap_or(json!(tier)),
            "escalated_from":    escalation.as_ref().map(|e| json!(e.0)).unwrap_or(Value::Null),
            "escalation_reason": escalation.as_ref().map(|e| json!(e.2)).unwrap_or(Value::Null),
            "temperature": ai_result.get("temperature_used").cloned().unwrap_or(temperature),
            "tokens_in": tokens_in, "tokens_out": tokens_out,
            "cost_usd":    ai_result.get("cost_usd").cloned().unwrap_or(json!(0)),
            "cached":      ai_result.get("cached").cloned().unwrap_or(json!(false)),
            "duration_ms": ai_result.get("duration_ms").cloned().unwrap_or(Value::Null),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory; ATTACH ':memory:' AS brain; ATTACH ':memory:' AS knowledge;").unwrap();
        c.execute_batch(
            "CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, title TEXT NOT NULL, guidance TEXT NOT NULL, type TEXT NOT NULL, rationale TEXT, validated INTEGER, superseded_by INTEGER);
             CREATE TABLE memory.embeddings (id INTEGER PRIMARY KEY AUTOINCREMENT, memory_id INTEGER NOT NULL, vector BLOB);
             CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT, updated_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE interviewer_turns (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, turn_index INTEGER, ts TEXT NOT NULL DEFAULT (datetime('now')), user_text TEXT, classified_kind TEXT, user_text_embedding BLOB);
             CREATE TABLE interviewer_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, user_identifier TEXT, started_at TEXT, last_active_at TEXT, status TEXT, ended_at TEXT, context_summary_md TEXT);
             CREATE TABLE interviewer_session_widgets (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, type TEXT, kind TEXT, payload_json TEXT);
             CREATE TABLE actions (name TEXT, owner_agent TEXT, description TEXT);
             CREATE TABLE agent_actions (agent_name TEXT, action_name TEXT, revoked_at TEXT);
             CREATE TABLE workspaces (name TEXT, root_path TEXT, vcs TEXT, is_active INTEGER);
             CREATE TABLE knowledge.skill_library (slug TEXT, name TEXT, description TEXT, enabled INTEGER);
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, tokens_in INTEGER, tokens_out INTEGER);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn requires_user_text() {
        let c = setup();
        let err = interviewerChat(&json!({}), &c).unwrap_err();
        assert!(err.contains("user_text is required"));
    }

    #[test]
    fn stub_mode_greeting() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        std::env::set_var("ANTILOKI_INTERVIEWER_CHAT_STUB", "1");
        let c = setup();
        let r = interviewerChat(&json!({ "user_text": "hi there" }), &c).unwrap();
        std::env::remove_var("ANTILOKI_INTERVIEWER_CHAT_STUB");
        assert_eq!(r["kind"], json!("clarify"));
        assert_eq!(r["ai_call_id"], Value::Null);
        assert_eq!(r["prompt_name"], json!(PROMPT_NAME));
    }

    #[test]
    fn stub_mode_list_cards() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        std::env::set_var("ANTILOKI_INTERVIEWER_CHAT_STUB", "1");
        let c = setup();
        let r = interviewerChat(&json!({ "user_text": "show me the cards" }), &c).unwrap();
        std::env::remove_var("ANTILOKI_INTERVIEWER_CHAT_STUB");
        assert_eq!(r["kind"], json!("read"));
        assert_eq!(r["suggested_action_name"], json!("board-keeper.card.list"));
        assert_eq!(r["delegated_to_agent"], json!("in"));
    }

    #[test]
    fn stub_mode_add_card() {
        // ENV_LOCK: ANTILOKI_INTERVIEWER_CHAT_STUB is process-global, and the production_path_*
        // tests below assert the NON-stub path. Unguarded, this set_var leaks into them under
        // parallel test threads and they get a stub Ok where they expect the aiCall boundary.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        std::env::set_var("ANTILOKI_INTERVIEWER_CHAT_STUB", "1");
        let c = setup();
        let r = interviewerChat(&json!({ "user_text": "add a card for X" }), &c).unwrap();
        std::env::remove_var("ANTILOKI_INTERVIEWER_CHAT_STUB");
        assert_eq!(r["kind"], json!("mutate"));
        assert_eq!(r["suggested_action_name"], json!("board-keeper.card.add"));
        assert_eq!(r["delegated_to_agent"], json!("board-keeper"));
    }

    #[test]
    fn stub_mode_driver_tick() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        std::env::set_var("ANTILOKI_INTERVIEWER_CHAT_STUB", "1");
        let c = setup();
        let r = interviewerChat(&json!({ "user_text": "tick the driver" }), &c).unwrap();
        std::env::remove_var("ANTILOKI_INTERVIEWER_CHAT_STUB");
        assert_eq!(r["kind"], json!("autonomous"));
        assert_eq!(r["suggested_action_name"], json!("system.driver.run"));
    }

    #[test]
    fn production_path_reaches_the_parity_deferred_aicall_boundary() {
        // Proves every deterministic gate ran cleanly (prompt ensure, tier routing, bundle
        // slice, bootstrap context, workspace brief, session intelligence, skills index, input_kv
        // assembly) and the error is aiCall's deferred stub, not an earlier bug.
        // paired with the stub_mode_* tests' guard: this asserts the NON-stub path, so it must not
        // observe their ANTILOKI_INTERVIEWER_CHAT_STUB window.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let c = setup();
        let err = interviewerChat(&json!({ "user_text": "hello there" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT_NAME], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn production_path_pane_overlay_ensures_pane_prompt() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let c = setup();
        let _ = interviewerChat(&json!({ "user_text": "hi", "pane_overlay": { "persona": "p", "scope": ["src/**"], "base_agent": "dev" } }), &c);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PANE_PROMPT_NAME], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn fetch_active_cards_reports_open_cards_only() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('C1','Card One','doing')", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('C2','Card Two','done')", []).unwrap();
        let s = fetchActiveCards(&c);
        assert!(s.contains("C1"));
        assert!(!s.contains("C2"));
    }

    #[test]
    fn fetch_recent_turns_defaults_without_session() {
        let c = setup();
        assert_eq!(fetchRecentTurns(&c, &Value::Null), "  (this is the first turn of a fresh session)");
    }

    #[test]
    fn fetch_tier2_memory_falls_back_to_validated_corrections() {
        let c = setup();
        c.execute("INSERT INTO memory.memories (id, title, guidance, type, validated) VALUES (1, 'Correction', 'do X', 'correction', 1)", []).unwrap();
        let s = fetchTier2Memory(&c);
        assert!(s.contains("Correction"));
    }

    #[test]
    fn stub_reply_classifies_variants() {
        assert_eq!(stub_reply("list cards")["suggested_action_name"], json!("board-keeper.card.list"));
        assert_eq!(stub_reply("show memories")["suggested_action_name"], json!("system.memory.list"));
        assert_eq!(stub_reply("create a card")["kind"], json!("mutate"));
        assert_eq!(stub_reply("random words")["kind"], json!("clarify"));
    }
}
