//! engine-handoff.rs — THE QUOTA HANDOFF (operator 2026-09-20: "we must have a handler for quota limit,
//! and also for approaching quota limit … warn, and on quota limit export the full artefact of that
//! chat for another of our choice, like a button to open another agent of another provider"). NO JS TWIN.
//!
//! Three pure-ish pieces, one tool (`antiloki_handoff`, dispatched by the pane context tools):
//!
//! * `quota_state(usage, hit_text)` — the pane's standing against its plan windows: `ok` under 80 %,
//!   `warn` from 80 %, `limit` at 100 % or when the terminal printed the CLI's own limit line.
//! * `transcript(engine, worktree)` — the pane's OWN conversation, read from where each CLI keeps it
//!   (read-only, never written): Claude's `~/.claude/projects/<slug>/<session>.jsonl` (the session id
//!   the PTY started it with, recorded in `.antiloki/pty-session.json`), Codex's newest TUI rollout for
//!   the box (`originator != codex_exec` — the goal runs' headless calls write there too), OpenCode's
//!   newest session for the directory whose first message is not one of antiloki's own headless prompts.
//! * `handoff_md(…)` — the artefact another engine continues from: why, the box, the files touched
//!   (audit + git), and the conversation, bounded.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const TURN_MAX: usize = 4_000;
const BODY_MAX: usize = 120_000;
const TURNS_MAX: usize = 80;

fn s(v: Option<&Value>) -> String { match v { Some(Value::String(x)) => x.clone(), Some(Value::Null) | None => String::new(), Some(o) => o.to_string() } }
fn home() -> PathBuf { std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).map(PathBuf::from).unwrap_or_default() }

/// The CLIs' own words when the plan window is spent — matched case-insensitively on ANSI-stripped
/// terminal text (serve's PTY reader) and on a headless run's error.
pub const LIMIT_PHRASES: &[&str] = &[
    "hit your usage limit", "hit your limit", "usage limit reached", "reached your usage limit", "usage limit has been reached",
    "rate limit reached", "quota exceeded", "insufficient_quota", "you have exceeded your", "out of credits", "credit balance is too low",
];
pub fn limit_hit(text: &str) -> Option<&'static str> {
    let t = text.to_ascii_lowercase();
    LIMIT_PHRASES.iter().copied().find(|p| t.contains(p))
}

/// `{ level: ok|warn|limit, pct, window, resets_at, why }` — the worst plan window decides; a limit
/// line seen in the terminal wins over the numbers (they lag a turn).
pub fn quota_state(usage: &Value, hit_text: Option<&str>) -> Value {
    let mut worst: Option<(f64, String, Option<i64>)> = None;
    for l in usage.get("limits").and_then(Value::as_array).into_iter().flatten() {
        let pct = l.get("used_pct").and_then(Value::as_f64).unwrap_or(0.0);
        if worst.as_ref().map(|w| pct > w.0).unwrap_or(true) { worst = Some((pct, s(l.get("label")), l.get("resets_at").and_then(Value::as_i64))); }
    }
    let (pct, window, resets_at) = worst.unwrap_or((0.0, String::new(), None));
    let hit = hit_text.map(|t| t.trim().chars().take(200).collect::<String>()).filter(|t| !t.is_empty());
    let level = if hit.is_some() || pct >= 100.0 { "limit" } else if pct >= 80.0 { "warn" } else { "ok" };
    let why = match (level, &hit) {
        ("limit", Some(t)) => format!("the CLI said: {t}"),
        ("limit", None) => format!("the {window} window is spent ({pct:.0}%)"),
        ("warn", _) => format!("the {window} window is at {pct:.0}%"),
        _ => String::new(),
    };
    json!({ "level": level, "pct": pct, "window": window, "resets_at": resets_at, "why": why })
}

/// A v4-shaped UUID without a uuid crate: sha256 over what makes this start unique.
pub fn session_uuid(seed: &str) -> String {
    use sha2::Digest;
    let h = sha2::Sha256::digest(seed.as_bytes());
    let mut b = [0u8; 16]; b.copy_from_slice(&h[..16]);
    b[6] = (b[6] & 0x0f) | 0x40; b[8] = (b[8] & 0x3f) | 0x80;
    let x = hex::encode(b);
    format!("{}-{}-{}-{}-{}", &x[0..8], &x[8..12], &x[12..16], &x[16..20], &x[20..32])
}

/// The heads of antiloki's own headless prompts — a session that starts with one is a goal run's
/// call, not the pane's conversation (OpenCode keeps no originator to tell them apart).
const HEADLESS_HEADS: &[&str] = &["You are the ", "You are task ", "You are one of ", "You are running an ", "You are reading the repositor", "You are an engine of antiloki", "Split the goal below"];
fn headless(first_user_text: &str) -> bool { HEADLESS_HEADS.iter().any(|h| first_user_text.trim_start().starts_with(h)) }

#[derive(Clone, Debug, PartialEq)]
pub struct Turn { pub role: String, pub text: String }
fn turn(role: &str, text: &str) -> Option<Turn> {
    let t = text.trim();
    if t.is_empty() { return None; }
    let text: String = if t.chars().count() > TURN_MAX { format!("{}\n… (cut at {TURN_MAX} chars)", t.chars().take(TURN_MAX).collect::<String>()) } else { t.to_string() };
    Some(Turn { role: role.into(), text })
}

/// Claude Code: `~/.claude/projects/<slug>/<session>.jsonl`. `session` = the id the PTY started the
/// TUI with (`.antiloki/pty-session.json`), else the newest file whose first prompt is not headless.
pub fn claude_transcript(worktree: &Path) -> (String, Vec<Turn>) {
    let slug = crate::context_context_library::claude_project_slug(&worktree.to_string_lossy());
    let dir = home().join(".claude").join("projects").join(slug);
    let pinned = pty_session(worktree).filter(|(e, _)| e == "claude-code").map(|(_, id)| dir.join(format!("{id}.jsonl"))).filter(|p| p.is_file());
    let file = pinned.or_else(|| {
        let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&dir).ok().into_iter().flatten().flatten().map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("jsonl"))
            .filter_map(|p| std::fs::metadata(&p).and_then(|m| m.modified()).ok().map(|m| (m, p))).collect();
        files.sort_by(|a, b| b.0.cmp(&a.0));
        files.into_iter().map(|(_, p)| p).find(|p| { let (_, t) = claude_from_jsonl(&std::fs::read_to_string(p).unwrap_or_default()); t.first().map(|f| !headless(&f.text)).unwrap_or(false) })
    });
    match file { Some(p) => { let (_, turns) = claude_from_jsonl(&std::fs::read_to_string(&p).unwrap_or_default()); (p.to_string_lossy().to_string(), turns) }, None => (String::new(), vec![]) }
}
/// Pure over the file's text: user/assistant text blocks; a tool use becomes one `[tool …]` line.
pub fn claude_from_jsonl(text: &str) -> (usize, Vec<Turn>) {
    let mut turns = vec![]; let mut lines = 0;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        lines += 1;
        let ty = s(v.get("type"));
        if ty != "user" && ty != "assistant" { continue; }
        let content = v.get("message").and_then(|m| m.get("content")).cloned().unwrap_or(Value::Null);
        let mut parts: Vec<String> = vec![];
        match &content {
            Value::String(t) => parts.push(t.clone()),
            Value::Array(blocks) => for b in blocks {
                match s(b.get("type")).as_str() {
                    "text" => parts.push(s(b.get("text"))),
                    "tool_use" => { let input = b.get("input").map(|i| i.to_string()).unwrap_or_default(); parts.push(format!("[tool {} {}]", s(b.get("name")), input.chars().take(160).collect::<String>())); }
                    _ => {} // tool_result blocks are the tools' output — not the conversation
                }
            },
            _ => {}
        }
        let joined = parts.join("\n");
        // the TUI's own injected turns (slash-command caveats, local command echoes) are not the conversation
        if ty == "user" && (joined.trim_start().starts_with("<local-command") || joined.trim_start().starts_with("<command-") || joined.trim_start().starts_with("<system-reminder>")) { continue; }
        if let Some(t) = turn(&ty, &joined) { turns.push(t); }
    }
    (lines, turns)
}

/// Codex: the newest rollout for this cwd that the TUI wrote (`originator` ≠ `codex_exec`).
pub fn codex_transcript(worktree: &Path) -> (String, Vec<Turn>) {
    let Some(root) = crate::cockpit__helpers_engine_usage::codex_home() else { return (String::new(), vec![]) };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    crate::cockpit__helpers_engine_usage::walk_jsonl(&root.join("sessions"), &mut files, 0);
    files.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, p) in files.into_iter().take(400) {
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Some(first) = text.lines().next() else { continue };
        let Ok(meta) = serde_json::from_str::<Value>(first) else { continue };
        let pl = meta.get("payload").cloned().unwrap_or(Value::Null);
        if s(pl.get("originator")) == "codex_exec" { continue; }
        let cwd = s(pl.get("cwd"));
        if cwd.is_empty() || !crate::cockpit__helpers_engine_usage::same_dir(&cwd, worktree) { continue; }
        return (p.to_string_lossy().to_string(), codex_from_rollout(&text));
    }
    (String::new(), vec![])
}
/// Pure: `response_item` messages (user/assistant text) and function calls as `[tool …]` lines.
pub fn codex_from_rollout(text: &str) -> Vec<Turn> {
    let mut turns = vec![];
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if s(v.get("type")) != "response_item" { continue; }
        let pl = v.get("payload").cloned().unwrap_or(Value::Null);
        match s(pl.get("type")).as_str() {
            "message" => {
                let role = s(pl.get("role"));
                if role != "user" && role != "assistant" { continue; }
                let text = pl.get("content").and_then(Value::as_array).map(|a| a.iter().filter_map(|c| c.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n")).unwrap_or_default();
                // the TUI's own injected turns (environment context, slash commands) are not the conversation
                let head = text.trim_start(); if head.starts_with("<environment_context>") || head.starts_with("<command-name>") || head.starts_with("<user_instructions>") || head.starts_with("# AGENTS.md instructions") || head.starts_with("<INSTRUCTIONS>") { continue; }
                if let Some(t) = turn(&role, &text) { turns.push(t); }
            }
            "function_call" => { if let Some(t) = turn("assistant", &format!("[tool {} {}]", s(pl.get("name")), s(pl.get("arguments")).chars().take(160).collect::<String>())) { turns.push(t); } }
            _ => {}
        }
    }
    turns
}

/// OpenCode: its sqlite db — the newest session for this directory whose first user message is not
/// one of antiloki's headless prompts; the messages' text parts in order.
pub fn opencode_transcript(worktree: &Path) -> (String, Vec<Turn>) {
    let p = home().join(".local").join("share").join("opencode").join("opencode.db");
    let Ok(c) = rusqlite::Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return (String::new(), vec![]) };
    let want = worktree.to_string_lossy().to_string();
    let Ok(mut st) = c.prepare("SELECT id, title FROM session WHERE directory = ? ORDER BY time_updated DESC LIMIT 40") else { return (String::new(), vec![]) };
    let sessions: Vec<(String, String)> = st.query_map([&want], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).ok().into_iter().flatten().flatten().collect();
    for (id, title) in sessions {
        let turns = opencode_session_turns(&c, &id);
        if turns.first().map(|f| headless(&f.text)).unwrap_or(true) { continue; }
        return (format!("{} · {title} ({id})", p.display()), turns);
    }
    (String::new(), vec![])
}
fn opencode_session_turns(c: &rusqlite::Connection, session_id: &str) -> Vec<Turn> {
    let mut turns = vec![];
    let Ok(mut ms) = c.prepare("SELECT id, data FROM message WHERE session_id = ? ORDER BY time_created ASC LIMIT 400") else { return turns };
    let msgs: Vec<(String, String)> = ms.query_map([session_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).ok().into_iter().flatten().flatten().collect();
    let Ok(mut ps) = c.prepare("SELECT data FROM part WHERE message_id = ? ORDER BY time_created ASC LIMIT 200") else { return turns };
    for (mid, data) in msgs {
        let role = serde_json::from_str::<Value>(&data).ok().map(|v| s(v.get("role"))).unwrap_or_default();
        if role != "user" && role != "assistant" { continue; }
        let parts: Vec<String> = ps.query_map([&mid], |r| r.get::<_, String>(0)).ok().into_iter().flatten().flatten()
            .filter_map(|d| serde_json::from_str::<Value>(&d).ok())
            .filter_map(|v| match s(v.get("type")).as_str() { "text" => Some(s(v.get("text"))), "tool" => Some(format!("[tool {}]", s(v.get("tool")))), _ => None }).collect();
        if let Some(t) = turn(&role, &parts.join("\n")) { turns.push(t); }
    }
    turns
}

/// `.antiloki/pty-session.json` — `{engine, session_id, t}`, written by serve when it boots the TUI.
pub fn pty_session(worktree: &Path) -> Option<(String, String)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(worktree.join(".antiloki").join("pty-session.json")).ok()?).ok()?;
    let (e, id) = (s(v.get("engine")), s(v.get("session_id")));
    if id.is_empty() { None } else { Some((e, id)) }
}

pub fn transcript(engine: &str, worktree: &Path) -> (String, Vec<Turn>) {
    match engine {
        "claude-code" => claude_transcript(worktree),
        "codex" => codex_transcript(worktree),
        "opencode" | "opencode-direct" => opencode_transcript(worktree),
        _ => (String::new(), vec![]),
    }
}

/// The artefact. `files`: (path, what) — from the audit's writes and git's changed files. Pure.
pub fn handoff_md(from_engine: &str, to_engine: &str, reason: &str, worktree: &str, branch: &str, scope: &[String], usage: &Value, files: &[(String, String)], turns: &[Turn], now: &str) -> String {
    let mut md = String::new();
    md.push_str(&format!("# Handoff from {from_engine}{}\n\n", if to_engine.is_empty() { String::new() } else { format!(" → {to_engine}") }));
    md.push_str(&format!("- when: {now}\n- why: {}\n- box: `{worktree}`{}\n", if reason.is_empty() { "the operator asked" } else { reason }, if branch.is_empty() { String::new() } else { format!(" · branch `{branch}`") }));
    if !scope.is_empty() { md.push_str(&format!("- scope (the files you may edit): {}\n", scope.join(", "))); }
    let q = quota_state(usage, None);
    if !s(q.get("window")).is_empty() { md.push_str(&format!("- {from_engine}'s {} window: {:.0}% used{}\n", s(q.get("window")), q.get("pct").and_then(Value::as_f64).unwrap_or(0.0), q.get("resets_at").and_then(Value::as_i64).map(|r| format!(" · resets at unix {r}")).unwrap_or_default())); }
    md.push_str("\n## How to continue\n\nYou are taking over this box mid-work. Read the conversation below — the last turns say what was being done and what is left. The files listed were touched in this box; `git diff` shows the exact state. Continue from where it stopped; do not redo what is done, do not undo it. Ask the operator only what the conversation cannot answer.\n");
    if !files.is_empty() {
        md.push_str("\n## Files touched in this box\n\n");
        for (p, what) in files.iter().take(200) { md.push_str(&format!("- `{p}` — {what}\n")); }
    }
    md.push_str(&format!("\n## The conversation ({} turn{})\n\n", turns.len(), if turns.len() == 1 { "" } else { "s" }));
    let keep: Vec<&Turn> = if turns.len() > TURNS_MAX { turns[turns.len() - TURNS_MAX..].iter().collect() } else { turns.iter().collect() };
    if turns.len() > TURNS_MAX { md.push_str(&format!("_(the first {} turns are left out — the last {TURNS_MAX} follow)_\n\n", turns.len() - TURNS_MAX)); }
    let mut body = String::new();
    for t in keep { body.push_str(&format!("### {}\n\n{}\n\n", if t.role == "user" { "operator" } else { from_engine }, t.text)); }
    if body.chars().count() > BODY_MAX { let cut: String = body.chars().rev().take(BODY_MAX).collect::<String>().chars().rev().collect(); md.push_str("_(the conversation is cut to its last 120k characters)_\n\n"); md.push_str(&cut); } else { md.push_str(&body); }
    md
}

/// Where a handoff lands: `.antiloki/handoff/<stamp>-<engine>.md`, and `latest.md` beside it.
pub fn write_handoff(worktree: &Path, from_engine: &str, md: &str, now: &str) -> Result<(String, String), String> {
    let dir = worktree.join(".antiloki").join("handoff");
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let stamp: String = now.chars().filter(|c| c.is_ascii_digit()).take(14).collect();
    let name = format!("{stamp}-{from_engine}.md");
    std::fs::write(dir.join(&name), md).map_err(|e| format!("could not write the handoff: {e}"))?;
    let _ = std::fs::write(dir.join("latest.md"), md);
    Ok((format!(".antiloki/handoff/{name}"), ".antiloki/handoff/latest.md".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_state_grades_the_worst_window_and_the_cli_line_wins() {
        let u = json!({ "limits": [{ "label": "5h", "used_pct": 40.0, "resets_at": 1 }, { "label": "week", "used_pct": 83.5, "resets_at": 2 }] });
        let q = quota_state(&u, None);
        assert_eq!((s(q.get("level")).as_str(), s(q.get("window")).as_str(), q["resets_at"].as_i64()), ("warn", "week", Some(2)));
        assert!(s(q.get("why")).contains("week window is at 84%"));
        assert_eq!(quota_state(&json!({ "limits": [{ "label": "5h", "used_pct": 100.0 }] }), None)["level"], "limit");
        assert_eq!(quota_state(&json!({ "limits": [] }), None)["level"], "ok");
        let hit = quota_state(&u, Some("You've hit your usage limit. Upgrade to Plus"));
        assert_eq!(hit["level"], "limit"); assert!(s(hit.get("why")).starts_with("the CLI said: You've hit"));
        assert_eq!(limit_hit("Error: You've HIT your usage limit."), Some("hit your usage limit"));
        assert_eq!(limit_hit("a rate limit is a concept"), None, "prose about limits is not a hit");
    }
    #[test]
    fn session_uuid_is_v4_shaped_and_stable() {
        let a = session_uuid("pane 9 · 1"); let b = session_uuid("pane 9 · 2");
        assert_eq!(a.len(), 36); assert_eq!(&a[14..15], "4"); assert!(matches!(&a[19..20], "8" | "9" | "a" | "b")); assert_ne!(a, b); assert_eq!(a, session_uuid("pane 9 · 1"));
    }
    #[test]
    fn claude_jsonl_keeps_the_conversation_and_names_the_tools() {
        let text = r#"{"type":"queue-operation","content":"x"}
{"type":"user","message":{"role":"user","content":"make the heap faster"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Looking."},{"type":"tool_use","name":"Read","input":{"file_path":"a.js"}}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"…file…"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Done: siftDown writes once."}]}}
"#;
        let (lines, t) = claude_from_jsonl(text);
        assert_eq!(lines, 5);
        assert_eq!(t.iter().map(|x| (x.role.as_str(), x.text.as_str())).collect::<Vec<_>>(), vec![("user", "make the heap faster"), ("assistant", "Looking.\n[tool Read {\"file_path\":\"a.js\"}]"), ("assistant", "Done: siftDown writes once.")]);
        assert!(headless("You are the ROUTER and PLANNER of a goal run") && !headless("make the heap faster"));
    }
    #[test]
    fn codex_rollout_keeps_messages_and_drops_the_injected_turns() {
        let text = r##"{"type":"session_meta","payload":{"originator":"codex_cli_rs","cwd":"/w"}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>x</environment_context>"}]}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions for /w <INSTRUCTIONS>brief</INSTRUCTIONS>"}]}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"gate it behind the flag"}]}}
{"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"cmd\":\"ls\"}"}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Gated."}]}}
{"type":"event_msg","payload":{"type":"agent_message","message":"Gated."}}
"##;
        let t = codex_from_rollout(text);
        assert_eq!(t.iter().map(|x| (x.role.as_str(), x.text.as_str())).collect::<Vec<_>>(), vec![("user", "gate it behind the flag"), ("assistant", "[tool shell {\"cmd\":\"ls\"}]"), ("assistant", "Gated.")]);
    }
    #[test]
    fn the_handoff_reads_as_a_brief_and_bounds_the_conversation() {
        let turns: Vec<Turn> = (0..100).map(|i| Turn { role: if i % 2 == 0 { "user".into() } else { "assistant".into() }, text: format!("turn {i}") }).collect();
        let u = json!({ "limits": [{ "label": "5h", "used_pct": 100.0, "resets_at": 1789933800 }] });
        let md = handoff_md("claude-code", "codex", "the CLI said: You've hit your limit", "/w/box", "cockpit/2-2", &["packages/scheduler/**".into()], &u, &[("packages/scheduler/src/a.js".into(), "written · 2 edits".into())], &turns, "2026-09-20T17:00:00Z");
        assert!(md.starts_with("# Handoff from claude-code → codex\n"));
        assert!(md.contains("- why: the CLI said: You've hit your limit") && md.contains("branch `cockpit/2-2`") && md.contains("scope (the files you may edit): packages/scheduler/**"));
        assert!(md.contains("claude-code's 5h window: 100% used · resets at unix 1789933800"));
        assert!(md.contains("- `packages/scheduler/src/a.js` — written · 2 edits"));
        assert!(md.contains("## The conversation (100 turns)") && md.contains("the first 20 turns are left out") && !md.contains("turn 19\n") && md.contains("### operator\n\nturn 20") && md.contains("### claude-code\n\nturn 99"));
    }
    #[test]
    fn write_handoff_lands_in_the_box_with_a_latest_alias() {
        let d = std::env::temp_dir().join(format!("antiloki-handoff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let (rel, latest) = write_handoff(&d, "codex", "# h", "2026-09-20T17:01:02.000Z").unwrap();
        assert_eq!(rel, ".antiloki/handoff/20260920170102-codex.md"); assert_eq!(latest, ".antiloki/handoff/latest.md");
        assert_eq!(std::fs::read_to_string(d.join(&rel)).unwrap(), "# h"); assert_eq!(std::fs::read_to_string(d.join(latest)).unwrap(), "# h");
        std::fs::write(d.join(".antiloki/pty-session.json"), r#"{"engine":"claude-code","session_id":"abc"}"#).unwrap();
        assert_eq!(pty_session(&d), Some(("claude-code".into(), "abc".into())));
        let _ = std::fs::remove_dir_all(&d);
    }
}
