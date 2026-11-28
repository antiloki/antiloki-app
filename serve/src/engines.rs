//! WHAT THIS MACHINE CAN ACTUALLY RUN.
//!
//! Operator 2026-09-24, from a Windows install: *"at the beggining it asks me to choose between the AIs planner
//! and worker wich is good. But it should also check if i have those AIs available … the system of creating
//! agents, should adapt to the faster, cheaper and best options AVAILABLE in the user machine and suggest the
//! user to add more options to unlock merges."*
//!
//! Until now nothing asked. The agent wizard offered every merge in its catalogue whether or not the engine was
//! installed, so a machine with only Claude was invited to pick `Kimi × DeepSeek` and got an agent that could
//! not start. And on Windows the question could not even be answered: engine detection shelled out to `which`,
//! which is not a program there, so every engine reported missing on a machine that had all three.
//!
//! This is the machine half of the answer and nothing else — which engines are on the PATH, and which models
//! each one offers here. Which merges those add up to is the catalogue's business, and the catalogue lives with
//! the wizard that shows it; duplicating it here would give the two halves a chance to disagree.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EngineInfo {
    pub key: String,
    pub label: String,
    /// the command that was looked for
    pub bin: String,
    pub installed: bool,
    /// where it was found, so "not installed" can be argued with
    pub path: Option<String>,
    /// the models this engine offers ON THIS MACHINE — for OpenCode that is its own listing, which is the only
    /// place the DeepSeek/Kimi/Qwen/GLM split is real; for the two subscription CLIs it is the tier ladder
    pub models: Vec<String>,
    /// how to get it, said once, for the ones that are missing
    pub install: String,
    /// IS THE SEAT REAL — `Some(true)` signed in, `Some(false)` installed but not, `None` when it cannot be
    /// known from disk (Antigravity keeps its credentials in the OS keyring). The operator's standing rule:
    /// "offer a provider only when the SEAT is real, never the binary" — an installed-but-signed-out CLI is
    /// not a subscription, and offering it promises an engine that answers every ask with an auth error.
    pub seat: Option<bool>,
    /// the command that signs in — ALWAYS the user's own act, run in their own terminal. antiloki never sees a
    /// credential, which is why this is a string to show and never a thing to execute.
    pub signin: String,
    /// what is missing before this seat can be used: "" when nothing is.
    pub missing: String,
    /// THE OPERATOR SWITCHED THIS SEAT OFF (Settings › Seats). Installed and signed in, and still not a seat: the advice and the merge catalogue must not count it
    /// (2026-10-01, the Claude-off audit: a switched-off Claude still made "Everything a merge needs is here" and still marked every Claude pair `ready`).
    #[serde(default)]
    pub off: bool,
    /// FOUND ON DISK BUT WILL NOT RUN (a shim whose Node is gone, a file that is not executable): the reason, else None. `installed` stays true (the file is there); the screen says it is found but broken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broken: Option<String>,
}

/// THE FIVE SEATS AND THE GATEWAY (operator 2026-09-27: the pivot). Every one of them is a chat pane since
/// 2026-09-28 — `agent.rs` · `agent_codex.rs` · `agent_opencode.rs` · `agent_gemini.rs` · `agent_print.rs`.
/// The install line is the one a person actually runs; the sign-in half is `onboarding_installs_the_seat`.
pub const ENGINES: [(&str, &str, &str, &str); 7] = [
    ("claude-code", "Claude Code", "claude", "npm i -g @anthropic-ai/claude-code, then `claude` once to sign in"),
    ("codex", "Codex", "codex", "npm i -g @openai/codex, then `codex` once to sign in"),
    ("gemini", "Antigravity", "agy", "curl -fsSL https://antigravity.google/cli/install.sh | bash, then run `agy` once — it opens a browser"),
    // GROK IS REACHED THROUGH CURSOR (operator 2026-09-28: "Grok we use cursor"). Its own xAI CLI is a SECOND
    // subscription for a model the Cursor seat already serves — and serves from Cursor's OWN pool, which carries
    // no token rate at all (`cursor-grok-4.6-*`, `is_cursor_own_model`). So the headline pair "Claude Grok" is
    // built as Claude × cursor-grok, not as Claude × the Grok CLI. The CLI stays supported for anyone who holds
    // an xAI seat and prefers it; it is simply not something onboarding asks anyone to buy.
    ("grok", "Grok (xAI CLI — optional; Cursor already serves Grok)", "grok",
     "optional — Cursor's own pool already serves Grok at no token rate. Only if you hold an xAI seat: npm i -g @xai-official/grok, then `grok login --device-code`"),
    ("cursor", "Cursor", "cursor-agent", "curl https://cursor.com/install -fsS | bash, then `cursor-agent login`"),
    ("opencode", "OpenCode", "opencode", "npm i -g opencode-ai, then `opencode auth login` — this is the one that carries DeepSeek, Kimi, Qwen and GLM"),
    // THE DEEPSEEK HARNESS (operator 2026-11-10: "i installed deepseek harness, can we have support to that on agents?"): the `dsh` CLI of `@deepseek-ai/dsh`; this release ships a web UI (`dsh web`) and a one-shot `headless`, no terminal UI. A pane provider:
    // it is not one of the router's seats, so it has no seat probe and no tiers (`cli::Engine` does not list it); a pane runs it and the person talks to it.
    ("deepseek", "DeepSeek Harness", "dsh", "npm i -g @deepseek-ai/dsh, then `dsh web` once to sign in (this release has a web UI, no terminal one)"),
];

/// The DeepSeek harness's `dsh`, where it is: on the PATH, or (it is usually run through `npx`) in npx's cache, `~/.npm/_npx/<hash>/node_modules/.bin/dsh`: a script that runs as it is. `None` when it is on neither.
pub fn deepseek_bin() -> Option<String> {
    if let Some(p) = crate::proc::which("dsh") { return Some(p.to_string_lossy().to_string()); }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(std::path::PathBuf::from)?;
    deepseek_bin_in(&home.join(".npm").join("_npx"))
}
/// The newest `dsh` among the npx cache's folders. Pure over a directory.
pub fn deepseek_bin_in(npx_cache: &std::path::Path) -> Option<String> {
    let mut found: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(npx_cache).into_iter().flatten().flatten()
        .map(|e| e.path().join("node_modules").join(".bin").join("dsh")).filter(|p| p.exists())
        .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p)).collect();
    found.sort(); found.pop().map(|(_, p)| p.to_string_lossy().to_string())
}

/// OpenCode's own model listing. Cached for the process: it shells out, and the answer does not change while
/// the app is open. Lines that are not model ids are dropped — a tool manager's shim prints a banner on stdout
/// ahead of the command's own output, and a banner pasted into a model list becomes a model nobody can run.
fn opencode_models() -> Vec<String> {
    static CACHE: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        let mut c = crate::proc::cmd("opencode"); c.arg("models");
        let Some(out) = antiloki_rs::proc::output_within(c, std::time::Duration::from_secs(15)) else { return vec![] };
        String::from_utf8_lossy(&out.stdout).lines()
            .map(str::trim)
            .filter(|l| l.contains('/') && !l.contains(char::is_whitespace) && l.len() < 80)
            .map(String::from).collect()
    }).clone()
}

/// The tier ladder a subscription CLI runs, as models. Pure over the router's own table.
fn ladder_of(key: &str) -> Vec<String> {
    crate::cli::engine_of(&json!(key)).map(crate::cli::ladder).unwrap_or_default()
}

/// WHAT IS MISSING BEFORE THIS SEAT CAN BE USED — "" when nothing is. Pure, so the sentence the screen shows is
/// testable without a machine to probe.
pub fn missing_for(installed: bool, seat: Option<bool>, bin: &str, install: &str, signin: &str) -> String {
    match (installed, seat) {
        (false, _) => format!("`{bin}` is not on this machine — {install}"),
        (true, Some(false)) => format!("`{bin}` is installed but not signed in — run `{signin}`"),
        // the keyring case: nothing on disk proves it either way, and a wrong guess costs one fast auth error
        (true, None) => String::new(),
        (true, Some(true)) => String::new(),
    }
}

/// DOES A FOUND CLI RUN. `<path> --version` with no keyboard and a short time limit, remembered for two minutes per path. Only a start that FAILS counts as broken: could not be executed, or the shell's own "not found" /
/// "cannot execute" (126, 127: an `npm` shim whose `node` is gone says exactly that). A slow answer is not a broken one. Impure.
fn broken_reason(path: &std::path::Path) -> Option<String> {
    static SEEN: std::sync::Mutex<Option<std::collections::HashMap<String, (Option<String>, std::time::Instant)>>> = std::sync::Mutex::new(None);
    let key = path.to_string_lossy().to_string();
    if let Some((r, at)) = SEEN.lock().ok().and_then(|g| g.as_ref().and_then(|m| m.get(&key).cloned())) { if at.elapsed() < std::time::Duration::from_secs(120) { return r; } }
    let result = (|| {
        let mut child = match std::process::Command::new(path).arg("--version").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn() {
            Ok(c) => c, Err(e) => return Some(format!("could not be started ({e})")),
        };
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(st)) => {
                    let code = st.code();
                    if matches!(code, Some(126) | Some(127)) {
                        let mut err = String::new(); if let Some(mut e) = child.stderr.take() { use std::io::Read; let _ = e.read_to_string(&mut err); }
                        let line = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
                        return Some(if line.is_empty() { format!("exited with code {}", code.unwrap_or(0)) } else { line });
                    }
                    return None;
                }
                Ok(None) if start.elapsed() < std::time::Duration::from_secs(8) => std::thread::sleep(std::time::Duration::from_millis(40)),
                _ => { let _ = child.kill(); let _ = child.wait(); return None; }
            }
        }
    })();
    if let Ok(mut g) = SEEN.lock() { g.get_or_insert_with(Default::default).insert(key, (result.clone(), std::time::Instant::now())); }
    result
}

/// Probe the machine. The only impure function here.
pub fn probe() -> Vec<EngineInfo> { probe_with(true) }

/// `deep` also starts every found CLI (`--version`, OpenCode's model list); without it nothing is started, so the answer is quick.
fn probe_with(deep: bool) -> Vec<EngineInfo> {
    // look up every seat first, then try the found ones at the same time, so seven slow starts cost one
    let found: Vec<Option<std::path::PathBuf>> = ENGINES.iter().map(|(key, _, bin, _)| crate::proc::which(bin).or_else(|| if *key == "deepseek" { deepseek_bin().map(std::path::PathBuf::from) } else { None })).collect();
    let broken: Vec<Option<String>> = std::thread::scope(|sc| {
        let hs: Vec<_> = found.iter().map(|p| sc.spawn(move || p.as_ref().filter(|_| deep).and_then(|p| broken_reason(p)))).collect();
        hs.into_iter().map(|h| h.join().unwrap_or(None)).collect()
    });
    ENGINES.iter().enumerate().map(|(idx, (key, label, bin, _))| {
        let path = found[idx].clone();
        let broken = broken[idx].clone();
        let installed = path.is_some();
        let engine = crate::cli::engine_of(&json!(key));
        // THE SEAT IS READ, NEVER SPENT (operator 2026-09-26: gemini, grok and cursor were all installed here
        // with no seat). `engine_seat` reads the CLI's own credential file — no token is spent to find out.
        let seat = if installed { engine.and_then(crate::cli::engine_seat) } else { Some(false) };
        let found: Option<String> = path.as_ref().map(|p| p.to_string_lossy().to_string());
        let signin = signin_for_found(key, found.as_deref());
        let install_line = install_for(key, std::env::consts::OS);
        EngineInfo {
            key: (*key).into(), label: (*label).into(), bin: (*bin).into(), installed,
            path: found,
            models: if !installed { vec![] } else if *key == "opencode" { if deep { opencode_models() } else { vec![] } } else { ladder_of(key) },
            missing: match &broken {
                Some(why) => format!("`{bin}` was found at {} but did not run: {why} — reinstall it, or run `{bin} --version` in a terminal to see why", path.as_ref().map(|p| p.display().to_string()).unwrap_or_default()),
                None if !installed => format!("{} ({})", missing_for(installed, seat, bin, &install_line, &signin), crate::proc::search_report()),
                None => missing_for(installed, seat, bin, &install_line, &signin),
            },
            install: install_line,
            seat, signin,
            off: engine.map_or(false, crate::cli::seat_off),
            broken,
        }
    }).collect()
}

/// THE INSTALL LINE FOR THIS OPERATING SYSTEM (2026-10-11). The table above was written on Linux: `curl ... | bash` is not a Windows command, and a Windows user who is
/// told to run it has been told their subscription cannot be used. Each line is the vendor's own documented installer for the platform (Claude Code, Cursor and
/// Antigravity ship PowerShell installers; Codex and the xAI CLI are npm packages, which need Node.js). `os` is `std::env::consts::OS`. Pure - tested.
pub fn install_for(key: &str, os: &str) -> String {
    let cmd = install_cmd_for(key, os);
    if cmd.is_empty() { return String::new() }
    if key == "grok" { return format!("optional — Cursor's own pool already serves Grok at no token rate. Only if you hold an xAI seat: {cmd}, then `grok login --device-code`") }
    let shell = if shell_name(os) == "PowerShell" && cmd.starts_with("irm ") { "In PowerShell: " } else { "" };
    format!("{shell}{cmd}   — {}", after_install_note(key))
}

/// Just the command to paste, nothing around it (the screen's copy button copies this). Pure - tested.
pub fn install_cmd_for(key: &str, os: &str) -> String {
    let win = os == "windows";
    match key {
        "claude-code" => if win { "irm https://claude.ai/install.ps1 | iex".into() } else { "curl -fsSL https://claude.ai/install.sh | bash".into() },
        "codex" => "npm i -g @openai/codex".into(),
        "gemini" => if win { "irm https://antigravity.google/cli/install.ps1 | iex".into() } else { "curl -fsSL https://antigravity.google/cli/install.sh | bash".into() },
        "grok" => "npm i -g @xai-official/grok".into(),
        "cursor" => if win { "irm 'https://cursor.com/install?win32=true' | iex".into() } else { "curl https://cursor.com/install -fsS | bash".into() },
        "opencode" => "npm i -g opencode-ai".into(),
        "deepseek" => "npm i -g @deepseek-ai/dsh".into(),
        _ => String::new(),
    }
}

/// Where the install command is typed: the PowerShell installers need PowerShell (not cmd.exe), and npm works in either. Pure.
pub fn shell_name(os: &str) -> &'static str { if os == "windows" { "PowerShell" } else { "a terminal" } }

/// What to do right after the install command, in words. Pure.
fn after_install_note(key: &str) -> &'static str {
    match key {
        "claude-code" => "then run `claude` once to sign in",
        "codex" => "then run `codex` once and choose Sign in with ChatGPT",
        "gemini" => "then run `agy` once: it opens a browser, choose Google OAuth and paste the code back",
        "cursor" => "then run `agent login`",
        "opencode" => "then `opencode auth login` — this is the one that carries DeepSeek, Kimi, Qwen and GLM",
        "deepseek" => "then `dsh web` once to sign in (this release has a web UI, no terminal one)",
        _ => "",
    }
}

/// WHAT MUST ALREADY BE ON THE MACHINE before the install line works ("" when nothing). Pure - tested.
pub fn needs_for(key: &str, _os: &str) -> &'static str {
    match key {
        "codex" | "grok" | "opencode" | "deepseek" => "needs Node.js 22 or newer (https://nodejs.org) — it brings npm",
        "gemini" => "needs a Google account; the free tier is enough, and Chrome helps for the sign-in",
        _ => "",
    }
}

/// The sign-in command for the binary that was FOUND: Cursor's CLI is `agent` in Cursor's docs and `cursor-agent` on older installs; the sign-in line names the one on this machine.
pub fn signin_for_found(key: &str, found: Option<&str>) -> String {
    if key == "cursor" {
        let bare = found.map(|f| f.rsplit(['/', '\\']).next().unwrap_or(f)).map(|f| f.trim_end_matches(".cmd").trim_end_matches(".exe").trim_end_matches(".bat")).unwrap_or("agent");
        return format!("{} login", if bare == "cursor-agent" { "cursor-agent" } else { "agent" });
    }
    signin_for(key).to_string()
}

/// The sign-in command for a seat — the user's own act, in their own terminal. Pure.
pub fn signin_for(key: &str) -> &'static str {
    match key {
        "claude-code" => "claude",
        "codex" => "codex",
        // Antigravity has no login subcommand: running it opens a browser
        "gemini" => "agy",
        "grok" => "grok login",
        "cursor" => "cursor-agent login",
        "opencode" => "opencode auth login",
        "deepseek" => "dsh web",
        _ => "",
    }
}

/// What to tell the person who picked an agent whose program is no longer on this machine; None when it is there (or is not one of the agents looked for here). Only looks at files: nothing is started. Blocking.
pub fn gone(key: &str) -> Option<String> { gone_when(key, |bin| crate::proc::which(bin).is_some()) }
fn gone_when(key: &str, there: impl Fn(&str) -> bool) -> Option<String> {
    let (_, label, bin, _) = ENGINES.iter().find(|e| e.0 == key && e.0 != "deepseek")?;
    if there(bin) { return None; }
    Some(format!("{label} is not installed any more: `{bin}` was not found on this machine. Install it ({}), then pick it again.", install_for(key, std::env::consts::OS)))
}

/// WHAT THIS MACHINE IS MISSING, AND WHY IT WOULD WANT IT.
///
/// The operator asked for the argument to be made explicitly and with examples, not implied by a greyed-out
/// button: *"suggest expansions with goos arguments, defending the merging as positive add for save lots of
/// tokens and save money. That should be explicitly with examples."*
///
/// Every number below is a measured bench row from this repo, named with its date — not a projection. Pure.
pub fn advice(engines: &[EngineInfo]) -> Value {
    // THE SAME SEATS AND THE SAME RULE AS THE ADD-AGENT WINDOW (2026-10-01, the Claude-off audit): this read
    // only claude-code, codex and opencode and wanted OpenCode for any merge, so on the machine the audit was
    // run on (Claude off, Codex + Cursor + Gemini signed in, no OpenCode) the welcome said "not a merge" while
    // `providers_from` said yes. A seat counts when it is installed, switched on and not known to be signed out -
    // exactly `engine_usable` - and two seats are a merge.
    let usable = |en: crate::cli::Engine| engines.iter().any(|e| e.key == en.key() && e.installed && !e.off && e.seat != Some(false));
    let p = crate::roles::providers_from(usable);
    let frontier = p["frontier"].as_array().map_or(0, |a| a.len());
    let cheap = p["cheap"].as_array().map_or(0, |a| a.len());
    let opencode = p["opencode"].as_bool().unwrap_or(false);
    let count = frontier + opencode as usize;
    let can_merge = p["can_merge"].as_bool().unwrap_or(false);
    let headline = if count == 0 {
        "No agent CLI is installed yet. antiloki drives the ones you already pay for — it bundles no model of its own."
    } else if !can_merge {
        "You can run agents, but not a merge. A merge is where the savings are."
    } else {
        "Everything a merge needs is here."
    };

    let mut unlock: Vec<Value> = vec![];
    // The cheap hands are what make the savings: OpenCode's catalogue, or Cursor's Composer / Gemini's Flash. A machine that already has one of those is not told to install OpenCode.
    if !opencode && cheap == 0 {
        unlock.push(json!({ "engine": "opencode", "unlocks": "every merge in the catalogue",
            "why": "OpenCode is the cheap hands. Measured 2026-09-22 on the same 8-module task: Claude × DeepSeek finished in 249 s for $0.25 with 37/37 checks passing; raw Opus doing the whole thing took 303 s and $1.28 for the same 37/37. Same result, a fifth of the money, and faster — because the planning went to the model worth paying for and the typing did not." }));
    }
    // no named seat: any of the five subscriptions is a brain, and the operator picks the one he pays for
    if frontier == 0 && opencode {
        unlock.push(json!({ "engine": "a frontier seat", "unlocks": "a frontier brain over cheap hands",
            "why": "Measured 2026-09-23, all-OpenCode: Kimi × DeepSeek-Flash did the same 8 modules in 163 s for $0.04 — the cheapest row there is, and 37/37. It is a real option. What it does not buy is the judgement: the all-OpenCode judge missed a bug that Opus caught, which is the reason the brain side is worth a subscription." }));
    }
    if frontier == 1 {
        unlock.push(json!({ "engine": "a second frontier seat", "unlocks": "a second brain to alternate with",
            "why": "Two frontier subscriptions means a weekly limit on one does not stop the work — the router moves the planning to the other." }));
    }
    json!({ "headline": headline, "can_merge": can_merge, "installed": count, "unlock": unlock })
}

/// THE MERGES THIS MACHINE CAN ACTUALLY HIRE.
///
/// `merges::pairs_from` says what the record measured; this says which of those can start here, and why the
/// rest cannot. A pair is offered only when BOTH its engines are installed — the missing side is named, not
/// implied by a greyed-out button. Pure over the two inputs, so it is tested without a machine or a database.
pub fn offerable(pairs: &[Value], engines: &[EngineInfo]) -> Vec<Value> {
    let side_engine = |k: &str| k.split(':').next().unwrap_or("").to_string();
    let installed = |k: &str| engines.iter().any(|e| e.key == k && e.installed && !e.off);
    let label_of = |k: &str| -> String {
        // THE LABEL MUST NAME ONE THING. A first version cut the model at its first dash, so
        // `deepseek-v4-flash` and `deepseek-v4-pro` both read "deepseek" and the picker offered what looked
        // like the same merge three times at three different prices. The model slug is kept whole; only the
        // gateway's `opencode/` path prefix is dropped, because it is the engine, which the row already names.
        let engine = k.split(':').next().unwrap_or(k);
        let model = k.split_once(':').map(|(_, m)| m).unwrap_or("").rsplit('/').next().unwrap_or("");
        // a bare reasoning effort ("low", "medium", "high") is Codex's tier, not a model — it identifies
        // nothing on its own, so it keeps its engine
        let effort = matches!(model, "low" | "medium" | "high");
        if model.is_empty() || model == "default" { engine.to_string() }
        else if effort { format!("{engine} {model}") }
        else { model.to_string() }
    };
    pairs.iter().map(|p| {
        let (pk, wk) = (p.get("planner").and_then(Value::as_str).unwrap_or(""), p.get("worker").and_then(Value::as_str).unwrap_or(""));
        let (pe, we) = (side_engine(pk), side_engine(wk));
        let missing: Vec<String> = [&pe, &we].iter().filter(|e| !installed(e)).map(|e| e.to_string())
            .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        let mut out = p.clone();
        out["engines"] = json!([pe, we]);
        out["label"] = json!(format!("{} × {}", label_of(pk), label_of(wk)));
        out["ready"] = json!(missing.is_empty());
        out["missing"] = json!(missing);
        out["install"] = json!(p.get("missing").and_then(Value::as_array).map(|_| "").unwrap_or(""));
        out
    }).collect()
}

fn payload() -> Result<serde_json::Value, String> {
    let e = probe();
    let a = advice(&e);
    // the merge catalogue is a VIEW over the record now, not a list in the UI — a pair appears the moment
    // it has rows (operator 2026-09-25: "you should be able to merge any model to any model")
    let merges = crate::merges::pairs().map(|p| offerable(&p, &e)).unwrap_or_default();
    Ok(json!({ "ok": true, "engines": e, "advice": a, "merges": merges }))
}
/// Make the answer once at start, in the background, so the first page load does not pay for the probe.
pub fn warm() { std::thread::spawn(|| { let _ = crate::swr::get("engines", std::time::Duration::from_secs(30), payload); }); }

/// HOW LONG THE SCREEN WAITS FOR THE FULL ANSWER (2026-11-27: on a friend's Windows PC the agent list never appeared). Past this, the agents found on the machine are listed at once, without starting any of them, and the full
/// answer keeps being made behind it.
const FULL_ANSWER_WITHIN: std::time::Duration = std::time::Duration::from_secs(8);

/// The full answer if it comes in time, else the quick one. Only one slow full probe runs at a time: while it does, later asks get the quick answer at once. Blocking.
fn answer_within(limit: std::time::Duration, full: impl FnOnce() -> Result<Value, String> + Send + 'static, quick: impl FnOnce() -> Value, making: &'static std::sync::atomic::AtomicBool) -> Value {
    use std::sync::atomic::Ordering;
    if making.swap(true, Ordering::SeqCst) { return quick(); }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(full)).unwrap_or_else(|_| Err("the engine probe panicked".into())); making.store(false, Ordering::SeqCst); let _ = tx.send(r); });
    match rx.recv_timeout(limit) { Ok(Ok(v)) => v, Ok(Err(e)) => { eprintln!("antiloki-serve: the engine probe failed: {e}"); quick() } Err(_) => quick() }
}
fn quick_payload() -> Value { let e = probe_with(false); let a = advice(&e); json!({ "ok": true, "engines": e, "advice": a, "merges": [], "partial": true }) }

pub async fn http() -> axum::response::Response {
    use axum::response::IntoResponse;
    // the probe touches the filesystem and, for OpenCode, shells out once — off the async thread
    // the probe takes seconds (it shells out and reads the whole merge record): the last answer is served at once and refreshed in the background (swr.rs)
    static MAKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let body = tokio::task::spawn_blocking(|| match crate::swr::peek("engines") {
        Some(_) => crate::swr::get("engines", std::time::Duration::from_secs(30), payload).unwrap_or_else(|_| quick_payload()),
        None => answer_within(FULL_ANSWER_WITHIN, || crate::swr::get("engines", std::time::Duration::from_secs(30), payload), quick_payload, &MAKING),
    }).await.unwrap_or_else(|_| json!({ "ok": false, "error": "the engine probe panicked" }));
    // the ready added providers (RFC 0023) join the list as it is served, so a Confirm shows at once
    let mut body = body;
    if let Some(list) = body.get_mut("engines").and_then(|v| v.as_array_mut()) { list.extend(tokio::task::spawn_blocking(crate::providers::engine_rows).await.unwrap_or_default()); }
    (axum::http::StatusCode::OK, axum::Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_agent_that_was_uninstalled_is_said_when_it_is_picked() {
        let why = super::gone_when("codex", |_| false).expect("a message"); assert!(why.contains("Codex is not installed any more") && why.contains("`codex`"), "{why}");
        assert!(super::gone_when("codex", |_| true).is_none()); assert!(super::gone_when("something-else", |_| false).is_none());
    }
    #[test]
    fn a_probe_that_takes_too_long_is_answered_with_the_quick_list_and_is_not_started_twice() {
        use std::time::Duration; static MAKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let quick = || serde_json::json!("quick");
        let start = std::time::Instant::now();
        assert_eq!(super::answer_within(Duration::from_millis(80), || { std::thread::sleep(Duration::from_millis(900)); Ok(serde_json::json!("full")) }, quick, &MAKING), "quick");
        assert_eq!(super::answer_within(Duration::from_secs(5), || panic!("a second probe must not start"), quick, &MAKING), "quick");
        assert!(start.elapsed() < Duration::from_millis(700), "{:?}", start.elapsed());
        std::thread::sleep(Duration::from_millis(1100));
        assert_eq!(super::answer_within(Duration::from_secs(5), || Ok(serde_json::json!("full")), quick, &MAKING), "full");
    }
    #[cfg(unix)]
    #[test]
    fn a_found_cli_that_cannot_run_is_called_broken_with_its_own_reason_and_a_working_one_is_not() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("antiloki-broken-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let script = |name: &str, body: &str| { let p = d.join(name); std::fs::write(&p, body).unwrap(); std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap(); p };
        let ok = script("fine", "#!/bin/sh\necho 1.2.3\n");
        let nonode = script("nonode", "#!/bin/sh\necho \"env: node: No such file or directory\" >&2\nexit 127\n");
        let noexec = d.join("noexec"); std::fs::write(&noexec, "#!/bin/sh\n").unwrap();   // no execute bit
        let nonzero = script("nonzero", "#!/bin/sh\necho usage >&2\nexit 2\n");           // a non-zero exit on --version is not "broken": it ran
        assert_eq!(broken_reason(&ok), None);
        assert!(broken_reason(&nonode).unwrap().contains("node: No such file"), "the shell's own words are shown");
        assert!(broken_reason(&noexec).unwrap().contains("could not be started"));
        assert_eq!(broken_reason(&nonzero), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    use super::*;

    fn e(key: &str, installed: bool) -> EngineInfo {
        EngineInfo { key: key.into(), label: key.into(), bin: key.into(), installed, path: None, models: vec![], install: String::new(), seat: None, signin: String::new(), missing: String::new(), off: false, broken: None }
    }

    #[test]
    fn the_machine_is_probed_not_assumed() {
        let all = probe();
        assert_eq!(all.len(), ENGINES.len(), "the probe reports every provider: the six seats (since 2026-09-28) and the DeepSeek harness");
        assert_eq!(all.len(), 7);
        // every engine that reports installed must say where it was found — "not installed" has to be arguable
        for x in &all { assert_eq!(x.installed, x.path.is_some(), "{} disagrees with itself", x.key); }
        assert!(all.iter().all(|x| x.installed || x.models.is_empty()), "a missing engine offers no models");
        // …and a seat that cannot be used says what is missing, with the command that fixes it
        for x in &all {
            if !x.installed { assert!(!x.missing.is_empty(), "{} is absent and says nothing about it", x.key); }
            if x.seat == Some(false) { assert!(x.missing.contains(&x.signin), "{} does not name its sign-in", x.key); }
        }
    }

    #[test]
    fn a_merge_is_offered_only_when_both_its_engines_are_here() {
        let pairs = vec![
            json!({ "planner": "claude-code:opus", "worker": "opencode:opencode/deepseek-v4-flash", "runs": 10 }),
            json!({ "planner": "opencode:opencode/kimi-k2.7-code", "worker": "opencode:opencode/qwen3.6-plus", "runs": 2 }),
            json!({ "planner": "codex:medium", "worker": "claude-code:sonnet", "runs": 4 }),
        ];
        let have = vec![e("claude-code", true), e("codex", false), e("opencode", true)];
        let out = offerable(&pairs, &have);

        assert_eq!(out[0]["ready"], json!(true));
        assert_eq!(out[0]["label"], json!("opus × deepseek-v4-flash"), "the model slug is kept WHOLE: cutting it at the dash made -flash and -pro the same option");
        assert_eq!(out[2]["label"], json!("codex medium × sonnet"), "a bare effort is Codex's tier, not a model, so it keeps its engine");
        assert_eq!(out[0]["engines"], json!(["claude-code", "opencode"]));

        assert_eq!(out[1]["ready"], json!(true), "Chinese × Chinese was never special — it is one row like any other");

        // Claude × Codex: the pairing the ask thought was impossible. It is offerable the moment Codex is here.
        assert_eq!(out[2]["ready"], json!(false));
        assert_eq!(out[2]["missing"], json!(["codex"]), "the missing side is NAMED, not implied by a grey button");
        let all_here = vec![e("claude-code", true), e("codex", true), e("opencode", true)];
        assert_eq!(offerable(&pairs, &all_here)[2]["ready"], json!(true));
    }

    #[test]
    fn a_seat_the_operator_switched_off_is_not_offered_nor_counted() {
        let mut claude = e("claude-code", true); claude.off = true;
        let have = vec![claude, e("codex", true), e("opencode", true)];
        // the 2026-10-01 audit: Claude off, and its pairs still said `ready` - the missing side is named instead
        let pairs = vec![json!({ "planner": "claude-code:opus", "worker": "opencode:opencode/deepseek-v4-flash", "runs": 10 }),
                         json!({ "planner": "codex:medium", "worker": "opencode:opencode/deepseek-v4-flash", "runs": 4 })];
        let out = offerable(&pairs, &have);
        assert_eq!(out[0]["ready"], json!(false));
        assert_eq!(out[0]["missing"], json!(["claude-code"]));
        assert_eq!(out[1]["ready"], json!(true), "the other pair is untouched");
        // …and the advice does not count it: with Claude off and Codex + OpenCode on, nothing is left to sell
        let a = advice(&have);
        assert_eq!(a["installed"], json!(2), "{a}");
        // a Claude-only machine whose Claude is off has nothing at all
        let mut only = e("claude-code", true); only.off = true;
        assert!(advice(&[only, e("codex", false), e("opencode", false)])["headline"].as_str().unwrap().contains("bundles no model"));
    }
    #[test]
    fn the_advice_knows_the_five_seats_and_agrees_with_the_add_agent_window() {
        // THE MACHINE OF THE 2026-10-01 AUDIT: Claude switched off, Codex + Cursor + Gemini signed in, no OpenCode.
        // Before: `can_merge: false`, installed 1, "install OpenCode: every merge in the catalogue" - while
        // `providers_from` said a merge was possible, and the welcome reads this one.
        let mut claude = e("claude-code", true); claude.off = true;
        let have = vec![claude, e("codex", true), e("cursor", true), e("gemini", true), e("grok", false), e("opencode", false)];
        let a = advice(&have);
        assert_eq!(a["can_merge"], json!(true), "{a}");
        assert_eq!(a["installed"], json!(3), "{a}");
        assert!(a["headline"].as_str().unwrap().contains("Everything a merge needs"), "{a}");
        assert_eq!(a["unlock"].as_array().unwrap().len(), 0, "Composer and Flash are the cheap hands already - no OpenCode, no Claude upsell: {a}");
        // the two endpoints cannot disagree: the same machine through the Add-agent window's rule
        let usable = |en: crate::cli::Engine| have.iter().any(|x| x.key == en.key() && x.installed && !x.off);
        assert_eq!(crate::roles::providers_from(usable)["can_merge"], a["can_merge"]);
        // a seat the CLI says is signed out is no seat either (the standing rule: offer the SEAT, never the binary)
        let mut out = e("cursor", true); out.seat = Some(false);
        let b = advice(&[e("codex", true), out, e("opencode", false)]);
        assert_eq!(b["can_merge"], json!(false), "{b}");
        assert_eq!(b["installed"], json!(1), "{b}");
        // one frontier seat and nothing cheap: the cheap hands are what is sold, and no seat is named for the second brain
        let c = advice(&[e("codex", true), e("opencode", false)]);
        assert_eq!(c["unlock"][0]["engine"], json!("opencode"), "{c}");
        assert_eq!(c["unlock"][1]["engine"], json!("a second frontier seat"), "{c}");
    }
    #[test]
    fn the_advice_names_what_is_missing_and_argues_for_it_with_a_measured_row() {
        // nothing installed: the first thing to say is that antiloki brings no model of its own
        let none = advice(&[e("claude-code", false), e("codex", false), e("opencode", false)]);
        assert!(none["headline"].as_str().unwrap().contains("bundles no model"));
        assert_eq!(none["can_merge"], json!(false));

        // a frontier subscription and nothing else: agents yes, merge no — and the pitch is the measured row
        let solo = advice(&[e("claude-code", true), e("codex", false), e("opencode", false)]);
        assert_eq!(solo["can_merge"], json!(false));
        let why = solo["unlock"][0]["why"].as_str().unwrap();
        assert!(why.contains("$0.25") && why.contains("$1.28"), "the argument is the measured pair, not an adjective: {why}");
        assert!(why.contains("37/37"), "…including that the cheap run did not cut quality");

        // OpenCode alone is ONE seat - the Add-agent window (`providers_from`) wants two - and the honest caveat is offered with the cheapest row
        let oc = advice(&[e("claude-code", false), e("codex", false), e("opencode", true)]);
        assert_eq!(oc["can_merge"], json!(false));
        assert!(oc["unlock"][0]["why"].as_str().unwrap().contains("missed a bug"), "the cheapest row's weakness is said out loud");

        // everything present: nothing to sell
        let full = advice(&[e("claude-code", true), e("codex", true), e("opencode", true)]);
        assert_eq!(full["unlock"].as_array().unwrap().len(), 0);
        assert!(full["headline"].as_str().unwrap().contains("Everything a merge needs"));
    }
}

#[cfg(test)]
mod seat_tests {
    use super::*;
    #[test]
    fn the_screen_says_what_is_missing_and_how_to_fix_it() {
        assert!(missing_for(false, Some(false), "agy", "curl … | bash", "agy").contains("not on this machine"));
        assert!(missing_for(true, Some(false), "grok", "npm i …", "grok login").contains("not signed in"));
        assert!(missing_for(true, Some(false), "grok", "npm i …", "grok login").contains("grok login"),
                "the fix must be in the sentence or the operator cannot act on it");
        assert_eq!(missing_for(true, Some(true), "claude", "…", "claude"), "", "a real seat is missing nothing");
        assert_eq!(missing_for(true, None, "agy", "…", "agy"), "",
                   "the keyring cannot be read cheaply — a maybe is offered, and a wrong guess costs one fast auth error");
    }
    #[test]
    fn every_seat_says_how_to_install_on_the_platform_it_is_on() {
        for (key, _, _, _) in ENGINES {
            for os in ["windows", "linux", "macos"] {
                let l = install_for(key, os);
                assert!(!l.is_empty(), "{key} has no install line on {os}");
                if os == "windows" { assert!(!l.contains("| bash"), "{key} on windows must not say `| bash`: {l}"); }
                else { assert!(!l.contains("irm "), "{key} on {os} must not say `irm`: {l}"); }
            }
        }
        assert!(install_for("claude-code", "windows").contains("irm https://claude.ai/install.ps1 | iex"));
        assert!(install_for("cursor", "windows").contains("irm 'https://cursor.com/install?win32=true' | iex"));
        assert!(install_for("gemini", "windows").contains("irm https://antigravity.google/cli/install.ps1 | iex"));
        assert!(install_for("cursor", "linux").contains("curl https://cursor.com/install -fsS | bash"));
        assert!(install_for("codex", "windows").contains("npm i -g @openai/codex") && needs_for("codex", "windows").contains("Node.js"), "an npm install says it needs Node");
        assert_eq!(needs_for("claude-code", "windows"), "", "the native Claude installer needs nothing");
        assert_eq!(install_for("nope", "linux"), "");
        for (key, _, _, _) in ENGINES { for os in ["windows", "linux"] {
            let c = install_cmd_for(key, os);
            assert!(!c.is_empty() && !c.contains(" — ") && !c.contains("then ") && !c.contains("In PowerShell"), "{key}/{os}: the copyable command is the command alone: {c}");
            assert!(install_for(key, os).contains(&c), "{key}/{os}: the sentence carries the command");
        } }
        assert_eq!(shell_name("windows"), "PowerShell");
        assert_eq!(shell_name("linux"), "a terminal");
    }
    #[test]
    fn the_cursor_sign_in_names_the_binary_that_is_on_the_machine() {
        assert_eq!(signin_for_found("cursor", Some("/home/u/.local/bin/agent")), "agent login");
        assert_eq!(signin_for_found("cursor", Some("C:\\Users\\u\\AppData\\Local\\cursor-agent\\agent.cmd")), "agent login");
        assert_eq!(signin_for_found("cursor", Some("/home/u/.local/bin/cursor-agent")), "cursor-agent login");
        assert_eq!(signin_for_found("cursor", Some("C:\\x\\cursor-agent.cmd")), "cursor-agent login");
        assert_eq!(signin_for_found("cursor", None), "agent login", "nothing found: the documented name");
        assert_eq!(signin_for_found("codex", Some("/x/codex")), "codex");
    }
    #[test]
    fn every_seat_has_a_sign_in_command_and_a_ladder() {
        for (key, _, _, _) in ENGINES {
            assert!(!signin_for(key).is_empty(), "{key} has no sign-in command");
            // Codex has no ladder of models: its tiers ARE a reasoning effort on the account's own model
            // the DeepSeek harness is a terminal provider, not one of the router's seats: it has no tiers to climb
            if !matches!(key, "opencode" | "codex" | "deepseek") { assert!(!ladder_of(key).is_empty(), "{key} has no ladder"); }
        }
    }

    #[test]
    fn the_deepseek_harness_is_found_in_npxs_cache_newest_first_and_is_a_provider_in_the_list() {
        let d = std::env::temp_dir().join(format!("antiloki-dsh-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d);
        assert_eq!(deepseek_bin_in(&d), None, "no cache, no harness");
        for h in ["aaa", "bbb"] { let bin = d.join(h).join("node_modules/.bin"); std::fs::create_dir_all(&bin).unwrap(); std::fs::write(bin.join("dsh"), "#!/bin/sh\n").unwrap(); std::thread::sleep(std::time::Duration::from_millis(20)); }
        std::fs::create_dir_all(d.join("ccc/node_modules/.bin")).unwrap();   // a cache folder of some other package: no dsh in it
        assert!(deepseek_bin_in(&d).unwrap().contains("/bbb/node_modules/.bin/dsh"), "the newest");
        let _ = std::fs::remove_dir_all(&d);
        let row = ENGINES.iter().find(|e| e.0 == "deepseek").unwrap(); assert_eq!((row.1, row.2), ("DeepSeek Harness", "dsh"));
        assert_eq!(install_cmd_for("deepseek", "linux"), "npm i -g @deepseek-ai/dsh"); assert_eq!(signin_for("deepseek"), "dsh web"); assert!(install_for("deepseek", "linux").contains("dsh web"));
        assert_eq!(ENGINES.len(), 7, "the six seats and the harness");
    }
}
