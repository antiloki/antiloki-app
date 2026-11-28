//! THE PREVIEW NODE (2026-11-21: "sometimes when a team is running some code frontend or backend a preview node should be live so we can see the results"): starts the run's project (its `dev` or `start` script, or the file its package.json names as main) in the folder the node works in, waits for the address it announces, and keeps it up until the run ends.
//! Own process group so stopping it ends the whole tree (the shell, npm, the server). Output goes to a log file the screen can read. Nothing here edits a project file; `npm install` runs only when dependencies are declared and `node_modules` is missing.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

struct Up { pid: u32, node: String, url: String, cmd: String, log: String, dir: String }
fn reg() -> &'static Mutex<HashMap<String, Vec<Up>>> { static R: OnceLock<Mutex<HashMap<String, Vec<Up>>>> = OnceLock::new(); R.get_or_init(Default::default) }

/// How a folder's project is started to be looked at: `npm run dev`, `npm start`, or `node <main>`; None when it has nothing to start. Pure over the folder.
pub fn start_command(dir: &Path) -> Option<String> {
    let pkg = std::fs::read_to_string(dir.join("package.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let script = |n: &str| pkg.as_ref().and_then(|p| p["scripts"][n].as_str()).map_or(false, |x| !x.trim().is_empty());
    if script("dev") { return Some("npm run dev".into()); } if script("start") { return Some("npm start".into()); }
    if let Some(m) = pkg.as_ref().and_then(|p| p["main"].as_str()).filter(|m| dir.join(m).is_file() && !m.contains("..")) { return Some(format!("node {m}")); }
    for f in ["server.js", "index.js", "app.js", "server/index.js", "src/server.js"] { if dir.join(f).is_file() { return Some(format!("node {f}")); } }
    None
}
/// Whether the project declares dependencies that are not installed yet. Pure over the folder.
pub fn needs_install(dir: &Path) -> bool {
    let Some(p) = std::fs::read_to_string(dir.join("package.json")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else { return false };
    let any = |k: &str| p[k].as_object().map_or(false, |o| !o.is_empty()); (any("dependencies") || any("devDependencies")) && !dir.join("node_modules").is_dir()
}
fn tail(log: &str, n: usize) -> String { let t = std::fs::read_to_string(log).unwrap_or_default(); t.lines().rev().take(n).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n") }
fn kill(pid: u32) {
    #[cfg(unix)] { let _ = crate::proc::cmd("kill").args(["--", &format!("-{pid}")]).status(); let _ = crate::proc::cmd("kill").arg(pid.to_string()).status(); }
    #[cfg(windows)] { let _ = crate::proc::cmd("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).status(); }
}
/// The PATH a project's own tools (node, npm) are found on even when the serve was started with a bare one (the same extras the Code screen's preview uses).
fn tool_path() -> String { antiloki_rs::proc::extend_path(&std::env::var("PATH").unwrap_or_default(), &antiloki_rs::proc::tool_path_extras()) }
/// Start the project of `dir` for run `run`, node `node`; Ok({url, cmd, log}) when it announced an address, Err(why) when it ended or said nothing within `wait_s`. One server per node: a second start replaces the first.
pub async fn start(run: &str, node: &str, dir: &Path, log_dir: &Path, wait_s: u64) -> Result<Value, String> {
    stop_node(run, node);
    let Some(cmd) = start_command(dir) else { return Err("this project has nothing to start: no `dev` or `start` script and no server file".into()) };
    let _ = std::fs::create_dir_all(log_dir); let log = log_dir.join(format!("preview-{node}.log")).to_string_lossy().to_string();
    if needs_install(dir) { let ok = tokio::time::timeout(std::time::Duration::from_secs(300), crate::proc_async::shell("npm install --no-audit --no-fund").current_dir(dir).env("PATH", tool_path()).output()).await; if !matches!(ok, Ok(Ok(ref o)) if o.status.success()) { return Err("npm install did not finish in this folder".into()); } }
    // a machine whose node is a version-manager shim with no default version prints "No version is set for shim": the project is started once more through the manager, with a current node
    let mut cmd = cmd; let mut retried = false;
    loop {
        let f = std::fs::File::create(&log).map_err(|e| e.to_string())?; let f2 = f.try_clone().map_err(|e| e.to_string())?;
        let mut c = crate::proc_async::shell(&cmd); c.current_dir(dir).env("PATH", tool_path()).env("FORCE_COLOR", "0").env("BROWSER", "none").stdout(f).stderr(f2).stdin(std::process::Stdio::null());
        #[cfg(unix)] { c.process_group(0); }
        let mut child = c.spawn().map_err(|e| format!("could not start `{cmd}`: {e}"))?; let pid = child.id().unwrap_or(0);
        let began = std::time::Instant::now();
        let shim = loop {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if let Ok(Some(st)) = child.try_wait() { let t = std::fs::read_to_string(&log).unwrap_or_default(); if !retried && t.contains("No version is set for shim") { break true; } return Err(format!("`{cmd}` ended at once (exit {}):\n{}", st.code().unwrap_or(-1), tail(&log, 12))); }
            if let Some(url) = crate::preview::sniff_url(&std::fs::read_to_string(&log).unwrap_or_default()) {
                reg().lock().unwrap_or_else(|p| p.into_inner()).entry(run.into()).or_default().push(Up { pid, node: node.into(), url: url.clone(), cmd: cmd.clone(), log: log.clone(), dir: dir.display().to_string() });
                tokio::spawn(async move { let _ = child.wait().await; });   // reaped when it ends
                return Ok(json!({ "url": url, "cmd": cmd, "log": log }));
            }
            if began.elapsed().as_secs() >= wait_s { kill(pid); return Err(format!("`{cmd}` did not announce an address in {wait_s} s:\n{}", tail(&log, 12))); }
        };
        if shim { retried = true; cmd = format!("mise x node@lts -- {cmd}"); }
    }
}
pub fn stop_node(run: &str, node: &str) { let mut g = reg().lock().unwrap_or_else(|p| p.into_inner()); if let Some(v) = g.get_mut(run) { v.retain(|u| { if u.node == node { kill(u.pid); false } else { true } }); } }
/// End every preview of a run (the run ended, or the person stopped it).
pub fn stop_run(run: &str) -> usize { let v = reg().lock().unwrap_or_else(|p| p.into_inner()).remove(run).unwrap_or_default(); for u in &v { kill(u.pid); } v.len() }
/// The previews a run has up: `[{node, url, cmd, dir, log_tail}]`.
pub fn list(run: &str) -> Vec<Value> { reg().lock().unwrap_or_else(|p| p.into_inner()).get(run).map(|v| v.iter().map(|u| json!({ "node": u.node, "url": u.url, "cmd": u.cmd, "dir": u.dir, "log_tail": tail(&u.log, 30) })).collect()).unwrap_or_default() }

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(n: &str) -> std::path::PathBuf { let d = std::env::temp_dir().join(format!("antiloki-prev-{n}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }
    #[test] fn a_project_is_started_by_its_own_script_or_server_file_or_not_at_all() {
        let d = tmp("a"); assert_eq!(start_command(&d), None); std::fs::write(d.join("server.js"), "").unwrap(); assert_eq!(start_command(&d).as_deref(), Some("node server.js"));
        std::fs::write(d.join("package.json"), r#"{"scripts":{"start":"node x.js"}}"#).unwrap(); assert_eq!(start_command(&d).as_deref(), Some("npm start")); std::fs::write(d.join("package.json"), r#"{"scripts":{"start":"a","dev":"vite"}}"#).unwrap(); assert_eq!(start_command(&d).as_deref(), Some("npm run dev"));
        std::fs::write(d.join("package.json"), r#"{"main":"../evil.js"}"#).unwrap(); assert_eq!(start_command(&d).as_deref(), Some("node server.js")); let _ = std::fs::remove_dir_all(&d);
    }
    #[test] fn install_runs_only_when_dependencies_are_declared_and_missing() {
        let d = tmp("b"); assert!(!needs_install(&d)); std::fs::write(d.join("package.json"), r#"{"dependencies":{"express":"4"}}"#).unwrap(); assert!(needs_install(&d)); std::fs::create_dir(d.join("node_modules")).unwrap(); assert!(!needs_install(&d)); let _ = std::fs::remove_dir_all(&d);
    }
    #[tokio::test] async fn a_server_that_announces_an_address_is_up_listed_and_stopped_with_the_run() {
        let d = tmp("c"); std::fs::write(d.join("server.js"), "require('http').createServer((q,r)=>r.end('hi')).listen(0,'127.0.0.1',function(){console.log('listening on http://127.0.0.1:'+this.address().port)})").unwrap();
        if crate::proc::cmd("node").arg("-v").output().map(|o| o.status.success()).unwrap_or(false) {
            let r = start("run-x", "prev", &d, &d.join("logs"), 20).await.unwrap(); let url = r["url"].as_str().unwrap().to_string(); assert!(url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:"), "{url}");
            assert_eq!(list("run-x").len(), 1); assert_eq!(stop_run("run-x"), 1); assert!(list("run-x").is_empty());
        }
        let _ = std::fs::remove_dir_all(&d);
    }
    #[tokio::test] async fn a_server_that_dies_at_once_says_why() {
        let d = tmp("d"); std::fs::write(d.join("server.js"), "console.error('boom'); process.exit(3)").unwrap();
        if crate::proc::cmd("node").arg("-v").output().map(|o| o.status.success()).unwrap_or(false) { let e = start("run-y", "p", &d, &d.join("l"), 10).await.unwrap_err(); assert!(e.contains("exit 3") && e.contains("boom"), "{e}"); }
        let _ = std::fs::remove_dir_all(&d);
    }
}
