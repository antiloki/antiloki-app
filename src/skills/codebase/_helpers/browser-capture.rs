//! browser-capture.rs — the browser session behind `designCaptureLive` (2026-09-17). The JS original
//! drove Playwright; the standalone is one Rust binary with no Node, so this drives the machine's own
//! Chrome / Chromium / Edge over CDP (`headless_chrome`, a pure-Rust client). Same contract as the JS
//! branch: open each seed URL of the operator's running dev server, wait for it to settle, run the
//! COLLECT script in the page and get back the REAL rendered DOM + the page's CSS + the framework's
//! component roots. Deterministic given the same running app — no model, no cost.
//!
//! Browser discovery: `ANTILOKI_CHROME` (a path) wins; then `headless_chrome`'s own finder (`CHROME`,
//! chromium / google-chrome on PATH, the usual install dirs); then the Edge and Playwright-cache
//! locations below. No browser → `Err` with the list of places we looked, so the operator knows what
//! to install (any Chromium works — Edge is one).

use headless_chrome::{Browser, LaunchOptions};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

/// Runs inside the page. Returns one JSON object: `{ title, framework, html, css, components:[{name,html}] }`.
/// `html` is the body's markup with scripts and our own attributes stripped; `css` is every rule the
/// page can read (same-origin sheets + inline `<style>`), in document order, so the frame re-renders
/// pixel-for-pixel; `components` are the outermost DOM roots of each distinct component the framework
/// exposes on its nodes (React fiber · Vue · Svelte · Angular), first instance each.
pub const COLLECT: &str = r#"(() => {
  const css = [];
  for (const sh of Array.from(document.styleSheets)) {
    try { css.push(Array.from(sh.cssRules).map((r) => r.cssText).join("\n")); } catch (e) { if (sh.href) css.push(`@import url("${sh.href}");`); }
  }
  const clone = document.body.cloneNode(true);
  for (const s of clone.querySelectorAll("script, noscript, link[rel=modulepreload]")) s.remove();
  let framework = "html";
  const comps = new Map();
  const nameOf = (el) => {
    for (const k of Object.keys(el)) {
      if (k.startsWith("__reactFiber$")) {
        framework = "react";
        let f = el[k];
        for (let i = 0; f && i < 12; i++) { const t = f.type; if (typeof t === "function" || (t && typeof t === "object" && t.render)) { const n = t.displayName || t.name || (t.render && t.render.name); if (n && !/^(Fragment|Provider|Consumer|Context)$/.test(n)) return n; } f = f.return; }
        return null;
      }
      if (k === "__vueParentComponent" || k === "__vue__") { framework = "vue"; const c = el[k]; const n = c && (c.type && (c.type.name || c.type.__name) || c.$options && c.$options.name); return n || null; }
      if (k.startsWith("__svelte")) { framework = "svelte"; return null; }
    }
    if (el.hasAttribute && Array.from(el.attributes).some((a) => a.name.startsWith("_ngcontent") || a.name.startsWith("ng-version"))) framework = "angular";
    return null;
  };
  const all = Array.from(document.body.querySelectorAll("*"));
  for (const el of all) {
    const n = nameOf(el);
    if (n && !comps.has(n) && el.outerHTML.length < 60000) comps.set(n, el.outerHTML);
    if (comps.size >= 60) break;
  }
  return JSON.stringify({ title: document.title || location.pathname, framework, html: clone.innerHTML, css: css.join("\n"), components: Array.from(comps, ([name, html]) => ({ name, html })) });
})()"#;

fn candidates() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("ANTILOKI_CHROME") { if !p.trim().is_empty() { v.push(PathBuf::from(p)); } }
    if let Ok(p) = headless_chrome::browser::default_executable() { v.push(p); }
    for p in [
        "/usr/bin/chromium", "/usr/bin/chromium-browser", "/usr/bin/google-chrome", "/usr/bin/google-chrome-stable", "/usr/bin/microsoft-edge", "/snap/bin/chromium",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/Applications/Chromium.app/Contents/MacOS/Chromium", "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe", "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
        "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe", "C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe",
    ] { v.push(PathBuf::from(p)); }
    // the Playwright cache, when the operator has the FE's toolchain around
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let cache = PathBuf::from(home).join(".cache").join("ms-playwright");
        if let Ok(rd) = std::fs::read_dir(&cache) {
            let mut dirs: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.file_name().map(|n| n.to_string_lossy().starts_with("chromium-")).unwrap_or(false)).collect();
            dirs.sort();
            for d in dirs.into_iter().rev() { for rel in ["chrome-linux/chrome", "chrome-mac/Chromium.app/Contents/MacOS/Chromium", "chrome-win/chrome.exe"] { v.push(d.join(rel)); } }
        }
    }
    v
}

/// The first browser that exists on this machine, or the list we searched.
pub fn find_browser() -> Result<PathBuf, String> {
    let c = candidates();
    c.iter().find(|p| p.is_file()).cloned().ok_or_else(|| format!(
        "no Chrome / Chromium / Edge found — install one, or point ANTILOKI_CHROME at the executable. Looked at: {}",
        c.iter().take(8).map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
    ))
}

pub struct Captured {
    pub path: String,
    pub title: String,
    pub framework: String,
    pub html: String,
    pub css: String,
    pub components: Vec<(String, String)>,
}

/// Open every seed URL in one headless browser and collect it. A page that fails to load is reported
/// in `errors`, never fatal — the operator still gets the screens that worked.
pub fn capture_all(seeds: &[String], settle_ms: u64) -> Result<(Vec<Captured>, Vec<String>), String> {
    let exe = find_browser()?;
    let opts = LaunchOptions::default_builder()
        .path(Some(exe))
        .headless(true)
        .window_size(Some((1440, 900)))
        .idle_browser_timeout(Duration::from_secs(120))
        .args(vec![std::ffi::OsStr::new("--hide-scrollbars"), std::ffi::OsStr::new("--disable-gpu")])
        .build()
        .map_err(|e| format!("browser options: {e}"))?;
    let browser = Browser::new(opts).map_err(|e| format!("could not launch the browser: {e}"))?;
    let tab = browser.new_tab().map_err(|e| format!("could not open a tab: {e}"))?;
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for url in seeds {
        let step = (|| -> Result<Captured, String> {
            tab.navigate_to(url).map_err(|e| e.to_string())?;
            tab.wait_until_navigated().map_err(|e| e.to_string())?;
            std::thread::sleep(Duration::from_millis(settle_ms)); // hydration, fonts, first data
            let ro = tab.evaluate(COLLECT, false).map_err(|e| e.to_string())?;
            let raw = ro.value.and_then(|v| v.as_str().map(str::to_string)).ok_or("collect returned nothing")?;
            let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            let path = url::Url::parse(url).map(|u| { let p = u.path().to_string(); if p.is_empty() { "/".into() } else { p } }).unwrap_or_else(|_| url.clone());
            Ok(Captured {
                path,
                title: v["title"].as_str().unwrap_or("").to_string(),
                framework: v["framework"].as_str().unwrap_or("html").to_string(),
                html: v["html"].as_str().unwrap_or("").to_string(),
                css: v["css"].as_str().unwrap_or("").to_string(),
                components: v["components"].as_array().map(|a| a.iter().filter_map(|c| Some((c["name"].as_str()?.to_string(), c["html"].as_str()?.to_string()))).collect()).unwrap_or_default(),
            })
        })();
        match step { Ok(c) => out.push(c), Err(e) => errors.push(format!("{url}: {e}")) }
    }
    Ok((out, errors))
}

/// Replace the origin's rows with what the browser saw: one stylesheet row (css + the base URL for
/// relative assets), one `screen` per seed, one `component` per distinct component root.
pub fn persist(db: &rusqlite::Connection, origin: &str, base_url: &str, shots: &[Captured]) -> Result<(usize, usize), String> {
    db.execute("DELETE FROM code_design_snapshots WHERE origin = ?", rusqlite::params![origin]).map_err(|e| e.to_string())?;
    let css = shots.iter().map(|s| s.css.as_str()).max_by_key(|c| c.len()).unwrap_or("");
    db.execute(
        "INSERT INTO code_design_snapshots (origin, kind, name, path, framework, html, css) VALUES (?,?,?,?,?,?,?)",
        rusqlite::params![origin, "stylesheet", "__css__", base_url, Option::<String>::None, Option::<String>::None, css],
    ).map_err(|e| e.to_string())?;
    let mut screens = 0usize;
    let mut comps = 0usize;
    let mut seen = std::collections::HashSet::new();
    for s in shots {
        let name = screen_name(&s.path, &s.title);
        db.execute(
            "INSERT INTO code_design_snapshots (origin, kind, name, path, framework, html, css) VALUES (?,?,?,?,?,?,?)",
            rusqlite::params![origin, "screen", name, s.path, s.framework, s.html, Option::<String>::None],
        ).map_err(|e| e.to_string())?;
        screens += 1;
        for (cname, html) in &s.components {
            if !seen.insert(cname.clone()) { continue; }
            db.execute(
                "INSERT INTO code_design_snapshots (origin, kind, name, path, framework, html, css) VALUES (?,?,?,?,?,?,?)",
                rusqlite::params![origin, "component", cname, s.path, s.framework, html, Option::<String>::None],
            ).map_err(|e| e.to_string())?;
            comps += 1;
        }
    }
    Ok((screens, comps))
}

/// "/" → "Home"; "/cart/items" → "cart / items"; a title wins when the path is the root and the title is short.
pub fn screen_name(path: &str, title: &str) -> String {
    let p = path.trim_matches('/');
    if p.is_empty() {
        let t = title.trim();
        return if !t.is_empty() && t.len() <= 40 { format!("Home · {t}") } else { "Home".into() };
    }
    p.split('/').filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" / ")
}

pub fn summary(shots: &[Captured], errors: &[String]) -> Value {
    json!({
        "screens": shots.iter().map(|s| json!({ "path": s.path, "name": screen_name(&s.path, &s.title), "framework": s.framework, "components": s.components.len(), "html_bytes": s.html.len() })).collect::<Vec<_>>(),
        "errors": errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn screen_names_read_like_routes() {
        assert_eq!(screen_name("/", "ministore"), "Home · ministore");
        assert_eq!(screen_name("/", ""), "Home");
        assert_eq!(screen_name("/cart/items/", "x"), "cart / items");
        assert_eq!(screen_name("/", &"t".repeat(80)), "Home");
    }
    #[test]
    fn persist_writes_one_stylesheet_and_dedupes_components() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE code_design_snapshots (origin TEXT, kind TEXT, name TEXT, path TEXT, framework TEXT, html TEXT, css TEXT);").unwrap();
        let shots = vec![
            Captured { path: "/".into(), title: "shop".into(), framework: "react".into(), html: "<main/>".into(), css: ".a{}".into(), components: vec![("App".into(), "<div/>".into()), ("Card".into(), "<b/>".into())] },
            Captured { path: "/cart".into(), title: "".into(), framework: "react".into(), html: "<aside/>".into(), css: ".a{} .b{}".into(), components: vec![("Card".into(), "<b/>".into()), ("Cart".into(), "<i/>".into())] },
        ];
        let (s, c) = persist(&db, "atlas", "http://localhost:5199", &shots).unwrap();
        assert_eq!((s, c), (2, 3));
        let css: String = db.query_row("SELECT css FROM code_design_snapshots WHERE kind='stylesheet'", [], |r| r.get(0)).unwrap();
        assert_eq!(css, ".a{} .b{}", "the longest stylesheet wins");
        let base: String = db.query_row("SELECT path FROM code_design_snapshots WHERE kind='stylesheet'", [], |r| r.get(0)).unwrap();
        assert_eq!(base, "http://localhost:5199");
        let n: i64 = db.query_row("SELECT count(*) FROM code_design_snapshots WHERE kind='component'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
    }
    #[test]
    fn the_collect_script_is_a_single_expression() {
        assert!(COLLECT.trim_start().starts_with("(() =>"));
        assert!(COLLECT.trim_end().ends_with("})()"));
        assert!(COLLECT.contains("__reactFiber$"));
    }
    #[test]
    fn find_browser_reports_where_it_looked_when_nothing_exists() {
        std::env::set_var("ANTILOKI_CHROME", "/nonexistent/chrome-for-this-test");
        let r = find_browser();
        std::env::remove_var("ANTILOKI_CHROME");
        match r { Ok(_) => { /* a real browser on this box — fine */ } Err(e) => assert!(e.contains("Looked at:") && e.contains("/nonexistent/chrome-for-this-test")) }
    }
}
