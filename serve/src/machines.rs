//! OTHER MACHINES, OVER SSH, ONE CLICK (operator 2026-11-10, asked twice: "add a machine, SSH ... one click, the address generated on the other machine and synced").
//!
//! A machine is a computer the person can SSH into without a password (keys or an agent: nothing here asks for one). `connect` does everything in one go and says each step:
//!   1. looks (`uname`, `$HOME`, whether antiloki is already there),
//!   2. puts antiloki there when it is not (this machine's `antiloki-serve`, copied with scp, when both are the same kind of system; the built app's files too, so the other machine has a page to show),
//!   3. starts it there, bound to that machine's own loopback (nothing is opened to anyone),
//!   4. keeps an SSH tunnel from a local port to it, and
//!   5. waits until the other antiloki answers through the tunnel. The address it is reached by (`http://127.0.0.1:<local port>`) is what is stored and synced here; "Open" shows its screen.
//! The SSH user is the other machine's owner, so through the tunnel the other serve sees a local request: no token is needed, and none is stored. Nothing is run on the other machine but what is written below.
use axum::{extract::Request, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const REMOTE_PORT: u16 = 41941;
const REMOTE_BIN: &str = ".antiloki/remote-bin/antiloki-serve";
const REMOTE_FE: &str = ".antiloki/remote-fe";

// ── pure ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

/// `user@host`, `host`, `user@host:2222`, `[::1]:22` → (user, host, port). Letters, digits, `.`, `-`, `_` only (a name that begins with `-` would be an option to ssh). Pure - tested.
pub fn parse_target(t: &str) -> Result<(Option<String>, String, u16), String> {
    let t = t.trim(); if t.is_empty() { return Err("write where to connect: user@host".into()); }
    let (user, rest) = match t.split_once('@') { Some((u, r)) => (Some(u), r), None => (None, t) };
    let ok = |s: &str| !s.is_empty() && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || ".-_".contains(c));
    if let Some(u) = user { if !ok(u) { return Err("the user name has a character ssh would take for something else".into()); } }
    let (host, port) = match rest.rsplit_once(':') { Some((h, p)) if !h.contains(':') => (h, p.parse::<u16>().map_err(|_| "the port is a number".to_string())?), _ => (rest, 22) };
    if !ok(host) { return Err("the host is a name or an address (letters, digits, dots and dashes)".into()); }
    Ok((user.map(String::from), host.to_string(), port))
}
fn target_of(user: &Option<String>, host: &str) -> String { match user { Some(u) => format!("{u}@{host}"), None => host.to_string() } }

/// The ssh options every call carries: never ask anything (keys or an agent or nothing), a short wait, a first host key accepted and remembered, and a tunnel that notices a dead line.
pub fn base_opts(ssh_port: u16) -> Vec<String> {
    ["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "StrictHostKeyChecking=accept-new", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3", "-p"].iter().map(|s| s.to_string()).chain(std::iter::once(ssh_port.to_string())).collect()
}
/// The one command run on the other machine to look: its system, its home, and whether antiloki is there.
pub fn probe_cmd() -> String { format!("echo \"os:$(uname -sm)\"; echo \"home:$HOME\"; if test -x \"$HOME/{REMOTE_BIN}\"; then echo have:bin; elif command -v antiloki-serve >/dev/null 2>&1; then echo have:path; else echo have:none; fi; if test -f \"$HOME/{REMOTE_FE}/index.html\"; then echo app:yes; else echo app:no; fi") }
/// What the probe said. Pure.
#[derive(Debug, PartialEq, Default, Clone)]
pub struct Probe { pub os: String, pub home: String, pub have: String, pub app: bool }
pub fn parse_probe(out: &str) -> Probe {
    let mut p = Probe::default();
    for l in out.lines() { if let Some(v) = l.strip_prefix("os:") { p.os = v.trim().to_string(); } else if let Some(v) = l.strip_prefix("home:") { p.home = v.trim().to_string(); } else if let Some(v) = l.strip_prefix("have:") { p.have = v.trim().to_string(); } else if let Some(v) = l.strip_prefix("app:") { p.app = v.trim() == "yes"; } }
    p
}
/// The command that starts antiloki on the other machine, once, bound to its own loopback, detached from the ssh line. Only numbers and constants go in.
pub fn start_cmd(have_path: bool, remote_port: u16) -> String {
    let bin = if have_path { "antiloki-serve".to_string() } else { format!("\"$HOME/{REMOTE_BIN}\"") };
    format!("mkdir -p \"$HOME/.antiloki\"; if [ -f \"$HOME/.antiloki/remote-serve.pid\" ] && kill -0 \"$(cat \"$HOME/.antiloki/remote-serve.pid\")\" 2>/dev/null; then echo running; else if [ -d \"$HOME/{REMOTE_FE}\" ]; then export ANTILOKI_FE_DIR=\"$HOME/{REMOTE_FE}\"; fi; ANTILOKI_SERVE_PORT={remote_port} nohup {bin} >\"$HOME/.antiloki/remote-serve.log\" 2>&1 </dev/null & echo $! > \"$HOME/.antiloki/remote-serve.pid\"; echo started; fi")
}
/// `ssh -N -L <local>:127.0.0.1:<remote> target`, as arguments.
pub fn tunnel_args(ssh_port: u16, target: &str, local: u16, remote: u16) -> Vec<String> { let mut a = base_opts(ssh_port); a.extend(["-o".into(), "ExitOnForwardFailure=yes".into(), "-N".into(), "-L".into(), format!("{local}:127.0.0.1:{remote}"), target.to_string()]); a }
/// Can this machine's serve be copied to that one: the same kind of system (`Linux x86_64` = `Linux x86_64`). Pure.
pub fn same_platform(local: &str, remote: &str) -> bool { !local.trim().is_empty() && local.trim().eq_ignore_ascii_case(remote.trim()) }
/// What a failed ssh meant, in a sentence the person can act on. Pure.
pub fn ssh_hint(stderr: &str, target: &str) -> String {
    let e = stderr.to_lowercase();
    if e.contains("permission denied") { format!("ssh could not sign in without a password. Set up a key (ssh-copy-id {target}) or start ssh-agent, and try again") }
    else if e.contains("could not resolve") { "the name does not resolve: check the host".into() }
    else if e.contains("refused") { "the machine refused the connection: is ssh running there, and is the port right?".into() }
    else if e.contains("timed out") || e.contains("no route") || e.contains("unreachable") { "the machine did not answer in time: is it on, and reachable from here?".into() }
    else if e.contains("host key verification failed") || e.contains("remote host identification has changed") { "the machine's host key changed: if you expected that, remove the old line from ~/.ssh/known_hosts".into() }
    else { let t = stderr.trim(); if t.is_empty() { "ssh failed".into() } else { format!("ssh said: {}", t.lines().last().unwrap_or("").chars().take(200).collect::<String>()) } }
}

// ── the record ──────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Machine { pub id: String, pub name: String, pub user: Option<String>, pub host: String, pub ssh_port: u16, pub remote_port: u16, pub local_port: u16, pub connected: bool }
impl Machine {
    fn target(&self) -> String { target_of(&self.user, &self.host) }
    fn to_json(&self, up: bool) -> Value { json!({ "id": self.id, "name": self.name, "target": self.target(), "host": self.host, "ssh_port": self.ssh_port, "local_port": self.local_port, "url": format!("http://127.0.0.1:{}", self.local_port), "connected": self.connected, "up": up }) }
}
fn file() -> PathBuf { crate::remote::dir().join("machines.json") }
fn load() -> Vec<Machine> {
    let v: Value = std::fs::read_to_string(file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    v.as_array().into_iter().flatten().filter_map(|m| Some(Machine { id: m["id"].as_str()?.into(), name: m["name"].as_str()?.into(), user: m["user"].as_str().map(String::from), host: m["host"].as_str()?.into(), ssh_port: m["ssh_port"].as_u64().unwrap_or(22) as u16, remote_port: m["remote_port"].as_u64().unwrap_or(REMOTE_PORT as u64) as u16, local_port: m["local_port"].as_u64()? as u16, connected: m["connected"].as_bool().unwrap_or(false) })).collect()
}
fn save(ms: &[Machine]) {
    let p = file(); if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    let v: Vec<Value> = ms.iter().map(|m| json!({ "id": m.id, "name": m.name, "user": m.user, "host": m.host, "ssh_port": m.ssh_port, "remote_port": m.remote_port, "local_port": m.local_port, "connected": m.connected })).collect();
    let tmp = p.with_extension("tmp"); if std::fs::write(&tmp, Value::Array(v).to_string()).is_ok() { let _ = std::fs::rename(&tmp, &p); }
}
fn tunnels() -> &'static Mutex<HashMap<String, tokio::process::Child>> { static T: OnceLock<Mutex<HashMap<String, tokio::process::Child>>> = OnceLock::new(); T.get_or_init(Default::default) }
fn free_port() -> u16 { std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).unwrap_or(0) }
fn slug(s: &str) -> String { let t: String = s.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect(); t.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-") }

// ── doing it ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

async fn run(bin: &str, args: &[String], stdin: Option<&str>) -> Result<(String, String, bool), String> {
    use tokio::io::AsyncWriteExt;
    let mut c = crate::proc::tokio_cmd(bin); c.args(args).stdin(if stdin.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() }).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    let mut child = c.spawn().map_err(|e| format!("could not run {bin}: {e}"))?;
    if let (Some(text), Some(mut si)) = (stdin, child.stdin.take()) { let _ = si.write_all(text.as_bytes()).await; }
    let out = tokio::time::timeout(std::time::Duration::from_secs(120), child.wait_with_output()).await.map_err(|_| format!("{bin} took too long"))?.map_err(|e| e.to_string())?;
    Ok((String::from_utf8_lossy(&out.stdout).to_string(), String::from_utf8_lossy(&out.stderr).to_string(), out.status.success()))
}
async fn ssh(m: &Machine, cmd: &str) -> Result<String, String> {
    let mut a = base_opts(m.ssh_port); a.push(m.target()); a.push(cmd.to_string());
    let (out, err, ok) = run("ssh", &a, None).await?; if ok { Ok(out) } else { Err(ssh_hint(&err, &m.target())) }
}
async fn scp(m: &Machine, from: &Path, to_rel: &str, recursive: bool) -> Result<(), String> {
    let mut a: Vec<String> = base_opts(m.ssh_port).into_iter().map(|x| if x == "-p" { "-P".to_string() } else { x }).collect(); if recursive { a.push("-r".into()); }
    a.push(from.to_string_lossy().to_string()); a.push(format!("{}:{}", m.target(), to_rel));
    let (_, err, ok) = run("scp", &a, None).await?; if ok { Ok(()) } else { Err(ssh_hint(&err, &m.target())) }
}
/// This machine's own `antiloki-serve` file, to copy: named by `ANTILOKI_SERVE_BIN`, else beside this program, else on the PATH.
fn serve_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ANTILOKI_SERVE_BIN") { let p = PathBuf::from(p); if p.is_file() { return Some(p); } }
    if let Ok(me) = std::env::current_exe() { if let Some(d) = me.parent() { let p = d.join("antiloki-serve"); if p.is_file() { return Some(p); } } }
    std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("antiloki-serve")).find(|f| f.is_file()))
}
async fn pings(port: u16) -> bool { reqwest::Client::new().get(format!("http://127.0.0.1:{port}/api/v1/remote/ping")).timeout(std::time::Duration::from_secs(3)).send().await.map(|r| r.status().is_success()).unwrap_or(false) }
async fn spawn_tunnel(m: &Machine) -> Result<(), String> {
    let child = crate::proc::tokio_cmd("ssh").args(tunnel_args(m.ssh_port, &m.target(), m.local_port, m.remote_port)).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true).spawn().map_err(|e| format!("could not run ssh: {e}"))?;
    if let Some(mut old) = tunnels().lock().unwrap_or_else(|p| p.into_inner()).insert(m.id.clone(), child) { let _ = old.start_kill(); }
    Ok(())
}

/// Connect: look, install if needed, start, tunnel, wait. Returns the steps said, or the step that failed.
pub async fn connect(id: &str) -> Result<Vec<String>, (Vec<String>, String)> {
    let mut log: Vec<String> = vec![];
    macro_rules! fail { ($e:expr) => { return Err((log, $e)) }; }
    let mut ms = load(); let Some(i) = ms.iter().position(|m| m.id == id) else { fail!("no such machine".to_string()) }; let m = ms[i].clone();
    let probe = match ssh(&m, &probe_cmd()).await { Ok(o) => parse_probe(&o), Err(e) => fail!(e) };
    log.push(format!("reached {} ({})", m.target(), if probe.os.is_empty() { "unknown system" } else { &probe.os }));
    let have_path = probe.have == "have:path" || probe.have == "path";
    if probe.have == "none" {
        let Some(bin) = serve_binary() else { fail!("antiloki is not on that machine, and this one has no antiloki-serve file to copy (set ANTILOKI_SERVE_BIN, or install antiloki there)".to_string()) };
        let local_os = run("uname", &["-sm".to_string()], None).await.map(|r| r.0).unwrap_or_default();
        if !same_platform(&local_os, &probe.os) { fail!(format!("that machine is a different kind of system ({}), so this machine's antiloki cannot run there. Install antiloki there and connect again", probe.os)); }
        if let Err(e) = ssh(&m, &format!("mkdir -p \"$HOME/{}\"", Path::new(REMOTE_BIN).parent().unwrap().display())).await { fail!(e) }
        if let Err(e) = scp(&m, &bin, REMOTE_BIN, false).await { fail!(e) }
        if let Err(e) = ssh(&m, &format!("chmod +x \"$HOME/{REMOTE_BIN}\"")).await { fail!(e) }
        log.push("put antiloki on that machine".into());
    } else { log.push("antiloki is already there".into()); }
    if !probe.app { if let Some(fe) = crate::remote::fe_dir() {
        if ssh(&m, &format!("mkdir -p \"$HOME/{REMOTE_FE}\"")).await.is_ok() { match scp(&m, &fe.join("."), &format!("{REMOTE_FE}/"), true).await { Ok(()) => log.push("put the app's files there, so it has a screen".into()), Err(e) => log.push(format!("could not put the app's files there ({e}); its screen will be missing")) } }
    } }
    if let Err(e) = spawn_tunnel(&m).await { fail!(e) }
    // the line takes a moment to come up: give a machine that is already running its chance to answer before starting anything
    let mut answered = false; for _ in 0..10 { if pings(m.local_port).await { answered = true; break; } tokio::time::sleep(std::time::Duration::from_millis(400)).await; }
    if !answered {
        match ssh(&m, &start_cmd(have_path, m.remote_port)).await { Ok(_) => log.push("started antiloki there".into()), Err(e) => fail!(e) }
        let _ = spawn_tunnel(&m).await;   // the first tunnel may have been refused while nothing listened there yet
        let mut up = false; for _ in 0..40 { tokio::time::sleep(std::time::Duration::from_millis(500)).await; if pings(m.local_port).await { up = true; break; } }
        if !up { fail!("antiloki was started there but did not answer through the tunnel in 20 seconds (see ~/.antiloki/remote-serve.log on that machine)".to_string()) }
    }
    log.push(format!("connected: its screen is at http://127.0.0.1:{}", m.local_port));
    ms[i].connected = true; save(&ms);
    Ok(log)
}
pub async fn disconnect(id: &str, stop_remote: bool) -> Result<(), String> {
    let mut ms = load(); let i = ms.iter().position(|m| m.id == id).ok_or("no such machine")?;
    if stop_remote { let m = ms[i].clone(); let _ = ssh(&m, "if [ -f \"$HOME/.antiloki/remote-serve.pid\" ]; then kill $(cat \"$HOME/.antiloki/remote-serve.pid\") >/dev/null 2>&1; rm -f \"$HOME/.antiloki/remote-serve.pid\"; fi; true").await; }
    if let Some(mut c) = tunnels().lock().unwrap_or_else(|p| p.into_inner()).remove(id) { let _ = c.start_kill(); }
    ms[i].connected = false; save(&ms); Ok(())
}
/// Bring back the tunnels of machines that were connected when this serve last stopped (nothing is installed or started: only the line).
pub fn restore() { tokio::spawn(async { for m in load().into_iter().filter(|m| m.connected) { let _ = spawn_tunnel(&m).await; } }); }

// ── HTTP ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

fn reply(v: Value) -> Response { (StatusCode::OK, Json(v)).into_response() }
async fn listing() -> Value {
    let ms = load(); let mut out = vec![]; for m in &ms { let up = m.connected && pings(m.local_port).await; out.push(m.to_json(up)); }
    json!({ "ok": true, "machines": out })
}
pub async fn list(req: Request) -> Response { if let Some(r) = crate::remote::local_only(&req) { return r; } reply(listing().await) }
pub async fn add(req: Request) -> Response {
    if let Some(r) = crate::remote::local_only(&req) { return r; }
    let body = axum::body::to_bytes(req.into_body(), 1 << 16).await.unwrap_or_default(); let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let (user, host, port) = match parse_target(b["target"].as_str().unwrap_or("")) { Ok(t) => t, Err(e) => return reply(json!({ "ok": false, "error": e })) };
    let name = { let n = b["name"].as_str().unwrap_or("").trim(); if n.is_empty() { host.clone() } else { n.chars().take(60).collect::<String>() } };
    let mut ms = load(); let id = slug(&name); let id = if id.is_empty() { slug(&host) } else { id };
    if ms.iter().any(|m| m.id == id) { return reply(json!({ "ok": false, "error": format!("a machine called `{name}` is already here") })); }
    ms.push(Machine { id: id.clone(), name, user, host, ssh_port: port, remote_port: b["remote_port"].as_u64().filter(|p| (1024..65536).contains(p)).map(|p| p as u16).unwrap_or(REMOTE_PORT), local_port: free_port(), connected: false }); save(&ms);
    reply(json!({ "ok": true, "id": id, "machines": listing().await["machines"] }))
}
pub async fn connect_http(req: Request) -> Response {
    if let Some(r) = crate::remote::local_only(&req) { return r; }
    let body = axum::body::to_bytes(req.into_body(), 1 << 16).await.unwrap_or_default(); let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    match connect(b["id"].as_str().unwrap_or("")).await { Ok(log) => reply(json!({ "ok": true, "steps": log, "machines": listing().await["machines"] })), Err((log, e)) => reply(json!({ "ok": false, "error": e, "steps": log })) }
}
pub async fn disconnect_http(req: Request) -> Response {
    if let Some(r) = crate::remote::local_only(&req) { return r; }
    let body = axum::body::to_bytes(req.into_body(), 1 << 16).await.unwrap_or_default(); let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    match disconnect(b["id"].as_str().unwrap_or(""), b["stop_remote"] == json!(true)).await { Ok(()) => reply(json!({ "ok": true, "machines": listing().await["machines"] })), Err(e) => reply(json!({ "ok": false, "error": e })) }
}
pub async fn remove(req: Request) -> Response {
    if let Some(r) = crate::remote::local_only(&req) { return r; }
    let body = axum::body::to_bytes(req.into_body(), 1 << 16).await.unwrap_or_default(); let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); let id = b["id"].as_str().unwrap_or("").to_string();
    let _ = disconnect(&id, false).await; let mut ms = load(); let n = ms.len(); ms.retain(|m| m.id != id); if ms.len() == n { return reply(json!({ "ok": false, "error": "no such machine" })); }
    save(&ms); reply(json!({ "ok": true, "machines": listing().await["machines"] }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_target_is_user_host_and_port_and_nothing_ssh_could_read_as_an_option() {
        assert_eq!(parse_target("dani@build.local").unwrap(), (Some("dani".into()), "build.local".into(), 22)); assert_eq!(parse_target("10.0.0.5:2222").unwrap(), (None, "10.0.0.5".into(), 2222)); assert_eq!(parse_target("  u@h  ").unwrap(), (Some("u".into()), "h".into(), 22));
        for bad in ["", "-oProxyCommand=x", "u@-h", "-u@h", "u@h;rm", "u@h x", "u@h:notaport", "u@h:99999", "a@b@c", "$(id)@h", "u@h/../x"] { assert!(parse_target(bad).is_err(), "{bad}"); }
    }
    #[test]
    fn ssh_is_told_never_to_ask_and_the_tunnel_to_fail_loudly() {
        let o = base_opts(2222).join(" "); assert!(o.contains("BatchMode=yes") && o.contains("ConnectTimeout=10") && o.contains("accept-new") && o.ends_with("-p 2222"), "{o}");
        let t = tunnel_args(22, "u@h", 41999, 41941).join(" "); assert!(t.contains("-N") && t.contains("-L 41999:127.0.0.1:41941") && t.contains("ExitOnForwardFailure=yes") && t.ends_with("u@h"), "{t}");
    }
    #[test]
    fn the_probe_and_the_start_command_say_only_what_they_must() {
        let p = parse_probe("os:Linux x86_64\nhome:/home/dani\nhave:none\napp:no\n"); assert_eq!(p, Probe { os: "Linux x86_64".into(), home: "/home/dani".into(), have: "none".into(), app: false });
        assert_eq!(parse_probe("have:bin\napp:yes").app, true); assert_eq!(parse_probe("junk"), Probe::default());
        let s = start_cmd(false, 41941); assert!(s.contains("kill -0") && s.contains("echo running") && s.contains("ANTILOKI_SERVE_PORT=41941") && s.contains("nohup") && s.contains(REMOTE_BIN) && s.contains("</dev/null &") && s.contains("remote-serve.pid"), "{s}"); assert!(start_cmd(true, 41941).contains("nohup antiloki-serve"));
        assert!(same_platform("Linux x86_64\n", "linux x86_64") && !same_platform("Linux x86_64", "Darwin arm64") && !same_platform("", ""));
    }
    #[test]
    fn a_failure_of_ssh_is_said_in_words_the_person_can_act_on() {
        assert!(ssh_hint("u@h: Permission denied (publickey,password).", "u@h").contains("ssh-copy-id u@h")); assert!(ssh_hint("ssh: Could not resolve hostname nope", "h").contains("does not resolve")); assert!(ssh_hint("connect to host h port 22: Connection refused", "h").contains("refused"));
        assert!(ssh_hint("ssh: connect to host h port 22: Connection timed out", "h").contains("did not answer")); assert!(ssh_hint("Host key verification failed.", "h").contains("known_hosts")); assert_eq!(ssh_hint("", "h"), "ssh failed"); assert!(ssh_hint("weird\nlast line", "h").contains("last line"));
    }
}
