//! brain-compact.rs — twin of skills/audit/brain-compact.mjs (db lane). NAMES ARE CONTRACT:
//! `brainCompact` (default). Archive-and-evict: moves OLD rows of an append-only brain table
//! into a fresh legacy sqlite file under `legacies/`, verifies the copy row-for-row, THEN
//! deletes the archived rows from live and VACUUMs. The recent `keep_last` rows stay live.
//!
//! BESPOKE-NATIVE — no conform-db fixture, no frontier entry (arms/frontier left empty in the
//! manifest for this file). Every non-throw return path is gated behind a REAL on-disk `brain`
//! database: `brainPath = pragma_database_list('brain').file`, and if that is empty the skill
//! throws immediately (`'brain is in-memory — nothing to compact'`) BEFORE the `dry_run`/`skipped`
//! checks even run — unlike the sibling `brain-decisions-vacuum` (which degrades gracefully to
//! null fields when brain is in-memory), this skill has no non-throw path when brain has no file.
//! The crossing harness's cross-db ATTACH lane (`conform-db.mjs` `attachDbs`) is HARDCODED to
//! `ATTACH ':memory:' AS <name>` for every attached db in every fixture case — there is no fixture
//! knob that makes `pragma_database_list('brain').file` non-empty. So under the harness this skill
//! can only ever hit the guard-throw, and guard-throws are never fixtured (crossing playbook
//! gotcha #9) — there is no case where a JSON fixture could carry BOTH a real archive-worthy
//! `brain` table AND a real file backing it. Proven instead by `#[cfg(test)]` below using a REAL
//! temp-dir-backed `brain.sqlite` ATTACHed by path (mirrors the JS test's own `makeBrain` helper
//! in brain-compact.test.mjs, and the same real-file technique used by
//! brain-decisions-vacuum.rs's `file_backed_brain_reports_byte_counts`).
//!
//! `datetime('now')` (the archive-file timestamp stamp) is frozen per the crossing playbook to
//! the literal `2026-01-01 00:00:00`.
// QUALITY: 80/100 — archive-then-evict-then-vacuum, non-destructive (archive verified before delete)

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
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
// JS Number.prototype.toString() for a raw f64 (here: the interpolated `keep` in a reason string).
fn js_number_to_string(f: f64) -> String {
    if f.is_nan() { return "NaN".to_string(); }
    if f == 0.0 { return "0".to_string(); }
    let neg = f.is_sign_negative();
    let abs = f.abs();
    if abs.is_infinite() { return if neg { "-Infinity".into() } else { "Infinity".into() }; }
    let sci = format!("{:e}", abs);
    let (mantissa, exp_str) = sci.split_once('e').unwrap();
    let exp: i32 = exp_str.parse().unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let body = if (-6..=20).contains(&exp) {
        if exp >= 0 {
            if exp + 1 >= k { format!("{digits}{}", "0".repeat((exp + 1 - k) as usize)) }
            else { let (i, f) = digits.split_at((exp + 1) as usize); format!("{i}.{f}") }
        } else { format!("0.{}{digits}", "0".repeat((-exp - 1) as usize)) }
    } else {
        let m = if k == 1 { digits.clone() } else { format!("{}.{}", &digits[..1], &digits[1..]) };
        format!("{m}e{}{}", if exp >= 0 { "+" } else { "-" }, exp.abs())
    };
    if neg { format!("-{body}") } else { body }
}

// Only append-only, non-hot brain tables (mirrors JS COMPACTABLE Set verbatim).
const COMPACTABLE: &[&str] = &["brain_decisions", "brain_fires", "driver_ticks"];

fn brain_path_from_db(db: &Connection) -> Option<String> {
    let rows = crate::db_json::query_rows(db, "SELECT file FROM pragma_database_list WHERE name = 'brain'", &[]).ok()?;
    let file = rows.as_array()?.first()?.get("file")?.as_str()?.to_string();
    if file.is_empty() { None } else { Some(file) }
}

pub fn brainCompact(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { table = 'brain_decisions', keep_last = 5000, dry_run = false } = input ?? {}` —
    // destructuring defaults trigger only on an UNDEFINED (here: missing) key, never on `null`.
    let table = match input.get("table") {
        None => "brain_decisions".to_string(),
        Some(Value::String(s)) => s.clone(),
        // A non-string `table` never matches the (string) COMPACTABLE set in JS either — falls
        // through to the same "not compactable" throw below.
        Some(_) => String::new(),
    };
    let dry_run_v: Value = input.get("dry_run").cloned().unwrap_or(Value::Bool(false));

    if !COMPACTABLE.contains(&table.as_str()) {
        return Err(format!(
            "brain-compact: table '{}' is not compactable (allow: {})",
            table,
            COMPACTABLE.join(", ")
        ));
    }
    // `Math.max(0, Number(keep_last) || 0)` — `|| 0` collapses NaN/0/-0 to 0 BEFORE the max (so a
    // NaN keep_last can't poison Math.max, which would otherwise propagate NaN).
    let keep_last_raw = match input.get("keep_last") { None => 5000.0, Some(v) => js_number(Some(v)) };
    let keep_last_raw = if keep_last_raw.is_nan() || keep_last_raw == 0.0 { 0.0 } else { keep_last_raw };
    let keep = keep_last_raw.max(0.0);
    let keep_i64 = keep as i64;

    let brain_path = match brain_path_from_db(db) {
        Some(p) => p,
        None => return Err("brain-compact: brain is in-memory — nothing to compact".to_string()),
    };

    let total: i64 = db
        .query_row(&format!("SELECT COUNT(*) AS n FROM brain.{table}"), [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if (total as f64) <= keep {
        return Ok(json!({
            "skipped": true,
            "reason": format!("{table} has {total} row(s) (<= keep_last {})", js_number_to_string(keep)),
        }));
    }

    // cutoff = id of the keep_last-th newest row; archive everything strictly older.
    let cutoff: Option<i64> = db
        .prepare(&format!("SELECT id FROM brain.{table} ORDER BY id DESC LIMIT 1 OFFSET ?1"))
        .map_err(|e| e.to_string())?
        .query_row(rusqlite::params![keep_i64 - 1], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    let cutoff = match cutoff {
        Some(c) => c,
        None => return Ok(json!({ "skipped": true, "reason": "could not resolve a cutoff id" })),
    };
    let archive_count: i64 = db
        .query_row(&format!("SELECT COUNT(*) AS n FROM brain.{table} WHERE id < ?1"), rusqlite::params![cutoff], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if archive_count == 0 {
        return Ok(json!({ "skipped": true, "reason": "nothing older than the keep_last cutoff" }));
    }

    let ts = "2026-01-01 00:00:00".to_string(); // frozen `datetime('now')`
    let stamp: String = ts.chars().filter(|c| c.is_ascii_digit()).collect();
    let legacy_dir = Path::new(&brain_path).parent().map(|p| p.join("legacies")).unwrap_or_else(|| Path::new("legacies").to_path_buf());
    let file_name = format!("{}-{}.sqlite", table.replace('_', "-"), stamp);
    let legacy_path = legacy_dir.join(&file_name);
    let legacy_path_display = legacy_path.to_string_lossy().into_owned(); // native separators, like Node path.join()

    if truthy(Some(&dry_run_v)) {
        return Ok(json!({
            "dry_run": true,
            "would_archive": file_name,
            "rows": archive_count,
            "kept": total - archive_count,
            "cutoff": cutoff,
        }));
    }

    fs::create_dir_all(&legacy_dir).map_err(|e| e.to_string())?;
    if Path::new(&legacy_path_display).exists() {
        return Err(format!("brain-compact: archive {file_name} already exists"));
    }

    // 1. Archive the old rows into a fresh legacy DB + verify BEFORE evicting.
    //    cutoff is a DB-derived integer and `table` is allow-listed → safe to inline.
    let lit = legacy_path_display.replace('\\', "/").replace('\'', "''");
    db.execute_batch(&format!("ATTACH '{lit}' AS arch")).map_err(|e| e.to_string())?;
    let archived_result: Result<i64, String> = (|| -> Result<i64, String> {
        db.execute_batch(&format!("CREATE TABLE arch.{table} AS SELECT * FROM brain.{table} WHERE id < {cutoff}"))
            .map_err(|e| e.to_string())?;
        let n: i64 = db
            .query_row(&format!("SELECT COUNT(*) AS n FROM arch.{table}"), [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if n != archive_count {
            // JS: `new AntilokiError({..., cause: 'archive count mismatch ${n}/${archiveCount}'})`
            // whose .message is "brain-compact: archive count mismatch n/archiveCount" — that
            // message becomes the OUTER catch's `e.message` below (nested prefix, intentional).
            return Err(format!("brain-compact: archive count mismatch {n}/{archive_count}"));
        }
        Ok(n)
    })();
    let archived_rows = match archived_result {
        Ok(n) => n,
        Err(inner_message) => {
            let _ = db.execute_batch("DETACH arch"); // best effort, like the JS try/catch swallow
            return Err(format!("brain-compact: archive failed: {inner_message} — live {table} untouched"));
        }
    };
    db.execute_batch("DETACH arch").map_err(|e| e.to_string())?;

    // 2. Evict the now-archived rows from live (a transaction), then reclaim space.
    db.execute_batch("BEGIN").map_err(|e| e.to_string())?;
    if let Err(e) = db.execute(&format!("DELETE FROM brain.{table} WHERE id < ?1"), rusqlite::params![cutoff]) {
        let _ = db.execute_batch("ROLLBACK");
        return Err(e.to_string()); // the archive survives; the live table is unchanged → safe to retry
    }
    db.execute_batch("COMMIT").map_err(|e| e.to_string())?;
    let vacuumed = db.execute_batch("VACUUM brain").is_ok();

    Ok(json!({
        "ok": true,
        "table": table,
        "archived_file": file_name,
        "archived_path": legacy_path_display,
        "archived_rows": archived_rows,
        "kept_rows": total - archived_rows,
        "cutoff": cutoff,
        "vacuumed": vacuumed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as std_fs;

    // A standalone connection with a REAL temp brain.sqlite ATTACHed as `brain` — mirrors the JS
    // test's `makeBrain` helper (brain-compact.test.mjs) and never touches an in-memory db, since
    // the whole point of this skill only fires when `brain` is file-backed.
    fn make_brain(tag: &str, n: i64) -> (Connection, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("antiloki-brain-compact-rs-{tag}-{}", std::process::id()));
        let _ = std_fs::remove_dir_all(&dir);
        std_fs::create_dir_all(&dir).unwrap();
        let main_path = dir.join("main.sqlite");
        let brain_path = dir.join("brain.sqlite");
        let c = Connection::open(&main_path).unwrap();
        c.execute_batch(&format!("ATTACH '{}' AS brain", brain_path.to_string_lossy().replace('\\', "/"))).unwrap();
        c.execute_batch("CREATE TABLE brain.brain_decisions (id INTEGER PRIMARY KEY AUTOINCREMENT, decision TEXT, ts TEXT)").unwrap();
        {
            let mut stmt = c.prepare("INSERT INTO brain.brain_decisions (decision, ts) VALUES (?1, ?2)").unwrap();
            for i in 0..n {
                stmt.execute(rusqlite::params!["rule_match", format!("2026-01-01 00:00:{:02}", i)]).unwrap();
            }
        }
        (c, dir)
    }
    fn count(c: &Connection) -> i64 {
        c.query_row("SELECT COUNT(*) AS n FROM brain.brain_decisions", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn archives_old_rows_and_evicts_keeping_recent_tail() {
        let (c, dir) = make_brain("archive-evict", 20);
        let r = brainCompact(&json!({ "table": "brain_decisions", "keep_last": 5 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["kept_rows"], json!(5));
        assert_eq!(r["archived_rows"], json!(15));
        assert_eq!(count(&c), 5);
        let archived_path = r["archived_path"].as_str().unwrap();
        assert!(Path::new(archived_path).exists(), "legacy archive file exists");
        let adb = Connection::open(archived_path).unwrap();
        let archived_n: i64 = adb.query_row("SELECT COUNT(*) AS n FROM brain_decisions", [], |row| row.get(0)).unwrap();
        assert_eq!(archived_n, 15);
        drop(c);
        drop(adb);
        let _ = std_fs::remove_dir_all(&dir);
    }

    #[test]
    fn dry_run_reports_without_touching_disk() {
        let (c, dir) = make_brain("dry-run", 20);
        let r = brainCompact(&json!({ "table": "brain_decisions", "keep_last": 5, "dry_run": true }), &c).unwrap();
        assert_eq!(r["dry_run"], json!(true));
        assert_eq!(r["rows"], json!(15));
        assert_eq!(r["kept"], json!(5));
        assert_eq!(count(&c), 20, "dry_run must not evict");
        let would_archive = r["would_archive"].as_str().unwrap();
        assert!(!dir.join("legacies").join(would_archive).exists(), "dry_run must not write the archive file");
        let _ = std_fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_when_row_count_is_at_or_under_keep_last() {
        let (c, dir) = make_brain("skip", 3);
        let r = brainCompact(&json!({ "table": "brain_decisions", "keep_last": 5 }), &c).unwrap();
        assert_eq!(r, json!({ "skipped": true, "reason": "brain_decisions has 3 row(s) (<= keep_last 5)" }));
        let _ = std_fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_a_table_outside_the_compactable_allowlist() {
        let (c, dir) = make_brain("disallow", 3);
        let r = brainCompact(&json!({ "table": "brain_locks" }), &c);
        assert_eq!(
            r,
            Err("brain-compact: table 'brain_locks' is not compactable (allow: brain_decisions, brain_fires, driver_ticks)".to_string())
        );
        let _ = std_fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_an_in_memory_brain() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch("CREATE TABLE brain.brain_decisions (id INTEGER PRIMARY KEY AUTOINCREMENT, decision TEXT, ts TEXT)").unwrap();
        let r = brainCompact(&json!({}), &c);
        assert_eq!(r, Err("brain-compact: brain is in-memory — nothing to compact".to_string()));
    }
}
