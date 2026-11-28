//! reconcile.rs — twin of skills/system/reconcile.mjs. NAMES ARE CONTRACT: `reconcile` (default,
//! standard lane) plus the JS re-export `detectContractAwaitingHumanApproval` (chat surfaces pull
//! just that one detector without running the full battery). The other 6 detectors
//! (`detectPlClarifyPending`, `detectPrioritizedButNotProcessed`, `detectOutOfBandCardMutation`,
//! `detectStuckTickLoop`, `detectOrphanSandboxes`, `detectWorkingTreeOutsideContracts`) are private
//! helpers in the JS (no `export`) — mirrored here as private `fn`s, not `pub`.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: `detectOrphanSandboxes` walks a REAL directory
//! (`existsSync`/`readdirSync` over `SANDBOX_DIR`) and `detectWorkingTreeOutsideContracts` shells
//! out to a REAL `git status --porcelain` subprocess against `REPO_ROOT` — same non-fixture-
//! seedable class as reconcile-external-code-drift.rs / terminal-sandbox.rs. Proven below via
//! `#[cfg(test)]` against a real temp-dir git repo + a real temp sandbox directory tree, reusing
//! reconcile-external-code-drift.rs's `ANTILOKI_SUBSTRATE_ROOT` test-injection-seam convention for
//! `repo_root()`. The other 5 detectors are pure db reads (no fs/subprocess) and get plain
//! in-memory-db native tests alongside the fs/git ones — kept in this same file (not split into a
//! fixture-eligible unit) because the aggregator (`reconcile`) unconditionally calls the fs/git
//! detectors too, so the whole module is bespoke-native per file, not per-fn.
//!
//! `SANDBOX_DIR` / `REPO_ROOT` (roots.mjs: `SUBSTRATE_ROOT` = `$ANTILOKI_SUBSTRATE_ROOT` env
//! override else `resolve(ENGINE_ROOT, '..')`; `SANDBOX_DIR` = `join(SUBSTRATE_ROOT, '.antiloki',
//! 'sandbox')`) are inlined here rather than imported — same per-file inlining convention as
//! reconcile-external-code-drift.rs / auth-sign.rs (no shared roots crate exists yet). Test seam
//! `ANTILOKI_SUBSTRATE_ROOT` has no JS equivalent and never fires in production.
//!
//! Cross-module calls (already-converted twins, reused not reimplemented):
//! `crate::system_reconcile_truth_fusion::reconcileTruthFusion`,
//! `crate::system_reconcile_external_code_drift::reconcileExternalCodeDrift`,
//! `crate::system_reconcile_resume_analyze::reconcileResumeAnalyze`. The JS wraps the LAST TWO in
//! try/catch (`reconcile_detector_error` findings on failure); `reconcileTruthFusion`'s call is
//! NOT wrapped in the JS (a throw there propagates), matched here via `?` instead of a catch arm.
//! `reconcileExternalCodeDrift`'s Rust twin returns a bare `Value` (not `Result`) — its JS
//! try/catch guard is not modeled for that call (there is nothing to catch a panic-free `Value`
//! producer's non-existent error from), matching the "guard-throws not exercised" convention.
//!
//! Frozen clock 2026-01-01 (`FROZEN_ISO`, `'2026-01-01 00:00:00'` base for every `datetime('now',
//! …)` window in the SQL below) — `ran_at` and the two `-5 minutes` / `-1 hour` recency windows.
//!
//! `context.db` missing → JS throws `err.contextMissing('reconcile', 'db')`. Not modeled: `db: &Connection`
//! is a required, non-nullable reference, same "guard-throws are not exercised" convention every
//! sibling twin uses.
// QUALITY: 5 pure db-read detectors (native-tested against an in-memory schema) + 2 bespoke fs/git
// detectors (native-tested against a real temp dir / temp git repo) + aggregator composing 3
// already-converted sibling twins; no fixture/frontier entry for this module.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

const FROZEN_ISO: &str = "2026-01-01T00:00:00.000Z";
const FROZEN_NOW: &str = "2026-01-01 00:00:00";

fn slice_n(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// See module doc: TEST-ONLY injection seam, no JS equivalent. Defaults to the real antiloki
// workspace root (this crate's parent dir), matching JS's SUBSTRATE_ROOT/REPO_ROOT computation.
fn repo_root() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}

fn sandbox_dir() -> PathBuf {
    repo_root().join(".antiloki").join("sandbox")
}

// ── Detectors ─────────────────────────────────────────────────────────

fn detect_pl_clarify_pending(db: &Connection) -> Result<Vec<Value>, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT s.id AS session_id, s.approved_card_slug, s.raw_intent, \
                COUNT(q.id) AS unanswered_count \
         FROM pl_sessions s \
         JOIN pl_questions q ON q.session_id = s.id \
         LEFT JOIN pl_answers a ON a.question_id = q.id \
         WHERE s.status = 'clarifying' AND a.id IS NULL \
         GROUP BY s.id",
        &[],
    )?;
    let rows = rows.as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for r in &rows {
        let session_id = r.get("session_id").cloned().unwrap_or(Value::Null);
        let card_slug = r.get("approved_card_slug").cloned().unwrap_or(Value::Null);
        let unanswered = r.get("unanswered_count").cloned().unwrap_or(json!(0));
        let questions = crate::db_json::query_rows(
            db,
            "SELECT q.id, q.step, q.question_md \
             FROM pl_questions q \
             LEFT JOIN pl_answers a ON a.question_id = q.id \
             WHERE q.session_id = ? AND a.id IS NULL \
             ORDER BY q.step, q.order_index",
            &[session_id.clone()],
        )?;
        let mut detail = Map::new();
        detail.insert("session_id".into(), session_id.clone());
        // `r.raw_intent?.slice(0, 200)`: optional chaining on a null column yields `undefined`,
        // which JSON.stringify OMITS from the object — a null column means the key is absent.
        if let Some(s) = r.get("raw_intent").and_then(Value::as_str) {
            detail.insert("raw_intent".into(), json!(slice_n(s, 200)));
        }
        detail.insert("questions".into(), questions);
        let card_slug_str = card_slug.as_str().unwrap_or("").to_string();
        let unanswered_str = if let Some(i) = unanswered.as_i64() { i.to_string() } else { unanswered.to_string() };
        out.push(json!({
            "kind": "pl_clarify_pending",
            "severity": "human_blocking",
            "message": format!(
                "Card '{card_slug_str}' has {unanswered_str} unanswered PL clarify question(s) — pipeline waits for human input."
            ),
            "card_slug": card_slug,
            "detail": Value::Object(detail),
            "suggested_action": "Answer each question via pl.session.answer.record (question_id + writein_md) — or abandon the session if the card no longer applies.",
        }));
    }
    Ok(out)
}

fn detect_prioritized_but_not_processed(db: &Connection) -> Result<Vec<Value>, String> {
    let cards = crate::db_json::query_rows(
        db,
        "SELECT slug FROM cards WHERE status NOT IN ('done', 'archived', 'blocked') AND priority >= 5",
        &[],
    )?;
    let cards = cards.as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for c in &cards {
        let slug = c.get("slug").cloned().unwrap_or(Value::Null);
        let recent = crate::db_json::query_rows(
            db,
            &format!(
                "SELECT COUNT(*) AS n FROM driver_ticks \
                 WHERE card_slug = ? AND started_at >= datetime('{FROZEN_NOW}', '-5 minutes')"
            ),
            &[slug.clone()],
        )?;
        let n = recent
            .as_array()
            .and_then(|a| a.first())
            .and_then(|r| r.get("n"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if n == 0 {
            let slug_str = slug.as_str().unwrap_or("").to_string();
            out.push(json!({
                "kind": "prioritized_but_not_processed",
                "severity": "drift",
                "message": format!(
                    "Card '{slug_str}' is priority=5 but no driver tick fired on it in the last 5 minutes."
                ),
                "card_slug": slug,
                "detail": { "recent_tick_count": 0, "window_minutes": 5 },
                "suggested_action": "Check halt-file (.antiloki/halt) or run `node bin/antiloki.mjs system.driver.tick '{}'` and see what the state machine matches.",
            }));
        }
    }
    Ok(out)
}

fn detect_out_of_band_card_mutation(db: &Connection) -> Result<Vec<Value>, String> {
    const GAP_SECONDS: f64 = 30.0;
    let rows = crate::db_json::query_rows(
        db,
        "SELECT c.slug, c.updated_at, \
                (SELECT MAX(ts) FROM sacred_timeline WHERE entity_type='card' AND entity_id = c.slug) AS last_audit \
         FROM cards c \
         WHERE last_audit IS NOT NULL \
           AND julianday(c.updated_at) - julianday(last_audit) > (? / 86400.0)",
        &[json!(GAP_SECONDS)],
    )?;
    let rows = rows.as_array().cloned().unwrap_or_default();
    Ok(rows
        .iter()
        .map(|r| {
            let slug = r.get("slug").cloned().unwrap_or(Value::Null);
            let updated_at = r.get("updated_at").cloned().unwrap_or(Value::Null);
            let last_audit = r.get("last_audit").cloned().unwrap_or(Value::Null);
            let slug_str = slug.as_str().unwrap_or("").to_string();
            let updated_str = updated_at.as_str().unwrap_or("").to_string();
            let last_audit_str = last_audit.as_str().unwrap_or("").to_string();
            json!({
                "kind": "out_of_band_card_mutation",
                "severity": "drift",
                "message": format!(
                    "Card '{slug_str}' was updated outside the audit chain (updated_at={updated_str}, last_audit={last_audit_str})."
                ),
                "card_slug": slug,
                "detail": { "updated_at": updated_at, "last_audit_ts": last_audit },
                "suggested_action": "Investigate who/what touched the row. If intentional, the substrate has a NEVER_BYPASS_WITH_AUDIT violation; if a tool, route it through with-audit.",
            })
        })
        .collect())
}

fn detect_stuck_tick_loop(db: &Connection) -> Result<Vec<Value>, String> {
    let rows = crate::db_json::query_rows(
        db,
        &format!(
            "SELECT card_slug, error_message, COUNT(*) AS n, \
                    MIN(started_at) AS first_fail, MAX(started_at) AS last_fail \
             FROM driver_ticks \
             WHERE outcome = 'failed' AND error_message IS NOT NULL \
               AND started_at >= datetime('{FROZEN_NOW}', '-1 hour') \
             GROUP BY card_slug, error_message \
             HAVING COUNT(*) >= 3 \
             ORDER BY n DESC"
        ),
        &[],
    )?;
    let rows = rows.as_array().cloned().unwrap_or_default();
    Ok(rows
        .iter()
        .map(|r| {
            let slug = r.get("card_slug").cloned().unwrap_or(Value::Null);
            let n = r.get("n").cloned().unwrap_or(json!(0));
            let error = r.get("error_message").and_then(Value::as_str).unwrap_or("");
            let first_fail = r.get("first_fail").cloned().unwrap_or(Value::Null);
            let last_fail = r.get("last_fail").cloned().unwrap_or(Value::Null);
            let slug_str = slug.as_str().unwrap_or("").to_string();
            let n_str = if let Some(i) = n.as_i64() { i.to_string() } else { n.to_string() };
            json!({
                "kind": "stuck_tick_loop",
                "severity": "human_blocking",
                "message": format!(
                    "Card '{slug_str}' failed {n_str} ticks in a row with the same error in the last hour."
                ),
                "card_slug": slug,
                "detail": {
                    "error": slice_n(error, 200),
                    "fail_count": n,
                    "first_fail": first_fail,
                    "last_fail": last_fail,
                },
                "suggested_action": "Run `node bin/antiloki.mjs system.driver.inspect '{\"tick_id\":\"latest\",\"render_only\":true}'`; archive the card, fix the substrate gap, or reject the contract to break the loop.",
            })
        })
        .collect())
}

/// Re-exported (JS: `export { detectContractAwaitingHumanApproval }`) so chat surfaces (greeting +
/// bootstrap-context) can pull JUST this signal without running the full battery. Table-missing
/// (or any other query failure) collapses to `[]`, matching the JS `try { ... } catch { return []; }`.
pub fn detectContractAwaitingHumanApproval(db: &Connection) -> Value {
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT slug, card_slug, tier, risk, complexity, updated_at \
         FROM contracts \
         WHERE status = 'tl_approved' AND tier = 'high-risk' AND superseded_by IS NULL \
         ORDER BY updated_at DESC",
        &[],
    ) {
        Ok(v) => v.as_array().cloned().unwrap_or_default(),
        Err(_) => return json!([]),
    };
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            let slug = r.get("slug").cloned().unwrap_or(Value::Null);
            let card_slug = r.get("card_slug").cloned().unwrap_or(Value::Null);
            let risk = r.get("risk").cloned().unwrap_or(Value::Null);
            let tier = r.get("tier").cloned().unwrap_or(Value::Null);
            let complexity = r.get("complexity").cloned().unwrap_or(Value::Null);
            let updated_at = r.get("updated_at").cloned().unwrap_or(Value::Null);
            let slug_str = slug.as_str().unwrap_or("").to_string();
            let risk_str = if risk.is_null() { "?".to_string() } else if let Some(i) = risk.as_i64() { i.to_string() } else { risk.to_string() };
            json!({
                "kind": "contract_awaiting_human_approval",
                "severity": "human_blocking",
                "message": format!("Contract '{slug_str}' is high-risk (risk={risk_str}/5) and awaiting human approve/reject."),
                "card_slug": card_slug,
                "contract_slug": slug,
                "detail": {
                    "tier": tier,
                    "risk": risk,
                    "complexity": complexity,
                    "updated_at": updated_at,
                    "html_path": format!("docs/contracts/{slug_str}.html"),
                    "approve_cmd": format!("node bin/antiloki.mjs contract.gate.human.approve '{{\"slug\":\"{slug_str}\"}}'"),
                    "reject_cmd": format!("node bin/antiloki.mjs contract.gate.human.reject '{{\"slug\":\"{slug_str}\",\"reason_md\":\"<why>\"}}'"),
                    "render_cmd": format!("node bin/antiloki.mjs contract.render.html '{{\"slug\":\"{slug_str}\"}}'"),
                },
                "suggested_action": format!(
                    "Render: `node bin/antiloki.mjs contract.render.html '{{\"slug\":\"{slug_str}\"}}'` → open docs/contracts/{slug_str}.html → approve via contract.gate.human.approve (or reject)."
                ),
            })
        })
        .collect();
    json!(out)
}

fn detect_orphan_sandboxes(db: &Connection) -> Vec<Value> {
    let root = sandbox_dir();
    if !root.exists() {
        return Vec::new();
    }
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort(); // readdirSync order is filesystem-dependent; sort for deterministic test output
    let mut out = Vec::new();
    for name in &names {
        if !name.contains("CONTRACT") {
            continue;
        }
        let row = crate::db_json::query_rows(db, "SELECT slug, status FROM contracts WHERE slug = ?", &[json!(name)])
            .ok()
            .and_then(|v| v.as_array().and_then(|a| a.first().cloned()));
        match row {
            None => {
                out.push(json!({
                    "kind": "orphan_sandbox",
                    "severity": "cleanup",
                    "message": format!("Sandbox dir '.antiloki/sandbox/{name}' has no matching contract row."),
                    "detail": { "dir": name },
                    "suggested_action": format!("If safe: rm -rf .antiloki/sandbox/{name}"),
                }));
            }
            Some(c) => {
                let status = c.get("status").and_then(Value::as_str).unwrap_or("").to_string();
                if ["done", "rejected", "superseded", "archived"].contains(&status.as_str()) {
                    let slug = c.get("slug").cloned().unwrap_or(Value::Null);
                    let slug_str = slug.as_str().unwrap_or("").to_string();
                    out.push(json!({
                        "kind": "orphan_sandbox",
                        "severity": "cleanup",
                        "message": format!("Sandbox dir for contract '{slug_str}' (status={status}) — old work not cleaned up."),
                        "contract_slug": slug,
                        "detail": { "dir": name, "contract_status": status },
                        "suggested_action": format!("If safe: rm -rf .antiloki/sandbox/{name}"),
                    }));
                }
            }
        }
    }
    out
}

const EXCLUDED_PREFIXES: [&str; 5] =
    [".antiloki/db/", ".antiloki/sandbox/", ".antiloki/run/", "docs/contracts/", "docs/proof-runs/"];
const SQLITE_SUFFIXES: [&str; 3] = [".sqlite", ".sqlite-wal", ".sqlite-shm"];

fn is_excluded_working_tree(p: &str) -> bool {
    EXCLUDED_PREFIXES.iter().any(|pfx| p.starts_with(pfx)) || SQLITE_SUFFIXES.iter().any(|sfx| p.ends_with(sfx))
}

fn detect_working_tree_outside_contracts(db: &Connection) -> Result<Vec<Value>, String> {
    let root = repo_root();
    let out = crate::proc::cmd("git").args(["status", "--porcelain"]).current_dir(&root).output();
    let porcelain = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => {
            return Ok(vec![json!({
                "kind": "reconcile_internal",
                "severity": "info",
                "message": "git status failed — skipping working-tree drift check",
                "detail": {},
            })])
        }
    };
    // `porcelain.split('\n').map(trim).filter(Boolean).map((l) => l.slice(3)).filter((p) => p && !isExcluded(p))`
    let changed: Vec<String> = porcelain
        .split('\n')
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
            let chars: Vec<char> = l.chars().collect();
            if chars.len() > 3 { chars[3..].iter().collect::<String>() } else { String::new() }
        })
        .filter(|p| !p.is_empty() && !is_excluded_working_tree(p))
        .collect();
    if changed.is_empty() {
        return Ok(Vec::new());
    }
    let contracts = crate::db_json::query_rows(
        db,
        "SELECT slug, files_touched_json FROM contracts \
         WHERE status NOT IN ('done', 'rejected', 'superseded', 'archived')",
        &[],
    )?;
    let contracts = contracts.as_array().cloned().unwrap_or_default();
    let mut all_allowed: std::collections::HashSet<String> = std::collections::HashSet::new();
    for c in &contracts {
        let parsed: Value = c
            .get("files_touched_json")
            .and_then(Value::as_str)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(json!([]));
        if let Some(arr) = parsed.as_array() {
            for p in arr {
                if let Some(s) = p.as_str() {
                    all_allowed.insert(s.to_string());
                }
            }
        }
    }
    let out_of_scope: Vec<Value> = changed.iter().filter(|p| !all_allowed.contains(*p)).map(|p| json!(p)).collect();
    if out_of_scope.is_empty() {
        return Ok(Vec::new());
    }
    let total = out_of_scope.len() as i64;
    let files: Vec<Value> = out_of_scope.into_iter().take(20).collect();
    Ok(vec![json!({
        "kind": "working_tree_outside_contracts",
        "severity": "info",
        "message": format!("{total} modified file(s) in the working tree are NOT declared in any open contract's files_touched."),
        "detail": { "files": files, "total": total },
        "suggested_action": "If the changes are intentional, commit + reset contract scope; if accidental, git restore.",
    })])
}

// ── Aggregator ────────────────────────────────────────────────────────

/// `export default function reconcile(input, context)`.
pub fn reconcile(input: &Value, db: &Connection) -> Result<Value, String> {
    let mut findings: Vec<Value> = Vec::new();
    findings.extend(detect_pl_clarify_pending(db)?);
    findings.extend(detect_prioritized_but_not_processed(db)?);
    findings.extend(detect_out_of_band_card_mutation(db)?);
    findings.extend(detect_stuck_tick_loop(db)?);
    findings.extend(detectContractAwaitingHumanApproval(db).as_array().cloned().unwrap_or_default());
    findings.extend(detect_orphan_sandboxes(db));
    findings.extend(detect_working_tree_outside_contracts(db)?);

    // #383 truth-fusion: composed at the end so legacy callers see no change to the first 7
    // detectors' ordering. JS does NOT wrap this call in try/catch — a throw propagates.
    let tf = crate::system_reconcile_truth_fusion::reconcileTruthFusion(input, db)?;
    findings.extend(tf.get("findings").and_then(Value::as_array).cloned().unwrap_or_default());

    // FEAT-RECONCILE-WIRES-RESUME-AND-EXTERNAL-DRIFT: each wrapped defensively in the JS.
    let ed = crate::system_reconcile_external_code_drift::reconcileExternalCodeDrift(input, db);
    findings.extend(ed.get("findings").and_then(Value::as_array).cloned().unwrap_or_default());

    match crate::system_reconcile_resume_analyze::reconcileResumeAnalyze(input, db) {
        Ok(ra) => findings.extend(ra.get("findings").and_then(Value::as_array).cloned().unwrap_or_default()),
        Err(e) => findings.push(json!({
            "kind": "reconcile_detector_error",
            "severity": "cleanup",
            "message": format!("resume-analyze detector failed: {e}"),
            "detail": { "detector": "reconcile-resume-analyze" },
            "suggested_action": "Inspect open contracts state; resume-analyze is bounded but may hit an unexpected row shape.",
        })),
    }

    let mut by_sev: Map<String, Value> = Map::new();
    for f in &findings {
        let sev = f.get("severity").and_then(Value::as_str).unwrap_or("").to_string();
        let n = by_sev.get(&sev).and_then(Value::as_i64).unwrap_or(0) + 1;
        by_sev.insert(sev, json!(n));
    }
    let mut by_kind: Map<String, Value> = Map::new();
    for f in &findings {
        let kind = f.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
        let n = by_kind.get(&kind).and_then(Value::as_i64).unwrap_or(0) + 1;
        by_kind.insert(kind, json!(n));
    }

    Ok(json!({
        "ran_at": FROZEN_ISO,
        "findings": findings,
        "summary": { "total": findings.len() as i64, "by_severity": Value::Object(by_sev), "by_kind": Value::Object(by_kind) },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // SAFETY: ANTILOKI_SUBSTRATE_ROOT is a process-global env var read by several twins' tests
    // across the crate — shared crate-wide lock (test_env_lock.rs); a local mutex would not
    // prevent a race with OTHER files' tests over the same real env var.
    fn with_substrate_root<T>(root: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-reconcile-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn git(args: &[&str], cwd: &Path) {
        let status = crate::proc::cmd("git").args(args).current_dir(cwd).status().unwrap();
        assert!(status.success(), "git {args:?} failed in {cwd:?}");
    }

    fn init_repo(dir: &Path) {
        git(&["init", "-q"], dir);
        git(&["config", "user.email", "test@example.com"], dir);
        git(&["config", "user.name", "Test"], dir);
        fs::write(dir.join("README.md"), "# t\n").unwrap();
        git(&["add", "-A"], dir);
        git(&["commit", "-q", "-m", "init"], dir);
    }

    fn full_schema() -> &'static str {
        "CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT, priority INTEGER, updated_at TEXT);
         CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT, status TEXT, tier TEXT, risk INTEGER, \
             complexity TEXT, sandbox_path TEXT, files_touched_json TEXT, superseded_by TEXT, updated_at TEXT, \
             current_attempt INTEGER, dev_microtask_progress INTEGER, microtasks_json TEXT);
         CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, entity_type TEXT, entity_id TEXT, ts TEXT);
         CREATE TABLE driver_ticks (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, started_at TEXT, \
             outcome TEXT, error_message TEXT, decided_action TEXT);
         CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY AUTOINCREMENT, approved_card_slug TEXT, raw_intent TEXT, status TEXT);
         CREATE TABLE pl_questions (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id INTEGER, step INTEGER, \
             order_index INTEGER, question_md TEXT);
         CREATE TABLE pl_answers (id INTEGER PRIMARY KEY AUTOINCREMENT, question_id INTEGER);"
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(full_schema()).unwrap();
        c
    }

    // ── detect_pl_clarify_pending ──────────────────────────────────────

    #[test]
    fn pl_clarify_pending_surfaces_unanswered_questions_and_omits_null_raw_intent() {
        let c = db();
        c.execute(
            "INSERT INTO pl_sessions (id, approved_card_slug, raw_intent, status) VALUES (1, 'FEAT-X', NULL, 'clarifying')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO pl_questions (id, session_id, step, order_index, question_md) VALUES (10, 1, 1, 0, 'What scope?')",
            [],
        )
        .unwrap();
        let findings = detect_pl_clarify_pending(&c).unwrap();
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f["kind"], json!("pl_clarify_pending"));
        assert_eq!(f["severity"], json!("human_blocking"));
        assert_eq!(f["card_slug"], json!("FEAT-X"));
        assert!(f["message"].as_str().unwrap().contains("1 unanswered"));
        assert!(!f["detail"].as_object().unwrap().contains_key("raw_intent"), "null raw_intent must be omitted");
        assert_eq!(f["detail"]["questions"][0]["question_md"], json!("What scope?"));
    }

    #[test]
    fn pl_clarify_pending_keeps_sliced_raw_intent_when_present() {
        let c = db();
        c.execute(
            "INSERT INTO pl_sessions (id, approved_card_slug, raw_intent, status) VALUES (1, 'FEAT-Y', 'do the thing', 'clarifying')",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO pl_questions (id, session_id, step, order_index, question_md) VALUES (10, 1, 1, 0, 'q1')", []).unwrap();
        let findings = detect_pl_clarify_pending(&c).unwrap();
        assert_eq!(findings[0]["detail"]["raw_intent"], json!("do the thing"));
    }

    #[test]
    fn pl_clarify_pending_answered_question_produces_no_finding() {
        let c = db();
        c.execute("INSERT INTO pl_sessions (id, approved_card_slug, status) VALUES (1, 'FEAT-Z', 'clarifying')", []).unwrap();
        c.execute("INSERT INTO pl_questions (id, session_id, step, order_index, question_md) VALUES (10, 1, 1, 0, 'q1')", []).unwrap();
        c.execute("INSERT INTO pl_answers (id, question_id) VALUES (100, 10)", []).unwrap();
        assert_eq!(detect_pl_clarify_pending(&c).unwrap(), Vec::<Value>::new());
    }

    // ── detect_prioritized_but_not_processed ──────────────────────────

    #[test]
    fn prioritized_card_with_no_recent_tick_is_flagged() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, priority) VALUES ('FEAT-A', 'in_progress', 5)", []).unwrap();
        let findings = detect_prioritized_but_not_processed(&c).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["kind"], json!("prioritized_but_not_processed"));
        assert_eq!(findings[0]["card_slug"], json!("FEAT-A"));
    }

    #[test]
    fn prioritized_card_with_recent_tick_inside_window_is_not_flagged() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, priority) VALUES ('FEAT-B', 'in_progress', 5)", []).unwrap();
        c.execute(
            "INSERT INTO driver_ticks (card_slug, started_at, outcome) VALUES ('FEAT-B', '2025-12-31 23:57:00', 'ok')",
            [],
        )
        .unwrap();
        assert_eq!(detect_prioritized_but_not_processed(&c).unwrap(), Vec::<Value>::new());
    }

    #[test]
    fn prioritized_card_with_tick_outside_window_is_still_flagged() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, priority) VALUES ('FEAT-C', 'in_progress', 5)", []).unwrap();
        c.execute(
            "INSERT INTO driver_ticks (card_slug, started_at, outcome) VALUES ('FEAT-C', '2025-12-31 23:00:00', 'ok')",
            [],
        )
        .unwrap();
        assert_eq!(detect_prioritized_but_not_processed(&c).unwrap().len(), 1);
    }

    #[test]
    fn low_priority_or_terminal_card_is_never_scanned() {
        let c = db();
        c.execute("INSERT INTO cards (slug, status, priority) VALUES ('FEAT-D', 'in_progress', 3)", []).unwrap();
        c.execute("INSERT INTO cards (slug, status, priority) VALUES ('FEAT-E', 'done', 5)", []).unwrap();
        assert_eq!(detect_prioritized_but_not_processed(&c).unwrap(), Vec::<Value>::new());
    }

    // ── detect_out_of_band_card_mutation ───────────────────────────────

    #[test]
    fn card_mutated_well_after_last_audit_row_is_flagged_as_drift() {
        let c = db();
        c.execute("INSERT INTO cards (slug, updated_at) VALUES ('FEAT-F', '2026-01-02 00:00:00')", []).unwrap();
        c.execute(
            "INSERT INTO sacred_timeline (entity_type, entity_id, ts) VALUES ('card', 'FEAT-F', '2026-01-01 00:00:00')",
            [],
        )
        .unwrap();
        let findings = detect_out_of_band_card_mutation(&c).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["kind"], json!("out_of_band_card_mutation"));
        assert_eq!(findings[0]["card_slug"], json!("FEAT-F"));
    }

    #[test]
    fn card_mutated_within_the_30s_gap_is_not_flagged() {
        let c = db();
        c.execute("INSERT INTO cards (slug, updated_at) VALUES ('FEAT-G', '2026-01-01 00:00:05')", []).unwrap();
        c.execute(
            "INSERT INTO sacred_timeline (entity_type, entity_id, ts) VALUES ('card', 'FEAT-G', '2026-01-01 00:00:00')",
            [],
        )
        .unwrap();
        assert_eq!(detect_out_of_band_card_mutation(&c).unwrap(), Vec::<Value>::new());
    }

    #[test]
    fn card_with_zero_audit_rows_is_historical_not_drift() {
        let c = db();
        c.execute("INSERT INTO cards (slug, updated_at) VALUES ('FEAT-H', '2026-01-02 00:00:00')", []).unwrap();
        assert_eq!(detect_out_of_band_card_mutation(&c).unwrap(), Vec::<Value>::new());
    }

    // ── detect_stuck_tick_loop ──────────────────────────────────────────

    #[test]
    fn three_identical_recent_failures_trip_the_stuck_loop_detector() {
        let c = db();
        for _ in 0..3 {
            c.execute(
                "INSERT INTO driver_ticks (card_slug, started_at, outcome, error_message) \
                 VALUES ('FEAT-I', '2025-12-31 23:50:00', 'failed', 'boom')",
                [],
            )
            .unwrap();
        }
        let findings = detect_stuck_tick_loop(&c).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["card_slug"], json!("FEAT-I"));
        assert_eq!(findings[0]["detail"]["fail_count"], json!(3));
    }

    #[test]
    fn only_two_failures_does_not_trip_the_detector() {
        let c = db();
        for _ in 0..2 {
            c.execute(
                "INSERT INTO driver_ticks (card_slug, started_at, outcome, error_message) \
                 VALUES ('FEAT-J', '2025-12-31 23:50:00', 'failed', 'boom')",
                [],
            )
            .unwrap();
        }
        assert_eq!(detect_stuck_tick_loop(&c).unwrap(), Vec::<Value>::new());
    }

    // ── detectContractAwaitingHumanApproval ─────────────────────────────

    #[test]
    fn high_risk_tl_approved_contract_is_surfaced() {
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, status, tier, risk, complexity, updated_at) \
             VALUES ('CONTRACT-1', 'FEAT-K', 'tl_approved', 'high-risk', 4, 'high', '2026-01-01 00:00:00')",
            [],
        )
        .unwrap();
        let out = detectContractAwaitingHumanApproval(&c);
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["contract_slug"], json!("CONTRACT-1"));
        assert!(arr[0]["message"].as_str().unwrap().contains("risk=4/5"));
    }

    #[test]
    fn missing_contracts_table_collapses_to_empty_array_not_an_error() {
        let c = Connection::open_in_memory().unwrap(); // no schema at all
        assert_eq!(detectContractAwaitingHumanApproval(&c), json!([]));
    }

    // ── detect_orphan_sandboxes ──────────────────────────────────────────

    #[test]
    fn sandbox_dir_with_no_matching_contract_row_is_flagged_orphan() {
        let root = temp_dir("orphan-none");
        let sandbox = root.join(".antiloki").join("sandbox");
        fs::create_dir_all(sandbox.join("CONTRACT-GHOST")).unwrap();
        let c = db();
        let findings = with_substrate_root(root.to_str().unwrap(), || detect_orphan_sandboxes(&c));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["detail"]["dir"], json!("CONTRACT-GHOST"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sandbox_dir_for_a_done_contract_is_flagged_cleanup() {
        let root = temp_dir("orphan-done");
        let sandbox = root.join(".antiloki").join("sandbox");
        fs::create_dir_all(sandbox.join("CONTRACT-DONE")).unwrap();
        let c = db();
        c.execute("INSERT INTO contracts (slug, status) VALUES ('CONTRACT-DONE', 'done')", []).unwrap();
        let findings = with_substrate_root(root.to_str().unwrap(), || detect_orphan_sandboxes(&c));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["contract_slug"], json!("CONTRACT-DONE"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn sandbox_dir_for_an_active_contract_is_not_flagged() {
        let root = temp_dir("orphan-active");
        let sandbox = root.join(".antiloki").join("sandbox");
        fs::create_dir_all(sandbox.join("CONTRACT-ACTIVE")).unwrap();
        let c = db();
        c.execute("INSERT INTO contracts (slug, status) VALUES ('CONTRACT-ACTIVE', 'impl')", []).unwrap();
        let findings = with_substrate_root(root.to_str().unwrap(), || detect_orphan_sandboxes(&c));
        assert_eq!(findings, Vec::<Value>::new());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_sandbox_root_directory_is_a_safe_noop() {
        let root = temp_dir("orphan-missing-root");
        let c = db();
        let findings = with_substrate_root(root.to_str().unwrap(), || detect_orphan_sandboxes(&c));
        assert_eq!(findings, Vec::<Value>::new());
        let _ = fs::remove_dir_all(&root);
    }

    // ── detect_working_tree_outside_contracts ────────────────────────────

    #[test]
    fn clean_working_tree_produces_no_findings() {
        let root = temp_dir("wt-clean");
        init_repo(&root);
        let c = db();
        let findings = with_substrate_root(root.to_str().unwrap(), || detect_working_tree_outside_contracts(&c).unwrap());
        assert_eq!(findings, Vec::<Value>::new());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn modified_file_outside_any_contract_scope_is_flagged() {
        let root = temp_dir("wt-outside");
        init_repo(&root);
        fs::write(root.join("orphan.mjs"), "x").unwrap();
        let c = db();
        let findings = with_substrate_root(root.to_str().unwrap(), || detect_working_tree_outside_contracts(&c).unwrap());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["kind"], json!("working_tree_outside_contracts"));
        assert_eq!(findings[0]["detail"]["total"], json!(1));
        assert_eq!(findings[0]["detail"]["files"], json!(["orphan.mjs"]));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn modified_file_declared_in_an_open_contract_is_not_flagged() {
        let root = temp_dir("wt-claimed");
        init_repo(&root);
        fs::write(root.join("claimed.mjs"), "x").unwrap();
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, status, files_touched_json) VALUES ('CONTRACT-2', 'impl', '[\"claimed.mjs\"]')",
            [],
        )
        .unwrap();
        let findings = with_substrate_root(root.to_str().unwrap(), || detect_working_tree_outside_contracts(&c).unwrap());
        assert_eq!(findings, Vec::<Value>::new());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn excluded_db_and_sandbox_paths_never_surface_as_drift() {
        // `git status --porcelain` reports an *untracked directory* as the directory path itself
        // (e.g. ".antiloki/"), not the files inside it — it never recurses into an untracked dir.
        // A path of ".antiloki/" would NOT match the ".antiloki/db/" exclusion prefix, so to
        // exercise the exclusion for real the file must already be tracked (committed at init)
        // and then modified, which makes git report the file-level path ".antiloki/db/state.sqlite".
        let root = temp_dir("wt-excluded");
        fs::create_dir_all(root.join(".antiloki").join("db")).unwrap();
        fs::write(root.join(".antiloki").join("db").join("state.sqlite"), "x").unwrap();
        init_repo(&root);
        fs::write(root.join(".antiloki").join("db").join("state.sqlite"), "y").unwrap();
        let c = db();
        let findings = with_substrate_root(root.to_str().unwrap(), || detect_working_tree_outside_contracts(&c).unwrap());
        assert_eq!(findings, Vec::<Value>::new());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn not_a_git_repo_returns_the_internal_skip_finding() {
        let root = temp_dir("wt-not-a-repo");
        let c = db();
        let findings = with_substrate_root(root.to_str().unwrap(), || detect_working_tree_outside_contracts(&c).unwrap());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0]["kind"], json!("reconcile_internal"));
        let _ = fs::remove_dir_all(&root);
    }

    // ── aggregator ────────────────────────────────────────────────────

    #[test]
    fn reconcile_aggregates_findings_from_multiple_detectors_and_tallies_summary() {
        let root = temp_dir("agg");
        init_repo(&root);
        let c = db();
        c.execute("INSERT INTO cards (slug, status, priority, updated_at) VALUES ('FEAT-L', 'in_progress', 5, '2026-01-01 00:00:00')", []).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, status, tier, risk, complexity, updated_at) \
             VALUES ('CONTRACT-3', 'FEAT-L', 'tl_approved', 'high-risk', 5, 'high', '2026-01-01 00:00:00')",
            [],
        )
        .unwrap();
        let r = with_substrate_root(root.to_str().unwrap(), || reconcile(&json!({}), &c).unwrap());
        assert_eq!(r["ran_at"], json!(FROZEN_ISO));
        let findings = r["findings"].as_array().unwrap();
        assert!(findings.iter().any(|f| f["kind"] == json!("prioritized_but_not_processed")));
        assert!(findings.iter().any(|f| f["kind"] == json!("contract_awaiting_human_approval")));
        assert_eq!(r["summary"]["total"], json!(findings.len() as i64));
        let by_kind = r["summary"]["by_kind"].as_object().unwrap();
        let total_by_kind: i64 = by_kind.values().map(|v| v.as_i64().unwrap()).sum();
        assert_eq!(total_by_kind, findings.len() as i64);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn reconcile_on_a_fully_quiet_substrate_returns_zero_findings() {
        let root = temp_dir("agg-quiet");
        init_repo(&root);
        let c = db();
        let r = with_substrate_root(root.to_str().unwrap(), || reconcile(&json!({}), &c).unwrap());
        assert_eq!(r["findings"], json!([]));
        assert_eq!(r["summary"]["total"], json!(0));
        let _ = fs::remove_dir_all(&root);
    }
}
