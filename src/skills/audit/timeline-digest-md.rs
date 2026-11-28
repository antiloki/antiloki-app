//! timeline-digest-md.rs — twin of skills/audit/timeline-digest-md.mjs (db lane). NAMES ARE
//! CONTRACT: `timelineDigestMd` (default). Layer-3 "wiki" digest of a sacred_timeline segment —
//! deterministic, template-rendered, zero AI calls. A regenerable convenience cache over the
//! verifiable record, never the record itself.
//!
//! FS classification: `input.path` is neither a hardcoded constant nor DB-derived — it is a plain
//! `input` field, the MOST directly fixture-controllable case (same precedent as
//! `codebase_atlas_register`). The JS opens a SECOND, independent `DatabaseSync` connection at
//! that real path (not through `context.db`); the twin mirrors this with its own
//! `Connection::open_with_flags(.., SQLITE_OPEN_READ_ONLY)`, not an ATTACH onto `db`. Fixture-
//! tested via two committed testdata files (`fixtures/testdata/timeline-digest-md-segment.sqlite`,
//! `…-empty.sqlite`, built by the sibling `_build-timeline-digest-md-testdata.mjs`) that both
//! lanes open by the same absolute path — same bytes, same engine.
//!
//! The `segment` (no `path`) branch resolves a file path from `context.db`'s own attached
//! 'timeline' file location (`timelinePathFromDb`/`discoverSegments`, inlined here rather than
//! converted from `_helpers/timeline-segments.mjs`, following the precedent set by
//! `timeline-stats.rs`/`verify-chain.rs` — that helper module does real fs I/O out of scope for a
//! shared twin). It is UNREACHABLE via the conform-db fixture harness: the cross-db lane attaches
//! `timeline` as `:memory:` on both sides, so `pragma_database_list.file` is always empty there —
//! ported faithfully (same as the sibling `timeline-stats.rs` archive census) but not fixture-
//! exercised.
//!
//! KNOWN GAP: `.gz` segment decompression (`gunzipToTemp`, node:zlib) has no Rust twin — no
//! compression crate is wired into Cargo.toml for this conversion. The twin returns a clear `Err`
//! for a `.gz` path instead of silently mis-decoding; this is untested by the fixture (only plain
//! `.sqlite` testdata is committed) and is a deliberate, documented follow-up, not a correctness
//! claim over compressed archives.
//!
//! Guard-throws (`segment == null && !path`, `!db`, in-memory timeline, segment not found) are not
//! fixture-exercised (playbook rule 9); `db: &Connection` is non-nullable in Rust so `!db` is
//! additionally unreachable here.
// QUALITY: 80/100 — GROUP BY aggregation (ORDER BY n DESC, k ASC) x4, canonical sha256 rows digest
// ported byte-for-byte from timeline-segments.mjs, GENESIS/null edge fallbacks, real second-connection fs read.

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

const ROWS_DIGEST_ALGO: &str = "rows-v1-sha256";

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }

// timelinePathFromDb: on-disk path of the attached 'timeline' DB, or None for an in-memory
// timeline. See module doc — always None under the conform-db fixture harness.
fn timeline_path_from_db(db: &Connection) -> Option<String> {
    let file: Option<String> = db
        .query_row("SELECT file FROM pragma_database_list WHERE name = 'timeline'", [], |r| r.get::<_, Option<String>>(0))
        .ok()
        .flatten();
    file.filter(|f| !f.is_empty())
}

// discoverSegments(dirname(tlPath)): archived timeline-NNNN-*.sqlite[.gz] files, (file, seq) pairs
// oldest -> newest (the caller only needs file+seq to resolve one requested seq).
fn discover_segments(db_dir: &Path) -> Vec<(String, i64)> {
    let legacy_dir = db_dir.join("legacy_timeline");
    let entries = match std::fs::read_dir(&legacy_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let re = regex::Regex::new(r"^timeline-(\d+)-.*\.sqlite(\.gz)?$").unwrap();
    let mut segs: Vec<(String, i64)> = Vec::new();
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if let Some(caps) = re.captures(name) {
                let seq: i64 = caps[1].parse().unwrap_or(0);
                segs.push((name.to_string(), seq));
            }
        }
    }
    segs.sort_by_key(|s| s.1);
    segs
}

// filePath.replace(/\.sqlite(\.gz)?$/, '.digest.md') — no-match leaves the string unchanged,
// matching JS String.prototype.replace semantics.
fn sqlite_to_digest_path(file_path: &str) -> String {
    if let Some(stripped) = file_path.strip_suffix(".sqlite.gz") {
        format!("{stripped}.digest.md")
    } else if let Some(stripped) = file_path.strip_suffix(".sqlite") {
        format!("{stripped}.digest.md")
    } else {
        file_path.to_string()
    }
}

// canonicalRowsDigest: sha256 over every row (id order), hashing exactly the digital_print input
// fields plus id and the stored prints. Batched reads (batch=500) are a memory optimization only —
// the hashed byte stream is identical regardless of batch size.
fn canonical_rows_digest(conn: &Connection, table_ref: &str) -> Result<String, String> {
    let mut hasher = Sha256::new();
    hasher.update(b"rows-v1");
    let batch = 500i64;
    let mut last_id = 0i64;
    loop {
        let sql = format!("SELECT * FROM {table_ref} WHERE id > ? ORDER BY id ASC LIMIT ?");
        let rows = crate::db_json::query_rows(conn, &sql, &[json!(last_id), json!(batch)])?;
        let rows_arr = rows.as_array().cloned().unwrap_or_default();
        if rows_arr.is_empty() { break; }
        for r in &rows_arr {
            let line = format!(
                "\n{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
                tmpl(r.get("id")), tmpl(r.get("ts")), tmpl(r.get("actor")), tmpl(r.get("operation")),
                tmpl(r.get("entity_type")), tmpl(r.get("entity_id")),
                str_qq(r.get("before_json")), str_qq(r.get("after_json")), str_qq(r.get("payload")),
                tmpl(r.get("prev_print")), tmpl(r.get("digital_print")),
            );
            hasher.update(line.as_bytes());
            last_id = r.get("id").and_then(|v| v.as_i64()).unwrap_or(last_id);
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

fn agg(conn: &Connection, table_ref: &str, col_expr: &str) -> Result<Vec<Value>, String> {
    let sql = format!("SELECT {col_expr} AS k, COUNT(*) AS n FROM {table_ref} GROUP BY {col_expr} ORDER BY n DESC, k ASC");
    let rows = crate::db_json::query_rows(conn, &sql, &[])?;
    Ok(rows.as_array().cloned().unwrap_or_default())
}

struct Stats {
    first_id: Value, last_id: Value, first_ts: Value, last_ts: Value, count: Value,
    first_prev: Value, last_print: Value, rows_digest: String,
    by_operation: Vec<Value>, by_entity_type: Vec<Value>, by_entity: Vec<Value>, by_actor: Vec<Value>,
}

fn stats_from(conn: &Connection, table_ref: &str) -> Result<Stats, String> {
    let edge_sql = format!("SELECT MIN(id) first_id, MAX(id) last_id, MIN(ts) first_ts, MAX(ts) last_ts, COUNT(*) count FROM {table_ref}");
    let edge_rows = crate::db_json::query_rows(conn, &edge_sql, &[])?;
    let edge = edge_rows.as_array().and_then(|a| a.first()).cloned().unwrap_or_else(|| json!({}));

    let first_sql = format!("SELECT prev_print FROM {table_ref} ORDER BY id ASC LIMIT 1");
    let first_rows = crate::db_json::query_rows(conn, &first_sql, &[])?;
    let first_row = first_rows.as_array().and_then(|a| a.first()).cloned();
    let first_prev = first_row.as_ref().and_then(|r| nz(r.get("prev_print")).cloned()).unwrap_or_else(|| json!("GENESIS"));

    let last_sql = format!("SELECT digital_print FROM {table_ref} ORDER BY id DESC LIMIT 1");
    let last_rows = crate::db_json::query_rows(conn, &last_sql, &[])?;
    let last_row = last_rows.as_array().and_then(|a| a.first()).cloned();
    let last_print = last_row.as_ref().and_then(|r| nz(r.get("digital_print")).cloned()).unwrap_or(Value::Null);

    let rows_digest = canonical_rows_digest(conn, table_ref)?;

    Ok(Stats {
        first_id: edge.get("first_id").cloned().unwrap_or(Value::Null),
        last_id: edge.get("last_id").cloned().unwrap_or(Value::Null),
        first_ts: edge.get("first_ts").cloned().unwrap_or(Value::Null),
        last_ts: edge.get("last_ts").cloned().unwrap_or(Value::Null),
        count: edge.get("count").cloned().unwrap_or_else(|| json!(0)),
        first_prev, last_print, rows_digest,
        by_operation: agg(conn, table_ref, "operation")?,
        by_entity_type: agg(conn, table_ref, "entity_type")?,
        by_entity: agg(conn, table_ref, "entity_type || ':' || entity_id")?,
        by_actor: agg(conn, table_ref, "actor")?,
    })
}

fn fmt_top(rows: &[Value], max: usize) -> String {
    if rows.is_empty() { return "_none_".to_string(); }
    rows.iter().take(max).map(|r| {
        let k: String = js_string(r.get("k").unwrap_or(&Value::Null)).chars().take(80).collect();
        format!("| `{k}` | {} |", tmpl(r.get("n")))
    }).collect::<Vec<_>>().join("\n")
}

fn render_digest(segment_file: &str, stats: &Stats) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("# Timeline digest — `{segment_file}`"));
    lines.push(String::new());
    lines.push("> **AI/operator convenience digest — regenerable, NOT the record.**".to_string());
    lines.push("> The provable truth is the archived segment itself, verified against the".to_string());
    lines.push("> boundary row sealed in the live chain (`verify-chain-full`). Regenerate".to_string());
    lines.push("> this file any time via the `timeline-digest-md` skill.".to_string());
    lines.push(String::new());
    lines.push(format!("- **Rows:** {}", tmpl(Some(&stats.count))));
    lines.push(format!(
        "- **Span:** {} → {} (ids {}–{})",
        tmpl(Some(&stats.first_ts)), tmpl(Some(&stats.last_ts)), tmpl(Some(&stats.first_id)), tmpl(Some(&stats.last_id)),
    ));
    lines.push(format!("- **First prev_print:** `{}`", tmpl(Some(&stats.first_prev))));
    lines.push(format!("- **Last digital_print:** `{}`", tmpl(Some(&stats.last_print))));
    lines.push(format!("- **Rows digest ({ROWS_DIGEST_ALGO}):** `{}`", stats.rows_digest));
    lines.push(String::new());
    lines.push("## Operations".to_string());
    lines.push(String::new());
    lines.push("| operation | rows |".to_string());
    lines.push("|---|---|".to_string());
    lines.push(fmt_top(&stats.by_operation, 15));
    lines.push(String::new());
    lines.push("## Entity types".to_string());
    lines.push(String::new());
    lines.push("| entity_type | rows |".to_string());
    lines.push("|---|---|".to_string());
    lines.push(fmt_top(&stats.by_entity_type, 15));
    lines.push(String::new());
    lines.push("## Most-touched entities".to_string());
    lines.push(String::new());
    lines.push("| entity | rows |".to_string());
    lines.push("|---|---|".to_string());
    lines.push(fmt_top(&stats.by_entity, 15));
    lines.push(String::new());
    lines.push("## Actors".to_string());
    lines.push(String::new());
    lines.push("| actor | rows |".to_string());
    lines.push("|---|---|".to_string());
    lines.push(fmt_top(&stats.by_actor, 15));
    lines.push(String::new());
    lines.join("\n")
}

pub fn timelineDigestMd(input: &Value, db: &Connection) -> Result<Value, String> {
    let path_in = nz(input.get("path")).and_then(|v| v.as_str());

    let file_path: String = if let Some(p) = path_in {
        p.to_string()
    } else {
        let segment = nz(input.get("segment"));
        if segment.is_none() {
            return Err("timeline-digest-md: segment (seq) or path is required".to_string());
        }
        let tl_path = timeline_path_from_db(db)
            .ok_or_else(|| "timeline-digest-md: timeline is in-memory — no archives exist".to_string())?;
        let db_dir = Path::new(&tl_path).parent().map(|p| p.to_path_buf()).unwrap_or_else(|| Path::new(&tl_path).to_path_buf());
        let want_seq = js_number(segment);
        let seg_file = discover_segments(&db_dir).into_iter()
            .find(|(_, seq)| *seq as f64 == want_seq)
            .map(|(f, _)| f)
            .ok_or_else(|| format!("timeline-digest-md: no archived segment with seq {}", tmpl(segment)))?;
        db_dir.join("legacy_timeline").join(seg_file).to_string_lossy().into_owned()
    };

    if file_path.ends_with(".gz") {
        return Err("timeline-digest-md: .gz segment decompression is not implemented in the Rust twin (no compression crate wired in)".to_string());
    }

    let conn = Connection::open_with_flags(&file_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("timeline-digest-md: {e}"))?;
    let stats = stats_from(&conn, "sacred_timeline")?;
    let segment_file = file_path.rsplit(['\\', '/']).next().unwrap_or(&file_path).to_string();
    let markdown = render_digest(&segment_file, &stats);

    let digest_path: Value = if truthy(input.get("write")) {
        let dp = sqlite_to_digest_path(&file_path);
        std::fs::write(&dp, &markdown).map_err(|e| format!("timeline-digest-md: {e}"))?;
        json!(dp)
    } else {
        Value::Null
    };

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("segment_file".into(), json!(segment_file));
    out.insert("rows".into(), stats.count);
    out.insert("rows_digest".into(), json!(stats.rows_digest));
    out.insert("digest_path".into(), digest_path);
    out.insert("markdown".into(), json!(markdown));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn build_segment(path: &Path, rows: &[(&str, &str, &str, &str, &str, Option<&str>, Option<&str>, Option<&str>, &str, &str)]) {
        let _ = fs::remove_file(path);
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "CREATE TABLE sacred_timeline (
               id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL, actor TEXT NOT NULL,
               operation TEXT NOT NULL, entity_type TEXT NOT NULL, entity_id TEXT NOT NULL,
               before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT NOT NULL,
               digital_print TEXT NOT NULL UNIQUE
             )",
        ).unwrap();
        for (ts, actor, op, etype, eid, before, after, payload, prev, digital) in rows {
            c.execute(
                "INSERT INTO sacred_timeline (ts, actor, operation, entity_type, entity_id, before_json, after_json, payload, prev_print, digital_print) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                rusqlite::params![ts, actor, op, etype, eid, before, after, payload, prev, digital],
            ).unwrap();
        }
        c.close().unwrap();
    }

    fn db0() -> Connection { Connection::open_in_memory().unwrap() }

    #[test]
    fn digests_a_populated_segment_with_ties_broken_by_key_asc() {
        let path = std::env::temp_dir().join(format!("timeline-digest-md-rs-test-{}.sqlite", std::process::id()));
        build_segment(&path, &[
            ("2025-12-20 08:00:00", "human:dani", "card.add", "card", "CARD-A", None, Some("{\"status\":\"backlog\"}"), None, "GENESIS", "print-0001"),
            ("2025-12-20 09:00:00", "human:dani", "card.update", "card", "CARD-A", Some("{\"status\":\"backlog\"}"), Some("{\"status\":\"doing\"}"), None, "print-0001", "print-0002"),
            ("2025-12-21 10:00:00", "claude", "card.update", "card", "CARD-B", None, None, Some("{\"note\":\"x\"}"), "print-0002", "print-0003"),
            ("2025-12-22 11:00:00", "claude", "law.add", "law", "LAW-1", None, Some("{\"kind\":\"hard\"}"), None, "print-0003", "print-0004"),
            ("2025-12-22 12:00:00", "system:bootstrap", "card.update", "card", "CARD-A", Some("{\"status\":\"doing\"}"), Some("{\"status\":\"done\"}"), None, "print-0004", "print-0005"),
            ("2025-12-23 13:00:00", "human:dani", "card.add", "card", "CARD-C", None, Some("{\"status\":\"backlog\"}"), None, "print-0005", "print-0006"),
        ]);

        let r = timelineDigestMd(&json!({ "path": path.to_string_lossy() }), &db0()).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["rows"], json!(6));
        assert_eq!(r["digest_path"], Value::Null);
        let md = r["markdown"].as_str().unwrap();
        assert!(md.starts_with("# Timeline digest — `"));
        assert!(md.contains("- **Rows:** 6"));
        assert!(md.contains("- **Span:** 2025-12-20 08:00:00 → 2025-12-23 13:00:00 (ids 1–6)"));
        assert!(md.contains("- **First prev_print:** `GENESIS`"));
        assert!(md.contains("- **Last digital_print:** `print-0006`"));
        assert!(md.contains("| `card.update` | 3 |\n| `card.add` | 2 |\n| `law.add` | 1 |"));
        assert!(md.contains("| `card:CARD-A` | 3 |\n| `card:CARD-B` | 1 |\n| `card:CARD-C` | 1 |\n| `law:LAW-1` | 1 |"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn empty_segment_falls_back_to_genesis_null_and_none_tables() {
        let path = std::env::temp_dir().join(format!("timeline-digest-md-rs-test-empty-{}.sqlite", std::process::id()));
        build_segment(&path, &[]);
        let r = timelineDigestMd(&json!({ "path": path.to_string_lossy() }), &db0()).unwrap();
        assert_eq!(r["rows"], json!(0));
        let md = r["markdown"].as_str().unwrap();
        assert!(md.contains("- **Span:** null → null (ids null–null)"));
        assert!(md.contains("- **First prev_print:** `GENESIS`"));
        assert!(md.contains("- **Last digital_print:** `null`"));
        assert!(md.contains("| operation | rows |\n|---|---|\n_none_"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn write_true_writes_the_sibling_digest_md() {
        let path = std::env::temp_dir().join(format!("timeline-digest-md-rs-test-write-{}.sqlite", std::process::id()));
        build_segment(&path, &[("2025-12-20 08:00:00", "a", "card.add", "card", "X", None, None, None, "GENESIS", "p1")]);
        let r = timelineDigestMd(&json!({ "path": path.to_string_lossy(), "write": true }), &db0()).unwrap();
        let digest_path = r["digest_path"].as_str().unwrap().to_string();
        assert!(digest_path.ends_with(".digest.md"));
        let contents = fs::read_to_string(&digest_path).unwrap();
        assert_eq!(contents, r["markdown"].as_str().unwrap());
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&digest_path);
    }

    #[test]
    fn missing_segment_and_path_errs() {
        let e = timelineDigestMd(&json!({}), &db0()).unwrap_err();
        assert!(e.contains("segment (seq) or path"));
    }

    #[test]
    fn gz_path_is_a_documented_unsupported_gap() {
        let e = timelineDigestMd(&json!({ "path": "C:/nowhere/timeline-0001-x.sqlite.gz" }), &db0()).unwrap_err();
        assert!(e.contains("not implemented"));
    }
}
