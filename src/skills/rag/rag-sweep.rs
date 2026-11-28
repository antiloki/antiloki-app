//! rag-sweep.rs — twin of skills/rag/rag-sweep.mjs (db WRITE lane, cross-db: reads every
//! workspace DB's PROJECTORS-listed tables, embeds changed rows, upserts into memory.rag_chunks,
//! tombstones vanished rows, then rebuilds the graph). NAMES ARE CONTRACT: `ragSweep` (default),
//! `PROJECTORS` (the named registry — pressed here as PROJECTOR_KEYS + pk_for/text_for, since a
//! Rust map can't hold JS closures; the KEY SET and per-key pk/text LOGIC are the actual contract).
//!
//! PARITY-DEFERRED BOUNDARY: the real 768-dim ONNX embed call lives in `crate::external_embed`
//! (twin of skills/external/embed.mjs) — `embed()` is a deferred stub that always Errs, mirroring
//! exactly how this file already handles a `catch (e)` around `await embed(...)`: increment
//! `errors` and move on, never abort the sweep. Everything else here — projection, content-hash
//! dedup/skip, upsert shaping, tombstone deletion, the implicit-gate short-circuit, and the
//! dry_run counting path — is fully deterministic and is what's under test below.
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

// ---- JS coercion helpers -------------------------------------------------------------------

fn get<'a>(row: &'a Value, k: &str) -> Option<&'a Value> {
    row.get(k)
}

// String(v) — JS coercion used for pk assembly (ids are usually INTEGER or TEXT columns).
fn js_string(v: Option<&Value>) -> String {
    match v {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.as_i64().map(|i| i.to_string()).unwrap_or_else(|| n.as_f64().unwrap_or(0.0).to_string()),
        Some(Value::Bool(b)) => b.to_string(),
        Some(other) => other.to_string(),
    }
}

// v ?? '' style: null/missing → None, else Some(js_string).
fn nullish_str(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        some => Some(js_string(some)),
    }
}

// .filter(Boolean) truthiness check, coerced to a String for `.join(...)` — matches JS: only
// null/undefined/''/0/false/NaN are dropped; everything else (including non-empty strings and
// nonzero numbers) survives and gets String()-coerced for the join.
fn truthy(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => if s.is_empty() { None } else { Some(s.clone()) },
        Some(Value::Number(n)) => {
            let f = n.as_f64().unwrap_or(0.0);
            if f == 0.0 || f.is_nan() { None } else { Some(js_string(v)) }
        }
        Some(Value::Bool(b)) => if *b { Some("true".to_string()) } else { None },
        some => Some(js_string(some)),
    }
}

fn truthy_owned(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

fn join_with(parts: &[Option<String>], sep: &str) -> String {
    parts.iter().filter_map(|p| p.clone()).collect::<Vec<_>>().join(sep)
}
fn join_lines(parts: &[Option<String>]) -> String {
    join_with(parts, "\n")
}

fn is_atlas(row: &Value) -> bool {
    matches!(get(row, "origin"), Some(Value::String(s)) if s == "atlas")
}

// JSON.parse(field ?? default) inside a try/catch that swallows parse errors — default applies
// when the column is NULL/missing (nullish), not on any other falsy value.
fn parse_nullish_or(v: Option<&Value>, default: &str) -> Value {
    let raw = match v {
        None | Some(Value::Null) => default.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    serde_json::from_str(&raw).unwrap_or(Value::Null)
}
// JSON.parse(field || default) — falls back on ANY falsy value (also empty string), not just nullish.
fn parse_falsy_or(v: Option<&Value>, default: &str) -> Value {
    let raw = match v {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        None | Some(Value::Null) => default.to_string(),
        Some(Value::String(_)) => default.to_string(), // empty string is falsy
        Some(other) => other.to_string(),
    };
    serde_json::from_str(&raw).unwrap_or(Value::Null)
}

// ---- PROJECTORS registry (pressed: key set + per-key pk/text logic) -------------------------

pub const PROJECTOR_KEYS: [&str; 19] = [
    "memory.memories",
    "laws.laws",
    "knowledge.documents",
    "knowledge.extracts",
    "knowledge.sources",
    "brain.brain_decisions",
    "code.code_nodes",
    "code.code_descriptions",
    "code.code_decorators",
    "code.code_data_stories",
    "code.code_signal_latest",
    "state.cards",
    "state.contracts",
    "state.man_reports",
    "nb.notebook_sources",
    "knowledge.book_blocks",
    "knowledge.book_concepts",
    "knowledge.book_snapshots",
    "knowledge.knowledge_units",
];

fn pk_for(key: &str, row: &Value) -> String {
    match key {
        "memory.memories" | "knowledge.documents" | "knowledge.extracts" | "knowledge.sources"
        | "brain.brain_decisions" | "state.man_reports" | "nb.notebook_sources"
        | "knowledge.book_blocks" | "knowledge.book_concepts" | "knowledge.book_snapshots"
        | "knowledge.knowledge_units" => js_string(get(row, "id")),
        "laws.laws" => js_string(get(row, "slug")),
        "code.code_nodes" | "code.code_data_stories" | "code.code_signal_latest" => {
            if is_atlas(row) { js_string(get(row, "id")) } else { String::new() }
        }
        "code.code_descriptions" => {
            if is_atlas(row) && truthy(get(row, "summary_md")).is_some() { js_string(get(row, "path")) } else { String::new() }
        }
        "code.code_decorators" => {
            if is_atlas(row) { format!("{}#{}", js_string(get(row, "path")), js_string(get(row, "name"))) } else { String::new() }
        }
        // r.slug ?? r.id ?? '' — nullish chain
        "state.cards" => nullish_str(get(row, "slug")).or_else(|| nullish_str(get(row, "id"))).unwrap_or_default(),
        // r.slug ?? ''
        "state.contracts" => nullish_str(get(row, "slug")).unwrap_or_default(),
        _ => String::new(),
    }
}

fn text_for(key: &str, row: &Value) -> String {
    match key {
        "memory.memories" => join_lines(&[truthy(get(row, "title")), truthy(get(row, "situation")), truthy(get(row, "guidance")), truthy(get(row, "rationale"))]),
        "laws.laws" => join_lines(&[truthy(get(row, "title")), truthy(get(row, "statement")), truthy(get(row, "rationale"))]),
        "knowledge.documents" => join_lines(&[truthy(get(row, "vault_path")), truthy(get(row, "summary"))]),
        "knowledge.extracts" => join_lines(&[truthy(get(row, "text")), truthy(get(row, "context"))]),
        "knowledge.sources" => join_lines(&[truthy(get(row, "title")), truthy(get(row, "filename")), truthy(get(row, "text"))]),
        "brain.brain_decisions" => join_lines(&[truthy(get(row, "card_slug")), truthy(get(row, "decision")), truthy(get(row, "rationale"))]),
        "code.code_nodes" => {
            if !is_atlas(row) { return String::new(); }
            join_lines(&[truthy(get(row, "path")), truthy(get(row, "name")), truthy(get(row, "type")), truthy(get(row, "signature")), truthy(get(row, "intent"))])
        }
        "code.code_descriptions" => {
            if !(is_atlas(row) && truthy(get(row, "summary_md")).is_some()) { return String::new(); }
            join_lines(&[truthy(get(row, "path")), truthy(get(row, "summary_md"))])
        }
        "code.code_decorators" => {
            if !is_atlas(row) { return String::new(); }
            join_with(&[truthy(get(row, "kind")), truthy(get(row, "signature")), truthy(get(row, "summary"))], " · ")
        }
        "code.code_data_stories" => {
            if !is_atlas(row) { return String::new(); }
            let hops_arr = parse_falsy_or(get(row, "hops_json"), "[]");
            let hops = hops_arr.as_array().cloned().unwrap_or_default().iter()
                .filter_map(|h| truthy(h.get("label")).or_else(|| truthy(h.get("node_id"))))
                .collect::<Vec<_>>()
                .join(" → ");
            join_lines(&[truthy(get(row, "name")), truthy(get(row, "description")), truthy_owned(hops)])
        }
        "code.code_signal_latest" => {
            if !is_atlas(row) { return String::new(); }
            nullish_str(get(row, "card")).unwrap_or_default()
        }
        "state.cards" => {
            let body = parse_nullish_or(get(row, "body_json"), "{}");
            let why = join_lines(&[truthy(body.get("why")), truthy(body.get("what"))]);
            join_lines(&[truthy(get(row, "slug")), truthy(get(row, "title")), truthy_owned(why)])
        }
        "state.contracts" => {
            let acc_arr = parse_nullish_or(get(row, "acceptance_json"), "[]");
            let acc = acc_arr.as_array().cloned().unwrap_or_default().iter()
                .filter_map(|a| truthy(a.get("intent")).or_else(|| truthy(a.get("done_criteria"))))
                .collect::<Vec<_>>()
                .join("; ");
            join_lines(&[truthy(get(row, "slug")), truthy(get(row, "title")), truthy(get(row, "goal_md")), truthy_owned(acc)])
        }
        "state.man_reports" => join_lines(&[truthy(get(row, "contract_slug")), truthy(get(row, "verdict")), truthy(get(row, "what_happened_md")), truthy(get(row, "lessons_md"))]),
        "nb.notebook_sources" => join_lines(&[truthy(get(row, "label")), truthy(get(row, "body_md"))]),
        "knowledge.book_blocks" => join_lines(&[truthy(get(row, "title")), truthy(get(row, "text"))]),
        "knowledge.book_concepts" => join_lines(&[truthy(get(row, "term")), truthy(get(row, "definition")), truthy(get(row, "ref"))]),
        "knowledge.book_snapshots" => nullish_str(get(row, "snapshot_md")).unwrap_or_default().trim().to_string(),
        "knowledge.knowledge_units" => {
            let body = parse_falsy_or(get(row, "body_json"), "{}");
            join_lines(&[truthy(get(row, "title")), truthy(body.get("definition")), truthy(body.get("statement")), truthy(body.get("question")), truthy(body.get("description"))])
        }
        _ => String::new(),
    }
}

// state is the MAIN database (never ATTACHed under a name) — its tables are unqualified in SQL.
fn sql_prefix(sdb: &str) -> String {
    if sdb == "state" { String::new() } else { format!("{sdb}.") }
}

fn sha256_hex(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}

// float32 little-endian blob, 4 bytes/dim — matches memory-embed-store's pack.
fn pack_vector(vec: &[f64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vec.len() * 4);
    for v in vec {
        out.extend_from_slice(&(*v as f32).to_le_bytes());
    }
    out
}

fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(true, |f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

pub fn ragSweep(input: &Value, context: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('rag-sweep','db') when no db — the db lane always injects
    // one, so that guard is unreachable given this fn's (input, context, db) signature.
    let tables_filter: Option<Vec<String>> = input
        .get("tables")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect());
    // limit_per_table defaults to 500 only when the key is ABSENT (destructuring default) —
    // an explicit non-numeric value would flow to SQLite as-is in JS; we mirror the common
    // (numeric) case and fall back to 500 for anything else rather than guess SQLite's NULL/LIMIT
    // edge behavior.
    let limit_per_table: i64 = match input.get("limit_per_table") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(500),
        _ => 500,
    };
    let dry_run = input.get("dry_run").map(is_truthy).unwrap_or(false);
    let implicit = input.get("implicit").map(is_truthy).unwrap_or(false);
    let model = crate::external_embed::DEFAULT_MODEL;

    if implicit && !dry_run {
        if std::env::var_os("NODE_TEST_CONTEXT").is_some() {
            return Ok(json!({ "skipped_implicit": true, "reason": "test runner — implicit sweeps are inert" }));
        }
        if !crate::external_embed::embedModelCached(model) {
            return Ok(json!({ "skipped_implicit": true, "reason": "embed model not cached — run system.rag.sweep once to initialize RAG" }));
        }
    }

    let keys: Vec<&str> = PROJECTOR_KEYS
        .iter()
        .copied()
        .filter(|k| tables_filter.as_ref().map_or(true, |t| t.iter().any(|s| s == k)))
        .collect();

    let mut embedded = 0i64;
    let mut skipped = 0i64;
    let mut deleted = 0i64;
    let mut errors = 0i64;
    let mut by_table = serde_json::Map::new();

    for key in keys {
        let dot = key.find('.').unwrap();
        let sdb = &key[..dot];
        let table = &key[dot + 1..];
        let sql = format!("SELECT * FROM {}{} LIMIT ?", sql_prefix(sdb), table);
        let rows: Vec<Value> = match crate::db_json::query_rows(db, &sql, &[json!(limit_per_table)]) {
            Ok(v) => v.as_array().cloned().unwrap_or_default(),
            Err(e) => {
                errors += 1;
                by_table.insert(key.to_string(), json!({ "error": e.chars().take(140).collect::<String>() }));
                continue;
            }
        };

        let mut emb = 0i64;
        let mut skip = 0i64;
        let mut del = 0i64;
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        for row in &rows {
            // JS wraps pk()/text() in a try/catch and `continue`s on throw; our helpers never
            // panic (safe accessors throughout), so there's nothing to catch here.
            let pk = pk_for(key, row);
            let text = text_for(key, row).trim().to_string();
            if pk.is_empty() || text.is_empty() {
                continue;
            }
            seen.insert(pk.clone());
            let hash = sha256_hex(&text);
            let existing: Option<String> = db
                .query_row(
                    "SELECT content_hash FROM memory.rag_chunks WHERE source_db=? AND source_table=? AND source_row=? AND chunk_ix=0",
                    rusqlite::params![sdb, table, pk],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            if existing.as_deref() == Some(hash.as_str()) {
                skip += 1;
                continue;
            }
            if dry_run {
                emb += 1;
                continue;
            }
            // text.slice(0, 8000): JS slices by UTF-16 code unit; we slice by Unicode scalar
            // (chars) — a documented parity gap for non-BMP text, immaterial for ASCII fixtures.
            let text_slice: String = text.chars().take(8000).collect();
            match crate::external_embed::embed(&json!({ "text": text_slice, "model": model }), context) {
                Err(_) => {
                    errors += 1;
                    continue;
                }
                Ok(e) => {
                    let vec: Vec<f64> = e.get("vector").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_f64()).collect()).unwrap_or_default();
                    let dim = e.get("dims").and_then(|v| v.as_i64()).unwrap_or(0);
                    let blob = pack_vector(&vec);
                    db.execute(
                        "INSERT INTO memory.rag_chunks (source_db, source_table, source_row, chunk_ix, text, content_hash, vector, model, dim, updated_at)
                         VALUES (?, ?, ?, 0, ?, ?, ?, ?, ?, datetime('now'))
                         ON CONFLICT(source_db, source_table, source_row, chunk_ix) DO UPDATE SET
                           text=excluded.text, content_hash=excluded.content_hash, vector=excluded.vector,
                           model=excluded.model, dim=excluded.dim, updated_at=datetime('now')",
                        rusqlite::params![sdb, table, pk, text_slice, hash, blob, model, dim],
                    )
                    .map_err(|e| e.to_string())?;
                    emb += 1;
                }
            }
        }

        // tombstone: drop chunks whose source row is gone — only when we read the FULL table (not capped).
        if !dry_run && (rows.len() as i64) < limit_per_table {
            let source_rows: Vec<String> = {
                let mut stmt = db
                    .prepare("SELECT source_row FROM memory.rag_chunks WHERE source_db=? AND source_table=?")
                    .map_err(|e| e.to_string())?;
                let collected: Result<Vec<String>, rusqlite::Error> = stmt
                    .query_map(rusqlite::params![sdb, table], |r| r.get(0))
                    .and_then(|rows| rows.collect());
                collected.map_err(|e| e.to_string())?
            };
            for c in source_rows {
                if !seen.contains(&c) {
                    db.execute(
                        "DELETE FROM memory.rag_chunks WHERE source_db=? AND source_table=? AND source_row=?",
                        rusqlite::params![sdb, table, c],
                    )
                    .map_err(|e| e.to_string())?;
                    del += 1;
                }
            }
        }

        embedded += emb;
        skipped += skip;
        deleted += del;
        by_table.insert(key.to_string(), json!({ "rows": rows.len(), "embedded": emb, "skipped": skip, "deleted": del }));
    }

    // rebuild the graph (kNN + ref edges) when anything changed.
    let mut graph = Value::Null;
    if !dry_run && (embedded + deleted) > 0 {
        match crate::rag_rag_graph_build::ragGraphBuild(&json!({}), db) {
            Ok(g) => graph = g,
            Err(_) => errors += 1,
        }
    }

    let total: i64 = db.query_row("SELECT COUNT(*) c FROM memory.rag_chunks", [], |r| r.get(0)).unwrap_or(0);

    Ok(json!({
        "embedded": embedded, "skipped": skipped, "deleted": deleted, "errors": errors,
        "by_table": by_table, "total_chunks": total, "graph": graph, "model": model, "dry_run": dry_run
    }))
}

pub use PROJECTOR_KEYS as PROJECTORS;

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory;
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, title TEXT, situation TEXT, guidance TEXT, rationale TEXT);
             CREATE TABLE memory.rag_chunks (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               source_db TEXT NOT NULL, source_table TEXT NOT NULL, source_row TEXT NOT NULL,
               chunk_ix INTEGER NOT NULL DEFAULT 0, text TEXT NOT NULL, content_hash TEXT NOT NULL,
               vector BLOB NOT NULL, model TEXT NOT NULL, dim INTEGER NOT NULL,
               updated_at TEXT NOT NULL DEFAULT (datetime('now')),
               UNIQUE (source_db, source_table, source_row, chunk_ix)
             );
             CREATE TABLE memory.rag_edges (from_chunk INTEGER NOT NULL, to_chunk INTEGER NOT NULL, kind TEXT NOT NULL, score REAL NOT NULL, PRIMARY KEY (from_chunk, to_chunk, kind));
             CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT, title TEXT, body_json TEXT);",
        )
        .unwrap();
        c
    }

    fn with_repo_env<T>(key: &str, val: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        let prior = std::env::var(key).ok();
        match val {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
        let result = f();
        match prior {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
        result
    }

    // ---- projector text/pk shaping (dry_run — never touches the deferred embed boundary) ----

    #[test]
    fn dry_run_counts_projected_rows_without_writing_chunks() {
        let c = fresh();
        c.execute("INSERT INTO memory.memories (id, title, situation) VALUES (1, 'T', 'S')", []).unwrap();
        let r = with_repo_env("NODE_TEST_CONTEXT", None, || {
            ragSweep(&json!({ "tables": ["memory.memories"], "dry_run": true }), &Value::Null, &c).unwrap()
        });
        assert_eq!(r["embedded"], json!(1));
        assert_eq!(r["skipped"], json!(0));
        assert_eq!(r["by_table"]["memory.memories"], json!({ "rows": 1, "embedded": 1, "skipped": 0, "deleted": 0 }));
        let count: i64 = c.query_row("SELECT COUNT(*) FROM memory.rag_chunks", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0, "dry_run never writes chunks");
    }

    #[test]
    fn rows_with_empty_pk_or_text_are_filtered_out() {
        let c = fresh();
        // no title/situation/guidance/rationale → text projects to '' → filtered
        c.execute("INSERT INTO memory.memories (id) VALUES (1)", []).unwrap();
        c.execute("INSERT INTO memory.memories (id, title) VALUES (2, 'has text')", []).unwrap();
        let r = ragSweep(&json!({ "tables": ["memory.memories"], "dry_run": true }), &Value::Null, &c).unwrap();
        assert_eq!(r["by_table"]["memory.memories"]["embedded"], json!(1));
    }

    #[test]
    fn state_cards_projects_slug_title_and_body_json_why_what() {
        let c = fresh();
        c.execute(
            "INSERT INTO cards (id, slug, title, body_json) VALUES (1, 'CARD-A', 'A title', '{\"why\":\"because\",\"what\":\"a thing\"}')",
            [],
        )
        .unwrap();
        let r = ragSweep(&json!({ "tables": ["state.cards"], "dry_run": true }), &Value::Null, &c).unwrap();
        assert_eq!(r["by_table"]["state.cards"], json!({ "rows": 1, "embedded": 1, "skipped": 0, "deleted": 0 }));
        assert_eq!(pk_for("state.cards", &json!({ "slug": "CARD-A", "id": 1 })), "CARD-A");
        assert_eq!(pk_for("state.cards", &json!({ "slug": null, "id": 7 })), "7");
        assert_eq!(text_for("state.cards", &json!({ "slug": "S", "title": "T", "body_json": "{\"why\":\"W\",\"what\":\"WH\"}" })), "S\nT\nW\nWH");
        assert_eq!(text_for("state.cards", &json!({ "slug": "S", "title": "T", "body_json": "not json" })), "S\nT");
    }

    #[test]
    fn code_projectors_filter_non_atlas_origin() {
        assert_eq!(pk_for("code.code_nodes", &json!({ "id": 1, "origin": "manual" })), "");
        assert_eq!(text_for("code.code_nodes", &json!({ "id": 1, "origin": "manual", "path": "a" })), "");
        assert_eq!(pk_for("code.code_nodes", &json!({ "id": 1, "origin": "atlas" })), "1");
        assert_eq!(text_for("code.code_nodes", &json!({ "id": 1, "origin": "atlas", "path": "p", "name": "n" })), "p\nn");
    }

    #[test]
    fn content_hash_dedup_skips_unchanged_rows_on_rerun() {
        // dry_run inserts nothing, so pre-seed rag_chunks directly with the SAME hash the sweep
        // would compute for this row's projected text, then re-run and assert skip (not embedded).
        let c = fresh();
        c.execute("INSERT INTO memory.memories (id, title) VALUES (1, 'stable text')", []).unwrap();
        let text = text_for("memory.memories", &json!({ "id": 1, "title": "stable text" }));
        let hash = sha256_hex(&text);
        c.execute(
            "INSERT INTO memory.rag_chunks (source_db, source_table, source_row, chunk_ix, text, content_hash, vector, model, dim) VALUES ('memory','memories','1',0,?,?,x'',?,3)",
            rusqlite::params![text, hash, crate::external_embed::DEFAULT_MODEL],
        )
        .unwrap();
        let r = ragSweep(&json!({ "tables": ["memory.memories"], "dry_run": true }), &Value::Null, &c).unwrap();
        assert_eq!(r["by_table"]["memory.memories"], json!({ "rows": 1, "embedded": 0, "skipped": 1, "deleted": 0 }));
    }

    // ---- non-dry-run: the embed boundary is deferred, so a real sweep always errors per row ----

    #[test]
    fn non_dry_run_hits_the_deferred_embed_boundary_and_counts_an_error_per_row() {
        let c = fresh();
        c.execute("INSERT INTO memory.memories (id, title) VALUES (1, 'needs embedding')", []).unwrap();
        let r = ragSweep(&json!({ "tables": ["memory.memories"] }), &Value::Null, &c).unwrap();
        assert_eq!(r["embedded"], json!(0));
        assert_eq!(r["errors"], json!(1));
        let count: i64 = c.query_row("SELECT COUNT(*) FROM memory.rag_chunks", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0, "a failed embed never reaches the upsert");
    }

    // ---- tombstone: drops chunks whose source row vanished, only on an uncapped read ----

    #[test]
    fn tombstone_deletes_chunks_for_vanished_rows_when_table_read_is_uncapped() {
        let c = fresh();
        c.execute("INSERT INTO memory.memories (id, title) VALUES (1, 'still here')", []).unwrap();
        c.execute(
            "INSERT INTO memory.rag_chunks (source_db, source_table, source_row, chunk_ix, text, content_hash, vector, model, dim) VALUES ('memory','memories','1',0,'x','h1',x'',?,1), ('memory','memories','999',0,'y','h2',x'',?,1)",
            rusqlite::params![crate::external_embed::DEFAULT_MODEL, crate::external_embed::DEFAULT_MODEL],
        )
        .unwrap();
        let r = ragSweep(&json!({ "tables": ["memory.memories"], "limit_per_table": 500 }), &Value::Null, &c).unwrap();
        assert_eq!(r["deleted"], json!(1));
        let remaining: i64 = c.query_row("SELECT COUNT(*) FROM memory.rag_chunks WHERE source_row='999'", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining, 0);
        let kept: i64 = c.query_row("SELECT COUNT(*) FROM memory.rag_chunks WHERE source_row='1'", [], |r| r.get(0)).unwrap();
        assert_eq!(kept, 1, "row 1's own chunk pre-existed and is untouched by tombstoning (embed still errors, doesn't re-upsert)");
    }

    #[test]
    fn tombstone_skipped_when_the_table_read_is_capped_at_the_limit() {
        let c = fresh();
        c.execute("INSERT INTO memory.memories (id, title) VALUES (1, 'a')", []).unwrap();
        c.execute(
            "INSERT INTO memory.rag_chunks (source_db, source_table, source_row, chunk_ix, text, content_hash, vector, model, dim) VALUES ('memory','memories','999',0,'y','h2',x'',?,1)",
            rusqlite::params![crate::external_embed::DEFAULT_MODEL],
        )
        .unwrap();
        // limit_per_table == rows.len() (1) → capped read → tombstone pass skipped entirely.
        let r = ragSweep(&json!({ "tables": ["memory.memories"], "limit_per_table": 1 }), &Value::Null, &c).unwrap();
        assert_eq!(r["deleted"], json!(0));
        let remaining: i64 = c.query_row("SELECT COUNT(*) FROM memory.rag_chunks WHERE source_row='999'", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining, 1, "capped read never tombstones");
    }

    // ---- the implicit auto-sweep gate: fail-CLOSED on heavy work ----

    #[test]
    fn implicit_sweep_skips_under_the_node_test_runner() {
        let c = fresh();
        let r = with_repo_env("NODE_TEST_CONTEXT", Some("1"), || ragSweep(&json!({ "implicit": true }), &Value::Null, &c).unwrap());
        assert_eq!(r, json!({ "skipped_implicit": true, "reason": "test runner — implicit sweeps are inert" }));
    }

    #[test]
    fn implicit_plus_dry_run_bypasses_the_gate_entirely() {
        let c = fresh();
        c.execute("INSERT INTO memory.memories (id, title) VALUES (1, 'x')", []).unwrap();
        // implicit && dry_run → guard `implicit && !dry_run` is false → gate never fires, even
        // under NODE_TEST_CONTEXT.
        let r = with_repo_env("NODE_TEST_CONTEXT", Some("1"), || {
            ragSweep(&json!({ "tables": ["memory.memories"], "implicit": true, "dry_run": true }), &Value::Null, &c).unwrap()
        });
        assert!(r.get("skipped_implicit").is_none());
        assert_eq!(r["embedded"], json!(1));
    }

    #[test]
    fn non_implicit_sweep_ignores_the_test_runner_env_entirely() {
        let c = fresh();
        c.execute("INSERT INTO memory.memories (id, title) VALUES (1, 'x')", []).unwrap();
        let r = with_repo_env("NODE_TEST_CONTEXT", Some("1"), || {
            ragSweep(&json!({ "tables": ["memory.memories"], "dry_run": true }), &Value::Null, &c).unwrap()
        });
        assert_eq!(r["embedded"], json!(1));
    }

    // ---- tables filter + total_chunks ----

    #[test]
    fn tables_filter_narrows_to_the_requested_keys_only() {
        let c = fresh();
        c.execute("INSERT INTO memory.memories (id, title) VALUES (1, 'x')", []).unwrap();
        c.execute("INSERT INTO cards (id, slug, title) VALUES (1, 'S', 'T')", []).unwrap();
        let r = ragSweep(&json!({ "tables": ["state.cards"], "dry_run": true }), &Value::Null, &c).unwrap();
        assert!(r["by_table"].get("state.cards").is_some());
        assert!(r["by_table"].get("memory.memories").is_none());
    }

    #[test]
    fn missing_source_table_records_an_error_and_keeps_going() {
        let c = fresh(); // no `cards` table content issue — use a genuinely absent table via filter
        let r = ragSweep(&json!({ "tables": ["knowledge.documents"] }), &Value::Null, &c).unwrap();
        // ATTACHed 'knowledge' schema was never attached in this fixture → SELECT fails → error recorded.
        assert_eq!(r["errors"], json!(1));
        assert!(r["by_table"]["knowledge.documents"].get("error").is_some());
    }

    #[test]
    fn total_chunks_reflects_the_live_rag_chunks_count() {
        let c = fresh();
        c.execute(
            "INSERT INTO memory.rag_chunks (source_db, source_table, source_row, chunk_ix, text, content_hash, vector, model, dim) VALUES ('memory','memories','1',0,'x','h1',x'',?,1)",
            rusqlite::params![crate::external_embed::DEFAULT_MODEL],
        )
        .unwrap();
        let r = ragSweep(&json!({ "tables": [], "dry_run": true }), &Value::Null, &c).unwrap();
        assert_eq!(r["total_chunks"], json!(1));
    }
}
