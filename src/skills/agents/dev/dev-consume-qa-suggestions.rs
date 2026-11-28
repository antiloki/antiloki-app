//! dev-consume-qa-suggestions.rs — twin of skills/agents/dev/dev-consume-qa-suggestions.mjs (db WRITE
//! lane). NAMES ARE CONTRACT: `devConsumeQaSuggestions` (default). Consume QA's suggested microtasks
//! from the newest failing qa_run past the contract's consume-cursor: drop contract-amendments +
//! intent-less items, stamp + append to microtasks_json, advance last_qa_consumed_run_id. Idempotent.
// QUALITY: cursor guard (id > last_consumed) · amendment filter · empty→unfixable · frozen-clock updated_at
use rusqlite::{params, Connection};
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
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// JS loadJsonArr(s): falsy s (null/'' ) → []; JSON.parse then Array?arr:[]; parse error → [].
fn load_json_arr(s: Option<&Value>) -> Vec<Value> {
    match s {
        Some(Value::String(x)) if !x.is_empty() => match serde_json::from_str::<Value>(x) {
            Ok(Value::Array(a)) => a,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

pub fn devConsumeQaSuggestions(input: &Value, db: &Connection) -> Result<Value, String> {
    // guard-throws (context.db missing / contract_slug missing / contract not found): not fixtured
    // — the harness only judges the success path — but mirror JS `throw` as Err(String).
    let contract_slug = match nz(input.get("contract_slug")) {
        Some(v) if truthy(Some(v)) => v.clone(),
        _ => return Err("dev-consume-qa-suggestions: contract_slug is required".into()),
    };
    let slug_s = contract_slug.as_str().unwrap_or("");

    let contract_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, microtasks_json, last_qa_consumed_run_id FROM contracts WHERE slug = ?",
        &[json!(slug_s)],
    )?;
    let contract = match contract_rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Err(format!("contract '{slug_s}' not found")),
    };

    // Number(contract.last_qa_consumed_run_id ?? 0) — null/missing → 0.
    let last_consumed = nz(contract.get("last_qa_consumed_run_id")).and_then(|v| v.as_i64()).unwrap_or(0);

    let qa_rows = crate::db_json::query_rows(
        db,
        "SELECT id, suggested_microtasks_json FROM qa_runs \
         WHERE contract_slug = ? AND all_passed = 0 AND id > ? \
           AND suggested_microtasks_json IS NOT NULL AND suggested_microtasks_json != '' \
         ORDER BY id DESC LIMIT 1",
        &[json!(slug_s), json!(last_consumed)],
    )?;
    let qa_rows = qa_rows.as_array().cloned().unwrap_or_default();
    let mt_json = contract.get("microtasks_json");

    // No newer failing qa_run with suggestions → appended=0, cursor untouched.
    let qa_run = match qa_rows.first() {
        Some(r) => r,
        None => {
            let total = load_json_arr(mt_json).len();
            let mut out = Map::new();
            out.insert("contract_slug".into(), contract_slug.clone());
            out.insert("consumed_qa_run_id".into(), Value::Null);
            out.insert("appended".into(), json!(0));
            out.insert("total_microtasks".into(), json!(total as i64));
            out.insert("new_microtasks_json".into(), nz(mt_json).cloned().unwrap_or(json!("[]")));
            return Ok(Value::Object(out));
        }
    };
    let qa_id = qa_run.get("id").cloned().unwrap_or(Value::Null);
    let qa_id_i = qa_id.as_i64().unwrap_or(0);

    // keep only real object suggestions with a truthy intent, then drop contract-amendments
    // (those route to TL, not DEV).
    let suggestions: Vec<Value> = load_json_arr(qa_run.get("suggested_microtasks_json"))
        .into_iter()
        .filter(|m| m.is_object() && truthy(m.get("intent")))
        .filter(|m| m.get("kind").and_then(|v| v.as_str()) != Some("contract-amendment"))
        .collect();

    // QA said "DEV can't fix this" — still advance the cursor so we don't re-evaluate forever.
    if suggestions.is_empty() {
        db.execute(
            &format!("UPDATE contracts SET last_qa_consumed_run_id = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
            params![qa_id_i, slug_s],
        ).map_err(|e| e.to_string())?;
        let mut out = Map::new();
        out.insert("contract_slug".into(), contract_slug.clone());
        out.insert("consumed_qa_run_id".into(), qa_id.clone());
        out.insert("appended".into(), json!(0));
        out.insert("total_microtasks".into(), json!(load_json_arr(mt_json).len() as i64));
        out.insert("new_microtasks_json".into(), nz(mt_json).cloned().unwrap_or(json!("[]")));
        out.insert("qa_said_unfixable".into(), json!(true));
        return Ok(Value::Object(out));
    }

    let existing = load_json_arr(mt_json);
    let start_idx = existing.len();
    let stamped: Vec<Value> = suggestions.iter().enumerate().map(|(i, m)| {
        let mut o = Map::new();
        let mid = nz(m.get("microtask_id")).cloned()
            .unwrap_or_else(|| json!(format!("qa{}-suggested-{}", qa_id_i, start_idx + i + 1)));
        o.insert("microtask_id".into(), mid);
        o.insert("intent".into(), m.get("intent").cloned().unwrap_or(Value::Null));
        o.insert("kind".into(), nz(m.get("kind")).cloned().unwrap_or(Value::Null));
        o.insert("hint".into(), nz(m.get("hint")).cloned().unwrap_or(Value::Null));
        o.insert("source".into(), json!({ "qa_run_id": qa_id.clone() }));
        Value::Object(o)
    }).collect();

    let mut merged = existing;
    merged.extend(stamped.iter().cloned());
    let merged_json = serde_json::to_string(&Value::Array(merged.clone())).map_err(|e| e.to_string())?;

    db.execute(
        &format!("UPDATE contracts SET microtasks_json = ?, last_qa_consumed_run_id = ?, updated_at = {} WHERE slug = ?", crate::clock::now_sql()),
        params![merged_json, qa_id_i, slug_s],
    ).map_err(|e| e.to_string())?;

    let mut out = Map::new();
    out.insert("contract_slug".into(), contract_slug.clone());
    out.insert("consumed_qa_run_id".into(), qa_id.clone());
    out.insert("appended".into(), json!(stamped.len() as i64));
    out.insert("total_microtasks".into(), json!(merged.len() as i64));
    out.insert("new_microtasks_json".into(), json!(merged_json));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE contracts (slug TEXT PRIMARY KEY, microtasks_json TEXT, last_qa_consumed_run_id INTEGER, updated_at TEXT); \
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY, contract_slug TEXT, all_passed INTEGER, suggested_microtasks_json TEXT);",
        ).unwrap();
        c
    }
    fn contract(c: &Connection, slug: &str, mt: Option<&str>, cursor: Option<i64>) {
        c.execute(
            "INSERT INTO contracts (slug, microtasks_json, last_qa_consumed_run_id, updated_at) VALUES (?1, ?2, ?3, '2025-12-01 00:00:00')",
            params![slug, mt, cursor],
        ).unwrap();
    }
    fn qa(c: &Connection, id: i64, slug: &str, passed: i64, sug: Option<&str>) {
        c.execute(
            "INSERT INTO qa_runs (id, contract_slug, all_passed, suggested_microtasks_json) VALUES (?1, ?2, ?3, ?4)",
            params![id, slug, passed, sug],
        ).unwrap();
    }
    fn cursor_of(c: &Connection, slug: &str) -> Option<i64> {
        c.query_row("SELECT last_qa_consumed_run_id FROM contracts WHERE slug=?1", params![slug], |r| r.get(0)).unwrap()
    }

    #[test]
    fn no_qa_run_leaves_cursor() {
        let c = db();
        contract(&c, "C1", Some("[{\"microtask_id\":\"m1\",\"intent\":\"do one\"}]"), None);
        let r = devConsumeQaSuggestions(&json!({ "contract_slug": "C1" }), &c).unwrap();
        assert_eq!(r["consumed_qa_run_id"], Value::Null);
        assert_eq!(r["appended"], json!(0));
        assert_eq!(r["total_microtasks"], json!(1));
        assert_eq!(r["new_microtasks_json"], json!("[{\"microtask_id\":\"m1\",\"intent\":\"do one\"}]"));
        assert_eq!(cursor_of(&c, "C1"), None);
    }

    #[test]
    fn null_microtasks_defaults_to_empty_array() {
        let c = db();
        contract(&c, "C2", None, None);
        let r = devConsumeQaSuggestions(&json!({ "contract_slug": "C2" }), &c).unwrap();
        assert_eq!(r["total_microtasks"], json!(0));
        assert_eq!(r["new_microtasks_json"], json!("[]"));
    }

    #[test]
    fn passing_and_consumed_runs_are_ignored() {
        let c = db();
        contract(&c, "C3", Some("[]"), Some(5));
        qa(&c, 4, "C3", 0, Some("[{\"intent\":\"below cursor\"}]")); // id 4 <= 5 → excluded
        qa(&c, 6, "C3", 1, Some("[{\"intent\":\"passing\"}]")); // all_passed=1 → excluded
        let r = devConsumeQaSuggestions(&json!({ "contract_slug": "C3" }), &c).unwrap();
        assert_eq!(r["consumed_qa_run_id"], Value::Null);
        assert_eq!(r["appended"], json!(0));
    }

    #[test]
    fn empty_after_filter_marks_unfixable() {
        let c = db();
        contract(&c, "C6", Some("[]"), None);
        qa(&c, 3, "C6", 0, Some("[{\"intent\":\"amend\",\"kind\":\"contract-amendment\"},{\"kind\":\"migration\"}]"));
        let r = devConsumeQaSuggestions(&json!({ "contract_slug": "C6" }), &c).unwrap();
        assert_eq!(r["consumed_qa_run_id"], json!(3));
        assert_eq!(r["appended"], json!(0));
        assert_eq!(r["qa_said_unfixable"], json!(true));
        assert_eq!(cursor_of(&c, "C6"), Some(3)); // cursor advanced, no append
        assert_eq!(r["new_microtasks_json"], json!("[]"));
    }

    #[test]
    fn appends_stamped_and_advances() {
        let c = db();
        contract(&c, "C7", Some("[{\"microtask_id\":\"m1\",\"intent\":\"pre\"}]"), None);
        qa(&c, 7, "C7", 0, Some("[{\"microtask_id\":\"fix-a\",\"intent\":\"add ALTER\",\"kind\":\"migration\",\"hint\":\"m/x.mjs\"},{\"intent\":\"no id here\"},{\"intent\":\"amend\",\"kind\":\"contract-amendment\"}]"));
        let r = devConsumeQaSuggestions(&json!({ "contract_slug": "C7" }), &c).unwrap();
        assert_eq!(r["consumed_qa_run_id"], json!(7));
        assert_eq!(r["appended"], json!(2)); // amendment dropped
        assert_eq!(r["total_microtasks"], json!(3));
        let expected = "[{\"microtask_id\":\"m1\",\"intent\":\"pre\"},{\"microtask_id\":\"fix-a\",\"intent\":\"add ALTER\",\"kind\":\"migration\",\"hint\":\"m/x.mjs\",\"source\":{\"qa_run_id\":7}},{\"microtask_id\":\"qa7-suggested-3\",\"intent\":\"no id here\",\"kind\":null,\"hint\":null,\"source\":{\"qa_run_id\":7}}]";
        assert_eq!(r["new_microtasks_json"], json!(expected));
        // db_after: the persisted value equals the returned string
        let stored: String = c.query_row("SELECT microtasks_json FROM contracts WHERE slug='C7'", [], |r| r.get(0)).unwrap();
        assert_eq!(stored, expected);
        assert_eq!(cursor_of(&c, "C7"), Some(7));
    }

    #[test]
    fn newest_unconsumed_run_wins() {
        let c = db();
        contract(&c, "C8", Some("[]"), Some(10));
        qa(&c, 9, "C8", 0, Some("[{\"intent\":\"below cursor\"}]")); // excluded (id<=10)
        qa(&c, 11, "C8", 0, Some("[{\"intent\":\"older new\"}]"));
        qa(&c, 12, "C8", 0, Some("[{\"intent\":\"newest\"}]"));
        qa(&c, 99, "OTHER", 0, Some("[{\"intent\":\"other contract\"}]")); // different slug
        let r = devConsumeQaSuggestions(&json!({ "contract_slug": "C8" }), &c).unwrap();
        assert_eq!(r["consumed_qa_run_id"], json!(12));
        assert_eq!(r["appended"], json!(1));
        assert_eq!(r["new_microtasks_json"], json!("[{\"microtask_id\":\"qa12-suggested-1\",\"intent\":\"newest\",\"kind\":null,\"hint\":null,\"source\":{\"qa_run_id\":12}}]"));
        assert_eq!(cursor_of(&c, "C8"), Some(12));
    }
}
