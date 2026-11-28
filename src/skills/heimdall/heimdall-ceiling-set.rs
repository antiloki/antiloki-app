//! heimdall-ceiling-set.rs — twin of skills/heimdall/heimdall-ceiling-set.mjs (db lane,
//! cross-db). NAMES ARE CONTRACT: `heimdallCeilingSet` (default). Sets/clears a GATE's daily
//! AI spend ceiling in brain.heimdall_gate_ceilings (template_base PRIMARY KEY): clearing
//! (ceiling_usd null/''/0) DELETEs the row (absent row = unlimited); a positive finite value
//! upserts it. FROZEN-CLOCK LANE: the upsert's `updated_at` column uses `datetime('now')` in
//! JS — pinned here to the frozen instant 2026-01-01 00:00:00 (playbook frozen-clock rule).
// QUALITY: 74/100 — thin upsert/clear gate over the already-converted casting twin's templateBase

use rusqlite::Connection;
use serde_json::{json, Value};

// Standard helper functions (playbook §Standard helper functions) — copied verbatim (subset used).
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}
// JS `Number(v)` re-rendered through JSON.stringify: an integral result prints without a
// trailing ".0" (JS numbers don't distinguish int/float); a genuinely fractional one keeps it.
fn num_json(f: f64) -> Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15 { json!(f as i64) } else { json!(f) }
}

pub fn heimdallCeilingSet(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing(...)` — the db lane
    // always injects a db; no Rust counterpart (guard-throw, not fixtured — playbook gotcha 9).
    let raw = input.get("template_base");
    // JS `if (!raw) throw err.required(...)` — falsy raw (absent/null/''/0/false) is a
    // guard-throw, not exercised by fixtures (playbook gotcha 9: success path only).
    let raw_truthy = match raw {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    };
    if !raw_truthy {
        return Err("heimdall-ceiling-set: template_base is required".to_string());
    }
    let base_v = crate::heimdall__helpers_casting::templateBase(raw.unwrap());
    let base = base_v.as_str().unwrap_or("").to_string();

    let v = input.get("ceiling_usd");
    let v_is_null = matches!(v, None | Some(Value::Null));
    let v_is_empty_string = matches!(v, Some(Value::String(s)) if s.is_empty());
    let num = js_number(v);
    // JS: `v == null || v === '' || Number(v) === 0` — NaN never equals 0, matching Rust's
    // NaN != 0.0 comparison semantics for free.
    if v_is_null || v_is_empty_string || num == 0.0 {
        let _ = db.execute(
            "DELETE FROM brain.heimdall_gate_ceilings WHERE template_base = ?",
            rusqlite::params![base],
        );
        return Ok(json!({ "ok": true, "template_base": base, "ceiling_usd": null, "cleared": true }));
    }
    let ceiling = num;
    // JS: `if (!Number.isFinite(ceiling) || ceiling <= 0) throw err.range(...)` — guard-throw,
    // not fixtured (playbook gotcha 9: only the success path is exercised).
    if !ceiling.is_finite() || ceiling <= 0.0 {
        return Err("heimdall-ceiling-set: ceiling_usd out of range".to_string());
    }
    let _ = db.execute(
        &format!("INSERT INTO brain.heimdall_gate_ceilings (template_base, ceiling_usd, updated_at)
         VALUES (?, ?, {})
         ON CONFLICT(template_base) DO UPDATE SET ceiling_usd = excluded.ceiling_usd, updated_at = {}", crate::clock::now_sql(), crate::clock::now_sql()),
        rusqlite::params![base, ceiling],
    );
    Ok(json!({ "ok": true, "template_base": base, "ceiling_usd": num_json(ceiling), "cleared": false }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_brain() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain;
             CREATE TABLE brain.heimdall_gate_ceilings (
               template_base TEXT PRIMARY KEY,
               ceiling_usd REAL NOT NULL CHECK (ceiling_usd > 0),
               updated_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00'))
             );",
        ).unwrap();
        c
    }

    #[test]
    fn sets_by_versioned_name_then_upserts_by_base() {
        let c = db_with_brain();
        let r1 = heimdallCeilingSet(&json!({ "template_base": "blueprint.stub.v1", "ceiling_usd": 0.5 }), &c).unwrap();
        assert_eq!(r1, json!({ "ok": true, "template_base": "blueprint.stub", "ceiling_usd": 0.5, "cleared": false }));
        let r2 = heimdallCeilingSet(&json!({ "template_base": "blueprint.stub", "ceiling_usd": 1.25 }), &c).unwrap();
        assert_eq!(r2, json!({ "ok": true, "template_base": "blueprint.stub", "ceiling_usd": 1.25, "cleared": false }));
        let rows = crate::db_json::query_rows(&c, "SELECT COUNT(*) n FROM brain.heimdall_gate_ceilings", &[]).unwrap();
        assert_eq!(rows[0]["n"], json!(1));
        let row = crate::db_json::query_rows(&c, "SELECT ceiling_usd, updated_at FROM brain.heimdall_gate_ceilings WHERE template_base = 'blueprint.stub'", &[]).unwrap();
        assert_eq!(row[0]["ceiling_usd"], json!(1.25));
        assert_eq!(row[0]["updated_at"], json!("2026-01-01 00:00:00"));
    }

    #[test]
    fn integral_ceiling_renders_without_decimal() {
        let c = db_with_brain();
        let r = heimdallCeilingSet(&json!({ "template_base": "chat.v3", "ceiling_usd": 5 }), &c).unwrap();
        assert_eq!(r["ceiling_usd"], json!(5));
    }

    #[test]
    fn clears_on_null_empty_string_and_zero() {
        let c = db_with_brain();
        heimdallCeilingSet(&json!({ "template_base": "chat.v1", "ceiling_usd": 2.0 }), &c).unwrap();
        let r_null = heimdallCeilingSet(&json!({ "template_base": "chat.v1", "ceiling_usd": null }), &c).unwrap();
        assert_eq!(r_null, json!({ "ok": true, "template_base": "chat", "ceiling_usd": null, "cleared": true }));
        let count_after_null = crate::db_json::query_rows(&c, "SELECT COUNT(*) n FROM brain.heimdall_gate_ceilings", &[]).unwrap();
        assert_eq!(count_after_null[0]["n"], json!(0));

        heimdallCeilingSet(&json!({ "template_base": "chat.v1", "ceiling_usd": 2.0 }), &c).unwrap();
        let r_empty = heimdallCeilingSet(&json!({ "template_base": "chat.v1", "ceiling_usd": "" }), &c).unwrap();
        assert_eq!(r_empty["cleared"], json!(true));

        heimdallCeilingSet(&json!({ "template_base": "chat.v1", "ceiling_usd": 2.0 }), &c).unwrap();
        let r_zero = heimdallCeilingSet(&json!({ "template_base": "chat.v1", "ceiling_usd": 0 }), &c).unwrap();
        assert_eq!(r_zero["cleared"], json!(true));

        // absent key also clears (v == undefined matches JS `v == null`)
        heimdallCeilingSet(&json!({ "template_base": "chat.v1", "ceiling_usd": 2.0 }), &c).unwrap();
        let r_absent = heimdallCeilingSet(&json!({ "template_base": "chat.v1" }), &c).unwrap();
        assert_eq!(r_absent["cleared"], json!(true));
    }

    #[test]
    fn missing_template_base_is_an_error() {
        let c = db_with_brain();
        assert!(heimdallCeilingSet(&json!({ "ceiling_usd": 1.0 }), &c).is_err());
    }

    #[test]
    fn negative_ceiling_is_an_error() {
        let c = db_with_brain();
        assert!(heimdallCeilingSet(&json!({ "template_base": "x", "ceiling_usd": -1 }), &c).is_err());
    }
}
