//! `antiloki` — the agent in your terminal (installed under that one word; the cargo bin is `antiloki-chat` because the engine crate already
//! owns the bin name `antiloki`). RFC 0001 §10.1, operator 2026-10-26: "like claude code, with a single word", "all in text", "on a workspace
//! folder", "use all tools of antiloki headless".
//!
//!   antiloki                      open the agent on this folder (type; /exit leaves)
//!   antiloki "add a retry to the checkout call"      one ask, print the log, leave
//!   antiloki -C ../other          another workspace folder
//!   antiloki app | stop | doctor
//!
//! It is a CLIENT. There is no agent loop here and no second engine: it talks to the same `antiloki-serve` the window talks to (attaching to
//! the one that is listening, or starting one), opens this folder as a workspace, finds its chat, and sends each line the way the window's
//! composer does: the serve's FRONT door first (closed replies like `go` / `yes` / `cancel` are resolved in code), then a TASK or a QUESTION
//! goes to Heimdall's queue and anything else is said to the agent. What comes back is the pane's own event log and the goal run's state,
//! printed as lines of text — so what you see here is what the window shows, and the work is in the window afterwards.
use serde_json::{json, Value};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DEFAULT_URL: &str = "http://127.0.0.1:41931";
const VERSION: &str = env!("CARGO_PKG_VERSION");

const HELP: &str = "antiloki — the agent in your terminal

  antiloki                          open the agent on the current folder; type, /exit leaves
  antiloki \"<ask>\"                  one ask: print what happens, then leave
  echo \"<ask>\" | antiloki           the same, from a pipe
  antiloki -C <folder>              work on another folder (a git repository)
  antiloki app                      open the antiloki window
  antiloki stop                     stop the serve this command started
  antiloki doctor                   what this machine has (seats, browser, git)

options:  -C, --workspace <dir>   --new   start a new conversation     --json   one event per line as JSON
          -i, --interactive       read the conversation line by line from stdin even when it is a pipe
          --no-color              -h, --help      -V, --version

In the conversation:  /help  /new  /stop (stop the agent)  /exit

It uses the serve the window uses (127.0.0.1:41931, or $ANTILOKI_BE_URL): one that is already listening is reused, otherwise this starts
one and leaves it running, so the next `antiloki` is instant. Close it with `antiloki stop` before opening the window if the window
says its port is taken. Exit codes: 0 done, 1 error, 2 usage, 3 the agent is waiting for your answer.
";

#[derive(Debug, PartialEq)]
enum Cmd { Help, Version, App, Stop, Doctor, Chat }

#[derive(Debug, PartialEq)]
struct Args { cmd: Cmd, workspace: Option<String>, json: bool, color: Option<bool>, new_chat: bool, interactive: bool, message: Option<String> }

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut a = Args { cmd: Cmd::Chat, workspace: None, json: false, color: None, new_chat: false, interactive: false, message: None };
    let mut words: Vec<String> = vec![];
    let mut i = 0;
    while i < argv.len() {
        let w = argv[i].as_str();
        match w {
            "-h" | "--help" | "help" if words.is_empty() => a.cmd = Cmd::Help,
            "-V" | "--version" => a.cmd = Cmd::Version,
            "app" if words.is_empty() && a.cmd == Cmd::Chat => a.cmd = Cmd::App,
            "stop" if words.is_empty() && a.cmd == Cmd::Chat => a.cmd = Cmd::Stop,
            "doctor" if words.is_empty() && a.cmd == Cmd::Chat => a.cmd = Cmd::Doctor,
            "-C" | "--workspace" => { i += 1; a.workspace = Some(argv.get(i).filter(|v| !v.starts_with('-')).cloned().ok_or(format!("{w} needs a folder."))?); }
            "--json" => a.json = true,
            "--no-color" => a.color = Some(false),
            "--new" => a.new_chat = true,
            "-i" | "--interactive" => a.interactive = true,
            "--" => { words.extend(argv[i + 1..].iter().cloned()); break; }
            o if o.starts_with('-') && o.len() > 1 && words.is_empty() => return Err(format!("unknown option {o}. Try --help.")),
            _ => words.push(w.to_string()),
        }
        i += 1;
    }
    if !words.is_empty() { a.message = Some(words.join(" ")); }
    Ok(a)
}

// ---- HTTP (the loopback serve; the socket is the trust boundary) ----

fn http(method: &str, url: &str, body: Option<&Value>, timeout_s: u64) -> Result<(u16, Value), String> {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(timeout_s)).build();
    let res = match (method, body) {
        ("POST", b) => agent.post(url).send_json(b.cloned().unwrap_or(json!({}))),
        _ => agent.get(url).call(),
    };
    match res {
        Ok(r) => { let st = r.status(); Ok((st, r.into_json().unwrap_or(Value::Null))) }
        Err(ureq::Error::Status(st, r)) => Ok((st, r.into_json().unwrap_or(Value::Null))),
        Err(e) => Err(e.to_string()),
    }
}

struct Api { base: String }
impl Api {
    fn get(&self, path: &str) -> Result<Value, String> { http("GET", &format!("{}{path}", self.base), None, 30).map(|(_, v)| v) }
    fn post(&self, path: &str, body: Value, timeout_s: u64) -> Result<Value, String> {
        let (st, v) = http("POST", &format!("{}{path}", self.base), Some(&body), timeout_s)?;
        // the serve wraps some answers in {result: …}; an error carries {error: {message}} or {error: "…"}
        if st >= 400 { return Err(error_text(&v).unwrap_or_else(|| format!("{path} answered {st}"))); }
        Ok(v.get("result").cloned().unwrap_or(v))
    }
    fn healthy(&self) -> bool { http("GET", &format!("{}/healthz", self.base), None, 2).map(|(s, _)| s == 200).unwrap_or(false) }
}

fn error_text(v: &Value) -> Option<String> {
    let e = v.get("error")?;
    e.as_str().map(String::from).or_else(|| e.get("message").and_then(Value::as_str).map(String::from))
}

// ---- finding and starting the serve ----

fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir).join("antiloki")
}

/// Where to look for a sibling program, in order: an explicit override, next to this executable, then PATH. Pure - tested.
fn candidates(name: &str, override_path: Option<String>, exe_dir: Option<PathBuf>, path_var: Option<String>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = vec![];
    if let Some(o) = override_path.filter(|o| !o.trim().is_empty()) { v.push(PathBuf::from(o)); }
    if let Some(d) = exe_dir { v.push(d.join(name)); }
    if let Some(p) = path_var { for d in std::env::split_paths(&p) { v.push(d.join(name)); } }
    v
}

fn find_program(name: &str, override_env: &str) -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
    candidates(name, std::env::var(override_env).ok(), exe_dir, std::env::var("PATH").ok()).into_iter().find(|p| p.is_file())
}

fn port_of(base: &str) -> String { base.rsplit(':').next().unwrap_or("41931").trim_end_matches('/').to_string() }

fn start_serve(api: &Api) -> Result<(), String> {
    let bin = find_program("antiloki-serve", "ANTILOKI_SERVE_BIN").ok_or("antiloki-serve was not found next to this program or on PATH (set ANTILOKI_SERVE_BIN).")?;
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let log = std::fs::File::create(dir.join("serve.log")).map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(&bin);
    cmd.env("ANTILOKI_SERVE_PORT", port_of(&api.base)).stdin(std::process::Stdio::null()).stdout(log.try_clone().map_err(|e| e.to_string())?).stderr(log);
    #[cfg(unix)]
    { use std::os::unix::process::CommandExt; cmd.process_group(0); }   // its own group: it outlives this command on purpose
    let child = cmd.spawn().map_err(|e| format!("could not start {}: {e}", bin.display()))?;
    let _ = std::fs::write(dir.join("serve.pid"), child.id().to_string());
    eprintln!("antiloki: starting the serve ({}) — the next antiloki attaches to it; `antiloki stop` ends it", bin.display());
    let end = Instant::now() + Duration::from_secs(90);
    while Instant::now() < end {
        if api.healthy() { return Ok(()); }
        std::thread::sleep(Duration::from_millis(300));
    }
    Err(format!("the serve did not answer on {} within 90 s — see {}", api.base, dir.join("serve.log").display()))
}

fn stop_serve() -> Result<String, String> {
    let pid_file = state_dir().join("serve.pid");
    let pid = std::fs::read_to_string(&pid_file).ok().and_then(|t| t.trim().parse::<u32>().ok());
    let Some(pid) = pid else { return Ok("nothing to stop: this command did not start a serve (a serve the window runs is closed from the window).".into()) };
    #[cfg(unix)]
    { let _ = std::process::Command::new("kill").args(["-TERM", &pid.to_string()]).status(); }
    #[cfg(not(unix))]
    { let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).status(); }
    let _ = std::fs::remove_file(&pid_file);
    Ok(format!("stopped the serve (pid {pid})."))
}

// ---- the workspace and its chat ----

/// The repository the folder is in: the agent works in a git worktree, so a folder outside a repository has nothing to cut one from.
fn repo_root(dir: &Path) -> Result<PathBuf, String> {
    let out = std::process::Command::new("git").args(["rev-parse", "--show-toplevel"]).current_dir(dir).output().map_err(|e| format!("git is needed: {e}"))?;
    if !out.status.success() { return Err(format!("{} is not inside a git repository — run `git init` there first (the agent works in a git worktree).", dir.display())); }
    Ok(PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

/// Which registry name this root has, or the one it should be added under: the folder's name, made unique. Pure - tested.
fn workspace_name(existing: &[(String, String)], root: &str) -> (String, bool) {
    if let Some((n, _)) = existing.iter().find(|(_, r)| r == root) { return (n.clone(), true); }
    let base = Path::new(root).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| "workspace".into());
    let mut name = base.clone();
    let mut k = 2;
    while existing.iter().any(|(n, _)| *n == name) { name = format!("{base}-{k}"); k += 1; }
    (name, false)
}

fn ensure_workspace(api: &Api, root: &str) -> Result<String, String> {
    let list = api.post("/api/v1/workspaces/list", json!({}), 30)?;
    let existing: Vec<(String, String)> = list["workspaces"].as_array().map(|a| a.iter().map(|w| (w["name"].as_str().unwrap_or("").to_string(), w["root_path"].as_str().unwrap_or("").to_string())).collect()).unwrap_or_default();
    let (name, known) = workspace_name(&existing, root);
    if !known {
        let r = api.post("/api/v1/workspaces/add", json!({ "name": name, "root_path": root }), 120)?;
        if r["ok"] != json!(true) { return Err(format!("could not add the workspace: {}", error_text(&r).unwrap_or_else(|| r.to_string()))); }
    }
    if list["active"]["name"].as_str() != Some(name.as_str()) {
        let r = api.post("/api/v1/workspaces/activate", json!({ "name": name }), 120)?;
        if r["ok"] != json!(true) { return Err(format!("could not open the workspace: {}", error_text(&r).unwrap_or_else(|| r.to_string()))); }
    }
    Ok(name)
}

struct Chat { pane_id: i64, session_id: i64, worktree: String }

fn open_chat(api: &Api, title: &str, new_chat: bool) -> Result<Chat, String> {
    if !new_chat {
        let chats = api.get("/api/v1/cockpit/chats")?;
        if let Some(c) = chats["chats"].as_array().and_then(|a| a.first()) {
            // the list does not carry the folder; the pane row does
            let pane_id = c["pane_id"].as_i64().unwrap_or(0);
            let session_id = c["session_id"].as_i64().unwrap_or(0);
            let worktree = pane_worktree(api, pane_id).unwrap_or_default();
            if pane_id > 0 { return Ok(Chat { pane_id, session_id, worktree }); }
        }
    }
    let r = api.post("/api/v1/cockpit/chat/create", json!({ "title": title }), 60)?;
    let p = &r["pane"];
    let pane_id = p["id"].as_i64().unwrap_or(0);
    if pane_id <= 0 { return Err(format!("could not open a conversation: {}", error_text(&r).unwrap_or_else(|| r.to_string()))); }
    Ok(Chat { pane_id, session_id: p["session_id"].as_i64().unwrap_or(0), worktree: p["worktree_path"].as_str().unwrap_or("").to_string() })
}

fn pane_worktree(api: &Api, pane_id: i64) -> Option<String> {
    // the antiQA route already answers with the pane's box; it is the cheapest read of it
    let r = api.get(&format!("/api/v1/cockpit/pane/antiqa?pane_id={pane_id}")).ok()?;
    r["worktree"].as_str().map(String::from)
}

// ---- rendering the pane's event log as text ----

fn clip(t: &str, n: usize) -> String {
    let one: String = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > n { format!("{}…", one.chars().take(n).collect::<String>()) } else { one }
}

fn tool_summary(input: &Value) -> String {
    for k in ["command", "file_path", "path", "pattern", "query", "url", "description"] {
        if let Some(s) = input.get(k).and_then(Value::as_str) { return clip(s, 110); }
    }
    clip(&input.to_string(), 110)
}

/// One event of the pane's log as lines `(tag, text)`. Events that are bookkeeping (init, thinking, partial streams) give no lines. Pure - tested.
fn render_event(ev: &Value) -> Vec<(String, String)> {
    let kind = ev["type"].as_str().unwrap_or("");
    let sub = ev["subtype"].as_str().unwrap_or("");
    let mut out: Vec<(String, String)> = vec![];
    match (kind, sub) {
        ("antiloki", "user") => out.push(("you".into(), ev["text"].as_str().unwrap_or("").trim().to_string())),
        ("antiloki", "role") | ("antiloki", "model") => out.push(("heimdall".into(), clip(ev["text"].as_str().or_else(|| ev["model"].as_str()).unwrap_or(""), 160))),
        ("antiloki", "note") => out.push(("note".into(), ev["text"].as_str().unwrap_or("").trim().to_string())),
        ("antiloki", "gates") => out.push(("gates".into(), clip(ev["text"].as_str().unwrap_or(""), 200))),
        ("antiloki", "audit") => {
            let mut s = format!("${:.4}", ev["cost"].as_f64().unwrap_or(0.0));
            if let Some(ms) = ev["ms"].as_i64() { s += &format!(" · {ms} ms"); }
            if let Some(sv) = ev["saved_usd"].as_f64().filter(|v| *v > 0.0) { s += &format!(" · saved ${sv:.4}"); }
            out.push(("cost".into(), s));
        }
        ("assistant", _) => {
            for b in ev["message"]["content"].as_array().cloned().unwrap_or_default() {
                match b["type"].as_str() {
                    Some("text") => { let t = b["text"].as_str().unwrap_or("").trim(); if !t.is_empty() { out.push(("agent".into(), t.to_string())); } }
                    Some("tool_use") => out.push(("tool".into(), format!("{} {}", b["name"].as_str().unwrap_or("?").replace("mcp__", "").replace("__", "·"), tool_summary(&b["input"])))),
                    _ => {}
                }
            }
        }
        ("user", _) => {
            for b in ev["message"]["content"].as_array().cloned().unwrap_or_default() {
                if b["type"] == "tool_result" {
                    let text = match &b["content"] { Value::String(s) => s.clone(), Value::Array(a) => a.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join(" "), o => o.to_string() };
                    out.push(("↳".into(), clip(&text, 140)));
                }
            }
        }
        ("result", _) => {
            if ev["is_error"] == json!(true) { out.push(("error".into(), clip(ev["result"].as_str().unwrap_or("the agent stopped with an error"), 200))); }
        }
        _ => {}
    }
    out
}

/// A goal run's state as one line, plus what it needs from you. Pure - tested.
fn render_run(run: &Value) -> Vec<(String, String)> {
    let id = run["id"].as_i64().unwrap_or(0);
    let status = run["status"].as_str().unwrap_or("?");
    let mut out = vec![("heimdall".to_string(), format!("run #{id} · {status}"))];
    match status {
        "awaiting-answers" => {
            for q in run["questions"].as_array().cloned().unwrap_or_default().iter().filter(|q| q["answer"].as_str().map_or(true, str::is_empty)) {
                let opts = q["options"].as_array().map(|o| o.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" / ")).unwrap_or_default();
                out.push(("asks".into(), format!("{}{}", q["text"].as_str().unwrap_or(""), if opts.is_empty() { String::new() } else { format!("  [{opts}]") })));
            }
            out.push(("asks".into(), "answer on the next line (or `go` to take the defaults)".into()));
        }
        "awaiting-approval" => {
            for t in run["tasks"].as_array().cloned().unwrap_or_default() { out.push(("plan".into(), clip(t["title"].as_str().unwrap_or(""), 160))); }
            out.push(("asks".into(), "`go` approves this plan, `cancel` drops it".into()));
        }
        "done" | "inline" => { if let Some(r) = run["report"].as_str().filter(|r| !r.trim().is_empty()) { out.push(("report".into(), r.trim().to_string())); } }
        "failed" | "cancelled" => { if let Some(e) = run["error"].as_str().filter(|e| !e.is_empty()) { out.push(("error".into(), clip(e, 200))); } }
        _ => {}
    }
    out
}

fn run_is_over(status: &str) -> bool { matches!(status, "done" | "failed" | "cancelled" | "inline") }
fn run_waits_for_you(status: &str) -> bool { matches!(status, "awaiting-answers" | "awaiting-approval") }

struct Out { json: bool, color: bool, t0: Instant }
impl Out {
    fn line(&self, tag: &str, text: &str) {
        let el = self.t0.elapsed().as_secs();
        let (dim, bold, off) = if self.color { ("\x1b[2m", "\x1b[1m", "\x1b[0m") } else { ("", "", "") };
        let mut lines = text.lines();
        if let Some(first) = lines.next() { println!("{dim}+{el:>3}s{off} {bold}{tag:<8}{off} {first}"); }
        for l in lines { println!("{:>10}{l}", ""); }
    }
}

// ---- one conversation ----

enum Outcome { Done, NeedsYou, Stopped }

struct Session { api: Api, chat: Chat, out: Out, last_seq: u64 }

impl Session {
    fn status(&self) -> (bool, bool) {
        let s = self.api.get(&format!("/api/v1/cockpit/agent/status?pane_id={}", self.chat.pane_id)).unwrap_or(Value::Null);
        (s["running"] == json!(true), s["busy"] == json!(true))
    }

    fn runs(&self) -> Vec<Value> {
        self.api.get("/api/v1/cockpit/goals").ok().and_then(|v| v["runs"].as_array().cloned()).unwrap_or_default().into_iter()
            .filter(|r| r["pane_id"].as_i64() == Some(self.chat.pane_id)).collect()
    }

    /// every run of this pane and its status NOW: what "changed since I sent" is measured against
    fn run_states(&self) -> std::collections::HashMap<i64, String> {
        self.runs().iter().filter_map(|r| Some((r["id"].as_i64()?, r["status"].as_str().unwrap_or("").to_string()))).collect()
    }

    /// Send one line the way the window's composer does. Returns what to follow.
    /// Returns whether the serve HANDLED the line itself (approved, answered, cancelled): the work it releases carries on a moment later.
    fn send(&self, line: &str) -> Result<bool, String> {
        let front = self.api.post("/api/v1/cockpit/agent/front", json!({ "pane_id": self.chat.pane_id, "text": line }), 90).unwrap_or(Value::Null);
        if front["handled"] == json!(true) { return Ok(true); }
        let kind = front["kind"].as_str().unwrap_or("");
        let ask = front["ask"].as_str().filter(|a| kind == "task" && !a.trim().is_empty()).unwrap_or(line);
        // a task or a question is Heimdall's; a pleasantry (or a front that did not answer, on a short line) is the agent's
        let route = kind == "task" || kind == "question" || (kind.is_empty() && line.contains(' ') && line.chars().count() >= 12);
        if route {
            let max = 4;
            let added = self.api.post("/api/v1/cockpit/queue/add", json!({ "parent": format!("{}-{}", self.chat.session_id, self.chat.pane_id), "goal": ask, "engine": "claude-code", "cwd": self.chat.worktree, "write": true, "n": max, "pane_id": self.chat.pane_id, "always": true }), 600);
            match added {
                Ok(v) if v["item"].is_object() => return Ok(false),
                Ok(v) => self.out.line("note", &format!("Heimdall did not take it ({}); sent to the agent as it was typed.", error_text(&v).unwrap_or_else(|| "no item".into()))),
                Err(e) => self.out.line("note", &format!("Heimdall did not route this one — {}. Sent to the agent as it was typed.", clip(&e, 160))),
            }
        }
        self.api.post("/api/v1/cockpit/agent/send", json!({ "pane_id": self.chat.pane_id, "text": line, "display": line }), 60).map(|_| false)
    }

    /// Print the log until the agent and Heimdall's run are idle, or the run waits for you.
    fn follow(&mut self, before: &std::collections::HashMap<i64, String>, expect_more: bool) -> Outcome {
        let mut shown: std::collections::HashMap<i64, String> = std::collections::HashMap::new();
        let mut quiet_since = Instant::now();
        let mut ever_busy = false;
        loop {
            let mut any = false;
            if let Ok(r) = self.api.get(&format!("/api/v1/cockpit/agent/events?pane_id={}&after={}&limit=200", self.chat.pane_id, self.last_seq)) {
                for e in r["events"].as_array().cloned().unwrap_or_default() {
                    self.last_seq = self.last_seq.max(e["seq"].as_u64().unwrap_or(0));
                    any = true;
                    if self.out.json { println!("{e}"); continue; }
                    for (tag, text) in render_event(&e["ev"]) { self.out.line(&tag, &text); }
                }
            }
            let mut waiting = false;
            let mut open = false;
            for run in self.runs() {
                let id = run["id"].as_i64().unwrap_or(0);
                let status = run["status"].as_str().unwrap_or("").to_string();
                // a run is ours to report when it is new, or when its state moved since the line was sent (an answered question carries on in the same run or a new one)
                if before.get(&id) == Some(&status) && !shown.contains_key(&id) { continue; }
                if shown.get(&id) != Some(&status) {
                    any = true;
                    if self.out.json { println!("{}", json!({ "run": run })); } else { for (tag, text) in render_run(&run) { self.out.line(&tag, &text); } }
                    shown.insert(id, status.clone());
                }
                if run_waits_for_you(&status) { waiting = true; }
                else if !run_is_over(&status) { open = true; }
            }
            if waiting { return Outcome::NeedsYou; }
            let (running, busy) = self.status();
            if busy || open { ever_busy = true; }
            if any || busy || open { quiet_since = Instant::now(); }
            // idle for a moment after something happened (or for a few seconds when nothing ever did) means the turn is over
            // after an answer or an approval the plan carries on in a NEW run a couple of seconds later: wait for it rather than call the turn over
            let patience = if ever_busy || !shown.is_empty() { Duration::from_millis(1500) } else if expect_more { Duration::from_secs(12) } else { Duration::from_secs(6) };
            if !busy && !open && quiet_since.elapsed() > patience { return if running || ever_busy { Outcome::Done } else { Outcome::Stopped }; }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

fn read_message_from_pipe() -> Option<String> {
    if std::io::stdin().is_terminal() { return None; }
    let mut s = String::new();
    std::io::stdin().lock().lines().map_while(Result::ok).for_each(|l| { s.push_str(&l); s.push('\n'); });
    let t = s.trim().to_string();
    if t.is_empty() { None } else { Some(t) }
}

fn run() -> Result<i32, String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = parse_args(&argv).map_err(|e| format!("{e}"))?;
    match args.cmd {
        Cmd::Help => { print!("{HELP}"); return Ok(0); }
        Cmd::Version => { println!("antiloki {VERSION}"); return Ok(0); }
        Cmd::Stop => { println!("{}", stop_serve()?); return Ok(0); }
        Cmd::App => {
            let _ = stop_serve();   // the window runs its own serve on the same port
            let bin = find_program("antiloki-desktop", "ANTILOKI_DESKTOP_BIN").ok_or("antiloki-desktop was not found next to this program or on PATH.")?;
            std::process::Command::new(&bin).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().map_err(|e| format!("could not open the window: {e}"))?;
            return Ok(0);
        }
        Cmd::Doctor => {
            let bin = find_program("antiloki-serve", "ANTILOKI_SERVE_BIN").ok_or("antiloki-serve was not found next to this program or on PATH.")?;
            let dir = args.workspace.clone().unwrap_or_else(|| ".".into());
            return Ok(std::process::Command::new(bin).arg("doctor").arg(dir).status().map(|s| s.code().unwrap_or(1)).unwrap_or(1));
        }
        Cmd::Chat => {}
    }
    let dir = PathBuf::from(args.workspace.clone().unwrap_or_else(|| ".".into()));
    let dir = dir.canonicalize().map_err(|e| format!("{}: {e}", dir.display()))?;
    let root = repo_root(&dir)?;
    let api = Api { base: std::env::var("ANTILOKI_BE_URL").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| DEFAULT_URL.into()).trim_end_matches('/').to_string() };
    if !api.healthy() { start_serve(&api)?; }
    let name = ensure_workspace(&api, &root.to_string_lossy())?;
    let chat = open_chat(&api, &name, args.new_chat)?;
    let color = args.color.unwrap_or_else(|| std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none());
    let out = Out { json: args.json, color, t0: Instant::now() };
    // READING THE LOG ONCE IS HOW THE SERVE LEARNS WHERE IT IS (the window does it when a pane mounts): an event emitted for a pane nobody has read
    // yet - the first "you" of a brand-new chat - has nowhere to land. So read first, and start after what is already there.
    let last_seq = api.get(&format!("/api/v1/cockpit/agent/events?pane_id={}&after=0&limit=1", chat.pane_id)).ok().and_then(|s| s["last_seq"].as_u64()).unwrap_or(0);
    let mut s = Session { api, chat, out, last_seq };
    // a pipe is ONE ask (like `claude -p`) unless -i asks for a conversation; an ask on the command line is one too, unless -i
    let message = if args.interactive { args.message.clone() } else { args.message.clone().or_else(read_message_from_pipe) };
    let one_shot = message.is_some() && !args.interactive;
    if !s.out.json { eprintln!("antiloki · {} · {}{}", name, root.display(), if one_shot { "" } else { " · /exit leaves, /help lists the commands" }); }
    let mut exit = 0;
    let mut pending = message;
    let stdin = std::io::stdin();
    loop {
        let line = match pending.take() {
            Some(m) => m,
            None => {
                if one_shot { break; }
                print!("antiloki> "); let _ = std::io::stdout().flush();
                let mut buf = String::new();
                if stdin.lock().read_line(&mut buf).map(|n| n == 0).unwrap_or(true) { break; }
                buf.trim().to_string()
            }
        };
        if line.is_empty() { continue; }
        match line.as_str() {
            "/exit" | "/quit" | "exit" | "quit" => break,
            "/help" => { println!("/new  start a new conversation\n/stop stop the agent\n/exit leave (the work stays, and is in the window)"); continue; }
            "/stop" => { let _ = s.api.post("/api/v1/cockpit/agent/stop", json!({ "pane_id": s.chat.pane_id }), 30); println!("stopped."); continue; }
            "/new" => { match open_chat(&s.api, &name, true) { Ok(c) => { s.chat = c; s.last_seq = s.api.get(&format!("/api/v1/cockpit/agent/events?pane_id={}&after=0&limit=1", s.chat.pane_id)).ok().and_then(|v| v["last_seq"].as_u64()).unwrap_or(0); println!("a new conversation."); } Err(e) => eprintln!("{e}") } continue; }
            _ => {}
        }
        let before = s.run_states();
        s.out.t0 = Instant::now();
        let handled = match s.send(&line) { Ok(h) => h, Err(e) => { eprintln!("antiloki: {e}"); exit = 1; if one_shot { break; } continue; } };
        match s.follow(&before, handled) {
            Outcome::NeedsYou => { exit = 3; if one_shot { eprintln!("antiloki: the agent is waiting for your answer — run `antiloki` and reply, or answer in the window."); break; } }
            Outcome::Stopped => { s.out.line("note", "no answer came back (is a seat signed in? `antiloki doctor` says)."); exit = 1; if one_shot { break; } }
            Outcome::Done => { exit = 0; }
        }
    }
    Ok(exit)
}

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => { eprintln!("antiloki: {e}"); std::process::exit(if e.starts_with("unknown option") || e.contains("needs a folder") { 2 } else { 1 }); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn a(s: &[&str]) -> Vec<String> { s.iter().map(|x| x.to_string()).collect() }

    #[test]
    fn arguments_pick_the_command_the_folder_and_the_ask() {
        assert_eq!(parse_args(&a(&[])).unwrap().cmd, Cmd::Chat);
        let x = parse_args(&a(&["-C", "../p", "add", "a", "retry", "--json"])).unwrap();
        assert_eq!((x.workspace.as_deref(), x.message.as_deref(), x.json), (Some("../p"), Some("add a retry"), true));
        assert_eq!(parse_args(&a(&["app"])).unwrap().cmd, Cmd::App);
        assert_eq!(parse_args(&a(&["stop"])).unwrap().cmd, Cmd::Stop);
        assert_eq!(parse_args(&a(&["--version"])).unwrap().cmd, Cmd::Version);
        // a word that is also a subcommand is part of an ask once the ask has begun
        assert_eq!(parse_args(&a(&["explain", "the", "app"])).unwrap(), Args { cmd: Cmd::Chat, workspace: None, json: false, color: None, new_chat: false, interactive: false, message: Some("explain the app".into()) });
        assert!(parse_args(&a(&["--bogus"])).unwrap_err().starts_with("unknown option"));
        assert!(parse_args(&a(&["-C"])).unwrap_err().contains("needs a folder"));
        assert_eq!(parse_args(&a(&["--", "--json"])).unwrap().message.as_deref(), Some("--json"), "after -- everything is the ask");
        assert!(parse_args(&a(&["-i"])).unwrap().interactive);
    }

    #[test]
    fn a_folder_keeps_its_registry_name_or_gets_a_unique_one() {
        let ex = vec![("shop".to_string(), "/w/shop".to_string()), ("api".to_string(), "/w/other/api".to_string())];
        assert_eq!(workspace_name(&ex, "/w/shop"), ("shop".into(), true));
        assert_eq!(workspace_name(&ex, "/w/new"), ("new".into(), false));
        assert_eq!(workspace_name(&ex, "/elsewhere/api"), ("api-2".into(), false), "the same folder name under another path does not take over the first");
        assert_eq!(workspace_name(&[], "/"), ("workspace".into(), false));
    }

    #[test]
    fn programs_are_looked_for_in_the_override_then_beside_this_one_then_on_path() {
        let c = candidates("antiloki-serve", Some("/x/serve".into()), Some(PathBuf::from("/opt/antiloki")), Some("/usr/bin:/home/u/bin".into()));
        assert_eq!(c, vec![PathBuf::from("/x/serve"), PathBuf::from("/opt/antiloki/antiloki-serve"), PathBuf::from("/usr/bin/antiloki-serve"), PathBuf::from("/home/u/bin/antiloki-serve")]);
        assert_eq!(candidates("p", Some("  ".into()), None, None), Vec::<PathBuf>::new());
        assert_eq!(port_of("http://127.0.0.1:41931/"), "41931");
    }

    #[test]
    fn the_pane_log_reads_as_lines_of_text() {
        let ev = |v: Value| render_event(&v);
        assert_eq!(ev(json!({ "type": "antiloki", "subtype": "user", "text": "hello " })), vec![("you".to_string(), "hello".to_string())]);
        assert_eq!(ev(json!({ "type": "antiloki", "subtype": "audit", "cost": 0.0006, "ms": 12, "saved_usd": 0.0004 })), vec![("cost".to_string(), "$0.0006 · 12 ms · saved $0.0004".to_string())]);
        let msg = ev(json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "ok\nthen" }, { "type": "tool_use", "name": "mcp__antiqa__antiqa_run_start", "input": { "product": "shop" } }] } }));
        assert_eq!(msg[0], ("agent".to_string(), "ok\nthen".to_string()));
        assert_eq!(msg[1].0, "tool");
        assert!(msg[1].1.starts_with("antiqa·antiqa_run_start"), "{:?}", msg[1]);
        let res = ev(json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "content": [{ "text": "started run  2026-10-26" }] }] } }));
        assert_eq!(res, vec![("↳".to_string(), "started run 2026-10-26".to_string())]);
        assert!(ev(json!({ "type": "system", "subtype": "init" })).is_empty(), "bookkeeping prints nothing");
        assert!(ev(json!({ "type": "result", "is_error": false, "result": "x" })).is_empty(), "a good result repeats the answer, so it says nothing");
        assert_eq!(ev(json!({ "type": "result", "is_error": true, "result": "boom" })), vec![("error".to_string(), "boom".to_string())]);
    }

    #[test]
    fn a_run_says_what_it_needs_from_you_and_when_it_is_over() {
        let q = render_run(&json!({ "id": 3, "status": "awaiting-answers", "questions": [{ "text": "Which format?", "options": ["json", "csv"], "answer": "" }, { "text": "answered", "answer": "x" }] }));
        assert_eq!(q[0].1, "run #3 · awaiting-answers");
        assert_eq!(q[1], ("asks".to_string(), "Which format?  [json / csv]".to_string()));
        assert!(q.last().unwrap().1.contains("`go`"), "the way out is stated");
        let p = render_run(&json!({ "id": 4, "status": "awaiting-approval", "tasks": [{ "title": "add the retry" }] }));
        assert_eq!(p[1], ("plan".to_string(), "add the retry".to_string()));
        assert_eq!(render_run(&json!({ "id": 5, "status": "done", "report": "REPORT: created out/n.txt." }))[1], ("report".to_string(), "REPORT: created out/n.txt.".to_string()));
        assert!(run_waits_for_you("awaiting-approval") && !run_waits_for_you("running"));
        assert!(run_is_over("failed") && !run_is_over("planning") && !run_is_over("awaiting-answers"));
    }

    #[test]
    fn a_server_error_is_read_from_either_shape() {
        assert_eq!(error_text(&json!({ "error": "no pane" })).as_deref(), Some("no pane"));
        assert_eq!(error_text(&json!({ "error": { "message": "bad" } })).as_deref(), Some("bad"));
        assert_eq!(error_text(&json!({ "ok": true })), None);
    }

    #[test]
    fn clipping_collapses_whitespace_and_marks_the_cut() {
        assert_eq!(clip("a   b\n c", 10), "a b c");
        assert_eq!(clip("abcdefghij", 4), "abcd…");
    }
}
