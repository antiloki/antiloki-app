//! CURSOR'S OWN USAGE METER, READ THROUGH CURSOR'S OWN CLI (operator 2026-10-10: "ask cursor where we are in my limit ... or use the command to display the weekly usage").
//!
//! Cursor publishes no usage figure in a file, a subcommand or a headless stream, and a model cannot know its own quota (asked directly: "I cannot see your usage"). But the INTERACTIVE
//! `cursor-agent` has a slash command, `/usage`, that prints the plan meters ("Included 100% used · Auto 100% · API 82% · Resets Oct 27"). It is not available with `-p`. So this
//! module does what a person would do: opens the CLI in a pseudo-terminal, types `/usage`, reads the screen, closes it. No login token is read, no model is called (so no quota is spent),
//! nothing is typed except that one command. The meters it returns are MEASURED (`measured: true`), unlike antiloki's own dollar estimate (`fuel::allowance_rows`), which they replace for
//! the cursor house whenever they are fresh.
//!
//! The probe runs on its own thread, never on a request path; a failure is remembered for a while (a seat that is not signed in must not be re-probed on every fuel read).
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// How long a reading is trusted before the next one is taken (the meter moves with use, not by the minute).
const TTL_MS: i64 = 20 * 60_000;
/// After a failed probe, how long to leave it alone.
const BACKOFF_MS: i64 = 10 * 60_000;

#[derive(Clone, Debug, PartialEq)]
pub struct CursorUsage {
    pub tier: String,
    pub included_pct: Option<f64>,
    pub auto_pct: Option<f64>,
    pub api_pct: Option<f64>,
    pub on_demand: bool,
    pub resets_at_s: Option<i64>,
    pub at_ms: i64,
}

fn strip_ansi(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut it = raw.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' { out.push(if c == '\r' { '\n' } else { c }); continue }
        match it.peek() {
            Some('[') => { it.next(); while let Some(&x) = it.peek() { it.next(); if x.is_ascii_alphabetic() { break } } }
            Some(']') => { it.next(); while let Some(x) = it.next() { if x == '\x07' { break } if x == '\x1b' { it.next(); break } } }
            Some(_) => { it.next(); it.next(); }
            None => {}
        }
    }
    out
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm), so no date crate is needed for one reset day.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}
fn year_of(now_s: i64) -> i64 {
    // civil year of a unix time: good enough for "which year is the next Oct 27 in"
    let z = now_s.div_euclid(86400) + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    yoe + era * 400 + if m <= 2 { 1 } else { 0 }
}

/// "Oct 27" -> the next 27 October (UTC midnight) at or after `now_s`.
fn reset_epoch(text: &str, now_s: i64) -> Option<i64> {
    let mut w = text.split_whitespace();
    let mon = w.next()?.trim_end_matches('.').to_ascii_lowercase();
    let day: i64 = w.next()?.trim_end_matches(|c: char| !c.is_ascii_digit()).parse().ok()?;
    let m = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"].iter().position(|x| mon.starts_with(x))? as i64 + 1;
    if !(1..=31).contains(&day) { return None }
    let y = year_of(now_s);
    let t = days_from_civil(y, m, day) * 86400;
    Some(if t + 86400 < now_s { days_from_civil(y + 1, m, day) * 86400 } else { t })
}

fn pct_of(line: &str) -> Option<f64> {
    let i = line.find("% used")?;
    let head = &line[..i];
    let digits: String = head.chars().rev().take_while(|c| c.is_ascii_digit() || *c == '.').collect::<Vec<_>>().into_iter().rev().collect();
    digits.parse().ok()
}

/// The meters on a captured `/usage` screen (the LAST paint of it - the terminal stream repaints the panel several times). None when no usage panel is on the screen. Pure - tested.
pub fn parse(screen: &str, now_ms: i64) -> Option<CursorUsage> {
    let text = strip_ansi(screen);
    let i = text.rfind("Usage •")?;
    let panel = &text[i..];
    let first = panel.lines().next()?;
    let tier = first.trim_start_matches("Usage •").split("Resets").next().unwrap_or("").trim().to_string();
    let resets_at_s = first.split("Resets").nth(1).and_then(|r| reset_epoch(r.trim(), now_ms / 1000));
    let (mut included, mut auto, mut api, mut on_demand) = (None, None, None, false);
    for l in panel.lines().skip(1).take(14) {
        let t = l.trim_start();
        if t.starts_with("Included") { included = pct_of(t) }
        else if t.starts_with("Auto") { auto = pct_of(t) }
        else if t.starts_with("API") { api = pct_of(t) }
        else if t.starts_with("On-Demand") { on_demand = !t.contains("Disabled") }
    }
    if included.is_none() && auto.is_none() && api.is_none() { return None }
    Some(CursorUsage { tier, included_pct: included, auto_pct: auto, api_pct: api, on_demand, resets_at_s, at_ms: now_ms })
}

impl CursorUsage {
    /// The window rows the preview, the limits window and the router read (`house`, `label`, `used_pct`, `resets_at`): one per meter Cursor shows.
    pub fn rows(&self) -> Vec<Value> {
        let resets = self.resets_at_s.map(Value::from).unwrap_or(Value::Null);
        [("included", self.included_pct, "all of the included usage"), ("auto", self.auto_pct, "the Auto / Composer pool"), ("api", self.api_pct, "the named-model (API) pool")].into_iter()
            .filter_map(|(label, pct, what)| pct.map(|p| json!({ "house": "cursor", "label": label, "used_pct": p, "resets_at": resets, "measured": true,
                "note": format!("Cursor's own meter (/usage, {} plan): {what}, {p:.0} % used{}", if self.tier.is_empty() { "?" } else { &self.tier }, if self.on_demand { "" } else { "; on-demand is off" }) })))
            .collect()
    }
}

fn cache() -> &'static Mutex<(Option<CursorUsage>, i64)> { static C: OnceLock<Mutex<(Option<CursorUsage>, i64)>> = OnceLock::new(); C.get_or_init(Default::default) }
fn inflight() -> &'static AtomicBool { static F: AtomicBool = AtomicBool::new(false); &F }
fn now_ms() -> i64 { crate::cli::now_ms() }

/// What `cursor-agent status` says about the sign-in: `Some(true)` for "Logged in as ...", `Some(false)` for "Not logged in", `None` for anything else. Pure - tested.
pub fn status_signed_in(text: &str) -> Option<bool> {
    let t = text.to_lowercase();
    // "Logged in (unable to fetch user details)" is NOT a sign-in (found 2026-11-26: that answer came with a token the CLI then refused, "Authentication required", and every start opened the login page)
    if t.contains("not logged in") || t.contains("unable to fetch user details") { Some(false) } else if t.contains("logged in") { Some(true) } else { None }
}
/// THE SIGN-IN, ASKED OF THE CLI (2026-11-24: the file `~/.config/cursor/auth.json` was rewritten to a stub with no token, while the CLI knew the truth, so the file check showed Cursor as signed in). Cached for five minutes, because the router asks often and a status check takes a second. `None` when the CLI is not installed or does not answer: the caller falls back to the file.
fn signed_in_cache() -> &'static Mutex<Option<(i64, Option<bool>)>> { static C: OnceLock<Mutex<Option<(i64, Option<bool>)>>> = OnceLock::new(); C.get_or_init(Default::default) }
pub fn signed_in_cached() -> Option<bool> {
    let now = now_ms();
    if let Some((at, v)) = *signed_in_cache().lock().unwrap_or_else(|p| p.into_inner()) { if now - at < 5 * 60_000 { return v; } }
    let v = crate::proc::which("cursor-agent").and_then(|bin| cursor_status_text(&bin, 4)).and_then(|t| status_signed_in(&t));
    // SIGNED IN AFTER A FAILED READ (2026-11-24: the usage meter stayed empty for six hours after the person signed in, because the failures before it had set that long wait): the wait is cleared, so the meter is read now
    let before = signed_in_cache().lock().unwrap_or_else(|p| p.into_inner()).map(|(_, b)| b);
    if v == Some(true) && before != Some(Some(true)) { cache().lock().unwrap_or_else(|p| p.into_inner()).1 = 0; }
    *signed_in_cache().lock().unwrap_or_else(|p| p.into_inner()) = Some((now, v));
    v
}
/// When a failed read may be tried again. Most failures wait `BACKOFF_MS`; one that says there was no usage panel (a sign-in nobody can see from here, or a layout that changed) waits about six hours, because each try opens
/// the Cursor CLI and, signed out, its login page. The stamp is moved into the future by the difference, since the check is `now - stamp < BACKOFF_MS`. Pure - tested.
fn fail_stamp(err: &str, now: i64) -> i64 { if err.contains("no usage panel") || err.contains("not signed in") { now + 6 * 60 * 60_000 - BACKOFF_MS } else { now } }

/// What `cursor-agent status` printed: is it the "Not logged in" answer? Pure - tested.
fn status_text_says_signed_out(text: &str) -> bool { status_signed_in(text) == Some(false) }
/// Ask the CLI itself, with no terminal and a 10 s limit; `BROWSER` is neutralised so even a surprise cannot open a page. Any failure to ask (no output, a timeout) is "unknown", never "signed out".
fn cursor_status_says_signed_out(bin: &std::path::Path) -> bool {
    cursor_status_text(bin, 10).map_or(false, |t| status_text_says_signed_out(&t))
}
/// The text `cursor-agent status` prints (stdout and stderr), or `None` when it cannot be asked in `secs` seconds (the seat check, which request handlers wait on, asks for 4; the usage probe, on its own thread, for 10).
fn cursor_status_text(bin: &std::path::Path, secs: u64) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = crate::proc::cmd(bin).arg("status").env("BROWSER", "true").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().ok()?;
    let t0 = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed() < Duration::from_secs(secs) => std::thread::sleep(Duration::from_millis(100)),
            _ => { let _ = child.kill(); let _ = child.wait(); return None; }
        }
    }
    let mut text = String::new();
    if let Some(mut o) = child.stdout.take() { let _ = o.read_to_string(&mut text); }
    if let Some(mut e) = child.stderr.take() { let _ = e.read_to_string(&mut text); }
    Some(text)
}

/// What to start in the pseudo-terminal for the CLI at `bin`: the program itself, or on Windows (for anything that is not an `.exe`, a `.cmd` or `.ps1` shim) `cmd /c <bin>`. Pure - tested.
fn pty_program(bin: &std::path::Path, windows: bool, comspec: Option<String>) -> (std::ffi::OsString, Vec<std::ffi::OsString>) {
    let is_exe = bin.extension().map_or(false, |e| e.eq_ignore_ascii_case("exe"));
    if windows && !is_exe { (comspec.filter(|c| !c.is_empty()).unwrap_or_else(|| "cmd.exe".into()).into(), vec!["/c".into(), bin.as_os_str().to_os_string()]) } else { (bin.as_os_str().to_os_string(), vec![]) }
}
/// Open `cursor-agent` in a pseudo-terminal, type `/usage`, read the panel, close it. Blocking (up to ~35 s); call from a thread.
pub fn probe() -> Result<CursorUsage, String> {
    use std::io::Write;
    // NEVER START CURSOR WHEN IT IS NOT SIGNED IN (operator 2026-11-23: "it keeps opening cursor login, this is annoying"): `cursor-agent` with no sign-in goes straight to its login page and opens the browser, and this probe
    // ran again every time its back-off ended. Only a seat that is KNOWN signed out stops it (an unsure platform, or a keyring login, still reads the meter).
    if crate::cli::engine_seat(crate::cli::Engine::Cursor) == Some(false) { return Err("Cursor is not signed in: not starting it, because it would open its login page".into()); }
    let bin = crate::proc::which("cursor-agent").ok_or("cursor-agent is not installed")?;
    // THE SIGN-IN FILE CAN LIE (2026-11-24, a real run): `~/.config/cursor/auth.json` held only `{"storedIn": ...}` (no token) while `cursor-agent status` said "Not logged in", so the file check above passed
    // and the terminal below opened the login page on every serve start. `status` is the CLI's own answer, needs no terminal and opens nothing.
    if cursor_status_says_signed_out(&bin) { return Err("Cursor is not signed in: not starting it, because it would open its login page".into()); }
    let pair = portable_pty::native_pty_system().openpty(portable_pty::PtySize { rows: 50, cols: 140, pixel_width: 0, pixel_height: 0 }).map_err(|e| e.to_string())?;
    // ON WINDOWS THE CLI IS A SHIM (`agent.cmd`) AND THE PSEUDO-TERMINAL NEEDS A REAL PROGRAM (found 2026-11-25: a friend's Cursor meter never appeared): it is run through `cmd /c`, as the terminal panes do
    let (program, lead) = pty_program(&bin, cfg!(windows), std::env::var("ComSpec").ok());
    let mut b = portable_pty::CommandBuilder::new(program);
    for a in lead { b.arg(a); }
    b.cwd(std::env::temp_dir());
    b.env("TERM", "xterm-256color");
    for k in ["CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_ENTRYPOINT"] { b.env_remove(k); }
    let mut child = pair.slave.spawn_command(b).map_err(|e| e.to_string())?;
    let (buf, mut writer) = crate::hidden_term::attach(pair.master.try_clone_reader().map_err(|e| e.to_string())?, pair.master.take_writer().map_err(|e| e.to_string())?);
    let seen = |needle: &str| { let all = buf.lock().unwrap_or_else(|p| p.into_inner()); String::from_utf8_lossy(&all).contains(needle) || crate::hidden_term::drawn(&all, 50, 140).contains(needle) };
    let wait = |needle: &str, secs: u64| { for _ in 0..(secs * 5) { if seen(needle) { return true } std::thread::sleep(Duration::from_millis(200)); } false };
    // the prompt box is up when the "→" marker has been drawn; a slow start is given 20 s. A MACHINE WITH AN MCP SERVER IN CURSOR'S OWN SETTINGS IS ASKED TO APPROVE IT FIRST (2026-11-26, antiloki's own entry): typing
    // "/usage" into that question reads nothing, and its "a" would approve. The reader continues WITHOUT approval: it needs no tools.
    for _ in 0..100 { if seen("\u{2192}") || seen("[c]") { break } std::thread::sleep(Duration::from_millis(200)); }
    if seen("[c]") && !seen("\u{2192}") { std::thread::sleep(Duration::from_millis(600)); let _ = writer.write_all(b"c"); wait("\u{2192}", 20); }
    std::thread::sleep(Duration::from_millis(800));
    let result = (|| {
        writer.write_all(b"/usage").map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(1500));
        writer.write_all(b"\r").map_err(|e| e.to_string())?;
        wait("Esc to close", 15);
        std::thread::sleep(Duration::from_millis(300));
        let all = buf.lock().unwrap_or_else(|p| p.into_inner()).clone();
        parse(&crate::hidden_term::drawn(&all, 50, 140), now_ms()).or_else(|| parse(&String::from_utf8_lossy(&all), now_ms())).ok_or_else(|| "no usage panel on the screen (not signed in, or a changed layout)".to_string())
    })();
    let _ = child.kill();
    result
}

/// Read the meter NOW and keep it (the first-run seat check waits for this; `rows_now` does the same in the background). Blocking up to ~35 s: call it from a blocking thread.
pub fn refresh_now() -> Result<CursorUsage, String> {
    let r = probe();
    match &r {
        Ok(u) => { *cache().lock().unwrap_or_else(|p| p.into_inner()) = (Some(u.clone()), 0); }
        Err(e) => { cache().lock().unwrap_or_else(|p| p.into_inner()).1 = fail_stamp(e, now_ms()); }
    }
    r
}

/// The last reading, if it is of today; never starts one (the meter is read on the person's click or the hourly look, not because a pane is open).
pub fn rows_cached() -> Vec<Value> { let g = cache().lock().unwrap_or_else(|p| p.into_inner()); g.0.as_ref().filter(|c| now_ms() - c.at_ms < 24 * 3_600_000).map(|c| c.rows()).unwrap_or_default() }

/// The measured Cursor meters when a reading is fresh; starts a background probe when it is not (never blocks, never in a test build, never twice at once). Empty until the first probe lands.
pub fn rows_now() -> Vec<Value> {
    let (cached, failed_at) = { let g = cache().lock().unwrap_or_else(|p| p.into_inner()); g.clone() };
    let now = now_ms();
    let fresh = cached.as_ref().map(|c| now - c.at_ms < TTL_MS).unwrap_or(false);
    let backing_off = failed_at > 0 && now - failed_at < BACKOFF_MS;
    if !cfg!(test) && !fresh && !backing_off && !inflight().swap(true, Ordering::SeqCst) {
        std::thread::spawn(|| {
            match probe() {
                Ok(u) => { *cache().lock().unwrap_or_else(|p| p.into_inner()) = (Some(u), 0); }
                Err(e) => { cache().lock().unwrap_or_else(|p| p.into_inner()).1 = fail_stamp(&e, now_ms()); }
            }
            inflight().store(false, Ordering::SeqCst);
        });
    }
    // a reading older than a day says nothing about today
    cached.filter(|c| now - c.at_ms < 24 * 3_600_000).map(|c| c.rows()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn on_windows_a_cmd_shim_is_started_through_cmd_and_an_exe_or_any_unix_program_as_it_is() {
        use std::ffi::OsString; let p = |s: &str| std::path::PathBuf::from(s);
        assert_eq!(pty_program(&p(r"C:\Users\Me\AppData\Local\cursor-agent\agent.cmd"), true, Some(r"C:\Windows\System32\cmd.exe".into())), (OsString::from(r"C:\Windows\System32\cmd.exe"), vec![OsString::from("/c"), OsString::from(r"C:\Users\Me\AppData\Local\cursor-agent\agent.cmd")]));
        assert_eq!(pty_program(&p(r"C:\tools\agent.EXE"), true, None), (OsString::from(r"C:\tools\agent.EXE"), vec![]), "a real executable is started directly");
        assert_eq!(pty_program(&p("/home/me/.local/bin/cursor-agent"), false, None), (OsString::from("/home/me/.local/bin/cursor-agent"), vec![]), "and so is everything on Linux and macOS");
        assert_eq!(pty_program(&p(r"C:\x\agent.cmd"), true, None).0, OsString::from("cmd.exe"), "no ComSpec: plain cmd.exe");
    }

    #[test]
    fn a_failed_read_that_means_no_panel_waits_hours_and_any_other_waits_the_short_backoff() {
        let now = 1_000_000_000_000i64;
        assert_eq!(fail_stamp("the usage command failed", now), now);
        assert!(now - fail_stamp("no usage panel on the screen (not signed in, or a changed layout)", now) < 0, "stamped into the future: it stays backed off");
        assert!(fail_stamp("Cursor is not signed in: not starting it", now) + BACKOFF_MS - now >= 5 * 60 * 60_000, "about six hours");
    }

    #[test]
    fn the_cli_status_decides_the_sign_in_and_only_an_unanswered_question_is_unknown() {
        assert_eq!(status_signed_in("✓ Logged in as someone@example.com\nAuthentication tokens stored securely."), Some(true));
        assert_eq!(status_signed_in("Not logged in\n"), Some(false));
        assert_eq!(status_signed_in("\u{1b}[31mNot Logged In\u{1b}[0m"), Some(false));
        assert_eq!(status_signed_in(""), None, "silence is not an answer");
        assert_eq!(status_signed_in("✓ Login successful!\nLogged in (unable to fetch user details)"), Some(false), "a token the CLI cannot use is no sign-in: nothing is started on it");
    }
    #[test]
    fn the_cli_status_decides_the_sign_in_and_silence_is_not_an_answer() {
        assert_eq!(status_signed_in("✓ Logged in as someone@example.com\nAuthentication tokens stored securely."), Some(true));
        assert_eq!(status_signed_in("Not logged in\n"), Some(false));
        assert_eq!(status_signed_in("\u{1b}[31mNot Logged In\u{1b}[0m"), Some(false));
        assert_eq!(status_signed_in(""), None);
    }
    #[test]
    fn the_status_answer_not_logged_in_is_signed_out_and_anything_else_is_not() {
        assert!(status_text_says_signed_out("Not logged in\n"));
        assert!(status_text_says_signed_out("\u{1b}[31mNot Logged In\u{1b}[0m"));
        assert!(!status_text_says_signed_out("Logged in as someone@example.com"));
        assert!(!status_text_says_signed_out(""));
    }

    #[test]
    fn cursor_is_not_started_when_it_is_known_to_be_signed_out() {
        let home = std::env::temp_dir().join(format!("antiloki-cursor-signedout-{}", std::process::id())); let _ = std::fs::remove_dir_all(&home); std::fs::create_dir_all(&home).unwrap();
        let was = std::env::var_os("HOME"); let key = std::env::var_os("CURSOR_API_KEY");
        unsafe { std::env::set_var("HOME", &home); std::env::remove_var("CURSOR_API_KEY"); }
        crate::cli::REAL_SEAT_CHECK.with(|c| c.set(true));
        let r = probe();
        crate::cli::REAL_SEAT_CHECK.with(|c| c.set(false));
        unsafe { if let Some(w) = was { std::env::set_var("HOME", w); } if let Some(k) = key { std::env::set_var("CURSOR_API_KEY", k); } }
        let _ = std::fs::remove_dir_all(&home);
        if cfg!(windows) { return; }   // on Windows a sign-in is not visible from disk, so it is tried
        assert!(r.unwrap_err().contains("not signed in"), "no pseudo-terminal was opened, so no login page");
    }

    use super::*;
    const SCREEN: &str = include_str!("cursor_usage_fixture.txt");

    #[test]
    fn reads_the_meters_off_a_real_usage_screen() {
        // 2026-10-10 12:00 UTC
        let u = parse(SCREEN, 1_791_633_600_000).expect("a usage panel");
        assert_eq!(u.tier, "Pro");
        assert_eq!((u.included_pct, u.auto_pct, u.api_pct), (Some(100.0), Some(100.0), Some(82.0)));
        assert!(!u.on_demand);
        assert_eq!(u.resets_at_s, Some(days_from_civil(2026, 10, 27) * 86400), "Oct 27 of this year");
    }
    #[test]
    fn the_last_paint_wins_and_no_panel_is_none() {
        let two = format!("Usage • Pro   Resets Oct 27\n Included        10% used\n\n{SCREEN}");
        assert_eq!(parse(&two, 1_791_633_600_000).and_then(|u| u.included_pct), Some(100.0));
        assert!(parse("Loading usage data...\n→ /usage", 0).is_none());
    }
    #[test]
    fn a_reset_day_already_past_means_next_year() {
        let dec = days_from_civil(2026, 12, 31) * 86400;
        assert_eq!(reset_epoch("Jan 5", dec), Some(days_from_civil(2027, 1, 5) * 86400));
        assert_eq!(reset_epoch("Oct 27", days_from_civil(2026, 10, 10) * 86400), Some(days_from_civil(2026, 10, 27) * 86400));
        assert_eq!(reset_epoch("Someday", 0), None);
    }
    #[test]
    fn the_rows_are_windows_the_router_reads() {
        let rows = parse(SCREEN, 1_791_633_600_000).unwrap().rows();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r["house"] == "cursor" && r["measured"] == true && r["resets_at"].is_i64()));
        let included = rows.iter().find(|r| r["label"] == "included").unwrap();
        assert_eq!(included["used_pct"].as_f64(), Some(100.0));
        // (the router's 95 % rule over exactly these fields is `cli::full_window`'s own test; set_windows is process-wide, so it is not called here)
    }
    #[test]
    fn ansi_is_stripped() {
        assert_eq!(strip_ansi("\x1b[1mUsage\x1b[0m \x1b]0;t\x07•"), "Usage •");
    }
}
