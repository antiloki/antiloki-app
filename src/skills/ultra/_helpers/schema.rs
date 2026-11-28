//! schema.rs — twin of skills/ultra/_helpers/schema.mjs (helper module). NAMES ARE
//! CONTRACT: `ensureUltraSchema` (db-first helper, db-only signature).
// QUALITY: 80/100 — ULTRA REVIEW's score substrate (judgments.sqlite): one row per survey
// run, one row per scored unit, one row per insight (dossier/brief), one row per concept.
// The SQL is EXECUTED (never returned), so per the frozen-clock mirror the executed copy pins
// datetime('now') to the frozen instant — the conform JS lane's db proxy rewrites executed
// SQL, so the created_at DEFAULTs fire frozen there; when a later twin INSERTs into these
// tables, an unfrozen Rust DEFAULT would diverge on db_after (cockpitBlackboardPost precedent).
use rusqlite::Connection;
use serde_json::Value;

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS judgments.ultra_runs (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  created_at      TEXT NOT NULL DEFAULT (datetime('now')),
  source          TEXT NOT NULL DEFAULT 'manual',
  root            TEXT,
  scope_json      TEXT NOT NULL DEFAULT '{}',      -- {folder?|files?, capped?}
  status          TEXT NOT NULL DEFAULT 'running', -- running | done | error
  files           INTEGER NOT NULL DEFAULT 0,
  formula_version TEXT NOT NULL DEFAULT 'v1',
  totals_json     TEXT,                            -- the run's summary (axes averages, meta scores)
  error           TEXT
);
CREATE TABLE IF NOT EXISTS judgments.ultra_scores (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id         INTEGER NOT NULL,
  path           TEXT NOT NULL,
  folder         TEXT NOT NULL,
  content_hash   TEXT,                             -- law 4: unchanged content replays free
  atlas_known    INTEGER NOT NULL DEFAULT 0,       -- the atlas can see this file (Determinism input)
  is_test        INTEGER NOT NULL DEFAULT 0,
  loc            INTEGER,
  fan_in         INTEGER,
  fan_out        INTEGER,
  complexity     INTEGER,
  transitive     INTEGER,                          -- transitive import mass (hidden complexity)
  covered        INTEGER,                          -- a test imports it
  churn          INTEGER,                          -- commits touching it (recent history)
  authors        INTEGER,                          -- distinct recent authors (bus factor)
  findings       INTEGER NOT NULL DEFAULT 0,       -- kept bug-finder findings (defect axis)
  findings_high  INTEGER NOT NULL DEFAULT 0,
  smells_json    TEXT,                             -- atlas smell kinds hitting this file
  axes_json      TEXT NOT NULL,                    -- {axis:{score,parts:{…}}} — the visible formula
  health         INTEGER,                          -- 0-100 weighted mean of available axes
  ai_readability INTEGER,                          -- 0-100 proxy_v1 (context locality)
  pct_repo       REAL,                             -- health percentile among scope non-test files
  pct_folder     REAL,                             -- health percentile among folder siblings
  created_at     TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS judgments.ultra_scores_run_idx ON ultra_scores (run_id);
CREATE INDEX IF NOT EXISTS judgments.ultra_scores_path_idx ON ultra_scores (path);
CREATE TABLE IF NOT EXISTS judgments.ultra_insights (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id     INTEGER NOT NULL,
  kind       TEXT NOT NULL,                       -- dossier | brief
  path       TEXT,                                -- the unit (dossiers) · NULL for the brief
  body_json  TEXT NOT NULL,                       -- dossier: {why, alternatives, fix_sketch, estimated_gain} · brief: {headline, state_md, moves}
  model      TEXT,                                -- epistemic tag: grounded claim BY this model, never a counted fact
  cost_usd   REAL NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS judgments.ultra_insights_run_idx ON ultra_insights (run_id, kind);
CREATE TABLE IF NOT EXISTS judgments.ultra_concepts (
  id           INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id       INTEGER NOT NULL,
  path         TEXT NOT NULL,
  content_hash TEXT NOT NULL,                     -- the cache key: unchanged file → free replay
  name         TEXT NOT NULL,                     -- normalized idea label ('audit chain')
  kind         TEXT NOT NULL DEFAULT 'mechanism', -- domain | mechanism | pattern
  evidence     TEXT,                              -- the symbol/line that carries the idea
  model        TEXT,                              -- grounded claim BY this model (law 2)
  created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS judgments.ultra_concepts_run_idx ON ultra_concepts (run_id);
CREATE INDEX IF NOT EXISTS judgments.ultra_concepts_hash_idx ON ultra_concepts (content_hash);
CREATE INDEX IF NOT EXISTS judgments.ultra_concepts_name_idx ON ultra_concepts (name);
"#;

// JS `ensureUltraSchema(db)` execs the qualified DDL, then swallow-heals one idempotent
// column-add (content_hash on ultra_scores — CREATE IF NOT EXISTS won't add columns to an
// already-existing table, and migrations don't run on the active workspace). Returns
// undefined (-> null).
pub fn ensureUltraSchema(db: &Connection) -> Value {
    let _ = db.execute_batch(&SCHEMA_SQL.replace("datetime('now'", &format!("datetime({}", crate::clock::now_sql_inner())));
    let _ = db.execute_batch("ALTER TABLE judgments.ultra_scores ADD COLUMN content_hash TEXT");
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
        assert_eq!(ensureUltraSchema(&c), Value::Null);
        // idempotent — CREATE IF NOT EXISTS, a second run does not error
        assert_eq!(ensureUltraSchema(&c), Value::Null);
        assert_eq!(
            names(&c, "judgments.sqlite_master"),
            vec![
                "ultra_concepts",
                "ultra_concepts_hash_idx",
                "ultra_concepts_name_idx",
                "ultra_concepts_run_idx",
                "ultra_insights",
                "ultra_insights_run_idx",
                "ultra_runs",
                "ultra_scores",
                "ultra_scores_path_idx",
                "ultra_scores_run_idx"
            ]
        );
        // main db stays untouched — the qualified DDL lands only in judgments
        assert!(names(&c, "sqlite_master").is_empty());
    }

    #[test]
    fn defaults_fire_frozen_and_content_hash_column_heals() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS judgments").unwrap();
        ensureUltraSchema(&c);
        // the executed copy is frozen: created_at DEFAULT fires at the pinned instant
        c.execute("INSERT INTO judgments.ultra_runs DEFAULT VALUES", []).unwrap();
        let (created_at, source, status, scope_json, files, formula_version): (String, String, String, String, i64, String) = c
            .query_row(
                "SELECT created_at, source, status, scope_json, files, formula_version FROM judgments.ultra_runs",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .unwrap();
        assert_eq!(created_at, "2026-01-01 00:00:00");
        assert_eq!(source, "manual");
        assert_eq!(status, "running");
        assert_eq!(scope_json, "{}");
        assert_eq!(files, 0);
        assert_eq!(formula_version, "v1");
        // content_hash: the ALTER-add loop ran against a table that already declares the
        // column via CREATE TABLE — the swallowed error must not have broken the column.
        c.execute(
            "INSERT INTO judgments.ultra_scores (run_id, path, folder, axes_json, content_hash) VALUES (1, 'a.mjs', 'a', '{}', 'deadbeef')",
            [],
        )
        .unwrap();
        let (content_hash, atlas_known, is_test, findings): (String, i64, i64, i64) = c
            .query_row(
                "SELECT content_hash, atlas_known, is_test, findings FROM judgments.ultra_scores",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(content_hash, "deadbeef");
        assert_eq!(atlas_known, 0);
        assert_eq!(is_test, 0);
        assert_eq!(findings, 0);
    }
}
