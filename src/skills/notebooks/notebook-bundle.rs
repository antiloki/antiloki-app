//! notebook-bundle.rs — twin of skills/notebooks/notebook-bundle.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `notebookBundle` (default). The `.notebook` portable file: export a WHOLE notebook as
//! one JSON bundle (body-bearing sources port verbatim; workspace-gathered sources are resolved
//! NOW into inert text snapshots via `gather`) and import it back (pure row-insert, no re-linking).
// QUALITY: 80/100 — op-routed export/import; export delegates scope resolution + per-source
// gathering to the already-converted notebook-scope/notebook-corpus twins.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::LazyLock;

const FORMAT: &str = "antiloki-notebook";
const MAX_ROWS: usize = 500;

static BODY_KINDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    ["text", "file", "url", "source", "paper", "conversation", "focus", "doc", "feed"].into_iter().collect()
});

// ── standard JS-coercion helpers (playbook) ──
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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
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
// node:sqlite binds an integral JS number as INTEGER, a fractional one as REAL.
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }
// `const cap = (s, n) => String(s ?? '').slice(0, n)`.
fn cap(v: Option<&Value>, n: usize) -> String { str_qq(v).chars().take(n).collect() }
// `const parse = (t, fb) => { try { return JSON.parse(t ?? '') ?? fb; } catch { return fb; } }`.
fn parse_or(v: Option<&Value>, fb: Value) -> Value {
    let raw = str_qq(v);
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Null) => fb,
        Ok(x) => x,
        Err(_) => fb,
    }
}
// `if (meta && typeof meta.file === 'string') delete meta.file;`
fn strip_meta_file(meta: &mut Value) {
    if let Value::Object(obj) = meta {
        if obj.get("file").map_or(false, |v| v.is_string()) { obj.remove("file"); }
    }
}

pub fn notebookBundle(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS destructures { db } from context and throws err.contextMissing without it — the harness
    // context always carries db, so that guard cannot fire here.
    crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));
    let op = str_qq(input.get("op"));

    if op == "export" {
        let id_n = js_number(input.get("id"));
        if id_n == 0.0 || id_n.is_nan() { return Err("notebook-bundle: id is required".into()); }
        let id_v = num_param(id_n);

        let nb = crate::notebooks_notebook_store::getNotebook(db, &id_v);
        if nb.is_null() {
            return Ok(json!({ "ok": false, "error": format!("notebook {} not found", tmpl(Some(&id_v))) }));
        }

        let scope_input = if truthy(nb.get("scope")) { nb.get("scope").cloned().unwrap() } else { json!({}) };
        let scope = crate::notebooks_notebook_scope::resolveScope(&scope_input, db)?;

        let rows = crate::db_json::query_rows(
            db,
            "SELECT * FROM nb.notebook_sources WHERE notebook_id = ? ORDER BY id ASC",
            &[id_v.clone()],
        )?;
        let rows: Vec<Value> = rows.as_array().cloned().unwrap_or_default();

        let mut sources: Vec<Value> = Vec::new();
        let code_context_md = scope.get("code_context_md").cloned().unwrap_or(Value::Null);
        if truthy(Some(&code_context_md)) && !js_string(&code_context_md).trim().is_empty() {
            sources.push(json!({
                "kind": "text", "ref": Value::Null, "label": "Code in scope (snapshot)",
                "body_md": cap(Some(&code_context_md), 120000),
                "enabled": 1,
                "meta": { "snapshot": true, "gathered_kind": "code_paths" },
            }));
        }

        for s in &rows {
            let mut meta = parse_or(s.get("meta_json"), json!({}));
            let kind = s.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            if BODY_KINDS.contains(kind) {
                let mut body = str_qq(s.get("body_md"));
                if kind == "paper" && body.is_empty() {
                    let gathered = crate::notebooks_notebook_corpus::gather(db, s, &scope, &Value::Null);
                    body = gathered.as_str().unwrap_or("").to_string();
                    if !body.is_empty() {
                        if let Value::Object(obj) = &mut meta { obj.insert("snapshot".into(), json!(true)); }
                    }
                }
                let ref_v = nz(s.get("ref")).cloned().unwrap_or(Value::Null);
                let label_v = nz(s.get("label")).or_else(|| nz(s.get("kind"))).cloned().unwrap_or(Value::Null);
                sources.push(json!({
                    "kind": kind, "ref": ref_v, "label": label_v, "body_md": body,
                    "enabled": if truthy(s.get("enabled")) { 1 } else { 0 },
                    "meta": meta,
                }));
            } else {
                let gathered = crate::notebooks_notebook_corpus::gather(db, s, &scope, &Value::Null);
                let text = gathered.as_str().unwrap_or("").to_string();
                if text.trim().is_empty() { continue; } // an empty snapshot is noise, not provenance
                let label_base = nz(s.get("label")).or_else(|| nz(s.get("kind"))).map(js_string).unwrap_or_default();
                sources.push(json!({
                    "kind": "text", "ref": Value::Null, "label": format!("{label_base} (snapshot)"),
                    "body_md": cap(Some(&json!(text)), 60000),
                    "enabled": if truthy(s.get("enabled")) { 1 } else { 0 },
                    "meta": { "snapshot": true, "gathered_kind": kind },
                }));
            }
        }

        let item_rows = crate::db_json::query_rows(
            db,
            "SELECT * FROM nb.notebook_items WHERE notebook_id = ? ORDER BY id ASC",
            &[id_v.clone()],
        )?;
        let items: Vec<Value> = item_rows.as_array().cloned().unwrap_or_default().iter().map(|it| {
            let mut meta = parse_or(it.get("meta_json"), Value::Null);
            strip_meta_file(&mut meta);
            json!({
                "kind": it.get("kind").cloned().unwrap_or(Value::Null),
                "title": nz(it.get("title")).cloned().unwrap_or(Value::Null),
                "body_md": nz(it.get("body_md")).cloned().unwrap_or(json!("")),
                "pinned": if truthy(it.get("pinned")) { 1 } else { 0 },
                "created_at": nz(it.get("created_at")).cloned().unwrap_or(Value::Null),
                "meta": meta,
            })
        }).collect();

        return Ok(json!({
            "ok": true,
            "bundle": {
                "format": FORMAT, "version": 1,
                // FROZEN CLOCK: new Date().toISOString() → the pinned instant.
                "exported_at": crate::clock::now_iso_nodb(),
                "title": nb.get("title").cloned().unwrap_or(Value::Null),
                "summary": nz(nb.get("summary")).cloned().unwrap_or(Value::Null),
                "sources": sources, "items": items,
            },
        }));
    }

    if op == "import" {
        let b = input.get("bundle");
        let b_is_object = matches!(b, Some(Value::Object(_)));
        if !b_is_object { return Err("notebook-bundle: bundle is required".into()); }
        let b = b.unwrap();

        let format_ok = b.get("format").and_then(|v| v.as_str()) == Some(FORMAT);
        let version_ok = js_number(b.get("version")) == 1.0;
        if !format_ok || !version_ok {
            let format_disp = nz(b.get("format")).map(|v| tmpl(Some(v))).unwrap_or_else(|| "?".to_string());
            let version_disp = nz(b.get("version")).map(|v| tmpl(Some(v))).unwrap_or_else(|| "?".to_string());
            return Ok(json!({
                "ok": false,
                "error": format!("not a v1 {FORMAT} bundle (format={format_disp}, version={version_disp})"),
            }));
        }

        let title_v: Value = if truthy(b.get("title")) { b.get("title").cloned().unwrap() } else { json!("Imported notebook") };
        let title = cap(Some(&title_v), 200);
        let src_in: Vec<Value> = b.get("sources").and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().take(MAX_ROWS).collect();
        let items_in: Vec<Value> = b.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().take(MAX_ROWS).collect();

        let summary_param: Value = if nz(b.get("summary")).is_some() {
            json!(cap(nz(b.get("summary")), 500))
        } else { Value::Null };

        let (_changes, nb_id) = crate::db_json::run_write(
            db,
            "INSERT INTO nb.notebooks (title, summary, scope_json) VALUES (?, ?, ?)",
            &[json!(title), summary_param, json!(r#"{"origin":"import"}"#)],
        )?;
        let nb_id_v = json!(nb_id);

        for s in &src_in {
            let Some(kind_v) = s.get("kind").filter(|v| v.is_string()) else { continue };
            let kind = kind_v.as_str().unwrap();
            let ref_param: Value = if nz(s.get("ref")).is_some() { json!(cap(nz(s.get("ref")), 400)) } else { Value::Null };
            let label_param: Value = if nz(s.get("label")).is_some() { json!(cap(nz(s.get("label")), 200)) } else { json!(cap(Some(kind_v), 40)) };
            let body_md_param: Value = if nz(s.get("body_md")).is_some() { json!(js_string(s.get("body_md").unwrap())) } else { Value::Null };
            let enabled_param: i64 = match s.get("enabled") {
                Some(Value::Number(n)) if n.as_f64() == Some(0.0) => 0,
                _ => 1,
            };
            let meta_param: Value = if truthy(s.get("meta")) { json!(serde_json::to_string(s.get("meta").unwrap()).unwrap()) } else { Value::Null };
            crate::db_json::run_write(
                db,
                "INSERT INTO nb.notebook_sources (notebook_id, kind, ref, label, body_md, enabled, meta_json) VALUES (?, ?, ?, ?, ?, ?, ?)",
                &[nb_id_v.clone(), json!(cap(Some(kind_v), 40)), ref_param, label_param, body_md_param, json!(enabled_param), meta_param],
            )?;
        }

        for it in &items_in {
            let Some(kind_v) = it.get("kind").filter(|v| v.is_string()) else { continue };
            // `it.meta && typeof it.meta === 'object' ? { ...it.meta } : null` — plain-object spread.
            let mut meta: Value = match it.get("meta") {
                Some(Value::Object(m)) if truthy(it.get("meta")) => Value::Object(m.clone()),
                _ => Value::Null,
            };
            strip_meta_file(&mut meta);
            let title_param: Value = if nz(it.get("title")).is_some() { json!(cap(nz(it.get("title")), 300)) } else { Value::Null };
            let body_md_param: Value = if nz(it.get("body_md")).is_some() { json!(js_string(it.get("body_md").unwrap())) } else { json!("") };
            let opts = json!({
                "kind": cap(Some(kind_v), 40),
                "title": title_param,
                "body_md": body_md_param,
                "pinned": if truthy(it.get("pinned")) { 1 } else { 0 },
                "meta": meta,
            });
            crate::notebooks_notebook_store::insertItem(db, &nb_id_v, &opts);
        }

        // count-after-insert (BATCH_OPERATIONS_VERIFY_COUNT): report what actually landed
        let items_count = crate::db_json::query_rows(db, "SELECT COUNT(*) n FROM nb.notebook_items WHERE notebook_id = ?", &[nb_id_v.clone()])?
            .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).cloned().unwrap_or(json!(0));
        let sources_count = crate::db_json::query_rows(db, "SELECT COUNT(*) n FROM nb.notebook_sources WHERE notebook_id = ?", &[nb_id_v.clone()])?
            .as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).cloned().unwrap_or(json!(0));

        return Ok(json!({ "ok": true, "notebook_id": nb_id, "items": items_count, "sources": sources_count }));
    }

    let op_disp = if op.is_empty() { "(none)".to_string() } else { op.clone() };
    Ok(json!({ "ok": false, "error": format!("unknown op: {op_disp} \u{2014} use export|import") }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS nb; \
             CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT UNIQUE, title TEXT, status TEXT, created_at TEXT, updated_at TEXT);",
        ).unwrap();
        c
    }

    fn create_nb(c: &Connection, title: &str, summary: Option<&str>) -> Value {
        crate::notebooks_notebook_store::notebookStore(
            &json!({ "op": "create", "title": title, "summary": summary }),
            c,
        ).unwrap()["notebook"]["id"].clone()
    }

    #[test]
    fn export_body_kind_and_workspace_gathered_sources_with_item_meta_file_stripped() {
        let c = fresh();
        let id = create_nb(&c, "Research NB", Some("sum"));

        crate::db_json::run_write(
            &c,
            "INSERT INTO nb.notebook_sources (notebook_id, kind, ref, label, body_md, enabled) VALUES (?, 'text', NULL, 'Pasted note', 'hello world', 1)",
            &[id.clone()],
        ).unwrap();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('CARD-1', 'A card', 'backlog')", []).unwrap();
        crate::db_json::run_write(
            &c,
            "INSERT INTO nb.notebook_sources (notebook_id, kind, ref, label, body_md, enabled) VALUES (?, 'cards', NULL, NULL, NULL, 1)",
            &[id.clone()],
        ).unwrap();
        // an empty workspace-gathered source (no matching rows) must be dropped as noise
        crate::db_json::run_write(
            &c,
            "INSERT INTO nb.notebook_sources (notebook_id, kind, ref, label, body_md, enabled) VALUES (?, 'entrypoints', NULL, NULL, NULL, 1)",
            &[id.clone()],
        ).unwrap();
        crate::notebooks_notebook_store::insertItem(
            &c, &id,
            &json!({ "kind": "note", "title": "Item 1", "body_md": "body", "meta": { "file": "/tmp/x.mp3", "keep": 1 } }),
        );

        let r = notebookBundle(&json!({ "op": "export", "id": id }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let b = &r["bundle"];
        assert_eq!(b["format"], json!(FORMAT));
        assert_eq!(b["version"], json!(1));
        assert_eq!(b["title"], json!("Research NB"));
        assert_eq!(b["summary"], json!("sum"));
        assert_eq!(b["exported_at"], json!("2026-01-01T00:00:00.000Z"));

        let sources = b["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 2); // entrypoints (empty) dropped
        assert_eq!(sources[0]["kind"], json!("text"));
        assert_eq!(sources[0]["label"], json!("Pasted note"));
        assert_eq!(sources[0]["body_md"], json!("hello world"));
        assert_eq!(sources[0]["meta"], json!({}));
        assert_eq!(sources[1]["kind"], json!("text"));
        assert_eq!(sources[1]["label"], json!("cards (snapshot)"));
        assert!(sources[1]["body_md"].as_str().unwrap().contains("CARD-1"));
        assert_eq!(sources[1]["meta"], json!({ "snapshot": true, "gathered_kind": "cards" }));

        let items = b["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["title"], json!("Item 1"));
        assert_eq!(items[0]["meta"], json!({ "keep": 1 }));
    }

    #[test]
    fn export_notebook_not_found_and_missing_id_throws() {
        let c = fresh();
        let r = notebookBundle(&json!({ "op": "export", "id": 999999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "notebook 999999 not found" }));
        assert!(notebookBundle(&json!({ "op": "export" }), &c).unwrap_err().contains("id is required"));
    }

    #[test]
    fn import_happy_path_inserts_notebook_sources_items_and_strips_meta_file() {
        let c = fresh();
        let bundle = json!({
            "format": FORMAT, "version": 1, "title": "Imported title", "summary": "Imported summary",
            "sources": [
                { "kind": "text", "ref": null, "label": "A source", "body_md": "content", "enabled": 1, "meta": { "snapshot": true } },
                { "kind": 42 }, // dropped: kind not a string
            ],
            "items": [
                { "kind": "note", "title": "T1", "body_md": "B1", "pinned": 1, "meta": { "file": "/x.mp3", "q": true } },
                { "kind": 7 }, // dropped: kind not a string
            ],
        });
        let r = notebookBundle(&json!({ "op": "import", "bundle": bundle }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["items"], json!(1));
        assert_eq!(r["sources"], json!(1));
        let nb_id = r["notebook_id"].clone();

        let nb = crate::notebooks_notebook_store::getNotebook(&c, &nb_id);
        assert_eq!(nb["title"], json!("Imported title"));
        assert_eq!(nb["summary"], json!("Imported summary"));
        assert_eq!(nb["scope"], json!({ "origin": "import" }));

        let items = crate::db_json::query_rows(&c, "SELECT meta_json FROM nb.notebook_items WHERE notebook_id = ?", &[nb_id]).unwrap();
        let meta: Value = serde_json::from_str(items[0]["meta_json"].as_str().unwrap()).unwrap();
        assert_eq!(meta, json!({ "q": true }));
    }

    #[test]
    fn import_rejects_non_object_bundle_and_wrong_format_version() {
        let c = fresh();
        assert!(notebookBundle(&json!({ "op": "import" }), &c).unwrap_err().contains("bundle is required"));

        let r = notebookBundle(&json!({ "op": "import", "bundle": { "format": "wrong", "version": 1 } }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("format=wrong"));

        let r2 = notebookBundle(&json!({ "op": "import", "bundle": { "format": FORMAT, "version": 2 } }), &c).unwrap();
        assert!(r2["error"].as_str().unwrap().contains("version=2"));
    }

    #[test]
    fn unknown_op_returns_error_object() {
        let c = fresh();
        let r = notebookBundle(&json!({ "op": "bogus" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "unknown op: bogus \u{2014} use export|import" }));
        let r2 = notebookBundle(&json!({}), &c).unwrap();
        assert_eq!(r2["error"], json!("unknown op: (none) \u{2014} use export|import"));
    }
}
