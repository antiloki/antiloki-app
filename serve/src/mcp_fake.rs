//! A FAKE MCP SERVER for the client's conformance tests and the journeys (RFC 0009 §8): the streamable-HTTP transport with a JSON or an SSE-framed reply, a session id, an optional
//! bearer token, two pages of tools, and a tool of each shape and risk class. Small on purpose; it speaks only what the client needs. Never started by the product itself.
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Default)]
pub struct FakeOpts { /** answer with `text/event-stream` */ pub sse: bool, /** require this bearer token */ pub token: Option<String>, /** answer every request 403 with this body (a token that is accepted but may not do this) */ pub forbid: Option<String> }

pub struct Fake { pub url: String, calls: Arc<AtomicUsize>, session_ok: Arc<AtomicBool>, pub log: Arc<std::sync::Mutex<Vec<String>>> }
impl Fake {
    pub fn calls(&self) -> usize { self.calls.load(Ordering::SeqCst) }
    /// did a request after `initialize` carry the session id the server issued?
    pub fn session_seen(&self) -> bool { self.session_ok.load(Ordering::SeqCst) }
}

fn tool(name: &str, desc: &str, props: Value, required: Value, ann: Value) -> Value { json!({ "name": name, "description": desc, "inputSchema": { "type": "object", "properties": props, "required": required }, "annotations": ann }) }

fn tools_page(cursor: Option<&str>) -> Value {
    if cursor.is_none() {
        json!({ "tools": [
            tool("echo", "say it back", json!({ "text": { "type": "string" } }), json!(["text"]), json!({ "readOnlyHint": true })),
            tool("add", "add two numbers", json!({ "a": { "type": "number" }, "b": { "type": "number" } }), json!(["a", "b"]), json!({ "readOnlyHint": true })),
            tool("flag", "a boolean and an enum", json!({ "on": { "type": "boolean" }, "mode": { "type": "string", "enum": ["fast", "safe"] } }), json!([]), json!({})),
        ], "nextCursor": "page2" })
    } else {
        json!({ "tools": [
            tool("list_items", "a list", json!({}), json!([]), json!({ "readOnlyHint": true })),
            tool("fail", "always an error result", json!({}), json!([]), json!({ "readOnlyHint": true })),
            tool("delete_thing", "destroys a thing", json!({ "id": { "type": "string" } }), json!(["id"]), json!({ "destructiveHint": true })),
            tool("slow", "sleeps", json!({ "ms": { "type": "integer" } }), json!([]), json!({ "readOnlyHint": true })),
        ] })
    }
}

async fn answer(method: &str, params: &Value) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => Ok(json!({ "protocolVersion": "2025-03-26", "capabilities": { "tools": {} }, "serverInfo": { "name": "fake", "version": "1" } })),
        "tools/list" => Ok(tools_page(params["cursor"].as_str())),
        "tools/call" => {
            let a = &params["arguments"];
            match params["name"].as_str().unwrap_or("") {
                "echo" => Ok(json!({ "content": [{ "type": "text", "text": a["text"].as_str().unwrap_or("") }] })),
                "add" => { let s = a["a"].as_f64().unwrap_or(0.0) + a["b"].as_f64().unwrap_or(0.0); Ok(json!({ "content": [{ "type": "text", "text": if s.fract() == 0.0 { format!("{}", s as i64) } else { format!("{s}") } }] })) }
                "flag" => Ok(json!({ "content": [{ "type": "text", "text": format!("{}:{}", a["on"], a["mode"]) }] })),
                "list_items" => Ok(json!({ "content": [{ "type": "text", "text": "2 items" }], "structuredContent": { "items": [{ "id": "1", "title": "first" }, { "id": "2", "title": "second" }] } })),
                "fail" => Ok(json!({ "isError": true, "content": [{ "type": "text", "text": "it broke" }] })),
                "delete_thing" => Ok(json!({ "content": [{ "type": "text", "text": format!("deleted {}", a["id"].as_str().unwrap_or("?")) }] })),
                "slow" => { tokio::time::sleep(std::time::Duration::from_millis(a["ms"].as_u64().unwrap_or(0))).await; Ok(json!({ "content": [{ "type": "text", "text": "done" }] })) }
                other => Err((-32602, format!("unknown tool: {other}"))),
            }
        }
        other => Err((-32601, format!("no method {other}"))),
    }
}

pub async fn start(o: FakeOpts) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap(); let port = listener.local_addr().unwrap().port();
    let (calls, session_ok, log) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)), Arc::new(std::sync::Mutex::new(vec![])));
    let (c2, s2, l2) = (calls.clone(), session_ok.clone(), log.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut st, _)) = listener.accept().await else { return };
            let (o, c2, s2, l2) = (o.clone(), c2.clone(), s2.clone(), l2.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new(); let mut chunk = [0u8; 4096];
                let (head, body) = loop {
                    let n = st.read(&mut chunk).await.unwrap_or(0); if n == 0 { return; } buf.extend_from_slice(&chunk[..n]);
                    let t = String::from_utf8_lossy(&buf).to_string();
                    if let Some(i) = t.find("\r\n\r\n") { let len = t[..i].to_lowercase().split("content-length:").nth(1).and_then(|x| x.trim().split_whitespace().next().and_then(|n| n.parse::<usize>().ok())).unwrap_or(0); if buf.len() >= i + 4 + len { break (t[..i].to_string(), t[i + 4..i + 4 + len].to_string()); } }
                };
                let hdr = |k: &str| head.lines().find_map(|l| { let (a, b) = l.split_once(':')?; if a.trim().eq_ignore_ascii_case(k) { Some(b.trim().to_string()) } else { None } });
                if let Ok(mut l) = l2.lock() { l.push(head.lines().next().unwrap_or("").to_string()); }
                let respond = |status: &str, extra: &str, ctype: &str, body: String| format!("HTTP/1.1 {status}\r\ncontent-type: {ctype}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                if let Some(t) = &o.token { if hdr("authorization").as_deref() != Some(&format!("Bearer {t}")) { let _ = st.write_all(respond("401 Unauthorized", "", "text/plain", "sign in".into()).as_bytes()).await; return; } }
                if let Some(why) = &o.forbid { let _ = st.write_all(respond("403 Forbidden", "", "text/plain", why.clone()).as_bytes()).await; return; }
                let req: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                let method = req["method"].as_str().unwrap_or("").to_string();
                if method != "initialize" { if hdr("mcp-session-id").as_deref() != Some("fake-session-1") { let _ = st.write_all(respond("400 Bad Request", "", "text/plain", "no session".into()).as_bytes()).await; return; } s2.store(true, Ordering::SeqCst); }
                if req.get("id").is_none() { let _ = st.write_all(respond("202 Accepted", "", "text/plain", String::new()).as_bytes()).await; return; }
                c2.fetch_add(1, Ordering::SeqCst);
                let msg = match answer(&method, &req["params"]).await { Ok(r) => json!({ "jsonrpc": "2.0", "id": req["id"], "result": r }), Err((code, message)) => json!({ "jsonrpc": "2.0", "id": req["id"], "error": { "code": code, "message": message } }) };
                let extra = if method == "initialize" { "mcp-session-id: fake-session-1\r\n" } else { "" };
                let out = if o.sse { respond("200 OK", extra, "text/event-stream", format!(": keep-alive\n\nevent: message\ndata: {msg}\n\n")) } else { respond("200 OK", extra, "application/json", msg.to_string()) };
                let _ = st.write_all(out.as_bytes()).await;
            });
        }
    });
    Fake { url: format!("http://127.0.0.1:{port}/mcp"), calls, session_ok, log }
}
