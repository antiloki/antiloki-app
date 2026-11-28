//! no-contract-for-doing-card.rs — twin of skills/system/focus-detectors/no-contract-for-doing-card.mjs
//! (db-last helper lane: detect(card, db)). NAMES ARE CONTRACT: the export is `detect` (the JS
//! named default). Fires when a 'doing' card has no live (non-superseded) contract; EPICs excluded.

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
// = JS String(v): JSON.parse coerces a non-string arg via String() before parsing.
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

/// JS: `detect(card, db)` — status must be exactly 'doing'; an epic body_json opts out
/// (epic-has-open-children handles epics); a live contract (COALESCE(superseded_by,'')='')
/// silences the finding. card_slug key-presence mirrors JS (JSON.stringify drops undefined).
pub fn detect(card: &Value, db: &Connection) -> Value {
    if card.get("status").and_then(|v| v.as_str()) != Some("doing") {
        return Value::Null;
    }

    // JS: try { body = card.body_json ? JSON.parse(card.body_json) : null } catch { body = null }
    let body: Option<Value> = match card.get("body_json") {
        Some(bj) if truthy(Some(bj)) => serde_json::from_str(&js_string(bj)).ok(),
        _ => None,
    };
    if let Some(b) = &body {
        if truthy(Some(b)) && b.get("kind").and_then(|v| v.as_str()) == Some("epic") {
            return Value::Null;
        }
    }

    let slug = card.get("slug").cloned().unwrap_or(Value::Null);
    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, status FROM contracts\n    WHERE card_slug = ? AND COALESCE(superseded_by, '') = ''\n    ORDER BY id DESC LIMIT 1",
        &[slug],
    )
    .ok()
    .and_then(|r| r.as_array().cloned())
    .unwrap_or_default();
    if !rows.is_empty() {
        return Value::Null;
    }

    let reason_md = concat!(
        "Card is in 'doing' but has no live contract. Without a contract ",
        "the state machine has nothing to dispatch. Common cause: card moved ",
        "to doing without a PL session producing a contract. Run pl.session.start ",
        "to create one, or move the card back to backlog.",
    );
    let mut opt_a = Map::new();
    opt_a.insert("id".into(), json!("A"));
    opt_a.insert("label".into(), json!("Start PL session (creates a contract)"));
    opt_a.insert("action".into(), json!("pl_session_start"));
    if let Some(s) = card.get("slug") {
        opt_a.insert("card_slug".into(), s.clone());
    }
    let mut opt_b = Map::new();
    opt_b.insert("id".into(), json!("B"));
    opt_b.insert("label".into(), json!("Move card back to backlog"));
    opt_b.insert("action".into(), json!("set_status"));
    if let Some(s) = card.get("slug") {
        opt_b.insert("card_slug".into(), s.clone());
    }
    opt_b.insert("status".into(), json!("backlog"));
    json!({
        "kind": "no_contract_for_doing_card",
        "detail": { "card_status": "doing" },
        "reason_md": reason_md,
        "surfaces_in": ["board:red-badge", "pipeline:blocked-pane", "fixit:finding"],
        "fix_options": [opt_a, opt_b],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with(rows: &[(&str, &str, &str, Option<&str>)]) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE contracts (id INTEGER PRIMARY KEY, slug TEXT, status TEXT, card_slug TEXT, superseded_by TEXT)").unwrap();
        for (slug, status, card_slug, superseded_by) in rows {
            c.execute(
                "INSERT INTO contracts (slug, status, card_slug, superseded_by) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![slug, status, card_slug, superseded_by],
            )
            .unwrap();
        }
        c
    }

    #[test]
    fn non_doing_and_epic_return_null() {
        let db = db_with(&[]);
        assert_eq!(detect(&json!({"status":"backlog","slug":"C1"}), &db), Value::Null);
        assert_eq!(
            detect(&json!({"status":"doing","slug":"E1","body_json":"{\"kind\":\"epic\"}"}), &db),
            Value::Null
        );
    }

    #[test]
    fn live_contract_silences_superseded_does_not() {
        let live = db_with(&[("CT-1", "active", "C1", None)]);
        assert_eq!(detect(&json!({"status":"doing","slug":"C1"}), &live), Value::Null);
        let superseded = db_with(&[("CT-1", "superseded", "C1", Some("CT-2"))]);
        let f = detect(&json!({"status":"doing","slug":"C1"}), &superseded);
        assert_eq!(f["kind"], json!("no_contract_for_doing_card"));
    }

    #[test]
    fn no_contract_fires_with_both_fix_options() {
        let db = db_with(&[]);
        let f = detect(&json!({"status":"doing","slug":"CARD-7","body_json":"{\"kind\":\"feat\"}"}), &db);
        assert_eq!(f["detail"], json!({"card_status":"doing"}));
        assert_eq!(f["fix_options"][0]["action"], json!("pl_session_start"));
        assert_eq!(f["fix_options"][0]["card_slug"], json!("CARD-7"));
        assert_eq!(f["fix_options"][1]["status"], json!("backlog"));
    }

    #[test]
    fn unparseable_body_json_treated_as_no_body() {
        let db = db_with(&[]);
        let f = detect(&json!({"status":"doing","slug":"C2","body_json":"{not json"}), &db);
        assert_eq!(f["kind"], json!("no_contract_for_doing_card"));
    }
}
