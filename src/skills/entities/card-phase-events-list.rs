//! card-phase-events-list.rs — twin of skills/entities/card-phase-events-list.mjs (db lane).
//! NAMES ARE CONTRACT: `cardPhaseEventsList` (named default). List + per-row evidence parse.
// QUALITY: 85/100 — filter-building + limit clamp + the evidence JSON-column map
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn cardPhaseEventsList(input: &Value, db: &Connection) -> Result<Value, String> {
    let mut sql = String::from("SELECT id, card_slug, phase, actor, ts, evidence_json FROM card_phase_events WHERE 1=1");
    let mut params: Vec<Value> = Vec::new();
    for key in ["card_slug", "phase", "actor"] {
        if let Some(v) = input.get(key).and_then(|x| x.as_str()).filter(|s| !s.is_empty()) {
            sql.push_str(&format!(" AND {key} = ?"));
            params.push(json!(v));
        }
    }
    sql.push_str(" ORDER BY ts DESC, id DESC LIMIT ?");
    // JS: Math.min(Math.max(1, Number(limit) || 100), 1000)
    let raw = input.get("limit").and_then(|v| v.as_f64()).filter(|n| *n != 0.0 && !n.is_nan()).unwrap_or(100.0);
    params.push(json!(raw.max(1.0).min(1000.0) as i64));

    let rows = crate::db_json::query_rows(db, &sql, &params)?;
    let mapped: Vec<Value> = rows.as_array().unwrap().iter().map(|r| {
        let mut o = r.as_object().unwrap().clone();
        // JS: evidence = r.evidence_json ? (try JSON.parse : raw string) : null
        let evidence = match o.get("evidence_json") {
            Some(Value::String(s)) if !s.is_empty() => serde_json::from_str::<Value>(s).unwrap_or_else(|_| json!(s)),
            _ => Value::Null,
        };
        o.insert("evidence".into(), evidence);
        Value::Object(o)
    }).collect();
    Ok(json!(mapped))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE card_phase_events (id INTEGER, card_slug TEXT, phase TEXT, actor TEXT, ts TEXT, evidence_json TEXT);
            INSERT INTO card_phase_events VALUES (1,'C1','plan','dev','t1','{\"k\":1}'),(2,'C1','build','qa','t2',NULL);").unwrap();
        c
    }
    #[test]
    fn lists_parses_evidence_and_filters() {
        let all = cardPhaseEventsList(&json!({ "card_slug": "C1" }), &seeded()).unwrap();
        assert_eq!(all.as_array().unwrap().len(), 2);
        // ts DESC → id 2 first
        assert_eq!(all[0]["evidence"], Value::Null);
        assert_eq!(all[1]["evidence"], json!({ "k": 1 }));
        assert_eq!(cardPhaseEventsList(&json!({ "phase": "plan" }), &seeded()).unwrap().as_array().unwrap().len(), 1);
    }
}
