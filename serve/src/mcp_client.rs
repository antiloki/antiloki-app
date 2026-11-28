//! THE SERVE AS AN MCP CLIENT (RFC 0009 §4.1, milestone M0): speaks the streamable-HTTP transport of an MCP server (a JSON reply, or an SSE-framed one), lists an app's tools with
//! their JSON schemas and annotations, calls them, and returns typed results. A call carries a timeout, a rate limit and a cost note; its risk class comes from the tool's annotations
//! (a tool that does not say it only reads is a write; one that says it destroys is destructive). Signing in is a token the person pasted, kept apart from the repo (see `app_tokens`).
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const PROTOCOL_VERSION: &str = "2025-03-26";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk { Read, Write, Destructive }

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Tool { pub name: String, pub description: String, pub input_schema: Value, pub risk: Risk, pub annotations: Value }

#[derive(Clone, Debug, PartialEq)]
pub enum McpError {
    /// the server answered 401: the token is missing, wrong or expired; the person must sign in again
    NeedsSignin,
    /// the server answered 403: the sign-in is fine but it may not do this (a missing permission, a scope, an organisation that has not approved the token); what the server said, trimmed
    Forbidden(String),
    Timeout(u64),
    RateLimited { per_min: u32, retry_in_ms: u64 },
    /// a JSON-RPC error object from the server
    Rpc { code: i64, message: String },
    Http(u16),
    Transport(String),
}
impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            McpError::NeedsSignin => write!(f, "needs sign-in: the app answered 401 (the token is missing, wrong or expired)"),
            McpError::Forbidden(d) => write!(f, "forbidden: the app answered 403: {d}"),
            McpError::Timeout(ms) => write!(f, "timeout: no answer in {ms} ms"),
            McpError::RateLimited { per_min, retry_in_ms } => write!(f, "rate limit: at most {per_min} calls a minute; try again in {} s", (retry_in_ms + 999) / 1000),
            McpError::Rpc { code, message } => write!(f, "the app said ({code}): {message}"),
            McpError::Http(s) => write!(f, "the app answered HTTP {s}"),
            McpError::Transport(e) => write!(f, "could not reach the app: {e}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CallResult { pub is_error: bool, pub text: String, pub content: Vec<Value>, pub structured: Option<Value>, pub ms: u64, pub note: String }

#[derive(Clone, Copy, Debug)]
pub struct Opts { pub call_timeout_ms: u64, pub max_calls_per_min: u32 }
impl Default for Opts { fn default() -> Opts { Opts { call_timeout_ms: 30_000, max_calls_per_min: 30 } } }

/// The risk class of a tool from its annotations. Unknown means write: it is only a read when the tool says so.
pub fn risk_of(annotations: &Value) -> Risk {
    if annotations["destructiveHint"].as_bool() == Some(true) { Risk::Destructive }
    else if annotations["readOnlyHint"].as_bool() == Some(true) { Risk::Read }
    else { Risk::Write }
}

/// May this caller make this call? A read is free; a write is the person's (an agent only when the person allowed writes for it); a destructive one is never an agent's and the person must confirm it.
pub fn may_call(risk: Risk, by_agent: bool, agent_may_write: bool, confirmed: bool) -> Result<(), String> {
    match risk {
        Risk::Read => Ok(()),
        Risk::Write if !by_agent || agent_may_write => Ok(()),
        Risk::Write => Err("this tool writes to the app: an agent may not unless the person allowed it".into()),
        Risk::Destructive if by_agent => Err("this tool destroys data in the app: an agent never calls it, the person does".into()),
        Risk::Destructive if !confirmed => Err("this tool destroys data in the app: confirm it (`confirm: true`) to go ahead".into()),
        Risk::Destructive => Ok(()),
    }
}

pub struct Client {
    http: reqwest::Client, url: String, token: Option<String>, opts: Opts,
    session: Mutex<Option<String>>, next_id: AtomicU64, window: Mutex<VecDeque<Instant>>,
}

/// The JSON-RPC message with this id in a reply that is plain JSON or SSE-framed (`data:` lines, events separated by a blank line).
fn message_of(body: &str, content_type: &str, id: u64) -> Result<Value, McpError> {
    if content_type.contains("text/event-stream") {
        let norm = body.replace("\r\n", "\n");
        for ev in norm.split("\n\n") {
            let data: Vec<&str> = ev.lines().filter_map(|l| l.strip_prefix("data:")).map(|d| d.strip_prefix(' ').unwrap_or(d)).collect();
            if data.is_empty() { continue; }
            if let Ok(v) = serde_json::from_str::<Value>(&data.join("\n")) { if v["id"] == json!(id) { return Ok(v); } }
        }
        Err(McpError::Transport("the event stream ended without an answer".into()))
    } else {
        let v: Value = serde_json::from_str(body).map_err(|e| McpError::Transport(format!("not JSON: {e}")))?;
        Ok(if v.is_array() { v.as_array().and_then(|a| a.iter().find(|m| m["id"] == json!(id)).cloned()).unwrap_or(Value::Null) } else { v })
    }
}

impl Client {
    pub fn new(url: &str, token: Option<String>, opts: Opts) -> Result<Client, McpError> {
        let http = reqwest::Client::builder().user_agent("antiloki").build().map_err(|e| McpError::Transport(e.to_string()))?;
        Ok(Client { http, url: url.to_string(), token, opts, session: Mutex::new(None), next_id: AtomicU64::new(1), window: Mutex::new(VecDeque::new()) })
    }

    /// Open the session: `initialize`, then the `initialized` notification.
    pub async fn connect(url: &str, token: Option<String>, opts: Opts) -> Result<Client, McpError> {
        let c = Client::new(url, token, opts)?;
        c.rpc("initialize", json!({ "protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": { "name": "antiloki", "version": env!("CARGO_PKG_VERSION") } }), 10_000).await?;
        c.notify("notifications/initialized").await?;
        Ok(c)
    }

    fn headers(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut rb = rb.header("content-type", "application/json").header("accept", "application/json, text/event-stream");
        if let Some(t) = &self.token { rb = rb.header("authorization", format!("Bearer {t}")); }
        if let Some(s) = self.session.lock().ok().and_then(|g| g.clone()) { rb = rb.header("mcp-session-id", s); }
        rb
    }

    /// What a refusing server said, trimmed and with the token taken out: the reason a person needs to fix the permission.
    async fn refusal(&self, r: reqwest::Response) -> String { let t = r.text().await.unwrap_or_default(); let t = self.redact(t.trim()); let t: String = t.chars().take(240).collect(); if t.is_empty() { "no reason given".into() } else { t } }

    async fn notify(&self, method: &str) -> Result<(), McpError> {
        let r = self.headers(self.http.post(&self.url)).timeout(Duration::from_secs(10)).json(&json!({ "jsonrpc": "2.0", "method": method })).send().await.map_err(|e| McpError::Transport(self.redact(&e.to_string())))?;
        let st = r.status().as_u16(); if st == 401 { return Err(McpError::NeedsSignin); } if st == 403 { return Err(McpError::Forbidden(self.refusal(r).await)); } if st >= 400 { return Err(McpError::Http(st)); }
        Ok(())
    }

    async fn rpc(&self, method: &str, params: Value, timeout_ms: u64) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let r = self.headers(self.http.post(&self.url)).timeout(Duration::from_millis(timeout_ms)).json(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).send().await
            .map_err(|e| if e.is_timeout() { McpError::Timeout(timeout_ms) } else { McpError::Transport(self.redact(&e.to_string())) })?;
        let st = r.status().as_u16();
        if st == 401 { return Err(McpError::NeedsSignin); }
        if st == 403 { return Err(McpError::Forbidden(self.refusal(r).await)); }
        if st >= 400 { return Err(McpError::Http(st)); }
        if let Some(s) = r.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()) { if let Ok(mut g) = self.session.lock() { *g = Some(s.to_string()); } }
        let ct = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let body = r.text().await.map_err(|e| if e.is_timeout() { McpError::Timeout(timeout_ms) } else { McpError::Transport(self.redact(&e.to_string())) })?;
        let m = message_of(&body, &ct, id)?;
        if let Some(e) = m.get("error").filter(|e| e.is_object()) { return Err(McpError::Rpc { code: e["code"].as_i64().unwrap_or(0), message: e["message"].as_str().unwrap_or("").to_string() }); }
        Ok(m["result"].clone())
    }

    /// Nothing that goes into an error or a log may hold the token.
    fn redact(&self, s: &str) -> String { match &self.token { Some(t) if !t.is_empty() => s.replace(t.as_str(), "[token]"), _ => s.to_string() } }

    /// Every tool of the app (the pages of `tools/list` followed).
    pub async fn tools(&self) -> Result<Vec<Tool>, McpError> {
        let (mut out, mut cursor) = (vec![], None::<String>);
        for _ in 0..20 {
            let r = self.rpc("tools/list", cursor.as_ref().map_or(json!({}), |c| json!({ "cursor": c })), 15_000).await?;
            for t in r["tools"].as_array().cloned().unwrap_or_default() {
                let ann = t.get("annotations").cloned().unwrap_or(Value::Null);
                out.push(Tool { name: t["name"].as_str().unwrap_or("").to_string(), description: t["description"].as_str().unwrap_or("").to_string(), input_schema: t.get("inputSchema").cloned().unwrap_or(json!({ "type": "object" })), risk: risk_of(&ann), annotations: ann });
            }
            cursor = r["nextCursor"].as_str().filter(|c| !c.is_empty()).map(String::from);
            if cursor.is_none() { break; }
        }
        Ok(out)
    }

    /// The rate limit: a sliding minute. Returns how many calls that minute holds, or how long to wait.
    fn take_slot(&self) -> Result<usize, McpError> {
        let mut w = self.window.lock().map_err(|_| McpError::Transport("busy".into()))?; let now = Instant::now();
        while w.front().map_or(false, |t| now.duration_since(*t) >= Duration::from_secs(60)) { w.pop_front(); }
        if w.len() as u32 >= self.opts.max_calls_per_min { let wait = w.front().map_or(0, |t| 60_000u64.saturating_sub(now.duration_since(*t).as_millis() as u64)); return Err(McpError::RateLimited { per_min: self.opts.max_calls_per_min, retry_in_ms: wait }); }
        w.push_back(now); Ok(w.len())
    }

    /// Call one tool. The result is typed: its text, its content blocks, its structured content, whether the app called it an error, how long it took.
    pub async fn call(&self, tool: &str, args: Value) -> Result<CallResult, McpError> {
        let n = self.take_slot()?; let t0 = Instant::now();
        let r = self.rpc("tools/call", json!({ "name": tool, "arguments": args }), self.opts.call_timeout_ms).await?;
        let content = r["content"].as_array().cloned().unwrap_or_default();
        let text = content.iter().filter_map(|c| if c["type"] == "text" { c["text"].as_str() } else { None }).collect::<Vec<_>>().join("\n");
        let ms = t0.elapsed().as_millis() as u64;
        Ok(CallResult { is_error: r["isError"].as_bool().unwrap_or(false), text, content, structured: r.get("structuredContent").filter(|v| !v.is_null()).cloned(), ms, note: format!("{ms} ms · call {n} of {} this minute · a call may cost something on the app's side", self.opts.max_calls_per_min) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_fake::{start, FakeOpts};

    #[tokio::test]
    async fn the_client_lists_tools_with_their_schemas_over_json_and_over_sse_and_follows_the_pages() {
        for sse in [false, true] {
            let f = start(FakeOpts { sse, ..Default::default() }).await;
            let c = Client::connect(&f.url, None, Opts::default()).await.unwrap();
            let tools = c.tools().await.unwrap();
            let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
            assert_eq!(names, vec!["echo", "add", "flag", "list_items", "fail", "delete_thing", "slow"], "sse={sse}: both pages were read");
            let add = tools.iter().find(|t| t.name == "add").unwrap();
            assert_eq!(add.input_schema["properties"]["a"]["type"], json!("number")); assert_eq!(add.input_schema["required"], json!(["a", "b"]));
            let flag = tools.iter().find(|t| t.name == "flag").unwrap(); assert_eq!(flag.input_schema["properties"]["mode"]["enum"], json!(["fast", "safe"])); assert_eq!(flag.input_schema["properties"]["on"]["type"], json!("boolean"));
            assert!(f.session_seen(), "the session id the server gave was sent back");
        }
    }

    #[tokio::test]
    async fn a_call_returns_typed_results_errors_and_json_rpc_failures_are_told_apart() {
        let f = start(FakeOpts::default()).await; let c = Client::connect(&f.url, None, Opts::default()).await.unwrap();
        let r = c.call("echo", json!({ "text": "hello" })).await.unwrap(); assert!(!r.is_error); assert_eq!(r.text, "hello"); assert!(r.note.contains("call 1 of 30"), "{}", r.note);
        assert_eq!(c.call("add", json!({ "a": 2, "b": 3.5 })).await.unwrap().text, "5.5");
        let l = c.call("list_items", json!({})).await.unwrap(); assert_eq!(l.structured.unwrap()["items"].as_array().unwrap().len(), 2);
        let e = c.call("fail", json!({})).await.unwrap(); assert!(e.is_error && e.text.contains("it broke"), "{e:?}");
        match c.call("nope", json!({})).await { Err(McpError::Rpc { code, message }) => { assert_eq!(code, -32602); assert!(message.contains("unknown tool")); } other => panic!("{other:?}") }
        assert!(f.calls() >= 5);
    }

    #[tokio::test]
    async fn a_slow_tool_times_out_and_the_rate_limit_stops_a_flood() {
        let f = start(FakeOpts::default()).await;
        let c = Client::connect(&f.url, None, Opts { call_timeout_ms: 150, max_calls_per_min: 3 }).await.unwrap();
        assert!(matches!(c.call("slow", json!({ "ms": 1500 })).await, Err(McpError::Timeout(150))));
        // the timed-out call counted: two more are allowed, the next is refused with a wait
        c.call("echo", json!({ "text": "a" })).await.unwrap(); c.call("echo", json!({ "text": "b" })).await.unwrap();
        match c.call("echo", json!({ "text": "c" })).await { Err(McpError::RateLimited { per_min: 3, retry_in_ms }) => assert!(retry_in_ms > 0 && retry_in_ms <= 60_000), other => panic!("{other:?}") }
        assert!(McpError::RateLimited { per_min: 3, retry_in_ms: 1200 }.to_string().contains("2 s"));
    }

    #[tokio::test]
    async fn a_403_is_told_from_a_401_and_carries_what_the_app_said() {
        let f = start(FakeOpts { forbid: Some("Resource not accessible by personal access token".into()), ..Default::default() }).await;
        match Client::connect(&f.url, Some("s3cret-token".into()), Opts::default()).await { Err(McpError::Forbidden(why)) => { assert!(why.contains("Resource not accessible"), "{why}"); assert!(!why.contains("s3cret-token")); } other => panic!("{:?}", other.map(|_| ())) }
        assert!(McpError::Forbidden("no".into()).to_string().contains("403"));
        assert!(McpError::NeedsSignin.to_string().contains("401"));
    }

    #[tokio::test]
    async fn an_app_that_wants_a_token_says_needs_signin_and_the_token_never_appears_in_an_error() {
        let f = start(FakeOpts { token: Some("s3cret-token".into()), ..Default::default() }).await;
        assert!(matches!(Client::connect(&f.url, None, Opts::default()).await, Err(McpError::NeedsSignin)));
        assert!(matches!(Client::connect(&f.url, Some("wrong".into()), Opts::default()).await, Err(McpError::NeedsSignin)));
        let c = Client::connect(&f.url, Some("s3cret-token".into()), Opts::default()).await.unwrap(); assert_eq!(c.call("echo", json!({ "text": "in" })).await.unwrap().text, "in");
        // an unreachable app: the error does not carry the token even if the transport put it in the text
        let c2 = Client::new("http://127.0.0.1:1/mcp", Some("s3cret-token".into()), Opts::default()).unwrap();
        let e = c2.tools().await.unwrap_err().to_string(); assert!(!e.contains("s3cret-token") && e.contains("could not reach"), "{e}");
        assert_eq!(c2.redact("boom s3cret-token boom"), "boom [token] boom");
    }

    #[test]
    fn the_risk_of_a_tool_comes_from_its_annotations_and_the_caller_decides_what_it_may_do() {
        assert_eq!(risk_of(&json!({ "readOnlyHint": true })), Risk::Read); assert_eq!(risk_of(&json!({ "destructiveHint": true, "readOnlyHint": false })), Risk::Destructive);
        assert_eq!(risk_of(&json!({})), Risk::Write, "a tool that does not say it only reads is a write"); assert_eq!(risk_of(&Value::Null), Risk::Write);
        assert!(may_call(Risk::Read, true, false, false).is_ok());
        assert!(may_call(Risk::Write, false, false, false).is_ok()); assert!(may_call(Risk::Write, true, false, false).is_err()); assert!(may_call(Risk::Write, true, true, false).is_ok());
        assert!(may_call(Risk::Destructive, true, true, true).is_err(), "an agent never destroys, even allowed and confirmed");
        assert!(may_call(Risk::Destructive, false, false, false).unwrap_err().contains("confirm")); assert!(may_call(Risk::Destructive, false, false, true).is_ok());
        // the SSE reader skips events that are not the answer and joins multi-line data
        let body = ": ping\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\ndata: \"result\":{\"ok\":true}}\n\n";
        assert_eq!(message_of(body, "text/event-stream", 7).unwrap()["result"]["ok"], json!(true)); assert!(message_of(body, "text/event-stream", 8).is_err());
    }
}
