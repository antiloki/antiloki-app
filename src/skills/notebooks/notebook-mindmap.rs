//! notebook-mindmap.rs — twin of skills/notebooks/notebook-mindmap.mjs (db lane, cross-db
//! brain.* via reused prompt/ai-call twins). NAMES ARE CONTRACT: default `notebookMindmap`.
//! EPIC-NOTEBOOKS: the AI returns a nested JSON tree grounded in the notebook's corpus; stored
//! as a `mindmap` item (`meta.mindmap`). The FE renders it as a collapsible tidy-tree.
//!
//! `context.db` missing → JS throws `err.contextMissing('notebook-mindmap', 'db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention as every
//! sibling db-lane twin). `context.provider_fn` (a JS function reference for provider selection/
//! DI) has no Rust equivalent to carry — threaded as `null` into the constructed aiCall input,
//! same as book-run.rs's `provider_fn`.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`. The JS wraps
//! `ensurePrompt` + the `aiCall` await + the response-string parsing in one `try { } catch (e) {
//! return { ok:false, error: ... } }` — a failed/absent provider degrades GRACEFULLY, it never
//! throws (same shape as file-summarize.rs). This twin mirrors that exactly: on `aiCall` failure
//! it returns `Ok({ ok:false, error: "notebook-mindmap AI failed: <deferred message> — check the
//! OpenRouter provider/model." })`, never propagating the deferred stub's `Err` up to the caller.
//! Everything else — guards, notebook lookup, corpus resolution (reusing the already-converted
//! `notebooks_notebook_corpus::resolveCorpus` twin), `ensurePrompt` (reusing the already-
//! converted `ai_prompt_find`/`ai_prompt_add` twins), and the response-string parsing/`sanitize`
//! tree-shaping helpers — is the real, fully ported deterministic surface. `sanitize` and the
//! fence-stripping parse are ported as their own PURE functions and unit-tested directly with
//! canned raw strings/JSON — unreachable from `notebookMindmap` itself (the deferred `aiCall`
//! folds to the catch branch before either runs) but correct by construction, same pattern as
//! file-summarize.rs's `pick_intent`.
// QUALITY: db-read/write lane (fully ported, fixture-reachable up to the deferred aiCall
// boundary) + 2 pure response-shaping helpers (unreachable from the default export, unit-tested
// directly) + 1 bespoke/deferred aiCall dispatch (no fixture/frontier).

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
fn falsy_num(n: f64) -> bool { n == 0.0 || n.is_nan() }

const PROMPT: &str = "notebook.mindmap.v2";
const SYSTEM: &str = "You build a mind map of a software scope, grounded ONLY in the provided context. Output STRICT JSON — no prose, no markdown, no code fences.";
const TEMPLATE: &str = "NOTEBOOK SCOPE: {{scope_title}}

=== SOURCES (code + signals) ===
{{code_context}}

Return a MIND MAP as STRICT JSON (no markdown / no fences) with this exact shape:
{\"label\":\"<central topic>\",\"children\":[{\"label\":\"<branch>\",\"children\":[{\"label\":\"<leaf>\"}]}]}
Rules: 3–6 top branches; each branch 2–6 children; up to 3 levels deep; labels <= 6 words; group by the real
structure / concepts in the scope. Output ONLY the JSON object.";

fn prompt_output_fields() -> Value {
    json!([{ "field_name": "mindmap_json", "field_type": "string", "required": true }])
}

// `function ensurePrompt(db)`: try promptFind, ignore any error → null; if missing, promptAdd.
// Unlike file-summarize's ensure_summarize_prompt, JS never re-checks template_md drift here.
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let found = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).is_ok();
    if found {
        return Ok(());
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "high",
            "default_temperature": "cold",
            "max_output_tokens": 1600,
            "phase_flow": "single_only",
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    Ok(())
}

// `function sanitize(node, depth = 0)` — keep the tree well-formed + bounded: a label per node,
// <=12 children, <=4 deep. `node.label ?? node.name ?? node.title`, capped 80 chars + trimmed.
fn sanitize(node: &Value, depth: i64) -> Option<Value> {
    if !node.is_object() {
        return None;
    }
    let label_raw = node
        .get("label")
        .filter(|v| !v.is_null())
        .or_else(|| node.get("name").filter(|v| !v.is_null()))
        .or_else(|| node.get("title").filter(|v| !v.is_null()));
    let empty = Value::String(String::new());
    let label: String = js_string(label_raw.unwrap_or(&empty))
        .chars()
        .take(80)
        .collect::<String>()
        .trim()
        .to_string();
    if label.is_empty() {
        return None;
    }
    let mut out = json!({ "label": label });
    let kids: Vec<Value> = match node.get("children") {
        Some(Value::Array(a)) => a.clone(),
        _ => vec![],
    };
    if depth < 4 && !kids.is_empty() {
        let c: Vec<Value> = kids.into_iter().take(12).filter_map(|k| sanitize(&k, depth + 1)).collect();
        if !c.is_empty() {
            out.as_object_mut().unwrap().insert("children".into(), json!(c));
        }
    }
    Some(out)
}

// `raw.replace(/^```[a-z]*\n?/i, '').replace(/```\s*$/, '').trim()` then locate the outermost
// `{...}` slice and JSON.parse it, sanitize()-ing on success, `null` on any failure.
fn parse_tree(raw: &str) -> Option<Value> {
    let mut s = raw;
    // strip a leading fence: ```[a-z]*\n? (case-insensitive lang tag, optional trailing newline)
    if let Some(rest) = s.strip_prefix("```") {
        let lang_end = rest.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(rest.len());
        let (_lang, after_lang) = rest.split_at(lang_end);
        let after_fence = after_lang.strip_prefix('\n').unwrap_or(after_lang);
        s = after_fence;
    }
    let trimmed_end = s.trim_end();
    let s = if let Some(idx) = trimmed_end.rfind("```") {
        if trimmed_end[idx + 3..].trim().is_empty() { &trimmed_end[..idx] } else { trimmed_end }
    } else {
        trimmed_end
    };
    let s = s.trim();
    let a = s.find('{');
    let b = s.rfind('}');
    let (a, b) = match (a, b) {
        (Some(a), Some(b)) if b > a => (a, b),
        _ => return None,
    };
    let slice = &s[a..=b];
    let parsed: Value = serde_json::from_str(slice).ok()?;
    sanitize(&parsed, 0)
}

// count(n) = 1 + sum(count(child)) — used only in the item's body_md, unreachable past the
// deferred aiCall boundary but ported as its own pure fn per the playbook.
fn count_nodes(n: &Value) -> i64 {
    let kids = match n.get("children") {
        Some(Value::Array(a)) => a.clone(),
        _ => vec![],
    };
    1 + kids.iter().map(count_nodes).sum::<i64>()
}

pub fn notebookMindmap(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const db = context?.db; if (!db) throw err.contextMissing(...)` — unreachable here
    // (Rust's `db: &Connection` is never absent). `context.provider_fn` has no Rust carrier — see
    // module doc; passed as `null` into the constructed aiCall input below.
    let provider_fn = Value::Null;
    crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));

    let id_num = js_number(input.get("id"));
    if falsy_num(id_num) {
        return Err("notebook-mindmap: id is required".to_string());
    }
    let id_val = json!(id_num);
    let nb = crate::notebooks_notebook_store::getNotebook(db, &id_val);
    if nb.is_null() {
        return Ok(json!({ "ok": false, "error": format!("notebook {} not found", id_num as i64) }));
    }

    let corpus = crate::notebooks_notebook_corpus::resolveCorpus(&nb, db)?;
    let blocks: Vec<Value> = corpus.get("blocks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let code_context = if blocks.is_empty() {
        "(no sources — set a scope or add sources.)".to_string()
    } else {
        blocks
            .iter()
            .map(|b| {
                let label = js_string(b.get("label").unwrap_or(&Value::Null)).to_uppercase();
                let text = js_string(b.get("text").unwrap_or(&Value::Null));
                format!("=== {label} ===\n{text}")
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    let nb_title = nb.get("title").cloned().unwrap_or(Value::Null);

    // ── the AI wave: PARITY-DEFERRED from here. JS wraps ensurePrompt + the aiCall await + the
    // response-string parsing in one try/catch — a failed/absent provider degrades GRACEFULLY,
    // it never throws. Mirrored here as one closure whose Err short-circuits to the catch branch
    // below (same shape as file-summarize.rs); everything inside is real, ported logic — the
    // success path (`tree`/`ai_m` populated) is unreachable in THIS build only because
    // `crate::ai_ai_call::aiCall` always returns `Err` (the deferred stub), never because it was
    // skipped in the port.
    let wave: Result<(Value, Value), String> = (|| {
        ensure_prompt(db)?;
        let tier = if truthy(input.get("tier")) { input.get("tier").cloned().unwrap() } else { json!("high") };
        let model_id = input.get("model_id").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null);
        let code_context_capped: String = code_context.chars().take(16000).collect();
        let ai_input = json!({
            "prompt_name": PROMPT,
            "tier": tier,
            "model_id": model_id,
            "provider_fn": provider_fn,
            "input_kv": [
                { "key": "scope_title", "value": nb_title },
                { "key": "code_context", "value": code_context_capped },
            ],
        });
        let res = crate::ai_ai_call::aiCall(&ai_input, db)?;
        let raw = res
            .get("output_kv")
            .and_then(Value::as_array)
            .and_then(|arr| {
                arr.iter().find(|o| {
                    o.get("field_name").and_then(Value::as_str) == Some("mindmap_json")
                        && o.get("parent_output_id").map_or(true, |v| v.is_null())
                })
            })
            .and_then(|o| o.get("value_text"))
            .filter(|v| truthy(Some(v)))
            .cloned()
            .unwrap_or(json!(""));
        let ai_m = crate::notebooks_notebook_store::aiMeta(db, &res);
        let tree = parse_tree(&js_string(&raw));
        Ok((tree.unwrap_or(Value::Null), ai_m))
    })();

    let (mut tree, ai_m) = match wave {
        Ok(v) => v,
        Err(e) => {
            return Ok(json!({ "ok": false, "error": format!("notebook-mindmap AI failed: {e} — check the OpenRouter provider/model.") }));
        }
    };
    if tree.is_null() {
        return Ok(json!({ "ok": false, "error": "the model did not return a valid mind-map JSON — try again." }));
    }
    if !truthy(tree.get("label")) {
        tree.as_object_mut().unwrap().insert("label".into(), nb.get("title").cloned().unwrap_or(Value::Null));
    }

    let nb_title_s = js_string(nb.get("title").unwrap_or(&Value::Null));
    let body_md = format!("Mind map of **{nb_title_s}** — {} nodes.", count_nodes(&tree));
    // JS: `meta: { mindmap: tree, ...aiM }` — mindmap key first, then aiM's keys spread after.
    let mut meta = serde_json::Map::new();
    meta.insert("mindmap".into(), tree);
    if let Some(am) = ai_m.as_object() {
        for (k, v) in am {
            meta.insert(k.clone(), v.clone());
        }
    }
    let item = crate::notebooks_notebook_store::insertItem(
        db,
        &id_val,
        &json!({ "kind": "mindmap", "title": "Mind map", "body_md": body_md, "meta": Value::Object(meta), "pinned": 0 }),
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
            "CREATE TABLE nb.notebooks (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, title TEXT NOT NULL, summary TEXT, scope_json TEXT NOT NULL DEFAULT '{}', created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), updated_at TEXT);
             CREATE TABLE nb.notebook_items (id INTEGER PRIMARY KEY AUTOINCREMENT, notebook_id INTEGER NOT NULL, kind TEXT NOT NULL DEFAULT 'note', title TEXT, body_md TEXT NOT NULL DEFAULT '', meta_json TEXT, pinned INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
             CREATE TABLE nb.notebook_sources (id INTEGER PRIMARY KEY AUTOINCREMENT, notebook_id INTEGER NOT NULL, kind TEXT NOT NULL, ref TEXT, label TEXT, body_md TEXT, enabled INTEGER NOT NULL DEFAULT 1, meta_json TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn a_notebook(c: &Connection, title: &str) -> i64 {
        c.execute("INSERT INTO nb.notebooks (title) VALUES (?)", [title]).unwrap();
        c.last_insert_rowid()
    }

    #[test]
    fn id_is_required() {
        let c = setup();
        let e = notebookMindmap(&json!({}), &c).unwrap_err();
        assert_eq!(e, "notebook-mindmap: id is required");
        let e2 = notebookMindmap(&json!({ "id": 0 }), &c).unwrap_err();
        assert_eq!(e2, "notebook-mindmap: id is required");
    }

    #[test]
    fn unknown_notebook_returns_ok_false() {
        let c = setup();
        let r = notebookMindmap(&json!({ "id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "notebook 999 not found" }));
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_and_degrades_gracefully() {
        // proves every deterministic gate (id guard, notebook lookup, corpus resolve, prompt
        // ensure) ran cleanly before the deferred aiCall boundary; the deferred stub's Err is
        // caught (never propagated), matching the JS try/catch degrade-gracefully contract.
        let c = setup();
        let nid = a_notebook(&c, "My Notebook");
        let out = notebookMindmap(&json!({ "id": nid }), &c).unwrap();
        assert_eq!(out["ok"], json!(false));
        assert!(out["error"].as_str().unwrap().starts_with("notebook-mindmap AI failed:"));
        // the prompt WAS ensured (ensure_prompt ran) before the deferred call.
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'notebook.mindmap.v2'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // no item was inserted — the wave never reached insertItem.
        let items: i64 = c.query_row("SELECT COUNT(*) FROM nb.notebook_items", [], |r| r.get(0)).unwrap();
        assert_eq!(items, 0);
    }

    // ── sanitize (pure, unreachable past the deferred boundary — unit-tested directly) ───────
    #[test]
    fn sanitize_uses_label_name_or_title_fallback_and_trims_capped() {
        assert_eq!(sanitize(&json!({ "label": "  Root  " }), 0), Some(json!({ "label": "Root" })));
        assert_eq!(sanitize(&json!({ "name": "N" }), 0), Some(json!({ "label": "N" })));
        assert_eq!(sanitize(&json!({ "title": "T" }), 0), Some(json!({ "label": "T" })));
        assert_eq!(sanitize(&json!({ "label": "" }), 0), None);
        assert_eq!(sanitize(&json!("not an object"), 0), None);
    }

    #[test]
    fn sanitize_caps_children_to_12_and_depth_to_4() {
        let kids: Vec<Value> = (0..20).map(|i| json!({ "label": format!("k{i}") })).collect();
        let node = json!({ "label": "root", "children": kids });
        let out = sanitize(&node, 0).unwrap();
        assert_eq!(out["children"].as_array().unwrap().len(), 12);

        // depth 4: children still processed one more level (depth<4 check happens at the PARENT)
        let mut deep = json!({ "label": "d0" });
        for i in 1..=6 {
            deep = json!({ "label": format!("d{i}"), "children": [deep] });
        }
        let out2 = sanitize(&deep, 0).unwrap();
        // walk down: depth 0 has children (d1<4), ... depth 4 node still emits itself but no children (4<4 false)
        let mut cur = &out2;
        let mut levels = 0;
        while let Some(c) = cur.get("children").and_then(|v| v.as_array()).and_then(|a| a.first()) {
            cur = c;
            levels += 1;
        }
        assert_eq!(levels, 4);
    }

    #[test]
    fn sanitize_drops_empty_children_arrays() {
        let out = sanitize(&json!({ "label": "leaf", "children": [] }), 0).unwrap();
        assert_eq!(out, json!({ "label": "leaf" }));
        let out2 = sanitize(&json!({ "label": "leaf", "children": [{ "label": "" }] }), 0).unwrap();
        assert_eq!(out2, json!({ "label": "leaf" }));
    }

    // ── parse_tree (pure fence-strip + brace-slice + JSON.parse + sanitize) ──────────────────
    #[test]
    fn parse_tree_strips_fences_and_parses_object() {
        let raw = "```json\n{\"label\":\"Root\",\"children\":[{\"label\":\"A\"}]}\n```";
        let out = parse_tree(raw).unwrap();
        assert_eq!(out, json!({ "label": "Root", "children": [{ "label": "A" }] }));
    }

    #[test]
    fn parse_tree_handles_bare_json_no_fences() {
        let raw = "{\"label\":\"Root\"}";
        assert_eq!(parse_tree(raw), Some(json!({ "label": "Root" })));
    }

    #[test]
    fn parse_tree_returns_none_on_invalid_json_or_missing_braces() {
        assert_eq!(parse_tree("no braces here"), None);
        assert_eq!(parse_tree("{not valid json}"), None);
    }

    #[test]
    fn parse_tree_returns_none_when_sanitize_rejects_root() {
        assert_eq!(parse_tree("{\"nope\":true}"), None);
    }

    // ── count_nodes ────────────────────────────────────────────────────────────────────────
    #[test]
    fn count_nodes_counts_self_plus_all_descendants() {
        let tree = json!({ "label": "r", "children": [{ "label": "a" }, { "label": "b", "children": [{ "label": "c" }] }] });
        assert_eq!(count_nodes(&tree), 4);
        assert_eq!(count_nodes(&json!({ "label": "leaf" })), 1);
    }
}
