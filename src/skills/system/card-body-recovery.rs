//! card-body-recovery.rs — twin of skills/system/card-body-recovery.mjs (db WRITE lane; composes
//! the entities_card_update twin). NAMES ARE CONTRACT: `cardBodyRecovery` (default). Walks
//! sacred_timeline per card, double-decodes the RICHEST historical body_json, restores it.
// QUALITY: richest-body pick (keyCount, tiebreak serialized length), double-decode fingerprint,
// dry_run / slug_glob / limit(cap 1..=10000); restore via cardUpdate → updated_at frozen 2026-01-01.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS String(x) coercion (only reached for the decode's non-string edge cases).
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
// JS Number(v).
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}

// JS decodeBody: JSON.parse once; if that yields a STRING (the double-encoded fingerprint), parse
// again, falling back to that string on failure. Anything non-parseable → null. Represented here as
// a Value (Value::Null for the JS `null` returns) — callers only care whether it is a plain object.
fn decode_body(raw: &Value) -> Value {
    if raw.is_null() { return Value::Null; }
    let s = match raw { Value::String(s) => s.clone(), other => js_string(other) };
    match serde_json::from_str::<Value>(&s) {
        Err(_) => Value::Null,
        Ok(first) => match &first {
            Value::String(inner) => serde_json::from_str::<Value>(inner).unwrap_or_else(|_| first.clone()),
            _ => first,
        },
    }
}

struct Best { body: Value, key_count: usize, serialized_len: usize }

pub fn cardBodyRecovery(input: &Value, db: &Connection) -> Result<Value, String> {
    // dry_run: destructuring default (false) only applies to an ABSENT key; the output echoes the
    // value. The write is gated on `!dry_run` (JS truthiness).
    let dry_run_out = match input.get("dry_run") { Some(v) => v.clone(), None => json!(false) };
    let write = !truthy(input.get("dry_run"));

    // limit default 1000; cap = Math.min(Math.max(1, Number(limit) || 1000), 10000).
    let raw_limit = match input.get("limit") { None => 1000.0, Some(v) => js_number(Some(v)) };
    let raw_limit = if raw_limit == 0.0 || raw_limit.is_nan() { 1000.0 } else { raw_limit };
    let cap = raw_limit.max(1.0).min(10000.0) as i64;

    let slug_glob = input.get("slug_glob");
    let use_glob = truthy(slug_glob);
    let cards_sql = if use_glob {
        format!("SELECT slug, body_json FROM cards WHERE slug LIKE ? LIMIT {cap}")
    } else {
        format!("SELECT slug, body_json FROM cards LIMIT {cap}")
    };
    let cards_params: Vec<Value> = if use_glob { vec![slug_glob.unwrap().clone()] } else { vec![] };
    let cards = crate::db_json::query_rows(db, &cards_sql, &cards_params)?
        .as_array().cloned().unwrap_or_default();

    // Distinct slugs in first-seen order (JS `new Set(cards.map(c => c.slug))`).
    let mut slug_values: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for c in &cards {
        let sv = c.get("slug").cloned().unwrap_or(Value::Null);
        let key = match &sv { Value::String(s) => s.clone(), o => o.to_string() };
        if seen.insert(key) { slug_values.push(sv); }
    }

    let events: Vec<Value> = if slug_values.is_empty() {
        Vec::new()
    } else {
        let placeholders = std::iter::repeat("?").take(slug_values.len()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT entity_id, after_json FROM sacred_timeline WHERE entity_type = 'card' AND entity_id IN ({placeholders}) ORDER BY id"
        );
        crate::db_json::query_rows(db, &sql, &slug_values)?.as_array().cloned().unwrap_or_default()
    };

    // Best (richest) decoded body per slug.
    let mut best_per_slug: HashMap<String, Best> = HashMap::new();
    for ev in &events {
        let after_json = ev.get("after_json");
        if !truthy(after_json) { continue; }
        let after = match after_json.and_then(|v| v.as_str()).and_then(|s| serde_json::from_str::<Value>(s).ok()) {
            Some(v) => v,
            None => continue, // JSON.parse threw
        };
        // after?.body_json — undefined/null → skip.
        let raw_body = match after.get("body_json") { Some(v) if !v.is_null() => v, _ => continue };
        let decoded = decode_body(raw_body);
        if !decoded.is_object() { continue; } // JS: !decoded || typeof!=='object' || Array.isArray
        let key_count = decoded.as_object().unwrap().len();
        let serialized_len = serde_json::to_string(&decoded).map_err(|e| e.to_string())?.chars().count();
        let entity_id = ev.get("entity_id").and_then(|v| v.as_str()).map(|s| s.to_string())
            .unwrap_or_else(|| ev.get("entity_id").map(js_string).unwrap_or_default());
        let better = match best_per_slug.get(&entity_id) {
            None => true,
            Some(prev) => key_count > prev.key_count
                || (key_count == prev.key_count && serialized_len > prev.serialized_len),
        };
        if better { best_per_slug.insert(entity_id, Best { body: decoded, key_count, serialized_len }); }
    }

    let mut processed = 0i64;
    let mut restored = 0i64;
    let mut skipped_already_good = 0i64;
    let mut skipped_no_history = 0i64;
    let mut errors: Vec<Value> = Vec::new();
    let mut samples: Vec<Value> = Vec::new();

    for c in &cards {
        processed += 1;
        let slug_v = c.get("slug").cloned().unwrap_or(Value::Null);
        let slug_key = match &slug_v { Value::String(s) => s.clone(), o => o.to_string() };
        let best = match best_per_slug.get(&slug_key) {
            None => { skipped_no_history += 1; continue; }
            Some(b) => b,
        };

        // Current body — double-decode-safe (some cards still carry the double-encoded form).
        let body_json_v = c.get("body_json");
        let current_decoded = if truthy(body_json_v) { decode_body(body_json_v.unwrap()) } else { Value::Null };
        let current_keys: i64 = if current_decoded.is_object() { current_decoded.as_object().unwrap().len() as i64 } else { 0 };

        if current_keys >= best.key_count as i64 { skipped_already_good += 1; continue; }

        if write {
            let up = json!({ "slug": slug_v.clone(), "body": best.body.clone() });
            if let Err(e) = crate::entities_card_update::cardUpdate(&up, db) {
                let truncated: String = e.chars().take(240).collect();
                errors.push(json!({ "slug": slug_v.clone(), "error": truncated }));
                continue;
            }
        }
        restored += 1;
        if samples.len() < 10 {
            samples.push(json!({ "slug": slug_v.clone(), "old_keys": current_keys, "new_keys": best.key_count as i64 }));
        }
    }

    let mut out = Map::new();
    out.insert("dry_run".into(), dry_run_out);
    out.insert("processed".into(), json!(processed));
    out.insert("restored".into(), json!(restored));
    out.insert("skipped_already_good".into(), json!(skipped_already_good));
    out.insert("skipped_no_history".into(), json!(skipped_no_history));
    out.insert("errors".into(), Value::Array(errors));
    out.insert("samples".into(), Value::Array(samples));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT, parent_slug TEXT, status TEXT, updated_at TEXT); \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, entity_type TEXT, entity_id TEXT, after_json TEXT);",
        ).unwrap();
        c
    }
    // Build an after_json TEXT value whose body_json field is the single-encoded JSON of `body`.
    fn after1(body: Value) -> String {
        let s = serde_json::to_string(&body).unwrap();
        serde_json::to_string(&json!({ "slug": "X", "body_json": s })).unwrap()
    }
    // ...and the DOUBLE-encoded variant (body_json is a JSON string of a JSON string).
    fn after2(body: Value) -> String {
        let s3 = serde_json::to_string(&body).unwrap();
        let s2 = serde_json::to_string(&s3).unwrap();
        serde_json::to_string(&json!({ "slug": "X", "body_json": s2 })).unwrap()
    }

    #[test]
    fn dry_run_picks_richest_and_reports_without_mutating() {
        let c = db();
        c.execute("INSERT INTO cards VALUES ('E-1','t','{\"kind\":\"chore\"}',NULL,'backlog','2025-12-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline VALUES (1,'card','E-1',?)", params![after1(json!({"kind":"feature","size":"m"}))]).unwrap();
        c.execute("INSERT INTO sacred_timeline VALUES (2,'card','E-1',?)", params![after1(json!({"kind":"feature","size":"m","why":"rich","notes":"x"}))]).unwrap();
        // an entity_type != 'card' row must be ignored even though it is richer
        c.execute("INSERT INTO sacred_timeline VALUES (3,'action','E-1',?)", params![after1(json!({"a":"1","b":"2","c":"3","d":"4","e":"5"}))]).unwrap();

        let r = cardBodyRecovery(&json!({ "dry_run": true }), &c).unwrap();
        assert_eq!(r["dry_run"], json!(true));
        assert_eq!(r["processed"], json!(1));
        assert_eq!(r["restored"], json!(1));
        assert_eq!(r["skipped_already_good"], json!(0));
        assert_eq!(r["skipped_no_history"], json!(0));
        assert_eq!(r["errors"], json!([]));
        assert_eq!(r["samples"][0], json!({ "slug": "E-1", "old_keys": 1, "new_keys": 4 }));
        // dry_run: card body untouched
        let body: String = c.query_row("SELECT body_json FROM cards WHERE slug='E-1'", [], |row| row.get(0)).unwrap();
        assert_eq!(body, "{\"kind\":\"chore\"}");
    }

    #[test]
    fn write_path_restores_and_double_decodes() {
        let c = db();
        c.execute("INSERT INTO cards VALUES ('D-1','keep','{\"kind\":\"bug\"}',NULL,'backlog','2025-11-01 00:00:00')", []).unwrap();
        // richest historical body is DOUBLE-encoded (the bug's fingerprint)
        c.execute("INSERT INTO sacred_timeline VALUES (1,'card','D-1',?)", params![after2(json!({"kind":"feature","size":"l","why":"human authored"}))]).unwrap();

        let r = cardBodyRecovery(&json!({}), &c).unwrap(); // dry_run absent → false → write
        assert_eq!(r["dry_run"], json!(false));
        assert_eq!(r["restored"], json!(1));
        assert_eq!(r["samples"][0], json!({ "slug": "D-1", "old_keys": 1, "new_keys": 3 }));
        // body_json now the stringified rich body, title preserved, updated_at frozen
        let (body, title, updated): (String, String, String) = c.query_row(
            "SELECT body_json, title, updated_at FROM cards WHERE slug='D-1'", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
        assert_eq!(body, "{\"kind\":\"feature\",\"size\":\"l\",\"why\":\"human authored\"}");
        assert_eq!(title, "keep");
        assert_eq!(updated, "2026-01-01 00:00:00");
    }

    #[test]
    fn skips_no_history_and_already_good_and_glob_filters() {
        let c = db();
        c.execute("INSERT INTO cards VALUES ('G-1','t','{\"kind\":\"bug\"}',NULL,'backlog','2025-01-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO cards VALUES ('G-2','t','{\"kind\":\"epic\",\"size\":\"s\",\"why\":\"solid\",\"x\":\"4\"}',NULL,'backlog','2025-01-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO cards VALUES ('G-3','t','{\"kind\":\"bug\"}',NULL,'backlog','2025-01-01 00:00:00')", []).unwrap(); // no history
        c.execute("INSERT INTO cards VALUES ('OTHER','t','{\"kind\":\"bug\"}',NULL,'backlog','2025-01-01 00:00:00')", []).unwrap(); // excluded by glob
        c.execute("INSERT INTO sacred_timeline VALUES (1,'card','G-2',?)", params![after1(json!({"kind":"epic","size":"s"}))]).unwrap(); // 2 keys < current 4
        c.execute("INSERT INTO sacred_timeline VALUES (2,'card','OTHER',?)", params![after1(json!({"a":"1","b":"2","c":"3"}))]).unwrap();

        let r = cardBodyRecovery(&json!({ "slug_glob": "G-%", "dry_run": true }), &c).unwrap();
        assert_eq!(r["processed"], json!(3)); // G-1,G-2,G-3 (OTHER excluded)
        assert_eq!(r["restored"], json!(0));
        assert_eq!(r["skipped_already_good"], json!(1)); // G-2
        assert_eq!(r["skipped_no_history"], json!(2)); // G-1, G-3
    }

    #[test]
    fn tiebreak_prefers_longer_serialization() {
        let c = db();
        c.execute("INSERT INTO cards VALUES ('T-1','t','{\"kind\":\"bug\"}',NULL,'backlog','2025-01-01 00:00:00')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline VALUES (1,'card','T-1',?)", params![after1(json!({"kind":"a","size":"b"}))]).unwrap();
        c.execute("INSERT INTO sacred_timeline VALUES (2,'card','T-1',?)", params![after1(json!({"kind":"feature","size":"a-much-longer-value"}))]).unwrap();
        let r = cardBodyRecovery(&json!({}), &c).unwrap();
        assert_eq!(r["restored"], json!(1));
        let body: String = c.query_row("SELECT body_json FROM cards WHERE slug='T-1'", [], |row| row.get(0)).unwrap();
        assert_eq!(body, "{\"kind\":\"feature\",\"size\":\"a-much-longer-value\"}"); // the longer of the two 2-key bodies
    }
}
