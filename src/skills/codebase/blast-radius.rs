//! blast-radius.rs — twin of skills/codebase/blast-radius.mjs (db READ lane). NAMES ARE CONTRACT:
//! `blastRadius` (named default). Reverse-edge transitive closure + affected test files.
// QUALITY: 79/100 — stack closure with HashSet dedup, dynamic IN placeholders; affected is sorted
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        o => o.to_string(),
    }
}

pub fn blastRadius(input: &Value, db: &Connection) -> Result<Value, String> {
    let seeds: Vec<String> = input
        .get("node_ids")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(s).collect())
        .unwrap_or_default();
    if seeds.is_empty() {
        return Err("blast-radius: node_ids is required".into());
    }

    // Reverse closure: from each node, everything that points TO it (depends on it).
    let mut seen: HashSet<String> = seeds.iter().cloned().collect();
    let mut order: Vec<String> = seeds.clone(); // insertion order for the IN clause (mirrors JS Set)
    let mut stack: Vec<String> = seeds.clone();
    while let Some(cur) = stack.pop() {
        // a type-only import does not carry a change at runtime — it is not in the blast
        let rev = crate::db_json::query_rows(db, "SELECT DISTINCT from_node FROM code_edges WHERE to_node = ? AND COALESCE(kind, 'value') <> 'type'", &[json!(cur)])?;
        for r in rev.as_array().unwrap() {
            let fnode = s(r.get("from_node").unwrap_or(&Value::Null));
            if seen.insert(fnode.clone()) {
                order.push(fnode.clone());
                stack.push(fnode);
            }
        }
    }

    let seed_set: HashSet<&String> = seeds.iter().collect();
    let mut affected: Vec<String> = order.iter().filter(|id| !seed_set.contains(id)).cloned().collect();
    affected.sort();

    // Affected tests among all reached file nodes.
    let affected_tests: Vec<Value> = if !order.is_empty() {
        let ph = order.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT DISTINCT path FROM code_nodes WHERE id IN ({ph}) AND type = 'file' \
             AND (path LIKE '%.test.%' OR path LIKE '%.spec.%' OR path LIKE '%__tests__%')"
        );
        let args: Vec<Value> = order.iter().map(|s| json!(s)).collect();
        crate::db_json::query_rows(db, &sql, &args)?
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.get("path").cloned().unwrap_or(Value::Null))
            .collect()
    } else {
        vec![]
    };

    Ok(json!({ "seeds": seeds, "affected": affected, "affected_tests": affected_tests, "count": affected.len() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reverse_closure() {
        let c = Connection::open_in_memory().unwrap();
        // c <- b <- a : changing 'a' affects b and c
        c.execute_batch("CREATE TABLE code_edges (from_node TEXT, to_node TEXT, kind TEXT); \
          CREATE TABLE code_nodes (id TEXT, type TEXT, path TEXT); \
          INSERT INTO code_edges (from_node,to_node) VALUES ('b','a'),('c','b');").unwrap();
        let r = blastRadius(&json!({ "node_ids": ["a"] }), &c).unwrap();
        assert_eq!(r["affected"], json!(["b", "c"]));
        assert_eq!(r["count"], json!(2));
        assert!(blastRadius(&json!({}), &c).is_err());
    }
}
