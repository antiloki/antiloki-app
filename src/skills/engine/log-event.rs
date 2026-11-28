//! log-event.rs — twin of skills/engine/log-event.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `logEvent` (default). Best-effort INSERT into runtime_events; a write failure must NEVER throw
//! (logging can't block dispatch) → the error is swallowed and the result is a null id.
// QUALITY: schema-aware INSERT (mig 033 user_goal_id / mig 035 source) via PRAGMA column probe;
// returns lastInsertRowid or null. The env-gated LIVE stderr trace is output-irrelevant (omitted).
use rusqlite::Connection;
use serde_json::{json, Value};

// JS destructuring default: applies ONLY when the key is absent/undefined; an explicit null keeps null.
fn field(input: &Value, key: &str, default: Value) -> Value {
    match input.get(key) {
        None => default,
        Some(v) => v.clone(),
    }
}
// JS truthy, for `if (!outcome)`: '' / 0 / NaN / null / undefined / false are falsy.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

pub fn logEvent(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS `const { db } = context ?? {}` — db is always the &Connection here (harness passes it).
    // JS `const { ... } = input ?? {}` — a non-object input yields all-defaults (undefined props).
    let input = if input.is_object() { input.clone() } else { json!({}) };

    let parent_event_id = field(&input, "parent_event_id", Value::Null);
    let actor = field(&input, "actor", json!("unknown"));
    let agent = field(&input, "agent", Value::Null);
    let action = field(&input, "action", Value::Null);
    let skill = field(&input, "skill", Value::Null);
    let duration_ms = field(&input, "duration_ms", Value::Null);
    let outcome = input.get("outcome").cloned().unwrap_or(Value::Null); // no default in JS
    let error_code = field(&input, "error_code", Value::Null);
    let ctx_obj = field(&input, "context", Value::Null);
    let user_goal_id = field(&input, "user_goal_id", Value::Null);
    let source = field(&input, "source", Value::Null);

    if !truthy(&outcome) {
        return Err("log-event: outcome is required".into());
    }

    // JS: if (ctxObj != null) context_json = JSON.stringify(ctxObj). `!= null` excludes null AND
    // undefined. serde compact + preserve_order == JSON.stringify for JSON-round-tripped values.
    let context_json: Value = if ctx_obj.is_null() {
        Value::Null
    } else {
        Value::String(
            serde_json::to_string(&ctx_obj)
                .unwrap_or_else(|_| "{\"_serialization_error\":true}".into()),
        )
    };

    // Schema-aware INSERT: mig 033 added user_goal_id, mig 035 added source. Probe present columns.
    let cols: Vec<String> = crate::db_json::query_rows(db, "PRAGMA table_info(runtime_events)", &[])
        .ok()
        .and_then(|r| r.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|c| c.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .collect();
    let has_user_goal = cols.iter().any(|c| c == "user_goal_id");
    let has_source = cols.iter().any(|c| c == "source");

    let (sql, params): (&str, Vec<Value>) = if has_user_goal && has_source {
        (
            "INSERT INTO runtime_events \
               (parent_event_id, actor, agent, action, skill, duration_ms, outcome, error_code, context_json, user_goal_id, source) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            vec![parent_event_id, actor, agent, action, skill, duration_ms, outcome, error_code, context_json, user_goal_id, source],
        )
    } else if has_user_goal {
        (
            "INSERT INTO runtime_events \
               (parent_event_id, actor, agent, action, skill, duration_ms, outcome, error_code, context_json, user_goal_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            vec![parent_event_id, actor, agent, action, skill, duration_ms, outcome, error_code, context_json, user_goal_id],
        )
    } else {
        (
            "INSERT INTO runtime_events \
               (parent_event_id, actor, agent, action, skill, duration_ms, outcome, error_code, context_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            vec![parent_event_id, actor, agent, action, skill, duration_ms, outcome, error_code, context_json],
        )
    };

    // Best-effort: swallow a write failure (e.g. the outcome CHECK constraint) → null id, no throw.
    let id = match crate::db_json::run_write(db, sql, &params) {
        Ok((_changes, rowid)) => json!(rowid),
        Err(_) => Value::Null,
    };
    // A CAP ON THE TABLE (Grokbot QA 2026-10-12: 323,684 rows and nothing to stop it; the age-based pruning only runs after an operator-gated notice, from 50,000 rows). Every thousandth row, anything more than
    // RUNTIME_EVENTS_KEEP rows behind the newest is removed, and a surviving child whose parent went is detached. Best-effort like the insert itself.
    if let Some(n) = id.as_i64() { if n > RUNTIME_EVENTS_KEEP && n % 1000 == 0 { cap_runtime_events(db, n); } }
    Ok(id)
}
/// How many of the newest rows the activity table keeps.
pub(crate) const RUNTIME_EVENTS_KEEP: i64 = 50_000;
/// The most rows one trim removes. A trim that took the whole backlog in one statement held the engine's only connection for minutes on a table that had grown
/// to millions of rows before the cap existed (2026-11-10: a 6,068,129-row workspace database wedged every request behind it, the workspace list included).
/// A bloated table is now worked down in slices, each a fraction of a second, one per thousand inserts.
pub(crate) const CAP_BATCH: i64 = 20_000;
/// How far past a removed slice the trim looks for children to detach. Children are logged inside their parent's own action, so they sit just after it; a
/// window of row ids is a range scan, where "any row with a parent below the cutoff" is a scan of the whole table (no index on parent_event_id).
const DETACH_WINDOW: i64 = 100_000;
pub(crate) fn cap_runtime_events(db: &Connection, newest: i64) { cap_runtime_events_by(db, newest, CAP_BATCH) }
pub(crate) fn cap_runtime_events_by(db: &Connection, newest: i64, batch: i64) {
    let cutoff = newest - RUNTIME_EVENTS_KEEP;
    // the oldest row (the primary key's own end: no scan), and at most `batch` rows from there, never past the cutoff
    let lo = match db.query_row("SELECT MIN(id) FROM runtime_events", [], |r| r.get::<_, Option<i64>>(0)) { Ok(Some(v)) => v, _ => return };
    if lo >= cutoff { return; }
    let hi = cutoff.min(lo + batch.max(1));
    // THE REAL COST WAS NEVER THE ROW COUNT (found live 2026-11-10 under gdb, then reproduced: 2,000 rows out of 400,000 took 39.9 s with foreign keys on and 0.03 s with them off).
    // `parent_event_id` is a foreign key to this same table with no index, and rusqlite's bundled SQLite enforces foreign keys by default (the sqlite3 shell does not), so every
    // deleted row made SQLite scan the whole table for children: rows x table, minutes on a 6,000,000-row table, with the engine's only connection held. The detach above already
    // clears the references that matter and a trim leaves nothing that needs the check, so it runs with enforcement off, put back straight after. The pragma cannot change inside a
    // transaction: there the trim waits for the next insert rather than run the slow path.
    let fk_on = db.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0)).map_or(false, |v| v == 1);
    if fk_on && !db.is_autocommit() { return; }
    if fk_on { let _ = db.execute_batch("PRAGMA foreign_keys = OFF"); }
    let _ = db.execute("UPDATE runtime_events SET parent_event_id = NULL WHERE id >= ?1 AND id < ?2 AND parent_event_id IS NOT NULL AND parent_event_id < ?1", [hi, hi + DETACH_WINDOW]);
    let _ = db.execute("DELETE FROM runtime_events WHERE id < ?1", [hi]);
    if fk_on { let _ = db.execute_batch("PRAGMA foreign_keys = ON"); }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANONICAL: &str = "CREATE TABLE runtime_events (\
        id INTEGER PRIMARY KEY AUTOINCREMENT, \
        ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', \
        parent_event_id INTEGER, actor TEXT NOT NULL, agent TEXT, action TEXT, skill TEXT, \
        duration_ms REAL, \
        outcome TEXT NOT NULL CHECK (outcome IN ('success','error','blocked','skipped','in_flight')), \
        error_code TEXT, context_json TEXT, user_goal_id INTEGER, source TEXT)";

    fn open(schema: &str) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(schema).unwrap();
        c
    }

    #[test]
    fn inserts_full_row_and_returns_id() {
        let c = open(CANONICAL);
        let id = logEvent(
            &json!({ "parent_event_id": 7, "actor": "runner", "agent": "dev", "action": "card.add",
                     "skill": "card-add", "duration_ms": 12.5, "outcome": "error", "error_code": "E_X",
                     "context": { "k": "v", "n": 2 }, "user_goal_id": 3, "source": "cli" }),
            &c,
        )
        .unwrap();
        assert_eq!(id, json!(1));
        let cj: String = c
            .query_row("SELECT context_json FROM runtime_events WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cj, "{\"k\":\"v\",\"n\":2}"); // == JS JSON.stringify
        let (actor, source): (String, String) = c
            .query_row("SELECT actor, source FROM runtime_events WHERE id=1", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(actor, "runner");
        assert_eq!(source, "cli");
    }

    #[test]
    fn defaults_actor_and_null_context() {
        let c = open(CANONICAL);
        let id = logEvent(&json!({ "outcome": "success", "action": "demo" }), &c).unwrap();
        assert_eq!(id, json!(1));
        let (actor, cj): (String, Option<String>) = c
            .query_row("SELECT actor, context_json FROM runtime_events WHERE id=1", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(actor, "unknown"); // missing actor → JS default
        assert_eq!(cj, None); // missing context → NULL, not the string "null"
    }

    #[test]
    fn the_table_is_capped_and_a_child_whose_parent_was_removed_is_detached() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(CANONICAL).unwrap();
        // 60 000 rows, row 59 990 a child of row 10 (which the cap will remove)
        c.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 60000) INSERT INTO runtime_events (actor, action, outcome) SELECT 'a', 'x', 'success' FROM n;").unwrap();
        c.execute("UPDATE runtime_events SET parent_event_id = 10 WHERE id = 59990", []).unwrap();
        cap_runtime_events(&c, 60_000);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM runtime_events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, RUNTIME_EVENTS_KEEP + 1, "the newest {RUNTIME_EVENTS_KEEP} stay (plus the cutoff row)");
        let (min,): (i64,) = c.query_row("SELECT MIN(id) FROM runtime_events", [], |r| Ok((r.get(0)?,))).unwrap();
        assert_eq!(min, 60_000 - RUNTIME_EVENTS_KEEP);
        let parent: Option<i64> = c.query_row("SELECT parent_event_id FROM runtime_events WHERE id = 59990", [], |r| r.get(0)).unwrap();
        assert_eq!(parent, None, "a survivor does not point at a row that is gone");
        // under the cap nothing is touched
        cap_runtime_events(&c, 40_000);
        let n2: i64 = c.query_row("SELECT COUNT(*) FROM runtime_events", [], |r| r.get(0)).unwrap();
        assert_eq!(n2, n);
    }

    #[test]
    fn a_table_far_over_the_cap_is_worked_down_in_slices_not_in_one_statement() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(CANONICAL).unwrap();
        c.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 60000) INSERT INTO runtime_events (actor, action, outcome) SELECT 'a', 'x', 'success' FROM n;").unwrap();
        let count = |c: &Connection| c.query_row("SELECT COUNT(*) FROM runtime_events", [], |r| r.get::<_, i64>(0)).unwrap();
        // 10 000 rows over the cap, a slice of 4 000: one call removes at most one slice
        cap_runtime_events_by(&c, 60_000, 4_000);
        assert_eq!(count(&c), 60_000 - 4_000, "one slice, not the whole backlog");
        cap_runtime_events_by(&c, 60_000, 4_000);
        cap_runtime_events_by(&c, 60_000, 4_000);
        assert_eq!(count(&c), RUNTIME_EVENTS_KEEP + 1, "three calls reach the cap and stop at it");
        cap_runtime_events_by(&c, 60_000, 4_000);
        assert_eq!(count(&c), RUNTIME_EVENTS_KEEP + 1, "nothing more to remove");
        // an empty table and a batch of nothing are harmless
        let e = Connection::open_in_memory().unwrap(); e.execute_batch(CANONICAL).unwrap();
        cap_runtime_events_by(&e, 90_000, 0); cap_runtime_events(&e, 90_000);
    }

    #[test]
    fn a_trim_does_not_pay_for_the_unindexed_self_reference_when_foreign_keys_are_enforced() {
        // the REAL table: parent_event_id references the table itself, with no index on it, and the connection enforces foreign keys (rusqlite's bundled SQLite does by default)
        let real = CANONICAL.replace("parent_event_id INTEGER,", "parent_event_id INTEGER REFERENCES runtime_events(id),");
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        c.execute_batch(&real).unwrap();
        c.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 150000) INSERT INTO runtime_events (actor, action, outcome) SELECT 'a', 'x', 'success' FROM n;").unwrap();
        c.execute("UPDATE runtime_events SET parent_event_id = 5 WHERE id = 100000", []).unwrap();
        let t0 = std::time::Instant::now();
        cap_runtime_events_by(&c, 150_000, 3_000);
        assert!(t0.elapsed() < std::time::Duration::from_secs(3), "a slice of 3,000 rows out of 150,000 took {:?}: the foreign-key scan per deleted row is back", t0.elapsed());
        let n: i64 = c.query_row("SELECT COUNT(*) FROM runtime_events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 150_000 - 3_000, "one slice went");
        let fk: i64 = c.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(fk, 1, "enforcement is put back for everything else");
        let parent: Option<i64> = c.query_row("SELECT parent_event_id FROM runtime_events WHERE id = 100000", [], |r| r.get(0)).unwrap();
        assert_eq!(parent, None, "a survivor does not point at a row that is gone");
    }

    #[test]
    fn bogus_outcome_is_swallowed_to_null() {
        let c = open(CANONICAL);
        let id = logEvent(&json!({ "outcome": "bogus", "actor": "x" }), &c).unwrap();
        assert_eq!(id, Value::Null); // CHECK violation swallowed
        let count: i64 = c.query_row("SELECT COUNT(*) FROM runtime_events", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn missing_outcome_errs() {
        let c = open(CANONICAL);
        assert!(logEvent(&json!({ "actor": "x" }), &c).is_err());
    }

    #[test]
    fn mig033_only_uses_ten_col_insert() {
        // user_goal_id present, source absent → 10-column INSERT shape
        let c = open(
            "CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT DEFAULT '2026-01-01 00:00:00', \
             parent_event_id INTEGER, actor TEXT NOT NULL, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, \
             outcome TEXT NOT NULL, error_code TEXT, context_json TEXT, user_goal_id INTEGER)",
        );
        let id = logEvent(&json!({ "outcome": "success", "actor": "a", "user_goal_id": 9 }), &c).unwrap();
        assert_eq!(id, json!(1));
        let ug: i64 = c.query_row("SELECT user_goal_id FROM runtime_events WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(ug, 9);
    }

    #[test]
    fn base_schema_uses_nine_col_insert() {
        // neither user_goal_id nor source → 9-column INSERT shape
        let c = open(
            "CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT DEFAULT '2026-01-01 00:00:00', \
             parent_event_id INTEGER, actor TEXT NOT NULL, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, \
             outcome TEXT NOT NULL, error_code TEXT, context_json TEXT)",
        );
        // passing user_goal_id/source is ignored by the 9-col shape (columns don't exist)
        let id = logEvent(&json!({ "outcome": "skipped", "actor": "a", "user_goal_id": 9, "source": "cli" }), &c).unwrap();
        assert_eq!(id, json!(1));
        let outcome: String = c.query_row("SELECT outcome FROM runtime_events WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(outcome, "skipped");
    }
}
