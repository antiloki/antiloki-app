//! THE WEB APPS DOOR (operator 2026-11-10): the store of official MCPs, what is installed, installing and removing, and "generate": looking at an app's address for the description of its API and turning each operation into a tool.
//!
//!   GET  /api/v1/webapps                          { catalog: [store apps], installed: [apps] }
//!   POST /api/v1/webapps {op:"install", store_id}            an app from the store
//!   POST /api/v1/webapps {op:"install", name, app_url, kind: "official"|"generated", mcp_url?, openapi_url?, tools?}   one of your own
//!   POST /api/v1/webapps {op:"uninstall", id}
//!   POST /api/v1/webapps {op:"frame", app_url}                    can the page be shown inside another app (its frame headers)? {frameable, why}
//!   POST /api/v1/webapps {op:"inspect", app_url, openapi_url?}   what an address offers: does the page answer, is there an OpenAPI description, and the tools it would give
//!
//! An install also says whether the app's MCP address answered (any reply counts: a sign-in wall answers 401). Nothing is signed into here: each agent's own CLI connects to the MCP server and asks for its own sign-in.
use antiloki_rs::cockpit__helpers_webapps as w;
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::time::Duration;

fn reply(v: Value) -> Response { (StatusCode::OK, Json(v)).into_response() }
fn s(v: &Value) -> String { v.as_str().unwrap_or("").trim().to_string() }

/// Does an address answer at all? Any HTTP reply is yes (a 401 is a server asking who you are); no reply is no, with why.
async fn reach(url: &str) -> Value {
    let c = match reqwest::Client::builder().timeout(Duration::from_secs(6)).redirect(reqwest::redirect::Policy::limited(3)).user_agent("antiloki").build() { Ok(c) => c, Err(e) => return json!({ "ok": false, "error": e.to_string() }) };
    match c.get(url).header("accept", "text/event-stream, application/json, text/html").send().await { Ok(r) => json!({ "ok": true, "status": r.status().as_u16() }), Err(e) => json!({ "ok": false, "error": if e.is_timeout() { "no answer in 6 seconds".to_string() } else if e.is_connect() { "could not connect".to_string() } else { e.to_string() } }) }
}

/// The places an API's description usually is, for an app at `base`. Pure.
pub fn spec_candidates(base: &str, given: &str) -> Vec<String> {
    if !given.trim().is_empty() { return vec![given.trim().to_string()]; }
    let b = base.trim().trim_end_matches('/'); let origin = { let rest = b.splitn(2, "://").nth(1).unwrap_or(b); let scheme = b.split("://").next().unwrap_or("https"); format!("{scheme}://{}", rest.split('/').next().unwrap_or("")) };
    let mut v: Vec<String> = ["openapi.json", "swagger.json", "api/openapi.json", "api/swagger.json", "v3/api-docs", ".well-known/openapi.json", "api-docs"].iter().flat_map(|p| [format!("{b}/{p}"), format!("{origin}/{p}")]).collect();
    v.dedup(); let mut seen = std::collections::HashSet::new(); v.retain(|x| seen.insert(x.clone())); v
}

async fn inspect(app_url: &str, openapi_url: &str) -> Value {
    let page = reach(app_url).await;
    let c = match reqwest::Client::builder().timeout(Duration::from_secs(8)).user_agent("antiloki").build() { Ok(c) => c, Err(e) => return json!({ "ok": false, "error": e.to_string() }) };
    for u in spec_candidates(app_url, openapi_url) {
        let Ok(r) = c.get(&u).header("accept", "application/json").send().await else { continue };
        if !r.status().is_success() { continue; }
        let Ok(doc) = r.json::<Value>().await else { continue };
        if !doc["paths"].is_object() { continue; }
        let ops = w::operations_of(&doc);
        return json!({ "ok": true, "page": page, "spec_url": u, "title": doc["info"]["title"], "version": doc["info"]["version"], "tools": ops, "count": ops.len(), "writes": ops.iter().filter(|o| o["risk"] != "read").count() });
    }
    json!({ "ok": true, "page": page, "spec_url": Value::Null, "tools": [], "count": 0, "note": "no OpenAPI description was found at the usual places: the app can still be opened and driven in its pane, or give the address of its description" })
}

/// CAN THIS PAGE BE SHOWN INSIDE ANOTHER APP? A site says no with `X-Frame-Options: DENY / SAMEORIGIN` or a `Content-Security-Policy: frame-ancestors` that does not allow everyone; the browser then shows "refused to connect"
/// and the page can never read why (operator 2026-11-11: "linear.app refused to connect"). So the serve asks, and the pane says so instead of showing a broken frame. Returns (frameable, why). Pure over the headers - tested.
pub fn frame_verdict(headers: &[(String, String)]) -> (bool, String) {
    let get = |n: &str| headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.to_ascii_lowercase()).collect::<Vec<_>>();
    for v in get("x-frame-options") { let v = v.trim(); if v.starts_with("deny") || v.starts_with("sameorigin") || v.starts_with("allow-from") { return (false, format!("it sends X-Frame-Options: {}", v.split(',').next().unwrap_or(v).trim())); } }
    for csp in get("content-security-policy") {
        if let Some(d) = csp.split(';').map(str::trim).find(|d| d.starts_with("frame-ancestors")) {
            let allow: Vec<&str> = d.split_whitespace().skip(1).collect();
            if !(allow.contains(&"*") || allow.iter().any(|a| *a == "http:" || *a == "https:")) { return (false, format!("its Content-Security-Policy says {d}")); }
        }
    }
    (true, String::new())
}
async fn frame_check(app_url: &str) -> Value {
    let c = match reqwest::Client::builder().timeout(Duration::from_secs(8)).redirect(reqwest::redirect::Policy::limited(4)).user_agent("Mozilla/5.0 antiloki").build() { Ok(c) => c, Err(e) => return json!({ "ok": false, "error": e.to_string() }) };
    match c.get(app_url).header("accept", "text/html").send().await {
        Ok(r) => { let hs: Vec<(String, String)> = r.headers().iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string())).collect(); let (frameable, why) = frame_verdict(&hs); json!({ "ok": true, "frameable": frameable, "why": why, "status": r.status().as_u16(), "url": r.url().to_string() }) }
        Err(e) => json!({ "ok": true, "frameable": true, "why": "", "unreachable": if e.is_timeout() { "timed out".to_string() } else { e.to_string() } }),
    }
}

/// THE REAL SITE, IN A WINDOW OF ITS OWN (2026-11-11): a site that forbids being framed (Linear, GitHub, Stripe ...) cannot be shown inside a page, but a Chromium-family browser can open it as an APP WINDOW (`--app=`: no tabs, no address bar) with a
/// profile of its own per app, so its sign-in is kept and never mixed with the person's browsing. The serve runs on the person's machine, so it can start that window. Pure parts below, tested.
pub const BROWSERS: &[&str] = &["chromium", "chromium-browser", "google-chrome-stable", "google-chrome", "brave-browser", "brave", "microsoft-edge-stable", "microsoft-edge", "vivaldi"];
/// Which browser to start: the one the person named (`ANTILOKI_BROWSER`), else the first Chromium-family one found. `has` says whether a program is on the PATH. Pure.
pub fn pick_browser(named: Option<&str>, has: &dyn Fn(&str) -> bool) -> Option<String> {
    if let Some(n) = named.map(str::trim).filter(|n| !n.is_empty()) { return Some(n.to_string()); }
    BROWSERS.iter().find(|b| has(b)).map(|b| b.to_string())
}
/// The arguments of an app window for `url`, with the profile folder of the app and a window class (so a tiling window manager can place it by rule). Pure.
pub fn app_args(url: &str, profile: &str, id: &str) -> Vec<String> {
    vec![format!("--app={url}"), format!("--user-data-dir={profile}"), "--no-first-run".into(), "--no-default-browser-check".into(), format!("--class=antiloki-{id}"), "--window-size=1180,820".into()]
}
fn on_path(bin: &str) -> bool { std::env::var_os("PATH").map_or(false, |p| std::env::split_paths(&p).any(|d| d.join(bin).is_file())) }
/// `POST /api/v1/webapps/launch {id?, app_url}`: from this machine only (a paired phone must not start programs here).
pub async fn launch_http(req: axum::extract::Request) -> Response {
    if let Some(r) = crate::remote::local_only(&req) { return r; }
    let body = match axum::body::to_bytes(req.into_body(), 64 * 1024).await { Ok(b) => b, Err(_) => return reply(json!({ "ok": false, "error": "unreadable request" })) };
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let url = match w::clean_url(&s(&b["app_url"])) { Ok(u) => u, Err(e) => return reply(json!({ "ok": false, "error": e })) };
    let id = { let i = w::slug(&s(&b["id"])); if i.is_empty() { w::slug(url.split("://").nth(1).unwrap_or("app").split('/').next().unwrap_or("app")) } else { i } };
    let Some(bin) = pick_browser(std::env::var("ANTILOKI_BROWSER").ok().as_deref(), &on_path) else { return reply(json!({ "ok": false, "error": "no Chromium-family browser was found on this machine (chromium, chrome, brave, edge): install one, or name it with ANTILOKI_BROWSER" })) };
    let profile = w::store_path().with_file_name("webapp-profiles").join(&id);
    if let Err(e) = std::fs::create_dir_all(&profile) { return reply(json!({ "ok": false, "error": format!("could not make the profile folder: {e}") })); }
    let mut cmd = crate::proc::cmd(&bin);
    cmd.args(app_args(&url, &profile.to_string_lossy(), &id)).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(unix)] { use std::os::unix::process::CommandExt; cmd.process_group(0); }
    match cmd.spawn() {
        Ok(mut child) => { std::thread::spawn(move || { let _ = child.wait(); }); reply(json!({ "ok": true, "browser": bin, "profile": profile.to_string_lossy(), "url": url })) }
        Err(e) => reply(json!({ "ok": false, "error": format!("could not start {bin}: {e}") })),
    }
}

/// `GET /api/v1/webapps/oauth/callback?code=..&state=..`: the browser is back from the app's sign-in. Nothing the address says is shown back, only our own words.
pub async fn oauth_callback(axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>) -> axum::response::Html<String> {
    let page = |t: &str, ok: bool| axum::response::Html(format!("<!doctype html><meta charset=utf-8><title>antiloki</title><body style=\"font:16px system-ui;background:#131316;color:#ededf0;display:grid;place-items:center;height:100vh;margin:0\"><div style=\"max-width:420px\"><h2 style=\"color:{}\">{}</h2><p>{}</p></div></body>", if ok { "#3ecf8e" } else { "#f04e4e" }, if ok { "Signed in" } else { "Not signed in" }, t));
    if let Some(e) = q.get("error") { let _ = e; return page("The app did not allow the sign-in. You can close this tab and try again in antiloki.", false); }
    let (Some(code), Some(state)) = (q.get("code"), q.get("state")) else { return page("This address needs a sign-in code. Start the sign-in again from antiloki.", false) };
    match crate::mcp_oauth::finish(state, code).await { Ok(_) => page("antiloki is connected. You can close this tab and go back to antiloki.", true), Err(e) => page(&format!("{}. Start the sign-in again from antiloki.", e.replace('<', "").replace('>', "")), false) }
}

/// The hosts an installed app keeps its uploads on (a picture or video in a description, a comment, a document). An address anywhere else is refused. Pure.
pub fn asset_hosts(app: &str) -> &'static [&'static str] { match app { "linear" => &["uploads.linear.app"], _ => &[] } }
/// May this address be fetched for this app? https only, no credentials in it, an upload host of the app. Pure.
pub fn asset_ok(app: &str, url: &str) -> Result<String, String> {
    let u = url.trim(); let low = u.to_ascii_lowercase(); let rest = low.strip_prefix("https://").ok_or("only https addresses")?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or(""); if host.contains('@') || host.contains(':') { return Err("no credentials or port in the address".into()); }
    if asset_hosts(app).contains(&host) { Ok(u.to_string()) } else { Err(format!("`{host}` is not an upload host of `{app}`")) }
}
/// `GET /api/v1/webapps/asset?app=linear&url=https://uploads.linear.app/...`: a picture or video of an app's own uploads, fetched with the person's connection (it needs it; the browser cannot) and streamed back, Range included so a video seeks. Nothing but the app's upload host.
pub async fn asset(axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>, headers: axum::http::HeaderMap) -> Response {
    let app = q.get("app").cloned().unwrap_or_default(); let plain = |c: StatusCode, t: &str| (c, t.to_string()).into_response();
    let Some(url) = q.get("url") else { return plain(StatusCode::BAD_REQUEST, "url required") };
    let url = match asset_ok(&app, url) { Ok(u) => u, Err(e) => return plain(StatusCode::BAD_REQUEST, &e) };
    if !w::load().iter().any(|a| a["id"] == json!(app)) { return plain(StatusCode::NOT_FOUND, "that app is not installed"); }
    crate::mcp_oauth::refresh_if_due(&app).await;
    let Some(token) = crate::app_tokens::get(&app) else { return plain(StatusCode::UNAUTHORIZED, "sign in to the app first") };
    // an OAuth sign-in is a bearer token; a personal key is sent as it is
    let auth = if crate::app_tokens::oauth_of(&app).is_some() { format!("Bearer {token}") } else { token };
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(300)).user_agent("antiloki").build() { Ok(c) => c, Err(e) => return plain(StatusCode::BAD_GATEWAY, &e.to_string()) };
    let mut req = client.get(&url).header("authorization", auth); if let Some(r) = headers.get("range").and_then(|v| v.to_str().ok()) { req = req.header("range", r); }
    let up = match req.send().await { Ok(r) => r, Err(e) => return plain(StatusCode::BAD_GATEWAY, &format!("the app did not answer: {e}")) };
    let status = StatusCode::from_u16(up.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut out = Response::builder().status(status);
    for k in ["content-type", "content-length", "content-range", "accept-ranges", "etag", "last-modified"] { if let Some(v) = up.headers().get(k) { out = out.header(k, v.as_bytes()); } }
    out = out.header("cache-control", "private, max-age=300").header("x-content-type-options", "nosniff");
    out.body(axum::body::Body::from_stream(up.bytes_stream())).unwrap_or_else(|_| plain(StatusCode::BAD_GATEWAY, "could not stream"))
}

pub async fn get() -> Response { reply(json!({ "ok": true, "catalog": w::catalog_json(), "installed": w::load() })) }

/// `POST /api/v1/webapps`
pub async fn post(body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    match s(&b["op"]).as_str() {
        "install" => {
            let mut apps = w::load();
            let entry = match w::install(&mut apps, &b, crate::cli::now_ms()) { Ok(e) => e, Err(e) => return reply(json!({ "ok": false, "error": e })) };
            if let Err(e) = w::save_to(&w::store_path(), &apps) { return reply(json!({ "ok": false, "error": e })); }
            let mcp = s(&entry["mcp_url"]); let reach = if mcp.is_empty() { Value::Null } else { reach(&mcp).await };
            reply(json!({ "ok": true, "app": entry, "mcp_reach": reach, "installed": apps }))
        }
        "uninstall" => { let mut apps = w::load(); let gone = w::uninstall(&mut apps, &s(&b["id"])); if !gone { return reply(json!({ "ok": false, "error": "that app is not installed" })); } match w::save_to(&w::store_path(), &apps) { Ok(()) => reply(json!({ "ok": true, "installed": apps })), Err(e) => reply(json!({ "ok": false, "error": e })) } }
        "inspect" => match w::clean_url(&s(&b["app_url"])) { Ok(u) => { let spec = s(&b["openapi_url"]); if !spec.is_empty() { if let Err(e) = w::clean_url(&spec) { return reply(json!({ "ok": false, "error": e })); } } reply(inspect(&u, &spec).await) } Err(e) => reply(json!({ "ok": false, "error": e })) },
        "frame" => match w::clean_url(&s(&b["app_url"])) { Ok(u) => reply(frame_check(&u).await), Err(e) => reply(json!({ "ok": false, "error": e })) },
        // THE MCP CLIENT (RFC 0009 M0/M1): list an installed app's tools, call one, keep or forget its token. An agent names an installed app (never an address) and never gives or reads a token.
        // SIGN IN BY THE APP'S OWN OAUTH (mcp_oauth): the page to open in the person's browser; they come back to `oauth/callback`. Only the person starts it.
        "oauth_begin" => {
            if by_agent_of(&b) { return reply(json!({ "ok": false, "error": "only the person signs in to an app" })); }
            let apps = w::load(); let Some(a) = apps.iter().find(|a| a["id"] == json!(s(&b["app"]))) else { return reply(json!({ "ok": false, "error": "that app is not installed" })) };
            let mcp = s(&a["mcp_url"]); if mcp.is_empty() { return reply(json!({ "ok": false, "error": "that app has no MCP address" })); }
            match crate::mcp_oauth::begin(&s(&a["id"]), &mcp).await {
                Err(e) => reply(json!({ "ok": false, "error": e })),
                Ok(url) => { let opened = crate::proc::cmd(if cfg!(target_os = "macos") { "open" } else if cfg!(target_os = "windows") { "explorer" } else { "xdg-open" }).arg(&url).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().is_ok(); reply(json!({ "ok": true, "opened": opened, "url": if opened { Value::Null } else { json!(url) }, "next": "sign in in the browser window that opened; it comes back here by itself" })) }
            }
        }
        "tools" | "call" | "token_set" | "token_forget" => app_mcp(&b).await,
        other => reply(json!({ "ok": false, "error": format!("op is one of: install, uninstall, inspect, frame, oauth_begin, tools, call, token_set, token_forget (not `{other}`)") })),
    }
}

/// The HTTP status behind a refusal (401 sign in again, 403 not allowed), so a screen can say which one it is. 0 when it was neither.
fn status_of(e: &crate::mcp_client::McpError) -> u16 { match e { crate::mcp_client::McpError::NeedsSignin => 401, crate::mcp_client::McpError::Forbidden(_) => 403, crate::mcp_client::McpError::Http(s) => *s, _ => 0 } }

/// When a call is flagged `read_only` (a free pane's), only a read-risk tool goes through. Pure.
pub fn read_only_ok(risk: crate::mcp_client::Risk, read_only: bool, tool: &str) -> Result<(), String> {
    if read_only && risk != crate::mcp_client::Risk::Read { return Err(format!("`{tool}` is not a read tool: a pane\'s read can only read")); } Ok(())
}
/// A free pane's WRITE (RFC 0016 M2): only a write or destructive tool, never without the person's click (`confirm`), and a destructive one needs the second tick (`confirm2`). Pure.
pub fn pane_write_ok(risk: crate::mcp_client::Risk, tool: &str, confirm: bool, confirm2: bool) -> Result<(), String> {
    use crate::mcp_client::Risk::*;
    match risk {
        Read => Err(format!("`{tool}` only reads: a pane reads it with read, not write")),
        _ if !confirm => Err(format!("`{tool}` changes the app: it goes only after the person's click")),
        Destructive if !confirm2 => Err(format!("`{tool}` destroys data in the app: it needs the person's second, explicit tick")),
        _ => Ok(()),
    }
}
/// The call, for the door of the free panes: the same code as the console's, its JSON answer back instead of a response. Pure over the network it talks to.
pub async fn call_for_pane(b: &Value) -> Value {
    let r = app_mcp(b).await; match axum::body::to_bytes(r.into_body(), 4 * 1024 * 1024).await { Ok(by) => serde_json::from_slice::<Value>(&by).unwrap_or_else(|_| json!({ "ok": false, "error": "the app answered something unreadable" })), Err(_) => json!({ "ok": false, "error": "the answer was too large" }) }
}

/// Who is asking: a pane (an agent) or the person.
fn by_agent_of(b: &Value) -> bool { b["pane_id"].as_i64().unwrap_or(0) > 0 }

fn trail_path() -> std::path::PathBuf { w::store_path().with_file_name("app-calls.jsonl") }
/// Every call is a row of the trail: who, which app and tool, its risk, whether it worked, how long. The arguments are not kept, only their names; the token never appears.
fn trail(who: &str, app: &str, tool: &str, risk: &str, ok: bool, ms: u64, arg_names: &[String], err: &str) {
    use std::io::Write;
    let row = json!({ "t_ms": crate::cli::now_ms(), "who": who, "app": app, "tool": tool, "risk": risk, "ok": ok, "ms": ms, "args": arg_names, "error": err });
    let p = trail_path(); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) { let _ = writeln!(f, "{row}"); }
}

async fn app_mcp(b: &Value) -> Response {
    let (op, by_agent, app) = (s(&b["op"]), by_agent_of(b), s(&b["app"]));
    let who = if !s(&b["pane"]).is_empty() { format!("pane:{}", s(&b["pane"])) } else if by_agent { format!("agent:{}", b["pane_id"].as_i64().unwrap_or(0)) } else { "person".to_string() };
    let fail = |e: String| reply(json!({ "ok": false, "error": e }));
    if op == "token_set" || op == "token_forget" {
        if by_agent { return fail("an app's token is the person's: an agent cannot give, read or remove it".into()); }
        if app.is_empty() { return fail("say which `app`".into()); }
        return match if op == "token_set" { crate::app_tokens::set(&app, &s(&b["token"])).map(|_| true) } else { crate::app_tokens::forget(&app) } { Ok(x) => reply(json!({ "ok": true, "app": app, "signed_in": crate::app_tokens::has(&app), "changed": x })), Err(e) => fail(e) };
    }
    // where to connect: an installed app by id (its token from the store), or, for the person only, an address to try
    let (url, token) = if !app.is_empty() {
        let apps = w::load(); let Some(a) = apps.iter().find(|a| a["id"] == json!(app)) else { return fail(format!("`{app}` is not installed")) };
        let u = s(&a["mcp_url"]); if u.is_empty() { return fail(format!("`{app}` has no MCP address")); } crate::mcp_oauth::refresh_if_due(&app).await; (u, crate::app_tokens::get(&app))
    } else if !s(&b["url"]).is_empty() {
        if by_agent { return fail("an agent names an installed app, not an address".into()); }
        match w::clean_url(&s(&b["url"])) { Ok(u) => (u, b["token"].as_str().filter(|t| !t.is_empty()).map(String::from)), Err(e) => return fail(e) }
    } else { return fail("say which `app` (an installed one)".into()) };
    let label = if app.is_empty() { url.clone() } else { app.clone() };
    let client = match crate::mcp_client::Client::connect(&url, token, Default::default()).await { Ok(c) => c, Err(e) => return reply(json!({ "ok": false, "error": e.to_string(), "needs_signin": e == crate::mcp_client::McpError::NeedsSignin, "status": status_of(&e) })) };
    let tools = match client.tools().await { Ok(t) => t, Err(e) => return reply(json!({ "ok": false, "error": e.to_string(), "needs_signin": e == crate::mcp_client::McpError::NeedsSignin, "status": status_of(&e) })) };
    if op == "tools" { return reply(json!({ "ok": true, "app": label, "signed_in": !app.is_empty() && crate::app_tokens::has(&app), "tools": tools })); }
    let tname = s(&b["tool"]); let Some(tool) = tools.iter().find(|t| t.name == tname) else { return fail(format!("the app has no tool `{tname}`")) };
    let agent_may_write = w::load().iter().find(|a| a["id"] == json!(app)).map_or(false, |a| a["agent_write"].as_bool() == Some(true));
    let names: Vec<String> = b["args"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
    // A FREE PANE's read (RFC 0016 M1) is a read and nothing else: whatever its manifest lists, a tool that is not read-risk is refused here, at the door
    if let Err(why) = read_only_ok(tool.risk, b["read_only"].as_bool() == Some(true), &tname) { trail(&who, &label, &tname, &format!("{:?}", tool.risk).to_lowercase(), false, 0, &names, &why); return reply(json!({ "ok": false, "error": why, "risk": tool.risk })); }
    if b["pane_write"].as_bool() == Some(true) { if let Err(why) = pane_write_ok(tool.risk, &tname, b["confirm"].as_bool() == Some(true), b["confirm2"].as_bool() == Some(true)) { trail(&who, &label, &tname, &format!("{:?}", tool.risk).to_lowercase(), false, 0, &names, &why); return reply(json!({ "ok": false, "error": why, "risk": tool.risk })); } }
    if let Err(why) = crate::mcp_client::may_call(tool.risk, by_agent, agent_may_write, b["confirm"].as_bool() == Some(true)) { trail(&who, &label, &tname, &format!("{:?}", tool.risk).to_lowercase(), false, 0, &names, &why); return reply(json!({ "ok": false, "error": why, "risk": tool.risk })); }
    match client.call(&tname, b["args"].clone()).await {
        Ok(r) => { trail(&who, &label, &tname, &format!("{:?}", tool.risk).to_lowercase(), !r.is_error, r.ms, &names, if r.is_error { &r.text } else { "" }); reply(json!({ "ok": true, "risk": tool.risk, "result": r })) }
        Err(e) => { trail(&who, &label, &tname, &format!("{:?}", tool.risk).to_lowercase(), false, 0, &names, &e.to_string()); reply(json!({ "ok": false, "error": e.to_string(), "needs_signin": e == crate::mcp_client::McpError::NeedsSignin, "status": status_of(&e) })) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_pane_write_needs_the_click_and_a_destructive_one_the_second_tick() {
        use crate::mcp_client::Risk::*;
        assert!(pane_write_ok(Read, "list", true, true).unwrap_err().contains("only reads"));
        assert!(pane_write_ok(Write, "save", false, false).unwrap_err().contains("click"));
        assert!(pane_write_ok(Write, "save", true, false).is_ok());
        assert!(pane_write_ok(Destructive, "delete", true, false).unwrap_err().contains("second"));
        assert!(pane_write_ok(Destructive, "delete", true, true).is_ok());
    }
    #[test]
    fn a_read_only_call_lets_only_a_read_tool_through() {
        use crate::mcp_client::Risk::*; assert!(read_only_ok(Read, true, "list").is_ok()); assert!(read_only_ok(Write, false, "save").is_ok(), "the console is unchanged");
        for r in [Write, Destructive] { let e = read_only_ok(r, true, "save_issue").unwrap_err(); assert!(e.contains("save_issue") && e.contains("can only read"), "{e}"); }
    }
    #[test]
    fn the_app_window_uses_the_named_browser_else_the_first_chromium_family_one_and_a_profile_per_app() {
        let has = |b: &str| b == "google-chrome" || b == "brave"; let none = |_: &str| false;
        assert_eq!(pick_browser(Some(" /opt/x/chrome "), &none).as_deref(), Some("/opt/x/chrome")); assert_eq!(pick_browser(None, &has).as_deref(), Some("google-chrome")); assert_eq!(pick_browser(Some(""), &none), None);
        let a = app_args("https://linear.app", "/p/linear", "linear"); assert_eq!(a[0], "--app=https://linear.app"); assert!(a.contains(&"--user-data-dir=/p/linear".to_string()) && a.contains(&"--class=antiloki-linear".to_string()));
    }
    #[test]
    fn a_site_that_forbids_framing_is_told_from_one_that_allows_it() {
        let h = |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
        assert!(frame_verdict(&h(&[("content-type", "text/html")])).0, "no header: it can be framed");
        let (ok, why) = frame_verdict(&h(&[("X-Frame-Options", "DENY")])); assert!(!ok && why.contains("X-Frame-Options: deny"), "{why}");
        assert!(!frame_verdict(&h(&[("x-frame-options", "SAMEORIGIN")])).0);
        let (ok, why) = frame_verdict(&h(&[("content-security-policy", "default-src 'self'; frame-ancestors 'self' https://linear.app")])); assert!(!ok && why.contains("frame-ancestors"), "{why}");
        assert!(frame_verdict(&h(&[("content-security-policy", "frame-ancestors *")])).0); assert!(frame_verdict(&h(&[("content-security-policy", "default-src 'self'")])).0);
    }
    #[test]
    fn the_usual_places_for_an_apis_description_are_tried_on_the_page_and_on_its_origin_or_only_the_one_given() {
        let c = spec_candidates("https://billing.acme.io/app/", "");
        assert!(c.contains(&"https://billing.acme.io/app/openapi.json".to_string()) && c.contains(&"https://billing.acme.io/openapi.json".to_string()) && c.contains(&"https://billing.acme.io/v3/api-docs".to_string()), "{c:?}");
        assert_eq!(c.len(), c.iter().collect::<std::collections::HashSet<_>>().len(), "no address twice");
        assert_eq!(spec_candidates("https://x.io", " https://x.io/spec.yaml "), vec!["https://x.io/spec.yaml"]);
        assert_eq!(spec_candidates("https://x.io", "")[0], "https://x.io/openapi.json");
    }
}

#[cfg(test)]
mod asset_tests {
    use super::*;
    #[test]
    fn only_an_apps_own_upload_host_over_https_is_fetched() {
        assert!(asset_ok("linear", "https://uploads.linear.app/a/b/c").is_ok());
        for bad in ["http://uploads.linear.app/x", "https://evil.example/x", "https://uploads.linear.app.evil.io/x", "https://user@uploads.linear.app/x", "https://uploads.linear.app:8443/x", "file:///etc/passwd"] { assert!(asset_ok("linear", bad).is_err(), "{bad}"); }
        assert!(asset_ok("notion", "https://uploads.linear.app/x").is_err(), "another app has no upload host here");
    }
}
