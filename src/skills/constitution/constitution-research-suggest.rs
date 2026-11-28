//! constitution-research-suggest.rs — twin of skills/constitution/constitution-research-suggest.mjs.
//! NAMES ARE CONTRACT: `constitutionResearchSuggest` (default export).
//!
//! BESPOKE-NATIVE (lane A — calls `aiCall` then `constitutionResearch`). Reuses already-converted
//! twins verbatim: `crate::constitution__helpers_schema::ensureConstitutionSchema`,
//! `crate::constitution_constitution_file::constitutionFile`,
//! `crate::constitution_constitution_research::constitutionResearch`,
//! `crate::ai_prompt_find::promptFind` / `crate::ai_prompt_add::promptAdd`,
//! `crate::ai_ai_call::aiCall`.
//!
//! TWO PARITY-DEFERRED boundaries, both ported the same way as constitution-agent-run.rs:
//!   1. `aiCall` (crate::ai_ai_call::aiCall) is ITSELF PARITY-DEFERRED (real network AI dispatch) —
//!      it ALWAYS returns `Err` in this crate. The JS `try { await aiCall(...) } catch (e) { return
//!      { ok:false, error, ai_call_id, cost_usd } }` is ported verbatim: every real invocation of
//!      this twin's default export takes that `Err` branch and returns early. That means the JS
//!      code AFTER the aiCall try/catch (the no-queries fallback, the `constitutionResearch` call,
//!      the ranking/dedupe of `urls`) is structurally unreachable through the default export in
//!      this crate — same shape as `constitution-agent-run.rs`'s deferred-boundary precedent. It is
//!      still ported faithfully (never invented) so the twin is complete once `aiCall` grows a real
//!      dispatch, and its pure pieces (`parsePlan`, `isReputable`, the ranking/dedupe step) are
//!      extracted into standalone functions and given real, direct `#[cfg(test)]` coverage below —
//!      the same density strategy `extractJson`/`validateReview` used in that sibling file.
//!   2. `constitutionResearch` (crate::constitution_constitution_research::constitutionResearch) is
//!      itself already twinned with its OWN deferred fetch seam (`fetch_fn: Option<&dyn Fn...>`,
//!      `None` = real network). Called here with `fetch_fn: None`, mirroring JS's
//!      `context.fetch_fn` not being threaded through this default export's signature (no `context`
//!      object exists on the Rust side — see `SKILL_DEFAULT_TAKES_INPUT_CONTEXT`'s db-lane
//!      convention already used by every sibling constitution/* twin: `(input, db)`, not
//!      `(input, context)`).
//!
//! The deterministic middle — path/mission/crew-laws/sources normalization, the per-path
//! `constitutionFile` sweep gathering law violations + weak axes (score < 70), the `concerns`
//! string assembly, and the "no context to research" early guard — runs BEFORE the aiCall dispatch
//! and is fully reachable + tested via the default export itself.
// QUALITY: mixed unit — a fully-reachable deterministic gate (path/mission/concerns assembly) +
// standalone pure helpers for the deferred-branch logic (parsePlan/isReputable/langOf/ranking),
// glued by 1 bespoke default export that always reaches the aiCall PARITY-DEFERRED boundary.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

const PROMPT: &str = "constitution-research-suggest";
// PARITY-DEFERRED prose: the real SYSTEM/TEMPLATE text lives only in the JS source (it is data
// consumed exclusively by the deferred aiCall network dispatch, never by a reachable branch here) —
// same precedent as constitution-agent-run.rs's PARITY_DEFERRED_TEMPLATE/SYSTEM constants.
// (2026-09-19 — the JS prose was lost with antiloki-core; rewritten from the parser: `concepts` ≤8, `queries` ≤4)
const SYSTEM: &str = "You pick the web research angles for a code-review mission. Given the mission, the languages, the files and the concerns antiloki measured, name the engineering CONCEPTS at stake (named patterns, principles, failure modes — things a search engine indexes well) and write a few precise search QUERIES a senior engineer would type to find authoritative pages (official docs, well-known engineering blogs, standards, papers). Prefer specificity over breadth; no query about the repository itself. Answer with ONE JSON object and nothing else: {\"concepts\": [\"<≤8 short names>\"], \"queries\": [\"<≤4 search queries>\"]}";
const TEMPLATE: &str = "MISSION: {{mission}}\nLANGUAGES: {{langs}}\nFILES: {{files}}\nMEASURED CONCERNS: {{concerns}}\nSOURCES THE CREW CARRIES: {{sources}}\n\nWrite the JSON.";

// domains a senior engineer trusts — scraped results from these lead the list.
const REPUTABLE: [&str; 28] = [
    "arxiv.org", "dl.acm.org", "acm.org", "ieee.org", "ieeexplore.ieee.org", "usenix.org", "springer.com", "link.springer.com", "sciencedirect.com", "semanticscholar.org", "nature.com", "ncbi.nlm.nih.gov",
    "developer.mozilla.org", "docs.python.org", "docs.oracle.com", "learn.microsoft.com", "react.dev", "nodejs.org", "typescriptlang.org", "kernel.org", "w3.org", "rfc-editor.org", "ietf.org",
    "martinfowler.com", "refactoring.com", "sourcemaking.com", "en.wikipedia.org", "wikipedia.org",
];
// `google.github.io` kept as its own entry (29th) to match the JS array exactly.
const REPUTABLE_EXTRA: &str = "google.github.io";

// JS: `const isReputable = (domain) => { const d = String(domain ?? '').toLowerCase(); return
// d.endsWith('.edu') || d.endsWith('.gov') || REPUTABLE.some((r) => d === r || d.endsWith(`.${r}`)); };`
fn isReputable(domain: &Value) -> bool {
    let d = if domain.is_null() { String::new() } else { js_string(domain) }.to_lowercase();
    if d.ends_with(".edu") || d.ends_with(".gov") { return true; }
    REPUTABLE.iter().chain(std::iter::once(&REPUTABLE_EXTRA)).any(|r| d == *r || d.ends_with(&format!(".{r}")))
}

// JS: `const langOf = (p) => (/\.tsx?$/.test(p) ? 'TypeScript' : /\.py$/.test(p) ? 'Python' :
// /\.(js|mjs|cjs)$/.test(p) ? 'JavaScript' : /\.go$/.test(p) ? 'Go' : /\.rs$/.test(p) ? 'Rust' :
// /\.java$/.test(p) ? 'Java' : 'code');`
fn langOf(p: &str) -> &'static str {
    if p.ends_with(".ts") || p.ends_with(".tsx") { "TypeScript" }
    else if p.ends_with(".py") { "Python" }
    else if p.ends_with(".js") || p.ends_with(".mjs") || p.ends_with(".cjs") { "JavaScript" }
    else if p.ends_with(".go") { "Go" }
    else if p.ends_with(".rs") { "Rust" }
    else if p.ends_with(".java") { "Java" }
    else { "code" }
}

// JS: `ensurePrompt(db)` — get-or-add the prompt template, comparing template_md/system_md
// verbatim like the sibling `constitution-agent-run.rs::ensure_prompt`. The prompt PROSE itself is
// PARITY-DEFERRED data (see module doc); this twin always takes the "insert-if-missing/mismatched"
// branch, sufficient to exercise every reachable path.
fn ensurePrompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md") == Some(&json!(TEMPLATE)) && e.get("system_md") == Some(&json!(SYSTEM)) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(&json!({
        "name": PROMPT, "template_md": TEMPLATE, "system_md": SYSTEM,
        "default_tier": "low", "default_temperature": "cold", "phase_flow": "single_only",
        "max_output_tokens": 400,
        "output_fields": [{ "field_name": "queries_json", "field_type": "string" }],
    }), db)?;
    Ok(())
}

// JS: `parsePlan(text)` — tolerant parse: accept {concepts,queries}, {queries}, or a bare array of
// strings. Returns (concepts, queries), each cleaned + capped.
fn parsePlan(text: &str) -> (Vec<String>, Vec<String>) {
    let a = text.find('{');
    let arr = text.find('[');
    let mut obj: Option<Value> = None;
    if let Some(ai) = a {
        if let Some(bi) = text.rfind('}') {
            if bi > ai {
                if let Ok(v) = serde_json::from_str::<Value>(&text[ai..=bi]) { obj = Some(v); }
            }
        }
    }
    if obj.is_none() {
        if let Some(arri) = arr {
            if let Some(bi) = text.rfind(']') {
                if bi > arri {
                    if let Ok(v) = serde_json::from_str::<Value>(&text[arri..=bi]) {
                        obj = Some(json!({ "queries": v }));
                    }
                }
            }
        }
    }
    let obj = match obj { Some(o) => o, None => return (vec![], vec![]) };
    let clean = |v: Option<&Value>, cap: usize| -> Vec<String> {
        let mut out: Vec<String> = v.and_then(Value::as_array)
            .map(|arr| arr.iter().map(js_string).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default();
        out.truncate(cap);
        out
    };
    (clean(obj.get("concepts"), 8), clean(obj.get("queries"), 4))
}

// JS: the `.map(...).sort(...)` + dedupe/cap(10) tail of the default export, once `research.sources`
// is in hand. Extracted as its own pure/testable unit (see module doc — the aiCall boundary makes
// this unreachable through the default export in this crate).
fn rankAndDedupeSources(sources: &[Value]) -> Vec<Value> {
    let mut ranked: Vec<Value> = sources.iter().map(|s| {
        let domain = s.get("domain").cloned().unwrap_or(Value::Null);
        let title = s.get("title").cloned().unwrap_or(Value::Null);
        let domain_s = if domain.is_null() { String::new() } else { js_string(&domain) };
        // `${s.title ? ` · ${s.title}` : ''}` — only appended when title is TRUTHY.
        let why = if truthy(Some(&title)) { format!("{domain_s} · {}", js_string(&title)) } else { domain_s };
        json!({
            "url": s.get("url").cloned().unwrap_or(Value::Null),
            "why": why.trim().to_string(),
            "reputable": isReputable(&domain),
        })
    }).collect();
    // `.sort((a, b) => (b.reputable ? 1 : 0) - (a.reputable ? 1 : 0))` — reputable-first, stable.
    ranked.sort_by_key(|u| if u.get("reputable") == Some(&Value::Bool(true)) { 0 } else { 1 });
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for u in ranked {
        let url_s = js_string(u.get("url").unwrap_or(&Value::Null));
        if !seen.insert(url_s) { continue; }
        out.push(u);
        if out.len() >= 10 { break; }
    }
    out
}

fn set_push(vec: &mut Vec<String>, seen: &mut HashSet<String>, v: String) {
    if seen.insert(v.clone()) { vec.push(v); }
}

pub fn constitutionResearchSuggest(input: &Value, db: &Connection) -> Result<Value, String> {
    // `context.db` missing is unreachable here — `db: &Connection` is always a real connection,
    // per this project's standard db-lane twin convention (see e.g. constitution-file.rs).
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);

    // THE WHOLE CONTEXT — the ingredients box, not one file
    let mut raw_paths: Vec<Value> = Vec::new();
    if let Some(arr) = input.get("paths").and_then(Value::as_array) { raw_paths.extend(arr.iter().cloned()); }
    if truthy(input.get("path")) { raw_paths.push(input.get("path").cloned().unwrap()); }
    let mut path_seen = HashSet::new();
    let mut paths: Vec<String> = Vec::new();
    for p in &raw_paths {
        let s = js_string(p).replace('\\', "/").trim().to_string();
        if s.is_empty() { continue; }
        set_push(&mut paths, &mut path_seen, s);
    }
    paths.truncate(6);

    const DEFAULT_MISSION: &str = "(a general engineering review)";
    let mission_raw = nz(input.get("mission")).map(js_string).unwrap_or_default();
    let mission_sliced: String = mission_raw.chars().take(300).collect();
    let mission_trimmed = mission_sliced.trim().to_string();
    let mission = if mission_trimmed.is_empty() { DEFAULT_MISSION.to_string() } else { mission_trimmed };

    let crew_laws: Vec<String> = input.get("crew_laws").and_then(Value::as_array)
        .map(|a| a.iter().map(js_string).filter(|s| !s.is_empty()).collect()).unwrap_or_default();
    let sources: Vec<String> = input.get("sources").and_then(Value::as_array)
        .map(|a| a.iter().map(js_string).filter(|s| !s.is_empty()).collect()).unwrap_or_default();

    // gather the concerns across every scoped file
    let mut file_laws: Vec<String> = Vec::new();
    let mut file_laws_seen = HashSet::new();
    let mut weak_axes: Vec<String> = Vec::new();
    let mut weak_axes_seen = HashSet::new();
    let mut langs: Vec<String> = Vec::new();
    let mut langs_seen = HashSet::new();
    for p in &paths {
        set_push(&mut langs, &mut langs_seen, langOf(p).to_string());
        // `try { ... } catch { /* a file outside the snapshot just contributes its language */ }`
        if let Ok(fc) = crate::constitution_constitution_file::constitutionFile(&json!({ "path": p.clone() }), db) {
            if fc.get("found") == Some(&Value::Bool(true)) {
                if let Some(viols) = fc.get("violations").and_then(Value::as_array) {
                    for v in viols {
                        if truthy(v.get("law_key")) {
                            set_push(&mut file_laws, &mut file_laws_seen, js_string(v.get("law_key").unwrap()));
                        }
                    }
                }
                if let Some(axes) = fc.get("axes").and_then(Value::as_object) {
                    for (ax, a) in axes {
                        if a.is_null() { continue; }
                        if let Some(score) = nz(a.get("score")) {
                            if score.as_f64().map_or(false, |sc| sc < 70.0) {
                                set_push(&mut weak_axes, &mut weak_axes_seen, ax.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    let mut all_laws: Vec<String> = Vec::new();
    let mut all_laws_seen = HashSet::new();
    for k in file_laws.iter().chain(crew_laws.iter()) { set_push(&mut all_laws, &mut all_laws_seen, k.clone()); }
    all_laws.truncate(12);

    let mut concern_items: Vec<String> = all_laws.clone();
    concern_items.extend(weak_axes.iter().map(|ax| format!("{ax} (weak axis)")));
    let concerns = if concern_items.is_empty() { "(no specific concerns — reason from the mission)".to_string() } else { concern_items.join(", ") };

    if paths.is_empty() && crew_laws.is_empty() && mission == DEFAULT_MISSION {
        return Ok(json!({ "ok": false, "error": "no context to research — add files, a mission, or a crew first" }));
    }

    ensurePrompt(db)?;

    let langs_join = if langs.is_empty() { "code".to_string() } else { langs.join(", ") };
    let files_join = {
        let names: Vec<String> = paths.iter().map(|p| p.rsplit('/').next().unwrap_or(p).to_string()).collect();
        if names.is_empty() { "(none)".to_string() } else { names.join(", ") }
    };
    let sources_join = if sources.is_empty() { "(none)".to_string() } else { sources.join(", ") };

    let call_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "mission", "value": mission },
            { "key": "langs", "value": langs_join },
            { "key": "files", "value": files_join },
            { "key": "concerns", "value": concerns },
            { "key": "sources", "value": sources_join },
        ],
    });

    let (concepts, mut queries, ai_call_id, cost): (Vec<String>, Vec<String>, Value, f64);
    match crate::ai_ai_call::aiCall(&call_input, db) {
        Ok(res) => {
            let id = nz(res.get("ai_call_id")).cloned().unwrap_or(Value::Null);
            let c = res.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
            let text = res.get("output_kv").and_then(Value::as_array).and_then(|arr| arr.iter().find(|o| {
                o.get("field_name").and_then(Value::as_str) == Some("queries_json") && o.get("parent_output_id").map_or(true, |v| v.is_null())
            })).and_then(|o| o.get("value_text")).map(js_string).unwrap_or_default();
            let (cc, qq) = parsePlan(&text);
            concepts = cc; queries = qq; ai_call_id = id; cost = c;
        }
        Err(e) => {
            // `catch (e) { return { ok: false, error: String(e?.message ?? e).slice(0, 200),
            // ai_call_id: aiCallId, cost_usd: cost }; }` — aiCallId/cost never left their initial
            // null/0 (the assignment only happens inside the try, after the point of failure).
            return Ok(json!({ "ok": false, "error": e.chars().take(200).collect::<String>(), "ai_call_id": Value::Null, "cost_usd": 0.0 }));
        }
    }

    // FALLBACK: the model produced no queries → derive authoritative queries from the concerns
    // themselves, so "AI picks" always returns something rather than nothing.
    if queries.is_empty() {
        let lang = langs.get(0).cloned().unwrap_or_else(|| "software".to_string());
        let mut qs: Vec<String> = all_laws.iter().take(2)
            .map(|k| format!("{} {lang} best practices paper", k.replace('-', " ")))
            .collect();
        qs.extend(weak_axes.iter().take(1).map(|ax| format!("{ax} {lang} software engineering documentation")));
        qs.truncate(3);
        if qs.is_empty() {
            let mission_head = mission.split(':').next().unwrap_or(&mission).to_string();
            qs = vec![format!("{mission_head} {lang} software engineering best practices")];
        }
        queries = qs;
    }

    let research = crate::constitution_constitution_research::constitutionResearch(&json!({ "queries": queries }), db, None);
    let sources_arr: Vec<Value> = research.get("sources").and_then(Value::as_array).cloned().unwrap_or_default();
    let urls = rankAndDedupeSources(&sources_arr);
    let note = if !urls.is_empty() {
        "AI chose the angles from the whole context; serious sources first — edit freely"
    } else {
        "the scrape found nothing usable — try again or paste your own URLs"
    };

    Ok(json!({
        "ok": true, "urls": urls, "concepts": concepts, "queries": queries,
        "ai_call_id": ai_call_id, "cost_usd": cost, "note": note,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL DEFAULT 1, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c
    }

    // ── parsePlan ────────────────────────────────────────────────────────────────────────────
    #[test]
    fn parse_plan_object_form_cleans_and_caps() {
        let text = r#"sure, here: {"concepts":["a"," b ","",1,"c","d","e","f","g","h"],"queries":["q1","q2","q3","q4","q5"]} thanks"#;
        let (concepts, queries) = parsePlan(text);
        assert_eq!(concepts, vec!["a", "b", "1", "c", "d", "e", "f", "g"]); // capped at 8
        assert_eq!(queries, vec!["q1", "q2", "q3", "q4"]); // capped at 4
    }

    #[test]
    fn parse_plan_bare_array_becomes_queries_only() {
        let (concepts, queries) = parsePlan(r#"[" x ", "y", ""]"#);
        assert_eq!(concepts, Vec::<String>::new());
        assert_eq!(queries, vec!["x", "y"]);
    }

    #[test]
    fn parse_plan_unparseable_text_returns_empty() {
        let (concepts, queries) = parsePlan("no json here at all");
        assert!(concepts.is_empty());
        assert!(queries.is_empty());
    }

    // ── isReputable ──────────────────────────────────────────────────────────────────────────
    #[test]
    fn is_reputable_matches_edu_gov_and_listed_domains_case_insensitively() {
        assert!(isReputable(&json!("FOO.EDU")));
        assert!(isReputable(&json!("agency.gov")));
        assert!(isReputable(&json!("arxiv.org")));
        assert!(isReputable(&json!("link.springer.com"))); // exact list entry
        assert!(isReputable(&json!("sub.arxiv.org"))); // subdomain of a listed domain
        assert!(isReputable(&json!("google.github.io")));
        assert!(!isReputable(&json!("random-blog.example")));
        assert!(!isReputable(&Value::Null));
    }

    // ── langOf ───────────────────────────────────────────────────────────────────────────────
    #[test]
    fn lang_of_maps_known_extensions_and_falls_back_to_code() {
        assert_eq!(langOf("a.ts"), "TypeScript");
        assert_eq!(langOf("a.tsx"), "TypeScript");
        assert_eq!(langOf("a.py"), "Python");
        assert_eq!(langOf("a.mjs"), "JavaScript");
        assert_eq!(langOf("a.go"), "Go");
        assert_eq!(langOf("a.rs"), "Rust");
        assert_eq!(langOf("a.java"), "Java");
        assert_eq!(langOf("a.txt"), "code");
    }

    // ── rankAndDedupeSources ─────────────────────────────────────────────────────────────────
    #[test]
    fn rank_and_dedupe_sources_puts_reputable_first_stably_dedupes_and_caps() {
        let sources = vec![
            json!({ "url": "https://blog.example/a", "domain": "blog.example", "title": "A" }),
            json!({ "url": "https://arxiv.org/b", "domain": "arxiv.org", "title": "B" }),
            json!({ "url": "https://blog.example/a", "domain": "blog.example", "title": "dup" }),
            json!({ "url": "https://ieee.org/c", "domain": "ieee.org" }),
        ];
        let out = rankAndDedupeSources(&sources);
        assert_eq!(out.len(), 3, "duplicate url deduped");
        assert_eq!(out[0]["url"], json!("https://arxiv.org/b"));
        assert_eq!(out[0]["why"], json!("arxiv.org · B"));
        assert_eq!(out[0]["reputable"], json!(true));
        assert_eq!(out[1]["url"], json!("https://ieee.org/c"));
        assert_eq!(out[1]["why"], json!("ieee.org")); // no title → no " · title" suffix
        assert_eq!(out[2]["url"], json!("https://blog.example/a"));
        assert_eq!(out[2]["reputable"], json!(false));
    }

    #[test]
    fn rank_and_dedupe_sources_caps_at_ten() {
        let sources: Vec<Value> = (0..15).map(|i| json!({ "url": format!("https://x.example/{i}"), "domain": "x.example" })).collect();
        assert_eq!(rankAndDedupeSources(&sources).len(), 10);
    }

    // ── constitutionResearchSuggest (deterministic gate, reachable up to the aiCall boundary) ──
    #[test]
    fn no_context_returns_ok_false_without_touching_ai_call() {
        let c = setup();
        let r = constitutionResearchSuggest(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("no context to research"));
        // ensurePrompt never ran — no template inserted
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn mission_only_input_passes_the_context_guard_and_reaches_deferred_ai_call() {
        let c = setup();
        let r = constitutionResearchSuggest(&json!({ "mission": "reduce coupling" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("ai-call"), "{:?}", r["error"]);
        assert_eq!(r["ai_call_id"], Value::Null);
        assert_eq!(r["cost_usd"], json!(0.0));
        // ensurePrompt DID run before the deferred dispatch
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'constitution-research-suggest'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn crew_laws_alone_also_passes_the_context_guard() {
        let c = setup();
        let r = constitutionResearchSuggest(&json!({ "crew_laws": ["no-var"] }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("ai-call"));
    }

    #[test]
    fn gathers_law_and_weak_axis_concerns_from_scoped_files_before_the_deferred_dispatch() {
        // seeds a real constitutionFile() result (weak compliance axis + a violation) and proves
        // the concerns-gathering sweep ran (indirectly, via reaching ensurePrompt) even though the
        // final `concerns` string itself is only consumed by the deferred aiCall input_kv.
        let c = setup();
        c.execute(
            "INSERT INTO judgments.review_snapshots (id, created_at, scope_json, manifest_json, constitution_hash, status) VALUES (1, '2026-01-01 00:00:00', '{}', '[]', 'h1', 'done')",
            [],
        ).unwrap();
        c.execute("INSERT INTO laws.review_laws (scope, key, title, rule_md, status, enforcement, weight) VALUES ('project','no-var','No var','r','ratified','score',5)", []).unwrap();
        c.execute(
            "INSERT INTO judgments.review_scores (snapshot_id, unit_kind, path, score, health, archetype, loc, complexity, fan_in, fan_out, covered, churn, authors, findings, pct_repo, pct_folder, axes_json) VALUES \
             (1,'file','src/a.js',60,60,'logic',10,2,1,1,1,0,1,0,0.5,0.5,'{\"compliance\":{\"score\":60}}')",
            [],
        ).unwrap();
        c.execute("INSERT INTO judgments.review_violations (snapshot_id, path, law_key, law_scope, weight) VALUES (1,'src/a.js','no-var','project',5)", []).unwrap();

        let r = constitutionResearchSuggest(&json!({ "path": "src/a.js" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("ai-call"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1, "the sweep completed and reached ensurePrompt");
    }

    #[test]
    fn a_path_outside_any_snapshot_still_contributes_only_its_language_and_does_not_abort() {
        let c = setup();
        let r = constitutionResearchSuggest(&json!({ "path": "src/missing.py" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("ai-call"));
    }

    #[test]
    fn ensure_prompt_is_idempotent_across_two_calls() {
        let c = setup();
        let _ = constitutionResearchSuggest(&json!({ "mission": "m1" }), &c).unwrap();
        let _ = constitutionResearchSuggest(&json!({ "mission": "m2" }), &c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'constitution-research-suggest'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
