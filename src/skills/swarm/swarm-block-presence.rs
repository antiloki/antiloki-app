//! swarm-block-presence.rs — twin of skills/swarm/swarm-block-presence.mjs (db-first helper). NAMES
//! ARE CONTRACT: `authoredBlockCount` (named). Count authored blocks on a contract's active manifest.
// QUALITY: 83/100 — active-manifest lookup + block count; table-absent/empty → 0 (flag-independent)
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn authoredBlockCount(db: &Connection, contract_slug: &str) -> Value {
    if contract_slug.is_empty() { return json!(0); }
    let man = match crate::db_json::query_rows(db,
        "SELECT id FROM swarm_manifests WHERE contract_slug = ? AND superseded_by IS NULL ORDER BY version DESC, id DESC LIMIT 1",
        &[json!(contract_slug)]) {
        Ok(r) => r,
        Err(_) => return json!(0), // tables absent on a legacy DB
    };
    let man_id = match man.as_array().and_then(|a| a.first()).and_then(|r| r.get("id")) {
        Some(v) => v.clone(), None => return json!(0),
    };
    match crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM swarm_blocks WHERE manifest_id = ?", &[man_id]) {
        Ok(r) => json!(r.as_array().and_then(|a| a.first()).and_then(|x| x.get("n")).and_then(|v| v.as_i64()).unwrap_or(0)),
        Err(_) => json!(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_blocks() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE swarm_manifests (id INTEGER, contract_slug TEXT, superseded_by TEXT, version INTEGER); \
          CREATE TABLE swarm_blocks (manifest_id INTEGER); \
          INSERT INTO swarm_manifests VALUES (5,'CT-1',NULL,2),(4,'CT-1','x',1); \
          INSERT INTO swarm_blocks VALUES (5),(5),(5);").unwrap();
        assert_eq!(authoredBlockCount(&c, "CT-1"), json!(3)); // active manifest 5 has 3 blocks
        assert_eq!(authoredBlockCount(&c, "NOPE"), json!(0));
        assert_eq!(authoredBlockCount(&c, ""), json!(0));
    }
}
