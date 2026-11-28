//! THE PRINT-MODE ENGINES — Cursor and Grok as chat sessions (operator 2026-09-27, the five-seat pivot).
//!
//! Three of the five seats already had a chat adapter (`agent.rs` Claude Code, `agent_codex.rs`, `agent_opencode.rs`)
//! and two did not, so "Claude Grok, Gemini Codex" was true of goal runs and false of the agent pane — the screen the
//! product is actually sold on. This module is the missing half for the two that speak the SAME wire format Claude
//! Code does, probed on this machine 2026-09-28:
//!
//!   cursor-agent -p --force --output-format stream-json --model composer-2.5   (signed in: `apiKeySource: login`)
//!     → {"type":"system","subtype":"init",…} · {"type":"user"…} · {"type":"thinking","subtype":"delta"…}
//!       · {"type":"assistant","message":{…}} · {"type":"result","subtype":"success","usage":{"inputTokens":…}}
//!   grok --output-format streaming-messages-json --prompt-file <f>             (NOT signed in here)
//!     → the same envelope; its own error arrives as `result.errors[]`
//!
//! WHY A PROCESS PER TURN, when Claude Code and Antigravity hold one open. Neither CLI has an `--input-format
//! stream-json`: there is no way to hand a second ask to a running process. Both do have a session to resume
//! (`--resume <id>` · `--continue`), so the CONVERSATION is continuous even though the process is not — which is
//! what the pane promises. The cost is one cold start per ask, and it is the only shape these two offer.
//!
//! The translation is deliberately thin: the envelope is already ours. What this adds is the cost the CLIs do not
//! report (`result.usage` × `cli::prices_of`), the camelCase→snake_case usage the rest of serve reads, and Cursor's
//! `thinking` deltas mapped onto the `stream_event` the thread already draws.

use crate::agent::{AgentRegistry, Live};
use crate::cli::{now_ms, Engine};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;

fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }

/// The engines this module drives. Both print one NDJSON turn and exit; both resume by session id.
/// THE ASK RIDES ON STDIN FOR CURSOR (found on Windows, 2026-10-11: "cursor-agent: batch file arguments are invalid"). On Windows `cursor-agent` is a `.cmd` shim, and Rust refuses to pass a batch file
/// any argument containing a line break (the 2024 batch-file escaping fix) - and the ask reaches the CLI wrapped in multi-line context, so even "hi" failed. `cursor-agent -p` reads its prompt from
/// stdin (probed 2026-10-11), so nothing multi-line is ever on its command line. Grok takes a file, as before. Pure - tested.
pub fn ask_on_stdin(engine: Engine) -> bool { engine == Engine::Cursor }
pub fn drives(engine: Engine) -> bool { matches!(engine, Engine::Cursor | Engine::Grok) }

/// USAGE, AS THE REST OF SERVE READS IT. Cursor answers in camelCase (`inputTokens`, `cacheReadTokens`) and Grok in
/// the Anthropic snake_case; `note_usage`, the quota banner and the savings line all read snake_case. Pure — tested.
pub fn usage_snake(u: &Value) -> Value {
    let n = |a: &str, b: &str| u.get(a).or_else(|| u.get(b)).and_then(Value::as_f64).unwrap_or(0.0);
    json!({
        "input_tokens": n("input_tokens", "inputTokens"),
        "output_tokens": n("output_tokens", "outputTokens"),
        "cache_read_input_tokens": n("cache_read_input_tokens", "cacheReadTokens"),
        "cache_creation_input_tokens": n("cache_creation_input_tokens", "cacheWriteTokens"),
    })
}

/// WHAT THE TURN COST, since neither CLI says. `total_cost_usd` when the CLI does report one (Grok may), else the
/// token counts at this model's published rate. A seat bills nothing per token, so this is the API-equivalent the
/// savings line is built from — the same basis `fuel` uses everywhere else. Pure — tested.
pub fn cost_of(engine: Engine, model: &str, result: &Value) -> f64 {
    if let Some(c) = result.get("total_cost_usd").and_then(Value::as_f64) { if c > 0.0 { return c } }
    let u = usage_snake(&result.get("usage").cloned().unwrap_or(json!({})));
    let g = |k: &str| u.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    // CACHE IS NOT PRICED LIKE FRESH INPUT (audit 2026-09-28). This charged every cached read at the full input
    // rate and dropped cache CREATION entirely — on a real Claude-shaped turn (14 fresh, 18k created, 234k read)
    // that overstated the cost 5.3x, and disagreed with `cli::price_tokens` and `agent::turn_tokens`, which are
    // what the record and the bench use. One weighting, in one place: a cached read is a tenth, a cache write is
    // a quarter more than fresh.
    let billable_in = g("input_tokens") + g("cache_read_input_tokens") * 0.1 + g("cache_creation_input_tokens") * 1.25;
    let (pin, pout) = crate::cli::prices_of(engine, if model.is_empty() { None } else { Some(model) });
    (billable_in * pin + g("output_tokens") * pout) / 1_000_000.0
}

/// ONE LINE OF THE CLI'S OUTPUT → the events the thread draws, and whether each is kept on the log.
///
/// `session` carries the id the CLI minted on its `init`, so the next ask can resume the same conversation.
/// Everything here is pure over its inputs — tested against the exact lines both CLIs produced on this machine.
pub fn translate(engine: Engine, model: &str, line: &Value, session: &mut String) -> Vec<(Value, bool)> {
    let ty = s(line.get("type"));
    match ty.as_str() {
        "system" => {
            if s(line.get("subtype")) == "init" {
                let id = s(line.get("session_id"));
                if !id.is_empty() { *session = id; }
            }
            // the init line names the tools and the cwd — useful on the log exactly once, at the top of the turn
            vec![(line.clone(), true)]
        }
        // THE ASK IS ALREADY ON THE THREAD. Both CLIs echo the prompt back as a `user` line; the pane drew it the
        // moment it was typed (`agent-chat-pane`'s placeholder), and drawing it again reads as a double send.
        "user" => vec![],
        // CURSOR'S REASONING, ON THE STREAM THE THREAD ALREADY HAS. `thinking · delta` is a token of the model's
        // own reasoning: the same thing Claude Code sends as a `stream_event`, so it arrives as one and is not
        // persisted — reasoning is watched live, never re-read from the log.
        "thinking" => {
            let d = s(line.get("text"));
            if d.is_empty() { return vec![] }
            vec![(json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "thinking_delta", "thinking": d } } }), false)]
        }
        "assistant" => {
            let mut ev = line.clone();
            if ev.get("message").and_then(|m| m.get("model")).is_none() {
                if let Some(m) = ev.get_mut("message").and_then(Value::as_object_mut) { m.insert("model".into(), json!(model)); }
            }
            vec![(ev, true)]
        }
        "result" => {
            let mut ev = line.clone();
            // GROK REPORTS ITS REFUSALS IN A LIST, NOT IN `result` (probed 2026-09-28: "Not signed in. To
            // authenticate without a browser, run: grok login --device-code"). Without this the thread showed an
            // empty error and the operator had no idea a login was all that was missing.
            let errs = line.get("errors").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ")).unwrap_or_default();
            let is_err = line.get("is_error").and_then(Value::as_bool).unwrap_or(false) || !errs.is_empty();
            let text = { let r = s(line.get("result")); if r.is_empty() { errs } else { r } };
            if let Some(o) = ev.as_object_mut() {
                o.insert("is_error".into(), json!(is_err));
                if is_err { o.insert("subtype".into(), json!("error")); }
                if !text.is_empty() { o.insert("result".into(), json!(text)); }
                o.insert("usage".into(), usage_snake(&line.get("usage").cloned().unwrap_or(json!({}))));
                o.insert("total_cost_usd".into(), json!(cost_of(engine, model, line)));
                o.insert("session_id".into(), json!(session.clone()));
            }
            vec![(ev, true)]
        }
        // A LINE WE DO NOT KNOW IS WATCHED, NEVER KEPT (audit 2026-09-28). This persisted everything it did not
        // recognise — and Grok runs with `--include-partial-messages`, so every token delta arrived as a
        // `stream_event` and was written to the pane's log with its own seq. A turn would leave thousands of
        // lines on disk, re-render every fragment as its own entry on reload, and throw the turn/reply counts
        // (which are seq arithmetic) completely off. `agent.rs` has always treated `stream_event` as live-only.
        "stream_event" => vec![(line.clone(), false)],
        _ => vec![(line.clone(), s(line.get("subtype")) != "delta")],
    }
}

/// The argv for one ask. `resume` is the session to continue, empty for the first turn of a conversation.
/// Pure, so the flags are testable without spawning anything — and they are exactly the flags probed to work.
pub fn args_for(engine: Engine, model: &str, resume: &str, prompt_file: &str, max_mode: bool, read_only: bool) -> Vec<String> {
    let mut a: Vec<String> = vec![];
    match engine {
        Engine::Cursor => {
            a.push("-p".into());
            a.push("--output-format".into()); a.push("stream-json".into());
            // WORKSPACE TRUST, ANSWERED ONCE (probed 2026-09-28: without it cursor-agent prints "Workspace Trust
            // Required" to stderr and exits 1, having emitted nothing). The box is a worktree antiloki itself cut.
            //
            // A CHAT IS READ-ONLY BY THE CLI'S OWN MODE, NOT BY A SENTENCE (operator 2026-09-29: an antiChat pane on Composer said "I'm read-only here",
            // then on "go" edited App.tsx and index.css itself and reported the build - no goal run, no Heimdall, no stats). `--force` allows every
            // command and every write; the prompt asking it not to was the only lock. A chat pane answers the trust prompt with `--trust` alone and runs
            // in `--mode ask`, the CLI's read-only Q&A mode. Work goes to Heimdall, which is what the pane is for.
            if read_only { a.push("--trust".into()); a.push("--mode".into()); a.push("ask".into()); } else { a.push("--force".into()); }
            if !model.is_empty() { a.push("--model".into()); a.push(model.into()); }
            if !resume.is_empty() { a.push("--resume".into()); a.push(resume.into()); }
        }
        Engine::Grok => {
            a.push("--output-format".into()); a.push("streaming-messages-json".into());
            a.push("--include-partial-messages".into());
            // Grok opens a tty for its prompt (`os error 6` on stdin) — the ask rides in a file, which also keeps
            // it out of argv. The same rule `cli::needs_prompt_file` already enforces for goal runs.
            a.push("--prompt-file".into()); a.push(prompt_file.into());
            if !model.is_empty() { a.push("-m".into()); a.push(model.into()); }
            if !resume.is_empty() { a.push("--resume".into()); a.push(resume.into()); }
            if max_mode { a.push("--always-approve".into()); }
        }
        _ => {}
    }
    if max_mode && engine == Engine::Cursor { /* --force already allows every command */ }
    a
}

/// THE FLAGS THAT LET A PANE USE THE MCP FILE THE BAKE WROTE (audit 2026-10-05). The bake writes `.antiloki/mcp.json` only while the pane has live tools, so its presence is the question. Cursor asks to approve each project
/// server unless told to (`--approve-mcps`); Grok loads a repo's MCP config only for a trusted folder (`--trust`) - the box is a worktree antiloki cut itself. Pure over the file system - tested.
pub fn mcp_args(engine: Engine, cwd: &str) -> Vec<String> {
    if !Path::new(cwd).join(".antiloki").join("mcp.json").is_file() { return vec![]; }
    match engine { Engine::Cursor => vec!["--approve-mcps".into()], Engine::Grok => vec!["--trust".into()], _ => vec![] }
}

fn pinned(cwd: &str, engine: Engine) -> Option<String> {
    antiloki_rs::cockpit__helpers_engine_handoff::pty_session(Path::new(cwd))
        .filter(|(e, _)| e == engine.key()).map(|(_, id)| id)
}

/// Start (or resume) a Cursor/Grok chat session for this pane — called by `agent::start`.
///
/// Nothing is spawned here: these CLIs run one process per ask, so the process appears when the first ask does.
/// What is created is the session — the id to resume, the channel the pane writes to, and the pump that turns each
/// ask into a run.
pub async fn start(reg: &Arc<AgentRegistry>, pane_id: i64, pane: &Value, cwd: &str, engine: Engine,
                   model: Option<String>, fresh: bool) -> Result<Value, String> {
    if !crate::cli::engine_available(engine) {
        return Err(format!("{} is not installed — the {} CLI reaches that seat", engine.key(), engine.bin()));
    }
    let settings: Value = pane.get("settings_json").and_then(Value::as_str).and_then(|t| serde_json::from_str(t).ok()).unwrap_or(json!({}));
    let max_mode = settings.get("max").and_then(Value::as_bool).unwrap_or(false);
    let read_only = crate::chat_pane::is_readonly(pane);   // a chat in front mode never edits (Heimdall writes); an agent pane does
    let model = model.or_else(|| crate::cli::tier_model(engine, crate::cli::Tier::Low)).unwrap_or_default();
    let prior = pinned(cwd, engine).filter(|_| !fresh).unwrap_or_default();
    let resumed = !prior.is_empty();

    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    reg.live.lock().unwrap_or_else(|p| p.into_inner()).insert(pane_id, Live {
        stdin: tx, child_id: None, engine: engine.key().into(), cancel: cancel.clone(), on_stop: None,
        session_id: prior.clone(), model: model.clone(), busy: false, cost_usd: 0.0, started: now_ms(),
        min: false, lift: false, turn_started: 0, last_ask: String::new(), parent: String::new(),
        turn_tin: 0, turn_tout: 0, gate_retry: false, paused: false,
    });
    reg.emit(pane_id, json!({ "type": "antiloki", "subtype": if resumed { "resumed" } else { "started" },
        "session_id": prior, "model": model, "engine": engine.key() }), true);

    // THE PUMP: one ask → one process → its lines → the thread. It outlives this function and ends when the pane
    // is stopped (the cancel flag) or the channel closes.
    let (reg2, cwd2, model2) = (reg.clone(), cwd.to_string(), model.clone());
    tokio::spawn(async move {
        let mut session = prior;
        while let Some(text) = rx.recv().await {
            if cancel.load(Ordering::Relaxed) { break }
            let started = now_ms();
            // the ask goes in a file for Grok, and for Cursor on stdin - never an argument, never an env var, and
            // never interpolated into a shell line (`crate::proc_async`, the standing rule)
            let pf = std::env::temp_dir().join(format!("antiloki-ask-{pane_id}-{}.txt", now_ms()));
            // 0600, BECAUSE IT IS THE OPERATOR'S ASK (audit 2026-09-28). The default 0644 left every question
            // they typed readable by every account on the machine, in a predictable path under /tmp.
            if engine == Engine::Grok {
                let _ = std::fs::write(&pf, &text);
                #[cfg(unix)] { use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&pf, std::fs::Permissions::from_mode(0o600)); }
            }
            let mut args = args_for(engine, &model2, &session, &pf.to_string_lossy(), max_mode, read_only);
            args.extend(mcp_args(engine, &cwd2));
            let mut cmd = crate::proc_async::tokio_cmd(engine.bin());
            cmd.args(&args).current_dir(&cwd2)
                .stdin(if ask_on_stdin(engine) { std::process::Stdio::piped() } else { std::process::Stdio::null() })
                .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
            let child = cmd.spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(e) => {
                    reg2.emit(pane_id, json!({ "type": "result", "subtype": "error", "is_error": true,
                        "result": format!("{}: {e}", engine.bin()), "num_turns": 0, "total_cost_usd": 0.0 }), true);
                    let mut live = reg2.live.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(x) = live.get_mut(&pane_id) { x.busy = false; }
                    continue;
                }
            };
            // the ask, on stdin, then closed so the CLI starts (never the command line: see `ask_on_stdin`)
            if ask_on_stdin(engine) {
                if let Some(mut si) = child.stdin.take() {
                    let t = text.clone();
                    tokio::spawn(async move { use tokio::io::AsyncWriteExt; let _ = si.write_all(t.as_bytes()).await; let _ = si.shutdown().await; });
                }
            }
            let child_id_now = child.id();
            { let mut live = reg2.live.lock().unwrap_or_else(|p| p.into_inner()); if let Some(x) = live.get_mut(&pane_id) { x.child_id = child_id_now; } }
            // stderr: the CLI's own complaints (a trust prompt, an unknown flag, a login) — one note per line
            if let Some(err) = child.stderr.take() {
                let r3 = reg2.clone();
                tokio::spawn(async move {
                    let mut lines = tokio::io::BufReader::new(err).lines();
                    while let Ok(Some(l)) = lines.next_line().await {
                        let l = l.trim().to_string();
                        if l.is_empty() { continue }
                        r3.emit(pane_id, json!({ "type": "antiloki", "subtype": "note", "tone": "warn", "text": l.chars().take(400).collect::<String>() }), true);
                    }
                });
            }
            let mut saw_result = false;
            if let Some(out) = child.stdout.take() {
                let mut lines = tokio::io::BufReader::new(out).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
                    for (mut ev, persist) in translate(engine, &model2, &v, &mut session) {
                        // THE TURN'S OWN CLOCK (audit 2026-09-28): `started` was computed and thrown away, so a
                        // print-engine turn carried no duration anywhere — the stats box read "0 s" for a turn
                        // that took two minutes. The CLI's own `duration_ms` wins when it reports one.
                        if s(ev.get("type")) == "result" && ev.get("duration_ms").and_then(Value::as_i64).unwrap_or(0) <= 0 {
                            if let Some(o) = ev.as_object_mut() { o.insert("duration_ms".into(), json!(now_ms() - started)); }
                        }
                        if s(ev.get("type")) == "result" {
                            saw_result = true;
                            let cost = ev.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
                            crate::agent::note_usage(Path::new(&cwd2), pane_id, &ev);
                            { let mut live = reg2.live.lock().unwrap_or_else(|p| p.into_inner());
                              if let Some(x) = live.get_mut(&pane_id) { x.busy = false; x.cost_usd += cost; x.session_id = session.clone(); } }
                            reg2.on_turn_result(pane_id, &cwd2, &ev);
                        }
                        if s(ev.get("type")) == "assistant" { crate::agent::note_usage(Path::new(&cwd2), pane_id, &ev); }
                        if s(ev.get("type")) == "assistant" { reg2.note_turn_tokens(pane_id, &ev); }
                        reg2.emit(pane_id, ev, persist);
                    }
                }
            }
            let code = child.wait().await.ok().and_then(|st| st.code());
            // THE PID DIES WITH THE CHILD (audit 2026-09-28). `child_id` was written per turn and never cleared,
            // so between turns the pane held the PID of a dead process — and `AgentRegistry::stop` SIGTERMs it
            // unconditionally. PIDs are recycled: pressing Stop on an idle Cursor pane could have killed whatever
            // process had since been given that number. `memory` and `set_paused` read the same field.
            { let mut live = reg2.live.lock().unwrap_or_else(|p| p.into_inner());
              if let Some(x) = live.get_mut(&pane_id) { if x.child_id == child_id_now { x.child_id = None; } } }
            let _ = std::fs::remove_file(&pf);
            // A TURN THAT PRINTED NOTHING STILL ENDS. Without this the pane stays busy for ever on a CLI that died
            // before its first line — the trust prompt did exactly that, and the composer would never come back.
            // …UNLESS THE OPERATOR STOPPED IT (audit 2026-09-28). `stop()` SIGTERMs the child, stdout hits EOF
            // and `saw_result` is false — so pressing Stop used to write a permanent "ended without answering"
            // failure into the transcript for something the operator did on purpose.
            if !saw_result && !cancel.load(Ordering::Relaxed) {
                { let mut live = reg2.live.lock().unwrap_or_else(|p| p.into_inner()); if let Some(x) = live.get_mut(&pane_id) { x.busy = false; } }
                reg2.emit(pane_id, json!({ "type": "result", "subtype": "error", "is_error": true, "num_turns": 0, "total_cost_usd": 0.0,
                    "result": format!("{} ended without answering (exit {})", engine.bin(), code.map(|c| c.to_string()).unwrap_or_else(|| "signal".into())) }), true);
            }
            // the session id the CLI minted, so a restart of serve resumes the same conversation
            if !session.is_empty() {
                let _ = std::fs::create_dir_all(Path::new(&cwd2).join(".antiloki"));
                let _ = std::fs::write(Path::new(&cwd2).join(".antiloki").join("pty-session.json"),
                    json!({ "engine": engine.key(), "session_id": session, "t": now_ms(), "ui": "chat" }).to_string());
            }
        }
        // THE SESSION ENDS WHEN THE CHANNEL DOES (audit 2026-09-28). Nothing removed this pane's `Live`, so
        // `agent/status` answered `running: true` for ever, `live_count` over-reported, and the pump task held an
        // Arc on the registry that held the sender that kept the pump alive — a cycle only `stop()` could break.
        { let mut live = reg2.live.lock().unwrap_or_else(|p| p.into_inner());
          if live.get(&pane_id).map(|x| x.engine == engine.key()).unwrap_or(false) { live.remove(&pane_id); } }
        reg2.emit(pane_id, json!({ "type": "antiloki", "subtype": "exited", "code": Value::Null, "cwd": cwd2 }), true);
    });
    Ok(json!({ "ok": true, "pane_id": pane_id, "session_id": "", "resumed": resumed, "model": model, "engine": engine.key() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the exact lines cursor-agent printed on this machine, 2026-09-28
    #[test]
    fn cursors_ask_is_on_stdin_never_on_the_command_line() {
        assert!(ask_on_stdin(Engine::Cursor)); assert!(!ask_on_stdin(Engine::Grok), "grok has its own file");
        // the argv the pane builds never contains the ask: it cannot, whatever the ask holds (a line break would make a Windows .cmd shim refuse the whole call)
        let ask = "hi\n\nCONTEXT:\n- a line\n- another";
        for read_only in [true, false] { let a = args_for(Engine::Cursor, "composer-2.5", "", "", false, read_only); assert!(!a.iter().any(|x| x.contains('\n') || x.contains("CONTEXT")), "{a:?}"); }
        let _ = ask;
    }
    #[test]
    fn cursors_stream_is_our_own_envelope_and_its_cost_is_priced_from_the_tokens_it_reports() {
        let mut sid = String::new();
        let init = json!({ "type": "system", "subtype": "init", "apiKeySource": "login", "session_id": "9bf0d37b", "model": "Composer 2.5" });
        let out = translate(Engine::Cursor, "composer-2.5", &init, &mut sid);
        assert_eq!(sid, "9bf0d37b", "the init line is where the session to resume comes from");
        assert_eq!(out.len(), 1);

        // the prompt echo is dropped — the pane drew the ask when it was typed
        assert!(translate(Engine::Cursor, "composer-2.5", &json!({ "type": "user", "message": { "role": "user", "content": [] } }), &mut sid).is_empty());

        // reasoning is a live stream, never persisted
        let th = translate(Engine::Cursor, "composer-2.5", &json!({ "type": "thinking", "subtype": "delta", "text": "The user requested" }), &mut sid);
        assert_eq!(th.len(), 1); assert!(!th[0].1, "a thinking delta is not kept on the log");
        assert_eq!(th[0].0["event"]["delta"]["thinking"], "The user requested");

        let res = json!({ "type": "result", "subtype": "success", "is_error": false, "duration_ms": 4848, "result": "ok",
                          "usage": { "inputTokens": 7738, "outputTokens": 24, "cacheReadTokens": 3916, "cacheWriteTokens": 0 } });
        let out = translate(Engine::Cursor, "composer-2.5", &res, &mut sid);
        let ev = &out[0].0;
        assert_eq!(ev["usage"]["input_tokens"], 7738.0, "camelCase becomes the snake_case the rest of serve reads");
        assert_eq!(ev["usage"]["cache_read_input_tokens"], 3916.0);
        assert_eq!(ev["session_id"], "9bf0d37b");
        let (pin, pout) = crate::cli::prices_of(Engine::Cursor, Some("composer-2.5"));
        let want = ((7738.0 + 3916.0 * 0.1) * pin + 24.0 * pout) / 1e6;
        assert!((ev["total_cost_usd"].as_f64().unwrap() - want).abs() < 1e-12,
                "cache reads are a tenth, the same weighting cli::price_tokens and agent::turn_tokens use");
    }

    #[test]
    fn groks_refusal_reaches_the_thread_as_the_sentence_it_actually_wrote() {
        let mut sid = String::new();
        let res = json!({ "type": "result", "subtype": "error_during_execution", "is_error": true, "num_turns": 0,
                          "errors": ["Not signed in. To authenticate without a browser, run:\n  grok login --device-code"] });
        let out = translate(Engine::Grok, "grok-code-fast-1", &res, &mut sid);
        let ev = &out[0].0;
        assert_eq!(ev["subtype"], "error");
        assert_eq!(ev["is_error"], true);
        assert!(ev["result"].as_str().unwrap().contains("grok login --device-code"), "the fix is in the message or the operator cannot act on it");
    }

    #[test]
    fn a_pane_with_live_tools_is_started_with_the_flags_that_load_them() {
        let d = std::env::temp_dir().join(format!("antiloki-mcpargs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join(".antiloki")).unwrap();
        let cwd = d.to_string_lossy().to_string();
        assert!(mcp_args(Engine::Cursor, &cwd).is_empty() && mcp_args(Engine::Grok, &cwd).is_empty(), "no live tools, no flags");
        std::fs::write(d.join(".antiloki/mcp.json"), "{}").unwrap();
        assert_eq!(mcp_args(Engine::Cursor, &cwd), vec!["--approve-mcps".to_string()]);
        assert_eq!(mcp_args(Engine::Grok, &cwd), vec!["--trust".to_string()]);
        assert!(mcp_args(Engine::Gemini, &cwd).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_flags_are_the_ones_that_were_probed_to_work() {
        // a CHAT is read-only by the CLI's own mode: trust answered, no --force, `--mode ask`
        let chat = args_for(Engine::Cursor, "composer-2.5", "", "", false, true);
        assert!(!chat.contains(&"--force".into()) && chat.contains(&"--trust".into()), "{chat:?}");
        assert!(chat.windows(2).any(|w| w == ["--mode", "ask"]), "{chat:?}");
        let a = args_for(Engine::Cursor, "composer-2.5", "", "", false, false);
        assert!(a.windows(2).any(|w| w == ["--output-format", "stream-json"]));
        assert!(a.contains(&"--force".into()), "without it the CLI stops on Workspace Trust and prints nothing");
        assert!(!a.contains(&"--resume".into()), "the first turn of a conversation has nothing to resume");
        let a = args_for(Engine::Cursor, "composer-2.5", "9bf0d37b", "", false, false);
        assert!(a.windows(2).any(|w| w == ["--resume", "9bf0d37b"]), "the process is new every ask; the conversation is not");
        let a = args_for(Engine::Grok, "grok-code-fast-1", "", "/tmp/ask.txt", true, false);
        assert!(a.windows(2).any(|w| w == ["--prompt-file", "/tmp/ask.txt"]), "Grok opens a tty for stdin (os error 6)");
        assert!(a.contains(&"--always-approve".into()));
    }

    #[test]
    fn only_the_two_print_mode_seats_are_driven_here() {
        assert!(drives(Engine::Cursor) && drives(Engine::Grok));
        assert!(!drives(Engine::ClaudeCode) && !drives(Engine::Codex) && !drives(Engine::Opencode) && !drives(Engine::Gemini));
    }
}
