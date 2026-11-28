//! driver-ticks-bloat.rs — twin of skills/reconcilers/driver-ticks-bloat.mjs (reconciler-object
//! lane: the METHODS are the units, both db-last). NAMES ARE CONTRACT: `detect` (detect(db)) and
//! `fix` (fix(finding, db)). Prunes the append-only brain.driver_ticks decision log, excluding any
//! row belonging to a card still in an ACTIVE (non-terminal: backlog/proposed/doing) status
//! regardless of age — same OPERATOR-GATED + NON-AUTO stance as runtime-events-bloat (confidence
//! 0.6 + fix_options; `fix` always prunes unconditionally, ignoring the finding).
//!
//! datetime-injection lane: both `datetime('now', ?)` call sites are written with the frozen
//! literal `datetime('2026-01-01 00:00:00', ?)` in place of `'now'` — the modifier stays a bound
//! parameter (`-<days> days`), only the base instant is pinned.
//!
//! cross-db lane: brain.driver_ticks is qualified SQL against the attached `brain` db; the
//! ACTIVE_GUARD subquery reads `cards` (unqualified — the main/state db), transcribed verbatim.
//!
//! fix_options[].fix are JS FUNCTIONS — the conform JS lane's normalizeResult stringifies them via
//! String(v) (Function.prototype.toString, exact source slice), so the twin embeds the literal
//! source text for options A and B verbatim (captured by reading the JS module directly).
//!
//! retentionDays()/minPruneRows() read process.env per call — VERBATIM from the JS source they
//! reuse the SAME env var names as runtime-events-bloat (ANTILOKI_RUNTIME_EVENTS_RETENTION_DAYS /
//! ANTILOKI_RUNTIME_EVENTS_MIN_PRUNE), a copy-paste artifact in the JS, not a Rust-side choice. No
//! fixture hook exists to set env vars through the two-lane harness, so the "finding surfaces"
//! branch (which needs n >= minPruneRows()'s default of 50000) is not fixture-exercised — same
//! limitation as runtime-events-bloat's twin. Covered instead by the untouched JS behavior test
//! (driver-ticks-bloat.test.mjs, which overrides env per-test) and by the `finding_for` unit test
//! below, which builds the finding shape directly without touching env.

use rusqlite::Connection;
use serde_json::{json, Value};

// JS Number(v) for a string sourced from process.env (trim; empty → 0; else parseFloat-like).
fn env_num(name: &str, default: f64) -> f64 {
    match std::env::var(name) {
        Ok(s) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) }
        }
        Err(_) => default,
    }
}
// JS: `const retentionDays = () => Number(process.env.ANTILOKI_RUNTIME_EVENTS_RETENTION_DAYS ?? 30);`
fn retention_days() -> f64 { env_num("ANTILOKI_RUNTIME_EVENTS_RETENTION_DAYS", 30.0) }
// JS: `const minPruneRows = () => Number(process.env.ANTILOKI_RUNTIME_EVENTS_MIN_PRUNE ?? 50000);`
fn min_prune_rows() -> f64 { env_num("ANTILOKI_RUNTIME_EVENTS_MIN_PRUNE", 50000.0) }

// JS Number → String for the `${days}`/`${n}` interpolations (fixture/test values are always
// whole, matching the integer-rendering branch).
fn num_to_str(n: f64) -> String {
    if n.is_nan() { "NaN".into() }
    else if n.is_infinite() { if n > 0.0 { "Infinity".into() } else { "-Infinity".into() } }
    else if n == n.trunc() { format!("{}", n as i64) }
    else { format!("{}", n) }
}
fn num_to_json(n: f64) -> Value {
    if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) }
}

/// JS: `const ACTIVE_GUARD` — excludes any row belonging to a card still in an active
/// (non-terminal) status; transcribed verbatim.
const ACTIVE_GUARD: &str = "\n  AND (card_slug IS NULL OR card_slug NOT IN (\n    SELECT slug FROM cards WHERE status IN ('backlog', 'proposed', 'doing')\n  ))\n";

/// JS: `function cutoffTs(db, days)`.
fn cutoff_ts(db: &Connection, days: f64) -> String {
    let param = format!("-{} days", num_to_str(days));
    crate::db_json::query_rows(db, &format!("SELECT datetime({}, ?) AS c", crate::clock::now_sql_inner()), &[json!(param)])
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
        .and_then(|r| r.get("c").cloned())
        .and_then(|c| c.as_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// JS: `function prunableCount(db)` — the try/catch maps ANY prepare/query error (brain.driver_ticks
/// absent on a pre-driver substrate, or cards absent) to 0.
fn prunable_count(db: &Connection) -> f64 {
    let days = retention_days();
    let param = format!("-{} days", num_to_str(days));
    let sql = format!(
        "\n      SELECT COUNT(*) AS n FROM brain.driver_ticks\n      WHERE started_at < datetime({},  ?) {}\n    ",
        crate::clock::now_sql_inner(),
        ACTIVE_GUARD
    );
    match crate::db_json::query_rows(db, &sql, &[json!(param)]) {
        Ok(v) => v
            .as_array()
            .and_then(|a| a.first())
            .and_then(|r| r.get("n"))
            .and_then(|n| n.as_f64())
            .unwrap_or(0.0),
        Err(_) => 0.0,
    }
}

/// JS: `function prune(db)` — `applied` is a static `true` (unlike runtime-events-bloat's
/// `r.changes > 0`); `note` is always present (not conditional).
fn prune(db: &Connection) -> Value {
    let days = retention_days();
    let cutoff = cutoff_ts(db, days);
    let before = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM brain.driver_ticks", &[])
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
        .and_then(|r| r.get("n").cloned())
        .and_then(|n| n.as_f64())
        .unwrap_or(0.0);
    let sql = format!("\n    DELETE FROM brain.driver_ticks\n    WHERE started_at < ? {}\n  ", ACTIVE_GUARD);
    let changes = crate::db_json::run_write(db, &sql, &[json!(cutoff)])
        .map(|(c, _)| c)
        .unwrap_or(0) as f64;
    json!({
        "applied": true,
        "pruned_rows": changes as i64,
        "kept_rows": (before - changes) as i64,
        "retention_days": num_to_json(days),
        "note": "disk is freelisted immediately; the file shrinks after an offline VACUUM (BE stopped) — same as runtime-events-bloat",
    })
}

// The JS fix_options[].fix values are FUNCTIONS; normalizeResult stringifies them via String(v)
// (Function.prototype.toString — the exact source slice). Both are single-line arrows in the
// source, captured verbatim.
const FIX_A_SRC: &str = "(_f, db2) => prune(db2)";
const FIX_B_SRC: &str = "async () => ({ applied: false, reason: 'operator chose to keep the full tick log — nothing pruned' })";

/// Builds the single Fixit finding `detect` returns once n >= minPruneRows(). Split out from
/// `detect` so its exact shape can be unit-tested directly without needing to cross the
/// (env-controlled, default 50000) threshold through a live query.
fn finding_for(n: f64, days: f64) -> Value {
    let n_str = num_to_str(n);
    let days_str = num_to_str(days);
    json!({
        "kind": "driver-ticks-bloat",
        "severity": "cleanup",
        "message": format!(
            "{} driver_ticks rows are older than {}d and belong to no active card — decision telemetry that only grows brain.sqlite. Prune keeps the recent tail (ticks feed, retry counters, cost windows) and every active card's full history.",
            n_str, days_str
        ),
        "detail": { "prunable_rows": num_to_json(n), "retention_days": num_to_json(days) },
        "suggested_action": "choose: prune the old rows, or keep the full tick log",
        "confidence": 0.6,
        "fix_options": [
            {
                "key": "A",
                "label": format!("Prune the {} rows older than {}d (active cards untouched; VACUUM offline afterwards to reclaim disk)", n_str, days_str),
                "fix": FIX_A_SRC,
            },
            {
                "key": "B",
                "label": "Skip — keep the full driver_ticks history",
                "fix": FIX_B_SRC,
            },
        ],
    })
}

/// JS: `async detect(db)` — counts driver_ticks older than the retention window belonging to no
/// active card; stays quiet ([]) below minPruneRows(), otherwise a single finding via `finding_for`.
pub fn detect(db: &Connection) -> Value {
    let n = prunable_count(db);
    if n < min_prune_rows() {
        return json!([]);
    }
    let days = retention_days();
    json!([finding_for(n, days)])
}

/// JS: `async fix(_finding, db) { return prune(db); }` — always prunes unconditionally, ignoring
/// the finding.
pub fn fix(_finding: &Value, db: &Connection) -> Value {
    prune(db)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE cards (slug TEXT, status TEXT); \
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, card_slug TEXT, started_at TEXT);",
        )
        .unwrap();
        c
    }

    fn insert_card(c: &Connection, slug: &str, status: &str) {
        c.execute("INSERT INTO cards (slug, status) VALUES (?1, ?2)", rusqlite::params![slug, status]).unwrap();
    }

    fn insert_tick(c: &Connection, id: i64, card_slug: Option<&str>, started_at: &str) {
        c.execute(
            "INSERT INTO brain.driver_ticks (id, card_slug, started_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, card_slug, started_at],
        )
        .unwrap();
    }

    #[test]
    fn absent_tables_return_zero_and_empty() {
        let bare = Connection::open_in_memory().unwrap();
        assert_eq!(prunable_count(&bare), 0.0); // brain.driver_ticks / cards absent → JS catch → 0
        assert_eq!(detect(&bare), json!([]));
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn detect_stays_quiet_under_the_default_min_prune_threshold() {
        // Frozen now = 2026-01-01 00:00:00; default retention 30d → cutoff 2025-12-02 00:00:00.
        let c = db();
        insert_card(&c, "DTB-DONE", "done");
        for i in 0..5 {
            insert_tick(&c, i, Some("DTB-DONE"), "2025-10-01 00:00:00"); // old — counted
        }
        // n = 5, far below the default minPruneRows() of 50000 → still [].
        assert_eq!(detect(&c), json!([]));
    }

    #[test]
    fn finding_for_matches_the_js_shape_and_embeds_the_stringified_fix_options() {
        let f = finding_for(75000.0, 30.0);
        assert_eq!(f["kind"], json!("driver-ticks-bloat"));
        assert_eq!(f["severity"], json!("cleanup"));
        assert_eq!(
            f["message"],
            json!("75000 driver_ticks rows are older than 30d and belong to no active card — decision telemetry that only grows brain.sqlite. Prune keeps the recent tail (ticks feed, retry counters, cost windows) and every active card's full history.")
        );
        assert_eq!(f["detail"], json!({ "prunable_rows": 75000, "retention_days": 30 }));
        assert_eq!(f["suggested_action"], json!("choose: prune the old rows, or keep the full tick log"));
        assert_eq!(f["confidence"], json!(0.6));
        assert_eq!(
            f["fix_options"][0],
            json!({ "key": "A", "label": "Prune the 75000 rows older than 30d (active cards untouched; VACUUM offline afterwards to reclaim disk)", "fix": "(_f, db2) => prune(db2)" })
        );
        assert_eq!(
            f["fix_options"][1],
            json!({ "key": "B", "label": "Skip — keep the full driver_ticks history", "fix": "async () => ({ applied: false, reason: 'operator chose to keep the full tick log — nothing pruned' })" })
        );
    }

    #[test]
    fn fix_prunes_old_inactive_card_rows_keeps_active_cards_and_the_recent_tail() {
        let c = db();
        insert_card(&c, "DTB-ACTIVE", "doing");
        insert_card(&c, "DTB-CLOSED", "done");
        insert_tick(&c, 1, Some("DTB-ACTIVE"), "2025-09-01 00:00:00"); // old, ACTIVE card → kept
        insert_tick(&c, 2, Some("DTB-ACTIVE"), "2025-09-02 00:00:00"); // old, ACTIVE card → kept
        insert_tick(&c, 3, Some("DTB-CLOSED"), "2025-09-01 00:00:00"); // old, inactive → pruned
        insert_tick(&c, 4, Some("DTB-CLOSED"), "2025-09-02 00:00:00"); // old, inactive → pruned
        insert_tick(&c, 5, Some("DTB-CLOSED"), "2025-12-20 00:00:00"); // recent, inactive → kept (recent)
        insert_tick(&c, 6, None, "2025-09-01 00:00:00"); // old, no card → pruned (card_slug IS NULL passes the guard)
        insert_tick(&c, 7, None, "2025-12-25 00:00:00"); // recent, no card → kept (recent)

        let r = fix(&json!({}), &c);
        assert_eq!(
            r,
            json!({
                "applied": true,
                "pruned_rows": 3,
                "kept_rows": 4,
                "retention_days": 30,
                "note": "disk is freelisted immediately; the file shrinks after an offline VACUUM (BE stopped) — same as runtime-events-bloat",
            })
        );

        let mut stmt = c.prepare("SELECT id FROM brain.driver_ticks ORDER BY id").unwrap();
        let remaining: Vec<i64> = stmt.query_map([], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(remaining, vec![1, 2, 5, 7]);
    }

    #[test]
    fn fix_is_a_true_noop_when_nothing_is_old() {
        let c = db();
        insert_card(&c, "DTB-X", "backlog");
        insert_tick(&c, 1, Some("DTB-X"), "2025-12-20 00:00:00");
        insert_tick(&c, 2, Some("DTB-X"), "2025-12-25 00:00:00");
        let r = fix(&json!({}), &c);
        assert_eq!(
            r,
            json!({
                "applied": true,
                "pruned_rows": 0,
                "kept_rows": 2,
                "retention_days": 30,
                "note": "disk is freelisted immediately; the file shrinks after an offline VACUUM (BE stopped) — same as runtime-events-bloat",
            })
        );
        let count: i64 = c.query_row("SELECT COUNT(*) FROM brain.driver_ticks", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 2);
    }
}
