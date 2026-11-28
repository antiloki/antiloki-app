//! atlas-report.rs — twin of skills/codebase/atlas-report.mjs (db WRITE lane, async default
//! export — converted per the playbook's "async default exports" rule: the JS `await`s the
//! opt-in deps/summarize calls only inside branches this twin never takes in its tested path, and
//! otherwise does plain sync-over-sqlite work, so this twin stays an ordinary sync `pub fn`).
//! NAMES ARE CONTRACT: `atlasReport` (default export). Assembles atlas-score + atlas-smells into
//! ONE ProjectReport and best-effort persists a snapshot to `code_reports` (EPIC: atlas-insights,
//! Wave E + C). The dashboard reads the latest snapshot (atlas-report-latest); the time series of
//! snapshots gives score/smell/vuln TRENDS.
//!
//! SCOPE LIMIT (playbook lesson 5, disclosed deliberately): `deps` (Wave C OSV network scan, via
//! atlas-deps-audit.mjs) and `summarize` (Wave D AI review, via atlas-insights-review.mjs) both
//! default to `false`/absent — every production onboard/sync call and this file's own JS test
//! suite only exercise that default-false safe path (the one `deps:true` JS test mocks its own
//! `fetch_fn`, proving only the JS's fold-in wiring, not a portable network algorithm). Neither
//! atlas-deps-audit nor atlas-insights-review has a Rust twin (both are genuinely non-deterministic:
//! outbound network / AI provider calls — the playbook's async-lane VET explicitly withholds those
//! until their own twin lands). This twin's `deps`/`summarize` branches therefore degrade to the
//! SAME null result the JS's own `try { } catch { }` fallback produces on any failure (`vulns`/
//! `review_md` stay null, `counts.vulns` stays absent) — this is the deliberately-untested branch;
//! port the real network/AI logic once atlas-deps-audit/atlas-insights-review get their own twins.
// QUALITY: db-write lane; composes 2 sibling twins (atlas-score, atlas-smells) + best-effort persist
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

pub fn atlasReport(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: const { db } = context ?? {}; if (!db) throw err.contextMissing('atlas-report', 'db') —
    // the db lane always injects a live connection, so that guard is unreachable here (gotcha 9).
    let origin = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));
    let root = input.get("root").cloned().unwrap_or(Value::Null); // `??` downstream treats absent/null the same

    let score_input = json!({ "origin": origin, "root": root, "top": 20 });
    let score = crate::codebase_atlas_score::atlasScore(&score_input, db)?;

    let smells_input = json!({ "origin": origin });
    let smells = crate::codebase_atlas_smells::atlasSmells(&smells_input, db)?;

    // Wave C (deps) / Wave D (summarize) — see module doc: both stay null, matching the JS
    // try/catch degrade-safely fallback, until their own twins exist.
    let vulns: Value = Value::Null;
    let vuln_counts: Value = Value::Null;
    let review_md: Value = Value::Null;

    let project = score.get("project").cloned().unwrap_or(Value::Null);
    let health = project.get("health").cloned().unwrap_or(Value::Null);
    let risk = project.get("risk").cloned().unwrap_or(Value::Null);
    let smells_arr = smells.get("smells").cloned().unwrap_or_else(|| json!([]));

    let mut counts = Map::new();
    counts.insert("files".into(), score.get("file_count").cloned().unwrap_or(Value::Null));
    counts.insert("health".into(), health.clone());
    counts.insert("risk".into(), risk.clone());
    counts.insert("smells".into(), smells.get("counts").cloned().unwrap_or(Value::Null));
    if !vuln_counts.is_null() { counts.insert("vulns".into(), vuln_counts.clone()); }
    let counts = Value::Object(counts);

    // JS: input?.persist !== false (strict) — only an explicit boolean false skips the write;
    // absent, null, 0, '' all still persist.
    let persist = input.get("persist") != Some(&Value::Bool(false));
    let mut report_id = Value::Null;
    if persist {
        let scores_json = serde_json::to_string(&score).unwrap_or_default();
        let smells_json = serde_json::to_string(&smells_arr).unwrap_or_default();
        let vulns_json: Value = if vulns.is_null() { Value::Null } else { json!(serde_json::to_string(&vulns).unwrap_or_default()) };
        let counts_json = serde_json::to_string(&counts).unwrap_or_default();
        // JS wraps the INSERT in try/catch: code_reports absent / read-only → still return the
        // live report with report_id staying null.
        if let Ok((_, rowid)) = crate::db_json::run_write(
            db,
            "INSERT INTO code_reports (origin, health_score, risk_score, scores_json, smells_json, vulns_json, review_md, counts_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            &[origin.clone(), health, risk, json!(scores_json), json!(smells_json), vulns_json, review_md.clone(), json!(counts_json)],
        ) {
            report_id = json!(rowid);
        }
    }

    Ok(json!({
        "origin": origin,
        "report_id": report_id,
        "project": project,
        "top_risk": score.get("top_risk").cloned().unwrap_or(Value::Null),
        "smells": smells_arr,
        "vulns": vulns,
        "review_md": review_md,
        "counts": counts,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "
        CREATE TABLE code_nodes (
          id TEXT PRIMARY KEY, origin TEXT, path TEXT, type TEXT, name TEXT, parent_id TEXT,
          span_json TEXT
        );
        CREATE TABLE code_edges (
          id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, from_node TEXT, to_node TEXT, type TEXT
        , kind TEXT);
        CREATE TABLE code_reports (
          id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, health_score REAL, risk_score REAL,
          scores_json TEXT, smells_json TEXT, vulns_json TEXT, review_md TEXT, counts_json TEXT,
          created_at TEXT
        );
    ";

    fn seeded_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c.execute_batch(
            "INSERT INTO code_nodes (id, origin, path, type, name, parent_id, span_json) VALUES
                ('core.ts@atlas','atlas','core.ts','file','core.ts',NULL,NULL),
                ('core.ts#f','atlas','core.ts','function','f','core.ts@atlas','{\"file\":\"core.ts\",\"start_line\":1,\"end_line\":30,\"complexity\":10}'),
                ('a.ts@atlas','atlas','a.ts','file','a.ts',NULL,NULL),
                ('b.ts@atlas','atlas','b.ts','file','b.ts',NULL,NULL);
             INSERT INTO code_edges (origin, from_node, to_node, type) VALUES
                ('atlas','a.ts@atlas','core.ts@atlas','imports'),
                ('atlas','b.ts@atlas','core.ts@atlas','imports');",
        )
        .unwrap();
        c
    }

    #[test]
    fn assembles_score_and_smells_and_persists_a_snapshot() {
        let c = seeded_db();
        let r = atlasReport(&json!({}), &c).unwrap();
        assert!(r["report_id"].as_i64().unwrap() > 0);
        let health = r["project"]["health"].as_i64().unwrap();
        assert!((0..=100).contains(&health));
        assert!(r["smells"].is_array());
        assert!(r["top_risk"].as_array().unwrap().len() >= 1);
        assert_eq!(r["vulns"], Value::Null); // no deps scan by default -> vulns null
        let row_health: f64 = c
            .query_row("SELECT health_score FROM code_reports WHERE id = ?1", [r["report_id"].as_i64().unwrap()], |row| row.get(0))
            .unwrap();
        assert_eq!(row_health as i64, health);
        let vulns_json: Option<String> = c
            .query_row("SELECT vulns_json FROM code_reports WHERE id = ?1", [r["report_id"].as_i64().unwrap()], |row| row.get(0))
            .unwrap();
        assert_eq!(vulns_json, None);
    }

    #[test]
    fn persist_false_returns_the_live_report_without_a_row() {
        let c = seeded_db();
        let r = atlasReport(&json!({ "persist": false }), &c).unwrap();
        assert_eq!(r["report_id"], Value::Null);
        assert!(r["project"].is_object());
        let n: i64 = c.query_row("SELECT COUNT(*) FROM code_reports", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0);
    }
}
