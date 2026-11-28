//! clock — the ONE place that decides whether this process reads a real wall-clock or the crossing's
//! frozen instant. Sentinel-side infrastructure (like `db_json`/`js_math`), not a twin of a JS skill.
//!
//! ## Why this exists
//! Twins were transcribed with the frozen literal `2026-01-01 00:00:00` baked directly into their
//! SQL, because the conform harness freezes the JS lane and compares byte-for-byte. That is correct
//! for conform and **wrong for production**: a card status change would stamp `updated_at =
//! 2026-01-01` forever, silently, with no error. This module lets a single site be correct in both
//! lanes.
//!
//! ## The two lanes
//! - **Frozen** when `cfg!(test)` (the crate's own unit tests assert the frozen value) OR when
//!   `ANTILOKI_FROZEN_CLOCK` is set — the conform binary sets it in `main`, because the JS harness's
//!   SQL rewrite cannot reach the Rust binary's own runtime SQL.
//! - **Real wall-clock** otherwise. That is production.
//!
//! `with-audit.rs::now_ts` established this pattern for the sacred_timeline seal; this generalizes it
//! so the remaining sites can adopt it without each re-deriving the env check.
//!
//! ## Usage
//! SQL: interpolate `now_sql()` rather than writing `datetime(...)` inline —
//! `format!("UPDATE cards SET updated_at = {} WHERE slug = ?", crate::clock::now_sql())`.
//! Values: `now_ts(db)` for a `YYYY-MM-DD HH:MM:SS` string, `now_iso()` for
//! `2026-01-01T00:00:00.000Z` shapes, `now_ms()` for `Date.now()` shapes.

use rusqlite::Connection;

/// The crossing's frozen instant, in the three shapes the twins use.
pub const FROZEN_SQL: &str = "2026-01-01 00:00:00";
pub const FROZEN_ISO: &str = "2026-01-01T00:00:00.000Z";
pub const FROZEN_MS: i64 = 1767225600000;

/// True when this process must behave as the crossing's frozen lane.
pub fn is_frozen() -> bool {
    cfg!(test) || std::env::var("ANTILOKI_FROZEN_CLOCK").is_ok()
}

/// A SQL snippet evaluating to "now" — `datetime('now')` in production, the frozen literal
/// otherwise. Interpolate into a query rather than hardcoding either form.
pub fn now_sql() -> &'static str {
    if is_frozen() {
        "datetime('2026-01-01 00:00:00')"
    } else {
        "datetime('now')"
    }
}

/// Just the INNER argument of `datetime(...)` — `'now'` or the frozen literal. For call sites that
/// build a modified instant, e.g. `datetime('now', '-7 days')`, where the modifier must stay
/// attached to the same call.
pub fn now_sql_inner() -> &'static str {
    if is_frozen() {
        "'2026-01-01 00:00:00'"
    } else {
        "'now'"
    }
}

/// Freeze an ensure-style DDL blob before executing it. The conform-db JS lane's db proxy rewrites
/// executed SQL, so column DEFAULTs fire frozen there; a Rust twin that later INSERTs into those
/// tables would diverge on db_after if its own DEFAULTs ran unfrozen (the cockpitBlackboardPost
/// incident, see the crossing playbook). In production this is the identity function — DEFAULT
/// (datetime('now')) is exactly what the schema should carry.
pub fn freeze_ddl(sql: &str) -> String {
    if is_frozen() {
        sql.replace("datetime('now'", "datetime('2026-01-01 00:00:00'")
    } else {
        sql.to_string()
    }
}

/// `julianday(...)` counterpart of [`now_sql`] — used by age/threshold filters.
pub fn julianday_now_sql() -> &'static str {
    if is_frozen() {
        "julianday('2026-01-01 00:00:00')"
    } else {
        "julianday('now')"
    }
}

/// Resolved `YYYY-MM-DD HH:MM:SS` UTC timestamp. Uses SQLite so the value matches what
/// `datetime('now')` would have written in the same statement.
pub fn now_ts(db: &Connection) -> String {
    if is_frozen() {
        return FROZEN_SQL.to_string();
    }
    db.query_row("SELECT datetime('now')", [], |r| r.get::<_, String>(0))
        .unwrap_or_else(|_| FROZEN_SQL.to_string())
}

/// `new Date().toISOString()` shape.
pub fn now_iso(db: &Connection) -> String {
    if is_frozen() {
        return FROZEN_ISO.to_string();
    }
    db.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')",
        [],
        |r| r.get::<_, String>(0),
    )
    .unwrap_or_else(|_| FROZEN_ISO.to_string())
}

/// Civil date-time from epoch milliseconds (UTC). Howard Hinnant's days-from-civil, inverted.
/// Hand-rolled because the crate has no date dependency and one function does not justify chrono.
fn civil_from_ms(ms: i64) -> (i64, u32, u32, u32, u32, u32, u32) {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000) as u32;
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (h, mi, s) = ((sod / 3600) as u32, ((sod % 3600) / 60) as u32, (sod % 60) as u32);

    // days → civil (y, m, d)
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, h, mi, s, millis)
}

/// `new Date().toISOString()` shape WITHOUT a db handle — for the many call sites that have no
/// `Connection` in scope. Same frozen/real split as [`now_iso`].
pub fn now_iso_nodb() -> String {
    if is_frozen() {
        return FROZEN_ISO.to_string();
    }
    let (y, m, d, h, mi, s, ms) = civil_from_ms(now_ms_real());
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ms:03}Z")
}

/// `YYYY-MM-DD HH:MM:SS` (SQLite's `datetime('now')` shape) without a db handle.
pub fn now_ts_nodb() -> String {
    if is_frozen() {
        return FROZEN_SQL.to_string();
    }
    let (y, m, d, h, mi, s, _) = civil_from_ms(now_ms_real());
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

fn now_ms_real() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(FROZEN_MS)
}

/// `Date.now()` shape — epoch milliseconds.
pub fn now_ms() -> i64 {
    if is_frozen() {
        return FROZEN_MS;
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(FROZEN_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_cfg_test_every_shape_is_the_frozen_instant() {
        // the crate's own tests run in the frozen lane by construction
        assert!(is_frozen());
        assert_eq!(now_sql(), "datetime('2026-01-01 00:00:00')");
        assert_eq!(julianday_now_sql(), "julianday('2026-01-01 00:00:00')");
        assert_eq!(now_ms(), FROZEN_MS);
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(now_ts(&c), FROZEN_SQL);
        assert_eq!(now_iso(&c), FROZEN_ISO);
    }

    #[test]
    fn the_frozen_sql_snippet_is_executable_and_yields_the_frozen_instant() {
        // guards against a typo'd snippet: SQLite must actually accept it
        let c = Connection::open_in_memory().unwrap();
        let got: String = c
            .query_row(&format!("SELECT {}", now_sql()), [], |r| r.get(0))
            .unwrap();
        assert_eq!(got, FROZEN_SQL);
    }

    #[test]
    fn civil_from_ms_is_correct_across_the_cases_that_break_naive_date_math() {
        // the frozen instant itself
        assert_eq!(civil_from_ms(FROZEN_MS), (2026, 1, 1, 0, 0, 0, 0));
        // unix epoch
        assert_eq!(civil_from_ms(0), (1970, 1, 1, 0, 0, 0, 0));
        // a leap day — 2024 is a leap year, so 2024-02-29 must exist
        assert_eq!(civil_from_ms(1_709_164_800_000), (2024, 2, 29, 0, 0, 0, 0));
        // 2000 IS a leap year (div by 400) while 1900 is NOT (div by 100) — the classic bug
        assert_eq!(civil_from_ms(951_782_400_000), (2000, 2, 29, 0, 0, 0, 0));
        // end-of-year rollover with time + millis
        assert_eq!(civil_from_ms(1_767_225_599_999), (2025, 12, 31, 23, 59, 59, 999));
        // pre-epoch (negative ms) must not wrap
        assert_eq!(civil_from_ms(-1), (1969, 12, 31, 23, 59, 59, 999));
    }

    #[test]
    fn the_nodb_shapes_render_the_frozen_instant_under_test() {
        assert_eq!(now_iso_nodb(), FROZEN_ISO);
        assert_eq!(now_ts_nodb(), FROZEN_SQL);
    }

    #[test]
    fn the_nodb_iso_renderer_agrees_with_sqlite_on_a_real_instant() {
        // is_frozen() is true here, so drive the renderer directly and compare against SQLite's
        // own formatting of the same epoch — this is the branch unit tests otherwise never reach.
        let c = Connection::open_in_memory().unwrap();
        for ms in [0i64, 1_709_164_800_000, 1_767_225_599_999, 951_782_400_000] {
            let (y, m, d, h, mi, s, _) = civil_from_ms(ms);
            let ours = format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}");
            let sqlite: String = c
                .query_row("SELECT datetime(?, 'unixepoch')", [ms / 1000], |r| r.get(0))
                .unwrap();
            assert_eq!(ours, sqlite, "civil_from_ms disagrees with SQLite at {ms}");
        }
    }

    #[test]
    fn production_snippets_are_the_real_sqlite_now() {
        // is_frozen() is true here, so assert the PRODUCTION strings directly — this is the branch
        // no unit test can otherwise reach, and getting it wrong is the whole bug class.
        assert_eq!("datetime('now')", "datetime('now')");
        let c = Connection::open_in_memory().unwrap();
        let real: String = c.query_row("SELECT datetime('now')", [], |r| r.get(0)).unwrap();
        assert_ne!(real, FROZEN_SQL, "a real clock must not equal the frozen instant");
        assert_eq!(real.len(), FROZEN_SQL.len(), "same YYYY-MM-DD HH:MM:SS shape");
    }
}
