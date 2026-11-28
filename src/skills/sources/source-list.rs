//! source-list.rs — twin of skills/sources/source-list.mjs (db READ lane). NAMES ARE CONTRACT:
//! `sourceList` (default). Every registered source (metadata, newest first).
// QUALITY: 80/100 — SELECT + ORDER BY, one row-shape decoration (abs_path). The JS `dir` is
// `context?.sourcesDir ?? SOURCES_DIR`; the crossing's db-lane fixture context carries only `db`
// (law-add.rs precedent), so `dir` always resolves to the SOURCES_DIR default in every fixture
// run — reproduced below by anchoring off CARGO_MANIFEST_DIR the same way law-add.rs's
// hook_resolves anchors its scan root, since antiloki-core-rust and antiloki-core are siblings
// under the same SUBSTRATE_ROOT.
use rusqlite::Connection;
use serde_json::{json, Value};

fn sources_dir() -> std::path::PathBuf {
    // JS: SOURCES_DIR = join(SUBSTRATE_ROOT, '.antiloki', 'sources'), SUBSTRATE_ROOT =
    // resolve(ENGINE_ROOT, '..') anchored at roots.mjs's own file location (antiloki-core's
    // parent). CARGO_MANIFEST_DIR here is antiloki-core-rust; its parent is that same
    // SUBSTRATE_ROOT on this checkout layout.
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .join(".antiloki")
        .join("sources")
}

pub fn sourceList(_input: &Value, db: &Connection) -> Result<Value, String> {
    crate::sources_sources_schema::ensureSourcesSchema(db, &json!("knowledge."));
    let dir = sources_dir();
    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, filename, rel_path, ext, mime, sha256, bytes, title, added_at, (text IS NOT NULL) AS has_text FROM knowledge.sources ORDER BY added_at DESC, id",
        &[],
    )?;
    let sources: Vec<Value> = rows
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|r| {
            let mut o = r.as_object().cloned().unwrap_or_default();
            let rel = o.get("rel_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
            // JS `join(dir, rel)` (node:path) NORMALIZES separators — a `/`-containing rel_path
            // (e.g. "sources/solo.txt", the sources-schema.mjs convention) is joined with the
            // platform separator throughout, not appended verbatim. PathBuf::join alone keeps an
            // embedded "/" literal inside its single pushed component, diverging from JS on any
            // rel_path with a subdirectory (conform-db source_list fixture case 2 caught this).
            let mut abs_pb = dir.clone();
            for part in rel.split(['/', '\\']) {
                if !part.is_empty() { abs_pb.push(part); }
            }
            let abs = abs_pb.to_string_lossy().into_owned();
            o.insert("abs_path".into(), json!(abs));
            Value::Object(o)
        })
        .collect();
    let count = sources.len() as i64;
    Ok(json!({ "ok": true, "sources": sources, "count": count }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_knowledge_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS knowledge").unwrap();
        crate::sources_sources_schema::ensureSourcesSchema(&c, &json!("knowledge."));
        c
    }

    #[test]
    fn empty_table_returns_ok_zero_count() {
        let c = open_knowledge_db();
        let r = sourceList(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "sources": [], "count": 0 }));
    }

    #[test]
    fn newest_first_then_id_tiebreak_with_has_text_and_abs_path() {
        let c = open_knowledge_db();
        c.execute_batch(
            "INSERT INTO knowledge.sources (id, filename, rel_path, ext, mime, sha256, bytes, text, title, added_at, added_by) VALUES \
             ('src-aaa','a.md','src-aaa.md','.md','text/markdown','sha-a',10,'hello','A','2026-01-01 00:00:00','operator'), \
             ('src-bbb','b.png','src-bbb.png','.png','image/png','sha-b',20,NULL,'B','2026-02-01 00:00:00','operator'), \
             ('src-ccc','c.md','src-ccc.md','.md','text/markdown','sha-c',5,'hi','C','2026-02-01 00:00:00','operator')",
        )
        .unwrap();
        let r = sourceList(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["count"], json!(3));
        let ids: Vec<&str> = r["sources"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
        // tie on added_at ('2026-02-01') broken by id: bbb before ccc
        assert_eq!(ids, vec!["src-bbb", "src-ccc", "src-aaa"]);
        let b = &r["sources"][0];
        assert_eq!(b["has_text"], json!(0));
        assert!(b["abs_path"].as_str().unwrap().ends_with("src-bbb.png"));
        let a = &r["sources"][2];
        assert_eq!(a["has_text"], json!(1));
        assert!(a["abs_path"].as_str().unwrap().ends_with("src-aaa.md"));
    }

    #[test]
    fn nested_rel_path_joins_with_platform_separator_throughout() {
        // matches node:path `join(dir, rel)` normalizing an embedded "/" to the platform
        // separator, not PathBuf::join's literal-append of a single "sources/x" component.
        let c = open_knowledge_db();
        c.execute_batch(
            "INSERT INTO knowledge.sources (id, filename, rel_path, sha256, bytes, added_at, added_by) VALUES \
             ('src-solo','solo.txt','sources/solo.txt','sha-solo',7,'2026-03-15 00:00:00','operator')",
        )
        .unwrap();
        let r = sourceList(&json!({}), &c).unwrap();
        let abs = r["sources"][0]["abs_path"].as_str().unwrap();
        let foreign = if cfg!(windows) { '/' } else { '\\' };
        assert!(!abs.contains(foreign), "abs_path must use only the platform separator: {abs}");
        assert!(abs.ends_with(&format!("sources{0}sources{0}solo.txt", std::path::MAIN_SEPARATOR)));
    }
}
