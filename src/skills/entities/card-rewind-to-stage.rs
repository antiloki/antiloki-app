//! card-rewind-to-stage.rs — twin of skills/entities/card-rewind-to-stage.mjs (standard, db
//! read+write). NAMES ARE CONTRACT: `cardRewindToStage` (default). Operator-level "rewind the
//! driver PROCESS to an earlier pipeline stage" (NOT a history revert — sacred_timeline is never
//! touched). Preview mode (confirm falsy) computes + returns what WOULD be torn down, mutating
//! nothing. Execute mode (confirm:true) applies it: resets contract status + gating flags,
//! garbage-collects artifact rows produced after the target stage, and re-arms the scheduler via
//! `card-rewind-rearm` (composed through with-audit so the rewind lands one sacred_timeline row).
//!
//! Cross-module: `cardRestartFromFailure` (redraft delegate, already converted), `qaSandboxReset`
//! (already converted, BESPOKE-NATIVE real-fs twin — see its own module doc), `withAudit`
//! (already converted; its `dispatch_skill` table gained a `card-rewind-rearm` arm alongside this
//! conversion so the rearm composition resolves).
//!
//! Datetime: `datetime('now')` writes use the frozen instant `2026-01-01 00:00:00` (playbook
//! frozen-clock convention).
//!
//! `console.error('[REWIND-DIAG] ...')` step logging in the JS source is diagnostic stderr output
//! only — it does not affect the return value or db state the conform harness compares, so it is
//! not ported.
//!
//! Fixture note (GOTCHA 9): only SUCCESS paths are fixtured. The three throw paths (card not
//! found, invalid target, no open contract for a non-redraft target) are real `Err(...)` here but
//! not fixture-exercised. Every confirm=true fixture case seeds `contracts.sandbox_path = NULL`
//! so the `reset_sandbox` branch's call into `qaSandboxReset` short-circuits to its `reset:false`
//! no-op (real filesystem deletion is never exercised through this harness — see qa-sandbox-reset.rs).
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
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
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

const POST_QA_PHASES: [&str; 3] = ["man_reviewed", "keeper_consolidated", "journey_appended"];

const VALID_TARGETS: [&str; 8] = [
    "redraft", "screening", "microtasks", "man_audit", "sandbox", "qa", "post_qa", "blueprint",
];
// NOTE: JS VALID_TARGETS also includes 'build' (Object.keys(TARGETS) — 9 entries total). Listed
// separately below to keep the const array declaration readable; the enum guard checks against
// the full 9-entry set via `all_valid_targets()`.
const VALID_TARGET_BUILD: &str = "build";
fn all_valid_targets() -> Vec<&'static str> {
    let mut v = VALID_TARGETS.to_vec();
    v.push(VALID_TARGET_BUILD);
    v
}

#[derive(Clone, Copy)]
enum ManReports {
    None,
    All,
    Phases(&'static [&'static str]),
}

struct TargetConfig {
    new_status: Option<&'static str>,
    contract_set: Option<&'static str>,
    reset_sandbox: bool,
    qa_runs: bool,
    man_reports: ManReports,
    blueprints: bool,
    supersede_designs: bool,
    blueprint_snippets: bool,
    pending_diffs: bool,
    post_qa_events: bool,
    journey: bool,
}

const QA_PASSED_REVISION_AUDIT: &[&str] = &["qa_passed", "revision_audit"];

fn target_config(target: &str) -> Option<TargetConfig> {
    match target {
        "microtasks" => Some(TargetConfig {
            new_status: Some("pl_ratified"),
            contract_set: Some(
                "status='pl_ratified', microtasks_json='[]', pl_review_count=0, dev_microtask_progress=0, last_qa_consumed_run_id=NULL, amendments_applied=0",
            ),
            reset_sandbox: true,
            qa_runs: true,
            man_reports: ManReports::All,
            blueprints: true,
            supersede_designs: false,
            blueprint_snippets: false,
            pending_diffs: true,
            post_qa_events: true,
            journey: true,
        }),
        "man_audit" => Some(TargetConfig {
            new_status: Some("tl_approved"),
            contract_set: Some(
                "status='tl_approved', pl_review_count=0, dev_microtask_progress=0, last_qa_consumed_run_id=NULL, amendments_applied=0",
            ),
            reset_sandbox: true,
            qa_runs: true,
            man_reports: ManReports::All,
            blueprints: true,
            supersede_designs: false,
            blueprint_snippets: false,
            pending_diffs: true,
            post_qa_events: true,
            journey: true,
        }),
        "sandbox" => Some(TargetConfig {
            new_status: Some("tl_approved"),
            contract_set: Some("status='tl_approved', dev_microtask_progress=0, last_qa_consumed_run_id=NULL, amendments_applied=0"),
            reset_sandbox: true,
            qa_runs: true,
            man_reports: ManReports::Phases(QA_PASSED_REVISION_AUDIT),
            blueprints: true,
            supersede_designs: false,
            blueprint_snippets: false,
            pending_diffs: true,
            post_qa_events: true,
            journey: true,
        }),
        "qa" => Some(TargetConfig {
            new_status: Some("impl"),
            contract_set: Some("status='impl', last_qa_consumed_run_id=NULL"),
            reset_sandbox: false,
            qa_runs: true,
            man_reports: ManReports::Phases(QA_PASSED_REVISION_AUDIT),
            blueprints: true,
            supersede_designs: false,
            blueprint_snippets: false,
            pending_diffs: false,
            post_qa_events: true,
            journey: true,
        }),
        "post_qa" => Some(TargetConfig {
            new_status: None,
            contract_set: None,
            reset_sandbox: false,
            qa_runs: false,
            man_reports: ManReports::None,
            blueprints: true,
            supersede_designs: false,
            blueprint_snippets: false,
            pending_diffs: false,
            post_qa_events: true,
            journey: true,
        }),
        "blueprint" => Some(TargetConfig {
            new_status: Some("pl_ratified"),
            contract_set: Some("status='pl_ratified'"),
            reset_sandbox: false,
            qa_runs: true,
            man_reports: ManReports::All,
            blueprints: true,
            supersede_designs: true,
            blueprint_snippets: false,
            pending_diffs: true,
            post_qa_events: true,
            journey: true,
        }),
        "build" => Some(TargetConfig {
            new_status: Some("tl_approved"),
            contract_set: Some("status='tl_approved'"),
            reset_sandbox: true,
            qa_runs: true,
            man_reports: ManReports::All,
            blueprints: true,
            supersede_designs: false,
            blueprint_snippets: true,
            pending_diffs: true,
            post_qa_events: true,
            journey: true,
        }),
        _ => None,
    }
}

fn count_one(db: &Connection, sql: &str, params: &[Value]) -> Result<i64, String> {
    let rows = crate::db_json::query_rows(db, sql, params)?;
    Ok(rows
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("n"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0))
}

fn count_in_phases(db: &Connection, sql_prefix: &str, id_param: &Value, phases: &[&str]) -> Result<i64, String> {
    let ph = phases.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!("{sql_prefix} ({ph})");
    let mut params: Vec<Value> = vec![id_param.clone()];
    params.extend(phases.iter().map(|p| json!(p)));
    count_one(db, &sql, &params)
}

fn gather_counts(db: &Connection, card_slug: &Value, contract_slug: &Value, m: &TargetConfig) -> Result<Map<String, Value>, String> {
    let mut out = Map::new();

    let qa_runs = if m.qa_runs {
        count_one(db, "SELECT COUNT(*) n FROM qa_runs WHERE contract_slug = ?", &[contract_slug.clone()])?
    } else {
        0
    };
    out.insert("qa_runs".into(), json!(qa_runs));

    let man_reports = match m.man_reports {
        ManReports::All => count_one(db, "SELECT COUNT(*) n FROM man_reports WHERE contract_slug = ?", &[contract_slug.clone()])?,
        ManReports::Phases(phases) => count_in_phases(
            db,
            "SELECT COUNT(*) n FROM man_reports WHERE contract_slug = ? AND phase IN",
            contract_slug,
            phases,
        )?,
        ManReports::None => 0,
    };
    out.insert("man_reports".into(), json!(man_reports));

    let blueprints = if m.blueprints {
        count_one(db, "SELECT COUNT(*) n FROM card_review_blueprints WHERE contract_slug = ?", &[contract_slug.clone()])?
    } else {
        0
    };
    out.insert("blueprints".into(), json!(blueprints));

    let pending_diffs = if m.pending_diffs {
        count_one(
            db,
            "SELECT COUNT(*) n FROM pending_diffs WHERE card_slug = ? AND status NOT IN ('superseded','rejected')",
            &[card_slug.clone()],
        )?
    } else {
        0
    };
    out.insert("pending_diffs".into(), json!(pending_diffs));

    let phase_events = if m.post_qa_events {
        count_in_phases(
            db,
            "SELECT COUNT(*) n FROM card_phase_events WHERE card_slug = ? AND phase IN",
            card_slug,
            &POST_QA_PHASES,
        )?
    } else {
        0
    };
    out.insert("phase_events".into(), json!(phase_events));

    let journey = if m.journey {
        count_one(db, "SELECT COUNT(*) n FROM project_journey WHERE card_slug = ?", &[card_slug.clone()])?
    } else {
        0
    };
    out.insert("journey".into(), json!(journey));

    let designs = if m.supersede_designs {
        count_one(
            db,
            "SELECT COUNT(*) n FROM blueprints WHERE contract_slug = ? AND superseded_by IS NULL",
            &[contract_slug.clone()],
        )?
    } else {
        0
    };
    out.insert("designs".into(), json!(designs));

    let blueprint_snippets = if m.blueprint_snippets {
        count_one(
            db,
            "SELECT COUNT(*) n FROM blueprint_snippets WHERE blueprint_id IN (SELECT id FROM blueprints WHERE contract_slug = ?)",
            &[contract_slug.clone()],
        )?
    } else {
        0
    };
    out.insert("blueprint_snippets".into(), json!(blueprint_snippets));

    Ok(out)
}

fn rearm(db: &Connection, slug: &Value) -> Result<i64, String> {
    let audit_input = json!({
        "skill": "card-rewind-rearm",
        "wrappedInput": { "slug": slug },
        "operation": "card.driver.rewind",
        "entity_type": "card",
        "entity_id": slug,
    });
    let result = crate::engine_with_audit::withAudit(&audit_input, db)?;
    Ok(result.get("cleared_notifications").and_then(|v| v.as_i64()).unwrap_or(0))
}

pub fn cardRewindToStage(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug_v = input.get("slug").cloned();
    if !truthy(nz(slug_v.as_ref())) {
        return Err("card-rewind-to-stage: slug is required".into());
    }
    let slug = slug_v.unwrap();
    let slug_s = tmpl(Some(&slug));

    let target_v = input.get("target").cloned();
    if !truthy(nz(target_v.as_ref())) {
        return Err("card-rewind-to-stage: target is required".into());
    }
    let target = target_v.unwrap().as_str().unwrap_or_default().to_string();

    let valid = all_valid_targets();
    if !valid.iter().any(|s| *s == target.as_str()) {
        return Err(format!(
            "card-rewind-to-stage: target must be one of: {} (got '{}')",
            valid.join(", "),
            target
        ));
    }

    let confirm = truthy(nz(input.get("confirm")));

    let card_rows = crate::db_json::query_rows(db, "SELECT slug, status, driver_intent FROM cards WHERE slug = ?", &[slug.clone()])?;
    let card = card_rows.as_array().and_then(|a| a.first()).cloned();
    let card = match card {
        Some(c) => c,
        None => return Err(format!("card '{}' not found", slug_s)),
    };

    if target == "redraft" {
        if !confirm {
            let mut out = Map::new();
            out.insert("preview".into(), json!(true));
            out.insert("slug".into(), slug.clone());
            out.insert("target".into(), json!("redraft"));
            out.insert("new_status".into(), json!("rejected→redraft"));
            out.insert("would_clear".into(), json!({}));
            return Ok(Value::Object(out));
        }
        let r = crate::entities_card_restart_from_failure::cardRestartFromFailure(&json!({ "slug": slug }), db)?;
        let mut out = r.as_object().cloned().unwrap_or_default();
        out.insert("target".into(), json!("redraft"));
        return Ok(Value::Object(out));
    }

    if target == "screening" {
        let ct_rows = crate::db_json::query_rows(
            db,
            "SELECT slug FROM contracts WHERE card_slug=? AND COALESCE(superseded_by,'')='' ORDER BY id DESC LIMIT 1",
            &[slug.clone()],
        )?;
        let ct = ct_rows.as_array().and_then(|a| a.first()).cloned();
        let pl_screenings = count_one(db, "SELECT COUNT(*) n FROM pl_screenings WHERE card_slug=?", &[slug.clone()])?;
        let mut would_clear = Map::new();
        would_clear.insert("contract".into(), json!(if ct.is_some() { 1 } else { 0 }));
        would_clear.insert("pl_screenings".into(), json!(pl_screenings));

        if !confirm {
            let mut out = Map::new();
            out.insert("preview".into(), json!(true));
            out.insert("slug".into(), slug.clone());
            out.insert("target".into(), json!("screening"));
            out.insert("new_status".into(), json!("backlog (re-screen)"));
            out.insert("would_clear".into(), Value::Object(would_clear));
            return Ok(Value::Object(out));
        }

        if ct.is_some() {
            crate::db_json::run_write(
                db,
                "UPDATE contracts SET superseded_by = slug WHERE card_slug=? AND COALESCE(superseded_by,'')=''",
                &[slug.clone()],
            )?;
        }
        crate::db_json::run_write(db, "DELETE FROM pl_screenings WHERE card_slug=?", &[slug.clone()])?;

        let cleared_notifications = rearm(db, &slug)?;

        let mut out = Map::new();
        out.insert("slug".into(), slug.clone());
        out.insert("intent".into(), json!("play"));
        out.insert("changed".into(), json!(true));
        out.insert("target".into(), json!("screening"));
        out.insert("new_status".into(), json!("backlog (re-screen)"));
        out.insert("cleared".into(), Value::Object(would_clear));
        out.insert("cleared_notifications".into(), json!(cleared_notifications));
        out.insert(
            "next_step".into(),
            json!("process rewound to 'screening'; the driver re-runs research→clarify→refine on the next tick"),
        );
        return Ok(Value::Object(out));
    }

    let m = target_config(&target).expect("target already validated against all_valid_targets");

    let contract_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, status, sandbox_path FROM contracts WHERE card_slug = ? AND COALESCE(superseded_by, '') = '' ORDER BY id DESC LIMIT 1",
        &[slug.clone()],
    )?;
    let contract = contract_rows.as_array().and_then(|a| a.first()).cloned();
    let contract = match contract {
        Some(c) => c,
        None => {
            return Err(format!(
                "card '{}' has no open contract — only target 'redraft' applies (it drafts a fresh contract)",
                slug_s
            ))
        }
    };
    let contract_slug = contract.get("slug").cloned().unwrap_or(Value::Null);
    let contract_status = contract.get("status").cloned().unwrap_or(Value::Null);
    let sandbox_path = contract.get("sandbox_path").cloned().unwrap_or(Value::Null);

    let mut would_clear = gather_counts(db, &slug, &contract_slug, &m)?;
    let sandbox_val = if m.reset_sandbox && truthy(Some(&sandbox_path)) {
        json!({ "path": sandbox_path.clone(), "will_delete": true })
    } else {
        json!({ "path": nz(Some(&sandbox_path)).cloned().unwrap_or(Value::Null), "will_delete": false })
    };
    would_clear.insert("sandbox".into(), sandbox_val);

    let new_status = m
        .new_status
        .map(|s| json!(s))
        .unwrap_or_else(|| contract_status.clone());

    if !confirm {
        let mut out = Map::new();
        out.insert("preview".into(), json!(true));
        out.insert("slug".into(), slug.clone());
        out.insert("target".into(), json!(target.clone()));
        out.insert("contract_slug".into(), contract_slug.clone());
        out.insert("new_status".into(), new_status.clone());
        out.insert("would_clear".into(), Value::Object(would_clear));
        return Ok(Value::Object(out));
    }

    // ── Execute ────────────────────────────────────────────────────────
    if m.reset_sandbox {
        crate::contract_qa_sandbox_reset::qaSandboxReset(
            &json!({ "contract_slug": contract_slug.clone(), "reason": format!("rewind to '{}'", target) }),
            db,
        )?;
    }

    if m.qa_runs {
        crate::db_json::run_write(
            db,
            "DELETE FROM qa_run_results WHERE qa_run_id IN (SELECT id FROM qa_runs WHERE contract_slug = ?)",
            &[contract_slug.clone()],
        )?;
        crate::db_json::run_write(db, "DELETE FROM qa_runs WHERE contract_slug = ?", &[contract_slug.clone()])?;
    }

    match m.man_reports {
        ManReports::All => {
            crate::db_json::run_write(db, "DELETE FROM man_reports WHERE contract_slug = ?", &[contract_slug.clone()])?;
        }
        ManReports::Phases(phases) => {
            let ph = phases.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("DELETE FROM man_reports WHERE contract_slug = ? AND phase IN ({ph})");
            let mut params: Vec<Value> = vec![contract_slug.clone()];
            params.extend(phases.iter().map(|p| json!(p)));
            crate::db_json::run_write(db, &sql, &params)?;
        }
        ManReports::None => {}
    }

    if m.blueprints {
        crate::db_json::run_write(db, "DELETE FROM card_review_blueprints WHERE contract_slug = ?", &[contract_slug.clone()])?;
    }

    if m.supersede_designs {
        crate::db_json::run_write(
            db,
            "UPDATE blueprints SET superseded_by = id WHERE contract_slug = ? AND superseded_by IS NULL",
            &[contract_slug.clone()],
        )?;
    }

    if m.blueprint_snippets {
        crate::db_json::run_write(
            db,
            "DELETE FROM blueprint_snippets WHERE blueprint_id IN (SELECT id FROM blueprints WHERE contract_slug = ?)",
            &[contract_slug.clone()],
        )?;
    }

    if m.pending_diffs {
        crate::db_json::run_write(
            db,
            "UPDATE pending_diffs SET status = 'superseded' WHERE card_slug = ? AND status NOT IN ('superseded','rejected')",
            &[slug.clone()],
        )?;
    }

    if m.post_qa_events {
        let ph = POST_QA_PHASES.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("DELETE FROM card_phase_events WHERE card_slug = ? AND phase IN ({ph})");
        let mut params: Vec<Value> = vec![slug.clone()];
        params.extend(POST_QA_PHASES.iter().map(|p| json!(p)));
        crate::db_json::run_write(db, &sql, &params)?;
    }

    if m.journey {
        crate::db_json::run_write(db, "DELETE FROM project_journey WHERE card_slug = ?", &[slug.clone()])?;
    }

    if let Some(contract_set) = m.contract_set {
        let sql = format!("UPDATE contracts SET {contract_set}, updated_at = {} WHERE slug = ?", crate::clock::now_sql());
        crate::db_json::run_write(db, &sql, &[contract_slug.clone()])?;
    }

    let cleared_notifications = rearm(db, &slug)?;

    let previous_intent = nz(card.get("driver_intent")).cloned().unwrap_or(json!("auto"));
    let previous_status = card.get("status").cloned().unwrap_or(Value::Null);
    let new_card_status = if previous_status == json!("blocked") || previous_status == json!("archived") {
        json!("backlog")
    } else {
        previous_status.clone()
    };

    let mut out = Map::new();
    out.insert("slug".into(), slug.clone());
    out.insert("intent".into(), json!("play"));
    out.insert("previous".into(), previous_intent);
    out.insert("open_contract_slug".into(), contract_slug.clone());
    out.insert("changed".into(), json!(true));
    out.insert("target".into(), json!(target.clone()));
    out.insert("contract_slug".into(), contract_slug);
    out.insert("prev_contract_status".into(), contract_status);
    out.insert("new_contract_status".into(), new_status.clone());
    out.insert("prev_card_status".into(), previous_status);
    out.insert("new_card_status".into(), new_card_status);
    out.insert("cleared".into(), Value::Object(would_clear));
    out.insert("cleared_notifications".into(), json!(cleared_notifications));
    out.insert(
        "next_step".into(),
        json!(format!(
            "process rewound to '{}' (contract → {}); scheduler re-walks forward from this stage on the next tick",
            target,
            new_status.as_str().unwrap_or_default()
        )),
    );
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "ATTACH ':memory:' AS memory;
             CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, driver_intent TEXT, updated_at TEXT, openrouter_session_id TEXT);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, card_slug TEXT, status TEXT, superseded_by TEXT, sandbox_path TEXT, microtasks_json TEXT, pl_review_count INTEGER, dev_microtask_progress INTEGER, last_qa_consumed_run_id INTEGER, amendments_applied INTEGER, updated_at TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT);
             CREATE TABLE qa_run_results (id INTEGER PRIMARY KEY AUTOINCREMENT, qa_run_id INTEGER);
             CREATE TABLE man_reports (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, phase TEXT);
             CREATE TABLE card_review_blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT);
             CREATE TABLE pending_diffs (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, status TEXT);
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, phase TEXT);
             CREATE TABLE project_journey (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT);
             CREATE TABLE blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, superseded_by INTEGER);
             CREATE TABLE blueprint_snippets (id INTEGER PRIMARY KEY AUTOINCREMENT, blueprint_id INTEGER);
             CREATE TABLE pl_screenings (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT);
             CREATE TABLE notifications (id INTEGER PRIMARY KEY AUTOINCREMENT, severity TEXT, dismissed_at TEXT, related_card_slug TEXT);
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT, digital_print TEXT, user_goal_id TEXT, exec_env TEXT, tick_id TEXT);",
        )
        .unwrap();
        db
    }

    #[test]
    fn invalid_target_errs() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('C1', 'impl', 'stopped')", []).unwrap();
        let err = cardRewindToStage(&json!({ "slug": "C1", "target": "martian" }), &db).unwrap_err();
        assert!(err.contains("target must be one of"));
    }

    #[test]
    fn qa_target_preview_computes_would_clear_without_mutating() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('C2', 'qa', 'stopped')", []).unwrap();
        db.execute(
            "INSERT INTO contracts (slug, card_slug, status, superseded_by, sandbox_path) VALUES ('C2-CT', 'C2', 'impl', '', NULL)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO qa_runs (contract_slug) VALUES ('C2-CT')", []).unwrap();
        db.execute("INSERT INTO qa_runs (contract_slug) VALUES ('C2-CT')", []).unwrap();
        db.execute("INSERT INTO man_reports (contract_slug, phase) VALUES ('C2-CT', 'qa_passed')", []).unwrap();

        let out = cardRewindToStage(&json!({ "slug": "C2", "target": "qa", "confirm": false }), &db).unwrap();
        assert_eq!(out["preview"], json!(true));
        assert_eq!(out["contract_slug"], json!("C2-CT"));
        assert_eq!(out["new_status"], json!("impl"));
        assert_eq!(out["would_clear"]["qa_runs"], json!(2));
        assert_eq!(out["would_clear"]["man_reports"], json!(1));
        assert_eq!(out["would_clear"]["sandbox"], json!({ "path": null, "will_delete": false }));

        let n: i64 = db.query_row("SELECT COUNT(*) FROM qa_runs", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2, "preview must not mutate");
    }

    #[test]
    fn qa_target_confirm_tears_down_and_rearms() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('C3', 'qa', 'stopped')", []).unwrap();
        db.execute(
            "INSERT INTO contracts (slug, card_slug, status, superseded_by, sandbox_path) VALUES ('C3-CT', 'C3', 'impl', '', NULL)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO qa_runs (id, contract_slug) VALUES (1, 'C3-CT')", []).unwrap();
        db.execute("INSERT INTO qa_run_results (qa_run_id) VALUES (1)", []).unwrap();
        db.execute("INSERT INTO man_reports (contract_slug, phase) VALUES ('C3-CT', 'revision_audit')", []).unwrap();

        let out = cardRewindToStage(&json!({ "slug": "C3", "target": "qa", "confirm": true }), &db).unwrap();
        assert_eq!(out["changed"], json!(true));
        assert_eq!(out["new_contract_status"], json!("impl"));
        assert_eq!(out["new_card_status"], json!("qa"));
        assert_eq!(out["cleared"]["qa_runs"], json!(1));

        let qa_count: i64 = db.query_row("SELECT COUNT(*) FROM qa_runs", [], |r| r.get(0)).unwrap();
        assert_eq!(qa_count, 0);
        let qrr_count: i64 = db.query_row("SELECT COUNT(*) FROM qa_run_results", [], |r| r.get(0)).unwrap();
        assert_eq!(qrr_count, 0);
        let cstatus: String = db.query_row("SELECT status FROM contracts WHERE slug='C3-CT'", [], |r| r.get(0)).unwrap();
        assert_eq!(cstatus, "impl");
        let intent: String = db.query_row("SELECT driver_intent FROM cards WHERE slug='C3'", [], |r| r.get(0)).unwrap();
        assert_eq!(intent, "play");
        let tl_count: i64 = db.query_row("SELECT COUNT(*) FROM sacred_timeline", [], |r| r.get(0)).unwrap();
        assert_eq!(tl_count, 1, "the rearm must land exactly one sacred_timeline row");
    }

    #[test]
    fn microtasks_target_clears_man_reports_all_and_resets_contract_fields() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('C4', 'impl', 'stopped')", []).unwrap();
        db.execute(
            "INSERT INTO contracts (slug, card_slug, status, superseded_by, sandbox_path, microtasks_json, pl_review_count) VALUES ('C4-CT', 'C4', 'pl_ratified', '', NULL, '[\"a\"]', 3)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO man_reports (contract_slug, phase) VALUES ('C4-CT', 'contract_signed')", []).unwrap();
        db.execute("INSERT INTO man_reports (contract_slug, phase) VALUES ('C4-CT', 'qa_passed')", []).unwrap();

        let out = cardRewindToStage(&json!({ "slug": "C4", "target": "microtasks", "confirm": true }), &db).unwrap();
        assert_eq!(out["new_contract_status"], json!("pl_ratified"));
        assert_eq!(out["cleared"]["man_reports"], json!(2));

        let mr_count: i64 = db.query_row("SELECT COUNT(*) FROM man_reports", [], |r| r.get(0)).unwrap();
        assert_eq!(mr_count, 0);
        let mj: String = db.query_row("SELECT microtasks_json FROM contracts WHERE slug='C4-CT'", [], |r| r.get(0)).unwrap();
        assert_eq!(mj, "[]");
    }

    #[test]
    fn post_qa_target_skips_qa_runs_and_man_reports_but_clears_journey() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('C5', 'done', 'stopped')", []).unwrap();
        db.execute(
            "INSERT INTO contracts (slug, card_slug, status, superseded_by, sandbox_path) VALUES ('C5-CT', 'C5', 'done', '', NULL)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO qa_runs (contract_slug) VALUES ('C5-CT')", []).unwrap();
        db.execute("INSERT INTO project_journey (card_slug) VALUES ('C5')", []).unwrap();
        db.execute("INSERT INTO card_phase_events (card_slug, phase) VALUES ('C5', 'man_reviewed')", []).unwrap();

        let out = cardRewindToStage(&json!({ "slug": "C5", "target": "post_qa", "confirm": true }), &db).unwrap();
        assert_eq!(out["new_contract_status"], json!("done"), "new_status falls back to contract.status when m.new_status is null");

        let qa_count: i64 = db.query_row("SELECT COUNT(*) FROM qa_runs", [], |r| r.get(0)).unwrap();
        assert_eq!(qa_count, 1, "post_qa does not touch qa_runs");
        let journey_count: i64 = db.query_row("SELECT COUNT(*) FROM project_journey", [], |r| r.get(0)).unwrap();
        assert_eq!(journey_count, 0);
        let phase_count: i64 = db.query_row("SELECT COUNT(*) FROM card_phase_events", [], |r| r.get(0)).unwrap();
        assert_eq!(phase_count, 0);
    }

    #[test]
    fn redraft_preview_returns_fixed_shape_without_mutating() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('C6', 'impl', 'stopped')", []).unwrap();
        let out = cardRewindToStage(&json!({ "slug": "C6", "target": "redraft", "confirm": false }), &db).unwrap();
        assert_eq!(
            out,
            json!({ "preview": true, "slug": "C6", "target": "redraft", "new_status": "rejected→redraft", "would_clear": {} })
        );
    }

    #[test]
    fn redraft_confirm_delegates_to_card_restart_from_failure() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('C7', 'backlog', 'stopped')", []).unwrap();
        let out = cardRewindToStage(&json!({ "slug": "C7", "target": "redraft", "confirm": true }), &db).unwrap();
        assert_eq!(out["target"], json!("redraft"));
        assert_eq!(out["intent"], json!("play"));
        let intent: String = db.query_row("SELECT driver_intent FROM cards WHERE slug='C7'", [], |r| r.get(0)).unwrap();
        assert_eq!(intent, "play");
    }

    #[test]
    fn screening_preview_and_confirm_supersedes_contract_and_clears_screenings() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('C8', 'backlog', 'stopped')", []).unwrap();
        db.execute(
            "INSERT INTO contracts (slug, card_slug, status, superseded_by) VALUES ('C8-CT', 'C8', 'draft', '')",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO pl_screenings (card_slug) VALUES ('C8')", []).unwrap();
        db.execute("INSERT INTO pl_screenings (card_slug) VALUES ('C8')", []).unwrap();

        let preview = cardRewindToStage(&json!({ "slug": "C8", "target": "screening", "confirm": false }), &db).unwrap();
        assert_eq!(preview["would_clear"], json!({ "contract": 1, "pl_screenings": 2 }));
        let still: i64 = db.query_row("SELECT COUNT(*) FROM pl_screenings", [], |r| r.get(0)).unwrap();
        assert_eq!(still, 2, "preview must not mutate");

        let out = cardRewindToStage(&json!({ "slug": "C8", "target": "screening", "confirm": true }), &db).unwrap();
        assert_eq!(out["new_status"], json!("backlog (re-screen)"));
        assert_eq!(out["cleared"], json!({ "contract": 1, "pl_screenings": 2 }));

        let sb: String = db.query_row("SELECT superseded_by FROM contracts WHERE slug='C8-CT'", [], |r| r.get(0)).unwrap();
        assert_eq!(sb, "C8-CT", "superseded_by set to the contract's own slug column value");
        let remaining: i64 = db.query_row("SELECT COUNT(*) FROM pl_screenings", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn missing_card_errs() {
        let db = setup();
        let err = cardRewindToStage(&json!({ "slug": "NOPE", "target": "qa" }), &db).unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn no_open_contract_errs_for_non_redraft_target() {
        let db = setup();
        db.execute("INSERT INTO cards (slug, status, driver_intent) VALUES ('C9', 'impl', 'stopped')", []).unwrap();
        let err = cardRewindToStage(&json!({ "slug": "C9", "target": "qa" }), &db).unwrap_err();
        assert!(err.contains("no open contract"));
    }
}
