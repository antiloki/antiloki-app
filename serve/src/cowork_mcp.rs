//! THE PANE TOOLS OF ANTILOKI'S MCP (RFC 0007 §17.10). An agent in a terminal pane can look at the screen it is in and use it: list the panes, open a terminal (to run the tests where
//! the person can watch them) or another agent, type into a terminal it opened, read what a terminal shows, close it. Every one of them goes through the screen (`cowork_cmds`), so
//! what the agent opens is a real pane in the person's layout, not a hidden process.
//!
//!   POST /api/v1/cockpit/cowork/mcp   `{pane_id, tool, args}` -> `{ok, text}`   (the door `antiloki-agent mcp` uses)
//!
//! The renderers are pure; the handlers only find the layout file, the clock and the terminals.
use crate::cowork_cmds as cmds;
use crate::dispatch::AppState;
use crate::pty::PtyRegistry;
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::sync::Arc;

/// How long the serve waits for the screen to do what an agent asked.
const SCREEN_WAIT_MS: u64 = 15_000;
/// A terminal's text is returned from the bottom, at most this many rows.
const READ_ROWS: usize = 40;
const READ_ROWS_MAX: usize = 200;

fn body_json(b: &Bytes) -> Value { serde_json::from_slice(b).unwrap_or(Value::Null) }
fn reply(s: StatusCode, v: Value) -> Response { (s, Json(v)).into_response() }
fn s(v: &Value) -> String { v.as_str().unwrap_or("").trim().to_string() }

// ── the channel's own routes ──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

pub async fn commands_get() -> Response {
    let list = cmds::with(|st| st.pending(crate::cli::now_ms()));
    reply(StatusCode::OK, json!({ "ok": true, "commands": list }))
}
pub async fn commands_result(body: Bytes) -> Response {
    let b = body_json(&body); let id = b["id"].as_u64().unwrap_or(0);
    match cmds::with(|st| st.report(id, &b)) { Ok(()) => reply(StatusCode::OK, json!({ "ok": true })), Err(e) => reply(StatusCode::NOT_FOUND, json!({ "ok": false, "error": e })) }
}
/// Ask the open screen to do something and wait for what it says. `Err` is a sentence the agent can act on.
pub async fn run_on_screen(op: &str, args: &Value, wait_ms: u64) -> Result<Value, String> {
    let now = crate::cli::now_ms();
    if !cmds::with(|st| st.screen_watching(now)) { return Err("no antiloki screen is open to show it: open the Cowork screen in the app first".into()); }
    let id = cmds::with(|st| st.enqueue(op, args, now))?;
    let end = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
    loop {
        if let Some(r) = cmds::with(|st| st.take_result(id)) {
            if r["ok"] == json!(false) { let e = s(&r["error"]); return Err(if e.is_empty() { "the screen could not do that".to_string() } else { e.chars().take(300).collect() }); }
            return Ok(r);
        }
        if std::time::Instant::now() >= end { cmds::with(|st| st.forget(id)); return Err("the screen did not answer in time (is it still open?)".into()); }
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    }
}
pub async fn commands_run(body: Bytes) -> Response {
    let b = body_json(&body);
    match run_on_screen(&s(&b["op"]), &b["args"], b["wait_ms"].as_u64().unwrap_or(SCREEN_WAIT_MS).min(60_000)).await {
        Ok(r) => reply(StatusCode::OK, json!({ "ok": true, "result": r })),
        Err(e) => reply(StatusCode::OK, json!({ "ok": false, "error": e })),
    }
}

// ── reading the layout ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

/// One pane of the layout as the tools name it.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneInfo { pub id: String, pub kind: String, pub title: String, pub what: String, pub pane_id: i64, pub focused: bool }

/// The leaves of a stored layout, in reading order. Pure.
pub fn panes_of(layout: &Value) -> Vec<PaneInfo> {
    fn walk(n: &Value, out: &mut Vec<Value>) {
        match n["type"].as_str() { Some("leaf") => out.push(n["pane"].clone()), Some("split") => for c in n["children"].as_array().into_iter().flatten() { walk(c, out); }, _ => {} }
    }
    let mut leaves = vec![]; walk(&layout["root"], &mut leaves);
    let focus = s(&layout["focus"]);
    leaves.iter().map(|p| {
        let kind = s(&p["kind"]);
        let what = match kind.as_str() { "engine" => s(&p["engine"]), "tool" => s(&p["tool"]), "worker" => "delegated worker".into(), _ => "terminal".into() };
        PaneInfo { id: s(&p["id"]), title: { let t = s(&p["title"]); if t.is_empty() { what.clone() } else { t } }, kind, what, pane_id: p["paneId"].as_i64().unwrap_or(0), focused: s(&p["id"]) == focus }
    }).filter(|p| !p.id.is_empty()).collect()
}
/// The engine of the agent that is asking, when it is one on the screen.
fn asking(panes: &[PaneInfo], you: i64) -> Option<crate::cli::Engine> { panes.iter().find(|p| you > 0 && p.pane_id == you && p.kind == "engine").and_then(|p| crate::cli::seat_engine(&p.what)) }
/// The linked-agent teams, then the team designs of this project. Pure.
fn with_designs(said: &str, designs: &[String]) -> String { if designs.is_empty() { said.to_string() } else { format!("linked agents: {said}\n\nteam designs here (antiloki_team_status shows one, antiloki_team_run starts it):\n{}", designs.join("\n")) } }
const NO_PANES: &str = "no screen has reported any pane (none is open, or the screen was just opened or reloaded: ask again in a few seconds).";
/// The panes as lines an agent can use.
pub fn render_panes(panes: &[PaneInfo], you: i64) -> String {
    if panes.is_empty() { return NO_PANES.into(); }
    panes.iter().map(|p| format!("{}{} [{}] {} · {}{}", p.id, if p.pane_id > 0 { format!(" (pane {})", p.pane_id) } else { String::new() }, p.kind, p.title, p.what,
        match (p.focused, p.pane_id == you && you > 0) { (true, true) => "  ← focused, this is you", (true, false) => "  ← focused", (false, true) => "  ← this is you", _ => "" })).collect::<Vec<_>>().join("\n")
}
/// Which pane an argument names: the layout id (`shell-12`), the number (`12`), or `"this"` for the caller's own.
pub fn resolve_pane<'a>(panes: &'a [PaneInfo], arg: &Value, you: i64) -> Option<&'a PaneInfo> {
    let by_num = arg.as_i64().or_else(|| arg.as_str().and_then(|x| x.trim().parse::<i64>().ok()));
    if let Some(n) = by_num { return panes.iter().find(|p| p.pane_id == n); }
    let t = s(arg); if t == "this" || t == "me" { return panes.iter().find(|p| p.pane_id == you); }
    panes.iter().find(|p| p.id == t)
}
/// What a terminal shows, from the bottom: trailing blank rows dropped, at most `n` rows, and trailing spaces trimmed. Pure.
pub fn render_screen(rows: &[String], n: usize) -> String {
    let mut r: Vec<String> = rows.iter().map(|x| x.trim_end().to_string()).collect();
    while r.last().map_or(false, |x| x.is_empty()) { r.pop(); }
    let n = n.clamp(1, READ_ROWS_MAX); let skip = r.len().saturating_sub(n);
    let out = r[skip..].join("\n");
    if out.is_empty() { "(the terminal shows nothing yet)".into() } else { out }
}

// ── THE SHAPE OF THE LAYOUT, AND HOW THE SCREEN LOOKS ─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

/// The layout as a tree an agent can read before it rearranges anything: each split says which way it runs (`row` side by side, `col` stacked) and each child its share. Pure.
pub fn render_tree(layout: &Value) -> String {
    fn node(n: &Value, share: Option<f64>, indent: &str, last: bool, out: &mut Vec<String>) {
        let (branch, next) = if indent.is_empty() && share.is_none() { (String::new(), String::new()) } else { (format!("{indent}{}", if last { "└─ " } else { "├─ " }), format!("{indent}{}", if last { "   " } else { "│  " })) };
        let pct = share.map(|x| format!("{}% ", (x * 100.0).round() as i64)).unwrap_or_default();
        match n["type"].as_str() {
            Some("leaf") => { let p = &n["pane"]; let t = s(&p["title"]); out.push(format!("{branch}{pct}{}{}", s(&p["id"]), if t.is_empty() { String::new() } else { format!(" \"{t}\"") })); }
            Some("split") => {
                out.push(format!("{branch}{pct}{} ({})", if n["dir"] == "row" { "row" } else { "col" }, if n["dir"] == "row" { "side by side" } else { "stacked" }));
                let kids = n["children"].as_array().cloned().unwrap_or_default(); let total: f64 = n["sizes"].as_array().map(|a| a.iter().filter_map(Value::as_f64).sum()).unwrap_or(0.0);
                for (i, c) in kids.iter().enumerate() { let sz = n["sizes"][i].as_f64().filter(|_| total > 0.0).map(|x| x / total); node(c, sz, &next, i + 1 == kids.len(), out); }
            }
            _ => {}
        }
    }
    if layout["root"].is_null() { return String::new(); }
    let mut out = vec![]; node(&layout["root"], None, "", true, &mut out);
    let zoom = s(&layout["zoom"]); if !zoom.is_empty() { out.push(format!("(zoomed: {zoom} fills the screen)")); }
    out.join("\n")
}
/// A number the caller means as a fraction: 0.4 is 40 %, and 40 is 40 % too. `None` when it is not a number.
pub fn unit(v: &Value) -> Option<f64> { let x = v.as_f64().or_else(|| v.as_str().and_then(|t| t.trim().trim_end_matches('%').parse::<f64>().ok()))?; Some((if x > 1.0 { x / 100.0 } else { x }).clamp(0.0, 1.0)) }
fn pct(v: &Value) -> String { format!("{}%", (v.as_f64().unwrap_or(0.0) * 100.0).round() as i64) }
/// What the screen answered about how it looks, as a few lines. Pure.
pub fn render_look(data: &Value) -> String {
    let a = &data["appearance"];
    let theme = if data["theme"].is_null() { "antiloki's own colours".to_string() } else { format!("{} ({})", s(&data["theme"]["name"]), s(&data["theme"]["id"])) };
    let wall = { let w = s(&a["wallpaper"]); if a["wallpaper"].as_str() == Some("") { "none".to_string() } else if w.is_empty() { data["wallpapers"].get(0).and_then(Value::as_str).map(|f| format!("{f} (the theme's first)")).unwrap_or_else(|| "none (this theme has no wallpaper)".into()) } else { w } };
    let mut out = vec![format!("terminals {} opaque, panes {} opaque, wallpaper darkened {}", pct(&a["termOpacity"]), pct(&a["paneOpacity"]), pct(&a["dim"])), format!("theme: {theme}{}", if a["follow"] == true { ", following the desktop" } else { "" }), format!("wallpaper: {wall}")];
    let w: Vec<&str> = data["wallpapers"].as_array().into_iter().flatten().filter_map(Value::as_str).collect(); if !w.is_empty() { out.push(format!("this theme's wallpapers: {}", w.join(", "))); }
    let t: Vec<String> = data["themes"].as_array().into_iter().flatten().map(|x| s(&x["id"])).collect(); if !t.is_empty() { out.push(format!("themes here: {}", t.join(", "))); }
    out.join("\n")
}
/// What the screen answered about the sidebar and the workspaces. Pure.
pub fn render_workspace(data: &Value) -> String {
    let ws: Vec<String> = data["workspaces"].as_array().into_iter().flatten().map(|w| format!("{}{}", s(&w["name"]), if w["active"] == true { " (open)" } else { "" })).collect();
    format!("sidebar: {}\nworkspaces: {}", s(&data["sidebar"]), if ws.is_empty() { "none".to_string() } else { ws.join(", ") })
}
/// A themes overview as lines. Pure.
pub fn render_themes(v: &Value) -> String {
    let mut out = vec![];
    if let Some(c) = v["current"].as_str() { out.push(format!("the desktop is wearing: {c}")); }
    for t in v["themes"].as_array().into_iter().flatten() {
        let w: Vec<&str> = t["wallpapers"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        out.push(format!("{} ({}, {}): {}", s(&t["id"]), s(&t["name"]), s(&t["mode"]), if w.is_empty() { "no wallpapers".to_string() } else { w.join(", ") }));
    }
    if out.is_empty() { "no themes are installed.".into() } else { out.join("\n") }
}

/// A pane as the agent sees it: where it is, what is written in it and every control it has, each with a ref the next act can name. Pure.
pub fn render_view(pane: &str, data: &Value) -> String {
    let v = &data["view"]; let title = s(&data["title"]);
    let mut out = vec![format!("pane {pane}{}{}", if title.is_empty() { String::new() } else { format!(" \"{title}\"") }, { let t = s(&data["tool"]); if t.is_empty() { String::new() } else { format!(" ({t})") } })];
    let els = v["elements"].as_array().cloned().unwrap_or_default();
    if !els.is_empty() {
        out.push("controls (name one by its ref, its test id or its text):".into());
        for e in &els {
            let mut l = format!("  [{}] {} \"{}\"", s(&e["ref"]), s(&e["role"]), s(&e["name"]));
            if let Some(val) = e["value"].as_str() { l.push_str(&format!(" value={}", serde_json::to_string(val).unwrap_or_default())); }
            if let Some(c) = e["checked"].as_bool() { l.push_str(if c { " (on)" } else { " (off)" }); }
            if e["disabled"] == true { l.push_str(" (disabled)"); }
            if let Some(o) = e["options"].as_array() { l.push_str(&format!(" options: {}", o.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" | "))); }
            if let Some(t) = e["testid"].as_str() { l.push_str(&format!("  #{t}")); }
            out.push(l);
        }
    } else { out.push("(no controls on it)".into()); }
    for f in v["frames"].as_array().into_iter().flatten() { out.push(format!("a frame \"{}\" shows {} (what is inside a frame cannot be reached from here)", s(&f["name"]), s(&f["src"]))); }
    let text = s(&v["text"]); if !text.is_empty() { out.push(format!("\nwhat it says:\n{text}")); }
    if v["truncated"] == true { out.push("(cut: ask for more text with `text_chars`, or narrow it down by acting)".into()); }
    out.join("\n")
}

// ── worktrees ─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

/// One box (the project's own checkout, or an agent's worktree) as `git status` reports it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Box { pub who: String, pub engine: String, pub pane_id: i64, pub path: String, pub branch: String, pub changed: usize, pub ahead: usize, pub behind: usize, pub upstream: bool }

/// `git status --porcelain -b`: the branch line (`## feat...origin/feat [ahead 2, behind 1]`) and one line per changed file. Pure.
pub fn parse_status(text: &str) -> (String, bool, usize, usize, usize) {
    let mut lines = text.lines();
    let head = lines.next().unwrap_or("").trim_start_matches("## ").to_string();
    let changed = lines.filter(|l| !l.trim().is_empty()).count();
    let (names, counts) = match head.split_once(" [") { Some((n, c)) => (n.to_string(), c.trim_end_matches(']').to_string()), None => (head.clone(), String::new()) };
    let upstream = names.contains("...");
    let branch = names.split("...").next().unwrap_or("").trim_start_matches("No commits yet on ").to_string();
    let num = |key: &str| counts.split(", ").find_map(|p| p.strip_prefix(key)).and_then(|n| n.trim().parse::<usize>().ok()).unwrap_or(0);
    (branch, upstream, changed, num("ahead "), num("behind "))
}
/// The boxes as lines an agent can use: who is in each, what it is on, and whether it has something to ship.
pub fn render_worktrees(boxes: &[Box], you: i64) -> String {
    if boxes.is_empty() { return "no worktrees found.".into(); }
    boxes.iter().map(|b| {
        let mut bits = vec![format!("branch {}", if b.branch.is_empty() { "(detached)" } else { &b.branch })];
        bits.push(if b.changed == 0 { "clean".into() } else { format!("{} changed", b.changed) });
        if b.upstream { if b.ahead > 0 { bits.push(format!("{} ahead", b.ahead)); } if b.behind > 0 { bits.push(format!("{} behind", b.behind)); } } else { bits.push("not pushed".into()); }
        format!("{}{} · {} · {}{}", b.who, if b.pane_id > 0 { format!(" (pane {})", b.pane_id) } else { String::new() }, bits.join(" · "), b.path, if b.pane_id == you && you > 0 { "  ← your box" } else { "" })
    }).collect::<Vec<_>>().join("\n")
}
fn git(dir: &str, args: &[&str]) -> Option<String> {
    let o = crate::proc::cmd("git").args(args).current_dir(dir).output().ok()?; if !o.status.success() { return None; }
    Some(String::from_utf8_lossy(&o.stdout).to_string())
}
/// Every agent box of the open project, and the project's own checkout, read from git. A blocking read; call it off the runtime.
fn worktrees(state: &Arc<AppState>) -> Vec<Box> {
    let root = crate::chat_pane::workspace_root_of(state);
    let conn = state.pick();
    let rows = { let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        antiloki_rs::db_json::query_rows(&c, "SELECT id, subagent_name, pane_kind, worktree_path, settings_json FROM cockpit_panes WHERE COALESCE(status,'') != 'closed' AND COALESCE(worktree_path,'') != '' AND pane_kind IN ('claude-code','codex','opencode','gemini','cursor','grok')", &[]).unwrap_or(json!([])) };
    let mut out = vec![];
    let mut add = |who: String, engine: String, pane_id: i64, path: String| {
        if !std::path::Path::new(&path).is_dir() { return; }
        let (branch, upstream, changed, ahead, behind) = git(&path, &["status", "--porcelain", "-b"]).map(|t| parse_status(&t)).unwrap_or_default();
        out.push(Box { who, engine, pane_id, path, branch, changed, ahead, behind, upstream });
    };
    if !root.is_empty() { add("the project's own checkout".into(), String::new(), 0, root.clone()); }
    for r in rows.as_array().cloned().unwrap_or_default() {
        let title = serde_json::from_str::<Value>(r["settings_json"].as_str().unwrap_or("{}")).ok().and_then(|v| v["title"].as_str().map(String::from)).filter(|t| !t.is_empty());
        let engine = s(&r["pane_kind"]);
        add(title.unwrap_or_else(|| engine.clone()), engine, r["id"].as_i64().unwrap_or(0), s(&r["worktree_path"]));
    }
    out
}

// ── QA IN A PANE THE PERSON WATCHES ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────
//
// `antiloki_qa` runs the deterministic QA executor (the same one the QA gate uses: a fixed viewport, animations off, the page's console errors collected) over a plan the agent writes, and
// opens a QA pane on the screen that shows the browser's frames as they come, the step being played, and each step's result. The agent gets the verdict and the screenshots' paths (it can
// read the PNGs). The executor is not changed; this is the door and the window.
//
//   GET /api/v1/cockpit/cowork/qa/view?id=N   the live snapshot of run N (`{type:"snapshot", timeline, last_frame, done}`), for the QA pane

pub async fn qa_view(q: std::collections::HashMap<String, String>) -> Response {
    let Some(id) = q.get("id").and_then(|v| v.trim().parse::<i64>().ok()) else { return reply(StatusCode::BAD_REQUEST, json!({ "ok": false, "error": "id required" })) };
    match crate::qa_evidence::hub_join(id) {
        Some((snap, _rx)) => reply(StatusCode::OK, json!({ "ok": true, "snapshot": snap })),
        None => reply(StatusCode::NOT_FOUND, json!({ "ok": false, "error": "this server holds no QA run with that id (it may have been run before a restart)" })),
    }
}

/// The verdict of a run, in lines an agent can act on. Pure over the evidence's parts.
pub fn render_qa(checks: &[(String, bool, String)], failed_steps: usize, console_errors: &[String], log: &[String], shots: &[(String, String)], app: &str) -> String {
    let failed = checks.iter().filter(|c| !c.1).count();
    let mut out = vec![format!("QA on {app}: {} step{} failed, {}/{} checks passed{}", failed_steps, if failed_steps == 1 { "" } else { "s" }, checks.len() - failed, checks.len(), if console_errors.is_empty() { String::new() } else { format!(", {} console error{}", console_errors.len(), if console_errors.len() == 1 { "" } else { "s" }) })];
    for c in checks.iter().filter(|c| !c.1) { out.push(format!("✗ {} (the page had: {})", c.0, c.2.chars().take(160).collect::<String>())); }
    for e in console_errors.iter().take(5) { out.push(format!("console error: {}", e.chars().take(200).collect::<String>())); }
    out.push("steps:".into()); out.extend(log.iter().take(40).map(|l| format!("  {l}")));
    if !shots.is_empty() { out.push("screenshots (read them to see the page):".into()); out.extend(shots.iter().map(|(n, p)| format!("  {n}: {p}"))); }
    out.join("\n")
}

pub(crate) fn pane_cwd(state: &Arc<AppState>, pane_id: i64) -> Option<String> {
    let conn = state.pick(); let c = conn.lock().unwrap_or_else(|p| p.into_inner());
    antiloki_rs::db_json::query_rows(&c, "SELECT worktree_path FROM cockpit_panes WHERE id = ?1", &[json!(pane_id)]).ok()?.as_array()?.first()?.get("worktree_path")?.as_str().map(String::from).filter(|p| !p.is_empty())
}

async fn qa(state: &Arc<AppState>, args: &Value, you: i64) -> Result<String, String> {
    let steps = crate::qa_evidence::parse_plan(&args["steps"]).map_err(|e| format!("the QA plan is not valid: {e}. A plan is a JSON array of steps like [{{\"do\":\"goto\",\"url\":\"/\"}},{{\"do\":\"click\",\"target\":\"text=Add to cart\"}},{{\"do\":\"expect\",\"text\":\"1 item\"}},{{\"do\":\"shot\",\"name\":\"cart\"}}]"))?;
    let cwd = match pane_cwd(state, you) { Some(own) => own, None => { let row = crate::chat_pane::pane_row(state, you).await.ok_or("no such pane")?; Some(crate::shared_folder::working_dir(state, &row).await).filter(|d| !d.is_empty()).ok_or("this pane has no folder to run the app from")? } };
    let app = s(&args["url"]); if !app.is_empty() { crate::qa_plans::app_ok(&app)?; }
    let run_id = crate::cli::now_ms();
    let out_dir = crate::qa_evidence::evidence_dir(&cwd, run_id);
    // a viewer counts as watching while it holds a receiver: the executor paces itself for someone who is looking (and runs flat out for nobody)
    let live = crate::qa_evidence::hub_open(run_id, Some(out_dir.join("film")), std::env::var("ANTILOKI_QA_PACE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(700));
    let _watching = crate::qa_evidence::hub_join(run_id).and_then(|(_, rx)| rx);
    let viewer = run_on_screen("open", &json!({ "kind": "studio", "tool": "qa", "run": run_id, "title": "QA", "side": "right" }), 8_000).await;
    let cwd2 = cwd.clone();
    let ev = tokio::task::spawn_blocking(move || -> Result<(crate::qa_evidence::Evidence, String), String> {
        let (url, _guard, _source) = crate::qa_plans::resolve_app(&cwd2, if app.is_empty() { None } else { Some(app.as_str()) }, "ui")?;
        let ev = crate::qa_evidence::run_plan_in(&url, Some(std::path::Path::new(&cwd2)), &steps, &out_dir, Some(live), false)?;
        Ok((ev, url))
    }).await.map_err(|e| e.to_string())??;
    let (ev, url) = ev;
    let shots: Vec<(String, String)> = ev.shots.iter().map(|(n, p)| (n.clone(), p.to_string_lossy().to_string())).collect();
    let text = render_qa(&ev.checks, ev.failed_steps, &ev.console_errors, &ev.log, &shots, &url);
    Ok(if viewer.is_err() { format!("{text}\n(no screen was open to show it live)") } else { text })
}

// ── the tools ─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

/// SAID WITH THE LIST, so an agent whose tool description is older than the serve still learns it (found 2026-11-26: a pane started before an update told the person "a theme cannot hold a picture").
pub const THEME_HINT: &str = "\n\nA custom theme CAN hold a picture wallpaper: antiloki_themes {op: save, theme: {..., wallpaper: {kind: \"image\", url: \"https://...\"}}} downloads it (or `path`: a file here, or `svg`: art you draw). For art from the internet use the address the person gives you, or a picture whose licence allows it, and say where it came from. Wear it with antiloki_look {theme: id, wallpaper: \"first\"}; custom themes work there.";
pub const TOOLS: [&str; 28] = ["antiloki_routine", "antiloki_group", "antiloki_queue", "antiloki_message", "antiloki_ask", "antiloki_browser", "antiloki_remember", "antiloki_machine", "antiloki_cost", "antiloki_team_templates", "antiloki_book", "antiloki_app_tools", "antiloki_free_pane_list", "antiloki_free_pane_write", "antiloki_free_pane_open", "antiloki_panes", "antiloki_pane_open", "antiloki_pane_read", "antiloki_pane_send", "antiloki_pane_close", "antiloki_worktrees", "antiloki_qa", "antiloki_pane_arrange", "antiloki_look", "antiloki_themes", "antiloki_screen", "antiloki_pane_view", "antiloki_pane_act"];

fn render_machine(v: &Value) -> String {
    let pct = |x: &Value| x["pct"].as_f64().map(|p| format!("{:.0}%", p)).unwrap_or_else(|| "not readable".into()); let mem = |x: &Value| if x.is_object() { format!("{} ({:.1} of {:.1} GB)", pct(x), x["used_mb"].as_f64().unwrap_or(0.0) / 1024.0, x["total_mb"].as_f64().unwrap_or(0.0) / 1024.0) } else { "no GPU found".into() };
    let mut o = format!("machine: {}\nCPU {} ({} cores)\nRAM {}\nVRAM {}", s(&v["tone"]), pct(&v["cpu"]), v["cpu"]["cores"], mem(&v["ram"]), mem(&v["vram"]));
    for p in v["procs"].as_array().into_iter().flatten().take(6) { o += &format!("\n- {} ({}) cpu {} ram {} MB", s(&p["name"]), s(&p["kind"]), p["cpu"].as_f64().map(|c| format!("{c:.0}%")).unwrap_or_else(|| "—".into()), p["ram_kb"].as_u64().unwrap_or(0) / 1024); }
    o += "\nA local model plus several agents is what fills VRAM and RAM: calm under 80%, loaded to 92%, danger above."; o
}
fn render_cost(v: &Value, days: i64) -> String {
    let mut o = format!("last {} (tokens priced at OpenRouter's list; subscriptions bill $0 metered): ${:.2}{}", if days == 0 { "day".to_string() } else { format!("{days} days") }, v["or_cost"].as_f64().unwrap_or(0.0), if v["or_prev"].as_f64().unwrap_or(0.0) > 0.0 { format!(" (the period before: ${:.2})", v["or_prev"].as_f64().unwrap_or(0.0)) } else { String::new() });
    for m in v["models"].as_array().into_iter().flatten().take(12) { o += &format!("\n- {} · {} · {} tokens · {} calls · {}", s(&m["house"]), s(&m["model"]), m["tokens"].as_u64().unwrap_or(0), m["calls"].as_u64().unwrap_or(0), match m["or_cost"].as_f64() { Some(c) => format!("${c:.2} (${} in / ${} out per 1M)", m["or_in"], m["or_out"]), None => "no price on OpenRouter's list".into() }); }
    for l in v["limits"].as_array().into_iter().flatten().filter(|l| l["used_pct"].is_number()).take(8) { o += &format!("\nlimit {} {}: {:.0}% used", s(&l["house"]), s(&l["label"]), l["used_pct"].as_f64().unwrap_or(0.0)); } o
}
async fn layout_of(state: &Arc<AppState>) -> Value {
    let root = crate::agents_http::active_root(state).await;
    let text = std::fs::read_to_string(crate::cowork_threads::layout_file(&root, &crate::cowork_threads::active(&root))).unwrap_or_default();
    serde_json::from_str(&text).unwrap_or(Value::Null)
}
pub(crate) async fn send_to(reg: &Arc<PtyRegistry>, pane_id: i64, text: &str) -> Result<(), String> {
    let r = crate::pty::pty_send(reg.clone(), Bytes::from(json!({ "pane_id": pane_id, "text": text }).to_string())).await;
    let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if v["ok"] == json!(true) { Ok(()) } else { Err(s(&v["error"])) }
}
/// THE PANE OF A SPECIALIST THAT CAN BE TYPED TO: the newest of its panes whose terminal is running. A pane that was closed or put to sleep stays bound to it, and a message "sent" there was lost (found 2026-11-26). Pure.
pub fn running_one(panes: &[i64], running: impl Fn(i64) -> bool) -> Option<i64> { panes.iter().copied().find(|p| running(*p)) }
fn terminal_runs(reg: &Arc<PtyRegistry>, pane_id: i64) -> bool { reg.status(pane_id)["running"] == json!(true) || crate::pty_remote::list_running().is_ok_and(|t| t.iter().any(|t| crate::pty_remote::pane_of(&t.id) == Some(pane_id))) }
/// How long the first message of a new pane is still tried: the terminal is up in seconds, but an agent asking its own question (Cursor's MCP approval) is answered by the person, in their own time. Pure.
fn keep_trying(error: &str, waited: std::time::Duration) -> bool { waited < std::time::Duration::from_secs(if error.contains("asking its own question") { 15 * 60 } else { 20 }) }
/// Type into a terminal that was just opened: the terminal starts a moment after its pane appears, so this tries for a while.
async fn send_when_up(reg: Arc<PtyRegistry>, pane_id: i64, text: String) {
    let t0 = std::time::Instant::now();
    while let Err(e) = send_to(&reg, pane_id, &text).await {
        if !keep_trying(&e, t0.elapsed()) { eprintln!("antiloki-serve: pane {pane_id} never got its first message: {e}"); return; }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// THE TEAM TOOL (operator 2026-11-10: "I asked an agent to create 2 new agents, QA and worker, and add them to my agent as a team, and it couldn't: the MCP needs team add, remove, edit"). Beyond the plain ops
/// (`list`, `link`, `unlink`, `rename`, `delete`, `run`, `status`, `stop`), which `team_run` does:
///   `add {team?, role, pane | engine, title?, prompt?}`  a member: an agent that is already there (`pane`: its number, or "this"), or a NEW one opened beside (`engine`, default claude-code). The team is made if it is not there yet;
///                                                         with no `team` it is the team of the agent asking, else the one team there is.
///   `remove {pane, close?}`                               takes a member out (the pane stays, writable again), or closes it as well
///   `edit {pane, role}`                                   changes a member's role (the role must be free)   /   `edit {team, name}` renames the team
///   `create {name, planner?, worker?, qa?}`               a team with its members at once; each member is a pane number, "this", or `{engine, title?, prompt?}` for a new agent
/// Pure over nothing: it opens panes on the screen, so it is tested through the real MCP.
async fn teams_tool(state: &Arc<AppState>, reg: &Arc<PtyRegistry>, args: &Value, you: i64, panes: &[PaneInfo]) -> Result<String, String> {
    let root = crate::agents_http::active_root(state).await;
    let call = |v: Value| { let (st, rg) = (state.clone(), reg.clone()); async move { let r = crate::team_run::handle(st, rg, v).await; if r["ok"] == json!(false) { Err(crate::team_run::render(&r)) } else { Ok(r) } } };
    let load = || crate::team_run::load_teams(&root);
    let near = asking(panes, you);
    // a member spec -> its pane number, opening a new agent when it names one
    let member = |spec: Value, title_fallback: String| { let (reg, panes) = (reg.clone(), panes.to_vec()); async move {
        if spec.is_null() { return Err("name the agent: `pane` (its number, or \"this\") or `engine` for a new one".to_string()); }
        if spec.is_object() && spec.get("pane").is_none() || spec.as_str() == Some("new") {
            let engine = { let e = s(&spec["engine"]); if e.is_empty() { crate::cli::agent_seat("", near).key().to_string() } else { e } }; let title = { let t = s(&spec["title"]); if t.is_empty() { title_fallback } else { t } };
            let r = run_on_screen("open", &json!({ "kind": "agent", "title": title, "side": s(&spec["side"]), "engine": engine, "tool": "" }), SCREEN_WAIT_MS).await?;
            let pid = r["pane_id"].as_i64().unwrap_or(0); if pid <= 0 { return Err("the screen opened the agent but did not say which pane it is: read antiloki_panes".to_string()); }
            let first = s(&spec["prompt"]); if !first.is_empty() { tokio::spawn(send_when_up(reg, pid, first)); }
            return Ok((pid, true));
        }
        let key = if spec.is_object() { spec["pane"].clone() } else { spec.clone() };
        let n = key.as_i64().or_else(|| key.as_str().and_then(|x| x.trim().parse().ok())).or_else(|| resolve_pane(&panes, &key, you).map(|p| p.pane_id)).ok_or("no such pane: name one from antiloki_panes (its number, or \"this\")")?;
        if n <= 0 { return Err("that pane is not an agent: only an agent can be in a team".to_string()); } Ok((n, false))
    } };
    let team_of_asker = || load().into_iter().find(|c| c.members().iter().any(|(_, p)| *p == you)).map(|c| c.id);
    let pick_team = |given: &str| -> Result<String, String> {
        if !given.is_empty() { return Ok(given.to_string()); }
        if let Some(c) = team_of_asker() { return Ok(c); }
        let all = load(); if all.len() == 1 { return Ok(all[0].id.clone()); }
        Err(if all.is_empty() { "there is no team yet: give a `team` name and it is made".into() } else { format!("which team? one of: {}", all.iter().map(|c| c.id.clone()).collect::<Vec<_>>().join(", ")) })
    };
    let role_of = |v: &Value| -> Result<String, String> { let r = s(v); if ["planner", "worker", "qa"].contains(&r.as_str()) { Ok(r) } else { Err("role is one of: planner, worker, qa".into()) } };
    match s(&args["op"]).as_str() {
        "add" => {
            let role = role_of(&args["role"])?; let given = s(&args["team"]);
            let team = if given.is_empty() { pick_team("")? } else { given };
            if !load().iter().any(|c| c.id == crate::team_run::slug(&team) || c.name.eq_ignore_ascii_case(team.trim())) { call(json!({ "op": "create", "name": team })).await?; }
            let spec = if args.get("pane").is_some() { json!(args["pane"]) } else if args.get("engine").is_some() || args["new"] == json!(true) { json!({ "engine": args["engine"], "title": args["title"], "prompt": args["prompt"], "side": args["side"] }) } else { Value::Null };
            let (pane, opened) = member(spec, format!("{} of {team}", role)).await?;
            match call(json!({ "op": "link", "team": team, "role": role, "pane": pane })).await {
                Ok(_) => Ok(format!("{} pane {pane} as the {role} of the team `{team}`. A team needs a planner and a worker (the QA is optional): add what is missing, then run it.", if opened { "opened and added" } else { "added" })),
                Err(e) => Err(if opened { format!("the agent was opened (pane {pane}) but could not be added: {e}. Add it with op `add` {{team, role, pane: {pane}}} once that is fixed.") } else { e }),
            }
        }
        "remove" => {
            let pane = args["pane"].as_i64().or_else(|| args["pane"].as_str().and_then(|x| resolve_pane(panes, &json!(x), you).map(|p| p.pane_id)).or_else(|| args["pane"].as_str().and_then(|x| x.parse().ok()))).ok_or("`pane` is the number of the member to take out")?;
            let r = call(json!({ "op": "unlink", "pane": pane })).await?;
            let mut out = format!("took pane {pane} out of the team `{}` (it was the {}); it is writable again", s(&r["team"]), s(&r["role"]));
            if args["close"] == json!(true) { if pane == you { return Err(format!("{out}. It is the pane you are in, so it was not closed: ask the person to close it.")); }
                if let Some(p) = panes.iter().find(|p| p.pane_id == pane) { match run_on_screen("close", &json!({ "id": p.id }), SCREEN_WAIT_MS).await { Ok(_) => out += " and closed", Err(e) => out += &format!("; it could not be closed: {e}") } } }
            Ok(out)
        }
        "edit" => {
            if args.get("name").is_some() { let team = pick_team(&s(&args["team"]))?; let r = call(json!({ "op": "rename", "team": team, "name": args["name"] })).await?; return Ok(format!("the team `{}` is now called {}", s(&r["team"]), s(&r["name"]))); }
            let role = role_of(&args["role"])?; let pane = args["pane"].as_i64().ok_or("`pane` is the member whose role changes")?;
            let from = load().into_iter().find(|c| c.members().iter().any(|(_, p)| *p == pane)).ok_or_else(|| format!("pane {pane} is in no team"))?;
            let was = from.members().into_iter().find(|(_, p)| *p == pane).map(|(r, _)| r.key().to_string()).unwrap_or_default();
            if was == role { return Ok(format!("pane {pane} is already the {role}")); }
            if let Some((_, other)) = from.members().into_iter().find(|(r, _)| r.key() == role) { return Err(format!("the {role} of `{}` is pane {other}: take it out first (op `remove`) or swap with `edit` on that pane", from.id)); }
            call(json!({ "op": "unlink", "pane": pane })).await?;
            if let Err(e) = call(json!({ "op": "link", "team": from.id, "role": role, "pane": pane })).await { let _ = call(json!({ "op": "link", "team": from.id, "role": was, "pane": pane })).await; return Err(e); }
            Ok(format!("pane {pane} is now the {role} of `{}` (it was the {was})", from.id))
        }
        "create" if ["planner", "worker", "qa"].iter().any(|k| args.get(*k).is_some()) => {
            let name = s(&args["name"]); call(json!({ "op": "create", "name": name })).await?; let mut done = vec![];
            for role in ["planner", "worker", "qa"] {
                if args.get(role).is_none() { continue; }
                match member(args[role].clone(), format!("{role} of {name}")).await { Ok((pane, opened)) => match call(json!({ "op": "link", "team": name, "role": role, "pane": pane })).await { Ok(_) => done.push(format!("{role}: pane {pane}{}", if opened { " (new)" } else { "" })), Err(e) => return Err(format!("made the team `{name}` ({}) but {role}: {e}", done.join(", "))) }, Err(e) => return Err(format!("made the team `{name}` ({}) but {role}: {e}", done.join(", "))) }
            }
            Ok(format!("the team `{name}` is made with {}. A team needs a planner and a worker (the QA is optional).", done.join(", ")))
        }
        // `list` also names the team DESIGNS (#29: it answered "0 teams" while two teams were on the screen)
        "" | "list" => { let said = call(args.clone()).await.map(|v| crate::team_run::render(&v))?; let r = root.clone(); let designs = tokio::task::spawn_blocking(move || crate::team_graph_run::list_graphs(&r).iter().map(|g| format!("- **{}** ({} nodes, budget ${:.2})", g.team, g.nodes.len(), g.budget_usd)).collect::<Vec<_>>()).await.unwrap_or_default(); Ok(with_designs(&said, &designs)) }
        _ => call(args.clone()).await.map(|v| crate::team_run::render(&v)),
    }
}

/// `POST /api/v1/cockpit/cowork/mcp`
pub async fn mcp(state: Arc<AppState>, reg: Arc<PtyRegistry>, body: Bytes) -> Response {
    let b = body_json(&body); let (tool, args) = (s(&b["tool"]), b["args"].clone()); let you = b["pane_id"].as_i64().unwrap_or(0);
    let (text, is_error) = match run_tool(&state, &reg, &tool, &args, you).await { Ok(t) => (t, false), Err(e) => (e, true) };
    reply(StatusCode::OK, json!({ "ok": !is_error, "text": text, "markdown": text, "error": if is_error { json!(text) } else { Value::Null } }))
}

async fn run_tool(state: &Arc<AppState>, reg: &Arc<PtyRegistry>, tool: &str, args: &Value, you: i64) -> Result<String, String> {
    let panes = panes_of(&layout_of(state).await);
    match tool {
        // the pack a provider reads once when the person switched it in mid-conversation (the tool marks it read)
        "antiloki_team_setup" => crate::team_onboarding::run_tool(state, you, args).await,
        "antiloki_reconcile" => crate::pane_provider::read_pack(you).ok_or_else(|| "there is no reconciliation pack for this pane: nobody switched you in from another provider. Carry on, and read `antiloki_sagas` for the project's history.".to_string()),
        "antiloki_panes" => { let l = layout_of(state).await; let tree = render_tree(&l); Ok(if panes.len() > 1 && !tree.is_empty() { format!("{}\n\nlayout:\n{tree}", render_panes(&panes, you)) } else { render_panes(&panes, you) }) }
        "antiloki_pane_view" => {
            let p = resolve_pane(&panes, &args["pane"], you).ok_or("no such pane: name one from antiloki_panes (its id, its number, or \"this\")")?;
            let r = run_on_screen("pane_view", &json!({ "id": p.id, "text_chars": args["text_chars"] }), SCREEN_WAIT_MS).await?;
            Ok(render_view(&p.id, &r["data"]))
        }
        "antiloki_pane_act" => {
            let p = resolve_pane(&panes, &args["pane"], you).ok_or("no such pane: name one from antiloki_panes (its id, its number, or \"this\")")?;
            let mut a = json!({ "id": p.id, "action": s(&args["action"]), "view": args["view"] });
            for k in ["ref", "testid", "text", "value", "key", "dy", "submit", "append"] { if !args[k].is_null() { a[k] = args[k].clone(); } }
            let r = run_on_screen("pane_act", &a, SCREEN_WAIT_MS).await?;
            let detail = s(&r["data"]["detail"]);
            Ok(if r["data"]["view"].is_null() { detail } else { format!("{detail}\n\n{}", render_view(&p.id, &json!({ "view": r["data"]["view"], "title": p.title, "tool": p.what }))) })
        }
        // THE MACHINE (2026-11-21): CPU, RAM, VRAM and the heaviest processes, read-only
        // ASK A SPECIALIST AND GET THE ANSWER (RFC 0024 C1): dormant or busy, it answers in a call of its own
        "antiloki_ask" => {
            let root = crate::agents_http::active_root(state).await;
            let from = crate::specialists::bound_of(you).map(|(_, id)| id).unwrap_or_else(|| format!("the agent in pane {you}"));
            crate::specialists::ask_one(&root, &from, &s(&args["to"]), &s(&args["question"])).await
        }
        // A GROUP OF AGENTS, proposed by the orchestrator and started only by the person (RFC 0025)
        "antiloki_group" => { let root = crate::agents_http::active_root(state).await; let work = crate::chat_pane::pane_row(state, you).await.and_then(|p| p["worktree_path"].as_str().map(String::from)).unwrap_or_default(); crate::groups::run_tool(you, &root, &work, args).await }
        // ROUTINES (RFC 0025 §7): an agent proposes one, OFF; only the person turns it on
        "antiloki_routine" => { let root = crate::agents_http::active_root(state).await; let a = args.clone(); tokio::task::spawn_blocking(move || crate::standing::run_tool(you, &root, &a)).await.map_err(|e| e.to_string())? }
        // THE AGENT'S OWN TASK LIST, shown to the person on its pane
        "antiloki_queue" => { let a = args.clone(); tokio::task::spawn_blocking(move || crate::task_queue::run_tool(you, &a)).await.map_err(|e| e.to_string())? }
        // MESSAGES BETWEEN AGENTS (RFC 0024 C3-C5): kept until the receiver is idle
        "antiloki_message" => {
            let root = crate::agents_http::active_root(state).await; let me = crate::specialists::bound_of(you).map(|(_, id)| id).unwrap_or_else(|| format!("pane-{you}")); let a = args.clone();
            tokio::task::spawn_blocking(move || crate::mailbox::run_tool(&root, &me, you, &a)).await.map_err(|e| e.to_string())?
        }
        // A REAL BROWSER, open and read (RFC 0024 B1, B2)
        "antiloki_browser" => {
            let root = { let w = crate::cowork::pane_facts(state, you).await.0; if w.is_empty() { crate::agents_http::active_root(state).await } else { w } };
            let a = args.clone(); let dir = std::path::Path::new(&root).join(".antiloki").join("browser");
            tokio::task::spawn_blocking(move || crate::agent_browser::run(&a, &dir, crate::cli::now_ms(), you)).await.map_err(|e| e.to_string())?
        }
        "antiloki_machine" => { let v = tokio::task::spawn_blocking(crate::machine::snapshot).await.map_err(|e| e.to_string())?; Ok(render_machine(&v)) }
        // WHAT THE AIs USED AND WHAT IT WOULD COST AT OPENROUTER (2026-11-21): the usage window's numbers, read-only
        "antiloki_cost" => {
            let days = args["days"].as_i64().filter(|d| [0, 7, 30].contains(d)).unwrap_or(7); let tz = args["tz"].as_i64().map(|t| format!("&tz={}", t.clamp(-840, 840))).unwrap_or_default();   // none named: the serve answers in the screen's zone (#26)
            let url = format!("http://127.0.0.1:{}/api/v1/cockpit/fuel?days={days}{tz}", std::env::var("ANTILOKI_SERVE_PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(41998));
            let v: Value = tokio::task::spawn_blocking(move || ureq::get(&url).timeout(std::time::Duration::from_secs(60)).call().ok().and_then(|r| r.into_json::<Value>().ok()).unwrap_or(Value::Null)).await.map_err(|e| e.to_string())?;
            let v = if v["result"].is_object() { v["result"].clone() } else { v }; if !v["models"].is_array() { return Err(cost_unread(&v).into()); } Ok(render_cost(&v, days))
        }
        // THE TEAM TEMPLATES (2026-11-21): the shipped and the person's own; an agent may list them and start a team from one (a team change: it reads the rubric first); sharing a team as a template is the person's
        "antiloki_team_templates" => {
            let op = { let o = s(&args["op"]); if o.is_empty() { "list".to_string() } else { o } }; if op != "list" && op != "use" { return Err("op is `list` or `use`".into()); }
            let mut body = json!({ "op": if op == "list" { "template_list" } else { "template_use" } }); if op == "use" { for k in ["template", "team", "budget_usd"] { body[k] = args[k].clone(); } }
            let url = format!("http://127.0.0.1:{}/api/v1/cockpit/team-graph", std::env::var("ANTILOKI_SERVE_PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(41998)); let body2 = body.clone();
            let v: Value = tokio::task::spawn_blocking(move || ureq::post(&url).timeout(std::time::Duration::from_secs(60)).send_json(body2).ok().and_then(|r| r.into_json::<Value>().ok()).unwrap_or(Value::Null)).await.map_err(|e| e.to_string())?;
            let v = if v["result"].is_object() { v["result"].clone() } else { v }; if v["ok"] != json!(true) { return Err(if s(&v["error"]).is_empty() { "the templates could not be read".into() } else { s(&v["error"]) }); }
            if op == "list" { Ok(v["templates"].as_array().into_iter().flatten().map(|t| format!("- {} [{}] ({}): {}{}", s(&t["name"]), s(&t["status"]), s(&t["category"]), s(&t["description"]).chars().take(200).collect::<String>(), if s(&t["ask"]).is_empty() { String::new() } else { format!(" · ask: {}", s(&t["ask"])) })).collect::<Vec<_>>().join("\n")) }
            else { Ok(format!("team `{}` made from `{}`. Start it with antiloki_team_run, or open it with the person. {}", s(&v["team"]), s(&args["template"]), s(&v["hint"]))) }
        }
        // A SPECIALIST'S MEMORY (RFC 0017 §2.4): recall what it knows, write down what it learned, draft a specialist. Approving one is the person's
        "antiloki_remember" => {
            let op = { let o = s(&args["op"]); if o.is_empty() { "recall".to_string() } else { o } }; let tg = match op.as_str() { "list" => "specialist_list", "recall" => "specialist_notes", "note" => "specialist_note", "propose" => "specialist_save", "brief" => "specialist_brief", "verify" => "specialist_verify", "consolidate" => "specialist_consolidate", "sql" => "specialist_sql", "ask" => "specialist_ask", "contract" => "specialist_contract_open", "contracts" => "specialist_contracts", "lint" => "specialist_contract_lint", "deliver" => "specialist_contract_deliver", "tool" => "specialist_tool", "check" => "specialist_contract_check", "cancel" => "specialist_contract_cancel", "suggest" => "specialist_suggest", "whoami" => return Ok(crate::specialists::spec_of_pane(you).map(|(id, tools, leader)| json!({ "id": id, "tools": tools, "leader": leader }).to_string()).unwrap_or_default()), _ => return Err("op is list, recall, note, propose, verify, consolidate, suggest, sql, ask or brief".into()) };
            let mut body = json!({ "op": tg, "pane_id": 1 }); for k in ["id", "kind", "text", "reason", "query", "limit", "work", "specialist", "sources", "n", "to", "seat", "name", "why", "agent", "sql", "assistant", "question", "to", "task", "deliverables", "checks", "contract", "open", "note", "name", "input"] { if !args[k].is_null() { body[k] = args[k].clone(); } }
            // an agent that is not a specialist makes and checks contracts under its pane's name, whatever id it sends
            if crate::specialists::bound_of(you).is_none() && matches!(op.as_str(), "contract" | "contracts" | "check" | "cancel") { body["id"] = json!(format!("pane-{you}")); }
            let url = format!("http://127.0.0.1:{}/api/v1/cockpit/team-graph", std::env::var("ANTILOKI_SERVE_PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(41998));
            let v: Value = tokio::task::spawn_blocking(move || ureq::post(&url).timeout(std::time::Duration::from_secs(60)).send_json(body).ok().and_then(|r| r.into_json::<Value>().ok()).unwrap_or(Value::Null)).await.map_err(|e| e.to_string())?;
            // only an ENVELOPE is opened (its result says ok itself): `sql` answers {ok, result: {columns, rows}}, and opening that lost the ok and called a good answer "could not be reached" (2026-11-26)
            let v = if v["result"]["ok"].is_boolean() { v["result"].clone() } else { v }; if v["ok"] != json!(true) { return Err(if s(&v["error"]).is_empty() { "the specialist could not be reached".into() } else { s(&v["error"]) }); }
            // A CONTRACT'S TASK GOES TO THE ASSISTANT: its open pane if it has one, else its pane is opened with the task
            let mut delivered = String::new();
            if op == "contract" {
                let c = &v["contract"]; let (cid, to, from, task) = (s(&c["id"]), s(&c["to"]), s(&c["from"]), s(&c["task"])); let checks: Vec<String> = c["checks"].as_array().into_iter().flatten().map(|x| match s(&x["kind"]).as_str() { "file_exists" => format!("{} exists", s(&x["path"])), "contains" => format!("{} contains \"{}\"", s(&x["path"]), s(&x["text"])), "not_contains" => format!("{} has no \"{}\"", s(&x["path"]), s(&x["text"])), k => k.to_string() }).collect();
                let text = format!("CONTRACT {cid} from {from}: {task}\nAt the end {from} checks: {}. When you are done, run antiloki_remember op `deliver` {{contract: \"{cid}\", note}} (it tells {from}, who then checks the contract). This help is for this task only.", checks.join("; "));
                let root = crate::agents_http::active_root(state).await; let (r2, to2) = (root.clone(), to.clone()); let open_panes = tokio::task::spawn_blocking(move || crate::specialists::panes_of(&r2, &to2)).await.unwrap_or_default();
                if let Some(pid) = running_one(&open_panes, |p| terminal_runs(reg, p)) { let (r3, from2, to3, text2) = (root.clone(), from.clone(), to.clone(), text.clone()); let sent = tokio::task::spawn_blocking(move || crate::mailbox::send_now(&r3, &from2, you, &to3, &text2)).await.map_err(|e| e.to_string())?; delivered = match sent { Ok(n) => format!(" The task waits for {to}'s open pane ({pid}) as message #{n} and is typed there when it is idle."), Err(e) => format!(" The task could not be sent to {to}: {e}") };
                } else { match std::boxed::Box::pin(run_tool(state, reg, "antiloki_pane_open", &json!({ "specialist": to, "prompt": text }), you)).await { Ok(t) => delivered = format!(" {to} had no open pane: {t}"), Err(e) => delivered = format!(" {to} has no open pane and it could not be opened ({e}): ask the person to open it.") } }
            }
            // A NEW SPECIALIST OPENS ON THE PERSON'S SCREEN (it is approved at once since 2026-11-25; the words said "draft" until 2026-11-27)
            // Operator 2026-11-25: "after a specialist is created it should open it by default". No screen open: the answer says so.
            if op == "propose" {
                let sid = s(&v["specialist"]["id"]); let sid = if sid.is_empty() { s(&args["specialist"]["id"]) } else { sid };
                if !sid.is_empty() { let shown = run_on_screen("specialist_edit", &json!({ "id": sid }), 8_000).await.is_ok(); return Ok(if shown { format!("saved `{sid}`: it is approved and ready to use, and its window is open on the person's screen to read or change. Do not open it again.") } else { format!("saved `{sid}`: it is approved and ready to use. No antiloki screen answered; the person finds it in the + menu.") }); }
            }
            if op == "deliver" {
                let c = &v["contract"]; let (cid, from, to, note) = (s(&c["id"]), s(&c["from"]), s(&c["to"]), s(&c["delivered"]["note"])); let root = crate::agents_http::active_root(state).await;
                let text = format!("CONTRACT {cid}: {to} says it is delivered: {note}\nCheck it: antiloki_remember op `check` {{contract: \"{cid}\"}}.");
                if let Err(e) = tokio::task::spawn_blocking(move || crate::mailbox::send_now(&root, &to, you, &from, &text)).await.map_err(|e| e.to_string())? { eprintln!("antiloki-serve: the caller of contract {cid} could not be told: {e}"); }
            }
            Ok(match op.as_str() {
                "list" => v["specialists"].as_array().into_iter().flatten().map(|x| format!("- {} ({}; {}; {} note(s)): {}", s(&x["specialist"]["id"]), s(&x["specialist"]["name"]), if x["usable"] == json!(true) { "approved" } else { "draft" }, x["notes"], s(&x["specialist"]["purpose"]))).collect::<Vec<_>>().join("\n"),
                "recall" => v["notes"].as_array().into_iter().flatten().map(|n| format!("#{} [{}] {}{}", n["n"], s(&n["kind"]), s(&n["text"]), if s(&n["reason"]).is_empty() { String::new() } else { format!(" (why: {})", s(&n["reason"])) })).collect::<Vec<_>>().join("\n"),
                "note" => format!("remembered as note #{}", v["note"]["n"]),
                "sql" => format!("{}\n{}", v["result"]["columns"].as_array().map(|a| a.iter().map(|c| s(c)).collect::<Vec<_>>().join(" | ")).unwrap_or_default(), v["result"]["rows"].as_array().into_iter().flatten().map(|r| r.as_array().map(|a| a.iter().map(|c| c.as_str().map(String::from).unwrap_or_else(|| c.to_string())).collect::<Vec<_>>().join(" | ")).unwrap_or_default()).collect::<Vec<_>>().join("\n")),
                "ask" => format!("{} ({}): {}\n{}", s(&v["answer"]["name"]), s(&v["answer"]["purpose"]), if v["answer"]["notes"].as_array().map_or(true, |a| a.is_empty()) { "it holds nothing about that.".to_string() } else { "what it remembers:".to_string() }, v["answer"]["notes"].as_array().into_iter().flatten().map(|n| format!("- {}", s(&n["text"]))).collect::<Vec<_>>().join("\n")),
                "contract" => format!("contract {} opened with {}: {} check(s). At the end run op `check` {{contract: \"{}\"}}.{delivered}", s(&v["contract"]["id"]), s(&v["contract"]["to"]), v["contract"]["checks"].as_array().map_or(0, |a| a.len()), s(&v["contract"]["id"])),
                "lint" => if v["valid"] == json!(true) { "the contract lints: it can be opened.".to_string() } else { v["issues"].as_array().into_iter().flatten().map(|i| format!("{}{}", if s(&i["path"]).is_empty() { String::new() } else { format!("{}: ", s(&i["path"])) }, s(&i["message"]))).collect::<Vec<_>>().join("\n") },
                "tool" => serde_json::to_string(&v["result"]).unwrap_or_default().chars().take(6000).collect::<String>(),
                "deliver" => format!("delivered: {} is told and will check contract {}.", s(&v["contract"]["from"]), s(&v["contract"]["id"])),
                "contracts" => v["contracts"].as_array().into_iter().flatten().map(|c| format!("{} ({} → {}) {}: {}", s(&c["id"]), s(&c["from"]), s(&c["to"]), s(&c["status"]), s(&c["task"]))).collect::<Vec<_>>().join("\n"),
                "check" | "cancel" => { let c = &v["contract"]; let rows: Vec<String> = c["last"]["rows"].as_array().into_iter().flatten().map(|r| format!("{} {}", if r["pass"] == json!(true) { "✓" } else { "✗" }, s(&r["why"]))).collect(); format!("contract {}: {}\n{}", s(&c["id"]), s(&c["status"]).to_uppercase(), rows.join("\n")) }
                "brief" => s(&v["brief"]),
                "consolidate" => format!("consolidated raw chunks {}-{} ({} tokens). A new seat starts from this text.", v["consolidation"]["from"], v["consolidation"]["to"], v["consolidation"]["tokens"]),
                "suggest" => format!("suggested `{}` to the person, once. Do not suggest again.", s(&v["suggestion"]["name"])),
                "verify" => format!("note #{}: {}. {} chunk(s), {} tokens in / {} out, ${:.4}. {}", v["verified"]["n"], if v["verified"]["grounded"] == json!(true) { "GROUNDED in the raw conversation" } else { "NOT GROUNDED" }, v["verified"]["chunks"].as_array().map_or(0, |a| a.len()), v["verified"]["tokens_in"], v["verified"]["tokens_out"], v["verified"]["cost_usd"].as_f64().unwrap_or(0.0), s(&v["verified"]["ungrounded_reason"])),
                _ => format!("saved `{}`: it is approved and ready to use; the person can read or change it in the app.", s(&v["specialist"]["id"])),
            })
        }
        // THE BOOK (RFC 0018): the laws of any kind of work. An agent may read, run, test and PROPOSE; ratifying is the person's, in the app
        "antiloki_book" => {
            let op = { let o = s(&args["op"]); if o.is_empty() { "list".to_string() } else { o } }; let tg = match op.as_str() { "list" => "book_list", "show" => "book_show", "run" => "book_run", "test" => "book_test", "propose" => "book_save", _ => return Err("op is list, show, run, test or propose (ratifying is the person's)".into()) };
            let mut body = json!({ "op": tg, "pane_id": 1 }); for k in ["pack", "law", "key", "run", "dir", "packs", "paths", "fail_on", "candidates", "limit"] { if !args[k].is_null() { body[k] = args[k].clone(); } }
            let url = format!("http://127.0.0.1:{}/api/v1/cockpit/team-graph", std::env::var("ANTILOKI_SERVE_PORT").ok().and_then(|p| p.parse::<u16>().ok()).unwrap_or(41998));
            let v: Value = tokio::task::spawn_blocking(move || ureq::post(&url).timeout(std::time::Duration::from_secs(120)).send_json(body).ok().and_then(|r| r.into_json::<Value>().ok()).unwrap_or(Value::Null)).await.map_err(|e| e.to_string())?;
            let v = if v["result"].is_object() { v["result"].clone() } else { v }; if v["ok"] != json!(true) { return Err(if s(&v["error"]).is_empty() { "the book could not be read".into() } else { s(&v["error"]) }); }
            Ok(match op.as_str() {
                "list" => v["packs"].as_array().into_iter().flatten().map(|p| format!("- {} ({}; kinds {}; {}): {}\n{}", s(&p["id"]), s(&p["origin"]), p["kinds"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("/")).unwrap_or_default(), p["laws"].as_array().map_or(0, |l| l.len()), s(&p["description"]).chars().take(140).collect::<String>(), p["laws"].as_array().into_iter().flatten().map(|l| format!("    · {} [{} {} {}] {}", s(&l["key"]), s(&l["status"]), s(&l["severity"]), s(&l["detector"]), s(&l["title"]))).collect::<Vec<_>>().join("\n"))).collect::<Vec<_>>().join("\n"),
                "run" => format!("{}\nfiles judged: {}; laws run: {}; gate: {}\n{}", crate::lanes::render_findings(&v, 40), v["artifacts"].as_array().map_or(0, |a| a.len()), v["laws_run"], if v["gate"]["skipped"] == json!(true) { "skipped (no law applies)".to_string() } else if v["gate"]["passed"] == json!(true) { "passed".to_string() } else { format!("failed ({} finding(s) at or above {})", v["gate"]["failing"], s(&v["gate"]["fail_on"])) }, if v["unread"].as_array().map_or(false, |u| !u.is_empty()) { format!("unread: {}", v["unread"].as_array().unwrap().iter().take(8).map(|u| format!("{} ({})", s(&u["path"]), s(&u["why"]))).collect::<Vec<_>>().join("; ")) } else { String::new() }),
                "test" => format!("law {}: {} · compiles: {} · examples: {}{}", s(&v["key"]), if v["passed"] == json!(true) { "ALL EXAMPLES AGREE" } else { "NOT READY" }, v["compiles"], v["examples"].as_array().map(|a| a.iter().map(|e| format!("{} {}", s(&e["name"]), if e["passed"] == json!(true) { "ok".to_string() } else { format!("FAILED ({} hits, wanted {}){}", e["hits"], s(&e["expect"]), if s(&e["error"]).is_empty() { String::new() } else { format!(" {}", s(&e["error"])) }) })).collect::<Vec<_>>().join("; ")).unwrap_or_default(), if s(&v["why"]).is_empty() { String::new() } else { format!(" · {}", s(&v["why"])) }),
                "propose" => format!("saved pack `{}` as candidates in {}. They advise but never gate. Run `test` on each law, `run` with candidates:true to show the person the hit-rate, then ask them to ratify in the app (Book).", s(&v["pack"]["id"]), s(&v["path"])),
                _ => serde_json::to_string(&v).unwrap_or_default().chars().take(6000).collect(),
            })
        }
        // WHAT AN INSTALLED APP CAN DO, for an agent that designs a pane for it: every tool with its risk class and its argument names (never a token, never a call)
        "antiloki_app_tools" => {
            let app = s(&args["app"]); if app.is_empty() { return Err("say which `app` (an installed one)".into()); }
            let v = crate::webapps::call_for_pane(&json!({ "op": "tools", "app": app })).await;
            if v["ok"] != json!(true) { return Err(s(&v["error"]).chars().take(300).collect::<String>()); }
            let rows: Vec<String> = v["tools"].as_array().into_iter().flatten().map(|t| {
                let props: Vec<String> = t["input_schema"]["properties"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
                format!("- {} [{}] ({}) — {}", s(&t["name"]), s(&t["risk"]), props.join(", "), s(&t["description"]).chars().take(160).collect::<String>()) }).collect();
            Ok(format!("{app}: {} tools (read tools go in a pane's `read` list, the rest in `write`)\n{}", rows.len(), rows.join("\n")))
        }
        // FREE PANES, FOR AN AGENT (RFC 0016 M3): a draft is written, never approved; the person looks at the card and approves
        "antiloki_free_pane_list" => { let root = crate::agents_http::active_root(state).await; let v = tokio::task::spawn_blocking(move || crate::panes::list(&root)).await.map_err(|e| e.to_string())?;
            let rows: Vec<String> = v.as_array().into_iter().flatten().map(|p| format!("- {} ({}) v{} — {}{}", s(&p["id"]), s(&p["title"]), s(&p["version"]), if p["approved"] == json!(true) { "approved" } else { "DRAFT" }, if p["problems"].as_array().map_or(false, |x| !x.is_empty()) { format!(" — problems: {}", p["problems"].as_array().unwrap().iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("; ")) } else { String::new() })).collect();
            Ok(if rows.is_empty() { "no free panes in this project yet (write one with antiloki_free_pane_write)".into() } else { rows.join("\n") }) }
        "antiloki_free_pane_write" => {
            let root = crate::agents_http::active_root(state).await; let (id, m, f) = (s(&args["id"]), args["manifest"].clone(), args["files"].clone());
            let r = tokio::task::spawn_blocking(move || crate::panes::save_draft(&root, &id, &m, &f, "agent")).await.map_err(|e| e.to_string())??;
            Ok(format!("draft written: {} (hash {}). It is a DRAFT: it can reach no app until the person approves it from its card (you cannot approve). Open it for them with antiloki_free_pane_open.{}", s(&r["id"]), s(&r["hash"]), if r["kept_old"].is_null() { String::new() } else { " The older version was kept under .antiloki/panes/.archive/.".to_string() }))
        }
        "antiloki_free_pane_open" => {
            let root = crate::agents_http::active_root(state).await; let id = s(&args["id"]); let (r2, i2) = (root.clone(), id.clone());
            let f = tokio::task::spawn_blocking(move || crate::panes::read(&r2, &i2)).await.map_err(|e| e.to_string())??;
            let title = if f["manifest"]["title"].is_string() { s(&f["manifest"]["title"]) } else { id.clone() };
            let r = run_on_screen("open", &json!({ "kind": "studio", "tool": "free-pane", "free": id, "title": title, "side": s(&args["side"]) }), SCREEN_WAIT_MS).await?;
            Ok(format!("opened {}{}. {} Read what it shows with antiloki_pane_view; drive it with antiloki_pane_act (what a pane lets you do is what its page declared).", s(&r["pane"]), if f["approved"] == json!(true) { "" } else { " (a draft: the person approves it from the card first)" }, if f["approved"] == json!(true) { "" } else { "It cannot reach any app yet." }))
        }
        "antiloki_themes" => {
            if args.get("op").and_then(Value::as_str).is_some() { let a = args.clone(); return tokio::task::spawn_blocking(move || crate::custom_themes::tool(&a)).await.map_err(|e| e.to_string())?; }
            let v = tokio::task::spawn_blocking(crate::themes::overview).await.map_err(|e| e.to_string())?; let mut out = render_themes(&v); let custom = tokio::task::spawn_blocking(crate::custom_themes::list).await.unwrap_or_default(); for t in custom { out += &format!("\n{} ({}, custom, look {})", t["id"].as_str().unwrap_or(""), t["name"].as_str().unwrap_or(""), t["look"].as_str().unwrap_or("")); } out += THEME_HINT; Ok(out)
        }
        "antiloki_pane_arrange" => {
            let p = resolve_pane(&panes, &args["pane"], you).ok_or("no such pane: name one from antiloki_panes (its id, its number, or \"this\")")?;
            let action = s(&args["action"]); if !["move", "swap", "flip", "zoom", "focus", "rename", "resize"].contains(&action.as_str()) { return Err("action is one of: move, swap, flip, zoom, focus, rename, resize".into()); }
            let other = |k: &str| -> Result<String, String> { if args[k].is_null() { return Ok(String::new()); } resolve_pane(&panes, &args[k], you).map(|x| x.id.clone()).ok_or_else(|| format!("no such pane for `{k}`: name one from antiloki_panes")) };
            let a = json!({ "action": action, "id": p.id, "dir": s(&args["dir"]), "to": other("to")?, "side": s(&args["side"]), "with": other("with")?, "title": s(&args["title"]), "share": unit(&args["share"]), "on": args["on"] });
            run_on_screen("arrange", &a, SCREEN_WAIT_MS).await?;
            let l = layout_of(state).await; let tree = render_tree(&l);
            Ok(format!("{action} done on {}.{}", p.id, if tree.is_empty() { String::new() } else { format!("\n\nlayout now:\n{tree}") }))
        }
        "antiloki_look" => {
            let mut patch = serde_json::Map::new();
            for (k, to) in [("opacity_terminals", "termOpacity"), ("opacity_panes", "paneOpacity"), ("dim", "dim")] { if !args[k].is_null() { patch.insert(to.into(), json!(unit(&args[k]).ok_or_else(|| format!("{k} is a number from 0 to 1 (or 0 to 100)"))?)); } }
            if let Some(t) = args["theme"].as_str() { let t = t.trim(); patch.insert("themeId".into(), if t.is_empty() || ["default", "antiloki", "none"].contains(&t.to_lowercase().as_str()) { Value::Null } else { json!(t) }); }
            if let Some(w) = args["wallpaper"].as_str() { let w = w.trim(); patch.insert("wallpaper".into(), if w.eq_ignore_ascii_case("none") { json!("") } else if w.is_empty() || w.eq_ignore_ascii_case("first") || w.eq_ignore_ascii_case("default") { Value::Null } else { json!(w) }); }
            if let Some(f) = args["follow_desktop"].as_bool() { patch.insert("follow".into(), json!(f)); }
            let r = run_on_screen("look", &json!({ "patch": patch }), SCREEN_WAIT_MS).await?;
            Ok(format!("{}{}", if patch.is_empty() { "" } else { "done.\n" }, render_look(&r["data"])))
        }
        "antiloki_screen" => {
            let sidebar = match s(&args["sidebar"]).as_str() { "show" | "shown" | "open" => "show", "hide" | "hidden" | "collapse" | "close" => "hide", "" => "", o => return Err(format!("sidebar is show or hide, not `{o}`")) };
            let r = run_on_screen("screen", &json!({ "sidebar": sidebar, "workspace": s(&args["workspace"]) }), SCREEN_WAIT_MS).await?;
            Ok(render_workspace(&r["data"]))
        }
        "antiloki_qa" => qa(state, args, you).await,
        "antiloki_worktrees" if s(&args["op"]) == "suggest" => { let root = crate::agents_http::active_root(state).await; let reason = s(&args["reason"]); tokio::task::spawn_blocking(move || crate::thread_worktree::suggest(&root, you, &reason)).await.unwrap_or_else(|e| Err(e.to_string())) }
        "antiloki_worktrees" => {
            let st = state.clone(); let boxes = tokio::task::spawn_blocking(move || worktrees(&st)).await.map_err(|e| e.to_string())?;
            Ok(render_worktrees(&boxes, you))
        }
        "antiloki_pane_read" => {
            let p = resolve_pane(&panes, &args["pane"], you).ok_or("no such pane: name one from antiloki_panes (its id, its number, or \"this\")")?;
            let rows = tokio::task::spawn_blocking({ let n = p.pane_id; move || crate::pty_remote::screen_rows(n) }).await.ok().flatten()
                .ok_or("this terminal's screen cannot be read here (it is not held by the terminal host, or it has ended)")?;
            Ok(render_screen(&rows, args["lines"].as_u64().map(|n| n as usize).unwrap_or(READ_ROWS)))
        }
        "antiloki_pane_send" => {
            let p = resolve_pane(&panes, &args["pane"], you).ok_or("no such pane: name one from antiloki_panes")?;
            if !matches!(p.kind.as_str(), "shell" | "tool") { return Err(format!("{} is another agent's terminal: type to a terminal you opened, not into an agent's conversation", p.id)); }
            let t = s(&args["text"]); if t.is_empty() { return Err("nothing to send".into()); }
            send_to(reg, p.pane_id, &t).await?; Ok(format!("sent to {}", p.id))
        }
        "antiloki_pane_close" => {
            let p = resolve_pane(&panes, &args["pane"], you).ok_or("no such pane: name one from antiloki_panes")?;
            if p.pane_id == you && you > 0 { return Err("that is the pane you are in: ask the person to close it".into()); }
            let id = p.id.clone();
            run_on_screen("close", &json!({ "id": id }), SCREEN_WAIT_MS).await?; Ok(format!("closed {id}"))
        }
        "antiloki_threads" if matches!(s(&args["op"]).as_str(), "integrate" | "land" | "discard") => {
            let v = crate::thread_tree::tree_run(state, &s(&args["op"]), &s(&args["thread"])).await;
            if v["ok"] == json!(true) { Ok(match s(&args["op"]).as_str() { "integrate" => format!("brought together: {} agent(s) merged into the thread's branch, head {}", v["merged"].as_array().map_or(0, |a| a.len()), s(&v["head"]).chars().take(8).collect::<String>()), "land" => format!("landed in the workspace, head {}", s(&v["head"]).chars().take(8).collect::<String>()), _ => "discarded".to_string() }) }
            else { Err(if v["conflicts"].as_array().map_or(false, |c| !c.is_empty()) { format!("conflicts: {}", v["conflicts"].as_array().unwrap().iter().map(|c| format!("{} ({})", s(&c["agent"]), c["files"].as_array().map(|f| f.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default())).collect::<Vec<_>>().join("; ")) } else { let e = s(&v["error"]); if e.is_empty() { v["errors"].to_string() } else { e } }) }
        }
        "antiloki_threads" => {
            let root = crate::agents_http::active_root(state).await; let a = args.clone();
            let r = tokio::task::spawn_blocking(move || crate::cowork_threads::apply(&root, &a)).await.unwrap_or_else(|e| Err(e.to_string()));
            r.map(|v| { let mut o = format!("threads (open: {}):", v["active"].as_str().unwrap_or("main")); for t in v["threads"].as_array().into_iter().flatten() { o += &format!("\n- {}{} ({}): {} pane{}", t["name"].as_str().unwrap_or(""), if t["active"] == json!(true) { " ← open" } else { "" }, t["id"].as_str().unwrap_or(""), t["panes"].as_array().map_or(0, |p| p.len()), if t["panes"].as_array().map_or(0, |p| p.len()) == 1 { "" } else { "s" }); } o })
        }
        "antiloki_teams" | "antiloki_crews" => match teams_tool(state, reg, &crate::team_run::in_new_words(args.clone()), you, &panes).await { Ok(t) => Ok(t), Err(e) => Err(e) },
        t if crate::cowork_xray::TOOLS.contains(&t) && t != "antiloki_xray_open" => crate::cowork_xray::run(state, t, args).await,
        t if crate::xray_widgets::TOOLS.contains(&t) => crate::xray_widgets::run_tool(state, t, args).await,
        // A SPECIALIST OPENS A SPECIALIST (operator 2026-11-25: "I want my specialist to open it"): a draft's editor comes up on the person's screen; an approved one opens as its pane
        "antiloki_specialist_open" => {
            let id = s(&args["id"]); if id.is_empty() { return Err("say which specialist: its id".into()); }
            let root = crate::agents_http::active_root(state).await; let (r2, i2) = (root.clone(), id.clone());
            let def = tokio::task::spawn_blocking(move || crate::specialists::get(&r2, &i2)).await.map_err(|e| e.to_string())?;
            let def = def.ok_or_else(|| format!("there is no specialist `{id}`"))?;
            // an approved specialist with NO seat cannot run as a pane (no engine to open it with): its editor shows it, where the person sets a seat
            if crate::specialists::usable(&def) && !def.seat.trim().is_empty() {
                run_on_screen("open", &json!({ "kind": "agent", "specialist": id }), 8_000).await.map_err(|e| format!("could not open it: {e}"))?;
                Ok(format!("opened `{id}` on the person's screen."))
            } else {
                run_on_screen("specialist_edit", &json!({ "id": id }), 8_000).await.map_err(|e| format!("could not show it: {e}"))?;
                Ok(format!("`{id}` is a draft: its editor is open on the person's screen, to read and approve."))
            }
        }
        t if crate::providers::TOOLS.contains(&t) => { let a = args.clone(); tokio::task::spawn_blocking(move || crate::providers::run_tool(you, &a)).await.map_err(|e| e.to_string())? }
        t if crate::connectors::TOOLS.contains(&t) => crate::connectors::run_tool(state, you, t, args).await,
        // A PANE SHOWING EXACTLY ONE PART OF X-RAY (the system diagram, a file's review, the rules, ...), for a space
        "antiloki_xray_open" => {
            let c = crate::cowork_xray::open_command(args)?;
            let space = s(&c["space"]);
            if !space.is_empty() { crate::cowork_xray::pick_space(&crate::cowork_xray::registry(), &space)?; }
            let r = run_on_screen("open", &c, SCREEN_WAIT_MS).await?;
            let (id, pane_id) = (s(&r["pane"]), r["pane_id"].as_i64().unwrap_or(0));
            Ok(format!("opened {id}{} showing {}{}{}. Read what it shows with antiloki_pane_view, drive it with antiloki_pane_act, close it with antiloki_pane_close.", if pane_id > 0 { format!(" (pane {pane_id})") } else { String::new() }, s(&c["view"]), if s(&c["path"]).is_empty() { String::new() } else { format!(" of {}", s(&c["path"])) }, if space.is_empty() { String::new() } else { format!(" in {space}") }))
        }
        "antiloki_pane_open" => {
            // A SPECIALIST'S PANE (the operator 2026-11-21): the same terminal and CLI, with the specialist's context and access behind it. Its brief is the first message; the pane is bound so its micro MCP shows only the specialist's tools
            let spec_id = s(&args["specialist"]); let mut args = args.clone(); let mut bind: Option<(String, String)> = None;
            if !spec_id.is_empty() {
                let root = crate::agents_http::active_root(state).await; let (r2, i2, work) = (root.clone(), spec_id.clone(), s(&args["prompt"]));
                let (def, brief) = tokio::task::spawn_blocking(move || (crate::specialists::get(&r2, &i2), crate::specialists::brief(&r2, &i2, &work))).await.map_err(|e| e.to_string())?;
                let def = def.ok_or_else(|| format!("no specialist `{spec_id}`"))?;
                // A DRAFT CANNOT WORK YET, BUT IT CAN BE OPENED FOR THE PERSON (2026-11-22: "open this specialist" failed on a draft the agent had just made): its editor opens on the screen, where the person reads it and approves it
                if !crate::specialists::usable(&def) { run_on_screen("specialist_edit", &json!({ "id": spec_id }), SCREEN_WAIT_MS).await?; return Ok(format!("`{}` is a draft: its editor is open on the person's screen. They read it and approve it there; after that it can be opened as a pane (antiloki_pane_open with `specialist`).", def.name)); }
                let brief = brief?; args["shape"] = json!(def.shape); args["color"] = json!(def.color); args["model"] = json!(def.model);
                args["kind"] = json!("agent"); if s(&args["engine"]).is_empty() { let e = crate::cli::agent_seat(&def.seat, asking(&panes, you)); args["engine"] = json!(e.key()); if crate::cli::seat_engine(&def.seat).is_some_and(|d| d != e) { args["model"] = json!(""); } } if s(&args["title"]).is_empty() { args["title"] = json!(def.name.clone()); }
                let work = s(&args["prompt"]); args["prompt"] = json!(if work.is_empty() { brief } else { format!("{brief}\n\nYOUR TASK NOW:\n{work}") }); bind = Some((root, spec_id.clone()));
            }
            let args = &args;
            let kind = { let k = s(&args["kind"]); if k.is_empty() { "terminal".to_string() } else { k } };
            if !matches!(kind.as_str(), "terminal" | "agent" | "studio") { return Err("kind is one of: terminal, agent, studio".into()); }
            let mut a = json!({ "kind": kind, "title": s(&args["title"]), "side": s(&args["side"]), "engine": s(&args["engine"]), "tool": s(&args["tool"]), "specialist": spec_id, "shape": s(&args["shape"]), "color": s(&args["color"]), "model": s(&args["model"]) });
            if kind == "studio" && s(&args["tool"]).is_empty() { return Err("studio needs `tool`: issues, notes, preview, shared-diff, diff, brainstorm, design-canvas, frames, design-system, code-graph, system-design, xray, authors, constitution, code, git-graph, skill-library or skillshare".into()); }
            if kind == "agent" && s(&args["engine"]).is_empty() { a["engine"] = json!(crate::cli::agent_seat("", asking(&panes, you)).key()); }
            let r = run_on_screen("open", &a, SCREEN_WAIT_MS).await?;
            let (id, pane_id) = (s(&r["pane"]), r["pane_id"].as_i64().unwrap_or(0)); if let (Some((root, sid)), true) = (&bind, pane_id > 0) { crate::specialists::bind_pane(pane_id, root, sid); }
            // what to put in the new terminal once it is up: the command for a terminal, the first message for an agent
            let first = if kind == "terminal" { s(&args["command"]) } else { s(&args["prompt"]) };
            let note = if !first.is_empty() && pane_id > 0 { tokio::spawn(send_when_up(reg.clone(), pane_id, first.clone())); if kind == "terminal" { format!(" and running `{}`", first.chars().take(80).collect::<String>()) } else { " and giving it your message".into() } } else { String::new() };
            Ok(format!("opened {id}{}{note}. Read it with antiloki_pane_read, close it with antiloki_pane_close.", if pane_id > 0 { format!(" (pane {pane_id})") } else { String::new() }))
        }
        _ => Err(format!("unknown pane tool: {tool}")),
    }
}

/// Why there is no cost to show. A first count reads every session file and can outlast the wait: that is not a failure (#26). Pure.
fn cost_unread(v: &Value) -> &'static str { if v["pending"] == json!(true) { "the usage is still being counted (it reads every session file on this computer): ask again in a minute" } else { "the usage record could not be read" } }

#[cfg(test)]
mod tests {
    #[test] fn a_count_still_running_is_not_called_a_failure() { assert!(super::cost_unread(&serde_json::json!({ "ok": false, "pending": true })).contains("ask again")); assert!(super::cost_unread(&serde_json::Value::Null).contains("could not be read")); }
    #[test]
    fn a_first_message_waits_for_the_person_to_answer_the_agents_question() {
        use std::time::Duration as D; let asking = "the agent is asking its own question: answer it in its terminal first";
        assert!(super::keep_trying(asking, D::from_secs(600)) && !super::keep_trying(asking, D::from_secs(901)));
        assert!(super::keep_trying("no live terminal", D::from_secs(5)) && !super::keep_trying("no live terminal", D::from_secs(21)));
    }
    #[test]
    fn a_contract_goes_to_a_running_pane_never_to_a_closed_one() {
        assert_eq!(super::running_one(&[9, 7, 3], |p| p == 3 || p == 7), Some(7));
        assert_eq!(super::running_one(&[9, 7], |_| false), None, "none running: a new pane is opened with the task");
    }
    #[test]
    fn the_machine_and_the_cost_read_as_lines_an_agent_can_use() {
        let m = render_machine(&json!({ "tone": "loaded", "cpu": { "pct": 34.2, "cores": 8 }, "ram": { "used_mb": 10240, "total_mb": 16384, "pct": 62.5 }, "vram": null, "procs": [{ "name": "ollama", "kind": "local model", "cpu": 12.0, "ram_kb": 5_242_880 }] }));
        assert!(m.contains("machine: loaded") && m.contains("CPU 34% (8 cores)") && m.contains("RAM 62% (10.0 of 16.0 GB)") && m.contains("VRAM no GPU found") && m.contains("ollama (local model) cpu 12% ram 5120 MB"), "{m}");
        let c = render_cost(&json!({ "or_cost": 12.5, "or_prev": 10.0, "models": [{ "house": "claude", "model": "claude-opus-5-5", "tokens": 1500, "calls": 3, "or_cost": 4.2, "or_in": 4.0, "or_out": 20.0 }, { "house": "x", "model": "mystery", "tokens": 5, "calls": 1, "or_cost": null }], "limits": [{ "house": "claude", "label": "week", "used_pct": 25.0 }, { "house": "grok", "label": "own" }] }), 7);
        assert!(c.contains("last 7 days") && c.contains("$12.50") && c.contains("the period before: $10.00") && c.contains("claude · claude-opus-5-5 · 1500 tokens · 3 calls · $4.20") && c.contains("no price on OpenRouter's list") && c.contains("limit claude week: 25% used") && !c.contains("grok"), "{c}");
    }

    use super::*;

    fn layout() -> Value {
        json!({ "focus": "shell-8", "zoom": null, "root": { "type": "split", "dir": "row", "sizes": [0.5, 0.5], "children": [
            { "type": "leaf", "pane": { "id": "claude-code-7", "kind": "engine", "engine": "claude-code", "title": "Backender", "paneId": 7 } },
            { "type": "split", "dir": "col", "sizes": [0.5, 0.5], "children": [
                { "type": "leaf", "pane": { "id": "shell-8", "kind": "shell", "paneId": 8 } },
                { "type": "leaf", "pane": { "id": "issues-9", "kind": "tool", "tool": "issues", "title": "Issues", "paneId": 9 } } ] } ] } })
    }

    #[test]
    fn the_layout_reads_as_panes_in_order_and_says_which_is_focused_and_which_is_the_caller() {
        let p = panes_of(&layout());
        assert_eq!(p.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), vec!["claude-code-7", "shell-8", "issues-9"]);
        assert_eq!(render_panes(&p, 7), "claude-code-7 (pane 7) [engine] Backender · claude-code  ← this is you\nshell-8 (pane 8) [shell] terminal · terminal  ← focused\nissues-9 (pane 9) [tool] Issues · issues");
        assert_eq!(render_panes(&[], 7), NO_PANES); assert!(panes_of(&Value::Null).is_empty() && panes_of(&json!({ "root": null })).is_empty());
    }

    #[test]
    fn a_pane_is_named_by_its_id_its_number_or_this() {
        let p = panes_of(&layout());
        assert_eq!(resolve_pane(&p, &json!("shell-8"), 7).unwrap().pane_id, 8); assert_eq!(resolve_pane(&p, &json!(9), 7).unwrap().id, "issues-9");
        assert_eq!(resolve_pane(&p, &json!("9"), 7).unwrap().id, "issues-9"); assert_eq!(resolve_pane(&p, &json!("this"), 7).unwrap().id, "claude-code-7");
        assert!(resolve_pane(&p, &json!("nope"), 7).is_none() && resolve_pane(&p, &json!(99), 7).is_none() && resolve_pane(&p, &Value::Null, 7).is_none());
    }

    #[test]
    fn a_terminals_text_is_read_from_the_bottom_without_the_empty_rows() {
        let rows: Vec<String> = ["$ npm test", "  PASS  a.test.ts", "  FAIL  b.test.ts   ", "", "$ ", "", ""].iter().map(|x| x.to_string()).collect();
        assert_eq!(render_screen(&rows, 40), "$ npm test\n  PASS  a.test.ts\n  FAIL  b.test.ts\n\n$");
        assert_eq!(render_screen(&rows, 2), "\n$", "the last rows, blank ones inside kept");
        assert_eq!(render_screen(&["".to_string(), "  ".to_string()], 5), "(the terminal shows nothing yet)");
        assert_eq!(render_screen(&rows, 0).lines().count(), 1, "at least one row");
    }

    #[test]
    fn the_layout_reads_as_a_tree_with_each_split_s_direction_and_each_pane_s_share() {
        let t = render_tree(&json!({ "zoom": null, "root": { "type": "split", "dir": "row", "sizes": [0.6, 0.4], "children": [
            { "type": "leaf", "pane": { "id": "claude-code-7", "title": "Backender" } },
            { "type": "split", "dir": "col", "sizes": [1.0, 1.0], "children": [{ "type": "leaf", "pane": { "id": "shell-8" } }, { "type": "leaf", "pane": { "id": "issues-9", "title": "Issues" } }] }] } }));
        assert_eq!(t, "row (side by side)\n├─ 60% claude-code-7 \"Backender\"\n└─ 40% col (stacked)\n   ├─ 50% shell-8\n   └─ 50% issues-9 \"Issues\"");
        assert_eq!(render_tree(&json!({ "root": { "type": "leaf", "pane": { "id": "a" } } })), "a"); assert_eq!(render_tree(&Value::Null), ""); assert!(render_tree(&json!({ "zoom": "a", "root": { "type": "leaf", "pane": { "id": "a" } } })).ends_with("(zoomed: a fills the screen)"));
    }

    #[test]
    fn a_fraction_may_be_said_as_a_fraction_or_a_percent_and_the_look_reads_back_in_words() {
        assert_eq!((unit(&json!(0.4)), unit(&json!(40)), unit(&json!("40%")), unit(&json!(250)), unit(&json!(-1)), unit(&json!("x")), unit(&json!(0))), (Some(0.4), Some(0.4), Some(0.4), Some(1.0), Some(0.0), None, Some(0.0)));
        let data = json!({ "appearance": { "termOpacity": 0.2, "paneOpacity": 0, "dim": 0.4, "themeId": "nord", "wallpaper": null, "follow": false }, "theme": { "id": "nord", "name": "Nord" }, "wallpapers": ["antiloki.svg", "1-a.jpg"], "themes": [{ "id": "nord" }, { "id": "tokyo-night" }] });
        assert_eq!(render_look(&data), "terminals 20% opaque, panes 0% opaque, wallpaper darkened 40%\ntheme: Nord (nord)\nwallpaper: antiloki.svg (the theme's first)\nthis theme's wallpapers: antiloki.svg, 1-a.jpg\nthemes here: nord, tokyo-night");
        let own = json!({ "appearance": { "termOpacity": 1, "paneOpacity": 1, "dim": 0, "wallpaper": "", "follow": true }, "theme": null, "wallpapers": [], "themes": [] }); assert!(render_look(&own).contains("antiloki's own colours, following the desktop") && render_look(&own).contains("wallpaper: none"));
        assert_eq!(render_workspace(&json!({ "sidebar": "hidden", "workspaces": [{ "name": "ministore", "active": true }, { "name": "other", "active": false }] })), "sidebar: hidden\nworkspaces: ministore (open), other");
        assert_eq!(render_themes(&json!({ "current": "nord", "themes": [{ "id": "nord", "name": "Nord", "mode": "dark", "wallpapers": ["a.jpg"] }, { "id": "bare", "name": "Bare", "mode": "light", "wallpapers": [] }] })), "the desktop is wearing: nord\nnord (Nord, dark): a.jpg\nbare (Bare, light): no wallpapers"); assert_eq!(render_themes(&json!({ "themes": [] })), "no themes are installed.");
    }

    #[test]
    fn a_pane_is_shown_to_an_agent_as_its_controls_with_refs_and_then_what_it_says() {
        let v = render_view("sagas-3", &json!({ "title": "Sagas", "tool": "sagas", "view": { "text": "Sagas 2\n+ New saga", "truncated": false, "frames": [{ "name": "the app", "src": "http://localhost:3000" }], "elements": [
            { "ref": "e1", "role": "button", "name": "+ New saga", "testid": "saga-new" }, { "ref": "e2", "role": "textbox", "name": "A title", "value": "hi" }, { "ref": "e3", "role": "checkbox", "name": "Only mine", "checked": true },
            { "ref": "e4", "role": "select", "name": "status", "value": "b", "options": ["todo", "doing"] }, { "ref": "e5", "role": "button", "name": "locked", "disabled": true }] } }));
        assert!(v.starts_with("pane sagas-3 \"Sagas\" (sagas)\ncontrols (name one by its ref, its test id or its text):"), "{v}");
        for l in ["  [e1] button \"+ New saga\"  #saga-new", "  [e2] textbox \"A title\" value=\"hi\"", "  [e3] checkbox \"Only mine\" (on)", "  [e4] select \"status\" value=\"b\" options: todo | doing", "  [e5] button \"locked\" (disabled)"] { assert!(v.contains(l), "{l} in {v}"); }
        assert!(v.contains("a frame \"the app\" shows http://localhost:3000") && v.ends_with("what it says:\nSagas 2\n+ New saga"));
        assert!(render_view("x", &json!({ "view": { "elements": [], "text": "" } })).contains("(no controls on it)"));
        assert!(render_view("x", &json!({ "view": { "elements": [], "text": "t", "truncated": true } })).contains("(cut: ask for more text"));
    }

    #[test]
    fn git_status_reads_as_branch_changes_and_distance_from_the_remote() {
        assert_eq!(parse_status("## feat/candle...origin/feat/candle [ahead 2, behind 1]\n M src/a.ts\n?? b.ts\n"), ("feat/candle".to_string(), true, 2, 2, 1));
        assert_eq!(parse_status("## main...origin/main\n"), ("main".to_string(), true, 0, 0, 0));
        assert_eq!(parse_status("## wip\n M a\n"), ("wip".to_string(), false, 1, 0, 0));
        assert_eq!(parse_status("## No commits yet on main\n"), ("main".to_string(), false, 0, 0, 0));
        assert_eq!(parse_status(""), (String::new(), false, 0, 0, 0));
    }

    #[test]
    fn the_boxes_read_as_who_is_where_and_what_it_has_to_ship() {
        let b = |who: &str, pane: i64, branch: &str, changed: usize, ahead: usize, behind: usize, up: bool| Box { who: who.into(), engine: String::new(), pane_id: pane, path: format!("/w/{who}"), branch: branch.into(), changed, ahead, behind, upstream: up };
        let out = render_worktrees(&[b("the project's own checkout", 0, "main", 0, 0, 0, true), b("Backender", 7, "antiloki/backender", 3, 2, 0, true), b("Tester", 8, "antiloki/tester", 0, 0, 0, false)], 7);
        assert_eq!(out, "the project's own checkout · branch main · clean · /w/the project's own checkout\nBackender (pane 7) · branch antiloki/backender · 3 changed · 2 ahead · /w/Backender  ← your box\nTester (pane 8) · branch antiloki/tester · clean · not pushed · /w/Tester");
        assert_eq!(render_worktrees(&[], 1), "no worktrees found.");
    }

    #[test]
    fn a_qa_verdict_says_what_failed_what_the_page_had_and_where_the_pictures_are() {
        let checks = vec![("the cart says 1 item".to_string(), true, "1 item".to_string()), ("the total is $12".to_string(), false, "$0".to_string())];
        let out = render_qa(&checks, 1, &["TypeError: x is undefined".to_string()], &["goto / ok".to_string(), "click text=Add: nothing matches".to_string()], &[("cart".to_string(), "/box/.antiloki/qa/evidence/run-1/cart.png".to_string())], "http://localhost:5173");
        assert!(out.starts_with("QA on http://localhost:5173: 1 step failed, 1/2 checks passed, 1 console error"), "{out}");
        assert!(out.contains("✗ the total is $12 (the page had: $0)") && out.contains("console error: TypeError") && out.contains("  click text=Add: nothing matches") && out.contains("cart: /box/.antiloki/qa/evidence/run-1/cart.png"));
        assert!(render_qa(&[], 0, &[], &[], &[], "x").starts_with("QA on x: 0 steps failed, 0/0 checks passed"));
    }

    #[tokio::test]
    async fn a_run_nobody_holds_is_not_found_and_a_bad_plan_is_refused_before_anything_starts() {
        let r = qa_view([("id".to_string(), "42".to_string())].into_iter().collect()).await; assert_eq!(r.status(), StatusCode::NOT_FOUND);
        assert_eq!(qa_view(Default::default()).await.status(), StatusCode::BAD_REQUEST);
        let live = crate::qa_evidence::hub_open(777, None, 0); (live.sink)(json!({ "type": "step", "n": 1, "of": 2, "kind": "goto", "text": "open /" }));
        let r = qa_view([("id".to_string(), "777".to_string())].into_iter().collect()).await; assert_eq!(r.status(), StatusCode::OK);
        let b = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap(); let v: Value = serde_json::from_slice(&b).unwrap();
        assert_eq!((v["snapshot"]["type"].clone(), v["snapshot"]["timeline"][0]["text"].clone()), (json!("snapshot"), json!("open /")));
    }

    #[tokio::test]
    async fn with_no_screen_open_the_answer_is_immediate_and_says_so() {
        let e = run_on_screen("open", &json!({ "kind": "terminal" }), 5_000).await.unwrap_err();
        assert!(e.contains("no antiloki screen is open"), "{e}");
    }
}
