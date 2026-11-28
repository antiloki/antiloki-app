//! THE AGENT SESSION (operator 2026-09-20: "is a raw terminal the right call for the UI? what would I lose
//! if I replaced the Claude Code agent for a chatbot — a different UI like antiChat, keeping all the power?").
//!
//! One long-lived `claude -p --input-format stream-json --output-format stream-json` process per pane —
//! the CLI's own programmatic mode, on the same subscription login, with the pane's context pack, MCP
//! tools and hooks exactly as the TUI gets them. The pane talks JSON both ways:
//!
//!   stdin   {"type":"user","message":{"role":"user","content":"…"}}
//!   stdout  system/init · stream_event (text deltas, live only) · assistant · user (tool results) ·
//!           result (cost, session id) · rate_limit_event (the plan window, for free)
//!
//! Every persisted event is appended to `<worktree>/.antiloki/agent-events.jsonl` as `{seq, t, ev}` so
//! the chat re-opens with its history after any restart, and broadcast live through the pane's
//! multiplexed stream (`pty/streams`) as `{pane, kind: "agent", seq, data}`. The session id is pinned
//! (`.antiloki/pty-session.json`, the same file the handoff reads) so a restart `--resume`s.
//!
//! No PTY: nothing to resize, no escape sequences, no per-key POSTs, no composer to intercept —
//! Heimdall reads the line before the agent does, in the pane's own composer.
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use crate::dispatch::{engine_call, AppState};

fn body_json(b: &Bytes) -> Value { serde_json::from_slice(b).unwrap_or(Value::Null) }
fn num(v: Option<&Value>) -> i64 { v.and_then(|x| x.as_i64().or_else(|| x.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0) }
fn s(v: Option<&Value>) -> String { v.and_then(Value::as_str).unwrap_or("").to_string() }
pub(crate) fn now_ms() -> i64 { crate::cli::now_ms() }

pub(crate) struct Live {
    /// the user's text, one message per send — the engine's pump wraps it its own way
    pub(crate) stdin: tokio::sync::mpsc::UnboundedSender<String>,
    pub(crate) child_id: Option<u32>,
    pub(crate) engine: String,
    /// set on stop: the engine's follower tasks end (OpenCode's SSE, its sender)
    pub(crate) cancel: Arc<std::sync::atomic::AtomicBool>,
    /// what stop() does beyond the flag — OpenCode aborts its session over HTTP
    pub(crate) on_stop: Option<Box<dyn FnOnce() + Send>>,
    pub(crate) session_id: String,
    pub(crate) model: String,
    /// a turn is in flight: between a user send and the CLI's `result`
    pub(crate) busy: bool,
    pub(crate) cost_usd: f64,
    pub(crate) started: i64,
    /// MIN (2026-09-21): the pane is MIN; `lift` = this one turn runs with exploration on (a no-intel route); the
    /// turn's start and the ask, so the result can be priced as a MIN turn; `parent` = the pane's run key
    pub(crate) min: bool,
    pub(crate) lift: bool,
    pub(crate) turn_started: i64,
    pub(crate) last_ask: String,
    pub(crate) parent: String,
    /// THE GATES' ONE RETRY (2026-09-24): true while this turn is the agent's second go at the same claim, after
    /// test/build/lint handed it the logs. Cleared on the operator's next send, so every ASK gets one retry and a
    /// model that cannot fix the build with the compiler's own error in front of it does not bill for a third try.
    /// THE TURN'S OWN TOKENS, ACCUMULATED (operator 2026-09-28: "0 tokens out … wich is a mistake").
    ///
    /// The `result` event carries the usage of the LAST assistant message, not of the turn. A turn that read a
    /// file, edited it and then said "Added Shoes." reported 22 output tokens — the sentence — while every token
    /// it actually produced sat in the intermediate messages. Reset when the ask is sent, added to on every
    /// assistant message, and preferred over the result's own figure when it is larger.
    pub(crate) turn_tin: i64,
    pub(crate) turn_tout: i64,
    pub(crate) gate_retry: bool,
    /// THE EMERGENCY (2026-09-24): this agent's process tree is SIGSTOPped because the box was about to thrash.
    /// It is work the operator asked for that is not happening, so it is resumed at the first opportunity.
    pub(crate) paused: bool,
}

#[derive(Default)]
pub struct AgentRegistry {
    pub(crate) live: Mutex<HashMap<i64, Live>>,
    /// PANES WITH A START IN FLIGHT (operator 2026-09-28: "it is always exiting on the first try").
    ///
    /// A start takes a second or two, and it is not in `live` until it finishes — so a send that arrives in that
    /// window sees "not running", starts a SECOND one, and the second `start()` SIGTERMs the first. The pane came
    /// up, but the thread showed the corpse of the one that was killed. One start per pane at a time.
    starting: Mutex<std::collections::HashSet<i64>>,
    /// per pane: the next persisted seq, and the live broadcast (seq, line)
    seqs: Mutex<HashMap<i64, u64>>,
    chans: Mutex<HashMap<i64, tokio::sync::broadcast::Sender<(u64, String)>>>,
    /// the worktree of every pane that ever started here — the events file lives there
    roots: Mutex<HashMap<i64, String>>,
    /// the goal runs, so a turn's tokens price the inline route that was delivered to the pane
    pub(crate) goals: Mutex<Option<Arc<crate::goals::Goals>>>,
    /// the db, so a finished turn can read its pane's own gate toggles fresh rather than a copy taken at start —
    /// the operator flips test/build/lint mid-conversation and expects the next turn to obey
    pub(crate) state: Mutex<Option<Arc<AppState>>>,
    /// WHAT IS RUNNING RIGHT NOW THAT IS NOT A MODEL (operator 2026-09-24: "we have a loading for subagents on the
    /// top, add on the side for QA, build, test, lint etc so we see live running processes"). A build takes
    /// minutes and until now spent them invisibly: the agent looked idle while the box was busy proving its
    /// claim. Per pane, the checks in flight, in the order they were started.
    checks: Mutex<HashMap<i64, Vec<String>>>,
    /// the workspace epoch this registry's state belongs to (`dispatch::workspace_epoch`, per serve)
    epoch: std::sync::atomic::AtomicU64,
}

impl AgentRegistry {
    pub fn new() -> Arc<Self> { Arc::new(Self::default()) }
    /// ONE WORKSPACE'S PANES MUST NEVER BE ANOTHER'S (2026-10-11, the operator: "I opened antiloki and it showed a continuation of the same conversation in all projects"). Pane ids are
    /// numbered per workspace database, so project A's pane 1 and project B's pane 1 shared every per-pane key here: the live agent (B's messages went to A's running CLI), the
    /// next event number, the broadcast, the folder cache. When the workspace changes, every agent is stopped and every per-pane map is emptied; the new workspace's panes start clean
    /// (a Claude pane resumes from its own folder, which is per workspace too). Returns whether it reset. Pure over the epoch it is given - tested.
    pub fn sync_to(&self, epoch: u64) -> bool {
        if self.epoch.swap(epoch, std::sync::atomic::Ordering::SeqCst) == epoch { return false; }
        let ids: Vec<i64> = self.live.lock().unwrap_or_else(|p| p.into_inner()).keys().copied().collect();
        for id in ids { self.stop(id); }
        self.starting.lock().unwrap_or_else(|p| p.into_inner()).clear();
        self.seqs.lock().unwrap_or_else(|p| p.into_inner()).clear();
        self.chans.lock().unwrap_or_else(|p| p.into_inner()).clear();
        self.roots.lock().unwrap_or_else(|p| p.into_inner()).clear();
        self.checks.lock().unwrap_or_else(|p| p.into_inner()).clear();
        true
    }
    /// `sync_to` the serve's current workspace epoch; called before every request.
    pub fn sync_epoch(&self) -> bool {
        let Some(path) = self.state.lock().unwrap_or_else(|p| p.into_inner()).as_ref().map(|st| st.db_path.clone()) else { return false };
        self.sync_to(crate::dispatch::workspace_epoch(&path))
    }
    pub(crate) fn root_of(&self, pane_id: i64) -> Option<String> { self.roots.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id).cloned() }
    /// a pane's box, known before its first start (chat_pane.rs writes episode markers there)
    pub(crate) fn set_root(&self, pane_id: i64, cwd: &str) { self.roots.lock().unwrap_or_else(|p| p.into_inner()).insert(pane_id, cwd.to_string()); { let mut m = self.seqs.lock().unwrap_or_else(|p| p.into_inner()); let e = m.entry(pane_id).or_insert(0); if *e == 0 { *e = seq_on_disk(cwd, pane_id); } } }
    pub fn set_goals(&self, g: Arc<crate::goals::Goals>) { *self.goals.lock().unwrap_or_else(|p| p.into_inner()) = Some(g); }
    pub fn set_state(&self, st: Arc<AppState>) { *self.state.lock().unwrap_or_else(|p| p.into_inner()) = Some(st); }
    /// the deterministic checks a pane has in flight — read by `agents/live` for the switcher's indicator
    /// how many agents are alive right now — the timeline's header count
    pub fn live_count(&self) -> usize { self.live.lock().unwrap_or_else(|p| p.into_inner()).len() }
    pub fn checks_of(&self, pane_id: i64) -> Vec<String> { self.checks.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id).cloned().unwrap_or_default() }

    /// WHAT EACH AGENT IS COSTING IN MEMORY (2026-09-24), and whether it is currently stopped. One walk of /proc
    /// for all of them — per-agent walks turn a 3-second poll into a directory storm.
    pub fn memory(&self) -> Vec<crate::mem::Watched> {
        let parents = crate::mem::process_parents();
        let live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        live.iter().map(|(id, l)| {
            let pid = l.child_id.unwrap_or(0) as i32;
            crate::mem::Watched { pane_id: *id, pid, rss: if pid > 0 { crate::mem::tree_rss(pid, &parents) } else { 0 }, busy: l.busy, paused: l.paused }
        }).collect()
    }
    /// THE MEMORY WATCHDOG (operator 2026-09-24: "have a measure if the PC is close to froze like pause one
    /// process till other finishes (only as emergency measure)").
    ///
    /// Every few seconds: read the machine, and if it is genuinely about to thrash, stop the hungriest agent that
    /// is actually working — never the last one, never more than one at a time — and resume everything the moment
    /// it is not critical any more. Both events are announced in the agent's own transcript, because an agent
    /// that silently stopped answering is indistinguishable from one that has hung, and the operator would be
    /// right to think antiloki had broken.
    /// A LOCAL MODEL THAT HOGS THE CPU TELLS THE PANE (2026-11-22: a model server sat at 320 % for 40 minutes and the screen crawled). Every 10 s the model server's CPU is sampled; three windows in a row over 250 % (2.5 cores) with no agent working says so, once every 10 minutes, in every pane on a local model.
    pub fn watch_cpu(self: &Arc<Self>) {
        let reg = self.clone();
        tokio::spawn(async move {
            let (mut prev, mut hot, mut last) = (crate::mem::model_server_ticks(), 0u32, 0i64);
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                let now = crate::mem::model_server_ticks(); let pct = crate::mem::cpu_percent(prev, now, 10); prev = now;
                hot = if pct > 250 { hot + 1 } else { 0 };
                if hot < 3 || now_ms().saturating_sub(last) < 600_000 { continue; }
                let panes: Vec<i64> = { let live = reg.live.lock().unwrap_or_else(|p| p.into_inner()); live.iter().filter(|(_, l)| crate::local_panes::tag_of(&l.model).is_some()).map(|(id, _)| *id).collect() };
                if panes.is_empty() { continue; }
                last = now_ms();
                for id in panes { reg.emit(id, json!({ "type": "antiloki", "subtype": "cpu", "text": format!("⚠ the local model server has used about {pct}% of a core for 30 s: the machine will feel slow. A smaller model, or a cloud seat for this pane, is lighter.") }), true); }
            }
        });
    }
    /// Stop or resume one agent's process tree. Returns the pid it acted on, so the caller can say so out loud.
    pub fn set_paused(&self, pane_id: i64, paused: bool) -> Option<i32> {
        let mut live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        let l = live.get_mut(&pane_id)?;
        let pid = l.child_id? as i32;
        let ok = if paused { crate::mem::pause_tree(pid) } else { crate::mem::resume_tree(pid) };
        if !ok { return None }
        l.paused = paused;
        Some(pid)
    }
    fn check_start(&self, pane_id: i64, what: &str) { self.checks.lock().unwrap_or_else(|p| p.into_inner()).entry(pane_id).or_default().push(what.to_string()); }
    fn check_end(&self, pane_id: i64, what: &str) {
        let mut m = self.checks.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(v) = m.get_mut(&pane_id) { if let Some(i) = v.iter().position(|x| x == what) { v.remove(i); } if v.is_empty() { m.remove(&pane_id); } }
    }

    /// THE DETERMINISTIC GATES AFTER A CLAIM (operator 2026-09-24: "3 buttons … run after the agent claim
    /// completition and force re-run in case of error with the error logs").
    ///
    /// The files are the box's own `git diff` rather than anything the agent reported: an agent that forgets to
    /// mention a file it edited is exactly the case these gates exist for, so what gets checked is what actually
    /// changed on disk.
    pub(crate) fn run_gates_after_turn(self: &Arc<Self>, pane_id: i64, cwd: String) {
        let Some(state) = self.state.lock().unwrap_or_else(|p| p.into_inner()).clone() else { return };
        let reg = self.clone();
        tokio::spawn(async move {
            let Some((pane, _)) = resolve_pane(&state, pane_id).await else { return };
            let settings = serde_json::from_str::<Value>(&s(pane.get("settings_json"))).unwrap_or(json!({}));
            let gates = crate::gates::enabled_on(&settings);
            if gates.is_empty() { return }
            let files = changed_files(&cwd);
            if files.is_empty() {
                reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "gates", "runs": [], "text": "nothing changed in the box this turn — the gates had nothing to check" }), true);
                return;
            }
            reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "gates", "state": "running",
                "text": format!("checking the claim: {} over {} changed file(s)", gates.iter().map(|g| g.key()).collect::<Vec<_>>().join(" · "), files.len()) }), true);
            let mut runs = Vec::new();
            for g in &gates {
                reg.check_start(pane_id, g.key());
                runs.push(crate::gates::run_one(&cwd, *g, &files).await);
                reg.check_end(pane_id, g.key());
            }
            // THE LAST VERDICT STAYS ON DISK (2026-09-24) so a pull request can post it as the forge's own
            // checks. Overwritten each turn: what matters is what the gates say about the code as it stands.
            let _ = std::fs::create_dir_all(Path::new(&cwd).join(".antiloki"));
            let _ = std::fs::write(Path::new(&cwd).join(".antiloki").join("gates.json"),
                                   serde_json::to_string(&runs).unwrap_or_else(|_| "[]".into()));
            // DID THE LAST WORK SURVIVE? (2026-09-25 — the other two survival signals). The re-ask reads the
            // operator's messages; these read the box's own git history, which every repository has whether or not it
            // has a test. Reported, never scored: an operator reverts deliberately, and the file being worked on this
            // week is naturally the file that changes. Silent when there is nothing worth saying.
            if let Some((share, fates)) = crate::survival::look_back(&cwd, 5) {
                let churn = fates.first().map(|f| f.touches).unwrap_or(0);
                if let Some(note) = crate::survival::survival_note(share, churn, 5) {
                    let where_ = fates.first().map(|f| f.path.clone()).unwrap_or_default();
                    reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "survival",
                        "text": if where_.is_empty() { note.clone() } else { format!("{note} ({where_})") } }), true);
                }
            }
            let lines: Vec<String> = runs.iter().map(crate::gates::gate_line).collect();
            let failed = runs.iter().any(|r| r.failed());

            // THE GATES REACH THE BENCHMARK (operator 2026-09-25: "if not our weights on the benchmark only
            // measure speed and cost"). They are the one deterministic quality signal real use produces, and
            // until now they only reached the transcript — the whole record held not a single gated row, so
            // every observed pairing was ranked on the verifier's opinion instead. A skipped gate is neither
            // met nor unmet: "no linter configured" is not a pass and not a failure, and counting it as either
            // would be the same lie in a new column.
            {
                let met = runs.iter().filter(|r| r.skipped.is_none() && r.passed).count() as i64;
                let unmet = runs.iter().filter(|r| r.failed()).count() as i64;
                let (engine, model) = { let l = reg.live.lock().unwrap_or_else(|p| p.into_inner()); l.get(&pane_id).map(|x| (x.engine.clone(), x.model.clone())).unwrap_or_default() };
                if !engine.is_empty() {
                    let domain = crate::roles::domain_of(&files).key().to_string();
                    let _ = crate::merges::record_gates("chat", &engine, if model.is_empty() { None } else { Some(&model) }, &domain, met, unmet);
                }
            }
            reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "gates", "state": "done", "runs": runs,
                "ok": !failed, "text": lines.join(" · ") }), true);
            if !failed { return }
            // one retry per ask: hand the agent the real logs and let it go again
            let Some(brief) = crate::gates::rerun_brief(&runs) else { return };
            let sent = {
                let mut live = reg.live.lock().unwrap_or_else(|p| p.into_inner());
                match live.get_mut(&pane_id) {
                    Some(l) if !l.gate_retry => { l.gate_retry = true; l.busy = true; l.turn_started = now_ms(); l.stdin.send(brief).is_ok() }
                    _ => false,
                }
            };
            reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "gates", "state": if sent { "rerun" } else { "stop" },
                "text": if sent { "a gate said no — the agent has the logs and is going again (once)".to_string() }
                        else { "a gate said no again — antiloki stops here rather than paying for a third try; the logs are above".to_string() } }), true);
        });
    }
    pub fn subscribe(&self, pane_id: i64) -> tokio::sync::broadcast::Receiver<(u64, String)> {
        self.chans.lock().unwrap_or_else(|p| p.into_inner()).entry(pane_id).or_insert_with(|| tokio::sync::broadcast::channel(1024).0).subscribe()
    }
    fn publish(&self, pane_id: i64, seq: u64, line: &str) {
        if let Some(tx) = self.chans.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id) { let _ = tx.send((seq, line.to_string())); }
    }
    pub fn status(&self, pane_id: i64) -> Value {
        let live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        let last_seq = self.seqs.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id).copied().unwrap_or(0);
        match live.get(&pane_id) {
            Some(l) => json!({ "running": true, "busy": l.busy, "session_id": l.session_id, "model": l.model, "cost_usd": l.cost_usd, "started": l.started, "last_seq": last_seq }),
            None => json!({ "running": false, "busy": false, "last_seq": last_seq }),
        }
    }
    pub fn stop(&self, pane_id: i64) -> bool {
        let removed = self.live.lock().unwrap_or_else(|p| p.into_inner()).remove(&pane_id);
        match removed { Some(l) => { l.cancel.store(true, std::sync::atomic::Ordering::SeqCst); if let Some(f) = l.on_stop { f(); } if let Some(pid) = l.child_id { unsafe { libc_kill(pid as i32); } } true }, None => false }
    }
    /// Persist + broadcast one event of ours or the CLI's. `persist: false` = live only (a text delta).
    pub(crate) fn emit(&self, pane_id: i64, ev: Value, persist: bool) {
        // THE COUNTER SEEDS ITSELF, WHEREVER IT IS FIRST NEEDED (2026-09-27).
        //
        // `seed_from_transcript` writes the events file with its OWN numbering, 1..N, and two of its three call
        // sites do not re-seed this counter afterwards — so the first live emit started at 1 again and collided
        // exactly with the imported turns. Pane 25's file held 30 lines carrying 16 distinct seqs: every event
        // twice, the conversation rendered twice, and "2 turns · 3 replies" counted as 4 and 6.
        //
        // Seeding in `set_root` was right and not sufficient, because it only fires on the path that calls it.
        // Asking here instead makes it true for every path, including ones added later: a counter at 0 with a file
        // on disk is always wrong, and this is the moment it is about to matter.
        let seq = if persist {
            let root = self.roots.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id).cloned();
            let mut m = self.seqs.lock().unwrap_or_else(|p| p.into_inner());
            let e = m.entry(pane_id).or_insert(0);
            if *e == 0 { if let Some(r) = &root { *e = seq_on_disk(r, pane_id); } }
            *e += 1; *e
        } else { 0 };
        let line = json!({ "seq": seq, "t": now_ms(), "id": crate::ids::next(crate::ids::pane_event_prefix(&ev)), "ev": ev }).to_string();
        if persist {
            if let Some(root) = self.roots.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id).cloned() {
                let p = events_path(&root, pane_id);
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
                    // APPEND ONTO A LINE BOUNDARY, NOT ONTO A LINE (2026-09-27). A file whose last byte is not a
                    // newline — one written by something that joined its lines, or truncated mid-write by a crash —
                    // takes the next `writeln!` onto the END of its last record. That yields one unparseable line
                    // and LOSES BOTH events: the old one and the new. Found by a test that wrote its fixture with
                    // `join("\n")`, which is exactly the shape a careless writer produces.
                    let needs_nl = std::fs::metadata(&p).ok().map(|m| m.len() > 0).unwrap_or(false)
                        && !std::fs::read(&p).ok().and_then(|b| b.last().copied()).map(|b| b == b'\n').unwrap_or(true);
                    if needs_nl { let _ = write!(f, "\n"); }
                    let _ = writeln!(f, "{line}");
                }
            }
        }
        self.publish(pane_id, seq, &line);
    }
}

/// The pane's MIN, the box's parent key and the tier its model runs at — what the result needs to price a turn.
pub(crate) fn pane_min(pane: &Value) -> bool { pane.get("settings_json").and_then(Value::as_str).and_then(|t| serde_json::from_str::<Value>(t).ok()).and_then(|v| v.get("min").and_then(Value::as_bool)).unwrap_or(false) }
pub(crate) fn pane_parent(pane: &Value) -> String { format!("{}-{}", num(pane.get("session_id")), num(pane.get("id"))) }
/// The tier a session's model stands for — the price a MIN turn is measured at. Pure — tested.
pub fn tier_of_model(engine: &str, model: &str) -> crate::cli::Tier {
    use crate::cli::Tier;
    let m = model.to_ascii_lowercase();
    match engine {
        "claude-code" => if m.contains("opus") { Tier::High } else if m.contains("haiku") { Tier::Low } else { Tier::Mid },
        "codex" => if m.ends_with("· high") || m.contains("xhigh") { Tier::High } else if m.ends_with("· low") { Tier::Low } else { Tier::Mid },
        _ => if m.contains("flash") || m.contains("mini") { Tier::Low } else { Tier::Mid },
    }
}
/// MIN's refusals during a turn: the hook's `min: true` lines in the box's reads log since `since_ms`. Pure over the log — tested.
pub fn min_refusals_since(log: &str, since_ms: i64) -> usize {
    log.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v.get("min") == Some(&json!(true)) && v.get("kind") == Some(&json!("blocked")))
        .filter(|v| v.get("t").and_then(Value::as_str).map(|t| crate::when::parse_iso(t).map(|s| s * 1000).unwrap_or(0) >= since_ms - 1000).unwrap_or(true)).count()
}
impl AgentRegistry {
    /// Every engine's `result` lands here: the turn's tokens price the inline route Heimdall delivered (if one
    /// waits for a number); a MIN turn is recorded as its own settled run (the pill's other column); a lifted
    /// turn puts MIN back on the manifest. `tin/tout` are the priced tokens.
    /// Add one assistant message's usage to the turn's running total (see `Live::turn_tout`).
    pub(crate) fn note_turn_tokens(&self, pane_id: i64, ev: &Value) {
        let (a, b) = turn_tokens(ev);
        if a == 0 && b == 0 { return }
        let mut l = self.live.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(x) = l.get_mut(&pane_id) { x.turn_tin += a; x.turn_tout += b; }
    }
    pub(crate) fn on_turn_result(self: &Arc<Self>, pane_id: i64, cwd: &str, ev: &Value) {
        // ONE CHECKPOINT PER RESPONSE (operator 2026-09-29): when this response left the box different from the agent's last checkpoint (the chat edited files itself), the history
        // gets it. An unchanged box adds nothing; the queue writes its own "after #N" for the runs it started. Off the request path, and never in tests (they run with cwd ".").
        if !cfg!(test) && !cwd.is_empty() && pane_id > 0 {
            let (cwd, pid, me) = (cwd.to_string(), pane_id, self.clone());
            std::thread::spawn(move || {
                let scope = format!("pane-{pid}");
                let before = crate::checkpoint::head(&cwd, &scope);
                let Some(cp) = crate::checkpoint::take_if_changed(&cwd, &scope, "after a chat response", &crate::queue::state_for(pid), now_ms()) else { return };
                if before.as_ref().map(|b| b.n) == Some(cp.n) { return }   // nothing new: the box is as the last checkpoint left it
                let files = before.as_ref().map(|b| crate::checkpoint::changes(&cwd, &b.tree, &cp.tree).len()).unwrap_or(0);
                me.emit(pid, json!({ "type": "antiloki", "subtype": "note", "tone": "dim", "text": format!("▸ checkpoint {} - this response left {} file(s) different in the box", crate::checkpoint::name(&scope, cp.n), files) }), true);
            });
        }
        let (rin, rout) = turn_tokens(ev);
        // THE TURN'S TOKENS, NOT THE LAST MESSAGE'S (operator 2026-09-28: "0 tokens out … wich is a mistake").
        // `result.usage` describes the final assistant message; a turn that read a file, edited it and then said
        // "Added Shoes." reported 22 output tokens. The accumulator has every message's, so the larger of the two
        // is the turn — larger rather than always-the-accumulator, because an engine whose result DOES summarise
        // the whole turn (Codex, OpenCode) must not be halved by a partial sum.
        let (atin, atout) = { let mut l = self.live.lock().unwrap_or_else(|p| p.into_inner());
            match l.get_mut(&pane_id) { Some(x) => { let v = (x.turn_tin, x.turn_tout); x.turn_tin = 0; x.turn_tout = 0; v } None => (0, 0) } };
        let (tin, tout) = (rin.max(atin), rout.max(atout));
        // THE AUDIT OF THIS TURN (2026-09-23): model, deterministic price, timer, tokens — one row, and a line on the
        // pane's timeline under the answer
        {
            // THE ASK COMES WITH IT NOW (II.5): `last_ask` is already on the pane and `classify_ask` is free, so the
            // turn is filed under the job the operator actually asked for instead of the single bucket `chat`.
            let (engine, model, started, ask) = { let l = self.live.lock().unwrap_or_else(|p| p.into_inner()); match l.get(&pane_id) { Some(x) => (x.engine.clone(), x.model.clone(), x.turn_started, x.last_ask.clone()), None => (String::new(), String::new(), 0, String::new()) } };
            if !engine.is_empty() {
                // AS_F64, NOT AS_I64 (operator 2026-09-25: "when i used codex seek on a task the stats detail box
                // showed 0 tokens used even tough is a lie"). Claude's CLI reports usage as JSON integers, so
                // `as_i64` worked and nobody noticed. Codex's numbers reach here as FLOATS — `agent_codex::usage_of`
                // builds them from f64 arithmetic — and `Value::as_i64()` on a float returns None, so every field
                // silently became 0: the turn was recorded with no tokens and no cost, while the result event
                // sitting beside it in the log carried `input_tokens: 1136.0`. Every codex and opencode chat turn
                // this machine has ever taken is a zero row because of this one accessor.
                let u = ev.get("usage").cloned().unwrap_or(Value::Null);
                let n = |k: &str| u.get(k).and_then(Value::as_f64).map(|x| x.round() as i64).unwrap_or(0);
                let call = crate::audit::from_turn_asked(pane_id, &engine, &model, started, n("input_tokens"), n("output_tokens"), n("cache_read_input_tokens"), n("cache_creation_input_tokens"), !ev.get("is_error").and_then(Value::as_bool).unwrap_or(false), &ask);
                // THE SAVING UNDER EVERY ANSWER (operator 2026-09-28: "below each response from the models should appear
                // saved money comparison with frontier opus or codex equivalent — deterministic calculator, not
                // speculation"): the SAME tokens this turn used, priced at the frontier model's list rate
                // (`cli::frontier_of`), beside what the turn really cost. Both numbers are arithmetic on the row.
                let fe = crate::cli::engine_of(&json!(engine)).unwrap_or(crate::cli::Engine::ClaudeCode);
                let (baseline_usd, baseline) = crate::cli::frontier_price(fe, call.tokens_in, call.tokens_out, call.cache_read, call.cache_write);
                self.emit(pane_id, json!({ "type": "antiloki", "subtype": "audit", "model": call.model, "engine": call.engine, "ms": call.ms, "tokens": call.tokens_in + call.tokens_out + call.cache_read + call.cache_write, "cost": call.cost,
                    "baseline": baseline, "baseline_usd": baseline_usd, "saved_usd": (baseline_usd - call.cost).max(0.0) }), true);
            }
        }
        let goals = self.goals.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let Some(g) = goals else { return };
        if let Some((rid, sv)) = g.note_inline_usage_saved(cwd, tin, tout) {
            // THE SAVING RIDES WITH THE TURN (2026-09-26). It was computed here already and only written to the
            // run's log, so the one number the product is sold on reached the operator nowhere near the reply it
            // belonged to. `None` when the turn could not be priced — the screen then says nothing rather than
            // printing a zero it did not measure.
            self.emit(pane_id, json!({ "type": "antiloki", "subtype": "savings", "run": rid, "tokens_in": tin, "tokens_out": tout,
                                       "saved_usd": sv.as_ref().map(|s| s.saved_usd), "actual_usd": sv.as_ref().map(|s| s.actual_usd),
                                       "baseline_usd": sv.as_ref().map(|s| s.baseline_usd), "baseline": sv.as_ref().map(|s| s.baseline.clone()) }), true);
            // QA v2: the inline route's turn is judged too — the box's diff against the ask, this reply as the report
            let reply = ev.get("result").and_then(Value::as_str).unwrap_or("");
            if g.judge_inline_later(rid, reply) { self.emit(pane_id, json!({ "type": "antiloki", "subtype": "qa", "run": rid, "text": "QA judges this turn — the box's diff against the ask" }), true); }
        }
        // THE THREE DETERMINISTIC GATES (operator 2026-09-24). The agent has just claimed the work is done; if the
        // pane has test/build/lint switched on, that claim gets checked against the project's own commands before
        // anybody believes it. Spawned, because a build takes minutes and the event pump must keep pumping.
        if !ev.get("is_error").and_then(Value::as_bool).unwrap_or(false) { self.run_gates_after_turn(pane_id, cwd.to_string()); }
        let (min, lift, engine, model, started, ask, parent) = { let l = self.live.lock().unwrap_or_else(|p| p.into_inner()); match l.get(&pane_id) { Some(x) => (x.min, x.lift, x.engine.clone(), x.model.clone(), x.turn_started, x.last_ask.clone(), x.parent.clone()), None => return } };
        if lift {
            set_manifest_mode(cwd, "min");
            { let mut l = self.live.lock().unwrap_or_else(|p| p.into_inner()); if let Some(x) = l.get_mut(&pane_id) { x.lift = false; } }
            self.emit(pane_id, json!({ "type": "antiloki", "subtype": "min", "state": "back", "text": "MIN is back on — that ask ran with exploration because it named files without intel" }), true);
            return;
        }
        if min && !ev.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
            let Some(eng) = crate::cli::engine_of(&json!(engine)) else { return };
            let log = std::fs::read_to_string(Path::new(cwd).join(".antiloki").join("reads.jsonl")).unwrap_or_default();
            let refusals = min_refusals_since(&log, started);
            if let Some(rid) = g.note_min_turn(&parent, eng, cwd, &ask, tier_of_model(&engine, &model), tin, tout, refusals) { self.emit(pane_id, json!({ "type": "antiloki", "subtype": "savings", "run": rid, "min": true, "tokens_in": tin, "tokens_out": tout, "refusals": refusals }), true); }
        }
    }
}
/// The manifest's `mode` rewritten in place (the hook reads it on every call — no bake, no restart).
pub(crate) fn set_manifest_mode(cwd: &str, mode: &str) -> bool {
    let p = Path::new(cwd).join(".antiloki").join("context").join("manifest.json");
    let Some(mut m) = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return false };
    m["mode"] = json!(mode);
    std::fs::write(&p, serde_json::to_string_pretty(&m).unwrap_or_default()).is_ok()
}

/// The seat a model NAME belongs to, when the name alone says (a GPT id is Codex's, a gemini id is Gemini's ...). `None` for a name that says nothing.
/// Pure - tested.
pub fn engine_of_model_name(model: &str) -> Option<&'static str> {
    let m = model.trim().to_ascii_lowercase();
    if m.is_empty() { return None }
    if m.starts_with("gpt-") || m.starts_with("o3") || m.starts_with("o4") || m.contains("codex") { return Some("codex") }
    if m.starts_with("gemini") { return Some("gemini") }
    if m.starts_with("composer") || m.starts_with("cursor-") { return Some("cursor") }
    if m.starts_with("grok") { return Some("grok") }
    if m.starts_with("claude") || m.starts_with("opus") || m.starts_with("sonnet") || m.starts_with("haiku") { return Some("claude-code") }
    None
}
/// THE USAGE FILE FROM THE STREAM (operator 2026-09-20: "fuel is showing nothing and the usage chip on
/// the agent header is empty"): the TUI's status line wrote `<worktree>/.antiloki/usage.json` on every
/// assistant message; a headless `claude -p` has no status line, so nothing wrote it and `antiloki_usage`
/// said "no usage.json yet". The stream carries the same numbers — each assistant message's context
/// (`message.usage`), each `result`'s session cost and per-model context window, each
/// `rate_limit_event`'s plan windows — so the file is kept from them in the status line's own shape.
/// Read-merge-write: a turn's events each add what they know. Pure on the value; the caller does the file.
pub fn merge_usage(mut u: Value, ev: &Value, now: &str) -> Option<Value> {
    if !u.is_object() { u = json!({}); }
    let o = u.as_object_mut().unwrap();
    let mut touched = false;
    match s(ev.get("type")).as_str() {
        "assistant" => {
            let msg = ev.get("message").cloned().unwrap_or(json!({}));
            let m = s(msg.get("model")); if !m.is_empty() { o.insert("model".into(), json!(m)); }
            if let Some(us) = msg.get("usage").and_then(Value::as_object) {
                let n = |k: &str| us.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                let used = n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens");
                let size = o.get("context").and_then(|c| c.get("size")).and_then(Value::as_f64).filter(|x| *x > 0.0);
                o.insert("context".into(), json!({ "used_pct": size.map(|sz| (used / sz * 100.0 * 10.0).round() / 10.0), "input_tokens": used, "output_tokens": n("output_tokens"), "size": size }));
                let msgs = o.get("session").and_then(|x| x.get("messages")).and_then(Value::as_u64).unwrap_or(0) + 1;
                let mut sess = o.get("session").cloned().unwrap_or(json!({})); if let Some(so) = sess.as_object_mut() { so.insert("messages".into(), json!(msgs)); }
                o.insert("session".into(), sess);
                touched = true;
            }
        }
        "result" => {
            if let Some(c) = ev.get("total_cost_usd").and_then(Value::as_f64) { o.insert("cost_usd".into(), json!((c * 10000.0).round() / 10000.0)); touched = true; }
            if let Some(us) = ev.get("usage").and_then(Value::as_object) {
                let n = |k: &str| us.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                let mut sess = o.get("session").cloned().unwrap_or(json!({}));
                if let Some(so) = sess.as_object_mut() {
                    so.insert("input_tokens".into(), json!(n("input_tokens"))); so.insert("output_tokens".into(), json!(n("output_tokens")));
                    so.insert("cached_input_tokens".into(), json!(n("cache_read_input_tokens")));
                    so.insert("tokens_total".into(), json!(n("input_tokens") + n("output_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens")));
                }
                o.insert("session".into(), sess); touched = true;
            }
            // the context window of the model that answered — the pct the chip shows needs it
            let model = s(o.get("model"));
            if let Some(mu) = ev.get("modelUsage").and_then(Value::as_object) {
                let win = mu.get(&model).or_else(|| mu.values().max_by_key(|m| m.get("outputTokens").and_then(Value::as_u64).unwrap_or(0))).and_then(|m| m.get("contextWindow")).and_then(Value::as_f64);
                if let Some(sz) = win {
                    let mut ctx = o.get("context").cloned().unwrap_or(json!({}));
                    if let Some(co) = ctx.as_object_mut() {
                        co.insert("size".into(), json!(sz));
                        if let Some(used) = co.get("input_tokens").and_then(Value::as_f64) { co.insert("used_pct".into(), json!((used / sz * 1000.0).round() / 10.0)); }
                    }
                    o.insert("context".into(), ctx); touched = true;
                }
            }
        }
        "rate_limit_event" => {
            let info = ev.get("rate_limit_info").cloned().unwrap_or(json!({}));
            let wins = info.get("unifiedWindows").cloned().unwrap_or(json!({}));
            let mut limits = vec![];
            for (key, label) in [("five_hour", "5h"), ("seven_day", "week")] {
                if let Some(w) = wins.get(key) {
                    let pct = w.get("utilization").and_then(Value::as_f64).map(|x| (x * 1000.0).round() / 10.0);
                    limits.push(json!({ "label": label, "used_pct": pct, "resets_at": w.get("resetsAt").cloned().unwrap_or(Value::Null) }));
                }
            }
            if !limits.is_empty() { o.insert("limits".into(), json!(limits)); touched = true; }
        }
        _ => {}
    }
    if !touched { return None; }
    // WHO ANSWERED IS WHAT THE EVENT SAYS, THEN WHAT ITS MODEL IS, THEN WHAT THE PANE ALREADY KNEW (operator 2026-09-30: the usage box read "Claude · gpt-5.6-terra" - a
    // Codex turn's assistant event names a model and no engine, and this defaulted the missing engine to claude-code, so a GPT model was labelled with Anthropic's name).
    let engine = s(ev.get("engine"));
    let engine = if !engine.is_empty() { engine } else if let Some(e) = engine_of_model_name(&s(o.get("model"))) { e.to_string() } else { let prior = s(o.get("engine")); if prior.is_empty() { "claude-code".to_string() } else { prior } };
    o.insert("source".into(), json!(if engine == "claude-code" { "claude -p stream (result · assistant usage · rate_limit_event)" } else { "the chat session's stream (result · assistant usage · rate limits)" }));
    o.insert("engine".into(), json!(engine)); o.insert("t".into(), json!(now));
    if !o.contains_key("limits") { o.insert("limits".into(), json!([])); }
    Some(u)
}
/// A turn's tokens as PRICED input (cache reads at a tenth, cache writes at 1.25×, list-price ratios) and
/// output — the number the inline saving is measured on. Pure.
pub fn turn_tokens(result: &Value) -> (i64, i64) {
    let us = result.get("usage").cloned().unwrap_or(json!({}));
    let n = |k: &str| us.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    ((n("input_tokens") + n("cache_creation_input_tokens") * 1.25 + n("cache_read_input_tokens") * 0.1).round() as i64, n("output_tokens").round() as i64)
}
/// The meter file for ONE pane. Same story as the event log above: `usage.json` was per-WORKTREE, and several
/// agents share a worktree, so each one overwrote the last and the header summed the same file once per pane —
/// six claude agents on one box reported six times one agent's spend, and every agent's chip showed whoever
/// wrote last. Per pane, and no fallback to the shared file: a stranger's spend is not this agent's fuel.
pub fn usage_path(root: &Path, pane_id: i64) -> std::path::PathBuf {
    root.join(".antiloki").join(format!("usage-{pane_id}.json"))
}
/// A pane that talked before this serve knew to write the file: replay its events once.
fn backfill_usage(cwd: &Path, pane_id: i64) {
    let p = usage_path(cwd, pane_id);
    if p.exists() { return; }
    let (events, _) = backlog(&cwd.to_string_lossy(), pane_id, 0, 100_000);
    let now = antiloki_rs::clock::now_iso_nodb();
    let mut u = json!({});
    for w in &events { if let Some(ev) = w.get("ev") { if let Some(n) = merge_usage(u.clone(), ev, &now) { u = n; } } }
    if u.get("t").is_some() { let _ = std::fs::write(&p, u.to_string()); }
}
/// `<worktree>/.antiloki/usage-<pane_id>.json` ← the event, if it carried anything.
pub fn note_usage(cwd: &Path, pane_id: i64, ev: &Value) {
    let p = usage_path(cwd, pane_id);
    let cur = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or(json!({}));
    let now = antiloki_rs::clock::now_iso_nodb();
    if let Some(next) = merge_usage(cur, ev, &now) { let _ = std::fs::create_dir_all(p.parent().unwrap()); let _ = std::fs::write(&p, next.to_string()); }
}

#[cfg(unix)]
unsafe fn libc_kill(pid: i32) { extern "C" { fn kill(pid: i32, sig: i32) -> i32; } kill(pid, 15); }
#[cfg(not(unix))]
unsafe fn libc_kill(_pid: i32) {}

/// WHERE ONE AGENT'S EVENTS LIVE. Per PANE since 2026-09-24: every agent of a workspace appended to a single
/// `<worktree>/.antiloki/agent-events.jsonl`, and they all read it back. So each agent replayed the others' turns —
/// a freshly made agent opened showing the previous agent's transcript, and its own work never appeared in its own
/// chat. The sequence counters were already per pane, so the shared file also interleaved two numbering schemes.
///
/// New events always go to the pane's own file. Reading falls back to the legacy shared file when a pane has none
/// yet, so the history already on disk is not thrown away — from the next event on, the agents diverge correctly.
pub fn events_path(root: &str, pane_id: i64) -> std::path::PathBuf {
    let dir = Path::new(root).join(".antiloki");
    dir.join(format!("agent-events-{pane_id}.jsonl"))
}
/// What actually changed in the box, tracked or not — the gates check the disk, not the agent's account of it.
/// An untracked new file is a change; a `git` that is not there or a box that is not a repo means no list, and
/// the gates say they had nothing to check rather than checking the whole tree.
/// The files antiloki itself puts in a repo — never the operator's change, never a gate's business. Kept in step
/// with the scope sentry's own rule (src/skills/agents/in/pane-scope-sentry.rs).
pub fn ours(path: &str) -> bool {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r#"^(AGENTS\.md|CLAUDE\.md|opencode\.json|\.claude/|\.codex/|\.opencode/|\.agents/|\.antiloki(-[A-Za-z0-9._-]+)?/)"#).unwrap()).is_match(path)
}
/// The asks this pane has already had, newest first — read from its own event log, which is the record the
/// transcript is drawn from, so the detector sees exactly what the operator saw. Cheap: the tail of one file.
pub fn recent_asks(cwd: &str, pane_id: i64, n: usize) -> Vec<String> {
    let path = events_read_path(cwd, pane_id);
    let Ok(body) = std::fs::read_to_string(&path) else { return vec![] };
    asks_in_log(&body, n)
}

/// The operator's own asks in an event log, newest first. A user event carrying a `source` (Heimdall's consolidated goal is logged
/// as `source: "heimdall"`) is the system's rewrite of an ask, not another ask: counting it made every routed ask "asked again"
/// against the rewrite of ITSELF (seen live on run #363, 2026-10-10: "72 % like the ask 1 turn ago"), and a false repeat lowers the
/// first seat's routing weight. Pure - tested.
pub fn asks_in_log(body: &str, n: usize) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for line in body.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let ev = v.get("ev").unwrap_or(&v);
        if ev.get("subtype").and_then(Value::as_str) == Some("user") && ev.get("source").is_none() {
            if let Some(t) = ev.get("text").and_then(Value::as_str) { out.push(t.to_string()); }
            if out.len() >= n { break }
        }
    }
    out
}

pub fn changed_files(cwd: &str) -> Vec<String> {
    let out = crate::proc::cmd("git").args(["status", "--porcelain", "--untracked-files=all"]).current_dir(cwd).output();
    let Ok(o) = out else { return vec![] };
    if !o.status.success() { return vec![] }
    String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| {
        if l.len() < 4 { return None }
        let path = l[3..].trim();
        // a rename reads `R  old -> new`; the new name is the one that exists to be tested
        let path = path.rsplit(" -> ").next().unwrap_or(path).trim_matches('"');
        // antiloki's own bookkeeping is never the operator's change. The same list the scope sentry keeps, and for
        // the same reason: AGENTS.md, CLAUDE.md, the engines' config directories and `.antiloki*` are files WE
        // write into someone's repo, so a gate must not lint them, test them, or blame a turn for them.
        if path.is_empty() || ours(path) { return None }
        Some(path.to_string())
    }).collect()
}

/// The file to READ for this pane: its own, and ONLY its own.
///
/// There is deliberately no fallback to the pre-2026-09-24 shared `agent-events.jsonl`. That file is a braid of
/// every agent that ever ran on the workspace, and nothing in it says which turn belonged to whom — so handing it
/// to each agent as "your history" is the bug restated rather than fixed, and it is exactly what made eight
/// different agents all open showing the same TypeScript review. The file stays on disk and nothing is deleted;
/// no agent claims it. Each one shows its own work from its first event onward.
pub fn events_read_path(root: &str, pane_id: i64) -> std::path::PathBuf { events_path(root, pane_id) }

/// The backlog: persisted events after `after`, oldest first (bounded by `limit`), and the last seq.
pub fn backlog(root: &str, pane_id: i64, after: u64, limit: usize) -> (Vec<Value>, u64) {
    let p = events_read_path(root, pane_id);
    let text = std::fs::read_to_string(&p).unwrap_or_default();
    let mut out = vec![]; let mut last = after;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let seq = v.get("seq").and_then(Value::as_u64).unwrap_or(0);
        if seq <= after { continue; }
        last = last.max(seq); out.push(v);
    }
    if out.len() > limit { let cut = out.len() - limit; out.drain(0..cut); }
    (out, last)
}
/// The seq the events file is at — so a serve restart continues the numbering instead of restarting at 1.
pub(crate) fn seq_on_disk(root: &str, pane_id: i64) -> u64 { backlog(root, pane_id, 0, 1).1 }
/// A pane that lived in the TUI has its conversation in Claude's own transcript, not in our events file:
/// the first time the chat opens, the transcript's turns are written as events so the switch is not a
/// blank screen (the session resumes the same conversation underneath).
pub fn seed_from_transcript(root: &str, pane_id: i64) -> usize {
    let p = events_path(root, pane_id);
    if p.exists() { return 0; }
    let (_, turns) = antiloki_rs::cockpit__helpers_engine_handoff::claude_transcript(Path::new(root));
    if turns.is_empty() { return 0; }
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) else { return 0 };
    let t = now_ms();
    let mut n = 0u64;
    n += 1; let _ = writeln!(f, "{}", json!({ "seq": n, "t": t, "ev": { "type": "antiloki", "subtype": "imported", "turns": turns.len() } }));
    for turn in &turns {
        n += 1;
        let ev = if turn.role == "user" { json!({ "type": "antiloki", "subtype": "user", "text": turn.text }) } else {
            // the transcript reader wrote each tool use as a `[tool Name input]` line — back into tool blocks
            let mut blocks: Vec<Value> = vec![]; let mut text = String::new();
            for line in turn.text.lines() {
                if let Some(rest) = line.strip_prefix("[tool ").and_then(|r| r.strip_suffix(']')) {
                    if !text.trim().is_empty() { blocks.push(json!({ "type": "text", "text": text.trim() })); text.clear(); }
                    let (name, input) = rest.split_once(' ').unwrap_or((rest, ""));
                    blocks.push(json!({ "type": "tool_use", "id": format!("imported-{n}-{}", blocks.len()), "name": name, "input": { "raw": input, "imported": true } }));
                } else { text.push_str(line); text.push('\n'); }
            }
            if !text.trim().is_empty() { blocks.push(json!({ "type": "text", "text": text.trim() })); }
            json!({ "type": "assistant", "message": { "role": "assistant", "content": blocks } })
        };
        let _ = writeln!(f, "{}", json!({ "seq": n, "t": t, "ev": ev }));
    }
    n as usize
}

/// The pane row and its box, resolved like the PTY does.
async fn resolve_pane(state: &Arc<AppState>, pane_id: i64) -> Option<(Value, String)> {
    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
    let pane = engine_call(state, "system.cockpit.pane.get", json!({ "id": pane_id }), ctx).await.ok().map(|(v, _)| v).filter(|v| !v.is_null())?;
    let cwd = pane.get("worktree_path").and_then(Value::as_str).map(String::from)?;
    Some((pane, cwd))
}

/// Start (or restart) the pane's session. `model`: the tier's model (`sonnet` / `opus`); `fresh`: a new
/// conversation on purpose — otherwise the pinned session resumes when its transcript exists.
/// The model a pane was made to start on (`settings_json.model`, a Studio base agent's preset) — None when unset or blank. Pure.
pub fn preset_model(pane: &Value) -> Option<String> {
    pane.get("settings_json").and_then(Value::as_str).and_then(|t| serde_json::from_str::<Value>(t).ok())
        .and_then(|v| v.get("model").and_then(Value::as_str).map(str::trim).filter(|m| !m.is_empty()).map(String::from))
}
/// A MODEL ONLY TRAVELS WITH ITS OWN SEAT. `Some(model)` when this engine can be asked for it, the engine's low
/// rung when the pane is carrying another engine's model (a switch, or the cheap-rung migration), `None` when the
/// engine has no opinion — the CLI's own default then stands. Pure — tested.
pub fn model_for_seat(engine: Option<crate::cli::Engine>, model: String, tier: Option<crate::cli::Tier>) -> Option<String> {
    let Some(e) = engine else { return Some(model) };
    if model.trim().is_empty() { return None }
    if crate::cli::model_blocked(e, Some(&model)).is_none() && crate::cli::model_is_of(e, &model) { return Some(model) }
    // THE TIER THE CALLER ASKED FOR SURVIVES THE SWAP (audit 2026-09-28). This fell to the LOW rung whatever was
    // wanted, so a router that asked for a high-tier brain and named a model this seat could not place got the
    // cheap rung instead — while the record, the audit and the savings line all filed the result under the tier
    // that was asked for. A Low-rung result attributed to a High pick is worse than no result.
    crate::cli::tier_model(e, tier.unwrap_or(crate::cli::Tier::Low))
}

/// Clears the pane's start-in-flight mark however `start` returns — including on an early `?`.
struct StartGuard { reg: Arc<AgentRegistry>, pane_id: i64, armed: bool }
impl Drop for StartGuard {
    fn drop(&mut self) {
        if self.armed { self.reg.starting.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.pane_id); }
    }
}

/// THE PERMISSION MODE A PANE RUNS UNDER (operator 2026-09-28: "I FUCKING TOLD YOU ALL AGENTS CAN EDIT NOW").
///
/// This said `"auto"`, which is **not one of Claude Code's modes** — they are `default`, `acceptEdits`,
/// `bypassPermissions` and `plan`. An unrecognised mode falls back to `default`, which ASKS before every write;
/// a headless `-p` session has nobody to ask, so the tool answered "Claude requested permissions to write to
/// src/products.ts, but you haven't granted it yet" and the turn ended having changed nothing. Every flag around
/// it said the pane could write. It could not, and had not been able to for as long as this line has existed.
///
/// `acceptEdits` is the right mode for a writable pane and not a loosening: antiloki's boundary is the SCOPE HOOK
/// (`PreToolUse` → `antiloki-agent hook`), which refuses a write outside the pane's globs before the file changes.
/// That is the product's stated design — "a write outside the files you allowed is refused by the tool before the
/// file changes, not by a sentence in the prompt" — and it cannot work if the tool never gets to run at all.
///
/// MAX keeps `bypassPermissions` (the operator's explicit "this agent can do anything"), and a read-only pane is
/// put in `plan`, which cannot write by construction rather than by refusal. Pure — tested.
pub fn permission_mode(max_mode: bool, readonly: bool) -> &'static str {
    if readonly { return "plan" }
    if max_mode { "bypassPermissions" } else { "acceptEdits" }
}

/// THE SEAT A PANE ACTUALLY RUNS ON, whatever its `pane_kind` says (found 2026-09-28).
///
/// A fork was created with `pane_kind = "agent"` — the word, not a seat — and a pane like that is unstartable and
/// unswitchable for ever, because the switch itself has to start it. Rather than refuse, the kind is read from the
/// next best thing the pane knows: its own settings, then its parent's seat is not available here so the Heimdall
/// seat stands in, then Claude Code. `None` only when nothing at all can run. Pure — tested.
pub fn engine_for_pane(pane: &Value, kind: String) -> Option<crate::cli::Engine> {
    if let Some(e) = crate::cli::engine_of(&json!(kind)) { return Some(e) }
    let settings: Value = pane.get("settings_json").and_then(Value::as_str).and_then(|t| serde_json::from_str(t).ok()).unwrap_or(json!({}));
    for k in ["engine", "provider"] {
        if let Some(e) = settings.get(k).and_then(|v| crate::cli::engine_of(v)) { return Some(e) }
    }
    // THE LAST RESORT HAS TO BE USABLE, OR THE `None` ARM IS A LIE (audit 2026-09-28). This ended in
    // `.or(Some(ClaudeCode))`, so the function could never answer `None` and the caller's error branch was dead
    // code — a pane with a nonsense kind was silently rewritten to Claude Code and persisted, even on a machine
    // with no Claude seat at all. It answers honestly now, and the caller's refusal is a real path.
    crate::cli::heimdall_engine(None)
        .or_else(|| CHAT_ENGINES_FALLBACK.iter().copied().find(|e| crate::cli::engine_usable(*e)))
}

/// the seats a repaired pane may be handed, cheapest first — the same order the router prefers
const CHAT_ENGINES_FALLBACK: [crate::cli::Engine; 6] = crate::chat_pane::CHAT_ENGINES;

pub async fn start(state: &Arc<AppState>, reg: &Arc<AgentRegistry>, pane_id: i64, model: Option<String>, tier: Option<crate::cli::Tier>, fresh: bool) -> Result<Value, String> {
    // ONE START PER PANE AT A TIME (operator 2026-09-28: "it is always exiting on the first try"). A start is not
    // in `live` until it finishes, so a send arriving in that window saw "not running" and started a second —
    // and the second `start()` stops the pane first, SIGTERMing the one that was almost up. The pane recovered
    // and the thread showed the corpse. The loser now backs off instead of killing the winner. `fresh` is the
    // deliberate restart (a provider switch, a new episode) and always proceeds.
    if !fresh && !reg.starting.lock().unwrap_or_else(|p| p.into_inner()).insert(pane_id) {
        return Ok(json!({ "ok": true, "pane_id": pane_id, "already_starting": true }));
    }
    let _guard = StartGuard { reg: reg.clone(), pane_id, armed: !fresh };
    let (pane, cwd) = resolve_pane(state, pane_id).await.ok_or_else(|| format!("pane {pane_id} not found or has no box yet"))?;
    { let c = cwd.clone(); let _ = tokio::task::spawn_blocking(move || crate::boxes::heal_in_place(&c)).await; }   // an empty box from before seeding existed is healed where its work starts
    // A BOX WITHOUT ITS GIT LINK IS NOT A PLACE TO WORK (Grokbot QA 2026-10-12, item 7: the worktree was "prunable" and its folder gone, yet "hello" was answered and nothing in the repo changed). Starting the session
    // there recreated the folder as a plain empty directory (the launch record makes `.antiloki/` inside it) and the engine worked in a void. Say so instead, and let the operator fork the agent again.
    if let Some(msg) = missing_box_error(&cwd, &crate::boxes::boxes_dir()) { return Err(msg); }
    let engine_key = s(pane.get("pane_kind"));
    // EVERY SEAT IS A CHAT NOW (operator 2026-09-27, the five-seat pivot). Cursor and Grok print one NDJSON turn
    // per process in Claude Code's own envelope (`agent_print`); Antigravity holds one process and speaks its own
    // (`agent_gemini`). Before this the pane ran three of the five seats and the landing page promised all five.
    //
    // …AND A PANE WHOSE KIND IS NOT AN ENGINE HEALS ITSELF (found 2026-09-28: the fork pane 50 was created with
    // `pane_kind = "agent"`, which is not a seat, so it could never start and could never be switched — the switch
    // itself goes through here). The kind is repaired from what the pane can tell us rather than failing.
    let engine_key = match engine_for_pane(&pane, engine_key.clone()) {
        Some(e) if e.key() == engine_key => engine_key,
        Some(e) => {
            reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "note", "tone": "dim",
                "text": format!("this pane's engine was recorded as `{engine_key}`, which is not a seat — repaired to {}", e.key()) }), true);
            let _ = engine_call(state, "system.cockpit.pane.update", json!({ "pane_id": pane_id, "pane_kind": e.key() }), crate::chat_pane::ctx()).await;
            e.key().to_string()
        }
        None => return Err(format!("{engine_key} is not an engine antiloki knows, and no seat is usable here")),
    };
    reg.stop(pane_id);
    reg.roots.lock().unwrap_or_else(|p| p.into_inner()).insert(pane_id, cwd.clone());
    if engine_key == "claude-code" { seed_from_transcript(&cwd, pane_id); }
    { let mut m = reg.seqs.lock().unwrap_or_else(|p| p.into_inner()); let e = m.entry(pane_id).or_insert(0); if *e == 0 { *e = seq_on_disk(&cwd, pane_id); } }
    // A SWITCHED-OFF SEAT IS NEVER STARTED (chat_pane::leave_blocked_seat, operator 2026-09-27: "i send a message
    // and nothing happens"): main stood on OpenCode with Zen off, so every resume and every send started a session
    // that could only answer 402. The pane moves to a seat that can run, and that rotate's start is this start.
    // After the seq counter above is primed from disk — an event emitted before that would restart the numbering
    // and the thread, which reads seqs as time, would drop it.
    if let Some(moved) = crate::chat_pane::leave_blocked_seat(state, reg, &pane).await { return moved; }
    // the context pack, baked before the CLI boots — exactly like the PTY
    let readonly = crate::chat_pane::is_readonly(&pane);
    if readonly { crate::chat_pane::harden(Path::new(&cwd)); }
    let pack = engine_call(state, "system.cockpit.pane.context.bake", json!({ "pane_id": pane_id }), json!({ "actor": "web-fe", "source": "web-fe" })).await.map(|(v, _)| v).unwrap_or(Value::Null);
    if readonly { crate::chat_pane::harden(Path::new(&cwd)); } // the bake sets `bash` — a read-only pane denies it
    // A BASE AGENT'S MODEL (2026-09-23: the Studio's DeepSeek · Kimi · Qwen tiles): the pane's `settings.model`
    // is the model it starts on when the caller names none — the router's pick still wins when it names one
    // …AND A MODEL BELONGS TO ITS SEAT (found 2026-09-28 switching a pane to Cursor: the pane carried `model:
    // "haiku"` from the cheap-rung migration, and haiku is not a Cursor model — `model_blocked` would have refused
    // the call, or worse, Cursor would have billed it against the $20 third-party allowance). A preset that does
    // not belong to this engine is dropped for the engine's own low rung, which is what "cheap rung" meant.
    let engine_now = crate::cli::engine_of(&json!(engine_key));
    let model = model.or_else(|| preset_model(&pane)).and_then(|m| model_for_seat(engine_now, m, tier));
    // OPENCODE (2026-09-20): the same session model over `opencode serve` — see agent_opencode.rs
    if engine_key == "opencode" { return crate::agent_opencode::start(reg, pane_id, &pane, &cwd, model, fresh).await; }
    // CODEX (2026-09-20): the same session model over `codex app-server` — see agent_codex.rs (the tier is its effort)
    if engine_key == "codex" { return crate::agent_codex::start(reg, pane_id, &pane, &cwd, &pack, tier, fresh).await; }
    // GEMINI (2026-09-28): `agy --input-format stream-json` — one process, a turn per line, its own event envelope
    if engine_key == "gemini" { return crate::agent_gemini::start(reg, pane_id, &pane, &cwd, model, fresh).await; }
    // CURSOR and GROK (2026-09-28): a process per ask, resumed by session id — see agent_print.rs
    if let Some(e) = crate::cli::engine_of(&json!(engine_key)).filter(|e| crate::agent_print::drives(*e)) {
        return crate::agent_print::start(reg, pane_id, &pane, &cwd, e, model, fresh).await;
    }
    // MAX (operator 2026-09-20: "this agent can do anything to any file — all permissions"): the pane's
    // `settings.max` lifts the permission mode to bypass; the scope hook was widened to ** by the same click
    let max_mode = pane.get("settings_json").and_then(Value::as_str).and_then(|t| serde_json::from_str::<Value>(t).ok()).and_then(|v| v.get("max").and_then(Value::as_bool)).unwrap_or(false);
    let mut args: Vec<String> = ["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--include-partial-messages", "--permission-mode", permission_mode(max_mode, readonly)].iter().map(|x| x.to_string()).collect();
    args.extend(crate::agents_http::context_args_for(&engine_key, &pack, &crate::agents_http::Delegation::of_pane(&pane)));
    // A READ-ONLY PANE (chat_pane.rs::is_readonly): read-only at the tool layer, the workspace readable
    if readonly { args.extend(crate::chat_pane::claude_readonly_args(&crate::chat_pane::workspace_root_of(state))); }
    if let Some(m) = &model { args.push("--model".into()); args.push(m.clone()); }
    // the pinned session: resume when its transcript exists, else a new pinned id (the handoff reads the same file)
    let prior = antiloki_rs::cockpit__helpers_engine_handoff::pty_session(Path::new(&cwd)).filter(|(e, _)| e == "claude-code").map(|(_, id)| id);
    // A RESTART INTO A NEW BOX KEEPS THE CONVERSATION: with no session of its own, a pane takes the newest one of its earlier boxes (its own, proven by its pane number)
    let prior = prior.filter(|id| crate::pty::resume_transcript_exists(&cwd, id)).or_else(|| if fresh { None } else { crate::cowork::adopt_earlier_session(&cwd, pane_id) });
    let resumable = prior.filter(|id| !fresh && crate::pty::resume_transcript_exists(&cwd, id));
    let (sid, resumed) = match resumable {
        Some(id) => { args.push("--resume".into()); args.push(id.clone()); (id, true) }
        None => { let sid = antiloki_rs::cockpit__helpers_engine_handoff::session_uuid(&format!("agent {pane_id} · {} · {}", now_ms(), std::process::id())); args.push("--session-id".into()); args.push(sid.clone()); (sid, false) }
    };
    let _ = std::fs::create_dir_all(Path::new(&cwd).join(".antiloki"));
    let _ = std::fs::write(Path::new(&cwd).join(".antiloki").join("pty-session.json"), json!({ "engine": "claude-code", "session_id": sid, "t": now_ms(), "ui": "chat" }).to_string());
    crate::agents_http::write_launch_record(&cwd, "claude", &args, &HashMap::new());

    backfill_usage(Path::new(&cwd), pane_id);
    let mut cmd = crate::proc::tokio_cmd("claude");
    cmd.args(&args).current_dir(&cwd).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(false);
    cmd.env_remove("CLAUDECODE"); // a nested Claude would refuse to start inside another's env
    let mut child = cmd.spawn().map_err(|e| format!("claude: {e}"))?;
    let child_id = child.id();
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    reg.live.lock().unwrap_or_else(|p| p.into_inner()).insert(pane_id, Live { stdin: tx, child_id, engine: "claude-code".into(), cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)), on_stop: None, session_id: sid.clone(), model: model.clone().unwrap_or_default(), busy: false, cost_usd: 0.0, started: now_ms(), min: pane_min(&pane), lift: false, turn_started: 0, last_ask: String::new(), parent: pane_parent(&pane), turn_tin: 0, turn_tout: 0, gate_retry: false, paused: false });
    reg.emit(pane_id, json!({ "type": "antiloki", "subtype": if resumed { "resumed" } else { "started" }, "session_id": sid, "model": model, "pid": child_id, "engine": "claude-code" }), true);
    // stdin pump
    tokio::spawn(async move { while let Some(text) = rx.recv().await { let line = json!({ "type": "user", "message": { "role": "user", "content": text } }).to_string(); if stdin.write_all(line.as_bytes()).await.is_err() { break; } if stdin.write_all(b"\n").await.is_err() { break; } let _ = stdin.flush().await; } });
    // stderr → one event per non-empty line (the CLI's own complaints: login, unknown flag, …)
    if let Some(err) = stderr { let (reg2, pid) = (reg.clone(), pane_id); tokio::spawn(async move { let mut lines = tokio::io::BufReader::new(err).lines(); while let Ok(Some(l)) = lines.next_line().await { let t = l.trim(); if !t.is_empty() && !t.starts_with("mise ") { reg2.emit(pid, json!({ "type": "antiloki", "subtype": "stderr", "text": t.chars().take(600).collect::<String>() }), true); } } }); }
    // stdout → events
    let (reg2, pid, cwd2) = (reg.clone(), pane_id, cwd.clone());
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            let Ok(ev) = serde_json::from_str::<Value>(&l) else { continue };
            let ty = s(ev.get("type"));
            if matches!(ty.as_str(), "assistant" | "result" | "rate_limit_event") { note_usage(Path::new(&cwd2), pid, &ev); }
            if ty == "assistant" { reg2.note_turn_tokens(pid, &ev); }
            match ty.as_str() {
                "stream_event" => reg2.emit(pid, ev, false),
                "result" => {
                    let cost = ev.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
                    { let mut live = reg2.live.lock().unwrap_or_else(|p| p.into_inner()); if let Some(x) = live.get_mut(&pid) { x.busy = false; x.cost_usd = cost; } }
                    reg2.on_turn_result(pid, &cwd2, &ev);
                    reg2.emit(pid, ev, true);
                }
                "system" => {
                    if s(ev.get("subtype")) == "init" { let m = s(ev.get("model")); let mut live = reg2.live.lock().unwrap_or_else(|p| p.into_inner()); if let Some(x) = live.get_mut(&pid) { if !m.is_empty() { x.model = m; } } }
                    reg2.emit(pid, ev, true);
                }
                _ => reg2.emit(pid, ev, true),
            }
        }
        // EOF: the process is gone
        let code = child.wait().await.ok().and_then(|st| st.code());
        // A SUPERSEDED CHILD IS NOT AN EVENT (operator 2026-09-28: "it is always exiting on the first try").
        //
        // `start()` stops the pane before it spawns, so the process it replaces dies of SIGTERM and exits 143 —
        // and this wrote "the agent's process exited (143)" into the thread every time, on a perfectly healthy
        // start. `was` already knows the difference and was computed and then ignored: if this child is no longer
        // the pane's, something newer is already running and the operator has nothing to be told.
        let was = reg2.live.lock().unwrap_or_else(|p| p.into_inner()).get(&pid).map(|x| x.child_id) == Some(child_id);
        if was {
            reg2.live.lock().unwrap_or_else(|p| p.into_inner()).remove(&pid);
            reg2.emit(pid, json!({ "type": "antiloki", "subtype": "exited", "code": code, "cwd": cwd2 }), true);
        }
    });
    Ok(json!({ "ok": true, "pane_id": pane_id, "session_id": sid, "resumed": resumed, "model": model, "pid": child_id }))
}

/// `POST /api/v1/cockpit/agent/start {pane_id, model?, fresh?}`
pub async fn agent_start(state: Arc<AppState>, reg: Arc<AgentRegistry>, body: Bytes) -> Response {
    let b = body_json(&body);
    match start(&state, &reg, num(b.get("pane_id")), b.get("model").and_then(Value::as_str).map(String::from).filter(|m| !m.is_empty()), b.get("tier").and_then(Value::as_str).and_then(crate::cli::Tier::of), b.get("fresh").and_then(Value::as_bool).unwrap_or(false)).await {
        Ok(v) => (StatusCode::CREATED, Json(v)).into_response(),
        Err(e) => (StatusCode::CREATED, Json(json!({ "ok": false, "error": e }))).into_response(),
    }
}
/// `POST /api/v1/cockpit/agent/send {pane_id, text, model?}` — starts the session if needed; a different
/// `model` restarts it on the resumed conversation first (the router's tier → sonnet / opus).
/// IS THIS AGENT'S BOX GONE? A folder under the agents' box directory that has no `.git` (a worktree's is a file) is a box whose worktree was removed or never cut; anything outside that directory (the shared
/// tree, a plain project folder) is not judged here. The message names the way out. Pure - tested.
pub fn missing_box_error(cwd: &str, boxes: &std::path::Path) -> Option<String> {
    let p = std::path::Path::new(cwd);
    if p.starts_with(boxes) && !p.join(".git").exists() {
        Some(format!("this agent's box {cwd} is missing (its git worktree is gone). Fork the agent again, and check that your home folder (or ANTILOKI_HOME) is kept between runs"))
    } else { None }
}

/// Does a send need a (re)start first? Never started: yes. Running on a seat that was switched off since: yes. Running on another model than the one asked for: yes. Pure - tested.
pub fn needs_start(live_model: Option<&str>, wanted: Option<&str>, seat_off: bool) -> bool {
    match (live_model, wanted) {
        (None, _) => true,
        _ if seat_off => true,
        (Some(cur), Some(m)) => !cur.starts_with(m) && !cur.contains(m),
        _ => false,
    }
}
pub async fn agent_send(state: Arc<AppState>, reg: Arc<AgentRegistry>, body: Bytes) -> Response {
    let b = body_json(&body);
    let pane_id = num(b.get("pane_id"));
    let text = s(b.get("text"));
    let mut model = b.get("model").and_then(Value::as_str).map(String::from).filter(|m| !m.is_empty());
    let tier = b.get("tier").and_then(Value::as_str).and_then(crate::cli::Tier::of);
    // a TIER instead of a model (the router's verdict for a non-Claude pane): the engine's model for it —
    // Codex has no model per tier (the subscription's own): the tier is the next turn's reasoning effort
    if model.is_none() {
        if let Some(tier) = tier {
            let engine = reg.live.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id).map(|l| l.engine.clone());
            let engine = match engine { Some(e) => Some(e), None => resolve_pane(&state, pane_id).await.map(|(p, _)| s(p.get("pane_kind"))) };
            if engine.as_deref() == Some("codex") && crate::agent_codex::set_effort(pane_id, tier) {
                let label = { let mut l = reg.live.lock().unwrap_or_else(|p| p.into_inner()); l.get_mut(&pane_id).map(|x| crate::agent_codex::model_label(&x.model, Some(crate::agent_codex::effort_of(tier)))) };
                reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "model", "model": label, "effort": crate::agent_codex::effort_of(tier) }), true);
            }
            model = engine.and_then(|e| crate::cli::engine_of(&json!(e))).and_then(|e| crate::cli::tier_model(e, tier));
        }
    }
    let current = reg.live.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id).map(|l| (l.model.clone(), true));
    // A SESSION THAT IS ALREADY RUNNING ON A SEAT THE OPERATOR SWITCHED OFF IS NOT KEPT (2026-10-01, the Claude-off audit: with every seat off, "hello" to a chat that was live on Cursor was still
    // answered by Cursor - the switch was honoured only when a session STARTED). Restarting goes through `start`, which moves the pane to a seat that can run or says plainly that none can.
    let seat_off = reg.live.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id).and_then(|l| crate::cli::engine_of(&json!(l.engine))).map_or(false, |e| crate::cli::model_blocked(e, None).is_some());
    let needs_start = needs_start(current.as_ref().map(|(m, _)| m.as_str()), model.as_deref(), seat_off);
    if needs_start {
        if let Err(e) = start(&state, &reg, pane_id, model.clone(), tier, false).await { return (StatusCode::CREATED, Json(json!({ "ok": false, "error": e }))).into_response(); }
        if current.is_some() { reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "model", "model": model }), true); }
    }
    if text.trim().is_empty() { return (StatusCode::CREATED, Json(json!({ "ok": true, "sent": false }))).into_response(); }
    let mut display = s(b.get("display"));
    // A CHAT (chat_pane.rs): an automatic episode boundary, the last snapshot and the retriever's picks ride in
    // front of the ask; the log keeps the ask as typed
    let (text, ctx_ev) = { let goals = reg.goals.lock().unwrap_or_else(|p| p.into_inner()).clone(); match goals { Some(g) => { let (t, c) = crate::chat_pane::before_send(&state, &reg, &g, pane_id, text.clone()).await; if t != text && display.is_empty() { display = text.clone(); } (t, c) } None => (text, None) } };
    // MIN'S LIFT (2026-09-21): a routed ask that named files without intel runs with exploration on — the manifest
    // says so for this one turn (the hook reads it per call); the result puts MIN back
    let lift = b.get("lift_min").and_then(Value::as_bool).unwrap_or(false);
    // THE ASK IS LOGGED BEFORE THE SEAT SEES IT (found by journey J1c, 2026-09-30): logging it after the write let a seat that answers
    // at once - a usage-limit error is instant - land its reply ABOVE the operator's question in the log every stream replays
    let live_now = reg.live.lock().unwrap_or_else(|p| p.into_inner()).contains_key(&pane_id);
    if live_now && !text.trim().is_empty() {
        let shown = if display.is_empty() { text.clone() } else { display.clone() };
        // DID THE LAST ANSWER HOLD? (operator 2026-09-25: "some codebasis dont have tests and are unknown to
        // the system … how to always be aware of the quality and correctness response?"). Asking the same
        // thing twice is the one correctness signal every repository can give, costs nothing, and needs no
        // test suite. It is REPORTED, not scored: an operator also re-asks to refine, and a signal that
        // punished that would make the router wrong in a new way. Whether it should move a row's quality is a
        // question for after the operator has seen how often it fires on their own work.
        let box_cwd = resolve_pane(&state, pane_id).await.map(|(p, _)| s(p.get("worktree_path"))).unwrap_or_default();
        if let Some(r) = crate::survival::reask_of(&shown, &recent_asks(&box_cwd, pane_id, 8), 8, 0.72) {
            // …AND IT MOVES THE WEIGHT NOW (operator 2026-09-25, asked directly: "yes"). It lands on the
            // PREVIOUS turn's row — the one that answered first — because the new ask has not been answered
            // yet and cannot have failed. Soft, and read as a ratio: see `quality_of`.
            let (engine, model) = { let l = reg.live.lock().unwrap_or_else(|p| p.into_inner()); l.get(&pane_id).map(|x| (x.engine.clone(), x.model.clone())).unwrap_or_default() };
            if !engine.is_empty() {
                let _ = crate::merges::record_reask("chat", &engine, if model.is_empty() { None } else { Some(&model) }, "any");
            }
            reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "reask", "text": crate::survival::note(&r),
                                      "of": r.of, "similarity": r.similarity, "turns_ago": r.turns_ago }), true);
        }
        reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "user", "text": shown }), true);
    }
    let sent = {
        let mut live = reg.live.lock().unwrap_or_else(|p| p.into_inner());
        match live.get_mut(&pane_id) {
            Some(l) => {
                l.busy = true; l.turn_started = now_ms(); l.turn_tin = 0; l.turn_tout = 0; l.last_ask = if display.is_empty() { text.clone() } else { display.clone() };
                l.gate_retry = false;   // a new ask earns its own retry; the latch is per ask, not per session
                if lift && l.min { if let Some(cwd) = reg.root_of(pane_id) { if set_manifest_mode(&cwd, "") { l.lift = true; } } }
                l.stdin.send(text.clone()).is_ok()
            }
            None => false,
        }
    };
    if sent && lift { reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "min", "state": "lifted", "text": "MIN lifted for this ask — it names files antiloki has no intel for, so the agent may explore" }), true); }
    if !sent && live_now { reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "note", "tone": "warn", "text": "▸ that message was not delivered - the agent's process had just ended" }), true); }
    if sent { if let Some(c) = ctx_ev { reg.emit(pane_id, c, true); } } // the picks, under the ask they served
    (StatusCode::CREATED, Json(json!({ "ok": sent }))).into_response()
}
/// `POST /api/v1/cockpit/agent/permission {pane_id, id, reply: once|always|reject}` — the pane's answer to a
/// permission card: Codex's (a JSON-RPC response) when the id is one of its asks, else OpenCode's (HTTP).
pub async fn agent_permission(reg: Arc<AgentRegistry>, body: Bytes) -> Response {
    let b = body_json(&body);
    let pane_id = num(b.get("pane_id"));
    let (id, reply) = (s(b.get("id")), s(b.get("reply")));
    let reply = if matches!(reply.as_str(), "once" | "always" | "reject") { reply } else { "once".to_string() };
    if let Some(ok) = crate::agent_codex::permission(&reg, pane_id, &id, &reply) { return (StatusCode::CREATED, Json(json!({ "ok": ok }))).into_response(); }
    crate::agent_opencode::permission(reg, body).await
}
pub async fn agent_stop(reg: Arc<AgentRegistry>, body: Bytes) -> Response {
    let b = body_json(&body); let pane_id = num(b.get("pane_id"));
    let ok = reg.stop(pane_id);
    if ok { reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "stopped" }), true); }
    (StatusCode::CREATED, Json(json!({ "ok": ok }))).into_response()
}
/// `POST cockpit/agent/note {pane_id, text, tone?}` — a line in the thread from the front door itself: what happened
/// to a message on its way in (2026-09-27: Heimdall refused the route and the ask went to the agent as typed —
/// before this, that refusal was a 400 nobody saw and the message was gone).
pub async fn agent_note(state: Arc<AppState>, reg: Arc<AgentRegistry>, body: Bytes) -> Response {
    let b = body_json(&body); let pane_id = num(b.get("pane_id")); let text = s(b.get("text"));
    if pane_id <= 0 || text.trim().is_empty() { return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "pane_id and text" }))).into_response(); }
    let tone = { let t = s(b.get("tone")); if ["ok", "warn", "bad", "dim"].contains(&t.as_str()) { t } else { "dim".to_string() } };
    // an idle pane has no root in the registry yet — the note must still land in ITS log, not nowhere
    if reg.roots.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id).is_none() {
        match resolve_pane(&state, pane_id).await { Some((_, cwd)) => reg.set_root(pane_id, &cwd), None => return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "no such pane" }))).into_response() }
    }
    reg.emit(pane_id, json!({ "type": "antiloki", "subtype": "note", "text": text.trim(), "tone": tone }), true);
    (StatusCode::CREATED, Json(json!({ "ok": true }))).into_response()
}
pub async fn agent_status(reg: Arc<AgentRegistry>, q: HashMap<String, String>) -> Response {
    let pane_id = q.get("pane_id").and_then(|v| v.parse().ok()).unwrap_or(0);
    Json(reg.status(pane_id)).into_response()
}
/// `GET /api/v1/cockpit/agent/events?pane_id&after&limit` — the persisted history.
pub async fn agent_events(state: Arc<AppState>, reg: Arc<AgentRegistry>, q: HashMap<String, String>) -> Response {
    let pane_id: i64 = q.get("pane_id").and_then(|v| v.parse().ok()).unwrap_or(0);
    let after: u64 = q.get("after").and_then(|v| v.parse().ok()).unwrap_or(0);
    let limit: usize = q.get("limit").and_then(|v| v.parse().ok()).unwrap_or(400);
    let known = { reg.roots.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id).cloned() };
    let root = match known { Some(r) => Some(r), None => resolve_pane(&state, pane_id).await.map(|(_, c)| c) };
    let Some(root) = root else { return Json(json!({ "ok": false, "events": [], "last_seq": 0 })).into_response() };
    reg.roots.lock().unwrap_or_else(|p| p.into_inner()).entry(pane_id).or_insert(root.clone());
    seed_from_transcript(&root, pane_id);
    let (events, last) = backlog(&root, pane_id, after, limit);
    Json(json!({ "ok": true, "events": events, "last_seq": last })).into_response()
}

/// `POST /api/v1/cockpit/agent/context {references, attachments}` — the chat's own reference resolver
/// (cards, knowledge docs, db rows, code files, live sources, attachments) rendered to markdown, so an
/// `@` chip in the agent's composer grounds the ask exactly as it grounds antiChat's.
pub async fn agent_context(state: Arc<AppState>, body: Bytes) -> Response {
    let b = body_json(&body);
    let input = json!({ "references": b.get("references").cloned().unwrap_or(json!([])), "attachments": b.get("attachments").cloned().unwrap_or(json!([])), "inputs": {} });
    let conn = state.pick();
    let out = tokio::task::spawn_blocking(move || {
        let c = conn.lock().unwrap_or_else(|p| p.into_inner());
        antiloki_rs::ai_assemble_context_envelope::assembleContextEnvelope(&input, &c)
    }).await.unwrap_or_else(|e| Err(e.to_string()));
    match out {
        Ok(v) => Json(json!({ "ok": true, "references_md": v.get("render").and_then(|r| r.get("references_md")).cloned().unwrap_or(json!("")), "attachments_md": v.get("render").and_then(|r| r.get("attachments_md")).cloned().unwrap_or(json!("")), "references": v.get("references").cloned().unwrap_or(json!([])) })).into_response(),
        Err(e) => Json(json!({ "ok": false, "error": e })).into_response(),
    }
}
/// The live forwarder for the multiplexed stream: backlog since `after`, then every event, as
/// `{pane, kind: "agent", seq, data}` on the tab's channel — until the tab drops it.
pub fn follow(state: Arc<AppState>, reg: Arc<AgentRegistry>, pane_id: i64, after: u64, tx: tokio::sync::mpsc::UnboundedSender<String>, cancel: Arc<std::sync::atomic::AtomicBool>) {
    tokio::spawn(async move {
        let stopped = || cancel.load(std::sync::atomic::Ordering::Relaxed);
        let mut rx = reg.subscribe(pane_id);
        let known = { reg.roots.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id).cloned() };
        let root = match known { Some(r) => Some(r), None => resolve_pane(&state, pane_id).await.map(|(_, c)| c) };
        let mut last = after;
        if let Some(root) = &root { seed_from_transcript(root, pane_id); let (events, l) = backlog(root, pane_id, after, 400); last = l; for ev in events { if tx.send(json!({ "pane": pane_id, "kind": "agent", "seq": ev.get("seq").cloned().unwrap_or(json!(0)), "data": ev }).to_string()).is_err() { return; } } }
        if tx.send(json!({ "pane": pane_id, "kind": "agent-ready", "seq": last, "data": reg.status(pane_id) }).to_string()).is_err() { return; }
        loop {
            if stopped() { return; }
            match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
                Ok(Ok((seq, line))) => { if seq != 0 && seq <= last { continue; } if seq != 0 { last = seq; } if tx.send(json!({ "pane": pane_id, "kind": "agent", "seq": seq, "data": serde_json::from_str::<Value>(&line).unwrap_or(Value::Null) }).to_string()).is_err() { return; } }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => { let _ = tx.send(json!({ "pane": pane_id, "kind": "agent-lagged" }).to_string()); }
                Ok(Err(_)) => return,
                Err(_) => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_heimdall_rewrite_is_not_an_ask_the_operator_repeated() {
        let ev = |seq: u32, text: &str, source: Option<&str>| {
            let mut e = json!({ "type": "antiloki", "subtype": "user", "text": text });
            if let Some(s) = source { e["source"] = json!(s); }
            json!({ "seq": seq, "t": 1, "ev": e }).to_string()
        };
        let log = [ev(1, "make the buttons deeper", None), ev(2, "Give the primary buttons more depth in src/App.css", Some("heimdall")), ev(3, "thanks", None)].join("\n");
        assert_eq!(asks_in_log(&log, 8), vec!["thanks".to_string(), "make the buttons deeper".to_string()], "newest first; the rewrite is not in the list");
        assert_eq!(asks_in_log(&log, 1), vec!["thanks".to_string()]);
    }

    use super::*;
    #[test]
    fn a_box_that_lost_its_git_link_is_reported_and_one_that_has_it_is_not() {
        let base = std::env::temp_dir().join(format!("antiloki-missingbox-{}", std::process::id()));
        let boxes = base.join("agents");
        let gone = boxes.join("proj-1234").join("fork-a");
        let live = boxes.join("proj-1234").join("fork-b");
        let shared = base.join("project");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&gone).unwrap(); std::fs::create_dir_all(&live).unwrap(); std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(live.join(".git"), "gitdir: /somewhere/.git/worktrees/fork-b\n").unwrap();
        assert!(missing_box_error(&gone.to_string_lossy(), &boxes).unwrap().contains("missing"));
        assert!(missing_box_error(&gone.to_string_lossy(), &boxes).unwrap().contains("Fork the agent again"));
        assert!(missing_box_error(&live.to_string_lossy(), &boxes).is_none());            // a worktree's `.git` is a file
        assert!(missing_box_error(&shared.to_string_lossy(), &boxes).is_none());          // not a box: not judged here
        assert!(!gone.join(".antiloki").exists());                                         // and the check made nothing
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_send_restarts_a_session_whose_seat_was_switched_off_since_it_started() {
        // never started: start it
        assert!(needs_start(None, None, false));
        // running, nothing asked, seat on: leave it alone
        assert!(!needs_start(Some("composer-2.5"), None, false));
        // running on the model asked for: leave it alone; on another: restart
        assert!(!needs_start(Some("composer-2.5"), Some("composer-2.5"), false));
        assert!(needs_start(Some("composer-2.5"), Some("gemini-3.8-flash-low"), false));
        // the Claude-off audit (2026-10-01): every seat was off and "hello" to a chat live on Cursor was still answered by Cursor
        assert!(needs_start(Some("composer-2.5"), None, true), "a session on a seat the operator switched off is not kept");
        assert!(needs_start(Some("composer-2.5"), Some("composer-2.5"), true));
    }
    #[test]
    fn preset_model_reads_the_pane_settings() {
        assert_eq!(preset_model(&json!({ "settings_json": "{\"model\":\"opencode/kimi-k2.7-code\",\"theme\":\"kimi\"}" })).as_deref(), Some("opencode/kimi-k2.7-code"));
        assert_eq!(preset_model(&json!({ "settings_json": "{\"model\":\"  \"}" })), None, "blank = unset");
        assert_eq!(preset_model(&json!({ "settings_json": "{}" })), None);
        assert_eq!(preset_model(&json!({ "settings_json": "not json" })), None);
        assert_eq!(preset_model(&json!({})), None);
    }
    #[test]
    fn min_prices_its_turns_at_the_models_tier_and_counts_the_refusals_since_the_turn_began() {
        use crate::cli::Tier;
        assert_eq!(tier_of_model("claude-code", "claude-opus-5"), Tier::High); assert_eq!(tier_of_model("claude-code", "sonnet"), Tier::Mid); assert_eq!(tier_of_model("claude-code", "claude-haiku-4-5"), Tier::Low);
        assert_eq!(tier_of_model("codex", "gpt-5.6-terra · low"), Tier::Low); assert_eq!(tier_of_model("codex", "gpt-5.6-terra · high"), Tier::High); assert_eq!(tier_of_model("codex", "gpt-5.6-terra"), Tier::Mid);
        assert_eq!(tier_of_model("opencode", "opencode/deepseek-v4-flash"), Tier::Low); assert_eq!(tier_of_model("opencode", "opencode/deepseek-v4-pro"), Tier::Mid);
        let log = [
            json!({ "t": "2026-09-21T03:29:00.000Z", "tool": "Grep", "kind": "blocked", "op": "search", "min": true }),
            json!({ "t": "2026-09-21T03:29:30.000Z", "tool": "Read", "kind": "blocked", "op": "read", "min": true }),
            json!({ "t": "2026-09-21T03:29:31.000Z", "tool": "Read", "kind": "source", "op": "read" }),
            json!({ "t": "2026-09-21T03:29:32.000Z", "tool": "Edit", "kind": "blocked", "op": "write" }),
        ].iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n");
        let t = crate::when::parse_iso("2026-09-21T03:29:20.000Z").unwrap() * 1000;
        assert_eq!(min_refusals_since(&log, t), 1, "only MIN's refusals, only since the turn began (the scope refusal is not MIN's)");
        assert_eq!(min_refusals_since(&log, 0), 2);
        // the manifest's mode flips in place — the hook reads it per call
        let d = std::env::temp_dir().join(format!("antiloki-lift-{}", std::process::id())); std::fs::create_dir_all(d.join(".antiloki/context")).unwrap();
        std::fs::write(d.join(".antiloki/context/manifest.json"), json!({ "mode": "min", "scope": ["a/**"] }).to_string()).unwrap();
        let cwd = d.to_string_lossy().to_string();
        assert!(set_manifest_mode(&cwd, "")); let m: Value = serde_json::from_str(&std::fs::read_to_string(d.join(".antiloki/context/manifest.json")).unwrap()).unwrap(); assert_eq!(m["mode"], ""); assert_eq!(m["scope"][0], "a/**");
        assert!(set_manifest_mode(&cwd, "min")); let m: Value = serde_json::from_str(&std::fs::read_to_string(d.join(".antiloki/context/manifest.json")).unwrap()).unwrap(); assert_eq!(m["mode"], "min");
        assert!(!set_manifest_mode("/definitely/not/a/box", "min"));
        assert!(pane_min(&json!({ "settings_json": "{\"min\":true}" })) && !pane_min(&json!({ "settings_json": "{}" })) && pane_parent(&json!({ "session_id": 5, "id": 3 })) == "5-3");
        let _ = std::fs::remove_dir_all(&d);
    }
    #[test]
    /// A FLOAT IS A NUMBER (operator 2026-09-25: the Stats Detail box showed 0 tokens for a codex turn).
    /// Claude's CLI reports usage as integers; Codex's reaches the same code as floats, and `Value::as_i64()`
    /// answers None for a float — so the audit recorded zero tokens and zero cost next to a result event that
    /// carried 1136 of them. This pins both spellings against the accessor the audit actually uses.
    /// A GPT MODEL IS NOT CLAUDE'S (operator 2026-09-30: the usage box read "Claude · gpt-5.6-terra"). A Codex turn's assistant event names a model and no engine, and
    /// the missing engine used to default to claude-code.
    #[test]
    fn a_model_keeps_its_own_seats_name_when_the_event_does_not_say_the_engine() {
        let codex_turn = json!({ "type": "assistant", "message": { "role": "assistant", "model": "gpt-5.6-terra@medium", "usage": { "input_tokens": 10.0, "output_tokens": 5.0 } } });
        let u = merge_usage(json!({}), &codex_turn, "t").unwrap();
        assert_eq!(u["engine"], "codex", "{u}"); assert_eq!(u["model"], "gpt-5.6-terra@medium");
        // a later event of the same pane with no model keeps what the pane already knew
        let later = merge_usage(u, &json!({ "type": "rate_limit_event", "rate_limit_info": { "unifiedWindows": { "five_hour": { "utilization": 1.0, "resetsAt": 5 } } } }), "t").unwrap();
        assert_eq!(later["engine"], "codex");
        // Claude's own turn is still Claude's
        let claude = merge_usage(json!({}), &json!({ "type": "assistant", "message": { "role": "assistant", "model": "claude-sonnet-5-5", "usage": { "input_tokens": 1 } } }), "t").unwrap();
        assert_eq!(claude["engine"], "claude-code");
        for (m, e) in [("gemini-3.8-flash-medium", Some("gemini")), ("composer-2.5", Some("cursor")), ("grok-4.6", Some("grok")), ("mystery", None), ("", None)] { assert_eq!(engine_of_model_name(m), e, "{m}"); }
    }

    #[test]
    fn usage_is_read_whether_the_engine_reports_integers_or_floats() {
        let read = |u: &Value| {
            let n = |k: &str| u.get(k).and_then(Value::as_f64).map(|x| x.round() as i64).unwrap_or(0);
            (n("input_tokens"), n("output_tokens"), n("cache_read_input_tokens"))
        };
        // Claude's shape: integers
        assert_eq!(read(&json!({ "input_tokens": 8, "output_tokens": 502, "cache_read_input_tokens": 125547 })), (8, 502, 125547));
        // Codex's shape, verbatim from this machine's own event log
        assert_eq!(read(&json!({ "input_tokens": 1136.0, "output_tokens": 44.0, "cache_read_input_tokens": 18176.0 })), (1136, 44, 18176));
        // and `as_i64` — what the code used to call — cannot read the second one at all
        assert_eq!(json!(1136.0).as_i64(), None, "if this ever becomes Some, the bug's cause has changed");
        assert_eq!(json!(1136).as_i64(), Some(1136));
        // a missing field is still zero, not a panic
        assert_eq!(read(&json!({})), (0, 0, 0));
    }

    fn turn_tokens_price_the_cache() {
        assert_eq!(turn_tokens(&json!({ "usage": { "input_tokens": 14, "cache_creation_input_tokens": 1000, "cache_read_input_tokens": 100_000, "output_tokens": 847 } })), (11264, 847));
        assert_eq!(turn_tokens(&json!({})), (0, 0));
    }
    #[test]
    fn usage_file_is_kept_from_the_stream() {
        // the three events that carry numbers, in the order the CLI sends them (real shapes, 2026-09-20)
        let u = merge_usage(json!({}), &json!({ "type": "rate_limit_event", "rate_limit_info": { "unifiedWindows": { "five_hour": { "utilization": 0.06, "resetsAt": 1789951800 }, "seven_day": { "utilization": 0.63, "resetsAt": 1790337600 } } } }), "t0").unwrap();
        assert_eq!(u["limits"], json!([{ "label": "5h", "used_pct": 6.0, "resets_at": 1789951800 }, { "label": "week", "used_pct": 63.0, "resets_at": 1790337600 }]));
        let u = merge_usage(u, &json!({ "type": "assistant", "message": { "model": "claude-sonnet-5", "usage": { "input_tokens": 2, "cache_creation_input_tokens": 394, "cache_read_input_tokens": 50757, "output_tokens": 5 } } }), "t1").unwrap();
        assert_eq!(u["model"], "claude-sonnet-5"); assert_eq!(u["context"]["input_tokens"], 51153.0); assert!(u["context"]["used_pct"].is_null(), "no window known yet");
        let u = merge_usage(u, &json!({ "type": "result", "total_cost_usd": 0.1315316, "usage": { "input_tokens": 14, "cache_creation_input_tokens": 18785, "cache_read_input_tokens": 234338, "output_tokens": 847 }, "modelUsage": { "claude-haiku-4-5-20251001": { "contextWindow": 200000, "outputTokens": 11 }, "claude-sonnet-5": { "contextWindow": 1000000, "outputTokens": 847 } } }), "t2").unwrap();
        assert_eq!(u["cost_usd"], 0.1315); assert_eq!(u["context"]["size"], 1000000.0); assert_eq!(u["context"]["used_pct"], 5.1);
        assert_eq!(u["session"]["messages"], 1); assert_eq!(u["session"]["tokens_total"], 253984.0);
        assert_eq!(u["engine"], "claude-code"); assert_eq!(u["t"], "t2");
        assert!(merge_usage(u.clone(), &json!({ "type": "system", "subtype": "status" }), "t3").is_none(), "an event without numbers leaves the file alone");
        // the next message's context pct uses the window learnt from the result
        let u = merge_usage(u, &json!({ "type": "assistant", "message": { "model": "claude-sonnet-5", "usage": { "input_tokens": 1, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 99999, "output_tokens": 1 } } }), "t4").unwrap();
        assert_eq!(u["context"]["used_pct"], 10.0); assert_eq!(u["session"]["messages"], 2);
    }
    #[test]
    fn the_backlog_reads_after_a_seq_bounded_and_says_the_last() {
        let d = std::env::temp_dir().join(format!("antiloki-agent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join(".antiloki")).unwrap();
        let p = d.join(".antiloki").join("agent-events.jsonl");
        std::fs::write(&p, (1..=5).map(|i| json!({ "seq": i, "t": i, "ev": { "type": "x", "i": i } }).to_string()).collect::<Vec<_>>().join("\n")).unwrap();
        std::fs::rename(&p, events_path(&d.to_string_lossy(), 7)).unwrap();
        let (evs, last) = backlog(&d.to_string_lossy(), 7, 2, 2);
        assert_eq!(evs.iter().map(|e| e["seq"].as_u64().unwrap()).collect::<Vec<_>>(), vec![4, 5]); assert_eq!(last, 5);
        assert_eq!(seq_on_disk(&d.to_string_lossy(), 7), 5);
        assert_eq!(backlog(&d.to_string_lossy(), 7, 9, 10).0.len(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    // THE GATES CHECK THE DISK (operator 2026-09-24): what changed is git's answer, not the agent's — an agent
    // that forgets to mention a file it edited is precisely what these gates are for.
    #[test]
    fn the_gates_see_what_actually_changed_including_files_nobody_tracked() {
        let d = std::env::temp_dir().join(format!("antiloki-changed-{}-{}", std::process::id(), now_ms()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        let root = d.to_string_lossy().to_string();
        let git = |args: &[&str]| { let _ = crate::proc::cmd("git").args(args).current_dir(&d).output(); };
        git(&["init", "-q"]); git(&["config", "user.email", "t@t"]); git(&["config", "user.name", "t"]);
        std::fs::write(d.join("src/kept.ts"), "export const a = 1;\n").unwrap();
        git(&["add", "-A"]); git(&["commit", "-qm", "base"]);

        assert_eq!(changed_files(&root), Vec::<String>::new(), "a clean box has nothing for the gates to check");

        std::fs::write(d.join("src/kept.ts"), "export const a = 2;\n").unwrap();   // edited
        std::fs::write(d.join("src/new.ts"), "export const b = 3;\n").unwrap();    // never tracked
        std::fs::create_dir_all(d.join(".antiloki")).unwrap();
        std::fs::write(d.join(".antiloki/usage-1.json"), "{}").unwrap();            // ours, not theirs
        std::fs::write(d.join("AGENTS.md"), "# ours too\n").unwrap();               // …and so is this
        let mut got = changed_files(&root); got.sort();
        assert_eq!(got, vec!["src/kept.ts".to_string(), "src/new.ts".to_string()],
                   "the untracked file counts, and antiloki's own bookkeeping never does");
        assert!(ours("AGENTS.md") && ours(".antiloki-nm-host-aside/x") && ours(".claude/settings.json"));
        assert!(!ours("src/AGENTS.md"), "only at the root — a file the operator wrote is theirs");

        assert_eq!(changed_files("/definitely/not/a/repo"), Vec::<String>::new(), "no repo, no list — and no pretending");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A SEQ IS UNIQUE PER PANE, AND THE COUNTER MUST FIND THAT OUT FOR ITSELF (2026-09-27).
    ///
    /// `seed_from_transcript` writes the events file with its own numbering, 1..N, and two of its three call sites
    /// never re-seeded the in-memory counter — so the first live emit started at 1 again and collided exactly with
    /// the imported turns. Pane 25's file held 30 lines carrying 16 distinct seqs: the conversation rendered twice
    /// and "2 turns · 3 replies" counted as 4 and 6.
    ///
    /// It matters more than double-counting now that the readers de-dupe ON seq: a restarted counter would make
    /// them DROP live events that happen to reuse an old number.
    #[test]
    fn the_seq_counter_continues_the_file_rather_than_restarting_at_one() {
        let d = std::env::temp_dir().join(format!("antiloki-seq-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&d);
        let root = d.to_string_lossy().to_string();
        let p = events_path(&root, 42);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        // a file already on disk — whatever wrote it, and by whatever route
        // NO TRAILING NEWLINE, deliberately: that is what a writer that joins its lines leaves behind, and appending
        // onto it used to corrupt the last record and lose two events at once.
        std::fs::write(&p, (1..=5).map(|n| json!({ "seq": n, "t": 0, "ev": { "type": "antiloki", "subtype": "user", "text": "imported" } }).to_string()).collect::<Vec<_>>().join("\n")).unwrap();
        assert_eq!(seq_on_disk(&root, 42), 5, "the file's last seq is what the counter has to continue from");

        // a registry that has never seen this pane: `emit` learns the root, then must NOT hand out seq 1 again
        let reg = AgentRegistry::default();
        reg.roots.lock().unwrap().insert(42, root.clone());
        reg.emit(42, json!({ "type": "antiloki", "subtype": "user", "text": "live" }), true);

        let seqs: Vec<u64> = std::fs::read_to_string(&p).unwrap().lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|v| v.get("seq").and_then(Value::as_u64)).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4, 5, 6], "the live event continues the file: {seqs:?}");
        assert_eq!(seqs.len(), seqs.iter().collect::<std::collections::HashSet<_>>().len(), "no seq appears twice");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn two_agents_on_one_worktree_do_not_read_each_others_turns() {
        // the bug of 2026-09-24: every agent of a workspace appended to and replayed ONE events file, so a newly
        // made agent opened showing the previous agent's transcript and never showed its own.
        let d = std::env::temp_dir().join(format!("antiloki-agent-split-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(d.join(".antiloki")).unwrap();
        let root = d.to_string_lossy().to_string();
        let line = |i: u64, who: &str| json!({ "seq": i, "t": i, "ev": { "type": "x", "who": who } }).to_string();

        // each writes to its OWN file
        std::fs::write(events_path(&root, 11), [line(1, "eleven"), line(2, "eleven")].join("\n")).unwrap();
        std::fs::write(events_path(&root, 22), [line(1, "twentytwo")].join("\n")).unwrap();

        let (a, _) = backlog(&root, 11, 0, 100);
        let (b, _) = backlog(&root, 22, 0, 100);
        assert_eq!(a.len(), 2, "pane 11 sees only its own");
        assert_eq!(b.len(), 1, "pane 22 sees only its own");
        assert!(a.iter().all(|e| e["ev"]["who"] == "eleven"));
        assert!(b.iter().all(|e| e["ev"]["who"] == "twentytwo"));

        // the legacy shared file is NOT adopted by anybody: it is a braid of every agent that ever ran here, and
        // giving it to each of them as "your history" is the bug restated. A pane with nothing of its own is empty.
        std::fs::write(d.join(".antiloki").join("agent-events.jsonl"), line(1, "legacy")).unwrap();
        assert_eq!(backlog(&root, 33, 0, 100).0.len(), 0, "a fresh agent starts empty, not with a stranger's turns");
        let (a2, _) = backlog(&root, 11, 0, 100);
        assert!(a2.iter().all(|e| e["ev"]["who"] == "eleven"), "and an agent with its own file is untouched by it");
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod engine_for_pane_tests {
    use super::*;
    use crate::cli::Engine;
    #[test]
    fn a_writable_pane_may_actually_write() {
        // "auto" was not a mode at all: Claude Code fell back to `default`, which asks — and a headless -p
        // session has nobody to ask, so every write was refused (operator 2026-09-28)
        assert_eq!(permission_mode(false, false), "acceptEdits", "the scope hook is the boundary, not a prompt");
        assert_eq!(permission_mode(true, false), "bypassPermissions", "MAX is the operator saying so explicitly");
        assert_eq!(permission_mode(false, true), "plan", "read-only cannot write by construction");
        assert_eq!(permission_mode(true, true), "plan", "read-only outranks MAX");
        for m in [permission_mode(false, false), permission_mode(true, false), permission_mode(false, true)] {
            assert!(["default", "acceptEdits", "bypassPermissions", "plan"].contains(&m), "{m} is not a Claude Code mode");
        }
    }
    #[test]
    fn a_model_does_not_travel_to_a_seat_that_has_never_heard_of_it() {
        assert_eq!(model_for_seat(Some(Engine::Cursor), "haiku".into(), None), Some("composer-2.5".into()),
                   "a Claude model on a Cursor pane would bill the $20 third-party allowance, or be refused");
        assert_eq!(model_for_seat(Some(Engine::Cursor), "composer-2.5".into(), None), Some("composer-2.5".into()));
        assert_eq!(model_for_seat(Some(Engine::ClaudeCode), "haiku".into(), None), Some("haiku".into()));
        assert_eq!(model_for_seat(Some(Engine::Gemini), "sonnet".into(), None), Some("gemini-3.8-flash-low".into()));
        assert_eq!(model_for_seat(Some(Engine::ClaudeCode), "".into(), None), None, "nothing named: the CLI's own default");
        // the audit: a swap must not quietly demote the tier the router asked for
        assert_eq!(model_for_seat(Some(Engine::Gemini), "sonnet".into(), Some(crate::cli::Tier::High)),
                   crate::cli::tier_model(Engine::Gemini, crate::cli::Tier::High),
                   "asked for high, given the seat's high rung — not its cheapest");
    }
    #[test]
    fn a_pane_whose_kind_is_not_a_seat_is_repaired_rather_than_left_unstartable() {
        let pane = |st: &str| json!({ "settings_json": st });
        assert_eq!(engine_for_pane(&pane("{}"), "codex".into()), Some(Engine::Codex), "a real kind is kept as it is");
        assert_eq!(engine_for_pane(&pane("{}"), "cursor".into()), Some(Engine::Cursor));
        assert_eq!(engine_for_pane(&pane(r#"{"engine":"gemini"}"#), "agent".into()), Some(Engine::Gemini),
                   "the word `agent` is not a seat — the pane's own settings answer next");
        // nothing to go on: whatever seat is usable here — and honestly `None` when none is, which is what the
        // caller's error branch is for (it was unreachable while this ended in `.or(Some(ClaudeCode))`)
        assert!(engine_for_pane(&pane("{}"), "agent".into()).is_some(), "this machine has usable seats");
    }
}

#[cfg(test)]
mod workspace_epoch_tests {
    use super::*;
    #[test]
    fn a_workspace_switch_empties_every_per_pane_map() {
        let r = AgentRegistry::default();
        assert!(!r.sync_to(0), "the first epoch is the starting one");
        r.set_root(1, "/a"); r.seqs.lock().unwrap().insert(1, 9);
        assert!(!r.sync_to(0), "nothing changed: nothing is dropped");
        assert_eq!(r.root_of(1).as_deref(), Some("/a"));
        assert!(r.sync_to(1), "another workspace is active now");
        assert_eq!(r.root_of(1), None, "pane 1 of the other project is a different pane");
        assert!(r.seqs.lock().unwrap().is_empty());
    }
}
