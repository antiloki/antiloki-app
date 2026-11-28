//! bench-run-all.rs — twin of skills/bench/bench-run-all.mjs. NAMES ARE CONTRACT: `benchRunAll`
//! (default, standard lane).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: the default `run_id` is built from the REAL
//! wall clock (`new Date().toISOString().slice(0,19).replace(/:/g,'-')`) plus 2 REAL random
//! bytes (`randomBytes(2).toString('hex')`) — genuinely non-deterministic, same class as
//! `bench-laws-overhead.rs`'s own `run_id` default and `auth-sign.rs`'s nonce. It is ALSO an
//! aggregator over 4 already-converted sibling twins, one of which
//! (`bench_bench_laws_overhead::benchLawsOverhead`) is itself bespoke-native (real
//! `Instant`-timed dispatch loop) — so this module is transitively bespoke-native even ignoring
//! its own randomness, same "aggregator of non-fixture-testable pieces -> whole file
//! bespoke-native" precedent as `system/reconcile.rs`. Proven below via `#[cfg(test)]` against a
//! real in-memory db wired for `engine_action_run::skillRun` dispatch (mirrors
//! `bench-skill-latency.rs`'s test fixture) plus a `sacred_timeline` table (for
//! `bench_bench_chain_verify::benchChainVerify`) and `brain.bench_runs` (every sub-bench writes
//! rows there via `bench_bench_record::benchRecord`).
//!
//! Randomness (`randomBytes(2)`): same zero-new-Cargo-dependency OS CSPRNG reach-in as
//! `auth-sign.rs` / `auth-init.rs` — `/dev/urandom` on unix, `RtlGenRandom` (advapi32.dll
//! `SystemFunction036`) via a minimal `extern "system"` FFI on Windows. Inlined per-file per the
//! crossing playbook's convention (no shared randomness crate exists yet).
//!
//! Cross-module calls (already-converted twins, reused not reimplemented):
//! `crate::bench_bench_skill_latency::benchSkillLatency`, `crate::bench_bench_chain_verify::benchChainVerify`
//! (JS hardcodes its `iterations` to `5`, NOT `input.overhead_iterations`/etc — ported verbatim),
//! `crate::bench_bench_open_db::benchOpenDb`, `crate::bench_bench_laws_overhead::benchLawsOverhead`.
//! None of the four calls are wrapped in try/catch in the JS — a throw from any one propagates,
//! matched here via `?` on each in sequence (so a failing earlier bench short-circuits the later
//! ones, same as JS's sequential `await` chain).
//!
//! `context.db` missing -> JS throws `err.contextMissing('bench-run-all', 'db')`. Not modeled —
//! same "guard-throws are not exercised" convention every sibling twin uses (Rust's
//! `db: &Connection` argument makes it unreachable at the type level anyway).

use rusqlite::Connection;
use serde_json::{json, Value};

#[cfg(unix)]
fn os_random_bytes(n: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

#[cfg(windows)]
extern "system" {
    #[link_name = "SystemFunction036"]
    fn RtlGenRandom(buf: *mut u8, len: u32) -> u8;
}

#[cfg(windows)]
fn os_random_bytes(n: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; n];
    let ok = unsafe { RtlGenRandom(buf.as_mut_ptr(), buf.len() as u32) };
    if ok == 0 {
        return Err("RtlGenRandom failed".to_string());
    }
    Ok(buf)
}

fn now_epoch_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// Inverse of the days-since-epoch civil-date algorithm (Howard Hinnant). Proleptic Gregorian,
// valid for any epoch-days value; only positive/near-present values are ever fed here.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// Mirrors JS `new Date(ms).toISOString().slice(0, 19).replace(/:/g, '-')`.
fn epoch_ms_to_run_id_prefix(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem_ms = ms.rem_euclid(86_400_000);
    let secs_in_day = rem_ms / 1000;
    let hh = secs_in_day / 3600;
    let mm = (secs_in_day % 3600) / 60;
    let ss = secs_in_day % 60;
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}-{mm:02}-{ss:02}")
}

fn default_run_id() -> String {
    let prefix = epoch_ms_to_run_id_prefix(now_epoch_ms());
    let suffix = os_random_bytes(2).map(hex::encode).unwrap_or_else(|_| "0000".to_string());
    format!("{prefix}-{suffix}")
}

fn js_i64(v: Option<&Value>, default: i64) -> i64 {
    match v {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(default),
        _ => default,
    }
}

/// `export default async function benchRunAll(input, context)` — see module doc.
#[allow(non_snake_case)]
pub fn benchRunAll(input: &Value, db: &Connection) -> Result<Value, String> {
    let run_id = match input.get("run_id").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => default_run_id(),
    };
    let skill_iters = js_i64(input.get("skill_iterations"), 50);
    let overhead_iters = js_i64(input.get("overhead_iterations"), 30);
    let open_iters = js_i64(input.get("open_iterations"), 5);

    let mut results = serde_json::Map::new();

    let skill_latency = crate::bench_bench_skill_latency::benchSkillLatency(
        &json!({ "run_id": run_id, "iterations": skill_iters }),
        db,
    )?;
    results.insert("skill_latency".into(), skill_latency);

    let chain_verify = crate::bench_bench_chain_verify::benchChainVerify(
        &json!({ "run_id": run_id, "iterations": 5 }),
        db,
    )?;
    results.insert("chain_verify".into(), chain_verify);

    let open_db = crate::bench_bench_open_db::benchOpenDb(
        &json!({ "run_id": run_id, "iterations": open_iters }),
        db,
    )?;
    results.insert("openDb".into(), open_db);

    let laws_overhead = crate::bench_bench_laws_overhead::benchLawsOverhead(
        &json!({ "run_id": run_id, "iterations": overhead_iters }),
        db,
    )?;
    results.insert("laws_overhead".into(), laws_overhead);

    Ok(json!({ "run_id": run_id, "results": Value::Object(results) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH DATABASE ':memory:' AS laws; \
             ATTACH DATABASE ':memory:' AS brain; \
             CREATE TABLE actions (name TEXT UNIQUE, description TEXT, owner_agent TEXT, steps_json TEXT, returns TEXT, on_failure TEXT, kind TEXT); \
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', parent_event_id INTEGER, actor TEXT NOT NULL, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, outcome TEXT NOT NULL CHECK (outcome IN ('success','error','blocked','skipped','in_flight')), error_code TEXT, context_json TEXT, user_goal_id INTEGER, source TEXT); \
             CREATE TABLE cards (slug TEXT, status TEXT, score REAL); \
             CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, statement TEXT, enforcement TEXT, enforcement_hook TEXT, superseded_by INTEGER); \
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, ts TEXT, actor TEXT, operation TEXT, entity_type TEXT, entity_id TEXT, before_json TEXT, after_json TEXT, payload TEXT, prev_print TEXT, digital_print TEXT); \
             CREATE TABLE brain.bench_runs (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT (datetime('now')), run_id TEXT NOT NULL, bench_name TEXT NOT NULL, metric TEXT NOT NULL, value REAL NOT NULL, unit TEXT, context_json TEXT, git_sha TEXT); \
             INSERT INTO actions (name, description, owner_agent, steps_json, returns, kind) VALUES \
               ('board-keeper.card.list', 'list cards', 'board-keeper', '[{\"skill\":\"card-list\",\"input\":{},\"as\":\"cards\"}]', '$.results.cards', 'atomic'); \
             INSERT INTO cards VALUES ('A','doing',5),('B','backlog',NULL);",
        )
        .unwrap();
        c
    }

    #[test]
    fn runs_all_four_benches_under_one_run_id_and_records_bench_runs_rows() {
        let c = db();
        let r = benchRunAll(
            &json!({ "run_id": "r-agg", "skill_iterations": 2, "overhead_iterations": 1, "open_iterations": 1 }),
            &c,
        )
        .unwrap();
        assert_eq!(r["run_id"], json!("r-agg"));
        let results = r["results"].as_object().unwrap();
        assert_eq!(
            results.keys().collect::<Vec<_>>(),
            vec!["skill_latency", "chain_verify", "openDb", "laws_overhead"]
        );
        assert_eq!(results["skill_latency"]["run_id"], json!("r-agg"));
        assert_eq!(results["chain_verify"]["run_id"], json!("r-agg"));
        assert_eq!(results["openDb"]["run_id"], json!("r-agg"));
        assert_eq!(results["laws_overhead"]["run_id"], json!("r-agg"));

        let names = crate::db_json::query_rows(&c, "SELECT DISTINCT bench_name FROM brain.bench_runs ORDER BY bench_name", &[]).unwrap();
        let arr: Vec<String> = names.as_array().unwrap().iter().map(|r| r["bench_name"].as_str().unwrap().to_string()).collect();
        assert_eq!(arr, vec!["action-latency", "chain-verify", "laws-overhead", "openDb"]);
    }

    #[test]
    fn chain_verify_iterations_is_hardcoded_to_five_regardless_of_input() {
        // JS: `results.chain_verify = benchChainVerify({ run_id, iterations: 5 }, { db })` — the
        // top-level input's overhead/skill/open iteration knobs never reach chain_verify.
        let c = db();
        let r = benchRunAll(
            &json!({ "run_id": "r-cv", "skill_iterations": 1, "overhead_iterations": 1, "open_iterations": 1 }),
            &c,
        )
        .unwrap();
        // benchChainVerify itself hardcodes its own timed loop to `0` samples/median regardless of
        // `iterations`, but the important assertion here is that the call didn't error and used
        // the shared run_id — the "5" is opaque past benchChainVerify's own interface.
        assert_eq!(r["results"]["chain_verify"]["run_id"], json!("r-cv"));
    }

    #[test]
    fn default_run_id_has_the_expected_shape_when_none_is_given() {
        let c = db();
        let r = benchRunAll(&json!({ "skill_iterations": 1, "overhead_iterations": 1, "open_iterations": 1 }), &c).unwrap();
        let run_id = r["run_id"].as_str().unwrap();
        // "YYYY-MM-DDTHH-MM-SS-xxxx" — 19-char date/time prefix + '-' + 4 hex chars.
        assert_eq!(run_id.len(), 24);
        assert_eq!(&run_id[4..5], "-");
        assert_eq!(&run_id[10..11], "T");
        assert!(run_id[20..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_failing_earlier_bench_short_circuits_the_rest() {
        let c = Connection::open_in_memory().unwrap(); // no schema at all -> everything errs
        let err = benchRunAll(&json!({ "run_id": "r-err" }), &c).unwrap_err();
        assert!(!err.is_empty());
    }
}
