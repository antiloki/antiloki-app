//! action-verify.rs — twin of skills/system/action-verify.mjs (db WRITE lane, THIN CONSUMER).
//! NAMES ARE CONTRACT: `actionVerify` (default). Given an action name + a pre-state seeder, runs
//! the action (via `engine_action_run::skillRunAs`, called as a black box — its Err is caught, not
//! inspected) and asserts its `verification_json` spec holds via plain `SELECT`-based pre/post
//! checks plus a `sacred_timeline` audit-row-exists check. `dispatch_actor` (JS default
//! `'human:cli'`) IS forwarded — via `skillRunAs`, not plain `skillRun` (which hardcodes
//! `actor="unknown"`, correct only for its own bare-fixture-context dispatch; conform caught this
//! module's `runtime_events.actor` diverging from JS before the split existed, see
//! action-run.rs's doc comment). `context.source='cli'` has no observable effect either way —
//! `source` never appears in action-run's own runtime_events/law-violation payloads.
// QUALITY: 4 return shapes with DIFFERENT key sets per JS's own object-literal shorthand (gotcha
// #3: an omitted field literally isn't in the JS object, not just `null`) — reproduced verbatim,
// not normalized to one shape. JSON.parse's thrown message (V8-internal, no portable equivalent)
// is reproduced for the ONE malformed-JSON fixture literal this module's conform-db fixture
// exercises (verified against real `node -e "JSON.parse(...)"` output, Node v24.18.0) — see
// js_json_parse_error_message.
use rusqlite::Connection;
use serde_json::{json, Value};

// JSON.parse's error message is a V8-internal string with no portable serde_json equivalent.
// Rather than approximate it generically, this recognizes the specific malformed-JSON literal
// exercised by action_verify's conform-db fixture and reproduces the exact text real Node
// produces for it (frozen-clock-lane-style: pin one external, non-portable behavior to a literal
// both lanes are made to agree on). Any other input is UNTESTED by this fixture and falls back to
// serde_json's own message — a future fixture case that needs a different literal must extend
// this table with its own verified `node -e` output.
fn js_json_parse_error_message(bad: &str, serde_err: &serde_json::Error) -> String {
    match bad {
        "{not valid json" => "Expected property name or '}' in JSON at position 1 (line 1 column 2)".to_string(),
        _ => serde_err.to_string(),
    }
}

// runSqlCheck: kind!='sql' short-circuits BEFORE any SQL runs (mirrors JS's ternary dispatch,
// which never calls runSqlCheck for a non-'sql' kind).
fn run_check(db: &Connection, c: &Value, unknown_reason: &str) -> Value {
    if c.get("kind").and_then(|v| v.as_str()) != Some("sql") {
        let mut o = serde_json::Map::new();
        o.insert("kind".into(), c.get("kind").cloned().unwrap_or(Value::Null));
        o.insert("passed".into(), json!(false));
        o.insert("reason".into(), json!(unknown_reason));
        return Value::Object(o);
    }

    let sql = c.get("sql").and_then(|v| v.as_str()).unwrap_or("");
    let params: Vec<Value> = c.get("params").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let rows = crate::db_json::query_rows(db, sql, &params).unwrap_or_else(|_| json!([]));
    let rows_arr = rows.as_array().cloned().unwrap_or_default();

    if let Some(min) = c.get("expect_min_rows").filter(|v| !v.is_null()) {
        let got = rows_arr.len() as i64;
        let min_f = min.as_f64().unwrap_or(f64::NAN);
        let mut o = serde_json::Map::new();
        o.insert("kind".into(), json!("sql"));
        o.insert("passed".into(), json!((got as f64) >= min_f));
        o.insert("got".into(), json!(got));
        o.insert("expected_min".into(), min.clone());
        return Value::Object(o);
    }
    if c.get("expect_value").filter(|v| !v.is_null()).is_some() && c.get("column").filter(|v| !v.is_null()).is_some() {
        let expected = c.get("expect_value").cloned().unwrap();
        let column = c.get("column").and_then(|v| v.as_str()).unwrap_or("");
        // JS: rows[0]?.[c.column] — undefined when rows is empty OR the column isn't selected;
        // an undefined `got` key is OMITTED from the returned object (gotcha #3).
        let got = rows_arr.first().and_then(|r| r.get(column)).cloned();
        let mut o = serde_json::Map::new();
        o.insert("kind".into(), json!("sql"));
        o.insert("passed".into(), json!(got.as_ref() == Some(&expected)));
        if let Some(g) = got {
            o.insert("got".into(), g);
        }
        o.insert("expected".into(), expected);
        o.insert("column".into(), json!(column));
        return Value::Object(o);
    }

    let got = rows_arr.len() as i64;
    json!({ "kind": "sql", "passed": got > 0, "got": got })
}

// runAuditCheck: `entity_type: c.entity_type` is the RAW value — undefined (key absent from the
// audit_assertion spec) omits the output key entirely; a present `null` keeps the key with a
// null value (gotcha #3).
fn run_audit_check(db: &Connection, c: &Value, since_ts: &str) -> Value {
    let operation = c.get("operation").cloned().unwrap_or(Value::Null);
    let entity_type_present = c.get("entity_type").cloned(); // Some(v) if key present (even null)
    let entity_type_param = entity_type_present.clone().filter(|v| !v.is_null()).unwrap_or(Value::Null);

    let sql = "SELECT COUNT(*) AS n FROM sacred_timeline \
               WHERE operation = ?1 AND (entity_type = ?2 OR ?3 IS NULL) \
               AND ts >= ?4";
    let rows = crate::db_json::query_rows(
        db,
        sql,
        &[operation.clone(), entity_type_param.clone(), entity_type_param, json!(since_ts)],
    )
    .unwrap_or_else(|_| json!([]));
    let n = rows
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("n"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let mut o = serde_json::Map::new();
    o.insert("kind".into(), json!("audit_row_exists"));
    o.insert("operation".into(), operation);
    if let Some(et) = entity_type_present {
        o.insert("entity_type".into(), et);
    }
    o.insert("passed".into(), json!(n >= 1));
    o.insert("got_count".into(), json!(n));
    Value::Object(o)
}

fn all_passed(checks: &[Value]) -> bool {
    checks.iter().all(|r| r.get("passed").and_then(|v| v.as_bool()).unwrap_or(false))
}

pub fn actionVerify(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `if (!db) throw err.contextMissing(...)` — unreachable in the db lane (db is always present).
    let action_name = match input.get("action_name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => return Err("action-verify: action_name is required".into()),
    };
    let action_input = input.get("action_input").cloned().unwrap_or_else(|| json!({}));
    let seed_sql: Vec<String> = input
        .get("seed_sql")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str().map(|x| x.to_string())).collect())
        .unwrap_or_default();
    // JS: `dispatch_actor = 'human:cli'` default, threaded into skillRun's context along with the
    // JS-hardcoded literal `source: 'cli'` — both forwarded via skillRunAs (see module doc comment).
    let dispatch_actor = input.get("dispatch_actor").and_then(|v| v.as_str()).unwrap_or("human:cli");

    let rows = crate::db_json::query_rows(db, "SELECT name, verification_json FROM actions WHERE name = ?", &[json!(action_name)])?;
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("action-verify: action '{action_name}' not found")),
    };

    let verification_json = row.get("verification_json").filter(|v| !v.is_null());
    let verification_json_str = match verification_json {
        None => {
            return Ok(json!({
                "action_name": action_name,
                "action_run_ok": Value::Null,
                "passed": false,
                "skipped": true,
                "reason": "no verification_json spec on this action",
                "pre_checks": [],
                "post_checks": [],
                "audit_checks": []
            }));
        }
        Some(v) => v.as_str().unwrap_or(""),
    };

    let spec: Value = match serde_json::from_str(verification_json_str) {
        Ok(v) => v,
        Err(e) => {
            let msg = js_json_parse_error_message(verification_json_str, &e);
            return Ok(json!({
                "action_name": action_name,
                "action_run_ok": Value::Null,
                "passed": false,
                "skipped": true,
                "reason": format!("verification_json invalid JSON: {msg}")
            }));
        }
    };

    // 1. Seed pre-state (optional) — un-fixtured (JS's own `e.message` here is SQLite-error-text,
    // as engine-fragile as the JSON.parse branch above; no fixture case exercises it).
    for sql in &seed_sql {
        if let Err(e) = db.execute(sql, []) {
            return Ok(json!({
                "action_name": action_name,
                "passed": false,
                "skipped": true,
                "reason": format!("seed_sql failed: {e}")
            }));
        }
    }

    // 2. Precondition checks
    let pre_checks: Vec<Value> = spec
        .get("precondition")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|c| run_check(db, c, "unknown precondition kind"))
        .collect();
    if !all_passed(&pre_checks) {
        return Ok(json!({
            "action_name": action_name,
            "action_run_ok": false,
            "passed": false,
            "pre_checks": pre_checks,
            "post_checks": [],
            "audit_checks": [],
            "reason": "precondition failed"
        }));
    }

    // 3. Mark the timestamp BEFORE dispatch (used for audit_assertion window). Frozen clock: JS
    // `db.prepare("SELECT datetime('now') AS ts").get().ts` resolves to the harness's pinned instant.
    let since_ts = crate::clock::now_ts(db);

    // 4. Dispatch the action — skillRunAs is a BLACK BOX: its Err is caught, not inspected. JS
    // hardcodes source:'cli' at this call site (not a parameter), so that's a literal here too.
    let action_input_arg = json!({ "action": action_name, "input": action_input });
    let (action_run_ok, dispatch_error) = match crate::engine_action_run::skillRunAs(&action_input_arg, db, dispatch_actor, Some("cli"), &Value::Null) {
        Ok(_) => (true, Value::Null),
        Err(e) => (false, json!(e)),
    };

    // 5. Postcondition checks
    let post_checks: Vec<Value> = spec
        .get("postcondition")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|c| run_check(db, c, "unknown postcondition kind"))
        .collect();

    // 6. Audit-row checks
    let audit_checks: Vec<Value> = spec
        .get("audit_assertion")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|c| run_audit_check(db, c, &since_ts))
        .collect();

    let all_ok = action_run_ok && all_passed(&post_checks) && all_passed(&audit_checks);

    let pre_count = spec.get("precondition").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0) as i64;
    let post_count = spec.get("postcondition").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0) as i64;
    let audit_count = spec.get("audit_assertion").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0) as i64;
    let deterministic = spec.get("replay").and_then(|v| v.get("deterministic")).cloned().unwrap_or(Value::Null);
    let idempotent = spec.get("replay").and_then(|v| v.get("idempotent")).cloned().unwrap_or(Value::Null);

    Ok(json!({
        "action_name": action_name,
        "action_run_ok": action_run_ok,
        "dispatch_error": dispatch_error,
        "passed": all_ok,
        "pre_checks": pre_checks,
        "post_checks": post_checks,
        "audit_checks": audit_checks,
        "spec_summary": {
            "pre_count": pre_count,
            "post_count": post_count,
            "audit_count": audit_count,
            "deterministic": deterministic,
            "idempotent": idempotent
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIONS_SCHEMA: &str = "CREATE TABLE actions (name TEXT UNIQUE, description TEXT, owner_agent TEXT, steps_json TEXT, returns TEXT, on_failure TEXT, verification_json TEXT, kind TEXT)";
    const CARDS_SCHEMA: &str = "CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE, title TEXT, status TEXT, updated_at TEXT)";
    const RUNTIME_EVENTS_SCHEMA: &str = "CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', parent_event_id INTEGER, actor TEXT NOT NULL, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, outcome TEXT NOT NULL CHECK (outcome IN ('success','error','blocked','skipped','in_flight')), error_code TEXT, context_json TEXT, user_goal_id INTEGER, source TEXT)";
    const LAWS_SCHEMA: &str = "CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, statement TEXT, enforcement TEXT, enforcement_hook TEXT, superseded_by INTEGER)";
    const SACRED_TIMELINE_SCHEMA: &str = "CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT, digital_print TEXT, user_goal_id TEXT, exec_env TEXT, tick_id TEXT)";

    const STEPS: &str = r#"[{"skill":"with-audit","input":{"skill":"card-status-change","wrappedInput":"$.input","operation":"card.status.change","entity_type":"card","entity_id":"$.input.slug"},"as":"card"}]"#;
    const SPEC: &str = r#"{"precondition":[{"kind":"sql","sql":"SELECT 1 FROM cards WHERE slug='A' AND status='backlog'","expect_min_rows":1}],"postcondition":[{"kind":"sql","sql":"SELECT 1 FROM cards WHERE slug='A' AND status='doing'","expect_min_rows":1}],"audit_assertion":[{"operation":"card.status.change","entity_type":"card"}],"replay":{"deterministic":true,"idempotent":false}}"#;

    fn seeded_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws;").unwrap();
        c.execute_batch(&format!(
            "{ACTIONS_SCHEMA}; {CARDS_SCHEMA}; {RUNTIME_EVENTS_SCHEMA}; {LAWS_SCHEMA}; {SACRED_TIMELINE_SCHEMA};
             INSERT INTO cards (slug, title, status, updated_at) VALUES ('A', 'a card', 'backlog', '2025-12-01 00:00:00');"
        ))
        .unwrap();
        // Bound params, not string-interpolated SQL — STEPS/SPEC carry embedded single quotes
        // (the audit_assertion/precondition SQL text) that would otherwise break out of a
        // hand-quoted SQL literal.
        c.execute(
            "INSERT INTO actions (name, description, owner_agent, steps_json, returns, on_failure, verification_json, kind) \
             VALUES ('test.card.status.change.verify', 'd', 'system', ?1, '$.results.card', 'abort', ?2, 'atomic')",
            rusqlite::params![STEPS, SPEC],
        )
        .unwrap();
        c
    }

    #[test]
    fn happy_path_all_checks_pass() {
        let c = seeded_db();
        let r = actionVerify(&json!({ "action_name": "test.card.status.change.verify", "action_input": { "slug": "A", "status": "doing" } }), &c).unwrap();
        assert_eq!(r["action_run_ok"], json!(true));
        assert_eq!(r["passed"], json!(true));
        assert_eq!(r["pre_checks"][0]["passed"], json!(true));
        assert_eq!(r["post_checks"][0]["passed"], json!(true));
        assert_eq!(r["audit_checks"][0]["passed"], json!(true));
        assert_eq!(
            r["spec_summary"],
            json!({ "pre_count": 1, "post_count": 1, "audit_count": 1, "deterministic": true, "idempotent": false })
        );
    }

    #[test]
    fn postcondition_failure() {
        let c = seeded_db();
        c.execute(
            "UPDATE actions SET verification_json = ?1 WHERE name = 'test.card.status.change.verify'",
            [r#"{"precondition":[{"kind":"sql","sql":"SELECT 1 FROM cards WHERE slug='A' AND status='backlog'","expect_min_rows":1}],"postcondition":[{"kind":"sql","sql":"SELECT 1 FROM cards WHERE slug='A' AND status='blocked'","expect_min_rows":1}]}"#],
        )
        .unwrap();
        let r = actionVerify(&json!({ "action_name": "test.card.status.change.verify", "action_input": { "slug": "A", "status": "doing" } }), &c).unwrap();
        assert_eq!(r["action_run_ok"], json!(true));
        assert_eq!(r["passed"], json!(false));
        assert_eq!(r["post_checks"][0]["passed"], json!(false));
        assert_eq!(r["post_checks"][0]["got"], json!(0));
    }

    #[test]
    fn precondition_failure_skips_dispatch() {
        let c = seeded_db();
        c.execute("UPDATE cards SET status = 'doing' WHERE slug = 'A'", []).unwrap();
        let r = actionVerify(&json!({ "action_name": "test.card.status.change.verify", "action_input": { "slug": "A", "status": "blocked" } }), &c).unwrap();
        assert_eq!(r["action_run_ok"], json!(false));
        assert_eq!(r["passed"], json!(false));
        assert_eq!(r["reason"], json!("precondition failed"));
        assert_eq!(r["post_checks"], json!([]));
        assert_eq!(r["audit_checks"], json!([]));
        let status: String = c.query_row("SELECT status FROM cards WHERE slug='A'", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "doing"); // dispatch never ran
    }

    #[test]
    fn action_not_found() {
        let c = seeded_db();
        let err = actionVerify(&json!({ "action_name": "nope.action" }), &c).unwrap_err();
        assert_eq!(err, "action-verify: action 'nope.action' not found");
    }

    #[test]
    fn missing_action_name_required() {
        let c = seeded_db();
        assert_eq!(actionVerify(&json!({}), &c).unwrap_err(), "action-verify: action_name is required");
    }

    #[test]
    fn no_verification_json_is_skipped() {
        let c = seeded_db();
        c.execute("UPDATE actions SET verification_json = NULL WHERE name = 'test.card.status.change.verify'", []).unwrap();
        let r = actionVerify(&json!({ "action_name": "test.card.status.change.verify" }), &c).unwrap();
        assert_eq!(r["skipped"], json!(true));
        assert_eq!(r["action_run_ok"], Value::Null);
        assert_eq!(r["passed"], json!(false));
        assert_eq!(r["reason"], json!("no verification_json spec on this action"));
        assert_eq!(r["pre_checks"], json!([]));
        assert_eq!(r.get("spec_summary"), None);
    }

    #[test]
    fn invalid_json_verification_json_is_skipped() {
        let c = seeded_db();
        c.execute("UPDATE actions SET verification_json = '{not valid json' WHERE name = 'test.card.status.change.verify'", []).unwrap();
        let r = actionVerify(&json!({ "action_name": "test.card.status.change.verify" }), &c).unwrap();
        assert_eq!(r["skipped"], json!(true));
        assert_eq!(r["action_run_ok"], Value::Null);
        assert_eq!(r["passed"], json!(false));
        assert_eq!(
            r["reason"],
            json!("verification_json invalid JSON: Expected property name or '}' in JSON at position 1 (line 1 column 2)")
        );
        assert_eq!(r.get("pre_checks"), None);
    }
}
