//! RFC 0007 M0: the terminal host, driven the way a client drives it — the real `antiloki-agent pty-host` binary over
//! its socket. The claim under test: a terminal outlives its clients, and an attach repaints the exact screen.
#![cfg(unix)]

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_antiloki-agent");
static N: AtomicU32 = AtomicU32::new(0);

fn write_frame(w: &mut impl Write, v: &Value) {
    let b = serde_json::to_vec(v).unwrap();
    w.write_all(&(b.len() as u32).to_be_bytes()).unwrap();
    w.write_all(&b).unwrap();
}
fn read_frame(r: &mut impl Read) -> Value {
    let mut l = [0u8; 4];
    r.read_exact(&mut l).unwrap();
    let mut b = vec![0u8; u32::from_be_bytes(l) as usize];
    r.read_exact(&mut b).unwrap();
    serde_json::from_slice(&b).unwrap()
}

/// A host process in its own temp dir; killed and removed on drop.
struct Host { child: Child, sock: PathBuf, dir: PathBuf }
impl Host {
    fn start() -> Host {
        let dir = std::env::temp_dir().join(format!("antiloki-pty-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("pty.sock");
        let child = Command::new(BIN).arg("pty-host").arg("--sock").arg(&sock)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let t = Instant::now();
        while UnixStream::connect(&sock).is_err() {
            assert!(t.elapsed() < Duration::from_secs(5), "the host did not start");
            std::thread::sleep(Duration::from_millis(30));
        }
        Host { child, sock, dir }
    }
    fn req(&self, v: Value) -> Value {
        let mut s = UnixStream::connect(&self.sock).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write_frame(&mut s, &v);
        read_frame(&mut s)
    }
    fn spawn(&self, id: &str, argv: &[&str]) {
        let r = self.req(json!({ "op": "spawn", "id": id, "argv": argv }));
        assert_eq!(r["ok"], true, "{r}");
    }
    fn attach(&self, id: &str) -> UnixStream {
        let mut s = UnixStream::connect(&self.sock).unwrap();
        s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        write_frame(&mut s, &json!({ "op": "attach", "id": id, "rows": 24, "cols": 80 }));
        let r = read_frame(&mut s);
        assert_eq!(r["ok"], true, "{r}");
        s
    }
    fn rows(&self, id: &str) -> Vec<String> {
        let r = self.req(json!({ "op": "snapshot", "id": id }));
        r["rows"].as_array().map(|a| a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default()
    }
    fn wait_rows(&self, id: &str, needle: &str) -> Vec<String> { self.wait_rows_for(id, needle, 8) }
    fn wait_rows_for(&self, id: &str, needle: &str, secs: u64) -> Vec<String> {
        let t = Instant::now();
        loop {
            let rows = self.rows(id);
            if rows.iter().any(|r| r.contains(needle)) { return rows; }
            assert!(t.elapsed() < Duration::from_secs(secs), "{needle:?} never appeared on the screen: {rows:?}");
            std::thread::sleep(Duration::from_millis(80));
        }
    }
}
impl Drop for Host {
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); let _ = std::fs::remove_dir_all(&self.dir); }
}

/// Read from an attached stream until `needle` shows up in what was read (or panic after `secs`).
fn read_until(s: &mut UnixStream, needle: &str, secs: u64) -> String {
    let t = Instant::now();
    let mut acc = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match s.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => acc.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
        if String::from_utf8_lossy(&acc).contains(needle) { return String::from_utf8_lossy(&acc).into_owned(); }
        if t.elapsed() > Duration::from_secs(secs) { break; }
    }
    panic!("{needle:?} not seen; got {:?}", String::from_utf8_lossy(&acc));
}

const TICKER: &str = "i=0; while true; do i=$((i+1)); echo tick$i; sleep 0.1; done";

#[test]
fn a_terminal_keeps_running_while_no_client_is_attached() {
    let h = Host::start();
    h.spawn("t1", &["sh", "-c", TICKER]);
    let mut c1 = h.attach("t1");
    read_until(&mut c1, "tick2", 5);
    drop(c1); // the client goes away: a serve restart, a closed window
    std::thread::sleep(Duration::from_millis(900));
    let list = h.req(json!({ "op": "list" }));
    assert_eq!(list["terminals"][0]["alive"], true, "{list}");
    let mut c2 = h.attach("t1"); // a new client, later
    let seen = read_until(&mut c2, "tick12", 6); // ~10 ticks happened while nobody was attached
    assert!(seen.contains("tick"), "{seen:?}");
}

#[test]
fn a_terminal_survives_the_death_of_the_client_process_that_started_it() {
    let h = Host::start();
    let sock = h.sock.to_string_lossy().to_string();
    let spawn = Command::new(BIN).args(["pty", "--sock", &sock, "spawn", "t2", "--", "sh", "-c", TICKER]).output().unwrap();
    assert!(spawn.status.success(), "{}", String::from_utf8_lossy(&spawn.stderr));
    // that client process has exited by now; a second, separate client process looks
    std::thread::sleep(Duration::from_millis(500));
    let ls = Command::new(BIN).args(["pty", "--sock", &sock, "ls"]).output().unwrap();
    let out = String::from_utf8_lossy(&ls.stdout);
    assert!(out.contains("\"alive\":true"), "{out}");
    let snap = Command::new(BIN).args(["pty", "--sock", &sock, "snapshot", "t2"]).output().unwrap();
    assert!(String::from_utf8_lossy(&snap.stdout).contains("tick"), "{}", String::from_utf8_lossy(&snap.stdout));
}

#[test]
fn an_attach_repaints_the_exact_screen() {
    let h = Host::start();
    h.spawn("t3", &["sh", "-c", r"printf '\033[2J\033[Hline1\r\n\033[1;32mgreen\033[0m\033[5;10Hcol'; sleep 30"]);
    let before = h.wait_rows("t3", "col");
    let mut c = h.attach("t3");
    std::thread::sleep(Duration::from_millis(300));
    let mut got = Vec::new();
    let mut buf = [0u8; 8192];
    while let Ok(n) = c.read(&mut buf) { if n == 0 { break; } got.extend_from_slice(&buf[..n]); }
    let mut fresh = vt100::Parser::new(std::num::NonZeroU16::new(24).unwrap(), std::num::NonZeroU16::new(80).unwrap(), 0);
    fresh.process(&got);
    let after: Vec<String> = fresh.screen().rows(0, 80).collect();
    assert_eq!(before, after, "the attach must repaint the screen the host holds");
    assert!(after[0].starts_with("line1") && after[1].starts_with("green") && after[4].contains("col"));
}

#[test]
fn keystrokes_reach_the_child_and_its_output_comes_back() {
    let h = Host::start();
    h.spawn("t4", &["cat"]);
    let mut c = h.attach("t4");
    c.write_all(b"hello from the client\n").unwrap();
    read_until(&mut c, "hello from the client", 5);
}

#[test]
fn a_detached_client_is_forgotten_even_when_the_terminal_is_idle() {
    let h = Host::start();
    h.spawn("t8", &["cat"]); // prints nothing on its own: only a detach can remove the client
    let clients = || h.req(json!({ "op": "list" }))["terminals"][0]["clients"].as_u64().unwrap();
    let a = h.attach("t8");
    let b = h.attach("t8");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(clients(), 2);
    drop(a);
    let t = Instant::now();
    while clients() != 1 { assert!(t.elapsed() < Duration::from_secs(3), "the first detach was never noticed"); std::thread::sleep(Duration::from_millis(50)); }
    drop(b);
    while clients() != 0 { assert!(t.elapsed() < Duration::from_secs(6), "the second detach was never noticed"); std::thread::sleep(Duration::from_millis(50)); }
}

#[test]
fn a_client_process_killed_outright_is_forgotten_and_the_terminal_lives() {
    let h = Host::start();
    h.spawn("t9", &["cat"]);
    let clients = || h.req(json!({ "op": "list" }))["terminals"][0]["clients"].as_u64().unwrap();
    let sock = h.sock.to_string_lossy().to_string();
    // the real attach client, as a separate process (no tty here: its stty calls fail harmlessly)
    let mut c = Command::new(BIN).args(["pty", "--sock", &sock, "attach", "t9"])
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let t = Instant::now();
    while clients() != 1 { assert!(t.elapsed() < Duration::from_secs(5), "the attach client never registered"); std::thread::sleep(Duration::from_millis(50)); }
    c.kill().unwrap(); // SIGKILL: no chance to say goodbye
    let _ = c.wait();
    while clients() != 0 { assert!(t.elapsed() < Duration::from_secs(8), "the dead client was never forgotten"); std::thread::sleep(Duration::from_millis(50)); }
    let l = h.req(json!({ "op": "list" }));
    assert_eq!(l["terminals"][0]["alive"], true, "{l}");
}

#[test]
fn a_spawn_can_start_from_a_clean_environment_and_remove_variables() {
    let h = Host::start();
    // env_clear: exactly the environment given, TERM defaulted; nothing of the host's own leaks in
    let r = h.req(json!({ "op": "spawn", "id": "e1", "env_clear": true, "env": { "A": "one", "PATH": "/usr/bin:/bin" },
        "argv": ["sh", "-c", "echo A=$A HOME=[$HOME] TERM=$TERM; sleep 30"] }));
    assert_eq!(r["ok"], true, "{r}");
    let rows = h.wait_rows("e1", "A=one");
    assert!(rows.iter().any(|x| x.contains("HOME=[]") && x.contains("TERM=xterm-256color")), "{rows:?}");
    // env_remove on the host's own environment
    let r = h.req(json!({ "op": "spawn", "id": "e2", "env_remove": ["HOME"], "argv": ["sh", "-c", "echo HOME=[$HOME]; sleep 30"] }));
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.wait_rows("e2", "HOME=[").iter().any(|x| x.contains("HOME=[]")));
}

#[test]
fn replace_ends_the_running_terminal_and_starts_a_new_one_under_the_same_id() {
    let h = Host::start();
    h.spawn("r1", &["sh", "-c", "echo first; sleep 60"]);
    h.wait_rows("r1", "first");
    // without replace it is refused
    let r = h.req(json!({ "op": "spawn", "id": "r1", "argv": ["sh", "-c", "echo second; sleep 60"] }));
    assert_eq!(r["ok"], false, "{r}");
    let r = h.req(json!({ "op": "spawn", "id": "r1", "replace": true, "argv": ["sh", "-c", "echo second; sleep 60"] }));
    assert_eq!(r["ok"], true, "{r}");
    let rows = h.wait_rows("r1", "second");
    assert!(!rows.iter().any(|x| x.contains("first")), "the new terminal starts on a clean screen: {rows:?}");
    assert_eq!(h.req(json!({ "op": "list" }))["terminals"].as_array().unwrap().len(), 1);
}

#[test]
fn a_resize_reaches_the_child() {
    let h = Host::start();
    h.spawn("t5", &["sh", "-c", "stty size; trap 'stty size' WINCH; while :; do sleep 0.1; done"]);
    h.wait_rows("t5", "24 80");
    let r = h.req(json!({ "op": "resize", "id": "t5", "rows": 30, "cols": 100 }));
    assert_eq!(r["ok"], true);
    h.wait_rows("t5", "30 100");
}

#[test]
fn a_stuck_client_is_dropped_and_never_stalls_the_host() {
    let h = Host::start();
    // 12 MB through the screen engine: seconds in a debug build, so the waits below are generous
    h.spawn("t6", &["sh", "-c", "head -c 12000000 /dev/zero | tr '\\0' x; echo FLOOD_DONE; sleep 30"]);
    let _stuck = h.attach("t6"); // attached, and it never reads
    let t = Instant::now();
    let mut worst = Duration::ZERO;
    while t.elapsed() < Duration::from_secs(3) {
        let s = Instant::now();
        h.req(json!({ "op": "list" })); // list takes the same lock the output pump holds, so it is the honest probe
        worst = worst.max(s.elapsed());
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(worst < Duration::from_secs(1), "the host stalled for {worst:?} behind a stuck client");
    h.wait_rows_for("t6", "FLOOD_DONE", 60);
    let list = h.req(json!({ "op": "list" }));
    assert_eq!(list["terminals"][0]["clients"], 0, "the stuck client should have been dropped: {list}");
}

#[test]
fn an_exited_terminal_reports_its_exit_code_and_keeps_its_last_screen() {
    let h = Host::start();
    h.spawn("t7", &["sh", "-c", "echo bye; exit 7"]);
    let t = Instant::now();
    loop {
        let list = h.req(json!({ "op": "list" }));
        if list["terminals"][0]["alive"] == false {
            assert_eq!(list["terminals"][0]["exit"], 7, "{list}");
            break;
        }
        assert!(t.elapsed() < Duration::from_secs(8), "the terminal never reported its exit");
        std::thread::sleep(Duration::from_millis(80));
    }
    assert!(h.rows("t7").iter().any(|r| r.contains("bye")));
}

#[test]
fn the_socket_is_for_the_current_user_only() {
    let h = Host::start();
    let mode = std::fs::metadata(Path::new(&h.sock)).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0, "socket mode is {mode:o}");
}

// ---- real TUIs -------------------------------------------------------------------------------------------------

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file())).unwrap_or(false)
}

/// Attach, read what the host sends first, parse it on a fresh engine, and compare with what the host says is on screen.
/// A live TUI may repaint between the two reads, so a few tries are allowed; one exact match is the proof.
fn repaint_matches(h: &Host, id: &str) -> bool {
    for _ in 0..8 {
        let before = h.rows(id);
        let mut c = h.attach(id);
        std::thread::sleep(Duration::from_millis(350));
        let mut got = Vec::new();
        let mut buf = [0u8; 65536];
        while let Ok(n) = c.read(&mut buf) { if n == 0 { break; } got.extend_from_slice(&buf[..n]); }
        let mut fresh = vt100::Parser::new(std::num::NonZeroU16::new(24).unwrap(), std::num::NonZeroU16::new(80).unwrap(), 0);
        fresh.process(&got);
        let parsed: Vec<String> = fresh.screen().rows(0, 80).collect();
        let after = h.rows(id);
        if parsed == before || parsed == after { return true; }
        std::thread::sleep(Duration::from_millis(400));
    }
    false
}

/// Real TUIs through the host: nvim and less by default; `ANTILOKI_PTY_REAL_TUIS="claude;codex;opencode"` adds more
/// (each entry is one command line, split on spaces). A TUI that is not installed is skipped and said so.
#[test]
fn real_tuis_repaint_exactly_on_attach() {
    let file = std::env::temp_dir().join(format!("antiloki-pty-tui-{}.txt", std::process::id()));
    std::fs::write(&file, (1..=60).map(|i| format!("line {i} of the file\n")).collect::<String>()).unwrap();
    let f = file.to_string_lossy().to_string();
    let mut tuis: Vec<Vec<String>> = vec![
        vec!["nvim".into(), "-u".into(), "NONE".into(), "-n".into(), f.clone()],
        vec!["less".into(), f.clone()],
    ];
    for extra in std::env::var("ANTILOKI_PTY_REAL_TUIS").unwrap_or_default().split(';').filter(|s| !s.trim().is_empty()) {
        tuis.push(extra.split_whitespace().map(String::from).collect());
    }
    let h = Host::start();
    let mut ran = 0;
    for (i, argv) in tuis.iter().enumerate() {
        if !on_path(&argv[0]) { eprintln!("skipping {}: not installed", argv[0]); continue; }
        let id = format!("tui{i}");
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        h.spawn(&id, &refs);
        let t = Instant::now();
        while h.rows(&id).iter().all(|r| r.trim().is_empty()) {
            assert!(t.elapsed() < Duration::from_secs(20), "{} drew nothing in 20 s", argv[0]);
            std::thread::sleep(Duration::from_millis(100));
        }
        std::thread::sleep(Duration::from_millis(1200)); // let the first paint settle
        let shown: Vec<String> = h.rows(&id).into_iter().filter(|r| !r.trim().is_empty()).take(3).collect();
        eprintln!("{} drew: {:?}", argv[0], shown);
        assert!(repaint_matches(&h, &id), "{}: the attach did not repaint the screen the host holds", argv[0]);
        h.req(json!({ "op": "kill", "id": id }));
        ran += 1;
    }
    let _ = std::fs::remove_file(file);
    assert!(ran >= 1, "no TUI was installed to test with");
}
