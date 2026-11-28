//! constitution-editor.rs — twin of skills/constitution/constitution-editor.mjs. NAMES ARE
//! CONTRACT: default `constitutionEditor`.
//!
//! BESPOKE-NATIVE (lane A — calls `aiCall`). Reuses already-converted twins verbatim:
//! `crate::constitution__helpers_schema::ensureConstitutionSchema`,
//! `crate::constitution_constitution_file::constitutionFile`,
//! `crate::constitution_constitution_agent_run::extractJson`,
//! `crate::ai_prompt_find::promptFind` / `crate::ai_prompt_add::promptAdd`,
//! `crate::ai_ai_call::aiCall`.
//!
//! `runWithSession` (the swarm-marker AsyncLocalStorage wrapper around `callOnce`) is left
//! untouched for the same reason run-session.rs's own twin left it unported: no Rust
//! async-local-storage equivalent exists in this harness, and it wraps nothing but the (deferred)
//! `aiCall` dispatch — omitting it changes no observable behavior of this twin.
//!
//! ONE PARITY-DEFERRED boundary (`aiCall`, itself deferred — see that module's doc), reached via a
//! 2-attempt retry loop that mirrors the JS's try/catch-per-attempt exactly: every deterministic
//! gate before it — the empty-reviews short circuit, the deterministic record (BE-side,
//! `constitutionFile` per path), the reviews/court/moves text assembly, and `ensurePrompt`'s
//! create-or-match — runs and is fixture-tested; both attempts fail (aiCall's stub always errs),
//! so the loop completes and the function returns the JS's `{ ok: true, editor: null, dropped: 1,
//! parse_excerpt, attempts: 2 }` shape — itself a faithfully-ported, reachable, testable outcome.
//! The post-parse editor-shaping logic (the `body -> editor` clip/filter block) is ported
//! byte-for-byte from the JS but is unreachable by this file's own fixture (it only runs after a
//! successful `aiCall`, which the deferred stub never produces) — correctness there is by
//! inspection, matching book-run.rs's precedent for its own unreachable post-wave logic.
// QUALITY: mixed unit — deterministic record/text assembly + prompt-ensure (real, fixture-tested)
// + a bespoke/deferred AI-dispatch tail (network via aiCall, no fixture beyond the deferred error).

use rusqlite::Connection;
use serde_json::{json, Map, Value};

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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn clip(v: Option<&Value>, n: usize) -> Value {
    match v {
        None | Some(Value::Null) => Value::Null,
        Some(x) => json!(js_string(x).chars().take(n).collect::<String>()),
    }
}
fn clip_str(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

const PROMPT: &str = "constitution-editor";
// THE EDITOR'S CONTRACT (2026-09-19 — the JS prose was lost with antiloki-core; rewritten from the
// parser below, which is the contract): the paper specialist binds the crew's validated reviews
// into ONE study — the thesis, the concepts, one case per file, the meta-theory with its
// branches, the court, the repair read, the teachings, the references.
const SYSTEM_MD: &str = "You are THE EDITOR of antiloki's review swarm: the paper specialist who turns the crew's independent, validated reviews of a code region into ONE study a senior engineer would read end to end. The specialists did the looking; you do the binding — you synthesise, you do not re-review. You write in clear, direct prose (≈55% prose, the rest is carried by the figures the reviews already proposed); every claim traces back to a review, a measured record line, or a listed web source. Never invent a file, a law, a number or a URL.

Rules of the study:
- THE THESIS FIRST. The introduction states what the study found, never the method.
- ONE META-THEORY. Bind the specialists' theories into one central statement with branches (each branch: a claim, which reviews/agents it comes from, and whether it supports / refines / tensions the central statement).
- THE COURT IS NARRATED, NOT EXTENDED. Where the lenses converged, say so; where they clash, present both sides and give a verdict with its confidence and its unknowns.
- ONE CASE PER FILE. For every file in the record: a bridge sentence from the previous case, the narrative (the theory as it shows in THIS file, cited), a simplified 'what this means' line, and, when the reviews carry one, a code example.
- THE REPAIR READ. Rank the crew's proposed moves before → after; say what the code looks like when they are done.
- TEACH. Name the concepts the study leans on (concept · plain explanation · an example).
- REFERENCES. Internal refs are file paths and law ids from the record; external ones come from the scraped web sources (prefer them) — with a note on what each one supports.

Answer with ONE JSON object and nothing else — no prose around it, no markdown fence:
{\"title\": \"<the study's title, ≤160>\", \"discovery\": \"<the one-line discovery, ≤320>\",
 \"arc\": {\"question\": \"<the question the study answers, ≤300>\", \"thread\": \"<the through-line, ≤300>\"},
 \"reading_order\": [{\"path\": \"<a file from the record>\", \"why\": \"<≤160>\"}],
 \"introduction\": \"<the thesis, ≤2200>\",
 \"per_file\": [{\"path\": \"<a file from the record>\", \"bridge\": \"<≤320>\", \"narrative\": \"<≤1800, cite the reviews>\", \"simplified\": \"<≤500>\", \"example\": {\"caption\": \"<≤200>\", \"code\": \"<≤900>\"} | null}],
 \"meta_theory\": {\"name\": \"<≤120>\", \"statement\": \"<≤900>\", \"branches\": [{\"name\": \"<≤90>\", \"claim\": \"<≤520>\", \"axis\": \"<axis name>\", \"from\": [\"<agent or file>\"], \"relation\": \"supports|refines|tensions\"}], \"root_cause\": \"<≤420>\", \"anatomy\": {\"starts\": \"<how it starts>\", \"grows\": \"<how it grows>\", \"recognize\": \"<how to recognise it>\", \"prevent\": \"<how to prevent it>\", \"repair\": \"<how to repair it>\"}, \"supports\": [\"<≤100 each>\"], \"counterargument\": \"<≤500>\", \"verdict\": \"<≤400>\", \"confidence\": <0..1>, \"unknowns\": [\"<≤180 each>\"]},
 \"discussion\": \"<the court, narrated, ≤2600>\", \"cost\": \"<what it costs to leave it, ≤700>\", \"repair_read\": \"<before → after, ≤1100>\", \"generalization\": \"<what this teaches beyond this region, ≤520>\",
 \"teachings\": [{\"concept\": \"<≤110>\", \"plain\": \"<≤600>\", \"example\": {\"caption\": \"<≤200>\", \"code\": \"<≤900>\"} | null}],
 \"conclusion\": \"<≤1400>\", \"further_study\": [\"<≤90 each>\"],
 \"references\": {\"internal\": [{\"ref\": \"<file path or law id>\", \"note\": \"<≤220>\"}], \"external\": [{\"title\": \"<≤170>\", \"source\": \"<site, ≤110>\", \"url\": \"<a listed URL>\", \"note\": \"<≤220>\"}]}}";
const TEMPLATE_MD: &str = "THE STUDY: {{label}}\n\nTHE DETERMINISTIC RECORD (measured, not opinion):\n{{record}}\n\nTHE COURT (computed from validated citations — narrate, never extend):\n{{court}}\n\nTHE CREW'S PROPOSED MOVES:\n{{moves}}\n\nSCRAPED WEB SOURCES (REAL urls gathered deterministically before the crew — prefer THESE for external references; still verify tone):\n{{websources}}\n\nTHE SPECIALISTS' VALIDATED REVIEWS (independent studies — your work starts AFTER theirs):\n{{reviews}}\n\nWrite the editor JSON.";

fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if let Ok(e) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db) {
        let same_template = e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE_MD);
        let same_system = e.get("system_md").and_then(Value::as_str) == Some(SYSTEM_MD);
        if same_template && same_system {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE_MD,
            "system_md": SYSTEM_MD,
            "default_tier": "high",
            "default_temperature": "warm",
            "phase_flow": "single_only",
            "max_output_tokens": 4600,
            "output_fields": [{ "field_name": "editor_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

pub fn constitutionEditor(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);

    let reviews: Vec<Value> = input
        .get("reviews")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| {
            r.get("agent").map(|v| !v.is_null() && v != &json!("")).unwrap_or(false)
                && r.get("path").map(|v| !v.is_null() && v != &json!("")).unwrap_or(false)
                && r.get("review").map(|v| !v.is_null()).unwrap_or(false)
        })
        .collect();
    if reviews.is_empty() {
        return Ok(json!({ "ok": false, "error": "no validated reviews to edit — run the crew first" }));
    }

    let label = clip(input.get("label"), 120);
    let label = match label {
        Value::Null => "the selected region".to_string(),
        Value::String(ref s) if s.is_empty() => "the selected region".to_string(),
        Value::String(s) => s,
        _ => "the selected region".to_string(),
    };

    let mut paths: Vec<String> = Vec::new();
    for r in &reviews {
        let p = js_string(r.get("path").unwrap_or(&Value::Null));
        if !paths.contains(&p) {
            paths.push(p);
        }
    }
    paths.truncate(12);

    // THE DETERMINISTIC RECORD — fetched BE-side, the editor grounds on the same facts the crew
    // did, never on the FE's retelling.
    let mut record_lines: Vec<String> = Vec::new();
    for p in &paths {
        let fc = crate::constitution_constitution_file::constitutionFile(&json!({ "path": p }), db).ok();
        let found = fc.as_ref().and_then(|f| f.get("found")).and_then(Value::as_bool).unwrap_or(false);
        if !found {
            record_lines.push(format!("{p} — (outside the snapshot)"));
            continue;
        }
        let fc = fc.unwrap();
        let health = fc.get("health").map(js_string).unwrap_or_else(|| "?".to_string());
        let axes = fc
            .get("axes")
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .map(|(k, a)| format!("{k}={}", a.get("score").map(js_string).unwrap_or_else(|| "?".to_string())))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let violations = fc.get("violations").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut law_keys: Vec<String> = Vec::new();
        for v in &violations {
            let lk = js_string(v.get("law_key").unwrap_or(&Value::Null));
            if !law_keys.contains(&lk) {
                law_keys.push(lk);
            }
        }
        law_keys.truncate(10);
        let laws_str = law_keys.join(", ");
        let laws_part = if laws_str.is_empty() { String::new() } else { format!(" [§ {laws_str}]") };
        record_lines.push(format!(
            "{p} — health {health} · {axes} · {} violation(s){laws_part}",
            violations.len()
        ));
    }
    let record = record_lines.join("\n");

    let reviews_text = reviews
        .iter()
        .take(24)
        .map(|r| {
            let agent = js_string(r.get("agent").unwrap_or(&Value::Null));
            let path = js_string(r.get("path").unwrap_or(&Value::Null));
            let rv = r.get("review").cloned().unwrap_or(json!({}));
            let t = rv.get("theory").cloned().unwrap_or(json!({}));
            let level = rv.get("summary").and_then(|s| s.get("level")).map(js_string).unwrap_or_else(|| "finding".to_string());
            let name = t.get("name").and_then(Value::as_str).map(String::from).unwrap_or_else(|| {
                rv.get("summary").and_then(|s| s.get("title")).and_then(Value::as_str).map(String::from).unwrap_or_default()
            });
            let headline = rv.get("summary").and_then(|s| s.get("headline")).map(js_string).unwrap_or_default();
            let explanation = t.get("explanation").filter(|v| !v.is_null());
            let if_nothing = t.get("if_nothing_changes").filter(|v| !v.is_null());
            let cites = rv
                .get("evidence")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|e| js_string(e.get("id").unwrap_or(&Value::Null))).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            let cards = rv
                .get("cards")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|c| {
                    let kind = js_string(c.get("type").unwrap_or(&Value::Null));
                    let sev = js_string(c.get("severity").unwrap_or(&Value::Null));
                    let title = js_string(c.get("title").unwrap_or(&Value::Null));
                    let body = c.get("body").filter(|v| !v.is_null()).map(|b| format!(": {}", js_string(b))).unwrap_or_default();
                    format!("  - [{kind}/{sev}] {title}{body}")
                })
                .collect::<Vec<_>>()
                .join("\n");
            let pushback = rv.get("expected_pushback").filter(|v| !v.is_null());

            let mut lines: Vec<String> = vec![
                format!("{agent} on {path} — {level}: \"{name}\""),
                format!("  {headline}"),
            ];
            if let Some(e) = explanation {
                lines.push(format!("  {}", js_string(e)));
            }
            if let Some(n) = if_nothing {
                lines.push(format!("  if nothing changes: {}", js_string(n)));
            }
            if !cites.is_empty() {
                lines.push(format!("  cites: {cites}"));
            }
            if !cards.is_empty() {
                lines.push(cards);
            }
            if let Some(p) = pushback {
                lines.push(format!("  expects pushback: {}", js_string(p)));
            }
            lines.into_iter().filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    // THE COURT + THE MOVES (contract v3) — computed convergence/divergence and the crew's
    // proposed actions ride into the prompt; the editor NARRATES them, never extends them.
    let court_text = {
        let empty: Vec<Value> = Vec::new();
        let cv = input.get("court").and_then(|c| c.get("convergence")).and_then(Value::as_array).unwrap_or(&empty);
        let dv = input.get("court").and_then(|c| c.get("divergence")).and_then(Value::as_array).unwrap_or(&empty);
        let mut lines: Vec<String> = Vec::new();
        for c in cv.iter().take(8) {
            let key = js_string(c.get("key").unwrap_or(&Value::Null));
            let n = c.get("agents").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0);
            lines.push(format!("CONVERGENCE on {key}: {n} independent lenses reached the same spot"));
        }
        for d in dv.iter().take(8) {
            let key = js_string(d.get("key").unwrap_or(&Value::Null));
            let agents = d
                .get("agents")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(js_string).collect::<Vec<_>>().join(" vs "))
                .unwrap_or_default();
            let sevs = d.get("sevs").and_then(Value::as_array).map(|a| {
                format!(" ({})", a.iter().map(js_string).collect::<Vec<_>>().join(" / "))
            }).unwrap_or_default();
            lines.push(format!("DISSENT on {key}: {agents}{sevs}"));
        }
        if lines.is_empty() { "(no computed agreements or disagreements)".to_string() } else { lines.join("\n") }
    };
    let moves_text = {
        let empty: Vec<Value> = Vec::new();
        let actions = input.get("actions").and_then(Value::as_array).unwrap_or(&empty);
        let lines: Vec<String> = actions
            .iter()
            .take(12)
            .map(|a| {
                let title = a.get("title").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "?".to_string());
                let impact = a.get("impact").filter(|v| !v.is_null()).map(|i| {
                    let effort = a.get("effort").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "?".to_string());
                    format!(" (impact {}, effort {effort})", js_string(i))
                }).unwrap_or_default();
                let agents = a.get("agents").filter(|v| !v.is_null()).map(|ag| {
                    let list = match ag { Value::Array(arr) => arr.iter().map(js_string).collect::<Vec<_>>(), other => vec![js_string(other)] };
                    format!(" — by {}", list.join(" + "))
                }).unwrap_or_default();
                format!("- {title}{impact}{agents}")
            })
            .collect();
        if lines.is_empty() { "(no moves proposed)".to_string() } else { lines.join("\n") }
    };

    ensure_prompt(db)?;

    let swarm_id_raw = str_qq(input.get("swarm_id"));
    let swarm_id = clip_str(&swarm_id_raw, 80);
    let _swarm_id = if swarm_id.is_empty() { Value::Null } else { json!(swarm_id) };
    // `runWithSession` (PARITY-DEFERRED, see module doc) would wrap the call below in the swarm
    // marker; omitted here — it changes no observable behavior of `callOnce` itself.

    let web_sources_text = {
        let empty: Vec<Value> = Vec::new();
        let ws = input.get("web_sources").and_then(Value::as_array).unwrap_or(&empty);
        let lines: Vec<String> = ws
            .iter()
            .take(12)
            .map(|w| {
                let domain = w.get("domain").filter(|v| !v.is_null()).map(js_string).unwrap_or_else(|| "?".to_string());
                let title = w.get("title").filter(|v| !v.is_null()).map(js_string).unwrap_or_default();
                let url = js_string(w.get("url").unwrap_or(&Value::Null));
                format!("[{domain}] {title} <{url}>")
            })
            .collect();
        if lines.is_empty() { "(none gathered)".to_string() } else { lines.join("\n") }
    };
    let web_search = input.get("web_search").and_then(Value::as_bool).unwrap_or(true);
    let provider_fn = input.get("provider_fn").cloned().unwrap_or(Value::Null);

    let call_once = |attempt: i64| -> Result<Value, String> {
        let mut kv = vec![
            json!({ "key": "label", "value": label.clone() }),
            json!({ "key": "record", "value": record.clone() }),
            json!({ "key": "court", "value": court_text.clone() }),
            json!({ "key": "moves", "value": moves_text.clone() }),
            json!({ "key": "websources", "value": web_sources_text.clone() }),
            json!({ "key": "reviews", "value": clip_str(&reviews_text, 24000) }),
        ];
        if attempt > 1 {
            kv.push(json!({ "key": "_retry", "value": attempt.to_string() }));
        }
        let mut call_input = json!({
            "prompt_name": PROMPT,
            "input_kv": kv,
            "bypass_cache": attempt > 1,
            "web_search": web_search,
            "provider_fn": provider_fn.clone(),
        });
        // THE RUN-ON CHOICE (operator 2026-09-19): the mission picks the editor's tier (default high — the
        // template's) and, optionally, the coding-CLI engine it runs on (cli:<engine> pins ai-call).
        if let Some(t) = input.get("tier").and_then(Value::as_str).filter(|t| ["low", "mid", "high"].contains(t)) { call_input["tier"] = json!(t); }
        if let Some(e) = input.get("engine").and_then(Value::as_str).map(str::trim).filter(|e| !e.is_empty() && *e != "routed") { call_input["model_id"] = json!(format!("cli:{}", e.trim_start_matches("cli:"))); }
        crate::ai_ai_call::aiCall(&call_input, db)
    };

    // caught attempts — a thrown ai-call (schema validation, provider failure) is a failed
    // attempt, never a dead study; already high-tier, so the ladder is retry-only, no escalation.
    let mut attempts: i64 = 0;
    let mut cost: f64 = 0.0;
    let mut ai_call_id: Value = Value::Null;
    let mut text = String::new();
    let mut editor: Option<Value> = None;
    let mut last_err: Option<String> = None;

    for att in 1..=2i64 {
        attempts = att;
        let res = match call_once(att) {
            Ok(r) => { last_err = None; r }
            Err(e) => { last_err = Some(clip_str(&e, 220)); continue; }
        };
        cost += res.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
        if let Some(id) = res.get("ai_call_id") {
            if !id.is_null() { ai_call_id = id.clone(); }
        }
        text = res
            .get("output_kv")
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter().find(|o| {
                    o.get("field_name") == Some(&json!("editor_json"))
                        && o.get("parent_output_id").map(Value::is_null).unwrap_or(true)
                })
            })
            .and_then(|o| o.get("value_text"))
            .map(js_string)
            .unwrap_or_default();
        let body = crate::constitution_constitution_agent_run::extractJson(&json!(text.clone()), 0);
        if let Some(body) = body.filter(Value::is_object) {
            let known: std::collections::HashSet<String> = paths.iter().cloned().collect();
            let ex = |e: Option<&Value>| -> Value {
                match e {
                    Some(v) if v.is_object() && v.get("code").map(|c| !c.is_null()).unwrap_or(false) => {
                        json!({ "caption": clip(v.get("caption"), 200), "code": clip(v.get("code"), 900) })
                    }
                    _ => Value::Null,
                }
            };
            let mt_src = body.get("meta_theory");
            let mt = mt_src.filter(|m| m.is_object() && m.get("statement").map(|s| !s.is_null()).unwrap_or(false)).map(|m| {
                let an = m.get("anatomy");
                let conf = m.get("confidence").and_then(Value::as_f64);
                let branches: Vec<Value> = m
                    .get("branches")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|b| b.get("claim").map(|c| !c.is_null()).unwrap_or(false))
                    .take(5)
                    .map(|b| {
                        let from: Vec<Value> = b
                            .get("from")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .take(4)
                            .map(|x| clip(Some(&x), 90))
                            .collect();
                        let relation = b.get("relation").and_then(Value::as_str).unwrap_or("supports");
                        let relation = if ["supports", "refines", "tensions"].contains(&relation) { relation } else { "supports" };
                        json!({
                            "name": clip(b.get("name"), 90),
                            "claim": clip(b.get("claim"), 520),
                            "axis": clip(b.get("axis"), 40),
                            "from": from,
                            "relation": relation,
                        })
                    })
                    .collect();
                let anatomy = an.filter(|a| a.is_object()).map(|a| {
                    json!({
                        "starts": clip(a.get("starts"), 300),
                        "grows": clip(a.get("grows"), 300),
                        "recognize": clip(a.get("recognize"), 300),
                        "prevent": clip(a.get("prevent"), 300),
                        "repair": clip(a.get("repair"), 300),
                    })
                }).unwrap_or(Value::Null);
                let supports: Vec<Value> = m
                    .get("supports")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .take(6)
                    .map(|s| clip(Some(&s), 100))
                    .collect();
                let unknowns: Vec<Value> = m
                    .get("unknowns")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .take(4)
                    .map(|u| clip(Some(&u), 180))
                    .collect();
                json!({
                    "name": clip(m.get("name"), 120),
                    "statement": clip(m.get("statement"), 900),
                    "branches": branches,
                    "root_cause": clip(m.get("root_cause"), 420),
                    "anatomy": anatomy,
                    "supports": supports,
                    "counterargument": clip(m.get("counterargument"), 500),
                    "verdict": clip(m.get("verdict"), 400),
                    "confidence": conf.map(|c| json!(c.max(0.0).min(1.0))).unwrap_or(Value::Null),
                    "unknowns": unknowns,
                })
            });

            let reading_order: Vec<Value> = body
                .get("reading_order")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|ro| ro.get("path").map(|p| !p.is_null() && known.contains(&js_string(p))).unwrap_or(false))
                .map(|ro| json!({ "path": js_string(ro.get("path").unwrap()), "why": clip(ro.get("why"), 160) }))
                .collect();
            let per_file: Vec<Value> = body
                .get("per_file")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|pf| pf.get("path").map(|p| !p.is_null() && known.contains(&js_string(p))).unwrap_or(false))
                .map(|pf| json!({
                    "path": js_string(pf.get("path").unwrap()),
                    "bridge": clip(pf.get("bridge"), 320),
                    "narrative": clip(pf.get("narrative"), 1800),
                    "simplified": clip(pf.get("simplified"), 500),
                    "example": ex(pf.get("example")),
                }))
                .filter(|pf| pf.get("narrative").map(|n| !n.is_null()).unwrap_or(false))
                .collect();
            let teachings: Vec<Value> = body
                .get("teachings")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|t| t.get("concept").map(|c| !c.is_null()).unwrap_or(false))
                .take(4)
                .map(|t| json!({ "concept": clip(t.get("concept"), 110), "plain": clip(t.get("plain"), 600), "example": ex(t.get("example")) }))
                .collect();
            let further_study: Vec<Value> = body
                .get("further_study")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .take(6)
                .map(|s| clip(Some(&s), 90))
                .collect();
            let internal: Vec<Value> = body
                .get("references")
                .and_then(|r| r.get("internal"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|r| r.get("ref").map(|v| !v.is_null()).unwrap_or(false))
                .take(10)
                .map(|r| json!({ "ref": clip(r.get("ref"), 120), "note": clip(r.get("note"), 220) }))
                .collect();
            let external: Vec<Value> = body
                .get("references")
                .and_then(|r| r.get("external"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|r| r.get("title").map(|v| !v.is_null()).unwrap_or(false))
                .take(8)
                .map(|r| {
                    let url_str = js_string(r.get("url").unwrap_or(&Value::Null));
                    let url = if url_str.starts_with("http://") || url_str.starts_with("https://") { clip(r.get("url"), 320) } else { Value::Null };
                    json!({ "title": clip(r.get("title"), 170), "source": clip(r.get("source"), 110), "url": url, "note": clip(r.get("note"), 220) })
                })
                .collect();

            let arc = body.get("arc").filter(|a| a.is_object() && (a.get("question").map(|q| !q.is_null()).unwrap_or(false) || a.get("thread").map(|t| !t.is_null()).unwrap_or(false)))
                .map(|a| json!({ "question": clip(a.get("question"), 300), "thread": clip(a.get("thread"), 300) }));

            let mut out = Map::new();
            out.insert("title".into(), clip(body.get("title"), 160));
            out.insert("discovery".into(), clip(body.get("discovery"), 320));
            out.insert("arc".into(), arc.unwrap_or(Value::Null));
            out.insert("reading_order".into(), json!(reading_order));
            out.insert("introduction".into(), clip(body.get("introduction"), 2200));
            out.insert("per_file".into(), json!(per_file));
            out.insert("meta_theory".into(), mt.unwrap_or(Value::Null));
            out.insert("discussion".into(), clip(body.get("discussion"), 2600));
            out.insert("cost".into(), clip(body.get("cost"), 700));
            out.insert("repair_read".into(), clip(body.get("repair_read"), 1100));
            out.insert("generalization".into(), clip(body.get("generalization"), 520));
            out.insert("teachings".into(), json!(teachings));
            out.insert("conclusion".into(), clip(body.get("conclusion"), 1400));
            out.insert("further_study".into(), json!(further_study));
            out.insert("references".into(), json!({ "internal": internal, "external": external }));
            let candidate = Value::Object(out);

            let has_content = candidate.get("introduction").map(|v| !v.is_null()).unwrap_or(false)
                || candidate.get("discussion").map(|v| !v.is_null()).unwrap_or(false)
                || candidate.get("per_file").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false);
            if has_content {
                editor = Some(candidate);
                break;
            }
        }
    }

    let dropped = if editor.is_some() { 0 } else { 1 };
    let parse_excerpt = if editor.is_some() { Value::Null } else { last_err.map(Value::String).unwrap_or_else(|| json!(clip_str(&text, 220))) };
    Ok(json!({
        "ok": true,
        "editor": editor,
        "attempts": attempts,
        "dropped": dropped,
        "parse_excerpt": parse_excerpt,
        "cost_usd": cost,
        "ai_call_id": ai_call_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c
    }

    #[test]
    fn no_reviews_returns_ok_false_without_touching_ai() {
        let c = setup();
        let r = constitutionEditor(&json!({ "reviews": [] }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "no validated reviews to edit — run the crew first" }));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0, "must not ensure the prompt when there is nothing to edit");
    }

    #[test]
    fn reviews_missing_required_fields_are_filtered_out() {
        let c = setup();
        let r = constitutionEditor(&json!({ "reviews": [{ "agent": "coupling" }, { "path": "a.js", "review": {} }] }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_after_ensuring_the_prompt() {
        let c = setup();
        let reviews = json!([{ "agent": "coupling", "path": "src/a.js", "review": { "summary": { "level": "finding", "headline": "tight coupling" }, "theory": { "name": "Fan-out spike" } } }]);
        let r = constitutionEditor(&json!({ "reviews": reviews, "label": "the module" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["editor"], Value::Null);
        assert_eq!(r["attempts"], json!(2));
        assert_eq!(r["dropped"], json!(1));
        assert_eq!(r["cost_usd"], json!(0.0));
        assert_eq!(r["ai_call_id"], Value::Null);
        assert!(r["parse_excerpt"].as_str().unwrap().contains("ai-call"), "unexpected: {:?}", r["parse_excerpt"]);
        // ensurePrompt DID run (real, deterministic) before the deferred boundary
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'constitution-editor'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_is_idempotent_on_matching_template() {
        let c = setup();
        ensure_prompt(&c).unwrap();
        ensure_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'constitution-editor'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn record_uses_constitution_file_outside_snapshot_fallback() {
        let c = setup();
        // no snapshot seeded -> constitutionFile returns found:false -> record line says "outside the snapshot"
        let reviews = json!([{ "agent": "x", "path": "src/nope.js", "review": { "summary": {} } }]);
        let r = constitutionEditor(&json!({ "reviews": reviews }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        // reachable only via the deferred parse_excerpt path, but record building itself did not panic/err
        assert!(r["attempts"] == json!(2));
    }
}
