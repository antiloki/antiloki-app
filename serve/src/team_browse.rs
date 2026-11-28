//! THE BROWSE TOOL (operator 2026-11-20: "browse node so the team will do what is needed"): a team node that looks at a web page the way a person does, signed in, and brings back what it saw.
//!
//!   `login`    opens a WINDOW the person signs in to (a profile of its own, kept on this machine outside every project) and returns when the window is closed. The team never types a password and never sees one.
//!   `capture`  opens the same profile with no window, visits the listed pages at each listed width and keeps, per page and width: a screenshot, and a reading of the page (the fonts, the colours by area, the
//!              outline of its big blocks with their boxes, the first words of text). A page that sends the visit to a sign-in page is said to be signed out, not captured.
//!
//! It only READS: no typing, no clicks, no forms. It goes only to the domains the node lists. Pure except `run_login` and `run_capture`.
use headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption;
use headless_chrome::{Browser, LaunchOptions, Tab};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub const NAMES: [&str; 2] = ["login", "capture"];

fn slug_ok(s: &str) -> bool { !s.is_empty() && s.len() <= 40 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !s.starts_with('-') }
/// A bare host name: letters, digits, dots and dashes, with a dot (or `localhost`). Pure.
pub fn host_ok(h: &str) -> bool { let h = h.trim(); !h.is_empty() && h.len() <= 100 && !h.starts_with('.') && !h.ends_with('.') && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') && (h.contains('.') || h == "localhost") }
/// The host of an http(s) address, lower case, with no user name inside. Pure.
pub fn host_of(url: &str) -> Option<String> {
    let low = url.trim().to_ascii_lowercase(); let rest = low.strip_prefix("https://").or_else(|| low.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next()?; if host.contains('@') { return None; }
    let h = host.split(':').next()?; if host_ok(h) { Some(h.to_string()) } else { None }
}
/// Is a host one of the listed domains, or a sub-domain of one? Pure.
pub fn host_allowed(host: &str, domains: &[String]) -> bool { let h = host.to_ascii_lowercase(); domains.iter().any(|d| { let d = d.trim().to_ascii_lowercase(); h == d || h.ends_with(&format!(".{d}")) }) }
fn strings(v: &Value) -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect()).unwrap_or_default() }

/// A node may name its TARGET instead of listing the profile, domains and pages: `{ "target": "{ask}" }` reads `<project>/.antiloki/browse-targets/<name>.json` (profile, domains, url, urls, widths, expect), so ONE team serves every app and the app is named when the run starts. Pure over the file.
pub fn target_name_ok(n: &str) -> bool { slug_ok(n.trim()) }
pub fn target_path(root: &str, name: &str) -> PathBuf { Path::new(root).join(".antiloki").join("browse-targets").join(format!("{}.json", name.trim())) }
/// The arguments of a node with its target's file folded in (the file gives profile, domains, url, urls, widths, expect; what the node says itself, `wait_s` and `mark`, is kept). Pure.
pub fn with_target(args: &Value, target: &Value) -> Value {
    let mut a = args.clone(); let m = a.as_object_mut().map(|m| { m.remove("target"); m.clone() }).unwrap_or_default(); let mut out = target.as_object().cloned().unwrap_or_default();
    for (k, v) in m { if k == "wait_s" || k == "mark" { out.insert(k, v); } } Value::Object(out)
}
/// WHAT THE TEAM BUILDS FROM THE CAPTURE (RFC 0016 M4): a target file may say `"output": "free-pane"` (a folder: `pane.json` + `index.html`, installed as a draft the person approves) or `"react"` (a component for antiloki's own front end, the default). Anything else is refused in words. Pure.
pub fn target_output(target: &Value) -> Result<&'static str, String> {
    match target.get("output") { None | Some(Value::Null) => Ok("react"), Some(v) => match v.as_str() { Some("react") => Ok("react"), Some("free-pane") => Ok("free-pane"), _ => Err("`output` is `react` or `free-pane`".into()) } }
}
/// What the node's arguments must be, said in plain words. Pure.
pub fn check_args(name: &str, args: &Value) -> Vec<String> {
    let mut e = vec![];
    if !NAMES.contains(&name) { e.push(format!("a browse tool is `login` or `capture` (not `{name}`)")); return e; }
    // a target is read when the run starts: only that it is named is checked here (the file is checked then)
    if let Some(t) = args["target"].as_str() { if t.trim().is_empty() { e.push("`target` names the app (a file in .antiloki/browse-targets), for example \"{ask}\"".into()); } return e; }
    let profile = args["profile"].as_str().unwrap_or(""); if !slug_ok(profile) { e.push("`profile` names the signed-in window: lower case letters, digits and dashes (for example `linear`)".into()); }
    let domains = strings(&args["domains"]); if domains.is_empty() { e.push("`domains` lists the sites this node may go to (for example [\"linear.app\"])".into()); }
    for d in &domains { if !host_ok(d) { e.push(format!("`{d}` is not a domain name")); } }
    let urls = if name == "login" { args["url"].as_str().map(|u| vec![u.to_string()]).unwrap_or_default() } else { strings(&args["urls"]) };
    if urls.is_empty() { e.push(if name == "login" { "`url` is the page the window opens on".into() } else { "`urls` lists the pages to capture".into() }); }
    for u in &urls { match host_of(u) { None => e.push(format!("`{u}` is not an http(s) address")), Some(h) => { if !host_allowed(&h, &domains) { e.push(format!("`{u}` is outside the listed domains")); } if u.to_ascii_lowercase().starts_with("http://") && h != "localhost" { e.push(format!("`{u}` must be https (http only for localhost)")); } } } }
    if urls.len() > 12 { e.push("at most 12 pages in one capture".into()); }
    if strings(&args["expect"]).len() > 6 { e.push("`expect` lists at most 6 words".into()); }
    if name == "capture" { let w = widths_of(args); if w.is_empty() || args["widths"].as_array().map_or(false, |a| a.len() > 4 || a.iter().any(|x| x.as_u64().map_or(true, |n| !(320..=2560).contains(&n)))) { e.push("`widths` is up to four widths between 320 and 2560 (default 1440, 768, 390)".into()); } }
    e
}
pub fn expect_of(args: &Value) -> Vec<String> { strings(&args["expect"]) }
pub fn widths_of(args: &Value) -> Vec<u32> { let w: Vec<u32> = args["widths"].as_array().map(|a| a.iter().filter_map(|x| x.as_u64()).map(|n| n as u32).collect()).unwrap_or_default(); if w.is_empty() { vec![1440, 768, 390] } else { w } }
/// Where a profile lives: the machine's antiloki home, never a project. Pure.
pub fn profile_dir(profile: &str) -> PathBuf { antiloki_rs::cockpit__helpers_webapps::store_path().with_file_name("browse-profiles").join(profile) }
/// A file-name part for a page address. Pure.
pub fn page_slug(url: &str) -> String { let s: String = url.trim().to_ascii_lowercase().trim_start_matches("https://").trim_start_matches("http://").chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect(); let t = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-"); t.chars().take(60).collect() }
/// Does a page's own address look like a sign-in page (so a capture of it is not the app)? Pure.
pub fn looks_signed_out(final_url: &str, domains: &[String]) -> bool {
    let Some(h) = host_of(final_url) else { return true }; if !host_allowed(&h, domains) { return true; }
    let l = final_url.to_ascii_lowercase(); ["/login", "/signin", "/sign-in", "/sign_in", "/auth/", "/oauth", "/sso"].iter().any(|k| l.contains(k))
}

fn options(headless: bool, width: u32, profile: &Path) -> Result<LaunchOptions<'static>, String> {
    let exe = antiloki_rs::codebase_helpers_browser_capture::find_browser()?;
    std::fs::create_dir_all(profile).map_err(|e| format!("profile folder: {e}"))?;
    LaunchOptions::default_builder().path(Some(exe)).headless(headless).sandbox(!crate::qa_evidence::chrome_needs_no_sandbox()).user_data_dir(Some(profile.to_path_buf()))
        .window_size(Some((width, 900))).idle_browser_timeout(Duration::from_secs(900)).ignore_default_args(vec![std::ffi::OsStr::new("--enable-automation")])
        .args(vec![std::ffi::OsStr::new("--hide-scrollbars"), std::ffi::OsStr::new("--force-device-scale-factor=1"), std::ffi::OsStr::new("--no-first-run"), std::ffi::OsStr::new("--no-default-browser-check"), std::ffi::OsStr::new("--disable-blink-features=AutomationControlled")])
        .build().map_err(|e| format!("browser options: {e}"))
}

/// Open a PLAIN browser window (no automation flags: Google and other sign-ins refuse a window a program drives, "this browser or app may not be secure") and RETURN AT ONCE. The window has a debugging
/// port on this machine only: the capture that comes after attaches to THIS window, because a sign-in that lives in a session cookie never reaches the profile folder and a second browser would start signed out.
/// The window's process id and port are kept beside the profile; the capture closes the window when it is done. Blocking.
pub fn run_login(url: &str, profile: &str, _wait_s: u64, mark: bool) -> Result<Value, String> {
    let dir = profile_dir(profile); std::fs::create_dir_all(&dir).map_err(|e| format!("profile folder: {e}"))?; close_window(profile);
    let exe = antiloki_rs::codebase_helpers_browser_capture::find_browser()?;
    let port = { let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?; l.local_addr().map_err(|e| e.to_string())?.port() };
    let child = crate::proc::cmd(&exe).arg(format!("--user-data-dir={}", dir.display())).arg(format!("--remote-debugging-port={port}")).arg("--remote-debugging-address=127.0.0.1")
        .args(["--no-first-run", "--no-default-browser-check", "--new-window"]).arg(url)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().map_err(|e| format!("could not open the window: {e}"))?;
    let pid = child.id(); let _ = std::fs::write(pid_file(profile), json!({ "pid": pid, "port": port }).to_string());
    std::thread::spawn(move || { let mut c = child; let _ = c.wait(); });   // reap it when it ends, whoever ends it
    for _ in 0..40 { if debugger_url(port).is_some() {
        if mark { keep_marked(profile, port); }   // the mark of antiloki's window on every tab while the person signs in (`mark: false` on the node turns it off, should a sign-in page object)
        return Ok(json!({ "ok": true, "opened": true, "profile": profile, "marked": mark, "note": "sign in in the window, then approve: the next step looks at the app through this same window and closes it" }));
    } std::thread::sleep(Duration::from_millis(250)); }
    Err("the window opened but did not answer on its local port: close it and run the step again".into())
}
/// A connection that watches a window lasts as long as the window: the library drops one after 30 quiet seconds otherwise (found 2026-11-28: tabs opened later had no mark).
pub const LASTING: Duration = Duration::from_secs(86_400);
/// Keep the mark on every tab of the sign-in window for as long as the window lives: each new tab or window gets it once. Runs on a thread of its own; ends when the window does.
fn keep_marked(profile: &str, port: u16) {
    let profile = profile.to_string();
    std::thread::spawn(move || {
        let Some(ws) = debugger_url(port) else { return }; let Ok(browser) = Browser::connect_with_timeout(ws, LASTING) else { return };
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        loop {
            std::thread::sleep(Duration::from_millis(500));
            match window_of(&profile) { Some((pid, p)) if p == port && process_alive(pid) => {} _ => break }
            let tabs: Vec<Arc<Tab>> = browser.get_tabs().lock().map(|t| t.clone()).unwrap_or_default();
            for t in tabs { if seen.insert(t.get_target_id().clone()) { mark_tab(&t); } }
        }
    });
}
fn pid_file(profile: &str) -> PathBuf { profile_dir(profile).with_extension("login.json") }
/// What a window with a debugging port says at `path`. Plain HTTP on 127.0.0.1.
fn local_json(port: u16, path: &str) -> Option<Value> {
    use std::io::{Read, Write};
    let mut c = std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(800)).ok()?; c.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    c.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes()).ok()?;
    // the browser keeps the connection open: read until the body its Content-Length promises has come
    let mut buf: Vec<u8> = vec![]; let mut chunk = [0u8; 4096];
    loop {
        let n = c.read(&mut chunk).ok()?; if n == 0 { break; } buf.extend_from_slice(&chunk[..n]);
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") { let head = String::from_utf8_lossy(&buf[..at]).to_ascii_lowercase(); let want = head.lines().find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok())).unwrap_or(0); let body = &buf[at + 4..]; if want > 0 && body.len() >= want { return serde_json::from_slice(&body[..want]).ok(); } }
        if buf.len() > 600_000 { return None; }
    }
    None
}
/// The address a program attaches to on a window with a debugging port, when that window answers.
fn debugger_url(port: u16) -> Option<String> { local_json(port, "/json/version")?["webSocketDebuggerUrl"].as_str().map(String::from) }
/// The ids of the window's tabs, asked of the browser itself: it answers even when a page behind a pop-up does not.
pub fn tab_ids(profile: &str) -> Option<Vec<String>> { let (_, port) = window_of(profile)?; Some(local_json(port, "/json/list")?.as_array()?.iter().filter_map(|t| t["id"].as_str().map(String::from)).collect()) }
/// The window's ordinary pages, as the browser lists them: id and address. Frames, workers and the browser's own parts are not pages.
pub fn pages(profile: &str) -> Option<Vec<(String, String)>> { let (_, port) = window_of(profile)?; Some(local_json(port, "/json/list")?.as_array()?.iter().filter(|t| t["type"] == "page").filter_map(|t| Some((t["id"].as_str()?.to_string(), t["url"].as_str()?.to_string()))).collect()) }
fn window_of(profile: &str) -> Option<(u32, u16)> { let v: Value = serde_json::from_str(&std::fs::read_to_string(pid_file(profile)).ok()?).ok()?; Some((v["pid"].as_u64()? as u32, v["port"].as_u64()? as u16)) }
/// The debugger address of a profile's window, when that window is open and answering.
pub fn live_window(profile: &str) -> Option<String> { window_of(profile).and_then(|(pid, port)| if process_alive(pid) { debugger_url(port) } else { None }) }
/// Is the process of this profile's window still there?
pub fn window_process_runs(profile: &str) -> bool { window_of(profile).is_some_and(|(pid, _)| process_alive(pid)) }
/// A browser already running on this profile whose record was lost: take it back. A second browser on one profile hands its page to the first and ends, which looked like a window that would not open. Linux.
pub fn adopt_window(profile: &str) {
    if window_process_runs(profile) { return; }
    let Some(pid) = std::fs::read_link(profile_dir(profile).join("SingletonLock")).ok().and_then(|l| l.to_string_lossy().rsplit('-').next().and_then(|p| p.parse::<u32>().ok())) else { return };
    let Ok(cmd) = std::fs::read(format!("/proc/{pid}/cmdline")) else { return };
    let cmd = String::from_utf8_lossy(&cmd).replace('\0', " "); let dir = format!("--user-data-dir={}", profile_dir(profile).display());
    if !cmd.contains(&dir) { return; }
    if let Some(port) = cmd.split("--remote-debugging-port=").nth(1).and_then(|r| r.split(' ').next()).and_then(|p| p.parse::<u16>().ok()) { let _ = std::fs::write(pid_file(profile), json!({ "pid": pid, "port": port }).to_string()); }
}
/// Close the sign-in window of a profile if one is open, the polite way (so the browser saves what it keeps), and wait a little for it to end.
pub fn close_window(profile: &str) {
    let Some((pid, _)) = window_of(profile) else { return }; let _ = std::fs::remove_file(pid_file(profile));
    let _ = if cfg!(windows) { crate::proc::cmd("taskkill").args(["/PID", &pid.to_string()]).output() } else { crate::proc::cmd("kill").args(["-TERM", &pid.to_string()]).output() };
    for _ in 0..40 { std::thread::sleep(Duration::from_millis(250)); if !process_alive(pid) { break; } }
    std::thread::sleep(Duration::from_millis(500));
}
fn process_alive(pid: u32) -> bool { if cfg!(windows) { crate::proc::cmd("tasklist").args(["/FI", &format!("PID eq {pid}")]).output().map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string())).unwrap_or(false) } else { std::path::Path::new(&format!("/proc/{pid}")).exists() } }

/// A page that is a sign-in wall and not the app, said from what it shows (Linear draws its wall at the address that was asked for, so the address alone cannot tell). Pure.
pub fn looks_like_login_wall(page_text: &str) -> bool {
    let t = page_text.to_ascii_lowercase(); let short = t.chars().count() < 1200;
    short && ["log in to", "sign in to", "continue with google", "continue with email", "log in with passkey", "sign in with google", "enter your password"].iter().any(|k| t.contains(k))
}
/// Did the visit end somewhere else than where it was sent, on the site's front page (a marketing page standing in for an app that wanted a sign-in)? Pure.
pub fn landed_on_front_page(asked: &str, now: &str) -> bool {
    let path = |u: &str| { let l = u.trim(); let rest = l.split("://").nth(1).unwrap_or(l); let p = rest.find('/').map(|i| &rest[i..]).unwrap_or("/"); p.split(['?', '#']).next().unwrap_or("/").trim_end_matches('/').to_string() };
    !path(asked).is_empty() && path(now).is_empty()
}
/// Is the page what the node expects (it lists words the app's own screen has, such as "My issues")? No list: anything goes. Pure.
pub fn has_expected(page_text: &str, expect: &[String]) -> bool { expect.is_empty() || { let t = page_text.to_ascii_lowercase(); expect.iter().any(|e| t.contains(&e.to_ascii_lowercase())) } }

/// THE MARK OF ANTILOKI'S CONTROL (the operator 2026-11-20: "make it like claude with the borders and mouse pointer with the antiloki logo a."): a pulsing ember border around the page, a badge, and a pointer marked "a." that glides to where the work is.
/// It lives in a closed shadow root on the root element (not the body, so a reading of the page never sees it) and is hidden while a screenshot is taken. Installed on every page a controlled tab loads.
const MARK_JS: &str = r##"(function(){ if (window.__antilokiMark) return; window.__antilokiMark = true;
  try { CSS.registerProperty({ name: '--antiloki-ang', syntax: '<angle>', inherits: false, initialValue: '0deg' }); } catch (e) {}
  function build(){ var root = document.documentElement; if (!root) { setTimeout(build, 30); return; }
    var host = document.createElement('div'); host.id = '__antiloki_mark'; host.style.cssText = 'all:initial;position:fixed;inset:0;z-index:2147483647;pointer-events:none;display:block';
    var sh = host.attachShadow({ mode: 'closed' });
    var RING = 'conic-gradient(from var(--antiloki-ang),#3ecf8e,#4d8dff,#e08fd0,#f5a524,#3ecf8e)';
    var MASK = '-webkit-mask:linear-gradient(#000 0 0) content-box,linear-gradient(#000 0 0);-webkit-mask-composite:xor;mask-composite:exclude';
    sh.innerHTML = '<style>@keyframes turn{to{--antiloki-ang:360deg}}'
      + '.ring{position:absolute;inset:0;border-radius:inherit;pointer-events:none;background:' + RING + ';animation:turn 3.2s linear infinite;' + MASK + '}'
      + '.b{position:fixed;inset:0}.b .ring{padding:3px}'
      + '.glow{position:fixed;inset:0;filter:blur(14px);opacity:.55;pointer-events:none}.glow .ring{padding:6px}'
      + '.t{position:fixed;top:10px;left:50%;transform:translateX(-50%);display:flex;align-items:center;gap:8px;padding:5px 12px 5px 8px;border-radius:999px;background:rgba(19,19,22,.78);color:#fff;font:600 12px/1 system-ui,sans-serif}'
      + '.t .ring{padding:1.5px;border-radius:999px}.t b{font:700 14px/1 system-ui,sans-serif;letter-spacing:-.5px;color:#fff}'
      + '.c{position:fixed;left:60%;top:40%;transition:left .7s cubic-bezier(.2,.7,.2,1),top .7s cubic-bezier(.2,.7,.2,1);filter:drop-shadow(0 2px 6px rgba(0,0,0,.5))}'
      + '.arrow{width:20px;height:24px;background:' + RING + ';animation:turn 3.2s linear infinite;-webkit-mask:url("data:image/svg+xml;utf8,<svg xmlns=\'http://www.w3.org/2000/svg\' viewBox=\'0 0 18 22\'><path d=\'M1.5 1.2 L1.5 17.2 L5.6 13.4 L8.4 20 L11 18.9 L8.2 12.4 L13.8 12.4 Z\'/></svg>") no-repeat center/contain;mask:url("data:image/svg+xml;utf8,<svg xmlns=\'http://www.w3.org/2000/svg\' viewBox=\'0 0 18 22\'><path d=\'M1.5 1.2 L1.5 17.2 L5.6 13.4 L8.4 20 L11 18.9 L8.2 12.4 L13.8 12.4 Z\'/></svg>") no-repeat center/contain}'
      + '.lab{position:absolute;left:16px;top:18px;padding:3px 7px;border-radius:9px;background:rgba(19,19,22,.78);color:#fff;font:700 13px/1 system-ui,sans-serif;letter-spacing:-.5px;white-space:nowrap}.lab .ring{padding:1.5px;border-radius:9px}</style>'
      + '<div class="glow"><div class="ring"></div></div><div class="b"><div class="ring"></div></div>'
      + '<div class="t"><div class="ring"></div><b>a.</b>antiloki is controlling this tab</div>'
      + '<div class="c"><div class="arrow"></div><div class="lab"><div class="ring"></div>a.</div></div>';
    root.appendChild(host); var c = sh.querySelector('.c');
    window.__antilokiMove = function(x, y){ c.style.left = Math.max(0, Math.min(innerWidth - 30, x)) + 'px'; c.style.top = Math.max(0, Math.min(innerHeight - 30, y)) + 'px'; };
    window.__antilokiSay = function(words){ var el = sh.querySelector('.t'); if (el && el.lastChild.textContent !== words) el.lastChild.textContent = words; };
    window.__antilokiHide = function(h){ host.style.display = h ? 'none' : 'block'; };
    window.__antilokiMove(innerWidth * 0.6, innerHeight * 0.45);
  } build(); })();"##;

const READ_JS: &str = r#"(function(){
  var out={url:location.href,title:document.title,viewport:[innerWidth,innerHeight],scroll_height:document.documentElement.scrollHeight,fonts:{},colors:{bg:{},fg:{}},blocks:[],text:''};
  var all=document.body?document.body.querySelectorAll('*'):[]; var n=0;
  for(var i=0;i<all.length&&n<4000;i++){var e=all[i];var r=e.getBoundingClientRect();if(r.width<2||r.height<2)continue;var cs=getComputedStyle(e);if(cs.visibility==='hidden'||cs.display==='none')continue;n++;
    var area=Math.round(r.width*r.height);var f=cs.fontFamily.split(',')[0].replace(/["']/g,'').trim()+' '+cs.fontSize+' '+cs.fontWeight;if(e.childNodes.length&&Array.prototype.some.call(e.childNodes,function(c){return c.nodeType===3&&c.textContent.trim()}))out.fonts[f]=(out.fonts[f]||0)+1;
    var bg=cs.backgroundColor;if(bg&&bg!=='rgba(0, 0, 0, 0)')out.colors.bg[bg]=(out.colors.bg[bg]||0)+area;var fg=cs.color;out.colors.fg[fg]=(out.colors.fg[fg]||0)+1;}
  function top(o,k){return Object.keys(o).map(function(x){return [x,o[x]]}).sort(function(a,b){return b[1]-a[1]}).slice(0,k)}
  out.fonts=top(out.fonts,8);out.colors.bg=top(out.colors.bg,8);out.colors.fg=top(out.colors.fg,6);
  var sel='header,nav,aside,main,section,footer,[role=navigation],[role=main],[role=list],[role=listbox],[role=dialog],table,form';var els=document.querySelectorAll(sel);
  for(var j=0;j<els.length&&out.blocks.length<40;j++){var b=els[j].getBoundingClientRect();if(b.width<80||b.height<30)continue;var c=getComputedStyle(els[j]);out.blocks.push({tag:els[j].tagName.toLowerCase(),role:els[j].getAttribute('role')||'',x:Math.round(b.x),y:Math.round(b.y),w:Math.round(b.width),h:Math.round(b.height),bg:c.backgroundColor,pad:c.padding,gap:c.gap});}
  out.text=(document.body?document.body.innerText:'').replace(/\s+/g,' ').trim().slice(0,1500);return JSON.stringify(out);})()"#;

/// Put the mark of control on a tab: on every page it loads from now on, and on the one it shows now.
fn mark_tab(tab: &Arc<Tab>) {
    let _ = tab.call_method(headless_chrome::protocol::cdp::Page::AddScriptToEvaluateOnNewDocument { source: MARK_JS.to_string(), world_name: None, include_command_line_api: None, run_immediately: None });
    let _ = tab.evaluate(MARK_JS, false);
}
/// Hide the mark for a screenshot (it is antiloki's, not the app's) or show it again, and glide the pointer to a place on the page.
fn mark_hidden(tab: &Tab, hidden: bool) { let _ = tab.evaluate(&format!("window.__antilokiHide && window.__antilokiHide({hidden})"), false); }
fn mark_move(tab: &Tab, x: u32, y: u32) { let _ = tab.evaluate(&format!("window.__antilokiMove && window.__antilokiMove({x},{y})"), false); }

fn settle(tab: &Tab) { let _ = tab.evaluate("document.fonts && document.fonts.ready ? document.fonts.ready.then(()=>true) : true", true); std::thread::sleep(Duration::from_millis(3000)); }
fn shot(tab: &Arc<Tab>, path: &Path) -> Result<(), String> { let png = tab.capture_screenshot(CaptureScreenshotFormatOption::Png, None, None, true).map_err(|e| e.to_string())?; std::fs::write(path, png).map_err(|e| e.to_string()) }

/// Visit the pages at each width with the person's sign-in. When the sign-in window is still open (`login` left it so) the visit goes through THAT window, in tabs of its own; else a browser with no window is
/// started on the profile (for an app whose sign-in is kept in the folder). Blocking. Writes `<out>/<page>-<width>.png` and `.json`; a page that is a sign-in wall, or not what the node expects, is `signed_out`.
pub fn run_capture(urls: &[String], domains: &[String], widths: &[u32], profile: &str, expect: &[String], out: &Path) -> Result<Value, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("output folder: {e}"))?;
    // THIS NODE'S OWN OUTPUT STARTS EMPTY: a run's folder can hold the screenshots of an earlier run (the thread's branch keeps them), and those are not this run's
    if let Ok(rd) = std::fs::read_dir(out) { for e in rd.flatten() { let p = e.path(); if p.is_file() && matches!(p.extension().and_then(|x| x.to_str()), Some("png" | "json")) { let _ = std::fs::remove_file(&p); } } }
    let live = window_of(profile).and_then(|(pid, port)| if process_alive(pid) { debugger_url(port) } else { None });
    if live.is_none() { close_window(profile); }
    let mut shots = vec![]; let mut pages = vec![]; let mut signed_out: Vec<Value> = vec![]; let mut skipped: Vec<Value> = vec![];
    let mut mark = |u: &str, why: &str| { if !signed_out.iter().any(|x| x["url"] == json!(u)) { signed_out.push(json!({ "url": u, "why": why })); } };
    let dir = profile_dir(profile);
    let attached = match &live { Some(ws) => Some(Browser::connect(ws.clone()).map_err(|e| format!("could not attach to the sign-in window: {e}"))?), None => None };
    for &w in widths {
        let own; let browser: &Browser = match &attached { Some(b) => b, None => { own = Browser::new(options(true, w, &dir)?).map_err(|e| format!("could not open the browser ({e}); is a window of this profile still open?"))?; &own } };
        let tab = browser.new_tab().map_err(|e| e.to_string())?; tab.set_default_timeout(Duration::from_secs(25));
        let _ = tab.call_method(headless_chrome::protocol::cdp::Emulation::SetDeviceMetricsOverride { width: w, height: 900, device_scale_factor: 1.0, mobile: w < 600, scale: None, screen_width: None, screen_height: None, position_x: None, position_y: None, dont_set_visible_size: None, screen_orientation: None, viewport: None, display_feature: None, device_posture: None });
        if attached.is_some() { mark_tab(&tab); }
        if attached.is_none() { let _ = tab.set_user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36", None, None); }
        for u in urls {
            tab.navigate_to(u).map_err(|e| format!("{u}: {e}"))?; tab.wait_until_navigated().map_err(|e| format!("{u}: {e}"))?; settle(&tab);
            let now = tab.get_url(); let text = tab.evaluate("document.body ? document.body.innerText : ''", false).ok().and_then(|r| r.value).and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
            if looks_signed_out(&now, domains) { mark(u, "it went to a sign-in page or another site"); continue; }
            if looks_like_login_wall(&text) { mark(u, "the page is a sign-in wall"); continue; }
            if landed_on_front_page(u, &now) { mark(u, "it landed on the site's front page, not the page asked for"); continue; }
            // a page that is no wall but lacks the expected words (a page with its sidebar closed) is SKIPPED with a note: the run is signed in, the other pages prove it; only when no page at all shows them is it signed out
            if !has_expected(&text, expect) { if !skipped.iter().any(|x| x["url"] == json!(u)) { skipped.push(json!({ "url": u, "why": "the page does not show what the node expects" })); } continue; }
            let stem = format!("{}-{w}", page_slug(u)); let (png, js) = (out.join(format!("{stem}.png")), out.join(format!("{stem}.json")));
            if attached.is_some() { mark_move(&tab, w / 2, 260); std::thread::sleep(Duration::from_millis(500)); mark_hidden(&tab, true); std::thread::sleep(Duration::from_millis(80)); }
            let shot_result = shot(&tab, &png); if attached.is_some() { mark_hidden(&tab, false); mark_move(&tab, w / 3, 520); } shot_result?;
            let read = tab.evaluate(READ_JS, false).ok().and_then(|r| r.value).and_then(|v| v.as_str().map(String::from)).and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null);
            std::fs::write(&js, serde_json::to_string_pretty(&read).unwrap_or_default()).map_err(|e| e.to_string())?;
            shots.push(png.to_string_lossy().to_string()); pages.push(json!({ "url": u, "width": w, "shot": png.to_string_lossy(), "reading": js.to_string_lossy(), "title": read["title"], "blocks": read["blocks"].as_array().map_or(0, |b| b.len()) }));
        }
        let _ = tab.close(false);
    }
    drop(attached); if live.is_some() { close_window(profile); }
    // nothing captured and nothing said: every page lacked the expected words, so say so as the reason
    if shots.is_empty() && signed_out.is_empty() { for k in &skipped { signed_out.push(k.clone()); } }
    Ok(json!({ "ok": signed_out.is_empty() && !shots.is_empty(), "signed_out": signed_out, "skipped": skipped, "count": shots.len(), "shots": shots, "pages": pages, "dir": out.to_string_lossy() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_browse_node_names_its_domains_pages_and_profile_and_goes_nowhere_else() {
        let ok = json!({ "profile": "linear", "domains": ["linear.app"], "urls": ["https://linear.app/team/issues"], "widths": [1440, 390] });
        assert!(check_args("capture", &ok).is_empty(), "{:?}", check_args("capture", &ok));
        assert!(check_args("login", &json!({ "profile": "linear", "domains": ["linear.app"], "url": "https://linear.app" })).is_empty());
        assert!(check_args("capture", &json!({ "profile": "linear", "domains": ["linear.app"], "urls": ["https://evil.example/x"] })).iter().any(|e| e.contains("outside")));
        assert!(check_args("capture", &json!({ "profile": "linear", "domains": ["linear.app"], "urls": ["http://linear.app/x"] })).iter().any(|e| e.contains("https")));
        assert!(check_args("capture", &json!({ "profile": "../x", "domains": ["linear.app"], "urls": ["https://linear.app"] })).iter().any(|e| e.contains("profile")));
        assert!(check_args("capture", &json!({ "profile": "a", "domains": [], "urls": ["https://linear.app"] })).iter().any(|e| e.contains("domains")));
        assert!(check_args("capture", &json!({ "profile": "a", "domains": ["linear.app"], "urls": ["https://linear.app"], "widths": [100] })).iter().any(|e| e.contains("widths")));
        assert!(check_args("type", &json!({})).iter().any(|e| e.contains("login")));
        assert!(check_args("capture", &json!({ "profile": "a", "domains": ["linear.app"], "urls": ["https://user:pw@linear.app/"] })).iter().any(|e| e.contains("not an http")));
    }
    #[test]
    fn a_node_can_name_a_target_file_and_the_file_gives_its_pages_and_domains() {
        assert!(check_args("capture", &json!({ "target": "{ask}" })).is_empty() && !check_args("capture", &json!({ "target": " " })).is_empty());
        let t = json!({ "profile": "notion", "domains": ["notion.so"], "url": "https://www.notion.so/login", "urls": ["https://www.notion.so/"], "expect": ["Search"] });
        let a = with_target(&json!({ "target": "notion", "wait_s": 300, "mark": false, "domains": ["evil.example"] }), &t);
        assert_eq!(a["profile"], json!("notion")); assert_eq!(a["domains"], json!(["notion.so"]), "the node cannot widen the target's domains"); assert_eq!(a["wait_s"], json!(300)); assert_eq!(a["mark"], json!(false)); assert!(a.get("target").is_none());
        assert!(check_args("capture", &a).is_empty(), "{:?}", check_args("capture", &a)); assert!(target_name_ok("notion") && !target_name_ok("../x") && !target_name_ok("A b"));
        assert_eq!(target_output(&json!({})), Ok("react")); assert_eq!(target_output(&json!({ "output": "free-pane" })), Ok("free-pane")); assert!(target_output(&json!({ "output": "vue" })).unwrap_err().contains("free-pane")); assert!(target_output(&json!({ "output": 3 })).is_err());
        assert!(with_target(&json!({ "target": "n" }), &json!({ "output": "free-pane", "profile": "n" }))["output"] == json!("free-pane"), "the target's output reaches the node");
        assert!(target_path("/p", "notion").ends_with(".antiloki/browse-targets/notion.json"));
    }
    #[test]
    fn a_sub_domain_is_inside_its_domain_and_a_look_alike_is_not() {
        let d = vec!["linear.app".to_string()];
        assert!(host_allowed("linear.app", &d) && host_allowed("cms.linear.app", &d) && !host_allowed("notlinear.app", &d) && !host_allowed("linear.app.evil.io", &d));
        assert_eq!(host_of("https://Linear.app:443/x?y"), Some("linear.app".into())); assert_eq!(host_of("ftp://x.io"), None); assert_eq!(host_of("https://a@b.io/"), None);
    }
    #[test]
    fn a_sign_in_wall_a_front_page_landing_and_a_missing_word_are_each_told_from_the_app() {
        assert!(looks_like_login_wall("Log in to Linear\nContinue with Google\nContinue with email") && !looks_like_login_wall(&format!("My issues Inbox {}", "word ".repeat(400))));
        assert!(landed_on_front_page("https://linear.app/akalli/team/DAN/active", "https://linear.app/") && !landed_on_front_page("https://linear.app/akalli/team/DAN/active", "https://linear.app/akalli/team/DAN/active/") && !landed_on_front_page("https://linear.app/", "https://linear.app/"));
        assert!(has_expected("Inbox  My issues", &["my issues".into()]) && !has_expected("Log in", &["My issues".into()]) && has_expected("anything", &[]));
        assert_eq!(expect_of(&json!({ "expect": ["My issues", " "] })), vec!["My issues".to_string()]);
    }
    #[test]
    fn a_visit_that_lands_on_a_sign_in_page_or_another_site_is_signed_out() {
        let d = vec!["linear.app".to_string()];
        assert!(looks_signed_out("https://linear.app/login", &d) && looks_signed_out("https://accounts.google.com/o/oauth2/auth", &d) && !looks_signed_out("https://linear.app/team/DAN/active", &d));
        assert_eq!(page_slug("https://linear.app/team/DAN/active?x=1"), "linear-app-team-dan-active-x-1"); assert_eq!(widths_of(&json!({})), vec![1440, 768, 390]);
    }
    /// A real browser against a page on this machine (`cargo test -- --ignored browse_real`): a screenshot and a reading come back, and a visit that lands on /login is called signed out.
    #[test]
    #[ignore]
    fn browse_real_captures_a_local_page_and_names_a_sign_in_redirect() {
        let dir = std::env::temp_dir().join(format!("antiloki-browse-{}", std::process::id())); std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "<html><body style='background:#131316;color:#ededf0;font:16px Inter,sans-serif'><nav>nav</nav><main><section><h1>Issues</h1><ul role=list><li>DAN-1 first</li></ul></section></main></body></html>").unwrap();
        std::fs::create_dir_all(dir.join("login")).unwrap(); std::fs::write(dir.join("login/index.html"), "<html><body>sign in</body></html>").unwrap();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let port = l.local_addr().unwrap().port(); drop(l);
        let d2 = dir.clone(); let mut srv = std::process::Command::new("python3").args(["-m", "http.server", &port.to_string(), "--bind", "127.0.0.1"]).current_dir(&d2).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
        std::thread::sleep(Duration::from_millis(900));
        let out = dir.join("out"); let domains = vec!["localhost".to_string(), "127.0.0.1".to_string()];
        let r = run_capture(&[format!("http://127.0.0.1:{port}/index.html")], &domains, &[1000, 390], "test-real", &[], &out);
        let r2 = run_capture(&[format!("http://127.0.0.1:{port}/login/")], &domains, &[800], "test-real", &[], &dir.join("out-login"));   // each capture clears its own folder, so each has one
        // the window path: a plain window with a debugging port is opened by `login`, the capture goes through THAT window and closes it
        let live_url = format!("http://127.0.0.1:{port}/index.html"); let l = run_login(&live_url, "test-live", 30, false); let l_ok = l.is_ok();
        let live = if l_ok { Some(run_capture(&[live_url.clone()], &domains, &[900], "test-live", &["Issues".to_string()], &dir.join("out-live"))) } else { None };
        let still = window_of("test-live").is_some();
        let miss = if l_ok { Some(run_login(&live_url, "test-live2", 30, false).and_then(|_| run_capture(&[live_url.clone()], &domains, &[900], "test-live2", &["Nothing like this".to_string()], &dir.join("out-miss")))) } else { None };
        let _ = srv.kill(); let _ = std::fs::remove_dir_all(profile_dir("test-real")); let _ = std::fs::remove_dir_all(profile_dir("test-live")); let _ = std::fs::remove_dir_all(profile_dir("test-live2"));
        assert!(l_ok, "{l:?}"); let live = live.unwrap().unwrap(); assert_eq!(live["count"], json!(1), "{live}"); assert!(!still, "the window was closed after the capture");
        let miss = miss.unwrap().unwrap(); assert_eq!(miss["ok"], json!(false), "a page without the expected word is signed out: {miss}");
        let r = r.unwrap(); assert_eq!(r["count"], json!(2), "{r}"); let rd: Value = serde_json::from_str(&std::fs::read_to_string(out.join(format!("{}-1000.json", page_slug(&format!("http://127.0.0.1:{port}/index.html"))))).unwrap()).unwrap();
        assert!(rd["text"].as_str().unwrap().contains("DAN-1"), "{rd}"); assert!(!rd["blocks"].as_array().unwrap().is_empty()); assert!(std::fs::metadata(r["shots"][0].as_str().unwrap()).unwrap().len() > 500);
        assert_eq!(r2.unwrap()["signed_out"].as_array().unwrap().len(), 1);
    }
    /// A window with the mark on it, held for a while so it can be looked at (`cargo test -- --ignored browse_real_mark_look`).
    #[test]
    #[ignore]
    fn browse_real_mark_look() {
        let dir = std::env::temp_dir().join(format!("antiloki-mark-{}", std::process::id())); std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "<html><body style='background:#131316;color:#ededf0;font:18px sans-serif;padding:60px'><h1>Issues</h1><p>DAN-1 Get familiar</p></body></html>").unwrap();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let port = l.local_addr().unwrap().port(); drop(l);
        let mut srv = std::process::Command::new("python3").args(["-m", "http.server", &port.to_string(), "--bind", "127.0.0.1"]).current_dir(&dir).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
        std::thread::sleep(Duration::from_millis(800));
        let url = format!("http://127.0.0.1:{port}/index.html"); run_login(&url, "test-look", 30, true).unwrap();
        let (_, dport) = window_of("test-look").unwrap(); let browser = Browser::connect(debugger_url(dport).unwrap()).unwrap();
        let tab = browser.new_tab().unwrap(); mark_tab(&tab); tab.navigate_to(&url).unwrap(); tab.wait_until_navigated().unwrap(); std::thread::sleep(Duration::from_millis(600)); mark_move(&tab, 400, 300);
        eprintln!("MARK-READY"); std::thread::sleep(Duration::from_secs(40));
        close_window("test-look"); let _ = srv.kill(); let _ = std::fs::remove_dir_all(profile_dir("test-look"));
    }
}
