//! bench-record.rs — twin of skills/bench/bench-record.mjs. NAMES ARE CONTRACT:
//! `benchRecord` (default), `gitSha` (named).
//!
//! BESPOKE-NATIVE: `gitSha()` unconditionally shells out to `git rev-parse HEAD` (mirrors JS
//! `execSync('git rev-parse HEAD', ...)`) on EVERY success path of `benchRecord` — there is no
//! input flag or optional field whose absence keeps the insert deterministic and fixture-safe,
//! unlike e.g. constitution-estimate.rs's flag-gated git-diff branch. Per the crossing
//! playbook's bespoke-native rule this unit therefore carries NO conform-db fixture/frontier
//! entry (same precedent as `_helpers/run-cmd.rs`'s `runCmd`) — proven instead by the real,
//! non-mocked `#[cfg(test)]` tests below: a real `git rev-parse HEAD` against this repo's working
//! tree, plus a real INSERT/SELECT round trip against an in-memory `brain.bench_runs`.
//!
//! `gitSha` caches process-wide, mirroring the JS module-level `let cachedSha = null` closure
//! cache (computed once per process, then reused). JS: on failure or empty output `cachedSha`
//! caches `''`, and the fn returns `cachedSha || null` → `null`; ported as
//! `OnceLock<Option<String>>` where `None` represents that `null` (both the "not yet computed"
//! JS state and the "computed, but empty" JS state collapse into the same cached `None` here,
//! which is observationally identical since JS never distinguishes them after the first call
//! either — both read back as `null`).
//!
//! `context.db` missing / `run_id`/`bench_name`/`metric` falsy / `value` not a number → JS throws
//! `err.contextMissing`/`err.required`. Not modeled — same "guard-throws are not exercised"
//! convention used by every sibling twin (Rust's `db: &Connection` argument makes the first one
//! unreachable at the type level anyway).

use rusqlite::Connection;
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

static CACHED_SHA: OnceLock<Option<String>> = OnceLock::new();

/// `export function gitSha()` — see module doc: bespoke-native, process-wide cached.
#[allow(non_snake_case)]
pub fn gitSha() -> Option<String> {
    CACHED_SHA
        .get_or_init(|| {
            let out = crate::proc::cmd("git")
                .args(["rev-parse", "HEAD"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    if s.is_empty() { None } else { Some(s) }
                }
                _ => None,
            }
        })
        .clone()
}

/// `export default function benchRecord(input, context)` — see module doc.
#[allow(non_snake_case)]
pub fn benchRecord(input: &Value, db: &Connection) -> Result<Value, String> {
    let run_id = match input.get("run_id") {
        Some(v) if truthy(Some(v)) => v.as_str().unwrap_or_default(),
        _ => return Err("bench-record: run_id is required".to_string()),
    };
    let bench_name = match input.get("bench_name") {
        Some(v) if truthy(Some(v)) => v.as_str().unwrap_or_default(),
        _ => return Err("bench-record: bench_name is required".to_string()),
    };
    let metric = match input.get("metric") {
        Some(v) if truthy(Some(v)) => v.as_str().unwrap_or_default(),
        _ => return Err("bench-record: metric is required".to_string()),
    };
    let value = match input.get("value") {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        _ => return Err("bench-record: value is required".to_string()),
    };
    let unit = input.get("unit").and_then(Value::as_str);
    // `context: ctx = null` destructure of input.context — NOT the outer skill `context` param.
    let ctx = input.get("context").filter(|v| !v.is_null());
    let ctx_json = ctx.map(|v| serde_json::to_string(v).unwrap());

    db.execute(
        "INSERT INTO brain.bench_runs (run_id, bench_name, metric, value, unit, context_json, git_sha) VALUES (?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![run_id, bench_name, metric, value, unit, ctx_json, gitSha()],
    ).map_err(|e| e.to_string())?;

    let id = db.last_insert_rowid();
    let rows = crate::db_json::query_rows(db, "SELECT * FROM brain.bench_runs WHERE id = ?", &[json!(id)])?;
    Ok(rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH DATABASE ':memory:' AS brain; \
             CREATE TABLE brain.bench_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, \
               ts TEXT NOT NULL DEFAULT (datetime('now')), run_id TEXT NOT NULL, \
               bench_name TEXT NOT NULL, metric TEXT NOT NULL, value REAL NOT NULL, \
               unit TEXT, context_json TEXT, git_sha TEXT);",
        )
        .unwrap();
        c
    }

    #[test]
    fn git_sha_returns_a_real_40_char_hex_sha_for_this_repo() {
        let sha = gitSha();
        let s = sha.expect("this crate is a git repo — git rev-parse HEAD must succeed");
        assert_eq!(s.len(), 40);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
        // Cached: a second call returns the identical value without re-shelling out.
        assert_eq!(gitSha(), Some(s));
    }

    #[test]
    fn bench_record_inserts_a_row_and_returns_it_with_git_sha_populated() {
        let c = db();
        let r = benchRecord(
            &json!({ "run_id": "r1", "bench_name": "card-add", "metric": "latency_ms", "value": 5.25, "unit": "ms" }),
            &c,
        )
        .unwrap();
        assert_eq!(r["run_id"], json!("r1"));
        assert_eq!(r["bench_name"], json!("card-add"));
        assert_eq!(r["metric"], json!("latency_ms"));
        assert_eq!(r["value"], json!(5.25));
        assert_eq!(r["unit"], json!("ms"));
        assert_eq!(r["context_json"], Value::Null);
        assert!(r["git_sha"].as_str().unwrap().len() == 40);
        assert!(r["id"].is_i64());
    }

    #[test]
    fn bench_record_serializes_context_and_defaults_unit_to_null() {
        let c = db();
        let r = benchRecord(
            &json!({ "run_id": "r2", "bench_name": "db-open", "metric": "cold_ms", "value": 50, "context": { "k": "v" } }),
            &c,
        )
        .unwrap();
        assert_eq!(r["unit"], Value::Null);
        assert_eq!(r["context_json"], json!("{\"k\":\"v\"}"));
        assert_eq!(r["value"], json!(50.0));
    }

    #[test]
    fn bench_record_missing_required_fields_errs() {
        let c = db();
        assert!(benchRecord(&json!({ "bench_name": "x", "metric": "y", "value": 1 }), &c).is_err());
        assert!(benchRecord(&json!({ "run_id": "x", "metric": "y", "value": 1 }), &c).is_err());
        assert!(benchRecord(&json!({ "run_id": "x", "bench_name": "y", "value": 1 }), &c).is_err());
        assert!(benchRecord(&json!({ "run_id": "x", "bench_name": "y", "metric": "z" }), &c).is_err());
        assert!(benchRecord(&json!({ "run_id": "x", "bench_name": "y", "metric": "z", "value": "not-a-number" }), &c).is_err());
    }
}
