//! man-seal.rs — twin of skills/blueprint/man-seal.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `manSeal` (default). MAN checks a phase's STAGED audit trail (3 deterministic pass/fail
//! criteria over timeline.audit_stage — no AI) then seals it into sacred_timeline via the
//! already-converted `flushAuditStage` twin (engine_with_audit) — UNCONDITIONALLY, even when the
//! verdict is 'blocked' (history is sacred: the wrong attempt's trail is canonical too).
// QUALITY: phase->man_reports.phase mapping, 3-criteria deterministic check, seal via with-audit twin

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
// JS `${v}` interpolation for the values that appear (string/number/bool/null/absent).
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
// Array.prototype.join(sep) element coercion: null/undefined -> '', else String(x) (unlike tmpl,
// which renders null as the literal text "null" for template-literal interpolation).
fn join_els(vals: &[Value], sep: &str) -> String {
    vals.iter()
        .map(|v| match v {
            Value::Null => String::new(),
            Value::String(s) => s.clone(),
            Value::Bool(b) => b.to_string(),
            n @ Value::Number(_) => n.to_string(),
            o => o.to_string(),
        })
        .collect::<Vec<_>>()
        .join(sep)
}

// PHASE_MAP: v2 phase -> the man_reports.phase CHECK vocabulary.
fn phase_mapped(phase: &str) -> Option<&'static str> {
    match phase {
        "plan" => Some("contract_signed"),
        "build" => Some("qa_passed"),
        _ => None,
    }
}

pub fn manSeal(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing('man-seal', 'db')` —
    // unreachable in the db lane, a real Connection is always supplied.
    let phase_v = input.get("phase");
    let phase = match phase_v.and_then(|v| v.as_str()).and_then(|p| phase_mapped(p).map(|m| (p, m))) {
        Some((p, m)) => (p, m),
        None => {
            return Err(format!(
                "man-seal: phase must be one of: plan, build (got '{}')",
                tmpl(phase_v)
            ));
        }
    };
    let (phase, mapped_phase) = phase;

    let contract_slug_v = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&contract_slug_v)) {
        return Err("man-seal: contract_slug is required".into());
    }

    // Resolve the session to seal: explicit, else the card's current-run session (read-only).
    // Any lookup failure (missing table/column) is swallowed, same as the JS try/catch -> null.
    let mut session_id_v: Option<Value> = input.get("session_id").cloned();
    if !truthy(session_id_v.as_ref()) {
        let mut card_slug_v: Option<Value> = input.get("card_slug").cloned();
        if !truthy(card_slug_v.as_ref()) {
            card_slug_v = crate::db_json::query_rows(db, "SELECT card_slug FROM contracts WHERE slug = ?", &[contract_slug_v.clone()])
                .ok()
                .and_then(|r| r.as_array().and_then(|a| a.first()).cloned())
                .and_then(|row| row.get("card_slug").cloned());
        }
        if truthy(card_slug_v.as_ref()) {
            session_id_v = crate::db_json::query_rows(db, "SELECT openrouter_session_id FROM cards WHERE slug = ?", &[card_slug_v.unwrap()])
                .ok()
                .and_then(|r| r.as_array().and_then(|a| a.first()).cloned())
                .and_then(|row| row.get("openrouter_session_id").cloned());
        } else {
            session_id_v = None;
        }
    }
    if !truthy(session_id_v.as_ref()) {
        return Err("man-seal: session_id is required".into());
    }
    let session_id_v = session_id_v.unwrap();

    let rows = crate::db_json::query_rows(
        db,
        "SELECT actor, operation, entity_type, entity_id FROM timeline.audit_stage WHERE session_id = ? ORDER BY id ASC",
        &[session_id_v.clone()],
    )?;
    let rows = rows.as_array().cloned().unwrap_or_default();

    // ── deterministic checks over the staged trail ──────────────────────
    let operations: Vec<Value> = rows.iter().map(|r| r.get("operation").cloned().unwrap_or(Value::Null)).collect();
    let expected: Vec<Value> = input.get("expected_stages").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let missing: Vec<Value> = expected.iter().filter(|op| !operations.iter().any(|o| o == *op)).cloned().collect();
    let malformed_count = rows
        .iter()
        .filter(|r| {
            !truthy(r.get("actor")) || !truthy(r.get("operation")) || !truthy(r.get("entity_type"))
                || r.get("entity_id").map_or(true, |v| v.is_null())
        })
        .count();

    let rows_len = rows.len() as i64;
    let c1_pass = rows_len > 0;
    let c2_pass = missing.is_empty();
    let c2_detail = if !missing.is_empty() {
        format!("missing: {}", join_els(&missing, ", "))
    } else {
        format!("all {} expected stages present", expected.len())
    };
    let c3_pass = malformed_count == 0;
    let c3_detail = if malformed_count > 0 {
        format!("{malformed_count} malformed rows")
    } else {
        "all rows well-formed".to_string()
    };

    let criteria = json!({
        "c1_log_non_empty": { "critical": true, "pass": c1_pass, "detail": format!("{rows_len} staged rows") },
        "c2_expected_stages_present": { "critical": true, "pass": c2_pass, "detail": c2_detail },
        "c3_rows_well_formed": { "critical": true, "pass": c3_pass, "detail": c3_detail },
    });
    // Object.entries(criteria).filter(([, c]) => c.critical && !c.pass) — insertion order c1,c2,c3.
    let crit_order = ["c1_log_non_empty", "c2_expected_stages_present", "c3_rows_well_formed"];
    let critical_fails: Vec<(&str, &Value)> = crit_order
        .iter()
        .filter_map(|k| {
            let c = &criteria[*k];
            if c["pass"] == json!(false) { Some((*k, c)) } else { None }
        })
        .collect();
    let verdict = if critical_fails.is_empty() { "approved" } else { "blocked" };

    // ── SEAL (unconditional — the trail is sacred even when blocked) ─────
    let flush_result = crate::engine_with_audit::flushAuditStage(db, &session_id_v)?;
    let flushed = flush_result.get("flushed").and_then(|v| v.as_i64()).unwrap_or(0);

    let distinct_operations: i64 = {
        let mut set: std::collections::HashSet<String> = std::collections::HashSet::new();
        for o in &operations {
            set.insert(serde_json::to_string(o).unwrap_or_default());
        }
        set.len() as i64
    };
    let metrics = json!({
        "staged_count": rows_len,
        "distinct_operations": distinct_operations,
        "sealed": flushed,
        "mapped_phase": mapped_phase,
    });

    let mut narrative_lines: Vec<String> = vec![
        format!(
            "MAN seal of the {} phase (session {}, contract {}).",
            phase.to_uppercase(),
            tmpl(Some(&session_id_v)),
            tmpl(Some(&contract_slug_v)),
        ),
        format!("Verdict: {} — sealed {} staged rows into the timeline.", verdict.to_uppercase(), flushed),
    ];
    for (name, c) in &critical_fails {
        narrative_lines.push(format!("  \u{2717} {} — {}", name, c["detail"].as_str().unwrap_or("")));
    }
    let narrative = narrative_lines.join("\n");

    let metrics_json = serde_json::to_string(&metrics).map_err(|e| e.to_string())?;
    let criteria_json = serde_json::to_string(&criteria).map_err(|e| e.to_string())?;

    // FK to contracts(slug) is the only hard constraint in production; on failure JS wraps it in
    // an AntilokiError (`man-seal: failed to write man_reports: <cause>`) — a guard-throw the
    // fixture never exercises (only the success path is fixture-tested per the harness's
    // fragile-thrown-message rule), so this mirrors the shape without chasing byte-exact text.
    let (_changes, report_id) = crate::db_json::run_write(
        db,
        "INSERT INTO man_reports (contract_slug, phase, verdict, metrics_json, criteria_json, what_happened_md) VALUES (?, ?, ?, ?, ?, ?)",
        &[
            contract_slug_v.clone(),
            json!(mapped_phase),
            json!(verdict),
            json!(metrics_json),
            json!(criteria_json),
            json!(narrative),
        ],
    )
    .map_err(|e| format!("man-seal: failed to write man_reports: {e}"))?;

    let mut out = Map::new();
    out.insert("session_id".into(), session_id_v);
    out.insert("phase".into(), json!(phase));
    out.insert("verdict".into(), json!(verdict));
    out.insert("sealed".into(), json!(flushed));
    out.insert("report_id".into(), json!(report_id));
    out.insert("criteria".into(), criteria);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS timeline;
             CREATE TABLE contracts (slug TEXT, card_slug TEXT);
             CREATE TABLE cards (slug TEXT, openrouter_session_id TEXT);
             CREATE TABLE man_reports (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, phase TEXT, verdict TEXT, metrics_json TEXT, criteria_json TEXT, what_happened_md TEXT);
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT, digital_print TEXT, user_goal_id TEXT, exec_env TEXT, tick_id TEXT);
             CREATE TABLE timeline.audit_stage (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, tick_id INTEGER, phase TEXT, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, exec_env TEXT, user_goal_id INTEGER);
             INSERT INTO contracts (slug, card_slug) VALUES ('CT-S', 'CARD-S');",
        )
        .unwrap();
        c
    }
    fn stage(c: &Connection, session_id: &str, op: &str) {
        c.execute(
            "INSERT INTO timeline.audit_stage (session_id, phase, actor, operation, entity_type, entity_id, after_json) VALUES (?1,'plan','tl',?2,'contract','CT-S','{}')",
            rusqlite::params![session_id, op],
        )
        .unwrap();
    }

    #[test]
    fn approved_checks_the_log_and_seals_the_staged_trail() {
        let c = seeded_db();
        stage(&c, "sess-1", "pl.screening");
        stage(&c, "sess-1", "tl.contract.assemble");
        stage(&c, "sess-1", "blueprint.design");
        let r = manSeal(
            &json!({ "session_id": "sess-1", "phase": "plan", "contract_slug": "CT-S", "expected_stages": ["pl.screening", "blueprint.design"] }),
            &c,
        )
        .unwrap();
        assert_eq!(r["verdict"], json!("approved"));
        assert_eq!(r["sealed"], json!(3));
        let staged = crate::db_json::query_rows(&c, "SELECT count(*) n FROM timeline.audit_stage WHERE session_id='sess-1'", &[]).unwrap();
        assert_eq!(staged[0]["n"], json!(0)); // drained
        let mr = crate::db_json::query_rows(&c, "SELECT phase, verdict FROM man_reports WHERE id = ?", &[r["report_id"].clone()]).unwrap();
        assert_eq!(mr[0]["phase"], json!("contract_signed")); // plan -> contract_signed
        assert_eq!(mr[0]["verdict"], json!("approved"));
    }

    #[test]
    fn blocked_verdict_still_seals() {
        let c = seeded_db();
        stage(&c, "sess-2", "pl.screening");
        let r = manSeal(&json!({ "session_id": "sess-2", "phase": "plan", "contract_slug": "CT-S", "expected_stages": ["blueprint.design"] }), &c).unwrap();
        assert_eq!(r["verdict"], json!("blocked"));
        assert_eq!(r["criteria"]["c2_expected_stages_present"]["pass"], json!(false));
        assert_eq!(r["sealed"], json!(1)); // sealed anyway — the trail survives the block
    }

    #[test]
    fn empty_log_blocked_build_phase_maps_to_qa_passed() {
        let c = seeded_db();
        let r = manSeal(&json!({ "session_id": "sess-empty", "phase": "build", "contract_slug": "CT-S" }), &c).unwrap();
        assert_eq!(r["verdict"], json!("blocked"));
        assert_eq!(r["criteria"]["c1_log_non_empty"]["pass"], json!(false));
        assert_eq!(r["sealed"], json!(0));
        let mr = crate::db_json::query_rows(&c, "SELECT phase FROM man_reports WHERE id = ?", &[r["report_id"].clone()]).unwrap();
        assert_eq!(mr[0]["phase"], json!("qa_passed"));
    }

    #[test]
    fn bad_phase_is_an_enum_error() {
        let c = seeded_db();
        let err = manSeal(&json!({ "session_id": "s", "phase": "close", "contract_slug": "CT-S" }), &c).unwrap_err();
        assert!(err.contains("phase"), "{err}");
    }

    #[test]
    fn resolves_the_session_from_the_card_when_session_id_is_omitted() {
        let c = seeded_db();
        c.execute("INSERT INTO cards (slug, openrouter_session_id) VALUES ('CARD-S', 'card-sess-9')", []).unwrap();
        stage(&c, "card-sess-9", "pl.screening");
        stage(&c, "card-sess-9", "blueprint.design");
        let r = manSeal(&json!({ "phase": "plan", "contract_slug": "CT-S" }), &c).unwrap(); // no session_id
        assert_eq!(r["session_id"], json!("card-sess-9"));
        assert_eq!(r["sealed"], json!(2));
        assert_eq!(r["verdict"], json!("approved"));
    }

    #[test]
    fn malformed_rows_fail_c3_and_distinct_operations_dedupes() {
        let c = seeded_db();
        c.execute(
            "INSERT INTO timeline.audit_stage (session_id, actor, operation, entity_type, entity_id, after_json) VALUES ('sess-4','tl','a.op','contract','CT-S','{}')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO timeline.audit_stage (session_id, actor, operation, entity_type, entity_id, after_json) VALUES ('sess-4','','a.op','contract','CT-S','{}')",
            [],
        )
        .unwrap();
        let r = manSeal(&json!({ "session_id": "sess-4", "phase": "plan", "contract_slug": "CT-S" }), &c).unwrap();
        assert_eq!(r["criteria"]["c3_rows_well_formed"]["pass"], json!(false));
        assert_eq!(r["criteria"]["c3_rows_well_formed"]["detail"], json!("1 malformed rows"));
        assert_eq!(r["verdict"], json!("blocked"));
        let mr = crate::db_json::query_rows(&c, "SELECT metrics_json FROM man_reports WHERE id = ?", &[r["report_id"].clone()]).unwrap();
        assert!(mr[0]["metrics_json"].as_str().unwrap().contains("\"distinct_operations\":1"));
    }
}
