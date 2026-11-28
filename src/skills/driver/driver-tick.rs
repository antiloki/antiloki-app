//! driver-tick.rs — twin of skills/driver/driver-tick.mjs. NAMES ARE CONTRACT: `driverTick`
//! (default). One iteration of the orchestration loop: insert a running tick row, check the
//! halt-file, run the scheduler (which itself runs the state machine over every candidate),
//! run the escalation pass, dispatch the chosen action via action-run, capture the timeline
//! linkage + AI cost since the tick started, and finalize the tick row.
//!
//! BESPOKE-NATIVE (Lane B — dispatcher over already-twinned skills). This module composes the
//! following already-converted twins: `driverHaltCheck`, `driverSchedule` (which itself calls
//! `driverStateMachine`), `actionInputBuilder`, `engine_action_run::skillRunAs`,
//! `engine_with_audit::withAuditAs`, `engine_log_decision::logDecision`, `ensureCardSession`,
//! `manDiagnoseFailure`, `aiCallPostmortem`, `promptQualityReport`, `heimdallAttribute`,
//! `heimdallLedgerRebuild`, `heimdallAiTagBatch`, `chainTableRef`, and the `runtime-events-bloat`
//! / `driver-ticks-bloat` reconcilers' `detect(db)`.
//!
//! PARITY-DEFERRED (documented, not silently dropped):
//!   1. The escalation pass's `memory-add` write (JS: `withAudit({ skill: 'memory-add', ... })`)
//!      has no twin reachable through `engine_with_audit`'s `dispatch_skill` table (no
//!      `"memory-add"` arm exists there, and this module may not edit with-audit.rs under the
//!      SPEED PROTOCOL write-only rule). The correction/lesson memory row is skipped; the card
//!      is still marked `blocked` and the notification/inbox rows still carry the full motive,
//!      so the operator-facing signal survives even though the memory-substrate lesson does not.
//!   2. `runWithSession` (JS: Node `AsyncLocalStorage`, threading `sessionId` through every
//!      nested AI call the dispatched action makes) has no Rust analogue — `engine_action_run`
//!      does not read a thread-local session id. `ensureCardSession` is still called (so the
//!      card's `openrouter_session_id` column stays populated exactly as JS leaves it), but the
//!      dispatch itself runs without a session wrapper.
//!   3. `loadOrCreateDriverToken` / `signDriverCall` (the driver HMAC over the dispatched
//!      action/input) are skipped: `engine_action_run::skillRunAs` has no
//!      `ANTILOKI_ENFORCE_DRIVER` / `auth-verify` hook wired to consume `driver_hmac`/`driver_ts`
//!      (its law-hook dispatch table is a fixed `KNOWN_HOOKS` set that does not include one), so
//!      computing a signature nothing downstream checks would be dead code.
//!   4. EPIC-SUBAGENT Layer B (`context.agent` / `ANTILOKI_SUBAGENT_DISPATCH`) is skipped for the
//!      same reason: `skillRunAs` has no agent-authz parameter to receive it (its action-run twin
//!      always resolves `"agent": Value::Null` in the runtime_events row). The audit-attribution
//!      half (`dispatchActor = driver:<agent>`) IS preserved — it only changes the `actor` string
//!      threaded into `skillRunAs`/`withAuditAs`, which both DO accept a real actor.
//!   5. `flushAllStaged` boot recovery and the per-phase `flushAuditStage` staging batches
//!      (EVENT-SOURCED-AUDIT Phase 2, `ANTILOKI_AUDIT_PACK`) are ported faithfully — both are
//!      exported by `engine_with_audit`.
//!
//! Frozen-clock lane: the one hardcoded relative-time literal (`datetime('now')` inside
//! `costSince`/`lastTimelineId` do not exist — JS passes `started_at` as a bound literal, not a
//! SQL relative clause) needed no adaptation; the three `datetime('now', ...)` throttle windows
//! inside the `maybe*` helpers mirror the JS literally (JS also uses `datetime('now', ...)`, not
//! a frozen constant — these are real production throttles, not conform-db fixture material).
//!
//! `context.db` is always injected by the db lane in production, so the JS
//! `if (!db) throw err.contextMissing(...)` guard is unreachable here (mirrors every sibling
//! twin's convention).

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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn env_lower(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.into()).to_lowercase()
}
fn row0(rows: &Value) -> Option<Value> {
    rows.as_array().and_then(|a| a.first()).cloned()
}
fn first_of(db: &Connection, sql: &str, params: &[Value]) -> Option<Value> {
    crate::db_json::query_rows(db, sql, params).ok().and_then(|v| row0(&v))
}

const DRIVER_ACTOR: &str = "driver:auto";
const ESCALATE_RULE: &str = "contract-impl-qa-failed-escalate";
const CONTRACT_REJECT_ESCALATE_RULE: &str = "contract-rejected-escalate-after-too-many";

fn qa_review_escalate_rules(rule: &str) -> bool {
    matches!(
        rule,
        "contract-tl-approved-qa-review-failed-escalate"
            | "contract-tl-approved-qa-review-contract-level-escalate"
            | "repeat-failure-escalate"
    )
}
fn v2_escalate_rules(rule: &str) -> bool {
    matches!(
        rule,
        "v2-design-stuck-escalate" | "v2-snippet-stuck-escalate" | "swarm-stuck-escalate"
    )
}

const HEIMDALL_FINISH_ACTIONS: &[&str] = &[
    "git-manager.worktree.merge",
    "git-manager.pr.open",
    "keeper.journey.append",
    "blueprint.build.seal",
];

// Best-effort: telemetry never fails a tick.
fn refresh_heimdall_learning(db: &Connection) {
    let _ = crate::heimdall_heimdall_attribute::heimdallAttribute(&json!({}), db);
    let _ = crate::heimdall_heimdall_ledger_rebuild::heimdallLedgerRebuild(&json!({}), db);
}

/// JS: `maybeBloatDetect(db)` — once a day, on an idle tick, run the DETECT side of the
/// retention reconcilers and surface ONE info notification when anything is prunable.
pub fn maybeBloatDetect(db: &Connection) -> Value {
    let undismissed = first_of(
        db,
        "SELECT 1 FROM notifications WHERE kind = 'bloat_detect' AND dismissed_at IS NULL LIMIT 1",
        &[],
    );
    if undismissed.is_some() {
        return json!({ "ran": false, "reason": "pending-pulse" });
    }
    let last = first_of(db, "SELECT MAX(ts) AS t FROM notifications WHERE kind = 'bloat_detect'", &[])
        .and_then(|r| r.get("t").cloned())
        .filter(|v| !v.is_null());
    if let Some(last_ts) = &last {
        let age_days = first_of(db, "SELECT julianday('now') - julianday(?) AS d", &[last_ts.clone()])
            .and_then(|r| r.get("d").and_then(Value::as_f64))
            .unwrap_or(0.0);
        if age_days < 1.0 {
            return json!({ "ran": false, "reason": "throttled" });
        }
    }
    let mut findings: Vec<Value> = Vec::new();
    findings.extend(
        crate::reconcilers_runtime_events_bloat::detect(db)
            .as_array()
            .cloned()
            .unwrap_or_default(),
    );
    findings.extend(
        crate::reconcilers_driver_ticks_bloat::detect(db)
            .as_array()
            .cloned()
            .unwrap_or_default(),
    );

    let mut timeline: Option<Value> = None;
    let ref_v = crate::audit__helpers_timeline_segments::chainTableRef(db);
    if let Some(ref_s) = ref_v.as_str() {
        let sql = format!(
            "SELECT COUNT(*) n, COALESCE(SUM(LENGTH(COALESCE(before_json,'')) + LENGTH(COALESCE(after_json,'')) + LENGTH(COALESCE(payload,''))), 0) b FROM {ref_s}"
        );
        if let Some(m) = first_of(db, &sql, &[]) {
            let n = m.get("n").and_then(Value::as_i64).unwrap_or(0);
            let b = m.get("b").and_then(Value::as_i64).unwrap_or(0);
            if n > 250_000 || b > 100 * 1_048_576 {
                timeline = Some(json!({ "rows": n, "text_bytes": b }));
            }
        }
    }

    if findings.is_empty() && timeline.is_none() {
        return json!({ "ran": true, "findings": 0 });
    }
    let mut parts: Vec<String> = findings
        .iter()
        .map(|f| {
            let n = f.get("detail").and_then(|d| d.get("prunable_rows")).and_then(Value::as_i64).unwrap_or(0);
            let kind = f.get("kind").and_then(Value::as_str).unwrap_or("").replace("-bloat", "");
            format!("{n} prunable {kind} rows")
        })
        .collect();
    if let Some(t) = &timeline {
        let rows = t.get("rows").and_then(Value::as_i64).unwrap_or(0);
        let mb = ((t.get("text_bytes").and_then(Value::as_i64).unwrap_or(0)) as f64 / 1_048_576.0).round() as i64;
        parts.push(format!(
            "sacred_timeline at {rows} rows / {mb} MB — consider timeline rotation (operator-run, BE stopped)"
        ));
    }
    let message = format!("Retention check: {} — review retention in Settings → Memory.", parts.join(" · "));
    let payload = json!({
        "findings": findings.iter().map(|f| {
            let mut m = f.get("detail").and_then(Value::as_object).cloned().unwrap_or_default();
            m.insert("kind".into(), f.get("kind").cloned().unwrap_or(Value::Null));
            Value::Object(m)
        }).collect::<Vec<_>>(),
        "timeline": timeline,
    });
    let n_findings = findings.len() as i64 + if timeline.is_some() { 1 } else { 0 };
    let _ = crate::db_json::run_write(
        db,
        "INSERT INTO notifications (kind, severity, source, message, related_card_slug, payload_json) VALUES ('bloat_detect', 'info', 'driver-tick', ?, NULL, ?)",
        &[json!(message), json!(payload.to_string())],
    );
    json!({ "ran": true, "findings": n_findings })
}

/// JS: `maybeHeimdallDigest(db)` — weekly digest notification.
pub fn maybeHeimdallDigest(db: &Connection) -> Value {
    if env_lower("ANTILOKI_HEIMDALL_DIGEST", "on") == "off" {
        return json!({ "ran": false, "reason": "env-off" });
    }
    let last = first_of(db, "SELECT MAX(ts) AS t FROM notifications WHERE kind = 'heimdall_digest'", &[])
        .and_then(|r| r.get("t").cloned())
        .filter(|v| !v.is_null());
    if let Some(last_ts) = &last {
        let age_days = first_of(db, "SELECT julianday('now') - julianday(?) AS d", &[last_ts.clone()])
            .and_then(|r| r.get("d").and_then(Value::as_f64))
            .unwrap_or(0.0);
        if age_days < 7.0 {
            return json!({ "ran": false, "reason": "throttled" });
        }
    }
    let n = |sql: &str| -> i64 { first_of(db, sql, &[]).and_then(|r| r.get("n").and_then(Value::as_i64)).unwrap_or(0) };
    let ev7 = n("SELECT COUNT(*) AS n FROM brain.heimdall_evidence WHERE ts >= datetime('now','-7 days')");
    let ht7 = n("SELECT COUNT(*) AS n FROM brain.heimdall_human_tags WHERE ts >= datetime('now','-7 days')");
    let at7 = n("SELECT COUNT(*) AS n FROM brain.heimdall_ai_tags WHERE ts >= datetime('now','-7 days')");
    let speaking = n("SELECT COUNT(DISTINCT scope_kind || '·' || scope_ref) AS n FROM brain.heimdall_ledger WHERE sample_ok = 1");
    let judge_cost = first_of(
        db,
        "SELECT ROUND(SUM(COALESCE(a.cost_usd_actual,0)),4) AS c FROM brain.ai_calls a JOIN brain.prompt_templates p ON p.id = a.prompt_template_id WHERE p.name LIKE 'heimdall.%' AND a.ts >= datetime('now','-7 days')",
        &[],
    )
    .and_then(|r| r.get("c").cloned())
    .filter(|v| !v.is_null())
    .unwrap_or(json!(0));
    if ev7 == 0 && ht7 == 0 && at7 == 0 {
        return json!({ "ran": false, "reason": "nothing moved" });
    }
    let message = format!(
        "Heimdall week: +{ev7} evidence rows · +{ht7} human tags · +{at7} AI tags · {speaking} ledger rungs speaking · judge/assist spend ${judge_cost}."
    );
    let payload = json!({
        "evidence_7d": ev7, "human_tags_7d": ht7, "ai_tags_7d": at7,
        "rungs_speaking": speaking, "heimdall_spend_7d_usd": judge_cost,
    });
    let _ = crate::db_json::run_write(
        db,
        "INSERT INTO notifications (kind, severity, source, message, related_card_slug, payload_json) VALUES ('heimdall_digest', 'info', 'heimdall', ?, NULL, ?)",
        &[json!(message), json!(payload.to_string())],
    );
    json!({ "ran": true })
}

/// JS: `maybeAiTagSweep(db, provider_fn = null)`.
pub fn maybeAiTagSweep(db: &Connection) -> Value {
    if env_lower("ANTILOKI_HEIMDALL_AITAG_SWEEP", "on") == "off" {
        return json!({ "ran": false, "reason": "env-off" });
    }
    let last = first_of(db, "SELECT MAX(ts) AS t FROM brain.heimdall_ai_tags", &[])
        .and_then(|r| r.get("t").cloned())
        .filter(|v| !v.is_null());
    if let Some(last_ts) = &last {
        let age_secs = first_of(db, "SELECT (julianday('now') - julianday(?)) * 86400.0 AS s", &[last_ts.clone()])
            .and_then(|r| r.get("s").and_then(Value::as_f64))
            .unwrap_or(0.0);
        if age_secs < 1800.0 {
            return json!({ "ran": false, "reason": "throttled" });
        }
    }
    let untagged = first_of(
        db,
        "SELECT 1 FROM brain.ai_calls a JOIN brain.heimdall_castings hc ON hc.ai_call_id = a.id \
         WHERE a.cached = 0 AND a.outcome_class IS NOT NULL AND a.outcome_class != 'provider_error' \
           AND hc.template_base != 'heimdall.ai.judge' \
           AND NOT EXISTS (SELECT 1 FROM brain.heimdall_human_tags t WHERE t.ai_call_id = a.id) \
           AND NOT EXISTS (SELECT 1 FROM brain.heimdall_ai_tags x WHERE x.target_ai_call_id = a.id) \
           AND NOT EXISTS (SELECT 1 FROM brain.heimdall_tag_skips s WHERE s.ai_call_id = a.id) \
         LIMIT 1",
        &[],
    );
    if untagged.is_none() {
        return json!({ "ran": false, "reason": "nothing-untagged" });
    }
    match crate::heimdall_heimdall_ai_tag_batch::heimdallAiTagBatch(&json!({ "limit": 8 }), db) {
        Ok(r) => json!({
            "ran": true,
            "tagged": r.get("tagged").cloned().unwrap_or(Value::Null),
            "skipped": r.get("skipped").cloned().unwrap_or(Value::Null),
        }),
        Err(_) => json!({ "ran": false, "reason": "error" }),
    }
}

fn insert_tick(db: &Connection) -> i64 {
    let reap_minutes = std::env::var("ANTILOKI_TICK_REAP_MINUTES")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(30)
        .max(1);
    let sql = format!(
        "UPDATE brain.driver_ticks SET outcome = 'failed', ended_at = datetime('now'), \
         error_message = COALESCE(error_message, 'reaped: stale tick never reached a terminal outcome (process crash/kill)') \
         WHERE outcome = 'running' AND ended_at IS NULL AND started_at < datetime('now', '-{reap_minutes} minutes')"
    );
    let _ = crate::db_json::run_write(db, &sql, &[]);
    let (_, id) = crate::db_json::run_write(db, "INSERT INTO brain.driver_ticks (outcome) VALUES ('running')", &[])
        .unwrap_or((0, 0));
    id
}

fn finalize_tick(db: &Connection, tick_id: i64, fields: &Map<String, Value>) -> Value {
    let mut sets: Vec<String> = Vec::new();
    let mut args: Vec<Value> = Vec::new();
    for (k, v) in fields {
        sets.push(format!("{k} = ?"));
        args.push(v.clone());
    }
    args.push(json!(tick_id));
    let sql = format!(
        "UPDATE brain.driver_ticks SET {}, ended_at = datetime('now') WHERE id = ?",
        sets.join(", ")
    );
    let _ = crate::db_json::run_write(db, &sql, &args);
    crate::db_json::query_rows(db, "SELECT * FROM brain.driver_ticks WHERE id = ?", &[json!(tick_id)])
        .ok()
        .and_then(|v| row0(&v))
        .unwrap_or(Value::Null)
}

/// JS: `maybeRunWeeklyPromptQuality(db, { force = false } = {})`. Module-level throttle omitted
/// (no `force`-only test caller in the Rust harness; the 7-day staleness check alone gates it).
pub fn maybeRunWeeklyPromptQuality(db: &Connection) -> Value {
    let last = first_of(
        db,
        "SELECT MAX(ts) AS t FROM brain.bench_runs WHERE bench_name LIKE 'prompt-quality:%'",
        &[],
    )
    .and_then(|r| r.get("t").cloned())
    .filter(|v| !v.is_null());
    let stale = match &last {
        None => true,
        Some(t) => first_of(db, "SELECT 1 AS s WHERE ? < datetime('now', '-7 days')", &[t.clone()]).is_some(),
    };
    if !stale {
        return json!({ "probed": true, "ran": false });
    }
    match crate::ai_prompt_quality_report::promptQualityReport(
        &json!({ "write_inbox": true, "write_memory": true, "narrative": true }),
        db,
    ) {
        Ok(r) => json!({ "probed": true, "ran": true, "run_id": r.get("run_id").cloned().unwrap_or(Value::Null) }),
        Err(_) => json!({ "probed": true, "ran": false }),
    }
}

fn cost_since(db: &Connection, started_at: &str) -> Value {
    first_of(db, "SELECT COALESCE(SUM(cost_usd_actual), 0) AS sum FROM brain.ai_calls WHERE ts >= ?", &[json!(started_at)])
        .and_then(|r| r.get("sum").cloned())
        .unwrap_or(json!(0))
}
fn last_timeline_id(db: &Connection, started_at: &str) -> Value {
    first_of(db, "SELECT id FROM sacred_timeline WHERE ts >= ? ORDER BY id DESC LIMIT 1", &[json!(started_at)])
        .and_then(|r| r.get("id").cloned())
        .unwrap_or(Value::Null)
}

fn v2_fe_group(action_name: &str) -> Option<&'static str> {
    match action_name {
        "pl.screening" | "tl.contract.assemble" | "blueprint.design" | "blueprint.judge.design" | "blueprint.plan.seal" => Some("plan"),
        "snippet.author" | "blueprint.judge.snippets" | "blueprint.apply" | "blueprint.build.seal" => Some("build"),
        _ => None,
    }
}

/// JS: `feGroupForAction(db, action_name)` (exported for tests).
pub fn feGroupForAction(db: &Connection, action_name: Option<&str>) -> Value {
    let an = match action_name {
        Some(a) if !a.is_empty() => a,
        _ => return Value::Null,
    };
    if let Some(row) = first_of(
        db,
        "SELECT s.fe_group FROM agent_stages s JOIN agent_stage_actions sa ON sa.agent_stage_id = s.id WHERE sa.action_name = ? AND s.fe_group IS NOT NULL LIMIT 1",
        &[json!(an)],
    ) {
        if let Some(g) = row.get("fe_group").filter(|v| truthy(Some(v))) {
            return g.clone();
        }
    }
    match v2_fe_group(an) {
        Some(g) => json!(g),
        None => Value::Null,
    }
}

#[allow(non_snake_case)]
pub fn driverTick(input: &Value, db: &Connection) -> Result<Value, String> {
    // EVENT-SOURCED-AUDIT (Phase 2) boot recovery. JS guards this with a once-per-process flag;
    // this twin has no persistent process-lifetime state to key that on beyond "every call", so
    // it runs the (idempotent) recovery every tick when staging is enabled — flushAllStaged is a
    // no-op when nothing is orphaned.
    if env_lower("ANTILOKI_AUDIT_PACK", "on") != "off" {
        let _ = crate::engine_with_audit::flushAllStaged(db);
    }

    let halt_path = input.get("halt_path").cloned();

    let tick_id = insert_tick(db);
    let started_at = first_of(db, "SELECT started_at FROM brain.driver_ticks WHERE id = ?", &[json!(tick_id)])
        .and_then(|r| r.get("started_at").cloned())
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();

    // ── 2. halt-file ────────────────────────────────────────────────────
    let halt_input = json!({ "halt_path": halt_path.unwrap_or(Value::Null) });
    let halted = crate::driver_driver_halt_check::driverHaltCheck(&halt_input, db)?;
    if truthy(halted.get("halted")) {
        let mut f = Map::new();
        f.insert("outcome".into(), json!("halt_requested"));
        f.insert("duration_ms".into(), json!(0));
        f.insert("decided_reason".into(), json!("halt-file present at tick start"));
        return Ok(finalize_tick(db, tick_id, &f));
    }

    // ── 3. schedule ─────────────────────────────────────────────────────
    let sched = crate::driver_driver_schedule::driverSchedule(&json!({}), db)?;
    let all_candidates = sched.get("all_candidates").and_then(Value::as_array).cloned().unwrap_or_default();
    let chosen_card = sched.get("chosen_card").cloned().unwrap_or(Value::Null);

    // ── 3a. escalation pass ─────────────────────────────────────────────
    let escalation = all_candidates.iter().find(|c| {
        let rule = c.get("decision").and_then(|d| d.get("matched_rule")).and_then(Value::as_str).unwrap_or("");
        rule == ESCALATE_RULE || rule == CONTRACT_REJECT_ESCALATE_RULE || qa_review_escalate_rules(rule) || v2_escalate_rules(rule)
    });

    if let Some(escalation) = escalation {
        let slug = escalation.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
        let decision = escalation.get("decision").cloned().unwrap_or(Value::Null);
        let rule = decision.get("matched_rule").and_then(Value::as_str).unwrap_or("").to_string();
        let reason = decision.get("reason").cloned().unwrap_or(Value::Null);
        let facts = decision.get("facts").cloned().unwrap_or(json!({}));
        let is_qa = rule == ESCALATE_RULE;
        let is_qa_review = qa_review_escalate_rules(&rule);
        let is_v2 = v2_escalate_rules(&rule);

        let mut motive = nz(facts.get("contract").and_then(|c| c.get("rejection_reason_md"))).cloned();
        let contract_slug = nz(facts.get("contract").and_then(|c| c.get("slug"))).and_then(Value::as_str).map(str::to_string);
        if !is_qa && !is_qa_review {
            if let Some(cs) = &contract_slug {
                if let Some(row) = first_of(db, "SELECT pl_last_pushback_md FROM contracts WHERE slug = ?", &[json!(cs)]) {
                    if let Some(pb) = row.get("pl_last_pushback_md").filter(|v| truthy(Some(v))) {
                        let base = motive.as_ref().and_then(Value::as_str).unwrap_or("");
                        motive = Some(json!(format!("{base}\n\n{}", pb.as_str().unwrap_or("")).trim().to_string()));
                    }
                }
            }
        }
        if is_qa_review {
            if let Some(cs) = &contract_slug {
                if let Some(review) = first_of(
                    db,
                    "SELECT findings_md, issues_json FROM contract_qa_reviews WHERE contract_slug = ? ORDER BY id DESC LIMIT 1",
                    &[json!(cs)],
                ) {
                    let mut hints_lines: Vec<String> = Vec::new();
                    if let Some(issues_raw) = review.get("issues_json").and_then(Value::as_str) {
                        if let Ok(issues) = serde_json::from_str::<Value>(issues_raw) {
                            if let Some(arr) = issues.as_array() {
                                for i in arr {
                                    if let Some(hint) = i.get("fix_hint").filter(|v| truthy(Some(v))) {
                                        let sev = i.get("severity").and_then(Value::as_str).unwrap_or("issue");
                                        hints_lines.push(format!("- [{sev}] {}", str_qq(Some(hint))));
                                    }
                                }
                            }
                        }
                    }
                    let findings = review.get("findings_md").and_then(Value::as_str).unwrap_or("");
                    let hints = hints_lines.join("\n");
                    let full = if hints.is_empty() {
                        findings.to_string()
                    } else {
                        format!("{findings}\n\n**QA's fix hints:**\n{hints}")
                    };
                    motive = Some(json!(full.trim().to_string()));
                }
            }
        }

        let mut v2_lesson: Option<Value> = None;
        let mut card_title = nz(facts.get("card").and_then(|c| c.get("title"))).cloned();
        if is_v2 {
            if card_title.is_none() {
                card_title = first_of(db, "SELECT title FROM cards WHERE slug = ?", &[json!(&slug)])
                    .and_then(|r| r.get("title").cloned());
            }
            let scope_like = if rule.contains("snippet") { "snippets-v%" } else { "design-v%" };
            if let Ok(cases_v) = crate::db_json::query_rows(
                db,
                "SELECT judgment_id, goal_line FROM judgments.judgment_cases WHERE card_slug = ? AND scope_kind = 'blueprint' AND scope_ref LIKE ? AND verdict = 'fail' ORDER BY updated_at DESC LIMIT 5",
                &[json!(&slug), json!(scope_like)],
            ) {
                let cases = cases_v.as_array().cloned().unwrap_or_default();
                let court_fails: Vec<&Value> = cases.iter().filter(|c| !c.get("goal_line").and_then(Value::as_str).unwrap_or("").starts_with('(')).collect();
                let gate_reds: Vec<&Value> = cases.iter().filter(|c| c.get("goal_line").and_then(Value::as_str).unwrap_or("").starts_with('(')).collect();
                let mut defects: Vec<String> = Vec::new();
                if let Some(first) = court_fails.first() {
                    if let Some(jid) = first.get("judgment_id") {
                        if let Ok(items) = crate::db_json::query_rows(
                            db,
                            "SELECT statement FROM judgments.judgment_items WHERE judgment_id = ? AND side = 'prosecutor' AND judge_value = 1 ORDER BY item_id LIMIT 4",
                            &[jid.clone()],
                        ) {
                            for it in items.as_array().cloned().unwrap_or_default() {
                                if let Some(s) = it.get("statement").and_then(Value::as_str) {
                                    defects.push(format!("- {s}"));
                                }
                            }
                        }
                    }
                }
                let defects_md = if !defects.is_empty() {
                    defects.join("\n")
                } else if let Some(gl) = gate_reds.first().and_then(|c| c.get("goal_line")).and_then(Value::as_str) {
                    format!("- {gl}")
                } else {
                    String::new()
                };
                v2_lesson = Some(json!({
                    "cause": format!("{} court rejection(s), {} gate RED(s)", court_fails.len(), gate_reds.len()),
                    "defects_md": defects_md,
                }));
            }
        }

        let failed_qa_ts = facts.get("lastQaRun").and_then(|r| r.get("ts")).and_then(Value::as_str).unwrap_or("unknown").to_string();
        let reason_str = reason.as_str().unwrap_or("").to_string();
        let cs_disp = contract_slug.clone().unwrap_or_else(|| "(unknown)".into());
        let situation = if is_qa {
            format!("Card {slug} failed QA after MAX_DEV_RETRIES dev.implement.code attempts (last failed qa_run at {failed_qa_ts}).")
        } else if is_qa_review {
            format!(
                "Card {slug} blocked at the QA #1 contract-review gate on contract {cs_disp} — {}.",
                if rule.contains("contract-level") {
                    "QA classified the failure as a CONTRACT defect (acceptance/scope) that microtask re-authoring cannot fix"
                } else {
                    "the re-author budget is exhausted and the review is still RED"
                }
            )
        } else if is_v2 {
            let kind = if rule.contains("snippet") { "code (snippets)" } else if rule.contains("swarm") { "swarm" } else { "blueprint design" };
            format!("Card {slug} blocked: the v2 {kind} pipeline was judged RED past its retry budget on contract {cs_disp}. Reason: {}", &reason_str.chars().take(300).collect::<String>())
        } else {
            format!("Card {slug} exhausted its auto-redraft budget on contract {cs_disp} — the auto pipeline kept rejecting the redraft (e.g. PL pushback). Reason: {}", &reason_str.chars().take(300).collect::<String>())
        };
        let motive_disp = motive.as_ref().and_then(Value::as_str).unwrap_or("").to_string();
        let guidance = if is_qa {
            "Human review required: DEV could not pass QA gate for this contract within the retry budget. Inspect contract briefing, acceptance criteria, and the failing qa_runs entries; either reset attempts (new contract, supersede), revise scope (contract.revise), or close manually with explanation.".to_string()
        } else if is_qa_review {
            let m = if motive_disp.is_empty() { "(none recorded)".to_string() } else { motive_disp.chars().take(800).collect() };
            format!("Human action required: QA's findings below include the exact fix. If the defect is in the acceptance criteria/scope, edit the card/contract accordingly, then manually rewind to TL (supreme — resets the budget). QA findings:\n{m}")
        } else if is_v2 {
            let defects_prefix = v2_lesson.as_ref().and_then(|l| l.get("defects_md")).and_then(Value::as_str).filter(|s| !s.is_empty())
                .map(|d| format!("Do NOT repeat these judged defects on the next attempt of this task:\n{d}\n\n")).unwrap_or_default();
            let cause = v2_lesson.as_ref().and_then(|l| l.get("cause")).and_then(Value::as_str).unwrap_or("retry budget exhausted");
            format!("{defects_prefix}Cause: {cause}. Human review: a low-tier judge can return a FALSE red (e.g. a degenerate court with no grounded claims) — open the Blueprint tab + the design/code judgment verdicts (judgments.sqlite, scope=blueprint) and verify BEFORE assuming the blueprint is wrong. Then rewind to re-run the stage (resets the budget) or revise the contract.")
        } else {
            let m = if motive_disp.is_empty() { "(none recorded)".to_string() } else { motive_disp.chars().take(600).collect() };
            format!("Human review required: the auto-redraft loop could not satisfy the reviewer within the budget. Read the pushback motive below, then either manually rewind (r — supreme, resets the auto budget) after fixing the steering, revise scope, or close. Motive:\n{m}")
        };

        // PARITY-DEFERRED #1: JS writes a `memory-add` correction/lesson row here via withAudit;
        // no `"memory-add"` arm exists in engine_with_audit's dispatch table (write-only rule
        // forbids adding one here). The card-block + notification/inbox rows below still carry
        // the full motive, so the operator-facing signal is not lost — only the memory-substrate
        // lesson (used for future prior-lessons matching) does not get written by this twin.
        let card_status = first_of(db, "SELECT status FROM cards WHERE slug = ?", &[json!(&slug)])
            .and_then(|r| r.get("status").and_then(Value::as_str).map(str::to_string));
        if card_status.as_deref() != Some("blocked") {
            let _ = crate::engine_with_audit::withAuditAs(
                &json!({
                    "skill": "card-status-change",
                    "wrappedInput": { "slug": slug.clone(), "status": "blocked" },
                    "operation": "card.status.change",
                    "entity_type": "card",
                    "entity_id": slug.clone(),
                }),
                db,
                DRIVER_ACTOR,
            );
        }

        let notif_message = {
            let base = if !motive_disp.is_empty() { motive_disp.clone() } else if !reason_str.is_empty() { reason_str.clone() } else { "auto budget exhausted".to_string() };
            format!("driver-tick escalated: {}", base.chars().take(200).collect::<String>())
        };
        let notif_payload = json!({ "rule": rule.clone(), "reason": reason.clone(), "motive": motive.clone(), "contract_slug": contract_slug.clone() });
        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO notifications (kind, severity, source, message, related_card_slug, payload_json) VALUES ('driver-tick-escalated', 'warn', 'driver-tick', ?, ?, ?)",
            &[json!(notif_message), json!(&slug), json!(notif_payload.to_string())],
        );

        let reason_snip: String = reason_str.chars().take(400).collect();
        let motive_snip: String = motive_disp.chars().take(800).collect();
        let body = format!(
            "## What happened\nCard `{slug}` escalated to human — the auto pipeline ran out of budget on this stage.\n\n## Why\n{}\n\n{}## What to do\n{}",
            if reason_snip.is_empty() { "(no reason recorded)".to_string() } else { reason_snip },
            if !motive_snip.is_empty() { format!("## Reviewer motive\n{motive_snip}\n\n") } else { String::new() },
            if is_qa {
                "DEV could not pass QA within the retry budget. Open the card (Enter from Board) and decide: revise scope, or close.".to_string()
            } else if is_qa_review {
                "QA reviewed the contract and the fix belongs to the CONTRACT (or the budget ran out). Apply QA's fix hints above to the card/acceptance, then manually rewind to TL (supreme — resets the budget).".to_string()
            } else if is_v2 {
                "The v2 pipeline hit its retry budget. A low-tier judge can mis-judge (false RED with no grounded claims) — check the Blueprint tab + the judge verdicts FIRST. Then rewind to re-run the stage (resets the budget) or revise scope.".to_string()
            } else {
                "The auto-redraft loop could not satisfy the reviewer. Open the card (Enter from Board) and press **r** to manually rewind (supreme — resets the auto budget) after fixing the steering, or revise scope.".to_string()
            },
        );
        let inbox_payload = json!({ "rule": rule.clone(), "reason": reason, "motive": motive, "contract_slug": contract_slug });
        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO inbox_messages (kind, subject, body_md, severity, related_card_slug, suggested_reconciler, suggested_card_action, payload_json) VALUES ('driver-tick-escalated', ?, ?, 'error', ?, ?, ?, ?)",
            &[
                json!(format!("Driver escalated: {slug}")),
                json!(body),
                json!(&slug),
                Value::Null,
                json!("restart"),
                json!(inbox_payload.to_string()),
            ],
        );

        refresh_heimdall_learning(db);

        let mut f = Map::new();
        f.insert("card_slug".into(), json!(slug));
        f.insert("decided_agent".into(), Value::Null);
        f.insert("decided_action".into(), Value::Null);
        f.insert("decided_reason".into(), json!(format!("{rule}: {}", reason_str)));
        f.insert("outcome".into(), json!("escalated"));
        f.insert("duration_ms".into(), json!(0));
        return Ok(finalize_tick(db, tick_id, &f));
    }

    if chosen_card.is_null() {
        let _ = maybeRunWeeklyPromptQuality(db);
        let _ = maybeAiTagSweep(db);
        let _ = maybeHeimdallDigest(db);
        let _ = maybeBloatDetect(db);
        for cand in &all_candidates {
            let terminal = truthy(cand.get("decision").and_then(|d| d.get("terminal")));
            let r = cand.get("decision").and_then(|d| d.get("reason")).and_then(Value::as_str).unwrap_or("").to_string();
            if !terminal || !r.to_lowercase().contains("escalate") {
                continue;
            }
            let cslug = cand.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
            let dup = first_of(
                db,
                "SELECT 1 FROM notifications WHERE related_card_slug = ? AND dismissed_at IS NULL AND json_extract(payload_json, '$.kind_detail') = 'driver-escalation' LIMIT 1",
                &[json!(&cslug)],
            );
            if dup.is_some() {
                continue;
            }
            let r_snip: String = r.chars().take(280).collect();
            let payload = json!({
                "kind_detail": "driver-escalation",
                "rule": cand.get("decision").and_then(|d| d.get("matched_rule")).cloned().unwrap_or(Value::Null),
                "reason": r.chars().take(500).collect::<String>(),
            });
            let _ = crate::db_json::run_write(
                db,
                "INSERT INTO notifications (kind, severity, source, message, related_card_slug, payload_json) VALUES ('generic', 'warn', 'driver', ?, ?, ?)",
                &[json!(format!("driver escalation — {cslug}: {r_snip}")), json!(&cslug), json!(payload.to_string())],
            );
        }
        let mut f = Map::new();
        f.insert("outcome".into(), json!("idle"));
        f.insert("duration_ms".into(), json!(0));
        f.insert(
            "decided_reason".into(),
            json!(if all_candidates.is_empty() {
                "no actionable cards (empty board)".to_string()
            } else {
                format!("{} card(s) but all terminal or awaiting human", all_candidates.len())
            }),
        );
        return Ok(finalize_tick(db, tick_id, &f));
    }

    // ── 5. decide ───────────────────────────────────────────────────────
    let decision = sched.get("decision").cloned().unwrap_or(Value::Null);
    let chosen_card_str = chosen_card.as_str().unwrap_or("").to_string();
    let next_action = decision.get("next_action").cloned().unwrap_or(Value::Null);

    // FIX-TICK-DOUBLE-FIRE in-flight guard.
    if let Some(inflight) = first_of(
        db,
        "SELECT id, decided_action FROM brain.driver_ticks WHERE card_slug = ? AND ended_at IS NULL AND outcome = 'running' AND id != ? LIMIT 1",
        &[json!(&chosen_card_str), json!(tick_id)],
    ) {
        let inflight_id = inflight.get("id").cloned().unwrap_or(Value::Null);
        let inflight_action = inflight.get("decided_action").and_then(Value::as_str).unwrap_or("deciding").to_string();
        let mut f = Map::new();
        f.insert("card_slug".into(), Value::Null);
        f.insert("decided_agent".into(), Value::Null);
        f.insert("decided_action".into(), Value::Null);
        f.insert(
            "decided_reason".into(),
            json!(format!(
                "in-flight-guard: tick #{} ({inflight_action}) is still running on {chosen_card_str} — no concurrent double-dispatch",
                inflight_id.as_i64().map(|n| n.to_string()).unwrap_or_default()
            )),
        );
        f.insert("outcome".into(), json!("idle"));
        f.insert("duration_ms".into(), json!(0));
        return Ok(finalize_tick(db, tick_id, &f));
    }

    // ── 4.5 FEAT-GENERIC-ACTION-BUDGET ──────────────────────────────────
    let action_budget: i64 = std::env::var("ANTILOKI_ACTION_BUDGET").ok().and_then(|s| s.parse().ok()).unwrap_or(5);
    if truthy(Some(&next_action)) && action_budget > 0 {
        let boundary = first_of(
            db,
            "SELECT COALESCE(MAX(id), 0) AS id FROM brain.driver_ticks WHERE card_slug = ? AND COALESCE(decided_action, '') != ? AND outcome IN ('fired', 'failed', 'escalated')",
            &[json!(&chosen_card_str), next_action.clone()],
        )
        .and_then(|r| r.get("id").and_then(Value::as_i64))
        .unwrap_or(0);
        let attempts_same_action = first_of(
            db,
            "SELECT COUNT(*) AS c FROM brain.driver_ticks WHERE card_slug = ? AND decided_action = ? AND id > ? AND outcome IN ('fired', 'failed')",
            &[json!(&chosen_card_str), next_action.clone(), json!(boundary)],
        )
        .and_then(|r| r.get("c").and_then(Value::as_i64))
        .unwrap_or(0);

        if attempts_same_action >= action_budget {
            let slug = chosen_card_str.clone();
            let contract_slug = nz(decision.get("facts").and_then(|f| f.get("contract")).and_then(|c| c.get("slug"))).and_then(Value::as_str).map(str::to_string);
            let failure_md = contract_slug
                .as_ref()
                .and_then(|cs| first_of(db, "SELECT last_failure_md FROM contracts WHERE slug = ?", &[json!(cs)]))
                .and_then(|r| r.get("last_failure_md").cloned())
                .filter(|v| !v.is_null());
            let next_action_str = next_action.as_str().unwrap_or("").to_string();
            let reason = format!(
                "action budget exhausted: '{next_action_str}' attempted {attempts_same_action}× consecutively without pipeline progress (budget {action_budget})"
            );

            let card_status = first_of(db, "SELECT status FROM cards WHERE slug = ?", &[json!(&slug)])
                .and_then(|r| r.get("status").and_then(Value::as_str).map(str::to_string));
            if card_status.as_deref() != Some("blocked") {
                let _ = crate::engine_with_audit::withAuditAs(
                    &json!({
                        "skill": "card-status-change",
                        "wrappedInput": { "slug": slug.clone(), "status": "blocked" },
                        "operation": "card.status.change",
                        "entity_type": "card",
                        "entity_id": slug.clone(),
                    }),
                    db,
                    DRIVER_ACTOR,
                );
            }
            let failure_snip: String = failure_md.as_ref().and_then(Value::as_str).unwrap_or("").chars().take(140).collect();
            let notif_message = format!("driver escalated: {reason}{}", if failure_snip.is_empty() { String::new() } else { format!(" — last failure: {failure_snip}") });
            let notif_payload = json!({ "rule": "generic-action-budget", "action": next_action.clone(), "attempts": attempts_same_action, "last_failure_md": failure_md.clone() });
            let _ = crate::db_json::run_write(
                db,
                "INSERT INTO notifications (kind, severity, source, message, related_card_slug, payload_json) VALUES ('driver-tick-escalated', 'warn', 'driver-tick', ?, ?, ?)",
                &[json!(notif_message), json!(&slug), json!(notif_payload.to_string())],
            );
            let failure_1200: String = failure_md.as_ref().and_then(Value::as_str).unwrap_or("(none recorded)").chars().take(1200).collect();
            let body = format!(
                "## What happened\nThe driver attempted `{next_action_str}` {attempts_same_action} times in a row on `{slug}` without the pipeline advancing.\n\n## Last recorded failure\n{failure_1200}\n\n## What to do\nRead the failure above, fix the cause (often the contract/acceptance), then manually rewind — manual rewind is supreme and resets this budget by changing the action stream."
            );
            let inbox_payload = json!({ "rule": "generic-action-budget", "action": next_action, "attempts": attempts_same_action });
            let _ = crate::db_json::run_write(
                db,
                "INSERT INTO inbox_messages (kind, subject, body_md, severity, related_card_slug, suggested_reconciler, suggested_card_action, payload_json) VALUES ('driver-tick-escalated', ?, ?, 'error', ?, ?, ?, ?)",
                &[
                    json!(format!("Driver escalated: {slug}")),
                    json!(body),
                    json!(&slug),
                    Value::Null,
                    json!("restart"),
                    json!(inbox_payload.to_string()),
                ],
            );
            let mut f = Map::new();
            f.insert("card_slug".into(), json!(slug));
            f.insert("decided_agent".into(), Value::Null);
            f.insert("decided_action".into(), Value::Null);
            f.insert("decided_reason".into(), json!(format!("generic-action-budget: {reason}")));
            f.insert("outcome".into(), json!("escalated"));
            f.insert("duration_ms".into(), json!(0));
            return Ok(finalize_tick(db, tick_id, &f));
        }
    }

    // FIX-IN-FLIGHT-TICK-ATTRIBUTION: populate decision columns before the (potentially long)
    // dispatch, so the running row is attributable to its card while it runs.
    let decided_agent = decision.get("next_agent").cloned().unwrap_or(Value::Null);
    let matched_rule = decision.get("matched_rule").cloned().unwrap_or(Value::Null);
    let decision_reason = decision.get("reason").cloned().unwrap_or(Value::Null);
    let decided_reason_pre = format!("{}: {}", str_qq(Some(&matched_rule)), str_qq(Some(&decision_reason)));
    let _ = crate::db_json::run_write(
        db,
        "UPDATE brain.driver_ticks SET card_slug = ?, decided_agent = ?, decided_action = ?, decided_reason = ? WHERE id = ?",
        &[chosen_card.clone(), decided_agent.clone(), next_action.clone(), json!(decided_reason_pre), json!(tick_id)],
    );

    // ── 6. dispatch ─────────────────────────────────────────────────────
    let mut fired_event_id: Value;
    let mut outcome = "fired".to_string();
    let mut error_message: Option<String> = None;

    let card_row = first_of(db, "SELECT slug, title, body_json FROM cards WHERE slug = ?", &[chosen_card.clone()]).unwrap_or(Value::Null);
    let built_input = crate::driver_action_input_builder::actionInputBuilder(
        &json!({ "action_name": next_action, "facts": decision.get("facts"), "cardRow": card_row }),
        db,
    )?;

    // PARITY-DEFERRED #2/#3/#4: session-threading (AsyncLocalStorage), the driver HMAC, and
    // subagent-authz dispatch have no consumer in engine_action_run's twin — see module doc.
    // ensureCardSession is still called for its real side effect (populating
    // cards.openrouter_session_id), matching JS's `runWithSession(sessionId, ...)` call site.
    let _session_id = crate::ai_run_session::ensureCardSession(db, &chosen_card);

    let executing_identity = decision.get("next_agent").cloned().filter(|v| !v.is_null());
    let dispatch_actor = match &executing_identity {
        Some(id) => format!("driver:{}", str_qq(Some(id))),
        None => DRIVER_ACTOR.to_string(),
    };

    let action_name_str = next_action.as_str().unwrap_or("").to_string();
    let dispatch_result = crate::engine_action_run::skillRunAs(
        &json!({ "action": action_name_str.clone(), "input": built_input }),
        db,
        &dispatch_actor,
        Some("driver"),
        &Value::Null,
    );
    match dispatch_result {
        Ok(_) => {}
        Err(e) => {
            outcome = "failed".to_string();
            error_message = Some(e);
        }
    }

    // Failed-stage recording: forward, audited event on the open contract.
    if outcome == "failed" {
        if let Some(open_contract) = first_of(
            db,
            "SELECT slug FROM contracts WHERE card_slug = ? AND COALESCE(superseded_by, '') = '' ORDER BY id DESC LIMIT 1",
            &[chosen_card.clone()],
        ) {
            let open_slug = open_contract.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
            let em: String = error_message.clone().unwrap_or_default().chars().take(1000).collect();
            let rec_input = json!({
                "contract_slug": open_slug.clone(),
                "stage": if action_name_str.is_empty() { "unknown".to_string() } else { action_name_str.clone() },
                "failure_kind": "stage_failed",
                "failure_md": em,
            });
            let _ = crate::engine_action_run::skillRunAs(
                &json!({ "action": "system.driver.stage.record.failure", "input": rec_input }),
                db,
                DRIVER_ACTOR,
                Some("driver"),
                &Value::Null,
            );

            let recent = first_of(
                db,
                "SELECT 1 FROM brain.ai_call_reports WHERE related_contract_slug = ? AND ts >= ? LIMIT 1",
                &[json!(&open_slug), json!(&started_at)],
            );
            if recent.is_none() {
                let em2: String = error_message.clone().unwrap_or_default().chars().take(2000).collect();
                let _ = crate::ai_ai_call_postmortem::aiCallPostmortem(
                    &json!({
                        "contract_slug": open_slug,
                        "related_action": if action_name_str.is_empty() { Value::Null } else { json!(action_name_str.clone()) },
                        "card_slug": chosen_card.clone(),
                        "failure_kind": "stage_failed",
                        "failure_md": em2,
                        "trigger": "driver_tick",
                    }),
                    db,
                );
            }
            let _ = crate::agents_man_man_diagnose_failure::manDiagnoseFailure(&json!({ "tick_id": tick_id, "gather_only": true }), db);
        }
    }

    // DRIVER-FAILURE-OBSERVABILITY.
    let escalated_no_action = outcome == "escalated" && next_action.is_null();
    if outcome == "failed" || escalated_no_action {
        let is_failed = outcome == "failed";
        let kind = if is_failed { "driver-tick-failed" } else { "driver-tick-escalated" };
        let em_or_reason: String = error_message.clone().unwrap_or_else(|| str_qq(Some(&decision_reason)));
        let em_or_reason = if em_or_reason.is_empty() { "unknown".to_string() } else { em_or_reason };
        let message = format!("driver-tick {outcome}: {}", em_or_reason.chars().take(200).collect::<String>());
        let notif_payload = json!({ "action": next_action.clone(), "reason": decision_reason.clone(), "error": error_message.clone() });
        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO notifications (kind, severity, source, message, related_card_slug, payload_json) VALUES (?, 'warn', 'driver-tick', ?, ?, ?)",
            &[json!(kind), json!(message), chosen_card.clone(), json!(notif_payload.to_string())],
        );

        let action_disp = if action_name_str.is_empty() { "(none)".to_string() } else { action_name_str.clone() };
        let card_disp = chosen_card.as_str().unwrap_or("(unknown)").to_string();
        let reason_disp: String = str_qq(Some(&decision_reason)).chars().take(400).collect();
        let reason_disp = if reason_disp.is_empty() { "(no reason recorded)".to_string() } else { reason_disp };
        let err_block = error_message.as_ref().map(|e| format!("## Error\n```\n{}\n```\n\n", e.chars().take(600).collect::<String>())).unwrap_or_default();
        let todo = if is_failed {
            "1. Press **x** to open the Fixit panel and check for related drift.\n2. If a red notification is halting dispatch, the `stuck-red-notifs` reconciler can clear it.\n3. If the card itself is stuck, open it (Enter from Board) and press **f** to fix detected drift on it.".to_string()
        } else {
            "This tick escalated — the state machine has no automatic path forward. Open the card (Enter from Board) and decide: **r** to restart with a fresh contract, or admin-reject if the work is no longer needed.".to_string()
        };
        let body = format!("## What happened\nDriver tick {outcome} on action **{action_disp}** for card `{card_disp}`.\n\n## Why\n{reason_disp}\n\n{err_block}## What to do\n{todo}");
        let inbox_payload = json!({ "action": next_action.clone(), "reason": decision_reason.clone(), "error": error_message.clone() });
        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO inbox_messages (kind, subject, body_md, severity, related_card_slug, suggested_reconciler, suggested_card_action, payload_json) VALUES (?, ?, ?, 'error', ?, ?, ?, ?)",
            &[
                json!(kind),
                json!(format!("Driver {outcome}: {} on {}", if action_name_str.is_empty() { "no-action".into() } else { action_name_str.clone() }, chosen_card.as_str().unwrap_or("no-card"))),
                json!(body),
                chosen_card.clone(),
                if is_failed { json!("stuck-red-notifs") } else { Value::Null },
                if is_failed { Value::Null } else { json!("restart") },
                json!(inbox_payload.to_string()),
            ],
        );
    }

    // EVENT-SOURCED-AUDIT (Phase 2): flush staged trail on failure or terminal card status.
    let audit_enabled = env_lower("ANTILOKI_AUDIT_PACK", "on") != "off";
    if audit_enabled {
        let terminal = first_of(db, "SELECT status FROM cards WHERE slug = ?", &[chosen_card.clone()])
            .and_then(|r| r.get("status").and_then(Value::as_str).map(str::to_string))
            .map(|s| s == "done" || s == "archived" || s == "blocked")
            .unwrap_or(false);
        if (outcome == "failed" || terminal) && truthy(Some(&_session_id)) {
            let _ = crate::engine_with_audit::flushAuditStage(db, &_session_id);
        }
    }

    // ── 7. capture audit linkage + cost ─────────────────────────────────
    fired_event_id = last_timeline_id(db, &started_at);
    let cost_usd = cost_since(db, &started_at);

    let log_inputs = json!({
        "candidates_count": all_candidates.len(),
        "chosen_card": chosen_card.clone(),
        "next_agent": decided_agent.clone(),
        "next_action": next_action.clone(),
        "outcome": outcome.clone(),
        "error": error_message.clone(),
    });
    let _ = crate::engine_log_decision::logDecision(
        &json!({
            "agent": "driver:auto",
            "decision": "schedule_and_dispatch",
            "card_slug": chosen_card.clone(),
            "rationale": format!("{}: {}; outcome={outcome}", str_qq(Some(&matched_rule)), str_qq(Some(&decision_reason))),
            "inputs": log_inputs,
        }),
        db,
    );

    if outcome == "fired" && HEIMDALL_FINISH_ACTIONS.contains(&action_name_str.as_str()) {
        refresh_heimdall_learning(db);
    }

    // ── 8. finalize ─────────────────────────────────────────────────────
    let facts_json = decision
        .get("facts")
        .map(|f| serde_json::to_string(f).unwrap_or_else(|_| "null".into()))
        .unwrap_or_else(|| "null".into());
    let mut f = Map::new();
    f.insert("card_slug".into(), chosen_card.clone());
    f.insert("decided_agent".into(), decided_agent);
    f.insert("decided_action".into(), next_action);
    f.insert("decided_reason".into(), json!(format!("{}: {}", str_qq(Some(&matched_rule)), str_qq(Some(&decision_reason)))));
    f.insert("fired_event_id".into(), fired_event_id.clone());
    f.insert("outcome".into(), json!(outcome));
    f.insert("cost_usd".into(), cost_usd);
    f.insert("error_message".into(), error_message.map(|e| json!(e)).unwrap_or(Value::Null));
    f.insert("duration_ms".into(), json!(0));
    f.insert("facts_json".into(), json!(facts_json));
    Ok(finalize_tick(db, tick_id, &f))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── pure escalation-rule predicates ───────────────────────────────────────────────────────
    #[test]
    fn qa_review_escalate_rules_matches_exactly_its_three_rules() {
        for r in [
            "contract-tl-approved-qa-review-failed-escalate",
            "contract-tl-approved-qa-review-contract-level-escalate",
            "repeat-failure-escalate",
        ] {
            assert!(qa_review_escalate_rules(r), "{r} must escalate");
        }
        // a near-miss must NOT match — these names gate a real escalation path
        assert!(!qa_review_escalate_rules("repeat-failure"));
        assert!(!qa_review_escalate_rules("v2-design-stuck-escalate"));
        assert!(!qa_review_escalate_rules(""));
    }

    #[test]
    fn v2_escalate_rules_is_disjoint_from_the_qa_set() {
        for r in ["v2-design-stuck-escalate", "v2-snippet-stuck-escalate", "swarm-stuck-escalate"] {
            assert!(v2_escalate_rules(r), "{r} must escalate");
            assert!(!qa_review_escalate_rules(r), "{r} must not be in BOTH sets");
        }
        assert!(!v2_escalate_rules("repeat-failure-escalate"));
    }

    #[test]
    fn heimdall_finish_actions_are_the_four_terminal_dispatches() {
        assert_eq!(HEIMDALL_FINISH_ACTIONS.len(), 4);
        assert!(HEIMDALL_FINISH_ACTIONS.contains(&"git-manager.worktree.merge"));
        assert!(HEIMDALL_FINISH_ACTIONS.contains(&"blueprint.build.seal"));
        assert!(!HEIMDALL_FINISH_ACTIONS.contains(&"board-keeper.card.add"));
    }

    // ── small JS-semantics helpers ────────────────────────────────────────────────────────────
    #[test]
    fn truthy_matches_js_falsiness() {
        assert!(!truthy(None));
        assert!(!truthy(Some(&Value::Null)));
        assert!(!truthy(Some(&json!(false))));
        assert!(!truthy(Some(&json!(0))));
        assert!(!truthy(Some(&json!(""))));
        assert!(truthy(Some(&json!("x"))));
        assert!(truthy(Some(&json!(1))));
        assert!(truthy(Some(&json!([]))), "an empty array is truthy in JS");
    }

    #[test]
    fn nz_is_the_nullish_guard_not_the_falsy_one() {
        assert!(nz(Some(&json!(0))).is_some(), "0 survives ?? unlike ||");
        assert!(nz(Some(&json!(""))).is_some());
        assert!(nz(Some(&Value::Null)).is_none());
        assert!(nz(None).is_none());
    }

    #[test]
    fn env_lower_reads_lowercased_with_a_default() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("ANTILOKI_TICK_TEST_FLAG") };
        assert_eq!(env_lower("ANTILOKI_TICK_TEST_FLAG", "on"), "on");
        unsafe { std::env::set_var("ANTILOKI_TICK_TEST_FLAG", "OFF") };
        assert_eq!(env_lower("ANTILOKI_TICK_TEST_FLAG", "on"), "off", "value is lowercased");
        unsafe { std::env::remove_var("ANTILOKI_TICK_TEST_FLAG") };
    }

    // ── the bloat-detect gate ─────────────────────────────────────────────────────────────────
    fn notif_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE notifications (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT, ts TEXT, dismissed_at TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn bloat_detect_is_skipped_while_a_pulse_is_undismissed() {
        let c = notif_db();
        c.execute_batch("INSERT INTO notifications (kind, ts, dismissed_at) VALUES ('bloat_detect', '2026-01-01 00:00:00', NULL);").unwrap();
        let r = maybeBloatDetect(&c);
        assert_eq!(r["ran"], json!(false));
        assert_eq!(r["reason"], json!("pending-pulse"), "an open pulse blocks a re-run");
    }

    #[test]
    fn bloat_detect_is_throttled_within_a_day_of_the_last_dismissed_pulse() {
        let c = notif_db();
        // dismissed, so not "pending" — but recent, so throttled
        let now: String = c.query_row("SELECT datetime('now')", [], |r| r.get(0)).unwrap();
        c.execute(
            "INSERT INTO notifications (kind, ts, dismissed_at) VALUES ('bloat_detect', ?1, ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        let r = maybeBloatDetect(&c);
        assert_eq!(r["ran"], json!(false));
        assert_eq!(r["reason"], json!("throttled"));
    }
}
