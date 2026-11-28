//! PAUSE AND CONTINUE FOR AGENTS AND SPECIALISTS (operator 2026-11-26: "a watcher to be able to pause and continue based on ram available, or vram available ... reuse that system"; "should be automatically to prevent
//! crashes, not just on click"). It reads the RAM and VRAM the machine window shows, the free disk (a full disk crashed Chromium and the linker) and the processor load, every few seconds.
//!   DANGER   (RAM or VRAM 92 % used, under 6 GB of disk free, load over twice the cores): ONE working agent is paused, the one holding the most memory, never the only one working. VRAM only pauses local models.
//!   CRITICAL (RAM 96 % used, under 3 GB of disk free): the machine is about to fail, so working agents are paused one per look, the only one too.
//! They continue one at a time when it is calm again, or when nothing else is working and nothing is in danger. What a person paused only a person continues. A pause stops the agent's processes (they keep their
//! memory, they stop growing and writing); nothing is lost. Paused processes are written to a file, so a restarted serve continues what the old one left stopped.
//!
//!   GET  /api/v1/cockpit/watch                      the RAM and VRAM shares and the paused agents
//!   POST /api/v1/cockpit/watch {pane_id, op}        op: pause | resume
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use crate::agent::AgentRegistry;
use crate::dispatch::{engine_call, AppState};

const EVERY_S: u64 = 4;
/// After a person continues an agent the watcher leaves it alone this long.
const LEFT_ALONE_MS: i64 = 120_000;

#[derive(Clone, Debug, PartialEq)]
pub struct Agent { pub pane_id: i64, pub rss: u64, pub working: bool, pub local: bool }
#[derive(Clone, Debug, PartialEq)]
pub struct Paused { pub pane_id: i64, pub by_person: bool, pub why: String, pub pids: Vec<i32>, pub since_ms: i64 }

#[derive(Default)]
struct Mem { paused: HashMap<i64, Paused>, left_alone: HashMap<i64, i64> }
fn mem() -> std::sync::MutexGuard<'static, Mem> { static M: OnceLock<Mutex<Mem>> = OnceLock::new(); M.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner()) }

/// What the machine has left. `ram` and `vram` are used shares in percent, `disk_gb` the free space where the work is, `load` the 1-minute load divided by the cores. A reading that cannot be taken is None.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Room { pub ram: Option<f64>, pub vram: Option<f64>, pub disk_gb: Option<f64>, pub load: Option<f64> }
const DISK_DANGER_GB: f64 = 6.0;
const DISK_CRITICAL_GB: f64 = 3.0;
const DISK_BACK_GB: f64 = 10.0;
const RAM_CRITICAL: f64 = 96.0;
const LOAD_DANGER: f64 = 2.0;
const LOAD_BACK: f64 = 1.0;
fn full(p: Option<f64>) -> bool { p.is_some_and(|p| crate::machine::tone(p) == "danger") }
/// Why the machine is about to fail, if it is. Pure.
pub fn critical(r: &Room) -> Option<String> {
    if let Some(d) = r.disk_gb.filter(|d| *d < DISK_CRITICAL_GB) { return Some(format!("only {d:.1} GB of disk is free")); }
    r.ram.filter(|p| *p >= RAM_CRITICAL).map(|p| format!("RAM is {p:.0} % used"))
}
/// Why it is in danger, and whether only agents on local models are the cure. Pure.
pub fn dangers(r: &Room) -> Vec<(String, bool)> {
    let mut v = vec![];
    if full(r.ram) { v.push((format!("RAM is {:.0} % used", r.ram.unwrap_or(0.0)), false)); }
    if let Some(d) = r.disk_gb.filter(|d| *d < DISK_DANGER_GB) { v.push((format!("only {d:.1} GB of disk is free"), false)); }
    if let Some(l) = r.load.filter(|l| *l > LOAD_DANGER) { v.push((format!("the processor is over-booked ({l:.1} times its cores)"), false)); }
    if full(r.vram) { v.push((format!("VRAM is {:.0} % used", r.vram.unwrap_or(0.0)), true)); }
    v
}
#[derive(Debug, PartialEq)]
pub enum Step { Pause(i64, String), Resume(i64), Hold }
/// One look, at most one switch. `paused` holds the agents the WATCHER stopped (oldest first), never the person's. Pure.
pub fn step(room: &Room, agents: &[Agent], paused: &[i64], stopped: &HashSet<i64>) -> Step {
    let running: Vec<&Agent> = agents.iter().filter(|a| a.working && !stopped.contains(&a.pane_id)).collect();
    if let Some(why) = critical(room) { if let Some(a) = running.iter().max_by_key(|a| a.rss) { return Step::Pause(a.pane_id, why) } }
    let dangers = dangers(room);
    for (why, only_local) in &dangers {
        let who: Vec<&&Agent> = running.iter().filter(|a| !only_local || a.local).collect();
        // short of critical, pausing the only one working stops all the work for little gain
        if who.len() >= 2 { if let Some(a) = who.iter().max_by_key(|a| a.rss) { return Step::Pause(a.pane_id, why.clone()) } }
    }
    let Some(&first) = paused.first() else { return Step::Hold };
    let calm = [room.ram, room.vram].into_iter().flatten().all(|p| crate::machine::tone(p) == "calm") && room.disk_gb.is_none_or(|d| d >= DISK_BACK_GB) && room.load.is_none_or(|l| l < LOAD_BACK);
    // a paused agent keeps its memory, so "calm" may never come: when nothing else is working and nothing is in danger, it goes on
    if critical(room).is_none() && (calm || (running.is_empty() && dangers.is_empty())) { Step::Resume(first) } else { Step::Hold }
}
/// Free space in GB from `df -Pk`'s answer. Pure.
pub fn free_gb(df: &str) -> Option<f64> { Some(df.lines().nth(1)?.split_whitespace().nth(3)?.parse::<f64>().ok()? / 1_048_576.0) }
fn room() -> Room {
    let snap = crate::machine::snapshot();
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let disk_gb = crate::proc::cmd("df").args(["-Pk", &home]).output().ok().filter(|o| o.status.success()).and_then(|o| free_gb(&String::from_utf8_lossy(&o.stdout)));
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).max(1) as f64;
    Room { ram: snap["ram"]["pct"].as_f64(), vram: snap["vram"]["pct"].as_f64(), disk_gb, load: crate::mem::load1().map(|l| l / cores) }
}

#[cfg(unix)]
fn signal_all(pids: &[i32], stop: bool) -> bool {
    let mut any = false;
    for &p in pids { if p > 1 { any |= if stop { crate::mem::pause_tree(p) } else { crate::mem::resume_tree(p) }; } }
    any
}
#[cfg(not(unix))]
fn signal_all(pids: &[i32], stop: bool) -> bool { let mut any = false; for &p in pids { any |= if stop { crate::mem::pause_tree(p) } else { crate::mem::resume_tree(p) }; } any }

fn file() -> Option<PathBuf> { std::env::var_os("ANTILOKI_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".antiloki"))).map(|d| d.join("paused.json")) }
fn save(m: &Mem) {
    if cfg!(test) { return }
    if let Some(f) = file() { let pids: Vec<i32> = m.paused.values().flat_map(|p| p.pids.clone()).collect(); let _ = std::fs::write(f, json!({ "pids": pids }).to_string()); }
}
/// What a serve that stopped left paused goes on.
fn continue_leftovers() {
    let Some(f) = file() else { return };
    let v: Value = std::fs::read_to_string(&f).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    let pids: Vec<i32> = v["pids"].as_array().into_iter().flatten().filter_map(|x| x.as_i64().map(|n| n as i32)).collect();
    if !pids.is_empty() { signal_all(&pids, false); let _ = std::fs::remove_file(f); }
}

/// The pane a process belongs to, from its environment (`ANTILOKI_PANE_ID`, NUL-separated as `/proc/<pid>/environ`). Pure.
pub fn pane_in_environ(environ: &[u8]) -> Option<i64> { environ.split(|b| *b == 0).find_map(|kv| std::str::from_utf8(kv).ok()?.strip_prefix("ANTILOKI_PANE_ID=")?.parse().ok()) }

/// A process's last argument against the terminal's: the host remembers `opencode`, the process says `/…/bin/opencode.exe`. Pure.
pub fn same_command(process: Option<&String>, terminal: Option<&String>) -> bool {
    let stem = |s: &String| Path::new(s).file_stem().map(|x| x.to_os_string());
    process.is_some() && (process == terminal || process.and_then(stem) == terminal.and_then(stem))
}

/// The root process of every pane terminal: a direct child of the terminal host (or of this serve) that says which pane it is, or, for a terminal started before panes said so, the only one with the pane's command in the pane's folder.
fn pane_roots(terms: &[(i64, Vec<String>, String)]) -> HashMap<i64, i32> {
    let parents = crate::mem::process_parents();
    let cmdline = |p: i32| std::fs::read(format!("/proc/{p}/cmdline")).map(|b| b.split(|c| *c == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).to_string()).collect::<Vec<_>>()).unwrap_or_default();
    let me = std::process::id() as i32;
    let hosts: HashSet<i32> = parents.iter().map(|&(p, _)| p).filter(|&p| cmdline(p).iter().any(|a| a == "pty-host")).chain([me]).collect();
    let kids: Vec<i32> = parents.iter().filter(|(_, pp)| hosts.contains(pp)).map(|&(p, _)| p).filter(|p| !hosts.contains(p)).collect();
    let mut out = HashMap::new(); let mut unnamed = vec![];
    for p in kids { match std::fs::read(format!("/proc/{p}/environ")).ok().and_then(|e| pane_in_environ(&e)) { Some(pane) => { out.insert(pane, p); } None => unnamed.push((p, cmdline(p), std::fs::read_link(format!("/proc/{p}/cwd")).unwrap_or_default())) } }
    for (pane, argv, cwd) in terms {
        if out.contains_key(pane) || cwd.is_empty() { continue }
        let same: Vec<i32> = unnamed.iter().filter(|(_, cmd, d)| d == Path::new(cwd) && same_command(cmd.last(), argv.last())).map(|x| x.0).collect();
        if same.len() == 1 { out.insert(*pane, same[0]); }
    }
    out
}

/// The working terminal agents and chat agents, with what each holds, and the pids to stop for each.
async fn look(st: &Arc<AppState>, chats: &Arc<AgentRegistry>) -> (Vec<Agent>, HashMap<i64, Vec<i32>>) {
    let terms = tokio::task::spawn_blocking(crate::pty_remote::list_running).await.ok().and_then(|r| r.ok()).unwrap_or_default();
    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
    let mut rows = vec![];
    for t in terms {
        let Some(pane_id) = crate::pty_remote::pane_of(&t.id) else { continue };
        let pane = engine_call(st, "system.cockpit.pane.get", json!({ "id": pane_id }), ctx.clone()).await.map(|(v, _)| v).unwrap_or(Value::Null);
        let kind = pane["pane_kind"].as_str().unwrap_or("").to_string();
        if kind.is_empty() || kind == "shell" { continue }
        let local = pane["settings_json"].as_str().map(crate::pane_model::model_of).is_some_and(|m| crate::local_panes::tag_of(&m).is_some());
        rows.push((pane_id, t.argv.clone(), crate::cowork::pane_facts(st, pane_id).await.0, kind, local));
    }
    let chat = chats.memory();
    let known: HashSet<i64> = { mem().paused.keys().copied().collect() };
    tokio::task::spawn_blocking(move || {
        let roots = pane_roots(&rows.iter().map(|r| (r.0, r.1.clone(), r.2.clone())).collect::<Vec<_>>());
        let parents = crate::mem::process_parents();
        let (mut agents, mut pids) = (vec![], HashMap::new());
        for (pane_id, _, cwd, kind, local) in rows {
            let Some(&root) = roots.get(&pane_id) else { continue };
            let working = known.contains(&pane_id) || crate::cowork::usage_for_pane(Path::new(&cwd), pane_id, &kind, crate::pty_remote::screen_rows(pane_id).as_deref())["working"] == true;
            agents.push(Agent { pane_id, rss: crate::mem::tree_rss(root, &parents), working, local });
            pids.insert(pane_id, crate::mem::tree_of(root, &parents));
        }
        for w in chat { if w.pid > 0 && !pids.contains_key(&w.pane_id) { agents.push(Agent { pane_id: w.pane_id, rss: w.rss, working: w.busy, local: false }); pids.insert(w.pane_id, crate::mem::tree_of(w.pid, &parents)); } }
        (agents, pids)
    }).await.unwrap_or_default()
}

fn pause(pane_id: i64, pids: Vec<i32>, by_person: bool, why: String) -> bool {
    if !signal_all(&pids, true) { return false }
    let mut m = mem(); m.paused.insert(pane_id, Paused { pane_id, by_person, why, pids, since_ms: crate::cli::now_ms() }); save(&m); true
}
/// Continue one pane if it is paused. Called too before a pane's terminal is closed: a stopped process cannot act on being told to end.
pub fn release(pane_id: i64) -> bool {
    let mut m = mem(); let Some(p) = m.paused.remove(&pane_id) else { return false }; signal_all(&p.pids, false); save(&m); true
}

async fn tick(st: &Arc<AppState>, chats: &Arc<AgentRegistry>) {
    let room = tokio::task::spawn_blocking(room).await.unwrap_or_default();
    if critical(&room).is_none() && dangers(&room).is_empty() && mem().paused.values().all(|p| p.by_person) { return }   // the common case reads only the machine
    let (agents, mut pids) = look(st, chats).await;
    let now = crate::cli::now_ms();
    let (auto, mut stopped, spared): (Vec<i64>, HashSet<i64>, HashSet<i64>) = {
        let mut m = mem();
        m.paused.retain(|_, p| p.pids.iter().any(|id| Path::new(&format!("/proc/{id}")).exists()) || cfg!(not(target_os = "linux")));   // its processes ended
        let mut a: Vec<&Paused> = m.paused.values().filter(|p| !p.by_person).collect(); a.sort_by_key(|p| p.since_ms);
        (a.iter().map(|p| p.pane_id).collect(), m.paused.keys().copied().collect(), m.left_alone.iter().filter(|(_, t)| now - **t < LEFT_ALONE_MS).map(|(id, _)| *id).collect())
    };
    stopped.extend(spared);
    // in a critical state a pane just continued by hand is not spared
    if critical(&room).is_some() { stopped = mem().paused.keys().copied().collect(); }
    match step(&room, &agents, &auto, &stopped) {
        Step::Pause(id, why) => { if let Some(p) = pids.remove(&id) { if pause(id, p, false, why) { chats.set_paused(id, true); } } }
        Step::Resume(id) => { if release(id) { chats.set_paused(id, false); } }
        Step::Hold => {}
    }
}

pub fn watch(st: Arc<AppState>, chats: Arc<AgentRegistry>) {
    continue_leftovers();
    tokio::spawn(async move { let mut last_long = std::time::Instant::now(); loop { tokio::time::sleep(std::time::Duration::from_secs(EVERY_S)).await; tick(&st, &chats).await; if last_long.elapsed().as_secs() >= 60 { last_long = std::time::Instant::now(); look_long(&st).await; } } });
}

/// A LOCAL MODEL THAT HAS BEEN WRITING FOR LONG IS SAID (found 2026-11-22: a tiny model held a processor at 320 % for forty minutes and nothing told the person). Minutes after which it is said.
pub const LONG_LOCAL_MIN: i64 = 15;
/// Minutes a run has been going, once it is long enough to say. `started` and `now` in ms. Pure.
pub fn long_run(working: bool, started: Option<i64>, now: i64) -> Option<i64> { let m = (now - started?) / 60_000; (working && m >= LONG_LOCAL_MIN).then_some(m) }
static LONG: std::sync::Mutex<Vec<(i64, i64, String)>> = std::sync::Mutex::new(Vec::new());
pub fn long_rows() -> Vec<Value> { LONG.lock().unwrap_or_else(|p| p.into_inner()).iter().map(|(p, m, model)| json!({ "pane_id": p, "minutes": m, "model": model })).collect() }
async fn look_long(st: &Arc<AppState>) {
    let terms = tokio::task::spawn_blocking(crate::pty_remote::list_running).await.ok().and_then(|r| r.ok()).unwrap_or_default();
    let mut found = vec![];
    for t in terms {
        let Some(pane_id) = crate::pty_remote::pane_of(&t.id) else { continue };
        let pane = engine_call(st, "system.cockpit.pane.get", json!({ "id": pane_id }), json!({ "actor": "web-fe", "source": "web-fe" })).await.map(|(v, _)| v).unwrap_or(Value::Null);
        let Some(tag) = pane["settings_json"].as_str().map(crate::pane_model::model_of).and_then(|m| crate::local_panes::tag_of(&m).map(String::from)) else { continue };
        let (cwd, kind) = (crate::cowork::pane_facts(st, pane_id).await.0, pane["pane_kind"].as_str().unwrap_or("").to_string());
        let u = tokio::task::spawn_blocking(move || crate::cowork::usage_for_pane(Path::new(&cwd), pane_id, &kind, crate::pty_remote::screen_rows(pane_id).as_deref())).await.unwrap_or(Value::Null);
        if let Some(m) = long_run(u["working"] == true, u["run"]["started_t"].as_str().and_then(crate::cowork::iso_to_ms), crate::cli::now_ms()) { found.push((pane_id, m, tag)); }
    }
    *LONG.lock().unwrap_or_else(|p| p.into_inner()) = found;
}

/// The paused agents, for the screens.
pub fn paused_rows() -> Vec<Value> { mem().paused.values().map(|p| json!({ "pane_id": p.pane_id, "by": if p.by_person { "person" } else { "watcher" }, "why": p.why, "since_ms": p.since_ms })).collect() }

pub async fn view() -> Value {
    let r = tokio::task::spawn_blocking(room).await.unwrap_or_default();
    let mail = tokio::task::spawn_blocking(crate::mailbox::waiting_rows).await.unwrap_or_default();
    json!({ "ok": true, "ram_pct": r.ram, "vram_pct": r.vram, "disk_free_gb": r.disk_gb, "load_per_core": r.load, "paused": paused_rows(), "long_local": long_rows(), "mail": mail, "browser_asks": crate::agent_browser::asks(), "browser_control": crate::agent_browser::controls(), "groups": crate::groups::summary() })
}
/// `{pane_id, op: pause|resume}`, a person's click.
pub async fn set(st: &Arc<AppState>, chats: &Arc<AgentRegistry>, b: &Value) -> Value {
    let id = b["pane_id"].as_i64().unwrap_or(0);
    match b["op"].as_str() {
        Some("resume") => { let was = release(id); if was { chats.set_paused(id, false); mem().left_alone.insert(id, crate::cli::now_ms()); } json!({ "ok": was, "error": if was { Value::Null } else { json!("that agent is not paused") } }) }
        Some("pause") => {
            if let Some(p) = mem().paused.get_mut(&id) { p.by_person = true; p.why = "paused by you".into(); return json!({ "ok": true }) }
            let (_, mut pids) = look(st, chats).await;
            let ok = pids.remove(&id).is_some_and(|p| pause(id, p, true, "paused by you".into()));
            if ok { chats.set_paused(id, true); }
            json!({ "ok": ok, "error": if ok { Value::Null } else { json!("no running agent in that pane") } })
        }
        _ => json!({ "ok": false, "error": "op is pause or resume" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn a(id: i64, rss: u64, working: bool, local: bool) -> Agent { Agent { pane_id: id, rss, working, local } }
    fn r(ram: f64) -> Room { Room { ram: Some(ram), ..Default::default() } }
    #[test]
    fn a_local_run_is_said_only_when_it_is_working_and_long() { assert_eq!(long_run(true, Some(0), 16 * 60_000), Some(16)); assert_eq!(long_run(true, Some(0), 14 * 60_000), None); assert_eq!(long_run(false, Some(0), 99 * 60_000), None); assert_eq!(long_run(true, None, 99 * 60_000), None); }
    #[test]
    fn danger_pauses_the_hungriest_of_two_and_never_the_only_one() {
        let two = [a(1, 100, true, false), a(2, 900, true, false), a(3, 5000, false, false)];
        assert_eq!(step(&r(93.0), &two, &[], &HashSet::new()), Step::Pause(2, "RAM is 93 % used".into()));
        assert_eq!(step(&r(93.0), &two[..1], &[], &HashSet::new()), Step::Hold);
        assert_eq!(step(&r(85.0), &two, &[], &HashSet::new()), Step::Hold, "loaded is not danger");
        assert_eq!(step(&r(93.0), &two, &[], &HashSet::from([2])), Step::Hold, "one left working");
        assert!(matches!(step(&Room { disk_gb: Some(5.0), ..Default::default() }, &two, &[], &HashSet::new()), Step::Pause(2, w) if w.contains("disk")));
        assert!(matches!(step(&Room { load: Some(3.0), ..Default::default() }, &two, &[], &HashSet::new()), Step::Pause(2, w) if w.contains("processor")));
    }
    #[test]
    fn about_to_fail_pauses_even_the_only_one() {
        let one = [a(1, 100, true, false)];
        assert!(matches!(step(&r(97.0), &one, &[], &HashSet::new()), Step::Pause(1, _)));
        assert!(matches!(step(&Room { disk_gb: Some(1.5), ..Default::default() }, &one, &[], &HashSet::new()), Step::Pause(1, w) if w == "only 1.5 GB of disk is free"));
        assert_eq!(step(&r(97.0), &one, &[1], &HashSet::from([1])), Step::Hold);
    }
    #[test]
    fn vram_pauses_only_agents_on_local_models() {
        let v = [a(1, 900, true, false), a(2, 100, true, true), a(3, 200, true, true)];
        let room = Room { ram: Some(40.0), vram: Some(97.0), ..Default::default() };
        assert!(matches!(step(&room, &v, &[], &HashSet::new()), Step::Pause(3, _)));
        assert_eq!(step(&room, &v[..2], &[], &HashSet::new()), Step::Hold);
    }
    #[test]
    fn it_goes_on_when_calm_or_when_nothing_else_works() {
        let busy = [a(1, 1, true, false), a(2, 1, true, false)];
        let stopped = HashSet::from([2]);
        assert_eq!(step(&r(60.0), &busy, &[2], &stopped), Step::Resume(2));
        assert_eq!(step(&r(85.0), &busy, &[2], &stopped), Step::Hold);
        assert_eq!(step(&r(85.0), &[a(1, 1, false, false), a(2, 1, true, false)], &[2], &stopped), Step::Resume(2));
        assert_eq!(step(&r(93.0), &[a(2, 1, true, false)], &[2], &stopped), Step::Hold, "still in danger");
        assert_eq!(step(&Room { disk_gb: Some(8.0), ..Default::default() }, &busy, &[2], &stopped), Step::Hold, "space not back yet");
        assert_eq!(free_gb("Filesystem 1024-blocks Used Available Capacity Mounted\n/dev/x 100 50 10485760 1% /\n"), Some(10.0));
    }
    #[test]
    fn the_same_command_is_known_by_its_name() { let s = |x: &str| x.to_string(); assert!(same_command(Some(&s("/a/node_modules/x/bin/opencode.exe")), Some(&s("opencode")))); assert!(same_command(Some(&s("ab-12")), Some(&s("ab-12")))); assert!(!same_command(Some(&s("ab-12")), Some(&s("cd-34")))); assert!(!same_command(None, None)); }
    #[test]
    fn a_process_says_its_pane() { assert_eq!(pane_in_environ(b"A=1\0ANTILOKI_PANE_ID=42\0B=2\0"), Some(42)); assert_eq!(pane_in_environ(b"A=1\0"), None); }
}
