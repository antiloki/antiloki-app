//! ANTIGRAVITY'S OWN QUOTA, READ THROUGH ITS OWN CLI (operator 2026-11-26: "i want to see usage for all providers"). The interactive `agy` has a `/usage` screen, "Models & Quota", with the share REMAINING of a
//! weekly and a 5-hour limit for each model group (seen live on agy 1.5.18). As with Cursor (`cursor_usage`), this module does what a person would do: opens the CLI in a pseudo-terminal, types `/usage`, reads
//! the screen, closes it. No login token is read and no model is called. The Gemini group's two limits are the `week` and `5h` rows of the `gemini` house, drawn like Claude's.
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const TTL_MS: i64 = 20 * 60_000;
const BACKOFF_MS: i64 = 15 * 60_000;

#[derive(Clone, Debug, PartialEq)]
pub struct Window { pub group: String, pub label: &'static str, pub used_pct: f64, pub resets_in_s: Option<i64> }
#[derive(Clone, Debug, PartialEq)]
pub struct AgyUsage { pub windows: Vec<Window>, pub at_ms: i64 }

/// The screen as lines: a cursor move starts a new line, a move right is a space, colours are dropped.
fn plain(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut it = raw.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' { out.push(if c == '\r' { '\n' } else { c }); continue }
        match it.peek() {
            Some('[') => { it.next(); while let Some(&x) = it.peek() { it.next(); if x.is_ascii_alphabetic() || x == '~' { match x { 'H' | 'f' | 'E' | 'B' | 'd' => out.push('\n'), 'C' | 'G' => out.push(' '), _ => {} } break } } }
            Some(']') => { it.next(); while let Some(x) = it.next() { if x == '\x07' { break } if x == '\x1b' { it.next(); break } } }
            Some(_) => { it.next(); }
            None => {}
        }
    }
    out
}

/// "6d 23h 46m" as seconds.
fn span_s(text: &str) -> Option<i64> {
    let mut s = 0i64; let mut seen = false;
    for w in text.split_whitespace() {
        let (n, unit) = w.split_at(w.find(|c: char| !c.is_ascii_digit()).unwrap_or(w.len()));
        let Ok(n) = n.parse::<i64>() else { break };
        s += n * match unit { "d" => 86400, "h" => 3600, "m" => 60, "s" => 1, _ => break }; seen = true;
    }
    seen.then_some(s)
}

/// The limits on a captured "Models & Quota" screen (its last paint). None when the screen holds none. Pure - tested.
pub fn parse(screen: &str, now_ms: i64) -> Option<AgyUsage> {
    let text = plain(screen);
    let panel = &text[text.rfind("Models & Quota")?..];
    let (mut group, mut open): (String, Option<&'static str>) = (String::new(), None);
    let mut windows: Vec<Window> = vec![];
    for line in panel.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.ends_with(" MODELS") { group = line.trim_end_matches(" MODELS").to_string(); open = None; }
        else if line.contains("Weekly Limit") && !line.contains(',') { open = Some("week"); }
        else if line.contains("Five Hour Limit") && !line.contains(',') { open = Some("5h"); }
        else if let Some(rest) = line.split("Refreshes in").nth(1) { if let Some(w) = windows.last_mut() { if w.group == group && w.resets_in_s.is_none() { w.resets_in_s = span_s(rest); } } }
        else if let (Some(label), Some(i)) = (open, line.find('%')) {
            // the share REMAINING, so used = 100 - it
            let digits: String = line[..i].chars().rev().take_while(|c| c.is_ascii_digit() || *c == '.').collect::<Vec<_>>().into_iter().rev().collect();
            if let Ok(left) = digits.parse::<f64>() { if (0.0..=100.0).contains(&left) && !group.is_empty() { windows.push(Window { group: group.clone(), label, used_pct: ((100.0 - left) * 100.0).round() / 100.0, resets_in_s: None }); open = None; } }
        }
    }
    (!windows.is_empty()).then_some(AgyUsage { windows, at_ms: now_ms })
}

impl AgyUsage {
    /// `week` and `5h` are the Gemini group (what an Antigravity pane runs on by default); every other group is named in its label.
    pub fn rows(&self) -> Vec<Value> {
        self.windows.iter().map(|w| {
            let label = if w.group == "GEMINI" { w.label.to_string() } else { format!("{} ({})", w.label, w.group.to_lowercase()) };
            json!({ "house": "gemini", "label": label, "used_pct": w.used_pct, "resets_at": w.resets_in_s.map(|s| self.at_ms / 1000 + s), "note": format!("Antigravity's own /usage: {} models, {:.0} % used", w.group.to_lowercase(), w.used_pct) })
        }).collect()
    }
}

fn cache() -> &'static Mutex<(Option<AgyUsage>, i64)> { static C: OnceLock<Mutex<(Option<AgyUsage>, i64)>> = OnceLock::new(); C.get_or_init(Default::default) }
fn inflight() -> &'static AtomicBool { static F: AtomicBool = AtomicBool::new(false); &F }

/// The folder the reader runs in: antiloki's own, empty.
fn probe_dir() -> std::path::PathBuf {
    let home = std::env::var_os("ANTILOKI_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|h| std::path::PathBuf::from(h).join(".antiloki")).unwrap_or_else(std::env::temp_dir));
    home.join("usage-probe")
}

/// Open `agy` in a pseudo-terminal, type `/usage`, read the panel, close it. Blocking (up to ~50 s); call from a thread. It never answers a question the CLI asks.
pub fn probe() -> Result<AgyUsage, String> {
    use std::io::Write;
    let bin = crate::proc::which("agy").ok_or("agy is not installed")?;
    let dir = probe_dir(); std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let pair = portable_pty::native_pty_system().openpty(portable_pty::PtySize { rows: 90, cols: 120, pixel_width: 0, pixel_height: 0 }).map_err(|e| e.to_string())?;
    let mut b = if cfg!(windows) && bin.extension().map_or(true, |e| !e.eq_ignore_ascii_case("exe")) { let mut b = portable_pty::CommandBuilder::new(std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into())); b.arg("/c"); b.arg(&bin); b } else { portable_pty::CommandBuilder::new(&bin) };
    b.cwd(&dir); b.env("TERM", "xterm-256color"); b.env("BROWSER", "true");   // a signed-out CLI must not open a login page on its own
    let mut child = pair.slave.spawn_command(b).map_err(|e| e.to_string())?;
    let (buf, mut writer) = crate::hidden_term::attach(pair.master.try_clone_reader().map_err(|e| e.to_string())?, pair.master.take_writer().map_err(|e| e.to_string())?);
    let screen = || String::from_utf8_lossy(&buf.lock().unwrap_or_else(|p| p.into_inner())).to_string();
    let wait = |needle: &str, secs: u64| { for _ in 0..(secs * 5) { if plain(&screen()).contains(needle) || crate::hidden_term::drawn(screen().as_bytes(), 90, 120).contains(needle) { return true } std::thread::sleep(Duration::from_millis(200)); } false };
    let result = (|| {
        if !wait("for shortcuts", 25) { return Err("no usage panel: agy did not reach its prompt (not signed in, or it asks a question)".to_string()); }
        std::thread::sleep(Duration::from_millis(3000));
        // a tip box may cover the prompt on a first start
        writer.write_all(b"\x1b").map_err(|e| e.to_string())?; std::thread::sleep(Duration::from_millis(800));
        writer.write_all(b"/usage").map_err(|e| e.to_string())?; std::thread::sleep(Duration::from_millis(1500));
        writer.write_all(b"\r").map_err(|e| e.to_string())?;
        wait("Five Hour Limit Remaining", 20); std::thread::sleep(Duration::from_millis(3000));
        parse(&screen(), crate::cli::now_ms()).or_else(|| parse(&crate::hidden_term::drawn(screen().as_bytes(), 90, 120), crate::cli::now_ms())).ok_or_else(|| "no usage panel on the screen (not signed in, or a changed layout)".to_string())
    })();
    let _ = child.kill();
    result
}

/// The measured limits when a reading is fresh; starts a background probe when it is not (never blocks, never in a test build, never twice at once). Empty until the first probe lands.
pub fn rows_now() -> Vec<Value> {
    let (cached, failed_at) = cache().lock().unwrap_or_else(|p| p.into_inner()).clone();
    let now = crate::cli::now_ms();
    let fresh = cached.as_ref().is_some_and(|c| now - c.at_ms < TTL_MS);
    if !cfg!(test) && !fresh && !(failed_at > 0 && now - failed_at < BACKOFF_MS) && !inflight().swap(true, Ordering::SeqCst) {
        std::thread::spawn(|| {
            match probe() {
                Ok(u) => { *cache().lock().unwrap_or_else(|p| p.into_inner()) = (Some(u), 0); }
                Err(e) => { eprintln!("antiloki-serve: Antigravity usage: {e}"); cache().lock().unwrap_or_else(|p| p.into_inner()).1 = crate::cli::now_ms(); }
            }
            inflight().store(false, Ordering::SeqCst);
        });
    }
    cached.filter(|c| now - c.at_ms < 24 * 3_600_000).map(|c| c.rows()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    /// By hand, on a machine with a signed-in `agy`: `cargo test -p antiloki-serve --lib live_antigravity_usage -- --ignored --nocapture`. No model is called.
    #[test] #[ignore] fn live_antigravity_usage() { match probe() { Ok(u) => for w in u.windows { println!("{} {} {}", w.group, w.label, w.used_pct) }, Err(e) => panic!("{e}") } }
    #[test]
    fn the_quota_screen_gives_used_shares_per_group_with_their_reset() {
        let s = "\x1b[2J> /usage\n└ Models & Quota\n Account:\n me@x\n GEMINI MODELS\n Models within this group: Gemini 4\n Weekly Limit Remaining\n [████░] 98.94%\n Refreshes in 6d 23h 46m\n Five Hour Limit Remaining\n [█████] 75.00%\n Refreshes in 4h 1m\n CLAUDE AND GPT MODELS\n Weekly Limit Remaining\n [█████] 100.00%\n Quota available\n Models share a weekly limit and a 5-hour limit, 5% maybe.\n";
        let u = parse(s, 1_000_000).unwrap();
        assert_eq!(u.windows.len(), 3);
        assert_eq!((u.windows[0].label, u.windows[0].used_pct, u.windows[0].resets_in_s), ("week", 1.06, Some(6 * 86400 + 23 * 3600 + 46 * 60)));
        assert_eq!((u.windows[1].label, u.windows[1].used_pct, u.windows[1].resets_in_s), ("5h", 25.0, Some(4 * 3600 + 60)));
        let rows = u.rows(); assert_eq!((rows[0]["house"].as_str(), rows[0]["label"].as_str()), (Some("gemini"), Some("week"))); assert_eq!(rows[2]["label"], "week (claude and gpt)");
        assert!(parse("nothing here", 0).is_none() && parse("Models & Quota\nloading", 0).is_none());
    }
}
