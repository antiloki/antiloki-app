//! law-hook-mutation-via-with-audit.rs — twin of skills/audit/law-hook-mutation-via-with-audit.mjs
//! (no-db, pure — env-gated). NAMES ARE CONTRACT: `lawHookMutationViaWithAudit` (default). Enforces
//! NEVER_BYPASS_WITH_AUDIT: refuses actions whose name carries an audit-bypass token, or whose exact
//! name matches a known internal-mutator basename that must only be invoked through its
//! with-audit-composing wrapper. Escape hatch: ANTILOKI_ALLOW_AUDIT_BYPASS=true.
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

// Action name substrings that signal audit-bypass intent (order = first-match order, as in JS).
const SUSPECT_NAME_TOKENS: [&str; 5] = [
    "unsafe-skill",
    "raw-mutate",
    "bypass-audit",
    "direct-write",
    "skip-audit",
];

// Internal mutator skills that should ONLY be invoked through their wrapping with-audit-composing
// action — action-run seeing the raw name directly is a bypass attempt. Exact-match set (not
// substring), unlike SUSPECT_NAME_TOKENS.
const INTERNAL_MUTATORS: [&str; 6] = [
    "card-add-raw",
    "card-update-raw",
    "agent-add-raw",
    "action-add-raw",
    "law-add-raw",
    "memory-add-raw",
];

pub fn lawHookMutationViaWithAudit(input: &Value, _db: &Connection) -> Result<Value, String> {
    // const { action_name } = input ?? {};
    let action_name = input.get("action_name");
    if !truthy(action_name) {
        return Ok(json!({ "allowed": true }));
    }

    if std::env::var("ANTILOKI_ALLOW_AUDIT_BYPASS").ok().as_deref() == Some("true") {
        return Ok(json!({ "allowed": true, "escape_hatch": "ANTILOKI_ALLOW_AUDIT_BYPASS=true" }));
    }

    let name = action_name.and_then(|v| v.as_str()).unwrap_or("");
    let lowered = name.to_lowercase();
    for tok in SUSPECT_NAME_TOKENS {
        if lowered.contains(tok) {
            return Ok(json!({
                "allowed": false,
                "reason": format!("action name contains audit-bypass token '{tok}'"),
                "suggested_path": "call the wrapping action (e.g., board-keeper.card.update) which composes with-audit",
            }));
        }
    }

    if INTERNAL_MUTATORS.contains(&name) {
        return Ok(json!({
            "allowed": false,
            "reason": format!("'{name}' is an internal mutator — must be called through its with-audit-composing wrapper"),
            "suggested_path": "find the wrapping action via system.action.find; never call raw mutators via action-run",
        }));
    }

    Ok(json!({ "allowed": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Value {
        lawHookMutationViaWithAudit(&i, &Connection::open_in_memory().unwrap()).unwrap()
    }

    #[test]
    fn gates() {
        // no action_name / empty → allowed
        assert_eq!(call(json!({})), json!({ "allowed": true }));
        assert_eq!(call(json!({ "action_name": "" })), json!({ "allowed": true }));
        // clean action → allowed
        assert_eq!(call(json!({ "action_name": "board-keeper.card.update" })), json!({ "allowed": true }));

        // name-token block (first-match + interpolated token)
        let u = call(json!({ "action_name": "call.unsafe-skill.now" }));
        assert_eq!(u["allowed"], json!(false));
        assert_eq!(u["reason"], json!("action name contains audit-bypass token 'unsafe-skill'"));
        assert_eq!(u["suggested_path"], json!("call the wrapping action (e.g., board-keeper.card.update) which composes with-audit"));
        // case-insensitive (toLowerCase)
        assert_eq!(call(json!({ "action_name": "BYPASS-AUDIT-NOW" }))["reason"],
            json!("action name contains audit-bypass token 'bypass-audit'"));
        assert_eq!(call(json!({ "action_name": "do-raw-mutate-thing" }))["reason"],
            json!("action name contains audit-bypass token 'raw-mutate'"));
        assert_eq!(call(json!({ "action_name": "direct-write-op" }))["reason"],
            json!("action name contains audit-bypass token 'direct-write'"));
        assert_eq!(call(json!({ "action_name": "skip-audit-please" }))["reason"],
            json!("action name contains audit-bypass token 'skip-audit'"));

        // internal mutator exact match
        let m = call(json!({ "action_name": "card-add-raw" }));
        assert_eq!(m["allowed"], json!(false));
        assert_eq!(m["reason"], json!("'card-add-raw' is an internal mutator — must be called through its with-audit-composing wrapper"));
        assert_eq!(m["suggested_path"], json!("find the wrapping action via system.action.find; never call raw mutators via action-run"));
        for name in ["card-update-raw", "agent-add-raw", "action-add-raw", "law-add-raw", "memory-add-raw"] {
            assert_eq!(call(json!({ "action_name": name }))["allowed"], json!(false));
        }

        // internal mutator check is EXACT (not substring) — case mismatch / suffix extension slip through
        assert_eq!(call(json!({ "action_name": "CARD-ADD-RAW" })), json!({ "allowed": true }));
        assert_eq!(call(json!({ "action_name": "card-add-raw-extra" })), json!({ "allowed": true }));
    }
}
