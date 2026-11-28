//! Port of `focus/pty.service.ts` — the cockpit terminal panes.
//!
//! node-pty -> `portable-pty`: the same ConPTY (Windows) / openpty (unix) mechanism, without a
//! Node native addon. This is one of the two pieces that made the BE need Node at all.
//!
//! Output is NOT streamed to the client. It is appended to the substrate as build-gate chunks
//! (`system.build-gate.chunk.append`, contract_slug `pane-pty-<id>`), which the FE reads back —
//! so terminal history survives a reload and is auditable like everything else.
//!
//! Two behaviours are load-bearing:
//!   * **16ms / 6000-char flush.** A PTY emits tiny writes per keystroke; one dispatch each would
//!     hammer SQLite. Buffer, then flush on size or timer.
//!   * **retry once on "locked".** The BE catches a SQLITE_BUSY-shaped message, waits 150ms and
//!     retries exactly once, then DROPS the chunk. Terminal output is not worth stalling on.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use crate::dispatch::{engine_call, AppState};
use crate::pty_remote;

const FLUSH_MS: u64 = 16;
const FLUSH_BYTES: usize = 6000;

struct Live {
    writer: Box<dyn Write + Send>,
    /// the PTY and the child, when this process owns them; `None` when the sidecar host does (RFC 0007 M1)
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    /// the terminal belongs to the sidecar host: resize, kill and the exit code go through it
    remote: bool,
    /// the command it runs (for the composer markers and the send quirks)
    cmd: String,
    /// which start made this entry: a pipeline that ends removes only its own, never a newer terminal's
    gen: u64,
    seq: u64,
    /// The CLI's composer has been painted — its stdin accepts a prompt now. Keystrokes typed before
    /// this are eaten by the TUI's boot (2026-09-19: OpenCode 1.18 swallowed a pane's first ask).
    ready: bool,
    /// THE QUOTA LINE (2026-09-20): the CLI printed its own "usage limit" sentence — kept (ANSI-stripped,
    /// bounded) so the pane can warn and offer the handoff; cleared by the next start.
    quota: Option<(String, i64)>,
    /// THE CLI IS ASKING ITS OWN QUESTION BEFORE IT CAN START (found 2026-11-24: Claude's "do you trust this folder" prompt comes before it loads any tool, so the connection gate covered the pane and the person could not answer it).
    ask: bool,
}

#[derive(Default)]
pub struct PtyRegistry {
    live: Mutex<HashMap<i64, Live>>,
    /// THE PANES WHOSE AGENT HAS CONNECTED TO ANTILOKI'S TOOLS (operator 2026-11-23): the tool helper each agent starts says "up" the moment the agent initialises it. Cleared by every fresh start.
    mcp_up: Mutex<std::collections::HashSet<i64>>,
    /// THE PUSH CHANNEL (2026-09-20): every flushed chunk is broadcast to the pane's SSE subscribers
    /// — the FE used to poll build-live 4–60×/s per pane and kept an idle serve at ~60% of a core
    chans: Mutex<HashMap<i64, tokio::sync::broadcast::Sender<(u64, String)>>>,
    /// the last quota line per pane — survives the CLI's exit (it usually quits right after saying it)
    last_quota: Mutex<HashMap<i64, (String, i64)>>,
    /// THE MULTIPLEXED STREAM (2026-09-20): one SSE per browser tab carrying every pane the tab shows —
    /// a stream per pane plus the events stream hit Chrome's 6-connections-per-host cap at five agents
    /// (the sixth terminal never connected, every other request queued: "the terminal UI broke").
    /// sub id → (the tab's outbound channel, the panes it follows with their cancel flags)
    subs: Mutex<HashMap<String, Sub>>,
}
struct Sub { tx: tokio::sync::mpsc::UnboundedSender<String>, panes: HashMap<i64, Arc<std::sync::atomic::AtomicBool>> }

impl PtyRegistry {
    /// (open subscriptions, followed panes across them) — live ones only (a closed tab's channel is gone)
    pub fn stream_census(&self) -> (usize, usize) {
        let subs = self.subs.lock().unwrap_or_else(|p| p.into_inner());
        let live: Vec<&Sub> = subs.values().filter(|s| !s.tx.is_closed()).collect();
        (live.len(), live.iter().map(|s| s.panes.len()).sum())
    }
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// subscribe to a pane's live output (chunks after the call), creating the channel if needed
    pub fn subscribe(&self, pane_id: i64) -> tokio::sync::broadcast::Receiver<(u64, String)> {
        let mut chans = self.chans.lock().unwrap_or_else(|p| p.into_inner());
        chans.entry(pane_id).or_insert_with(|| tokio::sync::broadcast::channel(512).0).subscribe()
    }
    fn publish(&self, pane_id: i64, seq: u64, chunk: &str) {
        if let Some(tx) = self.chans.lock().unwrap_or_else(|p| p.into_inner()).get(&pane_id) { let _ = tx.send((seq, chunk.to_string())); }
    }
    pub fn status(&self, pane_id: i64) -> Value {
        let live = self.live.lock().unwrap();
        match live.get(&pane_id) { Some(e) => json!({ "running": true, "ready": e.ready, "quota": e.quota.as_ref().map(|(t, at)| json!({ "text": t, "t": at })) }), None => json!({ "running": false, "ready": false, "quota": self.last_quota.lock().unwrap().get(&pane_id).map(|(t, at)| json!({ "text": t, "t": at, "exited": true })) }) }
    }
    /// The helper of this pane's agent connected. Ignored for a pane with no live terminal.
    pub fn mark_mcp(&self, pane_id: i64) { if self.live.lock().unwrap().contains_key(&pane_id) { self.mcp_up.lock().unwrap().insert(pane_id); } }
    /// Whether this pane's agent is connected to antiloki's tools, and whether this serve is told (older helpers never say, so `reports` is only for helpers that do - see `GET pane/mcp`).
    pub fn mcp_state(&self, pane_id: i64) -> Value { let running = self.live.lock().unwrap().contains_key(&pane_id); let ask = self.live.lock().unwrap().get(&pane_id).map_or(false, |e| e.ask); json!({ "ok": true, "running": running, "mcp": running && self.mcp_up.lock().unwrap().contains(&pane_id), "ask": ask }) }
    fn mark_ask(&self, pane_id: i64, on: bool) { if let Some(e) = self.live.lock().unwrap().get_mut(&pane_id) { e.ask = on; } }
    fn mark_ready(&self, pane_id: i64) { if let Some(e) = self.live.lock().unwrap().get_mut(&pane_id) { e.ready = true; } }
    fn mark_quota(&self, pane_id: i64, text: String) {
        let at = crate::cli::now_ms();
        if let Some(e) = self.live.lock().unwrap().get_mut(&pane_id) { if e.quota.is_none() { e.quota = Some((text.clone(), at)); } }
        self.last_quota.lock().unwrap().insert(pane_id, (text, at));
    }

    pub fn input(&self, pane_id: i64, data: &str) -> Value {
        let mut live = self.live.lock().unwrap();
        match live.get_mut(&pane_id) {
            None => json!({ "ok": false, "error": "no live terminal for this pane — start it first" }),
            Some(e) => {
                let _ = e.writer.write_all(data.as_bytes());
                let _ = e.writer.flush();
                json!({ "ok": true })
            }
        }
    }

    pub fn resize(&self, pane_id: i64, cols: i64, rows: i64) -> Value {
        // `Math.max(20, cols | 0)` / `Math.max(5, rows | 0)`; a mid-exit failure is swallowed.
        let (cols, rows) = (cols.max(20) as u16, rows.max(5) as u16);
        let remote = {
            let live = self.live.lock().unwrap();
            match live.get(&pane_id) {
                None => return json!({ "ok": false, "error": "no live terminal" }),
                Some(e) => {
                    if let Some(m) = e.master.as_ref() { let _ = m.resize(portable_pty::PtySize { cols, rows, pixel_width: 0, pixel_height: 0 }); }
                    e.remote
                }
            }
        };
        if remote { pty_remote::resize(pane_id, rows, cols); } // outside the lock: it talks to another process
        json!({ "ok": true })
    }

    pub fn kill(&self, pane_id: i64) -> Value {
        // the browser window it opened goes with it (RFC 0024 B5), whatever holds its terminal (found 2026-11-28: only the host's terminals did this); on its own thread, closing a window takes a moment
        std::thread::spawn(move || crate::agent_browser::pane_closed(pane_id));
        let gone = self.live.lock().unwrap().remove(&pane_id);
        match gone {
            None => json!({ "ok": true, "was_running": false }),
            Some(mut e) => {
                crate::pane_watch::release(pane_id); if e.remote { pty_remote::kill(pane_id); } else if let Some(c) = e.child.as_mut() { let _ = c.kill(); }
                json!({ "ok": true, "was_running": true })
            }
        }
    }
}

/// Append one terminal chunk to the substrate, with the BE's single "locked" retry.
async fn write_chunk(state: &Arc<AppState>, pane_id: i64, seq: u64, chunk: &str, done: i64) {
    let input = json!({
        "contract_slug": format!("pane-pty-{pane_id}"),
        "seq": seq,
        "chunk": chunk,
        "done": done,
    });
    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
    match engine_call(state, "system.build-gate.chunk.append", input.clone(), ctx.clone()).await {
        Ok(_) => {}
        Err(e) if e.to_lowercase().contains("locked") => {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            // One retry, then the chunk is dropped on purpose.
            let _ = engine_call(state, "system.build-gate.chunk.append", input, ctx).await;
        }
        Err(_) => {}
    }
}

/// Port of `PtyService.start()`. Returns the BE's exact success/failure shapes.
pub async fn start(
    state: Arc<AppState>,
    registry: Arc<PtyRegistry>,
    pane_id: i64,
    cmd: Option<String>,
    args: Vec<String>,
    cwd: String,
    env: HashMap<String, String>,
    cols: Option<i64>,
    rows: Option<i64>,
) -> Value {
    registry.kill(pane_id); // a re-start replaces the prior terminal

    // Same shell resolution: an explicit non-.exe command on Windows is run THROUGH cmd /c,
    // because ConPTY needs a real executable and `foo` may be a .cmd shim.
    let win = cfg!(windows);
    let default_shell = if win {
        std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "bash".into())
    };
    let mut shell = cmd.clone().unwrap_or_else(|| default_shell.clone());
    let mut argv = args.clone();
    if !win && !shell.contains('/') { shell = antiloki_rs::proc::real_program(&shell); } // a wrapper that starts itself would never start the agent (issue 6)
    if win {
        if let Some(c) = &cmd {
            if !c.to_lowercase().ends_with(".exe") {
                argv = {
                    let mut v = vec!["/c".to_string(), c.clone()];
                    v.extend(args.clone());
                    v
                };
                shell = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into());
            }
        }
    }

    // Where the terminal lives: the sidecar host (RFC 0007) or this process.
    type Parts = (Option<Box<dyn portable_pty::MasterPty + Send>>, Option<Box<dyn portable_pty::Child + Send + Sync>>, Box<dyn Read + Send>, Box<dyn Write + Send>);
    let remote = pty_remote::enabled();
    let spawn_result: Result<Parts, String> = if remote {
        let sp = pty_remote::Spawn { shell: shell.clone(), argv: argv.clone(), cwd: cwd.clone(), env: env.clone(),
            rows: rows.unwrap_or(30).clamp(5, 500) as u16, cols: cols.unwrap_or(100).clamp(20, 500) as u16 };
        match tokio::task::spawn_blocking(move || pty_remote::spawn_and_attach(pane_id, &sp)).await {
            Ok(Ok(stream)) => stream.try_clone().map_err(|e| e.to_string())
                .map(|w| (None, None, Box::new(stream) as Box<dyn Read + Send>, Box::new(w) as Box<dyn Write + Send>)),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(e.to_string()),
        }
    } else {
        (|| -> Result<Parts, String> {
            let pair = portable_pty::native_pty_system().openpty(portable_pty::PtySize {
                cols: cols.unwrap_or(100) as u16,
                rows: rows.unwrap_or(30) as u16,
                pixel_width: 0,
                pixel_height: 0,
            });
            let pair = pair.map_err(|e| e.to_string())?;
            let mut builder = portable_pty::CommandBuilder::new(&shell);
            for a in &argv {
                builder.arg(a);
            }
            builder.cwd(&cwd);
            for (k, v) in &env {
                builder.env(k, v);
            }
            // a server started from inside a Claude Code session inherits its markers — the pane's CLI
            // must not think it is a nested child ("transcript saving is off — inherited marker", 2026-09-19)
            for k in ["CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_ENTRYPOINT"] { builder.env_remove(k); }
            let child = pair.slave.spawn_command(builder).map_err(|e| e.to_string())?;
            let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
            let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
            Ok((Some(pair.master), Some(child), reader, writer))
        })()
    };

    let (master, child, reader, writer) = match spawn_result {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("spawn failed: {e} (cmd: {shell} {})", argv.join(" "));
            write_chunk(&state, pane_id, 0, &format!("⚠ {msg}\r\n"), 1).await;
            return json!({ "ok": false, "error": msg });
        }
    };

    let pid = child.as_ref().and_then(|c| c.process_id()).unwrap_or(0);
    let gen = next_gen();
    registry
        .live
        .lock()
        .unwrap()
        .insert(pane_id, Live { writer, master, child, remote, cmd: cmd.clone().unwrap_or_else(|| shell.clone()), gen, seq: 1, ready: false, quota: None, ask: false });
    registry.mcp_up.lock().unwrap().remove(&pane_id);   // a fresh start: the agent has not connected yet

    write_chunk(&state, pane_id, 0, &format!("$ {}\r\n", cmd.clone().unwrap_or_else(|| shell.clone())), 0).await;

    run_pipeline(state, registry, pane_id, gen, reader, cmd, false);

    json!({ "ok": true, "pane_id": pane_id, "pid": pid, "shell": shell })
}

/// Which `start` made an entry. A pipeline that ends removes only its own entry, so the end of an old terminal can never take
/// a newer terminal of the same pane out of the registry.
fn next_gen() -> u64 { static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1); GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed) }

/// Read the terminal's output (from the PTY, or from the sidecar host's attach stream), scan it for the composer, the
/// quota line and Codex's first-run prompts, flush it to the substrate and the pane's subscribers, and report the exit.
/// `already_ready`: a terminal re-attached after a serve restart is long past its boot, so the composer is not waited for.
fn run_pipeline(state: Arc<AppState>, registry: Arc<PtyRegistry>, pane_id: i64, gen: u64, mut reader: Box<dyn Read + Send>, cmd: Option<String>, already_ready: bool) {
    // Reader thread + async flusher. The blocking read cannot live on the runtime, and the
    // dispatch cannot live on the reader thread, so they are joined by a channel.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(Some(String::from_utf8_lossy(&buf[..n]).into_owned())).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = tx.send(None); // EOF -> the process exited
    });

    let st = state.clone();
    let reg = registry.clone();
    // THE CODEX FIRST-RUN PROMPTS (2026-09-19): a fresh box makes the Codex TUI ask "do you trust this
    // directory?" and "hooks need review" before anything runs; the pane's own first keystrokes used to
    // answer them (exit 0, nothing ran). The box and the hooks are antiloki's — answered once each.
    let is_codex = cmd.as_deref().map(|c| c.ends_with("codex")).unwrap_or(false);
    let mut answered: Vec<&'static str> = vec![];
    let mut recent = String::new();
    let ready_markers = composer_markers(cmd.as_deref().unwrap_or(""));
    let mut ready = ready_markers.is_empty() || already_ready; // a plain shell is ready at once
    if ready { registry.mark_ready(pane_id); }   // ...and says so: nothing below will, so a fresh terminal with no composer marker stayed `ready:false` for ever (#8)
    let mut boot = String::new();
    let is_cli = !ready_markers.is_empty(); // only a CLI's stream is scanned for its quota line
    let mut quota_seen = false; let mut tail = String::new();
    let mut ask_tail = String::new(); let mut ask_on = false;
    // Cursor has no composer marker, but its MCP approval prompt is read too (scanned for every CLI, not for a plain shell)
    let scans = is_cli || cmd.as_deref().map_or(false, |c| matches!(c.rsplit(['/', '\\']).next().unwrap_or(c).trim_end_matches(".exe"), "cursor-agent" | "cursor"));
    let mut dlg = String::new(); let mut dlg_open = false; let mut mcp_open = false;   // the CLI's "Switch model?" question, read from the stream so it works with or without the terminal host
    tokio::spawn(async move {
        let mut pending = String::new();
        loop {
            let timeout = tokio::time::sleep(std::time::Duration::from_millis(FLUSH_MS));
            tokio::pin!(timeout);
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(Some(chunk)) => {
                        if !ready {
                            boot.push_str(&chunk); trim_front(&mut boot, 16_000);
                            if composer_up(&boot, &ready_markers) { ready = true; boot = String::new(); reg.mark_ready(pane_id); }
                        }
                        // THE QUOTA LINE (2026-09-20): the CLI's own "usage limit" sentence, wherever it
                        // lands in the stream — a rolling window so a chunk boundary never hides it
                        if is_cli {
                            ask_tail.push_str(&chunk); trim_front(&mut ask_tail, 3000);
                            let on = asks_a_question(&ask_tail) || mcp_open; if on != ask_on { ask_on = on; reg.mark_ask(pane_id, on); }
                        }
                        if !quota_seen && is_cli {
                            tail.push_str(&chunk); trim_front(&mut tail, 6000);
                            if let Some(line) = quota_line(&tail) { quota_seen = true; reg.mark_quota(pane_id, line); }
                        }
                        if is_codex && answered.len() < 2 {
                            recent.push_str(&chunk); trim_front(&mut recent, 8000);
                            if let Some((key, reply)) = codex_prompt_answer(&recent, &answered) {
                                answered.push(key); recent.clear();
                                let reg2 = reg.clone();
                                tokio::spawn(async move { tokio::time::sleep(std::time::Duration::from_millis(400)).await; let _ = reg2.input(pane_id, reply); });
                            }
                        }
                        if scans {
                            dlg.push_str(&chunk); trim_front(&mut dlg, 6000);
                            if mcp_open || dlg.contains("MCP Server Approval") { let open = mcp_approval_open(&dlg); if open != mcp_open { mcp_open = open; ask_on = open || asks_a_question(&ask_tail); reg.mark_ask(pane_id, ask_on); crate::cowork::note_dialog(pane_id, open.then(|| MCP_DIALOG.to_string())); } }
                            /* an OPEN question is checked on every chunk: once its words left the window it was never looked at again, and the banner stayed for ever (2026-11-26) */ if dlg_open || dlg.contains("Switch") { let open = switch_dialog_open(&dlg); if open != dlg_open { dlg_open = open; crate::cowork::note_dialog(pane_id, open.then(|| SWITCH_DIALOG.to_string())); } }
                            if (dlg.contains("Set") || dlg.contains("Kept")) && crate::cowork::expecting_answer(pane_id) { if let Some(line) = model_result_line(&dlg) { crate::cowork::note_notice(pane_id, line); } }
                        }
                        pending.push_str(&chunk);
                        if pending.len() > FLUSH_BYTES {
                            flush(&st, &reg, pane_id, &mut pending).await;
                        }
                    }
                    _ => {
                        // EOF or channel closed: flush what is left, then report the exit.
                        flush(&st, &reg, pane_id, &mut pending).await;
                        // take our own entry (never a newer terminal's), then ask whoever owns the process how it ended
                        let (mine, superseded) = {
                            let mut live = reg.live.lock().unwrap();
                            let current = live.get(&pane_id).map(|e| e.gen);
                            if current == Some(gen) { (live.remove(&pane_id), false) } else { (None, current.is_some()) }
                        };
                        if superseded { break; } // this pane has a newer terminal now; this one's end is not its news
                        let code: i64 = match mine {
                            Some(e) if e.remote => tokio::task::spawn_blocking(move || pty_remote::exit_code(pane_id)).await.ok().flatten().unwrap_or(0),
                            Some(mut e) => e.child.as_mut().and_then(|c| c.wait().ok()).map(|s| s.exit_code() as i64).unwrap_or(0),
                            None => 0,
                        };
                        let seq = next_seq(&reg, pane_id);
                        reg.publish(pane_id, seq, &format!("\r\n(exit {code})\r\n"));
                        write_chunk(&st, pane_id, seq, &format!("\r\n(exit {code})\r\n"), 1).await;
                        break;
                    }
                },
                _ = &mut timeout => {
                    if !pending.is_empty() {
                        flush(&st, &reg, pane_id, &mut pending).await;
                    }
                }
            }
        }
    });

}

fn next_seq(registry: &Arc<PtyRegistry>, pane_id: i64) -> u64 {
    let mut live = registry.live.lock().unwrap();
    match live.get_mut(&pane_id) {
        Some(e) => {
            let s = e.seq;
            e.seq += 1;
            s
        }
        None => u64::MAX, // the entry is already gone (exit path); seq only has to be monotonic
    }
}

async fn flush(state: &Arc<AppState>, registry: &Arc<PtyRegistry>, pane_id: i64, pending: &mut String) {
    if pending.is_empty() {
        return;
    }
    let chunk = std::mem::take(pending);
    let seq = next_seq(registry, pane_id);
    registry.publish(pane_id, seq, &chunk);
    write_chunk(state, pane_id, seq, &chunk, 0).await;
}

/// RFC 0007 M1: at boot, attach to every pane terminal the host kept alive through the restart. Returns how many.
pub async fn reattach_host_terminals(state: Arc<AppState>, registry: Arc<PtyRegistry>) -> usize {
    if !pty_remote::enabled() { return 0; }
    let terms = match tokio::task::spawn_blocking(pty_remote::list_running).await { Ok(Ok(t)) => t, _ => return 0 };
    let mut n = 0;
    for t in terms {
        let Some(pane_id) = pty_remote::pane_of(&t.id) else { continue };
        if registry.live.lock().unwrap().contains_key(&pane_id) { continue }
        let (rows, cols) = (t.rows, t.cols);
        let stream = match tokio::task::spawn_blocking(move || pty_remote::attach_existing(pane_id, rows, cols)).await { Ok(Ok(s)) => s, _ => continue };
        let Ok(writer) = stream.try_clone() else { continue };
        let cmd = t.argv.first().cloned();
        let gen = next_gen();
        // above every seq the previous serve numbered for this pane, so its history keeps its order
        let seq = (crate::cli::now_ms() as u64).max(2);
        registry.live.lock().unwrap().insert(pane_id, Live { writer: Box::new(writer), master: None, child: None, remote: true,
            cmd: cmd.clone().unwrap_or_default(), gen, seq: seq + 1, ready: true, quota: None, ask: false });
        registry.mcp_up.lock().unwrap().insert(pane_id);   // reattached after a serve restart: the agent connected long ago and will not say so again
        write_chunk(&state, pane_id, seq, "\r\n↻ reattached: the serve restarted, this terminal did not\r\n", 0).await;
        run_pipeline(state.clone(), registry.clone(), pane_id, gen, Box::new(stream), cmd, true);
        n += 1;
    }
    n
}

// ── what the composer sends into a terminal (RFC 0007 §5.4) ───────────────────────────────────────────────

/// Control bytes are removed from pasted text: an escape inside a paste could end it early (`ESC[201~`) and turn the rest into
/// keystrokes. Newlines and tabs stay; CRLF and CR become LF. The same rules as the front end's `send.ts`.
pub fn sanitize_paste(text: &str) -> String {
    let kept: String = text.chars().filter(|c| !matches!(*c as u32, 0..=8 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f)).collect();
    kept.replace("\r\n", "\n").replace('\r', "\n")
}
/// (submit key, pause between the paste and the submit key in ms). Claude, Codex and OpenCode were confirmed to take a bracketed paste and Enter.
pub fn send_quirks(cmd: &str) -> (&'static str, u64) {
    let base = cmd.rsplit(['/', '\\']).next().unwrap_or(cmd);
    match base {
        "claude" | "claude.exe" | "claude.cmd" | "codex" | "codex.exe" | "codex.cmd" => ("\r", 150),
        _ => ("\r", 120),
    }
}
/// A plain shell (not an agent's TUI): it may not understand a bracketed paste, and would print the markers.
pub fn is_plain_shell(cmd: &str) -> bool {
    let base = cmd.rsplit(['/', '\\']).next().unwrap_or(cmd).trim_end_matches(".exe");
    matches!(base, "bash" | "zsh" | "fish" | "sh" | "dash" | "ksh" | "nu" | "pwsh" | "powershell" | "cmd")
}
/// The writes that send `text`: the paste now, the submit key after the engine's pause. Nothing for empty text.
pub fn send_plan(cmd: &str, text: &str) -> Vec<(String, u64)> {
    let t = sanitize_paste(text);
    let t = t.trim();
    if t.is_empty() { return vec![]; }
    let (submit, delay) = send_quirks(cmd);
    // an agent's composer takes a bracketed paste (so a newline in the text is not a submit); a plain shell may not understand the markers and would print them, so it is typed to
    let body = if is_plain_shell(cmd) { t.to_string() } else { format!("\x1b[200~{t}\x1b[201~") };
    vec![(body, 0), (submit.to_string(), delay)]
}

/// Is a question open on the pane's screen right now? `false` when there is no screen to read.
async fn screen_asks(pane_id: i64) -> bool {
    if !crate::pty_remote::enabled() { return false; }
    tokio::task::spawn_blocking(move || crate::pty_remote::screen_rows(pane_id)).await.ok().flatten().map_or(false, |rows| crate::cowork::blocked_from_screen(&rows).is_some())
}

/// `POST /api/v1/cockpit/pty/send {pane_id, text}`: type the text into the pane's terminal as a person's paste would, once its
/// composer is up. The pre-check has already run (or been skipped) in the front end; this is only the typing.
pub async fn pty_send(reg: Arc<PtyRegistry>, body: Bytes) -> Response {
    let b = body_json(&body);
    let pane_id = num(b.get("pane_id"));
    let text = b.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let reply = |v: Value| (StatusCode::CREATED, Json(v)).into_response();
    let cmd = reg.live.lock().unwrap().get(&pane_id).map(|e| e.cmd.clone());
    let Some(cmd) = cmd else { return reply(json!({ "ok": false, "error": "no live terminal for this pane — start it first" })) };
    let plan = send_plan(&cmd, &text);
    if plan.is_empty() { return reply(json!({ "ok": false, "error": "nothing to send" })); }
    // keystrokes typed before the CLI's composer paints are eaten by its boot (2026-09-19); a plain shell has no composer to wait for
    if !composer_markers(&cmd).is_empty() {
        let wait = b.get("wait_s").and_then(|v| v.as_u64()).unwrap_or(20).min(120);
        let t0 = std::time::Instant::now();
        while reg.status(pane_id)["ready"] != json!(true) {
            if t0.elapsed() > std::time::Duration::from_secs(wait) { return reply(json!({ "ok": false, "error": "the CLI is not ready for input yet" })); }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    // NEVER TYPE OVER THE AGENT'S OWN QUESTION (2026-11-25, seen live: the takeover text reached Cursor while its "MCP Server Approval Required" menu was up; its letters were the menu's hotkeys and `q` quit the agent). Trust-this-folder, MCP approval and resume menus all count.
    if !is_plain_shell(&cmd) {
        let wait = b.get("wait_s").and_then(|v| v.as_u64()).unwrap_or(20).min(120);
        let t0 = std::time::Instant::now();
        // the flag comes from the latest bytes printed and is lost when the CLI repaints under a question that is still open, so the screen itself is read too (found 2026-11-27: a trust question got antiloki's team memory typed into it)
        while reg.live.lock().unwrap().get(&pane_id).map_or(false, |e| e.ask) || screen_asks(pane_id).await {
            if t0.elapsed() > std::time::Duration::from_secs(wait) { return reply(json!({ "ok": false, "error": "the agent is asking its own question: answer it in its terminal first" })); }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }
    let mut bytes = 0usize;
    for (i, (data, delay)) in plan.into_iter().enumerate() {
        if delay > 0 { tokio::time::sleep(std::time::Duration::from_millis(delay)).await; }
        // a question that opened between the text and its Enter is not answered by that Enter
        if i > 0 && !is_plain_shell(&cmd) && screen_asks(pane_id).await { return reply(json!({ "ok": false, "error": "the agent is asking its own question: answer it in its terminal first" })); }
        let r = reg.input(pane_id, &data);
        if r["ok"] != json!(true) { return reply(r); }
        bytes += data.len();
    }
    crate::cowork::note_send(pane_id, &text);
    reply(json!({ "ok": true, "bytes": bytes }))
}

/// `POST /api/v1/cockpit/pane/mcp-up {pane_id}`: the tool helper of that pane's agent says it is connected.
pub async fn pane_mcp_up(reg: Arc<PtyRegistry>, body: Bytes) -> Response { let b = body_json(&body); reg.mark_mcp(num(b.get("pane_id"))); (StatusCode::OK, Json(json!({ "ok": true }))).into_response() }
/// `GET /api/v1/cockpit/pane/mcp?pane_id=`: is it connected.
pub async fn pane_mcp_get(reg: Arc<PtyRegistry>, q: HashMap<String, String>) -> Response {
    let id = q.get("pane_id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0); let mut v = reg.mcp_state(id);
    // the screen too: the cover "Connecting antiloki's tools" must never lie over a question the person has to answer
    if v["running"] == true && v["mcp"] != true && v["ask"] != true && screen_asks(id).await { v["ask"] = json!(true); }
    (StatusCode::OK, Json(v)).into_response()
}

/// `GET /api/v1/cockpit/pty/host`: who owns the terminals, and what the host holds.
pub async fn pty_host_status() -> Response {
    let v = tokio::task::spawn_blocking(pty_remote::status).await.unwrap_or_else(|e| json!({ "mode": "error", "error": e.to_string() }));
    (StatusCode::OK, Json(v)).into_response()
}

// ── HTTP wrappers ───────────────────────────────────────────────────────────

use futures_util::StreamExt;
use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

/// The `resolveEngine(key)` half pty/start needs: which binary to spawn (and what goes before the arguments the caller gave), and whether the engine
/// routes through the antiloki AI-proxy (a proxied engine never sees the real BYOK key). EVERY INSTALLED AGENT HAS A DOOR HERE (operator 2026-11-10: "release all installed on this machine"): until now
/// only Claude Code, Codex and OpenCode did, so a Gemini/Antigravity, Cursor or Grok pane opened a plain shell instead of the CLI, and the DeepSeek harness did not exist.
pub struct Launch { pub bin: String, pub lead: Vec<String>, pub proxied: bool }
fn resolve_engine(key: &str) -> Option<Launch> {
    let l = |bin: &str, lead: &[&str], proxied: bool| Some(Launch { bin: bin.to_string(), lead: lead.iter().map(|s| s.to_string()).collect(), proxied });
    // the first of an engine's binaries that is on this machine (Antigravity's `agy` before the legacy `gemini`; Cursor's `cursor-agent`, or `agent`), else the preferred name so the error says what was wanted
    let first = |e: crate::cli::Engine| e.bins().iter().find(|b| crate::proc::which(b).is_some()).copied().unwrap_or(e.bin()).to_string();
    match key {
        // the user's own OpenCode login unless a BYOK key is on the box (2026-09-19): the proxy env
        // (OPENAI_API_KEY) otherwise makes OpenCode enable an "openai" provider at the proxy → 404s
        "opencode" => l("opencode", &[], antiloki_rs::cockpit_cockpit_pane_add::opencode_uses_proxy("opencode", None)),
        "claude-code" => l("claude", &[], false),
        "codex" => l("codex", &[], false),
        "opencode-direct" => l("opencode", &[], true),
        "gemini" => Some(Launch { bin: first(crate::cli::Engine::Gemini), lead: vec![], proxied: false }),
        "cursor" => Some(Launch { bin: first(crate::cli::Engine::Cursor), lead: vec![], proxied: false }),
        "grok" => l("grok", &[], false),
        // THE DEEPSEEK HARNESS (`@deepseek-ai/dsh` 0.2): there is no terminal UI in this release, only the `web` profile (a local web app, which is the profile this machine has) and the one-shot `headless`.
        // The pane serves the web app without opening a browser; the terminal prints its address and the screen can show it in a Preview pane.
        "deepseek" => crate::engines::deepseek_bin().map(|b| Launch { bin: b, lead: vec!["web".into(), "--no-open".into()], proxied: false }).or_else(|| l("dsh", &["web", "--no-open"], false)),
        // an ADDED provider (RFC 0023): only one the person confirmed and antiloki tested has a door
        k => crate::providers::launch(k).map(|(bin, lead)| Launch { bin, lead, proxied: false }),
    }
}

/// Did the person set Antigravity's screen mode in their own settings?
fn agy_chose_screen_mode() -> bool {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).and_then(|h| std::fs::read_to_string(std::path::PathBuf::from(h).join(".gemini/antigravity-cli/settings.json")).ok()).is_some_and(|t| t.contains("\"altScreenMode\""))
}

/// Why this engine key may not be spawned in a terminal: the operator switched its seat off. `None` for a key that is not a seat. Pure over the switch - tested.
/// ONLY THE SEAT SWITCH, NOT `model_blocked` (2026-10-01 review): that gate also refuses OpenCode whenever Zen is off - the default - and a terminal's `opencode` runs on the operator's own OpenCode login, `opencode-direct` on a BYOK key through the proxy; neither is Zen-metered. The metered-gateway rule belongs to routed calls.
pub fn switched_off(engine_key: &str, off: impl Fn(crate::cli::Engine) -> bool) -> Option<String> {
    crate::cli::engine_of(&json!(engine_key)).filter(|e| off(*e)).map(|e| format!("{} is switched off (Settings › Seats).", crate::cli::provider_name(e)))
}

/// `Number(x)` over a field that may arrive as a JSON number or a string.
fn num(v: Option<&Value>) -> i64 {
    v.and_then(|x| x.as_i64())
        .or_else(|| v.and_then(|x| x.as_str()).and_then(|s| s.trim().parse().ok()))
        .unwrap_or(0)
}

fn body_json(body: &Bytes) -> Value {
    serde_json::from_slice(body).unwrap_or_else(|_| json!({}))
}

pub async fn pty_status(state: Arc<AppState>, reg: Arc<PtyRegistry>, q: HashMap<String, String>) -> Response {
    let pane_id: i64 = q.get("pane_id").and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    let mut st = reg.status(pane_id);
    // no quota seen on this serve: a headless goal run may have hit the window in the pane's box (quota.json)
    if st.get("quota").map(Value::is_null).unwrap_or(true) {
        let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
        let cwd = engine_call(&state, "system.cockpit.pane.get", json!({ "id": pane_id }), ctx).await.ok().map(|(v, _)| v).and_then(|v| v.get("worktree_path").and_then(Value::as_str).map(String::from));
        if let Some(q) = cwd.and_then(|c| quota_file(std::path::Path::new(&c))) { st["quota"] = q; }
    }
    Json(st).into_response()
}
/// The box's `.antiloki/quota.json` when it is recent (a plan window is hours, not days). Pure over the file.
pub fn quota_file(cwd: &std::path::Path) -> Option<Value> {
    let v = serde_json::from_str::<Value>(&std::fs::read_to_string(cwd.join(".antiloki").join("quota.json")).ok()?).ok()?;
    let t = v.get("t").and_then(Value::as_i64)?;
    if crate::cli::now_ms() - t > 6 * 3_600_000 { return None; }
    Some(v)
}

/// `GET /api/v1/cockpit/pty/stream?pane_id=&after=` — the pane's output as Server-Sent Events: the
/// backlog after `after` (the stored chunk id) first, then every chunk as it is flushed; `id:` carries
/// the stored id for the backlog and `seq` for live chunks, `event: exit` when the process ends. A
/// keepalive every 15 s. The FE falls back to polling when the stream cannot connect.
/// The backlog TAIL of a pane: `(trimmed rows, rows (id, chunk, done), last id)`. Paged (the telemetry
/// answers 400 rows a call) — up to 6000 rows. Only the tail is replayed (2026-09-20): a TUI's whole
/// history painted into a terminal of another width is garbage; the last ~64 KB is what the screen
/// would have shown, and the CLI redraws on its next output anyway.
pub(crate) async fn backlog_tail(state: &Arc<AppState>, pane_id: i64, after: i64) -> (usize, Vec<(i64, String, bool)>, i64) {
    const TAIL_BYTES: usize = 64 * 1024;
    let mut rows: Vec<(i64, String, bool)> = vec![];
    let mut last_id = after;
    for _page in 0..15 {
        let backlog = engine_call(state, "system.gate.telemetry", json!({ "op": "build_live", "contract_slug": format!("pane-pty-{pane_id}"), "after": last_id, "phase": "pty" }), json!({ "actor": "web-fe", "source": "web-fe" })).await.map(|(v, _)| v).unwrap_or(Value::Null);
        let backlog = backlog.get("result").cloned().unwrap_or(backlog);
        let chunks = backlog.get("chunks").and_then(Value::as_array).cloned().unwrap_or_default();
        for c in &chunks {
            let id = c.get("id").and_then(Value::as_i64).unwrap_or(0); last_id = last_id.max(id);
            rows.push((id, c.get("chunk").and_then(Value::as_str).unwrap_or("").to_string(), c.get("done").and_then(Value::as_i64) == Some(1)));
        }
        if chunks.len() < 400 { break; }
    }
    let mut keep_from = rows.len(); let mut bytes = 0usize;
    while keep_from > 0 && bytes < TAIL_BYTES { keep_from -= 1; bytes += rows[keep_from].1.len(); }
    let tail = rows.split_off(keep_from);
    (keep_from, tail, last_id)
}

/// `GET /api/v1/cockpit/pty/streams?sub=<id>` — THE MULTIPLEXED STREAM: one SSE per browser tab; the
/// panes it follows are added and removed through `pty/streams/sub`. Every event is `event: pane`
/// with `{pane, kind: chunk|ready|exit|trimmed|lagged, id?, data?}`; the first event is `hello` —
/// the tab re-adds its panes on it (a reconnect lands on a fresh, empty subscription).
pub async fn pty_streams(reg: Arc<PtyRegistry>, q: HashMap<String, String>) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    let sub = q.get("sub").cloned().unwrap_or_default();
    if sub.is_empty() || sub.len() > 64 { return (StatusCode::BAD_REQUEST, Json(json!({ "error": "sub required" }))).into_response(); }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    {
        let mut subs = reg.subs.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(old) = subs.remove(&sub) { for (_, c) in old.panes { c.store(true, std::sync::atomic::Ordering::Relaxed); } }
        subs.insert(sub.clone(), Sub { tx, panes: HashMap::new() });
    }
    let hello = futures_util::stream::iter(vec![Ok::<Event, std::convert::Infallible>(Event::default().event("hello").data(sub.clone()))]);
    let live = tokio_stream::wrappers::UnboundedReceiverStream::new(rx).map(|data| Ok::<Event, std::convert::Infallible>(Event::default().event("pane").data(data)));
    Sse::new(hello.chain(live)).keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15))).into_response()
}

/// `POST /api/v1/cockpit/pty/streams/sub` `{sub, add: [{pane_id, after}], remove: [pane_id]}` — each
/// added pane gets its backlog tail then its live output on the tab's stream, until removed or the
/// stream is gone.
pub async fn pty_streams_sub(state: Arc<AppState>, reg: Arc<PtyRegistry>, agents: Arc<crate::agent::AgentRegistry>, body: Bytes) -> Response {
    let b = body_json(&body);
    let sub = b.get("sub").and_then(Value::as_str).unwrap_or("").to_string();
    let removes: Vec<i64> = b.get("remove").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default();
    // `agent: true` on an add follows the pane's chat AGENT session (agent.rs) instead of its PTY; the
    // two are keyed apart (a negative key for the agent) so a tab may follow both
    let adds: Vec<(i64, i64, bool)> = b.get("add").and_then(Value::as_array).map(|a| a.iter().map(|x| (num(x.get("pane_id")), num(x.get("after")), x.get("agent").and_then(Value::as_bool).unwrap_or(false))).filter(|(p, _, _)| *p > 0).collect()).unwrap_or_default();
    let mut started = vec![];
    {
        let mut subs = reg.subs.lock().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = subs.get_mut(&sub) else { return (StatusCode::NOT_FOUND, Json(json!({ "ok": false, "error": "no such stream — reconnect and re-add on hello" }))).into_response() };
        for p in &removes { for k in [*p, -*p] { if let Some(c) = entry.panes.remove(&k) { c.store(true, std::sync::atomic::Ordering::Relaxed); } } }
        for (p, after, agent) in &adds {
            let key = if *agent { -*p } else { *p };
            if let Some(c) = entry.panes.remove(&key) { c.store(true, std::sync::atomic::Ordering::Relaxed); }
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            entry.panes.insert(key, cancel.clone());
            if *agent { crate::agent::follow(state.clone(), agents.clone(), *p, *after as u64, entry.tx.clone(), cancel); } else { started.push((*p, *after, entry.tx.clone(), cancel)); }
        }
    }
    for (pane_id, after, tx, cancel) in started {
        let (state, reg) = (state.clone(), reg.clone());
        tokio::spawn(async move {
            let stopped = || cancel.load(std::sync::atomic::Ordering::Relaxed);
            // `id` = a backlog row (the telemetry's global autoincrement) — the tab dedups on it;
            // `seq` = a live chunk (per PTY life, restarts at 1) — NEVER compared with an id: comparing
            // the two spaces dropped every live chunk after a restart (frozen screen, keys "useless", 2026-09-20)
            let ev = |kind: &str, id: Option<i64>, data: Option<&str>| json!({ "pane": pane_id, "kind": kind, "id": id, "data": data }).to_string();
            let live_ev = |seq: u64, data: &str| json!({ "pane": pane_id, "kind": "chunk", "seq": seq, "data": data }).to_string();
            // subscribe BEFORE reading the backlog so nothing flushed in between is lost (a duplicate
            // is harmless: the FE ignores a chunk whose id it already wrote)
            let mut rx = reg.subscribe(pane_id);
            let (trimmed, rows, last_id) = backlog_tail(&state, pane_id, after).await;
            if stopped() { return; }
            if trimmed > 0 && tx.send(ev("trimmed", Some(trimmed as i64), None)).is_err() { return; }
            for (id, chunk, done) in rows {
                if tx.send(ev("chunk", Some(id), Some(&chunk))).is_err() { return; }
                if done && tx.send(ev("exit", None, Some(""))).is_err() { return; }
            }
            if tx.send(ev("ready", Some(last_id), None)).is_err() { return; }
            loop {
                if stopped() { return; }
                match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
                    Ok(Ok((seq, chunk))) => {
                        let exit = chunk.contains("(exit ") && chunk.ends_with("\r\n");
                        if tx.send(if exit { ev("exit", None, Some(&chunk)) } else { live_ev(seq, &chunk) }).is_err() { return; }
                    }
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => { if tx.send(ev("lagged", None, None)).is_err() { return; } }
                    Ok(Err(_)) => return,
                    Err(_elapsed) => {} // idle — check the cancel flag again
                }
            }
        });
    }
    (StatusCode::CREATED, Json(json!({ "ok": true, "added": adds.len(), "removed": removes.len() }))).into_response()
}

pub async fn pty_stream(state: Arc<AppState>, reg: Arc<PtyRegistry>, q: HashMap<String, String>) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    let pane_id = q.get("pane_id").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    let after = q.get("after").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    // subscribe BEFORE reading the backlog so nothing flushed in between is lost (a duplicate is
    // harmless: the FE ignores a chunk whose id it already wrote)
    let rx = reg.subscribe(pane_id);
    let (keep_from, rows, last_id) = backlog_tail(&state, pane_id, after).await;
    let mut first: Vec<Result<Event, std::convert::Infallible>> = vec![];
    if keep_from > 0 { first.push(Ok(Event::default().event("trimmed").data(keep_from.to_string()))); }
    for (id, chunk, done) in rows {
        first.push(Ok(Event::default().id(id.to_string()).data(chunk)));
        if done { first.push(Ok(Event::default().event("exit").data(""))); }
    }
    first.push(Ok(Event::default().event("ready").data(last_id.to_string())));
    let live = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|r| async move {
        match r {
            Ok((seq, chunk)) => { let exit = chunk.contains("(exit ") && chunk.ends_with("\r\n"); Some(Ok::<Event, std::convert::Infallible>(if exit { Event::default().event("exit").data(chunk) } else { Event::default().id(format!("s{seq}")).data(chunk) })) }
            Err(_lagged) => Some(Ok(Event::default().event("lagged").data(""))),
        }
    });
    let stream = futures_util::stream::iter(first).chain(live);
    Sse::new(stream).keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15))).into_response()
}

pub async fn pty_input(reg: Arc<PtyRegistry>, body: Bytes) -> Response {
    let b = body_json(&body);
    let data = b.get("data").and_then(|v| v.as_str()).unwrap_or("");
    crate::mailbox::typed_by_person(num(b.get("pane_id")), data);   // a message never lands on a half-typed line
    (StatusCode::CREATED, Json(reg.input(num(b.get("pane_id")), data))).into_response()
}

pub async fn pty_resize(reg: Arc<PtyRegistry>, body: Bytes) -> Response {
    let b = body_json(&body);
    let out = reg.resize(num(b.get("pane_id")), num(b.get("cols")), num(b.get("rows")));
    (StatusCode::CREATED, Json(out)).into_response()
}

pub async fn pty_kill(reg: Arc<PtyRegistry>, body: Bytes) -> Response {
    let b = body_json(&body);
    (StatusCode::CREATED, Json(reg.kill(num(b.get("pane_id"))))).into_response()
}

/// `POST /api/v1/cockpit/pty/start` — resolve the pane, pick the engine binary, spawn.
pub async fn cockpit_pty_start(state: Arc<AppState>, reg: Arc<PtyRegistry>, body: Bytes) -> Response {
    let b = body_json(&body);
    let pane_id = num(b.get("pane_id"));
    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });

    let pane = match engine_call(&state, "system.cockpit.pane.get", json!({ "id": pane_id }), ctx).await {
        Ok((v, _)) if !v.is_null() => v,
        _ => {
            let e = json!({ "ok": false, "error": format!("pane {pane_id} not found") });
            return (StatusCode::CREATED, Json(e)).into_response();
        }
    };

    // a pane whose worktree could not be made ("worktree pending") works in the PROJECT, never in the folder the server happens to run from
    let root = crate::agents_http::active_root(&state).await;
    let cwd = pane_folder(pane.get("worktree_path").and_then(|v| v.as_str()), &crate::shared_folder::folder_of(&root, &pane), &cwd_string());

    let engine_key = b
        .get("agent")
        .and_then(|v| v.as_str())
        .or_else(|| pane.get("pane_kind").and_then(|v| v.as_str()))
        .unwrap_or("");
    let engine = resolve_engine(engine_key);
    // A SWITCHED-OFF SEAT IS NEVER SPAWNED, TERMINAL INCLUDED (2026-10-01, the Claude-off audit): the chat panes leave a seat the operator switched off (`chat_pane::leave_blocked_seat`) and every
    // routed call refuses it (`cli::run_cli`), but this door had no check - opening a Claude terminal ran `claude` with the switch off. The reason is the one Settings prints. It sits BEFORE the context bake: a refused terminal bakes nothing.
    if let Some(why) = switched_off(engine_key, crate::cli::seat_off) {
        return (StatusCode::CREATED, Json(json!({ "ok": false, "error": why }))).into_response();
    }

    // A LIMIT THAT IS NOT HELD IS SAID ON THE PANE (queue 9b, 2026-11-28): the person is never left believing an agent is boxed when it is not
    note_limits_off(pane_id, !crate::chat_pane::pane_scope_globs(&pane).is_empty() && !limits_enforced(engine_key, pane.get("worktree_path").and_then(|v| v.as_str()).is_some_and(|w| !w.is_empty())));

    // CONTEXT PACK (parity with the BE, 2026-09-18): bake before the CLI boots so it always starts
    // on a fresh pack (cheap when the hash says nothing changed). A failed bake never blocks the
    // boot — the brief on disk stands.
    let boxed = pane.get("worktree_path").and_then(|v| v.as_str()).is_some_and(|w| !w.is_empty());
    let pack = if boxed || engine.is_some() {
        engine_call(&state, "system.cockpit.pane.context.bake", json!({ "pane_id": pane_id, "shared": !boxed, "folder": cwd.clone() }), json!({ "actor": "web-fe", "source": "web-fe" })).await.map(|(v, _)| v).unwrap_or(Value::Null)
    } else { Value::Null };

    let mut env: HashMap<String, String> = HashMap::new();
    if engine.as_ref().map(|e| e.proxied).unwrap_or(false) {
        // The pane talks to THIS server's per-pane proxy with a placeholder key; the real BYOK
        // key stays server-side, metered and killable.
        let port = std::env::var("ANTILOKI_SERVE_PORT")
            .or_else(|_| std::env::var("ANTILOKI_BE_PORT"))
            .unwrap_or_else(|_| "41931".into());
        env.insert(
            "OPENAI_BASE_URL".into(),
            format!("http://127.0.0.1:{port}/api/v1/ai-proxy/pane/{pane_id}"),
        );
        env.insert("OPENAI_API_KEY".into(), "antiloki-pane-proxy".into());
    }
    // A LOCAL MODEL: Claude Code on the local server (the model the person chose for this pane starts with `local:`)
    if engine_key == "claude-code" { let m = pane.get("settings_json").and_then(Value::as_str).map(crate::pane_model::model_of).unwrap_or_default(); if let Some(tag) = crate::local_panes::tag_of(&m) { let eff = crate::local_panes::context_tag(tag); env.extend(crate::local_panes::env_for(&crate::local_panes::base(), &eff)); } }
    merge_env(&b, &mut env);
    // an Antigravity pane: its one shared MCP entry cannot name a pane, so the pane's process does (see mcp_install::ensure_agy)
    if engine.is_some() { env.insert("ANTILOKI_PANE_ID".into(), pane_id.to_string()); }   // the process says its pane (the watcher, and Antigravity's shared MCP entry)
    if engine_key == "gemini" { if let Some(e) = &engine { if let Err(err) = crate::mcp_install::ensure_agy(&e.bin).await { eprintln!("antiloki-serve: could not give Antigravity antiloki's tools: {err}"); } } }

    let cmd = engine.as_ref().map(|e| e.bin.clone()).or_else(|| b.get("cmd").and_then(|v| v.as_str()).map(String::from));

    // engine-specific flags carry the pack into the CLI's own channels (Claude: system prompt +
    // MCP config; Codex: MCP overrides; OpenCode reads opencode.json itself), and the launch is recorded
    let mut args = string_array(&b, "args");
    if let Some(e) = &engine { for (i, a) in e.lead.iter().enumerate() { args.insert(i, a.clone()); } }
    if engine.is_some() { args.extend(crate::agents_http::context_args_for(engine_key, &pack, &crate::agents_http::Delegation::of_pane(&pane))); }
    // ANTIGRAVITY'S PROMPT STAYS AT THE BOTTOM (operator 2026-11-26): its default draws from the top and leaves the lower half of the pane empty. Its full-screen mode anchors the prompt; set for this session only
    // (`--settings` is never saved), and not at all when the person chose a mode of their own.
    if engine_key == "gemini" && engine.as_ref().is_some_and(|e| e.bin == "agy") && !args.iter().any(|x| x == "--settings") && !agy_chose_screen_mode() { args.push("--settings".into()); args.push(r#"{"altScreenMode":"always"}"#.into()); }
    // THE MODEL THE PERSON CHOSE for this agent (the button above it): the engine's own flag, before the rest
    if engine.is_some() { let m = pane.get("settings_json").and_then(Value::as_str).map(crate::pane_model::model_of).unwrap_or_default(); let local = crate::local_panes::tag_of(&m).map(|t| (t.to_string(), crate::local_panes::context_tag(t))); for (i, a) in crate::pane_model::launch_args(engine_key, &m).into_iter().enumerate() { let a = match &local { Some((t, eff)) if a == *t => eff.clone(), _ => a }; args.insert(i, a); } }
    // MAX (2026-09-20): `settings.max` → the CLI's own "skip permissions" mode (the hook's scope is ** by the same click)
    let max_mode = pane.get("settings_json").and_then(Value::as_str).and_then(|t| serde_json::from_str::<Value>(t).ok()).and_then(|v| v.get("max").and_then(Value::as_bool)).unwrap_or(false);
    if max_mode { match engine_key { "claude-code" => { args.push("--permission-mode".into()); args.push("bypassPermissions".into()); } "codex" => { args.push("--dangerously-bypass-approvals-and-sandbox".into()); } _ => {} } }
    // CODEX TRUSTS ITS BOX (2026-09-19): a fresh worktree makes the Codex TUI ask "do you trust this
    // directory?" and the pane's first keystrokes answered it (exit 0, nothing ran). The box is ours.
    // THE PANE'S OWN SESSION (2026-09-20, the handoff): Claude's TUI is started with a known session id
    // so the pane's conversation can be found among the goal runs' headless sessions in the same dir;
    // every engine's boot is recorded so the handoff knows which store to read
    // A RESTART RESUMES (2026-09-20: a serve restart kills every agent's PTY — the operator's terminal
    // went black mid-work): when the box's last boot was Claude with a pinned session that has a
    // transcript on disk, the TUI is started with `--resume <id>` and the conversation continues.
    // `fresh: true` on the request skips that (a new conversation on purpose).
    let mut resumed: Option<String> = None;
    if engine.is_some() && boxed {
        let fresh = b.get("fresh").and_then(Value::as_bool).unwrap_or(false);
        let prior = antiloki_rs::cockpit__helpers_engine_handoff::pty_session(std::path::Path::new(&cwd)).filter(|(e, _)| e == engine_key && e == "claude-code");
        let prior = prior.filter(|(_, id)| resume_transcript_exists(&cwd, id)).or_else(|| if fresh || engine_key != "claude-code" { None } else { crate::cowork::adopt_earlier_session(&cwd, pane_id).map(|id| (engine_key.to_string(), id)) });
        let resumable = prior.filter(|(_, id)| !fresh && resume_transcript_exists(&cwd, id)).map(|(_, id)| id);
        let sid = match resumable {
            Some(id) => { args.push("--resume".into()); args.push(id.clone()); resumed = Some(id.clone()); id }
            None => {
                let sid = antiloki_rs::cockpit__helpers_engine_handoff::session_uuid(&format!("pane {pane_id} · {} · {}", crate::cli::now_ms(), std::process::id()));
                if engine_key == "claude-code" { args.push("--session-id".into()); args.push(sid.clone()); }
                sid
            }
        };
        let _ = std::fs::create_dir_all(std::path::Path::new(&cwd).join(".antiloki"));
        // WHERE THE ENGINE'S OWN PROMPT HOOK FINDS THE SERVE (the pre-check at the engine boundary, for anything typed straight into the terminal)
        let port = std::env::var("ANTILOKI_SERVE_PORT").or_else(|_| std::env::var("ANTILOKI_BE_PORT")).unwrap_or_else(|_| "41931".into());
        let _ = std::fs::write(std::path::Path::new(&cwd).join(".antiloki").join("serve.json"), json!({ "url": format!("http://127.0.0.1:{port}"), "pane_id": pane_id }).to_string());
        let _ = std::fs::write(std::path::Path::new(&cwd).join(".antiloki").join("pty-session.json"), json!({ "engine": engine_key, "session_id": if engine_key == "claude-code" { sid } else { String::new() }, "t": crate::cli::now_ms() }).to_string());
    }
    // AN AGENT IN THE SHARED FOLDER (no copy of its own): its start is stamped so its chat view reads only its own conversation, and Claude gets a named one that a restart continues
    if engine.is_some() && !boxed {
        let fresh = b.get("fresh").and_then(Value::as_bool).unwrap_or(false);
        let prior = crate::shared_folder::session_of(&cwd, pane_id).filter(|id| !fresh && resume_transcript_exists(&cwd, id));
        if engine_key == "claude-code" {
            let sid = match prior {
                Some(id) => { args.push("--resume".into()); args.push(id.clone()); resumed = Some(id.clone()); id }
                None => { let id = antiloki_rs::cockpit__helpers_engine_handoff::session_uuid(&format!("pane {pane_id} · {} · {}", crate::cli::now_ms(), std::process::id())); args.push("--session-id".into()); args.push(id.clone()); id }
            };
            crate::shared_folder::stamp(&cwd, pane_id, engine_key, &sid, crate::cli::now_ms() as u64);
        } else {
            // the other engines name their own conversations: the one this agent began since its first start is continued, never another agent's
            let (folder, eng) = (cwd.clone(), engine_key.to_string());
            let again = if fresh { None } else { tokio::task::spawn_blocking(move || earlier_conversation(&folder, pane_id, &eng, |w| crate::cowork::conversation_id(&eng, &folder, w))).await.ok().flatten() };
            match again {
                Some(id) => { resume_args(&mut args, engine.as_ref().map_or(0, |e| e.lead.len()), engine_key, &id); crate::shared_folder::stamp_resume(&cwd, pane_id, engine_key, &id); resumed = Some(id); }
                None => crate::shared_folder::stamp(&cwd, pane_id, engine_key, "", crate::cli::now_ms() as u64),
            }
        }
        let sid = crate::shared_folder::session_of(&cwd, pane_id).unwrap_or_default();
        // ITS OWN RECORDS (operator 2026-11-27: agents of a thread share its folder): prompts, answers, trace and meter go to a folder of this agent's, so usage, cost and sagas are per agent here too
        let records = crate::shared_folder::records(&cwd, pane_id); let _ = std::fs::create_dir_all(&records);
        if std::path::Path::new(&cwd).join(".git").is_dir() { antiloki_rs::cockpit_cockpit_pane_context_bake::git_exclude_line(std::path::Path::new(&cwd), ".antiloki/"); }
        let port = std::env::var("ANTILOKI_SERVE_PORT").or_else(|_| std::env::var("ANTILOKI_BE_PORT")).unwrap_or_else(|_| "41931".into());
        let _ = std::fs::write(records.join("serve.json"), json!({ "url": format!("http://127.0.0.1:{port}"), "pane_id": pane_id }).to_string());
        let _ = std::fs::write(records.join("pty-session.json"), json!({ "engine": engine_key, "session_id": sid, "t": crate::cli::now_ms() }).to_string());
        env.insert("ANTILOKI_RECORDS".into(), records.to_string_lossy().into_owned());
        env.insert("ANTILOKI_AGENT_PANE".into(), pane_id.to_string());
        if engine_key == "claude-code" {
            let live = pack.get("live").and_then(Value::as_bool).unwrap_or(false);
            antiloki_rs::cockpit_cockpit_pane_context_bake::write_shared_agent_settings(&records, pane_id, &cwd, live);
            crate::shared_folder::write_limits(&records, &crate::chat_pane::pane_scope_globs(&pane));
            own_claude_files(&mut args, &records, live);
            // what the person added to this agent's instructions: with no copy of its own there is no brief file for it, so it goes in at launch
            let mut added = pane.get("extra_prompt_md").and_then(Value::as_str).unwrap_or("").trim().to_string();
            // in a thread's worktree: a resumed conversation still names the project folder's paths, so it is told where it works now (no turn is spent on it)
            if cwd != root && !root.is_empty() { added = format!("{}\n\n{added}", worktree_note(&root, &cwd)).trim().to_string(); }
            if !added.is_empty() && !args.iter().any(|a| a == "--append-system-prompt") { args.push("--append-system-prompt".into()); args.push(added); }
        }
    }
    if engine_key == "codex" {
        // codex keys trust on the REPOSITORY ROOT of a linked worktree ("trusting will apply to the
        // repository root") and reads it from ~/.codex/config.toml, not from -c overrides: both the box
        // and the main checkout are written there once
        codex_trust(&codex_trust_paths(&cwd));
    }
    if engine_key == "cursor" { cursor_trust(&codex_trust_paths(&cwd)); }      // Cursor's "trust this folder?" prompt, answered before it is asked
    if let (Some(e), true) = (&engine, engine_key == "cursor") {
        let (bin, folder) = (e.bin.clone(), cwd.clone());
        if !tokio::task::spawn_blocking(move || cursor_allow_antiloki(&bin, &folder, &antiloki_rs::cockpit_cockpit_pane_context_bake::agent_bin())).await.unwrap_or(false) { eprintln!("antiloki-serve: pane {pane_id}: antiloki's tools were not approved in Cursor ahead of time; Cursor will ask"); }
    }
    if engine_key == "gemini" { antigravity_trust(&codex_trust_paths(&cwd)); }  // Antigravity's, the same
    if engine.as_ref().is_some_and(|e| e.bin.rsplit(['/', '\\']).next().is_some_and(|n| n.split('.').next() == Some("agy"))) && !cfg!(test) { if let Some(h) = user_home() { antigravity_allow_in(&h); } }
    grok_allow_args(engine_key, pack.get("live").and_then(Value::as_bool).unwrap_or(false), &mut args);
    if let (Some(bin), true) = (&cmd, pane.get("worktree_path").and_then(|v| v.as_str()).is_some() && engine.is_some()) {
        crate::agents_http::write_launch_record(&cwd, bin, &args, &env);
    }

    let reg_after = reg.clone();
    let mut out = start(state, reg, pane_id, cmd, args, cwd, env,
        b.get("cols").and_then(|v| v.as_i64()), b.get("rows").and_then(|v| v.as_i64())).await;
    // a pane whose provider was just switched: its new terminal is told to read the reconciliation pack first (once)
    if out.get("ok").and_then(|v| v.as_bool()) != Some(false) { crate::pane_provider::after_start(reg_after.clone(), pane_id); crate::team_onboarding::after_start(reg_after, pane_id); }
    if let (Some(id), Some(o)) = (resumed, out.as_object_mut()) { o.insert("resumed".into(), json!(id)); }
    (StatusCode::CREATED, Json(out)).into_response()
}

/// Point Claude Code at this agent's own tool settings (in place of the folder's shared file) and give it its own hooks and status line. Pure.
fn own_claude_files(args: &mut Vec<String>, records: &std::path::Path, live: bool) {
    if live { match args.iter().position(|a| a == "--mcp-config") { Some(i) if i + 1 < args.len() => args[i + 1] = records.join("mcp.json").to_string_lossy().into_owned(), _ => {} } }
    if !args.iter().any(|a| a == "--settings") { args.push("--settings".into()); args.push(records.join("settings.json").to_string_lossy().into_owned()); }
}

/// Claude keeps `~/.claude/projects/<slug of cwd>/<session>.jsonl` — no file, nothing to resume.
pub fn resume_transcript_exists(cwd: &str, session_id: &str) -> bool {
    if session_id.is_empty() || !session_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') { return false; }
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default();
    let slug = antiloki_rs::context_context_library::claude_project_slug(cwd);
    std::path::Path::new(&home).join(".claude").join("projects").join(slug).join(format!("{session_id}.jsonl")).is_file()
}

/// `POST /api/v1/workspace/pty/start` — resolve the shell target first; a docker runtime execs
/// into the container instead of spawning a local shell.
pub async fn workspace_pty_start(state: Arc<AppState>, reg: Arc<PtyRegistry>, body: Bytes) -> Response {
    let b = body_json(&body);
    // `Number(body?.pane_id) || 990001` — 0 and NaN both fall through to the default.
    let pane_id = match num(b.get("pane_id")) {
        0 => 990001,
        n => n,
    };
    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
    let info = engine_call(
        &state,
        "system.shell.target.resolve",
        json!({
            "ref_slug": b.get("ref_slug").and_then(|v| v.as_str()).unwrap_or(""),
            "wt_pane_id": b.get("wt_pane_id").cloned().unwrap_or(json!(0)),
        }),
        ctx,
    )
    .await
    .map(|(v, _)| v)
    .unwrap_or(Value::Null);

    let root = info.get("root").and_then(|v| v.as_str()).map(String::from).unwrap_or_else(cwd_string);
    let wt = info.get("wt").and_then(|v| v.as_str()).map(String::from);
    let container = info.get("container").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let docker = info.get("runtime").and_then(|v| v.as_str()) == Some("docker") && !container.is_empty();

    let work_path = match &wt {
        Some(w) => format!(
            "/worktrees/{}",
            w.split(['\\', '/']).filter(|s| !s.is_empty()).next_back().unwrap_or("")
        ),
        None => "/repo".to_string(),
    };

    let (cmd, args) = if docker {
        (
            Some("docker".to_string()),
            vec!["exec".into(), "-it".into(), "-w".into(), work_path, container, "sh".into()],
        )
    } else {
        (b.get("cmd").and_then(|v| v.as_str()).map(String::from), string_array(&b, "args"))
    };

    let mut env: HashMap<String, String> = HashMap::new();
    merge_env(&b, &mut env);

    let out = start(state, reg, pane_id, cmd, args, wt.unwrap_or(root), env,
        b.get("cols").and_then(|v| v.as_i64()), b.get("rows").and_then(|v| v.as_i64())).await;
    (StatusCode::CREATED, Json(out)).into_response()
}

/// `POST /api/v1/cockpit/pane/remove` — kill the pane's PTY FIRST. A live process holds the
/// worktree cwd, so `git worktree remove` would fight file locks for seconds while the dispatch
/// holds the DB, and the TUI keeps flooding chunk writes ("database is locked" storm).
pub async fn cockpit_pane_remove(
    state: Arc<AppState>,
    reg: Arc<PtyRegistry>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let b = body_json(&body);
    reg.kill(num(b.get("pane_id")));
    let ctx = crate::dispatch::attribution_ctx(&headers);
    crate::dispatch::finish(
        engine_call(&state, "system.cockpit.pane.remove", b, ctx).await,
        "POST",
        "system.cockpit.pane.remove",
    )
}

/// The conversation an agent in a shared folder goes on with: only after an earlier start of the SAME engine, the newest one in this agent's own time (else the one it continued last), and never one another agent holds.
fn earlier_conversation(folder: &str, pane: i64, engine: &str, newest: impl Fn(crate::shared_folder::Window) -> Option<String>) -> Option<String> {
    let (was, kept) = crate::shared_folder::last_start(folder, pane)?;
    if was != engine { return None; }
    newest(crate::shared_folder::window(folder, pane)).into_iter().chain(Some(kept).filter(|k| !k.is_empty())).find(|id| !crate::shared_folder::taken(folder, pane, id))
}
/// Each CLI's own way to go on with a conversation. Codex takes a subcommand, right after what starts the program; the others a flag. Pure.
fn resume_args(args: &mut Vec<String>, lead: usize, engine: &str, id: &str) {
    match engine {
        "codex" => { let at = lead.min(args.len()); args.insert(at, "resume".into()); args.insert(at + 1, id.into()); }
        "cursor" => args.extend(["--resume".into(), id.into()]),
        "opencode" => args.extend(["--session".into(), id.into()]),
        "gemini" => args.extend(["--conversation".into(), id.into()]),
        _ => {}
    }
}

/// What a Claude agent in a thread's worktree is told at launch. Pure.
fn worktree_note(project: &str, folder: &str) -> String { format!("You work in {folder}, this thread's own Git worktree. The project folder, {project}, is a different checkout: do not edit files there. A path under it from earlier in the conversation means the same file under {folder}.") }

/// Where a pane's process starts: its own worktree, else the project's folder, else (no project at all) the server's. Pure.
fn pane_folder(worktree: Option<&str>, project: &str, server: &str) -> String {
    [worktree.unwrap_or(""), project].into_iter().find(|p| !p.trim().is_empty()).unwrap_or(server).to_string()
}

fn cwd_string() -> String {
    std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

fn string_array(b: &Value, key: &str) -> Vec<String> {
    b.get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

fn merge_env(b: &Value, env: &mut HashMap<String, String>) {
    if let Some(extra) = b.get("env").and_then(|v| v.as_object()) {
        for (k, v) in extra {
            env.insert(k.clone(), v.as_str().unwrap_or_default().to_string());
        }
    }
}

/// The paths Codex must trust for a box: the worktree itself and, for a linked worktree, the main
/// repository it belongs to (`git rev-parse --git-common-dir` → its parent). Pure over git.
/// What each CLI paints once its composer accepts input. Empty = no TUI to wait for.
/// The sentence around a limit phrase, ANSI-stripped and bounded — `None` when the text holds none.
/// A CLI's own question before it starts (Claude: "Yes, I trust this folder"; Codex: "Do you trust the contents of this directory"; a new MCP server to approve). The TUI paints with cursor moves, so the words reach us with the spaces gone:
/// compared with whitespace removed, lower case. Pure - tested.
pub fn asks_a_question(recent: &str) -> bool {
    let n: Vec<char> = strip_ansi(recent).chars().filter(|c| !c.is_whitespace()).flat_map(|c| c.to_lowercase()).collect();
    // only the newest screen counts: a question that was answered scrolls away under the repaint that follows
    let n: String = n[n.len().saturating_sub(1500)..].iter().collect();
    ["itrustthisfolder", "doyoutrust", "newmcpserver", "usethisandallfuturemcpservers", "trustthecontents"].iter().any(|m| n.contains(m))
}

pub fn quota_line(raw: &str) -> Option<String> {
    let clean = strip_ansi(raw);
    let lower = clean.to_ascii_lowercase();
    let phrase = antiloki_rs::cockpit__helpers_engine_handoff::LIMIT_PHRASES.iter().find(|p| lower.contains(*p))?;
    let at = lower.find(phrase)?;
    let start = clean[..at].rfind(['\n', '\r']).map(|i| i + 1).unwrap_or(0);
    let end = clean[at..].find(['\n', '\r']).map(|i| at + i).unwrap_or(clean.len());
    let line: String = clean[start..end].split_whitespace().collect::<Vec<_>>().join(" ");
    Some(line.chars().take(240).collect())
}
fn strip_ansi(s: &str) -> String {
    let re = regex::Regex::new("\x1b\\[[0-9;?]*[A-Za-z]|\x1b\\][^\x07]*\x07|\x1b[()][A-Za-z0-9]").unwrap();
    re.replace_all(s, "").to_string()
}
/// The terminal text with cursor-column moves read as the spaces they stand for, and every other escape removed. Claude Code 2.1.296 paints its footer as `shift+tab\x1b[30Gto\x1b[33Gcycle` (found 2026-11-24 on a
/// resumed session), so the words are never next to a literal space in the byte stream. Pure - tested.
pub fn spaced_text(raw: &str) -> String {
    static MOVES: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();   // compiled once: this runs on every chunk a starting CLI paints
    let moves = MOVES.get_or_init(|| regex::Regex::new("\x1b\\[[0-9;]*[GC]").unwrap());
    strip_ansi(&moves.replace_all(raw, " "))
}
/// What a pane reports as blocked while Claude Code asks "Switch model?" (the question is about cost: the conversation is cached for the current model, so the next reply is slower and dearer). The chat answers it with buttons.
/// What a pane reports as blocked while an agent's CLI waits for the person to approve the antiloki MCP server (Cursor asks on every new start until it is approved).
pub const MCP_DIALOG: &str = "MCP approval: approve the antiloki MCP server in this agent's terminal, or it cannot read the handover or antiloki's tools";
/// Is the approval prompt on the screen now: it was shown and the CLI has not ended since. Pure - tested.
pub fn mcp_approval_open(raw_window: &str) -> bool {
    let seen = spaced_text(raw_window);
    let Some(at) = seen.rfind("MCP Server Approval") else { return false };
    let after = &seen[at..];
    // answered: Cursor prints "Applying your selection..." and then starts its own screen ("Cursor Agent")
    !(after.contains("process has ended") || after.contains("(exit ") || after.contains("Applying your selection") || after.contains("Cursor Agent"))
}
pub const SWITCH_DIALOG: &str = "Switch model? The next reply will be slower and use more tokens (the conversation is read again)";
/// Is that question on the screen right now: it was asked and nothing after it has answered it (`Set model to ...` after a yes, `Kept model as ...` after a no). Reads the text a person sees. Pure - tested.
pub fn switch_dialog_open(raw_window: &str) -> bool {
    let seen = spaced_text(raw_window);
    let Some(asked) = seen.rfind("Switch model?") else { return false };
    let after = &seen[asked..];
    !(after.contains("Kept model as") || after.contains("Set model to"))
}
/// The newest line the CLI answered a model command with (`Set model to Sonnet 5.5 and saved as your default for new sessions`, or `Kept model as Sonnet 5.5` after a "no"), as a person reads it. Pure - tested.
pub fn model_result_line(raw_window: &str) -> Option<String> {
    let seen = spaced_text(raw_window);
    let at = ["Set model to", "Kept model as"].iter().filter_map(|m| seen.rfind(m)).max()?;
    let line = seen[at..].split(['\r', '\n']).next().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
    (!line.is_empty()).then_some(line)
}
/// Is the CLI's composer painted: one of its markers is in the stream, as bytes or as the text a person sees. Pure - tested.
pub fn composer_up(boot: &str, markers: &[&str]) -> bool {
    if markers.iter().any(|m| boot.contains(m)) { return true; }
    let seen = spaced_text(boot);
    markers.iter().any(|m| seen.contains(m))
}
pub fn composer_markers(cmd: &str) -> Vec<&'static str> {
    let base = cmd.rsplit(['/', '\\']).next().unwrap_or(cmd);
    match base {
        "claude" | "claude.exe" | "claude.cmd" => vec!["? for shortcuts", "for shortcuts", "Try \"", "shift+tab to cycle"],
        "codex" | "codex.exe" | "codex.cmd" => vec!["Ask Codex to do anything"],
        "opencode" | "opencode.exe" | "opencode.cmd" => vec!["Ask anything", "ctrl+p commands"],
        // read from the real screens (#8, agy 1.5.18 and cursor-agent 2026.11.22): neither one's "trust this folder" question carries these words
        "agy" | "agy.exe" | "agy.cmd" => vec!["? for shortcuts", "for shortcuts"],
        "cursor-agent" | "cursor-agent.exe" | "cursor-agent.cmd" => vec!["Plan, search, build anything", "Add a follow-up"],
        _ => vec![],
    }
}

/// Keep the LAST `keep` bytes of `s`, cutting on a char boundary — the TUI paints box-drawing and
/// arrows (multibyte), and a byte-indexed drain panicked the whole output reader (2026-09-19: a
/// Codex pane froze at "Working (0s" with the reader thread dead).
pub fn trim_front(s: &mut String, keep: usize) {
    if s.len() <= keep { return; }
    let mut cut = s.len() - keep;
    while !s.is_char_boundary(cut) { cut += 1; }
    s.drain(..cut);
}

pub fn codex_trust_paths(cwd: &str) -> Vec<String> {
    let mut out = vec![cwd.trim_end_matches('/').to_string()];
    if let Ok(o) = crate::proc::cmd("git").args(["rev-parse", "--path-format=absolute", "--git-common-dir"]).current_dir(cwd).output() {
        if o.status.success() {
            let common = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if let Some(root) = std::path::Path::new(&common).parent().map(|p| p.to_string_lossy().trim_end_matches('/').to_string()) {
                if !root.is_empty() && !out.contains(&root) { out.push(root); }
            }
        }
    }
    out
}

/// Append `[projects."<path>"] trust_level = "trusted"` to ~/.codex/config.toml for each path not
/// already trusted there (the TUI asks otherwise, and a pane's first keystrokes answer the question).
pub fn codex_trust(paths: &[String]) {
    let Some(home) = std::env::var_os("HOME") else { return };
    let cfg = std::path::Path::new(&home).join(".codex").join("config.toml");
    let cur = std::fs::read_to_string(&cfg).unwrap_or_default();
    let mut add = String::new();
    for p in paths {
        let head = format!("[projects.{}]", serde_json::to_string(p).unwrap_or_default());
        if cur.contains(&head) { continue; }
        add.push_str(&format!("\n{head}\ntrust_level = \"trusted\"\n"));
    }
    if add.is_empty() { return; }
    let _ = std::fs::create_dir_all(cfg.parent().unwrap());
    let _ = std::fs::write(&cfg, format!("{}{}", cur.trim_end_matches('\n'), add.replace("\n\n", "\n")));
}

// ── CURSOR AND ANTIGRAVITY TRUST THEIR BOX (Step D2) ──────────────────────────────────────────────────────────────────────
// Their first-run "do you trust this folder?" prompt eats a pane's first keystrokes just as Codex's did. Cursor remembers a trusted folder as a marker file under `~/.cursor/projects/<the path as a slug>/`;
// Antigravity keeps a `trustedWorkspaces` list in `~/.gemini/antigravity-cli/settings.json`. The box is ours: both are written once, before the CLI starts, and nothing else in either file is touched.

fn user_home() -> Option<std::path::PathBuf> { std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).filter(|h| !h.is_empty()).map(std::path::PathBuf::from) }

/// Cursor's name for a folder: every run of characters that are not letters or digits becomes one dash, none at either end (`/home/dani/dev/my_app` → `home-dani-dev-my-app`). Pure.
pub fn cursor_project_slug(path: &str) -> String {
    let mut out = String::new(); let mut dash = false;
    for c in path.chars() { if c.is_ascii_alphanumeric() { if dash && !out.is_empty() { out.push('-'); } dash = false; out.push(c); } else { dash = true; } }
    out
}
/// A moment as `2026-11-14T20:15:00.000Z`. Pure.
pub fn iso_utc(ms: i64) -> String {
    let (days, rem) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000)); let z = days + 719_468; let era = z.div_euclid(146_097); let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; let y = yoe + era * 400; let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1; let m = if mp < 10 { mp + 3 } else { mp - 9 }; let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", rem / 3_600_000, rem / 60_000 % 60, rem / 1000 % 60, rem % 1000)
}
/// Write Cursor's trust marker for each folder that has none. Returns the folders it wrote.
pub fn cursor_trust_in(home: &std::path::Path, paths: &[String], now_ms: i64) -> Vec<String> {
    let mut wrote = vec![];
    for p in paths {
        let slug = cursor_project_slug(p); if slug.is_empty() { continue; }
        let dir = home.join(".cursor").join("projects").join(&slug); let marker = dir.join(".workspace-trusted");
        if marker.exists() { continue; }
        if std::fs::create_dir_all(&dir).is_err() { continue; }
        if std::fs::write(&marker, serde_json::to_string_pretty(&json!({ "trustedAt": iso_utc(now_ms), "workspacePath": p })).unwrap_or_default()).is_ok() { wrote.push(p.clone()); }
    }
    wrote
}
/// Add each folder to Antigravity's `trustedWorkspaces`, keeping every other setting. A settings file that is not a JSON object is left alone (never overwritten). Returns the folders it added.
pub fn antigravity_trust_in(home: &std::path::Path, paths: &[String]) -> Vec<String> {
    let f = home.join(".gemini").join("antigravity-cli").join("settings.json");
    let mut v: Value = match std::fs::read_to_string(&f) { Ok(t) => match serde_json::from_str(&t) { Ok(v) => v, Err(_) => return vec![] }, Err(_) => json!({}) };
    let Some(obj) = v.as_object_mut() else { return vec![] };
    let list = obj.entry("trustedWorkspaces").or_insert_with(|| json!([]));
    let Some(arr) = list.as_array_mut() else { return vec![] };
    let mut added = vec![];
    for p in paths { if !arr.iter().any(|x| x.as_str() == Some(p.as_str())) { arr.push(json!(p)); added.push(p.clone()); } }
    if added.is_empty() { return added; }
    if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
    if std::fs::write(&f, serde_json::to_string_pretty(&v).unwrap_or_default()).is_err() { return vec![]; }
    added
}
/// Is this agent KNOWN to be stopped from writing outside its file limits while it works? Claude Code's hook reads them wherever it runs. OpenCode's plugin does in a copy of its own (seen refusing a write, an edit and
/// a shell redirect on 2.4.2, 2026-11-28); in the shared folder it is not written, since that would change the project. Codex's hook was never seen holding a limit live, and the other CLIs have none. Pure.
pub fn limits_enforced(engine_key: &str, own_copy: bool) -> bool { engine_key == "claude-code" || (own_copy && matches!(engine_key, "opencode" | "opencode-direct")) }
static LIMITS_OFF: std::sync::Mutex<std::collections::BTreeSet<i64>> = std::sync::Mutex::new(std::collections::BTreeSet::new());
fn note_limits_off(pane_id: i64, off: bool) { if let Ok(mut s) = LIMITS_OFF.lock() { if off { s.insert(pane_id); } else { s.remove(&pane_id); } } }
pub fn limits_off(pane_id: i64) -> bool { LIMITS_OFF.lock().is_ok_and(|s| s.contains(&pane_id)) }

const ANTIGRAVITY_ALLOW: &str = "mcp(antiloki/*)";
/// ANTIGRAVITY ASKS BEFORE EACH TOOL CALL, AND ITS CHAT VIEW DOES NOT SHOW THE QUESTION (operator's yes, 2026-11-28: every provider pre-approves antiloki's tools). One rule is added to `permissions.allow` in its settings; a `deny` the person wrote
/// still wins, every other setting is kept, and a file that is not what it should be is left alone. True when the rule was added.
pub fn antigravity_allow_in(home: &std::path::Path) -> bool {
    let f = home.join(".gemini").join("antigravity-cli").join("settings.json");
    let mut v: Value = match std::fs::read_to_string(&f) { Ok(t) => match serde_json::from_str(&t) { Ok(v) => v, Err(_) => return false }, Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}), Err(_) => return false };
    let Some(obj) = v.as_object_mut() else { return false };
    let Some(perms) = obj.entry("permissions").or_insert_with(|| json!({})).as_object_mut() else { return false };
    let Some(allow) = perms.entry("allow").or_insert_with(|| json!([])).as_array_mut() else { return false };
    if allow.iter().any(|a| a.as_str() == Some(ANTIGRAVITY_ALLOW)) { return false; }
    allow.push(json!(ANTIGRAVITY_ALLOW));
    if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
    std::fs::write(&f, serde_json::to_string_pretty(&v).unwrap_or_default()).is_ok()
}
/// Grok's own flag for this one session: antiloki's tools are not asked about. No file of the person's is changed.
fn grok_allow_args(engine_key: &str, live: bool, args: &mut Vec<String>) { if engine_key == "grok" && live && !args.iter().any(|a| a == "MCPTool(antiloki__*)") { args.push("--allow".into()); args.push("MCPTool(antiloki__*)".into()); } }
/// Is the folder's `antiloki` entry the one antiloki wrote (it starts antiloki's own helper)? A project could carry its own entry under that name; only ours is approved for the person. Pure.
fn cursor_entry_is_ours(file: &str, helper: &str) -> bool { !helper.is_empty() && serde_json::from_str::<Value>(file).ok().is_some_and(|v| v["mcpServers"]["antiloki"]["command"].as_str() == Some(helper)) }
/// The folder's Cursor permissions with antiloki's own tools allowed, everything else as it was; None when there is nothing to change, or the file is not what it should be (never overwritten). Pure.
fn cursor_cli_allowing(file: Option<&str>) -> Option<String> {
    let mut v: Value = match file { None => json!({ "permissions": { "allow": [], "deny": [] } }), Some(t) => serde_json::from_str(t).ok()? };
    let perms = v.as_object_mut()?.entry("permissions").or_insert_with(|| json!({ "allow": [], "deny": [] }));
    let allow = perms.as_object_mut()?.entry("allow").or_insert_with(|| json!([])).as_array_mut()?;
    if allow.iter().any(|a| a.as_str() == Some(CURSOR_ALLOW)) { return None; }
    allow.push(json!(CURSOR_ALLOW)); serde_json::to_string_pretty(&v).ok()
}
const CURSOR_ALLOW: &str = "Mcp(antiloki:*)";
/// CURSOR ALSO ASKS BEFORE EACH TOOL CALL ("Run this MCP tool?"): antiloki's own are allowed in the folder's `.cursor/cli.json` (the operator's yes, 2026-11-27). A file the project keeps in Git is left as it is. Blocking.
fn cursor_allow_tools(folder: &std::path::Path) {
    if crate::thread_tree::git(&["ls-files", "--error-unmatch", ".cursor/cli.json"], folder).is_ok() { return; }
    let f = folder.join(".cursor").join("cli.json");
    let old = match std::fs::read_to_string(&f) { Ok(t) => Some(t), Err(e) if e.kind() == std::io::ErrorKind::NotFound => None, Err(_) => return };
    let Some(new) = cursor_cli_allowing(old.as_deref()) else { return };
    if old.is_none() { crate::thread_tree::exclude_line(folder, ".cursor/cli.json"); }   // antiloki's own file: never an agent's work
    let _ = std::fs::write(&f, new);
}
/// CURSOR ASKS BEFORE IT LOADS A PROJECT'S MCP SERVER, and on a Windows machine that question took no key (2026-11-27). The operator said yes: antiloki's own server is approved with Cursor's own command before the agent starts; every other server is still asked about. Blocking; a failure leaves Cursor's question as it was.
fn cursor_allow_antiloki(bin: &str, cwd: &str, helper: &str) -> bool {
    let ours = std::fs::read_to_string(std::path::Path::new(cwd).join(".cursor").join("mcp.json")).is_ok_and(|t| cursor_entry_is_ours(&t, helper));
    if !ours || cfg!(test) { return false; }
    cursor_allow_tools(std::path::Path::new(cwd));
    let mut c = antiloki_rs::proc::cmd(bin); c.args(["mcp", "enable", "antiloki"]).current_dir(cwd).env("BROWSER", "true").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    antiloki_rs::proc::output_within(c, std::time::Duration::from_secs(15)).is_some_and(|o| o.status.success())
}
pub fn cursor_trust(paths: &[String]) { if let Some(h) = user_home() { cursor_trust_in(&h, paths, crate::cli::now_ms()); } }
pub fn antigravity_trust(paths: &[String]) { if let Some(h) = user_home() { antigravity_trust_in(&h, paths); } }

/// Which Codex first-run prompt the terminal is showing, and the keys that accept it — once each.
/// "Do you trust the contents of this directory?" → 1 (yes); "Hooks need review" → 2 (trust all
/// and continue). Pure over the recent output.
pub fn codex_prompt_answer(recent: &str, answered: &[&str]) -> Option<(&'static str, &'static str)> {
    if !answered.contains(&"trust") && recent.contains("Do you trust the contents of this directory") { return Some(("trust", "1\r")); }
    if !answered.contains(&"hooks") && recent.contains("Hooks need review") { return Some(("hooks", "2\r")); }
    None
}

#[cfg(test)]
mod tests {
    #[test] fn an_agent_sharing_a_folder_goes_on_with_its_own_conversation_and_no_one_elses() {
        let d = std::env::temp_dir().join(format!("al-again-{}", std::process::id())); let r = d.to_string_lossy().to_string();
        let found = |id: &'static str| move |_| Some(id.to_string());
        assert_eq!(super::earlier_conversation(&r, 1, "codex", found("a")), None, "a first start continues nothing");
        crate::shared_folder::stamp(&r, 1, "codex", "", 1000);
        assert_eq!(super::earlier_conversation(&r, 1, "codex", found("a")).as_deref(), Some("a"));
        assert_eq!(super::earlier_conversation(&r, 1, "cursor", found("a")), None, "another engine now");
        crate::shared_folder::stamp(&r, 2, "codex", "theirs", 2000); crate::shared_folder::stamp_resume(&r, 1, "codex", "mine");
        assert_eq!(super::earlier_conversation(&r, 1, "codex", found("theirs")).as_deref(), Some("mine"));
        assert_eq!(super::earlier_conversation(&r, 1, "codex", |_| None).as_deref(), Some("mine"));
        let args = |engine: &str| { let mut a: Vec<String> = vec!["lead".into(), "--model".into()]; super::resume_args(&mut a, 1, engine, "X"); a.join(" ") };
        assert_eq!(args("codex"), "lead resume X --model"); assert_eq!(args("cursor"), "lead --model --resume X"); assert_eq!(args("opencode"), "lead --model --session X"); assert_eq!(args("gemini"), "lead --model --conversation X");
        let _ = std::fs::remove_dir_all(d);
    }
    #[test] fn a_claude_agent_sharing_a_folder_is_started_with_files_of_its_own() {
        let mut a: Vec<String> = ["--append-system-prompt", "x", "--mcp-config", ".antiloki/mcp.json"].map(String::from).to_vec();
        super::own_claude_files(&mut a, std::path::Path::new("/p/.antiloki/agents/7"), true);
        assert_eq!(a[3], std::path::Path::new("/p/.antiloki/agents/7").join("mcp.json").to_string_lossy()); assert_eq!(a[4], "--settings"); assert!(a[5].ends_with("settings.json"));
        let before = a.clone(); super::own_claude_files(&mut a, std::path::Path::new("/p/.antiloki/agents/7"), true); assert_eq!(a.len(), before.len(), "said once");
    }
    #[test]
    fn the_connection_state_of_a_pane_without_a_terminal_is_not_connected_and_a_report_for_it_is_ignored() {
        let reg = PtyRegistry::default();
        assert_eq!(reg.mcp_state(5), json!({ "ok": true, "running": false, "mcp": false, "ask": false }));
        reg.mark_mcp(5);   // the helper of a pane that has no live terminal says "up": nothing is recorded
        assert_eq!(reg.mcp_state(5)["mcp"], json!(false));
    }

    use super::*;
    #[test]
    fn a_limit_is_called_enforced_only_where_something_enforces_it() {
        assert!(limits_enforced("claude-code", false) && limits_enforced("opencode", true) && limits_enforced("opencode-direct", true));
        assert!(!limits_enforced("opencode", false) && ["codex", "cursor", "grok", "gemini"].iter().all(|e| !limits_enforced(e, true)));
        note_limits_off(-5, true); assert!(limits_off(-5)); note_limits_off(-5, false); assert!(!limits_off(-5));
    }
    #[test]
    fn antigravity_and_grok_are_allowed_antilokis_tools_and_nothing_else_is_changed() {
        let h = tmp_home("agy-allow"); let f = h.join(".gemini/antigravity-cli/settings.json"); std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, r#"{"model":"m","permissions":{"allow":["command(ls)"],"deny":["command(rm)"]}}"#).unwrap();
        assert!(antigravity_allow_in(&h)); assert!(!antigravity_allow_in(&h), "once is enough");
        assert_eq!(serde_json::from_str::<Value>(&std::fs::read_to_string(&f).unwrap()).unwrap(), json!({"model":"m","permissions":{"allow":["command(ls)","mcp(antiloki/*)"],"deny":["command(rm)"]}}));
        std::fs::write(&f, "not json").unwrap(); assert!(!antigravity_allow_in(&h)); assert_eq!(std::fs::read_to_string(&f).unwrap(), "not json");
        let mut a: Vec<String> = vec![]; grok_allow_args("codex", true, &mut a); grok_allow_args("grok", false, &mut a); assert!(a.is_empty());
        grok_allow_args("grok", true, &mut a); grok_allow_args("grok", true, &mut a); assert_eq!(a, ["--allow", "MCPTool(antiloki__*)"]);
    }
    #[test]
    fn cursor_is_allowed_antilokis_tools_and_nothing_else_is_changed() {
        let new = cursor_cli_allowing(None).unwrap(); assert_eq!(serde_json::from_str::<Value>(&new).unwrap()["permissions"]["allow"], json!(["Mcp(antiloki:*)"]));
        assert!(cursor_cli_allowing(Some(&new)).is_none(), "once is enough");
        let kept: Value = serde_json::from_str(&cursor_cli_allowing(Some(r#"{"permissions":{"allow":["Shell(ls)"],"deny":["Shell(rm)"]},"x":1}"#)).unwrap()).unwrap();
        assert_eq!(kept, json!({"permissions":{"allow":["Shell(ls)","Mcp(antiloki:*)"],"deny":["Shell(rm)"]},"x":1}));
        assert!(cursor_cli_allowing(Some("not json")).is_none() && cursor_cli_allowing(Some("[1]")).is_none());
    }
    #[test]
    fn only_antilokis_own_cursor_entry_is_approved_for_the_person() {
        let ours = r#"{"mcpServers":{"antiloki":{"command":"/opt/a/antiloki-agent","args":["mcp"]}}}"#;
        assert!(cursor_entry_is_ours(ours, "/opt/a/antiloki-agent"));
        assert!(!cursor_entry_is_ours(r#"{"mcpServers":{"antiloki":{"command":"./antiloki-agent"}}}"#, "/opt/a/antiloki-agent"), "a project's own entry under that name is still asked about");
        assert!(!cursor_entry_is_ours(r#"{"mcpServers":{"mine":{"command":"/opt/a/antiloki-agent"}}}"#, "/opt/a/antiloki-agent")); assert!(!cursor_entry_is_ours("not json", "x")); assert!(!cursor_entry_is_ours(ours, ""));
    }
    #[test]
    fn a_pane_without_a_worktree_starts_in_the_project_never_in_the_servers_folder() {
        assert_eq!(pane_folder(Some("/box"), "/project", "/server"), "/box");
        assert_eq!(pane_folder(None, "/project", "/server"), "/project");
        assert_eq!(pane_folder(Some(""), "/project", "/server"), "/project");
        assert_eq!(pane_folder(None, "", "/server"), "/server");
    }
    #[test]
    fn the_clis_answer_to_a_model_command_is_read_as_one_clean_line_and_the_newest_wins() {
        let a = "\x1b[1m\u{276f} /model sonnet\x1b[22m\r\n  \u{23bf}  Set\x1b[12Gmodel\x1b[18Gto Sonnet 5.5 and saved as your default for new sessions\r\n\r\n";
        assert_eq!(model_result_line(a).as_deref(), Some("Set model to Sonnet 5.5 and saved as your default for new sessions"));
        assert_eq!(model_result_line(&format!("{a}\u{276f} /model haiku\r\n  Kept model as Sonnet 5.5\r\n")).as_deref(), Some("Kept model as Sonnet 5.5"));
        assert_eq!(model_result_line("nothing here"), None);
    }
    #[test]
    fn cursors_mcp_approval_prompt_is_open_until_the_process_ends_or_is_answered() {
        let prompt = "\u{26a0} MCP Server Approval Required\r\n\r\n  \u{2022} antiloki (command: /x/antiloki-agent)\r\n\r\n  [a] Approve all servers\r\n  [c] Continue without approval";
        assert!(mcp_approval_open(prompt), "a shown prompt is open");
        assert!(!mcp_approval_open(&format!("{prompt}\r\n(exit 0)")), "an ended process is not waiting on anyone");
        assert!(!mcp_approval_open(&format!("{prompt}\r\n\u{1b}[1mthe terminal stopped\u{1b}[0m the process has ended")));
        assert!(!mcp_approval_open("Cursor ready"), "no prompt at all");
        assert!(!mcp_approval_open(&format!("{prompt}\r\n\u{231b} Applying your selection...\r\nCursor Agent v2026.11.22")), "an answered prompt is closed (seen live: ask stayed on after approving)");
    }
    #[test]
    fn the_switch_model_question_is_open_until_a_yes_or_a_no_answers_it() {
        let asked = "\x1b[1m /model haiku\x1b[22m\r\n  Switch\x1b[10Gmodel?\r\n  Your next response will be slower and use more tokens.\r\n  \u{276f} 1. Yes, switch to Haiku 5.5\r\n  2. No, go back";
        assert!(switch_dialog_open(asked));
        assert!(!switch_dialog_open(&format!("{asked}\r\n  Kept model as Sonnet 5.5")), "a no closes it");
        assert!(!switch_dialog_open(&format!("{asked}\r\n  Set model to Haiku 5.5 and saved")), "a yes closes it");
        assert!(switch_dialog_open(&format!("Kept model as Sonnet 5.5\r\n{asked}")), "an old answer before a new question does not close it");
        assert!(!switch_dialog_open("Set model to Sonnet 5.5"), "no question at all");
    }
    #[test]
    fn a_footer_painted_with_cursor_moves_instead_of_spaces_still_says_the_composer_is_up() {
        // the exact bytes Claude Code 2.1.296 sent on a resumed session (the footer has no literal space between its words)
        let boot = "\x1b[2K auto mode on (shift+tab\x1b[30Gto\x1b[33Gcycle) \u{b7} \x1b[1m1 agent\x1b[22m";
        let markers = composer_markers("claude");
        assert!(!markers.iter().any(|m| boot.contains(m)), "the literal match is the bug");
        assert!(composer_up(boot, &markers));
        assert!(!composer_up("Welcome back\x1b[2K", &markers), "a boot screen is not a composer");
    }
    #[test]
    fn a_terminal_never_spawns_a_seat_the_operator_switched_off() {
        // Claude is off (the first real run with Claude switched off opened a `claude` terminal anyway); Codex is on
        let off = |e: crate::cli::Engine| e == crate::cli::Engine::ClaudeCode;
        assert_eq!(switched_off("claude-code", off).as_deref(), Some("Claude is switched off (Settings › Seats)."));
        assert_eq!(switched_off("codex", off), None, "the others are untouched");
        assert_eq!(switched_off("", off), None, "a bare shell is not a seat");
    }
    #[test]
    fn the_opencode_terminals_open_with_zen_off_and_a_switched_off_seat_still_does_not() {
        // THE REAL SWITCH, Zen at its default (off): the first version asked `model_blocked`, which refuses OpenCode whenever Zen is off - so every `opencode` and `opencode-direct` terminal was refused on a default install
        assert!(!crate::cli::zen_enabled(), "the default is Zen off");
        assert_eq!(switched_off("opencode", crate::cli::seat_off), None);
        assert_eq!(switched_off("opencode-direct", crate::cli::seat_off), None);
        crate::cli::set_provider_flag("claude-code", false).unwrap();
        let why = switched_off("claude-code", crate::cli::seat_off);
        crate::cli::set_provider_flag("claude-code", true).unwrap();
        assert!(why.unwrap().contains("switched off"));
        // an OpenCode seat the operator did switch off is still refused
        crate::cli::set_provider_flag("opencode", false).unwrap();
        let oc = switched_off("opencode-direct", crate::cli::seat_off);
        crate::cli::set_provider_flag("opencode", true).unwrap();
        assert!(oc.is_some());
    }
    #[test]
    fn a_paste_cannot_end_itself_early() {
        // an escape inside the text could close the bracketed paste and turn the rest into keystrokes
        let evil = "hello\x1b[201~rm -rf /";
        assert_eq!(sanitize_paste(evil), "hello[201~rm -rf /");
        let plan = send_plan("claude", evil);
        assert_eq!(plan[0].0.matches("\x1b[201~").count(), 1, "only our own end marker");
        assert_eq!(sanitize_paste("a\r\nb\tc\x00\x07d\re"), "a\nb\tcd\ne");
        assert_eq!(sanitize_paste("até já ✓"), "até já ✓");
    }
    #[test]
    fn the_send_plan_is_the_paste_then_the_submit_key_after_the_engines_pause() {
        assert_eq!(send_plan("/home/x/.local/bin/claude", "fix it"), vec![("\x1b[200~fix it\x1b[201~".to_string(), 0), ("\r".to_string(), 150)]);
        assert_eq!(send_plan("opencode", "a\nb")[1], ("\r".to_string(), 120));
        assert!(send_plan("claude", "  \n\t ").is_empty());
        // a shell is typed to, not pasted into: it would print the markers
        assert_eq!(send_plan("/usr/bin/zsh", "echo hi"), vec![("echo hi".to_string(), 0), ("\r".to_string(), 120)]); assert_eq!(send_plan("bash", " ls ")[0].0, "ls");
        assert!(send_plan("gemini", "x")[0].0.starts_with("\x1b[200~"), "an agent CLI still gets the paste");
        assert_eq!(send_plan("claude", "  a  b  ")[0].0, "\x1b[200~a  b\x1b[201~");
    }
    #[test]
    fn the_prompt_buffer_trims_on_a_char_boundary() {
        let mut s = "╭──╮".repeat(10); // 4 chars, 10 bytes each rep: 100 bytes
        trim_front(&mut s, 7); // 7 lands inside a box-drawing char
        assert!(s.len() <= 7 && s.chars().all(|c| "╭─╮".contains(c)));
        let mut short = String::from("ab"); trim_front(&mut short, 8); assert_eq!(short, "ab");
    }
    #[test]
    fn the_quota_line_is_found_in_ansi_soup_and_never_in_prose() {
        assert_eq!(quota_line("\x1b[31m✗ You've hit your usage limit.\x1b[0m Upgrade to Plus\r\n> "), Some("✗ You've hit your usage limit. Upgrade to Plus".into()));
        assert_eq!(quota_line("Claude usage limit reached. Your limit will reset at 7pm (UTC).\n"), Some("Claude usage limit reached. Your limit will reset at 7pm (UTC).".into()));
        assert_eq!(quota_line("we should add a rate limiter to the api"), None);
    }
    #[test]
    fn each_cli_has_a_composer_marker_and_a_shell_has_none() {
        assert!(composer_markers("/opt/bin/codex").contains(&"Ask Codex to do anything"));
        assert!(composer_markers("opencode").contains(&"Ask anything"));
        assert!(composer_markers("/home/x/.local/bin/claude").contains(&"for shortcuts"));
        assert!(composer_markers("bash").is_empty());
    }
    #[test]
    fn every_engine_a_pane_can_start_says_when_it_is_ready() {
        for cmd in ["claude", "codex", "opencode", "agy", "agy.exe", "/home/x/.local/bin/cursor-agent"] { assert!(!composer_markers(cmd).is_empty(), "{cmd}"); }
        // the screens as they paint: the composer counts, the folder-trust question does not
        assert!(composer_up("────\r\n>\r\n? for shortcuts\r\nGemini 3.8 Flash", &composer_markers("agy")));
        assert!(composer_up("\x1b[2G\u{2192}\x1b[4GPlan, search, build anything", &composer_markers("cursor-agent")));
        assert!(!composer_up("Do you trust the contents of this project?\r\n> Yes, I trust this folder\r\nNo, exit\r\n↑/↓ Navigate · enter Confirm", &composer_markers("agy")));
        assert!(!composer_up("Workspace Trust Required\r\nDo you trust the contents of this directory?\r\n[a] Trust this workspace\r\n[q] Quit", &composer_markers("cursor-agent")));
        // Grok and a plain shell have no marker: they are ready from the start (the pipeline marks them so)
        for cmd in ["grok", "bash"] { assert!(composer_markers(cmd).is_empty(), "{cmd}"); }
    }
    #[test]
    fn codex_first_run_prompts_are_answered_once_each() {
        assert_eq!(codex_prompt_answer("… Do you trust the contents of this directory? …", &[]), Some(("trust", "1\r")));
        assert_eq!(codex_prompt_answer("… Do you trust the contents of this directory? …", &["trust"]), None);
        assert_eq!(codex_prompt_answer("Hooks need review\n1 hook is new or changed.", &["trust"]), Some(("hooks", "2\r")));
        assert_eq!(codex_prompt_answer("Hooks need review", &["trust", "hooks"]), None);
        assert_eq!(codex_prompt_answer("> working…", &[]), None);
    }
    #[test]
    fn codex_trust_paths_include_the_repo_root_of_a_linked_worktree() {
        let ps = codex_trust_paths("/definitely/not/a/repo");
        assert_eq!(ps, vec!["/definitely/not/a/repo".to_string()], "no git → the path alone");
    }

    #[test]
    fn every_installed_agent_has_a_door_that_runs_its_cli_and_the_harness_boots_its_tui() {
        for (key, bin) in [("claude-code", "claude"), ("codex", "codex"), ("opencode", "opencode"), ("grok", "grok")] { let l = resolve_engine(key).unwrap_or_else(|| panic!("{key}")); assert_eq!((l.bin.as_str(), l.lead.len()), (bin, 0), "{key}"); }
        // Antigravity (`agy`, else the legacy `gemini`) and Cursor (`cursor-agent`, else `agent`) are launched as the CLI, not as a shell
        let g = resolve_engine("gemini").unwrap(); assert!(g.bin.ends_with("agy") || g.bin.ends_with("gemini"), "{}", g.bin); let c = resolve_engine("cursor").unwrap(); assert!(c.bin.ends_with("cursor-agent") || c.bin.ends_with("agent"), "{}", c.bin);
        let d = resolve_engine("deepseek").unwrap(); assert_eq!(d.lead, vec!["web".to_string(), "--no-open".to_string()], "the harness has a web profile and no terminal UI"); assert!(d.bin.ends_with("dsh"), "{}", d.bin);
        assert!(resolve_engine("not-an-engine").is_none() && resolve_engine("").is_none());
    }

    fn tmp_home(name: &str) -> std::path::PathBuf { let d = std::env::temp_dir().join(format!("antiloki-trust-{}-{name}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }

    #[test]
    fn cursor_names_a_folder_by_its_path_in_dashes_and_the_timestamp_is_utc_iso() {
        assert_eq!(cursor_project_slug("/home/user/dev/antiloki"), "home-user-dev-antiloki");
        assert_eq!(cursor_project_slug("/home/user/dev/my_app.v2/"), "home-user-dev-my-app-v2");
        assert_eq!(cursor_project_slug("/home/user/antiloki/bench/work/psf__requests-1142"), "home-user-antiloki-bench-work-psf-requests-1142");
        assert_eq!(cursor_project_slug("/home/user/Projects/ministore"), "home-user-Projects-ministore", "case is kept");
        assert_eq!(cursor_project_slug("///"), "");
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00.000Z"); assert_eq!(iso_utc(1_794_696_000_123), "2026-11-14T22:40:00.123Z"); assert_eq!(iso_utc(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn cursor_is_told_to_trust_a_box_once_and_a_marker_that_is_there_is_left_alone() {
        let h = tmp_home("cursor"); let paths = vec!["/work/box one".to_string(), "/work/repo".to_string()];
        assert_eq!(cursor_trust_in(&h, &paths, 0), paths);
        let m = h.join(".cursor/projects/work-box-one/.workspace-trusted"); let v: Value = serde_json::from_str(&std::fs::read_to_string(&m).unwrap()).unwrap();
        assert_eq!(v["workspacePath"], json!("/work/box one")); assert_eq!(v["trustedAt"], json!("1970-01-01T00:00:00.000Z"));
        // the second time nothing is written, and a marker the user made is not touched
        std::fs::write(h.join(".cursor/projects/work-repo/.workspace-trusted"), "{\"trustedAt\":\"mine\",\"workspacePath\":\"/work/repo\"}").unwrap();
        assert!(cursor_trust_in(&h, &paths, 99).is_empty()); assert!(std::fs::read_to_string(h.join(".cursor/projects/work-repo/.workspace-trusted")).unwrap().contains("mine"));
        assert!(cursor_trust_in(&h, &["///".to_string()], 0).is_empty(), "a path with no name is not trusted");
    }

    #[test]
    fn antigravity_trust_adds_the_folder_and_keeps_every_other_setting() {
        let h = tmp_home("agy"); let f = h.join(".gemini/antigravity-cli/settings.json"); std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, r#"{"permissions":{"allow":["x"]},"trustedWorkspaces":["/home/user"]}"#).unwrap();
        assert_eq!(antigravity_trust_in(&h, &["/home/user".into(), "/work/box".into()]), vec!["/work/box".to_string()], "only what was missing");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(v["trustedWorkspaces"], json!(["/home/user", "/work/box"])); assert_eq!(v["permissions"]["allow"], json!(["x"]));
        assert!(antigravity_trust_in(&h, &["/work/box".into()]).is_empty(), "twice is once");
        // no file: it is made; a file that is not JSON is never overwritten
        let h2 = tmp_home("agy2"); assert_eq!(antigravity_trust_in(&h2, &["/a".into()]), vec!["/a".to_string()]); assert!(h2.join(".gemini/antigravity-cli/settings.json").exists());
        let h3 = tmp_home("agy3"); let f3 = h3.join(".gemini/antigravity-cli/settings.json"); std::fs::create_dir_all(f3.parent().unwrap()).unwrap(); std::fs::write(&f3, "not json {").unwrap();
        assert!(antigravity_trust_in(&h3, &["/a".into()]).is_empty()); assert_eq!(std::fs::read_to_string(&f3).unwrap(), "not json {");
        std::fs::write(&f3, "[1,2]").unwrap(); assert!(antigravity_trust_in(&h3, &["/a".into()]).is_empty()); assert_eq!(std::fs::read_to_string(&f3).unwrap(), "[1,2]");
    }
}

#[cfg(test)]
mod ask_tests {
    use super::asks_a_question;
    #[test]
    fn the_trust_prompt_is_a_question_and_a_normal_screen_is_not() {
        assert!(asks_a_question("Quick safety check ... ❯ No, exit\n  Yes, I trust this folder\nEnter to confirm"));
        assert!(asks_a_question("Yes,Itrustthisfolder"), "spaces eaten by cursor moves");
        assert!(asks_a_question("\x1b[1mYes,\x1b[1CI\x1b[1Ctrust\x1b[0m this folder"), "cursor moves between the words");
        assert!(asks_a_question("Do you trust the contents of this directory?"));
        assert!(!asks_a_question("→ Plan, search, build anything\n  claude-opus-5-5"));
        let answered = format!("Yes, I trust this folder{}", "welcome back ".repeat(200));
        assert!(!asks_a_question(&answered), "once answered and repainted over, it is not a question any more");
    }
}
