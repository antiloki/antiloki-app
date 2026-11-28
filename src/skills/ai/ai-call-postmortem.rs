//! ai-call-postmortem.rs — twin of skills/ai/ai-call-postmortem.mjs (db WRITE lane, attached
//! `brain` db). NAMES ARE CONTRACT: `aiCallPostmortem` (default). Assembles the evidence for a
//! failed AI call or stage (call row, attempts trail, sentinel raw output, paired reason-leg
//! outputs, contract failure narrative), classifies it via ai_failure_classes::classifyFailure,
//! renders the deterministic diagnosis/recommendation, and persists ONE brain.ai_call_reports
//! row — idempotent via (ai_call_id|related_contract_slug, trigger) dedup (a time-window dedup
//! for call-less stage reports, frozen to the crossing's canonical instant).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// = JS String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

// = JS String(v) (no ??; used for the raw_output_md/failure_md CLIP, which only runs on an
// already-known-non-null value).
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

fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() } // JS .slice(0,n) (BMP-equivalent)

// First non-nullish candidate, JS `a ?? b ?? … ?? default`.
fn coalesce(vals: Vec<Option<Value>>, default: Value) -> Value {
    for v in vals {
        if let Some(x) = v {
            if !x.is_null() { return x; }
        }
    }
    default
}

// SEVERITY_DEFAULT[trigger] — a plain object lookup; unknown keys fall through to the caller's
// own `?? 'warn'`.
fn severity_default(trigger: &Value) -> Option<Value> {
    match trigger.as_str() {
        Some("ai_call_validation") => Some(json!("warn")),
        Some("stage_lint") => Some(json!("error")),
        Some("driver_tick") => Some(json!("error")),
        Some("manual") => Some(json!("info")),
        _ => None,
    }
}

const GET_CALL_SQL: &str = "SELECT a.*, p.name AS prompt_name, p.version AS prompt_version, p.max_output_tokens FROM brain.ai_calls a LEFT JOIN brain.prompt_templates p ON p.id = a.prompt_template_id WHERE a.id = ?";

fn get_call(db: &Connection, id: &Value) -> Option<Value> {
    let rows = crate::db_json::query_rows(db, GET_CALL_SQL, &[id.clone()]).ok()?;
    rows.as_array().and_then(|a| a.first()).cloned()
}

// Bounded attribution heuristic (same shape as card-process-events): newest non-success call,
// within the last 5000 ai_calls rows, whose recorded inputs mention the slug.
const RESOLVE_CALL_BY_SLUG_SQL: &str = "SELECT a.*, p.name AS prompt_name, p.version AS prompt_version, p.max_output_tokens FROM brain.ai_calls a LEFT JOIN brain.prompt_templates p ON p.id = a.prompt_template_id JOIN brain.ai_call_inputs i ON i.ai_call_id = a.id WHERE i.ai_call_id >= (SELECT COALESCE(MAX(id),0) - 5000 FROM brain.ai_calls) AND a.outcome != 'success' AND i.value_text LIKE '%' || ? || '%' ORDER BY a.id DESC LIMIT 1";

fn resolve_call_by_slug(db: &Connection, slug: &Value) -> Option<Value> {
    let rows = crate::db_json::query_rows(db, RESOLVE_CALL_BY_SLUG_SQL, &[slug.clone()]).ok()?;
    rows.as_array().and_then(|a| a.first()).cloned()
}

const INSERT_REPORT_SQL: &str = "INSERT INTO brain.ai_call_reports (ai_call_id, prompt_template_id, prompt_name, prompt_version, tier, model_id, related_action, related_contract_slug, related_card_slug, trigger, failure_class, severity, evidence_json, diagnosis_md, recommendation_md, retry_recovered) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";

pub fn aiCallPostmortem(input: &Value, db: &Connection) -> Result<Value, String> {
    let ai_call_id = nz(input.get("ai_call_id")).cloned();
    let contract_slug = nz(input.get("contract_slug")).cloned();
    let card_slug = nz(input.get("card_slug")).cloned();
    let related_action = nz(input.get("related_action")).cloned();
    let lint_violations = nz(input.get("lint_violations")).cloned();
    let failure_kind = nz(input.get("failure_kind")).cloned();
    let failure_md = nz(input.get("failure_md")).cloned();
    let validation_errors = nz(input.get("validation_errors")).cloned();
    let raw_output_md = nz(input.get("raw_output_md")).cloned();
    // destructure default 'manual' fires only when the key is ABSENT (JS: an explicit null on
    // the input stays null — it is not replaced by the default).
    let trigger = match input.get("trigger") {
        None => json!("manual"),
        Some(v) => v.clone(),
    };
    let severity = nz(input.get("severity")).cloned();
    let dedup_since = nz(input.get("dedup_since")).cloned();

    if ai_call_id.is_none() && !truthy(contract_slug.as_ref()) {
        return Err("ai-call-postmortem: ai_call_id or contract_slug is required".into());
    }

    // ── resolve the call row (direct id, or the bounded slug heuristic) ─────────────────────
    let call: Option<Value> = match &ai_call_id {
        Some(id) => get_call(db, id),
        None => resolve_call_by_slug(db, contract_slug.as_ref().unwrap()),
    };
    let call_id = call.as_ref().and_then(|c| nz(c.get("id"))).cloned();

    // ── dedup (idempotent by construction) ───────────────────────────────────────────────────
    let mut existing: Option<Value> = None;
    let mut dedup_failed = false;
    if let Some(cid) = &call_id {
        match crate::db_json::query_rows(
            db,
            "SELECT id, failure_class FROM brain.ai_call_reports WHERE ai_call_id = ? AND trigger = ? LIMIT 1",
            &[cid.clone(), trigger.clone()],
        ) {
            Ok(rows) => existing = rows.as_array().and_then(|a| a.first()).cloned(),
            Err(_) => dedup_failed = true,
        }
    } else if truthy(contract_slug.as_ref()) {
        let slug = contract_slug.clone().unwrap();
        let res = match &dedup_since {
            Some(ds) => crate::db_json::query_rows(
                db,
                "SELECT id, failure_class FROM brain.ai_call_reports WHERE related_contract_slug = ? AND trigger = ? AND ai_call_id IS NULL AND ts >= ? ORDER BY id DESC LIMIT 1",
                &[slug, trigger.clone(), ds.clone()],
            ),
            None => crate::db_json::query_rows(
                db,
                &format!("SELECT id, failure_class FROM brain.ai_call_reports WHERE related_contract_slug = ? AND trigger = ? AND ai_call_id IS NULL AND ts >= datetime({},'-10 minutes') ORDER BY id DESC LIMIT 1", crate::clock::now_sql_inner()),
                &[slug, trigger.clone()],
            ),
        };
        match res {
            Ok(rows) => existing = rows.as_array().and_then(|a| a.first()).cloned(),
            Err(_) => dedup_failed = true,
        }
    }
    if dedup_failed {
        return Ok(json!({ "skipped": true, "reason": "ai_call_reports table absent (pre-migration-135)" }));
    }
    if let Some(e) = existing {
        return Ok(json!({
            "report_id": e.get("id").cloned().unwrap_or(Value::Null),
            "failure_class": e.get("failure_class").cloned().unwrap_or(Value::Null),
            "deduped": true,
            "ai_call_id": call_id.clone().unwrap_or(Value::Null),
        }));
    }

    // ── assemble evidence ────────────────────────────────────────────────────────────────────
    let mut contract_row: Option<Value> = None;
    if truthy(contract_slug.as_ref()) {
        if let Ok(rows) = crate::db_json::query_rows(
            db,
            "SELECT card_slug, last_failure_kind, last_failure_md FROM contracts WHERE slug = ?",
            &[contract_slug.clone().unwrap()],
        ) {
            contract_row = rows.as_array().and_then(|a| a.first()).cloned();
        }
    }

    let mut attempts: Vec<Value> = Vec::new();
    if let Some(cid) = &call_id {
        if let Ok(rows) = crate::db_json::query_rows(
            db,
            "SELECT attempt_no, outcome, correction_md, raw_output_md, validation_errors_json FROM brain.ai_call_attempts WHERE ai_call_id = ? ORDER BY attempt_no",
            &[cid.clone()],
        ) {
            attempts = rows.as_array().cloned().unwrap_or_default();
        }
    }
    let last_failed_attempt: Option<&Value> = attempts
        .iter()
        .rev()
        .find(|a| a.get("outcome").and_then(|v| v.as_str()) != Some("success"));

    let mut attempt_errors: Option<Value> = None;
    if let Some(lfa) = last_failed_attempt {
        if truthy(lfa.get("validation_errors_json")) {
            if let Some(s) = lfa.get("validation_errors_json").and_then(|v| v.as_str()) {
                if let Ok(parsed) = serde_json::from_str::<Value>(s) { attempt_errors = Some(parsed); }
            }
        }
    }

    let mut sentinel_raw: Option<Value> = None;
    if let Some(cid) = &call_id {
        if let Ok(rows) = crate::db_json::query_rows(
            db,
            "SELECT value_text FROM brain.ai_call_outputs WHERE ai_call_id = ? AND field_name IN ('__raw_output_on_validation_fail__', '__raw_form_md__') ORDER BY field_name LIMIT 1",
            &[cid.clone()],
        ) {
            if let Some(row) = rows.as_array().and_then(|a| a.first()) {
                sentinel_raw = nz(row.get("value_text")).cloned();
            }
        }
    }

    // Paired reason-leg outputs — the signal that separates dropped_reason_decisions from plain
    // vague_spec.
    let mut reason_outputs: Vec<Value> = Vec::new();
    let reason_call_id = call.as_ref().and_then(|c| nz(c.get("reason_call_id"))).cloned();
    if let Some(rcid) = &reason_call_id {
        if let Ok(rows) = crate::db_json::query_rows(
            db,
            "SELECT field_name, value_text FROM brain.ai_call_outputs WHERE ai_call_id = ? AND parent_output_id IS NULL AND value_text IS NOT NULL",
            &[rcid.clone()],
        ) {
            reason_outputs = rows.as_array().cloned().unwrap_or_default();
        }
    }

    let last_failed_raw_output_md = last_failed_attempt.and_then(|a| nz(a.get("raw_output_md"))).cloned();
    let attempts_mapped: Vec<Value> = attempts
        .iter()
        .map(|a| json!({
            "attempt_no": a.get("attempt_no").cloned().unwrap_or(Value::Null),
            "outcome": a.get("outcome").cloned().unwrap_or(Value::Null),
        }))
        .collect();

    let mut evidence = Map::new();
    evidence.insert("trigger".into(), trigger.clone());
    evidence.insert("outcome".into(), call.as_ref().and_then(|c| nz(c.get("outcome"))).cloned().unwrap_or(Value::Null));
    evidence.insert("validation_errors".into(), coalesce(vec![validation_errors.clone(), attempt_errors.clone()], json!([])));
    evidence.insert("raw_output_md".into(), coalesce(vec![raw_output_md.clone(), sentinel_raw.clone(), last_failed_raw_output_md], Value::Null));
    evidence.insert("tokens_out".into(), call.as_ref().and_then(|c| nz(c.get("tokens_out"))).cloned().unwrap_or(Value::Null));
    evidence.insert("max_output_tokens".into(), call.as_ref().and_then(|c| nz(c.get("max_output_tokens"))).cloned().unwrap_or(Value::Null));
    evidence.insert("lint_violations".into(), lint_violations.clone().unwrap_or(json!([])));
    evidence.insert("failure_kind".into(), coalesce(vec![failure_kind.clone(), contract_row.as_ref().and_then(|c| nz(c.get("last_failure_kind"))).cloned()], Value::Null));
    evidence.insert("failure_md".into(), coalesce(vec![failure_md.clone(), contract_row.as_ref().and_then(|c| nz(c.get("last_failure_md"))).cloned()], Value::Null));
    evidence.insert("reason_outputs".into(), json!(reason_outputs));
    evidence.insert("attempts".into(), json!(attempts_mapped));
    evidence.insert("prompt_name".into(), call.as_ref().and_then(|c| nz(c.get("prompt_name"))).cloned().unwrap_or(Value::Null));
    evidence.insert("tier".into(), call.as_ref().and_then(|c| nz(c.get("tier"))).cloned().unwrap_or(Value::Null));
    let evidence_v = Value::Object(evidence.clone());

    // ── classify + render + persist ──────────────────────────────────────────────────────────
    let failure_class = crate::ai_failure_classes::classifyFailure(&evidence_v);
    let diagnosis_md = crate::ai_failure_classes::renderDiagnosisMd(&failure_class, &evidence_v);
    let recommendation_md = crate::ai_failure_classes::renderRecommendationMd(&failure_class, &evidence_v);
    let sev = coalesce(vec![severity.clone(), severity_default(&trigger)], json!("warn"));
    let retry_recovered: i64 = if !attempts.is_empty()
        && attempts.iter().any(|a| a.get("outcome").and_then(|v| v.as_str()) == Some("success"))
    { 1 } else { 0 };

    // Evidence persists CLIPPED — diagnosis_md already carries the curated excerpts; the JSON is
    // for programmatic consumers. Spread-then-override keeps the SAME key order as `evidence`
    // (raw_output_md/failure_md/reason_outputs already exist at their original position).
    let reason_outputs_mapped: Vec<Value> = reason_outputs
        .iter()
        .map(|o| json!({
            "field_name": o.get("field_name").cloned().unwrap_or(Value::Null),
            "value_text": take_chars(&str_qq(o.get("value_text")), 1000),
        }))
        .collect();
    let clip4000 = |v: &Value| -> Value { if v.is_null() { Value::Null } else { json!(take_chars(&js_string(v), 4000)) } };
    let mut evidence_for_row = evidence;
    let raw_output_md_v = evidence_for_row.get("raw_output_md").cloned().unwrap_or(Value::Null);
    evidence_for_row.insert("raw_output_md".into(), clip4000(&raw_output_md_v));
    let failure_md_v = evidence_for_row.get("failure_md").cloned().unwrap_or(Value::Null);
    evidence_for_row.insert("failure_md".into(), clip4000(&failure_md_v));
    evidence_for_row.insert("reason_outputs".into(), json!(reason_outputs_mapped));
    let evidence_json = serde_json::to_string(&Value::Object(evidence_for_row)).unwrap_or_default();

    let related_card_slug = coalesce(vec![card_slug.clone(), contract_row.as_ref().and_then(|c| nz(c.get("card_slug"))).cloned()], Value::Null);

    let params = vec![
        call_id.clone().unwrap_or(Value::Null),
        call.as_ref().and_then(|c| nz(c.get("prompt_template_id"))).cloned().unwrap_or(Value::Null),
        call.as_ref().and_then(|c| nz(c.get("prompt_name"))).cloned().unwrap_or(Value::Null),
        call.as_ref().and_then(|c| nz(c.get("prompt_version"))).cloned().unwrap_or(Value::Null),
        call.as_ref().and_then(|c| nz(c.get("tier"))).cloned().unwrap_or(Value::Null),
        call.as_ref().and_then(|c| nz(c.get("model_id"))).cloned().unwrap_or(Value::Null),
        related_action.clone().unwrap_or(Value::Null),
        contract_slug.clone().unwrap_or(Value::Null),
        related_card_slug,
        trigger.clone(),
        failure_class.clone(),
        sev.clone(),
        json!(evidence_json),
        diagnosis_md.clone(),
        recommendation_md.clone(),
        json!(retry_recovered),
    ];
    match crate::db_json::run_write(db, INSERT_REPORT_SQL, &params) {
        Ok((_changes, id)) => Ok(json!({
            "report_id": id,
            "failure_class": failure_class,
            "severity": sev,
            "diagnosis_md": diagnosis_md,
            "recommendation_md": recommendation_md,
            "ai_call_id": call_id.unwrap_or(Value::Null),
            "deduped": false,
        })),
        Err(e) => Ok(json!({
            "skipped": true,
            "reason": format!("report insert failed: {}", take_chars(&e, 120)),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE contracts ( id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, card_slug TEXT, last_failure_kind TEXT, last_failure_md TEXT );
        CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, version INTEGER, max_output_tokens INTEGER );
        CREATE TABLE brain.ai_calls ( id INTEGER PRIMARY KEY AUTOINCREMENT, prompt_template_id INTEGER, tier TEXT, model_id TEXT, tokens_out INTEGER, outcome TEXT, reason_call_id INTEGER );
        CREATE TABLE brain.ai_call_inputs ( id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, value_text TEXT );
        CREATE TABLE brain.ai_call_attempts ( id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, attempt_no INTEGER, outcome TEXT, correction_md TEXT, raw_output_md TEXT, validation_errors_json TEXT );
        CREATE TABLE brain.ai_call_outputs ( id INTEGER PRIMARY KEY AUTOINCREMENT, ai_call_id INTEGER, field_name TEXT, value_text TEXT, parent_output_id INTEGER );
        CREATE TABLE brain.ai_call_reports ( id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', ai_call_id INTEGER, prompt_template_id INTEGER, prompt_name TEXT, prompt_version INTEGER, tier TEXT, model_id TEXT, related_action TEXT, related_contract_slug TEXT, related_card_slug TEXT, trigger TEXT, failure_class TEXT, severity TEXT, evidence_json TEXT, diagnosis_md TEXT, recommendation_md TEXT, retry_recovered INTEGER );
    ";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    #[test]
    fn happy_path_via_ai_call_id_classifies_and_persists() {
        let c = db();
        c.execute_batch("INSERT INTO brain.prompt_templates (id, name, version, max_output_tokens) VALUES (5, 'blueprint.v1', 2, 1000);
            INSERT INTO brain.ai_calls (id, prompt_template_id, tier, model_id, tokens_out, outcome) VALUES (10, 5, 'low', 'model-a', 120, 'validation_failed');").unwrap();
        let r = aiCallPostmortem(&json!({
            "ai_call_id": 10, "trigger": "ai_call_validation",
            "validation_errors": ["form compile: missing section ## CODE"]
        }), &c).unwrap();
        assert_eq!(r["failure_class"], json!("format_noncompliance"));
        assert_eq!(r["severity"], json!("warn")); // SEVERITY_DEFAULT.ai_call_validation
        assert_eq!(r["deduped"], json!(false));
        assert_eq!(r["ai_call_id"], json!(10));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.ai_call_reports", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
        let prompt_name: String = c.query_row("SELECT prompt_name FROM brain.ai_call_reports WHERE id = 1", [], |row| row.get(0)).unwrap();
        assert_eq!(prompt_name, "blueprint.v1"); // joined off prompt_template_id
    }

    #[test]
    fn dedup_by_ai_call_id_and_by_contract_slug() {
        let c = db();
        c.execute_batch("INSERT INTO brain.ai_calls (id, outcome) VALUES (11, 'success');
            INSERT INTO brain.ai_call_reports (id, ai_call_id, trigger, failure_class) VALUES (77, 11, 'manual', 'refusal_empty');").unwrap();
        let byid = aiCallPostmortem(&json!({ "ai_call_id": 11, "trigger": "manual" }), &c).unwrap();
        assert_eq!(byid, json!({ "report_id": 77, "failure_class": "refusal_empty", "deduped": true, "ai_call_id": 11 }));

        let c2 = db();
        c2.execute_batch("INSERT INTO brain.ai_call_reports (id, ts, related_contract_slug, trigger, failure_class, ai_call_id) VALUES (90, '2026-01-10 00:00:00', 'FEAT-X', 'stage_lint', 'vague_spec', NULL);").unwrap();
        let bySlug = aiCallPostmortem(&json!({ "contract_slug": "FEAT-X", "trigger": "stage_lint", "dedup_since": "2026-01-01 00:00:00" }), &c2).unwrap();
        assert_eq!(bySlug, json!({ "report_id": 90, "failure_class": "vague_spec", "deduped": true, "ai_call_id": Value::Null }));
    }

    #[test]
    fn contract_slug_fallback_uses_contract_row_and_freezes_the_dedup_window() {
        let c = db();
        c.execute_batch("INSERT INTO contracts (slug, card_slug, last_failure_kind, last_failure_md) VALUES ('FEAT-Z', 'CARD-1', 'byte_drift', 'byte-check: 2 MANDATORY lines miss in README.md');
            INSERT INTO brain.ai_call_reports (id, ts, related_contract_slug, trigger, failure_class, ai_call_id) VALUES (90, '2025-12-20 00:00:00', 'FEAT-Z', 'manual', 'vague_spec', NULL);").unwrap();
        // seeded report is far older than the frozen '-10 minutes' window → dedup misses,
        // falls through to a fresh classification sourced from the contract row.
        let r = aiCallPostmortem(&json!({ "contract_slug": "FEAT-Z", "trigger": "manual" }), &c).unwrap();
        assert_eq!(r["deduped"], json!(false));
        assert_eq!(r["failure_class"], json!("byte_drift"));
        assert_eq!(r["severity"], json!("info")); // SEVERITY_DEFAULT.manual
        let card_slug: String = c.query_row("SELECT related_card_slug FROM brain.ai_call_reports WHERE ai_call_id IS NULL AND trigger = 'manual' AND id != 90", [], |row| row.get(0)).unwrap();
        assert_eq!(card_slug, "CARD-1"); // fell back to contractRow.card_slug
    }

    #[test]
    fn ai_call_reports_table_absent_returns_skipped() {
        // call_id MUST resolve (so the dedup branch actually issues a query against the
        // missing table) — omitting ai_call_reports alone, with no matching call, would just
        // fall through to the (also-failing) INSERT and hit the OTHER catch instead.
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;
            CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, version INTEGER, max_output_tokens INTEGER );
            CREATE TABLE brain.ai_calls ( id INTEGER PRIMARY KEY AUTOINCREMENT, prompt_template_id INTEGER, tier TEXT, model_id TEXT, tokens_out INTEGER, outcome TEXT, reason_call_id INTEGER );
            INSERT INTO brain.ai_calls (id, outcome) VALUES (1, 'error');").unwrap();
        let r = aiCallPostmortem(&json!({ "ai_call_id": 1, "trigger": "manual" }), &c).unwrap();
        assert_eq!(r, json!({ "skipped": true, "reason": "ai_call_reports table absent (pre-migration-135)" }));
    }

    #[test]
    fn dropped_reason_decisions_via_paired_reason_leg_and_retry_recovered() {
        let c = db();
        c.execute_batch("INSERT INTO brain.prompt_templates (id, name, version, max_output_tokens) VALUES (8, 'tl.refine.microtasks.v12', 12, 2000);
            INSERT INTO brain.ai_calls (id, prompt_template_id, tier, model_id, outcome, reason_call_id) VALUES (30, 8, 'low', 'model-e', 'validation_failed', 31);
            INSERT INTO brain.ai_calls (id, prompt_template_id, tier, model_id, outcome) VALUES (31, 8, 'high', 'model-e-reason', 'success');
            INSERT INTO brain.ai_call_attempts (ai_call_id, attempt_no, outcome, raw_output_md, validation_errors_json) VALUES
                (30, 1, 'validation_failed', 'attempt one raw output text that is long enough to avoid the refusal threshold', '[\"code-file spec names no concrete structure\"]'),
                (30, 2, 'success', 'attempt two raw output fixed', NULL);
            INSERT INTO brain.ai_call_outputs (ai_call_id, field_name, value_text) VALUES
                (31, 'file_decisions_list', 'apps/react-app/vite.config.ts: import { defineConfig } from ''vite''; export default defineConfig()');").unwrap();
        let r = aiCallPostmortem(&json!({
            "ai_call_id": 30, "contract_slug": "FEAT-PM-CONTRACT-1", "related_action": "tl.plan.contract",
            "lint_violations": [{ "microtask_id": "create-vite-config", "file": "apps/react-app/vite.config.ts", "problem": "code-file spec names no concrete structure" }],
            "failure_kind": "tl.plan.contract:stage_failed", "trigger": "stage_lint"
        }), &c).unwrap();
        assert_eq!(r["failure_class"], json!("dropped_reason_decisions"));
        assert_eq!(r["severity"], json!("error")); // SEVERITY_DEFAULT.stage_lint
        let retry_recovered: i64 = c.query_row("SELECT retry_recovered FROM brain.ai_call_reports WHERE ai_call_id = 30", [], |row| row.get(0)).unwrap();
        assert_eq!(retry_recovered, 1); // attempt 2 succeeded
    }
}
