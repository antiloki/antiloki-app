//! research-find.rs — twin of skills/system/research-find.mjs (db lane; cross-db via ATTACH to
//! judgments.sqlite, plus a call into the constitution-paper twin for op=paper). NAMES ARE
//! CONTRACT: `researchFind` (default). THE RESEARCH KNOWLEDGE SURFACE (operator 2026-07-20):
//! one op-routed, read-only query over what the research stack has PUBLISHED — papers (the
//! shelf, judgments.review_papers), concepts (the atlas, judgments.ultra_concepts), evidence
//! (kept bugfinder findings, judgments.bugfinder_findings — dropped ones never surface).
//! Deterministic, $0, no writes. Missing/empty judgment tables degrade to empty lists, never
//! throw — a workspace that never ran a mission is a valid state.
use rusqlite::Connection;
use serde_json::{json, Value};

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand: nz(a).or(nz(b))
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// JS `const lim = (v, d = 25) => Math.max(1, Math.min(100, Number(v ?? d) || d));` — v??d resolves
// first (null/absent -> d), THEN Number(...) is taken; if that's falsy (0 or NaN) it falls to d
// directly (not through Number() again), only then clamped into [1,100].
fn lim(v: Option<&Value>, d: f64) -> f64 {
    let base = match nz(v) { Some(x) => js_number(Some(x)), None => d };
    let n = if base == 0.0 || base.is_nan() { d } else { base };
    n.max(1.0).min(100.0)
}

pub fn researchFind(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const db = context?.db; if (!db) throw err.contextMissing(...)` — this harness's db
    // lane always supplies context.db, so that guard never fires here (kept only as a doc note).
    let op = nz(input.get("op")).map(js_string).unwrap_or_else(|| "papers".into());
    let q = nz(input.get("query")).map(js_string).unwrap_or_default();
    let q = q.trim().to_string();
    let like = format!("%{q}%");

    if op == "papers" {
        let limit = lim(input.get("limit"), 25.0);
        let rows = if !q.is_empty() {
            crate::db_json::query_rows(db, "SELECT id, slug, title, label, discovery, cost_usd, created_at, updated_at FROM judgments.review_papers
                    WHERE title LIKE ? OR COALESCE(discovery,'') LIKE ? OR COALESCE(label,'') LIKE ?
                    ORDER BY id DESC LIMIT ?", &[json!(like.clone()), json!(like.clone()), json!(like.clone()), json!(limit as i64)])
        } else {
            crate::db_json::query_rows(db, "SELECT id, slug, title, label, discovery, cost_usd, created_at, updated_at FROM judgments.review_papers
                    ORDER BY id DESC LIMIT ?", &[json!(limit as i64)])
        }.unwrap_or_else(|_| json!([]));
        return Ok(json!({ "ok": true, "papers": rows }));
    }

    if op == "paper" {
        let args = json!({
            "op": "get",
            "id": input.get("id").cloned().unwrap_or(Value::Null),
            "slug": input.get("slug").cloned().unwrap_or(Value::Null),
        });
        return crate::constitution_constitution_paper::constitutionPaper(&args, db);
    }

    if op == "concepts" {
        let kind_str = input.get("kind").and_then(|v| v.as_str());
        let kind: Value = match kind_str {
            Some(k) if ["domain", "mechanism", "pattern"].contains(&k) => json!(k),
            _ => Value::Null,
        };
        let limit = lim(input.get("limit"), 25.0);
        let rows = crate::db_json::query_rows(db, "
      SELECT name, kind, COUNT(DISTINCT path) AS files, GROUP_CONCAT(DISTINCT path) AS paths, MAX(created_at) AS last_seen
      FROM judgments.ultra_concepts
      WHERE (? = '' OR name LIKE ?) AND (? IS NULL OR kind = ?)
      GROUP BY name, kind ORDER BY files DESC, name ASC LIMIT ?
    ", &[json!(q.clone()), json!(like.clone()), kind.clone(), kind.clone(), json!(limit as i64)]).unwrap_or_else(|_| json!([]));
        let concepts: Vec<Value> = rows.as_array().cloned().unwrap_or_default().into_iter().map(|r| {
            // `{ ...r, paths: ... }` — paths is already a key in r (from the SQL column list), so
            // re-assigning it keeps its ORIGINAL position (JS object key order is set on first
            // insertion, not on reassignment); serde_json::Map::insert mirrors that.
            let mut obj = r.as_object().cloned().unwrap_or_default();
            let paths_str = str_qq(obj.get("paths"));
            let paths: Vec<Value> = paths_str.split(',').filter(|s| !s.is_empty()).take(20).map(|s| json!(s)).collect();
            obj.insert("paths".into(), json!(paths));
            Value::Object(obj)
        }).collect();
        return Ok(json!({ "ok": true, "concepts": concepts }));
    }

    if op == "evidence" {
        let category: Value = if input.get("category").map_or(false, |v| !v.is_null()) { json!(js_string(input.get("category").unwrap())) } else { Value::Null };
        let file: Value = if input.get("file").map_or(false, |v| !v.is_null()) { json!(js_string(input.get("file").unwrap())) } else { Value::Null };
        let file_like: Value = match &file { Value::String(f) => json!(format!("%{f}%")), _ => Value::Null };
        let limit = lim(input.get("limit"), 25.0);
        let rows = crate::db_json::query_rows(db, "
      SELECT id, run_id, wave, category, severity, file, line, detail, evidence, verified, operator_class, created_at
      FROM judgments.bugfinder_findings
      WHERE dropped = 0
        AND (? = '' OR detail LIKE ? OR COALESCE(file,'') LIKE ?)
        AND (? IS NULL OR category = ?)
        AND (? IS NULL OR file LIKE ?)
      ORDER BY id DESC LIMIT ?
    ", &[json!(q.clone()), json!(like.clone()), json!(like.clone()), category.clone(), category.clone(), file.clone(), file_like, json!(limit as i64)]).unwrap_or_else(|_| json!([]));
        return Ok(json!({ "ok": true, "findings": rows }));
    }

    Ok(json!({ "ok": false, "error": format!("unknown op: {op} — use papers|paper|concepts|evidence") }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments;
          CREATE TABLE judgments.review_papers (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE, title TEXT NOT NULL, label TEXT, discovery TEXT, swarm_id TEXT, snapshot_id INTEGER, files_json TEXT NOT NULL DEFAULT '[]', agents_json TEXT NOT NULL DEFAULT '[]', paper_json TEXT NOT NULL, markdown_md TEXT, cost_usd REAL NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', updated_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
          CREATE TABLE judgments.ultra_concepts (id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL, name TEXT NOT NULL, kind TEXT NOT NULL DEFAULT 'mechanism', created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
          CREATE TABLE judgments.bugfinder_findings (id INTEGER PRIMARY KEY AUTOINCREMENT, run_id INTEGER NOT NULL, wave INTEGER NOT NULL, category TEXT NOT NULL, severity TEXT NOT NULL DEFAULT 'medium', file TEXT, line INTEGER, detail TEXT NOT NULL, evidence TEXT, verified INTEGER NOT NULL DEFAULT 0, dropped INTEGER NOT NULL DEFAULT 0, operator_class TEXT, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');").unwrap();
        c
    }

    #[test]
    fn papers_query_default_op_and_no_match() {
        let c = db();
        c.execute_batch("INSERT INTO judgments.review_papers (id, slug, title, discovery, paper_json) VALUES
          (1, 'paper-auth', 'Auth audit', 'the login flow', '{}'),
          (2, 'paper-billing', 'Billing audit', 'invoices', '{}');").unwrap();

        let r = researchFind(&json!({ "op": "papers", "query": "auth" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["papers"].as_array().unwrap().len(), 1);
        assert_eq!(r["papers"][0]["slug"], json!("paper-auth"));

        // op absent -> defaults to 'papers'; no query -> unfiltered, DESC order
        let r2 = researchFind(&json!({}), &c).unwrap();
        let ids: Vec<i64> = r2["papers"].as_array().unwrap().iter().map(|p| p["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, vec![2, 1]);

        assert_eq!(researchFind(&json!({ "op": "papers", "query": "zzz-no-match" }), &c).unwrap()["papers"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn paper_delegates_to_constitution_paper_get() {
        let c = db();
        c.execute("INSERT INTO judgments.review_papers (id, slug, title, markdown_md, paper_json) VALUES (1, 'paper-auth', 'Auth audit', '# Auth audit\n\nFull study.', '{}')", []).unwrap();

        let by_slug = researchFind(&json!({ "op": "paper", "slug": "paper-auth" }), &c).unwrap();
        assert_eq!(by_slug["ok"], json!(true));
        assert!(by_slug["paper"]["markdown"].as_str().unwrap().contains("Full study"));

        let by_id = researchFind(&json!({ "op": "paper", "id": 1 }), &c).unwrap();
        assert_eq!(by_id["paper"]["slug"], json!("paper-auth"));

        let missing = researchFind(&json!({ "op": "paper", "id": 999 }), &c).unwrap();
        assert_eq!(missing, json!({ "ok": false, "error": "no such paper" }));
    }

    #[test]
    fn concepts_query_kind_filter_and_aggregation() {
        let c = db();
        c.execute_batch("INSERT INTO judgments.ultra_concepts (path, name, kind) VALUES
          ('src/a.mjs', 'audit chain', 'mechanism'),
          ('src/b.mjs', 'audit chain', 'mechanism'),
          ('src/c.mjs', 'token bucket', 'pattern');").unwrap();

        let r = researchFind(&json!({ "op": "concepts", "query": "audit" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let cs = r["concepts"].as_array().unwrap();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0]["files"], json!(2), "the concept aggregates its carrier files");
        let mut paths: Vec<String> = cs[0]["paths"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect();
        paths.sort();
        assert_eq!(paths, vec!["src/a.mjs".to_string(), "src/b.mjs".to_string()]);

        let by_kind = researchFind(&json!({ "op": "concepts", "kind": "pattern" }), &c).unwrap();
        assert_eq!(by_kind["concepts"][0]["name"], json!("token bucket"));

        // an unrecognized kind value is IGNORED (not one of domain|mechanism|pattern) -> no filter
        let bad_kind = researchFind(&json!({ "op": "concepts", "kind": "invalid" }), &c).unwrap();
        assert_eq!(bad_kind["concepts"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn evidence_excludes_dropped_and_filters_by_category_file_query() {
        let c = db();
        c.execute_batch("INSERT INTO judgments.bugfinder_findings (run_id, wave, category, file, detail, dropped) VALUES
          (1, 2, 'security', 'src/a.mjs', 'unchecked token in header', 0),
          (1, 2, 'checking', 'src/b.mjs', 'null deref on empty list', 0),
          (1, 2, 'security', 'src/c.mjs', 'void by rule', 1);").unwrap();

        let by_cat = researchFind(&json!({ "op": "evidence", "category": "security" }), &c).unwrap();
        let fs = by_cat["findings"].as_array().unwrap();
        assert_eq!(fs.len(), 1, "dropped findings never surface");
        assert!(fs[0]["detail"].as_str().unwrap().contains("unchecked token"));

        let by_query = researchFind(&json!({ "op": "evidence", "query": "null deref" }), &c).unwrap();
        assert_eq!(by_query["findings"].as_array().unwrap().len(), 1);

        let by_file = researchFind(&json!({ "op": "evidence", "file": "b.mjs" }), &c).unwrap();
        assert_eq!(by_file["findings"].as_array().unwrap().len(), 1);
        assert_eq!(by_file["findings"][0]["file"], json!("src/b.mjs"));
    }

    #[test]
    fn unknown_op_returns_error() {
        let c = db();
        let r = researchFind(&json!({ "op": "nope" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "unknown op: nope \u{2014} use papers|paper|concepts|evidence" }));
    }

    #[test]
    fn missing_tables_degrade_to_empty_never_throw() {
        // a mission-less workspace: judgments attached but none of the mission tables exist yet.
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments;").unwrap();
        assert_eq!(researchFind(&json!({ "op": "papers" }), &c).unwrap()["papers"], json!([]));
        assert_eq!(researchFind(&json!({ "op": "concepts" }), &c).unwrap()["concepts"], json!([]));
        assert_eq!(researchFind(&json!({ "op": "evidence" }), &c).unwrap()["findings"], json!([]));
    }

    #[test]
    fn limit_falsy_zero_resets_to_default_negative_clamps_to_one() {
        let c = db();
        for i in 1..=3 {
            c.execute(&format!("INSERT INTO judgments.review_papers (id, slug, title, paper_json) VALUES ({i}, 's{i}', 'T{i}', '{{}}')"), []).unwrap();
        }
        // `Number(v ?? d) || d`: 0 is FALSY in JS, so limit=0 resets to the default (25), not 1 —
        // with only 3 rows seeded, all 3 come back (distinguishes this helper from a plain clamp).
        let zero = researchFind(&json!({ "op": "papers", "limit": 0 }), &c).unwrap();
        assert_eq!(zero["papers"].as_array().unwrap().len(), 3);

        // a negative limit is truthy (nonzero), so it skips the `|| d` fallback and clamps to 1.
        let neg = researchFind(&json!({ "op": "papers", "limit": -5 }), &c).unwrap();
        assert_eq!(neg["papers"].as_array().unwrap().len(), 1);

        let high = researchFind(&json!({ "op": "papers", "limit": 1000 }), &c).unwrap();
        assert_eq!(high["papers"].as_array().unwrap().len(), 3);
    }
}
