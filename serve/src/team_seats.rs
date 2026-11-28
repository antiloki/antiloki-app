//! THE LEADER AS MANAGER OF THE SEATS (operator 2026-11-13: "the team leader should see the models available on the machine, say if a model is a fit or not for the project, for the node ... if the user really wants it, it can
//! test it, with guards ... make everything easy"). The Leader is a frontier model and it JUDGES; this module gives it the facts and the levers, and the guards stay the serve's:
//!   `inventory`  what this machine has: the CLI seats, the models Ollama holds, the endpoints named in `~/.antiloki/seats.json` (reachable or not), a `llama-server` if there is one, the hardware, what could be downloaded and
//!                whether it would fit, and the measured evidence per stage kind (Appendix E of RFC 0010), with what is NOT measured said as such.
//!   `fit`        the evidence for one stage kind and one seat, the hardware verdict, and the guards that seat needs: facts for the Leader's judgement, never a veto on the person.
//!   `register`   name an endpoint (a local server) so a node can use it: one call, no hand-edited file.
//!   `pull`       download a model through Ollama, only when the person has said yes (`approved`): nothing is ever downloaded by itself.
//!   `trial`      run a seat on cases the Leader wrote (traps included) through the same machinery a node uses, and report accuracy, the mistakes, and how many were caught by the guards. A benchmark that makes no mistakes shows nothing.
use serde_json::{json, Map, Value};

const EVIDENCE: &str = include_str!("team_seats_evidence.json");
pub fn evidence() -> Value { serde_json::from_str(EVIDENCE).unwrap_or(Value::Null) }

// ── what the machine is ──────────────────────────────────────────────────────────────────────────────────────────────────────

fn meminfo_mb(key: &str) -> Option<u64> { std::fs::read_to_string("/proc/meminfo").ok()?.lines().find(|l| l.starts_with(key))?.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb / 1024) }
async fn gpu() -> Value {
    let out = tokio::time::timeout(std::time::Duration::from_secs(3), crate::proc::tokio_cmd("nvidia-smi").args(["--query-gpu=name,memory.total", "--format=csv,noheader"]).output()).await;
    match out { Ok(Ok(o)) if o.status.success() => json!({ "found": true, "detail": String::from_utf8_lossy(&o.stdout).trim() }), _ => json!({ "found": false, "detail": "none detected (CPU only: small models only, and they are slow)" }) }
}
fn llama_server() -> Option<String> {
    if let Ok(p) = std::env::var("PATH") { for d in p.split(':') { let f = std::path::Path::new(d).join("llama-server"); if f.is_file() { return Some(f.display().to_string()); } } }
    let base = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/opt");
    for e in std::fs::read_dir(&base).into_iter().flatten().flatten() { for sub in [e.path(), e.path().join("bin")].into_iter().chain(std::fs::read_dir(e.path()).into_iter().flatten().flatten().map(|x| x.path())) { let f = sub.join("llama-server"); if f.is_file() { return Some(f.display().to_string()); } } }
    None
}
/// A model that could be downloaded, and whether it fits this machine now (its file size x 1.3 of free RAM, leaving the desktop alone). Pure.
pub fn fits(size_mb: u64, ram_available_mb: Option<u64>) -> Value { let need = (size_mb as f64 * 1.3) as u64; match ram_available_mb { Some(a) => json!({ "ram_needed_mb": need, "fits_now": a >= need + 1500, "ram_available_mb": a }), None => json!({ "ram_needed_mb": need, "fits_now": null }) } }
fn suggestions(ram: Option<u64>, held: &[String], endpoints: &[String]) -> Vec<Value> {
    let have = |s: &str| held.iter().any(|h| h.contains(s)) || endpoints.iter().any(|e| e.contains(s));
    let mut v = vec![
        json!({ "name": "qwen3:4b", "how": "ollama pull qwen3:4b", "size_mb": 2500, "have": have("qwen3:4b"), "kind": "ollama", "good_for": ["rule_check", "intent", "tone_rubric", "field_extraction (with the source check)"], "measured": true, "slow": "about 6 s a call on a CPU-only laptop", "use_as": "local:qwen3:4b" }),
        json!({ "name": "LFM3-1.7B (Q4_K_M)", "how": "download the GGUF from huggingface.co/LiquidAI/LFM3-1.7B-GGUF and serve it with llama-server (`--reasoning-budget 0`); the installed Ollama may not load it", "size_mb": 1100, "have": have("lfm3"), "kind": "llama.cpp endpoint", "good_for": ["field_extraction (with the source check)", "faithfulness", "a cross-check partner"], "measured": true, "slow": "about 3-5 s a call on a CPU-only laptop", "use_as": "endpoint:lfm3 (register it with antiloki_team_seats register)" }),
        json!({ "name": "whisper tiny (faster-whisper)", "how": "pip install faster-whisper (or whisper-ctranslate2); the model downloads on first use", "size_mb": 75, "have": false, "kind": "tool", "good_for": ["transcribe"], "measured": true, "use_as": "a tool step, not a model node" }),
        json!({ "name": "piper en_US-lessac-low", "how": "pip install piper-tts, then the voice file from huggingface.co/rhasspy/piper-voices", "size_mb": 63, "have": false, "kind": "tool", "good_for": ["speak"], "measured": true, "use_as": "a tool step, not a model node" }),
    ];
    for s in v.iter_mut() { let size = s["size_mb"].as_u64().unwrap_or(0); if let (Some(o), Value::Object(f)) = (s.as_object_mut(), fits(size, ram)) { for (k, x) in f { o.insert(k, x); } } }
    v
}

// ── the inventory ────────────────────────────────────────────────────────────────────────────────────────────────────────────

fn port_ok(url: &str) -> bool { url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost") }
/// The endpoints the person has named, with whether each answers now.
async fn endpoints() -> Vec<Value> {
    let path = crate::team_llm::seats_path(); let v: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null); let mut out = vec![];
    let c = reqwest::Client::builder().timeout(std::time::Duration::from_secs(2)).connect_timeout(std::time::Duration::from_secs(1)).build().unwrap_or_default();
    for (alias, e) in v["endpoints"].as_object().into_iter().flatten() {
        let url = e["url"].as_str().unwrap_or("").trim_end_matches('/').to_string(); let mut up = false; let mut model = e["model"].as_str().unwrap_or("").to_string();
        for p in ["/v1/models", "/health"] { if let Ok(r) = c.get(format!("{url}{p}")).send().await { if r.status().is_success() { up = true; if p == "/v1/models" { if let Ok(j) = r.json::<Value>().await { if let Some(m) = j["data"][0]["id"].as_str() { if model.is_empty() { model = m.into(); } } } } break; } } }
        out.push(json!({ "alias": alias, "seat": format!("endpoint:{alias}"), "url": url, "model": model, "reachable": up, "max_tokens": e["max_tokens"] }));
    }
    out
}
pub async fn inventory() -> Value {
    let cfg = crate::local::Config::from_env(); let probe = crate::local::probe(&cfg).await; let ram = crate::local::memory_available_mb();
    let eps = endpoints().await; let held: Vec<String> = probe.models.iter().map(|m| m.name.to_lowercase()).collect(); let ep_names: Vec<String> = eps.iter().map(|e| format!("{} {}", e["alias"].as_str().unwrap_or(""), e["model"].as_str().unwrap_or("")).to_lowercase()).collect();
    let cli: Vec<Value> = crate::cli::Engine::all().iter().map(|e| json!({ "seat": format!("{}:*", e.key()), "engine": e.key(), "installed": crate::cli::engine_available(*e), "frontier": true, "note": if *e == crate::cli::Engine::Opencode { "can run any model, local ones included: not a Leader seat" } else { "a frontier seat" } })).collect();
    json!({ "ok": true,
        "hardware": { "cpu_threads": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1), "ram_total_mb": meminfo_mb("MemTotal:"), "ram_available_mb": ram, "gpu": gpu().await,
                      "advice": "on a CPU-only machine use models of 4B parameters or less, one at a time, and expect seconds per answer" },
        "cli_seats": cli,
        "ollama": { "reachable": probe.reachable, "version": probe.version, "error": probe.error, "models": probe.models.iter().map(|m| json!({ "seat": format!("local:{}", m.name), "size_mb": m.size_bytes / 1_048_576, "params": m.params })).collect::<Vec<_>>(), "num_ctx_set": cfg.num_ctx.is_some(), "note": if cfg.num_ctx.is_none() { "set ANTILOKI_LOCAL_NUM_CTX before a `local:` seat can be used (a context is never defaulted)" } else { "" } },
        "endpoints": eps, "llama_server": llama_server(), "seats_file": crate::team_llm::seats_path().display().to_string(),
        "could_download": suggestions(ram, &held, &ep_names),
        "evidence": evidence(),
        "how_to_judge": "A step suits a small seat when (1) the evidence shows it near the ceiling, or (2) a free check (verify.in_source, a rule) or a second seat can tell when it is wrong, and (3) a stronger seat is on the ladder. If the stage is not in the evidence, it is unmeasured: run `trial` with cases you write, traps included, before relying on it. The person may overrule you: then guard it and trial it." })
}

// ── fit: facts for the judgement ───────────────────────────────────────────────────────────────────────────────────────────

/// The evidence for one stage kind and one seat, what the hardware says, and the guards the seat needs. Facts, not a veto. Pure over `inv` (the inventory) and the evidence.
pub fn fit(stage: &str, seat: &str, inv: &Value) -> Value {
    let ev = evidence(); let st = ev["stages"][stage].clone();
    let frontier = seat.starts_with("claude-code:") || seat.is_empty(); let mut notes = vec![]; let mut guards = vec![];
    let measured = !st.is_null();
    if !measured { notes.push(format!("`{stage}` is not in the evidence: UNMEASURED. Run a trial (antiloki_team_seats trial) with cases you write, traps included, before this step relies on a small seat.")); }
    let mut seat_row = Value::Null;
    if measured && !frontier { let key = seat.trim_start_matches("endpoint:").trim_start_matches("local:").to_lowercase(); seat_row = st["local"].as_array().into_iter().flatten().find(|r| r["seat"].as_str().map_or(false, |s| s.to_lowercase().contains(&key))).cloned().unwrap_or(Value::Null); if seat_row.is_null() { notes.push(format!("`{seat}` has not been measured on `{stage}`: trial it.")); } }
    if !frontier {
        guards.push("a `fallback` ladder to a stronger seat (the ladder only goes up)".to_string());
        if st["verifiable_for_free"] == json!(true) { guards.push("`verify.in_source` or a `check` node: this stage can be checked for free".into()); } else { guards.push("`agree_with` another seat (or a `check` rule): nothing here can be verified for free".into()); }
        if let Some(a) = seat_row["accuracy"].as_f64() { if a < 0.9 { notes.push(format!("measured {:.0}% alone on `{stage}` (n={}): NOT a fit without the guards; with them the cost is the climbs.", a * 100.0, st["n"])); } else { notes.push(format!("measured {:.0}% alone on `{stage}` (n={}): near the ceiling; still keep the ladder.", a * 100.0, st["n"])); } }
        if stage == "code" { notes.push("do not give code to a small seat: measured 0 of 6 hidden tests, and a draft-then-fix cascade cost more than the frontier seat alone.".into()); }
    }
    let ram = inv["hardware"]["ram_available_mb"].as_u64(); let held = inv["ollama"]["models"].as_array().cloned().unwrap_or_default(); let eps = inv["endpoints"].as_array().cloned().unwrap_or_default();
    let present = if frontier { json!(true) } else if let Some(t) = seat.strip_prefix("local:") { json!(held.iter().any(|m| m["seat"] == json!(format!("local:{t}")))) } else if let Some(a) = seat.strip_prefix("endpoint:") { let alias = a.split(':').next().unwrap_or(""); json!(eps.iter().any(|e| e["alias"] == json!(alias) && e["reachable"] == json!(true))) } else { json!(false) };
    if present == json!(false) { notes.push("that seat is not available here now: see `could_download`, or register an endpoint, or start the server.".into()); }
    json!({ "ok": true, "stage": stage, "seat": seat, "measured": measured, "stage_evidence": st, "seat_evidence": seat_row, "seat_available": present, "ram_available_mb": ram, "guards_required": guards, "notes": notes,
            "reminder": "These are facts for your judgement. If the person insists on a seat you advise against, say the risk plainly, require the guards above, trial it first, and do it: the choice is theirs." })
}

// ── register, pull ───────────────────────────────────────────────────────────────────────────────────────────────────────────

/// Name an endpoint in the seats file. A local address (or https) only; never a secret; the file is the person's (we add one entry).
pub fn register(alias: &str, url: &str, model: &str, ctx: Option<u64>, max_tokens: Option<u64>) -> Result<Value, String> {
    if alias.is_empty() || !alias.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') { return Err("an alias is plain characters (lfm3)".into()); }
    if !(port_ok(url) || url.starts_with("https://")) { return Err("an endpoint is a local server (http://127.0.0.1:PORT) or an https address".into()); }
    if url.contains('@') || url.contains('?') { return Err("no credentials or query in an endpoint address".into()); }
    let path = crate::team_llm::seats_path(); let mut v: Value = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_else(|| json!({}));
    if !v.is_object() { v = json!({}); } if !v["endpoints"].is_object() { v["endpoints"] = json!({}); }
    let mut e = Map::new(); e.insert("url".into(), json!(url.trim_end_matches('/'))); if !model.is_empty() { e.insert("model".into(), json!(model)); } if let Some(c) = ctx { e.insert("ctx".into(), json!(c)); } if let Some(m) = max_tokens { e.insert("max_tokens".into(), json!(m)); }
    v["endpoints"][alias] = Value::Object(e);
    if let Some(d) = path.parent() { let _ = std::fs::create_dir_all(d); } std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap_or_default()).map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true, "seat": format!("endpoint:{alias}"), "file": path.display().to_string(), "note": "registered; `antiloki_team_seats list` says whether it answers" }))
}
/// `ollama pull <tag>` in the background, only after the person said yes. The log is a file; `list` shows the model once it is there.
pub fn pull(tag: &str, approved: bool) -> Result<Value, String> {
    if !approved { return Err("a download needs the person's yes: tell them the model, its size and why, and when they agree call this again with approved: true".into()); }
    if tag.is_empty() || tag.len() > 80 || !tag.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-')) || tag.starts_with('-') { return Err("that is not a model tag".into()); }
    let dir = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".antiloki/run"); let _ = std::fs::create_dir_all(&dir);
    let log = dir.join(format!("pull-{}.log", tag.replace(['/', ':'], "-"))); let f = std::fs::File::create(&log).map_err(|e| e.to_string())?; let f2 = f.try_clone().map_err(|e| e.to_string())?;
    crate::proc::cmd("ollama").args(["pull", tag]).stdout(f).stderr(f2).stdin(std::process::Stdio::null()).spawn().map_err(|e| format!("could not start ollama: {e}"))?;
    Ok(json!({ "ok": true, "started": tag, "log": log.display().to_string(), "note": "downloading in the background; `list` shows the model when it is done" }))
}

// ── trial ────────────────────────────────────────────────────────────────────────────────────────────────────────────────────

/// A trial: the seat, a prompt ({ask} is each case's input), the typed answer, optional free check, and the cases (at most 30): `{input, expect: {field: value}, trap?: bool}`. Through the same machinery a node uses, WITHOUT a ladder, so the
/// seat's own mistakes show; the ones the guards would have stopped are counted apart from the ones that would have been delivered.
pub async fn trial(args: &Value) -> Result<Value, String> {
    let seat = args["seat"].as_str().unwrap_or("").to_string(); crate::team_llm::parse_seat(&seat)?;
    let answer: std::collections::BTreeMap<String, Value> = args["answer"].as_object().ok_or("`answer` is the typed answer (field -> type)")?.clone().into_iter().collect();
    let prompt = args["prompt"].as_str().filter(|p| !p.trim().is_empty()).ok_or("`prompt` is the question, with {ask} where each case's input goes")?.to_string();
    let cases = args["cases"].as_array().ok_or("`cases` is a list of {input, expect, trap?}")?; if cases.is_empty() || cases.len() > 30 { return Err("between 1 and 30 cases".into()); }
    let verify = match args.get("verify") { Some(v) if v.is_object() => serde_json::from_value::<crate::team_llm::Verify>(v.clone()).map_err(|e| e.to_string())?, _ => Default::default() };
    let spec = crate::team_llm::Spec { seat: seat.clone(), prompt: prompt.clone(), answer, fallback: vec![], verify, agree_with: vec![], max_tokens: args["max_tokens"].as_u64().map(|m| m as u32), timeout_s: Some(args["timeout_s"].as_u64().unwrap_or(90)), examples_from: None, preset: None, router: None };
    let bad = crate::team_llm::check(&spec); if !bad.is_empty() { return Err(bad.join("; ")); }
    let started = std::time::Instant::now(); let (mut correct, mut wrong, mut caught, mut none, mut ms) = (0, 0, 0, 0, 0u64); let mut mistakes: Vec<Value> = vec![]; let (mut trap_n, mut trap_ok) = (0, 0);
    for c in cases {
        if started.elapsed() > std::time::Duration::from_secs(600) { return Err("the trial took over ten minutes: fewer cases".into()); }
        let input = c["input"].as_str().unwrap_or("").to_string(); let expect = c["expect"].as_object().cloned().unwrap_or_default(); let trap = c["trap"].as_bool().unwrap_or(false); if trap { trap_n += 1; }
        let filled = crate::team_llm::fill(&prompt.replace("{input}", "{ask}"), &|k| (k == "ask").then(|| input.clone())).map_err(|e| e.to_string())?; let source = if spec.verify.source.is_empty() { String::new() } else { crate::team_llm::fill(&spec.verify.source.replace("{input}", "{ask}"), &|k| (k == "ask").then(|| input.clone())).unwrap_or_default() };
        match crate::team_llm::run(&spec, &filled, &source, None).await {
            Ok(o) => { ms += o.ms; let good = expect.iter().all(|(k, x)| o.fields.get(k).map_or(false, |y| crate::team_llm::same_answer(&[(k.clone(), x.clone())].into_iter().collect(), &[(k.clone(), y.clone())].into_iter().collect()))); if good { correct += 1; if trap { trap_ok += 1; } } else { wrong += 1; mistakes.push(json!({ "input": input.chars().take(140).collect::<String>(), "got": o.fields, "expected": expect, "kind": "WRONG AND NOT CAUGHT: this one would have been delivered", "trap": trap })); } }
            Err(e) => { if e.contains("does not appear in the source") || e.contains("did not give the same answer") { caught += 1; mistakes.push(json!({ "input": input.chars().take(140).collect::<String>(), "kind": "caught by the guard", "why": e.chars().take(160).collect::<String>(), "trap": trap })); } else { none += 1; mistakes.push(json!({ "input": input.chars().take(140).collect::<String>(), "kind": "no usable answer", "why": e.chars().take(160).collect::<String>(), "trap": trap })); } }
        }
    }
    let n = cases.len(); mistakes.truncate(15); let answered = correct + wrong;
    let mut warnings = vec![]; if n < 10 { warnings.push("fewer than 10 cases: a small trial shows little".to_string()); } if trap_n * 3 < n { warnings.push("fewer than a third of the cases are traps (inputs the seat is likely to get wrong): a benchmark that makes no mistakes proves nothing".into()); } if correct + caught == n { warnings.push("it made no mistakes that would have been delivered: if you wrote traps, good; if not, write harder ones".into()); }
    Ok(json!({ "ok": true, "seat": seat, "cases": n, "correct": correct, "wrong_and_delivered": wrong, "caught_by_guards": caught, "no_usable_answer": none, "trap_cases": trap_n, "trap_correct": trap_ok,
               "accuracy_of_answers_given": if answered == 0 { Value::Null } else { json!((correct as f64 / answered as f64 * 1000.0).round() / 1000.0) }, "avg_ms": if answered == 0 { 0 } else { ms / answered as u64 }, "mistakes": mistakes, "warnings": warnings,
               "reading": "`wrong_and_delivered` is the number that matters: it is what a node on this seat would hand on without anyone noticing, unless a guard or the ladder stands in front of it." }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inv() -> Value { json!({ "hardware": { "ram_available_mb": 4000 }, "ollama": { "models": [{ "seat": "local:qwen3:4b" }] }, "endpoints": [{ "alias": "lfm3", "reachable": true }, { "alias": "down", "reachable": false }] }) }
    #[test]
    fn the_evidence_is_read_and_names_what_is_not_for_small_models() { let e = evidence(); assert!(e["stages"]["field_extraction"]["verifiable_for_free"] == json!(true) && e["stages"]["code"]["local"][0]["accuracy"] == json!(0.0)); assert!(e["not_for_small_models"].as_array().unwrap().iter().any(|x| x.as_str().unwrap().contains("writing code"))); }
    #[test]
    fn fit_gives_facts_for_a_measured_stage_and_says_unmeasured_when_it_is() {
        let f = fit("tone_rubric", "endpoint:lfm3", &inv()); assert_eq!(f["measured"], true); assert_eq!(f["seat_evidence"]["accuracy"], 0.8); assert!(f["notes"][0].as_str().unwrap().contains("NOT a fit without the guards")); assert!(f["guards_required"].as_array().unwrap().iter().any(|g| g.as_str().unwrap().contains("agree_with")));
        let e = fit("field_extraction", "endpoint:lfm3", &inv()); assert!(e["guards_required"].as_array().unwrap().iter().any(|g| g.as_str().unwrap().contains("verify.in_source"))); assert_eq!(e["seat_available"], true);
        let u = fit("poetry", "local:qwen3:4b", &inv()); assert_eq!(u["measured"], false); assert!(u["notes"][0].as_str().unwrap().contains("UNMEASURED"));
        assert_eq!(fit("rule_check", "endpoint:down", &inv())["seat_available"], false); assert!(fit("code", "endpoint:lfm3", &inv())["notes"].to_string().contains("do not give code")); assert_eq!(fit("intent", "claude-code:haiku", &inv())["guards_required"].as_array().unwrap().len(), 0);
        assert_eq!(fit("intent", "local:qwen3:4b", &inv())["seat_available"], true); assert!(fit("intent", "local:gemma:2b", &inv())["notes"].to_string().contains("not available"));
    }
    #[test]
    fn a_download_fits_only_with_room_to_spare_and_needs_the_persons_yes() {
        assert_eq!(fits(2500, Some(6000))["fits_now"], true); assert_eq!(fits(2500, Some(3000))["fits_now"], false); assert_eq!(fits(2500, None)["fits_now"], Value::Null);
        assert!(pull("qwen3:4b", false).unwrap_err().contains("person's yes")); assert!(pull("--evil", true).is_err()); assert!(pull("a b", true).is_err()); assert!(pull("", true).is_err());
        let s = suggestions(Some(6000), &["qwen3:4b".to_string()], &[]); assert_eq!(s[0]["have"], true); assert_eq!(s[1]["have"], false); assert_eq!(s[1]["fits_now"], true);
    }
    #[test]
    fn an_endpoint_is_registered_locally_and_never_with_credentials() {
        assert!(register("", "http://127.0.0.1:1", "", None, None).is_err()); assert!(register("a b", "http://127.0.0.1:1", "", None, None).is_err()); assert!(register("x", "http://evil.example", "", None, None).is_err()); assert!(register("x", "http://127.0.0.1:1?k=secret", "", None, None).is_err()); assert!(register("x", "http://user:pw@127.0.0.1:1", "", None, None).is_err());
    }
}
