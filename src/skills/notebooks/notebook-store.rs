//! notebook-store.rs — twin of skills/notebooks/notebook-store.mjs (db WRITE/READ lane,
//! op-routed). NAMES ARE CONTRACT: `notebookStore` (default) · `getNotebook` / `insertItem` /
//! `aiMeta` (db-first helpers reused by notebook-scope / notebook-ask / notebook-generate /
//! notebook-promote). CRUD for research notebooks + their items: create | scratch | list | get |
//! update | delete | add_item | pin. `scratch` finds-or-creates the hidden chat-Studio notebook
//! (reserved slug 'chat-scratch', excluded from `list`) and re-seeds its focus/conversation
//! sources on every open.
// QUALITY: 81/100 — op-routed CRUD + the hidden-scratch find-or-create/reseed dance; aiMeta reads
// a cross-db (brain.ai_calls) table defensively (a lookup miss/error leaves tokens null, never throws).
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
// JS `??` right operand.
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
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

// `const parse = (s, d) => { try { return JSON.parse(s ?? 'null') ?? d; } catch { return d; } }`
// specialized to `d = {}` (the only call sites here). Absent/NULL → parse 'null' → null → falls to
// {}; invalid JSON → catch → {}; a JSON `null` payload → also {}.
fn parse_meta(meta_json: Option<&str>) -> Value {
    let s = meta_json.unwrap_or("null");
    match serde_json::from_str::<Value>(s) {
        Ok(Value::Null) | Err(_) => json!({}),
        Ok(v) => v,
    }
}

// The in-chat Studio's HIDDEN scratch notebook — reserved slug, excluded from `list`.
const SCRATCH_SLUG: &str = "chat-scratch";

// `const slugify = (s) => String(s || 'notebook').toLowerCase().replace(/[^a-z0-9]+/g, '-')
//   .replace(/^-|-$/g, '').slice(0, 48) || 'notebook'`. The one call site always passes a
// non-empty trimmed title string, so the `s || 'notebook'` fallback never fires in practice —
// kept for fidelity with the JS source.
fn slugify(s: &str) -> String {
    let base = if !s.is_empty() { s.to_string() } else { "notebook".to_string() };
    let lower = base.to_lowercase();
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"[^a-z0-9]+").unwrap());
    let dashed = re.replace_all(&lower, "-");
    let trimmed = dashed.trim_matches('-');
    let sliced: String = trimmed.chars().take(48).collect();
    if sliced.is_empty() { "notebook".into() } else { sliced }
}

// `{ ...row, X: derived }` where X is a NEW key — appended after the existing (db-column-ordered)
// keys, matching JS spread-then-insert order.
fn with_extra(row: &Value, key: &str, val: Value) -> Value {
    let mut m = row.as_object().cloned().unwrap_or_default();
    m.insert(key.into(), val);
    Value::Object(m)
}

/// `export function getNotebook(db, id)` — reused by notebook-scope / notebook-ask /
/// notebook-generate / notebook-promote. `db.prepare(...).get(Number(id))`: a missing row is
/// `undefined` in JS → Rust `Value::Null`.
pub fn getNotebook(db: &Connection, id: &Value) -> Value {
    let rows = crate::db_json::query_rows(db, "SELECT * FROM nb.notebooks WHERE id = ?", &[num_param(js_number(Some(id)))])
        .unwrap_or_else(|_| json!([]));
    match rows.as_array().and_then(|a| a.first()) {
        Some(r) => with_extra(r, "scope", parse_meta(r.get("scope_json").and_then(|v| v.as_str()))),
        None => Value::Null,
    }
}

/// `export function insertItem(db, notebookId, { kind, title, body_md, meta, pinned } = {})`.
/// `opts` absent (JS default `{}`) maps to `Value::Null` here — `Value::get` on a non-object
/// returns `None` exactly like destructuring `undefined` properties off `{}`.
pub fn insertItem(db: &Connection, notebook_id: &Value, opts: &Value) -> Value {
    let nb_id = js_number(Some(notebook_id));
    let kind_v = if truthy(opts.get("kind")) { opts.get("kind").cloned().unwrap() } else { json!("note") };
    let title_v = if truthy(opts.get("title")) { opts.get("title").cloned().unwrap() } else { Value::Null };
    let body_md_v = if truthy(opts.get("body_md")) { opts.get("body_md").cloned().unwrap() } else { json!("") };
    let meta_v = if truthy(opts.get("meta")) { json!(serde_json::to_string(opts.get("meta").unwrap()).unwrap()) } else { Value::Null };
    let pinned_v: i64 = if truthy(opts.get("pinned")) { 1 } else { 0 };

    let (_changes, rowid) = crate::db_json::run_write(
        db,
        "INSERT INTO nb.notebook_items(notebook_id, kind, title, body_md, meta_json, pinned) VALUES (?, ?, ?, ?, ?, ?)",
        &[num_param(nb_id), kind_v, title_v, body_md_v, meta_v, json!(pinned_v)],
    ).unwrap();
    let rows = crate::db_json::query_rows(db, "SELECT * FROM nb.notebook_items WHERE id = ?", &[json!(rowid)]).unwrap();
    let row = rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
    // FROZEN CLOCK: datetime('now') → the pinned instant (conform-db lane freezes the JS side too).
    crate::db_json::run_write(db, &format!("UPDATE nb.notebooks SET updated_at = {} WHERE id = ?", crate::clock::now_sql()), &[num_param(nb_id)]).unwrap();

    with_extra(&row, "meta", parse_meta(row.get("meta_json").and_then(|v| v.as_str())))
}

/// `export function aiMeta(db, res)` — AI provenance for a notebook item: model/tier/cost/tokens +
/// the ai_call id that opens the I/O window. The lookup is wrapped in a JS try/catch (column/table
/// differs → tokens stay null); mirrored here by treating a query error as "no row found".
pub fn aiMeta(db: &Connection, res: &Value) -> Value {
    let id_v = nz(res.get("ai_call_id")).cloned().unwrap_or(Value::Null);
    let mut tokens_in = Value::Null;
    let mut tokens_out = Value::Null;
    if !id_v.is_null() {
        if let Ok(rows) = crate::db_json::query_rows(db, "SELECT tokens_in, tokens_out FROM brain.ai_calls WHERE id = ?", &[id_v.clone()]) {
            if let Some(r) = rows.as_array().and_then(|a| a.first()) {
                tokens_in = r.get("tokens_in").cloned().unwrap_or(Value::Null);
                tokens_out = r.get("tokens_out").cloned().unwrap_or(Value::Null);
            }
        }
    }
    json!({
        "ai_call_id": id_v,
        "cost_usd": nz(res.get("cost_usd")).cloned().unwrap_or(json!(0)),
        "tier": nz(res.get("tier_used")).cloned().unwrap_or(Value::Null),
        "model": nz(res.get("model_id")).cloned().unwrap_or(Value::Null),
        "duration_ms": nz(res.get("duration_ms")).cloned().unwrap_or(Value::Null),
        "tokens_in": tokens_in,
        "tokens_out": tokens_out,
    })
}

pub fn notebookStore(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS destructures { db } from context and throws err.contextMissing without it — the harness
    // context always carries db, so that guard cannot fire here.
    crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));

    // `const op = input?.op || 'list'` — a falsy op (absent/null/''/0/false) becomes the STRING
    // 'list'; a truthy non-matching op keeps its own value/type for the final `unknown op: ${op}`.
    let op_raw = input.get("op");
    let op_display: Value = if truthy(op_raw) { op_raw.cloned().unwrap() } else { json!("list") };
    let op = op_display.as_str().unwrap_or("");

    if op == "create" {
        let title_v = if truthy(input.get("title")) { input.get("title").cloned().unwrap() } else { json!("") };
        let title = js_string(&title_v).trim().to_string();
        if title.is_empty() { return Err("notebook-store: title is required".into()); }
        let scope: String = if truthy(input.get("scope")) {
            serde_json::to_string(input.get("scope").unwrap()).map_err(|e| e.to_string())?
        } else {
            "{}".into()
        };
        let base = slugify(&title);
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
        let summary_v = if truthy(input.get("summary")) { input.get("summary").cloned().unwrap() } else { Value::Null };
        let (_c, rowid) = crate::db_json::run_write(
            db,
            "INSERT INTO nb.notebooks(slug, title, summary, scope_json) VALUES (?, ?, ?, ?)",
            &[json!(slug), json!(title), summary_v, json!(scope)],
        )?;
        return Ok(json!({ "ok": true, "notebook": getNotebook(db, &json!(rowid)) }));
    }

    // Find-or-create the hidden chat Studio scratch; optionally (re)seed it with the current
    // conversation as a source. Re-seed both on every open (delete the prior pair + the legacy
    // 'text'/'This conversation' row).
    if op == "scratch" {
        let existing = crate::db_json::query_rows(db, "SELECT id FROM nb.notebooks WHERE slug = ?", &[json!(SCRATCH_SLUG)])?;
        let mut row_id = existing.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).cloned();
        if row_id.is_none() {
            crate::db_json::run_write(
                db,
                "INSERT INTO nb.notebooks(slug, title, summary, scope_json) VALUES (?, ?, ?, ?)",
                &[json!(SCRATCH_SLUG), json!("Chat scratch"), json!("Hidden in-chat Studio scratch."), json!("{}")],
            )?;
            let after = crate::db_json::query_rows(db, "SELECT id FROM nb.notebooks WHERE slug = ?", &[json!(SCRATCH_SLUG)])?;
            row_id = after.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).cloned();
        }
        let row_id_v = row_id.unwrap_or(Value::Null);
        if truthy(input.get("conversation_md")) || truthy(input.get("last_response_md")) {
            crate::db_json::run_write(
                db,
                "DELETE FROM nb.notebook_sources WHERE notebook_id = ? AND (kind IN ('conversation','focus') OR (kind = 'text' AND label = 'This conversation'))",
                &[row_id_v.clone()],
            )?;
            if truthy(input.get("last_response_md")) {
                crate::db_json::run_write(
                    db,
                    "INSERT INTO nb.notebook_sources(notebook_id, kind, ref, label, body_md, enabled) VALUES (?, 'focus', NULL, 'Last answer (primary focus)', ?, 1)",
                    &[row_id_v.clone(), json!(js_string(input.get("last_response_md").unwrap()))],
                )?;
            }
            if truthy(input.get("conversation_md")) {
                crate::db_json::run_write(
                    db,
                    "INSERT INTO nb.notebook_sources(notebook_id, kind, ref, label, body_md, enabled) VALUES (?, 'conversation', NULL, 'This conversation', ?, 1)",
                    &[row_id_v.clone(), json!(js_string(input.get("conversation_md").unwrap()))],
                )?;
            }
        }
        return Ok(json!({ "ok": true, "notebook": getNotebook(db, &row_id_v) }));
    }

    if op == "list" {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT n.*, \
               (SELECT COUNT(*) FROM nb.notebook_items i WHERE i.notebook_id = n.id) AS item_count, \
               (SELECT COUNT(*) FROM nb.notebook_items i WHERE i.notebook_id = n.id AND i.pinned = 1) AS pin_count \
             FROM nb.notebooks n WHERE n.slug != ? ORDER BY COALESCE(n.updated_at, n.created_at) DESC, n.id DESC LIMIT 200",
            &[json!(SCRATCH_SLUG)],
        )?;
        let notebooks: Vec<Value> = rows.as_array().cloned().unwrap_or_default().iter()
            .map(|r| with_extra(r, "scope", parse_meta(r.get("scope_json").and_then(|v| v.as_str()))))
            .collect();
        return Ok(json!({ "ok": true, "notebooks": notebooks }));
    }

    let id_n = js_number(input.get("id"));

    if op == "get" {
        if falsy_num(id_n) { return Err("notebook-store: id is required".into()); }
        let nb = getNotebook(db, &num_param(id_n));
        if nb.is_null() { return Ok(json!({ "ok": false, "error": format!("notebook {} not found", tmpl(Some(&num_param(id_n)))) })); }
        let rows = crate::db_json::query_rows(db, "SELECT * FROM nb.notebook_items WHERE notebook_id = ? ORDER BY created_at ASC, id ASC", &[num_param(id_n)])?;
        let items: Vec<Value> = rows.as_array().cloned().unwrap_or_default().iter()
            .map(|r| with_extra(r, "meta", parse_meta(r.get("meta_json").and_then(|v| v.as_str()))))
            .collect();
        return Ok(json!({ "ok": true, "notebook": nb, "items": items }));
    }

    if op == "update" {
        if falsy_num(id_n) { return Err("notebook-store: id is required".into()); }
        let mut sets: Vec<&str> = Vec::new();
        let mut vals: Vec<Value> = Vec::new();
        if input.get("title").map_or(false, |v| !v.is_null()) {
            sets.push("title = ?");
            vals.push(json!(js_string(input.get("title").unwrap())));
        }
        if input.get("summary").map_or(false, |v| !v.is_null()) {
            sets.push("summary = ?");
            vals.push(json!(js_string(input.get("summary").unwrap())));
        }
        if input.get("scope").map_or(false, |v| !v.is_null()) {
            sets.push("scope_json = ?");
            vals.push(json!(serde_json::to_string(input.get("scope").unwrap()).map_err(|e| e.to_string())?));
        }
        if !sets.is_empty() {
            // Vec<String> (not Vec<&str>): the clock snippet is chosen at runtime, so the
            // updated_at fragment is built, not borrowed from a literal.
            let mut full_sets: Vec<String> = sets.iter().map(|s| s.to_string()).collect();
            full_sets.push(format!("updated_at = {}", crate::clock::now_sql()));
            let sql = format!("UPDATE nb.notebooks SET {} WHERE id = ?", full_sets.join(", "));
            let mut params = vals.clone();
            params.push(num_param(id_n));
            crate::db_json::run_write(db, &sql, &params)?;
        }
        return Ok(json!({ "ok": true, "notebook": getNotebook(db, &num_param(id_n)) }));
    }

    if op == "delete" {
        if falsy_num(id_n) { return Err("notebook-store: id is required".into()); }
        crate::db_json::run_write(db, "DELETE FROM nb.notebook_items WHERE notebook_id = ?", &[num_param(id_n)])?;
        crate::db_json::run_write(db, "DELETE FROM nb.notebooks WHERE id = ?", &[num_param(id_n)])?;
        return Ok(json!({ "ok": true, "deleted": num_param(id_n) }));
    }

    if op == "add_item" {
        if falsy_num(id_n) { return Err("notebook-store: id is required".into()); }
        let kind_v = if truthy(input.get("kind")) { input.get("kind").cloned().unwrap() } else { json!("note") };
        let opts = json!({
            "kind": kind_v,
            "title": input.get("title").cloned().unwrap_or(Value::Null),
            "body_md": input.get("body_md").cloned().unwrap_or(Value::Null),
            "meta": input.get("meta").cloned().unwrap_or(Value::Null),
            "pinned": if truthy(input.get("pinned")) { 1 } else { 0 },
        });
        let item = insertItem(db, &num_param(id_n), &opts);
        return Ok(json!({ "ok": true, "item": item }));
    }

    if op == "pin" {
        let item_id_n = js_number(input.get("item_id"));
        if falsy_num(item_id_n) { return Err("notebook-store: item_id is required".into()); }
        // `input?.pinned === false ? 0 : 1` — strict equality against the boolean false ONLY.
        let pinned: i64 = if input.get("pinned") == Some(&Value::Bool(false)) { 0 } else { 1 };
        crate::db_json::run_write(db, "UPDATE nb.notebook_items SET pinned = ? WHERE id = ?", &[json!(pinned), num_param(item_id_n)])?;
        let rows = crate::db_json::query_rows(db, "SELECT * FROM nb.notebook_items WHERE id = ?", &[num_param(item_id_n)])?;
        let item = match rows.as_array().and_then(|a| a.first()) {
            Some(r) => with_extra(r, "meta", parse_meta(r.get("meta_json").and_then(|v| v.as_str()))),
            None => Value::Null,
        };
        return Ok(json!({ "ok": true, "item": item }));
    }

    Ok(json!({ "ok": false, "error": format!("unknown op: {}", tmpl(Some(&op_display))) }))
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
    fn create_slugifies_dedupes_and_defaults() {
        let c = fresh();
        let r = notebookStore(&json!({ "op": "create", "title": "  My First Notebook!!  " }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["notebook"]["slug"], json!("my-first-notebook"));
        assert_eq!(r["notebook"]["title"], json!("My First Notebook!!")); // trimmed, NOT slugified
        assert_eq!(r["notebook"]["summary"], Value::Null);
        assert_eq!(r["notebook"]["scope_json"], json!("{}"));
        assert_eq!(r["notebook"]["scope"], json!({}));

        // same title again → slug collision → base-2
        let r2 = notebookStore(&json!({ "op": "create", "title": "  My First Notebook!!  ", "summary": "s", "scope": { "paths": ["a"] } }), &c).unwrap();
        assert_eq!(r2["notebook"]["slug"], json!("my-first-notebook-2"));
        assert_eq!(r2["notebook"]["summary"], json!("s"));
        assert_eq!(r2["notebook"]["scope"], json!({ "paths": ["a"] }));

        assert!(notebookStore(&json!({ "op": "create", "title": "   " }), &c).is_err()); // blank title
    }

    #[test]
    fn scratch_find_or_create_and_reseed() {
        let c = fresh();
        let a = notebookStore(&json!({ "op": "scratch", "conversation_md": "**Q:** explain auth" }), &c).unwrap();
        assert_eq!(a["notebook"]["slug"], json!("chat-scratch"));
        let sources = crate::db_json::query_rows(&c, "SELECT kind, label, body_md FROM nb.notebook_sources WHERE notebook_id = ?", &[a["notebook"]["id"].clone()]).unwrap();
        assert_eq!(sources, json!([{ "kind": "conversation", "label": "This conversation", "body_md": "**Q:** explain auth" }]));

        let b = notebookStore(&json!({ "op": "scratch" }), &c).unwrap(); // find-or-create, no reseed (no fields)
        assert_eq!(b["notebook"]["id"], a["notebook"]["id"]);

        // reseed with both fields replaces (not duplicates) the pair
        notebookStore(&json!({ "op": "scratch", "conversation_md": "v2", "last_response_md": "answer v2" }), &c).unwrap();
        let after = crate::db_json::query_rows(&c, "SELECT kind, label, body_md FROM nb.notebook_sources WHERE notebook_id = ? ORDER BY kind", &[a["notebook"]["id"].clone()]).unwrap();
        assert_eq!(
            after,
            json!([
                { "kind": "conversation", "label": "This conversation", "body_md": "v2" },
                { "kind": "focus", "label": "Last answer (primary focus)", "body_md": "answer v2" }
            ])
        );
    }

    #[test]
    fn list_excludes_scratch_and_counts_items() {
        let c = fresh();
        notebookStore(&json!({ "op": "scratch", "conversation_md": "x" }), &c).unwrap();
        let real = notebookStore(&json!({ "op": "create", "title": "Real one" }), &c).unwrap();
        let nb_id = real["notebook"]["id"].clone();
        insertItem(&c, &nb_id, &json!({ "kind": "note", "pinned": 1 }));
        insertItem(&c, &nb_id, &json!({ "kind": "note" }));
        let list = notebookStore(&json!({ "op": "list" }), &c).unwrap();
        let nbs = list["notebooks"].as_array().unwrap();
        assert!(!nbs.iter().any(|n| n["slug"] == json!("chat-scratch")));
        let real_row = nbs.iter().find(|n| n["title"] == json!("Real one")).unwrap();
        assert_eq!(real_row["item_count"], json!(2));
        assert_eq!(real_row["pin_count"], json!(1));
    }

    #[test]
    fn get_found_and_not_found() {
        let c = fresh();
        let created = notebookStore(&json!({ "op": "create", "title": "T" }), &c).unwrap();
        let id = created["notebook"]["id"].clone();
        insertItem(&c, &id, &json!({ "kind": "note", "title": "Item A" }));
        let r = notebookStore(&json!({ "op": "get", "id": id.clone() }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["items"].as_array().unwrap().len(), 1);
        assert_eq!(r["items"][0]["title"], json!("Item A"));
        assert_eq!(r["items"][0]["meta"], json!({}));

        let nf = notebookStore(&json!({ "op": "get", "id": 999999 }), &c).unwrap();
        assert_eq!(nf, json!({ "ok": false, "error": "notebook 999999 not found" }));

        assert!(notebookStore(&json!({ "op": "get" }), &c).is_err()); // missing id
    }

    #[test]
    fn update_sets_only_provided_fields_and_noop() {
        let c = fresh();
        let created = notebookStore(&json!({ "op": "create", "title": "Orig", "summary": "orig sum" }), &c).unwrap();
        let id = created["notebook"]["id"].clone();
        let r = notebookStore(&json!({ "op": "update", "id": id.clone(), "title": "New Title" }), &c).unwrap();
        assert_eq!(r["notebook"]["title"], json!("New Title"));
        assert_eq!(r["notebook"]["summary"], json!("orig sum")); // untouched

        let r2 = notebookStore(&json!({ "op": "update", "id": id.clone(), "scope": { "x": 1 } }), &c).unwrap();
        assert_eq!(r2["notebook"]["scope"], json!({ "x": 1 }));

        // no fields → no-op update, still returns the notebook
        let r3 = notebookStore(&json!({ "op": "update", "id": id.clone() }), &c).unwrap();
        assert_eq!(r3["notebook"]["title"], json!("New Title"));

        assert!(notebookStore(&json!({ "op": "update" }), &c).is_err()); // missing id
    }

    #[test]
    fn delete_removes_items_and_notebook() {
        let c = fresh();
        let created = notebookStore(&json!({ "op": "create", "title": "Del me" }), &c).unwrap();
        let id = created["notebook"]["id"].clone();
        insertItem(&c, &id, &json!({}));
        let r = notebookStore(&json!({ "op": "delete", "id": id.clone() }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "deleted": id.clone() }));
        let items = crate::db_json::query_rows(&c, "SELECT COUNT(*) c FROM nb.notebook_items WHERE notebook_id = ?", &[id.clone()]).unwrap();
        assert_eq!(items[0]["c"], json!(0));
        let nbs = crate::db_json::query_rows(&c, "SELECT COUNT(*) c FROM nb.notebooks WHERE id = ?", &[id]).unwrap();
        assert_eq!(nbs[0]["c"], json!(0));
    }

    #[test]
    fn add_item_defaults_and_explicit() {
        let c = fresh();
        let created = notebookStore(&json!({ "op": "create", "title": "T" }), &c).unwrap();
        let id = created["notebook"]["id"].clone();
        let r = notebookStore(&json!({ "op": "add_item", "id": id.clone() }), &c).unwrap();
        assert_eq!(r["item"]["kind"], json!("note"));
        assert_eq!(r["item"]["title"], Value::Null);
        assert_eq!(r["item"]["body_md"], json!(""));
        assert_eq!(r["item"]["pinned"], json!(0));

        let r2 = notebookStore(&json!({ "op": "add_item", "id": id, "kind": "ask", "title": "Q", "body_md": "answer", "meta": { "a": 1 }, "pinned": true }), &c).unwrap();
        assert_eq!(r2["item"]["kind"], json!("ask"));
        assert_eq!(r2["item"]["title"], json!("Q"));
        assert_eq!(r2["item"]["pinned"], json!(1));
        assert_eq!(r2["item"]["meta"], json!({ "a": 1 }));

        assert!(notebookStore(&json!({ "op": "add_item" }), &c).is_err()); // missing id
    }

    #[test]
    fn pin_strict_false_and_absent_and_not_found() {
        let c = fresh();
        let created = notebookStore(&json!({ "op": "create", "title": "T" }), &c).unwrap();
        let nb_id = created["notebook"]["id"].clone();
        let item = insertItem(&c, &nb_id, &json!({ "kind": "note" }));
        let item_id = item["id"].clone();

        let p1 = notebookStore(&json!({ "op": "pin", "item_id": item_id.clone(), "pinned": false }), &c).unwrap();
        assert_eq!(p1["item"]["pinned"], json!(0));
        let p2 = notebookStore(&json!({ "op": "pin", "item_id": item_id.clone() }), &c).unwrap(); // absent → 1
        assert_eq!(p2["item"]["pinned"], json!(1));
        let p3 = notebookStore(&json!({ "op": "pin", "item_id": item_id.clone(), "pinned": 0 }), &c).unwrap(); // 0 !== false → 1
        assert_eq!(p3["item"]["pinned"], json!(1));

        let nf = notebookStore(&json!({ "op": "pin", "item_id": 999999 }), &c).unwrap();
        assert_eq!(nf, json!({ "ok": true, "item": Value::Null }));

        assert!(notebookStore(&json!({ "op": "pin" }), &c).is_err()); // missing item_id
    }

    #[test]
    fn default_op_and_unknown_op() {
        let c = fresh();
        let r = notebookStore(&json!({}), &c).unwrap(); // op absent → list
        assert_eq!(r, json!({ "ok": true, "notebooks": [] }));
        let bad = notebookStore(&json!({ "op": "bogus" }), &c).unwrap();
        assert_eq!(bad, json!({ "ok": false, "error": "unknown op: bogus" }));
    }

    #[test]
    fn get_notebook_helper_found_and_missing_scope() {
        let c = fresh();
        c.execute_batch("CREATE TABLE nb.notebooks (id INTEGER PRIMARY KEY, slug TEXT, title TEXT NOT NULL, summary TEXT, scope_json TEXT NOT NULL DEFAULT '{}', created_at TEXT, updated_at TEXT); \
            INSERT INTO nb.notebooks (id, slug, title, summary, scope_json, created_at, updated_at) VALUES (1, 's1', 'T1', NULL, '{\"a\":1}', '2026-01-01 00:00:00', NULL);").unwrap();
        let r = getNotebook(&c, &json!(1));
        assert_eq!(r["title"], json!("T1"));
        assert_eq!(r["scope"], json!({ "a": 1 }));
        assert_eq!(getNotebook(&c, &json!(999)), Value::Null);
    }

    #[test]
    fn ai_meta_found_missing_and_defaults() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, tokens_in INTEGER, tokens_out INTEGER);
            INSERT INTO brain.ai_calls (id, tokens_in, tokens_out) VALUES (7, 120, 45);").unwrap();
        let found = aiMeta(&c, &json!({ "ai_call_id": 7, "cost_usd": 0.02, "tier_used": "mid", "model_id": "m1", "duration_ms": 300 }));
        assert_eq!(found, json!({ "ai_call_id": 7, "cost_usd": 0.02, "tier": "mid", "model": "m1", "duration_ms": 300, "tokens_in": 120, "tokens_out": 45 }));

        let missing = aiMeta(&c, &json!({ "ai_call_id": 999 }));
        assert_eq!(missing["tokens_in"], Value::Null);
        assert_eq!(missing["tokens_out"], Value::Null);
        assert_eq!(missing["cost_usd"], json!(0));

        let absent = aiMeta(&c, &json!({}));
        assert_eq!(absent, json!({ "ai_call_id": Value::Null, "cost_usd": 0, "tier": Value::Null, "model": Value::Null, "duration_ms": Value::Null, "tokens_in": Value::Null, "tokens_out": Value::Null }));
    }

    #[test]
    fn insert_item_helper_opts_absent_defaults() {
        let c = fresh();
        c.execute_batch("CREATE TABLE nb.notebooks (id INTEGER PRIMARY KEY, slug TEXT, title TEXT NOT NULL, summary TEXT, scope_json TEXT NOT NULL DEFAULT '{}', created_at TEXT, updated_at TEXT); \
            INSERT INTO nb.notebooks (id, slug, title) VALUES (1, 's1', 'T1'); \
            CREATE TABLE nb.notebook_items (id INTEGER PRIMARY KEY AUTOINCREMENT, notebook_id INTEGER NOT NULL, kind TEXT NOT NULL DEFAULT 'note', title TEXT, body_md TEXT NOT NULL DEFAULT '', meta_json TEXT, pinned INTEGER NOT NULL DEFAULT 0, created_at TEXT);").unwrap();
        let r = insertItem(&c, &json!(1), &Value::Null); // opts entirely absent
        assert_eq!(r["kind"], json!("note"));
        assert_eq!(r["title"], Value::Null);
        assert_eq!(r["body_md"], json!(""));
        assert_eq!(r["pinned"], json!(0));
        assert_eq!(r["meta"], json!({}));
        let updated = crate::db_json::query_rows(&c, "SELECT updated_at FROM nb.notebooks WHERE id = 1", &[]).unwrap();
        assert_eq!(updated[0]["updated_at"], json!("2026-01-01 00:00:00"));
    }
}
