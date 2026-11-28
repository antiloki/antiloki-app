//! reconcile-resume-analyze.rs — twin of skills/system/reconcile-resume-analyze.mjs (db READ lane,
//! cross-db brain.driver_ticks, frozen-clock 'now'). NAMES ARE CONTRACT: `reconcileResumeAnalyze`
//! (default), `gatherContractFacts` (db-first helper), `decideResume` (no-db helper),
//! `severityFor` (no-db helper). Card #384 — deterministic resume-recommendation decision tree
//! for every non-terminal contract. Strictly diagnostic; never mutates state.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS: Number(process.env.ANTILOKI_RECONCILE_CONFIDENCE_THRESHOLD ?? 0.7) — the conform harness
// never sets this env var, so the JS lane always resolves the literal default too.
const CONFIDENCE_THRESHOLD: f64 = 0.7;

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
// String(v ?? '')
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS default Number→String: whole-valued floats print without a trailing ".0".
fn num_str(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 { format!("{}", n as i64) } else { format!("{}", n) }
}
fn is_zero_number(v: &Value) -> bool { matches!(v, Value::Number(n) if n.as_f64() == Some(0.0)) }

// ── Fact gathering ───────────────────────────────────────────────────
// HELPER (db-first): gatherContractFacts(db, contract).
pub fn gatherContractFacts(db: &Connection, contract: &Value) -> Value {
    let mut facts = Map::new();
    facts.insert("contract".into(), contract.clone());

    // facts.card = contract.card_slug ? db.prepare(...).get(card_slug) : null
    // — truthy card_slug + no matching row: JS `.get()` returns undefined and the property is
    // assigned undefined, which JSON.stringify OMITS. Falsy card_slug: explicit null.
    let card_slug_v = contract.get("card_slug").cloned().unwrap_or(Value::Null);
    if truthy(Some(&card_slug_v)) {
        let rows = crate::db_json::query_rows(db, "SELECT slug, status FROM cards WHERE slug = ?", &[card_slug_v.clone()])
            .ok().and_then(|r| r.as_array().cloned()).unwrap_or_default();
        if let Some(row) = rows.into_iter().next() {
            facts.insert("card".into(), row);
        }
        // no match → undefined in JS → key stays OMITTED (do not insert)
    } else {
        facts.insert("card".into(), Value::Null);
    }

    // JS wraps this in try/catch (legacy DB without brain.driver_ticks) → [] on failure.
    let recent_ticks = crate::db_json::query_rows(
        db,
        "SELECT id, started_at, decided_action, outcome, error_message FROM brain.driver_ticks WHERE card_slug = ? ORDER BY id DESC LIMIT 5",
        &[card_slug_v.clone()],
    ).unwrap_or(json!([]));
    facts.insert("recentTicks".into(), recent_ticks);

    let contract_slug_v = contract.get("slug").cloned().unwrap_or(Value::Null);
    // JS wraps this in try/catch → 0 on failure; row?.m ?? 0 when no match / NULL m.
    let minutes_since_update: i64 = (|| -> Option<i64> {
        let rows = crate::db_json::query_rows(
            db,
            &format!("SELECT CAST(({} - julianday(updated_at)) * 24 * 60 AS INTEGER) AS m FROM contracts WHERE slug = ?", crate::clock::julianday_now_sql()),
            &[contract_slug_v.clone()],
        ).ok()?;
        let row = rows.as_array()?.first()?;
        Some(row.get("m").and_then(|v| v.as_i64()).unwrap_or(0))
    })().unwrap_or(0);
    facts.insert("minutesSinceUpdate".into(), json!(minutes_since_update));

    // safeJsonParse(contract.microtasks_json, []) — JSON.parse(s ?? '') with a catch → fallback.
    let microtasks_json_str = contract.get("microtasks_json").and_then(|v| v.as_str()).map(str::to_string).unwrap_or_default();
    let microtasks: Value = serde_json::from_str(&microtasks_json_str).unwrap_or(json!([]));
    let microtasks_total: i64 = microtasks.as_array().map(|a| a.len() as i64).unwrap_or(0);
    facts.insert("microtasksTotal".into(), json!(microtasks_total));
    let dev_progress = contract.get("dev_microtask_progress").and_then(|v| v.as_i64()).unwrap_or(0);
    let microtasks_remaining = (microtasks_total - dev_progress).max(0);
    facts.insert("microtasksRemaining".into(), json!(microtasks_remaining));

    // JS: `... ?? null` — always present, null when no row (not wrapped in try/catch: the SQL
    // is assumed valid — a missing qa_runs table is an unexercised failure path).
    let last_qa_run = crate::db_json::query_rows(
        db,
        "SELECT id, all_passed, acceptance_pass, acceptance_fail FROM qa_runs WHERE contract_slug = ? ORDER BY id DESC LIMIT 1",
        &[contract_slug_v.clone()],
    ).ok().and_then(|r| r.as_array().and_then(|a| a.first().cloned())).unwrap_or(Value::Null);
    facts.insert("lastQaRun".into(), last_qa_run);

    // JS wraps this in try/catch → null on failure; `... ?? null` on no match.
    let last_man_report = crate::db_json::query_rows(
        db,
        "SELECT id, phase, verdict FROM man_reports WHERE contract_slug = ? ORDER BY id DESC LIMIT 1",
        &[contract_slug_v],
    ).ok().and_then(|r| r.as_array().and_then(|a| a.first().cloned())).unwrap_or(Value::Null);
    facts.insert("lastManReport".into(), last_man_report);

    Value::Object(facts)
}

// ── Decision tree ────────────────────────────────────────────────────
// HELPER (no-db): decideResume(facts). Each branch returns a finding object, or null (no opinion).
pub fn decideResume(facts: &Value) -> Value {
    let contract = facts.get("contract").cloned().unwrap_or(Value::Null);
    let card = facts.get("card"); // None = key omitted (undefined); Some(Null) = explicit null
    let recent_ticks: Vec<Value> = facts.get("recentTicks").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let minutes_since_update = facts.get("minutesSinceUpdate").and_then(|v| v.as_i64()).unwrap_or(0);
    let microtasks_total = facts.get("microtasksTotal").and_then(|v| v.as_i64()).unwrap_or(0);
    let microtasks_remaining = facts.get("microtasksRemaining").and_then(|v| v.as_i64()).unwrap_or(0);
    let last_qa_run = facts.get("lastQaRun");

    let contract_status = contract.get("status").cloned().unwrap_or(Value::Null);
    let contract_status_str = contract_status.as_str().unwrap_or("");

    // 1. Parent card archived → archive the contract
    if let Some(c) = card {
        if !c.is_null() && c.get("status").and_then(|v| v.as_str()) == Some("archived") {
            return json!({
                "recommendation": "archive",
                "reasoning_md": format!(
                    "Parent card '{}' is archived. The contract has no work owner. Recommend archiving the contract via contract.admin.reject.",
                    tmpl(c.get("slug"))
                ),
                "confidence": 0.95,
            });
        }
    }

    // 2. 3+ consecutive identical-error ticks → stuck, escalate
    let last_ticks: Vec<&Value> = recent_ticks.iter().take(5).collect();
    let failed: Vec<&Value> = last_ticks.iter().filter(|t| t.get("outcome").and_then(|v| v.as_str()) == Some("failed")).cloned().collect();
    if failed.len() >= 3 {
        let sigs: Vec<String> = failed.iter().take(3)
            .map(|t| str_qq(t.get("error_message").filter(|v| !v.is_null())).chars().take(80).collect::<String>())
            .collect();
        let all_same = sigs.iter().all(|s| s == &sigs[0]);
        if all_same && !sigs[0].is_empty() {
            return json!({
                "recommendation": "escalate",
                "reasoning_md": format!(
                    "Last 3+ driver ticks failed with identical error: \"{}\". The state machine cannot self-recover; operator review required.",
                    sigs[0]
                ),
                "confidence": 0.9,
            });
        }
    }

    // 3. No tick activity in >30min AND contract is in active state → likely abandoned
    let active = matches!(contract_status_str, "proposed" | "pl_ratified" | "tl_approved" | "impl" | "qa");
    if active && minutes_since_update >= 30 && recent_ticks.is_empty() {
        return json!({
            "recommendation": "investigate",
            "reasoning_md": format!(
                "Contract status={} for {} minutes with NO driver ticks recorded. Either daemon halted, or no state-machine rule matches this state. Inspect via Pipeline mode.",
                tmpl(Some(&contract_status)), minutes_since_update
            ),
            "confidence": 0.75,
        });
    }

    // 4. Stuck in impl with all microtasks done + QA fail + no recent qa retry
    let qa_all_passed_zero = last_qa_run
        .filter(|v| !v.is_null())
        .and_then(|v| v.get("all_passed"))
        .map_or(false, is_zero_number);
    if contract_status_str == "impl" && microtasks_total > 0 && microtasks_remaining == 0
        && qa_all_passed_zero && minutes_since_update >= 15 {
        let qa = last_qa_run.unwrap();
        let pass = qa.get("acceptance_pass").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let fail = qa.get("acceptance_fail").and_then(|v| v.as_f64()).unwrap_or(0.0);
        return json!({
            "recommendation": "escalate",
            "reasoning_md": format!(
                "Contract in impl: all {} microtasks complete, but last QA run failed ({}/{} ACs). DEV has no remaining work to retry. Substrate cannot self-recover.",
                microtasks_total, num_str(fail), num_str(pass + fail)
            ),
            "confidence": 0.85,
        });
    }

    // 5. tl_approved but no progress AND not very recent → maybe MAN/sandbox stuck
    if contract_status_str == "tl_approved" && minutes_since_update >= 20 {
        return json!({
            "recommendation": "investigate",
            "reasoning_md": format!(
                "Contract has been tl_approved for {} minutes without advancing. Either MAN Phase 1 hasn't fired, sandbox creation failed silently, or the state machine has no matching rule. Check man_reports + sandbox_path.",
                minutes_since_update
            ),
            "confidence": 0.7,
        });
    }

    // 6. Recent ticks, mixed outcomes → healthy progression, no finding
    Value::Null
}

// ── Aggregator ───────────────────────────────────────────────────────
// HELPER (no-db): severityFor(recommendation, confidence).
pub fn severityFor(recommendation: &Value, confidence: &Value) -> Value {
    let rec = recommendation.as_str();
    if rec == Some("escalate") { return json!("error"); }
    if rec == Some("archive") { return json!("warn"); }
    if rec == Some("investigate") {
        let conf = confidence.as_f64().unwrap_or(f64::NAN);
        return json!(if conf >= CONFIDENCE_THRESHOLD { "warn" } else { "error" });
    }
    json!("info")
}

pub fn reconcileResumeAnalyze(_input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: const { db } = context ?? {}; if (!db) throw err.contextMissing(...) — the db lane
    // always injects a connection, so that guard-throw is never exercised here.
    let contracts_v = crate::db_json::query_rows(
        db,
        "SELECT slug, status, card_slug, sandbox_path, current_attempt, dev_microtask_progress, microtasks_json, updated_at FROM contracts WHERE status NOT IN ('done', 'superseded', 'rejected') AND superseded_by IS NULL",
        &[],
    )?;
    let contracts = contracts_v.as_array().cloned().unwrap_or_default();

    let mut findings: Vec<Value> = Vec::new();
    for c in &contracts {
        let status = c.get("status").and_then(|v| v.as_str()).unwrap_or("");
        if matches!(status, "done" | "superseded" | "rejected") { continue; } // defensive; SQL already excludes these

        let facts = gatherContractFacts(db, c);
        let decision = decideResume(&facts);
        if decision.is_null() { continue; }

        let recommendation_v = decision.get("recommendation").cloned().unwrap_or(Value::Null);
        let confidence_v = decision.get("confidence").cloned().unwrap_or(Value::Null);
        let reasoning_md_v = decision.get("reasoning_md").cloned().unwrap_or(Value::Null);
        let recommendation = recommendation_v.as_str().unwrap_or("");
        let confidence_f = confidence_v.as_f64().unwrap_or(f64::NAN);
        let severity = severityFor(&recommendation_v, &confidence_v);

        let slug = c.get("slug").cloned().unwrap_or(Value::Null);
        let card_slug = c.get("card_slug").cloned().unwrap_or(Value::Null);

        let last_qa_passed = facts.get("lastQaRun")
            .filter(|v| !v.is_null())
            .and_then(|v| v.get("all_passed"))
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(Value::Null);

        let mut detail = Map::new();
        detail.insert("contract_slug".into(), slug.clone());
        detail.insert("status".into(), c.get("status").cloned().unwrap_or(Value::Null));
        detail.insert("recommendation".into(), recommendation_v.clone());
        detail.insert("confidence".into(), confidence_v.clone());
        detail.insert("reasoning_md".into(), reasoning_md_v.clone());
        detail.insert("minutes_since_update".into(), facts.get("minutesSinceUpdate").cloned().unwrap_or(Value::Null));
        detail.insert("microtasks_total".into(), facts.get("microtasksTotal").cloned().unwrap_or(Value::Null));
        detail.insert("microtasks_remaining".into(), facts.get("microtasksRemaining").cloned().unwrap_or(Value::Null));
        detail.insert("last_qa_passed".into(), last_qa_passed);

        let suggested_action = match recommendation {
            "continue" => "No action needed — state machine will progress.",
            "escalate" => "Operator review via Pipeline mode. Use contract.admin.reject to trigger auto-redraft, or investigate via Reasoning mode.",
            "archive" => "Run contract.admin.reject with reason=\"parent card archived; abandoning the contract\".",
            "investigate" => "Open Pipeline mode + Reasoning mode for the most recent tick. Common causes: daemon halted, missing state-machine rule, sandbox-create silent failure.",
            _ => "Inspect the contract.",
        };

        let reasoning_snip: String = reasoning_md_v.as_str().unwrap_or("").chars().take(200).collect();
        let message = format!(
            "Contract '{}' ({}): {} (confidence {}). {}",
            tmpl(Some(&slug)), tmpl(c.get("status")), recommendation, format!("{:.2}", confidence_f), reasoning_snip
        );

        let mut finding = Map::new();
        finding.insert("kind".into(), json!(format!("resume_analyze_{}", recommendation)));
        finding.insert("severity".into(), severity);
        finding.insert("message".into(), json!(message));
        finding.insert("contract_slug".into(), slug);
        finding.insert("card_slug".into(), card_slug);
        finding.insert("detail".into(), Value::Object(detail));
        finding.insert("suggested_action".into(), json!(suggested_action));
        findings.push(Value::Object(finding));
    }

    // Group by severity, preserving first-seen order (mirrors JS object-key insertion order).
    let mut sev_order: Vec<String> = Vec::new();
    let mut sev_counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for f in &findings {
        let sev = f.get("severity").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if !sev_counts.contains_key(&sev) { sev_order.push(sev.clone()); }
        *sev_counts.entry(sev).or_insert(0) += 1;
    }
    let mut by_sev = Map::new();
    for sev in &sev_order { by_sev.insert(sev.clone(), json!(sev_counts[sev])); }

    let mut summary = Map::new();
    summary.insert("total".into(), json!(findings.len() as i64));
    summary.insert("contracts_examined".into(), json!(contracts.len() as i64));
    summary.insert("by_severity".into(), Value::Object(by_sev));
    summary.insert("confidence_threshold".into(), json!(CONFIDENCE_THRESHOLD));

    let mut out = Map::new();
    out.insert("ran_at".into(), json!(crate::clock::now_iso_nodb()));
    out.insert("findings".into(), json!(findings));
    out.insert("summary".into(), Value::Object(summary));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH DATABASE ':memory:' AS brain; \
             CREATE TABLE contracts (slug TEXT PRIMARY KEY, status TEXT, card_slug TEXT, sandbox_path TEXT, \
               current_attempt INTEGER, dev_microtask_progress INTEGER, microtasks_json TEXT, updated_at TEXT, superseded_by TEXT); \
             CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT); \
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, all_passed INTEGER, acceptance_pass INTEGER, acceptance_fail INTEGER); \
             CREATE TABLE man_reports (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, phase TEXT, verdict TEXT); \
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, started_at TEXT, decided_action TEXT, outcome TEXT, error_message TEXT);",
        ).unwrap();
        c
    }

    #[test]
    fn severity_mapping() {
        assert_eq!(severityFor(&json!("escalate"), &json!(0.9)), json!("error"));
        assert_eq!(severityFor(&json!("archive"), &json!(0.9)), json!("warn"));
        assert_eq!(severityFor(&json!("investigate"), &json!(0.5)), json!("error"));
        assert_eq!(severityFor(&json!("investigate"), &json!(0.8)), json!("warn"));
        assert_eq!(severityFor(&json!("continue"), &json!(0.9)), json!("info"));
    }

    #[test]
    fn decide_resume_branches() {
        // 1. archived parent card
        let d = decideResume(&json!({
            "contract": {"slug":"C","status":"tl_approved","card_slug":"CARD"},
            "card": {"slug":"CARD","status":"archived"},
            "recentTicks": [], "minutesSinceUpdate": 5, "microtasksTotal": 0, "microtasksRemaining": 0,
            "lastQaRun": null,
        }));
        assert_eq!(d["recommendation"], json!("archive"));
        assert!(d["confidence"].as_f64().unwrap() >= 0.9);

        // healthy progression → null
        let h = decideResume(&json!({
            "contract": {"slug":"C","status":"tl_approved","card_slug":"CARD"},
            "card": {"slug":"CARD","status":"backlog"},
            "recentTicks": [{"id":5,"outcome":"fired"},{"id":4,"outcome":"fired"}],
            "minutesSinceUpdate": 5, "microtasksTotal": 1, "microtasksRemaining": 1,
            "lastQaRun": null,
        }));
        assert_eq!(h, Value::Null);
    }

    #[test]
    fn aggregator_archived_card_produces_warn_finding() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status) VALUES ('CARD-A', 'archived')", []).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, status, card_slug, dev_microtask_progress, microtasks_json, updated_at, superseded_by) VALUES ('C-A', 'tl_approved', 'CARD-A', 0, '[]', '2026-01-01 00:00:00', NULL)",
            [],
        ).unwrap();
        let r = reconcileResumeAnalyze(&Value::Null, &c).unwrap();
        assert_eq!(r["summary"]["total"], json!(1));
        assert_eq!(r["findings"][0]["kind"], json!("resume_analyze_archive"));
        assert_eq!(r["findings"][0]["severity"], json!("warn"));
        assert_eq!(r["ran_at"], json!("2026-01-01T00:00:00.000Z"));
    }

    #[test]
    fn aggregator_empty_and_terminal_excluded() {
        let c = db();
        let r = reconcileResumeAnalyze(&json!({}), &c).unwrap();
        assert_eq!(r["summary"]["total"], json!(0));
        assert_eq!(r["summary"]["contracts_examined"], json!(0));

        c.execute("INSERT INTO cards (slug, status) VALUES ('CT', 'done')", []).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, status, card_slug, dev_microtask_progress, microtasks_json, updated_at, superseded_by) VALUES ('CT-1', 'done', 'CT', 0, '[]', '2026-01-01 00:00:00', NULL)",
            [],
        ).unwrap();
        let r2 = reconcileResumeAnalyze(&Value::Null, &c).unwrap();
        assert_eq!(r2["summary"]["contracts_examined"], json!(0), "done contracts excluded by query");
    }
}
