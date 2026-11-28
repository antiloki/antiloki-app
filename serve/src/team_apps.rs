//! APPS IN TEAMS (operator 2026-11-13: teams connect to the web apps of the store, through their MCPs: research on the web, post on Instagram through Meta Business; "if I am not logged in, the Leader says so before the first run, can
//! open the site I need to log in, prepare everything"). The installed apps (`webapps.json`) are already given to every agent's CLI as MCP servers named `app-<id>`; an agent that lists `app-<id>` in its `mcp` is now also ALLOWED
//! them (see `agent_app_ids`, `mcp_config_for`). Sign-in is never antiloki's: each CLI connects to the MCP server and asks for its own; this module only SEES where each app stands and lets the Leader get the person there.
//!   `list`       every installed app: its address, whether it answers (a 401 is a sign-in wall: "needs sign-in"), and what it could be used for.
//!   `preflight`  for a team's graph: the apps its nodes use, each one's state, what only the person can do (sign in, a Business page, two-step codes) and which steps post outward (they stay behind a gate).
//!   `login`      open an installed app's own page in the person's browser (an https address of an app they installed), so they can sign in; it never types a password or reads one.
use serde_json::{json, Map, Value};
use antiloki_rs::cockpit__helpers_webapps as w;

/// The app ids a graph's agents and tool nodes use: `app-<id>` or `mcp__app-<id>[__tool]` in an agent's `mcp`, and `use_tool` of type `webapp` named by id. Pure.
pub fn app_ids(graph: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vec![];
    for (node, n) in graph["nodes"].as_object().into_iter().flatten() {
        for m in n["agent"]["mcp"].as_array().into_iter().flatten().filter_map(Value::as_str) { if let Some(id) = app_of(m) { out.push((node.clone(), id)); } }
        if n["node"] == "use_tool" && n["tool"]["type"] == "webapp" { if let Some(id) = n["tool"]["name"].as_str() { out.push((node.clone(), id.trim_start_matches("app-").to_string())); } }
    }
    out
}
/// `app-meta-business`, `app-meta-business:post`, `mcp__app-meta-business__post` -> `meta-business`. Pure.
pub fn app_of(entry: &str) -> Option<String> { let e = entry.trim().strip_prefix("mcp__").unwrap_or(entry.trim()); let r = e.strip_prefix("app-")?; let id: String = r.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect(); let id = id.trim_end_matches('-').to_string(); (!id.is_empty()).then_some(id) }
/// The CLI's own tool names to allow for the apps an agent lists (the whole server): `mcp__app-<id>`. Pure.
pub fn allowed_tools(ids: &[String]) -> Vec<String> { ids.iter().map(|i| format!("mcp__app-{i}")).collect() }
/// The `--mcp-config` file's content for these apps (only the ones installed, only the ones asked for). Pure over the installed list.
pub fn mcp_config_for(ids: &[String], installed: &[Value]) -> Value {
    let all = w::servers_of(installed); let mut m = Map::new(); for i in ids { let k = format!("app-{i}"); if let Some(s) = all.get(&k) { m.insert(k, s.clone()); } } json!({ "mcpServers": m })
}
/// A reply status as a state. Pure.
pub fn state_of(status: Option<u16>) -> &'static str { match status { Some(401 | 403) => "needs_signin", Some(s) if s < 500 => "answers", Some(_) => "server_error", None => "unreachable" } }
async fn reach(url: &str) -> Option<u16> {
    if url.is_empty() { return None; } let c = reqwest::Client::builder().timeout(std::time::Duration::from_secs(6)).redirect(reqwest::redirect::Policy::limited(3)).user_agent("antiloki").build().ok()?;
    c.get(url).header("accept", "text/event-stream, application/json, text/html").send().await.ok().map(|r| r.status().as_u16())
}
pub async fn list() -> Value {
    let apps = w::load(); let mut out = vec![];
    for a in &apps { let st = reach(a["mcp_url"].as_str().unwrap_or("")).await; out.push(json!({ "id": a["id"], "name": a["name"], "kind": a["kind"], "app_url": a["app_url"], "mcp_url": a["mcp_url"], "state": state_of(st), "http_status": st, "use_in_a_team": format!("give an agent `app-{}` in its `mcp`", a["id"].as_str().unwrap_or("")) })); }
    json!({ "ok": true, "installed": out, "store_hint": "an app not installed here is in the store (Cowork -> web apps): the person installs it, or you say which", "how_sign_in_works": "each agent's CLI asks the app for its own sign-in (OAuth): antiloki never sees a password. `needs_signin` = the app answered 401: the person must sign in once; `answers` = it answers, and whether the person is signed in is known only when a call is made (a first trial call tells)." })
}
/// What a team needs from the person before its first run, per app. Pure over the graph, the installed list and the states found.
pub fn preflight(graph: &Value, installed: &[Value], states: &std::collections::BTreeMap<String, Option<u16>>) -> Value {
    let used = app_ids(graph); let mut ids: Vec<String> = used.iter().map(|u| u.1.clone()).collect(); ids.sort(); ids.dedup();
    let mut items = vec![]; let mut human: Vec<String> = vec![]; let mut ready = true;
    for id in &ids {
        let nodes: Vec<&str> = used.iter().filter(|u| &u.1 == id).map(|u| u.0.as_str()).collect(); let app = installed.iter().find(|a| a["id"] == json!(id)); 
        let outward = nodes.iter().any(|n| graph["nodes"][n]["agent"]["effects"].as_array().map_or(false, |e| e.iter().any(|x| x == "outward" || x == "money")));
        match app {
            None => { ready = false; human.push(format!("install the app `{id}` from the store (Cowork -> web apps)")); items.push(json!({ "app": id, "nodes": nodes, "state": "not_installed", "action": "the person installs it from the store" })); }
            Some(a) => { let st = states.get(id).copied().flatten(); let state = state_of(states.get(id).copied().flatten()); if state != "answers" { ready = false; }
                if state == "needs_signin" { human.push(format!("sign in to {} (the Leader can open its page: antiloki_team_apps login {{app: \"{id}\"}})", a["name"].as_str().unwrap_or(id))); } else if state == "unreachable" || state == "server_error" { human.push(format!("{} does not answer right now: check its connection", a["name"].as_str().unwrap_or(id))); }
                if outward { human.push(format!("{} is used to POST or SPEND for you (outward): keep a human gate before that step, and check the account it acts for", a["name"].as_str().unwrap_or(id))); }
                items.push(json!({ "app": id, "name": a["name"], "nodes": nodes, "state": state, "http_status": st, "outward": outward, "app_url": a["app_url"] })); } }
    }
    json!({ "ok": true, "apps_used": ids.len(), "ready": ready, "items": items, "only_the_person_can": human,
            "reminder": "Say all of this to the person BEFORE the first run: what to install, where to sign in, what the team will post or spend and where the gate is. Whether a signed-in app really lets the call through is known only on a first call: make that call (a node or a small trial) before the team depends on it." })
}
pub async fn preflight_of(graph: &Value) -> Value {
    let installed = w::load(); let mut states = std::collections::BTreeMap::new();
    for id in app_ids(graph).into_iter().map(|u| u.1).collect::<std::collections::BTreeSet<_>>() { let url = installed.iter().find(|a| a["id"] == json!(id)).and_then(|a| a["mcp_url"].as_str().map(String::from)).unwrap_or_default(); let mut st = reach(&url).await;
        // a page that wants sign-in is still ready when the token the person kept lets the MCP list its tools (the answer an agent's call would get)
        if matches!(st, Some(401 | 403)) && crate::app_tokens::has(&id) { crate::mcp_oauth::refresh_if_due(&id).await; if let Ok(c) = crate::mcp_client::Client::connect(&url, crate::app_tokens::get(&id), Default::default()).await { if c.tools().await.is_ok() { st = Some(200); } } }
        states.insert(id, st); }
    preflight(graph, &installed, &states)
}
/// Open an installed app's own page in the person's browser. Only an https address (or an app on this machine) of an app that IS installed; nothing is typed or read.
pub fn login(app: &str) -> Result<Value, String> {
    let apps = w::load(); let a = apps.iter().find(|a| a["id"] == json!(app)).ok_or_else(|| format!("`{app}` is not installed here"))?; let url = a["app_url"].as_str().unwrap_or("").to_string(); w::clean_url(&url)?;
    let opener = if cfg!(target_os = "macos") { "open" } else if cfg!(target_os = "windows") { "explorer" } else { "xdg-open" };
    crate::proc::cmd(opener).arg(&url).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().map_err(|e| format!("could not open the browser ({e}): the address is {url}"))?;
    Ok(json!({ "ok": true, "opened": url, "next": "the person signs in there. For an OAuth MCP the first call from an agent's terminal asks for its own consent: tell them to expect it." }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn graph() -> Value { json!({ "nodes": { "researcher": { "node": "role", "role": "agent", "agent": { "mcp": ["app-google-search", "antiloki_panes"], "effects": ["read", "web"] } },
        "poster": { "node": "role", "role": "agent", "agent": { "mcp": ["mcp__app-meta-business__create_post"], "effects": ["outward"] } }, "n": { "node": "use_tool", "tool": { "type": "webapp", "name": "app-notion" } } } }) }
    #[test]
    fn the_apps_a_team_uses_are_found_in_every_spelling_and_become_allowed_tools_and_a_config() {
        for (e, id) in [("app-meta-business", Some("meta-business")), ("app-meta-business:post", Some("meta-business")), ("mcp__app-meta-business__post", Some("meta-business")), ("antiloki_panes", None), ("app-", None)] { assert_eq!(app_of(e).as_deref(), id, "{e}"); }
        let mut ids: Vec<String> = app_ids(&graph()).into_iter().map(|u| u.1).collect(); ids.sort(); assert_eq!(ids, vec!["google-search", "meta-business", "notion"]);
        assert_eq!(allowed_tools(&["a".into()]), vec!["mcp__app-a"]);
        let inst = vec![json!({ "id": "meta-business", "mcp_url": "https://mcp.example/meta", "transport": "http" }), json!({ "id": "other", "mcp_url": "https://x/y" })];
        let c = mcp_config_for(&["meta-business".into(), "ghost".into()], &inst); assert_eq!(c["mcpServers"]["app-meta-business"]["url"], "https://mcp.example/meta"); assert!(c["mcpServers"].get("app-ghost").is_none() && c["mcpServers"].get("app-other").is_none(), "only what was asked for and installed");
    }
    #[test]
    fn the_preflight_says_what_only_the_person_can_do_before_the_first_run_and_flags_posting() {
        let inst = vec![json!({ "id": "meta-business", "name": "Meta Business", "app_url": "https://business.facebook.com" }), json!({ "id": "google-search", "name": "Google Search", "app_url": "https://g.example" })];
        let mut st = std::collections::BTreeMap::new(); st.insert("meta-business".to_string(), Some(401u16)); st.insert("google-search".to_string(), Some(200u16)); st.insert("notion".to_string(), None);
        let p = preflight(&graph(), &inst, &st); assert_eq!(p["ready"], false); let said = p["only_the_person_can"].to_string();
        for n in ["sign in to Meta Business", "install the app `notion`", "POST or SPEND", "human gate"] { assert!(said.contains(n), "{n} in {said}"); }
        let item = |id: &str| p["items"].as_array().unwrap().iter().find(|i| i["app"] == json!(id)).cloned().unwrap(); assert_eq!(item("meta-business")["state"], "needs_signin"); assert_eq!(item("meta-business")["outward"], true); assert_eq!(item("google-search")["state"], "answers"); assert_eq!(item("notion")["state"], "not_installed");
        let ok = preflight(&json!({ "nodes": {} }), &[], &Default::default()); assert_eq!(ok["ready"], true); assert_eq!(state_of(Some(200)), "answers"); assert_eq!(state_of(None), "unreachable"); assert_eq!(state_of(Some(503)), "server_error");
    }
}
