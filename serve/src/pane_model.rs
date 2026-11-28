//! THE MODEL OF AN AGENT, CHANGED IN ONE CLICK (operator 2026-11-10: "an easy button to change the model of each agent - not the provider, the model, like Haiku, Sonnet").
//!
//!   GET  /api/v1/cockpit/pane/models?pane_id=N   -> { engine, current, models: [{id, label}] }
//!   POST /api/v1/cockpit/pane/model {pane_id, model}   remember the choice on the pane and apply it
//!
//! Claude Code switches live: the engine's own `/model <alias>` is typed into its terminal (the conversation goes on). Every other engine takes its model at launch (`--model`, `-m`), so the choice is remembered and the terminal is
//! restarted with it: the answer says `restart: true` and the screen does that (that engine's conversation starts again). An empty model goes back to the engine's own default.
use crate::dispatch::{engine_call, AppState};
use crate::pty::PtyRegistry;
use axum::{body::Bytes, http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

fn reply(v: Value) -> Response { (StatusCode::OK, Json(v)).into_response() }

/// The flag an engine's CLI takes for its model at launch, with the value in the form it wants. Pure. `None` for an engine that has no such flag (the DeepSeek harness).
pub fn launch_args(engine_key: &str, model: &str) -> Vec<String> {
    let m = model.trim(); if m.is_empty() { return vec![]; }
    // a LOCAL model (`local:<tag>`) is Claude Code on the local server: the CLI is given the tag; the server and the token are in the pane's environment (`local_panes::env_for`)
    if engine_key == "claude-code" { if let Some(tag) = crate::local_panes::tag_of(m) { return vec!["--model".into(), tag.into()]; } }
    match engine_key {
        "claude-code" | "cursor" | "gemini" => vec!["--model".into(), m.into()],
        "codex" | "grok" | "opencode" | "opencode-direct" => vec!["-m".into(), m.into()],
        _ => vec![],
    }
}

/// The model a pane is set to, from its settings JSON text. Pure.
pub fn model_of(settings_json: &str) -> String { serde_json::from_str::<Value>(settings_json).ok().and_then(|v| v["model"].as_str().map(|s| s.trim().to_string())).unwrap_or_default() }

/// What a model id may look like: letters, digits and `. _ - : / [ ]` (Cursor takes bracketed parameters). Anything else would be typed into a terminal, so it is refused. Pure.
pub fn clean_model(m: &str) -> Result<String, String> {
    let m = m.trim(); if m.len() > 80 { return Err("that model name is too long".into()); }
    if m.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:/[]=,".contains(c)) { Ok(m.to_string()) } else { Err("a model name is letters, digits and . - _ : / [ ] only".into()) }
}

/// The models an engine offers on this machine, easiest first: Claude's three aliases, then each engine's own listing (`<bin> models`, cached), else the router's tier ladder. Pure over a listing's text where it parses one.
pub fn parse_listing(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for l in text.lines() {
        let t = l.trim().trim_start_matches(['-', '*', '•', ' ']); let first = t.split_whitespace().next().unwrap_or("");
        if first.is_empty() || first.len() > 80 || first.ends_with(':') || !first.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:/[]=,".contains(c)) || !(first.chars().any(|c| c.is_ascii_digit() || c == '-') || t[first.len()..].starts_with(" - ") || t[first.len()..].starts_with('\t')) { continue; }
        if !out.iter().any(|x| x == first) { out.push(first.to_string()); }
    }
    out
}
fn listing(bin: &str, args: &[&str]) -> Vec<String> {
    static CACHE: OnceLock<Mutex<HashMap<String, (i64, Vec<String>)>>> = OnceLock::new();
    let key = format!("{bin} {}", args.join(" ")); let now = crate::cli::now_ms();
    if let Some((at, v)) = CACHE.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner()).get(&key) { if now - at < 10 * 60_000 { return v.clone(); } }
    // A CLI's own listing may ask the network or wait on a login: it gets a few seconds, then the list is what the router knows (an empty answer is cached for a short while, not ten minutes)
    let (b, a) = (bin.to_string(), args.iter().map(|x| x.to_string()).collect::<Vec<_>>()); let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || { let _ = tx.send(antiloki_rs::proc::cmd(&b).args(&a).stdin(std::process::Stdio::null()).output().ok().map(|o| parse_listing(&String::from_utf8_lossy(&o.stdout)))); });
    let v = rx.recv_timeout(std::time::Duration::from_secs(6)).ok().flatten().unwrap_or_default();
    if v.is_empty() { CACHE.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner()).insert(key, (now - 9 * 60_000 + 30_000, vec![])); return v; }
    CACHE.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner()).insert(key, (now, v.clone())); v
}
pub fn models_for(engine_key: &str) -> Vec<Value> {
    let mut ids: Vec<String> = match engine_key {
        "claude-code" => vec!["haiku".into(), "sonnet".into(), "opus".into()],
        _ => vec![],
    };
    // the models on this machine's own server, for Claude Code (it runs on them through the Messages API): `local:<tag>`
    let locals: Vec<Value> = if engine_key == "claude-code" { crate::local_panes::models(&crate::local_panes::base()).unwrap_or_default() } else { vec![] };
    let extra: Vec<String> = match engine_key {
        "cursor" => listing(&crate::engines::ENGINES.iter().find(|e| e.0 == "cursor").map(|e| e.2).unwrap_or("cursor-agent"), &["--list-models"]),
        "gemini" => listing("agy", &["models"]),
        "grok" => listing("grok", &["models"]),
        "opencode" | "opencode-direct" => listing("opencode", &["models"]),
        _ => vec![],
    };
    ids.extend(extra);
    if let Some(e) = crate::cli::engine_of(&json!(engine_key)) { ids.extend(crate::cli::ladder(e)); }
    let mut seen = std::collections::HashSet::new(); ids.retain(|i| seen.insert(i.clone()));
    let mut out: Vec<Value> = ids.into_iter().take(60).map(|id| json!({ "id": id, "label": id })).collect(); out.extend(locals.into_iter().map(|m| json!({ "id": m["id"], "label": m["label"], "local": true, "size_gb": m["size_gb"], "unavailable": m["unavailable"] }))); out
}

async fn pane_of(state: &Arc<AppState>, id: i64) -> Option<Value> {
    engine_call(state, "system.cockpit.pane.get", json!({ "id": id }), json!({ "actor": "web-fe", "source": "web-fe" })).await.ok().map(|(v, _)| v).filter(|v| !v.is_null())
}

/// `GET /api/v1/cockpit/pane/models?pane_id=N`
pub async fn models_get(state: Arc<AppState>, q: HashMap<String, String>) -> Response {
    let id: i64 = q.get("pane_id").and_then(|s| s.parse().ok()).unwrap_or(0);
    let Some(p) = pane_of(&state, id).await else { return reply(json!({ "ok": false, "error": "no such pane" })) };
    let engine = p["pane_kind"].as_str().unwrap_or("").to_string();
    let e2 = engine.clone(); let models = tokio::task::spawn_blocking(move || models_for(&e2)).await.unwrap_or_default();
    let settings: Value = serde_json::from_str(p["settings_json"].as_str().unwrap_or("{}")).unwrap_or(Value::Null);
    reply(json!({ "ok": true, "engine": engine, "current": model_of(p["settings_json"].as_str().unwrap_or("")), "models": models, "live": engine == "claude-code", "heimdall": crate::cowork::heimdall_wanted(&engine, &settings), "heimdall_default": crate::cowork::heimdall_wanted(&engine, &Value::Null) }))
}

/// `POST /api/v1/cockpit/pane/heimdall {pane_id, on: true|false|null}`: whether Heimdall screens this agent's asks. `null` goes back to what the provider's kind gets by default (frontier providers: no; open and local models: yes).
pub async fn heimdall_post(state: Arc<AppState>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); let id = b["pane_id"].as_i64().unwrap_or(0);
    let Some(p) = pane_of(&state, id).await else { return reply(json!({ "ok": false, "error": "no such pane" })) };
    let mut settings: Value = serde_json::from_str(p["settings_json"].as_str().unwrap_or("{}")).unwrap_or(json!({})); if !settings.is_object() { settings = json!({}); }
    match b["on"].as_bool() { Some(on) => { settings["heimdall"] = json!(on); } None => { settings.as_object_mut().unwrap().remove("heimdall"); } }
    if let Err(e) = engine_call(&state, "system.cockpit.pane.update", json!({ "pane_id": id, "settings": settings }), json!({ "actor": "web-fe", "source": "web-fe" })).await { return reply(json!({ "ok": false, "error": e })); }
    let engine = p["pane_kind"].as_str().unwrap_or("");
    reply(json!({ "ok": true, "heimdall": crate::cowork::heimdall_wanted(engine, &settings), "heimdall_default": crate::cowork::heimdall_wanted(engine, &Value::Null) }))
}

/// THE SESSION HOLDS 20 OPEN PANES, AND A PANE CLOSED ON THE SCREEN WAS NEVER MARKED CLOSED HERE: its terminal was killed and it left the layout, but its row stayed `idle` and kept its slot, so after twenty agents had ever been opened in the Cowork session
/// the next one was refused ("pane cap reached") with the screen saying only "could not make a real box". Found by the browser suite filling its session.
/// `POST /api/v1/cockpit/pane/release {pane_id}` marks that one pane closed (its box and episodes are untouched). With no `pane_id` it sweeps: every open agent pane whose terminal is not running and that no thread's screen shows is closed.
pub async fn release_post(state: Arc<AppState>, reg: Arc<PtyRegistry>, goals: Arc<crate::goals::Goals>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null); let ctx = json!({ "actor": "web-fe", "source": "web-fe" });
    let close = |id: i64| { let st = state.clone(); let ctx = ctx.clone(); async move { engine_call(&st, "system.cockpit.pane.update", json!({ "pane_id": id, "status": "closed" }), ctx).await.is_ok() } };
    if let Some(id) = b["pane_id"].as_i64().filter(|i| *i > 0) {
        // WHAT AN AGENT DID IS SAVED WHEN ITS PANE CLOSES (operator 2026-11-26): a specialist's conversation into its memory, and the work into its saga. A close is never held up for long by it.
        let (st, g, ctx2) = (state.clone(), goals.clone(), ctx.clone());
        let save = async move {
            let is_agent = engine_call(&st, "system.cockpit.pane.get", json!({ "id": id }), ctx2).await.ok().is_some_and(|(p, _)| crate::cli::engine_of(&p["pane_kind"]).is_some());
            if !is_agent { return; }
            let _ = crate::cowork::transcript_value(&st, id, 400).await;
            let root = crate::cowork::pane_facts(&st, id).await.0;
            let _ = tokio::task::spawn_blocking(move || crate::work::consolidate(&st, &g, &root, id)).await;
        };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(20), tokio::spawn(save)).await;
        tokio::spawn(crate::specialists::distill_pane(state.clone(), id));   // what a specialist learned, written after the close (never holding it up)
        return reply(json!({ "ok": close(id).await, "released": [id] }));
    }
    // the sweep
    let root = crate::agents_http::active_root(&state).await; let r2 = root.clone();
    let shown: std::collections::HashSet<i64> = tokio::task::spawn_blocking(move || crate::cowork_threads::view(&r2)["threads"].as_array().cloned().unwrap_or_default().iter().flat_map(|t| t["panes"].as_array().cloned().unwrap_or_default()).filter_map(|p| p["paneId"].as_i64()).collect()).await.unwrap_or_default();
    let list = engine_call(&state, "system.cockpit.session.get", json!({}), ctx.clone()).await.ok().map(|(v, _)| v).unwrap_or(Value::Null);
    let mut released = vec![];
    for sess in list["sessions"].as_array().cloned().unwrap_or_default() {
        if sess["name"] != json!("Cowork") || sess["status"] == json!("closed") { continue; }
        let one = engine_call(&state, "system.cockpit.session.get", json!({ "id": sess["id"] }), ctx.clone()).await.ok().map(|(v, _)| v).unwrap_or(Value::Null);
        for p in one["panes"].as_array().cloned().unwrap_or_default() {
            let id = p["id"].as_i64().unwrap_or(0); let kind = p["pane_kind"].as_str().unwrap_or("");
            if id <= 0 || p["status"] == json!("closed") || crate::cli::engine_of(&json!(kind)).is_none() || shown.contains(&id) || reg.status(id)["running"] == json!(true) { continue; }
            if close(id).await { released.push(id); }
        }
    }
    reply(json!({ "ok": true, "released": released }))
}

/// Did Claude Code confirm a `/model` switch: it prints "Set model to <name>". Pure.
pub fn switched(output: &str) -> bool { output.contains("Set model to") }

/// `POST /api/v1/cockpit/pane/model {pane_id, model}`
pub async fn model_post(state: Arc<AppState>, reg: Arc<PtyRegistry>, body: Bytes) -> Response {
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let id = b["pane_id"].as_i64().unwrap_or(0);
    let model = match clean_model(b["model"].as_str().unwrap_or("")) { Ok(m) => m, Err(e) => return reply(json!({ "ok": false, "error": e })) };
    let Some(p) = pane_of(&state, id).await else { return reply(json!({ "ok": false, "error": "no such pane" })) };
    let engine = p["pane_kind"].as_str().unwrap_or("").to_string();
    if crate::cli::engine_of(&json!(engine)).is_none() && engine != "opencode-direct" { return reply(json!({ "ok": false, "error": "this pane is not an agent with a model to choose" })); }
    // remembered on the pane (a restart, or a new serve, starts with it)
    let mut settings: Value = serde_json::from_str(p["settings_json"].as_str().unwrap_or("{}")).unwrap_or(json!({})); if !settings.is_object() { settings = json!({}); }
    if model.is_empty() { settings.as_object_mut().unwrap().remove("model"); } else { settings["model"] = json!(model); }
    if let Err(e) = engine_call(&state, "system.cockpit.pane.update", json!({ "pane_id": id, "settings": settings }), json!({ "actor": "web-fe", "source": "web-fe" })).await { return reply(json!({ "ok": false, "error": e })); }
    let running = reg.status(id)["running"] == json!(true);
    if !running { return reply(json!({ "ok": true, "model": model, "applied": "next start" })); }
    // a change that crosses the local line (to a local model, from one, or between two) is a new start: the server the CLI talks to is in its environment
    // THE MODEL THE PANE LEFT STOPS HOLDING MEMORY (2026-11-22: an old local model kept a CPU busy): unloaded unless another pane still names it
    if let Some(old) = crate::local_panes::tag_of(&model_of(p["settings_json"].as_str().unwrap_or(""))).map(String::from) { if crate::local_panes::tag_of(&model) != Some(old.as_str()) { tokio::task::spawn_blocking(move || crate::local_panes::unload(&crate::local_panes::base(), &old)); } }
    let was_local = crate::local_panes::tag_of(&model_of(p["settings_json"].as_str().unwrap_or(""))).is_some(); let crosses = crate::local_panes::tag_of(&model).is_some() || was_local;
    if engine == "claude-code" && !model.is_empty() && !crosses {
        // THE SWITCH IS CHECKED, NOT ASSUMED (operator 2026-11-23: "it changes on the antiloki model submenu, but doesn't change on the actual model"): typing `/model X` only proves the keys were sent. Claude answers a switch that worked with
        // "Set model to ...", so that line is looked for in the output that follows. If it does not come (the agent is busy, a menu has the keyboard, a version that words it differently), the session is restarted on the new model - its
        // conversation resumes - so what the button says and what the agent runs are the same.
        let (_, _, before) = crate::pty::backlog_tail(&state, id, 0).await;
        let r = crate::pty::pty_send(reg.clone(), Bytes::from(json!({ "pane_id": id, "text": format!("/model {model}") }).to_string())).await;
        let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap_or_default(); let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if v["ok"] != json!(true) { return reply(json!({ "ok": true, "model": model, "applied": "restart", "restart": true, "note": "could not type into the agent, so it is restarted on the new model" })); }
        for _ in 0..16 {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let (_, rows, _) = crate::pty::backlog_tail(&state, id, before).await;
            let text = crate::pty::spaced_text(&rows.iter().map(|r| r.1.as_str()).collect::<String>());
            if switched(&text) { return reply(json!({ "ok": true, "model": model, "applied": "live" })); }
        }
        return reply(json!({ "ok": true, "model": model, "applied": "restart", "restart": true, "note": "the agent did not confirm the change, so it is restarted on the new model" }));
    }
    reply(json!({ "ok": true, "model": model, "applied": "restart", "restart": true }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_model_switch_counts_only_when_claude_says_it_set_the_model() {
        assert!(switched("\u{1b}[1m> /model opus\n  \u{23bf}  Set model to Opus 4.1 (default)"));
        assert!(!switched("> /model opus\n"), "the keys typed are not a switch");
        assert!(!switched("Select model\n 1. Default (recommended)\n 2. Opus"), "a menu open is not a switch");
    }

    use super::*;
    #[test]
    fn each_engine_gets_its_own_flag_for_the_model_and_an_empty_model_adds_nothing() {
        assert_eq!(launch_args("claude-code", "haiku"), vec!["--model", "haiku"]); assert_eq!(launch_args("codex", "gpt-5.6"), vec!["-m", "gpt-5.6"]); assert_eq!(launch_args("opencode", "deepseek/v4"), vec!["-m", "deepseek/v4"]);
        assert_eq!(launch_args("cursor", "sonnet-4-thinking"), vec!["--model", "sonnet-4-thinking"]); assert_eq!(launch_args("gemini", "x"), vec!["--model", "x"]); assert_eq!(launch_args("grok", "g"), vec!["-m", "g"]);
        assert_eq!(launch_args("claude-code", "local:qwen3:4b"), vec!["--model", "qwen3:4b"], "a local model: the CLI gets its tag"); assert!(launch_args("codex", "local:x") == vec!["-m", "local:x"], "only Claude Code runs on the local server");
        assert!(launch_args("claude-code", "  ").is_empty() && launch_args("deepseek", "x").is_empty(), "no model, or an engine with no flag");
    }
    #[test]
    fn a_model_name_is_checked_before_it_is_typed_into_a_terminal() {
        for ok in ["haiku", "claude-sonnet-5-5", "gpt-5.6-terra", "deepseek/deepseek-v4-pro", "composer-2.5", "gpt-5[effort=high]", ""] { assert!(clean_model(ok).is_ok(), "{ok}"); }
        for bad in ["haiku; rm -rf /", "a b", "x\ny", "$(id)", "`id`", "a|b"] { assert!(clean_model(bad).is_err(), "{bad}"); }
        assert!(clean_model(&"a".repeat(81)).is_err());
    }
    #[test]
    fn a_models_listing_is_read_for_the_ids_and_the_chatter_around_them_is_left_out() {
        let t = "Available models:\n  composer-2.5   Composer 2.5\n- gpt-5.6-terra (default)\nopencode/big-pickle\n\nTip: use --model\n  sonnet-4-thinking\n";
        assert_eq!(parse_listing(t), vec!["composer-2.5", "gpt-5.6-terra", "opencode/big-pickle", "sonnet-4-thinking"]);
        // the real listings: a bare id counts when a description follows it after a dash or a tab, and the sentences around the ids do not
        assert_eq!(parse_listing("Available models\nauto - Auto (default)\ngpt-5.3-codex-low - Codex 5.3 Low\n"), vec!["auto", "gpt-5.3-codex-low"]);
        assert_eq!(parse_listing("Fetching available models...\ngemini-3.8-flash-high\tGemini 3.8 Flash (High)\n"), vec!["gemini-3.8-flash-high"]);
        assert!(parse_listing("").is_empty());
    }
    #[test]
    fn claude_offers_its_three_aliases_first_and_the_model_of_a_pane_is_read_from_its_settings() {
        let m = models_for("claude-code"); assert_eq!(m.iter().take(3).map(|x| x["id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["haiku", "sonnet", "opus"]);
        let ids: Vec<&str> = m.iter().map(|x| x["id"].as_str().unwrap()).collect(); let mut u = ids.clone(); u.sort(); u.dedup(); assert_eq!(u.len(), ids.len(), "no model twice");
        assert_eq!(model_of(r#"{"title":"x","model":" haiku "}"#), "haiku"); assert_eq!(model_of("{}"), ""); assert_eq!(model_of("nope"), "");
    }
}
