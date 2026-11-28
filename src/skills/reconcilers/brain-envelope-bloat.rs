//! brain-envelope-bloat.rs — twin of skills/reconcilers/brain-envelope-bloat.mjs (reconciler-object
//! lane: the METHODS are the units, both db-last). NAMES ARE CONTRACT: `detect` (detect(db)) and
//! `fix` (fix(finding, db)). Consolidate-then-prune brain.ai_calls.envelope_json: `detect` surfaces a
//! Fixit finding once >= MIN_REPORT_BYTES (16 MiB) of raw envelope is reclaimable from calls older
//! than RETENTION_DAYS (30d); `fix` == `consolidateThenPrune` — calls the already-converted
//! `system_brain_consolidate::brainConsolidate` FIRST (so a digest always exists before any envelope
//! dies), then nulls envelope_json for rows at-or-before the digested window's period_end. If
//! brainConsolidate finds nothing NEW to digest, `fix` falls back to a prior consolidation's
//! period_end (covering an interrupted prune) rather than being a no-op — matching the JS
//! `if (!c.consolidated)` branch verbatim. OPERATOR-GATED + NON-AUTO (confidence 0.6 + fix_options;
//! there is no code path that prunes an un-digested window).
//!
//! datetime-injection lane: the single `datetime('now', ...)` call site in `detect` is written with
//! the frozen literal `datetime('2026-01-01 00:00:00', '-30 days')` in place of `'now'` — RETENTION_DAYS
//! is baked into the SQL text at build time (a JS template-literal interpolation, not a bound param),
//! matching the JS source verbatim. The `datetime('now')` inside `consolidateThenPrune`'s
//! `UPDATE ... SET pruned_at = ...` is likewise pinned to the frozen literal.
//!
//! cross-db lane: `brain.ai_calls` / `memory.memory_consolidation` are qualified SQL against the
//! attached `brain`/`memory` dbs, transcribed verbatim. Fixtures declare `"attach": ["brain"]`
//! (detect) / `["brain", "memory"]` (fix).
//!
//! MIN_REPORT_BYTES (16 MiB) is a plain module const (not env-controlled), so the "finding surfaces"
//! branch of `detect` is not fixture-reachable without embedding a multi-megabyte string in a fixture
//! — ~60x the largest fixture in this repo (same call the sibling MIN_REPORT_ROWS made in
//! brain-decisions-bloat). That branch's exact shape (message wording, `toFixed(1)` MB rounding,
//! stringified fix_options) is instead proven by a direct unit test on `finding_for` fed fabricated
//! n/bytes, ground-truthed by running the real JS module (see `finding_for_matches_the_js_shape_*`
//! below). The db-lane `detect` fixture covers both REACHABLE branches: no rows, and rows present but
//! under threshold (also exercising the `envelope_json IS NOT NULL` / `ts < cutoff` filters together).
//!
//! fix_options[].fix are JS FUNCTIONS — the conform JS lane's normalizeResult stringifies them via
//! String(v) (Function.prototype.toString, the exact source slice, CRLF line endings included since
//! that's how the JS file is saved). Option A is the named `consolidateThenPrune` declaration; option
//! B is a single-line async arrow. Both captured verbatim by evaluating the live JS module and
//! printing String(fn), never retyped by hand.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

const RETENTION_DAYS: i64 = 30;
const MIN_REPORT_BYTES: i64 = 16 * 1024 * 1024;

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

// JS `(bytes / 1024 / 1024).toFixed(1)` — bytes is always >= 0 here (COALESCE(SUM(LENGTH(...)),0)),
// so half-away-from-zero rounding at 1 decimal matches toFixed's half-up tie-break.
fn to_fixed1(x: f64) -> String {
    let scaled = (x * 10.0).round() as i64;
    format!("{}.{}", scaled / 10, scaled % 10)
}

// The JS fix_options[].fix values are FUNCTIONS; normalizeResult stringifies them via String(v)
// (Function.prototype.toString — the exact source slice, CRLF line endings as saved on disk).
// Captured verbatim by evaluating the live JS module, never retyped by hand.
const FIX_A_SRC: &str = "async function consolidateThenPrune(_finding, db) {\r\n  // 1. Consolidate the window FIRST — the digest must exist before any\r\n  //    envelope inside it is destroyed.\r\n  const c = brainConsolidate({ older_than_days: RETENTION_DAYS }, { db });\r\n  if (!c.consolidated) {\r\n    // Nothing new to digest can still mean prunable envelopes from an\r\n    // ALREADY-consolidated window (e.g. a prior prune was interrupted).\r\n    const covered = db.prepare(`\r\n      SELECT MAX(period_end) AS pe FROM memory.memory_consolidation WHERE source = 'ai_calls'\r\n    `).get();\r\n    if (!covered?.pe) return { applied: false, reason: 'no consolidation exists and no rows to consolidate — nothing pruned' };\r\n    const r = db.prepare(`\r\n      UPDATE brain.ai_calls SET envelope_json = NULL\r\n      WHERE envelope_json IS NOT NULL AND ts <= ?\r\n    `).run(covered.pe);\r\n    return {\r\n      applied: r.changes > 0,\r\n      changes: { table: 'brain.ai_calls', rows_affected: r.changes, consolidation: 'pre-existing', covered_until: covered.pe },\r\n      reason: r.changes === 0 ? 'window already consolidated and already pruned' : undefined,\r\n    };\r\n  }\r\n  // 2. Prune ONLY the rows the fresh consolidation covers — never beyond\r\n  //    period_end, so an envelope can never die un-digested.\r\n  const r = db.prepare(`\r\n    UPDATE brain.ai_calls SET envelope_json = NULL\r\n    WHERE envelope_json IS NOT NULL AND ts <= ?\r\n  `).run(c.period_end);\r\n  db.prepare(`UPDATE memory.memory_consolidation SET pruned_at = datetime('now') WHERE id = ?`)\r\n    .run(c.consolidation_id);\r\n  return {\r\n    applied: true,\r\n    changes: {\r\n      table: 'brain.ai_calls', rows_affected: r.changes,\r\n      consolidation_id: c.consolidation_id,\r\n      period: `${c.period_start} → ${c.period_end}`,\r\n      calls_consolidated: c.calls_count,\r\n    },\r\n  };\r\n}";
const FIX_B_SRC: &str = "async () => ({ applied: false, reason: 'operator chose to keep the full envelope history — nothing consolidated, nothing pruned' })";

/// Builds the single Fixit finding `detect` returns once bytes >= MIN_REPORT_BYTES. Split out so its
/// exact shape can be unit-tested directly without embedding a multi-megabyte envelope in a fixture.
fn finding_for(n: i64, bytes: i64) -> Value {
    let mb = to_fixed1(bytes as f64 / 1024.0 / 1024.0);
    json!({
        "kind": "brain-envelope-bloat",
        "severity": "cleanup",
        "message": format!(
            "{n} ai_calls older than {RETENTION_DAYS}d still carry raw envelopes — {mb} MB reclaimable after consolidation (digest is written to memory_consolidation first; rows + cost metadata always kept)"
        ),
        "detail": { "stale_rows": n, "reclaimable_bytes": bytes, "retention_days": RETENTION_DAYS },
        "suggested_action": "choose: consolidate + prune, or skip and keep the full envelope history",
        "confidence": 0.6,
        "fix_options": [
            {
                "key": "A",
                "label": format!("Consolidate into memory_consolidation, then prune envelopes (reclaims ~{mb} MB after offline VACUUM)"),
                "fix": FIX_A_SRC,
            },
            {
                "key": "B",
                "label": "Skip — keep the full envelope history for now",
                "fix": FIX_B_SRC,
            },
        ],
    })
}

/// JS: `async detect(db)` — the try/catch around the query maps a missing `brain.ai_calls` table (or
/// an old-substrate column absence) to `row = null`, exactly like the JS
/// `catch { /* brain db / column absent on old substrates */ }`. Below MIN_REPORT_BYTES (or table
/// absent, or zero rows) → `[]`.
pub fn detect(db: &Connection) -> Value {
    let sql = format!(
        "SELECT COUNT(*) AS n, COALESCE(SUM(LENGTH(envelope_json)), 0) AS bytes \
         FROM brain.ai_calls \
         WHERE envelope_json IS NOT NULL AND ts < datetime({NOW}, '-{RETENTION_DAYS} days')",
        NOW = crate::clock::now_sql_inner()
    );
    let row = crate::db_json::query_rows(db, &sql, &[])
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()));

    let n = match row.as_ref().and_then(|r| r.get("n")).and_then(|v| v.as_i64()) {
        Some(v) => v,
        None => return json!([]), // brain.ai_calls / envelope_json absent (JS catch)
    };
    if n == 0 {
        return json!([]);
    }
    let bytes = row.as_ref().and_then(|r| r.get("bytes")).and_then(|v| v.as_i64()).unwrap_or(0);
    if bytes < MIN_REPORT_BYTES {
        return json!([]);
    }
    json!([finding_for(n, bytes)])
}

/// JS: `async function consolidateThenPrune(_finding, db)`. Consolidates the un-digested window via
/// the already-converted `brainConsolidate` FIRST; if there's nothing NEW to digest, falls back to
/// the prior consolidation's period_end (covering an interrupted prune) instead of being a no-op.
fn consolidate_then_prune(_finding: &Value, db: &Connection) -> Value {
    let c = crate::system_brain_consolidate::brainConsolidate(&json!({ "older_than_days": RETENTION_DAYS }), db)
        .unwrap_or_else(|_| json!({ "consolidated": false }));

    if !c.get("consolidated").and_then(|v| v.as_bool()).unwrap_or(false) {
        let covered_pe = crate::db_json::query_rows(
            db,
            "SELECT MAX(period_end) AS pe FROM memory.memory_consolidation WHERE source = 'ai_calls'",
            &[],
        )
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
        .and_then(|r| r.get("pe").cloned())
        .unwrap_or(Value::Null);

        if covered_pe.is_null() {
            return json!({ "applied": false, "reason": "no consolidation exists and no rows to consolidate — nothing pruned" });
        }

        let (changes, _) = crate::db_json::run_write(
            db,
            "UPDATE brain.ai_calls SET envelope_json = NULL WHERE envelope_json IS NOT NULL AND ts <= ?",
            &[covered_pe.clone()],
        )
        .unwrap_or((0, 0));

        let mut changes_obj = Map::new();
        changes_obj.insert("table".into(), json!("brain.ai_calls"));
        changes_obj.insert("rows_affected".into(), json!(changes as i64));
        changes_obj.insert("consolidation".into(), json!("pre-existing"));
        changes_obj.insert("covered_until".into(), covered_pe);

        let mut out = Map::new();
        out.insert("applied".into(), json!(changes > 0));
        out.insert("changes".into(), Value::Object(changes_obj));
        if changes == 0 {
            out.insert("reason".into(), json!("window already consolidated and already pruned"));
        }
        return Value::Object(out);
    }

    let period_end = c.get("period_end").cloned().unwrap_or(Value::Null);
    let (changes, _) = crate::db_json::run_write(
        db,
        "UPDATE brain.ai_calls SET envelope_json = NULL WHERE envelope_json IS NOT NULL AND ts <= ?",
        &[period_end.clone()],
    )
    .unwrap_or((0, 0));

    let consolidation_id = c.get("consolidation_id").cloned().unwrap_or(Value::Null);
    let _ = crate::db_json::run_write(
        db,
        &format!("UPDATE memory.memory_consolidation SET pruned_at = {} WHERE id = ?", crate::clock::now_sql()),
        &[consolidation_id.clone()],
    );

    let period_start = c.get("period_start").cloned().unwrap_or(Value::Null);
    let calls_count = c.get("calls_count").cloned().unwrap_or(Value::Null);

    let mut changes_obj = Map::new();
    changes_obj.insert("table".into(), json!("brain.ai_calls"));
    changes_obj.insert("rows_affected".into(), json!(changes as i64));
    changes_obj.insert("consolidation_id".into(), consolidation_id);
    changes_obj.insert(
        "period".into(),
        json!(format!("{} → {}", tmpl(Some(&period_start)), tmpl(Some(&period_end)))),
    );
    changes_obj.insert("calls_consolidated".into(), calls_count);

    let mut out = Map::new();
    out.insert("applied".into(), json!(true));
    out.insert("changes".into(), Value::Object(changes_obj));
    Value::Object(out)
}

/// JS: `async fix(finding, db) { return consolidateThenPrune(finding, db); }` — option A's real
/// behavior (there is no path that prunes an un-digested window).
pub fn fix(finding: &Value, db: &Connection) -> Value {
    consolidate_then_prune(finding, db)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_calls_only() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, envelope_json TEXT);",
        )
        .unwrap();
        c
    }

    fn insert_call(c: &Connection, ts: &str, envelope_json: Option<&str>) {
        c.execute(
            "INSERT INTO brain.ai_calls (ts, envelope_json) VALUES (?1, ?2)",
            rusqlite::params![ts, envelope_json],
        )
        .unwrap();
    }

    fn db_full() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; ATTACH ':memory:' AS memory; \
             CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT); \
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, prompt_template_id INTEGER, tier TEXT, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER, outcome TEXT, cached INTEGER, duration_ms REAL, envelope_json TEXT); \
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, started_at TEXT, outcome TEXT, cost_usd REAL); \
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT NOT NULL, title TEXT NOT NULL, created_at TEXT NOT NULL); \
             CREATE TABLE memory.memory_consolidation (id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT NOT NULL DEFAULT 'ai_calls', period_start TEXT NOT NULL, period_end TEXT NOT NULL, calls_count INTEGER NOT NULL, cost_usd_total REAL, tokens_in_total INTEGER, tokens_out_total INTEGER, overview_md TEXT NOT NULL, analysis_md TEXT, keypoints_json TEXT, learnings_md TEXT, fails_md TEXT, journey_md TEXT, meta_json TEXT, pruned_at TEXT);",
        )
        .unwrap();
        c
    }

    fn insert_ai_call(c: &Connection, ts: &str, envelope_json: Option<&str>) {
        c.execute(
            "INSERT INTO brain.ai_calls (ts, prompt_template_id, tier, cost_usd_actual, tokens_in, tokens_out, outcome, cached, envelope_json) \
             VALUES (?1, NULL, 'mid', 0.01, 10, 5, 'success', 0, ?2)",
            rusqlite::params![ts, envelope_json],
        )
        .unwrap();
    }

    fn insert_consolidation(c: &Connection, period_start: &str, period_end: &str, calls_count: i64) {
        c.execute(
            "INSERT INTO memory.memory_consolidation (source, period_start, period_end, calls_count, overview_md) \
             VALUES ('ai_calls', ?1, ?2, ?3, 'prior digest')",
            rusqlite::params![period_start, period_end, calls_count],
        )
        .unwrap();
    }

    #[test]
    fn detect_stays_quiet_when_the_table_is_absent_empty_or_below_threshold() {
        let bare = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&bare), json!([])); // brain.ai_calls absent (no ATTACH even) → JS catch → []

        let empty = db_calls_only();
        assert_eq!(detect(&empty), json!([])); // table present, zero rows → n=0 → []

        // Frozen now = 2026-01-01 00:00:00; retention 30d → cutoff 2025-12-02 00:00:00.
        let c = db_calls_only();
        insert_call(&c, "2025-11-01 00:00:00", Some("{\"a\":1}")); // old + counted (7 bytes)
        insert_call(&c, "2025-11-15 00:00:00", None); // old but null envelope — excluded
        insert_call(&c, "2025-12-20 00:00:00", Some("{\"big\":\"payload-data-string\"}")); // too recent — excluded
        insert_call(&c, "2025-10-01 00:00:00", Some("{\"c\":2}")); // old + counted (7 bytes)
        // n=2, bytes=14 — far below MIN_REPORT_BYTES (16 MiB) → still [].
        assert_eq!(detect(&c), json!([]));
    }

    #[test]
    fn finding_for_matches_the_real_js_shape_and_embeds_the_stringified_fix_options() {
        // n/bytes are ground truth captured by running the real JS module with a stubbed db.
        let f = finding_for(42, 16850000);
        assert_eq!(f["kind"], json!("brain-envelope-bloat"));
        assert_eq!(f["severity"], json!("cleanup"));
        assert_eq!(
            f["message"],
            json!("42 ai_calls older than 30d still carry raw envelopes — 16.1 MB reclaimable after consolidation (digest is written to memory_consolidation first; rows + cost metadata always kept)")
        );
        assert_eq!(f["detail"], json!({ "stale_rows": 42, "reclaimable_bytes": 16850000, "retention_days": 30 }));
        assert_eq!(f["suggested_action"], json!("choose: consolidate + prune, or skip and keep the full envelope history"));
        assert_eq!(f["confidence"], json!(0.6));
        assert_eq!(
            f["fix_options"][0],
            json!({ "key": "A", "label": "Consolidate into memory_consolidation, then prune envelopes (reclaims ~16.1 MB after offline VACUUM)", "fix": FIX_A_SRC })
        );
        assert_eq!(
            f["fix_options"][1],
            json!({ "key": "B", "label": "Skip — keep the full envelope history for now", "fix": FIX_B_SRC })
        );

        // Boundary: bytes exactly == MIN_REPORT_BYTES (16 MiB) still surfaces (JS uses `<`, not `<=`).
        let boundary = finding_for(1, 16 * 1024 * 1024);
        assert_eq!(
            boundary["message"],
            json!("1 ai_calls older than 30d still carry raw envelopes — 16.0 MB reclaimable after consolidation (digest is written to memory_consolidation first; rows + cost metadata always kept)")
        );
    }

    #[test]
    fn fix_consolidates_fresh_window_then_prunes_only_that_window() {
        let c = db_full();
        insert_ai_call(&c, "2025-11-01 00:00:00", Some("{\"e\":1}"));
        insert_ai_call(&c, "2025-11-05 00:00:00", Some("{\"e\":2}"));
        insert_ai_call(&c, "2025-11-10 00:00:00", None); // counted in totals (no envelope filter there); extends period_end
        insert_ai_call(&c, "2025-12-20 00:00:00", Some("{\"fresh\":true}")); // too recent — outside the window entirely

        let r = fix(&json!({}), &c);
        assert_eq!(
            r,
            json!({
                "applied": true,
                "changes": {
                    "table": "brain.ai_calls",
                    "rows_affected": 2,
                    "consolidation_id": 1,
                    "period": "2025-11-01 00:00:00 → 2025-11-10 00:00:00",
                    "calls_consolidated": 3,
                },
            })
        );

        let remaining_envelopes: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.ai_calls WHERE envelope_json IS NOT NULL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining_envelopes, 1); // only the recent (out-of-window) row keeps its envelope

        let pruned_at: Option<String> = c
            .query_row("SELECT pruned_at FROM memory.memory_consolidation WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(pruned_at.as_deref(), Some("2026-01-01 00:00:00"));

        // Idempotent: the window is now fully consolidated+pruned, so a second fix falls into the
        // "no NEW window, but nothing left to prune either" branch.
        let r2 = fix(&json!({}), &c);
        assert_eq!(
            r2,
            json!({
                "applied": false,
                "changes": {
                    "table": "brain.ai_calls",
                    "rows_affected": 0,
                    "consolidation": "pre-existing",
                    "covered_until": "2025-11-10 00:00:00",
                },
                "reason": "window already consolidated and already pruned",
            })
        );
    }

    #[test]
    fn fix_reports_nothing_to_prune_when_no_consolidation_and_no_window() {
        let c = db_full();
        insert_ai_call(&c, "2025-12-20 00:00:00", Some("{\"fresh\":true}")); // too recent — no window at all
        let r = fix(&json!({}), &c);
        assert_eq!(
            r,
            json!({ "applied": false, "reason": "no consolidation exists and no rows to consolidate — nothing pruned" })
        );
    }

    #[test]
    fn fix_falls_back_to_prior_consolidation_and_prunes_the_leftover_envelope() {
        let c = db_full();
        insert_consolidation(&c, "2025-10-01 00:00:00", "2025-11-10 00:00:00", 2);
        // Inside the already-digested window, envelope not yet nulled (an interrupted prior prune).
        insert_ai_call(&c, "2025-11-05 00:00:00", Some("{\"leftover\":true}"));

        let r = fix(&json!({}), &c);
        assert_eq!(
            r,
            json!({
                "applied": true,
                "changes": {
                    "table": "brain.ai_calls",
                    "rows_affected": 1,
                    "consolidation": "pre-existing",
                    "covered_until": "2025-11-10 00:00:00",
                },
            })
        );
        let envelope: Option<String> = c.query_row("SELECT envelope_json FROM brain.ai_calls", [], |row| row.get(0)).unwrap();
        assert_eq!(envelope, None);
    }

    #[test]
    fn fix_is_a_true_noop_when_the_prior_window_is_already_fully_pruned() {
        let c = db_full();
        insert_consolidation(&c, "2025-10-01 00:00:00", "2025-11-10 00:00:00", 1);
        insert_ai_call(&c, "2025-11-05 00:00:00", None); // already pruned

        let r = fix(&json!({}), &c);
        assert_eq!(
            r,
            json!({
                "applied": false,
                "changes": {
                    "table": "brain.ai_calls",
                    "rows_affected": 0,
                    "consolidation": "pre-existing",
                    "covered_until": "2025-11-10 00:00:00",
                },
                "reason": "window already consolidated and already pruned",
            })
        );
    }

    #[test]
    fn to_fixed1_matches_js_tofixed() {
        assert_eq!(to_fixed1(20.0), "20.0");
        assert_eq!(to_fixed1(20.5), "20.5");
        assert_eq!(to_fixed1(16850000.0 / 1024.0 / 1024.0), "16.1");
        assert_eq!(to_fixed1(16777216.0 / 1024.0 / 1024.0), "16.0");
    }
}
