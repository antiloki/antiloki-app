//! system/work-start.rs — twin of skills/system/work-start.mjs. NAMES ARE CONTRACT: `workStart`
//! (default export). Marks a card user-prioritized (priority=5) so the driver picks it next.
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: this single exported function mixes real
//! SQLite reads/writes (cards, pl_sessions/pl_questions/pl_answers — normally the "standard" db
//! lane) with real filesystem I/O, via the local `checkDriverStatus()` helper, against a
//! HARDCODED (non-input-injectable, HOME_DIR-anchored) constant pair — `RUN_DIR/driver-halt` and
//! `RUN_DIR/driver-daemon.pid` (roots.mjs; `RUN_DIR` = `HOME_DIR/run`, not DB-derived). Same
//! precedent as skills/system/driver-status.rs (identical RUN_DIR/driver-halt +
//! driver-daemon.pid mechanism, identical "mixes db + fs in one inseparable unit" reasoning) and
//! skills/agents/god/god-driver-halt.rs (identical HALT_FILE mechanism). Because this function's
//! ONE unit (the census counts `workStart` as a single default export) inseparably combines the
//! fixture-safe db half with the fixture-unsafe fs half, the whole function is bespoke-native —
//! proven here via #[cfg(test)] native tests (temp dir + `ANTILOKI_HOME` env-var injection,
//! copying the `with_home()` pattern from god-driver-halt.rs/driver-status.rs exactly) instead of
//! a conform-db fixture. No frontier entry, no fixture JSON for this unit.
//!
//! Unlike driver-status.rs, `checkDriverStatus()` here does NOT probe OS process liveness — it
//! only checks file EXISTENCE (`existsSync`) for the halt file, and for the pid file it reads the
//! raw content and tests it's a non-empty `/^\d+$/` string (no numeric parse, no `process.kill`
//! probe) — so no OS process-liveness FFI is needed here (contrast driver-status.rs's
//! `is_pid_alive`).
//!
//! Frozen-clock lane: JS `datetime('now')` -> `datetime('2026-01-01 00:00:00')` (two UPDATE
//! statements: cards.updated_at, pl_sessions.updated_at).
//!
//! `context.db` is always injected by the db lane in production, so the JS `if (!db) throw
//! err.contextMissing(...)` guard is unreachable here (Rust's `db: &Connection` is never absent)
//! — same convention as driver-status.rs / feature-inventory.rs. Other throws
//! (VALIDATION_REQUIRED `slug` / NOT_FOUND `card` / VALIDATION_LOGIC closed-card) collapse to
//! `Err(String)` carrying the exact `AntilokiError` message text (`"work-start: <cause>"`),
//! matching knowledge-document-add.rs's convention.

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
// RUN_DIR = join(HOME_DIR, 'run').
fn run_dir() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("run")
}
fn halt_path() -> PathBuf { run_dir().join("driver-halt") }
fn pid_path() -> PathBuf { run_dir().join("driver-daemon.pid") }

// JS /^\d+$/.test(content) — no leading/trailing whitespace allowed (already trimmed by caller).
fn is_all_digits(s: &str) -> bool { !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) }

struct DriverStatusOut { halted: bool, daemon_running: bool, hint: Option<String> }

// JS checkDriverStatus(): existsSync(halt) -> halted; existsSync(pid) && readFileSync(pid).trim()
// matching /^\d+$/ -> daemon_running. readFileSync failures are swallowed (JS try/catch), leaving
// daemon_running at its prior (false) value — mirrored here by only setting it inside `if let Ok`.
fn check_driver_status() -> DriverStatusOut {
    let halted = halt_path().exists();
    let mut daemon_running = false;
    if pid_path().exists() {
        if let Ok(raw) = std::fs::read_to_string(pid_path()) {
            let content = raw.trim();
            daemon_running = !content.is_empty() && is_all_digits(content);
        }
    }
    let hint = if halted && !daemon_running {
        Some("Driver is HALTED and daemon is NOT running. Remove the halt file (.antiloki/run/driver-halt) AND start the daemon: `node bin/driver-daemon.mjs` in a separate terminal, or press [s] in the FE-TUI Pipeline pane.".to_string())
    } else if halted {
        Some("Driver is HALTED. Remove .antiloki/run/driver-halt to resume ticking.".to_string())
    } else if !daemon_running {
        Some("Driver daemon is NOT running. Start it: `node bin/driver-daemon.mjs` in a separate terminal, or press [s] in the FE-TUI Pipeline pane.".to_string())
    } else {
        None
    };
    DriverStatusOut { halted, daemon_running, hint }
}

const ACTIVE_PRIORITY: i64 = 5;

// JS sizeEstimate(card): decode cards.body_json (a JSON-encoded TEXT column) into a plain string,
// falling back to the raw column text on a parse failure, then bucket by length/file-mentions/
// keyword heuristics. Returns XS|S|M|L.
fn size_estimate(card: &Value) -> &'static str {
    let raw_str = card.get("body_json").and_then(|v| v.as_str()).unwrap_or("");
    let mut body = String::new();
    if !raw_str.is_empty() {
        match serde_json::from_str::<Value>(raw_str) {
            Ok(Value::String(ps)) => body = ps,
            Ok(other) => body = serde_json::to_string(&other).unwrap_or_default(),
            Err(_) => body = raw_str.to_string(), // catch { body = String(card?.body_json ?? '') }
        }
    }
    let len = body.encode_utf16().count(); // JS .length = UTF-16 code units
    let lower = body.to_lowercase();
    let big_keywords = regex::Regex::new(r"\b(epic|multiple|decompose|several|many files|across)\b").unwrap();
    let file_re = regex::Regex::new(r"\.(mjs|js|ts|tsx|json|md|sql)\b").unwrap();
    let file_mentions = file_re.find_iter(&body).count();
    if len > 4000 || file_mentions >= 4 || big_keywords.is_match(&lower) { return "L"; }
    if len > 2000 || file_mentions >= 2 { return "M"; }
    if len > 600 { return "S"; }
    "XS"
}

pub fn workStart(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = input.get("slug").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let slug = match slug {
        Some(s) => s,
        None => return Err("work-start: slug is required".to_string()),
    };

    let card_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, status, priority, skip_pl_flag, body_json FROM cards WHERE slug = ?",
        &[json!(slug)],
    )?;
    let card = match card_rows.as_array().and_then(|a| a.first()) {
        Some(c) => c.clone(),
        None => return Err(format!("work-start: card '{slug}' not found")),
    };

    let status = card.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if status == "done" || status == "archived" {
        return Err(format!("work-start: card '{slug}' is {status}; cannot start work on a closed card"));
    }

    let prev_priority = card.get("priority").cloned().unwrap_or(Value::Null);
    let prev_skip_pl = card.get("skip_pl_flag").cloned().unwrap_or(Value::Null);

    let is_epic = slug.to_ascii_uppercase().starts_with("EPIC-");
    let has_children = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) c FROM cards WHERE parent_slug = ?",
        &[json!(slug)],
    )
    .ok()
    .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
    .and_then(|row| row.get("c").and_then(Value::as_i64))
    .unwrap_or(0)
        > 0;
    let size = size_estimate(&card);
    let too_big_for_skip = size == "M" || size == "L";
    let route_through_pl = is_epic || has_children || too_big_for_skip;

    let target_skip_pl: i64 = if route_through_pl { 0 } else { 1 };
    if prev_priority != json!(ACTIVE_PRIORITY) || prev_skip_pl != json!(target_skip_pl) {
        crate::db_json::run_write(
            db,
            "UPDATE cards SET priority = ?, skip_pl_flag = ?, updated_at = datetime('2026-01-01 00:00:00') WHERE slug = ?",
            &[json!(ACTIVE_PRIORITY), json!(target_skip_pl), json!(slug)],
        )?;
    }

    let stale_sessions = crate::db_json::query_rows(
        db,
        "SELECT s.id, s.status FROM pl_sessions s WHERE s.approved_card_slug = ? AND s.status = 'clarifying' AND EXISTS (SELECT 1 FROM pl_questions q WHERE q.session_id = s.id AND NOT EXISTS (SELECT 1 FROM pl_answers a WHERE a.question_id = q.id))",
        &[json!(slug)],
    )?;
    let mut abandoned: i64 = 0;
    if let Some(rows) = stale_sessions.as_array() {
        for s in rows {
            let id = s.get("id").cloned().unwrap_or(Value::Null);
            crate::db_json::run_write(
                db,
                "UPDATE pl_sessions SET status = 'abandoned', updated_at = datetime('2026-01-01 00:00:00') WHERE id = ?",
                &[id],
            )?;
            abandoned += 1;
        }
    }

    let priority_changed = prev_priority != json!(ACTIVE_PRIORITY);
    let skip_pl_changed = prev_skip_pl != json!(target_skip_pl);
    let route_note = if route_through_pl {
        format!(
            "routed through PL ({}) \u{2014} driver will fire pl.session.start, NOT contract.draft.auto.",
            if is_epic { "epic slug" } else { "card has children" }
        )
    } else {
        "skip_pl=1 \u{2014} driver will route directly to contract.draft.auto.".to_string()
    };
    let driver_status = check_driver_status();
    let base_message = if abandoned > 0 {
        format!(
            "{slug} {}; abandoned {abandoned} stale clarify session(s); {route_note}",
            if priority_changed { "prioritized" } else { "re-confirmed" }
        )
    } else if priority_changed || skip_pl_changed {
        format!("{slug} prioritized; {route_note}")
    } else {
        format!("{slug} already prioritized; nothing to do.")
    };

    let summary_md = match &driver_status.hint {
        Some(h) => format!("{base_message}\n\n\u{26A0} {h}"),
        None => format!("{base_message}\n\nDriver is running \u{2014} should pick this card up within the next tick."),
    };

    let mut driver_status_obj = Map::new();
    driver_status_obj.insert("halted".into(), json!(driver_status.halted));
    driver_status_obj.insert("daemon_running".into(), json!(driver_status.daemon_running));
    driver_status_obj.insert("hint".into(), driver_status.hint.map(|h| json!(h)).unwrap_or(Value::Null));

    let mut out = Map::new();
    out.insert("slug".into(), json!(slug));
    out.insert("prev_priority".into(), prev_priority);
    out.insert("new_priority".into(), json!(ACTIVE_PRIORITY));
    out.insert("prev_skip_pl_flag".into(), prev_skip_pl);
    out.insert("new_skip_pl_flag".into(), json!(target_skip_pl));
    out.insert("routed_through_pl".into(), json!(route_through_pl));
    out.insert("status".into(), json!(status));
    out.insert("noop".into(), json!(!priority_changed && !skip_pl_changed && abandoned == 0));
    out.insert("abandoned_sessions".into(), json!(abandoned));
    out.insert("driver_status".into(), Value::Object(driver_status_obj));
    out.insert("message".into(), json!(base_message));
    out.insert("summary_md".into(), json!(summary_md));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn with_home<T>(antiloki_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", antiloki_home);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-work-start-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE cards (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                slug TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'backlog',
                body_json TEXT,
                priority INTEGER,
                parent_slug TEXT,
                skip_pl_flag INTEGER NOT NULL DEFAULT 0,
                updated_at TEXT
            );
            CREATE TABLE pl_sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                raw_intent TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'clarifying',
                started_by TEXT NOT NULL,
                approved_card_slug TEXT,
                updated_at TEXT
            );
            CREATE TABLE pl_questions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL,
                step TEXT NOT NULL,
                question_md TEXT NOT NULL,
                allows_writein INTEGER NOT NULL DEFAULT 1,
                order_index INTEGER NOT NULL
            );
            CREATE TABLE pl_answers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                question_id INTEGER NOT NULL UNIQUE,
                writein_md TEXT,
                answered_by TEXT NOT NULL
            );",
        )
        .unwrap();
        c
    }

    fn seed_card(c: &Connection, slug: &str, status: &str, priority: Option<i64>) {
        c.execute(
            "INSERT INTO cards (slug, title, status, priority) VALUES (?1, 'test', ?2, ?3)",
            rusqlite::params![slug, status, priority],
        )
        .unwrap();
    }

    #[test]
    fn throws_when_slug_missing() {
        let db = make_db();
        let r = workStart(&json!({}), &db);
        assert_eq!(r, Err("work-start: slug is required".to_string()));
    }

    #[test]
    fn throws_not_found_for_missing_card() {
        let dir = temp_dir("not-found");
        let db = make_db();
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"DOES-NOT-EXIST"}), &db));
        assert_eq!(r, Err("work-start: card 'DOES-NOT-EXIST' not found".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sets_priority_5_on_backlog_card() {
        let dir = temp_dir("set-priority");
        let db = make_db();
        seed_card(&db, "FEAT-X", "backlog", None);
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"FEAT-X"}), &db).unwrap());
        assert_eq!(r["new_priority"], json!(5));
        assert_eq!(r["noop"], json!(false));
        let row = crate::db_json::query_rows(&db, "SELECT priority FROM cards WHERE slug = 'FEAT-X'", &[]).unwrap();
        assert_eq!(row[0]["priority"], json!(5));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn idempotent_when_already_priority_5_and_skip_pl_1() {
        let dir = temp_dir("idempotent");
        let db = make_db();
        db.execute(
            "INSERT INTO cards (slug, title, status, priority, skip_pl_flag) VALUES ('FEAT-X', 'x', 'backlog', 5, 1)",
            [],
        )
        .unwrap();
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"FEAT-X"}), &db).unwrap());
        assert_eq!(r["noop"], json!(true));
        assert_eq!(r["new_priority"], json!(5));
        assert_eq!(r["new_skip_pl_flag"], json!(1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sets_skip_pl_flag_1_on_first_call() {
        let dir = temp_dir("skip-pl-first");
        let db = make_db();
        db.execute("INSERT INTO cards (slug, title, status) VALUES ('FEAT-FRESH', 'fresh', 'backlog')", []).unwrap();
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"FEAT-FRESH"}), &db).unwrap());
        assert_eq!(r["new_priority"], json!(5));
        assert_eq!(r["new_skip_pl_flag"], json!(1));
        let row = crate::db_json::query_rows(&db, "SELECT priority, skip_pl_flag FROM cards WHERE slug = 'FEAT-FRESH'", &[]).unwrap();
        assert_eq!(row[0]["priority"], json!(5));
        assert_eq!(row[0]["skip_pl_flag"], json!(1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn epic_slug_routes_through_pl() {
        let dir = temp_dir("epic-routes");
        let db = make_db();
        db.execute("INSERT INTO cards (slug, title, status) VALUES ('EPIC-NEW', 'epic', 'backlog')", []).unwrap();
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"EPIC-NEW"}), &db).unwrap());
        assert_eq!(r["new_priority"], json!(5));
        assert_eq!(r["new_skip_pl_flag"], json!(0));
        assert_eq!(r["routed_through_pl"], json!(true));
        assert!(r["message"].as_str().unwrap().contains("routed through PL"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn card_with_children_routes_through_pl() {
        let dir = temp_dir("children-route");
        let db = make_db();
        db.execute("INSERT INTO cards (slug, title, status) VALUES ('PARENT', 'parent', 'backlog')", []).unwrap();
        db.execute(
            "INSERT INTO cards (slug, title, status, parent_slug) VALUES ('PARENT-CHILD-1', 'child', 'backlog', 'PARENT')",
            [],
        )
        .unwrap();
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"PARENT"}), &db).unwrap());
        assert_eq!(r["routed_through_pl"], json!(true));
        assert_eq!(r["new_skip_pl_flag"], json!(0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn leaf_card_gets_skip_pl_1() {
        let dir = temp_dir("leaf");
        let db = make_db();
        db.execute("INSERT INTO cards (slug, title, status) VALUES ('FEAT-LEAF', 'leaf', 'backlog')", []).unwrap();
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"FEAT-LEAF"}), &db).unwrap());
        assert_eq!(r["routed_through_pl"], json!(false));
        assert_eq!(r["new_skip_pl_flag"], json!(1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn large_body_card_refuses_skip_pl() {
        let dir = temp_dir("big-body");
        let db = make_db();
        let big_body = "x".repeat(5000);
        db.execute(
            "INSERT INTO cards (slug, title, status, body_json) VALUES ('FEAT-BIG', 'big', 'backlog', ?1)",
            rusqlite::params![serde_json::to_string(&big_body).unwrap()],
        )
        .unwrap();
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"FEAT-BIG"}), &db).unwrap());
        assert_eq!(r["routed_through_pl"], json!(true));
        assert_eq!(r["new_skip_pl_flag"], json!(0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn body_mentioning_many_files_routes_through_pl() {
        let dir = temp_dir("many-files");
        let db = make_db();
        let body = "touch a.mjs and b.mjs and c.mjs and d.mjs across multiple modules";
        db.execute(
            "INSERT INTO cards (slug, title, status, body_json) VALUES ('FEAT-MANY', 'many', 'backlog', ?1)",
            rusqlite::params![serde_json::to_string(&body).unwrap()],
        )
        .unwrap();
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"FEAT-MANY"}), &db).unwrap());
        assert_eq!(r["routed_through_pl"], json!(true));
        assert_eq!(r["new_skip_pl_flag"], json!(0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_on_done_card() {
        let dir = temp_dir("done");
        let db = make_db();
        seed_card(&db, "EPIC-DONE", "done", None);
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"EPIC-DONE"}), &db));
        assert_eq!(r, Err("work-start: card 'EPIC-DONE' is done; cannot start work on a closed card".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_on_archived_card() {
        let dir = temp_dir("archived");
        let db = make_db();
        seed_card(&db, "EPIC-ARC", "archived", None);
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"EPIC-ARC"}), &db));
        assert_eq!(r, Err("work-start: card 'EPIC-ARC' is archived; cannot start work on a closed card".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn works_on_doing_card_priority_bump() {
        let dir = temp_dir("doing-bump");
        let db = make_db();
        seed_card(&db, "EPIC-DOING", "doing", Some(2));
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"EPIC-DOING"}), &db).unwrap());
        assert_eq!(r["prev_priority"], json!(2));
        assert_eq!(r["new_priority"], json!(5));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn abandons_stale_clarifying_sessions_with_unanswered_questions() {
        let dir = temp_dir("abandon-stale");
        let db = make_db();
        seed_card(&db, "EPIC-STALE", "backlog", None);
        db.execute(
            "INSERT INTO pl_sessions (raw_intent, status, started_by, approved_card_slug) VALUES ('short intent', 'clarifying', 'driver:auto', 'EPIC-STALE')",
            [],
        )
        .unwrap();
        let sess_id = db.last_insert_rowid();
        db.execute(
            "INSERT INTO pl_questions (session_id, step, question_md, allows_writein, order_index) VALUES (?1, 'clarify', 'What exactly?', 1, 0)",
            rusqlite::params![sess_id],
        )
        .unwrap();
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"EPIC-STALE"}), &db).unwrap());
        assert_eq!(r["abandoned_sessions"], json!(1));
        let row = crate::db_json::query_rows(&db, "SELECT status FROM pl_sessions WHERE id = ?1", &[json!(sess_id)]).unwrap();
        assert_eq!(row[0]["status"], json!("abandoned"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn does_not_abandon_fully_answered_clarifying_session() {
        let dir = temp_dir("keep-answered");
        let db = make_db();
        seed_card(&db, "EPIC-ANSWERED", "backlog", None);
        db.execute(
            "INSERT INTO pl_sessions (raw_intent, status, started_by, approved_card_slug) VALUES ('x', 'clarifying', 'driver:auto', 'EPIC-ANSWERED')",
            [],
        )
        .unwrap();
        let sess_id = db.last_insert_rowid();
        db.execute(
            "INSERT INTO pl_questions (session_id, step, question_md, allows_writein, order_index) VALUES (?1, 'clarify', 'Q', 1, 0)",
            rusqlite::params![sess_id],
        )
        .unwrap();
        let q_id = db.last_insert_rowid();
        db.execute(
            "INSERT INTO pl_answers (question_id, writein_md, answered_by) VALUES (?1, 'A', 'human:cli')",
            rusqlite::params![q_id],
        )
        .unwrap();
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"EPIC-ANSWERED"}), &db).unwrap());
        assert_eq!(r["abandoned_sessions"], json!(0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn driver_status_surfaces_halted_and_no_daemon_hint() {
        let dir = temp_dir("driver-hint");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("driver-halt"), "2026-01-01T00:00:00.000Z").unwrap();
        let db = make_db();
        seed_card(&db, "FEAT-HINT", "backlog", None);
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"FEAT-HINT"}), &db).unwrap());
        assert_eq!(r["driver_status"]["halted"], json!(true));
        assert_eq!(r["driver_status"]["daemon_running"], json!(false));
        assert!(r["driver_status"]["hint"].as_str().unwrap().contains("HALTED and daemon is NOT running"));
        assert!(r["summary_md"].as_str().unwrap().contains("\u{26A0}"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn driver_status_running_normally_when_daemon_pid_present() {
        let dir = temp_dir("driver-ok");
        let run_dir = dir.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("driver-daemon.pid"), "12345").unwrap();
        let db = make_db();
        seed_card(&db, "FEAT-OK", "backlog", None);
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"FEAT-OK"}), &db).unwrap());
        assert_eq!(r["driver_status"]["halted"], json!(false));
        assert_eq!(r["driver_status"]["daemon_running"], json!(true));
        assert_eq!(r["driver_status"]["hint"], Value::Null);
        assert!(r["summary_md"].as_str().unwrap().contains("Driver is running"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn already_prioritized_no_stale_sessions_is_noop_message() {
        let dir = temp_dir("noop-msg");
        let db = make_db();
        db.execute(
            "INSERT INTO cards (slug, title, status, priority, skip_pl_flag) VALUES ('FEAT-DONE-ALREADY', 'x', 'backlog', 5, 1)",
            [],
        )
        .unwrap();
        let r = with_home(dir.to_str().unwrap(), || workStart(&json!({"slug":"FEAT-DONE-ALREADY"}), &db).unwrap());
        assert_eq!(r["message"], json!("FEAT-DONE-ALREADY already prioritized; nothing to do."));
        let _ = fs::remove_dir_all(&dir);
    }
}
