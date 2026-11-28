//! runtime-events-bloat.rs — twin of skills/reconcilers/runtime-events-bloat.mjs (reconciler-object
//! lane: the METHODS are the units, both db-last). NAMES ARE CONTRACT: `detect` (detect(db)) and
//! `fix` (fix(finding, db)). Plain retention prune of state.runtime_events rows older than a
//! retention window (default 30d), gated behind an operator A/B choice (confidence 0.6 +
//! fix_options — never auto-applied). `fix` always prunes unconditionally (both fix_options[A]'s
//! stringified body and the object's real `fix` method call the same `prune`).
//!
//! datetime-injection lane: both `datetime('now', ?)` call sites are written with the frozen
//! literal `datetime('2026-01-01 00:00:00', ?)` in place of `'now'` — the modifier stays a bound
//! parameter (`-<days> days`), only the base instant is pinned.
//!
//! fix_options[].fix are JS FUNCTIONS — the conform JS lane's normalizeResult stringifies them via
//! String(v) (Function.prototype.toString, exact source slice), so the twin embeds the literal
//! source text for options A and B verbatim (captured by evaluating the JS module directly).
//!
//! retentionDays()/minPruneRows() read process.env per call (no fixture hook exists to set env
//! vars through the two-lane harness, so the "finding surfaces" branch — which needs n >=
//! minPruneRows()'s default of 50000 — is not fixture-exercised, same limitation as the
//! table-absent catch branch on the stuck-contracts twin; both are covered by the untouched JS
//! behavior test (runtime-events-bloat.test.mjs, which overrides env per-test) and by the
//! `finding_for` unit test below, which builds the finding shape directly without touching env.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

// JS `${v}` for the row-count/day interpolations (already-known integer/number values).
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

// JS `${days}` where days is the Number returned by retentionDays()/etc. Fixture/test days are
// always whole (env unset → 30), so integer rendering (no decimal point) matches JS Number→String.
fn num_to_str(n: f64) -> String {
    if n.is_nan() { "NaN".into() }
    else if n.is_infinite() { if n > 0.0 { "Infinity".into() } else { "-Infinity".into() } }
    else if n == n.trunc() { format!("{}", n as i64) }
    else { format!("{}", n) }
}
fn num_to_json(n: f64) -> Value {
    if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) }
}

/// JS: `cutoffTs(db, days)` — `SELECT datetime('now', ?) AS c` computed once and reused by both
/// the sever UPDATE and the DELETE. This SQL has no FROM clause so it can never fail regardless of
/// whether runtime_events exists — the `.ok()` fallback below is defensive, not a real JS branch.
fn cutoff_ts(db: &Connection, days: f64) -> String {
    let param = format!("-{} days", num_to_str(days));
    crate::db_json::query_rows(db, &format!("SELECT datetime({}, ?) AS c", crate::clock::now_sql_inner()), &[json!(param)])
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
        .and_then(|r| r.get("c").cloned())
        .and_then(|c| c.as_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// JS: `function prune(db)` — sever any kept row's parent_event_id that points into the
/// about-to-be-pruned set (FK-safety), then delete rows older than the cutoff. `reason` is only
/// present when nothing was pruned (JS sets it to `undefined` otherwise, which JSON.stringify /
/// normalizeResult drop — so the key is simply omitted here, matching gotcha 3).
fn prune(db: &Connection) -> Value {
    let cutoff = cutoff_ts(db, retention_days());
    let _ = crate::db_json::run_write(
        db,
        "\n    UPDATE runtime_events SET parent_event_id = NULL\n    WHERE parent_event_id IS NOT NULL\n      AND ts >= ?\n      AND parent_event_id IN (SELECT id FROM runtime_events WHERE ts < ?)\n  ",
        &[json!(cutoff.clone()), json!(cutoff.clone())],
    );
    let changes = crate::db_json::run_write(db, "DELETE FROM runtime_events WHERE ts < ?", &[json!(cutoff.clone())])
        .map(|(c, _)| c)
        .unwrap_or(0);
    let mut out = Map::new();
    out.insert("applied".into(), json!(changes > 0));
    out.insert("changes".into(), json!({
        "table": "runtime_events",
        "rows_pruned": changes as i64,
        "older_than": cutoff,
    }));
    if changes == 0 {
        out.insert("reason".into(), json!("nothing older than the retention window"));
    }
    Value::Object(out)
}

// The JS fix_options[].fix values are FUNCTIONS; normalizeResult stringifies them via String(v)
// (Function.prototype.toString — the exact source slice). Both are single-line arrows in the
// source, captured verbatim.
const FIX_A_SRC: &str = "(_f, db2) => prune(db2)";
const FIX_B_SRC: &str = "async () => ({ applied: false, reason: 'operator chose to keep the full runtime log — nothing pruned' })";

/// Builds the single Fixit finding `detect` returns once n >= minPruneRows(). Split out from
/// `detect` so its exact shape (message wording, detail, fix_options) can be unit-tested directly
/// without needing to cross the (env-controlled, default 50000) threshold through a live query.
fn finding_for(n_val: &Value, days: f64) -> Value {
    let n_str = tmpl(Some(n_val));
    let days_str = num_to_str(days);
    json!({
        "kind": "runtime-events-bloat",
        "severity": "cleanup",
        "message": format!(
            "{} runtime_events rows are older than {}d — disposable step-telemetry that only grows state.sqlite. Prune keeps the recent tail (FE waterfall + forensics) intact.",
            n_str, days_str
        ),
        "detail": { "prunable_rows": n_val.clone(), "retention_days": num_to_json(days) },
        "suggested_action": "choose: prune the old rows, or keep the full runtime log",
        "confidence": 0.6,
        "fix_options": [
            {
                "key": "A",
                "label": format!("Prune the {} rows older than {}d (reclaim disk via an offline VACUUM afterwards)", n_str, days_str),
                "fix": FIX_A_SRC,
            },
            {
                "key": "B",
                "label": "Skip — keep the full runtime_events history",
                "fix": FIX_B_SRC,
            },
        ],
    })
}

/// JS: `async detect(db)` — counts runtime_events older than the retention window; the try/catch
/// around the query maps a missing runtime_events table (pre-RUNTIME-LOG substrate) to `row = null`
/// exactly like the JS `catch { /* table absent on a pre-RUNTIME-LOG substrate */ }`. Below the
/// min-prune threshold (or table absent) → `[]`; otherwise a single finding via `finding_for`.
pub fn detect(db: &Connection) -> Value {
    let days = retention_days();
    let param = format!("-{} days", num_to_str(days));
    let row = crate::db_json::query_rows(
        db,
        &format!("\n        SELECT COUNT(*) AS n FROM runtime_events WHERE ts < datetime({}, ?)\n      ", crate::clock::now_sql_inner()),
        &[json!(param)],
    )
    .ok()
    .and_then(|v| v.as_array().and_then(|a| a.first().cloned()));

    let n_val = match row.as_ref().and_then(|r| r.get("n")) {
        Some(v) => v.clone(),
        None => return json!([]),
    };
    let n = n_val.as_f64().unwrap_or(f64::NAN);
    if n < min_prune_rows() {
        return json!([]);
    }
    json!([finding_for(&n_val, days)])
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
            "CREATE TABLE runtime_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL,
                parent_event_id INTEGER,
                actor TEXT,
                action TEXT,
                skill TEXT,
                outcome TEXT
            )",
        )
        .unwrap();
        c
    }

    fn ins(c: &Connection, id: i64, ts: &str, parent: Option<i64>) {
        c.execute(
            "INSERT INTO runtime_events (id, ts, parent_event_id, actor, action, skill, outcome) VALUES (?1, ?2, ?3, 'test', 'a', 's', 'success')",
            rusqlite::params![id, ts, parent],
        )
        .unwrap();
    }

    #[test]
    fn absent_table_and_empty_table_return_empty() {
        let bare = Connection::open_in_memory().unwrap();
        assert_eq!(detect(&bare), json!([])); // runtime_events absent → JS catch → row stays null → []
        assert_eq!(detect(&db()), json!([]));
    }

    #[test]
    fn detect_stays_quiet_under_the_default_min_prune_threshold() {
        // Frozen now = 2026-01-01 00:00:00; default retention 30d → cutoff 2025-12-02 00:00:00.
        let c = db();
        ins(&c, 1, "2025-11-01 00:00:00", None); // old — counted by the WHERE ts < cutoff clause
        ins(&c, 2, "2025-11-05 00:00:00", None); // old — counted
        ins(&c, 3, "2025-12-20 00:00:00", None); // recent — excluded from the count
        // n = 2, far below the default minPruneRows() of 50000 → still [].
        assert_eq!(detect(&c), json!([]));
    }

    #[test]
    fn finding_for_matches_the_js_shape_and_embeds_the_stringified_fix_options() {
        let f = finding_for(&json!(999999), 30.0);
        assert_eq!(f["kind"], json!("runtime-events-bloat"));
        assert_eq!(f["severity"], json!("cleanup"));
        assert_eq!(
            f["message"],
            json!("999999 runtime_events rows are older than 30d — disposable step-telemetry that only grows state.sqlite. Prune keeps the recent tail (FE waterfall + forensics) intact.")
        );
        assert_eq!(f["detail"], json!({ "prunable_rows": 999999, "retention_days": 30 }));
        assert_eq!(f["suggested_action"], json!("choose: prune the old rows, or keep the full runtime log"));
        assert_eq!(f["confidence"], json!(0.6));
        assert_eq!(f["fix_options"][0]["key"], json!("A"));
        assert_eq!(
            f["fix_options"][0]["label"],
            json!("Prune the 999999 rows older than 30d (reclaim disk via an offline VACUUM afterwards)")
        );
        assert_eq!(f["fix_options"][0]["fix"], json!("(_f, db2) => prune(db2)"));
        assert_eq!(f["fix_options"][1]["key"], json!("B"));
        assert_eq!(f["fix_options"][1]["label"], json!("Skip — keep the full runtime_events history"));
        assert_eq!(
            f["fix_options"][1]["fix"],
            json!("async () => ({ applied: false, reason: 'operator chose to keep the full runtime log — nothing pruned' })")
        );
    }

    #[test]
    fn fix_prunes_old_rows_severs_the_straddler_fk_and_keeps_the_recent_tail() {
        let c = db();
        ins(&c, 1, "2025-11-01 00:00:00", None); // old parent — pruned
        ins(&c, 2, "2025-11-02 00:00:00", Some(1)); // old child — pruned
        ins(&c, 3, "2025-12-15 00:00:00", Some(1)); // recent straddler pointing at a pruned row
        ins(&c, 4, "2025-12-20 00:00:00", None); // normal recent row

        let r = fix(&json!({}), &c);
        assert_eq!(
            r,
            json!({ "applied": true, "changes": { "table": "runtime_events", "rows_pruned": 2, "older_than": "2025-12-02 00:00:00" } })
        );

        let remaining: Vec<(i64, Option<i64>)> = {
            let mut stmt = c.prepare("SELECT id, parent_event_id FROM runtime_events ORDER BY id").unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap().collect::<Result<_, _>>().unwrap()
        };
        assert_eq!(remaining, vec![(3, None), (4, None)]); // straddler's FK severed, both recent rows survive
    }

    #[test]
    fn fix_is_a_true_noop_when_nothing_is_old() {
        let c = db();
        ins(&c, 1, "2025-12-20 00:00:00", None);
        ins(&c, 2, "2025-12-25 00:00:00", None);
        let r = fix(&json!({}), &c);
        assert_eq!(
            r,
            json!({
                "applied": false,
                "changes": { "table": "runtime_events", "rows_pruned": 0, "older_than": "2025-12-02 00:00:00" },
                "reason": "nothing older than the retention window",
            })
        );
        let count: i64 = c.query_row("SELECT COUNT(*) FROM runtime_events", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 2);
    }
}
