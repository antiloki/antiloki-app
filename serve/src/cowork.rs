//! THE COWORK BACKEND (RFC 0007 §5.5 to §5.8): what the terminal screen asks of the serve besides a terminal.
//!
//!   POST /api/v1/cockpit/precheck        the deterministic tier of the pre-check (T0): paths the ask names that are not in the box,
//!                                         turned into the screening questions, with the answers as chips. No model, no quota.
//!   GET  /api/v1/cockpit/pty/usage       one pane's numbers for the strip and the run summary, READ, never injected: the status line's
//!                                         `usage.json`, the hook's `prompts.jsonl` / `answers.jsonl` (a run starts at the last prompt and
//!                                         ends at the next answer), and the engine's own transcript for the run's tokens.
//!   GET/POST /api/v1/cockpit/cowork/layout  the split-pane layout, per workspace.
//!   GET  /api/v1/cockpit/episodes        the sidebar's history: consolidated episodes with their summaries (`episodes_db`; legacy, retired with the Studio chat).
//!
//! Everything here is a pure function over text or a path, tested without a serve; the handlers only find the files.
use axum::{
    body::Bytes,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::dispatch::{engine_call, AppState};

// ── time ───────────────────────────────────────────────────────────────────────────────────────────────────────

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
/// `2026-11-10T00:31:29.941Z` (the hook's and the transcript's own format) → milliseconds since the epoch. UTC only.
pub fn iso_to_ms(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' { return None; }
    let n = |a: usize, z: usize| t.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, s) = (n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 { return None; }
    let mut ms = 0;
    if b[19] == b'.' {
        let frac: String = t[20..].chars().take_while(|c| c.is_ascii_digit()).collect();
        if frac.is_empty() { return None; }
        ms = format!("{frac:0<3}")[..3].parse::<i64>().ok()?;
    }
    Some((days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s) * 1000 + ms)
}

// ── a run: from the last prompt to the next answer ─────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Clone)]
pub struct RunWindow {
    pub start_t: String, pub end_t: Option<String>, pub session: Option<String>, pub prompt: String,
    /// The engine's own session cost when the run started: what the hook saw in `usage.json`, or 0 for the first prompt of a session.
    pub cost_before: Option<f64>,
}

/// The hook's records, parsed, torn lines skipped, and the near-duplicates collapsed. Two hook processes can record the same prompt or answer 60 ms apart: the box's, and an
/// older one that a repo's own `.claude/settings.local.json` still carries (Claude Code in a git worktree loads the main checkout's settings too; found live on `ministore`,
/// 2026-11-10). When one of the pair carries the session cost the hook recorded, that one is kept.
fn jsonl(text: &str) -> std::vec::IntoIter<Value> {
    let body = |v: &Value| v["prompt"].as_str().or_else(|| v["text"].as_str()).map(String::from);
    let near = |a: &Value, b: &Value| matches!((a["t"].as_str().and_then(iso_to_ms), b["t"].as_str().and_then(iso_to_ms)), (Some(x), Some(y)) if (x - y).abs() < 2_000);
    let mut out: Vec<Value> = vec![];
    for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
        let dup = out.last().map_or(false, |p| body(p).is_some() && body(p) == body(&v) && p["session_id"] == v["session_id"] && near(p, &v));
        if !dup { out.push(v); }
        else if v["cost_usd"].is_number() && !out.last().map_or(false, |p| p["cost_usd"].is_number()) { let i = out.len() - 1; out[i] = v; }
    }
    out.into_iter()
}

/// The hook's records for other modules (the sagas read them too): parsed, torn lines skipped, near-duplicates collapsed.
pub(crate) fn jsonl_values(text: &str) -> std::vec::IntoIter<Value> { jsonl(text) }

/// When the engine's OWN transcript says the turn that began at `from_t` is over, because the hook's answer record is missing or torn, or the turn was interrupted (Esc fires no
/// Stop hook): the last assistant message of the turn has `stop_reason: end_turn`, or the person's interruption note follows it, with nothing running after. `None`: still going. Pure.
pub fn turn_ended_in_transcript<'a>(lines: impl Iterator<Item = &'a str>, from_t: &str) -> Option<String> {
    let mut ended: Option<String> = None;
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let ts = v["timestamp"].as_str().unwrap_or("");
        if ts < from_t { continue; }
        match v["type"].as_str() {
            Some("assistant") => { ended = (v["message"]["stop_reason"] == "end_turn").then(|| ts.to_string()); }
            Some("user") => {
                let text = v["message"]["content"].as_str().map(String::from)
                    .or_else(|| v["message"]["content"].as_array().map(|a| a.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join(" ")))
                    .unwrap_or_default();
                if text.contains("[Request interrupted by user") { ended = Some(ts.to_string()); }
            }
            _ => {}
        }
    }
    ended
}

/// How long after a run ends its cost may stay "pending" while the engine's status line catches up.
const COST_PENDING_MS: i64 = 3_000;
/// What the run cost by the engine's own books: its session cost now minus its session cost when the run began. Only once the
/// run has finished and the status line has ticked after the answer (otherwise the number is not final), and never negative
/// (a resumed session starts its meter over). `None` means "price the tokens from the table instead". Pure.
pub fn engine_cost(status: &Value, w: &RunWindow) -> Option<f64> {
    let end = w.end_t.as_deref()?;
    if status["t"].as_str()? < end { return None; }
    let d = status["cost_usd"].as_f64()? - w.cost_before?;
    (d >= 0.0).then_some(d)
}

/// The latest run the hook has seen. It starts at the last line of `prompts.jsonl` and ends at the first line of `answers.jsonl`
/// at or after it; with no such answer the run is still going. ISO strings of one format compare in time order. Pure.
pub fn run_window(prompts: &str, answers: &str) -> Option<RunWindow> {
    let last = jsonl(prompts).filter(|v| v["t"].is_string()).last()?;
    let start_t = last["t"].as_str()?.to_string();
    let end_t = jsonl(answers).filter_map(|v| v["t"].as_str().map(String::from)).find(|t| t.as_str() >= start_t.as_str());
    let session = last["session_id"].as_str().map(String::from);
    let first = session.is_some() && jsonl(prompts).filter(|v| v["session_id"].as_str() == session.as_deref()).count() == 1;
    Some(RunWindow {
        cost_before: last["cost_usd"].as_f64().or(if first { Some(0.0) } else { None }),
        session,
        prompt: last["prompt"].as_str().unwrap_or("").chars().take(120).collect(),
        start_t, end_t,
    })
}

#[derive(Default, Debug, PartialEq, Clone)]
pub struct RunTokens { pub input: i64, pub output: i64, pub cache_read: i64, pub cache_write: i64, pub messages: usize, pub tools: usize, pub model: Option<String> }

/// The tokens a Claude transcript records for assistant messages between two instants (inclusive; `to` of `None` is "up to now").
/// One API message is logged on several lines (one per content block, each carrying the whole message's usage), so usage is taken
/// once per message id, and a tool is counted once per tool-use id. Pure.
/// What the model SAID in a window of the engine's transcript, in order: each assistant text block once (a streamed message repeats), as (timestamp, text). Tool calls and thinking are not text.
pub fn said_in_window<'a>(lines: impl Iterator<Item = &'a str>, from_t: &str, to_t: Option<&str>) -> Vec<(String, String)> {
    let mut seen: HashSet<(String, String)> = HashSet::new(); let mut out = vec![];
    for (n, line) in lines.enumerate() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["type"] != "assistant" { continue; }
        let ts = v["timestamp"].as_str().unwrap_or("");
        if ts < from_t || to_t.map_or(false, |t| ts > t) { continue; }
        let id = v["message"]["id"].as_str().map(String::from).unwrap_or_else(|| format!("line-{n}"));
        for c in v["message"]["content"].as_array().into_iter().flatten() {
            let text = c["text"].as_str().unwrap_or("").trim();
            if c["type"] == "text" && !text.is_empty() && seen.insert((id.clone(), text.to_string())) { out.push((ts.to_string(), text.to_string())); }
        }
    }
    out
}

pub fn tokens_in_window<'a>(lines: impl Iterator<Item = &'a str>, from_t: &str, to_t: Option<&str>) -> RunTokens {
    let mut by_msg: HashMap<String, [i64; 4]> = HashMap::new();
    let mut tools: HashSet<String> = HashSet::new();
    let mut model: Option<String> = None;
    for (n, line) in lines.enumerate() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["type"] != "assistant" { continue; }
        let ts = v["timestamp"].as_str().unwrap_or("");
        if ts < from_t || to_t.map_or(false, |t| ts > t) { continue; }
        let m = &v["message"];
        let id = m["id"].as_str().map(String::from).unwrap_or_else(|| format!("line-{n}"));
        let u = &m["usage"];
        let now = [u["input_tokens"].as_i64().unwrap_or(0), u["output_tokens"].as_i64().unwrap_or(0),
                   u["cache_read_input_tokens"].as_i64().unwrap_or(0), u["cache_creation_input_tokens"].as_i64().unwrap_or(0)];
        let e = by_msg.entry(id).or_insert([0; 4]);
        for i in 0..4 { e[i] = e[i].max(now[i]); }
        if let Some(mm) = m["model"].as_str().filter(|s| !s.is_empty() && !s.starts_with('<')) { model = Some(mm.to_string()); }
        for c in m["content"].as_array().into_iter().flatten() {
            if c["type"] == "tool_use" { tools.insert(c["id"].as_str().map(String::from).unwrap_or_else(|| format!("tool-{n}"))); }
        }
    }
    let mut t = RunTokens { messages: by_msg.len(), tools: tools.len(), model, ..Default::default() };
    for u in by_msg.values() { t.input += u[0]; t.output += u[1]; t.cache_read += u[2]; t.cache_write += u[3]; }
    t
}

// ── what the screen says about a pane ──────────────────────────────────────────────────────────────────────────

/// Whether a terminal's visible screen is waiting on the person (a permission or trust dialog), and the line that says so.
/// Conservative: a dialog's own words, never a guess from silence. A dialog that was answered leaves its words on the screen, so one only
/// counts while NOTHING BELOW IT shows the composer again (the CLI's own "? for shortcuts" / "Ask anything" line): a screen that has gone
/// back to the prompt is not waiting. Only the last rows are read. Pure.
pub fn blocked_from_screen(rows: &[String]) -> Option<String> {
    let tail: Vec<&String> = rows.iter().filter(|r| !r.trim().is_empty()).collect();
    let tail = &tail[tail.len().saturating_sub(12)..];
    let lower: Vec<String> = tail.iter().map(|r| r.to_lowercase()).collect();
    let composer = |r: &str| r.contains("for shortcuts") || r.contains("ask anything") || r.contains("ask codex to do anything") || r.contains("esc to interrupt") || r.contains("ctrl+p commands") || r.contains("plan, search, build anything") || r.contains("add a follow-up");
    let any = |needle: &str| lower.iter().rposition(|r| r.contains(needle));
    // the dialog's own question, in priority order; the footer ("enter to confirm · esc to cancel") is the fallback
    let questions = [any("do you want to proceed"), any("do you want to make this edit"), any("do you want to create"), any("do you trust"),
                     any("is this a project you created or one you trust"), any("press enter to continue")];
    let footer = any("esc to cancel").and(any("enter to confirm")).and_then(|_| any("esc to cancel"));
    // ANTIGRAVITY'S, OPENCODE'S AND CURSOR'S PERMISSION BOXES (2026-11-27, read from the real screens: the chat showed them, the pane still said "running"). Each counts only with its own choices under it, so the same words in an answer do not.
    let boxed = |question: &str, choice: &str| any(question).filter(|at| lower[*at..].iter().any(|r| r.contains(choice)));
    let permission = boxed("allow calling this tool", "yes, allow").or_else(|| boxed("permission required", "allow once")).or_else(|| boxed("run this command?", "(y)")).or_else(|| boxed("run this mcp tool?", "(y)"));
    let shown = questions.iter().flatten().next().copied().or(permission).or(footer)?;
    let latest = questions.iter().flatten().copied().chain(permission).chain(footer).max()?;
    if lower[latest + 1..].iter().any(|r| composer(r)) { return None; }
    Some(tail[shown].trim().trim_start_matches(['\u{2503}', '\u{2502}', '|', '\u{25b3}', ' ']).to_string()).filter(|l| !l.is_empty())
}

// ── the pre-check, tier 0 ──────────────────────────────────────────────────────────────────────────────────────

/// The router's job keys are internal ("editor-s"); the card says what they mean.
fn job_words(key: &str) -> &str {
    match key {
        "lookup" => "a lookup", "synthesis" => "a summary", "screening" => "a screening", "planner" => "a plan",
        "editor-s" => "a small change", "editor-l" => "a large change", "verifier" => "a check",
        "test-writer" => "writing tests", "qa-gate" => "a QA gate", "report" => "a report", other => other,
    }
}

/// The verdict for an ask, from facts only: a path it names that is not in the box becomes a screening question with the answers
/// as chips (the first is the default). With nothing to point at, it is "send" and says what it looked at. `exists` is injected so
/// this is pure. Same wire shape as the front end's `parseVerdict`.
pub fn precheck_verdict(text: &str, exists: impl Fn(&str) -> bool) -> Value {
    let t = text.trim();
    let mut notes: Vec<String> = vec![];
    let job = crate::cli::classify_ask(t);
    notes.push(format!("reads as: {}", job_words(job.key())));
    let doubts = crate::screen::doubts(t, &exists);
    if doubts.is_empty() {
        notes.push("no path in the ask is missing from the box".to_string());
        return json!({ "verdict": "send", "tier": 0, "notes": notes });
    }
    let questions: Vec<Value> = doubts.iter().enumerate().map(|(i, d)| json!({
        "id": format!("{}-{}", d.kind, i), "text": d.text, "options": d.options, "default": d.default, "evidence": d.evidence })).collect();
    json!({ "verdict": "ask", "tier": 0, "notes": notes, "questions": questions })
}

// ── usage for the strip and the summary ────────────────────────────────────────────────────────────────────────

fn read(p: &Path) -> String { std::fs::read_to_string(p).unwrap_or_default() }
/// The folder names Cursor may use for a project path: every non-alphanumeric character becomes "-" (Linux: `/home/me/x` is `home-me-x`), runs of them may be one "-", and on Windows
/// the drive letter is lower case (`C:\Users\Me\x` is `c-Users-Me-x`). A `\\?\` prefix is dropped. Found 2026-11-25: a friend's Windows chat for Cursor stayed empty.
pub(crate) fn cursor_slugs(cwd: &str) -> Vec<String> {
    let c = cwd.trim_start_matches(r"\\?\").trim_end_matches(['/', '\\']);
    let each: String = c.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' }).collect();
    let mut runs = String::new(); for ch in each.chars() { if ch == '-' && runs.ends_with('-') { continue; } runs.push(ch); }
    let drive = c.chars().nth(1) == Some(':') && c.chars().next().map_or(false, |d| d.is_ascii_alphabetic());
    let lower_first = |t: &str| { let mut it = t.chars(); match it.next() { Some(f) => f.to_ascii_lowercase().to_string() + it.as_str(), None => String::new() } };
    let mut out: Vec<String> = vec![each.trim_matches('-').to_string(), runs.trim_matches('-').to_string()];
    if drive { out.push(lower_first(each.trim_matches('-'))); out.push(lower_first(runs.trim_matches('-'))); }
    let mut seen = std::collections::HashSet::new(); out.retain(|x| !x.is_empty() && seen.insert(x.clone())); out
}
fn alnum_lower(s: &str) -> String { s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect() }
/// `~/.cursor/projects/<this project>`: the exact names first, then any folder whose letters and digits are the same (whatever Cursor did with case and separators).
pub(crate) fn cursor_project_dir(projects: &Path, cwd: &str) -> Option<PathBuf> {
    for slug in cursor_slugs(cwd) { let d = projects.join(&slug); if d.is_dir() { return Some(d); } }
    let want = alnum_lower(cwd); if want.is_empty() { return None; }
    std::fs::read_dir(projects).ok()?.flatten().find(|e| alnum_lower(&e.file_name().to_string_lossy()) == want).map(|e| e.path())
}
/// The newest Cursor transcript of this worktree: `~/.cursor/projects/<project>/agent-transcripts/<id>/<id>.jsonl`.
pub(crate) fn cursor_transcript_for(cwd: &str) -> Option<PathBuf> { cursor_transcript_in(cwd, crate::shared_folder::ALL) }
fn cursor_transcript_in(cwd: &str, w: crate::shared_folder::Window) -> Option<PathBuf> {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok()?;
    let dir = cursor_project_dir(&Path::new(&home).join(".cursor").join("projects"), cwd)?.join("agent-transcripts");
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in std::fs::read_dir(&dir).ok()?.flatten() {
        let Ok(rd) = std::fs::read_dir(e.path()) else { continue };
        for f in rd.flatten() { let p = f.path(); if p.extension().map_or(false, |x| x == "jsonl") && crate::shared_folder::holds(w, crate::shared_folder::born(&p)) { if let Ok(t) = f.metadata().and_then(|m| m.modified()) { if best.as_ref().map_or(true, |b| t > b.0) { best = Some((t, p)); } } } }
    }
    best.map(|b| b.1)
}
/// OpenCode keeps its sessions in its own database: the newest session whose directory is this worktree, as turns.
fn opencode_turns(root: &str, cap: usize) -> Vec<Value> { opencode_turns_in(root, cap, crate::shared_folder::ALL) }
fn opencode_turns_in(root: &str, cap: usize, w: crate::shared_folder::Window) -> Vec<Value> {
    let Some(path) = antiloki_rs::cockpit__helpers_engine_usage::opencode_db() else { return vec![] };
    let Ok(db) = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX) else { return vec![] };
    let wt = std::fs::canonicalize(root).unwrap_or_else(|_| PathBuf::from(root));
    // OpenCode 2 keeps its conversations in new tables (found 2026-11-26: the chat waited forever, its reader looked in the old ones)
    if let Some(turns) = opencode_v2_turns(&db, &wt, cap, w) { return turns; }
    let Ok(mut st) = db.prepare(&opencode_sessions_sql("session", w)) else { return vec![] };
    let sid = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))).ok().into_iter().flatten().flatten()
        .find(|(_, d, t)| crate::shared_folder::holds(w, *t as u64) && (antiloki_rs::cockpit__helpers_engine_usage::same_dir(d, &wt) || Path::new(d) == wt)).map(|x| x.0);
    let Some(sid) = sid else { return vec![] };
    let Ok(mut ms) = db.prepare("SELECT id, data FROM (SELECT id, data, time_created FROM message WHERE session_id = ?1 ORDER BY time_created DESC, id DESC LIMIT 400) ORDER BY time_created, id") else { return vec![] };
    let rows: Vec<(String, String)> = ms.query_map([&sid], |r| Ok((r.get(0)?, r.get(1)?))).ok().into_iter().flatten().flatten().collect();
    let msgs: Vec<(Value, Vec<Value>)> = rows.into_iter().map(|(id, d)| {
        let parts = db.prepare("SELECT data FROM part WHERE message_id = ?1 ORDER BY id").ok().map(|mut p| p.query_map([&id], |r| r.get::<_, String>(0)).ok().into_iter().flatten().flatten().filter_map(|x| serde_json::from_str::<Value>(&x).ok()).collect::<Vec<_>>()).unwrap_or_default();
        (serde_json::from_str::<Value>(&d).unwrap_or(Value::Null), parts)
    }).collect();
    crate::cowork_transcript::turns_from_opencode(&msgs, cap)
}
/// The newest OpenCode 2 conversation of this folder; None when this database has no such tables or no session for it.
/// The sessions newest first; the start time is only asked for where it matters.
fn opencode_sessions_sql(table: &str, w: crate::shared_folder::Window) -> String { format!("SELECT id, directory, {} FROM {table} ORDER BY time_updated DESC", if w == crate::shared_folder::ALL { "0" } else { "time_created" }) }
fn opencode_v2_turns(db: &rusqlite::Connection, wt: &Path, cap: usize, w: crate::shared_folder::Window) -> Option<Vec<Value>> {
    let mut st = db.prepare(&opencode_sessions_sql("session_v2", w)).ok()?;
    let sid = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))).ok()?.flatten().find(|(_, d, t)| crate::shared_folder::holds(w, *t as u64) && (antiloki_rs::cockpit__helpers_engine_usage::same_dir(d, wt) || Path::new(d) == wt))?.0;
    let mut ms = db.prepare("SELECT id, type, data FROM (SELECT id, type, data, seq FROM session_message WHERE session_id = ?1 AND type IN ('user','assistant') ORDER BY seq DESC LIMIT 400) ORDER BY seq").ok()?;
    let rows: Vec<(String, String, String)> = ms.query_map([&sid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).ok()?.flatten().collect();
    let mut part = db.prepare("SELECT data FROM session_part WHERE message_id = ?1 ORDER BY ordinal").ok();
    let msgs: Vec<(Value, Vec<Value>)> = rows.into_iter().map(|(id, kind, d)| {
        let parts = part.as_mut().and_then(|p| p.query_map([&id], |r| r.get::<_, String>(0)).ok().map(|it| it.flatten().filter_map(|x| serde_json::from_str::<Value>(&x).ok()).collect::<Vec<_>>())).unwrap_or_default();
        crate::cowork_transcript::opencode_v2_message(&kind, &serde_json::from_str::<Value>(&d).unwrap_or(Value::Null), parts)
    }).collect();
    Some(crate::cowork_transcript::turns_from_opencode(&msgs, cap))
}
/// The newest Codex rollout (`~/.codex/sessions/**/rollout-*.jsonl`) whose `session_meta.cwd` is `cwd`. Only the first line of each file is read to decide. Blocking.
pub(crate) fn codex_rollout_for(cwd: &str) -> Option<PathBuf> { codex_rollout_in(cwd, crate::shared_folder::ALL) }
/// The same, among the conversations begun inside `w` (see `shared_folder`).
fn codex_rollout_in(cwd: &str, w: crate::shared_folder::Window) -> Option<PathBuf> {
    use std::io::{BufRead, BufReader};
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok()?;
    let want = cwd.trim_end_matches(['/', '\\']);
    let mut stack = vec![Path::new(&home).join(".codex").join("sessions")];
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { stack.push(p); continue; }
            if !p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("rollout-") && n.ends_with(".jsonl")) { continue; }
            let Ok(f) = std::fs::File::open(&p) else { continue };
            let Some(Ok(first)) = BufReader::new(f).lines().next() else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&first) else { continue };
            if v["payload"]["cwd"].as_str().map(|c| c.trim_end_matches(['/', '\\'])) != Some(want) || !crate::shared_folder::holds(w, crate::shared_folder::born(&p)) { continue; }
            let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().map_or(true, |(bt, _)| t > *bt) { best = Some((t, p)); }
        }
    }
    best.map(|(_, p)| p)
}

/// The id of the newest conversation an engine began in `folder` inside `w`, as its own CLI takes it back. Claude Code is named by antiloki at launch and is not looked for here. Blocking.
pub(crate) fn conversation_id(engine: &str, folder: &str, w: crate::shared_folder::Window) -> Option<String> {
    if w == crate::shared_folder::NONE { return None; }
    match engine {
        "codex" => codex_rollout_in(folder, w).and_then(|p| codex_id(&p)),
        "cursor" => cursor_transcript_in(folder, w).and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned())),
        "gemini" => agy_transcript_within(folder, w).and_then(|p| agy_id(&p)),
        "opencode" => {
            let db = rusqlite::Connection::open_with_flags(antiloki_rs::cockpit__helpers_engine_usage::opencode_db()?, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
            let wt = std::fs::canonicalize(folder).unwrap_or_else(|_| PathBuf::from(folder));
            ["session_v2", "session"].iter().find_map(|table| opencode_session_in(&db, table, &wt, w))
        }
        _ => None,
    }.filter(|id| !id.is_empty() && id.len() <= 80 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && !id.starts_with('-'))
}
fn opencode_session_in(db: &rusqlite::Connection, table: &str, wt: &Path, w: crate::shared_folder::Window) -> Option<String> {
    let mut st = db.prepare(&opencode_sessions_sql(table, w)).ok()?;
    let found = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))).ok()?.flatten().find(|(_, d, t)| crate::shared_folder::holds(w, *t as u64) && (antiloki_rs::cockpit__helpers_engine_usage::same_dir(d, wt) || Path::new(d) == wt)).map(|x| x.0); found
}
/// A Codex rollout names its session in its first line. Pure over the file.
fn codex_id(p: &Path) -> Option<String> { use std::io::{BufRead, BufReader}; let first = BufReader::new(std::fs::File::open(p).ok()?).lines().next()?.ok()?; let v: Value = serde_json::from_str(&first).ok()?; v["payload"]["id"].as_str().or_else(|| v["payload"]["session_id"].as_str()).map(String::from) }
/// Antigravity keeps a conversation under `brain/<id>/.system_generated/logs/`. Pure.
fn agy_id(p: &Path) -> Option<String> { p.ancestors().nth(3).and_then(|d| d.file_name()).map(|n| n.to_string_lossy().into_owned()) }

pub(crate) fn claude_transcript(cwd: &str, session: &str) -> Option<PathBuf> {
    if session.is_empty() || !session.chars().all(|c| c.is_ascii_hexdigit() || c == '-') { return None; }
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok()?;
    let p = Path::new(&home).join(".claude").join("projects").join(antiloki_rs::context_context_library::claude_project_slug(cwd)).join(format!("{session}.jsonl"));
    p.is_file().then_some(p)
}

/// One pane's numbers. `root` is the pane's worktree. Claude Code is read here (its status line, hooks and transcript); Codex and
/// OpenCode in `cowork_engines`; every other engine answers `known: false` with the reason, so the strip shows `—`, never a guess.
/// The line under Cursor Agent's box: the model and, after the first answer, the share of the context used ("Grok 4.7 256K Medium · 7.6%"). Cursor keeps no token counts on this machine. Pure.
pub fn cursor_footer(rows: &[String]) -> Option<(String, Option<f64>)> {
    let lines: Vec<&str> = rows.iter().map(|r| r.trim()).filter(|r| !r.is_empty()).collect();
    // the lower edge of the input box; newer versions draw none, and the box is known by its arrow with the folder's line two under it
    let boxed = lines.iter().rposition(|l| l.starts_with('\u{2580}')).or_else(|| {
        let path = lines.iter().rposition(|l| (l.starts_with('~') || l.starts_with('/')) && l.contains(" \u{b7} "))?;
        let arrow = lines[..path].iter().rposition(|l| l.starts_with('\u{2192}'))?;
        (path - arrow <= 3 && path - arrow >= 2).then_some(arrow)
    })?;
    for l in lines.iter().skip(boxed + 1).take(3) {
        if l.starts_with('~') || l.starts_with('/') || l.contains("shift+tab") { continue; }
        let (model, pct) = match l.rsplit_once(" \u{b7} ") { Some((m, p)) if p.ends_with('%') => (m.trim(), p.trim_end_matches('%').parse::<f64>().ok()), _ => (*l, None) };
        if !model.is_empty() && model.len() < 60 { return Some((model.to_string(), pct)); }
    }
    None
}
pub fn usage_for(root: &Path, engine_key: &str, screen_rows: Option<&[String]>) -> Value { usage_in(root, &root.join(".antiloki"), engine_key, screen_rows) }
/// The same for one pane: an agent that shares its folder has its records in a folder of its own.
pub fn usage_for_pane(root: &Path, pane_id: i64, engine_key: &str, screen_rows: Option<&[String]>) -> Value { usage_in(root, &crate::shared_folder::records_of(&root.to_string_lossy(), pane_id), engine_key, screen_rows) }
fn usage_in(root: &Path, records: &Path, engine_key: &str, screen_rows: Option<&[String]>) -> Value {
    let blocked = screen_rows.and_then(blocked_from_screen);
    if engine_key == "cursor" {
        let shown = screen_rows.and_then(cursor_footer);
        let Some((model, ctx)) = shown else { return json!({ "ok": true, "known": false, "engine": engine_key, "reason": "Cursor's box is not on the screen", "blocked": blocked }) };
        let working = screen_rows.is_some_and(|r| r.iter().rev().take(12).any(|l| l.contains("ctrl+c to stop")));
        return json!({ "ok": true, "known": true, "engine": engine_key, "working": working, "blocked": blocked, "model": model, "context": ctx.map_or(Value::Null, |p| json!({ "used_pct": p })), "limits": crate::cursor_usage::rows_cached(), "session_cost": Value::Null, "lines_added": Value::Null, "lines_removed": Value::Null, "run": Value::Null });
    }
    if engine_key != "claude-code" {
        return crate::cowork_engines::usage_for_other(root, engine_key, blocked, crate::cli::now_ms());
    }
    let dir = records.to_path_buf();
    let status: Value = serde_json::from_str(&read(&dir.join("usage.json"))).unwrap_or(Value::Null);
    let mut window = run_window(&read(&dir.join("prompts.jsonl")), &read(&dir.join("answers.jsonl")));
    let pty_session: Option<String> = serde_json::from_str::<Value>(&read(&dir.join("pty-session.json"))).ok().and_then(|v| v["session_id"].as_str().map(String::from));
    let session = window.as_ref().and_then(|w| w.session.clone()).or(pty_session);
    let file = session.as_deref().and_then(|s| claude_transcript(&root.to_string_lossy(), s));
    // A RUN THE HOOK NEVER SAW END (its answer line torn by two writers, or Esc, which fires no Stop hook) is over when the engine's own transcript says so; without this the
    // pane read "working" for ever (found live 2026-11-10)
    if let Some(w) = window.as_mut() { if w.end_t.is_none() { let start = w.start_t.clone(); if let Some(e) = file.as_ref().and_then(|p| parsed(&format!("ended|{start}"), p, |t| turn_ended_in_transcript(t.lines(), &start))) { w.end_t = Some(e); } } }
    let mut run = Value::Null;
    if let Some(w) = &window {
        let t = match &file { Some(p) => parsed(&format!("tokens|{}|{:?}", w.start_t, w.end_t), p, |t| tokens_in_window(t.lines(), &w.start_t, w.end_t.as_deref())), None => tokens_in_window("".lines(), &w.start_t, w.end_t.as_deref()) };
        let model = t.model.clone().or_else(|| status["model"].as_str().map(String::from));
        let eng = crate::cli::Engine::ClaudeCode;
        let (table, (baseline, against)) = (
            crate::cli::price_tokens(eng, model.as_deref(), t.input, t.output, t.cache_read, t.cache_write),
            crate::cli::frontier_price(eng, t.input, t.output, t.cache_read, t.cache_write));
        // The engine's own accounting beats the price list, which lags new models (Haiku 5.5 read 6× high on 2026-11-10).
        // A run that just ended has no final cost until the status line ticks (about 300 ms after the answer): that moment is "pending", never the list price
        // flashing up and then jumping to the real one.
        let just_ended = w.end_t.as_deref().and_then(iso_to_ms).map_or(false, |e| crate::cli::now_ms() - e < COST_PENDING_MS);
        let (cost, cost_source): (Value, &str) = match engine_cost(&status, w) {
            Some(c) => (json!(c), "engine"),
            None if just_ended && w.cost_before.is_some() && status["cost_usd"].is_number() => (Value::Null, "pending"),
            None => (json!(table), "table"),
        };
        let ms = w.end_t.as_deref().and_then(iso_to_ms).zip(iso_to_ms(&w.start_t)).map(|(e, s)| (e - s).max(0));
        run = json!({
            "started_t": w.start_t, "finished_t": w.end_t, "ms": ms, "prompt": w.prompt,
            "tokens_in": t.input + t.cache_read + t.cache_write, "tokens_out": t.output, "raw": { "input": t.input, "cache_read": t.cache_read, "cache_write": t.cache_write },
            "turns": t.messages, "tools": t.tools, "model": model, "cost": cost, "cost_source": cost_source, "baseline": baseline, "against": against,
            "transcript": file.as_ref().is_some_and(|p| p.metadata().is_ok_and(|m| m.len() > 0)),
        });
    }
    json!({
        "ok": true, "known": true, "engine": "claude-code",
        "working": window.as_ref().map_or(false, |w| w.end_t.is_none()),
        "blocked": blocked,
        "model": claude_model(&status["model"], run["model"].as_str(), screen_rows), "context": status["context"], "limits": status["limits"], "session_cost": status["cost_usd"],
        "lines_added": status["lines_added"], "lines_removed": status["lines_removed"],
        "run": run,
    })
}

/// THE MODEL A CLAUDE PANE IS ON, EVEN BEFORE ITS FIRST ANSWER (2026-11-27: a new agent in a project without Git showed "Claude Code" and no model): what the status line said; else what the last run's log says; else the start-up
/// banner still on the screen. Null when none of them says. Pure.
fn claude_model(status: &Value, last_run: Option<&str>, rows: Option<&[String]>) -> Value {
    if status.as_str().is_some_and(|s| !s.is_empty()) { return status.clone(); }
    last_run.and_then(plain_claude_model).or_else(|| rows.and_then(model_from_banner)).map_or(Value::Null, Value::String)
}
const CLAUDE_FAMILIES: [&str; 4] = ["Fable", "Opus", "Sonnet", "Haiku"];
/// `claude-haiku-5-5-20260101` reads "Haiku 5.5". Pure.
fn plain_claude_model(id: &str) -> Option<String> {
    let parts: Vec<&str> = id.split(['-', '[']).collect();
    let at = parts.iter().position(|p| CLAUDE_FAMILIES.iter().any(|f| f.eq_ignore_ascii_case(p)))?;
    let family = CLAUDE_FAMILIES.iter().find(|f| f.eq_ignore_ascii_case(parts[at]))?;
    let version: Vec<&str> = parts[at + 1..].iter().take_while(|p| !p.is_empty() && p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit())).copied().collect();
    Some(if version.is_empty() { family.to_string() } else { format!("{family} {}", version.join(".")) })
}
/// The Claude model of the newest answer in a conversation. Pure.
fn last_model(turns: &Value) -> Option<String> { turns.as_array()?.iter().rev().find_map(|t| t["model"].as_str().and_then(plain_claude_model)) }
/// Claude Code's banner names the model on a line just under "Claude Code v…". Only there: a model named in the conversation is not the pane's model. Pure.
fn model_from_banner(rows: &[String]) -> Option<String> {
    let at = rows.iter().position(|l| l.replace(' ', "").contains("ClaudeCodev"))?;
    rows.iter().skip(at + 1).take(2).find_map(|l| {
        let words: Vec<&str> = l.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.')).filter(|w| !w.is_empty()).collect();   // the logo's block characters can touch the name
        let i = words.iter().position(|w| CLAUDE_FAMILIES.contains(w))?;
        let version = words.get(i + 1).filter(|v| v.starts_with(|c: char| c.is_ascii_digit()) && v.chars().all(|c| c.is_ascii_digit() || c == '.'));
        Some(version.map_or_else(|| words[i].to_string(), |v| format!("{} {v}", words[i])))
    })
}

// ── closing an episode is gone (operator 2026-11-11, RFC 0008 §4.10) ─────────────────────────────────────────────

/// `POST /api/v1/cockpit/pty/consolidate`: the button that closed an agent's stretch of work as a saga (with a model's title and summary) is retired: a saga is made by itself as an agent works, an episode for every
/// ask (`work_boxes.rs`), so there is nothing to close. The door answers an older screen with the reason, instead of a 404.
pub async fn consolidate_post(_body: Bytes) -> Response {
    reply(StatusCode::OK, json!({ "ok": false, "gone": true, "error": "closing an episode is gone: an agent's work becomes episodes of its own saga by itself" }))
}

// ── handlers ───────────────────────────────────────────────────────────────────────────────────────────────────

fn json_body(b: &Bytes) -> Value { serde_json::from_slice(b).unwrap_or_else(|_| json!({})) }
fn num(v: &Value) -> i64 { v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).unwrap_or(0) }
fn reply(status: StatusCode, v: Value) -> Response { (status, Json(v)).into_response() }

/// The pane's worktree and engine key, or the workspace root for a pane that has none.
async fn pane_root(state: &Arc<AppState>, pane_id: i64) -> (String, String) { let (r, k, _) = pane_facts(state, pane_id).await; (r, k) }
/// The pane's box, its engine, and whether the box is the CHAT SANDBOX (below).
pub(crate) async fn pane_facts(state: &Arc<AppState>, pane_id: i64) -> (String, String, bool) {
    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
    let pane = engine_call(state, "system.cockpit.pane.get", json!({ "id": pane_id }), ctx).await.map(|(v, _)| v).unwrap_or(Value::Null);
    let sandbox = is_sandbox(&pane);
    let root = if pane["worktree_path"].as_str().unwrap_or("").is_empty() { crate::shared_folder::folder_of(&crate::agents_http::active_root(state).await, &pane) } else { crate::shared_folder::folder_of("", &pane) };
    (root, pane["pane_kind"].as_str().unwrap_or("").to_string(), sandbox)
}
/// Is this pane's box the chat sandbox: an empty directory with the brief, MIN mode (reads outside the pane's scope refused), edits and shell denied for OpenCode, the repo reachable
/// only through antiloki's tools? That is right for the conversational chat and wrong for a terminal, where it reads as "Claude cannot look at the project" (operator 2026-11-10: a pane
/// made before terminal panes got real worktrees kept saying so). Pure over the pane row.
pub fn is_sandbox(pane: &Value) -> bool {
    let settings: Value = pane["settings_json"].as_str().and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null);
    settings["chat"] == true || pane["worktree_path"].as_str().map_or(false, |p| p.contains("/chats/"))
}

/// THE FRONTIER PROVIDERS SCREEN THEIR OWN WORK (operator 2026-11-11: "each frontier model provider has its own screening tool ... we don't need Heimdall for that; we will use it on our local models"). Claude Code, Codex, Gemini, Grok, Cursor and
/// DeepSeek already read an ask, plan it and ask back when it is unclear, so antiloki's pre-check and its router stay out of their way. OpenCode (which carries the open and local models) and anything unknown keep Heimdall. A pane's own
/// `settings.heimdall` (true/false) overrides either way. Pure - tested.
pub fn heimdall_wanted(pane_kind: &str, settings: &Value) -> bool {
    if let Some(b) = settings.get("heimdall").and_then(Value::as_bool) { return b; }
    !matches!(pane_kind, "claude-code" | "codex" | "gemini" | "grok" | "cursor" | "deepseek")
}

/// `POST /api/v1/cockpit/precheck {pane_id, text}`
pub async fn precheck(state: Arc<AppState>, body: Bytes) -> Response {
    let b = json_body(&body);
    let text = b["text"].as_str().unwrap_or("").to_string();
    if text.trim().is_empty() { return reply(StatusCode::OK, json!({ "verdict": "send", "tier": 0, "notes": [] })); }
    // asked by the ENGINE'S OWN PROMPT HOOK (anything typed straight into the terminal reaches it too): the composer's check already stands for what it sent
    let from_hook = b["source"] == "hook";
    // THE PLANNER OF A TEAM is told what it is alongside whatever the person types to it (the hook adds it to the prompt as context)
    let team_note = if from_hook { let root = crate::agents_http::active_root(&state).await; crate::team_run::planner_context(&crate::team_run::load_teams(&root), num(&b["pane_id"]), &text) } else { None };
    // a frontier provider's own screening is enough: nothing of Heimdall's runs for it (the team's note, which is orchestration and not screening, still rides along)
    if let Some(p) = engine_call(&state, "system.cockpit.pane.get", json!({ "id": num(&b["pane_id"]) }), json!({ "actor": "web-fe", "source": "web-fe" })).await.ok().map(|(v, _)| v).filter(|v| !v.is_null()) {
        let settings: Value = serde_json::from_str(p["settings_json"].as_str().unwrap_or("{}")).unwrap_or(Value::Null);
        if !heimdall_wanted(p["pane_kind"].as_str().unwrap_or(""), &settings) {
            let mut v = json!({ "verdict": "send", "tier": 0, "notes": ["this provider screens its own work: Heimdall stays out of it"], "by": "provider" }); if let Some(n) = &team_note { v["team"] = json!(n); }
            return reply(StatusCode::OK, v);
        }
    }
    if from_hook && checked_by_composer(sent_for(num(&b["pane_id"])), &text, crate::cli::now_ms()) {
        let mut v = json!({ "verdict": "send", "tier": 0, "notes": ["the composer already checked this ask"], "by": "composer" }); if let Some(n) = &team_note { v["team"] = json!(n); }
        return reply(StatusCode::OK, v);
    }
    let (root, _) = pane_root(&state, num(&b["pane_id"])).await;
    let t0 = std::time::Instant::now();
    let mut v = tokio::task::spawn_blocking(move || {
        let root = PathBuf::from(root);
        precheck_verdict(&text, |p| !p.contains("..") && root.join(p.trim_start_matches('/')).exists())
    }).await.unwrap_or_else(|_| json!({ "verdict": "skipped", "reason": "the pre-check failed to run", "notes": [] }));
    v["ms"] = json!(t0.elapsed().as_millis() as u64);
    if let Some(n) = &team_note { v["team"] = json!(n); }
    // what the router read this ask as, for the pane's header (the person need not have used the composer for it to show)
    if from_hook { if let Some(label) = v["notes"].as_array().into_iter().flatten().filter_map(Value::as_str).find_map(|n| n.strip_prefix("reads as: ")) { note_route(num(&b["pane_id"]), label); } }
    // A QUESTION WITH A SCREEN OPEN TO SHOW IT is parked for the person; the hook waits for the answer (`asks/wait`). With no screen watching it gets the question at once, as context.
    if from_hook && v["verdict"] == "ask" && crate::cowork_asks::with(|a| a.screen_watching(crate::cli::now_ms())) {
        let id = crate::cowork_asks::with(|a| a.register(num(&b["pane_id"]), &b["text"].as_str().unwrap_or(""), &v["questions"], &v["notes"], crate::cli::now_ms()));
        v["ask_id"] = json!(id); v["waiting"] = json!(true);
    }
    reply(StatusCode::OK, v)
}

// ── the questions, asked of the person (cowork_asks.rs) ────────────────────────────────────────────────────────

/// `GET /api/v1/cockpit/cowork/asks` - what the screen should ask the person now; asking is also how the serve knows a screen is open.
pub async fn asks_get() -> Response { reply(StatusCode::OK, json!({ "ok": true, "asks": crate::cowork_asks::with(|a| a.open(crate::cli::now_ms())) })) }
/// `POST /api/v1/cockpit/cowork/asks/answer {id, answers | skip}`
pub async fn asks_answer(body: Bytes) -> Response {
    let b = json_body(&body);
    match crate::cowork_asks::with(|a| a.answer(num(&b["id"]) as u64, &b)) {
        Ok(()) => reply(StatusCode::OK, json!({ "ok": true })),
        Err(e) => reply(StatusCode::NOT_FOUND, json!({ "ok": false, "error": e })),
    }
}
/// `POST /api/v1/cockpit/cowork/asks/wait {id}` - the hook's long poll: the person's answer as a verdict, or, when nobody answered in time, the question left for the model to ask.
pub async fn asks_wait(body: Bytes) -> Response {
    let b = json_body(&body); let id = num(&b["id"]) as u64;
    let waited = std::time::Instant::now();
    let limit = std::env::var("ANTILOKI_ASK_WAIT_MS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(crate::cowork_asks::WAIT_MS as u64);
    loop {
        if let Some(v) = crate::cowork_asks::with(|a| a.take_answer(id)) { return reply(StatusCode::OK, v); }
        if !crate::cowork_asks::with(|a| a.exists(id)) { return reply(StatusCode::OK, json!({ "verdict": "send", "tier": 0, "notes": ["the question is gone"], "by": "none" })); }
        if waited.elapsed().as_millis() as u64 >= limit { let v = crate::cowork_asks::with(|a| a.expire(id)).unwrap_or_else(|| json!({ "verdict": "send", "tier": 0, "notes": ["the question is gone"] })); return reply(StatusCode::OK, v); }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
}

/// What the router read each pane's last ask as ("a small change"), noted when the pane's own prompt hook asked.
static ROUTES: std::sync::OnceLock<std::sync::Mutex<HashMap<i64, (i64, String)>>> = std::sync::OnceLock::new();
fn note_route(pane_id: i64, label: &str) { if let Ok(mut m) = ROUTES.get_or_init(Default::default).lock() { m.insert(pane_id, (crate::cli::now_ms(), label.to_string())); } }
/// The label a pane's header shows while it is still about the same ask (ten minutes).
fn route_for(pane_id: i64) -> Option<String> { let (t, l) = ROUTES.get()?.lock().ok()?.get(&pane_id).cloned()?; (crate::cli::now_ms() - t < 600_000).then_some(l) }

/// WHEN THE SERVE LAST PASTED A PROMPT INTO A PANE, AND WHAT. Codex writes nothing until its first model reply (about 10 s on a new pane) and a
/// Claude hook can lag the keystroke, so for a moment after a send the engine's own record still shows the run before. The serve knows
/// the prompt went in; this lets the pane read as working in that gap instead of idle. The text's hash lets the engine's own prompt hook
/// (which asks the pre-check again for anything typed straight into the terminal) see that the composer already checked THIS ask.
static SENT: std::sync::OnceLock<std::sync::Mutex<HashMap<i64, (i64, u64)>>> = std::sync::OnceLock::new();
pub fn text_hash(text: &str) -> u64 { use std::hash::{Hash, Hasher}; let mut h = std::collections::hash_map::DefaultHasher::new(); text.trim().hash(&mut h); h.finish() }
/// THE QUESTION A PANE'S CLI IS ASKING RIGHT NOW (set by the stream reader when it sees one, cleared when it is answered).
static DIALOGS: std::sync::OnceLock<std::sync::Mutex<HashMap<i64, String>>> = std::sync::OnceLock::new();
pub fn note_dialog(pane_id: i64, question: Option<String>) { if let Ok(mut m) = DIALOGS.get_or_init(Default::default).lock() { match question { Some(q) => { m.insert(pane_id, q); } None => { m.remove(&pane_id); } } } }
pub fn dialog_for(pane_id: i64) -> Option<String> { DIALOGS.get()?.lock().ok()?.get(&pane_id).cloned() }
/// THE LAST THING A PANE'S CLI SAID IN ANSWER TO A SETTING COMMAND ("Set model to Sonnet 5.5 and saved as your default for new sessions"), with the time: the chat shows it, as the terminal does, for a short while.
static NOTICES: std::sync::OnceLock<std::sync::Mutex<HashMap<i64, (i64, String)>>> = std::sync::OnceLock::new();
const NOTICE_MS: i64 = 30_000;
/// When a setting command was last SENT to a pane: a resumed session redraws its old "Set model to ..." lines, and those are history, not an answer. Only a line that follows a send counts.
static SETTING_SENT: std::sync::OnceLock<std::sync::Mutex<HashMap<i64, (i64, String)>>> = std::sync::OnceLock::new();
const ANSWER_WITHIN_MS: i64 = 20_000;
/// The word after the command (`/model sonnet` -> `sonnet`), lower case: the answer to it names that model, and the redraw of old history that follows a command does not. Pure.
pub fn setting_arg(text: &str) -> String { text.trim().split_whitespace().nth(1).unwrap_or("").to_lowercase() }
fn note_setting_sent(pane_id: i64, text: &str) { if let Ok(mut m) = SETTING_SENT.get_or_init(Default::default).lock() { m.insert(pane_id, (crate::cli::now_ms(), setting_arg(text))); } }
fn setting_sent_at(pane_id: i64) -> Option<i64> { SETTING_SENT.get()?.lock().ok()?.get(&pane_id).map(|(t, _)| *t) }
fn setting_arg_for(pane_id: i64) -> String { SETTING_SENT.get().and_then(|m| m.lock().ok().and_then(|m| m.get(&pane_id).map(|(_, a)| a.clone()))).unwrap_or_default() }
/// Is this line the answer to the command that was sent: a "no" (`Kept model as ...`) always is; a "yes" (`Set model to ...`) must name the model that was asked for (when one was). Pure - tested.
pub fn answers_command(line: &str, arg: &str) -> bool { line.starts_with("Kept model as") || arg.is_empty() || line.to_lowercase().contains(arg) }
/// Is an answer to a setting command still to come (one was sent a moment ago and none has been noted since)? The stream reader asks this before it looks at the screen text. Pure over the clock and the two maps.
pub fn expecting_answer(pane_id: i64) -> bool {
    let Some(sent) = setting_sent_at(pane_id) else { return false };
    if crate::cli::now_ms() - sent >= ANSWER_WITHIN_MS { return false; }
    NOTICES.get().and_then(|m| m.lock().ok().and_then(|m| m.get(&pane_id).map(|(t, _)| *t))).map_or(true, |t| t < sent)
}
pub fn note_notice(pane_id: i64, text: String) { if !expecting_answer(pane_id) || !answers_command(&text, &setting_arg_for(pane_id)) { return; } if let Ok(mut m) = NOTICES.get_or_init(Default::default).lock() { m.insert(pane_id, (crate::cli::now_ms(), text)); } }
pub fn notice_for(pane_id: i64) -> Option<String> { let (t, s) = NOTICES.get()?.lock().ok()?.get(&pane_id).cloned()?; (crate::cli::now_ms() - t < NOTICE_MS).then_some(s) }
/// A SLASH COMMAND THAT ONLY CHANGES A SETTING STARTS NO RUN (2026-11-24: after `/model haiku` the chat showed "Reasoning..." for the whole grace period below, because a send makes the pane read as working until a run appears).
/// The command is the first word; arguments do not matter. Commands that DO run the model (`/compact`, `/init`, `/review`, a custom command) are not here. Pure - tested.
pub fn is_setting_command(text: &str) -> bool {
    let first = text.trim().split_whitespace().next().unwrap_or("");
    matches!(first, "/model" | "/effort" | "/help" | "/status" | "/cost" | "/usage" | "/config" | "/theme" | "/vim" | "/clear" | "/exit" | "/quit" | "/resume" | "/permissions" | "/mcp" | "/agents" | "/doctor" | "/context")
}
pub fn note_send(pane_id: i64, text: &str) { if is_setting_command(text) { note_setting_sent(pane_id, text); return; } if let Ok(mut m) = SENT.get_or_init(Default::default).lock() { m.insert(pane_id, (crate::cli::now_ms(), text_hash(text))); } }
fn sent_for(pane_id: i64) -> Option<(i64, u64)> { SENT.get()?.lock().ok()?.get(&pane_id).copied() }
fn sent_at(pane_id: i64) -> Option<i64> { sent_for(pane_id).map(|s| s.0) }
/// How long the composer's check of an ask stands for the engine's hook.
const CHECKED_FOR_MS: i64 = 120_000;
/// Did the composer send exactly this text to this pane a moment ago (so it has been through the pre-check already)? Pure.
pub fn checked_by_composer(sent: Option<(i64, u64)>, text: &str, now_ms: i64) -> bool {
    sent.map_or(false, |(t, h)| now_ms - t < CHECKED_FOR_MS && h == text_hash(text))
}
/// How long a send counts as "the run is about to appear" before the pane is believed again.
const SEND_GRACE_MS: i64 = 90_000;

/// Has the answer to what was sent at `sent` already arrived? Claude Code says so itself (`end_turn`). Pure.
pub fn claude_answered(lines: &str, sent_ms: i64) -> bool {
    turn_ended_in_transcript(lines.lines(), "").as_deref().and_then(iso_to_ms).map_or(false, |t| t >= sent_ms - 1000)
}
/// The same for an engine with no end mark: its last turn is an answer written after the send, no tool is left open, and nothing has been added for a few seconds. Pure.
pub fn turns_answered(turns: &Value, sent_ms: i64, now_ms: i64) -> bool {
    let Some(l) = turns.as_array().and_then(|a| a.last()) else { return false };
    let open = l["tools"].as_array().map_or(false, |a| a.iter().any(|t| t["done"] != true));
    l["role"] == "assistant" && !open && !l["text"].as_str().unwrap_or("").trim().is_empty()
        && l["t"].as_i64().or_else(|| l["t"].as_str().and_then(iso_to_ms)).map_or(false, |t| t >= sent_ms - 1000 && now_ms - t > 8_000)
}
/// `sent_at`, forgotten once the pane's own conversation holds the answer. An agent in the shared folder (a project without Git) has no run record, so without this it read "working" for 90 s after every message, the answer already on screen (found 2026-11-27).
async fn unanswered_send(state: &Arc<AppState>, root: &str, kind: &str, pane_id: i64) -> Option<i64> {
    let now = crate::cli::now_ms();
    let sent = sent_at(pane_id).filter(|s| now - s < SEND_GRACE_MS)?;
    let answered = if kind == "claude-code" {
        let shared = engine_call(state, "system.cockpit.pane.get", json!({ "id": pane_id }), json!({ "actor": "web-fe", "source": "web-fe" })).await.map(|(v, _)| v["worktree_path"].as_str().unwrap_or("").is_empty()).unwrap_or(false);
        let root = root.to_string();
        tokio::task::spawn_blocking(move || {
            let session = if shared { crate::shared_folder::session_of(&root, pane_id) } else { serde_json::from_str::<Value>(&read(&Path::new(&root).join(".antiloki").join("pty-session.json"))).ok().and_then(|v| v["session_id"].as_str().map(String::from)) };
            session.and_then(|s| claude_transcript(&root, &s)).map_or(false, |f| claude_answered(&read(&f), sent))
        }).await.unwrap_or(false)
    } else { turns_answered(&transcript_value(state, pane_id, 4).await["turns"], sent, now) };
    (!answered).then_some(sent)
}

/// A known pane that reads idle, a prompt sent to it a moment ago, and no run in the engine's record that started after that send:
/// the run has not been written yet, so it is working. A blocked pane is waiting on the person, not working. Pure.
pub fn with_pending_send(mut v: Value, sent_ms: Option<i64>, now_ms: i64) -> Value {
    let Some(sent) = sent_ms.filter(|s| now_ms - s < SEND_GRACE_MS) else { return v };
    let idle = v["known"] == true && v["working"] != true && v["blocked"].is_null();
    let newer = v["run"]["started_t"].as_str().and_then(iso_to_ms).map_or(false, |t| t >= sent - 1000);
    // the run still on record is the one before this prompt: showing its prompt and tokens as the current run would be wrong
    if idle && !newer { v["working"] = json!(true); v["pending_send"] = json!(true); v["run"] = Value::Null; }
    v
}

pub async fn usage_get(state: Arc<AppState>, q: HashMap<String, String>) -> Response {
    let pane_id: i64 = q.get("pane_id").and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let (root, kind, sandbox) = pane_facts(&state, pane_id).await;
    let rows: Option<Vec<String>> = if crate::pty_remote::enabled() {
        tokio::task::spawn_blocking(move || crate::pty_remote::screen_rows(pane_id)).await.ok().flatten()
    } else { None };
    let rows_seen = rows.clone();
    let (r2, k2) = (root.clone(), kind.clone());
    let v = tokio::task::spawn_blocking(move || usage_for_pane(Path::new(&r2), pane_id, &k2, rows.as_deref())).await.unwrap_or_else(|_| json!({ "ok": false }));
    let sent = unanswered_send(&state, &root, &kind, pane_id).await;
    let mut v = with_pending_send(v, sent, crate::cli::now_ms());
    // no status line (an agent in the shared folder) and the start banner scrolled away: the conversation names the model that answered
    if kind == "claude-code" && v["known"] == true && v["model"].is_null() {
        if let Some(m) = last_model(&transcript_value(&state, pane_id, 6).await["turns"]) { v["model"] = json!(m); }
    }
    if sandbox { v["sandbox"] = json!(true); }
    if let Some(r) = route_for(pane_id) { v["route"] = json!(r); }
    // the screen is the truth about whether a question is still asked: one read from the stream and answered since is forgotten
    if let Some(rows) = rows_seen.as_deref() { if dialog_for(pane_id).is_some_and(|d| d.starts_with("Switch model?")) && !rows.iter().any(|r| r.contains("Switch model?")) { note_dialog(pane_id, None); } }
    // a question the CLI is asking in its terminal (read from the stream, so it works without the terminal host too) is what the pane is waiting on
    if v["blocked"].is_null() { if let Some(d) = dialog_for(pane_id) { v["blocked"] = json!(d); } }
    if let Some(n) = notice_for(pane_id) { v["notice"] = json!(n); }
    if crate::pty::limits_off(pane_id) { v["limitsOff"] = json!(true); }
    // a pane whose provider was switched: has the new provider read the reconciliation pack yet ("pending" | "read")
    if let Some(r) = crate::pane_provider::status_of(pane_id) { v["reconcile"] = json!(r); }
    reply(StatusCode::OK, v)
}

/// `GET /api/v1/cockpit/pty/transcript?pane_id=N&cap=M`: the pane's own conversation as turns, for the chat skin (the terminal keeps running; this only reads what Claude Code wrote). Other engines answer `known: false`, so their pane stays a terminal.
pub async fn transcript_get(state: Arc<AppState>, q: HashMap<String, String>) -> Response {
    let pane_id: i64 = q.get("pane_id").and_then(|s| s.trim().parse().ok()).unwrap_or(0); let cap: usize = q.get("cap").and_then(|s| s.trim().parse().ok()).unwrap_or(200).clamp(1, 1000);
    reply(StatusCode::OK, transcript_value(&state, pane_id, cap).await)
}
/// THE TRANSCRIPTS OF A PANE'S EARLIER BOXES, oldest first (the operator 2026-11-22: "chat mode is not getting the history of the conversation"). A restart (a model change, a new serve) can make the pane a NEW box and a new Claude session, so what it said before sits in the file of an earlier box. A pane's boxes share a name (`cockpit-<session>-<index>-<stamp>`), but an index is REUSED by a different pane once the first is closed, so the name proves nothing: a box is the pane's own only when the `serve.json` it holds names this pane's number. Blocking.
pub fn sibling_transcripts(root: &str, current: &Path, pane_id: i64) -> Vec<(std::time::SystemTime, u64, PathBuf)> {
    let Some(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok() else { return vec![] };
    let Some(base) = Path::new(root).file_name().map(|n| n.to_string_lossy().to_string()) else { return vec![] }; let Some(parent) = Path::new(root).parent() else { return vec![] };
    let prefix = match base.rsplit_once('-') { Some((p, h)) if h.len() >= 8 && h.chars().all(|c| c.is_ascii_hexdigit()) => p.to_string(), _ => base.clone() };
    if !prefix.contains("cockpit-") || pane_id <= 0 { return vec![]; }
    let own = antiloki_rs::context_context_library::claude_project_slug(root); let stem = own[..own.len() - base.len()].to_string(); let want = format!("{stem}{prefix}");
    let proj = Path::new(&home).join(".claude").join("projects"); let mut files = vec![];
    for d in std::fs::read_dir(&proj).into_iter().flatten().flatten() {
        let name = d.file_name().to_string_lossy().to_string(); let sib = name == want || name.strip_prefix(&format!("{want}-")).map_or(false, |h| h.len() >= 8 && h.chars().all(|c| c.is_ascii_hexdigit()));
        if !sib || name == own { continue; }
        let box_dir = parent.join(&name[stem.len()..]);   // the box this project folder belongs to: it must say it was THIS pane's
        let mine = serde_json::from_str::<Value>(&read(&box_dir.join(".antiloki").join("serve.json"))).ok().and_then(|v| v["pane_id"].as_i64()) == Some(pane_id); if !mine { continue; }
        for f in std::fs::read_dir(d.path()).into_iter().flatten().flatten() { let p = f.path(); if p.extension().map_or(false, |e| e == "jsonl") && p != current { if let Ok(m) = f.metadata() { files.push((m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len(), p)); } } }
    }
    files.sort(); files
}
/// The engine's own session file, for what the turns do not carry (task lists, asks waiting); empty for an engine that keeps none. Blocking.
pub(crate) fn session_file(root: &str, kind: &str, session: &str) -> Option<PathBuf> {
    match kind { "claude-code" if !session.is_empty() => claude_transcript(root, session), "codex" => codex_rollout_for(root), _ => None }
}
/// A SESSION FILE IS PARSED ONCE FOR EACH TIME IT CHANGES (found 2026-11-26: every poll of the chat, the usage line and the queue parsed megabytes again, a third of a core while nothing happened).
/// `tag` says what was made of it (and anything else the result depends on). Blocking.
pub(crate) fn parsed<T: Clone + Send + 'static>(tag: &str, path: &Path, make: impl FnOnce(&str) -> T) -> T {
    type Kept = HashMap<(String, PathBuf), (std::time::SystemTime, u64, Box<dyn std::any::Any + Send>)>;
    static KEPT: std::sync::Mutex<Option<Kept>> = std::sync::Mutex::new(None);
    let stamp = std::fs::metadata(path).ok().map(|m| (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len()));
    let key = (tag.to_string(), path.to_path_buf());
    if let (Some((at, len)), Ok(g)) = (stamp, KEPT.lock()) { if let Some((a, l, v)) = g.as_ref().and_then(|k| k.get(&key)) { if *a == at && *l == len { if let Some(v) = v.downcast_ref::<T>() { return v.clone(); } } } }
    let out = make(&read(path));
    // the stamp is the one taken BEFORE reading: a file written while it was read is read again next time
    if let (Some((at, len)), Ok(mut g)) = (stamp, KEPT.lock()) { let k = g.get_or_insert_with(Default::default); if k.len() >= 160 { k.clear(); } k.insert(key, (at, len, Box::new(out.clone()))); }
    out
}
/// The turns said before the last restart, oldest first (see `sibling_transcripts`), cached by what is on disk because the chat asks every few seconds. Blocking.
pub fn earlier_turns(root: &str, current: &Path, cap: usize, pane_id: i64) -> Vec<Value> {
    let files = sibling_transcripts(root, current, pane_id); if files.is_empty() { return vec![]; }
    let sig: u64 = files.iter().map(|(t, l, _)| *l ^ t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)).fold(0, |a, b| a.wrapping_mul(31).wrapping_add(b)); let key = format!("{root}|{pane_id}");
    static CACHE: std::sync::Mutex<Option<(String, u64, Vec<Value>)>> = std::sync::Mutex::new(None);
    if let Some((k, g, v)) = CACHE.lock().ok().and_then(|c| c.clone()) { if k == key && g == sig { return v; } }
    let mut out: Vec<Value> = vec![]; for (_, _, p) in &files { out.extend(crate::cowork_transcript::turns_from_claude(read(p).lines(), cap)); }
    let n = out.len(); if n > cap { out.drain(0..n - cap); } if let Ok(mut c) = CACHE.lock() { *c = Some((key, sig, out.clone())); } out
}
/// A RESTART KEEPS THE CONVERSATION: a pane that starts in a new box with no session of its own takes the newest real conversation of its earlier boxes: the transcript is copied into this box's project folder and its id returned, so the CLI can `--resume` it. Blocking.
pub fn adopt_earlier_session(cwd: &str, pane_id: i64) -> Option<String> {
    let (_, _, newest) = sibling_transcripts(cwd, Path::new(""), pane_id).into_iter().filter(|(_, len, _)| *len >= 4096).last()?;
    let id = newest.file_stem()?.to_string_lossy().to_string(); if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') { return None; }
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok()?; let dest = Path::new(&home).join(".claude").join("projects").join(antiloki_rs::context_context_library::claude_project_slug(cwd));
    std::fs::create_dir_all(&dest).ok()?; let to = dest.join(format!("{id}.jsonl")); if !to.exists() { std::fs::copy(&newest, &to).ok()?; } Some(id)
}
/// The pane's conversation as the chat skin reads it; for a specialist's pane the turns are also kept as raw chunks of its memory (once each).
/// The newest Antigravity conversation whose workspace is this folder: `agy` records `{workspace, conversationId}` in its history file and writes each conversation's steps under `brain/<id>`.
pub(crate) fn agy_transcript_for(root: &str) -> Option<PathBuf> { agy_transcript_within(root, crate::shared_folder::ALL) }
fn agy_transcript_within(root: &str, w: crate::shared_folder::Window) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    agy_transcript_in(&PathBuf::from(home).join(".gemini").join("antigravity-cli"), root, w)
}
fn agy_transcript_in(base: &Path, root: &str, w: crate::shared_folder::Window) -> Option<PathBuf> {
    let want = Path::new(root);
    let file_of = |id: &str| { let p = base.join("brain").join(id).join(".system_generated").join("logs").join("transcript.jsonl"); (id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && p.is_file()).then_some(p) };
    let history = std::fs::read_to_string(base.join("history.jsonl")).unwrap_or_default();
    let mut found: Vec<PathBuf> = history.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v["workspace"].as_str().is_some_and(|w| Path::new(w) == want)).filter_map(|v| v["conversationId"].as_str().and_then(file_of)).collect();
    // a first message is written before its conversation has a number: the summaries table knows the workspace
    if let Ok(c) = rusqlite::Connection::open_with_flags(base.join("conversation_summaries.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
        if let Ok(mut st) = c.prepare("SELECT conversation_id, workspace_uris FROM conversation_summaries ORDER BY last_modified_time DESC LIMIT 200") {
            if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
                found.extend(rows.flatten().filter(|(_, uris)| uris.split(|ch: char| "[]\",\n ".contains(ch)).any(|u| Path::new(u.trim_start_matches("file://")) == want)).filter_map(|(id, _)| file_of(&id)));
            }
        }
    }
    found.into_iter().filter(|p| crate::shared_folder::holds(w, crate::shared_folder::born(p))).max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
}

/// The conversation of a box on an engine antiloki reads from the engine's own store (not Claude's, which has hooks). Blocking.
pub(crate) fn engine_turns(root: &str, engine: &str, cap: usize, window: crate::shared_folder::Window) -> Vec<Value> {
    match engine {
        "opencode" => opencode_turns_in(root, cap, window),
        "gemini" => agy_transcript_within(root, window).map(|f| crate::cowork_transcript::turns_from_agy(read(&f).lines(), cap)).unwrap_or_default(),
        "cursor" => cursor_transcript_in(root, window).map(|f| crate::cowork_transcript::turns_from_cursor(read(&f).lines(), cap)).unwrap_or_default(),
        _ => vec![],
    }
}

pub async fn transcript_value(state: &Arc<AppState>, pane_id: i64, cap: usize) -> Value {
    let (root, kind, _) = pane_facts(state, pane_id).await;
    let engine = kind.clone();
    let shared = engine_call(state, "system.cockpit.pane.get", json!({ "id": pane_id }), json!({ "actor": "web-fe", "source": "web-fe" })).await.map(|(v, _)| v["worktree_path"].as_str().unwrap_or("").is_empty()).unwrap_or(false);
    let value = tokio::task::spawn_blocking(move || {
        let w = if shared { crate::shared_folder::window(&root, pane_id) } else { crate::shared_folder::ALL };
        if kind == "codex" {
            // Codex keeps its conversations under ~/.codex/sessions: the newest one whose working folder is this pane's worktree
            let Some(file) = codex_rollout_in(&root, w) else { return json!({ "ok": true, "known": true, "turns": [], "why": "no Codex conversation in this worktree yet" }) };
            let turns = crate::cowork_transcript::turns_from_codex(read(&file).lines(), cap);
            return json!({ "ok": true, "known": true, "session": file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), "turns": turns });
        }
        if kind == "cursor" {
            let Some(file) = cursor_transcript_in(&root, w) else { return json!({ "ok": true, "known": true, "turns": [], "why": "no Cursor conversation in this worktree yet" }) };
            return json!({ "ok": true, "known": true, "session": file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), "turns": crate::cowork_transcript::turns_from_cursor(read(&file).lines(), cap) });
        }
        if kind == "gemini" {
            let Some(file) = agy_transcript_within(&root, w) else { return json!({ "ok": true, "known": true, "turns": [], "why": "no Antigravity conversation in this worktree yet" }) };
            return json!({ "ok": true, "known": true, "session": "antigravity", "turns": crate::cowork_transcript::turns_from_agy(read(&file).lines(), cap) });
        }
        if kind == "opencode" { return json!({ "ok": true, "known": true, "turns": opencode_turns_in(&root, cap, w) }); }
        if kind != "claude-code" { return json!({ "ok": true, "known": false, "why": "the chat view reads this engine's transcript; this engine stays a terminal", "turns": [] }); }
        let dir = Path::new(&root).join(".antiloki"); let session: Option<String> = serde_json::from_str::<Value>(&read(&dir.join("pty-session.json"))).ok().and_then(|v| v["session_id"].as_str().map(String::from));
        let session = if shared { crate::shared_folder::session_of(&root, pane_id) } else { session };
        let Some(s) = session else { return json!({ "ok": true, "known": true, "turns": [], "why": "no session yet" }) };
        let cur = claude_transcript(&root, &s);
        let turns = cur.as_ref().map(|p| parsed(&format!("turns|{cap}"), p, |t| crate::cowork_transcript::turns_from_claude(t.lines(), cap))).unwrap_or_default();
        // what was said before the pane was last restarted comes first (see `earlier_turns`)
        let before = earlier_turns(&root, cur.as_deref().unwrap_or(Path::new("")), cap, pane_id);
        // A SPECIALIST'S PANE KEEPS ITS CONVERSATION: the turns are stored once each as raw chunks of its memory (what a note can cite)
        if let Some((sroot, sid)) = crate::specialists::bound_of(pane_id) { let _ = crate::specialists::ingest_turns(&sroot, &sid, "claude-code", &s, &turns); }
        let mut all = before; all.extend(turns.iter().cloned()); let n = all.len(); if n > cap { all.drain(0..n - cap); }
        json!({ "ok": true, "known": true, "session": s, "turns": all })
    }).await.unwrap_or_else(|_| json!({ "ok": false }));
    let value = marked(value);
    // A SPECIALIST ON ANY OTHER ENGINE KEEPS ITS CONVERSATION TOO (Claude's is stored above, from the current session only)
    if engine != "claude-code" {
        if let Some((sroot, sid)) = crate::specialists::bound_of(pane_id) {
            if let Some(turns) = value["turns"].as_array().filter(|t| !t.is_empty()).cloned() {
                let session = value["session"].as_str().map(String::from).unwrap_or_else(|| format!("{engine}-{pane_id}"));
                let _ = tokio::task::spawn_blocking(move || crate::specialists::ingest_turns(&sroot, &sid, &engine, &session, &turns)).await;
            }
        }
    }
    value
}

/// What antiloki typed into a terminal (a handover, a setting command) is marked, so the chat does not draw it as something the person said (#28). Pure.
fn marked(mut v: Value) -> Value { if let Some(t) = v.get_mut("turns").and_then(Value::as_array_mut) { for turn in t.iter_mut().filter(|t| crate::pane_provider::typed_by_antiloki(t)) { turn["by"] = json!("antiloki"); } } v }
pub(crate) fn layout_path(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("cowork-layout.json") }
const LAYOUT_MAX: usize = 256 * 1024;

/// Drop from a stored layout every pane that is not this workspace's: an agent or tool pane whose cockpit pane number has no row here. Pane numbers are PER WORKSPACE (project A's pane 11 and B's are different panes with the same
/// number), so a layout that holds another project's panes would attach this project's terminals to the wrong agents (operator 2026-11-10: "e2escratch and ministore are completely apart spaces, but they are using the same panes").
/// A shell pane has no row (its number is the screen's own), so it stays. A split left with one child collapses into it. Pure.
pub fn prune_layout(layout: &Value, rows: &std::collections::HashMap<i64, i64>) -> Value {
    fn walk(n: &Value, rows: &std::collections::HashMap<i64, i64>) -> Option<Value> {
        match n["type"].as_str() {
            Some("leaf") => {
                let p = &n["pane"]; let kind = p["kind"].as_str().unwrap_or("");
                // foreign: no pane of that number here, or one of that number in ANOTHER session (the same number in two workspaces is two different panes)
                let foreign = kind != "shell" && kind != "worker" && p["paneId"].as_i64().map_or(false, |id| match rows.get(&id) { None => true, Some(sess) => p["sessionId"].as_i64().map_or(false, |ls| *sess != 0 && ls != *sess) });
                if foreign { None } else { Some(n.clone()) }
            }
            Some("split") => {
                let kids: Vec<(usize, Value)> = n["children"].as_array().into_iter().flatten().enumerate().filter_map(|(i, c)| walk(c, rows).map(|k| (i, k))).collect();
                if kids.is_empty() { return None; }
                if kids.len() == 1 { return Some(kids[0].1.clone()); }
                let sizes: Vec<f64> = kids.iter().map(|(i, _)| n["sizes"][*i].as_f64().unwrap_or(1.0)).collect(); let total: f64 = sizes.iter().sum::<f64>().max(1e-9);
                let mut o = n.clone(); o["children"] = Value::Array(kids.into_iter().map(|(_, k)| k).collect()); o["sizes"] = Value::Array(sizes.iter().map(|x| json!(x / total)).collect()); Some(o)
            }
            _ => None,
        }
    }
    let mut out = layout.clone();
    out["root"] = walk(&layout["root"], rows).unwrap_or(Value::Null);
    let ids: Vec<String> = { fn ids(n: &Value, o: &mut Vec<String>) { if n["type"] == "leaf" { if let Some(i) = n["pane"]["id"].as_str() { o.push(i.to_string()); } } else { for c in n["children"].as_array().into_iter().flatten() { ids(c, o); } } } let mut v = vec![]; ids(&out["root"], &mut v); v };
    for k in ["focus", "zoom"] { if let Some(f) = layout[k].as_str() { if !ids.iter().any(|i| i == f) { out[k] = if k == "focus" { ids.first().map(|i| json!(i)).unwrap_or(Value::Null) } else { Value::Null }; } } }
    out
}

/// `GET /api/v1/cockpit/cowork/layout` - the open workspace's layout, with what is not this workspace's left out.
pub async fn layout_get(state: Arc<AppState>, q: HashMap<String, String>) -> Response {
    let root = crate::agents_http::active_root(&state).await;
    // THE THREAD: the one asked for, else the open one (each thread of a space has a screen of its own)
    let thread = q.get("thread").filter(|t| !t.is_empty()).cloned().unwrap_or_else(|| crate::cowork_threads::active(&root));
    if !crate::cowork_threads::exists(&root, &thread) { return reply(StatusCode::NOT_FOUND, json!({ "ok": false, "error": "no such thread" })); }
    let text = std::fs::read_to_string(crate::cowork_threads::layout_file(&root, &thread)).unwrap_or_default();
    let mut layout: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if layout.is_object() {
        let st = state.clone();
        let rows: std::collections::HashMap<i64, i64> = tokio::task::spawn_blocking(move || { let c = st.pick(); let g = c.lock().unwrap_or_else(|p| p.into_inner()); antiloki_rs::db_json::query_rows(&g, "SELECT id, COALESCE(session_id, 0) AS sess FROM cockpit_panes", &[]).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default().iter().filter_map(|r| Some((r["id"].as_i64()?, r["sess"].as_i64().unwrap_or(0)))).collect() }).await.unwrap_or_default();
        layout = prune_layout(&layout, &rows);
    }
    reply(StatusCode::OK, json!({ "ok": true, "layout": layout, "workspace": root, "thread": thread }))
}
/// `POST /api/v1/cockpit/cowork/layout {layout, workspace?}`: written atomically (a temp file, then a rename), size-capped, objects only. `workspace` is the root the layout was made for: one that is not the open workspace is REFUSED
/// (a save that was on its way while the person switched spaces would otherwise land in the space they switched to).
pub async fn layout_put(state: Arc<AppState>, body: Bytes) -> Response {
    let b = json_body(&body);
    let layout = &b["layout"];
    if !layout.is_object() { return reply(StatusCode::BAD_REQUEST, json!({ "ok": false, "error": "layout must be an object" })); }
    let text = layout.to_string();
    if text.len() > LAYOUT_MAX { return reply(StatusCode::PAYLOAD_TOO_LARGE, json!({ "ok": false, "error": "the layout is too large" })); }
    let root = crate::agents_http::active_root(&state).await;
    if root.is_empty() { return reply(StatusCode::CONFLICT, json!({ "ok": false, "error": "no workspace is open" })); }
    if let Some(for_ws) = b["workspace"].as_str().filter(|w| !w.is_empty()) { if !same_root(for_ws, &root) { return reply(StatusCode::CONFLICT, json!({ "ok": false, "error": "this layout is for another workspace than the open one", "stale": true })); } }
    // the thread the layout was made for (a save on its way while the person changed thread must not land in the other one); none named = the open one
    let thread = b["thread"].as_str().filter(|t| !t.is_empty()).map(String::from).unwrap_or_else(|| crate::cowork_threads::active(&root));
    if !crate::cowork_threads::exists(&root, &thread) { return reply(StatusCode::CONFLICT, json!({ "ok": false, "error": "this layout is for a thread that no longer exists", "stale": true })); }
    let p = crate::cowork_threads::layout_file(&root, &thread);
    let r = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(p.parent().unwrap())?;
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, &p)
    })();
    match r { Ok(()) => reply(StatusCode::OK, json!({ "ok": true })), Err(e) => reply(StatusCode::INTERNAL_SERVER_ERROR, json!({ "ok": false, "error": e.to_string() })) }
}
/// `POST /api/v1/cockpit/cowork/threads {op, thread?, name?}`: the open space's threads (list, create, rename, select, delete). Changes are announced to the screens.
/// Close a thread's panes: end their terminals, mark them closed and clear the thread's layout. Deleting a thread does this first, because a team thread's two panes cannot be closed by hand (the operator 2026-11-15: "the button that discards threads or teams does not work"). Returns how many were closed.
pub async fn release_thread_panes(state: &Arc<AppState>, root: &str, thread: &str) -> usize {
    let ids = { let thread = thread.to_string(); let r2 = root.to_string(); let id = crate::cowork_threads::view(&r2)["threads"].as_array().into_iter().flatten().find(|x| x["id"] == json!(thread) || x["name"].as_str().map_or(false, |n| n.eq_ignore_ascii_case(&thread))).and_then(|x| x["id"].as_str().map(String::from)); match id { Some(id) => tokio::task::spawn_blocking(move || crate::cowork_threads::take_pane_ids(&r2, &id)).await.unwrap_or_default(), None => vec![] } };
    let ctx = json!({ "actor": "web-fe", "source": "web-fe" }); for pid in &ids { crate::team_graph_run::pty_kill(*pid); let _ = engine_call(state, "system.cockpit.pane.update", json!({ "pane_id": pid, "status": "closed" }), ctx.clone()).await; } ids.len()
}
pub async fn threads_op(state: Arc<AppState>, body: Bytes) -> Response {
    let b = json_body(&body); let root = crate::agents_http::active_root(&state).await;
    if root.is_empty() { return reply(StatusCode::CONFLICT, json!({ "ok": false, "error": "no workspace is open" })); }
    // a thread with unmerged work of its own is not thrown away by deleting it: land or discard it first (a thread with a tree and nothing in it goes with its tree)
    // DELETING A THREAD CLOSES ITS PANES AND NOTHING IS LOST: its folder (and the commits in its branch) are saved to the archive by the discard that follows, so a thread with work of its own can be deleted in one click
    // (only a TEAM thread: its two panes cannot be closed by hand, so deleting it closes them; a plain thread with terminals open is still refused: closing an agent is the person's decision)
    if b["op"] == "delete" { let t = b["thread"].as_str().unwrap_or("").to_string(); let r2 = root.clone(); let is_team = tokio::task::spawn_blocking(move || crate::cowork_threads::view(&r2)["threads"].as_array().into_iter().flatten().any(|x| (x["id"] == json!(t) || x["name"].as_str().map_or(false, |n| n.eq_ignore_ascii_case(&t))) && x["team"].as_str().map_or(false, |s| !s.is_empty()))).await.unwrap_or(false);
        if is_team { release_thread_panes(&state, &root, b["thread"].as_str().unwrap_or("")).await; } }
    let st2 = state.clone();
    let r = tokio::task::spawn_blocking(move || {
        let gone: Option<String> = if b["op"] == "delete" { crate::cowork_threads::view(&root)["threads"].as_array().into_iter().flatten().find(|x| x["id"] == b["thread"] || x["name"].as_str().map_or(false, |n| Some(n.to_ascii_lowercase()) == b["thread"].as_str().map(str::to_ascii_lowercase))).and_then(|x| x["id"].as_str().map(String::from)) } else { None };
        let r = crate::cowork_threads::apply(&root, &b);
        if r.is_ok() { if let Some(id) = gone { let _ = crate::thread_tree::discard(&root, &id); } }
        r.map(|_| { let mut v = crate::thread_tree::decorated(&root); crate::thread_tree::mark_copies(&mut v, &crate::work::boxes_of_project(&st2).iter().map(|b| b.pane_id).collect()); v })
    }).await.unwrap_or_else(|e| Err(e.to_string()));
    match r { Ok(v) => reply(StatusCode::OK, json!({ "ok": true, "active": v["active"], "threads": v["threads"], "git": v["git"] })), Err(e) => reply(StatusCode::OK, json!({ "ok": false, "error": e })) }
}
/// Two folder spellings that name the same place. Pure.
pub fn same_root(a: &str, b: &str) -> bool { let n = |x: &str| std::fs::canonicalize(x).map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| x.trim_end_matches(['/', '\\']).to_string()); n(a) == n(b) }

/// `GET /api/v1/cockpit/episodes?limit=`
pub async fn episodes_get(state: Arc<AppState>, q: HashMap<String, String>) -> Response {
    let limit = q.get("limit").and_then(|s| s.parse::<usize>().ok()).unwrap_or(20).clamp(1, 200);
    let root = crate::agents_http::active_root(&state).await;
    let list = tokio::task::spawn_blocking(move || crate::episodes_db::list(&root, None, limit)).await.unwrap_or_default();
    reply(StatusCode::OK, json!({ "ok": true, "episodes": list }))
}

#[cfg(test)]
mod tests {
    #[test] fn a_conversations_id_is_read_the_way_each_engine_keeps_it() {
        let d = std::env::temp_dir().join(format!("al-convid-{}", std::process::id())); std::fs::create_dir_all(&d).unwrap();
        let f = d.join("rollout-x.jsonl"); std::fs::write(&f, "{\"type\":\"session_meta\",\"payload\":{\"id\":\"019a-bc\",\"cwd\":\"/p\"}}\n{}\n").unwrap();
        assert_eq!(super::codex_id(&f).as_deref(), Some("019a-bc"));
        assert_eq!(super::agy_id(std::path::Path::new("/h/brain/c0ff-ee/.system_generated/logs/transcript.jsonl")).as_deref(), Some("c0ff-ee"));
        assert_eq!(super::conversation_id("codex", "/nowhere", crate::shared_folder::NONE), None);
        let _ = std::fs::remove_dir_all(d);
    }
    #[test]
    fn cursors_command_question_makes_the_pane_need_the_person_until_the_prompt_is_back() {
        let rows = |t: &[&str]| t.iter().map(|r| r.to_string()).collect::<Vec<_>>();
        let asking = [" $  pwd && ls -la in .", " Run this command?", " Not in allowlist: pwd", "  \u{2192} Run (once) (y)", "    Add Shell(pwd) to allowlist? (tab)", "    Run Everything (shift+tab)", "    Skip & tell the agent what to do instead (esc or n)"];
        assert_eq!(blocked_from_screen(&rows(&asking)).as_deref(), Some("Run this command?"));
        let mut answered = asking.to_vec(); answered.push("  \u{2192} Add a follow-up");
        assert_eq!(blocked_from_screen(&rows(&answered)), None);
        assert_eq!(blocked_from_screen(&rows(&["Should I run this command? Tell me.", "  \u{2192} Add a follow-up"])), None);
        assert_eq!(blocked_from_screen(&rows(&[" antiloki: antiloki_remember", " Run this MCP tool?", "  \u{2192} Run (once) (y)", "    Allowlist MCP Tool (tab)", "    Skip (esc or n)"])).as_deref(), Some("Run this MCP tool?"));
    }
    #[test]
    fn antigravity_and_opencode_permission_questions_make_the_pane_need_the_person() {
        let rows = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let agy = rows(&["\u{25cf} Called 1 MCP tool (antiloki/antiloki_cost) (ctrl+o to expand)", "MCP", "\u{2500}\u{2500}\u{2500}\u{2500}", "antiloki/antiloki_cost", "Allow calling this tool?", "> 1. Yes, allow tool call",
            "  2. Yes, and always allow tool 'antiloki/antiloki_cost' in this conversation", "  3. Yes, and always allow tool 'antiloki/antiloki_cost' (Persist to settings.json)", "  4. No, deny tool call",
            "  5. No, and always deny tool 'antiloki/antiloki_cost' in this conversation", "  6. No, and always deny tool 'antiloki/antiloki_cost' (Persist to settings.json)", "  \u{2191}/\u{2193} Navigate \u{b7} tab Amend", "esc to cancel      auto \u{b7} Gemini 3.8 Flash \u{b7} medium"]);
        assert_eq!(blocked_from_screen(&agy).as_deref(), Some("Allow calling this tool?"));
        let open = rows(&["  \u{2503}  \u{280b} ls", "     Press ctrl+b to move running work to the background", "  \u{2503}", "  \u{2503}  \u{25b3} Permission required", "  \u{2503}", "  \u{2503}  $ ls", "  \u{2503}", "  \u{2503}   Allow once   Always allow   Reject     ctrl+f fullscreen  \u{21c6} select  enter confirm"]);
        assert_eq!(blocked_from_screen(&open).as_deref(), Some("Permission required"));
        // answered: the composer is below it again
        let mut done = agy.clone(); done.push("? for shortcuts".into()); assert_eq!(blocked_from_screen(&done), None);
        let mut done = open.clone(); done.push("  ctrl+p commands".into()); assert_eq!(blocked_from_screen(&done), None);
        // the words in an answer are not a question
        assert_eq!(blocked_from_screen(&rows(&["Permission required means the tool asks first.", "It can also say allow calling this tool.", "> "])), None);
    }
    #[test]
    fn cursors_project_folder_is_found_for_linux_and_for_every_windows_spelling_of_a_path() {
        let root = std::env::temp_dir().join(format!("antiloki-cursor-slug-{}", std::process::id())); let _ = std::fs::remove_dir_all(&root);
        for d in ["home-me-dev-x", "c-Users-Me-proj", "D--Work-Other-app"] { std::fs::create_dir_all(root.join(d)).unwrap(); }
        assert_eq!(cursor_project_dir(&root, "/home/me/dev/x/").unwrap().file_name().unwrap(), "home-me-dev-x");
        assert_eq!(cursor_project_dir(&root, r"C:\Users\Me\proj").unwrap().file_name().unwrap(), "c-Users-Me-proj", "the drive letter is lower case");
        assert_eq!(cursor_project_dir(&root, r"\\?\C:\Users\Me\proj").unwrap().file_name().unwrap(), "c-Users-Me-proj", "a verbatim prefix is dropped");
        assert_eq!(cursor_project_dir(&root, r"D:\Work\Other\app").unwrap().file_name().unwrap(), "D--Work-Other-app", "doubled dashes are found too");
        assert_eq!(cursor_project_dir(&root, r"c:\users\me\proj").unwrap().file_name().unwrap(), "c-Users-Me-proj", "and so is a path in another case, by letters and digits");
        assert!(cursor_project_dir(&root, "/home/me/other").is_none());
        assert_eq!(cursor_slugs(r"C:\Users\Me\proj"), vec!["C--Users-Me-proj", "C-Users-Me-proj", "c--Users-Me-proj", "c-Users-Me-proj"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_clis_answer_to_a_setting_command_counts_once_and_only_after_the_command_was_sent() {
        use super::{expecting_answer, note_notice, note_send, notice_for};
        let p = 987_010;
        note_notice(p, "Set model to Haiku 5.5".into()); assert_eq!(notice_for(p), None, "a redrawn old line (a resumed session) is history, not an answer");
        assert!(!expecting_answer(p));
        note_send(p, "/model sonnet"); assert!(expecting_answer(p));
        note_notice(p, "Set model to Haiku 5.5 and saved".into()); assert_eq!(notice_for(p), None, "the redraw of an older answer is not the answer to this command");
        assert!(super::answers_command("Kept model as Haiku 5.5", "sonnet") && super::answers_command("Set model to Sonnet 5.5", "sonnet") && !super::answers_command("Set model to Haiku 5.5", "sonnet") && super::answers_command("Set model to anything", ""));
        note_notice(p, "Set model to Sonnet 5.5 and saved".into()); assert_eq!(notice_for(p).as_deref(), Some("Set model to Sonnet 5.5 and saved"));
        assert!(!expecting_answer(p), "one answer per command");
        std::thread::sleep(std::time::Duration::from_millis(3)); note_send(p, "/model haiku"); assert!(expecting_answer(p), "a second command expects its own answer, even a line that reads the same");
    }
    #[test]
    fn a_setting_command_is_not_a_send_that_starts_a_run_and_a_prompt_or_a_model_running_command_is() {
        use super::{is_setting_command, note_send, sent_at};
        for t in ["/model haiku", "  /model  sonnet ", "/effort high", "/clear", "/context"] { assert!(is_setting_command(t), "{t}"); }
        for t in ["hi", "fix the build", "/compact", "/init", "/review", "/modelfoo", "please /model haiku"] { assert!(!is_setting_command(t), "{t}"); }
        note_send(987_001, "/model haiku"); assert!(sent_at(987_001).is_none(), "no send is noted, so the pane does not read as working");
        note_send(987_002, "hi"); assert!(sent_at(987_002).is_some());
    }
    #[test]
    fn heimdall_screens_the_open_and_local_models_and_stays_out_of_the_frontier_providers_unless_the_pane_says_otherwise() {
        use super::heimdall_wanted; use serde_json::json;
        for k in ["claude-code", "codex", "gemini", "grok", "cursor", "deepseek"] { assert!(!heimdall_wanted(k, &json!({})), "{k}"); }
        for k in ["opencode", "opencode-direct", "local", "agent", ""] { assert!(heimdall_wanted(k, &json!({})), "{k}"); }
        assert!(heimdall_wanted("claude-code", &json!({ "heimdall": true })), "a pane may ask for it"); assert!(!heimdall_wanted("opencode", &json!({ "heimdall": false })), "or refuse it");
        assert!(!heimdall_wanted("codex", &json!({ "heimdall": "yes" })), "only a real boolean counts");
    }
    use super::*;

    #[test]
    fn iso_parses_the_hooks_and_the_transcripts_format() {
        assert_eq!(iso_to_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(iso_to_ms("1970-01-01T00:00:01Z"), Some(1000));
        assert_eq!(iso_to_ms("2026-11-10T00:31:29.941Z"), Some(1_794_270_689_941));
        assert_eq!(iso_to_ms("2026-11-10T00:31:29.9Z"), Some(1_794_270_689_900));
        assert_eq!(iso_to_ms("2024-02-29T12:00:00.000Z").zip(iso_to_ms("2024-03-01T12:00:00.000Z")).map(|(a, b)| b - a), Some(86_400_000), "a leap day");
        for bad in ["", "yesterday", "2026-13-01T00:00:00Z", "2026-11-10 00:31:29Z", "2026-11-10T25:00:00Z", "2026-11-10T00:31:29."] { assert_eq!(iso_to_ms(bad), None, "{bad}"); }
    }

    #[test]
    fn a_run_starts_at_the_last_prompt_and_ends_at_the_next_answer() {
        let prompts = "{\"t\":\"2026-11-10T00:00:00.000Z\",\"prompt\":\"first\",\"session_id\":\"s1\"}\n{\"t\":\"2026-11-10T00:10:00.000Z\",\"prompt\":\"second ask\",\"session_id\":\"s1\"}\n";
        let answers = "{\"t\":\"2026-11-10T00:05:00.000Z\",\"text\":\"a\"}\n{\"t\":\"2026-11-10T00:12:30.500Z\",\"text\":\"b\"}\n";
        let w = run_window(prompts, answers).unwrap();
        assert_eq!((w.start_t.as_str(), w.end_t.as_deref(), w.prompt.as_str(), w.session.as_deref()), ("2026-11-10T00:10:00.000Z", Some("2026-11-10T00:12:30.500Z"), "second ask", Some("s1")));
    }
    #[test]
    fn a_prompt_with_no_answer_after_it_is_a_run_still_going() {
        let prompts = "{\"t\":\"2026-11-10T00:10:00.000Z\",\"prompt\":\"x\"}\n";
        let answers = "{\"t\":\"2026-11-10T00:05:00.000Z\",\"text\":\"an older answer\"}\n";
        assert_eq!(run_window(prompts, answers).unwrap().end_t, None);
        assert_eq!(run_window("", ""), None);
        assert_eq!(run_window("not json\n{\"no\":\"t\"}\n", "").map(|w| w.start_t), None, "lines without a time are not runs");
    }

    fn assistant(ts: &str, id: &str, usage: (i64, i64, i64, i64), content: Value) -> String {
        json!({ "type": "assistant", "timestamp": ts, "message": { "id": id, "model": "claude-sonnet-5-5",
            "usage": { "input_tokens": usage.0, "output_tokens": usage.1, "cache_read_input_tokens": usage.2, "cache_creation_input_tokens": usage.3 }, "content": content } }).to_string()
    }
    #[test]
    fn tokens_are_counted_once_per_message_and_only_inside_the_window() {
        let lines = vec![
            assistant("2026-11-10T00:09:59.000Z", "m0", (9, 9, 9, 9), json!([])),                                           // before the run
            assistant("2026-11-10T00:10:01.000Z", "m1", (2, 300, 0, 45_000), json!([{ "type": "text" }])),                  // the first block of m1
            assistant("2026-11-10T00:10:01.500Z", "m1", (2, 300, 0, 45_000), json!([{ "type": "tool_use", "id": "t1" }])),   // the same message again: not twice
            assistant("2026-11-10T00:10:05.000Z", "m2", (2, 200, 45_000, 4_000), json!([{ "type": "tool_use", "id": "t2" }, { "type": "tool_use", "id": "t1" }])),
            json!({ "type": "user", "timestamp": "2026-11-10T00:10:06.000Z" }).to_string(),
            "not json".to_string(),
            assistant("2026-11-10T00:20:00.000Z", "m3", (7, 7, 7, 7), json!([])),                                           // after the run
        ];
        let t = tokens_in_window(lines.iter().map(String::as_str), "2026-11-10T00:10:00.000Z", Some("2026-11-10T00:12:00.000Z"));
        assert_eq!(t, RunTokens { input: 4, output: 500, cache_read: 45_000, cache_write: 49_000, messages: 2, tools: 2, model: Some("claude-sonnet-5-5".into()) });
        // a run still going has no upper bound
        let open = tokens_in_window(lines.iter().map(String::as_str), "2026-11-10T00:10:00.000Z", None);
        assert_eq!(open.messages, 3);
    }
    #[test]
    fn a_transcript_with_no_usage_gives_zeros_not_a_panic() {
        assert_eq!(tokens_in_window(["{\"type\":\"assistant\",\"timestamp\":\"2026-11-10T00:10:01.000Z\",\"message\":{}}"].into_iter(), "2026-11-10T00:00:00.000Z", None).messages, 1);
        assert_eq!(tokens_in_window(std::iter::empty(), "x", None), RunTokens::default());
    }

    #[test]
    fn a_screen_that_is_waiting_on_the_person_is_blocked_and_one_that_is_not_is_not() {
        let dialog: Vec<String> = [" Accessing workspace:", " Quick safety check: Is this a project you created or one you trust? (Like your own code,", " ❯ No, exit", "   Yes, I trust this folder", " Enter to confirm · Esc to cancel"].iter().map(|s| s.to_string()).collect();
        assert!(blocked_from_screen(&dialog).unwrap().contains("trust"));
        let codex: Vec<String> = ["> You are in /x", "  Do you trust the contents of this directory?", "› 1. Yes, continue", "  Press enter to continue"].iter().map(|s| s.to_string()).collect();
        assert!(blocked_from_screen(&codex).is_some());
        let perm: Vec<String> = ["⏺ Bash(rm -rf node_modules)", " Do you want to proceed?", " ❯ 1. Yes"].iter().map(|s| s.to_string()).collect();
        assert_eq!(blocked_from_screen(&perm).as_deref(), Some("Do you want to proceed?"));
        let working: Vec<String> = ["⏺ Read src/a.ts", "✻ Thinking… (12s · esc to interrupt)", "", "> "].iter().map(|s| s.to_string()).collect();
        assert_eq!(blocked_from_screen(&working), None, "'esc to interrupt' is working, not waiting");
        assert_eq!(blocked_from_screen(&[]), None);
        // an answered dialog leaves its words behind; the prompt below it means the screen is no longer waiting
        let answered: Vec<String> = [" Do you want to proceed?", " ❯ 1. Yes", "", "ack: delete the build folder", "", "? for shortcuts", "> "].iter().map(|s| s.to_string()).collect();
        assert_eq!(blocked_from_screen(&answered), None);
        // but a dialog BELOW an old prompt still counts
        let again: Vec<String> = ["? for shortcuts", "> go", "⏺ Bash(rm -rf x)", " Do you want to proceed?", " ❯ 1. Yes"].iter().map(|s| s.to_string()).collect();
        assert_eq!(blocked_from_screen(&again).as_deref(), Some("Do you want to proceed?"));
        // only the last rows are read: a dialog far up the screen history is not a live one
        let mut old = vec![" Do you want to proceed?".to_string()];
        old.extend((0..20).map(|i| format!("line {i}")));
        assert_eq!(blocked_from_screen(&old), None);
    }

    #[test]
    fn a_path_the_ask_names_that_is_not_there_becomes_a_question_with_chips_and_a_clear_ask_is_sent() {
        let exists = |p: &str| p == "src/loki/shell/workspace-gate.tsx";
        let v = precheck_verdict("fix the picker in src/loki/shell/missing-file.tsx", exists);
        assert_eq!(v["verdict"], "ask", "{v}");
        let q = &v["questions"][0];
        assert!(q["text"].as_str().unwrap().contains("missing-file.tsx"));
        assert!(q["options"].as_array().unwrap().len() >= 2 && q["options"][0].as_str().unwrap().starts_with("create"), "the first chip is the default: {q}");
        assert!(q["id"].as_str().unwrap().starts_with("missing-path"));
        let ok = precheck_verdict("fix the picker in src/loki/shell/workspace-gate.tsx", exists);
        assert_eq!(ok["verdict"], "send"); assert!(ok["notes"].as_array().unwrap().len() >= 1);
        assert_eq!(precheck_verdict("explain how the router decides", |_| false)["verdict"], "send", "an ask that names nothing is not blocked on a fact");
        let notes = precheck_verdict("reply with the single word pong", |_| false)["notes"].to_string();
        assert!(!notes.contains("editor-s") && !notes.contains("editor-l"), "the card speaks in words, not the router's keys: {notes}");
    }

    #[test]
    fn the_composers_check_stands_for_the_same_text_for_two_minutes_and_for_nothing_else() {
        let sent = Some((1_000_000, text_hash("fix the picker in src/a.tsx")));
        assert!(checked_by_composer(sent, "  fix the picker in src/a.tsx \n", 1_030_000), "whitespace around the paste is not a different ask");
        assert!(!checked_by_composer(sent, "fix the picker in src/b.tsx", 1_030_000), "another ask has not been checked");
        assert!(!checked_by_composer(sent, "fix the picker in src/a.tsx", 1_000_000 + CHECKED_FOR_MS), "and the check expires");
        assert!(!checked_by_composer(None, "fix the picker in src/a.tsx", 1_030_000), "typed straight into the terminal: nobody has checked it");
    }

    #[test]
    fn a_hook_pair_that_records_the_same_prompt_twice_is_one_prompt_and_a_torn_line_is_skipped() {
        let p = concat!(
            "{\"t\":\"2026-11-10T11:53:36.617Z\",\"prompt\":\"describe it\",\"session_id\":\"s\"}\n",
            "{\"t\":\"2026-11-10T11:53:36.681Z\",\"prompt\":\"describe it\",\"session_id\":\"s\",\"cost_usd\":0.0}\n",
            "{{\"t\":\"\"t2026-torn\n",
            "{\"t\":\"2026-11-10T11:54:22.205Z\",\"prompt\":\"describe it\",\"session_id\":\"s\"}\n");
        let v: Vec<Value> = jsonl(p).collect();
        assert_eq!(v.len(), 2, "the pair is one, the torn line is gone, the same words a minute later are a new prompt: {v:?}");
        assert_eq!(v[0]["cost_usd"], json!(0.0), "the copy that carries the cost is the one kept");
        // answers collapse the same way, and a different answer 50 ms later is not a duplicate
        let a = "{\"t\":\"2026-11-10T11:53:49.614Z\",\"text\":\"done\",\"session_id\":\"s\"}\n{\"t\":\"2026-11-10T11:53:49.615Z\",\"text\":\"done\",\"session_id\":\"s\"}\n{\"t\":\"2026-11-10T11:53:49.700Z\",\"text\":\"other\",\"session_id\":\"s\"}\n";
        assert_eq!(jsonl(a).count(), 2);
    }

    #[test]
    fn a_turn_the_hook_never_saw_end_is_over_when_the_transcript_says_so() {
        let asst = |t: &str, stop: Option<&str>| json!({ "type": "assistant", "timestamp": t, "message": { "stop_reason": stop, "content": [] } }).to_string();
        let user = |t: &str, text: &str| json!({ "type": "user", "timestamp": t, "message": { "content": [{ "type": "text", "text": text }] } }).to_string();
        let from = "2026-11-10T11:55:29.000Z";
        // working: the last assistant message is asking for a tool
        let going = [asst("2026-11-10T11:55:30.000Z", Some("tool_use"))].join("\n");
        assert_eq!(turn_ended_in_transcript(going.lines(), from), None);
        // over: a final message, and a later tool-using step of an EARLIER turn does not count
        let done = [asst("2026-11-10T11:50:00.000Z", Some("end_turn")), asst("2026-11-10T11:55:30.000Z", Some("tool_use")), asst("2026-11-10T11:55:40.000Z", Some("end_turn"))].join("\n");
        assert_eq!(turn_ended_in_transcript(done.lines(), from).as_deref(), Some("2026-11-10T11:55:40.000Z"));
        // a final message and then more work in the same turn (a follow-up tool call) is going again
        let again = [asst("2026-11-10T11:55:40.000Z", Some("end_turn")), asst("2026-11-10T11:55:41.000Z", None)].join("\n");
        assert_eq!(turn_ended_in_transcript(again.lines(), from), None);
        // Esc: no Stop hook, the transcript says the request was interrupted
        let esc = [asst("2026-11-10T11:55:30.000Z", Some("tool_use")), user("2026-11-10T11:55:33.000Z", "[Request interrupted by user for tool use]")].join("\n");
        assert_eq!(turn_ended_in_transcript(esc.lines(), from).as_deref(), Some("2026-11-10T11:55:33.000Z"));
        assert_eq!(turn_ended_in_transcript("garbage\n{".lines(), from), None);
    }

    #[test]
    fn a_run_costs_what_the_engine_says_when_it_has_said_it_and_the_price_list_otherwise() {
        let w = |end: Option<&str>, before: Option<f64>| RunWindow { start_t: "2026-11-10T08:53:18.816Z".into(), end_t: end.map(String::from), session: Some("s".into()), prompt: String::new(), cost_before: before };
        let st = |t: &str, c: f64| json!({ "t": t, "cost_usd": c });
        let done = Some("2026-11-10T08:53:21.284Z");
        // the real Haiku 5.5 run: session cost 0 -> 0.009443, ticked 300 ms after the answer
        assert_eq!(engine_cost(&st("2026-11-10T08:53:21.594Z", 0.009443), &w(done, Some(0.0))), Some(0.009443));
        // a later run in the same session is the difference, not the total
        let d = engine_cost(&st("2026-11-10T08:53:21.594Z", 0.50), &w(done, Some(0.42))).unwrap(); assert!((d - 0.08).abs() < 1e-9);
        assert_eq!(engine_cost(&st("2026-11-10T08:53:21.000Z", 0.50), &w(done, Some(0.42))), None, "the status line has not ticked since the answer: not final");
        assert_eq!(engine_cost(&st("2026-11-10T08:53:21.594Z", 0.50), &w(None, Some(0.42))), None, "a run still going has no final cost");
        assert_eq!(engine_cost(&st("2026-11-10T08:53:21.594Z", 0.10), &w(done, Some(0.42))), None, "a resumed session starts its meter over: never a negative cost");
        assert_eq!(engine_cost(&st("2026-11-10T08:53:21.594Z", 0.50), &w(done, None)), None, "no starting figure, no difference");
        // the first prompt of a session starts at zero even when the hook saw no usage.json yet
        let p = "{\"t\":\"2026-11-10T08:53:18.816Z\",\"prompt\":\"a\",\"session_id\":\"s\"}\n";
        assert_eq!(run_window(p, "").unwrap().cost_before, Some(0.0));
        let two = format!("{p}{{\"t\":\"2026-11-10T09:00:00.000Z\",\"prompt\":\"b\",\"session_id\":\"s\"}}\n");
        assert_eq!(run_window(&two, "").unwrap().cost_before, None, "a second prompt without the hook's figure is unknown, not zero");
    }

    #[test]
    fn the_chat_sandbox_is_told_from_a_real_box() {
        assert!(is_sandbox(&json!({ "settings_json": "{\"chat\":true,\"min\":true,\"title\":\"Claude Code\"}", "worktree_path": "/home/u/.antiloki/chats/ministore-e2a5f8a7/58" })));
        assert!(is_sandbox(&json!({ "settings_json": "{}", "worktree_path": "/x/chats/ws/3" })), "a chat directory is a sandbox whatever its settings say");
        assert!(!is_sandbox(&json!({ "settings_json": "{\"title\":\"Claude Code\"}", "worktree_path": "/home/u/.antiloki/worktrees/cockpit-15-0" })));
        assert!(!is_sandbox(&json!({})) && !is_sandbox(&Value::Null));
    }

    #[test]
    fn a_pane_that_was_just_sent_a_prompt_is_working_until_the_engine_records_a_run_after_it() {
        let sent = 1_794_300_798_816_i64;   // 08:53:18.816
        let idle = |started: Option<&str>| json!({ "known": true, "working": false, "blocked": Value::Null, "run": started.map_or(Value::Null, |t| json!({ "started_t": t })) });
        // nothing recorded yet (Codex's first turn), or only the run before: working
        assert_eq!(with_pending_send(idle(None), Some(sent), sent + 3_000)["working"], true);
        let stale = with_pending_send(idle(Some("2026-11-10T08:00:00.000Z")), Some(sent), sent + 3_000);
        assert_eq!((stale["working"].clone(), stale["run"].clone()), (json!(true), Value::Null), "the run before this prompt is not this run");
        // the engine's own record has the new run (it started a few ms after the send): believe the record
        assert_eq!(with_pending_send(idle(Some("2026-11-10T08:53:18.900Z")), Some(sent), sent + 3_000)["working"], false);
        // a send long ago, no send, an unknown engine and a blocked pane are all left as they were
        assert_eq!(with_pending_send(idle(None), Some(sent), sent + 120_000)["working"], false);
        assert_eq!(with_pending_send(idle(None), None, sent)["working"], false);
        assert_eq!(last_model(&json!([{ "model": "claude-haiku-5-5" }, { "role": "user" }])).as_deref(), Some("Haiku 5.5"));
        assert_eq!(last_model(&json!([{ "model": "<synthetic>" }])), None);
        // the answer in the conversation ends the wait
        let line = |at: &str, stop: &str| format!(r#"{{"type":"assistant","timestamp":"{at}","message":{{"stop_reason":"{stop}"}}}}"#);
        assert!(claude_answered(&line("2026-11-10T08:53:19.000Z", "end_turn"), sent));
        assert!(!claude_answered(&line("2026-11-10T08:53:19.000Z", "tool_use"), sent), "still at work");
        assert!(!claude_answered(&line("2026-11-10T08:00:00.000Z", "end_turn"), sent), "the answer before this message");
        let at = iso_to_ms("2026-11-10T08:53:19.000Z").unwrap();
        let said = json!([{ "role": "assistant", "t": "2026-11-10T08:53:19.000Z", "text": "hi", "tools": [] }]);
        assert!(turns_answered(&said, sent, at + 9_000)); assert!(!turns_answered(&said, sent, at + 2_000), "it may still be writing");
        assert!(turns_answered(&json!([{ "role": "assistant", "t": at, "text": "hi" }]), sent, at + 9_000), "OpenCode writes the time as a number");
        assert!(!turns_answered(&json!([{ "role": "user", "t": "2026-11-10T08:53:19.000Z", "text": "hi" }]), sent, at + 9_000));
        assert_eq!(with_pending_send(json!({ "known": false, "blocked": Value::Null }), Some(sent), sent + 1_000).get("working"), None);
        assert_eq!(with_pending_send(json!({ "known": true, "working": false, "blocked": "Do you want to proceed?", "run": Value::Null }), Some(sent), sent + 1_000)["working"], false);
    }

    #[test]
    fn cursors_footer_gives_the_model_and_the_context() {
        let r = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(cursor_footer(&r(&["  \u{2192} Add a follow-up", " \u{2580}\u{2580}\u{2580}", "  Plan (shift+tab to cycle)", "  Grok 4.7 256K Medium \u{b7} 7.6%", "  ~/x \u{b7} y"])), Some(("Grok 4.7 256K Medium".into(), Some(7.6))));
        assert_eq!(cursor_footer(&r(&[" \u{2580}\u{2580}", "  Auto", "  ~/x"])), Some(("Auto".into(), None)));
        assert_eq!(cursor_footer(&r(&["  done", "", "  \u{2192} Add a follow-up", " ", "", "  Grok 4.6 \u{b7} 11.9%", "  ~/p/box \u{b7} box", ""])), Some(("Grok 4.6".into(), Some(11.9))), "2026.11: no drawn edge");
        assert_eq!(cursor_footer(&r(&["  \u{2192} an arrow in an answer", "a", "b", "c", "d", "  ~/p \u{b7} x"])), None);
        assert_eq!(cursor_footer(&r(&["building"])), None);
    }

    #[test]
    fn a_session_file_is_parsed_again_only_when_it_changes() {
        let f = std::env::temp_dir().join(format!("antiloki-parsed-{}.jsonl", std::process::id())); std::fs::write(&f, "a\n").unwrap();
        let n = std::cell::Cell::new(0); let count = |tag: &str| parsed(tag, &f, |t| { n.set(n.get() + 1); t.lines().count() });
        assert_eq!((count("x"), count("x"), n.get()), (1, 1, 1)); assert_eq!((count("y"), n.get()), (1, 2), "another thing made of the same file");
        std::fs::write(&f, "a\nb\n").unwrap(); assert_eq!((count("x"), n.get()), (2, 3)); let _ = std::fs::remove_file(f);
    }
    #[test]
    fn a_new_claude_panes_model_is_read_from_its_banner_before_it_has_answered() {
        let rows: Vec<String> = [" ▐▛███▛█Claude Code v2.1.332", "▝▜██████▀Haiku 5.5 with medium effort · Claude Pro", "  ▘▘ ▝▝    ~/proj", "> try Opus 4"].iter().map(|s| s.to_string()).collect();
        assert_eq!(claude_model(&Value::Null, None, Some(&rows)), json!("Haiku 5.5"));
        assert_eq!(claude_model(&json!("Sonnet 5.5"), None, Some(&rows)), json!("Sonnet 5.5"));
        assert_eq!(claude_model(&Value::Null, Some("claude-opus-5-5-20261001"), None), json!("Opus 5.5"));
        assert_eq!(claude_model(&Value::Null, None, Some(&rows[3..])), Value::Null, "a name in the conversation is not the pane's model");
    }
    #[test]
    fn usage_for_a_pane_with_files_reads_the_run_and_prices_it() {
        let root = std::env::temp_dir().join(format!("antiloki-cowork-usage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".antiloki")).unwrap();
        std::fs::write(root.join(".antiloki/usage.json"), json!({ "model": "Sonnet 5.5", "context": { "used_pct": 38 }, "cost_usd": 2.84, "limits": [{ "label": "5h", "used_pct": 61 }] }).to_string()).unwrap();
        std::fs::write(root.join(".antiloki/prompts.jsonl"), "{\"t\":\"2026-11-10T00:10:00.000Z\",\"prompt\":\"do it\",\"session_id\":\"abc\"}\n").unwrap();
        let v = usage_for(&root, "claude-code", None);
        assert_eq!(v["known"], true); assert_eq!(v["working"], true, "a prompt with no answer is a run going");
        assert_eq!(v["session_cost"], 2.84); assert_eq!(v["context"]["used_pct"], 38); assert_eq!(v["limits"][0]["label"], "5h");
        assert_eq!(v["run"]["transcript"], false, "no transcript was found for that session");
        std::fs::write(root.join(".antiloki/answers.jsonl"), "{\"t\":\"2026-11-10T00:14:54.800Z\",\"text\":\"done\"}\n").unwrap();
        let v = usage_for(&root, "claude-code", None);
        assert_eq!(v["working"], false); assert_eq!(v["run"]["ms"], 294_800);
        // another engine: honest about having no reader
        let o = usage_for(&root, "codex", None);
        assert_eq!((o["known"].clone(), o["reason"].is_string()), (json!(false), true));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_layout_keeps_only_the_panes_this_workspace_has_and_collapses_what_that_leaves() {
        let leaf = |id: &str, kind: &str, pane: i64| json!({ "type": "leaf", "pane": { "id": id, "kind": kind, "paneId": pane, "sessionId": 3 } });
        let layout = json!({ "root": { "type": "split", "dir": "row", "sizes": [0.5, 0.3, 0.2], "children": [leaf("claude-code-27", "engine", 27), json!({ "type": "split", "dir": "col", "sizes": [0.5, 0.5], "team": "k", "children": [leaf("claude-code-11", "engine", 11), leaf("claude-code-12", "engine", 12)] }), leaf("shell-9", "shell", 900_001)] }, "focus": "claude-code-27", "zoom": "claude-code-11" });
        let rows: std::collections::HashMap<i64, i64> = [(11, 3), (12, 3), (27, 8)].into_iter().collect();     // pane 27 here is in session 8: the layout's 27 is in session 3, another workspace's
        let l = prune_layout(&layout, &rows);
        assert_eq!(l["root"]["children"].as_array().unwrap().len(), 2, "pane 27 is another workspace's: gone ({l})"); assert_eq!(l["root"]["children"][0]["team"], "k", "the team block is whole");
        assert_eq!(l["root"]["children"][1]["pane"]["id"], "shell-9", "a shell has no row and stays"); let sz: f64 = l["root"]["sizes"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).sum(); assert!((sz - 1.0).abs() < 1e-9);
        assert_eq!(l["focus"], "claude-code-11", "focus moves to a pane that is there"); assert_eq!(l["zoom"], "claude-code-11");
        let only_foreign = prune_layout(&layout, &std::collections::HashMap::new()); assert_eq!(only_foreign["root"]["type"], "leaf", "a split left with one child collapses into it"); assert_eq!(only_foreign["root"]["pane"]["id"], "shell-9");
        assert!(prune_layout(&json!({ "root": leaf("claude-code-5", "engine", 5), "focus": "claude-code-5" }), &rows)["root"].is_null(), "nothing of this workspace's: an empty layout");
        let w = leaf("w", "worker", 3); assert!(prune_layout(&json!({ "root": w }), &rows)["root"].is_object(), "a delegated worker's pane is derived, not stored");
    }
}
