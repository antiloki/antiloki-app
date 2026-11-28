# Extending antiloki

Two common changes, as a map of where to look. It is not a tutorial: the compiler and the tests will tell you what you missed, and the checklists below are made to be finished with a search.

Build and test rules are in [CONTRIBUTING.md](CONTRIBUTING.md); how the pieces fit is in [ARCHITECTURE.md](ARCHITECTURE.md).

## Add a seat (another AI tool run through its own CLI)

A seat is one AI subscription reached through its vendor's command-line program. The seat has an id (stored in settings, so never change an id that shipped), a name people see, a binary, an install line and a way to sign in.

1. **The engine itself** — `serve/src/cli.rs`:
   - add a variant to the `Engine` enum and its `key()`;
   - list the binary names it may be found under;
   - parse its id in `engine_of`;
   - give it a `provider_name`.
2. **What people are told about it** — `serve/src/engines.rs`: the seat table (id, name, binary, install command), then the install, sign-in and "what you need" sentences for each platform (Windows and Linux).
3. **How it is run headless** — `serve/src/agent_print.rs`: the flags that make the CLI print an answer and exit. Each CLI has one flag that, if wrong, prints nothing: test it against the real tool.
4. **Everywhere else the engine is a case** — search for an existing seat and follow it: `grep -rn "Engine::Grok" serve/src` lists the files (`goals.rs`, `describe.rs`, `looks.rs`, `roles.rs`, `custom.rs`, `chat_pane.rs`).
5. **The front end** — the seat's name, colour and glyph are in a handful of tables: `grep -rln "grok" src` in the `antiloki-ui` repository (`settings/ai-engines.tsx`, `cowork/provider-icons.tsx`, `sagas/model-icon.tsx`, `cockpit/agent-themes.ts`, `cockpit/usage-model.ts`, `widgets/agent-recipes.ts`, `chat/chat-modes.tsx`, `model-meta.ts`).
6. **Tests** — the seat lists are asserted in several tests; run the whole serve library and the front end's suite and fix the expectations that name the seats.

Windows: if the CLI behaves differently there (paths, quoting, how it signs in), say so in the sign-in sentence and check the server with `cargo xwin check -p antiloki-serve --target x86_64-pc-windows-msvc`.

## Add a node type (a new kind of step in a team graph)

A team is a graph of nodes. Each node has a kind (agent, tool, check, fork, gate, sub-team, and so on).

1. **The shape** — `serve/src/team_graph.rs`: add a variant to the `Kind` enum with its fields (`serde` defaults for every optional one, so older graphs still load), and teach the validator what is legal (what edges it needs, what it must not be wired to).
2. **What it does** — `serve/src/team_graph_run.rs`: the scheduler matches on the kind; add a branch and a function that runs it and returns either its result or a failure that says why. Look at how the `Check` and `Transform` nodes are written: a node that calls no model should say so.
3. **Telling the Leader** — the text that lists the nodes a Leader can use is built in `team_graph.rs` (grouped by category): give the new kind a one-line description there, or the Leader will never choose it.
4. **The front end** — in the `antiloki-ui` repository: the kind in `src/loki/team/types.ts`, its icon and label in `src/loki/team/AgentGraph.tsx` (the graph) and `src/loki/team/NodeInterior.tsx` (the settings it opens), and its place in the editor's palette.
5. **Tests** — a unit test for the validation, and if it touches a run, a journey in `serve/tests/journeys_process.rs` (those are heavy: run them alone).

## Before you open a pull request

- the serve library tests, the front end's type check and tests;
- a line in `CHANGELOG.md` for anything a person would notice;
- no secret, no personal path, no measured claim you cannot point to.
