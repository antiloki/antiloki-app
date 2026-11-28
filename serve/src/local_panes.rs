//! LOCAL MODELS AS AGENTS (the operator 2026-11-22: "open a pane on a local model, create a specialist powered by a local model, switch the provider of a specialist to a local model: full local model integration").
//! A local model is not a new kind of agent: it is Claude Code with a different brain. Ollama speaks Anthropic's Messages API (`/v1/messages`, checked live on 0.33.3), so the pane runs the same `claude`
//! (its tools, its MCP servers, the antiloki gate, the specialist's brief) with `ANTHROPIC_BASE_URL` pointing at the local server and the model set to the local tag. A model named `local:<tag>` is a local one.
//! Nothing here calls a model: it lists what the server has, names the environment a pane starts with, and says whether a model id is a local one.
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

/// THE ARCHITECTURES A SERVER NEEDS A NEWER VERSION FOR (found 2026-11-22: `lfm3` models were listed and every start died with "unknown model architecture", because the installed Ollama 0.33.3 and then 0.40.7 predate it; 0.40.13 loads it). The picker marks such a model instead of offering one that cannot start. Add a row when another one is found.
pub const NEEDS_VERSION: [(&str, &str); 1] = [("lfm3", "0.40.13")];
/// Is `have` older than `need` (dotted numbers)? Pure.
pub fn older_than(have: &str, need: &str) -> bool { let v = |s: &str| s.trim().trim_start_matches('v').split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>(); let (a, b) = (v(have), v(need)); for i in 0..a.len().max(b.len()) { let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0)); if x != y { return x < y; } } false }
/// The note on a model the running server is too old for, or None. Pure.
pub fn needs_note(family: &str, server_version: &str) -> Option<String> { NEEDS_VERSION.iter().find(|(f, _)| *f == family).filter(|(_, need)| !server_version.is_empty() && older_than(server_version, need)).map(|(f, need)| format!("needs Ollama {need} or newer (the `{f}` architecture); this server is {server_version}")) }
/// Ask the server to drop a model from memory now (`keep_alive: 0`), so a pane that left it does not leave it holding RAM for minutes. Best effort, blocking.
pub fn unload(base: &str, tag: &str) { let _ = ureq::post(&format!("{}/api/generate", base.trim_end_matches('/'))).timeout(Duration::from_secs(5)).send_json(json!({ "model": tag, "keep_alive": 0 })); }
/// `local:qwen3:4b` -> `qwen3:4b`; None for any other model id (and for an empty tag). Pure.
pub fn tag_of(model: &str) -> Option<&str> { model.trim().strip_prefix("local:").map(str::trim).filter(|t| !t.is_empty()) }
/// The model server: `ANTILOKI_LOCAL_URL`, else Ollama's own address.
pub fn base() -> String { crate::local::Config::from_env().base }
/// The environment a Claude Code pane starts with to run on the local model `tag`: the server, a placeholder token (Ollama asks for none, Claude Code insists on one), every model slot of Claude Code
/// pointed at the tag (it asks a small model for titles and the like: all of them are the local one), and no calls home. Pure.
pub fn env_for(base: &str, tag: &str) -> HashMap<String, String> {
    let mut e = HashMap::new(); let base = base.trim_end_matches('/').to_string();
    e.insert("ANTHROPIC_BASE_URL".into(), base); e.insert("ANTHROPIC_AUTH_TOKEN".into(), "ollama".into()); e.insert("ANTHROPIC_API_KEY".into(), String::new());
    for k in ["ANTHROPIC_MODEL", "ANTHROPIC_SMALL_FAST_MODEL", "ANTHROPIC_DEFAULT_HAIKU_MODEL", "ANTHROPIC_DEFAULT_SONNET_MODEL", "ANTHROPIC_DEFAULT_OPUS_MODEL"] { e.insert(k.into(), tag.to_string()); }
    e.insert("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(), "1".into()); e
}
/// The models the server holds that can chat (an embedding model cannot), as `{id: "local:<tag>", tag, label, size_gb, params}`. Blocking: one short request.
pub fn models(base: &str) -> Result<Vec<Value>, String> {
    let r = ureq::get(&format!("{}/api/tags", base.trim_end_matches('/'))).timeout(Duration::from_secs(3)).call().map_err(|e| format!("the model server at {base} did not answer ({e})"))?;
    let v: Value = r.into_json().map_err(|e| e.to_string())?; let mut m = parse_tags(&v);
    // what the running server is, so a model it cannot load is marked
    let ver = ureq::get(&format!("{}/api/version", base.trim_end_matches('/'))).timeout(Duration::from_secs(2)).call().ok().and_then(|r| r.into_json::<Value>().ok()).and_then(|j| j["version"].as_str().map(String::from)).unwrap_or_default();
    for x in m.iter_mut() { if let Some(n) = needs_note(x["family"].as_str().unwrap_or(""), &ver) { x["unavailable"] = json!(n); } } Ok(m)
}
/// THE CONTEXT A PANE'S LOCAL MODEL GETS (2026-11-23). Claude Code's own prompt and tool list are many thousands of tokens, and Ollama's default context under 24 GB of VRAM is 4 096: the start of the prompt (the instructions) is silently cut and a small model
/// looks confused. The Anthropic-compatible endpoint takes no context option, so the pane runs on a copy of the model with the context set (`PARAMETER num_ctx`): a few kilobytes that point at the same weights.
pub const PANE_CTX: u32 = 16_384;
fn pane_ctx() -> u32 { std::env::var("ANTILOKI_LOCAL_PANE_CTX").ok().and_then(|v| v.trim().parse::<u32>().ok()).filter(|n| (2048..=262_144).contains(n)).unwrap_or(PANE_CTX) }
/// The copy's name: `qwen3:4b` -> `antiloki-qwen3-4b-16k`. Pure.
pub fn context_name(tag: &str, ctx: u32) -> String { format!("antiloki-{}-{}k", tag.trim().replace([':', '/', ' '], "-"), ctx / 1024) }
/// A model made by `context_name`: not offered in the menu (it is the same model).
pub fn is_context_copy(tag: &str) -> bool { tag.starts_with("antiloki-") && tag.ends_with('k') }
fn made() -> &'static std::sync::Mutex<std::collections::HashSet<String>> { static M: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new(); M.get_or_init(Default::default) }
/// The model name a pane should start with: the copy with the context set, made now if it is not there; the plain tag if it cannot be made (the pane still opens, only with the server's default context). Blocking; never fails.
pub fn context_tag(tag: &str) -> String {
    if is_context_copy(tag) { return tag.to_string(); }
    let ctx = pane_ctx(); let name = context_name(tag, ctx);
    if made().lock().map(|g| g.contains(&name)).unwrap_or(false) { return name; }
    let base = base(); let b = base.trim_end_matches('/').to_string();
    let have = ureq::get(&format!("{b}/api/tags")).timeout(Duration::from_secs(3)).call().ok().and_then(|r| r.into_json::<Value>().ok()).map(|v| v["models"].as_array().into_iter().flatten().any(|m| m["name"].as_str().map(|n| n == name || n == format!("{name}:latest")).unwrap_or(false))).unwrap_or(false);
    let ok = have || ureq::post(&format!("{b}/api/create")).timeout(Duration::from_secs(90)).send_json(json!({ "model": name, "from": tag, "parameters": { "num_ctx": ctx }, "stream": false })).is_ok();
    if ok { if let Ok(mut g) = made().lock() { g.insert(name.clone()); } name } else { tag.to_string() }
}
/// `/api/ps` (the models the server has loaded now): for each, how much of it sits in VRAM: `{name, gpu_pct}`; 100 is all on the GPU, 0 is all on the CPU. Pure.
pub fn parse_loaded(v: &Value) -> Vec<Value> {
    v["models"].as_array().into_iter().flatten().filter_map(|m| { let name = m["name"].as_str()?; let size = m["size"].as_f64().unwrap_or(0.0); let vr = m["size_vram"].as_f64().unwrap_or(0.0);
        let pct = if size > 0.0 { (vr / size * 100.0).round().clamp(0.0, 100.0) } else { 0.0 }; Some(json!({ "name": name, "gpu_pct": pct, "context": m["context_length"].as_u64() })) }).collect()
}
/// The models loaded now, each with its GPU share. Empty when nothing is loaded or the server does not say. Blocking, short.
pub fn loaded(base: &str) -> Vec<Value> { ureq::get(&format!("{}/api/ps", base.trim_end_matches('/'))).timeout(Duration::from_secs(2)).call().ok().and_then(|r| r.into_json::<Value>().ok()).map(|v| parse_loaded(&v)).unwrap_or_default() }
/// `/api/tags` as pane model entries, smallest first. Pure.
pub fn parse_tags(v: &Value) -> Vec<Value> {
    let mut out: Vec<Value> = v["models"].as_array().into_iter().flatten().filter_map(|m| { let name = m["name"].as_str()?; if name.contains("embed") || is_context_copy(name.trim_end_matches(":latest")) { return None; } let gb = (m["size"].as_f64().unwrap_or(0.0) / 1e8).round() / 10.0;
        Some(json!({ "id": format!("local:{name}"), "tag": name, "label": format!("local · {name}"), "size_gb": gb, "params": m["details"]["parameter_size"].as_str().unwrap_or(""), "family": m["details"]["family"].as_str().unwrap_or("") })) }).collect();
    out.sort_by(|a, b| a["size_gb"].as_f64().partial_cmp(&b["size_gb"].as_f64()).unwrap_or(std::cmp::Ordering::Equal)); out
}
/// `GET /api/v1/local/models`: is the model server there, and what does it hold.
pub async fn models_get() -> Response {
    let b = base(); let b2 = b.clone(); let r = tokio::task::spawn_blocking(move || models(&b2)).await.unwrap_or_else(|_| Err("cut off".into()));
    let loaded = if r.is_ok() { let b3 = b.clone(); tokio::task::spawn_blocking(move || loaded(&b3)).await.unwrap_or_default() } else { vec![] };
    let gpu_mb = crate::machine::vram_total_mb();
    match r { Ok(m) => (StatusCode::OK, Json(json!({ "ok": true, "reachable": true, "base": b, "models": m, "gpu_mb": gpu_mb, "loaded": loaded }))).into_response(), Err(e) => (StatusCode::OK, Json(json!({ "ok": true, "reachable": false, "base": b, "models": [], "error": e }))).into_response() }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_context_copy_has_a_stable_name_is_not_offered_and_loaded_models_say_how_much_is_on_the_gpu() {
        assert_eq!(context_name("qwen3:4b", 16_384), "antiloki-qwen3-4b-16k"); assert_eq!(context_name("library/x:7b", 32_768), "antiloki-library-x-7b-32k");
        assert!(is_context_copy("antiloki-qwen3-4b-16k")); assert!(!is_context_copy("qwen3:4b")); assert!(!is_context_copy("anticut-lfm3-1.7b"));
        let tags = json!({ "models": [{ "name": "qwen3:4b", "size": 2.5e9 }, { "name": "antiloki-qwen3-4b-16k:latest", "size": 2.5e9 }] });
        let listed: Vec<String> = parse_tags(&tags).iter().map(|m| m["tag"].as_str().unwrap().to_string()).collect(); assert_eq!(listed, vec!["qwen3:4b"], "the copy is the same model: one entry");
        let ps = json!({ "models": [{ "name": "a", "size": 4.0e9, "size_vram": 4.0e9, "context_length": 16384 }, { "name": "b", "size": 4.0e9, "size_vram": 0 }, { "name": "c", "size": 8.0e9, "size_vram": 6.0e9 }] });
        let l = parse_loaded(&ps); assert_eq!(l[0]["gpu_pct"], json!(100.0)); assert_eq!(l[1]["gpu_pct"], json!(0.0)); assert_eq!(l[2]["gpu_pct"], json!(75.0)); assert_eq!(l[0]["context"], json!(16384));
    }

    use super::*;
    #[test] fn a_local_model_is_named_local_colon_tag_and_its_pane_gets_the_servers_environment() {
        assert_eq!(tag_of("local:qwen3:4b"), Some("qwen3:4b")); assert_eq!(tag_of("  local: llama3 "), Some("llama3")); assert_eq!(tag_of("haiku"), None); assert_eq!(tag_of("local:"), None); assert_eq!(tag_of(""), None);
        let e = env_for("http://127.0.0.1:11434/", "qwen3:4b"); assert_eq!(e["ANTHROPIC_BASE_URL"], "http://127.0.0.1:11434"); assert_eq!(e["ANTHROPIC_AUTH_TOKEN"], "ollama"); assert_eq!(e["ANTHROPIC_API_KEY"], ""); for k in ["ANTHROPIC_MODEL", "ANTHROPIC_SMALL_FAST_MODEL", "ANTHROPIC_DEFAULT_HAIKU_MODEL", "ANTHROPIC_DEFAULT_SONNET_MODEL", "ANTHROPIC_DEFAULT_OPUS_MODEL"] { assert_eq!(e[k], "qwen3:4b", "{k}"); } assert_eq!(e["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"], "1");
    }
    #[test] fn the_servers_listing_becomes_pane_models_smallest_first_without_the_embedding_ones() {
        let v = json!({ "models": [{ "name": "qwen3:4b", "size": 2_500_000_000u64, "details": { "parameter_size": "4.0B" } }, { "name": "nomic-embed-text:latest", "size": 300_000_000u64, "details": {} }, { "name": "qwen3:0.6b", "size": 500_000_000u64, "details": { "parameter_size": "751M" } }] });
        let m = parse_tags(&v); assert_eq!(m.iter().map(|x| x["id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["local:qwen3:0.6b", "local:qwen3:4b"]); assert_eq!(m[0]["label"], json!("local · qwen3:0.6b")); assert_eq!(m[1]["size_gb"], json!(2.5)); assert!(parse_tags(&json!({})).is_empty());
    }
    #[test] fn a_model_the_server_is_too_old_for_is_marked_not_offered() {
        assert!(older_than("0.40.7", "0.40.13") && older_than("0.33.3", "0.40.13") && !older_than("0.40.13", "0.40.13") && !older_than("0.41.0", "0.40.13") && !older_than("v1.0", "0.40.13"));
        assert!(needs_note("lfm3", "0.40.7").unwrap().contains("0.40.13")); assert!(needs_note("lfm3", "0.40.13").is_none() && needs_note("qwen3", "0.1").is_none() && needs_note("lfm3", "").is_none());
    }
    #[test] fn a_server_that_is_not_there_is_an_answer_not_a_crash() { assert!(models("http://127.0.0.1:9").unwrap_err().contains("did not answer")); }
}
