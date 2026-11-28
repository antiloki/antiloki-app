//! law-hook-no-direct-audit-write.rs — twin of skills/audit/law-hook-no-direct-audit-write.mjs
//! (no-db, pure — env-gated). NAMES ARE CONTRACT: `lawHookNoDirectAuditWrite` (default).
//! Refuses any action whose NAME signals audit-bypass intent, or whose input_data carries an
//! audit-internal field. Escape hatch: ANTILOKI_ALLOW_RAW_AUDIT=true. Returns { allowed, ... }.
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

// Action name tokens that signal audit-bypass intent (order = first-match order, as in JS).
const SUSPECT_NAME_TOKENS: [&str; 10] = [
    "force-qa",
    "fake-audit",
    "forge-print",
    "raw-audit",
    "falsify",
    "bypass-audit",
    "manual-pass",
    "manual-qa",
    "direct-timeline",
    "forge-digital-print",
];

// Field names that, in input_data, suggest a forged audit row is being injected directly.
const SUSPECT_INPUT_KEYS: [&str; 4] =
    ["digital_print", "prev_print", "sacred_timeline_id", "forge_print"];

pub fn lawHookNoDirectAuditWrite(input: &Value, _db: &Connection) -> Result<Value, String> {
    // const { action_name, input_data } = input ?? {};
    let action_name = input.get("action_name");
    if !truthy(action_name) {
        return Ok(json!({ "allowed": true }));
    }

    if std::env::var("ANTILOKI_ALLOW_RAW_AUDIT").ok().as_deref() == Some("true") {
        return Ok(json!({ "allowed": true, "escape_hatch": "ANTILOKI_ALLOW_RAW_AUDIT=true" }));
    }

    let lowered = action_name.and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
    for tok in SUSPECT_NAME_TOKENS {
        if lowered.contains(tok) {
            return Ok(json!({
                "allowed": false,
                "reason": format!("action name contains audit-bypass token '{tok}'"),
                "suggested_path": "route this mutation through with-audit; never compose audit-falsification actions",
            }));
        }
    }

    // if (input_data && typeof input_data === 'object') — only Object carries matching keys;
    // arrays (also typeof 'object') stringify their indices, which never hit the suspect set.
    let input_data = input.get("input_data");
    if truthy(input_data) {
        if let Some(Value::Object(obj)) = input_data {
            for k in obj.keys() {
                if SUSPECT_INPUT_KEYS.contains(&k.as_str()) {
                    return Ok(json!({
                        "allowed": false,
                        "reason": format!("input contains audit-internal field '{k}' — these are populated by with-audit, not by callers"),
                        "suggested_path": "remove the field; let with-audit compute the digital_print chain",
                    }));
                }
            }
        }
    }

    Ok(json!({ "allowed": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Value {
        lawHookNoDirectAuditWrite(&i, &Connection::open_in_memory().unwrap()).unwrap()
    }

    #[test]
    fn gates() {
        // no action_name → allowed
        assert_eq!(call(json!({})), json!({ "allowed": true }));
        assert_eq!(call(json!({ "action_name": "" })), json!({ "allowed": true }));
        // clean action → allowed
        assert_eq!(call(json!({ "action_name": "board-keeper.card.add" })), json!({ "allowed": true }));

        // name-token block (first-match + interpolated token)
        let f = call(json!({ "action_name": "force-qa-pass" }));
        assert_eq!(f["allowed"], json!(false));
        assert_eq!(f["reason"], json!("action name contains audit-bypass token 'force-qa'"));
        assert_eq!(f["suggested_path"], json!("route this mutation through with-audit; never compose audit-falsification actions"));
        // case-insensitive (toLowerCase)
        assert_eq!(call(json!({ "action_name": "FALSIFY-STATE" }))["reason"],
            json!("action name contains audit-bypass token 'falsify'"));
        // forge-digital-print is NOT matched early by 'forge-print'
        assert_eq!(call(json!({ "action_name": "x.forge-digital-print" }))["reason"],
            json!("action name contains audit-bypass token 'forge-digital-print'"));

        // input_data suspect key
        let d = call(json!({ "action_name": "clean.action", "input_data": { "digital_print": "abc" } }));
        assert_eq!(d["allowed"], json!(false));
        assert_eq!(d["reason"], json!("input contains audit-internal field 'digital_print' — these are populated by with-audit, not by callers"));
        assert_eq!(d["suggested_path"], json!("remove the field; let with-audit compute the digital_print chain"));
        // first suspect key in insertion order wins
        assert_eq!(call(json!({ "action_name": "clean.action", "input_data": { "foo": 1, "prev_print": "p" } }))["reason"],
            json!("input contains audit-internal field 'prev_print' — these are populated by with-audit, not by callers"));
        // name check precedes input check
        assert_eq!(call(json!({ "action_name": "bypass-audit-x", "input_data": { "digital_print": "z" } }))["reason"],
            json!("action name contains audit-bypass token 'bypass-audit'"));

        // clean input_data variants → allowed
        assert_eq!(call(json!({ "action_name": "clean.action", "input_data": {} })), json!({ "allowed": true }));
        assert_eq!(call(json!({ "action_name": "clean.action", "input_data": Value::Null })), json!({ "allowed": true }));
        assert_eq!(call(json!({ "action_name": "clean.action", "input_data": { "ok": 1 } })), json!({ "allowed": true }));
    }
}
