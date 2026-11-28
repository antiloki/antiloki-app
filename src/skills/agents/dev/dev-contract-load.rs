//! dev-contract-load.rs — twin of skills/agents/dev/dev-contract-load.mjs (db READ lane).
//! NAMES ARE CONTRACT: `devContractLoad` (default). Contract by slug (via contract-get twin) +
//! must_obey law texts + parent card + ready_for_impl gate.
// QUALITY: 84/100 — card key: null (no card_slug) vs OMITTED (.get() miss → JS undefined)
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

pub fn devContractLoad(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: slug = typeof input === 'string' ? input : input?.slug
    let slug = match input.as_str() {
        Some(s) => s.to_string(),
        None => input.get("slug").and_then(|v| v.as_str()).unwrap_or("").to_string(),
    };
    if slug.is_empty() {
        return Err("dev-contract-load: slug is required".into());
    }

    let contract = crate::contract_contract_get::contractGet(&json!(slug), db)?;
    if contract.is_null() {
        return Err(format!("dev-contract-load: contract '{}' not found", slug));
    }

    // JS: for (const lawSlug of (contract.must_obey ?? [])) — null/absent iterates nothing
    let mut laws: Vec<Value> = Vec::new();
    if let Some(must_obey) = contract.get("must_obey").and_then(|v| v.as_array()) {
        for law_slug in must_obey {
            let rows = crate::db_json::query_rows(
                db,
                "SELECT slug, title, statement, rationale, enforcement FROM laws.laws WHERE slug = ?",
                &[law_slug.clone()],
            )?;
            if let Some(row) = rows.as_array().and_then(|a| a.first()) {
                laws.push(row.clone());
            }
        }
    }

    let mut out = Map::new();
    out.insert("contract".into(), contract.clone());
    out.insert("laws_full".into(), Value::Array(laws));
    let card_slug = contract.get("card_slug");
    if truthy(card_slug) {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT slug, title, body_json FROM cards WHERE slug = ?",
            &[card_slug.unwrap().clone()],
        )?;
        if let Some(row) = rows.as_array().and_then(|a| a.first()) {
            out.insert("card".into(), row.clone());
        } // .get() miss → undefined → JSON.stringify OMITS the key
    } else {
        out.insert("card".into(), Value::Null);
    }
    let status = contract.get("status").and_then(|v| v.as_str());
    out.insert(
        "ready_for_impl".into(),
        // FIX-DEV-IMPL-ACCEPTS-IMPL-STATUS-ON-RETRY (card 307): 'impl' is a valid re-entry state
        json!(matches!(status, Some("tl_approved") | Some("human_approved") | Some("impl"))),
    );
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH DATABASE ':memory:' AS laws;
             CREATE TABLE contracts (slug TEXT, title TEXT, status TEXT, card_slug TEXT, must_obey_json TEXT, acceptance_json TEXT);
             CREATE TABLE cards (slug TEXT, title TEXT, body_json TEXT);
             CREATE TABLE laws.laws (slug TEXT, title TEXT, statement TEXT, rationale TEXT, enforcement TEXT);",
        ).unwrap();
        c
    }

    #[test]
    fn loads_contract_laws_card_and_ready_flag() {
        let c = make_db();
        c.execute_batch(
            "INSERT INTO contracts VALUES ('C1','Ship widget','tl_approved','CARD-1','[\"LAW_B\",\"LAW_A\"]','[{\"ac\":1}]');
             INSERT INTO cards VALUES ('CARD-1','Card one','{\"kind\":\"feature\"}');
             INSERT INTO laws.laws VALUES ('LAW_A','A law','stmt A','why A','hard'),('LAW_B','B law','stmt B','why B','soft');",
        ).unwrap();
        let r = devContractLoad(&json!({ "slug": "C1" }), &c).unwrap();
        assert_eq!(r["contract"]["acceptance"], json!([{ "ac": 1 }])); // contract-get parsed JSON cols
        assert_eq!(r["contract"]["must_obey"], json!(["LAW_B", "LAW_A"]));
        // laws_full follows must_obey order, not table order
        let lf = r["laws_full"].as_array().unwrap();
        assert_eq!(lf.len(), 2);
        assert_eq!(lf[0]["slug"], json!("LAW_B"));
        assert_eq!(lf[1]["slug"], json!("LAW_A"));
        assert_eq!(lf[1]["enforcement"], json!("hard"));
        assert_eq!(r["card"]["title"], json!("Card one"));
        assert_eq!(r["ready_for_impl"], json!(true));
    }

    #[test]
    fn missing_law_skipped_null_card_slug_not_ready() {
        let c = make_db();
        c.execute_batch(
            "INSERT INTO contracts VALUES ('C2','Draft','proposed',NULL,'[\"LAW_A\",\"LAW_MISSING\"]',NULL);
             INSERT INTO laws.laws VALUES ('LAW_A','A law','stmt A','why A','hard');",
        ).unwrap();
        let r = devContractLoad(&json!({ "slug": "C2" }), &c).unwrap();
        assert_eq!(r["laws_full"].as_array().unwrap().len(), 1);
        assert_eq!(r["card"], Value::Null); // card_slug NULL → falsy → null
        assert_eq!(r["ready_for_impl"], json!(false));
    }

    #[test]
    fn ghost_card_omits_key_and_impl_is_ready() {
        let c = make_db();
        c.execute_batch(
            "INSERT INTO contracts VALUES ('C3','Retry','impl','GHOST',NULL,'[]');",
        ).unwrap();
        let r = devContractLoad(&json!("C3"), &c).unwrap(); // string input branch
        assert_eq!(r["laws_full"], json!([])); // must_obey_json NULL → no must_obey key → []
        assert!(r.get("card").is_none()); // .get() miss → undefined → key OMITTED
        assert_eq!(r["ready_for_impl"], json!(true)); // 'impl' = QA-retry re-entry
    }

    #[test]
    fn invalid_must_obey_json_and_empty_card_slug() {
        let c = make_db();
        c.execute_batch(
            "INSERT INTO contracts VALUES ('C4','Approved','human_approved','','bad json','{\"a\":2}');",
        ).unwrap();
        let r = devContractLoad(&json!({ "slug": "C4" }), &c).unwrap();
        assert_eq!(r["contract"]["must_obey"], Value::Null); // parse failure → null
        assert_eq!(r["laws_full"], json!([]));
        assert_eq!(r["card"], Value::Null); // '' → falsy → null
        assert_eq!(r["ready_for_impl"], json!(true));
    }

    #[test]
    fn throws_on_missing_slug_and_unknown_contract() {
        let c = make_db();
        assert!(devContractLoad(&json!({}), &c).unwrap_err().contains("slug is required"));
        assert!(devContractLoad(&json!("ghost"), &c).unwrap_err().contains("contract 'ghost' not found"));
    }
}
