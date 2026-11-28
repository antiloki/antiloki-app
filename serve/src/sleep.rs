//! IDLE AGENTS SLEEP (operator 2026-11-16): an agent that has been idle for N minutes (30 by default) writes its saga, and only when the saga is confirmed saved does its terminal close; a click on its avatar wakes it and
//! it reads the saga again. Never put to sleep: a pane that is working or waiting for the person, a team's Leader, a plain shell (it may be running a command). Without this every terminal ever opened stays a ~400 MB process.
//!
//!   GET  /api/v1/cockpit/avatars      one row per terminal: pane, name, role, state (work|wait|idle|sleep|ended), idle minutes, plus the sleep setting and the host's counts
//!   POST /api/v1/cockpit/sleep        {enabled?, minutes?} the setting
//!   POST /api/v1/cockpit/sleep/now    {pane_id} put one to sleep now (same safeguards)
//!   POST /api/v1/cockpit/sleep/wake   {pane_id} clear the sleep; opening the pane starts a new terminal that resumes
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use crate::dispatch::{engine_call, AppState};

#[derive(Clone, Debug, PartialEq)]
pub struct Row { pub pane_id: i64, pub name: String, pub role: String, pub kind: String, pub state: String, pub busy_ms: i64, pub root: String }
struct Mem { rows: HashMap<i64, Row>, asleep: HashMap<i64, i64>, enabled: bool, minutes: i64, loaded: bool }
fn mem() -> std::sync::MutexGuard<'static, Mem> {
    static M: OnceLock<Mutex<Mem>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(Mem { rows: HashMap::new(), asleep: HashMap::new(), enabled: true, minutes: 30, loaded: false })).lock().unwrap_or_else(|p| p.into_inner())
}
fn settings_path() -> std::path::PathBuf { crate::goals::Goals::default_history_path().with_file_name("sleep.json") }
fn load(m: &mut Mem) {
    if m.loaded { return; } m.loaded = true;
    if let Some(v) = std::fs::read_to_string(settings_path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
        if let Some(b) = v["enabled"].as_bool() { m.enabled = b; } if let Some(n) = v["minutes"].as_i64() { m.minutes = n.clamp(1, 24 * 60); }
        if let Some(a) = v["asleep"].as_object() { for (k, t) in a { if let (Ok(p), Some(t)) = (k.parse::<i64>(), t.as_i64()) { m.asleep.insert(p, t); } } }
    }
}
fn save(m: &Mem) { let a: serde_json::Map<String, Value> = m.asleep.iter().map(|(k, v)| (k.to_string(), json!(v))).collect(); let _ = std::fs::write(settings_path(), json!({ "enabled": m.enabled, "minutes": m.minutes, "asleep": a }).to_string()); }

/// Has the pane been quiet for the whole window? Pure.
pub fn due(busy_ms: i64, now_ms: i64, minutes: i64) -> bool { minutes > 0 && now_ms - busy_ms >= minutes * 60_000 }
/// Never put to sleep: working, waiting for the person, a Leader, a shell. Pure.
pub fn spares(kind: &str, working: bool, blocked: bool, leader: bool) -> bool { working || blocked || leader || kind == "shell" || kind.is_empty() }
/// The avatar's state from what the sweep read. Pure.
pub fn state_of(alive: bool, asleep: bool, working: bool, blocked: bool) -> &'static str { if asleep { "sleep" } else if !alive { "ended" } else if blocked { "wait" } else if working { "work" } else { "idle" } }

pub async fn tick(st: &Arc<AppState>, goals: &Arc<crate::goals::Goals>) {
    let now = crate::cli::now_ms();
    let terms = tokio::task::spawn_blocking(crate::pty_remote::list_running).await.ok().and_then(|r| r.ok()).unwrap_or_default();
    let live: Vec<i64> = terms.iter().filter_map(|t| crate::pty_remote::pane_of(&t.id)).collect();
    let (enabled, minutes) = { let mut m = mem(); load(&mut m); (m.enabled, m.minutes) };
    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
    let mut seen: Vec<Row> = vec![];
    for pane_id in live {
        let pane = engine_call(st, "system.cockpit.pane.get", json!({ "id": pane_id }), ctx.clone()).await.map(|(v, _)| v).unwrap_or(Value::Null);
        let root = crate::cowork::pane_facts(st, pane_id).await.0;
        let kind = pane["pane_kind"].as_str().unwrap_or("").to_string(); let name = pane["title"].as_str().or(pane["name"].as_str()).unwrap_or("").to_string();
        let (r2, k2) = (root.clone(), kind.clone());
        let rows = tokio::task::spawn_blocking(move || crate::pty_remote::screen_rows(pane_id)).await.ok().flatten();
        let u = tokio::task::spawn_blocking(move || crate::cowork::usage_for_pane(Path::new(&r2), pane_id, &k2, rows.as_deref())).await.unwrap_or(Value::Null);
        let (working, blocked) = (u["working"] == true, !u["blocked"].is_null());
        let r3 = root.clone(); let members = tokio::task::spawn_blocking(move || crate::team_member::member_of(&r3, pane_id)).await.unwrap_or_default();
        let leader = members.iter().any(|m| m["kind"] == "leader"); let role = members.first().and_then(|m| m["role"].as_str()).unwrap_or("").to_string();
        let busy = { let mut m = mem(); let e = m.rows.get(&pane_id).map(|r| r.busy_ms).unwrap_or(now); let b = if working || blocked || leader || kind == "shell" { now } else { e }; b };
        seen.push(Row { pane_id, name, role, kind: kind.clone(), state: state_of(true, false, working, blocked).into(), busy_ms: busy, root: root.clone() });
        if enabled && !spares(&kind, working, blocked, leader) && due(busy, now, minutes) { let _ = put_to_sleep(st, goals, pane_id, &root).await; }
    }
    let mut m = mem(); m.rows = seen.into_iter().map(|r| (r.pane_id, r)).collect();
}

/// Save the saga, check it is there, then close. Err says why it stayed awake.
pub async fn put_to_sleep(st: &Arc<AppState>, goals: &Arc<crate::goals::Goals>, pane_id: i64, root: &str) -> Result<(), String> {
    let (s, g, r) = (st.clone(), goals.clone(), root.to_string());
    let saved = tokio::task::spawn_blocking(move || crate::work::consolidate(&s, &g, &r, pane_id)).await.map_err(|e| e.to_string())??;
    if !saved {
        // no saga row yet: stay awake and look again in ten minutes rather than every sweep
        let mut m = mem(); let back = (m.minutes - 10).max(0) * 60_000; if let Some(r) = m.rows.get_mut(&pane_id) { r.busy_ms = crate::cli::now_ms() - back; } return Err("the saga is not saved yet".into());
    }
    tokio::task::spawn_blocking(move || crate::pty_remote::kill(pane_id)).await.map_err(|e| e.to_string())?;
    tokio::spawn(crate::specialists::distill_pane(st.clone(), pane_id));
    let mut m = mem(); m.asleep.insert(pane_id, crate::cli::now_ms()); if let Some(r) = m.rows.get_mut(&pane_id) { r.state = "sleep".into(); } save(&m); Ok(())
}

pub fn view() -> Value {
    let mut m = mem(); load(&mut m); let now = crate::cli::now_ms();
    let mut rows: Vec<Value> = m.rows.values().map(|r| json!({ "pane_id": r.pane_id, "name": r.name, "role": r.role, "kind": r.kind, "state": r.state, "idle_min": if r.state == "idle" { (now - r.busy_ms) / 60_000 } else { 0 } })).collect();
    for (p, t) in &m.asleep { if !m.rows.contains_key(p) { rows.push(json!({ "pane_id": p, "name": "", "role": "", "kind": "", "state": "sleep", "asleep_since": t })); } }
    rows.sort_by_key(|r| r["pane_id"].as_i64().unwrap_or(0));
    let c = |s: &str| rows.iter().filter(|r| r["state"] == s).count();
    json!({ "ok": true, "enabled": m.enabled, "minutes": m.minutes, "agents": rows, "live": c("work") + c("wait") + c("idle"), "asleep": c("sleep") })
}
pub fn set(b: &Value) -> Value { let mut m = mem(); load(&mut m); if let Some(e) = b["enabled"].as_bool() { m.enabled = e; } if let Some(n) = b["minutes"].as_i64() { m.minutes = n.clamp(1, 24 * 60); } save(&m); json!({ "ok": true, "enabled": m.enabled, "minutes": m.minutes }) }
pub fn wake(pane_id: i64) -> Value { let mut m = mem(); load(&mut m); let was = m.asleep.remove(&pane_id).is_some(); if let Some(r) = m.rows.get_mut(&pane_id) { r.busy_ms = crate::cli::now_ms(); } save(&m); json!({ "ok": true, "was_asleep": was, "pane_id": pane_id }) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn only_a_quiet_enough_pane_is_due() { assert!(!due(0, 29 * 60_000, 30)); assert!(due(0, 30 * 60_000, 30)); assert!(!due(0, 10_000_000, 0), "0 minutes = off"); }
    #[test] fn the_four_never_sleep_cases_are_spared() {
        assert!(spares("claude", true, false, false) && spares("claude", false, true, false) && spares("claude", false, false, true) && spares("shell", false, false, false));
        assert!(!spares("claude", false, false, false));
    }
    #[test] fn the_avatar_state_follows_the_reading() { assert_eq!(state_of(true, false, true, false), "work"); assert_eq!(state_of(true, false, true, true), "wait"); assert_eq!(state_of(true, true, false, false), "sleep"); assert_eq!(state_of(false, false, false, false), "ended"); assert_eq!(state_of(true, false, false, false), "idle"); }
}
