//! error.rs — twin of skills/engine/error.mjs (wave 1 surface). NAMES ARE CONTRACT.
//!
//! Crossed now: `ErrorCodes` (data) + `errorCodesList` (named default accessor).
//! Deferred, by design, to the dispatch-layer crossing: the `AntilokiError` class (Rust wants
//! a struct + std::error::Error impl designed with the dispatcher, not guessed here) and the
//! `err` convenience constructors (they mint AntilokiError instances with live timestamps —
//! a nondeterministic surface no fixture can pin).
// QUALITY: 80/100 — partial surface by design (class + err deferred to dispatch lane); embedded-order list justified

use serde_json::Value;
use std::sync::LazyLock;

pub static ErrorCodes: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(r#"{"VALIDATION_REQUIRED":"VALIDATION_REQUIRED","VALIDATION_TYPE":"VALIDATION_TYPE","VALIDATION_RANGE":"VALIDATION_RANGE","VALIDATION_ENUM":"VALIDATION_ENUM","VALIDATION_LOGIC":"VALIDATION_LOGIC","NOT_FOUND":"NOT_FOUND","ALREADY_EXISTS":"ALREADY_EXISTS","ALREADY_SUPERSEDED":"ALREADY_SUPERSEDED","LOCK_CONTENTION":"LOCK_CONTENTION","LOCK_OWNER_MISMATCH":"LOCK_OWNER_MISMATCH","PERMISSION_DENIED":"PERMISSION_DENIED","LAW_RESOLUTION_FAILED":"LAW_RESOLUTION_FAILED","CONTEXT_MISSING":"CONTEXT_MISSING","CONFIG_MISSING":"CONFIG_MISSING","PARSE_ERROR":"PARSE_ERROR","IO_ERROR":"IO_ERROR","INTERNAL_ERROR":"INTERNAL_ERROR","SKILL_NOT_FOUND":"SKILL_NOT_FOUND","ACTION_NOT_FOUND":"ACTION_NOT_FOUND","SKILL_INVALID":"SKILL_INVALID"}"#).expect("ErrorCodes: embedded JSON is valid by construction")
});

/// JS: named default `errorCodesList(_input, _context)` → `Object.values(ErrorCodes)`.
/// The array is EMBEDDED in declaration order — deriving it from the map here would sort keys
/// (serde_json BTreeMap) and silently break order parity with V8's insertion order.
pub fn errorCodesList(_input: &Value, _context: &Value) -> Value {
    serde_json::from_str(r#"["VALIDATION_REQUIRED","VALIDATION_TYPE","VALIDATION_RANGE","VALIDATION_ENUM","VALIDATION_LOGIC","NOT_FOUND","ALREADY_EXISTS","ALREADY_SUPERSEDED","LOCK_CONTENTION","LOCK_OWNER_MISMATCH","PERMISSION_DENIED","LAW_RESOLUTION_FAILED","CONTEXT_MISSING","CONFIG_MISSING","PARSE_ERROR","IO_ERROR","INTERNAL_ERROR","SKILL_NOT_FOUND","ACTION_NOT_FOUND","SKILL_INVALID"]"#).expect("error codes list: embedded JSON is valid by construction")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_matches_the_registry_values() {
        let list = errorCodesList(&Value::Null, &Value::Null);
        let codes = ErrorCodes.as_object().expect("object");
        assert_eq!(list.as_array().expect("array").len(), codes.len());
        for v in list.as_array().unwrap() {
            assert!(codes.values().any(|c| c == v), "{v} present in ErrorCodes");
        }
    }
}
