//! antiloki — the first runnable entry point for the Rust substrate (M1 of the JS→Rust
//! runtime cutover). Composes the never-run spine: openDb (9-DB ATTACH + seed) → skillRun
//! (law loop + steps) → prints the action result as JSON.
//!
//! Usage: antiloki <state.sqlite path> <action> [input-json]
//! Example: antiloki C:/tmp/m1-db/state.sqlite board-keeper.card.list '{}'
//!
//! This is a smoke harness, NOT the production integration (that is napi-rs, a later
//! milestone). It exists to surface the composition bugs that isolated unit tests can't.

use serde_json::{json, Value};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let state = args.get(1).cloned().unwrap_or_default();
    let action = args.get(2).cloned().unwrap_or_else(|| "board-keeper.card.list".to_string());
    // Refuse malformed input-json rather than silently substituting `{}` — a shell that mangles the
    // quoting (Windows PowerShell 5.1 strips them when passing to a native exe) otherwise produces a
    // confusing downstream "X is required" from the skill instead of naming the real problem.
    let input: Value = match args.get(3) {
        None => json!({}),
        Some(s) => match serde_json::from_str::<Value>(s) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[antiloki] input-json is not valid JSON: {e}\n  received: {s}");
                std::process::exit(2);
            }
        },
    };

    // Optional 4th arg: the dispatch context ({actor, source, agent, auth_token, user_goal_id}) —
    // the same object the napi boundary will hand through from the BE. Supplying `agent` opts the
    // call into permission + auth enforcement.
    let context: Value = match args.get(4) {
        None => json!({ "actor": "human:cli", "source": "cli" }),
        Some(s) => match serde_json::from_str::<Value>(s) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[antiloki] context-json is not valid JSON: {e}\n  received: {s}");
                std::process::exit(2);
            }
        },
    };

    if state.is_empty() {
        eprintln!("usage: antiloki <state.sqlite path> <action> [input-json] [context-json]");
        std::process::exit(2);
    }

    eprintln!("[antiloki] openDb({state}) …");
    let db = match antiloki_rs::engine_open_db::openDb(&json!(state), &json!({})) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("[antiloki] OPENDB FAILED: {e}");
            std::process::exit(1);
        }
    };
    eprintln!("[antiloki] openDb ok — 9 DBs attached + seeded");

    // skillRunAs reads `action` and `input` as SIBLING keys of the call object — steps resolve
    // their own input via `$.input`. Merging the CLI json flat next to `action` instead left
    // `$.input` unresolvable; it went unnoticed because the M1 smoke action (card-list) takes no
    // input at all.
    let run_input = json!({ "action": action, "input": input });
    eprintln!("[antiloki] skillRun({action}) …");
    match antiloki_rs::engine_action_run::skillRunWithContext(&run_input, &db, &context) {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()));
        }
        Err(e) => {
            eprintln!("[antiloki] DISPATCH FAILED: {e}");
            std::process::exit(1);
        }
    }
}
