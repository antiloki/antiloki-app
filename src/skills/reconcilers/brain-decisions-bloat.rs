//! brain-decisions-bloat.rs — twin of skills/reconcilers/brain-decisions-bloat.mjs (reconciler-
//! object lane: the METHODS are the units, both db-last). NAMES ARE CONTRACT: `detect` (detect(db))
//! and `fix` (fix(finding, db)). Consolidate-then-prune the driver's rule_match heartbeat log:
//! digest per-day/per-card/per-rationale density into memory.memory_consolidation FIRST, then
//! delete only decision='rule_match' rows older than the 7-day retention window. Meaningful
//! decisions (schedule_and_dispatch, classify, …) are never touched. OPERATOR-GATED + NON-AUTO
//! (confidence 0.6 + fix_options) — `fix` always digests+prunes unconditionally, ignoring the
//! finding (there is no path that deletes un-digested rows).
//!
//! cross-db lane: `brain_decisions` is UNQUALIFIED in the JS source (never `brain.brain_decisions`)
//! — it resolves via SQLite's cross-attached-db name search (main → temp → each ATTACHed db in
//! attach order), since the table exists ONLY in the attached `brain` schema. Verified directly
//! against both node:sqlite and rusqlite (same underlying engine, same resolution). `fix`'s writes
//! into `memory.memory_consolidation` stay qualified, matching the JS source verbatim. Fixtures
//! declare `"attach": ["brain"]` (detect) / `["brain", "memory"]` (fix).
//!
//! datetime-injection lane: both `datetime('now', ...)` call sites are written with the frozen
//! literal `datetime('2026-01-01 00:00:00', ...)` in place of `'now'` — the `-7 days` modifier is a
//! literal baked into the query text (RETENTION_DAYS is a module const, not a bound param), same as
//! the JS template-literal interpolation.
//!
//! fix_options[].fix are JS FUNCTIONS — the conform JS lane's normalizeResult stringifies them via
//! String(v) (Function.prototype.toString, the exact source slice, CRLF line endings included since
//! that's how the JS file is saved). Option A is a NAMED function reference (digestAndPrune) —
//! captured verbatim (whitespace/comments/CRLF intact) by evaluating the live JS module and printing
//! String(fn), never retyped by hand. Option B is a single-line async arrow, captured the same way.
//!
//! MIN_REPORT_ROWS (50_000) is a plain module const, not env-controlled (unlike the sibling bloat
//! reconcilers' retentionDays()/minPruneRows()), so the "detect finds bloat" branch is technically
//! fixture-reachable — but doing so needs 50,001 seeded rows (a multi-MB fixture, ~30x the largest
//! fixture in this repo), which is disproportionate. Following the same call the sibling twins made
//! for their env-gated thresholds, that branch's SHAPE is instead proven directly by a unit test on
//! `finding_for` (fed fabricated n/bytes — no query involved, ground truth captured by running the
//! real JS module) plus the untouched JS behavior test (which already seeds 50,001 rows for real).
//! The db-lane `detect` fixture covers both REACHABLE branches: table absent, and n below threshold.
//! `fix`/`digestAndPrune` has NO row-count gate, so its db-lane fixture exercises the real digest
//! (small, hand-designed dataset — no tied GROUP BY counts, so the tie-break order some SQLite
//! builds might pick can never affect this comparison) alongside its no-op branch.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

const RETENTION_DAYS: i64 = 7;
const MIN_REPORT_ROWS: i64 = 50_000;

// JS row.n.toLocaleString() (en-US thousands grouping). Counts here are always >= 0, but the sign
// branch is kept for a faithful general Number.toLocaleString() mirror.
fn to_locale_string(n: i64) -> String {
    let neg = n < 0;
    let digits = n.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let grouped: String = grouped.chars().rev().collect();
    if neg {
        format!("-{grouped}")
    } else {
        grouped
    }
}

// JS (x).toFixed(0) — round-half-up. `bytes` (COALESCE(SUM(LENGTH(...)),0)) is always >= 0 here, so
// f64::round's half-away-from-zero tie-break agrees with toFixed's half-up tie-break.
fn to_fixed0(x: f64) -> String {
    format!("{}", x.round() as i64)
}

// The JS fix_options[].fix values are FUNCTIONS; normalizeResult stringifies them via String(v)
// (Function.prototype.toString — the exact source slice, CRLF line endings as saved on disk).
// Option A is the named `digestAndPrune` declaration; option B is a single-line async arrow. Both
// captured verbatim by evaluating the live JS module, never retyped by hand.
const FIX_A_SRC: &str = "function digestAndPrune(_finding, db) {\r\n  const win = db.prepare(`\r\n    SELECT COUNT(*) AS n, MIN(decided_at) AS a, MAX(decided_at) AS b\r\n    FROM brain_decisions\r\n    WHERE decision = 'rule_match'\r\n      AND decided_at < datetime('now', '-${RETENTION_DAYS} days')\r\n  `).get();\r\n  if (!win || win.n === 0) return { applied: false, reason: 'no rule_match rows past retention — nothing pruned' };\r\n\r\n  // Digest BEFORE deleting: density per day (meta) + per-card totals\r\n  // (journey) + rationale classes (analysis).\r\n  const perDay = db.prepare(`\r\n    SELECT date(decided_at) AS day, COUNT(*) AS n\r\n    FROM brain_decisions\r\n    WHERE decision = 'rule_match' AND decided_at >= ? AND decided_at <= ?\r\n    GROUP BY day ORDER BY day\r\n  `).all(win.a, win.b);\r\n  const perCard = db.prepare(`\r\n    SELECT COALESCE(card_slug, '(idle / no card)') AS card, COUNT(*) AS n\r\n    FROM brain_decisions\r\n    WHERE decision = 'rule_match' AND decided_at >= ? AND decided_at <= ?\r\n    GROUP BY card ORDER BY n DESC LIMIT 40\r\n  `).all(win.a, win.b);\r\n  const perRationale = db.prepare(`\r\n    SELECT substr(COALESCE(rationale, '(none)'), 1, 80) AS r, COUNT(*) AS n\r\n    FROM brain_decisions\r\n    WHERE decision = 'rule_match' AND decided_at >= ? AND decided_at <= ?\r\n    GROUP BY r ORDER BY n DESC LIMIT 25\r\n  `).all(win.a, win.b);\r\n  const kept = db.prepare(`\r\n    SELECT decision, COUNT(*) AS n FROM brain_decisions\r\n    WHERE decision != 'rule_match' GROUP BY decision ORDER BY n DESC\r\n  `).all();\r\n\r\n  const overview_md =\r\n    `## Driver heartbeat window ${win.a} → ${win.b}\\n\\n` +\r\n    `${win.n} rule_match decision rows digested (the pre-dormancy 2s polling loop wrote one per tick). ` +\r\n    `All ${kept.reduce((s, k) => s + k.n, 0)} meaningful decisions (${kept.map((k) => `${k.decision}:${k.n}`).join(', ')}) are KEPT in brain_decisions.`;\r\n  const analysis_md = `### Rationale classes (top)\\n` +\r\n    perRationale.map((r) => `- ${r.n} × \\`${r.r}\\``).join('\\n');\r\n  const journey_md = `### Heartbeats per card\\n` +\r\n    perCard.map((r) => `- ${r.card}: ${r.n}`).join('\\n');\r\n  const keypoints = perCard.slice(0, 15).map((r) => ({ kind: 'card-heartbeat', text: `${r.card}: ${r.n} ticks`, weight: r.n }));\r\n\r\n  const info = db.prepare(`\r\n    INSERT INTO memory.memory_consolidation\r\n      (source, period_start, period_end, calls_count, overview_md, analysis_md,\r\n       keypoints_json, journey_md, meta_json)\r\n    VALUES ('brain_decisions', ?, ?, ?, ?, ?, ?, ?, ?)\r\n  `).run(\r\n    win.a, win.b, win.n, overview_md, analysis_md,\r\n    JSON.stringify(keypoints), journey_md,\r\n    JSON.stringify({ retention_days: RETENTION_DAYS, per_day: perDay, kept_decisions: kept }),\r\n  );\r\n\r\n  // Prune exactly the digested window, rule_match only.\r\n  const r = db.prepare(`\r\n    DELETE FROM brain_decisions\r\n    WHERE decision = 'rule_match' AND decided_at >= ? AND decided_at <= ?\r\n  `).run(win.a, win.b);\r\n  db.prepare(`UPDATE memory.memory_consolidation SET pruned_at = datetime('now') WHERE id = ?`)\r\n    .run(info.lastInsertRowid);\r\n\r\n  return {\r\n    applied: true,\r\n    changes: {\r\n      table: 'brain_decisions', rows_affected: r.changes,\r\n      consolidation_id: Number(info.lastInsertRowid),\r\n      period: `${win.a} → ${win.b}`,\r\n    },\r\n  };\r\n}";
const FIX_B_SRC: &str = "async () => ({ applied: false, reason: 'operator chose to keep the full heartbeat log — nothing digested, nothing deleted' })";

/// Builds the single Fixit finding `detect` returns once n >= MIN_REPORT_ROWS. Split out so its
/// exact shape (message wording, mb rounding, stringified fix_options) can be unit-tested directly
/// without seeding 50,001 real rows through SQLite.
fn finding_for(n: i64, bytes: i64) -> Value {
    let mb = to_fixed0(bytes as f64 / 1024.0 / 1024.0);
    let n_str = to_locale_string(n);
    json!({
        "kind": "brain-decisions-bloat",
        "severity": "cleanup",
        "message": format!(
            "{n_str} rule_match heartbeat rows older than {RETENTION_DAYS}d (~{mb} MB) — digest per-day/per-card density into memory_consolidation, then delete. Meaningful decisions are never touched."
        ),
        "detail": { "stale_rows": n, "reclaimable_bytes": bytes, "retention_days": RETENTION_DAYS },
        "suggested_action": "choose: consolidate + delete heartbeat rows, or skip and keep everything",
        "confidence": 0.6,
        "fix_options": [
            {
                "key": "A",
                "label": format!("Digest into memory_consolidation, then delete {n_str} heartbeat rows (~{mb} MB; VACUUM offline afterwards)"),
                "fix": FIX_A_SRC,
            },
            {
                "key": "B",
                "label": "Skip — keep the full heartbeat log for now",
                "fix": FIX_B_SRC,
            },
        ],
    })
}

/// JS: `async detect(db)` — counts `brain_decisions` rows with decision='rule_match' older than the
/// retention window (+ their reclaimable byte total). The try/catch around the query maps a missing
/// `brain_decisions` table (old substrates) to `row = null`, exactly like the JS
/// `catch { /* table absent on old substrates */ }`. Below MIN_REPORT_ROWS (or table absent) → `[]`.
pub fn detect(db: &Connection) -> Value {
    let sql = format!(
        "\n        SELECT COUNT(*) AS n,\n               COALESCE(SUM(LENGTH(rationale) + LENGTH(COALESCE(inputs_json, ''))), 0) AS bytes\n        FROM brain_decisions\n        WHERE decision = 'rule_match'\n          AND decided_at < datetime({NOW}, '-{RETENTION_DAYS} days')\n      ",
        NOW = crate::clock::now_sql_inner()
    );
    let row = crate::db_json::query_rows(db, &sql, &[])
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()));

    let n = match row.as_ref().and_then(|r| r.get("n")).and_then(|v| v.as_i64()) {
        Some(v) => v,
        None => return json!([]), // brain_decisions absent (JS catch)
    };
    if n < MIN_REPORT_ROWS {
        return json!([]);
    }
    let bytes = row.as_ref().and_then(|r| r.get("bytes")).and_then(|v| v.as_i64()).unwrap_or(0);
    json!([finding_for(n, bytes)])
}

/// JS: `function digestAndPrune(_finding, db)`. Digests per-day/per-card/per-rationale density into
/// `memory.memory_consolidation` FIRST, then deletes exactly the digested `decision='rule_match'`
/// window. `_finding` is unused (matches the JS underscore-prefixed param — `fix` always digests the
/// live state, ignoring whichever finding triggered it).
fn digest_and_prune(_finding: &Value, db: &Connection) -> Value {
    let win_sql = format!(
        "\n    SELECT COUNT(*) AS n, MIN(decided_at) AS a, MAX(decided_at) AS b\n    FROM brain_decisions\n    WHERE decision = 'rule_match'\n      AND decided_at < datetime({NOW}, '-{RETENTION_DAYS} days')\n  ",
        NOW = crate::clock::now_sql_inner()
    );
    let win = crate::db_json::query_rows(db, &win_sql, &[])
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
        .unwrap_or(Value::Null);
    let win_n = win.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
    if win_n == 0 {
        return json!({ "applied": false, "reason": "no rule_match rows past retention — nothing pruned" });
    }
    let win_a = win.get("a").cloned().unwrap_or(Value::Null);
    let win_b = win.get("b").cloned().unwrap_or(Value::Null);
    let win_a_s = win_a.as_str().unwrap_or_default().to_string();
    let win_b_s = win_b.as_str().unwrap_or_default().to_string();

    let per_day = crate::db_json::query_rows(
        db,
        "\n    SELECT date(decided_at) AS day, COUNT(*) AS n\n    FROM brain_decisions\n    WHERE decision = 'rule_match' AND decided_at >= ? AND decided_at <= ?\n    GROUP BY day ORDER BY day\n  ",
        &[win_a.clone(), win_b.clone()],
    )
    .unwrap_or_else(|_| json!([]));
    let per_card = crate::db_json::query_rows(
        db,
        "\n    SELECT COALESCE(card_slug, '(idle / no card)') AS card, COUNT(*) AS n\n    FROM brain_decisions\n    WHERE decision = 'rule_match' AND decided_at >= ? AND decided_at <= ?\n    GROUP BY card ORDER BY n DESC LIMIT 40\n  ",
        &[win_a.clone(), win_b.clone()],
    )
    .unwrap_or_else(|_| json!([]));
    let per_rationale = crate::db_json::query_rows(
        db,
        "\n    SELECT substr(COALESCE(rationale, '(none)'), 1, 80) AS r, COUNT(*) AS n\n    FROM brain_decisions\n    WHERE decision = 'rule_match' AND decided_at >= ? AND decided_at <= ?\n    GROUP BY r ORDER BY n DESC LIMIT 25\n  ",
        &[win_a.clone(), win_b.clone()],
    )
    .unwrap_or_else(|_| json!([]));
    let kept = crate::db_json::query_rows(
        db,
        "\n    SELECT decision, COUNT(*) AS n FROM brain_decisions\n    WHERE decision != 'rule_match' GROUP BY decision ORDER BY n DESC\n  ",
        &[],
    )
    .unwrap_or_else(|_| json!([]));

    let kept_arr = kept.as_array().cloned().unwrap_or_default();
    let kept_total: i64 = kept_arr.iter().map(|k| k.get("n").and_then(|v| v.as_i64()).unwrap_or(0)).sum();
    let kept_list = kept_arr
        .iter()
        .map(|k| {
            format!(
                "{}:{}",
                k.get("decision").and_then(|v| v.as_str()).unwrap_or(""),
                k.get("n").and_then(|v| v.as_i64()).unwrap_or(0)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    let overview_md = format!(
        "## Driver heartbeat window {win_a_s} → {win_b_s}\n\n{win_n} rule_match decision rows digested (the pre-dormancy 2s polling loop wrote one per tick). All {kept_total} meaningful decisions ({kept_list}) are KEPT in brain_decisions."
    );

    let per_rationale_arr = per_rationale.as_array().cloned().unwrap_or_default();
    let analysis_md = format!(
        "### Rationale classes (top)\n{}",
        per_rationale_arr
            .iter()
            .map(|r| format!(
                "- {} × `{}`",
                r.get("n").and_then(|v| v.as_i64()).unwrap_or(0),
                r.get("r").and_then(|v| v.as_str()).unwrap_or("")
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let per_card_arr = per_card.as_array().cloned().unwrap_or_default();
    let journey_md = format!(
        "### Heartbeats per card\n{}",
        per_card_arr
            .iter()
            .map(|r| format!(
                "- {}: {}",
                r.get("card").and_then(|v| v.as_str()).unwrap_or(""),
                r.get("n").and_then(|v| v.as_i64()).unwrap_or(0)
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let keypoints: Vec<Value> = per_card_arr
        .iter()
        .take(15)
        .map(|r| {
            let card = r.get("card").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let n = r.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
            json!({ "kind": "card-heartbeat", "text": format!("{card}: {n} ticks"), "weight": n })
        })
        .collect();

    let mut meta = Map::new();
    meta.insert("retention_days".into(), json!(RETENTION_DAYS));
    meta.insert("per_day".into(), per_day.clone());
    meta.insert("kept_decisions".into(), kept.clone());

    let keypoints_json = serde_json::to_string(&Value::Array(keypoints)).unwrap_or_default();
    let meta_json = serde_json::to_string(&Value::Object(meta)).unwrap_or_default();

    let (_ins_changes, consolidation_id) = crate::db_json::run_write(
        db,
        "\n    INSERT INTO memory.memory_consolidation\n      (source, period_start, period_end, calls_count, overview_md, analysis_md,\n       keypoints_json, journey_md, meta_json)\n    VALUES ('brain_decisions', ?, ?, ?, ?, ?, ?, ?, ?)\n  ",
        &[
            win_a.clone(),
            win_b.clone(),
            json!(win_n),
            json!(overview_md),
            json!(analysis_md),
            json!(keypoints_json),
            json!(journey_md),
            json!(meta_json),
        ],
    )
    .unwrap_or((0, 0));

    let (del_changes, _) = crate::db_json::run_write(
        db,
        "\n    DELETE FROM brain_decisions\n    WHERE decision = 'rule_match' AND decided_at >= ? AND decided_at <= ?\n  ",
        &[win_a.clone(), win_b.clone()],
    )
    .unwrap_or((0, 0));

    let _ = crate::db_json::run_write(
        db,
        &format!("UPDATE memory.memory_consolidation SET pruned_at = {} WHERE id = ?", crate::clock::now_sql()),
        &[json!(consolidation_id)],
    );

    json!({
        "applied": true,
        "changes": {
            "table": "brain_decisions",
            "rows_affected": del_changes as i64,
            "consolidation_id": consolidation_id,
            "period": format!("{win_a_s} → {win_b_s}"),
        },
    })
}

/// JS: `async fix(finding, db) { return digestAndPrune(finding, db); }` — option A's real behavior
/// (there is no path that deletes un-digested rows).
pub fn fix(finding: &Value, db: &Connection) -> Value {
    digest_and_prune(finding, db)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; ATTACH ':memory:' AS memory; \
             CREATE TABLE brain.brain_decisions (id INTEGER PRIMARY KEY AUTOINCREMENT, decided_at TEXT, card_slug TEXT, decision TEXT, rationale TEXT, inputs_json TEXT); \
             CREATE TABLE memory.memory_consolidation (id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT, period_start TEXT, period_end TEXT, calls_count INTEGER, overview_md TEXT, analysis_md TEXT, keypoints_json TEXT, journey_md TEXT, meta_json TEXT, pruned_at TEXT);",
        )
        .unwrap();
        c
    }

    fn ins(c: &Connection, decided_at: &str, card_slug: Option<&str>, decision: &str, rationale: &str, inputs_json: Option<&str>) {
        c.execute(
            "INSERT INTO brain_decisions (decided_at, card_slug, decision, rationale, inputs_json) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![decided_at, card_slug, decision, rationale, inputs_json],
        )
        .unwrap();
    }

    #[test]
    fn absent_table_makes_detect_return_empty() {
        let bare = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&bare), json!([])); // brain_decisions absent → JS catch → row stays null → []
        assert_eq!(detect(&db()), json!([])); // present but empty → n=0 < threshold → []
    }

    #[test]
    fn detect_stays_quiet_under_the_min_report_rows_floor() {
        // Frozen now = 2026-01-01 00:00:00; retention 7d → cutoff 2025-12-25 00:00:00.
        let c = db();
        ins(&c, "2025-12-20 08:00:00", None, "rule_match", "r1", Some("{\"a\":1}"));
        ins(&c, "2025-12-20 09:00:00", None, "rule_match", "r2", None);
        ins(&c, "2025-12-21 08:00:00", None, "rule_match", "r3", Some("{}"));
        ins(&c, "2025-12-22 08:00:00", None, "rule_match", "r4", Some("{\"b\":2}"));
        ins(&c, "2025-12-22 09:00:00", None, "rule_match", "r5", Some("{}"));
        ins(&c, "2025-12-20 08:00:00", Some("CARD-A"), "classify", "x", Some("{}")); // not rule_match — excluded
        ins(&c, "2025-12-26 00:00:00", None, "rule_match", "recent", Some("{}")); // too recent — excluded
        // n = 5, far below MIN_REPORT_ROWS (50000) → still [].
        assert_eq!(detect(&c), json!([]));
    }

    #[test]
    fn finding_for_matches_the_real_js_shape_and_embeds_the_stringified_fix_options() {
        // n/bytes are ground truth captured by running the real JS module (seeded exactly like
        // brain-decisions-bloat.test.mjs's `seed()`: 50,001 rule_match rows).
        let f = finding_for(50001, 1_550_031);
        assert_eq!(f["kind"], json!("brain-decisions-bloat"));
        assert_eq!(f["severity"], json!("cleanup"));
        assert_eq!(
            f["message"],
            json!("50,001 rule_match heartbeat rows older than 7d (~1 MB) — digest per-day/per-card density into memory_consolidation, then delete. Meaningful decisions are never touched.")
        );
        assert_eq!(f["detail"], json!({ "stale_rows": 50001, "reclaimable_bytes": 1550031, "retention_days": 7 }));
        assert_eq!(f["suggested_action"], json!("choose: consolidate + delete heartbeat rows, or skip and keep everything"));
        assert_eq!(f["confidence"], json!(0.6));
        assert_eq!(f["fix_options"][0]["key"], json!("A"));
        assert_eq!(
            f["fix_options"][0]["label"],
            json!("Digest into memory_consolidation, then delete 50,001 heartbeat rows (~1 MB; VACUUM offline afterwards)")
        );
        assert_eq!(f["fix_options"][0]["fix"], json!(FIX_A_SRC));
        assert_eq!(f["fix_options"][1]["key"], json!("B"));
        assert_eq!(f["fix_options"][1]["label"], json!("Skip — keep the full heartbeat log for now"));
        assert_eq!(f["fix_options"][1]["fix"], json!(FIX_B_SRC));
    }

    #[test]
    fn fix_is_a_true_noop_when_nothing_is_past_retention() {
        let c = db();
        assert_eq!(
            fix(&json!({}), &c),
            json!({ "applied": false, "reason": "no rule_match rows past retention — nothing pruned" })
        );
        // Only recent rule_match + a meaningful decision — win.n stays 0.
        ins(&c, "2025-12-26 00:00:00", None, "rule_match", "recent", Some("{}"));
        ins(&c, "2025-12-20 08:00:00", Some("CARD-A"), "schedule_and_dispatch", "picked", Some("{}"));
        assert_eq!(
            fix(&json!({}), &c),
            json!({ "applied": false, "reason": "no rule_match rows past retention — nothing pruned" })
        );
        assert_eq!(c.query_row("SELECT COUNT(*) FROM memory.memory_consolidation", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }

    #[test]
    fn fix_digests_then_prunes_exactly_the_window_kept_decisions_and_recent_tail_survive() {
        let c = db();
        // Old rule_match window (decided_at < 2025-12-25 00:00:00) — digested + pruned.
        ins(&c, "2025-12-20 08:00:00", Some("CARD-A"), "rule_match", "no actionable card", Some("{\"idle\":true}"));
        ins(&c, "2025-12-20 09:00:00", Some("CARD-A"), "rule_match", "no actionable card", Some("{\"idle\":true}"));
        ins(&c, "2025-12-20 10:00:00", Some("CARD-A"), "rule_match", "no actionable card", Some("{\"idle\":true}"));
        ins(&c, "2025-12-21 08:00:00", Some("CARD-B"), "rule_match", "no actionable card", Some("{\"idle\":true}"));
        ins(&c, "2025-12-21 09:00:00", Some("CARD-B"), "rule_match", "different rationale text here", Some("{}"));
        ins(&c, "2025-12-22 08:00:00", None, "rule_match", "different rationale text here", Some("{}"));
        // Recent rule_match — survives (outside the digested window).
        ins(&c, "2025-12-26 00:00:00", None, "rule_match", "recent tick", Some("{}"));
        // Meaningful decisions — old AND new — always survive.
        ins(&c, "2025-12-20 08:00:00", Some("CARD-A"), "schedule_and_dispatch", "picked CARD-A", Some("{}"));
        ins(&c, "2025-12-26 00:00:00", None, "classify", "mutate", Some("{}"));
        ins(&c, "2025-12-27 00:00:00", None, "classify", "mutate again", Some("{}"));

        let r = fix(&json!({}), &c);
        assert_eq!(
            r,
            json!({
                "applied": true,
                "changes": {
                    "table": "brain_decisions",
                    "rows_affected": 6,
                    "consolidation_id": 1,
                    "period": "2025-12-20 08:00:00 → 2025-12-22 08:00:00",
                },
            })
        );

        let left: i64 = c.query_row("SELECT COUNT(*) FROM brain_decisions", [], |row| row.get(0)).unwrap();
        assert_eq!(left, 4); // the recent rule_match + the 3 meaningful decisions

        let (source, period_start, period_end, calls_count, overview_md, analysis_md, keypoints_json, journey_md, meta_json, pruned_at): (
            String, String, String, i64, String, String, String, String, String, Option<String>,
        ) = c
            .query_row(
                "SELECT source, period_start, period_end, calls_count, overview_md, analysis_md, keypoints_json, journey_md, meta_json, pruned_at FROM memory.memory_consolidation WHERE id = 1",
                [],
                |row| Ok((
                    row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
                    row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?,
                )),
            )
            .unwrap();
        assert_eq!(source, "brain_decisions");
        assert_eq!(period_start, "2025-12-20 08:00:00");
        assert_eq!(period_end, "2025-12-22 08:00:00");
        assert_eq!(calls_count, 6);
        assert_eq!(
            overview_md,
            "## Driver heartbeat window 2025-12-20 08:00:00 → 2025-12-22 08:00:00\n\n6 rule_match decision rows digested (the pre-dormancy 2s polling loop wrote one per tick). All 3 meaningful decisions (classify:2, schedule_and_dispatch:1) are KEPT in brain_decisions."
        );
        assert_eq!(
            analysis_md,
            "### Rationale classes (top)\n- 4 × `no actionable card`\n- 2 × `different rationale text here`"
        );
        assert_eq!(
            journey_md,
            "### Heartbeats per card\n- CARD-A: 3\n- CARD-B: 2\n- (idle / no card): 1"
        );
        assert_eq!(
            keypoints_json,
            "[{\"kind\":\"card-heartbeat\",\"text\":\"CARD-A: 3 ticks\",\"weight\":3},{\"kind\":\"card-heartbeat\",\"text\":\"CARD-B: 2 ticks\",\"weight\":2},{\"kind\":\"card-heartbeat\",\"text\":\"(idle / no card): 1 ticks\",\"weight\":1}]"
        );
        assert_eq!(
            meta_json,
            "{\"retention_days\":7,\"per_day\":[{\"day\":\"2025-12-20\",\"n\":3},{\"day\":\"2025-12-21\",\"n\":2},{\"day\":\"2025-12-22\",\"n\":1}],\"kept_decisions\":[{\"decision\":\"classify\",\"n\":2},{\"decision\":\"schedule_and_dispatch\",\"n\":1}]}"
        );
        assert_eq!(pruned_at.as_deref(), Some("2026-01-01 00:00:00"));

        // Idempotent: the window is gone, so a second fix is a true no-op.
        let r2 = fix(&json!({}), &c);
        assert_eq!(r2, json!({ "applied": false, "reason": "no rule_match rows past retention — nothing pruned" }));
    }

    #[test]
    fn locale_string_and_fixed0_match_js() {
        assert_eq!(to_locale_string(0), "0");
        assert_eq!(to_locale_string(999), "999");
        assert_eq!(to_locale_string(1000), "1,000");
        assert_eq!(to_locale_string(50001), "50,001");
        assert_eq!(to_locale_string(1234567), "1,234,567");
        assert_eq!(to_fixed0(0.0), "0");
        assert_eq!(to_fixed0(1_550_031.0 / 1024.0 / 1024.0), "1");
        assert_eq!(to_fixed0(0.5), "1"); // toFixed ties round up (bytes are always >= 0 here)
    }
}
