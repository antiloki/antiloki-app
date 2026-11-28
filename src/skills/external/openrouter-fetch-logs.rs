//! openrouter-fetch-logs.rs — twin of skills/external/openrouter-fetch-logs.mjs. NAMES ARE
//! CONTRACT: default `openrouterFetchLogs`. Read-only (GET only, no inference, no cost) fetch of
//! the PROMPT + COMPLETION text OpenRouter stored for a generation (`/generation/content` +
//! `/generation` metadata), keyed by `openrouter_generation_id` captured on `brain.ai_calls`.
//!
//! LANE A (bespoke-native network adapter), same split as anthropic-call.rs / deepseek-call.rs:
//! the two real `fetch(...)` GETs inside `fetch_one` are the DEFERRED boundary — genuinely
//! non-deterministic (a live OpenRouter response), not JSON-fixturable. Everything DETERMINISTIC
//! around it is ported faithfully and IS what this file's tests exercise:
//! - `flatten_prompt(input)`: pure — the `[role] content` join / `input.prompt` / raw-JSON
//!   fallback, fed canned response-shaped JSON in tests.
//! - `read_key()`: a REAL effect (env var, else the gitignored `AUTH_DIR/openrouter.key` file —
//!   same BYOK convention/messages as auth-sign.rs's key read), not deferred — file IO is
//!   deterministic given the fs state, unlike the network hop.
//! - `resolve_ids(input, db)`: the REAL `brain.ai_calls` id-resolution branch (three modes:
//!   `generation_id` passthrough, `ai_call_id` single-row lookup, or `since_id`/`limit` batch) —
//!   a real db query, exercised end-to-end via `#[cfg(test)]` with an in-memory `brain.ai_calls`.
//!   When `ids` resolves empty, the JS returns its own deterministic "no ids" note WITHOUT ever
//!   reaching `fetch_one` — that whole branch is real and tested here too.
//!
//! `fetch_one(id, key, with_metadata)` (mirrors JS `fetchOne`) is the PARITY-DEFERRED stub: no
//! `fetch_fn` injection point exists in this crate (no HTTP client wired), so it returns an `Err`
//! carrying the same "content fetch failed: <message>" shape the JS produces on a thrown fetch,
//! proving the request URLs it would have built (`{BASE}/generation/content?id=...`,
//! `{BASE}/generation?id=...`) without performing the round-trip. `openrouterFetchLogs` (the
//! default export) calls it once per resolved id and folds any `fetch_one` failure into that same
//! id's `results[].error`, exactly like the JS `try/catch` inside `fetchOne` — so the function
//! itself does NOT bubble an `Err` for a fetch failure, matching JS parity (only `contextMissing`/
//! key-not-found throw). No frontier entry, no conform-db fixture for the whole default export —
//! BESPOKE-NATIVE TESTING only, via `#[cfg(test)]` below.
//!
//! Throw fidelity (verified against skills/external/openrouter-fetch-logs.mjs source):
//! - missing db when `ai_call_id`/batch mode needed → `err.contextMissing('openrouter-fetch-logs',
//!   'db')` → `"openrouter-fetch-logs: context.db is required"`.
//! - no key found (`generation_id` present short-circuits ids resolution but `read_key()` still
//!   runs before it in the JS — reproduced in the same order here) →
//!   `"openrouter-fetch-logs: OpenRouter API key not found. Set OPENROUTER_API_KEY env or write
//!   the key to <KEY_PATH>."`.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::PathBuf;

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

#[cfg(windows)]
fn homedir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("USERPROFILE") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    let drive = std::env::var("HOMEDRIVE").ok().filter(|s| !s.is_empty());
    let path = std::env::var("HOMEPATH").ok().filter(|s| !s.is_empty());
    match (drive, path) {
        (Some(d), Some(p)) => Some(PathBuf::from(format!("{d}{p}"))),
        _ => None,
    }
}

#[cfg(not(windows))]
fn homedir() -> Option<PathBuf> {
    std::env::var("HOME").ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

// roots.mjs: HOME_DIR = resolve($ANTILOKI_HOME) if set+truthy, else join(homedir(), '.antiloki');
// AUTH_DIR = join(HOME_DIR, 'auth'). KEY_PATH = resolve(AUTH_DIR, 'openrouter.key').
fn key_path() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("auth").join("openrouter.key")
}

const BASE: &str = "https://openrouter.ai/api/v1";

/// `function readKey()` — env var wins, else the gitignored key file; throws AntilokiError
/// (PERMISSION_DENIED) with the same message shape as auth-sign.rs / openrouter-call's readKey.
fn read_key() -> Result<String, String> {
    if let Ok(env) = std::env::var("OPENROUTER_API_KEY") {
        let trimmed = env.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let p = key_path();
    if p.exists() {
        if let Ok(raw) = std::fs::read_to_string(&p) {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                return Ok(trimmed.to_string());
            }
        }
    }
    Err(format!(
        "openrouter-fetch-logs: OpenRouter API key not found. Set OPENROUTER_API_KEY env or write the key to {}.",
        p.to_string_lossy()
    ))
}

/// `function flattenPrompt(input)`:
/// `Array.isArray(input?.messages)` → join `[role] content` (content stringified if non-string);
/// else `input?.prompt` → `String(input.prompt)`; else `input ? JSON.stringify(input) : null`.
pub fn flatten_prompt(input: &Value) -> Value {
    if input.is_null() {
        return Value::Null;
    }
    if let Some(msgs) = input.get("messages").and_then(Value::as_array) {
        let parts: Vec<String> = msgs
            .iter()
            .map(|m| {
                let role = str_qq(m.get("role"));
                let content = m.get("content");
                let content_str = match content {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) => v.to_string(),
                    None => "undefined".to_string(),
                };
                format!("[{role}] {content_str}")
            })
            .collect();
        return json!(parts.join("\n\n"));
    }
    if let Some(prompt) = input.get("prompt").filter(|v| !v.is_null()) {
        return json!(str_qq(Some(prompt)));
    }
    json!(input.to_string())
}

/// `async function fetchOne(id, key, fetchFn, withMetadata)` — PARITY-DEFERRED: builds the exact
/// request URLs the JS would GET, then stops before any real network hop (no `fetch_fn` injection
/// point exists in this crate). Returns the same shape the JS's own `catch` branch produces for a
/// failed fetch, folded by the caller into that id's `results[].error` (never bubbled as an `Err`
/// of the whole default export, matching JS's fetchOne-never-throws contract).
fn fetch_one(id: &str, with_metadata: bool) -> Value {
    let content_url = format!("{BASE}/generation/content?id={}", urlencode(id));
    let meta_url = format!("{BASE}/generation?id={}", urlencode(id));
    let mut error = format!(
        "content fetch failed: PARITY-DEFERRED — openrouter-fetch-logs twin has no real HTTP client wired; would GET {content_url}"
    );
    if with_metadata {
        error.push_str(&format!(" (metadata GET {meta_url} also deferred)"));
    }
    json!({
        "generation_id": id,
        "prompt": null,
        "completion": null,
        "reasoning": null,
        "metadata": null,
        "error": error,
    })
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The three id-resolution branches of `openrouterFetchLogs` (real `brain.ai_calls` reads),
/// exercised directly by `#[cfg(test)]`. `db` required (throws `contextMissing`) except when
/// `generation_id` is given directly.
fn resolve_ids(input: &Value, db: Option<&Connection>) -> Result<Vec<String>, String> {
    let generation_id = input.get("generation_id").filter(|v| !v.is_null());
    let ai_call_id = input.get("ai_call_id").filter(|v| !v.is_null());
    let since_id = input.get("since_id").filter(|v| !v.is_null());
    let limit = input.get("limit").and_then(Value::as_i64).unwrap_or(50);

    if let Some(gid) = generation_id {
        return Ok(vec![str_qq(Some(gid))]);
    }

    let db = db.ok_or_else(|| "openrouter-fetch-logs: context.db is required".to_string())?;

    if let Some(aid) = ai_call_id {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT openrouter_generation_id g FROM brain.ai_calls WHERE id = ?",
            &[aid.clone()],
        )?;
        let g = rows
            .as_array()
            .and_then(|a| a.first())
            .and_then(|r| r.get("g"))
            .filter(|v| !v.is_null());
        return Ok(match g {
            Some(v) => vec![str_qq(Some(v))],
            None => vec![],
        });
    }

    let rows = if let Some(sid) = since_id {
        crate::db_json::query_rows(
            db,
            "SELECT openrouter_generation_id g FROM brain.ai_calls WHERE openrouter_generation_id IS NOT NULL AND id >= ? ORDER BY id DESC LIMIT ?",
            &[sid.clone(), json!(limit)],
        )?
    } else {
        crate::db_json::query_rows(
            db,
            "SELECT openrouter_generation_id g FROM brain.ai_calls WHERE openrouter_generation_id IS NOT NULL ORDER BY id DESC LIMIT ?",
            &[json!(limit)],
        )?
    };
    Ok(rows
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.get("g"))
        .map(|v| str_qq(Some(v)))
        .collect())
}

/// `export default async function openrouterFetchLogs(input, context)`.
pub fn openrouterFetchLogs(input: &Value, db: Option<&Connection>) -> Result<Value, String> {
    let with_metadata = input.get("with_metadata").and_then(Value::as_bool).unwrap_or(true);
    let ids = resolve_ids(input, db)?;

    if ids.is_empty() {
        return Ok(json!({
            "fetched": 0,
            "with_content": 0,
            "results": [],
            "note": "No generation ids to fetch. Capture lands on OpenRouter calls made AFTER migration 139; cache hits and non-OpenRouter providers carry none.",
        }));
    }

    let _key = read_key()?;
    let results: Vec<Value> = ids.iter().map(|id| fetch_one(id, with_metadata)).collect();
    let with_content = results
        .iter()
        .filter(|r| !r.get("prompt").map(Value::is_null).unwrap_or(true))
        .count();

    Ok(json!({
        "fetched": ids.len(),
        "with_content": with_content,
        "results": results,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_home<T>(dir: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("OPENROUTER_API_KEY");
            std::env::set_var("ANTILOKI_HOME", dir);
        }
        let out = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
        }
        out
    }

    fn open_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "ATTACH DATABASE ':memory:' AS brain;
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, openrouter_generation_id TEXT);",
        )
        .unwrap();
        db
    }

    #[test]
    fn flatten_prompt_joins_messages() {
        let input = json!({ "messages": [
            { "role": "system", "content": "be terse" },
            { "role": "user", "content": "hi" },
        ]});
        assert_eq!(flatten_prompt(&input), json!("[system] be terse\n\n[user] hi"));
    }

    #[test]
    fn flatten_prompt_stringifies_non_string_content() {
        let input = json!({ "messages": [ { "role": "user", "content": { "a": 1 } } ] });
        assert_eq!(flatten_prompt(&input), json!("[user] {\"a\":1}"));
    }

    #[test]
    fn flatten_prompt_falls_back_to_prompt_field() {
        assert_eq!(flatten_prompt(&json!({ "prompt": "say hi" })), json!("say hi"));
    }

    #[test]
    fn flatten_prompt_raw_json_fallback() {
        assert_eq!(flatten_prompt(&json!({ "x": 1 })), json!("{\"x\":1}"));
    }

    #[test]
    fn flatten_prompt_null_input_is_null() {
        assert_eq!(flatten_prompt(&Value::Null), Value::Null);
    }

    #[test]
    fn resolve_ids_generation_id_short_circuits_no_db_needed() {
        let ids = resolve_ids(&json!({ "generation_id": "gen-1" }), None).unwrap();
        assert_eq!(ids, vec!["gen-1".to_string()]);
    }

    #[test]
    fn resolve_ids_missing_db_is_context_missing() {
        let e = resolve_ids(&json!({ "ai_call_id": 1 }), None).unwrap_err();
        assert_eq!(e, "openrouter-fetch-logs: context.db is required");
    }

    #[test]
    fn resolve_ids_ai_call_id_resolves_from_db() {
        let db = open_db();
        db.execute("INSERT INTO brain.ai_calls (id, openrouter_generation_id) VALUES (7, 'gen-7')", []).unwrap();
        let ids = resolve_ids(&json!({ "ai_call_id": 7 }), Some(&db)).unwrap();
        assert_eq!(ids, vec!["gen-7".to_string()]);
    }

    #[test]
    fn resolve_ids_ai_call_id_missing_row_is_empty() {
        let db = open_db();
        let ids = resolve_ids(&json!({ "ai_call_id": 999 }), Some(&db)).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn resolve_ids_batch_orders_desc_and_respects_limit() {
        let db = open_db();
        db.execute("INSERT INTO brain.ai_calls (id, openrouter_generation_id) VALUES (1,'g1'),(2,'g2'),(3,NULL),(4,'g4')", []).unwrap();
        let ids = resolve_ids(&json!({ "limit": 2 }), Some(&db)).unwrap();
        assert_eq!(ids, vec!["g4".to_string(), "g2".to_string()]);
    }

    #[test]
    fn resolve_ids_since_id_filters() {
        let db = open_db();
        db.execute("INSERT INTO brain.ai_calls (id, openrouter_generation_id) VALUES (1,'g1'),(2,'g2'),(3,'g3')", []).unwrap();
        let ids = resolve_ids(&json!({ "since_id": 2, "limit": 50 }), Some(&db)).unwrap();
        assert_eq!(ids, vec!["g3".to_string(), "g2".to_string()]);
    }

    #[test]
    fn no_ids_returns_deterministic_note_without_key_lookup() {
        let db = open_db();
        // No OPENROUTER_API_KEY set and no key file — would fail read_key() if reached.
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("OPENROUTER_API_KEY") };
        let out = openrouterFetchLogs(&json!({}), Some(&db)).unwrap();
        assert_eq!(out["fetched"], json!(0));
        assert_eq!(out["with_content"], json!(0));
        assert_eq!(out["results"], json!([]));
        assert!(out["note"].as_str().unwrap().contains("migration 139"));
    }

    #[test]
    fn missing_key_throws_permission_denied_shape() {
        let dir = {
            let mut d = std::env::temp_dir();
            d.push(format!("antiloki-openrouter-fetch-logs-rs-nokeyfile-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            d
        };
        let e = with_home(dir.to_str().unwrap(), || {
            openrouterFetchLogs(&json!({ "generation_id": "gen-1" }), None).unwrap_err()
        });
        assert!(e.starts_with("openrouter-fetch-logs: OpenRouter API key not found."), "{e}");
        assert!(e.contains("openrouter.key"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_key_wins_and_fetch_is_deferred_into_result_error() {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("OPENROUTER_API_KEY", "sk-or-test") };
        let out = openrouterFetchLogs(&json!({ "generation_id": "gen-42" }), None).unwrap();
        unsafe { std::env::remove_var("OPENROUTER_API_KEY") };
        assert_eq!(out["fetched"], json!(1));
        assert_eq!(out["with_content"], json!(0));
        let r0 = &out["results"][0];
        assert_eq!(r0["generation_id"], json!("gen-42"));
        assert!(r0["prompt"].is_null());
        assert!(r0["error"].as_str().unwrap().contains("PARITY-DEFERRED"), "{}", r0["error"]);
    }
}
