//! notebook-source.rs — twin of skills/notebooks/notebook-source.mjs (db WRITE/READ lane,
//! op-routed). NAMES ARE CONTRACT: `notebookSource` (default). CRUD for a notebook's grounded-corpus
//! sources: list | add | import (dedup on notebook_id+kind='source'+ref) | toggle | remove.
// QUALITY: 82/100 — self-heals nb.* schema via the converted notebook-schema twin every call;
// import additionally self-heals knowledge.sources via the converted sources-schema twin.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

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
// JS `String(v)` (no `??`) — used inside the `String(x || '')` coercion chains below.
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
// bound value (and the plain output field built from the same coerced number) render identically.
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }
// JS `x || null`.
fn or_null(v: Option<&Value>) -> Value { if truthy(v) { v.cloned().unwrap() } else { Value::Null } }
// JS `String(x || '').trim()` — the `kind` / `source_id` coercion chain (falsy → '', else String(x)).
fn str_or_trim(v: Option<&Value>) -> String {
    let base = if truthy(v) { v.cloned().unwrap() } else { json!("") };
    js_string(&base).trim().to_string()
}

// `const parse = (s, d) => { try { return JSON.parse(s ?? 'null') ?? d; } catch { return d; } }`
// specialized to `d = {}` (the only call site, inside row2src). meta_json absent/NULL → parse the
// literal 'null' → null → falls to {}; invalid JSON → catch → {}; a JSON `null` payload → also {}.
fn parse_meta(meta_json: Option<&str>) -> Value {
    let s = meta_json.unwrap_or("null");
    match serde_json::from_str::<Value>(s) {
        Ok(Value::Null) | Err(_) => json!({}),
        Ok(v) => v,
    }
}
// `const row2src = (r) => ({ ...r, meta: parse(r.meta_json, {}) })` — meta is a NEW key, so it is
// appended after the existing (db-column-ordered) keys, matching JS spread-then-insert order.
fn row2src(row: &Value) -> Map<String, Value> {
    let mut m = row.as_object().cloned().unwrap_or_default();
    let meta = parse_meta(m.get("meta_json").and_then(|v| v.as_str()));
    m.insert("meta".into(), meta);
    m
}

pub fn notebookSource(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));

    // `const op = input?.op || 'list'` — a falsy op (absent/null/''/0/false) becomes the STRING
    // 'list'; a truthy non-matching op keeps its own value/type for the final `unknown op: ${op}`.
    let op_raw = input.get("op");
    let op_display: Value = if truthy(op_raw) { op_raw.cloned().unwrap() } else { json!("list") };
    let op = op_display.as_str().unwrap_or("");

    if op == "list" {
        let nb_id = js_number(input.get("notebook_id"));
        if falsy_num(nb_id) { return Err("notebook-source: notebook_id is required".into()); }
        let rows = crate::db_json::query_rows(
            db,
            "SELECT * FROM nb.notebook_sources WHERE notebook_id = ? ORDER BY id ASC",
            &[num_param(nb_id)],
        )?;
        let sources: Vec<Value> = rows.as_array().cloned().unwrap_or_default().iter().map(|r| {
            let mut o = row2src(r);
            let body_md = r.get("body_md");
            let tokens = if truthy(body_md) { json!(crate::system_tokens_count::estimateTokens(body_md.unwrap())) } else { Value::Null };
            o.insert("tokens".into(), tokens);
            Value::Object(o)
        }).collect();
        return Ok(json!({ "ok": true, "sources": sources }));
    }

    if op == "add" {
        let nb_id = js_number(input.get("notebook_id"));
        let kind = str_or_trim(input.get("kind"));
        if falsy_num(nb_id) { return Err("notebook-source: notebook_id is required".into()); }
        if kind.is_empty() { return Err("notebook-source: kind is required".into()); }
        let label_v = if truthy(input.get("label")) { input.get("label").cloned().unwrap() } else { json!(kind.clone()) };
        // `input?.enabled === false ? 0 : 1` — strict equality against the boolean false ONLY.
        let enabled: i64 = if input.get("enabled") == Some(&Value::Bool(false)) { 0 } else { 1 };
        let meta_v: Value = if truthy(input.get("meta")) {
            json!(serde_json::to_string(input.get("meta").unwrap()).unwrap())
        } else {
            Value::Null
        };
        let (_changes, rowid) = crate::db_json::run_write(
            db,
            "INSERT INTO nb.notebook_sources (notebook_id, kind, ref, label, body_md, enabled, meta_json) VALUES (?, ?, ?, ?, ?, ?, ?)",
            &[num_param(nb_id), json!(kind), or_null(input.get("ref")), label_v, or_null(input.get("body_md")), json!(enabled), meta_v],
        )?;
        let rows = crate::db_json::query_rows(db, "SELECT * FROM nb.notebook_sources WHERE id = ?", &[json!(rowid)])?;
        let row = rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        return Ok(json!({ "ok": true, "source": Value::Object(row2src(&row)) }));
    }

    if op == "import" {
        let nb_id = js_number(input.get("notebook_id"));
        if falsy_num(nb_id) { return Err("notebook-source: notebook_id is required".into()); }
        let src_id = str_or_trim(input.get("source_id"));
        if src_id.is_empty() { return Err("notebook-source: source_id is required".into()); }
        crate::sources_sources_schema::ensureSourcesSchema(db, &json!("knowledge."));
        let src_rows = crate::db_json::query_rows(
            db,
            "SELECT id, filename, title, mime, bytes, text FROM knowledge.sources WHERE id = ?",
            &[json!(src_id.clone())],
        )?;
        let src = match src_rows.as_array().and_then(|a| a.first()) {
            Some(s) => s.clone(),
            None => return Ok(json!({ "ok": false, "error": format!("source {} not found", src_id) })),
        };
        let dupe_rows = crate::db_json::query_rows(
            db,
            "SELECT id FROM nb.notebook_sources WHERE notebook_id = ? AND kind = 'source' AND ref = ?",
            &[num_param(nb_id), json!(src_id.clone())],
        )?;
        if let Some(dupe) = dupe_rows.as_array().and_then(|a| a.first()) {
            let rows = crate::db_json::query_rows(db, "SELECT * FROM nb.notebook_sources WHERE id = ?", &[dupe.get("id").cloned().unwrap_or(Value::Null)])?;
            let row = rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
            return Ok(json!({ "ok": true, "deduped": true, "source": Value::Object(row2src(&row)) }));
        }
        let meta = json!({
            "src_id": src.get("id").cloned().unwrap_or(Value::Null),
            "filename": src.get("filename").cloned().unwrap_or(Value::Null),
            "mime": src.get("mime").cloned().unwrap_or(Value::Null),
            "bytes": src.get("bytes").cloned().unwrap_or(Value::Null),
        });
        // JS `src.title || src.filename` — title falsy (null/'') falls back to filename AS-IS.
        let title_v = src.get("title").cloned().unwrap_or(Value::Null);
        let filename_v = src.get("filename").cloned().unwrap_or(Value::Null);
        let label_v = if truthy(Some(&title_v)) { title_v } else { filename_v };
        let body_v = or_null(src.get("text"));
        let (_changes, rowid) = crate::db_json::run_write(
            db,
            "INSERT INTO nb.notebook_sources (notebook_id, kind, ref, label, body_md, enabled, meta_json) VALUES (?, 'source', ?, ?, ?, 1, ?)",
            &[num_param(nb_id), json!(src_id.clone()), label_v, body_v, json!(serde_json::to_string(&meta).unwrap())],
        )?;
        let rows = crate::db_json::query_rows(db, "SELECT * FROM nb.notebook_sources WHERE id = ?", &[json!(rowid)])?;
        let row = rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        return Ok(json!({ "ok": true, "source": Value::Object(row2src(&row)) }));
    }

    if op == "toggle" {
        let id = js_number(input.get("id"));
        if falsy_num(id) { return Err("notebook-source: id is required".into()); }
        let cur_rows = crate::db_json::query_rows(db, "SELECT enabled FROM nb.notebook_sources WHERE id = ?", &[num_param(id)])?;
        let cur = match cur_rows.as_array().and_then(|a| a.first()) {
            Some(c) => c.clone(),
            None => return Ok(json!({ "ok": false, "error": format!("source {} not found", tmpl(Some(&num_param(id)))) })),
        };
        // `input?.enabled != null ? (input.enabled ? 1 : 0) : (cur.enabled ? 0 : 1)`
        let enabled_present = input.get("enabled").map_or(false, |v| !v.is_null());
        let next: i64 = if enabled_present {
            if truthy(input.get("enabled")) { 1 } else { 0 }
        } else if truthy(cur.get("enabled")) {
            0
        } else {
            1
        };
        crate::db_json::run_write(db, "UPDATE nb.notebook_sources SET enabled = ? WHERE id = ?", &[json!(next), num_param(id)])?;
        return Ok(json!({ "ok": true, "id": num_param(id), "enabled": next }));
    }

    if op == "remove" {
        let id = js_number(input.get("id"));
        if falsy_num(id) { return Err("notebook-source: id is required".into()); }
        crate::db_json::run_write(db, "DELETE FROM nb.notebook_sources WHERE id = ?", &[num_param(id)])?;
        return Ok(json!({ "ok": true, "deleted": num_param(id) }));
    }

    Ok(json!({ "ok": false, "error": format!("unknown op: {}", tmpl(Some(&op_display))) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS nb; ATTACH ':memory:' AS knowledge;").unwrap();
        c
    }

    #[test]
    fn add_defaults_and_explicit_fields_and_guards() {
        let c = fresh();
        let r = notebookSource(&json!({ "op": "add", "notebook_id": 1, "kind": "text" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["source"]["notebook_id"], json!(1));
        assert_eq!(r["source"]["kind"], json!("text"));
        assert_eq!(r["source"]["ref"], Value::Null);
        assert_eq!(r["source"]["label"], json!("text")); // label falls back to kind
        assert_eq!(r["source"]["body_md"], Value::Null);
        assert_eq!(r["source"]["enabled"], json!(1));
        assert_eq!(r["source"]["meta_json"], Value::Null);
        assert_eq!(r["source"]["meta"], json!({}));

        let r2 = notebookSource(
            &json!({ "op": "add", "notebook_id": 2, "kind": "file", "ref": "path/to/file.md", "label": "My File", "body_md": "hello", "enabled": false, "meta": { "a": 1 } }),
            &c,
        ).unwrap();
        assert_eq!(r2["source"]["ref"], json!("path/to/file.md"));
        assert_eq!(r2["source"]["label"], json!("My File"));
        assert_eq!(r2["source"]["enabled"], json!(0));
        assert_eq!(r2["source"]["meta_json"], json!("{\"a\":1}"));
        assert_eq!(r2["source"]["meta"], json!({ "a": 1 }));

        assert!(notebookSource(&json!({ "op": "add", "notebook_id": 3 }), &c).is_err()); // missing kind
        assert!(notebookSource(&json!({ "op": "add", "kind": "text" }), &c).is_err()); // missing notebook_id
    }

    #[test]
    fn list_orders_computes_tokens_and_parses_meta() {
        let c = fresh();
        notebookSource(&json!({ "op": "add", "notebook_id": 5, "kind": "text", "body_md": "twelve chars" }), &c).unwrap(); // id 1
        notebookSource(&json!({ "op": "add", "notebook_id": 5, "kind": "ast" }), &c).unwrap(); // id 2, no body
        let r = notebookSource(&json!({ "op": "list", "notebook_id": 5 }), &c).unwrap();
        let sources = r["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0]["id"], json!(1));
        assert_eq!(sources[0]["tokens"], json!(3)); // ceil(12/4)
        assert_eq!(sources[1]["id"], json!(2));
        assert_eq!(sources[1]["tokens"], Value::Null);

        // meta parse: invalid json + literal "null" both default {}; valid json passes through
        c.execute_batch(
            "INSERT INTO nb.notebook_sources (notebook_id, kind, meta_json) VALUES (9, 'text', 'not json'); \
             INSERT INTO nb.notebook_sources (notebook_id, kind, meta_json) VALUES (9, 'text', 'null'); \
             INSERT INTO nb.notebook_sources (notebook_id, kind, meta_json) VALUES (9, 'text', '{\"k\":\"v\"}');",
        ).unwrap();
        let r2 = notebookSource(&json!({ "op": "list", "notebook_id": 9 }), &c).unwrap();
        let metas: Vec<Value> = r2["sources"].as_array().unwrap().iter().map(|s| s["meta"].clone()).collect();
        assert_eq!(metas, vec![json!({}), json!({}), json!({"k":"v"})]);

        assert!(notebookSource(&json!({ "op": "list" }), &c).is_err()); // missing notebook_id
    }

    #[test]
    fn import_not_found_new_and_dedup() {
        let c = fresh();
        let r = notebookSource(&json!({ "op": "import", "notebook_id": 1, "source_id": "ghost" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "source ghost not found" }));

        c.execute(
            "INSERT INTO knowledge.sources (id, filename, rel_path, sha256, bytes, text, title) VALUES ('src-1','notes.md','sources/notes.md','aaa',42,'body text','Notes Title')",
            [],
        ).unwrap();
        let r1 = notebookSource(&json!({ "op": "import", "notebook_id": 1, "source_id": "src-1" }), &c).unwrap();
        assert_eq!(r1["ok"], json!(true));
        assert_eq!(r1["source"]["kind"], json!("source"));
        assert_eq!(r1["source"]["ref"], json!("src-1"));
        assert_eq!(r1["source"]["label"], json!("Notes Title"));
        assert_eq!(r1["source"]["body_md"], json!("body text"));
        assert_eq!(r1["source"]["meta"], json!({ "src_id": "src-1", "filename": "notes.md", "mime": null, "bytes": 42 }));

        c.execute(
            "INSERT INTO knowledge.sources (id, filename, rel_path, sha256, bytes, text, title) VALUES ('src-2','plain.md','sources/plain.md','bbb',10,NULL,NULL)",
            [],
        ).unwrap();
        let r2 = notebookSource(&json!({ "op": "import", "notebook_id": 1, "source_id": "src-2" }), &c).unwrap();
        assert_eq!(r2["source"]["label"], json!("plain.md")); // title null → fallback to filename
        assert_eq!(r2["source"]["body_md"], Value::Null);

        let r3 = notebookSource(&json!({ "op": "import", "notebook_id": 1, "source_id": "src-1" }), &c).unwrap();
        assert_eq!(r3["deduped"], json!(true));
        assert_eq!(r3["source"], r1["source"]);

        assert!(notebookSource(&json!({ "op": "import", "notebook_id": 1 }), &c).is_err()); // missing source_id
        assert!(notebookSource(&json!({ "op": "import", "source_id": "x" }), &c).is_err()); // missing notebook_id
    }

    #[test]
    fn toggle_and_remove() {
        let c = fresh();
        let a = notebookSource(&json!({ "op": "add", "notebook_id": 1, "kind": "text" }), &c).unwrap();
        let id = a["source"]["id"].clone();

        let t1 = notebookSource(&json!({ "op": "toggle", "id": id.clone() }), &c).unwrap(); // absent → flip 1→0
        assert_eq!(t1, json!({ "ok": true, "id": id.clone(), "enabled": 0 }));
        let t2 = notebookSource(&json!({ "op": "toggle", "id": id.clone(), "enabled": true }), &c).unwrap();
        assert_eq!(t2["enabled"], json!(1));
        let t3 = notebookSource(&json!({ "op": "toggle", "id": id.clone(), "enabled": false }), &c).unwrap();
        assert_eq!(t3["enabled"], json!(0));
        let t4 = notebookSource(&json!({ "op": "toggle", "id": id.clone() }), &c).unwrap(); // absent → flip 0→1
        assert_eq!(t4["enabled"], json!(1));

        let nf = notebookSource(&json!({ "op": "toggle", "id": 999 }), &c).unwrap();
        assert_eq!(nf, json!({ "ok": false, "error": "source 999 not found" }));

        let r = notebookSource(&json!({ "op": "remove", "id": id.clone() }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "deleted": id.clone() }));
        let after = notebookSource(&json!({ "op": "list", "notebook_id": 1 }), &c).unwrap();
        assert_eq!(after["sources"].as_array().unwrap().len(), 0);

        assert!(notebookSource(&json!({ "op": "toggle" }), &c).is_err());
        assert!(notebookSource(&json!({ "op": "remove" }), &c).is_err());
    }

    #[test]
    fn default_op_and_unknown_op() {
        let c = fresh();
        let r = notebookSource(&json!({ "notebook_id": 1 }), &c).unwrap(); // op absent → list
        assert_eq!(r, json!({ "ok": true, "sources": [] }));
        let bad = notebookSource(&json!({ "op": "bogus" }), &c).unwrap();
        assert_eq!(bad, json!({ "ok": false, "error": "unknown op: bogus" }));
    }
}
