//! qa-gate-contract-verify.rs — twin of skills/agents/qa/qa-gate-contract-verify.mjs (db WRITE
//! lane, composer). NAMES ARE CONTRACT: `qaGateContractVerify` (default). The canonical QA gate:
//! composes `qaScopeCheck` (diff stays inside files_touched), `qaAcceptanceRun` (every
//! acceptance_json item verified — also persists `qa_runs` + `qa_run_results`), then a
//! best-effort `qaSeniorReview` advisory pass, and rolls all three into one pass/fail verdict.
//!
//! On PASS: promotes the sandbox (`qaSandboxPromote`) or defers to the PR gate when a
//! `worktree_path` is live, then flips `contracts.status` to `'done'`.
//! On FAIL: fires `learnInsight` (moment_kind='rejection') and best-effort `qaFeedbackGenerate`
//! for DEV's remediation loop; `contracts.status` is left untouched (stays at `'impl'`/`'qa'` —
//! the driver re-fires `dev.implement.code`).
//!
//! PARITY-DEFERRED (transitively, single call site): `qaSeniorReview` is itself bespoke-native
//! and PARITY-DEFERRED at its own `aiCall` boundary (see qa-senior-review.rs's module doc). This
//! composer never awaits it "raw" — the JS wraps the call in try/catch and so does this twin: ANY
//! `Err` from `qaSeniorReview` (whether the deferred aiCall boundary, a missing prompt row, or a
//! genuine infra fault) is caught and folded into `{error, verdict:"pass", skipped:true}`,
//! matching the JS `catch (e) { seniorReview = { error: ..., verdict: 'pass', skipped: true } }`
//! byte-for-byte. This is why the deterministic verdict can never hard-block on the senior pass —
//! FEAT-QA-SENIOR-REVIEW-SECOND-PASS's advisory posture (2026-07-09) is preserved exactly.
//!
//! `context.db` missing → JS `throw err.contextMissing('qa-gate-contract-verify', 'db')`. Not
//! modeled: `db: &Connection` is a required, non-nullable reference here, same convention as
//! every sibling db-lane twin. `contract_slug` missing/falsy → JS `err.required(...)` → ported as
//! `Err`. `verifier = context?.actor ?? 'agent:qa'` — same established db-lane convention as
//! `qa-acceptance-run.rs`: this lane only ever threads `input` + `db` (no context object carrying
//! `actor`), so `context.actor` is never reachable; the sole reachable default is the
//! input-first precedence over the literal `"agent:qa"`.
//!
//! Contract-not-found: JS throws an `AntilokiError` with `code: NOT_FOUND` — ported as a plain
//! `Err(String)`, same simplification every sibling db-lane twin makes (the error TAXONOMY is not
//! reified as a Rust type; only the message content is preserved for parity).
//!
//! `qaScopeCheck` / `qaAcceptanceRun` calls are NOT wrapped in try/catch in the JS — a thrown
//! error there propagates straight to the caller. Ported with `?`. Same for the pass-path
//! `qaSandboxPromote` call (also unguarded in JS — a TAMPERED sandbox surfaces, not swallowed).
//!
//! The `verdict === 'fail' && acceptance.all_passed` branch (r35 + 2026-07-09 fix): acceptance can
//! pass while the gate fails on SCOPE (offenders or a git error) or the SENIOR REVIEW (blocker
//! issues). Ported verbatim: flips the just-persisted `qa_runs.all_passed` to 0 (best-effort,
//! swallowing any error exactly like the JS `try {} catch {}` around a possibly-absent row/column)
//! and appends a `feedback_md` note attributing the REAL reason. `scope.offenders` is guarded as
//! `Vec::new()` when absent (mirrors the JS `Array.isArray(scope.offenders) ? scope.offenders :
//! []` — the git-diff-failed early return in qa-scope-check.rs never sets `offenders`, which was
//! itself a latent crash the JS fix closed).
//!
//! `logDecision` is fire-and-forget in the JS (no `await`, no try/catch — the twin itself never
//! throws, only ever returns `null` on internal failure). Ported the same way: called for its
//! side effect, result discarded via `?` (the twin's own contract guarantees `Ok` always).
// QUALITY: composer — every branch traceable to the JS source; the two deliberate simplifications
// (AntilokiError → String, context.actor unreachable) match every sibling db-lane twin's convention.

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
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
fn as_str_vec(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}
// JS `String(v)` coercion — used for `String(scope.evidence.error)`.
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

#[allow(non_snake_case)]
pub fn qaGateContractVerify(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `const { db } = context ?? {}; if (!db) throw err.contextMissing(...)` — unreachable
    // (db: &Connection is a required, non-nullable reference), same convention as every sibling
    // db-lane twin.
    let contract_slug = input.get("contract_slug").and_then(Value::as_str).unwrap_or("").to_string();
    if contract_slug.is_empty() {
        return Err("qa-gate-contract-verify: contract_slug is required".to_string());
    }
    let verifier = input.get("verifier").and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "agent:qa".to_string()); // context.actor unreachable in this db-only lane

    let rows = crate::db_json::query_rows(
        db,
        "SELECT status, sandbox_path, worktree_path FROM contracts WHERE slug = ?",
        &[json!(contract_slug)],
    )?;
    let c = rows.as_array().and_then(|a| a.first()).cloned();
    let c = match c {
        Some(row) => row,
        None => return Err(format!("qa-gate-contract-verify: contract '{contract_slug}' not found")),
    };
    let worktree_path = c.get("worktree_path").cloned().unwrap_or(Value::Null);
    let sandbox_path = c.get("sandbox_path").cloned().unwrap_or(Value::Null);

    let scope = crate::agents_qa_qa_scope_check::qaScopeCheck(&json!({ "contract_slug": contract_slug }), db)?;
    // qa-acceptance-run persists the qa_runs + qa_run_results rows that both the driver retry
    // counter and qa-sandbox-promote rely on.
    let acceptance = crate::agents_qa_qa_acceptance_run::qaAcceptanceRun(
        &json!({ "contract_slug": contract_slug, "verifier": verifier }),
        db,
    )?;
    let scope_passed = scope.get("passed").and_then(Value::as_bool).unwrap_or(false);
    let acceptance_all_passed = acceptance.get("all_passed").and_then(Value::as_bool).unwrap_or(false);
    let acceptance_results: Vec<Value> = acceptance.get("results").and_then(Value::as_array).cloned().unwrap_or_default();

    // FEAT-QA-SENIOR-REVIEW-SECOND-PASS: after deterministic checks pass, a senior reviewer reads
    // the diff + briefing + asks "does this make sense?". Best-effort: ANY failure (deferred
    // aiCall boundary included) must NOT silently pass as a clean senior review — fold it into a
    // skipped/pass marker so promote is auditable as UN-reviewed.
    let senior_review: Option<Value> = if scope_passed && acceptance_all_passed {
        match crate::agents_qa_qa_senior_review::qaSeniorReview(&json!({ "contract_slug": contract_slug }), db) {
            Ok(r) => Some(r),
            Err(e) => Some(json!({ "error": take_chars(&e, 200), "verdict": "pass", "skipped": true })),
        }
    } else {
        None
    };

    // SENIOR REVIEW IS ADVISORY (2026-07-09): a single subjective "does this make sense?" pass
    // must NOT override the adversarial deterministic checks. Its concerns ride the qa_run as
    // ADVISORY notes; they never fail the gate.
    let senior_advisory = senior_review.as_ref().map_or(false, |sr| {
        sr.get("verdict").and_then(Value::as_str) == Some("fail") && !truthy(sr.get("skipped"))
    });
    let verdict = if scope_passed && acceptance_all_passed { "pass" } else { "fail" };

    // r35 + 2026-07-09: acceptance can pass while the GATE fails on SCOPE (offenders or a git
    // error) or the SENIOR REVIEW (blocker issues). Flip all_passed AND persist the REAL reason.
    if verdict == "fail" && acceptance_all_passed {
        let _ = db.execute(
            "UPDATE qa_runs SET all_passed = 0 WHERE id = (SELECT MAX(id) FROM qa_runs WHERE contract_slug = ?)",
            [&contract_slug],
        ); // best-effort — row absent
        let scope_offenders = as_str_vec(scope.get("offenders"));
        let mut parts: Vec<String> = Vec::new();
        if !scope_passed {
            if !scope_offenders.is_empty() {
                parts.push(format!(
                    "SCOPE — out-of-scope changes: {}",
                    scope_offenders.iter().take(10).cloned().collect::<Vec<_>>().join(", ")
                ));
            } else {
                let reason = scope.get("evidence").and_then(|e| e.get("reason")).and_then(Value::as_str).unwrap_or("failed");
                let err_suffix = scope.get("evidence").and_then(|e| e.get("error"))
                    .map(|e| format!(": {}", take_chars(&js_string(e), 300)))
                    .unwrap_or_default();
                parts.push(format!("SCOPE — {reason}{err_suffix}"));
            }
        }
        if let Some(sr) = &senior_review {
            if sr.get("verdict").and_then(Value::as_str) == Some("fail") {
                let blockers: Vec<String> = sr.get("issues").and_then(Value::as_array).cloned().unwrap_or_default()
                    .iter()
                    .filter(|i| i.get("severity").and_then(Value::as_str) == Some("blocker"))
                    .map(|i| {
                        let text = i.get("title").or_else(|| i.get("detail")).or_else(|| i.get("issue")).or_else(|| i.get("summary"))
                            .and_then(Value::as_str).unwrap_or("");
                        format!("· {}", take_chars(text, 200))
                    })
                    .collect();
                let summary = sr.get("summary_md").and_then(Value::as_str).unwrap_or("blocker issue(s) found");
                let mut msg = format!("SENIOR REVIEW — {}", take_chars(summary, 300));
                if !blockers.is_empty() {
                    msg.push('\n');
                    msg.push_str(&blockers.join("\n"));
                }
                parts.push(msg);
            }
        }
        if parts.is_empty() {
            parts.push("QA failed but no scope/senior detail was captured — a gate bug; investigate.".to_string());
        }
        let _ = db.execute(
            "UPDATE qa_runs SET feedback_md = COALESCE(feedback_md, '') || ?1 WHERE id = (SELECT MAX(id) FROM qa_runs WHERE contract_slug = ?2)",
            rusqlite::params![format!("\n=== QA GATE FAILED ===\n{}", parts.join("\n")), contract_slug],
        ); // best-effort — feedback column absent on old DBs
    }
    // FIX-SENIOR-REVIEW-AUDIT: 'skipped' = the senior AI threw (infra) and we fell through to
    // pass; 'not-run' = the deterministic checks already failed.
    let senior_status = match &senior_review {
        Some(sr) if truthy(sr.get("skipped")) => "skipped".to_string(),
        Some(sr) => sr.get("verdict").and_then(Value::as_str).unwrap_or("").to_string(),
        None => "not-run".to_string(),
    };

    // FEAT-AGENT-REASONING-LOG: every gate verdict is queryable later.
    let scope_offenders_all = as_str_vec(scope.get("offenders"));
    let failing_ac: Vec<Value> = acceptance_results.iter().filter(|r| !truthy(r.get("passed")))
        .map(|r| r.get("index").cloned().unwrap_or(Value::Null)).collect();
    let rationale = if verdict == "pass" {
        if senior_status == "skipped" {
            let err = senior_review.as_ref().and_then(|sr| sr.get("error")).and_then(Value::as_str).unwrap_or("error");
            format!(
                "pass-no-senior: senior-review SKIPPED (infra: {err}) — promoted UN-reviewed; scope clean + acceptance {}/{} ok",
                acceptance_results.len(), acceptance_results.len()
            )
        } else {
            format!(
                "pass: scope clean ({} offenders) + acceptance {}/{} ok{}",
                scope_offenders_all.len(), acceptance_results.len(), acceptance_results.len(),
                if senior_advisory { " · senior review flagged concerns (ADVISORY — non-blocking)" } else { "" }
            )
        }
    } else {
        format!(
            "fail: scope offenders={}, senior={}, acceptance fails={}",
            scope_offenders_all.len(), senior_status,
            acceptance_results.iter().filter(|r| !truthy(r.get("passed"))).count()
        )
    };
    let _ = crate::engine_log_decision::logDecision(
        &json!({
            "agent": "qa",
            "decision": "gate_verdict",
            "card_slug": contract_slug,
            "rationale": rationale,
            "inputs": {
                "contract_slug": contract_slug,
                "verifier": verifier,
                "verdict": verdict,
                "senior_review_status": senior_status,
                "scope_offenders": scope_offenders_all.iter().take(10).cloned().collect::<Vec<_>>(),
                "failing_ac": failing_ac,
            },
        }),
        db,
    )?;

    if verdict == "pass" {
        // Record the senior review's ADVISORY concerns on the passing run — they ride the PR for
        // the operator's review; they never block (deterministic gates + adversarial court passed).
        if senior_advisory {
            let sr = senior_review.as_ref().unwrap();
            let blockers: Vec<String> = sr.get("issues").and_then(Value::as_array).cloned().unwrap_or_default()
                .iter()
                .filter(|i| i.get("severity").and_then(Value::as_str) == Some("blocker"))
                .map(|i| {
                    let text = i.get("title").or_else(|| i.get("detail")).or_else(|| i.get("issue")).or_else(|| i.get("summary"))
                        .and_then(Value::as_str).unwrap_or("");
                    format!("· {}", take_chars(text, 200))
                })
                .collect();
            let summary = sr.get("summary_md").and_then(Value::as_str).unwrap_or("");
            let mut msg = format!(
                "\n=== SENIOR REVIEW — ADVISORY (did NOT block; the code court passed) ===\n{}",
                take_chars(summary, 300)
            );
            if !blockers.is_empty() {
                msg.push('\n');
                msg.push_str(&blockers.join("\n"));
            }
            let _ = db.execute(
                "UPDATE qa_runs SET feedback_md = COALESCE(feedback_md, '') || ?1 WHERE id = (SELECT MAX(id) FROM qa_runs WHERE contract_slug = ?2)",
                rusqlite::params![msg, contract_slug],
            ); // best-effort — column absent
        }
        // FEAT-SANDBOX-COW step 8: on pass with a sandbox, promote it atomically before flipping
        // status. If promote fails (e.g. TAMPERED), surface the failure rather than
        // half-completing (unguarded in JS — no try/catch around this call).
        let promote_result = if truthy(Some(&worktree_path)) {
            // FEAT-DRIVER-PR-GATE: QA pass does NOT merge — the worktree stays alive, the state
            // machine ships it as a PR (git-manager.pr.open); the operator's merge click runs the
            // real merge.
            json!({ "deferred": "pr_gate", "note": "worktree kept alive — the PR gate owns integration" })
        } else if truthy(Some(&sandbox_path)) {
            crate::contract_qa_sandbox_promote::qaSandboxPromote(&json!({ "contract_slug": contract_slug }), db, None)?
        } else {
            Value::Null
        };
        db.execute(
            "UPDATE contracts SET status='done', updated_at=datetime('now') WHERE slug = ?",
            [&contract_slug],
        ).map_err(|e| e.to_string())?;
        return Ok(json!({
            "contract_slug": contract_slug,
            "verdict": verdict,
            "verifier": verifier,
            "scope": scope,
            "acceptance": acceptance,
            "promote": promote_result,
            "new_status": "done",
        }));
    }

    // Fail path → learn.insight for QA-level rejection.
    let failing_ac_str = acceptance_results.iter().filter(|r| !truthy(r.get("passed")))
        .map(|r| format!("AC#{} ({})", r.get("index").cloned().unwrap_or(Value::Null), r.get("kind").and_then(Value::as_str).unwrap_or("")))
        .collect::<Vec<_>>().join(", ");
    let offenders_head = scope_offenders_all.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
    let corrected_md = format!(
        "Re-implement to satisfy: {}{}",
        if !scope_offenders_all.is_empty() { "remove out-of-scope changes" } else { "" },
        if !failing_ac_str.is_empty() { " + fix failing AC items" } else { "" }
    );
    let insight = crate::ai_learn_insight::learnInsight(
        &json!({
            "moment_kind": "rejection",
            "emitter": "qa.gate.contract.verify",
            "source_ref": contract_slug,
            "situation_md": format!("QA gate for contract '{contract_slug}'"),
            "naive_md": format!("Scope offenders: {offenders_head}; Acceptance fails: {failing_ac_str}"),
            "corrected_md": corrected_md,
            "tags": ["qa-rejection", "contract"],
            "created_by": verifier,
        }),
        db,
    )?;

    // FEAT-QA-DEV-FEEDBACK-LOOP: AI-generate feedback_md + suggested microtasks for DEV's
    // remediation. Persisted onto qa_runs by the generator. Best-effort: a failure here must NOT
    // block the verdict.
    let feedback_result = match crate::agents_qa_qa_feedback_generate::qaFeedbackGenerate(
        &json!({ "qa_run_id": acceptance.get("qa_run_id").cloned().unwrap_or(Value::Null) }),
        db,
    ) {
        Ok(r) => r,
        Err(e) => json!({ "error": take_chars(&e, 200) }),
    };

    Ok(json!({
        "contract_slug": contract_slug,
        "verdict": verdict,
        "verifier": verifier,
        "scope": scope,
        "acceptance": acceptance,
        "new_status": "qa",
        "insight_path": insight.get("insight_path").cloned().unwrap_or(Value::Null),
        "memory_id": insight.get("memory_id").cloned().unwrap_or(Value::Null),
        "feedback": feedback_result,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // Real-git-repo helper (same convention as qa-scope-check.rs's own tests): scope-check shells
    // out to real `git status --porcelain` against `contract.worktree_path`/`sandbox_path` as the
    // process cwd — a fake nonexistent path makes the subprocess spawn fail, which scope-check
    // folds into `{passed:false, evidence:{reason:'git_diff_failed'}}` rather than propagating.
    // A minimal real git-init'd temp dir (no commits needed since base_sha stays unset — only
    // `git status --porcelain` runs) lets the composer reach `scope.passed:true` deterministically.
    fn run_git(args: &[&str], cwd: &std::path::Path) -> std::process::Output {
        crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap()
    }
    fn make_repo(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-qa-gate-contract-verify-rs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run_git(&["init", "-q"], &dir);
        run_git(&["config", "user.email", "t@t.t"], &dir);
        run_git(&["config", "user.name", "t"], &dir);
        run_git(&["config", "commit.gpgsign", "false"], &dir);
        dir
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
             ATTACH ':memory:' AS brain;
             CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE, card_slug TEXT, status TEXT, sandbox_path TEXT, worktree_path TEXT, base_sha TEXT, bootstrap_cmd TEXT, files_touched_json TEXT, original_shas_json TEXT, acceptance_json TEXT, generated_paths_json TEXT, updated_at TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, contract_slug TEXT NOT NULL, ts TEXT NOT NULL DEFAULT (datetime('now')), verifier TEXT NOT NULL, scope_passed INTEGER, acceptance_pass INTEGER, acceptance_fail INTEGER, all_passed INTEGER NOT NULL, notes_md TEXT, feedback_md TEXT, suggested_microtasks_json TEXT, escalation_reason TEXT);
             CREATE TABLE qa_run_results (id INTEGER PRIMARY KEY AUTOINCREMENT, qa_run_id INTEGER NOT NULL, ac_index INTEGER NOT NULL, kind TEXT NOT NULL, passed INTEGER NOT NULL, evidence_md TEXT);
             CREATE TABLE memory.catalog_embeddings (entity_type TEXT NOT NULL, entity_name TEXT NOT NULL, model TEXT NOT NULL, dims INTEGER NOT NULL, vector BLOB NOT NULL);
             CREATE TABLE memory.memories (
               id              INTEGER PRIMARY KEY AUTOINCREMENT,
               type            TEXT    NOT NULL,
               title           TEXT    NOT NULL,
               situation       TEXT    NOT NULL,
               guidance        TEXT    NOT NULL,
               naive_response  TEXT,
               rationale       TEXT,
               tags_json       TEXT    NOT NULL DEFAULT '[]',
               source_card     TEXT,
               source_event    INTEGER,
               source_session  TEXT,
               confidence      INTEGER NOT NULL DEFAULT 0,
               validated       INTEGER NOT NULL DEFAULT 0,
               superseded_by   INTEGER,
               agent_slug      TEXT,
               created_at      TEXT    NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
               created_by      TEXT    NOT NULL
             );
             CREATE TABLE memory.memory_links (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               from_memory_id INTEGER NOT NULL,
               to_memory_id INTEGER NOT NULL,
               kind TEXT NOT NULL,
               rationale TEXT,
               weight REAL NOT NULL DEFAULT 1.0,
               created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')),
               created_by TEXT NOT NULL
             );
             CREATE TABLE brain.brain_decisions ( id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, decided_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), card_slug TEXT, decision TEXT NOT NULL, rationale TEXT NOT NULL, inputs_json TEXT, fire_id INTEGER );",
        ).unwrap();
        c
    }

    fn seed_contract(c: &Connection, slug: &str, acceptance: &Value) {
        c.execute(
            "INSERT INTO contracts (slug, status, acceptance_json) VALUES (?1, 'impl', ?2)",
            rusqlite::params![slug, acceptance.to_string()],
        ).unwrap();
    }

    #[test]
    fn guards_missing_contract_slug() {
        let c = db();
        assert_eq!(
            qaGateContractVerify(&json!({}), &c).unwrap_err(),
            "qa-gate-contract-verify: contract_slug is required"
        );
    }

    #[test]
    fn errors_when_contract_not_found() {
        let c = db();
        let err = qaGateContractVerify(&json!({ "contract_slug": "GHOST" }), &c).unwrap_err();
        assert!(err.contains("GHOST"));
        assert!(err.contains("not found"));
    }

    #[test]
    fn pass_verdict_promotes_nothing_flips_status_done_and_skips_senior() {
        let c = db();
        let slug = "GATE-PASS-1";
        seed_contract(&c, slug, &json!([{ "kind": "sql", "query": "SELECT 1 AS x" }]));

        let r = qaGateContractVerify(&json!({ "contract_slug": slug, "verifier": "agent:qa-bot" }), &c).unwrap();
        assert_eq!(r["verdict"], json!("pass"));
        assert_eq!(r["new_status"], json!("done"));
        assert_eq!(r["verifier"], json!("agent:qa-bot"));
        assert_eq!(r["scope"]["passed"], json!(true));
        assert_eq!(r["acceptance"]["all_passed"], json!(true));
        // no sandbox_path/worktree_path on the seeded contract -> no promote action.
        assert_eq!(r["promote"], Value::Null);

        let status: String = c.query_row("SELECT status FROM contracts WHERE slug = ?1", [slug], |row| row.get(0)).unwrap();
        assert_eq!(status, "done");

        // The senior review's aiCall boundary is PARITY-DEFERRED (always errors here) — the
        // composer must have caught it into a non-blocking skipped marker, not propagated it.
        let decision_count: i64 = c.query_row(
            "SELECT COUNT(*) FROM brain.brain_decisions WHERE card_slug = ?1 AND decision = 'gate_verdict'",
            [slug], |row| row.get(0),
        ).unwrap();
        assert_eq!(decision_count, 1);
        let rationale: String = c.query_row(
            "SELECT rationale FROM brain.brain_decisions WHERE card_slug = ?1", [slug], |row| row.get(0),
        ).unwrap();
        assert!(rationale.starts_with("pass-no-senior: senior-review SKIPPED"), "got: {rationale}");
    }

    #[test]
    fn pass_verdict_promotes_sandbox_when_sandbox_path_set() {
        // `sandbox_path` doubles as scope-check's real git cwd (a real, empty, git-init'd temp
        // dir makes `git status --porcelain` succeed trivially -> scope.passed:true) AND as
        // qaSandboxPromote's sandbox root. A files_touched entry whose expected sha can never
        // match (the real repo root has no such file) makes qaSandboxPromote fail with TAMPERED
        // — read-only detection, no writes occur — pinning that the gate PROPAGATES that failure
        // rather than swallowing it (unguarded `qaSandboxPromote` call in the JS composer, no
        // try/catch).
        let repo = make_repo("sandbox");
        let c = db();
        let slug = "GATE-PASS-SANDBOX";
        c.execute(
            "INSERT INTO contracts (slug, status, sandbox_path, files_touched_json, original_shas_json, acceptance_json) \
             VALUES (?1, 'impl', ?2, ?3, ?4, ?5)",
            rusqlite::params![
                slug,
                repo.to_string_lossy().to_string(),
                json!(["antiloki-qa-gate-verify-tamper-probe-file-that-does-not-exist.txt"]).to_string(),
                json!({ "antiloki-qa-gate-verify-tamper-probe-file-that-does-not-exist.txt": "deadbeef" }).to_string(),
                json!([{ "kind": "sql", "query": "SELECT 1 AS x" }]).to_string(),
            ],
        ).unwrap();
        let err = qaGateContractVerify(&json!({ "contract_slug": slug }), &c).unwrap_err();
        assert!(err.contains("qa-sandbox-promote") && err.contains("TAMPERED"), "got: {err}");
        // Status must NOT have flipped to done — the promote failure aborted before the UPDATE.
        let status: String = c.query_row("SELECT status FROM contracts WHERE slug = ?1", [slug], |row| row.get(0)).unwrap();
        assert_eq!(status, "impl");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn pass_verdict_defers_to_pr_gate_when_worktree_path_set() {
        // A real, empty, git-init'd temp dir as worktree_path -> scope-check's git status runs
        // for real and passes trivially. worktree_path truthy takes the PR-gate defer branch,
        // which never invokes qaSandboxPromote (no real-repo file I/O risk either way).
        let repo = make_repo("worktree");
        let c = db();
        let slug = "GATE-PASS-WORKTREE";
        c.execute(
            "INSERT INTO contracts (slug, status, worktree_path, acceptance_json) VALUES (?1, 'impl', ?2, ?3)",
            rusqlite::params![slug, repo.to_string_lossy().to_string(), json!([{ "kind": "sql", "query": "SELECT 1 AS x" }]).to_string()],
        ).unwrap();
        let r = qaGateContractVerify(&json!({ "contract_slug": slug }), &c).unwrap();
        assert_eq!(r["verdict"], json!("pass"));
        assert_eq!(r["scope"]["passed"], json!(true));
        assert_eq!(r["promote"]["deferred"], json!("pr_gate"));
        let status: String = c.query_row("SELECT status FROM contracts WHERE slug = ?1", [slug], |row| row.get(0)).unwrap();
        assert_eq!(status, "done");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn fail_verdict_leaves_status_unchanged_and_records_insight() {
        let c = db();
        let slug = "GATE-FAIL-1";
        seed_contract(&c, slug, &json!([
            { "kind": "sql", "query": "SELECT 1 AS c", "expect_value_min": 99, "expect_value_path": "$.c" },
        ]));

        let r = qaGateContractVerify(&json!({ "contract_slug": slug, "verifier": "agent:qa" }), &c).unwrap();
        assert_eq!(r["verdict"], json!("fail"));
        assert_eq!(r["new_status"], json!("qa"));
        assert!(r["memory_id"].as_i64().unwrap() > 0);
        assert!(r["insight_path"].is_array());
        // qaFeedbackGenerate has no brain.prompt_templates table seeded here -> it errors and the
        // gate must fold that into a non-fatal {error} shape (best-effort, matches the JS catch).
        assert!(r["feedback"].get("error").is_some());

        let status: String = c.query_row("SELECT status FROM contracts WHERE slug = ?1", [slug], |row| row.get(0)).unwrap();
        assert_eq!(status, "impl"); // unchanged — driver re-fires dev.implement.code

        let mem_count: i64 = c.query_row("SELECT COUNT(*) FROM memory.memories WHERE type = 'correction'", [], |row| row.get(0)).unwrap();
        assert_eq!(mem_count, 1);

        let decision_rationale: String = c.query_row(
            "SELECT rationale FROM brain.brain_decisions WHERE card_slug = ?1", [slug], |row| row.get(0),
        ).unwrap();
        assert!(decision_rationale.starts_with("fail: scope offenders=0, senior=not-run, acceptance fails=1"), "got: {decision_rationale}");
    }

    #[test]
    fn verifier_falls_back_to_agent_qa_default_when_absent() {
        let c = db();
        let slug = "GATE-VER-DEFAULT";
        seed_contract(&c, slug, &json!([{ "kind": "sql", "query": "SELECT 1 AS x" }]));
        let r = qaGateContractVerify(&json!({ "contract_slug": slug }), &c).unwrap();
        assert_eq!(r["verifier"], json!("agent:qa"));
    }
}
