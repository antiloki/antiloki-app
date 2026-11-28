//! bugfinder-schedule.rs — twin of skills/bugfinder/bugfinder-schedule.mjs (db lane, cross-db
//! judgments.*). NAMES ARE CONTRACT: default `bugfinderSchedule`. The Ultra Review scheduler:
//! recurring bug-finder runs over a saved scope (folder · files · diff base), daily or weekly.
//! Execution is operator-visible by design: 'run' fires one schedule now; 'run-due' fires every
//! enabled schedule past its due time.
//!
//! BESPOKE-NATIVE — dispatches to `bugfinder_bugfinder_run::bugfinderRun` (itself bespoke-native:
//! real fs directory walk + `git` subprocess + AI-shard/sweep waves behind a deferred aiCall
//! boundary, same class as book-run.rs / cockpit-pane-message.rs). This twin reuses that twin
//! rather than reimplementing it, per the dispatcher precedent (cockpit-pane-message.rs).
//!
//! Frozen-clock lane: explicit `datetime('now')` writes (last_run_at) pin to the frozen literal
//! `datetime('2026-01-01 00:00:00')`, matching the god-card-driver-intent-set.rs /
//! dev-consume-qa-suggestions.rs convention for non-DEFAULT datetime('now') call sites.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

const CADENCES: [&str; 3] = ["daily", "weekly", "manual"];

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

// Number.isInteger(v): Some(int) if v is a JSON number with no fractional part; else None.
fn js_integer(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Number(n)) => {
            let f = n.as_f64()?;
            if f.is_finite() && f.fract() == 0.0 { Some(f as i64) } else { None }
        }
        _ => None,
    }
}

fn get_schedule(db: &Connection, id: i64) -> Result<Value, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT * FROM judgments.bugfinder_schedules WHERE id = ?",
        &[json!(id)],
    )?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

fn run_schedule(db: &Connection, s: &Value) -> Result<Value, String> {
    let scope_json_str = str_qq(s.get("scope_json"));
    let scope: Value = Some(scope_json_str.as_str()).pipe_parse();
    let mut input = Map::new();
    if let Some(files) = scope.get("files").and_then(Value::as_array) {
        if !files.is_empty() { input.insert("files".into(), json!(files)); }
    }
    if let Some(folder) = scope.get("folder").and_then(Value::as_str) {
        if !folder.is_empty() { input.insert("folder".into(), json!(folder)); }
    }
    if let Some(base) = scope.get("base").and_then(Value::as_str) {
        if !base.is_empty() { input.insert("base".into(), json!(base)); }
    }
    if let Some(shards) = scope.get("shards").and_then(Value::as_array) {
        input.insert("shards".into(), json!(shards));
    }
    if let Some(skip) = scope.get("skip").and_then(Value::as_array) {
        input.insert("skip".into(), json!(skip));
    }
    if let Some(max_files) = js_integer(scope.get("max_files")) {
        input.insert("max_files".into(), json!(max_files));
    }
    if let Some(root) = scope.get("root").and_then(Value::as_str) {
        if !root.is_empty() { input.insert("root".into(), json!(root)); }
    }
    input.insert("source".into(), json!("ultra"));

    let run = crate::bugfinder_bugfinder_run::bugfinderRun(&Value::Object(input), db)?;
    crate::db_json::run_write(
        db,
        &format!("UPDATE judgments.bugfinder_schedules SET last_run_at = {}, last_run_id = ? WHERE id = ?", crate::clock::now_sql()),
        &[run.get("run_id").cloned().unwrap_or(Value::Null), s.get("id").cloned().unwrap_or(Value::Null)],
    )?;
    Ok(run)
}

// small local trait so `str.pipe_parse()` reads like the JS `JSON.parse(... ?? '{}')` fallback.
trait ParseOrEmptyObj { fn pipe_parse(self) -> Value; }
impl ParseOrEmptyObj for Option<&str> {
    fn pipe_parse(self) -> Value {
        let s = self.filter(|s| !s.is_empty()).unwrap_or("{}");
        serde_json::from_str(s).unwrap_or_else(|_| json!({}))
    }
}

/// `export default async function bugfinderSchedule(input, context)`.
pub fn bugfinderSchedule(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `const db = context?.db; if (!db) throw err.required(...)` — unreachable here
    // (`db: &Connection` is never absent), same convention as sibling db-lane twins.
    crate::bugfinder__helpers_schema::ensureBugfinderSchema(db);
    let op = str_qq(input.get("op"));

    if op == "save" {
        let name: String = str_qq(input.get("name")).trim().chars().take(80).collect();
        if name.is_empty() { return Err("bugfinder-schedule: name required".to_string()); }
        let cadence = input.get("cadence").and_then(Value::as_str)
            .filter(|c| CADENCES.contains(c)).unwrap_or("weekly").to_string();
        let hour = match js_integer(input.get("hour_utc")) {
            Some(h) if (0..=23).contains(&h) => h,
            _ => 6,
        };
        let enabled: i64 = if matches!(input.get("enabled"), Some(Value::Bool(false))) { 0 } else { 1 };
        let scope_obj = match input.get("scope") {
            Some(v) if !v.is_null() && (v.is_object() || v.is_array()) => v.clone(),
            _ => json!({}),
        };
        let scope_json = scope_obj.to_string();
        let existing_id = js_integer(input.get("id"));
        let id = match existing_id {
            Some(candidate) if !get_schedule(db, candidate)?.is_null() => {
                crate::db_json::run_write(
                    db,
                    "UPDATE judgments.bugfinder_schedules SET name = ?, scope_json = ?, cadence = ?, hour_utc = ?, enabled = ? WHERE id = ?",
                    &[json!(name), json!(scope_json), json!(cadence), json!(hour), json!(enabled), json!(candidate)],
                )?;
                candidate
            }
            _ => {
                let (_, rowid) = crate::db_json::run_write(
                    db,
                    "INSERT INTO judgments.bugfinder_schedules (name, scope_json, cadence, hour_utc, enabled) VALUES (?, ?, ?, ?, ?)",
                    &[json!(name), json!(scope_json), json!(cadence), json!(hour), json!(enabled)],
                )?;
                rowid
            }
        };
        return Ok(json!({ "schedule": get_schedule(db, id)? }));
    }

    if op == "delete" {
        let id = js_integer(input.get("id")).ok_or_else(|| "bugfinder-schedule: id required".to_string())?;
        crate::db_json::run_write(db, "DELETE FROM judgments.bugfinder_schedules WHERE id = ?", &[json!(id)])?;
        return Ok(json!({ "ok": true, "id": id }));
    }

    if op == "run" {
        let s = match js_integer(input.get("id")) {
            Some(id) => get_schedule(db, id)?,
            None => Value::Null,
        };
        if s.is_null() {
            return Err(format!("bugfinder-schedule: schedule {} (not found)", str_qq(input.get("id"))));
        }
        let run = run_schedule(db, &s)?;
        return Ok(json!({ "schedule_id": s.get("id").cloned().unwrap_or(Value::Null), "run": run }));
    }

    if op == "run-due" {
        let all = crate::db_json::query_rows(
            db,
            "SELECT * FROM judgments.bugfinder_schedules WHERE enabled = 1",
            &[],
        )?;
        let due: Vec<Value> = all.as_array().cloned().unwrap_or_default()
            .into_iter()
            .filter(|s| matches!(crate::bugfinder__helpers_schedule_math::isOverdue(s, &Value::Null), Value::Bool(true)))
            .collect();
        let mut ran = Vec::new();
        for s in due {
            match run_schedule(db, &s) {
                Ok(run) => {
                    ran.push(json!({
                        "schedule_id": s.get("id").cloned().unwrap_or(Value::Null),
                        "name": s.get("name").cloned().unwrap_or(Value::Null),
                        "run_id": run.get("run_id").cloned().unwrap_or(Value::Null),
                        "kept": run.get("counts").and_then(|c| c.get("kept")).cloned().unwrap_or(Value::Null),
                        "cost_usd": run.get("cost_usd").cloned().unwrap_or(Value::Null),
                    }));
                }
                Err(e) => {
                    let msg: String = e.chars().take(200).collect();
                    ran.push(json!({
                        "schedule_id": s.get("id").cloned().unwrap_or(Value::Null),
                        "name": s.get("name").cloned().unwrap_or(Value::Null),
                        "error": msg,
                    }));
                }
            }
        }
        return Ok(json!({ "ran": ran }));
    }

    Err(format!("bugfinder-schedule: op must be one of save, delete, run, run-due (got '{op}')"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ensureBugfinderSchema creates the tables, so the fixture only needs the attached db.
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS judgments;").unwrap();
        c
    }

    #[test]
    fn save_inserts_with_documented_defaults() {
        let c = db();
        let r = bugfinderSchedule(&json!({ "op": "save", "name": "nightly" }), &c).unwrap();
        let s = &r["schedule"];
        assert_eq!(s["name"], json!("nightly"));
        assert_eq!(s["cadence"], json!("weekly"), "cadence defaults to weekly");
        assert_eq!(s["hour_utc"], json!(6), "hour defaults to 6");
        assert_eq!(s["enabled"], json!(1), "enabled unless explicitly false");
        assert_eq!(s["scope_json"], json!("{}"), "absent scope becomes an empty object");
    }

    #[test]
    fn save_trims_and_caps_the_name_at_80_chars() {
        let c = db();
        let long = "x".repeat(200);
        let r = bugfinderSchedule(&json!({ "op": "save", "name": format!("  {long}  ") }), &c).unwrap();
        assert_eq!(r["schedule"]["name"].as_str().unwrap().len(), 80);
    }

    #[test]
    fn save_requires_a_non_empty_name() {
        let c = db();
        assert!(bugfinderSchedule(&json!({ "op": "save" }), &c).unwrap_err().contains("name required"));
        assert!(bugfinderSchedule(&json!({ "op": "save", "name": "   " }), &c).unwrap_err().contains("name required"));
    }

    #[test]
    fn cadence_and_hour_fall_back_when_out_of_range() {
        let c = db();
        let r = bugfinderSchedule(&json!({ "op": "save", "name": "n", "cadence": "hourly", "hour_utc": 99 }), &c).unwrap();
        assert_eq!(r["schedule"]["cadence"], json!("weekly"), "unknown cadence -> weekly");
        assert_eq!(r["schedule"]["hour_utc"], json!(6), "out-of-range hour -> 6");
        let ok = bugfinderSchedule(&json!({ "op": "save", "name": "n2", "cadence": "daily", "hour_utc": 0 }), &c).unwrap();
        assert_eq!(ok["schedule"]["cadence"], json!("daily"));
        assert_eq!(ok["schedule"]["hour_utc"], json!(0), "0 is in range, not falsy-defaulted");
    }

    #[test]
    fn enabled_false_is_the_only_way_to_disable() {
        let c = db();
        let off = bugfinderSchedule(&json!({ "op": "save", "name": "off", "enabled": false }), &c).unwrap();
        assert_eq!(off["schedule"]["enabled"], json!(0));
        let on = bugfinderSchedule(&json!({ "op": "save", "name": "on", "enabled": Value::Null }), &c).unwrap();
        assert_eq!(on["schedule"]["enabled"], json!(1), "null is not false");
    }

    #[test]
    fn save_with_an_existing_id_updates_instead_of_inserting() {
        let c = db();
        let first = bugfinderSchedule(&json!({ "op": "save", "name": "a" }), &c).unwrap();
        let id = first["schedule"]["id"].clone();
        let second = bugfinderSchedule(&json!({ "op": "save", "id": id, "name": "renamed", "cadence": "daily" }), &c).unwrap();
        assert_eq!(second["schedule"]["id"], id, "same row");
        assert_eq!(second["schedule"]["name"], json!("renamed"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM judgments.bugfinder_schedules", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "updated, not inserted");
    }

    #[test]
    fn save_with_an_unknown_id_inserts_a_new_row() {
        let c = db();
        let r = bugfinderSchedule(&json!({ "op": "save", "id": 4242, "name": "ghost" }), &c).unwrap();
        assert_ne!(r["schedule"]["id"], json!(4242), "a missing id falls through to INSERT");
    }

    #[test]
    fn delete_removes_the_row_and_requires_an_id() {
        let c = db();
        let s = bugfinderSchedule(&json!({ "op": "save", "name": "gone" }), &c).unwrap();
        let id = s["schedule"]["id"].as_i64().unwrap();
        let r = bugfinderSchedule(&json!({ "op": "delete", "id": id }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "id": id }));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM judgments.bugfinder_schedules", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        assert!(bugfinderSchedule(&json!({ "op": "delete" }), &c).unwrap_err().contains("id required"));
    }

    #[test]
    fn run_on_a_missing_schedule_is_not_found() {
        let c = db();
        let e = bugfinderSchedule(&json!({ "op": "run", "id": 999 }), &c).unwrap_err();
        assert!(e.contains("not found"), "{e}");
    }

    #[test]
    fn run_due_with_no_enabled_schedules_returns_an_empty_list() {
        let c = db();
        bugfinderSchedule(&json!({ "op": "save", "name": "disabled", "enabled": false }), &c).unwrap();
        let r = bugfinderSchedule(&json!({ "op": "run-due" }), &c).unwrap();
        assert_eq!(r, json!({ "ran": [] }), "a disabled schedule is never due");
    }

    #[test]
    fn unknown_op_lists_the_valid_ones() {
        let c = db();
        let e = bugfinderSchedule(&json!({ "op": "banana" }), &c).unwrap_err();
        assert!(e.contains("save, delete, run, run-due"), "{e}");
        assert!(e.contains("banana"), "the offending op is echoed: {e}");
    }
}
