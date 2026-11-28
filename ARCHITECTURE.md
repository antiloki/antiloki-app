# antiloki: architecture

How the pieces fit, for someone who wants to change them. Written 2026-11-23. Every file path in backticks below is checked by a test (`doctor::tests::every_file_architecture_md_points_at_still_exists`), so a rename that leaves this page stale fails the build.

## The shape

antiloki is a desktop app made of four parts that talk over local HTTP:

| Part | Where | What it is |
|---|---|---|
| Desktop shell | `tauri/src/main.rs`, `tauri/tauri.conf.json` | A Tauri 2 window around the front end. It starts the backend and the sidecar, owns the tray and the small avatar window. |
| Backend | `serve/src/lib.rs`, `serve/src/main.rs` | `antiloki-serve`: a Rust axum server on `127.0.0.1:41931` with a single SQLite store per project. Everything the app does is a route here. |
| Sidecar | `agent/src/main.rs`, `agent/src/mcp.rs`, `agent/src/hook.rs` | `antiloki-agent`: the MCP server, hooks and status line that the AI tools talk to. Each MCP tool calls back into the backend. |
| Front end | the `antiloki-ui` repository | React, TypeScript and Vite. In development it runs on `:5173`; in the desktop build its `dist/` is bundled by Tauri. |

Nothing leaves the machine except the AI CLIs' own calls and the few network calls listed in the privacy policy. The backend refuses requests from foreign origins and hosts (`serve/src/guard.rs`).

## The backend by area

- **Teams** are graphs of nodes (agent, tool, check, fork, gate, sub-team). `serve/src/team_graph_run.rs` runs the operations on a graph; `serve/src/team_template.rs` loads the shipped templates in `templates/teams/`.
- **Self-improvement.** Evolve proposes one change at a time and judges it against the noise (`serve/src/team_evolve.rs`, `serve/src/team_experiment.rs`). Swarm races several ideas under a concurrency cap (`serve/src/team_race.rs`). Design: `rfcs/0019-swarm.md`.
- **The machine comes first.** `serve/src/mem.rs` watches free memory and CPU and pauses background runs, one at a time, until there is room.
- **Specialists** are agents with a memory, tools of their own (a small scripting language) and a gate: `serve/src/specialists.rs`. Free panes (pages written by a person, an agent or a team, run in a sandbox) live in `serve/src/panes.rs`.
- **Routing (Heimdall).** `serve/src/goals.rs` turns an ask into tasks and runs them in git worktrees; `serve/src/merges.rs` holds the benchmark evidence that picks the seat per job (its starting table is `serve/src/merges-defaults.json`); `serve/src/front.rs` and `serve/src/gauge.rs` are the deterministic deciders in front of any model.
- **Seats.** Each AI subscription is reached through its own vendor CLI: `serve/src/cli.rs` starts and watches the processes, `serve/src/pty.rs` gives them a terminal.
- **Deterministic lanes (the book).** Laws written as data and run by plain code judge any kind of work: `serve/src/lanes/`.
- **The screen commands.** An agent can drive the app's screen over MCP: `serve/src/cowork_mcp.rs` receives the tool call and `serve/src/cowork_cmds.rs` queues the command for the front end.

## Rules that keep it portable

- Windows and Linux must both work. In `serve`, do not start processes with `Command::new` or `sh -c` directly and do not assume POSIX paths; use the process helper module.
- Never hold the single writer connection to the store across a call that can come back into the backend (it deadlocks).
- A failure must say why; a silent path is a bug.

## Build and test

See the README. The release build is `scripts/desktop-build.sh`.
