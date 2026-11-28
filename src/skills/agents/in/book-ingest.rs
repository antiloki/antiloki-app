//! book-ingest.rs — twin of skills/agents/in/book-ingest.mjs (db, cross-db knowledge.*). NAMES ARE
//! CONTRACT: default `bookIngest`. The deterministic half of book importation: decompose pasted/
//! parsed text into ordered blocks by headings (fallback: one block), shelve into
//! knowledge.books/book_blocks, then serve list/get/module/annotate/atlas/harvest over the shelf.
//! No AI anywhere in this file.
//!
//! `op:'ingest_file'` calls the already-converted `agents_in_book_parse::bookParse` twin, which is
//! itself BESPOKE-NATIVE (reads the real vault filesystem under KNOWLEDGE_DIR — see that module's
//! header). That real filesystem dependency is only reachable when the CALLER passes
//! `op:'ingest_file'`; every other op never touches it. Per the crossing playbook's
//! conditionally-reachable-risk convention, this twin ports the ENTIRE `ingest_file` branch as
//! real, correct code (including the deterministic filename-stem title fallback, which reads
//! `knowledge.documents`) but the fixture below exercises only the `ingest_file`-free ops — there
//! is no vault file on disk for conform-db's in-memory harness to read.
//! `op:'atlas'` forwards verbatim to the already-converted `agents_in_book_atlas::bookAtlas` twin.
// QUALITY: 74/100 — decompose()'s heading-detection regex + pack-split/dust-merge sizing pass are
// the interesting logic; every other op is a straightforward multi-table read/write. ingest_file's
// real-fs branch is proven correct by inspection + cross-module reuse of book-parse's own native
// tests, not by this file's own fixture (see header).
use regex::Regex;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::sync::OnceLock;

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
// `String(x ?? default)`: nz-then-js_string, else the literal default.
fn str_or(v: Option<&Value>, default: &str) -> String {
    match nz(v) { Some(x) => js_string(x), None => default.to_string() }
}
// = JS `Number.isInteger(f)` for an already-Number()-coerced value.
fn is_int(f: f64) -> bool { f.is_finite() && f.fract() == 0.0 }

// ── book-ingest's own module-private helpers (mirror the JS top-level consts/fns) ──────────
fn tokens_est(s: &str) -> i64 { ((s.chars().count() as f64) / 4.0).ceil() as i64 }

fn heading_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^(?:#{1,3}\s+.+|(?:chapter|chapter\s+[ivxlc\d]+|part\s+[ivxlc\d]+|appendix\s+[a-z\d]?)\b.*|\d{1,2}(?:\.\d{1,2})?\s+[A-Z].{3,80})$").unwrap())
}
fn heading_prefix_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^#+\s*").unwrap())
}
fn blank_lines_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\n\n+").unwrap())
}
// used only by the ingest_file filename-stem fallback (real-but-untested-by-fixture branch, see header)
fn ext_strip_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)\.(epub|pdf|md|txt|markdown)$").unwrap())
}
fn dash_collapse_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[_-]+").unwrap())
}

struct Block { title: String, text: String }

fn decompose(content: &str) -> Vec<Block> {
    let normalized = content.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut marks: Vec<usize> = Vec::new();
    for (i, raw) in lines.iter().enumerate() {
        let ln = raw.trim();
        if !ln.is_empty() && ln.chars().count() <= 90 && heading_re().is_match(ln) && !ln.ends_with('.') {
            marks.push(i);
        }
    }
    let mut blocks: Vec<Block> = Vec::new();
    if marks.len() >= 2 && marks.len() <= 400 {
        for m in 0..marks.len() {
            let lo = marks[m];
            let hi = if m + 1 < marks.len() { marks[m + 1] } else { lines.len() };
            let title_line = lines[lo].trim();
            let stripped = heading_prefix_re().replace(title_line, "").to_string();
            let title: String = stripped.chars().take(120).collect();
            let text = lines[lo..hi].join("\n").trim().to_string();
            if !text.is_empty() {
                let final_title = if title.is_empty() { format!("block {}", m + 1) } else { title };
                blocks.push(Block { title: final_title, text });
            }
        }
        let pre = lines[..marks[0]].join("\n").trim().to_string();
        if tokens_est(&pre) > 120 {
            blocks.insert(0, Block { title: "front matter".to_string(), text: pre });
        }
    } else {
        blocks.push(Block { title: "the text".to_string(), text: lines.join("\n").trim().to_string() });
    }

    const MAX: usize = 3200 * 4;
    const MIN: usize = 160 * 4;
    let mut sized: Vec<Block> = Vec::new();
    for b in blocks {
        if b.text.chars().count() <= MAX {
            sized.push(b);
            continue;
        }
        let paras: Vec<&str> = blank_lines_re().split(&b.text).collect();
        let mut cur = String::new();
        let mut part = 1;
        for p in paras {
            if (cur.chars().count() + p.chars().count()) > MAX && !cur.is_empty() {
                sized.push(Block { title: format!("{} · {}", b.title, part), text: cur.trim().to_string() });
                part += 1;
                cur = String::new();
            }
            cur.push_str(p);
            cur.push_str("\n\n");
        }
        if !cur.trim().is_empty() {
            let title = if part > 1 { format!("{} · {}", b.title, part) } else { b.title.clone() };
            sized.push(Block { title, text: cur.trim().to_string() });
        }
    }
    let mut merged: Vec<Block> = Vec::new();
    for b in sized {
        let should_merge = merged.last().is_some_and(|prev| {
            b.text.chars().count() < MIN && (prev.text.chars().count() + b.text.chars().count()) <= MAX
        });
        if should_merge {
            let prev = merged.last_mut().unwrap();
            prev.text.push_str("\n\n");
            prev.text.push_str(&b.text);
        } else {
            merged.push(b);
        }
    }
    merged
}

// ONE shelving path — 'ingest' (pasted text) and 'ingest_file' (parsed EPUB/PDF) are byte-identical
// past here.
fn shelve(db: &Connection, title: &str, author: &str, goal: &str, content: &str, source_kind: &str, warnings: Value) -> Result<Value, String> {
    let blocks = decompose(content);
    let total: i64 = blocks.iter().map(|b| tokens_est(&b.text)).sum();
    let author_param: Option<String> = if author.is_empty() { None } else { Some(author.to_string()) };
    db.execute(
        "INSERT INTO knowledge.books (title, author, goal, source_kind, tokens_est, blocks) VALUES (?, ?, ?, ?, ?, ?)",
        params![title, author_param, goal, source_kind, total, blocks.len() as i64],
    ).map_err(|e| e.to_string())?;
    let book_id = db.last_insert_rowid();
    {
        let mut ins = db.prepare("INSERT INTO knowledge.book_blocks (book_id, ord, title, text, tokens_est) VALUES (?, ?, ?, ?, ?)").map_err(|e| e.to_string())?;
        for (i, b) in blocks.iter().enumerate() {
            ins.execute(params![book_id, i as i64, b.title, b.text, tokens_est(&b.text)]).map_err(|e| e.to_string())?;
        }
    }
    let census: Vec<Value> = blocks.iter().enumerate().map(|(i, b)| json!({ "ord": i as i64, "title": b.title, "tokens": tokens_est(&b.text) })).collect();
    Ok(json!({
        "ok": true,
        "book": { "id": book_id, "title": title, "blocks": blocks.len() as i64, "tokens_est": total },
        "census": census,
        "warnings": warnings,
    }))
}

pub fn bookIngest(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing('book-ingest', 'db');`
    // — unreachable here (Rust's `db: &Connection` is never absent).
    let op = str_or(input.get("op"), "list");

    if op == "ingest" {
        let title = str_qq(input.get("title")).trim().to_string();
        let content = str_qq(input.get("content"));
        if title.is_empty() {
            return Err("book-ingest: title is required".to_string());
        }
        if content.trim().chars().count() < 400 {
            return Ok(json!({ "ok": false, "error": "the content is too short to be a book — paste the text or markdown, or upload the EPUB/PDF itself (ingest_file)" }));
        }
        return shelve(db, &title, &str_qq(input.get("author")), &str_qq(input.get("goal")), &content, "text", json!([]));
    }

    if op == "ingest_file" {
        // REAL-BUT-UNTESTED-BY-FIXTURE (see module header): reads the real vault filesystem via
        // the already-converted bookParse twin. Only reachable when op == 'ingest_file'.
        let parse_input = json!({ "document_id": input.get("document_id").cloned().unwrap_or(Value::Null) });
        let parsed = crate::agents_in_book_parse::bookParse(&parse_input, db)?;
        if !truthy(parsed.get("ok")) {
            return Ok(json!({
                "ok": false,
                "error": parsed.get("error").cloned().unwrap_or(Value::Null),
                "format": parsed.get("format").cloned().unwrap_or(Value::Null),
                "warnings": parsed.get("warnings").cloned().unwrap_or(Value::Null),
            }));
        }
        let mut title = str_qq(input.get("title")).trim().to_string();
        if title.is_empty() {
            title = str_qq(parsed.get("title")).trim().to_string();
        }
        if title.is_empty() {
            // deterministic last resort: the vault filename stem (txt/md carry no metadata; some
            // PDFs ship without /Title). The operator asked for zero manual naming.
            let doc_id_num = js_number(input.get("document_id"));
            let doc_id_param = if doc_id_num.is_finite() { json!(doc_id_num as i64) } else { Value::Null };
            let doc_rows = crate::db_json::query_rows(db, "SELECT vault_path FROM knowledge.documents WHERE id = ?", &[doc_id_param])?;
            let vault_path = doc_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("vault_path")).cloned();
            let stem = str_qq(vault_path.as_ref());
            let base = stem.rsplit('/').next().unwrap_or("").to_string();
            let no_ext = ext_strip_re().replace(&base, "").to_string();
            title = dash_collapse_re().replace_all(&no_ext, " ").trim().to_string();
        }
        if title.is_empty() {
            return Ok(json!({
                "ok": false,
                "error": "could not derive a title (no metadata, no filename)",
                "format": parsed.get("format").cloned().unwrap_or(Value::Null),
                "warnings": parsed.get("warnings").cloned().unwrap_or(Value::Null),
            }));
        }
        let author = {
            let a = str_qq(input.get("author")).trim().to_string();
            if !a.is_empty() { a } else { str_qq(parsed.get("author")) }
        };
        let goal = str_qq(input.get("goal"));
        let content = parsed.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let source_kind = parsed.get("format").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let warnings = parsed.get("warnings").cloned().unwrap_or(json!([]));
        return shelve(db, &title, &author, &goal, &content, &source_kind, warnings);
    }

    if op == "list" {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT b.id, b.title, b.author, b.goal, b.blocks, b.tokens_est, b.tier, b.created_at, \
             (SELECT status FROM knowledge.book_runs r WHERE r.book_id = b.id ORDER BY r.id DESC LIMIT 1) AS run_status, \
             (SELECT cursor FROM knowledge.book_runs r WHERE r.book_id = b.id ORDER BY r.id DESC LIMIT 1) AS cursor \
             FROM knowledge.books b ORDER BY b.id DESC LIMIT 30",
            &[],
        )?;
        return Ok(json!({ "ok": true, "books": rows }));
    }

    if op == "get" {
        let id_num = js_number(input.get("book_id"));
        if !is_int(id_num) || id_num <= 0.0 {
            return Err("book-ingest: book_id is required".to_string());
        }
        let id = id_num as i64;
        let book_rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.books WHERE id = ?", &[json!(id)])?;
        let Some(book) = book_rows.as_array().and_then(|a| a.first()).cloned() else {
            return Err(format!("book-ingest: book '{}' not found", id));
        };
        let blocks = crate::db_json::query_rows(db, "SELECT ord, title, tokens_est FROM knowledge.book_blocks WHERE book_id = ? ORDER BY ord", &[json!(id)])?;
        let snaps = crate::db_json::query_rows(db, "SELECT block_ord, snapshot_md, quality_json, cost_usd, ai_call_id FROM knowledge.book_snapshots WHERE book_id = ? ORDER BY block_ord", &[json!(id)])?;
        let run_rows = crate::db_json::query_rows(db, "SELECT * FROM knowledge.book_runs WHERE book_id = ? ORDER BY id DESC LIMIT 1", &[json!(id)])?;
        let run = run_rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        let concepts = crate::db_json::query_rows(db, "SELECT id, block_ord, term, definition, ref FROM knowledge.book_concepts WHERE book_id = ? ORDER BY id", &[json!(id)])?;
        let laws = crate::db_json::query_rows(db, "SELECT id, block_ord, key, statement, why, example, antipattern, ref, status FROM knowledge.book_law_drafts WHERE book_id = ? ORDER BY id", &[json!(id)])?;
        let skills = crate::db_json::query_rows(db, "SELECT id, block_ord, name, description, body_md, ref, status FROM knowledge.book_skill_drafts WHERE book_id = ? ORDER BY id", &[json!(id)])?;
        return Ok(json!({ "ok": true, "book": book, "blocks": blocks, "snaps": snaps, "run": run, "concepts": concepts, "laws": laws, "skills": skills }));
    }

    if op == "module" {
        // the WORKBENCH READER — one module's full text (bounded; the FE draws detections on it).
        let id_num = js_number(input.get("book_id"));
        let ord_num = js_number(input.get("ord"));
        if !is_int(id_num) || !is_int(ord_num) {
            return Err("book-ingest: book_id + ord is required".to_string());
        }
        let id = id_num as i64;
        let ord = ord_num as i64;
        let rows = crate::db_json::query_rows(db, "SELECT ord, title, text FROM knowledge.book_blocks WHERE book_id = ? AND ord = ?", &[json!(id), json!(ord)])?;
        let Some(b) = rows.as_array().and_then(|a| a.first()).cloned() else {
            return Err(format!("book-ingest: module '{}' not found", ord));
        };
        let text_full = js_string(b.get("text").unwrap_or(&Value::Null));
        let text_trunc: String = text_full.chars().take(60000).collect();
        return Ok(json!({ "ok": true, "module": { "ord": b.get("ord").cloned().unwrap_or(Value::Null), "title": b.get("title").cloned().unwrap_or(Value::Null), "text": text_trunc } }));
    }

    if op == "annotate" {
        // THE HUMAN'S HAND on the map — assert/reject/move/add a detection. Upsert by identity; the
        // swarm consumes the effective merge, so a correction changes what gets extracted.
        let id_num = js_number(input.get("book_id"));
        let tool = str_qq(input.get("tool"));
        let slot_key = str_qq(input.get("slot_key"));
        let status = str_or(input.get("status"), "confirmed");
        if !is_int(id_num) || tool.is_empty() || slot_key.is_empty() {
            return Err("book-ingest: book_id + tool + slot_key is required".to_string());
        }
        if !["confirmed", "rejected", "moved", "added", "clear"].contains(&status.as_str()) {
            return Err("book-ingest: a valid status (confirmed|rejected|moved|added|clear) is required".to_string());
        }
        let id = id_num as i64;
        if status == "clear" {
            db.execute("DELETE FROM knowledge.book_annotations WHERE book_id = ? AND tool = ? AND slot_key = ?", params![id, tool, slot_key]).map_err(|e| e.to_string())?;
            return Ok(json!({ "ok": true, "cleared": true }));
        }
        let slot_key_trunc: String = slot_key.chars().take(200).collect();
        let block_ord: Option<i64> = nz(input.get("block_ord")).map(|v| js_number(Some(v)) as i64);
        let line: Option<i64> = nz(input.get("line")).map(|v| js_number(Some(v)) as i64);
        let payload = nz(input.get("payload")).cloned().unwrap_or(json!({}));
        let payload_json = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
        let note: Option<String> = nz(input.get("note")).map(|v| js_string(v).chars().take(300).collect());
        db.execute(
            "INSERT INTO knowledge.book_annotations (book_id, tool, slot_key, status, block_ord, line, payload_json, note) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(book_id, tool, slot_key) DO UPDATE SET \
               status = excluded.status, block_ord = excluded.block_ord, line = excluded.line, \
               payload_json = excluded.payload_json, note = excluded.note",
            params![id, tool, slot_key_trunc, status, block_ord, line, payload_json, note],
        ).map_err(|e| e.to_string())?;
        return Ok(json!({ "ok": true }));
    }

    if op == "atlas" {
        // P1 of the Authority Atlas: the deterministic decomposition + THE SWARM PLAN — slots,
        // tokens and a priced estimate BEFORE any AI. Pure recompute; nothing is written.
        let atlas_input = json!({ "book_id": input.get("book_id").cloned().unwrap_or(Value::Null) });
        return crate::agents_in_book_atlas::bookAtlas(&atlas_input, db);
    }

    if op == "harvest" {
        let kind = str_qq(input.get("kind"));
        let id_num = js_number(input.get("id"));
        let status = str_or(input.get("status"), "harvested");
        if kind != "law" && kind != "skill" {
            return Err("book-ingest: kind 'law'|'skill' is required".to_string());
        }
        if !is_int(id_num) || id_num <= 0.0 {
            return Err("book-ingest: id is required".to_string());
        }
        if !["harvested", "dismissed", "draft"].contains(&status.as_str()) {
            return Err("book-ingest: a valid status is required".to_string());
        }
        let id = id_num as i64;
        let table = if kind == "law" { "book_law_drafts" } else { "book_skill_drafts" };
        let sql = format!("UPDATE knowledge.{} SET status = ? WHERE id = ?", table);
        db.execute(&sql, params![status, id]).map_err(|e| e.to_string())?;
        return Ok(json!({ "ok": true }));
    }

    Err("book-ingest: op 'ingest'|'ingest_file'|'list'|'get'|'atlas'|'module'|'annotate'|'harvest' is required".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS knowledge").unwrap();
        c.execute_batch(
            "CREATE TABLE knowledge.books (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, author TEXT, goal TEXT, source_kind TEXT, tokens_est INTEGER, blocks INTEGER, tier TEXT, created_at TEXT);
             CREATE TABLE knowledge.book_blocks (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER NOT NULL, ord INTEGER NOT NULL, title TEXT, text TEXT, tokens_est INTEGER);
             CREATE TABLE knowledge.book_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER NOT NULL, status TEXT, cursor INTEGER);
             CREATE TABLE knowledge.book_snapshots (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER NOT NULL, block_ord INTEGER, snapshot_md TEXT, quality_json TEXT, cost_usd REAL, ai_call_id INTEGER);
             CREATE TABLE knowledge.book_concepts (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER NOT NULL, block_ord INTEGER, term TEXT, definition TEXT, ref TEXT);
             CREATE TABLE knowledge.book_law_drafts (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER NOT NULL, block_ord INTEGER, key TEXT, statement TEXT, why TEXT, example TEXT, antipattern TEXT, ref TEXT, status TEXT);
             CREATE TABLE knowledge.book_skill_drafts (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER NOT NULL, block_ord INTEGER, name TEXT, description TEXT, body_md TEXT, ref TEXT, status TEXT);
             CREATE TABLE knowledge.book_annotations (book_id INTEGER NOT NULL, tool TEXT NOT NULL, slot_key TEXT NOT NULL, status TEXT, block_ord INTEGER, line INTEGER, payload_json TEXT, note TEXT, UNIQUE(book_id, tool, slot_key));",
        ).unwrap();
        c
    }

    #[test]
    fn ingest_decomposes_and_shelves() {
        let c = setup();
        let unit = "Some prose about design patterns and how they help software teams communicate ideas clearly. ".repeat(8);
        let content = format!("# Front Matter\n{}\n\nChapter I\n{}", unit, unit);
        let r = bookIngest(&json!({ "op": "ingest", "title": "Design Patterns Primer", "content": content }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["book"]["blocks"], json!(2));
        assert_eq!(r["census"].as_array().unwrap().len(), 2);

        let got = bookIngest(&json!({ "op": "get", "book_id": 1 }), &c).unwrap();
        assert_eq!(got["book"]["title"], json!("Design Patterns Primer"));
        assert_eq!(got["blocks"].as_array().unwrap().len(), 2);

        let module = bookIngest(&json!({ "op": "module", "book_id": 1, "ord": 0 }), &c).unwrap();
        assert_eq!(module["module"]["title"], json!("Front Matter"));
    }

    #[test]
    fn ingest_too_short_does_not_touch_db() {
        let c = setup();
        let r = bookIngest(&json!({ "op": "ingest", "title": "Tiny", "content": "too short" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        let count: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.books", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn ingest_flat_fallback_single_block() {
        let c = setup();
        let r = bookIngest(&json!({ "op": "ingest", "title": "Flat", "content": "A ".repeat(300) }), &c).unwrap();
        assert_eq!(r["book"]["blocks"], json!(1));
        assert_eq!(r["census"][0]["title"], json!("the text"));
    }

    #[test]
    fn annotate_upserts_then_clears() {
        let c = setup();
        c.execute("INSERT INTO knowledge.books (id, title) VALUES (1, 'B')", []).unwrap();
        let a1 = bookIngest(&json!({ "op": "annotate", "book_id": 1, "tool": "definitions", "slot_key": "k1", "status": "confirmed", "payload": { "x": 1 } }), &c).unwrap();
        assert_eq!(a1["ok"], json!(true));
        let a2 = bookIngest(&json!({ "op": "annotate", "book_id": 1, "tool": "definitions", "slot_key": "k1", "status": "rejected" }), &c).unwrap();
        assert_eq!(a2["ok"], json!(true));
        let status: String = c.query_row("SELECT status FROM knowledge.book_annotations WHERE book_id = 1 AND tool = 'definitions' AND slot_key = 'k1'", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "rejected");
        let cleared = bookIngest(&json!({ "op": "annotate", "book_id": 1, "tool": "definitions", "slot_key": "k1", "status": "clear" }), &c).unwrap();
        assert_eq!(cleared["cleared"], json!(true));
        let count: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.book_annotations", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn harvest_marks_a_law_draft() {
        let c = setup();
        c.execute("INSERT INTO knowledge.books (id, title) VALUES (1, 'B')", []).unwrap();
        c.execute("INSERT INTO knowledge.book_law_drafts (id, book_id, status) VALUES (1, 1, 'draft')", []).unwrap();
        let h = bookIngest(&json!({ "op": "harvest", "kind": "law", "id": 1, "status": "harvested" }), &c).unwrap();
        assert_eq!(h["ok"], json!(true));
        let status: String = c.query_row("SELECT status FROM knowledge.book_law_drafts WHERE id = 1", [], |row| row.get(0)).unwrap();
        assert_eq!(status, "harvested");
    }

    #[test]
    fn unknown_op_errors() {
        let c = setup();
        assert!(bookIngest(&json!({ "op": "bogus" }), &c).is_err());
    }
}
