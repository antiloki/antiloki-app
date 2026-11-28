# Changelog

## v1.0.0 — Loki

### 2026-11-27: worktrees belong to threads
- A new agent works in its thread's folder: your project folder, unless the thread has a worktree. It no longer gets a hidden copy of its own.
- A bar over the panes says where the thread works. "Create worktree" gives the thread its own branch and folder; its agents move there and keep their conversation (Claude Code resumes; the others get a handover). "Merge into project" and "Discard" are on the same bar.
- Uncommitted changes are copied into a new worktree only if you say so. Your project folder is not changed by that, so commit or discard them there before you merge.
- An agent can suggest a worktree (`antiloki_worktrees`, op `suggest`); you choose Create or Not now. It cannot make one.
- A warning appears when another thread also has agents in the project folder.
- Pairing agents from the screen gives each one a copy of its own (a team undoes and merges each member's files). Team runs keep their temporary copies.
- The "needs Git" question appears only when you ask for a worktree.
- New folders are named after the project (`shop-<id>-pricing`). Existing folders keep their names.
- Agents that share a folder have their own usage, cost, tools and sagas.
- Rewind stays limited to agents with a copy of their own.
- Codex, Cursor, OpenCode and Antigravity agents continue their own conversation after a terminal restart.
- A team thread in a project without Git offers "Start Git" on its bar.
- A single-model step's seat is picked from a list of what your computer has.
- A usage count that is still running is no longer reported as a failure.
- Antigravity, OpenCode and Grok agents use antiloki's own tools without asking, as Cursor and Codex already did.
- The cost and usage count is no longer slow when an agent asks after the screen: each time zone keeps its own count, and agents are answered in the screen's zone.
- A pane says "file limits not guaranteed" when its agent has file limits that it is not known to be held to (held: Claude Code, and OpenCode in its own copy).
- OpenCode 2 loads antiloki's plugin again: file limits are enforced in an agent's own copy, and its tool calls and prompts are recorded. A patch is now checked against the limits too.
- Chat view: a message sent while Claude is working now shows where Claude received it, and no longer leaves "Reasoning" running.
- After an update, an open agent's antiloki tools move to the new version at their next use, without restarting the agent (Linux; not Windows).
- Browser control: one window per profile (asking again goes to the same window); moving to another page of an opened site asks nothing; a page is read once it stops changing, frames of the same site included; new tabs are followed.
- Browser control: when a sign-in or a pop-up needs you, a card says so and the agent waits. A pop-up an agent's own click raises is answered the harmless way (a question: Cancel).
- Chat view: Claude Code's own summary of a long conversation, and its task notices, no longer show as your messages.
- Browser control: each agent has its own window (sign-ins are shared); No on a card means no; downloads ask first; a form with a password field no longer counts as a sign-in; a CAPTCHA calls you.
- Chat view: what an agent did is one line with icons that opens into the list.
- Browser control no longer stops answering on sites with background workers (seen on a Cloudflare check): after one such page every later call hung. A closed agent panel now closes its window whatever holds its terminal.
- Chat view: pasting an image attaches it.
- The side question button no longer gets stuck saying one is already open: the open one is shown again.
- New team: the leader asks what the team is for, names it, the "New team" thread is renamed and becomes the team's with your chat on it, and the leader builds the team. No run starts.

### 2026-11-27: provider switching
- Chat view shows Claude's "trust this folder" question, with the folder and both choices as buttons.
- An agent in a project without Git no longer shows "Reasoning" for 90 seconds after its answer, and its model is shown from its conversation.
- antiloki reads the agent's current screen before it types, so text of its own never goes into a question that is still open.
- Cursor no longer asks before each of antiloki's own tools (allowed in the agent's folder, `.cursor/cli.json`); if it asks about another tool, the pane says it needs you.
- Cursor (2026.11 and newer): the model and context share are read again. OpenCode: the model is shown.
- Opening a team thread starts nothing: its Leader waits for your "start" click, and is handed the team's memory only after that (#30). "+ new team" still starts at once, since you asked for it.
- A project without Git now asks in the middle of the screen: "Start Git" (makes the repository and one commit, leaving packages, builds and .env files out) or "Keep it as it is" (asked once per workspace).
- A new Claude agent shows its model in the chat view before its first answer.
- The list of installed agents is remembered and shown at once; picking one that was uninstalled says so.
- Chat view: a local model is named by its own tag ("gemma4:e4b", not "A4"), and the chat/terminal button stays in place (#28).
- A pane waiting on an Antigravity, OpenCode or Cursor permission question now says it needs you, and Cursor's command question shows as buttons in the chat (#8).
- Chat: what antiloki types itself (a handover, a setting command) is a small line, not your message, and is not listed among your asks; repeated switches are said once.
- Switching provider while a new team is being set up keeps the setup.
- Agent tools: `antiloki_team_templates` answers (it said "unknown tool"); a query without a source says so.
- Agent tools: team designs are listed; a team budget can be read; clearer refusals (unknown op, episode without a saga number, unknown specialist); a free pane that points outside is refused.
- A Cursor agent no longer asks to approve antiloki's own tools: they are approved before it starts (other servers are still asked about).
- Windows: the Cursor and Antigravity usage meters are read (the hidden terminal they use now answers it).
- In a project without Git, a new agent's chat view no longer shows an earlier agent's conversation; a Claude Code agent there continues its own after a restart.
- A narrow agent pane: the buttons that do not fit (open as its own app among them) open with a right click on its bar.
- One tooltip at a time: the browser's own no longer shows over antiloki's.
- Windows: the machine window reads the CPU and lists the processes.
- The queue of an agent has a Clear button that hides the tasks shown now.
- "New team" opens a thread of its own with the leader in it, instead of adding an agent to the thread you are in.
- Specialists can use antiloki's browser window (each site still needs the person's yes); a key that does not reach a terminal is said on screen.
- A specialist with no seat named runs on the person's own agent, not Claude; the seat is chosen from a list; a new pane's first message waits until its agent's own question is answered.
- An agent with no copy of its own (a project folder that is not a Git repository) now gets antiloki's tools too, and the screen says why it has no copy, or that the tools helper was not built.
- Windows reports: opening an agent says that it is opening, and why it failed; a signed-out agent (OpenCode) can be uncovered to sign in instead of staying behind "tools did not connect"; the "a." mark answers every click; the menu says who is not signed in.
- The list of agents in "new agent" always appears: a slow or stuck agent program (seen on Windows) can no longer keep it on "looking for installed agents".
- An agent whose worktree could not be made now starts in the project folder, never in the folder antiloki's server runs from.
- A handover no longer contains antiloki's own earlier takeover messages or setting commands (#23).
- Antigravity and Cursor panes now report when they are ready, so the takeover message can be typed into them; every other terminal with no known input screen is ready from the start (#8).
- A model name Claude Code cannot run (for example `gemma4:e4b` without `local:`) is refused before anything changes (#12).

### 2026-11-27: a pane as an app, and the built-in prompts
- Multiplex (the app closed, agents still running) shows the "a." mark with its ring of colour in place of the avatar, and a new window: a meter, a card per busy agent, motion. It is small and see-through until the pointer is on it.
- The desktop mark also shows while the app is minimised, can be dragged anywhere, and has a ✕ that closes the icon only. Its border is the theme's colour at rest, all colour while an agent runs, and orange while somebody is needed.
- Multiplex also installs from the browser as a small app (`/multiplex`), in one click from the start screen; a pin button puts it in a floating window kept above the others where the desktop allows.
- An agent, a terminal, or the files, notes, browser, board, connectors and group panes can be opened alone and installed from the browser as an app with its own name, icon and window. It needs antiloki running.
- A window shows the built-in prompts, word for word.

### 2026-11-27: no trial, no licence, no payments
- antiloki is free and open source. The trial, the licence keys and serials, the payment provider, the key tool, the relay and every check that could refuse a request are removed, from the server, the screen and the build scripts. The `/api/v1/license/*` routes no longer exist.
- The old screens are gone too: only Cowork, X-ray and Sagas have addresses.

### 2026-11-25: connectors, free panes, Windows fixes, specialists (browser-tested on a throwaway project; Windows fixes unit-tested only)
- **Connectors.** Ask any agent to connect a service ("connect my Google Drive") and it proposes a recipe with `antiloki_connector`; the **Connectors pane** opens by itself on that connector's screen with the agent's tutorial, a masked field for each key, and a test. Works for an HTTP API or a remote MCP server. The pane starts empty and lists connectors as icons (the name on hover). Secrets go to `.antiloki/.env` (mode 0600, kept out of git), never to the database, the recipe or an agent. A connector is **look-only** unless you switch it to look-and-change (it asks first). OAuth uses PKCE and renews an expired sign-in by itself; where a service lets an app register itself, or antiloki ships a client, **Connect** is one click with nothing to paste. Google stays bring-your-own-client. Design: `rfcs/0022-connectors-pane.md`.
- **Free panes.** An open pane reloads itself when an agent rewrites it. A new draft asks for approval in the floating card at the bottom right (what it can reach, in words); a pane that reaches no app opens by itself after 10 seconds unless rejected, one that reaches an app always waits for your click. In the pane a draft is one thin line, and Export is a small icon on the pane's top bar.
- **Specialists** can ask for connectors, make and open free panes, and read the sagas (read-only). Their brief now says to look in their memory, the raw conversation and the sagas before answering "I don't remember".
- **The "a." mark** on a pane's top bar reconnects the agent's tools when clicked (the agent starts again with its conversation); not in the middle of a turn.
- **Sagas.** The list on an agent's top bar shows each saga's newest episodes (one per answer). A standing brief or a paste tag no longer names a saga.
- **Windows and macOS.** Fixed a crash on Windows and macOS ("doesn't provide an export named FreePane") caused by file names that differ only by case; a test now forbids it. antiloki's own theme with seven wallpapers is built in, so a machine without an Omarchy desktop has wallpapers. Cursor: the chat finds its transcript for Windows paths, the usage reader starts Cursor's `.cmd` through `cmd /c`, and its MCP approval is one click in the chat. A new agent or the new-team leader is one the machine can run, not Claude Code regardless.
- **Found by running it, fixed.** A plain OpenCode pane was never wired to antiloki's tools (the gate could not lift); the tools helper says "connected" every 15 s so a restarted pane or serve reconnects; nothing is typed over an agent's own question (it quit Cursor through the approval menu's hotkeys); an OpenCode turn the provider refused shows its error in the chat; Cursor's approval note clears when answered; the provider menu and the saga list close on Escape.
- **Tests.** Journeys for the provider switch (`j35`), the new-team flow (`j36`) and opening the Connectors pane (`j37`); the tool-routing test; a guard for case-clashing file names.
- **RFC audit.** `rfcs/README.md` states what each RFC has built and what it has not.

### 2026-11-24: providers, chat, onboarding (tested by unit tests; the browser checks are listed in HANDOFF-2026-11-24.md §4)
- **Switch an agent's provider in the middle of a conversation.** The header button now picks the provider (Claude Code, Codex, Cursor, Antigravity, OpenCode) or a local model. The switch closes an episode in the saga, writes a reconciliation pack, and the new agent reads it through the MCP tool `antiloki_reconcile` (a local model gets the pack inside the first message).
- **Chat mode:** slash commands are suggested like in the terminal (and the values of `/model` and `/effort`); `/model <local model>` switches to it; Claude's "Switch model?" question and Cursor's MCP approval are answered or explained in the chat; chat mode also reads Codex conversations.
- **New team opens its leader**, which asks for the team's name and purpose and creates the team through the MCP tool `antiloki_team_setup` (the purpose is kept as the objective of a saga).
- **Fixes:** a resumed Claude pane was never "ready" (so `/model` and sends failed); a setting command no longer makes the pane read as working; Cursor's sign-in is asked of the CLI, and its login page is never opened on start; Enter in a terminal pane submits; the team's run button repeats the last run; the leader's Chat/Graph/Editor/Sequence switch is icons on the top bar; the Sagas list opens above the chat.
- The new-pane menu no longer lists the Studio tools or X-ray (`STUDIO_IN_MENU`); their code remains.

### Version 1.0: free, open source, no setup
- antiloki is free and open source (Apache-2.0). No trial, no key, no plan; nothing is sent to us.
- **No Settings page and no setup wizard.** A workspace opens straight into Cowork; the seats (Claude, Codex, Cursor, Antigravity, Grok) are detected from what is already installed and signed in on the machine.
- The retired screens (Heimdall router, Timeline, Knowledge, Benchmarks, Audit, Statistics, Prompts, Environments, Failures, Code checks, Other CLIs, Earlier agents, Cost) and about 20 dead routes were deleted, about 6,500 lines.
- Closing one agent of a pair unlinks the pair: the other is a normal agent again.
- The browser tab says "antiloki".


### The Google seat is Antigravity
- Gemini CLI was replaced by the Antigravity CLI (`agy`); the seat is named **Antigravity** in the app, the server's seat list and the provider names. The seat's id stays `gemini` (saved settings keep working) and the models keep their Gemini names.

### The machine guard can pause runs on Windows
- Pausing a background run used to be Unix only (SIGSTOP); on Windows it did nothing. It now freezes and thaws the whole process tree with `NtSuspendProcess` / `NtResumeProcess` (`mem.rs`). Compile-checked with `cargo xwin check`, not yet run on a Windows machine. On Windows the guard uses memory pressure only: there is no CPU-load reading there yet.

### Tests brought up to date with the free build
- The route tests no longer expect a trial or a 402 (the build is free); the journeys count only audit messages in the Leader's inbox (every finished run also sends a run report), wait for the sagas list (it is served stale-while-revalidate), and expect the Leader's skills to be approved on write (a draft is what an old or imported skill still is).

### Teams, simplified (the operator's seven points)
- The team pane has **no tabs and no floating menu**. Its top bar has **Chat, Graph, Editor, Sequence** (Chat is the Leader), a **Fork** button (the threads window), the sagas icon, **Evolve** and **Swarm**. Edit, Activity, Sagas, Result and X-ray are gone: editing is on the graph (a node opens its settings; the full editor opens from the graph's edit button).
- **Time and cost** moved to the bottom bar (left, after the icons): a small live reading of the team's run; a click opens the waterfall as one glass window in the middle of the screen.
- The graph numbers the nodes in run order and gives every branch its own line colour (`graph-order.ts`: topological order, loops left out, a fork starts a branch, a join takes the main line back).
- The evolution grid and table float over the graph's bottom right after a session ends (`EvolveBoxes`).
- The **menu** opens from the **a.** logo on the bottom bar; the corner logo and its column are gone, so the panes use the whole width.

### Evolve and Swarm
- **Swarm** (RFC 0019): several ideas tested at the same time under a cap (`team_race.rs`), the best admitted one applied, the others refused with the reason; MCP `swarm_config|start|status|stop`.
- New **panels** behind the DNA and the bee icons (Setup and Results pages: goal cards, gain slider with a today-versus-needed picture, idea squares, a budget that says whether it covers the ideas, sockets, machine bar; the live views: budget ring, stage track, a gain meter that draws its noise, version lineage, slots, bracket). Compact (264 px).
- Trials keep the noise-counted gain (`sure_pct`) and their reason (a comment had swallowed it).

### The machine comes first
- Background CLI runs register with a watcher (`mem.rs`): short RAM or an over-booked CPU **pauses one run at a time** (SIGSTOP) and room continues them; a swarm run waits for room before it starts. A local model server above 2.5 cores for 30 s warns its panes; a model a pane leaves is unloaded.

### Specialists and local models
- `antiloki_specialist_create` now carries the **whole rubric** (the rules, the allowed tools, and **Rhai**, the language of a specialist's own tools, with a worked example that a test keeps true); a refusal of a tool's code carries the language hint. An agent needed nine tries before.
- A **draft** appears under + › Waiting for your approval; `antiloki_pane_open {specialist}` is in the tool's schema and a draft opens its editor on the screen.
- Models a server is too old to load (lfm3) are greyed out with the reason. A restart into a new box resumes the pane's earlier session.

### Onboarding and docs
- The first-run cards, the four-step tour and the Cowork mini tour describe today's screens; the public docs have new sections (Evolve and Swarm, Specialists, Models on this machine, Keyboard, looks and windows) and the screens, panes, teams and usage sections are rewritten.

## v0.5.4.9 — 2026-11-22 — Tyr: specialists, the book, the Editor view, chat skin

- **Specialists**: named agents with a memory of their own (a window to edit everything they have: profile, persona, tools, packs, skills, MCP servers, assistants, context, limits, notes), forkable, with an avatar from the sheet and a pixelated top bar; their pane's micro MCP carries their brief so they know who they are across restarts. A team's Leader is a specialist that lives in the team (tag LEADER).
- **Agent panes have two looks**: the terminal, or a chat over the same terminal (default in Appearance, a switch on each bar): instant send, a reasoning box, glass like the terminal.
- **Microphone**: transcribes, never translates; the language is chosen between English and Portuguese by the model's own probabilities.
- Everything below is part of this release.

### 2026-11-21 (night) — Specialists, and SQLite as the source of truth

- **Specialists** (RFC 0017 §2.4): a named agent with a memory of its own. It is the normal terminal and CLI with different context and access behind it: `antiloki_pane_open {specialist}` opens it (its brief is the first message, its pane has a coloured top bar and an animated avatar), and its pane's micro MCP `antiloki-specialist` shows only its memory tool and the read-only tools its approved definition lists, with its id forced into every memory call.
- One `specialist.db` per specialist holds the definition (a draft until the person approves; hash-checked), append-only notes, and the RAW conversation in chunks (hash, seat, tokens, cost, time) that notes cite; `verify` checks a note against its chunks with no model. Consolidation is due at 200k raw tokens and a new seat (Claude Code, Codex, a local model) starts from it. An agent may suggest a specialist once.
- **One source of truth**: templates, branches and book packs are rows of the project's `.antiloki/antiloki.db`; YAML is `team_yaml::to_yaml` of a row (one pure function). Files from before are imported once; the row wins afterwards.

### 2026-11-21 (later) — Docs by the book

- **Docs rules**: a law with `purpose: doc` documents instead of judging; the Editor's docs icon builds a file's documentation from its own facts plus every doc law of the packs that apply (design patterns, routes, configuration, what it touches, claims, calls to action…). Shipped packs `code-docs` and `copy-docs`; the Leader writes more for any kind of project (skills/law-smith.md). Deterministic, no model.
- Editor: X-ray's file editor without its header of buttons and without doc cards; the path in the tab bar.

### 2026-11-21 — Deterministic lanes: the book for any kind of work (RFC 0018)

- **The book**: laws that judge what a team makes, whatever it is (code, copy, reports, plans, tables, web pages, images, SVG), as data. Eight detectors, readers for markdown, text, HTML, JSON, YAML, TOML, CSV, image headers and SVG, seven shipped packs (42 laws, each with its own passing and failing examples), one report shape (findings with file, line, evidence and fix; scores per file and axis; a gate where "no law applies" is a skip, never a pass; a digest that is the same for the same files and laws). 23 tests incl. determinism in any order, degenerate inputs and mutation of planted defects. `serve/src/lanes/`, `templates/book/`, `skills/law-smith.md`.
- **The lane in the graph**: a `law-gate` step judges what a node made and routes pass/fail (repair loop, human gate); no model.
- **The Leader writes, the engine tests, the person ratifies**: MCP `antiloki_book` (list, show, run, test, propose); agents can only propose candidates; a law cannot be ratified without a passing and a failing example. The **Book** view and a **Findings** tab in the Editor's panel; X-ray sources `book_findings`, `book_summary`, `book_packs`.
- **Editor view**: file tree with whole project, X-ray's real parts behind the activity bar, flip icons per file (file review, folder review, generated docs, git history replay), X-ray's own rebuild window, a fixed bottom panel; agents are their own git authors.

### 2026-11-21 — Editor view of a team

- **Editor view** (Graph · Editor · Sequence): the files the agents of a run write, as a code editor you watch. Serve records every write, edit, read and command as events (`<run>/edits.jsonl`, `run_edits.rs`, ops `edits`, `file_base`, `file_now`, `file_save`); the screen replays them deterministically (same events, same typing, live or afterwards): named coloured pointers, tabs opening and closing, two editors side by side when two agents write in different files, a lane timeline you can scrub. Read-only while agents run; when they stop it is the X-ray code editor and Ctrl/Cmd+S saves into the run's folder. Preview node (`run_preview.rs`, the `preview` skill, ops `previews`, `preview_stop`): starts the project the team builds and keeps it up, with its address, until the run ends; the Editor view's right side shows a live frame (frontend) or the address and log (backend). A counter on each agent's pointer shows tokens written (estimated, about 4 characters each) and the step's cost share.
- **Team Leader placement**: switches on the team pane's own top bar: beside the team (a framed card, a third of the width, resizable seam) or floating (drag, resize, see-through until the pointer is over it). The pointer over it puts the keyboard in it and gives it back on leaving; focus border like any pane.
- **Landing page and docs**: not live yet (no download links, no releases page), X-ray removed everywhere, the benchmark claims taken down (the cost ratios did not reproduce with current models), new rows for teams as graphs, the Editor view, templates, sub-teams, deterministic nodes and free panes; docs gain the Leader, Graph/Editor/Sequence and the Preview node.

### 2026-11-20 — Web apps drawn natively (Linear, Notion in progress), sign-in by the app's own OAuth, a browser teams can use, previews on the graph

- **New pane makes a team, always.** Each kind has a team template behind it: Web app → `pane-webapp` (reads the app's real tools with the new `antiloki_app_tools`, you approve the design, it writes the pane as a draft), Recreated → `recreate-webapp`, Deterministic → `pane-deterministic` (finds a zero-AI X-ray source, previews it, you approve, saves the widget). The dialog no longer opens a pane that already exists. **The bottom-bar money chip shows what was spent** (the saving, still $0.00 until something is routed, is in its hover) and opens the redrawn card: a Today / 7 days / 30 days switch, share and spent per seat, and buttons for the full windows and the fuel record. The left sidebar's scrollbar is hidden.

- **The usage window is a card in the middle of the screen**, redrawn after AffectUI's Model Router: a title and a "last 7 days" label, the spend as the big figure, a table of seats (5 h, week, spent, with each seat's share as a thin line) and a footer with its status and "All the numbers". **The spend was $0.00 while the sagas showed money**: team runs were not in the call record; they now count (a "Team runs" row). **Onboarding rewritten**: the Cowork tour (spaces and threads, panes of every kind, the bottom bar) and new tours for the New pane dialog, the New team dialog and the usage card; the old Portuguese welcome card and tour copy now say what the product is today. **Sagas in a pane are glass** like every other pane.

- **New shipped team template: `bug-hunter`** (draft: not yet proven by a run; a real run would cost about $1–4). It builds and tests your project (terminal steps: build, the existing tests, lint), maps the risky spots, runs a security hunter and a logic hunter in parallel (every finding needs a file and line), has a test writer put one failing test per finding under `tests/`, runs them, has a QA tester read the browser capture of the running app together with the test results (the browser step is a help: if it cannot look, QA goes on with the tests), and a triage step writes a ranked bug report that stops for your approval. Your code is not changed.

- **Free panes M2–M6.** A pane can now **write**: the page asks, the host draws its own confirmation card (outside the page, with every argument in words), nothing goes before your click, a destructive tool needs a second tick, declining tells the page `declined`, and each confirmed write is a line of `.antiloki/panes/trail.jsonl` and of the call trail. A pane has its own **memory** (`state`, 256 KB, outside its fingerprint), can show a **picture** of an app, open a **link** (asked once per domain) and put a **notice** in its header ("N calls · last write …"). An agent can **write a draft** (`antiloki_free_pane_write`, `_open`, `_list`; the old version is kept in `.antiloki/panes/.archive/`, never deleted; only you approve) and, by **meaning**, read a free pane (`antiloki_pane_view`) and act in it (`antiloki_pane_act`): the page says what it shows and what an agent may do, and the act runs the same code as a click, so a write still asks you. **Export / Import** a pane as one file (Export in its header; Import in the menu Your panes): it lands as a draft, a changed file is refused, nothing is overwritten, approvals are never imported. A team's bundle installs the same way (`install`). A browse target may say `"output": "free-pane"`. Example panes: Linear issues (read) and Quick capture (a form that writes). Skill for agents: `skills/free-panes.md`.

- **Free panes, M1 (reads)**: a pane page can call `antiloki.read(app, tool, args)`; the serve re-reads the manifest and the approved hash from disk, allows only a tool the manifest lists to read, refuses any tool that is not read-risk at the door, and caps the answer at 2 MB. A draft pane reaches nothing. Example pane `templates/panes/linear-issues`. In a real browser, a page's attempts to fetch, read its parent, use storage or call the serve all fail.

- **More shipped team templates**, each proven by one real run (about 1–3 cents each): `seo-audit`, `marketing-campaign` (checks every claim against the page, then stops for your approval), `lead-capture`, `sales-crm`. **New pane** dialog with four kinds (web app, web app recreated, deterministic over the zero-AI X-ray sources with preview and save, specialist: written down, not built). Fixes found by running them: a page's text no longer keeps half its stylesheet when an inline svg sits in a style; an agent after a join is told what the join waited for.

- **Team templates** (RFC 0017, T1): whole teams as YAML in the branch format; a library that ships with antiloki (`recreate-webapp`, verified by a real run) and one of your own; **New team** lists them with status, needs and cost; **Share as template** on a team checks it first (validates; refuses a secret or a private path; resets triggers, limits and setpoints and says so; `verified` only by a successful run). RFC 0017 also writes down the four kinds of pane (web app, web app REA, deterministic, specialist).

- **GitHub pane** (reads and confirmed writes through GitHub's own MCP: repositories, code with line numbers and colour, issues, pull requests with checks and diff, commits; merge asks twice), drawn from the browse team's frames. **A refusal now says what it is and where to fix it** (expired token, missing permission, SSO, rate limit) in every web app pane. The MCP client tells a 401 from a 403 and carries what the app said. **App tokens** are changed under one lock and written atomically (a sign-in refresh could drop a pasted token). Linear, Notion and GitHub start where they stopped when expanded.

- **Linear pane** (Web apps → Linear opens it in antiloki; the site cannot be framed): issues by status, an issue with its comments, projects, cycles, **Inbox**, **My issues**, **Documents**, **Views**; **write through Linear's own tools** (new issue; status, priority, assignee and labels; edit title and description; comment), each a confirmed write by the person; pictures and videos in descriptions, comments and documents are drawn (a new upload route in the serve holds your connection); the connected workspace is shown with a **Switch account** button. Every control has a test; the real calls were run against a real workspace (one cancelled test issue, DAN-6, is left in the "dani" workspace).
- **Notion pane** (made by the recreate-webapp team from a real capture, checked against Notion's real answers): the sidebar of pages, a page's content, search, comments, and **writes** (new page, rename, add text at the end, comment, move to Trash), each a confirmed call; Notion's own sign-in and Switch account. A whole-page replace is not offered (it can delete child pages).
- **Sign in to an app by its own OAuth** (RFC 0009 M2): discovery, dynamic registration, PKCE, loopback callback and refresh in the serve (`serve/src/mcp_oauth.rs`); one button, no key to paste (a key still works). The Web apps catalog now points at the makers' current MCP addresses (Linear's old `/sse` answered 404).
- **A browser teams can use** (`browse` tool): `login` opens a plain window for the person to sign in (marked as antiloki's, with the turning-ring border, badge and pointer of the MCP "a." chip) and `capture` attaches to that same window, takes screenshots and a reading of fonts, colours and layout at several widths, read-only and only the listed domains; a login wall or a missing expected word is "signed out". A node can name its **target** (`.antiloki/browse-targets/<app>.json`) so one team serves every app.
- **One team makes any app's pane** (`recreate-webapp`): sign in, capture, a designer draws the screens, **you approve the design in a frame on the approval card** (full screen in a glass window), then a coder writes the code as a document; the team edits no project file. New `approval.show` field on a human node.
- **Teams**: a **play button** (▶ runs a team that repeats a job; a team that takes your direction opens the Leader); a **run picker** over the graph (follows the live run; any past run can be shown); **mini previews** beside nodes that made something (pages, documents, pictures, videos; Deliver shows the Result page; a click opens a glass window; previews drag like nodes); a **failed node keeps its words** and shows why on the graph and inside the node; floating notices no longer cut their content and have a ✕; new agent / new pane are off on a team's thread.
- **Housekeeping**: `update-serve.sh` keeps only the last three saved serve versions (53 of them filled the disk). Markdown links written with angle brackets are links. RFC 0016 (free panes) is proposed.

## v0.5.4.8 — 2026-11-17 — Tyr, teams that improve themselves, fork, and code with small local models

This build also carries everything listed under the two Unreleased entries below (the licence hardening, webhook triggers, the away digest, the operator's first real team, the team graph).

- **Teams keep their own git** (RFC 0013): every team is rebuilt from one recipe file (`team.recipe.toml`) and a generated `TEAM.md` in a private repository per team. Improvements are **requests**: a branch with ONE small change and a page that says what changes and what it was measured on. You approve (the team is rebuilt), refuse (with a reason, which the improver reads next time) or ask for another try. A replay bank of saved tasks with deterministic checks (every third one held out) judges a change on a staged experiment with a lower-bound rule and a regression allowance; spend limits, gates, checks and triggers can never be changed by a request. Export and import a team as a file.
- **Threads of a team**: fork a team onto another engine (the same steps on Cursor instead of Claude) as its own branch and its own runs; compare two threads on their recorded runs.
- **Inbox and gate stack**: everything that waits for you, in the sidebar and on each team, answered in place. **Checkpoints** of a run with "rewind here" (a new run from that point; the old one stays). **Root cause** of a failed run: causes confirmed or ruled out with the evidence. **Conflict radar**: files changed in more than one worktree, same lines flagged. **Run reasoning card**: how a run spent its time, by step, kind and seat.
- **Small local models can write code** (RFC 0014): new nodes in the "Local-model support" group, each doing with plain code everything code can do and asking a model only for judgment and writing: `decompose`, `recurse`, `evolve`, `test`, `contract`, `stubs`, `skeleton`, `review`, `enforce` (a JSON enforcer), an X-ray node, an apply step, and a family of tiny-model nodes with guards (summary, translate, intent, redact, regex). The leader writes the tests and the contract once; a local coder (Qwen2.5-coder 3B) fills the stubs. Measured, local coder only: a one-function task in 34 s, a four-function module in 71 s, a three-file todo manager in 17 min, all passing the leader's tests. Start from the template `local_coder`; the MCP tool `antiloki_team_nodes` lists the nodes.
- **Desktop (new, NOT yet tried on a real desktop)**: a tray icon with a live tooltip (agents awake and asleep, team runs going); closing the window while agents or teams run asks once whether to keep them running in the background, and then shows ONE avatar at the screen's edge; the avatar dock is no longer drawn over the open app.
- **Fixed by the first real experiment**: the improvement experiment read each run's status one level too high (every run counted as failed) and judged the agent's own narration as part of its work.
- **Housekeeping**: the repository no longer tracks `target-check/`.

## Unreleased — 2026-11-15 — The queue finished (Steps A, B, D) and the operator's first real team

- **The operator's first real team (a video editor with HeyGen hyperframes) taught six things, all fixed** — (1) the Leader made a second team beside the one the person opened: an agent's design under another name is now refused ("you are working inside the team `x`"), `new_team: true` says it really wants a second; (2) it designed a coding crew for a video job: a team an agent makes from a name alone is BLANK (not the `feature` template), the validator warns `all_agents` for three or more agents with nothing deterministic, and `antiloki_team_design` opens with design rules (work in the open team, design for the job, prefer code/transform/check/tool steps, do not build what was not asked); (3) a team role node that hits a seat out of quota is moved to another seat before the call (and once after it), and the failure now says whose quota ran out, when it comes back, and that the team budget is fine ("Individual quota reached... Resets in 167h" was Cursor's own account quota, on a node that happened to be called `composer`); (4) the Run tab showed the old graph after the Leader remade the team: it now shows the team as it is now with a banner and a button to see the run's own version. Still open: a docs node that fetches the real docs into the team's files instead of returning links. `team_graph_run.rs`, `team_graph.rs`, `agent/src/mcp.rs`, journey j57.
- **Licence hardening L01-L09, L11-L13** (`PLAN-LICENSE-HARDENING.md`): per-environment licence folders and a high-water clock guard (L01-L03), symbols stripped (L04), four independent checks where real work passes with a 10-minute grace (L05), the licensee shown with the middle of the address hidden and printed under exports (L07), a per-install signing secret in two user-only files with one-time migration of old records (L08; the OS credential store waits for a hand test, O08), a signed revocation list (`antiloki-keygen revoke`, L09), a quiet 14-day integrity self-check with `antiloki-keygen seal` (L11), `LEAK-PLAYBOOK.md` (L12) and `scripts/licence-attack-checks.sh` (L13). L06 (hidden strings) and L10 (bundle bound to the licence, needs decision D1) are not done.
- **BL-7 webhook triggers**: `POST /api/v1/hooks/team/<team>/<trigger>` with the trigger's token; the door only checks the token and queues the call, the scheduler's tick runs it under the person's consent and the daily caps, the body reaches the run labelled as data. The schedule panel can add one and shows the token once; it also no longer turns every trigger into a schedule on save. j58.
- **BL-8 while you were away**: `away_digest` and a banner on the team pane (runs started, finished, failed, waiting for you, skipped by a limit). Pushing it to a phone or Discord is not done (needs the operator's account). j59.
- **BL-10 check rules** `links_ok` (structure only, no network) and `diff_size`; **BL-12 a web app through its MCP tools** (list with risk, sign-in token, call, destructive confirm); **B2 the proof card** (RFC 0010 Appendix D verdict rules over recorded pairs, bootstrap, judge gate, stale; `team_proof.rs`, op `proof`, j60); **BL-6** `docs/heimdall-label.md`; **BL-5/BL-13** decided in `docs/decisions-bl5-bl13.md`.
- **New tool nodes (the operator's ideas of 2026-11-14)**: `mcp_url` (any MCP server by address) and `api` (any http API; the method sets the risk; DELETE and destructive MCP tools are never a team's; secrets by NAME in the token store, sent as a bearer header, never written to a run) (BL-18, j61, j62); `git` (status, diff, log, branch, worktree_add/remove, commit, merge inside the team's own branch and worktrees, never the workspace; BL-20, j63); `telegram` (send; outward, behind a human gate; bot token by name; BL-19, j64, tested against a fake Bot API only); the `docs` web tool (reads a documentation site into files the team keeps); the template `pair` (driver, navigator, free tests, an independent QA; BL-21). The Leader's brief teaches all of them. Also: a seat's quota reported inside a normal result moves the call to another seat; the Leader's status line read the run budget as $0.00; a search node skips a seat that is out of quota. `docs/affectui-ideas.md` lists the UI ideas taken from AffectUI's component list.
- **Verified in a real browser** (private stack, latest binaries): sagas board/pane/run, threads, Diff, Frames, machines and the agent MCP: 17/17.

## Unreleased — 2026-11-12 — Team graph (RFC 0008, M0 to M7)

- **D2 — Cursor and Antigravity panes pre-trust their folder** (`serve/src/pty.rs`): before the CLI starts, Cursor gets its `.workspace-trusted` marker under `~/.cursor/projects/<path as dashes>/` and Antigravity gets the folder added to `trustedWorkspaces` in `~/.gemini/antigravity-cli/settings.json` (the box and, for a linked worktree, the repository root), once, touching nothing else; a settings file that is not a JSON object is never overwritten; a marker the user made is left alone. Tests: `cursor_names_a_folder_by_its_path_in_dashes_and_the_timestamp_is_utc_iso`, `cursor_is_told_to_trust_a_box_once_and_a_marker_that_is_there_is_left_alone`, `antigravity_trust_adds_the_folder_and_keeps_every_other_setting`.
- **D1 — the `antiloki_machines` MCP tool** (agent/src/mcp.rs): `list` shows the machines the person connected over SSH with their state, `open {id}` gives the address of a CONNECTED machine's antiloki on this computer to read from; adding, removing, connecting and disconnecting a machine stay the person's (the tool refuses them before any call, because a machine is somewhere antiloki logs in with the person's keys). Test: `the_machines_tool_lists_and_opens_a_connected_one_and_never_adds_for_an_agent`.
- **B3 — the serve as an MCP client** (RFC 0009 M0 and the first slice of M1; `mcp_client.rs`, `mcp_fake.rs`, `app_tokens.rs`; ops `tools`, `call`, `token_set`, `token_forget` on `/api/v1/webapps`; the `webapp` tool type of a team node): the streamable-HTTP transport with a JSON or an SSE-framed reply, the session id, `tools/list` with its pages and schemas, `tools/call` with a timeout, a rate limit (30 a minute) and a cost note, errors told apart (needs sign-in, timeout, rate limit, the app's own JSON-RPC error, an error result); a tool's risk class from its annotations (a tool that does not say it only reads is a write); an agent may read, never write unless the person allowed it, never destroy, and the person confirms a destroy; an agent names an installed app, never an address; the token is the person's, kept in the machine's antiloki home with owner-only permission (not the repo, a layout, a saga, a reply or the trail; the OS keyring is the planned home behind the same four functions) and scrubbed from any error; every call is a row of the trail (who, app, tool, risk, ok, ms and the names of the arguments, never their values). A fake MCP server (a tool of every shape and risk, two pages, JSON or SSE, optional bearer token) backs the conformance tests. Not built: OAuth 2.1 (M2), the tool console pane and views (the frontend, M1/M3), the legacy two-endpoint SSE transport, a pooled client. Tests: `the_client_lists_tools_with_their_schemas_over_json_and_over_sse_and_follows_the_pages`, `a_call_returns_typed_results_errors_and_json_rpc_failures_are_told_apart`, `a_slow_tool_times_out_and_the_rate_limit_stops_a_flood`, `an_app_that_wants_a_token_says_needs_signin_and_the_token_never_appears_in_an_error`, `the_risk_of_a_tool_comes_from_its_annotations_and_the_caller_decides_what_it_may_do`; journey `j56`.
- **S8b — sub-team, file-watch and on-error triggers** (RFC 0011 §14, §16; `team_subteam.rs`, `team_triggers.rs`): the `sub_team` node runs another team as a step (an ask template over this run's values, `outputs` mapped out of the child run, a `budget_share`; the inner team keeps its runs, ledger rows and audits; the outer node records the child run id and takes its cost); a team cannot include itself directly or through others (refused at save and at run) and nesting has a depth limit (default 3, a node may ask for less, never more than 5). Triggers `file_watch` (a glob inside the workspace, waits for the files to be quiet for `debounce_s`, the first look only records) and `on_error` (another named team, or any other team, failed or was not good, once per failed run, never its own) use the same consent hash and the same daily caps and overlap rule as schedules, and an agent cannot set or consent to any trigger. Tests: `a_team_cannot_include_itself_directly_or_through_others`, `the_depth_limit_is_enforced`, `the_child_run_id_is_recorded_on_the_outer_node`, `file_watch_fires_only_with_consent_and_within_limits`, `an_on_error_trigger_fires_when_a_named_team_fails`, `an_agent_cannot_set_or_consent_to_a_trigger`; journey `j55`.
- **S8 — the Data types: db, file, saga** (RFC 0011 §13, §9; `team_db.rs`, `team_file.rs`, `team_saga_node.rs`; `use_tool` types `db`, `file`, `saga`; ops `db_schema`, `db_query`, `db_write`, `db_consent`, `saga_read|write|run|sync`): a db is `sqlite`, `csv` (loaded into an in-memory SQLite and queried with the same SQL; writing back is an explicit `export`) or the team's own `bench.sqlite` (read-only, never writable); read-only by default, a write needs the person's consent bound to the team and the db (a hash on file), a schema change always the person's yes on that call, parameterized statements only (no quoted text and no number in the SQL), one statement, a row limit and a timeout (the connection is interrupted), a dry run that rolls back; the file type reads and writes only inside the run's own folder (no `..`, no absolute path, no link out, text/markdown/json/csv with the shape checked); the saga node is a typed door over `work.rs`/`saga_run.rs` with no new storage (read, write with an optional Check on its words, run = the goal its open episodes make, sync = an episode's state from a team run). Not built: Postgres (a later cargo feature), dataset download. Tests: `schema_lists_tables_and_columns`, `query_is_read_only_by_default`, `a_write_needs_the_persons_consent_bound_to_the_team`, `ddl_always_needs_the_persons_yes`, `statements_are_parameterized_only`, `the_row_limit_and_the_timeout_apply`, `csv_is_loaded_into_memory_and_queried_with_sql`, `bench_sqlite_is_readable_and_never_writable`, `file_read_and_write_are_confined_to_the_run_tree`, `the_saga_node_reads_writes_runs_and_syncs`; journey `j54`.
- **S7 — the Improver, backend** (RFC 0011 §18; `team_improve.rs`, MCP `antiloki_team_improve`): a read-only proposer that wakes only on a trigger (a gate finding `node_unreliable`/`node_regressed`/`node_dominates_cost`/`node_climbing`, N runs but never more than weekly, the person marks a result bad or asks), under a cooldown, off until the person turns it on; it proposes (hypothesis, metric, expected, the rows cited with n) and applies NOTHING: a decision goes through the checkpointed patch path, one accepted change per node per cycle, a changed node is left alone until its new version has enough calls, a patch that touches a gate, a budget, a human gate, triggers, limits or setpoints is the person's, `auto_low_risk` mode lets the engine take only wording and cheaper-seat changes that are not the person's, a freeze flag per team or node, a hit rate (helped over proposed), and ablation helpers (the graph without a node, the held-out set, a verdict that never removes a node if quality drops or a trap is lost). Tests: `a_review_is_triggered_by_a_gate_finding_or_n_runs_or_a_request_and_not_by_every_run`, `the_cooldown_blocks_a_second_review`, `one_accepted_change_per_node_per_cycle`, `the_improver_is_read_only_and_cannot_apply`, `a_proposal_that_weakens_a_gate_a_budget_or_a_human_gate_requires_the_person`, `auto_apply_never_touches_gates_budgets_or_triggers`, `the_freeze_flag_blocks_proposals`, `the_hit_rate_is_recorded`, `the_ablation_runner_compares_without_a_node_on_the_held_out_set`; journey `j53`. Not built here: the Improve tab (S7f) and variant generation.
- **S6b — benchmark, regulator, circuit breaker** (RFC 0011 §7): `team_bench` (Wilson intervals, `too few runs` under five, per-seat statistics from the ledger, `trial` with required traps and `compare` with a pre-registered margin rule, every call a `bench = 1` row; ops `bench_stats`, `bench_trial`, `bench_compare`), `team_regulate` (graph `setpoints`; after each run compares windowed statistics with them and acts inside the node's own envelope: a finding, a time-boxed block of a failing seat that the router skips, a narrower swarm; it takes the graph by shared reference and writes only `regulator.json`; setpoints are the person's to change), and `Retry` `open_after`/`cooldown_s` (the breaker stops calling a failing seat, then lets one try through). Tests: `trial_runs_a_labelled_set_with_traps_and_records_bench_rows`, `compare_records_every_arm`, `stats_use_wilson_intervals_and_say_too_few_runs_under_five`, `a_node_over_its_setpoints_raises_a_finding_and_reroutes_inside_the_envelope`, `the_regulator_never_edits_the_graph`, `retry_open_after_stops_calling_a_failing_seat_then_lets_it_recover`; journey `j52`.
- **S6 — cells: presets, router, swarm; for-each `batch`/`on_error`, Join `consensus`** (RFC 0011 §10): six presets (`classify`, `extract`, `judge`, `summarize`, `rewrite`, `sentiment`) each shipping a schema and a default guard (a strict enum, a lookup in the source, or a named ladder for prose: never silently unguarded); `seat: router:<name>` with a rules table, a Thompson-sampling bandit per task class and seat (seeded, reproducible, cost-aware) with a minimum exploration share and a safety floor, never moving the Leader, its choice and evidence on the ledger row (`seat`, `route`); the swarm template (for-each → llm → check → join, `swarm_unbounded` validator, `graph_swarm` op); for-each `batch` and `on_error: collect`; Join `consensus` that leaves by `climb` when the answers differ. Tests: `router_picks_the_cheapest_seat_that_meets_the_rule`, `router_respects_availability_and_quota`, `bandit_keeps_a_minimum_exploration_share_and_a_seeded_rng_is_reproducible`, `router_never_moves_the_leader`, `a_router_with_no_stronger_fallback_is_a_validator_error`, `each_preset_ships_a_schema_and_a_guard`, `a_small_seat_with_a_preset_is_never_unguarded`, `a_swarm_expands_to_foreach_llm_check_join`, `a_swarm_without_concurrency_or_a_budget_is_a_validator_error`, `join_consensus_climbs_on_disagreement`, `foreach_batch_and_on_error_collect`; journey `j51`.
- **S5 — team memory (`team_memory.rs`)** (RFC 0011 §8): a guard's verdict is marked on the ledger row (`verified`, `guard_failed`, `parse_fail`); a deterministic consolidation after each run promotes a verified success to the golden set and a failure a guard caught to the regression set, dedupes by hash and assigns a split once (never moved); FTS5 recall; the memory ops `recall`, `write` (sanitized), `forget` (the person's); `examples_from {task_class, k}` fills a cell's prompt with its best verified golden examples (never regression, trap, validation or held-out) and the call row records them in `examples_used`. Tests: `fts5_recall_finds_golden_examples_by_task_class_and_text`, `consolidation_dedupes_by_hash`, `a_failure_caught_by_a_guard_is_promoted_to_the_regression_set`, `a_verified_success_is_promoted_to_the_golden_set`, `a_split_is_assigned_once_and_never_moved`, `examples_from_fills_the_prompt_with_k_examples_and_the_call_row_records_them`, `held_out_and_regression_items_are_never_used_as_examples`, `memory_node_ops_recall_write_forget`; journey `j50`.
- **S4 — Code (Rhai sandbox), Transform v1, Check rule kinds, Stop, family view** (RFC 0011 §5, §11–§12, §15): `team_code` (JSON in/out, examples at save, time and operation limits), `team_transform` (modes edit_fields, filter, sort, aggregate, split, limit, dedupe, template, format_json, sanitize), Check kinds `pii`, `secrets`, `urls`, `keywords`, graph kinds `code`, `transform`, `stop`, and `family()`. Tests: `code_runs_json_in_and_json_out`, `code_without_a_passing_example_is_refused_at_save`, `code_cannot_read_the_clock_use_randomness_touch_files_or_the_network`, `code_is_stopped_by_the_time_and_operation_limits`, `code_is_deterministic_run_twice_same_answer`, `mode_edit_fields`, `mode_filter`, `mode_sort`, `mode_aggregate`, `mode_split`, `mode_limit`, `mode_dedupe`, `mode_template`, `mode_format_json`, `mode_sanitize`, `rule_pii_flags_emails_and_card_numbers`, `rule_secrets_flags_keys_and_tokens`, `rule_urls_enforces_an_allow_list`, `rule_keywords_flags_listed_words`, `family_covers_every_kind`, `saved_graphs_still_round_trip_unchanged`, `a_stop_node_fails_the_run_with_its_message`, `a_code_node_without_an_example_is_a_validator_error`; journey `j49_code_transform_check_kinds_stop_and_family_survive_save_validate_and_run_with_scripted_seats`.
- **S3 — graph versioning, patch, checkpoints, rollback** (RFC 0011 §17): JSON patch ops (`add_node`, `remove_node`, `set_field`, `add_edge`, `remove_edge`, `set_graph_field`, `rename`) with `requires_person` on checks, budgets, human gates, triggers and limits; checkpointed `graph_apply`, versions in `bench.sqlite`, rollback, and `antiloki_team_graph` (`versions`, `inspect`, `diff`, `propose`, `apply`, `rollback`). Tests: `a_patch_applies_every_op_add_node_remove_node_set_field_add_edge_remove_edge_set_graph_field_rename`, `an_applied_patch_writes_a_version_with_parent_hash_reason_and_proposal`, `a_failed_validator_stops_the_apply_and_names_the_checkpoint`, `rollback_restores_the_parent_version_exactly`, `a_patch_touching_a_check_a_budget_a_human_gate_triggers_or_limits_requires_the_person`, `a_requires_person_patch_is_refused_without_the_persons_yes`, `an_agent_pane_cannot_apply_a_requires_person_patch`, `only_the_last_k_versions_are_kept`, `diff_lists_added_removed_and_changed_nodes`; journey `j48_graph_versions_survive_propose_apply_checkpoint_rollback_and_refuse_gate_weakening_without_the_person`.
- **S2 — per-team call ledger (`bench.sqlite`)** (RFC 0011 §6): one WAL SQLite file per team under `.antiloki/teams/<slug>/bench.sqlite`; rows at `cli::run_cli` and `team_llm::call`, with `sanitize`, a per-team off switch, retention, and one-time import of `node-records.jsonl` into `node_runs`. Tests: `a_row_is_appended_per_cli_call_with_tokens_cost_and_outcome`, `a_row_is_appended_per_endpoint_and_local_call`, `sanitize_replaces_secrets_and_personal_data_before_a_row_is_stored`, `with_the_off_switch_only_hashes_are_stored`, `retention_deletes_old_calls_but_keeps_golden_regression_and_trap_examples`, `the_dataset_survives_reopening_the_file`, `a_failed_write_never_fails_the_call`, `node_records_jsonl_is_imported_once_into_node_runs`; journey `j47_team_bench_sqlite_keeps_llm_endpoint_rows_after_serve_restart`.
- **A team is a graph, run by the serve.** Nodes: the Leader (your terminal), roles (Planner, Worker, QA, Debugger, Audit and your own custom agents), control flow (if/else, switch, loop with a limit and a cost cap, for each, fork/join, retry, timer, budget gate, a human gate) and `useTool` nodes (skill, pane, MCP tool, source) that call a named tool with an allow-list and no model. The validator refuses what could run away: an unbounded cycle, a writer with no files, parallel writers on the same files, a tool with an effect that is not listed, an unwritten condition operand. Files: `.antiloki/teams/<team>.json`; runs: `.antiloki/team/<run>/`. Door: `POST /api/v1/cockpit/team-graph`; MCP: `antiloki_team_design / run / status / node / budget / pause / resume / stop / request_approval / simulate / skill / artifacts` (the Leader's; sub-agents may only read; **no tool approves**).
- **Every team has a thread of its own, a branch and a saga.** The team saga has one episode per pass of the main loop (a retry is a new episode that says what it retries); a pack of sagas is worked each in a tree of its own, parallel fork branches in sub-trees joined at the join; **nothing reaches your workspace until you approve the merge node**.
- **The team pane** (Run / Edit / Saga): the circuit on a calm canvas (microchip chips, wires that pulse where work flows, loop frames), a Linear-style pack list, a quiet peek panel per node with its read-only transcript, an editor with a palette, wires, an inspector and a simulation, and "Needs you" at a human gate.
- **The Leader builds any team from your own words** (RFC 0008 §4.16). "New team" asks what the team is for and which Leader to use; the Leader (a terminal) screens you with questions first, then designs the whole team through the MCP: custom agents with **one responsibility** each (brief, context files, skills, MCP allow-list, effects), control flow, tool nodes and the places you approve. Skills that do not exist are **written by the Leader as drafts** you read and approve in the team pane; a run does not start while a skill is a draft, and a skill never gives an agent more than its own effects. Anything that posts, sends, publishes or spends (`outward`, `money`) needs a human node on every way to it.
- **The artifact node** (§4.17): reports, dashboards and drafts that are not code are kept with the run, never in your branch; Markdown reads as text, HTML in a sandboxed frame.
- **Agent look themes:** pick how every agent looks (Microchip, Dot matrix, Bot, Pixel fantasy, Glyph, Prompt, Blueprint) from the pane's header; the app's own theme is untouched.
- **The web node** (§4.4): `use_tool` of type `web`, `search` or `fetch`, effect `network`. A page is read with no model (http/https only, text pages, a size cap, never an address inside this machine or its network, redirects re-checked). A search uses a Brave key if `ANTILOKI_BRAVE_KEY` is set, else **your seat's own web search** (a short headless call, a few cents, priced and capped like any node), else DuckDuckGo / Bing pages as a last resort (Bing serves decoys to scripts, so it is the last). What a web node finds is handed to the custom agents after it ("what the nodes before you found"), and an agent with the effect `web` may search and read the web itself. Agents had no web or MCP tools before: headless runs were launched with Read, Grep, Glob (and Edit, Write, Bash for writers) only.
- **V1-V3, the verification debt (handoff 2026-11-14).** V3: every serve spawn goes through `crate::proc` (16 sites in 12 files; portability lint 2/2) and `cargo xwin check` found two Windows-only compile bugs that had sat there (`webapps` `process_group`, `skillbank` symlink-copy type), both fixed. V2: `qa_evidence::a_dev_server_is_booted...` skips when `npm --version` cannot run (a mise shim with no version set); the cowork e2e fake microphone plays a real tone file (`e2e/fixtures/mic-tone.wav`, frontend). V1: full cowork Playwright run 61 pass, 1 skip, 1 fail (machines); fixed: the e2e stack sets `ANTILOKI_FE_DIR` so a connected machine gets the app files, and the machines spec accepts the workspace chooser a fresh machine opens on (it is now 1 pass); the drag-to-team fixme is rewritten as a team made through the teams door (cowork-teams 6 pass). Final full cowork run: 62 pass, 1 fail. The failing spec is `cowork-xray:140` (NOT flaky: 3 of 3 alone also fail): its trace showed `ConstitutionPanel` throwing "useShell must be used inside SlugProvider" in the shell-less X-ray embed (fixed: `useShellMaybe`, frontend), and a remaining `net::ERR_INSUFFICIENT_RESOURCES` that blanks the iframe on the same view; cause of that one NOT found, still open. Lib: 1309 pass, 0 failures after the qa_evidence fix (the run itself was before it; that test alone passes).
- **Never Opus** (operator 2026-11-13, said four times). A Claude Code call is made on Sonnet whatever asked for Opus (`cli_command` and the `llm` node's `claude-code:` seat): a route, a tier, a role table or a hand-typed id. The High tier now means Sonnet, Opus is off the QA gate roster, the model ladder is Haiku then Sonnet, and the node drawer no longer offers it. **Not changed, decisions for the operator:** the Cowork model button in an agent's header (an interactive terminal: it lists Opus because the engine offers it) and the savings figures, which still compare against a frontier price reference (`claude · opus`, priced only, never called).
- **The planner starts on the mid tier** (Sonnet 5.5), not the high one: it is capable enough (operator 2026-11-13). Backend `default_tier`, the editor's label and the template test follow; a node can still be set to any tier. The built-in High tier for Claude Code still maps to Opus (not changed; nothing in a default team uses it now).
- **A benchmark per node, and a gate that reads it** (RFC 0010 §4.20 A). When a run ends, every node that took part gets a row in the team's `node-records.jsonl`: which version of the node it was (a hash of its spec, so a change of seat, brief or cap is a new version), the seat, whether it succeeded by the rule of its kind, tries, whether a small seat climbed, cost and time; nothing new is measured during the run. From the rows: success, retry and climb rates, median cost and time, each node's share and rank of the team's cost, what one good output costs, and **whether the Leader's last change to a node helped** (with the number of runs on each side; under five runs it says "too few runs"). **The self-improving gate now also looks at nodes:** a node under 70% success over five runs, one that fell 20 points with nothing changed in it, a small seat that climbs in more than 40% of its runs, one node taking over half the cost five runs in a row, or a change that made things worse tells the Leader even when the run itself was good. `antiloki_team_audit nodes`, and a Benchmark block in the node drawer. Also: the drop-zone label said "link as a team" when dragging an agent onto another; it now says "start a pair session". Journey j46.
- **Campaigns: parallel runs and A/B tests inside the machine and the money** (RFC 0010 §4.19). `antiloki_team_campaign` runs a saved team once per arm (the same graph with a patch on some nodes: a seat, a brief, a cap, a tier), several at a time up to what the machine carries (half its threads and free RAM at about 600 MB a run), compares the arms by the audit and the cost and names a winner; the saved team is never edited. Three budgets that are never exceeded: an agent's `cap_usd`, a run's `budget_usd`, and now the **result's** `budget_usd` over all arms: an arm whose worst case could take the sum over it is not started (a campaign that cannot start any arm says why), and at the ceiling everything stops. Run ids are milliseconds, so runs started together no longer share one. Journey j45.
- **The Leader manages everything: seats, the self-improving engine, apps** (RFC 0010 §4.18). `antiloki_team_seats` (what this machine has, the measured evidence per kind of step, `fit`, `register` a local server, `pull` a download only with the person's yes, `trial` a seat on cases the Leader writes, traps included, counting the mistakes nothing would stop); a small seat with no guard is refused when saved (`llm_unguarded`). **Every run is audited when it ends** (free, from the run's own record: how it ended, checks, loops that ran out, failed nodes, declined gates, cost, small-model climbs, whether there is any measurement at all, three bad runs in a row, and what the last change did to the score); when the result is not good the Leader gets a `[team audit]` message in its terminal and in a durable inbox (`antiloki_team_audit`: inbox, get, history, ack), and decides whether to change the team. `antiloki_team_apps`: the installed web apps and whether they answer or need a sign-in, a preflight of what only the person can do before a team's first run (install, sign in, what posts or spends and where the gate is), and opening an app's page for the login; an agent that lists `app-<id>` in its `mcp` is now allowed that app's tools and handed its address. The Leader's brief covers all of it. Journey j44.\n- **The model node, `llm`** (RFC 0010 §4.4, §4.5, M2 slice 1; built from what the stage benchmark found). One call to a seat, no tools, no files, a **typed answer** (string, number, bool, enum, lists; read leniently, validated, one re-ask with the error). Seats: `endpoint:<alias>` (an OpenAI-compatible server named in `~/.antiloki/seats.json`, e.g. LFM3 1.7B on llama.cpp), `local:<ollama tag>`, `claude-code:<model>`. What makes a small seat safe is built around it: `verify.in_source` (an extracted field must literally appear in the source text; a price made from a weight is caught), `agree_with` (another seat must give the same answer), and a `fallback` ladder that only goes up and says why it climbed (`escalated_from`, `why_escalated`). Real test: 60 extraction items through real runs on LFM3 1.7B with a Haiku fallback: **58/60 correct, 3 climbed to Haiku, $0.008 in all** (Haiku alone: about $0.12). The check cannot catch an answer that is merely too long ("Kona, United States"). Journey j43; the editor has the fields; the Leader's brief says when to use it.
- **The Leader stays a frontier model.** The New team dialog offers only frontier seats for the Leader (Claude Code, Codex, Gemini, Grok, Cursor; not OpenCode, which can run local models), its brief says it is the frontier model by design, and RFC 0010 states that the router and the bench never move the Leader down: small and local models are for steps only.
- **The check node, and every node says why** (RFC 0010 §4.1, §4.6, M1). A `check` node judges a text the run made against **rules written as data**, with no model and no cost: `forbid`/`require` a pattern (with the line and the quote it found), `words`/`chars`/`lines` (whole or inside a Markdown section), `sections` (required, forbidden, in order), `json` (valid, with keys), `html_clean` (no script, only listed hosts, `lang`, `title`, viewport), `readability` (Flesch), `file_exists`/`file_size`. It writes `pass`, `failed`, `passed`, `score` and always leaves by `out`, so a loop or an `if` reads `check.pass`; what failed is told to the next agent ("fix exactly these"). A rule that is not a regex, a kind not built (links, diff size, the page ones), a source nobody makes, or a source that does not come before it is refused when the team is saved. Every node may carry a `why` (parallel, independent check, tool, cost, gate, schedule, memory, sense, and a sentence), shown in the drawer, and a team that is only a chain of agents with no reason, no check and no tool gets the warning `one_prompt_with_extra_steps`. The editor has the fields; the Leader's brief says: if a rule can be written, write a check. Journey j42.
- **A Pair, and a Leader that asks little and says when a team is not worth it.** Drag an agent onto the middle of another (both free) and a **Start a pair session** window opens (never a team dialog: a team is only made with the New team button, and dragging onto an agent already in a team or a pair says so): the one dropped on is the planner **and its own QA**, the dragged one is the worker, ⇄ swaps them before you start (the ⇄ on the bar swaps them; ⌁ unlinks, and a pair with one left is dissolved). A pair is not a team and not a single agent (pair programming: a planner that also judges + a worker): the same plan → work → judge → retry loop, with the planner judging the delivery against its own criteria (it is held read-only as before, and lent the delivery to inspect), and no third member. The Leader's brief now screens with **at most four short questions** and then asks itself whether a team is worth it: it must say in two sentences whether this team delivers more than one raw agent or costs less, and if one agent (or a pair) would do as well it **suggests that, never forces it**; if the person still wants the team, it builds it.
- **A run works in trees of its own.** A team's working tree used to be named by the unit alone (`<thread>--run`), so a second run started in what the first had left (its files, and the documents its agents drafted piling up). Now the tree is the run's (`<thread>--<run number>-<unit>`), cut fresh from the team's branch, and once its work is brought into the team's branch without a conflict the tree and its branch are taken away (a conflict keeps it for the debugger or you). The same for the side branches of a fork once joined. A run that was in flight when this is installed starts a new tree for its next step.
- **One result page for a run** (RFC 0010 §4.16). A run used to leave a pile of Markdown and HTML and nothing said which was the answer. A team now ends with a **Master page** (an `artifact` node with a `compose` spec): sections of widgets (hero, summary, markdown, kpis, table, checklist, timeline, cards, cost, links, callout, bar, embed) that the run turns into ONE self-contained, Linear-like page (section list on the left, a bar on a phone, light and dark), from data, with no model and no cost. All text is escaped, links are http(s) only, an embedded document sits in a sandboxed frame. A bad spec is refused when the team is saved, in words. The Run tab shows it first as the **Result** (Full page, Rebuild); a run that has no such node gets **Make the master page**, a default page built from what the run has. The Leader's brief ends every team with it.
- **Cowork's themes are on every screen** (RFC 0007 §17.8). The Omarchy palette you wear (and the wallpaper) used to live only while Cowork was open; now one holder at the root puts the palette on Sagas, X-ray and every other page, and on the framed X-ray panes, and draws the wallpaper behind every screen but Cowork (which draws its own). A page's big `--c-bg` washes go clear and `<main>` carries one glass layer (a card or chart keeps its own), so the picture shows through without stacking into a wall; menus and dialogs stay solid. The last palette is remembered so a reload paints it from the first frame. A theme worn here overrides Settings → Color theme until you pick "antiloki's own" in Cowork's appearance popover.
- **Teams on a schedule, and antiloki that keeps running when it is closed** (RFC 0010 §4.9, §4.17). A team may carry cron `triggers` and daily `limits`; the person says yes once in the Schedule panel (the yes is bound to exactly that schedule: changing it asks again), then **the serve starts the team itself**: overlap skips, a missed run goes once within 12 hours, a day's runs and dollars are capped, a gate parks and waits, an agent can set none of it, and `~/.antiloki/run/driver-halt` stops every schedule. `antiloki-main --service --yes` installs the serve as a **systemd user service** (starts at login, restarts if it dies, leaves terminals and seats alone when it restarts, runs with nobody logged in via linger). A schedule names the **seat** that does the work (`engine`, default Claude Code in the panel): a run with nobody there no longer falls to whatever the router picks first (it once picked a seat that was not installed and every run was parked). Checked against the real clock: two runs fired by themselves and finished, and after a 200 s outage exactly one catch-up run. The desktop app still embeds the serve (quitting it stops everything): attaching it to the service is the next step (M7b).
- **On a phone:** Cowork switches between its panes by **swiping** (a slide and a strip of dots; a drag that starts inside a field or a sideways-scrolling area is left alone), and the X-ray page folds its file tree and sections menu away and opens them from a bar of their own (they took 382 of 390 px). The app the serve hands out defaults to the Cowork screen, so a phone no longer opens the old chat screen.
- **Phone touch, fixed with a real finger test:** the swipe between panes now listens in the capture phase (a terminal stops touches from bubbling, so the first version never heard them), a finger scrolls a terminal's scrollback (xterm's canvas swallowed the touch), and the team circuit no longer sets `touch-action: none` over its whole area (it blocked scrolling the pane). `e2e/cowork-touch.spec.ts` drives real CDP touches at 390 x 844.
- **Team pane:** the Saga tab now lists the **sagas of the team** (the pack), not only the team's own saga; the Leader's chip follows the run (**watching** while it goes, **done** when it is) instead of "talking" for ever; an engine switched off in Settings says "turned off in Settings" instead of "not installed".
- **One instance:** the operator runs one serve on :41999 with the app on :5199 and the phone door on :41932; `antiloki-main` sets it up (backup first, dry run by default). Test stacks can no longer take the phone port.
- **Tried for real:** a real Claude Leader screened the goal "a marketing team for a coffee roaster", built four single-job agents with four skills (drafts, approved by the person), and the run on real seats made a brief, three social posts and an HTML dashboard as artifacts, delivered with nothing in the code. What it taught: a judging agent answers `pass` (a review loop ends on it), an agent that only makes documents has the effect `document`, `dry_run` to validate without making a team, and the scope gate now sees new files.
- **Tests can no longer take the phone's port.** A test stack left behind held the LAN port 41932 and the phone showed it (2026-11-12). `ANTILOKI_LAN_PORT` (0 = any free port) is set by the journeys, the browser tests and the real-stack launcher. `antiloki-main` (in `~/.local/bin`) sets up ONE instance: the current serve on :41999 with your data, the app on :5199, the phone door on :41932, with a backup first.
- Fixed on the way: two parallel branches racing to make the same git branch; a team with no planner landing before its work was integrated; a run not found by its pane because the door names its team with capitals and spaces.

## 2026-11-11 — Teams and episodes (RFC 0008, M-1a)

- **Crews are called teams.** The Cowork screen, the sidebar, the agents' MCP tool (`antiloki_teams`), the routes (`/api/v1/cockpit/teams`), the files (`.antiloki/teams.json`, `team-runs/`) and the saved layouts say team. The old names keep working for now: `antiloki_crews`, `/api/v1/cockpit/crews`, a request that says `crew`, a `crews.json` or `crew-runs/` written by an older build, a layout block that says `crew`.
- **A saga holds episodes, and an episode holds steps** (what the Sagas screen and the agents' tools called tasks and subtasks). The MCP tools are `antiloki_episode_add` and `antiloki_episode_update` (the old `antiloki_task_*` names answer, with a note); the HTTP ops are `episode-create | episode-update | episode-delete` and `GET /api/v1/sagas/episode` (the old `task-*` ones answer the same); every response carries both words (`episodes` and `tasks`, `steps` and `subtasks`). The database did not change, so an older build opens the same files.
- Not done yet (RFC 0008 §4.10): the old *agent* episode (the chapter of a conversation filed in the workspace memory) is legacy code and is retired in M-1b, after a survey of what the app still reaches.

## v0.5.4.7 — 2026-10-27 — Tyr, antiQA and the agent in your terminal

- **antiQA, the external QA tool, is a button in the Heimdall menu.** It sits beside QA and is not QA: Heimdall's QA judges its own runs; antiQA plays user stories against the running app. Turned on, the agent gets antiQA's tools (list the stories, run only the ones that cover what it changed, read the report, replay a story) and is told never to edit the stories to make a run pass. It needs `antiqa` 0.0.28 or newer on the machine (`ANTIQA_BIN`, else PATH); if it is not found the button says so ("antiQA !"). Chats and the read-only main agent do not get it.
- **You can see antiQA working.** A banner at the top of the pane says what it is checking and then what it found (green passed, red with the defect count, amber could-not-judge or drifted); the rail's ◎ icon blinks while it works and opens the list of runs with the stories the agent chose.
- **`antiloki`, the agent in your terminal, with one word.** In any git folder it opens the agent there, like Claude Code: it uses the serve the window uses (or starts one), sends each line the way the window's composer does (the front door resolves `go` / `yes` / `cancel`, a task or a question goes to Heimdall, the rest to the agent) and prints the log as text. `antiloki "<ask>"` or a pipe is one ask; `-i` reads a conversation; `antiloki app` opens the window. The work is in the window afterwards.
- **`scripts/install-linux.sh` installs it:** the terminal agent, the serve, the sidecar, the window and the programs-list entry, into `~/.local` (`--debug` for a fast build, `--no-window`, `--uninstall`).
- Checked: the scripted seat end to end in a real browser and from the terminal. Not checked: a turn where a model-backed pane calls antiQA through the config the button writes.

## v0.5.4.6 — 2026-10-17 — Tyr, checkpoints and the scope fence

- **Checkpoints are a timeline you can rewind and redo.** Save a checkpoint, rewind to any earlier one, and **redo** what the rewind undid. They live on the pane's worktree and never move your branch's HEAD. Rewinding asks for a second click on the button instead of a browser pop-up.
- **A run limited to some paths stays inside them.** A pane with a scope fence used to plan as unrestricted and could write outside it (a fenced pane wrote `package.json`). A request that names a file outside the fence now fails at once, before any model is called; a worker write outside the fence is put back while edits inside it stay; a task that owns files outside the fence never starts (a read-only one may).
- **antiloki leaves your project files alone.** In a normal checkout, a tracked `AGENTS.md`, `CLAUDE.md`, `opencode.json`, `mcp.json`, seat rule or `SKILL.md` is no longer rewritten, pruned or deleted, and a new file antiloki generates is listed in `.git/info/exclude` so `git status` stays clean.
- **The checkpoint door no longer says "not a git repository" for a repo that lists fine** when the system temp folder is a small capped one.
- **The checkpoint timeline shows this workspace and its agent boxes,** not every folder under the agents home.
- **The stop button's "are you sure?" survives a re-render** between the two clicks (it expires after four seconds).
- **Sign-in material is kept in a private folder** under the antiloki home.

## v0.5.4.5 — 2026-10-12 — Tyr, fixes from the re-test of 0.5.4.4

- **The role chip shows the role Heimdall chose.** It stayed "role · routed" because it read a field that does not exist; the verdict is stored on the run.
- **A worker that has said nothing for five minutes says so** ("still working: no output yet (N min)…", every five minutes), so a slow seat and a hung one no longer look the same on screen.
- **The Activity feed no longer drowns in status polls.** Every dispatched action, reads included, wrote two rows, so a screen polling all day filled the table; the six status polls (workspace list, runtime events, gate telemetry, driver status, rebuild status and latest) are no longer recorded, the table keeps its newest 50 000 rows, and the fifteen places that listed workspaces now share one request instead of making fifteen.
- **The budget box keeps what you type.** Clearing it snapped to 1000 and digits appended to it ("150000" became "1000150000"); it is tidied to at least 1000 when you leave it.
- **Esc closes the close-episode menu.**
- **Clicking a Heimdall switch or pill no longer takes the keyboard:** typing a space afterwards used to press it again.
- **"Save a checkpoint now" only commits.** It also sent the agent a made-up message ("checkpoint saved by the operator") that started an AI turn, even with nothing to save; it now says "checkpoint saved" or "nothing to checkpoint".
- **Forked agents are in the Sagas assignee list,** every clickable status icon names the move it makes (todo → in progress), and the stale "create the saga" tooltip is gone.
- **The code graph's buttons, pills, full-screen backdrops, the Architecture graph, the file tree's selected row and the code selection follow the theme** instead of staying black on a light theme.

## v0.5.4.4 — 2026-10-12 — Tyr, fixes from the QA of 0.5.4.3

Each claim of the tester's report was checked against the code; these are the ones that were real.

- **Typing no longer triggers shortcuts.** The letters `a p o q d k n ,` used to jump to other screens whenever the focus was on anything but a plain text field (a button, a select, or the page itself after a dialog closed). They are Alt chords now, and never fire while you are typing in a field, editor, terminal or dialog. The `\` key no longer opens the chat from inside a Notes page or the code editor.
- **The agent chat shows its replies even when the live stream is silent.** The pane read its turns only from a shared stream, so in a webview where that stream never delivered, the thread stayed blank while the episode card showed "3 replies". It now reads the history from the server whenever the server is ahead of what it has shown.
- **An agent that cannot start says why** (the banner showed "the agent is starting again" for ever), and the main agent no longer opens on an AI that is not installed or not signed in.
- **You can change an agent's engine again** (the switch had been removed by mistake with the antiChat chrome).
- **A missing agent box is reported** instead of the agent silently working in an empty folder.
- **Scope tree:** unticking a folder inside a ticked parent (`client/src` under `client/**`) now excludes it; before it added an entry and excluded nothing. The scope text box keeps what you type (commas and spaces were dropped).
- **The terminal keeps your keystrokes in order** ("git status" could arrive as " status").
- **X-ray:** the import graph of a small project no longer draws huge, clipped nodes; component source opens in projects that are not git repositories; the "not a git repo" message no longer talks about ZIP downloads.
- **The first preview no longer moves your own `node_modules` aside.** It did that for any `node_modules` without antiloki's marker file, and reinstalled everything.
- **Selects and inputs follow the theme** (light text on a white popup in the installed app).
- **Sagas:** the assignee list offers your agents; the activity line no longer repeats the task name; the status tooltip names the next status. **Checkpoints** no longer sit on "reading the branch…" when the folder has no git history.
- **Tours:** the Cowork tour described a fuel donut and clock that no longer exist; the Sagas tour no longer points at an empty strip.
- **Opening a project says what it writes** (`.antiloki/`, the antiloki block in `AGENTS.md` / `CLAUDE.md`, the seats' skill folders, a `node_modules` if the preview needs one) before you confirm.
- **The trial indicator is one of the top-right instruments** instead of a pill fixed in the bottom-left corner, where it sat over whatever a screen drew there.
- **Disabled buttons say why** (an empty saga title, "save as snapshot", the git host "check & save"), and Enter on an empty saga title answers instead of doing nothing.
- **The Sagas tour card no longer lands on the Telemetry panel.**
- **X-ray labels say what they measure:** the verdict's health (structural), the law health in the constitution, a box's own health on the map, the file count (source files) and the token chip.
- **Rebuilds record how long each phase took,** so Settings > Substrate no longer says "no rebuilds recorded" beside History's "1 rebuild recorded". Rebuilds from before this version have no timings.
- **Icons no longer show as empty boxes** on a machine without a symbol font: the app carries small subsets (about 16 KB) of open-licence symbol fonts for exactly the symbols it draws.

## v0.5.4.3 — 2026-10-12 — Tyr, a real code editor and the new Studio

- **X-ray's Code tab is a real editor.** Open a file, edit it and press Ctrl/Cmd+S: it is saved straight to your project. It has line numbers, syntax colours for the language, search and replace, folding, undo, and it keeps what you typed if you click to another file. If the file changed on disk while you were editing (an agent, git), nothing is overwritten without asking. The old "explain / regenerate / edit / playground / chat" line options are gone.
- **Edited code flags a rebuild.** After you save a change, the rebuild button says how many files were edited and that the analysis of them is out of date, until the next rebuild.
- **X-ray's menu is a sidebar on the right,** each section with a small line icon. The search and scope icons of the file tree no longer hide behind the logo. In the folder paper, play the paper, podcast, video cast and video class are shown blurred as "soon".
- **Issues replaces Todo and Kanban.** One tracker, Linear-style: a list grouped by status or a board, statuses (Backlog, Todo, In Progress, Done, Canceled), priorities, and a unique number for every issue (ISS-1, ISS-2…). Your old todos and cards are moved in the first time you open it.
- **Notes is one page that saves itself.** No list, no save button: you write, and markdown is drawn as you type, the way Obsidian does it. A `#` makes a title at once, `**bold**`, `- ` bullets, `- [ ]` checkboxes you can click, quotes and code blocks; the raw markdown comes back on the line you are editing.
- **Brainstorm replaces Intent.** A simple chat: pick what you want (what, where, how bold, what it must respect) as chips, say it in a line, and the plan lands on the shape pane. The planner's depth is chosen by the size of the ask; there is no Ultra / Quick choice and no sources line.
- **One Studio list.** The Studio is a single list under one title; Design canvas sits with Frames and Design system in the design colour. Automation, Context, Sketch, Standup and Components are no longer offered.
- **The router goes up a tier when it is unsure or the work is risky.** If the router grades a route low, or a task touches credentials, billing, a database migration or a deploy file (or the ask says production, password, billing…), it uses a stronger model for it. A tier you pinned yourself is never changed.
- **Everything the tools hold is in your project's database.** Issues, notes, sketches and the Brainstorm conversation are stored in `.antiloki/db/state.sqlite`, not in the browser.
- **Smaller fixes.** The text field of the agent chat wraps and grows instead of running off the edge; the main agent is yellow instead of brown.

## v0.5.4.2 — 2026-10-11 — Tyr, more from the Windows test

- **Setup can no longer be skipped at the start.** The first three steps (theme, your AIs, the check of each one) have no "skip the setup" button any more; the optional steps after them still do.
- **The last setup step is up to date.** It now describes what antiloki is today: Cowork, Sagas, X-ray, the top bar with your agents, the money saved and each AI's weekly limits, Heimdall and its confidence readings, and Settings. The old "antiChat" and "Threads" text is gone.
- **The first agent no longer says it is "read-only".** It answered "hi" with "I'm in read-only mode, I won't edit files". That describes how the chat is wired (the conversation never writes; Heimdall's runs do the editing, in a safe copy, and check it), not what you can get done, and Cursor's own ask mode volunteered it. The chat is now told what it is, and to say it will hand changes to Heimdall.
- **An agent's scope starts as the whole project.** In an agent's settings, and for a new agent, the scope is "everything" with every file ticked; narrow it by unticking. Before, it started empty.
- **The agent settings window opens above the Studio sidebar,** not underneath it.

## v0.5.4.1 — 2026-10-11 — Tyr, fixes from the first Windows test

Found the day 0.5.4 shipped, by testing it on a clean Windows machine.

- **Sign in from inside antiloki.** An AI that is installed but not signed in used to be skipped, with an instruction to open a terminal and run its login yourself. Now it has a **Sign in** button (on the setup screen and in the Check step) that opens a terminal window running that AI's own sign-in, which opens the vendor's browser flow. antiloki never sees your credentials. When the sign-in lands it notices by itself, and the Check step tests that AI right then; before, the check ran once, for whatever was ready at that moment.
- **The preview works in the installed app.** It showed "This content is blocked. Contact the site owner to fix the issue." because the app's security policy did not allow the preview's local page to be shown in a frame. It now allows pages from your own machine, and nothing else.
- **Cursor works in the chat on Windows.** The chat failed with "cursor-agent: batch file arguments are invalid": on Windows Cursor's command is a batch file, which cannot take a message with line breaks on its command line, and antiloki sends context with every ask. The message now goes in on stdin.
- **The money chip shows after the first check.** The saved-money value in the top-right pill stayed blank until the router had run a task. It now appears as soon as anything has been measured, including the first-run check's tiny calls: "$0.00" with a note that nothing has been routed yet, and the real figure once something has.
- **The antiloki icon** replaces the placeholder in the installer, the app window and the taskbar.

## v0.5.4 — 2026-10-11 — Tyr, projects no longer share a conversation

Found by the first full test on a clean Windows machine.

- **Sagas: a third screen, next to Cowork and X-ray.** A place, per project, where you and your agents keep the work. A **saga** is a titled goal (the plan an AI writes is one), it holds **tasks**, and a task holds subtasks. Each task has an assignee (you, Heimdall, or an agent), and the activity trail names who did each step. The numbers (time, cost, which AI) come from the real runs: ask an agent for something and its plan shows up as a saga by itself, with its tasks moving as the run does. The list, a saga page and a task page follow the layout of Linear's issues page, with "New saga" and "New task". It lives in `<your project>/.antiloki/work.sqlite`, so each project has its own, and agents can read and add to it through antiloki's own tools (sub-agents can read it, never write it). "Plan" is now called "saga" wherever you accept or reject one. Each task shows the agent's face and the icon of the exact model that did it (a failed one wears a small red mark), the list shows the faces and models behind each saga, and a finished saga can no longer be canceled or deleted. There is a back button inside a saga, the description grows with its text, and two mini tours (the screen, and a saga page) open the first time you visit.
- **Each project has its own conversations.** Opening antiloki used to show the same AI conversation in every project. Chats were kept in a folder named only by the chat's number, and every project numbers its chats from 1, so project A's chat 5 and project B's chat 5 were the same folder. Chats now live in a folder per project, and switching project stops the running agents of the old one. A chat made before this update starts fresh in its project; its old shared folder is left untouched.
- **Benchmarks ship with antiloki.** A new install used to start with an empty benchmark table, so the router had almost nothing to go on. antiloki now carries 373 measured combinations (1,186 runs) as its defaults. Your own runs add on top of them, never replace them. In Settings, every row says whether it is antiloki's default, measured on your machine, or both, and a line over the table counts each.
- **Commit & push under the chat input.** Next to the episode button: it shows what would be committed, writes the message for you (from what you asked and the files changed) or lets you edit it, and commits or commits and pushes. antiloki's own files are never included.
- **Agent settings are simpler.** "Merge & subagents" is hidden: the merge and the sub-agents are chosen automatically. It still appears when a run needs your answer or approval.
- **A tidier top bar.** The three instruments (agents, money, limits) now have a floating pill of their own at the top right, which also carries the collapsed antiloki logo; the screen switcher is just Cowork, X-ray and Sagas. The wordmark at the top left stays expanded on X-ray and Sagas. The fuel preview is narrower and taller, and previews no longer run off the screen edge. The floating chat field is gone from the Sagas screen.
- **Ultra review is "soon".** The tab stays in X-ray with a "soon" mark, and its page shows blurred behind a Soon card until it is fixed.
- **No house theme.** An agent with no theme of its own takes the look of its AI (Claude, Codex, Cursor, Gemini, Grok, OpenCode) instead of an "antiloki" theme.
- **Fresh-install testing.** The Windows uninstaller's "delete the application data" box now removes antiloki's data (database, licence, settings), and `scripts/reset-antiloki-windows.ps1` wipes it by hand, so the first-run onboarding can be tested from scratch.

## v0.5.3 — 2026-10-11 — Tyr, a full onboarding

The first thing a new user does is tell antiloki which AI subscriptions they have. That screen now does the whole job.

- **It asks first.** "Which subscriptions do you have?" with each one named for what it is (Claude Pro or Max, ChatGPT Plus or Pro for Codex, Cursor Pro, a Google account for the free Gemini tier). The ones whose tool is already on the computer are ticked for you.
- **It shows exactly what is missing, for your system.** On Windows the install line is the PowerShell one and says to open PowerShell; on Linux it is the terminal one. A line that needs Node.js (Codex) says so. You copy the bare command with one click and run it yourself: antiloki never sees a password.
- **It notices by itself.** Install a tool in another window, sign in, come back: antiloki rechecks every few seconds (and has a "check again" button), and ticks the new one. A tool installed after antiloki was opened is found without restarting it. Cursor's new `agent` command name is recognised as well as the old `cursor-agent`.
- **You choose the main provider for Heimdall** on the same screen (and it is chosen for you when only one AI is ready).
- **A new Check step tests every AI before you start.** Each ready AI gets one tiny real call, so a broken sign-in shows up now and not on your first task, and antiloki reads how much of this week each subscription has used. The switcher at the top starts with the real numbers: Claude's and Codex's 5-hour and weekly windows, Cursor's own meters, and for Gemini (which publishes none) what antiloki sends from now on.
- **Windows:** a missing login file no longer makes Cursor, Antigravity or Grok look signed out. Their login lives where antiloki cannot read it cheaply, so it now says "unknown" and the Check step's real call decides.

## v0.5.2 — 2026-10-11 — Tyr, offline serials work in the released builds

- **A serial you are given now unlocks the app.** 0.5.0 and 0.5.1 were built without the key that checks offline serials, so every serial was refused with "this build carries no serial public key". The key is in the build now, and the build scripts load it by themselves and warn when it is missing.

## v0.5.1 — 2026-10-11 — Tyr, the licence screen on a machine whose trial has ended

Found the day 0.5.0 shipped, on a Windows machine that had older antiloki versions and an ended trial: the app said "license required" and offered no way out.

- **A machine whose trial has ended now shows the licence screen**, with the field for your key and the button to buy. The server refuses every request once the trial is over, the list of workspaces included, so the screen that picks a workspace came up instead and the licence screen, which lived inside the app, never did. It now sits above everything, so it is there on every screen.
- **The buy button always goes somewhere:** a build that carries no store link opens the website's pricing section instead of an error.

## v0.5.0 — 2026-10-10 — Tyr, the first usable version

Tyr is the god of oaths, who kept his word at the price of a hand; this release is named for the same idea, a result that says only what it can show. 0.1.x was the test period. 0.5.0 is the first version meant to be used day to day and sold: a trial, a one-time licence, and a result you can judge without paying for a second opinion. (The jump from 0.1.6 is deliberate: it marks the first usable version, not a rewrite.)

- **QA is one optional button.** A run no longer pays for a QA pass unless you ask. Every finished run shows a **confidence reading** (high, medium or low) with its reasons (did the compiler or the project's tests pass, did the verifier accept the claims, how many attempts it took, did a design rule fail), and a button runs an independent QA on it when you want one. QA reports back without changing a delivered run.
- **Cursor limits come from Cursor itself.** The usage meters read your real Cursor plan (a monthly plan) instead of an estimate.
- **Small asks go straight to one worker**, with no planner round; a bigger ask is split into tasks as before. A per-run switch (never / always) overrides it.
- **QA catches more:** visible text that a restyle removed is flagged, and asks about a cart, checkout, panel, drawer, dialog and similar things are tested, not only looked at.
- **The code map follows imports and references on React projects**, so "who uses this" answers are complete.
- **The agent no longer asks the same question twice** after you answered it.
- **The agent picker folds the pairs that need a seat you have switched off.**
- **Licence:** a 14-day free trial, then one payment of USD 499 for a lifetime licence with two years of updates and support. A build released inside your window keeps working for good. The licence check supports Creem (the payment provider) and keeps the offline serials; a release build trusts only the address it was built with.
- **Platforms:** Windows and Linux. The Linux package built on Arch needs glibc 2.44 or newer; wider support comes from the CI build. macOS is coming.


## v0.1.6 — the numbers, and the platform that runs them

Two halves. One asks where a recommendation comes from — several of the answers turned out to be wrong, and a
benchmark that flatters the product is worse than none. The other is `WINDOWS-TESTING.md` finally run on the
machine it was written for, which turned up four bugs that had never once executed a line of their `#[cfg]` arm.
All four were silent: none logged, none errored. They answered zero, or false, or yes-to-everything — which reads
as a feature being calm rather than a feature being absent.

### Windows, finally run on Windows

- **Every toolchain claimed to be installed, on a machine that had none of them.** The probe ran `bin --version`
  and took *any* output as proof of life — and on Windows it went through `cmd /C`, which answers a name it
  cannot find by writing `'kotlinc' is not recognized as an internal or external command` to **stderr**. So the
  success condition was met by the failure message: go, javac, gcc, g++, php, ruby, kotlinc, swiftc, scalac,
  luac, mix and bash all reported present where only cargo and dotnet existed. Twelve false positives shown to
  you in the wizard, each one a process the checker then spawned for nothing. It resolves the PATH now, walking
  `PATHEXT` and spawning nothing at all.
- **The RAM beside each agent was always zero.** The watchdog reads `/proc`, which Windows does not have, so the
  process tree came back empty, every agent's memory summed to nothing, and the machine's own total was
  unreadable. An unreadable machine is deliberately never an emergency — correct, and here it hid the hole:
  the pressure never left *easy*, so the emergency pause could never have fired. Windows now answers all three
  questions through its own process snapshot and memory API.
- **An agent's box on Windows had no dependencies at all.** `node_modules` is linked from your workspace rather
  than installed per agent; off Unix that link was simply never made, which left the test, build and lint gates
  unrunnable inside a box — the exact failure the previous worktree design was abandoned over. It is a junction
  now, which needs no privileges, where a symlink needs Developer Mode.
- **The serve never told anything which port it bound.** Run on its own it listens on 41998 while the pane bake
  writes 41931 into every engine's MCP config, so a provisioned agent was handed an address with nothing behind
  it and its own antiloki tools could not reach the substrate that spawned it.
- `WINDOWS-TESTING.md` named the wrong port for its own step 2 and 3, where every command then answered
  `Unable to connect to the remote server` — indistinguishable from the bug it was written to test.

0.1.5 was what the first Windows install sent back. 0.1.6 is mostly one question asked over and over: **when
antiloki tells you a model is the best one for a job, where did that come from?** Several of the answers turned out
to be wrong, and a benchmark that flatters the product is worse than none.

### The record that picks your models

- **A benchmark can no longer quietly become a recommendation.** Every measurement now carries which *suite* it came
  from, and two benchmarks that happen to test the same combination stay two rows instead of averaging into a number
  that describes neither. SWE-bench was audited against what antiloki actually claims — it sees three of eight
  claims, is blind to four, and penalises splitting work into parts — so it is kept as a **control**: printed for you
  to read, and never allowed to move the router. Four separate places were still letting it through, including the
  router's own lookup; they now all ask one list.
- **Sonnet looked cheaper than Qwen, which it cannot be.** Every arm of a benchmark run wrote to the same file, so
  one engine read another's leftovers and reported a cost 15× too low. One cost parser per engine now, no
  fall-through, and a missing figure says *unknown* instead of zero.
- **Evidence from a bench and evidence from your own runs are no longer scored against each other.** They answer
  different questions, and the weight says which is which.
- **A one-off no longer outranks a repeated result.** Thin evidence is held toward the middle and says so.
- **The benchmark screen shows what was measured, and on what** — the kind of work, the language, which bench, which
  evidence class, and now a view per bench and per role. A winner cannot hide where it won.
- **It also says what to measure next, and what not to pay for** — with the cost of settling each open question.

### Things that stopped work

- **"Set the threshold to 0.5" halted a run.** The screening gate read `0.5` as a filename, could not find the file,
  and stopped to ask you about it. Decimals, fractions, dates, `e.g.` and `//` are no longer paths. Found by running
  a public benchmark against our own gate: it was blocking 13% of perfectly clear requests, now 2%.
- **Switching workspace kept the previous project's data on screen.** Real data, wrong repository, nothing saying
  so. Every surface now empties and refetches, behind a full-screen loading state.
- **A run could pick a model and then fail to start it.** When a pair's fallback was the same model as the winner,
  there was no fallback at all — two benchmark runs died with no retry.
- **Codex and OpenCode turns recorded zero tokens** — a float was being read as a whole number, so every one of
  their costs was silently lost.

### New

- **Correct what the planner assumed.** When a request is vague the planner does not stop to ask you — measured, it
  asks in under 6% of cases. It writes down the reading it took instead, on ~9 of 10 genuinely vague requests. Those
  assumptions are no longer a grey footnote: click one to tell it what it should have assumed.
- **Two ways to do an ask, priced before you choose** — the quick one and the thorough one, with the difference in
  money and minutes, and the deep one recommended only when it is actually better.
- **Run this one raw, for comparison** — the same ask through a plain session, priced first, offered only where the
  answer could change your mind.
- **The fallback ladder keeps its own record and reorders itself**, conservatively: a rung with few attempts keeps
  its declared place, because two failures on a Tuesday are not a verdict.
- **Every other coding CLI on your machine opens as a pane** — Cursor Agent, Gemini, Aider, Amp, Kimi, Copilot,
  Grok, Droid — stated plainly as raw panes, because antiloki cannot read their token accounting and will not
  pretend to.
- **A plan that worked is not paid for twice**, and a repeated ask is treated as the correctness signal it is.
- **X-ray's code laws are judged on the diff**, in process, and the gate is on by default.
- **The Agents screen is now Cowork.** Agents are the rows in the sidebar; the screen needed its own name.
- **`antiloki doctor`** — documentation that cannot go stale, plus a two-minute check that says whether antiloki
  still works.

### Faster

- **58% off every build** (thin LTO), and a guard against publishing a stale signature.

## v0.1.5 — 2026-09-25 — what the first Windows install sent back

0.1.4 was built the day antiloki was first installed on Windows. Using it there produced thirteen reports;
these are the ones fixed so far, and three of them had a single cause.

- **The CLIs were never findable on Windows.** `Command::new("claude")` searches the PATH for `claude` and
  `claude.exe` and nothing else — it does not walk `PATHEXT`. Every one of these CLIs is installed by npm,
  which on Windows installs a `.cmd` shim, so there was no `claude.exe` to find. The chat's engine never
  started and the pane stayed empty; the preview's `npm` never started. Both now resolve through the PATH
  properly, and the `cmd.exe` that runs a `.cmd` is created with `CREATE_NO_WINDOW`, so it does not flash.
- **The console windows.** 0.1.4 covered the server; the skills spawn 107 times of their own and were missed.
  Every child process in the product now comes from one place that cannot forget the flag.
- **The folder picker worked on Windows only.** Choosing a project folder went to a PowerShell dialog guarded
  by `if !cfg!(windows) { return None }`, so on Linux and macOS the button opened nothing and — because the
  reply omitted whether a dialog existed at all — said nothing either. Inside the app it is the desktop
  dialog now, which works on all three.
- **Every agent gets its own worktree.** It was a choice on the creation form, defaulting to writing straight
  into your workspace. The reason given for that default was that the live preview only shows changes made
  where you work — which was never true: the preview resolves a pane to that pane's own worktree. The choice
  is gone; an episode is a branch in the agent's box, merged back when it closes.
- **Your ask appears the moment you send it.** Every turn came from the server's event log, so nothing was
  drawn until the engine echoed the ask back — and when Heimdall routes, that echo only arrives once a model
  has been picked and started. The router's rail would blink while the transcript stayed empty.
- **The model picker is a dropdown**, over the models this machine actually has, and the engine list beside it
  offers only engines that are installed. Merges you cannot run are shown locked with the command that
  installs them.
- `WINDOWS-TESTING.md` — how to check all of the above from the Windows terminal, starting with the unit
  tests that assert the platform behaviour directly and need no build.

## v0.1.4 — 2026-09-24 — the Windows release

The first build made after antiloki was actually installed on Windows. Four reports came back from that
install and three of them were the same cause: the serve had one `#[cfg(windows)]` in it and forty places
where it starts a child process.

- **No more blinking terminals.** A GUI app owns no console on Windows, so every process it started was given
  a fresh one that opened, flashed and closed — eighteen of the forty spawns are `git`, most on the path a
  single click takes. Every spawn now sets `CREATE_NO_WINDOW`.
- **The test · build · lint gates run.** They went through `sh -c`, which does not exist on Windows, so they
  never started; it is the platform's own shell now. Their file paths were also wrapped in single quotes,
  which on `cmd.exe` is two literal apostrophes rather than quoting, and paths containing a space were
  dropped — on Windows that silently skipped most of a repo.
- **Gates discover how a project is built.** Discovery read `package.json` and nothing else, so every gate in
  a repo that is not a Node project skipped with "no package.json in the box". It now reads the manifests that
  are there — the npm script, a Makefile target the project declared, then Cargo, Go, Python, .NET, Gradle or
  Maven. It still never invents a command, and a gate with nothing to run is skipped, never passed.
- **Engines are detected.** Detection shelled out to `which`, which is not a program on Windows, so every
  engine reported missing on a machine that had all three.
- **The agent wizard asks this machine first.** Merges whose engine is not installed are shown locked with the
  command that installs them, and the case for installing it, from measured bench rows. OpenCode is no longer
  labelled after one of its models, so the picker shows DeepSeek, Kimi, Qwen and GLM for what they are.
- **Main explains itself.** The read-only agent opens with a deterministic account of what it is and how a
  merge works, built from what is installed here. Its Heimdall menu no longer offers QA or the build gates —
  controls for work an agent that never writes cannot do.
- **The tour stops describing a surface that was renamed**, and gains three steps on hiring an agent.

## v0.1.3 — 2026-09-21 · the first build that runs on a machine that is not the build machine

**The fix a fresh install needed.** An installed app kept looking for its database where the *build
machine* had its source checkout (`D:\a\antiloki-core-rust\…`), so a first run on any other computer
failed before the window opened. The app now keeps its data in your user data folder —
`%LOCALAPPDATA%\antiloki` on Windows, `~/Library/Application Support/antiloki` on macOS,
`~/.local/share/antiloki` on Linux — created on first run, with the 7-day trial starting there.

Everything listed under v0.1.2 below ships in this build (v0.1.2 was tagged but never built).

## v0.1.2 — 2026-09-21 · the chat session for all three engines, MIN, QA, and updates that keep their word

**Every engine is a conversation.** Claude Code, Codex (`codex app-server`) and OpenCode (`opencode serve`)
run as chat sessions on one event model — no terminal to resize, permissions as cards, the router's tier
as Codex's reasoning effort. Each pane spawns in its engine's own theme. `#` in the composer hands the
agent any other cell of the thread (a kanban, a diff, the code graph) as context; `@` stays as it was.
Buttons under every reply: simplify · expand · summary · copy.

**MIN, the opposite of MAX.** The agent navigates with antiloki's measured intel and the live tools and
never explores the tree; it reads a file only to edit it. Measured on 176 real sessions, exploration
was half of the context — MIN's saved money sits on the pill next to the router's. An ask naming files
antiloki has no intel for runs with exploration on, once, by itself.

**✓ QA.** One button: after each plan finishes, an isolated judge reviews the whole run against the
plan's criteria — unmet, drift, false claims, risks — and its verdict lands in the pane's rail.

**▶ preview.** One button: a run cell at the side boots the agent's worktree dev server and shows it live.

**The progressive tour.** Twelve sections, three steps each, unlocked the first time you reach one.

**Automation.** antiFlow's pipeline screen — the driver's queue and the cards through PL → TL → DEV →
QA → MAN → GIT — as a thread widget.

**Chats.** The desk has two tabs, CHATS · THREADS. A chat is a read-only agent over the workspace —
the same session model as the agents, on Claude Code, Codex or OpenCode — that answers from the code,
antiloki's intel and the other agents' work, and never writes. Before every ask a retriever picks the
chats, runs and intel that matter and shows them as chips under the reply. Episodes are consolidations
of one conversation: a new topic, a long gap or a full window closes a chapter, names it, and the
conversation goes on; `!` in the composer brings a closed chapter back; the provider chip switches the
same chat to another engine mid-way, and it gets the conversation so far.

**QA, sealed.** The judge also reviews the asks your pane does in one turn, not only the plans. Its
verdict rides on the pull request's body, and a box whose last judged run failed does not open a PR
until you say so.

**⊕ files.** The OS file dialog: a workspace file becomes an @chip with its scope; anything else, an
attachment. Pull requests are born when an agent closes.

**Serials.** A license is a serial you can verify offline — yearly or lifetime — signed by the
publisher's key; the trial is 7 days from the first run.

**Updates that keep their word.** A yearly key buys every version released before its date and keeps
them forever; a newer version is what the renewal buys. The app backs up its database before a newer
version first opens it. One set of streams per browser: any number of tabs, no more starving.

## v0.1.1 — 2026-09-19 · every engine drives the loop, and the result is a PR

**Delegation on all three CLIs, verified for real.** A Claude, Codex or OpenCode terminal calls
`antiloki_delegate`; a high-tier planner picks the specialist lenses per task; isolated sub-agents run
headless on the same subscription (Codex: reasoning effort per tier; OpenCode: your own Go/Zen login —
deepseek-v4-flash · deepseek-v4-pro — or the metered proxy when an OpenRouter key is on the box); a
verifier judges the real diff and rejects claims without evidence; the joined report lands back in the
terminal. Sub-agents never delegate again; an empty answer retries once a tier up; an engine failure
says why.

**Write mode ends in a PR.** A finished write run commits its box — the goal as the title, the joined
report as the body — and the ⎇ icon in the Threads · X-ray switch opens the whole Pulls & Tests
screen: diff, mergeability, analysis, merge. Verified on Claude, Codex and OpenCode.

**Ultra review** is the paper creator: the specialists rail, missions with their own scope (file
reviews and folder papers included), the run-on choice per role (agents mid · editor high) with a
token/cost estimate before the run, and a real-time run log — every call an isolated CLI run.

**The agent pane** keeps one ⚙ (context, subagents with the specialists and skills available to
the agent, plans with their history, checkpoints) and the quiet numbers; the plan strip on top opens
the watcher.

**Fixed:** Codex panes boot on a fresh box (trust + hooks prompts answered, MCP tools pre-approved);
the first ask is typed only once the CLI's composer is up; a multibyte TUI paint no longer freezes
a pane; the goal history survives a serve killed mid-write; the atlas loads on Linux (a Windows-only
path port) and beyond 5 s.

## v0.1.0 — 2026-09-19 · the first standalone

One executable per OS: the Rust engine, the axum server and the operator UI in a single Tauri app,
plus the `antiloki-agent` sidecar. 7-day trial, then yearly or lifetime (Lemon Squeezy); updates
from this release feed. Linux needs glibc ≥ 2.38 (Ubuntu 24.04+, Fedora 39+, Arch); macOS is arm64.

**Runs on the subscription you already pay for.** Every AI call — the chat, the reviews, the
descriptions, the plans — goes to your coding CLI (Claude Code · Codex · OpenCode), headless and
read-only, per tier (high · mid · low). No API key needed; an OpenRouter key or a local Ollama model
are the other options. The first run of a workspace asks for the theme, the engines and builds the
X-ray before the shell opens.

**Threads** — one job, one thread; the chat, the agents (each in its own worktree) and the tools side
by side. The Studio previews every widget live before you add it. Fuel and the timeline ride the top
switch; the timeline explains every row.

**The box** — a write outside a pane's scope is refused *before* the file changes (the sidecar's
PreToolUse hook), the audit records every read, write, refusal and shell command, and an agent can
delegate a goal into verified parallel tasks (goal runs) and get one joined report.

**X-ray** — the code-analysis tool: Overview (the verdict, the map, hotspots, a year of activity),
Code (the code first, the review a flip away — with "why not 100"; folders open their paper +
neighborhood), Design (system + drift, components wired to the UI by the engine, frames), Quality
(the rules switchboard), Architecture (data stories, deterministic and AI-scraped), History, and
Ultra review (the $0 survey → territory → dossiers → deep audit, then the engine's verified findings).
A check and a linter exist for every parsed language. Every section drags into the chat as context —
and the chat sees the screen: the brief, the open file's dossier, the numbers.

Known: macOS builds are unsigned until the Apple secrets are set; no intel-mac build; narration audio
is not in the standalone.
