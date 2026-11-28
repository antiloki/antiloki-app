//! brain-decisions-vacuum.rs — twin of skills/audit/brain-decisions-vacuum.mjs (db lane).
//! NAMES ARE CONTRACT: `brainDecisionsVacuum` (default). VACUUM step of FEAT-BRAIN-DECISIONS-ARCHIVE:
//! reclaims the disk pages an archive-evict DELETE only freed logically — the file shrinks only on
//! VACUUM (auto_vacuum=0 on brain.sqlite). OPERATOR-RUN, BE STOPPED: VACUUM takes an EXCLUSIVE lock
//! for its full duration; never wired to a driver rule, reached only via system.brain.compact from
//! the offline runner.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::fs;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// Mirrors the shape of the uncaught V8 exception a bare `statSync(path)` throws in the
// JS twin (verified via `node -e "require('fs').statSync('/definitely/does/not/exist')"`
// => "Error: ENOENT: no such file or directory, stat '<path>'"). The JS skill deliberately
// has no try/catch around statSync, so a missing/unreadable brain file must fail this skill
// too, not silently report null byte counts.
fn stat_err(e: &std::io::Error, path: &str) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => format!("ENOENT: no such file or directory, stat '{}'", path),
        std::io::ErrorKind::PermissionDenied => format!("EACCES: permission denied, stat '{}'", path),
        _ => format!("{}, stat '{}'", e, path),
    }
}

fn brain_path_from_db(db: &Connection) -> Option<String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT file FROM pragma_database_list WHERE name = 'brain'",
        &[],
    )
    .ok()?;
    let file = rows.as_array()?.first()?.get("file")?.as_str()?.to_string();
    if file.is_empty() { None } else { Some(file) }
}

pub fn brainDecisionsVacuum(input: &Value, db: &Connection) -> Result<Value, String> {
    // dry_run threads through the system.brain.compact action: the preview must not rewrite the
    // (potentially GB-scale) file. Honor it here so the whole sequence is a no-op.
    if truthy(input.get("dry_run")) {
        return Ok(json!({
            "skipped": true,
            "reason": "dry_run — VACUUM not run",
            "vacuumed": Value::Null,
        }));
    }

    let brain_path = brain_path_from_db(db);
    let before = match brain_path.as_deref() {
        Some(p) => Some(fs::metadata(p).map_err(|e| stat_err(&e, p))?.len()),
        None => None,
    };

    // VACUUM the attached 'brain' schema specifically (the state/main DB is small and untouched).
    // Must run outside any transaction — action-run executes steps without an enclosing
    // transaction, so this is safe.
    db.execute_batch("VACUUM brain").map_err(|e| e.to_string())?;

    let after = match brain_path.as_deref() {
        Some(p) => Some(fs::metadata(p).map_err(|e| stat_err(&e, p))?.len()),
        None => None,
    };

    let reclaimed = match (before, after) {
        (Some(b), Some(a)) => json!(b as i64 - a as i64),
        _ => Value::Null,
    };

    Ok(json!({
        "ok": true,
        "vacuumed": "brain",
        "brain_path": brain_path,
        "bytes_before": before.map(|v| v as i64),
        "bytes_after": after.map(|v| v as i64),
        "reclaimed_bytes": reclaimed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_skips_vacuum() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        let r = brainDecisionsVacuum(&json!({ "dry_run": true }), &c).unwrap();
        assert_eq!(
            r,
            json!({ "skipped": true, "reason": "dry_run — VACUUM not run", "vacuumed": Value::Null })
        );
    }

    #[test]
    fn memory_attached_brain_has_no_file_path() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        let r = brainDecisionsVacuum(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["vacuumed"], json!("brain"));
        assert_eq!(r["brain_path"], Value::Null);
        assert_eq!(r["bytes_before"], Value::Null);
        assert_eq!(r["bytes_after"], Value::Null);
        assert_eq!(r["reclaimed_bytes"], Value::Null);
    }

    #[test]
    fn file_backed_brain_reports_byte_counts() {
        let dir = std::env::temp_dir().join(format!("vacuum-rs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let brain_path = dir.join("brain.sqlite");
        let brain_path_str = brain_path.to_str().unwrap().to_string();

        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(&format!("ATTACH '{}' AS brain", brain_path_str)).unwrap();
        c.execute_batch("CREATE TABLE brain.junk (id INTEGER PRIMARY KEY, blob TEXT)").unwrap();
        {
            let big = "x".repeat(200_000);
            let mut stmt = c.prepare("INSERT INTO brain.junk (blob) VALUES (?1)").unwrap();
            for _ in 0..50 {
                stmt.execute(rusqlite::params![big]).unwrap();
            }
        }
        c.execute_batch("DELETE FROM brain.junk").unwrap();

        let r = brainDecisionsVacuum(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["vacuumed"], json!("brain"));
        assert_eq!(r["brain_path"], json!(brain_path_str));
        let before = r["bytes_before"].as_i64().unwrap();
        let after = r["bytes_after"].as_i64().unwrap();
        assert_eq!(r["reclaimed_bytes"], json!(before - after));
        assert!(before - after > 0, "VACUUM should shrink the bloated file");

        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
