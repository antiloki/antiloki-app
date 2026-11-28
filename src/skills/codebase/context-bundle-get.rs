//! context-bundle-get.rs — twin of skills/codebase/context-bundle-get.mjs (cross-db READ lane).
//! NAMES ARE CONTRACT: `contextBundleGet` (default). Reads brain.context_bundles for one ai_call,
//! parsing the *_json columns back into arrays; { found:false } when no bundle row (or legacy db).
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
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
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

// JS: const parse = (s, fallback) => { try { const v = JSON.parse(s ?? ''); return v ?? fallback; } catch { return fallback; } };
fn parse_or(v: Option<&Value>, fallback: Value) -> Value {
    let s = match v {
        None | Some(Value::Null) => String::new(), // s ?? '' → JSON.parse('') throws → fallback
        Some(Value::String(s)) => s.clone(),
        Some(o) => js_string(o), // JSON.parse coerces its argument via String()
    };
    match serde_json::from_str::<Value>(&s) {
        Ok(Value::Null) => fallback, // parsed null → the `v ?? fallback` arm
        Ok(x) => x,
        Err(_) => fallback,
    }
}

// absent column → JS `row.k` is undefined → key omitted by JSON.stringify; SQL NULL stays as null
fn put(m: &mut Map<String, Value>, k: &str, v: Option<&Value>) {
    if let Some(x) = v { m.insert(k.to_string(), x.clone()); }
}

pub fn contextBundleGet(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('context-bundle-get','db') when no db — the db lane always injects one.
    let n = js_number(input.get("ai_call_id"));
    if !(n.is_finite() && n.fract() == 0.0 && n > 0.0) {
        return Err("context-bundle-get: ai_call_id is required".into());
    }
    let ai_call_id = n as i64;
    let rows = match crate::db_json::query_rows(db, "SELECT * FROM brain.context_bundles WHERE ai_call_id = ?", &[json!(ai_call_id)]) {
        Ok(r) => r,
        Err(_) => return Ok(json!({ "ok": true, "found": false })), // table absent on a legacy DB
    };
    let row = match rows.as_array().and_then(|a| a.first()).cloned() {
        Some(r) => r,
        None => return Ok(json!({ "ok": true, "found": false })),
    };
    let items = parse_or(row.get("items_json"), json!([]));
    let mut bundle = Map::new();
    put(&mut bundle, "ai_call_id", row.get("ai_call_id"));
    put(&mut bundle, "agent", row.get("agent"));
    put(&mut bundle, "phase", row.get("phase"));
    put(&mut bundle, "bone_tokens", row.get("bone_tokens"));
    put(&mut bundle, "sliced_tokens", row.get("sliced_tokens"));
    put(&mut bundle, "total_tokens", row.get("total_tokens"));
    put(&mut bundle, "budget", row.get("budget"));
    bundle.insert("over_budget".into(), json!(truthy(row.get("over_budget"))));
    bundle.insert("items".into(), items.clone());
    bundle.insert("dropped".into(), parse_or(row.get("dropped_json"), json!([])));
    bundle.insert("skipped".into(), parse_or(row.get("skipped_json"), json!([])));
    // JS items.length: array → count, string → UTF-16 units, object → its own "length" key, else undefined (omitted)
    match &items {
        Value::Array(a) => { bundle.insert("item_count".into(), json!(a.len())); }
        Value::String(s) => { bundle.insert("item_count".into(), json!(s.encode_utf16().count())); }
        Value::Object(o) => { if let Some(l) = o.get("length") { bundle.insert("item_count".into(), l.clone()); } }
        _ => {}
    }
    put(&mut bundle, "created_at", row.get("created_at"));
    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("found".into(), json!(true));
    out.insert("bundle".into(), Value::Object(bundle));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db_with_table() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; \
          CREATE TABLE brain.context_bundles ( ai_call_id INTEGER PRIMARY KEY, agent TEXT, phase TEXT, bone_tokens INTEGER NOT NULL DEFAULT 0, sliced_tokens INTEGER NOT NULL DEFAULT 0, total_tokens INTEGER NOT NULL DEFAULT 0, budget INTEGER, over_budget INTEGER NOT NULL DEFAULT 0, items_json TEXT, dropped_json TEXT, skipped_json TEXT, created_at TEXT NOT NULL );").unwrap();
        c
    }
    #[test]
    fn found_parses_json_columns() {
        let c = db_with_table();
        c.execute_batch("INSERT INTO brain.context_bundles VALUES (7,'interviewer','chat',1200,3400,4600,8000,1,'[{\"id\":\"m:12\",\"tier\":\"sliced\",\"type\":\"memory\",\"source\":\"rag\",\"tokens\":140,\"relevance\":0.82,\"freshness\":0.5,\"text\":\"memory text\"}]','[{\"id\":\"m:99\",\"reason\":\"budget\"}]','[\"stale:law:4\"]','2026-01-01 00:00:00');").unwrap();
        let r = contextBundleGet(&json!({ "ai_call_id": 7 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["found"], json!(true));
        let b = &r["bundle"];
        assert_eq!(b["ai_call_id"], json!(7));
        assert_eq!(b["over_budget"], json!(true));
        assert_eq!(b["items"][0]["id"], json!("m:12"));
        assert_eq!(b["item_count"], json!(1));
        assert_eq!(b["dropped"][0]["reason"], json!("budget"));
        assert_eq!(b["skipped"], json!(["stale:law:4"]));
        assert_eq!(b["created_at"], json!("2026-01-01 00:00:00"));
    }
    #[test]
    fn bad_json_falls_back_and_null_parses_to_fallback() {
        let c = db_with_table();
        c.execute_batch("INSERT INTO brain.context_bundles VALUES (9,NULL,NULL,0,0,0,NULL,0,NULL,'not-json','null','2026-01-01 00:00:00');").unwrap();
        let b = &contextBundleGet(&json!({ "ai_call_id": 9 }), &c).unwrap()["bundle"];
        assert_eq!(b["items"], json!([]));
        assert_eq!(b["dropped"], json!([])); // invalid JSON → fallback
        assert_eq!(b["skipped"], json!([])); // JSON null → `?? fallback`
        assert_eq!(b["item_count"], json!(0));
        assert_eq!(b["over_budget"], json!(false));
        assert_eq!(b["agent"], json!(null));
    }
    #[test]
    fn non_array_items_omits_item_count() {
        let c = db_with_table();
        c.execute_batch("INSERT INTO brain.context_bundles VALUES (11,'a','b',1,2,3,4,0,'{\"weird\":1}','\"solo\"','0','2026-01-01 00:00:00');").unwrap();
        let b = &contextBundleGet(&json!({ "ai_call_id": "11" }), &c).unwrap()["bundle"];
        assert!(b.get("item_count").is_none()); // object without .length → undefined → omitted
        assert_eq!(b["dropped"], json!("solo"));
        assert_eq!(b["skipped"], json!(0));
    }
    #[test]
    fn absent_row_and_legacy_db_return_found_false() {
        let c = db_with_table();
        assert_eq!(contextBundleGet(&json!({ "ai_call_id": 500 }), &c).unwrap(), json!({ "ok": true, "found": false }));
        let legacy = Connection::open_in_memory().unwrap(); // no brain attach → query errors → caught
        assert_eq!(contextBundleGet(&json!({ "ai_call_id": 1 }), &legacy).unwrap(), json!({ "ok": true, "found": false }));
    }
    #[test]
    fn invalid_id_errs() {
        let c = db_with_table();
        assert!(contextBundleGet(&json!({}), &c).is_err());
        assert!(contextBundleGet(&json!({ "ai_call_id": 0 }), &c).is_err());
        assert!(contextBundleGet(&json!({ "ai_call_id": 2.5 }), &c).is_err());
    }
}
