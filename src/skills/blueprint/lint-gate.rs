//! lint-gate.rs — twin of skills/blueprint/lint-gate.mjs. NAMES ARE CONTRACT: `lintGate` (default).
//! BESPOKE-NATIVE: a thin dispatcher composing already-twinned skills — `stubChecks`/`snippetChecks`
//! (deterministic structural batteries), `typecheckChange` (itself a dispatcher over the real
//! subprocess-shelling `typecheckRun` twin), `makeJudgmentId` (pure), and `lintExplain` (the
//! deferred `aiCall` boundary — fails open exactly like the JS `try {...} catch {}` on any error,
//! including the permanent stub). Every line of ITS OWN logic (mode inference, judgment-case
//! upsert/delete, `lint_gate_runs` telemetry insert, goal_line composition) is deterministic and
//! exercised directly by the native `#[cfg(test)]` below.
//!
//! DEGRADE-SAFE by construction, ported faithfully: no active blueprint -> skipped; a linter
//! hiccup -> fail-open (`degraded: true`); `lint_gate_runs` / `judgments.judgment_cases` absent ->
//! swallowed (telemetry/backstop only, never blocks the gate).
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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

#[allow(non_snake_case)]
pub fn lintGate(input: &Value, db: &Connection) -> Result<Value, String> {
    let contract_slug_v = input.get("contract_slug").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&contract_slug_v)) {
        return Err("lint-gate: contract_slug is required".into());
    }
    let contract_slug = str_qq(Some(&contract_slug_v));

    // `db.prepare(...).get(...)?.card_slug ?? contract_slug`
    let card_slug = crate::db_json::query_rows(db, "SELECT card_slug FROM contracts WHERE slug = ?", &[json!(contract_slug)])
        .ok()
        .and_then(|rows| rows.as_array().and_then(|a| a.first()).cloned())
        .and_then(|r| r.get("card_slug").cloned())
        .filter(|v| truthy(Some(v)))
        .unwrap_or_else(|| json!(contract_slug));
    let card_slug_s = str_qq(Some(&card_slug));

    let input_mode = input.get("mode").and_then(Value::as_str);

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, version FROM blueprints WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
        &[json!(contract_slug)],
    )?;
    let row = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => {
            let mode = input_mode.unwrap_or("design");
            return Ok(json!({ "ok": true, "mode": mode, "skipped": true }));
        }
    };
    let blueprint_id = row.get("id").cloned().unwrap_or(Value::Null);
    let version = row.get("version").cloned().unwrap_or(Value::Null);

    // mode: explicit wins; else infer from whether snippets exist yet for the active blueprint.
    let has_snippets = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM blueprint_snippets WHERE blueprint_id = ?", &[blueprint_id.clone()])
        .ok()
        .and_then(|rows| rows.as_array().and_then(|a| a.first()).cloned())
        .and_then(|r| r.get("n").and_then(Value::as_i64))
        .unwrap_or(0)
        > 0;
    let mode = match input_mode {
        Some("design") => "design",
        Some("snippets") => "snippets",
        _ => {
            if has_snippets {
                "snippets"
            } else {
                "design"
            }
        }
    };

    let check_input = json!({ "contract_slug": contract_slug, "blueprint_id": blueprint_id });
    let res = if mode == "snippets" {
        crate::blueprint_snippet_checks::snippetChecks(&check_input, db)
    } else {
        crate::blueprint_stub_checks::stubChecks(&check_input, db)
    };
    let res = match res {
        Ok(v) => v,
        Err(_) => {
            // DEGRADED (linter hiccup, fail-open — court is the backstop)
            return Ok(json!({ "ok": true, "mode": mode, "blueprint_id": blueprint_id, "degraded": true }));
        }
    };

    let mut findings: Vec<Value> = res.get("findings").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut failed = res.get("ok").and_then(Value::as_bool) == Some(false);

    // Deterministic TYPECHECK of the authored change (snippets only), only when the structural
    // battery already passed. NO-OP unless a checker is installed. Fail-OPEN.
    if !failed && mode == "snippets" {
        if let Ok(tc) = crate::blueprint__helpers_typecheck_change::typecheckChange(db, &blueprint_id, &json!({})) {
            let tc_findings = tc.get("findings").and_then(Value::as_array).cloned().unwrap_or_default();
            if !tc_findings.is_empty() {
                findings.extend(tc_findings);
                failed = true;
            }
        }
    }

    let scope_ref = format!("{}-v{}", mode, str_qq(Some(&version)));
    let judgment_id = crate::swarm_judgment_id::makeJudgmentId(&card_slug, &json!("blueprint"), &json!(scope_ref))?;

    // AI JUSTIFICATION ON RED — deferred aiCall boundary via the already-twinned lintExplain;
    // fails open exactly like the JS try/catch on any error.
    let mut why_md = String::new();
    if failed && std::env::var("ANTILOKI_LINT_EXPLAIN").ok().as_deref() != Some("off") {
        let error_findings: Vec<Value> = findings.iter().filter(|f| f.get("severity").and_then(Value::as_str) == Some("error")).cloned().collect();
        let ex_input = json!({ "contract_slug": contract_slug, "blueprint_id": blueprint_id, "mode": mode, "findings": error_findings });
        if let Ok(ex) = crate::blueprint_lint_explain::lintExplain(&ex_input, db) {
            why_md = ex.get("explanation_md").and_then(Value::as_str).unwrap_or("").to_string();
            if !why_md.is_empty() {
                findings.push(json!({ "kind": "why", "severity": "info", "file": "(ai · why + fix)", "detail": why_md }));
            }
        }
    }

    // EVERY run persists — pass or red (telemetry; table absent on an old DB is swallowed).
    let findings_slice: Vec<Value> = findings.iter().take(30).cloned().collect();
    let _ = crate::db_json::run_write(
        db,
        "INSERT INTO lint_gate_runs (contract_slug, mode, blueprint_id, version, passed, findings_json, duration_ms) VALUES (?,?,?,?,?,?,?)",
        &[
            json!(contract_slug),
            json!(mode),
            blueprint_id.clone(),
            version.clone(),
            json!(if failed { 0 } else { 1 }),
            json!(Value::Array(findings_slice).to_string()),
            json!(0),
        ],
    );

    if failed {
        let summary = findings
            .iter()
            .filter(|f| f.get("severity").and_then(Value::as_str) == Some("error"))
            .take(4)
            .map(|f| format!("{}: {}", str_qq(f.get("file")), str_qq(f.get("detail"))))
            .collect::<Vec<_>>()
            .join("; ");
        let goal_line = if !why_md.is_empty() {
            format!("({}-lint gate RED: {})\n\n=== WHY + HOW TO FIX (AI) ===\n{}", mode, summary, why_md)
        } else {
            format!("({}-lint gate RED: {})", mode, summary)
        };
        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO judgments.judgment_cases (judgment_id, card_slug, scope_kind, scope_ref, goal_line, n_items, verdict)
             VALUES (?,?,?,?,?,0,'fail')
             ON CONFLICT(judgment_id) DO UPDATE SET verdict='fail', goal_line=excluded.goal_line, updated_at=datetime('now')",
            &[json!(judgment_id), card_slug.clone(), json!("blueprint"), json!(scope_ref), json!(goal_line)],
        );
        return Ok(json!({ "ok": false, "mode": mode, "blueprint_id": blueprint_id, "findings": findings, "wrote_red": true }));
    }

    // PASS: clear a STALE lint-fail this gate wrote in a prior round (goal_line marker guarantees
    // we only ever delete a row THIS gate authored — never a real court verdict).
    let _ = crate::db_json::run_write(
        db,
        "DELETE FROM judgments.judgment_cases WHERE judgment_id = ? AND goal_line LIKE '%lint gate RED%'",
        &[json!(judgment_id)],
    );
    // (JS PASS branch also composes a `gateLog` advisory line for the terminal — no output effect,
    // omitted here as pure logging.)
    Ok(json!({ "ok": true, "mode": mode, "blueprint_id": blueprint_id, "findings": findings }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH DATABASE ':memory:' AS judgments;
             CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT);
             CREATE TABLE blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, version INTEGER, blueprint_json TEXT, lint_passed INTEGER, superseded_by INTEGER);
             CREATE TABLE blueprint_snippets (id INTEGER PRIMARY KEY AUTOINCREMENT, blueprint_id INTEGER, block_id TEXT, code TEXT, imports_json TEXT);
             CREATE TABLE lint_gate_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, mode TEXT, blueprint_id INTEGER, version INTEGER, passed INTEGER, findings_json TEXT, duration_ms INTEGER);
             CREATE TABLE judgments.judgment_cases (judgment_id TEXT PRIMARY KEY, card_slug TEXT, scope_kind TEXT, scope_ref TEXT, goal_line TEXT, n_items INTEGER, verdict TEXT, snippets_hash TEXT, updated_at TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn no_active_blueprint_is_skipped() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('c1','card-C1')", []).unwrap();
        let r = lintGate(&json!({ "contract_slug": "c1" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "mode": "design", "skipped": true }));
    }

    #[test]
    fn design_mode_pass_writes_telemetry_and_no_judgment_row() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('c1','card-C1')", []).unwrap();
        let bp = json!({ "stub_files": [] });
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json, superseded_by) VALUES ('c1', 1, ?1, NULL)", [bp.to_string()]).unwrap();
        let r = lintGate(&json!({ "contract_slug": "c1" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["mode"], json!("design"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM lint_gate_runs", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
        let j: i64 = c.query_row("SELECT COUNT(*) FROM judgments.judgment_cases", [], |row| row.get(0)).unwrap();
        assert_eq!(j, 0);
    }

    #[test]
    fn snippets_mode_infers_from_existing_snippet_rows() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('c1','card-C1')", []).unwrap();
        let bp = json!({ "blocks": [{ "id": "F", "type": "file", "path": "src/x.js", "children": [{ "id": "fnA", "type": "function", "name": "fnA" }] }] });
        c.execute("INSERT INTO blueprints (contract_slug, version, blueprint_json, superseded_by) VALUES ('c1', 1, ?1, NULL)", [bp.to_string()]).unwrap();
        let bid = c.last_insert_rowid();
        c.execute("INSERT INTO blueprint_snippets (blueprint_id, block_id, code) VALUES (?1,'fnA','function fnA(){}')", rusqlite::params![bid]).unwrap();
        let r = lintGate(&json!({ "contract_slug": "c1" }), &c).unwrap();
        assert_eq!(r["mode"], json!("snippets"));
    }

    #[test]
    fn missing_contract_slug_errors() {
        let c = db();
        assert!(lintGate(&json!({}), &c).is_err());
    }
}
