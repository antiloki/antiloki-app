//! notebook-schema.rs — twin of skills/notebooks/notebook-schema.mjs. NAMES ARE CONTRACT:
//! `notebookSchemaSql` (no-db), `ensureNotebookSchema` (db-first) + `NOTEBOOK_SCHEMA` (data).
// QUALITY: 80/100 — the notebooks DDL: a fixed template with a ${schema} prefix at 5 points.
// datetime('now') stays LITERAL in the returned text (it is emitted, not executed) — no frozen-clock
// rewrite here; ensureNotebookSchema just runs the CREATE-IF-NOT-EXISTS batch (no rows → no default fires).
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::LazyLock;

const SEG0: &str = r#"
-- one notebook: a saved research surface bound to a scope of the workspace
CREATE TABLE IF NOT EXISTS "#;
const SEG1: &str = r#"notebooks (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  slug        TEXT,
  title       TEXT NOT NULL,
  summary     TEXT,                                -- optional one-liner
  scope_json  TEXT NOT NULL DEFAULT '{}',          -- { paths:[], node_ids:[], deps?, dependents?, dbs?:[], origin? }
  created_at  TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at  TEXT
);
-- the notebook thread: grounded answers, generated artifacts, and notes/pins
CREATE TABLE IF NOT EXISTS "#;
const SEG2: &str = r#"notebook_items (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  notebook_id INTEGER NOT NULL,
  kind        TEXT NOT NULL DEFAULT 'note',        -- ask | artifact | note
  title       TEXT,
  body_md     TEXT NOT NULL DEFAULT '',
  meta_json   TEXT,                                -- { question?, artifact_kind?, citations:[], cost_usd, ai_call_id, tier }
  pinned      INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS "#;
const SEG3: &str = r#"idx_notebook_items ON notebook_items (notebook_id, created_at);
-- the SOURCES of a notebook: a rich, multi-kind grounded corpus (code, AST, digest, entry-points, data-stories,
-- descriptions, design, tests, git, plus imported files / pasted text / urls). enabled rows feed ask/generate.
CREATE TABLE IF NOT EXISTS "#;
const SEG4: &str = r#"notebook_sources (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  notebook_id INTEGER NOT NULL,
  kind        TEXT NOT NULL,                       -- code_paths|ast|digest|entrypoints|data_stories|descriptions|design|tests|user_stories|git|file|url|text|source|paper
  ref         TEXT,                                -- selector: a path / url / src-id (sources) / null (scope lives on the notebook)
  label       TEXT,                                -- display name
  body_md     TEXT,                                -- ingested text for file/text/url sources
  enabled     INTEGER NOT NULL DEFAULT 1,
  meta_json   TEXT,
  created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS "#;
const SEG5: &str = r#"idx_notebook_sources ON notebook_sources (notebook_id);
"#;

// The `${schema}` template: 5 interpolation points, byte-identical to the JS literal. Only strings
// reach this in the fixtures; JS default `schema = ''` maps undefined/absent → "", matched by unwrap_or("").
fn schema_sql(schema: &str) -> String {
    [SEG0, schema, SEG1, schema, SEG2, schema, SEG3, schema, SEG4, schema, SEG5].concat()
}

pub fn notebookSchemaSql(schema: &Value) -> Value {
    json!(schema_sql(schema.as_str().unwrap_or("")))
}

// JS `ensureNotebookSchema(db, schema='')` runs the DDL and returns undefined (→ null). Idempotent
// CREATE IF NOT EXISTS. FROZEN-CLOCK MIRROR: the conform JS lane's db proxy rewrites executed
// 'now' to the frozen instant, so when a LATER twin inserts into these tables the created_at
// DEFAULT fires frozen there — the EXECUTED copy pins the instant to match (cockpit-schema
// precedent: cockpitBlackboardPost divergence). The returned string stays verbatim.
pub fn ensureNotebookSchema(db: &Connection, schema: &Value) -> Value {
    let _ = db.execute_batch(&schema_sql(schema.as_str().unwrap_or("")).replace("datetime('now'", &format!("datetime({}", crate::clock::now_sql_inner())));
    Value::Null
}

// NOTEBOOK_SCHEMA = notebookSchemaSql('') — the base-schema string, proven by the const lane (ε0).
pub static NOTEBOOK_SCHEMA: LazyLock<Value> = LazyLock::new(|| json!(schema_sql("")));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_sql_shape_and_prefix() {
        let base = notebookSchemaSql(&json!(""));
        let s = base.as_str().unwrap();
        assert_eq!(s.chars().count(), 2156); // byte/char-exact with the JS literal
        assert!(s.starts_with("\n-- one notebook:"));
        assert!(s.contains("CREATE TABLE IF NOT EXISTS notebooks ("));
        assert!(s.contains("CREATE TABLE IF NOT EXISTS notebook_items ("));
        assert!(s.contains("CREATE TABLE IF NOT EXISTS notebook_sources ("));
        assert!(s.contains("CREATE INDEX IF NOT EXISTS idx_notebook_items ON notebook_items"));
        assert!(s.contains("CREATE INDEX IF NOT EXISTS idx_notebook_sources ON notebook_sources"));
        assert!(s.contains("DEFAULT (datetime('now'))")); // literal, NOT frozen — it is emitted text

        // ${schema} interpolates at exactly the 5 name positions
        let pre = notebookSchemaSql(&json!("app_"));
        let p = pre.as_str().unwrap();
        assert_eq!(p.chars().count(), 2156 + 4 * 5);
        assert!(p.contains("CREATE TABLE IF NOT EXISTS app_notebooks ("));
        assert!(p.contains("CREATE INDEX IF NOT EXISTS app_idx_notebook_items ON notebook_items"));

        // const twin equals the empty-prefix build
        assert_eq!(&*NOTEBOOK_SCHEMA, &base);
    }

    #[test]
    fn ensure_creates_the_three_tables() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(ensureNotebookSchema(&c, &json!("")), Value::Null);
        let names: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(names, vec!["notebook_items", "notebook_sources", "notebooks"]);
        // idempotent — running again does not error
        assert_eq!(ensureNotebookSchema(&c, &json!("")), Value::Null);
    }
}
