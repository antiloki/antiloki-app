//! sources-schema.rs — twin of skills/sources/sources-schema.mjs. NAMES ARE CONTRACT:
//! `ensureSourcesSchema` (db-first helper, `p` defaults to 'knowledge.').
// QUALITY: 80/100 — the `sources` index DDL (knowledge.sqlite): a fixed template with a ${p}
// prefix at 2 name positions. The SQL is EXECUTED (never returned), so per the frozen-clock
// mirror the executed copy pins datetime('now') to the frozen instant — the conform JS lane's
// db proxy rewrites executed SQL, so the added_at DEFAULT fires frozen there; when a later
// twin INSERTs into sources, an unfrozen Rust DEFAULT would diverge on db_after
// (cockpitBlackboardPost precedent).
use rusqlite::Connection;
use serde_json::Value;

const SEG0: &str = r#"
    CREATE TABLE IF NOT EXISTS "#;
const SEG1: &str = r#"sources (
      id        TEXT    PRIMARY KEY,           -- 'src-' + sha256(content)[:12] (content-addressed)
      filename  TEXT    NOT NULL,
      rel_path  TEXT    NOT NULL UNIQUE,        -- under .antiloki/sources/
      ext       TEXT,
      mime      TEXT,
      sha256    TEXT    NOT NULL,
      bytes     INTEGER NOT NULL,
      text      TEXT,                           -- extracted text for md/txt/code; NULL for binary
      title     TEXT,
      added_at  TEXT    NOT NULL DEFAULT (datetime('now')),
      added_by  TEXT    NOT NULL DEFAULT 'operator'
    );
    CREATE INDEX IF NOT EXISTS "#;
const SEG2: &str = r#"idx_sources_added ON sources(added_at DESC);
  "#;

// The `${p}` template: 2 interpolation points (table name + index name), same shape as the JS
// literal. The index is name-qualified but `ON sources` stays bare — SQLite resolves the table
// in the index's own database, so the qualified default targets knowledge.sources correctly.
fn schema_sql(p: &str) -> String {
    [SEG0, p, SEG1, p, SEG2].concat()
}

// JS `ensureSourcesSchema(db, p = 'knowledge.')` execs the DDL and returns undefined (→ null).
// Idempotent CREATE IF NOT EXISTS. Missing/null arg → the JS default 'knowledge.' (only string
// prefixes reach this in fixtures, matching the notebook-schema unwrap_or precedent).
pub fn ensureSourcesSchema(db: &Connection, p: &Value) -> Value {
    let _ = db.execute_batch(
        &schema_sql(p.as_str().unwrap_or("knowledge."))
            .replace("datetime('now'", &format!("datetime({}", crate::clock::now_sql_inner())),
    );
    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn empty_prefix_creates_main_sources_idempotently() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(ensureSourcesSchema(&c, &json!("")), Value::Null);
        assert_eq!(names(&c, "sqlite_master"), vec!["idx_sources_added", "sources"]);
        // idempotent — a second run does not error
        assert_eq!(ensureSourcesSchema(&c, &json!("")), Value::Null);
        // the executed copy is frozen: an INSERT's added_at DEFAULT fires at the pinned instant
        c.execute(
            "INSERT INTO sources (id, filename, rel_path, sha256, bytes) VALUES ('src-abc', 'a.md', 'sources/a.md', 'deadbeef', 5)",
            [],
        )
        .unwrap();
        let (added_at, added_by): (String, String) = c
            .query_row("SELECT added_at, added_by FROM sources WHERE id='src-abc'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(added_at, "2026-01-01 00:00:00");
        assert_eq!(added_by, "operator");
    }

    #[test]
    fn default_prefix_targets_attached_knowledge_db() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS knowledge").unwrap();
        // missing arg (Value::Null) → the JS default 'knowledge.'
        assert_eq!(ensureSourcesSchema(&c, &Value::Null), Value::Null);
        assert_eq!(names(&c, "knowledge.sqlite_master"), vec!["idx_sources_added", "sources"]);
        // main db untouched
        assert!(names(&c, "sqlite_master").is_empty());
    }
}
