//! card-body-enrich-persist.rs — twin of skills/system/card-body-enrich-persist.mjs (db WRITE
//! lane; dispatcher composing the entities_card_update, entities_card_accuracy_score and
//! entities_card_score twins). NAMES ARE CONTRACT: `cardBodyEnrichPersist` (default). Operator's
//! per-proposal accept/reject/edit decision; on accept/edit writes through card-update (audited),
//! then best-effort re-scores accuracy + P×R×C metrics.
// QUALITY: decision enum guard, already-decided guard, best-effort re-score (failures swallowed,
// never roll back the accepted body) — mirrors card-accuracy-score.rs / card-score.rs's
// PARITY-DEFERRED convention: their aiCall stub always Err, so accuracy/metrics land None here.
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// JS template-literal interpolation of a primitive value (no surrounding quotes for strings).
fn js_display(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }
}

#[allow(non_snake_case)]
pub fn cardBodyEnrichPersist(input: &Value, db: &Connection) -> Result<Value, String> {
    let proposal_id = input.get("proposal_id").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&proposal_id)) {
        return Err("card-body-enrich-persist: proposal_id is required".into());
    }
    let decision = input.get("decision").and_then(Value::as_str).unwrap_or("");
    if !["accepted", "rejected", "edited"].contains(&decision) {
        return Err(format!(
            "card-body-enrich-persist: decision must be one of: accepted, rejected, edited (got '{decision}')"
        ));
    }

    let rows = crate::db_json::query_rows(
        db,
        "SELECT * FROM card_enrichment_proposals WHERE id = ?",
        &[proposal_id.clone()],
    )?;
    let Some(prop) = rows.as_array().and_then(|a| a.first()).cloned() else {
        return Err(format!(
            "card-body-enrich-persist: proposal '{}' not found",
            js_display(&proposal_id)
        ));
    };

    if truthy(prop.get("decided_at")) {
        let prop_decision = prop.get("decision").cloned().unwrap_or(Value::Null);
        return Err(format!(
            "card-body-enrich-persist: proposal {} already decided as {}",
            js_display(&proposal_id),
            js_display(&prop_decision)
        ));
    }

    let mut final_body: Option<Value> = None;
    if decision == "accepted" {
        let raw = prop.get("proposed_body_json").and_then(Value::as_str).unwrap_or("null");
        let parsed: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        final_body = Some(parsed);
    } else if decision == "edited" {
        let edited_body = input.get("edited_body");
        let ok = matches!(edited_body, Some(v) if v.is_object());
        if !ok {
            return Err(
                "card-body-enrich-persist: edited_body (object) when decision=edited is required".into(),
            );
        }
        final_body = edited_body.cloned();
    }

    let card_slug = prop.get("card_slug").cloned().unwrap_or(Value::Null);

    // Write through card-update (composes with-audit + fires schema law-hook in the JS twin;
    // this Rust twin performs the equivalent audited write via entities_card_update).
    if let Some(body) = &final_body {
        let up_input = json!({ "slug": card_slug.clone(), "body": body });
        crate::entities_card_update::cardUpdate(&up_input, db)?;
    }

    let accepted_body_json = final_body
        .as_ref()
        .map(|b| serde_json::to_string(b).map_err(|e| e.to_string()))
        .transpose()?;
    crate::db_json::run_write(
        db,
        &format!("UPDATE card_enrichment_proposals SET decided_at = {}, decision = ?, accepted_body_json = ? WHERE id = ?", crate::clock::now_sql()),
        &[
            json!(decision),
            accepted_body_json.map(Value::String).unwrap_or(Value::Null),
            proposal_id.clone(),
        ],
    )?;

    // Regenerating a field/body invalidates the accuracy score — recompute it best-effort. A
    // failed recompute (e.g. AI provider error — always the case via the deferred aiCall stub)
    // must not roll back the already-committed + audited body write.
    let mut accuracy: Value = Value::Null;
    if let Some(_body) = &final_body {
        let acc_input = json!({ "slug": card_slug.clone() });
        if let Ok(acc) = crate::entities_card_accuracy_score::cardAccuracyScore(&acc_input, db) {
            accuracy = acc.get("accuracy_score").cloned().unwrap_or(Value::Null);
        }
    }

    // Likewise the METRICS (priority/risk/complexity → P×R×C score) — best-effort, after the
    // audited body write.
    let mut metrics: Value = Value::Null;
    if let Some(_body) = &final_body {
        let score_input = json!({
            "slug": card_slug.clone(),
            "reason": "card body regenerated \u{2014} auto re-score",
        });
        if let Ok(sc) = crate::entities_card_score::cardScore(&score_input, db) {
            if truthy(Some(&sc)) {
                metrics = json!({
                    "risk": sc.get("risk").cloned().unwrap_or(Value::Null),
                    "complexity": sc.get("complexity").cloned().unwrap_or(Value::Null),
                    "priority": sc.get("priority").cloned().unwrap_or(Value::Null),
                    "score": sc.get("score").cloned().unwrap_or(Value::Null),
                });
            }
        }
    }

    Ok(json!({
        "ok": true,
        "proposal_id": proposal_id,
        "card_slug": card_slug,
        "decision": decision,
        "new_body": final_body.unwrap_or(Value::Null),
        "accuracy_score": accuracy,
        "metrics": metrics,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, body_json TEXT, parent_slug TEXT, status TEXT, updated_at TEXT); \
             CREATE TABLE card_enrichment_proposals (id INTEGER PRIMARY KEY, card_slug TEXT, proposed_body_json TEXT, decided_at TEXT, decision TEXT, accepted_body_json TEXT); \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, digital_print TEXT, created_at TEXT);",
        ).unwrap();
        c.execute("INSERT INTO cards VALUES ('E-1','t','{\"kind\":\"chore\"}',NULL,'backlog','2025-12-01 00:00:00')", []).unwrap();
        c
    }

    #[test]
    fn accept_writes_body_and_marks_decided() {
        let c = db();
        c.execute(
            "INSERT INTO card_enrichment_proposals VALUES (1,'E-1',?,NULL,NULL,NULL)",
            params![json!({"kind":"feature","size":"m","why":"x"}).to_string()],
        ).unwrap();

        let r = cardBodyEnrichPersist(&json!({ "proposal_id": 1, "decision": "accepted" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["card_slug"], json!("E-1"));
        assert_eq!(r["new_body"], json!({"kind":"feature","size":"m","why":"x"}));
        assert_eq!(r["accuracy_score"], Value::Null);
        assert_eq!(r["metrics"], Value::Null);

        let body: String = c.query_row("SELECT body_json FROM cards WHERE slug='E-1'", [], |row| row.get(0)).unwrap();
        assert_eq!(body, "{\"kind\":\"feature\",\"size\":\"m\",\"why\":\"x\"}");

        let (decided_at, decision, accepted): (String, String, String) = c.query_row(
            "SELECT decided_at, decision, accepted_body_json FROM card_enrichment_proposals WHERE id=1",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(decided_at, "2026-01-01 00:00:00");
        assert_eq!(decision, "accepted");
        assert_eq!(accepted, "{\"kind\":\"feature\",\"size\":\"m\",\"why\":\"x\"}");
    }

    #[test]
    fn rejected_leaves_card_untouched() {
        let c = db();
        c.execute(
            "INSERT INTO card_enrichment_proposals VALUES (2,'E-1',?,NULL,NULL,NULL)",
            params![json!({"kind":"feature"}).to_string()],
        ).unwrap();

        let r = cardBodyEnrichPersist(&json!({ "proposal_id": 2, "decision": "rejected" }), &c).unwrap();
        assert_eq!(r["new_body"], Value::Null);
        let body: String = c.query_row("SELECT body_json FROM cards WHERE slug='E-1'", [], |row| row.get(0)).unwrap();
        assert_eq!(body, "{\"kind\":\"chore\"}");
        let (decision, accepted): (String, Option<String>) = c.query_row(
            "SELECT decision, accepted_body_json FROM card_enrichment_proposals WHERE id=2",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(decision, "rejected");
        assert_eq!(accepted, None);
    }

    #[test]
    fn edited_requires_object_body() {
        let c = db();
        c.execute(
            "INSERT INTO card_enrichment_proposals VALUES (3,'E-1',?,NULL,NULL,NULL)",
            params![json!({"kind":"feature"}).to_string()],
        ).unwrap();
        let err = cardBodyEnrichPersist(&json!({ "proposal_id": 3, "decision": "edited" }), &c).unwrap_err();
        assert!(err.contains("edited_body"));

        let r = cardBodyEnrichPersist(&json!({ "proposal_id": 3, "decision": "edited", "edited_body": {"kind":"bug","size":"s"} }), &c).unwrap();
        assert_eq!(r["new_body"], json!({"kind":"bug","size":"s"}));
    }

    #[test]
    fn already_decided_is_rejected() {
        let c = db();
        c.execute(
            "INSERT INTO card_enrichment_proposals VALUES (4,'E-1',?,'2026-01-01 00:00:00','accepted',?)",
            params![json!({"kind":"feature"}).to_string(), json!({"kind":"feature"}).to_string()],
        ).unwrap();
        let err = cardBodyEnrichPersist(&json!({ "proposal_id": 4, "decision": "accepted" }), &c).unwrap_err();
        assert!(err.contains("already decided as accepted"));
    }

    #[test]
    fn unknown_decision_and_missing_proposal_id_and_not_found_are_rejected() {
        let c = db();
        let e1 = cardBodyEnrichPersist(&json!({ "proposal_id": 1, "decision": "bogus" }), &c).unwrap_err();
        assert!(e1.contains("decision must be one of"));
        let e2 = cardBodyEnrichPersist(&json!({ "decision": "accepted" }), &c).unwrap_err();
        assert!(e2.contains("proposal_id is required"));
        let e3 = cardBodyEnrichPersist(&json!({ "proposal_id": 999, "decision": "accepted" }), &c).unwrap_err();
        assert!(e3.contains("not found"));
    }
}
