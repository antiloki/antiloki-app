//! catalog-embed-all.rs — twin of skills/entities/catalog-embed-all.mjs (db WRITE lane).
//! NAMES ARE CONTRACT: `catalogEmbedAll` (default), `embedSet`, `packFloat32`, `PACK_DIMS_HINT`.
//!
//! Embeds every skill/action/agent/law/memory-correction entity description into
//! `memory.catalog_embeddings`, skipping entries already embedded for the resolved
//! (entity_type, entity_name, model) key. `context.db` missing → JS throws
//! `err.contextMissing('catalog-embed-all', 'db')` — unreachable here (`db: &Connection` is a
//! required, non-nullable parameter, same convention as every sibling db-lane twin).
//!
//! PARITY-DEFERRED: the per-row `embed()` call (`crate::external_embed::embed` —
//! `@huggingface/transformers` ONNX inference) always Errs in this crate, so every row that isn't
//! already-embedded falls into the `failed` bucket and `insertStmt.run` (the `Ok` store path) is
//! unreachable — mirrored faithfully, not fabricated. Everything AROUND that boundary is real and
//! tested: per-type row enumeration (skill/action/agent/law/memory-correction, each with its own
//! SQL shape — law excludes superseded/archived, memory-correction synthesizes `mem-{id}` names
//! from situation+guidance), the exists-check dedupe, the `{name}: {description}` / bare `{name}`
//! text-building fallback, and the embedded/skipped/failed counters returned per entity_type.

use rusqlite::{params, Connection};
use serde_json::{json, Value};

pub const PACK_DIMS_HINT: i64 = 768; // BGE-base default

/// Row shape shared by every entity_type source query: a unique `name` plus an optional
/// `description` (JS: `r.description ? \`${r.name}: ${r.description}\` : r.name`).
struct CatalogRow {
    name: String,
    description: Option<String>,
}

fn embed_set(
    db: &Connection,
    entity_type: &str,
    rows: &[CatalogRow],
    model: &str,
) -> Result<Value, String> {
    let mut embedded: i64 = 0;
    let mut skipped: i64 = 0;
    let mut failed: i64 = 0;

    for r in rows {
        let exists: bool = db
            .query_row(
                "SELECT 1 FROM memory.catalog_embeddings WHERE entity_type = ? AND entity_name = ? AND model = ?",
                params![entity_type, r.name, model],
                |_row| Ok(()),
            )
            .is_ok();
        if exists {
            skipped += 1;
            continue;
        }

        let text = match &r.description {
            Some(d) if !d.is_empty() => format!("{}: {}", r.name, d),
            _ => r.name.clone(),
        };

        match crate::external_embed::embed(&json!({ "text": text, "model": model }), &Value::Null) {
            // PARITY-DEFERRED: embed() never returns Ok in this crate — the store path
            // (packFloat32 -> INSERT INTO memory.catalog_embeddings) has no reachable branch.
            Ok(e) => {
                let dims = e.get("dims").and_then(Value::as_i64).unwrap_or(0);
                let vec_bytes: Vec<u8> = e
                    .get("vector")
                    .and_then(Value::as_array)
                    .map(|arr| pack_float32(arr))
                    .unwrap_or_default();
                let e_model = e.get("model").and_then(Value::as_str).unwrap_or(model);
                db.execute(
                    "INSERT INTO memory.catalog_embeddings (entity_type, entity_name, model, dims, vector, embedded_text)
                     VALUES (?, ?, ?, ?, ?, ?)",
                    params![entity_type, r.name, e_model, dims, vec_bytes, text],
                )
                .map_err(|e| format!("catalog-embed-all: {e}"))?;
                embedded += 1;
            }
            Err(_) => {
                failed += 1;
            }
        }
    }

    Ok(json!({ "embedded": embedded, "skipped": skipped, "failed": failed }))
}

/// JS: pack a Float32Array into a little-endian Uint8Array BLOB. Never exercised on the live
/// embed path here (embed() always Errs), kept for the store-path's completeness/documentation.
fn pack_float32(arr: &[Value]) -> Vec<u8> {
    let mut out = Vec::with_capacity(arr.len() * 4);
    for v in arr {
        let f = v.as_f64().unwrap_or(0.0) as f32;
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn truthy_str_array(v: Option<&Value>) -> Option<Vec<String>> {
    v.and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect()
    })
}

/// `export default async function catalogEmbedAll(input, context)`.
#[allow(non_snake_case)]
pub fn catalogEmbedAll(input: &Value, db: &Connection) -> Result<Value, String> {
    let obj = input.as_object();

    let model = obj
        .and_then(|o| o.get("model"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());

    let default_types = vec![
        "skill".to_string(),
        "action".to_string(),
        "agent".to_string(),
        "law".to_string(),
        "memory-correction".to_string(),
    ];
    let types = truthy_str_array(obj.and_then(|o| o.get("types"))).unwrap_or(default_types);

    let use_model = model.unwrap_or(crate::external_embed::DEFAULT_MODEL).to_string();

    let mut out = serde_json::Map::new();

    if types.iter().any(|t| t == "skill") {
        let rows = query_rows(db, "SELECT name, description FROM memory.skills")?;
        out.insert("skill".into(), embed_set(db, "skill", &rows, &use_model)?);
    }
    if types.iter().any(|t| t == "action") {
        let rows = query_rows(db, "SELECT name, description FROM actions")?;
        out.insert("action".into(), embed_set(db, "action", &rows, &use_model)?);
    }
    if types.iter().any(|t| t == "agent") {
        let rows = query_rows(db, "SELECT name, description FROM agents")?;
        out.insert("agent".into(), embed_set(db, "agent", &rows, &use_model)?);
    }
    if types.iter().any(|t| t == "law") {
        let rows = query_rows(
            db,
            "SELECT slug AS name, title || ': ' || statement AS description FROM laws.laws WHERE superseded_by IS NULL AND enforcement != 'archived'",
        )?;
        out.insert("law".into(), embed_set(db, "law", &rows, &use_model)?);
    }
    if types.iter().any(|t| t == "memory-correction") {
        let mut stmt = db
            .prepare(
                "SELECT id, situation, guidance, naive_response FROM memory.memories WHERE type = 'correction'",
            )
            .map_err(|e| format!("catalog-embed-all: {e}"))?;
        let raw_rows: Vec<(i64, Option<String>, Option<String>)> = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let situation: Option<String> = row.get(1)?;
                let guidance: Option<String> = row.get(2)?;
                Ok((id, situation, guidance))
            })
            .map_err(|e| format!("catalog-embed-all: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("catalog-embed-all: {e}"))?;
        let rows: Vec<CatalogRow> = raw_rows
            .into_iter()
            .map(|(id, situation, guidance)| {
                let description = format!(
                    "{}\n{}",
                    situation.unwrap_or_default(),
                    guidance.unwrap_or_default()
                )
                .trim()
                .to_string();
                CatalogRow {
                    name: format!("mem-{id}"),
                    description: Some(description),
                }
            })
            .collect();
        out.insert(
            "memory-correction".into(),
            embed_set(db, "memory-correction", &rows, &use_model)?,
        );
    }

    out.insert("model".into(), json!(use_model));
    out.insert("dims_hint".into(), json!(PACK_DIMS_HINT));

    Ok(Value::Object(out))
}

fn query_rows(db: &Connection, sql: &str) -> Result<Vec<CatalogRow>, String> {
    let mut stmt = db.prepare(sql).map_err(|e| format!("catalog-embed-all: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            let name: String = row.get(0)?;
            let description: Option<String> = row.get(1)?;
            Ok(CatalogRow { name, description })
        })
        .map_err(|e| format!("catalog-embed-all: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("catalog-embed-all: {e}"))?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory; ATTACH ':memory:' AS laws;
             CREATE TABLE memory.catalog_embeddings (entity_type TEXT NOT NULL, entity_name TEXT NOT NULL, model TEXT NOT NULL, dims INTEGER NOT NULL, vector BLOB NOT NULL, embedded_text TEXT NOT NULL);
             CREATE TABLE memory.skills (name TEXT, description TEXT);
             CREATE TABLE actions (name TEXT, description TEXT);
             CREATE TABLE agents (name TEXT, description TEXT);
             CREATE TABLE laws.laws (slug TEXT, title TEXT, statement TEXT, enforcement TEXT, superseded_by INTEGER);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, type TEXT, situation TEXT, guidance TEXT, naive_response TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn empty_db_all_types_returns_zero_counts_and_default_model() {
        let c = db();
        let r = catalogEmbedAll(&Value::Null, &c).unwrap();
        for t in ["skill", "action", "agent", "law", "memory-correction"] {
            assert_eq!(r[t]["embedded"], json!(0));
            assert_eq!(r[t]["skipped"], json!(0));
            assert_eq!(r[t]["failed"], json!(0));
        }
        assert_eq!(r["model"], json!(crate::external_embed::DEFAULT_MODEL));
        assert_eq!(r["dims_hint"], json!(768));
    }

    #[test]
    fn skill_rows_fail_via_deferred_embed_boundary() {
        let c = db();
        c.execute(
            "INSERT INTO memory.skills (name, description) VALUES ('card-add', 'adds a card')",
            [],
        )
        .unwrap();
        let r = catalogEmbedAll(&json!({ "types": ["skill"] }), &c).unwrap();
        assert_eq!(r["skill"]["failed"], json!(1));
        assert_eq!(r["skill"]["embedded"], json!(0));
        assert_eq!(r["skill"]["skipped"], json!(0));
        // Other types weren't requested — absent from output, mirroring JS's `if (types.includes(...))`.
        assert!(r.get("action").is_none());
    }

    #[test]
    fn already_embedded_entry_is_skipped_not_reattempted() {
        let c = db();
        c.execute(
            "INSERT INTO memory.skills (name, description) VALUES ('card-add', 'adds a card')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO memory.catalog_embeddings (entity_type, entity_name, model, dims, vector, embedded_text) VALUES ('skill', 'card-add', ?, 768, x'00', 'card-add: adds a card')",
            params![crate::external_embed::DEFAULT_MODEL],
        )
        .unwrap();
        let r = catalogEmbedAll(&json!({ "types": ["skill"] }), &c).unwrap();
        assert_eq!(r["skill"]["skipped"], json!(1));
        assert_eq!(r["skill"]["failed"], json!(0));
    }

    #[test]
    fn skill_without_description_falls_back_to_bare_name() {
        // No direct observable of the built `text` string here (it only surfaces on the
        // unreachable insert path), but the row must still be visited and counted as failed —
        // proves the `r.description ? ... : r.name` branch doesn't panic/skip on NULL.
        let c = db();
        c.execute("INSERT INTO memory.skills (name, description) VALUES ('bare-skill', NULL)", [])
            .unwrap();
        let r = catalogEmbedAll(&json!({ "types": ["skill"] }), &c).unwrap();
        assert_eq!(r["skill"]["failed"], json!(1));
    }

    #[test]
    fn law_query_excludes_superseded_and_archived() {
        let c = db();
        c.execute_batch(
            "INSERT INTO laws.laws (slug, title, statement, enforcement, superseded_by) VALUES
             ('LAW_A', 'Law A', 'do the thing', 'hard', NULL),
             ('LAW_B', 'Law B', 'do another thing', 'archived', NULL),
             ('LAW_C', 'Law C', 'superseded thing', 'hard', 1);",
        )
        .unwrap();
        let r = catalogEmbedAll(&json!({ "types": ["law"] }), &c).unwrap();
        // Only LAW_A is a live candidate row -> exactly one deferred-embed failure.
        assert_eq!(r["law"]["failed"], json!(1));
    }

    #[test]
    fn memory_correction_rows_synthesize_mem_prefixed_names() {
        let c = db();
        c.execute(
            "INSERT INTO memory.memories (id, type, situation, guidance, naive_response) VALUES (5, 'correction', 'sit', 'guide', 'naive')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO memory.memories (id, type, situation, guidance, naive_response) VALUES (6, 'other', 'sit2', 'guide2', 'naive2')",
            [],
        )
        .unwrap();
        let r = catalogEmbedAll(&json!({ "types": ["memory-correction"] }), &c).unwrap();
        // id=6 has type='other', excluded by WHERE type = 'correction' -> only 1 candidate row.
        assert_eq!(r["memory-correction"]["failed"], json!(1));
    }

    #[test]
    fn explicit_model_overrides_default_and_is_used_for_exists_check() {
        let c = db();
        c.execute("INSERT INTO memory.skills (name, description) VALUES ('x', 'y')", []).unwrap();
        c.execute(
            "INSERT INTO memory.catalog_embeddings (entity_type, entity_name, model, dims, vector, embedded_text) VALUES ('skill', 'x', 'other-model', 768, x'00', 'x: y')",
            [],
        )
        .unwrap();
        // Stored row is under 'other-model'; default model differs, so it's NOT a dedupe hit.
        let r = catalogEmbedAll(&Value::Null, &c).unwrap();
        assert_eq!(r["skill"]["failed"], json!(1));
        assert_eq!(r["skill"]["skipped"], json!(0));
        assert_eq!(r["model"], json!(crate::external_embed::DEFAULT_MODEL));

        // Now request the model that IS stored -> dedupe hits, skipped.
        let r2 = catalogEmbedAll(&json!({ "model": "other-model", "types": ["skill"] }), &c).unwrap();
        assert_eq!(r2["skill"]["skipped"], json!(1));
        assert_eq!(r2["model"], json!("other-model"));
    }

    #[test]
    fn pack_float32_encodes_little_endian_bytes() {
        let bytes = pack_float32(&[json!(1.0), json!(2.0)]);
        assert_eq!(bytes.len(), 8);
        assert_eq!(&bytes[0..4], &1.0f32.to_le_bytes());
        assert_eq!(&bytes[4..8], &2.0f32.to_le_bytes());
    }
}
