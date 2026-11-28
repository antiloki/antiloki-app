//! timeline-legacy-read.rs — twin of skills/audit/timeline-legacy-read.mjs. NAMES ARE CONTRACT:
//! `timelineLegacyRead` (default). Reads archived sacred_timeline segments (rotated out of the
//! live chain into `legacy_timeline/*.sqlite[.gz]`) without ever touching the live DB.
//!
//!   `{}`                                  → list segments + per-segment metadata
//!   `{ segment: 3 | "timeline-0003-…" }`  → rows of that segment (paginated)
//!   `{ segment, entity_type, entity_id }` → that entity's events in the segment
//!
//! BESPOKE-NATIVE (no conform-db fixture/frontier arm): this skill's entire observable output is
//! driven by which archive files exist on a real filesystem (`legacy_timeline/`), each one a real
//! SQLite file it opens read-only (transparently gunzipping `.gz` ones to a real scratch temp
//! file via `gunzipToTemp`) — conform-db's db-seed → db_after model has no notion of "other SQLite
//! files sitting next to this one on disk", same category as `gzipSegmentFile`/`gunzipToTemp`
//! themselves in timeline-segments.rs. Proven below via `#[cfg(test)]` against real temp dirs and
//! real (plain + gzip'd) SQLite archive files.
//!
//! `context.db` is always injected by the db lane in production, so the JS `if (!db) throw
//! err.contextMissing(...)` guard is unreachable here (mirrors every sibling twin's convention).
//!
//! Cross-module calls (pre-existing Rust twins, `crate::audit__helpers_timeline_segments::*`):
//! `timelinePathFromDb`, `legacyDirFor`, `discoverSegments`, `gunzipToTemp`.
//!
//! Error messages mirror `AntilokiError`'s `"${skill}: ${cause}"` rendering exactly:
//! - segment not found → `"timeline-legacy-read: segment '<value>' not found"` (`err.notFound`).
//! - any open/query failure inside a segment read → `"timeline-legacy-read: segment '<file>' is
//!   unreadable: <underlying message>"` (the JS `catch` re-wraps as `INTERNAL_ERROR`; a per-segment
//!   error INSIDE list mode is instead swallowed into that segment's own `error` field so one bad
//!   archive never aborts the whole listing — see JS module doc).

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::path::PathBuf;

fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }

fn js_number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => if *b { 1.0 } else { 0.0 },
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        _ => f64::NAN,
    }
}

fn js_to_string(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// `s.file === segment || s.seq === Number(segment) || String(s.seq) === String(segment)`
fn matches_segment(seg: &Value, segment: &Value) -> bool {
    let seg_file = seg.get("file").and_then(Value::as_str).unwrap_or("");
    if let Some(sv) = segment.as_str() {
        if seg_file == sv {
            return true;
        }
    }
    let seg_seq = seg.get("seq").and_then(Value::as_i64).unwrap_or(i64::MIN);
    let num_seg = js_number(segment);
    if !num_seg.is_nan() && (seg_seq as f64) == num_seg {
        return true;
    }
    seg_seq.to_string() == js_to_string(segment)
}

// Open an archived segment read-only, transparently gunzipping compressed ones to a scratch temp
// dir. Returns (conn, temp_dir_to_clean_up_after). Mirrors JS's `openSegment`/`done()` pair.
fn open_segment(legacy_dir: &str, seg: &Value) -> Result<(Connection, Option<PathBuf>), String> {
    let file = seg.get("file").and_then(Value::as_str).unwrap_or("");
    let gz = seg.get("gz").and_then(Value::as_bool).unwrap_or(false);
    let path = PathBuf::from(legacy_dir).join(file);
    if gz {
        let (out_path, temp_dir) =
            crate::audit__helpers_timeline_segments::gunzipToTemp(path.to_str().unwrap_or(""))
                .map_err(|e| e.to_string())?;
        let conn = Connection::open_with_flags(&out_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
        Ok((conn, Some(temp_dir)))
    } else {
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
        Ok((conn, None))
    }
}

fn close_segment(temp_dir: Option<PathBuf>) {
    if let Some(dir) = temp_dir {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[allow(non_snake_case)]
pub fn timelineLegacyRead(input: &Value, db: &Connection) -> Result<Value, String> {
    let tl_path = crate::audit__helpers_timeline_segments::timelinePathFromDb(db);
    let tl_path_str = match tl_path.as_str() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Ok(json!({ "segments": [], "reason": "in-memory timeline — no legacy archive" })),
    };
    let db_dir = std::path::Path::new(&tl_path_str)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let legacy_dir = crate::audit__helpers_timeline_segments::legacyDirFor(&json!(db_dir));
    let legacy_dir_str = legacy_dir.as_str().unwrap_or("").to_string();
    let segs = crate::audit__helpers_timeline_segments::discoverSegments(&json!(db_dir));
    let segs_arr = segs.as_array().cloned().unwrap_or_default();

    // `const { segment = null, entity_type = null, entity_id = null, limit = 100, offset = 0 } =
    // input ?? {}` — default fires only when the key is MISSING; a present `null` stays null.
    let segment_v = input.get("segment").cloned().unwrap_or(Value::Null);
    let entity_type_v = input.get("entity_type").cloned().unwrap_or(Value::Null);
    let entity_id_v = input.get("entity_id").cloned().unwrap_or(Value::Null);
    let limit_v = if input.get("limit").is_some() { input["limit"].clone() } else { json!(100) };
    let offset_v = if input.get("offset").is_some() { input["offset"].clone() } else { json!(0) };

    if segment_v.is_null() {
        // List mode: one summary row per archived segment. One bad archive must not crash the
        // whole listing — flag it inline via an `error` field instead.
        let mut segments_out: Vec<Value> = Vec::with_capacity(segs_arr.len());
        for s in &segs_arr {
            let file = s.get("file").cloned().unwrap_or(Value::Null);
            let seq = s.get("seq").cloned().unwrap_or(Value::Null);
            let compressed = s.get("gz").and_then(Value::as_bool).unwrap_or(false);

            let attempt = open_segment(&legacy_dir_str, s).and_then(|(conn, temp_dir)| {
                let r = (|| -> Result<Value, String> {
                    let n = crate::db_json::query_rows(&conn, "SELECT COUNT(*) n FROM sacred_timeline", &[])?
                        .as_array()
                        .and_then(|a| a.first())
                        .and_then(|row| row.get("n").cloned())
                        .unwrap_or(json!(0));
                    let first = crate::db_json::query_rows(
                        &conn,
                        "SELECT ts, prev_print, digital_print FROM sacred_timeline ORDER BY id ASC LIMIT 1",
                        &[],
                    )?
                    .as_array()
                    .and_then(|a| a.first().cloned());
                    let last = crate::db_json::query_rows(
                        &conn,
                        "SELECT ts, digital_print FROM sacred_timeline ORDER BY id DESC LIMIT 1",
                        &[],
                    )?
                    .as_array()
                    .and_then(|a| a.first().cloned());
                    Ok(json!({
                        "seq": seq,
                        "file": file,
                        "compressed": compressed,
                        "rows": n,
                        "first_ts": first.as_ref().and_then(|f| f.get("ts")).cloned().unwrap_or(Value::Null),
                        "last_ts": last.as_ref().and_then(|f| f.get("ts")).cloned().unwrap_or(Value::Null),
                        "start_prev": first.as_ref().and_then(|f| f.get("prev_print")).cloned().unwrap_or(Value::Null),
                        "last_print": last.as_ref().and_then(|f| f.get("digital_print")).cloned().unwrap_or(Value::Null),
                    }))
                })();
                close_segment(temp_dir);
                r
            });

            segments_out.push(match attempt {
                Ok(v) => v,
                Err(e) => json!({ "seq": seq, "file": file, "error": e }),
            });
        }
        let count = segments_out.len() as i64;
        return Ok(json!({ "segments": segments_out, "count": count }));
    }

    // Read mode: locate the segment by filename or by sequence number.
    let seg = segs_arr
        .iter()
        .find(|s| matches_segment(s, &segment_v))
        .cloned()
        .ok_or_else(|| format!("timeline-legacy-read: segment '{}' not found", js_to_string(&segment_v)))?;

    let seg_file = seg.get("file").cloned().unwrap_or(Value::Null);
    let seg_seq = seg.get("seq").cloned().unwrap_or(Value::Null);
    let seg_file_str = seg_file.as_str().unwrap_or("").to_string();

    let opened = open_segment(&legacy_dir_str, &seg);
    let (conn, temp_dir) = match opened {
        Ok(x) => x,
        Err(e) => return Err(format!("timeline-legacy-read: segment '{seg_file_str}' is unreadable: {e}")),
    };

    let result = (|| -> Result<Value, String> {
        let total = crate::db_json::query_rows(&conn, "SELECT COUNT(*) n FROM sacred_timeline", &[])?
            .as_array()
            .and_then(|a| a.first())
            .and_then(|row| row.get("n").cloned())
            .unwrap_or(json!(0));

        let rows = if !entity_type_v.is_null() && !entity_id_v.is_null() {
            crate::db_json::query_rows(
                &conn,
                "SELECT * FROM sacred_timeline WHERE entity_type = ? AND entity_id = ? ORDER BY id ASC LIMIT ? OFFSET ?",
                &[entity_type_v.clone(), json!(str_qq(Some(&entity_id_v))), limit_v.clone(), offset_v.clone()],
            )?
        } else if !entity_type_v.is_null() {
            crate::db_json::query_rows(
                &conn,
                "SELECT * FROM sacred_timeline WHERE entity_type = ? ORDER BY id ASC LIMIT ? OFFSET ?",
                &[entity_type_v.clone(), limit_v.clone(), offset_v.clone()],
            )?
        } else {
            crate::db_json::query_rows(
                &conn,
                "SELECT * FROM sacred_timeline ORDER BY id ASC LIMIT ? OFFSET ?",
                &[limit_v.clone(), offset_v.clone()],
            )?
        };

        let returned = rows.as_array().map(|a| a.len()).unwrap_or(0) as i64;

        Ok(json!({
            "segment": seg_file,
            "seq": seg_seq,
            "rows": rows,
            "returned": returned,
            "total": total,
            "limit": limit_v,
            "offset": offset_v,
        }))
    })();

    close_segment(temp_dir);
    result.map_err(|e| format!("timeline-legacy-read: segment '{seg_file_str}' is unreadable: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dbdir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-timeline-legacy-read-rs-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Wires up: a live `db` connection with a real (empty) attached 'timeline' file — so
    // `timelinePathFromDb` resolves to a real path — plus a `legacy_timeline/` sibling dir.
    fn live_db_over(dbdir: &PathBuf) -> Connection {
        let live_path = dbdir.join("timeline.sqlite");
        Connection::open(&live_path).unwrap(); // create+close: just needs to exist on disk
        let conn = Connection::open_in_memory().unwrap();
        let escaped = live_path.to_string_lossy().replace('\'', "''");
        conn.execute_batch(&format!("ATTACH '{escaped}' AS timeline")).unwrap();
        conn
    }

    fn write_archive(dbdir: &PathBuf, filename: &str, rows: &[(&str, &str, &str)]) -> PathBuf {
        let legacy = dbdir.join("legacy_timeline");
        std::fs::create_dir_all(&legacy).unwrap();
        let path = legacy.join(filename);
        let c = Connection::open(&path).unwrap();
        c.execute_batch(
            "CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, actor TEXT, \
             operation TEXT, entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, \
             payload TEXT, prev_print TEXT, digital_print TEXT)",
        )
        .unwrap();
        for (ts, entity_type, entity_id) in rows {
            c.execute(
                "INSERT INTO sacred_timeline (ts, actor, operation, entity_type, entity_id, prev_print, digital_print) \
                 VALUES (?1, 'tester', 'card.add', ?2, ?3, 'PREV', ?4)",
                rusqlite::params![ts, entity_type, entity_id, format!("H-{entity_id}")],
            )
            .unwrap();
        }
        drop(c);
        path
    }

    #[test]
    fn in_memory_timeline_returns_empty_segments_with_reason() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS timeline").unwrap();
        let r = timelineLegacyRead(&json!({}), &c).unwrap();
        assert_eq!(r["segments"], json!([]));
        assert_eq!(r["reason"], json!("in-memory timeline — no legacy archive"));
    }

    #[test]
    fn list_mode_summarizes_plain_and_gzipped_segments() {
        let dbdir = temp_dbdir("list");
        write_archive(&dbdir, "timeline-0001-a.sqlite", &[("2026-01-01T00:00:00.000Z", "card", "C1"), ("2026-01-02T00:00:00.000Z", "card", "C2")]);
        let plain2 = write_archive(&dbdir, "timeline-0002-b.sqlite", &[("2026-02-01T00:00:00.000Z", "card", "C3")]);
        let gz_path = crate::audit__helpers_timeline_segments::gzipSegmentFile(plain2.to_str().unwrap()).unwrap();
        assert!(PathBuf::from(&gz_path).exists());

        let db = live_db_over(&dbdir);
        let r = timelineLegacyRead(&json!({}), &db).unwrap();
        assert_eq!(r["count"], json!(2));
        let segs = r["segments"].as_array().unwrap();
        assert_eq!(segs[0]["seq"], json!(1));
        assert_eq!(segs[0]["file"], json!("timeline-0001-a.sqlite"));
        assert_eq!(segs[0]["compressed"], json!(false));
        assert_eq!(segs[0]["rows"], json!(2));
        assert_eq!(segs[0]["first_ts"], json!("2026-01-01T00:00:00.000Z"));
        assert_eq!(segs[0]["last_ts"], json!("2026-01-02T00:00:00.000Z"));

        assert_eq!(segs[1]["seq"], json!(2));
        assert_eq!(segs[1]["compressed"], json!(true));
        assert_eq!(segs[1]["rows"], json!(1));

        let _ = std::fs::remove_dir_all(&dbdir);
    }

    #[test]
    fn list_mode_flags_unreadable_archive_without_aborting_the_rest() {
        let dbdir = temp_dbdir("bad-archive");
        write_archive(&dbdir, "timeline-0001-good.sqlite", &[("2026-01-01T00:00:00.000Z", "card", "C1")]);
        let legacy = dbdir.join("legacy_timeline");
        std::fs::write(legacy.join("timeline-0002-bad.sqlite"), b"not a real sqlite file at all").unwrap();

        let db = live_db_over(&dbdir);
        let r = timelineLegacyRead(&json!({}), &db).unwrap();
        assert_eq!(r["count"], json!(2));
        let segs = r["segments"].as_array().unwrap();
        assert_eq!(segs[0]["rows"], json!(1));
        assert!(segs[1].get("error").is_some());
        assert_eq!(segs[1]["file"], json!("timeline-0002-bad.sqlite"));

        let _ = std::fs::remove_dir_all(&dbdir);
    }

    #[test]
    fn read_mode_by_seq_number_paginates_and_totals() {
        let dbdir = temp_dbdir("read-by-seq");
        write_archive(
            &dbdir,
            "timeline-0001-a.sqlite",
            &[
                ("2026-01-01T00:00:00.000Z", "card", "C1"),
                ("2026-01-02T00:00:00.000Z", "card", "C2"),
                ("2026-01-03T00:00:00.000Z", "mission", "M1"),
            ],
        );
        let db = live_db_over(&dbdir);

        let r = timelineLegacyRead(&json!({ "segment": 1, "limit": 2, "offset": 0 }), &db).unwrap();
        assert_eq!(r["segment"], json!("timeline-0001-a.sqlite"));
        assert_eq!(r["seq"], json!(1));
        assert_eq!(r["total"], json!(3));
        assert_eq!(r["returned"], json!(2));
        assert_eq!(r["rows"].as_array().unwrap().len(), 2);
        assert_eq!(r["rows"][0]["entity_id"], json!("C1"));

        let r2 = timelineLegacyRead(&json!({ "segment": 1, "limit": 2, "offset": 2 }), &db).unwrap();
        assert_eq!(r2["returned"], json!(1));
        assert_eq!(r2["rows"][0]["entity_id"], json!("M1"));

        let _ = std::fs::remove_dir_all(&dbdir);
    }

    #[test]
    fn read_mode_by_filename_string_and_seq_as_string_both_resolve() {
        let dbdir = temp_dbdir("read-by-file");
        write_archive(&dbdir, "timeline-0007-x.sqlite", &[("2026-01-01T00:00:00.000Z", "card", "C1")]);
        let db = live_db_over(&dbdir);

        let by_file = timelineLegacyRead(&json!({ "segment": "timeline-0007-x.sqlite" }), &db).unwrap();
        assert_eq!(by_file["seq"], json!(7));

        let by_str_seq = timelineLegacyRead(&json!({ "segment": "7" }), &db).unwrap();
        assert_eq!(by_str_seq["segment"], json!("timeline-0007-x.sqlite"));

        let _ = std::fs::remove_dir_all(&dbdir);
    }

    #[test]
    fn read_mode_filters_by_entity_type_and_entity_id() {
        let dbdir = temp_dbdir("read-filter");
        write_archive(
            &dbdir,
            "timeline-0001-a.sqlite",
            &[
                ("2026-01-01T00:00:00.000Z", "card", "C1"),
                ("2026-01-02T00:00:00.000Z", "card", "C2"),
                ("2026-01-03T00:00:00.000Z", "mission", "C1"),
            ],
        );
        let db = live_db_over(&dbdir);

        let by_type = timelineLegacyRead(&json!({ "segment": 1, "entity_type": "card" }), &db).unwrap();
        assert_eq!(by_type["returned"], json!(2));

        let by_both = timelineLegacyRead(&json!({ "segment": 1, "entity_type": "card", "entity_id": "C1" }), &db).unwrap();
        assert_eq!(by_both["returned"], json!(1));
        assert_eq!(by_both["rows"][0]["entity_type"], json!("card"));
        assert_eq!(by_both["rows"][0]["entity_id"], json!("C1"));

        let _ = std::fs::remove_dir_all(&dbdir);
    }

    #[test]
    fn read_mode_gzipped_segment_transparently_decompresses() {
        let dbdir = temp_dbdir("read-gz");
        let plain = write_archive(&dbdir, "timeline-0003-c.sqlite", &[("2026-03-01T00:00:00.000Z", "card", "C9")]);
        crate::audit__helpers_timeline_segments::gzipSegmentFile(plain.to_str().unwrap()).unwrap();
        assert!(!plain.exists());

        let db = live_db_over(&dbdir);
        let r = timelineLegacyRead(&json!({ "segment": 3 }), &db).unwrap();
        assert_eq!(r["total"], json!(1));
        assert_eq!(r["rows"][0]["entity_id"], json!("C9"));

        let _ = std::fs::remove_dir_all(&dbdir);
    }

    #[test]
    fn segment_not_found_returns_exact_error_message() {
        let dbdir = temp_dbdir("not-found");
        write_archive(&dbdir, "timeline-0001-a.sqlite", &[("2026-01-01T00:00:00.000Z", "card", "C1")]);
        let db = live_db_over(&dbdir);

        let e = timelineLegacyRead(&json!({ "segment": 99 }), &db).unwrap_err();
        assert_eq!(e, "timeline-legacy-read: segment '99' not found");

        let e2 = timelineLegacyRead(&json!({ "segment": "no-such-file.sqlite" }), &db).unwrap_err();
        assert_eq!(e2, "timeline-legacy-read: segment 'no-such-file.sqlite' not found");

        let _ = std::fs::remove_dir_all(&dbdir);
    }

    #[test]
    fn read_mode_unreadable_archive_wraps_error_with_segment_context() {
        let dbdir = temp_dbdir("read-bad");
        let legacy = dbdir.join("legacy_timeline");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("timeline-0005-corrupt.sqlite"), b"garbage bytes, not sqlite").unwrap();
        let db = live_db_over(&dbdir);

        let e = timelineLegacyRead(&json!({ "segment": 5 }), &db).unwrap_err();
        assert!(e.starts_with("timeline-legacy-read: segment 'timeline-0005-corrupt.sqlite' is unreadable:"));

        let _ = std::fs::remove_dir_all(&dbdir);
    }

    #[test]
    fn no_legacy_dir_yields_empty_segment_list() {
        let dbdir = temp_dbdir("no-legacy-dir");
        let db = live_db_over(&dbdir);
        let r = timelineLegacyRead(&json!({}), &db).unwrap();
        assert_eq!(r["segments"], json!([]));
        assert_eq!(r["count"], json!(0));
        let _ = std::fs::remove_dir_all(&dbdir);
    }
}
