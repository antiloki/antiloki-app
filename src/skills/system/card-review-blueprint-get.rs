//! card-review-blueprint-get.rs — twin of skills/system/card-review-blueprint-get.mjs (db READ).
//! NAMES ARE CONTRACT: `cardReviewBlueprintGet` (default). Latest frozen review blueprint for a
//! card (or a specific contract/attempt); returns null when none exists yet.
// QUALITY: 82/100 — dynamic WHERE over card_slug/contract_slug/attempt, then hydrate() JSON columns.
//
// The JS source imports `hydrate` from the sibling module card-review-blueprint-generate.mjs,
// which has no Rust twin yet. `hydrate` is a pure, side-effect-free JSON-column parser (no db,
// no cross-skill call) — it is inlined here verbatim rather than treating this as a blocking
// cross-module dependency (see card-review-blueprints-list.rs for the same local-helper pattern).
use rusqlite::Connection;
use serde_json::{Map, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `x != null`

// JS: function hydrate(row) { if (!row) return null; const out = {...row}; for (col of
// ['diff_json','anchors_json','behavioral_json']) out[col.replace(/_json$/,'')] = JSON.parse(row[col] ?? 'null')
// (catch -> null); return out; }  — appends diff/anchors/behavioral parsed keys after the raw columns.
fn hydrate(row: Option<Value>) -> Value {
    let mut out = match row {
        Some(Value::Object(m)) => m,
        _ => return Value::Null,
    };
    for col in ["diff_json", "anchors_json", "behavioral_json"] {
        let key = &col[..col.len() - "_json".len()];
        let parsed = out
            .get(col)
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .unwrap_or(Value::Null);
        out.insert(key.to_string(), parsed);
    }
    Value::Object(out)
}

pub fn cardReviewBlueprintGet(input: &Value, db: &Connection) -> Result<Value, String> {
    let card_slug = input.get("card_slug").filter(|v| truthy(Some(v)));
    let contract_slug = input.get("contract_slug").filter(|v| truthy(Some(v)));
    if card_slug.is_none() && contract_slug.is_none() {
        return Err("card-review-blueprint-get: card_slug or contract_slug is required".to_string());
    }
    let attempt = nz(input.get("attempt"));

    let mut clauses: Vec<&str> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(v) = card_slug { clauses.push("card_slug = ?"); params.push(v.clone()); }
    if let Some(v) = contract_slug { clauses.push("contract_slug = ?"); params.push(v.clone()); }
    if let Some(v) = attempt { clauses.push("attempt = ?"); params.push(v.clone()); }

    let sql = format!(
        "SELECT * FROM card_review_blueprints WHERE {} ORDER BY generated_at DESC, id DESC LIMIT 1",
        clauses.join(" AND ")
    );
    let rows = crate::db_json::query_rows(db, &sql, &params)?;
    let row = rows.as_array().and_then(|a| a.first()).cloned();
    Ok(hydrate(row))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE card_review_blueprints (
                id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, contract_slug TEXT, attempt INTEGER,
                qa_run_id INTEGER, base_sha TEXT, head_sha TEXT, diff_source TEXT,
                diff_json TEXT, anchors_json TEXT, behavioral_json TEXT, narrative_md TEXT,
                verdict TEXT, generated_by TEXT, generated_at TEXT);
             INSERT INTO card_review_blueprints
                (id, card_slug, contract_slug, attempt, diff_json, anchors_json, behavioral_json, verdict, generated_at) VALUES
                (1,'CARD-X','CONTRACT-1',1,'[{\"file\":\"a.js\"}]','[{\"passed\":true}]','[]','qa-passed','2026-01-01 00:00:00'),
                (2,'CARD-X','CONTRACT-1',2,'[{\"file\":\"b.js\"}]','[]','[{\"passed\":false}]','qa-failed','2026-02-01 00:00:00'),
                (3,'CARD-Y','CONTRACT-2',1,'[]','[]','[]','qa-passed','2026-01-10 00:00:00');",
        ).unwrap();
        c
    }

    #[test]
    fn requires_card_slug_or_contract_slug() {
        let c = seeded();
        let r = cardReviewBlueprintGet(&json!({}), &c);
        assert_eq!(r, Err("card-review-blueprint-get: card_slug or contract_slug is required".to_string()));
    }

    #[test]
    fn returns_null_when_no_row_matches() {
        let c = seeded();
        let r = cardReviewBlueprintGet(&json!({ "card_slug": "NOPE" }), &c).unwrap();
        assert_eq!(r, Value::Null);
    }

    #[test]
    fn returns_latest_hydrated_row_by_card_slug() {
        let c = seeded();
        let r = cardReviewBlueprintGet(&json!({ "card_slug": "CARD-X" }), &c).unwrap();
        assert_eq!(r["attempt"], json!(2));
        assert_eq!(r["verdict"], json!("qa-failed"));
        assert_eq!(r["diff"], json!([{ "file": "b.js" }]));
        assert_eq!(r["anchors"], json!([]));
        assert_eq!(r["behavioral"], json!([{ "passed": false }]));
    }

    #[test]
    fn contract_slug_alone_resolves_the_row() {
        let c = seeded();
        let r = cardReviewBlueprintGet(&json!({ "contract_slug": "CONTRACT-2" }), &c).unwrap();
        assert_eq!(r["card_slug"], json!("CARD-Y"));
    }

    #[test]
    fn attempt_filter_narrows_to_a_specific_attempt() {
        let c = seeded();
        let r = cardReviewBlueprintGet(&json!({ "card_slug": "CARD-X", "attempt": 1 }), &c).unwrap();
        assert_eq!(r["attempt"], json!(1));
        assert_eq!(r["diff"], json!([{ "file": "a.js" }]));
    }
}
