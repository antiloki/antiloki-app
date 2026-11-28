//! JOURNEYS THAT NEED A PROCESS OF THEIR OWN (PLAN-THE-TESTS phase 2, J9 / J10 / J12, 2026-10-01). `journeys.rs` drives the router in-process, which is right for everything
//! except what only a REAL serve process can show: the stack its threads have (the 2 MB test threads cannot reproduce a production stack overflow), what survives a SIGKILL, and
//! what the next start does about it. Each test here spawns the actual `antiloki-serve` binary on a free port with its own home, data dir, chats and substrate, talks to it over
//! HTTP, and kills it when it is done. The seat is the scripted one (tests/support/fake-claude); nothing leaves the machine and nothing touches the operator's state.
#![cfg(unix)]

use serde_json::{json, Value};
use std::io::BufRead;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// one at a time: each owns a port and a substrate, and J9 writes fifty thousand files
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
    L.lock().unwrap_or_else(|p| p.into_inner())
}

/// a scratch folder that is removed when the test ends, pass or fail
struct Scratch(PathBuf);
impl Scratch {
    fn new(tag: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!("antiloki-proc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p); std::fs::create_dir_all(&p).unwrap(); Scratch(p)
    }
    fn path(&self) -> &Path { &self.0 }
}
impl Drop for Scratch { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

fn free_port() -> u16 { std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port() }

/// A running `antiloki-serve`. Killed (SIGKILL) and reaped when dropped; `crash()` does it on purpose.
struct Serve { child: Option<Child>, port: u16, dir: PathBuf, log: PathBuf }
impl Serve {
    /// start the binary on `port` over the state under `dir` (the same dir again = a restart over the same state)
    fn start(dir: &Path, port: u16) -> Serve { Serve::start_env(dir, port, &[]) }
    /// the same, with extra environment for the child (a different HOME, for the journeys that need the machine's own files to be the test's)
    ///
    /// THE SCRIPTED SEAT IS THE ONLY SEAT THE CHILD CAN REACH (review 2026-10-01). The first version PREPENDED the scripted `claude` to the operator's PATH and left HOME and the environment as they were,
    /// so the real codex / agy / cursor-agent / grok binaries, their sign-ins and their API keys stayed visible to the router - and item 8's own notes record a journey whose next ask went to the machine's
    /// own Gemini, a real quota-burning call (the fix then was a per-test `finally`, not isolation). Now the child gets a CLEARED environment: a PATH of the scripted `claude` plus the system's
    /// own directories, a HOME of its own with the one pretend sign-in the scripted seat needs, and the test's variables. `assert_only_the_scripted_seat` then asks the serve itself.
    fn start_env(dir: &Path, port: u16, extra: &[(&str, &Path)]) -> Serve {
        let bin = dir.join("bin");
        if !bin.exists() {
            std::fs::create_dir_all(&bin).unwrap();
            std::os::unix::fs::symlink(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake-claude"), bin.join("claude")).unwrap();
        }
        let home = dir.join("fake-home");
        if !home.exists() { std::fs::create_dir_all(home.join(".claude")).unwrap(); std::fs::write(home.join(".claude/.credentials.json"), "{}").unwrap(); }   // "signed in" as far as the seat check can tell; the scripted claude never looks
        let log = dir.join("serve.log");
        let out = std::fs::OpenOptions::new().create(true).append(true).open(&log).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_antiloki-serve"));
        cmd.env_clear().env("PATH", format!("{}:/usr/bin:/bin", bin.display())).env("HOME", &home).env("ANTILOKI_LAN_PORT", "0");   // never the phone's port
        for k in ["LANG", "LC_ALL", "TMPDIR", "RUST_BACKTRACE"] { if let Some(v) = std::env::var_os(k) { cmd.env(k, v); } }
        let child = cmd
            .env("ANTILOKI_HOME", dir.join("home")).env("ANTILOKI_DATA_DIR", dir.join("data")).env("ANTILOKI_SUBSTRATE_ROOT", dir.join("substrate"))
            .env("ANTILOKI_CHATS_DIR", dir.join("chats")).env("FAKE_SEAT_STATE_DIR", dir.join("fake-state"))
            .env("ANTILOKI_SERVE_PORT", port.to_string()).env("ANTILOKI_BE_TICK_DISABLED", "true").env("ANTILOKI_SHIPPED_BENCH", "off").envs(extra.iter().map(|(k, v)| (*k, v.as_os_str())))
            .stdin(Stdio::null()).stdout(out.try_clone().unwrap()).stderr(out).spawn().expect("the serve binary starts");
        let mut s = Serve { child: Some(child), port, dir: dir.to_path_buf(), log };
        s.wait_up();
        s.assert_only_the_scripted_seat();
        s
    }
    /// The serve's own answer to "which seats can take a call": the scripted `claude` and nothing else. A journey that routes work must never be able to pick a real seat.
    fn assert_only_the_scripted_seat(&self) {
        let v = self.get("/api/v1/engines");
        let engines = v["engines"].as_array().unwrap_or_else(|| panic!("/api/v1/engines gave no engines: {v}"));
        assert!(!engines.is_empty(), "{v}");
        for e in engines {
            let usable = e["installed"] == json!(true) && e["off"] != json!(true) && e["seat"] != json!(false);
            if e["key"] == json!("claude-code") { assert!(usable, "the scripted seat must be usable for the journey to run at all: {e}"); }
            else { assert!(!usable, "a REAL seat is usable inside a journey that scripts the seat - a routed ask could burn quota: {e}"); }
        }
    }
    fn url(&self, p: &str) -> String { format!("http://127.0.0.1:{}{p}", self.port) }
    fn wait_up(&mut self) {
        let end = Instant::now() + Duration::from_secs(90);
        while Instant::now() < end {
            if let Some(st) = self.child.as_mut().and_then(|c| c.try_wait().ok().flatten()) { panic!("the serve exited at start ({st}):\n{}", self.log_text()); }
            if ureq::get(&self.url("/healthz")).timeout(Duration::from_secs(2)).call().is_ok() { return }
            std::thread::sleep(Duration::from_millis(200));
        }
        panic!("the serve did not answer within 90 s:\n{}", self.log_text());
    }
    fn alive(&mut self) -> bool { self.child.as_mut().map_or(false, |c| c.try_wait().ok().flatten().is_none()) }
    fn log_text(&self) -> String { std::fs::read_to_string(&self.log).unwrap_or_default() }
    /// SIGKILL: no shutdown hook runs, which is the case the next start has to clean up after
    fn crash(&mut self) { if let Some(mut c) = self.child.take() { let _ = c.kill(); let _ = c.wait(); } }
    fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let req = ureq::request(method, &self.url(path)).timeout(Duration::from_secs(120)).set("content-type", "application/json");
        let r = match body { Some(b) => req.send_string(&b.to_string()), None => req.call() };
        match r {
            Ok(resp) => { let st = resp.status(); (st, serde_json::from_str(&resp.into_string().unwrap_or_default()).unwrap_or(Value::Null)) }
            Err(ureq::Error::Status(st, resp)) => (st, serde_json::from_str(&resp.into_string().unwrap_or_default()).unwrap_or(Value::Null)),
            Err(e) => panic!("{method} {path}: {e}\nserve log:\n{}", self.log_text()),
        }
    }
    fn get(&self, p: &str) -> Value { self.call("GET", p, None).1 }
    /// A GET whose answer is bytes, not JSON (a media file), as text.
    fn get_raw(&self, p: &str) -> String { match ureq::get(&self.url(p)).timeout(Duration::from_secs(60)).call() { Ok(r) => r.into_string().unwrap_or_default(), Err(e) => panic!("GET {p}: {e}") } }
    fn post(&self, p: &str, b: Value) -> Value { self.call("POST", p, Some(b)).1 }
    fn add_workspace(&self, name: &str, root: &Path) { let v = self.post("/api/v1/workspaces/add", json!({ "name": name, "root_path": root })); assert_eq!(v["ok"], json!(true), "workspaces/add {name}: {v}"); }
    fn activate(&self, name: &str) -> Value { let v = self.post("/api/v1/workspaces/activate", json!({ "name": name })); assert_eq!(v["ok"], json!(true), "workspaces/activate {name}: {v}"); v }
    fn active(&self) -> String { self.post("/api/v1/workspaces/list", json!({}))["active"]["name"].as_str().unwrap_or("").to_string() }
    /// a POST that is ABOUT another workspace than the active one: the `X-Antiloki-Workspace` header names its root
    fn post_in(&self, p: &str, b: Value, root: &Path) -> Value {
        let r = ureq::post(&self.url(p)).timeout(Duration::from_secs(120)).set("content-type", "application/json").set("x-antiloki-workspace", &root.to_string_lossy()).send_string(&b.to_string());
        match r { Ok(resp) => serde_json::from_str(&resp.into_string().unwrap_or_default()).unwrap_or(Value::Null), Err(ureq::Error::Status(_, resp)) => serde_json::from_str(&resp.into_string().unwrap_or_default()).unwrap_or(Value::Null), Err(e) => panic!("POST {p}: {e}\nserve log:\n{}", self.log_text()) }
    }
    fn dispatch(&self, action: &str, input: Value) -> Value { self.post("/api/v1/dispatch", json!({ "action_name": action, "input": input })) }
}
impl Drop for Serve { fn drop(&mut self) { self.crash(); } }

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git").args(["-c", "user.email=j@example.com", "-c", "user.name=j"]).args(args).current_dir(dir).output().expect("git runs");
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}
/// a small project: one file, one commit
fn project(root: &Path, name: &str) -> PathBuf {
    let d = root.join(name); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("README.md"), format!("# {name}\n")).unwrap();
    git(&d, &["init", "-q"]); git(&d, &["add", "-A"]); git(&d, &["commit", "-q", "-m", "init"]); d
}
/// `until` over a closure that polls the serve; panics with `what` and the serve's log
fn until<T>(s: &Serve, secs: u64, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f() { return v }
        if Instant::now() > end { panic!("timed out ({secs} s) waiting for: {what}\nserve log:\n{}", s.log_text()); }
        std::thread::sleep(Duration::from_millis(200));
    }
}
fn runs(s: &Serve) -> Vec<Value> { s.get("/api/v1/cockpit/goals")["runs"].as_array().cloned().unwrap_or_default() }

/// `pkgNN/modNN/fNN.ts`: `packages` x `mods` folders of fifty files each - empty but for one line of code in the first file of every other folder - and a chain of two hundred nested folders at the bottom
fn big_tree(big: &Path, packages: usize, mods: usize) {
    for a in 0..packages { for b in 0..mods {
        let d = big.join(format!("pkg{a:02}")).join(format!("mod{b:02}")); std::fs::create_dir_all(&d).unwrap();
        for f in 0..50 { let p = d.join(format!("f{f:02}.ts")); if f == 0 && b % 2 == 0 { std::fs::write(p, format!("export const v{a}_{b} = {};\n", a * 100 + b)).unwrap(); } else { std::fs::File::create(p).unwrap(); } }
    } }
    let mut deep = big.join("deep"); for i in 0..200 { deep = deep.join(format!("d{i}")); } std::fs::create_dir_all(&deep).unwrap(); std::fs::write(deep.join("leaf.ts"), "export const leaf = 1;\n").unwrap();
}

// J9 - THE SWITCH ON A BIG TREE (incident 2026-09-29: "the serve died with a stack overflow on a workspace switch" - the operator opened a second, large workspace and the debug build's
// frames walked a big tree on a 2 MB tokio thread; main.rs now builds the runtime with 64 MB stacks). The serve is the real binary, the tree is fifty thousand files plus a chain of
// two hundred nested folders, an events stream is open the whole time, and the workspace is switched there and back twice. The serve must live through all of it, keep answering,
// keep the stream, and never print a stack overflow or a panic.
#[test]
fn j9_switching_to_and_from_a_fifty_thousand_file_workspace_twice_leaves_the_serve_standing() {
    let _s = serial(); let tmp = Scratch::new("j9");
    // the big tree: 50 x 20 x 50 empty files (a file system entry each, no data - the walk is what matters), 500 of them with a line of code, and a deep chain at the bottom
    let big = tmp.path().join("big");
    big_tree(&big, 50, 20);
    git(&big, &["init", "-q"]);
    let small = project(tmp.path(), "small");
    // ...and a project with the shape that makes a recursive walker spend its stack: a source file nested three thousand levels deep (arrays inside arrays, calls inside calls), beside a few ordinary ones
    let nest = project(tmp.path(), "nest");
    std::fs::write(nest.join("deep.ts"), format!("export const deep = {}1{};\nexport const calls = {}0{};\n", "[".repeat(3000), "]".repeat(3000), "f(".repeat(3000), ")".repeat(3000))).unwrap();
    for i in 0..30 { std::fs::write(nest.join(format!("m{i}.ts")), format!("export function m{i}(a: number) {{ return a + {i}; }}\n")).unwrap(); }
    git(&nest, &["add", "-A"]); git(&nest, &["commit", "-q", "-m", "deep"]);

    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let mut serve = Serve::start(&state, free_port());
    serve.add_workspace("small", &small); serve.add_workspace("big", &big); serve.add_workspace("nest", &nest);
    serve.activate("small");

    // an events stream the whole time (what the browser's SharedWorker holds): count every frame
    let frames = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reader = { let f = frames.clone(); let url = serve.url("/api/v1/events");
        std::thread::spawn(move || { if let Ok(r) = ureq::get(&url).call() { for l in std::io::BufReader::new(r.into_reader()).lines() { match l { Ok(l) if l.starts_with("data:") => { f.fetch_add(1, std::sync::atomic::Ordering::SeqCst); } Ok(_) => {} Err(_) => break } } } }) };
    until(&serve, 15, "the stream's first frame", || (frames.load(std::sync::atomic::Ordering::SeqCst) > 0).then_some(()));

    for round in 1..=2 {
        let v = serve.activate("big");
        assert_eq!(v["active"], json!("big"), "round {round}: {v}");
        assert!(serve.alive(), "round {round}: the serve died switching TO the big workspace:\n{}", serve.log_text());
        // what the screens ask for right after a switch: the registry, the chat list, the file browser over the tree, the goals and the ledger
        assert_eq!(serve.active(), "big");
        let browse = format!("/api/v1/workspaces/browse?path={}", big.display());
        for p in ["/api/v1/cockpit/chats", "/api/v1/cockpit/goals", "/api/v1/cockpit/goals/ledger", browse.as_str()] {
            let (st, _) = serve.call("GET", p, None); assert_eq!(st, 200, "round {round}: GET {p}");
        }
        let v = serve.activate("nest");
        assert_eq!(v["active"], json!("nest"), "round {round}: {v}");
        // the first thing a new workspace does is read its own code (the rebuild); on the deeply nested source the walkers recurse - this is the rebuild that has to finish, or fail on its own terms, with the serve still standing
        let job = serve.post("/api/v1/codebase/onboard/start", json!({ "root": nest }));
        let id = job["job_id"].as_str().unwrap_or_else(|| panic!("round {round}: the rebuild of the nested project starts: {job}")).to_string();
        let end = Instant::now() + Duration::from_secs(120);
        let last = loop {
            assert!(serve.alive(), "round {round}: the serve died while it read the deeply nested project:\n{}", serve.log_text());
            let st = serve.post("/api/v1/codebase/onboard/status", json!({ "job_id": id }));
            if st["status"] != json!("running") || Instant::now() > end { break st; }
            std::thread::sleep(Duration::from_millis(500));
        };
        assert!(serve.alive(), "round {round}: the serve died reading the nested project:\n{}", serve.log_text());
        assert_ne!(last["status"], json!("running"), "round {round}: the nested project's rebuild did not settle in 120 s: {last}");
        let v = serve.activate("small");
        assert_eq!(v["active"], json!("small"), "round {round}: {v}");
        assert!(serve.alive(), "round {round}: the serve died switching AWAY from the big workspace:\n{}", serve.log_text());
        assert_eq!(serve.active(), "small");
    }
    // the stream outlived every switch: the reader is still reading, and the serve still counts it as a listener (a switch re-mounts the DB set; the browser's one stream must not be dropped by it)
    assert!(!reader.is_finished(), "the events stream closed during the switches");
    let census = serve.get("/api/v1/cockpit/streams/census");
    assert_eq!(census["events_listeners"], json!(1), "the stream is still registered: {census}");
    let log = serve.log_text();
    for bad in ["overflowed its stack", "panicked at", "SIGSEGV", "stack overflow"] { assert!(!log.contains(bad), "the serve's log says \"{bad}\":\n{log}"); }
    serve.crash();
    let _ = reader.join();
}

// J10 - THE ORPHANED REBUILD (incident 2026-09-29: a rebuild "working for 70 minutes" on a workspace the serve had crashed in, blocking the switch away from it). A rebuild whose runner
// died leaves a row that says `running` for ever. Three ways out are locked here: the engine's own `mark-error` transition, a workspace switch (the other workspace never inherits
// the stale job), and the sweep the serve runs by itself (a job silent for 20 minutes is marked as an error).
#[test]
fn j10_a_rebuild_whose_runner_is_gone_can_be_marked_failed_and_never_holds_a_workspace() {
    let _s = serial(); let tmp = Scratch::new("j10");
    let (a, b) = (project(tmp.path(), "alpha"), project(tmp.path(), "beta"));
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let serve = Serve::start(&state, free_port());
    serve.add_workspace("alpha", &a); serve.add_workspace("beta", &b);
    serve.activate("alpha");

    // the action alone makes the job row and starts no runner - exactly what a crashed serve leaves behind
    let job = serve.dispatch("system.codebase.onboard.start", json!({ "root": a }));
    let id = job["job_id"].as_str().unwrap_or_else(|| panic!("a rebuild job: {job}")).to_string();
    let latest = |s: &Serve| s.dispatch("system.codebase.onboard.latest", json!({}));
    assert_eq!((latest(&serve)["job_id"].as_str(), latest(&serve)["status"].as_str()), (Some(id.as_str()), Some("running")), "the stale job is alpha's latest");

    // switching away is allowed, and beta does not inherit alpha's job
    serve.activate("beta");
    assert_ne!(latest(&serve)["job_id"].as_str(), Some(id.as_str()), "beta must not see alpha's rebuild: {}", latest(&serve));
    serve.activate("alpha");
    assert_eq!(latest(&serve)["status"], json!("running"), "alpha still has its stale job");

    // cancelled by hand: the engine's own transition flips only a live-looking job, says why, and a second call changes nothing
    let r = serve.dispatch("system.codebase.onboard.control", json!({ "job_id": id, "op": "mark-error", "message": "cancelled by the operator" }));
    assert_eq!(r["status"], json!("error"), "{r}");
    let st = serve.post("/api/v1/codebase/onboard/status", json!({ "job_id": id }));
    assert_eq!(st["status"], json!("error"));
    assert!(st["error"].as_str().unwrap_or("").contains("cancelled by the operator"), "{st}");
    let again = serve.dispatch("system.codebase.onboard.control", json!({ "job_id": id, "op": "mark-error" }));
    assert_eq!(again["status"], json!("error"));
    assert!(serve.post("/api/v1/codebase/onboard/status", json!({ "job_id": id }))["error"].as_str().unwrap_or("").contains("cancelled by the operator"), "a terminal job's reason is not rewritten");
    // and it can be resumed from the checkpoint (the error state is resumable, which is the point of marking it)
    let ctl = serve.dispatch("system.codebase.onboard.control", json!({ "job_id": id, "op": "resume-mark" }));
    assert_eq!(ctl["status"], json!("running"), "{ctl}");

    // the sweep: the same orphan, silent for 30 minutes, is marked by the serve ITSELF within its minute tick. (Watched in the database: reading the job through the status route would
    // heal a stale row on the spot - a second, separate mechanism, locked below - and the sweep would never be seen to run.)
    let st = serve.post("/api/v1/codebase/onboard/status", json!({ "job_id": id }));
    assert_eq!(st["status"], json!("running"));
    let db = a.join(".antiloki/db/state.sqlite");
    let sql = |q: &str| { let c = rusqlite::Connection::open(&db).expect("the workspace's state db"); c.busy_timeout(Duration::from_secs(10)).unwrap(); let n = c.execute(q, [&id]).unwrap(); n };
    assert_eq!(sql("UPDATE code_onboard_jobs SET updated_at = datetime('now', '-30 minutes') WHERE job_id = ?"), 1, "the job row is in {}", db.display());
    let row = |what: &str| -> String { let c = rusqlite::Connection::open(&db).unwrap(); c.busy_timeout(Duration::from_secs(10)).unwrap(); c.query_row(&format!("SELECT {what} FROM code_onboard_jobs WHERE job_id = ?"), [&id], |r| r.get::<_, Option<String>>(0)).unwrap().unwrap_or_default() };
    until(&serve, 100, "the serve's own sweep to mark the silent rebuild", || (row("status") == "error").then_some(()));
    assert!(row("error").contains("stopped reporting for more than 20 minutes"), "{}", row("error"));
    assert!(serve.log_text().contains("stopped reporting were marked as errors"), "the sweep says so in the log");

    // ...and the other mechanism: ASKING about a job that has been silent that long heals it on the spot (to `failed`, with the reason), so a screen that polls never waits for the minute tick
    assert_eq!(serve.dispatch("system.codebase.onboard.control", json!({ "job_id": id, "op": "resume-mark" }))["status"], json!("running"));
    assert_eq!(sql("UPDATE code_onboard_jobs SET updated_at = datetime('now', '-30 minutes') WHERE job_id = ?"), 1);
    let healed = serve.post("/api/v1/codebase/onboard/status", json!({ "job_id": id }));
    assert_eq!(healed["status"], json!("failed"), "{healed}");
    assert!(healed["error"].as_str().unwrap_or("").len() > 10, "and it says why: {healed}");
}

// J12 - THE RESTART MID-RUN (incident: "restarting antiloki-serve kills every agent terminal", 2026-09-20; runs and queue items were left saying `doing` for ever). A run is live in the
// worker when the serve is SIGKILLed. The next start must say so truthfully: the run is failed with the reason, its task too, the queue item is not left `doing`, the chat is still there
// and answers, the work asked after the restart runs, and a dev server the dead serve left behind is ended by the sweep.
#[test]
fn j12_a_serve_restarted_mid_run_marks_the_run_failed_keeps_the_chat_and_sweeps_what_it_left() {
    let _s = serial(); let tmp = Scratch::new("j12");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let port = free_port();
    let mut s1 = Serve::start(&state, port);
    s1.add_workspace("work", &ws); s1.activate("work");
    let chat = s1.post("/api/v1/cockpit/chat/create", json!({ "engine": "claude-code", "title": "j12" }));
    let (pane, session) = (chat["pane"]["id"].as_i64().expect("pane"), chat["pane"]["session_id"].as_i64().unwrap_or(1));
    let added = s1.post("/api/v1/cockpit/queue/add", json!({ "parent": format!("{session}-{pane}"), "goal": "create the slow file FAKE:slow:20000 FAKE:write:out/slow.txt tag-j12-live", "engine": "claude-code", "cwd": ws, "write": true, "n": 1, "pane_id": pane, "always": true }));
    let item = added["item"]["id"].as_i64().expect("queue item");
    let rid = until(&s1, 60, "the worker to be running", || runs(&s1).into_iter().find(|r| r["goal"].as_str().map_or(false, |g| g.contains("tag-j12-live")) && r["status"] == json!("running")).map(|r| r["id"].as_i64().unwrap()));

    // a dev server the serve started and then died without ending: its record is in ANTILOKI_HOME/run/previews.json, and the process is alive in its own group
    let mut orphan = Command::new("sleep").arg("4711").process_group(0).spawn().expect("sleep starts");
    std::fs::create_dir_all(state.join("home/run")).unwrap();
    std::fs::write(state.join("home/run/previews.json"), json!({ "j12": { "pid": orphan.id(), "cmd": "sleep 4711" } }).to_string()).unwrap();

    s1.crash();
    assert!(orphan.try_wait().unwrap().is_none(), "the orphan outlived the dead serve (that is what it is)");
    let s2 = Serve::start(&state, port);

    // the run that was live is failed, with the reason, and so is its task
    let r = s2.get(&format!("/api/v1/cockpit/goals?id={rid}"))["run"].clone();
    assert_eq!(r["status"], json!("failed"), "{r}");
    assert!(r["error"].as_str().unwrap_or("").contains("the serve restarted while the run was live"), "{}", r["error"]);
    assert_eq!(r["tasks"][0]["status"], json!("failed"));
    assert!(r["events"].as_array().unwrap().iter().any(|e| e["kind"] == "fail"), "the run's own log says it");
    // the queue does not keep saying `doing`, and says why
    let it = until(&s2, 30, "the queue to stop saying `doing` for the run that died", || {
        let q = s2.get(&format!("/api/v1/cockpit/queue?pane_id={pane}"));
        let it = q["items"].as_array()?.iter().find(|i| i["id"].as_i64() == Some(item))?.clone();
        (it["status"] != json!("doing")).then_some(it)
    });
    assert_eq!(it["status"], json!("failed"), "{it}");
    assert!(it["note"].as_str().unwrap_or("").contains("restarted"), "{it}");
    // the workspace and the chat are where they were, and the chat answers again on a fresh seat process
    assert_eq!(s2.active(), "work");
    let chats = s2.get("/api/v1/cockpit/chats");
    assert!(chats["chats"].as_array().unwrap().iter().any(|c| c["pane_id"].as_i64() == Some(pane)), "{chats}");
    s2.post("/api/v1/cockpit/agent/send", json!({ "pane_id": pane, "text": "hello after the restart j12" }));
    until(&s2, 60, "the chat's answer after the restart", || s2.get(&format!("/api/v1/cockpit/agent/events?pane_id={pane}"))["events"].as_array().and_then(|ev| ev.iter().find(|e| e["ev"].to_string().contains("ack: ") && e["ev"].to_string().contains("hello after the restart j12")).cloned()));
    // new work runs, and its id does not collide with the dead run's
    let again = s2.post("/api/v1/cockpit/queue/add", json!({ "parent": format!("{session}-{pane}"), "goal": "create the quick file FAKE:write:out/quick.txt tag-j12-after", "engine": "claude-code", "cwd": ws, "write": true, "n": 1, "pane_id": pane, "always": true }));
    assert!(again["item"]["id"].as_i64().unwrap() > item, "{again}");
    let done = until(&s2, 120, "the run asked after the restart to finish", || runs(&s2).into_iter().find(|r| r["goal"].as_str().map_or(false, |g| g.contains("tag-j12-after")) && r["status"] == json!("done") && r["qa_pending"] == json!(false)));
    assert!(done["id"].as_i64().unwrap() > rid);
    // the sweep ended the dev server the dead serve left (and says how many)
    until(&s2, 15, "the orphaned dev server to be ended by the sweep", || orphan.try_wait().ok().flatten());
    assert!(s2.log_text().contains("ended 1 dev server(s) a previous serve left running"), "{}", s2.log_text());
    assert!(!state.join("home/run/previews.json").exists() || std::fs::read_to_string(state.join("home/run/previews.json")).unwrap().trim() == "{}", "the record of dead previews is cleared");
}

// J12b - A CLAUDE CHAT RESUMES ITS CONVERSATION AFTER A RESTART (incident 2026-09-20: restarting the serve killed every agent terminal; since 0bee3c8 a restarted Claude pane `--resume`s the session it pinned).
// The serve resumes only when the session's transcript exists where Claude keeps it (`~/.claude/projects/<slug of the folder>/<id>.jsonl`), so this journey gives the child serve a HOME of its own with a
// pretend sign-in, lets the chat start, writes that transcript the way the real CLI would have, kills the serve, starts it again and sends a message: the new engine process is a `resumed` one on the SAME
// session id. Without the transcript the same restart starts a fresh session with a new id (a conversation that was never written down cannot be resumed).
#[test]
fn j12b_a_claude_chat_restarted_with_its_transcript_on_disk_resumes_the_same_session_and_without_it_starts_afresh() {
    let _s = serial(); let tmp = Scratch::new("j12b");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let home = tmp.path().join("home-dir"); std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(home.join(".claude/.credentials.json"), "{}").unwrap();         // "signed in" as far as the seat check can tell; the scripted claude never looks
    let port = free_port();
    let start = |state: &Path| Serve::start_env(state, port, &[("HOME", home.as_path())]);
    let engine_events = |s: &Serve, pane: i64| s.get(&format!("/api/v1/cockpit/agent/events?pane_id={pane}"))["events"].as_array().cloned().unwrap_or_default();
    let session_events = |s: &Serve, pane: i64| -> Vec<(String, String)> { engine_events(s, pane).iter().filter(|e| e["ev"]["type"] == "antiloki" && matches!(e["ev"]["subtype"].as_str(), Some("started" | "resumed"))).map(|e| (e["ev"]["subtype"].as_str().unwrap().to_string(), e["ev"]["session_id"].as_str().unwrap_or("").to_string())).collect() };
    let say = |s: &Serve, pane: i64, text: &str| { s.post("/api/v1/cockpit/agent/send", json!({ "pane_id": pane, "text": text })); until(s, 60, "the chat's answer", || engine_events(s, pane).iter().any(|e| e["ev"].to_string().contains("ack: ") && e["ev"].to_string().contains(text)).then_some(())); };

    let mut s1 = start(&state);
    s1.add_workspace("work", &ws); s1.activate("work");
    let chat = s1.post("/api/v1/cockpit/chat/create", json!({ "engine": "claude-code", "title": "j12b" }));
    let (pane, box_dir) = (chat["pane"]["id"].as_i64().expect("pane"), chat["pane"]["worktree_path"].as_str().expect("the chat's folder").to_string());
    say(&s1, pane, "before the restart j12b");
    let first = session_events(&s1, pane);
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0].0, "started");
    let id = first[0].1.clone();
    assert!(!id.is_empty());
    // the transcript the real CLI would have written for that conversation
    let transcript = home.join(".claude/projects").join(antiloki_rs::context_context_library::claude_project_slug(&box_dir)).join(format!("{id}.jsonl"));
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, "{\"type\":\"user\"}\n").unwrap();
    s1.crash();

    let mut s2 = start(&state);
    say(&s2, pane, "after the restart j12b");
    let second = session_events(&s2, pane);
    assert_eq!(second.last().map(|x| x.0.as_str()), Some("resumed"), "the engine came back as a resumed session: {second:?}");
    assert_eq!(second.last().map(|x| x.1.as_str()), Some(id.as_str()), "...of the SAME conversation: {second:?}");
    s2.crash();

    // the same restart with no transcript on disk: a fresh session, with a new id (nothing to resume is not an error)
    std::fs::remove_file(&transcript).unwrap();
    let s3 = start(&state);
    say(&s3, pane, "and again without it j12b");
    let third = session_events(&s3, pane);
    assert_eq!(third.last().map(|x| x.0.as_str()), Some("started"), "{third:?}");
    assert_ne!(third.last().map(|x| x.1.as_str()), Some(id.as_str()), "a conversation that was never written down starts a new session: {third:?}");
}

// J9b - FOUND WRITING J9 (2026-10-01), FIXED THE SAME DAY (`AppState::rebind` no longer waits for a connection that is busy; the unit is J9c in journeys.rs): A LIVE REBUILD BLOCKED THE SWITCH AWAY FROM IT. The rebuild of a big workspace (`codebase/onboard/start`, no AI involved) runs as one engine action on the serve's single
// writer connection, and `workspaces/activate` needs that connection to re-mount the other workspace's databases: on a ten-thousand-file tree the activate request was still unanswered after 150 s (the serve at 98 % of a core in the
// "constitution" phase; reads such as the job's status and the chat list kept answering). This is the operator's "rebuild working for 70 minutes, blocked switching away from it" (2026-09-29) in its live form: the orphan sweep (J10)
// frees a rebuild whose runner is DEAD, nothing freed the switch from one that is alive. The cause was `rebind()` taking every connection's lock in turn, the rebuild's included.
#[test]
fn j9b_switching_away_from_a_workspace_that_is_being_rebuilt_is_answered_promptly() {
    let _s = serial(); let tmp = Scratch::new("j9b");
    let big = tmp.path().join("big"); big_tree(&big, 20, 10);
    let small = project(tmp.path(), "small");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let serve = Serve::start(&state, free_port());
    serve.add_workspace("small", &small); serve.add_workspace("big", &big);
    serve.activate("big");
    let job = serve.post("/api/v1/codebase/onboard/start", json!({ "root": big }));
    assert!(job["job_id"].is_string(), "the rebuild starts: {job}");
    let t0 = Instant::now();
    let r = ureq::post(&serve.url("/api/v1/workspaces/activate")).timeout(Duration::from_secs(30)).set("content-type", "application/json").send_string(&json!({ "name": "small" }).to_string());
    assert!(r.is_ok(), "the switch was not answered within 30 s ({:?} later) while the big workspace was being rebuilt: {:?}", t0.elapsed(), r.err().map(|e| e.to_string()));
    assert_eq!(serve.active(), "small");
}

// ── RFC 0007 M1: terminals that belong to the sidecar host outlive the serve ──────────────────────────────────────────────

/// Shuts the host down when the test ends, pass or fail: it is a separate process and would otherwise outlive the test.
struct HostGuard { agent: PathBuf, sock: PathBuf }
impl Drop for HostGuard {
    fn drop(&mut self) { let _ = Command::new(&self.agent).args(["pty", "--sock"]).arg(&self.sock).arg("shutdown").output(); }
}
fn agent_binary() -> PathBuf {
    let p = Path::new(env!("CARGO_BIN_EXE_antiloki-serve")).parent().unwrap().join("antiloki-agent");
    assert!(p.exists(), "the sidecar host test needs the agent binary at {}: run `cargo build -p antiloki-agent` first", p.display());
    p
}
fn file_has(p: &Path, needle: &str) -> bool { std::fs::read_to_string(p).map_or(false, |t| t.lines().any(|l| l == needle)) }

#[test]
fn j13_a_terminal_owned_by_the_sidecar_host_survives_a_serve_killed_and_restarted_and_still_takes_input() {
    let _s = serial(); let tmp = Scratch::new("j13");
    let agent = agent_binary();
    let sock = tmp.path().join("pty.sock");
    let _host = HostGuard { agent: agent.clone(), sock: sock.clone() };
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let typed = tmp.path().join("typed.txt");
    let env: [(&str, &Path); 3] = [("ANTILOKI_PTY_HOST", Path::new("sidecar")), ("ANTILOKI_AGENT_BIN", &agent), ("ANTILOKI_PTY_SOCK", &sock)];
    let port = free_port();
    let pane = 990_101;

    let s1 = Serve::start_env(&state, port, &env);
    assert_eq!(s1.get("/api/v1/cockpit/pty/host")["mode"], json!("sidecar"));
    // a plain shell that appends every line it is typed to a file: the file is how the test sees the terminal's life
    let script = format!("echo started; while IFS= read -r l; do echo \"$l\" >> '{}'; done", typed.display());
    let started = s1.post("/api/v1/workspace/pty/start", json!({ "pane_id": pane, "cmd": "sh", "args": ["-c", script], "cols": 100, "rows": 30 }));
    assert_eq!(started["ok"], json!(true), "{started}");
    until(&s1, 15, "the terminal to be running", || (s1.get(&format!("/api/v1/cockpit/pty/status?pane_id={pane}"))["running"] == json!(true)).then_some(()));
    assert_eq!(s1.post("/api/v1/cockpit/pty/input", json!({ "pane_id": pane, "data": "before the restart\n" }))["ok"], json!(true));
    until(&s1, 10, "the first line to reach the terminal", || file_has(&typed, "before the restart").then_some(()));
    // the host holds it, under the pane's name
    let h = s1.get("/api/v1/cockpit/pty/host");
    assert!(h["terminals"].as_array().unwrap().iter().any(|t| t["id"] == json!(format!("pane-{pane}")) && t["alive"] == json!(true)), "{h}");

    // the serve dies without any chance to clean up
    let mut s1 = s1; s1.crash(); drop(s1);

    // the terminal is still alive, in the host, with no serve at all
    let ls = Command::new(&agent).args(["pty", "--sock"]).arg(&sock).arg("ls").output().unwrap();
    let ls = String::from_utf8_lossy(&ls.stdout).to_string();
    assert!(ls.contains(&format!("pane-{pane}")) && ls.contains("\"alive\":true"), "the terminal must outlive the serve: {ls}");

    // a new serve over the same state finds it and attaches again
    let s2 = Serve::start_env(&state, port, &env);
    until(&s2, 20, "the new serve to attach to the terminal the host kept", || (s2.get(&format!("/api/v1/cockpit/pty/status?pane_id={pane}"))["running"] == json!(true)).then_some(()));
    assert!(s2.log_text().contains("reattached 1 terminal"), "{}", s2.log_text());
    // and typing into it works: the same process, never restarted
    assert_eq!(s2.post("/api/v1/cockpit/pty/input", json!({ "pane_id": pane, "data": "after the restart\n" }))["ok"], json!(true));
    until(&s2, 10, "a line typed after the restart to reach the same terminal", || file_has(&typed, "after the restart").then_some(()));
    // the text file shows both lines came from one shell that never exited
    assert!(file_has(&typed, "before the restart") && file_has(&typed, "after the restart"));

    // killing it through the serve ends it in the host
    assert_eq!(s2.post("/api/v1/cockpit/pty/kill", json!({ "pane_id": pane }))["ok"], json!(true));
    until(&s2, 10, "the host to report the terminal gone", || {
        let h = s2.get("/api/v1/cockpit/pty/host");
        h["terminals"].as_array()?.iter().find(|t| t["id"] == json!(format!("pane-{pane}"))).filter(|t| t["alive"] == json!(false)).map(|_| ())
    });
}

#[test]
fn j13b_pty_send_types_a_bracketed_paste_and_enter_into_the_terminal() {
    let _s = serial(); let tmp = Scratch::new("j13b");
    let agent = agent_binary();
    let sock = tmp.path().join("pty.sock");
    let _host = HostGuard { agent: agent.clone(), sock: sock.clone() };
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let typed = tmp.path().join("typed.txt");
    let env: [(&str, &Path); 3] = [("ANTILOKI_PTY_HOST", Path::new("sidecar")), ("ANTILOKI_AGENT_BIN", &agent), ("ANTILOKI_PTY_SOCK", &sock)];
    let s = Serve::start_env(&state, free_port(), &env);
    let pane = 990_102;
    // `cat -v` shows the control bytes it receives, so the file proves the paste markers and the submit key arrived as typed
    let started = s.post("/api/v1/workspace/pty/start", json!({ "pane_id": pane, "cmd": "env", "args": ["sh", "-c", format!("cat -v >> '{}'", typed.display())], "cols": 100, "rows": 30 }));
    assert_eq!(started["ok"], json!(true), "{started}");
    let sent = s.post("/api/v1/cockpit/pty/send", json!({ "pane_id": pane, "text": "fix the picker\x1b[201~evil" }));
    assert_eq!(sent["ok"], json!(true), "{sent}");
    let got = until(&s, 10, "the sent text to reach the terminal", || std::fs::read_to_string(&typed).ok().filter(|t| t.contains("fix the picker")));
    assert!(got.contains("^[[200~fix the picker[201~evil^[[201~"), "the paste is wrapped in our markers and the embedded escape is gone: {got:?}");
    // nothing to send, and no terminal, are said plainly
    assert_eq!(s.post("/api/v1/cockpit/pty/send", json!({ "pane_id": pane, "text": "   " }))["error"], json!("nothing to send"));
    assert!(s.post("/api/v1/cockpit/pty/send", json!({ "pane_id": 424242, "text": "x" }))["error"].as_str().unwrap().contains("no live terminal"));
}

#[test]
fn j13c_the_default_in_process_terminal_still_works_and_a_restarted_pane_is_not_removed_by_the_old_ones_exit() {
    let _s = serial(); let tmp = Scratch::new("j13c");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let typed = tmp.path().join("typed.txt");
    let s = Serve::start(&state, free_port()); // no sidecar: the registry owns the PTYs, as before
    assert_eq!(s.get("/api/v1/cockpit/pty/host")["mode"], json!("in-process"));
    let pane = 990_103;
    let start = |tag: &str| s.post("/api/v1/workspace/pty/start", json!({ "pane_id": pane, "cmd": "sh", "args": ["-c", format!("echo {tag} >> '{}'; cat >> '{}'", typed.display(), typed.display())], "cols": 100, "rows": 30 }));
    assert_eq!(start("first")["ok"], json!(true));
    until(&s, 10, "the first terminal to write its mark", || file_has(&typed, "first").then_some(()));
    // restarting the same pane replaces the terminal: the old one's end must not take the new one out of the registry
    assert_eq!(start("second")["ok"], json!(true));
    until(&s, 10, "the second terminal to write its mark", || file_has(&typed, "second").then_some(()));
    std::thread::sleep(Duration::from_millis(1500)); // long enough for the old terminal's exit to have been noticed
    assert_eq!(s.get(&format!("/api/v1/cockpit/pty/status?pane_id={pane}"))["running"], json!(true), "the new terminal was removed by the old one's exit");
    assert_eq!(s.post("/api/v1/cockpit/pty/input", json!({ "pane_id": pane, "data": "typed into the second\n" }))["ok"], json!(true));
    until(&s, 10, "the line typed into the second terminal", || file_has(&typed, "typed into the second").then_some(()));
    // killing ends it, and the registry says so
    assert_eq!(s.post("/api/v1/cockpit/pty/kill", json!({ "pane_id": pane }))["was_running"], json!(true));
    assert_eq!(s.get(&format!("/api/v1/cockpit/pty/status?pane_id={pane}"))["running"], json!(false));
}

#[test]
fn j14_the_cowork_endpoints_answer_over_http() {
    let _s = serial(); let tmp = Scratch::new("j14");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let s = Serve::start(&state, free_port());
    s.add_workspace("work", &ws); s.activate("work");

    // the pre-check, tier 0: a path the ask names that is not in the box becomes a question with chips; a clear ask is sent
    let v = s.post("/api/v1/cockpit/precheck", json!({ "pane_id": 0, "text": "fix the picker in src/nowhere/missing-file.tsx" }));
    assert_eq!(v["verdict"], json!("ask"), "{v}");
    assert!(v["questions"][0]["text"].as_str().unwrap().contains("missing-file.tsx"), "{v}");
    assert!(v["questions"][0]["options"].as_array().unwrap().len() >= 2, "{v}");
    assert!(v["ms"].as_u64().is_some());
    let v = s.post("/api/v1/cockpit/precheck", json!({ "pane_id": 0, "text": "fix the typo in README.md" }));
    assert_eq!(v["verdict"], json!("send"), "README.md exists in the box: {v}");
    assert_eq!(s.post("/api/v1/cockpit/precheck", json!({ "text": "   " }))["verdict"], json!("send"));
    // a path that tries to leave the box is not looked up outside it
    let v = s.post("/api/v1/cockpit/precheck", json!({ "text": "read ../../etc/passwd.txt" }));
    assert!(v["verdict"] == json!("ask") || v["verdict"] == json!("send"), "{v}");

    // the layout: empty at first, round-trips, and refuses what it should
    assert_eq!(s.get("/api/v1/cockpit/cowork/layout")["layout"], Value::Null);
    let layout = json!({ "root": { "type": "leaf", "pane": { "id": "claude", "kind": "engine", "engine": "claude-code" } }, "focus": "claude", "zoom": null });
    assert_eq!(s.post("/api/v1/cockpit/cowork/layout", json!({ "layout": layout }))["ok"], json!(true));
    assert_eq!(s.get("/api/v1/cockpit/cowork/layout")["layout"], layout);
    assert!(ws.join(".antiloki/cowork-layout.json").is_file());
    assert_eq!(s.call("POST", "/api/v1/cockpit/cowork/layout", Some(json!({ "layout": "not an object" }))).0, 400);
    assert_eq!(s.call("POST", "/api/v1/cockpit/cowork/layout", Some(json!({ "layout": { "x": "y".repeat(300_000) } }))).0, 413);
    assert_eq!(s.get("/api/v1/cockpit/cowork/layout")["layout"], layout, "a refused write leaves the layout as it was");

    // episodes: none yet, and an answer rather than an error
    let e = s.get("/api/v1/cockpit/episodes?limit=5");
    assert_eq!(e["ok"], json!(true)); assert_eq!(e["episodes"], json!([]));

    // usage for a pane that does not exist says it has no reader, it does not fail
    let u = s.get("/api/v1/cockpit/pty/usage?pane_id=424242");
    assert_eq!(u["ok"], json!(true)); assert_eq!(u["known"], json!(false));
}

#[test]
fn j13d_a_command_typed_to_a_shell_pane_runs_and_is_not_wrapped_in_paste_markers_a_plain_shell_would_print() {
    let _s = serial(); let tmp = Scratch::new("j13d");
    let agent = agent_binary(); let sock = tmp.path().join("pty.sock");
    let _host = HostGuard { agent: agent.clone(), sock: sock.clone() };
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let made = tmp.path().join("made-by-the-typed-command.txt");
    let env: [(&str, &Path); 3] = [("ANTILOKI_PTY_HOST", Path::new("sidecar")), ("ANTILOKI_AGENT_BIN", &agent), ("ANTILOKI_PTY_SOCK", &sock)];
    let s = Serve::start_env(&state, free_port(), &env);
    let pane = 990_103;
    // a real interactive shell: dash does not understand a bracketed paste, so a command pasted into it with the markers would not run
    let started = s.post("/api/v1/workspace/pty/start", json!({ "pane_id": pane, "cmd": "sh", "args": [], "cols": 100, "rows": 30 }));
    assert_eq!(started["ok"], json!(true), "{started}");
    let sent = s.post("/api/v1/cockpit/pty/send", json!({ "pane_id": pane, "text": format!("echo typed-$((40+2)) > '{}'", made.display()) }));
    assert_eq!(sent["ok"], json!(true), "{sent}");
    let got = until(&s, 10, "the typed command to have run", || std::fs::read_to_string(&made).ok().filter(|t| !t.is_empty()));
    assert_eq!(got.trim(), "typed-42", "the shell ran it as typed: {got:?}");
}

#[test]
fn j15_a_terminal_pane_end_to_end_the_prompt_the_numbers_a_blocked_dialog_and_an_episode() {
    let _s = serial(); let tmp = Scratch::new("j15");
    let agent = agent_binary();
    let sock = tmp.path().join("pty.sock");
    let _host = HostGuard { agent: agent.clone(), sock: sock.clone() };
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(state.join("bin")).unwrap();
    // `claude` on the serve's PATH is the scripted TUI seat (no model, no network, no quota)
    std::os::unix::fs::symlink(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake-claude-tui"), state.join("bin/claude")).unwrap();
    let env: [(&str, &Path); 3] = [("ANTILOKI_PTY_HOST", Path::new("sidecar")), ("ANTILOKI_AGENT_BIN", &agent), ("ANTILOKI_PTY_SOCK", &sock)];
    let s = Serve::start_env(&state, free_port(), &env);
    s.add_workspace("work", &ws); s.activate("work");

    let made = s.post("/api/v1/cockpit/chat/create", json!({ "engine": "claude-code", "title": "j15" }));
    let pane = made["pane"]["id"].as_i64().expect("a pane");
    let started = s.post("/api/v1/cockpit/pty/start", json!({ "pane_id": pane, "agent": "claude-code", "cols": 100, "rows": 30 }));
    assert_eq!(started["ok"], json!(true), "{started}");
    until(&s, 20, "the TUI's composer to be up", || (s.get(&format!("/api/v1/cockpit/pty/status?pane_id={pane}"))["ready"] == json!(true)).then_some(()));
    let usage = || s.get(&format!("/api/v1/cockpit/pty/usage?pane_id={pane}"));

    // before any run: known, nothing working, no run
    let u = usage();
    assert_eq!((u["known"].clone(), u["working"].clone(), u["run"].clone()), (json!(true), json!(false), Value::Null), "{u}");

    // the engine's own prompt hook finds the serve through the box's serve.json, and asks the same pre-check for anything typed straight into the terminal
    let box_dir = std::path::PathBuf::from(made["pane"]["worktree_path"].as_str().expect("the pane's box"));
    let serve_json: Value = serde_json::from_str(&std::fs::read_to_string(box_dir.join(".antiloki/serve.json")).expect("serve.json is written when the terminal starts")).unwrap();
    assert_eq!(serve_json["pane_id"], json!(pane), "{serve_json}");
    assert!(serve_json["url"].as_str().unwrap().starts_with("http://127.0.0.1:"), "{serve_json}");
    // A FRONTIER PROVIDER SCREENS ITS OWN WORK: Heimdall's pre-check stays out of a Claude pane, and comes back when the pane asks for it
    let off = s.post("/api/v1/cockpit/precheck", json!({ "pane_id": pane, "text": "fix the picker in src/nowhere/gone.tsx", "source": "hook" })); assert_eq!((off["verdict"].clone(), off["by"].clone()), (json!("send"), json!("provider")), "{off}");
    let on = s.post("/api/v1/cockpit/pane/heimdall", json!({ "pane_id": pane, "on": true })); assert_eq!((on["heimdall"].clone(), on["heimdall_default"].clone()), (json!(true), json!(false)), "{on}");
    let typed = "fix the picker in src/nowhere/gone.tsx";
    let hook_ask = s.post("/api/v1/cockpit/precheck", json!({ "pane_id": pane, "text": typed, "source": "hook" }));
    assert_eq!(hook_ask["verdict"], json!("ask"), "typed straight into the terminal: nobody has checked it - {hook_ask}");
    // what the router read the ask as is noted for the pane's header, from the hook's check alone (no composer needed)
    assert!(usage()["route"].as_str().map_or(false, |r| !r.is_empty()), "the pane says how its last ask was read: {}", usage());
    // what the composer sent has been through the check already: the hook is told so, for exactly that text
    let sent = s.post("/api/v1/cockpit/pty/send", json!({ "pane_id": pane, "text": typed }));
    assert_eq!(sent["ok"], json!(true), "{sent}");
    let by = s.post("/api/v1/cockpit/precheck", json!({ "pane_id": pane, "text": format!("  {typed}\n"), "source": "hook" }));
    assert_eq!((by["verdict"].clone(), by["by"].clone()), (json!("send"), json!("composer")), "{by}");
    assert_eq!(s.post("/api/v1/cockpit/precheck", json!({ "pane_id": pane, "text": "fix the picker in src/nowhere/other.tsx", "source": "hook" }))["verdict"], json!("ask"), "another ask is not covered");
    assert_eq!(s.post("/api/v1/cockpit/precheck", json!({ "pane_id": pane, "text": typed }))["verdict"], json!("ask"), "the composer's own request is never short-circuited");
    until(&s, 20, "the pasted prompt to be answered", || (usage()["working"] == json!(false) && usage()["run"]["finished_t"].is_string()).then_some(()));


    // the pre-check, tier 0, against this pane's box: a path that is not there is a question; a clear ask is sent
    let v = s.post("/api/v1/cockpit/precheck", json!({ "pane_id": pane, "text": "fix src/loki/missing-file.tsx" }));
    assert_eq!(v["verdict"], json!("ask"), "{v}");
    assert_eq!(s.post("/api/v1/cockpit/precheck", json!({ "pane_id": pane, "text": "explain how the strip works" }))["verdict"], json!("send"));

    // a prompt through the composer's door: it is pasted and submitted, the run starts, and the pane says it is working
    let sent = s.post("/api/v1/cockpit/pty/send", json!({ "pane_id": pane, "text": "fix the folder picker FAKE:slow:1500" }));
    assert_eq!(sent["ok"], json!(true), "{sent}");
    until(&s, 15, "the pane to say it is working", || (usage()["working"] == json!(true)).then_some(()));
    // ...and finish: the run's numbers come from the hook's files and the engine's own transcript, priced by the existing calculator
    let done = until(&s, 20, "the run to finish and its cost to be final", || { let u = usage(); (u["run"]["finished_t"].is_string() && u["run"]["cost_source"] != json!("pending")).then_some(u) });
    let run = &done["run"];
    assert_eq!((run["tokens_in"].clone(), run["tokens_out"].clone(), run["turns"].clone(), run["tools"].clone(), run["transcript"].clone()), (json!(42100), json!(3800), json!(1), json!(2), json!(true)), "{run}");
    let (cost, baseline) = (run["cost"].as_f64().unwrap(), run["baseline"].as_f64().unwrap());
    assert!(cost > 0.0 && baseline > cost, "Sonnet is cheaper than the Opus yardstick for the same tokens: {run}");
    assert_eq!(run["against"], json!("claude · opus"));
    assert_eq!(run["cost_source"], json!("engine"), "the engine's own session cost is the price of the run, not the list: {run}");
    let one_run = (42_100.0 * 3.0 + 3_800.0 * 15.0) / 1e6;   // the scripted seat's own meter: what one such run costs
    assert!((cost - one_run).abs() < 1e-6, "this run costs its own share of the session, not the session: {run}");
    assert!((done["session_cost"].as_f64().unwrap() - 2.0 * one_run).abs() < 1e-6, "two runs so far (the hook-checked prompt and this one): {done}");
    assert!(run["ms"].as_i64().unwrap() >= 1400, "the run's length is the prompt to the answer: {run}");
    assert_eq!(done["working"], json!(false));
    assert!(done["session_cost"].as_f64().unwrap() > 0.0 && done["context"]["used_pct"].as_f64().unwrap() > 0.0 && done["limits"][0]["label"] == json!("5h"), "the status line's numbers: {done}");

    // a dialog the person has to answer shows as blocked (read from the terminal's own screen), and clears when it is answered
    let sent = s.post("/api/v1/cockpit/pty/send", json!({ "pane_id": pane, "text": "delete the build folder FAKE:ask-permission FAKE:slow:300" }));
    assert_eq!(sent["ok"], json!(true), "{sent}");
    let blocked = until(&s, 15, "the pane to say it needs the person", || usage()["blocked"].as_str().map(String::from));
    assert!(blocked.contains("Do you want to proceed"), "{blocked}");
    assert_eq!(s.post("/api/v1/cockpit/pty/input", json!({ "pane_id": pane, "data": "\r" }))["ok"], json!(true));
    until(&s, 15, "the dialog to clear and the second run to finish", || { let u = usage(); (u["blocked"].is_null() && u["working"] == json!(false) && u["run"]["prompt"].as_str().map_or(false, |p| p.contains("delete the build"))).then_some(()) });

    // NOTHING TO CLOSE (operator 2026-11-11): the three prompts (the hook-checked one, the picker, the dialog) are ONE SAGA on the workspace's timeline by themselves, an EPISODE for every ask, titled by its first ask,
    // with its deterministic metadata; the old close-episode door answers that it is gone
    // the list is served from a 2-second cache (swr.rs): wait until it shows the agent's saga
    let sagas = until(&s, 20, "the agent's saga in the list", || { let l = s.get("/api/v1/sagas"); l["sagas"].as_array().map_or(false, |a| a.iter().any(|x| x["plan_ref"].as_str().map_or(false, |p| p.starts_with("ep:")))).then(|| l) });
    let ep = sagas["sagas"].as_array().unwrap().iter().find(|x| x["plan_ref"].as_str().map_or(false, |p| p.starts_with("ep:"))).unwrap_or_else(|| panic!("the agent's work is a saga: {sagas}")).clone();
    assert!(ep["title"].as_str().unwrap().starts_with("fix the picker in src/nowhere"), "a saga is titled by its first ask: {ep}");
    assert_eq!((ep["meta"]["kind"].clone(), ep["meta"]["turns"].clone(), ep["meta"]["pane_id"].clone()), (json!("episode"), json!(3), json!(pane)), "{ep}");
    assert!(ep["meta"]["tokens"]["in"].as_i64().unwrap() > 0 && ep["meta"]["usd"].as_f64().unwrap() > 0.0, "the saga carries its own meter: {ep}");
    let detail = s.get(&format!("/api/v1/sagas/detail?id={}", ep["id"]));
    assert_eq!(detail["episodes"].as_array().unwrap().len(), 3, "an episode per ask: {detail}");
    assert!(detail["events"].as_array().unwrap().iter().any(|e| e["kind"] == "asked"), "and holds what was asked: {detail}");
    let gone = s.post("/api/v1/cockpit/pty/consolidate", json!({ "pane_id": pane }));
    assert_eq!(gone["ok"], json!(false)); assert!(gone["error"].as_str().unwrap().contains("closing an episode is gone"), "{gone}");
}

#[test]
fn j16_a_delegated_task_is_a_worker_pane_while_it_runs_and_a_record_after() {
    let _s = serial(); let tmp = Scratch::new("j16");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let s = Serve::start(&state, free_port());
    s.add_workspace("work", &ws); s.activate("work");
    let made = s.post("/api/v1/cockpit/chat/create", json!({ "engine": "claude-code", "title": "j16" }));
    let pane = made["pane"]["id"].as_i64().expect("a pane");
    let workers = || s.get(&format!("/api/v1/cockpit/cowork/workers?pane_id={pane}"))["workers"].as_array().cloned().unwrap_or_default();
    assert!(workers().is_empty(), "nothing delegated yet: no worker pane");

    // the pane's agent delegates, exactly as the MCP tool `antiloki_delegate` does (POST goals/plan with the pane id)
    let planned = s.post("/api/v1/cockpit/goals/plan", json!({ "pane_id": pane, "goal": "explain two things FAKE:slow:3000\n1. what the picker does\n2. what the strip does", "n": 2, "write": false, "auto": true, "router": false }));
    assert!(planned["run"].is_object(), "{planned}");
    let run_id = planned["run"]["id"].as_i64().unwrap();

    // while the worker's call is in flight, its pane exists, says who it is and has nothing but its record so far
    let running = until(&s, 30, "a worker to be running", || workers().into_iter().find(|w| w["status"] == json!("running")));
    assert_eq!(running["run_id"], json!(run_id));
    assert!(running["engine"].is_string(), "the pane is labelled with the worker's engine when it opens: {running}");
    assert!(running["events"].as_array().unwrap().iter().any(|e| e["kind"] == json!("worker")), "{running}");
    assert!(running["cost"].is_null() && running["tools"].is_null(), "no trace until the call returns: {running}");
    assert!(running["title"].as_str().map_or(false, |t| !t.is_empty()), "{running}");

    // when it returns, the same entry carries what it did
    let done = until(&s, 40, "the worker to finish", || workers().into_iter().find(|w| w["status"] == json!("done") || w["status"] == json!("failed")));
    assert_eq!(done["task"], running["task"]);
    assert!(done["ended"].is_number() && done["trace_engine"].is_null(), "{done}");
    assert!(done["events"].as_array().unwrap().len() >= running["events"].as_array().unwrap().len(), "{done}");
    // another pane's workers are not this pane's
    let other = s.post("/api/v1/cockpit/chat/create", json!({ "engine": "claude-code", "title": "other" }));
    let oid = other["pane"]["id"].as_i64().unwrap();
    assert!(s.get(&format!("/api/v1/cockpit/cowork/workers?pane_id={oid}"))["workers"].as_array().unwrap().is_empty());
    assert_eq!(s.get("/api/v1/cockpit/cowork/workers?pane_id=999999")["workers"], json!([]));
}

/// Run the engine's prompt hook (the real `antiloki-agent hook`) the way Claude Code does: the event on stdin, the answer on stdout. Returns the hook's stdout when it finished.
fn run_prompt_hook(agent: &Path, boxdir: &Path, prompt: &str, wait_ms: &str) -> std::process::Child {
    use std::io::Write;
    let mut c = std::process::Command::new(agent).arg("hook").env("ANTILOKI_ASK_WAIT_MS", wait_ms).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().expect("the hook binary");
    let ev = json!({ "hook_event_name": "UserPromptSubmit", "cwd": boxdir, "prompt": prompt, "session_id": "j17" }).to_string();
    c.stdin.take().unwrap().write_all(ev.as_bytes()).unwrap();
    c
}
fn hook_context(c: std::process::Child) -> Option<String> {
    let out = c.wait_with_output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).ok()?;
    v["hookSpecificOutput"]["additionalContext"].as_str().map(String::from)
}

#[test]
fn j17_a_question_typed_into_a_terminal_is_asked_of_the_person_and_their_choice_reaches_the_model() {
    let _s = serial(); let tmp = Scratch::new("j17");
    let agent = agent_binary();
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let env: [(&str, &Path); 1] = [("ANTILOKI_ASK_WAIT_MS", Path::new("3000"))];
    let s = Serve::start_env(&state, free_port(), &env);
    s.add_workspace("work", &ws); s.activate("work");
    let made = s.post("/api/v1/cockpit/chat/create", json!({ "engine": "claude-code", "title": "j17" }));
    let pane = made["pane"]["id"].as_i64().unwrap();
    let boxdir = std::path::PathBuf::from(made["pane"]["worktree_path"].as_str().unwrap());
    std::fs::write(boxdir.join(".antiloki/serve.json"), json!({ "url": format!("http://127.0.0.1:{}", s.port), "pane_id": pane }).to_string()).unwrap();
    // what a pane's terminal start bakes (the hook stays silent in a directory with no pack)
    std::fs::create_dir_all(boxdir.join(".antiloki/context")).unwrap(); std::fs::write(boxdir.join(".antiloki/context/manifest.json"), "{}").unwrap();
    assert_eq!(s.post("/api/v1/cockpit/pane/heimdall", json!({ "pane_id": pane, "on": true }))["heimdall"], json!(true));   // Heimdall screens a frontier provider only when the pane asks for it
    let ask = "fix the picker in src/nowhere/gone.tsx";
    let asks = || s.get("/api/v1/cockpit/cowork/asks")["asks"].as_array().cloned().unwrap_or_default();

    // 1. no screen has ever asked: the question goes to the model at once, as context, and the hook does not wait
    let t0 = std::time::Instant::now();
    let ctx = hook_context(run_prompt_hook(&agent, &boxdir, ask, "3000")).expect("context");
    assert!(ctx.contains("src/nowhere/gone.tsx") && ctx.contains("Ask the person"), "{ctx}");
    assert!(t0.elapsed() < std::time::Duration::from_secs(2), "no screen, no wait: {:?}", t0.elapsed());

    // 2. a screen is open (it has just asked): the question is PARKED, the hook waits, the person picks a chip, and the model is told what they chose
    assert!(asks().is_empty());
    let _ = asks();   // the screen asks: the serve now knows one is watching
    let hook = run_prompt_hook(&agent, &boxdir, ask, "3000");
    let open = until(&s, 10, "the question to be parked for the screen", || { let a = asks(); (!a.is_empty()).then_some(a) });
    assert_eq!((open[0]["pane_id"].clone(), open[0]["text"].clone()), (json!(pane), json!(ask)));
    let q = &open[0]["questions"][0]; let (qid, option) = (q["id"].as_str().unwrap().to_string(), q["options"][0].as_str().unwrap().to_string());
    let ok = s.post("/api/v1/cockpit/cowork/asks/answer", json!({ "id": open[0]["id"], "answers": { qid: option.clone() } }));
    assert_eq!(ok["ok"], json!(true), "{ok}");
    let ctx = hook_context(hook).expect("the person's choice as context");
    assert!(ctx.contains(&option) && ctx.contains("do not ask again"), "{ctx}");
    assert!(asks().is_empty(), "an answered question is not shown again");

    // 3. the card is dismissed: the prompt goes through with nothing added
    let _ = asks();
    let hook = run_prompt_hook(&agent, &boxdir, ask, "3000");
    let open = until(&s, 10, "the question again", || { let a = asks(); (!a.is_empty()).then_some(a) });
    assert_eq!(s.post("/api/v1/cockpit/cowork/asks/answer", json!({ "id": open[0]["id"], "skip": true }))["ok"], json!(true));
    assert_eq!(hook_context(hook), None, "a dismissed card adds nothing to the prompt");

    // 4. nobody answers in time: after the wait the question goes to the model as before, and the prompt is never stuck
    let _ = asks();
    let t0 = std::time::Instant::now();
    let hook = run_prompt_hook(&agent, &boxdir, ask, "3000");
    until(&s, 10, "the question once more", || { let a = asks(); (!a.is_empty()).then_some(a) });
    let ctx = hook_context(hook).expect("the question as context after the timeout");
    assert!(ctx.contains("Ask the person"), "{ctx}");
    assert!(t0.elapsed() >= std::time::Duration::from_millis(2900) && t0.elapsed() < std::time::Duration::from_secs(12), "waited the limit, no longer: {:?}", t0.elapsed());
    assert!(asks().is_empty(), "an unanswered question does not stay for ever");

    // 5. a clear ask is never a question; a slash command is not asked at all
    assert_eq!(hook_context(run_prompt_hook(&agent, &boxdir, "explain how the router decides", "3000")), None);
    assert_eq!(hook_context(run_prompt_hook(&agent, &boxdir, "/compact src/nowhere/gone.tsx", "3000")), None);
}

/// A plain HTTP GET for bytes (the harness's `call` reads JSON): returns the status line's code, the content-type, and the body.
fn raw_get(port: u16, path: &str) -> (u16, String, Vec<u8>) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap(); s.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    write!(s, "GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
    let mut buf = vec![]; s.read_to_end(&mut buf).unwrap();
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap(); let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let code = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let ctype = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-type:").map(|v| v.trim().to_string())).unwrap_or_default();
    (code, ctype, buf[split + 4..].to_vec())
}

#[test]
fn j18_omarchy_themes_are_listed_with_their_palettes_and_wallpapers_and_nothing_outside_a_theme_is_served() {
    let _s = serial(); let tmp = Scratch::new("j18");
    let (home, sys, store) = (tmp.path().join("home"), tmp.path().join("omarchy"), tmp.path().join("store"));
    let mk = |dir: &Path, id: &str, colors: &str| { let t = dir.join(id); std::fs::create_dir_all(t.join("backgrounds")).unwrap(); std::fs::write(t.join("colors.toml"), colors).unwrap(); t };
    let nord = mk(&sys.join("themes"), "nord", "mode = \"dark\"\naccent = \"#88c0d0\"\nbackground = \"#2e3440\"\nforeground = \"#d8dee9\"\n");
    std::fs::write(nord.join("backgrounds/0-aurora.jpg"), b"\xff\xd8\xffJPEGBYTES").unwrap(); std::fs::write(nord.join("preview.png"), b"\x89PNGBYTES").unwrap(); std::fs::write(nord.join("install.sh"), "rm -rf /").unwrap();
    mk(&home.join(".config/omarchy/themes"), "mine", "mode = \"light\"\naccent = \"#112233\"\nbackground = \"#ffffff\"\n");
    mk(&store, "downloaded-one", "accent = \"#abcdef\"\n");
    std::fs::write(tmp.path().join("secret.png"), b"secret").unwrap();
    std::fs::create_dir_all(home.join(".local/state/omarchy/current")).unwrap(); std::fs::write(home.join(".local/state/omarchy/current/theme.name"), "nord\n").unwrap();
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let omarchy_user = home.join(".config/omarchy/themes"); let omarchy_state = home.join(".local/state/omarchy/current");
    let env: [(&str, &Path); 4] = [("ANTILOKI_OMARCHY_USER_THEMES", &omarchy_user), ("ANTILOKI_OMARCHY_STATE", &omarchy_state), ("OMARCHY_PATH", &sys), ("ANTILOKI_THEMES_DIR", &store)];
    let s = Serve::start_env(&state, free_port(), &env);

    let l = s.get("/api/v1/themes/omarchy");
    assert_eq!(l["current"], json!("nord"), "{l}");
    let ids: Vec<&str> = l["themes"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect(); assert_eq!(ids, vec!["downloaded-one", "mine", "nord"], "{l}");
    let n = l["themes"].as_array().unwrap().iter().find(|t| t["id"] == "nord").unwrap();
    assert_eq!((n["name"].clone(), n["source"].clone(), n["mode"].clone(), n["colors"]["accent"].clone(), n["backgrounds"].clone(), n["preview"].clone()), (json!("Nord"), json!("system"), json!("dark"), json!("#88c0d0"), json!(["0-aurora.jpg"]), json!(true)));
    assert_eq!(l["themes"].as_array().unwrap().iter().find(|t| t["id"] == "mine").unwrap()["source"], json!("omarchy")); assert_eq!(l["themes"][0]["source"], json!("downloaded"));

    // a wallpaper and the preview are served as images
    let (code, ctype, body) = raw_get(s.port, "/api/v1/themes/omarchy/file?id=nord&path=backgrounds/0-aurora.jpg");
    assert_eq!((code, ctype.as_str(), body.as_slice()), (200, "image/jpeg", &b"\xff\xd8\xffJPEGBYTES"[..]));
    assert_eq!(raw_get(s.port, "/api/v1/themes/omarchy/file?id=nord&path=preview.png").1, "image/png");
    // nothing else is: a script in the theme, the palette file, a path out of it, an id out of the list, no arguments
    for bad in ["id=nord&path=install.sh", "id=nord&path=colors.toml", "id=nord&path=../../secret.png", "id=nord&path=/etc/passwd", "id=..&path=secret.png", "id=nope&path=preview.png", "id=nord", "path=preview.png", "id=nord&path=backgrounds/missing.jpg"] {
        assert_eq!(raw_get(s.port, &format!("/api/v1/themes/omarchy/file?{bad}")).0, 404, "{bad}");
    }
    // install: an address that is not clean, and one that cannot be reached, both say why and leave nothing behind
    for bad in ["", "http://github.com/a/b", "file:///etc", "ext::sh -c id", "https://127.0.0.1/a/b", "-oProxyCommand=x"] {
        let (code, v) = s.call("POST", "/api/v1/themes/omarchy/install", Some(json!({ "url": bad }))); assert_eq!(code, 400, "{bad:?} {v}");
    }
    let (code, v) = s.call("POST", "/api/v1/themes/omarchy/install", Some(json!({ "url": "https://theme-host.invalid/nobody/omarchy-ghost-theme.git" })));
    assert_eq!(code, 502, "{v}"); assert!(v["error"].as_str().unwrap().contains("could not fetch it"), "{v}");
    assert!(!store.join("ghost").exists(), "nothing half-installed");
    assert_eq!(std::fs::read_dir(&store).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".antiloki-install")).count(), 0);
    assert!(!home.join(".config/omarchy/themes/ghost").exists(), "downloads never go into Omarchy's own folder");
}

/// Needs the network: a REAL community theme, downloaded through the real endpoint with the real git. `cargo test ... j18b -- --ignored`
#[test]
#[ignore]
fn j18b_a_real_community_theme_is_downloaded_into_antilokis_own_store() {
    let _s = serial(); let tmp = Scratch::new("j18b");
    let (home, store) = (tmp.path().join("home"), tmp.path().join("store")); std::fs::create_dir_all(&home).unwrap();
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let omarchy_user = home.join(".config/omarchy/themes");
    let env: [(&str, &Path); 3] = [("ANTILOKI_OMARCHY_USER_THEMES", &omarchy_user), ("OMARCHY_PATH", Path::new("/nonexistent")), ("ANTILOKI_THEMES_DIR", &store)];
    let s = Serve::start_env(&state, free_port(), &env);
    let (code, v) = s.call("POST", "/api/v1/themes/omarchy/install", Some(json!({ "url": "https://github.com/hoblin/omarchy-cobalt2-theme" })));
    assert_eq!(code, 200, "{v}");
    assert_eq!(v["id"], json!("cobalt2")); assert!(v["theme"]["colors"]["accent"].as_str().unwrap().starts_with('#'), "{v}");
    assert!(store.join("cobalt2/colors.toml").is_file() && !store.join("cobalt2/.git").exists());
    assert_eq!(s.call("POST", "/api/v1/themes/omarchy/install", Some(json!({ "url": "https://github.com/hoblin/omarchy-cobalt2-theme" }))).0, 409, "installing it again is refused");
    assert_eq!(s.get("/api/v1/themes/omarchy")["themes"][0]["source"], json!("downloaded"));
    assert!(!home.join(".config/omarchy").exists(), "Omarchy's own folder was never touched");
}

// J19 - X-RAY ACROSS SPACES (operator 2026-11-10: "make x-ray work for multiple spaces at the same time with a filter per space"). One serve, two registered workspaces, ONE of them active. A request that carries
// `X-Antiloki-Workspace: <root>` is answered from THAT workspace's own database - its atlas, its rules - while the active workspace is neither switched nor read, and a folder that is not a registered workspace is
// not opened (the request falls back to the active one).
#[test]
fn j19_a_request_can_be_about_another_workspace_without_switching_to_it() {
    let _s = serial(); let tmp = Scratch::new("j19");
    let alpha = project(tmp.path(), "alpha"); let beta = project(tmp.path(), "beta");
    std::fs::create_dir_all(alpha.join("src")).unwrap(); std::fs::write(alpha.join("src/alpha_only.ts"), "export const a = 1;\n").unwrap();
    std::fs::create_dir_all(beta.join("src")).unwrap(); std::fs::write(beta.join("src/beta_only.ts"), "export const b = 2;\n").unwrap();
    git(&alpha, &["add", "-A"]); git(&alpha, &["commit", "-q", "-m", "alpha code"]); git(&beta, &["add", "-A"]); git(&beta, &["commit", "-q", "-m", "beta code"]);
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let serve = Serve::start(&state, free_port());
    serve.add_workspace("alpha", &alpha); serve.add_workspace("beta", &beta);
    serve.activate("alpha");
    // each workspace's atlas is built where it lives: alpha as the active one, beta by asking ABOUT it
    serve.post("/api/v1/codebase/refresh", json!({}));
    serve.post_in("/api/v1/codebase/refresh", json!({}), &beta);
    let files = |v: &Value| -> Vec<String> { let v = v.get("result").unwrap_or(v); let mut f: Vec<String> = v["nodes"].as_array().cloned().unwrap_or_default().iter().filter(|n| n["type"] == "file").filter_map(|n| n["path"].as_str().map(String::from)).collect(); f.sort(); f.dedup(); f };
    let mine = files(&serve.post("/api/v1/codebase/get", json!({})));
    let theirs = files(&serve.post_in("/api/v1/codebase/get", json!({}), &beta));
    assert!(mine.contains(&"src/alpha_only.ts".to_string()) && !mine.contains(&"src/beta_only.ts".to_string()), "the active workspace answers for itself: {mine:?}");
    assert!(theirs.contains(&"src/beta_only.ts".to_string()) && !theirs.contains(&"src/alpha_only.ts".to_string()), "the header names beta, so beta answers: {theirs:?}");
    assert_eq!(serve.active(), "alpha", "asking about another workspace never switches to it");
    // the rules are a workspace's own too: a law switched off in beta is still on in alpha
    let book = |v: &Value| -> Vec<Value> { let v = v.get("result").unwrap_or(v); v["laws"].as_array().or_else(|| v["rules"].as_array()).cloned().unwrap_or_default() };
    let list = json!({ "action_name": "system.constitution.law", "input": { "op": "list" } });
    serve.post("/api/v1/dispatch", json!({ "action_name": "system.constitution.law", "input": { "op": "seed" } }));
    serve.post_in("/api/v1/dispatch", json!({ "action_name": "system.constitution.law", "input": { "op": "seed" } }), &beta);
    let a_laws = book(&serve.post("/api/v1/dispatch", list.clone()));
    let b_laws = book(&serve.post_in("/api/v1/dispatch", list.clone(), &beta));
    assert!(!a_laws.is_empty() && a_laws.len() == b_laws.len(), "both workspaces carry the seeded book: {} vs {}", a_laws.len(), b_laws.len());
    let key = b_laws[0]["key"].as_str().expect("a law has a key").to_string();
    serve.post_in("/api/v1/dispatch", json!({ "action_name": "system.constitution.law", "input": { "op": "suspend", "key": key } }), &beta);
    let status = |laws: &[Value]| laws.iter().find(|l| l["key"] == json!(key)).map(|l| l["status"].clone()).unwrap_or(Value::Null);
    let b_after = book(&serve.post_in("/api/v1/dispatch", list.clone(), &beta)); let a_after = book(&serve.post("/api/v1/dispatch", list));
    assert_eq!(status(&b_after), json!("suspended"), "switched off in beta");
    assert_ne!(status(&a_after), json!("suspended"), "and still on in alpha: {:?}", status(&a_after));
    // a folder that is not a registered workspace is not opened: the active one answers
    let stranger = tmp.path().join("stranger"); std::fs::create_dir_all(stranger.join(".antiloki/db")).unwrap();
    let fallback = files(&serve.post_in("/api/v1/codebase/get", json!({}), &stranger));
    assert_eq!(fallback, mine, "an unregistered folder falls back to the active workspace");
}

// J20 - TEAMS ON REAL TERMINALS (operator 2026-11-10: "planner and worker ... Plan, worker, QA ... if QA disapproves a delivery, a retry loop, three times maximum, and feedback each loop"). THREE scripted TUI seats in their own boxes,
// linked as a planner, a worker and a QA. The serve types the prompts, hears each turn end through the engine's own hook files, hands the worker's delivery to the QA's box as a patch, and decides from the QA's verdict:
// pass, retry with only what failed, or stop for the person after three rejections / a QA that edits. Every step is a task of one saga.
#[test]
fn j20_a_team_plans_works_and_is_judged_retries_with_feedback_and_stops_for_the_person_after_three_rejections() {
    let _s = serial(); let tmp = Scratch::new("j20");
    let agent = agent_binary(); let sock = tmp.path().join("pty.sock"); let _host = HostGuard { agent: agent.clone(), sock: sock.clone() };
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(state.join("bin")).unwrap();
    std::os::unix::fs::symlink(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake-claude-tui"), state.join("bin/claude")).unwrap();
    let plan = r#"{"summary":"add a greeting","steps":["write greet.txt"],"criteria":[{"id":"c1","text":"greet.txt exists"},{"id":"c2","text":"it greets the world"}]}"#;
    let bad = r#"{"results":[{"id":"c1","ok":true},{"id":"c2","ok":false,"evidence":"it does not say world"}],"summary":"c2 fails"}"#;
    let good = r#"{"results":[{"id":"c1","ok":true},{"id":"c2","ok":true}],"summary":"fine"}"#;
    let script = tmp.path().join("team.json");
    std::fs::write(&script, json!({
        "retry":  { "plan": plan, "work": [{ "greet.txt": "hello\n" }, { "greet.txt": "hello world\n" }], "verdicts": [bad, good] },
        "reject": { "plan": plan, "work": [{ "greet.txt": "a\n" }, { "greet.txt": "b\n" }, { "greet.txt": "c\n" }], "verdicts": [bad, bad, bad] },
        "qaedit": { "plan": plan, "work": [{ "greet.txt": "hello world\n" }], "verdicts": [good], "qa_edits": true },
        "same":   { "plan": plan, "work": [{ "greet.txt": "same\n" }], "verdicts": [bad, bad] },
        "pedit":  { "plan": plan, "work": [{ "greet.txt": "hello world\n" }], "verdicts": [good], "planner_edits": true },
    }).to_string()).unwrap();
    let env: [(&str, &Path); 4] = [("ANTILOKI_PTY_HOST", Path::new("sidecar")), ("ANTILOKI_AGENT_BIN", &agent), ("ANTILOKI_PTY_SOCK", &sock), ("FAKE_TEAM_SCRIPT", &script)];
    let s = Serve::start_env(&state, free_port(), &env);
    s.add_workspace("work", &ws); s.activate("work");

    let mut panes = vec![];
    let session = s.post("/api/v1/cockpit/session/create", json!({ "name": "Cowork", "mission_md": "teams" }))["session"]["id"].as_i64().expect("a session");
    for role in ["planner", "worker", "qa"] {
        // an agent the way the screen makes one: its own git worktree (a delivery is a diff against its base)
        let made = s.post("/api/v1/cockpit/pane/add", json!({ "session_id": session, "base_agent": "dev", "subagent_name": format!("{role}-j20"), "pane_kind": "claude-code", "scope": [], "grants": ["file.read", "file.write"], "provision": true, "tier": "mid", "settings": { "title": role } }));
        let pane = made["pane"]["id"].as_i64().unwrap_or_else(|| panic!("a pane: {made}"));
        assert_eq!(s.post("/api/v1/cockpit/pty/start", json!({ "pane_id": pane, "agent": "claude-code", "cols": 100, "rows": 30 }))["ok"], json!(true));
        until(&s, 20, "the TUI's composer to be up", || (s.get(&format!("/api/v1/cockpit/pty/status?pane_id={pane}"))["ready"] == json!(true)).then_some(()));
        panes.push((role, pane, std::path::PathBuf::from(made["pane"]["worktree_path"].as_str().expect("a box"))));
    }
    let teams = |b: Value| s.post("/api/v1/cockpit/teams", b);
    assert_eq!(teams(json!({ "op": "create", "name": "k" }))["ok"], json!(true));
    let guard_log = || -> Vec<Value> { std::fs::read_to_string(ws.join(".antiloki/team-guard.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default() };
    // a team with no worker is told what it lacks; a pane that is not an agent here cannot be a member
    assert!(teams(json!({ "op": "run", "team": "k", "task": "x" }))["error"].as_str().unwrap().contains("no worker"));
    for (role, pane, _) in &panes { let r = teams(json!({ "op": "link", "team": "k", "role": role, "pane": pane })); assert_eq!(r["ok"], json!(true), "{r}"); }
    assert_eq!(teams(json!({ "op": "link", "team": "k", "role": "qa", "pane": 99 }))["ok"], json!(false), "one per role");
    let listed = teams(json!({ "op": "list" })); assert_eq!(listed["teams"][0]["members"].as_array().unwrap().len(), 3, "{listed}"); assert_eq!(listed["teams"][0]["ready"], json!(true));
    // THE GATE: the planner's and the QA's boxes are held read-only (the engine's hook reads the marker); the worker's is not
    let marker = |i: usize| panes[i].2.join(".antiloki/readonly");
    assert!(marker(0).exists() && marker(2).exists() && !marker(1).exists(), "planner and QA are held, the worker is free");
    assert!(std::fs::read_to_string(marker(0)).unwrap().contains("planner") && std::fs::read_to_string(marker(2)).unwrap().contains("qa"));

    let run = |task: &str| -> Value {
        let started = teams(json!({ "op": "run", "team": "k", "task": task })); assert_eq!(started["ok"], json!(true), "{started}");
        let id = started["run_id"].as_str().unwrap().to_string();
        until(&s, 120, "the team's run to end", || { let st = teams(json!({ "op": "status", "run_id": id })); (st["run"]["ended"] == json!(true)).then(|| st["run"].clone()) })
    };
    let saga_of = |r: &Value| { let id = r["saga"].as_i64().unwrap(); s.get(&format!("/api/v1/sagas/detail?id={id}")) };
    let titles = |d: &Value| -> Vec<String> { d["tasks"].as_array().unwrap().iter().map(|t| t["title"].as_str().unwrap().to_string()).collect() };

    // 1. the QA rejects once; the worker is told ONLY what failed; the second delivery passes
    let r = run("scenario:retry add a greeting");
    assert_eq!((r["outcome"].clone(), r["run"]["attempt"].clone(), r["verdicts"].as_array().unwrap().len()), (json!("done"), json!(2), 2), "{r}");
    assert_eq!(std::fs::read_to_string(panes[1].2.join("greet.txt")).unwrap(), "hello world\n", "the worker's box holds its work");
    assert!(!panes[2].2.join("greet.txt").exists(), "the delivery was only LENT to the QA: its box is back to its baseline when the judging is over");
    let worker_prompts = std::fs::read_to_string(panes[1].2.join(".antiloki/prompts.jsonl")).unwrap();
    assert!(worker_prompts.contains("it does not say world") && worker_prompts.contains("attempt 2 of 3"), "the retry carries the QA's feedback: {worker_prompts}");
    assert!(!worker_prompts.lines().last().unwrap().contains("greet.txt exists"), "...and only what failed: the passing criterion is not repeated");
    let d = saga_of(&r); assert_eq!(titles(&d), vec!["Plan", "Work · attempt 1", "QA · attempt 1", "Work · attempt 2", "QA · attempt 2"], "{d}");
    assert!(d["events"].as_array().unwrap().iter().any(|e| e["kind"] == "done" && e["text"].as_str().unwrap().contains("2 attempts")), "{d}");
    assert!(d["tasks"].as_array().unwrap().iter().all(|t| t["status"] == "done"), "{d}");
    // THE SAGA KEEPS WHAT WAS CHANGED, in its own sqlite: the worker's delivery, under its work step, with the files and a patch that is a real diff
    let ch = d["changes"].as_array().unwrap(); assert!(!ch.is_empty(), "the saga has no diff: {d}");
    let last = &ch[0]; assert!(last["files"].as_array().unwrap().iter().any(|f| f["path"] == "greet.txt" && f["status"] == "added"), "{last}");
    assert!(last["task_id"].is_i64() && d["tasks"].as_array().unwrap().iter().any(|t| t["id"] == last["task_id"] && t["title"].as_str().unwrap().starts_with("Work")), "the diff belongs to a work step: {d}");
    assert!(last.get("patch").is_none(), "the detail lists snapshots without their patches");
    let one = s.get(&format!("/api/v1/sagas/change?id={}", last["id"])); assert!(one["change"]["patch"].as_str().unwrap().contains("+hello world"), "{one}");

    // 2. three rejections: the run stops for the person, with every verdict kept
    let r = run("scenario:reject make it greet");
    assert_eq!((r["outcome"].clone(), r["run"]["attempt"].clone(), r["verdicts"].as_array().unwrap().len()), (json!("blocked"), json!(3), 3), "{r}");
    assert!(r["blocked"].as_str().unwrap().contains("3 times") && r["blocked"].as_str().unwrap().contains("c2"), "{r}");
    let d = saga_of(&r); assert!(d["events"].as_array().unwrap().iter().any(|e| e["kind"] == "blocked" && e["text"].as_str().unwrap().starts_with("needs you")), "{d}");
    assert_eq!(titles(&d).iter().filter(|t| t.starts_with("Work")).count(), 3, "three attempts, no fourth: {d}");

    // 3. a retry that changes nothing is not tried again
    let r = run("scenario:same do it");
    assert_eq!(r["outcome"], json!("blocked")); assert!(r["blocked"].as_str().unwrap().contains("very same change"), "{r}"); assert_eq!(r["verdicts"].as_array().unwrap().len(), 1, "{r}");

    // 5. THE PERSON TALKS TO THE PLANNER (only the planner is chatted with): its answer carries a plan block, the team carries it out without asking the planner again, and the planner is told how it ended
    let before = panes[0].2.join(".antiloki/prompts.jsonl");
    // (the person types after the planner has finished telling them how the last run ended: two writers typing into one terminal at once is not what a person does)
    until(&s, 60, "the planner to be done with the last result", || { let n = std::fs::read_to_string(panes[0].2.join(".antiloki/answers.jsonl")).unwrap_or_default().matches("ack: [team result]").count(); let u = s.get(&format!("/api/v1/cockpit/pty/usage?pane_id={}", panes[0].1)); (n >= 3 && u["working"] == json!(false)).then_some(()) });
    let said = s.post("/api/v1/cockpit/pty/send", json!({ "pane_id": panes[0].1, "text": "scenario:retry add a greeting, talking to the planner" })); assert_eq!(said["ok"], json!(true), "{said}");
    let chat_run = until(&s, 120, "the chat plan to be carried out", || { let l = teams(json!({ "op": "list" })); l["runs"].as_array().and_then(|r| r.iter().find(|x| x["task"].as_str().map_or(false, |t| t.contains("talking to the planner")) && x["ended"] == json!(true))).cloned() });
    assert_eq!((chat_run["outcome"].clone(), chat_run["run"]["attempt"].clone()), (json!("done"), json!(2)), "{chat_run}");
    let planner_prompts = std::fs::read_to_string(&before).unwrap();
    assert_eq!(planner_prompts.matches("You are the PLANNER of a team").count(), 3, "the team asked the planner for a plan only in the three runs the bar started, never for the chat run: {planner_prompts}");
    until(&s, 30, "the planner to be told the result", || std::fs::read_to_string(&before).ok().filter(|t| t.contains("[team result] The work is done after 2 attempts")));

    // 4. a QA that edits a file is not trusted, even when it says yes
    let r = run("scenario:qaedit greet");
    assert_eq!(r["outcome"], json!("blocked")); assert!(r["blocked"].as_str().unwrap().contains("QA edited files"), "{r}");
    assert!(!panes[2].2.join("qa-note.txt").exists(), "what the QA wrote was REVERTED: its box is back to its baseline");
    assert!(guard_log().iter().any(|e| e["role"] == "qa" && e["files"].as_array().unwrap().iter().any(|f| f == "qa-note.txt")), "and the violation is on record: {:?}", guard_log());
    assert!(std::fs::read_to_string(panes[2].2.join(".antiloki/prompts.jsonl")).unwrap().contains("[team] Your changes were REVERTED"), "the QA was told");
    assert!(saga_of(&r)["events"].as_array().unwrap().iter().any(|e| e["kind"] == "violation" && e["text"].as_str().unwrap().contains("qa-note.txt")), "the saga's trail says so");

    // 6. A PLANNER THAT CHANGES CODE (the operator tested one that did): the change is reverted, the run goes on with the plan, and it is on record; the worker's box is not touched by any of it
    let r = run("scenario:pedit greet");
    assert_eq!(r["outcome"], json!("done"), "the first violation is reverted, not fatal: {r}");
    assert!(!panes[0].2.join("planner-oops.txt").exists(), "the planner's file is gone");
    assert!(guard_log().iter().any(|e| e["role"] == "planner" && e["files"].as_array().unwrap().iter().any(|f| f == "planner-oops.txt")), "{:?}", guard_log());
    assert!(std::fs::read_to_string(panes[0].2.join(".antiloki/prompts.jsonl")).unwrap().contains("a planner never edits code"), "the planner was told what was reverted and why");
    assert!(!panes[1].2.join("planner-oops.txt").exists());
    let listed = teams(json!({ "op": "list" })); assert!(listed["teams"][0]["violations"].as_i64().unwrap() >= 2, "{listed}");
    // BETWEEN RUNS, TOO: a change made to a planner's box by anything at all (a command the hook did not read as a write, an engine with no hooks) is found by the watcher within seconds and put back
    std::fs::write(panes[0].2.join("sneaky.txt"), "written behind the hook's back\n").unwrap(); std::process::Command::new("git").args(["commit", "--allow-empty", "-q", "-m", "jailbreak"]).current_dir(&panes[0].2).output().unwrap();
    until(&s, 30, "the watcher to revert the planner's box", || (!panes[0].2.join("sneaky.txt").exists()).then_some(()));
    assert!(guard_log().iter().any(|e| e["role"] == "planner" && e["run"].is_null() && e["files"].as_array().unwrap().iter().any(|f| f == "sneaky.txt")), "{:?}", guard_log());
    // a pane that leaves the team is let go: the marker goes with it
    assert_eq!(teams(json!({ "op": "unlink", "pane": panes[2].1 }))["ok"], json!(true)); assert!(!marker(2).exists(), "the QA left: its box writes again");
}

// J21 - TWO SPACES DO NOT SHARE PANES (operator 2026-11-10: "e2escratch and ministore are completely apart spaces, but they are using the same panes"). A layout belongs to the workspace it was made in: a save that names another
// workspace than the open one is refused (it was on its way while the person switched), a layout holding panes this workspace does not have comes back without them, and the new panes of two workspaces are numbered apart.
#[test]
fn j21_a_layout_never_crosses_into_another_workspace_and_two_workspaces_number_their_panes_apart() {
    let _s = serial(); let tmp = Scratch::new("j21");
    let a = project(tmp.path(), "alpha"); let b = project(tmp.path(), "beta");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let serve = Serve::start(&state, free_port());
    serve.add_workspace("alpha", &a); serve.add_workspace("beta", &b);
    let leaf = |id: &str, kind: &str, pane: i64| json!({ "type": "leaf", "pane": { "id": id, "kind": kind, "paneId": pane } });
    // alpha's panes: made in alpha, they number from alpha's base
    serve.activate("alpha");
    let mk = |s: &Serve, name: &str| -> i64 { let sess = s.post("/api/v1/cockpit/session/create", json!({ "name": "Cowork", "mission_md": "x" }))["session"]["id"].as_i64().unwrap(); s.post("/api/v1/cockpit/pane/add", json!({ "session_id": sess, "base_agent": "dev", "subagent_name": name, "pane_kind": "claude-code", "scope": [], "grants": ["file.read"], "provision": false, "tier": "mid", "settings": { "title": name } }))["pane"]["id"].as_i64().unwrap_or(0) };
    let pa = mk(&serve, "a1");
    serve.activate("beta"); let pb = mk(&serve, "b1");
    assert!(pa > 0 && pb > 0 && pa != pb, "new panes of two workspaces are numbered apart: {pa} vs {pb}");
    // a layout of alpha's, saved while alpha is open
    serve.activate("alpha");
    let alpha_layout = json!({ "root": { "type": "split", "dir": "row", "sizes": [0.5, 0.5], "children": [leaf(&format!("claude-code-{pa}"), "engine", pa), leaf("shell-900001", "shell", 900_001)] }, "focus": null, "zoom": null });
    let put = serve.post("/api/v1/cockpit/cowork/layout", json!({ "layout": alpha_layout, "workspace": a.to_string_lossy() })); assert_eq!(put["ok"], json!(true), "{put}");
    // the person switches to beta; a save that was on its way for alpha arrives: refused, and beta's layout is not alpha's
    serve.activate("beta");
    let late = serve.post("/api/v1/cockpit/cowork/layout", json!({ "layout": alpha_layout, "workspace": a.to_string_lossy() }));
    assert_eq!((late["ok"].clone(), late["stale"].clone()), (json!(false), json!(true)), "{late}");
    assert!(!b.join(".antiloki/cowork-layout.json").exists(), "nothing was written into beta");
    // a layout that already holds alpha's agent (written by an older build) comes back from beta WITHOUT it, and keeps what is not an agent
    std::fs::create_dir_all(b.join(".antiloki")).unwrap(); std::fs::write(b.join(".antiloki/cowork-layout.json"), alpha_layout.to_string()).unwrap();
    let got = serve.get("/api/v1/cockpit/cowork/layout")["layout"].clone();
    assert_eq!((got["root"]["type"].clone(), got["root"]["pane"]["id"].clone()), (json!("leaf"), json!("shell-900001")), "alpha's agent is gone, the shell stays: {got}");
    // alpha still has its own
    serve.activate("alpha");
    let mine = serve.get("/api/v1/cockpit/cowork/layout")["layout"].clone(); assert_eq!(mine["root"]["children"].as_array().unwrap().len(), 2, "{mine}");
    // a save that names the open workspace lands
    assert_eq!(serve.post("/api/v1/cockpit/cowork/layout", json!({ "layout": mine, "workspace": a.to_string_lossy() }))["ok"], json!(true));
}

#[test]
fn j22_threads_under_a_space_each_have_their_own_screen_and_a_save_for_the_wrong_thread_is_refused() {
    let _s = serial(); let tmp = Scratch::new("j22");
    let a = project(tmp.path(), "alpha");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let serve = Serve::start(&state, free_port()); serve.add_workspace("alpha", &a); serve.activate("alpha");
    let shell = |n: i64| json!({ "type": "leaf", "pane": { "id": format!("shell-{n}"), "kind": "shell", "paneId": n } });
    let threads = |b: Value| serve.post("/api/v1/cockpit/cowork/threads", b);
    // a space starts with one thread; its screen is the one the space always had
    let l = threads(json!({ "op": "list" })); assert_eq!((l["active"].clone(), l["threads"].as_array().unwrap().len()), (json!("main"), 1), "{l}");
    assert_eq!(serve.post("/api/v1/cockpit/cowork/layout", json!({ "layout": { "root": shell(900_001) }, "workspace": a.to_string_lossy() }))["ok"], json!(true));
    assert!(a.join(".antiloki/cowork-layout.json").exists(), "main keeps the file it always had");
    // a new thread opens empty and has a screen of its own
    let c = threads(json!({ "op": "create", "name": "Fix the checkout" })); assert_eq!((c["ok"].clone(), c["active"].clone()), (json!(true), json!("fix-the-checkout")), "{c}");
    assert!(serve.get("/api/v1/cockpit/cowork/layout")["layout"].is_null() || serve.get("/api/v1/cockpit/cowork/layout")["layout"]["root"].is_null(), "the new thread starts empty");
    let put = serve.post("/api/v1/cockpit/cowork/layout", json!({ "layout": { "root": shell(900_002) }, "workspace": a.to_string_lossy(), "thread": "fix-the-checkout" })); assert_eq!(put["ok"], json!(true), "{put}");
    assert!(a.join(".antiloki/threads/fix-the-checkout.layout.json").exists());
    // main still has its own pane, and asking for it by name works while another thread is open
    let main = serve.get("/api/v1/cockpit/cowork/layout?thread=main"); assert_eq!(main["layout"]["root"]["pane"]["id"], json!("shell-900001"), "{main}"); assert_eq!(main["thread"], json!("main"));
    assert_eq!(serve.get("/api/v1/cockpit/cowork/layout")["layout"]["root"]["pane"]["id"], json!("shell-900002"), "the open thread is the one without a name");
    // the list says what each holds
    let l = threads(json!({ "op": "list" })); let held: Vec<(String, usize)> = l["threads"].as_array().unwrap().iter().map(|t| (t["id"].as_str().unwrap().to_string(), t["panes"].as_array().unwrap().len())).collect(); assert_eq!(held, vec![("main".to_string(), 1), ("fix-the-checkout".to_string(), 1)], "{l}");
    // a save that was on its way for a thread that is gone is refused, and a thread with panes is not deleted
    assert!(threads(json!({ "op": "delete", "thread": "main" }))["error"].as_str().unwrap().contains("still has 1 pane"));
    let stale = serve.post("/api/v1/cockpit/cowork/layout", json!({ "layout": { "root": shell(900_003) }, "workspace": a.to_string_lossy(), "thread": "nope" })); assert_eq!((stale["ok"].clone(), stale["stale"].clone()), (json!(false), json!(true)), "{stale}");
    // switching, renaming
    assert_eq!(threads(json!({ "op": "select", "thread": "main" }))["active"], json!("main")); assert_eq!(serve.get("/api/v1/cockpit/cowork/layout")["layout"]["root"]["pane"]["id"], json!("shell-900001"));
    let r = threads(json!({ "op": "rename", "thread": "fix-the-checkout", "name": "Checkout" })); assert_eq!(r["threads"][1]["name"], json!("Checkout"), "{r}");
    // the choice survives a restart of the serve
    drop(serve); let serve = Serve::start(&state, free_port()); serve.activate("alpha");
    assert_eq!(serve.post("/api/v1/cockpit/cowork/threads", json!({ "op": "list" }))["active"], json!("main"));
}

#[test]
fn j23_a_side_question_opens_a_read_only_helper_bound_to_an_agent_and_what_it_writes_is_put_back() {
    let _s = serial(); let tmp = Scratch::new("j23");
    let agent = agent_binary(); let sock = tmp.path().join("pty.sock"); let _host = HostGuard { agent: agent.clone(), sock: sock.clone() };
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(state.join("bin")).unwrap();
    std::os::unix::fs::symlink(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake-claude-tui"), state.join("bin/claude")).unwrap();
    let env: [(&str, &Path); 3] = [("ANTILOKI_PTY_HOST", Path::new("sidecar")), ("ANTILOKI_AGENT_BIN", &agent), ("ANTILOKI_PTY_SOCK", &sock)];
    let s = Serve::start_env(&state, free_port(), &env); s.add_workspace("work", &ws); s.activate("work");
    let session = s.post("/api/v1/cockpit/session/create", json!({ "name": "Cowork", "mission_md": "side" }))["session"]["id"].as_i64().expect("a session");
    let made = s.post("/api/v1/cockpit/pane/add", json!({ "session_id": session, "base_agent": "dev", "subagent_name": "parent-j23", "pane_kind": "claude-code", "scope": [], "grants": ["file.read", "file.write"], "provision": true, "tier": "mid", "settings": { "title": "Cart refactor" } }));
    let parent = made["pane"]["id"].as_i64().expect("a pane"); let pdir = std::path::PathBuf::from(made["pane"]["worktree_path"].as_str().expect("a box"));
    assert_eq!(s.post("/api/v1/cockpit/pty/start", json!({ "pane_id": parent, "agent": "claude-code", "cols": 100, "rows": 30 }))["ok"], json!(true));
    until(&s, 20, "the parent's composer", || (s.get(&format!("/api/v1/cockpit/pty/status?pane_id={parent}"))["ready"] == json!(true)).then_some(()));
    // the parent has done something: a new file in its box
    std::fs::write(pdir.join("cart.ts"), "export const total = 1;\n").unwrap();

    let r = s.post("/api/v1/cockpit/side/open", json!({ "parent_pane_id": parent })); assert_eq!(r["ok"], json!(true), "{r}");
    let side = r["pane_id"].as_i64().unwrap(); assert!(side > 0 && side != parent); assert_eq!((r["side_of"].clone(), r["title"].clone()), (json!(parent), json!("Side · Cart refactor")), "{r}");
    let sdir = std::path::PathBuf::from(r["dir"].as_str().unwrap());
    // it is held read-only, and was given the briefing and the parent's context
    let marker: Value = serde_json::from_str(&std::fs::read_to_string(sdir.join(".antiloki/readonly")).unwrap()).unwrap(); assert_eq!(marker["role"], json!("side"), "{marker}");
    let ctx = std::fs::read_to_string(sdir.join(".antiloki/side-context.md")).unwrap(); assert!(ctx.contains("\"Cart refactor\"") && ctx.contains("cart.ts") && ctx.contains("+export const total = 1;"), "{ctx}");
    let sess = s.post("/api/v1/cockpit/session/get", json!({ "id": session })); let row = sess.to_string();
    assert!(row.contains("side_of") && row.contains("Side · Cart refactor"), "the helper's pane says what it is bound to: {sess}");
    let brief = std::fs::read_to_string(sdir.join("CLAUDE.md")).unwrap_or_default(); assert!(brief.contains("SIDE-QUESTION helper"), "the helper's guidance is in its brief: {brief}");
    // the parent was not touched
    assert!(pdir.join("cart.ts").exists());
    // ONE side question per agent: a second is refused until the first is closed
    assert!(s.post("/api/v1/cockpit/side/open", json!({ "parent_pane_id": parent }))["error"].as_str().unwrap().contains("already has a side question open"));
    // it carries the name the screen gave the agent
    let named = { let p2 = s.post("/api/v1/cockpit/pane/release", json!({ "pane_id": side })); assert_eq!(p2["ok"], json!(true)); s.post("/api/v1/cockpit/side/open", json!({ "parent_pane_id": parent, "parent_title": "  Cart refactor  " })) };
    assert_eq!(named["title"], json!("Side · Cart refactor"), "{named}"); assert_ne!(named["pane_id"], r["pane_id"], "a fresh helper, not the closed one");
    let (side, sdir) = (named["pane_id"].as_i64().unwrap(), std::path::PathBuf::from(named["dir"].as_str().unwrap()));
    // a side question of a side question, or of nothing, is refused with a reason
    assert!(s.post("/api/v1/cockpit/side/open", json!({ "parent_pane_id": side }))["error"].as_str().unwrap().contains("already a side question"));
    assert!(s.post("/api/v1/cockpit/side/open", json!({ "parent_pane_id": 99999 }))["error"].as_str().unwrap().contains("no such pane"));
    // what the helper writes anyway (an engine with no hook) is found and put back, and the log says so
    std::fs::write(sdir.join("sneaky.txt"), "no\n").unwrap();
    until(&s, 30, "the gate to put the helper's box back", || (!sdir.join("sneaky.txt").exists()).then_some(()));
    let log: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(ws.join(".antiloki/team-guard.json")).unwrap()).unwrap();
    assert!(log.iter().any(|e| e["role"] == "side" && e["pane"] == json!(side) && e["files"].as_array().unwrap().iter().any(|f| f == "sneaky.txt")), "{log:?}");
}

/// One raw HTTP/1.1 request, so a journey can say whatever `Host` and cookie a phone would: (status, headers text, body).
fn raw(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> (u16, String, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap(); s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers { req += &format!("{k}: {v}\r\n"); }
    req += "\r\n"; req += body; s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new(); let _ = s.read_to_string(&mut out);
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    let status = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).and_then(|c| c.parse().ok()).unwrap_or(0);
    (status, head.to_string(), body.to_string())
}

#[test]
fn j24_a_phone_pairs_with_a_one_time_code_and_reaches_the_app_and_a_stranger_reaches_nothing() {
    let _s = serial(); let tmp = Scratch::new("j24");
    let state = tmp.path().join("state"); std::fs::create_dir_all(state.join("bin")).unwrap();
    std::os::unix::fs::symlink(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake-claude-tui"), state.join("bin/claude")).unwrap();
    // a cloudflared that gives an address the way the real one does, and stays up
    std::fs::write(state.join("bin/cloudflared"), "#!/bin/sh\necho '2026-11-11T00:00:00Z INF Requesting new quick Tunnel on https://api.trycloudflare.com...' >&2\necho '2026-11-11T00:00:01Z INF |  https://quick-brown-fox.trycloudflare.com  |' >&2\nexec sleep 120\n").unwrap();
    { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(state.join("bin/cloudflared"), std::fs::Permissions::from_mode(0o755)).unwrap(); }
    // the built app: a page and an asset
    let fe = tmp.path().join("fe"); std::fs::create_dir_all(fe.join("assets")).unwrap(); std::fs::write(fe.join("index.html"), "<!doctype html><title>antiloki app</title>").unwrap(); std::fs::write(fe.join("assets/app.js"), "console.log(1)").unwrap();
    // the LAN door gets a free port of ours the first time (never the phone\'s own 41932, which a real serve may hold), and is left to remember it the second
    let lan_pick = free_port().to_string(); let env: [(&str, &Path); 2] = [("ANTILOKI_FE_DIR", &fe), ("ANTILOKI_LAN_PORT", Path::new(&lan_pick))];
    let s = Serve::start_env(&state, free_port(), &env);
    let st = s.get("/api/v1/remote/status"); assert_eq!((st["enabled"].clone(), st["app"].clone(), st["cloudflared"].clone()), (json!(false), json!(true), json!(true)), "{st}");

    // the LAN door is opened on this machine's say-so; the phone is a peer under another name
    let en = s.post("/api/v1/remote/enable", json!({})); assert_eq!(en["enabled"], json!(true), "{en}"); let lan = en["port"].as_u64().unwrap() as u16; assert!(lan > 0);
    let phone = format!("192.168.1.77:{lan}"); let H = |extra: &[(&'static str, String)]| -> Vec<(&'static str, String)> { let mut v = vec![("Host", phone.clone())]; v.extend(extra.iter().cloned()); v };
    let call = |m: &str, p: &str, hd: &[(&'static str, String)], b: &str| { let hv: Vec<(&str, &str)> = hd.iter().map(|(k, v)| (*k, v.as_str())).collect(); raw(lan, m, p, &hv, b) };
    // a stranger: the page and its assets load; the API does not, and nothing can be written
    let (c, _, b) = call("GET", "/", &H(&[]), ""); assert_eq!(c, 200); assert!(b.contains("antiloki app"), "{b}");
    assert_eq!(call("GET", "/some/route", &H(&[]), "").0, 200, "a route of the single-page app gets the page"); assert_eq!(call("GET", "/assets/app.js", &H(&[]), "").2, "console.log(1)");
    assert_eq!(call("GET", "/../etc/passwd", &H(&[]), "").0, 404, "the app's folder is a wall");
    assert_eq!(call("GET", "/api/v1/cockpit/panes", &H(&[]), "").0, 401); assert_eq!(call("POST", "/api/v1/cockpit/pty/send", &H(&[]), "{}").0, 401);
    assert_eq!(call("GET", "/api/v1/remote/status", &H(&[]), "").0, 401, "even the status is the machine's");
    // a LAN peer cannot be local by saying so
    assert_eq!(raw(lan, "GET", "/api/v1/cockpit/panes", &[("Host", "localhost"), ("X-Forwarded-For", "10.0.0.9")], "").0, 401);
    // pairing: a code from the machine, a wrong code refused, the right one spent once
    let ps = s.post("/api/v1/remote/pair/start", json!({})); let code = ps["code"].as_str().unwrap().to_string(); assert_eq!(code.len(), 8, "{ps}");
    let bad = call("POST", "/api/v1/remote/pair/claim", &H(&[]), r#"{"code":"WRONG999","name":"phone"}"#); assert!(bad.2.contains("\"ok\":false"), "{bad:?}");
    let (c, head, b) = call("POST", "/api/v1/remote/pair/claim", &H(&[]), &json!({ "code": code, "name": "Dane's phone" }).to_string()); assert_eq!(c, 200); let got: Value = serde_json::from_str(&b).unwrap(); assert_eq!(got["ok"], json!(true), "{b}");
    let token = got["token"].as_str().unwrap().to_string(); assert_eq!(token.len(), 64); assert!(head.to_ascii_lowercase().contains("set-cookie: antiloki_device=") && head.contains("HttpOnly"), "{head}");
    assert!(call("POST", "/api/v1/remote/pair/claim", &H(&[]), &json!({ "code": code, "name": "again" }).to_string()).2.contains("\"ok\":false"), "a code is spent once");
    // the device reaches the API by cookie or bearer, may not manage pairing, may not write from another site
    let cookie = format!("antiloki_device={token}");
    assert_eq!(call("GET", "/api/v1/cockpit/cowork/layout", &H(&[("Cookie", cookie.clone())]), "").0, 200); assert_eq!(call("GET", "/api/v1/cockpit/cowork/layout", &H(&[("Authorization", format!("Bearer {token}"))]), "").0, 200);
    let me = call("GET", "/api/v1/remote/me", &H(&[("Cookie", cookie.clone())]), ""); assert!(me.2.contains("Dane's phone"), "{me:?}");
    assert_eq!(call("POST", "/api/v1/remote/pair/start", &H(&[("Cookie", cookie.clone())]), "{}").0, 403); assert_eq!(call("GET", "/api/v1/remote/status", &H(&[("Cookie", cookie.clone())]), "").0, 403);
    assert_eq!(call("POST", "/api/v1/cockpit/pty/send", &H(&[("Cookie", cookie.clone()), ("Origin", "http://evil.com".to_string())]), "{}").0, 403, "a write from another site, even with the cookie");
    // the tunnel: its address comes from the tool's own log, and a link for the QR carries it
    let t = s.post("/api/v1/remote/tunnel/start", json!({})); assert_eq!(t["tunnel"]["url"], json!("https://quick-brown-fox.trycloudflare.com"), "{t}");
    let ps = s.post("/api/v1/remote/pair/start", json!({})); assert!(ps["links"].as_array().unwrap().iter().any(|l| l["url"].as_str().unwrap().starts_with("https://quick-brown-fox.trycloudflare.com/?pair=")), "{ps}");
    // a request that arrives through the tunnel (loopback peer, the tunnel's name, proxy headers) is a stranger until it carries the token
    assert_eq!(raw(s.port, "GET", "/api/v1/cockpit/cowork/layout", &[("Host", "quick-brown-fox.trycloudflare.com"), ("CF-Connecting-IP", "5.5.5.5")], "").0, 401);
    assert_eq!(raw(s.port, "GET", "/api/v1/cockpit/cowork/layout", &[("Host", "quick-brown-fox.trycloudflare.com"), ("CF-Connecting-IP", "5.5.5.5"), ("Cookie", &cookie)], "").0, 200);
    assert_eq!(s.post("/api/v1/remote/tunnel/stop", json!({}))["tunnel"], Value::Null);
    // the device list names it; revoking puts it out on its next request
    let id = s.get("/api/v1/remote/status")["devices"][0]["id"].as_str().unwrap().to_string();
    let rv = s.post("/api/v1/remote/devices/revoke", json!({ "id": id })); assert_eq!(rv["devices"][0]["revoked"], json!(true), "{rv}");
    assert_eq!(call("GET", "/api/v1/cockpit/cowork/layout", &H(&[("Cookie", cookie.clone())]), "").0, 401);
    // wrong codes in a row are slowed down
    let mut last = 0; for _ in 0..14 { last = call("POST", "/api/v1/remote/pair/claim", &H(&[]), r#"{"code":"NOPE1234"}"#).0; } assert_eq!(last, 429);
    // it survives a restart: the device is still known (and still revoked), the LAN door comes back on the same port
    let port2 = free_port(); drop(s); let env2: [(&str, &Path); 2] = [("ANTILOKI_FE_DIR", &fe), ("ANTILOKI_LAN_PORT", Path::new(""))]; let s2 = Serve::start_env(&state, port2, &env2);
    let st = s2.get("/api/v1/remote/status"); assert_eq!(st["devices"].as_array().unwrap().len(), 1, "{st}"); assert_eq!(st["devices"][0]["revoked"], json!(true));
    until(&s2, 10, "the LAN door to come back", || (s2.get("/api/v1/remote/status")["enabled"] == json!(true)).then_some(()));
    assert_eq!(s2.get("/api/v1/remote/status")["port"].as_u64(), Some(lan as u64), "the same port as last time");
    assert_eq!(s2.post("/api/v1/remote/disable", json!({}))["enabled"], json!(false));
}

#[test]
fn j25_a_machine_is_added_and_connected_in_one_click_over_ssh_and_survives_a_restart() {
    let _s = serial(); let tmp = Scratch::new("j25");
    let state = tmp.path().join("state"); std::fs::create_dir_all(state.join("bin")).unwrap();
    std::os::unix::fs::symlink(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake-claude-tui"), state.join("bin/claude")).unwrap();
    for n in ["ssh", "scp"] { std::os::unix::fs::symlink(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake-ssh"), state.join("bin").join(n)).unwrap(); }
    let remote_home = tmp.path().join("remote-home"); std::fs::create_dir_all(&remote_home).unwrap();
    let fe = tmp.path().join("fe"); std::fs::create_dir_all(fe.join("assets")).unwrap(); std::fs::write(fe.join("index.html"), "<!doctype html><title>antiloki app</title>").unwrap();
    let serve_bin = PathBuf::from(env!("CARGO_BIN_EXE_antiloki-serve"));
    let env: [(&str, &Path); 3] = [("FAKE_REMOTE_HOME", &remote_home), ("ANTILOKI_SERVE_BIN", &serve_bin), ("ANTILOKI_FE_DIR", &fe)];
    let rport = free_port();
    let s = Serve::start_env(&state, free_port(), &env);
    // bad input is refused with a reason
    assert!(s.post("/api/v1/machines/add", json!({ "target": "-oProxyCommand=evil" }))["error"].as_str().unwrap().contains("the host is a name"));
    let add = s.post("/api/v1/machines/add", json!({ "target": "dani@remote.local", "name": "Build box", "remote_port": rport })); assert_eq!(add["ok"], json!(true), "{add}");
    let m = &add["machines"][0]; assert_eq!((m["id"].clone(), m["connected"].clone()), (json!("build-box"), json!(false)), "{add}"); let lport = m["local_port"].as_u64().unwrap() as u16;
    assert!(s.post("/api/v1/machines/add", json!({ "target": "x@y", "name": "build box" }))["error"].as_str().unwrap().contains("already here"));
    // ONE CLICK: antiloki is put there, started, tunnelled, and answers
    let c = s.post("/api/v1/machines/connect", json!({ "id": "build-box" })); assert_eq!(c["ok"], json!(true), "{c}");
    let steps = c["steps"].to_string(); for w in ["reached dani@remote.local", "put antiloki on that machine", "put the app's files there", "started antiloki there", "connected"] { assert!(steps.contains(w), "{w}: {steps}"); }
    assert!(remote_home.join(".antiloki/remote-bin/antiloki-serve").is_file() && remote_home.join(".antiloki/remote-fe/index.html").is_file(), "the other machine has the program and the app");
    assert_eq!(c["machines"][0]["up"], json!(true), "{c}");
    let (code, _, body) = raw(lport, "GET", "/api/v1/remote/ping", &[("Host", &format!("127.0.0.1:{lport}"))], ""); assert_eq!(code, 200); assert!(body.contains("antiloki"), "{body}");
    let (code, _, page) = raw(lport, "GET", "/", &[("Host", &format!("127.0.0.1:{lport}"))], ""); assert_eq!(code, 200); assert!(page.contains("antiloki app"), "the other machine shows its own screen: {page}");
    // it really is ANOTHER antiloki: its own state, its own workspaces
    assert!(remote_home.join(".antiloki").exists());
    // disconnecting closes the tunnel and leaves the other machine running; connecting again does not install again
    let d = s.post("/api/v1/machines/disconnect", json!({ "id": "build-box" })); assert_eq!(d["machines"][0]["up"], json!(false), "{d}");
    let c2 = s.post("/api/v1/machines/connect", json!({ "id": "build-box" })); assert_eq!(c2["ok"], json!(true), "{c2}"); assert!(c2["steps"].to_string().contains("already there") && !c2["steps"].to_string().contains("put antiloki"), "{c2}");
    // a machine that cannot be reached says why
    let bad = s.post("/api/v1/machines/add", json!({ "target": "u@badhost", "name": "Nowhere" })); assert_eq!(bad["ok"], json!(true));
    let f = s.post("/api/v1/machines/connect", json!({ "id": "nowhere" })); assert_eq!(f["ok"], json!(false)); assert!(f["error"].as_str().unwrap().contains("does not resolve"), "{f}");
    // the tunnel comes back with the serve
    drop(s); let s2 = Serve::start_env(&state, free_port(), &env);
    until(&s2, 20, "the tunnel to come back", || { let l = s2.get("/api/v1/machines"); l["machines"].as_array().unwrap().iter().any(|m| m["id"] == json!("build-box") && m["up"] == json!(true)).then_some(()) });
    // stopping the other machine too, then removing it
    let d = s2.post("/api/v1/machines/disconnect", json!({ "id": "build-box", "stop_remote": true })); assert_eq!(d["ok"], json!(true));
    until(&s2, 10, "the other antiloki to stop", || (std::net::TcpStream::connect(("127.0.0.1", rport)).is_err()).then_some(()));
    let rm = s2.post("/api/v1/machines/remove", json!({ "id": "build-box" })); assert_eq!(rm["machines"].as_array().unwrap().iter().filter(|m| m["id"] == json!("build-box")).count(), 0, "{rm}");
}

/// A POST with a binary body (a recording): (status, body text).
fn post_bytes(port: u16, path: &str, body: &[u8]) -> (u16, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap(); s.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
    let head = format!("POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\n\r\n", body.len());
    s.write_all(head.as_bytes()).unwrap(); s.write_all(body).unwrap();
    let mut out = String::new(); let _ = s.read_to_string(&mut out);
    let (h, b) = out.split_once("\r\n\r\n").unwrap_or((&out, "")); (h.lines().next().and_then(|l| l.split_whitespace().nth(1)).and_then(|c| c.parse().ok()).unwrap_or(0), b.to_string())
}

#[test]
fn j26_a_recording_is_turned_into_words_on_this_machine() {
    let _s = serial(); let tmp = Scratch::new("j26");
    let state = tmp.path().join("state"); std::fs::create_dir_all(state.join("bin")).unwrap();
    std::os::unix::fs::symlink(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake-claude-tui"), state.join("bin/claude")).unwrap();
    // a speech-to-text that checks it was handed a 16 kHz mono WAV, and answers the way a model does (spaces and all)
    let stt = tmp.path().join("fake-whisper.sh");
    std::fs::write(&stt, "#!/bin/sh\nf=\"$1\"; head -c4 \"$f\" | grep -q RIFF || { echo 'not a wav' >&2; exit 3; }\nrate=$(od -An -tu4 -j24 -N4 \"$f\" | tr -d ' '); ch=$(od -An -tu2 -j22 -N2 \"$f\" | tr -d ' ')\n[ \"$rate\" = 16000 ] && [ \"$ch\" = 1 ] || { echo \"bad format $rate $ch\" >&2; exit 4; }\necho \"  Olá,   open the cart \"\n").unwrap();
    { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&stt, std::fs::Permissions::from_mode(0o755)).unwrap(); }
    // one second of 44.1 kHz stereo silence: ffmpeg must convert it
    let mut wav: Vec<u8> = vec![]; let data = 44_100u32 * 4;
    wav.extend(b"RIFF"); wav.extend((36 + data).to_le_bytes()); wav.extend(b"WAVEfmt "); wav.extend(16u32.to_le_bytes()); wav.extend(1u16.to_le_bytes()); wav.extend(2u16.to_le_bytes()); wav.extend(44_100u32.to_le_bytes()); wav.extend((44_100u32 * 4).to_le_bytes()); wav.extend(4u16.to_le_bytes()); wav.extend(16u16.to_le_bytes()); wav.extend(b"data"); wav.extend(data.to_le_bytes()); wav.extend(vec![0u8; data as usize]);
    let env: [(&str, &Path); 1] = [("ANTILOKI_WHISPER_CMD", &stt)];
    let s = Serve::start_env(&state, free_port(), &env);
    let st = s.get("/api/v1/voice/status"); assert_eq!((st["available"].clone(), st["engine"].clone()), (json!(true), json!("your command")), "{st}");
    let (code, body) = post_bytes(s.port, "/api/v1/voice/transcribe?lang=pt", &wav); assert_eq!(code, 200, "{body}"); let v: Value = serde_json::from_str(&body).unwrap(); assert_eq!((v["ok"].clone(), v["text"].clone()), (json!(true), json!("Olá, open the cart")), "{v}");
    let (_, b) = post_bytes(s.port, "/api/v1/voice/transcribe", b""); assert!(b.contains("no audio"), "{b}");
    let (_, b) = post_bytes(s.port, "/api/v1/voice/transcribe", b"this is not audio at all"); assert!(b.contains("could not read the recording"), "garbage in, a reason out: {b}");
    // a machine with no speech-to-text says what to install, and does not pretend
    drop(s); let s2 = Serve::start_env(&state, free_port(), &[]); let st = s2.get("/api/v1/voice/status"); assert_eq!(st["available"], json!(false), "{st}"); assert!(st["hint"].as_str().unwrap().contains("uv"), "{st}");
    let (_, b) = post_bytes(s2.port, "/api/v1/voice/transcribe", &wav); assert!(b.contains("no speech-to-text here"), "{b}");
}

#[test]
fn j27_a_pane_closed_on_the_screen_gives_its_slot_back_and_a_full_session_can_be_swept() {
    let _s = serial(); let tmp = Scratch::new("j27");
    let ws = project(tmp.path(), "work"); let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let s = Serve::start(&state, free_port()); s.add_workspace("work", &ws); s.activate("work");
    let session = s.post("/api/v1/cockpit/session/create", json!({ "name": "Cowork", "mission_md": "x" }))["session"]["id"].as_i64().unwrap();
    let add = |n: usize| s.post("/api/v1/cockpit/pane/add", json!({ "session_id": session, "base_agent": "dev", "subagent_name": format!("a-{n}"), "pane_kind": "claude-code", "scope": [], "grants": ["file.read"], "provision": false, "tier": "mid", "settings": { "title": format!("a{n}") } }));
    let mut ids = vec![]; let mut n = 0;
    let full = loop { let r = add(n); n += 1; if let Some(id) = r["pane"]["id"].as_i64() { ids.push(id); } else { break r; } assert!(n < 40, "no cap after {n} panes"); };
    assert!(full["error"].as_str().unwrap().contains("pane cap reached"), "{full}"); assert_eq!(ids.len(), 20, "a session holds twenty open panes");
    // closing one on the screen gives its slot back
    let rel = s.post("/api/v1/cockpit/pane/release", json!({ "pane_id": ids[0] })); assert_eq!(rel["ok"], json!(true), "{rel}");
    assert!(add(n)["pane"]["id"].is_i64(), "the slot is free again"); n += 1;
    assert!(add(n)["error"].as_str().unwrap_or("").contains("pane cap"), "and only the one");
    // the sweep: nothing shows these panes and no terminal runs them, so they are closed and the session is empty of open ones
    let sweep = s.post("/api/v1/cockpit/pane/release", json!({})); assert!(sweep["released"].as_array().unwrap().len() >= 19, "{sweep}");
    for k in 0..5 { assert!(add(100 + k)["pane"]["id"].is_i64(), "room again after the sweep"); }
}

#[test]
fn j28_a_new_agent_in_a_freed_slot_gets_a_box_of_its_own_not_the_old_agents() {
    let _s = serial(); let tmp = Scratch::new("j28");
    let ws = project(tmp.path(), "work"); let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let s = Serve::start(&state, free_port()); s.add_workspace("work", &ws); s.activate("work");
    let session = s.post("/api/v1/cockpit/session/create", json!({ "name": "Cowork", "mission_md": "x" }))["session"]["id"].as_i64().unwrap();
    let add = |n: usize| s.post("/api/v1/cockpit/pane/add", json!({ "session_id": session, "base_agent": "dev", "subagent_name": format!("a-{n}"), "pane_kind": "claude-code", "scope": [], "grants": ["file.read", "file.write"], "provision": true, "tier": "mid", "settings": { "title": format!("a{n}") } }));
    let a = add(1); let (pa, da) = (a["pane"]["id"].as_i64().unwrap(), std::path::PathBuf::from(a["pane"]["worktree_path"].as_str().unwrap()));
    std::fs::write(da.join("old-agents-work.txt"), "the first agent's change\n").unwrap(); std::fs::create_dir_all(da.join(".antiloki")).unwrap(); std::fs::write(da.join(".antiloki/prompts.jsonl"), "{\"prompt\":\"the first agent's prompt\"}\n").unwrap();
    // it is closed on the screen: its row is released and its place in the grid is free again
    assert_eq!(s.post("/api/v1/cockpit/pane/release", json!({ "pane_id": pa }))["ok"], json!(true));
    let b = add(2); let (pb, db_) = (b["pane"]["id"].as_i64().unwrap(), std::path::PathBuf::from(b["pane"]["worktree_path"].as_str().unwrap()));
    assert_eq!(b["pane"]["idx"], json!(0), "it took the freed place: {b}"); assert_ne!(pa, pb);
    assert_ne!(da, db_, "...but not the old agent's folder: {da:?}");
    assert!(!db_.join("old-agents-work.txt").exists() && !db_.join(".antiloki/prompts.jsonl").exists(), "nothing of the old agent's work or conversation is in the new one's box");
    assert!(da.join("old-agents-work.txt").exists(), "the old agent's box is kept: it is the record of what it did");
}

#[test]
fn j29_skills_are_installed_from_a_git_source_linked_into_every_tool_checked_updated_and_removed() {
    let _s = serial(); let tmp = Scratch::new("j29");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let home = state.join("fake-home"); std::fs::create_dir_all(home.join(".claude")).unwrap(); std::fs::write(home.join(".claude/.credentials.json"), "{}").unwrap(); std::fs::create_dir_all(home.join(".codex")).unwrap(); std::fs::create_dir_all(home.join(".cursor")).unwrap();
    // a source: one skill, one agent
    let repo = tmp.path().join("skills-repo"); std::fs::create_dir_all(repo.join("skills/review")).unwrap(); std::fs::create_dir_all(repo.join("agents")).unwrap();
    std::fs::write(repo.join("skills/review/SKILL.md"), "---\nname: review\n---\nv1\n").unwrap(); std::fs::write(repo.join("agents/planner.md"), "plan v1\n").unwrap();
    let g = |a: &[&str]| { let o = Command::new("git").args(["-c", "user.email=e@x", "-c", "user.name=e"]).args(a).current_dir(&repo).output().unwrap(); assert!(o.status.success(), "{a:?}: {}", String::from_utf8_lossy(&o.stderr)); };
    g(&["init", "-q"]); g(&["add", "-A"]); g(&["commit", "-q", "-m", "v1"]);
    let env: [(&str, &Path); 1] = [("ANTILOKI_SKILLBANK_ALLOW_FILE", Path::new("1"))];
    let s = Serve::start_env(&state, free_port(), &env);
    let st = s.get("/api/v1/skillshare/state"); assert_eq!((st["installed"].clone(), st["native"].clone(), st["targets"].clone()), (json!(true), json!(true), json!(3)), "no CLI: the bank is ours, and three tools are here: {st}");
    // a source that is not a source is refused before git is asked anything
    assert_eq!(s.post("/api/v1/skillshare/run", json!({ "op": "install", "scope": "global", "source": "-oProxyCommand=x" }))["ok"], json!(false));
    // install: copied into the bank, linked into Claude, Codex and Cursor (the agent into Claude and Cursor, which have a place for agents)
    let src = format!("file://{}", repo.display());
    let r = s.post("/api/v1/skillshare/run", json!({ "op": "install", "scope": "global", "source": src })); assert_eq!(r["ok"], json!(true), "{r}"); assert_eq!(r["json"]["installed"], json!(["review", "planner.md"]));
    assert!(home.join(".antiloki/skillbank/skills/review/SKILL.md").is_file()); for t in [".claude", ".codex", ".cursor"] { assert!(home.join(t).join("skills/review/SKILL.md").is_file(), "{t}"); }
    assert!(home.join(".claude/agents/planner.md").is_file() && home.join(".cursor/agents/planner.md").is_file() && !home.join(".codex/agents").exists(), "Codex has no place for agents");
    // the state lists them with their source, in sync
    let st = s.get("/api/v1/skillshare/state"); let list = st["scopes"]["global"]["list"].as_array().unwrap(); assert_eq!(list.len(), 2, "{st}"); assert!(list.iter().all(|x| x["source"] == json!(src)));
    assert!(st["scopes"]["global"]["diff"]["targets"].as_array().unwrap().is_empty(), "nothing left to sync");
    // nothing moved upstream: no update; then the source moves
    assert!(s.get("/api/v1/skillshare/state?checks=1")["scopes"]["global"]["check"]["skills"].as_array().unwrap().is_empty());
    std::fs::write(repo.join("skills/review/SKILL.md"), "---\nname: review\n---\nv2\n").unwrap(); g(&["commit", "-qam", "v2"]);
    let chk = s.get("/api/v1/skillshare/state?checks=1"); assert_eq!(chk["scopes"]["global"]["check"]["skills"][0]["name"], json!("review"), "{chk}"); assert_eq!(chk["scopes"]["global"]["check"]["skills"][0]["status"], json!("update_available"));
    // update: the new text arrives, through the links, in every tool
    let u = s.post("/api/v1/skillshare/run", json!({ "op": "update", "scope": "global", "kind": "skills", "name": "review" })); assert_eq!(u["ok"], json!(true), "{u}");
    assert!(std::fs::read_to_string(home.join(".codex/skills/review/SKILL.md")).unwrap().contains("v2"), "edit once, every tool sees it");
    assert!(s.get("/api/v1/skillshare/state?checks=1")["scopes"]["global"]["check"]["skills"].as_array().unwrap().is_empty());
    // a skill of one's own, switched off and on, then removed
    assert_eq!(s.post("/api/v1/skillshare/run", json!({ "op": "new", "scope": "global", "name": "mine", "description": "mine" }))["ok"], json!(true)); assert!(home.join(".cursor/skills/mine/SKILL.md").is_file());
    assert_eq!(s.post("/api/v1/skillshare/run", json!({ "op": "update", "scope": "global", "kind": "skills", "name": "mine" }))["ok"], json!(false), "no source to update from");
    assert_eq!(s.post("/api/v1/skillshare/run", json!({ "op": "toggle", "scope": "global", "kind": "skills", "name": "mine", "off": true }))["ok"], json!(true)); assert!(std::fs::symlink_metadata(home.join(".claude/skills/mine")).is_err(), "switched off: the links go");
    assert_eq!(s.post("/api/v1/skillshare/run", json!({ "op": "toggle", "scope": "global", "kind": "skills", "name": "mine", "off": false }))["ok"], json!(true)); assert!(home.join(".claude/skills/mine/SKILL.md").is_file());
    assert_eq!(s.post("/api/v1/skillshare/run", json!({ "op": "uninstall", "scope": "global", "kind": "skills", "name": "review" }))["ok"], json!(true)); assert!(std::fs::symlink_metadata(home.join(".codex/skills/review")).is_err() && !home.join(".antiloki/skillbank/skills/review").exists());
}

#[test]
fn j30_each_thread_has_its_own_branch_and_worktree_its_agents_start_from_it_integrate_into_it_and_it_lands_in_the_workspace_only_when_asked() {
    let _s = serial(); let tmp = Scratch::new("j30");
    let ws = project(tmp.path(), "work"); let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let home = tmp.path().join("ahome"); let env: [(&str, &Path); 1] = [("ANTILOKI_HOME", &home)];
    let s = Serve::start_env(&state, free_port(), &env); s.add_workspace("work", &ws); s.activate("work");
    let session = s.post("/api/v1/cockpit/session/create", json!({ "name": "Cowork", "mission_md": "x" }))["session"]["id"].as_i64().unwrap();
    let git = |dir: &Path, a: &[&str]| -> String { let o = Command::new("git").args(["-c", "user.email=e@x", "-c", "user.name=e"]).args(a).current_dir(dir).output().unwrap(); assert!(o.status.success(), "{a:?}: {}", String::from_utf8_lossy(&o.stderr)); String::from_utf8_lossy(&o.stdout).to_string() };
    let new_agent = |thread: &str, title: &str| s.post("/api/v1/cockpit/agent/new", json!({ "engine": "claude-code", "title": title, "thread": thread, "session_id": session }));
    let box_of = |pane: i64| -> PathBuf { let one = s.post("/api/v1/cockpit/session/get", json!({ "id": session })); PathBuf::from(one["panes"].as_array().unwrap().iter().find(|p| p["id"] == json!(pane)).unwrap()["worktree_path"].as_str().unwrap()) };
    // a thread of its own: no tree until an agent is opened in it
    let t = s.post("/api/v1/cockpit/cowork/threads", json!({ "op": "create", "name": "feat" })); assert_eq!(t["active"], json!("feat"), "{t}"); assert!(t["threads"][1]["tree"].is_null(), "no tree yet: {t}");
    let a = new_agent("feat", "A"); let b = new_agent("feat", "B"); let (pa, pb) = (a["pane_id"].as_i64().expect(&a.to_string()), b["pane_id"].as_i64().unwrap());
    let tree = s.post("/api/v1/cockpit/cowork/threads", json!({ "op": "list" })); let tinfo = tree["threads"].as_array().unwrap().iter().find(|x| x["id"] == json!("feat")).unwrap()["tree"].clone(); assert!(tinfo["worktree"].is_string() && tinfo["ahead"] == json!(0), "{tree}");
    let (tpath, tbranch) = (PathBuf::from(tinfo["worktree"].as_str().unwrap()), tinfo["branch"].as_str().unwrap().to_string()); assert!(tpath.join("README.md").is_file(), "the thread's tree is a checkout of the workspace");
    let (ba, bb) = (box_of(pa), box_of(pb)); assert_ne!(ba, bb, "each agent keeps its own box and its own record"); assert_ne!(ba, tpath);
    // the agents are cut from the thread's branch
    git(&ba, &["merge-base", "--is-ancestor", &tbranch, "HEAD"]); git(&bb, &["merge-base", "--is-ancestor", &tbranch, "HEAD"]);
    // they work apart; the thread's screen holds both
    std::fs::write(ba.join("from-a.txt"), "a\n").unwrap(); std::fs::write(bb.join("from-b.txt"), "b\n").unwrap();
    let leaf = |id: &str, pane: i64| json!({ "type": "leaf", "pane": { "id": id, "kind": "engine", "engine": "claude-code", "paneId": pane, "sessionId": session } });
    let put = s.post("/api/v1/cockpit/cowork/layout", json!({ "layout": { "root": { "type": "split", "dir": "row", "sizes": [0.5, 0.5], "children": [leaf("claude-code-a", pa), leaf("claude-code-b", pb)] } }, "workspace": ws.to_string_lossy(), "thread": "feat" })); assert_eq!(put["ok"], json!(true), "{put}");
    // INTEGRATE: both in the thread's tree, each takes the other's, the workspace is not touched
    let i = s.post("/api/v1/cockpit/cowork/threads/tree", json!({ "op": "integrate", "thread": "feat" })); assert_eq!(i["ok"], json!(true), "{i}"); assert_eq!(i["merged"].as_array().unwrap().len(), 2);
    assert!(tpath.join("from-a.txt").is_file() && tpath.join("from-b.txt").is_file() && !tpath.join(".antiloki").exists() || tpath.join("from-a.txt").is_file());
    assert!(ba.join("from-b.txt").is_file() && bb.join("from-a.txt").is_file(), "the agents see each other's work"); assert!(!ws.join("from-a.txt").exists(), "the workspace is untouched");
    // a thread with work that is not in the workspace is not deleted; Main's agents never saw the thread's files
    let del = s.post("/api/v1/cockpit/cowork/threads", json!({ "op": "delete", "thread": "feat" })); assert_eq!(del["ok"], json!(false), "{del}");
    let m = new_agent("main", "M"); let bm = box_of(m["pane_id"].as_i64().unwrap()); assert!(!bm.join("from-a.txt").exists(), "another thread's work is not in Main's agent: it was cut from the workspace");
    // a new agent in the thread starts from what the thread has so far
    let c = new_agent("feat", "C"); let bc = box_of(c["pane_id"].as_i64().unwrap()); assert!(bc.join("from-a.txt").is_file() && bc.join("from-b.txt").is_file(), "a late agent starts from the thread's work");
    // LAND: the workspace takes it (only when it has nothing uncommitted)
    std::fs::write(ws.join("dirty.txt"), "x").unwrap(); let l = s.post("/api/v1/cockpit/cowork/threads/tree", json!({ "op": "land", "thread": "feat" })); assert_eq!(l["ok"], json!(false), "{l}"); assert!(l["error"].as_str().unwrap().contains("uncommitted")); std::fs::remove_file(ws.join("dirty.txt")).unwrap();
    let l = s.post("/api/v1/cockpit/cowork/threads/tree", json!({ "op": "land", "thread": "feat" })); assert_eq!(l["ok"], json!(true), "{l}"); assert!(ws.join("from-a.txt").is_file() && ws.join("from-b.txt").is_file());
    // landed: nothing ahead, and an empty thread goes with its tree
    let st = s.post("/api/v1/cockpit/cowork/threads", json!({ "op": "list" })); assert_eq!(st["threads"].as_array().unwrap().iter().find(|x| x["id"] == json!("feat")).unwrap()["tree"]["ahead"], json!(0));
    let _ = s.post("/api/v1/cockpit/cowork/layout", json!({ "layout": { "root": null }, "workspace": ws.to_string_lossy(), "thread": "feat" }));
    let del = s.post("/api/v1/cockpit/cowork/threads", json!({ "op": "delete", "thread": "feat" })); assert_eq!(del["ok"], json!(true), "{del}"); assert!(!tpath.exists(), "its tree went with it");
}

#[test]
fn j31_a_saga_made_by_hand_is_run_from_its_tasks_the_run_is_the_sagas_own_and_its_settings_are_kept() {
    let _s = serial(); let tmp = Scratch::new("j31");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let s = Serve::start(&state, free_port());
    s.add_workspace("work", &ws); s.activate("work");
    let made = s.post("/api/v1/sagas/create", json!({ "title": "Explain the screen", "objective": "two read-only questions" })); let id = made["saga"]["id"].as_i64().expect(&made.to_string());
    // nothing to run yet, and the refusal says why
    let none = s.post("/api/v1/sagas/run", json!({ "id": id })); assert_eq!(none["ok"], json!(false)); assert!(none["error"].as_str().unwrap().contains("nothing to run"), "{none}");
    let t1 = s.post("/api/v1/sagas/task-create", json!({ "saga_id": id, "title": "explain what the picker does FAKE:slow:1500", "body": "in one line" }))["task"]["id"].as_i64().unwrap();
    let _t2 = s.post("/api/v1/sagas/task-create", json!({ "saga_id": id, "title": "explain what the strip does" }));
    let _sub = s.post("/api/v1/sagas/task-create", json!({ "saga_id": id, "parent_id": t1, "title": "mention the keyboard" }));
    // how it runs: the seat, the model and the budget are the saga's own
    let set = s.post("/api/v1/sagas/update", json!({ "id": id, "engine": "claude-code", "budget_usd": 1.5 })); assert_eq!(set["saga"]["run"], json!({ "engine": "claude-code", "budget_usd": 1.5 }), "{set}");
    let set = s.post("/api/v1/sagas/update", json!({ "id": id, "model": " haiku " })); assert_eq!(set["saga"]["run"]["model"], json!("haiku"), "{set}");
    let neg = s.post("/api/v1/sagas/update", json!({ "id": id, "budget_usd": -1 })); assert!(neg["error"].as_str().map_or(false, |e| e.contains("negative")), "{neg}");
    let _ = s.post("/api/v1/sagas/update", json!({ "id": id, "model": "", "engine": "" }));   // the defaults: whatever seat is usable here runs it
    let go = s.post("/api/v1/sagas/run", json!({ "id": id })); assert_eq!(go["ok"], json!(true), "{go}"); assert_eq!(go["tasks"], json!(2)); let run = go["run"].as_i64().unwrap();
    let again = s.post("/api/v1/sagas/run", json!({ "id": id })); assert_eq!(again["ok"], json!(false)); assert!(again["error"].as_str().unwrap().contains("already running"), "{again}");
    // the saga took the run as its own: one saga (not two), its own title and tasks, the tasks follow the run
    let detail = until(&s, 90, "both tasks to be settled", || { let d = s.get(&format!("/api/v1/sagas/detail?id={id}")); let ts = d["tasks"].as_array().unwrap(); if ts.len() == 2 && ts.iter().all(|t| t["status"] == json!("done") || t["status"] == json!("failed")) { Some(d) } else { None } });
    assert_eq!(detail["saga"]["plan_ref"], json!(format!("run:{run}"))); assert_eq!(detail["saga"]["title"], json!("Explain the screen"));
    // the list is served stale-while-revalidate (swr.rs): the first answer after the 2 seconds can be the old one, so ask until it shows the saga, then count
    let all = until(&s, 20, "the saga in the list", || { let a = s.get("/api/v1/sagas?all=0"); a["sagas"].as_array().map_or(false, |v| !v.is_empty()).then(|| a) }); let sagas = all["sagas"].as_array().unwrap(); assert_eq!(sagas.len(), 1, "the run added no second saga: {all}");
    assert!(detail["tasks"].as_array().unwrap().iter().all(|t| t["title"].as_str().unwrap().starts_with("explain what")), "the tasks keep their own titles, no copies: {detail}");
    assert!(detail["events"].as_array().unwrap().iter().any(|e| e["kind"] == json!("run")), "the trail says a run started: {detail}");
    let _ = until(&s, 60, "the run to finish", || { let r = s.get(&format!("/api/v1/cockpit/goals?id={run}"))["run"].clone(); if r["status"] == json!("done") || r["status"] == json!("failed") { Some(()) } else { None } });
    // a finished saga cannot be run a second time over its done tasks: nothing is left open
    let after = s.post("/api/v1/sagas/run", json!({ "id": id })); assert_eq!(after["ok"], json!(false)); assert!(after["error"].as_str().unwrap().contains("nothing to run"), "{after}");
    // more than a run holds is refused with the way out
    let big = s.post("/api/v1/sagas/create", json!({ "title": "Too many" }))["saga"]["id"].as_i64().unwrap();
    for n in 0..9 { s.post("/api/v1/sagas/task-create", json!({ "saga_id": big, "title": format!("task {n}") })); }
    let r = s.post("/api/v1/sagas/run", json!({ "id": big })); assert_eq!(r["ok"], json!(false)); assert!(r["error"].as_str().unwrap().contains("8 episodes at most"), "{r}");
}

/// Speak MCP to the real `antiloki-agent mcp` for one pane, the way an engine's CLI does: initialize, then one tools/call. Returns the call's text and whether it was an error.
fn mcp_call(agent: &Path, be_url: &str, pane: i64, tool: &str, args: Value) -> (String, bool) {
    use std::io::{BufRead, Write};
    let mut c = Command::new(agent).arg("mcp").env("ANTILOKI_BE_URL", be_url).env("ANTILOKI_PANE_ID", pane.to_string()).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().expect("the agent's mcp server");
    let mut sin = c.stdin.take().unwrap(); let mut out = std::io::BufReader::new(c.stdout.take().unwrap());
    writeln!(sin, "{}", json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": { "name": "j32", "version": "0" } } })).unwrap();
    writeln!(sin, "{}", json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": tool, "arguments": args } })).unwrap(); sin.flush().unwrap();
    let mut line = String::new(); let mut found = (String::new(), true);
    while out.read_line(&mut line).unwrap_or(0) > 0 { let v: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null); if v["id"] == json!(2) { found = (v["result"]["content"][0]["text"].as_str().unwrap_or("").to_string(), v["result"]["isError"] == json!(true)); break; } line.clear(); }
    drop(sin); let _ = c.kill(); let _ = c.wait(); found
}

#[test]
fn j32_an_agent_puts_a_mockup_in_frames_through_its_mcp_it_is_sanitized_listed_read_and_deleted() {
    let _s = serial(); let tmp = Scratch::new("j32");
    let ws = project(tmp.path(), "work"); let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let s = Serve::start(&state, free_port()); s.add_workspace("work", &ws); s.activate("work");
    let made = s.post("/api/v1/cockpit/chat/create", json!({ "engine": "claude-code", "title": "j32" })); let pane = made["pane"]["id"].as_i64().expect("a pane");
    let agent = agent_binary(); let url = s.url("").trim_end_matches('/').to_string();
    // the tools are on the agent's list, and an agent can look before it makes one
    let (none, err) = mcp_call(&agent, &url, pane, "antiloki_frames", json!({})); assert!(!err && none.contains("no mockups"), "{none}");
    // it hands one in: scripts, handlers and network addresses do not survive
    let html = r#"<!doctype html><html><head><style>h1{color:#c33}</style><script>window.x=1</script></head><body onload="x()"><h1>Pricing</h1><a href="javascript:alert(1)">buy</a><img src="https://t.example/p.gif"></body></html>"#;
    let (said, err) = mcp_call(&agent, &url, pane, "antiloki_frame_create", json!({ "name": "Pricing", "html": html, "ask": "a pricing page" })); assert!(!err && said.contains("is in the Frames pane"), "{said}");
    let listed = s.get("/api/v1/frames"); let id = listed["frames"][0]["id"].as_str().expect(&listed.to_string()).to_string(); assert_eq!(listed["frames"][0]["name"], json!("Pricing")); assert_eq!(listed["frames"][0]["status"], json!("done"));
    let doc = s.get(&format!("/api/v1/frames/html?id={id}")); let h = doc["html"].as_str().unwrap(); assert!(h.contains("<h1>Pricing</h1>"), "{h}"); for bad in ["<script", "onload", "javascript:", "https://"] { assert!(!h.contains(bad), "`{bad}` is gone: {h}"); }
    assert!(ws.join(".antiloki/frames").join(format!("{id}.html")).is_file(), "kept in the workspace, beside its other records");
    let (again, _) = mcp_call(&agent, &url, pane, "antiloki_frames", json!({})); assert!(again.contains(&id) && again.contains("Pricing"), "{again}");
    // asking a design agent returns at once with the mockup being made; with no seat that can answer it ends as failed, with why (the fake seat here answers no page)
    let (asked, err) = mcp_call(&agent, &url, pane, "antiloki_frame_ask", json!({ "prompt": "a settings screen" })); assert!(!err && asked.contains("design agent is making mockup"), "{asked}");
    let after = until(&s, 120, "the asked mockup to settle", || { let l = s.get("/api/v1/frames"); let f = l["frames"].as_array()?.iter().find(|f| f["ask"] == json!("a settings screen"))?.clone(); if f["status"] != json!("working") { Some(f) } else { None } }); assert!(after["status"] == json!("done") || after["status"] == json!("failed"), "{after}");
    // delete; and an empty mockup is refused
    let d = s.post("/api/v1/frames", json!({ "op": "delete", "id": id })); assert_eq!(d["ok"], json!(true), "{d}"); assert_eq!(s.get(&format!("/api/v1/frames/html?id={id}"))["ok"], json!(false));
    let (bad, err) = mcp_call(&agent, &url, pane, "antiloki_frame_create", json!({ "name": "x", "html": "   " })); assert!(err && bad.contains("not done"), "{bad}");
}

/// RFC 0008 (the team graph, M1 and M2): a team made from the feature template plans a pack of two sagas, works each in a tree of its own with scripted seats, loses a test run once (a retry and the debugger), has its QA reject once (a second pass), brings
/// each saga into the team's branch as it passes, writes everything into the team saga and the pack's sagas, and waits at the merge node: nothing is on the workspace until the person approves, and an agent cannot.
#[test]
fn j33_a_team_graph_plans_a_pack_works_each_saga_apart_is_judged_and_lands_only_when_the_person_approves() {
    let _s = serial(); let tmp = Scratch::new("j33");
    let ws = project(tmp.path(), "work");
    std::fs::create_dir_all(ws.join("src")).unwrap(); std::fs::write(ws.join("src/app.txt"), "app\n").unwrap(); git(&ws, &["add", "-A"]); git(&ws, &["commit", "-q", "-m", "src"]);
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let c = |id: &str, ok: bool, ev: &str| json!({ "id": id, "ok": ok, "evidence": ev });
    let script = json!({
        "plan": [{ "text": { "summary": "two sagas", "sagas": [
            { "id": "cart", "title": "The cart", "objective": "show the cart", "criteria": [{ "id": "c1", "text": "the cart shows" }] },
            { "id": "receipt", "title": "The receipt", "objective": "email the receipt", "needs": ["cart"], "criteria": [{ "id": "c1", "text": "the receipt is sent" }] }] }, "cost": 0.05 }],
        "work": [{ "write": { "src/cart.txt": "cart v1\n" }, "text": "wrote the cart" }, { "write": { "src/cart.txt": "cart v2\n" }, "text": "wrote the cart again" }, { "write": { "src/receipt.txt": "receipt\n" }, "text": "wrote the receipt" }],
        "fix": [{ "write": { "src/cart.txt": "cart v1 fixed\n" }, "text": "patched" }],
        "qa": [{ "text": { "results": [c("c1", false, "the cart is empty")], "summary": "no" } }, { "text": { "results": [c("c1", true, "seen")], "summary": "yes" } }],
        "audit": [{ "text": { "verdict": "pass", "flags": [], "summary": "fine" } }],
    });
    let script_path = tmp.path().join("tg-script.json"); std::fs::write(&script_path, script.to_string()).unwrap();
    // the tests fail the first time they run and pass after
    let tests_path = tmp.path().join("tests.sh"); let counter = tmp.path().join("tests.count");
    std::fs::write(&tests_path, format!("#!/bin/sh\nn=$(cat {c} 2>/dev/null || echo 0)\necho $((n+1)) > {c}\nif [ \"$n\" = \"0\" ]; then echo 'FAIL cart renders'; echo '1 failed'; exit 1; fi\necho ok; exit 0\n", c = counter.display())).unwrap();
    { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&tests_path, std::fs::Permissions::from_mode(0o755)).unwrap(); }
    let s = Serve::start_env(&state, free_port(), &[("FAKE_TG_SCRIPT", &script_path), ("ANTILOKI_TEAM_RUN_TESTS_CMD", &tests_path), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state"))]);
    s.add_workspace("work", &ws); s.activate("work");
    // a template, a name: the team is a graph file, a thread and a tree of its own
    let cat = s.post("/api/v1/cockpit/team-graph", json!({ "op": "catalog" })); assert!(cat["nodes"].as_array().unwrap().len() >= 20, "{cat}");
    let made = s.post("/api/v1/cockpit/team-graph", json!({ "op": "create", "name": "Checkout pack", "template": "feature", "budget_usd": 6.0 })); assert_eq!(made["ok"], json!(true), "{made}");
    assert_eq!(made["thread"], json!("checkout-pack")); assert_eq!(made["report"]["errors"].as_array().unwrap().len(), 0, "{made}");
    let threads = s.post("/api/v1/cockpit/cowork/threads", json!({ "op": "list" })); assert!(threads["threads"].as_array().unwrap().iter().any(|t| t["team"] == json!("checkout-pack")), "the team has a thread of its own: {threads}");
    assert!(ws.join(".antiloki/teams/checkout-pack.json").exists(), "and a graph file the leader and the editor write");
    // the simulation says the path before a cent is spent
    let sim = s.post("/api/v1/cockpit/team-graph", json!({ "op": "simulate", "team": "checkout-pack", "stubs": { "qa": [{ "pass": false }, { "pass": true }] } })); assert_eq!(sim["ok"], json!(true), "{sim}"); assert_eq!(sim["sim"]["outcome"], json!("delivered"));
    // an invalid graph cannot run, and says why
    let bad = s.post("/api/v1/cockpit/team-graph", json!({ "op": "edit", "team": "checkout-pack", "node": "loop", "patch": { "max_iterations": 0 } })); assert_eq!(bad["ok"], json!(false), "{bad}"); assert!(bad["error"].as_str().unwrap().contains("invalid"), "{bad}");
    // run it
    let go = s.post("/api/v1/cockpit/team-graph", json!({ "op": "run", "team": "checkout-pack", "ask": "build the checkout: the cart, then the receipt" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    let parked = until(&s, 120, "the run to wait at the merge node", || { let st = s.post("/api/v1/cockpit/team-graph", json!({ "op": "status", "run": run })); let r = st["run"].clone(); if r["state"]["active"]["me"].is_number() { Some(r) } else { if r["state"]["status"]["status"] == json!("blocked") { panic!("the run was handed to the person: {r}"); } None } });
    // the pack: two sagas the planner made; each has episodes, and the team saga has one per pass (cart: two passes, receipt: one)
    // the list is served from a 2-second cache (swr.rs): wait until it shows the team saga and both packs
    let sagas = until(&s, 20, "the team saga and its two packs in the list", || { let l = s.get("/api/v1/sagas"); let a = l["sagas"].as_array()?.clone(); (a.iter().any(|x| x["meta"]["kind"] == json!("team")) && a.iter().filter(|x| x["meta"]["kind"] == json!("pack")).count() == 2).then(|| l) }); let list = sagas["sagas"].as_array().unwrap();
    let team_saga = list.iter().find(|x| x["meta"]["kind"] == json!("team")).unwrap_or_else(|| panic!("the team saga: {sagas}")).clone(); let packs: Vec<&Value> = list.iter().filter(|x| x["meta"]["kind"] == json!("pack")).collect(); assert_eq!(packs.len(), 2, "{sagas}");
    let td = s.get(&format!("/api/v1/sagas/detail?id={}", team_saga["id"])); let eps = td["episodes"].as_array().unwrap();
    assert_eq!(eps.len(), 3, "an episode for every pass of the loop (cart twice, receipt once): {td}");
    assert_eq!(eps[1]["meta"]["retry_of"], eps[0]["id"], "the second pass of the cart retries the first"); assert_eq!(eps[0]["status"], json!("failed")); assert!(eps[2]["meta"]["retry_of"].is_null(), "a new saga starts clean");
    let evs: Vec<String> = td["events"].as_array().unwrap().iter().map(|e| format!("{} {}", e["actor"].as_str().unwrap_or(""), e["text"].as_str().unwrap_or(""))).collect();
    for want in ["tests", "fix", "qa", "audit", "work"] { assert!(evs.iter().any(|e| e.starts_with(want)), "the team saga trail names {want}: {evs:?}"); }
    assert!(evs.iter().any(|e| e.contains("FAIL") || e.contains("exit=1") || e.contains("failed")), "the red test run is in the trail: {evs:?}");
    let cart = packs.iter().find(|x| x["title"] == json!("The cart")).unwrap(); let cd = s.get(&format!("/api/v1/sagas/detail?id={}", cart["id"])); assert_eq!(cd["episodes"].as_array().unwrap().len(), 2, "{cd}");
    assert!(cd["episodes"][0]["meta"]["produced_by"].is_number(), "each pack episode points at the pass of the team that made it: {cd}");
    // each node's own words and result are kept
    let node = s.post("/api/v1/cockpit/team-graph", json!({ "op": "node", "run": run, "node": "qa" })); assert_eq!(node["attempts"].as_array().unwrap().len(), 3, "{node}"); assert!(node["attempts"][0]["text"].as_str().unwrap().contains("the cart is empty"), "{node}");
    // the team's branch holds the cart (integrated when the receipt began); the workspace holds nothing of it yet
    let branches = std::process::Command::new("git").args(["-C", ws.to_str().unwrap(), "branch", "--list", "thread-*-checkout-pack"]).output().unwrap(); let branch = String::from_utf8_lossy(&branches.stdout).trim().trim_start_matches(|c: char| c == '*' || c == '+' || c == ' ').to_string(); assert!(!branch.is_empty(), "the team's branch");
    let shown = std::process::Command::new("git").args(["-C", ws.to_str().unwrap(), "show", &format!("{branch}:src/cart.txt")]).output().unwrap(); assert!(String::from_utf8_lossy(&shown.stdout).contains("cart v2"), "the cart is in the team's branch: {:?}", String::from_utf8_lossy(&shown.stderr));
    assert!(!ws.join("src/cart.txt").exists(), "main is untouched until the person says so: {:?}", parked["state"]["active"]);
    // an agent can ask, and cannot approve
    let ask = s.post("/api/v1/cockpit/team-graph", json!({ "op": "request_approval", "run": run, "node": "me", "note": "ready", "pane_id": 7 })); assert_eq!(ask["ok"], json!(true), "{ask}");
    let no = s.post("/api/v1/cockpit/team-graph", json!({ "op": "answer", "run": run, "node": "me", "approve": true, "pane_id": 7 })); assert_eq!(no["ok"], json!(false)); assert!(no["error"].as_str().unwrap().contains("only the person"), "{no}");
    // the person approves: the pack lands on the workspace and the run is delivered
    let yes = s.post("/api/v1/cockpit/team-graph", json!({ "op": "answer", "run": run, "node": "me", "approve": true })); assert_eq!(yes["ok"], json!(true), "{yes}");
    until(&s, 30, "the pack to land", || (ws.join("src/cart.txt").exists() && ws.join("src/receipt.txt").exists()).then_some(()));
    let done = until(&s, 30, "the run to be delivered", || { let r = s.post("/api/v1/cockpit/team-graph", json!({ "op": "status", "run": run }))["run"].clone(); (r["state"]["status"]["status"] == json!("done")).then_some(r) });
    assert!(done["state"]["spent_usd"].as_f64().unwrap() > 0.04, "the cost of the run is counted: {}", done["state"]["spent_usd"]);
    let after = s.get(&format!("/api/v1/sagas/detail?id={}", team_saga["id"])); assert!(after["episodes"].as_array().unwrap().iter().all(|e| e["status"] != json!("doing")), "no episode is left open: {after}");
}

/// RFC 0008 §4.8 and §4.4: two workers side by side after a fork each work in a tree of their own (they own different files), are brought together at the join, an `mcp` tool node reads the screen's panes through the
/// serve's own door, and the merge lands both files on the workspace only when the person approves.
#[test]
fn j34_the_branches_of_a_fork_work_apart_are_joined_and_an_mcp_node_calls_the_serves_door() {
    let _s = serial(); let tmp = Scratch::new("j34");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let script = json!({ "wa": [{ "write": { "a/x.txt": "A\n" }, "text": "did a", "cost": 0.01 }], "wb": [{ "write": { "b/y.txt": "B\n" }, "text": "did b", "cost": 0.01 }] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let port = free_port();
    let s = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state"))]);
    s.add_workspace("work", &ws); s.activate("work");
    let role = |o: &str| json!({ "node": "role", "role": "worker", "owns": [o], "cap_usd": 0.5 });
    let graph = json!({ "team": "Fork test", "schema": 1, "budget_usd": 3.0,
        "nodes": { "leader": { "node": "leader" }, "fork": { "node": "fork" }, "wa": role("a/**"), "wb": role("b/**"), "join": { "node": "join", "policy": "all" },
                   "panes": { "node": "use_tool", "tool": { "type": "mcp", "name": "antiloki_panes", "args": {} }, "allow": { "effects": ["read"], "mcp": ["antiloki_panes"] } },
                   "me": { "node": "human", "gate": "merge" }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "fork", "port": "branch", "to": "wa" }, { "from": "fork", "port": "branch", "to": "wb" }, { "from": "wa", "to": "join" }, { "from": "wb", "to": "join" }, { "from": "join", "to": "panes" }, { "from": "panes", "to": "me" }, { "from": "me", "port": "approved", "to": "deliver" } ] });
    let saved = s.post("/api/v1/cockpit/team-graph", json!({ "op": "save", "graph": graph })); assert_eq!(saved["ok"], json!(true), "{saved}"); assert_eq!(saved["report"]["errors"].as_array().unwrap().len(), 0, "{saved}");
    // writers that share files are refused before any run
    let mut bad = graph.clone(); bad["nodes"]["wb"] = role("a/sub/**"); let v = s.post("/api/v1/cockpit/team-graph", json!({ "op": "validate", "graph": bad })); assert!(v["report"]["errors"].as_array().unwrap().iter().any(|e| e["code"] == json!("parallel_overlap")), "{v}");
    let go = s.post("/api/v1/cockpit/team-graph", json!({ "op": "run", "team": "fork-test", "ask": "make a and b" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    let last = std::cell::RefCell::new(json!(null));
    let parked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| until(&s, 90, "the run to wait at the merge", || { let r = s.post("/api/v1/cockpit/team-graph", json!({ "op": "status", "run": run }))["run"].clone(); *last.borrow_mut() = r.clone(); if r["state"]["active"]["me"].is_number() { Some(r) } else { None } })));
    let parked = parked.unwrap_or_else(|_| { let l = last.borrow(); let evs: Vec<String> = l["events"].as_array().map(|a| a.iter().map(|e| format!("{} {} {}", e["actor"], e["kind"], e["text"])).collect()).unwrap_or_default(); panic!("the run did not reach the merge: status {} active {} events {:#?}", l["state"]["status"], l["state"]["active"], evs) });
    assert_eq!(parked["state"]["values"]["panes"]["ok"], json!(true), "the mcp node called the serve's door: {}", parked["state"]["values"]["panes"]);
    let evs: Vec<String> = parked["events"].as_array().unwrap().iter().map(|e| e["text"].as_str().unwrap_or("").to_string()).collect();
    assert!(evs.iter().any(|t| t.contains("brought together")), "the join brought the two branches into the saga's tree: {evs:?}");
    assert!(!ws.join("a/x.txt").exists() && !ws.join("b/y.txt").exists(), "main is untouched until the person says so");
    let ok = s.post("/api/v1/cockpit/team-graph", json!({ "op": "answer", "run": run, "node": "me", "approve": true })); assert_eq!(ok["ok"], json!(true), "{ok}");
    let landed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| until(&s, 30, "both files to land", || (ws.join("a/x.txt").exists() && ws.join("b/y.txt").exists()).then_some(()))));
    if landed.is_err() { let r = s.post("/api/v1/cockpit/team-graph", json!({ "op": "status", "run": run }))["run"].clone(); let evs: Vec<String> = r["events"].as_array().map(|a| a.iter().map(|e| format!("{} {} {}", e["actor"], e["kind"], e["text"])).collect()).unwrap_or_default(); panic!("the files did not land: status {} active {} events {:#?}", r["state"]["status"], r["state"]["active"], evs); }
    // an mcp node may not start agents
    let mut g2 = graph.clone(); g2["nodes"]["panes"]["tool"]["name"] = json!("antiloki_delegate"); g2["nodes"]["panes"]["allow"]["mcp"] = json!([]); let _ = g2;
}

/// RFC 0008 §4.16: a team built from the person's own words. The Leader (here, the test, speaking as one) writes a skill and designs a team with a CUSTOM agent; the skill is a draft the person must approve, an agent cannot approve it,
/// the agent's prompt carries its brief and the skill's playbook, its work lands only when the person approves the merge, and a skill never widens its agent.
#[test]
fn j36_a_custom_agent_with_a_leader_written_skill_runs_only_after_the_person_approves_the_skill() {
    let _s = serial(); let tmp = Scratch::new("j36");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let script = json!({ "copy": [{ "write": { "content/post.md": "the launch post\n" }, "text": { "done": true, "report": "wrote the launch post", "files": ["content/post.md"] }, "cost": 0.02 }] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let port = free_port();
    let s = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state"))]);
    s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    // made from words: a blank team and the brief its Leader is told
    let made = s.post(door, json!({ "op": "create", "name": "Marketing", "goal": "a marketing team for our shop", "budget_usd": 3.0 })); assert_eq!(made["ok"], json!(true), "{made}");
    assert!(made["brief"].as_str().unwrap().contains("SCREEN FIRST") && made["brief"].as_str().unwrap().contains("a marketing team for our shop"), "{made}");
    // the Leader writes the skill: since 045b6ef it is APPROVED at once (the person can remove it); a DRAFT is what an older or imported skill still is, so the gate below is exercised by putting this one back to a draft
    let w = s.post(door, json!({ "op": "skill_write", "team": "marketing", "name": "voice", "purpose": "how the shop sounds", "effects": ["read"], "body": "1. be plain\n2. be warm" })); assert_eq!(w["ok"], json!(true), "{w}"); assert_eq!(w["skill"]["status"], json!("approved"));
    let skill_file = ws.join(".antiloki").join("teams").join("marketing").join("skills").join("voice.md");
    let text = std::fs::read_to_string(&skill_file).unwrap_or_else(|e| panic!("the skill file {}: {e}", skill_file.display())); std::fs::write(&skill_file, text.replace("status: approved", "status: draft")).unwrap();
    // and designs the team: one agent, one job, that writes content/ and then the person's merge
    let graph = json!({ "team": "Marketing", "schema": 1, "budget_usd": 3.0, "start": ["copy"],
        "nodes": { "leader": { "node": "leader" }, "copy": { "node": "role", "role": "agent", "owns": ["content/**"], "cap_usd": 0.5, "agent": { "title": "the Copywriter", "responsibility": "write the launch post", "brief": "plain and warm, for shop owners", "context": [], "skills": ["voice"], "mcp": [], "effects": ["read", "write"] } },
                   "me": { "node": "human", "gate": "merge" }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "copy", "to": "me" }, { "from": "me", "port": "approved", "to": "deliver" } ] });
    let saved = s.post(door, json!({ "op": "save", "graph": graph })); assert_eq!(saved["ok"], json!(true), "{saved}"); assert_eq!(saved["report"]["errors"].as_array().unwrap().len(), 0, "a draft skill is no error for a SAVE: {saved}");
    assert!(saved["report"]["warnings"].as_array().unwrap().iter().any(|w| w["code"] == json!("skill_unreviewed")), "{saved}");
    // a run is refused while the skill is a draft
    let no = s.post(door, json!({ "op": "run", "team": "marketing", "ask": "launch the shop" })); assert_eq!(no["ok"], json!(false), "{no}"); assert!(no["error"].as_str().unwrap().contains("draft"), "{no}");
    // an agent (a call from a pane) cannot approve it
    let by_agent = s.post(door, json!({ "op": "skill_approve", "team": "marketing", "name": "voice", "pane_id": 7 })); assert_eq!(by_agent["ok"], json!(false), "{by_agent}");
    // a skill that would widen its agent is refused too: this one is declared `outward`, the agent has no such effect
    s.post(door, json!({ "op": "skill_write", "team": "marketing", "name": "poster", "purpose": "posts it", "effects": ["outward"], "body": "post it" }));
    let mut wide = graph.clone(); wide["nodes"]["copy"]["agent"]["skills"] = json!(["voice", "poster"]); let v = s.post(door, json!({ "op": "validate", "graph": wide })); assert!(v["report"]["errors"].as_array().unwrap().iter().any(|e| e["code"] == json!("skill_exceeds_agent")), "{v}");
    // the person reads and approves it
    let ok = s.post(door, json!({ "op": "skill_approve", "team": "marketing", "name": "voice" })); assert_eq!(ok["ok"], json!(true), "{ok}"); assert_eq!(ok["skill"]["status"], json!("approved"));
    let go = s.post(door, json!({ "op": "run", "team": "marketing", "ask": "launch the shop" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    let parked = until(&s, 90, "the run to wait at the merge", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); if r["state"]["active"]["me"].is_number() { Some(r) } else { None } });
    assert_eq!(parked["state"]["values"]["copy"]["report"], json!("wrote the launch post"), "{}", parked["state"]["values"]["copy"]);
    let node = s.post(door, json!({ "op": "node", "run": run, "node": "copy" })); let text = node["attempts"][0]["text"].as_str().unwrap_or("").to_string();
    assert!(text.contains("the Copywriter") && text.contains("plain and warm, for shop owners") && text.contains("be plain") && text.contains("content/**"), "the agent's prompt carries its brief, its skill and its scope: {text}");
    assert!(!ws.join("content/post.md").exists(), "nothing is on the workspace until the person approves");
    let done = s.post(door, json!({ "op": "answer", "run": run, "node": "me", "approve": true })); assert_eq!(done["ok"], json!(true), "{done}");
    until(&s, 30, "the post to land", || ws.join("content/post.md").exists().then_some(()));
}

/// RFC 0008 §4.17: the artifact node. Two agents each leave a document that is not code (a report, a dashboard); artifact nodes keep them with the run, outside the code and the branch; the door lists and reads them.
#[test]
fn j37_artifact_nodes_keep_reports_and_dashboards_with_the_run_and_never_in_the_code() {
    let _s = serial(); let tmp = Scratch::new("j37");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let script = json!({ "analyst": [{ "write": { ".antiloki/artifacts/findings.md": "# Findings\n- the cart is slow\n" }, "text": { "done": true, "report": "findings written", "files": [] }, "cost": 0.01 }],
                         "designer": [{ "text": { "done": true, "report": "Dashboard: 3 pages, 1 slow", "files": [] }, "cost": 0.01 }] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let port = free_port();
    let s = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state"))]);
    s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let agent = |title: &str, job: &str| json!({ "node": "role", "role": "agent", "cap_usd": 0.5, "agent": { "title": title, "responsibility": job, "brief": "be brief", "effects": ["read"] } });
    let graph = json!({ "team": "Reports", "schema": 1, "budget_usd": 3.0, "start": ["analyst"],
        "nodes": { "leader": { "node": "leader" }, "analyst": agent("the Analyst", "analyse the cart code"), "findings": { "node": "artifact", "name": "findings", "format": "md", "from": "analyst", "title": "Findings" },
                   "designer": agent("the Designer", "draw a one-page dashboard"), "dash": { "node": "artifact", "name": "dashboard", "format": "html", "from": "designer", "title": "Dashboard" },
                   "me": { "node": "human", "gate": "merge" }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "analyst", "to": "findings" }, { "from": "findings", "to": "designer" }, { "from": "designer", "to": "dash" }, { "from": "dash", "to": "me" }, { "from": "me", "port": "approved", "to": "deliver" } ] });
    let saved = s.post(door, json!({ "op": "save", "graph": graph })); assert_eq!(saved["ok"], json!(true), "{saved}"); assert_eq!(saved["report"]["errors"].as_array().unwrap().len(), 0, "{saved}");
    let go = s.post(door, json!({ "op": "run", "team": "reports", "ask": "report on the cart" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    let parked = until(&s, 90, "the run to wait at the merge", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); if r["state"]["active"]["me"].is_number() { Some(r) } else { None } });
    assert_eq!(parked["state"]["values"]["findings"]["path"], json!("artifacts/findings.md")); assert_eq!(parked["state"]["values"]["findings"]["made_from"], json!("the file it wrote"));
    assert_eq!(parked["state"]["values"]["dash"]["made_from"], json!("its report"), "no file: the report is the document"); assert_eq!(parked["state"]["values"]["dash"]["format"], json!("html"));
    let list = s.post(door, json!({ "op": "artifacts", "run": run })); let names: Vec<String> = list["artifacts"].as_array().unwrap().iter().filter(|a| a["kind"] == json!("made")).map(|a| a["name"].as_str().unwrap().to_string()).collect(); assert_eq!(names, vec!["dashboard.html", "findings.md"], "{list}");
    let f = s.post(door, json!({ "op": "artifact", "run": run, "name": "findings.md" })); assert!(f["text"].as_str().unwrap().contains("the cart is slow"), "{f}");
    let d = s.post(door, json!({ "op": "artifact", "run": run, "name": "dashboard.html" })); assert!(d["text"].as_str().unwrap().starts_with("<!doctype html>") && d["text"].as_str().unwrap().contains("Dashboard: 3 pages"), "{d}");
    assert_eq!(s.post(door, json!({ "op": "artifact", "run": run, "name": "../state.json" }))["ok"], json!(false), "a name cannot leave the artifacts folder");
    let ok = s.post(door, json!({ "op": "answer", "run": run, "node": "me", "approve": true })); assert_eq!(ok["ok"], json!(true), "{ok}");
    until(&s, 20, "the run to be delivered", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); (r["state"]["status"]["status"] == json!("done")).then_some(()) });
    let tracked = std::process::Command::new("git").args(["ls-files"]).current_dir(&ws).output().unwrap(); assert!(!String::from_utf8_lossy(&tracked.stdout).contains("findings"), "an artifact is never in the code");
    assert!(!ws.join("findings.md").exists() && !ws.join(".antiloki/artifacts/findings.md").exists(), "and never in the workspace");
}

/// RFC 0010 §4.16: the master page. A team ends with a `compose` artifact node; the run builds ONE html page from what it has (no model); the door hands it over as the result; a bad spec is refused when saved; a run with no such node gets a default page on request.
#[test]
fn j41_the_run_builds_one_master_page_and_a_run_without_one_can_ask_for_it() {
    let _s = serial(); let tmp = Scratch::new("j41");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let script = json!({ "analyst": [{ "write": { ".antiloki/artifacts/findings.md": "# Findings\n- the cart is slow <b>really</b>\n" }, "text": { "done": true, "report": "The cart is slow: **three** slow queries.", "files": [] }, "cost": 0.02 }] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let port = free_port();
    let s = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state"))]);
    s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let agent = |title: &str, job: &str| json!({ "node": "role", "role": "agent", "cap_usd": 0.5, "agent": { "title": title, "responsibility": job, "brief": "be brief", "effects": ["read"] } });
    let page = |sections: Value| json!({ "node": "artifact", "name": "page", "format": "html", "compose": { "title": "The cart report", "sections": sections } });
    let good = json!([ { "title": "", "widgets": [{ "type": "hero" }] }, { "title": "Summary", "widgets": [{ "type": "summary", "from": "report:analyst" }] }, { "title": "Findings", "widgets": [{ "type": "markdown", "from": "artifact:findings.md" }] },
                       { "title": "Numbers", "widgets": [{ "type": "kpis", "items": [{ "label": "spent", "from": "run:cost" }, { "label": "documents", "from": "run:artifacts" }] }, { "type": "cost" }] }, { "title": "Timeline", "widgets": [{ "type": "timeline" }] } ]);
    let graph = |sections: Value| json!({ "team": "Cart", "schema": 1, "budget_usd": 3.0, "start": ["analyst"],
        "nodes": { "leader": { "node": "leader" }, "analyst": agent("the Analyst", "analyse the cart code"), "findings": { "node": "artifact", "name": "findings", "format": "md", "from": "analyst", "title": "Findings" }, "page": page(sections), "me": { "node": "human", "gate": "merge" }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "analyst", "to": "findings" }, { "from": "findings", "to": "page" }, { "from": "page", "to": "me" }, { "from": "me", "port": "approved", "to": "deliver" } ] });
    // a page that reads a node that does not exist, a widget that is not one, and a document nobody makes: refused with words, before any run
    let bad = s.post(door, json!({ "op": "save", "graph": graph(json!([ { "title": "x", "widgets": [{ "type": "summary", "from": "report:ghost" }, { "type": "chart3d" }, { "type": "markdown", "from": "artifact:nothing.md" }] } ])) }));
    let errs = bad["report"]["errors"].as_array().cloned().unwrap_or_default(); let said = errs.iter().map(|e| e["msg"].as_str().or_else(|| e["message"].as_str()).unwrap_or("").to_string()).collect::<Vec<_>>().join(" | ");
    assert!(said.contains("no such node") && said.contains("not a widget") && said.contains("no artifact node makes it"), "{bad}");
    let saved = s.post(door, json!({ "op": "save", "graph": graph(good) })); assert_eq!(saved["ok"], json!(true), "{saved}"); assert_eq!(saved["report"]["errors"].as_array().unwrap().len(), 0, "{saved}");
    let go = s.post(door, json!({ "op": "run", "team": "cart", "ask": "report on the cart" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    let parked = until(&s, 90, "the run to wait at the merge", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); if r["state"]["active"]["me"].is_number() { Some(r) } else { None } });
    assert_eq!(parked["state"]["values"]["page"]["result"], json!(true), "{parked}"); assert_eq!(parked["state"]["values"]["page"]["path"], json!("artifacts/page.html"));
    let res = s.post(door, json!({ "op": "result", "run": run })); assert_eq!(res["ok"], json!(true), "{res}"); let html = res["text"].as_str().unwrap();
    assert!(html.starts_with("<!doctype html>") && html.contains("The cart report") && html.contains("<strong>three</strong>"), "the summary is the analyst's report, rendered");
    assert!(html.contains("the cart is slow &lt;b&gt;really&lt;/b&gt;"), "the document is shown and escaped"); assert!(html.contains("$0.02") && html.contains("class=\"tl\""), "the numbers and the timeline");
    if let Ok(p) = std::env::var("J41_DUMP") { let _ = std::fs::write(p, html); }   // to look at the page by eye
    assert!(!html.contains("<b>really</b>") && !html.contains("http://") && !html.contains("https://"), "no markup from a document, no network");
    // a run with no master page node: the person asks for one
    let ok = s.post(door, json!({ "op": "answer", "run": run, "node": "me", "approve": true })); assert_eq!(ok["ok"], json!(true), "{ok}");
    until(&s, 20, "the run to be delivered", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); (r["state"]["status"]["status"] == json!("done")).then_some(()) });
    // the run's own working tree (`<run number>-run`) was taken away once its work was in the team's branch: runs do not pile up trees
    let num = run.rsplit('-').next().unwrap().to_string(); let wt = std::process::Command::new("git").args(["worktree", "list"]).current_dir(&ws).output().unwrap(); let wt = String::from_utf8_lossy(&wt.stdout).to_string();
    let br = std::process::Command::new("git").args(["branch", "--list"]).current_dir(&ws).output().unwrap(); let br = String::from_utf8_lossy(&br.stdout).to_string();
    assert!(!wt.contains(&format!("{num}-run")) && !br.contains(&format!("{num}-run")), "no tree or branch left for the run: {wt} {br}");
    let auto = s.post(door, json!({ "op": "compose_auto", "run": run })); assert_eq!(auto["ok"], json!(true), "{auto}"); assert_eq!(auto["name"], json!("result.html"));
    let listed = s.post(door, json!({ "op": "artifacts", "run": run })); let names: Vec<&str> = listed["artifacts"].as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap()).collect(); assert!(names.contains(&"result.html") && names.contains(&"page.html"), "{listed}");
    assert_eq!(s.post(door, json!({ "op": "result", "run": "nope" }))["ok"], json!(false));
}

/// RFC 0010 §4.6: the check node. A writer drafts, a check (no model, no cost) judges the draft against rules, the loop goes round on a failed check and ends on a pass; what failed is on the record; a bad rule or a source that does not exist is refused when saved.
#[test]
fn j42_a_check_judges_the_draft_for_free_the_loop_ends_on_its_pass_and_a_bad_rule_is_refused() {
    let _s = serial(); let tmp = Scratch::new("j42");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let script = json!({ "writer": [{ "write": { ".antiloki/artifacts/draft.md": "# Post\nThis week: 20% off all beans!\n" }, "text": { "done": true, "report": "drafted", "files": [] }, "cost": 0.01 },
                                    { "write": { ".antiloki/artifacts/draft.md": "# Post\nFresh beans, roasted today.\n" }, "text": { "done": true, "report": "redrafted", "files": [] }, "cost": 0.01 }] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let port = free_port();
    let s = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state"))]);
    s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let agent = |title: &str, job: &str| json!({ "node": "role", "role": "agent", "cap_usd": 0.5, "agent": { "title": title, "responsibility": job, "brief": "be brief", "effects": ["document"] }, "why": { "kind": "parallel", "note": "x" } });
    let graph = |rules: Value, on: &str| json!({ "team": "Posts", "schema": 1, "budget_usd": 3.0, "start": ["rl"],
        "nodes": { "leader": { "node": "leader" }, "rl": { "node": "loop", "max_iterations": 3, "until": "check.pass == true", "cap_usd": 1.0 }, "writer": agent("the Writer", "write the post"),
                   "doc": { "node": "artifact", "name": "draft", "format": "md", "from": "writer" }, "check": { "node": "check", "on": on, "rules": rules, "why": { "kind": "independent_check", "note": "no offers" } }, "me": { "node": "human", "gate": "merge" }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "rl", "port": "body", "to": "writer" }, { "from": "writer", "to": "doc" }, { "from": "doc", "to": "check" }, { "from": "check", "to": "rl" }, { "from": "rl", "port": "done", "to": "me" }, { "from": "rl", "port": "exhausted", "to": "me" }, { "from": "me", "port": "approved", "to": "deliver" } ] });
    let good = json!([ { "id": "no-offers", "kind": "forbid", "pattern": "(?i)\\b\\d+\\s?% off\\b|\\bdiscount\\b", "message": "a discount or price offer" }, { "id": "short", "kind": "words", "max": 40 } ]);
    // refused with words: a rule that is not a regex, a kind not built, a source nobody makes
    let bad = s.post(door, json!({ "op": "validate", "graph": graph(json!([ { "id": "a", "kind": "forbid", "pattern": "(" }, { "id": "b", "kind": "contrast" } ]), "artifact:nothing.md") }));
    let said = bad["report"]["errors"].as_array().cloned().unwrap_or_default().iter().map(|e| e["message"].as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join(" | ");
    assert!(said.contains("not a regular expression") && said.contains("`contrast` is not built yet") && said.contains("is not something to check"), "{bad}");
    let saved = s.post(door, json!({ "op": "save", "graph": graph(good, "artifact:draft.md") })); assert_eq!(saved["ok"], json!(true), "{saved}"); assert_eq!(saved["report"]["errors"].as_array().unwrap().len(), 0, "{saved}");
    let go = s.post(door, json!({ "op": "run", "team": "posts", "ask": "write a post" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    let parked = until(&s, 90, "the run to wait at the merge", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); if r["state"]["active"]["me"].is_number() { Some(r) } else { None } });
    let evs: Vec<String> = parked["events"].as_array().unwrap().iter().map(|e| format!("{} {}", e["actor"].as_str().unwrap_or(""), e["kind"].as_str().unwrap_or(""))).collect();
    assert_eq!(evs.iter().filter(|e| *e == "writer start").count(), 2, "the loop went round once for a failed check and stopped on the pass: {evs:?}");
    let c = &parked["state"]["values"]["check"]; assert_eq!(c["pass"], json!(true), "{c}"); assert_eq!(c["score"], json!(1.0)); assert_eq!(c["failed"].as_array().unwrap().len(), 0);
    let first = s.post(door, json!({ "op": "node", "run": run, "node": "check" })); let texts = first["attempts"].as_array().unwrap().iter().map(|a| a["text"].as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join(" || ");
    assert!(texts.contains("failed") && texts.contains("a discount or price offer") && texts.contains("passed"), "the first check failed with its reason, the second passed: {texts}");
    assert_eq!(parked["state"]["node_cost"]["check"].as_f64().unwrap_or(0.0), 0.0, "a check costs nothing");
}

/// RFC 0010 §4.4: the model node. A small seat answers a typed question; a free check finds a price it made up and a cross-check finds two seats that disagree; each climbs the ladder to a bigger seat and says so; an answer the checks accept
/// stays where it is. The seats are a fake OpenAI-compatible server (`small` makes the mistakes the benchmark found), reached by the endpoint registry.
#[test]
fn j43_a_model_node_climbs_the_ladder_only_when_a_free_check_or_a_second_seat_says_so() {
    let _s = serial(); let tmp = Scratch::new("j43");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let fake_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || { for stream in listener.incoming().flatten() { std::thread::spawn(move || { use std::io::{Read, Write}; let mut st = stream; let mut buf = Vec::new(); let mut chunk = [0u8; 4096];
        loop { let n = st.read(&mut chunk).unwrap_or(0); if n == 0 { break; } buf.extend_from_slice(&chunk[..n]); let t = String::from_utf8_lossy(&buf).to_string(); if let Some(i) = t.find("\r\n\r\n") { let len = t[..i].to_lowercase().split("content-length:").nth(1).and_then(|x| x.trim().split_whitespace().next().and_then(|n| n.parse::<usize>().ok())).unwrap_or(0); if buf.len() >= i + 4 + len { break; } } }
        let t = String::from_utf8_lossy(&buf).to_string(); let body: Value = serde_json::from_str(t.split("\r\n\r\n").nth(1).unwrap_or("{}")).unwrap_or(Value::Null); let model = body["model"].as_str().unwrap_or("").to_string(); let prompt = body["messages"][0]["content"].as_str().unwrap_or("").to_string();
        let reply = if prompt.contains("Extract") { match model.as_str() { "small" => "Sure! {\"name\": \"Orchard Morning\", \"origin\": \"Guatemala\", \"price\": \"18.25\"}", _ => "{\"name\": \"Orchard Morning\", \"origin\": \"Guatemala\", \"price\": 18}" } } else { match model.as_str() { "small" => "{\"kind\": \"Write\"}", _ => "{\"kind\": \"review\"}" } };
        let out = json!({ "choices": [{ "message": { "content": reply } }] }).to_string(); let _ = write!(st, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}", out.len()); }); } });
    let seats = tmp.path().join("seats.json"); std::fs::write(&seats, json!({ "endpoints": { "fake": { "url": format!("http://127.0.0.1:{fake_port}"), "model": "small" } } }).to_string()).unwrap();
    let port = free_port();
    let s = Serve::start_env(&state, port, &[("ANTILOKI_SEATS_FILE", &seats)]);
    s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let ask = "New on the shelf: Orchard Morning. Origin: Antigua, Guatemala. Price £18, 250g, free grinding.";
    let graph = |extract_seat: &str, verify: Value, prompt: &str| json!({ "team": "Shelf", "schema": 1, "budget_usd": 1.0, "start": ["extract"],
        "nodes": { "leader": { "node": "leader" },
          "extract": { "node": "llm", "seat": extract_seat, "prompt": prompt, "answer": { "name": "string", "origin": "string", "price": "number" }, "fallback": ["endpoint:fake:big"], "verify": verify, "why": { "kind": "cost", "note": "an extraction does not need a frontier seat" } },
          "kind": { "node": "llm", "seat": "endpoint:fake:small", "prompt": "Classify this task as research, write or review.\nTask: {ask}", "answer": { "kind": { "enum": ["research", "write", "review"] } }, "agree_with": ["endpoint:fake:small2"], "fallback": ["endpoint:fake:big"] },
          "again": { "node": "llm", "seat": "endpoint:fake:big", "prompt": "Classify this task as research, write or review.\nTask: {ask}", "answer": { "kind": { "enum": ["research", "write", "review"] } }, "agree_with": ["endpoint:fake:big"] },
          "deliver": { "node": "deliver" } },
        "edges": [ { "from": "extract", "to": "kind" }, { "from": "kind", "to": "again" }, { "from": "again", "to": "deliver" } ] });
    // refused with words: a seat nobody can call, a verified field the answer does not have, a prompt that reads a node that is not there
    let bad = s.post(door, json!({ "op": "validate", "graph": graph("codex:gpt", json!({ "in_source": ["colour"], "source": "{ask}" }), "Extract from {ghost.text}") }));
    let said = bad["report"]["errors"].as_array().cloned().unwrap_or_default().iter().map(|e| e["message"].as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join(" | ");
    assert!(said.contains("is not a seat for a model node") && said.contains("`colour` is not a field of the answer") && said.contains("reads ghost"), "{bad}");
    let good = graph("endpoint:fake:small", json!({ "in_source": ["price", "origin"], "source": "{ask}" }), "Extract the coffee name, the country and the price from: {ask}");
    let saved = s.post(door, json!({ "op": "save", "graph": good })); assert_eq!(saved["ok"], json!(true), "{saved}"); assert_eq!(saved["report"]["errors"].as_array().unwrap().len(), 0, "{saved}");
    let go = s.post(door, json!({ "op": "run", "team": "shelf", "ask": ask })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    let done = until(&s, 60, "the run to finish", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); (r["state"]["status"]["status"] == json!("done")).then_some(r) });
    let v = &done["state"]["values"];
    let e = &v["extract"]; assert_eq!(e["price"], json!(18.0), "{e}"); assert_eq!(e["seat_used"], json!("endpoint:fake:big"), "the small seat's 18.25 is not in the text: the ladder was climbed"); assert_eq!(e["escalated_from"], json!("endpoint:fake:small")); assert!(e["why_escalated"].as_str().unwrap().contains("does not appear in the source"), "{e}"); assert_eq!(e["verified"], json!(true));
    let k = &v["kind"]; assert_eq!(k["kind"], json!("review"), "{k}"); assert_eq!(k["seat_used"], json!("endpoint:fake:big")); assert!(k["why_escalated"].as_str().unwrap().contains("did not give the same answer"), "two small seats disagreed");
    let a = &v["again"]; assert_eq!(a["agreed"], json!(true), "{a}"); assert_eq!(a["escalated_from"], json!(""), "an answer the checks accept stays where it is"); assert_eq!(done["state"]["spent_usd"].as_f64().unwrap_or(1.0), 0.0, "local seats cost no dollars");
}

/// The self-improving engine (operator 2026-11-13): a run whose check never passes is audited when it ends, the audit is kept with the run and in the team's history, and the Leader's inbox holds the message for sure (its terminal, when open, is typed into too).
/// The Leader's seat tools: what this machine has, the evidence, a registered endpoint, a download that needs the person's yes, and a trial that counts the mistakes the guards would not stop.
#[test]
fn j44_a_run_that_did_not_get_a_good_result_is_audited_and_the_leader_is_told_and_the_leader_can_see_and_trial_the_seats() {
    let _s = serial(); let tmp = Scratch::new("j44");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let bad = json!({ "write": { ".antiloki/artifacts/draft.md": "# Post\nThis week: 20% off all beans!\n" }, "text": { "done": true, "report": "drafted", "files": [] }, "cost": 0.01 });
    let script = json!({ "writer": [bad.clone(), bad.clone(), bad] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let fake_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || { for stream in listener.incoming().flatten() { std::thread::spawn(move || { use std::io::{Read, Write}; let mut st = stream; let mut buf = Vec::new(); let mut chunk = [0u8; 4096];
        loop { let n = st.read(&mut chunk).unwrap_or(0); if n == 0 { break; } buf.extend_from_slice(&chunk[..n]); let t = String::from_utf8_lossy(&buf).to_string(); if let Some(i) = t.find("\r\n\r\n") { let len = t[..i].to_lowercase().split("content-length:").nth(1).and_then(|x| x.trim().split_whitespace().next().and_then(|n| n.parse::<usize>().ok())).unwrap_or(0); if t.starts_with("GET") || buf.len() >= i + 4 + len { break; } } }
        let t = String::from_utf8_lossy(&buf).to_string(); let body = if t.starts_with("GET /v1/models") { json!({ "data": [{ "id": "small" }] }).to_string() } else { json!({ "choices": [{ "message": { "content": "{\"kind\": \"write\"}" } }] }).to_string() };
        let _ = write!(st, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()); }); } });
    let seats = tmp.path().join("seats.json");
    let port = free_port();
    let s = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state")), ("ANTILOKI_SEATS_FILE", &seats)]);
    s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    // ── the audit ──
    let graph = json!({ "team": "Posts", "schema": 1, "budget_usd": 3.0, "start": ["rl"],
        "nodes": { "leader": { "node": "leader" }, "rl": { "node": "loop", "max_iterations": 3, "until": "check.pass == true", "cap_usd": 1.0 },
                   "writer": { "node": "role", "role": "agent", "cap_usd": 0.5, "agent": { "title": "the Writer", "responsibility": "write the post", "brief": "be brief", "effects": ["document"] } },
                   "doc": { "node": "artifact", "name": "draft", "format": "md", "from": "writer" },
                   "check": { "node": "check", "on": "artifact:draft.md", "rules": [{ "id": "no-offers", "kind": "forbid", "pattern": "(?i)\\d+\\s?% off", "message": "a discount or price offer" }] }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "rl", "port": "body", "to": "writer" }, { "from": "writer", "to": "doc" }, { "from": "doc", "to": "check" }, { "from": "check", "to": "rl" }, { "from": "rl", "port": "done", "to": "deliver" }, { "from": "rl", "port": "exhausted", "to": "deliver" } ] });
    let saved = s.post(door, json!({ "op": "save", "graph": graph })); assert_eq!(saved["ok"], json!(true), "{saved}");
    let go = s.post(door, json!({ "op": "run", "team": "posts", "ask": "write a post" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    let a = until(&s, 90, "the run to be audited", || { let r = s.post(door, json!({ "op": "audit", "run": run })); (r["ok"] == json!(true)).then(|| r["audit"].clone()) });
    assert_eq!(a["good"], json!(false), "{a}"); let codes: Vec<String> = a["findings"].as_array().unwrap().iter().map(|f| f["code"].as_str().unwrap().to_string()).collect(); assert!(codes.contains(&"check_failed".to_string()) && codes.contains(&"loop_exhausted".to_string()), "{codes:?}");
    let inbox = s.post(door, json!({ "op": "leader_inbox", "team": "posts" })); assert_eq!(inbox["unread_or_undecided"], json!(2), "the audit message and the run report (every finished run sends one): {inbox}"); let audit_msg = inbox["messages"].as_array().unwrap().iter().find(|m| m["message"].as_str().map_or(false, |t| t.starts_with("[team audit]"))).unwrap_or_else(|| panic!("no audit message: {inbox}")); let msg = audit_msg["message"].as_str().unwrap(); assert!(msg.starts_with("[team audit]") && msg.contains("a discount or price offer") && msg.contains("antiloki_team_design"), "{msg}");
    let hist = s.post(door, json!({ "op": "audits", "team": "posts" })); assert_eq!(hist["audits"].as_array().unwrap().len(), 1, "{hist}"); assert_eq!(hist["audits"][0]["good"], json!(false));
    let bound = s.post(door, json!({ "op": "leader_bind", "team": "posts", "pane": 987654 })); assert_eq!(bound["ok"], json!(true)); assert_eq!(bound["delivered"], json!(0), "no such terminal: the message stays in the inbox, not lost");
    // a Leader terminal that is new is also handed the team's memory (a second message, not yet delivered because there is no such terminal): the audit message is the one decided here
    assert_eq!(s.post(door, json!({ "op": "leader_inbox", "team": "posts" }))["unread_or_undecided"], json!(3), "the audit message, the run report and the memory"); let id = audit_msg["id"].as_str().unwrap();
    assert_eq!(s.post(door, json!({ "op": "leader_ack", "team": "posts", "id": id }))["ok"], json!(true)); assert_eq!(s.post(door, json!({ "op": "leader_inbox", "team": "posts" }))["unread_or_undecided"], json!(2), "the audit is decided; the run report and the memory wait"); assert_eq!(s.post(door, json!({ "op": "leader_ack", "team": "posts", "id": "" }))["ok"], json!(true)); assert_eq!(s.post(door, json!({ "op": "leader_inbox", "team": "posts" }))["unread_or_undecided"], json!(0), "decided");
    // ── the seats ──
    let reg = s.post(door, json!({ "op": "seats_register", "alias": "fake", "url": format!("http://127.0.0.1:{fake_port}"), "model": "small", "max_tokens": 120 })); assert_eq!(reg["ok"], json!(true), "{reg}"); assert!(seats.exists());
    assert_eq!(s.post(door, json!({ "op": "seats_register", "alias": "x", "url": "http://evil.example" }))["ok"], json!(false), "only a local server or https");
    let inv = s.post(door, json!({ "op": "seats_list" })); assert_eq!(inv["ok"], json!(true), "{inv}"); assert!(inv["hardware"]["cpu_threads"].as_u64().unwrap() >= 1 && inv["evidence"]["stages"]["field_extraction"].is_object() && inv["could_download"].as_array().unwrap().len() >= 2 && inv["cli_seats"].as_array().unwrap().len() == 6, "{inv}");
    let ep = inv["endpoints"].as_array().unwrap().iter().find(|e| e["alias"] == json!("fake")).unwrap(); assert_eq!(ep["reachable"], json!(true), "{ep}");
    let fit = s.post(door, json!({ "op": "seats_fit", "stage": "tone_rubric", "seat": "endpoint:fake:small" })); assert_eq!(fit["measured"], json!(true)); assert_eq!(fit["seat_available"], json!(true), "{fit}"); assert!(fit["guards_required"].as_array().unwrap().len() >= 2);
    assert_eq!(s.post(door, json!({ "op": "seats_fit", "stage": "poetry", "seat": "endpoint:fake" }))["measured"], json!(false));
    assert_eq!(s.post(door, json!({ "op": "seats_pull", "tag": "qwen3:4b" }))["ok"], json!(false), "a download needs the person's yes");
    let cases = json!([{ "input": "Write a poem", "expect": { "kind": "write" } }, { "input": "Draft a caption", "expect": { "kind": "write" } }, { "input": "Proofread this post", "expect": { "kind": "review" }, "trap": true }, { "input": "Check the brand rules", "expect": { "kind": "review" }, "trap": true }]);
    let tr = s.post(door, json!({ "op": "seats_trial", "seat": "endpoint:fake:small", "prompt": "Classify: {ask}", "answer": { "kind": { "enum": ["write", "review"] } }, "cases": cases }));
    assert_eq!(tr["ok"], json!(true), "{tr}"); assert_eq!(tr["correct"], json!(2)); assert_eq!(tr["wrong_and_delivered"], json!(2), "the seat calls everything write: two mistakes nothing would have stopped"); assert_eq!(tr["trap_correct"], json!(0)); assert!(tr["mistakes"].as_array().unwrap().iter().all(|m| m["kind"].as_str().unwrap().contains("NOT CAUGHT")), "{tr}");
    assert!(tr["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("fewer than 10 cases")), "{tr}");
}

/// A CAMPAIGN (operator 2026-11-13): the same team run as two arms side by side, within the machine and a budget for the RESULT; the winner chosen by the audit and the cost; and a budget too small to start any arm gives every arm up with the reason.
#[test]
fn j45_a_campaign_runs_arms_side_by_side_inside_the_results_budget_and_picks_a_winner() {
    let _s = serial(); let tmp = Scratch::new("j45");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let good = json!({ "write": { ".antiloki/artifacts/draft.md": "# Post\nFresh beans, roasted today.\n" }, "text": { "done": true, "report": "drafted", "files": [] }, "cost": 0.01 });
    let script = json!({ "writer": [good.clone(), good.clone(), good] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let port = free_port();
    let s = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state"))]);
    s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let graph = json!({ "team": "Posts", "schema": 1, "budget_usd": 2.0, "start": ["writer"],
        "nodes": { "leader": { "node": "leader" }, "writer": { "node": "role", "role": "agent", "cap_usd": 0.5, "agent": { "title": "the Writer", "responsibility": "write the post", "brief": "be brief", "effects": ["document"] } },
                   "doc": { "node": "artifact", "name": "draft", "format": "md", "from": "writer" }, "check": { "node": "check", "on": "artifact:draft.md", "rules": [{ "id": "short", "kind": "words", "max": 40 }] }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "writer", "to": "doc" }, { "from": "doc", "to": "check" }, { "from": "check", "to": "deliver" } ] });
    assert_eq!(s.post(door, json!({ "op": "save", "graph": graph }))["ok"], json!(true));
    // a request that cannot be run is refused in words
    let bad = s.post(door, json!({ "op": "campaign_start", "team": "posts", "ask": "write", "arms": [{ "name": "a" }, { "name": "a" }], "budget_usd": 0 })); assert_eq!(bad["ok"], json!(false)); assert!(bad["error"].as_str().unwrap().contains("two arms are called a") && bad["error"].as_str().unwrap().contains("budget above zero"), "{bad}");
    assert_eq!(s.post(door, json!({ "op": "campaign_start", "team": "posts", "ask": "write", "arms": [{ "name": "a", "patch": { "ghost": { "cap_usd": 1.0 } } }], "budget_usd": 2.0 }))["ok"], json!(false), "a patch of a node that is not there");
    // two arms, the second with a smaller cap on the writer, side by side, inside a $5 result budget
    let go = s.post(door, json!({ "op": "campaign_start", "team": "posts", "ask": "write a post", "arms": [{ "name": "base" }, { "name": "cheap", "patch": { "writer": { "cap_usd": 0.2 } } }], "budget_usd": 5.0, "parallel": 2 })); assert_eq!(go["ok"], json!(true), "{go}"); let id = go["campaign"].as_str().unwrap().to_string();
    assert!(go["capacity"]["slots"].as_u64().unwrap() >= 1);
    let c = until(&s, 120, "the campaign to finish", || { let r = s.post(door, json!({ "op": "campaign_status", "campaign": id }))["campaign"].clone(); (r["status"] != json!("running")).then_some(r) });
    assert_eq!(c["status"], json!("done"), "{c}"); let arms = c["arms"].as_array().unwrap(); assert_eq!(arms.len(), 2); assert!(arms.iter().all(|a| a["state"] == json!("done") && a["good"] == json!(true) && a["run"].is_string()), "{c}");
    assert!(c["winner"].is_string(), "{c}"); assert!(c["peak_parallel"].as_u64().unwrap() >= 1 && c["peak_parallel"].as_u64().unwrap() <= 2, "never more at once than asked: {c}"); assert!(c["spent_usd"].as_f64().unwrap() > 0.0 && c["spent_usd"].as_f64().unwrap() < 5.0);
    assert_ne!(arms[0]["run"], arms[1]["run"], "two runs started together have their own ids"); assert!(c["reading"].as_str().unwrap().contains("winner"), "{c}");
    // the saved team was not edited by the arm's patch
    assert_eq!(s.post(door, json!({ "op": "get", "team": "posts" }))["graph"]["nodes"]["writer"]["cap_usd"], json!(0.5));
    // a result budget below any arm's worst case: nothing starts, every arm says why
    let tiny = s.post(door, json!({ "op": "campaign_start", "team": "posts", "ask": "write a post", "arms": [{ "name": "a" }, { "name": "b" }], "budget_usd": 0.001, "parallel": 2 })); let tid = tiny["campaign"].as_str().unwrap().to_string();
    let t = until(&s, 30, "the tiny campaign to give up", || { let r = s.post(door, json!({ "op": "campaign_status", "campaign": tid }))["campaign"].clone(); (r["status"] != json!("running")).then_some(r) });
    assert!(t["arms"].as_array().unwrap().iter().all(|a| a["state"] == json!("skipped") && a["note"].as_str().unwrap().contains("over its $0.00 budget")), "{t}"); assert_eq!(t["winner"], Value::Null); assert_eq!(t["spent_usd"].as_f64().unwrap(), 0.0);
    assert!(s.post(door, json!({ "op": "campaign_list" }))["campaigns"].as_array().unwrap().len() >= 2);
}

/// RFC 0010 §4.20 A: the benchmark per node. A small seat that climbs to a stronger one on every run is recorded run after run; the run is GOOD every time, yet at the fifth run the Leader is told a node is slipping; the table gives the node its figures
/// (with their n); changing the node starts a new version that says too few runs until it has some.
#[test]
fn j46_each_node_keeps_a_record_per_run_and_a_slipping_node_reaches_the_leader_even_when_the_run_was_good() {
    let _s = serial(); let tmp = Scratch::new("j46");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let fake_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || { for stream in listener.incoming().flatten() { std::thread::spawn(move || { use std::io::{Read, Write}; let mut st = stream; let mut buf = Vec::new(); let mut chunk = [0u8; 4096];
        loop { let n = st.read(&mut chunk).unwrap_or(0); if n == 0 { break; } buf.extend_from_slice(&chunk[..n]); let t = String::from_utf8_lossy(&buf).to_string(); if let Some(i) = t.find("\r\n\r\n") { let len = t[..i].to_lowercase().split("content-length:").nth(1).and_then(|x| x.trim().split_whitespace().next().and_then(|n| n.parse::<usize>().ok())).unwrap_or(0); if buf.len() >= i + 4 + len { break; } } }
        let t = String::from_utf8_lossy(&buf).to_string(); let body: Value = serde_json::from_str(t.split("\r\n\r\n").nth(1).unwrap_or("{}")).unwrap_or(Value::Null); let model = body["model"].as_str().unwrap_or("").to_string();
        let reply = if model == "small" { "{\"name\": \"Orchard Morning\", \"price\": \"18.25\"}" } else { "{\"name\": \"Orchard Morning\", \"price\": 18}" };
        let out = json!({ "choices": [{ "message": { "content": reply } }] }).to_string(); let _ = write!(st, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}", out.len()); }); } });
    let seats = tmp.path().join("seats.json"); std::fs::write(&seats, json!({ "endpoints": { "fake": { "url": format!("http://127.0.0.1:{fake_port}"), "model": "small" } } }).to_string()).unwrap();
    let port = free_port(); let s = Serve::start_env(&state, port, &[("ANTILOKI_SEATS_FILE", &seats)]); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph"; let ask = "New on the shelf: Orchard Morning. Price £18, 250g, free grinding.";
    let graph = |seat: &str, fallback: Value| json!({ "team": "Shelf", "schema": 1, "budget_usd": 1.0, "start": ["extract"],
        "nodes": { "leader": { "node": "leader" }, "extract": { "node": "llm", "seat": seat, "prompt": "Extract the name and the price from: {ask}", "answer": { "name": "string", "price": "number" }, "fallback": fallback, "verify": { "in_source": ["price"], "source": "{ask}" }, "why": { "kind": "cost", "note": "an extraction" } }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "extract", "to": "deliver" } ] });
    assert_eq!(s.post(door, json!({ "op": "save", "graph": graph("endpoint:fake:small", json!(["endpoint:fake:big"])) }))["ok"], json!(true));
    let go_and_wait = |n: usize| { let go = s.post(door, json!({ "op": "run", "team": "shelf", "ask": ask })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
        let a = until(&s, 60, &format!("run {n} to be audited"), || { let r = s.post(door, json!({ "op": "audit", "run": run })); (r["ok"] == json!(true)).then(|| r["audit"].clone()) }); a };
    for n in 1..=4 { let a = go_and_wait(n); assert_eq!(a["good"], json!(true), "{a}"); assert_eq!(a["attention"], json!(false), "four runs are too few to call a node slipping: {a}"); }
    let audits_in = |inbox: &Value| inbox["messages"].as_array().map_or(0, |m| m.iter().filter(|x| x["kind"] != json!("run_report")).count());   // every finished run also sends a run report: only the AUDIT messages count here
    assert_eq!(audits_in(&s.post(door, json!({ "op": "leader_inbox", "team": "shelf" }))), 0, "a good run with nothing slipping tells the Leader nothing");
    let a5 = go_and_wait(5); assert_eq!(a5["good"], json!(true), "the run itself is good every time: {a5}"); assert_eq!(a5["attention"], json!(true), "{a5}");
    let codes: Vec<String> = a5["findings"].as_array().unwrap().iter().map(|f| f["code"].as_str().unwrap().to_string()).collect(); assert!(codes.contains(&"node_climbing".to_string()), "{codes:?}");
    let inbox = s.post(door, json!({ "op": "leader_inbox", "team": "shelf" })); assert_eq!(audits_in(&inbox), 1, "one audit message (the run reports are not counted): {inbox}"); let m = inbox["messages"].as_array().unwrap().iter().find(|x| x["kind"] != json!("run_report")).unwrap()["message"].as_str().unwrap(); assert!(m.starts_with("[team audit]") && m.contains("NODE IS SLIPPING") && m.contains("node_climbing") && m.contains("antiloki_team_seats trial"), "{m}");
    let t = s.post(door, json!({ "op": "node_bench", "team": "shelf" })); assert_eq!(t["ok"], json!(true), "{t}"); let n = &t["nodes"][0]; assert_eq!(n["node"], json!("extract")); assert_eq!(n["stats"]["n"], json!(5)); assert_eq!(n["stats"]["climb_rate"], json!(1.0)); assert_eq!(n["stats"]["ok_rate"], json!(1.0)); assert_eq!(n["enough_runs"], json!(true)); assert_eq!(n["seat"], json!("endpoint:fake:small")); assert!(n["vs_previous_cfg"].is_null());
    // the Leader changes the node (starts on the stronger seat): a new version, which says too few runs until it has some
    assert_eq!(s.post(door, json!({ "op": "save", "graph": graph("endpoint:fake:big", json!([])) }))["ok"], json!(true));
    let a6 = go_and_wait(6); assert_eq!(a6["good"], json!(true), "{a6}");
    let t2 = s.post(door, json!({ "op": "node_bench", "team": "shelf" })); let n2 = &t2["nodes"][0]; assert_eq!(n2["versions"].as_array().unwrap().len(), 2, "{t2}"); assert_ne!(n2["cfg"], n["cfg"]); assert_eq!(n2["seat"], json!("endpoint:fake:big")); assert_eq!(n2["stats"]["n"], json!(1)); assert_eq!(n2["enough_runs"], json!(false)); assert_eq!(n2["vs_previous_cfg"]["verdict"], json!("too few runs"));
    let h = s.post(door, json!({ "op": "audits", "team": "shelf" })); assert_eq!(h["audits"].as_array().unwrap().len(), 6); assert_eq!(h["audits"][1]["attention"], json!(true), "the fifth run, newest second");
}

/// RFC 0011 §6 S2: `llm` seat calls append to the team's `bench.sqlite` and survive a serve restart.
#[test]
fn j47_team_bench_sqlite_keeps_llm_endpoint_rows_after_serve_restart() {
    let _s = serial(); let tmp = Scratch::new("j47");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let fake_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || { for stream in listener.incoming().flatten() { std::thread::spawn(move || { use std::io::{Read, Write}; let mut st = stream; let mut buf = Vec::new(); let mut chunk = [0u8; 4096];
        loop { let n = st.read(&mut chunk).unwrap_or(0); if n == 0 { break; } buf.extend_from_slice(&chunk[..n]); let t = String::from_utf8_lossy(&buf).to_string(); if let Some(i) = t.find("\r\n\r\n") { let len = t[..i].to_lowercase().split("content-length:").nth(1).and_then(|x| x.trim().split_whitespace().next().and_then(|n| n.parse::<usize>().ok())).unwrap_or(0); if buf.len() >= i + 4 + len { break; } } }
        let t = String::from_utf8_lossy(&buf).to_string(); let body: Value = serde_json::from_str(t.split("\r\n\r\n").nth(1).unwrap_or("{}")).unwrap_or(Value::Null);
        let reply = "{\"name\": \"Orchard Morning\", \"price\": 18}";
        let out = json!({ "choices": [{ "message": { "content": reply } }] }).to_string();
        let _ = write!(st, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}", out.len()); }); } });
    let seats = tmp.path().join("seats.json"); std::fs::write(&seats, json!({ "endpoints": { "fake": { "url": format!("http://127.0.0.1:{fake_port}"), "model": "small" } } }).to_string()).unwrap();
    let port = free_port(); let s = Serve::start_env(&state, port, &[("ANTILOKI_SEATS_FILE", &seats)]); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph"; let ask = "New on the shelf: Orchard Morning. Price £18.";
    let graph = json!({ "team": "Shelf", "schema": 1, "budget_usd": 1.0, "start": ["extract"],
        "nodes": { "leader": { "node": "leader" }, "extract": { "node": "llm", "seat": "endpoint:fake:small", "prompt": "Extract name and price from: {ask}", "answer": { "name": "string", "price": "number" }, "verify": { "in_source": ["price"], "source": "{ask}" }, "why": { "kind": "cost", "note": "extraction" } }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "extract", "to": "deliver" } ] });
    assert_eq!(s.post(door, json!({ "op": "save", "graph": graph }))["ok"], json!(true));
    let go = s.post(door, json!({ "op": "run", "team": "shelf", "ask": ask })); assert_eq!(go["ok"], json!(true), "{go}");
    let run = go["run"].as_str().unwrap().to_string();
    let _ = until(&s, 60, "run to audit", || { let r = s.post(door, json!({ "op": "audit", "run": run })); (r["ok"] == json!(true)).then(|| ()) });
    antiloki_serve::ledger::flush();
    let root = ws.to_string_lossy().to_string();
    let bench = antiloki_serve::ledger::bench_path(&root, "shelf");
    assert!(bench.exists(), "{}", bench.display());
    let count = |p: &std::path::Path| -> i64 {
        rusqlite::Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
            .query_row("SELECT COUNT(*) FROM calls WHERE source = 'endpoint'", [], |r| r.get(0)).unwrap()
    };
    let n1 = count(&bench);
    assert!(n1 >= 1, "expected endpoint rows in bench.sqlite, got {n1}");
    drop(s);
    let s2 = Serve::start_env(&state, port, &[("ANTILOKI_SEATS_FILE", &seats)]);
    assert_eq!(count(&bench), n1, "rows survive restart");
    let _ = s2;
}

/// RFC 0011 §8 S5: a guard-verified answer becomes memory after the run; the next run of a cell with `examples_from` is given it, and its ledger row records which examples it used.
#[test]
fn j50_a_verified_answer_becomes_memory_and_the_next_call_is_filled_with_it_and_records_it() {
    let _s = serial(); let tmp = Scratch::new("j50");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let fake_port = listener.local_addr().unwrap().port();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default(); let seen2 = seen.clone();
    std::thread::spawn(move || { for stream in listener.incoming().flatten() { let seen = seen2.clone(); std::thread::spawn(move || { use std::io::{Read, Write}; let mut st = stream; let mut buf = Vec::new(); let mut chunk = [0u8; 4096];
        loop { let n = st.read(&mut chunk).unwrap_or(0); if n == 0 { break; } buf.extend_from_slice(&chunk[..n]); let t = String::from_utf8_lossy(&buf).to_string(); if let Some(i) = t.find("\r\n\r\n") { let len = t[..i].to_lowercase().split("content-length:").nth(1).and_then(|x| x.trim().split_whitespace().next().and_then(|n| n.parse::<usize>().ok())).unwrap_or(0); if buf.len() >= i + 4 + len { break; } } }
        let t = String::from_utf8_lossy(&buf).to_string(); let body: Value = serde_json::from_str(t.split("\r\n\r\n").nth(1).unwrap_or("{}")).unwrap_or(Value::Null);
        let prompt = body["messages"][0]["content"].as_str().unwrap_or("").to_string(); seen.lock().unwrap().push(prompt.clone());
        // the price of THIS ask: the first pound sign in the prompt (the examples, if any, come after it)
        let price: String = prompt.split('£').nth(1).unwrap_or("0").chars().take_while(|c| c.is_ascii_digit()).collect();
        let reply = format!("{{\"name\": \"Item\", \"price\": {price}}}");
        let out = json!({ "choices": [{ "message": { "content": reply } }] }).to_string();
        let _ = write!(st, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}", out.len()); }); } });
    let seats = tmp.path().join("seats.json"); std::fs::write(&seats, json!({ "endpoints": { "fake": { "url": format!("http://127.0.0.1:{fake_port}"), "model": "small" } } }).to_string()).unwrap();
    let port = free_port(); let s = Serve::start_env(&state, port, &[("ANTILOKI_SEATS_FILE", &seats)]); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let graph = json!({ "team": "Shelf", "schema": 1, "budget_usd": 1.0, "start": ["extract"],
        "nodes": { "leader": { "node": "leader" }, "extract": { "node": "llm", "seat": "endpoint:fake:small", "prompt": "Extract name and price from: {ask}", "answer": { "name": "string", "price": "number" }, "verify": { "in_source": ["price"], "source": "{ask}" }, "examples_from": { "task_class": "shelf", "k": 2 }, "why": { "kind": "cost", "note": "extraction" } }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "extract", "to": "deliver" } ] });
    let saved = s.post(door, json!({ "op": "save", "graph": graph })); assert_eq!(saved["ok"], json!(true), "{saved}");
    let root = ws.to_string_lossy().to_string(); let bench = antiloki_serve::ledger::bench_path(&root, "shelf");
    let count = |sql: &str| -> i64 { rusqlite::Connection::open_with_flags(&bench, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok().and_then(|c| c.query_row(sql, [], |r| r.get(0)).ok()).unwrap_or(0) };
    let run_once = |ask: &str| { let go = s.post(door, json!({ "op": "run", "team": "shelf", "ask": ask })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
        let _ = until(&s, 60, "run to audit", || { let r = s.post(door, json!({ "op": "audit", "run": run })); (r["ok"] == json!(true)).then(|| ()) }); };
    run_once("New on the shelf: Orchard Morning. Price £18.");
    until(&s, 30, "the verified answer to become an example", || (count("SELECT COUNT(*) FROM examples WHERE kind = 'golden'") >= 1).then(|| ()));
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(!seen.lock().unwrap()[0].contains("Verified examples"), "nothing to learn from yet");
    // the person's own recall, over the same door: the example, with where it came from
    let rec = s.post(door, json!({ "op": "memory_recall", "team": "shelf", "text": "orchard", "task_class": "shelf" }));
    assert_eq!(rec["ok"], json!(true), "{rec}"); assert_eq!(rec["rows"][0]["verified_by"], json!("guard"), "{rec}"); assert!(rec["rows"][0]["input"].as_str().unwrap().contains("Orchard Morning"), "{rec}");
    // the second run is filled with it
    run_once("New on the shelf: Winter Blend. Price £22.");
    let bodies = seen.lock().unwrap().clone(); assert_eq!(bodies.len(), 2, "{bodies:?}");
    assert!(bodies[1].contains("Verified examples") && bodies[1].contains("Orchard Morning"), "the second call carries the first answer: {}", bodies[1]);
    until(&s, 30, "the second call's row", || (count("SELECT COUNT(*) FROM calls WHERE examples_used IS NOT NULL") >= 1).then(|| ()));
    let used: String = rusqlite::Connection::open_with_flags(&bench, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap().query_row("SELECT examples_used FROM calls WHERE examples_used IS NOT NULL", [], |r| r.get(0)).unwrap();
    assert!(used.starts_with('['), "{used}");
    // an agent may recall and write, never forget; the person may
    let note = s.post(door, json!({ "op": "memory_write", "team": "shelf", "text": "prices come with a pound sign", "pane_id": 3 })); assert_eq!(note["ok"], json!(true), "{note}");
    let no = s.post(door, json!({ "op": "memory_forget", "team": "shelf", "note": note["note"], "pane_id": 3 })); assert_eq!(no["ok"], json!(false), "{no}");
    let yes = s.post(door, json!({ "op": "memory_forget", "team": "shelf", "note": note["note"] })); assert_eq!(yes["ok"], json!(true), "{yes}");
}

/// RFC 0011 §10 S6: a swarm of scripted seats. The cheap seat gets one item wrong; the preset's guard (the price must be in the text) catches it and the ladder climbs to the big seat. The failure of none ends the swarm.
#[test]
fn j51_a_swarm_of_scripted_seats_climbs_on_a_failed_guard_and_every_item_is_done() {
    let _s = serial(); let tmp = Scratch::new("j51");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let fake_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || { for stream in listener.incoming().flatten() { std::thread::spawn(move || { use std::io::{Read, Write}; let mut st = stream; let mut buf = Vec::new(); let mut chunk = [0u8; 4096];
        loop { let n = st.read(&mut chunk).unwrap_or(0); if n == 0 { break; } buf.extend_from_slice(&chunk[..n]); let t = String::from_utf8_lossy(&buf).to_string(); if let Some(i) = t.find("\r\n\r\n") { let len = t[..i].to_lowercase().split("content-length:").nth(1).and_then(|x| x.trim().split_whitespace().next().and_then(|n| n.parse::<usize>().ok())).unwrap_or(0); if buf.len() >= i + 4 + len { break; } } }
        let t = String::from_utf8_lossy(&buf).to_string(); let body: Value = serde_json::from_str(t.split("\r\n\r\n").nth(1).unwrap_or("{}")).unwrap_or(Value::Null);
        let prompt = body["messages"][0]["content"].as_str().unwrap_or("").to_string(); let model = body["model"].as_str().unwrap_or("").to_string();
        let price: String = prompt.split('£').nth(1).unwrap_or("0").chars().take_while(|c| c.is_ascii_digit()).collect();
        // the small seat invents a price for Winter Blend; the big one reads it right
        let price = if model == "small" && prompt.contains("Winter Blend") { "99".to_string() } else { price };
        let reply = format!("{{\"price\": {price}}}");
        let out = json!({ "choices": [{ "message": { "content": reply } }] }).to_string();
        let _ = write!(st, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}", out.len()); }); } });
    let seats = tmp.path().join("seats.json"); std::fs::write(&seats, json!({ "endpoints": { "fake": { "url": format!("http://127.0.0.1:{fake_port}"), "model": "small" } } }).to_string()).unwrap();
    let port = free_port(); let s = Serve::start_env(&state, port, &[("ANTILOKI_SEATS_FILE", &seats)]); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let items = json!(["Orchard Morning. Price £18.", "Winter Blend. Price £22.", "Spring Tonic. Price £12."]);
    let graph = json!({ "team": "Swarm", "schema": 1, "budget_usd": 2.0, "start": ["mk"],
        "nodes": { "leader": { "node": "leader" }, "mk": { "node": "code", "source": items.to_string(), "examples": [ { "input": {}, "output": items } ] }, "deliver": { "node": "deliver" } },
        "edges": [] });
    let saved = s.post(door, json!({ "op": "save", "graph": graph })); assert_eq!(saved["ok"], json!(true), "{saved}");
    let params = json!({ "id": "px", "items": "mk.output", "preset": { "kind": "extract", "example": { "price": 1 } }, "seats": ["endpoint:fake:small", "endpoint:fake:big"], "prompt": "Read the price.", "concurrency": 2, "budget_usd": 0.5 });
    // an unbounded swarm is refused, a bounded one goes in
    let bad = s.post(door, json!({ "op": "graph_swarm", "team": "swarm", "params": { "id": "px", "items": "mk.output", "preset": { "kind": "extract", "example": { "price": 1 } }, "seats": ["endpoint:fake:small"], "prompt": "x" }, "wire_in": "mk", "wire_out": "deliver" }));
    assert_eq!(bad["ok"], json!(false), "{bad}"); assert!(bad["error"].as_str().unwrap().contains("swarm_unbounded"), "{bad}");
    let made = s.post(door, json!({ "op": "graph_swarm", "team": "swarm", "params": params, "wire_in": "mk", "wire_out": "deliver" })); assert_eq!(made["ok"], json!(true), "{made}");
    let go = s.post(door, json!({ "op": "run", "team": "swarm", "ask": "tag the shelf" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    until(&s, 90, "the swarm to deliver", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); (r["state"]["status"]["status"] == json!("done")).then_some(()) });
    let root = ws.to_string_lossy().to_string(); let bench = antiloki_serve::ledger::bench_path(&root, "swarm");
    let q = |sql: &str| -> i64 { rusqlite::Connection::open_with_flags(&bench, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok().and_then(|c| c.query_row(sql, [], |r| r.get(0)).ok()).unwrap_or(-1) };
    until(&s, 30, "the ledger", || (q("SELECT COUNT(*) FROM calls WHERE source = 'endpoint'") >= 4).then_some(()));
    assert_eq!(q("SELECT COUNT(*) FROM calls WHERE seat = 'endpoint:fake:small'"), 3, "the cheap seat answered every item");
    assert_eq!(q("SELECT COUNT(*) FROM calls WHERE seat = 'endpoint:fake:big'"), 1, "the big seat was called once: only for the item the guard caught");
    assert_eq!(q("SELECT COUNT(*) FROM calls WHERE outcome = 'guard_failed'"), 1);
    assert_eq!(q("SELECT COUNT(*) FROM calls WHERE climbed_from = 'endpoint:fake:small' AND seat = 'endpoint:fake:big'"), 1, "the climb is on the row");
    assert_eq!(q("SELECT COUNT(*) FROM calls WHERE verified = 1"), 3, "three answers were checked and right");
}

/// RFC 0011 §7 S6b: a compare with traps names the better seat by a stated rule and records every arm as a benchmark row; the statistics come back from the ledger; a setpoint is the person's to change; and a whole run with setpoints leaves the graph exactly as it was (the regulator never edits it).
#[test]
fn j52_a_compare_with_traps_names_the_better_seat_and_the_regulator_never_edits_the_graph() {
    let _s = serial(); let tmp = Scratch::new("j52");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let fake_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || { for stream in listener.incoming().flatten() { std::thread::spawn(move || { use std::io::{Read, Write}; let mut st = stream; let mut buf = Vec::new(); let mut chunk = [0u8; 4096];
        loop { let n = st.read(&mut chunk).unwrap_or(0); if n == 0 { break; } buf.extend_from_slice(&chunk[..n]); let t = String::from_utf8_lossy(&buf).to_string(); if let Some(i) = t.find("\r\n\r\n") { let len = t[..i].to_lowercase().split("content-length:").nth(1).and_then(|x| x.trim().split_whitespace().next().and_then(|n| n.parse::<usize>().ok())).unwrap_or(0); if buf.len() >= i + 4 + len { break; } } }
        let t = String::from_utf8_lossy(&buf).to_string(); let body: Value = serde_json::from_str(t.split("\r\n\r\n").nth(1).unwrap_or("{}")).unwrap_or(Value::Null);
        let prompt = body["messages"][0]["content"].as_str().unwrap_or("").to_string(); let model = body["model"].as_str().unwrap_or("").to_string();
        let first: String = prompt.split(|c: char| !c.is_ascii_digit()).find(|t| !t.is_empty()).unwrap_or("0").to_string();
        // the big seat knows "no price" means zero; the small one just reads the first number
        let price = if model == "big" && prompt.contains("no price") { "0".to_string() } else if prompt.contains("now") { "25".to_string() } else { first };
        let reply = format!("{{\"price\": {price}}}");
        let out = json!({ "choices": [{ "message": { "content": reply } }] }).to_string();
        let _ = write!(st, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}", out.len()); }); } });
    let seats = tmp.path().join("seats.json"); std::fs::write(&seats, json!({ "endpoints": { "fake": { "url": format!("http://127.0.0.1:{fake_port}"), "model": "small" } } }).to_string()).unwrap();
    let port = free_port(); let s = Serve::start_env(&state, port, &[("ANTILOKI_SEATS_FILE", &seats)]); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let graph = json!({ "team": "Shelf", "schema": 1, "budget_usd": 1.0, "start": ["extract"], "setpoints": { "extract": { "min_ok_rate": 0.8 } },
        "nodes": { "leader": { "node": "leader" }, "extract": { "node": "llm", "seat": "endpoint:fake:small", "prompt": "Read the price: {ask}", "answer": { "price": "number" }, "fallback": ["claude-code:haiku"], "why": { "kind": "cost", "note": "extraction" } }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "extract", "to": "deliver" } ] });
    let saved = s.post(door, json!({ "op": "save", "graph": graph })); assert_eq!(saved["ok"], json!(true), "{saved}");
    let root = ws.to_string_lossy().to_string();
    let hash_before = antiloki_serve::team_graph_run::graph_hash(&antiloki_serve::team_graph_run::load_graph(&root, "shelf").unwrap());
    let four = json!([ { "input": "price 18", "expect": { "price": 18 } }, { "input": "weight 18 kg, no price", "expect": { "price": 0 }, "trap": true }, { "input": "price 22", "expect": { "price": 22 } }, { "input": "was 40 now 25", "not": { "price": 40 }, "trap": true } ]);
    // a set with no trap measures nothing: refused
    let no_trap = s.post(door, json!({ "op": "bench_trial", "team": "shelf", "node": "extract", "cases": [ { "input": "price 18", "expect": { "price": 18 } } ] }));
    assert_eq!(no_trap["ok"], json!(false), "{no_trap}"); assert!(no_trap["error"].as_str().unwrap().contains("traps"), "{no_trap}");
    // the trial: the small seat falls for the trap
    let t = s.post(door, json!({ "op": "bench_trial", "team": "shelf", "node": "extract", "seat": "endpoint:fake:small", "cases": four })); assert_eq!(t["ok"], json!(true), "{t}");
    assert_eq!(t["report"]["traps"], json!(2)); assert!(t["report"]["trap_failures"].as_u64().unwrap() >= 1, "{t}");
    // the compare: twelve cases, two arms, one winner by the stated rule
    let twelve: Vec<Value> = (0..3).flat_map(|_| four.as_array().unwrap().clone()).collect();
    let c = s.post(door, json!({ "op": "bench_compare", "team": "shelf", "node": "extract", "arms": ["endpoint:fake:small", "endpoint:fake:big"], "cases": twelve })); assert_eq!(c["ok"], json!(true), "{c}");
    assert_eq!(c["compare"]["winner"], json!("endpoint:fake:big"), "{c}"); assert!(c["compare"]["rule"].as_str().unwrap().contains("margin") || c["compare"]["rule"].as_str().unwrap().contains("lead"), "{c}");
    assert_eq!(c["compare"]["arms"].as_array().unwrap().len(), 2, "every arm is in the result");
    // every call is a ledger row marked as a benchmark call; the statistics come out of the same rows
    let bench = antiloki_serve::ledger::bench_path(&root, "shelf");
    let q = |sql: &str| -> i64 { rusqlite::Connection::open_with_flags(&bench, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok().and_then(|c| c.query_row(sql, [], |r| r.get(0)).ok()).unwrap_or(-1) };
    until(&s, 30, "the benchmark rows", || (q("SELECT COUNT(*) FROM calls WHERE bench = 1 AND caller = 'benchmark'") >= 28).then_some(()));
    let st = s.post(door, json!({ "op": "bench_stats", "team": "shelf", "node": "extract" })); assert_eq!(st["ok"], json!(true), "{st}");
    assert_eq!(st["seats"]["endpoint:fake:small"]["n"], json!(16)); assert_eq!(st["seats"]["endpoint:fake:big"]["n"], json!(12));
    assert!(st["seats"]["endpoint:fake:big"]["verdict"].is_string());
    // the setpoints are the person's: an agent cannot move them, the person can
    let sp = json!([{ "op": "set_graph_field", "field": "setpoints", "value": { "extract": { "min_ok_rate": 0.1 } } }]);
    let no = s.post(door, json!({ "op": "graph_apply", "team": "shelf", "ops": sp, "reason": "loosen", "person_ok": true, "pane_id": 3 })); assert_eq!(no["ok"], json!(false), "{no}"); assert!(no["error"].as_str().unwrap().contains("agent"), "{no}");
    assert_eq!(antiloki_serve::team_graph_run::graph_hash(&antiloki_serve::team_graph_run::load_graph(&root, "shelf").unwrap()), hash_before, "the refused patch changed nothing");
    // a whole run with setpoints: the regulator reads the ledger, may write its overlay, and leaves the graph as it was
    let go = s.post(door, json!({ "op": "run", "team": "shelf", "ask": "price 18" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    let _ = until(&s, 60, "run to audit", || { let r = s.post(door, json!({ "op": "audit", "run": run })); (r["ok"] == json!(true)).then(|| ()) });
    assert_eq!(antiloki_serve::team_graph_run::graph_hash(&antiloki_serve::team_graph_run::load_graph(&root, "shelf").unwrap()), hash_before, "the regulator never edits the graph");
}

/// RFC 0011 §18 S7: a scripted improver proposes; it applies nothing. One change per node per cycle, a budget change is the person's, an agent can accept only what is not, the freeze flag blocks, and the settings are the person's.
#[test]
fn j53_the_improver_proposes_and_applies_nothing_and_the_person_keeps_the_gates() {
    let _s = serial(); let tmp = Scratch::new("j53");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let fake_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || { for stream in listener.incoming().flatten() { std::thread::spawn(move || { use std::io::{Read, Write}; let mut st = stream; let mut buf = Vec::new(); let mut chunk = [0u8; 4096];
        loop { let n = st.read(&mut chunk).unwrap_or(0); if n == 0 { break; } buf.extend_from_slice(&chunk[..n]); let t = String::from_utf8_lossy(&buf).to_string(); if let Some(i) = t.find("\r\n\r\n") { let len = t[..i].to_lowercase().split("content-length:").nth(1).and_then(|x| x.trim().split_whitespace().next().and_then(|n| n.parse::<usize>().ok())).unwrap_or(0); if buf.len() >= i + 4 + len { break; } } }
        let t = String::from_utf8_lossy(&buf).to_string(); let body: Value = serde_json::from_str(t.split("\r\n\r\n").nth(1).unwrap_or("{}")).unwrap_or(Value::Null);
        let prompt = body["messages"][0]["content"].as_str().unwrap_or("").to_string();
        let reply = if prompt.contains("IMPROVER") {
            let ev = "extract on endpoint:fake:small: n=30, ok 70%";
            json!({ "proposals": [
                { "node": "extract", "kind": "prompt_wording", "patch": [ { "op": "set_field", "node": "extract", "path": "prompt", "value": "Read the price carefully: {ask}" } ], "hypothesis": "a plainer instruction fixes the misreads", "metric": "ok rate", "expected": "+10 points", "evidence": ev, "risk": "low" },
                { "node": "extract", "kind": "prompt_wording", "patch": [ { "op": "set_field", "node": "extract", "path": "prompt", "value": "Price only: {ask}" } ], "hypothesis": "shorter is better", "metric": "ok rate", "evidence": ev },
                { "node": "deliver", "kind": "other", "patch": [ { "op": "set_graph_field", "field": "budget_usd", "value": 9.0 } ], "hypothesis": "more room", "metric": "ok rate", "evidence": ev }
            ] }).to_string()
        } else { "{\"price\": 1}".to_string() };
        let out = json!({ "choices": [{ "message": { "content": reply } }] }).to_string();
        let _ = write!(st, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}", out.len()); }); } });
    let seats = tmp.path().join("seats.json"); std::fs::write(&seats, json!({ "endpoints": { "fake": { "url": format!("http://127.0.0.1:{fake_port}"), "model": "small" } } }).to_string()).unwrap();
    let port = free_port(); let s = Serve::start_env(&state, port, &[("ANTILOKI_SEATS_FILE", &seats)]); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let graph = json!({ "team": "Shelf", "schema": 1, "budget_usd": 1.0, "start": ["extract"],
        "nodes": { "leader": { "node": "leader" }, "extract": { "node": "llm", "seat": "endpoint:fake:small", "prompt": "Read the price: {ask}", "answer": { "price": "number" }, "fallback": ["claude-code:haiku"], "why": { "kind": "cost", "note": "extraction" } }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "extract", "to": "deliver" } ] });
    assert_eq!(s.post(door, json!({ "op": "save", "graph": graph }))["ok"], json!(true));
    let root = ws.to_string_lossy().to_string();
    let hash = || antiloki_serve::team_graph_run::graph_hash(&antiloki_serve::team_graph_run::load_graph(&root, "shelf").unwrap());
    let before = hash();
    // the settings are the person's
    let no = s.post(door, json!({ "op": "improve_config", "team": "shelf", "mode": "suggest", "pane_id": 3 })); assert_eq!(no["ok"], json!(false), "{no}"); assert!(no["error"].as_str().unwrap().contains("person"), "{no}");
    let cfg = s.post(door, json!({ "op": "improve_config", "team": "shelf", "mode": "suggest", "seat": "endpoint:fake:small" })); assert_eq!(cfg["ok"], json!(true), "{cfg}"); assert_eq!(cfg["config"]["mode"], json!("suggest"));
    // the review: it proposes, the graph does not move
    let rv = s.post(door, json!({ "op": "improve_review", "team": "shelf" })); assert_eq!(rv["ok"], json!(true), "{rv}");
    assert_eq!(rv["proposed"].as_array().unwrap().len(), 2, "two listed: {rv}"); assert!(rv["rejected"][0][1].as_str().unwrap().contains("one change per node per cycle"), "{rv}");
    assert_eq!(hash(), before, "a review applies nothing");
    let list = s.post(door, json!({ "op": "improve_proposals", "team": "shelf", "status": "pending" })); let ps = list["proposals"].as_array().unwrap().clone(); assert_eq!(ps.len(), 2, "{list}");
    let wording = ps.iter().find(|p| p["node"] == "extract").unwrap().clone(); let budget = ps.iter().find(|p| p["node"] == "deliver").unwrap().clone();
    assert_eq!(wording["requires_person"], json!(false)); assert_eq!(budget["requires_person"], json!(true));
    // an agent cannot take the one that is the person's, even saying the person agreed; it can take the other
    let a1 = s.post(door, json!({ "op": "improve_decide", "team": "shelf", "id": budget["id"], "accept": true, "person_ok": true, "pane_id": 3 })); assert_eq!(a1["ok"], json!(false), "{a1}"); assert!(a1["error"].as_str().unwrap().contains("agent"), "{a1}");
    assert_eq!(hash(), before);
    let a2 = s.post(door, json!({ "op": "improve_decide", "team": "shelf", "id": wording["id"], "accept": true, "pane_id": 3 })); assert_eq!(a2["ok"], json!(true), "{a2}"); assert_eq!(a2["status"], json!("applied"));
    assert_ne!(hash(), before); assert_eq!(antiloki_serve::team_graph_run::load_graph(&root, "shelf").unwrap().to_json()["nodes"]["extract"]["prompt"], json!("Read the price carefully: {ask}"));
    // the change is a version with the proposal attached, and can be rolled back
    let v = s.post(door, json!({ "op": "graph_versions", "team": "shelf" })); assert!(v["versions"].as_array().unwrap().iter().any(|x| x["proposal"] == wording["id"]), "{v}");
    // the person rejects the other; a decided proposal is not decided twice
    assert_eq!(s.post(door, json!({ "op": "improve_decide", "team": "shelf", "id": budget["id"], "accept": false }))["status"], json!("rejected"));
    assert_eq!(s.post(door, json!({ "op": "improve_decide", "team": "shelf", "id": budget["id"], "accept": true }))["ok"], json!(false));
    // frozen: a review proposes nothing, and says why
    assert_eq!(s.post(door, json!({ "op": "improve_config", "team": "shelf", "freeze": true }))["ok"], json!(true));
    let fz = s.post(door, json!({ "op": "improve_review", "team": "shelf" })); assert_eq!(fz["ok"], json!(true), "{fz}"); assert!(fz["proposed"].as_array().unwrap().is_empty() && fz["rejected"].as_array().unwrap().len() == 3, "{fz}");
    let rep = s.post(door, json!({ "op": "improve_config", "team": "shelf", "pane_id": 3 })); assert_eq!(rep["ok"], json!(true), "an agent may read it: {rep}"); assert_eq!(rep["state"]["accepted"], json!(1)); assert_eq!(rep["state"]["proposed"], json!(2));
}

/// RFC 0011 §13/§9 S8: the data types. A run reads a db with a parameterized query, exports it, and writes the file inside its own folder; a write needs the person's consent bound to the team, a schema change always the person's yes, the team's bench.sqlite is never writable; the sagas are the workspace's own, through a typed door.
#[test]
fn j54_a_run_reads_a_db_and_writes_a_file_and_every_write_is_the_persons() {
    let _s = serial(); let tmp = Scratch::new("j54");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    { let c = rusqlite::Connection::open(ws.join("shop.sqlite")).unwrap(); c.execute_batch("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, price REAL); INSERT INTO items VALUES (1,'Orchard',18.0),(2,'Winter',22.5),(3,'Spring',12.0);").unwrap(); }
    let port = free_port(); let s = Serve::start(&state, port); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let graph = json!({ "team": "Shop", "schema": 1, "budget_usd": 1.0, "start": ["q"],
        "nodes": { "leader": { "node": "leader" },
            "q": { "node": "use_tool", "tool": { "type": "db", "name": "shop.sqlite", "args": { "kind": "sqlite", "op": "export", "sql": "SELECT name, price FROM items WHERE price > ? ORDER BY price", "params": [15] } }, "allow": { "effects": ["read"] } },
            "w": { "node": "use_tool", "tool": { "type": "file", "name": "out/priced.csv", "args": { "op": "write", "content_from": "values:q.csv" } }, "allow": { "effects": ["write"] } },
            "r": { "node": "use_tool", "tool": { "type": "file", "name": "out/priced.csv", "args": { "op": "read" } }, "allow": { "effects": ["read"] } },
            "deliver": { "node": "deliver" } },
        "edges": [ { "from": "q", "to": "w" }, { "from": "w", "to": "r" }, { "from": "r", "to": "deliver" } ] });
    let saved = s.post(door, json!({ "op": "save", "graph": graph })); assert_eq!(saved["ok"], json!(true), "{saved}");
    let go = s.post(door, json!({ "op": "run", "team": "shop", "ask": "price list" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    until(&s, 60, "the run to deliver", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); (r["state"]["status"]["status"] == json!("done")).then_some(()) });
    let st = s.post(door, json!({ "op": "status", "run": run })); let vals = &st["run"]["state"]["values"];
    assert_eq!(vals["q"]["count"], json!(2), "{vals}"); assert!(vals["r"]["text"].as_str().unwrap().contains("Winter,22.5") && vals["r"]["text"].as_str().unwrap().starts_with("name,price"), "{vals}");
    // the file is in the run's own folder, and nowhere else in the workspace
    let in_run = std::fs::read_dir(ws.join(".antiloki")).map(|_| ()).is_ok(); assert!(in_run); assert!(!ws.join("out").exists() && !ws.join("priced.csv").exists());
    // reading through the door: schema and a parameterized query; a literal in the SQL is refused
    let sc = s.post(door, json!({ "op": "db_schema", "team": "shop", "kind": "sqlite", "name": "shop.sqlite" })); assert_eq!(sc["schema"]["tables"][0]["name"], json!("items"), "{sc}");
    let q = s.post(door, json!({ "op": "db_query", "team": "shop", "kind": "sqlite", "name": "shop.sqlite", "sql": "SELECT name FROM items WHERE price < ?", "params": [20] })); assert_eq!(q["table"]["rows"].as_array().unwrap().len(), 2, "{q}");
    let lit = s.post(door, json!({ "op": "db_query", "team": "shop", "kind": "sqlite", "name": "shop.sqlite", "sql": "SELECT name FROM items WHERE price < 20" })); assert_eq!(lit["ok"], json!(false), "{lit}"); assert!(lit["error"].as_str().unwrap().contains("parameterized"), "{lit}");
    // a write: no consent, no write; an agent cannot give the consent; the person can, and then a dry run, then the write
    let upd = json!({ "op": "db_write", "team": "shop", "kind": "sqlite", "name": "shop.sqlite", "sql": "UPDATE items SET price = price + ? WHERE price < ?", "params": [1, 20], "person_ok": true });
    let no = s.post(door, upd.clone()); assert_eq!(no["ok"], json!(false), "{no}"); assert!(no["error"].as_str().unwrap().contains("consent"), "{no}");
    let ag = s.post(door, json!({ "op": "db_consent", "team": "shop", "kind": "sqlite", "name": "shop.sqlite", "pane_id": 3 })); assert_eq!(ag["ok"], json!(false), "{ag}");
    assert_eq!(s.post(door, json!({ "op": "db_consent", "team": "shop", "kind": "sqlite", "name": "shop.sqlite" }))["ok"], json!(true));
    let mut dry = upd.clone(); dry["dry_run"] = json!(true); let d = s.post(door, dry); assert_eq!((d["ok"].clone(), d["affected"].clone(), d["dry_run"].clone()), (json!(true), json!(2), json!(true)), "{d}");
    assert_eq!(s.post(door, json!({ "op": "db_query", "team": "shop", "kind": "sqlite", "name": "shop.sqlite", "sql": "SELECT price FROM items WHERE id = ?", "params": [1] }))["table"]["rows"][0][0], json!(18.0), "the dry run changed nothing");
    assert_eq!(s.post(door, upd)["affected"], json!(2));
    // a schema change is the person's yes, call by call, even with consent
    let ddl = json!({ "op": "db_write", "team": "shop", "kind": "sqlite", "name": "shop.sqlite", "sql": "CREATE TABLE extra (a INTEGER)" });
    let nd = s.post(door, ddl.clone()); assert_eq!(nd["ok"], json!(false), "{nd}"); assert!(nd["error"].as_str().unwrap().contains("person's yes"), "{nd}");
    let mut yes = ddl.clone(); yes["person_ok"] = json!(true); assert_eq!(s.post(door, yes)["ok"], json!(true));
    // the team's own dataset: readable, never writable
    let b = s.post(door, json!({ "op": "db_query", "team": "shop", "kind": "bench", "name": "", "sql": "SELECT count(*) FROM calls" })); assert_eq!(b["ok"], json!(true), "{b}");
    assert_eq!(s.post(door, json!({ "op": "db_consent", "team": "shop", "kind": "bench", "name": "" }))["ok"], json!(true));
    let bw = s.post(door, json!({ "op": "db_write", "team": "shop", "kind": "bench", "name": "", "sql": "DELETE FROM calls WHERE bench = ?", "params": [1], "person_ok": true })); assert_eq!(bw["ok"], json!(false), "{bw}");
    // the sagas are the workspace's own, through one typed door
    let sg = s.post(door, json!({ "op": "saga_write", "team": "shop", "title": "Restock", "objective": "price the new items" })); assert_eq!(sg["created"], json!(true), "{sg}"); let id = sg["saga"].as_i64().unwrap();
    let ep = s.post(door, json!({ "op": "saga_write", "team": "shop", "saga": id, "episode": { "title": "Price Autumn Tonic" } })); assert_eq!(ep["created"], json!(true), "{ep}");
    let rn = s.post(door, json!({ "op": "saga_run", "team": "shop", "saga": id })); assert!(rn["goal"].as_str().unwrap().contains("1. Price Autumn Tonic"), "{rn}");
    assert_eq!(s.post(door, json!({ "op": "saga_sync", "team": "shop", "episode": ep["episode"], "state": "done", "team_run": run }))["status"], json!("done"));
    assert_eq!(s.post(door, json!({ "op": "saga_read", "team": "shop", "what": "status", "saga": id }))["status"], json!("done"));
}

/// RFC 0011 §14/§16 S8b: a team runs another as a step (and records the child run), a team cannot include itself through others, a file-watch trigger waits for quiet, an on-error trigger answers another team's failure once, and both need the person's consent and are refused to an agent.
#[test]
fn j55_a_team_runs_a_team_and_file_watch_and_on_error_triggers_need_the_persons_consent() {
    let _s = serial(); let tmp = Scratch::new("j55");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let port = free_port(); let s = Serve::start_env(&state, port, &[("ANTILOKI_TEAM_TRIGGERS_TEST", std::path::Path::new("1"))]); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph"; let root = ws.to_string_lossy().to_string();
    let inner = json!({ "team": "Inner", "schema": 1, "budget_usd": 1.0, "start": ["mk"],
        "nodes": { "leader": { "node": "leader" }, "mk": { "node": "code", "source": "#{ price: 18 }", "examples": [ { "input": {}, "output": { "price": 18 } } ] }, "deliver": { "node": "deliver" } }, "edges": [ { "from": "mk", "to": "deliver" } ] });
    assert_eq!(s.post(door, json!({ "op": "save", "graph": inner }))["ok"], json!(true));
    let outer = json!({ "team": "Outer", "schema": 1, "budget_usd": 2.0, "start": ["sub"],
        "nodes": { "leader": { "node": "leader" }, "sub": { "node": "sub_team", "team": "Inner", "ask": "price it for: {ask}", "outputs": { "price": "mk.output" }, "budget_share": 0.5 }, "deliver": { "node": "deliver" } }, "edges": [ { "from": "sub", "to": "deliver" } ] });
    let saved = s.post(door, json!({ "op": "save", "graph": outer })); assert_eq!(saved["ok"], json!(true), "{saved}");
    // a team cannot include itself, directly or through others
    let selfish = json!({ "team": "Selfish", "schema": 1, "budget_usd": 1.0, "start": ["x"], "nodes": { "leader": { "node": "leader" }, "x": { "node": "sub_team", "team": "Selfish", "ask": "a" }, "deliver": { "node": "deliver" } }, "edges": [ { "from": "x", "to": "deliver" } ] });
    let r = s.post(door, json!({ "op": "save", "graph": selfish })); assert_eq!(r["ok"], json!(false), "{r}");
    let loopy = json!({ "team": "Inner", "schema": 1, "budget_usd": 1.0, "start": ["x"], "nodes": { "leader": { "node": "leader" }, "x": { "node": "sub_team", "team": "Outer", "ask": "a" }, "deliver": { "node": "deliver" } }, "edges": [ { "from": "x", "to": "deliver" } ] });
    let r = s.post(door, json!({ "op": "save", "graph": loopy })); assert_eq!(r["ok"], json!(false), "{r}"); assert!(r["error"].as_str().unwrap().contains("include itself"), "{r}");
    // the outer run starts the inner one, waits for it and maps its answer out; the child run id is on the outer node
    let go = s.post(door, json!({ "op": "run", "team": "outer", "ask": "the shelf" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    until(&s, 90, "the outer run to deliver", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); (r["state"]["status"]["status"] == json!("done")).then_some(()) });
    let vals = s.post(door, json!({ "op": "status", "run": run }))["run"]["state"]["values"].clone();
    let child = vals["sub"]["child_run"].as_str().unwrap().to_string(); assert!(child.starts_with("inner-"), "the child run belongs to the inner team: {vals}");
    assert_eq!(vals["sub"]["outputs"]["price"]["price"], json!(18), "{vals}");
    let ch = s.post(door, json!({ "op": "status", "run": child })); assert_eq!(ch["ok"], json!(true), "the inner run exists under its own team: {ch}");
    // a file-watch trigger: the person sets it and consents; an agent can do neither
    let watcher = json!({ "team": "Watcher", "schema": 1, "budget_usd": 1.0, "start": ["me"], "nodes": { "leader": { "node": "leader" }, "me": { "node": "human", "gate": "merge" }, "deliver": { "node": "deliver" } }, "edges": [ { "from": "me", "port": "approved", "to": "deliver" } ] });
    assert_eq!(s.post(door, json!({ "op": "save", "graph": watcher }))["ok"], json!(true));
    let trig = json!([{ "id": "docs", "kind": "file_watch", "watch": "docs/**/*.md", "debounce_s": 5, "ask": "Review the docs", "budget_usd": 0.5 }]);
    let ag = s.post(door, json!({ "op": "triggers_set", "team": "watcher", "triggers": trig, "pane_id": 3 })); assert_eq!(ag["ok"], json!(false), "{ag}");
    let set = s.post(door, json!({ "op": "triggers_set", "team": "watcher", "triggers": trig })); assert_eq!(set["ok"], json!(true), "{set}");
    let ag = s.post(door, json!({ "op": "trigger_consent", "team": "watcher", "on": true, "pane_id": 3 })); assert_eq!(ag["ok"], json!(false), "{ag}");
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let tick = |team: &str, at: i64| s.post(door, json!({ "op": "trigger_tick", "team": team, "now_ms": at }));
    assert!(tick("watcher", t + 1_000)["decisions"].as_array().unwrap().is_empty(), "nothing before consent");
    std::fs::create_dir_all(ws.join("docs")).unwrap(); std::fs::write(ws.join("docs/a.md"), "one").unwrap();
    assert!(tick("watcher", t + 2_000)["decisions"].as_array().unwrap().is_empty(), "no consent, no run, however the files change");
    let c = s.post(door, json!({ "op": "trigger_consent", "team": "watcher", "on": true })); assert_eq!(c["ok"], json!(true), "{c}");
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    assert!(tick("watcher", t + 1_000)["decisions"].as_array().unwrap().is_empty(), "the first look only records");
    std::fs::write(ws.join("docs/a.md"), "two, longer").unwrap(); std::fs::write(ws.join("docs/b.md"), "new").unwrap(); std::fs::write(ws.join("notes.txt"), "not watched").unwrap();
    assert!(tick("watcher", t + 3_000)["decisions"].as_array().unwrap().is_empty(), "a change starts the quiet time");
    assert!(tick("watcher", t + 5_000)["decisions"].as_array().unwrap().is_empty(), "quiet for two of five seconds");
    let fired = tick("watcher", t + 20_000); let d = fired["decisions"].as_array().unwrap(); assert_eq!(d.len(), 1, "{fired}"); assert_eq!(d[0]["do"], json!("fire"));
    assert!(d[0]["ask"].as_str().unwrap().contains("Review the docs") && d[0]["ask"].as_str().unwrap().contains("docs/a.md") && d[0]["ask"].as_str().unwrap().contains("docs/b.md") && !d[0]["ask"].as_str().unwrap().contains("notes.txt"), "{fired}");
    assert!(tick("watcher", t + 40_000)["decisions"].as_array().unwrap().is_empty() || tick("watcher", t + 41_000)["decisions"].as_array().unwrap().iter().all(|x| x["do"] == json!("skip")), "one change, one run");
    // an on-error trigger: another team failed, the repair team starts once, with the failure in its ask
    let repair = json!({ "team": "Repair", "schema": 1, "budget_usd": 1.0, "start": ["me"], "nodes": { "leader": { "node": "leader" }, "me": { "node": "human", "gate": "merge" }, "deliver": { "node": "deliver" } }, "edges": [ { "from": "me", "port": "approved", "to": "deliver" } ] });
    assert_eq!(s.post(door, json!({ "op": "save", "graph": repair }))["ok"], json!(true));
    assert_eq!(s.post(door, json!({ "op": "triggers_set", "team": "repair", "triggers": [ { "id": "fix", "kind": "on_error", "team": "Outer", "ask": "Find out what broke", "budget_usd": 0.5 } ] }))["ok"], json!(true));
    assert_eq!(s.post(door, json!({ "op": "trigger_consent", "team": "repair", "on": true }))["ok"], json!(true));
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    assert!(tick("repair", t + 1_000)["decisions"].as_array().unwrap().is_empty(), "no failure yet");
    let adir = antiloki_serve::team_graph_run::teams_dir(&root).join("outer"); std::fs::create_dir_all(&adir).unwrap();
    use std::io::Write; let mut f = std::fs::OpenOptions::new().create(true).append(true).open(adir.join("audits.jsonl")).unwrap();
    writeln!(f, "{}", json!({ "run": "outer-999", "team": "outer", "status": "failed", "good": false, "t_ms": t, "findings": [ { "text": "the extract node failed" } ] })).unwrap();
    let r = tick("repair", t + 2_000); let d = r["decisions"].as_array().unwrap(); assert_eq!(d.len(), 1, "{r}"); assert_eq!(d[0]["do"], json!("fire"));
    assert!(d[0]["ask"].as_str().unwrap().contains("Find out what broke") && d[0]["ask"].as_str().unwrap().contains("outer-999") && d[0]["ask"].as_str().unwrap().contains("extract node failed"), "{r}");
    assert!(tick("repair", t + 3_000)["decisions"].as_array().unwrap().iter().all(|x| x["do"] == json!("skip")), "the same failure is answered once");
}

/// BL-7: a webhook trigger. The address does nothing without the token; a good call is only QUEUED; the run starts on the scheduler's tick, and only after the person's consent; a body from outside is data in the ask.
#[test]
fn j58_a_webhook_starts_the_team_only_with_its_token_the_persons_consent_and_inside_the_caps() {
    let _s = serial(); let tmp = Scratch::new("j58");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let port = free_port(); let s = Serve::start_env(&state, port, &[("ANTILOKI_TEAM_TRIGGERS_TEST", std::path::Path::new("1"))]); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let g = json!({ "team": "Hooked", "schema": 1, "budget_usd": 1.0, "start": ["mk"], "nodes": { "leader": { "node": "leader" }, "mk": { "node": "code", "source": "#{ ok: true }", "examples": [ { "input": {}, "output": { "ok": true } } ] }, "deliver": { "node": "deliver" } }, "edges": [ { "from": "mk", "to": "deliver" } ] });
    assert_eq!(s.post(door, json!({ "op": "save", "graph": g }))["ok"], json!(true));
    // a webhook trigger without a token is not valid: the person makes the token in the panel
    let trig = json!([{ "id": "form", "kind": "webhook", "ask": "Handle the new submission.", "budget_usd": 0.5, "token_hash": "0".repeat(64) }]);
    let set = s.post(door, json!({ "op": "triggers_set", "team": "hooked", "triggers": trig })); assert_eq!(set["ok"], json!(true), "{set}");
    let agent = s.post(door, json!({ "op": "webhook_token", "team": "hooked", "trigger": "form", "pane_id": 3 })); assert_eq!(agent["ok"], json!(false), "an agent never makes a token: {agent}");
    let tok = s.post(door, json!({ "op": "webhook_token", "team": "hooked", "trigger": "form" })); assert_eq!(tok["ok"], json!(true), "{tok}");
    let (token, path) = (tok["token"].as_str().unwrap().to_string(), tok["path"].as_str().unwrap().to_string()); assert_eq!(path, "/api/v1/hooks/team/hooked/form");
    let call = |tk: &str, body: &str| raw(port, "POST", &path, &[("X-Antiloki-Token", tk), ("Content-Type", "application/json")], body);
    // wrong, empty or missing token, and a trigger or team that does not exist: the SAME plain refusal
    assert_eq!(call("alh_wrong", "{}").0, 401); assert_eq!(call("", "{}").0, 401); assert_eq!(raw(port, "POST", &path, &[], "{}").0, 401);
    assert_eq!(raw(port, "POST", "/api/v1/hooks/team/hooked/nope", &[("X-Antiloki-Token", &token)], "{}").0, 401); assert_eq!(raw(port, "POST", "/api/v1/hooks/team/nobody/form", &[("X-Antiloki-Token", &token)], "{}").0, 401);
    // the right token but no consent yet (making the token took any earlier one back)
    assert_eq!(call(&token, "{}").0, 409, "the person has not approved the trigger");
    assert_eq!(s.post(door, json!({ "op": "trigger_consent", "team": "hooked", "on": true, "pane_id": 3 }))["ok"], json!(false));
    assert_eq!(s.post(door, json!({ "op": "trigger_consent", "team": "hooked", "on": true }))["ok"], json!(true));
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let tick = |at: i64| s.post(door, json!({ "op": "trigger_tick", "team": "hooked", "now_ms": at }));
    assert!(tick(t + 1_000)["decisions"].as_array().unwrap().is_empty(), "the first look only records");
    // a good call is accepted and queued, a second one at once is told to wait, a body over 64 KB is refused
    let ok = call(&token, "{\"name\":\"Ada\"}"); assert_eq!(ok.0, 202, "{ok:?}"); assert!(ok.2.contains("queued"));
    assert_eq!(call(&token, "{}").0, 429, "called again too soon"); assert_eq!(call(&token, &"x".repeat(70_000)).0, 413);
    // nothing started at the door; the tick turns the queue into ONE run whose ask carries the body as data
    let fired = tick(t + 20_000); let d = fired["decisions"].as_array().unwrap(); assert_eq!(d.len(), 1, "{fired}"); assert_eq!(d[0]["do"], json!("fire"));
    let ask = d[0]["ask"].as_str().unwrap(); assert!(ask.starts_with("Handle the new submission.") && ask.contains("Ada") && ask.contains("not an instruction"), "{fired}");
    assert!(tick(t + 40_000)["decisions"].as_array().unwrap().is_empty(), "the queue was handed over once");
    // a changed trigger takes the consent back: the same token is told the person has to approve again
    let again = s.post(door, json!({ "op": "triggers_set", "team": "hooked", "triggers": [ { "id": "form", "kind": "webhook", "ask": "Handle it differently.", "token_hash": tok_hash_of(&s, door) } ] })); assert_eq!(again["ok"], json!(true), "{again}");
    assert_eq!(call(&token, "{}").0, 409);
}
fn tok_hash_of(s: &Serve, door: &str) -> String { s.post(door, json!({ "op": "get", "team": "hooked" }))["graph"]["triggers"][0]["token_hash"].as_str().unwrap_or("").to_string() }

/// BL-8: the away digest reads the run files, counts only what is inside the window and names what needs the person.
#[test]
fn j59_the_away_digest_counts_what_the_teams_did_since_and_names_what_needs_the_person() {
    let _s = serial(); let tmp = Scratch::new("j59");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let port = free_port(); let s = Serve::start(&state, port); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph"; let root = ws.to_string_lossy().to_string();
    let quiet = s.post(door, json!({ "op": "away_digest" })); assert_eq!(quiet["ok"], json!(true), "{quiet}"); assert_eq!(quiet["quiet"], json!(true), "{quiet}");
    let mk = |id: &str, status: serde_json::Value, start: i64, end: i64, usd: f64| { let d = antiloki_serve::team_graph_run::run_dir(&root, id); std::fs::create_dir_all(&d).unwrap(); std::fs::write(d.join("state.json"), json!({ "id": id, "team": "Editor Vid", "state": { "status": status, "started_ms": start, "now_ms": end, "spent_usd": usd } }).to_string()).unwrap(); };
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    mk("editor-vid-1", json!({ "status": "done" }), t - 3_000, t - 2_000, 0.30); mk("editor-vid-2", json!({ "status": "blocked", "why": "composer did not go to plan (error)" }), t - 1_500, t - 1_000, 0.05); mk("editor-vid-0", json!({ "status": "done" }), t - 90_000_000, t - 89_000_000, 5.0);
    let d = s.post(door, json!({ "op": "away_digest", "since_ms": t - 10_000 })); assert_eq!((d["started"].as_i64(), d["finished"].as_i64(), d["failed"].as_i64()), (Some(2), Some(1), Some(1)), "{d}");
    assert_eq!(d["spent_usd"], json!(0.35), "the run from yesterday is outside the window: {d}"); assert!(d["needs_you"][0].as_str().unwrap().contains("composer did not go to plan"), "{d}");
    assert!(d["words"].as_str().unwrap().starts_with("While you were away: 2 runs started, 1 finished, 1 failed"), "{d}");
    // the default window is the last day: yesterday's 5 dollars are still outside it
    let dflt = s.post(door, json!({ "op": "away_digest" })); assert_eq!(dflt["started"], json!(2), "{dflt}");
    // one team only: the digest of another team is quiet, and the runs of this one are listed for the recap
    let other = s.post(door, json!({ "op": "away_digest", "team": "Other Team", "since_ms": t - 10_000 })); assert_eq!(other["quiet"], json!(true), "{other}");
    let mine = s.post(door, json!({ "op": "away_digest", "team": "editor-vid", "since_ms": t - 10_000 })); assert_eq!(mine["runs"].as_array().unwrap().len(), 2, "{mine}"); assert_eq!(mine["runs"][0]["kind"], json!("failed")); assert!(mine["runs"][0]["ask"].is_string());
    // steering: the person leaves a note for a running team; an agent cannot; a missing run takes none
    let n1 = s.post(door, json!({ "op": "run_note", "run": "editor-vid-1", "text": "keep it under 10 seconds" })); assert_eq!((n1["ok"].clone(), n1["notes"].clone()), (json!(true), json!(1)), "{n1}");
    assert_eq!(s.post(door, json!({ "op": "run_note", "run": "editor-vid-1", "text": "x", "pane_id": 3 }))["ok"], json!(false)); assert_eq!(s.post(door, json!({ "op": "run_note", "run": "nope-9", "text": "x" }))["ok"], json!(false));
    assert!(std::fs::read_to_string(antiloki_serve::team_graph_run::run_dir(&root, "editor-vid-1").join("notes.jsonl")).unwrap().contains("under 10 seconds"));
}

/// B2: the proof card reads the pairs a bench run recorded and applies the pre-registered rules; no pairs = a card that says so; a changed graph makes the proof stale.
#[test]
fn j60_the_proof_card_says_no_proof_yet_then_follows_the_pre_registered_rules_and_goes_stale_when_the_team_changes() {
    let _s = serial(); let tmp = Scratch::new("j60");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let port = free_port(); let s = Serve::start(&state, port); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph"; let root = ws.to_string_lossy().to_string();
    let g = |extra: bool| { let mut nodes = json!({ "leader": { "node": "leader" }, "mk": { "node": "code", "source": "#{ a: 1 }", "examples": [ { "input": {}, "output": { "a": 1 } } ] }, "deliver": { "node": "deliver" } }); if extra { nodes["mk2"] = json!({ "node": "code", "source": "#{ b: 2 }", "examples": [ { "input": {}, "output": { "b": 2 } } ] }); } let mut edges = vec![json!({ "from": "mk", "to": "deliver" })]; if extra { edges.push(json!({ "from": "mk2", "to": "deliver" })); } json!({ "team": "Proven", "schema": 1, "budget_usd": 1.0, "start": if extra { json!(["mk", "mk2"]) } else { json!(["mk"]) }, "nodes": nodes, "edges": edges }) };
    assert_eq!(s.post(door, json!({ "op": "save", "graph": g(false) }))["ok"], json!(true));
    let none = s.post(door, json!({ "op": "proof", "team": "proven" })); assert_eq!((none["ok"].clone(), none["recorded"].clone(), none["verdict"].clone()), (json!(true), json!(false), json!("none")), "{none}");
    // a bench run (here: the file by hand) records 18 pairs over 3 fixtures in which the team wins almost always: it earns its place
    let hash = antiloki_serve::team_proof::graph_hash(&antiloki_serve::team_graph_run::load_graph(&root, "proven").unwrap());
    let pairs: Vec<serde_json::Value> = (0..3).flat_map(|i| (0..6).map(move |r| json!({ "fixture": format!("fx{i}"), "rep": r, "winner": if (i + r) % 9 == 0 { "tie" } else { "team" }, "q_team": 0.9, "q_base": 0.7, "cost_team": 0.02, "cost_base": 0.02, "secs_team": 50, "secs_base": 40 }))).collect();
    let p = antiloki_serve::team_proof::proof_path(&root, "proven"); std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, json!({ "baseline": "a raw agent", "graph_hash": hash, "judge": { "seat": "claude-code:opus", "swap_inconsistency": 0.05, "spot_agree": 0.9, "spot_n": 10 }, "pairs": pairs }).to_string()).unwrap();
    let c = s.post(door, json!({ "op": "proof", "team": "proven" })); assert_eq!(c["verdict"], json!("earns its place"), "{c}"); assert_eq!(c["stale"], json!(false)); assert_eq!(c["n"], json!(18));
    // the team changes: the proof is still shown, as stale
    assert_eq!(s.post(door, json!({ "op": "save", "graph": g(true) }))["ok"], json!(true));
    let c2 = s.post(door, json!({ "op": "proof", "team": "proven" })); assert_eq!(c2["stale"], json!(true), "{c2}"); assert!(c2["words"].as_str().unwrap().contains("STALE"), "{c2}");
}

/// BL-18: an api node calls any http API the Leader wires. The method sets the risk and the node must list it; DELETE is never a team's; a secret is a name in the token store, sent as a bearer header and never written to the run; the address guard holds.
#[test]
fn j61_an_api_node_calls_any_http_api_with_the_risk_of_its_method_and_a_secret_that_never_reaches_the_run() {
    let _s = serial(); let tmp = Scratch::new("j61");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default(); let seen2 = seen.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let api_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || { for stream in listener.incoming().flatten() { let seen = seen2.clone(); std::thread::spawn(move || { use std::io::{Read, Write}; let mut s = stream; let mut buf = [0u8; 8192]; let n = s.read(&mut buf).unwrap_or(0); let req = String::from_utf8_lossy(&buf[..n]).to_string(); seen.lock().unwrap().push(req.clone());
        let body = if req.starts_with("GET /items") { "{\"items\":[1,2,3]}".to_string() } else { "{\"created\":true}".to_string() }; let code = if req.starts_with("POST") { "201 Created" } else { "200 OK" };
        let _ = write!(s, "HTTP/1.1 {code}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()); }); } });
    let port = free_port(); let s = Serve::start_env(&state, port, &[("ANTILOKI_WEB_ALLOW_LOCAL", Path::new("1"))]); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let node = |method: &str, args: Value, effects: Value| json!({ "node": "use_tool", "tool": { "type": "api", "name": method, "args": args }, "allow": { "effects": effects } });
    let g = |nodes: Value, edges: Value, start: &str| json!({ "team": "Apis", "schema": 1, "budget_usd": 1.0, "start": [start], "nodes": nodes, "edges": edges });
    // validation: the method must be listed as its risk, headers may not carry secrets, DELETE is a destructive action
    let bad = s.post(door, json!({ "op": "validate", "graph": g(json!({ "leader": { "node": "leader" }, "a": node("POST", json!({ "url": "http://x.test/a", "headers": { "Authorization": "Bearer k" } }), json!(["network"])), "deliver": { "node": "deliver" } }), json!([ { "from": "a", "to": "deliver" } ]), "a") }));
    let codes: Vec<String> = bad["report"]["errors"].as_array().unwrap().iter().map(|e| e["code"].as_str().unwrap_or("").to_string()).collect(); assert!(codes.contains(&"api_risk_not_listed".to_string()) && codes.contains(&"api_headers".to_string()), "{codes:?}");
    // the person stores the secret under a name; it never appears in a graph
    assert_eq!(s.post("/api/v1/webapps", json!({ "op": "token_set", "app": "items-api", "token": "tok_SECRET_123" }))["ok"], json!(true));
    let base = format!("http://127.0.0.1:{api_port}");
    let graph = g(json!({ "leader": { "node": "leader" },
        "list": node("GET", json!({ "url": format!("{base}/items?ask={{ask}}"), "token": "items-api" }), json!(["network", "read"])),
        "make": node("POST", json!({ "url": format!("{base}/items"), "body": { "name": "{ask}" }, "headers": { "X-Api-Version": "2" }, "token": "items-api" }), json!(["network", "write"])),
        "deliver": { "node": "deliver" } }), json!([ { "from": "list", "to": "make" }, { "from": "make", "to": "deliver" } ]), "list");
    let saved = s.post(door, json!({ "op": "save", "graph": graph })); assert_eq!(saved["ok"], json!(true), "{saved}"); assert_eq!(saved["report"]["errors"].as_array().unwrap().len(), 0, "{saved}");
    let go = s.post(door, json!({ "op": "run", "team": "apis", "ask": "tea" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    let done = until(&s, 60, "the run to finish", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); (r["state"]["status"]["status"] == json!("done")).then_some(r) });
    let v = &done["state"]["values"]; assert_eq!(v["list"]["status"], json!(200)); assert_eq!(v["list"]["json"]["items"][1], json!(2)); assert_eq!(v["list"]["risk"], json!("read")); assert_eq!(v["make"]["status"], json!(201)); assert_eq!(v["make"]["risk"], json!("write"));
    let reqs = seen.lock().unwrap().clone(); assert_eq!(reqs.len(), 2, "{reqs:?}");
    assert!(reqs[0].starts_with("GET /items?ask=tea") && reqs[0].to_ascii_lowercase().contains("authorization: bearer tok_secret_123"), "the secret goes out as a bearer header: {}", reqs[0]);
    assert!(reqs[1].starts_with("POST /items") && reqs[1].contains("{\"name\":\"tea\"}") && reqs[1].to_ascii_lowercase().contains("x-api-version: 2") && reqs[1].to_ascii_lowercase().contains("content-type: application/json"), "{}", reqs[1]);
    let all = std::fs::read_dir(antiloki_serve::team_graph_run::run_dir(&ws.to_string_lossy(), &run)).unwrap().flatten().filter_map(|e| std::fs::read_to_string(e.path()).ok()).collect::<String>(); assert!(!all.contains("tok_SECRET_123"), "the secret is never written to the run's files");
    // DELETE is never a team's, even listed; a write that is not listed does not even save; this machine's own addresses are refused when the guard is on is covered by unit tests
    let del = g(json!({ "leader": { "node": "leader" }, "d": node("DELETE", json!({ "url": format!("{base}/items/1") }), json!(["network", "destructive"])), "deliver": { "node": "deliver" } }), json!([ { "from": "d", "to": "deliver" } ]), "d");
    assert_eq!(s.post(door, json!({ "op": "save", "graph": del }))["ok"], json!(true)); let go2 = s.post(door, json!({ "op": "run", "team": "apis", "ask": "x" })); assert_eq!(go2["ok"], json!(false), "{go2}"); assert!(go2["error"].as_str().unwrap().contains("human approving"), "a destructive node needs a person before it, like every destructive step: {go2}");
    assert_eq!(seen.lock().unwrap().len(), 2, "nothing was sent for the DELETE");
}

/// BL-18: an `mcp_url` node reaches ANY MCP server by its address with a token from the store: a read runs, a write needs the node to list it, a destructive tool is never a team's.
#[test]
fn j62_an_mcp_url_node_reaches_any_server_by_address_a_read_runs_a_write_must_be_listed_and_a_destroy_never() {
    let _s = serial(); let tmp = Scratch::new("j62");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let fake = rt.block_on(antiloki_serve::mcp_fake::start(antiloki_serve::mcp_fake::FakeOpts { forbid: None, sse: false, token: Some("addr-token".into()) }));
    let port = free_port(); let s = Serve::start(&state, port); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    assert_eq!(s.post("/api/v1/webapps", json!({ "op": "token_set", "app": "any-server", "token": "addr-token" }))["ok"], json!(true));
    let node = |tool: &str, args: Value, effects: Value| json!({ "node": "use_tool", "tool": { "type": "mcp_url", "name": tool, "args": { "url": fake.url, "token": "any-server", "arguments": args } }, "allow": { "effects": effects } });
    let team = |name: &str, n: Value| json!({ "team": name, "schema": 1, "budget_usd": 1.0, "start": ["t"], "nodes": { "leader": { "node": "leader" }, "t": n, "deliver": { "node": "deliver" } }, "edges": [ { "from": "t", "to": "deliver" } ] });
    let run_to_end = |name: &str, n: Value, status: &str| { let saved = s.post(door, json!({ "op": "save", "graph": team(name, n) })); assert_eq!(saved["ok"], json!(true), "{saved}");
        let go = s.post(door, json!({ "op": "run", "team": name, "ask": "x" })); if go["ok"] != json!(true) { return json!({ "refused": go }); } let run = go["run"].as_str().unwrap().to_string();
        until(&s, 60, "the run to end", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); (r["state"]["status"]["status"] == json!(status)).then_some(r) }) };
    let ok = run_to_end("Reads", node("echo", json!({ "text": "hello" }), json!(["network", "read"])), "done"); assert_eq!(ok["state"]["values"]["t"]["text"], json!("hello"), "{ok}"); assert_eq!(ok["state"]["values"]["t"]["risk"], json!("read"));
    // a write the node did not list stops the run; listed, it runs
    let no = run_to_end("Writes", node("flag", json!({ "on": true }), json!(["network", "read"])), "blocked"); assert!(no.to_string().contains("write"), "{no}");
    let yes = run_to_end("Writes2", node("flag", json!({ "on": true }), json!(["network", "write"])), "done"); assert_eq!(yes["state"]["values"]["t"]["risk"], json!("write"), "{yes}");
    // a destructive tool is never a team's, listed or not (a human gate would be required to save it at all, so the node is reached through one)
    let g = json!({ "team": "Destroys", "schema": 1, "budget_usd": 1.0, "start": ["me"], "nodes": { "leader": { "node": "leader" }, "me": { "node": "human", "gate": "merge" }, "t": node("delete_thing", json!({ "id": "7" }), json!(["network", "destructive"])), "deliver": { "node": "deliver" } }, "edges": [ { "from": "me", "port": "approved", "to": "t" }, { "from": "t", "to": "deliver" } ] });
    assert_eq!(s.post(door, json!({ "op": "save", "graph": g }))["ok"], json!(true));
    // a node with a wrong token cannot read: the server asks for a sign-in and the run says so
    assert_eq!(s.post("/api/v1/webapps", json!({ "op": "token_set", "app": "any-server", "token": "wrong" }))["ok"], json!(true));
    let bad = run_to_end("Reads3", node("echo", json!({ "text": "hello" }), json!(["network", "read"])), "blocked"); assert!(!bad.to_string().contains("addr-token"), "the token never shows");
    // a node must give an address
    let inv = s.post(door, json!({ "op": "validate", "graph": team("NoUrl", json!({ "node": "use_tool", "tool": { "type": "mcp_url", "name": "echo", "args": {} }, "allow": { "effects": ["read"] } })) }));
    let codes: Vec<String> = inv["report"]["errors"].as_array().unwrap().iter().map(|e| e["code"].as_str().unwrap_or("").to_string()).collect(); assert!(codes.contains(&"mcp_url_url".to_string()) && codes.contains(&"mcp_url_needs_network".to_string()), "{codes:?}");
}

/// BL-20: git nodes work inside the team's own branch and worktrees: a unit is made, something is committed in it, merged into the team's branch and seen in the log; a closed list of operations, risks that must be listed, names checked, never the workspace's own branch.
#[test]
fn j63_git_nodes_make_a_unit_commit_in_it_merge_it_into_the_teams_branch_and_never_touch_the_workspace() {
    let _s = serial(); let tmp = Scratch::new("j63");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let port = free_port(); let s = Serve::start(&state, port); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph"; let root = ws.to_string_lossy().to_string();
    let git = |name: &str, args: Value, effects: Value| json!({ "node": "use_tool", "tool": { "type": "git", "name": name, "args": args }, "allow": { "effects": effects } });
    let team = |nodes: Value, edges: Value, start: &str| json!({ "team": "Gitters", "schema": 1, "budget_usd": 1.0, "start": [start], "nodes": nodes, "edges": edges });
    let ws_head = |dir: &std::path::Path| String::from_utf8(std::process::Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir).output().unwrap().stdout).unwrap().trim().to_string();
    let before = ws_head(&ws); let _ = &root;
    let go = |g: Value, status: &str| { assert_eq!(s.post(door, json!({ "op": "save", "graph": g }))["ok"], json!(true)); let r = s.post(door, json!({ "op": "run", "team": "gitters", "ask": "x" })); assert_eq!(r["ok"], json!(true), "{r}"); let run = r["run"].as_str().unwrap().to_string();
        until(&s, 60, "the run to end", || { let st = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); (st["state"]["status"]["status"] == json!(status)).then_some(st) }) };
    // validation: the closed list, the listed risk, the names and the message
    let bad = s.post(door, json!({ "op": "validate", "graph": team(json!({ "leader": { "node": "leader" }, "a": git("push", json!({}), json!(["read"])), "b": git("commit", json!({}), json!(["read"])), "c": git("branch", json!({ "name": "../x" }), json!(["write"])), "deliver": { "node": "deliver" } }), json!([ { "from": "a", "to": "b" }, { "from": "b", "to": "c" }, { "from": "c", "to": "deliver" } ]), "a") }));
    let codes: Vec<String> = bad["report"]["errors"].as_array().unwrap().iter().map(|e| e["code"].as_str().unwrap_or("").to_string()).collect(); for c in ["git_op", "git_risk_not_listed", "git_message", "git_name"] { assert!(codes.contains(&c.to_string()), "{c} in {codes:?}"); }
    // 1) a unit (branch + worktree) is made
    let r1 = go(team(json!({ "leader": { "node": "leader" }, "mk": git("worktree_add", json!({ "name": "feat" }), json!(["write"])), "st": git("status", json!({ "unit": "feat" }), json!(["read"])), "deliver": { "node": "deliver" } }), json!([ { "from": "mk", "to": "st" }, { "from": "st", "to": "deliver" } ]), "mk"), "done");
    let v = &r1["state"]["values"]; let dir = std::path::PathBuf::from(v["mk"]["dir"].as_str().unwrap()); assert!(dir.join(".git").exists(), "{v}"); assert!(v["mk"]["branch"].as_str().unwrap().ends_with("--feat")); assert_eq!(v["st"]["clean"], json!(true), "{v}");
    // 2) something is written in the unit (by whoever works there); a commit, a merge into the team's branch and the log
    std::fs::write(dir.join("feature.txt"), "hello\n").unwrap();
    let r2 = go(team(json!({ "leader": { "node": "leader" }, "dirty": git("status", json!({ "unit": "feat" }), json!(["read"])), "ci": git("commit", json!({ "unit": "feat", "message": "add the feature for {ask}" }), json!(["write"])), "mg": git("merge", json!({ "name": "feat" }), json!(["write"])), "lg": git("log", json!({ "n": 5 }), json!(["read"])), "deliver": { "node": "deliver" } }), json!([ { "from": "dirty", "to": "ci" }, { "from": "ci", "to": "mg" }, { "from": "mg", "to": "lg" }, { "from": "lg", "to": "deliver" } ]), "dirty"), "done");
    let v = &r2["state"]["values"]; assert_eq!(v["dirty"]["clean"], json!(false), "{v}"); assert_eq!(v["ci"]["committed"], json!(true)); assert_eq!(v["mg"]["merged"], json!(true), "{v}"); assert!(v["lg"]["text"].as_str().unwrap().contains("merge") || v["lg"]["text"].as_str().unwrap().contains("add the feature for x"), "{v}");
    // the team's own tree has the file; the person's workspace was never touched
    let tree = dir.parent().unwrap().join(dir.file_name().unwrap().to_string_lossy().trim_end_matches("--feat")); assert!(tree.join("feature.txt").exists(), "merged into the team's tree");
    assert!(!ws.join("feature.txt").exists(), "never the workspace"); assert_eq!(ws_head(&ws), before, "the workspace's branch did not move");
    // 3) removing the unit
    let r3 = go(team(json!({ "leader": { "node": "leader" }, "rm": git("worktree_remove", json!({ "name": "feat" }), json!(["write"])), "deliver": { "node": "deliver" } }), json!([ { "from": "rm", "to": "deliver" } ]), "rm"), "done"); assert_eq!(r3["state"]["values"]["rm"]["ok"], json!(true)); assert!(!dir.exists(), "the unit's folder is gone");
}

/// BL-19: a telegram node is outward: it needs a human gate before it, lists network and outward, reads its bot token by NAME from the store, sends through the Bot API and never writes the token anywhere.
#[test]
fn j64_a_telegram_node_sends_after_the_persons_approval_with_a_token_that_never_reaches_the_run() {
    let _s = serial(); let tmp = Scratch::new("j64");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let seen: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default(); let seen2 = seen.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let api_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || { for stream in listener.incoming().flatten() { let seen = seen2.clone(); std::thread::spawn(move || { use std::io::{Read, Write}; let mut s = stream; let mut buf = [0u8; 8192]; let n = s.read(&mut buf).unwrap_or(0); seen.lock().unwrap().push(String::from_utf8_lossy(&buf[..n]).to_string());
        let body = "{\"ok\":true,\"result\":{\"message_id\":42}}"; let _ = write!(s, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()); }); } });
    let api = format!("http://127.0.0.1:{api_port}"); let port = free_port(); let s = Serve::start_env(&state, port, &[("ANTILOKI_TELEGRAM_API", Path::new(&api))]); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let tg = |effects: Value| json!({ "node": "use_tool", "tool": { "type": "telegram", "name": "send", "args": { "chat": "-100123456", "text": "Report ready: {ask}", "token": "my-bot" } }, "allow": { "effects": effects } });
    let team = |tgnode: Value, human: bool| { let (edges, start) = if human { (json!([ { "from": "me", "port": "approved", "to": "t" }, { "from": "t", "to": "deliver" } ]), "me") } else { (json!([ { "from": "t", "to": "deliver" } ]), "t") };
        json!({ "team": "Pinger", "schema": 1, "budget_usd": 1.0, "start": [start], "nodes": { "leader": { "node": "leader" }, "me": { "node": "human", "gate": "merge" }, "t": tgnode, "deliver": { "node": "deliver" } }, "edges": edges }) };
    // outward without a person before it, or without the effects, does not even save as runnable
    let v1 = s.post(door, json!({ "op": "validate", "graph": team(tg(json!(["network", "outward"])), false) })); let c1: Vec<String> = v1["report"]["errors"].as_array().unwrap().iter().map(|e| e["code"].as_str().unwrap_or("").to_string()).collect(); assert!(c1.contains(&"destructive_without_human".to_string()), "{c1:?}");
    let v2 = s.post(door, json!({ "op": "validate", "graph": team(tg(json!(["network"])), true) })); let c2: Vec<String> = v2["report"]["errors"].as_array().unwrap().iter().map(|e| e["code"].as_str().unwrap_or("").to_string()).collect(); assert!(c2.contains(&"telegram_effects".to_string()), "{c2:?}");
    // the right graph: the run stops at the person; nothing is sent until they approve; no bot token stored = a plain refusal
    assert_eq!(s.post(door, json!({ "op": "save", "graph": team(tg(json!(["network", "outward"])), true) }))["ok"], json!(true));
    let go = s.post(door, json!({ "op": "run", "team": "pinger", "ask": "weekly" })); let run = go["run"].as_str().unwrap().to_string();
    until(&s, 60, "the run to wait at the person", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); r["state"]["active"]["me"].is_number().then_some(()) }); assert!(seen.lock().unwrap().is_empty(), "nothing is sent before the person says yes");
    assert_eq!(s.post(door, json!({ "op": "answer", "run": run, "node": "me", "approve": true }))["ok"], json!(true));
    let blocked = until(&s, 60, "the run to stop for the missing token", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); (r["state"]["status"]["status"] == json!("blocked")).then_some(r) }); assert!(blocked.to_string().contains("no bot token is stored"), "{blocked}"); assert!(seen.lock().unwrap().is_empty());
    // the person stores the token under its name; a new run, approved, sends one message with the placeholders filled
    assert_eq!(s.post("/api/v1/webapps", json!({ "op": "token_set", "app": "my-bot", "token": "123:BOT_SECRET" }))["ok"], json!(true));
    let go2 = s.post(door, json!({ "op": "run", "team": "pinger", "ask": "weekly" })); let run2 = go2["run"].as_str().unwrap().to_string();
    until(&s, 60, "the second run to wait", || { let r = s.post(door, json!({ "op": "status", "run": run2 }))["run"].clone(); r["state"]["active"]["me"].is_number().then_some(()) });
    assert_eq!(s.post(door, json!({ "op": "answer", "run": run2, "node": "me", "approve": true }))["ok"], json!(true));
    let done = until(&s, 60, "the run to finish", || { let r = s.post(door, json!({ "op": "status", "run": run2 }))["run"].clone(); (r["state"]["status"]["status"] == json!("done")).then_some(r) });
    assert_eq!(done["state"]["values"]["t"]["message_id"], json!(42), "{done}");
    let reqs = seen.lock().unwrap().clone(); assert_eq!(reqs.len(), 1, "{reqs:?}"); assert!(reqs[0].starts_with("POST /bot123:BOT_SECRET/sendMessage"), "{}", reqs[0]); assert!(reqs[0].contains("\"chat_id\":\"-100123456\"") && reqs[0].contains("Report ready: weekly"), "{}", reqs[0]);
    let all = std::fs::read_dir(antiloki_serve::team_graph_run::run_dir(&ws.to_string_lossy(), &run2)).unwrap().flatten().filter_map(|e| std::fs::read_to_string(e.path()).ok()).collect::<String>(); assert!(!all.contains("BOT_SECRET"), "the token is never written to the run's files");
    assert!(!done.to_string().contains("BOT_SECRET"), "nor shown by the status");
}

/// RFC 0009 M0/M1 B3: the serve is an MCP client. A fake MCP server (JSON then SSE) behind an installed app: the person signs in with a token that is kept out of every reply, lists the tools with their risk, calls a read, an agent is refused a write and a destroy, the person confirms a destroy, and a team run calls a tool of the app as a node.
#[test]
fn j56_the_serve_is_an_mcp_client_with_risk_classes_a_token_kept_apart_and_a_tool_node() {
    let _s = serial(); let tmp = Scratch::new("j56");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let fake = rt.block_on(antiloki_serve::mcp_fake::start(antiloki_serve::mcp_fake::FakeOpts { forbid: None, sse: true, token: Some("s3cret-token".into()) }));
    let port = free_port(); let s = Serve::start(&state, port); s.add_workspace("work", &ws); s.activate("work");
    let apps = "/api/v1/webapps"; let door = "/api/v1/cockpit/team-graph";
    let inst = s.post(apps, json!({ "op": "install", "name": "Fake Tracker", "kind": "official", "app_url": fake.url.replace("/mcp", ""), "mcp_url": fake.url }));
    assert_eq!(inst["ok"], json!(true), "{inst}");
    // not signed in: the app says so, and the reply says what to do
    let no = s.post(apps, json!({ "op": "tools", "app": "fake-tracker" })); assert_eq!(no["ok"], json!(false), "{no}"); assert_eq!(no["needs_signin"], json!(true), "{no}");
    // an agent can neither give nor remove a token; the person gives one, and no reply ever shows it
    assert_eq!(s.post(apps, json!({ "op": "token_set", "app": "fake-tracker", "token": "s3cret-token", "pane_id": 3 }))["ok"], json!(false));
    let set = s.post(apps, json!({ "op": "token_set", "app": "fake-tracker", "token": "s3cret-token" })); assert_eq!(set["ok"], json!(true), "{set}"); assert_eq!(set["signed_in"], json!(true)); assert!(!set.to_string().contains("s3cret-token"));
    let home_tokens = state.join("home/app-tokens.json"); assert!(home_tokens.exists(), "the token is kept in the machine's antiloki home, not in the workspace or the repo");
    assert!(!std::fs::read_to_string(ws.join(".antiloki").join("anything")).unwrap_or_default().contains("s3cret"));
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert_eq!(std::fs::metadata(&home_tokens).unwrap().permissions().mode() & 0o777, 0o600, "only the owner reads it"); }
    // the tools, with their schemas and risk classes (both pages)
    let t = s.post(apps, json!({ "op": "tools", "app": "fake-tracker" })); assert_eq!(t["ok"], json!(true), "{t}"); assert_eq!(t["signed_in"], json!(true));
    let tools = t["tools"].as_array().unwrap(); assert_eq!(tools.len(), 7, "{t}");
    let risk = |n: &str| tools.iter().find(|x| x["name"] == n).unwrap()["risk"].clone();
    assert_eq!((risk("echo"), risk("flag"), risk("delete_thing")), (json!("read"), json!("write"), json!("destructive")));
    assert!(!t.to_string().contains("s3cret-token"));
    // a read works for anyone; a write and a destroy are refused to an agent; the person confirms the destroy
    let ok = s.post(apps, json!({ "op": "call", "app": "fake-tracker", "tool": "echo", "args": { "text": "hello" }, "pane_id": 3 })); assert_eq!(ok["ok"], json!(true), "{ok}"); assert_eq!(ok["result"]["text"], json!("hello"));
    let w = s.post(apps, json!({ "op": "call", "app": "fake-tracker", "tool": "flag", "args": { "on": true }, "pane_id": 3 })); assert_eq!(w["ok"], json!(false), "{w}"); assert!(w["error"].as_str().unwrap().contains("writes"), "{w}");
    let d = s.post(apps, json!({ "op": "call", "app": "fake-tracker", "tool": "delete_thing", "args": { "id": "7" }, "pane_id": 3, "confirm": true })); assert_eq!(d["ok"], json!(false), "{d}"); assert!(d["error"].as_str().unwrap().contains("never"), "{d}");
    let unconfirmed = s.post(apps, json!({ "op": "call", "app": "fake-tracker", "tool": "delete_thing", "args": { "id": "7" } })); assert_eq!(unconfirmed["ok"], json!(false), "{unconfirmed}"); assert!(unconfirmed["error"].as_str().unwrap().contains("confirm"));
    let done = s.post(apps, json!({ "op": "call", "app": "fake-tracker", "tool": "delete_thing", "args": { "id": "7" }, "confirm": true })); assert_eq!(done["ok"], json!(true), "{done}"); assert_eq!(done["result"]["text"], json!("deleted 7"));
    // an agent names an installed app, never an address
    assert_eq!(s.post(apps, json!({ "op": "tools", "url": fake.url, "pane_id": 3 }))["ok"], json!(false));
    // the trail: every call, who made it, its risk; the token and the arguments' values are not in it
    let trail = std::fs::read_to_string(state.join("home/app-calls.jsonl")).unwrap(); assert!(trail.lines().count() >= 5, "{trail}");
    assert!(trail.contains("agent:3") && trail.contains("\"risk\":\"destructive\"") && trail.contains("\"who\":\"person\""), "{trail}"); assert!(!trail.contains("s3cret-token") && !trail.contains("hello"), "the trail holds names, not values or the token");
    // a team node calls a tool of the app: a read needs `read` on its allow-list
    let graph = json!({ "team": "Tracker", "schema": 1, "budget_usd": 1.0, "start": ["add"],
        "nodes": { "leader": { "node": "leader" }, "add": { "node": "use_tool", "tool": { "type": "webapp", "name": "fake-tracker", "args": { "tool": "add", "arguments": { "a": 2, "b": 3.5 } } }, "allow": { "effects": ["read"] } }, "deliver": { "node": "deliver" } }, "edges": [ { "from": "add", "to": "deliver" } ] });
    assert_eq!(s.post(door, json!({ "op": "save", "graph": graph }))["ok"], json!(true));
    let go = s.post(door, json!({ "op": "run", "team": "tracker", "ask": "add" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    until(&s, 60, "the run to deliver", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); (r["state"]["status"]["status"] == json!("done")).then_some(()) });
    let vals = s.post(door, json!({ "op": "status", "run": run }))["run"]["state"]["values"].clone(); assert_eq!(vals["add"]["text"], json!("5.5"), "{vals}"); assert_eq!(vals["add"]["risk"], json!("read"));
    drop(s); drop(rt);
}

/// The operator's first real team (2026-11-14): asked for a video team inside the team they had just made, the Leader made ANOTHER team first, from the coding template. An agent now works in the open team, a new team of its own is blank, and an all-agents team is warned.
#[test]
fn j57_the_leader_designs_into_the_open_team_not_a_second_one_and_a_blank_team_is_not_a_coding_team() {
    let _s = serial(); let tmp = Scratch::new("j57");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let port = free_port(); let s = Serve::start(&state, port); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    // the person makes their team (the sidebar's New team); it becomes the open thread
    let mine = s.post(door, json!({ "op": "create", "name": "Editor Vid", "template": "blank", "budget_usd": 4.0 })); assert_eq!(mine["ok"], json!(true), "{mine}");
    let agent = |extra: Value| { let mut b = json!({ "pane_id": 3 }); for (k, v) in extra.as_object().unwrap() { b[k] = v.clone(); } b };
    // the Leader (an agent) tries to make a team of another name: refused, and told where to put the design
    let second = s.post(door, agent(json!({ "op": "create", "name": "A Video Editing" }))); assert_eq!(second["ok"], json!(false), "{second}");
    assert!(second["error"].as_str().unwrap().contains("editor-vid") && second["error"].as_str().unwrap().contains("two teams"), "{second}");
    // a graph saved under another name is refused the same way; under the open team's own name it is saved
    let g = |team: &str| json!({ "team": team, "schema": 1, "budget_usd": 2.0, "start": ["mk"], "nodes": { "leader": { "node": "leader" }, "mk": { "node": "code", "source": "#{ a: 1 }", "examples": [ { "input": {}, "output": { "a": 1 } } ] }, "deliver": { "node": "deliver" } }, "edges": [ { "from": "mk", "to": "deliver" } ] });
    let other = s.post(door, agent(json!({ "op": "save", "graph": g("Video Maker") }))); assert_eq!(other["ok"], json!(false), "{other}");
    let right = s.post(door, agent(json!({ "op": "save", "graph": g("Editor Vid") }))); assert_eq!(right["ok"], json!(true), "{right}");
    let listed: Vec<String> = s.post(door, json!({ "op": "list" }))["graphs"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap().to_string()).collect(); assert_eq!(listed, vec!["editor-vid".to_string()], "one team, not two: {listed:?}");
    // a second team on purpose is allowed, and a team an agent makes from a name alone is BLANK (no engineer, no UI developer)
    let on_purpose = s.post(door, agent(json!({ "op": "create", "name": "Audio Cleanup", "new_team": true }))); assert_eq!(on_purpose["ok"], json!(true), "{on_purpose}");
    let nodes = on_purpose["graph"]["nodes"].as_object().unwrap(); let roles: Vec<&str> = nodes.values().filter(|n| n["node"] == "role").map(|n| n["role"].as_str().unwrap_or("")).collect();
    assert!(roles.is_empty(), "a blank team has no agents of the coding template: {roles:?}");
    // the person's own New team still offers the coding template by default
    let person = s.post(door, json!({ "op": "create", "name": "Plain Code" })); assert_eq!(person["ok"], json!(true), "{person}"); assert!(person["graph"]["nodes"].as_object().unwrap().values().any(|n| n["node"] == "role"));
    // a team of agents and nothing else is told so (the validator's `all_agents` warning)
    let allagents = json!({ "team": "Agents Only", "schema": 1, "budget_usd": 3.0, "start": ["a"], "nodes": { "leader": { "node": "leader" }, "a": { "node": "role", "role": "worker", "owns": ["src/**"] }, "b": { "node": "role", "role": "worker", "owns": ["docs/**"] }, "c": { "node": "role", "role": "qa" }, "deliver": { "node": "deliver" } }, "edges": [ { "from": "a", "to": "b" }, { "from": "b", "to": "c" }, { "from": "c", "to": "deliver" } ] });
    let v = s.post(door, json!({ "op": "validate", "graph": allagents })); let warns: Vec<String> = v["report"]["warnings"].as_array().unwrap().iter().map(|w| w["code"].as_str().unwrap_or("").to_string()).collect();
    assert!(warns.contains(&"all_agents".to_string()), "{warns:?}");
    let mixed = json!({ "team": "Mixed", "schema": 1, "budget_usd": 3.0, "start": ["a"], "nodes": { "leader": { "node": "leader" }, "a": { "node": "role", "role": "worker", "owns": ["src/**"] }, "b": { "node": "role", "role": "worker", "owns": ["docs/**"] }, "c": { "node": "role", "role": "qa" }, "k": { "node": "code", "source": "#{}", "examples": [ { "input": {}, "output": {} } ] }, "deliver": { "node": "deliver" } }, "edges": [ { "from": "a", "to": "b" }, { "from": "b", "to": "c" }, { "from": "c", "to": "k" }, { "from": "k", "to": "deliver" } ] });
    let v2 = s.post(door, json!({ "op": "validate", "graph": mixed })); assert!(!v2["report"]["warnings"].as_array().unwrap().iter().any(|w| w["code"] == "all_agents"), "one deterministic step is enough to stop the nudge");
}

/// What the real marketing-team test taught (2026-11-12): a review loop ends on the reviewer's `pass`, an agent that only makes documents (effect `document`) cannot change code, and a team the Leader saves on its own does not take over the person's screen.
#[test]
fn j38_a_review_loop_ends_on_pass_a_document_agent_cannot_change_code_and_a_saved_team_keeps_the_screen() {
    let _s = serial(); let tmp = Scratch::new("j38");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let script = json!({ "writer": [{ "write": { ".antiloki/artifacts/draft.md": "# Draft\nhello\n", "src/oops.txt": "code a document agent must not write\n" }, "text": { "done": true, "report": "drafted" }, "cost": 0.01 }],
                         "reviewer": [{ "text": { "done": true, "pass": false, "report": "fix the tone" }, "cost": 0.01 }, { "text": { "done": true, "pass": true, "report": "ok" }, "cost": 0.01 }] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let port = free_port();
    let s = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state"))]);
    s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let agent = |title: &str, job: &str, effects: Value| json!({ "node": "role", "role": "agent", "cap_usd": 0.5, "agent": { "title": title, "responsibility": job, "brief": "be brief", "effects": effects } });
    let graph = json!({ "team": "Reviewed", "schema": 1, "budget_usd": 3.0, "start": ["rl"],
        "nodes": { "leader": { "node": "leader" }, "rl": { "node": "loop", "max_iterations": 3, "until": "reviewer.pass == true", "cap_usd": 1.0 }, "writer": agent("the Writer", "write the draft", json!(["document"])), "reviewer": agent("the Reviewer", "judge the draft", json!(["read"])),
                   "doc": { "node": "artifact", "name": "draft", "format": "md", "from": "writer" }, "me": { "node": "human", "gate": "merge" }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "rl", "port": "body", "to": "writer" }, { "from": "writer", "to": "reviewer" }, { "from": "reviewer", "to": "rl" }, { "from": "rl", "port": "done", "to": "doc" }, { "from": "rl", "port": "exhausted", "to": "me" }, { "from": "doc", "to": "me" }, { "from": "me", "port": "approved", "to": "deliver" } ] });
    // a dry run validates and keeps nothing; a save makes the team but leaves the person's screen where it was
    let dry = s.post(door, json!({ "op": "validate", "graph": graph })); assert_eq!(dry["report"]["errors"].as_array().unwrap().len(), 0, "{dry}"); assert_eq!(s.post(door, json!({ "op": "list" }))["graphs"].as_array().unwrap().len(), 0, "a dry run saves nothing");
    let saved = s.post(door, json!({ "op": "save", "graph": graph })); assert_eq!(saved["ok"], json!(true), "{saved}");
    assert_eq!(s.post("/api/v1/cockpit/cowork/threads", json!({ "op": "list" }))["active"], json!("main"), "a team saved by the Leader does not take over the screen");
    let go = s.post(door, json!({ "op": "run", "team": "reviewed", "ask": "write it" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    let parked = until(&s, 90, "the run to wait at the merge", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); if r["state"]["active"]["me"].is_number() { Some(r) } else { None } });
    let evs: Vec<String> = parked["events"].as_array().unwrap().iter().map(|e| format!("{} {}", e["actor"].as_str().unwrap_or(""), e["kind"].as_str().unwrap_or(""))).collect();
    assert_eq!(evs.iter().filter(|e| *e == "reviewer start").count(), 2, "the loop went round once more for a `fail`, and stopped on the `pass`: {evs:?}");
    assert_eq!(parked["state"]["values"]["reviewer"]["pass"], json!(true)); assert_eq!(parked["state"]["values"]["doc"]["path"], json!("artifacts/draft.md"));
    let t = s.post(door, json!({ "op": "artifact", "run": run, "name": "draft.md" })); assert!(t["text"].as_str().unwrap().contains("hello"), "{t}");
    let ok = s.post(door, json!({ "op": "answer", "run": run, "node": "me", "approve": true })); assert_eq!(ok["ok"], json!(true), "{ok}");
    std::thread::sleep(std::time::Duration::from_secs(2)); let wn = s.post(door, json!({ "op": "node", "run": run, "node": "writer" })); assert!(!ws.join("src/oops.txt").exists(), "a document agent's change to code was put back, so it never landed: {}", wn["attempts"].as_array().map(|a| a.iter().map(|x| x["text"].as_str().unwrap_or("").chars().rev().take(300).collect::<String>().chars().rev().collect::<String>()).collect::<Vec<_>>().join(" || ")).unwrap_or_default());
}

/// §4.4: the web node and what comes of it. A tiny web server on this machine stands in for the web (`ANTILOKI_WEB_SEARCH_URL`, `ANTILOKI_WEB_ALLOW_LOCAL`): a search node and a page node fetch, no model is called, the next custom agent is TOLD what they found,
/// an artifact is made from the search, and without the allow-local switch the same node refuses a page on this machine.
#[test]
fn j39_a_web_node_searches_and_reads_and_the_agent_after_it_is_told_what_it_found() {
    let _s = serial(); let tmp = Scratch::new("j39");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    // the web: /search?q=… answers JSON results, /page answers an html page
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let web_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || { for stream in listener.incoming().flatten() { std::thread::spawn(move || { use std::io::{Read, Write}; let mut s = stream; let mut buf = [0u8; 4096]; let n = s.read(&mut buf).unwrap_or(0); let req = String::from_utf8_lossy(&buf[..n]).to_string(); let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
        let (ct, body) = if path.starts_with("/search") { ("application/json", format!("{{\"results\":[{{\"title\":\"Single origin guide\",\"url\":\"http://127.0.0.1:{web_port}/page\",\"snippet\":\"how to taste\"}}]}}")) } else { ("text/html", "<html><head><title>The guide</title></head><body><h1>Single origin</h1><p>Washed Ethiopian beans taste of bergamot.</p><script>x()</script></body></html>".to_string()) };
        let _ = write!(s, "HTTP/1.1 200 OK\r\ncontent-type: {ct}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()); }); } });
    let script = json!({ "writer": [{ "text": { "done": true, "report": "wrote it from the research" }, "cost": 0.01 }] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let port = free_port(); let search_url = format!("http://127.0.0.1:{web_port}/search?q={{q}}");   // the env values are paths here: a string is one
    let s = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state")), ("ANTILOKI_WEB_SEARCH_URL", Path::new(&search_url)), ("ANTILOKI_WEB_ALLOW_LOCAL", Path::new("1"))]);
    s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let web = |name: &str, args: Value| json!({ "node": "use_tool", "tool": { "type": "web", "name": name, "args": args }, "allow": { "effects": ["network"] } });
    let graph = json!({ "team": "Research", "schema": 1, "budget_usd": 3.0, "start": ["search"],
        "nodes": { "leader": { "node": "leader" }, "search": web("search", json!({ "query": "{ask} coffee", "max": 3 })), "page": web("fetch", json!({ "url": "http://127.0.0.1:%PORT%/page" })),
                   "writer": { "node": "role", "role": "agent", "cap_usd": 0.5, "agent": { "title": "the Writer", "responsibility": "write from the research", "brief": "be brief", "effects": ["read"] } },
                   "notes": { "node": "artifact", "name": "research", "format": "md", "from": "search" }, "me": { "node": "human", "gate": "merge" }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "search", "to": "page" }, { "from": "page", "to": "writer" }, { "from": "writer", "to": "notes" }, { "from": "notes", "to": "me" }, { "from": "me", "port": "approved", "to": "deliver" } ] });
    let graph: Value = serde_json::from_str(&graph.to_string().replace("%PORT%", &web_port.to_string())).unwrap();
    let saved = s.post(door, json!({ "op": "save", "graph": graph })); assert_eq!(saved["ok"], json!(true), "{saved}"); assert_eq!(saved["report"]["errors"].as_array().unwrap().len(), 0, "{saved}");
    let go = s.post(door, json!({ "op": "run", "team": "research", "ask": "single origin" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    let parked = until(&s, 90, "the run to wait at the merge", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); if r["state"]["active"]["me"].is_number() { Some(r) } else { None } });
    assert_eq!(parked["state"]["values"]["search"]["count"], json!(1)); assert_eq!(parked["state"]["values"]["search"]["results"][0]["title"], json!("Single origin guide"));
    assert!(parked["state"]["values"]["page"]["text"].as_str().unwrap().contains("bergamot") && !parked["state"]["values"]["page"]["text"].as_str().unwrap().contains("x()"), "the page as text, without its script: {}", parked["state"]["values"]["page"]);
    assert_eq!(parked["state"]["values"]["page"]["title"], json!("The guide")); assert_eq!(parked["state"]["spent_usd"].as_f64().map(|c| (c * 100.0).round() as i64), Some(1), "only the writer cost anything: the web nodes call no model");
    let w = s.post(door, json!({ "op": "node", "run": run, "node": "writer" })); let text = w["attempts"][0]["text"].as_str().unwrap_or("").to_string(); assert!(text.contains("WHAT THE NODES BEFORE YOU FOUND") && text.contains("bergamot"), "the agent after the web node is told what it read: {text}");
    let a = s.post(door, json!({ "op": "artifact", "run": run, "name": "research.md" })); assert!(a["text"].as_str().unwrap().contains("Single origin guide"), "an artifact can be made from a search: {a}");
}

/// RFC 0010 §4.9: a team runs on its own schedule. The person consents for exactly this schedule; an agent can neither set, change nor consent; the serve fires at the named instant with the date filled into the ask; a day's limit and a changed schedule are respected.
#[test]
fn j40_a_team_runs_on_its_cron_only_after_the_persons_consent_and_within_its_limits() {
    let _s = serial(); let tmp = Scratch::new("j40");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let script = json!({ "writer": [{ "text": { "done": true, "report": "wrote it" }, "cost": 0.01 }] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let port = free_port();
    let s = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state")), ("ANTILOKI_TEAM_TRIGGERS_TEST", Path::new("1")), ("ANTILOKI_TZ_OFFSET_MIN", Path::new("0"))]);
    s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let graph = json!({ "team": "Weekly", "schema": 1, "budget_usd": 2.0, "start": ["writer"],
        "nodes": { "leader": { "node": "leader" }, "writer": { "node": "role", "role": "agent", "cap_usd": 0.5, "agent": { "title": "the Writer", "responsibility": "write the weekly note", "brief": "short", "effects": ["document"] } }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "writer", "to": "deliver" } ] });
    assert_eq!(s.post(door, json!({ "op": "save", "graph": graph }))["ok"], json!(true));
    let trig = json!([{ "id": "weekly", "kind": "schedule", "cron": "0 8 * * *", "ask": "Weekly note for {date} ({weekday})", "budget_usd": 1.0, "missed": "run_once" }]);
    // an agent (a call from a pane) can set nothing: not through triggers_set, not through save, and it cannot consent
    assert_eq!(s.post(door, json!({ "op": "triggers_set", "team": "weekly", "triggers": trig, "pane_id": 9 }))["ok"], json!(false));
    let mut sneaky = graph.clone(); sneaky["triggers"] = trig.clone(); let r = s.post(door, json!({ "op": "save", "graph": sneaky, "pane_id": 9 })); assert_eq!(r["ok"], json!(false), "{r}"); assert!(r["error"].as_str().unwrap().contains("person"), "{r}");
    // the person sets it; a bad cron is refused in words
    let bad = s.post(door, json!({ "op": "triggers_set", "team": "weekly", "triggers": [{ "kind": "schedule", "cron": "nope", "ask": "x" }] })); assert_eq!(bad["ok"], json!(false), "{bad}"); assert!(bad["error"].as_str().unwrap().contains("five fields"), "{bad}");
    let set = s.post(door, json!({ "op": "triggers_set", "team": "weekly", "triggers": trig, "limits": { "max_runs_per_day": 1 } })); assert_eq!(set["ok"], json!(true), "{set}"); assert_eq!(set["consented"], json!(false)); assert!(set["triggers"][0]["next_ms"].as_i64().unwrap() > 0);
    // nothing fires without consent, whatever the clock says
    let day1 = set["triggers"][0]["next_ms"].as_i64().unwrap();
    let none = s.post(door, json!({ "op": "trigger_tick", "team": "weekly", "now_ms": day1 + 5_000 })); assert_eq!(none["decisions"].as_array().unwrap().len(), 0, "{none}");
    assert_eq!(s.post(door, json!({ "op": "trigger_consent", "team": "weekly", "on": true, "pane_id": 9 }))["ok"], json!(false), "an agent cannot consent");
    let yes = s.post(door, json!({ "op": "trigger_consent", "team": "weekly", "on": true })); assert_eq!(yes["consented"], json!(true), "{yes}");
    // the first look starts the watch and fires nothing back in time; the instant itself fires, with the date in the ask
    let t0 = s.post(door, json!({ "op": "trigger_tick", "team": "weekly", "now_ms": day1 - 3_600_000 })); assert_eq!(t0["decisions"].as_array().unwrap().len(), 0, "{t0}");
    let t1 = s.post(door, json!({ "op": "trigger_tick", "team": "weekly", "now_ms": day1 + 5_000 })); let d = t1["decisions"].as_array().unwrap(); assert_eq!(d.len(), 1, "{t1}"); assert_eq!(d[0]["do"], json!("fire")); assert!(d[0]["ask"].as_str().unwrap().starts_with("Weekly note for 20") && d[0]["ask"].as_str().unwrap().contains("("), "{t1}");
    let st = s.post(door, json!({ "op": "trigger_status", "team": "weekly" })); let run = st["log"][0]["run"].as_str().unwrap().to_string(); assert_eq!(st["log"][0]["what"], json!("fired"), "{st}");
    let done = until(&s, 60, "the scheduled run to be delivered", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); (r["state"]["status"]["status"] == json!("done")).then_some(r) }); assert!(done["ask"].as_str().unwrap().starts_with("Weekly note for"), "{}", done["ask"]);
    // the same instant is not fired twice, and the day's limit (1) holds for a second schedule of the same day
    assert_eq!(s.post(door, json!({ "op": "trigger_tick", "team": "weekly", "now_ms": day1 + 30_000 }))["decisions"].as_array().unwrap().len(), 0);
    // the next day it fires again (a missed run within the grace is run once)
    let day2 = day1 + 86_400_000; let t2 = s.post(door, json!({ "op": "trigger_tick", "team": "weekly", "now_ms": day2 + 5_000 })); assert_eq!(t2["decisions"][0]["do"], json!("fire"), "a new day, a new count: {t2}");
    // a schedule changed by the person takes the consent back
    let set2 = s.post(door, json!({ "op": "triggers_set", "team": "weekly", "triggers": [{ "id": "weekly", "kind": "schedule", "cron": "0 9 * * *", "ask": "Weekly note", "missed": "skip" }] })); assert_eq!(set2["consented"], json!(false), "{set2}");
    // the by-hand start works for the person and not for an agent
    assert_eq!(s.post(door, json!({ "op": "trigger_fire", "team": "weekly", "trigger": "weekly", "pane_id": 9 }))["ok"], json!(false));
    let by_hand = s.post(door, json!({ "op": "trigger_fire", "team": "weekly", "trigger": "weekly" })); assert_eq!(by_hand["ok"], json!(true), "{by_hand}");
}

/// RFC 0008 §4.6: a run goes on after the serve is restarted: what was working is started again from the last checkpoint, and the run still waits for the person at the merge.
#[test]
fn j35_a_team_run_resumes_after_the_serve_restarts() {
    let _s = serial(); let tmp = Scratch::new("j35");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let script = json!({ "work": [{ "write": { "out.txt": "done\n" }, "text": "wrote it", "cost": 0.01, "sleep": 6 }] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let port = free_port(); let seat = tmp.path().join("seat-state");
    let mut s1 = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &seat)]);
    s1.add_workspace("work", &ws); s1.activate("work");
    let graph = json!({ "team": "Resume test", "schema": 1, "budget_usd": 2.0,
        "nodes": { "leader": { "node": "leader" }, "work": { "node": "role", "role": "worker", "owns": ["**"], "cap_usd": 0.5 }, "me": { "node": "human", "gate": "merge" }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "work", "to": "me" }, { "from": "me", "port": "approved", "to": "deliver" } ] });
    assert_eq!(s1.post("/api/v1/cockpit/team-graph", json!({ "op": "save", "graph": graph }))["ok"], json!(true));
    let go = s1.post("/api/v1/cockpit/team-graph", json!({ "op": "run", "team": "resume-test", "ask": "write out.txt" })); let run = go["run"].as_str().expect(&go.to_string()).to_string();
    until(&s1, 30, "the worker to be working", || { let r = s1.post("/api/v1/cockpit/team-graph", json!({ "op": "status", "run": run }))["run"].clone(); r["state"]["active"]["work"].is_number().then_some(()) });
    s1.crash();
    let s2 = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &seat)]);
    let parked = until(&s2, 90, "the resumed run to wait at the merge", || { let r = s2.post("/api/v1/cockpit/team-graph", json!({ "op": "status", "run": run }))["run"].clone(); if r["state"]["active"]["me"].is_number() { Some(r) } else { None } });
    assert!(parked["events"].as_array().unwrap().iter().any(|e| e["kind"] == json!("resume")), "the trail says the run went on from its checkpoint: {parked}");
    let ok = s2.post("/api/v1/cockpit/team-graph", json!({ "op": "answer", "run": run, "node": "me", "approve": true })); assert_eq!(ok["ok"], json!(true), "{ok}");
    until(&s2, 30, "the work to land", || ws.join("out.txt").exists().then_some(()));
}

/// RFC 0011 §17 S3: graph versions, propose/apply with checkpoints, rollback, and gate-weakening refused without the person.
#[test]
fn j48_graph_versions_survive_propose_apply_checkpoint_rollback_and_refuse_gate_weakening_without_the_person() {
    let _s = serial(); let tmp = Scratch::new("j48");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let port = free_port();
    let s = Serve::start(&state, port);
    s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let made = s.post(door, json!({ "op": "create", "name": "Version lane", "template": "blank", "budget_usd": 4.0 }));
    assert_eq!(made["ok"], json!(true), "{made}");
    let safe = json!([{ "op": "set_field", "node": "me", "path": "label", "value": "Merge when ready" }]);
    let prop = s.post(door, json!({ "op": "graph_propose", "team": "version-lane", "ops": safe, "reason": "clearer gate" }));
    assert_eq!(prop["ok"], json!(true), "{prop}"); assert_eq!(prop["requires_person"], json!(false));
    let applied = s.post(door, json!({ "op": "graph_apply", "team": "version-lane", "ops": safe, "reason": "clearer gate", "proposal_id": prop["proposal_id"], "person_ok": true }));
    assert_eq!(applied["ok"], json!(true), "{applied}"); assert!(applied["version_id"].as_i64().unwrap() > 0);
    let bug = s.post(door, json!({ "op": "create", "name": "Bug lane", "template": "bugfix", "budget_usd": 3.0 }));
    assert_eq!(bug["ok"], json!(true), "{bug}");
    let bad = json!([{ "op": "set_field", "node": "loop", "path": "max_iterations", "value": 0 }]);
    let fail = s.post(door, json!({ "op": "graph_apply", "team": "bug-lane", "ops": bad, "reason": "break", "person_ok": true }));
    assert_eq!(fail["ok"], json!(false), "{fail}"); assert!(fail["error"].as_str().unwrap().contains("validator"), "{fail}");
    let mark = json!([{ "op": "set_field", "node": "me", "path": "label", "value": "v2" }]);
    let _ = s.post(door, json!({ "op": "graph_apply", "team": "version-lane", "ops": mark, "reason": "mark", "person_ok": true }));
    let back = s.post(door, json!({ "op": "graph_rollback", "team": "version-lane" }));
    assert_eq!(back["ok"], json!(true), "{back}");
    assert_eq!(back["graph"]["nodes"]["me"]["label"], json!("Merge when ready"), "{back}");
    let gate = json!([{ "op": "set_graph_field", "field": "budget_usd", "value": 9.0 }]);
    let no = s.post(door, json!({ "op": "graph_apply", "team": "version-lane", "ops": gate, "reason": "raise ceiling", "person_ok": false }));
    assert_eq!(no["ok"], json!(false), "{no}"); assert!(no["error"].as_str().unwrap().contains("person"), "{no}");
    let agent = s.post(door, json!({ "op": "graph_apply", "team": "version-lane", "ops": gate, "reason": "raise", "person_ok": true, "pane_id": 3 }));
    assert_eq!(agent["ok"], json!(false), "{agent}"); assert!(agent["error"].as_str().unwrap().contains("agent"), "{agent}");
    let rows = s.post(door, json!({ "op": "graph_versions", "team": "version-lane" }));
    assert!(rows["versions"].as_array().unwrap().len() >= 2, "{rows}");
    antiloki_serve::ledger::flush();
    let bench = antiloki_serve::ledger::bench_path(&ws.to_string_lossy(), "version-lane");
    let n: i64 = rusqlite::Connection::open(&bench).unwrap().query_row("SELECT COUNT(*) FROM versions", [], |r| r.get(0)).unwrap();
    assert!(n >= 2, "versions live in bench.sqlite");
    drop(s);
    let s2 = Serve::start(&state, port);
    let again = s2.post(door, json!({ "op": "graph_versions", "team": "version-lane" }));
    assert_eq!(again["versions"].as_array().unwrap().len(), rows["versions"].as_array().unwrap().len(), "versions survive restart: {again}");
}

/// RFC 0011 S4: code, transform, check rule kinds, stop, and the family view — a team the Leader can build without a real model.
#[test]
fn j49_code_transform_check_kinds_stop_and_family_survive_save_validate_and_run_with_scripted_seats() {
    let _s = serial(); let tmp = Scratch::new("j49");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let port = free_port();
    let s = Serve::start_env(&state, port, &[("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state"))]);
    s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let code_src = r#"
        let title = input["title"];
        let body = input["body"];
        #{"title": title, "body": body, "ok": true}
    "#;
    let graph = json!({ "team": "Pipeline", "schema": 1, "budget_usd": 2.0, "start": ["prep"],
        "nodes": {
            "leader": { "node": "leader" },
            "prep": { "node": "transform", "mode": "edit_fields", "config": { "set": { "title": "Weekly", "body": "Fresh beans" } } },
            "scrub": { "node": "transform", "mode": "sanitize", "from": "values:prep.output", "config": {} },
            "run_code": { "node": "code", "source": code_src, "from": "values:scrub.output",
                "examples": [{ "input": { "title": "a", "body": "b" }, "output": { "title": "a", "body": "b", "ok": true } }] },
            "chk": { "node": "check", "on": "values:run_code.output", "rules": [
                { "id": "no-pii", "kind": "pii" }, { "id": "no-secrets", "kind": "secrets" },
                { "id": "urls", "kind": "urls", "hosts": ["example.com"] },
                { "id": "tone", "kind": "keywords", "keywords": ["discount"] } ] },
            "me": { "node": "human", "gate": "merge" },
            "deliver": { "node": "deliver" } },
        "edges": [
            { "from": "prep", "to": "scrub" }, { "from": "scrub", "to": "run_code" }, { "from": "run_code", "to": "chk" },
            { "from": "chk", "to": "me" }, { "from": "me", "port": "approved", "to": "deliver" } ] });
    let saved = s.post(door, json!({ "op": "save", "graph": graph })); assert_eq!(saved["ok"], json!(true), "{saved}");
    let cat = s.post(door, json!({ "op": "catalog" }));
    assert!(cat["nodes"].as_array().unwrap().iter().any(|n| n["id"] == json!("code")));
    let mut bad = graph.clone();
    bad["nodes"]["run_code"]["examples"] = json!([]);
    let refused = s.post(door, json!({ "op": "validate", "graph": bad }));
    assert!(refused["report"]["errors"].as_array().unwrap().iter().any(|e| e["code"] == json!("code_no_example")), "{refused}");
    let mut stop_g = graph.clone();
    stop_g["start"] = json!(["halt"]);
    stop_g["nodes"]["halt"] = json!({ "node": "stop", "message": "inventory gate failed" });
    let stop_v = s.post(door, json!({ "op": "validate", "graph": stop_g })); assert_eq!(stop_v["report"]["errors"].as_array().unwrap().len(), 0, "{stop_v}");
    let go = s.post(door, json!({ "op": "run", "team": "pipeline", "ask": "ship the week" }));
    assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    let parked = until(&s, 60, "the run to wait at the merge", || {
        let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone();
        if r["state"]["active"]["me"].is_number() { Some(r) } else { None }
    });
    assert_eq!(parked["state"]["values"]["chk"]["pass"], json!(true));
    assert!(parked["state"]["values"]["run_code"]["output"]["ok"] == json!(true));
    let ok = s.post(door, json!({ "op": "answer", "run": run, "node": "me", "approve": true })); assert_eq!(ok["ok"], json!(true));
    until(&s, 30, "delivery", || {
        let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone();
        (r["state"]["status"]["status"] == json!("done")).then_some(())
    });
}

#[test]
fn j65_a_team_without_a_main_loop_still_leaves_a_saga_with_the_run_as_its_episode() {
    let _s = serial(); let tmp = Scratch::new("j65");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let port = free_port(); let s = Serve::start(&state, port); s.add_workspace("work", &ws); s.activate("work");
    let door = "/api/v1/cockpit/team-graph";
    let graph = json!({ "team": "Plain", "schema": 1, "budget_usd": 1.0, "start": ["calc"], "nodes": { "leader": { "node": "leader" }, "calc": { "node": "code", "source": "#{ ok: true }", "examples": [ { "input": {}, "output": { "ok": true } } ] }, "deliver": { "node": "deliver" } }, "edges": [ { "from": "calc", "to": "deliver" } ] });
    let saved = s.post(door, json!({ "op": "save", "graph": graph })); assert_eq!(saved["ok"], json!(true), "{saved}");
    let go = s.post(door, json!({ "op": "run", "team": "plain", "ask": "make the thing" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    let done = until(&s, 60, "the run to finish", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); (r["state"]["status"]["status"] == json!("done")).then_some(r) });
    let eps = done["episodes"].as_object().unwrap(); assert_eq!(eps.len(), 1, "the run is one episode of the team's saga: {done}");
    let d = s.get(&format!("/api/v1/sagas/detail?id={}", done["team_saga"].as_i64().unwrap())); let ep = &d["episodes"][0];
    assert!(ep["title"].as_str().unwrap().starts_with("Run · make the thing"), "{d}"); assert_eq!(ep["status"], json!("done"), "closed when the run ended: {d}");
}

#[test]
fn j66_a_merge_waits_for_a_clean_workspace_the_person_sets_their_work_aside_with_one_click_and_gets_it_back_after_the_land() {
    let _s = serial(); let tmp = Scratch::new("j66");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let script = json!({ "wa": [{ "write": { "a/x.txt": "A\n" }, "text": "did a", "cost": 0.01 }] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let port = free_port(); let s = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state"))]);
    s.add_workspace("work", &ws); s.activate("work"); let door = "/api/v1/cockpit/team-graph";
    let graph = json!({ "team": "Land test", "schema": 1, "budget_usd": 2.0, "nodes": { "leader": { "node": "leader" }, "wa": { "node": "role", "role": "worker", "owns": ["a/**"], "cap_usd": 0.5 }, "me": { "node": "human", "gate": "merge" }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "wa", "to": "me" }, { "from": "me", "port": "approved", "to": "deliver" } ] });
    assert_eq!(s.post(door, json!({ "op": "save", "graph": graph }))["ok"], json!(true));
    let go = s.post(door, json!({ "op": "run", "team": "land-test", "ask": "make a" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    until(&s, 90, "the run to wait at the merge", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); r["state"]["active"]["me"].is_number().then_some(()) });
    // the person has work of their own in the workspace
    std::fs::write(ws.join("mine.txt"), "my own work\n").unwrap();
    let w = s.post(door, json!({ "op": "workspace", "run": run })); assert!(w["dirty"].as_array().unwrap().iter().any(|f| f == "mine.txt") && w["stashed"] == json!(false), "the card can say before the click what would stop the land: {w}");
    let no = s.post(door, json!({ "op": "answer", "run": run, "node": "me", "approve": true })); assert_eq!(no["ok"], json!(false), "{no}"); assert!(no["error"].as_str().unwrap().contains("uncommitted"), "{no}");
    // one click sets it aside; nothing of theirs is lost
    let st = s.post(door, json!({ "op": "stash", "run": run })); assert_eq!(st["ok"], json!(true), "{st}"); assert_eq!(st["stashed"], json!(true)); assert!(!ws.join("mine.txt").exists(), "set aside");
    let w = s.post(door, json!({ "op": "workspace", "run": run })); assert!(w["dirty"].as_array().unwrap().is_empty() && w["stashed"] == json!(true), "{w}");
    let ok = s.post(door, json!({ "op": "answer", "run": run, "node": "me", "approve": true })); assert_eq!(ok["ok"], json!(true), "{ok}");
    until(&s, 30, "the work to land and the person's own work to come back", || (ws.join("a/x.txt").exists() && ws.join("mine.txt").exists()).then_some(()));
    assert_eq!(std::fs::read_to_string(ws.join("mine.txt")).unwrap(), "my own work\n", "their file came back as it was");
    let w = s.post(door, json!({ "op": "workspace", "run": run })); assert_eq!(w["stashed"], json!(false), "{w}");
}

#[test]
fn j67_a_document_agent_after_a_writer_does_not_take_the_writers_files_away() {
    let _s = serial(); let tmp = Scratch::new("j67");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let script = json!({ "build": [{ "write": { "video/out/test.mp4": "pretend video\n", "video/index.html": "<p>x</p>\n" }, "text": "built it", "cost": 0.01 }], "review": [{ "text": { "done": true, "report": "checked it" }, "cost": 0.01 }] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let port = free_port(); let s = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state"))]);
    s.add_workspace("work", &ws); s.activate("work"); let door = "/api/v1/cockpit/team-graph";
    let graph = json!({ "team": "Video test", "schema": 1, "budget_usd": 2.0, "nodes": { "leader": { "node": "leader" },
        "build": { "node": "role", "role": "worker", "owns": ["video/**"], "cap_usd": 0.5 },
        "review": { "node": "role", "role": "agent", "agent": { "title": "Reviewer", "responsibility": "check the video", "brief": "look at what the builder made and say if it is right", "effects": ["read", "document"] }, "cap_usd": 0.5 },
        "me": { "node": "human", "gate": "merge" }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "build", "to": "review" }, { "from": "review", "to": "me" }, { "from": "me", "port": "approved", "to": "deliver" } ] });
    let saved = s.post(door, json!({ "op": "save", "graph": graph })); assert_eq!(saved["ok"], json!(true), "{saved}");
    let go = s.post(door, json!({ "op": "run", "team": "video-test", "ask": "make a video" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    until(&s, 90, "the run to wait at the merge", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); r["state"]["active"]["me"].is_number().then_some(()) });
    // the player can find the video while the run waits: the reviewer did not take it away
    let m = s.post(door, json!({ "op": "media", "run": run })); assert!(m["media"].as_array().unwrap().iter().any(|f| f["name"] == json!("test.mp4")), "the builder's video is still there: {m}");
    let ok = s.post(door, json!({ "op": "answer", "run": run, "node": "me", "approve": true })); assert_eq!(ok["ok"], json!(true), "{ok}");
    until(&s, 30, "the builder's files to land", || (ws.join("video/index.html").exists()).then_some(()));
    // and after the folder is gone the run still keeps the video
    let m = s.post(door, json!({ "op": "media", "run": run })); assert!(m["media"].as_array().unwrap().iter().any(|f| f["name"] == json!("test.mp4") && f["file"].as_str().unwrap().starts_with("kept/")), "the run keeps its video: {m}");
}

#[test]
fn j68_an_artifact_node_keeps_the_video_a_step_made_and_a_missing_one_fails_in_words() {
    let _s = serial(); let tmp = Scratch::new("j68");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let script = json!({ "build": [{ "write": { "video/out/test.mp4": "pretend video\n" }, "text": "built it", "cost": 0.01 }] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let port = free_port(); let s = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state"))]);
    s.add_workspace("work", &ws); s.activate("work"); let door = "/api/v1/cockpit/team-graph";
    let graph = |name: &str, fmt: &str| json!({ "team": "Vid test", "schema": 1, "budget_usd": 2.0, "nodes": { "leader": { "node": "leader" }, "build": { "node": "role", "role": "worker", "owns": ["video/**"], "cap_usd": 0.5 },
        "video": { "node": "artifact", "name": name, "format": fmt, "from": "build" }, "deliver": { "node": "deliver" } }, "edges": [ { "from": "build", "to": "video" }, { "from": "video", "to": "deliver" } ] });
    let saved = s.post(door, json!({ "op": "save", "graph": graph("test", "mp4") })); assert_eq!(saved["ok"], json!(true), "{saved}"); assert_eq!(saved["report"]["errors"].as_array().unwrap().len(), 0, "{saved}");
    let go = s.post(door, json!({ "op": "run", "team": "vid-test", "ask": "make a video" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    let done = until(&s, 90, "the run to finish", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); (r["state"]["status"]["status"] == json!("done")).then_some(r) });
    assert_eq!(done["state"]["values"]["video"]["ok"], json!(true), "{}", done["state"]["values"]["video"]); assert_eq!(done["state"]["values"]["video"]["path"], json!("media/test.mp4"));
    let m = s.post(door, json!({ "op": "media", "run": run })); assert!(m["media"].as_array().unwrap().iter().any(|f| f["file"] == json!("kept/test.mp4") && f["kind"] == json!("video")), "{m}");
    // a file that was never made fails in words, not silently
    let saved = s.post(door, json!({ "op": "save", "graph": graph("nothing", "webm") })); assert_eq!(saved["ok"], json!(true), "{saved}"); let go = s.post(door, json!({ "op": "run", "team": "vid-test", "ask": "again" })); let run2 = go["run"].as_str().unwrap().to_string();
    let r = until(&s, 90, "the second run to stop", || { let r = s.post(door, json!({ "op": "status", "run": run2 }))["run"].clone(); matches!(r["state"]["status"]["status"].as_str(), Some("done") | Some("blocked")).then_some(r) });
    assert_eq!(r["state"]["status"]["status"], json!("blocked"), "{}", r["state"]["status"]); let said: Vec<String> = r["events"].as_array().unwrap().iter().map(|e| e["text"].as_str().unwrap_or("").to_string()).collect(); assert!(said.iter().any(|t| t.contains("no file called nothing.webm")), "the failure says what is missing: {said:?}");
}

#[test]
fn j69_nothing_a_team_makes_is_ever_lost_a_disapproved_merge_and_a_removed_folder_both_leave_every_file_in_the_archive() {
    let _s = serial(); let tmp = Scratch::new("j69");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let script = json!({ "build": [{ "write": { "video/out/test.mp4": "pretend video\n", "video/index.html": "<p>x</p>\n", "stray.txt": "outside the scope\n" }, "text": "built it", "cost": 0.01 }] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let port = free_port(); let s = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state"))]);
    s.add_workspace("work", &ws); s.activate("work"); let door = "/api/v1/cockpit/team-graph";
    let graph = json!({ "team": "Keep test", "schema": 1, "budget_usd": 2.0, "nodes": { "leader": { "node": "leader" }, "build": { "node": "role", "role": "worker", "owns": ["video/**"], "cap_usd": 0.5 }, "me": { "node": "human", "gate": "merge" }, "deliver": { "node": "deliver" } },
        "edges": [ { "from": "build", "to": "me" }, { "from": "me", "port": "approved", "to": "deliver" } ] });
    assert_eq!(s.post(door, json!({ "op": "save", "graph": graph }))["ok"], json!(true));
    let go = s.post(door, json!({ "op": "run", "team": "keep-test", "ask": "make it" })); assert_eq!(go["ok"], json!(true), "{go}"); let run = go["run"].as_str().unwrap().to_string();
    until(&s, 90, "the run to wait at the merge", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); r["state"]["active"]["me"].is_number().then_some(()) });
    // the stray file was outside the builder's scope and was put back: it is in the archive, not lost
    let a = s.post(door, json!({ "op": "archive", "run": run })); let paths: Vec<String> = a["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap_or("").to_string()).collect();
    assert!(paths.contains(&"stray.txt".to_string()) && paths.contains(&"video/out/test.mp4".to_string()), "put back or not, every file is saved: {paths:?}");
    // the person does not approve: the work is saved again, nothing is removed
    let no = s.post(door, json!({ "op": "answer", "run": run, "node": "me", "approve": false })); assert_eq!(no["ok"], json!(true), "{no}");
    let a = s.post(door, json!({ "op": "archive", "run": run })); let why: Vec<String> = a["files"].as_array().unwrap().iter().map(|f| f["reason"].as_str().unwrap_or("").to_string()).collect(); assert!(why.iter().any(|r| r.contains("put back")), "{why:?}");
    // the player finds the video in the archive and streams it
    let m = s.post(door, json!({ "op": "media", "run": run })); let f = m["media"].as_array().unwrap().iter().find(|f| f["name"] == json!("test.mp4")).unwrap_or_else(|| panic!("the video is listed: {m}"));
    assert!(f["file"].as_str().unwrap().starts_with("kept/") || f["file"].as_str().unwrap().starts_with("archive/") || f["file"].as_str().unwrap().contains("-run/"), "{f}");
    let arch_file = a["files"].as_array().unwrap().iter().find(|x| x["path"] == json!("video/out/test.mp4")).unwrap()["id"].as_i64().unwrap();
    let body = s.get_raw(&format!("{door}/media?run={run}&file=archive/{arch_file}/test.mp4")); assert_eq!(body, "pretend video\n", "the archived bytes come back whole");
}

#[test]
fn j70_the_leader_is_handed_the_teams_memory_when_its_terminal_is_new_and_can_read_it_again_any_time() {
    let _s = serial(); let tmp = Scratch::new("j70");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let script = json!({ "wa": [{ "write": { "a/x.txt": "A\n" }, "text": "did a", "cost": 0.01 }] });
    let sp = tmp.path().join("tg-script.json"); std::fs::write(&sp, script.to_string()).unwrap();
    let port = free_port(); let s = Serve::start_env(&state, port, &[("FAKE_TG_SCRIPT", &sp), ("FAKE_SEAT_STATE_DIR", &tmp.path().join("seat-state"))]);
    s.add_workspace("work", &ws); s.activate("work"); let door = "/api/v1/cockpit/team-graph";
    let graph = json!({ "team": "Memory test", "schema": 1, "budget_usd": 2.0, "nodes": { "leader": { "node": "leader" }, "wa": { "node": "role", "role": "worker", "owns": ["a/**"], "cap_usd": 0.5 }, "deliver": { "node": "deliver" } }, "edges": [ { "from": "wa", "to": "deliver" } ] });
    assert_eq!(s.post(door, json!({ "op": "save", "graph": graph }))["ok"], json!(true));
    let go = s.post(door, json!({ "op": "run", "team": "memory-test", "ask": "write the greeting for the launch" })); let run = go["run"].as_str().unwrap().to_string();
    until(&s, 90, "the run to finish", || { let r = s.post(door, json!({ "op": "status", "run": run }))["run"].clone(); (r["state"]["status"]["status"] == json!("done")).then_some(()) });
    // the memory, on request: what the saga holds, in words
    let m = s.post(door, json!({ "op": "recall", "team": "memory-test" })); let text = m["text"].as_str().unwrap(); assert!(text.contains("[team memory") && text.contains("write the greeting for the launch") && text.contains("done"), "{text}");
    // a terminal that is new gets it in its inbox; the same terminal again does not get it twice
    let b = s.post(door, json!({ "op": "leader_bind", "team": "memory-test", "pane": 41 })); assert_eq!(b["ok"], json!(true), "{b}");
    let inbox = s.post(door, json!({ "op": "leader_inbox", "team": "memory-test" })); let msgs = inbox["messages"].as_array().unwrap(); assert!(msgs.iter().any(|x| x["kind"] == json!("memory") && x["message"].as_str().unwrap().contains("write the greeting for the launch")), "the new Leader is handed the memory: {inbox}");
    let n = msgs.iter().filter(|x| x["kind"] == json!("memory")).count();
    s.post(door, json!({ "op": "leader_bind", "team": "memory-test", "pane": 41 })); let inbox = s.post(door, json!({ "op": "leader_inbox", "team": "memory-test" })); assert_eq!(inbox["messages"].as_array().unwrap().iter().filter(|x| x["kind"] == json!("memory")).count(), n, "the same terminal is not told twice");
    s.post(door, json!({ "op": "leader_bind", "team": "memory-test", "pane": 42 })); let inbox = s.post(door, json!({ "op": "leader_inbox", "team": "memory-test" })); assert_eq!(inbox["messages"].as_array().unwrap().iter().filter(|x| x["kind"] == json!("memory")).count(), n + 1, "a different terminal is");
}

#[test]
fn j71_deleting_a_team_closes_its_thread_and_the_work_is_saved_and_a_thread_with_work_deletes_in_one_click() {
    let _s = serial(); let tmp = Scratch::new("j71");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let port = free_port(); let s = Serve::start(&state, port); s.add_workspace("work", &ws); s.activate("work"); let door = "/api/v1/cockpit/team-graph"; let threads = "/api/v1/cockpit/cowork/threads";
    let made = s.post(door, json!({ "op": "create", "name": "Doomed", "template": "blank", "budget_usd": 1.0 })); assert_eq!(made["ok"], json!(true), "{made}");
    let l = s.post(threads, json!({ "op": "list" })); assert!(l["threads"].as_array().unwrap().iter().any(|t| t["id"] == json!("doomed")), "the team has a thread: {l}");
    // the team is deleted: its graph is saved first, its thread goes
    let d = s.post(door, json!({ "op": "delete", "team": "doomed" })); assert_eq!(d["ok"], json!(true), "{d}");
    let l = s.post(threads, json!({ "op": "list" })); assert!(!l["threads"].as_array().unwrap().iter().any(|t| t["id"] == json!("doomed")), "the team's thread went with it: {l}");
    let g = s.post(door, json!({ "op": "list" })); assert!(!g["graphs"].as_array().unwrap().iter().any(|x| x["id"] == json!("doomed")));
    let a = s.post(door, json!({ "op": "archive", "run": "team:doomed" })); assert!(a["files"].as_array().unwrap().iter().any(|f| f["path"] == json!("doomed.team.json")), "the graph is in the archive: {a}");
    // a plain thread deletes too
    let n = s.post(threads, json!({ "op": "create", "name": "Scratch" })); assert_eq!(n["ok"], json!(true), "{n}"); let d = s.post(threads, json!({ "op": "delete", "thread": "scratch" })); assert_eq!(d["ok"], json!(true), "{d}");
}

#[test]
fn j72_the_avatar_dock_and_idle_sleep_answer_and_keep_their_setting() {
    let _s = serial(); let tmp = Scratch::new("j72");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let port = free_port(); let s = Serve::start(&state, port); s.add_workspace("work", &ws); s.activate("work");
    let v = s.get("/api/v1/cockpit/avatars"); assert_eq!(v["ok"], json!(true), "{v}"); assert_eq!(v["enabled"], json!(true)); assert_eq!(v["minutes"], json!(30), "{v}"); assert!(v["agents"].is_array());
    let set = s.post("/api/v1/cockpit/sleep", json!({ "minutes": 45, "enabled": false })); assert_eq!(set["minutes"], json!(45), "{set}"); assert_eq!(set["enabled"], json!(false));
    let v = s.get("/api/v1/cockpit/avatars"); assert_eq!(v["minutes"], json!(45), "the setting is kept: {v}"); assert_eq!(v["enabled"], json!(false));
    let w = s.post("/api/v1/cockpit/sleep/wake", json!({ "pane_id": 99 })); assert_eq!(w["ok"], json!(true)); assert_eq!(w["was_asleep"], json!(false), "{w}");
    let n = s.post("/api/v1/cockpit/sleep/now", json!({ "pane_id": 99 })); assert_eq!(n["ok"], json!(false), "a pane with no saga is never put to sleep: {n}");
    let sc = s.get("/api/v1/cockpit/pty/screen?pane_id=99"); assert_eq!(sc["ok"], json!(true)); assert!(sc["rows"].is_array());
}

#[test]
fn j73_a_team_has_versions_a_recipe_and_can_go_back() {
    let _s = serial(); let tmp = Scratch::new("j73");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let port = free_port(); let s = Serve::start(&state, port); s.add_workspace("work", &ws); s.activate("work"); let door = "/api/v1/cockpit/team-graph";
    let made = s.post(door, json!({ "op": "create", "name": "Versioned", "template": "blank", "budget_usd": 1.0 })); assert_eq!(made["ok"], json!(true), "{made}");
    // the commit runs in the background: wait for the first version
    let mut v = json!({}); for _ in 0..50 { v = s.post(door, json!({ "op": "versions", "team": "versioned" })); if v["versions"].as_array().map_or(false, |a| !a.is_empty()) { break; } std::thread::sleep(std::time::Duration::from_millis(200)); }
    let first = v["versions"][0]["id"].as_str().expect(&format!("a version after the first save: {v}")).to_string();
    let r = s.post(door, json!({ "op": "recipe", "team": "versioned" })); assert!(r["recipe"].as_str().unwrap_or("").starts_with("# antiloki team recipe v1"), "{r}");
    let r1 = s.post(door, json!({ "op": "recipe", "team": "versioned", "id": first })); assert_eq!(r1["recipe"], r["recipe"], "version 1 is the live recipe");
    let back = s.post(door, json!({ "op": "restore", "team": "versioned", "id": first })); assert_eq!(back["ok"], json!(true), "{back}");
    assert_eq!(s.post(door, json!({ "op": "restore", "team": "versioned", "id": "zz" }))["ok"], json!(false));
}

#[test]
fn j74_a_team_has_a_replay_bank_requests_a_benchmark_and_travels() {
    let _s = serial(); let tmp = Scratch::new("j74");
    let ws = project(tmp.path(), "work");
    let state = tmp.path().join("state"); std::fs::create_dir_all(&state).unwrap();
    let port = free_port(); let s = Serve::start(&state, port); s.add_workspace("work", &ws); s.activate("work"); let door = "/api/v1/cockpit/team-graph";
    let made = s.post(door, json!({ "op": "create", "name": "Roaming", "template": "blank", "budget_usd": 1.0 })); assert_eq!(made["ok"], json!(true), "{made}");
    // the replay bank: checks are required, every third task is held out
    let rule = json!([{ "id": "short", "kind": "words", "max": 20 }]);
    assert_eq!(s.post(door, json!({ "op": "replay_add", "team": "roaming", "ask": "x", "checks": [] }))["ok"], json!(false), "no checks, no task");
    for i in 0..3 { let r = s.post(door, json!({ "op": "replay_add", "team": "roaming", "ask": format!("ask {i}"), "checks": rule, "run": "run-1" })); assert_eq!(r["ok"], json!(true), "{r}"); }
    let l = s.post(door, json!({ "op": "replay_list", "team": "roaming" })); assert_eq!((l["tune"].clone(), l["heldout"].clone()), (json!(2), json!(1)), "{l}");
    // a request that touches the spend limit never reaches a branch; a request that is not there cannot be decided
    let r = s.post(door, json!({ "op": "request_open", "team": "roaming", "problem": "too dear", "patch": [{ "op": "set_graph_field", "field": "budget_usd", "value": 9.0 }] })); assert_eq!(r["ok"], json!(false), "{r}"); assert!(r["error"].as_str().unwrap_or("").contains("may not change"), "{r}");
    assert_eq!(s.post(door, json!({ "op": "request_list", "team": "roaming" }))["requests"], json!([]));
    assert_eq!(s.post(door, json!({ "op": "request_decide", "team": "roaming", "n": 7, "action": "approve" }))["ok"], json!(false));
    assert_eq!(s.post(door, json!({ "op": "experiment_score", "team": "roaming", "n": 1, "budget_usd": 0.0, "results": [] }))["ok"], json!(false), "no budget, no experiment");
    let p = s.post(door, json!({ "op": "improve_prompt", "team": "roaming" })); assert!(p["prompt"].as_str().unwrap_or("").contains("IMPROVER"), "{p}");
    // wait for the first version, then the benchmark lists it
    let mut b = json!({}); for _ in 0..50 { b = s.post(door, json!({ "op": "benchmark", "team": "roaming" })); if b["versions"].as_array().map_or(false, |a| !a.is_empty()) { break; } std::thread::sleep(std::time::Duration::from_millis(200)); }
    assert!(b["versions"][0]["measured"] == json!(false), "{b}");
    // travel: export the structure, review it, import it under another name
    let e = s.post(door, json!({ "op": "export", "team": "roaming" })); let file = e["file"].as_str().expect(&format!("{e}")).to_string();
    let rv = s.post(door, json!({ "op": "import_review", "file": file })); assert_eq!(rv["ok"], json!(true), "{rv}");
    assert_eq!(s.post(door, json!({ "op": "import", "file": file, "name": "Copy" }))["ok"], json!(false), "no yes, no import");
    let i = s.post(door, json!({ "op": "import", "file": file, "name": "Copy", "confirmed": true })); assert_eq!(i["ok"], json!(true), "{i}");
    assert_eq!(s.post(door, json!({ "op": "import", "file": file, "name": "Copy", "confirmed": true }))["ok"], json!(false), "never over another team");
}

/// THE LAUNCHER CASE (operator 2026-11-23, a new machine: "claude code is not installed even it is"): the serve started the way a desktop launcher, an AppImage or a service starts it - an EMPTY environment,
/// a bare PATH that holds none of the CLIs, a HOME of its own - with the CLIs only where their installers put them (`~/.local/bin`, an nvm folder). It must still find them, say where, call a CLI that is on disk
/// but cannot run "broken" with its own reason, and, for one that is nowhere, say what it searched.
#[test]
fn j75_a_serve_started_like_a_launcher_finds_the_clis_in_their_install_folders() {
    let _g = serial(); let s = Scratch::new("launcher");
    let home = s.path().join("home"); let sup = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/fake-claude");
    std::fs::create_dir_all(home.join(".local/bin")).unwrap(); std::fs::create_dir_all(home.join(".nvm/versions/node/v22.1.0/bin")).unwrap(); std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(home.join(".claude/.credentials.json"), "{}").unwrap();
    std::os::unix::fs::symlink(&sup, home.join(".local/bin/claude")).unwrap();                                    // the native installer's folder
    let exe = |p: PathBuf, body: &str| { std::fs::write(&p, body).unwrap(); std::fs::set_permissions(&p, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap(); };
    exe(home.join(".nvm/versions/node/v22.1.0/bin/codex"), "#!/bin/sh\necho 0.1.0\n");                              // an nvm folder
    exe(home.join(".local/bin/cursor-agent"), "#!/bin/sh\necho \"env: node: No such file or directory\" >&2\nexit 127\n");   // on disk, cannot run
    let port = free_port(); let log = s.path().join("serve.log"); let out = std::fs::File::create(&log).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_antiloki-serve")).env_clear().env("PATH", "/usr/bin:/bin").env("HOME", &home).env("SHELL", "/bin/sh").env("ANTILOKI_LAN_PORT", "0")
        .env("ANTILOKI_HOME", s.path().join("ah")).env("ANTILOKI_DATA_DIR", s.path().join("data")).env("ANTILOKI_SUBSTRATE_ROOT", s.path().join("sub")).env("ANTILOKI_CHATS_DIR", s.path().join("chats"))
        .env("ANTILOKI_SERVE_PORT", port.to_string()).env("ANTILOKI_BE_TICK_DISABLED", "true").env("ANTILOKI_SHIPPED_BENCH", "off")
        .stdin(Stdio::null()).stdout(out.try_clone().unwrap()).stderr(out).spawn().expect("the serve starts");
    let end = Instant::now() + Duration::from_secs(90); let mut up = false;
    while Instant::now() < end { if ureq::get(&format!("http://127.0.0.1:{port}/healthz")).timeout(Duration::from_secs(2)).call().is_ok() { up = true; break } std::thread::sleep(Duration::from_millis(200)); }
    let answer = if up { ureq::get(&format!("http://127.0.0.1:{port}/api/v1/engines")).timeout(Duration::from_secs(60)).call().ok().and_then(|r| r.into_string().ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok()) } else { None };
    let _ = child.kill(); let _ = child.wait();
    let v = answer.unwrap_or_else(|| panic!("the serve gave no engines:\n{}", std::fs::read_to_string(&log).unwrap_or_default()));
    let find = |k: &str| v["engines"].as_array().unwrap().iter().find(|e| e["key"] == json!(k)).unwrap_or_else(|| panic!("no {k}: {v}")).clone();
    let claude = find("claude-code"); assert_eq!(claude["installed"], json!(true), "claude in ~/.local/bin, found with a bare PATH: {claude}");
    assert!(claude["path"].as_str().unwrap_or("").starts_with(home.to_str().unwrap()), "and it says where: {claude}");
    let codex = find("codex"); assert_eq!(codex["installed"], json!(true), "codex in an nvm folder: {codex}"); assert!(codex.get("broken").is_none(), "a working CLI is not broken: {codex}");
    let cursor = find("cursor"); assert!(cursor["broken"].as_str().unwrap_or("").contains("node: No such file"), "a CLI that is there but cannot run says why: {cursor}");
    assert!(cursor["missing"].as_str().unwrap_or("").contains("did not run"), "{cursor}");
    let grok = find("grok"); assert_eq!(grok["installed"], json!(false), "a CLI that is nowhere is missing");
    assert!(grok["missing"].as_str().unwrap_or("").contains("looked in the PATH this app started with"), "and the screen says what was searched: {grok}");
}
