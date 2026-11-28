//! THE UPGRADE GUARD (operator 2026-09-21, continuous delivery: "the client will have one or two years
//! of updates"): an update replaces the executable and leaves `state.sqlite` where it is — so the FIRST
//! time a newer build opens a database, before anything else touches it:
//!   1. the database is stamped (`PRAGMA user_version` = the build's version code) so we know which
//!      build last wrote it;
//!   2. a copy is kept next to it (`state.sqlite.bak-v<old>`, the WAL folded in first) — the one-file
//!      insurance an update can never take away;
//!   3. the migrations between the two versions run, in order (none yet: every table is
//!      `CREATE TABLE IF NOT EXISTS` at open; the list is where an `ALTER TABLE … ADD COLUMN` goes
//!      the day one is needed).
//! A downgrade (a stamp newer than this build) is warned about and left alone. Pure over the paths —
//! tested on a temp db.
use rusqlite::Connection;
use std::path::Path;

/// `0.2.10` → 2010 (major·1 000 000 + minor·1 000 + patch). Pure — tested.
pub fn version_code(v: &str) -> i64 {
    let mut it = v.trim().trim_start_matches('v').split(['.', '-', '+']).map(|p| p.parse::<i64>().unwrap_or(0));
    let (a, b, c) = (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0));
    a * 1_000_000 + b * 1_000 + c
}
pub fn version_label(code: i64) -> String { format!("{}.{}.{}", code / 1_000_000, (code / 1_000) % 1_000, code % 1_000) }

/// The migrations, oldest first: `(the version that introduced it, its SQL)`. A migration runs when the
/// stored stamp is older than its version and this build is at or past it. Keep every statement
/// idempotent (`IF NOT EXISTS`, a guarded `ADD COLUMN`) — a copy of the db is taken before they run.
pub const MIGRATIONS: &[(&str, &str)] = &[];

#[derive(Debug, PartialEq)]
pub struct Outcome { pub from: i64, pub to: i64, pub backup: Option<String>, pub migrated: usize }

/// Stamp, back up, migrate. `None` when the database does not exist yet (the open will create it — nothing
/// to protect). Errors name the step; the caller decides whether to boot anyway.
pub fn guard(db_path: &str, app_version: &str) -> Result<Option<Outcome>, String> {
    let path = Path::new(db_path);
    if !path.is_file() { return Ok(None); }
    let to = version_code(app_version);
    let conn = Connection::open(path).map_err(|e| format!("open: {e}"))?;
    let from: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).map_err(|e| format!("user_version: {e}"))?;
    if from == to { return Ok(Some(Outcome { from, to, backup: None, migrated: 0 })); }
    if from > to {
        eprintln!("antiloki-serve: this database was last written by v{} — this build is v{} (older). Left as is.", version_label(from), version_label(to));
        return Ok(Some(Outcome { from, to, backup: None, migrated: 0 }));
    }
    // the copy: fold the WAL into the main file first so the copy is whole on its own
    let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
    let backup = format!("{}.bak-v{}", db_path, if from == 0 { "unstamped".to_string() } else { version_label(from) });
    std::fs::copy(path, &backup).map_err(|e| format!("backup copy: {e}"))?;
    let mut migrated = 0usize;
    for (ver, sql) in MIGRATIONS {
        let v = version_code(ver);
        if v > from && v <= to {
            conn.execute_batch(sql).map_err(|e| format!("migration {ver}: {e}"))?;
            migrated += 1;
        }
    }
    conn.execute_batch(&format!("PRAGMA user_version = {to}")).map_err(|e| format!("stamp: {e}"))?;
    Ok(Some(Outcome { from, to, backup: Some(backup), migrated }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_codes_order_like_versions() {
        assert_eq!(version_code("0.1.0"), 1_000); assert_eq!(version_code("v0.2.10"), 2_010); assert_eq!(version_code("1.0.0"), 1_000_000); assert_eq!(version_code("0.1.1-rc.2"), 1_001);
        assert!(version_code("0.10.0") > version_code("0.9.9")); assert_eq!(version_label(2_010), "0.2.10");
    }
    #[test]
    fn the_first_open_by_a_newer_build_stamps_backs_up_and_never_downgrades() {
        let d = std::env::temp_dir().join(format!("antiloki-upgrade-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap();
        let db = d.join("state.sqlite"); let p = db.to_string_lossy().to_string();
        assert_eq!(guard(&p, "0.2.0").unwrap(), None, "no database yet — nothing to protect");
        { let c = Connection::open(&db).unwrap(); c.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t (x); INSERT INTO t VALUES (1);").unwrap(); }
        // an unstamped database (every install before the guard existed) is backed up and stamped
        let o = guard(&p, "0.2.0").unwrap().unwrap();
        assert_eq!((o.from, o.to, o.migrated), (0, 2_000, 0)); assert!(o.backup.as_deref().unwrap().ends_with("state.sqlite.bak-vunstamped"));
        let b = Connection::open(o.backup.as_ref().unwrap()).unwrap(); assert_eq!(b.query_row::<i64, _, _>("SELECT count(*) FROM t", [], |r| r.get(0)).unwrap(), 1, "the copy is whole (WAL folded in)");
        // the same build again: nothing happens; a newer build: a second copy named by the old version
        assert_eq!(guard(&p, "0.2.0").unwrap().unwrap(), Outcome { from: 2_000, to: 2_000, backup: None, migrated: 0 });
        let o = guard(&p, "0.3.1").unwrap().unwrap();
        assert_eq!(o.from, 2_000); assert!(o.backup.unwrap().ends_with("state.sqlite.bak-v0.2.0"));
        // an older build meets a newer stamp: warned, untouched
        let o = guard(&p, "0.2.5").unwrap().unwrap(); assert_eq!((o.from, o.to, o.backup), (3_001, 2_005, None));
        let c = Connection::open(&db).unwrap(); assert_eq!(c.query_row::<i64, _, _>("PRAGMA user_version", [], |r| r.get(0)).unwrap(), 3_001);
        let _ = std::fs::remove_dir_all(&d);
    }
}
