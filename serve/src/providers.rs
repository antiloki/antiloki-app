//! AGENT PROVIDERS THE PERSON CAN ADD (RFC 0023). The five defaults are built in. An agent may PROPOSE another one (`antiloki_provider_propose`); nothing of a proposal is run until the person presses
//! Confirm on a card that shows the exact commands. Confirm runs the install (when there is one) and then antiloki's own test: the command is found, it answers its version, and a short headless prompt gets a
//! real reply. Only a provider that passed is offered in the pane menu. SQLite (`<antiloki home>/providers.db`) is the truth; YAML is its rendering. A recipe holds no secret.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use yaml_rust2::{Yaml, YamlEmitter, YamlLoader};

/// An added provider's pane kind is `x-<provider>`, so it can never collide with a built-in seat.
pub const PREFIX: &str = "x-";
pub const TOOLS: [&str; 1] = ["antiloki_provider_propose"];
const WORD: &str = "ANTILOKI-OK";
const BUILTINS: [&str; 7] = ["claude-code", "codex", "cursor", "gemini", "opencode", "grok", "deepseek"];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub provider: String,
    pub title: String,
    /// the program a pane starts (a name on the PATH, or a full path)
    pub command: String,
    #[serde(default)] pub args: Vec<String>,
    /// how it proves it is there
    #[serde(default = "version_flag")] pub detect: Vec<String>,
    /// the headless one-shot: exactly one item is `{prompt}`
    pub smoke: Vec<String>,
    /// the human's steps (a sign-in), in words
    #[serde(default)] pub setup: String,
    #[serde(default)] pub link: String,
    /// the install command as separate words; empty = the person installs it themselves
    #[serde(default)] pub install: Vec<String>,
}
fn version_flag() -> Vec<String> { vec!["--version".into()] }

#[derive(Clone, Debug, Serialize)]
pub struct Row { pub provider: String, pub recipe: Recipe, pub status: String, pub report: String, pub proposed_by: String, pub updated_ms: i64 }

/// What a recipe may be. Pure - tested.
pub fn validate(r: &Recipe) -> Result<(), String> {
    let slug = |s: &str| (2..=32).contains(&s.len()) && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !s.starts_with('-');
    if !slug(&r.provider) { return Err("provider: 2 to 32 of a-z, 0-9 and -".into()); }
    if BUILTINS.contains(&r.provider.as_str()) { return Err(format!("`{}` is already built into antiloki", r.provider)); }
    if r.title.trim().is_empty() || r.title.len() > 60 { return Err("title: 1 to 60 characters".into()); }
    // a program, not a shell line: nothing here is ever handed to a shell
    let plain = |s: &str| !s.is_empty() && !s.chars().any(|c| c.is_control() || "|&;<>$`\"'*?(){}[]!\n".contains(c));
    if !plain(&r.command) || r.command.contains(' ') { return Err("command: one program name or path, no spaces or shell characters".into()); }
    for (what, list) in [("args", &r.args), ("detect", &r.detect), ("install", &r.install)] {
        if list.len() > 24 || list.iter().any(|a| a.len() > 300 || a.chars().any(char::is_control)) { return Err(format!("{what}: at most 24 plain words")); }
    }
    if r.install.first().is_some_and(|p| !plain(p) || ["sh", "bash", "zsh", "cmd", "powershell", "pwsh"].contains(&p.as_str())) { return Err("install: a program and its words, never a shell (a person runs a script installer themselves; say so in `setup`)".into()); }
    if r.smoke.iter().filter(|a| a.as_str() == "{prompt}").count() != 1 || r.smoke.len() > 24 { return Err("smoke: the headless arguments with exactly one item that is {prompt}".into()); }
    let all = serde_json::to_string(r).unwrap_or_default().to_lowercase();
    if ["sk-", "api_key=", "apikey=", "token=", "password="].iter().any(|m| all.contains(m)) { return Err("a recipe holds no secret: sign-in is the person's step".into()); }
    if !r.link.is_empty() && !r.link.starts_with("https://") { return Err("link: https only".into()); }
    Ok(())
}

pub fn to_yaml(r: &Recipe) -> String {
    let st = |s: &str| Yaml::String(s.to_string());
    let arr = |v: &[String]| Yaml::Array(v.iter().map(|x| st(x)).collect());
    let mut h = yaml_rust2::yaml::Hash::new();
    for (k, v) in [("provider", st(&r.provider)), ("title", st(&r.title)), ("command", st(&r.command)), ("args", arr(&r.args)), ("detect", arr(&r.detect)), ("smoke", arr(&r.smoke)), ("setup", st(&r.setup)), ("link", st(&r.link)), ("install", arr(&r.install))] { h.insert(st(k), v); }
    let mut out = String::new(); let _ = YamlEmitter::new(&mut out).dump(&Yaml::Hash(h));
    out.trim_start_matches("---").trim_start().to_string() + "\n"
}
pub fn from_yaml(text: &str) -> Result<Recipe, String> {
    let _ = YamlLoader::load_from_str(text).map_err(|e| format!("not YAML: {e}"))?;
    let v = crate::team_yaml::parse_strict(text)?;
    let r: Recipe = serde_json::from_value(v).map_err(|e| format!("not a provider recipe: {e}"))?; validate(&r)?; Ok(r)
}

fn home() -> PathBuf { std::env::var_os("ANTILOKI_HOME").map(PathBuf::from).unwrap_or_else(|| std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|h| Path::new(&h).join(".antiloki")).unwrap_or_else(|| PathBuf::from(".antiloki"))) }
fn now_ms() -> i64 { crate::cli::now_ms() }

pub fn open(dir: &Path) -> Result<rusqlite::Connection, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let c = rusqlite::Connection::open(dir.join("providers.db")).map_err(|e| e.to_string())?;
    c.execute_batch("CREATE TABLE IF NOT EXISTS providers(provider TEXT PRIMARY KEY, recipe TEXT NOT NULL, status TEXT NOT NULL, report TEXT NOT NULL DEFAULT '', proposed_by TEXT NOT NULL DEFAULT '', updated_ms INTEGER NOT NULL);").map_err(|e| e.to_string())?;
    Ok(c)
}
pub fn rows(dir: &Path) -> Vec<Row> {
    let Ok(c) = open(dir) else { return vec![] };
    let Ok(mut st) = c.prepare("SELECT provider, recipe, status, report, proposed_by, updated_ms FROM providers ORDER BY provider") else { return vec![] };
    let it = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?, r.get::<_, i64>(5)?)));
    it.map(|rs| rs.flatten().filter_map(|(provider, recipe, status, report, proposed_by, updated_ms)| Some(Row { provider, recipe: serde_json::from_str(&recipe).ok()?, status, report, proposed_by, updated_ms })).collect()).unwrap_or_default()
}
pub fn get(dir: &Path, provider: &str) -> Option<Row> { rows(dir).into_iter().find(|r| r.provider == provider) }

/// An agent's (or an imported) recipe: stored as a proposal. A change to a provider that was ready makes it a proposal again: what runs is what the person confirmed.
pub fn propose(dir: &Path, r: Recipe, by: &str) -> Result<Row, String> {
    validate(&r)?;
    if let Some(old) = get(dir, &r.provider) { if old.status == "ready" && old.recipe == r { return Ok(old); } }
    let c = open(dir)?;
    c.execute("INSERT INTO providers(provider, recipe, status, report, proposed_by, updated_ms) VALUES(?1,?2,'proposed','',?3,?4) ON CONFLICT(provider) DO UPDATE SET recipe=?2, status='proposed', report='', proposed_by=?3, updated_ms=?4",
        rusqlite::params![r.provider, serde_json::to_string(&r).map_err(|e| e.to_string())?, by, now_ms()]).map_err(|e| e.to_string())?;
    get(dir, &r.provider).ok_or_else(|| "the proposal was not stored".into())
}
fn set_status(dir: &Path, provider: &str, status: &str, report: &str) -> Result<(), String> {
    open(dir)?.execute("UPDATE providers SET status=?2, report=?3, updated_ms=?4 WHERE provider=?1", rusqlite::params![provider, status, report, now_ms()]).map(|_| ()).map_err(|e| e.to_string())
}
pub fn remove(dir: &Path, provider: &str) -> Result<(), String> { open(dir)?.execute("DELETE FROM providers WHERE provider=?1", [provider]).map(|_| ()).map_err(|e| e.to_string()) }

fn tail(s: &str) -> String { let n: Vec<char> = s.trim().chars().collect(); n[n.len().saturating_sub(400)..].iter().collect() }
async fn run(bin: &str, args: &[String], secs: u64, cwd: &Path) -> Result<String, String> {
    let mut c = crate::proc_async::tokio_cmd(bin); c.args(args).current_dir(cwd).stdin(std::process::Stdio::null()).kill_on_drop(true);
    let o = tokio::time::timeout(Duration::from_secs(secs), c.output()).await.map_err(|_| format!("no answer in {secs} s"))?.map_err(|e| format!("did not start: {e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    if o.status.success() { Ok(text) } else { Err(tail(&text)) }
}

/// ANTILOKI'S OWN TEST of a recipe: found, answers its version, and a short headless prompt gets the reply it asked for. The word is asked for in two halves, so a CLI that only echoes the prompt does not pass.
pub async fn test(r: &Recipe, cwd: &Path) -> Result<String, String> {
    let bin = antiloki_rs::proc::which(&r.command).ok_or_else(|| format!("detect: `{}` is not on this machine", r.command))?.to_string_lossy().to_string();
    run(&bin, &r.detect, 30, cwd).await.map_err(|e| format!("detect: `{} {}` failed: {e}", r.command, r.detect.join(" ")))?;
    let prompt = "Reply with one word only: join ANTILOKI and OK with a single hyphen, in capitals. No other text.".to_string();
    let args: Vec<String> = r.smoke.iter().map(|a| if a == "{prompt}" { prompt.clone() } else { a.clone() }).collect();
    let out = run(&bin, &args, 120, cwd).await.map_err(|e| format!("smoke test (is it signed in?): {e}"))?;
    if !out.contains(WORD) { return Err(format!("smoke test: no real reply. It said: {}", tail(&out))); }
    Ok("found, answers its version, and replied to a test prompt".into())
}

/// THE PERSON'S CONFIRM: the install (when the recipe has one and the program is missing), then the test. Ready only when the test passed.
pub async fn confirm(dir: &Path, provider: &str) -> Value {
    let Some(row) = get(dir, provider) else { return json!({ "ok": false, "error": "no such provider" }) };
    let r = row.recipe; let work = dir.join("provider-test"); let _ = std::fs::create_dir_all(&work);
    let _ = set_status(dir, provider, "testing", "");
    if !r.install.is_empty() && antiloki_rs::proc::which(&r.command).is_none() {
        if let Err(e) = run(&r.install[0], &r.install[1..], 900, &work).await { let why = format!("install failed: {e}"); let _ = set_status(dir, provider, "failed", &why); return json!({ "ok": false, "error": why }); }
    }
    match test(&r, &work).await {
        Ok(m) => { let _ = set_status(dir, provider, "ready", &m); json!({ "ok": true, "status": "ready", "report": m }) }
        Err(e) => { let _ = set_status(dir, provider, "failed", &e); json!({ "ok": false, "status": "failed", "error": e }) }
    }
}

/// The program and first words a pane of a READY added provider starts with (`kind` is `x-<provider>`).
pub fn launch(kind: &str) -> Option<(String, Vec<String>)> { launch_in(&home(), kind) }
pub fn launch_in(dir: &Path, kind: &str) -> Option<(String, Vec<String>)> {
    let r = get(dir, kind.strip_prefix(PREFIX)?)?; (r.status == "ready").then(|| (r.recipe.command, r.recipe.args))
}

/// The ready ones as rows of the engines list.
pub fn engine_rows_in(dir: &Path) -> Vec<Value> {
    rows(dir).into_iter().filter(|r| r.status == "ready").map(|r| {
        let path = antiloki_rs::proc::which(&r.recipe.command).map(|p| p.to_string_lossy().to_string());
        json!({ "key": format!("{PREFIX}{}", r.provider), "label": r.recipe.title, "bin": r.recipe.command, "installed": path.is_some(), "path": path, "models": [], "install": r.recipe.setup, "seat": true, "signin": r.recipe.setup, "missing": "", "off": false, "added": true })
    }).collect()
}
pub fn engine_rows() -> Vec<Value> { engine_rows_in(&home()) }

fn public(r: &Row) -> Value { json!({ "provider": r.provider, "kind": format!("{PREFIX}{}", r.provider), "title": r.recipe.title, "status": r.status, "report": r.report, "proposed_by": r.proposed_by, "updated_ms": r.updated_ms, "recipe": r.recipe, "yaml": to_yaml(&r.recipe) }) }

/// `GET /api/v1/providers`
pub async fn list_http() -> axum::Json<Value> {
    let v = tokio::task::spawn_blocking(|| rows(&home()).iter().map(public).collect::<Vec<_>>()).await.unwrap_or_default();
    axum::Json(json!({ "ok": true, "providers": v }))
}
/// `POST /api/v1/providers` `{op: confirm|reject|import, provider?, yaml?}`: the person's screen.
pub async fn act_http(body: axum::body::Bytes) -> axum::Json<Value> {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let key = b["provider"].as_str().unwrap_or("").to_string(); let dir = home();
    axum::Json(match b["op"].as_str().unwrap_or("") {
        // run apart from the request, so a closed browser does not cut an install half-way; the list shows `testing` until it ends
        "confirm" => { if get(&dir, &key).is_none() { json!({ "ok": false, "error": "no such provider" }) } else { tokio::spawn(async move { confirm(&dir, &key).await; }); json!({ "ok": true, "status": "testing" }) } }
        "reject" | "remove" => match remove(&dir, &key) { Ok(()) => json!({ "ok": true }), Err(e) => json!({ "ok": false, "error": e }) },
        "import" => match from_yaml(b["yaml"].as_str().unwrap_or("")).and_then(|r| propose(&dir, r, "you")) { Ok(r) => json!({ "ok": true, "provider": public(&r) }), Err(e) => json!({ "ok": false, "error": e }) },
        _ => json!({ "ok": false, "error": "op is confirm, reject or import" }),
    })
}

/// The agent's door: it proposes, and reads how its proposals stand. It cannot confirm.
pub fn run_tool(you: i64, args: &Value) -> Result<String, String> { run_tool_in(&home(), you, args) }
pub fn run_tool_in(dir: &Path, you: i64, args: &Value) -> Result<String, String> {
    if args["op"].as_str() == Some("list") || (args["recipe"].is_null() && args["provider"].is_null()) {
        let v = rows(dir); if v.is_empty() { return Ok("no added providers yet".into()); }
        return Ok(v.iter().map(|r| format!("- {} ({}): {}{}", r.provider, r.recipe.title, r.status, if r.report.is_empty() { String::new() } else { format!(": {}", r.report) })).collect::<Vec<_>>().join("\n"));
    }
    let mut a = if args["recipe"].is_object() { args["recipe"].clone() } else { args.clone() };
    if let Some(o) = a.as_object_mut() { o.remove("op"); }
    let r: Recipe = serde_json::from_value(a).map_err(|e| format!("not a provider recipe: {e}"))?;
    let row = propose(dir, r, &format!("pane {you}"))?;
    Ok(format!("proposed `{}`. Nothing was run. A card on the person's screen shows the commands; antiloki installs (if you gave a command) and tests it only when they press Confirm. Read the outcome later with op `list`.", row.provider))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(n: &str) -> PathBuf { let p = std::env::temp_dir().join(format!("providers-{n}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&p); std::fs::create_dir_all(&p).unwrap(); p }
    fn recipe(cmd: &str) -> Recipe { Recipe { provider: "hermes".into(), title: "Hermes".into(), command: cmd.into(), args: vec![], detect: version_flag(), smoke: vec!["-p".into(), "{prompt}".into()], setup: "hermes setup".into(), link: String::new(), install: vec![] } }

    #[test]
    fn a_recipe_is_a_program_with_a_smoke_test_and_never_a_shell_a_builtin_or_a_secret() {
        assert!(validate(&recipe("hermes")).is_ok());
        let bad = |f: &dyn Fn(&mut Recipe)| { let mut r = recipe("hermes"); f(&mut r); validate(&r).is_err() };
        assert!(bad(&|r| r.command = "curl x | bash".into()) && bad(&|r| r.install = vec!["bash".into(), "-c".into(), "x".into()]) && bad(&|r| r.provider = "codex".into()));
        assert!(bad(&|r| r.smoke = vec!["-p".into()]) && bad(&|r| r.args = vec!["--api_key=abc".into()]) && bad(&|r| r.link = "http://x".into()));
        let r = recipe("hermes"); assert_eq!(from_yaml(&to_yaml(&r)).unwrap(), r, "YAML is a lossless rendering");
    }
    #[test]
    fn an_agent_can_only_propose_and_a_changed_recipe_must_be_confirmed_again() {
        let d = tmp("a");
        let said = run_tool_in(&d, 7, &serde_json::to_value(recipe("hermes")).unwrap()).unwrap(); assert!(said.contains("Nothing was run"), "{said}");
        assert_eq!(get(&d, "hermes").unwrap().status, "proposed"); assert!(engine_rows_in(&d).is_empty() && launch_in(&d, "x-hermes").is_none(), "a proposal is not a provider");
        set_status(&d, "hermes", "ready", "ok").unwrap(); assert_eq!(launch_in(&d, "x-hermes").unwrap().0, "hermes"); assert_eq!(engine_rows_in(&d)[0]["key"], "x-hermes");
        assert_eq!(propose(&d, recipe("hermes"), "pane 7").unwrap().status, "ready", "the same recipe changes nothing");
        assert_eq!(propose(&d, recipe("other-bin"), "pane 7").unwrap().status, "proposed", "a changed command is not what the person confirmed");
        remove(&d, "hermes").unwrap(); assert!(rows(&d).is_empty()); let _ = std::fs::remove_dir_all(&d);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn confirm_makes_ready_only_what_replies_and_says_the_step_that_failed() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp("t");
        let script = |name: &str, body: &str| { let p = d.join(name); std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap(); std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap(); p.to_string_lossy().to_string() };
        let good = script("good", "[ \"$1\" = --version ] && { echo 1.0; exit 0; }\necho ANTILOKI-OK");
        propose(&d, recipe(&good), "t").unwrap(); assert_eq!(confirm(&d, "hermes").await["status"], "ready");
        let echo = script("echo", "echo \"$@\"");
        propose(&d, recipe(&echo), "t").unwrap(); let v = confirm(&d, "hermes").await; assert!(v["status"] == "failed" && v["error"].as_str().unwrap().contains("no real reply"), "{v}");
        propose(&d, recipe("/nowhere/nothing"), "t").unwrap(); assert!(confirm(&d, "hermes").await["error"].as_str().unwrap().starts_with("detect"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
