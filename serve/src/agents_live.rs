//! THE AGENTS, WHEREVER THE OPERATOR IS (operator 2026-09-20: "I'd like to leave the agent screen and
//! keep it running, and have a loading bar and preview floating at the bottom right so I can go anywhere,
//! like X-ray, and still see all my agents work — and click to be taken to them"). One call answers the
//! floating bar: every CLI pane of the active workspace's open sessions with what it is doing right now —
//! the chat session's running/busy, the PTY's running, the latest goal run's progress (tasks settled /
//! tasks), and a one-line preview from the pane's events file (the last thing the agent said, the tool
//! it is on). The agents themselves live on this serve, so leaving the screen never stops one.
use axum::{response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

use crate::agent::AgentRegistry;
use crate::dispatch::AppState;
use crate::goals::{GoalRun, Goals, TaskStatus};
use crate::pty::PtyRegistry;

/// THE PANES THAT ARE AGENTS — every seat, not three of them (audit 2026-09-28). While this was a 3-list,
/// `GET cockpit/agents/live` dropped every Gemini, Grok and Cursor pane, so the agents bar could not see three
/// of the five seats and the fork strip could not rebuild them. `chat_pane::ENGINES` is the one list.
const CLI_KINDS: [&str; 6] = crate::chat_pane::ENGINES;

fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }
fn one_line(t: &str, cap: usize) -> String {
    let t: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= cap { t } else { format!("{}…", t.chars().take(cap).collect::<String>()) }
}
/// A tool call in a few words: `Read src/x.ts` · `Bash npm test` · `Edit …`. Pure.
pub fn tool_hint(name: &str, input: &Value) -> String {
    let name = name.rsplit("__").next().unwrap_or(name);
    let arg = ["file_path", "command", "pattern", "path", "description", "prompt", "query", "url"].iter().map(|k| s(input.get(k))).find(|v| !v.is_empty()).unwrap_or_default();
    if arg.is_empty() { name.to_string() } else { format!("{name} {}", one_line(&arg, 60)) }
}
/// THE PREVIEW of a chat pane from the tail of its events file: the last text the agent wrote and the
/// last tool it called (with `busy`, that tool is the one it is on). Pure over the lines.
pub fn preview_from_lines<'a>(lines: impl Iterator<Item = &'a str>) -> Value {
    let (mut text, mut tool, mut t) = (String::new(), String::new(), 0i64);
    let mut tool_open = false; // a tool_use without its tool_result yet
    for line in lines {
        let Ok(w) = serde_json::from_str::<Value>(line) else { continue };
        let ev = w.get("ev").cloned().unwrap_or(Value::Null);
        let at = w.get("t").and_then(Value::as_i64).unwrap_or(0);
        match s(ev.get("type")).as_str() {
            "assistant" => {
                for b in ev.get("message").and_then(|m| m.get("content")).and_then(Value::as_array).cloned().unwrap_or_default() {
                    match s(b.get("type")).as_str() {
                        "text" => { let x = s(b.get("text")); if !x.trim().is_empty() { text = one_line(&x, 140); tool_open = false; t = at; } }
                        "tool_use" => { tool = tool_hint(&s(b.get("name")), b.get("input").unwrap_or(&Value::Null)); tool_open = true; t = at; }
                        _ => {}
                    }
                }
            }
            "user" => { if ev.get("message").and_then(|m| m.get("content")).and_then(Value::as_array).map(|a| a.iter().any(|b| s(b.get("type")) == "tool_result")).unwrap_or(false) { tool_open = false; } }
            "antiloki" => { if s(ev.get("subtype")) == "user" { text.clear(); tool.clear(); tool_open = false; t = at; } }
            _ => {}
        }
    }
    json!({ "text": text, "tool": tool, "tool_open": tool_open, "t": t })
}
fn preview(root: &str, pane_id: i64) -> Value {
    let p = crate::agent::events_read_path(root, pane_id);
    let Ok(md) = std::fs::metadata(&p) else { return json!({ "text": "", "tool": "", "tool_open": false, "t": 0 }) };
    // the tail is enough: the last exchange is what the bar shows
    let take = 96 * 1024u64; let len = md.len();
    let text = if len > take {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = match std::fs::File::open(&p) { Ok(f) => f, Err(_) => return json!({}) };
        // BYTES, then lossy: the seek can land inside a multi-byte character (the log is full of "—", "▸", accents), and
        // `read_to_string` would then return nothing at all - a blank preview - instead of just spoiling the line we drop anyway
        let _ = f.seek(SeekFrom::Start(len - take)); let mut raw = Vec::new(); let _ = f.read_to_end(&mut raw);
        String::from_utf8_lossy(&raw).splitn(2, '\n').nth(1).unwrap_or("").to_string() // drop the cut line
    } else { std::fs::read_to_string(&p).unwrap_or_default() };
    preview_from_lines(text.lines())
}
/// The latest goal run of a box, as progress: settled tasks over all tasks. Pure.
pub fn run_progress(runs: &[GoalRun], cwd: &str) -> Value {
    let Some(r) = runs.iter().filter(|r| r.cwd == cwd).max_by_key(|r| r.created) else { return Value::Null };
    let total = r.tasks.len();
    let done = r.tasks.iter().filter(|t| matches!(t.status, TaskStatus::Done | TaskStatus::Failed | TaskStatus::Blocked | TaskStatus::Skipped)).count();
    let running = r.tasks.iter().filter(|t| matches!(t.status, TaskStatus::Running | TaskStatus::Verifying)).count();
    json!({ "id": r.id, "status": r.status, "goal": one_line(&r.goal, 120), "done": done, "running": running, "total": total, "created": r.created, "ended": r.ended, "questions": r.questions.len() })
}

/// THE TRANSCRIPT of a chat pane (operator 2026-09-21: "@ should reference any other agent in any thread to add
/// its chat as context"): the operator's asks and the agent's texts from the events file, oldest first, as
/// markdown; tools are skipped. `cap` chars from the END — the latest exchanges are the context. Pure — tested.
pub fn transcript_from_lines<'a>(lines: impl Iterator<Item = &'a str>, cap: usize) -> String {
    let mut turns: Vec<String> = vec![];
    for line in lines {
        let Ok(w) = serde_json::from_str::<Value>(line) else { continue };
        let ev = w.get("ev").cloned().unwrap_or(Value::Null);
        match s(ev.get("type")).as_str() {
            "antiloki" if s(ev.get("subtype")) == "user" => { let t = s(ev.get("text")); if !t.trim().is_empty() { turns.push(format!("**operator:** {}", t.trim())); } }
            "assistant" => {
                let text = ev.get("message").and_then(|m| m.get("content")).and_then(Value::as_array).map(|a| a.iter().filter(|b| s(b.get("type")) == "text").map(|b| s(b.get("text"))).filter(|t| !t.trim().is_empty()).collect::<Vec<_>>().join("\n")).unwrap_or_default();
                if !text.trim().is_empty() { turns.push(format!("**agent:** {}", text.trim())); }
            }
            _ => {}
        }
    }
    let mut out = turns.join("\n\n");
    if out.chars().count() > cap {
        let skip = out.chars().count() - cap;
        out = out.chars().skip(skip).collect();
        if let Some(i) = out.find("\n\n**") { out = format!("…{}", &out[i..]); } else { out = format!("…{out}"); }
    }
    out
}
/// `GET /api/v1/cockpit/agent/transcript?pane_id=N&chars=8000`
pub async fn transcript(state: Arc<AppState>, q: std::collections::HashMap<String, String>) -> Response {
    let pane_id: i64 = q.get("pane_id").and_then(|v| v.parse().ok()).unwrap_or(0);
    let cap: usize = q.get("chars").and_then(|v| v.parse().ok()).unwrap_or(8_000).clamp(500, 60_000);
    if pane_id <= 0 { return (axum::http::StatusCode::BAD_REQUEST, Json(json!({ "error": { "message": "pane_id required" } }))).into_response(); }
    let conn = state.pick();
    let row = tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        antiloki_rs::db_json::query_rows(&c, "SELECT p.id, p.subagent_name, p.worktree_path, p.settings_json, s.name AS session_name FROM cockpit_panes p JOIN cockpit_sessions s ON s.id = p.session_id WHERE p.id = ?", &[json!(pane_id)]).ok().and_then(|r| r.as_array().and_then(|a| a.first().cloned()))
    }).await.unwrap_or(None);
    let Some(row) = row else { return (axum::http::StatusCode::NOT_FOUND, Json(json!({ "error": { "message": format!("pane {pane_id} not found") } }))).into_response() };
    let wt = s(row.get("worktree_path"));
    let p = crate::agent::events_read_path(&wt, pane_id);
    let text = std::fs::read_to_string(&p).unwrap_or_default();
    let md = transcript_from_lines(text.lines(), cap);
    let settings = serde_json::from_str::<Value>(&s(row.get("settings_json"))).unwrap_or(json!({}));
    let name = settings.get("recipe").and_then(|r| r.get("name")).and_then(Value::as_str).map(String::from).unwrap_or_else(|| s(row.get("session_name")));
    Json(json!({ "ok": true, "pane_id": pane_id, "name": name, "key": row.get("subagent_name"), "worktree": wt, "transcript": md, "chars": md.chars().count() })).into_response()
}

/// `GET /api/v1/cockpit/agents/live`
pub async fn live(state: Arc<AppState>, agents: Arc<AgentRegistry>, ptys: Arc<PtyRegistry>, goals: Arc<Goals>) -> Response {
    let conn = state.pick();
    let panes = tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        antiloki_rs::db_json::query_rows(&c, "SELECT p.id, p.session_id, s.name AS session_name, p.subagent_name, p.pane_kind, p.worktree_path, p.settings_json, p.status, p.last_activity FROM cockpit_panes p JOIN cockpit_sessions s ON s.id = p.session_id WHERE s.status != 'closed' AND COALESCE(p.status, '') != 'closed' AND p.worktree_path IS NOT NULL AND p.worktree_path != '' ORDER BY p.id ASC", &[]).unwrap_or(json!([]))
    }).await.unwrap_or(json!([]));
    let runs = goals.list(None);
    // one /proc walk for every agent on the page, not one per agent
    let mem_by_pane: std::collections::HashMap<i64, crate::mem::Watched> = agents.memory().into_iter().map(|w| (w.pane_id, w)).collect();
    let sys = crate::mem::system();
    let mut out = vec![];
    for p in panes.as_array().cloned().unwrap_or_default() {
        let kind = s(p.get("pane_kind"));
        if !CLI_KINDS.contains(&kind.as_str()) { continue; }
        let id = p.get("id").and_then(Value::as_i64).unwrap_or(0);
        let wt = s(p.get("worktree_path"));
        let settings = serde_json::from_str::<Value>(&s(p.get("settings_json"))).unwrap_or(json!({}));
        let chat = agents.status(id);
        let pty = ptys.status(id);
        let chat_running = chat.get("running").and_then(Value::as_bool).unwrap_or(false);
        let pty_running = pty.get("running").and_then(Value::as_bool).unwrap_or(false);
        let recipe = settings.get("recipe").cloned().unwrap_or(Value::Null);
        out.push(json!({
            "pane_id": id, "session_id": p.get("session_id"), "session_name": p.get("session_name"), "key": p.get("subagent_name"),
            // A FORK SAYS SO HERE (2026-09-27): the tab strip is rebuilt from what actually exists, because it used
            // to be React state only — a page refresh dropped the tabs while the panes and their worktrees stayed on
            // disk, invisible and un-closable. Two orphans accumulated that way in one evening.
            "forked_from": settings.get("forked_from").cloned().unwrap_or(Value::Null),
            "temporary": settings.get("temporary").cloned().unwrap_or(Value::Null),
            "kind": kind, "worktree": wt, "ui": if chat_running { "chat".to_string() } else if pty_running { "terminal".to_string() } else { s(settings.get("ui")) },
            "icon": recipe.get("icon").cloned().unwrap_or(Value::Null), "name": settings.get("title").filter(|t| t.as_str().map(|x| !x.trim().is_empty()).unwrap_or(false)).cloned().or_else(|| recipe.get("name").cloned()).unwrap_or(Value::Null),
            "max": settings.get("max").and_then(Value::as_bool).unwrap_or(false),
            "running": chat_running || pty_running, "busy": chat.get("busy").and_then(Value::as_bool).unwrap_or(false),
            "model": chat.get("model").cloned().unwrap_or(Value::Null), "cost_usd": chat.get("cost_usd").cloned().unwrap_or(Value::Null),
            "quota": pty.get("quota").cloned().filter(|q| !q.is_null()).or_else(|| crate::pty::quota_file(Path::new(&wt))).unwrap_or(Value::Null),
            "preview": preview(&wt, id), "run": run_progress(&runs, &wt), "last_activity": p.get("last_activity"),
            // the deterministic checks in flight: test · build · lint, so a box busy proving a claim does not
            // look like an agent sitting idle
            "checks": agents.checks_of(id),
            // WHAT IT IS COSTING IN MEMORY (2026-09-24), and whether the watchdog has it stopped
            "rss": mem_by_pane.get(&id).map(|w| w.rss).unwrap_or(0),
            "paused": mem_by_pane.get(&id).map(|w| w.paused).unwrap_or(false),
        }));
    }
    let machine = sys.map(|(total, avail)| json!({ "total": total, "available": avail,
        "pressure": crate::mem::pressure_of(total, avail), "human": crate::mem::human(avail) })).unwrap_or(Value::Null);
    Json(json!({ "ok": true, "agents": out, "machine": machine, "t": crate::cli::now_ms() })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_reads_the_last_exchange() {
        let lines = vec![
            r#"{"seq":1,"t":10,"ev":{"type":"antiloki","subtype":"user","text":"fix the scheduler"}}"#,
            r#"{"seq":2,"t":11,"ev":{"type":"assistant","message":{"content":[{"type":"text","text":"Looking at the   scheduler\nnow."},{"type":"tool_use","name":"Read","input":{"file_path":"src/scheduler.ts"}}]}}}"#,
        ];
        let p = preview_from_lines(lines.iter().copied());
        assert_eq!(p["text"], "Looking at the scheduler now."); assert_eq!(p["tool"], "Read src/scheduler.ts"); assert_eq!(p["tool_open"], true); assert_eq!(p["t"], 11);
        let more = [r#"{"seq":3,"t":12,"ev":{"type":"user","message":{"content":[{"type":"tool_result","content":"…"}]}}}"#, r#"{"seq":4,"t":13,"ev":{"type":"assistant","message":{"content":[{"type":"text","text":"Found it."}]}}}"#];
        let p = preview_from_lines(lines.iter().copied().chain(more.iter().copied()));
        assert_eq!(p["text"], "Found it."); assert_eq!(p["tool_open"], false);
        // a new ask clears the previous exchange
        let p = preview_from_lines(lines.iter().copied().chain(std::iter::once(r#"{"seq":5,"t":14,"ev":{"type":"antiloki","subtype":"user","text":"next"}}"#)));
        assert_eq!(p["text"], ""); assert_eq!(p["tool"], "");
        assert_eq!(tool_hint("mcp__antiloki__antiloki_delegate", &json!({ "goal": "x", "description": "split the work" })), "antiloki_delegate split the work");
        assert_eq!(tool_hint("Bash", &json!({ "command": "npm   test  --  --run" })), "Bash npm test -- --run");
        // the transcript: asks and texts only, oldest first; capped from the end at a turn boundary
        let md = transcript_from_lines(lines.iter().copied().chain(more.iter().copied()), 10_000);
        assert_eq!(md, "**operator:** fix the scheduler\n\n**agent:** Looking at the   scheduler\nnow.\n\n**agent:** Found it.");
        let short = transcript_from_lines(lines.iter().copied().chain(more.iter().copied()), 30);
        assert!(short.starts_with("…") && short.ends_with("**agent:** Found it."), "{short}");
    }

    // ── PHASE 5 (PLAN-THE-TESTS): the bar's rules at their edges ──
    fn scratch(tag: &str) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!("antiloki-live-{tag}-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".antiloki")).unwrap();
        d
    }
    fn ev_user(seq: u64, t: i64, text: &str) -> String { json!({ "seq": seq, "t": t, "ev": { "type": "antiloki", "subtype": "user", "text": text } }).to_string() }
    fn ev_say(seq: u64, t: i64, text: &str) -> String { json!({ "seq": seq, "t": t, "ev": { "type": "assistant", "message": { "content": [{ "type": "text", "text": text }] } } }).to_string() }

    #[test]
    fn a_tool_is_named_in_a_few_words_by_the_first_argument_that_says_something() {
        assert_eq!(tool_hint("Read", &json!({ "file_path": "src/x.ts", "command": "ignored" })), "Read src/x.ts", "file_path outranks command");
        assert_eq!(tool_hint("Grep", &json!({ "pattern": "TODO", "path": "src" })), "Grep TODO", "pattern outranks path");
        assert_eq!(tool_hint("Bash", &json!({ "command": "" , "description": "run the tests" })), "Bash run the tests", "an empty argument is skipped, not chosen");
        assert_eq!(tool_hint("Task", &json!({})), "Task", "no argument, no trailing space");
        assert_eq!(tool_hint("Task", &Value::Null), "Task");
        assert_eq!(tool_hint("mcp__server__deep__tool_name", &json!({ "query": "x" })), "tool_name x", "only the last segment of a namespaced tool");
        assert_eq!(tool_hint("", &json!({ "url": "http://x" })), " http://x", "an unnamed tool does not panic");
        let long = tool_hint("Bash", &json!({ "command": "x".repeat(200) }));
        assert_eq!(long.chars().count(), "Bash ".len() + 60 + 1, "{long}");
        assert!(long.ends_with('…'));
        assert_eq!(tool_hint("Bash", &json!({ "command": "é".repeat(60) })), format!("Bash {}", "é".repeat(60)), "60 characters (not bytes) fit whole");
    }

    #[test]
    fn one_line_collapses_whitespace_and_cuts_on_characters_at_the_boundary() {
        assert_eq!(one_line("  a \n\t b   c ", 10), "a b c");
        assert_eq!(one_line("abcde", 5), "abcde", "exactly the cap is untouched");
        assert_eq!(one_line("abcdef", 5), "abcde…");
        assert_eq!(one_line("日本語日本語", 3), "日本語…", "characters, never a split code point");
        assert_eq!(one_line("", 5), "");
        assert_eq!(one_line("   ", 5), "");
    }

    #[test]
    fn a_preview_ignores_junk_and_empty_text_and_tracks_whether_the_last_tool_is_still_open() {
        let assistant = |blocks: Value, t: i64| json!({ "seq": 1, "t": t, "ev": { "type": "assistant", "message": { "content": blocks } } }).to_string();
        let empty = preview_from_lines(std::iter::empty());
        assert_eq!(empty, json!({ "text": "", "tool": "", "tool_open": false, "t": 0 }));
        let lines = vec![
            "not json at all".to_string(), "{}".to_string(), "".to_string(), r#"{"ev":{"type":"weird"}}"#.to_string(),
            assistant(json!([{ "type": "text", "text": "   " }]), 5),                                   // blank text: ignored, t untouched
            assistant(json!([{ "type": "tool_use", "name": "Bash", "input": { "command": "make" } }]), 6),
        ];
        let p = preview_from_lines(lines.iter().map(String::as_str));
        assert_eq!((p["text"].as_str(), p["tool"].as_str(), p["tool_open"].as_bool(), p["t"].as_i64()), (Some(""), Some("Bash make"), Some(true), Some(6)));
        // the agent speaks after the tool: the tool is over (its text is now the latest), but the tool name is remembered
        let mut more = lines.clone();
        more.push(assistant(json!([{ "type": "text", "text": "built it" }]), 7));
        let p = preview_from_lines(more.iter().map(String::as_str));
        assert_eq!((p["text"].as_str(), p["tool"].as_str(), p["tool_open"].as_bool(), p["t"].as_i64()), (Some("built it"), Some("Bash make"), Some(false), Some(7)));
        // a user event that is not a tool result leaves an open tool open
        let mut chatty = lines.clone();
        chatty.push(json!({ "t": 8, "ev": { "type": "user", "message": { "content": [{ "type": "text", "text": "hm" }] } } }).to_string());
        assert_eq!(preview_from_lines(chatty.iter().map(String::as_str))["tool_open"], true);
        // a very long line of speech is cut to 140 characters
        let p = preview_from_lines([assistant(json!([{ "type": "text", "text": "w ".repeat(200) }]), 1)].iter().map(String::as_str));
        assert_eq!(p["text"].as_str().unwrap().chars().count(), 141);
    }

    #[test]
    fn the_preview_of_a_missing_or_unreadable_log_is_empty_not_an_error() {
        let d = scratch("preview-missing");
        assert_eq!(preview(&d.to_string_lossy(), 1), json!({ "text": "", "tool": "", "tool_open": false, "t": 0 }));
        std::fs::write(crate::agent::events_read_path(&d.to_string_lossy(), 2), "").unwrap();
        assert_eq!(preview(&d.to_string_lossy(), 2)["text"], "");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_preview_of_a_huge_log_reads_only_the_tail_and_drops_the_line_it_cut() {
        let d = scratch("preview-big");
        let root = d.to_string_lossy().to_string();
        let mut body = String::new();
        body.push_str(&ev_user(1, 1, "the first ask, long ago")); body.push('\n');
        body.push_str(&ev_say(2, 2, "the FIRST answer")); body.push('\n');
        let filler = ev_say(3, 3, &"padding ".repeat(1000));
        while body.len() < 300 * 1024 { body.push_str(&filler); body.push('\n'); }
        body.push_str(&ev_user(4, 4, "the last ask")); body.push('\n');
        body.push_str(&ev_say(5, 5, "the LAST answer")); body.push('\n');
        std::fs::write(crate::agent::events_read_path(&root, 1), &body).unwrap();
        let p = preview(&root, 1);
        assert_eq!((p["text"].as_str(), p["t"].as_i64()), (Some("the LAST answer"), Some(5)), "the bar shows the latest exchange of a 300 KB log");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// RULE: the tail read may start in the middle of a multi-byte character; that must cost the cut line, never the whole preview.
    #[test]
    fn a_tail_cut_inside_a_multibyte_character_still_shows_the_last_answer() {
        let take = 96 * 1024usize;
        // find a tail length that puts the cut byte (len - take) on a UTF-8 continuation byte: trailing spaces in the last
        // answer shift where the cut falls, and one_line() trims them from the preview
        let mut found = None;
        for pad in 0..64usize {
            let mut body = String::new();
            while body.len() < 200 * 1024 { body.push_str(&ev_say(1, 1, &"ção ".repeat(500))); body.push('\n'); }
            body.push_str(&ev_say(9, 9, &format!("a resposta final — está pronta{}", " ".repeat(pad)))); body.push('\n');
            let cut = body.len() - take;
            if body.as_bytes()[cut] & 0xC0 == 0x80 { found = Some(body); break; }
        }
        let body = found.expect("some padding lands the cut inside a character");
        let d = scratch("preview-utf8");
        let root = d.to_string_lossy().to_string();
        std::fs::write(crate::agent::events_read_path(&root, 1), &body).unwrap();
        assert_eq!(preview(&root, 1)["text"], "a resposta final — está pronta");
        let _ = std::fs::remove_dir_all(&d);
    }

    fn run_of(id: i64, cwd: &str, created: i64, statuses: &[&str]) -> GoalRun {
        let mut r: GoalRun = serde_json::from_value(json!({ "id": id, "parent": "p", "goal": "a   goal\nover lines", "engine": "claude-code", "cwd": cwd, "write": false, "concurrency": 1, "status": "running", "created": created, "tasks": [], "blackboard": [], "events": [], "delivered": false, "router": true, "min": false, "plan_args": { "n": 4, "router": true, "auto": false, "always": false }, "stages": [], "qa_double": false, "qa_review_wanted": false, "qa_baseline_pending": false, "mid_task": false })).unwrap();
        for (i, st) in statuses.iter().enumerate() {
            r.tasks.push(serde_json::from_value(json!({ "id": format!("t{i}"), "title": "t", "prompt": "p", "owns": [], "needs": [], "acceptance": [], "status": st, "wave": 0, "attempts": 0, "started": null, "ended": null, "report": null, "verdict": null, "error": null, "trace": null, "changed": [] })).unwrap());
        }
        r
    }

    #[test]
    fn progress_counts_settled_and_running_tasks_of_the_newest_run_in_that_box_only() {
        assert_eq!(run_progress(&[], "/box"), Value::Null);
        let other_box = run_of(1, "/other", 100, &["done"]);
        assert_eq!(run_progress(&[other_box.clone()], "/box"), Value::Null, "another box's run is not this agent's");
        let old = run_of(2, "/box", 10, &["done", "done"]);
        let new = run_of(3, "/box", 20, &["done", "failed", "blocked", "skipped", "running", "verifying", "pending", "ready"]);
        let p = run_progress(&[old, other_box, new], "/box");
        assert_eq!((p["id"].clone(), p["done"].clone(), p["running"].clone(), p["total"].clone()), (json!(3), json!(4), json!(2), json!(8)), "done = done+failed+blocked+skipped; running = running+verifying; pending and ready are neither");
        assert_eq!(p["goal"], "a goal over lines", "the goal is one line");
        assert_eq!((p["status"].clone(), p["questions"].clone()), (json!("running"), json!(0)));
        // a run with no tasks yet is 0 of 0, not a division
        assert_eq!(run_progress(&[run_of(4, "/box", 5, &[])], "/box")["total"], 0);
    }

    #[test]
    fn a_transcript_keeps_the_asks_and_the_words_and_nothing_else_and_cuts_at_a_turn() {
        let tool = json!({ "seq": 2, "t": 2, "ev": { "type": "assistant", "message": { "content": [{ "type": "tool_use", "name": "Read", "input": {} }, { "type": "text", "text": "" }] } } }).to_string();
        let two_blocks = json!({ "seq": 3, "t": 3, "ev": { "type": "assistant", "message": { "content": [{ "type": "text", "text": "first block" }, { "type": "tool_use", "name": "X", "input": {} }, { "type": "text", "text": "second block" }] } } }).to_string();
        let lines = vec![ev_user(1, 1, "  the ask  "), tool, "garbage".into(), ev_user(4, 4, "   "), two_blocks, ev_say(5, 5, "  ")];
        let md = transcript_from_lines(lines.iter().map(String::as_str), 10_000);
        assert_eq!(md, "**operator:** the ask\n\n**agent:** first block\nsecond block", "tool-only turns, blank asks and junk vanish; the blocks of one turn are joined");
        assert_eq!(transcript_from_lines(std::iter::empty(), 100), "");
        // the cap counts characters from the END; exactly at the cap nothing is cut
        let len = md.chars().count();
        assert_eq!(transcript_from_lines(lines.iter().map(String::as_str), len), md);
        let cut = transcript_from_lines(lines.iter().map(String::as_str), len - 1);
        assert!(cut.starts_with("…\n\n**agent:**") || cut.starts_with("…**agent:**") || cut.starts_with("…"), "{cut}");
        assert!(cut.ends_with("second block") && !cut.contains("the ask"), "the oldest turn goes first: {cut}");
        // one enormous turn has no boundary to cut at: it is trimmed from its front with an ellipsis
        let big = vec![ev_say(1, 1, &"word ".repeat(400))];
        let t = transcript_from_lines(big.iter().map(String::as_str), 100);
        assert!(t.starts_with('…') && t.chars().count() <= 101, "{}", t.chars().count());
        // characters, not bytes
        let uni = vec![ev_say(1, 1, &"ç".repeat(50))];
        assert_eq!(transcript_from_lines(uni.iter().map(String::as_str), 20).chars().count(), 21);
    }

    // ── the two handlers, over a database with the two tables they read ──
    fn cockpit(panes: &[(i64, i64, &str, &str, Option<&str>, &str, Value)]) -> Arc<AppState> {
        // (id, session_id, subagent_name, pane_kind, worktree_path, status, settings)
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE cockpit_sessions (id INTEGER PRIMARY KEY, name TEXT, status TEXT);
            CREATE TABLE cockpit_panes (id INTEGER PRIMARY KEY, session_id INTEGER, subagent_name TEXT, pane_kind TEXT, worktree_path TEXT, settings_json TEXT, status TEXT, last_activity TEXT);
            INSERT INTO cockpit_sessions VALUES (1, 'main', 'open'), (2, 'old', 'closed');").unwrap();
        for (id, sid, key, kind, wt, status, settings) in panes {
            conn.execute("INSERT INTO cockpit_panes (id, session_id, subagent_name, pane_kind, worktree_path, status, settings_json, last_activity) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'yesterday')", rusqlite::params![id, sid, key, kind, wt, status, settings.to_string()]).unwrap();
        }
        crate::dispatch::test_app_state(conn)
    }
    async fn json_of(r: Response) -> (axum::http::StatusCode, Value) {
        let st = r.status();
        let b = axum::body::to_bytes(r.into_body(), usize::MAX).await.unwrap();
        (st, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }
    fn q(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> { pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() }

    #[tokio::test]
    async fn the_transcript_route_needs_a_real_pane_and_reads_its_log_with_a_clamped_cap() {
        let d = scratch("transcript");
        let root = d.to_string_lossy().to_string();
        std::fs::write(crate::agent::events_read_path(&root, 5), [ev_user(1, 1, "hello agent"), ev_say(2, 2, &"answer ".repeat(300))].join("\n")).unwrap();
        let st = cockpit(&[(5, 1, "alice", "claude-code", Some(&root), "idle", json!({ "recipe": { "name": "Alice" } })), (6, 1, "bob", "codex", Some(&root), "idle", json!({}))]);
        for bad in [q(&[]), q(&[("pane_id", "0")]), q(&[("pane_id", "-4")]), q(&[("pane_id", "abc")])] {
            let (code, v) = json_of(transcript(st.clone(), bad).await).await;
            assert_eq!((code, v["error"]["message"].as_str()), (axum::http::StatusCode::BAD_REQUEST, Some("pane_id required")));
        }
        let (code, v) = json_of(transcript(st.clone(), q(&[("pane_id", "99")])).await).await;
        assert_eq!((code, v["error"]["message"].as_str()), (axum::http::StatusCode::NOT_FOUND, Some("pane 99 not found")));
        let (code, v) = json_of(transcript(st.clone(), q(&[("pane_id", "5"), ("chars", "1")])).await).await;
        assert_eq!(code, axum::http::StatusCode::OK);
        assert_eq!((v["name"].as_str(), v["key"].as_str(), v["worktree"].as_str()), (Some("Alice"), Some("alice"), Some(root.as_str())), "the recipe's name, else the session's");
        assert_eq!(v["chars"], 501, "chars=1 is raised to the 500 floor (+ the ellipsis)");
        assert!(v["transcript"].as_str().unwrap().starts_with('…'));
        let (_, v) = json_of(transcript(st.clone(), q(&[("pane_id", "5"), ("chars", "9999999")])).await).await;
        assert!(v["chars"].as_i64().unwrap() < 60_000 && v["transcript"].as_str().unwrap().contains("hello agent"), "and the ceiling is 60000");
        // a pane without a recipe is named by its session; one with no log has an empty transcript
        let (_, v) = json_of(transcript(st.clone(), q(&[("pane_id", "6")])).await).await;
        assert_eq!(v["name"], "main");
        let empty = scratch("transcript-empty");
        let st2 = cockpit(&[(7, 1, "carol", "claude-code", Some(&empty.to_string_lossy()), "idle", json!({}))]);
        let (_, v) = json_of(transcript(st2, q(&[("pane_id", "7")])).await).await;
        assert_eq!((v["transcript"].as_str(), v["chars"].as_i64()), (Some(""), Some(0)));
        let _ = (std::fs::remove_dir_all(&d), std::fs::remove_dir_all(&empty));
    }

    #[tokio::test]
    async fn the_floating_bar_lists_every_open_cli_pane_in_order_and_nothing_else() {
        let a = scratch("live-a"); let b = scratch("live-b"); let c = scratch("live-c");
        std::fs::write(crate::agent::events_read_path(&a.to_string_lossy(), 11), [ev_user(1, 1, "go"), ev_say(2, 2, "working on   it")].join("\n")).unwrap();
        let (ra, rb, rc) = (a.to_string_lossy().to_string(), b.to_string_lossy().to_string(), c.to_string_lossy().to_string());
        let st = cockpit(&[
            (12, 1, "second", "codex", Some(&rb), "idle", json!({ "title": "Mine", "recipe": { "name": "Recipe", "icon": "R" }, "max": true })),
            (11, 1, "first", "claude-code", Some(&ra), "working", json!({ "title": "  ", "recipe": { "name": "Recipe", "icon": "R" }, "ui": "terminal", "forked_from": 3, "temporary": true })),
            (13, 1, "shell", "shell", Some(&rc), "idle", json!({})),                    // not a CLI agent
            (14, 2, "closed-session", "claude-code", Some(&rc), "idle", json!({})),     // its session is closed
            (15, 1, "closed-pane", "claude-code", Some(&rc), "closed", json!({})),      // the pane is closed
            (16, 1, "no-box", "claude-code", Some(""), "idle", json!({})),              // no worktree yet
            (17, 1, "null-box", "claude-code", None, "idle", json!({})),
            (18, 1, "cursor-seat", "cursor", Some(&rc), "idle", json!({})),             // every seat counts, not three of them
            (19, 1, "gemini-seat", "gemini", Some(&rc), "idle", json!({})),
        ]);
        let (code, v) = json_of(live(st, crate::agent::AgentRegistry::new(), Arc::new(crate::pty::PtyRegistry::default()), crate::goals::Goals::new(crate::goals::scripted_prompts_for_queue().0, None, 1, 5)).await).await;
        assert_eq!(code, axum::http::StatusCode::OK);
        let agents = v["agents"].as_array().unwrap();
        assert_eq!(agents.iter().map(|x| x["pane_id"].as_i64().unwrap()).collect::<Vec<_>>(), vec![11, 12, 18, 19], "ordered by pane id; shells, closed sessions/panes and box-less panes are not agents");
        let first = &agents[0];
        assert_eq!((first["name"].as_str(), first["icon"].as_str(), first["ui"].as_str(), first["kind"].as_str()), (Some("Recipe"), Some("R"), Some("terminal"), Some("claude-code")), "a blank title falls to the recipe; the ui setting shows while nothing is running");
        assert_eq!((first["forked_from"].clone(), first["temporary"].clone(), first["running"].clone(), first["busy"].clone(), first["paused"].clone()), (json!(3), json!(true), json!(false), json!(false), json!(false)));
        assert_eq!(first["preview"]["text"], "working on it");
        assert_eq!(first["run"], Value::Null);
        assert_eq!(first["checks"], json!([]));
        assert_eq!(first["last_activity"], "yesterday");
        assert_eq!((agents[1]["name"].as_str(), agents[1]["max"].clone()), (Some("Mine"), json!(true)), "a set title beats the recipe's name");
        assert_eq!(agents[2]["name"], Value::Null, "no title, no recipe: the FE names it");
        assert_eq!(agents[2]["ui"], "");
        assert!(v["t"].as_i64().unwrap() > 0);
        let _ = (std::fs::remove_dir_all(&a), std::fs::remove_dir_all(&b), std::fs::remove_dir_all(&c));
    }

    #[tokio::test]
    async fn the_bar_over_an_empty_or_unreadable_database_is_an_empty_list() {
        let bare = crate::dispatch::test_app_state(rusqlite::Connection::open_in_memory().unwrap());
        let (code, v) = json_of(live(bare, crate::agent::AgentRegistry::new(), Arc::new(crate::pty::PtyRegistry::default()), crate::goals::Goals::new(crate::goals::scripted_prompts_for_queue().0, None, 1, 5)).await).await;
        assert_eq!((code, v["agents"].clone(), v["ok"].clone()), (axum::http::StatusCode::OK, json!([]), json!(true)), "no tables at all: the bar shows nothing, it does not fail");
    }
}
