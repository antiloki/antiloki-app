//! notebook-flashcards.rs — twin of skills/notebooks/notebook-flashcards.mjs (db + cross-db
//! brain.* via reused ai twins). NAMES ARE CONTRACT: default `notebookFlashcards`. NotebookLM-style
//! learning flashcards for a notebook's scope: the AI returns STRICT JSON (front/back pairs),
//! grounded in the corpus; stored as a `flashcards` item (meta.cards).
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`. The JS wraps
//! `ensurePrompt(db)` + `await aiCall(...)` together in ONE `try { } catch (e) { return { ok:
//! false, error: \`notebook-flashcards AI failed: ${e?.message || 'provider error'} — check the
//! OpenRouter provider/model.\` } }` — a failed provider (or a failed ensurePrompt/promptAdd)
//! degrades GRACEFULLY, it never throws out of the skill. This twin mirrors that exactly with an
//! inline closure that propagates both `ensure_prompt`'s and `aiCall`'s `Err` into the SAME
//! catch-all `Ok({ok:false,...})` branch. `resolveCorpus` (before the try in JS) is NOT caught —
//! its `Err` propagates via `?`, matching the JS behavior of letting that throw escape unhandled.
//! Everything else — guards, corpus/context assembly, the fence-stripping + brace-slice +
//! `sanitize` response-parse helpers (pure, unit-tested directly since aiCall's Err
//! short-circuits before they run against a real payload), and `insertItem`'s meta shaping — is
//! ported faithfully. Reused twins: `notebooks_notebook_schema::ensureNotebookSchema`,
//! `notebooks_notebook_store::{getNotebook, insertItem, aiMeta}`, `notebooks_notebook_corpus::resolveCorpus`,
//! `ai_prompt_find::promptFind`, `ai_prompt_add::promptAdd`.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
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
} // = JS String(v ?? '')
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
} // = JS String(v)
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
} // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
} // = JS Number(v)
fn falsy_num(n: f64) -> bool { n == 0.0 || n.is_nan() }
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }

const PROMPT: &str = "notebook.flashcards.v1";
const SYSTEM: &str = "You write study flashcards about a software scope, grounded ONLY in the provided context. Output STRICT JSON — no prose, no markdown, no code fences.";
const TEMPLATE: &str = "NOTEBOOK SCOPE: {{scope_title}}

=== SOURCES (code + signals) ===
{{code_context}}

Return FLASHCARDS as STRICT JSON (no markdown / no fences):
{\"cards\":[{\"front\":\"<term or question>\",\"back\":\"<definition or answer>\"}]}
Rules: 10–15 cards covering the core concepts of the scope; front is short (a term/question), back is the concise grounded answer. Output ONLY the JSON object.";

// `function ensurePrompt(db)` — JS wraps ONLY the promptFind lookup in its own try/catch
// (existence-check errors fold to "not found" → add); a promptAdd failure propagates OUT of
// ensurePrompt into notebookFlashcards' outer catch (mirrored: this fn's Err bubbles the same way).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let ex = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if ex.is_none() {
        let output_fields = json!([{ "field_name": "cards_json", "field_type": "string", "required": true }]);
        crate::ai_prompt_add::promptAdd(
            &json!({
                "name": PROMPT,
                "template_md": TEMPLATE,
                "system_md": SYSTEM,
                "default_tier": "high",
                "default_temperature": "cold",
                "max_output_tokens": 2200,
                "phase_flow": "single_only",
                "output_fields": output_fields,
            }),
            db,
        )?;
    }
    Ok(())
}

// `/^```[a-z]*\n?/i` — strip an optional leading fenced-code marker + language tag + one newline.
fn strip_leading_fence(s: &str) -> String {
    if !s.starts_with("```") { return s.to_string(); }
    let rest = &s[3..];
    let mut idx = 0;
    for c in rest.chars() {
        if c.is_ascii_alphabetic() { idx += c.len_utf8(); } else { break; }
    }
    let after_lang = &rest[idx..];
    after_lang.strip_prefix('\n').unwrap_or(after_lang).to_string()
}
// `/```\s*$/` — strip a trailing fence marker (only when the trimmed-end tail actually matches;
// a non-match leaves the string byte-for-byte untouched, matching JS .replace() semantics).
fn strip_trailing_fence(s: &str) -> String {
    let te = s.trim_end();
    if te.ends_with("```") { te[..te.len() - 3].to_string() } else { s.to_string() }
}

// `String(c?.front ?? c?.term ?? c?.q ?? '')` style chained-`??` string pick.
fn pick_str(c: &Value, keys: &[&str]) -> String {
    for k in keys {
        if let Some(v) = nz(c.get(*k)) { return str_qq(Some(v)); }
    }
    String::new()
}

// `function sanitize(obj)` — pure, canned-value-tested (aiCall's Err short-circuits this in the
// real path). `cs.slice(0,24).map(...).filter(...)`.
fn sanitize(obj: &Value) -> Vec<Value> {
    let cs: Vec<Value> = obj.get("cards").and_then(Value::as_array).cloned().unwrap_or_default();
    cs.into_iter()
        .take(24)
        .filter_map(|c| {
            let front: String = pick_str(&c, &["front", "term", "q"]).chars().take(300).collect::<String>().trim().to_string();
            let back: String = pick_str(&c, &["back", "definition", "a"]).chars().take(700).collect::<String>().trim().to_string();
            if front.is_empty() || back.is_empty() { None } else { Some(json!({ "front": front, "back": back })) }
        })
        .collect()
}

pub fn notebookFlashcards(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS destructures { db } from context and throws err.contextMissing without it — the harness
    // context always carries db, so that guard cannot fire here.
    crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));

    let id_n = js_number(input.get("id"));
    if falsy_num(id_n) { return Err("notebook-flashcards: id is required".into()); }
    let id_v = num_param(id_n);
    let nb = crate::notebooks_notebook_store::getNotebook(db, &id_v);
    if nb.is_null() {
        return Ok(json!({ "ok": false, "error": format!("notebook {} not found", tmpl(Some(&id_v))) }));
    }

    let corpus = crate::notebooks_notebook_corpus::resolveCorpus(&nb, db)?; // NOT caught in JS — propagates.
    let blocks: Vec<Value> = corpus.get("blocks").and_then(Value::as_array).cloned().unwrap_or_default();
    let code_context: String = if !blocks.is_empty() {
        blocks
            .iter()
            .map(|b| {
                let label_v = b.get("label").cloned().unwrap_or(Value::Null);
                let label_upper = js_string(&label_v).to_uppercase();
                format!("=== {} ===\n{}", label_upper, tmpl(b.get("text")))
            })
            .collect::<Vec<String>>()
            .join("\n\n")
    } else {
        "(no sources — set a scope or add sources.)".to_string()
    };

    let title_v = nb.get("title").cloned().unwrap_or(Value::Null);
    let tier_v = if truthy(input.get("tier")) { input.get("tier").cloned().unwrap() } else { json!("high") };
    let model_id_v = if truthy(input.get("model_id")) { input.get("model_id").cloned().unwrap() } else { Value::Null };
    let code_context_capped: String = code_context.chars().take(16000).collect();

    let ai_input = json!({
        "prompt_name": PROMPT,
        "tier": tier_v,
        "model_id": model_id_v,
        "provider_fn": Value::Null,
        "input_kv": [
            { "key": "scope_title", "value": title_v },
            { "key": "code_context", "value": code_context_capped },
        ],
    });

    let outcome: Result<Value, String> = (|| {
        ensure_prompt(db)?;
        crate::ai_ai_call::aiCall(&ai_input, db)
    })();

    let res = match outcome {
        Ok(r) => r,
        Err(e) => {
            let msg = if e.is_empty() { "provider error".to_string() } else { e };
            return Ok(json!({
                "ok": false,
                "error": format!("notebook-flashcards AI failed: {} — check the OpenRouter provider/model.", msg),
            }));
        }
    };

    // Unreachable via the deferred aiCall stub (which always Err's above) — ported byte-for-byte
    // for completeness, exercised directly through `sanitize`/fence helpers in the test module.
    let found = res.get("output_kv").and_then(Value::as_array).and_then(|arr| {
        arr.iter().find(|o| {
            o.get("field_name").and_then(Value::as_str) == Some("cards_json")
                && o.get("parent_output_id").map_or(true, |v| v.is_null())
        })
    });
    let raw_v = found.and_then(|o| o.get("value_text")).cloned();
    let raw0 = if truthy(raw_v.as_ref()) { str_qq(raw_v.as_ref()) } else { String::new() };
    let ai_m = crate::notebooks_notebook_store::aiMeta(db, &res);

    let raw1 = strip_leading_fence(&raw0);
    let raw2 = strip_trailing_fence(&raw1);
    let raw = raw2.trim().to_string();

    let mut cards: Option<Vec<Value>> = None;
    if let (Some(ai), Some(bi)) = (raw.find('{'), raw.rfind('}')) {
        if bi > ai {
            if let Ok(parsed) = serde_json::from_str::<Value>(&raw[ai..=bi]) {
                cards = Some(sanitize(&parsed));
            }
        }
    }

    let cards = match cards {
        Some(c) if !c.is_empty() => c,
        _ => return Ok(json!({ "ok": false, "error": "the model did not return valid flashcards — try again." })),
    };

    let n = cards.len();
    let title = format!("Flashcards · {n}");
    let body_md = format!("Flashcards for **{}** — {n} cards.", tmpl(Some(&nb.get("title").cloned().unwrap_or(Value::Null))));
    let mut meta_map: Map<String, Value> = Map::new();
    meta_map.insert("cards".into(), json!(cards));
    if let Some(aim_obj) = ai_m.as_object() {
        for (k, v) in aim_obj { meta_map.insert(k.clone(), v.clone()); }
    }
    let opts = json!({ "kind": "flashcards", "title": title, "body_md": body_md, "meta": Value::Object(meta_map), "pinned": 0 });
    let item = crate::notebooks_notebook_store::insertItem(db, &id_v, &opts);
    Ok(json!({ "ok": true, "item": item }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS nb; ATTACH ':memory:' AS brain;").unwrap();
        crate::notebooks_notebook_schema::ensureNotebookSchema(&c, &json!("nb."));
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.ai_calls ( id INTEGER PRIMARY KEY, tokens_in INTEGER, tokens_out INTEGER );",
        ).unwrap();
        c
    }

    #[test]
    fn id_is_required() {
        let c = setup();
        let e = notebookFlashcards(&json!({}), &c).unwrap_err();
        assert_eq!(e, "notebook-flashcards: id is required");
        let e2 = notebookFlashcards(&json!({ "id": 0 }), &c).unwrap_err();
        assert_eq!(e2, "notebook-flashcards: id is required");
    }

    #[test]
    fn unknown_notebook_returns_ok_false() {
        let c = setup();
        let out = notebookFlashcards(&json!({ "id": 999 }), &c).unwrap();
        assert_eq!(out, json!({ "ok": false, "error": "notebook 999 not found" }));
    }

    #[test]
    fn deferred_aicall_degrades_to_ok_false_status() {
        let c = setup();
        let (_ch, rowid) = crate::db_json::run_write(
            &c,
            "INSERT INTO nb.notebooks(slug, title, summary, scope_json) VALUES (?, ?, ?, ?)",
            &[json!("s1"), json!("My Notebook"), Value::Null, json!("{}")],
        ).unwrap();
        let out = notebookFlashcards(&json!({ "id": rowid }), &c).unwrap();
        assert_eq!(out["ok"], json!(false));
        let msg = out["error"].as_str().unwrap();
        assert!(msg.starts_with("notebook-flashcards AI failed: "));
        assert!(msg.ends_with("— check the OpenRouter provider/model."));
        // ensure_prompt DID run (deterministic surface reached the AI boundary): prompt row created.
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'notebook.flashcards.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_is_idempotent() {
        let c = setup();
        ensure_prompt(&c).unwrap();
        ensure_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'notebook.flashcards.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn sanitize_trims_slices_and_filters_blank_cards() {
        let obj = json!({ "cards": [
            { "front": "  A  ", "back": "  B  " },
            { "term": "T2", "definition": "D2" },
            { "front": "no back" },
            { "front": "", "back": "x" },
        ] });
        let out = sanitize(&obj);
        assert_eq!(out, vec![
            json!({ "front": "A", "back": "B" }),
            json!({ "front": "T2", "back": "D2" }),
        ]);
    }

    #[test]
    fn sanitize_caps_at_24_and_non_array_cards_yields_empty() {
        let many: Vec<Value> = (0..30).map(|i| json!({ "front": format!("f{i}"), "back": format!("b{i}") })).collect();
        let obj = json!({ "cards": many });
        assert_eq!(sanitize(&obj).len(), 24);
        assert_eq!(sanitize(&json!({})), Vec::<Value>::new());
        assert_eq!(sanitize(&json!({ "cards": "nope" })), Vec::<Value>::new());
    }

    #[test]
    fn fence_stripping_matches_js_regex_semantics() {
        assert_eq!(strip_leading_fence("```json\n{\"a\":1}"), "{\"a\":1}");
        assert_eq!(strip_leading_fence("```\n{\"a\":1}"), "{\"a\":1}");
        assert_eq!(strip_leading_fence("{\"a\":1}"), "{\"a\":1}");
        assert_eq!(strip_trailing_fence("{\"a\":1}\n```\n"), "{\"a\":1}\n");
        assert_eq!(strip_trailing_fence("{\"a\":1}"), "{\"a\":1}");
    }
}
