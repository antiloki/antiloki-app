//! instrument-detectors.rs — twin of skills/codebase/instrument-detectors.mjs (db READ, attach
//! lane). NAMES ARE CONTRACT: `instrumentDetectors` (default). P5 INSTRUMENT FOUNDRY anti-pattern
//! detectors — pure arithmetic over the code atlas (god-file / shotgun-surgery / hub-dependency),
//! each citing an IR knowledge_unit when the concept is imported there, else a static attribution.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand: nz(a).or(nz(b))
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

const DETECTOR_KEYS: [&str; 3] = ["god-file", "shotgun-surgery", "hub-dependency"];

fn threshold_json(key: &str) -> &'static str {
    match key {
        "god-file" => "{\"defs\":25,\"fanin\":12}",
        "shotgun-surgery" => "{\"fanout\":20}",
        "hub-dependency" => "{\"fanin\":15,\"fanout\":10}",
        _ => "{}",
    }
}
fn ir_key(key: &str) -> &'static str {
    match key {
        "god-file" => "god object",
        "shotgun-surgery" => "shotgun surgery",
        "hub-dependency" => "stable dependencies principle",
        _ => "",
    }
}
fn static_cite(key: &str) -> &'static str {
    match key {
        "god-file" => "Riel, Object-Oriented Design Heuristics (1996) · Fowler, Refactoring (1999) — static attribution; import the book to cite its own words",
        "shotgun-surgery" => "Fowler, Refactoring (1999), \"Shotgun Surgery\" — static attribution; import the book to cite its own words",
        "hub-dependency" => "Martin, Agile Software Development (2002), stability metrics — static attribution; import the book to cite its own words",
        _ => "",
    }
}

// mirrors JS `(JSON.parse(u.receipts_json || '[]')[0]?.quote ?? '').slice(0,160)`. A malformed
// receipts_json (or no matching row) is not an error worth reporting — both fall through to the
// static citation exactly like the JS try/catch collapses either case into the same fallback.
fn try_ir_citation(db: &Connection, key: &str) -> Result<Option<Value>, String> {
    let rows = crate::db_json::query_rows(db,
        "SELECT title, authority, receipts_json FROM knowledge.knowledge_units WHERE kind IN ('concept','doctrine') AND identity_key = ? ORDER BY id DESC LIMIT 1",
        &[json!(ir_key(key))])?;
    let arr = rows.as_array().cloned().unwrap_or_default();
    let u = match arr.first() { Some(u) => u, None => return Ok(None) };
    let raw = u.get("receipts_json").filter(|v| !v.is_null());
    let receipts_str = match raw { Some(Value::String(s)) if !s.is_empty() => s.clone(), _ => "[]".to_string() };
    let receipts: Value = serde_json::from_str(&receipts_str).map_err(|e| e.to_string())?;
    let quote_val = receipts.as_array().and_then(|a| a.first()).and_then(|o| o.get("quote")).cloned();
    let quote = str_qq(nz(quote_val.as_ref()));
    let quote_sliced: String = quote.chars().take(160).collect();
    let mut out = Map::new();
    out.insert("source".into(), json!("ir"));
    out.insert("authority".into(), u.get("authority").cloned().unwrap_or(Value::Null));
    out.insert("title".into(), u.get("title").cloned().unwrap_or(Value::Null));
    out.insert("quote".into(), json!(quote_sliced));
    Ok(Some(Value::Object(out)))
}

fn citation(db: &Connection, key: &str) -> Value {
    if let Ok(Some(v)) = try_ir_citation(db, key) { return v; }
    let mut out = Map::new();
    out.insert("source".into(), json!("static"));
    out.insert("note".into(), json!(static_cite(key)));
    Value::Object(out)
}

pub fn instrumentDetectors(input: &Value, db: &Connection) -> Result<Value, String> {
    let prefixes: Option<Vec<String>> = input.get("paths").and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(js_string).filter(|s| !s.is_empty()).collect());
    let limit_input = nz(input.get("limit")).cloned().unwrap_or(json!(20));
    let n = js_number(Some(&limit_input));
    let n = if n.is_nan() || n == 0.0 { 20.0 } else { n };
    let n = n.max(1.0).min(100.0);
    let limit = n.floor() as usize;

    let in_scope = |p: &str| -> bool {
        match &prefixes {
            None => true,
            Some(list) => list.iter().any(|x| {
                let xs = x.as_str();
                let trimmed = xs.strip_suffix('/').unwrap_or(xs);
                let needle = format!("{trimmed}/");
                p == xs || p.starts_with(needle.as_str())
            }),
        }
    };

    // the arithmetic layer — all from the code atlas, one pass.
    let files_rows = crate::db_json::query_rows(db, "SELECT id, path FROM code.code_nodes WHERE origin = 'atlas' AND type = 'file'", &[])?;
    let files: Vec<(String, String)> = files_rows.as_array().unwrap().iter()
        .map(|r| (js_string(r.get("id").unwrap_or(&Value::Null)), js_string(r.get("path").unwrap_or(&Value::Null))))
        .collect();

    let mut defs: HashMap<String, i64> = HashMap::new();
    let defs_rows = crate::db_json::query_rows(db, "SELECT path, COUNT(*) AS n FROM code.code_nodes WHERE origin = 'atlas' AND type != 'file' GROUP BY path", &[])?;
    for r in defs_rows.as_array().unwrap() {
        defs.insert(js_string(r.get("path").unwrap_or(&Value::Null)), r.get("n").and_then(|v| v.as_i64()).unwrap_or(0));
    }

    let mut fanin: HashMap<String, i64> = HashMap::new();
    let mut fanout: HashMap<String, i64> = HashMap::new();
    let edge_rows = crate::db_json::query_rows(db, "SELECT from_node, to_node FROM code.code_edges", &[])?;
    for e in edge_rows.as_array().unwrap() {
        let from_node = js_string(e.get("from_node").unwrap_or(&Value::Null));
        let to_node = js_string(e.get("to_node").unwrap_or(&Value::Null));
        *fanout.entry(from_node).or_insert(0) += 1;
        *fanin.entry(to_node).or_insert(0) += 1;
    }

    let mut cites: HashMap<&str, Value> = HashMap::new();
    for k in DETECTOR_KEYS { cites.insert(k, citation(db, k)); }

    let mut findings: Vec<Value> = Vec::new();
    for (id, path) in &files {
        if !in_scope(path) { continue; }
        let d = defs.get(path).copied().unwrap_or(0);
        let fi = fanin.get(id).copied().unwrap_or(0);
        let fo = fanout.get(id).copied().unwrap_or(0);
        if d >= 25 && fi >= 12 {
            let mut ev = Map::new();
            ev.insert("defs".into(), json!(d));
            ev.insert("fanin".into(), json!(fi));
            let mut f = Map::new();
            f.insert("detector".into(), json!("god-file"));
            f.insert("path".into(), json!(path));
            f.insert("evidence".into(), Value::Object(ev));
            f.insert("threshold".into(), json!("defs ≥ 25 AND fan-in ≥ 12"));
            f.insert("citation".into(), cites["god-file"].clone());
            findings.push(Value::Object(f));
        }
        if fo >= 20 {
            let mut ev = Map::new();
            ev.insert("fanout".into(), json!(fo));
            let mut f = Map::new();
            f.insert("detector".into(), json!("shotgun-surgery"));
            f.insert("path".into(), json!(path));
            f.insert("evidence".into(), Value::Object(ev));
            f.insert("threshold".into(), json!("fan-out ≥ 20"));
            f.insert("citation".into(), cites["shotgun-surgery"].clone());
            findings.push(Value::Object(f));
        }
        if fi >= 15 && fo >= 10 {
            let mut ev = Map::new();
            ev.insert("fanin".into(), json!(fi));
            ev.insert("fanout".into(), json!(fo));
            let mut f = Map::new();
            f.insert("detector".into(), json!("hub-dependency"));
            f.insert("path".into(), json!(path));
            f.insert("evidence".into(), Value::Object(ev));
            f.insert("threshold".into(), json!("fan-in ≥ 15 AND fan-out ≥ 10"));
            f.insert("citation".into(), cites["hub-dependency"].clone());
            findings.push(Value::Object(f));
        }
    }

    fn evidence_sum(v: &Value) -> i64 {
        v.get("evidence").and_then(|e| e.as_object()).map(|m| m.values().filter_map(|x| x.as_i64()).sum()).unwrap_or(0)
    }
    findings.sort_by(|a, b| evidence_sum(b).cmp(&evidence_sum(a)));
    findings.truncate(limit);

    let scanned = files.iter().filter(|f| in_scope(&f.1)).count();

    let detectors: Vec<Value> = DETECTOR_KEYS.into_iter().map(|k| {
        let mut o = Map::new();
        o.insert("key".into(), json!(k));
        o.insert("threshold".into(), json!(threshold_json(k)));
        o.insert("citation".into(), cites[k].clone());
        Value::Object(o)
    }).collect();

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("detectors".into(), json!(detectors));
    out.insert("findings".into(), json!(findings));
    out.insert("scanned".into(), json!(scanned as i64));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_file(c: &Connection, id: &str, path: &str, defs: i64, fanin: i64, fanout: i64) {
        c.execute("INSERT INTO code.code_nodes (id, origin, path, type) VALUES (?1, 'atlas', ?2, 'file')", rusqlite::params![id, path]).unwrap();
        for i in 0..defs {
            c.execute("INSERT INTO code.code_nodes (id, origin, path, type) VALUES (?1, 'atlas', ?2, 'function')", rusqlite::params![format!("{id}#def{i}"), path]).unwrap();
        }
        for i in 0..fanin {
            c.execute("INSERT INTO code.code_edges (from_node, to_node) VALUES (?1, ?2)", rusqlite::params![format!("{id}#src{i}"), id]).unwrap();
        }
        for i in 0..fanout {
            c.execute("INSERT INTO code.code_edges (from_node, to_node) VALUES (?1, ?2)", rusqlite::params![id, format!("{id}#dst{i}")]).unwrap();
        }
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS code; ATTACH ':memory:' AS knowledge; \
          CREATE TABLE code.code_nodes (id TEXT PRIMARY KEY, origin TEXT, path TEXT, type TEXT); \
          CREATE TABLE code.code_edges (from_node TEXT, to_node TEXT); \
          CREATE TABLE knowledge.knowledge_units (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT, identity_key TEXT, title TEXT, authority TEXT, receipts_json TEXT);").unwrap();
        seed_file(&c, "a", "src/a.js", 25, 12, 3);   // god-file only (exactly at threshold)
        seed_file(&c, "b", "src/b.js", 2, 1, 20);     // shotgun-surgery only (exactly at threshold)
        seed_file(&c, "c", "src/c.js", 3, 15, 10);    // hub-dependency only (exactly at threshold)
        seed_file(&c, "d", "src/d.js", 30, 20, 25);   // triggers all three
        seed_file(&c, "e", "other/e.js", 1, 1, 1);    // triggers nothing
        seed_file(&c, "f", "src/f.js", 24, 12, 0);    // defs just under the god-file threshold
        seed_file(&c, "g", "src/g.js", 25, 11, 0);    // fanin just under the god-file threshold
        c.execute("INSERT INTO code.code_nodes (id, origin, path, type) VALUES ('x','bp','src/x.js','file')", []).unwrap();
        let long_quote = format!("{}END", "A ".repeat(90)); // 183 chars — exercises the 160-char slice
        c.execute("INSERT INTO knowledge.knowledge_units (kind, identity_key, title, authority, receipts_json) VALUES ('concept','god object','God Object','Riel',?1)",
            rusqlite::params![json!([{ "quote": long_quote }]).to_string()]).unwrap();
        c.execute("INSERT INTO knowledge.knowledge_units (kind, identity_key, title, authority, receipts_json) VALUES ('concept','stable dependencies principle','Stable Dependencies','Martin','not-json{{{')", []).unwrap();
        c
    }

    #[test]
    fn detects_sorts_and_cites() {
        let c = db();
        let r = instrumentDetectors(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["scanned"], json!(7)); // 'x' excluded: origin != 'atlas'
        let findings = r["findings"].as_array().unwrap();
        let order: Vec<(String, String)> = findings.iter()
            .map(|f| (f["detector"].as_str().unwrap().to_string(), f["path"].as_str().unwrap().to_string()))
            .collect();
        assert_eq!(order, vec![
            ("god-file".to_string(), "src/d.js".to_string()),
            ("hub-dependency".to_string(), "src/d.js".to_string()),
            ("god-file".to_string(), "src/a.js".to_string()),
            ("hub-dependency".to_string(), "src/c.js".to_string()),
            ("shotgun-surgery".to_string(), "src/d.js".to_string()),
            ("shotgun-surgery".to_string(), "src/b.js".to_string()),
        ]);
        // god-file's IR row exists and is well-formed -> 'ir' sourced, quote sliced to 160 chars
        assert_eq!(findings[0]["citation"]["source"], json!("ir"));
        assert_eq!(findings[0]["citation"]["authority"], json!("Riel"));
        assert_eq!(findings[0]["citation"]["quote"].as_str().unwrap().chars().count(), 160);
        // hub-dependency's IR row exists but receipts_json is malformed -> falls back to static
        assert_eq!(findings[1]["citation"]["source"], json!("static"));
        // shotgun-surgery has no matching IR row at all -> static
        assert_eq!(findings[4]["citation"]["source"], json!("static"));
        // f/g never cross the god-file AND boundary (one side of the threshold missed each)
        assert!(!findings.iter().any(|f| f["path"] == json!("src/f.js") || f["path"] == json!("src/g.js")));
        let detectors = r["detectors"].as_array().unwrap();
        assert_eq!(detectors.len(), 3);
        assert_eq!(detectors[0]["key"], json!("god-file"));
        assert_eq!(detectors[0]["threshold"], json!("{\"defs\":25,\"fanin\":12}"));
        assert_eq!(detectors[1]["threshold"], json!("{\"fanout\":20}"));
        assert_eq!(detectors[2]["threshold"], json!("{\"fanin\":15,\"fanout\":10}"));
    }

    #[test]
    fn path_scoping() {
        let c = db();
        let only_other = instrumentDetectors(&json!({ "paths": ["other"] }), &c).unwrap();
        assert_eq!(only_other["scanned"], json!(1));
        assert_eq!(only_other["findings"], json!([]));
        let exact = instrumentDetectors(&json!({ "paths": ["src/d.js"] }), &c).unwrap();
        assert_eq!(exact["scanned"], json!(1));
        assert_eq!(exact["findings"].as_array().unwrap().len(), 3);
        // trailing slash on the prefix is stripped before the startsWith check
        assert_eq!(instrumentDetectors(&json!({ "paths": ["src/"] }), &c).unwrap()["scanned"], json!(6));
        // Array.isArray(true) + .some on an empty array is false -> nothing is in scope
        let none = instrumentDetectors(&json!({ "paths": [] }), &c).unwrap();
        assert_eq!(none["scanned"], json!(0));
        assert_eq!(none["findings"], json!([]));
    }

    #[test]
    fn limit_clamping() {
        let c = db();
        assert_eq!(instrumentDetectors(&json!({ "limit": 1 }), &c).unwrap()["findings"].as_array().unwrap().len(), 1);
        // 0 is falsy in JS -> `|| 20` fallback, NOT zero results
        assert_eq!(instrumentDetectors(&json!({ "limit": 0 }), &c).unwrap()["findings"].as_array().unwrap().len(), 6);
        assert_eq!(instrumentDetectors(&json!({ "limit": 200 }), &c).unwrap()["findings"].as_array().unwrap().len(), 6);
        assert_eq!(instrumentDetectors(&json!({ "limit": -5 }), &c).unwrap()["findings"].as_array().unwrap().len(), 1);
    }
}
