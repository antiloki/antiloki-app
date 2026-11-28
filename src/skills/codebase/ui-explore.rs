//! ui-explore.rs — twin of skills/codebase/ui-explore.mjs (UI-STORIES P0). NAMES ARE CONTRACT:
//! default `uiExplore` (ported here as `ui_explore`).
//!
//! PARITY-DEFERRED: the ENTIRE body of JS `uiExplore` past input validation is a live
//! playwright/chromium browser session — `loadPlaywright()` (from `./design-capture-live.mjs`),
//! `chromium.launch`, `page.goto`, injecting the History-API `RECORDER` init script, then
//! `page.evaluate(buildDrive(...))` to enumerate + interact with every entry point in the live DOM
//! and record navigations/console-errors/failed-requests. None of that is JSON-fixturable — same
//! class as `codebase_design_capture_live::designCaptureLive`'s deferred browser boundary (see that
//! module's doc) and `external/*-call.rs`'s provider network hop. This crate has no
//! playwright/CDP client wired, so `ui_explore` below never reaches that branch — it takes the same
//! real degenerate path JS's OWN `loadPlaywright()` already takes when the package isn't
//! resolvable: `{ ok: false, url, routes: [], entryPoints: [], counts: null, errors: [], error:
//! 'playwright not resolvable — install it in the engine or the FE' }`, a returned RESULT (JS never
//! throws for this branch — only a missing `url` throws, via `err.required`).
//!
//! Ported faithfully (real deterministic logic, unit-tested below):
//! - `url` required guard (`err.required('ui-explore', 'url')`) + trailing-slash strip
//!   (`.replace(/\/$/, '')` — exactly one trailing slash, matching JS regex semantics)
//! - `safeMode` default-on (`input?.safeMode !== false`)
//! - `fillValue` default `'test'` (only a JS `typeof === 'string'` input is honored)
//! - `maxClicks` clamp: `Math.max(1, Math.min(Number(input?.maxClicks) || 40, 200))`
//! - `perClickMs` clamp: `Math.max(50, Math.min(Number(input?.perClickMs) || 220, 2000))`
//! - the no-playwright result shape, including the fixed `error` message text
//!
//! Not ported (cannot be, deterministically): `RECORDER` / `buildDrive` in-page JS strings (they
//! exist only to be `page.evaluate`d inside a real browser — no interpreter for them runs here),
//! the `page.on(...)` console/pageerror/requestfailed/response error-collection wiring, route
//! recording/dedup, entry-point enumeration/interaction, and the `finally { browser?.close() }`
//! cleanup — all of it lives strictly inside the deferred browser session this twin never opens.

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

/// `export default async function uiExplore(input, context)` — see module doc: the real input
/// validation/normalization JS performs BEFORE ever touching `loadPlaywright()`, then the same
/// `{ ok: false, ..., error: 'playwright not resolvable ...' }` shape JS's own playwright-missing
/// branch returns (this crate has no browser driver wired, so that branch is always taken).
pub fn ui_explore(input: &Value) -> Result<Value, String> {
    let mut url = input.get("url").and_then(Value::as_str).unwrap_or("").to_string();
    if url.ends_with('/') { url.pop(); } // JS: .replace(/\/$/, '') strips exactly one trailing slash
    if url.is_empty() {
        return Err("ui-explore: url is required".to_string());
    }
    let safe_mode = !matches!(input.get("safeMode"), Some(Value::Bool(false))); // default ON
    let _fill_value = input.get("fillValue").and_then(Value::as_str).unwrap_or("test").to_string();
    let max_clicks = (number_or(input.get("maxClicks"), 40.0).min(200.0)).max(1.0);
    let per_click_ms = (number_or(input.get("perClickMs"), 220.0).min(2000.0)).max(50.0);
    let _ = (safe_mode, max_clicks, per_click_ms); // consumed only by the deferred browser session below

    // PARITY-DEFERRED (see module doc): no playwright/CDP client is wired in this crate — the
    // browser session never launches. Same result JS's own `loadPlaywright()`-missing branch
    // returns.
    Ok(json!({
        "ok": false,
        "url": url,
        "routes": [],
        "entryPoints": [],
        "counts": null,
        "errors": [],
        "error": "playwright not resolvable — install it in the engine or the FE",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_url() {
        let e = ui_explore(&json!({})).unwrap_err();
        assert!(e.contains("url is required"));
    }

    #[test]
    fn requires_url_when_blank_string() {
        let e = ui_explore(&json!({ "url": "" })).unwrap_err();
        assert!(e.contains("url is required"));
    }

    #[test]
    fn strips_exactly_one_trailing_slash() {
        let r = ui_explore(&json!({ "url": "http://localhost:3000/" })).unwrap();
        assert_eq!(r["url"], json!("http://localhost:3000"));
        let r2 = ui_explore(&json!({ "url": "http://localhost:3000//" })).unwrap();
        assert_eq!(r2["url"], json!("http://localhost:3000/")); // only ONE trailing slash stripped
    }

    #[test]
    fn returns_playwright_not_resolvable_shape() {
        let r = ui_explore(&json!({ "url": "http://localhost:3000" })).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["routes"], json!([]));
        assert_eq!(r["entryPoints"], json!([]));
        assert_eq!(r["counts"], json!(null));
        assert_eq!(r["errors"], json!([]));
        assert!(r["error"].as_str().unwrap().contains("playwright not resolvable"));
    }

    #[test]
    fn max_clicks_clamps_to_1_200_with_40_default() {
        assert_eq!(number_or(Some(&json!(500)), 40.0).min(200.0).max(1.0), 200.0);
        assert_eq!(number_or(Some(&json!(0)), 40.0).min(200.0).max(1.0), 40.0); // 0 is falsy -> default
        assert_eq!(number_or(None, 40.0).min(200.0).max(1.0), 40.0);
        assert_eq!(number_or(Some(&json!(-5)), 40.0).min(200.0).max(1.0), 1.0);
        assert_eq!(number_or(Some(&json!(75)), 40.0).min(200.0).max(1.0), 75.0);
    }

    #[test]
    fn per_click_ms_clamps_to_50_2000_with_220_default() {
        assert_eq!(number_or(Some(&json!(9999)), 220.0).min(2000.0).max(50.0), 2000.0);
        assert_eq!(number_or(Some(&json!(1)), 220.0).min(2000.0).max(50.0), 50.0);
        assert_eq!(number_or(None, 220.0).min(2000.0).max(50.0), 220.0);
    }

    #[test]
    fn safe_mode_defaults_on_and_honors_explicit_false() {
        assert!(!matches!(json!({}).get("safeMode"), Some(Value::Bool(false))));
        assert!(matches!(json!({ "safeMode": false }).get("safeMode"), Some(Value::Bool(false))));
    }
}
