//! widget-save-to-notebook.rs — twin of skills/agents/in/widget-save-to-notebook.mjs (db WRITE/READ
//! lane). NAMES ARE CONTRACT: `widgetSaveToNotebook` (default). The artifact⇄widget BRIDGE: persists
//! a chat-generated widget as a notebook ITEM (a readable body_md + the structured payload kept in
//! meta for re-render). Target = a given notebook (by id, else by title find-or-create), else the
//! default "Chat captures" notebook is found/created.
// QUALITY: 78/100 — 4-way body_of/defaultTitle dispatch by widget kind (code/diagram/improvements/
// memory-search/json-fallback); reuses the already-converted notebook-schema/notebook-store twins.
use rusqlite::Connection;
use serde_json::{json, Value};
use regex::Regex;
use std::sync::OnceLock;

// Standard helper functions (playbook-verbatim) — JS coercion semantics.
fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)
// node:sqlite binds an integral JS number as INTEGER, a fractional one as REAL.
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }
// `x || fallback` then `String(...)` — the JS pattern behind p.language / p.code defaults.
fn or_default_str(v: Option<&Value>, fallback: &str) -> String { if truthy(v) { js_string(v.unwrap()) } else { fallback.to_string() } }
// `String(x).split('/').pop()`.
fn basename(s: &str) -> String { match s.rfind('/') { Some(i) => s[i + 1..].to_string(), None => s.to_string() } }

const CAPTURE_SLUG: &str = "chat-captures";

// `const slugify = (s) => String(s || 'notebook').toLowerCase().replace(/[^a-z0-9]+/g, '-')
//   .replace(/^-|-$/g, '').slice(0, 48) || 'notebook'`. Called directly on the raw `input.notebook_title`
// value (any JS type) — so the `s || 'notebook'` fallback is evaluated on the untouched value.
fn slugify(v: &Value) -> String {
    let base = if truthy(Some(v)) { js_string(v) } else { "notebook".to_string() };
    let lower = base.to_lowercase();
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"[^a-z0-9]+").unwrap());
    let dashed = re.replace_all(&lower, "-");
    let trimmed = dashed.trim_matches('-');
    let sliced: String = trimmed.chars().take(48).collect();
    if sliced.is_empty() { "notebook".into() } else { sliced }
}

// A readable markdown view of the widget (the structured payload also lives in meta for re-render).
fn body_of(kind: &Value, payload: Option<&Value>) -> String {
    let p = nz(payload).cloned().unwrap_or_else(|| json!({}));
    let kind_s = kind.as_str();
    if kind_s == Some("code") {
        return if truthy(p.get("file")) {
            let language = or_default_str(p.get("language"), "");
            let code = or_default_str(p.get("code"), "");
            let code_sliced: String = code.chars().take(8000).collect();
            format!("**{}**\n\n```{}\n{}\n```", tmpl(p.get("file")), language, code_sliced)
        } else {
            "_(no code)_".to_string()
        };
    }
    if kind_s == Some("diagram") {
        let d = nz(p.get("diagram")).cloned().unwrap_or_else(|| p.clone());
        let cap = if truthy(d.get("caption")) { format!("{}\n\n", tmpl(d.get("caption"))) } else { String::new() };
        let nodes_arr = d.get("nodes").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let nodes_str = nodes_arr.iter().map(|n| {
            let file_part = if truthy(n.get("file")) { format!(" \u{2014} `{}`", tmpl(n.get("file"))) } else { String::new() };
            format!("- **{}** ({}){}", tmpl(n.get("label")), tmpl(n.get("role")), file_part)
        }).collect::<Vec<_>>().join("\n");
        let body = if nodes_str.is_empty() { "_(no diagram)_".to_string() } else { nodes_str };
        return format!("{}{}", cap, body);
    }
    if kind_s == Some("improvements") {
        let arr = p.get("improvements").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let parts: Vec<String> = arr.iter().enumerate().map(|(i, it)| {
            let target = if truthy(it.get("target")) { tmpl(it.get("target")) } else { "improvement".to_string() };
            let file_part = if truthy(it.get("file")) { format!(" \u{2014} `{}`", tmpl(it.get("file"))) } else { String::new() };
            let rationale = if truthy(it.get("rationale")) { tmpl(it.get("rationale")) } else { String::new() };
            format!("{}. **{}**{}\n\n{}", i + 1, target, file_part, rationale)
        }).collect();
        let joined = parts.join("\n\n");
        return if joined.is_empty() { "_(no improvements)_".to_string() } else { joined };
    }
    if kind_s == Some("memory-search") {
        let arr = p.get("hits").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let parts: Vec<String> = arr.iter().map(|h| format!("- **{}** \u{2014} {}", tmpl(h.get("title")), tmpl(h.get("snippet")))).collect();
        let joined = parts.join("\n");
        return if joined.is_empty() { "_(no memories)_".to_string() } else { joined };
    }
    let pretty = serde_json::to_string_pretty(&p).unwrap_or_default();
    let sliced: String = pretty.chars().take(4000).collect();
    format!("```json\n{}\n```", sliced)
}

fn default_title(kind: &Value, input: &Value) -> Value {
    let p = nz(input.get("payload")).cloned().unwrap_or_else(|| json!({}));
    let kind_s = kind.as_str();
    if kind_s == Some("code") && truthy(p.get("file")) {
        let file_s = js_string(p.get("file").unwrap());
        return json!(format!("Code: {}", basename(&file_s)));
    }
    if kind_s == Some("diagram") {
        let dt = p.get("diagram").and_then(|d| d.get("title"));
        let pt = p.get("title");
        let title_val = if truthy(dt) { dt } else { pt };
        return if truthy(title_val) { json!(format!("Diagram: {}", tmpl(title_val))) } else { json!("Diagram") };
    }
    if kind_s == Some("improvements") {
        let len = p.get("improvements").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
        return json!(format!("Improvements ({})", len));
    }
    if truthy(input.get("request")) {
        let s = js_string(input.get("request").unwrap());
        let sliced: String = s.chars().take(60).collect();
        return json!(sliced);
    }
    kind.clone()
}

// `SELECT id, slug FROM nb.notebooks WHERE slug = ?` find, else INSERT-then-reselect (the two-column
// projection is the JS shape — `existing`/the re-select only ever carry id + slug).
fn find_or_create(db: &Connection, slug: &str, title: &str, scope: Option<&Value>) -> Result<Value, String> {
    let existing = crate::db_json::query_rows(db, "SELECT id, slug FROM nb.notebooks WHERE slug = ?", &[json!(slug)])?;
    if let Some(r) = existing.as_array().and_then(|a| a.first()) {
        return Ok(r.clone());
    }
    let scope_json = if truthy(scope) { json!(serde_json::to_string(scope.unwrap()).map_err(|e| e.to_string())?) } else { json!("{}") };
    crate::db_json::run_write(
        db,
        "INSERT INTO nb.notebooks(slug, title, summary, scope_json) VALUES (?, ?, ?, ?)",
        &[json!(slug), json!(title), Value::Null, scope_json],
    )?;
    let after = crate::db_json::query_rows(db, "SELECT id, slug FROM nb.notebooks WHERE slug = ?", &[json!(slug)])?;
    Ok(after.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

pub fn widgetSaveToNotebook(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: const { db } = context ?? {}; if (!db) throw err.contextMissing(...) — the harness always
    // supplies db (db: &Connection is non-optional here), so that guard cannot fire in this lane.
    let kind_opt = input.get("kind");
    if !truthy(kind_opt) {
        return Err("widget-save-to-notebook: kind is required".into());
    }
    let kind = kind_opt.unwrap();

    crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));

    let mut nb = Value::Null;
    if truthy(input.get("notebook_id")) {
        let id_n = js_number(input.get("notebook_id"));
        let rows = crate::db_json::query_rows(db, "SELECT id, slug FROM nb.notebooks WHERE id = ?", &[num_param(id_n)])?;
        nb = rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
    }
    if nb.is_null() && truthy(input.get("notebook_title")) {
        let title_raw = input.get("notebook_title").unwrap();
        let base = slugify(title_raw);
        let mut slug = base.clone();
        let mut n: i64 = 1;
        loop {
            let exists = crate::db_json::query_rows(db, "SELECT 1 FROM nb.notebooks WHERE slug = ?", &[json!(slug.clone())])?;
            if exists.as_array().map_or(false, |a| !a.is_empty()) {
                n += 1;
                slug = format!("{}-{}", base, n);
            } else {
                break;
            }
        }
        let title_str = js_string(title_raw);
        nb = find_or_create(db, &slug, &title_str, input.get("scope"))?;
    }
    if nb.is_null() {
        nb = find_or_create(db, CAPTURE_SLUG, "Chat captures", None)?;
    }

    let meta = json!({
        "widget_kind": kind.clone(),
        "payload": nz(input.get("payload")).cloned().unwrap_or(Value::Null),
        "scope": nz(input.get("scope")).cloned().unwrap_or(Value::Null),
        "request": nz(input.get("request")).cloned().unwrap_or(Value::Null),
    });
    let item_kind = format!("widget:{}", tmpl(Some(kind)));
    let title_v = if truthy(input.get("title")) { input.get("title").cloned().unwrap() } else { default_title(kind, input) };
    let body_md = body_of(kind, input.get("payload"));

    let opts = json!({ "kind": item_kind, "title": title_v, "body_md": body_md, "meta": meta });
    let nb_id = nb.get("id").cloned().unwrap_or(Value::Null);
    let item = crate::notebooks_notebook_store::insertItem(db, &nb_id, &opts);

    Ok(json!({
        "ok": true,
        "notebook_id": nb_id,
        "notebook_slug": nb.get("slug").cloned().unwrap_or(Value::Null),
        "item_id": item.get("id").cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS nb;").unwrap();
        c
    }

    #[test]
    fn code_widget_creates_default_capture_notebook() {
        let c = fresh();
        let r = widgetSaveToNotebook(&json!({
            "kind": "code",
            "payload": { "file": "src/auth.ts", "code": "export const x=1;", "language": "typescript" },
            "scope": { "kind": "file", "path": "src/auth.ts" }
        }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["notebook_slug"], json!("chat-captures"));
        let items = crate::db_json::query_rows(&c, "SELECT kind, body_md, meta_json FROM nb.notebook_items WHERE id = ?", &[r["item_id"].clone()]).unwrap();
        let item = &items[0];
        assert_eq!(item["kind"], json!("widget:code"));
        assert!(item["body_md"].as_str().unwrap().contains("src/auth.ts"));
        assert!(item["body_md"].as_str().unwrap().contains("```typescript"));
        let meta: Value = serde_json::from_str(item["meta_json"].as_str().unwrap()).unwrap();
        assert_eq!(meta["widget_kind"], json!("code"));
        assert_eq!(meta["payload"]["file"], json!("src/auth.ts"));
        assert_eq!(meta["scope"], json!({ "kind": "file", "path": "src/auth.ts" }));
    }

    #[test]
    fn second_save_reuses_default_notebook() {
        let c = fresh();
        let a = widgetSaveToNotebook(&json!({ "kind": "diagram", "payload": { "diagram": { "title": "Auth", "nodes": [{ "label": "x", "role": "logic" }] } } }), &c).unwrap();
        let b = widgetSaveToNotebook(&json!({ "kind": "improvements", "payload": { "improvements": [] } }), &c).unwrap();
        assert_eq!(a["notebook_id"], b["notebook_id"]);
        let count = crate::db_json::query_rows(&c, "SELECT COUNT(*) c FROM nb.notebooks", &[]).unwrap();
        assert_eq!(count[0]["c"], json!(1));
    }

    #[test]
    fn notebook_title_creates_own_notebook_with_slug_collision() {
        let c = fresh();
        let a = widgetSaveToNotebook(&json!({ "kind": "code", "payload": { "file": "a.ts", "code": "x" }, "notebook_title": "Auth deep dive" }), &c).unwrap();
        assert_eq!(a["notebook_slug"], json!("auth-deep-dive"));
        let b = widgetSaveToNotebook(&json!({ "kind": "code", "payload": { "file": "b.ts", "code": "y" }, "notebook_title": "Auth deep dive" }), &c).unwrap();
        assert_eq!(b["notebook_slug"], json!("auth-deep-dive-2"));
        assert_ne!(a["notebook_id"], b["notebook_id"]);
    }

    #[test]
    fn notebook_id_targets_existing_notebook() {
        let c = fresh();
        let created = widgetSaveToNotebook(&json!({ "kind": "code", "payload": { "file": "a.ts", "code": "1" }, "notebook_title": "Targeted NB" }), &c).unwrap();
        let id = created["notebook_id"].clone();
        let r = widgetSaveToNotebook(&json!({ "kind": "note", "payload": { "a": 1 }, "notebook_id": id.as_i64().unwrap() }), &c).unwrap();
        assert_eq!(r["notebook_id"], id);
        assert_eq!(r["notebook_slug"], json!("targeted-nb"));
    }

    #[test]
    fn code_without_file_falls_back_and_title_defaults_to_kind() {
        let c = fresh();
        let r = widgetSaveToNotebook(&json!({ "kind": "code", "payload": {} }), &c).unwrap();
        let items = crate::db_json::query_rows(&c, "SELECT title, body_md FROM nb.notebook_items WHERE id = ?", &[r["item_id"].clone()]).unwrap();
        assert_eq!(items[0]["body_md"], json!("_(no code)_"));
        assert_eq!(items[0]["title"], json!("code"));
    }

    #[test]
    fn diagram_title_falls_back_to_payload_title_then_diagram_default() {
        let c = fresh();
        let r1 = widgetSaveToNotebook(&json!({ "kind": "diagram", "payload": { "title": "Fallback Title", "diagram": { "nodes": [] } } }), &c).unwrap();
        let i1 = crate::db_json::query_rows(&c, "SELECT title, body_md FROM nb.notebook_items WHERE id = ?", &[r1["item_id"].clone()]).unwrap();
        assert_eq!(i1[0]["title"], json!("Diagram: Fallback Title"));
        assert_eq!(i1[0]["body_md"], json!("_(no diagram)_"));

        let r2 = widgetSaveToNotebook(&json!({ "kind": "diagram", "payload": {} }), &c).unwrap();
        let i2 = crate::db_json::query_rows(&c, "SELECT title FROM nb.notebook_items WHERE id = ?", &[r2["item_id"].clone()]).unwrap();
        assert_eq!(i2[0]["title"], json!("Diagram"));
    }

    #[test]
    fn improvements_and_memory_search_bodies_and_titles() {
        let c = fresh();
        let r1 = widgetSaveToNotebook(&json!({ "kind": "improvements", "payload": { "improvements": [{ "target": "auth.ts", "file": "src/auth.ts", "rationale": "tighten types" }] } }), &c).unwrap();
        let i1 = crate::db_json::query_rows(&c, "SELECT title, body_md FROM nb.notebook_items WHERE id = ?", &[r1["item_id"].clone()]).unwrap();
        assert_eq!(i1[0]["title"], json!("Improvements (1)"));
        assert!(i1[0]["body_md"].as_str().unwrap().contains("1. **auth.ts** \u{2014} `src/auth.ts`"));

        let r2 = widgetSaveToNotebook(&json!({ "kind": "memory-search", "payload": { "hits": [{ "title": "Note A", "snippet": "snip" }] } }), &c).unwrap();
        let i2 = crate::db_json::query_rows(&c, "SELECT body_md FROM nb.notebook_items WHERE id = ?", &[r2["item_id"].clone()]).unwrap();
        assert_eq!(i2[0]["body_md"], json!("- **Note A** \u{2014} snip"));
    }

    #[test]
    fn unmatched_kind_falls_back_to_json_body_and_request_title() {
        let c = fresh();
        let r = widgetSaveToNotebook(&json!({ "kind": "note", "payload": { "a": 1 }, "request": "what changed in auth?" }), &c).unwrap();
        let items = crate::db_json::query_rows(&c, "SELECT title, body_md FROM nb.notebook_items WHERE id = ?", &[r["item_id"].clone()]).unwrap();
        assert_eq!(items[0]["title"], json!("what changed in auth?"));
        assert_eq!(items[0]["body_md"], json!("```json\n{\n  \"a\": 1\n}\n```"));
    }

    #[test]
    fn explicit_title_overrides_default() {
        let c = fresh();
        let r = widgetSaveToNotebook(&json!({ "kind": "code", "payload": { "file": "x.ts", "code": "1" }, "title": "My Title" }), &c).unwrap();
        let items = crate::db_json::query_rows(&c, "SELECT title FROM nb.notebook_items WHERE id = ?", &[r["item_id"].clone()]).unwrap();
        assert_eq!(items[0]["title"], json!("My Title"));
    }

    #[test]
    fn missing_kind_errors() {
        let c = fresh();
        assert!(widgetSaveToNotebook(&json!({}), &c).is_err());
    }
}
