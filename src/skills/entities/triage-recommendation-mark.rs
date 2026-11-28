//! triage-recommendation-mark.rs — twin of skills/entities/triage-recommendation-mark.mjs (db WRITE
//! lane). NAMES ARE CONTRACT: `triageRecommendationMark` (default). Records an operator decision on a
//! Triage advisory: INSERT + { id (lastInsertRowid), rec_id, state, expires_at }.
// QUALITY: frozen-clock lane — Date.now() is pinned to 2026-01-01 00:00:00 (epoch ms 1767225600000);
// the 'note' expiry is that instant + expiry_days, formatted like new Date(ms).toISOString().slice(0,19).
use rusqlite::Connection;
use serde_json::{json, Value};

// JS truthiness (used for the `!rec_id` guard).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS Number(v) coercion (used for `expiry_days * 86400_000`).
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}

// Howard Hinnant's civil-from-days: days since 1970-01-01 (UTC) → (year, month, day).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// = new Date(FROZEN_NOW_MS + expiry_days*86400000).toISOString().slice(0,19).replace('T',' ')
// FROZEN_NOW_MS is the harness's pinned instant (2026-01-01T00:00:00.000Z).
fn frozen_expires(expiry_days: f64) -> String {
    let ms = (crate::clock::now_ms() as f64 + expiry_days * 86400000.0_f64).trunc() as i64;
    let total_secs = ms.div_euclid(1000);
    let days = total_secs.div_euclid(86400);
    let sod = total_secs.rem_euclid(86400);
    let (h, mi, s) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, mo, d, h, mi, s)
}

const SQL: &str = "INSERT INTO triage_recommendation_states (rec_id, state, marked_by, expires_at, note_text) VALUES (?, ?, ?, ?, ?)";

pub fn triageRecommendationMark(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();
    let get = |k: &str| obj.and_then(|o| o.get(k));
    let rec_id = get("rec_id");
    // JS also guards `if (!db) throw` and `if (!rec_id) throw` — db is always present in this lane.
    if !truthy(rec_id) {
        return Err("triage-recommendation-mark: rec_id is required".into());
    }
    let state = get("state");
    let state_str = state.and_then(|v| v.as_str());
    if state_str != Some("note") && state_str != Some("schedule") {
        return Err(format!("state must be one of note|schedule; got '{}'", state_str.unwrap_or("")));
    }
    // JS destructuring defaults apply only when the key is ABSENT (undefined), not when null.
    let expiry_days = match get("expiry_days") { None => 7.0, Some(v) => js_number(Some(v)) };
    let marked_by = match get("marked_by") { None => Value::String("human:tui-operator".into()), Some(v) => v.clone() };
    let note_text = match get("note_text") { None => Value::Null, Some(v) => v.clone() };

    let expires_val = if state_str == Some("note") {
        Value::String(frozen_expires(expiry_days))
    } else {
        Value::Null
    };

    let params = vec![
        rec_id.cloned().unwrap_or(Value::Null),
        state.cloned().unwrap_or(Value::Null),
        marked_by,
        expires_val.clone(),
        note_text,
    ];
    let (_changes, last_id) = crate::db_json::run_write(db, SQL, &params)?;

    Ok(json!({
        "id": last_id,
        "rec_id": rec_id.cloned().unwrap_or(Value::Null),
        "state": state.cloned().unwrap_or(Value::Null),
        "expires_at": expires_val,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    // Frozen the marked_at DEFAULT the same way the fixture schema does — deterministic across lanes.
    const SCHEMA: &str = "CREATE TABLE triage_recommendation_states ( id INTEGER PRIMARY KEY AUTOINCREMENT, rec_id TEXT NOT NULL, state TEXT NOT NULL CHECK (state IN ('note', 'schedule')), marked_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), marked_by TEXT NOT NULL, expires_at TEXT, note_text TEXT )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    #[test]
    fn note_stamps_expiry_from_frozen_clock() {
        let c = db();
        let r = triageRecommendationMark(&json!({ "rec_id": "adv:foo", "state": "note", "expiry_days": 7 }), &c).unwrap();
        assert_eq!(r, json!({ "id": 1, "rec_id": "adv:foo", "state": "note", "expires_at": "2026-01-08 00:00:00" }));
        // default marked_by + null note_text landed in the row
        let (mb, nt): (String, Option<String>) = c
            .query_row("SELECT marked_by, note_text FROM triage_recommendation_states WHERE id=1", [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!(mb, "human:tui-operator");
        assert_eq!(nt, None);
    }

    #[test]
    fn note_default_expiry_days_is_seven() {
        let c = db();
        let r = triageRecommendationMark(&json!({ "rec_id": "adv:def", "state": "note" }), &c).unwrap();
        assert_eq!(r["expires_at"], json!("2026-01-08 00:00:00"));
    }

    #[test]
    fn note_custom_expiry_and_fields() {
        let c = db();
        let r = triageRecommendationMark(&json!({ "rec_id": "adv:baz", "state": "note", "expiry_days": 30, "marked_by": "human:custom", "note_text": "remember this" }), &c).unwrap();
        assert_eq!(r["expires_at"], json!("2026-01-31 00:00:00"));
        let nt: String = c.query_row("SELECT note_text FROM triage_recommendation_states WHERE id=1", [], |row| row.get(0)).unwrap();
        assert_eq!(nt, "remember this");
    }

    #[test]
    fn schedule_has_null_expiry() {
        let c = db();
        let r = triageRecommendationMark(&json!({ "rec_id": "adv:bar", "state": "schedule" }), &c).unwrap();
        assert_eq!(r, json!({ "id": 1, "rec_id": "adv:bar", "state": "schedule", "expires_at": null }));
    }

    #[test]
    fn autoincrement_continues_from_seed() {
        let c = db();
        c.execute("INSERT INTO triage_recommendation_states (id, rec_id, state, marked_by) VALUES (5, 'seed:x', 'note', 'seed')", []).unwrap();
        let r = triageRecommendationMark(&json!({ "rec_id": "adv:next", "state": "schedule" }), &c).unwrap();
        assert_eq!(r["id"], json!(6));
    }

    #[test]
    fn rejects_bad_input() {
        let c = db();
        assert!(triageRecommendationMark(&json!({ "state": "note" }), &c).is_err()); // no rec_id
        assert!(triageRecommendationMark(&json!({ "rec_id": "X", "state": "banana" }), &c).is_err()); // bad state
    }
}
