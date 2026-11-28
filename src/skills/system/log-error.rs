//! log-error.rs — twin of skills/system/log-error.mjs (helper module, both db-first). NAMES ARE
//! CONTRACT: logError · markRecovered. Writes/updates memory.error_events, the substrate error
//! log for the operator's Drift surface + pattern analysis. logError caps + secret-scrubs the
//! input before persistence, classifies a category (explicit or code/message-derived), and
//! derives a stable pattern_key by stripping volatile fragments (quotes, timestamps, digits,
//! hex runs) from the message so identical failure shapes group together. Designed to NEVER
//! throw: any db failure is swallowed and Null returned, mirroring the JS try/catch-null
//! contract (a logging failure must not break the action it's diagnosing).
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

const CAP_MESSAGE: usize = 1000;
const CAP_STACK: usize = 8000;
const CAP_INPUT: usize = 4000;
const CAP_META: usize = 4000;

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
// JS String(v) (no ??)
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

// `function cap(s, n) { if (s == null) return null; const str = typeof s === 'string' ? s :
// String(s); return str.length > n ? str.slice(0, n) + `…[+${str.length - n} chars]` : str; }`
fn cap(v: Option<&Value>, n: usize) -> Option<String> {
    let v = v.filter(|x| !x.is_null())?;
    let s = match v { Value::String(s) => s.clone(), other => js_string(other) };
    let len = s.chars().count();
    if len > n {
        let truncated: String = s.chars().take(n).collect();
        Some(format!("{}\u{2026}[+{} chars]", truncated, len - n))
    } else {
        Some(s)
    }
}

// `function safeJSON(v) { try { return JSON.stringify(v); } catch { return String(v); } }`
fn safe_json(v: &Value) -> String { serde_json::to_string(v).unwrap_or_else(|_| js_string(v)) }

fn re_secret1() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?i)"(api_key|apikey|token|password|secret|hmac|signature)"\s*:\s*"[^"]+""#).unwrap())
}
fn re_secret2() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"sk-[A-Za-z0-9]{20,}").unwrap()) }
fn re_secret3() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)[a-f0-9]{40,}").unwrap()) }

// Best-effort secret scrub — strip anything that looks like an API key/hex hash from
// input_json before persistence. Note the 3 patterns run in sequence over the RUNNING string
// (not the original), same as the JS `for (const re of SECRET_PATTERNS) out = out.replace(...)`.
fn scrub(s: &str) -> String {
    let s1 = re_secret1().replace_all(s, "\"***\"");
    let s2 = re_secret2().replace_all(&s1, "\"***\"");
    let s3 = re_secret3().replace_all(&s2, "\"***\"");
    s3.into_owned()
}

fn re_sq() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"'[^']*'").unwrap()) }
fn re_dq() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#""[^"]*""#).unwrap()) }
fn re_ts() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}\b").unwrap()) }
fn re_num() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b\d+\b").unwrap()) }
fn re_hex() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\b[a-f0-9]{8,}\b").unwrap()) }
fn re_ws() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\s+").unwrap()) }

// Build a stable pattern_key by stripping volatile fragments (quoted slugs/paths, numeric ids,
// sha-like hex, timestamps) IN THIS ORDER — later passes see the output of earlier ones, so e.g.
// digits inside an already-quote-stripped '%' survive, but a run of hex-alphabet letters left
// behind by an earlier substitution (like the "…[+N chars]" ellipsis suffix once N is %N%'d)
// can still get %HEX%'d if it's 8+ chars long. `!message` (JS falsy: null/undefined/'') -> null.
fn pattern_key_of(message: Option<&str>) -> Value {
    let m = match message { Some(s) if !s.is_empty() => s, _ => return Value::Null };
    let mut s: String = re_sq().replace_all(m, "'%'").into_owned();
    s = re_dq().replace_all(&s, "\"%\"").into_owned();
    s = re_ts().replace_all(&s, "%TS%").into_owned();
    s = re_num().replace_all(&s, "%N%").into_owned();
    s = re_hex().replace_all(&s, "%HEX%").into_owned();
    s = re_ws().replace_all(&s, " ").into_owned();
    let truncated: String = s.trim().chars().take(200).collect();
    json!(truncated)
}

// Map error code or message -> category. Order matters: most specific first.
fn category_of(code: Option<&str>, message: Option<&str>) -> Value {
    let m = message.unwrap_or("").to_lowercase();
    if code == Some("PERMISSION_DENIED") || m.contains("permission denied") || m.contains("dispatch refused") { return json!("permission_denied"); }
    if code == Some("NOT_FOUND") || m.contains("not found") { return json!("not_found"); }
    if m.contains("sanity gate refused") || m.contains("gate refused") { return json!("gate_refused"); }
    if code == Some("VALIDATION_LOGIC") || m.contains("validation failed") || m.contains("schema") { return json!("validation"); }
    if m.contains("terminal status") || m.contains("only apply during") || m.contains("status=") { return json!("state_machine"); }
    if m.contains("timeout") || m.contains("exceeded") { return json!("timeout"); }
    if m.contains("econn") || m.contains("fetch failed") || m.contains("socket") { return json!("integration"); }
    json!("unknown")
}

fn re_skill_name() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)^([a-z][a-z0-9-]+):").unwrap()) }
// Extract a skill name from an AntilokiError-shaped `e` if present: `(e.message ?? '').match(/^([a-z][a-z0-9-]+):/i)`.
fn skill_name_from_code(e: Option<&Value>) -> Option<String> {
    let e = e.filter(|x| truthy(Some(*x)))?;
    let msg = e.get("message").and_then(|v| v.as_str()).unwrap_or("");
    re_skill_name().captures(msg).map(|c| c[1].to_string())
}

/// `export function logError(db, info = {})` — writes one row to memory.error_events. Returns
/// `{ id, category, pattern_key }` on success, `Value::Null` on any failure (never throws).
pub fn logError(db: &Connection, info: &Value) -> Value {
    let e = info.get("error").filter(|v| !v.is_null());

    let message_raw = e.and_then(|ev| ev.get("message")).filter(|v| !v.is_null()).cloned()
        .or_else(|| info.get("error_message").filter(|v| !v.is_null()).cloned())
        .unwrap_or(json!("(no message)"));
    let message = cap(Some(&message_raw), CAP_MESSAGE);

    let code_v = e.and_then(|ev| ev.get("code")).filter(|v| !v.is_null()).cloned()
        .or_else(|| info.get("error_code").filter(|v| !v.is_null()).cloned());
    let code_str = code_v.as_ref().and_then(|v| v.as_str());

    let stack_raw = e.and_then(|ev| ev.get("stack")).filter(|v| !v.is_null()).cloned()
        .or_else(|| info.get("error_stack").filter(|v| !v.is_null()).cloned());
    let stack = cap(stack_raw.as_ref(), CAP_STACK);

    let category_v = info.get("category").filter(|v| !v.is_null()).cloned()
        .unwrap_or_else(|| category_of(code_str, message.as_deref()));
    let pattern_key_v = pattern_key_of(message.as_deref());

    let input_json = info.get("input").filter(|v| !v.is_null())
        .map(|v| cap(Some(&json!(scrub(&safe_json(v)))), CAP_INPUT).unwrap());
    let meta_json = info.get("meta").filter(|v| !v.is_null())
        .map(|v| cap(Some(&json!(safe_json(v))), CAP_META).unwrap());

    let skill_name = info.get("skill_name").filter(|v| !v.is_null()).cloned()
        .or_else(|| skill_name_from_code(e).map(|s| json!(s)));

    let params: Vec<Value> = vec![
        code_v.unwrap_or(Value::Null),
        message.map(|s| json!(s)).unwrap_or(Value::Null),
        stack.map(|s| json!(s)).unwrap_or(Value::Null),
        nz(info.get("action_name")).cloned().unwrap_or(Value::Null),
        skill_name.unwrap_or(Value::Null),
        nz(info.get("source")).cloned().unwrap_or(Value::Null),
        nz(info.get("actor")).cloned().unwrap_or(Value::Null),
        input_json.map(|s| json!(s)).unwrap_or(Value::Null),
        nz(info.get("related_card_slug")).cloned().unwrap_or(Value::Null),
        nz(info.get("related_contract_slug")).cloned().unwrap_or(Value::Null),
        nz(info.get("related_drift_id")).cloned().unwrap_or(Value::Null),
        nz(info.get("related_tick_id")).cloned().unwrap_or(Value::Null),
        nz(info.get("related_turn_id")).cloned().unwrap_or(Value::Null),
        category_v.clone(),
        pattern_key_v.clone(),
        meta_json.map(|s| json!(s)).unwrap_or(Value::Null),
    ];

    const SQL: &str = "\n      INSERT INTO memory.error_events (\n        error_code, error_message, error_stack,\n        action_name, skill_name, source, actor,\n        input_json,\n        related_card_slug, related_contract_slug,\n        related_drift_id, related_tick_id, related_turn_id,\n        category, pattern_key,\n        meta_json\n      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)\n    ";

    let rowid = match crate::db_json::run_write(db, SQL, &params) {
        Ok((_, id)) => id,
        Err(_) => return Value::Null,
    };

    json!({ "id": rowid, "category": category_v, "pattern_key": pattern_key_v })
}

/// `export function markRecovered(db, errorId, recovery_action, recovery_outcome)` — closes the
/// loop when a later dispatch succeeds. Falsy errorId short-circuits to `false` (no update
/// attempted). `recovered_at` uses the FROZEN clock literal in place of `datetime('now')` — the
/// twin has no clock hook, so it writes the SAME instant the harness rewrites the JS SQL to.
pub fn markRecovered(db: &Connection, error_id: &Value, recovery_action: &Value, recovery_outcome: &Value) -> Value {
    if !truthy(Some(error_id)) { return json!(false); }
    // a `let`, not a `const`: the clock choice is made at runtime by crate::clock.
    let sql = format!("\n      UPDATE memory.error_events\n      SET recovery_action = ?, recovery_outcome = ?, recovered_at = {}\n      WHERE id = ?\n    ", crate::clock::now_sql());
    let params = vec![nz(Some(recovery_action)).cloned().unwrap_or(Value::Null), nz(Some(recovery_outcome)).cloned().unwrap_or(Value::Null), error_id.clone()];
    match crate::db_json::run_write(db, &sql, &params) {
        Ok(_) => json!(true),
        Err(_) => json!(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS memory; \
             CREATE TABLE memory.error_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', \
               error_code TEXT, error_message TEXT NOT NULL, error_stack TEXT, action_name TEXT, skill_name TEXT, source TEXT, actor TEXT, \
               input_json TEXT, related_card_slug TEXT, related_contract_slug TEXT, related_drift_id TEXT, related_tick_id INTEGER, related_turn_id INTEGER, \
               category TEXT NOT NULL DEFAULT 'unknown', pattern_key TEXT, recovery_action TEXT, recovery_outcome TEXT, recovered_at TEXT, meta_json TEXT);",
        ).unwrap();
        c
    }
    fn row(c: &Connection, id: i64) -> Value {
        let rows = crate::db_json::query_rows(c, "SELECT * FROM memory.error_events WHERE id = ?", &[json!(id)]).unwrap();
        rows.as_array().and_then(|a| a.first()).cloned().unwrap()
    }

    #[test]
    fn log_error_full_object_and_scrub() {
        let c = fresh();
        let r = logError(&c, &json!({
            "error": { "message": "widget-loader: card 'ABC-123' not found", "code": "NOT_FOUND", "stack": "Error: not found\n    at foo (bar.js:1:1)" },
            "action_name": "tl.contract.amend", "source": "web-fe", "actor": "web-fe:drift-resolver",
            "input": { "slug": "ABC-123", "token": "abcdef1234567890" }, "meta": { "extra": true },
            "related_card_slug": "ABC-123", "related_contract_slug": "CT-1", "related_drift_id": "D-1",
            "related_tick_id": 7, "related_turn_id": 3
        }));
        assert_eq!(r, json!({ "id": 1, "category": "not_found", "pattern_key": "widget-loader: card '%' not found" }));
        let row = row(&c, 1);
        assert_eq!(row["error_code"], json!("NOT_FOUND"));
        assert_eq!(row["skill_name"], json!("widget-loader")); // derived from e.message prefix, not explicit
        assert_eq!(row["input_json"], json!("{\"slug\":\"ABC-123\",\"***\"}")); // token key-value pair scrubbed whole
        assert_eq!(row["meta_json"], json!("{\"extra\":true}")); // meta is NEVER scrubbed
        assert_eq!(row["ts"], json!("2026-01-01 00:00:00"));
        assert_eq!(row["related_tick_id"], json!(7));
    }

    #[test]
    fn log_error_fallback_fields_explicit_category_and_falsy_message() {
        let c = fresh();
        let r = logError(&c, &json!({ "error_message": "", "error_code": "VALIDATION_LOGIC", "category": "custom_cat", "skill_name": "explicit-skill", "source": "cli" }));
        assert_eq!(r, json!({ "id": 1, "category": "custom_cat", "pattern_key": Value::Null }));
        let row = row(&c, 1);
        assert_eq!(row["error_message"], json!(""));
        assert_eq!(row["skill_name"], json!("explicit-skill"));
        assert_eq!(row["input_json"], Value::Null);
        assert_eq!(row["meta_json"], Value::Null);
    }

    #[test]
    fn log_error_absent_info_defaults() {
        let c = fresh();
        let r = logError(&c, &json!({}));
        assert_eq!(r, json!({ "id": 1, "category": "unknown", "pattern_key": "(no message)" }));
        assert_eq!(row(&c, 1)["error_message"], json!("(no message)"));
    }

    #[test]
    fn log_error_sk_and_hex_scrub_and_permission_category() {
        let c = fresh();
        let r = logError(&c, &json!({
            "error": { "message": "permission denied: dispatch refused" },
            "input": { "sk": "sk-ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef1234", "hash": "0123456789abcdef0123456789abcdef01234567" }
        }));
        assert_eq!(r["category"], json!("permission_denied"));
        assert_eq!(r["pattern_key"], json!("permission denied: dispatch refused"));
        assert_eq!(row(&c, 1)["input_json"], json!("{\"sk\":\"\"***\"\",\"hash\":\"\"***\"\"}"));
        // "permission denied:" has a space before the colon, so the ^([a-z][a-z0-9-]+): prefix
        // regex never matches (the space breaks the [a-z0-9-]+ run before ':' is reached).
        assert_eq!(row(&c, 1)["skill_name"], Value::Null);
    }

    #[test]
    fn log_error_cap_truncation_then_hex_pass_eats_the_ellipsis_suffix() {
        let c = fresh();
        let long = "A".repeat(1050);
        let r = logError(&c, &json!({ "error": { "message": long }, "meta": { "foo": "bar", "big": 1 } }));
        assert_eq!(r["category"], json!("unknown"));
        assert_eq!(r["pattern_key"], json!("%HEX%\u{2026}[+%N% chars]"));
        let stored_message = row(&c, 1)["error_message"].as_str().unwrap().to_string();
        assert_eq!(stored_message.chars().count(), 1012); // 1000 capped + "…[+50 chars]"
        assert!(stored_message.ends_with("\u{2026}[+50 chars]"));
        assert_eq!(row(&c, 1)["skill_name"], Value::Null); // no colon-prefixed pattern in a run of 'A's
    }

    #[test]
    fn mark_recovered_branches() {
        let c = fresh();
        c.execute("INSERT INTO memory.error_events (id, error_message) VALUES (10, 'boom')", []).unwrap();

        assert_eq!(markRecovered(&c, &json!(10), &json!("manual-retry"), &json!("ok")), json!(true));
        let r = row(&c, 10);
        assert_eq!(r["recovery_action"], json!("manual-retry"));
        assert_eq!(r["recovery_outcome"], json!("ok"));
        assert_eq!(r["recovered_at"], json!("2026-01-01 00:00:00"));

        // falsy errorId short-circuits, no SQL runs
        assert_eq!(markRecovered(&c, &Value::Null, &json!("x"), &json!("y")), json!(false));
        assert_eq!(markRecovered(&c, &json!(0), &json!("a"), &json!("b")), json!(false));

        // no matching row: UPDATE still "succeeds" (0 rows affected is not an error)
        assert_eq!(markRecovered(&c, &json!(999), &Value::Null, &Value::Null), json!(true));
    }
}
