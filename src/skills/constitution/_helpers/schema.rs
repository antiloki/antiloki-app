//! schema.rs — twin of skills/constitution/_helpers/schema.mjs (helper module). NAMES ARE
//! CONTRACT: `normalizeScope` (no-db helper) + `ensureConstitutionSchema` (db-first helper,
//! db-only signature). THE CONSTITUTION REVIEW's storage: laws.sqlite (review_laws +
//! review_law_exceptions) + judgments.sqlite (review_snapshots/scores/violations/insights/
//! papers/classifications).
// QUALITY: 80/100 — self-heals a pre-taxonomy review_laws table (drops a CHECK SQLite can't
// ALTER away, rebuilding once via rename→create→copy→drop) BEFORE the idempotent
// CREATE-IF-NOT-EXISTS block, then defensive column-adds. The DDL is EXECUTED (never
// returned), so per the frozen-clock mirror both executed copies pin datetime('now') to the
// frozen instant — the conform JS lane's db proxy rewrites executed SQL, so created_at/
// updated_at DEFAULTs fire frozen there; an unfrozen Rust DEFAULT would diverge on db_after
// (cockpitBlackboardPost precedent).
use rusqlite::Connection;
use serde_json::{json, Value};

const SCOPES: [&str; 6] = ["business", "architecture", "project", "framework", "community", "language"];

pub fn normalizeScope(s: &Value) -> Value {
    match s.as_str() {
        Some("code") => json!("project"),
        Some(x) if SCOPES.contains(&x) => json!(x),
        _ => json!("project"),
    }
}

// the one-time rebuild: legacy review_laws baked `scope IN ('community','language','code')`
// as a CHECK — SQLite can't drop a CHECK via ALTER, so rename, recreate without it, copy rows
// (mapping the legacy 'code' scope to 'project'), drop the legacy copy.
const MIGRATION_SQL: &str = r#"
ALTER TABLE laws.review_laws RENAME TO review_laws_legacy;
CREATE TABLE laws.review_laws (
  id INTEGER PRIMARY KEY AUTOINCREMENT, scope TEXT NOT NULL, key TEXT NOT NULL UNIQUE, title TEXT NOT NULL,
  rule_md TEXT NOT NULL, detector TEXT NOT NULL DEFAULT 'none', config_json TEXT NOT NULL DEFAULT '{}',
  weight INTEGER NOT NULL DEFAULT 5, severity TEXT NOT NULL DEFAULT 'warn', confidence REAL NOT NULL DEFAULT 0.9,
  enforcement TEXT NOT NULL DEFAULT 'score', applies_json TEXT NOT NULL DEFAULT '{}',
  status TEXT NOT NULL DEFAULT 'ratified', evidence_json TEXT, version INTEGER NOT NULL DEFAULT 1,
  created_by TEXT NOT NULL DEFAULT 'seed', created_at TEXT NOT NULL DEFAULT (datetime('now')), updated_at TEXT NOT NULL DEFAULT (datetime('now')));
INSERT INTO laws.review_laws (id, scope, key, title, rule_md, detector, config_json, weight, enforcement, applies_json, status, evidence_json, version, created_by, created_at, updated_at)
  SELECT id, CASE scope WHEN 'code' THEN 'project' ELSE scope END, key, title, rule_md, detector, config_json, weight, enforcement, applies_json, status, evidence_json, version, created_by, created_at, updated_at FROM laws.review_laws_legacy;
DROP TABLE laws.review_laws_legacy;
"#;

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS laws.review_laws (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  scope         TEXT NOT NULL,                     -- business|architecture|project|framework|community|language (code-validated)
  key           TEXT NOT NULL UNIQUE,              -- kebab, stable (findings cite key@version)
  title         TEXT NOT NULL,
  rule_md       TEXT NOT NULL,
  detector      TEXT NOT NULL DEFAULT 'none',      -- regex | absent | path | import | metric | graph | ast | layer | none
  config_json   TEXT NOT NULL DEFAULT '{}',        -- the detector's parameters
  weight        INTEGER NOT NULL DEFAULT 5,        -- 0-10; only detector-backed laws score
  severity      TEXT NOT NULL DEFAULT 'warn',      -- info | warn | error | critical (independent of frequency)
  confidence    REAL NOT NULL DEFAULT 0.9,         -- 0-1; how sure the detector is (regex < ast/graph) — scales the penalty
  enforcement   TEXT NOT NULL DEFAULT 'score',     -- score | advise (detector 'none' ⇒ advise)
  applies_json  TEXT NOT NULL DEFAULT '{}',        -- {languages:[], frameworks:[], paths:[], paths_exclude:[]}
  status        TEXT NOT NULL DEFAULT 'ratified',  -- candidate | ratified | suspended
  evidence_json TEXT,                              -- mining evidence: {pct, count_for, count_against, exemplars}
  version       INTEGER NOT NULL DEFAULT 1,
  created_by    TEXT NOT NULL DEFAULT 'seed',
  created_at    TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS laws.review_law_exceptions (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  law_key    TEXT NOT NULL,
  path_glob  TEXT NOT NULL,
  reason     TEXT NOT NULL,                        -- the exceptions court: accepted deviations, on record
  created_by TEXT NOT NULL DEFAULT 'operator',
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS judgments.review_snapshots (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  created_at        TEXT NOT NULL DEFAULT (datetime('now')),
  source            TEXT NOT NULL DEFAULT 'wizard',
  scope_json        TEXT NOT NULL DEFAULT '{}',    -- what the wizard chose
  manifest_json     TEXT NOT NULL DEFAULT '[]',    -- files this snapshot COVERED (honest staleness)
  constitution_hash TEXT NOT NULL,                 -- hash of ratified laws (key@version:weight) + exceptions
  status            TEXT NOT NULL DEFAULT 'running',
  files             INTEGER NOT NULL DEFAULT 0,
  judged            INTEGER NOT NULL DEFAULT 0,    -- computed fresh (rest carried from cache)
  duration_ms       INTEGER,
  totals_json       TEXT,
  error             TEXT,
  engineering_version TEXT                          -- engineering::ENGINEERING_VERSION; with constitution_hash, the comparability key
);
CREATE TABLE IF NOT EXISTS judgments.review_scores (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  snapshot_id   INTEGER NOT NULL,
  unit_kind     TEXT NOT NULL DEFAULT 'file',      -- file | binding
  path          TEXT NOT NULL,                     -- binding: 'a|b'
  content_hash  TEXT,
  languages     TEXT,                              -- csv, from extension
  score         INTEGER,                           -- LAW compliance: 100 - Σ(weight × hits × confidence)
  breakdown_json TEXT,                             -- {law_key: {hits, weight, confidence, penalty}}
  fan_in        INTEGER,
  risk          REAL,                              -- (100-health)/100 × (1+fan_in) — the AI spot ranking
  cached        INTEGER NOT NULL DEFAULT 0,        -- 1 = carried forward, not re-judged
  -- the deterministic ENGINEERING MODEL (folded in from the retired ultra screen)
  loc           INTEGER, complexity INTEGER, fan_out INTEGER, transitive INTEGER,
  covered       INTEGER, churn INTEGER, authors INTEGER, findings INTEGER,
  axes_json     TEXT,                              -- {axis:{score,parts}} incl. 'compliance' (the law axis)
  archetype     TEXT,                              -- logic | glue (barrel/re-export/const/type/config) | test | unknown
  health        INTEGER,                           -- engineering health = weighted axes (compliance is one)
  ai_readability INTEGER,
  pct_repo      REAL, pct_folder REAL,             -- neighborhood: percentile vs repo / folder siblings
  created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS judgments.review_scores_snap_idx ON review_scores (snapshot_id, risk);
CREATE TABLE IF NOT EXISTS judgments.review_violations (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  snapshot_id INTEGER NOT NULL,
  path        TEXT NOT NULL,
  symbol      TEXT,                                -- enclosing block when the atlas knows the spans
  line        INTEGER,
  law_key     TEXT NOT NULL,
  law_version INTEGER NOT NULL DEFAULT 1,          -- findings stay attributable when laws evolve
  law_scope   TEXT,
  weight      INTEGER NOT NULL,
  severity    TEXT,                                -- the proof chain: the law's severity
  confidence  REAL,                                -- and its confidence — both travel onto the finding
  evidence    TEXT,                                -- the matched source, verbatim
  created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS judgments.review_violations_snap_idx ON review_violations (snapshot_id, law_key);
CREATE TABLE IF NOT EXISTS judgments.review_insights (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  snapshot_id INTEGER NOT NULL,
  path        TEXT NOT NULL,
  body_json   TEXT NOT NULL,                       -- {insight, fix_sketch, kind, cited_laws}
  model       TEXT,                                -- grounded claim BY this model (law 2 of the fundamentals)
  attempts    INTEGER NOT NULL DEFAULT 1,          -- 2 = escalated (extreme risk or first output failed)
  cost_usd    REAL NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS judgments.review_papers (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  slug        TEXT UNIQUE,                            -- stable handle (the swarm_id when present)
  title       TEXT NOT NULL,
  label       TEXT,                                   -- the mission label
  discovery   TEXT,                                   -- the paper in one sentence
  swarm_id    TEXT,                                   -- joins the ai_calls ledger (session_id)
  snapshot_id INTEGER,                                -- the population this study stood on
  files_json  TEXT NOT NULL DEFAULT '[]',             -- the cases
  agents_json TEXT NOT NULL DEFAULT '[]',             -- the crew [{id,title}]
  paper_json  TEXT NOT NULL,                          -- the FULL session (sections + widgets + ingredients)
  markdown_md TEXT,                                   -- the rendered study, retrievable everywhere
  cost_usd    REAL NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE TABLE IF NOT EXISTS judgments.review_classifications (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  law_key    TEXT NOT NULL,
  path       TEXT,
  class      TEXT NOT NULL CHECK (class IN ('fixed','dismissed','known')),
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS judgments.review_classifications_law_idx ON review_classifications (law_key);
"#;

// defensive column-adds for tables created before these columns existed (CREATE IF NOT
// EXISTS never adds a column — the cockpit-schema lesson). Each is independently swallowed:
// duplicate-column errors are expected once the column already exists (fresh creates above
// already declare it, or a prior run already healed it).
const ALTER_COLUMNS: [&str; 20] = [
    "ALTER TABLE judgments.review_snapshots ADD COLUMN engineering_version TEXT",
    "ALTER TABLE laws.review_laws ADD COLUMN severity TEXT NOT NULL DEFAULT 'warn'",
    "ALTER TABLE laws.review_laws ADD COLUMN confidence REAL NOT NULL DEFAULT 0.9",
    "ALTER TABLE judgments.review_violations ADD COLUMN severity TEXT",
    "ALTER TABLE judgments.review_violations ADD COLUMN confidence REAL",
    "ALTER TABLE judgments.review_scores ADD COLUMN loc INTEGER",
    "ALTER TABLE judgments.review_scores ADD COLUMN complexity INTEGER",
    "ALTER TABLE judgments.review_scores ADD COLUMN fan_out INTEGER",
    "ALTER TABLE judgments.review_scores ADD COLUMN transitive INTEGER",
    "ALTER TABLE judgments.review_scores ADD COLUMN covered INTEGER",
    "ALTER TABLE judgments.review_scores ADD COLUMN churn INTEGER",
    "ALTER TABLE judgments.review_scores ADD COLUMN authors INTEGER",
    "ALTER TABLE judgments.review_scores ADD COLUMN findings INTEGER",
    "ALTER TABLE judgments.review_scores ADD COLUMN axes_json TEXT",
    "ALTER TABLE judgments.review_scores ADD COLUMN archetype TEXT",
    "ALTER TABLE judgments.review_scores ADD COLUMN health INTEGER",
    "ALTER TABLE judgments.review_scores ADD COLUMN ai_readability INTEGER",
    "ALTER TABLE judgments.review_scores ADD COLUMN pct_repo REAL",
    "ALTER TABLE judgments.review_scores ADD COLUMN pct_folder REAL",
    "ALTER TABLE judgments.review_scores ADD COLUMN blocks_json TEXT",
];

// JS `ensureConstitutionSchema(db)`: self-heal a pre-taxonomy review_laws (rebuild once, iff
// its DDL still carries the retired CHECK), exec the idempotent qualified DDL, then swallow-
// heal defensive column-adds. Returns undefined (-> null).
pub fn ensureConstitutionSchema(db: &Connection) -> Value {
    let ddl: String = db
        .query_row(
            "SELECT sql FROM laws.sqlite_master WHERE type='table' AND name='review_laws'",
            [],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
        .unwrap_or_default();
    if ddl.contains("scope IN ('community','language','code')") {
        let _ = db.execute_batch(&crate::clock::freeze_ddl(MIGRATION_SQL));
    }

    let _ = db.execute_batch(&crate::clock::freeze_ddl(SCHEMA_SQL));

    for sql in ALTER_COLUMNS {
        let _ = db.execute_batch(sql);
    }

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
    fn normalize_scope_maps_code_and_unknowns_to_project() {
        assert_eq!(normalizeScope(&json!("business")), json!("business"));
        assert_eq!(normalizeScope(&json!("language")), json!("language"));
        assert_eq!(normalizeScope(&json!("code")), json!("project"));
        assert_eq!(normalizeScope(&json!("nonsense")), json!("project"));
        assert_eq!(normalizeScope(&Value::Null), json!("project"));
    }

    #[test]
    fn creates_full_ledger_idempotently() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments").unwrap();
        assert_eq!(ensureConstitutionSchema(&c), Value::Null);
        assert_eq!(ensureConstitutionSchema(&c), Value::Null); // idempotent
        assert_eq!(
            names(&c, "laws.sqlite_master"),
            vec!["review_law_exceptions", "review_laws"]
        );
        assert_eq!(
            names(&c, "judgments.sqlite_master"),
            vec![
                "review_classifications",
                "review_classifications_law_idx",
                "review_insights",
                "review_papers",
                "review_scores",
                "review_scores_snap_idx",
                "review_snapshots",
                "review_violations",
                "review_violations_snap_idx",
            ]
        );
        assert!(names(&c, "sqlite_master").is_empty()); // main db untouched
    }

    #[test]
    fn self_heals_legacy_check_constraint_and_migrates_code_scope_to_project() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments").unwrap();
        c.execute_batch(
            "CREATE TABLE laws.review_laws (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               scope TEXT NOT NULL CHECK (scope IN ('community','language','code')),
               key TEXT NOT NULL UNIQUE, title TEXT NOT NULL, rule_md TEXT NOT NULL,
               detector TEXT NOT NULL DEFAULT 'none', config_json TEXT NOT NULL DEFAULT '{}',
               weight INTEGER NOT NULL DEFAULT 5, enforcement TEXT NOT NULL DEFAULT 'score',
               applies_json TEXT NOT NULL DEFAULT '{}', status TEXT NOT NULL DEFAULT 'ratified',
               evidence_json TEXT, version INTEGER NOT NULL DEFAULT 1,
               created_by TEXT NOT NULL DEFAULT 'seed',
               created_at TEXT NOT NULL DEFAULT (datetime('now')),
               updated_at TEXT NOT NULL DEFAULT (datetime('now')))",
        )
        .unwrap();
        c.execute(
            "INSERT INTO laws.review_laws (id, scope, key, title, rule_md) VALUES (1, 'code', 'old-law', 'Old Law', 'do it')",
            [],
        )
        .unwrap();
        assert_eq!(ensureConstitutionSchema(&c), Value::Null);
        // migrated: legacy 'code' scope became 'project', row survived the rebuild
        let (scope, key): (String, String) = c
            .query_row("SELECT scope, key FROM laws.review_laws WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(scope, "project");
        assert_eq!(key, "old-law");
        // the CHECK is gone — a scope the old CHECK would have rejected now inserts fine
        c.execute(
            "INSERT INTO laws.review_laws (id, scope, key, title, rule_md) VALUES (2, 'business', 'new-law', 'New Law', 'do it')",
            [],
        )
        .unwrap();
        assert_eq!(names(&c, "laws.sqlite_master"), vec!["review_law_exceptions", "review_laws"]);
    }
}
