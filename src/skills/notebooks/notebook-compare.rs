//! notebook-compare.rs — twin of skills/notebooks/notebook-compare.mjs (db lane, HIGH-tier AI
//! call). NAMES ARE CONTRACT: default `notebookCompare`. Loads two notebooks' full corpora
//! (`resolveCorpus`, reused from `notebooks_notebook_corpus`), asks the AI for a structured
//! markdown comparison (matrix + key-differences narrative + recommendation), and stores it as a
//! `compare` item on notebook A (`insertItem`, reused from `notebooks_notebook_store`).
//!
//! `context.db` missing → JS throws `err.contextMissing('notebook-compare', 'db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention as every sibling
//! db-lane twin).
//!
//! PARITY-DEFERRED: the AI call goes through `crate::ai_ai_call::aiCall`, the deferred network-AI
//! stub (see ai-call.rs's module doc) — it always returns `Err`. The JS **CATCHES** the aiCall
//! error (and any error `ensurePrompt` might raise) in a `try { ensurePrompt(db); await aiCall(...);
//! ... } catch (e) { return { ok:false, error: ... } }` block — it does NOT propagate/throw. This
//! twin mirrors that exactly: `ensure_prompt` + `aiCall` run inside an inner closure whose `Err` is
//! turned into `Ok(json!({"ok":false,"error":...}))`, never a Rust `Err` out of `notebookCompare`.
//! Everything else is ported faithfully and IS the deterministic, fixture-reachable surface: the
//! schema ensure, both notebook loads + the "both required" guard, both `resolveCorpus` calls, the
//! `fmt_corpus` markdown-block renderer (pure, unit-tested directly), `ensure_prompt` (reusing the
//! already-converted `ai_prompt_find`/`ai_prompt_add` twins, mirroring book-run.rs's
//! `ensure_faculty`), and the final `insertItem` write — reached with the deferred-boundary error
//! message standing in for the AI's `compare_md`, exactly like the JS's own catch path would
//! produce.
// QUALITY: deterministic assembly/db-read/write surface (fully ported, fixture-reachable through
// and past the deferred aiCall boundary since the JS CATCHES it) + 1 pure helper (`fmt_corpus`,
// unit-tested directly) + 1 bespoke/deferred aiCall dispatch (no fixture/frontier for aiCall itself).
use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
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
} // = JS String(v) (no ??)
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
} // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
} // JS `??` right operand

const PROMPT: &str = "notebook.compare.v2";
const SYSTEM: &str = r#"You compare two software scopes for a researcher. Ground ONLY in the two provided corpora. Be specific and fair; cite [path]/[db#id]. Never invent."#;
const TEMPLATE: &str = r#"Compare scope A vs scope B, grounded only in their corpora.

=== SCOPE A: {{title_a}} ===
{{corpus_a}}

=== SCOPE B: {{title_b}} ===
{{corpus_b}}

Return markdown with THREE sections:
1. A COMPARISON TABLE — rows = meaningful dimensions (purpose · structure · dependencies · tests · risks · maturity · …), columns: | Dimension | A | B |.
2. "Key differences" — a short narrative of what actually differs and why it matters.
3. "Recommendation" — one or two lines.
Ground every claim with a [citation]."#;

/// `(c.blocks || []).map(b => \`=== ${String(b.label).toUpperCase()} ===\n${b.text}\`).join('\n\n').slice(0,9000) || '(empty — no scope/sources)'`.
fn fmt_corpus(c: &Value) -> String {
    let blocks = c.get("blocks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let joined = blocks
        .iter()
        .map(|b| {
            let label = js_string(&b.get("label").cloned().unwrap_or(Value::Null)).to_uppercase();
            format!("=== {label} ===\n{}", tmpl(b.get("text")))
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let capped: String = joined.chars().take(9000).collect();
    if capped.is_empty() { "(empty — no scope/sources)".to_string() } else { capped }
}

/// mirrors JS `ensurePrompt`: `try { promptFind(...) } catch { null }` then, if not found,
/// `promptAdd(...)` (NOT wrapped in its own try — a promptAdd failure propagates to the caller's
/// outer catch, same as here via `?`).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).is_ok() {
        return Ok(());
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "high",
            "default_temperature": "cold",
            "max_output_tokens": 2400,
            "phase_flow": "single_only",
            "output_fields": [{ "field_name": "compare_md", "field_type": "string", "required": true }],
        }),
        db,
    )?;
    Ok(())
}

/// `export default async function notebookCompare(input, context)`.
pub fn notebookCompare(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));
    let a = crate::notebooks_notebook_store::getNotebook(db, input.get("id_a").unwrap_or(&Value::Null));
    let b = crate::notebooks_notebook_store::getNotebook(db, input.get("id_b").unwrap_or(&Value::Null));
    if a.is_null() || b.is_null() {
        return Ok(json!({ "ok": false, "error": "both id_a and id_b are required (existing notebooks)" }));
    }

    let ca = crate::notebooks_notebook_corpus::resolveCorpus(&a, db)?;
    let cb = crate::notebooks_notebook_corpus::resolveCorpus(&b, db)?;

    // ── the AI call: JS wraps ensurePrompt + aiCall in try/catch and CATCHES (returns ok:false),
    // never throws. Mirror with an inner closure whose Err becomes the ok:false branch below.
    let attempt: Result<(String, Value), String> = (|| {
        ensure_prompt(db)?;
        let tier = if truthy(input.get("tier")) { input.get("tier").cloned().unwrap() } else { json!("high") };
        let model_id = nz(input.get("model_id")).cloned().unwrap_or(Value::Null);
        let ai_input = json!({
            "prompt_name": PROMPT,
            "tier": tier,
            "model_id": model_id,
            "provider_fn": Value::Null,
            "input_kv": [
                { "key": "title_a", "value": a.get("title").cloned().unwrap_or(Value::Null) },
                { "key": "corpus_a", "value": fmt_corpus(&ca) },
                { "key": "title_b", "value": b.get("title").cloned().unwrap_or(Value::Null) },
                { "key": "corpus_b", "value": fmt_corpus(&cb) },
            ],
        });
        let res = crate::ai_ai_call::aiCall(&ai_input, db)?;
        let md_val = res.get("output_kv").and_then(|v| v.as_array()).and_then(|arr| {
            arr.iter().find(|o| {
                o.get("field_name").and_then(Value::as_str) == Some("compare_md")
                    && o.get("parent_output_id").map_or(true, |v| v.is_null())
            })
        });
        // JS: `...?.value_text || ''` — falsy value_text (missing/empty/0/false) becomes ''.
        let md = md_val.and_then(|o| o.get("value_text")).filter(|v| truthy(Some(v))).map(|v| str_qq(Some(v))).unwrap_or_default();
        let ai_meta = crate::notebooks_notebook_store::aiMeta(db, &res);
        Ok((md, ai_meta))
    })();

    let (md, ai_meta) = match attempt {
        Ok(v) => v,
        Err(e) => return Ok(json!({ "ok": false, "error": format!("notebook-compare AI failed: {e} — check the OpenRouter provider/model.") })),
    };
    let md_final = if md.trim().is_empty() { "_(the model returned an empty comparison.)_".to_string() } else { md };

    let mut meta = ai_meta.as_object().cloned().unwrap_or_default();
    meta.insert("compared_with".to_string(), b.get("id").cloned().unwrap_or(Value::Null));
    meta.insert("compared_title".to_string(), b.get("title").cloned().unwrap_or(Value::Null));

    let title = format!("{} vs {}", tmpl(a.get("title")), tmpl(b.get("title")));
    let item = crate::notebooks_notebook_store::insertItem(
        db,
        a.get("id").unwrap_or(&Value::Null),
        &json!({ "kind": "compare", "title": title, "body_md": md_final, "meta": Value::Object(meta), "pinned": 0 }),
    );
    Ok(json!({ "ok": true, "item": item }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS nb; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        )
        .unwrap();
        c
    }

    #[test]
    fn missing_notebook_returns_ok_false_without_calling_ai() {
        let c = setup();
        let r = notebookCompare(&json!({ "id_a": 1, "id_b": 2 }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("both id_a and id_b are required (existing notebooks)"));
    }

    #[test]
    fn full_deterministic_surface_then_deferred_ai_boundary_is_caught_not_thrown() {
        let c = setup();
        crate::notebooks_notebook_schema::ensureNotebookSchema(&c, &json!("nb."));
        c.execute_batch(
            "INSERT INTO nb.notebooks (id, title, scope_json) VALUES (1, 'Alpha', '{}');
             INSERT INTO nb.notebooks (id, title, scope_json) VALUES (2, 'Beta', '{}');",
        )
        .unwrap();
        let r = notebookCompare(&json!({ "id_a": 1, "id_b": 2 }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        let err = r["error"].as_str().unwrap();
        assert!(err.starts_with("notebook-compare AI failed:"), "unexpected error: {err}");
        assert!(err.ends_with("— check the OpenRouter provider/model."));
    }

    #[test]
    fn fmt_corpus_joins_uppercases_labels_and_defaults_when_empty() {
        let c = json!({ "blocks": [ { "label": "code", "text": "hello" }, { "label": "notes", "text": "world" } ] });
        assert_eq!(fmt_corpus(&c), "=== CODE ===\nhello\n\n=== NOTES ===\nworld");
        assert_eq!(fmt_corpus(&json!({ "blocks": [] })), "(empty — no scope/sources)");
        assert_eq!(fmt_corpus(&json!({})), "(empty — no scope/sources)");
    }
}
