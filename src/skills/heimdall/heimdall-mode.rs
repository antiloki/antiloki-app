//! heimdall-mode.rs — twin of skills/heimdall/heimdall-mode.mjs (db lane, cross-db). NAMES ARE
//! CONTRACT: `heimdallMode` (default). The PER-SURFACE routing flag: 'train' keeps gathering
//! evidence (rotation runs regardless of the global cooking switch), 'rank1' pins the ledger's
//! top sample-ok model for the template, absent leaves the global cooking-mode switch in charge.
//! Pure orchestration: op dispatch + template_base normalization over the already-converted
//! casting twin (heimdall__helpers_casting), which owns the storage + ladder-walk logic.
// QUALITY: 82/100 — op-routed dispatch (list/set/get-default) delegating to the casting helper twin

use rusqlite::Connection;
use serde_json::{json, Map, Value};

// Standard helper functions (playbook §Standard helper functions) — copied verbatim (subset used).
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
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
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

pub fn heimdallMode(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const db = context?.db; if (!db) throw err.required('heimdall-mode','context.db')` —
    // a guard-throw the db lane never exercises (it always injects a db); no Rust counterpart.

    // `String(input?.op ?? 'get')` — nz(...)+js_string covers the `?? 'get'` non-empty default
    // (str_qq only covers `?? ''`).
    let op = nz(input.get("op")).map(js_string).unwrap_or_else(|| "get".to_string());

    if op == "list" {
        crate::heimdall__helpers_casting::ensureHeimdallModes(db);
        let rows = crate::db_json::query_rows(
            db,
            "SELECT template_base, mode, updated_at FROM brain.heimdall_modes ORDER BY template_base",
            &[],
        ).unwrap_or_else(|_| json!([]));
        return Ok(json!({ "ok": true, "rows": rows }));
    }

    // `templateBase(String(input?.template_base ?? '').trim())` — str_qq covers `String(x ?? '')`.
    let trimmed = str_qq(input.get("template_base")).trim().to_string();
    let base = crate::heimdall__helpers_casting::templateBase(&json!(trimmed));
    let base_str = base.as_str().unwrap_or("").to_string();
    // JS throws err.required('heimdall-mode','template_base') when base is falsy — a guard-throw,
    // not exercised by fixtures (only success paths are — playbook gotcha 9).
    if base_str.is_empty() {
        return Err("heimdall-mode: template_base is required".to_string());
    }

    if op == "set" {
        let mode_v = nz(input.get("mode")).cloned().unwrap_or(Value::Null);
        let r = crate::heimdall__helpers_casting::setHeimdallMode(db, &base, &mode_v);
        let rank1_model = crate::heimdall__helpers_casting::heimdallRank1(db, &base);
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        if let Value::Object(rm) = &r {
            for (k, v) in rm { out.insert(k.clone(), v.clone()); } // spreads {template_base, mode}
        }
        out.insert("rank1_model".into(), rank1_model);
        return Ok(Value::Object(out));
    }

    // Any op other than 'list'/'set' (including the default 'get') falls through here — the JS
    // source has no explicit `op === 'get'` branch either.
    Ok(json!({
        "ok": true,
        "template_base": base_str,
        "mode": crate::heimdall__helpers_casting::heimdallModeFor(db, &base),
        "rank1_model": crate::heimdall__helpers_casting::heimdallRank1(db, &base),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_ledger(db: &Connection) {
        db.execute_batch("
            CREATE TABLE IF NOT EXISTS brain.heimdall_template_categories (template_base TEXT PRIMARY KEY, category TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS brain.heimdall_categories (slug TEXT PRIMARY KEY, family TEXT NOT NULL, weight_profile TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS brain.heimdall_ledger (model_slug TEXT NOT NULL, scope_kind TEXT NOT NULL, scope_ref TEXT NOT NULL, window_days INTEGER NOT NULL, calls INTEGER NOT NULL DEFAULT 0, clean INTEGER NOT NULL DEFAULT 0, fails INTEGER NOT NULL DEFAULT 0, burns INTEGER NOT NULL DEFAULT 0, ships INTEGER NOT NULL DEFAULT 0, weighted_score REAL, sample_ok INTEGER NOT NULL DEFAULT 0);
            INSERT INTO brain.heimdall_template_categories (template_base, category) VALUES ('chat', 'conversational');
            INSERT INTO brain.heimdall_categories (slug, family, weight_profile) VALUES ('conversational', 'LANGUAGE', 'TALKER');
            INSERT INTO brain.heimdall_ledger VALUES ('claude-sonnet-4-6','template','chat',30,50,40,5,2,3,0.82,1);
        ").unwrap();
    }

    fn db_with_brain() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c
    }

    #[test]
    fn list_on_fresh_table_is_empty() {
        let c = db_with_brain();
        assert_eq!(heimdallMode(&json!({ "op": "list" }), &c).unwrap(), json!({ "ok": true, "rows": [] }));
    }

    #[test]
    fn set_train_then_rank1_resolves_from_ledger() {
        let c = db_with_brain();
        seed_ledger(&c);
        let r = heimdallMode(&json!({ "op": "set", "template_base": "chat.v11", "mode": "train" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "template_base": "chat", "mode": "train", "rank1_model": "claude-sonnet-4-6" }));
    }

    #[test]
    fn set_rank1_with_cold_ledger_is_null() {
        let c = db_with_brain();
        let r = heimdallMode(&json!({ "op": "set", "template_base": "summarize.v3", "mode": "rank1" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "template_base": "summarize", "mode": "rank1", "rank1_model": Value::Null }));
    }

    #[test]
    fn set_clears_via_null_and_default() {
        let c = db_with_brain();
        let r1 = heimdallMode(&json!({ "op": "set", "template_base": "chat.v11", "mode": Value::Null }), &c).unwrap();
        assert_eq!(r1["mode"], Value::Null);
        assert_eq!(r1["template_base"], json!("chat"));
        let r2 = heimdallMode(&json!({ "op": "set", "template_base": "chat.v11", "mode": "default" }), &c).unwrap();
        assert_eq!(r2["mode"], Value::Null);
    }

    #[test]
    fn get_explicit_reads_cold_mode_and_resolved_rank1() {
        let c = db_with_brain();
        seed_ledger(&c);
        let r = heimdallMode(&json!({ "op": "get", "template_base": "chat.v11" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "template_base": "chat", "mode": Value::Null, "rank1_model": "claude-sonnet-4-6" }));
    }

    #[test]
    fn default_op_trims_and_strips_version() {
        let c = db_with_brain();
        let r = heimdallMode(&json!({ "template_base": "  billing.v2  " }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "template_base": "billing", "mode": Value::Null, "rank1_model": Value::Null }));
    }

    #[test]
    fn unrecognized_op_falls_through_to_get() {
        let c = db_with_brain();
        let r = heimdallMode(&json!({ "op": "frobnicate", "template_base": "faq" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "template_base": "faq", "mode": Value::Null, "rank1_model": Value::Null }));
    }
}
