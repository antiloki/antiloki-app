//! `antiloki` (the cargo bin is `antiloki-chat`) against a REAL serve on a private stack: its own port, data root and git workspace, and the
//! scripted seat (`serve/tests/support/fake-claude`) as the only seat it can reach - no model, no network, no quota, and never your serve.
//! Ignored by default because it needs a built `antiloki-serve` and python3 (the scripted seat is a script):
//!
//!   cargo build -p antiloki-serve && cargo test -p antiloki-agent --test chat_e2e -- --ignored --nocapture
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CHAT: &str = env!("CARGO_BIN_EXE_antiloki-chat");

struct Guard(Child);
impl Drop for Guard { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }

fn free_port() -> u16 { std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port() }

fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = Command::new("git").args(["-c", "user.email=e2e@example.com", "-c", "user.name=e2e"]).args(args).current_dir(dir).status().unwrap().success();
    assert!(ok, "git {args:?}");
}

struct Stack { home: PathBuf, ws: PathBuf, url: String, _serve: Guard }

fn stack() -> Stack {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let serve_bin = std::env::var("ANTILOKI_SERVE_BIN").map(PathBuf::from).unwrap_or_else(|_| root.join("target/debug/antiloki-serve"));
    assert!(serve_bin.is_file(), "no serve binary at {} - `cargo build -p antiloki-serve` or set ANTILOKI_SERVE_BIN", serve_bin.display());
    let home = std::env::temp_dir().join(format!("antiloki-chat-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let (bin, fake_home, ws) = (home.join("bin"), home.join("fake-home"), home.join("ws"));
    for d in [&bin, &fake_home.join(".claude"), &ws] { std::fs::create_dir_all(d).unwrap(); }
    std::fs::write(fake_home.join(".claude/.credentials.json"), "{}").unwrap();
    std::os::unix::fs::symlink(root.join("serve/tests/support/fake-claude").canonicalize().unwrap(), bin.join("claude")).unwrap();
    git(&ws, &["init", "-q"]);
    std::fs::write(ws.join("README.md"), "# scratch\n").unwrap();
    git(&ws, &["add", "-A"]); git(&ws, &["commit", "-q", "-m", "init"]);
    let port = free_port();
    // a CLEARED environment: the scripted `claude` plus the system's own directories, so no real seat of this machine is reachable
    let serve = Command::new(&serve_bin).env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display())).env("HOME", &fake_home).env("FAKE_SEAT_STATE_DIR", home.join("fake-state"))
        .env("ANTILOKI_SERVE_PORT", port.to_string()).env("ANTILOKI_SUBSTRATE_ROOT", home.join("substrate")).env("ANTILOKI_DATA_DIR", home.join("data"))
        .env("ANTILOKI_HOME", home.join("home")).env("ANTILOKI_CHATS_DIR", home.join("chats"))
        .stdin(Stdio::null()).stdout(std::fs::File::create(home.join("serve.log")).unwrap()).stderr(Stdio::null()).spawn().unwrap();
    let url = format!("http://127.0.0.1:{port}");
    let end = Instant::now() + Duration::from_secs(60);
    while Instant::now() < end && std::net::TcpStream::connect(("127.0.0.1", port)).is_err() { std::thread::sleep(Duration::from_millis(200)); }
    Stack { home, ws, url, _serve: Guard(serve) }
}

fn chat(s: &Stack, args: &[&str], stdin: Option<&str>) -> (i32, String, String) {
    let mut c = Command::new(CHAT);
    c.args(["--no-color", "-C"]).arg(&s.ws).args(args).env("ANTILOKI_BE_URL", &s.url).env("XDG_STATE_HOME", s.home.join("state")).stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    if let Some(t) = stdin { child.stdin.take().unwrap().write_all(t.as_bytes()).unwrap(); }
    let o = child.wait_with_output().unwrap();
    (o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
#[ignore = "needs a built antiloki-serve and python3 for the scripted seat"]
fn antiloki_on_a_folder_routes_work_answers_a_question_and_chats() {
    let s = stack();

    // a task goes to Heimdall: planned, run by the scripted worker, reported
    let (code, out, err) = chat(&s, &["create the notes file FAKE:write:out/n.txt tag-e2e1"], None);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("you      create the notes file"), "{out}");
    assert!(out.contains("run #1 · running") && out.contains("run #1 · done"), "the run's states are printed: {out}");
    assert!(out.contains("report   REPORT: created out/n.txt."), "{out}");
    assert!(err.contains("antiloki · ws"), "the header names the workspace it opened: {err}");

    // a pleasantry is the agent's own, not Heimdall's
    let (code, out, err) = chat(&s, &["hello there"], None);
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("agent    ack:") && out.contains("hello there") && !out.contains("run #"), "{out}");

    // a run that needs an answer stops the one-shot with exit 3 and says what it needs...
    let (code, out, err) = chat(&s, &["create the report file FAKE:ask FAKE:write:out/q.txt tag-e2e2"], None);
    assert_eq!(code, 3, "{out}\n{err}");
    assert!(out.contains("awaiting-answers") && out.contains("Which format should the file use?") && out.contains("`go`"), "{out}");
    // ...and in a conversation the next line answers it (the serve's front door resolves `go` in code), the run finishes, and talk resumes
    let (code, out, err) = chat(&s, &["-i"], Some("go\nhello again\n/exit\n"));
    assert_eq!(code, 0, "{out}\n{err}");
    assert!(out.contains("answered run #") && out.contains("with the defaults") && out.contains("· done") && out.contains("REPORT: created out/q.txt."), "{out}");
    assert!(out.contains("agent    ack:") && out.contains("hello again"), "{out}");

    // the work is the window's too: the serve, which the window talks to, holds the runs
    let runs = ureq_get(&format!("{}/api/v1/cockpit/goals", s.url));
    assert!(runs.matches("\"status\":\"done\"").count() >= 2, "{runs}");

    // not a repository: a clear message, not a stack trace
    let plain = s.home.join("plain"); std::fs::create_dir_all(&plain).unwrap();
    let o = Command::new(CHAT).args(["-C"]).arg(&plain).arg("hi").env("ANTILOKI_BE_URL", &s.url).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("not inside a git repository"));
    let _ = std::fs::remove_dir_all(&s.home);
}

fn ureq_get(url: &str) -> String { ureq::get(url).call().unwrap().into_string().unwrap() }
