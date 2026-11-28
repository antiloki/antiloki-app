//! cockpit-plan-tribunal.rs — twin of skills/cockpit/cockpit-plan-tribunal.mjs (db lane, cross-db
//! brain.*/knowledge.*/judgments.*). NAMES ARE CONTRACT: default `cockpitPlanTribunal`.
//!
//! The JUDGE PANEL of the Ultra Plan, run as a tribunal: two ADVOCATE calls (defender +
//! prosecutor) over one shared CASE FILE, then a HIGH-tier JUDGE call that rules + proposes
//! concrete node changes. A separate CODE BLUEPRINT design flow (mode `tribunal-design`) draws a
//! file spine (1 mid call) then writes one code stub per file (1 low call each, parse-accepted via
//! the already-converted `blueprint__helpers_author_accept` twin) before handing the stubs to
//! `blueprint_blueprint_parse`. Two read-only modes (`tribunal-history`, `tribunal-blueprint`) make
//! no AI call at all and are the file's fully fixture-reachable surface.
//!
//! `context.db` missing → JS throws `err.contextMissing('cockpit-plan-tribunal', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin). `context.provider_fn` has no Rust equivalent to carry — threaded as
//! `null` into every constructed aiCall input, same as book-run.rs's `provider_fn`.
//!
//! PARITY-DEFERRED: every AI call (`advocate`, the judge's `runJudge`, `formatRuling`, the
//! skeleton call and each per-file stub call in `tribunal-design`) dispatches through
//! `crate::ai_ai_call::aiCall`, the deferred network-AI stub (see ai-call.rs's module doc) — it
//! always returns `Err`, so any code path that reaches an AI call propagates that error once the
//! (real, ported) deterministic surface above it has run. The two read-only modes never touch
//! aiCall and are fully reachable. `renderShape`, `gatherIntentContext`, `parseAdvocate`,
//! `buildRuling`/`lintRuling`, and `parseSkeletonFiles` are ported as their own PURE functions and
//! unit-tested directly with canned values — the response-shaping ones (`parseAdvocate`,
//! `buildRuling`, `parseSkeletonFiles`) are unreachable from the default export itself (the `?` on
//! aiCall short-circuits before they run) but correct by construction, same pattern as book-run.rs's
//! `parse_arr`/`parse_obj` and pl-research.rs's `build_research_report`.
//!
//! Deps reused (already-converted twins): `ai_prompt_find`/`ai_prompt_add` (ensure-prompt),
//! `cockpit__helpers_context_brief::resolveSourceText`, `blueprint_blueprint_parse::blueprintParse`,
//! `blueprint__helpers_code_checks::extractRawCode`, `blueprint__helpers_author_accept::acceptAuthoredCode`,
//! `cockpit_cockpit_schema::ensureCockpitSchema`.
// QUALITY: mixed unit — 2 fully fixture-reachable read-only modes + deterministic assembly/db-read
// surface up to each mode's first aiCall dispatch (PARITY-DEFERRED) + 5 pure response-shaping
// helpers unit-tested directly (unreachable from the default export, correct by construction).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
fn first(rows: &Value) -> Option<Value> { rows.as_array().and_then(|a| a.first()).cloned() }
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

// ── prompt bodies (verbatim from the JS module) ─────────────────────────────────────────────
const PROMPT_ADV: &str = "cockpit-plan-advocate";
const SYSTEM_ADV: &str = "You are an ADVOCATE in a tribunal reviewing an ULTRA PLAN for a software project. You argue exactly the SIDE you are assigned, as forcefully but as HONESTLY as the evidence allows — never invent facts beyond the case file. Output ONLY valid JSON: {\"stance\":\"one-sentence overall position for your side\",\"arguments\":[{\"claim\":\"one line\",\"support\":\"why, grounded in the plan/context, <=2 sentences\",\"weight\":\"low|medium|high\"}],\"strongest\":\"your single most compelling point\"}. Give 3-6 arguments, each distinct. No prose outside the JSON.";
const TEMPLATE_ADV: &str = "YOUR SIDE: {{side}}\nYOUR GOAL: {{goal}}\n\n{{case_file}}\n\nArgue your side. Output the JSON.";

const PROMPT_JUDGE: &str = "cockpit-plan-tribunal-judge";
const JUDGE_CAP: i64 = 8000;
const SYSTEM_JUDGE: &str = "You are the presiding JUDGE of a tribunal over an ULTRA PLAN. You have read the CASE FILE (the intent context + the full plan, whose shape is branches of microtask NODES), the DEFENSE, and the PROSECUTION. Weigh both pleas and rule from the evidence — decide, do not average. Then turn the ruling into CONCRETE edits to the node tree. Output ONLY valid JSON: {\"verdict\":\"sound|sound_with_revisions|not_ready\",\"confidence\":0-100,\"headline\":\"the verdict in one sentence\",\"evidence\":[\"a SPECIFIC grounded fact from the plan or a plea, <=20 words\"],\"insights\":[\"what that evidence MEANS — your analysis, <=20 words\"],\"to_approve\":[\"a concrete correction that would flip this plan to APPROVED, <=16 words\"],\"opinion\":\"a 2-3 sentence synthesis that ties the evidence and insights together and justifies the verdict\",\"dimensions\":[{\"name\":\"product|scope|feasibility|risk|sequencing|clarity\",\"score\":0-100,\"note\":\"one line\"}],\"changes\":{\"add\":[{\"branch\":\"the dimension key it belongs to\",\"title\":\"the new microtask (<=10 words)\",\"detail\":\"the nuance (<=22 words)\",\"why\":\"why the plan needs it\"}],\"update\":[{\"target\":\"the EXACT title of an existing node from the plan\",\"change\":\"what it should become\",\"why\":\"why\"}],\"remove\":[{\"target\":\"the EXACT title of an existing node\",\"why\":\"why it should be cut\"}]},\"preserve\":[\"a real strength to keep\"],\"decisive_factor\":\"the one thing that tipped the ruling\"}. Give 3-5 evidence, 2-4 insights, 1-4 to_approve, 4-6 dimensions; 1-4 add, 1-4 update, 0-3 remove — each grounded in a real gap or flaw, never filler, and every change target must match a node title from the case file; the to_approve items summarise those changes; 1-4 preserve. If a side submitted NO arguments, still deliver a COMPLETE ruling — judge the plan on its own merits and never shrink your answer to just the verdict; your ENTIRE answer is that one JSON object. You MUST populate evidence, insights AND opinion — a ruling that is only a verdict/headline is INVALID and will be rejected. No prose outside the JSON.";
const TEMPLATE_JUDGE: &str = "{{case_file}}\n\n════ THE DEFENSE (argues the plan is SOUND) ════\n{{defense}}\n\n════ THE PROSECUTION (argues the plan is NOT ready) ════\n{{prosecution}}\n\nWeigh both, write the DETAILED opinion, and propose the concrete node changes. Output the JSON.";

const PROMPT_SKEL: &str = "cockpit-tribunal-skeleton";
const SYSTEM_SKEL: &str = "You are the ARCHITECT drawing the SPINE of the change a plan (revised by a court ruling) requires: WHICH FILES, nothing more. Output ONLY valid JSON: {\"files\":[{\"path\":\"exact/relative/path.ext\",\"action\":\"create|modify\",\"intent\":\"what this file does for the change, 1-2 sentences\"}]}. Rules: 2-6 files — the SMALLEST spine that delivers the plan; for \"modify\", the path MUST be a real existing path copied from the grounding; for \"create\", use a path conventional for the project's stack. No prose outside the JSON.";
const TEMPLATE_SKEL: &str = "THE PLAN:\n{{plan}}\n\nTHE COURT'S CORRECTIONS (the ruling to honour):\n{{corrections}}\n\nGROUNDING (the codebase as the substrate knows it):\n{{grounding}}\n\nDraw the file spine. Output the JSON.";

const PROMPT_STUB: &str = "cockpit-tribunal-stub";
const SYSTEM_STUB: &str = "You are the ARCHITECT writing ONE file of a change as a CODE STUB — real, parseable code in the file's language (infer it from the path): the imports it needs + every function/class/type/const signature the file defines, each body a TODO comment. When MODIFYING a file, keep the current symbols you are not changing (same signatures) so the diff is honest. Output ONLY the code — no prose, no fences, no JSON; your very first character is code.";
const TEMPLATE_STUB: &str = "FILE: {{path}} ({{action}})\nINTENT: {{intent}}\n\nTHE PLAN (what the change delivers):\n{{plan}}\n\nCURRENT SYMBOLS IN THIS FILE (keep the ones you do not change):\n{{current}}\n\nWrite the code stub for this ONE file.";

const PROMPT_FMT: &str = "cockpit-plan-tribunal-format";
const SYSTEM_FMT: &str = "You are a strict JSON FORMATTER. You receive a judge's ruling that may be messy, partial, or written as prose. Reshape ALL of its content into EXACTLY this schema — invent nothing beyond what the ruling contains, but if a field is genuinely absent derive a short faithful value from the rest. Output ONLY valid JSON: {\"verdict\":\"sound|sound_with_revisions|not_ready\",\"confidence\":0-100,\"headline\":\"the verdict in one sentence\",\"evidence\":[\"a grounded fact <=20 words\"],\"insights\":[\"what it means <=20 words\"],\"to_approve\":[\"a correction to get approved <=16 words\"],\"opinion\":\"2-3 sentence synthesis\",\"dimensions\":[{\"name\":\"product|scope|feasibility|risk|sequencing|clarity\",\"score\":0-100,\"note\":\"one line\"}],\"changes\":{\"add\":[{\"branch\":\"key\",\"title\":\"<=10 words\",\"detail\":\"<=22 words\",\"why\":\"why\"}],\"update\":[{\"target\":\"exact node title\",\"change\":\"what it becomes\",\"why\":\"why\"}],\"remove\":[{\"target\":\"exact node title\",\"why\":\"why\"}]},\"preserve\":[\"a strength\"],\"decisive_factor\":\"the tipping point\"}. Fill evidence (3-5), insights (2-4), opinion and dimensions (4-6) from the source. No prose outside the JSON.";
const TEMPLATE_FMT: &str = "THE RAW RULING TO REFORMAT (reshape everything below into the clean JSON):\n{{raw}}\n\nOutput the clean JSON.";

// try promptFind + compare template/system(/cap); on miss or mismatch, promptAdd (which itself
// versions). Mirrors book-run.rs's ensure_faculty, extended with the JS's re-seed-on-change check.
fn ensure_prompt(db: &Connection, name: &str, template_md: &str, system_md: &str, tier: &str, temp: &str, cap: i64, output_fields: Value) -> Result<(), String> {
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": name }), db) {
        let t_ok = existing.get("template_md").and_then(|v| v.as_str()) == Some(template_md);
        let s_ok = existing.get("system_md").and_then(|v| v.as_str()) == Some(system_md);
        let c_ok = existing.get("max_output_tokens").and_then(|v| v.as_i64()).map_or(true, |c| c == cap);
        if t_ok && s_ok && c_ok {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": name, "template_md": template_md, "system_md": system_md,
            "default_tier": tier, "default_temperature": temp, "phase_flow": "single_only",
            "max_output_tokens": cap, "output_fields": output_fields,
        }),
        db,
    )?;
    Ok(())
}
fn ensure_advocate_prompt(db: &Connection) -> Result<(), String> {
    ensure_prompt(db, PROMPT_ADV, TEMPLATE_ADV, SYSTEM_ADV, "mid", "cold", 2200, json!([{ "field_name": "advocate_json", "field_type": "string" }]))
}
fn ensure_judge_prompt(db: &Connection) -> Result<(), String> {
    ensure_prompt(db, PROMPT_JUDGE, TEMPLATE_JUDGE, SYSTEM_JUDGE, "high", "cold", JUDGE_CAP, json!([{ "field_name": "ruling_json", "field_type": "string" }]))
}
fn ensure_skeleton_prompt(db: &Connection) -> Result<(), String> {
    ensure_prompt(db, PROMPT_SKEL, TEMPLATE_SKEL, SYSTEM_SKEL, "mid", "cold", 1600, json!([{ "field_name": "skeleton_json", "field_type": "string" }]))
}
fn ensure_stub_prompt(db: &Connection) -> Result<(), String> {
    ensure_prompt(db, PROMPT_STUB, TEMPLATE_STUB, SYSTEM_STUB, "low", "cold", 2000, json!([{ "field_name": "stub_source", "field_type": "string" }]))
}
fn ensure_format_prompt(db: &Connection) -> Result<(), String> {
    ensure_prompt(db, PROMPT_FMT, TEMPLATE_FMT, SYSTEM_FMT, "low", "freezed", 4000, json!([{ "field_name": "ruling_json", "field_type": "string" }]))
}

// ── jsonOf / fieldText ───────────────────────────────────────────────────────────────────────
fn fence_re() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"```[a-z]*\n?|```").unwrap())
}
fn brace_re() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"(?s)\{.*\}").unwrap())
}
pub fn jsonOf(text: &Value) -> Value {
    let t = fence_re().replace_all(&str_qq(Some(text)), "").trim().to_string();
    let raw = if t.starts_with('{') { t } else { brace_re().find(&t).map(|m| m.as_str().to_string()).unwrap_or_default() };
    match serde_json::from_str::<Value>(&raw) {
        Ok(v) => v,
        Err(_) => Value::Null,
    }
}
pub fn fieldText(res: &Value, field: &str) -> Value {
    let s = res.get("output_kv").and_then(|v| v.as_array()).and_then(|arr| {
        arr.iter().find(|o| o.get("field_name").and_then(|v| v.as_str()) == Some(field) && o.get("parent_output_id").map_or(true, |v| v.is_null()))
    }).and_then(|o| o.get("value_text")).cloned();
    json!(str_qq(s.as_ref()))
}
const WEIGHTS: [&str; 3] = ["low", "medium", "high"];

// ── gatherIntentContext — the whole context that fed the intent: mission + shared docs + the
// selected sources (resolved the same way the agents read them). db-reachable, fully ported.
pub fn gatherIntentContext(db: &Connection, session_id: i64, sources: &Value, context: &Value) -> Value {
    let sel: Vec<String> = match sources.as_array() {
        Some(a) if !a.is_empty() => a.iter().map(js_string).collect(),
        _ => vec!["mission".into(), "shared".into(), "digest".into()],
    };
    let mut parts: Vec<String> = Vec::new();
    let per = ((7000.0 / (sel.len().max(1) as f64)).floor() as usize).max(500);
    for kind in sel.iter().take(20) {
        let mut text = String::new();
        if kind.as_str() == "mission" {
            if let Ok(rows) = crate::db_json::query_rows(db, "SELECT mission_md FROM cockpit_sessions WHERE id = ?", &[json!(session_id)]) {
                text = str_qq(first(&rows).and_then(|r| r.get("mission_md").cloned()).as_ref());
            }
        } else if kind.as_str() == "shared" {
            if let Ok(rows) = crate::db_json::query_rows(db, "SELECT body_md FROM cockpit_blackboard WHERE session_id = ? AND kind = 'context' ORDER BY id DESC LIMIT 8", &[json!(session_id)]) {
                if let Some(arr) = rows.as_array() {
                    text = arr.iter().map(|r| str_qq(r.get("body_md"))).collect::<Vec<_>>().join("\n---\n");
                }
            }
        } else {
            text = str_qq(Some(&crate::cockpit__helpers_context_brief::resolveSourceText(db, &json!(kind), context)));
        }
        if !text.trim().is_empty() {
            parts.push(format!("【{kind}】\n{}", take_chars(&text, per)));
        }
    }
    let joined = parts.join("\n\n");
    let capped = take_chars(&joined, 8000);
    json!(if capped.is_empty() { "(no intent context selected)".to_string() } else { capped })
}

// ── renderShape — the shape tree into prose the tribunal reads. Pure, fully testable.
pub fn renderShape(tree: &Value) -> Value {
    if !tree.is_object() {
        return json!("");
    }
    let mut l: Vec<String> = Vec::new();
    for b in tree.get("branches").and_then(|v| v.as_array()).cloned().unwrap_or_default() {
        let title = b.get("title").cloned().unwrap_or(Value::Null);
        let key_v = b.get("key").cloned().unwrap_or(Value::Null);
        let header = if title.is_null() { js_string(&key_v) } else { js_string(&title) };
        l.push(format!("### {header} ({})", js_string(&key_v)));
        for (i, n) in b.get("nodes").and_then(|v| v.as_array()).cloned().unwrap_or_default().iter().enumerate() {
            let detail = n.get("detail");
            let suffix = match detail { Some(d) if !d.is_null() => format!(" — {}", js_string(d)), _ => String::new() };
            l.push(format!("{}. {}{}", i + 1, js_string(n.get("title").unwrap_or(&Value::Null)), suffix));
            if let Some(alts) = n.get("alts").and_then(|v| v.as_array()) {
                if !alts.is_empty() {
                    let chosen = n.get("chosen");
                    let chosen_idx = chosen.filter(|c| !c.is_null()).and_then(|c| c.as_i64());
                    match chosen_idx.and_then(|i| alts.get(i as usize)) {
                        Some(a) => l.push(format!("   → CHOSEN: {} ({})", js_string(a.get("title").unwrap_or(&Value::Null)), js_string(a.get("why").unwrap_or(&Value::Null)))),
                        None => l.push(format!("   → UNRESOLVED FORK: {}", alts.iter().map(|a| js_string(a.get("title").unwrap_or(&Value::Null))).collect::<Vec<_>>().join(" | "))),
                    }
                }
            }
            if let Some(files) = n.get("files").and_then(|v| v.as_array()) {
                if !files.is_empty() {
                    l.push(format!("   files: {}", files.iter().map(js_string).collect::<Vec<_>>().join(", ")));
                }
            }
        }
    }
    if let Some(phases) = tree.get("phases").and_then(|v| v.as_array()) {
        if !phases.is_empty() {
            l.push("Phases:".to_string());
            for f in phases {
                let refs = f.get("refs").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                l.push(format!("- {} → {}", js_string(f.get("title").unwrap_or(&Value::Null)), refs.iter().map(js_string).collect::<Vec<_>>().join(", ")));
            }
        }
    }
    json!(l.join("\n"))
}

// ── parseAdvocate — pure response-shaping (unreachable from the default export: aiCall always
// errs before this ever sees a real response), unit-tested directly with canned `res` values.
fn arg_salvage_re() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r#"(?s)\{[^{}]*?"claim"\s*:\s*"((?:[^"\\]|\\.)*)"[^{}]*?"support"\s*:\s*"((?:[^"\\]|\\.)*)"[^{}]*?\}"#).unwrap())
}
fn weight_re() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r#""weight"\s*:\s*"(low|medium|high)""#).unwrap())
}
pub fn parseAdvocate(res: &Value) -> Value {
    let raw = str_qq(Some(&fieldText(res, "advocate_json")));
    let v = jsonOf(&json!(raw));
    let mut args: Vec<Value> = v.get("arguments").and_then(|a| a.as_array()).map(|a| {
        a.iter().filter_map(|x| {
            let claim = take_chars(&js_string(x.get("claim").unwrap_or(&Value::Null)), 240);
            if claim.is_empty() { return None; }
            let support = take_chars(&js_string(x.get("support").unwrap_or(&Value::Null)), 500);
            let weight_v = x.get("weight").and_then(|w| w.as_str());
            let weight = if weight_v.map_or(false, |w| WEIGHTS.contains(&w)) { weight_v.unwrap().to_string() } else { "medium".to_string() };
            Some(json!({ "claim": claim, "support": support, "weight": weight }))
        }).take(6).collect()
    }).unwrap_or_default();
    if args.is_empty() {
        let mut salvaged: Vec<Value> = Vec::new();
        for cap in arg_salvage_re().captures_iter(&raw) {
            let whole = cap.get(0).map(|m| m.as_str()).unwrap_or("");
            let weight = weight_re().captures(whole).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()).unwrap_or_else(|| "medium".to_string());
            let claim = take_chars(&cap[1].replace("\\\"", "\""), 240);
            let support = take_chars(&cap[2].replace("\\\"", "\""), 500);
            salvaged.push(json!({ "claim": claim, "support": support, "weight": weight }));
        }
        args = salvaged.into_iter().take(6).collect();
    }
    json!({
        "stance": take_chars(&js_string(v.get("stance").unwrap_or(&Value::Null)), 400),
        "arguments": args,
        "strongest": take_chars(&js_string(v.get("strongest").unwrap_or(&Value::Null)), 300),
        "ai_call_id": nz(res.get("ai_call_id")).cloned().unwrap_or(Value::Null),
    })
}

// advocate — PARITY-DEFERRED: dispatches through crate::ai_ai_call::aiCall, which always returns
// Err (see that module's doc), so this always propagates an Err once ensure_advocate_prompt
// (real, ported) has run. The retry-on-empty-arguments branch is unreachable but ported faithfully.
fn advocate(db: &Connection, side: &str, goal: &str, case_file: &str, provider_fn: &Value) -> Result<Value, String> {
    let call = |bypass: bool| -> Result<Value, String> {
        let input = json!({
            "prompt_name": PROMPT_ADV, "tier": "mid",
            "input_kv": [{ "key": "side", "value": side }, { "key": "goal", "value": goal }, { "key": "case_file", "value": case_file }],
            "provider_fn": provider_fn, "bypass_cache": bypass,
        });
        crate::ai_ai_call::aiCall(&input, db)
    };
    let mut out = parseAdvocate(&call(false)?);
    if out.get("arguments").and_then(|a| a.as_array()).map_or(true, |a| a.is_empty()) {
        let retry = parseAdvocate(&call(true)?);
        if retry.get("arguments").and_then(|a| a.as_array()).map_or(false, |a| !a.is_empty()) {
            out = retry;
        }
    }
    Ok(out)
}

// ── buildRuling / lintRuling — the ONE parser+salvage for a raw ruling string, shared by the
// HIGH judge and the LOW formatter. Pure, unreachable from the default export, unit-tested directly.
const VERDICTS: [&str; 3] = ["sound", "sound_with_revisions", "not_ready"];
fn clean_str_re() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"\\+$").unwrap())
}
pub fn cleanStr(s: &Value) -> Value {
    let t = str_qq(Some(s)).replace("\\\"", "\"").replace("\\n", " ");
    json!(clean_str_re().replace(&t, "").trim().to_string())
}
fn salv_str(jraw: &str, key: &str) -> Option<String> {
    let re = regex::Regex::new(&format!(r#""{key}"\s*:\s*"((?:[^"\\]|\\.)*)""#)).ok()?;
    re.captures(jraw).map(|c| str_qq(Some(&cleanStr(&json!(c[1].to_string())))))
}
fn salv_num(jraw: &str, key: &str) -> Option<f64> {
    let re = regex::Regex::new(&format!(r#""{key}"\s*:\s*(\d+)"#)).ok()?;
    re.captures(jraw).and_then(|c| c[1].parse::<f64>().ok())
}
fn salv_arr(jraw: &str, key: &str) -> Vec<String> {
    let re = match regex::Regex::new(&format!(r#"(?s)"{key}"\s*:\s*\[(.*?)(?:\]|$)"#)) { Ok(r) => r, Err(_) => return vec![] };
    let inner = match re.captures(jraw) { Some(c) => c[1].to_string(), None => return vec![] };
    let item_re = regex::Regex::new(r#""((?:[^"\\]|\\.)*)""#).unwrap();
    item_re.captures_iter(&inner).map(|c| str_qq(Some(&cleanStr(&json!(c[1].to_string()))))).filter(|s| !s.is_empty()).collect()
}
fn str_list(jv: &Value, jv_strict_ok: bool, jraw: &str, key: &str, cap: usize, n: usize) -> Vec<String> {
    let base: Vec<String> = if let Some(arr) = jv.get(key).and_then(|v| v.as_array()) {
        arr.iter().map(js_string).collect()
    } else if jv_strict_ok {
        vec![]
    } else {
        salv_arr(jraw, key)
    };
    base.into_iter().map(|x| take_chars(&x, cap)).filter(|x| !x.is_empty()).take(n).collect()
}
pub fn buildRuling(jraw: &Value, ai_call_id: &Value) -> Value {
    let jraw = str_qq(Some(jraw));
    let jv_strict = jsonOf(&json!(jraw));
    let jv_strict_ok = !jv_strict.is_null();
    let jv = if jv_strict_ok { jv_strict.clone() } else { json!({}) };

    let op_full = regex::Regex::new(r#"(?s)"opinion"\s*:\s*"((?:[^"\\]|\\.)*)""#).unwrap().captures(&jraw).map(|c| c[1].to_string());
    let op_part = regex::Regex::new(r#"(?s)"opinion"\s*:\s*"((?:[^"\\]|\\.)*)$"#).unwrap().captures(&jraw).map(|c| c[1].to_string());
    let opinion_salv = if let Some(o) = jv.get("opinion") { js_string(o) }
        else if let Some(f) = &op_full { str_qq(Some(&cleanStr(&json!(f.clone())))) }
        else if let Some(p) = &op_part { str_qq(Some(&cleanStr(&json!(p.clone())))) }
        else { String::new() };

    let dim_from_obj = |d: &Value| -> Option<Value> {
        let name = take_chars(&js_string(d.get("name").unwrap_or(&Value::Null)), 40);
        if name.is_empty() { return None; }
        // JS: `Math.max(0, Math.min(100, Math.round(Number(d?.score ?? 50)) || 0))` — default 50 when
        // absent, but a rounded-to-zero/unparseable result falls through to 0 (not 50, unlike confidence).
        let num = match nz(d.get("score")) { Some(s) => js_number(Some(s)), None => 50.0 };
        let rounded = num.round();
        let effective = if rounded == 0.0 || rounded.is_nan() { 0.0 } else { rounded };
        let score = effective.max(0.0).min(100.0) as i64;
        Some(json!({ "name": name, "score": score, "note": take_chars(&js_string(d.get("note").unwrap_or(&Value::Null)), 200) }))
    };
    let mut dimensions: Vec<Value> = jv.get("dimensions").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(dim_from_obj).take(6).collect()).unwrap_or_default();
    if dimensions.is_empty() && !jv_strict_ok {
        let re = regex::Regex::new(r#"(?s)\{[^{}]*?"name"\s*:\s*"([^"]+)"[^{}]*?"score"\s*:\s*(\d+)[^{}]*?\}"#).unwrap();
        dimensions = re.captures_iter(&jraw).map(|c| {
            let name = take_chars(&str_qq(Some(&cleanStr(&json!(c[1].to_string())))), 40);
            let score: i64 = c[2].parse::<i64>().unwrap_or(0).max(0).min(100);
            json!({ "name": name, "score": score, "note": "" })
        }).take(6).collect();
    }

    let verdict_salv = salv_str(&jraw, "verdict").unwrap_or_default();
    let verdict_from_jv = jv.get("verdict").and_then(|v| v.as_str());
    let verdict = if verdict_from_jv.map_or(false, |v| VERDICTS.contains(&v)) {
        verdict_from_jv.unwrap().to_string()
    } else if VERDICTS.contains(&verdict_salv.as_str()) {
        verdict_salv
    } else {
        "sound_with_revisions".to_string()
    };
    // JS: `Math.max(0, Math.min(100, Math.round(Number(jv.confidence ?? salvNum('confidence') ?? 50)) || 50))`
    // — `0 || 50` and `NaN || 50` both fall through to 50 (JS falsy-OR quirk), so a rounded-to-zero
    // or unparseable confidence becomes 50, not 0.
    let confidence_raw = match jv.get("confidence") { Some(c) => js_number(Some(c)), None => salv_num(&jraw, "confidence").unwrap_or(50.0) };
    let rounded = confidence_raw.round();
    let effective = if rounded == 0.0 || rounded.is_nan() { 50.0 } else { rounded };
    let confidence = effective.max(0.0).min(100.0) as i64;
    let headline_raw = if let Some(h) = jv.get("headline") { js_string(h) } else { salv_str(&jraw, "headline").unwrap_or_default() };
    let headline = { let t = take_chars(&headline_raw, 240); if t.is_empty() { "the tribunal reached a ruling".to_string() } else { t } };

    let map_add = |c: &Value| -> Option<Value> {
        let title = take_chars(&js_string(c.get("title").unwrap_or(&Value::Null)), 90);
        if title.is_empty() { return None; }
        Some(json!({ "branch": take_chars(&js_string(c.get("branch").unwrap_or(&Value::Null)), 20), "title": title, "detail": take_chars(&js_string(c.get("detail").unwrap_or(&Value::Null)), 200), "why": take_chars(&js_string(c.get("why").unwrap_or(&Value::Null)), 240) }))
    };
    let map_update = |c: &Value| -> Option<Value> {
        let target = take_chars(&js_string(c.get("target").unwrap_or(&Value::Null)), 120);
        let change = take_chars(&js_string(c.get("change").unwrap_or(&Value::Null)), 240);
        if target.is_empty() || change.is_empty() { return None; }
        Some(json!({ "target": target, "change": change, "why": take_chars(&js_string(c.get("why").unwrap_or(&Value::Null)), 240) }))
    };
    let map_remove = |c: &Value| -> Option<Value> {
        let target = take_chars(&js_string(c.get("target").unwrap_or(&Value::Null)), 120);
        if target.is_empty() { return None; }
        Some(json!({ "target": target, "why": take_chars(&js_string(c.get("why").unwrap_or(&Value::Null)), 240) }))
    };
    let changes = jv.get("changes").cloned().unwrap_or(Value::Null);
    let add: Vec<Value> = changes.get("add").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(map_add).take(5).collect()).unwrap_or_default();
    let update: Vec<Value> = changes.get("update").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(map_update).take(6).collect()).unwrap_or_default();
    let remove: Vec<Value> = changes.get("remove").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(map_remove).take(4).collect()).unwrap_or_default();

    let preserve: Vec<String> = jv.get("preserve").and_then(|v| v.as_array()).map(|a| a.iter().map(|x| take_chars(&js_string(x), 240)).filter(|x| !x.is_empty()).take(4).collect()).unwrap_or_default();
    let decisive_factor = take_chars(&if let Some(d) = jv.get("decisive_factor") { js_string(d) } else { salv_str(&jraw, "decisive_factor").unwrap_or_default() }, 300);

    json!({
        "verdict": verdict,
        "confidence": confidence,
        "headline": headline,
        "opinion": take_chars(&opinion_salv, 1600),
        "evidence": str_list(&jv, jv_strict_ok, &jraw, "evidence", 220, 5),
        "insights": str_list(&jv, jv_strict_ok, &jraw, "insights", 220, 4),
        "to_approve": str_list(&jv, jv_strict_ok, &jraw, "to_approve", 200, 4),
        "parse_ok": jv_strict_ok,
        "dimensions": dimensions,
        "changes": { "add": add, "update": update, "remove": remove },
        "preserve": preserve,
        "decisive_factor": decisive_factor,
        "ai_call_id": nz(Some(ai_call_id)).cloned().unwrap_or(Value::Null),
        "_raw": jraw,
    })
}
pub fn lintRuling(j: &Value) -> Value {
    let mut issues: Vec<&str> = Vec::new();
    let verdict_ok = j.get("verdict").and_then(|v| v.as_str()).map_or(false, |v| VERDICTS.contains(&v));
    if !verdict_ok { issues.push("verdict missing/invalid"); }
    let has_opinion = j.get("opinion").and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty());
    let has_evidence = j.get("evidence").and_then(|v| v.as_array()).map_or(false, |a| !a.is_empty());
    let has_insights = j.get("insights").and_then(|v| v.as_array()).map_or(false, |a| !a.is_empty());
    if !(has_opinion || has_evidence || has_insights) { issues.push("no reasoning: needs an opinion, evidence or insights"); }
    json!({ "ok": issues.is_empty(), "issues": issues })
}

// format_ruling — PARITY-DEFERRED: dispatches through aiCall (always Err) once ensure_format_prompt
// (real, ported) has run.
fn format_ruling(db: &Connection, raw: &str, provider_fn: &Value) -> Result<Option<Value>, String> {
    if raw.trim().is_empty() { return Ok(None); }
    ensure_format_prompt(db)?;
    let input = json!({
        "prompt_name": PROMPT_FMT, "tier": "low",
        "input_kv": [{ "key": "raw", "value": take_chars(raw, 9000) }],
        "provider_fn": provider_fn,
    });
    let res = crate::ai_ai_call::aiCall(&input, db)?;
    Ok(Some(buildRuling(&fieldText(&res, "ruling_json"), res.get("ai_call_id").unwrap_or(&Value::Null))))
}

// ── parseSkeletonFiles — the skeleton JSON → normalized file list. Pure, unreachable from the
// default export, unit-tested directly.
fn skel_salvage_re() -> &'static regex::Regex {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r#"(?s)\{[^{}]*?"path"\s*:\s*"([^"]+)"[^{}]*?\}"#).unwrap())
}
pub fn parseSkeletonFiles(sraw: &Value) -> Value {
    let sraw = str_qq(Some(sraw));
    let cleaned = fence_re().replace_all(&sraw, "").to_string();
    let mut files: Vec<Value> = brace_re().find(&cleaned).and_then(|m| serde_json::from_str::<Value>(m.as_str()).ok())
        .and_then(|v| v.get("files").and_then(|f| f.as_array()).cloned())
        .unwrap_or_default();
    if files.is_empty() {
        for cap in skel_salvage_re().captures_iter(&sraw) {
            let whole = cap.get(0).map(|m| m.as_str()).unwrap_or("");
            let action = regex::Regex::new(r#""action"\s*:\s*"(create|modify)""#).unwrap().captures(whole).map(|c| c[1].to_string());
            let intent = regex::Regex::new(r#""intent"\s*:\s*"((?:[^"\\]|\\.)*)""#).unwrap().captures(whole).map(|c| c[1].to_string());
            files.push(json!({ "path": cap[1].to_string(), "action": action, "intent": intent.unwrap_or_default() }));
        }
    }
    let mut seen = std::collections::HashSet::new();
    let out: Vec<Value> = files.into_iter().filter_map(|f| {
        let path = take_chars(&js_string(f.get("path").unwrap_or(&Value::Null)).replace('\\', "/").trim(), 200);
        if path.is_empty() || !seen.insert(path.clone()) { return None; }
        let action = if f.get("action").and_then(|v| v.as_str()) == Some("create") { "create" } else { "modify" };
        Some(json!({ "path": path, "action": action, "intent": take_chars(&js_string(f.get("intent").unwrap_or(&Value::Null)), 300) }))
    }).take(6).collect();
    json!(out)
}

pub fn cockpitPlanTribunal(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent); provider_fn threaded as null.
    let provider_fn = Value::Null;
    let _ = crate::cockpit_cockpit_schema::ensureCockpitSchema(db, &json!(""));
    let session_id_num = js_number(input.get("session_id"));
    if session_id_num.is_nan() || session_id_num == 0.0 {
        return Err("cockpit-plan-tribunal: session_id is required".to_string());
    }
    let session_id = session_id_num as i64;
    let mode = input.get("mode").and_then(|v| v.as_str()).unwrap_or("");

    // ── read-only mode: complete history, newest first — no AI, no spend ──
    if mode == "tribunal-history" {
        let rows = crate::db_json::query_rows(db, "SELECT id, verdict, headline, case_json, created_at FROM cockpit_tribunals WHERE session_id = ? ORDER BY id DESC LIMIT 40", &[json!(session_id)])?;
        let sessions: Vec<Value> = rows.as_array().cloned().unwrap_or_default().into_iter().filter_map(|r| {
            let c: Value = serde_json::from_str(&str_qq(r.get("case_json"))).ok()?;
            Some(json!({ "id": r.get("id"), "verdict": r.get("verdict"), "headline": r.get("headline"), "created_at": r.get("created_at"), "case": c }))
        }).collect();
        return Ok(json!({ "ok": true, "sessions": sessions }));
    }

    // ── read-only mode: the saved CODE BLUEPRINT design — no AI, no spend ──
    if mode == "tribunal-blueprint" {
        let rows = crate::db_json::query_rows(db, "SELECT blueprint_json FROM ultra_plans WHERE session_id = ? ORDER BY id DESC LIMIT 1", &[json!(session_id)])?;
        let design = first(&rows).and_then(|r| r.get("blueprint_json").cloned()).filter(|v| !v.is_null())
            .and_then(|v| serde_json::from_str::<Value>(&str_qq(Some(&v))).ok());
        if let Some(design) = design {
            return Ok(json!({ "ok": true, "design": design }));
        }
        return Ok(json!({ "ok": true, "needs_design": true, "note": "no code blueprint yet — ⚡ design it: the architect draws the file spine, writes the stubs, and parses them into the planned graph + delta" }));
    }

    // ── CODE BLUEPRINT design — PARITY-DEFERRED past the skeleton aiCall call ──
    if mode == "tribunal-design" {
        let base_plan_rows = crate::db_json::query_rows(db, "SELECT body_md FROM cockpit_blackboard WHERE session_id = ? AND pane_id IS NULL AND kind = 'baseplan' ORDER BY id DESC LIMIT 1", &[json!(session_id)])?;
        let base_plan = str_qq(first(&base_plan_rows).and_then(|r| r.get("body_md").cloned()).as_ref()).trim().to_string();
        let shape_rows = crate::db_json::query_rows(db, "SELECT body_md FROM cockpit_blackboard WHERE session_id = ? AND kind = 'shape' ORDER BY id DESC LIMIT 1", &[json!(session_id)])?;
        let shape_tree: Value = first(&shape_rows).and_then(|r| r.get("body_md").cloned()).and_then(|v| serde_json::from_str::<Value>(&str_qq(Some(&v))).ok()).unwrap_or(Value::Null);
        let plan_text = take_chars(&[base_plan.clone(), str_qq(Some(&renderShape(&shape_tree)))].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n"), 4500);
        if plan_text.is_empty() {
            return Ok(json!({ "ok": false, "error": "no plan to design from — forge a base plan / grow the shape tree first" }));
        }
        let mut corrections = "(no ruling yet — design from the plan alone)".to_string();
        if let Ok(tr_rows) = crate::db_json::query_rows(db, "SELECT case_json FROM cockpit_tribunals WHERE session_id = ? ORDER BY id DESC LIMIT 1", &[json!(session_id)]) {
            if let Some(tr) = first(&tr_rows) {
                if let Some(case) = serde_json::from_str::<Value>(&str_qq(tr.get("case_json"))).ok() {
                    let j = case.get("judge").cloned().unwrap_or(Value::Null);
                    let mut ls: Vec<String> = Vec::new();
                    for x in j.get("to_approve").and_then(|v| v.as_array()).cloned().unwrap_or_default() { ls.push(format!("- {}", js_string(&x))); }
                    for c in j.get("changes").and_then(|c| c.get("add")).and_then(|v| v.as_array()).cloned().unwrap_or_default() { ls.push(format!("- add \"{}\" ({}) — {}", js_string(c.get("title").unwrap_or(&Value::Null)), js_string(c.get("branch").unwrap_or(&Value::Null)), js_string(c.get("why").unwrap_or(&Value::Null)))); }
                    for c in j.get("changes").and_then(|c| c.get("update")).and_then(|v| v.as_array()).cloned().unwrap_or_default() { ls.push(format!("- update \"{}\" → {}", js_string(c.get("target").unwrap_or(&Value::Null)), js_string(c.get("change").unwrap_or(&Value::Null)))); }
                    for c in j.get("changes").and_then(|c| c.get("remove")).and_then(|v| v.as_array()).cloned().unwrap_or_default() { ls.push(format!("- remove \"{}\" — {}", js_string(c.get("target").unwrap_or(&Value::Null)), js_string(c.get("why").unwrap_or(&Value::Null)))); }
                    if !ls.is_empty() { corrections = take_chars(&ls.join("\n"), 1600); }
                }
            }
        }
        let digest = str_qq(Some(&crate::cockpit__helpers_context_brief::resolveSourceText(db, &json!("digest"), &Value::Null)));
        let ast = str_qq(Some(&crate::cockpit__helpers_context_brief::resolveSourceText(db, &json!("ast"), &Value::Null)));
        let grounding = { let g = [take_chars(&digest, 2200), take_chars(&ast, 3200)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n"); if g.is_empty() { "(atlas not onboarded — create-only design)".to_string() } else { g } };

        ensure_skeleton_prompt(db)?;
        let skel_input = json!({
            "prompt_name": PROMPT_SKEL, "tier": "mid",
            "input_kv": [{ "key": "plan", "value": plan_text }, { "key": "corrections", "value": corrections }, { "key": "grounding", "value": grounding }],
            "provider_fn": provider_fn,
        });
        // PARITY-DEFERRED — aiCall always errs; everything below is unreachable from this call
        // site but ported faithfully (parseSkeletonFiles / the per-file stub loop are pure/testable
        // separately, mirroring book-run.rs's treatment of its own post-aiCall logic).
        let sres = crate::ai_ai_call::aiCall(&skel_input, db)?;
        let sraw = fieldText(&sres, "skeleton_json");
        let files = parseSkeletonFiles(&sraw);
        let files_arr = files.as_array().cloned().unwrap_or_default();
        if files_arr.is_empty() {
            return Ok(json!({ "ok": false, "error": "the skeleton drew no files — try again", "ai_call_id": sres.get("ai_call_id").cloned().unwrap_or(Value::Null) }));
        }

        ensure_stub_prompt(db)?;
        let mut stub_calls: Vec<Value> = Vec::new();
        let mut stub_files: Vec<Value> = Vec::new();
        for f in &files_arr {
            let path = str_qq(f.get("path"));
            let action = str_qq(f.get("action"));
            let mut current = "(new file)".to_string();
            if let Ok(syms_rows) = crate::db_json::query_rows(db, "SELECT name, type, signature FROM code.code_nodes WHERE origin NOT LIKE 'blueprint:%' AND path = ? AND type != 'file' ORDER BY id LIMIT 14", &[json!(path.clone())]) {
                if let Some(syms) = syms_rows.as_array() {
                    if !syms.is_empty() {
                        current = syms.iter().map(|s| {
                            let sig = str_qq(s.get("signature"));
                            format!("{} {}{}", str_qq(s.get("type")), str_qq(s.get("name")), if sig.is_empty() { String::new() } else { format!(" {sig}") })
                        }).collect::<Vec<_>>().join("\n");
                    } else if action == "modify" {
                        current = "(no symbols indexed for this file)".to_string();
                    }
                }
            }
            let intent = { let i = str_qq(f.get("intent")); if i.is_empty() { "(see the plan)".to_string() } else { i } };
            let call_stub = |bypass: bool| -> Result<Value, String> {
                let input = json!({
                    "prompt_name": PROMPT_STUB, "tier": "low",
                    "input_kv": [{ "key": "path", "value": path }, { "key": "action", "value": action }, { "key": "intent", "value": intent }, { "key": "plan", "value": take_chars(&plan_text, 1800) }, { "key": "current", "value": current }],
                    "provider_fn": provider_fn, "bypass_cache": bypass,
                });
                crate::ai_ai_call::aiCall(&input, db)
            };
            let mut r1 = call_stub(false);
            let mut code_and_ai_call_id: (String, Value) = match &r1 {
                Ok(res) => (str_qq(Some(&crate::blueprint__helpers_code_checks::extractRawCode(&fieldText(res, "stub_source")))), res.get("ai_call_id").cloned().unwrap_or(Value::Null)),
                Err(_) => (String::new(), Value::Null),
            };
            let mut acc = crate::blueprint__helpers_author_accept::acceptAuthoredCode(db, &json!({ "code": code_and_ai_call_id.0, "path": path, "ai_call_id": code_and_ai_call_id.1 }));
            if !acc.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                r1 = call_stub(true);
                code_and_ai_call_id = match &r1 {
                    Ok(res) => (str_qq(Some(&crate::blueprint__helpers_code_checks::extractRawCode(&fieldText(res, "stub_source")))), res.get("ai_call_id").cloned().unwrap_or(Value::Null)),
                    Err(_) => (String::new(), Value::Null),
                };
                acc = crate::blueprint__helpers_author_accept::acceptAuthoredCode(db, &json!({ "code": code_and_ai_call_id.0, "path": path, "ai_call_id": code_and_ai_call_id.1 }));
            }
            stub_calls.push(r1.ok().and_then(|res| res.get("ai_call_id").cloned()).unwrap_or(Value::Null));
            let source = if !code_and_ai_call_id.0.is_empty() { code_and_ai_call_id.0 } else { format!("// stub rejected ({}) — redesign to fill this file", str_qq(acc.get("reason"))) };
            stub_files.push(json!({ "path": path, "source": source }));
        }

        let parsed = crate::blueprint_blueprint_parse::blueprintParse(&json!({ "stub_files": stub_files }), db)?;
        let design = json!({
            "designed_at": crate::clock::now_iso_nodb(),
            "files": files_arr, "stub_files": stub_files,
            "planned_nodes": parsed.get("nodes").cloned().unwrap_or(json!([])),
            "planned_edges": parsed.get("edges").cloned().unwrap_or(json!([])),
            "delta": parsed.get("delta").cloned().unwrap_or(Value::Null),
            "parse_errors": parsed.get("errors").cloned().unwrap_or(json!([])),
            "ai_calls": { "skeleton": sres.get("ai_call_id").cloned().unwrap_or(Value::Null), "stubs": stub_calls },
        });
        let plan_row_rows = crate::db_json::query_rows(db, "SELECT id FROM ultra_plans WHERE session_id = ? ORDER BY id DESC LIMIT 1", &[json!(session_id)])?;
        let body = take_chars(&design.to_string(), 200000);
        if let Some(pr) = first(&plan_row_rows) {
            crate::db_json::run_write(db, "UPDATE ultra_plans SET blueprint_json = ?, updated_at = datetime('now') WHERE id = ?", &[json!(body), pr.get("id").cloned().unwrap_or(Value::Null)])?;
        } else {
            crate::db_json::run_write(db, "INSERT INTO ultra_plans (session_id, blueprint_json, updated_at) VALUES (?, ?, datetime('now'))", &[json!(session_id), json!(body)])?;
        }
        return Ok(json!({ "ok": true, "design": design }));
    }

    // ── the FULL PLAN: the forged base plan (session-level) + the shape tree ──
    let base_plan_rows = crate::db_json::query_rows(db, "SELECT body_md FROM cockpit_blackboard WHERE session_id = ? AND pane_id IS NULL AND kind = 'baseplan' ORDER BY id DESC LIMIT 1", &[json!(session_id)])?;
    let base_plan = str_qq(first(&base_plan_rows).and_then(|r| r.get("body_md").cloned()).as_ref()).trim().to_string();
    let shape_rows = crate::db_json::query_rows(db, "SELECT body_md FROM cockpit_blackboard WHERE session_id = ? AND kind = 'shape' ORDER BY id DESC LIMIT 1", &[json!(session_id)])?;
    let shape_tree: Value = first(&shape_rows).and_then(|r| r.get("body_md").cloned()).and_then(|v| serde_json::from_str::<Value>(&str_qq(Some(&v))).ok()).unwrap_or(Value::Null);
    let shape_text = str_qq(Some(&renderShape(&shape_tree)));
    if base_plan.is_empty() && shape_text.is_empty() {
        return Ok(json!({ "ok": false, "error": "no plan to try — forge a base plan and grow the shape tree first" }));
    }

    let intent_context = str_qq(Some(&gatherIntentContext(db, session_id, input.get("sources").unwrap_or(&Value::Null), &Value::Null)));
    let case_file = take_chars(&[
        "# CASE FILE — the ultra plan under review".to_string(), String::new(),
        "## THE INTENT CONTEXT (what informed this plan)".to_string(), intent_context, String::new(),
        "## THE BASE PLAN (the forged artifact)".to_string(), if base_plan.is_empty() { "(none forged)".to_string() } else { base_plan.clone() }, String::new(),
        "## THE FULL PLAN — the shape decomposition (branches · microtasks · decisions · phases)".to_string(), if shape_text.is_empty() { "(no shape tree yet)".to_string() } else { shape_text },
    ].join("\n"), 16000);

    // PARITY-DEFERRED from here — advocate() always propagates aiCall's Err once
    // ensure_advocate_prompt (real, ported) has run.
    ensure_advocate_prompt(db)?;
    let defender = advocate(db, "THE DEFENSE", "Argue this ultra plan is SOUND, sufficient and ready to execute. Surface its real strengths and why the chosen decisions are right.", &case_file, &provider_fn)?;
    let prosecutor = advocate(db, "THE PROSECUTION", "Argue this ultra plan is INSUFFICIENT, risky or premature. Surface the gaps, unaddressed risks, weak/unresolved decisions and hidden complexity.", &case_file, &provider_fn)?;

    // unreachable past the defender/prosecutor calls above, but ported faithfully.
    ensure_judge_prompt(db)?;
    let plea_text = |a: &Value| -> String {
        let stance = str_qq(a.get("stance"));
        let mut lines: Vec<String> = if !stance.is_empty() { vec![stance] } else { vec![] };
        for (i, x) in a.get("arguments").and_then(|v| v.as_array()).cloned().unwrap_or_default().iter().enumerate() {
            lines.push(format!("{}. {} — {} [{}]", i + 1, str_qq(x.get("claim")), str_qq(x.get("support")), str_qq(x.get("weight"))));
        }
        let strongest = str_qq(a.get("strongest"));
        if !strongest.is_empty() { lines.push(format!("STRONGEST: {strongest}")); }
        take_chars(&lines.join("\n"), 4000)
    };
    let defense_text = { let t = plea_text(&defender); if t.is_empty() { "(the defense submitted no usable arguments — weigh the plan on its own merits)".to_string() } else { t } };
    let prosecution_text = { let t = plea_text(&prosecutor); if t.is_empty() { "(the prosecution submitted no usable arguments — scrutinise the plan yourself for the gaps and risks)".to_string() } else { t } };
    let run_judge = |bypass: bool| -> Result<Value, String> {
        let input = json!({
            "prompt_name": PROMPT_JUDGE, "tier": "high",
            "input_kv": [{ "key": "case_file", "value": case_file }, { "key": "defense", "value": defense_text }, { "key": "prosecution", "value": prosecution_text }],
            "provider_fn": provider_fn, "bypass_cache": bypass,
        });
        let jres = crate::ai_ai_call::aiCall(&input, db)?;
        Ok(buildRuling(&fieldText(&jres, "ruling_json"), jres.get("ai_call_id").unwrap_or(&Value::Null)))
    };
    let mut judge = run_judge(false)?;
    if !lintRuling(&judge).get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        let raw = str_qq(judge.get("_raw"));
        let fmt = format_ruling(db, &raw, &provider_fn).unwrap_or(None);
        judge = match fmt {
            Some(f) if lintRuling(&f).get("ok").and_then(|v| v.as_bool()).unwrap_or(false) => f,
            _ => run_judge(true)?,
        };
    }
    if let Some(o) = judge.as_object_mut() { o.remove("_raw"); }

    let meta = json!({
        "session_id": session_id, "tried_at": crate::clock::now_iso_nodb(),
        "has_base_plan": !base_plan.is_empty(),
        "branches": shape_tree.get("branches").and_then(|v| v.as_array()).map_or(0, |a| a.len()),
    });
    let case_obj = json!({ "defender": defender, "prosecutor": prosecutor, "judge": judge, "meta": meta });

    let mut tribunal_id: Value = Value::Null;
    if let Ok(plan_id_rows) = crate::db_json::query_rows(db, "SELECT id FROM ultra_plans WHERE session_id = ? ORDER BY id DESC LIMIT 1", &[json!(session_id)]) {
        let plan_id = first(&plan_id_rows).and_then(|r| r.get("id").cloned()).unwrap_or(Value::Null);
        let headline = take_chars(&str_qq(judge.get("headline")), 200);
        let case_json = take_chars(&case_obj.to_string(), 60000);
        if let Ok((_, rowid)) = crate::db_json::run_write(db, "INSERT INTO cockpit_tribunals (session_id, ultra_plan_id, verdict, headline, case_json) VALUES (?,?,?,?,?)", &[json!(session_id), plan_id, judge.get("verdict").cloned().unwrap_or(Value::Null), json!(headline), json!(case_json)]) {
            tribunal_id = json!(rowid);
        }
    }
    let n_items = defender.get("arguments").and_then(|a| a.as_array()).map_or(0, |a| a.len()) + prosecutor.get("arguments").and_then(|a| a.as_array()).map_or(0, |a| a.len());
    let _ = crate::db_json::run_write(
        db,
        "INSERT INTO judgments.judgment_cases (card_slug, scope_kind, scope_ref, goal_line, n_items, verdict, created_at, updated_at) VALUES (?,?,?,?,?,?, datetime('now'), datetime('now'))",
        &[json!(format!("VIBE-SESSION-{session_id}")), json!("vibe_plan"), json!(format!("session:{session_id}:tribunal")), json!(take_chars(&str_qq(judge.get("headline")), 200)), json!(n_items as i64), judge.get("verdict").cloned().unwrap_or(Value::Null)],
    );

    Ok(json!({
        "ok": true, "verdict": judge.get("verdict").cloned().unwrap_or(Value::Null), "tribunal_id": tribunal_id,
        "case": case_obj,
        "ai_calls": { "defender": defender.get("ai_call_id").cloned().unwrap_or(Value::Null), "prosecutor": prosecutor.get("ai_call_id").cloned().unwrap_or(Value::Null), "judge": judge.get("ai_call_id").cloned().unwrap_or(Value::Null) },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS code;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE judgments.judgment_cases ( id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, scope_kind TEXT, scope_ref TEXT, goal_line TEXT, n_items INTEGER, verdict TEXT, created_at TEXT, updated_at TEXT );
             CREATE TABLE code.code_nodes ( id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT );",
        ).unwrap();
        crate::cockpit_cockpit_schema::ensureCockpitSchema(&c, &json!(""));
        c
    }

    // ── dispatch guards ─────────────────────────────────────────────────────
    #[test]
    fn session_id_is_required() {
        let c = setup();
        let err = cockpitPlanTribunal(&json!({}), &c).unwrap_err();
        assert!(err.contains("session_id is required"));
    }

    #[test]
    fn no_plan_returns_ok_false() {
        let c = setup();
        c.execute("INSERT INTO cockpit_sessions (name) VALUES ('s')", []).unwrap();
        let r = cockpitPlanTribunal(&json!({ "session_id": 1 }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("no plan to try"));
    }

    // ── tribunal-history (read-only, no AI) ────────────────────────────────
    #[test]
    fn tribunal_history_returns_parsed_sessions_newest_first() {
        let c = setup();
        c.execute("INSERT INTO cockpit_sessions (name) VALUES ('s')", []).unwrap();
        c.execute("INSERT INTO cockpit_tribunals (session_id, verdict, headline, case_json) VALUES (1, 'sound', 'h1', '{\"a\":1}')", []).unwrap();
        c.execute("INSERT INTO cockpit_tribunals (session_id, verdict, headline, case_json) VALUES (1, 'not_ready', 'h2', 'not json')", []).unwrap();
        let r = cockpitPlanTribunal(&json!({ "session_id": 1, "mode": "tribunal-history" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let sessions = r["sessions"].as_array().unwrap();
        // the malformed case_json row is filtered out; newest (id DESC) sorts first pre-filter
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["headline"], json!("h1"));
        assert_eq!(sessions[0]["case"], json!({"a":1}));
    }

    // ── tribunal-blueprint (read-only, no AI) ──────────────────────────────
    #[test]
    fn tribunal_blueprint_needs_design_when_absent() {
        let c = setup();
        let r = cockpitPlanTribunal(&json!({ "session_id": 7, "mode": "tribunal-blueprint" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["needs_design"], json!(true));
    }

    #[test]
    fn tribunal_blueprint_returns_saved_design() {
        let c = setup();
        c.execute("INSERT INTO cockpit_sessions (name) VALUES ('s')", []).unwrap();
        c.execute("INSERT INTO ultra_plans (session_id, blueprint_json) VALUES (1, '{\"files\":[]}')", []).unwrap();
        let r = cockpitPlanTribunal(&json!({ "session_id": 1, "mode": "tribunal-blueprint" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["design"], json!({"files":[]}));
    }

    // ── deferred boundary — proves the deterministic gates ran before the Err ──
    #[test]
    fn full_tribunal_reaches_the_parity_deferred_aicall_boundary() {
        let c = setup();
        c.execute("INSERT INTO cockpit_sessions (name) VALUES ('s')", []).unwrap();
        c.execute("INSERT INTO cockpit_blackboard (session_id, kind, body_md) VALUES (1, 'baseplan', 'do the thing')", []).unwrap();
        let err = cockpitPlanTribunal(&json!({ "session_id": 1 }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        // the advocate prompt WAS ensured (ensure_advocate_prompt ran) before the deferred call
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'cockpit-plan-advocate'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn tribunal_design_reaches_the_parity_deferred_aicall_boundary() {
        let c = setup();
        c.execute("INSERT INTO cockpit_sessions (name) VALUES ('s')", []).unwrap();
        c.execute("INSERT INTO cockpit_blackboard (session_id, kind, body_md) VALUES (1, 'baseplan', 'do the thing')", []).unwrap();
        let err = cockpitPlanTribunal(&json!({ "session_id": 1, "mode": "tribunal-design" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'cockpit-tribunal-skeleton'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn tribunal_design_requires_a_plan() {
        let c = setup();
        c.execute("INSERT INTO cockpit_sessions (name) VALUES ('s')", []).unwrap();
        let r = cockpitPlanTribunal(&json!({ "session_id": 1, "mode": "tribunal-design" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("no plan to design from"));
    }

    // ── renderShape ─────────────────────────────────────────────────────────
    #[test]
    fn render_shape_empty_tree() {
        assert_eq!(renderShape(&Value::Null), json!(""));
    }

    #[test]
    fn render_shape_renders_branches_alts_files_and_phases() {
        let tree = json!({
            "branches": [{
                "key": "scope", "title": "Scope",
                "nodes": [
                    { "title": "Node A", "detail": "do it", "alts": [{ "title": "opt1", "why": "cheap" }], "chosen": 0, "files": ["a.rs", "b.rs"] },
                    { "title": "Node B", "alts": [{ "title": "x" }, { "title": "y" }] },
                ],
            }],
            "phases": [{ "title": "Phase 1", "refs": ["scope"] }],
        });
        let s = renderShape(&tree).as_str().unwrap().to_string();
        assert!(s.contains("### Scope (scope)"));
        assert!(s.contains("1. Node A — do it"));
        assert!(s.contains("→ CHOSEN: opt1 (cheap)"));
        assert!(s.contains("files: a.rs, b.rs"));
        assert!(s.contains("2. Node B"));
        assert!(s.contains("→ UNRESOLVED FORK: x | y"));
        assert!(s.contains("Phases:"));
        assert!(s.contains("- Phase 1 → scope"));
    }

    // ── parseAdvocate ───────────────────────────────────────────────────────
    #[test]
    fn parse_advocate_happy_path() {
        let res = json!({
            "ai_call_id": 42,
            "output_kv": [{ "field_name": "advocate_json", "parent_output_id": null, "value_text": "{\"stance\":\"strong\",\"arguments\":[{\"claim\":\"c1\",\"support\":\"s1\",\"weight\":\"high\"}],\"strongest\":\"c1\"}" }],
        });
        let out = parseAdvocate(&res);
        assert_eq!(out["stance"], json!("strong"));
        assert_eq!(out["arguments"][0]["claim"], json!("c1"));
        assert_eq!(out["arguments"][0]["weight"], json!("high"));
        assert_eq!(out["ai_call_id"], json!(42));
    }

    #[test]
    fn parse_advocate_salvages_truncated_json() {
        let raw = r#"{"stance":"x","arguments":[{"claim":"c1","support":"s1","weight":"high"},{"claim":"c2","support":"s2"#;
        let res = json!({ "output_kv": [{ "field_name": "advocate_json", "value_text": raw }] });
        let out = parseAdvocate(&res);
        let args = out["arguments"].as_array().unwrap();
        assert_eq!(args.len(), 1);
        assert_eq!(args[0]["claim"], json!("c1"));
        assert_eq!(args[0]["weight"], json!("high"));
    }

    #[test]
    fn parse_advocate_bad_weight_defaults_medium() {
        let res = json!({ "output_kv": [{ "field_name": "advocate_json", "value_text": "{\"arguments\":[{\"claim\":\"c\",\"support\":\"s\",\"weight\":\"nonsense\"}]}" }] });
        let out = parseAdvocate(&res);
        assert_eq!(out["arguments"][0]["weight"], json!("medium"));
    }

    // ── buildRuling / lintRuling ────────────────────────────────────────────
    #[test]
    fn build_ruling_happy_path() {
        let raw = json!(r#"{"verdict":"sound","confidence":80,"headline":"h","opinion":"o","evidence":["e1"],"insights":["i1"],"to_approve":["ta"],"dimensions":[{"name":"risk","score":50,"note":"n"}],"changes":{"add":[{"branch":"scope","title":"t","detail":"d","why":"w"}],"update":[],"remove":[]},"preserve":["p"],"decisive_factor":"df"}"#);
        let j = buildRuling(&raw, &json!(9));
        assert_eq!(j["verdict"], json!("sound"));
        assert_eq!(j["confidence"], json!(80));
        assert_eq!(j["headline"], json!("h"));
        assert_eq!(j["opinion"], json!("o"));
        assert_eq!(j["evidence"], json!(["e1"]));
        assert_eq!(j["dimensions"][0]["name"], json!("risk"));
        assert_eq!(j["changes"]["add"][0]["title"], json!("t"));
        assert_eq!(j["parse_ok"], json!(true));
        assert_eq!(j["ai_call_id"], json!(9));
        assert!(j.get("_raw").is_some());
    }

    #[test]
    fn build_ruling_invalid_verdict_and_no_reasoning_fails_lint() {
        let raw = json!(r#"{"headline":"h"}"#);
        let j = buildRuling(&raw, &Value::Null);
        assert_eq!(j["verdict"], json!("sound_with_revisions")); // default fallback
        let lint = lintRuling(&j);
        assert_eq!(lint["ok"], json!(false));
    }

    #[test]
    fn build_ruling_salvages_from_broken_json() {
        let raw = json!(r#"{"verdict":"not_ready","opinion":"partial reasoning here"#);
        let j = buildRuling(&raw, &Value::Null);
        assert_eq!(j["verdict"], json!("not_ready"));
        assert_eq!(j["parse_ok"], json!(false));
        assert_eq!(j["opinion"], json!("partial reasoning here"));
        assert_eq!(lintRuling(&j)["ok"], json!(true));
    }

    #[test]
    fn lint_ruling_passes_with_evidence_only() {
        let j = json!({ "verdict": "sound", "opinion": "", "evidence": ["e"], "insights": [] });
        assert_eq!(lintRuling(&j)["ok"], json!(true));
    }

    // ── parseSkeletonFiles ──────────────────────────────────────────────────
    #[test]
    fn parse_skeleton_files_happy_path() {
        let raw = json!(r#"{"files":[{"path":"src\\a.rs","action":"create","intent":"do it"},{"path":"src/b.rs","action":"modify","intent":"tweak"}]}"#);
        let files = parseSkeletonFiles(&raw);
        let arr = files.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["path"], json!("src/a.rs")); // backslashes normalized
        assert_eq!(arr[0]["action"], json!("create"));
        assert_eq!(arr[1]["action"], json!("modify"));
    }

    #[test]
    fn parse_skeleton_files_dedupes_and_caps_at_six() {
        let files_json: Vec<Value> = (0..8).map(|i| json!({ "path": format!("f{}.rs", i % 4), "action": "create" })).collect();
        let raw = json!(serde_json::to_string(&json!({ "files": files_json })).unwrap());
        let out = parseSkeletonFiles(&raw).as_array().unwrap().len();
        assert!(out <= 6);
    }

    #[test]
    fn parse_skeleton_files_defaults_unknown_action_to_modify() {
        let raw = json!(r#"{"files":[{"path":"x.rs","action":"bogus"}]}"#);
        let arr = parseSkeletonFiles(&raw);
        assert_eq!(arr[0]["action"], json!("modify"));
    }
}
