//! paper-store.rs — twin of skills/agents/in/paper-store.mjs (db WRITE+READ lane, cross-db via
//! ATTACH knowledge). NAMES ARE CONTRACT: `paperStore` (default). The PAPER CORPUS: `press`
//! registers a pressed paper idempotently by cite_id (ON CONFLICT updates title + findings_json
//! ONLY — kind/subject/snapshot_id keep the first edition's values); `list` answers the shelf
//! newest-first (optional subject filter; a falsy subject means unfiltered); `related` answers
//! the literature around a subject — prior editions, papers on the provided related dirs
//! (deduped, capped at 12, self excluded), and the newest of the rest. Deterministic; no AI.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS String(v) — dirs.map(String): null → "null", numbers/bools stringified
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
// JS String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

pub fn paperStore(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS destructures { db } from context and throws err.contextMissing without it — the harness
    // context always carries db, so that guard cannot fire here.
    let op = match nz(input.get("op")) { Some(v) => js_string(v), None => "list".into() };

    if op == "press" {
        let cite = str_qq(input.get("cite_id")).trim().to_string();
        let subject = str_qq(input.get("subject")).trim().to_string();
        if cite.is_empty() { return Err("paper-store: cite_id is required".into()); }
        if subject.is_empty() { return Err("paper-store: subject is required".into()); }
        let kind = match nz(input.get("kind")) { Some(v) => js_string(v), None => "folder".into() };
        let snapshot_id = nz(input.get("snapshot_id")).cloned().unwrap_or(Value::Null);
        // String(input?.title ?? subject).slice(0, 200)
        let title: String = (match nz(input.get("title")) { Some(v) => js_string(v), None => subject.clone() })
            .chars().take(200).collect();
        // JSON.stringify(input?.findings ?? {}).slice(0, 4000)
        let findings_v = nz(input.get("findings")).cloned().unwrap_or_else(|| json!({}));
        let findings: String = serde_json::to_string(&findings_v).map_err(|e| e.to_string())?.chars().take(4000).collect();
        crate::db_json::run_write(
            db,
            "\n      INSERT INTO knowledge.papers (cite_id, kind, subject, snapshot_id, title, findings_json)\n      VALUES (?, ?, ?, ?, ?, ?)\n      ON CONFLICT(cite_id) DO UPDATE SET title = excluded.title, findings_json = excluded.findings_json\n    ",
            &[json!(cite), json!(kind), json!(subject), snapshot_id, json!(title), json!(findings)],
        )?;
        let rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.papers WHERE cite_id = ?", &[json!(cite)])?;
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        // node:sqlite .get(): a missing row is `undefined` → JSON.stringify omits the key
        if let Some(p) = rows.as_array().and_then(|a| a.first()) { out.insert("paper".into(), p.clone()); }
        return Ok(Value::Object(out));
    }

    if op == "list" {
        // JS: input?.subject != null ? String(input.subject) : null — then TRUTHY gates the filter
        let subject: Option<String> = match input.get("subject") { None | Some(Value::Null) => None, Some(v) => Some(js_string(v)) };
        let rows = match subject.as_deref() {
            Some(s) if !s.is_empty() => crate::db_json::query_rows(db, "SELECT * FROM knowledge.papers WHERE subject = ? ORDER BY id DESC LIMIT 30", &[json!(s)])?,
            _ => crate::db_json::query_rows(db, "SELECT * FROM knowledge.papers ORDER BY id DESC LIMIT 30", &[])?,
        };
        return Ok(json!({ "ok": true, "papers": rows }));
    }

    if op == "related" {
        let subject = str_qq(input.get("subject")).trim().to_string();
        if subject.is_empty() { return Err("paper-store: subject is required".into()); }
        // [...new Set(input.dirs.map(String).filter(Boolean))].slice(0, 12) — dedupe keeps FIRST
        // occurrence order; the cap applies AFTER dedupe
        let mut dirs: Vec<String> = Vec::new();
        if let Some(arr) = input.get("dirs").and_then(|v| v.as_array()) {
            let mut seen = std::collections::HashSet::new();
            for d in arr {
                let s = js_string(d);
                if s.is_empty() { continue; }
                if seen.insert(s.clone()) { dirs.push(s); }
            }
            dirs.truncate(12);
        }
        let editions = crate::db_json::query_rows(db, "SELECT * FROM knowledge.papers WHERE subject = ? ORDER BY id DESC LIMIT 6", &[json!(subject)])?;
        let mut neighbors = json!([]);
        if !dirs.is_empty() {
            let qs = vec!["?"; dirs.len()].join(",");
            let mut params: Vec<Value> = dirs.iter().map(|d| json!(d)).collect();
            params.push(json!(subject));
            neighbors = crate::db_json::query_rows(
                db,
                &format!("SELECT * FROM knowledge.papers WHERE subject IN ({qs}) AND subject != ? GROUP BY subject ORDER BY id DESC LIMIT 12"),
                &params,
            )?;
        }
        let latest = crate::db_json::query_rows(db, "SELECT * FROM knowledge.papers WHERE subject != ? GROUP BY subject ORDER BY id DESC LIMIT 6", &[json!(subject)])?;
        return Ok(json!({ "ok": true, "editions": editions, "neighbors": neighbors, "latest": latest }));
    }

    Err("paper-store: op 'press'|'list'|'related' is required".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS knowledge;
            CREATE TABLE knowledge.papers (id INTEGER PRIMARY KEY, cite_id TEXT NOT NULL UNIQUE, kind TEXT NOT NULL DEFAULT 'folder', subject TEXT NOT NULL, snapshot_id INTEGER, title TEXT NOT NULL, findings_json TEXT NOT NULL DEFAULT '{}', created_at TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn press_minimal_defaults_and_trim() {
        let c = fresh();
        let r = paperStore(&json!({ "op": "press", "cite_id": "P-CORE-1", "subject": "src/core" }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "ok": true, "paper": { "id": 1, "cite_id": "P-CORE-1", "kind": "folder", "subject": "src/core", "snapshot_id": null, "title": "src/core", "findings_json": "{}", "created_at": null } })
        );
        // trims + full fields + findings serialized compact in insertion order
        let r2 = paperStore(&json!({ "op": "press", "cite_id": "  P-UI-1  ", "subject": "  src/ui  ", "kind": "file", "snapshot_id": 7, "title": "The UI Literature", "findings": { "grade": "A", "projections": [1, 2] } }), &c).unwrap();
        assert_eq!(r2["paper"]["cite_id"], json!("P-UI-1"));
        assert_eq!(r2["paper"]["subject"], json!("src/ui"));
        assert_eq!(r2["paper"]["snapshot_id"], json!(7));
        assert_eq!(r2["paper"]["findings_json"], json!("{\"grade\":\"A\",\"projections\":[1,2]}"));
    }

    #[test]
    fn press_upsert_updates_only_title_and_findings() {
        let c = fresh();
        c.execute_batch("INSERT INTO knowledge.papers (id, cite_id, kind, subject, snapshot_id, title, findings_json, created_at) VALUES (1,'P-CORE-1','folder','src/core',NULL,'old title','{}','2025-12-01 10:00:00');").unwrap();
        let r = paperStore(&json!({ "op": "press", "cite_id": "P-CORE-1", "kind": "snapshot", "subject": "src/elsewhere", "snapshot_id": 9, "title": "new title", "findings": { "v": 2 } }), &c).unwrap();
        let p = &r["paper"];
        assert_eq!(p["title"], json!("new title"));
        assert_eq!(p["findings_json"], json!("{\"v\":2}"));
        assert_eq!(p["kind"], json!("folder")); // NOT updated by the upsert
        assert_eq!(p["subject"], json!("src/core")); // NOT updated
        assert_eq!(p["snapshot_id"], Value::Null); // NOT updated
        assert_eq!(p["created_at"], json!("2025-12-01 10:00:00")); // preserved
        let n: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.papers", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn press_slices_title_to_200_chars() {
        let c = fresh();
        let long: String = "x".repeat(210);
        let r = paperStore(&json!({ "op": "press", "cite_id": "P-LONG", "subject": "src/long", "title": long }), &c).unwrap();
        assert_eq!(r["paper"]["title"].as_str().unwrap().len(), 200);
    }

    #[test]
    fn list_defaults_filters_and_falsy_subject() {
        let c = fresh();
        c.execute_batch("INSERT INTO knowledge.papers (id, cite_id, kind, subject, snapshot_id, title, findings_json, created_at) VALUES
            (1,'a1','folder','src/core',NULL,'core v1','{}',NULL),
            (2,'a2','folder','src/core',NULL,'core v2','{}',NULL),
            (3,'b1','folder','src/ui',NULL,'ui v1','{}',NULL),
            (4,'n1','folder','42',NULL,'answer','{}',NULL);").unwrap();
        // absent op → list; absent subject → all, id DESC
        let all = paperStore(&json!({}), &c).unwrap();
        let ids: Vec<i64> = all["papers"].as_array().unwrap().iter().map(|p| p["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![4, 3, 2, 1]);
        // subject filter
        let core = paperStore(&json!({ "op": "list", "subject": "src/core" }), &c).unwrap();
        assert_eq!(core["papers"].as_array().unwrap().len(), 2);
        // empty-string subject is FALSY → unfiltered
        let falsy = paperStore(&json!({ "op": "list", "subject": "" }), &c).unwrap();
        assert_eq!(falsy["papers"].as_array().unwrap().len(), 4);
        // numeric subject → String(42) = "42"
        let num = paperStore(&json!({ "op": "list", "subject": 42 }), &c).unwrap();
        assert_eq!(num["papers"].as_array().unwrap()[0]["cite_id"], json!("n1"));
    }

    #[test]
    fn related_editions_neighbors_latest() {
        let c = fresh();
        c.execute_batch("INSERT INTO knowledge.papers (id, cite_id, kind, subject, snapshot_id, title, findings_json, created_at) VALUES
            (1,'c1','folder','src/core',NULL,'core e1','{}',NULL),
            (2,'c2','folder','src/core',NULL,'core e2','{}',NULL),
            (3,'c3','folder','src/core',NULL,'core e3','{}',NULL),
            (4,'c4','folder','src/core',NULL,'core e4','{}',NULL),
            (5,'c5','folder','src/core',NULL,'core e5','{}',NULL),
            (6,'c6','folder','src/core',NULL,'core e6','{}',NULL),
            (7,'c7','folder','src/core',NULL,'core e7','{}',NULL),
            (8,'u1','folder','src/ui',NULL,'ui paper','{}',NULL),
            (9,'d1','folder','src/db',NULL,'db paper','{}',NULL),
            (10,'p1','folder','src/api',NULL,'api paper','{}',NULL);").unwrap();
        // dirs: dupes deduped, '' filtered, self excluded by subject != ?
        let r = paperStore(&json!({ "op": "related", "subject": "src/core", "dirs": ["src/ui", "src/ui", "", "src/db", "src/core"] }), &c).unwrap();
        let ed: Vec<i64> = r["editions"].as_array().unwrap().iter().map(|p| p["id"].as_i64().unwrap()).collect();
        assert_eq!(ed, vec![7, 6, 5, 4, 3, 2]); // LIMIT 6, newest first
        let nb: Vec<i64> = r["neighbors"].as_array().unwrap().iter().map(|p| p["id"].as_i64().unwrap()).collect();
        assert_eq!(nb, vec![9, 8]); // ui + db, self excluded
        let lt: Vec<i64> = r["latest"].as_array().unwrap().iter().map(|p| p["id"].as_i64().unwrap()).collect();
        assert_eq!(lt, vec![10, 9, 8]); // other subjects, one per subject
        // no dirs → neighbors []; non-array dirs → neighbors []
        let r2 = paperStore(&json!({ "op": "related", "subject": "src/ui" }), &c).unwrap();
        assert_eq!(r2["neighbors"], json!([]));
        let r3 = paperStore(&json!({ "op": "related", "subject": "src/ui", "dirs": "src/core" }), &c).unwrap();
        assert_eq!(r3["neighbors"], json!([]));
    }

    #[test]
    fn guards_reject() {
        let c = fresh();
        assert!(paperStore(&json!({ "op": "press", "subject": "s" }), &c).unwrap_err().contains("cite_id is required"));
        assert!(paperStore(&json!({ "op": "press", "cite_id": "x" }), &c).unwrap_err().contains("subject is required"));
        assert!(paperStore(&json!({ "op": "related" }), &c).unwrap_err().contains("subject is required"));
        assert!(paperStore(&json!({ "op": "nope" }), &c).unwrap_err().contains("op 'press'|'list'|'related'"));
    }
}
