//! knowledge-ir.rs — twin of skills/entities/knowledge-ir.mjs (db WRITE lane, attached `knowledge`
//! db). NAMES ARE CONTRACT: `knowledgeIr` (default). THE IR GATE: put (gate + upsert by kind/
//! identity_key/authority, consumers[] non-empty required else refused — hoarding guard), counts
//! (by kind + edges total), list (filtered rows with body/receipts/consumers JSON-parsed back),
//! edge (upsert unit_edges — ON CONFLICT touches ONLY topic, not receipts_json), dialogue
//! (identity_key collisions across authorities lacking an edge yet).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// = JS String(v)
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
// = JS `${v}`
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
// JS `??` right operand: nz(a).or(nz(b))
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
// = JS Number(v)
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
// = JS Number.isInteger(v) — unlike js_number, does NOT coerce: only a genuine JSON number that
// is integer-valued passes; strings/bools/null/absent all fail. Used for u.year / u.book_id.
fn number_isinteger(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0).map(|f| f as i64),
        _ => None,
    }
}
// Mirrors how a JS Number binds through node:sqlite: safe-integer-valued -> INTEGER, else REAL.
fn bind_number(n: f64) -> Value {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9e15 { json!(n as i64) } else { json!(n) }
}

const KINDS: &[&str] = &["fact", "concept", "heuristic", "trade-off", "doctrine", "question", "method", "example", "metric-link"];
const TYPE_CLASSES: &[&str] = &["definition", "normative", "example", "anecdote", "opinion"];
const CONSUMERS: &[&str] = &["detector", "question", "ontology", "glossary", "rag", "paper", "metric-link", "ultra-review", "law-draft", "skilllib"];
const EDGE_KINDS: &[&str] = &["IS_A", "REQUIRES", "RELATED", "CONFLICTS", "SUPPORTS", "DESCENDS", "REFINES"];

pub fn knowledgeIr(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('knowledge-ir','db') when no db — the db lane always injects one.
    let op = str_qq(input.get("op"));

    if op == "put" {
        let empty_obj = json!({});
        let u = nz(input.get("unit")).unwrap_or(&empty_obj);
        let kind = str_qq(u.get("kind"));
        if !KINDS.contains(&kind.as_str()) {
            return Err(format!("knowledge-ir: unit.kind must be one of: {} (got '{}')", KINDS.join(", "), kind));
        }
        let identity_key: String = str_qq(u.get("identity_key")).trim().to_lowercase().chars().take(200).collect();
        if identity_key.is_empty() {
            return Err("knowledge-ir: unit.identity_key is required".into());
        }
        let title: String = str_qq(u.get("title")).trim().chars().take(240).collect();
        if title.is_empty() {
            return Err("knowledge-ir: unit.title is required".into());
        }
        let authority: String = str_qq(u.get("authority")).trim().chars().take(200).collect();
        if authority.is_empty() {
            return Err("knowledge-ir: unit.authority is required".into());
        }
        // THE CONSUMER GATE — the whole point of the IR.
        let consumers: Vec<Value> = match u.get("consumers").and_then(|v| v.as_array()) {
            Some(arr) => arr.iter().map(js_string).filter(|c| CONSUMERS.contains(&c.as_str())).map(|c| json!(c)).collect(),
            None => Vec::new(),
        };
        if consumers.is_empty() {
            return Err(format!(
                "knowledge-ir: unit.consumers (non-empty, from: {}) — a row that names no consumer is hoarding, not knowledge",
                CONSUMERS.join("|")
            ));
        }
        let type_class = {
            let tc = tmpl(u.get("type_class"));
            if TYPE_CLASSES.contains(&tc.as_str()) { tc } else { "definition".to_string() }
        };
        let receipts: Vec<Value> = match u.get("receipts").and_then(|v| v.as_array()) {
            Some(arr) => arr.iter().take(6).cloned().collect(),
            None => Vec::new(),
        };
        let body = nz(u.get("body")).cloned().unwrap_or(json!({}));
        let edition: Value = match nz(u.get("edition")) {
            Some(v) => json!(js_string(v).chars().take(60).collect::<String>()),
            None => Value::Null,
        };
        let year: Value = number_isinteger(u.get("year")).map(|n| json!(n)).unwrap_or(Value::Null);
        let applies_to: Vec<Value> = match u.get("applies_to").and_then(|v| v.as_array()) {
            Some(arr) => arr.iter().take(12).cloned().collect(),
            None => Vec::new(),
        };
        let exceptions: Vec<Value> = match u.get("exceptions").and_then(|v| v.as_array()) {
            Some(arr) => arr.iter().take(8).cloned().collect(),
            None => Vec::new(),
        };
        let book_id: Value = number_isinteger(u.get("book_id")).map(|n| json!(n)).unwrap_or(Value::Null);
        let verdict = if u.get("verdict").and_then(|v| v.as_str()) == Some("unverified") { "unverified" } else { "kept" };

        crate::db_json::run_write(
            db,
            "INSERT INTO knowledge.knowledge_units (kind, identity_key, title, body_json, receipts_json, type_class, authority, edition, year, applies_to, exceptions_json, consumers_json, book_id, verdict)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(kind, identity_key, authority) DO UPDATE SET
               title = excluded.title, body_json = excluded.body_json, receipts_json = excluded.receipts_json,
               type_class = excluded.type_class, exceptions_json = excluded.exceptions_json,
               consumers_json = excluded.consumers_json, verdict = excluded.verdict",
            &[
                json!(kind.clone()),
                json!(identity_key.clone()),
                json!(title),
                json!(serde_json::to_string(&body).unwrap()),
                json!(serde_json::to_string(&receipts).unwrap()),
                json!(type_class),
                json!(authority.clone()),
                edition,
                year,
                json!(serde_json::to_string(&applies_to).unwrap()),
                json!(serde_json::to_string(&exceptions).unwrap()),
                json!(serde_json::to_string(&consumers).unwrap()),
                book_id,
                json!(verdict),
            ],
        )?;
        let row = crate::db_json::query_rows(
            db,
            "SELECT id FROM knowledge.knowledge_units WHERE kind = ? AND identity_key = ? AND authority = ?",
            &[json!(kind), json!(identity_key), json!(authority)],
        )?;
        let unit_id = row.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")).cloned().unwrap_or(Value::Null);
        return Ok(json!({ "ok": true, "unit_id": unit_id }));
    }

    if op == "counts" {
        let mut where_parts: Vec<&str> = Vec::new();
        let mut args: Vec<Value> = Vec::new();
        if nz(input.get("book_id")).is_some() {
            where_parts.push("book_id = ?");
            args.push(bind_number(js_number(input.get("book_id"))));
        }
        if truthy(input.get("authority")) {
            where_parts.push("authority = ?");
            args.push(json!(js_string(input.get("authority").unwrap())));
        }
        let w = if where_parts.is_empty() { String::new() } else { format!("WHERE {}", where_parts.join(" AND ")) };
        let rows = crate::db_json::query_rows(db, &format!("SELECT kind, COUNT(*) AS n FROM knowledge.knowledge_units {w} GROUP BY kind"), &args)?;
        let edges_row = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM knowledge.unit_edges", &[])?;
        let edges = edges_row.as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).cloned().unwrap_or(json!(0));
        let mut by_kind = Map::new();
        let mut total: i64 = 0;
        if let Some(arr) = rows.as_array() {
            for r in arr {
                let key = r.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let n = r.get("n").cloned().unwrap_or(json!(0));
                total += n.as_i64().unwrap_or(0);
                by_kind.insert(key, n);
            }
        }
        return Ok(json!({ "ok": true, "by_kind": Value::Object(by_kind), "total": total, "edges": edges }));
    }

    if op == "list" {
        let mut where_parts: Vec<&str> = Vec::new();
        let mut args: Vec<Value> = Vec::new();
        if truthy(input.get("kind")) {
            where_parts.push("kind = ?");
            args.push(json!(js_string(input.get("kind").unwrap())));
        }
        if nz(input.get("book_id")).is_some() {
            where_parts.push("book_id = ?");
            args.push(bind_number(js_number(input.get("book_id"))));
        }
        if truthy(input.get("identity_key")) {
            where_parts.push("identity_key = ?");
            args.push(json!(js_string(input.get("identity_key").unwrap()).to_lowercase()));
        }
        let w = if where_parts.is_empty() { String::new() } else { format!("WHERE {}", where_parts.join(" AND ")) };
        // destructuring default (?? 100) fires only when the key is ABSENT/null; `|| 100` then
        // covers the remaining falsy numeric cases (0 / NaN).
        let raw_limit = nz(input.get("limit")).cloned().unwrap_or(json!(100));
        let num = js_number(Some(&raw_limit));
        let num2 = if num != 0.0 && !num.is_nan() { num } else { 100.0 };
        let lim = num2.max(1.0).min(500.0);
        let mut all_args = args.clone();
        all_args.push(bind_number(lim));
        let rows = crate::db_json::query_rows(db, &format!("SELECT * FROM knowledge.knowledge_units {w} ORDER BY id DESC LIMIT ?"), &all_args)?;
        let units: Vec<Value> = rows
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|r| {
                let mut obj = r.as_object().cloned().unwrap_or_default();
                // JS `r.body_json || '{}'` etc — falsy (incl. empty string), not just nullish.
                let body_str = if truthy(obj.get("body_json")) { obj.get("body_json").and_then(|v| v.as_str()).unwrap_or("{}").to_string() } else { "{}".to_string() };
                let receipts_str = if truthy(obj.get("receipts_json")) { obj.get("receipts_json").and_then(|v| v.as_str()).unwrap_or("[]").to_string() } else { "[]".to_string() };
                let consumers_str = if truthy(obj.get("consumers_json")) { obj.get("consumers_json").and_then(|v| v.as_str()).unwrap_or("[]").to_string() } else { "[]".to_string() };
                let body: Value = serde_json::from_str(&body_str).unwrap_or(Value::Null);
                let receipts: Value = serde_json::from_str(&receipts_str).unwrap_or(Value::Null);
                let consumers: Value = serde_json::from_str(&consumers_str).unwrap_or(Value::Null);
                obj.insert("body".into(), body);
                obj.insert("receipts".into(), receipts);
                obj.insert("consumers".into(), consumers);
                Value::Object(obj)
            })
            .collect();
        return Ok(json!({ "ok": true, "units": units }));
    }

    if op == "edge" {
        let kind = str_qq(input.get("kind"));
        if !EDGE_KINDS.contains(&kind.as_str()) {
            return Err(format!("knowledge-ir: kind must be one of: {} (got '{}')", EDGE_KINDS.join(", "), kind));
        }
        let a_f = js_number(input.get("from_unit"));
        let b_f = js_number(input.get("to_unit"));
        let is_int = |f: f64| f.is_finite() && f.fract() == 0.0;
        if !is_int(a_f) || !is_int(b_f) || a_f == b_f {
            return Err("knowledge-ir: from_unit/to_unit (distinct unit ids) is required".into());
        }
        let topic: Value = match nz(input.get("topic")) {
            Some(v) => json!(js_string(v).chars().take(200).collect::<String>()),
            None => Value::Null,
        };
        let receipts: Vec<Value> = match input.get("receipts").and_then(|v| v.as_array()) {
            Some(arr) => arr.iter().take(4).cloned().collect(),
            None => Vec::new(),
        };
        crate::db_json::run_write(
            db,
            "INSERT INTO knowledge.unit_edges (from_unit, to_unit, kind, topic, receipts_json)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(from_unit, to_unit, kind) DO UPDATE SET topic = excluded.topic",
            &[json!(a_f as i64), json!(b_f as i64), json!(kind), topic, json!(serde_json::to_string(&receipts).unwrap())],
        )?;
        return Ok(json!({ "ok": true }));
    }

    if op == "dialogue" {
        let raw_limit = nz(input.get("limit")).cloned().unwrap_or(json!(20));
        let num = js_number(Some(&raw_limit));
        let num2 = if num != 0.0 && !num.is_nan() { num } else { 20.0 };
        let lim = num2.max(1.0).min(100.0);
        let pairs = crate::db_json::query_rows(
            db,
            "SELECT a.id AS from_unit, b.id AS to_unit, a.identity_key, a.kind,
                    a.authority AS from_authority, b.authority AS to_authority,
                    a.title AS from_title, b.title AS to_title
             FROM knowledge.knowledge_units a
             JOIN knowledge.knowledge_units b
               ON a.identity_key = b.identity_key AND a.kind = b.kind AND a.authority < b.authority
             WHERE NOT EXISTS (
               SELECT 1 FROM knowledge.unit_edges e
               WHERE (e.from_unit = a.id AND e.to_unit = b.id) OR (e.from_unit = b.id AND e.to_unit = a.id)
             )
             LIMIT ?",
            &[bind_number(lim)],
        )?;
        return Ok(json!({ "ok": true, "pairs": pairs }));
    }

    Err("knowledge-ir: op 'put'|'counts'|'list'|'edge'|'dialogue' is required".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS knowledge;
            CREATE TABLE knowledge.knowledge_units (
              id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, identity_key TEXT NOT NULL,
              title TEXT NOT NULL, body_json TEXT NOT NULL DEFAULT '{}', receipts_json TEXT NOT NULL DEFAULT '[]',
              type_class TEXT NOT NULL DEFAULT 'definition', authority TEXT NOT NULL, edition TEXT, year INTEGER,
              applies_to TEXT NOT NULL DEFAULT '[]', exceptions_json TEXT NOT NULL DEFAULT '[]',
              consumers_json TEXT NOT NULL, book_id INTEGER, verdict TEXT NOT NULL DEFAULT 'kept',
              created_at TEXT, UNIQUE (kind, identity_key, authority));
            CREATE TABLE knowledge.unit_edges (
              id INTEGER PRIMARY KEY AUTOINCREMENT, from_unit INTEGER NOT NULL, to_unit INTEGER NOT NULL,
              kind TEXT NOT NULL, topic TEXT, receipts_json TEXT NOT NULL DEFAULT '[]', created_at TEXT,
              UNIQUE (from_unit, to_unit, kind));",
        )
        .unwrap();
        c
    }

    #[test]
    fn put_normalizes_and_upserts() {
        let c = db();
        let applies_to: Vec<Value> = (0..13).map(|i| json!(format!("t{i}"))).collect();
        let unit = json!({ "kind": "concept", "identity_key": "  Some KEY  ", "title": "  A Title  ", "authority": "  The Authority  ", "consumers": ["detector", "bogus", "rag"], "receipts": [1,2,3,4,5,6,7,8], "applies_to": applies_to, "book_id": 42, "verdict": "unverified" });
        let r = knowledgeIr(&json!({ "op": "put", "unit": unit }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let id = r["unit_id"].as_i64().unwrap();
        let row = crate::db_json::query_rows(&c, "SELECT * FROM knowledge.knowledge_units WHERE id = ?", &[json!(id)]).unwrap();
        let row0 = &row[0];
        assert_eq!(row0["identity_key"], json!("some key"));
        assert_eq!(row0["title"], json!("A Title"));
        assert_eq!(serde_json::from_str::<Value>(row0["consumers_json"].as_str().unwrap()).unwrap(), json!(["detector", "rag"]));
        assert_eq!(serde_json::from_str::<Value>(row0["receipts_json"].as_str().unwrap()).unwrap().as_array().unwrap().len(), 6);
        assert_eq!(serde_json::from_str::<Value>(row0["applies_to"].as_str().unwrap()).unwrap().as_array().unwrap().len(), 12);
        assert_eq!(row0["book_id"], json!(42));
        assert_eq!(row0["verdict"], json!("unverified"));

        // conflict path: same (kind, identity_key, authority) — updates a subset, keeps id
        let r2 = knowledgeIr(
            &json!({ "op": "put", "unit": { "kind": "concept", "identity_key": "some key", "title": "New", "authority": "The Authority", "consumers": ["glossary"] } }),
            &c,
        )
        .unwrap();
        assert_eq!(r2["unit_id"], json!(id));
        let row = crate::db_json::query_rows(&c, "SELECT * FROM knowledge.knowledge_units WHERE id = ?", &[json!(id)]).unwrap();
        assert_eq!(row[0]["title"], json!("New"));
        assert_eq!(row[0]["book_id"], json!(42)); // NOT in the DO UPDATE SET — stays
    }

    #[test]
    fn counts_list_edge_dialogue() {
        let c = db();
        c.execute_batch(
            "INSERT INTO knowledge.knowledge_units (id, kind, identity_key, title, authority, consumers_json, book_id) VALUES
              (1, 'fact', 'shared', 'T-A', 'AuthorityA', '[\"rag\"]', 1),
              (2, 'fact', 'shared', 'T-B', 'AuthorityB', '[\"rag\"]', 2),
              (3, 'fact', 'shared', 'T-C', 'AuthorityC', '[\"rag\"]', 1);
             INSERT INTO knowledge.unit_edges (from_unit, to_unit, kind) VALUES (2, 3, 'RELATED');",
        )
        .unwrap();

        let counts = knowledgeIr(&json!({ "op": "counts" }), &c).unwrap();
        assert_eq!(counts["by_kind"], json!({ "fact": 3 }));
        assert_eq!(counts["total"], json!(3));
        assert_eq!(counts["edges"], json!(1));
        let filtered = knowledgeIr(&json!({ "op": "counts", "book_id": 1 }), &c).unwrap();
        assert_eq!(filtered["total"], json!(2));

        let list = knowledgeIr(&json!({ "op": "list" }), &c).unwrap();
        let ids: Vec<i64> = list["units"].as_array().unwrap().iter().map(|u| u["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![3, 2, 1]);
        let by_identity = knowledgeIr(&json!({ "op": "list", "identity_key": "SHARED" }), &c).unwrap();
        assert_eq!(by_identity["units"].as_array().unwrap().len(), 3);

        let e = knowledgeIr(&json!({ "op": "edge", "kind": "RELATED", "from_unit": 1, "to_unit": 2, "topic": "t1", "receipts": ["orig"] }), &c).unwrap();
        assert_eq!(e["ok"], json!(true));
        knowledgeIr(&json!({ "op": "edge", "kind": "RELATED", "from_unit": 1, "to_unit": 2, "topic": "t2", "receipts": ["new"] }), &c).unwrap();
        let edge_row = crate::db_json::query_rows(&c, "SELECT * FROM knowledge.unit_edges WHERE from_unit=1 AND to_unit=2 AND kind='RELATED'", &[]).unwrap();
        assert_eq!(edge_row[0]["topic"], json!("t2"));
        assert_eq!(edge_row[0]["receipts_json"], json!("[\"orig\"]")); // unchanged — not in DO UPDATE SET

        let dialogue = knowledgeIr(&json!({ "op": "dialogue" }), &c).unwrap();
        let pairs: Vec<(i64, i64)> = dialogue["pairs"].as_array().unwrap().iter().map(|p| (p["from_unit"].as_i64().unwrap(), p["to_unit"].as_i64().unwrap())).collect();
        assert!(pairs.contains(&(1, 3))); // no edge yet
        assert!(!pairs.contains(&(2, 3))); // edge exists
    }
}
