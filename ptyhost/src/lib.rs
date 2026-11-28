//! The terminal host (RFC 0007), run as `antiloki-agent pty-host`.
//!
//! A small daemon that OWNS the PTYs, so a terminal outlives the process that started it: the serve, the window, an
//! update. Today `serve/src/pty.rs` keeps its PTYs in-process, and OPERATING-RULES §2 says a serve restart kills every
//! agent terminal. Here the serve (or any client) connects over a per-user local socket, attaches, detaches, and the
//! terminal keeps running.
//!
//! What it keeps per terminal: the PTY, the child, and a screen state from a `ScreenEngine` (vt100 today; Ghostty's
//! libghostty-vt is the other candidate, RFC 0007 §5.1.1), so an attach is handed the exact screen first (input modes
//! included: bracketed paste, application cursor), then the live bytes. Nothing is parsed or rewritten on the way
//! through: the child writes to a real PTY and the attached client gets the bytes as they came.
//!
//! Protocol: length-prefixed JSON frames (u32 big-endian length, then JSON) for requests and replies. `attach` is the
//! one request that turns the connection into a raw byte stream: the reply frame, then the snapshot bytes, then live
//! output one way, and the client's keystrokes the other. Closing the connection is the detach; the terminal lives on.
//!
//! A slow client is dropped, never waited for: each attached client has a bounded queue and is disconnected when it
//! fills, so one stuck viewer cannot stall the child or the others.
//!
//! Unix only for now (a Unix socket in a 0700 directory). The Windows named-pipe transport is RFC 0007 M7.
#![cfg(unix)]

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::Duration;

const MAX_FRAME: usize = 16 << 20;
/// Queued output chunks per attached client before it is dropped as too slow.
const CLIENT_QUEUE: usize = 512;
const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;

fn lk<T>(m: &Mutex<T>) -> MutexGuard<'_, T> { m.lock().unwrap_or_else(|p| p.into_inner()) }
fn nz(v: u16) -> NonZeroU16 { NonZeroU16::new(v.max(1)).unwrap() }

// ---------------------------------------------------------------------------------------------------------------
// The screen engine
// ---------------------------------------------------------------------------------------------------------------

/// What the host needs from a terminal-emulation engine. One engine instance belongs to one terminal and is only ever
/// touched under that terminal's lock, so it need not be thread-safe beyond `Send`.
pub trait ScreenEngine: Send {
    fn feed(&mut self, bytes: &[u8]);
    fn resize(&mut self, rows: u16, cols: u16);
    /// Bytes that repaint this exact screen, and restore its input modes, on a fresh terminal of the same size.
    fn snapshot(&self) -> Vec<u8>;
    /// The visible rows as plain text, one string per row.
    fn text(&self) -> Vec<String>;
    /// (row, column), zero-based.
    fn cursor(&self) -> (u16, u16);
}

pub struct Vt100Engine { p: vt100::Parser }
impl Vt100Engine {
    pub fn new(rows: u16, cols: u16) -> Self { Vt100Engine { p: vt100::Parser::new(nz(rows), nz(cols), 2000) } }
}
impl ScreenEngine for Vt100Engine {
    fn feed(&mut self, bytes: &[u8]) { self.p.process(bytes); }
    fn resize(&mut self, rows: u16, cols: u16) { self.p.set_size(nz(rows), nz(cols)); }
    fn snapshot(&self) -> Vec<u8> { self.p.screen().state_formatted().into_bytes() }
    fn text(&self) -> Vec<String> {
        let (_, cols) = self.p.screen().size();
        self.p.screen().rows(0, cols.get()).collect()
    }
    fn cursor(&self) -> (u16, u16) { self.p.screen().cursor_position() }
}

// ---------------------------------------------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------------------------------------------

pub fn write_frame(w: &mut impl Write, v: &Value) -> std::io::Result<()> {
    let b = serde_json::to_vec(v).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    w.write_all(&(b.len() as u32).to_be_bytes())?;
    w.write_all(&b)?;
    w.flush()
}

/// `Ok(None)` is a clean end of the connection.
pub fn read_frame(r: &mut impl Read) -> std::io::Result<Option<Value>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_be_bytes(len) as usize;
    if n > MAX_FRAME { return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large")); }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b)?;
    serde_json::from_slice(&b).map(Some).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

// ---------------------------------------------------------------------------------------------------------------
// The host
// ---------------------------------------------------------------------------------------------------------------

struct St {
    engine: Box<dyn ScreenEngine>,
    /// attached clients: (id, queue). The id lets a detach remove exactly its own entry, so an idle terminal's count is true.
    subs: Vec<(u64, mpsc::SyncSender<Vec<u8>>)>,
    next_sub: u64,
    exit: Option<i32>,
    rows: u16,
    cols: u16,
    argv: Vec<String>,
}

struct Term {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    st: Mutex<St>,
}

#[derive(Default)]
struct Host { terms: Mutex<HashMap<String, Arc<Term>>> }

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str { v.get(k).and_then(Value::as_str).unwrap_or("") }
fn u16_of(v: &Value, k: &str, d: u16) -> u16 { v.get(k).and_then(Value::as_u64).map(|n| n.min(u16::MAX as u64) as u16).filter(|n| *n > 0).unwrap_or(d) }
fn err(msg: impl Into<String>) -> Value { json!({ "ok": false, "error": msg.into() }) }

impl Host {
    fn get(&self, id: &str) -> Option<Arc<Term>> { lk(&self.terms).get(id).cloned() }

    fn spawn(&self, req: &Value) -> Value {
        let id = str_of(req, "id").to_string();
        if id.is_empty() { return err("spawn needs an id"); }
        let argv: Vec<String> = req.get("argv").and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        if argv.is_empty() { return err("spawn needs a non-empty argv"); }
        if let Some(t) = self.get(&id) {
            let running = lk(&t.st).exit.is_none();
            if running && req.get("replace").and_then(Value::as_bool) == Some(true) {
                // a restart on purpose (the serve's "a re-start replaces the prior terminal"): end the old one and wait for it
                let _ = lk(&t.child).kill();
                let t0 = std::time::Instant::now();
                while lk(&t.st).exit.is_none() && t0.elapsed() < Duration::from_secs(3) { std::thread::sleep(Duration::from_millis(20)); }
            } else if running { return err(format!("terminal {id} is already running")); }
        }
        let (rows, cols) = (u16_of(req, "rows", DEFAULT_ROWS), u16_of(req, "cols", DEFAULT_COLS));
        let pair = match native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }) {
            Ok(p) => p, Err(e) => return err(format!("could not open a pty: {e}")),
        };
        let mut cmd = CommandBuilder::new(&argv[0]);
        cmd.args(&argv[1..]);
        if let Some(cwd) = req.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()) { cmd.cwd(cwd); }
        // `env_clear`: start from nothing and use exactly `env` (the serve sends its own environment, so a terminal gets the
        // serve's PATH and HOME, not the ones the host happened to be started with). Otherwise the host's environment is the base.
        let clear = req.get("env_clear").and_then(Value::as_bool) == Some(true);
        if clear { cmd.env_clear(); }
        let env = req.get("env").and_then(Value::as_object);
        if let Some(env) = env { for (k, v) in env { if let Some(v) = v.as_str() { cmd.env(k, v); } } }
        if let Some(rm) = req.get("env_remove").and_then(Value::as_array) { for k in rm.iter().filter_map(Value::as_str) { cmd.env_remove(k); } }
        let given = |k: &str| env.map_or(false, |e| e.contains_key(k));
        if !given("TERM") && (clear || std::env::var_os("TERM").is_none()) { cmd.env("TERM", "xterm-256color"); }
        if !given("COLORTERM") { cmd.env("COLORTERM", "truecolor"); }
        let child = match pair.slave.spawn_command(cmd) {
            Ok(c) => c, Err(e) => return err(format!("could not start {}: {e}", argv[0])),
        };
        drop(pair.slave);
        let (reader, writer) = match (pair.master.try_clone_reader(), pair.master.take_writer()) {
            (Ok(r), Ok(w)) => (r, w), (Err(e), _) | (_, Err(e)) => return err(format!("pty io: {e}")),
        };
        let term = Arc::new(Term {
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            st: Mutex::new(St { engine: Box::new(Vt100Engine::new(rows, cols)), subs: vec![], next_sub: 0, exit: None, rows, cols, argv }),
        });
        lk(&self.terms).insert(id, term.clone());
        std::thread::spawn(move || pump(term, reader));
        json!({ "ok": true })
    }

    fn list(&self) -> Value {
        let terms = lk(&self.terms);
        let mut out: Vec<Value> = terms.iter().map(|(id, t)| {
            let st = lk(&t.st);
            json!({ "id": id, "alive": st.exit.is_none(), "exit": st.exit, "rows": st.rows, "cols": st.cols, "argv": st.argv, "clients": st.subs.len() })
        }).collect();
        out.sort_by(|a, b| str_of(a, "id").cmp(str_of(b, "id")));
        json!({ "ok": true, "terminals": out })
    }

    fn snapshot(&self, id: &str) -> Value {
        let Some(t) = self.get(id) else { return err(format!("no terminal {id}")) };
        let st = lk(&t.st);
        let (r, c) = st.engine.cursor();
        json!({ "ok": true, "rows": st.engine.text(), "cursor": [r, c], "ansi": String::from_utf8_lossy(&st.engine.snapshot()),
                "alive": st.exit.is_none(), "exit": st.exit })
    }

    fn write(&self, id: &str, data: &str) -> Value {
        let Some(t) = self.get(id) else { return err(format!("no terminal {id}")) };
        if lk(&t.st).exit.is_some() { return err(format!("terminal {id} has exited")); }
        let mut w = lk(&t.writer);
        match w.write_all(data.as_bytes()).and_then(|_| w.flush()) { Ok(()) => json!({ "ok": true }), Err(e) => err(e.to_string()) }
    }

    fn resize(&self, id: &str, rows: u16, cols: u16) -> Value {
        let Some(t) = self.get(id) else { return err(format!("no terminal {id}")) };
        resize_term(&t, rows, cols);
        json!({ "ok": true })
    }

    fn kill(&self, id: &str) -> Value {
        let Some(t) = self.get(id) else { return err(format!("no terminal {id}")) };
        let killed = lk(&t.child).kill();
        match killed { Ok(()) => json!({ "ok": true }), Err(e) => err(e.to_string()) }
    }
}

fn resize_term(t: &Term, rows: u16, cols: u16) {
    let _ = lk(&t.master).resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
    let mut st = lk(&t.st);
    st.engine.resize(rows, cols);
    st.rows = rows;
    st.cols = cols;
}

/// Read the child's output until it closes, feed the screen engine, fan the bytes out. Then record the exit.
fn pump(t: Arc<Term>, mut reader: Box<dyn Read + Send>) {
    let mut buf = [0u8; 8192];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut st = lk(&t.st);
                st.engine.feed(&buf[..n]);
                // a full or gone client is dropped here: it never holds up the child or the other clients
                st.subs.retain(|(_, s)| s.try_send(buf[..n].to_vec()).is_ok());
            }
        }
    }
    let code = loop {
        let polled = lk(&t.child).try_wait();
        match polled {
            Ok(Some(status)) => break status.exit_code() as i32,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break -1,
        }
    };
    let mut st = lk(&t.st);
    st.exit = Some(code);
    st.subs.clear(); // closes every attached client's stream
}

/// The connection becomes a raw byte stream: snapshot, then live output out; keystrokes in. Returns when it ends.
fn attach(host: &Host, mut stream: UnixStream, req: &Value) {
    let id = str_of(req, "id").to_string();
    let Some(t) = host.get(&id) else { let _ = write_frame(&mut stream, &err(format!("no terminal {id}"))); return };
    if req.get("rows").is_some() && req.get("cols").is_some() {
        resize_term(&t, u16_of(req, "rows", DEFAULT_ROWS), u16_of(req, "cols", DEFAULT_COLS));
    }
    // Snapshot and subscription are taken together under the terminal's lock, so no byte falls between them.
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(CLIENT_QUEUE);
    let (snap, exit, sub_id) = {
        let mut st = lk(&t.st);
        let snap = st.engine.snapshot();
        st.next_sub += 1;
        let sub_id = st.next_sub;
        if st.exit.is_none() { st.subs.push((sub_id, tx)); }
        (snap, st.exit, sub_id)
    };
    if write_frame(&mut stream, &json!({ "ok": true, "stream": true, "exit": exit })).is_err() { return; }
    let done = Arc::new(AtomicBool::new(false));
    let Ok(mut out) = stream.try_clone() else { return };
    // a client that stops reading must not pin a thread forever: its write fails after this and the thread ends
    let _ = out.set_write_timeout(Some(Duration::from_secs(5)));
    let done_w = done.clone();
    let writer = std::thread::spawn(move || {
        if out.write_all(&snap).is_err() { return; }
        loop {
            match rx.recv_timeout(Duration::from_millis(300)) {
                Ok(chunk) => { if out.write_all(&chunk).is_err() { break; } }
                Err(mpsc::RecvTimeoutError::Timeout) => { if done_w.load(Ordering::Relaxed) { break; } }
                Err(mpsc::RecvTimeoutError::Disconnected) => break, // the terminal ended, or this client was too slow
            }
        }
        let _ = out.shutdown(std::net::Shutdown::Both);
    });
    // client → child
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut w = lk(&t.writer);
                if w.write_all(&buf[..n]).and_then(|_| w.flush()).is_err() { break; }
            }
        }
    }
    done.store(true, Ordering::Relaxed);
    let _ = writer.join();
    // the client is gone: take exactly its entry out, even if the terminal is idle and nothing would ever fail to send
    lk(&t.st).subs.retain(|(i, _)| *i != sub_id);
}

fn handle_conn(host: Arc<Host>, mut stream: UnixStream) {
    loop {
        let req = match read_frame(&mut stream) { Ok(Some(r)) => r, _ => return };
        let reply = match str_of(&req, "op") {
            "ping" => json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }),
            "spawn" => host.spawn(&req),
            "list" => host.list(),
            "snapshot" => host.snapshot(str_of(&req, "id")),
            "write" => host.write(str_of(&req, "id"), str_of(&req, "data")),
            "resize" => host.resize(str_of(&req, "id"), u16_of(&req, "rows", DEFAULT_ROWS), u16_of(&req, "cols", DEFAULT_COLS)),
            "kill" => host.kill(str_of(&req, "id")),
            "attach" => { attach(&host, stream, &req); return }
            "shutdown" => { let _ = write_frame(&mut stream, &json!({ "ok": true })); std::process::exit(0) }
            other => err(format!("unknown op {other:?}")),
        };
        if write_frame(&mut stream, &reply).is_err() { return; }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Socket location, server, client
// ---------------------------------------------------------------------------------------------------------------

/// `--sock`, then `ANTILOKI_PTY_SOCK`, then `$XDG_RUNTIME_DIR/antiloki/pty.sock`, then `~/.antiloki/run/pty/pty.sock`.
/// The directory is created 0700 when this derives it, so the socket is reachable by the current user only.
pub fn sock_path(arg: Option<&str>) -> PathBuf {
    if let Some(p) = arg.filter(|p| !p.is_empty()) { return PathBuf::from(p); }
    if let Some(p) = std::env::var_os("ANTILOKI_PTY_SOCK").filter(|p| !p.is_empty()) { return PathBuf::from(p); }
    if let Some(x) = std::env::var_os("XDG_RUNTIME_DIR").filter(|p| !p.is_empty()) { return PathBuf::from(x).join("antiloki").join("pty.sock"); }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".antiloki").join("run").join("pty").join("pty.sock")
}

fn prepare_dir(sock: &Path, derived: bool) -> std::io::Result<()> {
    if let Some(dir) = sock.parent() {
        let existed = dir.exists();
        std::fs::create_dir_all(dir)?;
        if derived && !existed { std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?; }
    }
    Ok(())
}

pub struct Client { s: UnixStream }
impl Client {
    pub fn connect(sock: &Path) -> std::io::Result<Client> { Ok(Client { s: UnixStream::connect(sock)? }) }
    pub fn request(&mut self, v: &Value) -> std::io::Result<Value> {
        write_frame(&mut self.s, v)?;
        read_frame(&mut self.s)?.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "host closed the connection"))
    }
    /// The reply to `attach`, then the connection is yours as a raw byte stream.
    pub fn attach(mut self, id: &str, rows: u16, cols: u16) -> std::io::Result<(Value, UnixStream)> {
        let reply = self.request(&json!({ "op": "attach", "id": id, "rows": rows, "cols": cols }))?;
        Ok((reply, self.s))
    }
}

/// Connect to the host, starting it detached when nobody answers. The host is this same binary (`antiloki-agent`).
pub fn connect_or_start(sock: &Path) -> std::io::Result<Client> { connect_or_start_with(&std::env::current_exe()?, sock) }

/// The same, with the host binary named: the serve is not the agent, so it says where the agent is.
pub fn connect_or_start_with(exe: &Path, sock: &Path) -> std::io::Result<Client> {
    if let Ok(c) = Client::connect(sock) { return Ok(c); }
    use std::os::unix::process::CommandExt;
    std::process::Command::new(exe).arg("pty-host").arg("--sock").arg(sock)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .process_group(0) // its own group: closing the starter's terminal does not signal it
        .spawn()?;
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(50));
        if let Ok(c) = Client::connect(sock) { return Ok(c); }
    }
    Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the pty host did not start"))
}

fn arg_after(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

/// `antiloki-agent pty-host [--sock <path>]` — run the host in the foreground.
pub fn host_main(args: &[String]) -> i32 {
    let arg = arg_after(args, "--sock");
    let sock = sock_path(arg.as_deref());
    if let Err(e) = prepare_dir(&sock, arg.is_none() && std::env::var_os("ANTILOKI_PTY_SOCK").is_none()) {
        eprintln!("antiloki pty-host: {e}"); return 1;
    }
    if Client::connect(&sock).is_ok() { eprintln!("antiloki pty-host: already running at {}", sock.display()); return 0; }
    let _ = std::fs::remove_file(&sock); // a stale socket nobody answers on
    let listener = match UnixListener::bind(&sock) { Ok(l) => l, Err(e) => { eprintln!("antiloki pty-host: cannot bind {}: {e}", sock.display()); return 1; } };
    let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));
    eprintln!("antiloki pty-host listening on {}", sock.display());
    let host = Arc::new(Host::default());
    for conn in listener.incoming().flatten() {
        let h = host.clone();
        std::thread::spawn(move || handle_conn(h, conn));
    }
    0
}

/// `antiloki-agent pty [--sock <path>] <op> …` — a small client, for scripts and for trying the host by hand.
///   ls · ping · shutdown · spawn <id> [--rows N] [--cols N] [--cwd D] -- <argv…> · snapshot <id>
///   send <id> <text> (types it and presses enter) · write <id> <text> (raw) · resize <id> <cols> <rows> · kill <id>
///   attach <id> (this terminal becomes the terminal's screen and keyboard; ctrl+] detaches, the terminal keeps running)
pub fn ctl_main(args: &[String]) -> i32 {
    let mut rest: Vec<String> = args.to_vec();
    let sock_arg = arg_after(&rest, "--sock");
    if let Some(i) = rest.iter().position(|a| a == "--sock") { rest.drain(i..(i + 2).min(rest.len())); }
    let sock = sock_path(sock_arg.as_deref());
    let Some(op) = rest.first().cloned() else { eprintln!("usage: antiloki-agent pty [--sock P] <ls|ping|shutdown|spawn|snapshot|send|write|resize|kill> …"); return 2 };
    let id = rest.get(1).cloned().unwrap_or_default();
    if op == "attach" { return attach_cli(&sock, &id); }
    let req = match op.as_str() {
        "ls" => json!({ "op": "list" }),
        "ping" | "shutdown" => json!({ "op": op }),
        "snapshot" | "kill" => json!({ "op": op, "id": id }),
        "send" => json!({ "op": "write", "id": id, "data": format!("{}\r", rest[2..].join(" ")) }),
        "write" => json!({ "op": "write", "id": id, "data": rest[2..].join(" ") }),
        "resize" => json!({ "op": "resize", "id": id, "cols": rest.get(2).and_then(|v| v.parse::<u64>().ok()).unwrap_or(80), "rows": rest.get(3).and_then(|v| v.parse::<u64>().ok()).unwrap_or(24) }),
        "spawn" => {
            let dd = rest.iter().position(|a| a == "--").unwrap_or(rest.len());
            let opts = &rest[..dd];
            let argv: Vec<String> = rest.get(dd + 1..).map(|a| a.to_vec()).unwrap_or_default();
            json!({ "op": "spawn", "id": id, "argv": argv, "cwd": arg_after(opts, "--cwd").unwrap_or_default(),
                    "rows": arg_after(opts, "--rows").and_then(|v| v.parse::<u64>().ok()).unwrap_or(DEFAULT_ROWS as u64),
                    "cols": arg_after(opts, "--cols").and_then(|v| v.parse::<u64>().ok()).unwrap_or(DEFAULT_COLS as u64) })
        }
        other => { eprintln!("antiloki pty: unknown op {other:?}"); return 2 }
    };
    let mut c = match if op == "spawn" { connect_or_start(&sock) } else { Client::connect(&sock) } {
        Ok(c) => c, Err(e) => { eprintln!("antiloki pty: no host at {} ({e}); run `antiloki-agent pty spawn …` to start one", sock.display()); return 1 }
    };
    match c.request(&req) {
        Ok(v) => {
            if op == "snapshot" && v.get("ok") == Some(&Value::Bool(true)) {
                for r in v.get("rows").and_then(Value::as_array).into_iter().flatten() { println!("{}", r.as_str().unwrap_or("").trim_end()); }
            } else { println!("{v}"); }
            if v.get("ok") == Some(&Value::Bool(true)) { 0 } else { 1 }
        }
        Err(e) if op == "shutdown" && e.kind() == std::io::ErrorKind::UnexpectedEof => 0,
        Err(e) => { eprintln!("antiloki pty: {e}"); 1 }
    }
}

/// The terminal size of this process's own tty, from `stty size` (no ioctl binding needed in this crate).
fn tty_size() -> (u16, u16) {
    let out = std::process::Command::new("stty").arg("size").stdin(std::process::Stdio::inherit()).output().ok();
    let text = out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    let mut it = text.split_whitespace().filter_map(|v| v.parse::<u16>().ok());
    (it.next().unwrap_or(DEFAULT_ROWS), it.next().unwrap_or(DEFAULT_COLS))
}

fn stty(args: &[&str]) -> String {
    std::process::Command::new("stty").args(args).stdin(std::process::Stdio::inherit()).output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

/// `pty attach <id>`: raw mode, the host's snapshot and live output to stdout, keystrokes to the terminal.
/// Ctrl+] detaches. The size is sent at attach and again when this tty is resized (polled; M1 uses the front end's events).
fn attach_cli(sock: &Path, id: &str) -> i32 {
    if id.is_empty() { eprintln!("usage: antiloki-agent pty attach <id>"); return 2; }
    let (rows, cols) = tty_size();
    let client = match Client::connect(sock) { Ok(c) => c, Err(e) => { eprintln!("antiloki pty: no host at {} ({e})", sock.display()); return 1 } };
    let (reply, stream) = match client.attach(id, rows, cols) { Ok(r) => r, Err(e) => { eprintln!("antiloki pty: {e}"); return 1 } };
    if reply.get("ok") != Some(&Value::Bool(true)) { eprintln!("antiloki pty: {}", reply.get("error").and_then(Value::as_str).unwrap_or("attach refused")); return 1; }
    let saved = stty(&["-g"]);
    let _ = stty(&["raw", "-echo"]);
    let (done_tx, done_rx) = mpsc::channel::<&'static str>();
    // terminal → screen
    let (mut from_host, d1) = (stream.try_clone().expect("clone"), done_tx.clone());
    std::thread::spawn(move || {
        let mut out = std::io::stdout();
        let mut buf = [0u8; 16384];
        while let Ok(n) = from_host.read(&mut buf) { if n == 0 || out.write_all(&buf[..n]).and_then(|_| out.flush()).is_err() { break; } }
        let _ = d1.send("the terminal ended");
    });
    // keyboard → terminal
    let (mut to_host, d2) = (stream.try_clone().expect("clone"), done_tx.clone());
    std::thread::spawn(move || {
        let mut inp = std::io::stdin();
        let mut buf = [0u8; 4096];
        while let Ok(n) = inp.read(&mut buf) {
            if n == 0 { break; }
            if let Some(i) = buf[..n].iter().position(|b| *b == 0x1d) { // ctrl+]
                if i > 0 { let _ = to_host.write_all(&buf[..i]); }
                let _ = d2.send("detached");
                return;
            }
            if to_host.write_all(&buf[..n]).is_err() { break; }
        }
        let _ = d2.send("input closed");
    });
    // window size changes
    let (sock_p, id_s, mut last) = (sock.to_path_buf(), id.to_string(), (rows, cols));
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(400));
        let now = tty_size();
        if now != last {
            last = now;
            if let Ok(mut c) = Client::connect(&sock_p) { let _ = c.request(&json!({ "op": "resize", "id": id_s, "rows": now.0, "cols": now.1 })); }
        }
    });
    let why = done_rx.recv().unwrap_or("closed");
    let _ = stty(&[&saved]);
    println!("\r\n[antiloki pty: {why}; terminal {id} keeps running]");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_a_closed_stream_is_none() {
        let mut buf: Vec<u8> = vec![];
        write_frame(&mut buf, &json!({ "op": "ping", "n": 3 })).unwrap();
        let mut cur = std::io::Cursor::new(buf);
        assert_eq!(read_frame(&mut cur).unwrap(), Some(json!({ "op": "ping", "n": 3 })));
        assert_eq!(read_frame(&mut cur).unwrap(), None);
    }

    #[test]
    fn an_oversized_frame_is_refused_not_allocated() {
        let mut cur = std::io::Cursor::new(((MAX_FRAME as u32) + 1).to_be_bytes().to_vec());
        assert!(read_frame(&mut cur).is_err());
    }

    #[test]
    fn a_snapshot_repaints_the_same_screen_on_a_fresh_engine() {
        let mut a = Vt100Engine::new(10, 40);
        a.feed(b"\x1b[2J\x1b[Hhello\r\n\x1b[1;32mgreen\x1b[0m\x1b[5;12Hcol");
        let mut b = Vt100Engine::new(10, 40);
        b.feed(&a.snapshot());
        assert_eq!(a.text(), b.text());
        assert_eq!(a.cursor(), b.cursor());
        assert!(a.text()[0].starts_with("hello"));
    }

    #[test]
    fn a_snapshot_keeps_input_modes_a_tui_turned_on() {
        let mut a = Vt100Engine::new(5, 20);
        a.feed(b"\x1b[?2004h"); // bracketed paste on, as the vendor TUIs do
        let snap = String::from_utf8(a.snapshot()).unwrap();
        assert!(snap.contains("\x1b[?2004h"), "bracketed paste must survive a reattach: {snap:?}");
    }

    #[test]
    fn resize_reflows_the_screen_size() {
        let mut a = Vt100Engine::new(5, 20);
        a.resize(8, 30);
        assert_eq!(a.text().len(), 8);
    }

    #[test]
    fn the_socket_path_prefers_the_argument() {
        assert_eq!(sock_path(Some("/x/y.sock")), PathBuf::from("/x/y.sock"));
    }
}
