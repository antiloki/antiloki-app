//! memory-reads.rs — the BE's memory READ queries, moved into skills. NAMES ARE CONTRACT:
//! `memoryBrowseList`, `memoryGraphFull`, `memoryGetOne`, `memoryCountsByType`.
//!
//! WHY THEY EXIST. antiloki-BE's MemoryService ran these as raw SQL against memory.memories.
//! Only a skill touches the databases — see board-reads.rs for the full reasoning.
//!
//! `system.memory.list` and `system.memory.graph` already exist as actions, but they answer
//! different questions (the RAG-shaped list, the narrative graph). These four are the sidebar's
//! own projections, and quietly repointing the FE at a differently-shaped action would be a
//! behaviour change wearing a refactor's clothes.
//!
//! ACTIVE-ONLY is `COALESCE(superseded_by, '') = ''`, not `superseded_by IS NULL`, because both
//! NULL and the empty string occur in practice — a memory superseded and then un-superseded by an
//! older code path leaves '' behind. Matching only NULL silently drops those rows from the
//! sidebar while they stay live everywhere else.
// QUALITY: 88/100 — verbatim SQL port, legacy-links tolerance, dangling-edge filter
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

const ACTIVE: &str = "COALESCE(superseded_by, '') = ''";

pub fn memoryBrowseList(input: &Value, db: &Connection) -> Result<Value, String> {
    let mut sql = format!(
        "SELECT id, type, title, situation, guidance, rationale,
                tags_json, validated, confidence, created_by, created_at
         FROM memory.memories WHERE {ACTIVE}"
    );
    let mut params: Vec<Value> = Vec::new();
    if let Some(t) = input.get("type").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        sql.push_str(" AND type = ?");
        params.push(json!(t));
    }
    sql.push_str(" ORDER BY id DESC LIMIT ? OFFSET ?");
    params.push(json!(input.get("limit").and_then(|v| v.as_i64()).filter(|&n| n > 0).unwrap_or(30)));
    params.push(json!(input.get("offset").and_then(|v| v.as_i64()).filter(|&n| n >= 0).unwrap_or(0)));
    crate::db_json::query_rows(db, &sql, &params)
}

/// The whole memory graph for the graph-RAG visualization: active memories as nodes, plus every
/// memory_links edge BETWEEN TWO ACTIVE memories.
///
/// Edges are filtered rather than joined so a link pointing at a superseded memory disappears
/// instead of dragging a dead node back into the picture. A legacy database with no links table
/// degrades to an unlinked node cloud — the graph is still useful without edges, and a missing
/// table should not blank the whole view.
pub fn memoryGraphFull(_input: &Value, db: &Connection) -> Result<Value, String> {
    let nodes = crate::db_json::query_rows(
        db,
        &format!(
            "SELECT id, type, title, confidence, validated, created_by, created_at
             FROM memory.memories WHERE {ACTIVE}"
        ),
        &[],
    )?;
    let ids: HashSet<i64> = nodes
        .as_array()
        .map(|a| a.iter().filter_map(|n| n.get("id").and_then(|v| v.as_i64())).collect())
        .unwrap_or_default();

    let edges = match crate::db_json::query_rows(
        db,
        "SELECT from_memory_id AS \"from\", to_memory_id AS \"to\", kind, weight, rationale
         FROM memory.memory_links",
        &[],
    ) {
        Err(_) => json!([]),
        Ok(rows) => Value::Array(
            rows.as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|e| {
                    let end = |k: &str| e.get(k).and_then(|v| v.as_i64()).is_some_and(|i| ids.contains(&i));
                    end("from") && end("to")
                })
                .collect(),
        ),
    };
    Ok(json!({ "nodes": nodes, "edges": edges }))
}

pub fn memoryGetOne(input: &Value, db: &Connection) -> Result<Value, String> {
    let id = input
        .get("id")
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .filter(|&n| n > 0)
        .ok_or("memory-get-one: id is required")?;
    crate::db_json::query_rows(db, "SELECT * FROM memory.memories WHERE id = ?", &[json!(id)])?
        .as_array()
        .and_then(|a| a.first())
        .cloned()
        // "not found" is the wording the BE maps onto a 404.
        .ok_or_else(|| format!("memory-get-one: memory #{id} not found"))
}

pub fn memoryCountsByType(_input: &Value, db: &Connection) -> Result<Value, String> {
    crate::db_json::query_rows(
        db,
        &format!(
            "SELECT type, COUNT(*) AS n FROM memory.memories
             WHERE {ACTIVE} GROUP BY type ORDER BY type"
        ),
        &[],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory;").unwrap();
        c.execute_batch(
            "CREATE TABLE memory.memories (
               id INTEGER PRIMARY KEY, type TEXT, title TEXT, situation TEXT, guidance TEXT,
               rationale TEXT, tags_json TEXT, validated INTEGER, confidence REAL,
               created_by TEXT, created_at TEXT, superseded_by TEXT
             );
             CREATE TABLE memory.memory_links (from_memory_id INTEGER, to_memory_id INTEGER,
                                               kind TEXT, weight REAL, rationale TEXT);
             INSERT INTO memory.memories (id,type,title,validated,confidence,created_by,created_at,superseded_by) VALUES
               (1,'decision','first',1,0.9,'claude','t1',NULL),
               (2,'correction','second',0,0.5,'dani','t2',''),
               (3,'decision','third',1,0.7,'claude','t3',NULL),
               (4,'decision','retired',1,0.1,'claude','t4','5');
             INSERT INTO memory.memory_links VALUES (1,3,'supports',0.8,'because'),
                                                    (1,4,'supports',0.4,'points at a dead node');",
        )
        .unwrap();
        c
    }

    #[test]
    fn list_excludes_superseded_and_orders_newest_first() {
        let r = memoryBrowseList(&json!({}), &db()).unwrap();
        let ids: Vec<i64> = r.as_array().unwrap().iter().map(|m| m["id"].as_i64().unwrap()).collect();
        assert_eq!(ids, [3, 2, 1], "the superseded memory is not listed");
    }

    #[test]
    fn EMPTY_STRING_superseded_by_still_counts_as_active() {
        // Both NULL and '' occur in practice; matching only NULL would silently drop memory #2
        // from the sidebar while it stays live everywhere else.
        let r = memoryBrowseList(&json!({}), &db()).unwrap();
        assert!(r.to_string().contains("second"));
    }

    #[test]
    fn list_filters_by_type_and_paginates() {
        let c = db();
        assert_eq!(memoryBrowseList(&json!({ "type": "decision" }), &c).unwrap().as_array().unwrap().len(), 2);
        let page = memoryBrowseList(&json!({ "limit": 1, "offset": 1 }), &c).unwrap();
        assert_eq!(page.as_array().unwrap().len(), 1);
        assert_eq!(page[0]["id"], json!(2), "offset walks the DESC order");
    }

    #[test]
    fn the_graph_DROPS_edges_that_point_at_a_superseded_memory() {
        // Otherwise a retired memory is dragged back into the picture by its inbound link.
        let g = memoryGraphFull(&json!({}), &db()).unwrap();
        assert_eq!(g["nodes"].as_array().unwrap().len(), 3);
        let edges = g["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0]["from"], json!(1));
        assert_eq!(edges[0]["to"], json!(3));
    }

    #[test]
    fn a_LEGACY_db_with_no_links_table_degrades_to_an_unlinked_cloud() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory;").unwrap();
        c.execute_batch(
            "CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT, title TEXT,
               confidence REAL, validated INTEGER, created_by TEXT, created_at TEXT, superseded_by TEXT);
             INSERT INTO memory.memories VALUES (1,'decision','only',0.5,1,'x','t',NULL);",
        )
        .unwrap();
        let g = memoryGraphFull(&json!({}), &c).unwrap();
        assert_eq!(g["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(g["edges"], json!([]), "a missing links table must not blank the view");
    }

    #[test]
    fn get_one_returns_the_row_and_says_NOT_FOUND_otherwise() {
        let c = db();
        assert_eq!(memoryGetOne(&json!({ "id": 1 }), &c).unwrap()["title"], json!("first"));
        // A superseded memory is still directly addressable — the detail view must open it.
        assert_eq!(memoryGetOne(&json!({ "id": 4 }), &c).unwrap()["title"], json!("retired"));
        assert!(memoryGetOne(&json!({ "id": 999 }), &c).unwrap_err().contains("not found"));
        assert!(memoryGetOne(&json!({}), &c).unwrap_err().contains("id is required"));
    }

    #[test]
    fn counts_group_active_memories_by_type() {
        let r = memoryCountsByType(&json!({}), &db()).unwrap();
        assert_eq!(r, json!([{ "type": "correction", "n": 1 }, { "type": "decision", "n": 2 }]));
    }
}
