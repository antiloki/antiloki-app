//! The HTTP surface of the goal runs, the temporary subagents and the CLI traces — the routes the
//! NestJS chat.controller proxied to its worker thread (2026-09-17), served here directly:
//!   GET  /api/v1/cockpit/goals?parent=|id=        POST /api/v1/cockpit/goals/{op}
//!   GET  /api/v1/cockpit/subagents?parent=        POST /api/v1/cockpit/subagents/{op}
//!   GET  /api/v1/cli-llm/traces?since=
use crate::cli::{engine_of, Engine};
use crate::dispatch::{engine_call, AppState};
use crate::goals::{Goals, Subagents};
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// THE SEAT A REQUEST WITHOUT AN `engine` RUNS ON: the one Heimdall plans on, never a hard-coded Claude (2026-10-01: with Claude switched off, a body that named no engine planned on a seat that
/// refuses every call). Claude only when nothing at all can run - the refusal the caller then gets is the honest one.
fn default_seat() -> Engine { crate::cli::heimdall_engine(None).unwrap_or(Engine::ClaudeCode) }
fn body_json(body: &Bytes) -> Value { serde_json::from_slice(body).unwrap_or_else(|_| json!({})) }
fn n(v: Option<&Value>) -> i64 { v.and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64)).or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))).unwrap_or(0) }
fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }
fn ok(v: Value) -> Response { (StatusCode::OK, Json(v)).into_response() }
fn bad(msg: &str) -> Response { (StatusCode::BAD_REQUEST, Json(json!({ "error": { "message": msg } }))).into_response() }
fn root() -> String { std::env::var("ANTILOKI_SUBSTRATE_ROOT").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()) }

/// HEIMDALL'S LEDGER over the agents: per engine × tier, samples and first-attempt pass rate, the bias in force
/// the ledger view + THE SAVINGS TOTALS (2026-09-20): every settled run's API-equivalent saved by the
/// router, summed — "tokens or cost saved by using the router", always visible
pub async fn goals_ledger(state: Arc<AppState>, goals: Arc<Goals>) -> Response {
    let mut v = goals.ledger().summary();
    // THIS WORKSPACE'S RUNS ONLY (operator 2026-09-30: "the value that appears on the switcher is still wrong, is from another workspace") - the chip in the header reads this route
    let root = active_root(&state).await;
    let runs = runs_in_workspace(goals.list(None), &root);
    let settled: Vec<&crate::goals::GoalRun> = runs.iter().filter(|r| r.savings.is_some()).collect();
    let routed: Vec<&crate::goals::GoalRun> = settled.iter().copied().filter(|r| r.router && !r.min).collect();
    // MIN's column (2026-09-21): the turns taken with exploration off, priced against the same turns with it
    let min_runs: Vec<&crate::goals::GoalRun> = settled.iter().copied().filter(|r| r.min).collect();
    let sum = |rs: &[&crate::goals::GoalRun], f: fn(&crate::goals::Savings) -> f64| rs.iter().filter_map(|r| r.savings.as_ref()).map(f).sum::<f64>();
    let tokens: i64 = routed.iter().filter_map(|r| r.savings.as_ref()).map(|s| s.tokens_in + s.tokens_out).sum();
    let (calls, calls_usd) = crate::audit::totals();   // every measured call on this machine (the first-run check's tiny ones too): the chip shows from the first one
    v["savings"] = json!({ "calls": calls, "calls_usd": (calls_usd * 10000.0).round() / 10000.0, "runs": routed.len(), "tokens": tokens, "actual_usd": sum(&routed, |s| s.actual_usd), "baseline_usd": sum(&routed, |s| s.baseline_usd), "saved_usd": sum(&routed, |s| s.saved_usd), "unrouted_runs": settled.len() - routed.len() - min_runs.len(), "min_turns": min_runs.len(), "min_saved_usd": sum(&min_runs, |s| s.saved_usd), "min_actual_usd": sum(&min_runs, |s| s.actual_usd), "min_note": format!("MIN turns priced against the same turn with exploration — the measured share ({:.0}%) put back; an estimate", crate::goals::MIN_EXPLORE_SHARE * 100.0), "note": "API-equivalent list prices — a subscription pays $0 metered; the baseline is one high-tier agent doing the same tokens" });
    ok(v)
}

pub async fn goals_get(state: Arc<AppState>, goals: Arc<Goals>, q: HashMap<String, String>) -> Response {
    if let Some(id) = q.get("id").and_then(|v| v.trim().parse::<i64>().ok()).filter(|i| *i > 0) {
        return match goals.get(id) { Some(r) => ok(json!({ "run": r })), None => (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "no such run" } }))).into_response() };
    }
    let runs = goals.list(q.get("parent").map(String::as_str));
    // `scope=workspace` (2026-10-09): only this workspace's runs, the SAME rule the header chip (goals/ledger) and the fuel window use - a pane's saved sum read from this list can then never exceed the total
    // (run 273 sat in the old registry folder: the chip skipped it, the pane menu counted it, and the menu was $0.13 bigger than the sum of all agents)
    let runs = if q.get("scope").map(String::as_str) == Some("workspace") { let root = active_root(&state).await; runs_in_workspace(runs, &root) } else { runs };
    ok(json!({ "runs": runs }))
}

/// GET /api/v1/cockpit/timeline?tz=<minutes east>&limit=N — the merged graph and the work stats, in one call.
pub async fn timeline_get(state: Arc<AppState>, agents: Arc<crate::agent::AgentRegistry>, goals: Arc<Goals>, q: HashMap<String, String>) -> Response {
    let tz = q.get("tz").and_then(|v| v.trim().parse::<i64>().ok()).filter(|m| m.abs() <= 14 * 60).unwrap_or(0);
    let limit = q.get("limit").and_then(|v| v.trim().parse::<usize>().ok()).filter(|l| (10..=2000).contains(l)).unwrap_or(400);
    let root = active_root(&state).await;
    let box_parent = if root.is_empty() { String::new() } else { crate::boxes::box_of(&root, "__scope__").parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default() };
    let runs: Vec<Value> = goals.list(None).into_iter().filter(|r| timeline_run_visible(r, &root, &box_parent)).map(|r| json!(r)).collect();

    // every agent pane's OWN log — the attribution the box's shared reads.jsonl cannot give
    let conn = state.pick();
    let panes = tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        antiloki_rs::db_json::query_rows(&c, "SELECT p.id, p.subagent_name, p.worktree_path, p.settings_json FROM cockpit_panes p JOIN cockpit_sessions s ON s.id = p.session_id WHERE s.status != 'closed' AND COALESCE(p.status,'') != 'closed' AND p.worktree_path IS NOT NULL AND p.worktree_path != '' AND p.pane_kind IN ('claude-code','codex','opencode')", &[]).unwrap_or(json!([]))
    }).await.unwrap_or(json!([]));

    let mut frames: Vec<Value> = vec![];
    let mut times: Vec<i64> = vec![];
    for p in panes.as_array().cloned().unwrap_or_default() {
        let id = p.get("id").and_then(Value::as_i64).unwrap_or(0);
        let wt = p.get("worktree_path").and_then(Value::as_str).unwrap_or("").to_string();
        let settings = serde_json::from_str::<Value>(p.get("settings_json").and_then(Value::as_str).unwrap_or("{}")).unwrap_or(json!({}));
        let name = settings.get("title").and_then(Value::as_str)
            .or_else(|| settings.get("agent").and_then(Value::as_str))
            .or_else(|| p.get("subagent_name").and_then(Value::as_str)).unwrap_or("agent").to_string();
        let text = tokio::task::spawn_blocking(move || crate::chat_pane::events_text(&wt, id)).await.unwrap_or_default();
        if text.is_empty() { continue }
        let mine = crate::work_stats::frames_of_log(text.lines(), &name, 240);
        for f in &mine { if let Some(t) = f.get("t").and_then(Value::as_i64) { times.push(t) } }
        frames.extend(mine);
    }
    for r in &runs { frames.extend(crate::work_stats::frames_of_run(r)); }
    let agents_n = agents.live_count();
    ok(json!({ "ok": true, "frames": crate::work_stats::merge_frames(frames, limit),
        "agents": agents_n, "stats": crate::work_stats::work_stats(&runs, &times, tz) }))
}

/// GET /api/v1/cockpit/fuel?tz=<minutes east>&days=<window> — what has been spent, and what Heimdall saved.
///
/// Built on `brain.ai_calls` rather than on the engines' status files: a meter file does not exist until an agent
/// has spoken, which is why fuel read empty for an idle agent. The rate-limit WINDOWS still come from the
/// engines, because only the CLI knows how much of a subscription is left.
pub async fn fuel_get(state: Arc<AppState>, goals: Arc<Goals>, q: HashMap<String, String>) -> Response {
    let asked = q.get("tz").and_then(|v| v.trim().parse::<i64>().ok()).filter(|m| m.abs() <= 14 * 60); if let Some(t) = asked { crate::cli_usage::screen_asked(t); }
    let tz = asked.or_else(crate::cli_usage::screen_tz).unwrap_or(0);
    let days = q.get("days").and_then(|v| v.trim().parse::<i64>().ok()).filter(|d| (0..=365).contains(d)).unwrap_or(7);
    // INSTANT ON REOPEN (operator 2026-09-24: "fuel takes a lot of time to display the data should be instant").
    // Four sqlite opens and a ledger walk is ~750 ms — fine once, and the whole wait every time the operator
    // hovers the gauge. Two seconds of cache per (window, timezone): long enough that opening, closing and
    // reopening is free, short enough that a running agent's spend is never stale on screen.
    //
    // …PER WORKSPACE TOO (found 2026-10-01): the key was (window, timezone) only, and the body is one workspace's - its calls, its runs' savings - so a second workspace
    // activated inside the two seconds was served the first one's numbers. The root is part of the key, which means it is read before the cache is asked.
    // THE TOTALS ARE SCANNED WHEN THE PERSON ASKS, OR ONCE AN HOUR (operator 2026-11-26: "this should happen when clicked on usage, or once each 1 hour, not all the time"). One scan reads gigabytes of history; it
    // ran for every poll of every gauge and they piled up (the serve at 158 % CPU for half an hour). One scan per key at a time, detached from the request, so a closed request never starts it over.
    let root = active_root(&state).await;
    if root.is_empty() { return ok(json!({ "ok": true, "models": [], "houses": [], "saved": crate::fuel::savings_of(&[]), "note": "no workspace open" })) }
    let refresh = q.get("refresh").is_some_and(|v| v == "1");
    let ttl = std::env::var("ANTILOKI_FUEL_TTL_MS").ok().and_then(|v| v.parse::<i64>().ok()).unwrap_or(3_600_000);
    let key = fuel_cache_key(days, tz, &root);
    if ttl <= 0 { return ok(fuel_body(state, goals, root, days, tz).await); }
    let cached = FUEL_CACHE.lock().ok().and_then(|c| c.get(&key).cloned());
    if !refresh { if let Some(hit) = fuel_cache_hit(cached.as_ref(), crate::cli::now_ms(), ttl) { return ok(hit); } }
    let before = cached.as_ref().map(|c| c.0).unwrap_or(0);
    if FUEL_JOBS.lock().map(|mut j| j.insert(key.clone())).unwrap_or(false) {
        let k = key.clone();
        tokio::spawn(async move {
            // run in a task of its own so that even a panic frees the key
            if let Ok(body) = tokio::spawn(fuel_body(state, goals, root, days, tz)).await { if let Ok(mut c) = FUEL_CACHE.lock() { c.insert(k.clone(), (crate::cli::now_ms(), body)); } }
            if let Ok(mut j) = FUEL_JOBS.lock() { j.remove(&k); }
        });
    }
    // an older answer is served at once; the person's own click waits a while for the new one
    if !refresh { if let Some((_, body)) = cached { return ok(body); } }
    for _ in 0..75 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if let Some((at, body)) = FUEL_CACHE.lock().ok().and_then(|c| c.get(&key).cloned()) { if at > before { return ok(body); } }
        if !FUEL_JOBS.lock().map(|j| j.contains(&key)).unwrap_or(false) { break; }
    }
    match FUEL_CACHE.lock().ok().and_then(|c| c.get(&key).cloned()) { Some((_, body)) => ok(body), None => ok(json!({ "ok": false, "pending": true })) }
}

/// One full scan of what was spent: the heavy part.
async fn fuel_body(state: Arc<AppState>, goals: Arc<Goals>, root: String, days: i64, tz: i64) -> Value {
    let r2 = root.clone();
    let (models, prior) = tokio::task::spawn_blocking(move || (
        crate::episodes_db::calls_by_model(&r2, days, tz),
        crate::episodes_db::calls_by_model_prior(&r2, days, tz),
    )).await.unwrap_or((vec![], vec![]));
    // THIS WORKSPACE'S RUNS ONLY (operator 2026-09-30: "the preview of money saved is holding another workspace's"). The run ledger is one file for the whole machine; summing it
    // all put every project's savings on every project's screen.
    let runs: Vec<Value> = runs_in_workspace(goals.list(None), &root).into_iter().map(|r| json!(r)).collect();
    let saved = crate::fuel::savings_of(&runs);
    // per house, for the three lines the operator reads at a glance
    let mut houses: Vec<Value> = vec![];
    // EVERY HOUSE THAT MADE A CALL, NOT THREE OF THEM (operator 2026-09-30: "where is cursor, gemini, grok limits etc?"). The three subscriptions with windows
    // come first, then whatever else the record shows (Cursor, Gemini, Grok ...), then any seat currently out of quota.
    let mut house_keys: Vec<String> = ["claude", "codex", "opencode"].iter().map(|s| s.to_string()).collect();
    for m in &models { if let Some(h) = m.get("house").and_then(Value::as_str) { if !house_keys.iter().any(|k| k == h) && h != "unknown" { house_keys.push(h.to_string()); } } }
    for e in crate::cli::Engine::all() { if crate::cli::seat_limit(e).is_some() { let h = crate::fuel::house_of(e.key()); if !house_keys.contains(&h) { house_keys.push(h); } } }
    for h in house_keys.iter().map(String::as_str) {
        let mine: Vec<&Value> = models.iter().filter(|m| m.get("house").and_then(Value::as_str) == Some(h)).collect();
        let sum = |k: &str| mine.iter().map(|m| m.get(k).and_then(Value::as_f64).unwrap_or(0.0)).sum::<f64>();
        houses.push(json!({ "house": h, "calls": sum("calls"), "cost": sum("cost"),
            "tokens_in": sum("tokens_in"), "tokens_out": sum("tokens_out"), "tokens": sum("tokens"),
            "failed": sum("failed"), "models": mine.len() }));
    }
    // THE TEAMS' OWN MONEY: a house of its own, so the total under the bars is what the workspace really spent
    let (tcalls, tcost) = { let r3 = root.clone(); let since = window_start_ms(crate::cli::now_ms() as i64, days, tz); tokio::task::spawn_blocking(move || team_spend(&crate::team_graph_run::runs_of(&r3, ""), since)).await.unwrap_or((0, 0.0)) };
    // WHAT THE SAME TOKENS WOULD COST AT OPENROUTER, per model (a daily list, read off the request thread)
    let prices = tokio::task::spawn_blocking(crate::openrouter::table).await.unwrap_or_default();
    let today = day_of(crate::cli::now_ms() as i64, tz);
    // WHAT THE CLIs REALLY USED, from their own session files (Claude Code, Codex): per model and day, with the cache apart
    // read twice the window, so the period BEFORE it is known too (the "+35% since the period before" line)
    let cli_all = { let t2 = today.clone(); tokio::task::spawn_blocking(move || crate::cli_usage::scan(if days > 0 { days * 2 } else { 1 }, tz, &t2)).await.unwrap_or_default() };
    let split = if days > 0 { window_first_day(&today, days) } else { today.clone() };
    let (cli, cli_prev): (Vec<_>, Vec<_>) = cli_all.into_iter().partition(|r| r.0.as_str() >= split.as_str() && (days > 0 || r.0 == today));
    let prev_cost: f64 = { let t = crate::openrouter::table(); cli_prev.iter().filter(|r| days > 0 || r.0.as_str() < today.as_str()).filter_map(|(_, m, _, u)| crate::openrouter::price_for(m, &t).map(|p| crate::openrouter::cost_cached(p, u.inp as f64, u.out as f64, u.cr as f64, u.cw as f64))).sum() };
    let mut rows_dm = { let r4 = root.clone(); tokio::task::spawn_blocking(move || crate::episodes_db::calls_by_day_model(&r4, days, tz)).await.unwrap_or_default() }.into_iter().map(|(d, m, i, o)| (d, m, i, o, 0i64, 0i64)).collect::<Vec<_>>();
    for (d, m, _h, u) in &cli { rows_dm.push((d.clone(), m.clone(), u.inp as i64, u.out as i64, u.cr as i64, u.cw as i64)); }
    let runs_all = { let r5 = root.clone(); tokio::task::spawn_blocking(move || crate::team_graph_run::runs_of(&r5, "")).await.unwrap_or_default() };
    let daily = daily_series(&rows_dm, &runs_all, if days > 0 { days } else { 0 }, &today, &prices, tz);
    let mut models: Vec<Value> = models.iter().map(|m| crate::openrouter::priced(m, &prices)).collect();
    { let mut by: std::collections::BTreeMap<(String, String), crate::cli_usage::Use> = Default::default();
      for (_d, m, h, u) in &cli { let e = by.entry((h.clone(), m.clone())).or_default(); e.inp += u.inp; e.out += u.out; e.cr += u.cr; e.cw += u.cw; e.calls += u.calls; }
      for ((h, m), u) in by { models.push(crate::openrouter::priced(&json!({ "model": m, "house": h, "tier": "cli", "calls": u.calls, "cost": 0.0, "tokens": u.inp + u.out + u.cr + u.cw, "tokens_in": u.inp, "tokens_out": u.out, "cache_read": u.cr, "cache_write": u.cw, "failed": 0, "ms": 0, "source": "session files" }), &prices)); } }
    let or_total: f64 = models.iter().filter_map(|m| m["or_cost"].as_f64()).sum();
    if tcalls > 0 { houses.push(json!({ "house": "teams", "calls": tcalls, "cost": tcost, "tokens_in": 0, "tokens_out": 0, "tokens": 0, "failed": 0, "models": 1 })); models.push(json!({ "model": "team runs", "house": "teams", "tier": "", "calls": tcalls, "cost": tcost, "tokens": 0, "failed": 0, "ms": 0 })); }
    // THE WINDOWS still come from the engines — only the CLI knows how much of a subscription is left. Read from
    // the meters on the box rather than by asking every pane, which is what made this forty round-trips.
    let limits = tokio::task::spawn_blocking({ let r = root.clone(); move || account_windows(&r) }).await.unwrap_or_default();
    let mut limits = limits;
    crate::cli::set_windows(&limits);      // the same figures the router checks BEFORE a job (cli::seat_limit)
    // a seat that said "you've hit your usage limit" is at 100% until it is back, whatever window it was - and it says so in the CLI's own words
    for e in crate::cli::Engine::all() { if let Some((until, said)) = crate::cli::seat_limit(e) {
        limits.push(json!({ "house": crate::fuel::house_of(e.key()), "label": "limit", "used_pct": 100.0, "resets_at": until / 1000, "note": said }));
    } }
    // per PANE, from the serve's own audit — the only table that records which pane made a call
    let panes = tokio::task::spawn_blocking(crate::audit::by_pane).await.unwrap_or_default();
    let body = json!({ "ok": true, "days": days, "models": models, "houses": houses, "saved": saved, "limits": limits, "panes": panes, "or_cost": or_total, "or_prev": prev_cost, "daily": daily,
        "compare": crate::fuel::compare(&models, &prior),
        "analysis": crate::fuel::analysis(&models, &saved) });
    body
}
/// The cache key of one fuel body: the window, the timezone AND the workspace root it was built for. Pure - tested.
/// WHAT THE TEAMS SPENT (operator 2026-11-21: the bottom bar said $0.00 while the sagas said $0.08): a team run's money is not a row of the call record, it is the run's own `spent_usd`. The runs that started inside the window, counted and summed. Pure.
pub fn team_spend(runs: &[Value], since_ms: i64) -> (u64, f64) {
    let mine: Vec<&Value> = runs.iter().filter(|r| r["started_ms"].as_i64().or_else(|| r["started_ms"].as_f64().map(|f| f as i64)).map_or(false, |t| t >= since_ms)).collect();
    (mine.len() as u64, mine.iter().map(|r| r["spent_usd"].as_f64().unwrap_or(0.0)).sum())
}
/// The per-day line: each day of the window with its cost at OpenRouter's prices (model tokens priced, plus the team runs that started that day, at what they spent). Missing days are zero, so the line has one point per day. Pure.
pub fn daily_series(rows: &[(String, String, i64, i64, i64, i64)], runs: &[Value], days: i64, today: &str, table: &[(String, f64, f64)], tz_min: i64) -> Vec<Value> {
    use std::collections::BTreeMap; let mut by: BTreeMap<String, f64> = BTreeMap::new();
    for (d, m, i, o, cr, cw) in rows { if let Some(p) = crate::openrouter::price_for(m, table) { *by.entry(d.clone()).or_insert(0.0) += crate::openrouter::cost_cached(p, *i as f64, *o as f64, *cr as f64, *cw as f64); } }
    for r in runs { if let Some(ms) = r["started_ms"].as_i64() { let d = day_of(ms, tz_min); *by.entry(d).or_insert(0.0) += r["spent_usd"].as_f64().unwrap_or(0.0); } }
    let end = ymd_to_days(today); (0..days.max(0)).rev().filter_map(|k| end.map(|e| days_to_ymd(e - k))).map(|d| json!({ "day": d.clone(), "cost": by.get(&d).copied().unwrap_or(0.0) })).collect()
}
/// The first day of a window of `days` days ending today. Pure.
pub fn window_first_day(today: &str, days: i64) -> String { ymd_to_days(today).map(|e| days_to_ymd(e - (days - 1).max(0))).unwrap_or_else(|| today.to_string()) }
fn day_of(ms: i64, tz_min: i64) -> String { days_to_ymd(((ms / 60_000 + tz_min) as f64 / 1440.0).floor() as i64) }
/// Days since 1970-01-01 → `YYYY-MM-DD` (civil-from-days). Pure.
fn days_to_ymd(z: i64) -> String { let z = z + 719_468; let era = z.div_euclid(146_097); let doe = z.rem_euclid(146_097); let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; let y = yoe + era * 400; let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); let mp = (5 * doy + 2) / 153; let d = doy - (153 * mp + 2) / 5 + 1; let m = if mp < 10 { mp + 3 } else { mp - 9 }; format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d) }
fn ymd_to_days(s: &str) -> Option<i64> { let p: Vec<i64> = s.split('-').filter_map(|x| x.parse().ok()).collect(); if p.len() != 3 { return None } let (y, m, d) = (p[0], p[1], p[2]); let y = if m <= 2 { y - 1 } else { y }; let era = y.div_euclid(400); let yoe = y.rem_euclid(400); let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; Some(era * 146_097 + doe - 719_468) }
/// Where the fuel window starts: `days` back, or this local day's midnight for 0. Pure.
pub fn window_start_ms(now_ms: i64, days: i64, tz_min: i64) -> i64 { if days > 0 { now_ms - days * 86_400_000 } else { let local = now_ms + tz_min * 60_000; local - local.rem_euclid(86_400_000) - tz_min * 60_000 } }
pub fn fuel_cache_key(days: i64, tz: i64, root: &str) -> String { format!("{days}:{tz}:{root}") }
/// The cached body while it is younger than `ttl_ms`. Pure - tested.
pub fn fuel_cache_hit(cache: Option<&(i64, Value)>, now_ms: i64, ttl_ms: i64) -> Option<Value> {
    cache.filter(|(at, _)| now_ms - at < ttl_ms).map(|(_, v)| v.clone())
}
/// key (window, timezone, workspace) -> when it was scanned, and the body; and the keys being scanned now.
static FUEL_CACHE: std::sync::Mutex<std::collections::BTreeMap<String, (i64, Value)>> = std::sync::Mutex::new(std::collections::BTreeMap::new());
static FUEL_JOBS: std::sync::Mutex<std::collections::BTreeSet<String>> = std::sync::Mutex::new(std::collections::BTreeSet::new());

/// The folder of the workspace the serve is bound to right now ("" when none is open).
pub async fn active_root(state: &Arc<AppState>) -> String {
    let conn = state.pick();
    tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        antiloki_rs::engine_workspace_root::getWorkspaceRoot(&c).as_str().unwrap_or("").to_string()
    }).await.unwrap_or_default()
}

/// Does a run's working folder belong to this workspace - the folder itself, or anything under it (a sub-folder or a worktree). Slashes of either kind. Pure - tested.
pub fn in_workspace(cwd: &str, root: &str) -> bool {
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_string();
    let (c, r) = (norm(cwd), norm(root));
    !r.is_empty() && (c == r || c.starts_with(&format!("{r}/")))
}
/// 2026-10-13: timeline includes this workspace's tree and its agent boxes, not every path under ANTILOKI_HOME/agents.
pub fn timeline_run_visible(r: &crate::goals::GoalRun, root: &str, box_parent: &str) -> bool {
    if in_workspace(&r.cwd, root) { return true; }
    !box_parent.is_empty() && in_workspace(&r.cwd, box_parent)
}
pub fn runs_in_workspace(runs: Vec<crate::goals::GoalRun>, root: &str) -> Vec<crate::goals::GoalRun> { runs.into_iter().filter(|r| in_workspace(&r.cwd, root)).collect() }

/// POST /api/v1/cockpit/checkpoints/{op} — list|save|rewind|redo for a pane's worktree (refs/antiloki/cp/pane-{id}/*, HEAD untouched).
pub async fn checkpoints_op(state: Arc<AppState>, op: String, body: Bytes) -> Response {
    let b = body_json(&body);
    let pane_id = n(b.get("pane_id"));
    if pane_id <= 0 { return bad("pane_id required"); }
    let conn = state.pick();
    let row = tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        antiloki_rs::db_json::query_rows(&c, "SELECT id, worktree_path FROM cockpit_panes WHERE id = ?1", &[json!(pane_id)]).ok()
    }).await.unwrap_or(None);
    let wt = row.and_then(|v| v.as_array()?.first()?.get("worktree_path")?.as_str().map(String::from)).unwrap_or_default();
    if wt.is_empty() { return bad("this agent shares its folder, so there is nothing of its own to rewind: a rewind would also undo other work in it"); }
    let cwd = wt;
    let scope = format!("pane-{pane_id}");
    let now = crate::cli::now_ms();
    match op.as_str() {
        "list" => {
            let hist: Vec<Value> = crate::checkpoint::history(&cwd, &scope, 80).into_iter().map(|(n, rev, subject)| {
                let label = subject.split_once(" · ").map(|(_, l)| l.to_string()).unwrap_or_else(|| subject.clone());
                let when = crate::checkpoint::committed_at(&cwd, &rev);
                json!({ "n": n, "rev": rev, "short": crate::checkpoint::short_rev(&rev), "subject": subject, "label": label, "when": when })
            }).collect();
            let redo = crate::checkpoint::peek_redo(&scope).map(|t| json!({ "tree": t }));
            ok(json!({ "ok": true, "checkpoints": hist, "redo": redo }))
        }
        "save" => {
            let label = s(b.get("label")).trim().to_string();
            let label = if label.is_empty() { "manual".into() } else { label };
            let st = json!({});
            let cp = tokio::task::spawn_blocking(move || crate::checkpoint::take_if_changed(&cwd, &scope, &label, &st, now)).await.unwrap_or(None);
            match cp {
                Some(c) => ok(json!({ "ok": true, "n": c.n, "rev": c.rev, "short": crate::checkpoint::short_rev(&c.rev), "label": c.label })),
                None => ok(json!({ "ok": false, "error": "could not snapshot the working tree" })),
            }
        }
        "rewind" => {
            let n_cp = n(b.get("n"));
            let rev_in = s(b.get("rev"));
            let target = if !rev_in.is_empty() { rev_in } else {
                crate::checkpoint::history(&cwd, &scope, 200).into_iter().find(|(num, _, _)| *num == n_cp as u32).map(|(_, rev, _)| rev).unwrap_or_default()
            };
            if target.is_empty() { return bad("checkpoint not found"); }
            let Some(to_tree) = crate::checkpoint::tree_of(&cwd, &target) else { return bad("checkpoint tree missing"); };
            let cur = crate::checkpoint::current_tree(&cwd);
            if let Some(t) = cur.as_ref() { crate::checkpoint::remember_redo(&scope, t); }
            let n_files = crate::checkpoint::restore(&cwd, &to_tree);
            ok(json!({ "ok": true, "restored": n_files, "n": n_cp, "rev": target }))
        }
        "redo" => {
            let Some(tree) = crate::checkpoint::take_redo(&scope) else { return bad("nothing to redo"); };
            let n_files = crate::checkpoint::restore(&cwd, &tree);
            ok(json!({ "ok": true, "restored": n_files }))
        }
        _ => bad("unknown checkpoint op"),
    }
}

/// Claude's windows are the ACCOUNT's, not the folder's: the newest meter any Claude pane on this box wrote, whichever workspace it ran in (operator 2026-09-30: the limits preview showed
/// only Codex). A window that has already reset is dropped - its percentage describes a period that is over.
pub fn claude_windows_anywhere(roots: &[std::path::PathBuf], now_secs: i64) -> Vec<Value> {
    let now = antiloki_rs::clock::now_iso_nodb();
    let mut best: Option<(String, Vec<Value>)> = None;
    for r in roots {
        let u = antiloki_rs::cockpit__helpers_engine_usage::claude_usage(r, 0, &now);
        let ls = u.get("limits").and_then(Value::as_array).cloned().unwrap_or_default();
        if ls.is_empty() { continue; }
        let at = u.get("measured_at").and_then(Value::as_str).unwrap_or("").to_string();
        if best.as_ref().is_none_or(|(b, _)| at > *b) { best = Some((at, ls)); }
    }
    best.map(|(_, ls)| ls).unwrap_or_default().into_iter()
        .filter(|l| l.get("resets_at").and_then(Value::as_i64).is_none_or(|t| t > now_secs))
        .collect()
}

/// Every folder that may hold a Claude meter: this workspace, the registered ones, and the agent worktrees.
fn meter_roots(root: &str) -> Vec<std::path::PathBuf> {
    let mut v = vec![std::path::PathBuf::from(root)];
    if let Ok(c) = rusqlite::Connection::open_with_flags(antiloki_rs::engine_open_db::default_state_path(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
        if let Ok(mut st) = c.prepare("SELECT root_path FROM workspaces WHERE root_path IS NOT NULL AND root_path != ''") {
            if let Ok(rows) = st.query_map([], |r| r.get::<_, String>(0)) { for p in rows.flatten() { v.push(p.into()); } }
        }
    }
    if let Some(h) = std::env::var_os("HOME") {
        if let Ok(rd) = std::fs::read_dir(std::path::Path::new(&h).join(".antiloki").join("worktrees")) { for e in rd.flatten() { v.push(e.path()); } }
    }
    v.sort(); v.dedup(); v
}

/// The rate-limit windows on this box: the newest claude meter's, Codex's session files, whatever OpenCode's own store reports, and - for the seats whose CLI publishes none - what antiloki itself sent them.
/// One file read instead of a tool call per pane.
pub(crate) fn account_windows(root: &str) -> Vec<Value> {
    let now = antiloki_rs::clock::now_iso_nodb();
    let mut out = vec![];
    for l in claude_windows_anywhere(&meter_roots(root), crate::cli::now_ms() / 1000) {
        out.push(json!({ "house": "claude", "label": l.get("label"), "used_pct": l.get("used_pct"), "resets_at": l.get("resets_at") }));
    }
    out.extend(crate::fuel::codex_windows(crate::cli::now_ms() / 1000));
    let o = antiloki_rs::cockpit__helpers_engine_usage::opencode_usage(std::path::Path::new(root), None, &now);
    if let Some(w) = o.get("weekly_local").filter(|v| !v.is_null()) {
        out.push(json!({ "house": "opencode", "label": "week", "tokens": w.get("tokens"), "cost": w.get("cost_usd") }));
    }
    // CURSOR, GEMINI AND GROK PUBLISH NO WINDOW (checked 2026-10-01: no file, subcommand or stream field carries one), so what is shown for them is what antiloki itself sent in the last 5 h and 7 d,
    // from the serve's audit table (every CLI call and pane turn, any workspace) - not a workspace's brain.ai_calls, which pane turns never reach.
    // These rows have no `used_pct`, so `cli::set_windows` drops them and the router never mistakes a count for a share of an allowance.
    let week = crate::audit::calls_since_hours(7 * 24);
    out.extend(crate::fuel::own_use(&crate::audit::calls_since_hours(5), &week));
    // ...and, for a seat whose allowance the operator declared (Settings > Seats), antiloki's own estimate of the week as a real window row: the preview shows it, the router skips the seat at 95 %
    // ...unless Cursor's own meter has been read (cursor_usage: the CLI's `/usage` through a pseudo-terminal): a measured figure replaces the estimate for that house
    let measured = crate::cursor_usage::rows_now();
    let mut est = crate::fuel::allowance_rows(&week, &crate::cli::allowances());
    if !measured.is_empty() { est.retain(|r| r.get("house").and_then(Value::as_str) != Some("cursor")); }
    out.extend(est);
    out.extend(measured);
    // Antigravity publishes its quota on its own `/usage` screen (agy_usage); a measured figure replaces the estimate for that house
    let agy = crate::agy_usage::rows_now();
    if !agy.is_empty() { out.retain(|r| !(r.get("house").and_then(Value::as_str) == Some("gemini") && r.get("estimated").and_then(Value::as_bool) == Some(true))); }
    out.extend(agy);
    out
}

/// GET /api/v1/cockpit/days?tz=<minutes east of UTC> — the operator's days (days.rs).
pub async fn days_http(state: Arc<AppState>, agents: Arc<crate::agent::AgentRegistry>, goals: Arc<Goals>, q: HashMap<String, String>) -> Response {
    // the viewer's own offset, so a day breaks at their midnight and not UTC's; `new Date().getTimezoneOffset()`
    // is minutes WEST, so the browser sends its negation and a missing value means UTC
    let tz = q.get("tz").and_then(|v| v.trim().parse::<i64>().ok()).filter(|m| m.abs() <= 14 * 60).unwrap_or(0);
    let chats = crate::chat_pane::chats_of(&state, &agents).await;
    let runs: Vec<serde_json::Value> = goals.list(None).into_iter().map(|r| json!(r)).collect();
    // THE REPO'S OWN RECORD (2026-09-24): the filed episodes and every AI call this workspace made. Both are
    // sqlite rows written when the thing happened, which is why a day's numbers cannot drift from what ran.
    let root = {
        let conn = state.pick();
        tokio::task::spawn_blocking(move || {
            let c = conn.lock().unwrap_or_else(|p| p.into_inner());
            antiloki_rs::engine_workspace_root::getWorkspaceRoot(&c).as_str().unwrap_or("").to_string()
        }).await.unwrap_or_default()
    };
    let (filed, calls) = if root.is_empty() { (vec![], vec![]) } else {
        let r2 = root.clone();
        tokio::task::spawn_blocking(move || (crate::episodes_db::list(&r2, None, 500), crate::episodes_db::call_days(&r2, tz, 120)))
            .await.unwrap_or((vec![], vec![]))
    };
    ok(crate::days::days_with(&chats, &runs, &filed, &calls, tz, crate::cli::now_ms()))
}

/// GET /api/v1/cockpit/goals/diff?id=N — every file that run wrote, committed text beside the text on disk.
/// `GET /api/v1/cockpit/pane/work?pane_id=N` — WHAT THIS PANE HAS DONE IN ITS OWN BOX.
///
/// Operator 2026-09-25: *"on the sidebar right of the agent pane should have the icon to see the diff done on
/// that pane, and another to see the commits done on that pane. Only on that pane."*
///
/// Two answers from one worktree: the files this agent has changed and not yet committed, as real before/after
/// pairs, and the commits it has already made on its branch. Scoped to the pane — the point of the ask was
/// "only on that pane", and a rail that showed the whole repository would be the workspace view that already
/// exists elsewhere.
/// `GET /api/v1/cockpit/pane/handoff-tax?pane_id=N` — WHAT CHANGING FORM HAS COST THIS PANE (PLAN-THE-CHAMELEON C4).
///
/// WIRED (audit 2026-09-28). `chat_pane::handoff_tax` computed the two populations the plan asks for — a provider
/// switch that crosses engines and pays a cold start, against an in-place restart in the same engine — and nothing
/// but its own test ever called it. The whole point of C4 is that the stay-put rule trades on a cost, so the cost
/// has to be readable; a measurement no caller can reach is a measurement nobody has.
///
/// `n: 0` on a side means no hand-off of that kind has happened yet, which the screen must print as "not measured"
/// rather than as a reassuring 0 ms.
pub async fn pane_handoff_tax(state: Arc<AppState>, q: HashMap<String, String>) -> Response {
    let Some(pane_id) = q.get("pane_id").and_then(|v| v.trim().parse::<i64>().ok()).filter(|i| *i > 0) else {
        return bad("a pane's hand-off tax needs a pane_id")
    };
    let cwd = {
        let conn = state.pick();
        tokio::task::spawn_blocking(move || {
            let c = conn.lock().unwrap_or_else(|p| p.into_inner());
            antiloki_rs::db_json::query_rows(&c, "SELECT worktree_path FROM cockpit_panes WHERE id = ?", &[json!(pane_id)])
                .ok().and_then(|r| r.as_array().and_then(|a| a.first()).and_then(|x| x.get("worktree_path")).and_then(Value::as_str).map(String::from))
                .unwrap_or_default()
        }).await.unwrap_or_default()
    };
    // a pane with no worktree has no log yet, and "nothing has happened" is a real answer here rather than an error
    let out = tokio::task::spawn_blocking(move || {
        let log = crate::chat_pane::events_text(&cwd, pane_id);
        crate::chat_pane::handoff_tax(log.lines())
    }).await.unwrap_or(Value::Null);
    ok(json!({ "ok": true, "pane_id": pane_id, "tax": out }))
}

/// `GET /api/v1/cockpit/pane/antiqa?pane_id=N` - the pane's antiQA button and its runs, for the rail's cue (RFC 0001 7). Read-only: the
/// runs are the folders `antiqa mcp` wrote into the pane's own box (`antiqa_runs.rs`). `installed` says whether the program can be found,
/// so a button switched on over a missing program can say so.
pub async fn pane_antiqa(state: Arc<AppState>, q: HashMap<String, String>) -> Response {
    let Some(pane_id) = q.get("pane_id").and_then(|v| v.trim().parse::<i64>().ok()).filter(|i| *i > 0) else {
        return bad("a pane's antiQA runs need a pane_id")
    };
    let limit = q.get("limit").and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(8).clamp(1, 30);
    let (cwd, settings) = {
        let conn = state.pick();
        tokio::task::spawn_blocking(move || {
            let c = conn.lock().unwrap_or_else(|p| p.into_inner());
            let row = antiloki_rs::db_json::query_rows(&c, "SELECT worktree_path, settings_json FROM cockpit_panes WHERE id = ?", &[json!(pane_id)])
                .ok().and_then(|r| r.as_array().and_then(|a| a.first()).cloned()).unwrap_or(Value::Null);
            (row.get("worktree_path").and_then(Value::as_str).unwrap_or("").to_string(), row.get("settings_json").and_then(Value::as_str).unwrap_or("").to_string())
        }).await.unwrap_or_default()
    };
    let enabled = serde_json::from_str::<Value>(&settings).map_or(false, |s| s["antiqa"]["enabled"] == json!(true));
    let installed = antiloki_rs::cockpit_cockpit_pane_context_bake::antiqa_bin_from(std::env::var("ANTIQA_BIN").ok(), || antiloki_rs::proc::which("antiqa")).is_some();
    let cwd2 = cwd.clone();
    let runs = if cwd.is_empty() { vec![] } else { tokio::task::spawn_blocking(move || crate::antiqa_runs::runs(std::path::Path::new(&cwd), limit)).await.unwrap_or_default() };
    ok(json!({ "ok": true, "enabled": enabled, "installed": installed, "worktree": if cwd2.is_empty() { Value::Null } else { json!(cwd2) }, "running": runs.iter().filter(|r| r["state"] == "running").count(), "runs": runs }))
}

pub async fn pane_work(state: Arc<AppState>, q: HashMap<String, String>) -> Response {
    let Some(pane_id) = q.get("pane_id").and_then(|v| v.trim().parse::<i64>().ok()).filter(|i| *i > 0) else {
        return bad("a pane's work needs a pane_id")
    };
    let cwd = {
        let conn = state.pick();
        tokio::task::spawn_blocking(move || {
            let c = conn.lock().unwrap_or_else(|p| p.into_inner());
            antiloki_rs::db_json::query_rows(&c, "SELECT worktree_path FROM cockpit_panes WHERE id = ?", &[json!(pane_id)])
                .ok().and_then(|r| r.as_array().and_then(|a| a.first()).and_then(|x| x.get("worktree_path")).and_then(Value::as_str).map(String::from))
                .unwrap_or_default()
        }).await.unwrap_or_default()
    };
    if cwd.is_empty() { return bad("that pane has no worktree yet — it gets one on its first turn") }

    let out = tokio::task::spawn_blocking(move || {
        let changed = crate::agent::changed_files(&cwd);
        // the same pair assembly the story's diff uses — one definition of "before and after"
        let diff = crate::work_diff::pairs(&cwd, &changed);
        json!({ "ok": true, "cwd": cwd, "changed": changed.len(), "diff": diff, "commits": crate::boxes::commits(&cwd, 30) })
    }).await.unwrap_or_else(|_| json!({ "ok": false, "error": "reading the box panicked" }));
    ok(out)
}



/// `GET /api/v1/cockpit/goals/live?id=N` - the QA's play-by-play as a server-sent stream. A viewer that joins gets ONE `snapshot` event (the step timeline so far, the newest frame,
/// whether it is over, and the kept film's timeline for a replay), then every step / frame / step-done / shot as it happens, and the stream ends after `done`. A run this process
/// did not play (a restart) is answered from its kept film alone; a run that never looked at anything is a 404.
pub async fn goal_live(goals: Arc<Goals>, q: HashMap<String, String>) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use tokio_stream::StreamExt;
    let Some(id) = q.get("id").and_then(|v| v.trim().parse::<i64>().ok()).filter(|i| *i > 0) else { return bad("live needs a run id") };
    let Some(run) = goals.get(id) else { return (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "no such run" } }))).into_response() };
    let film = crate::qa_evidence::film_timeline(&run.cwd, id);
    let ev = |v: Value| -> Result<Event, std::convert::Infallible> { Ok(Event::default().data(v.to_string())) };
    match crate::qa_evidence::hub_join(id) {
        Some((mut snap, rx)) => {
            snap["film"] = film.unwrap_or(Value::Null);
            let first = tokio_stream::once(ev(snap));
            let rest = futures_util::stream::unfold((rx, false), |(rx, over)| async move {
                let mut rx = rx?; if over { return None }
                match rx.recv().await {
                    Ok(v) => { let done = v["type"] == "done"; Some((Ok::<Event, std::convert::Infallible>(Event::default().data(v.to_string())), (Some(rx), done))) }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => Some((Ok(Event::default().comment("lagged")), (Some(rx), false))),
                    Err(_) => None,
                }
            });
            Sse::new(first.chain(rest)).keep_alive(KeepAlive::default()).into_response()
        }
        None => match film {
            Some(tl) => Sse::new(tokio_stream::once(ev(json!({ "type": "snapshot", "timeline": [], "last_frame": null, "done": true, "film": tl })))).into_response(),
            None => (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "the QA did not look at anything on this run" } }))).into_response(),
        },
    }
}

/// `GET /api/v1/cockpit/goals/evidence?id=N` - the screenshots the QA looked at for this run: `{images:[{side:"after"|"before", file, name}]}`. Visual QA (2026-09-30).
pub async fn goal_evidence(goals: Arc<Goals>, q: HashMap<String, String>) -> Response {
    let Some(id) = q.get("id").and_then(|v| v.trim().parse::<i64>().ok()).filter(|i| *i > 0) else { return bad("evidence needs a run id") };
    let Some(run) = goals.get(id) else { return (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "no such run" } }))).into_response() };
    let dir = crate::qa_evidence::evidence_dir(&run.cwd, id);
    // `film=0007.jpg` serves one frame of the kept film (the replay)
    if let Some(f) = q.get("film") {
        if !crate::qa_evidence::safe_film_name(f) { return bad("not a film frame") }
        return match std::fs::read(dir.join("film").join(f)) { Ok(b) => ([(axum::http::header::CONTENT_TYPE, "image/jpeg"), (axum::http::header::CACHE_CONTROL, "no-cache")], b).into_response(), Err(_) => (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "no such frame" } }))).into_response() };
    }
    // `file=` serves one PNG (the name is checked: a plain file name in one of the two folders, never a path)
    if let Some(file) = q.get("file") {
        let side = q.get("side").map(String::as_str).unwrap_or("after");
        if !crate::qa_evidence::safe_image_name(file) || !(side == "after" || side == "before") { return bad("not an evidence image") }
        let path = if side == "before" { dir.join("base").join(file) } else { dir.join(file) };
        return match std::fs::read(&path) { Ok(b) => ([(axum::http::header::CONTENT_TYPE, "image/png"), (axum::http::header::CACHE_CONTROL, "no-cache")], b).into_response(), Err(_) => (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "no such image" } }))).into_response() };
    }
    let list = |d: &std::path::Path, side: &str| -> Vec<Value> {
        let mut v: Vec<String> = std::fs::read_dir(d).map(|rd| rd.flatten().filter_map(|e| e.file_name().to_str().map(String::from)).filter(|n| crate::qa_evidence::safe_image_name(n)).collect()).unwrap_or_default();
        v.sort();
        v.into_iter().map(|f| json!({ "side": side, "file": f, "name": f.trim_end_matches(".png").splitn(2, '-').nth(1).unwrap_or(&f) })).collect()
    };
    let mut images = list(&dir, "after"); images.extend(list(&dir.join("base"), "before"));
    // `film`: the QA's play-by-play was kept (a replayable browser film or a server test's terminal) - the thread uses it to know a run HAS a QA box without opening a stream for it
    ok(json!({ "ok": true, "images": images, "film": dir.join("film").join("timeline.json").is_file(), "live": crate::qa_evidence::hub_join(id).is_some() }))
}
pub async fn goal_diff(goals: Arc<Goals>, q: HashMap<String, String>) -> Response {
    let Some(id) = q.get("id").and_then(|v| v.trim().parse::<i64>().ok()).filter(|i| *i > 0) else { return bad("a diff needs a run id") };
    let Some(run) = goals.get(id) else { return (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "no such run" } }))).into_response() };
    let run = json!(run);
    // the same list the story shows, from the same assembly — one definition of "what this run wrote"
    let siblings: Vec<Value> = goals.list(None).into_iter().map(|r| json!(r)).collect();
    let story = crate::work_story::story_with(&run, &[], &siblings, crate::cli::now_ms());
    let files: Vec<String> = story["summary"]["files"]["written"].as_array().cloned().unwrap_or_default()
        .into_iter().filter_map(|v| v.as_str().map(String::from)).collect();
    let cwd = run.get("cwd").and_then(Value::as_str).unwrap_or("").to_string();
    let out = tokio::task::spawn_blocking(move || crate::work_diff::pairs(&cwd, &files)).await.unwrap_or(json!({ "files": [] }));
    ok(out)
}

/// GET /api/v1/cockpit/goals/story?id=N — the run, told (work_story.rs).
pub async fn goal_story(goals: Arc<Goals>, q: HashMap<String, String>) -> Response {
    let Some(id) = q.get("id").and_then(|v| v.trim().parse::<i64>().ok()).filter(|i| *i > 0) else { return bad("story needs a run id") };
    let Some(run) = goals.get(id) else { return (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "no such run" } }))).into_response() };
    // the bench table is only read for the measured raw-time baseline; unreadable is not fatal, it just means no claim
    let rows = crate::merges::table().unwrap_or_default();
    // the rest of the ledger bounds an inline turn's window: the next run on the same box is the next turn
    let siblings: Vec<Value> = goals.list(None).into_iter().map(|r| json!(r)).collect();
    ok(json!({ "story": crate::work_story::story_with(&json!(run), &rows, &siblings, crate::cli::now_ms()) }))
}

/// THE PANE'S DELEGATION POLICY and whether it runs no checks (none of test / build / lint switched on), read straight from the pane's settings row on a pool connection
/// (`pane.live` carries no settings). Shared by the plan door and the queue, which decides an item's binding with the same policy the plan would.
pub(crate) async fn pane_delegation(state: &Arc<AppState>, pid: i64) -> (Delegation, bool) {
    let conn = state.pick();
    tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        let full = antiloki_rs::db_json::query_rows(&c, "SELECT settings_json FROM cockpit_panes WHERE id = ?", &[json!(pid)]).ok().and_then(|r| r.as_array().and_then(|a| a.first()).cloned()).unwrap_or(Value::Null);
        let gates = crate::gates::enabled_on(&serde_json::from_str::<Value>(full.get("settings_json").and_then(Value::as_str).unwrap_or("{}")).unwrap_or(Value::Null));
        // none of test / build / lint switched on (the laws gate is free and always allowed) → the workers run no checks
        (Delegation::of_pane(&full), !gates.iter().any(|g| matches!(g, crate::gates::Gate::Test | crate::gates::Gate::Build | crate::gates::Gate::Lint)))
    }).await.unwrap_or_default()
}
pub async fn goals_op(state: Arc<AppState>, goals: Arc<Goals>, reg: Arc<crate::agent::AgentRegistry>, op: String, body: Bytes) -> Response {
    let mut b = body_json(&body);
    if !b.is_object() { b = json!({}); }
    if !["plan", "answer", "approve", "setPlan", "pause", "resume", "cancel", "retry", "skip", "delivered", "finding", "result", "compare", "pr", "qa"].contains(&op.as_str()) { return bad("unknown goal op"); }
    let id = n(b.get("id"));
    // an AGENT-started run (MCP tool) names only its pane: resolve the widget key + box from the pane row
    // the pane's policy applies whenever the run names its pane — the MCP tool (no parent/cwd) and the
    // pane's own Heimdall ask bar (2026-09-20: the operator's prompt goes through the router first)
    let mut pane_checks_off = false; // set by the pane's own gates below; a run with no pane keeps its workers' habits
    if op == "plan" && b.get("pane_id").map(|v| !v.is_null()).unwrap_or(false) {
        let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
        let pane = engine_call(&state, "system.cockpit.pane.live", json!({ "id": n(b.get("pane_id")) }), ctx).await.map(|(v, _)| v).unwrap_or(Value::Null);
        let row = pane.get("result").cloned().unwrap_or(pane);
        if !row.get("found").and_then(Value::as_bool).unwrap_or(false) { return bad(&format!("pane {} not found", n(b.get("pane_id")))); }
        if s(b.get("parent")).is_empty() { b["parent"] = json!(s(row.get("subagent_name")).trim_start_matches("chat-")); }
        if s(b.get("cwd")).is_empty() { b["cwd"] = row.get("worktree_path").cloned().unwrap_or(Value::Null); }
        // THE PANE'S DELEGATION POLICY (operator 2026-09-19): off = refused; max caps the tasks; the engine
        // and tier the operator chose override what the agent asked; the parent's own engine is the default.
        // (`pane.live` carries no settings — the row is read straight from the db on a pool connection.)
        let d = pane_delegation(&state, n(b.get("pane_id"))).await;
        let (d, checks_off) = d;
        pane_checks_off = checks_off;
        if !d.enabled { return bad("delegation is off for this pane — the operator turns it on in ⚙ › subagents"); }
        if d.min { b["min"] = json!(true); }
        if d.qa { b["qa"] = json!(true); }
        // HEIMDALL PASSES THE BATON (operator 2026-09-26: "heimdall not just routes the model, it routes the
        // role"). The ask names the work, the work names the role, and the role's own measured winner does it —
        // so nobody has to assemble a roster before working.
        //
        // THREE GUARDS, because a router that overrules the operator is worse than one that does not route:
        //   · `d.roles` off → nothing happens;
        //   · a pair already on the body is the operator's explicit choice and is never replaced;
        //   · only in `auto`, which is the mode that already means "the router picks the models" — in `on` the
        //     operator's own bars win, and in `off` nothing routes at all.
        // The verdict rides along either way, so the run can say what it read even when it changed nothing.
        if d.roles && b.get("goal").and_then(Value::as_str).map(|g| !g.trim().is_empty()).unwrap_or(false) {
            let goal = b["goal"].as_str().unwrap_or("").to_string();
            let size = if b.get("size").and_then(Value::as_str) == Some("s") { "s" } else { "l" };
            let route = crate::roles::route_role_in(&goal, size, d.role_objective, &s(b.get("cwd")));
            let had_pair = b.get("pair").map(|p| !p.is_null()).unwrap_or(false);
            if d.mode == "auto" && !had_pair {
                if let Some(pair) = crate::roles::pair_of_route(&route) {
                    b["pair"] = serde_json::to_value(&pair).unwrap_or(Value::Null);
                }
            }
            // the run picks this up when it is born and logs the sentence (`goals::take_role`)
            goals.set_role_for_next(Some(route.clone()));
            // C2: each task may take its own form, if the pane asked for it (off by default until C4's third
            // number exists). Set beside the role so the two travel together.
            goals.set_mid_task_for_next(d.roles_mid_task);
            b["role"] = route;
        }
        // AUTO lets the router pick the models (the role bindings, held to "a worker cheaper than its planner");
        // ON hands it the operator's own stop of the models bar; OFF routes nothing at all.
        if d.mode == "on" { if let Some(c) = d.cost { b["cost"] = json!(c); } }
        if let Some(p) = &d.pair { b["pair"] = serde_json::to_value(p).unwrap_or(Value::Null); }
        let asked = n(b.get("n")); b["n"] = json!(if asked <= 0 { d.max as i64 } else { asked.min(d.max as i64) });
        if let Some(e) = &d.engine { b["engine"] = json!(e); } else {
            // HEIMDALL PLANS ON ITS OWN SEAT (cli::heimdall_engine, operator 2026-09-27): the seat chosen in Settings ›
            // Providers, else the pane's own when it is usable, else the first usable one — never a blocked one. The
            // FE sends the pane's engine here; with main handed to OpenCode and Zen off, that was a guaranteed refusal.
            let pane_engine = engine_of(&json!(s(b.get("engine")))).or_else(|| engine_of(row.get("pane_kind").unwrap_or(&Value::Null)));
            match crate::cli::heimdall_engine(pane_engine) {
                Some(e) => b["engine"] = json!(e.key()),
                None => {
                    // SAY WHY EACH SEAT WAS REFUSED (2026-10-09): "every engine is uninstalled, signed out or switched off" hid that one of them was merely out of quota until a given time
                    let why: Vec<String> = crate::cli::Engine::all().into_iter().map(|e| {
                        let r = if crate::cli::seat_off(e) { "switched off".to_string() }
                            else if !crate::cli::engine_usable(e) { "not installed or signed out".to_string() }
                            else if let Some((_, w)) = crate::cli::seat_limit(e) { format!("out of quota ({w})") }
                            else if let Some(b) = crate::cli::model_blocked(e, None) { b }
                            else { "refused".to_string() };
                        format!("{}: {}", e.key(), r)
                    }).collect();
                    return bad(&format!("no seat can plan this - {} (Settings › Seats)", why.join("; ")))
                }
            }
        }

        if let Some(t) = d.tier { b["tier"] = json!(t.key()); }
        b["router"] = json!(d.mode != "off");
        // THE PANE'S OWN POLICY DECIDES, IN BOTH DIRECTIONS (operator 2026-09-28: "1 subagent should be
        // automatically accepted, but keeps asking for approval").
        //
        // This was `if d.ask_first { b["auto"] = json!(false) }` — one-sided. `approve: "ask"` forced the gate on,
        // but `approve: "auto"` did NOTHING: it left the body's default, and the body's default is already false
        // (`auto` below reads `unwrap_or(false)`), because the FE never sends the key at all. So the wizard's own
        // label — "runs at once · the ratified plan runs without asking" — was never true for an operator's ask,
        // and every plan parked at `awaiting-approval` behind a rail that is closed by default. A one-task plan
        // waited exactly like an eight-task one; the agent path worked only because `mcp.rs` posts `auto: true`
        // by hand.
        //
        // The policy is the operator's stated intent, so it answers both ways.
        b["auto"] = json!(!d.ask_first);
    }
    match op.as_str() {
        "plan" => {
            let goal = s(b.get("goal")).trim().to_string();
            if goal.is_empty() { return bad("goal required"); }
            // WHAT THE OPERATOR TYPED IS A FACT ABOUT THE CONVERSATION (operator 2026-09-28: "on reloading my ask
            // disappeared. But i know it ran, because showed running").
            //
            // An ask typed into a pane goes through one of two doors. Door B is the pane's own engine, and
            // `agent_send` writes an `antiloki/user` event for it. Door A is Heimdall: the FE sees a verb it can
            // route, calls THIS endpoint instead, and returns before ever reaching `/agent/send` — so nothing was
            // ever written. The bubble on screen was optimistic React state; a reload replays the pane's `.jsonl`
            // and the ask is simply gone, while the run it started carries on invisibly.
            //
            // Verified on the operator's own pane 50: the log holds "hi" and four copies of "what does this
            // project sell?" — all Door B — and no trace of the ask that produced goal run #245, which did land
            // its diff. They asked the same question four times because nothing came back.
            //
            // So the ask is recorded here, before the planner runs, under the pane that received it. It is written
            // whatever the plan then does, because the operator said it either way.
            if let Some(pid) = b.get("pane_id").and_then(Value::as_i64).filter(|v| *v > 0).filter(|_| !b.get("announced").and_then(Value::as_bool).unwrap_or(false)) {
                reg.emit(pid, json!({ "type": "antiloki", "subtype": "user", "text": goal, "source": "heimdall" }), true);   // the consolidated goal Heimdall wrote, not what the operator typed: the chat shows it as a task view
            }
            let engine = engine_of(b.get("engine").unwrap_or(&Value::Null)).unwrap_or_else(default_seat);
            let cwd = { let c = s(b.get("cwd")); if c.is_empty() { root() } else { c } };
            let n_tasks = { let k = n(b.get("n")); if k <= 0 { 4 } else { k as usize } };
            let forced = b.get("tier").and_then(Value::as_str).and_then(crate::cli::Tier::of);
            let router = b.get("router").and_then(Value::as_bool).unwrap_or(true);
            let auto = b.get("auto").and_then(Value::as_bool).unwrap_or(false);
            // BLUEPRINT MODE (2026-09-22): the deterministic pipeline — contract → skeleton → tests → gate → fills → check
            // THE MODELS BAR: the pane's stop, consumed once by the plan below (goals::set_cost_for_next)
            goals.set_cost_for_next(b.get("cost").and_then(Value::as_u64).map(|c| c.min(2) as u8));
            // a name ("claude-deepseek"), the pane's shape, or the serialised Pair — all three are pairs
            // NEVER TWO FRONTIER MODELS (operator 2026-09-23) — the pair is refused at the door, with the reason
            let pair_in = b.get("pair").and_then(|p| serde_json::from_value::<crate::cli::Pair>(p.clone()).ok().or_else(|| crate::cli::Pair::of_settings(&json!({ "pair": p }))));
            // THE PAIR MUST BE REACHABLE TOO (operator 2026-09-27: "OpenCode · Zen is off … even though at settings is
            // already selected another provider to heimdall"): the roles record or the operator's own pair may stand on
            // a seat that is switched off today. It bends to the planning seat instead of failing the ask
            // (cli::pair_reachable) — `problem()` below then has nothing left to refuse.
            //
            // EVERY BEND IS LOGGED, IN ORDER (operator 2026-10-01): the unreachable-seat bend and the frontier-rule bend are one `bend_pair`, and the run's log
            // got only the first reason while the second moved the hands again ("cursor is switched off, the brain does the work itself", then gemini flash-low).
            // A seat can be off in Settings OR out of quota; the line says which.
            let mut bent: Vec<String> = vec![];
            let fb = engine_of(b.get("engine").unwrap_or(&Value::Null)).unwrap_or_else(default_seat);
            let mut bench_arm_bent = false;
            let pair_in = pair_in.map(|p| {
                let bench = !s(b.get("suite")).trim().is_empty();
                // A BENCH ARM IS NEVER BENT (audit 2026-09-29). The bend's reason used to be written onto `role`, which a bench arm has none of, so the S2
                // `gemini-composer` arms ran with Sonnet planning under the Gemini arm's name and nothing said so. An arm names a suite; bending it
                // measures something other than what it asked for, so it is refused with the reason instead.
                let (q, lines, reachable) = crate::cli::bend_pair(p, fb,
                    |e, m| crate::cli::model_blocked(e, m).map(|_| "switched off").or_else(|| crate::cli::seat_limit(e).map(|_| "out of quota")),
                    // …AND IT MUST OBEY THE FRONTIER RULE, WHICH NEITHER THE RECORD NOR A ROLE CONSULTS (cli::pair_affordable, found 2026-09-28): a fresh
                    // fork's every ask died on `problem()` with a 400 telling the operator to pick a cheaper side of a pair they never picked. It bends -
                    // the hands drop a rung - and `problem()` below then has nothing left to refuse. The refusal stays as the last guard, for a pair sent in by hand.
                    |e| crate::cli::tier_model(e, crate::cli::Tier::Low),
                    |e, m| crate::cli::model_blocked(e, m).is_some());
                bench_arm_bent = bench && reachable > 0;
                if !lines.is_empty() { if let Some(r) = b.get_mut("role").and_then(Value::as_object_mut) { r.insert("bent".into(), json!(lines.join("; "))); } }
                bent = lines;
                q
            });
            if bench_arm_bent {
                return bad(&format!("this bench arm's pair would be bent, so it is refused — {}", bent.join(" · ")));
            }
            // THE DOOR GUARD, MADE REACHABLE AGAIN (audit 2026-09-28). `pair_affordable` always resolves — its
            // last resort satisfies `problem()` by construction — so checking `problem()` here could never fire
            // and was dead code pretending to be a safety net. What CAN still be wrong after the bend is a pair
            // the provider gate refuses, which is the failure the operator actually saw ("OpenCode · Zen is off …"),
            // so that is what the door checks.
            if let Some(p) = pair_in.as_ref() {
                if let Some(why) = crate::cli::model_blocked(p.brain, p.brain_model.as_deref())
                    .or_else(|| crate::cli::model_blocked(p.hands, p.hands_model.as_deref())) { return bad(&why); }
                if let Some(why) = p.problem() { return bad(&why); }   // an invariant, not an expectation
            }
            goals.set_pair_for_next(pair_in);
            // A BENCH ARM SAYS IT IS ONE. `suite` names the fixture the arm belongs to (the merge bench sends
            // "ministore"); the run then records itself as controlled evidence rather than as a thing that happened.
            goals.set_suite_for_next(s(b.get("suite")).trim().to_ascii_lowercase());
            // `planner`: "never" | "always" | absent = Heimdall decides (a per-run switch so one task can be run both ways)
            goals.set_planner_for_next(s(b.get("planner")).trim().to_ascii_lowercase());
            if s(b.get("mode")) == "blueprint" {
                // QA IS PART OF THE FLOW (operator 2026-09-27: "make QA part of the flow not optional"). Absent means ON.
                // The evidence: the writers get an ambiguous ask wrong about one run in three, every judge caught that
                // defect 4/4, and NO arm produced a single false rejection in 48 judged artefacts — so the gate costs a
                // re-ask only when there is something to re-ask about. An explicit `false` still turns it off, which the
                // benches need for their controls.
                let run = goals.plan_blueprint(&s(b.get("parent")), &goal, engine, &cwd, b.get("workers").and_then(|w| engine_of(w)), b.get("qa").and_then(Value::as_bool).unwrap_or(true)).await;
                return ok(json!({ "ok": true, "run": run }));
            }
            // WORKERS (2026-09-22): `workers: "opencode"` — the tasks run there (at the tier the planner assigns),
            // the plan, verify, join and judge stay on `engine`
            // 2026-10-13: pane.live does not carry scope_json, so a fenced pane planned as unrestricted and wrote package.json. Read the column.
            if let Some(pid) = b.get("pane_id").and_then(Value::as_i64).filter(|v| *v > 0) {
                let conn = state.pick();
                let scope = tokio::task::spawn_blocking(move || {
                    let c = conn.lock().unwrap_or_else(|p| p.into_inner());
                    let row = antiloki_rs::db_json::query_rows(&c, "SELECT scope_json FROM cockpit_panes WHERE id = ?", &[json!(pid)]).ok().and_then(|r| r.as_array().and_then(|a| a.first()).cloned()).unwrap_or(Value::Null);
                    crate::chat_pane::pane_scope_globs(&row)
                }).await.unwrap_or_default();
                goals.set_scope_for_next(scope);
            }
            goals.set_pane_for_next(n(b.get("pane_id")), pane_checks_off && !crate::goals::ask_wants_checks(&goal));
            goals.set_workers_for_next(b.get("workers").and_then(|w| engine_of(w)), b.get("always").and_then(Value::as_bool).unwrap_or(false)); // the same engine is allowed: the planner still learns its workers are cheap
            let mut run = goals.plan_full(&s(b.get("parent")), &goal, engine, &cwd, b.get("write").and_then(Value::as_bool).unwrap_or(false), n_tasks, Some(n(b.get("concurrency")) as usize).filter(|c| *c > 0), forced, router, auto).await;
            // QA (2026-09-21): the pane's judge reviews this run after its join
            // QA IS OBLIGATORY (operator 2026-09-28: "the QA is obligatory for all tasks"). The request's `qa`
            // flag no longer switches it off — it asks for the SECOND pass, which is what the Heimdall menu's QA
            // button now buys.
            goals.set_qa_wanted(run.id, b.get("qa").and_then(Value::as_bool).unwrap_or(false));
            for w in &bent { goals.note(run.id, "pair", &format!("the pair was bent: {w}")); }
            if !bent.is_empty() { if let Some(p) = run.pair.as_ref() { goals.note(run.id, "pair", &crate::cli::work_line(p)); } }
            goals.set_checks_off(run.id, pane_checks_off && !crate::goals::ask_wants_checks(&goal));
            if let Some(r2) = goals.get(run.id) { run = r2; }
            // MIN'S FALLBACK (2026-09-21): an ask naming files neither the pack nor the snapshot know would only be
            // refused under MIN — the route says so and the pane runs this one ask with exploration on
            if b.get("min").and_then(Value::as_bool).unwrap_or(false) {
                let paths = crate::goals::named_paths(&goal);
                if !paths.is_empty() {
                    let manifest: Value = std::fs::read_to_string(std::path::Path::new(&cwd).join(".antiloki/context/manifest.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(json!({}));
                    let mut judged_set = std::collections::HashSet::new();
                    for p in &paths { let r = engine_call(&state, "system.constitution.file", json!({ "path": p }), json!({ "actor": "web-fe", "source": "web-fe" })).await.ok().map(|(v, _)| v).unwrap_or(Value::Null); if r.get("found").and_then(Value::as_bool).unwrap_or(false) { judged_set.insert(p.clone()); } }
                    let missing = crate::goals::without_intel(&cwd, &paths, &manifest, &|p| judged_set.contains(p));
                    if !missing.is_empty() { if let Some(r2) = goals.mark_no_intel(run.id, missing) { run = r2; } }
                }
            }
            // AND THE PANE GETS ITS STATS BOX (operator 2026-09-28: "the stats detail box didnt show after
            // completion of my prompt, neither an answer of the AI. Even tough it did the task i asked, didnt say
            // anything after").
            //
            // A delegated run is done by SUB-AGENT processes in the box, not by the pane's own session, so the
            // pane's log never sees an `assistant` or a `result` for it and `on_turn_result` — the only thing that
            // emits `savings` — never fires. No savings event, no story turn, no stats box. The work landed on
            // disk and the chat said nothing at all until the delivery tick injected a summary minutes later.
            //
            // The run's story already exists and the box already knows how to poll one; it was never told a run
            // had started. This is that telling — a marker, not a measurement, so it carries the id and nothing
            // it has not measured yet.
            if let Some(pid) = b.get("pane_id").and_then(Value::as_i64).filter(|v| *v > 0) {
                reg.emit(pid, json!({ "type": "antiloki", "subtype": "run", "run": run.id }), true);
            }
            // auto-approve (the agent orchestrates — no operator gate)
            if auto && run.status == crate::goals::RunStatus::AwaitingApproval {
                if let Some(a) = goals.approve(run.id) { return ok(json!({ "ok": true, "run": a })); }
            }
            ok(json!({ "ok": true, "run": run }))
        }
        "result" => {
            // long-poll: wait (bounded) for the run to settle, then answer; the FE's own delivery is skipped for it
            let wait = n(b.get("wait_ms")).clamp(0, 240_000) as u64;
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait);
            loop {
                let Some(run) = goals.get(id) else { return ok(json!({ "ok": false, "error": "no such run" })) };
                let settled = matches!(run.status, crate::goals::RunStatus::Done | crate::goals::RunStatus::Failed | crate::goals::RunStatus::Cancelled);
                if settled || std::time::Instant::now() >= deadline {
                    if run.report.is_some() && !run.delivered { goals.mark_delivered(run.id); }
                    return ok(json!({ "ok": true, "run": goals.get(id) }));
                }
                tokio::time::sleep(std::time::Duration::from_millis(3000)).await;
            }
        }
        // `{claim: true}`: first caller wins (the tab that typed it), every later caller gets won: false
        // `undo` (2026-09-20: a route was claimed, the send was refused, the ask was lost): the claim is released
        "delivered" if b.get("undo").and_then(Value::as_bool).unwrap_or(false) => ok(json!({ "ok": goals.release_delivery(id) })),
        "delivered" => if b.get("claim").and_then(Value::as_bool).unwrap_or(false) { match goals.claim_delivery(id) { Some(won) => ok(json!({ "ok": true, "won": won })), None => ok(json!({ "ok": false, "won": false })) } } else { ok(json!({ "ok": goals.mark_delivered(id) })) },
        // THE RESULT AS A PR (operator 2026-09-19): commit the run's box with the goal as the title
        // and the joined report as the body — the cockpit PR record (diff, mergeability) the
        // Diff/review widget already lists. The pane is the one whose box the run ran in.
        "pr" => {
            let Some(run) = goals.get(id) else { return bad("no such run") };
            if !run.write { return bad("a read-only run changed nothing — there is no diff to open a PR from"); }
            if !matches!(run.status, crate::goals::RunStatus::Done | crate::goals::RunStatus::Failed) { return bad("the run is still going — open the PR once it settles"); }
            let pane_id = {
                let conn = state.pick(); let cwd = run.cwd.clone();
                tokio::task::spawn_blocking(move || {
                    let c = conn.lock().unwrap_or_else(|p| p.into_inner());
                    antiloki_rs::db_json::query_rows(&c, "SELECT id FROM cockpit_panes WHERE worktree_path = ? ORDER BY id DESC LIMIT 1", &[json!(cwd)]).ok().and_then(|r| r.as_array().and_then(|a| a.first()).and_then(|p| p.get("id")).and_then(Value::as_i64))
                }).await.unwrap_or(None)
            };
            let Some(pane_id) = pane_id else { return bad(&format!("no pane owns the box {} — the PR needs the pane's branch", run.cwd)) };
            // the title: the goal's first line when it reads like one; an agent-written goal ("Write-mode,
            // exactly two independent tasks with disjoint ownership: …") is replaced by the task titles
            let first = run.goal.lines().next().unwrap_or_default().trim().to_string();
            let title = if first.chars().count() <= 80 && !first.to_ascii_lowercase().starts_with("write-mode") && !first.to_ascii_lowercase().starts_with("read-only") { first }
                else { let t = run.tasks.iter().map(|t| t.title.trim().to_string()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join(" · "); if t.is_empty() { first.chars().take(80).collect() } else { t.chars().take(110).collect() } };
            let body = crate::goals::Goals::pr_body(&run);
            let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
            match engine_call(&state, "system.cockpit.pr.open", json!({ "pane_id": pane_id, "title": format!("goal run #{}: {}", run.id, title), "message": body }), ctx).await {
                Ok((v, _)) => {
                    let r = v.get("result").cloned().unwrap_or(v);
                    let opened = r.get("opened").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_i64);
                    match opened {
                        Some(pr) => {
                            goals.set_pr(id, pr);
                            // THE PR AS A TEST ENVIRONMENT (operator 2026-09-19: "this screen had a way to QA —
                            // run tests etc"): the proof suite, the gate terminals, the live preview and the file
                            // review all key on a `contracts` row with a worktree. A goal run's PR gets one —
                            // `pr-<id>`: the tasks' acceptance as the criteria, the changed files, the box.
                            let slug = format!("pr-{pr}");
                            let acceptance: Vec<Value> = run.tasks.iter().flat_map(|t| t.acceptance.iter().enumerate().map(move |(i, a)| json!({ "id": format!("{}-ac{}", t.id, i + 1), "text": a, "task": t.id, "title": t.title }))).collect();
                            let files: Vec<String> = run.tasks.iter().flat_map(|t| t.changed.iter().cloned()).collect();
                            let conn = state.pick(); let cwd = run.cwd.clone(); let title_c = title.clone(); let goal_c = run.goal.clone(); let body_c = body.clone();
                            let _ = tokio::task::spawn_blocking(move || {
                                let c = conn.lock().unwrap_or_else(|p| p.into_inner());
                                let _ = c.execute("INSERT INTO contracts (slug, title, status, worktree_path, acceptance_json, files_touched_json, goal_md, briefing_md) VALUES (?1, ?2, 'qa', ?3, ?4, ?5, ?6, ?7) ON CONFLICT(slug) DO UPDATE SET worktree_path = excluded.worktree_path, acceptance_json = excluded.acceptance_json, files_touched_json = excluded.files_touched_json, goal_md = excluded.goal_md, briefing_md = excluded.briefing_md, status = 'qa'",
                                    rusqlite::params![slug, format!("goal run #{}: {}", id, title_c), cwd, serde_json::to_string(&acceptance).unwrap_or_else(|_| "[]".into()), serde_json::to_string(&files).unwrap_or_else(|_| "[]".into()), goal_c, body_c]);
                                let _ = c.execute("UPDATE cockpit_pull_requests SET ref_slug = ?1 WHERE id = ?2", rusqlite::params![slug, pr]);
                            }).await;
                            ok(json!({ "ok": true, "pr": pr, "pane_id": pane_id, "ref_slug": format!("pr-{pr}"), "run": goals.get(id) }))
                        }
                        None => ok(json!({ "ok": false, "pane_id": pane_id, "skipped": r.get("skipped").cloned().unwrap_or(json!([])), "error": { "message": "nothing to open — the box has no changes in scope (see skipped)" } })),
                    }
                }
                Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": { "message": e } }))).into_response(),
            }
        }
        "compare" => { let r = goals.compare_raw(id).await; ok(json!({ "ok": r.is_some(), "run": r })) }
        // the screening gate's answer round: `{ id, answers: { q1: "…" } }` — a missing answer takes the default
        // WHAT WAS CHOSEN IS PART OF THE PROMPT (operator 2026-09-28: "on screening, the choices screened becomes
        // part of the sent prompt on the frontend. right now they keep hidded on its pane").
        //
        // Screening asks the operator a question and their answer goes straight into what the planner is told — it
        // is as much of the ask as the sentence they typed. It was written to the RUN's log and nowhere else, so
        // the chat that raised the question showed the question, then showed nothing, and the plan that followed
        // was built on words the transcript had no record of.
        //
        // The run knows its pane now, so the choices are said back there, under the ask they belong to.
        "answer" => {
            let chosen = b.get("answers").cloned().unwrap_or(json!({}));
            let r = goals.answer(id, &chosen).await;
            if r.is_some() {
                let pane = goals.pane_of(id);
                if pane > 0 {
                    if let Some(line) = crate::goals::answers_line(&chosen) {
                        reg.emit(pane, json!({ "type": "antiloki", "subtype": "user", "text": line }), true);
                    }
                }
            }
            ok(json!({ "ok": r.is_some(), "run": r }))
        }
        // RUN INDEPENDENT QA on a finished run that skipped it (QA is optional): the result's button
        "qa" => { let r = goals.run_qa_now(id); ok(json!({ "ok": r.is_ok(), "error": r.as_ref().err(), "run": goals.get(id) })) }
        "finding" => ok(json!({ "ok": goals.add_finding(id, { let t = s(b.get("task")); if t.is_empty() { "operator".into() } else { t } }.as_str(), &s(b.get("text"))) })),
        _ => {
            let run = match op.as_str() {
                "approve" => goals.approve(id), "pause" => goals.pause(id), "resume" => goals.resume(id), "cancel" => goals.cancel(id),
                "retry" => goals.retry(id, &s(b.get("task"))), "skip" => goals.skip(id, &s(b.get("task"))),
                "setPlan" => goals.set_plan(id, b.get("tasks").unwrap_or(&json!([]))),
                _ => None,
            };
            ok(json!({ "ok": run.is_some(), "run": run }))
        }
    }
}

/// `POST /api/v1/cockpit/pane/pr {pane_id, force?}` — THE PANE'S PULL REQUEST (QA v2, 2026-09-21): the PR an
/// agent leaves behind when it is closed. The box's QA verdicts ride on the body; when the latest judged
/// run FAILED the box is sealed — `{ok:false, sealed:true, qa}` — until the operator forces it.
pub async fn pane_pr(state: Arc<AppState>, goals: Arc<Goals>, body: Bytes) -> Response {
    let b = body_json(&body);
    let pane_id = n(b.get("pane_id"));
    if pane_id <= 0 { return bad("pane_id required"); }
    let force = b.get("force").and_then(Value::as_bool).unwrap_or(false);
    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
    let pane = engine_call(&state, "system.cockpit.pane.live", json!({ "id": pane_id }), ctx.clone()).await.map(|(v, _)| v).unwrap_or(Value::Null);
    let row = pane.get("result").cloned().unwrap_or(pane);
    if !row.get("found").and_then(Value::as_bool).unwrap_or(false) { return bad(&format!("pane {pane_id} not found")); }
    let cwd = s(row.get("worktree_path"));
    let runs = goals.list(None);
    if !force {
        if let Some(q) = Goals::seal_for(&runs, &cwd) {
            return ok(json!({ "ok": false, "sealed": true, "pane_id": pane_id, "qa": { "passed": q.passed, "score": q.score, "summary": q.summary, "findings": q.findings }, "error": { "message": format!("sealed — the QA judge failed the latest run ({}/100): {}", q.score, q.summary) } }));
        }
    }
    let verdicts = Goals::box_verdicts_md(&runs, &cwd);
    let mut input = json!({ "pane_id": pane_id });
    if let Some(t) = b.get("title").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) { input["title"] = json!(t); }
    if !verdicts.is_empty() { input["message"] = json!(format!("{}{}", b.get("message").and_then(Value::as_str).map(|m| format!("{m}\n\n")).unwrap_or_default(), verdicts)); }
    else if let Some(m) = b.get("message").and_then(Value::as_str) { input["message"] = json!(m); }
    match engine_call(&state, "system.cockpit.pr.open", input, ctx).await {
        Ok((v, _)) => { let r = v.get("result").cloned().unwrap_or(v); ok(json!({ "ok": true, "result": r, "forced": force && Goals::seal_for(&runs, &cwd).is_some() })) }
        Err(e) => bad(&format!("could not open the pull request: {e}")),
    }
}

pub async fn subagents_get(subs: Arc<Subagents>, q: HashMap<String, String>) -> Response {
    ok(json!({ "jobs": subs.list(q.get("parent").map(String::as_str)) }))
}

pub async fn subagents_op(subs: Arc<Subagents>, run: crate::goals::RunFn, op: String, body: Bytes) -> Response {
    let b = body_json(&body);
    match op.as_str() {
        "spawn" => {
            let engine = engine_of(b.get("engine").unwrap_or(&Value::Null)).unwrap_or_else(default_seat);
            let tasks: Vec<(String, Option<String>)> = b.get("tasks").and_then(Value::as_array).map(|a| a.iter().filter_map(|t| { let title = s(t.get("title")).trim().to_string(); if title.is_empty() { None } else { Some((title, t.get("prompt").and_then(Value::as_str).map(String::from))) } }).collect()).unwrap_or_default();
            if tasks.is_empty() { return bad("tasks required"); }
            let cwd = { let c = s(b.get("cwd")); if c.is_empty() { root() } else { c } };
            let jobs = subs.spawn(&s(b.get("parent")), &s(b.get("goal")), &tasks, engine, &cwd, b.get("write").and_then(Value::as_bool).unwrap_or(false));
            ok(json!({ "ok": true, "jobs": jobs }))
        }
        "plan" => {
            let engine = engine_of(b.get("engine").unwrap_or(&Value::Null)).unwrap_or_else(default_seat);
            let k = n(b.get("n")).clamp(2, 8) as usize;
            let cwd = { let c = s(b.get("cwd")); if c.is_empty() { root() } else { c } };
            match run(engine, vec![crate::cli::ChatMessage { role: "user".into(), content: crate::goals::simple_plan_prompt(&s(b.get("goal")), k) }], cwd, true, crate::cli::RunOpts { template: Some("goal.simple_plan".into()), ..Default::default() }, None).await {
                Ok(r) => ok(json!({ "ok": true, "tasks": crate::goals::parse_simple_plan(&r.parsed.content, k).into_iter().map(|(title, prompt)| json!({ "title": title, "prompt": prompt })).collect::<Vec<_>>(), "ms": r.ms })),
                Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": { "message": e } }))).into_response(),
            }
        }
        "cancel" => ok(json!({ "ok": subs.cancel(n(b.get("id"))) })),
        "delivered" => ok(json!({ "ok": subs.mark_delivered(n(b.get("id"))) })),
        _ => bad("unknown subagent op"),
    }
}

/// THE CLI COMPLETIONS (2026-09-19 — the product's own "chat engine server"; the dev BE ran it in a
/// worker thread): `POST /api/v1/cli-llm/chat/completions {model: "cli:<engine>" | "cli:auto", messages,
/// response_format?}` runs the named coding CLI headless in the workspace root, READ-ONLY, and answers
/// in the OpenAI shape ai-call parses. `cli:auto` (2026-10-01, 4ba2d50) names no seat: the serve picks the one
/// Heimdall plans on; a named seat that is switched off or gone is swapped for the router's pick (see below). Every `cli` provider row of every tier dispatches here — the
/// operator's subscriptions carry the substrate's AI. `ANTILOKI_CLI_LLM_URL` points ai-call at it.
pub async fn cli_completions(body: Bytes) -> Response {
    let b = body_json(&body);
    // `cli:auto` = the caller pinned nothing and the serve picks the seat Heimdall plans on (2026-10-01, "Engine config also": the FE used to send `cli:claude-code` for "no pin")
    let Some(engine) = crate::cli::engine_or_auto(b.get("model").unwrap_or(&Value::Null)) else {
        return bad(&format!("unknown CLI engine '{}' — use cli:auto or cli:<{}>", s(b.get("model")), Engine::all().iter().map(|e| e.key()).collect::<Vec<_>>().join(" · ")));
    };
    // THE SEAT THE CALLER NAMED MAY BE GONE (2026-10-01, first real run with Claude switched off): the FE's `askEngine` pins `cli:claude-code` unless the operator picked another, so every X-ray
    // feature (ultra review, data stories, the design view, side questions, the context summaries) answered "Claude is switched off" with a seat that could have done it sitting idle. The
    // call goes to the seat Heimdall would plan on; the answer's `model` says which one ran.
    let engine = crate::cli::seat_or_heimdall(engine);
    let mut messages: Vec<crate::cli::ChatMessage> = b.get("messages").and_then(Value::as_array).map(|a| a.iter().map(|m| crate::cli::ChatMessage {
        role: s(m.get("role")), content: match m.get("content") { Some(Value::String(c)) => c.clone(), Some(v) => v.to_string(), None => String::new() },
    }).collect()).unwrap_or_default();
    if messages.is_empty() && s(b.get("template")).is_empty() { return bad("messages required"); }
    let wants_json = s(b.get("response_format").and_then(|r| r.get("type"))) == "json_object" || b.get("response_format").and_then(|r| r.get("json_schema")).is_some();
    // `template` + `vars` (2026-09-19): the caller names a prompt-catalog base and the serve renders its
    // ACTIVE version as the user message — the FE never carries prompt text, and the call is recorded
    // under that template so Settings › Prompts and Statistics count it.
    let template = { let t = s(b.get("template")); if t.is_empty() { None } else { Some(t) } };
    if let Some(base) = &template {
        if crate::prompts::catalog_entry(base).is_none() && crate::prompts::resolve(base).is_none() { return bad(&format!("unknown prompt template '{base}'")); }
        let vars = b.get("vars").and_then(Value::as_object).cloned().unwrap_or_default();
        messages.push(crate::cli::ChatMessage { role: "user".into(), content: crate::prompts::render_base(base, &vars) });
    }
    // the substrate's calls are READ-ONLY: the CLI may read and search the workspace, never change it
    messages.insert(0, crate::cli::ChatMessage { role: "system".into(), content: crate::prompts::text("cli.readonly_system") });
    // the engine runs in the ACTIVE WORKSPACE (the X-ray's repo), not in the substrate root
    let cwd = { let c = s(b.get("cwd")); if c.is_empty() { crate::prompts::workspace_root().unwrap_or_else(root) } else { c } };
    // `max_turns`: a deep read (the ultra review) needs more tool turns than a chat answer — the caller says how many
    let max_turns = b.get("max_turns").and_then(Value::as_u64).map(|n| n.clamp(1, 80) as u32);
    let tier = b.get("tier").and_then(Value::as_str).and_then(crate::cli::Tier::of).or_else(|| template.as_deref().and_then(crate::prompts::catalog_entry).and_then(|t| crate::cli::Tier::of(t.tier)));
    // `cli_model` (2026-10-10): name the MODEL of the seat (a bench compares models of one seat; without it the tier picks the seat's model as before)
    let model = b.get("cli_model").and_then(Value::as_str).map(str::trim).filter(|m| !m.is_empty()).map(String::from);
    match crate::cli::run_cli(engine, &messages, &cwd, wants_json, crate::cli::RunOpts { max_turns, tier, template, model, ledger: Some(crate::ledger::Ctx::workspace(&cwd, "substrate")), ..Default::default() }, None).await {
        Ok(out) if !out.parsed.content.trim().is_empty() => ok(crate::cli::completion(&format!("cli:{}", engine.key()), &out.parsed.content, &out.parsed)),
        Ok(out) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": { "message": format!("{} returned no text{}", engine.key(), out.parsed.error.map(|e| format!(" — {e}")).unwrap_or_default()) } }))).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({ "error": { "message": e } }))).into_response(),
    }
}

pub async fn traces(q: HashMap<String, String>) -> Response {
    let since = q.get("since").and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0);
    ok(json!({ "traces": crate::cli::traces_since(since) }))
}

// ── the pane boot: the pack baked before the CLI starts, and the flags that carry it in ─────────
// A port of the BE's contextArgs / packSystemPrompt (focus/engines.ts) and the launch record.
pub const PACK_INDEX: &str = ".antiloki/context/INDEX.md";
pub const PACK_MCP_CONFIG: &str = ".antiloki/mcp.json";

pub fn pack_system_prompt(pack: &Value) -> String { pack_system_prompt_with(pack, &Delegation::default()) }
/// THE DELEGATION POLICY of a pane (operator 2026-09-19: "sub-agents are spawned by the parent agent,
/// not on a click — just config: on/off, how many, which AI, which tier"): read from the pane's
/// settings_json.delegate; everything defaults to "on, 4, the parent's engine, the planner decides".
#[derive(Clone, Debug)]
pub struct Delegation { pub enabled: bool, pub max: usize, pub engine: Option<String>, pub tier: Option<crate::cli::Tier>,
    /// HEIMDALL'S ROUTER (operator 2026-09-20): `settings.router.enabled`, default on
    pub router: bool,
    /// THE HUMAN GATE (2026-09-20, antiFlow's human-approve stage): `settings.delegate.approve` —
    /// "auto" (the agent orchestrates) or "ask" (the plan waits for the operator in the pane)
    pub ask_first: bool,
    /// MIN (2026-09-21): `settings.min` — the pane navigates with the pack and the tools, never the tree
    pub min: bool,
    /// QA (2026-09-21): `settings.qa.enabled` — an isolated judge reviews every finished run of this pane
    pub qa: bool,
    /// THE MODELS BAR (`settings.dial.cost`, 2026-09-22): 0 CHEAPER · 1 BALANCED · 2 SMARTER — one rung down or up
    /// the operator's own ladder for each role. None when the pane has no dial yet.
    pub cost: Option<u8>,
    /// HEIMDALL'S THREE STATES (`settings.router.mode`, operator 2026-09-22: "add an auto mode on heimdall as well
    /// auto(best), and an on and off also"). `auto` = the router routes AND picks the models itself — the role
    /// bindings as bound, the bars' model stop ignored; `on` = the router routes, the operator's bars win;
    /// `off` = no routing at all. Older panes carry only `router.enabled`, which reads as on/off.
    pub mode: String,
    /// THE PAIR (`settings.pair`, 2026-09-22): the merge this agent runs as — brain and hands. None = solo.
    pub pair: Option<crate::cli::Pair>,
    /// HEIMDALL ROUTES THE ROLE TOO (`settings.roles.enabled`, operator 2026-09-26: "on the roles we add the
    /// dinamic to heimdall. So heimdall not just routes the model, it routes the role, like it passes the baston
    /// like on maratons").
    ///
    /// DEFAULT ON, unlike `qa`. The whole point is that nobody should have to assemble a roster before working:
    /// the ask says what kind of work it is, the role adapter follows, and the operator overrides only when they
    /// disagree. A flag that defaulted off would leave the feature switched off for everyone who never found it.
    pub roles: bool,
    /// WHICH WINNER A ROLE GETS (same ask: "it switches to the best ranked merge of that role, unless the user on
    /// settings chose to go by the faster ou cheaper").
    ///
    /// IT IS EFFORT (`settings.dial.cost`, operator 2026-09-26: "Make it all effort → Cheaper Balanced Faster …
    /// because effort already routes through providers merge and model inside provider").
    ///
    /// Effort used to mean "one rung up or down WITHIN the merge you are bound to". Freed from the provider it
    /// means the only thing left to choose: among the pairings the quality band already accepts, which resource
    /// would you rather spend. 0 CHEAPER · 1 BALANCED · 2 FASTER.
    ///
    /// AND THERE IS NO "SMARTER" STOP, which is the operator's point and it is right. Quality stopped being a
    /// score and became a gate, so "buy more quality" is not a thing you can do: a row outside the band is
    /// disqualified rather than expensive, and differences INSIDE the band are — by construction — smaller than
    /// the record can measure. `merges::separated` says so out loud. A SMARTER stop would have sold the operator
    /// the right to buy noise.
    pub role_objective: crate::roles::Objective,
    /// MID-TASK FORM CHANGES (`settings.roles.mid_task`, PLAN-THE-CHAMELEON C2) — each TASK takes the role of the
    /// files it owns, so a plan that touches the screen and then the API is done by two forms.
    ///
    /// DEFAULT OFF, and the reason is a measurement that does not exist yet. C4 answers "what does a hand-off
    /// cost" in seconds and tokens — both now measured in situ — but the third number, whether the work is
    /// actually BETTER for having switched or whether the next form just re-reads the last one's output, needs a
    /// paid run of the same task with and without. Until that exists this is a flag, not a default, because the
    /// honest position is "we think so" and a default would say "we measured it".
    pub roles_mid_task: bool }
impl Default for Delegation { fn default() -> Self { Delegation { enabled: true, max: 4, engine: None, tier: None, router: true, ask_first: false, min: false, qa: true, cost: None, mode: "auto".into(), pair: None, roles: true, role_objective: crate::roles::Objective::Best, roles_mid_task: false } } }
impl Delegation {
    pub fn of_pane(pane: &Value) -> Delegation {
        let settings: Value = pane.get("settings_json").and_then(Value::as_str).and_then(|raw| serde_json::from_str(raw).ok()).unwrap_or(Value::Null);
        let d = settings.get("delegate").cloned().unwrap_or(Value::Null);
        let engine = d.get("engine").and_then(Value::as_str).map(str::trim).filter(|e| !e.is_empty() && *e != "routed" && *e != "same").map(String::from);
        Delegation {
            enabled: d.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            max: d.get("max").and_then(Value::as_u64).map(|m| (m as usize).clamp(1, 8)).unwrap_or(4),
            engine,
            tier: d.get("tier").and_then(Value::as_str).and_then(crate::cli::Tier::of),
            router: settings.get("router").and_then(|r| r.get("enabled")).and_then(Value::as_bool).unwrap_or(true),
            ask_first: d.get("approve").and_then(Value::as_str) == Some("ask"),
            min: settings.get("min").and_then(Value::as_bool).unwrap_or(false),
            qa: settings.get("qa").and_then(|q| q.get("enabled")).and_then(Value::as_bool).unwrap_or(false),
            cost: settings.get("dial").and_then(|d| d.get("cost")).and_then(Value::as_u64).map(|c| c.min(2) as u8),
            pair: crate::cli::Pair::of_settings(&settings),
            roles: settings.get("roles").and_then(|r| r.get("enabled")).and_then(Value::as_bool).unwrap_or(true),
            roles_mid_task: settings.get("roles").and_then(|r| r.get("mid_task")).and_then(Value::as_bool).unwrap_or(false),
            // Effort's three stops, in the order the bar draws them; anything unexpected is `best`
            role_objective: match settings.get("dial").and_then(|d| d.get("cost")).and_then(Value::as_u64) {
                Some(0) => crate::roles::Objective::Cheaper,
                Some(2) => crate::roles::Objective::Faster,
                _ => crate::roles::Objective::Best,
            },
            mode: match settings.get("router").and_then(|r| r.get("mode")).and_then(Value::as_str).map(str::trim) {
                Some("auto") => "auto".into(), Some("on") => "on".into(), Some("off") => "off".into(),
                // no mode on this pane yet: the old boolean is the answer
                _ => if settings.get("router").and_then(|r| r.get("enabled")).and_then(Value::as_bool).unwrap_or(true) { "auto".into() } else { "off".into() },
            },
        }
    }
}
pub fn pack_system_prompt_with(pack: &Value, d: &Delegation) -> String {
    let files = pack.get("items").and_then(Value::as_array).map(|a| a.iter().filter(|i| i.get("written").and_then(Value::as_bool).unwrap_or(false)).count()).unwrap_or(0);
    let live = pack.get("live").and_then(Value::as_bool).unwrap_or(false);
    // rendered from the catalog (agent.pack_system): the pack line (or the no-pack line), the live MCP
    // tools when the server is on, the orchestration sentence only when the pane may delegate
    let vars: serde_json::Map<String, Value> = [("files", if files > 0 { json!(files) } else { json!("") }), ("index", json!(PACK_INDEX)), ("live", json!(live)), ("delegate", json!(live && d.enabled)), ("min", json!(d.min)), ("max_subagents", json!(d.max))].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    let intro = crate::prompts::render_base("agent.antiloki_intro", &serde_json::Map::new());
    format!("{}\n\n{intro}", crate::prompts::render_base("agent.pack_system", &vars))
}

/// The CLI flags that carry a pane's baked context into the engine's OWN channels, on top of the
/// AGENTS.md/CLAUDE.md brief every engine reads. `pack` is the bake result — null when nothing was baked.
///   claude   → `--append-system-prompt` and, with live tools, `--mcp-config .antiloki/mcp.json`
///   codex    → `-c mcp_servers.antiloki.command=… -c mcp_servers.antiloki.args=[…]` (+ env, tools pre-approved)
///   opencode → nothing: the bake already wrote `instructions` + `mcp` into opencode.json
pub fn context_args(engine_key: &str, pack: &Value) -> Vec<String> { context_args_for(engine_key, pack, &Delegation::default()) }
pub fn context_args_for(engine_key: &str, pack: &Value, d: &Delegation) -> Vec<String> {
    if pack.get("ok") != Some(&Value::Bool(true)) { return vec![]; }
    let live = pack.get("live").and_then(Value::as_bool).unwrap_or(false);
    match engine_key {
        "claude-code" => { let mut a = vec!["--append-system-prompt".to_string(), pack_system_prompt_with(pack, d)]; if live { a.push("--mcp-config".into()); a.push(PACK_MCP_CONFIG.into()); } a }
        "codex" => {
            let Some(server) = pack.get("mcp_server").filter(|e| e.get("command").map(Value::is_string).unwrap_or(false)) else { return vec![] };
            if !live { return vec![]; }
            let mut a = vec!["-c".to_string(), format!("mcp_servers.antiloki.command={}", server["command"]), "-c".into(), format!("mcp_servers.antiloki.args={}", server.get("args").cloned().unwrap_or(json!([])))];
            if let Some(env) = server.get("env").and_then(Value::as_object) { for (k, v) in env { a.push("-c".into()); a.push(format!("mcp_servers.antiloki.env.{k}={}", json!(v.as_str().map(String::from).unwrap_or_else(|| v.to_string())))); } }
            // Codex asks "Allow the antiloki MCP server to run tool X?" per tool otherwise (2026-09-19: a
            // parent stalled on antiloki_delegate_result). The tools are antiloki's own — pre-approved.
            // The value is "approve" (codex 0.149 knows prompt | writes | approve; "auto" from newer docs
            // was silently ignored and the prompt came back).
            a.push("-c".into()); a.push("mcp_servers.antiloki.default_tools_approval_mode=\"approve\"".into());
            // antiQA beside it when the pane's Heimdall button is on (RFC 0001): the same `-c` form, and pre-approved for the same reason -
            // the operator switched the tool on, so a per-call prompt would only stall a headless turn
            if let Some(q) = pack.get("antiqa_server").filter(|e| e.get("command").map(Value::is_string).unwrap_or(false)) {
                a.push("-c".into()); a.push(format!("mcp_servers.antiqa.command={}", q["command"]));
                a.push("-c".into()); a.push(format!("mcp_servers.antiqa.args={}", q.get("args").cloned().unwrap_or(json!([]))));
                if let Some(env) = q.get("env").and_then(Value::as_object) { for (k, v) in env { a.push("-c".into()); a.push(format!("mcp_servers.antiqa.env.{k}={}", json!(v.as_str().map(String::from).unwrap_or_else(|| v.to_string())))); } }
                a.push("-c".into()); a.push("mcp_servers.antiqa.default_tools_approval_mode=\"approve\"".into());
            }
            a
        }
        _ => vec![],
    }
}

/// THE LAUNCH RECORD: what the CLI was started with — the prompt window shows it as the first thing
/// delivered to the model (system prompt addendum, flags, env names; never the proxy key).
pub fn write_launch_record(cwd: &str, bin: &str, args: &[String], env: &HashMap<String, String>) {
    let i = args.iter().position(|a| a == "--append-system-prompt");
    let env_shown: serde_json::Map<String, Value> = env.iter().map(|(k, v)| (k.clone(), json!(if k == "OPENAI_API_KEY" { "(pane proxy key)" } else { v.as_str() }))).collect();
    let rec = json!({ "cmd": bin, "args": args, "system_prompt": i.and_then(|i| args.get(i + 1)), "env": env_shown, "t": crate::cli::now_ms() });
    let dir = std::path::Path::new(cwd).join(".antiloki");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("launch.json"), rec.to_string());
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_daily_line_has_one_point_per_day_priced_at_openrouter_and_counts_team_runs() {
        use serde_json::json; let t: Vec<(String, f64, f64)> = vec![("anthropic/claude-opus-5.5".into(), 4.0, 20.0)];
        let rows = vec![("2026-11-20".to_string(), "claude-opus-5-5".to_string(), 1_000_000, 0, 0, 0), ("2026-11-21".to_string(), "mystery".to_string(), 5, 5, 0, 0)];
        let runs = vec![json!({ "started_ms": 1_795_267_200_000i64, "spent_usd": 0.5 })];   // 2026-11-21 in UTC
        let d = super::daily_series(&rows, &runs, 3, "2026-11-21", &t, 0);
        assert_eq!(d.len(), 3); assert_eq!(d[0]["day"], json!("2026-11-19")); assert_eq!(d[0]["cost"], json!(0.0)); assert!((d[1]["cost"].as_f64().unwrap() - 4.0).abs() < 1e-9); assert!((d[2]["cost"].as_f64().unwrap() - 0.5).abs() < 1e-9, "an unpriced model adds nothing, the run adds what it spent");
        assert_eq!(super::days_to_ymd(0), "1970-01-01"); assert_eq!(super::ymd_to_days("2026-11-21").map(super::days_to_ymd).as_deref(), Some("2026-11-21"));
    }
    #[test]
    fn a_window_starts_on_the_right_day() { assert_eq!(super::window_first_day("2026-11-21", 7), "2026-11-15"); assert_eq!(super::window_first_day("2026-11-21", 1), "2026-11-21"); assert_eq!(super::window_first_day("2026-03-02", 3), "2026-02-28"); }
    #[test]
    fn the_teams_money_counts_the_runs_inside_the_window_and_the_window_starts_where_it_says() {
        use serde_json::json; let runs = vec![json!({ "started_ms": 1000, "spent_usd": 0.5 }), json!({ "started_ms": 90_000_000, "spent_usd": 0.25 }), json!({ "started_ms": null, "spent_usd": 9.0 })];
        assert_eq!(super::team_spend(&runs, 0), (2, 0.75)); assert_eq!(super::team_spend(&runs, 2000), (1, 0.25)); assert_eq!(super::team_spend(&[], 0), (0, 0.0));
        assert_eq!(super::window_start_ms(200_000_000, 1, 0), 200_000_000 - 86_400_000);
        assert_eq!(super::window_start_ms(100_000_000, 0, 0), 86_400_000, "midnight UTC"); assert_eq!(super::window_start_ms(100_000_000, 0, -180), 97_200_000, "midnight at UTC-3 is 03:00 UTC");
    }
    use super::*;

    /// THE FUEL CACHE IS PER WORKSPACE (found 2026-10-01): two workspaces within one TTL were served each other's body because the key was (days, tz) only.
    #[test]
    fn the_fuel_cache_never_serves_one_workspace_the_body_of_another() {
        let a = fuel_cache_key(7, 0, "/home/user/Projects/a");
        let b = fuel_cache_key(7, 0, "/home/user/Projects/b");
        assert_ne!(a, b, "the workspace root is part of the key");
        let mut cache = std::collections::BTreeMap::new(); cache.insert(a.clone(), (10_000i64, json!({ "ok": true, "who": "a" })));
        let hour = 3_600_000;
        assert_eq!(fuel_cache_hit(cache.get(&a), 10_000 + hour - 1, hour), Some(json!({ "ok": true, "who": "a" })), "the same workspace within the hour is not scanned again");
        assert_eq!(fuel_cache_hit(cache.get(&b), 10_500, hour), None, "another workspace is a miss");
        assert_eq!(fuel_cache_hit(cache.get(&a), 10_000 + hour, hour), None, "after the hour it is scanned again");
        assert_ne!(fuel_cache_key(7, 0, "/x"), fuel_cache_key(30, 0, "/x"));
        assert_ne!(fuel_cache_key(7, 0, "/x"), fuel_cache_key(7, 60, "/x"));
    }

    /// HEIMDALL ROUTES THE ROLE, AND THE DEFAULT IS THE POINT (2026-09-26). A flag that defaulted off would leave
    /// the feature switched off for everyone who never went looking for it, which is the opposite of "the client
    /// doesn't need to create any agent".
    #[test]
    fn role_routing_is_on_unless_the_pane_says_otherwise_and_best_is_the_default_winner() {
        let pane = |settings: Value| json!({ "settings_json": settings.to_string() });

        // a pane that has never heard of the setting still routes roles, at `best`
        let d = Delegation::of_pane(&pane(json!({})));
        assert!(d.roles, "role routing must not need to be discovered");
        assert_eq!(d.role_objective, crate::roles::Objective::Best);

        // EFFORT IS THE OBJECTIVE (operator 2026-09-26). It already sits in the Heimdall menu where routing is
        // decided; a second control for the same question would have left two bars disagreeing.
        for (stop, want) in [(0, crate::roles::Objective::Cheaper), (1, crate::roles::Objective::Best), (2, crate::roles::Objective::Faster)] {
            let d = Delegation::of_pane(&pane(json!({ "dial": { "cost": stop } })));
            assert_eq!(d.role_objective, want, "effort stop {stop}");
        }
        // an unset or unexpected dial is `best`, never a guess
        assert_eq!(Delegation::of_pane(&pane(json!({}))).role_objective, crate::roles::Objective::Best);
        assert_eq!(Delegation::of_pane(&pane(json!({ "dial": { "cost": 9 } }))).role_objective, crate::roles::Objective::Best);
        // AND THERE IS NO SMARTER STOP: quality is a gate now, so there is no "more quality" to buy, and
        // differences inside the band are smaller than the record can measure.
        assert_eq!(crate::roles::Objective::OFFERED.map(|o| o.key()), ["cheaper", "faster", "best"]);

        // MID-TASK IS OFF UNTIL IT IS MEASURED (C2/C4). Role routing defaults ON because it only picks between
        // winners already in the record; mid-task changes ENGINES inside a running plan, and whether that pays is
        // the one number C4 has not bought yet. A default here would claim a measurement we do not have.
        assert!(!Delegation::of_pane(&pane(json!({}))).roles_mid_task);
        assert!(Delegation::of_pane(&pane(json!({ "roles": { "mid_task": true } }))).roles_mid_task);
        assert!(Delegation::of_pane(&pane(json!({}))).roles, "…while role routing itself stays on");

        // and it can be switched off, like any other flag
        assert!(!Delegation::of_pane(&pane(json!({ "roles": { "enabled": false } }))).roles);
        // QA IS ON BY DEFAULT (operator 2026-09-27: "make QA part of the flow not optional", agreed after the
        // measurement). This assertion used to read `!.qa`, with the reasoning that a judge's call on every run is too
        // much to spend by default. The benches answered that: the writers get an ambiguous ask wrong about one run in
        // three, every judge caught that defect 4/4 — including haiku and Composer — and across 48 judged artefacts no
        // arm produced a single FALSE REJECTION. So the gate spends a re-ask only when there is something to re-ask
        // about, which is a different bargain from the one this comment assumed.
        // QA IS OPTIONAL AGAIN (operator 2026-10-10: "now QA is optional not obligatory; confidence is displayed after the results and a suggestion to run independent QA can be shown like a button"): absent means OFF;
        // the result carries its confidence and offers to run QA (`confidence.rs`, goals op `qa`). An explicit yes turns the one judge on.
        assert!(!Delegation::of_pane(&pane(json!({}))).qa, "absent means no QA pass");
        assert!(Delegation::of_pane(&pane(json!({ "qa": { "enabled": true } }))).qa, "an explicit yes is a yes");
        assert!(!Delegation::of_pane(&pane(json!({ "qa": { "enabled": false } }))).qa, "an explicit no is still a no");
    }

    #[test]
    fn context_args_per_engine() {
        let pack = json!({ "ok": true, "live": true, "items": [{ "written": true }, { "written": false }], "mcp_server": { "command": "/opt/antiloki-agent", "args": ["mcp"], "env": { "ANTILOKI_PANE_ID": "7" } } });
        let c = context_args("claude-code", &pack); assert!(c[1].contains("YOU ARE NOT A PLAIN CLI AGENT") && c[1].contains("antiloki_pane_open"), "the antiloki introduction rides in every Claude start");
        assert_eq!(c[0], "--append-system-prompt"); assert!(c[1].starts_with("antiloki has baked a CONTEXT PACK for this worktree: 1 file(s)")); assert!(c[1].contains("antiloki_delegate"));
        assert_eq!(&c[2..], ["--mcp-config", ".antiloki/mcp.json"]);
        let x = context_args("codex", &pack);
        assert_eq!(x, ["-c", "mcp_servers.antiloki.command=\"/opt/antiloki-agent\"", "-c", "mcp_servers.antiloki.args=[\"mcp\"]", "-c", "mcp_servers.antiloki.env.ANTILOKI_PANE_ID=\"7\"", "-c", "mcp_servers.antiloki.default_tools_approval_mode=\"approve\""]);
        assert!(context_args("opencode", &pack).is_empty());
        assert!(context_args("claude-code", &json!({ "ok": false })).is_empty());
        let quiet = context_args("claude-code", &json!({ "ok": true, "live": false, "items": [] }));
        assert_eq!(quiet.len(), 2); assert!(quiet[1].starts_with("antiloki measures this repo."));
        assert!(context_args("codex", &json!({ "ok": true, "live": false, "mcp_server": { "command": "x" } })).is_empty());
    }

    #[test]
    fn the_delegation_policy_reads_the_pane_with_defaults_and_clamps() {
        let d = Delegation::of_pane(&json!({ "settings_json": "{}" }));
        assert!(d.enabled && d.max == 4 && d.engine.is_none() && d.tier.is_none());
        let d = Delegation::of_pane(&json!({ "settings_json": "{\"delegate\":{\"enabled\":false,\"max\":99,\"engine\":\"codex\",\"tier\":\"low\"}}" }));
        assert!(!d.enabled); assert_eq!(d.max, 8); assert_eq!(d.engine.as_deref(), Some("codex")); assert_eq!(d.tier, Some(crate::cli::Tier::Low));
        assert!(!d.min); assert!(Delegation::of_pane(&json!({ "settings_json": "{\"min\":true}" })).min, "MIN is a pane setting");
        let d = Delegation::of_pane(&json!({ "settings_json": "{\"delegate\":{\"max\":0,\"engine\":\"routed\",\"tier\":\"auto\"}}" }));
        assert_eq!(d.max, 1); assert!(d.engine.is_none()); assert!(d.tier.is_none());
        assert!(Delegation::of_pane(&json!({ "settings_json": "not json" })).enabled, "junk settings = defaults");
        // HEIMDALL'S THREE STATES: an explicit mode wins; a pane that has only the old boolean still reads right
        let mode = |j: &str| Delegation::of_pane(&json!({ "settings_json": j })).mode;
        assert_eq!(mode("{}"), "auto", "a new pane is on AUTO — the router picks the models");
        assert_eq!(mode("{\"router\":{\"mode\":\"on\"}}"), "on");
        assert_eq!(mode("{\"router\":{\"mode\":\"off\"}}"), "off");
        assert_eq!(mode("{\"router\":{\"enabled\":false}}"), "off", "the old boolean, no mode written yet");
        assert_eq!(mode("{\"router\":{\"enabled\":true}}"), "auto");
        assert_eq!(mode("{\"router\":{\"enabled\":false,\"mode\":\"auto\"}}"), "auto", "the mode is the newer truth");
        // THE PAIR rides on the pane: a name, a shape, or nothing
        let pair = |j: &str| Delegation::of_pane(&json!({ "settings_json": j })).pair.map(|p| p.label());
        assert_eq!(pair("{}"), None);
        // PORTED off the withdrawn Zen merges (2026-09-27) — a retired NAME now resolves to nothing, which is the
        // point of withdrawing it, so the named case uses a subscription merge.
        assert_eq!(pair("{\"pair\":\"claude-grok\"}"), Some("Claude · Opus × Cursor · Grok 4.6 · medium".into()));
        assert_eq!(pair("{\"pair\":\"claude-deepseek\"}"), None, "a discontinued merge cannot be named back into existence");
        // an explicit SHAPE still carries whatever it names — the pane may describe a pair the built-ins do not offer
        assert_eq!(pair("{\"pair\":{\"name\":\"x\",\"brain\":{\"engine\":\"claude-code\"},\"hands\":{\"engine\":\"cursor\",\"model\":\"composer-2.5\"}}}"), Some("Claude · the job's model × Cursor · Composer 2.5".into()));
        // the plan body: the same three spellings (the merge bench sends the name)
        let from_body = |v: Value| serde_json::from_value::<crate::cli::Pair>(v.clone()).ok().or_else(|| crate::cli::Pair::of_settings(&json!({ "pair": v }))).map(|p| p.label());
        assert_eq!(from_body(json!("codex-gemini")), Some("Codex · the account's model · medium × Antigravity · 3.8 Flash Low".into()));
        assert_eq!(from_body(serde_json::to_value(crate::cli::Pair::builtin("grok-composer").unwrap()).unwrap()), Some("Cursor · Grok 4.6 · high × Cursor · Composer 2.5".into()));
        assert_eq!(from_body(json!("solo")), None);
        // the brief follows the policy: delegation off drops the orchestration sentence, on names the cap
        let pack = json!({ "ok": true, "live": true, "items": [] });
        let off = pack_system_prompt_with(&pack, &Delegation { enabled: false, ..Delegation::default() });
        assert!(!off.contains("ORCHESTRATE") && off.contains("Live MCP tools"));
        let on = pack_system_prompt_with(&pack, &Delegation { max: 2, ..Delegation::default() });
        assert!(on.contains("up to 2 at a time"));
    }
}

#[cfg(test)]
mod fuel_scope_tests {
    use super::*;
    #[test]
    fn timeline_keeps_this_workspace_and_its_boxes_not_every_agent_home() {
        let root = "/home/me/shop";
        let box_parent = crate::boxes::box_of(root, "__scope__").parent().unwrap().to_string_lossy().into_owned();
        let mk = |cwd: &str| crate::goals::GoalRun { id: 1, parent: String::new(), goal: String::new(), engine: crate::cli::Engine::Codex, cwd: cwd.into(), write: false, concurrency: 1, status: crate::goals::RunStatus::Done, created: 0, started: None, ended: None, tasks: vec![], blackboard: vec![], events: vec![], report: None, delivered: false, error: None, route: None, questions: vec![], assumptions: vec![], plan_args: crate::goals::PlanArgs::default(), pr: None, router: false, min: false, workers: None, mode: String::new(), blueprint: None, stages: vec![], qa_gate: None, cost: None, pair: None, qa_double: false, qa_first: None, qa_review_wanted: false, qa_review: None, savings: None, qa: None, qa_baseline: None, qa_baseline_pending: false, compare: None, mid_task: false, base: None, qa_pending: false, qa_rounds: 0, design: None, plan_usd: 0.0, screen_usd: 0.0, plan_basis: String::new(), plan_binding: String::new(), worker_binding: String::new(), suite: String::new(), confidence: None, pane_id: 0, scope: vec![], checks_off: false, merge_outcome: None, role: None, plan_ms: 0 };
        assert!(timeline_run_visible(&mk(root), root, &box_parent));
        assert!(timeline_run_visible(&mk(&format!("{root}/src")), root, &box_parent));
        assert!(timeline_run_visible(&mk(&format!("{box_parent}/agent-a")), root, &box_parent));
        assert!(!timeline_run_visible(&mk("/home/other/proj"), root, &box_parent));
        let other_box = crate::boxes::box_of("/home/other", "__scope__").parent().unwrap().to_string_lossy().into_owned();
        assert!(!timeline_run_visible(&mk(&format!("{other_box}/x")), root, &box_parent));
    }
    #[test]
    fn a_run_belongs_to_its_own_workspace_only() {
        assert!(in_workspace("/home/me/shop", "/home/me/shop"));
        assert!(in_workspace("/home/me/shop/", "/home/me/shop"));
        assert!(in_workspace("/home/me/shop/packages/web", "/home/me/shop"));
        assert!(!in_workspace("/home/me/shop-2", "/home/me/shop"), "a sibling that shares a prefix is another workspace");
        assert!(!in_workspace("/home/me/other", "/home/me/shop"));
        assert!(in_workspace("C:\\dev\\shop\\src", "C:/dev/shop"));
        assert!(!in_workspace("/anything", ""), "no workspace open owns nothing");
    }
    #[test]
    fn claude_windows_come_from_the_newest_meter_on_the_box_and_drop_the_ones_already_reset() {
        let d = std::env::temp_dir().join(format!("antiloki-meter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (a, b) = (d.join("a"), d.join("b"));
        for r in [&a, &b] { std::fs::create_dir_all(r.join(".antiloki")).unwrap(); }
        std::fs::write(a.join(".antiloki/usage.json"), r#"{"t":"2026-09-20T10:00:00Z","limits":[{"label":"5h","used_pct":1.0,"resets_at":2000000000}]}"#).unwrap();
        std::fs::write(b.join(".antiloki/usage.json"), r#"{"t":"2026-09-30T10:00:00Z","limits":[{"label":"5h","used_pct":40.0,"resets_at":1000},{"label":"week","used_pct":83.0,"resets_at":2000000000}]}"#).unwrap();
        let got = claude_windows_anywhere(&[a.clone(), b.clone(), d.join("missing")], 1_900_000_000);
        assert_eq!(got.len(), 1, "the newer meter wins, and its already-reset 5h window is dropped: {got:?}");
        assert_eq!(got[0]["label"], "week");
        assert!(claude_windows_anywhere(&[d.join("missing")], 1).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn codex_gets_antiqa_beside_antiloki_only_when_the_pane_has_it() {
        let q = json!({ "command": "/opt/antiqa/antiqa", "args": ["mcp", "--out", "/w/qa"], "env": { "ANTIQA_ORIGIN": "antiloki:pane-7" } });
        let base = json!({ "ok": true, "live": true, "items": [], "mcp_server": { "command": "/opt/antiloki-agent", "args": ["mcp"], "env": { "ANTILOKI_PANE_ID": "7" } } });
        let without = context_args("codex", &base);
        assert!(!without.iter().any(|a| a.contains("antiqa")), "no button, no flags: {without:?}");
        let mut with_pack = base.clone(); with_pack["antiqa_server"] = q;
        let x = context_args("codex", &with_pack);
        assert_eq!(&x[..without.len()], &without[..], "antiloki's own flags come first and are unchanged");
        for want in ["mcp_servers.antiqa.command=\"/opt/antiqa/antiqa\"", "mcp_servers.antiqa.args=[\"mcp\",\"--out\",\"/w/qa\"]", "mcp_servers.antiqa.env.ANTIQA_ORIGIN=\"antiloki:pane-7\"", "mcp_servers.antiqa.default_tools_approval_mode=\"approve\""] {
            assert!(x.iter().any(|a| a == want), "{want} in {x:?}");
        }
    }
}
