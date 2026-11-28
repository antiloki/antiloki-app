//! CODEX ON THE CHAT SESSION (queue item 5, 2026-09-20): the same session model as agent.rs — one
//! conversation per pane, events persisted and broadcast, the pane's composer in front — over
//! `codex app-server`, Codex's own JSON-RPC server (stdio, one JSON object per line, no `jsonrpc`
//! key; shapes in `codex app-server generate-ts`). One app-server child PER PANE, started with the
//! pane's own `-c` overrides (the antiloki MCP server from the bake, its tools pre-approved) so the
//! box's context reaches the model exactly as the TUI's did; one thread per pane, `thread/start` in
//! the box (`cwd`) or `thread/resume` on the pinned id in `.antiloki/pty-session.json` so a restart
//! continues the same conversation. The tiers are REASONING EFFORT on the account's model (a
//! subscription refuses other models): `agent/send {tier}` sets the effort of the next turn.
//!
//! The wire: `turn/start` per send; notifications `item/agentMessage/delta` (text deltas),
//! `item/started` / `item/completed` (agentMessage · commandExecution · fileChange · mcpToolCall ·
//! reasoning · plan), `thread/tokenUsage/updated`, `error`, `turn/completed`, `account/rateLimits/updated`
//! — TRANSLATED into the Claude-shaped events the chat already renders (`stream_event` · `assistant`
//! · `user` tool results · `result` · `rate_limit_event`), one event model for the FE. A server
//! REQUEST (`item/commandExecution/requestApproval`, `item/fileChange/requestApproval`,
//! `item/permissions/requestApproval`) becomes an `antiloki/permission` card the pane answers
//! (`agent/permission` → the JSON-RPC response), unless the pane is MAX — then the thread runs with
//! `approvalPolicy: never` + full access and nothing is ever asked. A usage-limit failure marks the box
//! (`quota.json`) so the pane's banner reads it, the same as a headless run's.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use crate::agent::{note_usage, now_ms, AgentRegistry, Live};

fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }

// ── the connection: one app-server per pane ──────────────────────────────────────────────────
pub(crate) struct Conn {
    /// raw lines to the app-server's stdin (requests, notifications, and our responses to its requests)
    out: tokio::sync::mpsc::UnboundedSender<String>,
    next_id: AtomicI64,
    pending: Mutex<HashMap<i64, tokio::sync::oneshot::Sender<Result<Value, String>>>>,
    /// server requests waiting for the pane's answer: our permission id → (the rpc id, the method, what was asked)
    asks: Mutex<HashMap<String, (Value, String, Value)>>,
    pub(crate) thread_id: Mutex<String>,
    /// the next turn's reasoning effort (the router's tier); None = the account's default
    pub(crate) effort: Mutex<Option<String>>,
}
impl Conn {
    fn send_raw(&self, v: &Value) -> bool { self.out.send(v.to_string()).is_ok() }
    /// A request → its `result`, or the `error.message`.
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.lock().unwrap_or_else(|p| p.into_inner()).insert(id, tx);
        if !self.send_raw(&json!({ "id": id, "method": method, "params": params })) { self.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&id); return Err("codex app-server is gone".into()); }
        match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("codex app-server closed before answering".into()),
            Err(_) => { self.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&id); Err(format!("codex app-server: no answer to {method} in 60 s")) }
        }
    }
    fn notify(&self, method: &str, params: Value) { let _ = self.send_raw(&json!({ "method": method, "params": params })); }
    fn respond(&self, id: &Value, result: Value) { let _ = self.send_raw(&json!({ "id": id, "result": result })); }
    /// A response line from the server: routed to the waiting request. True when it was one.
    fn settle(&self, msg: &Value) -> bool {
        let Some(id) = msg.get("id").and_then(Value::as_i64) else { return false };
        if msg.get("method").is_some() { return false; } // a server request carries an id too
        let Some(tx) = self.pending.lock().unwrap_or_else(|p| p.into_inner()).remove(&id) else { return true };
        let r = match msg.get("error") { Some(e) => Err(s(e.get("message"))), None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)) };
        let _ = tx.send(r);
        true
    }
}
static CONNS: OnceLock<Mutex<HashMap<i64, Arc<Conn>>>> = OnceLock::new();
fn conns() -> &'static Mutex<HashMap<i64, Arc<Conn>>> { CONNS.get_or_init(|| Mutex::new(HashMap::new())) }
pub(crate) fn conn_of(pane_id: i64) -> Option<Arc<Conn>> { conns().lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id).cloned() }
/// The router's tier for this pane's next turns → the reasoning effort (`low` · `medium` · `high`). Pure mapping.
pub fn effort_of(tier: crate::cli::Tier) -> &'static str { match tier { crate::cli::Tier::Low => "low", crate::cli::Tier::Mid => "medium", crate::cli::Tier::High => "high" } }
pub fn set_effort(pane_id: i64, tier: crate::cli::Tier) -> bool {
    match conn_of(pane_id) { Some(c) => { *c.effort.lock().unwrap_or_else(|p| p.into_inner()) = Some(effort_of(tier).into()); true }, None => false }
}

// ── the translation: app-server notifications → the chat's events ────────────────────────────
/// Per pane, across one turn: the tool calls announced, the last token usage, the turn's start.
#[derive(Default)]
pub struct Turn { pub announced: HashMap<String, bool>, pub usage: Option<Value>, pub context_window: Option<f64>, pub started: i64, pub tools: u32, pub effort: Option<String> }

/// The model the chat shows for a Codex thread: the account's model, with the effort when one is set. Pure.
pub fn model_label(model: &str, effort: Option<&str>) -> String { match effort { Some(e) if !e.is_empty() => format!("{model} · {e}"), _ => model.to_string() } }

/// A tool call announced as the chat's `tool_use`, from an item that just started. Pure.
fn tool_use_of(item: &Value) -> Option<(String, String, Value)> {
    let id = s(item.get("id"));
    match s(item.get("type")).as_str() {
        "commandExecution" => Some((id, "bash".into(), json!({ "command": s(item.get("command")), "cwd": s(item.get("cwd")) }))),
        "fileChange" => Some((id, "apply_patch".into(), json!({ "files": item.get("changes").and_then(Value::as_array).map(|c| c.iter().map(|x| json!({ "path": s(x.get("path")), "kind": s(x.get("kind")) })).collect::<Vec<_>>()).unwrap_or_default() }))),
        "mcpToolCall" => Some((id, format!("{}/{}", s(item.get("server")), s(item.get("tool"))), item.get("arguments").cloned().unwrap_or(json!({})))),
        "dynamicToolCall" => Some((id, s(item.get("tool")), item.get("arguments").cloned().unwrap_or(json!({})))),
        "webSearch" => Some((id, "web_search".into(), json!({ "query": s(item.get("query")) }))),
        _ => None,
    }
}
/// The finished tool item as the chat's `tool_result` (content, is_error). Pure.
fn tool_result_of(item: &Value) -> Option<(String, bool)> {
    match s(item.get("type")).as_str() {
        "commandExecution" => {
            let code = item.get("exitCode").and_then(Value::as_i64);
            let status = s(item.get("status"));
            let out = s(item.get("aggregatedOutput"));
            let out = if out.is_empty() { format!("(exit {}) {status}", code.map(|c| c.to_string()).unwrap_or_else(|| "?".into())) } else { out };
            Some((out, status == "failed" || status == "declined" || code.map(|c| c != 0).unwrap_or(false)))
        }
        "fileChange" => {
            let status = s(item.get("status"));
            let n = item.get("changes").and_then(Value::as_array).map(|c| c.len()).unwrap_or(0);
            let paths = item.get("changes").and_then(Value::as_array).map(|c| c.iter().map(|x| s(x.get("path"))).collect::<Vec<_>>().join("\n")).unwrap_or_default();
            Some((format!("{status}: {n} file(s)\n{paths}"), status == "failed" || status == "declined"))
        }
        "mcpToolCall" => {
            let err = item.get("error").filter(|e| !e.is_null()).map(|e| s(e.get("message")).chars().take(4000).collect::<String>());
            match err { Some(e) if !e.is_empty() => Some((e, true)), _ => Some((item.get("result").map(|r| r.to_string()).unwrap_or_default().chars().take(4000).collect(), s(item.get("status")) == "failed")) }
        }
        "dynamicToolCall" => Some((item.get("contentItems").map(|c| c.to_string()).unwrap_or_default().chars().take(4000).collect(), item.get("success") == Some(&json!(false)))),
        "webSearch" => Some((format!("searched: {}", s(item.get("query"))), false)),
        _ => None,
    }
}
/// The turn's usage in Claude's shape (input excludes the cache reads, which are their own number). Pure.
pub fn usage_of(last: &Value) -> Value {
    let n = |k: &str| last.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    let cached = n("cachedInputTokens");
    json!({ "input_tokens": (n("inputTokens") - cached).max(0.0), "output_tokens": n("outputTokens"), "cache_read_input_tokens": cached, "cache_creation_input_tokens": n("cacheWriteInputTokens") })
}
/// The API-equivalent price of a turn on the subscription — Codex's list price for the effort's tier
/// (cache reads at a tenth), the yardstick Heimdall's savings use everywhere. Pure.
pub fn price_of(usage: &Value, effort: Option<&str>) -> f64 {
    let tier = match effort { Some("high") => crate::cli::Tier::High, Some("low") => crate::cli::Tier::Low, _ => crate::cli::Tier::Mid };
    let (_, pin, pout) = crate::cli::list_price(crate::cli::Engine::Codex, tier);
    let n = |k: &str| usage.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    (n("input_tokens") * pin + n("cache_read_input_tokens") * pin * 0.1 + n("cache_creation_input_tokens") * pin * 1.25 + n("output_tokens") * pout) / 1e6
}
/// One notification → zero or more chat events `(event, persist)`. Only this thread's. Pure over `turn`.
pub fn translate(turn: &mut Turn, thread_id: &str, model: &str, msg: &Value) -> Vec<(Value, bool)> {
    let method = s(msg.get("method"));
    let p = msg.get("params").cloned().unwrap_or(json!({}));
    let tid = s(p.get("threadId"));
    if !tid.is_empty() && tid != thread_id { return vec![]; }
    let label = model_label(model, turn.effort.as_deref());
    let mut out = vec![];
    match method.as_str() {
        "turn/started" => { turn.started = now_ms(); }
        "item/agentMessage/delta" => out.push((json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": s(p.get("delta")) } } }), false)),
        "item/started" => {
            let item = p.get("item").cloned().unwrap_or(json!({}));
            if let Some((id, name, input)) = tool_use_of(&item) {
                if !turn.announced.contains_key(&id) { turn.announced.insert(id.clone(), true); turn.tools += 1; out.push((json!({ "type": "assistant", "message": { "role": "assistant", "model": label, "content": [{ "type": "tool_use", "id": id, "name": name, "input": input }] } }), true)); }
            }
        }
        "item/completed" => {
            let item = p.get("item").cloned().unwrap_or(json!({}));
            match s(item.get("type")).as_str() {
                "agentMessage" => { let text = s(item.get("text")); if !text.trim().is_empty() { out.push((json!({ "type": "assistant", "message": { "role": "assistant", "model": label, "content": [{ "type": "text", "text": text }] } }), true)); } }
                "plan" => { let text = s(item.get("text")); if !text.trim().is_empty() { out.push((json!({ "type": "assistant", "message": { "role": "assistant", "model": label, "content": [{ "type": "text", "text": format!("**Plan**\n\n{text}") }] } }), true)); } }
                "reasoning" => {
                    let summary = item.get("summary").and_then(Value::as_array).map(|a| a.iter().map(|x| s(Some(x))).collect::<Vec<_>>().join("\n")).unwrap_or_default();
                    if !summary.trim().is_empty() { out.push((json!({ "type": "assistant", "message": { "role": "assistant", "model": label, "content": [{ "type": "thinking", "thinking": summary }] } }), true)); }
                }
                "userMessage" | "contextCompaction" | "enteredReviewMode" | "exitedReviewMode" | "hookPrompt" | "sleep" => {}
                _ => {
                    // a tool: announced now if `item/started` never came, then resolved
                    if let Some((id, name, input)) = tool_use_of(&item) {
                        if !turn.announced.contains_key(&id) { turn.announced.insert(id.clone(), true); turn.tools += 1; out.push((json!({ "type": "assistant", "message": { "role": "assistant", "model": label, "content": [{ "type": "tool_use", "id": id.clone(), "name": name, "input": input }] } }), true)); }
                        if let Some((content, is_error)) = tool_result_of(&item) { out.push((json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": id, "content": content.chars().take(4000).collect::<String>(), "is_error": is_error }] } }), true)); }
                    }
                }
            }
        }
        "thread/tokenUsage/updated" => {
            let tu = p.get("tokenUsage").cloned().unwrap_or(json!({}));
            turn.usage = tu.get("last").cloned();
            turn.context_window = tu.get("modelContextWindow").and_then(Value::as_f64);
        }
        "error" => {
            let e = p.get("error").cloned().unwrap_or(json!({}));
            let will_retry = p.get("willRetry").and_then(Value::as_bool).unwrap_or(false);
            let text = format!("{}{}", s(e.get("message")), if will_retry { " (retrying)" } else { "" });
            if will_retry { out.push((json!({ "type": "antiloki", "subtype": "stderr", "text": text }), true)); }
        }
        "turn/completed" => {
            let t = p.get("turn").cloned().unwrap_or(json!({}));
            let status = s(t.get("status"));
            let usage = turn.usage.as_ref().map(usage_of).unwrap_or(json!({}));
            let cost = price_of(&usage, turn.effort.as_deref());
            let ms = t.get("durationMs").and_then(Value::as_i64).unwrap_or(if turn.started > 0 { now_ms() - turn.started } else { 0 });
            let mut ev = json!({ "type": "result", "subtype": if status == "failed" { "error" } else { "success" }, "is_error": status == "failed", "session_id": thread_id, "engine": "codex", "total_cost_usd": cost, "duration_ms": ms, "num_turns": turn.tools + 1, "usage": usage, "modelUsage": { label.clone(): { "contextWindow": turn.context_window.unwrap_or(258_000.0) } } });
            if status == "failed" { let e = t.get("error").cloned().unwrap_or(json!({})); ev["result"] = json!(s(e.get("message"))); ev["error_info"] = e.get("codexErrorInfo").cloned().unwrap_or(Value::Null); }
            if status == "interrupted" { ev["result"] = json!("interrupted"); }
            out.push((ev, true));
            let effort = turn.effort.take();
            *turn = Turn { effort, ..Turn::default() };
        }
        "account/rateLimits/updated" => {
            let rl = p.get("rateLimits").cloned().unwrap_or(json!({}));
            let win = |w: Option<&Value>| w.filter(|w| !w.is_null()).map(|w| json!({ "utilization": w.get("usedPercent").and_then(Value::as_f64).unwrap_or(0.0) / 100.0, "resetsAt": w.get("resetsAt").cloned().unwrap_or(Value::Null) }));
            let (five, week) = (win(rl.get("primary")), win(rl.get("secondary")));
            if five.is_some() || week.is_some() {
                let mut wins = json!({}); if let Some(f) = five { wins["five_hour"] = f; } if let Some(w) = week { wins["seven_day"] = w; }
                out.push((json!({ "type": "rate_limit_event", "engine": "codex", "rate_limit_info": { "unifiedWindows": wins } }), true));
            }
        }
        _ => {}
    }
    out
}
/// A server request that needs the pane's answer → the permission card `(our id, card)`; None when
/// it is answered on the spot (`auto` says how). Pure.
pub fn approval_card(msg: &Value) -> Option<(String, Value)> {
    let method = s(msg.get("method"));
    let p = msg.get("params").cloned().unwrap_or(json!({}));
    let rid = msg.get("id").map(|i| i.to_string()).unwrap_or_default();
    let (permission, patterns, meta) = match method.as_str() {
        "item/commandExecution/requestApproval" | "execCommandApproval" => ("bash", vec![s(p.get("command"))], json!({ "reason": p.get("reason").cloned().unwrap_or(Value::Null), "cwd": s(p.get("cwd")) })),
        "item/fileChange/requestApproval" | "applyPatchApproval" => ("edit", vec![s(p.get("grantRoot"))].into_iter().filter(|x| !x.is_empty()).collect(), json!({ "reason": p.get("reason").cloned().unwrap_or(Value::Null) })),
        "item/permissions/requestApproval" => ("permissions", vec![p.get("permissions").map(|x| x.to_string()).unwrap_or_default()], json!({ "reason": p.get("reason").cloned().unwrap_or(Value::Null), "cwd": s(p.get("cwd")) })),
        _ => return None,
    };
    let id = format!("codex-{rid}");
    Some((id.clone(), json!({ "type": "antiloki", "subtype": "permission", "id": id, "permission": permission, "patterns": patterns, "metadata": meta })))
}
/// The pane's `once | always | reject` → the method's response body. Pure.
pub fn decision_of(method: &str, reply: &str, asked: &Value) -> Value {
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => json!({ "decision": match reply { "always" => "acceptForSession", "reject" => "decline", _ => "accept" } }),
        "execCommandApproval" | "applyPatchApproval" => json!({ "decision": match reply { "always" => "approved_for_session", "reject" => "denied", _ => "approved" } }),
        "item/permissions/requestApproval" => {
            let granted = if reply == "reject" { json!({}) } else { asked.get("permissions").cloned().unwrap_or(json!({})) };
            json!({ "permissions": granted, "scope": if reply == "always" { "session" } else { "turn" } })
        }
        _ => json!({}),
    }
}
/// What a server request gets when nobody can answer it (no pane UI for it): a harmless response. Pure.
pub fn auto_response(method: &str, params: &Value) -> Option<Value> {
    match method {
        "item/tool/requestUserInput" => Some(json!({ "answers": {} })),
        "mcpServer/elicitation/request" => Some(json!({ "action": "decline", "content": null, "_meta": null })),
        "account/chatgptAuthTokens/refresh" => Some(json!({})),
        "attestation/generate" => Some(json!({})),
        "item/tool/call" => Some(json!({ "success": false, "contentItems": [{ "type": "inputText", "text": format!("antiloki: no dynamic tool host ({})", s(params.get("tool"))) }] })),
        _ => None,
    }
}

// ── the session ───────────────────────────────────────────────────────────────────────────────
fn pinned_thread(cwd: &str) -> Option<String> {
    antiloki_rs::cockpit__helpers_engine_handoff::pty_session(Path::new(cwd)).filter(|(e, id)| e == "codex" && !id.is_empty()).map(|(_, id)| id)
}
/// Start (or resume) the pane's Codex thread — called by `agent::start` for a codex pane.
pub async fn start(reg: &Arc<AgentRegistry>, pane_id: i64, pane: &Value, cwd: &str, pack: &Value, tier: Option<crate::cli::Tier>, fresh: bool) -> Result<Value, String> {
    let settings = pane.get("settings_json").and_then(Value::as_str).and_then(|t| serde_json::from_str::<Value>(t).ok()).unwrap_or(json!({}));
    let max_mode = settings.get("max").and_then(Value::as_bool).unwrap_or(false);
    // the box is trusted (the app-server reads ~/.codex/config.toml like the TUI)
    crate::pty::codex_trust(&crate::pty::codex_trust_paths(cwd));
    let mut args: Vec<String> = vec!["app-server".into()];
    args.extend(crate::agents_http::context_args_for("codex", pack, &crate::agents_http::Delegation::of_pane(pane)));
    crate::agents_http::write_launch_record(cwd, "codex", &args, &HashMap::new());
    let mut cmd = crate::proc::tokio_cmd("codex");
    cmd.args(&args).current_dir(cwd).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(false);
    cmd.env_remove("CLAUDECODE");
    let mut child = cmd.spawn().map_err(|e| format!("codex app-server: {e}"))?;
    let child_id = child.id();
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let conn = Arc::new(Conn { out: out_tx, next_id: AtomicI64::new(1), pending: Mutex::new(HashMap::new()), asks: Mutex::new(HashMap::new()), thread_id: Mutex::new(String::new()), effort: Mutex::new(tier.map(|t| effort_of(t).to_string())) });
    conns().lock().unwrap_or_else(|p| p.into_inner()).insert(pane_id, conn.clone());
    // stdin pump: every line the connection sends
    tokio::spawn(async move { while let Some(line) = out_rx.recv().await { if stdin.write_all(line.as_bytes()).await.is_err() || stdin.write_all(b"\n").await.is_err() { break; } } });
    if let Some(err) = stderr { let (reg2, pid) = (reg.clone(), pane_id); tokio::spawn(async move { let mut lines = tokio::io::BufReader::new(err).lines(); while let Ok(Some(l)) = lines.next_line().await { let t = l.trim(); if !t.is_empty() && !t.starts_with("mise ") { reg2.emit(pid, json!({ "type": "antiloki", "subtype": "stderr", "text": t }), true); } } }); }
    // the reader: responses settle requests; server requests become cards (or are auto-answered); notifications translate
    let cancel = Arc::new(AtomicBool::new(false));
    let (reg2, conn2, cancel2, cwd2, pid) = (reg.clone(), conn.clone(), cancel.clone(), cwd.to_string(), pane_id);
    let model_slot = Arc::new(Mutex::new(String::from("codex")));
    let model2 = model_slot.clone();
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        let mut turn = Turn::default();
        while let Ok(Some(l)) = lines.next_line().await {
            if cancel2.load(Ordering::SeqCst) { break; }
            let Ok(msg) = serde_json::from_str::<Value>(&l) else { continue };
            if conn2.settle(&msg) { continue; }
            let method = s(msg.get("method"));
            if msg.get("id").is_some() {
                // a server request
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                if let Some(r) = auto_response(&method, &params) { conn2.respond(&msg["id"], r); continue; }
                if let Some((id, card)) = approval_card(&msg) {
                    if max_mode { conn2.respond(&msg["id"], decision_of(&method, "always", &params)); continue; }
                    conn2.asks.lock().unwrap_or_else(|p| p.into_inner()).insert(id, (msg["id"].clone(), method.clone(), params));
                    reg2.emit(pid, card, true);
                    continue;
                }
                conn2.respond(&msg["id"], json!({}));
                continue;
            }
            let thread_id = conn2.thread_id.lock().unwrap_or_else(|p| p.into_inner()).clone();
            if method == "thread/settings/updated" {
                let m = s(msg.get("params").and_then(|p| p.get("threadSettings")).and_then(|t| t.get("model")));
                if !m.is_empty() { *model2.lock().unwrap_or_else(|p| p.into_inner()) = m.clone(); let mut l = reg2.live.lock().unwrap_or_else(|p| p.into_inner()); if let Some(x) = l.get_mut(&pid) { x.model = m; } }
            }
            if method == "turn/started" { turn.effort = conn2.effort.lock().unwrap_or_else(|p| p.into_inner()).clone(); let mut l = reg2.live.lock().unwrap_or_else(|p| p.into_inner()); if let Some(x) = l.get_mut(&pid) { x.busy = true; } }
            let model = model2.lock().unwrap_or_else(|p| p.into_inner()).clone();
            for (out, persist) in translate(&mut turn, &thread_id, &model, &msg) {
                let oty = s(out.get("type"));
                if matches!(oty.as_str(), "assistant" | "result" | "rate_limit_event") { note_usage(Path::new(&cwd2), pid, &out); }
                if oty == "result" {
                    let cost = out.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
                    { let mut l = reg2.live.lock().unwrap_or_else(|p| p.into_inner()); if let Some(x) = l.get_mut(&pid) { x.busy = false; x.cost_usd += cost; } }
                    // the plan window spent: the pane's banner reads the box's mark, like a headless run's
                    if out.get("error_info") == Some(&json!("usageLimitExceeded")) { crate::goals::mark_quota_file(&cwd2, "codex", &s(out.get("result"))); }
                    reg2.on_turn_result(pid, &cwd2, &out);
                }
                reg2.emit(pid, out, persist);
            }
        }
        let code = child.wait().await.ok().and_then(|st| st.code());
        conns().lock().unwrap_or_else(|p| p.into_inner()).remove(&pid);
        let was = reg2.live.lock().unwrap_or_else(|p| p.into_inner()).get(&pid).map(|x| x.child_id) == Some(child_id);
        if was { reg2.live.lock().unwrap_or_else(|p| p.into_inner()).remove(&pid); }
        reg2.emit(pid, json!({ "type": "antiloki", "subtype": "exited", "code": code, "cwd": cwd2, "engine": "codex" }), true);
    });
    // the handshake, then the thread: resumed on the pinned id when it still loads, else started in the box
    conn.request("initialize", json!({ "clientInfo": { "name": "antiloki", "title": "antiloki", "version": env!("CARGO_PKG_VERSION") }, "capabilities": { "experimentalApi": true, "requestAttestation": false, "optOutNotificationMethods": ["mcpServer/startupStatus/updated", "remoteControl/status/changed"] } })).await?;
    conn.notify("initialized", json!({}));
    // a READ-ONLY pane reads only (chat_pane.rs::is_readonly) — the sandbox says so, whatever the dial
    let sandbox = if crate::chat_pane::is_readonly(pane) { "read-only" } else if max_mode { "danger-full-access" } else { "workspace-write" };
    let approval = if max_mode { "never" } else { "on-request" };
    let base = json!({ "cwd": cwd, "sandbox": sandbox, "approvalPolicy": approval, "approvalsReviewer": "user" });
    let prior = pinned_thread(cwd).filter(|_| !fresh);
    let (thread, resumed) = match prior {
        Some(id) => { let mut p = base.clone(); p["threadId"] = json!(id); match conn.request("thread/resume", p).await { Ok(r) => (r, true), Err(e) => { reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "stderr", "text": format!("codex could not resume thread {id}: {e} — starting a new one") }), true); (conn.request("thread/start", base.clone()).await?, false) } } }
        None => (conn.request("thread/start", base.clone()).await?, false),
    };
    let tid = s(thread.get("thread").and_then(|t| t.get("id")));
    if tid.is_empty() { return Err(format!("codex thread/start answered without an id: {thread}")); }
    let model = s(thread.get("model")); let model = if model.is_empty() { "codex".to_string() } else { model };
    *model_slot.lock().unwrap_or_else(|p| p.into_inner()) = model.clone();
    *conn.thread_id.lock().unwrap_or_else(|p| p.into_inner()) = tid.clone();
    let _ = std::fs::create_dir_all(Path::new(cwd).join(".antiloki"));
    let _ = std::fs::write(Path::new(cwd).join(".antiloki").join("pty-session.json"), json!({ "engine": "codex", "session_id": tid, "t": now_ms(), "ui": "chat" }).to_string());
    // the sender: each text → one turn on the thread, at the effort the router last set
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    { let (conn3, reg3, tid2, cancel3) = (conn.clone(), reg.clone(), tid.clone(), cancel.clone());
      tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if cancel3.load(Ordering::SeqCst) { break; }
            let effort = conn3.effort.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let mut p = json!({ "threadId": tid2, "input": [{ "type": "text", "text": text, "text_elements": [] }] });
            if let Some(e) = effort { p["effort"] = json!(e); }
            if let Err(e) = conn3.request("turn/start", p).await { reg3.emit(pane_id, json!({ "type": "antiloki", "subtype": "stderr", "text": format!("codex refused the turn: {e}") }), true); let mut l = reg3.live.lock().unwrap_or_else(|p| p.into_inner()); if let Some(x) = l.get_mut(&pane_id) { x.busy = false; } }
        }
      }); }
    let on_stop: Box<dyn FnOnce() + Send> = { let (c, t) = (conn.clone(), tid.clone()); Box::new(move || { let _ = c.send_raw(&json!({ "id": 0, "method": "turn/interrupt", "params": { "threadId": t } })); }) };
    let label = model_label(&model, conn.effort.lock().unwrap_or_else(|p| p.into_inner()).as_deref());
    reg.live.lock().unwrap_or_else(|p| p.into_inner()).insert(pane_id, Live { stdin: tx, child_id, engine: "codex".into(), cancel: cancel.clone(), on_stop: Some(on_stop), session_id: tid.clone(), model: model.clone(), busy: false, cost_usd: 0.0, started: now_ms(), min: crate::agent::pane_min(pane), lift: false, turn_started: 0, last_ask: String::new(), parent: crate::agent::pane_parent(pane), turn_tin: 0, turn_tout: 0, gate_retry: false, paused: false });
    reg.emit(pane_id, json!({ "type": "antiloki", "subtype": if resumed { "resumed" } else { "started" }, "session_id": tid, "model": label, "engine": "codex", "pid": child_id, "sandbox": sandbox, "approval": approval }), true);
    Ok(json!({ "ok": true, "pane_id": pane_id, "session_id": tid, "resumed": resumed, "model": label, "engine": "codex", "pid": child_id }))
}
/// The pane's `once | always | reject` for a card this module raised. True when it was ours.
pub fn permission(reg: &Arc<AgentRegistry>, pane_id: i64, id: &str, reply: &str) -> Option<bool> {
    let conn = conn_of(pane_id)?;
    let (rpc_id, method, asked) = conn.asks.lock().unwrap_or_else(|p| p.into_inner()).remove(id)?;
    conn.respond(&rpc_id, decision_of(&method, reply, &asked));
    reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "permission_reply", "id": id, "reply": reply }), true);
    Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    const T: &str = "01a0c1d0-e1ae-72d3-a084-79e5904cf170";
    #[test]
    fn a_turn_becomes_the_chats_events() {
        let mut t = Turn::default();
        let n = |m: &str, p: Value| json!({ "method": m, "params": p });
        // another thread's notification is nobody's business
        assert!(translate(&mut t, T, "gpt-5.6-terra", &n("item/agentMessage/delta", json!({ "threadId": "other", "delta": "no" }))).is_empty());
        translate(&mut t, T, "gpt-5.6-terra", &n("turn/started", json!({ "threadId": T, "turn": { "id": "u1", "status": "inProgress" } })));
        assert!(t.started > 0);
        let d = translate(&mut t, T, "gpt-5.6-terra", &n("item/agentMessage/delta", json!({ "threadId": T, "turnId": "u1", "itemId": "m1", "delta": "Hi" })));
        assert_eq!(d.len(), 1); assert!(!d[0].1); assert_eq!(d[0].0["event"]["delta"]["text"], "Hi");
        // a command: announced at start, resolved at completion with its output and exit code
        let cmd = json!({ "type": "commandExecution", "id": "c1", "command": "ls -la", "cwd": "/w", "status": "inProgress", "aggregatedOutput": null, "exitCode": null });
        let r = translate(&mut t, T, "gpt-5.6-terra", &n("item/started", json!({ "threadId": T, "turnId": "u1", "item": cmd })));
        assert_eq!(r.len(), 1); assert_eq!(r[0].0["message"]["content"][0]["type"], "tool_use"); assert_eq!(r[0].0["message"]["content"][0]["name"], "bash"); assert_eq!(r[0].0["message"]["content"][0]["input"]["command"], "ls -la");
        let done = json!({ "type": "commandExecution", "id": "c1", "command": "ls -la", "cwd": "/w", "status": "completed", "aggregatedOutput": "a.txt\nb.txt", "exitCode": 0 });
        let r = translate(&mut t, T, "gpt-5.6-terra", &n("item/completed", json!({ "threadId": T, "turnId": "u1", "item": done })));
        assert_eq!(r.len(), 1, "announced already — only the result"); assert_eq!(r[0].0["type"], "user"); assert_eq!(r[0].0["message"]["content"][0]["tool_use_id"], "c1"); assert_eq!(r[0].0["message"]["content"][0]["is_error"], false);
        // a file change that was never announced: announced and resolved in one go; a failed one is an error
        let fc = json!({ "type": "fileChange", "id": "f1", "status": "failed", "changes": [{ "path": "src/a.rs", "kind": "update", "diff": "" }] });
        let r = translate(&mut t, T, "gpt-5.6-terra", &n("item/completed", json!({ "threadId": T, "turnId": "u1", "item": fc })));
        assert_eq!(r.len(), 2); assert_eq!(r[0].0["message"]["content"][0]["name"], "apply_patch"); assert_eq!(r[1].0["message"]["content"][0]["is_error"], true);
        // an MCP tool carries its server and arguments
        let mcp = json!({ "type": "mcpToolCall", "id": "t1", "server": "antiloki", "tool": "antiloki_query", "status": "completed", "arguments": { "q": "x" }, "result": { "content": [] }, "error": null });
        let r = translate(&mut t, T, "gpt-5.6-terra", &n("item/completed", json!({ "threadId": T, "turnId": "u1", "item": mcp })));
        assert_eq!(r[0].0["message"]["content"][0]["name"], "antiloki/antiloki_query"); assert_eq!(r[0].0["message"]["content"][0]["input"]["q"], "x");
        // the final message, the reasoning summary as thinking, the usage, and the turn closed as a priced result
        let r = translate(&mut t, T, "gpt-5.6-terra", &n("item/completed", json!({ "threadId": T, "turnId": "u1", "item": { "type": "reasoning", "id": "r1", "summary": ["Looking at the tree"], "content": [] } })));
        assert_eq!(r[0].0["message"]["content"][0]["type"], "thinking");
        let r = translate(&mut t, T, "gpt-5.6-terra", &n("item/completed", json!({ "threadId": T, "turnId": "u1", "item": { "type": "agentMessage", "id": "m1", "text": "Hi there.", "phase": "final_answer" } })));
        assert_eq!(r[0].0["type"], "assistant"); assert_eq!(r[0].0["message"]["content"][0]["text"], "Hi there."); assert_eq!(r[0].0["message"]["model"], "gpt-5.6-terra");
        translate(&mut t, T, "gpt-5.6-terra", &n("thread/tokenUsage/updated", json!({ "threadId": T, "turnId": "u1", "tokenUsage": { "total": {}, "last": { "totalTokens": 1300, "inputTokens": 1200, "cachedInputTokens": 1000, "cacheWriteInputTokens": 0, "outputTokens": 100, "reasoningOutputTokens": 20 }, "modelContextWindow": 258000 } })));
        let r = translate(&mut t, T, "gpt-5.6-terra", &n("turn/completed", json!({ "threadId": T, "turn": { "id": "u1", "status": "completed", "error": null, "durationMs": 2806 } })));
        assert_eq!(r.len(), 1); let res = &r[0].0;
        assert_eq!(res["type"], "result"); assert_eq!(res["subtype"], "success"); assert_eq!(res["engine"], "codex"); assert_eq!(res["session_id"], T);
        assert_eq!(res["usage"]["input_tokens"], 200.0); assert_eq!(res["usage"]["cache_read_input_tokens"], 1000.0); assert_eq!(res["usage"]["output_tokens"], 100.0);
        assert_eq!(res["duration_ms"], 2806); assert_eq!(res["num_turns"], 4); assert_eq!(res["modelUsage"]["gpt-5.6-terra"]["contextWindow"], 258000.0);
        // gpt-5 list price: $1.25 in · $10 out per Mtok — 200 in + 1000 cached at a tenth + 100 out
        assert!((res["total_cost_usd"].as_f64().unwrap() - (200.0 * 1.25 + 100.0 * 1.25 + 100.0 * 10.0) / 1e6).abs() < 1e-12);
        assert_eq!(t.tools, 0, "the turn resets");
    }
    #[test]
    fn a_usage_limit_is_an_error_result_that_names_the_cause_and_the_effort_rides_on_the_label() {
        let mut t = Turn { effort: Some("low".into()), ..Turn::default() };
        let n = |m: &str, p: Value| json!({ "method": m, "params": p });
        // the real shapes of 2026-09-20 (the operator's exhausted ChatGPT window)
        let e = translate(&mut t, T, "gpt-5.6-terra", &n("error", json!({ "threadId": T, "turnId": "u1", "willRetry": false, "error": { "message": "You've hit your usage limit. Upgrade to Plus … or try again at Oct 9th, 2026 6:39 PM.", "codexErrorInfo": "usageLimitExceeded" } })));
        assert!(e.is_empty(), "a final error is reported by the turn, not twice");
        let e = translate(&mut t, T, "gpt-5.6-terra", &n("error", json!({ "threadId": T, "turnId": "u1", "willRetry": true, "error": { "message": "stream dropped", "codexErrorInfo": { "responseStreamDisconnected": { "httpStatusCode": null } } } })));
        assert_eq!(e[0].0["subtype"], "stderr"); assert!(e[0].0["text"].as_str().unwrap().ends_with("(retrying)"));
        let r = translate(&mut t, T, "gpt-5.6-terra", &n("turn/completed", json!({ "threadId": T, "turn": { "id": "u1", "status": "failed", "error": { "message": "You've hit your usage limit. Upgrade to Plus … or try again at Oct 9th, 2026 6:39 PM.", "codexErrorInfo": "usageLimitExceeded" }, "durationMs": 2806 } })));
        let res = &r[0].0;
        assert_eq!(res["is_error"], true); assert_eq!(res["subtype"], "error"); assert_eq!(res["error_info"], "usageLimitExceeded"); assert!(res["result"].as_str().unwrap().contains("usage limit"));
        assert!(antiloki_rs::cockpit__helpers_engine_handoff::limit_hit(res["result"].as_str().unwrap()).is_some(), "the handoff's own detector agrees");
        assert_eq!(res["modelUsage"].as_object().unwrap().keys().next().unwrap(), "gpt-5.6-terra · low");
        assert_eq!(t.effort.as_deref(), Some("low"), "the effort outlives the turn");
        assert_eq!(model_label("gpt-5.6-terra", None), "gpt-5.6-terra"); assert_eq!(effort_of(crate::cli::Tier::Mid), "medium");
        // an interrupted turn is not an error
        let r = translate(&mut t, T, "gpt-5.6-terra", &n("turn/completed", json!({ "threadId": T, "turn": { "id": "u2", "status": "interrupted", "error": null } })));
        assert_eq!(r[0].0["is_error"], false); assert_eq!(r[0].0["result"], "interrupted");
        // the plan windows in the status line's shape
        let r = translate(&mut t, T, "gpt-5.6-terra", &n("account/rateLimits/updated", json!({ "rateLimits": { "primary": { "usedPercent": 42.0, "windowDurationMins": 300, "resetsAt": 1790000000 }, "secondary": { "usedPercent": 63.0, "windowDurationMins": 10080, "resetsAt": 1790337600 } } })));
        assert_eq!(r[0].0["type"], "rate_limit_event"); assert_eq!(r[0].0["rate_limit_info"]["unifiedWindows"]["five_hour"]["utilization"], 0.42); assert_eq!(r[0].0["rate_limit_info"]["unifiedWindows"]["seven_day"]["resetsAt"], 1790337600);
        assert!(translate(&mut t, T, "gpt-5.6-terra", &n("account/rateLimits/updated", json!({ "rateLimits": { "primary": null, "secondary": null } }))).is_empty(), "a sparse update without windows says nothing");
    }
    #[test]
    fn approvals_become_cards_and_the_panes_answer_becomes_the_decision() {
        let ask = json!({ "id": 7, "method": "item/commandExecution/requestApproval", "params": { "threadId": T, "turnId": "u1", "itemId": "c1", "command": "npm test", "cwd": "/w", "reason": "network" } });
        let (id, card) = approval_card(&ask).unwrap();
        assert_eq!(id, "codex-7"); assert_eq!(card["subtype"], "permission"); assert_eq!(card["permission"], "bash"); assert_eq!(card["patterns"][0], "npm test"); assert_eq!(card["metadata"]["reason"], "network");
        assert_eq!(decision_of("item/commandExecution/requestApproval", "once", &json!({})), json!({ "decision": "accept" }));
        assert_eq!(decision_of("item/commandExecution/requestApproval", "always", &json!({})), json!({ "decision": "acceptForSession" }));
        assert_eq!(decision_of("item/fileChange/requestApproval", "reject", &json!({})), json!({ "decision": "decline" }));
        assert_eq!(decision_of("execCommandApproval", "always", &json!({})), json!({ "decision": "approved_for_session" }));
        let perms = json!({ "permissions": { "network": { "enabled": true }, "fileSystem": null } });
        assert_eq!(decision_of("item/permissions/requestApproval", "always", &perms), json!({ "permissions": { "network": { "enabled": true }, "fileSystem": null }, "scope": "session" }));
        assert_eq!(decision_of("item/permissions/requestApproval", "reject", &perms), json!({ "permissions": {}, "scope": "turn" }));
        let (_, card) = approval_card(&json!({ "id": 8, "method": "item/fileChange/requestApproval", "params": { "threadId": T, "turnId": "u1", "itemId": "f1", "grantRoot": "/w/src" } })).unwrap();
        assert_eq!(card["permission"], "edit"); assert_eq!(card["patterns"][0], "/w/src");
        assert!(approval_card(&json!({ "id": 9, "method": "item/tool/requestUserInput", "params": {} })).is_none());
        assert_eq!(auto_response("item/tool/requestUserInput", &json!({})), Some(json!({ "answers": {} })));
        assert_eq!(auto_response("mcpServer/elicitation/request", &json!({})).unwrap()["action"], "decline");
        assert!(auto_response("turn/completed", &json!({})).is_none());
    }
    /// THE REAL APP-SERVER (run by hand: `cargo test -p antiloki-serve --lib agent_codex::tests::live -- --ignored --nocapture`):
    /// the handshake, a thread in a temp box, one turn, and its result on the pane's events. On the
    /// operator's exhausted ChatGPT window (until 2026-10-09) the result is the usage-limit error —
    /// which is still the whole path: initialize → thread/start → turn/start → the reader → `result`.
    #[tokio::test]
    #[ignore]
    async fn live_turn_reaches_the_pane_as_a_result() {
        let dir = std::env::temp_dir().join(format!("antiloki-codex-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let cwd = dir.to_string_lossy().to_string();
        let reg = AgentRegistry::new();
        let mut rx = reg.subscribe(77);
        let pane = json!({ "id": 77, "pane_kind": "codex", "worktree_path": cwd, "settings_json": "{}" });
        let r = start(&reg, 77, &pane, &cwd, &Value::Null, Some(crate::cli::Tier::Low), true).await.expect("start");
        eprintln!("started: {r}");
        assert_eq!(r["engine"], "codex"); assert!(r["session_id"].as_str().unwrap().len() > 10);
        assert!(pinned_thread(&cwd).is_some(), "the thread is pinned in the box");
        { let l = reg.live.lock().unwrap(); let x = l.get(&77).unwrap(); assert_eq!(x.engine, "codex"); x.stdin.send("say hi".into()).unwrap(); }
        let mut result = None;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        while tokio::time::Instant::now() < deadline {
            let Ok(Ok((_, line))) = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await else { continue };
            let ev: Value = serde_json::from_str(&line).unwrap();
            let ev = ev["ev"].clone();
            eprintln!("{}", ev.to_string().chars().take(300).collect::<String>());
            if ev["type"] == "result" { result = Some(ev); break; }
        }
        let res = result.expect("a result within 60 s");
        assert_eq!(res["engine"], "codex");
        if res["is_error"] == true { assert!(res["result"].as_str().unwrap().len() > 10); eprintln!("turn failed (expected on an exhausted window): {}", res["result"]); }
        else { assert!(res["usage"]["output_tokens"].as_f64().unwrap() > 0.0); }
        assert!(!reg.live.lock().unwrap().get(&77).unwrap().busy, "the turn settled");
        assert!(reg.stop(77));
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn responses_settle_their_requests_and_server_requests_do_not() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let c = Conn { out: tx, next_id: AtomicI64::new(1), pending: Mutex::new(HashMap::new()), asks: Mutex::new(HashMap::new()), thread_id: Mutex::new(String::new()), effort: Mutex::new(None) };
        let (otx, mut orx) = tokio::sync::oneshot::channel();
        c.pending.lock().unwrap().insert(3, otx);
        assert!(!c.settle(&json!({ "id": 5, "method": "item/commandExecution/requestApproval", "params": {} })), "a server request has an id AND a method");
        assert!(!c.settle(&json!({ "method": "turn/started", "params": {} })));
        assert!(c.settle(&json!({ "id": 3, "error": { "code": -32600, "message": "invalid thread id" } })));
        assert_eq!(orx.try_recv().unwrap(), Err("invalid thread id".to_string()));
        assert!(c.settle(&json!({ "id": 99, "result": {} })), "an answer nobody waits for is still an answer");
    }
}
