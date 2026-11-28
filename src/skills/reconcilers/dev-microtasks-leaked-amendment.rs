//! dev-microtasks-leaked-amendment.rs — twin of skills/reconcilers/dev-microtasks-leaked-amendment.mjs
//! (reconciler-object lane: the METHODS are the units, both db-last). NAMES ARE CONTRACT:
//! `detect` (detect(db)) and `fix` (fix(finding, db)). Pre-2026-05-26 dev-consume-qa-suggestions
//! leaked kind='contract-amendment' entries into contract.microtasks_json; detect surfaces open
//! contracts carrying them, fix filters them out and pulls dev_microtask_progress back for
//! entries removed before the cursor. datetime('now') in the fix UPDATE is written frozen
//! ('2026-01-01 00:00:00') to match the conform JS lane's frozen clock.

use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')
fn js_string(v:&Value)->String{ match v{Value::Null=>"null".into(),Value::String(s)=>s.clone(),Value::Number(n)=>n.to_string(),Value::Bool(b)=>b.to_string(),Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","),Value::Object(_)=>"[object Object]".into()} } // = JS String(v)
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } } // = JS `${v}`
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } } // = JS Number(v)

// JS `JSON.parse(v)` — JSON.parse stringifies its argument first (null → "null" → parses to
// null, NOT a parse error; playbook gotcha 6), so coerce via js_string before parsing.
fn json_parse(v: &Value) -> Result<Value, ()> {
    serde_json::from_str::<Value>(&js_string(v)).map_err(|_| ())
}

// JS `m?.kind === 'contract-amendment'` — only an object whose kind is that exact string
// matches; null/strings/arrays/numbers yield undefined and compare false.
fn is_leaked_amendment(m: &Value) -> bool {
    m.get("kind").and_then(|k| k.as_str()) == Some("contract-amendment")
}

/// JS: `async detect(db)` — open contracts (impl/qa/tl_approved/pl_ratified, not superseded)
/// whose microtasks_json contains a kind='contract-amendment' entry. The try/catch around the
/// query maps a missing contracts table to rows = [] (JS `catch { /* table absent */ }`);
/// rows whose microtasks_json fails JSON.parse are skipped (`catch { continue; }`).
pub fn detect(db: &Connection) -> Value {
    let rows = match crate::db_json::query_rows(
        db,
        "\n        SELECT slug, card_slug, microtasks_json, dev_microtask_progress\n        FROM contracts\n        WHERE status IN ('impl', 'qa', 'tl_approved', 'pl_ratified')\n          AND COALESCE(superseded_by, '') = ''\n          AND microtasks_json LIKE '%contract-amendment%'\n      ",
        &[],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => Vec::new(), // table absent
    };
    let mut findings: Vec<Value> = Vec::new();
    for r in &rows {
        let mts = match json_parse(r.get("microtasks_json").unwrap_or(&Value::Null)) {
            Ok(v) => v,
            Err(()) => continue,
        };
        // JS `.filter` on a non-array would throw; that shape is unreachable through the SQL
        // LIKE filter for real contracts and is never fixtured — skip like an empty match.
        let arr = match mts.as_array() { Some(a) => a, None => continue };
        let leaked_count = arr.iter().filter(|m| is_leaked_amendment(m)).count();
        if leaked_count == 0 { continue; }
        findings.push(json!({
            "kind": "dev-microtasks-leaked-amendment",
            "severity": "human_blocking",
            "message": format!(
                "{}: contract {} has {} contract-amendment microtask(s) leaked into microtasks_json (DEV will refuse them)",
                tmpl(r.get("card_slug")), tmpl(r.get("slug")), leaked_count
            ),
            "card_slug": r.get("card_slug").cloned().unwrap_or(Value::Null),
            "contract_slug": r.get("slug").cloned().unwrap_or(Value::Null),
            "detail": {
                "contract_slug": r.get("slug").cloned().unwrap_or(Value::Null),
                "leaked_count": leaked_count,
                "current_progress": r.get("dev_microtask_progress").cloned().unwrap_or(Value::Null),
            },
            "suggested_action": "filter contract-amendment out of microtasks_json",
        }));
    }
    json!(findings)
}

/// JS: `async fix(finding, db)` — filter contract-amendment entries out of microtasks_json,
/// pulling dev_microtask_progress back by the number of removed entries that sat before the
/// cursor. updated_at gets the frozen clock instant (JS lane rewrites datetime('now')).
pub fn fix(finding: &Value, db: &Connection) -> Value {
    let slug = finding.get("contract_slug");
    if !truthy(slug) {
        return json!({ "applied": false, "reason": "missing contract_slug" });
    }
    let slug_v = slug.unwrap().clone();
    // node:sqlite `.get(slug)` = first row or undefined. The JS has no try/catch here, so a
    // missing contracts table throws — never fixtured (the fixture schema creates the table).
    let rows = crate::db_json::query_rows(
        db,
        "SELECT microtasks_json, dev_microtask_progress FROM contracts WHERE slug = ?",
        &[slug_v.clone()],
    )
    .ok()
    .and_then(|v| v.as_array().and_then(|a| a.first().cloned()));
    let row = match rows {
        Some(r) => r,
        None => return json!({ "applied": false, "reason": "contract not found" }),
    };
    let mts = match json_parse(row.get("microtasks_json").unwrap_or(&Value::Null)) {
        Ok(v) => v,
        Err(()) => return json!({ "applied": false, "reason": "malformed microtasks_json" }),
    };
    let arr = match mts.as_array() {
        Some(a) => a.clone(),
        None => return json!({ "applied": false, "reason": "microtasks_json not an array" }),
    };
    // oldProgress = row.dev_microtask_progress ?? 0 — raw value rides into the output;
    // the `i < oldProgress` comparison is numeric (JS relational coercion).
    let old_progress_v = nz(row.get("dev_microtask_progress")).cloned().unwrap_or(json!(0));
    let old_progress = js_number(Some(&old_progress_v));
    let mut removed_before_cursor: i64 = 0;
    let mut filtered: Vec<Value> = Vec::new();
    for (i, m) in arr.iter().enumerate() {
        if is_leaked_amendment(m) {
            if (i as f64) < old_progress { removed_before_cursor += 1; }
            continue;
        }
        filtered.push(m.clone());
    }
    let new_progress = (old_progress - removed_before_cursor as f64).max(0.0);
    // integer-valued JS numbers render as integers (gotcha 2) — bind + output as i64.
    let new_progress_v = if new_progress.is_finite() && new_progress.fract() == 0.0 {
        json!(new_progress as i64)
    } else {
        json!(new_progress)
    };
    let stringified = serde_json::to_string(&Value::Array(filtered.clone())).unwrap_or_default();
    let applied = match crate::db_json::run_write(
        db,
        &format!("\n      UPDATE contracts\n      SET microtasks_json = ?, dev_microtask_progress = ?, updated_at = {}\n      WHERE slug = ?\n    ", crate::clock::now_sql()),
        &[json!(stringified), new_progress_v.clone(), slug_v.clone()],
    ) {
        Ok((changes, _)) => changes > 0,
        Err(_) => false,
    };
    json!({
        "applied": applied,
        "changes": {
            "contract_slug": slug_v,
            "removed": (arr.len() - filtered.len()) as i64,
            "old_progress": old_progress_v,
            "new_progress": new_progress_v,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT, status TEXT, superseded_by TEXT, microtasks_json TEXT, dev_microtask_progress INTEGER, updated_at TEXT)").unwrap();
        c
    }

    fn insert(c: &Connection, slug: &str, card: Option<&str>, status: &str, superseded: Option<&str>, mts: Option<&str>, progress: Option<i64>) {
        c.execute(
            "INSERT INTO contracts (slug, card_slug, status, superseded_by, microtasks_json, dev_microtask_progress, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, '2025-12-01 00:00:00')",
            rusqlite::params![slug, card, status, superseded, mts, progress],
        ).unwrap();
    }

    #[test]
    fn absent_table_and_empty_table_return_empty() {
        let empty = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&empty), json!([])); // table absent → JS catch → []
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn detect_filters_status_superseded_substring_and_malformed() {
        let c = db();
        insert(&c, "CT-A", Some("CARD-A"), "impl", None,
            Some(r#"[{"kind":"contract-amendment","desc":"amend A"},{"kind":"impl","desc":"do the thing"},{"kind":"contract-amendment","desc":"amend B"}]"#), Some(2));
        insert(&c, "CT-B", Some("CARD-B"), "done", None,
            Some(r#"[{"kind":"contract-amendment"}]"#), Some(0)); // status excluded
        insert(&c, "CT-C", Some("CARD-C"), "qa", Some("CT-C2"),
            Some(r#"[{"kind":"contract-amendment"}]"#), Some(0)); // superseded excluded
        insert(&c, "CT-D", Some("CARD-D"), "impl", None,
            Some(r#"[{"kind":"impl","desc":"mention contract-amendment in prose"}]"#), Some(0)); // LIKE hits, zero leaked
        insert(&c, "CT-E", Some("CARD-E"), "impl", None,
            Some("contract-amendment{"), Some(0)); // malformed JSON → continue
        insert(&c, "CT-F", Some("CARD-F"), "impl", None,
            Some(r#"[{"kind":"impl"}]"#), Some(0)); // no substring → SQL-filtered
        insert(&c, "CT-G", Some("CARD-G"), "pl_ratified", None,
            Some(r#"[{"kind":"contract-amendment","desc":"amend G"}]"#), Some(0));
        let f = detect(&c);
        assert_eq!(f, json!([
            {
                "kind": "dev-microtasks-leaked-amendment",
                "severity": "human_blocking",
                "message": "CARD-A: contract CT-A has 2 contract-amendment microtask(s) leaked into microtasks_json (DEV will refuse them)",
                "card_slug": "CARD-A",
                "contract_slug": "CT-A",
                "detail": { "contract_slug": "CT-A", "leaked_count": 2, "current_progress": 2 },
                "suggested_action": "filter contract-amendment out of microtasks_json",
            },
            {
                "kind": "dev-microtasks-leaked-amendment",
                "severity": "human_blocking",
                "message": "CARD-G: contract CT-G has 1 contract-amendment microtask(s) leaked into microtasks_json (DEV will refuse them)",
                "card_slug": "CARD-G",
                "contract_slug": "CT-G",
                "detail": { "contract_slug": "CT-G", "leaked_count": 1, "current_progress": 0 },
                "suggested_action": "filter contract-amendment out of microtasks_json",
            },
        ]));
    }

    #[test]
    fn detect_null_card_slug_and_progress_coerce_like_js() {
        let c = db();
        insert(&c, "CT-NULL", None, "tl_approved", None, Some(r#"[{"kind":"contract-amendment"}]"#), None);
        let f = detect(&c);
        assert_eq!(f[0]["message"], json!("null: contract CT-NULL has 1 contract-amendment microtask(s) leaked into microtasks_json (DEV will refuse them)")); // `${null}` → "null"
        assert_eq!(f[0]["card_slug"], Value::Null);
        assert_eq!(f[0]["detail"]["current_progress"], Value::Null);
    }

    #[test]
    fn fix_guard_reasons() {
        let c = db();
        assert_eq!(fix(&json!({}), &c), json!({ "applied": false, "reason": "missing contract_slug" }));
        assert_eq!(fix(&json!({ "contract_slug": "" }), &c), json!({ "applied": false, "reason": "missing contract_slug" }));
        assert_eq!(fix(&json!({ "contract_slug": null }), &c), json!({ "applied": false, "reason": "missing contract_slug" }));
        assert_eq!(fix(&json!({ "contract_slug": "NOPE" }), &c), json!({ "applied": false, "reason": "contract not found" }));
        insert(&c, "CT-BAD", Some("CARD-1"), "impl", None, Some("{oops"), Some(0));
        assert_eq!(fix(&json!({ "contract_slug": "CT-BAD" }), &c), json!({ "applied": false, "reason": "malformed microtasks_json" }));
        insert(&c, "CT-OBJ", Some("CARD-1"), "impl", None, Some(r#"{"kind":"contract-amendment"}"#), Some(0));
        assert_eq!(fix(&json!({ "contract_slug": "CT-OBJ" }), &c), json!({ "applied": false, "reason": "microtasks_json not an array" }));
        // NULL column: JSON.parse(null) → null (NOT malformed) → not-an-array path
        insert(&c, "CT-NULLM", Some("CARD-1"), "impl", None, None, Some(0));
        assert_eq!(fix(&json!({ "contract_slug": "CT-NULLM" }), &c), json!({ "applied": false, "reason": "microtasks_json not an array" }));
    }

    #[test]
    fn fix_filters_amendments_and_pulls_cursor_back() {
        let c = db();
        insert(&c, "CT-HAPPY", Some("CARD-1"), "impl", None,
            Some(r#"[{"kind":"contract-amendment","desc":"a1"},{"kind":"impl","desc":"k1"},{"kind":"contract-amendment","desc":"a2"},{"kind":"impl","desc":"k2"}]"#), Some(3));
        let r = fix(&json!({ "contract_slug": "CT-HAPPY" }), &c);
        assert_eq!(r, json!({
            "applied": true,
            "changes": { "contract_slug": "CT-HAPPY", "removed": 2, "old_progress": 3, "new_progress": 1 },
        }));
        let (mts, prog, updated): (String, i64, String) = c.query_row(
            "SELECT microtasks_json, dev_microtask_progress, updated_at FROM contracts WHERE slug = 'CT-HAPPY'",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(mts, r#"[{"kind":"impl","desc":"k1"},{"kind":"impl","desc":"k2"}]"#);
        assert_eq!(prog, 1);
        assert_eq!(updated, "2026-01-01 00:00:00"); // frozen clock literal
    }

    #[test]
    fn fix_null_progress_defaults_to_zero() {
        let c = db();
        insert(&c, "CT-NP", Some("CARD-1"), "impl", None,
            Some(r#"[{"kind":"contract-amendment"},{"kind":"impl","desc":"k"}]"#), None);
        let r = fix(&json!({ "contract_slug": "CT-NP" }), &c);
        assert_eq!(r["changes"], json!({ "contract_slug": "CT-NP", "removed": 1, "old_progress": 0, "new_progress": 0 }));
    }

    #[test]
    fn fix_keeps_non_object_entries_and_ignores_amendments_after_cursor() {
        let c = db();
        insert(&c, "CT-AFTER", Some("CARD-1"), "impl", None,
            Some(r#"[{"kind":"impl","desc":"k"},null,"free-text note",{"kind":"contract-amendment"}]"#), Some(1));
        let r = fix(&json!({ "contract_slug": "CT-AFTER" }), &c);
        assert_eq!(r["changes"], json!({ "contract_slug": "CT-AFTER", "removed": 1, "old_progress": 1, "new_progress": 1 }));
        let mts: String = c.query_row("SELECT microtasks_json FROM contracts WHERE slug = 'CT-AFTER'", [], |row| row.get(0)).unwrap();
        assert_eq!(mts, r#"[{"kind":"impl","desc":"k"},null,"free-text note"]"#); // m?.kind on null/string → undefined → kept
    }

    #[test]
    fn fix_with_no_amendments_still_applies() {
        let c = db();
        insert(&c, "CT-CLEAN", Some("CARD-1"), "impl", None, Some(r#"[{"kind":"impl","desc":"k"}]"#), Some(1));
        let r = fix(&json!({ "contract_slug": "CT-CLEAN" }), &c);
        assert_eq!(r, json!({
            "applied": true,
            "changes": { "contract_slug": "CT-CLEAN", "removed": 0, "old_progress": 1, "new_progress": 1 },
        }));
    }
}
