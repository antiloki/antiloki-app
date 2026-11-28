//! reconcile-apply.rs — twin of skills/system/reconcile-apply.mjs (standard lane, `reconcileApply`
//! default export). Iterates the reconciler registry (`skills/reconcilers/*.mjs`), runs `detect`,
//! then gates `fix` per finding on `dry_run` / `only_safety` / a per-finding `option_key` pick.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: the JS source dynamically discovers reconcilers
//! via `readdirSync` + `await import(...)` (skills/reconcilers/index.mjs — itself a documented
//! PARITY-DEFERRED boundary, see reconcilers/index.rs). This twin cannot re-derive that dynamic
//! registry either, so it HARDCODES the same 23-reconciler roster as a `const` table (id, safety,
//! scope), sorted by id exactly like `listReconcilers()`'s `.sort((a,b) => a.id.localeCompare(b.id))`
//! — every entry mirrors a reconciler that ALREADY has a converted Rust twin under
//! `skills/reconcilers/*.rs` (verified against `src/lib.rs`'s `reconcilers_*` mod list before
//! writing this file). If a 24th reconciler is added to the JS registry, this table needs a
//! matching hand-edit — same "registry drifts from source" caveat every hardcoded-roster twin in
//! this crate carries (documented, not silently wrong: the roster is exhaustive as of this port).
//!
//! `r.scope` note: the JS doc comment claims `scope` "default 'global'", but the ACTUAL filter is
//! `r.scope === 'global'` — a reconciler with NO `scope` key (JS: `undefined`) does NOT match that
//! strict-equality check, so it is NEVER excluded by a `card_slug` scope filter even though the doc
//! says it should behave as "global". `driver-ticks-bloat.mjs` genuinely has no `scope` field in its
//! default export (verified against source) — ported here as `scope: None`, not `Some("global")`,
//! to match the REAL (not documented) filter behavior.
//!
//! Every reconciler's `detect`/`fix` Rust twin returns a bare `Value` (not `Result`) — each one
//! already collapses its own internal failures (e.g. "table absent") to an empty/safe result
//! exactly like the JS `try { ... } catch { ... }` guards inside each reconciler. That means the
//! JS `reconcile-apply.mjs`'s own outer `try { detected = await r.detect(db, ...) } catch (e) {
//! ...'reconciler-error' finding... }` and the `try { result = ... } catch (e) { ...errored++... }`
//! around `r.fix(...)` are BOTH unreachable on this call path (same "guard-throws not exercised"
//! convention used throughout the crate) — `errored` stays `0` and no `reconciler-error` finding is
//! ever produced by THIS twin's registry, even though the JS code technically has that branch.
//!
//! `option_key` / `fix_options[].fix` — PARITY-DEFERRED, matching an established crate-wide
//! precedent: every reconciler twin that has `fix_options` (brain-decisions-bloat,
//! brain-envelope-bloat, driver-ticks-bloat, escalated-contract-no-more-work,
//! playing-card-no-driver-progress, runtime-events-bloat, stuck-contracts, unread-error-inbox)
//! stores each option's `fix` as a STRINGIFIED JS function SOURCE (`FIX_A_SRC` etc — the exact
//! Function.prototype.toString() text), not a callable value — those files' own doc comments say
//! this explicitly ("the conform JS lane's normalizeResult stringifies them via String(v)"). The
//! per-option closures themselves are private free functions inside their own files (e.g.
//! `driver-ticks-bloat.rs::prune`), not `pub`, and several `await import(...)` an ADDITIONAL
//! sibling skill at call time (e.g. stuck-contracts option B → `card-restart-from-failure.mjs`).
//! Faithfully invoking `chosen.fix(finding, db)` here would require either (a) making those private
//! fns `pub` in files this port must not touch (SPEED PROTOCOL: write-only, no shared-file edits),
//! or (b) re-implementing each option's logic a second time in this file — both rejected. So: the
//! JS branch `typeof chosen.fix === 'function' → call it` is ported as `Value::String(_) => false`
//! (our `fix` is always a string, never "callable"), which deterministically takes the JS `else`
//! branch instead: `result = r.fix(f, db, ctx)` (the reconciler's own generic `.fix()`, ignoring
//! `option_key`) — exactly the fallback the JS itself uses whenever `chosen.fix` is NOT a function.
//! `option_key` matching an UNKNOWN key still returns the JS's `{applied:false, reason:"option key
//! '<k>' not in fix_options"}` verbatim, since that only depends on the `key` field, which IS ported
//! faithfully (`{key, label}` per option, `fix` field ignored/undispatched).
//!
//! Post-fix side effects (`memoryAdd`, `inboxMessageAdd`, `cardBodyGenerate` + `cardAdd` via a JS
//! `await import(...)` deferred boundary) reuse the already-converted twins
//! `crate::entities_memory_add::memoryAdd`, `crate::entities_inbox_message_add::inboxMessageAdd`,
//! `crate::system_card_body_generate::cardBodyGenerate`, `crate::entities_card_add::cardAdd` — all
//! best-effort (errors swallowed), matching the JS `try { ... } catch { /* best-effort */ }` guards.
//!
//! Frozen clock 2026-01-01 is NOT directly referenced in this file (no `datetime('now')` SQL here);
//! any frozen-clock behavior lives inside the reconciler twins this file calls.
//!
//! `context.db` missing → JS throws `err.contextMissing('reconcile-apply', 'db')`. Not modeled:
//! `db: &Connection` is a required, non-nullable reference (same convention every sibling twin uses).
// QUALITY: registry-composition dispatcher over 23 already-converted reconciler twins; the dynamic
// module-discovery + per-option-closure-invocation halves of the JS are PARITY-DEFERRED (documented
// above) — everything else (safety gating, dry-run, confidence/needs-option-pick logic, side
// effects) is ported and native-tested.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn safety_rank(s: &str) -> i64 {
    match s {
        "safe" => 1,
        "risky" => 2,
        "manual" => 3,
        _ => 0,
    }
}

/// The registry, hardcoded + sorted by id — see module doc.
const REGISTRY: &[(&str, &str, Option<&str>)] = &[
    ("brain-decisions-bloat", "risky", Some("global")),
    ("brain-envelope-bloat", "risky", Some("global")),
    ("card-blocked-but-intent-play", "risky", Some("global")),
    ("card-causes-repeat-failures", "safe", Some("global")),
    ("card-intent-invariants", "safe", Some("global")),
    ("card-status-mirrors-work", "safe", Some("global")),
    ("contracts-rejected-no-redraft", "safe", Some("global")),
    ("dev-microtasks-leaked-amendment", "safe", Some("global")),
    ("driver-ticks-bloat", "risky", None),
    ("escalated-contract-no-more-work", "manual", Some("global")),
    ("inflight-ai-calls", "manual", Some("global")),
    ("orphan-sandbox-dirs", "safe", Some("global")),
    ("orphan-worktree-dirs", "safe", Some("global")),
    ("pending-warn-notifications", "manual", Some("global")),
    ("playing-card-no-driver-progress", "manual", Some("global")),
    ("recent-blocked-actions", "manual", Some("global")),
    ("red-notifs-blocking-dispatch", "risky", Some("global")),
    ("runtime-events-bloat", "risky", Some("global")),
    ("stale-pid-files", "safe", Some("global")),
    ("stuck-contracts", "manual", Some("global")),
    ("stuck-red-notifs", "safe", Some("global")),
    ("uncommitted-dev-output", "safe", Some("global")),
    ("unread-error-inbox", "manual", Some("global")),
];

fn run_detect(id: &str, db: &Connection) -> Value {
    match id {
        "brain-decisions-bloat" => crate::reconcilers_brain_decisions_bloat::detect(db),
        "brain-envelope-bloat" => crate::reconcilers_brain_envelope_bloat::detect(db),
        "card-blocked-but-intent-play" => crate::reconcilers_card_blocked_but_intent_play::detect(db),
        "card-causes-repeat-failures" => crate::reconcilers_card_causes_repeat_failures::detect(db),
        "card-intent-invariants" => crate::reconcilers_card_intent_invariants::detect(db),
        "card-status-mirrors-work" => crate::reconcilers_card_status_mirrors_work::detect(db),
        "contracts-rejected-no-redraft" => crate::reconcilers_contracts_rejected_no_redraft::detect(db),
        "dev-microtasks-leaked-amendment" => crate::reconcilers_dev_microtasks_leaked_amendment::detect(db),
        "driver-ticks-bloat" => crate::reconcilers_driver_ticks_bloat::detect(db),
        "escalated-contract-no-more-work" => crate::reconcilers_escalated_contract_no_more_work::detect(db),
        "inflight-ai-calls" => crate::reconcilers_inflight_ai_calls::detect(db),
        "orphan-sandbox-dirs" => crate::reconcilers_orphan_sandbox_dirs::detect(db),
        "orphan-worktree-dirs" => crate::reconcilers_orphan_worktree_dirs::detect(db, None),
        "pending-warn-notifications" => crate::reconcilers_pending_warn_notifications::detect(db),
        "playing-card-no-driver-progress" => crate::reconcilers_playing_card_no_driver_progress::detect(db),
        "recent-blocked-actions" => crate::reconcilers_recent_blocked_actions::detect(db),
        "red-notifs-blocking-dispatch" => crate::reconcilers_red_notifs_blocking_dispatch::detect(db),
        "runtime-events-bloat" => crate::reconcilers_runtime_events_bloat::detect(db),
        "stale-pid-files" => crate::reconcilers_stale_pid_files::detect(db),
        "stuck-contracts" => crate::reconcilers_stuck_contracts::detect(db),
        "stuck-red-notifs" => crate::reconcilers_stuck_red_notifs::detect(db),
        "uncommitted-dev-output" => crate::reconcilers_uncommitted_dev_output::detect(db),
        "unread-error-inbox" => crate::reconcilers_unread_error_inbox::detect(db),
        _ => json!([]),
    }
}

fn run_fix(id: &str, finding: &Value, db: &Connection) -> Value {
    match id {
        "brain-decisions-bloat" => crate::reconcilers_brain_decisions_bloat::fix(finding, db),
        "brain-envelope-bloat" => crate::reconcilers_brain_envelope_bloat::fix(finding, db),
        "card-blocked-but-intent-play" => crate::reconcilers_card_blocked_but_intent_play::fix(finding, db),
        "card-causes-repeat-failures" => crate::reconcilers_card_causes_repeat_failures::fix(finding, db),
        "card-intent-invariants" => crate::reconcilers_card_intent_invariants::fix(finding, db),
        "card-status-mirrors-work" => crate::reconcilers_card_status_mirrors_work::fix(finding, db),
        "contracts-rejected-no-redraft" => crate::reconcilers_contracts_rejected_no_redraft::fix(finding, db),
        "dev-microtasks-leaked-amendment" => crate::reconcilers_dev_microtasks_leaked_amendment::fix(finding, db),
        "driver-ticks-bloat" => crate::reconcilers_driver_ticks_bloat::fix(finding, db),
        "escalated-contract-no-more-work" => crate::reconcilers_escalated_contract_no_more_work::fix(finding, db),
        "inflight-ai-calls" => crate::reconcilers_inflight_ai_calls::fix(finding, db),
        "orphan-sandbox-dirs" => crate::reconcilers_orphan_sandbox_dirs::fix(finding, db),
        "orphan-worktree-dirs" => crate::reconcilers_orphan_worktree_dirs::fix(finding, db),
        "pending-warn-notifications" => crate::reconcilers_pending_warn_notifications::fix(finding, db),
        "playing-card-no-driver-progress" => crate::reconcilers_playing_card_no_driver_progress::fix(finding, db),
        "recent-blocked-actions" => crate::reconcilers_recent_blocked_actions::fix(finding, db),
        "red-notifs-blocking-dispatch" => crate::reconcilers_red_notifs_blocking_dispatch::fix(finding, db),
        "runtime-events-bloat" => crate::reconcilers_runtime_events_bloat::fix(finding, db),
        "stale-pid-files" => crate::reconcilers_stale_pid_files::fix(finding, db),
        "stuck-contracts" => crate::reconcilers_stuck_contracts::fix(finding, db),
        "stuck-red-notifs" => crate::reconcilers_stuck_red_notifs::fix(finding, db),
        "uncommitted-dev-output" => crate::reconcilers_uncommitted_dev_output::fix(finding, db),
        "unread-error-inbox" => crate::reconcilers_unread_error_inbox::fix(finding, db),
        _ => json!({ "applied": false, "reason": format!("unknown reconciler '{id}'") }),
    }
}

/// `export default async function reconcileApply(input, context)`.
pub fn reconcileApply(input: &Value, db: &Connection) -> Result<Value, String> {
    let names: Option<HashSet<&str>> = input
        .get("names")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .map(|a| a.iter().filter_map(Value::as_str).collect());
    // `dry_run !== false` — anything other than the literal boolean false defaults true.
    let dry_run = !matches!(input.get("dry_run"), Some(Value::Bool(false)));
    let safety = input
        .get("only_safety")
        .filter(|v| !v.is_null())
        .and_then(Value::as_str)
        .unwrap_or("safe");
    if !["safe", "risky", "all"].contains(&safety) {
        return Err(format!(
            "reconcile-apply: only_safety must be one of safe/risky/all (got '{safety}')"
        ));
    }
    let card_slug = input.get("card_slug").and_then(Value::as_str).filter(|s| !s.is_empty());
    let option_key = input.get("option_key").and_then(Value::as_str).filter(|s| !s.is_empty());
    let custom_guidance = input.get("custom_guidance").and_then(Value::as_str).filter(|s| !s.is_empty());
    let finding_kind = input.get("finding_kind").and_then(Value::as_str).filter(|s| !s.is_empty());

    let selected: Vec<(&str, &str, Option<&str>)> = REGISTRY
        .iter()
        .copied()
        .filter(|(id, _, scope)| {
            if let Some(ns) = &names {
                if !ns.contains(id) {
                    return false;
                }
            }
            if card_slug.is_some() && *scope == Some("global") {
                return false;
            }
            true
        })
        .collect();

    let mut findings: Vec<Value> = Vec::new();
    let mut fixes: Vec<(String, Value, Value)> = Vec::new(); // (reconciler_id, tagged_finding, result)
    let mut by_reconciler: Map<String, Value> = Map::new();
    let errored: i64 = 0; // see module doc: unreachable on this call path

    for (id, r_safety, _scope) in selected.iter().copied() {
        let mut r_fixed: i64 = 0;
        let mut r_skipped: i64 = 0;
        let r_errored: i64 = 0; // see module doc: unreachable on this call path

        let mut detected: Vec<Value> = run_detect(id, db).as_array().cloned().unwrap_or_default();
        if let Some(cs) = card_slug {
            detected.retain(|f| {
                let fcs = f.get("card_slug");
                !truthy(fcs) || fcs.and_then(Value::as_str) == Some(cs)
            });
        }
        let r_detected = detected.len() as i64;

        for f in &detected {
            let confidence = f.get("confidence").and_then(Value::as_f64).unwrap_or(1.0);
            let fix_options: Option<Vec<Value>> = f.get("fix_options").and_then(Value::as_array).cloned();
            let f_kind = f.get("kind").and_then(Value::as_str);

            let mut tagged = f.as_object().cloned().unwrap_or_default();
            tagged.insert("reconciler_id".into(), json!(id));
            tagged.insert("reconciler_safety".into(), json!(r_safety));
            tagged.insert("confidence".into(), json!(confidence));
            tagged.insert(
                "fix_options".into(),
                match &fix_options {
                    Some(opts) => json!(opts
                        .iter()
                        .map(|o| json!({ "key": o.get("key").cloned().unwrap_or(Value::Null), "label": o.get("label").cloned().unwrap_or(Value::Null) }))
                        .collect::<Vec<_>>()),
                    None => Value::Null,
                },
            );
            let tagged_val = Value::Object(tagged);
            findings.push(tagged_val.clone());

            let kind_matches = finding_kind.is_none() || finding_kind == f_kind;
            let operator_picked_this_option = option_key.is_some() && fix_options.is_some() && kind_matches;
            let safety_match = operator_picked_this_option
                || match safety {
                    "all" => r_safety != "manual",
                    "risky" => safety_rank(r_safety) <= safety_rank("risky"),
                    _ => r_safety == "safe",
                };
            if dry_run || !safety_match {
                r_skipped += 1;
                continue;
            }
            let needs_option_pick = confidence < 0.8 && fix_options.is_some() && option_key.is_none() && kind_matches;
            if needs_option_pick {
                r_skipped += 1;
                continue;
            }

            let result: Value = if let (Some(ok), Some(opts)) = (option_key, &fix_options) {
                if kind_matches {
                    let chosen = opts.iter().find(|o| o.get("key").and_then(Value::as_str) == Some(ok));
                    match chosen {
                        None => json!({ "applied": false, "reason": format!("option key '{ok}' not in fix_options") }),
                        // `chosen.fix` is always a stringified source (never callable) in this
                        // crate's reconciler twins — see module doc. JS falls to `r.fix(...)`
                        // whenever `typeof chosen.fix !== 'function'`, so we always do too.
                        Some(_) => run_fix(id, &tagged_val, db),
                    }
                } else {
                    run_fix(id, &tagged_val, db)
                }
            } else {
                run_fix(id, &tagged_val, db)
            };

            let applied = truthy(result.get("applied"));
            fixes.push((id.to_string(), tagged_val, result));
            if applied {
                r_fixed += 1;
            } else {
                r_skipped += 1;
            }
        }
        by_reconciler.insert(
            id.to_string(),
            json!({ "detected": r_detected, "fixed": r_fixed, "skipped": r_skipped, "errored": r_errored }),
        );
    }

    let detected_count = findings.iter().filter(|f| f.get("kind").and_then(Value::as_str) != Some("reconciler-error")).count();
    let fixed_count = fixes.iter().filter(|(_, _, r)| truthy(r.get("applied"))).count();
    let skipped_count = fixes.iter().filter(|(_, _, r)| !truthy(r.get("applied"))).count() + if dry_run { findings.len() } else { 0 };

    let mut counts = Map::new();
    counts.insert("detected".into(), json!(detected_count as i64));
    counts.insert("fixed".into(), json!(fixed_count as i64));
    counts.insert("skipped".into(), json!(skipped_count as i64));
    counts.insert("errored".into(), json!(errored));
    counts.insert("by_reconciler".into(), Value::Object(by_reconciler.clone()));

    if !dry_run && fixed_count > 0 {
        let mut fixed_ids: Vec<String> = Vec::new();
        for (rid, _, r) in &fixes {
            if truthy(r.get("applied")) && !fixed_ids.contains(rid) {
                fixed_ids.push(rid.clone());
            }
        }
        let summary = fixed_ids
            .iter()
            .map(|id| {
                let n = by_reconciler.get(id).and_then(|c| c.get("fixed")).and_then(Value::as_i64).unwrap_or(0);
                format!("{id} ({n})")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let card_clause = card_slug.map(|cs| format!(" on card {cs}")).unwrap_or_default();

        let mut tags: Vec<String> = vec!["reconcile".to_string(), "substrate-drift".to_string()];
        tags.extend(fixed_ids.iter().map(|id| format!("reconciler:{id}")));
        let _ = crate::entities_memory_add::memoryAdd(
            &json!({
                "type": "lesson",
                "title": format!("reconcile: {fixed_count} drift fix(es) applied{card_clause}"),
                "summary": format!("system.reconcile.apply found and fixed: {summary}. Future drift in these patterns should be detected by the same reconcilers."),
                "tags": tags,
                "confidence": 0.9,
            }),
            db,
        );

        let bullets = fixes
            .iter()
            .filter(|(_, _, r)| truthy(r.get("applied")))
            .take(20)
            .map(|(rid, f, _)| format!("- **{rid}** — {}", f.get("message").and_then(Value::as_str).unwrap_or_default()))
            .collect::<Vec<_>>()
            .join("\n");
        let _ = crate::entities_inbox_message_add::inboxMessageAdd(
            &json!({
                "kind": "reconcile-summary",
                "subject": format!("Reconciled {fixed_count} drift item(s){card_clause}"),
                "body_md": format!(
                    "## Summary\n{fixed_count} drift item(s) auto-fixed across {} reconciler(s).\n\n## What was fixed\n{bullets}\n\n## What now\n- The substrate is back to a consistent state.\n- If the same drift returns, the reconciler pattern is sound — the bug producing it may need a deeper fix.",
                    fixed_ids.len()
                ),
                "severity": "info",
                "related_card_slug": card_slug,
                "payload_json": { "fixed_ids": fixed_ids, "counts": Value::Object(by_reconciler.clone()) },
            }),
            db,
        );

        for id in &fixed_ids {
            let sample = fixes.iter().find(|(rid, _, r)| rid == id && truthy(r.get("applied")));
            let Some((_, sample_finding, _)) = sample else { continue };
            let card_slug_new = format!("PREVENT-{}", id.to_uppercase());
            let custom_note = custom_guidance
                .map(|g| format!("\n\nOperator custom guidance: {g}"))
                .unwrap_or_default();
            let fixed_n = by_reconciler.get(id).and_then(|c| c.get("fixed")).and_then(Value::as_i64).unwrap_or(0);
            let diagnosis: String = sample_finding
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(300)
                .collect();

            let body_res = crate::system_card_body_generate::cardBodyGenerate(
                &json!({
                    "kind": "prevent",
                    "slug": card_slug_new,
                    "why": format!("Reconciler '{id}' fixed {fixed_n} finding(s){card_clause}. Each fix is a signal that a deeper substrate change could prevent this drift class from arising at all."),
                    "diagnosis": diagnosis,
                    "notes": format!(
                        "Investigate why {id} keeps firing. Options:\n- Fix at the source (the skill/action that creates the drift)\n- Add a precondition or validation that catches the bad state at write time\n- Strengthen the cap/threshold that controls when the drift accumulates\n\nOperator: accept (start work), reject (this drift is acceptable / handled by reconciler), or edit this plan.{custom_note}"
                    ),
                }),
                db,
            );
            if let Ok(body_wrap) = body_res {
                if let Some(body) = body_wrap.get("body").cloned() {
                    let _ = crate::entities_card_add::cardAdd(
                        &json!({
                            "slug": card_slug_new,
                            "title": format!("Prevent recurrence: {id}"),
                            "status": "backlog",
                            "body": body,
                        }),
                        db,
                    );
                }
            }
        }
    }

    Ok(json!({
        "ran": selected.iter().map(|(id, _, _)| json!(*id)).collect::<Vec<_>>(),
        "findings": findings,
        "fixes": fixes.iter().map(|(rid, f, r)| json!({ "reconciler_id": rid, "finding": f, "result": r })).collect::<Vec<_>>(),
        "counts": Value::Object(counts),
        "dry_run": dry_run,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_schema() -> &'static str {
        "CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT UNIQUE, status TEXT, priority INTEGER, updated_at TEXT, driver_intent TEXT, title TEXT, body_json TEXT, parent_slug TEXT);
         CREATE TABLE notifications (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, kind TEXT, source TEXT, message TEXT, severity TEXT, dismissed_at TEXT);
         CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT, status TEXT, tier TEXT, risk INTEGER, complexity TEXT, sandbox_path TEXT, files_touched_json TEXT, superseded_by TEXT, updated_at TEXT, current_attempt INTEGER, dev_microtask_progress INTEGER, microtasks_json TEXT);
         CREATE TABLE inbox_messages (id INTEGER PRIMARY KEY, ts TEXT, kind TEXT, subject TEXT, body_md TEXT, severity TEXT, related_card_slug TEXT, related_contract_slug TEXT, suggested_reconciler TEXT, suggested_card_action TEXT, payload_json TEXT);
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
           source_event    TEXT,
           source_session  TEXT,
           confidence      REAL,
           created_by      TEXT
         );"
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH DATABASE ':memory:' AS memory; ATTACH DATABASE ':memory:' AS brain;").unwrap();
        c.execute_batch(full_schema()).unwrap();
        c
    }

    #[test]
    fn invalid_only_safety_is_rejected() {
        let c = db();
        let e = reconcileApply(&json!({ "only_safety": "yolo" }), &c).unwrap_err();
        assert_eq!(e, "reconcile-apply: only_safety must be one of safe/risky/all (got 'yolo')");
    }

    #[test]
    fn dry_run_defaults_true_and_never_mutates() {
        let c = db();
        c.execute(
            "INSERT INTO notifications (ts, kind, source, message, severity) VALUES ('2025-01-01 00:00:00', 'x', 'y', 'stuck red', 'error')",
            [],
        )
        .unwrap();
        let r = reconcileApply(&json!({}), &c).unwrap();
        assert_eq!(r["dry_run"], json!(true));
        assert_eq!(r["counts"]["fixed"], json!(0));
        let still_undismissed: i64 = c
            .query_row("SELECT COUNT(*) FROM notifications WHERE dismissed_at IS NULL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(still_undismissed, 1);
    }

    #[test]
    fn safe_reconciler_fixes_when_dry_run_false_and_safety_safe() {
        let c = db();
        c.execute(
            "INSERT INTO notifications (ts, kind, source, message, severity) VALUES ('2025-01-01 00:00:00', 'x', 'y', 'stuck red', 'error')",
            [],
        )
        .unwrap();
        let r = reconcileApply(&json!({ "names": ["stuck-red-notifs"], "dry_run": false, "only_safety": "safe" }), &c).unwrap();
        assert_eq!(r["dry_run"], json!(false));
        assert!(r["counts"]["fixed"].as_i64().unwrap() >= 1);
        let ran = r["ran"].as_array().unwrap();
        assert_eq!(ran, &vec![json!("stuck-red-notifs")]);
    }

    #[test]
    fn names_filter_restricts_to_the_named_subset() {
        let c = db();
        let r = reconcileApply(&json!({ "names": ["stuck-red-notifs", "orphan-sandbox-dirs"] }), &c).unwrap();
        let ran: Vec<String> = r["ran"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert_eq!(ran, vec!["orphan-sandbox-dirs", "stuck-red-notifs"]);
    }

    #[test]
    fn card_slug_excludes_every_scope_global_reconciler() {
        let c = db();
        let r = reconcileApply(&json!({ "names": ["stuck-red-notifs"], "card_slug": "FEAT-X" }), &c).unwrap();
        assert_eq!(r["ran"], json!([]));
    }

    #[test]
    fn card_slug_does_not_exclude_a_no_scope_reconciler() {
        // driver-ticks-bloat has NO `scope` key in the JS — it is never excluded by card_slug,
        // matching the real `r.scope === 'global'` filter (not the doc's claimed default).
        let c = db();
        let r = reconcileApply(&json!({ "names": ["driver-ticks-bloat"], "card_slug": "FEAT-X" }), &c).unwrap();
        assert_eq!(r["ran"], json!(["driver-ticks-bloat"]));
    }

    #[test]
    fn unknown_option_key_is_reported_as_not_in_fix_options() {
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, status, superseded_by, updated_at) VALUES ('CT-1', 'CARD-A', 'proposed', '', '2025-12-20 00:00:00')",
            [],
        )
        .unwrap();
        let r = reconcileApply(
            &json!({ "names": ["stuck-contracts"], "dry_run": false, "option_key": "Z", "finding_kind": "stuck-contract" }),
            &c,
        )
        .unwrap();
        let fixes = r["fixes"].as_array().unwrap();
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0]["result"], json!({ "applied": false, "reason": "option key 'Z' not in fix_options" }));
    }

    #[test]
    fn known_option_key_falls_back_to_the_reconcilers_own_fix_per_parity_deferred_boundary() {
        // See module doc: chosen.fix is always a string in this crate, never callable, so the JS
        // `else` branch (r.fix(...)) always runs — for stuck-contracts that's the manual refusal.
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, status, superseded_by, updated_at) VALUES ('CT-1', 'CARD-A', 'proposed', '', '2025-12-20 00:00:00')",
            [],
        )
        .unwrap();
        let r = reconcileApply(
            &json!({ "names": ["stuck-contracts"], "dry_run": false, "option_key": "A", "finding_kind": "stuck-contract" }),
            &c,
        )
        .unwrap();
        let fixes = r["fixes"].as_array().unwrap();
        assert_eq!(fixes.len(), 1);
        assert_eq!(fixes[0]["result"], json!({ "applied": false, "reason": "manual — operator must pick option A/B/C/D" }));
    }

    #[test]
    fn low_confidence_finding_without_option_key_is_skipped_not_auto_applied() {
        let c = db();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, status, superseded_by, updated_at) VALUES ('CT-1', 'CARD-A', 'proposed', '', '2025-12-20 00:00:00')",
            [],
        )
        .unwrap();
        // safety='all' never matches a manual reconciler ('all' means r.safety !== 'manual'), so
        // this finding is skipped at the safety gate itself — the low-confidence/fix_options/
        // no-option_key check below it is never reached. Verified against real JS
        // (reconcile-apply.mjs): top-level counts.skipped only counts fixes[] entries (+ dry_run
        // findings), and this finding never gets pushed to fixes — it's only reflected in
        // by_reconciler.skipped. counts.skipped stays 0 here, not 1.
        let r = reconcileApply(&json!({ "names": ["stuck-contracts"], "dry_run": false, "only_safety": "all" }), &c).unwrap();
        assert_eq!(r["fixes"], json!([]));
        assert_eq!(r["counts"]["skipped"], json!(0));
        assert_eq!(r["counts"]["by_reconciler"]["stuck-contracts"]["skipped"], json!(1));
    }

    #[test]
    fn manual_reconciler_never_auto_fixes_at_default_safety() {
        let c = db();
        c.execute(
            "INSERT INTO notifications (ts, message, severity) VALUES ('2025-01-01 00:00:00', 'boom', 'warn')",
            [],
        )
        .unwrap();
        let r = reconcileApply(&json!({ "names": ["pending-warn-notifications"], "dry_run": false }), &c).unwrap();
        assert_eq!(r["counts"]["fixed"], json!(0));
    }

    #[test]
    fn fixed_reconciler_files_a_prevent_recurrence_card_best_effort() {
        let c = db();
        c.execute(
            "INSERT INTO notifications (ts, kind, source, message, severity) VALUES ('2025-01-01 00:00:00', 'x', 'y', 'stuck red', 'error')",
            [],
        )
        .unwrap();
        let _ = reconcileApply(&json!({ "names": ["stuck-red-notifs"], "dry_run": false }), &c).unwrap();
        let exists: i64 = c
            .query_row("SELECT COUNT(*) FROM cards WHERE slug = 'PREVENT-STUCK-RED-NOTIFS'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(exists, 1);
    }
}
