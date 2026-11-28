//! schema.rs — twin of skills/bugfinder/_helpers/schema.mjs (helper module). NAMES ARE
//! CONTRACT: `ensureBugfinderSchema` (db-first helper, db-only signature).
// QUALITY: 80/100 — the BUG FINDER's ledger DDL (judgments.sqlite): 3 tables + 2 indexes,
// one row per run, one row per finding INCLUDING dropped leads (dropped ≠ hidden). The SQL
// is EXECUTED (never returned), so per the frozen-clock mirror the executed copy pins
// datetime('now') to the frozen instant — the conform JS lane's db proxy rewrites executed
// SQL, so the created_at DEFAULTs fire frozen there; when a later twin INSERTs into these
// tables, an unfrozen Rust DEFAULT would diverge on db_after (cockpitBlackboardPost precedent).
use rusqlite::Connection;
use serde_json::Value;

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS judgments.bugfinder_runs (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  created_at    TEXT NOT NULL DEFAULT (datetime('now')),
  source        TEXT NOT NULL DEFAULT 'manual',   -- manual | pipeline | vibe | ultra
  session_id    TEXT,                              -- vibe session when source='vibe'
  contract_slug TEXT,                              -- card contract when source='pipeline'
  base_sha      TEXT,
  head_sha      TEXT,
  scope_json    TEXT NOT NULL DEFAULT '{}',        -- {files:[…], base?, capped?}
  status        TEXT NOT NULL DEFAULT 'running',   -- running | done | error
  waves_json    TEXT,                              -- per-wave telemetry {w1,w2,w3}
  totals_json   TEXT,                              -- {by_wave, by_category, kept, dropped}
  cost_usd      REAL NOT NULL DEFAULT 0,
  error         TEXT
);
CREATE TABLE IF NOT EXISTS judgments.bugfinder_findings (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id        INTEGER NOT NULL,
  wave          INTEGER NOT NULL,                  -- 1 deterministic | 2 shard swarm | 3 mid sweep
  category      TEXT NOT NULL,                     -- lint|typecheck|hallucinated-import|unknown-symbol|structure:*|checking|algorithm|timing|interface|security|test-oracle|other:*
  severity      TEXT NOT NULL DEFAULT 'medium',    -- low | medium | high
  file          TEXT,
  line          INTEGER,
  detail        TEXT NOT NULL,
  evidence      TEXT,                              -- wave 1: tool output · waves 2-3: VERBATIM quote from the file
  model         TEXT,                              -- the model that proposed it (waves 2-3)
  verified      INTEGER NOT NULL DEFAULT 0,        -- 1 = decided by a deterministic oracle (wave 1); AI leads stay 0 until a repro rung exists
  dropped       INTEGER NOT NULL DEFAULT 0,        -- 1 = void by rule; drop_reason says why
  drop_reason   TEXT,                              -- ungrounded-evidence | off-scope | malformed | …
  operator_class TEXT,                             -- real | false-positive | known (one-click classify)
  created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS judgments.bugfinder_findings_run_idx ON bugfinder_findings (run_id);
CREATE INDEX IF NOT EXISTS judgments.bugfinder_findings_cat_idx ON bugfinder_findings (category, dropped);
CREATE TABLE IF NOT EXISTS judgments.bugfinder_schedules (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  name        TEXT NOT NULL,
  scope_json  TEXT NOT NULL DEFAULT '{}',       -- {folder?|files?|base?, shards?, skip?, max_files?}
  cadence     TEXT NOT NULL DEFAULT 'weekly',   -- daily | weekly | manual
  hour_utc    INTEGER NOT NULL DEFAULT 6,
  enabled     INTEGER NOT NULL DEFAULT 1,
  last_run_at TEXT,
  last_run_id INTEGER,
  created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

// JS `ensureBugfinderSchema(db)` execs the qualified DDL and returns undefined (→ null).
// Idempotent CREATE IF NOT EXISTS only — called by every bugfinder skill at runtime so the
// active workspace self-heals (the cockpit-schema pattern; migrations don't run there).
pub fn ensureBugfinderSchema(db: &Connection) -> Value {
    let _ = db.execute_batch(&SCHEMA_SQL.replace("datetime('now'", &format!("datetime({}", crate::clock::now_sql_inner())));
    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(c: &Connection, master: &str) -> Vec<String> {
        c.prepare(&format!(
            "SELECT name FROM {master} WHERE type IN ('table','index') AND name NOT LIKE 'sqlite_%' ORDER BY name"
        ))
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    #[test]
    fn creates_judgments_ledger_idempotently_main_untouched() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS judgments").unwrap();
        assert_eq!(ensureBugfinderSchema(&c), Value::Null);
        // idempotent — CREATE IF NOT EXISTS, a second run does not error
        assert_eq!(ensureBugfinderSchema(&c), Value::Null);
        assert_eq!(
            names(&c, "judgments.sqlite_master"),
            vec![
                "bugfinder_findings",
                "bugfinder_findings_cat_idx",
                "bugfinder_findings_run_idx",
                "bugfinder_runs",
                "bugfinder_schedules"
            ]
        );
        // main db stays untouched — the qualified DDL lands only in judgments
        assert!(names(&c, "sqlite_master").is_empty());
    }

    #[test]
    fn defaults_fire_frozen_and_dropped_leads_are_counted() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS judgments").unwrap();
        ensureBugfinderSchema(&c);
        // the executed copy is frozen: created_at DEFAULT fires at the pinned instant
        c.execute("INSERT INTO judgments.bugfinder_runs DEFAULT VALUES", []).unwrap();
        let (created_at, source, status, scope_json, cost_usd): (String, String, String, String, f64) = c
            .query_row(
                "SELECT created_at, source, status, scope_json, cost_usd FROM judgments.bugfinder_runs",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(created_at, "2026-01-01 00:00:00");
        assert_eq!(source, "manual");
        assert_eq!(status, "running");
        assert_eq!(scope_json, "{}");
        assert_eq!(cost_usd, 0.0);
        // dropped ≠ hidden: a dropped lead lands as a counted row with its drop_reason
        c.execute(
            "INSERT INTO judgments.bugfinder_findings (run_id, wave, category, detail, dropped, drop_reason) VALUES (1, 2, 'checking', 'lead', 1, 'off-scope')",
            [],
        )
        .unwrap();
        let (severity, verified, dropped, drop_reason): (String, i64, i64, String) = c
            .query_row(
                "SELECT severity, verified, dropped, drop_reason FROM judgments.bugfinder_findings",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(severity, "medium");
        assert_eq!(verified, 0);
        assert_eq!(dropped, 1);
        assert_eq!(drop_reason, "off-scope");
    }
}
