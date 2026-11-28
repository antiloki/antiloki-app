//! brain-decisions-archive.rs — twin of skills/audit/brain-decisions-archive.mjs (db lane).
//! NAMES ARE CONTRACT: `brainDecisionsArchive` (default). FEAT-BRAIN-DECISIONS-ARCHIVE: moves the
//! driver's `rule_match` heartbeat backlog out of the hot brain.sqlite into a cold legacy segment
//! the operator owns. Archive-verified-then-delete, atomic (one transaction): the old rows are
//! copied WHOLE into `legacy_brain_decisions/<segment>.sqlite`, the archive is verified row-for-
//! row, and ONLY THEN are they DELETEd from the hot table. Every meaningful decision
//! (gate_verdict, review_pass, classify, …) and every recent `rule_match` stays hot.
//!
//! BESPOKE-NATIVE — no conform-db fixture, no frontier entry (arms/frontier left empty in the
//! manifest for this file). Every non-throw return path is gated behind a REAL on-disk `brain`
//! database: `brainPath = pragma_database_list('brain').file`, and if that is empty the skill
//! throws immediately (`'brain is in-memory — nothing to archive'`) BEFORE the
//! skipped/dry_run/archived branches ever run. The crossing harness's cross-db ATTACH lane
//! (conform-db.mjs `attachDbs`) is HARDCODED to `ATTACH ':memory:' AS <name>` for every attached
//! db in every fixture case — there is no fixture knob that makes `pragma_database_list('brain')
//! .file` non-empty, so under the harness this skill can only ever hit the guard-throw, and
//! guard-throws are never fixtured (crossing playbook gotcha #9). Identical precedent + identical
//! `brainPathFromDb` guard + identical harness limitation as the sibling `brain-compact.rs`.
//! Proven instead by `#[cfg(test)]` below using a REAL temp-dir-backed `brain.sqlite` ATTACHed by
//! path (mirrors the JS test's own `freshWorkspace` helper in brain-decisions-archive.test.mjs,
//! and the same real-file technique used by brain-compact.rs's `make_brain`).
//!
//! `datetime('now', ?)` / `datetime('now')` are frozen per the crossing playbook to the literal
//! `2026-01-01 00:00:00` (the `-N days` modifier is preserved verbatim — SQLite does the
//! arithmetic identically both sides).

use rusqlite::Connection;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const RETENTION_DAYS_DEFAULT: i64 = 7;
const FROZEN_NOW_SQL: &str = "2026-01-01 00:00:00";
const FROZEN_NOW_STAMP: &str = "20260101000000"; // FROZEN_NOW_SQL with non-digits stripped

// Faithful copy of the hot brain_decisions columns (original id preserved). The CHECK constraint
// is dropped — the archive is cold storage, not operational. Matches ARCHIVE_TABLE_DDL verbatim.
const ARCHIVE_TABLE_DDL: &str = "
CREATE TABLE IF NOT EXISTS brain_legacy.brain_decisions (
  id          INTEGER PRIMARY KEY,
  session_id  TEXT NOT NULL,
  decided_at  TEXT NOT NULL,
  card_slug   TEXT,
  decision    TEXT NOT NULL,
  rationale   TEXT NOT NULL,
  inputs_json TEXT,
  fire_id     INTEGER
)";

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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
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

// On-disk path of the attached 'brain' DB, or None for an in-memory brain (archival is
// meaningless there). Mirrors the sibling brain-decisions-vacuum.rs / brain-compact.rs helper.
fn brain_path_from_db(db: &Connection) -> Option<String> {
    let rows = crate::db_json::query_rows(db, "SELECT file FROM pragma_database_list WHERE name = 'brain'", &[]).ok()?;
    let file = rows.as_array()?.first()?.get("file")?.as_str()?.to_string();
    if file.is_empty() { None } else { Some(file) }
}

fn legacy_dir_for(db_dir: &Path) -> PathBuf {
    db_dir.join("legacy_brain_decisions")
}

// Next archive sequence number (max existing + 1, else 1). Files are the source of truth — the
// zero-padded seq makes a numeric sort chronological. Mirrors JS ARCHIVE_FILE_RE = /^brain-decisions-(\d+)-.*\.sqlite$/.
fn next_seq(legacy_dir: &Path) -> i64 {
    if !legacy_dir.exists() {
        return 1;
    }
    let re = regex::Regex::new(r"^brain-decisions-(\d+)-.*\.sqlite$").unwrap();
    let mut max_seq: i64 = 0;
    if let Ok(entries) = fs::read_dir(legacy_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(caps) = re.captures(&name) {
                if let Ok(n) = caps[1].parse::<i64>() {
                    if n > max_seq {
                        max_seq = n;
                    }
                }
            }
        }
    }
    if max_seq == 0 { 1 } else { max_seq + 1 }
}

pub fn brainDecisionsArchive(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `if (!db) throw err.contextMissing(...)` — unreachable here, `db: &Connection` is a
    // required non-nullable reference (same convention as every sibling db-lane twin).
    let older_than_days_default = json!(RETENTION_DAYS_DEFAULT);
    let older_than_days_val = input.get("older_than_days").unwrap_or(&older_than_days_default);
    let decision = input.get("decision").and_then(Value::as_str).unwrap_or("rule_match").to_string();
    let dry_run = truthy(input.get("dry_run"));

    let brain_path = match brain_path_from_db(db) {
        Some(p) => p,
        None => return Err("brain-decisions-archive: brain is in-memory — nothing to archive".to_string()),
    };
    let db_dir = Path::new(&brain_path).parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));

    // Pin the cutoff ONCE so the INSERT and the DELETE select the byte-identical row set.
    let modifier = format!("-{} days", js_number_to_string(js_number(Some(older_than_days_val))));
    let cutoff: String = db
        .query_row(
            &format!("SELECT datetime('{FROZEN_NOW_SQL}', ?1) AS c"),
            rusqlite::params![modifier],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;

    let (win_n, win_a, win_b): (i64, Option<String>, Option<String>) = db
        .query_row(
            "SELECT COUNT(*) AS n, MIN(decided_at) AS a, MAX(decided_at) AS b FROM brain.brain_decisions WHERE decision = ?1 AND decided_at < ?2",
            rusqlite::params![decision, cutoff],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| e.to_string())?;
    if win_n == 0 {
        return Ok(json!({
            "skipped": true,
            "reason": format!("no '{}' rows older than {}d", decision, tmpl(Some(older_than_days_val))),
            "archived_rows": 0,
            "cutoff": cutoff,
        }));
    }

    let legacy_dir = legacy_dir_for(&db_dir);
    let seq = next_seq(&legacy_dir);
    let file_name = format!("brain-decisions-{seq:04}-{FROZEN_NOW_STAMP}.sqlite");
    let archive_path = legacy_dir.join(&file_name);
    let archive_path_str = archive_path.to_string_lossy().into_owned();

    if dry_run {
        return Ok(json!({
            "dry_run": true,
            "would_archive": file_name,
            "archive_path": archive_path_str,
            "rows": win_n,
            "cutoff": cutoff,
            "window": format!("{} → {}", win_a.clone().unwrap_or_default(), win_b.clone().unwrap_or_default()),
            "decision": decision,
        }));
    }

    fs::create_dir_all(&legacy_dir).map_err(|e| e.to_string())?;
    if Path::new(&archive_path_str).exists() {
        return Err(format!("brain-decisions-archive: archive {file_name} already exists"));
    }

    let lit = archive_path_str.replace('\\', "/").replace('\'', "''");
    db.execute_batch(&format!("ATTACH DATABASE '{lit}' AS brain_legacy")).map_err(|e| e.to_string())?;

    // One transaction across both attached DBs: archive-insert + hot-delete commit atomically,
    // or both roll back. The COUNT verification runs on the uncommitted INSERT, so a mismatch
    // aborts BEFORE the hot rows are removed.
    let attempt: Result<(i64, i64), String> = (|| -> Result<(i64, i64), String> {
        db.execute_batch(ARCHIVE_TABLE_DDL).map_err(|e| e.to_string())?;
        db.execute_batch("BEGIN").map_err(|e| e.to_string())?;
        let inner: Result<(i64, i64), String> = (|| -> Result<(i64, i64), String> {
            let archived = db
                .execute(
                    "INSERT INTO brain_legacy.brain_decisions \
                       (id, session_id, decided_at, card_slug, decision, rationale, inputs_json, fire_id) \
                     SELECT id, session_id, decided_at, card_slug, decision, rationale, inputs_json, fire_id \
                     FROM brain.brain_decisions WHERE decision = ?1 AND decided_at < ?2",
                    rusqlite::params![decision, cutoff],
                )
                .map_err(|e| e.to_string())? as i64;
            let arc_count: i64 = db
                .query_row("SELECT COUNT(*) AS n FROM brain_legacy.brain_decisions", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            if archived != win_n || arc_count != win_n {
                return Err(format!(
                    "brain-decisions-archive: archive verification failed (inserted {archived}, in archive {arc_count}, expected {win_n}) — hot table untouched"
                ));
            }
            let deleted = db
                .execute(
                    "DELETE FROM brain.brain_decisions WHERE decision = ?1 AND decided_at < ?2",
                    rusqlite::params![decision, cutoff],
                )
                .map_err(|e| e.to_string())? as i64;
            if deleted != archived {
                return Err(format!(
                    "brain-decisions-archive: evict count {deleted} != archived count {archived} — rolling back"
                ));
            }
            Ok((archived, deleted))
        })();
        match inner {
            Ok(v) => { db.execute_batch("COMMIT").map_err(|e| e.to_string())?; Ok(v) }
            Err(e) => { let _ = db.execute_batch("ROLLBACK"); Err(e) }
        }
    })();

    let (archived, deleted) = match attempt {
        Ok(v) => v,
        Err(e) => {
            let _ = db.execute_batch("DETACH DATABASE brain_legacy");
            if Path::new(&archive_path_str).exists() {
                let _ = fs::remove_file(&archive_path_str);
            }
            return Err(e);
        }
    };
    db.execute_batch("DETACH DATABASE brain_legacy").map_err(|e| e.to_string())?;

    let remaining: i64 = db
        .query_row(
            "SELECT COUNT(*) AS n FROM brain.brain_decisions WHERE decision = ?1",
            rusqlite::params![decision],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;

    Ok(json!({
        "ok": true,
        "archived_file": file_name,
        "archive_path": archive_path_str,
        "legacy_dir": legacy_dir.to_string_lossy().into_owned(),
        "archived_rows": archived,
        "deleted_rows": deleted,
        "remaining_in_hot": remaining,
        "cutoff": cutoff,
        "window": format!("{} → {}", win_a.unwrap_or_default(), win_b.unwrap_or_default()),
        "note": "Disk is reclaimed by the VACUUM step (brain-decisions-vacuum). The archive segment is yours to delete whenever you choose.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as std_fs;

    // A standalone connection with a REAL temp brain.sqlite ATTACHed as `brain` — mirrors the JS
    // test's `freshWorkspace` helper (brain-decisions-archive.test.mjs) and brain-compact.rs's
    // `make_brain`. The archive skill only fires when `brain` is file-backed, so this is the only
    // way to exercise its real behavior (the harness's fixture lane always ATTACHes ':memory:').
    fn make_brain(tag: &str) -> (Connection, PathBuf) {
        let dir = std::env::temp_dir().join(format!("antiloki-brain-decisions-archive-rs-{tag}-{}", std::process::id()));
        let _ = std_fs::remove_dir_all(&dir);
        std_fs::create_dir_all(&dir).unwrap();
        let main_path = dir.join("main.sqlite");
        let brain_path = dir.join("brain.sqlite");
        let c = Connection::open(&main_path).unwrap();
        c.execute_batch(&format!("ATTACH '{}' AS brain", brain_path.to_string_lossy().replace('\\', "/"))).unwrap();
        c.execute_batch(
            "CREATE TABLE brain.brain_decisions (\
               id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL, decided_at TEXT NOT NULL, \
               card_slug TEXT, decision TEXT NOT NULL, rationale TEXT NOT NULL, inputs_json TEXT, fire_id INTEGER)",
        )
        .unwrap();
        (c, dir)
    }
    fn seed(c: &Connection, decision: &str, decided_at: &str, rationale: &str) {
        c.execute(
            "INSERT INTO brain.brain_decisions (session_id, decided_at, card_slug, decision, rationale) VALUES ('s', ?1, 'C', ?2, ?3)",
            rusqlite::params![decided_at, decision, rationale],
        )
        .unwrap();
    }
    fn count_hot(c: &Connection, decision: &str) -> i64 {
        c.query_row(
            "SELECT COUNT(*) AS n FROM brain.brain_decisions WHERE decision = ?1",
            rusqlite::params![decision],
            |r| r.get(0),
        )
        .unwrap()
    }
    fn legacy_files(dir: &Path) -> Vec<String> {
        let d = dir.join("legacy_brain_decisions");
        if !d.exists() { return vec![]; }
        let mut names: Vec<String> = std_fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".sqlite"))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn rejects_an_in_memory_brain() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.brain_decisions (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, decided_at TEXT, card_slug TEXT, decision TEXT, rationale TEXT, inputs_json TEXT, fire_id INTEGER)",
        )
        .unwrap();
        let r = brainDecisionsArchive(&json!({}), &c);
        assert_eq!(r, Err("brain-decisions-archive: brain is in-memory — nothing to archive".to_string()));
    }

    #[test]
    fn archives_old_rule_match_keeps_recent_and_meaningful() {
        let (c, dir) = make_brain("archive-evict");
        for i in 0..5 {
            seed(&c, "rule_match", "2025-12-01 00:00:00", &format!("old-{i}"));
        }
        for i in 0..2 {
            seed(&c, "rule_match", "2025-12-30 00:00:00", &format!("recent-{i}"));
        }
        seed(&c, "gate_verdict", "2025-12-01 00:00:00", "gv");
        seed(&c, "review_pass", "2025-12-01 00:00:00", "rp");
        seed(&c, "classify", "2025-12-01 00:00:00", "cl");

        let r = brainDecisionsArchive(&json!({ "older_than_days": 7 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["archived_rows"], json!(5), "only the 5 old rule_match rows archived");
        assert_eq!(r["deleted_rows"], json!(5));
        assert_eq!(r["remaining_in_hot"], json!(2), "the 2 recent rule_match stay hot");
        assert_eq!(r["cutoff"], json!("2025-12-25 00:00:00"));

        assert_eq!(count_hot(&c, "rule_match"), 2);
        assert_eq!(count_hot(&c, "gate_verdict"), 1, "meaningful decisions never touched");
        assert_eq!(count_hot(&c, "review_pass"), 1);
        assert_eq!(count_hot(&c, "classify"), 1);

        let files = legacy_files(&dir);
        assert_eq!(files.len(), 1, "one archive segment written");
        let archive_path = r["archive_path"].as_str().unwrap();
        let adb = Connection::open(archive_path).unwrap();
        let archived_n: i64 = adb.query_row("SELECT COUNT(*) AS n FROM brain_decisions", [], |row| row.get(0)).unwrap();
        assert_eq!(archived_n, 5, "segment holds the 5 evicted rows");
        let mut rats: Vec<String> = adb
            .prepare("SELECT rationale FROM brain_decisions ORDER BY rationale")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        rats.sort();
        assert_eq!(rats, vec!["old-0", "old-1", "old-2", "old-3", "old-4"], "rows preserved whole, not digested");
        drop(c);
        drop(adb);
        let _ = std_fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_older_than_retention_is_skipped() {
        let (c, dir) = make_brain("skip");
        for i in 0..3 {
            seed(&c, "rule_match", "2025-12-30 00:00:00", &format!("recent-{i}"));
        }
        let r = brainDecisionsArchive(&json!({ "older_than_days": 7 }), &c).unwrap();
        assert_eq!(r["skipped"], json!(true));
        assert_eq!(r["archived_rows"], json!(0));
        assert_eq!(legacy_files(&dir).len(), 0);
        assert_eq!(count_hot(&c, "rule_match"), 3, "nothing deleted");
        let _ = std_fs::remove_dir_all(&dir);
    }

    #[test]
    fn dry_run_reports_the_plan_without_writing_or_deleting() {
        let (c, dir) = make_brain("dry-run");
        for i in 0..4 {
            seed(&c, "rule_match", "2025-12-01 00:00:00", &format!("old-{i}"));
        }
        let r = brainDecisionsArchive(&json!({ "older_than_days": 7, "dry_run": true }), &c).unwrap();
        assert_eq!(r["dry_run"], json!(true));
        assert_eq!(r["rows"], json!(4));
        assert_eq!(legacy_files(&dir).len(), 0, "no file on dry run");
        assert_eq!(count_hot(&c, "rule_match"), 4, "no rows deleted");
        let _ = std_fs::remove_dir_all(&dir);
    }

    #[test]
    fn older_than_days_zero_archives_every_rule_match_row() {
        let (c, dir) = make_brain("zero-days");
        for i in 0..3 {
            seed(&c, "rule_match", "2025-12-31 23:59:59", &format!("r-{i}"));
        }
        seed(&c, "gate_verdict", "2025-12-31 23:59:59", "gv");
        let r = brainDecisionsArchive(&json!({ "older_than_days": 0 }), &c).unwrap();
        assert_eq!(r["archived_rows"], json!(3), "all rule_match archived regardless of age");
        assert_eq!(r["remaining_in_hot"], json!(0));
        assert_eq!(count_hot(&c, "gate_verdict"), 1);
        let _ = std_fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_second_run_writes_a_new_sequential_segment() {
        let (c, dir) = make_brain("sequential");
        for i in 0..3 {
            seed(&c, "rule_match", "2025-12-01 00:00:00", &format!("a-{i}"));
        }
        let r1 = brainDecisionsArchive(&json!({ "older_than_days": 7 }), &c).unwrap();
        assert_eq!(r1["archived_rows"], json!(3));
        for i in 0..2 {
            seed(&c, "rule_match", "2025-12-01 00:00:00", &format!("b-{i}"));
        }
        let r2 = brainDecisionsArchive(&json!({ "older_than_days": 7 }), &c).unwrap();
        assert_eq!(r2["archived_rows"], json!(2));
        let files = legacy_files(&dir);
        assert_eq!(files.len(), 2, "two segments");
        assert!(files[0].starts_with("brain-decisions-0001-"));
        assert!(files[1].starts_with("brain-decisions-0002-"));
        let _ = std_fs::remove_dir_all(&dir);
    }
}
