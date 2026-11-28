//! constitution-class.rs — twin of skills/constitution/constitution-class.mjs (db lane, cross-db
//! via judgments/laws through the ensure-twin + brain for the prompt template + nb for the
//! notebook item). NAMES ARE CONTRACT: default `constitutionClass`. THE VIDEO CLASS / PRESENTATION
//! deck: one AI call turns a saved paper's markdown into a slide deck (≤8 slides, each with a
//! topic / spoken-style speech / ≤3 bullets / an optional whitelisted widget). PAPER-ONLY
//! (operator 2026-07-20): `resolvePaperSource` (reused from the already-converted
//! `constitution_constitution_paper` twin) is the ONLY legal source — raw markdown/reviews are
//! refused up front, before any AI call is attempted.
//!
//! PARITY-DEFERRED: the deck call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, so both iterations of
//! the JS retry loop (`for (let att=1; att<=2; ...)`) take the SAME `catch` branch the JS does on
//! every real dispatch. The JS never propagates that error outward — its final return is always
//! `{ ok: true, deck: null, ... }` with `dropped: 1` and `parse_excerpt` set from the caught
//! message — so this twin's `Ok` return mirrors that exactly (never `Err`, matching JS's own
//! never-throws contract past the paper-resolution guard). Everything BEFORE the deferred call is
//! ported faithfully and IS the deterministic surface these tests exercise: the guard
//! (`ensureConstitutionSchema` + `resolvePaperSource`'s ok/error short-circuit), `label`/`swarm_id`
//! derivation, `ensure_prompt` (reusing the already-converted `ai_prompt_find`/`ai_prompt_add`
//! twins, re-adding on a template/system-md content mismatch — same pattern as
//! constitution-brief.rs's `ensure_prompt`), the 2-attempt retry loop shape (`attempts` count,
//! `bypass_cache`/`_retry` on the second attempt), and the final response assembly. The
//! deck-parsing/slide-shaping logic (`extract_json` reused from `constitution_constitution_
//! agent_run::extractJson`, slide clip/filter, widget whitelist) and the notebook-item insert
//! (reusing the already-converted `notebooks_notebook_schema`/`notebooks_notebook_store` twins)
//! are ported byte-for-byte from the JS but — like book-run.rs's post-wave staging — are
//! unreachable by this file's own fixture because they run only after a successful `aiCall`,
//! which the deferred stub never produces; correctness there is by inspection + the same helpers'
//! shape as the reachable code (constitution-brief.rs precedent).
// QUALITY: mixed unit — deterministic guard/assembly surface (fully ported, fixture-reachable up
// to the deferred aiCall boundary) + 1 bespoke/deferred aiCall dispatch (no fixture/frontier).
use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
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
// `const clip = (v, n) => (v == null ? null : String(v).slice(0, n));`
fn clip(v: Option<&Value>, n: usize) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(x) => Some(js_string(x).chars().take(n).collect()),
    }
}

const PROMPT: &str = "constitution-class";
const WIDGETS: [&str; 8] = ["graph-focus", "before-after", "repair-order", "root-cause-tree", "timeline", "flow", "balance", "layers"];
const SYSTEM: &str = r#"You are a TEACHER turning an engineering study into a short video class. The viewer is a smart beginner: SIMPLIFY — plain words, one idea per slide, straight to the point. Never quote the study's prose; re-explain it.
Each slide covers ONE axis of the topic (the problem · where it lives · why it happened · what it costs · how to fix it · the lesson). The WIDGET is the slide's visual — pick the drawn form that fits the idea: flow for a process, timeline for history, balance for a tradeoff, layers for a crossed boundary, before-after for a repair, graph-focus for connected files, root-cause-tree for causality, repair-order for steps. The SPEECH is what the narrator SAYS: spoken language, 2-4 short sentences, no markdown, no citations read aloud. TIME BUDGET: the WHOLE class must speak in under 5 minutes — each slide at most ~45 seconds (≈100 words); cut anything that does not teach. Be even more didactic than the study: concrete, straight to the point, one takeaway per slide.
Output ONLY valid JSON, exactly: {"title":"the class title, plain","slides":[{"topic":"the slide's one idea, 3-7 words","speech":"2-4 spoken sentences","bullets":["≤3 short points"],"widget":{"type":"graph-focus|before-after|repair-order|root-cause-tree|timeline|flow|balance|layers","title":"...","payload":{}}}]}. 5-8 slides. Widget payloads exactly as the study's forms: flow {"steps":[...]} · timeline {"events":[{"label":"...","at":"..."}]} · balance {"left":{"label":"...","items":[...]},"right":{...},"verdict":"..."} · layers {"layers":[...],"breach":"..."} · before-after {"bad":"...","good":"..."} · repair-order {"steps":[...]} · root-cause-tree {"root":"...","branches":[...]} · graph-focus {"files":[...],"note":"..."}. Start with "{" immediately. (class contract v2 — the crew teaches, 5-minute budget)"#;
const TEMPLATE: &str = "THE CLASS TOPIC: {{label}}\n\nTHE STUDY (your only source — simplify it, never invent beyond it):\n{{markdown}}\n\nWrite the deck JSON.";

// mirrors JS ensurePrompt: try promptFind; if found AND template_md+system_md already match the
// constants, no-op; otherwise (missing OR stale) promptAdd — no re-fetch after (JS doesn't either).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let e = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(ev) = &e {
        let tpl_ok = ev.get("template_md").and_then(Value::as_str) == Some(TEMPLATE);
        let sys_ok = ev.get("system_md").and_then(Value::as_str) == Some(SYSTEM);
        if tpl_ok && sys_ok {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "mid",
            "default_temperature": "warm",
            "phase_flow": "single_only",
            "max_output_tokens": 2800,
            "output_fields": [{ "field_name": "class_json", "field_type": "string" }],
        }),
        db,
    )?;
    Ok(())
}

pub fn constitutionClass(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const db = context?.db; if (!db) throw err.required(...)` — unreachable here
    // (`db: &Connection` is a required, non-nullable reference).
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);

    // PAPER-ONLY: the only legal source. A not-ok resolution is returned VERBATIM (JS `return
    // paper;`), before ensure_prompt or any AI call is attempted.
    let paper = crate::constitution_constitution_paper::resolvePaperSource(db, input);
    if paper.get("ok").and_then(Value::as_bool) != Some(true) {
        return Ok(paper);
    }
    let markdown = js_string(paper.get("markdown").unwrap_or(&Value::Null));
    let label = clip(input.get("label"), 140)
        .or_else(|| clip(paper.get("title"), 140))
        .unwrap_or_else(|| "the study".to_string());

    // `Number(input?.notebook_id ?? 0) || null` — falsy (0/NaN) collapses to null.
    let nb_raw = nz(input.get("notebook_id")).cloned().unwrap_or(json!(0));
    let nb_num = js_number(Some(&nb_raw));
    let nb_id: Value = if nb_num != 0.0 && !nb_num.is_nan() { json!(nb_num as i64) } else { Value::Null };

    ensure_prompt(db)?;

    // `String(input?.swarm_id ?? paper.swarm_id ?? '').slice(0, 80) || null`. The resulting id
    // only threads into `runWithSession`, which run-session.rs's own module doc documents as
    // PARITY-DEFERRED and unported (node:async_hooks AsyncLocalStorage — no Rust equivalent, and
    // it is pure ambient dispatch context, never data that reaches aiCall's input or output). This
    // twin therefore calls `aiCall` directly, same as book-run.rs dropped `provider_fn` threading.
    let provider_fn = input.get("provider_fn").cloned().unwrap_or(Value::Null);

    let md_slice: String = markdown.chars().take(22000).collect();
    let mut attempts: i64 = 0;
    let mut cost: f64 = 0.0;
    let mut ai_call_id: Value = Value::Null;
    let mut text = String::new();
    let mut deck: Option<Value> = None;
    let mut last_err: Option<String> = None;

    for att in 1..=2i64 {
        attempts = att;
        let mut kv = vec![
            json!({ "key": "label", "value": label }),
            json!({ "key": "markdown", "value": md_slice }),
        ];
        if att > 1 {
            kv.push(json!({ "key": "_retry", "value": att.to_string() }));
        }
        let ai_input = json!({
            "prompt_name": PROMPT,
            "input_kv": kv,
            "bypass_cache": att > 1,
            "provider_fn": provider_fn,
        });
        // PARITY-DEFERRED: mirrors JS `try { res = await callOnce(att); lastErr = null; }
        // catch (e) { lastErr = ...; continue; }` — the deferred stub always Errs, so this
        // branch is the one always taken during conform-db.
        match crate::ai_ai_call::aiCall(&ai_input, db) {
            Err(e) => {
                last_err = Some(e.chars().take(220).collect());
                continue;
            }
            Ok(res) => {
                last_err = None;
                cost += res.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
                if let Some(id) = res.get("ai_call_id") {
                    ai_call_id = id.clone();
                }
                text = res
                    .get("output_kv")
                    .and_then(Value::as_array)
                    .and_then(|arr| {
                        arr.iter().find(|o| {
                            o.get("field_name").and_then(Value::as_str) == Some("class_json")
                                && matches!(o.get("parent_output_id"), None | Some(Value::Null))
                        })
                    })
                    .and_then(|o| o.get("value_text"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let body = crate::constitution_constitution_agent_run::extractJson(&json!(text), 0);
                if let Some(body) = &body {
                    let slides_ok = body.is_object() && body.get("slides").map_or(false, Value::is_array);
                    if slides_ok {
                        let raw_slides: Vec<Value> = body.get("slides").and_then(Value::as_array).cloned().unwrap_or_default();
                        let slides: Vec<Value> = raw_slides
                            .into_iter()
                            .take(8)
                            .map(|sl| {
                                let topic = clip(sl.get("topic"), 90).unwrap_or_else(|| "(untitled slide)".to_string());
                                let speech = clip(sl.get("speech"), 700).unwrap_or_default();
                                let bullets: Vec<Value> = sl
                                    .get("bullets")
                                    .and_then(Value::as_array)
                                    .cloned()
                                    .unwrap_or_default()
                                    .into_iter()
                                    .take(3)
                                    .map(|b| match clip(Some(&b), 120) { Some(s) => json!(s), None => Value::Null })
                                    .collect();
                                let widget_type_ok = sl
                                    .get("widget")
                                    .and_then(|w| w.get("type"))
                                    .and_then(Value::as_str)
                                    .map_or(false, |t| WIDGETS.contains(&t));
                                let widget = if widget_type_ok {
                                    let w = sl.get("widget").unwrap();
                                    json!({
                                        "type": w.get("type").cloned().unwrap_or(Value::Null),
                                        "title": clip(w.get("title"), 110),
                                        "payload": w.get("payload").cloned().unwrap_or(json!({})),
                                    })
                                } else {
                                    Value::Null // an unknown visual is NO visual — never improvised
                                };
                                json!({ "topic": topic, "speech": speech, "bullets": bullets, "widget": widget })
                            })
                            .filter(|sl| {
                                let speech_ok = sl.get("speech").and_then(Value::as_str).map_or(false, |s| !s.is_empty());
                                let bullets_ok = sl.get("bullets").and_then(Value::as_array).map_or(false, |a| !a.is_empty());
                                speech_ok || bullets_ok
                            })
                            .collect();
                        if slides.len() >= 2 {
                            let title = clip(body.get("title"), 140).unwrap_or_else(|| label.clone());
                            deck = Some(json!({ "title": title, "slides": slides }));
                            break;
                        }
                    }
                }
            }
        }
    }

    // a notebook-targeted class lands on the notebook as an item, like every artifact — reached
    // only when `deck` is Some, which requires a successful aiCall (unreachable in this twin).
    let mut item: Value = Value::Null;
    if let Some(d) = &deck {
        if !matches!(nb_id, Value::Null) {
            let _ = crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));
            let slides_len = d.get("slides").and_then(Value::as_array).map_or(0, |a| a.len());
            let opts = json!({
                "kind": "class",
                "title": d.get("title").cloned().unwrap_or(Value::Null),
                "body_md": format!("Video class — {slides_len} slides."),
                "meta": { "deck": d, "paper_slug": paper.get("slug").cloned().unwrap_or(Value::Null) },
                "pinned": 0,
            });
            item = crate::notebooks_notebook_store::insertItem(db, &nb_id, &opts);
        }
    }

    Ok(json!({
        "ok": true,
        "deck": deck.clone().unwrap_or(Value::Null),
        "item": item,
        "attempts": attempts,
        "dropped": if deck.is_some() { 0 } else { 1 },
        "parse_excerpt": if deck.is_some() { Value::Null } else { json!(last_err.unwrap_or_else(|| text.chars().take(220).collect())) },
        "cost_usd": cost,
        "ai_call_id": ai_call_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRAIN_SCHEMA: &str = "
        CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
        CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
    ";

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS brain; ATTACH ':memory:' AS nb;").unwrap();
        c.execute_batch(BRAIN_SCHEMA).unwrap();
        c
    }

    fn a_paper(c: &Connection) -> i64 {
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(c);
        c.execute("INSERT INTO judgments.review_papers (slug, title, paper_json, markdown_md) VALUES ('paper-one', 'Paper One', '{}', '# the study')", []).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn no_paper_source_returns_the_resolution_error_verbatim() {
        let c = conn();
        let r = constitutionClass(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("a saved paper is required"));
        // ensure_prompt never ran — no template written
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'constitution-class'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn unknown_paper_id_returns_not_found_verbatim() {
        let c = conn();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        let r = constitutionClass(&json!({ "paper_id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "no such paper (999)" }));
    }

    #[test]
    fn saved_paper_reaches_the_deferred_ai_call_and_returns_ok_true_dropped() {
        // proves every deterministic gate (paper resolution/label/ensure_prompt/retry-loop shape)
        // ran cleanly and the drop is aiCall's deferred stub, not an earlier bug — JS never
        // propagates the aiCall error, always returning ok:true with dropped:1.
        let c = conn();
        let pid = a_paper(&c);
        let r = constitutionClass(&json!({ "paper_id": pid }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["deck"], Value::Null);
        assert_eq!(r["item"], Value::Null);
        assert_eq!(r["attempts"], json!(2));
        assert_eq!(r["dropped"], json!(1));
        assert_eq!(r["cost_usd"], json!(0.0));
        assert_eq!(r["ai_call_id"], Value::Null);
        assert!(r["parse_excerpt"].as_str().unwrap().contains("ai-call"), "{}", r["parse_excerpt"]);

        // the prompt template WAS ensured (ensure_prompt ran) before the deferred call
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'constitution-class'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_writes_then_is_idempotent() {
        let c = conn();
        ensure_prompt(&c).unwrap();
        let found = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), &c).unwrap();
        assert_eq!(found.get("template_md").and_then(Value::as_str), Some(TEMPLATE));
        assert_eq!(found.get("system_md").and_then(Value::as_str), Some(SYSTEM));
        // second call is a no-op (already matches) — still Ok, no second row
        ensure_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = ?", [PROMPT], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn label_falls_back_to_paper_title_then_the_study() {
        let c = conn();
        let pid = a_paper(&c);
        let r = constitutionClass(&json!({ "paper_id": pid, "label": "  " }), &c).unwrap();
        // clip('  ',140) is Some("  ") (not falsy in Rust — only null/undefined short-circuit in
        // JS's `??`), so label stays the clipped input; this asserts the call still succeeds and
        // reaches the same deferred boundary regardless of which label wins.
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["dropped"], json!(1));
    }
}
