//! THE MODEL NODE, `llm` (RFC 0010 §4.4, §4.5; slice 1, built from what the stage benchmark of Appendix E found). One call, no tools, no files, no conversation: a prompt goes to a seat and a TYPED
//! answer comes back. What makes a small local model safe to give a step is not the model, it is what stands around it, and that is built in: the answer is validated against a schema (one re-ask
//! with the error), checked for free (`verify`: every named field must literally appear in the source text), cross-checked (`agree_with`: another seat must give the same answer), and any failure climbs
//! a ladder (`fallback`) to a stronger seat, never down. The pure parts (the schema, the lenient JSON, the template, the checks, the seat grammar) are here and tested; the calls are at the bottom.
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

pub const MAX_FIELDS: usize = 12;

// ── seats ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

/// Where a call goes. `endpoint:<alias>[:model]` (an OpenAI-compatible server named in `~/.antiloki/seats.json`), `local:<ollama tag>` (the Ollama seat), `claude-code:<model>` (the CLI, headless).
#[derive(Clone, Debug, PartialEq)]
pub enum Seat { Endpoint { alias: String, model: Option<String> }, Local { tag: String }, Claude { model: String } }
pub fn parse_seat(s: &str) -> Result<Seat, String> {
    let s = s.trim();
    if let Some(r) = s.strip_prefix("endpoint:") { let (a, m) = match r.split_once(':') { Some((a, m)) => (a, Some(m.to_string())), None => (r, None) }; if a.is_empty() || !a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return Err(format!("`{s}`: an endpoint has a plain alias (endpoint:lfm3)")); } return Ok(Seat::Endpoint { alias: a.into(), model: m }); }
    if let Some(r) = s.strip_prefix("local:") { if r.trim().is_empty() { return Err(format!("`{s}`: say the model (local:qwen3:4b)")); } return Ok(Seat::Local { tag: r.trim().into() }); }
    if let Some(r) = s.strip_prefix("claude-code:") { if r.trim().is_empty() || !r.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_')) { return Err(format!("`{s}`: say the model (claude-code:haiku)")); } return Ok(Seat::Claude { model: r.trim().into() }); }
    Err(format!("`{s}` is not a seat for a model node: endpoint:<alias>, local:<ollama tag> or claude-code:<model> (the other CLI seats are not wired for single calls yet)"))
}
/// One line a person can read: which seat is a frontier model. A node may ladder UP only (the ladder never moves down).
/// The seat as the grammar spells it.
/// Embeddings of several texts by a local embedding model (Ollama `/api/embed`): tiny, fast, deterministic. Used by the `match` cell to CHOOSE by similarity instead of asking a model to generate a label.
pub async fn embed(tag: &str, texts: &[String], timeout_s: u64) -> Result<Vec<Vec<f64>>, String> {
    let cfg = crate::local::Config::from_env();
    let r = client(timeout_s).post(format!("{}/api/embed", cfg.base)).json(&json!({ "model": tag, "input": texts })).send().await.map_err(|e| format!("local {tag}: {}{e}", if e.is_timeout() { "TIMED OUT: " } else { "" }))?;
    if !r.status().is_success() { return Err(format!("local {tag} answered {} (is the embedding model pulled? `ollama pull {tag}`)", r.status())); }
    let j: Value = r.json().await.map_err(|e| e.to_string())?;
    let v: Vec<Vec<f64>> = j["embeddings"].as_array().map(|a| a.iter().map(|e| e.as_array().map(|x| x.iter().filter_map(Value::as_f64).collect()).unwrap_or_default()).collect()).unwrap_or_default();
    if v.len() != texts.len() || v.iter().any(Vec::is_empty) { return Err(format!("local {tag} returned {} embeddings for {} texts", v.len(), texts.len())); }
    Ok(v)
}
pub fn seat_key(s: &Seat) -> String { match s { Seat::Endpoint { alias, model: Some(m) } => format!("endpoint:{alias}:{m}"), Seat::Endpoint { alias, model: None } => format!("endpoint:{alias}"), Seat::Local { tag } => format!("local:{tag}"), Seat::Claude { model } => format!("claude-code:{model}") } }
pub fn frontier(s: &Seat) -> bool { matches!(s, Seat::Claude { .. }) }

// ── the answer: a small schema ───────────────────────────────────────────────────────────────────────────────────────────────

/// `{"kind": {"enum": ["research","write"]}, "n": "number", "ok": "bool", "why": "string?", "tags": ["string"]}`; a trailing `?` makes a field optional.
pub fn check_schema(schema: &Map<String, Value>) -> Vec<String> {
    let mut out = vec![]; if schema.is_empty() { out.push("say the typed answer (`answer`: at least one field)".into()); } if schema.len() > MAX_FIELDS { out.push(format!("at most {MAX_FIELDS} fields in the answer")); }
    for (k, v) in schema {
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || k.starts_with(|c: char| c.is_ascii_digit()) { out.push(format!("`{k}` is not a plain field name")); continue; }
        if ["seat_used", "escalated_from", "schema_ok", "ok", "verified", "agreed"].contains(&k.as_str()) { out.push(format!("`{k}` is a field the node writes itself")); }
        let good = match v { Value::String(t) => ["string", "string?", "number", "number?", "bool", "bool?"].contains(&t.as_str()), Value::Object(o) => o.len() == 1 && o.get("enum").and_then(Value::as_array).map_or(false, |a| !a.is_empty() && a.len() <= 40 && a.iter().all(Value::is_string)), Value::Array(a) => a.len() == 1 && matches!(a[0].as_str(), Some("string" | "number")), _ => false };
        if !good { out.push(format!("`{k}`: a type is string, number, bool (add ? for optional), {{\"enum\": [..]}} or [\"string\"] / [\"number\"]")); }
    }
    out
}
/// The first JSON object in a reply, read leniently (a fence, words around it). Pure.
pub fn extract_json(text: &str) -> Option<Value> {
    let t = text.trim(); if let Ok(v) = serde_json::from_str::<Value>(t) { if v.is_object() { return Some(v); } }
    let a = t.find('{')?; let b = t.rfind('}')?; if b <= a { return None; } serde_json::from_str::<Value>(&t[a..=b]).ok().filter(Value::is_object)
}
/// The reply against the schema: the typed fields only, or what is wrong in words (that sentence is what the one re-ask is told). Numbers given as strings ("18.25", "12,5") are accepted when they parse.
pub fn validate(schema: &Map<String, Value>, v: &Value) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();
    for (k, ty) in schema {
        let optional = ty.as_str().map_or(false, |t| t.ends_with('?')); let got = v.get(k).filter(|x| !x.is_null());
        let Some(x) = got else { if optional { continue; } return Err(format!("`{k}` is missing")); };
        let ok = match ty {
            Value::String(t) => match t.trim_end_matches('?') {
                "string" => x.as_str().map(|s| out.insert(k.clone(), json!(s.trim()))).is_some(),
                "number" => { let n = x.as_f64().or_else(|| x.as_str().and_then(|s| s.trim().replace(',', ".").parse::<f64>().ok())); n.map(|n| out.insert(k.clone(), json!(n))).is_some() }
                "bool" => x.as_bool().or_else(|| x.as_str().and_then(|s| match s.trim().to_lowercase().as_str() { "true" | "yes" | "pass" => Some(true), "false" | "no" | "fail" => Some(false), _ => None })).map(|b| out.insert(k.clone(), json!(b))).is_some(),
                _ => false },
            Value::Object(o) => { let allowed = o["enum"].as_array().cloned().unwrap_or_default(); x.as_str().map(|s| s.trim().to_lowercase()).and_then(|s| allowed.iter().filter_map(Value::as_str).find(|a| a.to_lowercase() == s).map(String::from)).map(|a| out.insert(k.clone(), json!(a))).is_some() }
            Value::Array(a) => x.as_array().map_or(false, |items| { let num = a[0] == "number"; let all: Option<Vec<Value>> = items.iter().map(|i| if num { i.as_f64().map(|n| json!(n)) } else { i.as_str().map(|s| json!(s)) }).collect(); all.map(|l| out.insert(k.clone(), Value::Array(l))).is_some() }),
            _ => false,
        };
        if !ok { return Err(format!("`{k}` is {} but must be {}", short(x), describe(ty))); }
    }
    Ok(out)
}
fn short(v: &Value) -> String { let s = v.to_string(); if s.chars().count() > 40 { format!("{}…", s.chars().take(40).collect::<String>()) } else { s } }
fn describe(ty: &Value) -> String { match ty { Value::String(t) => t.trim_end_matches('?').to_string(), Value::Object(o) => format!("one of {}", o["enum"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).unwrap_or_default()), Value::Array(a) => format!("a list of {}", a[0].as_str().unwrap_or("string")), _ => "valid".into() } }

// ── the prompt ───────────────────────────────────────────────────────────────────────────────────────────────────────────────

/// `{ask}`, `{node.field}` and `{artifact:name.ext}` filled from what the run has. An unknown name is an error naming it (never an empty hole). Pure over `get`.
pub fn fill(prompt: &str, get: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut out = String::new(); let mut rest = prompt;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]); let after = &rest[i + 1..];
        match after.find('}') { Some(j) if !after[..j].is_empty() && !after[..j].contains(['\n', '"', '{', ' ']) => { let key = &after[..j]; out.push_str(&get(key).ok_or_else(|| format!("`{{{key}}}` is nothing the run has"))?); rest = &after[j + 1..]; } _ => { out.push('{'); rest = after; } }
    }
    out.push_str(rest); Ok(out)
}
/// The names a prompt reads, for the validator: `(node ids, artifact names)`.
pub fn reads(prompt: &str) -> (Vec<String>, Vec<String>) {
    let (mut nodes, mut arts) = (vec![], vec![]); let mut rest = prompt;
    while let Some(i) = rest.find('{') { let after = &rest[i + 1..]; match after.find('}') { Some(j) if !after[..j].is_empty() && !after[..j].contains(['\n', '"', '{', ' ']) => { let k = &after[..j]; if let Some(a) = k.strip_prefix("artifact:") { arts.push(a.into()); } else if k != "ask" { if let Some((n, _)) = k.split_once('.') { nodes.push(n.into()); } } rest = &after[j + 1..]; } _ => rest = after } }
    (nodes, arts)
}

// ── the free safeguards ──────────────────────────────────────────────────────────────────────────────────────────────────────

/// `verify`: every listed answer field must LITERALLY appear in the source text (a price the model made from a weight does not). Numbers are compared as numbers (18 matches "18", "18.00"; 18.25 matches only 18.25).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Verify { #[serde(default)] pub in_source: Vec<String>, #[serde(default)] pub source: String }
pub fn verify_in_source(fields: &[String], answer: &Map<String, Value>, source: &str) -> Result<(), String> {
    let low = source.to_lowercase(); let nums: Vec<f64> = regex::Regex::new(r"\d+(?:[.,]\d+)?").ok().map(|re| re.find_iter(source).filter_map(|m| m.as_str().replace(',', ".").parse().ok()).collect()).unwrap_or_default();
    for f in fields {
        let Some(v) = answer.get(f) else { return Err(format!("`{f}` was not answered, so it cannot be found in the source")); };
        let found = match v { Value::Number(n) => n.as_f64().map_or(false, |x| nums.iter().any(|y| (x - y).abs() < 0.005)), Value::String(s) => low.contains(&s.to_lowercase()), Value::Array(a) => a.iter().all(|i| i.as_str().map_or(false, |s| low.contains(&s.to_lowercase()))), _ => true };
        if !found { return Err(format!("`{f}` ({}) does not appear in the source text", short(v))); }
    }
    Ok(())
}
/// The same answer, field for field (numbers within 0.005, text case-insensitive). Pure.
pub fn same_answer(a: &Map<String, Value>, b: &Map<String, Value>) -> bool {
    a.len() == b.len() && a.iter().all(|(k, x)| b.get(k).map_or(false, |y| match (x, y) { (Value::Number(p), Value::Number(q)) => (p.as_f64().unwrap_or(0.0) - q.as_f64().unwrap_or(1.0)).abs() < 0.005, (Value::String(p), Value::String(q)) => p.trim().eq_ignore_ascii_case(q.trim()), _ => x == y }))
}

// ── the node's spec, as the graph holds it ───────────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    pub seat: String, pub prompt: String,
    /// the typed answer; a node with a `preset` may leave it out (the preset ships the schema)
    #[serde(default)] pub answer: BTreeMap<String, Value>,
    /// seats tried, in order, when the call fails its schema twice, the source check, or the agreement; each stronger than the one before
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub fallback: Vec<String>,
    /// free checks on the answer
    #[serde(default, skip_serializing_if = "is_default_verify")] pub verify: Verify,
    /// other seats that must give the same answer (a cheap cross-check: if they differ, climb the ladder)
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub agree_with: Vec<String>,
    #[serde(default)] pub max_tokens: Option<u32>, #[serde(default)] pub timeout_s: Option<u64>,
    /// the first payoff of memory (RFC 0011 §8.4): the prompt is filled with the `k` best verified golden examples of this task class, and the call row records which
    #[serde(default, skip_serializing_if = "Option::is_none")] pub examples_from: Option<ExamplesFrom>,
    /// a ready-made kind of cell (RFC 0011 §10.1): it ships a schema and a default guard, so a small seat is never unguarded
    #[serde(default, skip_serializing_if = "Option::is_none")] pub preset: Option<PresetSpec>,
    /// the policy behind `seat: "router:<name>"` (RFC 0011 §10.2)
    #[serde(default, skip_serializing_if = "Option::is_none")] pub router: Option<crate::team_router::RouterSpec>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExamplesFrom { pub task_class: String, #[serde(default = "default_k")] pub k: u32 }
fn default_k() -> u32 { 3 }
fn is_default_verify(v: &Verify) -> bool { *v == Verify::default() }
/// What is wrong with the spec before any run, in words. Pure.
pub fn check(spec: &Spec) -> Vec<String> {
    let mut out = vec![]; let schema: Map<String, Value> = effective(spec).answer.clone().into_iter().collect();
    out.extend(check_schema(&schema));
    if spec.prompt.trim().is_empty() { out.push("say what to ask (`prompt`)".into()); }
    if !crate::team_router::is_router_seat(&spec.seat) { if let Err(e) = parse_seat(&spec.seat) { out.push(e); } }
    let mut ladder = vec![]; for f in &spec.fallback { match parse_seat(f) { Ok(s) => ladder.push(s), Err(e) => out.push(e) } }
    if let (Ok(first), Some(last)) = (parse_seat(&spec.seat), ladder.last()) { if frontier(&first) && !frontier(last) { out.push("the ladder only goes up: a frontier seat is not followed by a smaller one".into()); } }
    for a in &spec.agree_with { if let Err(e) = parse_seat(a) { out.push(e); } }
    if spec.agree_with.len() > 2 { out.push("agree_with: at most two other seats".into()); }
    for f in &spec.verify.in_source { if !spec.answer.contains_key(f) { out.push(format!("verify: `{f}` is not a field of the answer")); } }
    if !spec.verify.in_source.is_empty() && spec.verify.source.trim().is_empty() { out.push("verify.in_source needs the `source` text to look in (e.g. \"{ask}\")".into()); }
    if spec.max_tokens.map_or(false, |m| !(16..=4000).contains(&m)) { out.push("max_tokens is between 16 and 4000".into()); }
    out.extend(preset_check(spec));
    if let Some(r) = &spec.router { out.extend(crate::team_router::check(r)); }
    if crate::team_router::is_router_seat(&spec.seat) && spec.router.is_none() { out.push("router_no_floor: the seat is a router but the node has no `router` part".into()); }
    if let Some(ef) = &spec.examples_from { if ef.task_class.trim().is_empty() { out.push("examples_from: say the task class".into()); } if !(1..=8).contains(&ef.k) { out.push("examples_from: k is between 1 and 8".into()); } }
    out
}
/// The instruction added to the prompt so the reply is the typed answer only. Pure.
pub fn answer_note(spec: &Spec) -> String {
    let fields: Vec<String> = spec.answer.iter().map(|(k, v)| format!("\"{k}\": {}", match v { Value::String(t) => format!("<{}>", t.trim_end_matches('?')), Value::Object(o) => format!("{}", o["enum"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(" or ")).unwrap_or_default()), Value::Array(a) => format!("[<{}>, ...]", a[0].as_str().unwrap_or("string")), _ => "?".into() })).collect();
    format!("\nAnswer ONLY JSON, with exactly these fields: {{{}}}", fields.join(", "))
}

// ── the calls ────────────────────────────────────────────────────────────────────────────────────────────────────────────────

/// One reply and what it cost: (text, usd, milliseconds).
pub type Reply = (String, f64, u64);
fn client(secs: u64) -> reqwest::Client { reqwest::Client::builder().timeout(std::time::Duration::from_secs(secs)).connect_timeout(std::time::Duration::from_secs(3)).build().unwrap_or_default() }
/// `~/.antiloki/seats.json` (or `ANTILOKI_SEATS_FILE`): `{"endpoints": {"lfm3": {"url": "http://127.0.0.1:8091", "model": "x", "max_tokens": 200}}}`.
pub fn seats_path() -> std::path::PathBuf { std::env::var("ANTILOKI_SEATS_FILE").ok().filter(|p| !p.is_empty()).map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".antiloki/seats.json")) }
pub fn endpoint_of(alias: &str) -> Result<(String, String, Option<u32>), String> {
    let path = seats_path();
    let v: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).ok_or_else(|| format!("no seats file at {}: name the endpoint `{alias}` there (a server's url and model)", path.display()))?;
    let e = &v["endpoints"][alias]; let url = e["url"].as_str().filter(|u| u.starts_with("http://127.0.0.1") || u.starts_with("http://localhost") || u.starts_with("https://")).ok_or_else(|| format!("endpoint `{alias}` is not in {} (or its url is not local or https)", path.display()))?;
    Ok((url.trim_end_matches('/').to_string(), e["model"].as_str().unwrap_or("x").to_string(), e["max_tokens"].as_u64().map(|m| m as u32)))
}
/// One call to one seat. `temperature 0`. The local seats cost nothing in dollars; the CLI seat reports its cost.
pub async fn call(seat: &Seat, prompt: &str, max_tokens: u32, timeout_s: u64, lctx: Option<crate::ledger::Ctx>) -> Result<Reply, String> {
    let out = call_seat(seat, prompt, max_tokens, timeout_s, true).await;
    crate::ledger::record_llm(lctx.as_ref(), seat, prompt, &out);
    out
}
/// A call whose reply is held to a JSON Schema (Ollama `format`): the local seat's output cannot be invalid JSON. Other seats get the schema in the prompt only. Returns the reply text.
pub async fn call_schema(seat: &Seat, prompt: &str, schema: &Value, max_tokens: u32, timeout_s: u64, lctx: Option<crate::ledger::Ctx>) -> Result<Reply, String> {
    let out = match seat {
        Seat::Local { tag } => {
            let cfg = crate::local::Config::from_env(); let ctx = cfg.num_ctx.ok_or("the local seat needs ANTILOKI_LOCAL_NUM_CTX (a context is never defaulted)")?;
            let _slot = crate::local::slot(std::time::Duration::from_secs(timeout_s)).await.ok_or("the local model is busy")?; let start = std::time::Instant::now();
            let body = json!({ "model": tag, "messages": [{ "role": "user", "content": prompt }], "stream": false, "think": false, "format": schema, "options": { "temperature": 0, "num_ctx": ctx, "num_predict": max_tokens } });
            let r = client(timeout_s).post(format!("{}/api/chat", cfg.base)).json(&body).send().await.map_err(|e| format!("local {tag}: {}{e}", if e.is_timeout() { "TIMED OUT: " } else { "" }))?; if !r.status().is_success() { return Err(format!("local {tag} answered {}", r.status())); }
            let j: Value = r.json().await.map_err(|e| e.to_string())?; Ok((j["message"]["content"].as_str().unwrap_or("").to_string(), 0.0, start.elapsed().as_millis() as u64))
        }
        other => call_seat(other, &format!("{prompt}\n\nAnswer with ONLY a JSON object that matches this JSON Schema: {schema}"), max_tokens, timeout_s, false).await,
    };
    crate::ledger::record_llm(lctx.as_ref(), seat, prompt, &out); out
}
/// The same call for a reply that is TEXT (a plan, a file in a code block), not a typed answer: the local seat is not told to speak JSON, and the endpoint's token cap is lifted to what the caller asks (a code file is longer than a classification).
pub async fn call_plain(seat: &Seat, prompt: &str, max_tokens: u32, timeout_s: u64, lctx: Option<crate::ledger::Ctx>) -> Result<Reply, String> {
    let out = call_seat(seat, prompt, max_tokens, timeout_s, false).await;
    crate::ledger::record_llm(lctx.as_ref(), seat, prompt, &out);
    out
}
async fn call_seat(seat: &Seat, prompt: &str, max_tokens: u32, timeout_s: u64, typed: bool) -> Result<Reply, String> {
    let start = std::time::Instant::now(); let ms = |s: std::time::Instant| s.elapsed().as_millis() as u64;
    match seat {
        Seat::Endpoint { alias, model } => {
            let (url, default_model, cap) = endpoint_of(alias)?; let body = json!({ "model": model.clone().unwrap_or(default_model), "messages": [{ "role": "user", "content": prompt }], "temperature": 0, "max_tokens": if typed { cap.map_or(max_tokens, |c| c.min(max_tokens)) } else { max_tokens } });
            let r = client(timeout_s).post(format!("{url}/v1/chat/completions")).json(&body).send().await.map_err(|e| format!("endpoint {alias}: {e}"))?; if !r.status().is_success() { return Err(format!("endpoint {alias} answered {}", r.status())); }
            let j: Value = r.json().await.map_err(|e| e.to_string())?; let text = j["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string(); if text.trim().is_empty() { return Err(format!("endpoint {alias} answered nothing (a thinking model at a small token cap: turn its reasoning off)")); } Ok((text, 0.0, ms(start)))
        }
        Seat::Local { tag } => {
            let cfg = crate::local::Config::from_env(); let ctx = cfg.num_ctx.ok_or("the local seat needs ANTILOKI_LOCAL_NUM_CTX (a context is never defaulted)")?;
            let _slot = crate::local::slot(std::time::Duration::from_secs(timeout_s)).await.ok_or("the local model is busy")?;
            let body = json!({ "model": tag, "messages": [{ "role": "user", "content": prompt }], "stream": false, "think": false, "options": { "temperature": 0, "num_ctx": ctx, "num_predict": max_tokens } }); let mut body = body; if typed { body["format"] = json!("json"); }
            let r = client(timeout_s).post(format!("{}/api/chat", cfg.base)).json(&body).send().await.map_err(|e| format!("local {tag}: {}{e}", if e.is_timeout() { "TIMED OUT (the model was too slow for this limit): " } else { "" }))?; if !r.status().is_success() { return Err(format!("local {tag} answered {}", r.status())); }
            let j: Value = r.json().await.map_err(|e| e.to_string())?; Ok((j["message"]["content"].as_str().unwrap_or("").to_string(), 0.0, ms(start)))
        }
        Seat::Claude { model } => {
            let bin = crate::cli::Engine::ClaudeCode.bin().to_string(); let (m, p) = (if model.to_ascii_lowercase().contains("opus") { "sonnet".to_string() } else { model.clone() }, prompt.to_string());   // never Opus
            let out = tokio::time::timeout(std::time::Duration::from_secs(timeout_s), crate::proc::tokio_cmd(bin).args(["-p", &p, "--model", &m, "--output-format", "json", "--max-turns", "2"]).kill_on_drop(true).output()).await.map_err(|_| "claude-code did not answer in time".to_string())?.map_err(|e| e.to_string())?;
            let s = String::from_utf8_lossy(&out.stdout).to_string(); let j: Value = s.find('{').and_then(|i| serde_json::from_str(&s[i..]).ok()).ok_or_else(|| format!("claude-code gave no result: {}", String::from_utf8_lossy(&out.stderr).chars().take(160).collect::<String>()))?;
            Ok((j["result"].as_str().unwrap_or("").to_string(), j["total_cost_usd"].as_f64().unwrap_or(0.0), ms(start)))
        }
    }
}


// ── presets (RFC 0011 §10.1): data only. Each ships a schema and a default guard. ───────────────────────────────────────────

pub const PRESETS: [&str; 6] = ["classify", "extract", "judge", "summarize", "rewrite", "sentiment"];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PresetSpec {
    pub kind: String,
    /// classify: the categories (anything else is "other")
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub categories: Vec<String>,
    /// extract: an example of the JSON to produce; its fields and their types are the schema
    #[serde(default, skip_serializing_if = "Option::is_none")] pub example: Option<Value>,
    /// judge: the answer must be at least this sure (0 to 1)
    #[serde(default, skip_serializing_if = "Option::is_none")] pub threshold: Option<f64>,
}

/// How a preset's answer is held to account without a model: the schema is strict (an enum accepts nothing else), an extraction is looked up in its source, and prose has no free check, so it must name a seat to climb to.
#[derive(Clone, Debug, PartialEq)]
pub enum PresetGuard { StrictEnum(Vec<String>), InSource(Vec<String>), NeedsLadder }

pub fn preset_schema(p: &PresetSpec) -> Result<BTreeMap<String, Value>, String> {
    let mut m = BTreeMap::new();
    match p.kind.as_str() {
        "classify" => {
            if p.categories.is_empty() { return Err("classify: name the categories".into()); }
            let mut cats = p.categories.clone(); if !cats.iter().any(|c| c == "other") { cats.push("other".into()); }
            m.insert("category".into(), json!({ "enum": cats }));
        }
        "extract" => {
            let ex = p.example.as_ref().and_then(Value::as_object).ok_or("extract: give an `example` of the JSON to produce")?;
            if ex.is_empty() { return Err("extract: the example has no fields".into()); }
            for (k, v) in ex { m.insert(k.clone(), json!(match v { Value::Number(_) => "number", Value::Bool(_) => "bool", _ => "string" })); }
        }
        "judge" => { m.insert("verdict".into(), json!({ "enum": ["yes", "no"] })); m.insert("reason".into(), json!("string")); m.insert("confidence".into(), json!("number")); }
        "summarize" => { m.insert("summary".into(), json!("string")); }
        "rewrite" => { m.insert("text".into(), json!("string")); }
        "sentiment" => { m.insert("sentiment".into(), json!({ "enum": ["positive", "neutral", "negative"] })); }
        other => return Err(format!("`{other}` is not a preset; they are {}", PRESETS.join(", "))),
    }
    Ok(m)
}

pub fn preset_guard(p: &PresetSpec) -> PresetGuard {
    match p.kind.as_str() {
        "classify" => PresetGuard::StrictEnum({ let mut c = p.categories.clone(); if !c.iter().any(|x| x == "other") { c.push("other".into()); } c }),
        "judge" => PresetGuard::StrictEnum(vec!["yes".into(), "no".into()]),
        "sentiment" => PresetGuard::StrictEnum(vec!["positive".into(), "neutral".into(), "negative".into()]),
        "extract" => PresetGuard::InSource(p.example.as_ref().and_then(Value::as_object).map(|o| o.iter().filter(|(_, v)| matches!(v, Value::Number(_) | Value::String(_))).map(|(k, _)| k.clone()).collect()).unwrap_or_default()),
        _ => PresetGuard::NeedsLadder,
    }
}

/// The words a preset adds to the prompt.
pub fn preset_instruction(p: &PresetSpec) -> String {
    match p.kind.as_str() {
        "classify" => format!("\nChoose exactly one category from: {}. If none fits, answer \"other\".", { let mut c = p.categories.clone(); if !c.iter().any(|x| x == "other") { c.push("other".into()); } c.join(", ") }),
        "extract" => "\nExtract only what the text says. Do not guess a value that is not in it.".into(),
        "judge" => format!("\nAnswer yes or no with a short reason and your confidence from 0 to 1.{}", p.threshold.map(|t| format!(" Say yes only if you are at least {t} sure.")).unwrap_or_default()),
        "summarize" => "\nSummarize faithfully and briefly. Add nothing the text does not say.".into(),
        "rewrite" => "\nRewrite as asked. Keep every fact.".into(),
        "sentiment" => "\nSay whether the sentiment of the text is positive, neutral or negative.".into(),
        _ => String::new(),
    }
}

/// The spec a call actually runs: a preset fills in the schema, the prompt's instruction and its default guard where the node did not say its own.
pub fn effective(spec: &Spec) -> Spec {
    let Some(p) = &spec.preset else { return spec.clone() };
    let mut e = spec.clone();
    if let Ok(schema) = preset_schema(p) { if e.answer.is_empty() { e.answer = schema; } }
    e.prompt = format!("{}{}", e.prompt, preset_instruction(p));
    if let PresetGuard::InSource(fields) = preset_guard(p) { if e.verify.in_source.is_empty() { e.verify.in_source = fields; } if e.verify.source.trim().is_empty() { e.verify.source = "{ask}".into(); } }
    e
}

/// A preset on a small seat with nothing to hold it to account (prose, no ladder, no cross-check) is a mistake the validator names.
pub fn preset_check(spec: &Spec) -> Vec<String> {
    let Some(p) = &spec.preset else { return vec![] };
    let mut out = vec![];
    if let Err(e) = preset_schema(p) { out.push(e); return out; }
    if let PresetGuard::NeedsLadder = preset_guard(p) {
        let small = !parse_seat(&spec.seat).map_or(true, |s| frontier(&s)) && !crate::team_router::is_router_seat(&spec.seat);
        if small && spec.fallback.is_empty() && spec.agree_with.is_empty() { out.push(format!("preset_unguarded: a `{}` answer cannot be checked for free, so a small seat needs a `fallback` seat to climb to", p.kind)); }
    }
    out
}

/// Does a small seat have anything holding its answer to account? The preset's own default guard counts.
pub fn guarded(spec: &Spec) -> bool {
    !spec.fallback.is_empty() || !spec.agree_with.is_empty() || !spec.verify.in_source.is_empty()
        || spec.preset.as_ref().map_or(false, |p| matches!(preset_guard(p), PresetGuard::StrictEnum(_) | PresetGuard::InSource(_)) && preset_schema(p).is_ok())
}

// ── the router's seat for one call ──────────────────────────────────────────────────────────────────────────────────────────

/// A seat is usable by the router when it can be reached now: a CLI seat must have quota; an endpoint or a local model is assumed up (the call says if not, and the ladder climbs).
fn seat_usable(s: &str) -> bool {
    match parse_seat(s) { Ok(Seat::Claude { .. }) => crate::cli::engine_routable(crate::cli::Engine::ClaudeCode), Ok(_) => true, Err(_) => false }
}

/// Turn `router:<name>` into one real seat for this call, with its evidence. A seat that is not a router seat is returned as it is.
fn route(spec: &Spec, input_chars: usize, lctx: &mut Option<crate::ledger::Ctx>) -> Result<String, String> {
    if !crate::team_router::is_router_seat(&spec.seat) { return Ok(spec.seat.clone()); }
    let r = spec.router.as_ref().ok_or("the seat is a router but the node has no `router` part")?;
    let stats = lctx.as_ref().map(|c| crate::ledger::seat_stats(&c.root, &c.team, c.task_class.as_deref().or(c.node.as_deref()))).unwrap_or_default();
    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) ^ (crate::cli::now_ms() as u64).rotate_left(17);
    let mut rng = crate::team_router::Rng::new(seed);
    // the regulator may have told the router to leave a seat alone for a while (a circuit breaker, inside this node's own candidates)
    let blocked: Vec<String> = lctx.as_ref().and_then(|c| c.node.as_ref().map(|n| crate::team_regulate::blocked_seats(&c.root, &c.team, n, crate::cli::now_ms()))).unwrap_or_default();
    let usable = |s: &str| seat_usable(s) && !blocked.iter().any(|b| b == s);
    let c = crate::team_router::choose(r, &stats, &usable, input_chars, &mut rng);
    if let Some(l) = lctx.as_mut() { l.route = Some(json!({ "seat": c.seat, "explored": c.explored, "floor": c.floor, "why": c.why, "seed": seed, "evidence": c.evidence }).to_string()); }
    Ok(c.seat)
}

/// What a finished node writes, and how it got there.
#[derive(Clone, Debug)]
pub struct Outcome { pub fields: Map<String, Value>, pub seat_used: String, pub escalated_from: Option<String>, pub why_escalated: Option<String>, pub usd: f64, pub ms: u64, pub calls: u32, pub verified: bool, pub agreed: Option<bool> }

/// What a good try carries back: the answer, the exact prompt asked (the ledger row is found by its hash) and the raw text.
struct Tried { fields: Map<String, Value>, asked: String, text: String }
/// One seat's try: ask, validate, re-ask once with the error. Ok(fields) or why it failed.
async fn attempt(seat: &Seat, prompt: &str, schema: &Map<String, Value>, max_tokens: u32, timeout_s: u64, lctx: Option<crate::ledger::Ctx>, usd: &mut f64, ms: &mut u64, calls: &mut u32) -> Result<Tried, String> {
    let mut asked = prompt.to_string(); let mut last = String::new();
    for round in 0..2 {
        let (text, c, t) = call(seat, &asked, max_tokens, timeout_s, lctx.clone().map(|mut c| { c.try_n = Some(round + 1); c })).await?; *usd += c; *ms += t; *calls += 1;
        match extract_json(&text).ok_or_else(|| "the reply had no JSON object".to_string()).and_then(|v| validate(schema, &v)) { Ok(f) => return Ok(Tried { fields: f, asked, text }), Err(e) => { last = e.clone(); crate::ledger::mark_verdict(lctx.as_ref(), &asked, &text, Some(false), None, Some("parse_fail")); if round == 0 { asked = format!("{prompt}\nYour last answer was not valid: {e}. Answer again, ONLY the JSON.") } } }
    }
    Err(format!("it did not answer in the typed shape twice ({last})"))
}
/// The whole node: the seat, then the ladder. A seat's answer is accepted only if it passes the free source check and (when asked) the other seats agree; otherwise the next seat on the ladder is tried and the reason is kept.
pub async fn run(spec: &Spec, prompt: &str, source: &str, lctx: Option<crate::ledger::Ctx>) -> Result<Outcome, String> {
    let spec = &effective(spec);
    let mut lctx = lctx;
    let chosen = route(spec, prompt.chars().count(), &mut lctx)?;
    let schema: Map<String, Value> = spec.answer.clone().into_iter().collect(); let mut full = format!("{prompt}{}", answer_note(spec));
    if let (Some(ef), Some(c)) = (&spec.examples_from, lctx.as_mut()) {
        let ex = crate::team_memory::examples_for(&c.root, &c.team, &ef.task_class, prompt, ef.k as usize);
        if !ex.is_empty() { full = format!("{full}{}", crate::team_memory::render_examples(&ex)); c.examples_used = Some(json!(ex.iter().map(|e| e.id).collect::<Vec<_>>()).to_string()); }
    }
    let guarded = !spec.verify.in_source.is_empty() || !spec.agree_with.is_empty(); let max = spec.max_tokens.unwrap_or(200); let t_s = spec.timeout_s.unwrap_or(120);
    let ladder: Vec<String> = std::iter::once(chosen.clone()).chain(spec.fallback.iter().cloned()).chain(if crate::team_router::is_router_seat(&spec.seat) { spec.router.iter().map(|r| r.floor.clone()).filter(|f| !f.is_empty() && *f != chosen).collect::<Vec<_>>() } else { vec![] }).collect(); let (mut usd, mut ms, mut calls) = (0.0, 0u64, 0u32);
    let mut first_failed: Option<(String, String)> = None; let mut errs = vec![];
    for (i, s) in ladder.iter().enumerate() {
        let seat = parse_seat(s)?; let climbing = i > 0;
        match attempt(&seat, &full, &schema, max, t_s, lctx.clone().map(|mut c| { if climbing { c.climbed_from = Some(ladder[0].clone()); } c }), &mut usd, &mut ms, &mut calls).await {
            Ok(Tried { fields, asked, text }) => {
                if !spec.verify.in_source.is_empty() { if let Err(e) = verify_in_source(&spec.verify.in_source, &fields, source) { crate::ledger::mark_verdict(lctx.as_ref(), &asked, &text, Some(false), Some("guard"), Some("guard_failed")); errs.push(format!("{s}: {e}")); first_failed.get_or_insert((s.clone(), e)); continue; } }
                // the cross-check is for the cheap seats: a frontier seat's answer is not second-guessed by a smaller one
                let mut agreed = None;
                if !spec.agree_with.is_empty() && !frontier(&seat) {
                    let mut all = true;
                    for other in &spec.agree_with { let o = parse_seat(other)?; match attempt(&o, &full, &schema, max, t_s, lctx.clone(), &mut usd, &mut ms, &mut calls).await { Ok(g) => { if !same_answer(&fields, &g.fields) { all = false; break; } } Err(_) => { all = false; break; } } }
                    agreed = Some(all); if !all { crate::ledger::mark_verdict(lctx.as_ref(), &asked, &text, Some(false), Some("guard"), Some("guard_failed")); let e = "the seats did not give the same answer".to_string(); errs.push(format!("{s}: {e}")); first_failed.get_or_insert((s.clone(), e)); continue; }
                }
                if guarded { crate::ledger::mark_verdict(lctx.as_ref(), &asked, &text, Some(true), Some("guard"), None); }
                let (from, why) = match &first_failed { Some((f, w)) if climbing => (Some(f.clone()), Some(w.clone())), _ => (None, None) };
                return Ok(Outcome { fields, seat_used: s.clone(), escalated_from: from, why_escalated: why, usd, ms, calls, verified: !spec.verify.in_source.is_empty(), agreed });
            }
            Err(e) => { errs.push(format!("{s}: {e}")); first_failed.get_or_insert((s.clone(), e)); }
        }
    }
    Err(format!("no seat on the ladder gave a usable answer: {}", errs.join(" | ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn schema(v: Value) -> Map<String, Value> { v.as_object().unwrap().clone() }
    fn spec() -> Spec { Spec { seat: "endpoint:lfm3".into(), prompt: "Classify {ask}".into(), answer: schema(json!({ "kind": { "enum": ["research", "write", "review"] }, "confidence": "number?" })).into_iter().collect(), fallback: vec!["claude-code:haiku".into()], verify: Verify::default(), agree_with: vec![], max_tokens: None, timeout_s: None, examples_from: None, preset: None, router: None } }

    #[test]
    fn seats_have_a_grammar_and_the_ladder_only_goes_up() {
        assert_eq!(parse_seat("endpoint:lfm3").unwrap(), Seat::Endpoint { alias: "lfm3".into(), model: None }); assert_eq!(parse_seat("local:qwen3:4b").unwrap(), Seat::Local { tag: "qwen3:4b".into() }); assert_eq!(parse_seat("claude-code:haiku").unwrap(), Seat::Claude { model: "haiku".into() });
        for bad in ["", "lfm3", "endpoint:", "endpoint:a b", "local:", "claude-code:", "codex:gpt", "claude-code:x y"] { assert!(parse_seat(bad).is_err(), "{bad}"); }
        let mut s = spec(); assert!(check(&s).is_empty(), "{:?}", check(&s)); s.seat = "claude-code:sonnet".into(); s.fallback = vec!["endpoint:lfm3".into()]; assert!(check(&s).iter().any(|e| e.contains("only goes up")));
    }
    #[test]
    fn the_schema_is_checked_and_the_reply_is_read_leniently_and_typed() {
        assert!(check_schema(&schema(json!({ "kind": { "enum": ["a"] }, "n": "number", "ok2": "bool?", "t": ["string"] }))).is_empty());
        let bad = check_schema(&schema(json!({ "": "string", "x": "date", "y": { "enum": [] }, "z": 3, "seat_used": "string" }))).join(" | "); for n in ["plain field name", "`x`: a type is", "`y`: a type is", "`z`: a type is", "writes itself"] { assert!(bad.contains(n), "{n} in {bad}"); }
        let sc = spec().answer.clone().into_iter().collect::<Map<_, _>>();
        assert_eq!(extract_json("Sure!\n```json\n{\"kind\": \"Write\"}\n```").unwrap()["kind"], "Write"); assert!(extract_json("no json here").is_none());
        let ok = validate(&sc, &json!({ "kind": "Write", "confidence": "0,8", "extra": 1 })).unwrap(); assert_eq!(ok["kind"], "write", "an enum is matched without case and written as the schema spells it"); assert_eq!(ok["confidence"], 0.8); assert!(!ok.contains_key("extra"), "only the typed fields");
        assert!(validate(&sc, &json!({ "kind": "poem" })).unwrap_err().contains("must be one of research, write, review")); assert!(validate(&sc, &json!({})).unwrap_err().contains("`kind` is missing")); assert!(validate(&sc, &json!({ "kind": "write", "confidence": "high" })).unwrap_err().contains("must be number"));
        let l = schema(json!({ "tags": ["string"], "ok": "bool" })); assert!(validate(&l, &json!({ "tags": ["a", "b"], "ok": "yes" })).is_ok()); assert!(validate(&l, &json!({ "tags": ["a", 2], "ok": true })).is_err());
    }
    #[test]
    fn the_prompt_is_filled_from_the_run_and_an_unknown_name_is_said() {
        let get = |k: &str| match k { "ask" => Some("a post".to_string()), "writer.report" => Some("done".to_string()), "artifact:brief.md" => Some("# Brief".to_string()), _ => None };
        assert_eq!(fill("Judge {ask}: {writer.report} / {artifact:brief.md} / {\"json\": 1}", &get).unwrap(), "Judge a post: done / # Brief / {\"json\": 1}"); assert!(fill("Read {ghost.x}", &get).unwrap_err().contains("{ghost.x}"));
        assert_eq!(reads("{ask} {a.b} {artifact:x.md} {c.d.e}"), (vec!["a".to_string(), "c".to_string()], vec!["x.md".to_string()]));
    }
    #[test]
    fn a_field_the_model_made_up_is_caught_for_free_and_two_seats_must_agree_field_for_field() {
        let src = "Origin: Antigua, Guatemala. Price £18, 250g, free grinding."; let a = |v: Value| v.as_object().unwrap().clone();
        assert!(verify_in_source(&["price".into(), "origin".into()], &a(json!({ "price": 18.0, "origin": "Guatemala" })), src).is_ok());
        assert!(verify_in_source(&["price".into()], &a(json!({ "price": 18.25 })), src).unwrap_err().contains("does not appear"), "18.25 is 18 and 250g glued together"); assert!(verify_in_source(&["name".into()], &a(json!({})), src).is_err());
        assert!(same_answer(&a(json!({ "k": "Write", "n": 0.8 })), &a(json!({ "k": "write ", "n": 0.8001 })))); assert!(!same_answer(&a(json!({ "k": "write" })), &a(json!({ "k": "review" })))); assert!(!same_answer(&a(json!({ "k": "w" })), &a(json!({ "k": "w", "n": 1 }))));
        let mut s = spec(); s.verify = Verify { in_source: vec!["nope".into()], source: String::new() }; let e = check(&s).join("|"); assert!(e.contains("not a field of the answer") && e.contains("needs the `source`"));
    }
    #[test]
    fn the_prompt_ends_with_the_typed_answer_asked_for() { let n = answer_note(&spec()); assert!(n.contains("Answer ONLY JSON") && n.contains("\"kind\": \"research\" or \"write\" or \"review\"") && n.contains("\"confidence\": <number>"), "{n}"); }

    fn preset(kind: &str) -> PresetSpec { PresetSpec { kind: kind.into(), categories: vec!["bug".into(), "idea".into()], example: Some(json!({ "name": "Orchard", "price": 18 })), threshold: Some(0.7) } }
    fn small(p: PresetSpec) -> Spec { Spec { seat: "local:qwen3:4b".into(), prompt: "Do it".into(), preset: Some(p), ..Default::default() } }

    #[test]
    fn each_preset_ships_a_schema_and_a_guard() {
        for kind in PRESETS {
            let p = preset(kind);
            let schema = preset_schema(&p).unwrap_or_else(|e| panic!("{kind}: {e}"));
            assert!(!schema.is_empty(), "{kind} has a schema");
            assert!(check_schema(&schema.clone().into_iter().collect()).is_empty(), "{kind}: its schema is a valid one: {:?}", check_schema(&schema.into_iter().collect()));
            let _guard = preset_guard(&p);   // every preset names how it is held to account
            assert!(!preset_instruction(&p).is_empty(), "{kind} says what it wants");
        }
        // classify: the categories and "other", strict
        match preset_guard(&preset("classify")) { PresetGuard::StrictEnum(c) => assert_eq!(c, vec!["bug", "idea", "other"]), g => panic!("{g:?}") }
        assert_eq!(preset_schema(&preset("classify")).unwrap()["category"]["enum"], json!(["bug", "idea", "other"]));
        // extract: the schema is the example's fields and types, and the guard looks each one up in the source
        let s = preset_schema(&preset("extract")).unwrap(); assert_eq!(s["price"], json!("number")); assert_eq!(s["name"], json!("string"));
        match preset_guard(&preset("extract")) { PresetGuard::InSource(f) => assert_eq!(f, vec!["name", "price"]), g => panic!("{g:?}") }
        let e = effective(&small(preset("extract"))); assert_eq!(e.verify.in_source, vec!["name", "price"]); assert_eq!(e.verify.source, "{ask}"); assert!(e.prompt.contains("Do not guess"));
        // a judge answers yes or no; prose has no free check
        assert_eq!(preset_schema(&preset("judge")).unwrap()["verdict"]["enum"], json!(["yes", "no"]));
        assert_eq!(preset_guard(&preset("summarize")), PresetGuard::NeedsLadder);
        // the ones that cannot be built say why
        assert!(preset_schema(&PresetSpec { kind: "classify".into(), ..Default::default() }).unwrap_err().contains("categories"));
        assert!(preset_schema(&PresetSpec { kind: "extract".into(), ..Default::default() }).unwrap_err().contains("example"));
        assert!(preset_schema(&PresetSpec { kind: "poem".into(), ..Default::default() }).unwrap_err().contains("not a preset"));
    }

    #[test]
    fn a_small_seat_with_a_preset_is_never_unguarded() {
        for kind in PRESETS {
            let sp = small(preset(kind));
            let said = check(&sp);
            // either the preset's own guard holds it, or the validator says what to add: never silence
            assert!(guarded(&sp) || said.iter().any(|e| e.contains("preset_unguarded")), "{kind}: {said:?}");
            if guarded(&sp) { assert!(said.is_empty(), "{kind}: {said:?}"); }
        }
        // prose needs a seat to climb to; with one it is fine
        assert!(check(&small(preset("summarize"))).iter().any(|e| e.contains("preset_unguarded")));
        let mut ok = small(preset("summarize")); ok.fallback = vec!["claude-code:haiku".into()]; assert!(check(&ok).is_empty(), "{:?}", check(&ok)); assert!(guarded(&ok));
        // without a preset the old rule stands: a small seat with nothing is unguarded
        let bare = Spec { seat: "local:qwen3:4b".into(), prompt: "x".into(), answer: [("a".to_string(), json!("string"))].into_iter().collect(), ..Default::default() };
        assert!(!guarded(&bare));
        // and the node-level validator agrees
        let mut g = crate::team_graph::template("blank", "t", 1.0).unwrap(); g.nodes.insert("c".into(), crate::team_graph::Node::new(crate::team_graph::Kind::Llm(small(preset("classify")))));
        assert!(!crate::team_graph::validate(&g).words().iter().any(|w| w.contains("llm_unguarded")), "{:?}", crate::team_graph::validate(&g).words());
        g.nodes.insert("d".into(), crate::team_graph::Node::new(crate::team_graph::Kind::Llm(bare)));
        assert!(crate::team_graph::validate(&g).words().iter().any(|w| w.contains("d")), "an unguarded cell is still flagged");
    }

    #[test]
    fn a_node_with_a_preset_can_be_written_without_an_answer_and_is_valid() {
        // as a person or the Leader writes it: no `answer`, the preset ships the schema
        let sp: Spec = serde_json::from_value(json!({ "seat": "endpoint:a:b", "prompt": "classify {ask}", "preset": { "kind": "classify", "categories": ["bug", "idea"] }, "fallback": ["claude-code:haiku"] })).unwrap();
        assert!(sp.answer.is_empty()); assert!(check(&sp).is_empty(), "{:?}", check(&sp));
        assert_eq!(effective(&sp).answer["category"]["enum"], json!(["bug", "idea", "other"]));
        // without a preset an empty answer is still refused
        let bare: Spec = serde_json::from_value(json!({ "seat": "claude-code:haiku", "prompt": "p" })).unwrap(); assert!(!check(&bare).is_empty());
    }
}
