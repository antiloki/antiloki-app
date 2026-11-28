//! CONNECT ANTILOKI TO A PROVIDER'S CLI (operator 2026-11-18). A pane antiloki launches gets its MCP server through the launch flags;
//! a CLI the person starts in an ordinary terminal has none, and says so ("you don't have access?"). One click per provider fixes it:
//! the provider's OWN `mcp add` where it has one (its format, its scopes), and the config file written directly, after a backup, where it has not.
//! Never touches a file it cannot parse; never overwrites another server; `remove` takes out only the `antiloki` entry.
use serde_json::{json, Value};
use std::path::PathBuf;

pub const NAME: &str = "antiloki";
/// (key, label, the CLI's program, how it is connected)
pub const PROVIDERS: [(&str, &str, &str, &str); 7] = [
    ("claude-code", "Claude Code", "claude", "command"), ("codex", "Codex", "codex", "command"), ("gemini", "Gemini CLI", "gemini", "command"),
    ("grok", "Grok", "grok", "command"), ("antigravity", "Antigravity", "agy", "command"), ("opencode", "OpenCode", "opencode", "file"), ("cursor", "Cursor", "cursor-agent", "file"),
];
fn home() -> PathBuf { std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default() }
/// The config file of a provider that is connected by writing it. Pure over the home directory.
pub fn file_of(key: &str, home: &std::path::Path) -> Option<PathBuf> {
    match key { "opencode" => Some(home.join(".config/opencode/opencode.json")), "cursor" => Some(home.join(".cursor/mcp.json")), _ => None }
}
/// The arguments of `<cli> mcp add` for a provider that has one. Pure.
pub fn add_args(key: &str, agent: &str, url: &str) -> Option<Vec<String>> {
    let env = format!("ANTILOKI_BE_URL={url}"); let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    Some(match key {
        "claude-code" => [s(&["mcp", "add", "--scope", "user", NAME, "-e", &env, "--", agent, "mcp"])].concat(),
        "codex" => s(&["mcp", "add", NAME, "--env", &env, "--", agent, "mcp"]),
        "gemini" => s(&["mcp", "add", "-s", "user", "-e", &env, NAME, agent, "mcp"]),
        "grok" => s(&["mcp", "add", "--scope", "user", "-e", &env, NAME, "--", agent, "mcp"]),
        "antigravity" => s(&["mcp", "add", "-e", &env, NAME, "--", agent, "mcp"]),
        _ => return None,
    })
}
pub fn remove_args(key: &str) -> Option<Vec<String>> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    Some(match key { "claude-code" => s(&["mcp", "remove", "--scope", "user", NAME]), "gemini" => s(&["mcp", "remove", "-s", "user", NAME]), "codex" | "grok" | "antigravity" => s(&["mcp", "remove", NAME]), _ => return None })
}
/// A config document with the antiloki server added (other servers kept). Pure.
pub fn with_server(key: &str, mut doc: Value, agent: &str, url: &str) -> Result<Value, String> {
    if !doc.is_object() { return Err("the config file is not a JSON object".into()); }
    let (top, entry) = match key {
        "opencode" => ("mcp", json!({ "type": "local", "command": [agent, "mcp"], "environment": { "ANTILOKI_BE_URL": url }, "enabled": true })),
        "cursor" => ("mcpServers", json!({ "command": agent, "args": ["mcp"], "env": { "ANTILOKI_BE_URL": url } })),
        _ => return Err(format!("{key} is connected with its own command")),
    };
    let o = doc.as_object_mut().unwrap(); let servers = o.entry(top).or_insert_with(|| json!({}));
    servers.as_object_mut().ok_or_else(|| format!("`{top}` in the config file is not an object"))?.insert(NAME.into(), entry); Ok(doc)
}
pub fn without_server(key: &str, mut doc: Value) -> Value {
    let top = if key == "opencode" { "mcp" } else { "mcpServers" };
    if let Some(m) = doc.get_mut(top).and_then(Value::as_object_mut) { m.remove(NAME); } doc
}
pub fn has_server(key: &str, doc: &Value) -> bool { doc.get(if key == "opencode" { "mcp" } else { "mcpServers" }).and_then(|m| m.get(NAME)).is_some() }
/// What `<cli> mcp list` printed: is there a server called antiloki in it? Pure.
pub fn listed(out: &str) -> bool { out.lines().any(|l| l.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_')).any(|w| w == NAME)) }

pub fn agent_bin() -> String { std::env::var("ANTILOKI_AGENT_BIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| std::env::current_exe().ok().map(|p| p.with_file_name(if cfg!(windows) { "antiloki-agent.exe" } else { "antiloki-agent" }).to_string_lossy().into_owned()).unwrap_or_else(|| "antiloki-agent".into())) }
fn be_url() -> String { std::env::var("ANTILOKI_BE_URL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| format!("http://127.0.0.1:{}", std::env::var("ANTILOKI_SERVE_PORT").unwrap_or_else(|_| "41931".into()))) }
async fn run(bin: &str, args: &[String]) -> Result<String, String> { run_for(bin, args, 25).await }
/// A read-only probe gets a short limit; `mcp add` / `mcp remove` write, and a slow wrapper must not cut them off half-way.
async fn run_for(bin: &str, args: &[String], secs: u64) -> Result<String, String> {
    let o = tokio::time::timeout(std::time::Duration::from_secs(secs), crate::proc_async::tokio_cmd(bin).args(args).kill_on_drop(true).output()).await
        .map_err(|_| format!("{bin} did not answer in {secs} s"))?.map_err(|e| format!("{bin} did not start: {e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    if o.status.success() { Ok(text) } else { Err(text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("it failed").trim().to_string()) }
}
/// ANTIGRAVITY HAS ONE MCP LIST PER PERSON, no project file (probed: `agy mcp add <name> <command> [args]`, "add or update"). So a pane of it gets antiloki's tools through one entry that names NO pane; the
/// pane's own process carries `ANTILOKI_PANE_ID` and the helper reads it from there. The operator allowed this write on 2026-11-26. Outside antiloki the helper answers that no pane is set. A unit test never touches the real config.
pub async fn ensure_agy(bin: &str) -> Result<(), String> {
    if cfg!(test) || std::path::Path::new(bin).file_stem().and_then(|n| n.to_str()) != Some("agy") { return Ok(()); }
    let args = add_args("antigravity", &agent_bin(), &be_url()).ok_or("no add command for antigravity")?;
    run(bin, &args).await.map(|_| ())
}
/// Claude and Codex keep their user-scope servers in a file, so antiloki READS it (issue 14: `claude mcp list` health-checks every server, goes through the person's mise wrapper, and took over 70 s). Pure.
pub fn in_cli_config(key: &str, text: &str) -> Option<bool> {
    match key {
        "claude-code" => Some(serde_json::from_str::<Value>(text).ok()?.get("mcpServers").and_then(|m| m.get(NAME)).is_some()),
        "codex" => Some(text.lines().any(|l| { let l = l.trim(); l == "[mcp_servers.antiloki]" || l.starts_with("[mcp_servers.antiloki.") || l == "[mcp_servers.\"antiloki\"]" })),
        _ => None,
    }
}
fn cli_config_file(key: &str) -> Option<PathBuf> { match key { "claude-code" => Some(std::env::var_os("CLAUDE_CONFIG_DIR").map(|d| PathBuf::from(d).join(".claude.json")).unwrap_or_else(|| home().join(".claude.json"))), "codex" => Some(std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".codex")).join("config.toml")), _ => None } }
async fn connected(key: &str, bin: &str) -> bool {
    // the config file answers when it can be read and understood; otherwise (another config folder, an unreadable file) the CLI is asked, with a short limit
    if let Some(b) = cli_config_file(key).and_then(|f| std::fs::read_to_string(f).ok()).and_then(|t| in_cli_config(key, &t)) { return b; }
    if let Some(f) = file_of(key, &home()) { return std::fs::read_to_string(f).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).map_or(false, |d| has_server(key, &d)); }
    run_for(bin, &["mcp".to_string(), "list".to_string()], 10).await.map_or(false, |o| listed(&o))
}
/// `GET /api/v1/onboarding/mcp`: every provider, whether its CLI is installed here, whether antiloki is connected to it, and what the button will do.
pub async fn status() -> Value {
    // ONE AT A TIME, KEPT 30 s (issue DAN-84, 2026-11-24): every pane header polls this, and each poll ran `claude mcp list` / `codex mcp list` (slow: they health-check every MCP server the person has). Timed-out ones were never killed and
    // piled up (37 at once), holding mise's lock and starving the agent's own start. Now the lock is held while one runs, a second caller waits for it and reads its answer, and a timed-out child is killed (`kill_on_drop` in `run`).
    let mut g = status_cache().lock().await;
    if let Some((at, v)) = g.as_ref() { if at.elapsed() < std::time::Duration::from_secs(30) { return v.clone(); } }
    let v = status_uncached().await;
    *g = Some((std::time::Instant::now(), v.clone()));
    v
}
/// Forget the kept answer (a provider was just connected or removed).
async fn forget_status() { *status_cache().lock().await = None; }
fn status_cache() -> &'static tokio::sync::Mutex<Option<(std::time::Instant, Value)>> {
    static CACHE: std::sync::OnceLock<tokio::sync::Mutex<Option<(std::time::Instant, Value)>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| tokio::sync::Mutex::new(None))
}
async fn status_uncached() -> Value {
    let mut rows = vec![];
    for (key, label, cli, how) in PROVIDERS {
        let installed = crate::proc_async::which(cli).is_some(); let on = installed && connected(key, cli).await;
        rows.push(json!({ "key": key, "label": label, "cli": cli, "installed": installed, "connected": on, "how": how,
            "will": if how == "file" { format!("adds an `antiloki` entry to {} (a backup of the file is kept next to it)", file_of(key, &home()).map(|p| p.display().to_string()).unwrap_or_default()) } else { format!("runs `{cli} mcp add antiloki …` (the CLI's own command, for you as a user)") } }));
    }
    json!({ "ok": true, "why": "A CLI started in an ordinary terminal does not know antiloki. Connect it once and every session of that CLI can use antiloki's tools (context, reviews, panes, teams). Panes antiloki starts itself are always connected.", "providers": rows, "agent": agent_bin(), "url": be_url() })
}
/// `POST /api/v1/onboarding/mcp {provider, action: "install" | "remove"}`
pub async fn apply(provider: &str, action: &str) -> Value {
    let Some((key, _, cli, _)) = PROVIDERS.iter().find(|p| p.0 == provider).copied() else { return json!({ "ok": false, "error": format!("`{provider}` is not a provider antiloki can connect") }) };
    if crate::proc_async::which(cli).is_none() { return json!({ "ok": false, "error": format!("`{cli}` is not installed here: install it first") }) }
    let (agent, url) = (agent_bin(), be_url());
    let res: Result<(), String> = async {
        if let Some(f) = file_of(key, &home()) {
            let text = std::fs::read_to_string(&f).unwrap_or_default();
            let doc: Value = if text.trim().is_empty() { json!({}) } else { serde_json::from_str(&text).map_err(|e| format!("{} is not plain JSON ({e}): not touched; add it by hand", f.display()))? };
            let new = if action == "remove" { without_server(key, doc) } else { with_server(key, doc, &agent, &url)? };
            if f.exists() { let bak = f.with_extension("json.before-antiloki"); if !bak.exists() { std::fs::copy(&f, &bak).map_err(|e| format!("no backup, nothing changed: {e}"))?; } }
            if let Some(d) = f.parent() { let _ = std::fs::create_dir_all(d); }
            std::fs::write(&f, serde_json::to_string_pretty(&new).unwrap_or_default() + "\n").map_err(|e| e.to_string())?;
            if key == "cursor" && action != "remove" { let _ = run(cli, &["mcp".into(), "enable".into(), NAME.into()]).await; }
            Ok(())
        } else {
            if action == "remove" { run(cli, &remove_args(key).unwrap_or_default()).await.map(|_| ()) }
            else { if connected(key, cli).await { let _ = run(cli, &remove_args(key).unwrap_or_default()).await; } run(cli, &add_args(key, &agent, &url).unwrap_or_default()).await.map(|_| ()) }
        }
    }.await;
    forget_status().await;
    match res { Ok(()) => json!({ "ok": true, "connected": action != "remove" && connected(key, cli).await, "provider": key }), Err(e) => json!({ "ok": false, "error": e, "provider": key }) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn claude_and_codex_are_read_from_their_own_config_file() {
        assert_eq!(in_cli_config("claude-code", r#"{"mcpServers":{"antiloki":{}}}"#), Some(true));
        assert_eq!(in_cli_config("claude-code", r#"{"mcpServers":{"other":{}}}"#), Some(false));
        assert_eq!(in_cli_config("codex", "[mcp_servers.antiloki]\ncommand = \"x\"\n"), Some(true));
        assert_eq!(in_cli_config("codex", "[mcp_servers.other]\n"), Some(false));
        assert_eq!(in_cli_config("gemini", "x"), None);
    }
    #[test]
    fn each_command_provider_gets_its_own_add_and_the_url_travels_in_the_env() {
        for k in ["claude-code", "codex", "gemini", "grok", "antigravity"] { let a = add_args(k, "/x/antiloki-agent", "http://127.0.0.1:41999").unwrap(); assert_eq!(a[0], "mcp"); assert!(a.contains(&"ANTILOKI_BE_URL=http://127.0.0.1:41999".to_string()), "{k}"); assert!(a.iter().any(|x| x == "antiloki") && a.ends_with(&["/x/antiloki-agent".to_string(), "mcp".to_string()]), "{k}: {a:?}"); assert!(remove_args(k).is_some(), "{k}"); }
        assert!(add_args("opencode", "a", "u").is_none() && add_args("cursor", "a", "u").is_none());
        assert_eq!(PROVIDERS.iter().filter(|p| p.3 == "file").count(), 2);
    }
    #[test]
    fn a_file_provider_keeps_every_other_server_and_remove_takes_only_ours() {
        let before = json!({ "model": "x", "mcp": { "other": { "type": "local", "command": ["o"] } } });
        let on = with_server("opencode", before.clone(), "/a/agent", "http://h:1").unwrap();
        assert!(has_server("opencode", &on) && on["mcp"]["other"] == before["mcp"]["other"] && on["model"] == "x");
        assert_eq!(on["mcp"]["antiloki"]["command"], json!(["/a/agent", "mcp"]));
        let off = without_server("opencode", on.clone()); assert_eq!(off, before);
        let c = with_server("cursor", json!({}), "/a/agent", "http://h:1").unwrap(); assert_eq!(c["mcpServers"]["antiloki"]["args"], json!(["mcp"])); assert!(has_server("cursor", &c));
        assert!(with_server("cursor", json!([]), "a", "u").is_err() && with_server("cursor", json!({ "mcpServers": 3 }), "a", "u").is_err(), "a file it does not understand is refused");
        assert!(with_server("claude-code", json!({}), "a", "u").is_err());
    }
    #[test]
    fn the_list_output_is_read_by_word_not_by_substring() {
        assert!(listed("✓ antiloki: /x/agent mcp - Connected\n  other: y")); assert!(listed("antiloki  stdio  enabled")); assert!(!listed("antiloki-old: x\nmy_antiloki_copy: y")); assert!(!listed("No MCP servers configured."));
        assert_eq!(file_of("cursor", std::path::Path::new("/h")).unwrap(), PathBuf::from("/h/.cursor/mcp.json")); assert!(file_of("codex", std::path::Path::new("/h")).is_none());
    }
}
