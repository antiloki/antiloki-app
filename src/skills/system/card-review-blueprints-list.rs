//! card-review-blueprints-list.rs — twin of skills/system/card-review-blueprints-list.mjs (db READ).
//! NAMES ARE CONTRACT: `cardReviewBlueprintsList` (named default). Latest blueprint per card, newest first.
// QUALITY: 81/100 — correlated latest-per-card subquery, optional verdict filter, failing-count over JSON
use rusqlite::Connection;
use serde_json::{json, Value};

fn count_arr(v: Option<&Value>) -> Vec<Value> {
    match v.and_then(|x| x.as_str()) {
        Some(s) => serde_json::from_str::<Value>(s).ok().and_then(|x| x.as_array().cloned()).unwrap_or_default(),
        None => vec![],
    }
}

pub fn cardReviewBlueprintsList(input: &Value, db: &Connection) -> Result<Value, String> {
    let limit = (input.get("limit").and_then(|v| v.as_f64()).unwrap_or(200.0) as i64).clamp(1, 500);
    let verdict = input.get("verdict").filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false)).cloned();

    let and_clause = if verdict.is_some() { "AND b.verdict = ?" } else { "" };
    let sql = format!(
        "SELECT b.* FROM card_review_blueprints b \
         WHERE b.id = ( SELECT id FROM card_review_blueprints b2 WHERE b2.card_slug = b.card_slug ORDER BY generated_at DESC, id DESC LIMIT 1 ) \
         {and_clause} ORDER BY b.generated_at DESC, b.id DESC LIMIT ?"
    );
    let mut args: Vec<Value> = Vec::new();
    if let Some(v) = &verdict { args.push(v.clone()); }
    args.push(json!(limit));
    let rows = crate::db_json::query_rows(db, &sql, &args)?;

    let reviews: Vec<Value> = rows.as_array().unwrap().iter().map(|r| {
        let o = r.as_object().unwrap();
        let g = |k: &str| o.get(k).cloned().unwrap_or(Value::Null);
        let anchors = count_arr(o.get("anchors_json"));
        let behavioral = count_arr(o.get("behavioral_json"));
        let failing = anchors.iter().chain(behavioral.iter())
            .filter(|a| a.is_object() && a.get("passed") == Some(&Value::Bool(false))).count();
        json!({
            "card_slug": g("card_slug"), "contract_slug": g("contract_slug"), "attempt": g("attempt"),
            "verdict": g("verdict"), "generated_at": g("generated_at"), "generated_by": g("generated_by"),
            "qa_run_id": g("qa_run_id"), "anchored": anchors.len(), "behavioral": behavioral.len(), "failing": failing,
        })
    }).collect();
    Ok(json!({ "reviews": reviews }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn latest_per_card_with_failing_count() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE card_review_blueprints (id INTEGER PRIMARY KEY, card_slug TEXT, contract_slug TEXT, attempt INTEGER, verdict TEXT, generated_at TEXT, generated_by TEXT, qa_run_id TEXT, anchors_json TEXT, behavioral_json TEXT); \
          INSERT INTO card_review_blueprints (id,card_slug,verdict,generated_at,anchors_json,behavioral_json) VALUES \
            (1,'C','pass','2026-01-01','[{\"passed\":true}]','[]'), \
            (2,'C','fail','2026-02-01','[{\"passed\":false}]','[{\"passed\":true}]');").unwrap();
        let r = cardReviewBlueprintsList(&json!({}), &c).unwrap();
        assert_eq!(r["reviews"].as_array().unwrap().len(), 1); // latest per card
        assert_eq!(r["reviews"][0]["verdict"], json!("fail"));
        assert_eq!(r["reviews"][0]["failing"], json!(1));
    }
}
