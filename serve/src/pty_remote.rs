//! THE SIDECAR HOST AS THE OWNER OF A PANE'S TERMINAL (RFC 0007, milestone M1).
//!
//! `pty.rs` used to keep every PTY inside this process, so a serve restart ended every agent terminal
//! (`OPERATING-RULES.md` §2). With `ANTILOKI_PTY_HOST=sidecar` the PTYs belong to `antiloki-agent pty-host` instead: the
//! serve asks the host to spawn, attaches to read output and write keystrokes, and on the next start it finds the
//! terminals still alive and attaches again. The default stays the in-process registry for one release (RFC 0007 §8).
//!
//! The host is addressed by terminal id `pane-<pane id>`; anything else in the host (a terminal someone started with
//! `antiloki-agent pty spawn`) is none of the serve's business and is left alone.
//!
//! Every call here is blocking and short, except starting the host, which may wait for it to come up; call sites that run
//! on the async runtime use `spawn_blocking` for that.
use serde_json::{json, Value};
use std::collections::HashMap;

/// One terminal the host knows, as `list` reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct HostTerm { pub id: String, pub alive: bool, pub exit: Option<i64>, pub argv: Vec<String>, pub rows: u16, pub cols: u16, pub clients: u64 }

/// What to run, already resolved by `pty::start` (shell, args, cwd, the environment the caller wants on top of the serve's own).
pub struct Spawn { pub shell: String, pub argv: Vec<String>, pub cwd: String, pub env: HashMap<String, String>, pub rows: u16, pub cols: u16 }

pub fn term_id(pane_id: i64) -> String { format!("pane-{pane_id}") }
pub fn pane_of(term_id: &str) -> Option<i64> { term_id.strip_prefix("pane-")?.parse().ok() }
/// The host is on only when asked for, and only where it exists (Unix; the Windows transport is RFC 0007 M7).
pub fn enabled() -> bool { cfg!(unix) && std::env::var("ANTILOKI_PTY_HOST").map(|v| v.trim().eq_ignore_ascii_case("sidecar")).unwrap_or(false) }

/// The variables a pane's CLI must not inherit from whatever started the serve: it would think it is a nested child.
pub const SCRUBBED_ENV: [&str; 3] = ["CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_ENTRYPOINT"];

/// The environment the terminal gets: the serve's own, plus the caller's, minus the scrubbed markers. The host starts it from
/// nothing and uses exactly this, so the terminal does not depend on the environment the host was first started with.
pub fn terminal_env(extra: &HashMap<String, String>) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    env.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
    for k in SCRUBBED_ENV { env.remove(k); }
    env
}

#[cfg(unix)]
mod imp {
    use super::*;
    use antiloki_ptyhost::{connect_or_start_with, sock_path, Client};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn sock() -> PathBuf { sock_path(None) }
    fn host_exe() -> PathBuf { PathBuf::from(antiloki_rs::cockpit_cockpit_pane_context_bake::agent_bin()) }
    /// A connection to the host, starting it when nobody answers.
    fn client() -> Result<Client, String> {
        connect_or_start_with(&host_exe(), &sock()).map_err(|e| format!("the terminal host is not reachable ({e}); antiloki-agent must sit next to the serve, or ANTILOKI_AGENT_BIN must name it"))
    }
    /// A connection only if the host is already running; never starts one.
    fn client_if_running() -> Option<Client> { Client::connect(&sock()).ok() }
    fn ok(v: &Value) -> Result<(), String> { if v.get("ok") == Some(&Value::Bool(true)) { Ok(()) } else { Err(v.get("error").and_then(Value::as_str).unwrap_or("the host refused").to_string()) } }

    /// Start (or replace) pane `pane_id`'s terminal in the host and attach to it. The returned stream is the terminal: the first
    /// bytes are its screen, then live output; writing to it types.
    pub fn spawn_and_attach(pane_id: i64, sp: &Spawn) -> Result<UnixStream, String> {
        let id = term_id(pane_id);
        let mut argv = vec![sp.shell.clone()];
        argv.extend(sp.argv.iter().cloned());
        let mut c = client()?;
        let r = c.request(&json!({ "op": "spawn", "id": id, "argv": argv, "cwd": sp.cwd, "env": terminal_env(&sp.env), "env_clear": true,
                                    "replace": true, "rows": sp.rows, "cols": sp.cols })).map_err(|e| e.to_string())?;
        ok(&r)?;
        attach_existing(pane_id, sp.rows, sp.cols)
    }

    pub fn attach_existing(pane_id: i64, rows: u16, cols: u16) -> Result<UnixStream, String> {
        let c = Client::connect(&sock()).map_err(|e| e.to_string())?;
        let (reply, stream) = c.attach(&term_id(pane_id), rows, cols).map_err(|e| e.to_string())?;
        ok(&reply)?;
        Ok(stream)
    }

    pub fn list() -> Result<Vec<HostTerm>, String> {
        let Some(mut c) = client_if_running() else { return Ok(vec![]) };
        let v = c.request(&json!({ "op": "list" })).map_err(|e| e.to_string())?;
        Ok(v["terminals"].as_array().cloned().unwrap_or_default().iter().map(|t| HostTerm {
            id: t["id"].as_str().unwrap_or("").to_string(), alive: t["alive"].as_bool().unwrap_or(false), exit: t["exit"].as_i64(),
            argv: t["argv"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
            rows: t["rows"].as_u64().unwrap_or(24) as u16, cols: t["cols"].as_u64().unwrap_or(80) as u16, clients: t["clients"].as_u64().unwrap_or(0),
        }).collect())
    }
    /// Panes' terminals the host still has alive, for the boot-time reattach.
    pub fn list_running() -> Result<Vec<HostTerm>, String> { Ok(list()?.into_iter().filter(|t| t.alive && pane_of(&t.id).is_some()).collect()) }

    pub fn resize(pane_id: i64, rows: u16, cols: u16) {
        if let Some(mut c) = client_if_running() { let _ = c.request(&json!({ "op": "resize", "id": term_id(pane_id), "rows": rows, "cols": cols })); }
    }
    pub fn kill(pane_id: i64) {
        crate::pane_watch::release(pane_id);   // a stopped process cannot act on being told to end
        if let Some(mut c) = client_if_running() { let _ = c.request(&json!({ "op": "kill", "id": term_id(pane_id) })); }
    }
    /// The exit code the host recorded, waiting a moment for it (the host notes the exit just after the output closes).
    pub fn exit_code(pane_id: i64) -> Option<i64> {
        let id = term_id(pane_id);
        let end = Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(ts) = list() { if let Some(t) = ts.into_iter().find(|t| t.id == id) { if let Some(code) = t.exit { return Some(code); } } }
            if Instant::now() >= end { return None; }
            std::thread::sleep(Duration::from_millis(40));
        }
    }
    pub fn host_reachable() -> bool { client_if_running().is_some() }
    pub fn socket_path() -> String { sock().to_string_lossy().to_string() }
    /// The visible rows of the pane's terminal, as the host's screen engine holds them. `None` when there is no host or no such terminal.
    pub fn screen_rows(pane_id: i64) -> Option<Vec<String>> {
        let mut c = client_if_running()?;
        let v = c.request(&json!({ "op": "snapshot", "id": term_id(pane_id) })).ok()?;
        if v.get("ok") != Some(&Value::Bool(true)) { return None; }
        Some(v["rows"].as_array()?.iter().map(|r| r.as_str().unwrap_or("").to_string()).collect())
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;
    pub fn spawn_and_attach(_: i64, _: &Spawn) -> Result<std::net::TcpStream, String> { Err("the terminal host is not available on this platform yet (RFC 0007 M7)".into()) }
    pub fn attach_existing(_: i64, _: u16, _: u16) -> Result<std::net::TcpStream, String> { Err("the terminal host is not available on this platform yet".into()) }
    pub fn list() -> Result<Vec<HostTerm>, String> { Ok(vec![]) }
    pub fn list_running() -> Result<Vec<HostTerm>, String> { Ok(vec![]) }
    pub fn resize(_: i64, _: u16, _: u16) {}
    pub fn kill(_: i64) {}
    pub fn exit_code(_: i64) -> Option<i64> { None }
    pub fn host_reachable() -> bool { false }
    pub fn socket_path() -> String { String::new() }
    pub fn screen_rows(_: i64) -> Option<Vec<String>> { None }
}
pub use imp::*;

/// `GET /api/v1/cockpit/pty/host`: which owner the terminals have, and what the host holds.
pub fn status() -> Value {
    let terms = list().unwrap_or_default();
    json!({ "mode": if enabled() { "sidecar" } else { "in-process" }, "reachable": host_reachable(), "socket": socket_path(),
            "terminals": terms.iter().map(|t| json!({ "id": t.id, "pane_id": pane_of(&t.id), "alive": t.alive, "exit": t.exit, "rows": t.rows, "cols": t.cols, "clients": t.clients, "argv": t.argv })).collect::<Vec<_>>() })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_ids_round_trip_and_only_panes_are_ours() {
        assert_eq!(term_id(42), "pane-42"); assert_eq!(pane_of("pane-42"), Some(42));
        for other in ["w1", "pane-", "pane-x", "xpane-1", ""] { assert_eq!(pane_of(other), None, "{other}"); }
    }
    #[test]
    fn the_terminal_environment_is_the_serves_plus_the_callers_minus_the_markers() {
        let mut extra = HashMap::new();
        extra.insert("ANTILOKI_TEST_X".to_string(), "1".to_string());
        extra.insert("CLAUDECODE".to_string(), "1".to_string()); // even a caller cannot put a marker back
        let env = terminal_env(&extra);
        assert_eq!(env.get("ANTILOKI_TEST_X").map(String::as_str), Some("1"));
        for k in SCRUBBED_ENV { assert!(!env.contains_key(k), "{k}"); }
        assert_eq!(env.get("PATH"), std::env::var("PATH").ok().as_ref());
    }
}
