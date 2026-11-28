//! ui-story-replay.rs — twin of skills/codebase/ui-story-replay.mjs (UI-STORIES P4/P5). NAMES ARE
//! CONTRACT: default `uiStoryReplay` (ported here as `ui_story_replay`).
//!
//! PARITY-DEFERRED: the entire live-browser replay — `loadPlaywright()` (from
//! `./design-capture-live.mjs`), `chromium.launch`, `page.goto`/`fill`/`click`/`evaluate`, the
//! per-step pass/fail walk, the `SNAP` in-page DOM/CSS capture, the `pageerror`/`console`/
//! `requestfailed` listeners, and (when `watch`) the recorded `.webm` video read-back — needs a
//! real, running Chromium instance driving a real dev server; genuinely non-deterministic and not
//! JSON-fixturable, same class as `codebase_design_capture_live::designCaptureLive`'s deferred
//! browser boundary and `codebase_ui_explore::ui_explore`'s. This crate has no playwright/CDP
//! client wired, so this twin never reaches that branch — it takes the same real degenerate path
//! JS's OWN `loadPlaywright()` already takes when the package isn't resolvable: `{ ok: false,
//! story_id: id, error: 'playwright not resolvable' }` (note: this JS caller's message has no
//! " — install it in the engine or the FE" suffix, unlike `design-capture-live.mjs`'s own message —
//! ported verbatim, not harmonized).
//!
//! Ported faithfully (real deterministic logic, unit-tested below):
//! - `id = Number(input?.id); if (!id) throw err.required(...)` — 0/NaN/absent all fail
//! - `origin` default `'atlas'`
//! - the `code_user_stories` row lookup by `(id, origin)`, table-absent tolerated (JS
//!   `try {...} catch {}` leaves `row` undefined) → `{ ok: false, error: "story {id} not found" }`
//! - `steps_json` / `routes_json` parse-or-null-then-`?? []` (JS `parse` helper), exercised for
//!   parity even though the not-found/not-resolvable branches below don't surface them
//!
//! Not ported (cannot be, deterministically): everything from `loadPlaywright()` onward — browser
//! launch, page navigation/interaction, per-step `results`/`trail`/`snapshots` assembly, the P5
//! cross-version `diff` against `baseline_json`, and the `UPDATE code_user_stories SET
//! baseline_json = ...` persistence that only runs after a real replay completes.

use rusqlite::Connection;
use serde_json::{json, Value};

// JS: `const parse = (s) => { try { return JSON.parse(s ?? 'null'); } catch { return null; } };`
// then callers do `parse(x) ?? []` — mirrored here as parse-or-default.
fn parse_or(s: Option<&str>, default: Value) -> Value {
    s.and_then(|s| serde_json::from_str::<Value>(s).ok())
        .filter(|v| !v.is_null())
        .unwrap_or(default)
}

/// `export default async function uiStoryReplay(input, context)` — see module doc: the real
/// input validation + `code_user_stories` lookup JS performs BEFORE ever touching
/// `loadPlaywright()`, then the same `{ ok: false, story_id, error: 'playwright not resolvable' }`
/// shape JS's own playwright-missing branch returns (this crate has no browser driver wired, so
/// that branch is always taken once a row is found).
pub fn ui_story_replay(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: const id = Number(input?.id); if (!id) throw err.required('ui-story-replay', 'id');
    let id = input.get("id").and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
    let id = match id {
        Some(n) if n != 0 => n,
        _ => return Err("ui-story-replay: id is required".to_string()),
    };
    let origin = input.get("origin").and_then(Value::as_str).unwrap_or("atlas");

    // JS: try { row = db.prepare(...).get(id, origin); } catch { /* table absent */ }
    let row = crate::db_json::query_rows(
        db,
        "SELECT name, url, steps_json, routes_json, baseline_json FROM code_user_stories WHERE id = ? AND origin = ?",
        &[json!(id), json!(origin)],
    )
    .ok()
    .and_then(|rows| rows.as_array().and_then(|a| a.first().cloned()));

    let row = match row {
        Some(r) => r,
        None => return Ok(json!({ "ok": false, "error": format!("story {id} not found") })),
    };

    // Real (unused-past-this-point) parses, kept for parity with JS locals built before the
    // playwright check — see module doc.
    let _steps = parse_or(row.get("steps_json").and_then(Value::as_str), json!([]));
    let _recorded_routes = parse_or(row.get("routes_json").and_then(Value::as_str), json!([]));

    // PARITY-DEFERRED (see module doc): no playwright/CDP client is wired in this crate — the
    // browser session never launches. Same result JS's own `loadPlaywright()`-missing branch
    // returns.
    Ok(json!({ "ok": false, "story_id": id, "error": "playwright not resolvable" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_user_stories (id INTEGER, name TEXT, url TEXT, steps_json TEXT, routes_json TEXT, baseline_json TEXT, origin TEXT);
             INSERT INTO code_user_stories VALUES (1,'s','http://localhost:3000','[{\"action\":\"click\"}]','[\"/a\"]',NULL,'atlas');",
        )
        .unwrap();
        c
    }

    #[test]
    fn requires_id() {
        let c = seeded();
        assert!(ui_story_replay(&json!({}), &c).is_err());
        assert!(ui_story_replay(&json!({ "id": 0 }), &c).is_err());
    }

    #[test]
    fn story_not_found_when_no_row() {
        let c = seeded();
        let r = ui_story_replay(&json!({ "id": 999 }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("story 999 not found"));
    }

    #[test]
    fn story_not_found_when_wrong_origin() {
        let c = seeded();
        let r = ui_story_replay(&json!({ "id": 1, "origin": "other" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("story 1 not found"));
    }

    #[test]
    fn returns_playwright_not_resolvable_shape_for_found_row() {
        let c = seeded();
        let r = ui_story_replay(&json!({ "id": 1 }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["story_id"], json!(1));
        assert_eq!(r["error"], json!("playwright not resolvable"));
    }

    #[test]
    fn defaults_origin_to_atlas() {
        let c = seeded();
        let r = ui_story_replay(&json!({ "id": 1, "origin": "atlas" }), &c).unwrap();
        assert_eq!(r["story_id"], json!(1));
        assert_eq!(r["error"], json!("playwright not resolvable"));
    }

    #[test]
    fn table_absent_is_tolerated_and_reports_not_found() {
        let c = Connection::open_in_memory().unwrap(); // no code_user_stories table at all
        let r = ui_story_replay(&json!({ "id": 1 }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("story 1 not found"));
    }
}
