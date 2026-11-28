//! antiloki desktop — ONE executable: Tauri window + axum server + the substrate engine.
//!
//! The server is LINKED, not spawned. `antiloki-serve` is a library dependency, so there is no
//! sidecar process, no node.exe and no napi addon in the shipping path — which is the whole point
//! of the cutover.
//!
//! Startup order matters: the axum server is bound BEFORE the webview opens, because the FE's
//! very first request fires as soon as the page loads. It runs on its own multi-threaded runtime
//! on a background thread; Tauri owns the main thread (macOS requires the UI on thread 0).
//!
//! Port: pinned to the FE's compiled-in default (41931) unless ANTILOKI_SERVE_PORT overrides it,
//! so a stock `vite build` needs no rebuild to talk to the shell.
//!
//! STANDALONE (operator 2026-09-17): the app is sold as a local product — trial, yearly or lifetime
//! license (serve/src/license.rs, all inside the linked server) — and it updates itself from a
//! signed static manifest (tauri-plugin-updater; see DESKTOP.md). The FE talks to this shell
//! through `window.__TAURI__` (withGlobalTauri) — three commands, below.

// Release builds get no console window on Windows. Debug keeps it — the tick loop and dispatch
// logs are the only way to see what the engine is doing.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Serialize;
use tauri_plugin_updater::UpdaterExt;

const DEFAULT_PORT: u16 = 41931;

#[derive(Serialize, Clone)]
struct UpdateInfo {
    available: bool,
    current: String,
    version: Option<String>,
    notes: Option<String>,
    date: Option<String>,
}

/// Ask the manifest whether a newer build exists. Never installs — the FE shows a pill and the
/// operator decides when to restart.
#[tauri::command]
async fn update_check(app: tauri::AppHandle) -> Result<UpdateInfo, String> {
    let current = app.package_info().version.to_string();
    let updater = app.updater().map_err(|e| e.to_string())?;
    match updater.check().await.map_err(|e| e.to_string())? {
        Some(u) => Ok(UpdateInfo { available: true, current, version: Some(u.version.clone()), notes: u.body.clone(), date: u.date.map(|d| d.to_string()) }),
        None => Ok(UpdateInfo { available: false, current, version: None, notes: None, date: None }),
    }
}

/// Download, verify the signature against the compiled-in public key, install, relaunch. The
/// engine's SQLite state is untouched — an update only replaces the executable.
#[tauri::command]
async fn update_install(app: tauri::AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let Some(update) = updater.check().await.map_err(|e| e.to_string())? else { return Err("already on the latest version".into()) };
    update.download_and_install(|_chunk, _total| {}, || {}).await.map_err(|e| e.to_string())?;
    app.restart();
}

/// What the FE shows in the about line and sends with a support request.
#[tauri::command]
fn app_info(app: tauri::AppHandle) -> serde_json::Value {
    serde_json::json!({
        "name": app.package_info().name,
        "version": app.package_info().version.to_string(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "port": port(),
    })
}

fn port() -> u16 {
    std::env::var("ANTILOKI_SERVE_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT)
}

/// Poll the linked server's health until it answers (or 10 s pass). Replaces the old fixed
/// 300 ms sleep: on a cold machine the engine's first openDb (9-DB attach + seed) can take longer,
/// and a failed first request looks to the operator like "the app is broken".
fn wait_for_server(port: u16) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), std::time::Duration::from_millis(200)).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    eprintln!("antiloki: the server did not answer on {port} within 10 s — opening the window anyway");
}


/// A request to the linked server over a plain socket (it is on this machine and answers JSON): the body of the reply, parsed. None when it does not answer.
fn local_json(port: u16, method: &str, path: &str, body: &str) -> Option<serde_json::Value> {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), std::time::Duration::from_millis(500)).ok()?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(3))).ok()?;
    write!(s, "{method} {path} HTTP/1.0\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).ok()?;
    let mut out = String::new(); s.read_to_string(&mut out).ok()?;
    serde_json::from_str(out.split_once("\r\n\r\n")?.1).ok()
}
/// BL-31 (operator: "how would I know it is running? an icon like Windows background apps"): the line the tray shows. Pure over the two answers.
fn tray_line(agents: Option<(u64, u64)>, teams_running: Option<usize>) -> String {
    let a = agents.map_or("agents unknown".to_string(), |(live, asleep)| format!("{live} agent{} awake, {asleep} asleep", if live == 1 { "" } else { "s" }));
    let t = teams_running.map_or(String::new(), |n| format!(" · {n} team run{} going", if n == 1 { "" } else { "s" }));
    format!("antiloki is running: {a}{t}")
}
fn status_line(port: u16) -> String {
    let agents = local_json(port, "GET", "/api/v1/cockpit/avatars", "").and_then(|v| Some((v["live"].as_u64()?, v["asleep"].as_u64()?)));
    let teams = local_json(port, "POST", "/api/v1/cockpit/team-graph", "{\"op\":\"list\"}").and_then(|v| v["live"].as_array().map(|a| a.len()));
    tray_line(agents, teams)
}

/// IS ANTILOKI ALREADY SERVING THERE (the installed server, or a dev one)? Then this window only shows it: a second server on the same data is never started (2026-11-27). Pure over the answer.
fn is_antiloki(avatars: Option<serde_json::Value>) -> bool { avatars.is_some_and(|v| v["ok"] == serde_json::json!(true) && v["agents"].is_array()) }
/// What the tray's second line and the question on closing say: when this window only shows a server that runs without it, quitting stops nothing. Pure.
fn quit_words(attached: bool) -> (&'static str, &'static str, &'static str) { if attached { ("Close antiloki's window and icon", "antiloki has agents or teams running. Keep the small a. mark on screen?", "Close") } else { ("Quit (stops agents and teams)", "antiloki has agents or teams running. Keep them running in the background?", "Quit") } }
/// The person's answer given ahead of time (`ANTILOKI_DESKTOP_ON_CLOSE=keep|quit`): no question is asked. Pure.
fn answered(v: Option<String>) -> Option<bool> { match v.as_deref().map(str::trim) { Some("keep") => Some(true), Some("quit") => Some(false), _ => None } }

/// Something is going while the window is closed: an awake agent or a team run. Unknown answers count as nothing (a closed window then quits as it always did).
fn anything_running(port: u16) -> bool {
    let agents = local_json(port, "GET", "/api/v1/cockpit/avatars", "").and_then(|v| v["live"].as_u64()).unwrap_or(0);
    let teams = local_json(port, "POST", "/api/v1/cockpit/team-graph", "{\"op\":\"list\"}").and_then(|v| v["live"].as_array().map(|a| a.len())).unwrap_or(0);
    agents > 0 || teams > 0
}
/// Bring the app back: the main window shows, the one avatar goes away.
fn show_main_app(app: &tauri::AppHandle) {
    use tauri::Manager;
    if let Some(w) = app.get_webview_window("main") { let _ = w.show(); let _ = w.unminimize(); let _ = w.set_focus(); }
    if let Some(a) = app.get_webview_window("avatar") { let _ = a.hide(); }
}
/// The avatar's "Open antiloki" button.
#[tauri::command]
fn show_main(app: tauri::AppHandle) { show_main_app(&app); }

/// THE BROWSER PANE'S REAL WEBVIEW (the operator 2026-11-22: "why can I not fully use the browser inside antiloki?"): a page in an iframe cannot show Google or most big sites (their own frame rule). In the desktop app the pane asks for a real webview laid over its rectangle: the same engine as the window, a normal tab with cookies and logins, no frame rule. It has no IPC (the capability file names only `main`), so a web page can never call the shell.
/// The label is the pane's own (letters, digits, dashes). `x y w h` are the pane's rectangle in window pixels.
#[tauri::command]
fn browser_show(app: tauri::AppHandle, label: String, url: String, x: f64, y: f64, w: f64, h: f64) -> Result<(), String> {
    use tauri::{webview::WebviewBuilder, LogicalPosition, LogicalSize, Manager, WebviewUrl};
    let u: tauri::Url = url.parse().map_err(|_| "that is not a web address".to_string())?;
    if !matches!(u.scheme(), "http" | "https") { return Err("only http and https pages open here".into()); }
    let label = browser_label(&label)?;
    if let Some(wv) = app.get_webview(&label) {
        wv.set_position(LogicalPosition::new(x, y)).map_err(|e| e.to_string())?; wv.set_size(LogicalSize::new(w.max(1.0), h.max(1.0))).map_err(|e| e.to_string())?; let _ = wv.show();
        if wv.url().map(|c| c != u).unwrap_or(true) { let _ = wv.navigate(u); }
        return Ok(());
    }
    let win = app.get_window("main").ok_or("no main window")?;
    win.add_child(WebviewBuilder::new(label, WebviewUrl::External(u)), LogicalPosition::new(x, y), LogicalSize::new(w.max(1.0), h.max(1.0))).map_err(|e| e.to_string())?;
    Ok(())
}
/// Hide the pane's webview (a window or menu is over it, or the pane is not shown); the page stays loaded.
#[tauri::command]
fn browser_hide(app: tauri::AppHandle, label: String) -> Result<(), String> { use tauri::Manager; let label = browser_label(&label)?; if let Some(wv) = app.get_webview(&label) { let _ = wv.hide(); } Ok(()) }
/// The pane closed: its webview goes away.
#[tauri::command]
fn browser_close(app: tauri::AppHandle, label: String) -> Result<(), String> { use tauri::Manager; let label = browser_label(&label)?; if let Some(wv) = app.get_webview(&label) { let _ = wv.close(); } Ok(()) }
/// A pane's webview label: `browser-` and what the pane gave, letters digits and dashes only. Pure.
fn browser_label(raw: &str) -> Result<String, String> {
    let t: String = raw.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(40).collect(); if t.is_empty() { return Err("a browser pane needs a name".into()); } Ok(format!("browser-{t}"))
}
/// Whether the shell shows the corner mark here. On Wayland an app can neither place its window nor keep it on top, so there the browser's Multiplex app is the way (the tray stays). Pure.
fn mark_here(linux: bool, wayland: bool) -> bool { !(linux && wayland) }
fn show_mark(app: &tauri::AppHandle) {
    use tauri::Manager;
    if !mark_here(cfg!(target_os = "linux"), std::env::var_os("WAYLAND_DISPLAY").is_some()) { return; }
    if let Some(a) = app.get_webview_window("avatar") { let _ = a.show(); }
}
/// What a change of the main window's minimised state asks of the mark: show it, hide it, or nothing (so a mark the person closed stays closed until the next minimise). Pure.
fn on_minimised(was: bool, is: bool) -> Option<bool> { (was != is).then_some(is) }
static MINIMISED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static KEEP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn main() {
    // THE WINDOW CLOSED THE MOMENT IT OPENED on a Wayland desktop (Hyprland, 2026-10-27: "Gdk-Message: Error 71 (Protocol error) dispatching to Wayland display"). WebKitGTK's
    // DMA-BUF renderer is what Wayland compositors reject; turning it off keeps native Wayland and draws the same window. Left alone if the person has set the variable themselves.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1"); }
    // an installed app's data lives in the user's data folder, never the build machine's checkout
    // (serve/src/lib.rs::ensure_data_root) — set before the server thread exists, so every reader agrees
    antiloki_serve::ensure_data_root();
    let port = port();

    // The engine is synchronous and blocking (rusqlite), so it gets a real multi-threaded runtime
    // rather than sharing one with the UI.
    let attached = is_antiloki(local_json(port, "GET", "/api/v1/cockpit/avatars", ""));
    if attached { eprintln!("antiloki: a server already answers on {port}: showing it, starting none"); } else { std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_stack_size(64 * 1024 * 1024) // a big tree walked on a 2 MB worker stack overflowed the serve (2026-09-29)
            .build()
            .expect("antiloki: could not start the server runtime");
        rt.block_on(antiloki_serve::run(Some(port)));
    }); }

    wait_for_server(port);

    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init()) // ⊕ the OS file dialog (2026-09-21): a file anywhere on the PC, onto the ask
        .setup(move |app| {
            // THE TRAY ICON (BL-31): antiloki keeps agents, teams and schedules going, so there is an icon that says so; its tooltip is refreshed every few seconds. Menu: Open, Quit.
            use tauri::{menu::{Menu, MenuItem}, tray::TrayIconBuilder, Manager};
            let open = MenuItem::with_id(app, "open", "Open antiloki", true, None::<&str>)?; let quit = MenuItem::with_id(app, "quit", quit_words(attached).0, true, None::<&str>)?;
            // the screen of the server it found: the same pages a browser shows, so both stay one version
            if attached { if let (Some(w), Ok(u)) = (app.get_webview_window("main"), format!("http://localhost:{port}/").parse::<tauri::Url>()) { let _ = w.navigate(u); } }
            let menu = Menu::with_items(app, &[&open, &quit])?;
            let mut b = TrayIconBuilder::with_id("antiloki").tooltip("antiloki is running").menu(&menu).on_menu_event(|app, ev| match ev.id().as_ref() {
                "open" => show_main_app(app),
                "quit" => app.exit(0),
                _ => {}
            });
            if let Some(i) = app.default_window_icon() { b = b.icon(i.clone()); }
            let tray = b.build(app)?;
            std::thread::spawn(move || loop { let _ = tray.set_tooltip(Some(status_line(port))); std::thread::sleep(std::time::Duration::from_secs(10)); });
            // N13 (operator 2026-11-16: "one avatar, only when antiloki is closed"): a small window with the one avatar, hidden until the main window is closed with something still running.
            let url = format!("http://localhost:{port}/avatar");
            if let Ok(u) = url.parse() { let _ = tauri::WebviewWindowBuilder::new(app, "avatar", tauri::WebviewUrl::External(u)).title("antiloki avatar" /* its own title, so a window rule can float and pin only this one */).inner_size(340.0, 420.0).decorations(false).transparent(true).always_on_top(true).skip_taskbar(true).resizable(false).visible(false).build().map(|w| {
                // bottom-right to begin with; from then on it stays wherever the person drags it
                if let Ok(Some(m)) = w.primary_monitor() { let (s, area) = (m.scale_factor(), m.work_area()); let _ = w.set_position(tauri::PhysicalPosition::new(area.position.x + area.size.width as i32 - (352.0 * s) as i32, area.position.y + area.size.height as i32 - (432.0 * s) as i32)); }
            }); }
            Ok(())
        })
        .on_window_event(move |win, ev| {
            // Closing the main window with agents or teams running asks ONCE per launch: keep them going in the background (the one avatar and the tray stay) or quit. Nothing running: it quits as before.
            use tauri::Manager;
            if win.label() != "main" { return; }
            // MINIMISED TOO (operator 2026-11-27): the mark comes while the app is minimised and goes when it is back
            if matches!(ev, tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Focused(_)) {
                let is = win.is_minimized().unwrap_or(false);
                if let Some(show) = on_minimised(MINIMISED.swap(is, std::sync::atomic::Ordering::Relaxed), is) { if show { show_mark(win.app_handle()); } else if let Some(a) = win.app_handle().get_webview_window("avatar") { let _ = a.hide(); } }
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = ev {
                if !anything_running(port) { return; }
                api.prevent_close(); let app = win.app_handle().clone();
                std::thread::spawn(move || {
                    use std::sync::atomic::Ordering::Relaxed; use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
                    if !ASKED.swap(true, Relaxed) { let (_, question, no) = quit_words(attached); KEEP.store(match answered(std::env::var("ANTILOKI_DESKTOP_ON_CLOSE").ok()) { Some(a) => a, None => app.dialog().message(question).title("antiloki").buttons(MessageDialogButtons::OkCancelCustom("Keep running".into(), no.into())).blocking_show() }, Relaxed); }
                    if KEEP.load(Relaxed) { if let Some(w) = app.get_webview_window("main") { let _ = w.hide(); } show_mark(&app); } else { app.exit(0); }
                });
            }
        })
        .invoke_handler(tauri::generate_handler![update_check, update_install, app_info, show_main, browser_show, browser_hide, browser_close])
        .run(tauri::generate_context!())
        .expect("antiloki: failed to start the desktop shell");
}

#[cfg(test)]
mod tests {
    use super::{browser_label, tray_line};
    #[test] fn the_mark_shows_on_minimise_and_not_on_wayland() { use super::{mark_here, on_minimised}; assert_eq!((on_minimised(false, true), on_minimised(true, false), on_minimised(true, true), on_minimised(false, false)), (Some(true), Some(false), None, None)); assert!(!mark_here(true, true) && mark_here(true, false) && mark_here(false, false)); }
    #[test] fn a_browser_webview_label_is_plain() { assert_eq!(browser_label("pane 7/../x").unwrap(), "browser-pane7x"); assert!(browser_label("///").is_err()); }
    #[test] fn a_running_antiloki_is_shown_not_doubled() {
        use super::{answered, is_antiloki, quit_words}; use serde_json::json;
        assert!(is_antiloki(Some(json!({ "ok": true, "agents": [] })))); for other in [None, Some(json!({ "ok": true })), Some(json!({ "message": "Not Found" }))] { assert!(!is_antiloki(other)); }
        assert!(quit_words(true).0.starts_with("Close") && quit_words(false).0.contains("stops agents")); assert_eq!((answered(Some("keep".into())), answered(Some("quit ".into())), answered(Some("x".into())), answered(None)), (Some(true), Some(false), None, None));
    }
    #[test] fn the_tray_says_what_runs_and_admits_what_it_does_not_know() {
        assert_eq!(tray_line(Some((2, 1)), Some(1)), "antiloki is running: 2 agents awake, 1 asleep · 1 team run going");
        assert_eq!(tray_line(Some((1, 0)), Some(0)), "antiloki is running: 1 agent awake, 0 asleep · 0 team runs going");
        assert_eq!(tray_line(None, None), "antiloki is running: agents unknown");
    }
}
