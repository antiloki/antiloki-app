//! MOCKUPS IN FRAMES, MADE BY AGENTS (operator 2026-11-11: "frames also should be the place where, when we ask for a mockup, it is created by some agent").
//!
//!   GET  /api/v1/frames                 the mockups of the open workspace: [{id, name, ask, author, status, created_ms, error?}]
//!   GET  /api/v1/frames/html?id=        one mockup's document (sanitized)
//!   POST /api/v1/frames {op:"ask", prompt, name?}                a design agent (one headless call on the seat the record picks) makes the mockup; returns at once with the entry `working`
//!   POST /api/v1/frames {op:"create", name, html, ask?, pane_id?}   an agent that already made one (the MCP tool `antiloki_frame_create`) hands it in
//!   POST /api/v1/frames {op:"rename"|"delete", id, name?}
//!
//! A mockup is a folder of two files per workspace: `<root>/.antiloki/frames/index.json` and `<id>.html`. The editor shows a mockup in a SAME-ORIGIN frame to edit it, so whatever an agent wrote is untrusted: it is
//! sanitized here before it is stored (no scripts, no handlers, no embedded pages, no network addresses), and again is data when it is read. Pure parts (sanitize, extract, prompt) are tested.
use crate::cli::{self, ChatMessage, Job, RunOpts};
use crate::dispatch::AppState;
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use regex::Regex;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

const MAX_HTML: usize = 400_000;
fn reply(v: Value) -> Response { (StatusCode::OK, Json(v)).into_response() }
fn s(v: &Value) -> String { v.as_str().unwrap_or("").trim().to_string() }
fn now() -> i64 { cli::now_ms() }

/// What a mockup may contain: the document with scripts, event handlers, `javascript:` addresses, embedded pages and every network address taken out. Images must be inline (`data:`) or drawn (SVG, CSS). Pure - tested.
pub fn sanitize_html(input: &str) -> String {
    let mut h: String = input.chars().take(MAX_HTML).collect();
    let rm = |h: &mut String, re: &str| { if let Ok(r) = Regex::new(re) { *h = r.replace_all(h, "").into_owned(); } };
    rm(&mut h, r"(?is)<script\b.*?</script\s*>"); rm(&mut h, r"(?is)<script\b[^>]*>");
    for tag in ["iframe", "object", "embed", "frame", "frameset", "applet"] { rm(&mut h, &format!(r"(?is)<{tag}\b.*?</{tag}\s*>")); rm(&mut h, &format!(r"(?is)<{tag}\b[^>]*>")); }
    rm(&mut h, r"(?is)<link\b[^>]*>"); rm(&mut h, r"(?is)<base\b[^>]*>"); rm(&mut h, r#"(?is)<meta\b[^>]*http-equiv[^>]*>"#);
    rm(&mut h, r#"(?is)\son[a-z]+\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)"#);
    let nonet = |h: &mut String, re: &str, to: &str| { if let Ok(r) = Regex::new(re) { *h = r.replace_all(h, to).into_owned(); } };
    nonet(&mut h, r#"(?is)\b(href|src|action|formaction|poster|srcset)\s*=\s*("\s*(?:javascript:|vbscript:|https?:|//)[^"]*"|'\s*(?:javascript:|vbscript:|https?:|//)[^']*')"#, r##"$1="#""##);
    nonet(&mut h, r"(?is)@import\b[^;]*;?", "");
    nonet(&mut h, r#"(?is)url\(\s*['"]?\s*(?:https?:|//|javascript:)[^)]*\)"#, "none");
    h
}
/// The document in a model's reply: a fenced ```html block, else from `<!doctype`/`<html` to the last `</html>`, else the reply when it is markup. Pure - tested.
pub fn extract_html(reply: &str) -> Option<String> {
    if let Some(a) = reply.find("```html").or_else(|| reply.find("```HTML")) { let rest = &reply[a + 7..]; let end = rest.find("```").unwrap_or(rest.len()); let t = rest[..end].trim(); if !t.is_empty() { return Some(t.to_string()); } }
    let low = reply.to_ascii_lowercase();
    let start = low.find("<!doctype").or_else(|| low.find("<html"));
    if let Some(a) = start { let end = low.rfind("</html>").map(|e| e + 7).unwrap_or(reply.len()); return Some(reply[a..end].trim().to_string()); }
    let t = reply.trim(); if t.starts_with('<') && t.contains("</") { Some(t.to_string()) } else { None }
}
/// What the design agent is told. Pure - tested.
pub fn design_prompt(ask: &str, project: &str) -> String {
    format!("You are a product designer making a MOCKUP for the project \"{project}\".\n\nThe ask:\n{ask}\n\nAnswer with ONE complete, self-contained HTML document and nothing else (no explanation, no markdown outside the code block).\nRules: all CSS inline in a <style> block; NO <script>, no event handlers, no iframes; NO network addresses of any kind (no external fonts, images, stylesheets): draw icons and pictures with inline SVG or CSS, or data: URIs; a fixed design width of 1280px for a screen (390px for a phone screen if the ask says phone); realistic copy and numbers, never lorem ipsum; real <button>, <a>, <input> elements; readable contrast; one clear visual idea carried through the whole page. The document is shown in an editor where each element can be moved and restyled, so use plain elements and inline-block, flex and grid layout, not canvas or images of text.")
}

fn dir_of(root: &str) -> PathBuf { PathBuf::from(root).join(".antiloki").join("frames") }
fn read_index(root: &str) -> Vec<Value> { std::fs::read_to_string(dir_of(root).join("index.json")).ok().and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok()).unwrap_or_default() }
fn write_index(root: &str, v: &[Value]) -> Result<(), String> { let d = dir_of(root); std::fs::create_dir_all(&d).map_err(|e| e.to_string())?; std::fs::write(d.join("index.json"), serde_json::to_string_pretty(v).unwrap_or_default()).map_err(|e| e.to_string()) }
fn mint(root: &str) -> String { let n = read_index(root).len() as i64; format!("m{}{}", (now() / 1000) % 1_000_000, n) }
/// A working entry older than this was cut off (the serve stopped): it is shown as failed, not as working forever.
const STALE_MS: i64 = 8 * 60 * 1000;
fn tidy(mut list: Vec<Value>) -> Vec<Value> {
    let n = now();
    for e in list.iter_mut() { if e["status"] == json!("working") && n - e["created_ms"].as_i64().unwrap_or(0) > STALE_MS { e["status"] = json!("failed"); e["error"] = json!("it was cut off: the serve stopped while it worked"); } }
    list.sort_by(|a, b| b["created_ms"].as_i64().cmp(&a["created_ms"].as_i64())); list
}
fn save_entry(root: &str, entry: Value, html: Option<&str>) -> Result<(), String> {
    let mut list = read_index(root); let id = s(&entry["id"]);
    if let Some(h) = html { let d = dir_of(root); std::fs::create_dir_all(&d).map_err(|e| e.to_string())?; std::fs::write(d.join(format!("{id}.html")), h).map_err(|e| e.to_string())?; }
    list.retain(|e| e["id"] != entry["id"]); list.push(entry); write_index(root, &list)
}

/// THE PROJECT AN AGENT'S CALL IS ABOUT: its own box is a git worktree of the project, so the project is the parent of the box's common git dir. (The workspace the person has open may be another one: a mockup an agent makes
/// must land in ITS project, where the person who asked will look. Found 2026-11-11 with a real Claude Code agent: the mockup went to the serve's active workspace, not the screen's.) Pure over the path - tested.
pub fn project_of_common_dir(common_dir: &str) -> Option<String> {
    let p = std::path::Path::new(common_dir.trim()); if p.file_name().map_or(true, |n| n != ".git") { return None; }
    p.parent().map(|d| d.to_string_lossy().to_string()).filter(|d| !d.is_empty())
}
async fn root_for(state: &Arc<AppState>, pane_id: i64) -> String {
    if pane_id > 0 {
        if let Some(row) = crate::chat_pane::pane_row(state, pane_id).await {
            let wt = row["worktree_path"].as_str().unwrap_or("").to_string();
            if !wt.is_empty() && std::path::Path::new(&wt).exists() {
                let got = tokio::task::spawn_blocking(move || crate::proc::cmd("git").arg("-C").arg(&wt).args(["rev-parse", "--path-format=absolute", "--git-common-dir"]).output().ok().filter(|o| o.status.success()).and_then(|o| project_of_common_dir(&String::from_utf8_lossy(&o.stdout)))).await.ok().flatten();
                if let Some(r) = got { return r; }
            }
        }
    }
    crate::agents_http::active_root(state).await
}
/// Show the mockup to the person: the Cowork screen opens (or focuses) the Frames pane with it selected (operator 2026-11-11: "it did not open Frames and it should, to show me the work"). Nothing happens when no screen is open.
fn show(id: &str) { let id = id.to_string(); tokio::spawn(async move { let _ = crate::cowork_mcp::run_on_screen("open", &json!({ "kind": "studio", "tool": "frames", "frame": id, "side": "right" }), 4_000).await; }); }
pub async fn list_http(state: Arc<AppState>, q: std::collections::HashMap<String, String>) -> Response { let root = root_for(&state, q.get("pane_id").and_then(|v| v.parse().ok()).unwrap_or(0)).await; reply(json!({ "ok": true, "frames": tidy(read_index(&root)) })) }
pub async fn html_http(state: Arc<AppState>, q: std::collections::HashMap<String, String>) -> Response {
    let root = root_for(&state, q.get("pane_id").and_then(|v| v.parse().ok()).unwrap_or(0)).await; let id = q.get("id").cloned().unwrap_or_default();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return reply(json!({ "ok": false, "error": "no such mockup" })); }
    match std::fs::read_to_string(dir_of(&root).join(format!("{id}.html"))) { Ok(h) => reply(json!({ "ok": true, "html": h })), Err(_) => reply(json!({ "ok": false, "error": "no such mockup" })) }
}

pub async fn post(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let root = root_for(&state, b["pane_id"].as_i64().unwrap_or(0)).await; if root.is_empty() { return reply(json!({ "ok": false, "error": "no workspace is open" })); }
    match s(&b["op"]).as_str() {
        "create" => {
            let html = sanitize_html(&s(&b["html"])); if html.trim().is_empty() { return reply(json!({ "ok": false, "error": "the mockup has no HTML" })); }
            let name = { let n = s(&b["name"]); if n.is_empty() { "Mockup".to_string() } else { n.chars().take(80).collect() } };
            let id = mint(&root);
            let entry = json!({ "id": id, "name": name, "ask": s(&b["ask"]).chars().take(600).collect::<String>(), "author": { "kind": "agent", "pane_id": b["pane_id"].as_i64().unwrap_or(0) }, "status": "done", "created_ms": now() });
            match save_entry(&root, entry.clone(), Some(&html)) { Ok(_) => { if b["pane_id"].as_i64().unwrap_or(0) > 0 { show(&id); } reply(json!({ "ok": true, "frame": entry })) } Err(e) => reply(json!({ "ok": false, "error": e })) }
        }
        "ask" => {
            let ask = s(&b["prompt"]); if ask.is_empty() { return reply(json!({ "ok": false, "error": "say what the mockup is of" })); }
            let name = { let n = s(&b["name"]); if n.is_empty() { ask.lines().next().unwrap_or("Mockup").chars().take(60).collect::<String>() } else { n.chars().take(80).collect() } };
            let engine_key = s(&b["engine"]); let run_engine = cli::engine_of(&json!(engine_key)).unwrap_or_else(cli::auto_engine);
            let id = mint(&root);
            let entry = json!({ "id": id, "name": name, "ask": ask.chars().take(600).collect::<String>(), "author": { "kind": "agent", "engine": run_engine.key() }, "status": "working", "created_ms": now() });
            if let Err(e) = save_entry(&root, entry.clone(), None) { return reply(json!({ "ok": false, "error": e })); }
            let (r2, id2, ask2) = (root.clone(), id.clone(), ask.clone());
            if b["pane_id"].as_i64().unwrap_or(0) > 0 { show(&id); }
            tokio::spawn(async move { make(r2, id2, ask2, run_engine).await; });
            reply(json!({ "ok": true, "frame": entry }))
        }
        "rename" => { let mut list = read_index(&root); let n: String = s(&b["name"]).chars().take(80).collect(); if n.is_empty() { return reply(json!({ "ok": false, "error": "a name is needed" })); } let mut hit = false; for e in list.iter_mut() { if e["id"] == b["id"] { e["name"] = json!(n); hit = true; } } if !hit { return reply(json!({ "ok": false, "error": "no such mockup" })); } match write_index(&root, &list) { Ok(_) => reply(json!({ "ok": true })), Err(e) => reply(json!({ "ok": false, "error": e })) } }
        "delete" => { let id = s(&b["id"]); let mut list = read_index(&root); let n = list.len(); list.retain(|e| s(&e["id"]) != id); if list.len() == n { return reply(json!({ "ok": false, "error": "no such mockup" })); } let _ = std::fs::remove_file(dir_of(&root).join(format!("{id}.html"))); match write_index(&root, &list) { Ok(_) => reply(json!({ "ok": true })), Err(e) => reply(json!({ "ok": false, "error": e })) } }
        other => reply(json!({ "ok": false, "error": format!("op is one of: ask, create, rename, delete (not `{other}`)") })),
    }
}

/// The design agent's call: the seat the record picks for a large edit, one turn, three minutes. Whatever it answers is extracted, sanitized and stored; a failure is the entry's `error`.
async fn make(root: String, id: String, ask: String, engine: cli::Engine) {
    let project = std::path::Path::new(&root).file_name().and_then(|n| n.to_str()).unwrap_or("the project").to_string();
    let (winner, fallback) = cli::job_binding_any(Job::EditorL, engine, None);
    let mut last = String::from("no seat could make it");
    let mut done: Option<(String, cli::Engine, Option<String>)> = None;
    for bnd in [winner, fallback] {
        if !cli::seat_ok(bnd.engine, bnd.model.as_deref()) { last = "no seat is signed in and free right now (Settings › Seats)".into(); continue; }
        let opts = RunOpts { model: bnd.model.clone(), max_turns: Some(1), timeout_ms: Some(180_000), ledger: Some(crate::ledger::Ctx::workspace(&root, "frames")), ..Default::default() };
        let msgs = vec![ChatMessage { role: "user".into(), content: design_prompt(&ask, &project) }];
        match cli::run_cli(bnd.engine, &msgs, &root, false, opts, None).await {
            Ok(out) => match extract_html(&out.parsed.content) { Some(h) => { let clean = sanitize_html(&h); if clean.trim().len() > 40 { done = Some((clean, bnd.engine, bnd.model.clone())); break; } last = "the answer had no usable HTML".into(); } None => { last = "the answer was not a page".into(); } },
            Err(e) => { last = e.chars().take(200).collect(); }
        }
    }
    let mut entry = read_index(&root).into_iter().find(|e| s(&e["id"]) == id).unwrap_or(json!({ "id": id, "name": "Mockup", "ask": ask, "created_ms": now() }));
    match done {
        Some((html, eng, model)) => { entry["status"] = json!("done"); entry["author"] = json!({ "kind": "agent", "engine": eng.key(), "model": model }); entry.as_object_mut().map(|o| o.remove("error")); let _ = save_entry(&root, entry, Some(&html)); }
        None => { entry["status"] = json!("failed"); entry["error"] = json!(last); let _ = save_entry(&root, entry, None); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_mockup_loses_scripts_handlers_embedded_pages_and_every_network_address() {
        let dirty = r#"<html><head><link rel="stylesheet" href="https://x.io/a.css"><style>@import url(https://x.io/f.css); .a{background:url(https://x.io/i.png)} .b{background:url(data:image/png;base64,AAA)}</style><script>alert(1)</script></head>
<body onload="x()"><a href="javascript:alert(1)" onclick='y()'>go</a><img src="https://t.io/p.gif" alt="a"><img src="data:image/svg+xml;utf8,<svg/>"><iframe src="https://e.io"></iframe><form action="//evil.io"><input></form><SCRIPT SRC=x></SCRIPT></body></html>"#;
        let c = sanitize_html(dirty);
        for bad in ["<script", "alert(1)", "onload", "onclick", "javascript:", "<iframe", "https://", "//evil", "<link", "@import"] { assert!(!c.to_lowercase().contains(bad), "`{bad}` is gone: {c}"); }
        assert!(c.contains("data:image/png;base64,AAA") && c.contains("data:image/svg+xml") && c.contains("<button") == false && c.contains("<input>") && c.contains("go</a>"), "what is safe stays: {c}");
        assert!(c.contains(r##"href="#""##), "{c}");
    }
    #[test]
    fn the_document_is_found_in_a_fence_or_in_the_text_or_not_at_all() {
        assert_eq!(extract_html("Here you go:\n```html\n<div>a</div>\n```\nenjoy").as_deref(), Some("<div>a</div>"));
        assert_eq!(extract_html("sure! <!DOCTYPE html><html><body>x</body></html> done").as_deref(), Some("<!DOCTYPE html><html><body>x</body></html>"));
        assert_eq!(extract_html("<div>just markup</div>").as_deref(), Some("<div>just markup</div>")); assert_eq!(extract_html("I cannot do that."), None);
    }
    #[test]
    fn the_design_agent_is_told_the_ask_and_the_rules_that_keep_the_page_safe_and_editable() {
        let p = design_prompt("a settings screen", "shop"); assert!(p.contains("a settings screen") && p.contains("\"shop\"") && p.contains("NO <script>") && p.contains("NO network addresses") && p.contains("1280px"));
    }
    #[test]
    fn the_project_of_an_agents_box_is_the_parent_of_its_common_git_dir() {
        assert_eq!(project_of_common_dir("/home/me/Projects/shop/.git\n").as_deref(), Some("/home/me/Projects/shop")); assert_eq!(project_of_common_dir("/x/.git/worktrees/a"), None); assert_eq!(project_of_common_dir(""), None);
    }
    #[test]
    fn a_working_entry_that_was_cut_off_is_shown_as_failed() {
        let old = json!({ "id": "a", "status": "working", "created_ms": now() - STALE_MS - 1000 }); let fresh = json!({ "id": "b", "status": "working", "created_ms": now() });
        let t = tidy(vec![old, fresh]); let by = |id: &str| t.iter().find(|e| e["id"] == json!(id)).unwrap().clone();
        assert_eq!(by("a")["status"], json!("failed")); assert_eq!(by("b")["status"], json!("working"));
    }
}
