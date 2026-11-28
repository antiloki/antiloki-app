//! OPENCODE ON THE CHAT SESSION (queue item 5, 2026-09-20): the same session model as agent.rs — one
//! conversation per pane, events persisted and broadcast, the pane's composer in front — over
//! `opencode serve`, OpenCode's own headless HTTP server. One shared server child for every OpenCode
//! pane (started on the first, `--port 0`, the port read off its banner); one OpenCode session per pane
//! created with `?directory=<worktree>` so the box's `opencode.json` (the pane's model, permissions,
//! MCP, the antiloki plugin) applies exactly as it does in the TUI. The pane's session id is pinned in
//! `.antiloki/pty-session.json` like Claude's, so a restart resumes the same conversation.
//!
//! The wire is OpenCode's bus (`GET /event?directory=`): `message.part.delta` text deltas,
//! `message.part.updated` parts (text · tool with its state · step-finish with the tokens),
//! `session.status` busy/idle, `session.error`, `permission.asked`. They are TRANSLATED into the
//! Claude-shaped events the chat already renders (`stream_event` · `assistant` · `user` tool results ·
//! `result`), so the FE has one event model. A `permission.asked` becomes `antiloki/permission` for
//! the pane to answer (`agent/permission`), unless the pane is MAX — then it is granted at once.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::agent::{note_usage, now_ms, AgentRegistry, Live};

fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }

// ── the shared server ─────────────────────────────────────────────────────────────────────────
struct Server { port: u16, child_id: Option<u32> }
static SERVER: OnceLock<Mutex<Option<Server>>> = OnceLock::new();
fn server_slot() -> &'static Mutex<Option<Server>> { SERVER.get_or_init(|| Mutex::new(None)) }
/// The port the operator pinned (`ANTILOKI_OPENCODE_PORT`) — an already-running `opencode serve` is used as is.
fn pinned_port() -> Option<u16> { std::env::var("ANTILOKI_OPENCODE_PORT").ok().and_then(|v| v.trim().parse().ok()) }
pub fn parse_banner(line: &str) -> Option<u16> {
    let i = line.find("listening on ")?;
    let url = line[i + 13..].trim();
    url.rsplit(':').next().and_then(|p| p.trim().trim_end_matches('/').parse().ok())
}
async fn healthy(port: u16) -> bool {
    reqwest::Client::new().get(format!("http://127.0.0.1:{port}/global/health")).timeout(std::time::Duration::from_secs(3)).send().await.map(|r| r.status().is_success()).unwrap_or(false)
}
/// The shared `opencode serve`: running → its port; gone → started, its banner read, health awaited.
async fn ensure_server() -> Result<u16, String> {
    if let Some(p) = pinned_port() { if healthy(p).await { return Ok(p); } }
    let known = server_slot().lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(|s| s.port);
    if let Some(p) = known { if healthy(p).await { return Ok(p); } *server_slot().lock().unwrap_or_else(|p| p.into_inner()) = None; }
    let mut cmd = crate::proc::tokio_cmd("opencode");
    cmd.args(["serve", "--port", "0", "--hostname", "127.0.0.1"]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(false);
    cmd.env_remove("CLAUDECODE");
    let mut child = cmd.spawn().map_err(|e| format!("opencode serve: {e}"))?;
    let child_id = child.id();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<u16>();
    // the banner lands on stdout ("opencode server listening on http://127.0.0.1:NNNN"); stderr carries the logs
    if let Some(out) = child.stdout.take() { let tx = tx.clone(); tokio::spawn(async move { use tokio::io::AsyncBufReadExt; let mut l = tokio::io::BufReader::new(out).lines(); while let Ok(Some(line)) = l.next_line().await { if let Some(p) = parse_banner(&line) { let _ = tx.send(p); } } }); }
    if let Some(err) = child.stderr.take() { tokio::spawn(async move { use tokio::io::AsyncBufReadExt; let mut l = tokio::io::BufReader::new(err).lines(); while let Ok(Some(line)) = l.next_line().await { if let Some(p) = parse_banner(&line) { let _ = tx.send(p); } else if line.contains("ERROR") { eprintln!("opencode serve: {line}"); } } }); }
    tokio::spawn(async move { let _ = child.wait().await; *server_slot().lock().unwrap_or_else(|p| p.into_inner()) = None; eprintln!("antiloki-serve: the shared opencode serve exited"); });
    let port = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv()).await.map_err(|_| "opencode serve did not announce its port in 30 s".to_string())?.ok_or("opencode serve exited before announcing its port")?;
    for _ in 0..40 { if healthy(port).await { break; } tokio::time::sleep(std::time::Duration::from_millis(250)).await; }
    *server_slot().lock().unwrap_or_else(|p| p.into_inner()) = Some(Server { port, child_id });
    eprintln!("antiloki-serve: opencode serve on :{port} (pid {child_id:?})");
    Ok(port)
}

// ── the translation: OpenCode's bus → the chat's events ───────────────────────────────────────
/// `opencode/deepseek-v4-pro` → the prompt's `model` object. Pure.
pub fn model_ref(m: &str) -> Option<Value> {
    let (p, id) = m.trim().split_once('/')?;
    if p.is_empty() || id.is_empty() { return None; }
    Some(json!({ "providerID": p, "modelID": id }))
}
/// Per pane, across one turn: which tool calls were announced, the turn's tokens, the text parts done.
#[derive(Default)]
pub struct Turn { pub announced: HashMap<String, bool>, pub done_text: HashMap<String, bool>, pub tokens_in: f64, pub tokens_out: f64, pub cache_read: f64, pub cache_write: f64, pub steps: u32, pub started: i64, pub cost: f64 }
/// One bus event → zero or more chat events `(event, persist)`. Only this session's. Pure over `turn`.
pub fn translate(turn: &mut Turn, session_id: &str, model: &str, ev: &Value) -> Vec<(Value, bool)> {
    let props = ev.get("properties").cloned().unwrap_or(json!({}));
    let sid = s(props.get("sessionID")).to_string();
    let sid = if sid.is_empty() { s(props.get("part").and_then(|p| p.get("sessionID"))) } else { sid };
    if !sid.is_empty() && sid != session_id { return vec![]; }
    let mut out = vec![];
    match s(ev.get("type")).as_str() {
        "message.part.delta" => {
            if s(props.get("field")) == "text" {
                let part = s(props.get("partID"));
                if !turn.done_text.contains_key(&part) { out.push((json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": s(props.get("delta")) } } }), false)); }
            }
        }
        "message.part.updated" => {
            let part = props.get("part").cloned().unwrap_or(json!({}));
            let pid = s(part.get("id"));
            match s(part.get("type")).as_str() {
                "text" => {
                    // a finished text part (it has an end time) becomes the assistant message; the user's own echo has no time
                    let text = s(part.get("text"));
                    let ended = part.get("time").and_then(|t| t.get("end")).is_some();
                    if ended && !text.trim().is_empty() && !turn.done_text.contains_key(&pid) {
                        turn.done_text.insert(pid, true);
                        out.push((json!({ "type": "assistant", "message": { "role": "assistant", "model": model, "content": [{ "type": "text", "text": text }] } }), true));
                    }
                }
                "tool" => {
                    let call = s(part.get("callID"));
                    let state = part.get("state").cloned().unwrap_or(json!({}));
                    let status = s(state.get("status"));
                    let input = state.get("input").cloned().unwrap_or(json!({}));
                    let has_input = input.as_object().map(|o| !o.is_empty()).unwrap_or(false);
                    if !turn.announced.contains_key(&call) && (status == "running" || status == "completed" || status == "error") && has_input {
                        turn.announced.insert(call.clone(), true);
                        out.push((json!({ "type": "assistant", "message": { "role": "assistant", "model": model, "content": [{ "type": "tool_use", "id": call, "name": s(part.get("tool")), "input": input }] } }), true));
                    }
                    if status == "completed" || status == "error" {
                        let output = if status == "error" { s(state.get("error")) } else { s(state.get("output")) };
                        let output: String = output.chars().take(4000).collect();
                        out.push((json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": call, "content": output, "is_error": status == "error" }] } }), true));
                    }
                }
                "step-finish" => {
                    let tk = part.get("tokens").cloned().unwrap_or(json!({}));
                    let n = |k: &str| tk.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                    turn.tokens_in += n("input"); turn.tokens_out += n("output") + n("reasoning");
                    turn.cache_read += tk.get("cache").and_then(|c| c.get("read")).and_then(Value::as_f64).unwrap_or(0.0);
                    turn.cache_write += tk.get("cache").and_then(|c| c.get("write")).and_then(Value::as_f64).unwrap_or(0.0);
                    turn.cost += part.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
                    turn.steps += 1;
                }
                _ => {}
            }
        }
        "session.idle" => {
            let usage = json!({ "input_tokens": turn.tokens_in, "output_tokens": turn.tokens_out, "cache_read_input_tokens": turn.cache_read, "cache_creation_input_tokens": turn.cache_write });
            out.push((json!({ "type": "result", "subtype": "success", "is_error": false, "session_id": session_id, "total_cost_usd": turn.cost, "duration_ms": if turn.started > 0 { now_ms() - turn.started } else { 0 }, "num_turns": turn.steps, "usage": usage, "modelUsage": { model: { "contextWindow": 128000 } } }), true));
            *turn = Turn::default();
        }
        "session.error" => {
            let err = props.get("error").cloned().unwrap_or(json!({}));
            let msg = s(err.get("data").and_then(|d| d.get("message"))); let msg = if msg.is_empty() { s(err.get("name")) } else { msg };
            out.push((json!({ "type": "result", "subtype": "error", "is_error": true, "result": msg, "session_id": session_id, "total_cost_usd": turn.cost, "num_turns": turn.steps, "usage": {} }), true));
            *turn = Turn::default();
        }
        "permission.asked" => {
            out.push((json!({ "type": "antiloki", "subtype": "permission", "id": s(props.get("id")), "permission": s(props.get("permission")), "patterns": props.get("patterns").cloned().unwrap_or(json!([])), "metadata": props.get("metadata").cloned().unwrap_or(json!({})) }), true));
        }
        _ => {}
    }
    out
}

// ── the session ───────────────────────────────────────────────────────────────────────────────
fn pinned_session(cwd: &str) -> Option<String> {
    antiloki_rs::cockpit__helpers_engine_handoff::pty_session(Path::new(cwd)).filter(|(e, _)| e == "opencode").map(|(_, id)| id)
}
async fn session_exists(client: &reqwest::Client, port: u16, cwd: &str, id: &str) -> bool {
    client.get(format!("http://127.0.0.1:{port}/session/{id}")).query(&[("directory", cwd)]).send().await.map(|r| r.status().is_success()).unwrap_or(false)
}
/// Start (or resume) the pane's OpenCode session — called by `agent::start` for an opencode pane.
pub async fn start(reg: &Arc<AgentRegistry>, pane_id: i64, pane: &Value, cwd: &str, model: Option<String>, fresh: bool) -> Result<Value, String> {
    let port = ensure_server().await?;
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let settings = pane.get("settings_json").and_then(Value::as_str).and_then(|t| serde_json::from_str::<Value>(t).ok()).unwrap_or(json!({}));
    let max_mode = settings.get("max").and_then(Value::as_bool).unwrap_or(false);
    // the model: the tier's, else the box's own default (opencode.json) — `opencode/deepseek-v4-pro` for mid/high
    let model = model.or_else(|| crate::cli::tier_model(crate::cli::Engine::Opencode, crate::cli::Tier::Mid)).unwrap_or_default();
    let prior = pinned_session(cwd).filter(|_| !fresh);
    let (sid, resumed) = match prior {
        Some(id) if session_exists(&client, port, cwd, &id).await => (id, true),
        _ => {
            let r = client.post(format!("{base}/session")).query(&[("directory", cwd)]).json(&json!({ "title": format!("antiloki pane {pane_id}") })).send().await.map_err(|e| format!("opencode session: {e}"))?;
            let v: Value = r.json().await.map_err(|e| format!("opencode session: {e}"))?;
            let id = s(v.get("id")); if id.is_empty() { return Err(format!("opencode session/create answered without an id: {v}")); }
            (id, false)
        }
    };
    let _ = std::fs::create_dir_all(Path::new(cwd).join(".antiloki"));
    let _ = std::fs::write(Path::new(cwd).join(".antiloki").join("pty-session.json"), json!({ "engine": "opencode", "session_id": sid, "t": now_ms(), "ui": "chat", "port": port }).to_string());
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let on_stop: Box<dyn FnOnce() + Send> = { let (c, b, sid2, cwd2) = (client.clone(), base.clone(), sid.clone(), cwd.to_string()); Box::new(move || { tokio::spawn(async move { let _ = c.post(format!("{b}/session/{sid2}/abort")).query(&[("directory", cwd2)]).send().await; }); }) };
    reg.live.lock().unwrap_or_else(|p| p.into_inner()).insert(pane_id, Live { stdin: tx, child_id: None, engine: "opencode".into(), cancel: cancel.clone(), on_stop: Some(on_stop), session_id: sid.clone(), model: model.clone(), busy: false, cost_usd: 0.0, started: now_ms(), min: crate::agent::pane_min(pane), lift: false, turn_started: 0, last_ask: String::new(), parent: crate::agent::pane_parent(pane), turn_tin: 0, turn_tout: 0, gate_retry: false, paused: false });
    reg.emit(pane_id, json!({ "type": "antiloki", "subtype": if resumed { "resumed" } else { "started" }, "session_id": sid, "model": model, "engine": "opencode", "port": port }), true);
    // the sender: each text → prompt_async on the session (the model named on every prompt)
    { let (c, b, sid2, cwd2, m, reg2, cancel2) = (client.clone(), base.clone(), sid.clone(), cwd.to_string(), model.clone(), reg.clone(), cancel.clone());
      tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if cancel2.load(Ordering::SeqCst) { break; }
            let mut body = json!({ "parts": [{ "type": "text", "text": text }] });
            if let Some(mr) = model_ref(&m) { body["model"] = mr; }
            let r = c.post(format!("{b}/session/{sid2}/prompt_async")).query(&[("directory", cwd2.as_str())]).json(&body).send().await;
            match r { Ok(r) if r.status().is_success() => {}, Ok(r) => { let t = r.text().await.unwrap_or_default(); reg2.emit(pane_id, json!({ "type": "antiloki", "subtype": "stderr", "text": format!("opencode refused the prompt: {t}") }), true); { let mut l = reg2.live.lock().unwrap_or_else(|p| p.into_inner()); if let Some(x) = l.get_mut(&pane_id) { x.busy = false; } } }, Err(e) => { reg2.emit(pane_id, json!({ "type": "antiloki", "subtype": "stderr", "text": format!("opencode unreachable: {e}") }), true); } }
        }
      }); }
    // the follower: the bus for this directory, this session's events translated
    { let (c, b, sid2, cwd2, m, reg2, cancel2) = (client.clone(), base.clone(), sid.clone(), cwd.to_string(), model.clone(), reg.clone(), cancel.clone());
      tokio::spawn(async move {
        use futures_util::StreamExt;
        let mut turn = Turn::default();
        'outer: loop {
            if cancel2.load(Ordering::SeqCst) { break; }
            let resp = match c.get(format!("{b}/event")).query(&[("directory", cwd2.as_str())]).send().await { Ok(r) => r, Err(_) => { tokio::time::sleep(std::time::Duration::from_secs(2)).await; if cancel2.load(Ordering::SeqCst) { break; } continue; } };
            let mut stream = resp.bytes_stream();
            let mut buf = String::new();
            while let Some(chunk) = stream.next().await {
                if cancel2.load(Ordering::SeqCst) { break 'outer; }
                let Ok(bytes) = chunk else { break };
                buf.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(i) = buf.find('\n') {
                    let line = buf[..i].trim_end_matches('\r').to_string(); buf.drain(..=i);
                    let Some(data) = line.strip_prefix("data:") else { continue };
                    let Ok(ev) = serde_json::from_str::<Value>(data.trim()) else { continue };
                    let ty = s(ev.get("type"));
                    if ty == "session.status" { let st = s(ev.get("properties").and_then(|p| p.get("status")).and_then(|x| x.get("type"))); let same = s(ev.get("properties").and_then(|p| p.get("sessionID"))) == sid2; if same { let mut l = reg2.live.lock().unwrap_or_else(|p| p.into_inner()); if let Some(x) = l.get_mut(&pane_id) { if st == "busy" { x.busy = true; if turn.started == 0 { turn.started = now_ms(); } } } } }
                    for (out, persist) in translate(&mut turn, &sid2, &m, &ev) {
                        let oty = s(out.get("type"));
                        if matches!(oty.as_str(), "assistant" | "result") { note_usage(Path::new(&cwd2), pane_id, &out); }
                        if oty == "result" {
                            let cost = out.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
                                  { let mut l = reg2.live.lock().unwrap_or_else(|p| p.into_inner()); if let Some(x) = l.get_mut(&pane_id) { x.busy = false; x.cost_usd += cost; } }
                            reg2.on_turn_result(pane_id, &cwd2, &out);
                        }
                        // MAX grants every permission at once; otherwise the pane answers
                        if oty == "antiloki" && s(out.get("subtype")) == "permission" && max_mode {
                            let id = s(out.get("id")); let (c2, b2, cwd3) = (c.clone(), b.clone(), cwd2.clone());
                            tokio::spawn(async move { let _ = c2.post(format!("{b2}/permission/{id}/reply")).query(&[("directory", cwd3)]).json(&json!({ "reply": "always" })).send().await; });
                            continue;
                        }
                        reg2.emit(pane_id, out, persist);
                    }
                }
            }
            if cancel2.load(Ordering::SeqCst) { break; }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await; // the bus dropped — reattach
        }
      }); }
    Ok(json!({ "ok": true, "pane_id": pane_id, "session_id": sid, "resumed": resumed, "model": model, "engine": "opencode", "port": port }))
}
/// `POST /api/v1/cockpit/agent/permission {pane_id, id, reply: once|always|reject}` — the pane's answer.
pub async fn permission(reg: Arc<AgentRegistry>, body: axum::body::Bytes) -> axum::response::Response {
    use axum::response::IntoResponse;
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let pane_id = b.get("pane_id").and_then(Value::as_i64).unwrap_or(0);
    let (id, reply) = (s(b.get("id")), s(b.get("reply")));
    let reply = if matches!(reply.as_str(), "once" | "always" | "reject") { reply } else { "once".to_string() };
    let cwd = reg.root_of(pane_id);
    let port = server_slot().lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(|x| x.port).or_else(pinned_port);
    let (Some(cwd), Some(port)) = (cwd, port) else { return (axum::http::StatusCode::CREATED, axum::Json(json!({ "ok": false, "error": "no opencode session for this pane" }))).into_response() };
    let r = reqwest::Client::new().post(format!("http://127.0.0.1:{port}/permission/{id}/reply")).query(&[("directory", cwd.as_str())]).json(&json!({ "reply": reply })).send().await;
    let ok = r.map(|r| r.status().is_success()).unwrap_or(false);
    if ok { reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "permission_reply", "id": id, "reply": reply }), true); }
    (axum::http::StatusCode::CREATED, axum::Json(json!({ "ok": ok }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn banner_and_model() {
        assert_eq!(parse_banner("opencode server listening on http://127.0.0.1:4096"), Some(4096));
        assert_eq!(parse_banner("opencode server listening on http://127.0.0.1:41234/"), Some(41234));
        assert_eq!(parse_banner("Warning: OPENCODE_SERVER_PASSWORD is not set"), None);
        assert_eq!(model_ref("opencode/deepseek-v4-pro"), Some(json!({ "providerID": "opencode", "modelID": "deepseek-v4-pro" })));
        assert_eq!(model_ref("sonnet"), None);
    }
    #[test]
    fn the_bus_becomes_the_chats_events() {
        let sid = "ses_1"; let mut t = Turn::default();
        let ev = |v: Value| v;
        // another session's event is nobody's business
        assert!(translate(&mut t, sid, "opencode/x", &ev(json!({ "type": "message.part.delta", "properties": { "sessionID": "ses_2", "field": "text", "delta": "no" } }))).is_empty());
        let d = translate(&mut t, sid, "opencode/x", &ev(json!({ "type": "message.part.delta", "properties": { "sessionID": sid, "partID": "p1", "field": "text", "delta": "The project" } })));
        assert_eq!(d.len(), 1); assert_eq!(d[0].1, false); assert_eq!(d[0].0["event"]["delta"]["text"], "The project");
        // the tool: announced once with its input, resolved with its output
        let tool = |status: &str, extra: Value| { let mut st = json!({ "status": status, "input": { "filePath": "/w/package.json" } }); if let Some(o) = extra.as_object() { for (k, v) in o { st[k] = v.clone(); } } json!({ "type": "message.part.updated", "properties": { "part": { "id": "p2", "sessionID": sid, "type": "tool", "tool": "read", "callID": "call_1", "state": st } } }) };
        assert!(translate(&mut t, sid, "opencode/x", &json!({ "type": "message.part.updated", "properties": { "part": { "id": "p2", "sessionID": sid, "type": "tool", "tool": "read", "callID": "call_1", "state": { "status": "pending", "input": {} } } } })).is_empty(), "pending without input: nothing yet");
        let r = translate(&mut t, sid, "opencode/x", &tool("running", json!({})));
        assert_eq!(r.len(), 1); assert_eq!(r[0].0["message"]["content"][0]["type"], "tool_use"); assert_eq!(r[0].0["message"]["content"][0]["id"], "call_1"); assert_eq!(r[0].0["message"]["content"][0]["name"], "read");
        let r = translate(&mut t, sid, "opencode/x", &tool("completed", json!({ "output": "1: {\n2: \"name\": \"ministore\"" })));
        assert_eq!(r.len(), 1, "announced already — only the result"); assert_eq!(r[0].0["type"], "user"); assert_eq!(r[0].0["message"]["content"][0]["tool_use_id"], "call_1"); assert_eq!(r[0].0["message"]["content"][0]["is_error"], false);
        // the finished text part is the assistant message; its deltas stop afterwards
        let r = translate(&mut t, sid, "opencode/x", &json!({ "type": "message.part.updated", "properties": { "part": { "id": "p1", "sessionID": sid, "type": "text", "text": "The project is ministore.", "time": { "start": 1, "end": 2 } } } }));
        assert_eq!(r.len(), 1); assert_eq!(r[0].0["type"], "assistant"); assert_eq!(r[0].0["message"]["content"][0]["text"], "The project is ministore."); assert_eq!(r[0].0["message"]["model"], "opencode/x");
        assert!(translate(&mut t, sid, "opencode/x", &json!({ "type": "message.part.delta", "properties": { "sessionID": sid, "partID": "p1", "field": "text", "delta": "late" } })).is_empty());
        // steps add up; idle closes the turn as a result
        translate(&mut t, sid, "opencode/x", &json!({ "type": "message.part.updated", "properties": { "part": { "id": "p3", "sessionID": sid, "type": "step-finish", "tokens": { "input": 6874, "output": 56, "reasoning": 0, "cache": { "read": 1792, "write": 0 } }, "cost": 0.001 } } }));
        translate(&mut t, sid, "opencode/x", &json!({ "type": "message.part.updated", "properties": { "part": { "id": "p4", "sessionID": sid, "type": "step-finish", "tokens": { "input": 327, "output": 10, "reasoning": 4, "cache": { "read": 8704, "write": 0 } }, "cost": 0.0003 } } }));
        let r = translate(&mut t, sid, "opencode/x", &json!({ "type": "session.idle", "properties": { "sessionID": sid } }));
        assert_eq!(r.len(), 1); let res = &r[0].0; assert_eq!(res["type"], "result"); assert_eq!(res["usage"]["input_tokens"], 7201.0); assert_eq!(res["usage"]["output_tokens"], 70.0); assert_eq!(res["usage"]["cache_read_input_tokens"], 10496.0); assert_eq!(res["num_turns"], 2); assert!((res["total_cost_usd"].as_f64().unwrap() - 0.0013).abs() < 1e-9);
        assert_eq!(t.steps, 0, "the turn resets");
        // an error closes the turn as an error result; a permission is the pane's to answer
        let r = translate(&mut t, sid, "opencode/x", &json!({ "type": "session.error", "properties": { "sessionID": sid, "error": { "name": "ProviderAuthError", "data": { "message": "not logged in" } } } }));
        assert_eq!(r[0].0["is_error"], true); assert_eq!(r[0].0["result"], "not logged in");
        let r = translate(&mut t, sid, "opencode/x", &json!({ "type": "permission.asked", "properties": { "id": "per_1", "sessionID": sid, "permission": "bash", "patterns": ["npm test"], "metadata": {} } }));
        assert_eq!(r[0].0["subtype"], "permission"); assert_eq!(r[0].0["patterns"][0], "npm test");
    }
}
