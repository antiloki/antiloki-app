//! driver-parallel-plan.rs — twin of skills/driver/driver-parallel-plan.mjs (db lane). NAMES ARE
//! CONTRACT: `driverParallelPlan` (default) + `fileOverlap` (named, no-db helper). Pack non-overlapping cards.
// QUALITY: 82/100 — greedy non-overlap grouping, latest-contract files, fileOverlap helper via sig lane
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

// no-db helper: do two file-lists share any path?
pub fn fileOverlap(files_a: &Value, files_b: &Value) -> Value {
    let set_b: HashSet<String> = files_b.as_array().map(|b| b.iter().map(|v| v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())).collect()).unwrap_or_default();
    let empty = vec![];
    let a = files_a.as_array().unwrap_or(&empty);
    json!(a.iter().any(|f| set_b.contains(&f.as_str().map(|s| s.to_string()).unwrap_or_else(|| f.to_string()))))
}

pub fn driverParallelPlan(input: &Value, db: &Connection) -> Result<Value, String> {
    let max_parallel = input.get("max_parallel").and_then(|v| v.as_i64()).unwrap_or(4);
    let candidate = input.get("candidate_slugs").and_then(|v| v.as_array()).filter(|a| !a.is_empty());
    let slugs: Vec<String> = if let Some(cs) = candidate {
        cs.iter().map(|v| v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())).collect()
    } else {
        crate::db_json::query_rows(db, "SELECT slug FROM cards WHERE status IN ('doing','impl','qa') ORDER BY rowid", &[])?
            .as_array().unwrap().iter().filter_map(|r| r.get("slug").and_then(|v| v.as_str()).map(|s| s.to_string())).collect()
    };

    // (card_slug, contract_slug, files)
    let mut enriched: Vec<(String, Value, Value)> = Vec::new();
    for slug in &slugs {
        let c = crate::db_json::query_rows(db, "SELECT slug, files_touched_json FROM contracts WHERE card_slug = ? ORDER BY id DESC LIMIT 1", &[json!(slug)])?;
        let c0 = match c.as_array().and_then(|a| a.first()) { Some(r) => r.clone(), None => continue };
        let files = match c0.get("files_touched_json").and_then(|v| v.as_str()) {
            Some(s) => serde_json::from_str::<Value>(s).unwrap_or(json!([])),
            None => json!([]),
        };
        enriched.push((slug.clone(), c0.get("slug").cloned().unwrap_or(Value::Null), files));
    }

    let mut groups: Vec<Vec<(String, Value, Value)>> = Vec::new();
    for item in &enriched {
        let mut placed = false;
        for gi in 0..groups.len() {
            if groups[gi].len() as i64 >= max_parallel { continue; }
            let overlaps = groups[gi].iter().any(|x| fileOverlap(&x.2, &item.2).as_bool().unwrap_or(false));
            if !overlaps { groups[gi].push(item.clone()); placed = true; break; }
        }
        if !placed { groups.push(vec![item.clone()]); }
    }

    let out_groups: Vec<Value> = groups.iter().enumerate().map(|(i, g)| json!({
        "group": i as i64 + 1,
        "size": g.len(),
        "cards": g.iter().map(|x| json!({ "card_slug": x.0, "contract_slug": x.1, "file_count": x.2.as_array().map(|a| a.len()).unwrap_or(0) })).collect::<Vec<_>>(),
    })).collect();

    Ok(json!({
        "total_candidates": enriched.len(),
        "skipped_no_contract": slugs.len() as i64 - enriched.len() as i64,
        "group_count": groups.len(),
        "max_parallel": max_parallel,
        "groups": out_groups,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn groups_non_overlapping() {
        assert_eq!(fileOverlap(&json!(["a.ts"]), &json!(["b.ts"])), json!(false));
        assert_eq!(fileOverlap(&json!(["a.ts"]), &json!(["a.ts"])), json!(true));
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (slug TEXT, status TEXT, rowid_ INTEGER); CREATE TABLE contracts (id INTEGER PRIMARY KEY, slug TEXT, card_slug TEXT, files_touched_json TEXT); \
          INSERT INTO cards (slug,status) VALUES ('C1','doing'),('C2','doing'); \
          INSERT INTO contracts (slug,card_slug,files_touched_json) VALUES ('ct1','C1','[\"a.ts\"]'),('ct2','C2','[\"a.ts\"]');").unwrap();
        let r = driverParallelPlan(&json!({}), &c).unwrap();
        assert_eq!(r["total_candidates"], json!(2));
        assert_eq!(r["group_count"], json!(2)); // both touch a.ts → cannot parallelize
    }
}
