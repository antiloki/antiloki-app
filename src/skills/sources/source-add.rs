//! source-add.rs — twin of skills/sources/source-add.mjs (default export `sourceAdd`, standard
//! db lane, async-over-sync). NAMES ARE CONTRACT.
//!
//! Registers a raw file as a source (content-addressed: id = 'src-' + sha256(bytes)[:12], so
//! identical bytes dedup to the same row), writes/copies the bytes into the sources dir, indexes
//! it in knowledge.sources, and extracts text (via extract-text.rs's `extractText`) for text-ish
//! files so it's readable + RAG-indexable.
//!
//! REAL FS WRITE (not inert, unlike source-remove.rs's best-effort unlink / source-read.rs's
//! fallback read): the "new source" branch unconditionally `mkdir -p`s + writes the resolved-dir
//! path in BOTH lanes. The JS `dir` is `context?.sourcesDir ?? SOURCES_DIR`; the crossing's
//! db-lane fixture context carries only `db` (source-list.rs / source-get.rs / source-read.rs /
//! source-remove.rs precedent — no `context.sourcesDir` injection), so `dir` resolves to the SAME
//! real on-disk default in both lanes here too — anchored off CARGO_MANIFEST_DIR exactly like
//! those siblings, since antiloki-core-rust and antiloki-core are siblings under the same
//! SUBSTRATE_ROOT. Content-addressing makes this idempotent (same fixture content -> same
//! filename -> same bytes overwritten), so re-running conform is safe and doesn't accumulate.
//! `db_after` + the returned JSON (computed strings, not a disk existence check) are what conform
//! actually judges, so this rides the normal conform-db fixture harness — no bespoke native test
//! needed (contrast with the roots.mjs-derived-path files in Lesson 3, which needed one because
//! their own OUTPUT depended on real fs mutation being observable).
//!
//! `{ path }` INPUT (real-file-read) BRANCH: not exercised by the fixture (no fixture-controlled
//! way to plant a file at a path both lanes resolve identically inside the JSON-only harness —
//! same scoping call as extract-text.rs's `.pdf` branch and source-read.rs's disk-fallback
//! branch). The branch is still written for real, `resolve_path`/`extname`/`basename_of` below
//! are faithful-effort ports (extname is a byte-for-byte port of node:path's algorithm, verified
//! against real `node:path` ground truth; `resolve_path` does NOT collapse `.`/`..` segments like
//! node:path's `resolve` does — DISCLOSED gap, harmless here since it's unexercised and the
//! result is only used for `existsSync`/`readFileSync`/`basename`).
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// present(v) = JS `v != null` (loose): true for anything except a missing key or an explicit null.
fn present(v: Option<&Value>) -> bool {
    matches!(v, Some(x) if !x.is_null())
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

fn sources_dir() -> std::path::PathBuf {
    // JS: SOURCES_DIR = join(SUBSTRATE_ROOT, '.antiloki', 'sources'), anchored at roots.mjs's own
    // file location (antiloki-core's parent). CARGO_MANIFEST_DIR here is antiloki-core-rust; its
    // parent is that same SUBSTRATE_ROOT on this checkout layout (source-list.rs precedent).
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .join(".antiloki")
        .join("sources")
}

// node:path `join(dir, rel)` NORMALIZES separators throughout, not a literal single-component
// append (source-list.rs precedent / conform-db fixture case that caught the divergence).
fn join_rel(dir: &std::path::Path, rel: &str) -> std::path::PathBuf {
    let mut pb = dir.to_path_buf();
    for part in rel.split(['/', '\\']) {
        if !part.is_empty() {
            pb.push(part);
        }
    }
    pb
}

// node:path `basename(p)` — last path component (unexercised `{ path }` branch; simple port).
fn basename_of(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

// node:path `resolve(p)` for the unexercised `{ path }` branch: absolute stays as-is, relative is
// anchored at the process cwd. DISCLOSED gap: unlike node:path, this does NOT collapse `.`/`..`
// segments — acceptable here since the result only feeds existsSync/readFileSync/basename, never
// compared as a string, and the branch is never fixture-exercised (see module doc).
fn resolve_path(p: &str) -> std::path::PathBuf {
    let pb = std::path::Path::new(p);
    if pb.is_absolute() {
        pb.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(pb)
    }
}

// node:path `extname(path)` — byte-for-byte port of the real algorithm (leading-dot hidden files
// like ".gitignore" -> "", a lone trailing dot like "index." -> ".", multi-dot picks the LAST
// run). Verified against real `node:path` ground truth for the edge cases in the module doc.
fn extname(path: &str) -> String {
    let chars: Vec<char> = path.chars().collect();
    let len = chars.len() as i64;
    let mut start_dot: i64 = -1;
    let start_part: i64 = 0; // no path-separator scanning needed: `filename` here is already a basename
    let mut end: i64 = -1;
    let mut pre_dot_state: i32 = 0;
    let mut i = len - 1;
    while i >= 0 {
        let c = chars[i as usize];
        if c == '/' || c == '\\' {
            break; // filename has no separators in practice; bail defensively rather than misparse
        }
        if end == -1 {
            end = i + 1;
        }
        if c == '.' {
            if start_dot == -1 {
                start_dot = i;
            } else if pre_dot_state != 1 {
                pre_dot_state = 1;
            }
        } else if start_dot != -1 {
            pre_dot_state = -1;
        }
        i -= 1;
    }
    if start_dot == -1
        || end == -1
        || pre_dot_state == 0
        || (pre_dot_state == 1 && start_dot == end - 1 && start_dot == start_part + 1)
    {
        return String::new();
    }
    chars[start_dot as usize..end as usize].iter().collect()
}

fn mime_for_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        ".md" | ".markdown" => "text/markdown",
        ".txt" => "text/plain",
        ".pdf" => "application/pdf",
        ".png" => "image/png",
        ".jpg" | ".jpeg" => "image/jpeg",
        ".gif" => "image/gif",
        ".svg" => "image/svg+xml",
        ".json" => "application/json",
        ".csv" => "text/csv",
        ".html" => "text/html",
        ".xml" => "application/xml",
        _ => return None,
    })
}

pub fn sourceAdd(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::sources_sources_schema::ensureSourcesSchema(db, &json!("knowledge."));
    // JS: if (!db) throw err.contextMissing('source-add','db') — unreachable in the db lane (a
    // connection is always injected).
    let dir = sources_dir();

    // const p = resolve(String(input.path)); ... filename = basename(p)
    let path_val = input.get("path");
    let (filename, buf): (String, Vec<u8>) = if truthy(path_val) {
        let p = resolve_path(&js_string(path_val.unwrap()));
        let p_str = p.to_string_lossy().into_owned();
        if !p.exists() {
            return Ok(json!({ "ok": false, "error": format!("file not found: {p_str}") }));
        }
        match std::fs::read(&p) {
            Ok(b) => (basename_of(&p_str), b),
            Err(e) => return Ok(json!({ "ok": false, "error": format!("could not read {p_str}: {e}") })),
        }
    } else if present(input.get("filename")) && (present(input.get("content")) || present(input.get("content_base64"))) {
        let filename = js_string(input.get("filename").unwrap());
        let content_b64 = input.get("content_base64");
        let buf = if present(content_b64) {
            use base64::Engine;
            let s = js_string(content_b64.unwrap());
            // node Buffer.from(str,'base64') decodes leniently (ignores stray chars, tolerates
            // missing padding); the `base64` crate's STANDARD engine is strict. Fixtures/tests
            // here only feed well-formed base64 (matching extract-text.rs's precedent), so this
            // gap is disclosed but not exercised.
            base64::engine::general_purpose::STANDARD
                .decode(s.as_bytes())
                .map_err(|e| format!("source-add: invalid content_base64: {e}"))?
        } else {
            js_string(input.get("content").unwrap()).into_bytes()
        };
        (filename, buf)
    } else {
        return Ok(json!({ "ok": false, "error": "provide { path } OR { filename, content } OR { filename, content_base64 }" }));
    };

    let sha = hex::encode(Sha256::digest(&buf));
    let id = format!("src-{}", &sha[..12]);

    // content-addressed dedup: identical bytes -> same id -> return the existing entry untouched.
    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, filename, rel_path, ext, mime, sha256, bytes, title, added_at FROM knowledge.sources WHERE id = ?",
        &[json!(id.clone())],
    )?;
    if let Some(existing) = rows.as_array().and_then(|a| a.first()) {
        let mut o = existing.as_object().cloned().unwrap_or_default();
        let rel = o.get("rel_path").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let abs = join_rel(&dir, &rel).to_string_lossy().into_owned();
        o.insert("abs_path".into(), json!(abs));
        return Ok(json!({ "ok": true, "deduped": true, "source": Value::Object(o) }));
    }

    let ext = extname(&filename).to_lowercase();
    let rel = if ext.is_empty() { id.clone() } else { format!("{id}{ext}") };
    let abs = join_rel(&dir, &rel);
    std::fs::create_dir_all(&dir).map_err(|e| format!("source-add: {e}"))?;
    std::fs::write(&abs, &buf).map_err(|e| format!("source-add: {e}"))?;

    use base64::Engine;
    let buf_arg = json!({ "__bytes_b64": base64::engine::general_purpose::STANDARD.encode(&buf) });
    let text = crate::sources_extract_text::extractText(&buf_arg, &json!(ext.clone()))?;

    let mime = mime_for_ext(&ext)
        .map(|m| m.to_string())
        .unwrap_or_else(|| if !text.is_null() { "text/plain".to_string() } else { "application/octet-stream".to_string() });
    let title_val = input.get("title");
    let title = if truthy(title_val) { js_string(title_val.unwrap()) } else { filename.clone() };
    let added_by = if truthy(input.get("added_by")) { js_string(input.get("added_by").unwrap()) } else { "operator".to_string() };

    crate::db_json::run_write(
        db,
        "INSERT INTO knowledge.sources (id, filename, rel_path, ext, mime, sha256, bytes, text, title, added_by) VALUES (?,?,?,?,?,?,?,?,?,?)",
        &[
            json!(id.clone()),
            json!(filename),
            json!(rel),
            json!(ext),
            json!(mime),
            json!(sha),
            json!(buf.len() as i64),
            text,
            json!(title),
            json!(added_by),
        ],
    )?;

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, filename, rel_path, ext, mime, sha256, bytes, title, added_at FROM knowledge.sources WHERE id = ?",
        &[json!(id)],
    )?;
    let row = rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
    let mut o = row.as_object().cloned().unwrap_or_default();
    o.insert("abs_path".into(), json!(abs.to_string_lossy().into_owned()));

    Ok(json!({ "ok": true, "source": Value::Object(o) }))
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
    fn extname_matches_node_ground_truth() {
        assert_eq!(extname("index.html"), ".html");
        assert_eq!(extname("index."), ".");
        assert_eq!(extname("index"), "");
        assert_eq!(extname(".index"), "");
        assert_eq!(extname(".index.md"), ".md");
        assert_eq!(extname("a.b.c"), ".c");
        assert_eq!(extname("noext"), "");
        assert_eq!(extname(".gitignore"), "");
        assert_eq!(extname("a..b"), ".b");
        assert_eq!(extname("file.TXT"), ".TXT");
    }

    #[test]
    fn no_input_variant_returns_error() {
        let c = open_knowledge_db();
        let r = sourceAdd(&json!({}), &c).unwrap();
        assert_eq!(
            r,
            json!({ "ok": false, "error": "provide { path } OR { filename, content } OR { filename, content_base64 }" })
        );
        let r2 = sourceAdd(&json!({ "filename": "noext" }), &c).unwrap();
        assert_eq!(
            r2,
            json!({ "ok": false, "error": "provide { path } OR { filename, content } OR { filename, content_base64 }" })
        );
    }

    #[test]
    fn filename_content_registers_writes_and_extracts_text() {
        let c = open_knowledge_db();
        let r = sourceAdd(&json!({ "filename": "hello.md", "content": "hello world" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["source"]["id"], json!("src-b94d27b9934d"));
        assert_eq!(r["source"]["filename"], json!("hello.md"));
        assert_eq!(r["source"]["rel_path"], json!("src-b94d27b9934d.md"));
        assert_eq!(r["source"]["ext"], json!(".md"));
        assert_eq!(r["source"]["mime"], json!("text/markdown"));
        assert_eq!(r["source"]["bytes"], json!(11));
        assert_eq!(r["source"]["title"], json!("hello.md"));
        let abs = r["source"]["abs_path"].as_str().unwrap().to_string();
        assert!(abs.ends_with("src-b94d27b9934d.md"));
        assert_eq!(std::fs::read_to_string(&abs).unwrap(), "hello world");
        let text: String = c
            .query_row("SELECT text FROM knowledge.sources WHERE id='src-b94d27b9934d'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(text, "hello world");
        let _ = std::fs::remove_file(&abs);
    }

    #[test]
    fn identical_bytes_dedup_to_same_id_without_rewriting_filename() {
        // Content-addressed storage means IDENTICAL CONTENT IS A SHARED FILE PATH across tests.
        // This body used to be "hello world", the same as
        // filename_content_registers_writes_and_extracts_text — so both tests wrote and then
        // remove_file'd src-b94d27b9934d.md concurrently, and one would intermittently die with
        // "Access is denied (os error 5)" on Windows. The content only has to be identical
        // BETWEEN this test's two calls; it must be unique to this test.
        let c = open_knowledge_db();
        let first = sourceAdd(&json!({ "filename": "hello.md", "content": "hello dedup world" }), &c).unwrap();
        let again = sourceAdd(&json!({ "filename": "hello-again.md", "content": "hello dedup world" }), &c).unwrap();
        assert_eq!(again["ok"], json!(true));
        assert_eq!(again["deduped"], json!(true));
        assert_eq!(again["source"]["id"], first["source"]["id"]);
        assert_eq!(again["source"]["filename"], json!("hello.md"));
        assert_eq!(again["source"]["abs_path"], first["source"]["abs_path"]);
        let count: i64 = c.query_row("SELECT COUNT(*) FROM knowledge.sources", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
        let abs = first["source"]["abs_path"].as_str().unwrap().to_string();
        let _ = std::fs::remove_file(&abs);
    }

    #[test]
    fn content_base64_decodes_binary_falls_back_to_octet_stream() {
        let c = open_knowledge_db();
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode([0u8, 1, 2, 3]);
        let r = sourceAdd(&json!({ "filename": "bin.dat", "content_base64": b64 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["source"]["ext"], json!(".dat"));
        assert_eq!(r["source"]["mime"], json!("application/octet-stream"));
        assert_eq!(r["source"]["bytes"], json!(4));
        let abs = r["source"]["abs_path"].as_str().unwrap().to_string();
        assert_eq!(std::fs::read(&abs).unwrap(), vec![0u8, 1, 2, 3]);
        let text: Value = c
            .query_row("SELECT text FROM knowledge.sources WHERE id=?", [r["source"]["id"].as_str().unwrap()], |row| {
                let t: Option<String> = row.get(0)?;
                Ok(t.map(Value::String).unwrap_or(Value::Null))
            })
            .unwrap();
        assert_eq!(text, Value::Null);
        let _ = std::fs::remove_file(&abs);
    }

    #[test]
    fn text_ext_with_no_mime_entry_falls_back_to_text_plain() {
        let c = open_knowledge_db();
        let r = sourceAdd(&json!({ "filename": "code.rs", "content": "fn main(){}" }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["source"]["ext"], json!(".rs"));
        assert_eq!(r["source"]["mime"], json!("text/plain"));
        let abs = r["source"]["abs_path"].as_str().unwrap().to_string();
        let _ = std::fs::remove_file(&abs);
    }

    #[test]
    fn explicit_title_overrides_filename_default() {
        let c = open_knowledge_db();
        let r = sourceAdd(&json!({ "filename": "titled.md", "content": "x", "title": "My Title" }), &c).unwrap();
        assert_eq!(r["source"]["title"], json!("My Title"));
        let abs = r["source"]["abs_path"].as_str().unwrap().to_string();
        let _ = std::fs::remove_file(&abs);
    }
}
