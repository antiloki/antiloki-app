//! atlas-report-latest.rs — twin of skills/codebase/atlas-report-latest.mjs (db lane). NAMES ARE
//! CONTRACT: `atlasReportLatest` (named default). Trend (reversed) + latest report row.
// QUALITY: 84/100 — safe() JSON parse + nested optional access + reverse; report-or-null
use rusqlite::Connection;
use serde_json::{json, Value};

fn safe(v: Option<&Value>) -> Value {
    match v {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}
fn total_or0(counts: &Value, key: &str) -> Value {
    counts.get(key).and_then(|v| v.get("total")).filter(|v| !v.is_null()).cloned().unwrap_or(json!(0))
}

pub fn atlasReportLatest(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(|v| v.as_str()).unwrap_or("atlas");
    let n = input.get("trend_limit").and_then(|v| v.as_f64()).unwrap_or(20.0);
    let limit = n.max(1.0).min(200.0) as i64;
    let trend_rows = crate::db_json::query_rows(db,
        "SELECT health_score AS health, risk_score AS risk, counts_json, created_at FROM code_reports WHERE origin = ? ORDER BY id DESC LIMIT ?",
        &[json!(origin), json!(limit)])?;
    let mut trend: Vec<Value> = trend_rows.as_array().unwrap().iter().map(|r| {
        let o = r.as_object().unwrap();
        let counts = safe(o.get("counts_json"));
        json!({ "health": o["health"].clone(), "risk": o["risk"].clone(),
            "smells": total_or0(&counts, "smells"), "vulns": total_or0(&counts, "vulns"),
            "created_at": o["created_at"].clone() })
    }).collect();
    trend.reverse();

    let rowq = crate::db_json::query_rows(db,
        "SELECT id, origin, health_score, risk_score, scores_json, smells_json, vulns_json, review_md, counts_json, created_at FROM code_reports WHERE origin = ? ORDER BY id DESC LIMIT 1",
        &[json!(origin)])?;
    let arr = rowq.as_array().unwrap();
    if arr.is_empty() {
        return Ok(json!({ "report": null, "trend": trend }));
    }
    let o = arr[0].as_object().unwrap();
    let smells = { let s = safe(o.get("smells_json")); if s.is_null() { json!([]) } else { s } };
    Ok(json!({
        "report_id": o["id"].clone(), "origin": o["origin"].clone(),
        "project": { "health": o["health_score"].clone(), "risk": o["risk_score"].clone() },
        "scores": safe(o.get("scores_json")), "smells": smells, "vulns": safe(o.get("vulns_json")),
        "review_md": o.get("review_md").cloned().unwrap_or(Value::Null),
        "counts": safe(o.get("counts_json")), "created_at": o["created_at"].clone(), "trend": trend,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_and_trend_or_null() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_reports (id INTEGER, origin TEXT, health_score REAL, risk_score REAL, scores_json TEXT, smells_json TEXT, vulns_json TEXT, review_md TEXT, counts_json TEXT, created_at TEXT);").unwrap();
        assert_eq!(atlasReportLatest(&json!({}), &c).unwrap()["report"], Value::Null);
        c.execute_batch("INSERT INTO code_reports VALUES (1,'atlas',80,20,'{\"a\":1}',NULL,NULL,NULL,'{\"smells\":{\"total\":3}}','t1');").unwrap();
        let r = atlasReportLatest(&json!({}), &c).unwrap();
        assert_eq!(r["report_id"], json!(1));
        assert_eq!(r["smells"], json!([])); // null smells_json → []
        assert_eq!(r["trend"][0]["smells"], json!(3));
    }
}
