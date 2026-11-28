//! notebook-promote.rs — twin of skills/notebooks/notebook-promote.mjs (db WRITE lane). NAMES ARE
//! CONTRACT: `notebookPromote` (default). The research → action bridge: turns a notebook (or one of
//! its items) into a backlog card, with the notebook's scope baked in as a `## Scope (files)` block.
//! `draft` returns `{ title, body }` for the FE to preview/edit; without it, title/body → card-add.
// QUALITY: 82/100 — title/body fallback chain (own → item → notebook), scope-block templating,
// slug dedupe loop shared shape with notebook-store's `create`, delegates the INSERT to card-add.
use rusqlite::Connection;
use serde_json::{json, Value};
use regex::Regex;
use std::sync::OnceLock;

// JS truthiness.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `String(v)` (no `??`).
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
// JS `${v}` template interpolation.
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS `Number(v)`.
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) }
        }
        Some(_) => f64::NAN,
    }
}
// JS `!n` on a Number result: falsy at 0 / -0 / NaN. True means the guard throws.
fn falsy_num(n: f64) -> bool { n == 0.0 || n.is_nan() }
// node:sqlite binds an integral JS number as INTEGER, a fractional one as REAL; mirror that so the
// bound value (and any plain output field built from the same coerced number) render identically.
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }

// `const slugify = (s) => String(s || 'card').toUpperCase().replace(/[^A-Z0-9]+/g, '-')
//   .replace(/^-|-$/g, '').slice(0, 40) || 'CARD'`.
fn slugify(s: &Value) -> String {
    let base_v: Value = if truthy(Some(s)) { s.clone() } else { json!("card") };
    let upper = js_string(&base_v).to_uppercase();
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"[^A-Z0-9]+").unwrap());
    let dashed = re.replace_all(&upper, "-");
    let trimmed = dashed.trim_matches('-');
    let sliced: String = trimmed.chars().take(40).collect();
    if sliced.is_empty() { "CARD".into() } else { sliced }
}

pub fn notebookPromote(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS destructures { db } from context and throws err.contextMissing without it — the harness
    // context always carries db, so that guard cannot fire here.
    crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));

    let id_n = js_number(input.get("id"));
    if falsy_num(id_n) { return Err("notebook-promote: id is required".into()); }
    let id_v = num_param(id_n);

    let nb = crate::notebooks_notebook_store::getNotebook(db, &id_v);
    if nb.is_null() {
        return Ok(json!({ "ok": false, "error": format!("notebook {} not found", tmpl(Some(&id_v))) }));
    }

    let mut title_v: Value = input.get("title").cloned().unwrap_or(Value::Null);
    let mut body_v: Value = input.get("body").cloned().unwrap_or(Value::Null);

    if !truthy(Some(&title_v)) || !truthy(Some(&body_v)) {
        let mut item_title = String::new();
        let mut item_body = String::new();
        if truthy(input.get("item_id")) {
            let item_id_n = js_number(input.get("item_id"));
            let rows = crate::db_json::query_rows(
                db,
                "SELECT * FROM nb.notebook_items WHERE id = ? AND notebook_id = ?",
                &[num_param(item_id_n), id_v.clone()],
            )?;
            if let Some(it) = rows.as_array().and_then(|a| a.first()) {
                item_title = if truthy(it.get("title")) { js_string(it.get("title").unwrap()) } else { String::new() };
                item_body = if truthy(it.get("body_md")) { js_string(it.get("body_md").unwrap()) } else { String::new() };
            }
        }
        title_v = if truthy(Some(&title_v)) {
            title_v.clone()
        } else if !item_title.is_empty() {
            json!(item_title)
        } else {
            nb.get("title").cloned().unwrap_or(Value::Null)
        };

        let scope_paths: Vec<Value> = nb.get("scope").and_then(|s| s.get("paths")).and_then(|p| p.as_array()).cloned().unwrap_or_default();
        let scope_block = if !scope_paths.is_empty() {
            let lines: Vec<String> = scope_paths.iter().map(|p| format!("- {}", tmpl(Some(p)))).collect();
            format!("\n\n## Scope (files)\n{}", lines.join("\n"))
        } else {
            String::new()
        };
        let item_body_or_default = if !item_body.is_empty() {
            item_body.clone()
        } else {
            format!("Research from notebook \"{}\".", tmpl(nb.get("title")))
        };
        let nb_slug = nb.get("slug");
        let slug_or_title = if truthy(nb_slug) { tmpl(nb_slug) } else { tmpl(nb.get("title")) };
        let default_body = format!(
            "{}{}\n\n_Promoted from notebook #{} ({})._",
            item_body_or_default, scope_block, tmpl(Some(&id_v)), slug_or_title
        );
        body_v = if truthy(Some(&body_v)) { body_v.clone() } else { json!(default_body) };
    }

    if truthy(input.get("draft")) {
        return Ok(json!({ "ok": true, "draft": true, "title": title_v, "body": body_v }));
    }

    let base = slugify(&title_v);
    let mut slug = base.clone();
    let mut n: i64 = 1;
    loop {
        let exists = crate::db_json::query_rows(db, "SELECT 1 FROM cards WHERE slug = ?", &[json!(slug.clone())])?;
        if exists.as_array().map_or(false, |a| !a.is_empty()) {
            n += 1;
            slug = format!("{}-{}", base, n);
        } else {
            break;
        }
    }

    let card_input = json!({ "slug": slug, "title": title_v, "status": "backlog", "body": body_v });
    let card = crate::entities_card_add::cardAdd(&card_input, db)?;

    let insert_title = format!("\u{2192} card {}", slug);
    let insert_body = format!("Promoted to backlog card **{}** \u{2014} {}.", slug, tmpl(Some(&title_v)));
    crate::notebooks_notebook_store::insertItem(
        db,
        &id_v,
        &json!({ "kind": "note", "title": insert_title, "body_md": insert_body, "pinned": 1 }),
    );

    Ok(json!({ "ok": true, "slug": slug, "card": card }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS nb; \
             CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT UNIQUE, title TEXT, status TEXT, body_json TEXT, parent_slug TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn draft_auto_title_body_with_scope_block_no_card_created() {
        let c = fresh();
        let created = crate::notebooks_notebook_store::notebookStore(
            &json!({ "op": "create", "title": "Auth flow", "scope": { "paths": ["src/auth.js", "src/session.js"] } }),
            &c,
        )
        .unwrap();
        let id = created["notebook"]["id"].clone();

        let r = notebookPromote(&json!({ "id": id, "draft": true }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["draft"], json!(true));
        assert_eq!(r["title"], json!("Auth flow"));
        let body = r["body"].as_str().unwrap();
        assert!(body.contains("## Scope (files)"));
        assert!(body.contains("- src/auth.js"));
        assert!(body.contains("- src/session.js"));
        assert!(body.contains(&format!("_Promoted from notebook #{} (auth-flow)._", id)));

        let cards = crate::db_json::query_rows(&c, "SELECT COUNT(*) c FROM cards", &[]).unwrap();
        assert_eq!(cards[0]["c"], json!(0));
    }

    #[test]
    fn title_and_body_provided_creates_backlog_card_and_dedupes_slug() {
        let c = fresh();
        let created = crate::notebooks_notebook_store::notebookStore(&json!({ "op": "create", "title": "Perf work" }), &c).unwrap();
        let id = created["notebook"]["id"].clone();

        let r1 = notebookPromote(&json!({ "id": id, "title": "Fix slow query", "body": "details" }), &c).unwrap();
        assert_eq!(r1["ok"], json!(true));
        assert_eq!(r1["slug"], json!("FIX-SLOW-QUERY"));
        assert_eq!(r1["card"]["status"], json!("backlog"));
        assert_eq!(r1["card"]["title"], json!("Fix slow query"));

        let r2 = notebookPromote(&json!({ "id": id, "title": "Fix slow query", "body": "details2" }), &c).unwrap();
        assert_eq!(r2["slug"], json!("FIX-SLOW-QUERY-2"));

        let items = crate::db_json::query_rows(&c, "SELECT title FROM nb.notebook_items WHERE notebook_id = ? ORDER BY id", &[id]).unwrap();
        let items = items.as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items[0]["title"].as_str().unwrap().contains("FIX-SLOW-QUERY"));
        assert!(items[1]["title"].as_str().unwrap().contains("FIX-SLOW-QUERY-2"));
    }

    #[test]
    fn item_id_fallback_unknown_notebook_and_missing_id() {
        let c = fresh();
        let created = crate::notebooks_notebook_store::notebookStore(&json!({ "op": "create", "title": "Research" }), &c).unwrap();
        let id = created["notebook"]["id"].clone();
        let added = crate::notebooks_notebook_store::notebookStore(
            &json!({ "op": "add_item", "id": id, "title": "Item title", "body_md": "Item body" }),
            &c,
        )
        .unwrap();
        let item_id = added["item"]["id"].clone();

        let r = notebookPromote(&json!({ "id": id, "item_id": item_id }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["card"]["title"], json!("Item title"));
        let stored: Value = serde_json::from_str(r["card"]["body_json"].as_str().unwrap()).unwrap();
        assert!(stored.as_str().unwrap().starts_with("Item body"));

        let nf = notebookPromote(&json!({ "id": 999999 }), &c).unwrap();
        assert_eq!(nf, json!({ "ok": false, "error": "notebook 999999 not found" }));

        assert!(notebookPromote(&json!({}), &c).unwrap_err().contains("id is required"));
    }
}
