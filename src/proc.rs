//! EVERY CHILD PROCESS THE SERVE STARTS.
//!
//! Operator 2026-09-24, from a Windows install: *"always when i click features keeps blinking terminals wich is
//! noise. They appear and disappear from nothing and looks very unprofessional."* antiloki is a GUI app, so on
//! Windows it owns no console; every process it starts is given a fresh one, which opens, flashes and closes.
//! There are forty spawn sites in this crate — eighteen of them `git`, most on the path a single click takes —
//! and the flag that suppresses the window, `CREATE_NO_WINDOW`, has to be on every one of them. That is the
//! reason this module exists: not to wrap `Command` for its own sake, but so that there is exactly one place
//! where a child process is born and the flag cannot be forgotten at the forty-first.
//!
//! The same install reported the other half: *"antiloki didnt discovery how to run, build and test scripts
//! automatically and features depending on that didnt work."* Part of that was discovery (see `gates.rs`), and
//! part was this: the gates ran the project's command through `sh -c`, and Windows has no `sh`. Every one of
//! those runs failed to start at all, which the UI could only report as the feature not working.
//!
//! On Unix nothing here changes behaviour — the flag does not exist, `shell()` is the same `sh -c` as before.

/// Windows `CREATE_NO_WINDOW`: the child gets no console, so no window appears for it.
/// <https://learn.microsoft.com/windows/win32/procthread/process-creation-flags>
#[cfg(windows)]
pub const NO_WINDOW: u32 = 0x0800_0000;

/// `cmd.output()` WITH A TIME LIMIT (issues 7/14, 2026-11-24): a probe that never finishes (a tool-manager shim waiting on the network, a CLI health-checking servers) must not hold a request for ever.
/// The child is killed at the limit and `None` is returned. Output is read on threads so a full pipe cannot stall the wait.
pub fn output_within(mut c: std::process::Command, limit: std::time::Duration) -> Option<std::process::Output> {
    let mut child = c.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().ok()?;
    let (a, b) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() < limit => std::thread::sleep(std::time::Duration::from_millis(25)),
            _ => { let _ = child.kill(); let _ = child.wait(); return None; }
        }
    };
    // THE PIPES MAY OUTLIVE THE PROGRAM (2026-11-27, a Windows machine whose agent list never came): a helper it left running (a background service) holds them open, so "read to the end" never ends. What was said is
    // taken as soon as both close, or two seconds after the program ended.
    let ended = std::time::Instant::now();
    while !(a.1.is_finished() && b.1.is_finished()) && ended.elapsed() < std::time::Duration::from_secs(2) { std::thread::sleep(std::time::Duration::from_millis(10)); }
    let take = |x: &std::sync::Mutex<Vec<u8>>| std::mem::take(&mut *x.lock().unwrap_or_else(|e| e.into_inner()));
    Some(std::process::Output { status, stdout: take(&a.0), stderr: take(&b.0) })
}

/// Read a pipe on a thread of its own, keeping what has arrived where it can be taken at any moment.
fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> (std::sync::Arc<std::sync::Mutex<Vec<u8>>>, std::thread::JoinHandle<()>) {
    let kept = std::sync::Arc::new(std::sync::Mutex::new(Vec::new())); let into = kept.clone();
    let h = std::thread::spawn(move || {
        let Some(mut p) = pipe else { return }; let mut buf = [0u8; 8192];
        while let Ok(n) = p.read(&mut buf) { if n == 0 { break; } into.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(&buf[..n]); }
    });
    (kept, h)
}


/// A WRAPPER THAT STARTS ITSELF (issue 6, 2026-11-24): a person's `~/.local/bin/claude` that is `mise use -g claude; exec mise x claude -- claude "$@"` is found first on PATH, and inside mise `claude` is that same
/// script again, so it re-runs for ever and Claude never starts. True for a text script that calls `mise x <its own name>`. Pure over the script's text.
pub fn is_self_calling_wrapper(name: &str, script: &str) -> bool {
    script.starts_with("#!") && script.lines().any(|l| { let l = l.trim(); (l.contains("mise x ") || l.contains("mise exec ")) && l.contains(name) })
}
/// The program to START for a bare agent name: the real binary when the one on PATH is a self-calling wrapper and mise has the tool installed; otherwise the name unchanged.
pub fn real_program(name: &str) -> String {
    if cfg!(windows) || name.contains('/') { return name.to_string() }
    let Some(found) = which(name) else { return name.to_string() };
    let text = { use std::io::Read; let mut buf = vec![0u8; 4096]; let n = match std::fs::File::open(&found).and_then(|mut f| f.read(&mut buf)) { Ok(n) => n, Err(_) => return name.to_string() }; buf.truncate(n); if !buf.starts_with(b"#!") { return name.to_string() } String::from_utf8_lossy(&buf).into_owned() };
    if !is_self_calling_wrapper(name, &text) { return name.to_string() }
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default();
    let base = std::env::var_os("MISE_DATA_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| home.join(".local/share/mise")).join("installs").join(name).join("latest");
    for c in [base.join(name), base.join("bin").join(name)] { if c.is_file() { return c.to_string_lossy().into_owned(); } }
    name.to_string()
}

/// A `std::process::Command` that will not flash a console on Windows. Use this instead of
/// `std::process::Command::new` everywhere in this crate.
pub fn cmd(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    #[allow(unused_mut)]
    let mut c = std::process::Command::new(resolve(program.as_ref()));
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(NO_WINDOW); }
    c
}

/// WHAT THE OPERATING SYSTEM WILL ACTUALLY START, given a bare program name.
///
/// Operator 2026-09-25, from the Windows install: *"even the chat is not working, the agent Main i sent a
/// message and the chat was empty"*, *"still couldnt run the preview"*, *"still has the issue of terminals
/// blinks opening"*. One cause under all three.
///
/// `std::process::Command::new("claude")` on Windows searches the PATH for `claude` and `claude.exe` — and
/// **nothing else**. It does not walk `PATHEXT`. Every one of these CLIs is installed by npm, and npm on
/// Windows installs a `.cmd` shim: there is no `claude.exe` to find. So the chat's engine never started, the
/// preview's `npm` never started, and the errors read as the feature being broken rather than the program
/// being unfindable.
///
/// Resolving it here hands Command a full path ending in `.cmd`, which the standard library knows to run
/// through `cmd.exe` (with the argument escaping that CVE-2024-24576 forced) — and because the constructors
/// above set `CREATE_NO_WINDOW`, that `cmd.exe` is the one that no longer flashes a console.
///
/// A name that is already a path, or that resolves to nothing, is passed through untouched so the OS can
/// report its own error rather than this function inventing one.
pub fn resolve(program: &std::ffi::OsStr) -> std::ffi::OsString {
    let name = program.to_string_lossy();
    if name.contains('/') || name.contains('\\') { return program.to_os_string() }
    // off Windows a bare name is left to the OS, except a name that has an alias: a machine with only `agent` must still start `cursor-agent`
    if !cfg!(windows) && !ALIASES.iter().any(|(from, _)| *from == name) { return program.to_os_string() }
    match which(&name) { Some(p) => p.into_os_string(), None => program.to_os_string() }
}

/// THE DIRECTORIES A DEV SCRIPT'S TOOLS LIVE IN. The serve may be started from a launcher, a service or a terminal with a bare `PATH`; the project's `npm run dev` then dies with "npm: command not found" (127),
/// and Preview and Frames read as broken. `PATH` with the usual tool homes added where they exist and are missing (mise's shims, `~/.local/bin`, cargo, volta, bun, Homebrew), after what is already there. Pure
/// over the paths it is given; `tool_path_extras` finds the real ones.
pub fn extend_path(path: &str, extras: &[std::path::PathBuf]) -> String {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let have: Vec<&str> = path.split(sep).filter(|p| !p.is_empty()).collect();
    let mut out: Vec<String> = have.iter().map(|p| p.to_string()).collect();
    for e in extras { let t = e.to_string_lossy().to_string(); if !t.is_empty() && !have.contains(&t.as_str()) && !out.contains(&t) { out.push(t); } }
    out.join(&sep.to_string())
}
/// The tool directories that exist on this machine.
pub fn tool_path_extras() -> Vec<std::path::PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from);
    let mut v: Vec<std::path::PathBuf> = vec![];
    if let Some(h) = &home { for d in [".local/share/mise/shims", ".local/bin", ".cargo/bin", ".volta/bin", ".bun/bin", ".asdf/shims", "go/bin"] { v.push(h.join(d)); } }
    for d in ["/opt/homebrew/bin", "/usr/local/bin"] { v.push(std::path::PathBuf::from(d)); }
    v.into_iter().filter(|p| p.is_dir()).collect()
}

/// The platform's shell, and the flag it takes to run one command line: `sh -c` on Unix, `cmd /C` on Windows.
/// Pure — the pair every `shell*` constructor is built from, here and in the serve's async twins.
pub fn shell_parts() -> (&'static str, &'static str) {
    if cfg!(windows) { ("cmd", "/C") } else { ("sh", "-c") }
}

/// The blocking twin, for the spawn-and-read-the-pipes paths.
pub fn shell_sync(line: &str) -> std::process::Command {
    let (sh, flag) = shell_parts();
    let mut c = cmd(sh);
    c.arg(flag).arg(line);
    c
}

/// Quote one path so the platform's shell sees it as a single argument.
///
/// This is not cosmetic. The gates build a command line naming the files a turn changed, and the Unix form —
/// single quotes — is not quoting on `cmd.exe`: it is two literal apostrophes wrapped around the path, so the
/// runner is handed a file name that does not exist and reports zero tests for a turn that changed ten files.
/// A path containing the platform's own quote character is refused rather than escaped, because there is no
/// escape for `"` inside a `cmd.exe` argument that survives every runner in between. Pure — tested.
pub fn quote(path: &str) -> Option<String> {
    if path.is_empty() { return None }
    if cfg!(windows) {
        if path.contains('"') || path.contains('%') { return None }
        Some(format!("\"{path}\""))
    } else {
        if path.contains('\'') { return None }
        Some(format!("'{path}'"))
    }
}

/// Quote a list of paths, dropping the ones no shell can carry. Pure — tested.
pub fn quote_all(paths: &[String]) -> Vec<String> { paths.iter().filter_map(|p| quote(p)).collect() }

/// IS THIS BINARY ON THE PATH, AND WHERE.
///
/// This used to be `which <bin>` in a subprocess, and `which` does not exist on Windows — `where.exe` does. So
/// on a Windows install every engine detection answered "not installed", and antiloki decided the machine had
/// no Claude, no Codex and no OpenCode on a machine that had them. It is also forty spawns of a console window
/// per session on the one platform that shows them.
///
/// Resolving the PATH here needs no child process at all. On Windows a name is tried against each extension in
/// PATHEXT, which is how the shell finds `claude.cmd` when the caller asked for `claude` — an npm-installed CLI
/// is a `.cmd` shim there and nothing else.
pub fn which(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    if let Some(p) = which_aliased(bin, &path) { return Some(p) }
    // A CLI installed AFTER antiloki opened is not on the PATH this process started with: Windows hands a new PATH only to processes started afterwards
    // (the onboarding's whole point is "install it, come back, it is detected"). On a miss, the registry's PATH and the CLIs' known install folders are
    // searched too, and merged into this process's PATH so the spawns that follow find it as well (2026-10-11).
    if cfg!(windows) {
        refresh_path_from_registry();
        let path = std::env::var_os("PATH").unwrap_or_default();
        if let Some(p) = which_aliased(bin, &path) { return Some(p) }
        let extra = std::env::join_paths(known_install_dirs()).unwrap_or_default();
        return which_aliased(bin, &extra);
    }
    // Linux and macOS: a desktop launcher (an app menu, an AppImage, a .desktop file) starts this process with a bare PATH, not the one the person's terminal has, so a Claude Code in `~/.local/bin`, an npm or
    // nvm folder, Homebrew or mise was "not installed" (2026-11-23, a new machine: "claude code is not installed even it is"). On a miss: the login shell's PATH, then the folders those CLIs install into.
    // a caller that limited the PATH on purpose (a test sandbox) says so: nothing outside it is looked at (found 2026-11-26: the journeys reached the developer's real `cursor-agent` and `agy` through this fallback)
    if std::env::var_os("ANTILOKI_NO_PATH_FALLBACK").is_some() { return None; }
    refresh_path_from_login_shell();
    let path = std::env::var_os("PATH").unwrap_or_default();
    if let Some(p) = which_aliased(bin, &path) { return Some(p) }
    let extra = std::env::join_paths(known_install_dirs_unix()).unwrap_or_default();
    which_aliased(bin, &extra)
}

/// Where the CLIs that antiloki drives install themselves on Linux and macOS, those that exist on this machine. Not a guess at a layout: the native installer's folder, npm / bun / volta / pnpm / yarn globals,
/// version managers (nvm, fnm, mise, asdf), Homebrew, snap and Nix.
pub fn known_install_dirs_unix() -> Vec<std::path::PathBuf> {
    let Some(h) = std::env::var_os("HOME").map(std::path::PathBuf::from) else { return vec![] };
    let mut v: Vec<std::path::PathBuf> = ["/usr/local/bin", "/opt/homebrew/bin", "/home/linuxbrew/.linuxbrew/bin", "/snap/bin"].iter().map(std::path::PathBuf::from).collect();
    for rel in [".local/bin", ".claude/local", ".npm-global/bin", ".bun/bin", ".volta/bin", ".cargo/bin", ".local/share/pnpm", ".yarn/bin", ".config/yarn/global/node_modules/.bin", ".asdf/shims", ".local/share/mise/shims", ".nix-profile/bin", "bin"] {
        v.push(h.join(rel));
    }
    // version managers keep one folder per Node version: every one of them counts (the newest first, so a newer install wins)
    for (base, tail) in [(".nvm/versions/node", "bin"), (".local/share/fnm/node-versions", "installation/bin"), (".local/share/mise/installs/node", "bin"), (".asdf/installs/nodejs", "bin")] {
        let mut vers: Vec<std::path::PathBuf> = std::fs::read_dir(h.join(base)).into_iter().flatten().flatten().map(|e| e.path()).collect();
        vers.sort(); vers.reverse();
        for d in vers { v.push(d.join(tail)); }
    }
    for tool in ["claude", "codex", "opencode"] { v.push(h.join(".local/share/mise/installs").join(tool).join("latest").join("bin")); v.push(h.join(".local/share/mise/installs").join(tool).join("latest")); }
    v.retain(|d| d.is_dir());
    v
}

/// WHAT WAS LOOKED AT, in words, so "it is installed but antiloki cannot find it" is something a person can read off the screen: the PATH this app started with, the login shell's, and the known install folders
/// (the first few, with the home folder written `~`). Pure over its inputs - tested.
pub fn format_search_report(path_folders: usize, known: &[std::path::PathBuf], home: Option<&std::path::Path>, windows: bool) -> String {
    let short = |p: &std::path::Path| -> String { match home.and_then(|h| p.strip_prefix(h).ok()) { Some(r) => format!("~{}{}", if windows { "\\" } else { "/" }, r.display()), None => p.display().to_string() } };
    let shown: Vec<String> = known.iter().take(6).map(|p| short(p)).collect();
    let more = known.len().saturating_sub(shown.len());
    format!("looked in the PATH this app started with ({path_folders} folders){} and {} known install folders ({}{})",
        if windows { ", then the registry's PATH," } else { ", then your login shell's PATH," }, known.len(), shown.join(", "), if more > 0 { format!(", and {more} more") } else { String::new() })
}
/// The report for this machine, now.
pub fn search_report() -> String {
    let n = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).count()).unwrap_or(0);
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from);
    let known = if cfg!(windows) { known_install_dirs() } else { known_install_dirs_unix() };
    format_search_report(n, &known, home.as_deref(), cfg!(windows))
}

/// The text a login shell printed between our markers -> the PATH it holds, so a banner or a motd the shell prints around it cannot get in. Pure - tested.
pub fn parse_marked_path(out: &str) -> Option<String> {
    let a = out.find("__ANTILOKI_PATH__")? + "__ANTILOKI_PATH__".len();
    let b = out[a..].find("__ANTILOKI_PATH__")? + a;
    let p = out[a..b].trim();
    if p.is_empty() { None } else { Some(p.to_string()) }
}

/// Linux and macOS: ask the person's own login shell for its PATH (the one their terminal has) and add what this process lacks. At most once every 30 seconds (every `which` miss lands here); the shell gets four
/// seconds and no keyboard, so a slow or interactive startup file cannot hang antiloki.
#[cfg(not(windows))]
fn refresh_path_from_login_shell() {
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    { let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner()); if l.map_or(false, |t| t.elapsed() < std::time::Duration::from_secs(30)) { return } *l = Some(std::time::Instant::now()); }
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into());
    // std::process::Command on purpose: `cmd()` resolves through `which`, which is the caller
    let Ok(mut child) = std::process::Command::new(&shell).args(["-ilc", "printf '__ANTILOKI_PATH__%s__ANTILOKI_PATH__' \"$PATH\""])
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn() else { return };
    let mut out = child.stdout.take();
    let reader = std::thread::spawn(move || { use std::io::Read; let mut s = String::new(); if let Some(o) = out.as_mut() { let _ = o.read_to_string(&mut s); } s });
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() { Ok(Some(_)) => break, Ok(None) if start.elapsed() < std::time::Duration::from_secs(4) => std::thread::sleep(std::time::Duration::from_millis(50)), _ => { let _ = child.kill(); let _ = child.wait(); break } }
    }
    let text = reader.join().unwrap_or_default();
    if let Some(extra) = parse_marked_path(&text) {
        let current = std::env::var("PATH").unwrap_or_default();
        if let Some(m) = merge_paths(&current, &extra, ':') { unsafe { std::env::set_var("PATH", m); } }
    }
}
#[cfg(windows)]
fn refresh_path_from_login_shell() {}

/// Names a CLI is also known by. Cursor's CLI is `agent` in Cursor's own installation docs (the Windows installer too); `cursor-agent` is the older name and the
/// one every call site here spells, so a machine that has only `agent` is still found as `cursor-agent` (only when `cursor-agent` itself is absent).
const ALIASES: &[(&str, &str)] = &[("cursor-agent", "agent")];

fn which_aliased(bin: &str, path: &std::ffi::OsStr) -> Option<std::path::PathBuf> {
    if let Some(p) = which_in(bin, path) { return Some(p) }
    ALIASES.iter().find(|(from, _)| *from == bin).and_then(|(_, to)| which_in(to, path))
}

fn which_in(bin: &str, path: &std::ffi::OsStr) -> Option<std::path::PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';').map(|e| e.trim().to_lowercase()).filter(|e| !e.is_empty()).collect()
    } else { vec![] };
    // On Windows a name with no extension is not runnable, so the bare spelling is only tried when the caller
    // gave one; everywhere else the bare spelling is the only one there is.
    let bare_ok = !cfg!(windows) || std::path::Path::new(bin).extension().is_some();
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() { continue }
        if bare_ok && is_exe(&dir.join(bin)) { return Some(dir.join(bin)) }
        for e in &exts {
            let c = dir.join(format!("{bin}{e}"));
            if is_exe(&c) { return Some(c) }
        }
    }
    None
}

/// Where the CLIs' own installers put them on Windows, for a PATH that has not caught up. Not a guess at a layout: the folders each vendor's installer documents.
/// (`%LOCALAPPDATA%\agy\bin` for Antigravity; `%USERPROFILE%\.local\bin` for Claude Code's native installer and Cursor's; `%APPDATA%\npm` for the npm ones.)
pub fn known_install_dirs() -> Vec<std::path::PathBuf> {
    let var = |k: &str| std::env::var_os(k).map(std::path::PathBuf::from);
    let mut v = vec![];
    if let Some(l) = var("LOCALAPPDATA") { v.push(l.join("agy").join("bin")); v.push(l.join("Programs").join("cursor-agent")); v.push(l.join("cursor-agent")); }
    if let Some(u) = var("USERPROFILE").or_else(|| var("HOME")) { v.push(u.join(".local").join("bin")); }
    if let Some(a) = var("APPDATA") { v.push(a.join("npm")); }
    v
}

/// The text of `reg query <key> /v Path` -> the Path value, or None. Pure - tested. The line looks like `    Path    REG_EXPAND_SZ    C:\a;%USERPROFILE%\b`.
pub fn parse_reg_path(out: &str) -> Option<String> {
    for line in out.lines() {
        let t = line.trim();
        let mut it = t.split_whitespace();   // `Path    REG_EXPAND_SZ    value`: runs of spaces, not single ones
        let Some(name) = it.next() else { continue };
        let kind = it.next().unwrap_or("");
        if name.eq_ignore_ascii_case("path") && (kind == "REG_SZ" || kind == "REG_EXPAND_SZ") {
            let value = t[name.len()..].trim_start().strip_prefix(kind)?.trim();
            if !value.is_empty() { return Some(value.to_string()) }
        }
    }
    None
}

/// `%NAME%` -> the variable's value, through `lookup`; an unknown name is left as written. Pure - tested.
pub fn expand_percent_vars(s: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new(); let mut rest = s;
    while let Some(a) = rest.find('%') {
        out.push_str(&rest[..a]);
        match rest[a + 1..].find('%') {
            Some(b) => { let name = &rest[a + 1..a + 1 + b]; match lookup(name) { Some(v) if !name.is_empty() => out.push_str(&v), _ => { out.push('%'); out.push_str(name); out.push('%'); } } rest = &rest[a + b + 2..]; }
            None => { out.push_str(&rest[a..]); rest = ""; }
        }
    }
    out.push_str(rest); out
}

/// `current` plus every entry of `extra` it does not already hold (compared case-insensitively, trailing separators ignored). None when nothing is new. Pure - tested.
pub fn merge_paths(current: &str, extra: &str, sep: char) -> Option<String> {
    let norm = |d: &str| d.trim().trim_end_matches(|c| c == '\\' || c == '/').to_lowercase();
    let have: std::collections::HashSet<String> = current.split(sep).map(norm).collect();
    let new: Vec<&str> = extra.split(sep).filter(|d| !d.trim().is_empty() && !have.contains(&norm(d))).collect();
    if new.is_empty() { return None }
    let mut m = current.trim_end_matches(sep).to_string();
    for d in new { if !m.is_empty() { m.push(sep); } m.push_str(d.trim()); }
    Some(m)
}

/// Windows: re-read the user and machine PATH from the registry and add what this process lacks. At most once every three seconds (every `which` miss lands here).
#[cfg(windows)]
fn refresh_path_from_registry() {
    use std::os::windows::process::CommandExt;
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    { let mut l = LAST.lock().unwrap_or_else(|e| e.into_inner()); if l.map_or(false, |t| t.elapsed() < std::time::Duration::from_secs(3)) { return } *l = Some(std::time::Instant::now()); }
    let mut extra = String::new();
    for key in ["HKCU\\Environment", "HKLM\\SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment"] {
        // std::process::Command on purpose: `cmd()` resolves through `which`, which is the caller
        let Ok(o) = std::process::Command::new("reg").args(["query", key, "/v", "Path"]).creation_flags(NO_WINDOW).output() else { continue };
        if let Some(v) = parse_reg_path(&String::from_utf8_lossy(&o.stdout)) {
            let v = expand_percent_vars(&v, &|n| std::env::var(n).ok());
            if !extra.is_empty() { extra.push(';'); } extra.push_str(&v);
        }
    }
    let current = std::env::var("PATH").unwrap_or_default();
    if let Some(m) = merge_paths(&current, &extra, ';') { unsafe { std::env::set_var("PATH", m); } }
}
#[cfg(not(windows))]
fn refresh_path_from_registry() {}

/// A file that exists and, on Unix, carries an execute bit. A directory is never it.
fn is_exe(p: &std::path::Path) -> bool {
    let Ok(md) = std::fs::metadata(p) else { return false };
    if !md.is_file() { return false }
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; md.permissions().mode() & 0o111 != 0 }
    #[cfg(not(unix))] { true }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn a_program_that_leaves_a_helper_holding_its_output_still_answers_in_time() {
        let mut c = std::process::Command::new("sh"); c.args(["-c", "sleep 20 & echo hello"]);
        let start = std::time::Instant::now(); let out = super::output_within(c, std::time::Duration::from_secs(10)).expect("it ended");
        assert!(start.elapsed() < std::time::Duration::from_secs(6), "{:?}", start.elapsed());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
    }
    #[test]
    fn what_was_searched_is_said_in_words_with_the_home_folder_short() {
        let home = std::path::PathBuf::from("/home/u");
        let known: Vec<std::path::PathBuf> = ["/usr/local/bin", "/home/u/.local/bin", "/home/u/.nvm/versions/node/v22/bin"].iter().map(std::path::PathBuf::from).collect();
        let r = format_search_report(4, &known, Some(&home), false);
        assert!(r.contains("PATH this app started with (4 folders)") && r.contains("login shell"), "{r}");
        assert!(r.contains("3 known install folders") && r.contains("~/.local/bin") && r.contains("/usr/local/bin") && !r.contains("/home/u/"), "{r}");
        let many: Vec<std::path::PathBuf> = (0..9).map(|i| std::path::PathBuf::from(format!("/x/{i}"))).collect();
        assert!(format_search_report(1, &many, None, false).contains("and 3 more"));
    }

    #[test]
    fn the_login_shells_path_is_read_between_markers_so_a_banner_cannot_get_in() {
        assert_eq!(parse_marked_path("Welcome!\n__ANTILOKI_PATH__/a/bin:/b/bin__ANTILOKI_PATH__\nbye"), Some("/a/bin:/b/bin".into()));
        assert_eq!(parse_marked_path("no markers at all"), None);
        assert_eq!(parse_marked_path("__ANTILOKI_PATH____ANTILOKI_PATH__"), None, "an empty PATH is nothing");
    }

    #[cfg(unix)]
    #[test]
    fn a_cli_in_a_known_install_folder_is_found_though_the_launcher_gave_a_bare_path() {
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!("antiloki-home-{}", std::process::id())); let _ = std::fs::remove_dir_all(&home);
        for d in [".local/bin", ".nvm/versions/node/v20.1.0/bin", ".nvm/versions/node/v22.3.0/bin"] { std::fs::create_dir_all(home.join(d)).unwrap(); }
        for f in [".local/bin/zzclaude", ".nvm/versions/node/v20.1.0/bin/zzcodex"] { let p = home.join(f); std::fs::write(&p, "#!/bin/sh\n").unwrap(); std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap(); }
        let was = std::env::var_os("HOME"); unsafe { std::env::set_var("HOME", &home); }
        let dirs = known_install_dirs_unix();
        if let Some(w) = was { unsafe { std::env::set_var("HOME", w); } }
        let joined = std::env::join_paths(&dirs).unwrap();
        assert!(which_in("zzclaude", &joined).is_some(), "the native installer's folder");
        assert!(which_in("zzcodex", &joined).is_some(), "an older nvm Node version still counts");
        assert!(dirs.iter().position(|d| d.ends_with("v22.3.0/bin")) < dirs.iter().position(|d| d.ends_with("v20.1.0/bin")), "the newest version first");
        assert!(dirs.iter().all(|d| d.is_dir()), "only folders that exist");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_bare_path_gets_the_tool_homes_that_exist_added_after_what_it_had_and_nothing_twice() {
        use std::path::PathBuf;
        let extras = [PathBuf::from("/home/u/.local/share/mise/shims"), PathBuf::from("/usr/bin"), PathBuf::from("/home/u/.local/bin")];
        assert_eq!(extend_path("/usr/bin:/bin", &extras), "/usr/bin:/bin:/home/u/.local/share/mise/shims:/home/u/.local/bin", "what was there stays first; what was already there is not added again");
        assert_eq!(extend_path("", &extras), "/home/u/.local/share/mise/shims:/usr/bin:/home/u/.local/bin"); assert_eq!(extend_path("/a", &[]), "/a"); assert_eq!(extend_path("/a::/b", &[PathBuf::from("/a")]), "/a:/b");
        assert!(tool_path_extras().iter().all(|p| p.is_dir()), "only directories that exist");
    }

    use super::*;

    #[test]
    fn the_shell_is_the_platforms_own() {
        let (sh, flag) = shell_parts();
        if cfg!(windows) { assert_eq!((sh, flag), ("cmd", "/C")) } else { assert_eq!((sh, flag), ("sh", "-c")) }
        // the constructed command really names it — a `sh -c` hardcoded anywhere else is the bug this replaces
        // CONTAINS, not equals: on Windows `cmd()` resolves the name to a full path (C:\Windows\System32\cmd.exe),
        // which is the whole point of `resolve` — asserting the bare spelling here would fail on the one platform
        // this branch exists for, and that failure would be the test's, not the code's.
        let got = shell_sync("npm test").get_program().to_string_lossy().to_lowercase();
        assert!(got.contains(sh), "{got} is not {sh}");
    }

    #[test]
    fn a_reg_query_answer_is_read_expanded_and_merged() {
        let out = "\r\nHKEY_CURRENT_USER\\Environment\r\n    TEMP    REG_EXPAND_SZ    %USERPROFILE%\\AppData\\Local\\Temp\r\n    Path    REG_EXPAND_SZ    %USERPROFILE%\\.local\\bin;C:\\Tools;D:\\with space\\bin\r\n\r\n";
        let v = parse_reg_path(out).unwrap();
        assert_eq!(v, "%USERPROFILE%\\.local\\bin;C:\\Tools;D:\\with space\\bin");
        assert_eq!(parse_reg_path("    Path    REG_SZ    C:\\a\r\n").as_deref(), Some("C:\\a"));
        assert_eq!(parse_reg_path("    PATHEXT    REG_SZ    .EXE\r\n"), None, "PATHEXT is not Path");
        assert_eq!(parse_reg_path("ERROR: The system was unable to find the specified registry key or value.\r\n"), None);
        let x = expand_percent_vars(&v, &|n| if n == "USERPROFILE" { Some("C:\\Users\\d".into()) } else { None });
        assert_eq!(x, "C:\\Users\\d\\.local\\bin;C:\\Tools;D:\\with space\\bin");
        assert_eq!(expand_percent_vars("%NOPE%\\x;100%", &|_| None), "%NOPE%\\x;100%", "an unknown variable and a stray percent stay as written");
        let m = merge_paths("C:\\Tools;C:\\Win", &x, ';').unwrap();
        assert_eq!(m, "C:\\Tools;C:\\Win;C:\\Users\\d\\.local\\bin;D:\\with space\\bin", "only what was missing is appended, in order");
        assert_eq!(merge_paths("c:\\tools\\;C:\\Win", "C:\\TOOLS;C:\\win\\", ';'), None, "case and trailing separators do not make a new entry");
    }
    #[test]
    fn cursors_cli_is_found_under_either_name_and_the_old_name_wins() {
        let dir = std::env::temp_dir().join(format!("antiloki-which-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mk = |n: &str| { let p = dir.join(if cfg!(windows) { format!("{n}.cmd") } else { n.to_string() }); std::fs::write(&p, "x").unwrap(); #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap(); } p };
        let agent = mk("agent");
        let path = std::env::join_paths([dir.clone()]).unwrap();
        assert_eq!(which_aliased("cursor-agent", &path), Some(agent.clone()), "a machine with only `agent` has the Cursor CLI");
        assert_eq!(which_aliased("agent", &path), Some(agent.clone()));
        let old = mk("cursor-agent");
        assert_eq!(which_aliased("cursor-agent", &path), Some(old), "the old name, when it exists, is the one used");
        assert_eq!(which_aliased("claude", &path), None, "no alias for anything else");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_binary_on_the_path_is_found_without_spawning_anything() {
        // the interpreter running this test is on the PATH by definition of how it was invoked
        let name = if cfg!(windows) { "cmd" } else { "sh" };
        let found = which(name).unwrap_or_else(|| panic!("{name} is not on the PATH?"));
        assert!(found.is_file());
        assert!(which("a-binary-nobody-has-installed-9f2c").is_none());
        // a directory on the PATH is not a program
        assert!(!is_exe(std::path::Path::new("/")));
    }

    #[test]
    fn a_bare_program_name_becomes_something_the_os_can_actually_start() {
        // On Unix nothing changes: the name is the name, and the PATH search is the OS's job.
        // On Windows the bare name is replaced by the full resolved path, which is the whole point —
        // `claude` is a .cmd shim there and Command would never have found it.
        let sh = if cfg!(windows) { "cmd" } else { "sh" };
        let got = resolve(std::ffi::OsStr::new(sh));
        if cfg!(windows) { assert!(std::path::Path::new(&got).is_absolute(), "{got:?} was not resolved") }
        else { assert_eq!(got, std::ffi::OsString::from(sh)) }

        // a name the machine does not have is passed through, so the OS reports its own error
        assert_eq!(resolve(std::ffi::OsStr::new("no-such-program-9f2c")), std::ffi::OsString::from("no-such-program-9f2c"));
        // an explicit path is never second-guessed
        assert_eq!(resolve(std::ffi::OsStr::new("/usr/bin/env")), std::ffi::OsString::from("/usr/bin/env"));
    }

    #[test]
    fn a_path_is_quoted_the_way_this_platforms_shell_reads_it() {
        let q = quote("src/a b.ts").unwrap();
        if cfg!(windows) { assert_eq!(q, "\"src/a b.ts\"") } else { assert_eq!(q, "'src/a b.ts'") }
        assert_eq!(quote(""), None);
    }

    #[test]
    fn a_path_the_shell_cannot_carry_is_dropped_not_mangled() {
        // the quote character itself: escaping it differs per shell and per runner, so it is refused. A dropped
        // file is one file untested and reported as such; a mangled one is a command that fails as a whole.
        let bad = if cfg!(windows) { "src/say\"hi.ts" } else { "src/it's.ts" };
        assert_eq!(quote(bad), None);
        assert_eq!(quote_all(&["src/a.ts".into(), bad.into(), "src/b.ts".into()]).len(), 2);
    }
}

/// THE DESKTOP SESSION'S ADDRESSES, WHEN THE PROCESS WAS STARTED WITHOUT THEM (operator 2026-11-23: "it keeps opening cursor login"). Cursor, Antigravity and others keep their sign-in in the OS keyring (Secret Service,
/// reached over the user's D-Bus). A serve started from a launcher or a service has no `DBUS_SESSION_BUS_ADDRESS` / `XDG_RUNTIME_DIR`, so every CLI it starts cannot read the stored login and opens its login page.
/// systemd and every common desktop put the bus at `/run/user/<uid>/bus`: when that socket exists and the variable is missing, give it. Pure (the lookups are passed in) - tested.
pub fn session_env_gaps(uid: u32, get: &dyn Fn(&str) -> Option<String>, exists: &dyn Fn(&str) -> bool) -> Vec<(&'static str, String)> {
    let dir = format!("/run/user/{uid}");
    let mut out = vec![];
    if get("XDG_RUNTIME_DIR").map_or(true, |v| v.is_empty()) && exists(&dir) { out.push(("XDG_RUNTIME_DIR", dir.clone())); }
    let bus = format!("{dir}/bus");
    if get("DBUS_SESSION_BUS_ADDRESS").map_or(true, |v| v.is_empty()) && exists(&bus) { out.push(("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={bus}"))); }
    out
}

/// Apply `session_env_gaps` to this process (Linux only; once, before anything starts).
pub fn import_session_env() {
    #[cfg(target_os = "linux")]
    {
        let uid = unsafe { libc_getuid() };
        for (k, v) in session_env_gaps(uid, &|k| std::env::var(k).ok(), &|p| std::path::Path::new(p).exists()) { std::env::set_var(k, v); }
    }
}
#[cfg(target_os = "linux")]
extern "C" { #[link_name = "getuid"] fn libc_getuid() -> u32; }

#[cfg(test)]
mod session_env_tests {
    use super::session_env_gaps;
    #[test]
    fn a_missing_bus_is_filled_in_only_when_the_socket_is_there() {
        let none = |_: &str| None;
        let all = |_: &str| true;
        let g = session_env_gaps(1000, &none, &all);
        assert_eq!(g, vec![("XDG_RUNTIME_DIR", "/run/user/1000".to_string()), ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus".to_string())]);
        assert!(session_env_gaps(1000, &none, &|_| false).is_empty(), "no socket, nothing invented");
        let have = |k: &str| Some(format!("x-{k}"));
        assert!(session_env_gaps(1000, &have, &all).is_empty(), "what the process already has is left alone");
    }
}

#[cfg(all(test, unix))]
mod within_tests {
    #[test]
    fn the_users_self_calling_claude_wrapper_is_recognised() {
        let w = "#!/bin/bash\nexport MISE_MINIMUM_RELEASE_AGE=0\nmise use -g \"claude\" || exit 1\nexec mise x \"claude\" -- \"claude\" \"$@\"\n";
        assert!(super::is_self_calling_wrapper("claude", w));
        assert!(!super::is_self_calling_wrapper("claude", "#!/bin/sh\nexec node cli.js \"$@\"\n"));
        assert!(!super::is_self_calling_wrapper("claude", "\x7fELF mise x claude"));
    }
    #[test]
    fn a_probe_that_never_ends_is_killed_and_a_quick_one_answers() {
        let mut slow = std::process::Command::new("sh"); slow.args(["-c", "sleep 30"]);
        let t = std::time::Instant::now(); assert!(super::output_within(slow, std::time::Duration::from_millis(300)).is_none()); assert!(t.elapsed().as_secs() < 5);
        let mut q = std::process::Command::new("sh"); q.args(["-c", "echo hi"]);
        assert_eq!(String::from_utf8_lossy(&super::output_within(q, std::time::Duration::from_secs(5)).unwrap().stdout).trim(), "hi");
    }
}
