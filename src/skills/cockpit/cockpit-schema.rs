//! cockpit-schema.rs — twin of skills/cockpit/cockpit-schema.mjs (helper module). NAMES ARE
//! CONTRACT: cockpitSchemaSql (no-db) · ensureCockpitSchema(db) (db-first). EPIC-VIBE-COCKPIT DDL.
// QUALITY: 80/100 — self-healing cockpit schema: CREATE block + idempotent column-add ALTERs (swallowed)
use rusqlite::Connection;
use serde_json::{json, Value};

// The EPIC-VIBE-COCKPIT DDL template. `${schema}` is a table-name prefix ('' or e.g. 'main.').
// JS template literals normalize CRLF->LF (ES spec) so the JS return is pure-LF; we strip any CRLF
// this .rs may carry on disk before substituting, so the twin emits identical bytes on every platform.
const COCKPIT_TEMPLATE: &str = r#"
-- one vibe session: the container for N panes, branched off a known base so drift is detectable
CREATE TABLE IF NOT EXISTS ${schema}cockpit_sessions (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  name        TEXT NOT NULL,
  status      TEXT NOT NULL DEFAULT 'active',     -- active | integrating | merged | abandoned
  mission_md  TEXT,                                -- the shared, read-only mission brief
  base_branch TEXT NOT NULL DEFAULT '',            -- where panes branch from + merge back to
  base_sha    TEXT NOT NULL DEFAULT '',            -- HEAD snapshot at session start
  mode        TEXT NOT NULL DEFAULT 'steer',       -- steer (<=4 active panes) | monitor (<=8)
  created_at  TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at  TEXT
);
-- one pane = one agent working in isolation (a scoped sub-agent bound to a worktree)
CREATE TABLE IF NOT EXISTS ${schema}cockpit_panes (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id    INTEGER NOT NULL,
  idx           INTEGER NOT NULL,                  -- pane position 0..7
  subagent_name TEXT NOT NULL DEFAULT '',          -- the bound sub-agent identity
  scope_json    TEXT NOT NULL DEFAULT '[]',        -- owned globs (STRICT, non-overlapping) e.g. ["src/ui/**"]
  grants_json   TEXT NOT NULL DEFAULT '[]',        -- action allowlist subset for this pane
  worktree_path TEXT,                              -- .antiloki/worktrees/cockpit-<session>-<idx>
  branch        TEXT,                              -- antiloki/cockpit/<session>/<idx>
  tier          TEXT NOT NULL DEFAULT 'low',       -- Helper default; escalate per pane
  base_agent    TEXT,                              -- base IDENTITY: an existing system agent whose persona this pane inherits
  extra_prompt_md TEXT,                            -- operator's extra prompt, appended to the base persona for this session
  model_id      TEXT,                              -- the specific AI within the tier (BYOK model id); NULL = the tier router picks
  calls         INTEGER NOT NULL DEFAULT 0,        -- AI turns this pane has run (the cost meter)
  cost_usd      REAL NOT NULL DEFAULT 0,           -- accumulated BYOK spend for this pane (metered)
  status        TEXT NOT NULL DEFAULT 'idle',      -- idle | working | blocked | merged | error
  last_activity TEXT,
  UNIQUE (session_id, idx)
);
-- the blackboard: shared brief + per-pane private scratch + an append-only coordination log
CREATE TABLE IF NOT EXISTS ${schema}cockpit_blackboard (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id INTEGER NOT NULL,
  pane_id    INTEGER,                              -- NULL = shared (coordination); set = that pane's private scratch
  kind       TEXT NOT NULL DEFAULT 'note',         -- claim | note | handoff | scratch | decision
  body_md    TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS ${schema}idx_cockpit_bb ON cockpit_blackboard (session_id, pane_id, created_at);
-- explicit file claims → runtime non-overlap enforcement (a pane can't claim what another already holds)
CREATE TABLE IF NOT EXISTS ${schema}cockpit_claims (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id INTEGER NOT NULL,
  pane_id    INTEGER NOT NULL,
  path_glob  TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
-- PULL REQUESTS: a pane's branch proposed for merge into base (the "integrate" reframed). Carries the diff, the
-- mergeability/conflict preview, and the smart analysis (AI summary + blast-radius). One PR per (session, branch).
CREATE TABLE IF NOT EXISTS ${schema}cockpit_pull_requests (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id     INTEGER NOT NULL,
  pane_id        INTEGER,
  idx            INTEGER,
  agent          TEXT,
  branch         TEXT NOT NULL,
  base_branch    TEXT NOT NULL,
  source         TEXT NOT NULL DEFAULT 'vibe',     -- vibe (cockpit pane) | driver (card worktree)
  ref_slug       TEXT,                             -- the card/contract slug, for driver PRs
  title          TEXT,
  status         TEXT NOT NULL DEFAULT 'open',     -- open | merged | closed
  changed        INTEGER NOT NULL DEFAULT 0,        -- changed-file count
  mergeable      INTEGER,                           -- 1 clean · 0 conflicts · NULL unknown
  conflicts_json TEXT,                              -- conflicted file paths
  patch          TEXT,                              -- the diff (base..branch)
  summary_md     TEXT,                              -- AI analysis / documentation (short, for the list)
  analysis_json  TEXT,                              -- the TEMPLATED high-tier analysis (fixed fields, JSON)
  blast_json     TEXT,                              -- reverse-dependency impact (blast radius)
  created_at     TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at     TEXT,
  UNIQUE (session_id, branch)
);
-- COURT SESSIONS: the full history of tribunal rulings over a session's ultra
-- plan (operator 2026-07-16: "the court sessions should be saved at the table …
-- complete history … be able to retrieve"). APPEND-ONLY — one row per convene.
CREATE TABLE IF NOT EXISTS ${schema}cockpit_tribunals (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id INTEGER NOT NULL,
  verdict    TEXT,
  headline   TEXT,
  case_json  TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS ${schema}idx_cockpit_tribunals ON cockpit_tribunals (session_id, created_at);
-- ULTRA PLANS: the first-class plan entity tying its three artifacts together
-- (operator 2026-07-16: "the ultra plan must have those 3: brainstorm chips,
-- shape and court connected and saved"). One active plan per session (upserted
-- on forge); the shape row's JSON + court sessions carry its id.
CREATE TABLE IF NOT EXISTS ${schema}ultra_plans (
  id              INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id      INTEGER NOT NULL,
  title           TEXT,
  brainstorm_json TEXT,                             -- the intent wizard's dimensions + picked chips (durable)
  baseplan_md     TEXT,                             -- the forged base plan
  blueprint_json  TEXT,                             -- the CODE blueprint (skeleton + stubs + planned graph + delta)
  created_at      TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at      TEXT
);
CREATE INDEX IF NOT EXISTS ${schema}idx_ultra_plans_session ON ultra_plans (session_id, updated_at);
"#;

// JS `${schema}`: string -> itself; the default param '' covers the absent arg (harness Null).
fn schema_prefix(v: &Value) -> String {
    match v { Value::String(s) => s.clone(), Value::Null => String::new(), o => o.to_string() }
}

fn cockpit_schema_sql_str(schema: &str) -> String {
    COCKPIT_TEMPLATE.replace("\r\n", "\n").replace("${schema}", schema)
}

pub fn cockpitSchemaSql(schema: &Value) -> Value {
    json!(cockpit_schema_sql_str(&schema_prefix(schema)))
}

// Mirrors JS: exec the CREATE block, then each ALTER in a swallow-errors loop (column already
// present -> harmless). Returns undefined in JS -> the db lane records null, so the twin returns Null.
pub fn ensureCockpitSchema(db: &Connection, schema: &Value) -> Value {
    let s = schema_prefix(schema);
    // FROZEN-CLOCK MIRROR: the conform JS lane rewrites executed 'now' to the frozen instant via
    // its db proxy, so column DEFAULTs (created_at) fire frozen there. The returned DDL string
    // (cockpitSchemaSql) stays verbatim; only the EXECUTED copy pins the instant to match.
    let _ = db.execute_batch(&crate::clock::freeze_ddl(&cockpit_schema_sql_str(&s)));
    for sql in [
        format!("ALTER TABLE {s}cockpit_panes ADD COLUMN base_agent TEXT"),
        format!("ALTER TABLE {s}cockpit_panes ADD COLUMN extra_prompt_md TEXT"),
        format!("ALTER TABLE {s}cockpit_panes ADD COLUMN model_id TEXT"),
        format!("ALTER TABLE {s}cockpit_panes ADD COLUMN calls INTEGER NOT NULL DEFAULT 0"),
        format!("ALTER TABLE {s}cockpit_panes ADD COLUMN cost_usd REAL NOT NULL DEFAULT 0"),
        format!("ALTER TABLE {s}cockpit_pull_requests ADD COLUMN source TEXT NOT NULL DEFAULT 'vibe'"),
        format!("ALTER TABLE {s}cockpit_pull_requests ADD COLUMN ref_slug TEXT"),
        format!("ALTER TABLE {s}cockpit_pull_requests ADD COLUMN analysis_json TEXT"),
        format!("ALTER TABLE {s}cockpit_panes ADD COLUMN auto_apply INTEGER NOT NULL DEFAULT 0"),
        format!("ALTER TABLE {s}cockpit_panes ADD COLUMN pending_edits_json TEXT"),
        format!("ALTER TABLE {s}cockpit_panes ADD COLUMN pane_kind TEXT NOT NULL DEFAULT 'agent'"),
        format!("ALTER TABLE {s}cockpit_panes ADD COLUMN script_kind TEXT"),
        format!("ALTER TABLE {s}cockpit_panes ADD COLUMN script_worktree_pane INTEGER"),
        format!("ALTER TABLE {s}cockpit_panes ADD COLUMN span_cols INTEGER NOT NULL DEFAULT 1"),
        format!("ALTER TABLE {s}cockpit_panes ADD COLUMN span_rows INTEGER NOT NULL DEFAULT 1"),
        format!("ALTER TABLE {s}cockpit_panes ADD COLUMN chat_session_id INTEGER"),
        format!("ALTER TABLE {s}cockpit_panes ADD COLUMN context_excludes_json TEXT"),
        format!("ALTER TABLE {s}cockpit_panes ADD COLUMN settings_json TEXT"),
        format!("ALTER TABLE {s}cockpit_sessions ADD COLUMN board_json TEXT"),
        format!("ALTER TABLE {s}cockpit_tribunals ADD COLUMN ultra_plan_id INTEGER"),
        format!("ALTER TABLE {s}ultra_plans ADD COLUMN blueprint_json TEXT"),
    ] {
        let _ = db.execute_batch(&sql);
    }
    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schema_sql_interpolates() {
        let empty = cockpitSchemaSql(&json!(""));
        let s = empty.as_str().unwrap();
        assert!(s.starts_with("\n-- one vibe session"));
        assert!(s.ends_with("updated_at);\n"));
        assert!(s.contains("CREATE TABLE IF NOT EXISTS cockpit_sessions ("));
        assert!(!s.contains("${schema}"));
        assert!(!s.contains('\r'));
        let pre = cockpitSchemaSql(&json!("main."));
        let p = pre.as_str().unwrap();
        assert!(p.contains("CREATE TABLE IF NOT EXISTS main.cockpit_sessions ("));
        assert!(p.contains("CREATE INDEX IF NOT EXISTS main.idx_ultra_plans_session"));
        assert_eq!(p.matches("main.").count(), 10);
    }
    #[test]
    fn ensure_creates_and_heals() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(ensureCockpitSchema(&c, &json!("")), Value::Null);
        let names = crate::db_json::query_rows(&c, "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name", &[]).unwrap();
        let got: Vec<String> = names.as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(got, vec!["cockpit_blackboard", "cockpit_claims", "cockpit_panes", "cockpit_pull_requests", "cockpit_sessions", "cockpit_tribunals", "ultra_plans"]);
        let cols = crate::db_json::query_rows(&c, "SELECT name FROM pragma_table_info('cockpit_panes')", &[]).unwrap();
        let colset: Vec<String> = cols.as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap().to_string()).collect();
        assert!(colset.iter().any(|c| c == "auto_apply"));
        assert!(colset.iter().any(|c| c == "settings_json"));
        // idempotent: a second heal does not error
        assert_eq!(ensureCockpitSchema(&c, &json!("")), Value::Null);
    }
}
