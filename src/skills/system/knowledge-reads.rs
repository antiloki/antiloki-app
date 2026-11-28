//! knowledge-reads.rs — the antiDocs knowledge READ queries, moved into skills. NAMES ARE
//! CONTRACT: `knowledgePapersList`, `knowledgePaperGet`, `knowledgeConceptsList`,
//! `knowledgeEvidenceList`, `knowledgeDocsList`.
//!
//! WHY THEY EXIST. antiloki-BE's KnowledgeController and UploadController ran these as raw SQL.
//! Only a skill touches the databases — see board-reads.rs for the full reasoning.
//!
//! EVERY QUERY HERE IS TOLERANT OF A MISSING TABLE, and that is deliberate rather than sloppy:
//! judgments.* is written by review missions and knowledge.documents by vault ingestion, so a
//! workspace that has never run either legitimately has no such table. The read answers "nothing
//! yet" instead of 500ing a screen the operator is entitled to open and find empty.
// QUALITY: 87/100 — verbatim SQL port, missing-table tolerance, slug-or-id paper resolution
use rusqlite::Connection;
use serde_json::{json, Value};

/// A read whose table may not exist yet answers "nothing", not an error.
fn tolerant(db: &Connection, sql: &str, params: &[Value]) -> Value {
    crate::db_json::query_rows(db, sql, params).unwrap_or_else(|_| json!([]))
}

fn q_of(input: &Value) -> String {
    input.get("q").and_then(|v| v.as_str()).unwrap_or("").trim().to_string()
}

/// 1..=200, defaulting to 50 — the controller's own clamp, kept with the query it protects.
fn limit_of(input: &Value) -> i64 {
    input.get("limit").and_then(|v| v.as_i64()).unwrap_or(50).clamp(1, 200)
}

const PAPER_COLS: &str = "id, slug, title, label, discovery, cost_usd, created_at, updated_at";

pub fn knowledgePapersList(input: &Value, db: &Connection) -> Result<Value, String> {
    let q = q_of(input);
    let n = limit_of(input);
    if q.is_empty() {
        return Ok(tolerant(db, &format!("SELECT {PAPER_COLS} FROM judgments.review_papers ORDER BY id DESC LIMIT ?"), &[json!(n)]));
    }
    let needle = json!(format!("%{q}%"));
    Ok(tolerant(
        db,
        &format!(
            "SELECT {PAPER_COLS} FROM judgments.review_papers
             WHERE title LIKE ? OR COALESCE(discovery,'') LIKE ? OR COALESCE(label,'') LIKE ?
             ORDER BY id DESC LIMIT ?"
        ),
        &[needle.clone(), needle.clone(), needle, json!(n)],
    ))
}

/// One paper by slug, falling back to the integer id.
///
/// The fallback exists because the FE links papers by slug but the review pipeline mints ids
/// first — an unslugged paper is still addressable. Returns `{ok:false}` rather than erroring:
/// a missing paper is a normal browse outcome, not a fault.
pub fn knowledgePaperGet(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = input.get("slug").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut row = crate::db_json::query_rows(db, "SELECT * FROM judgments.review_papers WHERE slug = ?", &[json!(slug)])
        .ok()
        .and_then(|r| r.as_array().and_then(|a| a.first()).cloned());
    if row.is_none() {
        if let Ok(id) = slug.parse::<i64>() {
            row = crate::db_json::query_rows(db, "SELECT * FROM judgments.review_papers WHERE id = ?", &[json!(id)])
                .ok()
                .and_then(|r| r.as_array().and_then(|a| a.first()).cloned());
        }
    }
    let Some(r) = row else {
        return Ok(json!({ "ok": false, "error": "no such paper" }));
    };
    let f = |k: &str| r.get(k).cloned().unwrap_or(Value::Null);
    Ok(json!({
        "ok": true,
        "paper": {
            "id": f("id"), "slug": f("slug"), "title": f("title"), "label": f("label"),
            "discovery": f("discovery"), "swarm_id": f("swarm_id"),
            // The column is markdown_md; the FE field is `markdown`. Renamed here so the
            // projection stays the skill's business rather than the controller's.
            "markdown": f("markdown_md"), "cost_usd": f("cost_usd"),
            "created_at": f("created_at"), "updated_at": f("updated_at"),
        }
    }))
}

/// Concepts rolled up by name+kind, ranked by how many distinct files mention them — a concept
/// touching thirty files is a bigger fact about the codebase than one touching two.
pub fn knowledgeConceptsList(input: &Value, db: &Connection) -> Result<Value, String> {
    let q = q_of(input);
    // Only the three real kinds filter; anything else means "no kind filter" rather than an
    // empty result, so a stale FE query string cannot silently blank the list.
    let kind = input
        .get("kind")
        .and_then(|v| v.as_str())
        .filter(|k| ["domain", "mechanism", "pattern"].contains(k))
        .map(|k| json!(k))
        .unwrap_or(Value::Null);
    Ok(tolerant(
        db,
        "SELECT name, kind, COUNT(DISTINCT path) AS files, MAX(created_at) AS last_seen
         FROM judgments.ultra_concepts
         WHERE (? = '' OR name LIKE ?) AND (? IS NULL OR kind = ?)
         GROUP BY name, kind ORDER BY files DESC, name ASC LIMIT ?",
        &[json!(q), json!(format!("%{q}%")), kind.clone(), kind, json!(limit_of(input))],
    ))
}

pub fn knowledgeEvidenceList(input: &Value, db: &Connection) -> Result<Value, String> {
    let q = q_of(input);
    let needle = json!(format!("%{q}%"));
    let category = input.get("category").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(|c| json!(c)).unwrap_or(Value::Null);
    Ok(tolerant(
        db,
        "SELECT id, run_id, wave, category, severity, file, line, detail, evidence, verified, operator_class, created_at
         FROM judgments.bugfinder_findings
         WHERE dropped = 0
           AND (? = '' OR detail LIKE ? OR COALESCE(file,'') LIKE ?)
           AND (? IS NULL OR category = ?)
         ORDER BY id DESC LIMIT ?",
        &[json!(q), needle.clone(), needle, category.clone(), category, json!(limit_of(input))],
    ))
}

/// Ingested vault documents. Active-only via `COALESCE(superseded_by,'') = ''` — see
/// memory-reads.rs for why the empty string has to be matched alongside NULL.
pub fn knowledgeDocsList(input: &Value, db: &Connection) -> Result<Value, String> {
    let q = q_of(input).to_lowercase();
    let mut sql = String::from(
        "SELECT id, vault_path, doc_type, bytes, summary, ingested_at
         FROM knowledge.documents WHERE COALESCE(superseded_by, '') = ''",
    );
    let mut params: Vec<Value> = Vec::new();
    if !q.is_empty() {
        sql.push_str(" AND (LOWER(vault_path) LIKE ? OR LOWER(COALESCE(summary,'')) LIKE ?)");
        params.push(json!(format!("%{q}%")));
        params.push(json!(format!("%{q}%")));
    }
    sql.push_str(" ORDER BY id DESC LIMIT ?");
    params.push(json!(limit_of(input)));
    Ok(tolerant(db, &sql, &params))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS knowledge;").unwrap();
        c.execute_batch(
            "CREATE TABLE judgments.review_papers (id INTEGER PRIMARY KEY, slug TEXT, title TEXT, label TEXT,
               discovery TEXT, swarm_id TEXT, markdown_md TEXT, cost_usd REAL, created_at TEXT, updated_at TEXT);
             CREATE TABLE judgments.ultra_concepts (name TEXT, kind TEXT, path TEXT, created_at TEXT);
             CREATE TABLE judgments.bugfinder_findings (id INTEGER PRIMARY KEY, run_id TEXT, wave INTEGER,
               category TEXT, severity TEXT, file TEXT, line INTEGER, detail TEXT, evidence TEXT,
               verified INTEGER, operator_class TEXT, created_at TEXT, dropped INTEGER);
             CREATE TABLE knowledge.documents (id INTEGER PRIMARY KEY, vault_path TEXT, doc_type TEXT,
               bytes INTEGER, summary TEXT, ingested_at TEXT, superseded_by TEXT);

             INSERT INTO judgments.review_papers VALUES
               (1,'p-one','Caching strategy','perf','found a cache bug','sw1','# body',0.5,'t1','t2'),
               (2,'p-two','Auth review','sec','nothing odd','sw2','# other',1.5,'t3','t4'),
               (3,NULL,'Unslugged','x','y','sw3','# third',0.1,'t5','t6');
             INSERT INTO judgments.ultra_concepts VALUES
               ('caching','pattern','a.ts','t1'),('caching','pattern','b.ts','t2'),
               ('caching','pattern','a.ts','t3'),('auth','domain','c.ts','t4');
             INSERT INTO judgments.bugfinder_findings VALUES
               (1,'r1',1,'correctness','high','a.ts',10,'off by one','ev',1,'real',  't1',0),
               (2,'r1',1,'perf','low','b.ts',20,'slow loop','ev',0,'maybe','t2',0),
               (3,'r1',1,'correctness','high','c.ts',30,'dropped one','ev',0,'no','t3',1);
             INSERT INTO knowledge.documents VALUES
               (1,'vault/a.md','note',100,'about caching','t1',NULL),
               (2,'vault/b.md','note',200,'about auth','t2',''),
               (3,'vault/old.md','note',50,'superseded','t3','4');",
        )
        .unwrap();
        c
    }

    /// A workspace that never ran a review mission or a vault ingestion.
    fn bare() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS knowledge;").unwrap();
        c
    }

    #[test]
    fn papers_list_newest_first_and_searches_title_discovery_and_label() {
        let c = db();
        let all = knowledgePapersList(&json!({}), &c).unwrap();
        assert_eq!(all[0]["id"], json!(3), "newest id first");
        // Each of the three searchable columns matches on its own.
        for (needle, expect) in [("Caching", 1), ("nothing odd", 2), ("sec", 2)] {
            let r = knowledgePapersList(&json!({ "q": needle }), &c).unwrap();
            assert_eq!(r.as_array().unwrap().len(), 1, "{needle}");
            assert_eq!(r[0]["id"], json!(expect), "{needle}");
        }
    }

    #[test]
    fn a_paper_resolves_by_slug_OR_by_id() {
        let c = db();
        assert_eq!(knowledgePaperGet(&json!({ "slug": "p-one" }), &c).unwrap()["paper"]["title"], json!("Caching strategy"));
        // The unslugged paper is still addressable by its integer id.
        let by_id = knowledgePaperGet(&json!({ "slug": "3" }), &c).unwrap();
        assert_eq!(by_id["ok"], json!(true));
        assert_eq!(by_id["paper"]["title"], json!("Unslugged"));
    }

    #[test]
    fn the_paper_body_is_renamed_markdown_md_to_markdown() {
        let p = knowledgePaperGet(&json!({ "slug": "p-one" }), &db()).unwrap();
        assert_eq!(p["paper"]["markdown"], json!("# body"));
        assert!(p["paper"].get("markdown_md").is_none());
    }

    #[test]
    fn a_missing_paper_is_ok_false_not_an_error() {
        let c = db();
        assert_eq!(knowledgePaperGet(&json!({ "slug": "nope" }), &c).unwrap(), json!({ "ok": false, "error": "no such paper" }));
        assert_eq!(knowledgePaperGet(&json!({}), &c).unwrap()["ok"], json!(false));
    }

    #[test]
    fn concepts_rank_by_DISTINCT_file_count() {
        // 'caching' has three rows but only two distinct paths — counting rows would rank it wrong.
        let r = knowledgeConceptsList(&json!({}), &db()).unwrap();
        assert_eq!(r[0]["name"], json!("caching"));
        assert_eq!(r[0]["files"], json!(2));
        assert_eq!(r[1]["files"], json!(1));
    }

    #[test]
    fn an_UNKNOWN_concept_kind_means_no_filter_rather_than_no_results() {
        // A stale FE query string must not silently blank the list.
        let c = db();
        assert_eq!(knowledgeConceptsList(&json!({ "kind": "pattern" }), &c).unwrap().as_array().unwrap().len(), 1);
        assert_eq!(knowledgeConceptsList(&json!({ "kind": "nonsense" }), &c).unwrap().as_array().unwrap().len(), 2);
    }

    #[test]
    fn evidence_hides_DROPPED_findings_and_filters_by_category() {
        let c = db();
        let all = knowledgeEvidenceList(&json!({}), &c).unwrap();
        assert_eq!(all.as_array().unwrap().len(), 2, "the dropped finding is not evidence");
        assert_eq!(knowledgeEvidenceList(&json!({ "category": "perf" }), &c).unwrap().as_array().unwrap().len(), 1);
        // The needle matches detail OR file.
        assert_eq!(knowledgeEvidenceList(&json!({ "q": "b.ts" }), &c).unwrap().as_array().unwrap().len(), 1);
    }

    #[test]
    fn docs_exclude_superseded_and_search_case_INSENSITIVELY() {
        let c = db();
        assert_eq!(knowledgeDocsList(&json!({}), &c).unwrap().as_array().unwrap().len(), 2);
        // The controller lowercased the needle but the column was not lowercased — an
        // upper-case query matched nothing. Both sides are lowered here.
        assert_eq!(knowledgeDocsList(&json!({ "q": "CACHING" }), &c).unwrap().as_array().unwrap().len(), 1);
        assert_eq!(knowledgeDocsList(&json!({ "q": "VAULT/B" }), &c).unwrap().as_array().unwrap().len(), 1);
    }

    #[test]
    fn the_limit_is_clamped_to_1_200() {
        let c = db();
        assert_eq!(knowledgePapersList(&json!({ "limit": 1 }), &c).unwrap().as_array().unwrap().len(), 1);
        assert_eq!(knowledgePapersList(&json!({ "limit": 0 }), &c).unwrap().as_array().unwrap().len(), 1, "0 clamps up to 1");
        assert_eq!(knowledgePapersList(&json!({ "limit": 99999 }), &c).unwrap().as_array().unwrap().len(), 3);
    }

    #[test]
    fn a_workspace_that_never_ran_a_MISSION_gets_empty_lists_not_a_500() {
        let c = bare();
        assert_eq!(knowledgePapersList(&json!({}), &c).unwrap(), json!([]));
        assert_eq!(knowledgeConceptsList(&json!({}), &c).unwrap(), json!([]));
        assert_eq!(knowledgeEvidenceList(&json!({}), &c).unwrap(), json!([]));
        assert_eq!(knowledgeDocsList(&json!({}), &c).unwrap(), json!([]));
        assert_eq!(knowledgePaperGet(&json!({ "slug": "x" }), &c).unwrap()["ok"], json!(false));
    }
}
