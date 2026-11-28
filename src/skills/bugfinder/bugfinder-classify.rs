//! bugfinder-classify.rs — twin of skills/bugfinder/bugfinder-classify.mjs (db WRITE lane,
//! async-over-sync). NAMES ARE CONTRACT: `bugfinderClassify` (default). The one-click feedback
//! loop (the tagging doctrine: the system surfaces, the operator classifies with ONE click,
//! never a form) — classifications feed the precision ledger, the only path from advisory to
//! gating, per category, from evidence instead of taste.
// QUALITY: 84/100 — self-heals via the ensure-twin, then UPDATE + changes==0 -> NOT_FOUND

use rusqlite::{params, Connection};
use serde_json::{json, Value};

const CLASSES: [&str; 3] = ["real", "false-positive", "known"];

// JS Number(v)
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) }
        }
        Some(_) => f64::NAN,
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
// JS `${v}` interpolation (used in the enum-validation error message).
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

pub fn bugfinderClassify(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `const db = context?.db; if (!db) throw err.required(...)` — the db lane always injects
    // a live connection, so that guard is unreachable here (Rust's signature takes `db: &Connection`,
    // never absent).
    let id_f = js_number(input.get("finding_id"));
    // JS `Number.isInteger(id)` — NaN/Infinity/fractional values all fail this.
    if !id_f.is_finite() || id_f.fract() != 0.0 {
        return Err("bugfinder-classify: finding_id is required".to_string());
    }
    let id = id_f as i64;
    let cls = nz(input.get("class")).cloned(); // `input?.class ?? null`
    if let Some(c) = &cls {
        let c_str = c.as_str().unwrap_or("");
        if !CLASSES.contains(&c_str) {
            return Err(format!(
                "bugfinder-classify: class must be one of: {} (got '{}')",
                CLASSES.join(", "),
                tmpl(Some(c))
            ));
        }
    }
    crate::bugfinder__helpers_schema::ensureBugfinderSchema(db);

    let cls_param = cls.as_ref().and_then(|v| v.as_str());
    let changed = db
        .execute(
            "UPDATE judgments.bugfinder_findings SET operator_class = ? WHERE id = ?",
            params![cls_param, id],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err(format!("bugfinder-classify: finding {id} (not found) is required"));
    }
    Ok(json!({ "ok": true, "finding_id": id, "operator_class": cls.unwrap_or(Value::Null) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS judgments").unwrap();
        c
    }

    fn seed_finding(c: &Connection, id: i64, operator_class: Option<&str>) {
        let _ = crate::bugfinder__helpers_schema::ensureBugfinderSchema(c);
        c.execute(
            "INSERT INTO judgments.bugfinder_findings (id, run_id, wave, category, detail, operator_class) VALUES (?, 1, 1, 'lint', 'issue', ?)",
            params![id, operator_class],
        )
        .unwrap();
    }

    #[test]
    fn classifies_a_finding_real() {
        let c = conn();
        seed_finding(&c, 1, None);
        let r = bugfinderClassify(&json!({ "finding_id": 1, "class": "real" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "finding_id": 1, "operator_class": "real" }));
        let row = crate::db_json::query_rows(&c, "SELECT operator_class FROM judgments.bugfinder_findings WHERE id=1", &[]).unwrap();
        assert_eq!(row[0]["operator_class"], json!("real"));
    }

    #[test]
    fn null_class_clears_a_mis_click() {
        let c = conn();
        seed_finding(&c, 2, Some("known"));
        let r = bugfinderClassify(&json!({ "finding_id": 2, "class": null }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "finding_id": 2, "operator_class": Value::Null }));
        let row = crate::db_json::query_rows(&c, "SELECT operator_class FROM judgments.bugfinder_findings WHERE id=2", &[]).unwrap();
        assert_eq!(row[0]["operator_class"], Value::Null);
    }

    #[test]
    fn class_absent_from_input_behaves_like_null() {
        let c = conn();
        seed_finding(&c, 3, Some("known"));
        let r = bugfinderClassify(&json!({ "finding_id": 3 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "finding_id": 3, "operator_class": Value::Null }));
    }

    #[test]
    fn rejects_bad_class_bad_id_and_not_found() {
        let c = conn();
        seed_finding(&c, 4, None);
        assert!(bugfinderClassify(&json!({ "finding_id": 4, "class": "bogus" }), &c).unwrap_err().contains("class must be one of"));
        assert!(bugfinderClassify(&json!({ "finding_id": "nope", "class": "real" }), &c).unwrap_err().contains("finding_id is required"));
        assert!(bugfinderClassify(&json!({ "finding_id": 1.5, "class": "real" }), &c).unwrap_err().contains("finding_id is required"));
        assert!(bugfinderClassify(&json!({ "finding_id": 999, "class": "real" }), &c).unwrap_err().contains("finding 999 (not found)"));
    }
}
