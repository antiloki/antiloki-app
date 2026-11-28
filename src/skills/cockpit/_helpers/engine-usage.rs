//! engine-usage.rs — token / session / plan-window usage for a code-agent pane, per engine. NO JS TWIN.
//!
//! Added 2026-09-16 (operator: "token usage, session usage and weekly usage — claude, codex and
//! opencode should display this info all the time"). Each engine keeps its numbers somewhere
//! different; this module reads them and returns ONE shape the pane chip and the `antiloki_usage`
//! tool render:
//!
//! ```jsonc
//! { "engine": "claude-code" | "codex" | "opencode",
//!   "t": "…iso…", "source": "…where it came from…", "model": "…",
//!   "context": { "used_pct": 12, "input_tokens": 15500, "output_tokens": 1200, "size": 200000 },
//!   "session": { "tokens_total": 989636, "input_tokens": …, "output_tokens": …, "cached_input_tokens": …, "messages": 14 },
//!   "cost_usd": 0.42,
//!   "limits": [ { "label": "5h", "used_pct": 23.5, "resets_at": 1738425600 }, { "label": "week", … } ],
//!   "weekly_local": { "tokens": …, "cost_usd": … } }   // OpenCode only: a 7-day tally from its own db
//! ```
//!
//! * **Claude Code** — its status-line script (`antiloki-statusline.js`, wired by the bake) writes
//!   `<worktree>/.antiloki/usage.json` on every assistant message: context window, cost, the plan's
//!   5-hour / 7-day windows. We read that file.
//! * **Codex** — every session writes a rollout log under `$CODEX_HOME/sessions/**/rollout-*.jsonl`;
//!   the first line names the `cwd`, each `token_count` event carries totals, the context window
//!   and the plan's `rate_limits` (primary/secondary windows). We find the newest rollout for the
//!   pane's worktree and take its last `token_count`.
//! * **OpenCode** — a local SQLite db (`$XDG_DATA_HOME/opencode/opencode.db`): sessions by
//!   `directory`, assistant messages with `tokens` and `cost`. Session = the sum for this worktree's
//!   sessions; weekly = the sum over every assistant message of the last 7 days (BYOK has no plan
//!   window, so the local tally is the honest number). The antiloki proxy's own meter
//!   (`cockpit_panes.cost_usd` / `calls`) is added when the pane is proxied.
//!
//! Every reader is best-effort and returns `{engine, source: "none", reason}` when the engine has
//! written nothing yet — never a guess.
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

fn s(v: Option<&Value>) -> String { match v { Some(Value::String(x)) => x.clone(), Some(Value::Null) | None => String::new(), Some(o) => o.to_string() } }
fn num(v: Option<&Value>) -> Option<f64> { v.and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|t| t.parse().ok()))) }

fn home() -> Option<PathBuf> {
    std::env::var("HOME").ok().filter(|h| !h.is_empty()).map(PathBuf::from)
        .or_else(|| std::env::var("USERPROFILE").ok().filter(|h| !h.is_empty()).map(PathBuf::from))
}

fn none(engine: &str, reason: &str, now: &str) -> Value {
    json!({ "engine": engine, "t": now, "source": "none", "reason": reason, "limits": [] })
}

/// A plan window label from its length: 300 min → `5h`, 10080 → `week`, 43200 → `30d`, else `Nh`.
pub fn window_label(minutes: f64) -> String {
    if minutes <= 0.0 { return "window".into(); }
    if (minutes - 300.0).abs() < 1.0 { return "5h".into(); }
    if (minutes - 10080.0).abs() < 1.0 { return "week".into(); }
    if (minutes - 43200.0).abs() < 1.0 { return "30d".into(); }
    if minutes < 60.0 { return format!("{}m", minutes.round() as i64); }
    if minutes < 1440.0 { return format!("{}h", (minutes / 60.0).round() as i64); }
    format!("{}d", (minutes / 1440.0).round() as i64)
}

// ── Claude Code ─────────────────────────────────────────────────────────────────────────────
/// The account's rate-limit windows, from whatever meter on this box was written last.
///
/// SPEND IS THE AGENT'S, THE LIMITS ARE THE ACCOUNT'S (2026-09-24). Tokens and dollars belong to the pane that
/// burned them, so an agent with no meter of its own has spent nothing and must not inherit a neighbour's figure.
/// The 5-hour and weekly windows are not like that: they are one subscription's, identical for every agent on the
/// machine, so an agent that has not spoken yet can still report them truthfully. That is also what keeps the
/// header's donut — which is nothing but those two windows — lit before anybody has worked today.
fn account_limits(dir: &Path, now: &str) -> Value {
    let mut best: Option<(std::time::SystemTime, Value)> = None;
    // agents that share the folder write their meters one level down, each in its own records
    let shared: Vec<std::path::PathBuf> = std::fs::read_dir(dir.join("agents")).into_iter().flatten().flatten().map(|e| e.path()).collect();
    if shared.is_empty() && std::fs::read_dir(dir).is_err() { return none("claude-code", "no meter on this box yet", now) }
    for e in std::iter::once(dir.to_path_buf()).chain(shared).flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten()) {
        let n = e.file_name().to_string_lossy().to_string();
        if !(n == "usage.json" || (n.starts_with("usage-") && n.ends_with(".json"))) { continue }
        let Ok(t) = std::fs::read_to_string(e.path()) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
        if v.get("limits").and_then(Value::as_array).is_none_or(|a| a.is_empty()) { continue }
        let m = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(bm, _)| m > *bm) { best = Some((m, v)); }
    }
    let Some((_, v)) = best else { return none("claude-code", "no meter on this box yet", now) };
    // a source of its own, not "none": the windows are real and must survive the reader's is-it-reporting check,
    // and every agent in this state shares the one string, so an aggregate counts the account's windows once.
    //
    // `measured_at` is the METER's own stamp, not now (operator 2026-09-24: "the weekly window is at 96% … but
    // now is 99% and didnt update"). These windows only change when a CLI rewrites its status file, which happens
    // when an agent speaks — so between turns the number is genuinely old, and presenting it as current is the
    // lie. Carrying when it was taken lets the banner say "measured 3 h ago" instead.
    json!({ "engine": "claude-code", "t": now, "source": ".antiloki (account windows)",
            "measured_at": v.get("t").cloned().unwrap_or(Value::Null),
            "reason": "this agent has not spent anything yet — the windows are the account's",
            "limits": v.get("limits").cloned().unwrap_or(json!([])) })
}
pub fn claude_usage(worktree: &Path, pane_id: i64, now: &str) -> Value {
    // per PANE, not per worktree — several agents share a box and were overwriting each other's meter
    let dir = worktree.join(".antiloki");
    let p = dir.join(format!("usage-{pane_id}.json"));
    let Ok(text) = std::fs::read_to_string(&p) else { return account_limits(&dir, now) };
    let Ok(mut v) = serde_json::from_str::<Value>(&text) else { return none("claude-code", "usage.json unreadable", now) };
    if let Some(o) = v.as_object_mut() {
        let taken = o.get("t").cloned().unwrap_or(Value::Null);
        o.insert("measured_at".into(), taken);
        o.insert("engine".into(), json!("claude-code"));
        o.insert("source".into(), json!(format!(".antiloki/usage-{pane_id}.json (Claude Code status line)")));
        if !o.contains_key("limits") { o.insert("limits".into(), json!([])); }
    }
    v
}

// ── Codex ───────────────────────────────────────────────────────────────────────────────────
pub fn codex_home() -> Option<PathBuf> {
    std::env::var("CODEX_HOME").ok().filter(|h| !h.is_empty()).map(PathBuf::from).or_else(|| home().map(|h| h.join(".codex")))
}

pub fn walk_jsonl(dir: &Path, out: &mut Vec<(std::time::SystemTime, PathBuf)>, depth: usize) {
    if depth > 6 { return; }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() { walk_jsonl(&p, out, depth + 1); continue; }
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !(name.starts_with("rollout-") && name.ends_with(".jsonl")) { continue; }
        if let Ok(md) = e.metadata() { if let Ok(m) = md.modified() { out.push((m, p)); } }
    }
}

pub fn same_dir(a: &str, b: &Path) -> bool {
    let a = Path::new(a);
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

/// Codex: totals + plan windows from the pane worktree's newest rollout. Public so the pure part
/// (`codex_from_rollout`) is testable on a fixture file.
pub fn codex_usage(worktree: &Path, now: &str) -> Value {
    let Some(root) = codex_home() else { return none("codex", "no home directory", now) };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    walk_jsonl(&root.join("sessions"), &mut files, 0);
    files.sort_by(|a, b| b.0.cmp(&a.0));
    let cutoff = std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(3 * 86400));
    for (m, p) in files {
        if let Some(c) = cutoff { if m < c { break; } }
        // the first line is session_meta — cheap to check before reading the rest
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Some(first) = text.lines().next() else { continue };
        let Ok(meta) = serde_json::from_str::<Value>(first) else { continue };
        let cwd = s(meta.get("payload").and_then(|pl| pl.get("cwd")));
        if cwd.is_empty() || !same_dir(&cwd, worktree) { continue; }
        if let Some(v) = codex_from_rollout(&text, &p.to_string_lossy(), now) { return v; }
    }
    none("codex", "no Codex rollout for this worktree in the last 3 days", now)
}

pub fn codex_from_rollout(text: &str, source: &str, now: &str) -> Option<Value> {
    let mut last: Option<Value> = None;
    let mut turns = 0i64;
    let mut model = String::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("type").and_then(Value::as_str) == Some("session_meta") {
            let m = s(v.get("payload").and_then(|p| p.get("model")));
            if !m.is_empty() { model = m; }
        }
        if v.get("type").and_then(Value::as_str) == Some("turn_context") {
            let m = s(v.get("payload").and_then(|p| p.get("model")));
            if !m.is_empty() { model = m; }
            turns += 1;
        }
        if v.get("type").and_then(Value::as_str) == Some("event_msg") && v.get("payload").and_then(|p| p.get("type")).and_then(Value::as_str) == Some("token_count") {
            last = v.get("payload").cloned();
        }
    }
    let payload = last?;
    let info = payload.get("info").cloned().unwrap_or(Value::Null);
    let total = info.get("total_token_usage").cloned().unwrap_or(json!({}));
    let lastu = info.get("last_token_usage").cloned().unwrap_or(json!({}));
    let window = num(info.get("model_context_window"));
    let last_total = num(lastu.get("total_tokens"));
    let used_pct = match (last_total, window) { (Some(t), Some(w)) if w > 0.0 => Some((t / w * 100.0 * 10.0).round() / 10.0), _ => None };
    let mut limits: Vec<Value> = Vec::new();
    if let Some(rl) = payload.get("rate_limits").filter(|r| !r.is_null()) {
        for key in ["primary", "secondary"] {
            if let Some(w) = rl.get(key).filter(|w| !w.is_null()) {
                if let Some(used) = num(w.get("used_percent")) {
                    limits.push(json!({ "label": window_label(num(w.get("window_minutes")).unwrap_or(0.0)), "used_pct": used, "resets_at": w.get("resets_at").cloned().unwrap_or(Value::Null) }));
                }
            }
        }
    }
    Some(json!({
        "engine": "codex",
        "t": now,
        "source": source,
        "model": if model.is_empty() { Value::Null } else { json!(model) },
        "context": { "used_pct": used_pct, "input_tokens": lastu.get("input_tokens"), "output_tokens": lastu.get("output_tokens"), "size": window },
        "session": { "tokens_total": total.get("total_tokens"), "input_tokens": total.get("input_tokens"), "output_tokens": total.get("output_tokens"), "cached_input_tokens": total.get("cached_input_tokens"), "reasoning_output_tokens": total.get("reasoning_output_tokens"), "turns": turns },
        "cost_usd": Value::Null,
        "limits": limits,
    }))
}

// ── OpenCode ────────────────────────────────────────────────────────────────────────────────
pub fn opencode_db() -> Option<PathBuf> {
    let base = std::env::var("XDG_DATA_HOME").ok().filter(|h| !h.is_empty()).map(PathBuf::from).or_else(|| home().map(|h| h.join(".local").join("share")))?;
    let p = base.join("opencode").join("opencode.db");
    p.is_file().then_some(p)
}

/// OpenCode: session tallies for this worktree + a 7-day local tally. `proxy` = the antiloki
/// meter for the pane (`{cost_usd, calls}`) when the engine is proxied.
pub fn opencode_usage(worktree: &Path, proxy: Option<Value>, now: &str) -> Value {
    let Some(db_path) = opencode_db() else { return with_proxy(none("opencode", "no OpenCode database on this machine yet", now), proxy) };
    let Ok(db) = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX) else { return with_proxy(none("opencode", "OpenCode database unreadable", now), proxy) };
    let v = opencode_from_db(&db, worktree, &db_path.to_string_lossy(), now).unwrap_or_else(|e| none("opencode", &format!("OpenCode database query failed: {e}"), now));
    with_proxy(v, proxy)
}

fn with_proxy(mut v: Value, proxy: Option<Value>) -> Value {
    if let (Some(o), Some(p)) = (v.as_object_mut(), proxy) { o.insert("proxy".into(), p); }
    v
}

pub fn opencode_from_db(db: &Connection, worktree: &Path, source: &str, now: &str) -> Result<Value, String> {
    let wt = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf()).to_string_lossy().into_owned();
    let mut stmt = db.prepare("SELECT id, directory, time_updated FROM session").map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))).map_err(|e| e.to_string())?;
    let mut ids: Vec<String> = Vec::new();
    for r in rows.flatten() { if same_dir(&r.1, Path::new(&wt)) || r.1 == wt { ids.push(r.0); } }
    let mut session = Map::new();
    let (mut tin, mut tout, mut tcache, mut ttotal, mut cost, mut msgs) = (0.0, 0.0, 0.0, 0.0, 0.0, 0i64);
    let mut model = String::new();
    if !ids.is_empty() {
        let ph = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT data FROM message WHERE session_id IN ({ph}) ORDER BY time_created ASC");
        let mut st = db.prepare(&sql).map_err(|e| e.to_string())?;
        let params: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|x| x as &dyn rusqlite::ToSql).collect();
        let datas = st.query_map(params.as_slice(), |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        for d in datas.flatten() {
            let Ok(m) = serde_json::from_str::<Value>(&d) else { continue };
            if m.get("role").and_then(Value::as_str) != Some("assistant") { continue; }
            msgs += 1;
            let tk = m.get("tokens").cloned().unwrap_or(json!({}));
            tin += num(tk.get("input")).unwrap_or(0.0);
            tout += num(tk.get("output")).unwrap_or(0.0);
            tcache += num(tk.get("cache").and_then(|c| c.get("read"))).unwrap_or(0.0);
            ttotal += num(tk.get("total")).unwrap_or(0.0);
            cost += num(m.get("cost")).unwrap_or(0.0);
            let mid = s(m.get("modelID"));
            if !mid.is_empty() { model = mid; }
        }
    }
    session.insert("tokens_total".into(), json!(ttotal as i64));
    session.insert("input_tokens".into(), json!(tin as i64));
    session.insert("output_tokens".into(), json!(tout as i64));
    session.insert("cached_input_tokens".into(), json!(tcache as i64));
    session.insert("messages".into(), json!(msgs));
    session.insert("sessions".into(), json!(ids.len()));
    // the 7-day local tally — every assistant message on this machine, all projects
    let week_ago_ms = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)) - 7 * 86400 * 1000;
    let mut st = db.prepare("SELECT data FROM message WHERE time_created >= ?1").map_err(|e| e.to_string())?;
    let (mut wtok, mut wcost, mut wmsgs) = (0.0, 0.0, 0i64);
    let datas = st.query_map([week_ago_ms], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
    for d in datas.flatten() {
        let Ok(m) = serde_json::from_str::<Value>(&d) else { continue };
        if m.get("role").and_then(Value::as_str) != Some("assistant") { continue; }
        wmsgs += 1;
        wtok += num(m.get("tokens").and_then(|t| t.get("total"))).unwrap_or(0.0);
        wcost += num(m.get("cost")).unwrap_or(0.0);
    }
    Ok(json!({
        "engine": "opencode",
        "t": now,
        "source": source,
        "model": if model.is_empty() { Value::Null } else { json!(model) },
        "context": { "used_pct": Value::Null, "input_tokens": Value::Null, "output_tokens": Value::Null, "size": Value::Null },
        "session": session,
        "cost_usd": cost,
        "limits": [],
        "weekly_local": { "tokens": wtok as i64, "cost_usd": wcost, "messages": wmsgs },
    }))
}

/// OpenCode: the assistant messages' text parts for this worktree's sessions, newest first.
pub fn opencode_answers(worktree: &Path, limit: usize) -> Vec<Value> {
    let Some(db_path) = opencode_db() else { return Vec::new() };
    let Ok(db) = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX) else { return Vec::new() };
    opencode_answers_from_db(&db, worktree, limit).unwrap_or_default()
}

pub fn opencode_answers_from_db(db: &Connection, worktree: &Path, limit: usize) -> Result<Vec<Value>, String> {
    let wt = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf()).to_string_lossy().into_owned();
    let mut st = db.prepare("SELECT id, directory FROM session").map_err(|e| e.to_string())?;
    let ids: Vec<String> = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(|e| e.to_string())?.flatten().filter(|(_, d)| same_dir(d, Path::new(&wt)) || *d == wt).map(|(id, _)| id).collect();
    if ids.is_empty() { return Ok(Vec::new()); }
    let ph = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("SELECT m.id, m.time_created, m.data, p.data FROM message m JOIN part p ON p.message_id = m.id WHERE m.session_id IN ({ph}) ORDER BY m.time_created DESC, p.time_created ASC");
    let mut st = db.prepare(&sql).map_err(|e| e.to_string())?;
    let params: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|x| x as &dyn rusqlite::ToSql).collect();
    let rows = st.query_map(params.as_slice(), |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))).map_err(|e| e.to_string())?;
    let mut by_msg: Vec<(String, i64, String)> = Vec::new(); // (id, t, text) in message order
    for (id, t, mdata, pdata) in rows.flatten() {
        let Ok(m) = serde_json::from_str::<Value>(&mdata) else { continue };
        if m.get("role").and_then(Value::as_str) != Some("assistant") { continue; }
        let Ok(p) = serde_json::from_str::<Value>(&pdata) else { continue };
        if p.get("type").and_then(Value::as_str) != Some("text") { continue; }
        let text = s(p.get("text"));
        match by_msg.iter_mut().find(|(mid, _, _)| *mid == id) {
            Some(entry) => { entry.2.push('\n'); entry.2.push_str(&text); }
            None => by_msg.push((id, t, text)),
        }
    }
    Ok(by_msg.into_iter().take(limit).map(|(id, t, text)| json!({ "id": id, "t": chrono_like(t), "text": text })).collect())
}

/// epoch ms → `YYYY-MM-DDTHH:MM:SSZ` without pulling a date crate in.
fn chrono_like(ms: i64) -> String {
    let secs = ms / 1000;
    let days = secs.div_euclid(86400); let rem = secs.rem_euclid(86400);
    // civil-from-days (Howard Hinnant)
    let z = days + 719468; let era = z.div_euclid(146097); let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1; let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// One line for the tool answer / the chip title.
pub fn usage_md(u: &Value) -> String {
    let engine = s(u.get("engine"));
    if s(u.get("source")) == "none" { return format!("# Usage ({engine})\n\n_{}_", s(u.get("reason"))); }
    let mut bits: Vec<String> = Vec::new();
    if let Some(m) = u.get("model").filter(|m| !m.is_null()) { bits.push(s(Some(m))); }
    if let Some(p) = num(u.get("context").and_then(|c| c.get("used_pct"))) { bits.push(format!("context {}%", p.round() as i64)); }
    if let Some(t) = num(u.get("session").and_then(|c| c.get("tokens_total"))) { bits.push(format!("session {} tokens", fmt_k(t))); }
    if let Some(c) = num(u.get("cost_usd")) { bits.push(format!("${c:.2}")); }
    for l in u.get("limits").and_then(Value::as_array).cloned().unwrap_or_default() {
        bits.push(format!("{} {}%", s(l.get("label")), num(l.get("used_pct")).unwrap_or(0.0).round() as i64));
    }
    if let Some(w) = u.get("weekly_local") { bits.push(format!("7-day local {} tokens · ${:.2}", fmt_k(num(w.get("tokens")).unwrap_or(0.0)), num(w.get("cost_usd")).unwrap_or(0.0))); }
    if let Some(p) = u.get("proxy") { bits.push(format!("antiloki proxy ${:.4} · {} calls", num(p.get("cost_usd")).unwrap_or(0.0), num(p.get("calls")).unwrap_or(0.0) as i64)); }
    format!("# Usage ({engine})\n\n{}\n\nsource: {}", bits.join(" · "), s(u.get("source")))
}

pub fn fmt_k(n: f64) -> String {
    if n >= 1_000_000.0 { format!("{:.1}M", n / 1_000_000.0) } else if n >= 1000.0 { format!("{}k", (n / 1000.0).round() as i64) } else { format!("{}", n as i64) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_labels() {
        assert_eq!(window_label(300.0), "5h");
        assert_eq!(window_label(10080.0), "week");
        assert_eq!(window_label(43200.0), "30d");
        assert_eq!(window_label(90.0), "2h");
    }

    #[test]
    fn codex_rollout_is_read_to_the_last_token_count() {
        let text = r#"{"timestamp":"t","type":"session_meta","payload":{"cwd":"/wt","model":"gpt-5-codex"}}
{"type":"turn_context","payload":{"model":"gpt-5-codex"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":10,"total_tokens":12},"last_token_usage":{"total_tokens":12},"model_context_window":100},"rate_limits":null}}
not json
{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":979929,"cached_input_tokens":907008,"output_tokens":9707,"total_tokens":989636},"last_token_usage":{"input_tokens":66521,"output_tokens":48,"total_tokens":66569},"model_context_window":258400},"rate_limits":{"primary":{"used_percent":22.0,"window_minutes":300,"resets_at":1791581953},"secondary":{"used_percent":41.5,"window_minutes":10080,"resets_at":1791981953}}}}"#;
        let v = codex_from_rollout(text, "f", "now").unwrap();
        assert_eq!(v["engine"], json!("codex"));
        assert_eq!(v["model"], json!("gpt-5-codex"));
        assert_eq!(v["session"]["tokens_total"], json!(989636));
        assert_eq!(v["session"]["turns"], json!(1));
        assert_eq!(v["context"]["used_pct"], json!(25.8));
        assert_eq!(v["limits"], json!([{ "label": "5h", "used_pct": 22.0, "resets_at": 1791581953 }, { "label": "week", "used_pct": 41.5, "resets_at": 1791981953 }]));
        let md = usage_md(&v);
        assert!(md.contains("gpt-5-codex · context 26% · session 990k tokens · 5h 22% · week 42%"), "{md}");
        assert!(codex_from_rollout("{\"type\":\"session_meta\",\"payload\":{}}", "f", "now").is_none(), "no token_count → none");
    }

    #[test]
    fn opencode_db_is_summed_per_worktree_and_per_week() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL, time_updated INTEGER NOT NULL); CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);").unwrap();
        let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
        db.execute("INSERT INTO session VALUES ('s1', '/wt', 1), ('s2', '/other', 1)", []).unwrap();
        db.execute("INSERT INTO message VALUES ('m1','s1',?1,'{\"role\":\"assistant\",\"tokens\":{\"total\":100,\"input\":60,\"output\":40,\"cache\":{\"read\":5}},\"cost\":0.5,\"modelID\":\"big-pickle\"}'), ('m2','s1',?1,'{\"role\":\"user\"}'), ('m3','s2',?1,'{\"role\":\"assistant\",\"tokens\":{\"total\":1000,\"input\":1,\"output\":1},\"cost\":1.5}'), ('m4','s2',?2,'{\"role\":\"assistant\",\"tokens\":{\"total\":5000},\"cost\":9}')", rusqlite::params![now_ms - 1000, now_ms - 10 * 86400 * 1000]).unwrap();
        let v = opencode_from_db(&db, Path::new("/wt"), "db", "now").unwrap();
        assert_eq!(v["session"]["tokens_total"], json!(100));
        assert_eq!(v["session"]["messages"], json!(1));
        assert_eq!(v["cost_usd"], json!(0.5));
        assert_eq!(v["model"], json!("big-pickle"));
        assert_eq!(v["weekly_local"]["tokens"], json!(1100), "the 10-day-old message is out of the week");
        assert_eq!(v["weekly_local"]["cost_usd"], json!(2.0));
        let md = usage_md(&with_proxy(v, Some(json!({ "cost_usd": 0.0123, "calls": 3 }))));
        assert!(md.contains("7-day local 1k tokens · $2.00 · antiloki proxy $0.0123 · 3 calls"), "{md}");
    }

    #[test]
    fn opencode_answers_join_text_parts_newest_first() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL, time_updated INTEGER NOT NULL); CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL); CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL);").unwrap();
        db.execute("INSERT INTO session VALUES ('s1','/wt',1)", []).unwrap();
        db.execute("INSERT INTO message VALUES ('m1','s1',1789306211178,'{\"role\":\"assistant\"}'), ('m2','s1',1789306300000,'{\"role\":\"assistant\"}'), ('u1','s1',1789306400000,'{\"role\":\"user\"}')", []).unwrap();
        db.execute("INSERT INTO part VALUES ('p1','m1','s1',1,'{\"type\":\"text\",\"text\":\"hello\"}'), ('p2','m1','s1',2,'{\"type\":\"text\",\"text\":\"world\"}'), ('p3','m1','s1',3,'{\"type\":\"tool\",\"tool\":\"read\"}'), ('p4','m2','s1',4,'{\"type\":\"text\",\"text\":\"later\"}'), ('p5','u1','s1',5,'{\"type\":\"text\",\"text\":\"user text\"}')", []).unwrap();
        let a = opencode_answers_from_db(&db, Path::new("/wt"), 10).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0]["text"], json!("later"));
        assert_eq!(a[0]["t"], json!("2026-09-13T13:31:40Z"));
        assert_eq!(a[1]["text"], json!("hello\nworld"));
    }

    #[test]
    fn claude_reads_the_statusline_file_or_says_none() {
        let d = std::env::temp_dir().join(format!("antiloki-usage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join(".antiloki")).unwrap();
        let none = claude_usage(&d, 7, "now");
        assert_eq!(none["source"], json!("none"));
        std::fs::write(d.join(".antiloki/usage-7.json"), r#"{"model":"Opus 5","context":{"used_pct":12},"cost_usd":0.42,"limits":[{"label":"5h","used_pct":23.5,"resets_at":1}]}"#).unwrap();
        let v = claude_usage(&d, 7, "now");
        assert_eq!(v["engine"], json!("claude-code"));
        assert!(usage_md(&v).contains("Opus 5 · context 12% · $0.42 · 5h 24%"));
        // A SECOND AGENT ON THE SAME BOX (2026-09-24). It has no meter of its own, so it has spent nothing — it must
        // not inherit pane 7's $0.42. The rate-limit windows are the subscription's, though, and stay true for it.
        let other = claude_usage(&d, 99, "now");
        assert_eq!(other["source"], json!(".antiloki (account windows)"), "the windows are reported, not a stranger's spend");
        // …and WHEN they were taken, because a window only moves when a CLI rewrites its status file
        assert!(other.get("measured_at").is_some(), "the meter's own stamp rides along, so a banner can say how old it is");
        assert_eq!(other["limits"][0]["used_pct"], json!(23.5));
        assert!(other.get("cost_usd").is_none(), "no inherited cost");
        assert!(other.get("session").is_none(), "no inherited tokens");
        let _ = std::fs::remove_dir_all(&d);
    }
}
