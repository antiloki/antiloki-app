//! GROUPS (RFC 0025, operator 2026-11-26): agents working together without a team. The agent the person talks to is the ORCHESTRATOR. Groups are OFF on every agent until the person turns its button on; a group is
//! only PROPOSED by the agent, with an estimate of tokens, price and time, and exists only after the person approves it on the card; the person can refuse ("do it alone") or stop it at any moment. The group stops
//! by itself at the approved number of turns, at one and a half times the high estimate of tokens, or when its time is over. Everything said is kept in the group's record, which the person reads in a pane.
//!
//! The members are the person's specialists, each answering in a call of its own (`specialists::ask_with`). By default they only read. A member may be given FILES TO CHANGE (its scope, shown on the card): the
//! pre-write hook then refuses every other file, and it gets no shell. The person may also approve websites the members read, and a limit the orchestrator may widen a scope within (`grant`). A member may ask
//! another by writing a line `ASK <id>: question`, inside the approved turns. A finished group is kept as a saga.
//!
//!   antiloki_group {op: propose, goal, why, members: [id | {id, write: [globs]}], turns?, sites?: [hosts], limit?: [globs]} | {op: status} | {op: ask, to, question} | {op: grant, to, paths} | {op: close, result}
//!   the screen: POST /api/v1/cockpit/groups {op: on|off, pane_id} | {op: approve|deny|stop, id}      GET /api/v1/cockpit/groups?id=
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_MEMBERS: usize = 4;
const MAX_TURNS: u64 = 20;
const MIN_LIFE_MS: i64 = 15 * 60_000;
const KEPT: usize = 40;

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
pub struct Estimate { pub tokens_low: u64, pub tokens_high: u64, pub usd_low: f64, pub usd_high: f64, pub secs_low: u64, pub secs_high: u64 }
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Line { pub t_ms: i64, pub from: String, pub to: String, pub kind: String, pub text: String }
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Group { /** member -> the files it may change */ #[serde(default)] pub write: BTreeMap<String, Vec<String>>, /** websites the members may read */ #[serde(default)] pub sites: Vec<String>, /** how far the orchestrator may widen a scope */ #[serde(default)] pub limit: Vec<String>, /** the worktree the writers work in */ #[serde(default)] pub work: String, pub id: u64, pub pane_id: i64, pub root: String, pub goal: String, pub why: String, pub members: Vec<String>, pub turns: u64, pub status: String, pub estimate: Estimate, pub turns_used: u64, pub tokens: u64, pub usd: f64, pub created_ms: i64, pub approved_ms: i64, pub note: String, pub log: Vec<Line> }
#[derive(Serialize, Deserialize, Default)]
pub struct Store { pub next: u64, pub on: Vec<i64>, pub groups: Vec<Group> }

fn file() -> PathBuf {
    #[cfg(test)] { return std::env::temp_dir().join(format!("antiloki-groups-test-{}", std::process::id())).join("groups.json"); }
    #[allow(unreachable_code)]
    std::env::var_os("ANTILOKI_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".antiloki"))).unwrap_or_else(std::env::temp_dir).join("groups.json")
}
fn load() -> Store { std::fs::read_to_string(file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default() }
/// Read, change, write, one at a time. Blocking.
fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    static LOCK: Mutex<()> = Mutex::new(()); let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut s = load(); let r = f(&mut s);
    if s.groups.len() > KEPT { let cut = s.groups.len() - KEPT; s.groups.drain(0..cut); }
    let p = file(); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    if let Ok(t) = serde_json::to_string(&s) { let tmp = p.with_extension("tmp"); if std::fs::write(&tmp, t).is_ok() { let _ = std::fs::rename(tmp, p); } }
    r
}
fn open(g: &Group) -> bool { g.status == "proposed" || g.status == "active" }

/// What the person's yes allows, beyond reading the project.
#[derive(Clone, Debug, Default)]
pub struct Scope { pub write: BTreeMap<String, Vec<String>>, pub sites: Vec<String>, pub limit: Vec<String>, pub work: String }
const MAX_SITES: usize = 5;
const MAX_GLOBS: usize = 20;

/// A file pattern a member may be given: inside the worktree, never the whole of it, never the folders antiloki, git and the engines run from. Pure.
pub fn glob_ok(g: &str) -> Result<String, String> {
    let g = g.trim().trim_start_matches("./"); let bad = |why: &str| Err(format!("`{g}`: {why}"));
    if g.is_empty() || g.len() > 200 { return bad("give a file or folder pattern"); }
    if g.starts_with('/') || g.starts_with('~') || g.contains('\\') || g.contains(':') { return bad("a path inside the worktree, with forward slashes"); }
    if g.split('/').any(|p| p == "..") { return bad("no `..`"); }
    let head = g.split('/').next().unwrap_or("");
    if head.contains('*') || head.contains('?') || head.contains('[') && !head.ends_with(']') { return bad("name the folder or file it starts in: the whole worktree is not a scope"); }
    if matches!(head.to_ascii_lowercase().as_str(), ".git" | ".antiloki" | ".claude" | ".codex" | ".cursor" | ".opencode" | ".grok" | ".mcp.json" | "agents.md" | "claude.md") { return bad("that is how the agents and the repository are run: never in a member's scope"); }
    Ok(g.to_string())
}
/// Is file `p` inside the patterns, as the pre-write hook reads them: a plain name holds itself and everything under it. Pure.
pub fn holds(p: &str, globs: &[String]) -> bool { globs.iter().any(|g| { let g = g.trim_end_matches('/'); (!g.contains(['*', '?', '[']) && (p == g || p.starts_with(&format!("{g}/")))) || antiloki_rs::agents_in_pane_scope_sentry::covered(p, std::slice::from_ref(&g.to_string())) }) }
/// Does pattern `g` stay inside `globs`? A pattern is tried by what it would match: one file in it, and one deeper. Pure.
pub fn within(g: &str, globs: &[String]) -> bool {
    let covered = holds;
    let g = g.trim_end_matches('/');
    if g.contains("**") { covered(&g.replace("**", "zz/zz").replace('*', "zz"), globs) && covered(&g.replace("**", "zz/zz/zz").replace('*', "zz"), globs) }
    else if g.contains('*') { covered(&g.replace('*', "zz"), globs) }
    // a plain name is a file, or a folder with everything under it
    else { covered(g, globs) && (g.rsplit('/').next().is_some_and(|n| n.contains('.')) || covered(&format!("{g}/zz/zz"), globs)) }
}
fn overlap(a: &str, b: &str) -> bool { within(a, &[b.to_string()]) || within(b, &[a.to_string()]) }
/// Two members never hold the same files. Pure.
pub fn clash(write: &BTreeMap<String, Vec<String>>) -> Option<String> {
    let all: Vec<(&String, &String)> = write.iter().flat_map(|(m, gs)| gs.iter().map(move |g| (m, g))).collect();
    for (i, (m, g)) in all.iter().enumerate() { for (n, h) in &all[i + 1..] { if m != n && overlap(g, h) { return Some(format!("{m} (`{g}`) and {n} (`{h}`) would change the same files: give each its own")); } } }
    None
}
/// A website by its name only. Pure.
pub fn site_ok(s: &str) -> Result<String, String> {
    let h = s.trim().trim_start_matches("https://").trim_start_matches("http://").split('/').next().unwrap_or("").trim_end_matches('.').to_ascii_lowercase();
    let ok = h.len() <= 100 && h.contains('.') && !h.starts_with('.') && !h.starts_with('-') && !h.contains("..") && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') && h.rsplit('.').next().is_some_and(|t| t.chars().any(|c| c.is_ascii_alphabetic()));
    if ok { Ok(h) } else { Err(format!("`{s}` is not a website's name (like docs.example.com)")) }
}
/// Widen `to`'s scope inside the person's limit, without taking another member's files. Pure over the group.
pub fn grant(g: &mut Group, to: &str, paths: &[String]) -> Result<Vec<String>, String> {
    if g.status != "active" { return Err(format!("the group is {}", g.status)); }
    if !g.members.iter().any(|m| m == to) { return Err(format!("`{to}` is not a member")); }
    if g.limit.is_empty() { return Err("the person approved no room to widen a scope: close this group and propose one with the files, or with `limit`".into()); }
    let mut add = vec![];
    for p in paths { let p = glob_ok(p)?; if !within(&p, &g.limit) { return Err(format!("`{p}` is outside what the person approved ({}): it needs a new group they approve", g.limit.join(", "))); }
        if let Some((m, h)) = g.write.iter().filter(|(m, _)| *m != to).flat_map(|(m, gs)| gs.iter().map(move |h| (m, h))).find(|(_, h)| overlap(&p, h)) { return Err(format!("`{p}` overlaps {m}'s `{h}`")); } add.push(p); }
    if add.is_empty() { return Err("which paths?".into()); }
    let have = g.write.entry(to.to_string()).or_default(); for p in &add { if !have.contains(p) { have.push(p.clone()); } }
    if have.len() > MAX_GLOBS { have.truncate(MAX_GLOBS); }
    Ok(add)
}
/// The first `ASK <member>: question` line of an answer: one member asking another. Pure.
pub fn asks_another(answer: &str, members: &[String], me: &str) -> Option<(String, String)> {
    // the member's id is matched whole: an ordinary agent's id has a colon of its own (`agent:codex`)
    answer.lines().filter_map(|l| l.trim().trim_start_matches(['*', '-', '>', ' ']).strip_prefix("ASK ")).find_map(|r| { let r = r.trim_start_matches('`'); members.iter().filter(|m| *m != me).find_map(|m| r.strip_prefix(m.as_str())?.trim_start_matches('`').trim_start().strip_prefix(':').map(|q| (m.clone(), q.trim().to_string())).filter(|(_, q)| !q.is_empty())) })
}

/// Compared once with a real run (2026-11-26: two turns took 142k tokens, over the first high estimate). What one turn of a member costs, low and high, as (tokens in, tokens out): its brief plus the CLI's own opening and, at the high end, reading the project. Pure.
pub fn turn_tokens(brief_chars: usize) -> ((u64, u64), (u64, u64)) { let b = (brief_chars / 4) as u64; ((b + 25_000, 600), (b + 110_000, 3_500)) }
/// The estimate of a group: `turns` shared evenly among members given as (brief characters, price in, price out per million). A RANGE, not a promise. Pure.
pub fn estimate(members: &[(usize, f64, f64)], turns: u64) -> Estimate {
    if members.is_empty() { return Estimate::default(); }
    let mut e = Estimate::default();
    for i in 0..turns { let (chars, pin, pout) = members[i as usize % members.len()]; let ((li, lo), (hi, ho)) = turn_tokens(chars);
        e.tokens_low += li + lo; e.tokens_high += hi + ho; e.usd_low += (li as f64 * pin + lo as f64 * pout) / 1e6; e.usd_high += (hi as f64 * pin + ho as f64 * pout) / 1e6; }
    e.secs_low = turns * 8; e.secs_high = turns * 90; e
}
/// May this group take one more turn? Pure.
pub fn may_turn(g: &Group, to: &str, now_ms: i64) -> Result<(), String> {
    if g.status != "active" { return Err(format!("the group is {}: it takes no more turns", g.status)); }
    if !g.members.iter().any(|m| m == to) { return Err(format!("`{to}` is not a member ({}); another member needs a new group the person approves", g.members.join(", "))); }
    if g.turns_used >= g.turns { return Err(format!("the {} approved turns are used: close the group and tell the person where it stands", g.turns)); }
    if g.tokens > g.estimate.tokens_high * 3 / 2 { return Err("the group has spent more than its estimate allows: close it and tell the person".into()); }
    if now_ms - g.approved_ms > ((g.estimate.secs_high as i64) * 3_000).max(MIN_LIFE_MS) { return Err("the group's time is over: close it and tell the person".into()); }
    Ok(())
}
pub fn is_on(pane_id: i64) -> bool { load().on.contains(&pane_id) }

/// For the screens' one poll: which agents have groups on, and the groups worth showing (no record text).
/// One group as the screen's card needs it: WHAT THE YES ALLOWS GOES WITH IT (found 2026-11-27: the card said "members only read" while a member had files to change, because those fields were not sent). Pure.
fn brief(g: &Group) -> Value { json!({ "id": g.id, "pane_id": g.pane_id, "status": g.status, "goal": g.goal.chars().take(300).collect::<String>(), "why": g.why.chars().take(300).collect::<String>(), "members": g.members, "write": g.write, "sites": g.sites, "limit": g.limit, "turns": g.turns, "turns_used": g.turns_used, "tokens": g.tokens, "usd": g.usd, "estimate": g.estimate }) }
pub fn summary() -> Value {
    let s = load(); let list: Vec<Value> = s.groups.iter().rev().filter(|g| open(g) || s.on.contains(&g.pane_id)).take(12).map(brief).collect();
    json!({ "on": s.on, "list": list })
}
pub fn one(id: u64) -> Value { match load().groups.into_iter().find(|g| g.id == id) { Some(g) => json!({ "ok": true, "group": g }), None => json!({ "ok": false, "error": "no such group" }) } }

/// The person's clicks. Tells the orchestrator what was decided.
pub fn set(b: &Value, now_ms: i64) -> Value {
    let op = b["op"].as_str().unwrap_or(""); let pane = b["pane_id"].as_i64().unwrap_or(0); let id = b["id"].as_u64().unwrap_or(0);
    let told: Result<Option<(String, i64, String)>, String> = with(|s| match op {
        "on" => { if pane <= 0 { return Err("which agent?".into()); } if !s.on.contains(&pane) { s.on.push(pane); } Ok(None) }
        "off" => { s.on.retain(|p| *p != pane); let mut tell = None; for g in s.groups.iter_mut().filter(|g| g.pane_id == pane && open(g)) { g.status = "stopped".into(); g.note = "the person turned groups off".into(); tell = Some((g.root.clone(), pane, format!("The person turned groups off: group #{} is stopped. Finish the work yourself.", g.id))); } Ok(tell) }
        "approve" | "deny" | "stop" => {
            let g = s.groups.iter_mut().find(|g| g.id == id).ok_or("that group is gone")?;
            let (from, to, words) = match op { "approve" => ("proposed", "active", format!("The person APPROVED group #{id}. Ask its members with antiloki_group {{op: \"ask\", to, question}}, then {{op: \"close\", result}}.")), "deny" => ("proposed", "denied", format!("The person REFUSED group #{id}. Do the work yourself, without a group; do not propose another for this ask.")), _ => ("active", "stopped", format!("The person STOPPED group #{id}. Report where it stands and continue alone only if the person's ask needs it.")) };
            if g.status != from { return Err(format!("the group is {}", g.status)); }
            if op == "approve" && !s.on.contains(&g.pane_id) { return Err("groups are off on that agent".into()); }
            g.status = to.into(); if op == "approve" { g.approved_ms = now_ms; }
            g.log.push(Line { t_ms: now_ms, from: "you".into(), to: String::new(), kind: "decision".into(), text: to.into() });
            Ok(Some((g.root.clone(), g.pane_id, words)))
        }
        _ => Err("op is on, off, approve, deny or stop".into()),
    });
    match told { Ok(t) => { if let Some((root, p, words)) = t.filter(|_| !cfg!(test)) { if let Err(e) = crate::mailbox::send_now(&root, "antiloki", 0, &format!("pane-{p}"), &words) { eprintln!("antiloki-serve: the agent could not be told about its group: {e}"); } } json!({ "ok": true }) } Err(e) => json!({ "ok": false, "error": e }) }
}

/// Propose: kept as `proposed` until the person decides. `priced`: (brief characters, price in, price out) of each member. Blocking.
pub fn propose(pane_id: i64, root: &str, goal: &str, why: &str, members: Vec<String>, turns: Option<u64>, scope: Scope, priced: &[(usize, f64, f64)], now_ms: i64) -> Result<Group, String> {
    let (goal, why) = (goal.trim(), why.trim()); if goal.chars().count() < 10 { return Err("say the goal of the group".into()); } if why.chars().count() < 10 { return Err("say why one agent is not enough".into()); }
    if members.is_empty() || members.len() > MAX_MEMBERS { return Err(format!("a group has 1 to {MAX_MEMBERS} members besides you")); }
    if scope.write.keys().any(|m| !members.contains(m)) { return Err("only a member holds files".into()); }
    if let Some(w) = clash(&scope.write) { return Err(w); }
    if scope.sites.len() > MAX_SITES { return Err(format!("at most {MAX_SITES} websites")); }
    let turns = turns.unwrap_or(members.len() as u64 * 2).clamp(1, MAX_TURNS); let est = estimate(priced, turns);
    with(|s| {
        if !s.on.contains(&pane_id) { return Err("GROUPS ARE OFF on this agent. Do the work yourself. If a group would clearly help, tell the person they can turn on the groups button on your pane.".to_string()); }
        if s.groups.iter().any(|g| g.pane_id == pane_id && open(g)) { return Err("you already have a group waiting or running (op `status`): one at a time".into()); }
        s.next += 1; let g = Group { write: scope.write, sites: scope.sites, limit: scope.limit, work: scope.work, id: s.next, pane_id, root: root.into(), goal: goal.chars().take(2_000).collect(), why: why.chars().take(600).collect(), members, turns, status: "proposed".into(), estimate: est, created_ms: now_ms, log: vec![Line { t_ms: now_ms, from: format!("pane-{pane_id}"), to: String::new(), kind: "goal".into(), text: goal.chars().take(2_000).collect() }], ..Default::default() };
        s.groups.push(g.clone()); Ok(g)
    })
}
fn mine(s: &Store, pane_id: i64) -> Option<usize> { s.groups.iter().rposition(|g| g.pane_id == pane_id) }

/// THE LAST ANSWER ENDS THE GROUP (2026-11-26: a group with its turns used stayed "active", turning, until somebody stopped it). Pure over the group.
pub fn finish_if_spent(g: &mut Group) -> bool { if g.status == "active" && g.turns_used >= g.turns && g.log.iter().filter(|l| l.kind == "answer").count() as u64 >= g.turns { g.status = "closed".into(); true } else { false } }
/// One turn: the member is asked and its answer comes back to the orchestrator. A member's own `ASK <id>: …` line is taken as the next turn, while turns are left.
async fn ask(pane_id: i64, to: &str, question: &str) -> Result<String, String> {
    let mut said = String::new(); let (mut from, mut to, mut question) = (format!("pane-{pane_id}"), to.to_string(), question.to_string());
    for hop in 0..MAX_TURNS {
        match turn(pane_id, &from, &to, &question).await {
            Ok((text, next)) => { said.push_str(&text); match next { Some((n, q)) => { said.push_str(&format!("\n\n{to} asked {n}.\n")); from = to; to = n; question = q; } None => break } }
            Err(e) if hop == 0 => return Err(e),
            Err(e) => { said.push_str(&format!("\n{to} could not be asked: {e}")); break }
        }
    }
    Ok(said)
}
async fn turn(pane_id: i64, from: &str, to: &str, question: &str) -> Result<(String, Option<(String, String)>), String> {
    let q = question.trim().to_string(); if q.is_empty() { return Err("ask a question".into()); }
    let (t, q2, f) = (to.to_string(), q.clone(), from.to_string());
    // the turn is taken before the call, so two calls cannot spend the same one
    let g = tokio::task::spawn_blocking(move || with(|s| {
        if !s.on.contains(&pane_id) { return Err("groups are off on this agent".to_string()); }
        let i = mine(s, pane_id).ok_or("you have no group: op `propose`")?; let now = crate::cli::now_ms(); may_turn(&s.groups[i], &t, now)?;
        let g = &mut s.groups[i]; g.turns_used += 1; g.log.push(Line { t_ms: now, from: f.clone(), to: t.clone(), kind: "ask".into(), text: q2.chars().take(4_000).collect() }); Ok(g.clone())
    })).await.map_err(|e| e.to_string())??;
    let id = g.id; let write = g.write.get(to).cloned().unwrap_or_default();
    // members that write take turns one at a time, so what changed can be told apart
    static WRITING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = if write.is_empty() { None } else { Some(WRITING.lock().await) };
    let extra = crate::specialists::Extra { write: write.clone(), sites: g.sites.clone(), work: g.work.clone(), others: g.members.iter().filter(|m| *m != to).cloned().collect(), ..Default::default() };
    let asker = if from.starts_with("pane-") { format!("the orchestrator of group #{id}") } else { format!("{from}, a member of group #{id}") };
    let before = if write.is_empty() { Default::default() } else { changed_files(g.work.clone()).await };
    let said = crate::specialists::ask_with(&g.root, &asker, to, &format!("The group's goal: {}\n\n{q}", g.goal), extra).await;
    let changed: Vec<String> = if write.is_empty() { vec![] } else { changed_files(g.work.clone()).await.into_iter().filter(|(p, h)| before.get(p) != Some(h)).map(|(p, _)| p).filter(|p| !p.starts_with(".antiloki/")).collect() };
    let outside: Vec<String> = changed.iter().filter(|p| !holds(p, &write)).cloned().collect();
    let t = to.to_string(); let members = g.members.clone();
    tokio::task::spawn_blocking(move || with(|s| {
        let g = s.groups.iter_mut().find(|g| g.id == id).ok_or("the group is gone")?; let now = crate::cli::now_ms();
        match said {
            Ok(a) => { g.tokens += a.tokens_in + a.tokens_out; g.usd += a.usd; g.log.push(Line { t_ms: now, from: t.clone(), to: format!("pane-{pane_id}"), kind: "answer".into(), text: a.text.chars().take(12_000).collect() });
                let mut note = String::new();
                if !changed.is_empty() { let list = changed.join(", "); g.log.push(Line { t_ms: now, from: t.clone(), to: String::new(), kind: "changed".into(), text: list.clone() }); note = format!("\nFiles changed during this turn: {list}"); }
                if !outside.is_empty() { let w = format!("changed outside {t}'s scope while it worked (it, or somebody else in the worktree): {}", outside.join(", ")); g.log.push(Line { t_ms: now, from: t.clone(), to: String::new(), kind: "error".into(), text: w.clone() }); note.push_str(&format!("\nWARNING: {w}")); }
                let last = finish_if_spent(g);
                let next = if g.status == "active" && g.turns_used < g.turns { asks_another(&a.text, &members, &t) } else { None };
                Ok((format!("{t} answers:\n{}{note}\n\n({} of {} turns used{})", a.text, g.turns_used, g.turns, if last { "; the group is finished: close it with your result" } else { "" }), next)) }
            Err(e) => { g.turns_used = g.turns_used.saturating_sub(1); g.log.push(Line { t_ms: now, from: t.clone(), to: String::new(), kind: "error".into(), text: e.clone() }); Err(e) }
        }
    })).await.map_err(|e| e.to_string())?
}
/// path -> what is in it, for every file git sees as changed or new in the worktree. Empty where there is no repository.
async fn changed_files(work: String) -> BTreeMap<String, u64> {
    tokio::task::spawn_blocking(move || {
        let Ok(o) = crate::proc::cmd("git").args(["status", "--porcelain", "-z", "--untracked-files=all"]).current_dir(&work).output() else { return BTreeMap::new() };
        String::from_utf8_lossy(&o.stdout).split('\0').filter(|l| l.len() > 3).map(|l| { let p = l[3..].to_string(); use std::hash::{Hash, Hasher}; let mut h = std::collections::hash_map::DefaultHasher::new(); std::fs::read(PathBuf::from(&work).join(&p)).unwrap_or_default().hash(&mut h); (p, h.finish()) }).collect()
    }).await.unwrap_or_default()
}
/// A finished group is kept with the workspace's sagas: the goal, every answer, the result. Blocking.
fn keep_as_saga(g: &Group) -> Result<i64, String> {
    let c = crate::work::open(&g.root)?; let actor = format!("pane-{}", g.pane_id);
    let id = crate::work::create_saga(&c, &format!("Group: {}", g.goal.chars().take(90).collect::<String>()), &g.goal, &actor)?;
    for l in g.log.iter().filter(|l| l.kind == "answer" || l.kind == "result") {
        let who = if l.kind == "result" { actor.clone() } else { l.from.clone() };
        let title = if l.kind == "result" { "The group's result".to_string() } else { format!("{} answered", l.from) };
        let e = crate::work::create_episode(&c, id, None, &title, &l.text.chars().take(6_000).collect::<String>(), &who, &who)?;
        crate::work::update_episode(&c, e, &json!({ "status": "done" }), &who)?;
    }
    Ok(id)
}

/// The agent's tool.
pub async fn run_tool(pane_id: i64, root: &str, work: &str, args: &Value) -> Result<String, String> {
    if pane_id <= 0 { return Err("only an agent in a pane leads a group".into()); }
    let s = |k: &str| args[k].as_str().unwrap_or("").to_string();
    match s("op").as_str() {
        "propose" => {
            let mut write: BTreeMap<String, Vec<String>> = BTreeMap::new(); let mut members: Vec<String> = vec![];
            let words = |v: &Value| -> Vec<String> { v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(|x| x.trim().to_string())).filter(|x| !x.is_empty()).collect() };
            for m in args["members"].as_array().into_iter().flatten() {
                let id = m.as_str().or_else(|| m["id"].as_str()).unwrap_or("").trim().to_string(); if id.is_empty() || members.contains(&id) { continue; }
                let globs = words(&m["write"]).iter().map(|g| glob_ok(g)).collect::<Result<Vec<_>, _>>()?; if globs.len() > MAX_GLOBS { return Err(format!("at most {MAX_GLOBS} patterns a member")); }
                if !globs.is_empty() { write.insert(id.clone(), globs); }
                members.push(id);
            }
            let scope = Scope { write, sites: words(&args["sites"]).iter().map(|s| site_ok(s)).collect::<Result<Vec<_>, _>>()?, limit: words(&args["limit"]).iter().map(|g| glob_ok(g)).collect::<Result<Vec<_>, _>>()?, work: work.to_string() };
            if !scope.write.is_empty() && (work.is_empty() || !std::path::Path::new(work).is_dir()) { return Err("members change files only in your own worktree, and this pane has none".into()); }
            let (r, ms, goal, why, turns) = (root.to_string(), members.clone(), s("goal"), s("why"), args["turns"].as_u64());
            let g = tokio::task::spawn_blocking(move || {
                let mut priced = vec![];
                for m in &ms {
                    // a member is one of the person's specialists, or an ordinary agent on a provider (`agent:codex`)
                    let (name, engine, model, chars) = match crate::specialists::ordinary(m) {
                        Some(e) => { let named = crate::specialists::ordinary_model(m); if e == crate::cli::Engine::Opencode && named.is_none() { return Err("an OpenCode member names its model: agent:opencode@<provider>/<model> (`opencode models` lists them)".to_string()); }
                            (m.clone(), e, named.or_else(|| crate::cli::tier_model(e, crate::cli::Tier::Mid)), crate::specialists::ordinary_brief(m).len()) }
                        None => { let sp = crate::specialists::get(&r, m).ok_or_else(|| format!("no member `{m}`: a specialist's id (antiloki_remember op `list`), or agent:claude-code, agent:codex, agent:gemini, agent:cursor, agent:opencode"))?;
                            if !crate::specialists::usable(&sp) { return Err(format!("`{}` is a draft: the person approves it first", sp.name)); }
                            let engine = crate::cli::engine_of(&json!(sp.seat)).unwrap_or(crate::cli::Engine::ClaudeCode);
                            let model = Some(sp.model.trim().to_string()).filter(|x| !x.is_empty()).or_else(|| crate::cli::tier_model(engine, crate::cli::Tier::Mid));
                            (sp.name.clone(), engine, model, crate::specialists::brief(&r, m, "").map(|b| b.len()).unwrap_or(4_000)) }
                    };
                    if !crate::cli::member_usable(engine, model.as_deref()) { return Err(format!("`{name}` runs on {}, which is not usable here (not installed, signed out or switched off)", engine.key())); }
                    if engine != crate::cli::Engine::ClaudeCode && scope.write.contains_key(m) { crate::wall::available().map_err(|e| format!("`{name}`: {e}"))?; }
                    if let Some(why) = crate::cli::model_blocked(engine, model.as_deref()) { return Err(format!("`{name}`: {why}")); }
                    let (pin, pout) = crate::cli::prices_of(engine, model.as_deref());
                    priced.push((chars, pin, pout));
                }
                propose(pane_id, &r, &goal, &why, ms, turns, scope, &priced, crate::cli::now_ms())
            }).await.map_err(|e| e.to_string())??;
            Ok(format!("NOT STARTED: group #{} waits for the person's approval on a card on their screen ({} turns, about {}-{} thousand tokens). Tell them, then STOP and wait: you are told here when they decide.", g.id, g.turns, g.estimate.tokens_low / 1000, g.estimate.tokens_high / 1000))
        }
        "status" => Ok(tokio::task::spawn_blocking(move || { let s = load(); let on = s.on.contains(&pane_id); match mine(&s, pane_id) { Some(i) => { let g = &s.groups[i]; format!("groups are {} on this agent. Group #{}: {} ({} of {} turns, {} tokens). Members: {}.", if on { "ON" } else { "OFF" }, g.id, g.status, g.turns_used, g.turns, g.tokens, g.members.join(", ")) } None => format!("groups are {} on this agent; you have no group", if on { "ON" } else { "OFF" }) } }).await.map_err(|e| e.to_string())?),
        "ask" => ask(pane_id, &s("to"), &s("question")).await,
        "grant" => { let to = s("to"); let paths: Vec<String> = args["paths"].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect(); tokio::task::spawn_blocking(move || with(|st| { let i = mine(st, pane_id).ok_or("you have no group")?; let g = &mut st.groups[i]; let added = grant(g, &to, &paths)?; g.log.push(Line { t_ms: crate::cli::now_ms(), from: format!("pane-{pane_id}"), to: to.clone(), kind: "grant".into(), text: added.join(", ") }); Ok(format!("{to} may now also change {}", added.join(", "))) })).await.map_err(|e| e.to_string())? }
        "close" => { let result = s("result"); tokio::task::spawn_blocking(move || { let done = with(|st| { let i = mine(st, pane_id).ok_or("you have no group")?; let g = &mut st.groups[i]; if g.status == "proposed" { g.status = "closed".into(); return Ok(format!("group #{} is withdrawn", g.id)); }
            // a finished group still takes the orchestrator's result, once
            if !(g.status == "active" || g.status == "closed") || g.log.iter().any(|l| l.kind == "result") { return Err(format!("the group is {}", g.status)); } g.status = "closed".into(); g.log.push(Line { t_ms: crate::cli::now_ms(), from: format!("pane-{pane_id}"), to: String::new(), kind: "result".into(), text: result.chars().take(8_000).collect() }); Ok(format!("group #{} is closed: {} turns, {} tokens. Tell the person the result and what it cost.", g.id, g.turns_used, g.tokens)) }); let said: Result<String, String> = done; if said.is_ok() { if let Some(g) = load().groups.iter().rev().find(|g| g.pane_id == pane_id && g.log.iter().any(|l| l.kind == "result")) { if let Err(e) = keep_as_saga(g) { eprintln!("antiloki-serve: group #{} was not kept as a saga: {e}", g.id); } } } said }).await.map_err(|e| e.to_string())? }
        _ => Err("op is propose, status, ask, grant or close".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_estimate_is_a_range_that_grows_with_turns() {
        let m = [(8_000, 1.0, 5.0), (4_000, 3.0, 15.0)];
        let a = estimate(&m, 2); let b = estimate(&m, 4);
        assert!(a.tokens_low > 30_000 && a.tokens_low < a.tokens_high && a.usd_low < a.usd_high && a.secs_low < a.secs_high);
        assert_eq!(b.tokens_low, a.tokens_low * 2); assert!(b.usd_high > a.usd_high); assert_eq!(estimate(&[], 3), Estimate::default());
    }
    #[test]
    fn a_turn_needs_an_approved_group_a_member_and_room_in_every_limit() {
        let g = Group { status: "active".into(), members: vec!["cc".into()], turns: 2, estimate: Estimate { tokens_high: 1_000, secs_high: 60, ..Default::default() }, approved_ms: 1_000, ..Default::default() };
        assert!(may_turn(&g, "cc", 2_000).is_ok());
        assert!(may_turn(&Group { status: "proposed".into(), ..g.clone() }, "cc", 2_000).is_err(), "nothing before the person approves");
        assert!(may_turn(&g, "other", 2_000).is_err()); assert!(may_turn(&Group { turns_used: 2, ..g.clone() }, "cc", 2_000).is_err());
        assert!(may_turn(&Group { tokens: 1_501, ..g.clone() }, "cc", 2_000).is_err()); assert!(may_turn(&g, "cc", 1_000 + MIN_LIFE_MS + 1).is_err());
    }
    #[test]
    fn a_scope_stays_in_the_worktree_and_two_members_never_share_a_file() {
        for bad in ["", "/etc/passwd", "~/x", "../x", "a/../../x", "**", "*.rs", "**/*.md", ".git/config", ".antiloki/x", ".claude/settings.json", "AGENTS.md", "c:\\x"] { assert!(glob_ok(bad).is_err(), "{bad}"); }
        for good in ["docs", "docs/**", "src/a.rs", "./src/ui/*.tsx"] { assert!(glob_ok(good).is_ok(), "{good}"); }
        let l = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(within("docs/a/**", &l(&["docs/**"])) && within("docs/a.md", &l(&["docs"])) && !within("docs", &l(&["docs/*"])) && !within("docs/**", &l(&["docs/a/**"])) && !within("docsx/a", &l(&["docs/**"])));
        let w = |a: &[&str], b: &[&str]| BTreeMap::from([("a".to_string(), l(a)), ("b".to_string(), l(b))]);
        assert!(clash(&w(&["docs/**"], &["docs/x.md"])).is_some()); assert!(clash(&w(&["docs/**"], &["src/**"])).is_none());
    }
    #[test]
    fn a_scope_widens_only_inside_the_persons_limit_and_never_onto_anothers_files() {
        let l = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let mut g = Group { status: "active".into(), members: l(&["a", "b"]), write: BTreeMap::from([("a".to_string(), l(&["docs/**"])), ("b".to_string(), l(&["src/b/**"]))]), limit: l(&["src/**"]), ..Default::default() };
        assert_eq!(grant(&mut g, "a", &l(&["src/a/**"])).unwrap(), l(&["src/a/**"])); assert_eq!(g.write["a"], l(&["docs/**", "src/a/**"]));
        assert!(grant(&mut g, "a", &l(&["config/x"])).unwrap_err().contains("outside")); assert!(grant(&mut g, "a", &l(&["src/b/x.rs"])).unwrap_err().contains("overlaps"));
        assert!(grant(&mut g, "c", &l(&["src/c/**"])).is_err()); g.limit.clear(); assert!(grant(&mut g, "a", &l(&["src/z/**"])).is_err());
    }
    #[test]
    fn a_site_is_a_name_and_a_member_asks_another_by_a_line() {
        assert_eq!(site_ok("https://Docs.Example.com/a?b").unwrap(), "docs.example.com"); for bad in ["localhost", "127.0.0.1", "a b.com", "..com", "x.com)", "*.x.com"] { assert!(site_ok(bad).is_err(), "{bad}"); }
        let m = vec!["a".to_string(), "b".to_string()];
        let two = vec!["agent:codex".to_string(), "agent:cursor".to_string()]; assert_eq!(asks_another("ASK agent:cursor: agreed?", &two, "agent:codex"), Some(("agent:cursor".into(), "agreed?".into())));
        assert_eq!(crate::specialists::ordinary_model("agent:opencode@opencode-go/x#2").as_deref(), Some("opencode-go/x")); assert!(crate::specialists::ordinary("agent:opencode@a/b").is_some() && crate::specialists::ordinary_model("agent:codex").is_none());
        let p = crate::specialists::ask_prompt("brief", "x", "q"); let o = crate::specialists::own_tools_only(&p); assert!(o.contains("own file and shell tools") && !o.contains("antiloki_book") && o.ends_with("q"), "{o}");
        assert!(crate::specialists::ordinary("agent:codex").is_some() && crate::specialists::ordinary("agent:gemini#2").is_some() && crate::specialists::ordinary("agent:nope").is_none() && crate::specialists::ordinary("copy").is_none());
        assert_eq!(asks_another("Fine.\n- ASK b: is the price right?", &m, "a"), Some(("b".into(), "is the price right?".into()))); assert_eq!(asks_another("ASK a: me?", &m, "a"), None); assert_eq!(asks_another("ASK c: who?", &m, "a"), None); assert_eq!(asks_another("I will ask b: later", &m, "a"), None);
    }
    #[test]
    fn the_card_is_told_every_thing_the_yes_allows() {
        let g = Group { write: BTreeMap::from([("w".to_string(), vec!["docs/**".to_string()])]), sites: vec!["example.com".into()], limit: vec!["src/**".into()], ..Default::default() };
        let v = brief(&g); assert_eq!((v["write"]["w"][0].as_str(), v["sites"][0].as_str(), v["limit"][0].as_str()), (Some("docs/**"), Some("example.com"), Some("src/**")));
    }
    #[test]
    fn the_last_answer_finishes_the_group() {
        let line = |k: &str| Line { kind: k.into(), ..Default::default() };
        let mut g = Group { status: "active".into(), turns: 2, turns_used: 2, log: vec![line("answer")], ..Default::default() };
        assert!(!finish_if_spent(&mut g), "one answer is still on its way"); g.log.push(line("answer")); assert!(finish_if_spent(&mut g)); assert_eq!(g.status, "closed");
    }
    #[test]
    fn off_by_default_one_group_at_a_time_and_only_the_person_starts_it() {
        let _ = std::fs::remove_file(file());   // the only test of this module that touches the file
        let ask = || propose(7, "/p", "plan the launch of the product", "it needs two points of view", vec!["cc".into()], None, Scope::default(), &[(4_000, 1.0, 5.0)], 10);
        assert!(ask().unwrap_err().contains("GROUPS ARE OFF"));
        assert_eq!(set(&json!({ "op": "on", "pane_id": 7 }), 11)["ok"], json!(true));
        let g = ask().unwrap(); assert_eq!((g.status.as_str(), g.turns), ("proposed", 2)); assert!(ask().unwrap_err().contains("one at a time"));
        assert!(may_turn(&load().groups[0], "cc", 12).is_err());
        assert_eq!(set(&json!({ "op": "stop", "id": g.id }), 13)["ok"], json!(false), "only a running one is stopped");
        assert_eq!(set(&json!({ "op": "off", "pane_id": 7 }), 14)["ok"], json!(true)); assert_eq!(load().groups[0].status, "stopped"); assert!(!is_on(7));
        let _ = std::fs::remove_file(file());
    }
}
