//! THE PHONE, AND ANY OTHER DEVICE (operator 2026-11-10, asked three times: "a tunnel to use our cell phone like Claude Code does, using antiloki" - "antiloki" being the dictation of the word the earlier notes carried).
//!
//! The serve has always answered only to its own machine (guard.rs). This module opens it to a device the person has PAIRED, and to nobody else:
//!
//! * TWO DOORS. A request is LOCAL when it comes from a loopback peer, under a loopback `Host`, with no proxy headers. Anything else is REMOTE: a LAN address, a tunnel (it arrives from loopback but under its own `Host` and with
//!   `X-Forwarded-*`/`CF-*` headers), a page on the internet rebinding its name to 127.0.0.1. A remote request must carry a device token or it is refused (401). The app's own files (a GET that is not `/api`) and the pairing
//!   claim are the only things a stranger may reach, so the phone can load the page and trade a code for a token.
//! * PAIRING. The person asks (on this machine) for a one-time code, good for five minutes; the phone claims it with the QR's link and gets a long random token, kept as a cookie (so the event streams carry it too) and returned
//!   for a client that wants a header. The serve stores only the token's hash. Claims are rate-limited. A device is listed, named, and can be revoked at once.
//! * A PAIRED DEVICE IS THE OWNER'S, so it reaches the whole API, with three exceptions: it cannot manage pairing (`/api/v1/remote/*`), it cannot write from another origin than the page it came from, and when it is revoked it is out on its next request.
//! * REACHING THE MACHINE. `enable` opens a second listener on every address of the LAN (a port kept between runs), the way to the phone on the same Wi-Fi or a VPN such as Tailscale. `tunnel/start` runs `cloudflared` (when it is installed)
//!   for a public https address; nothing is installed or started by itself. Both are off until the person turns them on.
//! * THE APP. A phone needs the page: when `ANTILOKI_FE_DIR` names the built app, the serve serves its files (with the single-page fallback) on any door; without it the phone is told the app is not installed here.
use axum::{
    body::Body,
    extract::{ConnectInfo, Request},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const COOKIE: &str = "antiloki_device";
const CODE_TTL_MS: i64 = 5 * 60_000;
const MAX_CODES: usize = 5;
const MAX_FAILS: usize = 10;
const FAIL_WINDOW_MS: i64 = 60_000;
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

// ── pure ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Door { Local, Remote }

/// Which door a request came through: LOCAL only when the peer is loopback (or unknown: an in-process call), the `Host` is a loopback name and no proxy header is present. Pure - tested.
pub fn door(peer: Option<IpAddr>, headers: &HeaderMap) -> Door {
    let peer_ok = peer.map_or(true, |p| p.is_loopback());
    let host_ok = headers.get(header::HOST).map_or(true, |h| h.to_str().map_or(false, crate::guard::host_allowed));
    let proxied = ["x-forwarded-for", "x-forwarded-host", "x-forwarded-proto", "forwarded", "cf-connecting-ip", "cf-ray", "x-real-ip", "true-client-ip", "via"].iter().any(|k| headers.contains_key(*k));
    if peer_ok && host_ok && !proxied { Door::Local } else { Door::Remote }
}

/// The device token a request carries: `Authorization: Bearer …`, else the cookie. Pure.
pub fn token_of(headers: &HeaderMap) -> Option<String> {
    if let Some(a) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) { if let Some(t) = a.strip_prefix("Bearer ").or_else(|| a.strip_prefix("bearer ")) { let t = t.trim(); if !t.is_empty() { return Some(t.to_string()); } } }
    let c = headers.get(header::COOKIE)?.to_str().ok()?;
    c.split(';').filter_map(|p| p.trim().split_once('=')).find(|(k, _)| *k == COOKIE).map(|(_, v)| v.trim().to_string()).filter(|v| !v.is_empty())
}
pub fn token_hash(token: &str) -> String { hex::encode(Sha256::digest(token.as_bytes())) }

pub enum Verdict { Pass, Unauthorized(&'static str), Forbidden(&'static str) }
/// What a REMOTE request may do, given whether it carried a valid device token. Pure - tested.
pub fn remote_verdict(method: &Method, path: &str, headers: &HeaderMap, authed: bool) -> Verdict {
    let safe = matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    if path == "/api/v1/remote/pair/claim" && *method == Method::POST { return Verdict::Pass; }
    if path == "/api/v1/remote/ping" && safe { return Verdict::Pass; }
    // a team webhook proves itself with its own token (BL-7): it only checks the token and queues the call, the team's consent and daily caps decide the run
    if path.starts_with("/api/v1/hooks/team/") && *method == Method::POST { return Verdict::Pass; }
    // the app's own files: a stranger may load the page that pairs
    if !path.starts_with("/api/") && matches!(*method, Method::GET | Method::HEAD) { return Verdict::Pass; }
    if !authed { return Verdict::Unauthorized("this device is not paired: open the pairing link from the machine, or scan its QR code"); }
    if path.starts_with("/api/v1/remote/") && path != "/api/v1/remote/me" { return Verdict::Forbidden("a paired device cannot manage pairing: do it on the machine"); }
    if !safe {
        // a write must come from the page this serve gave the device, never from another site
        if let Some(o) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
            let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
            let oh = o.split_once("://").map(|(_, r)| r).unwrap_or(o);
            if !host.is_empty() && !oh.eq_ignore_ascii_case(host) { return Verdict::Forbidden("a page from another origin may not change anything here"); }
        }
    }
    Verdict::Pass
}

/// A pairing code: eight letters and digits without the look-alikes, read aloud or typed if the QR is out of reach. `bytes` are random. Pure over them.
pub fn code_from(bytes: &[u8]) -> String { bytes.iter().take(8).map(|b| CODE_ALPHABET[(*b as usize) % CODE_ALPHABET.len()] as char).collect() }

/// The mime type of a file of the app, by its extension. Pure.
pub fn mime_of(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8", "js" | "mjs" => "text/javascript; charset=utf-8", "css" => "text/css; charset=utf-8", "json" | "map" | "webmanifest" => "application/json",
        "svg" => "image/svg+xml", "png" => "image/png", "jpg" | "jpeg" => "image/jpeg", "gif" => "image/gif", "webp" => "image/webp", "ico" => "image/x-icon", "woff2" => "font/woff2", "woff" => "font/woff", "ttf" => "font/ttf", "txt" => "text/plain; charset=utf-8", "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}
/// The file of the app a request path names, inside `root`, or None when it would leave `root` (no `..`, no absolute path) - the single-page fallback is the caller's. Pure over the strings.
pub fn app_file(root: &Path, url_path: &str) -> Option<PathBuf> {
    let rel = url_path.trim_start_matches('/'); if rel.split('/').any(|s| s == ".." || s.contains('\\') || s.contains('\0')) { return None; }
    Some(if rel.is_empty() { root.join("index.html") } else { root.join(rel) })
}
/// A network interface that only containers and virtual machines on this machine see (docker0, br-..., veth..., virbr0, cni0, podman...): a phone can never reach an address on it, and offering one as "the address" sent the phone nowhere. Pure.
pub fn is_container_iface(name: &str) -> bool { ["docker", "br-", "veth", "virbr", "cni", "flannel", "podman", "lxc", "lxd", "vboxnet", "vmnet"].iter().any(|p| name.starts_with(p)) }
/// The addresses of this machine on its networks, from `ip -o -4 addr` text: every IPv4 that is not loopback and not on a container interface. Pure.
pub fn lan_ips(ip_output: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for l in ip_output.lines().filter(|l| !l.split_whitespace().any(|w| is_container_iface(w.trim_end_matches(':')))) { if let Some(i) = l.split_whitespace().position(|w| w == "inet") { if let Some(a) = l.split_whitespace().nth(i + 1) { let ip = a.split('/').next().unwrap_or(""); if !ip.is_empty() && !ip.starts_with("127.") && !ip.starts_with("169.254.") && !out.iter().any(|x| x == ip) { out.push(ip.to_string()); } } } }
    out
}
/// The `https://….trycloudflare.com` address in a cloudflared log line, if it holds one. Pure.
pub fn tunnel_url_in(line: &str) -> Option<String> {
    let at = line.find("https://")?; let rest = &line[at..];
    let end = rest.find(|c: char| c.is_whitespace() || c == '|' || c == '"' || c == '\'').unwrap_or(rest.len()); let u = rest[..end].trim_end_matches(['.', ',']);
    let host = u.trim_start_matches("https://"); (host.ends_with(".trycloudflare.com") && !host.starts_with("api.")).then(|| u.to_string())
}

// ── state ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Device { pub id: String, pub name: String, pub hash: String, pub created_ms: i64, pub last_seen_ms: i64, pub revoked: bool }
impl Device { fn to_json(&self) -> Value { json!({ "id": self.id, "name": self.name, "created_ms": self.created_ms, "last_seen_ms": self.last_seen_ms, "revoked": self.revoked }) } }

struct Tunnel { url: String, child: tokio::process::Child }
#[derive(Default)]
struct State {
    dir: PathBuf, devices: Vec<Device>, codes: HashMap<String, i64>, fails: VecDeque<i64>, enabled: bool, port: u16, /** the port a phone is paired to: kept between runs, never replaced by a one-run fallback */ kept: u16, main_port: u16,
    lan: Option<tokio::task::JoinHandle<()>>, tunnel: Option<Tunnel>,
}
fn st() -> &'static Mutex<State> { static S: OnceLock<Mutex<State>> = OnceLock::new(); S.get_or_init(Default::default) }
fn lock() -> std::sync::MutexGuard<'static, State> { st().lock().unwrap_or_else(|p| p.into_inner()) }
static ROUTER: OnceLock<Router> = OnceLock::new();
fn now() -> i64 { crate::cli::now_ms() }

fn files(dir: &Path) -> (PathBuf, PathBuf) { (dir.join("devices.json"), dir.join("remote.json")) }
fn load_devices(dir: &Path) -> Vec<Device> {
    let v: Value = std::fs::read_to_string(files(dir).0).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    v.as_array().into_iter().flatten().filter_map(|d| Some(Device { id: d["id"].as_str()?.to_string(), name: d["name"].as_str().unwrap_or("device").to_string(), hash: d["hash"].as_str()?.to_string(), created_ms: d["created_ms"].as_i64().unwrap_or(0), last_seen_ms: d["last_seen_ms"].as_i64().unwrap_or(0), revoked: d["revoked"].as_bool().unwrap_or(false) })).collect()
}
fn save(s: &State) {
    if s.dir.as_os_str().is_empty() { return; }
    let _ = std::fs::create_dir_all(&s.dir); let (d, r) = files(&s.dir);
    let devs: Vec<Value> = s.devices.iter().map(|x| json!({ "id": x.id, "name": x.name, "hash": x.hash, "created_ms": x.created_ms, "last_seen_ms": x.last_seen_ms, "revoked": x.revoked })).collect();
    let w = |p: &Path, v: &Value| { let tmp = p.with_extension("tmp"); if std::fs::write(&tmp, v.to_string()).is_ok() { let _ = std::fs::rename(&tmp, p); } };
    w(&d, &Value::Array(devs)); w(&r, &json!({ "enabled": s.enabled, "port": if s.kept != 0 { s.kept } else { s.port } }));
}

/// Wire the module at startup: its folder (beside the database), the port the serve listens on, and the router the second listener will serve. Brings the LAN listener back if it was on.
pub fn init(dir: PathBuf, main_port: u16, router: Router) {
    let _ = ROUTER.set(router);
    let cfg: Value = std::fs::read_to_string(files(&dir).1).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    { let mut s = lock(); s.devices = load_devices(&dir); s.dir = dir; s.main_port = main_port; s.port = cfg["port"].as_u64().unwrap_or(0) as u16; s.kept = s.port; s.enabled = false; }
    if cfg["enabled"].as_bool() == Some(true) { tokio::spawn(async { let _ = enable().await; }); }
}

fn local_addresses() -> Vec<String> {
    let out = crate::proc::cmd("ip").args(["-o", "-4", "addr", "show"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    let mut ips = lan_ips(&out);
    if ips.is_empty() { if let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") { if sock.connect("192.0.2.1:9").is_ok() { if let Ok(a) = sock.local_addr() { let ip = a.ip().to_string(); if !ip.starts_with("0.") && !ip.starts_with("127.") { ips.push(ip); } } } } }
    ips
}

/// Open the LAN listener (every address of this machine), on the port kept from last time when it is free. Returns the addresses to reach it.
pub async fn enable() -> Result<Vec<String>, String> {
    let router = ROUTER.get().ok_or("the remote door is not ready")?.clone();
    // ANTILOKI_LAN_PORT: a test harness asks for any free port (0), so a stack that outlives its run can never hold the port a phone is paired to (2026-11-12: a left-over e2e serve held 41932 and the phone showed it)
    let want = std::env::var("ANTILOKI_LAN_PORT").ok().and_then(|v| v.trim().parse::<u16>().ok()).unwrap_or_else(|| { let p = lock().kept; if p == 0 { DEFAULT_PORT } else { p } });
    // THE PORT IS KEPT (2026-11-18: the phone showed a very old antiloki): a phone is paired to one address, so when the kept port cannot be had this time, another one is used for THIS run only and the kept port is NOT overwritten; the reason goes to the log
    let (listener, fell_back) = match tokio::net::TcpListener::bind(("0.0.0.0", want)).await { Ok(l) => (l, false), Err(e) if want != 0 => { eprintln!("antiloki-serve: the LAN door could not take port {want} ({e}); using another port for this run, {want} stays the kept one"); (tokio::net::TcpListener::bind(("0.0.0.0", 0)).await.map_err(|e| e.to_string())?, true) } Err(e) => return Err(e.to_string()) };
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let handle = tokio::spawn(async move { let _ = axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await; });
    { let mut s = lock(); if let Some(old) = s.lan.take() { old.abort(); } s.lan = Some(handle); s.enabled = true; s.port = port; if !fell_back { s.kept = port; } save(&s); }
    Ok(addresses())
}
pub fn disable() { let mut s = lock(); if let Some(h) = s.lan.take() { h.abort(); } s.enabled = false; save(&s); }
fn addresses() -> Vec<String> { let s = lock(); if !s.enabled || s.port == 0 { return vec![]; } let p = s.port; drop(s); local_addresses().into_iter().map(|ip| format!("http://{ip}:{p}")).collect() }

/// The port the LAN door opens on the first time: the same one every time, so the firewall rule the person adds once keeps working (a random port each run could never be opened ahead).
pub const DEFAULT_PORT: u16 = 41932;
/// The folder this module (and machines.rs) keeps its files in.
pub fn dir() -> PathBuf { lock().dir.clone() }
/// The app's folder, when the serve was given one.
pub fn fe_dir() -> Option<PathBuf> { std::env::var("ANTILOKI_FE_DIR").ok().filter(|d| !d.trim().is_empty()).map(PathBuf::from).filter(|p| p.join("index.html").is_file()) }
/// WHICH BUILD OF THE APP THIS SERVE HANDS OUT (operator 2026-11-11: "the phone showed a completely different version of antiloki, old, strange"): the name of the entry script in the app's index.html (`index-Bhn54EIs.js`: its hash changes
/// with every build). The page reads its own entry script's name and compares; a page that differs from what the serve now hands out says so. None when there are no app files. Pure over the html - tested.
pub fn entry_of(index_html: &str) -> Option<String> {
    let at = index_html.find("/assets/index-")?; let rest = &index_html[at + 8..];
    let end = rest.find(".js")? + 3; let name = &rest[..end]; (name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))).then(|| name.to_string())
}
pub fn fe_build() -> Option<String> { let d = fe_dir()?; entry_of(&std::fs::read_to_string(d.join("index.html")).ok()?) }
/// A file of the app for a GET that is not the API, or None (the caller answers 404). Unknown paths without an extension are the single-page app's own routes.
pub fn serve_app(path: &str) -> Option<Response> {
    let root = fe_dir()?; let want = app_file(&root, path)?;
    let root_c = std::fs::canonicalize(&root).ok()?;
    let pick = |p: &Path| -> Option<PathBuf> { let c = std::fs::canonicalize(p).ok()?; (c.starts_with(&root_c) && c.is_file()).then_some(c) };
    let file = pick(&want).or_else(|| if path.rsplit('/').next().map_or(true, |f| !f.contains('.')) { pick(&root.join("index.html")) } else { None })?;
    let bytes = std::fs::read(&file).ok()?;
    let cache = if file.to_string_lossy().contains("/assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    Some(Response::builder().status(StatusCode::OK).header(header::CONTENT_TYPE, mime_of(&file.to_string_lossy())).header(header::CACHE_CONTROL, cache).body(Body::from(bytes)).unwrap())
}

// ── the gate, called by guard::guard for every request ───────────────────────────────────────────────────────────────────────────────────────────────────────

/// Decide a request that came through the REMOTE door. `Ok(())` = pass (a device that was used is marked as seen); `Err` = the answer.
pub fn gate_remote(method: &Method, path: &str, headers: &HeaderMap) -> Result<(), Response> {
    let id = token_of(headers).map(|t| token_hash(&t)).and_then(|h| { let mut s = lock(); let n = now(); s.devices.iter_mut().find(|d| d.hash == h && !d.revoked).map(|d| { if n - d.last_seen_ms > 60_000 { d.last_seen_ms = n; } d.id.clone() }) });
    match remote_verdict(method, path, headers, id.is_some()) {
        Verdict::Pass => Ok(()),
        Verdict::Unauthorized(m) => Err((StatusCode::UNAUTHORIZED, Json(json!({ "statusCode": 401, "message": m, "pairing": true }))).into_response()),
        Verdict::Forbidden(m) => Err((StatusCode::FORBIDDEN, Json(json!({ "statusCode": 403, "message": m }))).into_response()),
    }
}

// ── the doors of the API ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

fn reply(code: StatusCode, v: Value) -> Response { (code, Json(v)).into_response() }
fn peer_of(req: &Request) -> Option<IpAddr> { req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip()) }
/// Admin calls come from this machine only (the gate already refuses a remote one that is not a paired device's; this refuses a paired device too).
pub(crate) fn local_only(req: &Request) -> Option<Response> { if door(peer_of(req), req.headers()) == Door::Local { None } else { Some(reply(StatusCode::FORBIDDEN, json!({ "ok": false, "error": "pairing is managed on the machine itself" }))) } }

fn tunnel_json(s: &State) -> Value { s.tunnel.as_ref().map(|t| json!({ "provider": "cloudflared", "url": t.url })).unwrap_or(Value::Null) }
fn which(bin: &str) -> bool { std::env::var_os("PATH").map_or(false, |p| std::env::split_paths(&p).any(|d| d.join(bin).is_file())) }
/// Does a ufw rules file already let this TCP port in? Pure - tested.
pub fn ufw_allows(rules: &str, port: u16) -> bool { rules.lines().any(|l| { let l = l.trim(); l.starts_with("-A ufw-user-input") && l.ends_with("-j ACCEPT") && !l.contains("-i docker") && !l.contains("-i br-") && l.split_whitespace().collect::<Vec<_>>().windows(2).any(|w| w[0] == "--dport" && w[1] == port.to_string()) && (l.contains("-p tcp") || !l.contains("-p ")) }) }
/// The command that lets the phone's network in on this port, for the firewall that is on: from this machine's own network only (`192.168.1.0/24`), never the whole world. Pure - tested.
pub fn ufw_command(port: u16, ip: &str) -> String { let net = ip.rsplitn(2, '.').nth(1).map(|n| format!("{n}.0/24")); match net { Some(n) if ip.split('.').count() == 4 => format!("sudo ufw allow from {n} to any port {port} proto tcp"), _ => format!("sudo ufw allow {port}/tcp") } }
/// THE FIREWALL IN THE WAY (found 2026-11-11 on the operator's machine: ufw with every incoming connection dropped made the phone load forever: the page never even failed). `{kind, active, open, command}` or null when none is on.
fn firewall_json(port: u16, ips: &[String]) -> Value {
    if port == 0 { return Value::Null; }
    let ufw_on = std::fs::read_to_string("/etc/ufw/ufw.conf").map_or(false, |t| t.lines().any(|l| l.trim().eq_ignore_ascii_case("ENABLED=yes")));
    if ufw_on {
        // the rules file is readable by anyone on most systems; if it is not, the person is told to run the command anyway (it is harmless when the rule already exists)
        let open = std::fs::read_to_string("/etc/ufw/user.rules").map_or(false, |t| ufw_allows(&t, port));
        let ip = ips.first().map(String::as_str).unwrap_or("");
        return json!({ "kind": "ufw", "active": true, "open": open, "command": ufw_command(port, ip) });
    }
    let fwd = crate::proc::cmd("systemctl").args(["is-active", "firewalld"]).output().ok().map_or(false, |o| String::from_utf8_lossy(&o.stdout).trim() == "active");
    if fwd { return json!({ "kind": "firewalld", "active": true, "open": false, "command": format!("sudo firewall-cmd --add-port={port}/tcp") }); }
    Value::Null
}
fn status_json() -> Value {
    let addrs = addresses(); let s = lock();
    json!({ "ok": true, "enabled": s.enabled, "port": s.port, "addresses": addrs, "tunnel": tunnel_json(&s), "devices": s.devices.iter().map(Device::to_json).collect::<Vec<_>>(), "app": fe_dir().is_some(), "cloudflared": which("cloudflared"), "fe_build": fe_build(), "firewall": if s.enabled { firewall_json(s.port, &local_addresses()) } else { Value::Null } })
}

pub async fn status(req: Request) -> Response { if let Some(r) = local_only(&req) { return r; } reply(StatusCode::OK, status_json()) }
pub async fn enable_http(req: Request) -> Response { if let Some(r) = local_only(&req) { return r; } match enable().await { Ok(_) => reply(StatusCode::OK, status_json()), Err(e) => reply(StatusCode::OK, json!({ "ok": false, "error": e })) } }
pub async fn disable_http(req: Request) -> Response { if let Some(r) = local_only(&req) { return r; } disable(); reply(StatusCode::OK, status_json()) }
pub async fn ping() -> Response { reply(StatusCode::OK, json!({ "ok": true, "antiloki": true })) }

/// `POST remote/pair/start`: a one-time code (and the links that carry it, for the QR).
pub async fn pair_start(req: Request) -> Response {
    if let Some(r) = local_only(&req) { return r; }
    use rand::RngCore; let mut b = [0u8; 8]; rand::thread_rng().fill_bytes(&mut b); let code = code_from(&b);
    let (base, tunnel) = { let mut s = lock(); let n = now(); s.codes.retain(|_, exp| *exp > n); if s.codes.len() >= MAX_CODES { let oldest = s.codes.iter().min_by_key(|(_, e)| **e).map(|(k, _)| k.clone()); if let Some(k) = oldest { s.codes.remove(&k); } } s.codes.insert(code.clone(), n + CODE_TTL_MS); (s.port, s.tunnel.as_ref().map(|t| t.url.clone())) };
    let mut links: Vec<Value> = addresses().into_iter().map(|a| json!({ "label": "same network", "url": format!("{a}/?pair={code}") })).collect();
    if let Some(u) = tunnel { links.insert(0, json!({ "label": "tunnel", "url": format!("{}/?pair={code}", u.trim_end_matches('/')) })); }
    let _ = base;
    reply(StatusCode::OK, json!({ "ok": true, "code": code, "expires_ms": now() + CODE_TTL_MS, "links": links, "app": fe_dir().is_some() }))
}

/// `POST remote/pair/claim {code, name}`: from the device. A good code is spent and becomes a token (a cookie, and in the body).
pub async fn pair_claim(headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); let code = b["code"].as_str().unwrap_or("").trim().to_ascii_uppercase();
    let name: String = b["name"].as_str().unwrap_or("").trim().chars().take(60).collect(); let name = if name.is_empty() { "phone".to_string() } else { name };
    let https = headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()) == Some("https");
    let mut s = lock(); let n = now();
    s.fails.retain(|t| n - *t < FAIL_WINDOW_MS);
    if s.fails.len() >= MAX_FAILS { return reply(StatusCode::TOO_MANY_REQUESTS, json!({ "ok": false, "error": "too many wrong codes: wait a minute" })); }
    s.codes.retain(|_, exp| *exp > n);
    if code.is_empty() || s.codes.remove(&code).is_none() { s.fails.push_back(n); return reply(StatusCode::OK, json!({ "ok": false, "error": "that code is wrong or has expired: ask the machine for a new one" })); }
    use rand::RngCore; let mut raw = [0u8; 32]; rand::thread_rng().fill_bytes(&mut raw); let token = hex::encode(raw);
    let mut idb = [0u8; 4]; rand::thread_rng().fill_bytes(&mut idb); let id = hex::encode(idb);
    s.devices.push(Device { id: id.clone(), name: name.clone(), hash: token_hash(&token), created_ms: n, last_seen_ms: n, revoked: false }); save(&s);
    drop(s);
    let cookie = format!("{COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000{}", if https { "; Secure" } else { "" });
    let mut r = reply(StatusCode::OK, json!({ "ok": true, "token": token, "device_id": id, "name": name }));
    if let Ok(v) = HeaderValue::from_str(&cookie) { r.headers_mut().insert(header::SET_COOKIE, v); }
    r
}

/// `GET remote/me`: who a paired device is (a device that is not paired never gets here: the gate answers 401).
pub async fn me(headers: HeaderMap) -> Response {
    let h = token_of(&headers).map(|t| token_hash(&t)); let s = lock();
    match h.and_then(|h| s.devices.iter().find(|d| d.hash == h && !d.revoked).map(Device::to_json)) { Some(d) => reply(StatusCode::OK, json!({ "ok": true, "device": d })), None => reply(StatusCode::OK, json!({ "ok": true, "device": Value::Null, "local": true })) }
}
pub async fn revoke(req: Request) -> Response {
    if let Some(r) = local_only(&req) { return r; }
    let body = axum::body::to_bytes(req.into_body(), 1 << 16).await.unwrap_or_default(); let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); let id = b["id"].as_str().unwrap_or("");
    let mut s = lock(); match s.devices.iter_mut().find(|d| d.id == id) { Some(d) => { d.revoked = true; save(&s); drop(s); reply(StatusCode::OK, status_json()) } None => reply(StatusCode::OK, json!({ "ok": false, "error": "no such device" })) }
}

/// `POST remote/tunnel/start`: `cloudflared tunnel --url` to this serve, the address read from its log. Refused with the way to get it when it is not installed.
pub async fn tunnel_start(req: Request) -> Response {
    if let Some(r) = local_only(&req) { return r; }
    if lock().tunnel.is_some() { return reply(StatusCode::OK, status_json()); }
    if !which("cloudflared") { return reply(StatusCode::OK, json!({ "ok": false, "error": "cloudflared is not installed on this machine. Install it (https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/) and try again: antiloki starts it for you, nothing runs until then", "install": "cloudflared" })); }
    let port = lock().main_port;
    let mut child = match crate::proc::tokio_cmd("cloudflared").args(["tunnel", "--no-autoupdate", "--url", &format!("http://127.0.0.1:{port}")]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true).spawn() { Ok(c) => c, Err(e) => return reply(StatusCode::OK, json!({ "ok": false, "error": format!("could not start cloudflared: {e}") })) };
    use tokio::io::{AsyncBufReadExt, BufReader};
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    for out in [child.stderr.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>), child.stdout.take().map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>)].into_iter().flatten() {
        let tx = tx.clone(); tokio::spawn(async move { let mut l = BufReader::new(out).lines(); while let Ok(Some(line)) = l.next_line().await { let _ = tx.send(line); } });
    }
    drop(tx);
    let found = tokio::time::timeout(std::time::Duration::from_secs(30), async { while let Some(line) = rx.recv().await { if let Some(u) = tunnel_url_in(&line) { return Some(u); } } None }).await.ok().flatten();
    match found { Some(url) => { lock().tunnel = Some(Tunnel { url, child }); reply(StatusCode::OK, status_json()) } None => { let _ = child.kill().await; reply(StatusCode::OK, json!({ "ok": false, "error": "cloudflared did not give an address in 30 seconds" })) } }
}
pub async fn tunnel_stop(req: Request) -> Response {
    if let Some(r) = local_only(&req) { return r; }
    let t = lock().tunnel.take(); if let Some(mut t) = t { let _ = t.child.kill().await; } reply(StatusCode::OK, status_json())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn h(pairs: &[(&str, &str)]) -> HeaderMap { let mut m = HeaderMap::new(); for (k, v) in pairs { m.insert(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), HeaderValue::from_str(v).unwrap()); } m }
    fn ip(s: &str) -> Option<IpAddr> { Some(s.parse().unwrap()) }

    #[test]
    fn a_request_is_local_only_from_loopback_under_a_loopback_name_with_no_proxy_header() {
        assert_eq!(door(ip("127.0.0.1"), &h(&[("host", "localhost:41931")])), Door::Local);
        assert_eq!(door(None, &h(&[])), Door::Local, "an in-process call has no peer and no host");
        assert_eq!(door(ip("192.168.1.20"), &h(&[("host", "192.168.1.5:41932")])), Door::Remote, "a phone on the Wi-Fi");
        assert_eq!(door(ip("192.168.1.20"), &h(&[("host", "localhost")])), Door::Remote, "a LAN peer cannot be local by SAYING it is localhost");
        assert_eq!(door(ip("127.0.0.1"), &h(&[("host", "random-words.trycloudflare.com")])), Door::Remote, "a tunnel arrives from loopback under its own name");
        assert_eq!(door(ip("127.0.0.1"), &h(&[("host", "localhost"), ("x-forwarded-for", "1.2.3.4")])), Door::Remote, "a proxy header makes it remote even under a loopback name");
        assert_eq!(door(ip("127.0.0.1"), &h(&[("host", "localhost"), ("cf-connecting-ip", "1.2.3.4")])), Door::Remote);
        assert_eq!(door(ip("127.0.0.1"), &h(&[("host", "evil.com")])), Door::Remote, "a page rebinding its name is a stranger");
    }
    #[test]
    fn the_token_is_read_from_a_bearer_header_or_the_cookie_and_stored_only_as_a_hash() {
        assert_eq!(token_of(&h(&[("authorization", "Bearer abc123")])).as_deref(), Some("abc123"));
        assert_eq!(token_of(&h(&[("cookie", "theme=dark; antiloki_device=tok9; x=1")])).as_deref(), Some("tok9"));
        assert_eq!(token_of(&h(&[("cookie", "other=1")])), None); assert_eq!(token_of(&h(&[("authorization", "Basic xx")])), None); assert_eq!(token_of(&h(&[])), None);
        assert_eq!(token_hash("a").len(), 64); assert_ne!(token_hash("a"), token_hash("b")); assert_eq!(token_hash("a"), token_hash("a"));
    }
    #[test]
    fn a_stranger_reaches_the_app_and_the_claim_only_a_paired_device_the_api_and_no_device_the_pairing_admin() {
        let v = |m: Method, p: &str, hd: &[(&str, &str)], authed: bool| match remote_verdict(&m, p, &h(hd), authed) { Verdict::Pass => "pass", Verdict::Unauthorized(_) => "401", Verdict::Forbidden(_) => "403" };
        assert_eq!(v(Method::GET, "/", &[], false), "pass"); assert_eq!(v(Method::GET, "/assets/app.js", &[], false), "pass"); assert_eq!(v(Method::POST, "/api/v1/remote/pair/claim", &[], false), "pass"); assert_eq!(v(Method::POST, "/api/v1/hooks/team/t/form", &[], false), "pass"); assert_eq!(v(Method::GET, "/api/v1/hooks/team/t/form", &[], false), "401"); assert_eq!(v(Method::GET, "/api/v1/remote/ping", &[], false), "pass");
        assert_eq!(v(Method::GET, "/api/v1/cockpit/panes", &[], false), "401"); assert_eq!(v(Method::POST, "/api/v1/cockpit/pty/send", &[], false), "401"); assert_eq!(v(Method::POST, "/", &[], false), "401", "a POST to the page is not a page");
        assert_eq!(v(Method::GET, "/api/v1/cockpit/panes", &[], true), "pass"); assert_eq!(v(Method::POST, "/api/v1/cockpit/pty/send", &[("host", "192.168.1.5:41932"), ("origin", "http://192.168.1.5:41932")], true), "pass");
        assert_eq!(v(Method::POST, "/api/v1/cockpit/pty/send", &[("host", "192.168.1.5:41932"), ("origin", "http://evil.com")], true), "403", "a write from another site, even with the cookie");
        assert_eq!(v(Method::POST, "/api/v1/remote/pair/start", &[], true), "403"); assert_eq!(v(Method::POST, "/api/v1/remote/devices/revoke", &[], true), "403"); assert_eq!(v(Method::GET, "/api/v1/remote/me", &[], true), "pass");
    }
    #[test]
    fn a_code_is_eight_characters_without_look_alikes() {
        let c = code_from(&[0, 1, 2, 3, 4, 250, 251, 252]); assert_eq!(c.len(), 8); assert!(c.bytes().all(|b| CODE_ALPHABET.contains(&b)));
        for bad in ['0', 'O', '1', 'I', 'L', 'l'] { assert!(!CODE_ALPHABET.contains(&(bad as u8)), "{bad}"); }
    }
    #[test]
    fn the_apps_files_are_found_inside_their_folder_and_never_outside_it() {
        let r = Path::new("/app"); assert_eq!(app_file(r, "/"), Some(PathBuf::from("/app/index.html"))); assert_eq!(app_file(r, "/assets/a.js"), Some(PathBuf::from("/app/assets/a.js")));
        for bad in ["/../etc/passwd", "/a/../../x", "/a\\b", "/a/\0"] { assert_eq!(app_file(r, bad), None, "{bad}"); }
        assert_eq!(mime_of("a.js"), "text/javascript; charset=utf-8"); assert_eq!(mime_of("x.woff2"), "font/woff2"); assert_eq!(mime_of("noext"), "application/octet-stream");
    }
    #[test]
    fn a_firewall_rule_for_the_port_is_found_and_the_command_asks_only_for_the_local_network() {
        let rules = "-A ufw-user-input -p tcp --dport 22 -j ACCEPT\n-A ufw-user-input -i docker0 -p tcp --dport 41932 -j ACCEPT\n-A ufw-user-input -p udp --dport 41932 -j ACCEPT\n";
        assert!(!ufw_allows(rules, 41932), "a rule for another interface or for udp does not let a phone in"); assert!(ufw_allows(rules, 22));
        assert!(ufw_allows(&format!("{rules}-A ufw-user-input -p tcp --dport 41932 -j ACCEPT\n"), 41932)); assert!(ufw_allows("-A ufw-user-input -s 192.168.1.0/24 -p tcp --dport 41932 -j ACCEPT", 41932));
        assert_eq!(ufw_command(41932, "192.168.1.107"), "sudo ufw allow from 192.168.1.0/24 to any port 41932 proto tcp"); assert_eq!(ufw_command(41932, ""), "sudo ufw allow 41932/tcp");
    }
    #[test]
    fn the_build_of_the_app_is_the_name_of_its_entry_script() {
        assert_eq!(entry_of(r#"<script type="module" crossorigin src="/assets/index-Bhn54EIs.js"></script>"#).as_deref(), Some("index-Bhn54EIs.js")); assert_eq!(entry_of("<html></html>"), None); assert_eq!(entry_of(r#"src="/assets/index-a b.js""#), None);
    }
    #[test]
    fn the_machines_addresses_and_the_tunnels_address_are_read_from_their_text() {
        let ip_out = "1: lo    inet 127.0.0.1/8 scope host lo\n2: wlan0    inet 192.168.1.5/24 brd 192.168.1.255 scope global wlan0\n3: docker0    inet 172.17.0.1/16 scope global docker0\n4: x inet 169.254.3.4/16 scope link x\n";
        assert_eq!(lan_ips(ip_out), vec!["192.168.1.5"], "docker0 is for containers: a phone cannot reach it");
        assert_eq!(lan_ips("5: tailscale0    inet 100.64.0.7/32 scope global tailscale0\n6: br-6f2c5b11ca09    inet 172.18.0.1/16 scope global br-6f2c5b11ca09\n"), vec!["100.64.0.7"]);
        assert_eq!(tunnel_url_in("2026 INF |  https://quick-brown-fox.trycloudflare.com  |").as_deref(), Some("https://quick-brown-fox.trycloudflare.com"));
        assert_eq!(tunnel_url_in("INF Requesting new quick Tunnel on https://api.trycloudflare.com..."), None); assert_eq!(tunnel_url_in("see https://example.com"), None); assert_eq!(tunnel_url_in("nothing"), None);
    }
}
