//! context-bundle-persist.rs — twin of skills/codebase/context-bundle-persist.mjs (db WRITE lane,
//! attached `brain` db). NAMES ARE CONTRACT: `contextBundlePersist` (default). Best-effort persist
//! of a structured context bundle per ai_call (INSERT OR REPLACE, idempotent per ai_call_id); bad
//! id / bad bundle / absent table → { ok: true, persisted: false } — audit must never break the call.
use rusqlite::Connection;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
// JS Number(v): absent → NaN, null → 0, bool → 0/1, string → trimmed parse ('' → 0).
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

pub fn contextBundlePersist(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS throws err.contextMissing('context-bundle-persist','db') when no db — the db lane always
    // injects one, so that guard is unreachable here.
    let n = js_number(input.get("ai_call_id")); // Number(input?.ai_call_id)
    let b = input.get("bundle");
    // JS guard: !Number.isInteger(id) || id <= 0 || !b || typeof b !== 'object' → best-effort no-op.
    // truthy b of typeof 'object' = a non-null Object or Array (typeof null is 'object' but !b fires first).
    let b_ok = matches!(b, Some(Value::Object(_)) | Some(Value::Array(_)));
    if !(n.is_finite() && n.fract() == 0.0) || n <= 0.0 || !b_ok {
        return Ok(json!({ "ok": true, "persisted": false }));
    }
    let ai_call_id = n as i64;
    let b = b.unwrap();
    let g = |k: &str| b.get(k);
    let empty = json!([]);
    let items_json = serde_json::to_string(nz(g("items")).unwrap_or(&empty)).map_err(|e| e.to_string())?;
    let dropped_json = serde_json::to_string(nz(g("dropped")).unwrap_or(&empty)).map_err(|e| e.to_string())?;
    let skipped_json = serde_json::to_string(nz(g("skipped")).unwrap_or(&empty)).map_err(|e| e.to_string())?;
    let over_budget: i64 = if truthy(g("over_budget")) { 1 } else { 0 }; // JS `b.over_budget ? 1 : 0`
    let res = crate::db_json::run_write(
        db,
        "\n      INSERT OR REPLACE INTO brain.context_bundles\n        (ai_call_id, agent, phase, bone_tokens, sliced_tokens, total_tokens, budget, over_budget, items_json, dropped_json, skipped_json)\n      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)\n    ",
        &[
            json!(ai_call_id),
            nz(g("agent")).cloned().unwrap_or(Value::Null),
            nz(g("phase")).cloned().unwrap_or(Value::Null),
            nz(g("bone_tokens")).cloned().unwrap_or(json!(0)),
            nz(g("sliced_tokens")).cloned().unwrap_or(json!(0)),
            nz(g("total_tokens")).cloned().unwrap_or(json!(0)),
            nz(g("budget")).cloned().unwrap_or(Value::Null),
            json!(over_budget),
            json!(items_json),
            json!(dropped_json),
            json!(skipped_json),
        ],
    );
    // JS try/catch: table absent (legacy) / attach issue → never break the call.
    Ok(match res {
        Ok(_) => json!({ "ok": true, "persisted": true }),
        Err(_) => json!({ "ok": true, "persisted": false }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real brain.context_bundles shape (minus the ai_calls FK) with created_at DEFAULT frozen the
    // same way the fixture does.
    const SCHEMA: &str = "CREATE TABLE brain.context_bundles ( ai_call_id INTEGER PRIMARY KEY, agent TEXT, phase TEXT, bone_tokens INTEGER NOT NULL DEFAULT 0, sliced_tokens INTEGER NOT NULL DEFAULT 0, total_tokens INTEGER NOT NULL DEFAULT 0, budget INTEGER, over_budget INTEGER NOT NULL DEFAULT 0, items_json TEXT, dropped_json TEXT, skipped_json TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')) )";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }
    fn count(c: &Connection) -> i64 {
        c.query_row("SELECT COUNT(*) FROM brain.context_bundles", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn persists_the_full_bundle_row() {
        let c = db();
        let r = contextBundlePersist(
            &json!({ "ai_call_id": 4242, "bundle": {
                "agent": "pl", "phase": "research", "bone_tokens": 120, "sliced_tokens": 80,
                "total_tokens": 200, "budget": 6000, "over_budget": false,
                "items": [{ "id": "stack", "tier": "bone", "tokens": 120 }],
                "dropped": [{ "id": "x", "reason": "over token budget" }],
                "skipped": [{ "id": "diagram", "reason": "gatherer deferred" }] } }),
            &c,
        )
        .unwrap();
        assert_eq!(r, json!({ "ok": true, "persisted": true }));
        let row = crate::db_json::query_rows(&c, "SELECT * FROM brain.context_bundles WHERE ai_call_id = 4242", &[]).unwrap();
        assert_eq!(
            row.as_array().unwrap()[0],
            json!({ "ai_call_id": 4242, "agent": "pl", "phase": "research", "bone_tokens": 120,
                    "sliced_tokens": 80, "total_tokens": 200, "budget": 6000, "over_budget": 0,
                    "items_json": "[{\"id\":\"stack\",\"tier\":\"bone\",\"tokens\":120}]",
                    "dropped_json": "[{\"id\":\"x\",\"reason\":\"over token budget\"}]",
                    "skipped_json": "[{\"id\":\"diagram\",\"reason\":\"gatherer deferred\"}]",
                    "created_at": "2026-01-01 00:00:00" })
        );
    }

    #[test]
    fn empty_bundle_lands_on_defaults_and_over_budget_is_ternaried() {
        let c = db();
        assert_eq!(contextBundlePersist(&json!({ "ai_call_id": 9, "bundle": {} }), &c).unwrap(), json!({ "ok": true, "persisted": true }));
        let rows = crate::db_json::query_rows(&c, "SELECT agent, phase, bone_tokens, sliced_tokens, total_tokens, budget, over_budget, items_json FROM brain.context_bundles WHERE ai_call_id = 9", &[]).unwrap();
        assert_eq!(rows[0], json!({ "agent": null, "phase": null, "bone_tokens": 0, "sliced_tokens": 0, "total_tokens": 0, "budget": null, "over_budget": 0, "items_json": "[]" }));
        // truthy over_budget → 1; an array bundle passes the typeof-object guard (all defaults)
        contextBundlePersist(&json!({ "ai_call_id": 10, "bundle": { "over_budget": true } }), &c).unwrap();
        let ob: i64 = c.query_row("SELECT over_budget FROM brain.context_bundles WHERE ai_call_id = 10", [], |r| r.get(0)).unwrap();
        assert_eq!(ob, 1);
        assert_eq!(contextBundlePersist(&json!({ "ai_call_id": 11, "bundle": [] }), &c).unwrap(), json!({ "ok": true, "persisted": true }));
        assert_eq!(count(&c), 3);
    }

    #[test]
    fn insert_or_replace_is_idempotent_per_ai_call_id() {
        let c = db();
        contextBundlePersist(&json!({ "ai_call_id": 7, "bundle": { "total_tokens": 111 } }), &c).unwrap();
        contextBundlePersist(&json!({ "ai_call_id": 7, "bundle": { "total_tokens": 222 } }), &c).unwrap();
        let t: i64 = c.query_row("SELECT total_tokens FROM brain.context_bundles WHERE ai_call_id = 7", [], |r| r.get(0)).unwrap();
        assert_eq!(t, 222);
        assert_eq!(count(&c), 1);
    }

    #[test]
    fn string_integer_id_persists_like_js_number_coercion() {
        let c = db();
        assert_eq!(contextBundlePersist(&json!({ "ai_call_id": "42", "bundle": { "agent": "dev" } }), &c).unwrap(), json!({ "ok": true, "persisted": true }));
        let id: i64 = c.query_row("SELECT ai_call_id FROM brain.context_bundles", [], |r| r.get(0)).unwrap();
        assert_eq!(id, 42);
    }

    #[test]
    fn guards_bad_id_or_bad_bundle_without_writing() {
        let c = db();
        let no = json!({ "ok": true, "persisted": false });
        assert_eq!(contextBundlePersist(&json!({ "ai_call_id": 0, "bundle": {} }), &c).unwrap(), no); // <= 0
        assert_eq!(contextBundlePersist(&json!({ "ai_call_id": -3, "bundle": {} }), &c).unwrap(), no);
        assert_eq!(contextBundlePersist(&json!({ "ai_call_id": 4.5, "bundle": {} }), &c).unwrap(), no); // not integer
        assert_eq!(contextBundlePersist(&json!({ "bundle": {} }), &c).unwrap(), no); // Number(undefined) → NaN
        assert_eq!(contextBundlePersist(&json!({ "ai_call_id": null, "bundle": {} }), &c).unwrap(), no); // Number(null) → 0
        assert_eq!(contextBundlePersist(&json!({ "ai_call_id": 5, "bundle": null }), &c).unwrap(), no); // !b
        assert_eq!(contextBundlePersist(&json!({ "ai_call_id": 5 }), &c).unwrap(), no); // b undefined
        assert_eq!(contextBundlePersist(&json!({ "ai_call_id": 5, "bundle": "nope" }), &c).unwrap(), no); // typeof string
        assert_eq!(contextBundlePersist(&json!({ "ai_call_id": 5, "bundle": 12 }), &c).unwrap(), no); // typeof number
        assert_eq!(count(&c), 0);
    }

    #[test]
    fn absent_table_is_swallowed_never_breaks_the_call() {
        let bare = Connection::open_in_memory().unwrap(); // no brain attached → run errs → catch path
        assert_eq!(
            contextBundlePersist(&json!({ "ai_call_id": 1, "bundle": { "agent": "pl" } }), &bare).unwrap(),
            json!({ "ok": true, "persisted": false })
        );
    }
}
