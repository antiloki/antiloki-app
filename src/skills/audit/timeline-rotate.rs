//! timeline-rotate.rs — twin of skills/audit/timeline-rotate.mjs. NAMES ARE CONTRACT:
//! `timelineRotate` (default). FEAT-TIMELINE-ROTATION: archives the live `sacred_timeline` into
//! `legacy_timeline/` (VACUUM INTO a clean snapshot, verified row-for-row, then compressed) and
//! reopens a fresh live segment sealed with a BOUNDARY row chained to the archived segment's last
//! `digital_print` — the Merkle chain stays cryptographically continuous across the file boundary.
//!
//! BESPOKE-NATIVE — real `VACUUM INTO`, `DROP TABLE` + schema recreate, and a verified-archive
//! transaction against a file-backed `timeline` attachment. conform-db's fixture model (db seed →
//! db_after diff, `ATTACH ':memory:' AS timeline` only) cannot exercise VACUUM INTO (needs a real
//! on-disk target) or the archive-verify read against a second real `DatabaseSync`/`Connection` —
//! same precedent as the sibling `brain-decisions-archive.rs`. Proven instead by `#[cfg(test)]`
//! against a real temp-dir-backed `timeline.sqlite`, mirroring that sibling's `make_brain` helper.
//!
//! `context.actor` — per the db-lane harness precedent (brain-decisions-archive.rs et al.), the
//! harness context carries only `db`, so `actor` is always its JS default `'system'`, modeled
//! below as the literal constant `ACTOR_DEFAULT`.
//!
//! Reuses the already-converted `_helpers/timeline-segments.rs` twin for
//! computeDigitalPrint/timelinePathFromDb/legacyDirFor/nextSegmentSeq/chainLayout/
//! canonicalRowsDigest/gzipSegmentFile/deviceId/SEGMENT_BOUNDARY_OP/ROWS_DIGEST_ALGO, the
//! `TIMELINE_SCHEMA` data twin (engine/open-db/schemas.rs) to recreate the live table, and
//! `timeline-digest-md.rs`'s `timelineDigestMd` for step 6 (best-effort — its failure never voids
//! the rotation, same as the JS `catch {}`).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::fs;
use std::path::Path;

use crate::audit__helpers_timeline_segments::{
    canonicalRowsDigest, chainLayout, computeDigitalPrint, deviceId, gzipSegmentFile,
    legacyDirFor, nextSegmentSeq, timelinePathFromDb, ROWS_DIGEST_ALGO, SEGMENT_BOUNDARY_OP,
};
use crate::audit_timeline_digest_md::timelineDigestMd;

const ACTOR_DEFAULT: &str = "system";

// JS truthiness (JSON has no undefined, so this only needs Null/Bool/Number/String/other cases —
// same shape as every sibling twin's `truthy`).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// `Number(x)` coercion for the `count < min_rows` comparison — a present `null` coerces to 0
// (destructuring defaults only fire on an ABSENT/undefined key, never on an explicit null).
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

pub fn timelineRotate(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `if (!db) throw err.contextMissing(...)` — unreachable here, `db: &Connection` is a
    // required non-nullable reference (same convention as every sibling db-lane twin).
    let dry_run = truthy(input.get("dry_run"));
    let min_rows = match input.get("min_rows") { None => 1.0, Some(v) => js_number(Some(v)) };
    let compress = match input.get("compress") { None => true, Some(v) => truthy(Some(v)) };
    let copy_dir = input.get("copy_dir").and_then(Value::as_str).map(str::to_string);

    // LAYOUT GUARD — rotate operates ONLY on the canonical layout (chain in the attached
    // 'timeline' DB). See the JS module comment for the pre-relocation split-brain incident this
    // guards against.
    let layout = chainLayout(db);
    let layout_str = layout.as_str().unwrap_or("none");
    if layout_str != "timeline" {
        let hint = match layout_str {
            "legacy-main" => "chain still lives in state.sqlite (pre-relocation layout) — apply migration 20260613-138-relocate-sacred-timeline with the BE stopped, then re-run",
            "both" => "sacred_timeline exists in BOTH main and timeline — layout drift; resolve before rotating (consistency-scan D18)",
            _ => "no sacred_timeline table found on this connection — nothing to rotate",
        };
        return Err(format!("timeline-rotate: refusing to rotate: {hint}"));
    }

    let tl_path = match timelinePathFromDb(db).as_str().map(str::to_string) {
        Some(p) => p,
        None => return Err("timeline-rotate: timeline is in-memory — nothing to rotate".to_string()),
    };
    let db_dir = Path::new(&tl_path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();

    let count: i64 = db
        .query_row("SELECT COUNT(*) n FROM timeline.sacred_timeline", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if (count as f64) < min_rows {
        return Ok(json!({ "skipped": true, "reason": format!("live timeline has {count} row(s) (< min_rows {})", js_number_display(min_rows)) }));
    }
    // A timeline holding only its opening boundary row has nothing real to archive.
    if count == 1 {
        let only_op: Option<String> = db
            .query_row("SELECT operation FROM timeline.sacred_timeline LIMIT 1", [], |r| r.get(0))
            .ok();
        if only_op.as_deref() == Some(SEGMENT_BOUNDARY_OP) {
            return Ok(json!({ "skipped": true, "reason": "live timeline holds only a boundary row" }));
        }
    }

    let last_print: String = db
        .query_row("SELECT digital_print FROM timeline.sacred_timeline ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let ts: String = db.query_row("SELECT datetime('now') AS ts", [], |r| r.get(0)).map_err(|e| e.to_string())?;

    let seq = nextSegmentSeq(&json!(db_dir)).as_i64().unwrap_or(1);
    let stamp: String = ts.chars().filter(|c| c.is_ascii_digit()).collect(); // 2026-06-13 20:30:00 → 20260613203000
    let file_name = format!("timeline-{:04}-{}.sqlite", seq, stamp);
    let legacy_dir = legacyDirFor(&json!(db_dir)).as_str().unwrap_or_default().to_string();
    let legacy_path = Path::new(&legacy_dir).join(&file_name).to_string_lossy().into_owned();

    if dry_run {
        return Ok(json!({ "dry_run": true, "would_archive": file_name, "rows": count, "last_print": last_print, "seq": seq }));
    }

    fs::create_dir_all(&legacy_dir).map_err(|e| e.to_string())?;
    if Path::new(&legacy_path).exists() {
        return Err(format!("timeline-rotate: archive {file_name} already exists"));
    }

    // 1. Clean, defragmented snapshot of the live timeline → archive (VACUUM INTO keeps the full
    //    schema incl. immutability triggers). Outside any transaction — live data untouched.
    let lit = legacy_path.replace('\\', "/").replace('\'', "''");
    if let Err(e) = db.execute_batch(&format!("VACUUM timeline INTO '{lit}'")) {
        return Err(format!("timeline-rotate: archive snapshot failed: {e} — live timeline untouched"));
    }

    // 2. Verify the archive row-for-row BEFORE touching the live data, computing its canonical
    //    rows digest (sealed into the boundary payload below).
    let (archive_count, archive_last, rows_digest) = {
        let adb = Connection::open_with_flags(&legacy_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?;
        let n: i64 = adb.query_row("SELECT COUNT(*) n FROM sacred_timeline", [], |r| r.get(0)).map_err(|e| e.to_string())?;
        let last: Option<String> = adb
            .query_row("SELECT digital_print FROM sacred_timeline ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
            .ok();
        let digest = canonicalRowsDigest(&adb, &Value::Null, &Value::Null);
        (n, last, digest)
    };
    if archive_count != count || archive_last.as_deref() != Some(last_print.as_str()) {
        let _ = fs::remove_file(&legacy_path);
        let last_match = archive_last.as_deref() == Some(last_print.as_str());
        return Err(format!(
            "timeline-rotate: archive verification failed (rows {archive_count}/{count}, last {}) — live timeline untouched",
            if last_match { "ok" } else { "mismatch" }
        ));
    }

    // 2b. Compress the verified archive (gzip). BEFORE the seal so the boundary payload records
    //     what is actually on disk. compress=false keeps plaintext.
    let (final_path, final_file) = if compress {
        let gz = gzipSegmentFile(&legacy_path).map_err(|e| e.to_string())?;
        (gz, format!("{file_name}.gz"))
    } else {
        (legacy_path.clone(), file_name.clone())
    };

    // 2c. Optional second copy — cheap insurance against a sealed-but-deleted archive.
    let copied: Option<String> = match &copy_dir {
        Some(dir) => {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            let dest = Path::new(dir).join(&final_file).to_string_lossy().into_owned();
            fs::copy(&final_path, &dest).map_err(|e| e.to_string())?;
            Some(dest)
        }
        None => None,
    };

    // 3. Boundary row that opens the fresh segment, chained to the archive.
    let mut payload_map = Map::new();
    payload_map.insert("archived_file".into(), json!(file_name));
    payload_map.insert("archived_rows".into(), json!(count));
    payload_map.insert("archived_last_print".into(), json!(last_print));
    payload_map.insert("seq".into(), json!(seq));
    payload_map.insert("segment_digest_rows".into(), rows_digest.clone());
    payload_map.insert("digest_algo".into(), json!(ROWS_DIGEST_ALGO));
    payload_map.insert("compression".into(), json!(if compress { "gzip" } else { "none" }));
    payload_map.insert("device_id".into(), deviceId());
    let payload = serde_json::to_string(&Value::Object(payload_map)).map_err(|e| e.to_string())?;

    let event = json!({
        "ts": ts,
        "actor": ACTOR_DEFAULT,
        "operation": SEGMENT_BOUNDARY_OP,
        "entity_type": "timeline_segment",
        "entity_id": file_name,
        "before_json": Value::Null,
        "after_json": Value::Null,
        "payload": payload,
    });
    let boundary_print = computeDigitalPrint(&json!(last_print), &event);

    // 4. Seal the live segment: drop + re-seed the table, insert the boundary. Transactional — a
    //    failure here rolls back and removes the archive, leaving the live timeline untouched.
    let seal: Result<(), String> = (|| {
        db.execute_batch("BEGIN").map_err(|e| e.to_string())?;
        let attempt: Result<(), String> = (|| {
            db.execute_batch("DROP TABLE timeline.sacred_timeline").map_err(|e| e.to_string())?;
            let schema_sql = crate::engine_open_db_schemas::TIMELINE_SCHEMA.as_str().unwrap_or_default();
            db.execute_batch(schema_sql).map_err(|e| e.to_string())?; // recreates table + indexes + immutability triggers
            db.execute(
                "INSERT INTO timeline.sacred_timeline \
                   (ts, actor, operation, entity_type, entity_id, before_json, after_json, payload, prev_print, digital_print, user_goal_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, ?6, ?7, ?8, NULL)",
                rusqlite::params![ts, ACTOR_DEFAULT, SEGMENT_BOUNDARY_OP, "timeline_segment", file_name, payload, last_print, boundary_print.as_str().unwrap_or_default()],
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        })();
        match attempt {
            Ok(()) => { db.execute_batch("COMMIT").map_err(|e| e.to_string()) }
            Err(e) => { let _ = db.execute_batch("ROLLBACK"); Err(e) }
        }
    })();
    if let Err(e) = seal {
        let _ = fs::remove_file(&final_path);
        if let Some(c) = &copied { let _ = fs::remove_file(c); }
        return Err(e);
    }

    // 5. Reclaim the emptied live file (it kept the archived data's freed pages).
    let reclaimed = db.execute_batch("VACUUM timeline").is_ok();

    // 6. Layer-3 digest — deterministic template render next to the archive (regenerable, never
    //    the record). Failure here never voids the rotation.
    let digest_path: Value = timelineDigestMd(&json!({ "path": final_path, "write": true }), db)
        .ok()
        .and_then(|v| v.get("digest_path").cloned())
        .unwrap_or(Value::Null);

    Ok(json!({
        "ok": true,
        "archived_file": final_file,
        "archived_path": final_path,
        "archived_rows": count,
        "archived_last_print": last_print,
        "segment_digest_rows": rows_digest,
        "digest_algo": ROWS_DIGEST_ALGO,
        "compressed": compress,
        "copy_path": copied,
        "digest_md": digest_path,
        "boundary_print": boundary_print,
        "seq": seq,
        "live_rows_now": 1,
        "live_file_vacuumed": reclaimed,
    }))
}

// JS `Number` displayed via template-literal interpolation in the skipped-reason string — an
// integral f64 prints without a trailing ".0" (matches `${min_rows}` for the common integer case).
fn js_number_display(n: f64) -> String {
    if n.fract() == 0.0 && n.is_finite() { format!("{}", n as i64) } else { format!("{n}") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_timeline(tag: &str) -> (Connection, PathBuf) {
        let dir = std::env::temp_dir().join(format!("antiloki-timeline-rotate-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let main_path = dir.join("main.sqlite");
        let tl_path = dir.join("timeline.sqlite");
        let c = Connection::open(&main_path).unwrap();
        c.execute_batch(&format!("ATTACH '{}' AS timeline", tl_path.to_string_lossy().replace('\\', "/"))).unwrap();
        let schema_sql = crate::engine_open_db_schemas::TIMELINE_SCHEMA.as_str().unwrap();
        c.execute_batch(schema_sql).unwrap();
        (c, dir)
    }

    fn insert_row(c: &Connection, entity_id: &str, prev: &str) -> String {
        let event = json!({ "ts": "2026-01-01 00:00:00", "actor": "tester", "operation": "card.add", "entity_type": "card", "entity_id": entity_id, "before_json": null, "after_json": null, "payload": null });
        let print = computeDigitalPrint(&json!(prev), &event).as_str().unwrap().to_string();
        c.execute(
            "INSERT INTO timeline.sacred_timeline (ts, actor, operation, entity_type, entity_id, before_json, after_json, payload, prev_print, digital_print, user_goal_id) \
             VALUES ('2026-01-01 00:00:00', 'tester', 'card.add', 'card', ?1, NULL, NULL, NULL, ?2, ?3, NULL)",
            rusqlite::params![entity_id, prev, print],
        )
        .unwrap();
        print
    }

    // Matches the JS test's `legacyFiles` helper: only archive files, not the
    // sibling `.digest.md` convenience cache that step 6 may also drop in the
    // same directory (compressed archives skip the digest — .gz decompression
    // is a documented KNOWN GAP in timeline-digest-md.rs — but plain .sqlite
    // archives get one, so an unfiltered listing overcounts).
    fn legacy_files(dir: &Path) -> Vec<String> {
        let d = dir.join("legacy_timeline");
        if !d.exists() { return vec![]; }
        let mut names: Vec<String> = fs::read_dir(&d).unwrap().flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".sqlite") || n.ends_with(".sqlite.gz"))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn rejects_a_non_timeline_layout() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY)").unwrap();
        let r = timelineRotate(&json!({}), &c);
        assert!(r.unwrap_err().contains("chain still lives in state.sqlite"));
    }

    #[test]
    fn rejects_an_in_memory_timeline() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS timeline").unwrap();
        c.execute_batch(&crate::engine_open_db_schemas::TIMELINE_SCHEMA.as_str().unwrap().to_string()).unwrap();
        let r = timelineRotate(&json!({}), &c);
        assert_eq!(r, Err("timeline-rotate: timeline is in-memory — nothing to rotate".to_string()));
    }

    #[test]
    fn skips_below_min_rows_and_boundary_only_segment() {
        let (c, dir) = make_timeline("skip");
        insert_row(&c, "c1", "GENESIS");
        let r = timelineRotate(&json!({ "min_rows": 5 }), &c).unwrap();
        assert_eq!(r["skipped"], json!(true));
        assert!(r["reason"].as_str().unwrap().contains("< min_rows 5"));

        // Drive to a boundary-only live segment via a REAL rotation —
        // sacred_timeline rows are immutable (no DELETE, law
        // SACRED_TIMELINE_ROWS_MUST_HAVE_DIGITAL_PRINT), so a hand-rolled
        // DELETE + fake boundary row (as this test used to do) is not a
        // reachable state; rotating the single real row is.
        let r1 = timelineRotate(&json!({}), &c).unwrap();
        assert_eq!(r1["ok"], json!(true));

        let r2 = timelineRotate(&json!({}), &c).unwrap();
        assert_eq!(r2["skipped"], json!(true));
        assert_eq!(r2["reason"], json!("live timeline holds only a boundary row"));
        drop(c);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dry_run_reports_the_plan_without_writing() {
        let (c, dir) = make_timeline("dry-run");
        insert_row(&c, "c1", "GENESIS");
        insert_row(&c, "c2", "H1");
        let r = timelineRotate(&json!({ "dry_run": true }), &c).unwrap();
        assert_eq!(r["dry_run"], json!(true));
        assert_eq!(r["rows"], json!(2));
        assert_eq!(legacy_files(&dir).len(), 0, "no file written on dry run");
        let count: i64 = c.query_row("SELECT COUNT(*) n FROM timeline.sacred_timeline", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 2, "live table untouched");
        drop(c);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotates_archives_verified_and_seals_a_chained_boundary() {
        let (c, dir) = make_timeline("rotate");
        insert_row(&c, "c1", "GENESIS");
        insert_row(&c, "c2", "H1");
        let last_print = insert_row(&c, "c3", "H2");

        let r = timelineRotate(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["archived_rows"], json!(3));
        assert_eq!(r["archived_last_print"], json!(last_print));
        assert_eq!(r["seq"], json!(1));
        assert_eq!(r["compressed"], json!(true));
        assert_eq!(r["live_rows_now"], json!(1));
        assert!(r["archived_file"].as_str().unwrap().ends_with(".sqlite.gz"));

        // Archive holds the 3 evicted rows, verified independently.
        let files = legacy_files(&dir);
        assert_eq!(files.len(), 1);
        let (segment_path, temp_dir) = crate::audit__helpers_timeline_segments::gunzipToTemp(r["archived_path"].as_str().unwrap()).unwrap();
        let adb = Connection::open_with_flags(&segment_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let archived_n: i64 = adb.query_row("SELECT COUNT(*) n FROM sacred_timeline", [], |r| r.get(0)).unwrap();
        assert_eq!(archived_n, 3);
        drop(adb);
        let _ = fs::remove_dir_all(&temp_dir);

        // Live table now holds exactly the boundary row, chained to the archive's last print.
        let (live_op, live_prev, live_entity): (String, String, String) = c
            .query_row("SELECT operation, prev_print, entity_id FROM timeline.sacred_timeline LIMIT 1", [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap();
        assert_eq!(live_op, SEGMENT_BOUNDARY_OP);
        assert_eq!(live_prev, last_print);
        // entity_id is always the UNCOMPRESSED file name; archived_file appends ".gz" on top of it.
        assert!(live_entity.ends_with(".sqlite"));
        assert_eq!(r["archived_file"].as_str().unwrap(), format!("{live_entity}.gz"));

        // A second rotation opens segment 2 (only the boundary row lives, so seed one real row first).
        insert_row(&c, "c4", "ignored-prev-not-checked");
        let r2 = timelineRotate(&json!({ "compress": false }), &c).unwrap();
        assert_eq!(r2["seq"], json!(2));
        assert_eq!(r2["compressed"], json!(false));
        assert!(r2["archived_file"].as_str().unwrap().ends_with(".sqlite"));
        assert_eq!(legacy_files(&dir).len(), 2);

        drop(c);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_dir_writes_a_second_insurance_copy() {
        let (c, dir) = make_timeline("copy-dir");
        insert_row(&c, "c1", "GENESIS");
        insert_row(&c, "c2", "H1");
        let copy_dir = dir.join("insurance");
        let r = timelineRotate(&json!({ "copy_dir": copy_dir.to_string_lossy() }), &c).unwrap();
        let copy_path = r["copy_path"].as_str().unwrap();
        assert!(Path::new(copy_path).exists());
        assert!(Path::new(r["archived_path"].as_str().unwrap()).exists());
        drop(c);
        let _ = fs::remove_dir_all(&dir);
    }
}
