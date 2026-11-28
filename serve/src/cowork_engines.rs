//! THE STRIP'S NUMBERS FOR THE OTHER ENGINES (RFC 0007 §5.5): Codex and OpenCode, read from what each engine keeps on disk, never
//! injected into its terminal. Claude Code's reader lives in `cowork.rs` (its hooks and transcript); these two need no hook at all:
//!
//!   Codex     the session's rollout (`$CODEX_HOME/sessions/**/rollout-*.jsonl`): a turn is `task_started` .. `task_complete`, and the
//!             cumulative `token_count` events give the turn's tokens as a difference.
//!   OpenCode  its own database (`opencode.db`): the run is the assistant messages after the session's last user message, each with
//!             tokens, a cost and a completion time.
//!
//! Gemini, Cursor and Grok keep no token meter on disk that has been checked (2026-11-10), so they stay `known: false`.
//! Pure functions over text or a connection; the caller finds the files.
use crate::cli::Engine;
use crate::cowork::{iso_to_ms, RunTokens};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::Path;

/// Milliseconds since the epoch → `2026-11-10T08:53:21.284Z`, the format the hooks and transcripts use.
pub fn ms_to_iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000); let milli = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400); let rem = secs.rem_euclid(86_400);
    let z = days + 719_468; let era = z.div_euclid(146_097); let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1; let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// The `run` object the strip reads (same shape as Claude's). `engine_cost` is what the engine itself says the run cost; without it
/// the tokens are priced from the list and `cost_source` says so. The saving's baseline is the same tokens at the frontier yardstick.
pub fn run_value(eng: Engine, start_t: &str, end_t: Option<&str>, prompt: &str, t: &RunTokens, model: Option<&str>, engine_cost: Option<f64>) -> Value {
    let table = crate::cli::price_tokens(eng, model, t.input, t.output, t.cache_read, t.cache_write);
    let (baseline, against) = crate::cli::frontier_price(eng, t.input, t.output, t.cache_read, t.cache_write);
    let (cost, source) = match engine_cost { Some(c) => (c, "engine"), None => (table, "table") };
    let ms = end_t.and_then(iso_to_ms).zip(iso_to_ms(start_t)).map(|(e, s)| (e - s).max(0));
    json!({
        "started_t": start_t, "finished_t": end_t, "ms": ms, "prompt": prompt,
        "tokens_in": t.input + t.cache_read + t.cache_write, "tokens_out": t.output, "raw": { "input": t.input, "cache_read": t.cache_read, "cache_write": t.cache_write },
        "turns": t.messages, "tools": t.tools, "model": model, "cost": cost, "cost_source": source, "baseline": baseline, "against": against,
        "transcript": true,
    })
}

// ── Codex ──────────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Clone)]
pub struct EngineRun {
    pub start_t: String, pub end_t: Option<String>, pub prompt: String, pub tokens: RunTokens, pub model: Option<String>, pub cost: Option<f64>,
    /// The latest instant the engine wrote anything for this run: a run with no end and nothing written for a long while is a dead one.
    pub seen_ms: i64,
}

fn usage4(u: &Value) -> [i64; 4] {
    let n = |k: &str| u[k].as_i64().unwrap_or(0);
    [n("input_tokens"), n("cached_input_tokens"), n("output_tokens"), n("cache_write_input_tokens")]
}

/// The latest turn in a Codex rollout. It starts at the last `task_started` and ends at the `task_complete` after it (none: still going).
/// Tokens are the rise in the cumulative `token_count` totals across the turn. Codex counts cached tokens inside `input_tokens`, so they
/// are taken out of it to be priced apart, as Claude's are. Pure.
pub fn codex_run(text: &str) -> Option<EngineRun> {
    let mut cur: Option<EngineRun> = None;
    let (mut seen, mut base, mut last) = ([0i64; 4], [0i64; 4], [0i64; 4]);
    let (mut msgs, mut tools) = (0usize, 0usize);
    let (mut latest_model, mut pending_prompt): (Option<String>, String) = (None, String::new());
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let ts = v["timestamp"].as_str().unwrap_or("");
        let p = &v["payload"];
        if let (Some(c), Some(ms)) = (cur.as_mut(), iso_to_ms(ts)) { c.seen_ms = c.seen_ms.max(ms); }
        match (v["type"].as_str().unwrap_or(""), p["type"].as_str().unwrap_or("")) {
            ("session_meta", _) | ("turn_context", _) => { if let Some(m) = p["model"].as_str().filter(|m| !m.is_empty()) { latest_model = Some(m.to_string()); if let Some(c) = cur.as_mut() { c.model = latest_model.clone(); } } }
            ("event_msg", "task_started") => {
                cur = Some(EngineRun { start_t: ts.to_string(), end_t: None, prompt: std::mem::take(&mut pending_prompt), tokens: RunTokens::default(), model: latest_model.clone(), cost: None, seen_ms: 0 });
                base = seen; last = seen; msgs = 0; tools = 0;
            }
            ("event_msg", "task_complete") => { if let Some(c) = cur.as_mut() { if c.end_t.is_none() { c.end_t = Some(ts.to_string()); } } pending_prompt.clear(); }
            ("event_msg", "token_count") => {
                if p["info"]["total_token_usage"].is_object() { seen = usage4(&p["info"]["total_token_usage"]); if cur.is_some() { last = seen; } }
            }
            ("response_item", "message") => {
                let role = p["role"].as_str().unwrap_or("");
                if role == "assistant" && cur.is_some() { msgs += 1; }
                if role == "user" {
                    // Codex opens a turn with messages of its own (an <environment_context> block, the repo's AGENTS.md); the ask is the last other one
                    let text = p["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).map(str::trim).find(|t| !t.is_empty() && !t.starts_with('<') && !t.starts_with("# AGENTS.md"));
                    if let Some(t) = text {
                        let t: String = t.chars().take(120).collect();
                        match cur.as_mut() { Some(c) if c.end_t.is_none() => c.prompt = t, _ => pending_prompt = t }
                    }
                }
            }
            ("response_item", "custom_tool_call") | ("response_item", "function_call") | ("response_item", "local_shell_call") => { if cur.is_some() { tools += 1; } }
            _ => {}
        }
    }
    let mut c = cur?;
    let d = [last[0] - base[0], last[1] - base[1], last[2] - base[2], last[3] - base[3]].map(|x| x.max(0));
    c.tokens = RunTokens { input: (d[0] - d[1] - d[3]).max(0), output: d[2], cache_read: d[1], cache_write: d[3], messages: msgs, tools, model: c.model.clone() };
    Some(c)
}

// ── OpenCode ───────────────────────────────────────────────────────────────────────────────────────────────────

/// The latest run in OpenCode's database for this worktree: the assistant messages after the newest session's last user message.
/// `cost` is OpenCode's own figure (0 for a free or unmetered model, which then falls back to the list). `end_t` is set only once
/// every assistant message has a completion time. Pure over a connection.
pub fn opencode_run(db: &Connection, worktree: &Path) -> Result<Option<EngineRun>, String> {
    let wt = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    let mut st = db.prepare("SELECT id, directory FROM session ORDER BY time_updated DESC").map_err(|e| e.to_string())?;
    let sid = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())?
        .flatten().find(|(_, dir)| antiloki_rs::cockpit__helpers_engine_usage::same_dir(dir, &wt) || Path::new(dir) == wt).map(|(id, _)| id);
    let Some(sid) = sid else { return Ok(None) };
    let mut st = db.prepare("SELECT id, time_created, data FROM message WHERE session_id = ?1 ORDER BY time_created DESC, id DESC LIMIT 600").map_err(|e| e.to_string())?;
    let rows: Vec<(String, i64, Value)> = st.query_map([&sid], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))
        .map_err(|e| e.to_string())?.flatten().map(|(i, t, d)| (i, t, serde_json::from_str(&d).unwrap_or(Value::Null))).collect();
    let mut assistants: Vec<&(String, i64, Value)> = vec![];
    let mut user: Option<&(String, i64, Value)> = None;
    for r in &rows { match r.2["role"].as_str() { Some("user") => { user = Some(r); break } Some("assistant") => assistants.push(r), _ => {} } }
    let Some(u) = user else { return Ok(None) };
    let start_ms = u.2["time"]["created"].as_i64().unwrap_or(u.1);
    let prompt = {
        let mut ps = db.prepare("SELECT data FROM part WHERE message_id = ?1 ORDER BY id").map_err(|e| e.to_string())?;
        let found = ps.query_map([&u.0], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?.flatten()
            .filter_map(|d| serde_json::from_str::<Value>(&d).ok()).find(|p| p["type"] == "text").and_then(|p| p["text"].as_str().map(String::from));
        found.unwrap_or_default().trim().chars().take(120).collect::<String>()
    };
    let mut t = RunTokens::default();
    let (mut cost, mut model, mut done_ms, mut pending) = (0.0f64, None::<String>, 0i64, assistants.is_empty());
    let mut seen_ms = start_ms;
    for a in &assistants {
        let d = &a.2; let tk = &d["tokens"];
        t.input += tk["input"].as_i64().unwrap_or(0);
        t.output += tk["output"].as_i64().unwrap_or(0) + tk["reasoning"].as_i64().unwrap_or(0);
        t.cache_read += tk["cache"]["read"].as_i64().unwrap_or(0);
        t.cache_write += tk["cache"]["write"].as_i64().unwrap_or(0);
        t.messages += 1;
        cost += d["cost"].as_f64().unwrap_or(0.0);
        if model.is_none() { model = d["modelID"].as_str().filter(|m| !m.is_empty()).map(String::from); }
        seen_ms = seen_ms.max(a.1).max(d["time"]["created"].as_i64().unwrap_or(0));
        match d["time"]["completed"].as_i64() { Some(c) => { done_ms = done_ms.max(c); seen_ms = seen_ms.max(c) } None => pending = true }
        let mut ps = db.prepare("SELECT count(*) FROM part WHERE message_id = ?1 AND data LIKE '%\"type\":\"tool\"%'").map_err(|e| e.to_string())?;
        t.tools += ps.query_row([&a.0], |r| r.get::<_, i64>(0)).map_err(|e| e.to_string())? as usize;
    }
    t.model = model.clone();
    Ok(Some(EngineRun { start_t: ms_to_iso(start_ms), end_t: (!pending).then(|| ms_to_iso(done_ms)), prompt, tokens: t, model, cost: (cost > 0.0).then_some(cost), seen_ms }))
}

/// The latest run in OpenCode 2's tables (`session_message`): the assistant steps after the session's last user message. It has ended when the last step finished for another reason than calling a tool.
/// `None` when this database has no such tables or no conversation for the folder.
pub fn opencode_v2_run(db: &Connection, worktree: &Path) -> Option<EngineRun> {
    let wt = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    let mut st = db.prepare("SELECT id, directory FROM session_v2 ORDER BY time_updated DESC").ok()?;
    let sid = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).ok()?.flatten().find(|(_, d)| antiloki_rs::cockpit__helpers_engine_usage::same_dir(d, &wt) || Path::new(d) == wt)?.0;
    let mut st = db.prepare("SELECT id, type, data FROM session_message WHERE session_id = ?1 AND type IN ('user','assistant') ORDER BY seq DESC LIMIT 600").ok()?;
    let rows: Vec<(String, String, Value)> = st.query_map([&sid], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))).ok()?.flatten().map(|(i, k, d)| (i, k, serde_json::from_str(&d).unwrap_or(Value::Null))).collect();
    let user = rows.iter().find(|r| r.1 == "user")?; let start_ms = user.2["time"]["created"].as_i64()?;
    let steps: Vec<&(String, String, Value)> = rows.iter().take_while(|r| r.1 == "assistant").collect();   // newest first
    let mut t = RunTokens::default(); let (mut cost, mut model, mut seen_ms) = (0.0f64, None::<String>, start_ms);
    for (id, _, d) in &steps {
        let tk = &d["tokens"]; let n = |v: &Value| v.as_i64().unwrap_or(0);
        t.input += n(&tk["input"]); t.output += n(&tk["output"]) + n(&tk["reasoning"]); t.cache_read += n(&tk["cache"]["read"]); t.cache_write += n(&tk["cache"]["write"]); t.messages += 1;
        cost += d["cost"].as_f64().unwrap_or(0.0);
        if model.is_none() { model = d["model"]["id"].as_str().map(String::from); }
        seen_ms = seen_ms.max(n(&d["time"]["created"])).max(n(&d["time"]["completed"]));
        t.tools += db.query_row("SELECT count(*) FROM session_part WHERE message_id = ?1 AND type = 'tool'", [id], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize;
    }
    let end = steps.first().filter(|(_, _, d)| d["finish"].as_str().is_some_and(|f| f != "tool-calls")).and_then(|(_, _, d)| d["time"]["completed"].as_i64());
    t.model = model.clone();
    Some(EngineRun { start_t: ms_to_iso(start_ms), end_t: end.map(ms_to_iso), prompt: user.2["text"].as_str().unwrap_or("").trim().chars().take(120).collect(), tokens: t, model, cost: (cost > 0.0).then_some(cost), seen_ms })
}

/// The latest run in an Antigravity transcript: the model steps after the last thing the person said, with their tokens. It has ended when the last model step answered without calling a tool. Pure.
pub fn agy_run(text: &str) -> Option<EngineRun> {
    let steps: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let at = steps.iter().rposition(|s| s["type"] == json!("USER_INPUT"))?;
    let said = steps[at]["content"].as_str().unwrap_or(""); let prompt = said.split("<USER_REQUEST>").nth(1).map(|r| r.split("</USER_REQUEST>").next().unwrap_or(r)).unwrap_or(said).trim().chars().take(120).collect();
    let start_t = steps[at]["created_at"].as_str().unwrap_or("").to_string();
    let num = |v: &Value| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0);
    let (mut t, mut model, mut end_t, mut seen) = (RunTokens::default(), None::<String>, None::<String>, iso_to_ms(&start_t).unwrap_or(0));
    for s in &steps[at + 1..] {
        if let Some(ms) = s["created_at"].as_str().and_then(iso_to_ms) { seen = seen.max(ms); }
        if s["type"] != json!("PLANNER_RESPONSE") { end_t = None; continue; }
        t.input += num(&s["input_tokens"]); t.output += num(&s["output_tokens"]); t.cache_read += num(&s["cache_read_tokens"]); t.messages += 1;
        if let Some(m) = s["model"].as_str() { model = Some(m.to_string()); }
        let calls = s["tool_calls"].as_array().map_or(0, Vec::len); t.tools += calls;
        end_t = (calls == 0 && s["content"].as_str().is_some_and(|c| !c.trim().is_empty())).then(|| s["created_at"].as_str().unwrap_or("").to_string());
    }
    t.model = model.clone();
    Some(EngineRun { start_t, end_t, prompt, tokens: t, model, cost: None, seen_ms: seen })
}

/// A run with no end that nothing has touched for this long is a dead engine, not a long one.
const DEAD_AFTER_MS: i64 = 30 * 60 * 1000;

/// One pane's numbers for Codex or OpenCode: the session figures from the existing readers (`engine-usage.rs`: model, context, plan
/// windows, session cost) and the latest run from the engine's own files. Anything else answers `known: false`, so the strip shows `—`.
pub fn usage_for_other(root: &Path, engine_key: &str, blocked: Option<String>, now_ms: i64) -> Value {
    let now = ms_to_iso(now_ms);
    use antiloki_rs::cockpit__helpers_engine_usage as eu;
    let (eng, base, run): (Engine, Value, Option<EngineRun>) = match engine_key {
        "codex" => {
            let base = eu::codex_usage(root, &now);
            let run = base["source"].as_str().filter(|s| *s != "none").and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| codex_run(&t));
            (Engine::Codex, base, run)
        }
        "opencode" => {
            let base = eu::opencode_usage(root, None, &now);
            let run = eu::opencode_db().and_then(|p| Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).ok())
                .and_then(|db| opencode_v2_run(&db, root).or_else(|| opencode_run(&db, root).ok().flatten()));
            (Engine::Opencode, base, run)
        }
        "gemini" => {
            let run = crate::cowork::agy_transcript_for(&root.to_string_lossy()).and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| agy_run(&t));
            // the plan windows are Antigravity's own `/usage` (agy_usage); empty until its first reading
            let limits: Vec<Value> = crate::agy_usage::rows_now().into_iter().filter(|r| matches!(r["label"].as_str(), Some("5h" | "week"))).collect();
            let model = run.as_ref().and_then(|r| r.model.clone());
            (Engine::Gemini, json!({ "source": if run.is_some() { "agy" } else { "none" }, "reason": "no Antigravity conversation in this worktree yet", "model": model, "context": Value::Null, "limits": limits, "cost_usd": Value::Null }), run)
        }
        _ => return json!({ "ok": true, "known": false, "engine": engine_key, "reason": "no local usage source for this engine yet", "blocked": blocked }),
    };
    if run.is_none() && base["source"] == "none" {
        return json!({ "ok": true, "known": false, "engine": engine_key, "reason": base["reason"], "blocked": blocked });
    }
    let working = run.as_ref().map_or(false, |r| r.end_t.is_none() && now_ms - r.seen_ms < DEAD_AFTER_MS);
    let run_json = run.as_ref().map_or(Value::Null, |r| run_value(eng, &r.start_t, r.end_t.as_deref(), &r.prompt, &r.tokens, r.model.as_deref(), r.cost));
    json!({
        "ok": true, "known": true, "engine": engine_key, "working": working, "blocked": blocked,
        // the session reader finds no model for OpenCode's newer store: the run just read from it names it
        "model": if base["model"].as_str().is_some_and(|m| !m.is_empty()) { base["model"].clone() } else { json!(run.as_ref().and_then(|r| r.model.clone())) }, "context": base["context"], "limits": base["limits"], "session_cost": base["cost_usd"],
        "lines_added": Value::Null, "lines_removed": Value::Null,
        "run": run_json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ts: &str, ty: &str, payload: Value) -> String { json!({ "timestamp": ts, "type": ty, "payload": payload }).to_string() }
    fn count(ts: &str, input: i64, cached: i64, output: i64) -> String {
        ev(ts, "event_msg", json!({ "type": "token_count", "info": { "total_token_usage": { "input_tokens": input, "cached_input_tokens": cached, "output_tokens": output, "cache_write_input_tokens": 0, "total_tokens": input + output } } }))
    }
    #[test]
    fn an_antigravity_run_counts_the_steps_after_the_last_ask_and_ends_on_an_answer() {
        let t = |lines: &[&str]| agy_run(&lines.join("\n")).unwrap();
        let ask = r#"{"type":"USER_INPUT","created_at":"2026-11-26T14:00:00Z","content":"<USER_REQUEST>\nhi\n</USER_REQUEST>"}"#;
        let call = r#"{"type":"PLANNER_RESPONSE","created_at":"2026-11-26T14:00:01Z","model":"g","input_tokens":"10","output_tokens":2,"tool_calls":[{"name":"x"}]}"#;
        let answer = r#"{"type":"PLANNER_RESPONSE","created_at":"2026-11-26T14:00:03Z","model":"g","input_tokens":5,"cache_read_tokens":7,"output_tokens":3,"content":"Hey"}"#;
        let open = t(&[ask, call]); assert!(open.end_t.is_none() && open.prompt == "hi" && open.tokens.tools == 1, "{open:?}");
        let done = t(&[ask, call, answer]); assert_eq!((done.end_t.as_deref(), done.tokens.input, done.tokens.cache_read, done.tokens.output, done.tokens.messages), (Some("2026-11-26T14:00:03Z"), 15, 7, 5, 2));
        assert!(t(&[ask, call, answer, ask]).tokens.messages == 0, "a new ask starts a new run");
    }

    fn rollout() -> String {
        [
            ev("2026-10-22T20:22:03.279Z", "session_meta", json!({ "id": "s", "cwd": "/w", "model_provider": "openai" })),
            ev("2026-10-22T20:22:03.290Z", "event_msg", json!({ "type": "task_started", "turn_id": "t1" })),
            ev("2026-10-22T20:22:04.000Z", "turn_context", json!({ "model": "gpt-5.6-terra" })),
            ev("2026-10-22T20:22:04.050Z", "response_item", json!({ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "# AGENTS.md instructions for /w\n\n<INSTRUCTIONS>be brief</INSTRUCTIONS>" }] })),
            ev("2026-10-22T20:22:04.100Z", "response_item", json!({ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "<environment_context>x</environment_context>" }, { "type": "input_text", "text": "why did the portal crash?" }] })),
            count("2026-10-22T20:22:09.400Z", 14_526, 5_888, 125),
            ev("2026-10-22T20:22:10.000Z", "response_item", json!({ "type": "custom_tool_call" })),
            ev("2026-10-22T20:22:11.000Z", "response_item", json!({ "type": "message", "role": "assistant", "content": [] })),
            count("2026-10-22T20:22:12.000Z", 30_000, 20_000, 400),
            ev("2026-10-22T20:22:13.000Z", "event_msg", json!({ "type": "task_complete", "turn_id": "t1", "last_agent_message": "because" })),
            // a second turn in the same session: its tokens are the rise over the first turn's total, not the session total
            ev("2026-10-22T21:00:00.000Z", "event_msg", json!({ "type": "task_started", "turn_id": "t2" })),
            ev("2026-10-22T21:00:00.500Z", "response_item", json!({ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "and the fix?" }] })),
            count("2026-10-22T21:00:05.000Z", 31_000, 20_800, 480),
        ].join("\n")
    }

    #[test]
    fn a_codex_turn_is_the_rise_in_the_cumulative_totals_between_its_start_and_its_end() {
        let text = rollout();
        // only the first turn's lines: it has finished
        let first: String = text.lines().take(10).collect::<Vec<_>>().join("\n");
        let r = codex_run(&first).unwrap();
        assert_eq!((r.start_t.as_str(), r.end_t.as_deref(), r.prompt.as_str(), r.model.as_deref()), ("2026-10-22T20:22:03.290Z", Some("2026-10-22T20:22:13.000Z"), "why did the portal crash?", Some("gpt-5.6-terra")));
        // 30_000 in of which 20_000 cached; 400 out
        assert_eq!((r.tokens.input, r.tokens.cache_read, r.tokens.output, r.tokens.messages, r.tokens.tools), (10_000, 20_000, 400, 1, 1), "{r:?}");
    }

    #[test]
    fn the_latest_codex_turn_is_a_difference_and_with_no_task_complete_it_is_still_going() {
        let r = codex_run(&rollout()).unwrap();
        assert_eq!(r.end_t, None, "no task_complete after the second task_started: the run is going");
        assert_eq!(r.prompt, "and the fix?");
        // the rise since the first turn ended: 1_000 in of which 800 cached, 80 out
        assert_eq!((r.tokens.input, r.tokens.cache_read, r.tokens.output), (200, 800, 80), "{r:?}");
    }

    #[test]
    fn a_rollout_with_no_turn_yet_and_garbage_lines_give_nothing_not_a_panic() {
        assert_eq!(codex_run(""), None);
        assert_eq!(codex_run("not json\n{\"type\":\"session_meta\",\"payload\":{}}\n"), None);
    }

    #[test]
    fn milliseconds_round_trip_through_the_hooks_own_format() {
        for ms in [0, 1_790_563_734_755i64, 1_794_300_798_816i64] { assert_eq!(iso_to_ms(&ms_to_iso(ms)), Some(ms), "{ms}"); }
        assert_eq!(ms_to_iso(1_794_300_798_816i64), "2026-11-10T08:53:18.816Z");
    }

    fn oc_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, time_updated INTEGER);
            CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);
            CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);").unwrap();
        db
    }
    fn msg(db: &Connection, id: &str, t: i64, data: Value) { db.execute("INSERT INTO message VALUES (?1, 's1', ?2, ?2, ?3)", rusqlite::params![id, t, data.to_string()]).unwrap(); }
    fn part(db: &Connection, id: &str, m: &str, data: Value) { db.execute("INSERT INTO part VALUES (?1, ?2, 's1', 0, 0, ?3)", rusqlite::params![id, m, data.to_string()]).unwrap(); }

    #[test]
    fn an_opencode_run_is_the_assistant_messages_after_the_last_user_message() {
        let db = oc_db();
        let dir = std::env::temp_dir().to_string_lossy().into_owned();
        db.execute("INSERT INTO session VALUES ('s1', ?1, 9)", [&dir]).unwrap();
        db.execute("INSERT INTO session VALUES ('other', '/somewhere/else', 99)", []).unwrap();
        // an earlier run, which must not leak into this one
        msg(&db, "m1", 100, json!({ "role": "user", "time": { "created": 100 } }));
        msg(&db, "m2", 110, json!({ "role": "assistant", "cost": 9.0, "tokens": { "input": 5000, "output": 500, "reasoning": 0, "cache": { "read": 0, "write": 0 } }, "modelID": "old", "time": { "created": 110, "completed": 120 } }));
        // the latest run: a user message and two assistant messages (one used a tool)
        msg(&db, "m3", 1_794_300_798_816i64, json!({ "role": "user", "time": { "created": 1_794_300_798_816i64 } }));
        part(&db, "p1", "m3", json!({ "type": "text", "text": "  add a retry to the picker  " }));
        msg(&db, "m4", 1_794_300_799_000i64, json!({ "role": "assistant", "cost": 0.01, "tokens": { "input": 1000, "output": 100, "reasoning": 20, "cache": { "read": 4000, "write": 300 } }, "modelID": "deepseek-v4-flash", "time": { "created": 1_794_300_799_000i64, "completed": 1_794_300_801_000i64 } }));
        part(&db, "p2", "m4", json!({ "type": "tool", "tool": "edit" }));
        msg(&db, "m5", 1_794_300_802_000i64, json!({ "role": "assistant", "cost": 0.02, "tokens": { "input": 2000, "output": 200, "reasoning": 0, "cache": { "read": 0, "write": 0 } }, "modelID": "deepseek-v4-flash", "time": { "created": 1_794_300_802_000i64, "completed": 1_794_300_804_500i64 } }));
        let r = opencode_run(&db, Path::new(&dir)).unwrap().unwrap();
        assert_eq!((r.start_t.as_str(), r.end_t.as_deref(), r.prompt.as_str()), ("2026-11-10T08:53:18.816Z", Some("2026-11-10T08:53:24.500Z"), "add a retry to the picker"));
        assert_eq!((r.tokens.input, r.tokens.output, r.tokens.cache_read, r.tokens.cache_write, r.tokens.messages, r.tokens.tools), (3000, 320, 4000, 300, 2, 1), "{r:?}");
        assert!((r.cost.unwrap() - 0.03).abs() < 1e-9, "OpenCode's own cost, summed over the run only: {r:?}");
        assert_eq!(r.model.as_deref(), Some("deepseek-v4-flash"));
        // an assistant message with no completion time: the run is going
        msg(&db, "m6", 1_794_300_806_000i64, json!({ "role": "assistant", "cost": 0, "tokens": { "input": 1, "output": 0, "reasoning": 0, "cache": { "read": 0, "write": 0 } }, "time": { "created": 1_794_300_806_000i64 } }));
        assert_eq!(opencode_run(&db, Path::new(&dir)).unwrap().unwrap().end_t, None);
        // a user message with no answer yet is a run going too; a directory OpenCode never saw gives nothing
        msg(&db, "m7", 1_794_300_807_000i64, json!({ "role": "user", "time": { "created": 1_794_300_807_000i64 } }));
        let going = opencode_run(&db, Path::new(&dir)).unwrap().unwrap();
        assert_eq!((going.end_t, going.tokens.messages), (None, 0));
        assert_eq!(opencode_run(&db, Path::new("/never/seen")).unwrap(), None);
    }

    #[test]
    fn the_run_object_prices_from_the_list_unless_the_engine_gave_a_cost() {
        let t = RunTokens { input: 10_000, output: 400, cache_read: 20_000, cache_write: 0, messages: 1, tools: 1, model: None };
        let listed = run_value(Engine::Codex, "2026-10-22T20:22:03.290Z", Some("2026-10-22T20:22:13.000Z"), "q", &t, Some("gpt-5.6-terra"), None);
        assert_eq!((listed["cost_source"].clone(), listed["tokens_in"].clone(), listed["ms"].clone()), (json!("table"), json!(30_000), json!(9_710)));
        assert!(listed["cost"].as_f64().unwrap() > 0.0 && listed["baseline"].as_f64().unwrap() > 0.0);
        let metered = run_value(Engine::Opencode, "2026-10-22T20:22:03.290Z", None, "q", &t, Some("deepseek-v4-flash"), Some(0.03));
        assert_eq!((metered["cost"].clone(), metered["cost_source"].clone(), metered["finished_t"].clone()), (json!(0.03), json!("engine"), Value::Null));
    }

    /// Against the files this machine really has: `ANTILOKI_REAL_CODEX_ROLLOUT=<rollout.jsonl>`, `ANTILOKI_REAL_OPENCODE_DIR=<a directory OpenCode has worked in>`.
    /// `cargo test -p antiloki-serve --lib real_ -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_codex_rollout() {
        let Ok(p) = std::env::var("ANTILOKI_REAL_CODEX_ROLLOUT") else { return };
        let text = std::fs::read_to_string(&p).unwrap();
        let r = codex_run(&text).expect("a real rollout has a turn");
        let last_total = text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v["payload"]["type"] == "token_count").filter_map(|v| v["payload"]["info"]["total_token_usage"].as_object().cloned()).last().unwrap();
        println!("{r:#?}\nrollout's own final total: {}", Value::Object(last_total.clone()));
        let t = &r.tokens;
        assert_eq!(t.input + t.cache_read + t.cache_write, last_total["input_tokens"].as_i64().unwrap(), "a one-turn rollout: the turn is the whole session");
        assert_eq!(t.output, last_total["output_tokens"].as_i64().unwrap());
        assert!(r.end_t.is_some());
        println!("{}", run_value(Engine::Codex, &r.start_t, r.end_t.as_deref(), &r.prompt, t, r.model.as_deref(), None));
    }

    #[test]
    #[ignore]
    fn real_opencode_db() {
        let Ok(dir) = std::env::var("ANTILOKI_REAL_OPENCODE_DIR") else { return };
        let p = antiloki_rs::cockpit__helpers_engine_usage::opencode_db().expect("an OpenCode database");
        let db = Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let t0 = std::time::Instant::now();
        let r = opencode_run(&db, Path::new(&dir)).unwrap().expect("a run for that directory");
        println!("{r:#?}\nin {:?}", t0.elapsed());
        println!("{}", run_value(Engine::Opencode, &r.start_t, r.end_t.as_deref(), &r.prompt, &r.tokens, r.model.as_deref(), r.cost));
    }
}
