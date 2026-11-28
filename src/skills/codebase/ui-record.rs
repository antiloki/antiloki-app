//! ui-record.rs — twin of skills/codebase/ui-record.mjs (UI-STORIES P3, record mode). NAMES ARE
//! CONTRACT: default `uiRecord` (ported here as `ui_record`).
//!
//! PARITY-DEFERRED: past the `stop`-signal short-circuit and input validation, JS opens a real
//! VISIBLE playwright/chromium session (`loadPlaywright()` from `./design-capture-live.mjs`,
//! `chromium.launch({ headless: false })`, `page.addInitScript(RECORDER)`, `page.goto`, an overlay
//! paint-verify retry loop, then a poll loop reading `window.__antiloki_story` + the
//! `code_record_control.stop` flag until the operator stops or `timeoutMs` elapses) — none of that
//! is JSON-fixturable, same class as `codebase_design_capture_live::designCaptureLive`'s deferred
//! browser boundary and `codebase_ui_explore::ui_explore`'s. This crate has no playwright/CDP
//! client wired, so `ui_record` below never reaches that branch — it takes the same real
//! degenerate path JS's OWN `loadPlaywright()` already takes when the package isn't resolvable:
//! `{ ok: false, error: 'playwright not resolvable — install it in the engine or the FE' }`, a
//! returned RESULT (JS never throws for this branch — only a missing `url`, via `err.required`,
//! and a missing `db`, via `err.contextMissing`, throw; the latter is unreachable here — `db: &
//! Connection` is a required, non-nullable reference, same convention as every sibling db-lane
//! twin).
//!
//! Ported faithfully (real deterministic logic, unit-tested below):
//! - `ensureCodeSchema(db, '')` best-effort call (JS: `try { ... } catch { }`) before anything else
//! - the `input.stop` short-circuit: upsert `code_record_control` (id=1, stop=1, active=0,
//!   updated_at=now) and return `{ ok: true, stopped: true }` — this path never touches `url` or
//!   playwright at all, matching JS's early `return` before the `url` guard
//! - `url` required guard (`err.required('ui-record', 'url')`) + trailing-slash strip
//!   (`.replace(/\/$/, '')` — exactly one trailing slash)
//! - `origin` default `'atlas'` (`input?.origin ?? 'atlas'`)
//! - `timeoutMs` clamp: `Math.max(10000, Math.min(Number(input?.timeoutMs) || 600000, 1800000))`
//! - the no-playwright result shape, including the fixed `error` message text
//! - the `finally` block's `UPDATE code_record_control SET active = 0 ...` — reached here because
//!   the deferred branch is never entered so control falls straight into it (JS: this statement
//!   runs unconditionally on the no-playwright early `return` too, since `finally` always runs)
//!
//! Not ported (cannot be, deterministically): the `RECORDER` in-page JS string (exists only to be
//! `page.evaluate`d inside a real browser), `chromium.launch`/`page.goto`/the overlay paint-verify
//! loop, the `code_record_control` active-session upsert + stop-poll loop that only run AFTER a
//! browser is actually open, and the `code_user_stories` persistence of the recorded `story` — all
//! of it lives strictly inside the deferred browser session this twin never opens.
use rusqlite::Connection;
use serde_json::{json, Value};

fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
} // = JS Number(v)

// `Number(v) || default` — NaN and 0 (falsy) both fall through to default, matching JS `||`.
fn number_or(v: Option<&Value>, default: f64) -> f64 {
    let n = js_number(v);
    if n.is_nan() || n == 0.0 { default } else { n }
}

fn truthy(v: Option<&Value>) -> bool {
    // JS `if (input?.stop)` truthiness
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `export default async function uiRecord(input, context)`. See module doc: `context.db`
/// missing is unreachable (non-nullable `db: &Connection`, same convention as every sibling).
pub fn ui_record(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::codebase_code_schema::ensureCodeSchema(db, &Value::String(String::new()));

    // STOP signal from the antiloki FE (no browser) — flip the flag the live record session polls.
    if truthy(input.get("stop")) {
        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO code_record_control (id, stop, active, updated_at) VALUES (1, 1, 0, datetime('now'))
             ON CONFLICT(id) DO UPDATE SET stop = 1, updated_at = datetime('now')",
            &[],
        );
        return Ok(json!({ "ok": true, "stopped": true }));
    }

    let mut url = input.get("url").and_then(Value::as_str).unwrap_or("").to_string();
    if url.ends_with('/') { url.pop(); } // JS: .replace(/\/$/, '') strips exactly one trailing slash
    if url.is_empty() {
        return Err("ui-record: url is required".to_string());
    }
    let _origin = input.get("origin").and_then(Value::as_str).unwrap_or("atlas").to_string();
    let timeout_ms = (number_or(input.get("timeoutMs"), 600000.0).min(1800000.0)).max(10000.0);
    let _ = timeout_ms; // consumed only by the deferred browser poll loop below

    // PARITY-DEFERRED (see module doc): no playwright/CDP client is wired in this crate — the
    // browser session never launches. Same result JS's own `loadPlaywright()`-missing branch
    // returns.
    let result = json!({
        "ok": false,
        "error": "playwright not resolvable — install it in the engine or the FE",
    });

    // JS `finally { ... UPDATE code_record_control SET active = 0 ... }` — runs unconditionally,
    // including on this early no-playwright return.
    let _ = crate::db_json::run_write(
        db,
        "UPDATE code_record_control SET active = 0, updated_at = datetime('now') WHERE id = 1",
        &[],
    );

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_record_control (id INTEGER PRIMARY KEY CHECK (id = 1), stop INTEGER NOT NULL DEFAULT 0, active INTEGER NOT NULL DEFAULT 0, url TEXT, updated_at TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn stop_signal_upserts_control_row_and_short_circuits() {
        let c = db();
        let r = ui_record(&json!({ "stop": true }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "stopped": true }));
        let row: (i64, i64) = c
            .query_row("SELECT stop, active FROM code_record_control WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(row, (1, 0));
    }

    #[test]
    fn stop_signal_updates_existing_row_via_conflict() {
        let c = db();
        c.execute("INSERT INTO code_record_control (id, stop, active, url) VALUES (1, 0, 1, 'http://x')", []).unwrap();
        let r = ui_record(&json!({ "stop": 1 }), &c).unwrap();
        assert_eq!(r["stopped"], json!(true));
        let row: (i64, i64, String) = c
            .query_row("SELECT stop, active, url FROM code_record_control WHERE id = 1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert_eq!(row, (1, 1, "http://x".to_string())); // active untouched by the stop upsert; url preserved
    }

    #[test]
    fn requires_url_when_no_stop_signal() {
        let e = ui_record(&json!({}), &db()).unwrap_err();
        assert!(e.contains("url is required"));
    }

    #[test]
    fn requires_url_when_blank_string() {
        let e = ui_record(&json!({ "url": "" }), &db()).unwrap_err();
        assert!(e.contains("url is required"));
    }

    #[test]
    fn strips_exactly_one_trailing_slash_and_returns_no_playwright_shape() {
        let c = db();
        let r = ui_record(&json!({ "url": "http://localhost:3000/" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("playwright not resolvable"));
    }

    #[test]
    fn finally_clears_active_flag_even_on_no_playwright_return() {
        let c = db();
        c.execute("INSERT INTO code_record_control (id, stop, active, url) VALUES (1, 0, 1, 'http://x')", []).unwrap();
        ui_record(&json!({ "url": "http://localhost:3000" }), &c).unwrap();
        let active: i64 = c.query_row("SELECT active FROM code_record_control WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(active, 0);
    }

    #[test]
    fn timeout_ms_clamps_to_10s_1800s_with_600s_default() {
        assert_eq!(number_or(Some(&json!(1)), 600000.0).min(1800000.0).max(10000.0), 10000.0);
        assert_eq!(number_or(Some(&json!(9_999_999)), 600000.0).min(1800000.0).max(10000.0), 1800000.0);
        assert_eq!(number_or(None, 600000.0).min(1800000.0).max(10000.0), 600000.0);
        assert_eq!(number_or(Some(&json!(0)), 600000.0).min(1800000.0).max(10000.0), 600000.0); // 0 falsy -> default
    }

    #[test]
    fn stop_truthiness_matches_js() {
        assert!(!truthy(None));
        assert!(!truthy(Some(&json!(false))));
        assert!(!truthy(Some(&json!(0))));
        assert!(!truthy(Some(&json!(""))));
        assert!(truthy(Some(&json!(true))));
        assert!(truthy(Some(&json!(1))));
        assert!(truthy(Some(&json!("x"))));
    }
}
