//! ai-telemetry.rs — twin of skills/ai/_helpers/ai-telemetry.mjs (helper module). NAMES ARE CONTRACT:
//! `telemetryFromResult`. Builds a uniform per-call telemetry record from an aiCall result; tokens come
//! from the persisted brain.ai_calls row when the result carries an ai_call_id.
//!
//! Signature note: the JS is `telemetryFromResult(res, db, phase, promptName)` — db sits SECOND, a shape
//! the harness sig lanes can't spread. Conform rides `sig:"db-last"` with `args:[res]` only, which in JS
//! is the correct partial application `fn(res, db)` (phase/promptName = undefined → keys dropped on the
//! wire). The twin keeps the full 4-param contract: `Option<&Value>` = JS undefined (None omits the key).
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null()) // JS `?? / != null` nullish test
}

pub fn telemetryFromResult(res: &Value, db: &Connection, phase: Option<&Value>, prompt_name: Option<&Value>) -> Value {
    let mut tokens_in = Value::Null;
    let mut tokens_out = Value::Null;
    // JS: if (res?.ai_call_id != null && db) — loose != null is exactly the nullish test
    if let Some(id) = nz(res.get("ai_call_id")) {
        // JS try/catch: a failed prepare (brain not attached / table missing) leaves tokens null
        if let Ok(rows) = crate::db_json::query_rows(db, "SELECT tokens_in, tokens_out FROM brain.ai_calls WHERE id = ?", &[id.clone()]) {
            if let Some(r) = rows.as_array().and_then(|a| a.first()) {
                tokens_in = nz(r.get("tokens_in")).cloned().unwrap_or(Value::Null); // r?.tokens_in ?? null
                tokens_out = nz(r.get("tokens_out")).cloned().unwrap_or(Value::Null);
            } // no row → r undefined → tokens stay null
        }
    }
    let mut out = Map::new();
    if let Some(p) = phase { out.insert("phase".into(), p.clone()); } // None = JS undefined → key vanishes on the wire
    if let Some(p) = prompt_name { out.insert("prompt_name".into(), p.clone()); }
    out.insert("model_id".into(), nz(res.get("model_id")).cloned().unwrap_or(Value::Null));
    out.insert("provider".into(), nz(res.get("provider")).cloned().unwrap_or(Value::Null));
    out.insert("tier".into(), nz(res.get("tier_used")).cloned().unwrap_or(Value::Null));
    out.insert("temperature".into(), nz(res.get("temperature_used")).cloned().unwrap_or(Value::Null));
    out.insert("tokens_in".into(), tokens_in);
    out.insert("tokens_out".into(), tokens_out);
    out.insert("cost_usd".into(), nz(res.get("cost_usd")).cloned().unwrap_or(json!(0)));
    out.insert("cached".into(), nz(res.get("cached")).cloned().unwrap_or(json!(false)));
    out.insert("duration_ms".into(), nz(res.get("duration_ms")).cloned().unwrap_or(Value::Null));
    out.insert("ai_call_id".into(), nz(res.get("ai_call_id")).cloned().unwrap_or(Value::Null));
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brain_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain;\n             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, tokens_in INTEGER, tokens_out INTEGER);",
        )
        .unwrap();
        c
    }

    #[test]
    fn fills_tokens_from_brain_row_and_keeps_key_order() {
        let c = brain_db();
        c.execute("INSERT INTO brain.ai_calls (id, tokens_in, tokens_out) VALUES (7, 100, 42)", []).unwrap();
        let res = json!({ "ai_call_id": 7, "model_id": "m-alpha", "provider": "prov-a", "tier_used": "mid", "temperature_used": "cold", "cost_usd": 0.0123, "cached": true, "duration_ms": 512 });
        let t = telemetryFromResult(&res, &c, Some(&json!("code-sample")), Some(&json!("atlas.code.sample.v4")));
        assert_eq!(
            serde_json::to_string(&t).unwrap(),
            r#"{"phase":"code-sample","prompt_name":"atlas.code.sample.v4","model_id":"m-alpha","provider":"prov-a","tier":"mid","temperature":"cold","tokens_in":100,"tokens_out":42,"cost_usd":0.0123,"cached":true,"duration_ms":512,"ai_call_id":7}"#
        );
    }

    #[test]
    fn row_miss_null_res_and_undefined_phase() {
        let c = brain_db();
        let miss = telemetryFromResult(&json!({ "ai_call_id": 99 }), &c, Some(&json!("p")), None);
        assert_eq!(miss["tokens_in"], Value::Null);
        assert_eq!(miss["tokens_out"], Value::Null);
        assert_eq!(miss["ai_call_id"], json!(99));
        assert!(miss.get("prompt_name").is_none()); // undefined promptName → key omitted

        let empty = telemetryFromResult(&Value::Null, &c, None, None);
        assert!(empty.get("phase").is_none());
        assert_eq!(
            serde_json::to_string(&empty).unwrap(),
            r#"{"model_id":null,"provider":null,"tier":null,"temperature":null,"tokens_in":null,"tokens_out":null,"cost_usd":0,"cached":false,"duration_ms":null,"ai_call_id":null}"#
        );
    }

    #[test]
    fn missing_brain_table_is_nonfatal_and_zero_id_reads() {
        let bare = Connection::open_in_memory().unwrap(); // no brain attached → query errs → catch path
        let t = telemetryFromResult(&json!({ "ai_call_id": 1, "cost_usd": 0 }), &bare, None, None);
        assert_eq!(t["tokens_in"], Value::Null);
        assert_eq!(t["cost_usd"], json!(0));
        assert_eq!(t["ai_call_id"], json!(1));

        let c = brain_db(); // ai_call_id = 0 is NOT nullish (0 != null) → query runs
        c.execute("INSERT INTO brain.ai_calls (id, tokens_in, tokens_out) VALUES (0, 11, 22)", []).unwrap();
        let z = telemetryFromResult(&json!({ "ai_call_id": 0 }), &c, None, None);
        assert_eq!(z["tokens_in"], json!(11));
        assert_eq!(z["tokens_out"], json!(22));
        assert_eq!(z["ai_call_id"], json!(0));
    }
}
