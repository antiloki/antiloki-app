//! THE SIGN-IN FLOW (operator 2026-10-11, Windows: "some AIs existed at local, but weren't logged. It must make a login flow for this case"). Until now antiloki only TOLD a user to open a terminal and run the
//! CLI's own login, and a seat that was installed but signed out was simply skipped. This opens that terminal for them, running that seat's own sign-in command (which opens the browser flow the vendor
//! built); the onboarding screen already looks again every few seconds, so the seat ticks by itself when the sign-in lands.
//!
//! antiloki never sees a credential: the CLI talks to its vendor in a terminal window the user owns, exactly as if they had typed it. The command comes from a fixed table (`engines::signin_for_found`), never
//! from the request, so the route cannot be asked to run anything else.
//!
//!   POST /api/v1/onboarding/signin {"engine": "claude-code"}  ->  {ok, command, how}  (a terminal window has opened)
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};

/// The command that signs a seat in, for the binary that was found. Codex has an explicit `login`; the table's other lines already are the login (Claude and Antigravity start their own sign-in when run
/// signed out, Cursor/Grok/OpenCode have a `login` subcommand). Pure - tested.
pub fn login_command(key: &str, found: Option<&str>) -> Option<String> {
    let c = match key { "codex" => "codex login".to_string(), _ => crate::engines::signin_for_found(key, found) };
    if c.is_empty() { None } else { Some(c) }
}

/// How to open a terminal window running `cmdline` and keep it open after it ends, on this kind of system: the program and its arguments. Windows is not here (it needs a raw command line, see `launch`).
/// `have` says which programs exist. Pure - tested.
pub fn linux_terminal_plan(cmdline: &str, have: &dyn Fn(&str) -> bool) -> Option<(String, Vec<String>)> {
    // the terminals people actually have, the generic launcher first; each takes "run this" in its own spelling
    const TERMS: &[(&str, &[&str])] = &[
        ("x-terminal-emulator", &["-e"]), ("gnome-terminal", &["--"]), ("konsole", &["-e"]), ("xfce4-terminal", &["-x"]),
        ("kitty", &[]), ("alacritty", &["-e"]), ("ghostty", &["-e"]), ("xterm", &["-e"]),
    ];
    // a window that closes the moment the command ends hides what it said, so it waits for Enter
    let script = format!("{cmdline}; echo; echo 'Signing in is finished (or stopped). You can close this window.'; read -r _");
    TERMS.iter().find(|(t, _)| have(t)).map(|(t, pre)| {
        let mut a: Vec<String> = pre.iter().map(|s| s.to_string()).collect();
        a.extend(["sh".to_string(), "-c".to_string(), script.clone()]);
        (t.to_string(), a)
    })
}

/// Open the terminal. The window belongs to the user from here on; this only starts it.
fn launch(cmdline: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // `start` opens a NEW console window even though this process has none; `cmd /k` keeps it open after the sign-in ends so its last line can be read. raw_arg: the line must reach cmd.exe as written
        let mut c = crate::proc::cmd("cmd.exe");
        c.raw_arg(format!("/c start \"antiloki sign-in\" cmd /k {cmdline}")).creation_flags(0x0800_0000); // CREATE_NO_WINDOW for this launcher only
        let mut child = c.spawn().map_err(|e| format!("could not open a terminal: {e}"))?;
        std::thread::spawn(move || { let _ = child.wait(); });
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let (prog, args) = linux_terminal_plan(cmdline, &|t| crate::proc::which(t).is_some())
            .ok_or("no terminal program found (tried x-terminal-emulator, gnome-terminal, konsole, xfce4-terminal, kitty, alacritty, ghostty, xterm): open a terminal yourself and run the command shown")?;
        let mut child = crate::proc::cmd(&prog).args(&args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .spawn().map_err(|e| format!("could not open {prog}: {e}"))?;
        std::thread::spawn(move || { let _ = child.wait(); });   // reaped when the user closes it
        Ok(())
    }
}

/// `POST /api/v1/onboarding/signin {engine}`. The seat must be a known one and installed; what runs is the fixed sign-in command for it.
pub async fn start(body: axum::body::Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let key = b.get("engine").and_then(Value::as_str).unwrap_or("");
    let bad = |m: String| (StatusCode::BAD_REQUEST, Json(json!({ "ok": false, "error": m }))).into_response();
    let Some(engine) = crate::cli::engine_of(&json!(key)) else { return bad(format!("`{key}` is not an AI antiloki drives")) };
    let Some(found) = engine.bin_present() else { return bad(format!("`{}` is not installed on this computer: install it first", engine.bin())) };
    let path = crate::proc::which(found).map(|p| p.to_string_lossy().to_string());
    let Some(cmdline) = login_command(engine.key(), path.as_deref()) else { return bad(format!("{} has no sign-in command", engine.key())) };
    let c2 = cmdline.clone();
    match tokio::task::spawn_blocking(move || launch(&c2)).await.unwrap_or_else(|e| Err(e.to_string())) {
        Ok(()) => Json(json!({ "ok": true, "engine": engine.key(), "command": cmdline, "how": "a terminal window opened: finish signing in there, then come back" })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "ok": false, "error": e, "command": cmdline }))).into_response(),
    }
}

/// Does an error read as "this seat is signed out" (so the screen should offer to sign in) rather than "this seat is broken"? Pure - tested.
pub fn looks_signed_out(error: &str) -> bool {
    let e = error.to_lowercase();
    ["log in", "login", "logged in", "sign in", "signed in", "sign-in", "not authenticated", "unauthenticated", "unauthorized", "authentication", "auth required", "401", "no credentials", "credentials", "api key", "run `agent login`", "please authenticate"]
        .iter().any(|k| e.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn each_seat_has_its_own_sign_in_command_and_cursor_follows_the_binary_found() {
        assert_eq!(login_command("codex", None).as_deref(), Some("codex login"));
        assert_eq!(login_command("claude-code", None).as_deref(), Some("claude"));
        assert_eq!(login_command("grok", None).as_deref(), Some("grok login"));
        assert_eq!(login_command("opencode", None).as_deref(), Some("opencode auth login"));
        assert_eq!(login_command("cursor", Some("C:\\Users\\u\\AppData\\Local\\cursor-agent\\agent.cmd")).as_deref(), Some("agent login"));
        assert_eq!(login_command("cursor", Some("/home/u/.local/bin/cursor-agent")).as_deref(), Some("cursor-agent login"));
        assert_eq!(login_command("nobody", None), None);
    }
    #[test]
    fn a_linux_terminal_is_chosen_in_order_and_the_window_stays_open() {
        let only = |names: &'static [&'static str]| move |t: &str| names.contains(&t);
        let (p, a) = linux_terminal_plan("agent login", &only(&["xterm", "konsole"])).unwrap();
        assert_eq!(p, "konsole"); assert_eq!(&a[..2], &["-e", "sh"]);
        assert!(a.last().unwrap().starts_with("agent login; ") && a.last().unwrap().ends_with("read -r _"), "{a:?}");
        let (p, a) = linux_terminal_plan("claude", &only(&["gnome-terminal"])).unwrap(); assert_eq!(p, "gnome-terminal"); assert_eq!(a[0], "--");
        let (p, a) = linux_terminal_plan("claude", &only(&["kitty"])).unwrap(); assert_eq!(p, "kitty"); assert_eq!(a[0], "sh", "kitty takes the command with no flag");
        assert!(linux_terminal_plan("claude", &only(&[])).is_none());
    }
    #[test]
    fn a_signed_out_seat_is_told_from_a_broken_one() {
        for yes in ["Please log in to continue", "Error: not authenticated. Run `agent login`", "401 Unauthorized", "You are not signed in", "no credentials found", "Invalid API key"] { assert!(looks_signed_out(yes), "{yes}"); }
        for no in ["it answered with nothing", "timed out after 120 s", "model not found", "rate limit reached"] { assert!(!looks_signed_out(no), "{no}"); }
    }
}
