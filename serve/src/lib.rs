//! antiloki-serve — the axum surface that replaces antiloki-BE, dispatching straight into the
//! engine crate instead of across a napi boundary into Node.
//!
//! A LIBRARY as well as a binary: the Tauri desktop shell links this crate and calls `run()` on a
//! background runtime, so the single binary contains the server rather than spawning one. The
//! standalone bin stays for development and for the A/B parity harness.
//!
//! Port: ANTILOKI_SERVE_PORT, default 41998 for the standalone bin so it never collides with the
//! BE's 41931.

mod catalog;
mod custom;
pub mod dispatch;
pub mod preview;
pub mod antiqa_runs;
mod providers;
mod agy_usage;
mod proxy;
mod files;
mod pty;
mod pty_remote;
pub mod cowork;
mod cowork_engines;
mod cowork_workers;
mod cowork_asks;
mod cowork_cmds;
mod cowork_mcp;
mod cowork_xray;
mod xray_widgets;
pub mod team;
mod team_run;
pub mod team_graph;
pub mod team_compose;
pub mod team_graph_run;
mod team_skills;
pub mod team_check;
pub mod team_code;
pub mod team_memory;
pub mod team_router;
pub mod team_swarm;
pub mod mcp_install;
pub mod team_bench;
pub mod team_regulate;
pub mod team_improve;
pub mod team_db;
pub mod team_file;
pub mod team_saga_node;
pub mod team_subteam;
pub mod mcp_client;
pub mod mcp_fake;
pub mod app_tokens;
pub mod mcp_oauth;
pub mod panes;
pub mod team_transform;
pub mod team_llm;
pub mod team_seats;
pub mod team_apps;
pub mod team_campaign;
pub mod team_audit;
pub mod team_nodebench;
pub mod team_patch;
mod team_web;
mod team_triggers;
mod team_guard;
mod attach;
mod diff_view;
mod frames;
mod saga_run;
mod webapps;
mod pane_model;
mod handover_store;
mod connectors;
mod pane_provider;
mod team_onboarding;
pub mod cowork_threads;
pub mod shared_folder;
pub mod hidden_term;
pub mod side;
pub mod remote;
pub mod machines;
pub mod pane_watch;
pub mod agent_browser;
pub mod mailbox;
pub mod task_queue;
pub mod groups;
pub mod standing;
pub mod wall;
pub mod when;
pub mod board;
pub mod skillbank;
pub mod thread_tree;
pub mod thread_worktree;
pub mod voice;
pub mod work_changes;
pub mod team_media;
pub mod team_land;
pub mod team_archive;
pub mod team_member;
pub mod sleep;
pub mod team_executor;
pub mod team_thread;
pub mod team_conflict;
pub mod team_rootcause;
pub mod team_package;
pub mod team_xray;
pub mod team_apply;
pub mod team_decompose;
pub mod team_contract;
pub mod team_stubs;
pub mod team_review;
pub mod team_enforce;
pub mod team_skeleton;
pub mod team_rubric;
pub mod team_settings;
pub mod team_sound;
pub mod team_browse;
pub mod team_yaml;
pub mod team_branch;
pub mod team_template;
pub mod team_cell;
pub mod team_recurse;
pub mod team_test;
pub mod team_experiment;
pub mod team_export;
pub mod team_improver;
pub mod team_notes;
pub mod team_replay;
pub mod team_request;
pub mod team_recipe;
pub mod team_repo;
pub mod team_recall;
mod skillshare;
mod themes;
mod wordmark;
mod work_boxes;
pub mod events;
mod validate;
mod error;
pub mod routes;
mod tick;
pub mod req_meter;
pub mod guard;
pub mod agent;
pub mod recipes;
pub mod checkpoint;
pub mod front;
pub mod ids;
pub mod queue;
pub mod queue_http;
pub mod qa_evidence;
pub mod qa_api;
pub mod qa_plans;
pub mod gauge;
pub mod design_lint;
pub mod looks;
pub mod design_round;
pub mod lexicon;
pub mod roles;
pub mod agents_live;
pub mod chat_auto;
pub mod chat_pane;
pub mod agent_opencode;
pub mod local;
pub mod agent_local;
pub mod agent_codex;
pub mod agent_print;
pub mod agent_gemini;
pub mod upgrade;
pub mod team_digest;
pub mod team_proof;
pub mod team_git;
pub mod team_telegram;
// GOAL RUNS · SUBAGENTS · the headless CLI runner (2026-09-18): the chat-engine worker the NestJS
// BE ran in a thread, as plain modules here — no worker, no Node.
pub mod cli;
pub mod goals;
pub mod blueprint;
pub mod eta;
pub mod boxes;
mod ship;
mod work;
mod signin;
mod file_save;   // saving a file from the X-ray code editor (2026-10-11)
pub mod days;
pub mod doctor;
pub mod episodes_db;
pub mod ledger;
pub mod forge;
pub mod fuel;
pub mod gates;
pub mod laws;
pub mod mem;
pub mod machine;
pub mod openrouter;
pub mod lint_tools;
pub mod cli_usage;
pub mod run_edits;
pub mod run_preview;
pub mod lanes;
pub mod specialists;
pub mod docstore;
pub mod local_panes;
pub mod ports;
pub mod team_evolve;
pub mod team_race;
pub mod files_explorer;
pub mod custom_themes;
pub mod cowork_transcript;
pub mod engines;
pub mod swr;
/// EVERY CHILD PROCESS (2026-09-24) — the definition lives in the core crate, because `src/` spawns too:
/// that is where the Windows console windows the operator still sees are coming from.
/// EVERY CHILD PROCESS (2026-09-24). The definition lives in the core crate, because `src/` spawns 220 times
/// of its own — that is where the Windows console windows the operator still sees are coming from. This module
/// is the core's plus the two tokio constructors it cannot host.
pub mod proc_async;
pub use proc_async as proc;
pub mod estimate;
pub mod describe;
pub mod merges;
pub mod calibration;
pub mod jsonlint;
pub mod ladder;
pub mod launchers;
pub mod replay;
pub mod two_ways;
pub mod screen;
pub mod screen_kinds;
pub mod sampler;
pub mod survival;
pub mod cursor_usage;
pub mod confidence;
pub mod work_diff;
pub mod work_stats;
pub mod work_story;
pub mod audit;
pub mod agents_tab;
pub mod routines;
mod agents_http;
pub mod prompts;

use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use dispatch::AppState;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

/// Port of the BE's dbPathForRust(): same env precedence, so both engines open the same file.
pub(crate) fn db_path() -> String {
    if let Ok(p) = std::env::var("ANTILOKI_BE_DB_PATH") {
        if !p.is_empty() && p != ":memory:" {
            return p;
        }
    }
    if let Ok(p) = std::env::var("ANTILOKI_DB") {
        if !p.is_empty() {
            return p;
        }
    }
    let data_dir = std::env::var("ANTILOKI_DATA_DIR").unwrap_or_else(|_| {
        let root = std::env::var("ANTILOKI_SUBSTRATE_ROOT")
            .unwrap_or_else(|_| std::env::current_dir().unwrap().to_string_lossy().into_owned());
        format!("{root}/.antiloki")
    });
    format!("{data_dir}/db/state.sqlite")
}

/// THE DATA ROOT OF AN INSTALLED APP (2026-09-21, a tester on a fresh Windows PC: "unable to open database
/// file: D:\a\antiloki-core-rust\antiloki-core-rust\.antiloki\db\state.sqlite" — the CI runner's checkout,
/// baked in): the engine anchors everything on `ANTILOKI_SUBSTRATE_ROOT`, and without it falls back to the
/// crate's compile-time directory (`engine/roots.rs`, `engine/open-db.rs`) — right for a dev checkout,
/// meaningless on a customer's machine. So an unset root becomes the OS's per-user data folder:
/// `%LOCALAPPDATA%\antiloki` · `~/Library/Application Support/antiloki` · `$XDG_DATA_HOME/antiloki`
/// (else `~/.local/share/antiloki`). The dev loop keeps exporting the variable and is untouched.
/// Returns the root in force. Pure over the environment it is given — tested.
pub fn default_data_root(os: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<std::path::PathBuf> {
    let some = |k: &str| env(k).filter(|v| !v.trim().is_empty());
    let home = || some("HOME").or_else(|| some("USERPROFILE"));
    let base = match os {
        "windows" => some("LOCALAPPDATA").or_else(|| some("APPDATA")).or_else(|| home().map(|h| format!("{h}\\AppData\\Local"))),
        "macos" => home().map(|h| format!("{h}/Library/Application Support")),
        _ => some("XDG_DATA_HOME").or_else(|| home().map(|h| format!("{h}/.local/share"))),
    }?;
    Some(std::path::Path::new(&base).join("antiloki"))
}
/// Sets `ANTILOKI_SUBSTRATE_ROOT` to the per-user folder when nothing set it, creating `.antiloki/db` there.
/// Call it BEFORE any other thread runs (the desktop shell does, first thing in `main`; `run` does too, for
/// the standalone binary). Returns the root in force.
pub fn ensure_data_root() -> Option<String> {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") { if !v.trim().is_empty() { return Some(v); } }
    let root = default_data_root(std::env::consts::OS, &|k| std::env::var(k).ok())?;
    if let Err(e) = std::fs::create_dir_all(root.join(".antiloki").join("db")) { eprintln!("antiloki-serve: cannot create the data folder {} ({e})", root.display()); return None; }
    let s = root.to_string_lossy().into_owned();
    std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", &s);
    eprintln!("antiloki-serve: data root {s} (ANTILOKI_SUBSTRATE_ROOT was not set)");
    Some(s)
}

/// 2026-10-13: create `auth_dir()` if needed and, on Unix, chmod 0700 that directory only. Parents and `~/.codex` are left alone.
pub fn ensure_private_auth_dir() {
    let dir = antiloki_rs::engine_roots::auth_dir();
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
}

/// Bind and serve until the process ends. `port` overrides ANTILOKI_SERVE_PORT, which is what the
/// desktop shell uses to pin the FE's expected origin.
pub async fn run(port_override: Option<u16>) {
    ensure_data_root();
    ensure_private_auth_dir();
    // A PIN ON A SEAT THE OPERATOR SWITCHED OFF BECOMES `auto` (2026-10-01, "Engine config also"; narrowed by 8bfe0a8: only `seats_off` and Zen off, never "not installed" or "signed out" - this process's PATH/HOME
    // may be thinner than the shell's, and `role_binding` already swaps a seat that is merely not found at call time) - once, before anything reads the roles. The old file is kept as a `.bak-<date>`
    // (`-2`, `-3`... for a second run that day) and the new one is written atomically.
    cli::migrate_models_once();
    // RE-SCORE ONCE AT STARTUP (2026-09-25). Splitting controlled from observed evidence stops new
    // contamination, but a weight already on disk keeps whatever the old shared-baseline rules gave it until
    // something writes to its cell again — which for a rarely-used pairing could be never. Hundreds of rows,
    // idempotent, and it means a build carrying a fix to the scoring actually applies it to what is there.
    match crate::merges::rescore_all() {
        Ok(n) if n > 0 => eprintln!("antiloki-serve: re-scored {n} merge cell(s) under the current evidence rules"),
        Err(e) => eprintln!("antiloki-serve: could not re-score the merge record ({e}) — suggestions may be stale"),
        _ => {}
    }
    // …and drop the rungs whose whole record is refusals (ladder::is_refusal): the Zen rows stood at 0 ok out of
    // 5 without a single call ever being made, which reads back as a verdict on the model and is not one.
    crate::ladder::forget_refusals();
    let path = db_path();
    // THE UPGRADE GUARD (2026-09-21): the first open by a newer build stamps, backs up and migrates the db
    match upgrade::guard(&path, env!("CARGO_PKG_VERSION")) {
        Ok(Some(o)) if o.backup.is_some() => println!("antiloki-serve: v{} → v{} — the database was copied to {} ({} migration(s))", upgrade::version_label(o.from), upgrade::version_label(o.to), o.backup.as_deref().unwrap_or(""), o.migrated),
        Ok(_) => {}
        Err(e) => eprintln!("antiloki-serve: the upgrade guard failed ({e}) — booting on the database as it is"),
    }
    let conn = antiloki_rs::engine_open_db::openDb(&json!(path), &json!({ "readOnly": false }))
        .unwrap_or_else(|e| panic!("antiloki-serve: openDb failed for {path}: {e}"));

    // Same WAL check the napi boundary makes, and for the same reason: without WAL a concurrent
    // reader turns into intermittent SQLITE_BUSY instead of a clear failure here.
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .expect("antiloki-serve: could not read journal_mode");
    assert_eq!(mode.to_lowercase(), "wal", "antiloki-serve: journal_mode is '{mode}', not 'wal'");

    let hub = events::EventsHub::new();
    // the dispatch pool: ANTILOKI_SERVE_DB_POOL more connections (default 4) to the same WAL db
    let pool_n: usize = std::env::var("ANTILOKI_SERVE_DB_POOL").ok().and_then(|v| v.parse().ok()).unwrap_or(4);
    let pool: Vec<Arc<Mutex<rusqlite::Connection>>> = (0..pool_n).filter_map(|i| match antiloki_rs::engine_open_db::openDb(&json!(path), &json!({ "readOnly": false })) {
        Ok(c) => Some(Arc::new(Mutex::new(c))),
        Err(e) => { eprintln!("antiloki-serve: pool connection {i} not opened — {e}"); None }
    }).collect();
    let state = Arc::new(AppState {
        conn: Arc::new(Mutex::new(conn)),
        pool,
        db_path: path.clone(),
        events: hub.clone(),
        waker: std::sync::OnceLock::new(),
    });

    // FIX-PHANTOM-RUNNING-TICK-ON-RESTART: a tick that was in flight when the process died stays
    // outcome='running' forever and makes the driver look busy. Reaped once at boot, exactly
    // where the BE does it (ensureReady), never per tick.
    {
        let guard = state.conn.lock().unwrap();
        let reaped = guard
            .execute(
                &format!(
                    "UPDATE brain.driver_ticks SET outcome='failed', ended_at={},
                       error_message=COALESCE(error_message,'reaped: BE restarted (tick orphaned mid-flight)')
                     WHERE outcome='running' AND ended_at IS NULL",
                    antiloki_rs::clock::now_sql()
                ),
                [],
            )
            .unwrap_or(0);
        if reaped > 0 {
            println!("antiloki-serve: reaped {reaped} orphaned tick(s)");
        }
    }

    // the dev servers a previous serve left running are ended before this one starts its own (preview.rs)
    let swept = preview::sweep_orphans();
    if swept > 0 { println!("antiloki-serve: ended {swept} dev server(s) a previous serve left running"); }
    let app = build_router(state.clone());

    let port: u16 = port_override.unwrap_or_else(|| {
        std::env::var("ANTILOKI_SERVE_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(41998)
    });
    let addr = format!("127.0.0.1:{port}");
    // ai-call's `cli` providers dispatch to THIS process (the subscription engines, 2026-09-19)
    if std::env::var("ANTILOKI_CLI_LLM_URL").ok().filter(|v| !v.trim().is_empty()).is_none() {
        unsafe { std::env::set_var("ANTILOKI_CLI_LLM_URL", format!("http://{addr}/api/v1/cli-llm/chat/completions")); }
    }
    // THE PORT THIS PROCESS ACTUALLY BOUND, told to everything that has to call back into it (2026-09-25).
    // `ANTILOKI_SERVE_PORT` (default 41998, above) is what the serve listens on; `ANTILOKI_BE_PORT` (default
    // 41931) is what the pane context bake writes into every engine's MCP config, and what the sidecar reads.
    // Run standalone — `cargo run -p antiloki-serve` — those two defaults disagree, so a provisioned pane was
    // handed ANTILOKI_BE_URL=http://127.0.0.1:41931 with nothing listening there: the agent's own antiloki
    // tools could not reach the substrate that spawned it. Publishing the real port here fixes every consumer
    // at once instead of teaching each one a second default. An explicit value is never overridden.
    for key in ["ANTILOKI_BE_PORT", "ANTILOKI_BE_URL"] {
        if std::env::var(key).ok().filter(|v| !v.trim().is_empty()).is_some() { continue }
        let v = if key == "ANTILOKI_BE_PORT" { port.to_string() } else { format!("http://{addr}") };
        unsafe { std::env::set_var(key, v) };
    }
    engines::warm();
    custom::forge_tokens_warm();
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("antiloki-serve: cannot bind {addr}: {e}"));

    println!("antiloki-serve listening on http://{addr} ({} routes, db={path})", routes::ROUTES.len());
    // THE FIRST SAGAS LIST AFTER A START IS THE SLOW ONE (2026-11-18: 15 s on a real workspace, the git diff of every agent's box): ask for it once ourselves, so the cache is warm when the first screen asks
    { let warm = format!("http://127.0.0.1:{port}/api/v1/sagas"); tokio::spawn(async move { tokio::time::sleep(std::time::Duration::from_millis(800)).await; if let Ok(c) = reqwest::Client::builder().timeout(std::time::Duration::from_secs(180)).build() { let _ = c.get(warm).send().await; } }); }
    remote::init(std::path::Path::new(&path).parent().map(|p| p.join("remote")).unwrap_or_else(|| std::path::PathBuf::from("remote")), port, app.clone());
    machines::restore();
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.expect("antiloki-serve: server error");
}

/// The whole HTTP surface as a plain `Router`, with no listener and no process-global state.
///
/// This exists so the route tests can drive every endpoint through `tower::ServiceExt::oneshot`
/// — no port to bind, no server to race, no BE to compare against. Until now the routes were only
/// verified by A/B against the running NestJS, and that oracle disappears at cutover.
pub fn build_router(state: Arc<AppState>) -> Router {
    // The SSE hub lives on the state; run() no longer hands it in separately.
    state.ensure_pane_base();
    let hub = state.events.clone();
    let started = std::time::Instant::now();
    let ptys = pty::PtyRegistry::new();
    team_graph_run::set_pty(ptys.clone());   // the self-audit types into the Leader's terminal
    // RFC 0007 M1: with the sidecar host, the terminals outlived the last serve: attach to them again (route tests have no runtime to spawn on)
    if pty_remote::enabled() {
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let (st, r) = (state.clone(), ptys.clone());
            rt.spawn(async move { let n = pty::reattach_host_terminals(st, r).await; if n > 0 { println!("antiloki-serve: reattached {n} terminal(s) the host kept alive through the restart"); } });
        }
    }
    // TEAMS: the person talks to a team's planner; when it answers with a plan, the team carries it out (team_run::watch)
    if let Ok(rt) = tokio::runtime::Handle::try_current() { let (st, r) = (state.clone(), ptys.clone()); rt.spawn(async move { team_run::watch(st, r).await; }); }
    // TEAM SCHEDULES (RFC 0010 §4.9): the serve starts a team on its cron when the person has consented, window or no window
    if let Ok(rt) = tokio::runtime::Handle::try_current() { let st = state.clone(); rt.spawn(async move { team_graph_run::scheduler(st).await; }); }
    // TEAM GRAPH RUNS that were going when the serve stopped are taken up from their last checkpoint (RFC 0008 §4.6)
    if let Ok(rt) = tokio::runtime::Handle::try_current() { let st = state.clone(); rt.spawn(async move { let root = chat_pane::workspace_root_of(&st); if !root.is_empty() { team_graph_run::resume_all(&root).await; } }); }
    let agents = agent::AgentRegistry::new();
    let previews = preview::PreviewRegistry::new();
    let runner = goals::real_runner();
    let goal_runs = goals::Goals::new(runner.clone(), None, std::env::var("ANTILOKI_GOALRUN_CONCURRENCY").ok().and_then(|v| v.parse().ok()).unwrap_or(3), 2000);
    agents.set_goals(goal_runs.clone()); // a chat turn's tokens price the inline route it answered
    agents.set_state(state.clone());     // …and a finished turn reads its pane's gate toggles fresh (gates.rs)
    crate::mem::watch_headless();        // background runs pause when the person's machine is short of RAM or CPU, and continue by themselves
    agents.watch_cpu();
    if let Ok(rt) = tokio::runtime::Handle::try_current() { let (st, ag) = (state.clone(), agents.clone()); let _in = rt.enter(); pane_watch::watch(st, ag); }
    if let Ok(rt) = tokio::runtime::Handle::try_current() { let (st, r) = (state.clone(), ptys.clone()); let _in = rt.enter(); mailbox::watch(st, r); }   // messages between agents wait until their receiver is idle   // pauses and continues agents on the machine's RAM and VRAM
    if let Ok(rt) = tokio::runtime::Handle::try_current() { let _in = rt.enter(); standing::start(); }   // routines the person turned on (RFC 0025 §7)
    routines::start(state.clone(), agents.clone()); // the routines' ticker: every minute, every due brief goes to its agent
    // THE SPECIALISTS for goal runs (2026-09-19): the box's pane (by worktree) says which lenses are
    // available; a review is one constitution.agent.run on a pool connection, off the async runtime
    {
        let st = state.clone();
        let list: goals::SpecialistListFn = Arc::new(move |cwd: String| {
            let st = st.clone();
            Box::pin(async move {
                let conn = st.pick();
                tokio::task::spawn_blocking(move || {
                    let c = conn.lock().unwrap_or_else(|p| p.into_inner());
                    let pane = pane_by_worktree(&c, &cwd);
                    antiloki_rs::cockpit_cockpit_pane_context_bake::pane_specialists(&c, &pane).into_iter()
                        .map(|a| (a.get("id").and_then(Value::as_str).unwrap_or("").to_string(), a.get("title").and_then(Value::as_str).unwrap_or("").to_string(), a.get("description").and_then(Value::as_str).unwrap_or("").to_string()))
                        .filter(|(id, _, _)| !id.is_empty()).collect::<Vec<_>>()
                }).await.unwrap_or_default()
            })
        });
        let st = state.clone();
        let review: goals::SpecialistReviewFn = Arc::new(move |cwd: String, id: String, path: String| {
            let st = st.clone();
            Box::pin(async move {
                let conn = st.pick();
                tokio::task::spawn_blocking(move || {
                    let c = conn.lock().unwrap_or_else(|p| p.into_inner());
                    let pane = pane_by_worktree(&c, &cwd);
                    let allowed = antiloki_rs::cockpit_cockpit_pane_context_bake::pane_specialists(&c, &pane).iter().any(|a| a.get("id").and_then(Value::as_str) == Some(id.as_str()));
                    if !allowed { return Err(format!("{id} is not available to this box (⚙ › subagents)")); }
                    let r = antiloki_rs::constitution_constitution_agent_run::constitutionAgentRun(&json!({ "agent_id": id, "path": path, "research": false, "sources": [], "tier": "mid", "swarm_id": format!("goal:{}", cwd.rsplit('/').next().unwrap_or("box")) }), &c).map_err(|e| e.to_string())?;
                    if r.get("ok") != Some(&Value::Bool(true)) { return Err(r.get("error").and_then(Value::as_str).unwrap_or("the call failed").to_string()); }
                    Ok(antiloki_rs::cockpit_cockpit_pane_context_bake::specialist_review_md(&r))
                }).await.unwrap_or_else(|e| Err(e.to_string()))
            })
        });
        goal_runs.with_specialists(list, review);
        let restored = goal_runs.with_history(goals::Goals::default_history_path());
        if restored > 0 { println!("antiloki-serve: {restored} goal run(s) restored from the last serve"); }
    }
    // STALE CLAIMS (queue item 4, 2026-09-20): a pane whose box is gone from disk still held its scope
    // and idled in the lists — closed at boot, once, on the active workspace's db
    {
        // THE SWEEP DECIDES, THE ACTION WRITES (audit 2026-09-28). `cockpit_panes` belongs to
        // `system.cockpit.pane.update`; the raw UPDATE that used to be here skipped everything that action does
        // besides setting the column, which is the same deviation that produced the read-only bug. The read stays
        // on the blocking pool and the lock is released before any dispatch — `engine_call` re-enters it.
        let (conn, st) = (state.pick(), state.clone());
        tokio::spawn(async move {
            let rows = tokio::task::spawn_blocking(move || {
                let c = conn.lock().unwrap_or_else(|p| p.into_inner());
                antiloki_rs::db_json::query_rows(&c, "SELECT id, worktree_path FROM cockpit_panes WHERE worktree_path IS NOT NULL AND worktree_path != '' AND status != 'closed'", &[]).unwrap_or(Value::Null)
            }).await.unwrap_or(Value::Null);
            let mut n = 0;
            for r in rows.as_array().cloned().unwrap_or_default() {
                let wt = r.get("worktree_path").and_then(Value::as_str).unwrap_or("");
                if wt.is_empty() || std::path::Path::new(wt).is_dir() { continue }
                let id = r.get("id").cloned().unwrap_or(Value::Null);
                if crate::dispatch::engine_call(&st, "system.cockpit.pane.update",
                        json!({ "pane_id": id, "status": "closed", "scope": [] }), crate::chat_pane::ctx()).await.is_ok() { n += 1 }
            }
            if n > 0 { println!("antiloki-serve: {n} pane(s) whose box is gone were closed (their scope claims released)"); }
        });
    }
    // THE ACTIVE WORKSPACE, KNOWN FROM BOOT (2026-09-25). `audit::set_workspace_root` was called from ONE route —
    // the Agents roster — so until the operator happened to open that tab the serve did not know which repo it was
    // serving: every call logged before it went down with no `stack`, and the benchmark screen's "this repo is
    // <technology>" line stayed blank on a fresh serve. The root is in the db the whole time; read it once here.
    {
        let conn = state.pick();
        tokio::task::spawn_blocking(move || {
            let c = conn.lock().unwrap_or_else(|p| p.into_inner());
            let root = antiloki_rs::engine_workspace_root::getWorkspaceRoot(&c).as_str().unwrap_or("").to_string();
            if !root.is_empty() { crate::audit::set_workspace_root(&root); }
        });
    }
    let subagents = goals::Subagents::new(runner.clone(), std::env::var("ANTILOKI_SUBAGENT_CONCURRENCY").ok().and_then(|v| v.parse().ok()).unwrap_or(4));
    // THE PROMPT CATALOG (prompts.rs): every serve/FE prompt is a brain.prompt_templates row
    prompts::attach(state.conn.clone());
    let registered = prompts::ensure();
    if !registered.is_empty() { println!("antiloki-serve: prompt catalog registered {}", registered.join(", ")); }
    // THE QUEUES (queue.rs): each agent's list of what was asked, its persistence, the loop that hands it its next item, and the router's binding for each
    let queues = queue::global();
    { let n = queues.with_history(goals::Goals::default_history_path().with_file_name("queues.json")); if n > 0 { println!("antiloki-serve: {n} queue item(s) restored"); } }
    let (q_start, q_bind) = queue_http::runners(state.clone(), goal_runs.clone(), agents.clone());
    queue_http::spawn_loop(queues.clone(), goal_runs.clone(), q_start.clone(), q_bind.clone());
    // AN ORPHANED REBUILD MUST NOT HOLD THE WORKSPACE (operator 2026-09-29: a rebuild "working for 70 minutes" on a workspace the serve had crashed in blocked switching away from
    // it). A job that says running / pause_requested and has not written a word for 20 minutes has no runner any more - a crash, a restart, a killed child. The engine's own
    // `mark-error` transition says the same thing for one job; this applies it to every such job, once a minute, on whichever workspace is bound now.
    { let st = state.clone();
      tokio::spawn(async move { loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let conn = st.pick();
        let n = tokio::task::spawn_blocking(move || {
            let c = conn.lock().unwrap_or_else(|p| p.into_inner());
            c.execute("UPDATE code_onboard_jobs SET status = 'error', error = '[' || COALESCE(phase, '?') || '] the rebuild stopped reporting for more than 20 minutes - its runner is gone (a crash or a restart); resume it from the last checkpoint', updated_at = datetime('now') WHERE status IN ('running', 'pause_requested') AND updated_at < datetime('now', '-20 minutes')", []).unwrap_or(0)
        }).await.unwrap_or(0);
        if n > 0 { eprintln!("antiloki-serve: {n} rebuild job(s) that stopped reporting were marked as errors (their runner is gone)"); }
      } }); }
    // IDLE AGENTS SLEEP (operator 2026-11-16): every 20 s read each terminal; one quiet for the window saves its saga and closes (sleep.rs).
    { let (st, gr) = (state.clone(), goal_runs.clone()); tokio::spawn(async move { tokio::time::sleep(std::time::Duration::from_secs(30)).await; loop { sleep::tick(&st, &gr).await; tokio::time::sleep(std::time::Duration::from_secs(20)).await; } }); }
    let mut app = Router::new()
        // THE PROMPT CATALOG: what the FE previews, and a render for the prompt window
        .route("/api/v1/prompts/catalog", get(|| async { axum::Json(json!({ "ok": true, "prompts": prompts::list() })) }))
        .route("/api/v1/prompts/render", post(|body: Bytes| async move {
            let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let base = b.get("base").and_then(Value::as_str).unwrap_or("");
            let vars = b.get("vars").and_then(Value::as_object).cloned().unwrap_or_default();
            match prompts::resolve(base).map(|r| r.template_md).or_else(|| prompts::catalog_entry(base).map(|t| t.template_md.to_string())) {
                Some(t) => axum::response::IntoResponse::into_response(axum::Json(json!({ "ok": true, "base": base, "text": prompts::render(&t, &vars) }))),
                None => axum::response::IntoResponse::into_response((axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({ "ok": false, "error": { "message": format!("unknown prompt template '{base}'") } })))),
            }
        }))
        // GOAL RUNS + SUBAGENTS + the CLI audit (agents_http.rs)
        .route("/api/v1/cockpit/goals", get({ let (st, g) = (state.clone(), goal_runs.clone()); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_http::goals_get(st.clone(), g, q) }))
        .route("/api/v1/cockpit/goals/ledger", get({ let (st, g) = (state.clone(), goal_runs.clone()); move || agents_http::goals_ledger(st.clone(), g.clone()) }))
        // THE STORY OF ONE RUN (operator 2026-09-24) — the same ledger, told in English
        .route("/api/v1/cockpit/goals/live", get({ let g = goal_runs.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_http::goal_live(g, q) }))
        .route("/api/v1/cockpit/goals/evidence", get({ let g = goal_runs.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_http::goal_evidence(g, q) }))
        .route("/api/v1/cockpit/goals/diff", get({ let g = goal_runs.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_http::goal_diff(g, q) }))
        .route("/api/v1/cockpit/goals/story", get({ let g = goal_runs.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_http::goal_story(g, q) }))
        // THE QA'S PLAN, KEPT AS A REGRESSION TEST (item 8d, 2026-10-01): list what a workspace has saved, keep the plan a run ran, replay a saved one now
        .route("/api/v1/cockpit/qa/plans", get({ let (st, g) = (state.clone(), goal_runs.clone()); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| qa_plans::plans_get(st.clone(), g.clone(), q) })
            .post({ let (st, g) = (state.clone(), goal_runs.clone()); move |body: Bytes| qa_plans::plans_post(st.clone(), g.clone(), body) }))
        .route("/api/v1/cockpit/qa/replay", post({ let (st, g) = (state.clone(), goal_runs.clone()); move |body: Bytes| qa_plans::replay_post(st.clone(), g.clone(), body) }))
        .route("/api/v1/cockpit/goals/{op}", post({ let (st, g, ag) = (state.clone(), goal_runs.clone(), agents.clone()); move |axum::extract::Path(op): axum::extract::Path<String>, body: Bytes| agents_http::goals_op(st, g, ag, op, body) }))
        .route("/api/v1/cockpit/checkpoints/{op}", post({ let st = state.clone(); move |axum::extract::Path(op): axum::extract::Path<String>, body: Bytes| agents_http::checkpoints_op(st, op, body) }))
        .route("/api/v1/cockpit/pane/pr", post({ let (st, g) = (state.clone(), goal_runs.clone()); move |body: Bytes| agents_http::pane_pr(st, g, body) }))
        .route("/api/v1/cockpit/subagents", get({ let sb = subagents.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_http::subagents_get(sb, q) }))
        .route("/api/v1/cockpit/subagents/{op}", post({ let (sb, r) = (subagents.clone(), runner.clone()); move |axum::extract::Path(op): axum::extract::Path<String>, body: Bytes| agents_http::subagents_op(sb, r, op, body) }))
        .route("/api/v1/cli-llm/traces", get(|axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_http::traces(q)))
        .route("/api/v1/cli-llm/chat/completions", post(|body: Bytes| agents_http::cli_completions(body)))
        // THE FILE DESCRIBER (2026-10-01): prose for the files that matter, priced before it is bought (describe.rs)
        // SAVE FROM THE CODE EDITOR (operator 2026-10-11): the write twin of codebase/file/read, stricter than it - see file_save.rs
        .route("/api/v1/codebase/file/save", post({ let st = state.clone(); move |body: Bytes| file_save::handle(st.clone(), body) }))
        .route("/api/v1/codebase/describe/plan", post({ let st = state.clone(); move |body: Bytes| describe::plan(st.clone(), body) }))
        .route("/api/v1/codebase/describe/batch", post({ let st = state.clone(); move |body: Bytes| describe::batch(st.clone(), body) }))
        .route("/api/v1/codebase/describe/status", get({ let st = state.clone(); move || describe::status(st.clone()) }))
        .route("/api/v1/codebase/describe/cancel", post(|| describe::cancel()))
        .route("/healthz", get(move || custom::healthz(started)))
        .route("/healthz/engine", get(custom::healthz_engine))
        // The one SSE route. Registered by hand rather than generated: it is a stream, not a
        // request/response dispatch, so it shares nothing with the passthrough handler.
        // Hand-written one-offs: a static registry, fire-and-forget telemetry, a detached
        // dispatch and the generic dispatcher. See custom.rs for why these are not generated.
        .route("/api/v1/artefacts/audio/{file}", get({
            let st = state.clone();
            move |axum::extract::Path(file): axum::extract::Path<String>, headers: HeaderMap| files::artefact_audio(st, headers, file)
        }))
        .route("/api/v1/narration/audio/{renderId}", get({
            let st = state.clone();
            move |axum::extract::Path(rid): axum::extract::Path<String>, headers: HeaderMap| files::narration_audio(st, headers, rid)
        }))
        .route("/api/v1/notebook/audio/{id}", get({
            let st = state.clone();
            move |axum::extract::Path(id): axum::extract::Path<String>| files::notebook_audio(st, id)
        }))
        .route("/api/v1/workspaces/pick-folder", post(preview::pick_folder))
        .route("/api/v1/workspaces/browse", get(preview::browse_folder))
        .route("/api/v1/workspaces/mkdir", post(preview::make_folder))
        .route("/api/v1/pr/preview/start", post({
            let (st, r) = (state.clone(), previews.clone());
            move |headers: HeaderMap, body: Bytes| preview::preview_start(st, r, headers, body)
        }))
        .route("/api/v1/pr/preview/status", get({
            let r = previews.clone();
            move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| preview::preview_status(r, q)
        }))
        .route("/api/v1/pr/preview/stop", post({
            let r = previews.clone();
            move |body: Bytes| preview::preview_stop(r, body)
        }))
        .route("/api/v1/pr/worktree/open", post({
            let st = state.clone();
            move |body: Bytes| preview::worktree_open(st, body)
        }))
        .route("/api/v1/upload/path", post({ let st = state.clone(); move |headers: axum::http::HeaderMap, body: axum::body::Bytes| custom::upload_path(st.clone(), headers, body) }))
        .route("/api/v1/upload", post({
            let st = state.clone();
            move |headers: HeaderMap, mp: axum::extract::Multipart| custom::upload(st, headers, mp)
        }))
        .route("/api/v1/ai-proxy/pane/{id}/models", get(proxy::models))
        .route("/api/v1/ai-proxy/pane/{id}/chat/completions", post({
            let st = state.clone();
            move |axum::extract::Path(id): axum::extract::Path<String>, body: Bytes| proxy::chat_completions(st, id, body)
        }))
        .route("/api/v1/codebase/onboard/start", post({
            let st = state.clone();
            move |headers: HeaderMap, body: Bytes| custom::codebase_onboard_start(st, headers, body)
        }))
        .route("/api/v1/skills", get(catalog::skills))
        .route("/api/v1/cockpit/engines", get(custom::cockpit_engines))
        .route("/api/v1/settings/engine-models", get(custom::engine_models).post(custom::engine_models_set))
        // THE TWO PROVIDER SWITCHES, both off by default — Zen is metered credit, Chinese models are optional
        .route("/api/v1/settings/providers", get(custom::providers).post(custom::providers_set))
        .route("/api/v1/settings/forge-tokens", get(custom::forge_tokens).post(custom::forge_tokens_set))
        .route("/api/v1/settings/engine-models/role", post(custom::engine_models_role))
        .route("/api/v1/settings/engine-models/test", post(custom::engine_models_test))
        // THE SIGN-IN FLOW (operator 2026-10-11): opens a terminal running the seat's own login; see signin.rs
        .route("/api/v1/onboarding/signin", post(|body: Bytes| signin::start(body)))
        // AGENT PROVIDERS THE PERSON CAN ADD (RFC 0023): the list, and the person's Confirm / Reject
        .route("/api/v1/providers", get(providers::list_http).post(providers::act_http))
        // CONNECT ANTILOKI TO A PROVIDER'S CLI (2026-11-18): which CLIs know the antiloki MCP server, and one click to connect or disconnect each
        .route("/api/v1/onboarding/mcp", get(|| async { Json(mcp_install::status().await) }).post(|body: Bytes| async move {
            let b: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
            Json(mcp_install::apply(b["provider"].as_str().unwrap_or(""), b["action"].as_str().unwrap_or("install")).await)
        }))
        .route("/api/v1/onboarding/seat-check", post({
            let st = state.clone();
            move |body: Bytes| custom::seat_check(st.clone(), body)
        }))
        .route("/api/v1/settings/engine-models/job", post(custom::engine_models_job))
        .route("/api/v1/settings/roles", get(|axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| async move { roles::http_roles(q).await }))
        // THE DESIGN ROUND'S SWITCH (PLAN-THE-DESIGN S5): { design: auto | on | off } - auto = on for frontend and design asks only
        .route("/api/v1/settings/qa", get(design_round::http_get).post(design_round::http_set))
        .route("/api/v1/settings/merges", get(merges::http_table))
        .route("/api/v1/settings/merges/record", post(merges::http_record))
        // THE DISCONTINUED ARMS — the Zen merges, kept for audit, shown to no one and used for no route
        .route("/api/v1/settings/merges/legacy", get(merges::http_legacy))
        // A WEIGHT IS DERIVED, NOT MEASURED — recompute them all after pricing or scoring changes
        .route("/api/v1/settings/merges/reweigh", post(merges::http_reweigh))
        // WHAT TO MEASURE NEXT (PLAN-THE-LEAN-YEARS II.3): the bandit over cells, and what it refuses to spend
        .route("/api/v1/settings/sampler", get(sampler::http_plan))
        // RUN THIS ONE RAW (II.6): should the button be drawn and what does it cost — then the one run, in its own box
        // WHAT KINDS OF ASK ACTUALLY ARRIVE (II.5) — the classifier's own verdicts, counted from real use
        // DOES "SURE" MEAN "RIGHT" — the router's own confidence, checked against what happened
        .route("/api/v1/settings/calibration", get(calibration::http_report))
        // HOW OFTEN A STRUCTURED REPLY IS MALFORMED — measured before anything is built to repair it
        .route("/api/v1/settings/json-health", get(jsonlint::http_report))
        // WHAT PLAN REPLAY HAS ACTUALLY SAVED — hits, misses, and why it missed
        .route("/api/v1/settings/replay", get(replay::http_report))
        // THE TWO WAYS TO DO AN ASK, PRICED — thin (one call) vs deep (planned · verified · gated)
        .route("/api/v1/settings/two-ways", get(two_ways::http_ways))
        // THE FALLBACK LADDER — which rung answered, and whether the record has moved the order
        .route("/api/v1/settings/ladder", get(ladder::http_report))
        // EVERY OTHER CODING CLI, AS A PANE — opened, never routed or priced
        .route("/api/v1/launchers", get(launchers::http))
        .route("/api/v1/settings/taxonomy", get(|| async { match audit::taxonomy() { Ok(v) => axum::Json(v), Err(e) => axum::Json(json!({ "ok": false, "error": e })) } }))
        .route("/api/v1/settings/audit", get(audit::http_report))
        .route("/api/v1/cockpit/pty/status", get({
            let (st, r) = (state.clone(), ptys.clone());
            move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| pty::pty_status(st.clone(), r.clone(), q)
        }))
        .route("/api/v1/cockpit/pty/stream", get({
            let st = state.clone(); let r = ptys.clone();
            move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| pty::pty_stream(st.clone(), r.clone(), q)
        }))
        .route("/api/v1/cockpit/pty/streams", get({
            let r = ptys.clone();
            move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| pty::pty_streams(r.clone(), q)
        }))
        // the stream census (2026-09-21): how many browser-wide subscriptions and events listeners are open —
        // the number the SharedWorker keeps at one per browser, not one per tab
        .route("/api/v1/cockpit/streams/census", get({ let r = ptys.clone(); let st = state.clone(); move || { let (subs, panes) = r.stream_census(); let events = st.events.listeners(); async move { axum::Json(serde_json::json!({ "pty_subs": subs, "pty_panes": panes, "events_listeners": events })) } } }))
        .route("/api/v1/cockpit/pty/streams/sub", post({
            let st = state.clone(); let r = ptys.clone(); let a = agents.clone();
            move |body: axum::body::Bytes| pty::pty_streams_sub(st.clone(), r.clone(), a.clone(), body)
        }))
        // THE CHAT AGENT (2026-09-20): Claude Code's programmatic session per pane — no PTY
        .route("/api/v1/cockpit/agent/start", post({ let st = state.clone(); let a = agents.clone(); move |body: axum::body::Bytes| agent::agent_start(st.clone(), a.clone(), body) }))
        .route("/api/v1/cockpit/agent/send", post({ let st = state.clone(); let a = agents.clone(); move |body: axum::body::Bytes| agent::agent_send(st.clone(), a.clone(), body) }))
        .route("/api/v1/cockpit/queue", get({ let q = queues.clone(); move |axum::extract::Query(p): axum::extract::Query<std::collections::HashMap<String, String>>| { let q = q.clone(); async move { queue_http::list(&q, p.get("pane_id").and_then(|x| x.parse().ok()).unwrap_or(0)) } } }))
        .route("/api/v1/cockpit/queue/{op}", post({ let (st, g, ag, q, s0, b0) = (state.clone(), goal_runs.clone(), agents.clone(), queues.clone(), q_start.clone(), q_bind.clone()); move |axum::extract::Path(op): axum::extract::Path<String>, body: axum::body::Bytes| queue_http::op(st.clone(), g.clone(), ag.clone(), q.clone(), s0.clone(), b0.clone(), op, body) }))
        .route("/api/v1/cockpit/agent/front", post({ let (st, a, g) = (state.clone(), agents.clone(), goal_runs.clone()); move |body: axum::body::Bytes| front::front_op(st.clone(), a.clone(), g.clone(), body) }))
        .route("/api/v1/cockpit/agent/context", post({ let st = state.clone(); move |body: axum::body::Bytes| agent::agent_context(st.clone(), body) }))
        // AGENT RECIPES (2026-09-20): the Studio's custom agents — provider · theme · icon · scope · specialists · skills
        .route("/api/v1/cockpit/agents/recipes", get({ let st = state.clone(); move || recipes::list(st.clone()) }))
        .route("/api/v1/cockpit/agents/recipes", post({ let st = state.clone(); move |body: axum::body::Bytes| recipes::save(st.clone(), body) }))
        .route("/api/v1/cockpit/agents/recipes/delete", post({ let st = state.clone(); move |body: axum::body::Bytes| recipes::delete(st.clone(), body) }))
        .route("/api/v1/cockpit/agents/recipes/archive", post({ let st = state.clone(); move |body: axum::body::Bytes| recipes::archive(st.clone(), body) }))
        .route("/api/v1/cockpit/agent/transcript", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_live::transcript(st.clone(), q) }))
        .route("/api/v1/chat/auto_context", post({ let (st, g) = (state.clone(), goal_runs.clone()); move |body: Bytes| chat_auto::auto_context(st.clone(), g.clone(), body) }))
        // CHATS AS PANES (2026-09-21, chat_pane.rs): read-only agent sessions with episodes and an engine switch
        .route("/api/v1/cockpit/chats", get({ let (st, a) = (state.clone(), agents.clone()); move || chat_pane::list(st.clone(), a.clone()) }))
        // THE OPERATOR'S DAYS (2026-09-24): the same episodes and runs, folded up one level (days.rs)
        .route("/api/v1/cockpit/timeline", get({ let (st, a, g) = (state.clone(), agents.clone(), goal_runs.clone()); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_http::timeline_get(st.clone(), a.clone(), g.clone(), q) }))
        .route("/api/v1/cockpit/fuel", get({ let (st, g) = (state.clone(), goal_runs.clone()); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_http::fuel_get(st.clone(), g.clone(), q) }))
        // WHAT THIS MACHINE CAN RUN (operator 2026-09-24): the agent wizard asks before it offers, instead of
        // listing merges that cannot start here.
        .route("/api/v1/engines", get(|| engines::http()))
        // THE MACHINE WATCHER (2026-11-21): CPU, RAM, VRAM and the heaviest processes, for the bottom bar's watcher
        .route("/api/v1/machine", get(|| machine::http()))
        .route("/api/v1/machine/unload", post(machine::unload_http))
        // THE LOCAL SEAT (RFC 0001 phase E): off unless ANTILOKI_LOCAL_WORKER=1; status says what is usable and why not, fill runs one scoped fill on a local model
        .route("/api/v1/local/status", get(|| local::http_status()))
        .route("/api/v1/local/fill", post(local::http_fill))
        // WHAT ONE PANE HAS DONE IN ITS OWN BOX (operator 2026-09-25): its uncommitted diff, and its commits.
        .route("/api/v1/cockpit/pane/antiqa", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_http::pane_antiqa(st.clone(), q) }))
        .route("/api/v1/cockpit/pane/work", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_http::pane_work(st.clone(), q) }))
        .route("/api/v1/cockpit/pane/handoff-tax", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_http::pane_handoff_tax(st.clone(), q) }))
        .route("/api/v1/cockpit/days", get({ let (st, a, g) = (state.clone(), agents.clone(), goal_runs.clone()); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_http::days_http(st.clone(), a.clone(), g.clone(), q) }))
        .route("/api/v1/cockpit/agents/roster", get({ let (st, a, g) = (state.clone(), agents.clone(), goal_runs.clone()); move || agents_tab::roster(st.clone(), a.clone(), g.clone()) }))
        // ONE EPISODE'S CONVERSATION — the log between its seq and the next one's (operator 2026-09-27)
        // FORK THIS AGENT — same config, new worktree, its own episodes (operator 2026-09-27)
        .route("/api/v1/cockpit/pane/fork/status", get({ move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_tab::fork_status(q) }))
        .route("/api/v1/cockpit/pane/fork", post({ let st = state.clone(); move |body: Bytes| agents_tab::pane_fork(st.clone(), body) }))
        .route("/api/v1/cockpit/agents/episode/turns", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agents_tab::episode_turns(st.clone(), q) }))
        .route("/api/v1/cockpit/agents/episode/open", post({ let st = state.clone(); move |body: Bytes| agents_tab::episode_open(st.clone(), body) }))
        .route("/api/v1/cockpit/agents/episode/close", post({ let st = state.clone(); move |body: Bytes| agents_tab::episode_close(st.clone(), body) }))
        .route("/api/v1/cockpit/agents/open", post({ let st = state.clone(); move |body: Bytes| agents_tab::open(st.clone(), body) }))
        .route("/api/v1/cockpit/agents/routines", get({ let st = state.clone(); move || routines::list(st.clone()) }))
        .route("/api/v1/cockpit/agents/routines", post({ let st = state.clone(); move |body: Bytes| routines::save(st.clone(), body) }))
        .route("/api/v1/cockpit/agents/routines/delete", post({ let st = state.clone(); move |body: Bytes| routines::delete(st.clone(), body) }))
        .route("/api/v1/cockpit/agents/routines/run", post({ let (st, a) = (state.clone(), agents.clone()); move |body: Bytes| routines::run_now(st.clone(), a.clone(), body) }))
        .route("/api/v1/cockpit/chat/create", post({ let st = state.clone(); move |body: Bytes| chat_pane::create(st.clone(), body) }))
        .route("/api/v1/cockpit/chat/switch", post({ let (st, a) = (state.clone(), agents.clone()); move |body: Bytes| chat_pane::switch(st.clone(), a.clone(), body) }))
        .route("/api/v1/cockpit/deliver/preflight", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| chat_pane::deliver_preflight(st.clone(), q) }))
        // SAGAS (operator 2026-10-11): sagas > episodes > steps (RFC 0008 §4.10; the tables keep the old names `tasks`), per project, in <project>/.antiloki/work.sqlite; a goal run is synced in as a saga on every read
        .route("/api/v1/sagas", get({ let (st, g) = (state.clone(), goal_runs.clone()); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| work::http_list(st.clone(), g.clone(), q) }))
        .route("/api/v1/sagas/detail", get({ let (st, g) = (state.clone(), goal_runs.clone()); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| work::http_detail(st.clone(), g.clone(), q) }))
        .route("/api/v1/sagas/change", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| work::http_change(st.clone(), q) }))
        .route("/api/v1/sagas/task", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| work::http_episode(st.clone(), q) }))
        .route("/api/v1/sagas/episode", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| work::http_episode(st.clone(), q) }))
        .route("/api/v1/sagas/run", post({ let (st, g, ag) = (state.clone(), goal_runs.clone(), agents.clone()); move |body: Bytes| saga_run::post(st.clone(), g.clone(), ag.clone(), body) }))
        .route("/api/v1/sagas/{op}", post({ let st = state.clone(); move |axum::extract::Path(op): axum::extract::Path<String>, body: Bytes| work::http_op(st.clone(), op, body) }))
        .route("/api/v1/cockpit/agent/ship", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| ship::http_preview(st.clone(), q) }).post({ let st = state.clone(); move |body: Bytes| ship::http_ship(st.clone(), body) }))
        .route("/api/v1/cockpit/deliver/publish", post({ let st = state.clone(); move |body: Bytes| chat_pane::deliver_publish(st.clone(), body) }))
        .route("/api/v1/cockpit/chat/episode", post({ let (st, a) = (state.clone(), agents.clone()); move |body: Bytes| chat_pane::episode(st.clone(), a.clone(), body) }))
        .route("/api/v1/cockpit/chat/rename", post({ let st = state.clone(); move |body: Bytes| chat_pane::rename(st.clone(), body) }))
        .route("/api/v1/cockpit/chat/close", post({ let (st, a) = (state.clone(), agents.clone()); move |body: Bytes| chat_pane::close(st.clone(), a.clone(), body) }))
        .route("/api/v1/cockpit/agents/live", get({ let (st, a, p, g) = (state.clone(), agents.clone(), ptys.clone(), goal_runs.clone()); move || agents_live::live(st.clone(), a.clone(), p.clone(), g.clone()) }))
        .route("/api/v1/cockpit/agent/stop", post({ let a = agents.clone(); move |body: axum::body::Bytes| agent::agent_stop(a.clone(), body) }))
        .route("/api/v1/cockpit/agent/note", post({ let st = state.clone(); let a = agents.clone(); move |body: axum::body::Bytes| agent::agent_note(st.clone(), a.clone(), body) }))
        .route("/api/v1/cockpit/agent/permission", post({ let a = agents.clone(); move |body: axum::body::Bytes| agent::agent_permission(a.clone(), body) }))
        .route("/api/v1/cockpit/agent/status", get({ let a = agents.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agent::agent_status(a.clone(), q) }))
        .route("/api/v1/cockpit/agent/events", get({ let st = state.clone(); let a = agents.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| agent::agent_events(st.clone(), a.clone(), q) }))
        .route("/api/v1/cockpit/pty/input", post({
            let r = ptys.clone();
            move |body: Bytes| pty::pty_input(r, body)
        }))
        .route("/api/v1/cockpit/pty/send", post({
            let r = ptys.clone();
            move |body: Bytes| pty::pty_send(r, body)
        }))
        .route("/api/v1/cockpit/pty/screen", get(|axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| async move { let p: i64 = q.get("pane_id").and_then(|s| s.parse().ok()).unwrap_or(0); let rows = tokio::task::spawn_blocking(move || crate::pty_remote::screen_rows(p)).await.ok().flatten().unwrap_or_default(); axum::Json(json!({ "ok": true, "rows": rows })) }))
        .route("/api/v1/cockpit/watch", get(|| async { axum::Json(pane_watch::view().await) }).post({ let (st, ag) = (state.clone(), agents.clone()); move |body: Bytes| { let (st, ag) = (st.clone(), ag.clone()); async move { let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); axum::Json(pane_watch::set(&st, &ag, &b).await) } } }))
        .route("/api/v1/cockpit/browser", post(|body: Bytes| async move { let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); axum::Json(tokio::task::spawn_blocking(move || agent_browser::answer(&b, cli::now_ms())).await.unwrap_or(Value::Null)) }))
        .route("/api/v1/cockpit/pane/queue", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| { let st = st.clone(); async move { axum::Json(task_queue::view(&st, q.get("pane_id").and_then(|v| v.parse().ok()).unwrap_or(0)).await) } } }))
        .route("/api/v1/cockpit/groups", get(|axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| async move { let id: u64 = q.get("id").and_then(|v| v.parse().ok()).unwrap_or(0); axum::Json(tokio::task::spawn_blocking(move || groups::one(id)).await.unwrap_or(Value::Null)) }).post(|body: Bytes| async move { let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); axum::Json(tokio::task::spawn_blocking(move || groups::set(&b, cli::now_ms())).await.unwrap_or(Value::Null)) }))
        .route("/api/v1/cockpit/jobs", get({ let st = state.clone(); move || { let st = st.clone(); async move { let root = agents_http::active_root(&st).await; axum::Json(tokio::task::spawn_blocking(move || standing::view(&root)).await.unwrap_or(Value::Null)) } } }).post({ let st = state.clone(); move |body: Bytes| { let st = st.clone(); async move { let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); let root = agents_http::active_root(&st).await; axum::Json(standing::set(&root, &b).await) } } }))
        .route("/api/v1/cockpit/board", get({ let st = state.clone(); move || { let st = st.clone(); async move { axum::Json(board::view(&st).await) } } }))
        .route("/api/v1/cockpit/mail", post({ let r = ptys.clone(); move |body: Bytes| { let r = r.clone(); async move { let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); axum::Json(mailbox::set(&r, &b).await) } } }))
        .route("/api/v1/cockpit/avatars", get(|| async { axum::Json(sleep::view()) }))
        .route("/api/v1/cockpit/sleep", post(|body: Bytes| async move { axum::Json(sleep::set(&serde_json::from_slice(&body).unwrap_or(Value::Null))) }))
        .route("/api/v1/cockpit/sleep/wake", post(|body: Bytes| async move { let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); axum::Json(sleep::wake(b["pane_id"].as_i64().unwrap_or(0))) }))
        .route("/api/v1/cockpit/sleep/now", post({ let (st, gr) = (state.clone(), goal_runs.clone()); move |body: Bytes| { let (st, gr) = (st.clone(), gr.clone()); async move {
            let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); let p = b["pane_id"].as_i64().unwrap_or(0);
            let root = crate::agents_http::active_root(&st).await;
            axum::Json(match sleep::put_to_sleep(&st, &gr, p, &root).await { Ok(()) => json!({ "ok": true }), Err(e) => json!({ "ok": false, "error": e }) }) } } }))
        .route("/api/v1/cockpit/pty/host", get(pty::pty_host_status))
        // RFC 0007 §5.5 to §5.8: what the terminal screen asks of the serve besides a terminal (cowork.rs)
        .route("/api/v1/cockpit/precheck", post({ let st = state.clone(); move |body: Bytes| cowork::precheck(st.clone(), body) }))
        .route("/api/v1/cockpit/pty/transcript", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| cowork::transcript_get(st.clone(), q) }))
        .route("/api/v1/cockpit/pty/usage", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| cowork::usage_get(st.clone(), q) }))
        .route("/api/v1/cockpit/pty/consolidate", post(|body: Bytes| cowork::consolidate_post(body)))
        .route("/api/v1/cockpit/cowork/layout", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| cowork::layout_get(st.clone(), q) }))
        .route("/api/v1/voice/status", get(voice::status))
        // a recording is bigger than axum's 2 MB default for a body (ten minutes of opus is a few MB): this one route takes what voice.rs accepts
        .route("/api/v1/voice/transcribe", post(voice::transcribe).layer(axum::extract::DefaultBodyLimit::max(26 * 1024 * 1024)))
        .route("/api/v1/machines", get(machines::list))
        .route("/api/v1/machines/add", post(machines::add))
        .route("/api/v1/machines/connect", post(machines::connect_http))
        .route("/api/v1/machines/disconnect", post(machines::disconnect_http))
        .route("/api/v1/machines/remove", post(machines::remove))
        .route("/api/v1/remote/status", get(remote::status))
        .route("/api/v1/remote/enable", post(remote::enable_http))
        .route("/api/v1/remote/disable", post(remote::disable_http))
        .route("/api/v1/remote/ping", get(remote::ping))
        .route("/api/v1/remote/me", get(remote::me))
        .route("/api/v1/remote/pair/start", post(remote::pair_start))
        .route("/api/v1/remote/pair/claim", post(remote::pair_claim))
        .route("/api/v1/hooks/team/{team}/{trigger}", post({ let st = state.clone(); move |axum::extract::Path((team, trigger)): axum::extract::Path<(String, String)>, headers: axum::http::HeaderMap, body: axum::body::Bytes| team_graph_run::webhook(st.clone(), team, trigger, headers, body) }))
        .route("/api/v1/remote/devices/revoke", post(remote::revoke))
        .route("/api/v1/remote/tunnel/start", post(remote::tunnel_start))
        .route("/api/v1/remote/tunnel/stop", post(remote::tunnel_stop))
        .route("/api/v1/cockpit/side/open", post({ let st = state.clone(); move |body: Bytes| side::open(st.clone(), body) }))
        .route("/api/v1/cockpit/cowork/threads/tree", post({ let st = state.clone(); move |body: Bytes| thread_tree::tree_op(st.clone(), body) }))
        .route("/api/v1/cockpit/cowork/git/start", post({ let st = state.clone(); move || thread_tree::git_start(st.clone()) }))
        .route("/api/v1/cockpit/agent/new", post({ let st = state.clone(); move |body: Bytes| thread_tree::agent_new(st.clone(), body) }))
        .route("/api/v1/cockpit/cowork/threads", post({ let st = state.clone(); move |body: Bytes| cowork::threads_op(st.clone(), body) }))
        .route("/api/v1/cockpit/cowork/layout", post({ let st = state.clone(); move |body: Bytes| cowork::layout_put(st.clone(), body) }))
        .route("/api/v1/themes/omarchy", get(|| themes::themes_get()))
        .route("/api/v1/themes/omarchy/file", get(|axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| themes::theme_file(q)))
        .route("/api/v1/themes/omarchy/install", post(|body: Bytes| themes::theme_install(body)))
        .route("/api/v1/cockpit/team-graph/media", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>, headers: axum::http::HeaderMap| team_media::http(st.clone(), q, headers) }))
        .route("/api/v1/cockpit/team-graph", post({ let st = state.clone(); move |body: Bytes| team_graph_run::http(st.clone(), body) }))
        .route("/api/v1/panes", post({ let st = state.clone(); move |body: Bytes| panes::http(st.clone(), body) }))
        .route("/api/v1/cockpit/teams", post({ let (st, r) = (state.clone(), ptys.clone()); move |body: Bytes| team_run::http(st.clone(), r.clone(), body) }))
        .route("/api/v1/cockpit/crews", post({ let (st, r) = (state.clone(), ptys.clone()); move |body: Bytes| team_run::http(st.clone(), r.clone(), body) }))  // the old name: an older screen or script
        .route("/api/v1/cockpit/pane/attach", post({ let st = state.clone(); move |body: Bytes| attach::post(st.clone(), body) }).layer(axum::extract::DefaultBodyLimit::max(30 * 1024 * 1024)))
        .route("/api/v1/diff", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| diff_view::files_http(st.clone(), q) }))
        .route("/api/v1/diff/patch", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| diff_view::patch_http(st.clone(), q) }))
        .route("/api/v1/frames", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| frames::list_http(st.clone(), q) }).post({ let st = state.clone(); move |body: Bytes| frames::post(st.clone(), body) }))
        .route("/api/v1/frames/html", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| frames::html_http(st.clone(), q) }))
        .route("/api/v1/webapps/launch", post(webapps::launch_http))
        .route("/api/v1/webapps/oauth/callback", get(webapps::oauth_callback))
        .route("/api/v1/webapps/asset", get(webapps::asset))
        .route("/api/v1/webapps", get(|| webapps::get()).post(|body: Bytes| webapps::post(body)))
        .route("/api/v1/local/models", get(local_panes::models_get))
        .route("/api/v1/local/ports", get(ports::ports_get))
        .route("/api/v1/files/list", get(files_explorer::list_get))
        .route("/api/v1/files/read", get(files_explorer::read_get))
        .route("/api/v1/files/raw", get(files_explorer::raw_get))
        .route("/api/v1/cockpit/tokens-total", get(work::tokens_total_get))
        .route("/api/v1/cockpit/specialist/panes", get(specialists::panes_get))
        .route("/api/v1/cockpit/pane/models", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| pane_model::models_get(st.clone(), q) }))
        .route("/api/v1/cockpit/pane/release", post({ let (st, r, g) = (state.clone(), ptys.clone(), goal_runs.clone()); move |body: Bytes| pane_model::release_post(st.clone(), r.clone(), g.clone(), body) }))
        .route("/api/v1/cockpit/pane/heimdall", post({ let st = state.clone(); move |body: Bytes| pane_model::heimdall_post(st.clone(), body) }))
        .route("/api/v1/cockpit/pane/mcp-up", post({ let r = ptys.clone(); move |body: Bytes| pty::pane_mcp_up(r.clone(), body) }))
        .route("/api/v1/cockpit/pane/mcp", get({ let r = ptys.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| pty::pane_mcp_get(r.clone(), q) }))
        .route("/api/v1/cockpit/pane/model", post({ let (st, r) = (state.clone(), ptys.clone()); move |body: Bytes| pane_model::model_post(st.clone(), r.clone(), body) }))
        // CONNECTORS (RFC 0022): the Connectors pane's routes; the agent's tool is `antiloki_connector`
        .route("/api/v1/connectors/list", get({ let st = state.clone(); move || connectors::list_get(st.clone()) }))
        .route("/api/v1/connectors/get", get({ let st = state.clone(); move |q: axum::extract::Query<std::collections::HashMap<String, String>>| connectors::get_get(st.clone(), q) }))
        .route("/api/v1/connectors/save", post({ let st = state.clone(); move |body: Bytes| connectors::save_post(st.clone(), body) }))
        .route("/api/v1/connectors/test", post({ let st = state.clone(); move |body: Bytes| connectors::test_post(st.clone(), body) }))
        .route("/api/v1/connectors/remove", post({ let st = state.clone(); move |body: Bytes| connectors::remove_post(st.clone(), body) }))
        .route("/api/v1/connectors/access", post({ let st = state.clone(); move |body: Bytes| connectors::access_post(st.clone(), body) }))
        .route("/api/v1/connectors/oauth/start", get({ let st = state.clone(); move |q: axum::extract::Query<std::collections::HashMap<String, String>>| connectors::oauth_start_get(st.clone(), q) }))
        .route("/api/v1/connectors/oauth/callback", get({ let st = state.clone(); move |q: axum::extract::Query<std::collections::HashMap<String, String>>| connectors::oauth_callback_get(st.clone(), q) }))
        .route("/api/v1/connectors/logo", get({ let st = state.clone(); move |q: axum::extract::Query<std::collections::HashMap<String, String>>| connectors::logo_get(st.clone(), q) }))
        .route("/api/v1/cockpit/team/onboard", post({ let st = state.clone(); move |body: Bytes| team_onboarding::onboard_post(st.clone(), body) }))
        .route("/api/v1/cockpit/pane/provider", post({ let (st, g) = (state.clone(), goal_runs.clone()); move |body: Bytes| pane_provider::provider_post(st.clone(), g.clone(), body) }))
        .route("/api/v1/xray/query", post({ let st = state.clone(); move |body: Bytes| cowork_xray::query_http(st.clone(), body) }))
        .route("/api/v1/xray/v2", post({ let st = state.clone(); move |body: Bytes| xray_widgets::http(st.clone(), body) }))
        .route("/api/v1/cockpit/cowork/commands", get(|| cowork_mcp::commands_get()))
        .route("/api/v1/cockpit/cowork/commands/result", post(|body: Bytes| cowork_mcp::commands_result(body)))
        .route("/api/v1/cockpit/cowork/commands/run", post(|body: Bytes| cowork_mcp::commands_run(body)))
        .route("/api/v1/cockpit/cowork/qa/view", get(|axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| cowork_mcp::qa_view(q)))
        .route("/api/v1/cockpit/cowork/mcp", post({ let (st, r) = (state.clone(), ptys.clone()); move |body: Bytes| cowork_mcp::mcp(st.clone(), r.clone(), body) }))
        .route("/api/v1/skillshare/state", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| skillshare::state_get(st.clone(), q) }))
        .route("/api/v1/skillshare/run", post({ let st = state.clone(); move |body: Bytes| skillshare::run_post(st.clone(), body) }))
        .route("/api/v1/cockpit/cowork/asks", get(|| cowork::asks_get()))
        .route("/api/v1/cockpit/cowork/asks/answer", post(|body: Bytes| cowork::asks_answer(body)))
        .route("/api/v1/cockpit/cowork/asks/wait", post(|body: Bytes| cowork::asks_wait(body)))
        .route("/api/v1/cockpit/cowork/workers", get({ let (st, g) = (state.clone(), goal_runs.clone()); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| cowork_workers::workers_get(st.clone(), g.clone(), q) }))
        .route("/api/v1/cockpit/episodes", get({ let st = state.clone(); move |axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>| cowork::episodes_get(st.clone(), q) }))
        .route("/api/v1/cockpit/pty/resize", post({
            let r = ptys.clone();
            move |body: Bytes| pty::pty_resize(r, body)
        }))
        .route("/api/v1/cockpit/pty/kill", post({
            let r = ptys.clone();
            move |body: Bytes| pty::pty_kill(r, body)
        }))
        .route("/api/v1/cockpit/pty/start", post({
            let (st, r) = (state.clone(), ptys.clone());
            move |body: Bytes| pty::cockpit_pty_start(st, r, body)
        }))
        .route("/api/v1/workspace/pty/start", post({
            let (st, r) = (state.clone(), ptys.clone());
            move |body: Bytes| pty::workspace_pty_start(st, r, body)
        }))
        .route("/api/v1/cockpit/pane/remove", post({
            let (st, r) = (state.clone(), ptys.clone());
            move |headers: HeaderMap, body: Bytes| pty::cockpit_pane_remove(st, r, headers, body)
        }))
        .route("/api/v1/fe-errors", post({
            let st = state.clone();
            move |body: Bytes| custom::fe_errors(st, body)
        }))
        .route("/api/v1/codebase/ui/record", post({
            let st = state.clone();
            move |headers: HeaderMap, body: Bytes| custom::codebase_ui_record(st, headers, body)
        }))
        .route("/api/v1/dispatch", post({
            let st = state.clone();
            move |headers: HeaderMap, body: Bytes| custom::generic_dispatch(st, headers, body)
        }))
        .route("/api/v1/events", get({
            let hub = hub.clone();
            move || events::sse_handler(hub)
        }));

    for r in routes::ROUTES {
        let st = state.clone();
        // Parsed once here, not per request. A malformed template is a codegen bug, so it fails
        // loudly at startup rather than turning into a wrong-arguments call at runtime.
        let tpl: Arc<dispatch::InputTpl> = Arc::new(
            serde_json::from_str(r.input_tpl)
                .unwrap_or_else(|e| panic!("antiloki-serve: bad input template for {} {}: {e}", r.method, r.path)),
        );
        let action = r.action;
        let method = r.method;
        // RawPathParams works on routes with and without parameters, so one handler shape covers
        // both. Body must stay the LAST extractor — it consumes the request.
        let handler = move |headers: HeaderMap,
                            raw: axum::extract::RawPathParams,
                            axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
                            body: Bytes| async move {
            let path: Vec<(String, String)> =
                raw.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            dispatch::dispatch(st, tpl, headers, path, query, method, action, body).await
        };
        app = match r.method {
            "POST" => app.route(r.path, post(handler)),
            "GET" => app.route(r.path, get(handler)),
            other => panic!("antiloki-serve: unsupported method {other} for {}", r.path),
        };
    }

    // The FE is a separate origin in dev (vite on 5173), so CORS is on - but ONLY for the app's own origins (guard.rs; audit 2026-10-01 F-01:
    // `allow_origin(Any)` let any web page the operator visited write to this loopback server and read its answers).
    let app = app
        .fallback(|req: axum::extract::Request| async move {
            // THE APP'S OWN FILES, for a device that came to load the page (remote.rs): a GET that is not the API, when the serve was given the built app
            if matches!(*req.method(), axum::http::Method::GET | axum::http::Method::HEAD) && !req.uri().path().starts_with("/api/") { if let Some(r) = remote::serve_app(req.uri().path()) { return r; } }
            axum::response::IntoResponse::into_response((axum::http::StatusCode::NOT_FOUND, Json(json!({ "message": "Not Found", "statusCode": 404 }))))
        })
        // A PROJECT SWITCH EMPTIES THE LIVE AGENTS' STATE (see AgentRegistry::sync_to): checked before every request, a counter compare when nothing changed
        .layer(axum::middleware::from_fn({ let ag = agents.clone(); move |req: axum::extract::Request, next: axum::middleware::Next| { let ag = ag.clone(); async move { ag.sync_epoch(); next.run(req).await } } }))
        // THE REQUEST METER (2026-09-20): ANTILOKI_REQ_METER=1 prints path × calls × ms every 60 s —
        // the tool that showed the FE's pollers keeping an idle serve at ~60% of a core
        .layer(axum::middleware::from_fn(req_meter::meter))
        // THE LOCAL DOOR: a foreign Origin may not write, a foreign Host (DNS rebinding) may not do anything (guard.rs)
        .layer(axum::middleware::from_fn(guard::guard))
        .layer(guard::cors_layer());

    // The driver tick loop. Honours ANTILOKI_BE_TICK_DISABLED so a probe (or a test) can bring the
    // surface up with no pipeline motion — the same switch the BE's claude-test lane uses.
    let ticker = tick::Ticker::new(state.clone());
    {
        let t = ticker.clone();
        let _ = state.waker.set(Arc::new(move || t.wake()));
    }
    ticker.start();

    app
}

/// The cockpit pane whose worktree is `cwd` (a goal run's box) — Null when none matches, which makes
/// every ready specialist available (no whitelist to honour).
fn pane_by_worktree(conn: &rusqlite::Connection, cwd: &str) -> Value {
    let norm = |p: &str| p.trim_end_matches(['/', '\\']).replace('\\', "/");
    let want = norm(cwd);
    antiloki_rs::db_json::query_rows(conn, "SELECT * FROM cockpit_panes WHERE worktree_path IS NOT NULL", &[]).ok()
        .and_then(|rows| rows.as_array().and_then(|a| a.iter().find(|p| p.get("worktree_path").and_then(Value::as_str).map(|w| norm(w) == want).unwrap_or(false)).cloned()))
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod data_root_tests {
    use super::default_data_root;
    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a { move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string()) }
    #[test]
    fn the_installed_apps_data_root_is_the_users_data_folder_per_os() {
        let p = |v: Option<std::path::PathBuf>| v.unwrap().to_string_lossy().replace('\\', "/");
        assert_eq!(p(default_data_root("windows", &env(&[("LOCALAPPDATA", "C:\\Users\\ana\\AppData\\Local")]))), "C:/Users/ana/AppData/Local/antiloki");
        assert_eq!(p(default_data_root("windows", &env(&[("USERPROFILE", "C:\\Users\\ana")]))), "C:/Users/ana/AppData/Local/antiloki");
        assert_eq!(p(default_data_root("macos", &env(&[("HOME", "/Users/ana")]))), "/Users/ana/Library/Application Support/antiloki");
        assert_eq!(p(default_data_root("linux", &env(&[("HOME", "/home/ana")]))), "/home/ana/.local/share/antiloki");
        assert_eq!(p(default_data_root("linux", &env(&[("HOME", "/home/ana"), ("XDG_DATA_HOME", "/data")]))), "/data/antiloki");
        assert!(default_data_root("linux", &env(&[])).is_none(), "no home, no guess — never the build checkout");
    }
}

#[cfg(test)]
mod private_auth_tests {
    use super::ensure_private_auth_dir;
    /// 2026-10-13: the auth directory is created mode 0700; its parent and a sibling decoy are not chmod'd.
    #[test]
    fn ensure_private_auth_dir_chmods_only_that_directory() {
        let home = std::env::temp_dir().join(format!("antiloki-auth-priv-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let decoy = home.join("codex");
        std::fs::create_dir_all(&decoy).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::set_permissions(&decoy, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let prev = std::env::var("ANTILOKI_HOME").ok();
        std::env::set_var("ANTILOKI_HOME", &home);
        ensure_private_auth_dir();
        let auth = antiloki_rs::engine_roots::auth_dir();
        assert_eq!(auth, home.join("auth"));
        assert!(auth.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&auth).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::metadata(&home).unwrap().permissions().mode() & 0o777, 0o755, "parents are not chmod'd");
            assert_eq!(std::fs::metadata(&decoy).unwrap().permissions().mode() & 0o777, 0o755, "a sibling decoy is not ~/.codex and is not touched");
        }
        match prev {
            Some(v) => std::env::set_var("ANTILOKI_HOME", v),
            None => std::env::remove_var("ANTILOKI_HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }
}
