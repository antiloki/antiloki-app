//! ROUTINES (RFC 0025 §7, operator 2026-11-26: "unattended work only via routines"): a brief a specialist is given at a set time or every so often, with a budget for one run, the files it may change and the websites it may
//! read. A routine is OFF until the person turns it on; an agent may only propose one. It runs only while antiloki is open and says how many runs it missed; it never starts on top of its previous run; one routine
//! runs at a time; it waits while the seat's limit is near; the call is made like a group member's (`specialists::ask_with`): no shell, no tools that send, buy, delete or publish, and a hard budget.
//!
//!   the screen: GET /api/v1/cockpit/jobs · POST {op: save|on|off|run|remove, ...}     an agent: antiloki_routine {op: list|propose|withdraw}
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

const MIN_EVERY: u64 = 15;
const KEPT_RUNS: usize = 10;
const MAX_ROUTINES: usize = 30;
/// Due this much later than planned: the machine was off or antiloki was closed.
const LATE_MS: i64 = 5 * 60_000;
const WAIT_MS: i64 = 15 * 60_000;
const NEAR_FULL_PCT: f64 = 90.0;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Run { pub t_ms: i64, pub ms: i64, pub usd: f64, pub tokens: u64, /** done | failed | waited | missed | skipped */ pub status: String, pub text: String }
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Routine { pub id: u64, pub name: String, pub root: String, pub who: String, pub brief: String, /** minutes between runs; 0 when it runs daily at `at` */ pub every_min: u64, /** "HH:MM", the person's own time */ pub at: String, pub tz_min: i64, pub budget_usd: f64, pub write: Vec<String>, pub sites: Vec<String>, pub on: bool, /** "you" or "pane-N" */ pub by: String, pub next_ms: i64, pub missed: u64, pub runs: Vec<Run> }
#[derive(Serialize, Deserialize, Default)]
pub struct Store { pub next: u64, pub list: Vec<Routine> }

fn file() -> PathBuf {
    #[cfg(test)] { return std::env::temp_dir().join(format!("antiloki-jobs-test-{}", std::process::id())).join("jobs.json"); }
    #[allow(unreachable_code)]
    std::env::var_os("ANTILOKI_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".antiloki"))).unwrap_or_else(std::env::temp_dir).join("jobs.json")
}
fn load() -> Store { std::fs::read_to_string(file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default() }
/// Read, change, write, one at a time. Blocking.
fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    static LOCK: Mutex<()> = Mutex::new(()); let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut s = load(); let r = f(&mut s);
    let p = file(); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    if let Ok(t) = serde_json::to_string(&s) { let tmp = p.with_extension("tmp"); if std::fs::write(&tmp, t).is_ok() { let _ = std::fs::rename(tmp, p); } }
    r
}
fn running() -> std::sync::MutexGuard<'static, HashSet<u64>> { static R: std::sync::OnceLock<Mutex<HashSet<u64>>> = std::sync::OnceLock::new(); R.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner()) }

/// "HH:MM" as milliseconds into a day. Pure.
pub fn clock(at: &str) -> Option<i64> { let (h, m) = at.trim().split_once(':')?; let (h, m): (i64, i64) = (h.parse().ok()?, m.parse().ok()?); ((0..24).contains(&h) && (0..60).contains(&m)).then(|| (h * 3600 + m * 60) * 1000) }
/// The first time after `now`: `every_min` on, or the next `at` in the person's own time. Pure.
pub fn next_time(now_ms: i64, every_min: u64, at: &str, tz_min: i64) -> i64 {
    match clock(at) {
        Some(t) => { let local = now_ms + tz_min * 60_000; let day = local - local.rem_euclid(86_400_000); let mut n = day + t - tz_min * 60_000; if n <= now_ms { n += 86_400_000; } n }
        None => now_ms + every_min.max(MIN_EVERY) as i64 * 60_000,
    }
}
/// How many planned runs went by unrun, when one due at `due` is looked at only at `now`. Pure.
pub fn missed(due: i64, now_ms: i64, every_min: u64, at: &str) -> u64 { let late = now_ms - due; if late <= LATE_MS { return 0; } let period = if clock(at).is_some() { 86_400_000 } else { every_min.max(MIN_EVERY) as i64 * 60_000 }; 1 + (late / period) as u64 }
fn note(r: &mut Routine, status: &str, text: &str, now: i64) {
    // the same wait said again is one line, not a page of them
    if r.runs.last().is_some_and(|l| l.status == status && l.text == text && status != "done") { return; }
    r.runs.push(Run { t_ms: now, status: status.into(), text: text.chars().take(6_000).collect(), ..Default::default() }); if r.runs.len() > KEPT_RUNS { let cut = r.runs.len() - KEPT_RUNS; r.runs.drain(0..cut); }
}

/// What the person (or an agent proposing) sent, checked. `who` is checked by the caller against the specialists. Pure.
pub fn read_def(b: &Value, root: &str, by: &str) -> Result<Routine, String> {
    let s = |k: &str| b[k].as_str().unwrap_or("").trim().to_string(); let words = |k: &str| -> Vec<String> { b[k].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(|x| x.trim().to_string())).filter(|x| !x.is_empty()).collect() };
    let brief = s("brief"); if brief.chars().count() < 10 { return Err("say what the routine does".into()); }
    let who = s("who"); if who.is_empty() { return Err("which specialist does it?".into()); }
    let at = s("at"); if !at.is_empty() && clock(&at).is_none() { return Err("the time of day is like 09:30".into()); }
    let every = b["every_min"].as_u64().unwrap_or(0); if at.is_empty() && every < MIN_EVERY { return Err(format!("give a time of day, or an interval of at least {MIN_EVERY} minutes")); }
    let budget = b["budget_usd"].as_f64().unwrap_or(0.5); if !(0.05..=5.0).contains(&budget) { return Err("the budget of one run is between $0.05 and $5".into()); }
    let write = words("write").iter().map(|g| crate::groups::glob_ok(g)).collect::<Result<Vec<_>, _>>()?;
    let sites = words("sites").iter().map(|g| crate::groups::site_ok(g)).collect::<Result<Vec<_>, _>>()?; if write.len() > 20 || sites.len() > 5 { return Err("at most 20 file patterns and 5 websites".into()); }
    let name = Some(s("name")).filter(|n| !n.is_empty()).unwrap_or_else(|| brief.lines().next().unwrap_or("routine").to_string());
    Ok(Routine { id: b["id"].as_u64().unwrap_or(0), name: name.chars().take(60).collect(), root: root.into(), who, brief: brief.chars().take(4_000).collect(), every_min: if at.is_empty() { every.min(43_200) } else { 0 }, at, tz_min: b["tz_min"].as_i64().unwrap_or(0).clamp(-840, 840), budget_usd: budget, write, sites, by: by.into(), ..Default::default() })
}
/// Keep a routine. From the screen it keeps the history of the one it replaces and is on or off as the person left it; a proposal is new, and OFF. Blocking.
pub fn save(mut r: Routine, now: i64) -> Result<u64, String> {
    with(|s| {
        if r.id > 0 { let old = s.list.iter_mut().find(|x| x.id == r.id && x.root == r.root).ok_or("that routine is gone")?; r.on = old.on; r.runs = old.runs.clone(); r.missed = old.missed; r.by = old.by.clone(); r.next_ms = next_time(now, r.every_min, &r.at, r.tz_min); *old = r.clone(); return Ok(r.id); }
        if s.list.len() >= MAX_ROUTINES { return Err(format!("at most {MAX_ROUTINES} routines: remove one first")); }
        s.next += 1; r.id = s.next; r.on = false; r.next_ms = 0; s.list.push(r.clone()); Ok(r.id)
    })
}
/// On, off or removed. Turning on plans the next run; nothing runs at once. Blocking.
pub fn switch(id: u64, op: &str, now: i64) -> Result<(), String> {
    with(|s| { let i = s.list.iter().position(|r| r.id == id).ok_or("that routine is gone")?;
        match op { "on" => { let r = &mut s.list[i]; r.on = true; r.next_ms = next_time(now, r.every_min, &r.at, r.tz_min); } "off" => s.list[i].on = false, "remove" => { s.list.remove(i); } _ => return Err("op is on, off or remove".into()) } Ok(()) })
}
pub fn view(root: &str) -> Value { let busy: Vec<u64> = running().iter().copied().collect(); json!({ "ok": true, "routines": load().list.into_iter().filter(|r| r.root == root).collect::<Vec<_>>(), "running": busy, "now": crate::cli::now_ms() }) }

/// Why the seat cannot take an unattended run now, if so.
fn seat_wait(root: &str, who: &str) -> Option<String> {
    let engine = crate::specialists::get(root, who).and_then(|s| crate::cli::engine_of(&json!(s.seat))).unwrap_or(crate::cli::Engine::ClaudeCode);
    if let Some((_, why)) = crate::cli::seat_limit(engine) { return Some(why); }
    let house = crate::fuel::house_of(engine.key());
    crate::agents_http::account_windows(root).iter().find(|w| w["house"] == json!(house) && w["used_pct"].as_f64().is_some_and(|p| p >= NEAR_FULL_PCT)).map(|w| format!("the {} {} window is {:.0}% used", house, w["label"].as_str().unwrap_or(""), w["used_pct"].as_f64().unwrap_or(0.0)))
}
/// One run, start to end. The caller put the id in `running`.
async fn run(id: u64) {
    struct Free(u64); impl Drop for Free { fn drop(&mut self) { running().remove(&self.0); } }
    let _free = Free(id);
    static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = ONE.lock().await;
    let Some(r) = load().list.into_iter().find(|r| r.id == id) else { return };
    let (root, who) = (r.root.clone(), r.who.clone());
    if let Some(why) = tokio::task::spawn_blocking(move || seat_wait(&root, &who)).await.ok().flatten() {
        let _ = tokio::task::spawn_blocking(move || with(|s| { if let Some(x) = s.list.iter_mut().find(|x| x.id == id) { let now = crate::cli::now_ms(); x.next_ms = now + WAIT_MS; note(x, "waited", &format!("waiting: {why}"), now); } })).await; return;
    }
    let t0 = crate::cli::now_ms();
    let extra = crate::specialists::Extra { write: r.write.clone(), sites: r.sites.clone(), work: r.root.clone(), others: vec![], budget_usd: Some(r.budget_usd), long: true };
    let brief = format!("This is the scheduled routine \"{}\": nobody is watching. Do it now, then end with a short report: what you found or changed, and what needs a yes from the person (or \"nothing\"). Never send, buy, delete or publish anything.\n\n{}", r.name, r.brief);
    let said = crate::specialists::ask_with(&r.root, &format!("routine #{id}"), &r.who, &brief, extra).await;
    let _ = tokio::task::spawn_blocking(move || with(|s| {
        let Some(x) = s.list.iter_mut().find(|x| x.id == id) else { return }; let now = crate::cli::now_ms();
        let line = match said { Ok(a) => Run { t_ms: t0, ms: now - t0, usd: a.usd, tokens: a.tokens_in + a.tokens_out, status: "done".into(), text: a.text.chars().take(6_000).collect() }, Err(e) => Run { t_ms: t0, ms: now - t0, status: "failed".into(), text: e.chars().take(600).collect(), ..Default::default() } };
        x.runs.push(line); if x.runs.len() > KEPT_RUNS { let cut = x.runs.len() - KEPT_RUNS; x.runs.drain(0..cut); }
    })).await;
}
/// What is due, and the bookkeeping around it. Returns the ids to start. Pure over the store, given which are running.
pub fn due(s: &mut Store, now: i64, busy: &HashSet<u64>) -> Vec<u64> {
    let mut start = vec![];
    for r in s.list.iter_mut().filter(|r| r.on && r.next_ms > 0 && r.next_ms <= now) {
        let n = missed(r.next_ms, now, r.every_min, &r.at); r.next_ms = next_time(now, r.every_min, &r.at, r.tz_min);
        if busy.contains(&r.id) { note(r, "skipped", "the previous run was still going", now); continue; }
        if n > 0 { r.missed += n; note(r, "missed", &format!("antiloki was closed or the machine off: {n} run(s) missed; running once now"), now); }
        start.push(r.id);
    }
    start
}
fn begin(id: u64) -> bool { if !running().insert(id) { return false; } tokio::spawn(run(id)); true }
/// The ticker: twice a minute.
pub fn start() {
    tokio::spawn(async move { loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        if load().list.iter().all(|r| !r.on) { continue; }
        let busy: HashSet<u64> = running().clone();
        let ids = tokio::task::spawn_blocking(move || with(|s| due(s, crate::cli::now_ms(), &busy))).await.unwrap_or_default();
        for id in ids { begin(id); }
    } });
}
/// The person's clicks.
pub async fn set(root: &str, b: &Value) -> Value {
    let (root, b) = (root.to_string(), b.clone()); let op = b["op"].as_str().unwrap_or("").to_string(); let id = b["id"].as_u64().unwrap_or(0);
    if op == "run" { if !load().list.iter().any(|r| r.id == id && r.root == root) { return json!({ "ok": false, "error": "that routine is gone" }); } return if begin(id) { json!({ "ok": true }) } else { json!({ "ok": false, "error": "it is running already" }) }; }
    tokio::task::spawn_blocking(move || {
        let now = crate::cli::now_ms();
        let done = match op.as_str() {
            "save" => read_def(&b, &root, "you").and_then(|r| { let sp = crate::specialists::get(&root, &r.who).ok_or_else(|| format!("no specialist `{}`", r.who))?; if !crate::specialists::usable(&sp) { return Err(format!("`{}` is a draft: approve it first", sp.name)); } if crate::cli::engine_of(&json!(sp.seat)).is_some_and(|e| e != crate::cli::Engine::ClaudeCode) { return Err("a routine's budget holds only on Claude Code today: pick a Claude specialist".into()); } save(r, now).map(|id| json!({ "ok": true, "id": id })) }),
            "on" | "off" | "remove" => if load().list.iter().any(|r| r.id == id && r.root == root) { switch(id, &op, now).map(|_| json!({ "ok": true })) } else { Err("that routine is gone".into()) },
            _ => Err("op is save, on, off, run or remove".into()),
        };
        done.unwrap_or_else(|e| json!({ "ok": false, "error": e }))
    }).await.unwrap_or_else(|_| json!({ "ok": false, "error": "cut off" }))
}
/// The agent's tool: it reads, proposes (always off) and withdraws its own proposal. It never turns one on or changes one.
pub fn run_tool(pane_id: i64, root: &str, args: &Value) -> Result<String, String> {
    let me = format!("pane-{pane_id}");
    match args["op"].as_str().unwrap_or("list") {
        "list" => { let l: Vec<String> = load().list.iter().filter(|r| r.root == root).map(|r| format!("#{} {} ({}, {}, ${:.2} a run, {}){}", r.id, r.name, r.who, if r.at.is_empty() { format!("every {} min", r.every_min) } else { format!("daily at {}", r.at) }, r.budget_usd, if r.on { "ON" } else { "off" }, r.runs.last().map(|l| format!(" last: {}", l.status)).unwrap_or_default())).collect(); Ok(if l.is_empty() { "no routines in this workspace".into() } else { l.join("\n") }) }
        "propose" => { if pane_id <= 0 { return Err("only an agent in a pane proposes a routine".into()); } let mut b = args.clone(); if let Some(o) = b.as_object_mut() { o.remove("id"); }
            let r = read_def(&b, root, &me)?; crate::specialists::get(root, &r.who).ok_or_else(|| format!("no specialist `{}` (antiloki_remember op `list`)", r.who))?; let id = save(r, crate::cli::now_ms())?;
            Ok(format!("proposed as routine #{id}, OFF. It runs only after the person reads it and turns it on (the Board pane, Routines). Tell them; do not ask them to trust it unread.")) }
        "withdraw" => { let id = args["id"].as_u64().unwrap_or(0); if !load().list.iter().any(|r| r.id == id && r.root == root && r.by == me && !r.on) { return Err("you can withdraw only a routine you proposed that is still off".into()); } switch(id, "remove", 0)?; Ok(format!("routine #{id} withdrawn")) }
        _ => Err("op is list, propose or withdraw".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const H: i64 = 3_600_000;
    #[test]
    fn the_next_run_is_the_persons_own_time_of_day_or_the_interval() {
        let day = 20_000 * 86_400_000i64;
        assert_eq!(next_time(day + 8 * H, 0, "09:30", 0), day + 9 * H + 30 * 60_000); assert_eq!(next_time(day + 10 * H, 0, "09:30", 0), day + 86_400_000 + 9 * H + 30 * 60_000);
        assert_eq!(next_time(day + 10 * H, 0, "09:00", -180), day + 12 * H, "09:00 at UTC-3 is 12:00 UTC");
        assert_eq!(next_time(1_000, 60, "", 0), 1_000 + H); assert_eq!(next_time(1_000, 1, "", 0), 1_000 + 15 * 60_000, "never more often than allowed");
        assert!(clock("24:00").is_none() && clock("9").is_none() && clock("09:05").is_some());
    }
    #[test]
    fn a_late_look_counts_what_was_missed_and_a_running_one_is_not_started_again() {
        assert_eq!(missed(1_000, 1_000 + LATE_MS, 60, ""), 0); assert_eq!(missed(0, 3 * H + 60_000, 60, ""), 4); assert_eq!(missed(0, 2 * H, 0, "09:00"), 1);
        let r = |id: u64, on: bool, next: i64| Routine { id, on, next_ms: next, every_min: 60, ..Default::default() };
        let mut s = Store { next: 3, list: vec![r(1, true, 10), r(2, false, 10), r(3, true, 10)] };
        assert_eq!(due(&mut s, 20, &HashSet::from([3])), vec![1]); assert_eq!(s.list[2].runs[0].status, "skipped"); assert!(s.list[0].next_ms > 20); assert_eq!(s.list[1].next_ms, 10, "an off one is left alone");
        s.list[0].next_ms = 10; s.list[2].on = false; assert_eq!(due(&mut s, 10 + 5 * H, &HashSet::new()), vec![1]); assert!(s.list[0].missed >= 5 && s.list[0].runs.last().unwrap().status == "missed");
    }
    #[test]
    fn a_routine_is_checked_is_born_off_and_only_the_person_turns_it_on() {
        let _ = std::fs::remove_file(file());   // the only test of this module that touches the file
        let good = json!({ "who": "cc", "brief": "read the changelog and report", "every_min": 60 });
        for (k, v) in [("brief", json!("x")), ("every_min", json!(5)), ("budget_usd", json!(50)), ("write", json!(["**"])), ("sites", json!(["localhost"])), ("at", json!("25:00"))] { let mut b = good.clone(); b[k] = v; assert!(read_def(&b, "/p", "you").is_err(), "{k}"); }
        let mut asked = good.clone(); asked["on"] = json!(true);
        let id = save(read_def(&asked, "/p", "pane-4").unwrap(), 1_000).unwrap(); let got = load().list[0].clone(); assert!(!got.on && got.next_ms == 0 && got.budget_usd == 0.5, "a proposal never starts itself: {got:?}");
        assert!(run_tool(5, "/p", &json!({ "op": "withdraw", "id": id })).is_err(), "not another agent's"); switch(id, "on", 2_000).unwrap(); assert_eq!(load().list[0].next_ms, 2_000 + H); assert!(run_tool(4, "/p", &json!({ "op": "withdraw", "id": id })).is_err(), "not one the person turned on");
        assert!(run_tool(4, "/p", &json!({ "op": "on", "id": id })).is_err()); let _ = std::fs::remove_file(file());
    }
}
