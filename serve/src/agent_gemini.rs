//! GEMINI AS A CHAT SESSION — the Antigravity CLI (`agy`), the live path since the Gemini CLI was retired for
//! Pro/Ultra/free on 18 June 2026.
//!
//! Probed on this machine 2026-09-28 (signed in: `~/.gemini/oauth_creds.json`), `agy --output-format stream-json`
//! answers in an envelope of its OWN, not Claude Code's:
//!
//!   {"event":"init","conversation_id":"2102c87e…","init":{"cwd":…,"tools":[…]}}
//!   {"event":"step_update","step_update":{"step_index":0,"state":"DONE","step_type":"user_input"}}
//!   {"event":"step_update","step_update":{"step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"ok"}}
//!   {"event":"step_update","step_update":{"step_index":1,"state":"DONE","step_type":"agent_response",
//!                                          "duration_seconds":5.94,"usage":{"input_tokens":13126,"output_tokens":28,…}}}
//!   {"event":"result","result":{"status":"SUCCESS","response":"ok\n","num_turns":1,"usage":{…}}}
//!
//! So unlike Cursor and Grok (`agent_print.rs`) this one needs a real translation — and unlike them it holds ONE
//! process for the whole conversation: `--input-format stream-json` reads one NDJSON message per line from stdin
//! and runs a turn for each, which is exactly Claude Code's arrangement in `agent.rs`.
//!
//! THE KNOWN RISK (memory, 2026-09-27): issues #318 and #76 report `agy -p` hanging or dropping stdout in
//! non-TTY environments, which is precisely how serve spawns engines. If that bites, the fallback is `pty.rs`.
//! It did not bite in the probe — a piped run answered in 6 s — and `--input-format stream-json` is a different
//! code path from `-p` besides.

use crate::agent::{AgentRegistry, Live};
use crate::cli::now_ms;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

/// how long a turn may say nothing at all before it is declared stuck — long enough for a slow tool call, short
/// enough that the composer is not locked for an afternoon
const TURN_STALL_MS: i64 = 10 * 60_000;

fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }
fn f(v: Option<&Value>) -> f64 { v.and_then(Value::as_f64).unwrap_or(0.0) }

/// What one turn has accumulated — Antigravity streams deltas and reports usage only at the end of a step.
#[derive(Default, Debug)]
pub struct Turn {
    pub text: String,
    pub started: i64,
    pub tokens_in: f64,
    pub tokens_out: f64,
    pub cache_read: f64,
    pub steps: i64,
    /// the step index whose deltas are currently being collected, so two steps never merge into one bubble
    pub step: i64,
}

/// ONE ANTIGRAVITY EVENT → the events the thread draws, and whether each is kept on the log.
///
/// The shape it has to produce is the one every other adapter produces: `assistant` messages, a `result` that ends
/// the turn, `stream_event` deltas for what is being typed right now. Pure over `turn` and the line — tested
/// against the exact events `agy` printed here.
pub fn translate(turn: &mut Turn, model: &str, conversation: &mut String, line: &Value) -> Vec<(Value, bool)> {
    let mut out = vec![];
    match s(line.get("event")).as_str() {
        "init" => {
            let id = s(line.get("conversation_id"));
            if !id.is_empty() { *conversation = id.clone(); }
            let init = line.get("init").cloned().unwrap_or(json!({}));
            out.push((json!({ "type": "system", "subtype": "init", "session_id": id, "model": model,
                              "cwd": s(init.get("cwd")), "tools": init.get("tools").cloned().unwrap_or(json!([])) }), true));
        }
        "step_update" => {
            let u = line.get("step_update").cloned().unwrap_or(json!({}));
            let kind = s(u.get("step_type"));
            let state = s(u.get("state"));
            let idx = u.get("step_index").and_then(Value::as_i64).unwrap_or(0);
            // the echo of what was just sent — the pane already drew it
            if kind == "user_input" { if turn.started == 0 { turn.started = now_ms(); } return out }
            let delta = s(u.get("text_delta"));
            if kind == "agent_response" {
                // A NEW STEP CLOSES THE ONE BEFORE IT. Antigravity can answer in several steps (a tool call between
                // them); each is its own bubble, and merging them would put a tool's output inside the sentence
                // that preceded it.
                if idx != turn.step && !turn.text.trim().is_empty() {
                    out.push((json!({ "type": "assistant", "message": { "role": "assistant", "model": model,
                                      "content": [{ "type": "text", "text": turn.text.clone() }] } }), true));
                    turn.text.clear();
                }
                turn.step = idx;
                if !delta.is_empty() {
                    turn.text.push_str(&delta);
                    out.push((json!({ "type": "stream_event", "event": { "type": "content_block_delta",
                                      "delta": { "type": "text_delta", "text": delta } } }), false));
                }
                if state == "DONE" {
                    let usage = u.get("usage").cloned().unwrap_or(json!({}));
                    // INPUT IS CUMULATIVE, OUTPUT IS NOT (audit 2026-09-28). Each step's `input_tokens` counts
                    // the whole conversation so far, so SUMMING them charges the context once per step: a
                    // five-step turn reported five times the input it actually sent. The turn's input is the
                    // largest step's; the output and the thinking genuinely add up.
                    turn.tokens_in = turn.tokens_in.max(f(usage.get("input_tokens")));
                    turn.tokens_out += f(usage.get("output_tokens")) + f(usage.get("thinking_tokens"));
                    turn.cache_read = turn.cache_read.max(f(usage.get("cache_read_tokens")));
                    turn.steps += 1;
                    if !turn.text.trim().is_empty() {
                        out.push((json!({ "type": "assistant", "message": { "role": "assistant", "model": model,
                                          "content": [{ "type": "text", "text": turn.text.clone() }] } }), true));
                        turn.text.clear();
                    }
                }
            }
            // a tool step: announced as the tool_use the thread already knows how to draw
            if kind == "tool_call" || kind == "tool" {
                // …and it closes whatever prose is still buffered, or the sentence that INTRODUCED the tool call
                // renders after it (audit 2026-09-28). The step-boundary check above only fires between two
                // agent_response steps, which is not the only way a bubble can be interrupted.
                if !turn.text.trim().is_empty() {
                    out.push((json!({ "type": "assistant", "message": { "role": "assistant", "model": model,
                                      "content": [{ "type": "text", "text": turn.text.clone() }] } }), true));
                    turn.text.clear();
                }
                let name = { let n = s(u.get("tool_name")); if n.is_empty() { s(u.get("name")) } else { n } };
                if state == "ACTIVE" && !name.is_empty() {
                    out.push((json!({ "type": "assistant", "message": { "role": "assistant", "model": model,
                        "content": [{ "type": "tool_use", "id": format!("agy-{idx}"), "name": name,
                                      "input": u.get("tool_input").cloned().unwrap_or(json!({})) }] } }), true));
                }
                if state == "DONE" {
                    let output: String = s(u.get("tool_output")).chars().take(4000).collect();
                    out.push((json!({ "type": "user", "message": { "role": "user",
                        "content": [{ "type": "tool_result", "tool_use_id": format!("agy-{idx}"), "content": output, "is_error": false }] } }), true));
                }
            }
        }
        "result" => {
            let r = line.get("result").cloned().unwrap_or(json!({}));
            let status = s(r.get("status"));
            let ok = status.eq_ignore_ascii_case("SUCCESS");
            // whatever is still buffered belongs to the answer, not to the next turn
            if !turn.text.trim().is_empty() {
                out.push((json!({ "type": "assistant", "message": { "role": "assistant", "model": model,
                                  "content": [{ "type": "text", "text": turn.text.clone() }] } }), true));
            }
            let usage = r.get("usage").cloned().unwrap_or(json!({}));
            let tin = if turn.tokens_in > 0.0 { turn.tokens_in } else { f(usage.get("input_tokens")) };
            let tout = if turn.tokens_out > 0.0 { turn.tokens_out } else { f(usage.get("output_tokens")) + f(usage.get("thinking_tokens")) };
            let (pin, pout) = crate::cli::prices_of(crate::cli::Engine::Gemini, if model.is_empty() { None } else { Some(model) });
            let cost = ((tin + turn.cache_read) * pin + tout * pout) / 1_000_000.0;
            let msg = { let e = s(r.get("error")); if e.is_empty() { s(r.get("message")) } else { e } };
            out.push((json!({ "type": "result", "subtype": if ok { "success" } else { "error" }, "is_error": !ok,
                "result": if ok { s(r.get("response")) } else if msg.is_empty() { status.clone() } else { msg },
                "session_id": conversation.clone(),
                "duration_ms": (f(r.get("duration_seconds")) * 1000.0) as i64,
                "num_turns": r.get("num_turns").and_then(Value::as_i64).unwrap_or(turn.steps),
                "total_cost_usd": cost,
                "usage": { "input_tokens": tin, "output_tokens": tout, "cache_read_input_tokens": turn.cache_read, "cache_creation_input_tokens": 0.0 } }), true));
            *turn = Turn::default();
        }
        _ => {}
    }
    out
}

/// THE ASK, IN ANTIGRAVITY'S INPUT ENVELOPE. `{"event":"user","message":{"role":"user","content":…}}` — probed
/// 2026-09-28; the CLI names every shape it rejects, and this is the only one it takes. Pure — tested.
pub fn input_line(text: &str) -> String {
    json!({ "event": "user", "message": { "role": "user", "content": text } }).to_string()
}

/// The argv for the session. `conversation` resumes a previous one, empty for a new conversation.
/// Pure — these are the flags `agy --help` documents and the probe confirmed.
pub fn args_for(model: &str, conversation: &str, max_mode: bool) -> Vec<String> {
    let mut a: Vec<String> = vec![
        "--input-format".into(), "stream-json".into(),
        "--output-format".into(), "stream-json".into(),
    ];
    // `--print` is what puts the CLI in headless mode at all — and it is a flag that DEMANDS a value, so bare
    // `--print` swallows whatever follows it ("--print took \"--model\" as its prompt", seen 2026-09-28) and
    // `--print` last fails with "flag needs an argument". With stream-json on stdin there IS no prompt argument,
    // so the value is empty: `--print=`. Probed: that is the form that runs.
    a.push("--print=".into());
    if !model.is_empty() { a.push("--model".into()); a.push(model.into()); }
    if !conversation.is_empty() { a.push("--conversation".into()); a.push(conversation.into()); }
    if max_mode { a.push("--dangerously-skip-permissions".into()); }
    a
}

fn pinned(cwd: &str) -> Option<String> {
    antiloki_rs::cockpit__helpers_engine_handoff::pty_session(Path::new(cwd))
        .filter(|(e, _)| e == "gemini").map(|(_, id)| id)
}

/// Start (or resume) the pane's Antigravity session — called by `agent::start` for a gemini pane.
pub async fn start(reg: &Arc<AgentRegistry>, pane_id: i64, pane: &Value, cwd: &str,
                   model: Option<String>, fresh: bool) -> Result<Value, String> {
    let engine = crate::cli::Engine::Gemini;
    if !crate::cli::engine_available(engine) {
        return Err("the Antigravity CLI is not installed — `curl -fsSL https://antigravity.google/cli/install.sh | bash` puts `agy` on the PATH".into());
    }
    let settings: Value = pane.get("settings_json").and_then(Value::as_str).and_then(|t| serde_json::from_str(t).ok()).unwrap_or(json!({}));
    let max_mode = settings.get("max").and_then(Value::as_bool).unwrap_or(false);
    let model = model.or_else(|| crate::cli::tier_model(engine, crate::cli::Tier::Low)).unwrap_or_default();
    let prior = pinned(cwd).filter(|_| !fresh).unwrap_or_default();
    let resumed = !prior.is_empty();

    let args = args_for(&model, &prior, max_mode);
    let mut cmd = crate::proc_async::tokio_cmd(engine.bin());
    cmd.args(&args).current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", engine.bin()))?;
    let child_id = child.id();
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take();

    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    reg.live.lock().unwrap_or_else(|p| p.into_inner()).insert(pane_id, Live {
        stdin: tx, child_id, engine: "gemini".into(), cancel: cancel.clone(), on_stop: None,
        session_id: prior.clone(), model: model.clone(), busy: false, cost_usd: 0.0, started: now_ms(),
        min: false, lift: false, turn_started: 0, last_ask: String::new(), parent: String::new(),
        turn_tin: 0, turn_tout: 0, gate_retry: false, paused: false,
    });
    reg.emit(pane_id, json!({ "type": "antiloki", "subtype": if resumed { "resumed" } else { "started" },
        "session_id": prior, "model": model, "engine": "gemini", "pid": child_id }), true);

    // stdin: one NDJSON message per ask, in ANTIGRAVITY'S OWN input envelope — `{"event":"user","message":{…}}`.
    // Not Claude Code's `{"type":"user",…}`: that is rejected with `stream input message is missing the "event"
    // field`, and every other event name is rejected by name ("prompt", "turn", "input", "user_message" …).
    // Found by probing the CLI on 2026-09-28; `message` is the one field it insists on.
    tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            let line = input_line(&text);
            if stdin.write_all(format!("{line}\n").as_bytes()).await.is_err() { break }
            let _ = stdin.flush().await;
        }
    });
    if let Some(err) = stderr {
        let r = reg.clone();
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let l = l.trim().to_string();
                if l.is_empty() { continue }
                r.emit(pane_id, json!({ "type": "antiloki", "subtype": "note", "tone": "warn", "text": l.chars().take(400).collect::<String>() }), true);
            }
        });
    }
    // THE HANG HAS A FLOOR (audit 2026-09-28, and the module header's own known risk: issues #318 / #76 report
    // `agy` dropping stdout in a non-TTY, which is exactly how serve spawns it). If the process neither speaks
    // nor exits, the reader parks in `next_line()` for ever and `busy` never clears — the composer locks and the
    // only way out is Stop and restart. A watchdog ends the TURN, not the session: the conversation survives and
    // the operator gets a sentence they can act on.
    { let (reg3, cancel3) = (reg.clone(), cancel.clone());
      tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            if cancel3.load(std::sync::atomic::Ordering::Relaxed) { break }
            let stuck = { let l = reg3.live.lock().unwrap_or_else(|p| p.into_inner());
                match l.get(&pane_id) { Some(x) => x.busy && x.turn_started > 0 && now_ms() - x.turn_started > TURN_STALL_MS, None => break } };
            if !stuck { continue }
            { let mut l = reg3.live.lock().unwrap_or_else(|p| p.into_inner()); if let Some(x) = l.get_mut(&pane_id) { x.busy = false; } }
            reg3.emit(pane_id, json!({ "type": "result", "subtype": "error", "is_error": true, "num_turns": 0, "total_cost_usd": 0.0,
                "result": format!("the Antigravity CLI stopped answering after {} minutes and did not exit — the turn was ended so you can try again (agy issues #318/#76: `agy` can drop stdout when it is not attached to a terminal)", TURN_STALL_MS / 60_000) }), true);
        }
      }); }
    let (reg2, cwd2, model2) = (reg.clone(), cwd.to_string(), model.clone());
    tokio::spawn(async move {
        let mut turn = Turn::default();
        let mut conversation = prior;
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
            for (ev, persist) in translate(&mut turn, &model2, &mut conversation, &v) {
                let ty = s(ev.get("type"));
                if ty == "result" || ty == "assistant" { crate::agent::note_usage(Path::new(&cwd2), pane_id, &ev); }
                if ty == "assistant" { reg2.note_turn_tokens(pane_id, &ev); }
                if ty == "result" {
                    let cost = ev.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
                    { let mut live = reg2.live.lock().unwrap_or_else(|p| p.into_inner());
                      if let Some(x) = live.get_mut(&pane_id) { x.busy = false; x.cost_usd += cost; x.session_id = conversation.clone(); } }
                    reg2.on_turn_result(pane_id, &cwd2, &ev);
                }
                if ty == "system" && !conversation.is_empty() {
                    let _ = std::fs::create_dir_all(Path::new(&cwd2).join(".antiloki"));
                    let _ = std::fs::write(Path::new(&cwd2).join(".antiloki").join("pty-session.json"),
                        json!({ "engine": "gemini", "session_id": conversation, "t": now_ms(), "ui": "chat" }).to_string());
                }
                reg2.emit(pane_id, ev, persist);
            }
        }
        let code = child.wait().await.ok().and_then(|st| st.code());
        let was = reg2.live.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id).map(|x| x.child_id) == Some(child_id);
        if was { reg2.live.lock().unwrap_or_else(|p| p.into_inner()).remove(&pane_id); }
        reg2.emit(pane_id, json!({ "type": "antiloki", "subtype": "exited", "code": code, "cwd": cwd2 }), true);
    });
    Ok(json!({ "ok": true, "pane_id": pane_id, "session_id": "", "resumed": resumed, "model": model, "engine": "gemini", "pid": child_id }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the exact five lines `agy --output-format stream-json` printed here on 2026-09-28
    #[test]
    fn antigravitys_own_envelope_becomes_the_one_every_other_engine_speaks() {
        let mut t = Turn::default();
        let mut conv = String::new();
        let out = translate(&mut t, "gemini-3.8-flash", &mut conv, &json!({
            "event": "init", "conversation_id": "2102c87e", "init": { "cwd": "/box", "tools": ["grep_search"] } }));
        assert_eq!(conv, "2102c87e", "the conversation to resume comes off the init line");
        assert_eq!(out[0].0["type"], "system");

        // the echo of the ask draws nothing
        assert!(translate(&mut t, "gemini-3.8-flash", &mut conv, &json!({
            "event": "step_update", "step_update": { "step_index": 0, "state": "DONE", "step_type": "user_input" } })).is_empty());

        // a delta streams live and is never persisted
        let d = translate(&mut t, "gemini-3.8-flash", &mut conv, &json!({
            "event": "step_update", "step_update": { "step_index": 1, "state": "ACTIVE", "step_type": "agent_response", "text_delta": "ok" } }));
        assert_eq!(d.len(), 1); assert!(!d[0].1);
        assert_eq!(d[0].0["event"]["delta"]["text"], "ok");

        // the step closing lands the whole sentence as one assistant message, with the usage it reports
        let done = translate(&mut t, "gemini-3.8-flash", &mut conv, &json!({
            "event": "step_update", "step_update": { "step_index": 1, "state": "DONE", "step_type": "agent_response",
                "duration_seconds": 5.94, "usage": { "input_tokens": 13126, "output_tokens": 28, "thinking_tokens": 27, "cache_read_tokens": 0 } } }));
        assert!(done.iter().any(|(e, p)| e["type"] == "assistant" && e["message"]["content"][0]["text"] == "ok" && *p));

        let res = translate(&mut t, "gemini-3.8-flash", &mut conv, &json!({
            "event": "result", "result": { "status": "SUCCESS", "response": "ok\n", "duration_seconds": 6.05, "num_turns": 1,
                "usage": { "input_tokens": 13126, "output_tokens": 28 } } }));
        let r = &res.last().unwrap().0;
        assert_eq!(r["type"], "result"); assert_eq!(r["is_error"], false);
        assert_eq!(r["session_id"], "2102c87e");
        assert_eq!(r["usage"]["input_tokens"], 13126.0, "the step's own count is kept, not the summary's");
        assert_eq!(r["usage"]["output_tokens"], 55.0, "thinking tokens are output tokens — they are billed as such");
        assert!(r["total_cost_usd"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn two_steps_are_two_bubbles_and_a_failure_says_why() {
        let mut t = Turn::default(); let mut conv = "c".to_string();
        let step = |i: i64, st: &str, d: &str| json!({ "event": "step_update", "step_update": {
            "step_index": i, "state": st, "step_type": "agent_response", "text_delta": d } });
        translate(&mut t, "m", &mut conv, &step(1, "ACTIVE", "first"));
        let out = translate(&mut t, "m", &mut conv, &step(2, "ACTIVE", "second"));
        assert!(out.iter().any(|(e, _)| e["type"] == "assistant" && e["message"]["content"][0]["text"] == "first"),
                "a new step closes the one before it — otherwise a tool's output lands inside the sentence before it");

        let mut t = Turn::default();
        let res = translate(&mut t, "m", &mut conv, &json!({ "event": "result",
            "result": { "status": "ERROR", "error": "authentication required", "num_turns": 0 } }));
        let r = &res[0].0;
        assert_eq!(r["is_error"], true); assert_eq!(r["subtype"], "error");
        assert_eq!(r["result"], "authentication required");
    }

    #[test]
    fn the_ask_goes_in_antigravitys_own_input_envelope() {
        let v: Value = serde_json::from_str(&input_line("hi")).unwrap();
        assert_eq!(v["event"], "user", "`type` is rejected outright: the field it reads is `event`");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"], "hi");
    }
    #[test]
    fn the_session_is_one_process_for_the_whole_conversation() {
        let a = args_for("gemini-3.8-flash", "", false);
        assert!(a.windows(2).any(|w| w == ["--input-format", "stream-json"]), "one process, a turn per stdin line");
        assert!(a.windows(2).any(|w| w == ["--output-format", "stream-json"]));
        assert!(a.contains(&"--print=".into()), "bare --print eats the next flag; --print last fails outright");
        assert!(!a.contains(&"--conversation".into()));
        let a = args_for("gemini-3.8-flash", "2102c87e", true);
        assert!(a.windows(2).any(|w| w == ["--conversation", "2102c87e"]));
        assert!(a.contains(&"--dangerously-skip-permissions".into()));
    }
}
