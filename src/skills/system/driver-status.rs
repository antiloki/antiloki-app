//! driver-status.rs — twin of skills/system/driver-status.mjs. NAMES ARE CONTRACT: `driverStatus`
//! (default export). Daemon health + tick stats + dispatch gate the Pipeline banner renders.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: this single exported function mixes real
//! SQLite reads (brain.driver_ticks / notifications — normally the "standard" db lane) with real
//! filesystem I/O against a HARDCODED (non-input-injectable, HOME_DIR-anchored) constant —
//! `PID_FILE` = `RUN_DIR/driver-daemon.pid` and `HALT_FILE` = `RUN_DIR/driver-halt` (roots.mjs;
//! `RUN_DIR` = `HOME_DIR/run`, not DB-derived) — PLUS a real OS process-liveness probe
//! (`process.kill(pid, 0)`, genuinely non-deterministic: depends on the live host process table,
//! not seedable). Same precedent as skills/reconcilers/stale-pid-files.rs (identical PID_FILE +
//! isPidAlive mechanism) and skills/agents/god/god-driver-halt.rs (identical HALT_FILE mechanism):
//! both are bespoke-native, no fixture/frontier entry. Because this function's ONE unit (the
//! census counts `driverStatus` as a single default export) inseparably combines the fixture-safe
//! db half with the fixture-unsafe fs half, the whole function is bespoke-native — proven here via
//! #[cfg(test)] native tests (temp dir + `ANTILOKI_HOME` env-var injection, copying the
//! `with_home()` pattern from god-driver-halt.rs/stale-pid-files.rs exactly) instead.
//!
//! HOME_DIR resolution mirrors those siblings (no `dirs`-crate dependency): `ANTILOKI_HOME` env
//! override (plain path join, no cwd-resolve) else Node `os.homedir()` semantics (POSIX `$HOME`;
//! Windows `%USERPROFILE%` falling back to `%HOMEDRIVE%%HOMEPATH%`). `isPidAlive` FFI (unix
//! `kill(2)` with EPERM==alive; Windows `OpenProcess`+`GetExitCodeProcess`) is copied verbatim
//! from stale-pid-files.rs's `is_pid_alive`.
//!
//! Frozen-clock lane (still applied here for project-wide consistency with every other twin, even
//! though this module never runs through the conform-db two-lane comparison): JS
//! `datetime('now', ...)` -> `datetime('2026-01-01 00:00:00', ...)`; JS `Date.now()` ->
//! `1767225600000i64`. `parse_sql_ts_ms`/`days_from_civil` (SQLite `'YYYY-MM-DD HH:MM:SS'` cell ->
//! UTC epoch ms, mirrors JS `new Date(s.replace(' ','T')+'Z').getTime()`) are copied verbatim from
//! skills/system/benchmark-status.rs.
//!
//! `context.db` is always injected by the db lane in production, so the JS `if (!db) throw
//! err.contextMissing(...)` guard is unreachable here (Rust's `db: &Connection` is never absent) —
//! same convention as feature-inventory.rs / benchmark-status.rs.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

#[cfg(windows)]
fn homedir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let drive = std::env::var("HOMEDRIVE").ok().filter(|s| !s.is_empty());
    let path = std::env::var("HOMEPATH").ok().filter(|s| !s.is_empty());
    match (drive, path) {
        (Some(d), Some(p)) => Some(PathBuf::from(format!("{d}{p}"))),
        _ => None,
    }
}

#[cfg(not(windows))]
fn homedir() -> Option<PathBuf> {
    std::env::var("HOME").ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

// roots.mjs: HOME_DIR = resolve($ANTILOKI_HOME) if set+truthy, else join(homedir(), '.antiloki');
// RUN_DIR = join(HOME_DIR, 'run'); PID_FILE = join(RUN_DIR, 'driver-daemon.pid'); HALT_FILE =
// join(RUN_DIR, 'driver-halt').
fn run_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("run")
}
fn pid_file_path() -> PathBuf { run_dir().join("driver-daemon.pid") }
fn halt_file_path() -> PathBuf { run_dir().join("driver-halt") }

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

// JS: process.kill(pid, 0) — EPERM (exists, no permission to signal) is treated as alive; any
// other failure (ESRCH: no such process) is dead. Copied verbatim from stale-pid-files.rs.
#[cfg(unix)]
fn is_pid_alive(pid: f64) -> bool {
    if pid <= 0.0 || pid > i32::MAX as f64 { return false; }
    let ret = unsafe { kill(pid as i32, 0) };
    if ret == 0 { return true; }
    std::io::Error::last_os_error().raw_os_error() == Some(1) // EPERM
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(dw_desired_access: u32, b_inherit_handle: i32, dw_process_id: u32) -> *mut core::ffi::c_void;
    fn CloseHandle(h_object: *mut core::ffi::c_void) -> i32;
    fn GetExitCodeProcess(h_process: *mut core::ffi::c_void, lp_exit_code: *mut u32) -> i32;
}

#[cfg(windows)]
fn is_pid_alive(pid: f64) -> bool {
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    if pid <= 0.0 || pid > u32::MAX as f64 { return false; }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
        if handle.is_null() { return false; }
        let mut exit_code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut exit_code as *mut u32);
        CloseHandle(handle);
        ok != 0 && exit_code == STILL_ACTIVE
    }
}

// = JS Number(v), String-branch only (this module only ever feeds it a trimmed pid-file string).
fn js_number_str(s: &str) -> f64 {
    let t = s.trim();
    if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) }
}

// JSON.stringify(Number) collapses a whole-valued f64 to an integer-looking token (gotcha 2) —
// used for `pid` (raw `Number(content)`) and `cost_usd_1h` (`Number(...)` of a COALESCE(SUM,...)
// cell, which may land on an integer 0 or a REAL sum).
fn finite_num_to_json(f: f64) -> Value {
    if !f.is_finite() { return Value::Null; }
    if f.fract() == 0.0 && f.abs() < 9.0e15 { json!(f as i64) } else { json!(f) }
}

// = JS String(v) (used for `outcomes_5m[r.outcome] = r.c` — a bracket-assignment key is always
// ToString'd, and String(null) === 'null', unlike `String(v ?? '')`).
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

// JS truthiness (used for `if (content)` and `if (last?.started_at)` / `if (running?.started_at)`).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// days since 1970-01-01 for a proleptic-Gregorian y/m/d (m in 1..=12) — Howard Hinnant's
// algorithm. Copied verbatim from benchmark-status.rs.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

// Parse a 'YYYY-MM-DD HH:MM:SS' sqlite datetime cell as a UTC epoch-ms instant — mirrors JS
// `new Date(s.replace(' ','T') + 'Z').getTime()`. Malformed input -> None (JS would yield NaN;
// every NaN comparison below is false, matching the `if let Some(...)` guards here). Copied
// verbatim from benchmark-status.rs.
fn parse_sql_ts_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |lo: usize, hi: usize| s.get(lo..hi)?.parse::<i64>().ok();
    let y = num(0, 4)?;
    let mo = num(5, 7)?;
    let d = num(8, 10)?;
    let hh = num(11, 13)?;
    let mm = num(14, 16)?;
    let ss = num(17, 19)?;
    Some((((days_from_civil(y, mo, d) * 24 + hh) * 60 + mm) * 60 + ss) * 1000)
}


// JS wraps ticks_5m/ticks_1h/cost_usd_1h in a SINGLE try/catch — if the FIRST query fails (table
// missing) the other two never run either, and all three stay at their initial 0. Mirrored here
// by threading `?` through one fallible fn instead of three independent `if let Ok`s.
fn tick_rollup(db: &Connection) -> Result<(i64, i64, f64), String> {
    let r1 = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS c FROM brain.driver_ticks WHERE started_at >= datetime('2026-01-01 00:00:00', '-5 minutes')",
        &[],
    )?;
    let ticks_5m = r1.as_array().and_then(|a| a.first()).and_then(|row| row.get("c")).and_then(Value::as_i64).unwrap_or(0);
    let r2 = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS c FROM brain.driver_ticks WHERE started_at >= datetime('2026-01-01 00:00:00', '-1 hour')",
        &[],
    )?;
    let ticks_1h = r2.as_array().and_then(|a| a.first()).and_then(|row| row.get("c")).and_then(Value::as_i64).unwrap_or(0);
    let r3 = crate::db_json::query_rows(
        db,
        "SELECT COALESCE(SUM(cost_usd), 0) AS s FROM brain.driver_ticks WHERE started_at >= datetime('2026-01-01 00:00:00', '-1 hour')",
        &[],
    )?;
    let cost_usd_1h = r3.as_array().and_then(|a| a.first()).and_then(|row| row.get("s")).and_then(Value::as_f64).unwrap_or(0.0);
    Ok((ticks_5m, ticks_1h, cost_usd_1h))
}

pub fn driverStatus(_input: &Value, db: &Connection) -> Result<Value, String> {
    // Daemon pid file. Dead pid -> unlink orphan cruft. Mirrors JS exactly, including leaving
    // `pid`/`pid_file_exists` at their last-assigned value if the unlink itself fails (JS's
    // catch swallows that failure too, without resetting them back).
    let pid_path = pid_file_path();
    let mut pid: Value = Value::Null;
    let mut alive = false;
    let mut pid_file_exists = pid_path.exists();
    if pid_file_exists {
        if let Ok(raw) = std::fs::read_to_string(&pid_path) {
            let content = raw.trim();
            if !content.is_empty() {
                let n = js_number_str(content);
                if n.is_finite() && n > 0.0 {
                    pid = finite_num_to_json(n);
                    alive = is_pid_alive(n);
                }
            }
        }
        if !alive && std::fs::remove_file(&pid_path).is_ok() {
            pid_file_exists = false;
            pid = Value::Null;
        }
    }
    let halt_file_exists = halt_file_path().exists();

    // Most-recent tick.
    let last_row: Option<Value> = crate::db_json::query_rows(
        db,
        "SELECT started_at FROM brain.driver_ticks ORDER BY id DESC LIMIT 1",
        &[],
    )
    .ok()
    .and_then(|v| v.as_array().and_then(|a| a.first().cloned()));

    // 5m / 1h tick counts + cost rollup.
    let (ticks_5m, ticks_1h, cost_usd_1h) = tick_rollup(db).unwrap_or((0, 0, 0.0));

    // Outcome breakdown over last 5 minutes.
    let mut outcomes_5m = Map::new();
    if let Ok(rows) = crate::db_json::query_rows(
        db,
        "SELECT outcome, COUNT(*) AS c FROM brain.driver_ticks WHERE started_at >= datetime('2026-01-01 00:00:00', '-5 minutes') GROUP BY outcome",
        &[],
    ) {
        if let Some(arr) = rows.as_array() {
            for row in arr {
                let key = js_string(row.get("outcome").unwrap_or(&Value::Null));
                outcomes_5m.insert(key, row.get("c").cloned().unwrap_or(Value::Null));
            }
        }
    }

    // BE tick-loop detection: a recent tick (<15s) that the standalone daemon isn't accounting
    // for (no live pid) means the BE's own loop is the source.
    let mut be_tick_running = false;
    if let Some(last) = &last_row {
        let started_at = last.get("started_at");
        if truthy(started_at) {
            if let Some(ts_ms) = parse_sql_ts_ms(&js_string(started_at.unwrap())) {
                let since_ms = crate::clock::now_ms() - ts_ms;
                let daemon_accounts_for_it = !pid.is_null() && alive;
                if since_ms < 15_000 && !daemon_accounts_for_it { be_tick_running = true; }
            }
        }
    }

    // In-flight tick detection: a driver_ticks row with outcome='running', capped at 5min (a row
    // stuck 'running' longer than that is a crashed/hung tick and should surface as dead).
    let running_row: Option<Value> = crate::db_json::query_rows(
        db,
        "SELECT started_at FROM brain.driver_ticks WHERE outcome='running' ORDER BY id DESC LIMIT 1",
        &[],
    )
    .ok()
    .and_then(|v| v.as_array().and_then(|a| a.first().cloned()));
    let mut tick_in_progress = false;
    if let Some(running) = &running_row {
        let started_at = running.get("started_at");
        if truthy(started_at) {
            if let Some(ts_ms) = parse_sql_ts_ms(&js_string(started_at.unwrap())) {
                let age_ms = crate::clock::now_ms() - ts_ms;
                if age_ms < 300_000 { tick_in_progress = true; }
            }
        }
    }

    // Count of unresolved error notifications (informational only — no longer gates dispatch).
    let mut red_notifications_pending: i64 = 0;
    if let Ok(r) = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS c FROM notifications WHERE severity='error' AND dismissed_at IS NULL",
        &[],
    ) {
        red_notifications_pending = r.as_array().and_then(|a| a.first()).and_then(|row| row.get("c")).and_then(Value::as_i64).unwrap_or(0);
    }

    // PIPELINE-V2 is now THE driver (default on); =off is the emergency rollback to v1.
    let pipeline_v2_raw = std::env::var("ANTILOKI_PIPELINE_V2").unwrap_or_else(|_| "on".to_string());
    let pipeline_v2 = pipeline_v2_raw.to_lowercase() != "off";

    let mut out = Map::new();
    out.insert("pid".into(), pid);
    out.insert("alive".into(), json!(alive));
    out.insert("pid_file_exists".into(), json!(pid_file_exists));
    out.insert("halt_file_exists".into(), json!(halt_file_exists));
    out.insert(
        "last_tick_at".into(),
        last_row.as_ref().and_then(|l| l.get("started_at")).cloned().unwrap_or(Value::Null),
    );
    out.insert("ticks_5m".into(), json!(ticks_5m));
    out.insert("ticks_1h".into(), json!(ticks_1h));
    out.insert("cost_usd_1h".into(), finite_num_to_json(cost_usd_1h));
    out.insert("outcomes_5m".into(), Value::Object(outcomes_5m));
    out.insert("be_tick_running".into(), json!(be_tick_running));
    out.insert("tick_in_progress".into(), json!(tick_in_progress));
    out.insert("red_notifications_pending".into(), json!(red_notifications_pending));
    out.insert("pipeline_v2".into(), json!(pipeline_v2));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    // SAFETY: ANTILOKI_HOME / ANTILOKI_PIPELINE_V2 are process-global; serialize via the shared
    // crate-wide lock (test_env_lock.rs) — a local mutex here would not prevent a race with
    // OTHER files' tests over the same real env vars.
    fn with_home_and_env<T>(antiloki_home: &str, extra: &[(&str, &str)], f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", antiloki_home);
            for (k, v) in extra { std::env::set_var(k, v); }
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
            for (k, _) in extra { std::env::remove_var(k); }
        }
        result
    }
    fn with_home<T>(antiloki_home: &str, f: impl FnOnce() -> T) -> T { with_home_and_env(antiloki_home, &[], f) }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-driver-status-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // A pid the OS will not reuse within this test's lifetime: spawn a trivial child and wait for
    // it to exit before reading back its pid. Copied technique from stale-pid-files.rs.
    fn dead_pid() -> u32 {
        let mut child = if cfg!(windows) {
            Command::new("cmd").args(["/C", "exit", "0"]).spawn().unwrap()
        } else {
            Command::new("true").spawn().unwrap()
        };
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    fn make_db(with_driver_ticks: bool, with_notifications: bool) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH DATABASE ':memory:' AS brain;").unwrap();
        if with_driver_ticks {
            c.execute_batch(
                "CREATE TABLE brain.driver_ticks (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    started_at TEXT,
                    card_slug TEXT,
                    outcome TEXT,
                    cost_usd REAL DEFAULT 0
                )",
            )
            .unwrap();
        }
        if with_notifications {
            c.execute_batch(
                "CREATE TABLE notifications (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    severity TEXT,
                    dismissed_at TEXT
                )",
            )
            .unwrap();
        }
        c
    }
    fn insert_tick(c: &Connection, started_at: &str, outcome: &str, cost_usd: f64) {
        c.execute(
            "INSERT INTO brain.driver_ticks (started_at, outcome, cost_usd) VALUES (?1, ?2, ?3)",
            rusqlite::params![started_at, outcome, cost_usd],
        )
        .unwrap();
    }
    fn insert_notification(c: &Connection, severity: &str, dismissed_at: Option<&str>) {
        c.execute(
            "INSERT INTO notifications (severity, dismissed_at) VALUES (?1, ?2)",
            rusqlite::params![severity, dismissed_at],
        )
        .unwrap();
    }

    fn approx(a: f64, b: f64) -> bool { (a - b).abs() < 0.001 }

    #[test]
    fn empty_db_all_zeros_no_banner() {
        let dir = temp_dir("empty");
        let db = make_db(true, true);
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["pid"], Value::Null);
        assert_eq!(r["alive"], json!(false));
        assert_eq!(r["pid_file_exists"], json!(false));
        assert_eq!(r["halt_file_exists"], json!(false));
        assert_eq!(r["last_tick_at"], Value::Null);
        assert_eq!(r["ticks_5m"], json!(0));
        assert_eq!(r["ticks_1h"], json!(0));
        assert_eq!(r["cost_usd_1h"], json!(0));
        assert_eq!(r["outcomes_5m"], json!({}));
        assert_eq!(r["be_tick_running"], json!(false));
        assert_eq!(r["tick_in_progress"], json!(false));
        assert_eq!(r["red_notifications_pending"], json!(0));
        assert_eq!(r["pipeline_v2"], json!(true));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "pid", "alive", "pid_file_exists", "halt_file_exists", "last_tick_at", "ticks_5m", "ticks_1h",
                "cost_usd_1h", "outcomes_5m", "be_tick_running", "tick_in_progress", "red_notifications_pending",
                "pipeline_v2"
            ]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn counts_ticks_in_5m_and_1h_windows_and_cost() {
        let dir = temp_dir("counts");
        let db = make_db(true, true);
        insert_tick(&db, "2025-12-31 23:58:00", "fired", 0.01); // -2 min
        insert_tick(&db, "2025-12-31 23:30:00", "fired", 0.05); // -30 min
        insert_tick(&db, "2025-12-31 22:00:00", "fired", 0.99); // -2 hours
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["ticks_5m"], json!(1));
        assert_eq!(r["ticks_1h"], json!(2));
        assert!(approx(r["cost_usd_1h"].as_f64().unwrap(), 0.06));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn outcome_breakdown_in_last_5_minutes() {
        let dir = temp_dir("outcomes");
        let db = make_db(true, true);
        insert_tick(&db, "2026-01-01 00:00:00", "fired", 0.0);
        insert_tick(&db, "2026-01-01 00:00:00", "fired", 0.0);
        insert_tick(&db, "2026-01-01 00:00:00", "idle", 0.0);
        insert_tick(&db, "2026-01-01 00:00:00", "failed", 0.0);
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["outcomes_5m"], json!({ "fired": 2, "idle": 1, "failed": 1 }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn be_tick_running_true_when_recent_and_no_live_daemon() {
        let dir = temp_dir("betick-true");
        let db = make_db(true, true);
        insert_tick(&db, "2025-12-31 23:59:57", "fired", 0.0); // -3s
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["be_tick_running"], json!(true));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn be_tick_running_false_when_old() {
        let dir = temp_dir("betick-old");
        let db = make_db(true, true);
        insert_tick(&db, "2025-12-31 22:00:00", "fired", 0.0); // -2h, far past the 15s threshold
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["be_tick_running"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn be_tick_running_false_when_daemon_accounts_for_it() {
        let dir = temp_dir("betick-daemon");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("driver-daemon.pid"), std::process::id().to_string()).unwrap();
        let db = make_db(true, true);
        insert_tick(&db, "2025-12-31 23:59:57", "fired", 0.0); // -3s, would be true except daemon is alive
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["be_tick_running"], json!(false));
        assert_eq!(r["alive"], json!(true));
        assert_eq!(r["pid"], json!(std::process::id() as i64));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tick_in_progress_true_for_a_long_running_ai_tick() {
        let dir = temp_dir("tip-true");
        let db = make_db(true, true);
        insert_tick(&db, "2025-12-31 23:58:47", "running", 0.0); // -73s
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["tick_in_progress"], json!(true));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tick_in_progress_false_when_stuck_past_5_minutes() {
        let dir = temp_dir("tip-stuck");
        let db = make_db(true, true);
        insert_tick(&db, "2025-12-31 23:50:00", "running", 0.0); // -10min
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["tick_in_progress"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tick_in_progress_false_when_latest_tick_finished() {
        let dir = temp_dir("tip-finished");
        let db = make_db(true, true);
        insert_tick(&db, "2025-12-31 23:59:57", "fired", 0.0); // -3s but not 'running'
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["tick_in_progress"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn red_notifications_pending_counts_unresolved_errors_only() {
        let dir = temp_dir("red-notifs");
        let db = make_db(true, true);
        insert_notification(&db, "error", None);
        insert_notification(&db, "error", Some("2026-01-01 00:00:00"));
        insert_notification(&db, "warning", None);
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["red_notifications_pending"], json!(1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_notifications_table_yields_zero_no_crash() {
        let dir = temp_dir("no-notifs-table");
        let db = make_db(true, false);
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["red_notifications_pending"], json!(0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_driver_ticks_table_yields_all_zero_fields_no_crash() {
        let dir = temp_dir("no-ticks-table");
        let db = make_db(false, true);
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["ticks_5m"], json!(0));
        assert_eq!(r["ticks_1h"], json!(0));
        assert_eq!(r["cost_usd_1h"], json!(0));
        assert_eq!(r["last_tick_at"], Value::Null);
        assert_eq!(r["be_tick_running"], json!(false));
        assert_eq!(r["tick_in_progress"], json!(false));
        assert_eq!(r["outcomes_5m"], json!({}));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_pid_file_reports_alive_and_is_kept() {
        let dir = temp_dir("pid-alive");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        let pid_path = run_dir.join("driver-daemon.pid");
        fs::write(&pid_path, std::process::id().to_string()).unwrap();
        let db = make_db(true, true);
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["alive"], json!(true));
        assert_eq!(r["pid"], json!(std::process::id() as i64));
        assert_eq!(r["pid_file_exists"], json!(true));
        assert!(pid_path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dead_pid_file_gets_unlinked() {
        let dir = temp_dir("pid-dead");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        let pid_path = run_dir.join("driver-daemon.pid");
        let dp = dead_pid();
        fs::write(&pid_path, dp.to_string()).unwrap();
        let db = make_db(true, true);
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["alive"], json!(false));
        assert_eq!(r["pid"], Value::Null);
        assert_eq!(r["pid_file_exists"], json!(false));
        assert!(!pid_path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_pid_file_content_treated_as_dead_and_unlinked() {
        let dir = temp_dir("pid-empty");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        let pid_path = run_dir.join("driver-daemon.pid");
        fs::write(&pid_path, "   \n").unwrap();
        let db = make_db(true, true);
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["alive"], json!(false));
        assert_eq!(r["pid"], Value::Null);
        assert_eq!(r["pid_file_exists"], json!(false));
        assert!(!pid_path.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_numeric_pid_file_content_treated_as_dead_and_unlinked() {
        let dir = temp_dir("pid-nan");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        let pid_path = run_dir.join("driver-daemon.pid");
        fs::write(&pid_path, "not-a-number").unwrap();
        let db = make_db(true, true);
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["alive"], json!(false));
        assert_eq!(r["pid"], Value::Null);
        assert_eq!(r["pid_file_exists"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_pid_file_reports_null_pid_and_not_alive() {
        let dir = temp_dir("pid-none");
        let db = make_db(true, true);
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["pid"], Value::Null);
        assert_eq!(r["alive"], json!(false));
        assert_eq!(r["pid_file_exists"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn halt_file_existence_is_reported() {
        let dir = temp_dir("halt");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("driver-halt"), "").unwrap();
        let db = make_db(true, true);
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["halt_file_exists"], json!(true));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pipeline_v2_defaults_on_when_unset() {
        let dir = temp_dir("pv2-default");
        let db = make_db(true, true);
        let r = with_home(dir.to_str().unwrap(), || driverStatus(&json!({}), &db).unwrap());
        assert_eq!(r["pipeline_v2"], json!(true));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pipeline_v2_off_case_insensitive() {
        let dir = temp_dir("pv2-off");
        let db = make_db(true, true);
        let r = with_home_and_env(dir.to_str().unwrap(), &[("ANTILOKI_PIPELINE_V2", "OFF")], || {
            driverStatus(&json!({}), &db).unwrap()
        });
        assert_eq!(r["pipeline_v2"], json!(false));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pipeline_v2_any_other_value_stays_on() {
        let dir = temp_dir("pv2-other");
        let db = make_db(true, true);
        let r = with_home_and_env(dir.to_str().unwrap(), &[("ANTILOKI_PIPELINE_V2", "canary")], || {
            driverStatus(&json!({}), &db).unwrap()
        });
        assert_eq!(r["pipeline_v2"], json!(true));
        let _ = fs::remove_dir_all(&dir);
    }
}
