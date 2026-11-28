//! SAGAS FROM AGENT BOXES (RFC 0007 §17.9). An agent in a terminal pane is the engine's own CLI, so nothing it does passes through antiloki's hands. What antiloki has is what its hooks
//! leave in the box (`.antiloki/prompts.jsonl`, `answers.jsonl`, `trace.jsonl`: every prompt, every final answer, every tool call) and the engine's own transcript. This turns them
//! into sagas, the way `work.rs` turns goal runs into sagas: by SYNC ON READ, idempotent, nothing written twice.
//!
//!   one SAGA per EPISODE of an agent pane (RFC 0007 §17.11): an episode is the stretch of a pane's work between two consolidations, so the pane's work is a timeline of sagas on the workspace.
//!     The open stretch is a live saga (`doing`); consolidating closes it (`done`), gives it the model's title and summary, and the next prompts go into the next one.
//!     Its deterministic METADATA (pane name and id, engine, model, tokens, price, time, tools used, files touched) is stored with it.
//!   one TASK per prompt in it: `doing` until its answer, then `done`, with its time, tokens and cost
//!   its SUBTASKS are the agent's own to-do list (the last TodoWrite of the turn): pending / in progress / completed
//!   its TRAIL is the stack trace of every tool call (`tool` events: what ran, on what, whether it worked)
//!
//! Every row has a key made from what happened (the session, the prompt's time, the tool call's id), so two hooks recording the same call, or the screen reading ten times a second,
//! change nothing. Pure over strings and a connection; the handler that finds the boxes is `sync_project`.
use crate::cowork::{claude_transcript, iso_to_ms, jsonl_values, said_in_window, tokens_in_window, turn_ended_in_transcript};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

const TRAIL_PER_TASK: usize = 400;

#[derive(Debug, Clone, PartialEq)]
pub struct Tool { pub t_ms: i64, pub id: String, pub tool: String, pub summary: String, pub outcome: String, pub error: String }
#[derive(Debug, Clone, PartialEq)]
pub struct Turn { pub start_ms: i64, pub end_ms: Option<i64>, pub prompt: String, pub answer: String, pub tokens: i64, pub usd: f64, pub usage: Usage, pub screen: String, /** what the model said before its conclusion, in order */ pub said: Vec<(i64, String)>, pub tools: Vec<Tool>, pub todos: Vec<(String, String)> }
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Usage { pub input: i64, pub output: i64, pub cache_read: i64, pub cache_write: i64 }
#[derive(Debug, Clone, PartialEq)]
pub struct BoxSession { pub session: String, pub agent: String, pub engine: String, pub model: String, pub turns: Vec<Turn> }

/// One agent box of the project: the pane's name, its engine, where it lives, and the pane's id.
#[derive(Debug, Clone, PartialEq)]
pub struct BoxRef { pub agent: String, pub engine: String, pub dir: std::path::PathBuf, pub pane_id: i64, /** the folder is shared with other agents: its files are not this agent's alone, so nothing of git's is read from it as this agent's work */ pub shared: bool }
impl BoxRef {
    /// Where its prompts, answers, trace and episodes are kept.
    pub fn records(&self) -> std::path::PathBuf { if self.shared { crate::shared_folder::records(&self.dir.to_string_lossy(), self.pane_id) } else { self.dir.join(".antiloki") } }
}
/// How an episode ended: when, and what the model (or, failing it, the deterministic fallback) called it. One line of `<box>/.antiloki/episodes.jsonl`.
#[derive(Debug, Clone, PartialEq)]
pub struct Boundary { pub n: i64, pub end_ms: i64, pub title: String, pub summary: String, /** model | deterministic */ pub source: String, pub model: String, /** you | pane-close */ pub by: String }
/// A stretch of one pane's work: its turns, and how it ended if it did.
#[derive(Debug, Clone, PartialEq)]
pub struct Episode { pub n: i64, pub pane_id: i64, pub agent: String, pub engine: String, pub model: String, pub closed: Option<Boundary>, pub turns: Vec<Turn> }

/// The episodes a box has closed, from its `episodes.jsonl` (a torn or foreign line is skipped), by number. Pure.
pub fn boundaries_of(text: &str) -> Vec<Boundary> {
    let mut v: Vec<Boundary> = jsonl_values(text).filter_map(|b| Some(Boundary { n: b["n"].as_i64()?, end_ms: ms_of(&b)?, title: b["title"].as_str().unwrap_or("").to_string(), summary: b["summary"].as_str().unwrap_or("").to_string(),
        source: b["source"].as_str().unwrap_or("deterministic").to_string(), model: b["model"].as_str().unwrap_or("").to_string(), by: b["by"].as_str().unwrap_or("you").to_string() })).collect();
    v.sort_by_key(|b| b.n); v.dedup_by_key(|b| b.n); v
}
/// A box's sessions cut into episodes: the turns in time order, those up to a boundary belonging to that episode, those after the last boundary to the open one. A boundary with no turns left
/// (it was written twice, or the files were trimmed) is skipped, and the numbers go on from the last boundary. Pure.
pub fn episodes_of(sessions: &[BoxSession], pane_id: i64, boundaries: &[Boundary]) -> Vec<Episode> {
    let mut turns: Vec<Turn> = sessions.iter().flat_map(|s| s.turns.clone()).collect(); turns.sort_by_key(|t| t.start_ms);
    let (agent, engine) = sessions.first().map(|s| (s.agent.clone(), s.engine.clone())).unwrap_or_default();
    let model = sessions.iter().rev().map(|s| s.model.clone()).find(|m| !m.is_empty()).unwrap_or_default();
    let mut out = vec![];
    for b in boundaries {
        let (mine, rest): (Vec<Turn>, Vec<Turn>) = turns.into_iter().partition(|t| t.start_ms <= b.end_ms); turns = rest;
        if !mine.is_empty() { out.push(Episode { n: b.n, pane_id, agent: agent.clone(), engine: engine.clone(), model: model.clone(), closed: Some(b.clone()), turns: mine }); }
    }
    if !turns.is_empty() { out.push(Episode { n: boundaries.last().map_or(0, |b| b.n) + 1, pane_id, agent, engine, model, closed: None, turns }); }
    out
}
/// What an episode is, deterministically: its pane, its model, its meter, the tools it used and the files it touched. Pure.
pub fn episode_meta(e: &Episode) -> Value {
    let mut tools: std::collections::BTreeMap<String, i64> = Default::default(); let mut mcp: std::collections::BTreeMap<String, i64> = Default::default(); let mut files: Vec<String> = vec![]; let (mut refused, mut errors) = (0, 0);
    for t in e.turns.iter().flat_map(|t| t.tools.iter()) {
        match t.tool.strip_prefix("mcp__") { Some(rest) => { let (srv, name) = rest.split_once("__").unwrap_or(("", rest)); *mcp.entry(if srv.is_empty() { name.to_string() } else { format!("{srv}.{name}") }).or_default() += 1; } None => *tools.entry(t.tool.clone()).or_default() += 1 }
        if matches!(t.tool.as_str(), "Edit" | "Write" | "MultiEdit" | "NotebookEdit") && !t.summary.is_empty() && !files.contains(&t.summary) && t.outcome == "ok" { files.push(t.summary.clone()); }
        match t.outcome.as_str() { "refused" => refused += 1, "error" => errors += 1, _ => {} }
    }
    let sum = |f: fn(&Usage) -> i64| e.turns.iter().map(|t| f(&t.usage)).sum::<i64>();
    let first = e.turns.first().map_or(0, |t| t.start_ms); let last = e.turns.iter().map(|t| t.end_ms.unwrap_or(t.start_ms)).max().unwrap_or(first);
    // a saga is not named by the brief antiloki typed (a specialist's "You are …, a specialist"): by the first ask of the person, else by what was concluded
    let first_prompt = e.turns.iter().map(|t| crate::work::strip_paste_markup(&t.prompt)).find(|p| !crate::chat_pane::is_internal_brief(p)).map(|p| p.chars().take(600).collect::<String>()).or_else(|| e.turns.iter().find(|t| !t.answer.trim().is_empty()).map(|t| summary_title(&t.answer, &t.prompt))).unwrap_or_default();
    json!({ "kind": "episode", "n": e.n, "pane_id": e.pane_id, "pane": e.agent, "engine": e.engine, "model": e.model, "turns": e.turns.len(), "open": e.closed.is_none(),
            "tokens": { "in": sum(|u| u.input), "out": sum(|u| u.output), "cache_read": sum(|u| u.cache_read), "cache_write": sum(|u| u.cache_write) },
            "usd": (e.turns.iter().map(|t| t.usd).sum::<f64>() * 10000.0).round() / 10000.0, "ms": e.turns.iter().map(|t| t.end_ms.map_or(0, |x| (x - t.start_ms).max(0))).sum::<i64>(),
            "span_ms": [first, last], "tools": tools, "mcp": mcp, "files": files, "refused": refused, "errors": errors,
            "summary_source": e.closed.as_ref().map_or("", |b| b.source.as_str()), "summary_model": e.closed.as_ref().map_or("", |b| b.model.as_str()), "closed_by": e.closed.as_ref().map_or("", |b| b.by.as_str()),
            // what the saga was called before it was closed: only a title that still reads so is replaced by the model's (a title the person changed is theirs)
            "auto_title": crate::work::short_title(&first_prompt, 100), "auto_objective": first_prompt })
}

fn ms_of(v: &Value) -> Option<i64> { v["t"].as_str().and_then(iso_to_ms) }

/// The sessions in one box, from the files its hooks wrote (and the engine's transcript, which may be empty). Pure.
pub fn sessions_from_files(agent: &str, engine: &str, prompts: &str, answers: &str, trace: &str, transcript: &str) -> Vec<BoxSession> {
    let ps: Vec<Value> = jsonl_values(prompts).filter(|v| v["prompt"].is_string() && ms_of(v).is_some()).collect();
    let ans: Vec<Value> = jsonl_values(answers).filter(|v| ms_of(v).is_some()).collect();
    let tr: Vec<Value> = jsonl_values(trace).filter(|v| ms_of(v).is_some()).collect();
    let mut order: Vec<String> = vec![]; for p in &ps { let s = p["session_id"].as_str().unwrap_or("").to_string(); if !order.contains(&s) { order.push(s); } }
    order.into_iter().map(|session| {
        let mine: Vec<&Value> = ps.iter().filter(|p| p["session_id"].as_str().unwrap_or("") == session).collect();
        let mut model = String::new();
        let turns = mine.iter().enumerate().map(|(i, p)| {
            let start = ms_of(p).unwrap_or(0); let next = mine.get(i + 1).and_then(|n| ms_of(n)).unwrap_or(i64::MAX);
            let a = ans.iter().find(|a| ms_of(a).map_or(false, |t| t >= start && t < next) && (a["session_id"].as_str().unwrap_or("") == session || a["session_id"].is_null()));
            let mut end = a.and_then(|a| ms_of(a));
            // the hook never saw this turn end (a torn answer line, or Esc, which fires no Stop hook): the engine's own transcript may say it did
            if end.is_none() && i + 1 == mine.len() { end = turn_ended_in_transcript(transcript.lines(), p["t"].as_str().unwrap_or("")).as_deref().and_then(iso_to_ms); }
            let in_turn = |t: &Value| ms_of(t).map_or(false, |m| m >= start && m < next) && (t["session_id"].as_str().unwrap_or("") == session || t["session_id"].is_null());
            let tools: Vec<Tool> = tr.iter().filter(|t| in_turn(t)).enumerate().map(|(n, t)| Tool {
                t_ms: ms_of(t).unwrap_or(0), id: t["tool_use_id"].as_str().map(String::from).unwrap_or_else(|| format!("{}-{n}", t["t"].as_str().unwrap_or(""))),
                tool: t["tool"].as_str().unwrap_or("").to_string(), summary: t["summary"].as_str().unwrap_or("").to_string(), outcome: t["outcome"].as_str().unwrap_or("ok").to_string(), error: t["error"].as_str().unwrap_or("").to_string() }).collect();
            let todos: Vec<(String, String)> = tr.iter().filter(|t| in_turn(t) && t["tool"] == "TodoWrite").last().and_then(|t| t["todos"].as_array().cloned()).unwrap_or_default().iter()
                .map(|t| (t["content"].as_str().unwrap_or("").to_string(), t["status"].as_str().unwrap_or("pending").to_string())).filter(|(c, _)| !c.is_empty()).collect();
            let t = tokens_in_window(transcript.lines(), p["t"].as_str().unwrap_or(""), end.map(|_| a.and_then(|a| a["t"].as_str())).flatten());
            if model.is_empty() { if let Some(m) = &t.model { model = m.clone(); } }
            // what the turn cost: the engine's own meter (the session cost the hook saw at this prompt and at the next), else the tokens at the price list
            let cost_at = |v: &Value| v["cost_usd"].as_f64();
            // the engine's own meter first: the session cost at this turn's answer minus the one its prompt saw (when it moved), else at the next prompt; the price list only when the meter said nothing
            let usd = match (cost_at(p), a.and_then(|a| cost_at(a)), mine.get(i + 1).and_then(|n| cost_at(n))) {
                (Some(b0), Some(b1), _) if b1 > b0 => b1 - b0,
                (Some(b0), _, Some(b1)) if b1 > b0 => b1 - b0,
                _ => crate::cli::price_tokens(crate::cli::Engine::ClaudeCode, Some(&model), t.input, t.output, t.cache_read, t.cache_write),
            };
            let answer = a.and_then(|a| a["text"].as_str()).unwrap_or("").to_string();
            // what the model said on the way (every text block of the window but the conclusion itself, which is the answer)
            let said: Vec<(i64, String)> = said_in_window(transcript.lines(), p["t"].as_str().unwrap_or(""), end.map(|_| a.and_then(|a| a["t"].as_str())).flatten()).into_iter()
                .filter(|(_, x)| x.trim() != answer.trim()).filter_map(|(ts, x)| Some((iso_to_ms(&ts)?, x))).collect();
            Turn { start_ms: start, end_ms: end, prompt: p["prompt"].as_str().unwrap_or("").to_string(), answer, screen: p["screen"].as_str().unwrap_or("").to_string(), said,
                   tokens: t.input + t.output + t.cache_read + t.cache_write, usd: if t.messages == 0 { 0.0 } else { usd }, usage: Usage { input: t.input, output: t.output, cache_read: t.cache_read, cache_write: t.cache_write }, tools, todos }
        }).collect();
        BoxSession { session, agent: agent.to_string(), engine: engine.to_string(), model, turns }
    }).collect()
}

/// A line of markdown as plain words: heading marks, list bullets, emphasis, backticks and links dropped.
fn plain(line: &str) -> String {
    let mut t = line.trim().trim_start_matches(|c: char| c == '#' || c == '>' || c == '-' || c == '*' || c == '+' || c.is_whitespace()).to_string();
    t = t.replace("**", "").replace("__", "").replace('`', "");
    // [text](url) -> text
    while let (Some(a), Some(b)) = (t.find('['), t.find("](")) { if b < a { break; } let Some(c) = t[b..].find(')') else { break }; t = format!("{}{}{}", &t[..a], &t[a + 1..b], &t[b + c + 1..]); }
    t.trim().trim_matches('*').trim().to_string()
}
/// A TITLE FOR WHAT THE MODEL CONCLUDED: its first heading, else its first sentence, else (nothing said yet) what was asked. Deterministic: the same words give the same title.
pub fn summary_title(answer: &str, prompt: &str) -> String {
    let heading = answer.lines().map(str::trim).find(|l| l.starts_with('#')).map(plain).filter(|h| h.chars().filter(|c| c.is_alphabetic()).count() >= 3);
    // a title needs words: a line of only numbers or symbols ("100", "---") says nothing about what was concluded
    let words = |l: &str| l.chars().filter(|c| c.is_alphabetic()).count() >= 3;
    let first = answer.lines().map(plain).find(|l| words(l) && !l.starts_with("---"));
    let pick = heading.or_else(|| first.map(|l| { let end = l.char_indices().find(|(i, c)| matches!(c, '.' | '!' | '?' | ':') && l[i + 1..].starts_with(|n: char| n.is_whitespace() || n == '\0') ).map(|(i, c)| if c == ':' { i } else { i + 1 }); end.map_or(l.clone(), |e| l[..e].to_string()) }));
    let t = pick.unwrap_or_else(|| first_line(prompt, 200));
    let t: String = t.trim().trim_end_matches(':').chars().take(100).collect();
    if t.is_empty() { first_line(prompt, 100) } else { t }
}
/// THE DESCRIPTION: what the model said while it worked, in order, then its conclusion. The model's own words, unchanged.
pub fn body_of(t: &Turn) -> String {
    let mut parts: Vec<String> = t.said.iter().map(|(_, x)| x.trim().to_string()).collect();
    if !t.answer.trim().is_empty() { if !parts.is_empty() { parts.push("— conclusion —".to_string()); } parts.push(t.answer.trim().to_string()); }
    parts.join("\n\n").chars().take(8000).collect()
}
fn k(n: i64) -> String { if n >= 1_000_000 { format!("{:.1}M", n as f64 / 1e6) } else if n >= 1_000 { format!("{:.1}k", n as f64 / 1e3) } else { n.to_string() } }
/// The turn's meter in a line: model, tokens in and out, cache, price, time.
pub fn usage_line(t: &Turn, model: &str, ms: i64) -> String {
    let mut p = vec![];
    if !model.is_empty() { p.push(model.to_string()); }
    p.push(format!("{} in · {} out", k(t.usage.input), k(t.usage.output)));
    if t.usage.cache_read + t.usage.cache_write > 0 { p.push(format!("{} cached", k(t.usage.cache_read + t.usage.cache_write))); }
    if t.usd > 0.0 { p.push(format!("${:.4}", t.usd)); }
    if ms > 0 { p.push(if ms >= 60_000 { format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000) } else { format!("{:.1}s", ms as f64 / 1000.0) }); }
    p.join(" · ")
}
/// An MCP call is named `mcp__<server>__<tool>`: it reads as `server.tool`, and is its own kind of line in the trail, so antiloki's own tools stand out from the engine's.
pub fn tool_event(tool: &Tool) -> (&'static str, String) {
    let tail = match tool.outcome.as_str() { "ok" => String::new(), o => format!("  [{o}{}]", if tool.error.is_empty() { String::new() } else { format!(": {}", tool.error) }) };
    if tool.outcome == "refused" { return ("refused", format!("{} {}{tail}", tool.tool, tool.summary)); }
    if let Some(rest) = tool.tool.strip_prefix("mcp__") { let (srv, name) = rest.split_once("__").unwrap_or(("", rest)); return ("mcp", format!("{}{} {}{tail}", if srv.is_empty() { String::new() } else { format!("{srv}.") }, name, tool.summary)); }
    ("tool", format!("{} {}{tail}", tool.tool, tool.summary))
}
fn fnv(s: &str) -> String { let mut h: u64 = 0xcbf29ce484222325; for b in s.bytes() { h ^= b as u64; h = h.wrapping_mul(0x100000001b3); } format!("{h:08x}") }

fn first_line(s: &str, n: usize) -> String { s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(n).collect() }
fn fingerprint(s: &Episode) -> String {
    let last = s.turns.last();
    format!("{}|{}|{}|{}|{}|{}|{}", s.turns.len(), last.map_or(0, |t| t.start_ms), last.and_then(|t| t.end_ms).unwrap_or(0), s.turns.iter().map(|t| t.tools.len()).sum::<usize>(), last.map_or(0, |t| t.todos.iter().map(|(c, st)| c.len() + st.len()).sum::<usize>()),
        s.turns.iter().map(|t| t.tokens + t.said.len() as i64 + t.screen.len() as i64 + t.answer.len() as i64).sum::<i64>(), s.closed.as_ref().map_or(String::new(), |b| format!("{}:{}:{}:{}", b.end_ms, b.title.len(), b.summary.len(), b.source)))
}
fn todo_status(s: &str) -> &'static str { match s { "completed" | "done" => "done", "in_progress" | "doing" => "doing", _ => "todo" } }

/// Put the episodes into the saga store. Returns how many rows changed (0 when nothing was new).
pub fn sync_episodes(c: &Connection, episodes: &[Episode]) -> Result<usize, String> {
    let mut changed = 0usize;
    for s in episodes {
        if s.turns.is_empty() { continue; }
        let key = format!("ep:{}:{}", s.pane_id, s.n);
        let sig = fingerprint(s);
        let seen: Option<String> = c.query_row("SELECT sig FROM sagas WHERE plan_ref = ?1", [&key], |x| x.get(0)).ok();
        if seen.as_deref() == Some(sig.as_str()) { continue; }
        let tx = c.unchecked_transaction().map_err(|e| e.to_string())?;
        let actor = if s.agent.is_empty() { s.engine.clone() } else { s.agent.clone() };
        let meta = episode_meta(s);
        let saga: i64 = match tx.query_row("SELECT id FROM sagas WHERE plan_ref = ?1", [&key], |x| x.get(0)).ok() {
            Some(id) => id,
            None => {
                let first = &s.turns[0];
                tx.execute("INSERT INTO sagas (title, objective, origin, plan_ref, created_ms, updated_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?5)", params![meta["auto_title"].as_str().unwrap_or(""), meta["auto_objective"].as_str().unwrap_or(""), actor, key, first.start_ms]).map_err(|e| e.to_string())?;
                let id = tx.last_insert_rowid();
                tx.execute("INSERT OR IGNORE INTO events (saga_id, t_ms, actor, kind, text, ext) VALUES (?1, ?2, ?3, 'created', ?4, ?5)", params![id, first.start_ms, actor, format!("started episode {} in its terminal", s.n), format!("{key}:created")]).map_err(|e| e.to_string())?;
                changed += 1; id
            }
        };
        // THE EPISODE CLOSED: the model's title and summary replace what the saga was called while it was open, unless the person already changed it
        if let Some(b) = &s.closed {
            changed += tx.execute("UPDATE sagas SET title = ?1 WHERE id = ?2 AND title = ?3 AND title != ?1", params![b.title.chars().take(100).collect::<String>(), saga, meta["auto_title"].as_str().unwrap_or("")]).map_err(|e| e.to_string())?;
            changed += tx.execute("UPDATE sagas SET objective = ?1 WHERE id = ?2 AND objective = ?3 AND objective != ?1", params![b.summary.chars().take(2000).collect::<String>(), saga, meta["auto_objective"].as_str().unwrap_or("")]).map_err(|e| e.to_string())?;
            changed += tx.execute("INSERT OR IGNORE INTO events (saga_id, t_ms, actor, kind, text, ext) VALUES (?1, ?2, ?3, 'closed', ?4, ?5)", params![saga, b.end_ms, actor, format!("episode {} closed{}: {}", b.n, if b.by == "pane-close" { " (the pane was closed)" } else { "" }, b.title), format!("{key}:closed")]).map_err(|e| e.to_string())?;
        }
        tx.execute("UPDATE sagas SET meta = ?1 WHERE id = ?2", params![meta.to_string(), saga]).map_err(|e| e.to_string())?;
        for (ti, t) in s.turns.iter().enumerate() {
            let ext = format!("{key}:turn:{}", t.start_ms);
            let status = if t.end_ms.is_some() { "done" } else { "doing" };
            let ms = t.end_ms.map_or(0, |e| (e - t.start_ms).max(0));
            let task: i64 = match tx.query_row("SELECT id FROM tasks WHERE ext = ?1", [&ext], |x| x.get(0)).ok() {
                Some(id) => {
                    let n = tx.execute("UPDATE tasks SET status = ?1, body = ?2, engine = ?3, model = ?4, ms_total = ?5, usd_total = ?6, tokens = ?7, updated_ms = ?8, done_ms = ?9 WHERE id = ?10 AND (status != ?1 OR ms_total != ?5 OR tokens != ?7 OR body != ?2)",
                        params![status, body_of(t), s.engine, s.model, ms, t.usd, t.tokens, t.end_ms.unwrap_or(t.start_ms), t.end_ms, id]).map_err(|e| e.to_string())?;
                    // the title becomes a summary of the conclusion once there is one, unless the person has renamed the task (then it is no longer the line that was asked)
                    if t.end_ms.is_some() { changed += tx.execute("UPDATE tasks SET title = ?1 WHERE id = ?2 AND title = ?3 AND title != ?1", params![summary_title(&t.answer, &t.prompt), id, first_line(&t.prompt, 200)]).map_err(|e| e.to_string())?; }
                    changed += n; id
                }
                None => {
                    tx.execute("INSERT INTO tasks (saga_id, title, body, status, assignee, created_by, position, ext, engine, model, ms_total, usd_total, tokens, created_ms, updated_ms, started_ms, done_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13, ?13, ?14)",
                        params![saga, if t.end_ms.is_some() { summary_title(&t.answer, &t.prompt) } else { first_line(&t.prompt, 200) }, body_of(t), status, actor, ti as i64 + 1, ext, s.engine, s.model, ms, t.usd, t.tokens, t.start_ms, t.end_ms]).map_err(|e| e.to_string())?;
                    changed += 1;
                    tx.execute("INSERT OR IGNORE INTO events (saga_id, task_id, t_ms, actor, kind, text, ext) VALUES (?1, ?2, ?3, ?4, 'asked', ?5, ?6)", params![saga, tx.last_insert_rowid(), t.start_ms, "you", format!("asked: {}", t.prompt.trim().chars().take(500).collect::<String>()), format!("{ext}:asked")]).map_err(|e| e.to_string())?;
                    tx.query_row("SELECT id FROM tasks WHERE ext = ?1", [&ext], |x| x.get(0)).map_err(|e| e.to_string())?
                }
            };
            // the agent's own to-do list is the task's subtasks; one that is no longer on it is canceled, never deleted
            for (i, (content, st)) in t.todos.iter().enumerate() {
                let sext = format!("{ext}:todo:{i}"); let st = todo_status(st);
                match tx.query_row("SELECT id, status, title FROM tasks WHERE ext = ?1", [&sext], |x| Ok((x.get::<_, i64>(0)?, x.get::<_, String>(1)?, x.get::<_, String>(2)?))).ok() {
                    Some((id, old, title)) => { if old != st || title != first_line(content, 200) { changed += tx.execute("UPDATE tasks SET status = ?1, title = ?2, updated_ms = ?3 WHERE id = ?4", params![st, first_line(content, 200), t.end_ms.unwrap_or(t.start_ms), id]).map_err(|e| e.to_string())?; } }
                    None => { tx.execute("INSERT INTO tasks (saga_id, parent_id, title, status, assignee, created_by, position, ext, created_ms, updated_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, ?8)", params![saga, task, first_line(content, 200), st, actor, i as i64 + 1, sext, t.start_ms]).map_err(|e| e.to_string())?; changed += 1; }
                }
            }
            changed += tx.execute("UPDATE tasks SET status = 'canceled' WHERE parent_id = ?1 AND ext LIKE ?2 AND CAST(substr(ext, length(?3) + 1) AS INTEGER) >= ?4 AND status != 'canceled'", params![task, format!("{ext}:todo:%"), format!("{ext}:todo:"), t.todos.len() as i64]).map_err(|e| e.to_string())?;
            // the hook's screening verdict, then every word the model said on the way (each once): the timeline is the record, not a summary of it
            if !t.screen.is_empty() { changed += tx.execute("INSERT OR IGNORE INTO events (saga_id, task_id, t_ms, actor, kind, text, ext) VALUES (?1, ?2, ?3, 'antiloki', 'screen', ?4, ?5)", params![saga, task, t.start_ms + 1, t.screen, format!("{ext}:screen")]).map_err(|e| e.to_string())?; }
            for (at, said) in &t.said { changed += tx.execute("INSERT OR IGNORE INTO events (saga_id, task_id, t_ms, actor, kind, text, ext) VALUES (?1, ?2, ?3, ?4, 'said', ?5, ?6)", params![saga, task, at, actor, said.chars().take(600).collect::<String>(), format!("{ext}:said:{at}:{}", fnv(said))]).map_err(|e| e.to_string())?; }
            // the stack trace: every tool call of the turn, once each
            let skip = t.tools.len().saturating_sub(TRAIL_PER_TASK);
            for tool in t.tools.iter().skip(skip) {
                let (kind, text) = tool_event(tool);
                changed += tx.execute("INSERT OR IGNORE INTO events (saga_id, task_id, t_ms, actor, kind, text, ext) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![saga, task, tool.t_ms, actor, kind, text.chars().take(600).collect::<String>(), format!("{key}:tool:{}", tool.id)]).map_err(|e| e.to_string())?;
            }
            // the end of the turn: its conclusion by title, and the meter (tokens, price, time): once, when it has ended
            if let Some(end) = t.end_ms {
                changed += tx.execute("INSERT OR IGNORE INTO events (saga_id, task_id, t_ms, actor, kind, text, ext) VALUES (?1, ?2, ?3, ?4, 'answered', ?5, ?6)", params![saga, task, end, actor, format!("concluded: {}", summary_title(&t.answer, &t.prompt)), format!("{ext}:answered")]).map_err(|e| e.to_string())?;
                if t.tokens > 0 { changed += tx.execute("INSERT OR IGNORE INTO events (saga_id, task_id, t_ms, actor, kind, text, ext) VALUES (?1, ?2, ?3, ?4, 'usage', ?5, ?6)", params![saga, task, end, actor, usage_line(t, &s.model, ms), format!("{ext}:usage")]).map_err(|e| e.to_string())?; }
            }
        }
        // IN PROGRESS MEANS WORK IS HAPPENING: an episode is `doing` while a prompt is in flight, and `done` when the agent is idle or the episode is closed (whether it is still OPEN is in its metadata, and the
        // sidebar marks it). A saga the person canceled stays canceled. (An open episode used to read `doing` for as long as the pane existed: a list full of idle agents' work "in progress".)
        let in_flight = s.closed.is_none() && s.turns.iter().any(|t| t.end_ms.is_none());
        tx.execute("UPDATE sagas SET status = CASE WHEN status = 'canceled' THEN 'canceled' WHEN ?1 THEN 'doing' ELSE 'done' END, updated_ms = MAX(updated_ms, ?2), sig = ?3 WHERE id = ?4", params![in_flight, s.closed.as_ref().map_or(s.turns.last().map_or(0, |t| t.end_ms.unwrap_or(t.start_ms)), |b| b.end_ms), sig, saga]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
    }
    Ok(changed)
}

/// A box's files, as a signature: unchanged since the last read means nothing to parse.
fn files_sig(dir: &std::path::Path) -> String {
    ["prompts.jsonl", "answers.jsonl", "trace.jsonl", "episodes.jsonl"].iter().map(|f| std::fs::metadata(dir.join(f)).map(|m| format!("{}:{}", m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis()))).unwrap_or_default()).collect::<Vec<_>>().join("|")
}

/// THE ENGINES WITH NO HOOKS (operator 2026-11-26: "on opencode and antigravity the sagas menu has nothing"): Antigravity has none, and OpenCode 2 does not load antiloki's plugin. Their sagas are made from the
/// conversation the chat view already reads: one task per ask, titled and described by the answer. Tokens and cost are not known from it. Pure over those turns.
pub fn session_from_turns(agent: &str, engine: &str, turns: &[Value]) -> Option<BoxSession> {
    let ms = |v: &Value| v["t"].as_str().and_then(|t| t.parse::<i64>().ok().or_else(|| iso_to_ms(t)));
    let mut out: Vec<Turn> = vec![];
    for v in turns {
        let text = v["text"].as_str().unwrap_or("").trim().to_string();
        if v["role"] == json!("user") { let Some(at) = ms(v) else { continue }; out.push(Turn { start_ms: at, end_ms: None, prompt: text, answer: String::new(), tokens: 0, usd: 0.0, usage: Usage::default(), screen: String::new(), said: vec![], tools: vec![], todos: vec![] }); continue; }
        let Some(turn) = out.last_mut() else { continue };
        let at = ms(v).unwrap_or(turn.start_ms);
        for (n, t) in v["tools"].as_array().into_iter().flatten().enumerate() {
            turn.tools.push(Tool { t_ms: at, id: format!("{at}-{}-{n}", turn.tools.len()), tool: t["name"].as_str().unwrap_or("tool").to_string(), summary: t["brief"].as_str().unwrap_or("").to_string(), outcome: if t["ok"] == json!(false) { "error" } else { "ok" }.to_string(), error: String::new() });
        }
        // the last thing said is the conclusion; what came before it was said on the way. A call still open means the turn is not over.
        if !text.is_empty() { if !turn.answer.is_empty() { let (t0, a) = (turn.end_ms.unwrap_or(at), std::mem::take(&mut turn.answer)); turn.said.push((t0, a)); } turn.answer = text; turn.end_ms = Some(at); }
        if v["tools"].as_array().is_some_and(|a| a.iter().any(|t| t["done"] == json!(false))) { turn.end_ms = None; }
    }
    let first = out.first()?.start_ms;
    Some(BoxSession { session: format!("{engine}-{first}"), agent: agent.to_string(), engine: engine.to_string(), model: turns.iter().rev().find_map(|v| v["model"].as_str().filter(|m| !m.is_empty()).map(String::from)).unwrap_or_default(), turns: out })
}
/// The conversation of a hookless box, when it is one: its hooks wrote no prompt.
fn hookless_turns(b: &BoxRef) -> Option<Vec<Value>> {
    if !matches!(b.engine.as_str(), "opencode" | "gemini" | "cursor") || std::fs::metadata(b.records().join("prompts.jsonl")).is_ok_and(|m| m.len() > 0) { return None; }
    // in a shared folder only the conversations begun in this agent's own time are its own
    let window = if b.shared { crate::shared_folder::window(&b.dir.to_string_lossy(), b.pane_id) } else { crate::shared_folder::ALL };
    Some(crate::cowork::engine_turns(&b.dir.to_string_lossy(), &b.engine, 400, window)).filter(|t| !t.is_empty())
}

/// The episodes of one box, from the files its hooks wrote, the episodes it has closed, and the engine's own transcripts (for tokens, and for a turn the hook never saw end). Blocking reads.
pub fn box_episodes(b: &BoxRef) -> Vec<Episode> {
    let records = b.records();
    let read = |f: &str| std::fs::read_to_string(records.join(f)).unwrap_or_default();
    if let Some(turns) = hookless_turns(b) { return session_from_turns(&b.agent, &b.engine, &turns).map(|s| episodes_of(&[s], b.pane_id, &boundaries_of(&read("episodes.jsonl")))).unwrap_or_default(); }
    let (prompts, answers, trace) = (read("prompts.jsonl"), read("answers.jsonl"), read("trace.jsonl"));
    let sessions: Vec<BoxSession> = sessions_from_files(&b.agent, &b.engine, &prompts, &answers, &trace, "").into_iter().map(|s| {
        let text = if b.engine == "claude-code" && !s.session.is_empty() { claude_transcript(&b.dir.to_string_lossy(), &s.session).map(|p| std::fs::read_to_string(p).unwrap_or_default()).unwrap_or_default() } else { String::new() };
        if text.is_empty() { s } else { sessions_from_files(&b.agent, &b.engine, &prompts, &answers, &trace, &text).into_iter().find(|x| x.session == s.session).unwrap_or(s) }
    }).collect();
    episodes_of(&sessions, b.pane_id, &boundaries_of(&read("episodes.jsonl")))
}

/// A pane that is gone (closed, its box removed) can have no prompt in flight: anything its episodes still call `doing` was interrupted. Settles those sagas to `done` and their running tasks to `canceled`,
/// once, with a line in the trail. `live` are the pane ids that still have a box. Returns how many sagas it settled.
pub fn settle_orphans(c: &Connection, live: &[i64]) -> usize {
    let pane_of = "CAST(substr(plan_ref, 4, instr(substr(plan_ref, 4), ':') - 1) AS INTEGER)";
    let ids: Vec<(i64, i64)> = c.prepare(&format!("SELECT id, {pane_of} FROM sagas WHERE plan_ref LIKE 'ep:%' AND status != 'canceled'")).and_then(|mut q| q.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).map(|m| m.flatten().collect())).unwrap_or_default();
    let mut n = 0;
    for (id, pane) in ids {
        if live.contains(&pane) { continue; }
        let running: i64 = c.query_row("SELECT COUNT(*) FROM tasks WHERE saga_id = ?1 AND status = 'doing'", [id], |r| r.get(0)).unwrap_or(0);
        let status: String = c.query_row("SELECT status FROM sagas WHERE id = ?1", [id], |r| r.get(0)).unwrap_or_default();
        if running == 0 && status != "doing" { continue; }
        let _ = c.execute("UPDATE tasks SET status = 'canceled' WHERE saga_id = ?1 AND status = 'doing'", [id]);
        let _ = c.execute("UPDATE sagas SET status = 'done' WHERE id = ?1 AND status = 'doing'", [id]);
        let _ = c.execute("INSERT OR IGNORE INTO events (saga_id, t_ms, actor, kind, text, ext) VALUES (?1, ?2, 'antiloki', 'status', 'the pane went away: whatever was in flight was interrupted', ?3)", params![id, crate::cli::now_ms(), format!("ep-orphan:{id}")]);
        n += 1;
    }
    n
}

/// SAGAS NAMED BY A BRIEF BEFORE 2026-11-26 ("You are …, a specialist") take the name of their first task, which is what was concluded. Idempotent; a title the person wrote never matches.
pub fn retitle_briefs(c: &Connection) -> usize {
    c.execute("UPDATE sagas SET title = (SELECT t.title FROM tasks t WHERE t.saga_id = sagas.id AND t.title NOT LIKE 'You are %, a specialist%' AND t.title NOT LIKE '<pasted_content%' AND t.title != '' ORDER BY t.position, t.id LIMIT 1) \
        WHERE (title LIKE 'You are %, a specialist%' OR title LIKE '<pasted_content%') AND plan_ref LIKE 'ep:%' AND EXISTS (SELECT 1 FROM tasks t WHERE t.saga_id = sagas.id AND t.title NOT LIKE 'You are %, a specialist%' AND t.title NOT LIKE '<pasted_content%' AND t.title != '')", []).unwrap_or(0)
}

/// Every agent box of this project, read and put into the saga store. Cheap when nothing changed (a file signature per box), and never an error the screen sees.
pub fn sync_project(c: &Connection, boxes: &[BoxRef], root: &str) -> usize {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, String>>> = std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    let mut changed = retitle_briefs(c);
    for b in boxes {
        // a hookless box has no hook files to watch: its conversation's length and last words are its signature
        let sig = match hookless_turns(b) { Some(t) => format!("{}:{}", t.len(), t.last().map(|v| format!("{}{}{}", v["t"], v["text"].as_str().map_or(0, str::len), v["tools"].as_array().map_or(0, Vec::len))).unwrap_or_default()), None => files_sig(&b.records()) }; let memo = format!("{root}|{}", b.records().to_string_lossy());
        if sig.chars().all(|c| c == '|') || seen.lock().map_or(false, |m| m.get(&memo) == Some(&sig)) { continue; }
        match sync_episodes(c, &box_episodes(b)) { Ok(n) => { changed += n; if let Ok(mut m) = seen.lock() { m.insert(memo, sig); } } Err(_) => {} }
    }
    // what each open episode has changed in its box so far goes into its saga
    let own: Vec<BoxRef> = boxes.iter().filter(|b| !b.shared).cloned().collect();
    changed += crate::work_changes::record_boxes(c, &own);
    changed += settle_orphans(c, &boxes.iter().map(|b| b.pane_id).collect::<Vec<_>>());
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conversation_without_hooks_becomes_a_task_per_ask_and_syncs_once() {
        let turns = vec![
            json!({ "role": "user", "t": "1000", "text": "add a greeting", "tools": [] }),
            json!({ "role": "assistant", "t": "2000", "text": "looking", "tools": [{ "name": "read", "brief": "a.txt", "done": true, "ok": true }], "model": "m" }),
            json!({ "role": "assistant", "t": "3000", "text": "done", "tools": [] }),
            json!({ "role": "user", "t": "2026-11-26T14:00:00Z", "text": "again", "tools": [] }),
        ];
        let s = session_from_turns("opencode", "opencode", &turns).unwrap();
        assert_eq!((s.session.as_str(), s.turns.len(), s.model.as_str()), ("opencode-1000", 2, "m"));
        assert_eq!((s.turns[0].answer.as_str(), s.turns[0].end_ms, s.turns[0].tools.len(), s.turns[0].said.len(), s.turns[1].end_ms), ("done", Some(3000), 1, 1, None));
        let c = db(); assert!(sync_sessions(&c, std::slice::from_ref(&s)).unwrap() > 0);
        let tasks: i64 = c.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap(); sync_sessions(&c, &[s]).unwrap();
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM tasks", [], |r| r.get(0)).unwrap(), tasks, "reading it again adds nothing");
        assert!(session_from_turns("a", "gemini", &[]).is_none());
    }

    #[test]
    fn a_saga_named_by_a_brief_takes_its_first_concluded_task_and_a_persons_title_stays() {
        let c = db();
        for (id, title) in [(1, "<pasted_content id=\"5bc6\"> You are Reels Specialist, a specialist"), (2, "My own name")] { c.execute("INSERT INTO sagas (id, title, objective, origin, plan_ref, created_ms, updated_ms) VALUES (?1, ?2, '', 'a', ?3, 1, 1)", params![id, title, format!("ep:{id}:1")]).unwrap(); c.execute("INSERT INTO tasks (saga_id, title, position, created_ms, updated_ms) VALUES (?1, 'Planned the week', 1, 1, 1)", [id]).unwrap(); }
        assert_eq!(retitle_briefs(&c), 1); assert_eq!(retitle_briefs(&c), 0);
        let t = |id: i64| c.query_row::<String, _, _>("SELECT title FROM sagas WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert_eq!((t(1).as_str(), t(2).as_str()), ("Planned the week", "My own name"));
    }

    fn db() -> Connection { let c = Connection::open_in_memory().unwrap(); crate::work::init_for_tests(&c).unwrap(); c }
    /// the sessions of pane 7 as episodes, with no episode closed yet
    fn sync_sessions(c: &Connection, s: &[BoxSession]) -> Result<usize, String> { sync_episodes(c, &episodes_of(s, 7, &[])) }
    const P: &str = concat!(
        "{\"t\":\"2026-11-10T11:53:36.617Z\",\"prompt\":\"Describe this project's architecture\",\"session_id\":\"S1\",\"cost_usd\":0.0}\n",
        "{\"t\":\"2026-11-10T11:54:22.205Z\",\"prompt\":\"Add a seventh product to src/products.ts\",\"session_id\":\"S1\",\"cost_usd\":0.0042}\n",
        "{\"t\":\"2026-11-10T12:10:00.000Z\",\"prompt\":\"a different conversation\",\"session_id\":\"S2\"}\n");
    const A: &str = concat!(
        "{\"t\":\"2026-11-10T11:53:49.614Z\",\"session_id\":\"S1\",\"text\":\"## ministore architecture\"}\n",
        "{\"t\":\"2026-11-10T11:54:52.997Z\",\"session_id\":\"S1\",\"text\":\"I added the candle\"}\n");
    const T: &str = concat!(
        "{\"t\":\"2026-11-10T11:53:40.000Z\",\"session_id\":\"S1\",\"tool_use_id\":\"u1\",\"tool\":\"Read\",\"summary\":\"src/App.tsx\",\"outcome\":\"ok\"}\n",
        "{\"t\":\"2026-11-10T11:54:30.000Z\",\"session_id\":\"S1\",\"tool_use_id\":\"u2\",\"tool\":\"TodoWrite\",\"summary\":\"2 to-do items\",\"outcome\":\"ok\",\"todos\":[{\"content\":\"read products\",\"status\":\"completed\"},{\"content\":\"add the candle\",\"status\":\"in_progress\"}]}\n",
        "{\"t\":\"2026-11-10T11:54:40.000Z\",\"session_id\":\"S1\",\"tool_use_id\":\"u3\",\"tool\":\"Edit\",\"summary\":\"src/products.ts\",\"outcome\":\"ok\"}\n",
        "{\"t\":\"2026-11-10T11:54:45.000Z\",\"session_id\":\"S1\",\"tool_use_id\":\"u4\",\"tool\":\"Bash\",\"summary\":\"npm run lint\",\"outcome\":\"error\",\"error\":\"Exit code 1\"}\n");

    #[test]
    fn a_box_becomes_a_saga_per_conversation_a_task_per_prompt_with_its_own_trail() {
        let s = sessions_from_files("Claude Code", "claude-code", P, A, T, "");
        assert_eq!(s.iter().map(|x| (x.session.as_str(), x.turns.len())).collect::<Vec<_>>(), vec![("S1", 2), ("S2", 1)]);
        let t1 = &s[0].turns[0]; let t2 = &s[0].turns[1];
        assert_eq!((t1.tools.len(), t1.end_ms.is_some(), t1.answer.as_str()), (1, true, "## ministore architecture"));
        assert_eq!((t2.tools.iter().map(|t| t.tool.as_str()).collect::<Vec<_>>(), t2.todos.clone()), (vec!["TodoWrite", "Edit", "Bash"], vec![("read products".to_string(), "completed".to_string()), ("add the candle".to_string(), "in_progress".to_string())]));
        assert!(s[1].turns[0].end_ms.is_none(), "no answer yet: still going");
    }

    #[test]
    fn syncing_is_idempotent_and_writes_the_saga_its_tasks_its_subtasks_and_every_tool_call() {
        let c = db(); let s = sessions_from_files("Claude Code", "claude-code", P, A, T, "");
        let n = sync_sessions(&c, &s).unwrap(); assert!(n > 0);
        assert_eq!(sync_sessions(&c, &s).unwrap(), 0, "the same files again change nothing");
        let count = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(count("SELECT COUNT(*) FROM sagas"), 1, "no episode has been closed: all of the pane's work so far is one open episode");
        assert_eq!(count("SELECT COUNT(*) FROM tasks WHERE parent_id IS NULL"), 3); assert_eq!(count("SELECT COUNT(*) FROM tasks WHERE parent_id IS NOT NULL"), 2, "the to-do list is the subtasks");
        assert_eq!(count("SELECT COUNT(*) FROM events WHERE kind = 'tool'"), 4, "every tool call is in the trail");
        let title: String = c.query_row("SELECT title FROM sagas WHERE plan_ref = 'ep:7:1'", [], |r| r.get(0)).unwrap(); assert_eq!(title, "Describe this project's architecture");
        let sub: (String, String) = c.query_row("SELECT title, status FROM tasks WHERE title = 'add the candle'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap(); assert_eq!(sub.1, "doing");
        let bash: String = c.query_row("SELECT text FROM events WHERE ext LIKE '%tool:u4'", [], |r| r.get(0)).unwrap(); assert_eq!(bash, "Bash npm run lint  [error: Exit code 1]");
        let statuses: Vec<(String, String)> = c.prepare("SELECT plan_ref, status FROM sagas ORDER BY id").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().flatten().collect();
        assert_eq!(statuses, vec![("ep:7:1".to_string(), "doing".to_string())], "an episode is open until it is consolidated");
    }

    #[test]
    fn a_task_moves_on_when_its_answer_arrives_and_a_dropped_todo_is_canceled_not_deleted() {
        let c = db();
        sync_sessions(&c, &sessions_from_files("a", "claude-code", P, "", T, "")).unwrap();
        let status = |c: &Connection| c.query_row("SELECT status FROM tasks WHERE parent_id IS NULL AND (title LIKE 'Add a seventh%' OR title = 'I added the candle')", [], |r| r.get::<_, String>(0)).unwrap();
        assert_eq!(status(&c), "doing");
        sync_sessions(&c, &sessions_from_files("a", "claude-code", P, A, T, "")).unwrap();
        assert_eq!(status(&c), "done");
        let title: String = c.query_row("SELECT title FROM tasks WHERE parent_id IS NULL AND (title LIKE 'Add a seventh%' OR title = 'I added the candle')", [], |r| r.get(0)).unwrap();
        assert_eq!(title, "I added the candle", "once it concluded, the task is titled by the conclusion");
        // the agent rewrites its list with one item: the other subtask is canceled, the row stays
        let shorter = format!("{T}{{\"t\":\"2026-11-10T11:54:50.000Z\",\"session_id\":\"S1\",\"tool_use_id\":\"u5\",\"tool\":\"TodoWrite\",\"summary\":\"1 to-do item\",\"outcome\":\"ok\",\"todos\":[{{\"content\":\"read products\",\"status\":\"completed\"}}]}}\n");
        sync_sessions(&c, &sessions_from_files("a", "claude-code", P, A, &shorter, "")).unwrap();
        let dropped: String = c.query_row("SELECT status FROM tasks WHERE ext LIKE '%:todo:1'", [], |r| r.get(0)).unwrap(); assert_eq!(dropped, "canceled");
        assert_eq!(c.query_row("SELECT COUNT(*) FROM tasks WHERE parent_id IS NOT NULL", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    }

    #[test]
    fn duplicate_hook_lines_and_torn_lines_do_not_make_duplicates() {
        let c = db();
        let doubled = P.lines().flat_map(|l| [l.to_string(), l.to_string()]).collect::<Vec<_>>().join("\n") + "\n{\"t\":\"2026-torn\n";
        let tdoubled = T.lines().flat_map(|l| [l.to_string(), l.to_string()]).collect::<Vec<_>>().join("\n") + "\n";
        sync_sessions(&c, &sessions_from_files("a", "claude-code", &doubled, A, &tdoubled, "")).unwrap();
        assert_eq!(c.query_row("SELECT COUNT(*) FROM tasks WHERE parent_id IS NULL", [], |r| r.get::<_, i64>(0)).unwrap(), 3);
        assert_eq!(c.query_row("SELECT COUNT(*) FROM events WHERE kind = 'tool'", [], |r| r.get::<_, i64>(0)).unwrap(), 4, "a tool call recorded by two hooks is one line of the trail");
    }

    #[test]
    fn a_turn_the_hook_never_saw_end_is_done_when_the_transcript_says_so_and_a_box_with_no_prompts_is_nothing() {
        let c = db();
        let transcript = "{\"type\":\"assistant\",\"timestamp\":\"2026-11-10T12:10:30.000Z\",\"message\":{\"stop_reason\":\"end_turn\",\"content\":[]}}\n";
        let s = sessions_from_files("a", "claude-code", P, A, T, transcript); assert!(s[1].turns[0].end_ms.is_some(), "{:?}", s[1]);
        sync_sessions(&c, &s).unwrap(); assert_eq!(c.query_row("SELECT status FROM tasks WHERE title = 'a different conversation'", [], |r| r.get::<_, String>(0)).unwrap(), "done");
        assert!(sessions_from_files("a", "claude-code", "", "", "", "").is_empty()); assert_eq!(sync_sessions(&c, &[]).unwrap(), 0);
    }

    const TRANSCRIPT: &str = concat!(
        "{\"type\":\"assistant\",\"timestamp\":\"2026-11-10T11:53:38.000Z\",\"message\":{\"id\":\"m1\",\"model\":\"claude-sonnet-5-5\",\"usage\":{\"input_tokens\":1200,\"output_tokens\":80,\"cache_read_input_tokens\":9000,\"cache_creation_input_tokens\":500},\"content\":[{\"type\":\"text\",\"text\":\"I will read the app first.\"},{\"type\":\"tool_use\",\"id\":\"u1\"}]}}\n",
        "{\"type\":\"assistant\",\"timestamp\":\"2026-11-10T11:53:38.000Z\",\"message\":{\"id\":\"m1\",\"usage\":{\"input_tokens\":1200,\"output_tokens\":80},\"content\":[{\"type\":\"text\",\"text\":\"I will read the app first.\"}]}}\n",
        "{\"type\":\"assistant\",\"timestamp\":\"2026-11-10T11:53:48.000Z\",\"message\":{\"id\":\"m2\",\"usage\":{\"input_tokens\":300,\"output_tokens\":700},\"content\":[{\"type\":\"text\",\"text\":\"## ministore architecture\"}]}}\n");

    #[test]
    fn a_task_is_titled_by_what_the_model_concluded_and_described_by_what_it_said() {
        assert_eq!(summary_title("## ministore architecture\n\nIt is a Vite app.", "Describe it"), "ministore architecture");
        assert_eq!(summary_title("I added the candle to `src/products.ts`. Then I ran the lint.", "x"), "I added the candle to src/products.ts.");
        assert_eq!(summary_title("**Done:** the picker works now", "x"), "Done");
        assert_eq!(summary_title("", "fix the picker\nplease"), "fix the picker");
        assert_eq!(summary_title("1\n2\n100", "Print the numbers 1 to 120"), "Print the numbers 1 to 120", "numbers alone are not a title: it falls back to what was asked");
        assert_eq!(summary_title("# 42\n\nThe answer.", "x"), "The answer.");
        assert_eq!(summary_title("---\n\n- [the docs](https://x.y) are stale", "x"), "the docs are stale");
        assert_eq!(summary_title("a".repeat(300).as_str(), "x").chars().count(), 100);
        let s = sessions_from_files("Claude Code", "claude-code", P, A, T, TRANSCRIPT);
        let t1 = &s[0].turns[0];
        assert_eq!(t1.said.iter().map(|(_, x)| x.as_str()).collect::<Vec<_>>(), vec!["I will read the app first."], "said once, the conclusion is not 'said on the way'");
        assert_eq!(body_of(t1), "I will read the app first.\n\n— conclusion —\n\n## ministore architecture");
        assert_eq!((t1.usage.input, t1.usage.output, t1.usage.cache_read, t1.usage.cache_write), (1500, 780, 9000, 500));
        let c = db(); sync_sessions(&c, &s).unwrap();
        let first = crate::cowork::iso_to_ms("2026-11-10T11:53:36.617Z").unwrap();
        let row: (String, String) = c.query_row("SELECT title, body FROM tasks WHERE ext = ?1", [format!("ep:7:1:turn:{first}")], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(row.0, "ministore architecture"); assert!(row.1.starts_with("I will read the app first.") && row.1.ends_with("## ministore architecture"));
    }

    #[test]
    fn the_timeline_holds_the_screening_what_was_said_every_call_by_kind_the_conclusion_and_the_meter() {
        let p = P.replacen("\"cost_usd\":0.0}", "\"cost_usd\":0.0,\"screen\":\"pre-check: clear enough, sent as written\"}", 1);
        let t = format!("{T}{{\"t\":\"2026-11-10T11:53:44.000Z\",\"session_id\":\"S1\",\"tool_use_id\":\"u9\",\"tool\":\"mcp__antiloki__antiloki_check\",\"summary\":\"src/App.tsx\",\"outcome\":\"ok\"}}\n{{\"t\":\"2026-11-10T11:53:45.000Z\",\"session_id\":\"S1\",\"tool_use_id\":\"u10\",\"tool\":\"Write\",\"summary\":\".env\",\"outcome\":\"refused\",\"error\":\"outside the scope\"}}\n");
        let c = db(); sync_sessions(&c, &sessions_from_files("Claude Code", "claude-code", &p, A, &t, TRANSCRIPT)).unwrap();
        let kinds: Vec<(String, String)> = c.prepare("SELECT kind, text FROM events WHERE saga_id = (SELECT id FROM sagas WHERE plan_ref = 'ep:7:1') ORDER BY t_ms, id").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().flatten().collect();
        let of = |k: &str| kinds.iter().filter(|(x, _)| x == k).map(|(_, t)| t.clone()).collect::<Vec<_>>();
        assert_eq!(of("asked")[0], "asked: Describe this project's architecture");
        assert_eq!(of("screen"), vec!["pre-check: clear enough, sent as written"]);
        assert_eq!(of("said"), vec!["I will read the app first."]);
        assert_eq!(of("mcp"), vec!["antiloki.antiloki_check src/App.tsx"], "antiloki's own tools stand out as their own kind");
        assert_eq!(of("refused"), vec!["Write .env  [refused: outside the scope]"]);
        assert!(of("tool").iter().any(|t| t == "Read src/App.tsx") && of("tool").iter().any(|t| t.starts_with("Edit ")), "{:?}", of("tool"));
        assert!(of("answered").iter().any(|t| t == "concluded: ministore architecture"));
        let usage = of("usage"); assert!(usage[0].starts_with("claude-sonnet-5-5 · 1.5k in · 780 out · 9.5k cached"), "{:?}", usage);
        // and the order is the order it happened in: asked, screen, said, the call, the conclusion
        let order: Vec<&str> = kinds.iter().map(|(k, _)| k.as_str()).filter(|k| ["asked", "screen", "said", "mcp", "answered"].contains(k)).collect();
        assert_eq!(&order[..4], &["asked", "screen", "said", "mcp"], "{order:?}");
        // syncing again adds nothing
        let again = sync_sessions(&c, &sessions_from_files("Claude Code", "claude-code", &p, A, &t, TRANSCRIPT)).unwrap(); assert_eq!(again, 0);
    }

    #[test]
    fn a_turn_is_priced_by_the_engines_own_meter_when_it_moved_and_by_the_price_list_when_it_did_not() {
        let p = "{\"t\":\"2026-11-10T11:53:36.617Z\",\"prompt\":\"a\",\"session_id\":\"S1\",\"cost_usd\":1.00}\n";
        let a = |c: &str| format!("{{\"t\":\"2026-11-10T11:53:49.614Z\",\"session_id\":\"S1\",\"text\":\"ok\",\"cost_usd\":{c}}}\n");
        let usd = |answers: String| sessions_from_files("a", "claude-code", p, &answers, "", TRANSCRIPT)[0].turns[0].usd;
        assert!((usd(a("1.25")) - 0.25).abs() < 1e-9, "the meter moved: its difference is the price");
        let flat = usd(a("1.00")); assert!(flat > 0.0 && (flat - 0.25).abs() > 1e-9, "the meter did not move: the token price list, not zero ({flat})");
        assert!(usd(a("null")) > 0.0, "no meter at all: the token price list");
    }

    #[test]
    fn the_usage_line_and_the_tool_kinds_read_plainly() {
        let t = Turn { start_ms: 0, end_ms: Some(1), prompt: String::new(), answer: String::new(), tokens: 0, usd: 0.0412, usage: Usage { input: 12_400, output: 1_200, cache_read: 80_000, cache_write: 0 }, screen: String::new(), said: vec![], tools: vec![], todos: vec![] };
        assert_eq!(usage_line(&t, "claude-opus-5-5", 151_000), "claude-opus-5-5 · 12.4k in · 1.2k out · 80.0k cached · $0.0412 · 2m31s");
        assert_eq!(usage_line(&Turn { usd: 0.0, usage: Usage::default(), ..t.clone() }, "", 3_400), "0 in · 0 out · 3.4s");
        let tool = |n: &str, o: &str| Tool { t_ms: 0, id: "x".into(), tool: n.into(), summary: "s".into(), outcome: o.into(), error: String::new() };
        assert_eq!(tool_event(&tool("mcp__antiloki__antiloki_laws", "ok")), ("mcp", "antiloki.antiloki_laws s".to_string()));
        assert_eq!(tool_event(&tool("Bash", "error")), ("tool", "Bash s  [error]".to_string()));
        assert_eq!(tool_event(&tool("Edit", "refused")).0, "refused");
    }

    const BOUNDS: &str = concat!(
        "{\"t\":\"2026-11-10T11:54:00.000Z\",\"n\":1,\"title\":\"Mapped the ministore architecture\",\"summary\":\"Read the app and described how the cart, the products and the checkout fit together.\",\"source\":\"model\",\"model\":\"claude-haiku-5-5\",\"by\":\"you\"}\n",
        "{torn\n", "{\"t\":\"2026-11-10T11:54:00.000Z\",\"n\":1,\"title\":\"a duplicate of the same episode\"}\n");

    #[test]
    fn an_episode_is_the_stretch_between_two_consolidations_and_the_open_stretch_is_the_next() {
        let b = boundaries_of(BOUNDS); assert_eq!(b.len(), 1, "a torn line and a repeated number are skipped"); assert_eq!((b[0].n, b[0].source.as_str(), b[0].model.as_str()), (1, "model", "claude-haiku-5-5"));
        let s = sessions_from_files("Backender", "claude-code", P, A, T, TRANSCRIPT);
        let e = episodes_of(&s, 7, &b);
        assert_eq!(e.iter().map(|x| (x.n, x.closed.is_some(), x.turns.len())).collect::<Vec<_>>(), vec![(1, true, 1), (2, false, 2)], "the first prompt closed in episode 1; the second and the other conversation's are the open episode 2");
        assert_eq!(episodes_of(&s, 7, &[]).iter().map(|x| (x.n, x.turns.len())).collect::<Vec<_>>(), vec![(1, 3)]); assert!(episodes_of(&[], 7, &b).is_empty());
        // a boundary after everything leaves no open episode; one before everything is skipped and the numbers go on
        let all = boundaries_of("{\"t\":\"2026-11-10T13:00:00.000Z\",\"n\":4,\"title\":\"t\"}\n"); assert_eq!(episodes_of(&s, 7, &all).iter().map(|x| (x.n, x.closed.is_some())).collect::<Vec<_>>(), vec![(4, true)]);
        let early = boundaries_of("{\"t\":\"2026-11-10T10:00:00.000Z\",\"n\":1,\"title\":\"t\"}\n"); assert_eq!(episodes_of(&s, 7, &early).iter().map(|x| x.n).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn a_closed_episode_is_a_done_saga_with_the_models_title_and_summary_and_the_next_prompts_go_in_a_new_open_one() {
        let c = db();
        let sess = |p: &str| sessions_from_files("Backender", "claude-code", p, A, T, TRANSCRIPT);
        sync_episodes(&c, &episodes_of(&sess(P), 7, &[])).unwrap();
        let status = |key: &str| c.query_row("SELECT title, objective, status FROM sagas WHERE plan_ref = ?1", [key], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))).unwrap();
        assert_eq!(status("ep:7:1").2, "doing"); assert_eq!(status("ep:7:1").0, "Describe this project's architecture", "while open it is called by what was asked");
        // consolidated: everything the pane has done so far is closed (it always is: the episode ends where the work is now)
        let closed = "{\"t\":\"2026-11-10T12:11:00.000Z\",\"n\":1,\"title\":\"Mapped the ministore architecture\",\"summary\":\"Read the app and described how the cart, the products and the checkout fit together.\",\"source\":\"model\",\"model\":\"claude-haiku-5-5\",\"by\":\"you\"}\n";
        sync_episodes(&c, &episodes_of(&sess(P), 7, &boundaries_of(closed))).unwrap();
        let (t1, o1, s1) = status("ep:7:1"); assert_eq!((t1.as_str(), s1.as_str()), ("Mapped the ministore architecture", "done")); assert!(o1.starts_with("Read the app and described"), "the summary is the saga's description");
        let roots = |c: &Connection| c.query_row("SELECT COUNT(*) FROM tasks WHERE parent_id IS NULL", [], |r| r.get::<_, i64>(0)).unwrap(); assert_eq!(roots(&c), 3, "closing moved no task and doubled none");
        let ev: Vec<String> = c.prepare("SELECT text FROM events WHERE kind = 'closed'").unwrap().query_map([], |r| r.get(0)).unwrap().flatten().collect(); assert_eq!(ev, vec!["episode 1 closed: Mapped the ministore architecture"]);
        assert_eq!(sync_episodes(&c, &episodes_of(&sess(P), 7, &boundaries_of(closed))).unwrap(), 0, "again: nothing changes");
        // the next prompt, after the episode closed, is a new open episode with its own saga
        let p2 = format!("{P}{{\"t\":\"2026-11-10T12:30:00.000Z\",\"prompt\":\"Now add a coupon field\",\"session_id\":\"S2\"}}\n");
        sync_episodes(&c, &episodes_of(&sess(&p2), 7, &boundaries_of(closed))).unwrap();
        assert_eq!(status("ep:7:2").2, "doing"); assert_eq!(status("ep:7:2").0, "Now add a coupon field"); assert_eq!(status("ep:7:1").2, "done", "the closed one stays closed"); assert_eq!(roots(&c), 4);
    }

    #[test]
    fn a_title_the_person_changed_is_theirs_and_survives_the_episode_closing() {
        let c = db(); let s = sessions_from_files("Backender", "claude-code", P, A, T, TRANSCRIPT);
        sync_episodes(&c, &episodes_of(&s, 7, &[])).unwrap();
        c.execute("UPDATE sagas SET title = 'My own name for this', objective = 'My own words' WHERE plan_ref = 'ep:7:1'", []).unwrap();
        sync_episodes(&c, &episodes_of(&s, 7, &boundaries_of(BOUNDS))).unwrap();
        let (t, o, st): (String, String, String) = c.query_row("SELECT title, objective, status FROM sagas WHERE plan_ref = 'ep:7:1'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((t.as_str(), o.as_str(), st.as_str()), ("My own name for this", "My own words", "done"), "the model's title and summary do not overwrite what the person wrote; the episode still closes");
    }

    #[test]
    fn an_episode_carries_its_deterministic_metadata_pane_model_meter_tools_and_files() {
        let t = format!("{T}{{\"t\":\"2026-11-10T11:53:44.000Z\",\"session_id\":\"S1\",\"tool_use_id\":\"u9\",\"tool\":\"mcp__antiloki__antiloki_check\",\"summary\":\"src/App.tsx\",\"outcome\":\"ok\"}}\n{{\"t\":\"2026-11-10T11:53:45.000Z\",\"session_id\":\"S1\",\"tool_use_id\":\"u10\",\"tool\":\"Write\",\"summary\":\".env\",\"outcome\":\"refused\",\"error\":\"x\"}}\n");
        let c = db(); let s = sessions_from_files("Backender", "claude-code", P, A, &t, TRANSCRIPT);
        sync_episodes(&c, &episodes_of(&s, 7, &boundaries_of(BOUNDS))).unwrap();
        let meta: Value = serde_json::from_str(&c.query_row("SELECT meta FROM sagas WHERE plan_ref = 'ep:7:1'", [], |r| r.get::<_, String>(0)).unwrap()).unwrap();
        assert_eq!((meta["kind"].clone(), meta["pane"].clone(), meta["pane_id"].clone(), meta["engine"].clone(), meta["model"].clone(), meta["n"].clone(), meta["turns"].clone()), (json!("episode"), json!("Backender"), json!(7), json!("claude-code"), json!("claude-sonnet-5-5"), json!(1), json!(1)));
        assert_eq!((meta["tokens"]["in"].clone(), meta["tokens"]["out"].clone(), meta["tokens"]["cache_read"].clone()), (json!(1500), json!(780), json!(9000)));
        assert_eq!((meta["tools"]["Read"].clone(), meta["mcp"]["antiloki.antiloki_check"].clone(), meta["refused"].clone(), meta["summary_source"].clone(), meta["closed_by"].clone()), (json!(1), json!(1), json!(1), json!("model"), json!("you")));
        assert_eq!(meta["files"], json!([]), "a refused write is not a file touched"); assert!(meta["usd"].as_f64().unwrap() > 0.0);
        let open: Value = serde_json::from_str(&c.query_row("SELECT meta FROM sagas WHERE plan_ref = 'ep:7:2'", [], |r| r.get::<_, String>(0)).unwrap()).unwrap();
        assert_eq!((open["open"].clone(), open["n"].clone(), open["files"].clone(), open["tools"]["Edit"].clone()), (json!(true), json!(2), json!(["src/products.ts"]), json!(1)), "the open episode's files are the ones it wrote");
        // the list and the detail hand the metadata on as JSON
        let l = crate::work::list(&c).unwrap(); assert!(l.iter().any(|x| x["meta"]["pane"] == "Backender")); let id: i64 = c.query_row("SELECT id FROM sagas WHERE plan_ref = 'ep:7:1'", [], |r| r.get(0)).unwrap();
        assert_eq!(crate::work::saga_detail(&c, id).unwrap()["saga"]["meta"]["n"], 1);
    }

    #[test]
    fn an_episode_is_in_progress_only_while_a_prompt_is_in_flight_and_a_vanished_panes_leftovers_are_settled() {
        let c = db();
        let idle = |c: &Connection| c.query_row("SELECT status FROM sagas WHERE plan_ref = 'ep:7:1'", [], |r| r.get::<_, String>(0)).unwrap();
        // all answered, episode still open: the agent is idle, not working
        let only_first = "{\"t\":\"2026-11-10T11:53:36.617Z\",\"prompt\":\"Describe this project's architecture\",\"session_id\":\"S1\"}\n";
        sync_episodes(&c, &episodes_of(&sessions_from_files("Backender", "claude-code", only_first, A, T, ""), 7, &[])).unwrap(); assert_eq!(idle(&c), "done", "an idle open episode is not 'in progress'");
        let meta: Value = serde_json::from_str(&c.query_row("SELECT meta FROM sagas WHERE plan_ref = 'ep:7:1'", [], |r| r.get::<_, String>(0)).unwrap()).unwrap(); assert_eq!(meta["open"], json!(true), "but it is still OPEN, and says so");
        // a prompt with no answer yet: in flight
        let two = format!("{only_first}{{\"t\":\"2026-11-10T12:10:00.000Z\",\"prompt\":\"and now this\",\"session_id\":\"S1\"}}\n");
        sync_episodes(&c, &episodes_of(&sessions_from_files("Backender", "claude-code", &two, A, T, ""), 7, &[])).unwrap(); assert_eq!(idle(&c), "doing", "a prompt in flight is work in progress");
        // the pane is gone: nothing is in flight any more
        assert_eq!(settle_orphans(&c, &[7]), 0, "a pane that still has its box is left alone"); assert_eq!(idle(&c), "doing");
        assert_eq!(settle_orphans(&c, &[8, 9]), 1); assert_eq!(idle(&c), "done");
        assert_eq!(c.query_row("SELECT COUNT(*) FROM tasks WHERE status = 'doing'", [], |r| r.get::<_, i64>(0)).unwrap(), 0, "its running task was interrupted too");
        assert_eq!(c.query_row("SELECT COUNT(*) FROM events WHERE text LIKE 'the pane went away%'", [], |r| r.get::<_, i64>(0)).unwrap(), 1); assert_eq!(settle_orphans(&c, &[8, 9]), 0, "once");
        // a goal run's saga and the person's own are none of its business
        c.execute("INSERT INTO sagas (title, status, plan_ref, created_ms, updated_ms) VALUES ('a goal', 'doing', 'run:5', 1, 1)", []).unwrap(); c.execute("INSERT INTO sagas (title, status, created_ms, updated_ms) VALUES ('mine', 'doing', 1, 1)", []).unwrap();
        assert_eq!(settle_orphans(&c, &[]), 0); assert_eq!(c.query_row("SELECT COUNT(*) FROM sagas WHERE status = 'doing'", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    }
}
