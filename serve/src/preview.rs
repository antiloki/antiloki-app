//! Port of `focus/pr-preview.service.ts` — the PR preview registry, plus `pr/worktree/open`.
//!
//! `start` (spawning a dev server, with a docker branch) is NOT here yet; this covers the
//! in-memory registry half (status/stop) and the folder-open, which is what the FE polls.

use axum::{
    body::Bytes,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::dispatch::{engine_call, AppState};

pub struct PreviewEntry {
    pub url: Option<String>,
    /// the worktree this dev server serves - so the QA looks at a preview only when it is THIS agent's (operator 2026-09-30: the QA "showed another frontend, not the one from this agent worktree")
    pub cwd: String,
    pub log: String,
    pub exited: Option<i32>,
    pub cmd: String,
    /// the dev server's pid, so `stop` can end it (None when the spawn failed)
    pub pid: Option<u32>,
}

/// Strip ANSI escapes — Vite colours the port INSIDE the URL (`http://localhost:\x1b[1m5174\x1b[22m/`),
/// which no URL regex survives. Pure — tested.
pub fn strip_ansi(s: &str) -> String {
    let re = regex::Regex::new("\x1b\\[[0-9;?]*[ -/]*[@-~]").unwrap();
    re.replace_all(s, "").into_owned()
}

/// The dev server's own announcement: the first `http://localhost:PORT…` (or 127.0.0.1 / 0.0.0.0,
/// mapped to localhost) in the ANSI-stripped log — Vite, Next, CRA, Angular all print one. Pure — tested.
pub fn sniff_url(log: &str) -> Option<String> {
    let clean = strip_ansi(log);
    let re = regex::Regex::new(r#"(?i)https?://(?:localhost|127\.0\.0\.1|0\.0\.0\.0):(\d+)[^\s"']*"#).unwrap();
    let m = re.find(&clean)?;
    Some(m.as_str().replace("0.0.0.0", "localhost").trim_end_matches(['.', ',', ')', ']']).to_string())
}

#[derive(Default)]
pub struct PreviewRegistry {
    entries: Mutex<HashMap<String, PreviewEntry>>,
}
impl PreviewRegistry {
    /// the URLs of the dev servers that are up right now (announced a URL and not exited) - visual QA looks at the operator's running preview when there is exactly one
    pub fn live_urls(&self) -> Vec<String> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).values().filter(|e| e.exited.is_none()).filter_map(|e| e.url.clone()).collect()
    }
    /// the live dev servers that serve THIS worktree - a preview of another agent, another workspace or the main checkout shows other code and is never evidence for this change
    pub fn live_urls_for(&self, cwd: &str) -> Vec<String> {
        let canon = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| std::path::PathBuf::from(p));
        let want = canon(cwd);
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).values().filter(|e| e.exited.is_none() && !e.cwd.is_empty() && canon(&e.cwd) == want).filter_map(|e| e.url.clone()).collect()
    }
}
/// the process-wide registry's live URLs (empty before the first preview ever started)
pub fn live_urls_global() -> Vec<String> { LIVE.get().map(|r| r.live_urls()).unwrap_or_default() }
/// the live previews of one worktree (empty before the first preview ever started)
pub fn live_urls_for(cwd: &str) -> Vec<String> { LIVE.get().map(|r| r.live_urls_for(cwd)).unwrap_or_default() }

/// THE REGISTRY THIS PROCESS SERVES, for the shutdown hook and the tests' sake (2026-09-28)
static LIVE: std::sync::OnceLock<Arc<PreviewRegistry>> = std::sync::OnceLock::new();
/// Stop every dev server this process started — the standalone binary calls it on SIGTERM / SIGINT (main.rs); the desktop
/// app, which links `run()` in-process, may call it from its own exit handler.
pub fn stop_all_global() { if let Some(r) = LIVE.get() { r.stop_all(); } }

// A DEV SERVER MUST NOT OUTLIVE THE SERVE THAT STARTED IT (2026-09-28). It runs in its own process group so `stop` can end
// the whole tree — which also means it no longer dies with serve, and every restart left the old vite servers running,
// untracked, holding their ports (13 by evening). The pids are written down as they start; a serve that starts finds the
// previous one's leftovers and ends them, which covers a crash or a SIGKILL as well as a clean stop.
static PIDS: Mutex<()> = Mutex::new(());
fn pid_file() -> std::path::PathBuf {
    let home = std::env::var("ANTILOKI_HOME").map(std::path::PathBuf::from).unwrap_or_else(|_| {
        let h = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).map(std::path::PathBuf::from).unwrap_or_else(|_| std::path::PathBuf::from("."));
        h.join(".antiloki")
    });
    home.join("run").join("previews.json")
}
fn read_pids() -> serde_json::Map<String, Value> {
    std::fs::read_to_string(pid_file()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default()
}
fn write_pids(m: &serde_json::Map<String, Value>) {
    let f = pid_file();
    if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
    let _ = std::fs::write(f, Value::Object(m.clone()).to_string());
}
fn remember_pid(slug: &str, pid: u32, cmd: &str) {
    let _g = PIDS.lock().unwrap_or_else(|p| p.into_inner());
    let mut m = read_pids(); m.insert(slug.to_string(), json!({ "pid": pid, "cmd": cmd })); write_pids(&m);
}
fn forget_pid(slug: &str) {
    let _g = PIDS.lock().unwrap_or_else(|p| p.into_inner());
    let mut m = read_pids(); if m.remove(slug).is_some() { write_pids(&m); }
}
/// Does this pid still name the shell we started? Its process group is its own pid (we made it so) and its command line
/// carries the dev command — so a pid the OS has since given to something else is never touched. Pure over the `ps` line — tested.
pub fn is_our_preview(ps_line: &str, pid: u32, cmd: &str) -> bool {
    let mut it = ps_line.trim().splitn(2, char::is_whitespace);
    let pgid = it.next().and_then(|t| t.trim().parse::<u32>().ok());
    let command = it.next().unwrap_or("");
    pgid == Some(pid) && !cmd.trim().is_empty() && command.contains(cmd.trim())
}
/// End the dev servers a previous serve left behind. Returns how many groups were ended.
pub fn sweep_orphans() -> usize {
    let _g = PIDS.lock().unwrap_or_else(|p| p.into_inner());
    let m = read_pids();
    let mut ended = 0usize;
    #[cfg(unix)]
    for (_, v) in m.iter() {
        let pid = v.get("pid").and_then(Value::as_u64).unwrap_or(0) as u32;
        let cmd = v.get("cmd").and_then(Value::as_str).unwrap_or("");
        if pid == 0 { continue }
        let line = crate::proc::cmd("ps").args(["-o", "pgid=,command=", "-p", &pid.to_string()]).output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
        if is_our_preview(&line, pid, cmd) {
            let _ = crate::proc::cmd("kill").args(["--", &format!("-{pid}")]).status();
            ended += 1;
        }
    }
    if !m.is_empty() { write_pids(&serde_json::Map::new()); }
    ended
}

impl PreviewRegistry {
    pub fn new() -> Arc<Self> {
        let r = Arc::new(Self::default());
        let _ = LIVE.set(r.clone());
        r
    }
    /// every dev server, ended — the shutdown path
    pub fn stop_all(&self) {
        let slugs: Vec<String> = self.entries.lock().unwrap().keys().cloned().collect();
        for s in slugs { self.stop(&s); }
    }

    /// `{running:false, url:null, log:'', exited:null}` when unknown — note the ABSENT `cmd` key
    /// in that branch, which the live BE also omits.
    pub fn status(&self, ref_slug: &str) -> Value {
        let entries = self.entries.lock().unwrap();
        match entries.get(ref_slug) {
            None => json!({ "running": false, "url": null, "log": "", "exited": null }),
            Some(p) => json!({
                "running": p.exited.is_none(),
                "url": p.url,
                // `log.slice(-4000)` — the LAST 4000 chars, not the first.
                "log": p.log.chars().rev().take(4000).collect::<String>().chars().rev().collect::<String>(),
                "cmd": p.cmd,
                "exited": p.exited,
            }),
        }
    }

    pub fn stop(&self, ref_slug: &str) -> Value {
        let mut entries = self.entries.lock().unwrap();
        forget_pid(ref_slug);
        match entries.remove(ref_slug) {
            None => json!({ "ok": true, "stopped": false }),
            Some(p) => {
                if let (Some(pid), None) = (p.pid, p.exited) {
                    // the shell's whole process group: `sh -c "npm run dev"` + vite + its esbuild
                    #[cfg(unix)]
                    { let _ = crate::proc::cmd("kill").args(["--", &format!("-{pid}")]).status(); let _ = crate::proc::cmd("kill").arg(pid.to_string()).status(); }
                    #[cfg(windows)]
                    { let _ = crate::proc::cmd("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).status(); }
                }
                json!({ "ok": true, "stopped": true })
            }
        }
    }

    /// The reader threads' seam: append output, sniff the URL once, record the exit.
    fn append(&self, ref_slug: &str, chunk: &str) {
        let mut entries = self.entries.lock().unwrap();
        if let Some(p) = entries.get_mut(ref_slug) {
            p.log.push_str(chunk);
            if p.log.len() > 12_000 { let cut = p.log.len() - 12_000; let at = p.log.char_indices().map(|(i, _)| i).find(|&i| i >= cut).unwrap_or(0); p.log = p.log[at..].to_string(); }
            if p.url.is_none() { p.url = sniff_url(&p.log); }
        }
    }
    fn mark_exit(&self, ref_slug: &str, code: i32) {
        let mut entries = self.entries.lock().unwrap();
        if let Some(p) = entries.get_mut(ref_slug) { if p.exited.is_none() { p.exited = Some(code); } }
    }
}

fn body_json(body: &Bytes) -> Value {
    serde_json::from_slice(body).unwrap_or_else(|_| json!({}))
}

pub async fn preview_status(reg: Arc<PreviewRegistry>, q: HashMap<String, String>) -> Response {
    let slug = q.get("ref_slug").cloned().unwrap_or_default();
    Json(reg.status(&slug)).into_response()
}

pub async fn preview_stop(reg: Arc<PreviewRegistry>, body: Bytes) -> Response {
    let b = body_json(&body);
    let slug = b.get("ref_slug").and_then(|v| v.as_str()).unwrap_or("");
    (StatusCode::CREATED, Json(reg.stop(slug))).into_response()
}

/// `POST /api/v1/pr/worktree/open` — resolve the PR's worktree, then open it in the OS file
/// manager. Both failure branches answer 201 with `{ok:false,...}`, not an error status.
pub async fn worktree_open(state: Arc<AppState>, body: Bytes) -> Response {
    let b = body_json(&body);
    let ref_slug = b.get("ref_slug").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if ref_slug.is_empty() {
        return (StatusCode::CREATED, Json(json!({ "ok": false, "error": "ref_slug required" }))).into_response();
    }

    let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
    let target = engine_call(&state, "system.preview.target.resolve", json!({ "ref_slug": ref_slug }), ctx)
        .await
        .map(|(v, _)| v)
        .unwrap_or(Value::Null);

    if target.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let err = target
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("no worktree on this PR (merged or cleaned)");
        return (StatusCode::CREATED, Json(json!({ "ok": false, "error": err }))).into_response();
    }

    let wt = crate::shared_folder::working_dir(&state, &target).await;
    (StatusCode::CREATED, Json(open_folder(&wt))).into_response()
}

/// `POST /api/v1/pr/preview/start` — resolve the target, resolve the dev script, then boot a dev
/// server for the PR's worktree.
///
/// ⚠️ NOT VERIFIED against the BE. Proving parity means booting real dev servers (and, on the
/// docker path, a running container), which cannot be done safely from an automated sweep. This
/// is written to match the source, not measured against it.
///
/// ⚠️ It also contains the only DESTRUCTIVE branch in the whole port: when the existing
/// `node_modules` was installed for a different runtime flavor (host vs docker), the BE RENAMES it
/// aside so the correct one can be installed. Native modules built for Linux do not load on
/// Windows and vice versa, so the swap is the point — but it moves a real directory, which is why
/// it is reproduced exactly rather than "improved".
/// The command a package.json runs for a dev server: its `dev` script, else `start`. Pure - tested.
pub fn detect_dev_script(package_json: &str) -> Option<String> {
    let v: Value = serde_json::from_str(package_json).ok()?; let sc = v.get("scripts")?;
    if sc.get("dev").and_then(Value::as_str).map_or(false, |d| !d.trim().is_empty()) { return Some("npm run dev".into()); }
    if sc.get("start").and_then(Value::as_str).map_or(false, |d| !d.trim().is_empty()) { return Some("npm start".into()); }
    None
}
pub async fn preview_start(
    state: Arc<AppState>,
    reg: Arc<PreviewRegistry>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let b = body_json(&body);
    let ctx = crate::dispatch::attribution_ctx(&headers);

    let target = engine_call(
        &state,
        "system.preview.target.resolve",
        json!({
            "ref_slug": b.get("ref_slug").and_then(|v| v.as_str()).unwrap_or(""),
            "pane_id": b.get("pane_id").cloned().unwrap_or(json!(0)),
            "target": b.get("target").and_then(|v| v.as_str()).unwrap_or(""),
        }),
        ctx.clone(),
    )
    .await
    .map(|(v, _)| v)
    .unwrap_or(Value::Null);

    if target.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let e = target
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("could not resolve a preview target");
        return (StatusCode::CREATED, Json(json!({ "ok": false, "error": e }))).into_response();
    }

    let key = target.get("key").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let own = target.get("worktree_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let cwd = crate::shared_folder::working_dir(&state, &target).await;
    // a box cut from a never-committed project before seeding existed is empty: heal it before `npm` looks for a package.json in it
    if !own.is_empty() { let c = own.clone(); let _ = tokio::task::spawn_blocking(move || crate::boxes::heal_in_place(&c)).await; }

    let scripts = engine_call(&state, "system.workspace.scripts", json!({ "op": "get" }), ctx)
        .await
        .map(|(v, _)| v)
        .unwrap_or(Value::Null);
    // `(scripts?.result ?? scripts) ?? {}`
    let s = scripts.get("result").cloned().unwrap_or(scripts);
    let pick = |k: &str| -> Option<String> {
        s.get("scripts")
            .and_then(|x| x.get(k))
            .or_else(|| s.get("detected").and_then(|x| x.get(k)))
            .or_else(|| s.get(k))
            .and_then(|v| v.as_str())
            .map(String::from)
    };
    // NO SCRIPT RECORDED: the box's own package.json says how it runs (2026-11-11: a box the agent built an app in asked the person to configure a dev script it already had)
    let detected = std::fs::read_to_string(std::path::Path::new(&cwd).join("package.json")).ok().and_then(|t| detect_dev_script(&t));
    let Some(dev) = pick("dev").filter(|d| !d.is_empty()).or(detected) else {
        return (
            StatusCode::CREATED,
            Json(json!({ "ok": false, "error": "no dev script set — configure it on the Code screen" })),
        )
            .into_response();
    };
    let install_cmd = pick("install");
    let env_config = pick("env");

    // docker preflight: the container must actually be running before anything is spawned.
    let rt = target.get("runtime").cloned().unwrap_or(Value::Null);
    let mut docker: Option<(String, Option<u32>)> = None;
    if rt.get("runtime").and_then(|v| v.as_str()) == Some("docker") {
        if let Some(container) = rt.get("container").and_then(|v| v.as_str()).filter(|c| !c.is_empty()) {
            let running = crate::proc::cmd("docker")
                .args(["inspect", "-f", "{{.State.Running}}", container])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains("true"))
                .unwrap_or(false);
            if !running {
                let msg = format!(
                    "workspace runtime is docker but container '{container}' is not running — start the container first"
                );
                return (StatusCode::CREATED, Json(json!({ "ok": false, "error": msg }))).into_response();
            }
            let node_major = rt
                .get("image")
                .and_then(|v| v.as_str())
                .and_then(|img| regex::Regex::new(r"node:(\d+)").ok()?.captures(img)?.get(1)?.as_str().parse().ok());
            docker = Some((container.to_string(), node_major));
        }
    }

    (StatusCode::CREATED, Json(start_dev(&reg, &key, &cwd, &dev, install_cmd, env_config, docker))).into_response()
}

/// Port of `PrPreviewService.start()`.
/// MUST AN EXISTING `node_modules` BE MOVED ASIDE BEFORE THE PREVIEW INSTALLS ITS OWN? (Grokbot QA 2026-10-12, item 3's `.antiloki-nm-host-aside/`: a user's own `node_modules` has no antiloki marker, so it
/// read as "the wrong flavour", was renamed away and reinstalled from scratch on the first preview.) The marker records which runtime (host or docker) installed the native binaries. A marker that
/// disagrees with the wanted runtime is a real mismatch. NO marker is the user's own host install: a host preview uses it as it is; only a docker preview, which needs the container's binaries, moves it.
/// Pure - tested.
pub(crate) fn node_modules_must_move(exists: bool, flavor: Option<&str>, want: &str) -> bool {
    exists && match flavor { Some(f) => f != want, None => want != "host" }
}

fn start_dev(
    reg: &Arc<PreviewRegistry>,
    ref_slug: &str,
    cwd: &str,
    dev_cmd: &str,
    install_cmd: Option<String>,
    env_config: Option<String>,
    docker: Option<(String, Option<u32>)>,
) -> Value {
    let cwd_path = std::path::Path::new(cwd);
    if !cwd_path.exists() {
        return json!({ "ok": false, "error": format!("worktree gone on disk: {cwd}") });
    }
    reg.stop(ref_slug); // one per PR — replace any prior server

    let install_cmd = install_cmd.filter(|s| !s.is_empty()).unwrap_or_else(|| {
        if cwd_path.join("package-lock.json").exists() {
            "npm ci --no-audit --no-fund".into()
        } else {
            "npm install --no-audit --no-fund".into()
        }
    });

    let want = if docker.is_some() { "docker" } else { "host" };
    let nm = cwd_path.join("node_modules");
    let marker = nm.join(".antiloki-runtime");
    let flavor = std::fs::read_to_string(&marker).ok().map(|s| s.trim().to_string());
    if node_modules_must_move(nm.exists(), flavor.as_deref(), want) {
        // THE DESTRUCTIVE BRANCH — only for a REAL mismatch (see `node_modules_must_move`).
        let aside = cwd_path.join(format!(".antiloki-nm-{}-aside", flavor.as_deref().unwrap_or("host")));
        let _ = std::fs::remove_dir_all(&aside);
        let _ = std::fs::rename(&nm, &aside); // locked -> install works in place, best effort
    }
    let needs_install = !nm.exists();

    let marker_cmd = if docker.is_some() {
        format!("mkdir -p node_modules && echo {want} > node_modules/.antiloki-runtime")   // a project with no dependencies has no node_modules after its install: the marker makes the folder (2026-11-11: it failed with ENOENT)
    } else {
        format!("node -e \"const fs=require('fs');fs.mkdirSync('node_modules',{{recursive:true}});fs.writeFileSync('node_modules/.antiloki-runtime','{want}')\"")
    };
    let chain = if needs_install {
        format!("{install_cmd} && {marker_cmd} && {dev_cmd}")
    } else {
        dev_cmd.to_string()
    };

    let cfg_env = parse_env_lines(env_config.as_deref());

    let (child, docker_info, env_note) = match &docker {
        Some((container, node_major)) => {
            let work_path = format!(
                "/worktrees/{}",
                cwd_path.file_name().and_then(|s| s.to_str()).unwrap_or("")
            );
            // --openssl-legacy-provider only exists from node 17; on an older container it is
            // stripped, with a note in the log so the operator sees why.
            let legacy_ok = node_major.unwrap_or(0) >= 17;
            let mut env_obj: Vec<(String, String)> = vec![
                ("FORCE_COLOR".into(), "0".into()),
                ("BROWSER".into(), "none".into()),
                ("CI".into(), "".into()),
            ];
            if legacy_ok {
                env_obj.push(("NODE_OPTIONS".into(), "--openssl-legacy-provider".into()));
            }
            for (k, v) in &cfg_env {
                env_obj.retain(|(ek, _)| ek != k);
                env_obj.push((k.clone(), v.clone()));
            }
            let note = if !legacy_ok {
                format!(
                    "[stripped --openssl-legacy-provider from env: the container's node {} predates it]\n",
                    node_major.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
                )
            } else {
                String::new()
            };
            let exports = env_obj
                .iter()
                .map(|(k, v)| format!("export {k}='{}'", v.replace('\'', r"'\''")))
                .collect::<Vec<_>>()
                .join("; ");
            let inner = format!("{exports}; {chain}");
            let q = |s: &str| format!("\"{}\"", s.replace('"', "\\\""));
            let cmdline = format!("docker exec -w {} {container} sh -lc {}", q(&work_path), q(&inner));
            let child = spawn_shell(&cmdline, None, &[]);
            (child, Some((container.clone(), work_path)), note)
        }
        None => {
            let mut envs: Vec<(String, String)> = vec![
                ("FORCE_COLOR".into(), "0".into()),
                ("BROWSER".into(), "none".into()),
                ("CI".into(), "".into()),
                (
                    "NODE_OPTIONS".into(),
                    [std::env::var("NODE_OPTIONS").ok().filter(|s| !s.is_empty()), Some("--openssl-legacy-provider".into())]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
            ];
            for (k, v) in &cfg_env {
                envs.retain(|(ek, _)| ek != k);
                envs.push((k.clone(), v.clone()));
            }
            (spawn_shell(&chain, Some(cwd), &envs), None, String::new())
        }
    };

    let boot_note = format!(
        "{}{}{}",
        docker_info
            .as_ref()
            .map(|(c, w)| format!("[docker: running inside {c} at {w}]\n"))
            .unwrap_or_default(),
        env_note,
        if needs_install {
            if docker_info.is_some() {
                "[installing dependencies inside the container (native modules compile for Linux — first time can take minutes)…]\n"
            } else {
                "[installing dependencies — this can take a minute…]\n"
            }
        } else {
            ""
        }
    );

    let ok = child.is_some();
    // a second boot of the same target replaces the entry — end the server the old entry pointed at first
    reg.stop(ref_slug);
    reg.entries.lock().unwrap().insert(
        ref_slug.to_string(),
        PreviewEntry { url: None, cwd: cwd.to_string(), log: boot_note, exited: if ok { None } else { Some(-1) }, cmd: dev_cmd.to_string(), pid: child.as_ref().map(|c| c.id()) },
    );
    if let Some(pid) = child.as_ref().map(|c| c.id()) { remember_pid(ref_slug, pid, dev_cmd); }
    // READ THE SERVER (2026-09-17): stdout + stderr feed the log; the URL is sniffed from the server's
    // own announcement; the exit code lands when it dies. Without this the entry stayed `url:null`
    // forever and nothing that waits on a preview (Run pane, Pulls, Design frames) could work.
    if let Some(mut child) = child {
        let key = ref_slug.to_string();
        let pump = |stream: Option<Box<dyn std::io::Read + Send>>, reg: Arc<PreviewRegistry>, key: String| {
            if let Some(mut s) = stream {
                std::thread::spawn(move || { let mut buf = [0u8; 4096]; loop { match s.read(&mut buf) { Ok(0) | Err(_) => break, Ok(n) => reg.append(&key, &String::from_utf8_lossy(&buf[..n])) } } });
            }
        };
        pump(child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), reg.clone(), key.clone());
        pump(child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), reg.clone(), key.clone());
        let reg2 = reg.clone();
        std::thread::spawn(move || { let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1); reg2.mark_exit(&key, code); });
    }

    json!({ "ok": true, "booting": true, "cmd": dev_cmd, "docker": docker_info.is_some() })
}

pub(crate) fn spawn_shell(cmdline: &str, cwd: Option<&str>, envs: &[(String, String)]) -> Option<std::process::Child> {
    let mut cmd = crate::proc::shell_sync(cmdline);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    // the project's own tools (node, npm, pnpm ...) are found even when the serve was started with a bare PATH
    let base = envs.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.clone()).unwrap_or_else(|| std::env::var("PATH").unwrap_or_default());
    cmd.env("PATH", antiloki_rs::proc::extend_path(&base, &antiloki_rs::proc::tool_path_extras()));
    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    // ITS OWN PROCESS GROUP (2026-09-28): `stop` kills `-pid`, the group of the shell it spawned — but the child
    // inherited serve's group, so that kill hit nothing, `npm` died alone and vite lived on. 13 orphaned dev
    // servers had piled up by evening (3 on one worktree).
    #[cfg(unix)]
    { use std::os::unix::process::CommandExt; cmd.process_group(0); }
    cmd.spawn().ok()
}

/// `.env`-style lines: `KEY=value`, `#` comments and blanks skipped, surrounding quotes stripped.
fn parse_env_lines(src: Option<&str>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in src.unwrap_or("").lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
            out.push((k.trim().to_string(), v));
        }
    }
    out
}

/// `POST /api/v1/workspaces/pick-folder` — the native folder dialog.
///
/// A PowerShell child drives the shell's IFileOpenDialog. `-STA` is REQUIRED for COM and
/// `-EncodedCommand` takes base64 of the UTF-16LE script, which sidesteps every quoting problem the
/// multi-line script would otherwise have. Non-Windows answers `{path: null}` — the BE returns null
/// there too rather than pretending.
///
/// NOTE for W6: this opens a dialog owned by the SERVER process. Inside Tauri the right call is
/// the webview's own dialog API; keep this only while the server is a separate process.
/// `GET /api/v1/workspaces/browse?path=<dir>` — THE FOLDERS INSIDE ONE FOLDER, for a browser tab that has no native
/// dialog (operator 2026-09-28: "i tried to add a new workspace and got this: no native folder dialog on this
/// machine — type the path instead").
///
/// The message was true and it was a dead end. `pick_folder` below is a PowerShell/COM dialog that exists on Windows
/// only, and the desktop build uses its own plugin — so in a browser on Linux or macOS the only way to add a
/// workspace was to type an absolute path from memory, with no way to see what was there or whether it existed.
///
/// The serve runs on the operator's own machine, so it can simply say what is on it. Directories only: this is for
/// choosing a project root, and listing every file in a home directory would be noise as well as a wider answer than
/// the question needs. An unreadable entry is skipped rather than failing the whole listing, and `parent` is what
/// lets the caller walk back up.
pub async fn browse_folder(q: axum::extract::Query<HashMap<String, String>>) -> Response {
    let want = q.get("path").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let hidden = q.get("hidden").map(|v| v == "1" || v == "true").unwrap_or(false);
    let dir = match want {
        Some(p) => std::path::PathBuf::from(p),
        // no path asked for: start where the operator's projects most likely are
        None => dirs_home().unwrap_or_else(|| std::path::PathBuf::from("/")),
    };
    let dir = if dir.is_absolute() { dir } else { dirs_home().unwrap_or_default().join(dir) };
    let listed = tokio::task::spawn_blocking(move || {
        let mut out: Vec<Value> = vec![];
        let md = std::fs::metadata(&dir);
        if md.as_ref().map(|m| !m.is_dir()).unwrap_or(true) {
            return (dir.to_string_lossy().to_string(), None, out, Some(match md {
                Err(e) => format!("{e}"),
                Ok(_) => "that path is a file, not a folder".to_string(),
            }));
        }
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                // a directory we cannot stat is skipped, not fatal — a home folder usually holds a few
                let Ok(ft) = e.file_type() else { continue };
                if !ft.is_dir() { continue }
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') && !hidden { continue }          // dotfolders are not project roots (unless asked for)
                // a git repo is what the operator is usually looking for, so it is worth pointing at
                let repo = e.path().join(".git").exists();
                out.push(json!({ "name": name, "path": e.path().to_string_lossy(), "repo": repo }));
            }
        }
        out.sort_by(|a, b| a.get("name").and_then(Value::as_str).unwrap_or("")
                        .to_lowercase().cmp(&b.get("name").and_then(Value::as_str).unwrap_or("").to_lowercase()));
        let parent = dir.parent().map(|p| p.to_string_lossy().to_string());
        (dir.to_string_lossy().to_string(), parent, out, None)
    }).await.unwrap_or_else(|e| (String::new(), None, vec![], Some(e.to_string())));

    let (path, parent, dirs, error) = listed;
    (StatusCode::OK, Json(json!({ "ok": error.is_none(), "path": path, "parent": parent, "dirs": dirs, "error": error, "places": quick_places() }))).into_response()
}

/// The places a person looks first, those that exist on this machine: Home, Desktop, Documents, Downloads and the usual project folders. Never anything outside the home folder.
fn quick_places() -> Vec<Value> {
    let Some(home) = dirs_home() else { return vec![] };
    let mut v = vec![json!({ "name": "Home", "path": home.to_string_lossy() })];
    for n in ["Desktop", "Documents", "Downloads", "dev", "Dev", "projects", "Projects", "code", "Code", "src", "workspace", "repos", "git"] {
        let p = home.join(n); if p.is_dir() { v.push(json!({ "name": n, "path": p.to_string_lossy() })); }
    }
    v
}

/// A folder name that is one plain name: not empty, not `.` or `..`, no separator, no control character, not too long. Pure.
fn valid_folder_name(n: &str) -> bool {
    let t = n.trim();
    !t.is_empty() && t != "." && t != ".." && t.len() <= 200 && !t.contains(['/', '\\', '\0', ':', '*', '?', '"', '<', '>', '|']) && !t.chars().any(|c| c.is_control())
}

/// `POST /api/v1/workspaces/mkdir {parent, name}`: make a folder inside an existing one, for the folder picker's New folder button. One level only, no overwrite.
pub async fn make_folder(body: axum::body::Bytes) -> Response {
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let parent = v.get("parent").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let name = v.get("name").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if !valid_folder_name(&name) { return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "a folder name is one plain name: no slashes or special characters" }))).into_response(); }
    let parent = std::path::PathBuf::from(parent);
    if !parent.is_absolute() || !parent.is_dir() { return (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": "the folder to create it in does not exist" }))).into_response(); }
    let target = parent.join(&name);
    if target.exists() { return (StatusCode::CONFLICT, Json(json!({ "ok": false, "error": format!("{name} already exists here") }))).into_response(); }
    match std::fs::create_dir(&target) {
        Ok(()) => (StatusCode::CREATED, Json(json!({ "ok": true, "path": target.to_string_lossy() }))).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": format!("could not create it: {e}") }))).into_response(),
    }
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from)
}

pub async fn pick_folder() -> Response {
    let (path, supported) = tokio::task::spawn_blocking(pick_native_folder).await.unwrap_or((None, false));
    // `supported` IS THE DIFFERENCE BETWEEN A CANCEL AND NO DIALOG AT ALL (operator 2026-09-24, from the
    // installed Linux build: "the binary has a bug on click open project doesnt open the file explorer to
    // choose a folder"). The picker used to be a PowerShell/COM dialog on Windows only, so everywhere else it
    // answered `{"path": null}` — which the caller could only read as "the operator cancelled". The button
    // opened nothing and said nothing. Linux now drives zenity / kdialog and macOS osascript (2026-09-29: the
    // operator's Arch box has zenity, and the button still did nothing because nothing ever called it); when no
    // tool can show a dialog this says so plainly and the FE falls back to its own in-app folder browser.
    (StatusCode::CREATED, Json(json!({ "path": path, "supported": supported }))).into_response()
}

/// What one dialog tool did: chose a folder, was dismissed, or could not run at all (missing binary, no display).
#[derive(Debug, PartialEq)]
enum DialogOutcome { Picked(String), Cancelled, Unavailable }

/// Read a finished dialog process. zenity and kdialog exit 1 on Cancel with an EMPTY stderr; the same exit code with
/// text on stderr is a toolkit that could not open a display, which is "unavailable", not "cancelled". A cancel must
/// stay silent in the UI while unavailable must open the fallback, so the two cannot be merged.
fn classify_dialog(success: bool, stdout: &str, stderr: &str) -> DialogOutcome {
    let out = stdout.trim_end_matches(['\n', '\r']).trim();
    if success && !out.is_empty() { return DialogOutcome::Picked(out.to_string()); }
    let err = stderr.to_lowercase();
    if err.contains("cannot open display") || err.contains("could not connect") || err.contains("no such file")
        || err.contains("failed to connect") || err.contains("no protocol specified") || err.contains("xdg_runtime_dir") {
        return DialogOutcome::Unavailable;
    }
    DialogOutcome::Cancelled
}

/// Try one dialog program. A missing binary is `Unavailable`; the caller moves on to the next.
fn run_dialog(bin: &str, args: &[&str]) -> DialogOutcome {
    if crate::proc::which(bin).is_none() { return DialogOutcome::Unavailable; }
    match crate::proc::cmd(bin).args(args).output() {
        Ok(o) => classify_dialog(o.status.success(), &String::from_utf8_lossy(&o.stdout), &String::from_utf8_lossy(&o.stderr)),
        Err(_) => DialogOutcome::Unavailable,
    }
}

/// (chosen path, whether ANY dialog could be shown here).
fn pick_native_folder() -> (Option<String>, bool) {
    if cfg!(windows) { return (pick_windows_folder(), true); }
    let title = "Select the project folder";
    let mut tries: Vec<(&str, Vec<&str>)> = vec![];
    if cfg!(target_os = "macos") {
        tries.push(("osascript", vec!["-e", "POSIX path of (choose folder with prompt \"Select the project folder\")"]));
    } else {
        // no display at all (a headless box, an ssh session) means no toolkit can draw; do not even try
        let has_display = std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
        if !has_display { return (None, false); }
        tries.push(("zenity", vec!["--file-selection", "--directory", "--title", title]));
        tries.push(("kdialog", vec!["--getexistingdirectory", ".", "--title", title]));
        tries.push(("yad", vec!["--file", "--directory", "--title", title]));
        tries.push(("qarma", vec!["--file-selection", "--directory", "--title", title]));
    }
    for (bin, args) in tries {
        match run_dialog(bin, &args) {
            DialogOutcome::Picked(p) => {
                let t = p.trim_end_matches('/');
                return (Some(if t.is_empty() { "/".to_string() } else { t.to_string() }), true);
            }
            DialogOutcome::Cancelled => return (None, true),
            DialogOutcome::Unavailable => continue,
        }
    }
    // LAST, the desktop's own chooser through the XDG portal: every GNOME, KDE and most other desktops ship it, so a distro with none of the programs above still shows its own window
    #[cfg(unix)]
    if !cfg!(target_os = "macos") {
        match portal_pick_folder() {
            DialogOutcome::Picked(p) => { let t = p.trim_end_matches('/'); return (Some(if t.is_empty() { "/".to_string() } else { t.to_string() }), true); }
            DialogOutcome::Cancelled => return (None, true),
            DialogOutcome::Unavailable => {}
        }
    }
    (None, false)
}

/// `%41` -> `A`, for the `file://` addresses the portal answers with.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes(); let mut out: Vec<u8> = Vec::with_capacity(b.len()); let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() { if let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) { out.push(v); i += 3; continue; } }
        out.push(b[i]); i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// One line of `gdbus monitor` on the portal: the answer of a file chooser is `...Request.Response (0, {'uris': <['file:///home/u/proj']>, ...})`, code 0 = chosen, 1 = dismissed, 2 = it failed.
/// `None` for any other line. Pure.
fn parse_portal_response(line: &str) -> Option<DialogOutcome> {
    let at = line.find("org.freedesktop.portal.Request.Response (")?;
    let rest = &line[at + "org.freedesktop.portal.Request.Response (".len()..];
    let code: u32 = rest.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().ok()?;
    match code {
        0 => {
            let u = rest.find("file://")?; let tail = &rest[u + "file://".len()..];
            let end = tail.find(['\'', '"', '>', ']', ',']).unwrap_or(tail.len());
            let path = percent_decode(&tail[..end]);
            Some(if path.is_empty() { DialogOutcome::Cancelled } else { DialogOutcome::Picked(path) })
        }
        1 => Some(DialogOutcome::Cancelled),
        _ => Some(DialogOutcome::Unavailable),
    }
}

/// Ask the desktop's folder chooser through the XDG portal, using `gdbus` (part of glib, present wherever GTK is). Listen first, then ask: the answer is a signal on a request object whose name is only known after the call.
#[cfg(unix)]
fn portal_pick_folder() -> DialogOutcome {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    if crate::proc::which("gdbus").is_none() { return DialogOutcome::Unavailable; }
    let Ok(mut mon) = crate::proc::cmd("gdbus").args(["monitor", "--session", "--dest", "org.freedesktop.portal.Desktop"]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else { return DialogOutcome::Unavailable };
    let Some(out) = mon.stdout.take() else { let _ = mon.kill(); return DialogOutcome::Unavailable };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { for line in BufReader::new(out).lines().map_while(Result::ok) { if let Some(o) = parse_portal_response(&line) { let _ = tx.send(o); return; } } });
    std::thread::sleep(std::time::Duration::from_millis(250));
    let call = crate::proc::cmd("gdbus").args(["call", "--session", "--dest", "org.freedesktop.portal.Desktop", "--object-path", "/org/freedesktop/portal/desktop",
        "--method", "org.freedesktop.portal.FileChooser.OpenFile", "", "Select the project folder", "{'directory': <true>}"]).output();
    let outcome = match call {
        Ok(o) if o.status.success() => rx.recv_timeout(std::time::Duration::from_secs(300)).unwrap_or(DialogOutcome::Unavailable),
        _ => DialogOutcome::Unavailable,
    };
    let _ = mon.kill(); let _ = mon.wait();
    outcome
}

fn pick_windows_folder() -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    // The shell's IFileOpenDialog in FOS_PICKFOLDERS mode — the same Explorer-style chooser every
    // other Windows app opens. WinForms' FolderBrowserDialog was used here before and is the wrong
    // control twice over: on .NET Framework (which is what powershell.exe hosts) it renders the
    // legacy XP tree view, and it was parented to a hidden zero-opacity form, so it could neither be
    // seen nor raised. Owning the dialog to GetForegroundWindow() is what makes it appear at all —
    // a background server process cannot take foreground for itself, but a dialog parented to the
    // window that already holds it is drawn above that window.
    const PS_SCRIPT: &str = r##"
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class ModernPicker
{
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();

    [ComImport, Guid("DC1C5A9C-E88A-4dde-A5A1-60F82A20AEF7")]
    class FileOpenDialogRCW { }

    [ComImport, Guid("43826D1E-E718-42EE-BC55-A1E261C37BFE"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IShellItem
    {
        [PreserveSig] int BindToHandler(IntPtr pbc, ref Guid bhid, ref Guid riid, out IntPtr ppv);
        [PreserveSig] int GetParent(out IShellItem ppsi);
        [PreserveSig] int GetDisplayName(uint sigdnName, [MarshalAs(UnmanagedType.LPWStr)] out string ppszName);
        [PreserveSig] int GetAttributes(uint sfgaoMask, out uint psfgaoAttribs);
        [PreserveSig] int Compare(IShellItem psi, uint hint, out int piOrder);
    }

    // The FULL IFileDialog vtable: unused slots are stubbed but must stay in their exact order,
    // because COM dispatches by slot index, not by name.
    [ComImport, Guid("42f85136-db7e-439c-85f1-e4075d135fc8"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IFileDialog
    {
        [PreserveSig] int Show(IntPtr parent);
        [PreserveSig] int SetFileTypes(uint cFileTypes, IntPtr rgFilterSpec);
        [PreserveSig] int SetFileTypeIndex(uint iFileType);
        [PreserveSig] int GetFileTypeIndex(out uint piFileType);
        [PreserveSig] int Advise(IntPtr pfde, out uint pdwCookie);
        [PreserveSig] int Unadvise(uint dwCookie);
        [PreserveSig] int SetOptions(uint fos);
        [PreserveSig] int GetOptions(out uint pfos);
        [PreserveSig] int SetDefaultFolder(IShellItem psi);
        [PreserveSig] int SetFolder(IShellItem psi);
        [PreserveSig] int GetFolder(out IShellItem ppsi);
        [PreserveSig] int GetCurrentSelection(out IShellItem ppsi);
        [PreserveSig] int SetFileName([MarshalAs(UnmanagedType.LPWStr)] string pszName);
        [PreserveSig] int GetFileName([MarshalAs(UnmanagedType.LPWStr)] out string pszName);
        [PreserveSig] int SetTitle([MarshalAs(UnmanagedType.LPWStr)] string pszTitle);
        [PreserveSig] int SetOkButtonLabel([MarshalAs(UnmanagedType.LPWStr)] string pszText);
        [PreserveSig] int SetFileNameLabel([MarshalAs(UnmanagedType.LPWStr)] string pszLabel);
        [PreserveSig] int GetResult(out IShellItem ppsi);
        [PreserveSig] int AddPlace(IShellItem psi, int fdap);
        [PreserveSig] int SetDefaultExtension([MarshalAs(UnmanagedType.LPWStr)] string pszDefaultExtension);
        [PreserveSig] int Close(int hr);
        [PreserveSig] int SetClientGuid(ref Guid guid);
        [PreserveSig] int ClearClientData();
        [PreserveSig] int SetFilter(IntPtr pFilter);
    }

    const uint FOS_PICKFOLDERS     = 0x00000020;
    const uint FOS_FORCEFILESYSTEM = 0x00000040;
    const uint SIGDN_FILESYSPATH   = 0x80058000;

    public static string Pick(string title)
    {
        var dlg = (IFileDialog)(new FileOpenDialogRCW());
        uint opts;
        dlg.GetOptions(out opts);
        dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
        if (title != null) dlg.SetTitle(title);

        if (dlg.Show(GetForegroundWindow()) != 0) return null;   // non-zero = cancelled or error

        IShellItem item;
        if (dlg.GetResult(out item) != 0 || item == null) return null;
        string path;
        if (item.GetDisplayName(SIGDN_FILESYSPATH, out path) != 0) return null;
        return path;
    }
}
"@

$p = [ModernPicker]::Pick("Select the project folder for the new antiloki workspace")
if ($p) { [Console]::Out.Write($p) }
"##;
    // UTF-16LE, then base64 — exactly what -EncodedCommand expects.
    let utf16: Vec<u8> = PS_SCRIPT.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let encoded = base64_encode(&utf16);

    let out = crate::proc::cmd("powershell.exe")
        .args(["-NoProfile", "-STA", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &encoded])
        .output()
        .ok()?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

/// Standard base64 — small enough not to justify pulling the crate into this crate's tree.
fn base64_encode(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Port of `openFolder(cwd)`. Detached spawn, and the exit code is deliberately NOT checked —
/// Windows explorer exits 1 even on success.
fn open_folder(cwd: &str) -> Value {
    if !std::path::Path::new(cwd).exists() {
        return json!({ "ok": false, "error": format!("worktree gone on disk: {cwd}") });
    }
    let cmd = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    match crate::proc::cmd(cmd).arg(cwd).spawn() {
        Ok(_) => json!({ "ok": true, "path": cwd }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_new_folder_name_is_one_plain_name() {
        for ok in ["my-app", "My App", "site.v2", "über"] { assert!(valid_folder_name(ok), "{ok}"); }
        for bad in ["", "  ", ".", "..", "a/b", "a\\b", "x:y", "q?", "tab\there", "../up"] { assert!(!valid_folder_name(bad), "{bad:?}"); }
    }

    #[test]
    fn the_portals_answer_is_read_as_chosen_dismissed_or_failed_and_other_lines_are_ignored() {
        let ok = "/org/freedesktop/portal/desktop/request/1_42/t: org.freedesktop.portal.Request.Response (0, {'uris': <['file:///home/u/My%20Project']>, 'choices': <@a(ss) []>})";
        assert_eq!(parse_portal_response(ok), Some(DialogOutcome::Picked("/home/u/My Project".into())), "percent-encoding is undone");
        assert_eq!(parse_portal_response("x: org.freedesktop.portal.Request.Response (1, {})"), Some(DialogOutcome::Cancelled), "dismissed stays silent");
        assert_eq!(parse_portal_response("x: org.freedesktop.portal.Request.Response (2, {})"), Some(DialogOutcome::Unavailable), "a failed portal opens the fallback");
        assert_eq!(parse_portal_response("/org/freedesktop/portal/desktop: org.freedesktop.DBus.Properties.PropertiesChanged ()"), None);
        assert_eq!(percent_decode("a%2Fb%zz%"), "a/b%zz%", "bad escapes are kept as they are");
    }

    #[test]
    fn a_users_own_node_modules_is_not_moved_aside_for_a_host_preview() {
        assert!(!super::node_modules_must_move(true, None, "host"));          // no marker: the user's own install, used as it is
        assert!(!super::node_modules_must_move(true, Some("host"), "host"));  // antiloki's own host install: fine
        assert!(!super::node_modules_must_move(false, None, "host"));         // nothing there: nothing to move
        assert!(!super::node_modules_must_move(false, Some("docker"), "host"));
    }
    #[test]
    fn a_real_runtime_mismatch_still_moves_it_aside() {
        assert!(super::node_modules_must_move(true, Some("docker"), "host"));
        assert!(super::node_modules_must_move(true, Some("host"), "docker"));
        assert!(super::node_modules_must_move(true, None, "docker"));         // a docker preview needs the container's binaries, not the host's
    }

    use super::*;

    /// A restart must end the dev servers the last serve left, and only those (2026-09-28: 13 vite servers by evening).
    #[test]
    fn a_leftover_dev_server_is_ours_only_by_process_group_and_command() {
        let ours = "  4242 /bin/sh -c npm run dev -- --port 0";
        assert!(is_our_preview(ours, 4242, "npm run dev"));
        assert!(!is_our_preview(ours, 4243, "npm run dev"), "another pid is not ours");
        assert!(!is_our_preview("  4242 /usr/bin/firefox --new-window", 4242, "npm run dev"), "a reused pid running something else is never touched");
        assert!(!is_our_preview("  100 /bin/sh -c npm run dev", 4242, "npm run dev"), "its group is not its own: not the one we made");
        assert!(!is_our_preview("", 4242, "npm run dev") && !is_our_preview(ours, 4242, ""), "no line or no command proves nothing");
    }
    #[test]
    fn sniffs_vites_coloured_url() {
        let log = "  \x1b[32m➜\x1b[39m  \x1b[1mLocal\x1b[22m:   \x1b[36mhttp://localhost:\x1b[1m5174\x1b[22m/\x1b[39m\n";
        assert_eq!(sniff_url(log).as_deref(), Some("http://localhost:5174/"));
        assert_eq!(sniff_url("You can now view app in the browser.\n  Local: http://localhost:3000\n").as_deref(), Some("http://localhost:3000"));
        assert_eq!(sniff_url("ready - started server on 0.0.0.0:3000, url: http://0.0.0.0:3000").as_deref(), Some("http://localhost:3000"));
        assert_eq!(sniff_url("compiling…"), None);
        assert_eq!(strip_ansi("a\x1b[1mb\x1b[0mc"), "abc");
    }

    #[test]
    fn a_dialog_that_could_not_open_is_unavailable_but_a_cancel_is_only_a_cancel() {
        assert_eq!(classify_dialog(true, "/home/me/proj\n", ""), DialogOutcome::Picked("/home/me/proj".into()));
        assert_eq!(classify_dialog(false, "", ""), DialogOutcome::Cancelled, "zenity Cancel: exit 1, silent");
        assert_eq!(classify_dialog(false, "", "Gtk-WARNING **: cannot open display: \n"), DialogOutcome::Unavailable);
        assert_eq!(classify_dialog(false, "", "Failed to connect to Wayland display"), DialogOutcome::Unavailable);
        assert_eq!(classify_dialog(true, "\n", ""), DialogOutcome::Cancelled, "success with no path chose nothing");
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    /// THE QA MUST LOOK AT THIS AGENT'S FRONTEND (operator 2026-09-30, run 265 on "ministore / cheeky double": the QA took the one preview that was running - another agent's - and
    /// judged a search field against a page that did not have it).
    #[test]
    fn a_preview_counts_only_for_the_worktree_it_serves() {
        let reg = PreviewRegistry::default();
        let entry = |cwd: &str, url: &str, exited: Option<i32>| PreviewEntry { url: Some(url.into()), cwd: cwd.into(), log: String::new(), exited, cmd: "npm run dev".into(), pid: None };
        let (a, b) = (std::env::temp_dir().join("antiloki-prev-a"), std::env::temp_dir().join("antiloki-prev-b"));
        std::fs::create_dir_all(&a).unwrap(); std::fs::create_dir_all(&b).unwrap();
        let (a, b) = (a.to_string_lossy().to_string(), b.to_string_lossy().to_string());
        reg.entries.lock().unwrap().insert("agent-a".into(), entry(&a, "http://localhost:5175/", None));
        reg.entries.lock().unwrap().insert("agent-b".into(), entry(&b, "http://localhost:5177/", None));
        reg.entries.lock().unwrap().insert("gone".into(), entry(&a, "http://localhost:5999/", Some(1)));
        assert_eq!(reg.live_urls().len(), 2, "globally there are two previews");
        assert_eq!(reg.live_urls_for(&a), vec!["http://localhost:5175/".to_string()], "A's QA sees A's preview, not B's and not a dead one");
        assert_eq!(reg.live_urls_for(&b), vec!["http://localhost:5177/".to_string()]);
        assert!(reg.live_urls_for("/somewhere/else").is_empty(), "a worktree with no preview of its own gets none - the QA boots the box's own dev server instead");
        assert!(reg.live_urls_for(&format!("{a}/")).len() <= 1);
    }
    #[test]
    fn detect_dev_script_reads_the_dev_script_then_start_then_nothing() {
        assert_eq!(detect_dev_script(r#"{"scripts":{"dev":"vite","start":"node ."}}"#).as_deref(), Some("npm run dev"));
        assert_eq!(detect_dev_script(r#"{"scripts":{"start":"node ."}}"#).as_deref(), Some("npm start"));
        assert_eq!(detect_dev_script(r#"{"scripts":{"build":"tsc"}}"#), None); assert_eq!(detect_dev_script(r#"{"scripts":{"dev":" "}}"#), None); assert_eq!(detect_dev_script("not json"), None);
    }
}
