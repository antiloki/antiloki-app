//! driver-state-machine.rs — twin of skills/driver/driver-state-machine.mjs. NAMES ARE CONTRACT:
//! `driverStateMachine` (default), `resolveWorktreeEnabled` (named). Pure-READ orchestration:
//! given a card_slug, gathers ~40 facts from state.cards/contracts/pl_sessions/qa_runs/
//! brain.driver_ticks/judgments.judgment_cases/code.code_nodes and feeds them to
//! `driver_state_machine_rules::evaluateRules`.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: the single exported `driverStateMachine` unit
//! inseparably mixes the fixture-safe db-read half with a real filesystem probe —
//! `workspaceIsGitRepo` calls `getWorkspaceRoot(db)` (a DB-derived, non-input-injectable real
//! path in production) then does a real `existsSync(join(root, '.git'))` — genuinely
//! non-fixture-seedable, same category as `driver-status.rs`'s PID-liveness probe and
//! `stale-pid-files.rs`'s PID_FILE check. Because the ONE census unit here combines that fs half
//! with ~35 db queries in one function body, the whole function is bespoke-native (same
//! precedent), proven via `#[cfg(test)]` native tests instead of a conform-db fixture.
//!
//! Frozen-clock lane (applied for project-wide consistency even though this module never runs
//! through the conform-db two-lane comparison): every executed `datetime('now', ...)` ->
//! `datetime('2026-01-01 00:00:00', ...)`.
//!
//! `context.db` is always injected by the db lane in production, so the JS `if (!db) throw
//! err.contextMissing(...)` guard is unreachable here (mirrors every sibling twin's convention).
//! `card_slug` truthiness IS modeled (a real input-validation guard, not a wiring guard) — see
//! `err.required` convention used by bench-record.rs / ai-outcome-report.rs siblings.
//!
//! Cross-module calls (all pre-existing Rust twins): `crate::driver_state_machine_rules::evaluateRules`,
//! `crate::engine_log_decision::logDecision`, `crate::system_driver_snippet_gate::{snippetGateOn,
//! authoredSig, approvedSig}`, `crate::engine_workspace_root::getWorkspaceRoot`,
//! `crate::blueprint__helpers_snippets_hash::snippetsContentHash`.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::Path;

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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

// ── small db helpers (JS `.get()` / `.all()` / COUNT(*)-as-c convenience) ───────────────────
fn first_row(db: &Connection, sql: &str, params: &[Value]) -> Option<Value> {
    crate::db_json::query_rows(db, sql, params)
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first()).cloned())
}
fn all_rows(db: &Connection, sql: &str, params: &[Value]) -> Vec<Value> {
    crate::db_json::query_rows(db, sql, params)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}
fn count_c(db: &Connection, sql: &str, params: &[Value]) -> i64 {
    first_row(db, sql, params)
        .and_then(|r| r.get("c").and_then(Value::as_i64))
        .unwrap_or(0)
}

/// `export function resolveWorktreeEnabled(envVal, isGitRepo)`.
pub fn resolveWorktreeEnabled(env_val: Option<&Value>, is_git_repo: bool) -> bool {
    let v = nz(env_val).map(|x| str_qq(Some(x))).unwrap_or_else(|| "auto".to_string()).to_lowercase();
    if v == "on" {
        return true;
    }
    if v == "off" {
        return false;
    }
    is_git_repo // 'auto' (default)
}

// = JS `function workspaceIsGitRepo(db) { try { return existsSync(join(getWorkspaceRoot(db),
// '.git')); } catch { return false; } }` — real fs probe, see module doc.
fn workspace_is_git_repo(db: &Connection) -> bool {
    let root = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root_str = match root.as_str() {
        Some(s) if !s.is_empty() => s,
        _ => return false,
    };
    Path::new(root_str).join(".git").exists()
}

/// `export default function driverStateMachine(input, context)`.
pub fn driverStateMachine(input: &Value, db: &Connection) -> Result<Value, String> {
    let card_slug_v = input.get("card_slug");
    if !truthy(card_slug_v) {
        return Err("driver-state-machine: card_slug is required".to_string());
    }
    let card_slug = str_qq(card_slug_v);

    let card = first_row(
        db,
        "SELECT slug, status, skip_pl_flag, priority, body_json, COALESCE(driver_intent, 'auto') AS driver_intent, COALESCE(auto_merge, 0) AS auto_merge FROM cards WHERE slug = ?",
        &[json!(card_slug)],
    );
    let card = match card {
        Some(c) => c,
        None => {
            let mut facts = Map::new();
            facts.insert("card_slug".into(), json!(card_slug));
            facts.insert("card".into(), Value::Null);
            let mut out = Map::new();
            out.insert("next_agent".into(), Value::Null);
            out.insert("next_action".into(), Value::Null);
            out.insert("reason".into(), json!(format!("card '{card_slug}' not found")));
            out.insert("terminal".into(), json!(true));
            out.insert("matched_rule".into(), Value::Null);
            out.insert("facts".into(), Value::Object(facts));
            return Ok(Value::Object(out));
        }
    };

    let mut contract = first_row(
        db,
        "SELECT id, slug, status, tier, size, microtasks_json, current_attempt, sandbox_path, worktree_path, \
         dev_microtask_progress, pl_review_count, rejection_reason_md, failure_kind, superseded_by, \
         created_at, updated_at FROM contracts WHERE card_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
        &[json!(card_slug)],
    );

    if let Some(c) = contract.as_mut() {
        let obj = c.as_object_mut().unwrap();
        let total = obj
            .get("microtasks_json")
            .and_then(|v| nz(Some(v)))
            .map(|v| str_qq(Some(v)))
            .unwrap_or_else(|| "[]".to_string());
        let total = serde_json::from_str::<Value>(&total)
            .ok()
            .and_then(|v| v.as_array().map(|a| a.len() as i64))
            .unwrap_or(0);
        let progress = obj.get("dev_microtask_progress").and_then(Value::as_i64).unwrap_or(0);
        obj.insert("microtasks_total".into(), json!(total));
        obj.insert("microtasks_remaining".into(), json!((total - progress).max(0)));
    }

    let card_rejection_count = count_c(
        db,
        "SELECT COUNT(*) AS c FROM contracts WHERE card_slug = ? AND status = 'rejected' AND COALESCE(failure_kind, '') != 'operator_restart'",
        &[json!(card_slug)],
    );

    let auto_redrafts_since_manual = {
        let cutoff_id = first_row(
            db,
            "SELECT MAX(id) AS id FROM contracts WHERE card_slug = ? AND COALESCE(failure_kind, '') = 'operator_restart'",
            &[json!(card_slug)],
        )
        .and_then(|r| r.get("id").and_then(Value::as_i64))
        .unwrap_or(0);
        count_c(
            db,
            "SELECT COUNT(*) AS c FROM contracts WHERE card_slug = ? AND id > ? AND status = 'rejected' AND COALESCE(failure_kind, '') NOT IN ('', 'operator_restart')",
            &[json!(card_slug), json!(cutoff_id)],
        )
    };

    let man_blocked_redrafts = {
        let cutoff_id = first_row(
            db,
            "SELECT MAX(id) AS id FROM contracts WHERE card_slug = ? AND COALESCE(failure_kind, '') = 'operator_restart'",
            &[json!(card_slug)],
        )
        .and_then(|r| r.get("id").and_then(Value::as_i64))
        .unwrap_or(0);
        count_c(
            db,
            "SELECT COUNT(*) AS c FROM contracts WHERE card_slug = ? AND id > ? AND failure_kind = 'man_revision_blocked'",
            &[json!(card_slug), json!(cutoff_id)],
        )
    };

    if let Some(c) = contract.as_mut() {
        let slug = c.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
        let row = first_row(
            db,
            "SELECT id, verdict FROM man_reports WHERE contract_slug = ? AND phase = 'contract_signed' AND verdict != 'blocked' ORDER BY id DESC LIMIT 1",
            &[json!(slug)],
        );
        let obj = c.as_object_mut().unwrap();
        obj.insert("man_phase1_passed".into(), json!(row.is_some()));
        obj.insert(
            "man_phase1_report_id".into(),
            row.and_then(|r| r.get("id").cloned()).unwrap_or(Value::Null),
        );
    }

    if let Some(c) = contract.as_mut() {
        let slug = c.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
        let last_consumed = first_row(db, "SELECT last_qa_consumed_run_id FROM contracts WHERE slug = ?", &[json!(slug)])
            .and_then(|r| r.get("last_qa_consumed_run_id").and_then(Value::as_i64))
            .unwrap_or(0);
        let fresh = first_row(
            db,
            "SELECT 1 AS x FROM qa_runs WHERE contract_slug = ? AND all_passed = 0 AND id > ? AND suggested_microtasks_json IS NOT NULL AND suggested_microtasks_json NOT IN ('', '[]') LIMIT 1",
            &[json!(slug), json!(last_consumed)],
        );
        let mut has_unconsumed_amendment = false;
        let latest = first_row(
            db,
            "SELECT suggested_microtasks_json FROM qa_runs WHERE contract_slug = ? AND all_passed = 0 ORDER BY id DESC LIMIT 1",
            &[json!(slug)],
        );
        if let Some(latest) = &latest {
            if let Some(s) = latest.get("suggested_microtasks_json").and_then(Value::as_str) {
                if !s.is_empty() {
                    if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(s) {
                        has_unconsumed_amendment = arr.iter().any(|m| {
                            m.get("kind").and_then(Value::as_str) == Some("contract-amendment")
                                && m.get("consumed_at").map_or(true, |v| v.is_null())
                        });
                    }
                }
            }
        }
        let amendments_applied = first_row(db, "SELECT amendments_applied FROM contracts WHERE slug = ?", &[json!(slug)])
            .and_then(|r| r.get("amendments_applied").and_then(Value::as_i64))
            .unwrap_or(0);
        let obj = c.as_object_mut().unwrap();
        obj.insert("has_unconsumed_qa_suggestions".into(), json!(fresh.is_some()));
        obj.insert("has_unconsumed_amendment".into(), json!(has_unconsumed_amendment));
        obj.insert("amendments_applied".into(), json!(amendments_applied));
    }

    let pl_session = first_row(
        db,
        "SELECT id, status, approved_card_slug FROM pl_sessions WHERE approved_card_slug = ? AND status != 'abandoned' ORDER BY id DESC LIMIT 1",
        &[json!(card_slug)],
    );
    let mut pl_session = pl_session;
    let mut pending_questions = 0i64;
    if let Some(p) = &pl_session {
        let session_id = p.get("id").cloned().unwrap_or(Value::Null);
        pending_questions = count_c(
            db,
            "SELECT COUNT(*) AS c FROM pl_questions q WHERE q.session_id = ? AND q.step = 'clarify' AND NOT EXISTS (SELECT 1 FROM pl_answers a WHERE a.question_id = q.id)",
            &[session_id],
        );
    }
    if let Some(p) = pl_session.as_mut() {
        p.as_object_mut().unwrap().insert("pending_questions".into(), json!(pending_questions));
    }

    let last_qa_run = contract.as_ref().and_then(|c| {
        let slug = c.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
        first_row(
            db,
            "SELECT id, ts, all_passed, scope_passed, acceptance_pass, acceptance_fail, escalation_reason FROM qa_runs WHERE contract_slug = ? ORDER BY id DESC LIMIT 1",
            &[json!(slug)],
        )
    });

    let since_ts: Option<String> = match &last_qa_run {
        Some(r) if r.get("all_passed").and_then(Value::as_i64) == Some(0) => {
            r.get("ts").and_then(Value::as_str).map(|s| s.to_string())
        }
        _ => None,
    };

    let dev_attempts = match &since_ts {
        Some(ts) => count_c(
            db,
            "SELECT COUNT(*) AS c FROM brain.driver_ticks WHERE card_slug = ? AND decided_action = 'git-manager.implement.code' AND outcome = 'fired' AND started_at > ?",
            &[json!(card_slug), json!(ts)],
        ),
        None => 0,
    };

    let reauthor_attempts = match &since_ts {
        Some(ts) => count_c(
            db,
            "SELECT COUNT(*) AS c FROM brain.driver_ticks WHERE card_slug = ? AND decided_action IN ('tl.refine.microtasks', 'tl.author.snippets') AND outcome = 'fired' AND started_at > ?",
            &[json!(card_slug), json!(ts)],
        ),
        None => 0,
    };

    // ── build/test gate facts ────────────────────────────────────────────────────────────
    let mut last_build_gate: Option<Value> = None;
    let mut build_gate_fresh = false;
    let mut build_gate_reauthors = 0i64;
    let mut v2_baseline_build_done = false;
    let mut last_test_gate: Option<Value> = None;
    let mut test_gate_fresh = false;
    let mut test_gate_reds = 0i64;
    let mut v2_baseline_test_done = false;
    if let Some(c) = &contract {
        let slug = c.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
        last_build_gate = first_row(
            db,
            "SELECT id, ts, passed, skipped, build_cmd, evidence_md, applied_at, snippets_hash FROM build_gate_runs WHERE contract_slug = ? AND COALESCE(phase, 'after') = 'after' ORDER BY id DESC LIMIT 1",
            &[json!(slug)],
        );
        v2_baseline_build_done = first_row(
            db,
            "SELECT 1 AS x FROM build_gate_runs WHERE contract_slug = ? AND COALESCE(phase, 'after') = 'baseline' LIMIT 1",
            &[json!(slug)],
        )
        .is_some();
        if let Some(bg) = &last_build_gate {
            let last_impl_tick = first_row(
                db,
                "SELECT MAX(started_at) AS ts FROM brain.driver_ticks WHERE card_slug = ? AND decided_action = 'git-manager.implement.code' AND outcome = 'fired'",
                &[json!(card_slug)],
            )
            .and_then(|r| r.get("ts").cloned())
            .filter(|v| !v.is_null());
            let bg_ts = bg.get("ts").and_then(Value::as_str).unwrap_or("");
            build_gate_fresh = match &last_impl_tick {
                None => true,
                Some(t) => bg_ts >= t.as_str().unwrap_or(""),
            };
            if bg.get("passed").and_then(Value::as_i64) == Some(0) {
                build_gate_reauthors = count_c(
                    db,
                    "SELECT COUNT(*) AS c FROM build_gate_runs WHERE contract_slug = ? AND passed = 0 AND COALESCE(skipped, 0) = 0 AND COALESCE(phase, 'after') = 'after'",
                    &[json!(slug)],
                );
            }
        }
    }

    // ── contract QA #1 review freshness (with review_mode column fallback) ─────────────────
    let mut last_contract_qa_review: Option<Value> = None;
    let mut contract_qa_review_fresh = false;
    let mut contract_qa_reauthors = 0i64;
    if let Some(c) = &contract {
        let slug = c.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
        last_contract_qa_review = crate::db_json::query_rows(
            db,
            "SELECT id, ts, passed, verdict, findings_md, issues_json, reviewer, failure_target FROM contract_qa_reviews WHERE contract_slug = ? AND COALESCE(review_mode, 'full') NOT IN ('plan', 'plan-coverage', 'manifest-lint', 'composition', 'review-swarm') ORDER BY id DESC LIMIT 1",
            &[json!(slug.clone())],
        )
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first()).cloned())
        .or_else(|| {
            first_row(
                db,
                "SELECT id, ts, passed, verdict, findings_md, issues_json, reviewer, failure_target FROM contract_qa_reviews WHERE contract_slug = ? ORDER BY id DESC LIMIT 1",
                &[json!(slug.clone())],
            )
        });

        if let Some(r) = &last_contract_qa_review {
            let last_reauthor_tick = first_row(
                db,
                "SELECT MAX(started_at) AS ts FROM brain.driver_ticks WHERE card_slug = ? AND decided_action IN ('tl.refine.microtasks', 'tl.author.snippets') AND outcome = 'fired'",
                &[json!(card_slug)],
            )
            .and_then(|row| row.get("ts").cloned())
            .filter(|v| !v.is_null());
            let r_ts = r.get("ts").and_then(Value::as_str).unwrap_or("");
            contract_qa_review_fresh = match &last_reauthor_tick {
                None => true,
                Some(t) => r_ts >= t.as_str().unwrap_or(""),
            };
            if r.get("passed").and_then(Value::as_i64) == Some(0) {
                let last_green_ts = first_row(
                    db,
                    "SELECT MAX(ts) AS ts FROM contract_qa_reviews WHERE contract_slug = ? AND passed = 1",
                    &[json!(slug.clone())],
                )
                .and_then(|row| row.get("ts").and_then(Value::as_str).map(|s| s.to_string()));
                let created_at = c.get("created_at").and_then(Value::as_str).map(|s| s.to_string());
                let mut candidates: Vec<String> = Vec::new();
                if let Some(v) = last_green_ts {
                    candidates.push(v);
                }
                if let Some(v) = created_at {
                    candidates.push(v);
                }
                candidates.sort();
                let floor = candidates.pop().unwrap_or_else(|| "1970-01-01".to_string());
                contract_qa_reauthors = count_c(
                    db,
                    "SELECT COUNT(*) AS c FROM brain.driver_ticks WHERE card_slug = ? AND decided_action IN ('tl.refine.microtasks', 'tl.author.snippets') AND outcome IN ('fired', 'failed') AND started_at > ?",
                    &[json!(card_slug), json!(floor)],
                );
            }
        }
    }

    // ── plan-stage facts (TL-REFINE-STAGE-SPLIT) ────────────────────────────────────────────
    let mut has_plan_defs = false;
    let mut all_files_authored = false;
    let mut last_plan_review: Option<Value> = None;
    let mut plan_review_fresh = false;
    let mut last_plan_coverage: Option<Value> = None;
    let mut plan_coverage_fresh = false;
    if let Some(c) = &contract {
        let slug = c.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
        let raw = c.get("microtasks_json");
        let src = nz(raw).map(|v| str_qq(Some(v))).unwrap_or_else(|| "[]".to_string());
        if let Ok(Value::Array(defs)) = serde_json::from_str::<Value>(&src) {
            if !defs.is_empty() {
                has_plan_defs = true;
                all_files_authored = defs
                    .iter()
                    .all(|m| m.get("patches").and_then(Value::as_array).map_or(false, |p| !p.is_empty()));
            }
        }
        last_plan_review = first_row(
            db,
            "SELECT id, ts, passed, verdict, findings_md, issues_json, reviewer, failure_target FROM contract_qa_reviews WHERE contract_slug = ? AND review_mode = 'plan' ORDER BY id DESC LIMIT 1",
            &[json!(slug.clone())],
        );
        if let Some(r) = &last_plan_review {
            let last_plan_tick = first_row(
                db,
                "SELECT MAX(started_at) AS ts FROM brain.driver_ticks WHERE card_slug = ? AND decided_action = 'tl.plan.contract' AND outcome IN ('fired', 'failed')",
                &[json!(card_slug)],
            )
            .and_then(|row| row.get("ts").cloned())
            .filter(|v| !v.is_null());
            let r_ts = r.get("ts").and_then(Value::as_str).unwrap_or("");
            plan_review_fresh = match &last_plan_tick {
                None => true,
                Some(t) => r_ts >= t.as_str().unwrap_or(""),
            };
        }
        last_plan_coverage = first_row(
            db,
            "SELECT id, ts, passed, verdict, findings_md, issues_json, reviewer, failure_target FROM contract_qa_reviews WHERE contract_slug = ? AND review_mode = 'plan-coverage' ORDER BY id DESC LIMIT 1",
            &[json!(slug.clone())],
        );
        if let Some(r) = &last_plan_coverage {
            let last_plan_tick_c = first_row(
                db,
                "SELECT MAX(started_at) AS ts FROM brain.driver_ticks WHERE card_slug = ? AND decided_action = 'tl.plan.contract' AND outcome IN ('fired', 'failed')",
                &[json!(card_slug)],
            )
            .and_then(|row| row.get("ts").cloned())
            .filter(|v| !v.is_null());
            let r_ts = r.get("ts").and_then(Value::as_str).unwrap_or("");
            plan_coverage_fresh = match &last_plan_tick_c {
                None => true,
                Some(t) => r_ts >= t.as_str().unwrap_or(""),
            };
        }
    }

    // ── post-QA pipeline progress flags ─────────────────────────────────────────────────────
    let mut post_qa = Map::new();
    {
        let phases = all_rows(
            db,
            "SELECT phase FROM card_phase_events WHERE card_slug = ? AND phase IN ('man_reviewed','keeper_consolidated','journey_appended','atlas_integrated')",
            &[json!(card_slug)],
        );
        let set: std::collections::HashSet<String> =
            phases.iter().filter_map(|p| p.get("phase").and_then(Value::as_str).map(|s| s.to_string())).collect();
        post_qa.insert("man".into(), json!(set.contains("man_reviewed")));
        post_qa.insert("keeper".into(), json!(set.contains("keeper_consolidated")));
        post_qa.insert("journey".into(), json!(set.contains("journey_appended")));
        post_qa.insert("blueprint".into(), json!(false));
        post_qa.insert("atlas".into(), json!(set.contains("atlas_integrated")));
    }
    {
        let contract_slug = contract.as_ref().and_then(|c| c.get("slug").and_then(Value::as_str)).unwrap_or("").to_string();
        let attempt = contract.as_ref().and_then(|c| c.get("current_attempt").and_then(Value::as_i64)).unwrap_or(1);
        let blueprint_frozen = first_row(
            db,
            "SELECT 1 AS x FROM card_review_blueprints WHERE card_slug = ? AND contract_slug = ? AND attempt = ? LIMIT 1",
            &[json!(card_slug), json!(contract_slug), json!(attempt)],
        )
        .is_some();
        post_qa.insert("blueprint".into(), json!(blueprint_frozen));
    }

    let card_children = count_c(db, "SELECT COUNT(*) AS c FROM cards WHERE parent_slug = ?", &[json!(card_slug)]);

    // ── repeat-failure escalation window ────────────────────────────────────────────────────
    let mut recent_consecutive_failures = 0i64;
    let mut recent_failure_signature: Option<String> = None;
    {
        let recent_ticks = match contract.as_ref().and_then(|c| c.get("created_at").and_then(Value::as_str)) {
            Some(created_at) => all_rows(
                db,
                "SELECT outcome, error_message FROM brain.driver_ticks WHERE card_slug = ? AND started_at > MAX(?, datetime('2026-01-01 00:00:00', '-60 minutes')) ORDER BY id DESC LIMIT 10",
                &[json!(card_slug), json!(created_at)],
            ),
            None => all_rows(
                db,
                "SELECT outcome, error_message FROM brain.driver_ticks WHERE card_slug = ? AND started_at > datetime('2026-01-01 00:00:00', '-10 minutes') ORDER BY id DESC LIMIT 10",
                &[json!(card_slug)],
            ),
        };
        for t in &recent_ticks {
            if t.get("outcome").and_then(Value::as_str) != Some("failed") {
                break;
            }
            let msg = t.get("error_message").and_then(Value::as_str).unwrap_or("");
            let sig: String = msg.chars().take(80).collect();
            match &recent_failure_signature {
                None => {
                    recent_failure_signature = Some(sig);
                    recent_consecutive_failures = 1;
                }
                Some(prev) if *prev == sig => {
                    recent_consecutive_failures += 1;
                }
                _ => break,
            }
        }
    }

    // ── env-toggle facts (deploy-time, read per-tick) ───────────────────────────────────────
    let swarm_enabled = std::env::var("ANTILOKI_SWARM").unwrap_or_else(|_| "on".to_string()).to_lowercase() != "off";
    let worktree_env = std::env::var("ANTILOKI_WORKTREE").ok().map(|s| json!(s));
    let worktree_enabled = resolveWorktreeEnabled(worktree_env.as_ref(), workspace_is_git_repo(db));
    let auto_approve_enabled = std::env::var("ANTILOKI_AUTO_APPROVE").unwrap_or_else(|_| "on".to_string()).to_lowercase() != "off";
    let snippet_gate_enabled = crate::system_driver_snippet_gate::snippetGateOn();

    // ── swarm-nucleus facts ──────────────────────────────────────────────────────────────────
    let mut swarm_manifest_draft = false;
    let mut swarm_manifest_versions = 0i64;
    let mut last_manifest_lint: Option<Value> = None;
    let mut manifest_lint_fresh = false;
    let mut last_composition: Option<Value> = None;
    let mut composition_fresh = false;
    let mut swarm_authored = false;
    let mut swarm_author_versions = 0i64;
    let mut last_review_swarm: Option<Value> = None;
    let mut swarm_reviewed = false;
    let mut last_swarm_coverage: Option<Value> = None;
    let mut swarm_coverage_fresh = false;
    let mut swarm_coverage_green = false;
    if swarm_enabled {
        if let Some(c) = &contract {
            let slug = c.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
            if let Some(mrow) = first_row(
                db,
                "SELECT COUNT(*) AS n, MAX(CASE WHEN superseded_by IS NULL THEN 1 ELSE 0 END) AS active FROM swarm_manifests WHERE contract_slug = ?",
                &[json!(slug.clone())],
            ) {
                swarm_manifest_versions = mrow.get("n").and_then(Value::as_i64).unwrap_or(0);
                swarm_manifest_draft = truthy(mrow.get("active"));
            }
            last_manifest_lint = first_row(
                db,
                "SELECT id, ts, passed, verdict, findings_md, failure_target FROM contract_qa_reviews WHERE contract_slug = ? AND review_mode = 'manifest-lint' ORDER BY id DESC LIMIT 1",
                &[json!(slug.clone())],
            );
            if let Some(r) = &last_manifest_lint {
                let last_design_tick = first_row(
                    db,
                    "SELECT MAX(started_at) AS ts FROM brain.driver_ticks WHERE card_slug = ? AND decided_action = 'swarm.manifest.design' AND outcome IN ('fired', 'failed')",
                    &[json!(card_slug)],
                )
                .and_then(|row| row.get("ts").cloned())
                .filter(|v| !v.is_null());
                let r_ts = r.get("ts").and_then(Value::as_str).unwrap_or("");
                manifest_lint_fresh = match &last_design_tick {
                    None => true,
                    Some(t) => r_ts >= t.as_str().unwrap_or(""),
                };
            }
            last_composition = first_row(
                db,
                "SELECT id, ts, passed, verdict, findings_md, failure_target FROM contract_qa_reviews WHERE contract_slug = ? AND review_mode = 'composition' ORDER BY id DESC LIMIT 1",
                &[json!(slug.clone())],
            );
            if last_composition.is_some() {
                swarm_author_versions = count_c(
                    db,
                    "SELECT COUNT(*) AS c FROM brain.driver_ticks WHERE card_slug = ? AND decided_action = 'swarm.author.blocks' AND outcome IN ('fired', 'failed')",
                    &[json!(card_slug)],
                );
            }
            if let Some(r) = &last_composition {
                let last_author_tick = first_row(
                    db,
                    "SELECT MAX(started_at) AS ts FROM brain.driver_ticks WHERE card_slug = ? AND decided_action = 'swarm.author.blocks' AND outcome IN ('fired', 'failed')",
                    &[json!(card_slug)],
                )
                .and_then(|row| row.get("ts").cloned())
                .filter(|v| !v.is_null());
                let r_ts = r.get("ts").and_then(Value::as_str).unwrap_or("");
                composition_fresh = match &last_author_tick {
                    None => true,
                    Some(t) => r_ts >= t.as_str().unwrap_or(""),
                };
                swarm_authored = composition_fresh && r.get("passed").and_then(Value::as_i64) == Some(1);
            }
            last_review_swarm = first_row(
                db,
                "SELECT id, ts, passed FROM contract_qa_reviews WHERE contract_slug = ? AND review_mode = 'review-swarm' ORDER BY id DESC LIMIT 1",
                &[json!(slug.clone())],
            );
            if let Some(r) = &last_review_swarm {
                let last_author_tick_r = first_row(
                    db,
                    "SELECT MAX(started_at) AS ts FROM brain.driver_ticks WHERE card_slug = ? AND decided_action = 'swarm.author.blocks' AND outcome IN ('fired', 'failed')",
                    &[json!(card_slug)],
                )
                .and_then(|row| row.get("ts").cloned())
                .filter(|v| !v.is_null());
                let r_ts = r.get("ts").and_then(Value::as_str).unwrap_or("");
                swarm_reviewed = match &last_author_tick_r {
                    None => true,
                    Some(t) => r_ts >= t.as_str().unwrap_or(""),
                };
            }
            last_swarm_coverage = first_row(
                db,
                "SELECT id, ts, passed FROM contract_qa_reviews WHERE contract_slug = ? AND review_mode = 'swarm-coverage' ORDER BY id DESC LIMIT 1",
                &[json!(slug.clone())],
            );
            if let Some(r) = &last_swarm_coverage {
                let last_author_tick_c = first_row(
                    db,
                    "SELECT MAX(started_at) AS ts FROM brain.driver_ticks WHERE card_slug = ? AND decided_action = 'swarm.author.blocks' AND outcome IN ('fired', 'failed')",
                    &[json!(card_slug)],
                )
                .and_then(|row| row.get("ts").cloned())
                .filter(|v| !v.is_null());
                let r_ts = r.get("ts").and_then(Value::as_str).unwrap_or("");
                swarm_coverage_fresh = match &last_author_tick_c {
                    None => true,
                    Some(t) => r_ts >= t.as_str().unwrap_or(""),
                };
                swarm_coverage_green = swarm_coverage_fresh && r.get("passed").and_then(Value::as_i64) == Some(1);
            }
        }
    }

    // ── PIPELINE-V2 facts ────────────────────────────────────────────────────────────────────
    let pipeline_v2_enabled = std::env::var("ANTILOKI_PIPELINE_V2").unwrap_or_else(|_| "on".to_string()).to_lowercase() != "off";
    let mut v2_screening_fresh = false;
    let mut v2_blueprint_active = false;
    let mut v2_blueprint_id: Value = Value::Null;
    let mut v2_design_versions = 0i64;
    let mut v2_design_court_rejections = 0i64;
    let mut v2_design_judge_fresh = false;
    let mut v2_design_judge_passed = false;
    let mut v2_snippets_authored = false;
    let mut v2_author_versions = 0i64;
    let mut v2_snippet_judge_fresh = false;
    let mut v2_snippet_judge_passed = false;
    let mut v2_snippet_defect_in_blueprint = false;
    let mut v2_build_sealed = false;
    if pipeline_v2_enabled {
        v2_screening_fresh = first_row(db, "SELECT 1 AS x FROM pl_screenings WHERE card_slug = ? LIMIT 1", &[json!(card_slug)]).is_some();
        if let Some(c) = &contract {
            let slug = c.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
            let mut active_version: Option<Value> = None;
            if let Some(m) = first_row(
                db,
                "SELECT id, version FROM blueprints WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
                &[json!(slug.clone())],
            ) {
                v2_blueprint_active = true;
                v2_blueprint_id = m.get("id").cloned().unwrap_or(Value::Null);
                active_version = m.get("version").cloned();
            }
            v2_design_versions = first_row(db, "SELECT COUNT(*) AS n FROM blueprints WHERE contract_slug = ?", &[json!(slug.clone())])
                .and_then(|r| r.get("n").and_then(Value::as_i64))
                .unwrap_or(0);
            v2_design_court_rejections = first_row(
                db,
                "SELECT COUNT(*) AS n FROM judgments.judgment_cases WHERE card_slug = ? AND scope_kind = 'blueprint' AND scope_ref LIKE 'design-v%' AND verdict = 'fail' AND goal_line NOT LIKE '(%'",
                &[json!(card_slug)],
            )
            .and_then(|r| r.get("n").and_then(Value::as_i64))
            .unwrap_or(0);
            if v2_blueprint_active {
                if let Some(j) = first_row(
                    db,
                    "SELECT verdict FROM judgments.judgment_cases WHERE card_slug = ? AND scope_kind = 'blueprint' AND scope_ref = ? LIMIT 1",
                    &[json!(card_slug), json!(format!("design-v{}", tmpl(active_version.as_ref())))],
                ) {
                    v2_design_judge_fresh = true;
                    v2_design_judge_passed = j.get("verdict").and_then(Value::as_str) == Some("pass");
                }
                v2_snippets_authored = first_row(
                    db,
                    "SELECT COUNT(*) AS n FROM blueprint_snippets WHERE blueprint_id = ?",
                    &[v2_blueprint_id.clone()],
                )
                .and_then(|r| r.get("n").and_then(Value::as_i64))
                .unwrap_or(0)
                    > 0;
            }
            v2_author_versions = first_row(
                db,
                "SELECT COUNT(*) AS n FROM brain.driver_ticks WHERE card_slug = ? AND decided_action = 'snippet.author' AND outcome IN ('fired','failed')",
                &[json!(card_slug)],
            )
            .and_then(|r| r.get("n").and_then(Value::as_i64))
            .unwrap_or(0);
            if v2_snippets_authored {
                if let Some(j) = first_row(
                    db,
                    "SELECT verdict, updated_at AS ts, snippets_hash FROM judgments.judgment_cases WHERE card_slug = ? AND scope_kind = 'blueprint' AND scope_ref = ? LIMIT 1",
                    &[json!(card_slug), json!(format!("snippets-v{}", tmpl(active_version.as_ref())))],
                ) {
                    if let Some(h) = j.get("snippets_hash").filter(|v| !v.is_null()) {
                        let current = crate::blueprint__helpers_snippets_hash::snippetsContentHash(db, &v2_blueprint_id).unwrap_or(Value::Null);
                        v2_snippet_judge_fresh = h == &current;
                    } else {
                        let last_author_tick = first_row(
                            db,
                            "SELECT MAX(started_at) AS ts FROM brain.driver_ticks WHERE card_slug = ? AND decided_action = 'snippet.author' AND outcome IN ('fired','failed')",
                            &[json!(card_slug)],
                        )
                        .and_then(|row| row.get("ts").cloned())
                        .filter(|v| !v.is_null());
                        let j_ts = j.get("ts").and_then(Value::as_str);
                        v2_snippet_judge_fresh = match (&last_author_tick, j_ts) {
                            (None, _) => true,
                            (Some(_), None) => false,
                            (Some(t), Some(jt)) => jt > t.as_str().unwrap_or(""),
                        };
                    }
                    v2_snippet_judge_passed = j.get("verdict").and_then(Value::as_str) == Some("pass");
                }
            }
            if v2_snippet_judge_fresh && !v2_snippet_judge_passed && !v2_blueprint_id.is_null() {
                if let Some(bp) = first_row(db, "SELECT blueprint_json FROM blueprints WHERE id = ?", &[v2_blueprint_id.clone()]) {
                    let bp_text = str_qq(bp.get("blueprint_json"));
                    let items = all_rows(
                        db,
                        "SELECT ji.statement, ji.judge_why FROM judgments.judgment_items ji JOIN judgments.judgment_cases jc ON jc.judgment_id = ji.judgment_id WHERE jc.card_slug = ? AND jc.scope_kind = 'blueprint' AND jc.scope_ref = ? AND ji.side = 'prosecutor' AND ji.judge_value = 1",
                        &[json!(card_slug), json!(format!("snippets-v{}", tmpl(active_version.as_ref())))],
                    );
                    'outer: for it in &items {
                        let text = format!("{} {}", str_qq(it.get("statement")), str_qq(it.get("judge_why")));
                        for m in extract_quoted_literals(&text) {
                            if bp_text.contains(m.as_str()) {
                                v2_snippet_defect_in_blueprint = true;
                                break 'outer;
                            }
                        }
                    }
                }
            }
            v2_build_sealed = first_row(db, "SELECT 1 AS x FROM man_reports WHERE contract_slug = ? AND phase = 'qa_passed' LIMIT 1", &[json!(slug.clone())]).is_some();

            if v2_blueprint_active {
                if let Some(bg) = &last_build_gate {
                    if let Some(h) = bg.get("snippets_hash").filter(|v| !v.is_null()) {
                        if let Ok(current) = crate::blueprint__helpers_snippets_hash::snippetsContentHash(db, &v2_blueprint_id) {
                            build_gate_fresh = h == &current;
                        }
                    }
                }
            }

            v2_baseline_test_done =
                first_row(db, "SELECT 1 AS x FROM test_gate_runs WHERE contract_slug = ? AND phase = 'baseline' LIMIT 1", &[json!(slug.clone())]).is_some();
            last_test_gate = first_row(
                db,
                "SELECT id, ts, passed, skipped, test_cmd, snippets_hash FROM test_gate_runs WHERE contract_slug = ? AND COALESCE(phase, 'after') = 'after' ORDER BY id DESC LIMIT 1",
                &[json!(slug.clone())],
            );
            if let Some(tg) = &last_test_gate {
                test_gate_fresh = match tg.get("snippets_hash").filter(|v| !v.is_null()) {
                    Some(h) if !v2_blueprint_id.is_null() => {
                        let current = crate::blueprint__helpers_snippets_hash::snippetsContentHash(db, &v2_blueprint_id).unwrap_or(Value::Null);
                        h == &current
                    }
                    _ => true,
                };
            }
            test_gate_reds = count_c(
                db,
                "SELECT COUNT(*) AS c FROM test_gate_runs WHERE contract_slug = ? AND passed = 0 AND COALESCE(skipped,0) = 0 AND COALESCE(phase, 'after') = 'after'",
                &[json!(slug)],
            );
        }
    }

    // ── bug-finder advisory stage ────────────────────────────────────────────────────────────
    let mut bugfinder_attempted = true;
    if contract.is_some() {
        if let Some(tg) = &last_test_gate {
            if tg.get("passed").and_then(Value::as_i64) == Some(1) && test_gate_fresh {
                let ts = tg.get("ts").and_then(Value::as_str).unwrap_or("");
                bugfinder_attempted = first_row(
                    db,
                    "SELECT 1 AS x FROM brain.driver_ticks WHERE card_slug = ? AND decided_action = 'system.bugfinder.run' AND started_at >= ? LIMIT 1",
                    &[json!(card_slug), json!(ts)],
                )
                .is_some();
            }
        }
    }

    let atlas_populated = first_row(db, "SELECT count(*) AS n FROM code.code_nodes WHERE origin = 'atlas'", &[])
        .and_then(|r| r.get("n").and_then(Value::as_i64))
        .unwrap_or(0)
        > 0;

    let mut snippet_approved = false;
    if snippet_gate_enabled {
        if let Some(c) = &contract {
            if all_files_authored || swarm_authored {
                let slug = c.get("slug").and_then(Value::as_str).unwrap_or("");
                let authored = crate::system_driver_snippet_gate::authoredSig(db, c);
                snippet_approved = crate::system_driver_snippet_gate::approvedSig(slug) == Some(authored);
            }
        }
    }

    let card_auto_merge = truthy(card.get("auto_merge"));
    let mut driver_pr_open = false;
    if let Some(c) = &contract {
        let slug = c.get("slug").and_then(Value::as_str).unwrap_or("").to_string();
        driver_pr_open = first_row(
            db,
            "SELECT 1 AS x FROM cockpit_pull_requests WHERE source = 'driver' AND ref_slug = ? AND status = 'open' LIMIT 1",
            &[json!(slug)],
        )
        .is_some();
    }

    let v2_author_ticks = count_c(
        db,
        "SELECT COUNT(*) AS c FROM brain.driver_ticks WHERE card_slug = ? AND decided_action = 'snippet.author' AND outcome IN ('fired', 'failed')",
        &[json!(card_slug)],
    );
    let v2_ratify_rejects = if truthy(card.get("slug")) {
        count_c(
            db,
            "SELECT COUNT(*) AS c FROM contracts WHERE card_slug = ? AND failure_kind = 'pl_ratify_rejected'",
            &[json!(card_slug)],
        )
    } else {
        0
    };

    let mut last_pending_contract = true;
    if let Some(c) = &contract {
        if truthy(c.get("card_slug")) {
            let contract_card_slug = c.get("card_slug").cloned().unwrap();
            let contract_slug = c.get("slug").cloned().unwrap_or(Value::Null);
            last_pending_contract = first_row(
                db,
                "SELECT 1 AS x FROM contracts WHERE card_slug = ? AND slug != ? AND superseded_by IS NULL AND status != 'done' LIMIT 1",
                &[contract_card_slug, contract_slug],
            )
            .is_none();
        }
    }

    // ── assemble facts (JS insertion order) ─────────────────────────────────────────────────
    let mut facts = Map::new();
    facts.insert("card".into(), card.clone());
    facts.insert("contract".into(), contract.clone().unwrap_or(Value::Null));
    facts.insert("plSession".into(), pl_session.clone().unwrap_or(Value::Null));
    facts.insert("lastQaRun".into(), last_qa_run.clone().unwrap_or(Value::Null));
    facts.insert("devAttempts".into(), json!(dev_attempts));
    facts.insert("reauthorAttempts".into(), json!(reauthor_attempts));
    facts.insert("lastBuildGate".into(), last_build_gate.clone().unwrap_or(Value::Null));
    facts.insert("buildGateFresh".into(), json!(build_gate_fresh));
    facts.insert("buildGateReauthors".into(), json!(build_gate_reauthors));
    facts.insert("v2BaselineBuildDone".into(), json!(v2_baseline_build_done));
    facts.insert("lastTestGate".into(), last_test_gate.clone().unwrap_or(Value::Null));
    facts.insert("testGateFresh".into(), json!(test_gate_fresh));
    facts.insert("testGateReds".into(), json!(test_gate_reds));
    facts.insert("v2BaselineTestDone".into(), json!(v2_baseline_test_done));
    facts.insert("bugfinderAttempted".into(), json!(bugfinder_attempted));
    facts.insert("lastContractQaReview".into(), last_contract_qa_review.clone().unwrap_or(Value::Null));
    facts.insert("contractQaReviewFresh".into(), json!(contract_qa_review_fresh));
    facts.insert("contractQaReauthors".into(), json!(contract_qa_reauthors));
    facts.insert("hasPlanDefs".into(), json!(has_plan_defs));
    facts.insert("allFilesAuthored".into(), json!(all_files_authored));
    facts.insert("lastPlanReview".into(), last_plan_review.clone().unwrap_or(Value::Null));
    facts.insert("planReviewFresh".into(), json!(plan_review_fresh));
    facts.insert("lastPlanCoverage".into(), last_plan_coverage.clone().unwrap_or(Value::Null));
    facts.insert("planCoverageFresh".into(), json!(plan_coverage_fresh));
    facts.insert("postQa".into(), Value::Object(post_qa));
    facts.insert("cardChildren".into(), json!(card_children));
    facts.insert("cardRejectionCount".into(), json!(card_rejection_count));
    facts.insert("autoRedraftsSinceManual".into(), json!(auto_redrafts_since_manual));
    facts.insert("manBlockedRedrafts".into(), json!(man_blocked_redrafts));
    facts.insert("recentConsecutiveFailures".into(), json!(recent_consecutive_failures));
    facts.insert("recentFailureSignature".into(), recent_failure_signature.map(|s| json!(s)).unwrap_or(Value::Null));
    facts.insert("swarmEnabled".into(), json!(swarm_enabled));
    facts.insert("swarmManifestDraft".into(), json!(swarm_manifest_draft));
    facts.insert("swarmManifestVersions".into(), json!(swarm_manifest_versions));
    facts.insert("lastManifestLint".into(), last_manifest_lint.clone().unwrap_or(Value::Null));
    facts.insert("manifestLintFresh".into(), json!(manifest_lint_fresh));
    facts.insert("lastComposition".into(), last_composition.clone().unwrap_or(Value::Null));
    facts.insert("compositionFresh".into(), json!(composition_fresh));
    facts.insert("swarmAuthored".into(), json!(swarm_authored));
    facts.insert("swarmAuthorVersions".into(), json!(swarm_author_versions));
    facts.insert("lastReviewSwarm".into(), last_review_swarm.clone().unwrap_or(Value::Null));
    facts.insert("swarmReviewed".into(), json!(swarm_reviewed));
    facts.insert("lastSwarmCoverage".into(), last_swarm_coverage.clone().unwrap_or(Value::Null));
    facts.insert("swarmCoverageFresh".into(), json!(swarm_coverage_fresh));
    facts.insert("swarmCoverageGreen".into(), json!(swarm_coverage_green));
    facts.insert("worktreeEnabled".into(), json!(worktree_enabled));
    facts.insert("autoApproveEnabled".into(), json!(auto_approve_enabled));
    facts.insert("snippetGateEnabled".into(), json!(snippet_gate_enabled));
    facts.insert("snippetApproved".into(), json!(snippet_approved));
    facts.insert("pipelineV2Enabled".into(), json!(pipeline_v2_enabled));
    facts.insert("v2ScreeningFresh".into(), json!(v2_screening_fresh));
    facts.insert("v2BlueprintActive".into(), json!(v2_blueprint_active));
    facts.insert("v2BlueprintId".into(), v2_blueprint_id.clone());
    facts.insert("v2DesignVersions".into(), json!(v2_design_versions));
    facts.insert("v2DesignCourtRejections".into(), json!(v2_design_court_rejections));
    facts.insert("v2DesignJudgeFresh".into(), json!(v2_design_judge_fresh));
    facts.insert("v2DesignJudgePassed".into(), json!(v2_design_judge_passed));
    facts.insert("v2SnippetsAuthored".into(), json!(v2_snippets_authored));
    facts.insert("v2AuthorVersions".into(), json!(v2_author_versions));
    facts.insert("v2SnippetJudgeFresh".into(), json!(v2_snippet_judge_fresh));
    facts.insert("v2SnippetJudgePassed".into(), json!(v2_snippet_judge_passed));
    facts.insert("v2SnippetDefectInBlueprint".into(), json!(v2_snippet_defect_in_blueprint));
    facts.insert("v2BuildSealed".into(), json!(v2_build_sealed));
    facts.insert("atlasPopulated".into(), json!(atlas_populated));
    facts.insert("cardAutoMerge".into(), json!(card_auto_merge));
    facts.insert("driverPrOpen".into(), json!(driver_pr_open));
    facts.insert("lastPendingContract".into(), json!(last_pending_contract));
    facts.insert("v2RatifyRejects".into(), json!(v2_ratify_rejects));
    facts.insert("v2AuthorTicks".into(), json!(v2_author_ticks));

    let facts_value = Value::Object(facts);
    let decision = crate::driver_state_machine_rules::evaluateRules(&facts_value)?;

    let mut log_inputs = Map::new();
    log_inputs.insert("card_status".into(), card.get("status").cloned().unwrap_or(Value::Null));
    log_inputs.insert(
        "contract_status".into(),
        contract.as_ref().and_then(|c| c.get("status").cloned()).unwrap_or(Value::Null),
    );
    log_inputs.insert(
        "pl_session_status".into(),
        pl_session.as_ref().and_then(|p| p.get("status").cloned()).unwrap_or(Value::Null),
    );
    log_inputs.insert(
        "last_qa_verdict".into(),
        last_qa_run.as_ref().and_then(|r| r.get("verdict").cloned()).unwrap_or(Value::Null),
    );
    log_inputs.insert("dev_attempts_since_qa".into(), json!(dev_attempts));
    log_inputs.insert("has_plan_defs".into(), json!(has_plan_defs));
    log_inputs.insert("all_files_authored".into(), json!(all_files_authored));
    log_inputs.insert("plan_review_fresh".into(), json!(plan_review_fresh));
    log_inputs.insert("plan_coverage_fresh".into(), json!(plan_coverage_fresh));
    log_inputs.insert("matched_rule".into(), decision.get("matched_rule").cloned().unwrap_or(Value::Null));
    log_inputs.insert("next_agent".into(), decision.get("next_agent").cloned().unwrap_or(Value::Null));
    log_inputs.insert("next_action".into(), decision.get("next_action").cloned().unwrap_or(Value::Null));
    log_inputs.insert("terminal".into(), json!(truthy(decision.get("terminal"))));

    let matched_rule_str = decision
        .get("matched_rule")
        .and_then(|v| v.as_str())
        .unwrap_or("no_match")
        .to_string();
    let reason_str = tmpl(decision.get("reason"));

    let mut log_input = Map::new();
    log_input.insert("agent".into(), json!("state-machine"));
    log_input.insert("decision".into(), json!("rule_match"));
    log_input.insert("card_slug".into(), json!(card_slug));
    log_input.insert("rationale".into(), json!(format!("{matched_rule_str}: {reason_str}")));
    log_input.insert("inputs".into(), Value::Object(log_inputs));
    let _ = crate::engine_log_decision::logDecision(&Value::Object(log_input), db);

    let mut out = match decision {
        Value::Object(o) => o,
        _ => Map::new(),
    };
    out.insert("facts".into(), facts_value);
    Ok(Value::Object(out))
}

// = JS `[...text.matchAll(/['"`]([^'"`]{4,80})['"`]/g)].map((m) => m[1])` — quoted literal
// extraction (4-80 chars) with no lookahead needed (see playbook gotcha 7).
fn extract_quoted_literals(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' || c == '"' || c == '`' {
            let quote = c;
            let mut j = i + 1;
            while j < chars.len() && chars[j] != quote {
                j += 1;
            }
            if j < chars.len() {
                let len = j - (i + 1);
                if (4..=80).contains(&len) {
                    out.push(chars[i + 1..j].iter().collect());
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS code;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, skip_pl_flag INTEGER, priority TEXT, body_json TEXT, driver_intent TEXT, auto_merge INTEGER, parent_slug TEXT);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, slug TEXT, status TEXT, tier TEXT, size TEXT, microtasks_json TEXT, current_attempt INTEGER, sandbox_path TEXT, worktree_path TEXT, dev_microtask_progress INTEGER, pl_review_count INTEGER, rejection_reason_md TEXT, failure_kind TEXT, superseded_by TEXT, created_at TEXT, updated_at TEXT, last_qa_consumed_run_id INTEGER, amendments_applied INTEGER);
             CREATE TABLE man_reports (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, phase TEXT, verdict TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, ts TEXT, all_passed INTEGER, scope_passed INTEGER, acceptance_pass INTEGER, acceptance_fail INTEGER, escalation_reason TEXT, suggested_microtasks_json TEXT, verdict TEXT);
             CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, status TEXT, approved_card_slug TEXT);
             CREATE TABLE pl_questions (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER, step TEXT);
             CREATE TABLE pl_answers (id INTEGER PRIMARY KEY AUTOINCREMENT, question_id INTEGER);
             CREATE TABLE pl_screenings (card_slug TEXT);
             CREATE TABLE build_gate_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, ts TEXT, passed INTEGER, skipped INTEGER, build_cmd TEXT, evidence_md TEXT, applied_at TEXT, phase TEXT, snippets_hash TEXT);
             CREATE TABLE test_gate_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, ts TEXT, passed INTEGER, skipped INTEGER, test_cmd TEXT, phase TEXT, snippets_hash TEXT);
             CREATE TABLE contract_qa_reviews (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, ts TEXT, passed INTEGER, verdict TEXT, findings_md TEXT, issues_json TEXT, reviewer TEXT, failure_target TEXT, review_mode TEXT);
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, phase TEXT);
             CREATE TABLE card_review_blueprints (card_slug TEXT, contract_slug TEXT, attempt INTEGER);
             CREATE TABLE swarm_manifests (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, superseded_by TEXT);
             CREATE TABLE swarm_blocks (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, block_id TEXT, file TEXT, content TEXT);
             CREATE TABLE blueprints (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT, version INTEGER, superseded_by TEXT, blueprint_json TEXT);
             CREATE TABLE blueprint_snippets (id INTEGER PRIMARY KEY AUTOINCREMENT, blueprint_id INTEGER, block_id TEXT, code TEXT);
             CREATE TABLE cockpit_pull_requests (id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT, ref_slug TEXT, status TEXT);
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, created_at TEXT);
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, decided_action TEXT, outcome TEXT, started_at TEXT, error_message TEXT);
             CREATE TABLE brain.brain_decisions (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, decided_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), card_slug TEXT, decision TEXT NOT NULL, rationale TEXT NOT NULL, inputs_json TEXT, fire_id INTEGER, CHECK (decision IN ('fire','wait','rollback','escalate','done','pause','session_start','session_end','goal_set','classify','gate_verdict','rule_match','sandbox_create','review_pass','schedule_and_dispatch','agent_step')));
             CREATE TABLE judgments.judgment_cases (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, scope_kind TEXT, scope_ref TEXT, verdict TEXT, goal_line TEXT, updated_at TEXT, judgment_id INTEGER);
             CREATE TABLE judgments.judgment_items (id INTEGER PRIMARY KEY AUTOINCREMENT, judgment_id INTEGER, statement TEXT, judge_why TEXT, side TEXT, judge_value INTEGER);
             CREATE TABLE code.code_nodes (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn resolve_worktree_enabled_honors_on_off_auto() {
        assert_eq!(resolveWorktreeEnabled(Some(&json!("on")), false), true);
        assert_eq!(resolveWorktreeEnabled(Some(&json!("OFF")), true), false);
        assert_eq!(resolveWorktreeEnabled(None, true), true);
        assert_eq!(resolveWorktreeEnabled(None, false), false);
    }

    #[test]
    fn card_not_found_returns_terminal_reason() {
        let c = db();
        let r = driverStateMachine(&json!({ "card_slug": "GHOST" }), &c).unwrap();
        assert_eq!(r["reason"], json!("card 'GHOST' not found"));
        assert_eq!(r["terminal"], json!(true));
        assert_eq!(r["matched_rule"], Value::Null);
        assert_eq!(r["facts"]["card"], Value::Null);
    }

    #[test]
    fn missing_card_slug_input_errors() {
        let c = db();
        let err = driverStateMachine(&json!({}), &c).unwrap_err();
        assert!(err.contains("card_slug is required"), "{err}");
    }

    #[test]
    fn card_with_no_contract_reaches_evaluate_rules_with_null_contract() {
        let c = db();
        c.execute(
            "INSERT INTO cards (slug, status, skip_pl_flag, priority, driver_intent, auto_merge) VALUES ('CARD-1', 'proposed', 0, 'P2', 'auto', 0)",
            [],
        )
        .unwrap();
        let r = driverStateMachine(&json!({ "card_slug": "CARD-1" }), &c).unwrap();
        assert_eq!(r["facts"]["card"]["slug"], json!("CARD-1"));
        assert_eq!(r["facts"]["contract"], Value::Null);
        assert!(r.get("matched_rule").is_some());
    }

    #[test]
    fn contract_microtasks_totals_and_rejection_counts_are_computed() {
        let c = db();
        c.execute(
            "INSERT INTO cards (slug, status, skip_pl_flag, priority, driver_intent, auto_merge) VALUES ('CARD-2', 'in_progress', 0, 'P2', 'auto', 0)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO contracts (card_slug, slug, status, microtasks_json, dev_microtask_progress, created_at, updated_at) VALUES \
             ('CARD-2', 'CT-2', 'pl_ratified', '[{\"patches\":[{\"op\":\"insert\"}]}]', 0, '2026-01-01 00:00:00', '2026-01-01 00:00:00')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO contracts (card_slug, slug, status, failure_kind, superseded_by) VALUES ('CARD-2', 'CT-2-OLD', 'rejected', 'qa_fail', 'CT-2')",
            [],
        )
        .unwrap();
        let r = driverStateMachine(&json!({ "card_slug": "CARD-2" }), &c).unwrap();
        assert_eq!(r["facts"]["contract"]["microtasks_total"], json!(1));
        assert_eq!(r["facts"]["contract"]["microtasks_remaining"], json!(1));
        assert_eq!(r["facts"]["cardRejectionCount"], json!(1));
        assert_eq!(r["facts"]["allFilesAuthored"], json!(true));
        assert_eq!(r["facts"]["hasPlanDefs"], json!(true));
    }

    #[test]
    fn env_toggles_flow_through_to_facts_off_values() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_SWARM", "off");
            std::env::set_var("ANTILOKI_PIPELINE_V2", "off");
            std::env::set_var("ANTILOKI_AUTO_APPROVE", "off");
        }
        let c = db();
        c.execute(
            "INSERT INTO cards (slug, status, skip_pl_flag, priority, driver_intent, auto_merge) VALUES ('CARD-3', 'proposed', 0, 'P2', 'auto', 0)",
            [],
        )
        .unwrap();
        let r = driverStateMachine(&json!({ "card_slug": "CARD-3" }), &c).unwrap();
        unsafe {
            std::env::remove_var("ANTILOKI_SWARM");
            std::env::remove_var("ANTILOKI_PIPELINE_V2");
            std::env::remove_var("ANTILOKI_AUTO_APPROVE");
        }
        assert_eq!(r["facts"]["swarmEnabled"], json!(false));
        assert_eq!(r["facts"]["pipelineV2Enabled"], json!(false));
        assert_eq!(r["facts"]["autoApproveEnabled"], json!(false));
    }

    #[test]
    fn extract_quoted_literals_matches_js_regex_length_bounds() {
        let text = "bad key 'ab' and 'longenoughliteral' and \"1234\"";
        let out = extract_quoted_literals(text);
        assert_eq!(out, vec!["longenoughliteral".to_string(), "1234".to_string()]);
    }
}
