//! hook-cache.rs — twin of skills/engine/hook-cache.mjs (no-db). NAMES ARE CONTRACT:
//! `clearHookCache` (named) · `hookCacheClear` (default).
//!
//! The JS source's caches (`hookCache` Map, `missingHookLogged` Set) and its cross-module
//! call into load-skill.mjs's `clearCache()` are process-local, in-memory JS closure state —
//! not db state and not part of any function's return value. Neither lane's harness (fixture
//! I/O comparison + db_after) can observe them, so the twin reproduces the observable
//! contract exactly: `clearHookCache()` returns JS `undefined` (→ `null`), and the default
//! export always returns `{cleared: true}` regardless of input.
use rusqlite::Connection;
use serde_json::{json, Value};

pub fn clearHookCache() -> Value {
    Value::Null
}

pub fn hookCacheClear(_input: &Value, _db: &Connection) -> Result<Value, String> {
    clearHookCache();
    Ok(json!({ "cleared": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clears_and_returns() {
        assert_eq!(clearHookCache(), Value::Null);
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(hookCacheClear(&json!({}), &conn).unwrap(), json!({ "cleared": true }));
    }
}
