//! card-metrics.rs — twin of skills/system/card-metrics.mjs (db lane, cross-db brain/laws/judgments).
//! NAMES ARE CONTRACT: `cardMetrics` (default). Per-card cost/tokens/commits/lock rollup for the
//! waterfall view's right sidebar: AI-call cost attribution by the card's run session
//! (cards.openrouter_session_id), a memoized async git commit-count probe, sandbox-lock state via
//! real fs, a bounded recent-window drift scan (law_violations + laws.laws), the latest
//! non-superseded contract, the latest driver tick, and the most recent FAILING court's findings
//! (judgments.judgment_cases / judgment_items).
//!
//! `slug` guard-throw narrows to "must be a non-empty string" (card-run-preflight.rs precedent —
//! real callers always pass a TEXT PRIMARY KEY value; a truthy non-string slug would in real JS
//! proceed to a query that structurally can't match any row, landing on the SAME NOT_FOUND throw
//! this twin's `card not found` path already models, so the divergence is unobservable in
//! practice). `card not found` mirrors `err.notFound('card-metrics','card',slug)` ->
//! "card-metrics: card '<slug>' not found". Neither guard is fixture-tested (throw-fidelity
//! gotcha #9).
//!
//! COMMIT COUNT — real-but-deterministically-observed subprocess (playbook lesson 5's
//! flag-skippable pattern, applied to an async fire-and-forget branch instead of an input flag):
//! `commitCountFor` in the JS mirrors a module-level `Map` cache + a background `exec('git
//! log ...')` that NEVER blocks the synchronous return — the function always returns the
//! CURRENTLY-CACHED value (0 on a cold cache) and kicks off a refresh whose result lands only on a
//! LATER call. Since conform-db invokes this unit as one synchronous call per fixture case, the
//! refresh thread cannot possibly complete before the return statement executes — the observed
//! `commit_count` is deterministically `0` for every fixture case, matching real JS's own
//! "first-ever call returns 0" behavior. This twin ports the ENTIRE cache + background-thread +
//! real `git log --all --no-merges --grep=<safe> --oneline` subprocess for production parity
//! (spawned via `std::thread`, mirroring the JS event-loop callback), proven for real by the
//! native `commit_count_for_learns_from_a_background_git_probe` test below (which polls past the
//! real subprocess's completion and observes a SECOND call pick up the cached value) — not by the
//! conform-db fixture, which can only ever observe the cold-cache 0. Same `execSync`→
//! `Command`-args-not-shell-string choice as card-commits.rs (avoids a shell-injection surface the
//! JS's one interpolated string carries; the resulting argv is identical once the shell's own
//! quote-stripping is accounted for).
//!
//! LOCK STATE — real fs (existsSync/statSync-equivalent) against `SUBSTRATE_ROOT/.antiloki/sandbox/
//! <slug>` (roots.mjs SANDBOX_DIR, inlined per the sandbox-resolve-path.rs/card-commits.rs
//! `substrate_root()` convention). No fixture case's slug has a real sandbox directory on disk, so
//! every fixture case deterministically observes `lock_state:"open"`/`lock_age_seconds:null` — the
//! `locked` branch is real code, disclosed-untested-by-fixture, proven natively below via
//! ANTILOKI_SUBSTRATE_ROOT + tempdir injection (sandbox-resolve-path.rs `with_home` precedent).
//!
//! `drift_count`/`drift_recent`, `contract`, `last_tick`, `failure_*`: each wrapped in the JS's own
//! `try { ... } catch {}` — ported as a query attempt that leaves the PRE-INITIALIZED defaults in
//! place on any `Err`. The failure block's TWO queries share ONE JS try block: a throw from the
//! second (judgment_items) query is caught WITHOUT undoing the first query's already-assigned
//! failure_scope/verdict/goal/counts — modeled here by leaving those four already-set while
//! `failure_findings` alone stays at its initial `[]` (matches JS's statement-order assignment,
//! not a rolled-back transaction).
// QUALITY: real fs + real subprocess (both disclosed, native-tested) + pure db reads

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

const OPUS_INPUT_PER_1M: f64 = 15.0;
const OPUS_OUTPUT_PER_1M: f64 = 75.0;
// Cache TTL + lock age: both are DURATIONS against now, so a pinned instant makes the TTL
// never expire and the lock age meaningless off the frozen lane.
fn now_ms() -> i64 { crate::clock::now_ms() }

// JS `+x.toFixed(d)`: collapses to an integer Value when the rounded result is whole
// (JSON.stringify(5.0) === "5"), matching cost-aggregate.rs's identically-named helper.
fn to_fixed_num(x: f64, d: usize) -> Value {
    let p: f64 = format!("{:.*}", d, x).parse().unwrap();
    if p.fract() == 0.0 && p.abs() < 9.007_199_254_740_992e15 {
        json!(p as i64)
    } else {
        json!(p)
    }
}

// roots.mjs: SUBSTRATE_ROOT = resolve($ANTILOKI_SUBSTRATE_ROOT) if set+truthy, else
// resolve(ENGINE_ROOT, '..') anchored at roots.mjs's own file location. CARGO_MANIFEST_DIR here is
// antiloki-core-rust; its parent is that same SUBSTRATE_ROOT on this checkout layout
// (card-commits.rs / sandbox-resolve-path.rs precedent).
fn substrate_root() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .to_path_buf()
}
// roots.mjs: SANDBOX_DIR = join(SUBSTRATE_ROOT, '.antiloki', 'sandbox').
fn sandbox_dir() -> PathBuf {
    substrate_root().join(".antiloki").join("sandbox")
}

// ── memoized async git commit-count probe — see module doc ─────────────────────────────
struct CommitCountEntry {
    value: i64,
    at: i64,
}

fn commit_count_cache() -> &'static Mutex<HashMap<String, CommitCountEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CommitCountEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}
fn commit_count_inflight() -> &'static Mutex<HashSet<String>> {
    static INFLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    INFLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}
// `Number(process.env.ANTILOKI_COMMIT_COUNT_TTL_MS ?? 600_000)`.
fn commit_count_ttl_ms() -> f64 {
    match std::env::var("ANTILOKI_COMMIT_COUNT_TTL_MS") {
        Ok(v) => {
            let t = v.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse().unwrap_or(f64::NAN)
            }
        }
        Err(_) => 600_000.0,
    }
}

// `function refreshCommitCount(slug)`.
fn refresh_commit_count(slug: &str) {
    if commit_count_inflight().lock().unwrap().contains(slug) {
        return;
    }
    // `String(slug).replace(/[^A-Z0-9_-]/g, '')` — case-SENSITIVE (lowercase stripped too).
    let safe: String = slug
        .chars()
        .filter(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
        .collect();
    if safe.is_empty() {
        return;
    }
    commit_count_inflight().lock().unwrap().insert(slug.to_string());

    let slug_owned = slug.to_string();
    let root = substrate_root();
    std::thread::spawn(move || {
        let output = crate::proc::cmd("git")
            .arg("log")
            .arg("--all")
            .arg("--no-merges")
            .arg(format!("--grep={safe}"))
            .arg("--oneline")
            .current_dir(&root)
            .output();
        commit_count_inflight().lock().unwrap().remove(&slug_owned);
        let stdout = match output {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            _ => return, // gitErr — keep the prior/zero value, cache untouched
        };
        let value = if stdout.is_empty() {
            0
        } else {
            stdout.split('\n').filter(|l| !l.is_empty()).count() as i64
        };
        commit_count_cache()
            .lock()
            .unwrap()
            .insert(slug_owned, CommitCountEntry { value, at: now_ms() });
    });
}

// `function commitCountFor(slug)`.
fn commit_count_for(slug: &str) -> i64 {
    let ttl = commit_count_ttl_ms();
    // captured ONCE, before refresh — mirrors JS's `hit` local, which the async refresh callback
    // cannot mutate before this synchronous function returns.
    let hit = commit_count_cache()
        .lock()
        .unwrap()
        .get(slug)
        .map(|e| (e.value, e.at));
    if let Some((value, at)) = hit {
        if ((now_ms() - at) as f64) < ttl {
            return value;
        }
    }
    refresh_commit_count(slug);
    hit.map(|(v, _)| v).unwrap_or(0)
}

// `function parseStructured(v)` — preserves the original JSON structure (objects stay objects).
fn parse_structured(v: Option<&Value>) -> Value {
    match v {
        None | Some(Value::Null) => json!([]),
        Some(Value::String(s)) => serde_json::from_str::<Value>(s)
            .ok()
            .filter(Value::is_array)
            .unwrap_or(json!([])),
        Some(other) => {
            if other.is_array() {
                other.clone()
            } else {
                json!([])
            }
        }
    }
}

/// `export default function cardMetrics(input, context)`
pub fn cardMetrics(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing(...)` — unreachable in
    // the db lane (a connection is always injected).
    let slug_str = match input.get("slug").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => return Err("card-metrics: slug is required".into()),
    };

    let card_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, driver_intent FROM cards WHERE slug = ?",
        &[json!(slug_str.clone())],
    )?;
    let card = match card_rows.as_array().and_then(|a| a.first()).cloned() {
        Some(c) => c,
        None => return Err(format!("card-metrics: card '{slug_str}' not found")),
    };

    let cost_rows = crate::db_json::query_rows(
        db,
        "SELECT \
           COALESCE(SUM(cost_usd_actual), 0) AS cost_actual, \
           COALESCE(SUM(tokens_in), 0)       AS tokens_in, \
           COALESCE(SUM(tokens_out), 0)      AS tokens_out, \
           COUNT(*)                          AS calls, \
           COALESCE(SUM(CASE WHEN cached=1 THEN 1 ELSE 0 END), 0) AS cached \
         FROM brain.ai_calls \
         WHERE session_id = (SELECT openrouter_session_id FROM cards WHERE slug = ?)",
        &[json!(slug_str.clone())],
    )?;
    let cost_row = cost_rows.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
    let tokens_in = cost_row.get("tokens_in").and_then(Value::as_i64).unwrap_or(0);
    let tokens_out = cost_row.get("tokens_out").and_then(Value::as_i64).unwrap_or(0);
    let calls = cost_row.get("calls").and_then(Value::as_i64).unwrap_or(0);
    let cached = cost_row.get("cached").and_then(Value::as_i64).unwrap_or(0);
    let cost_actual = cost_row.get("cost_actual").and_then(Value::as_f64).unwrap_or(0.0);
    let est_baseline = (tokens_in as f64 * OPUS_INPUT_PER_1M / 1_000_000.0)
        + (tokens_out as f64 * OPUS_OUTPUT_PER_1M / 1_000_000.0);

    let commit_count = commit_count_for(&slug_str);

    let sandbox_path = sandbox_dir().join(&slug_str);
    let mut lock_state = "open";
    let mut lock_age_seconds = Value::Null;
    if let Ok(meta) = std::fs::metadata(&sandbox_path) {
        lock_state = "locked";
        if let Ok(modified) = meta.modified() {
            if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                let mtime_ms = dur.as_millis() as i64;
                lock_age_seconds = json!(((now_ms() - mtime_ms) as f64 / 1000.0).floor() as i64);
            }
        }
    }

    let (mut drift_count, mut drift_recent): (i64, Vec<Value>) = (0, Vec::new());
    if let Ok((n, recent)) = (|| -> Result<(i64, Vec<Value>), String> {
        let n_rows = crate::db_json::query_rows(
            db,
            "SELECT COUNT(*) AS n FROM law_violations \
             WHERE id >= (SELECT COALESCE(MAX(id),0) - 5000 FROM law_violations) \
               AND context_json LIKE '%' || ? || '%'",
            &[json!(slug_str.clone())],
        )?;
        let n = n_rows
            .as_array()
            .and_then(|a| a.first())
            .and_then(|r| r.get("n"))
            .and_then(Value::as_i64)
            .unwrap_or(0);

        let recent_rows = crate::db_json::query_rows(
            db,
            "SELECT lv.id, lv.attempted_at AS ts, lv.attempted_by AS actor, \
                    lv.agent_name, lv.action_name, lv.outcome, lv.context_json, \
                    l.slug AS law_slug \
             FROM law_violations lv \
             LEFT JOIN laws.laws l ON l.id = lv.law_id \
             WHERE lv.id >= (SELECT COALESCE(MAX(id),0) - 5000 FROM law_violations) \
               AND lv.context_json LIKE '%' || ? || '%' \
             ORDER BY lv.id DESC LIMIT 10",
            &[json!(slug_str.clone())],
        )?;
        let recent: Vec<Value> = recent_rows
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|r| {
                let ctx: Option<Value> = r
                    .get("context_json")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .and_then(|s| serde_json::from_str(s).ok());
                json!({
                    "id": r.get("id").cloned().unwrap_or(Value::Null),
                    "ts": r.get("ts").cloned().unwrap_or(Value::Null),
                    "actor": r.get("actor").cloned().unwrap_or(Value::Null),
                    "agent": r.get("agent_name").cloned().unwrap_or(Value::Null),
                    "action": r.get("action_name").cloned().unwrap_or(Value::Null),
                    "outcome": r.get("outcome").cloned().unwrap_or(Value::Null),
                    "law_slug": r.get("law_slug").cloned().unwrap_or(Value::Null),
                    "reason": ctx.as_ref().and_then(|c| c.get("reason")).cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        Ok((n, recent))
    })() {
        drift_count = n;
        drift_recent = recent;
    }

    let mut contract = Value::Null;
    if let Ok(Some(c)) = (|| -> Result<Option<Value>, String> {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT slug, status, tier, size, signed_by, signed_at, sandbox_path, worktree_path, \
                    dev_microtask_progress, current_attempt, \
                    briefing_md, goal_md, tl_briefing_md, tl_evaluation_md, \
                    microtasks_json, acceptance_json, files_touched_json, \
                    failure_kind, rejection_reason_md, \
                    created_at, updated_at \
             FROM contracts \
             WHERE card_slug = ? AND superseded_by IS NULL \
             ORDER BY id DESC LIMIT 1",
            &[json!(slug_str.clone())],
        )?;
        Ok(rows.as_array().and_then(|a| a.first()).cloned())
    })() {
        let microtasks = parse_structured(c.get("microtasks_json"));
        let acceptance = parse_structured(c.get("acceptance_json"));
        let files_touched = parse_structured(c.get("files_touched_json"));
        let total = microtasks.as_array().map(|a| a.len() as i64).unwrap_or(0);
        // `Number(c.dev_microtask_progress ?? 0)` — dev_microtask_progress is always an integer
        // counter column in practice; narrowed to as_i64 (disclosed, same class of narrowing as
        // sandbox-resolve-path.rs's slug-is-always-a-string assumption).
        let done = nz(c.get("dev_microtask_progress")).and_then(Value::as_i64).unwrap_or(0);
        let body_md = nz(c.get("goal_md"))
            .or_else(|| nz(c.get("briefing_md")))
            .or_else(|| nz(c.get("tl_briefing_md")))
            .cloned()
            .unwrap_or(Value::Null);
        contract = json!({
            "slug": c.get("slug").cloned().unwrap_or(Value::Null),
            "status": c.get("status").cloned().unwrap_or(Value::Null),
            "tier": c.get("tier").cloned().unwrap_or(Value::Null),
            "size": c.get("size").cloned().unwrap_or(Value::Null),
            "signed_by": c.get("signed_by").cloned().unwrap_or(Value::Null),
            "signed_at": c.get("signed_at").cloned().unwrap_or(Value::Null),
            "sandbox_path": c.get("sandbox_path").cloned().unwrap_or(Value::Null),
            "worktree_path": c.get("worktree_path").cloned().unwrap_or(Value::Null),
            "current_attempt": c.get("current_attempt").cloned().unwrap_or(Value::Null),
            "microtask_progress": { "done": done, "total": total, "tasks": microtasks },
            "acceptance": acceptance,
            "files_touched": files_touched,
            "body_md": body_md,
            "tl_evaluation_md": nz(c.get("tl_evaluation_md")).cloned().unwrap_or(Value::Null),
            "created_at": c.get("created_at").cloned().unwrap_or(Value::Null),
            "updated_at": c.get("updated_at").cloned().unwrap_or(Value::Null),
            "failure_kind": nz(c.get("failure_kind")).cloned().unwrap_or(Value::Null),
            "rejection_reason_md": nz(c.get("rejection_reason_md")).cloned().unwrap_or(Value::Null),
        });
    }

    let last_tick = crate::db_json::query_rows(
        db,
        "SELECT id, started_at, ended_at, decided_agent, decided_action, \
                decided_reason, outcome, error_message, cost_usd, duration_ms \
         FROM brain.driver_ticks \
         WHERE card_slug = ? \
         ORDER BY id DESC LIMIT 1",
        &[json!(slug_str.clone())],
    )
    .ok()
    .and_then(|rows| rows.as_array().and_then(|a| a.first()).cloned())
    .unwrap_or(Value::Null);

    let mut failure_findings: Vec<Value> = Vec::new();
    let mut failure_scope = Value::Null;
    let mut failure_verdict = Value::Null;
    let mut failure_goal = Value::Null;
    let mut failure_counts = Value::Null;
    if let Ok(Some(fc)) = (|| -> Result<Option<Value>, String> {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT judgment_id, scope_kind, verdict, goal_line, n_items, defender_trues, prosecutor_trues \
             FROM judgments.judgment_cases WHERE card_slug = ? AND verdict IN ('fail','invalid') \
             ORDER BY rowid DESC LIMIT 1",
            &[json!(slug_str.clone())],
        )?;
        Ok(rows.as_array().and_then(|a| a.first()).cloned())
    })() {
        failure_scope = fc.get("scope_kind").cloned().unwrap_or(Value::Null);
        failure_verdict = fc.get("verdict").cloned().unwrap_or(Value::Null);
        failure_goal = fc.get("goal_line").cloned().unwrap_or(Value::Null);
        failure_counts = json!({
            "defects_upheld": fc.get("prosecutor_trues").cloned().unwrap_or(Value::Null),
            "sufficiency_upheld": fc.get("defender_trues").cloned().unwrap_or(Value::Null),
            "items": fc.get("n_items").cloned().unwrap_or(Value::Null),
        });
        // Second query shares the JS's ONE try block — a throw here leaves failure_findings at
        // its initial [] WITHOUT undoing the four assignments above (see module doc).
        if let Ok(items_rows) = crate::db_json::query_rows(
            db,
            "SELECT item_id, statement, judge_why, judge_accuracy \
             FROM judgments.judgment_items \
             WHERE judgment_id = ? AND side = 'prosecutor' AND judge_value = 1 \
             ORDER BY judge_accuracy DESC, rowid ASC",
            &[fc.get("judgment_id").cloned().unwrap_or(Value::Null)],
        ) {
            failure_findings = items_rows
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|r| {
                    json!({
                        "item_id": r.get("item_id").cloned().unwrap_or(Value::Null),
                        "problem": r.get("statement").cloned().unwrap_or(Value::Null),
                        "why": r.get("judge_why").cloned().unwrap_or(Value::Null),
                        "confidence": r.get("judge_accuracy").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect();
        }
    }

    Ok(json!({
        "slug": slug_str,
        "cost_usd_actual": to_fixed_num(cost_actual, 4),
        "cost_usd_estimated_baseline": to_fixed_num(est_baseline, 4),
        "tokens_in": tokens_in,
        "tokens_out": tokens_out,
        "tokens_total": tokens_in + tokens_out,
        "ai_calls_count": calls,
        "cached_calls": cached,
        "commit_count": commit_count,
        "driver_intent": card.get("driver_intent").cloned().unwrap_or(Value::Null),
        "lock_state": lock_state,
        "lock_age_seconds": lock_age_seconds,
        "drift_count": drift_count,
        "drift_recent": drift_recent,
        "contract": contract,
        "last_tick": last_tick,
        "failure_findings": failure_findings,
        "failure_scope": failure_scope,
        "failure_verdict": failure_verdict,
        "failure_goal": failure_goal,
        "failure_counts": failure_counts,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    const SCHEMA: &str = "
        CREATE TABLE cards (slug TEXT PRIMARY KEY, driver_intent TEXT, openrouter_session_id TEXT);
        CREATE TABLE law_violations (id INTEGER PRIMARY KEY AUTOINCREMENT, attempted_at TEXT, attempted_by TEXT, agent_name TEXT, action_name TEXT, outcome TEXT, context_json TEXT, law_id INTEGER);
        CREATE TABLE contracts (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT, card_slug TEXT, superseded_by INTEGER, status TEXT, tier TEXT, size TEXT, signed_by TEXT, signed_at TEXT, sandbox_path TEXT, worktree_path TEXT, dev_microtask_progress INTEGER, current_attempt INTEGER, briefing_md TEXT, goal_md TEXT, tl_briefing_md TEXT, tl_evaluation_md TEXT, microtasks_json TEXT, acceptance_json TEXT, files_touched_json TEXT, failure_kind TEXT, rejection_reason_md TEXT, created_at TEXT, updated_at TEXT);
        CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT, cost_usd_actual REAL, tokens_in INTEGER, tokens_out INTEGER, cached INTEGER);
        CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY AUTOINCREMENT, card_slug TEXT, started_at TEXT, ended_at TEXT, decided_agent TEXT, decided_action TEXT, decided_reason TEXT, outcome TEXT, error_message TEXT, cost_usd REAL, duration_ms REAL);
        CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT);
        CREATE TABLE judgments.judgment_cases (judgment_id INTEGER, card_slug TEXT, scope_kind TEXT, verdict TEXT, goal_line TEXT, n_items INTEGER, defender_trues INTEGER, prosecutor_trues INTEGER);
        CREATE TABLE judgments.judgment_items (judgment_id INTEGER, item_id TEXT, statement TEXT, judge_why TEXT, judge_accuracy REAL, side TEXT, judge_value INTEGER);
    ";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain; ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments;").unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    #[test]
    fn missing_slug_and_card_not_found_are_errors() {
        let c = db();
        assert_eq!(cardMetrics(&json!({}), &c), Err("card-metrics: slug is required".to_string()));
        assert_eq!(
            cardMetrics(&json!({ "slug": "GHOST" }), &c),
            Err("card-metrics: card 'GHOST' not found".to_string())
        );
    }

    #[test]
    fn full_rollup_shape_across_every_join() {
        let c = db();
        let slug = "ZTEST-CARD-METRICS-FULL-1";
        c.execute(
            "INSERT INTO cards (slug, driver_intent, openrouter_session_id) VALUES (?1, 'auto', 'sess-1')",
            rusqlite::params![slug],
        )
        .unwrap();
        c.execute(
            "INSERT INTO brain.ai_calls (session_id, cost_usd_actual, tokens_in, tokens_out, cached) VALUES ('sess-1', 0.1234, 200, 100, 0)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO brain.ai_calls (session_id, cost_usd_actual, tokens_in, tokens_out, cached) VALUES ('other-sess', 50, 500, 500, 1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO law_violations (attempted_at, attempted_by, agent_name, action_name, outcome, context_json, law_id) VALUES ('2025-12-20 00:00:00', 'dev-agent', 'dev', 'dev.contract.load', 'blocked', ?1, 10)",
            rusqlite::params![format!(r#"{{"reason":"needs {slug} fix"}}"#)],
        )
        .unwrap();
        c.execute(
            "INSERT INTO law_violations (attempted_at, attempted_by, agent_name, action_name, outcome, context_json, law_id) VALUES ('2025-12-21 00:00:00', 'dev-agent', 'dev', 'dev.other', 'blocked', '{\"reason\":\"unrelated\"}', NULL)",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO laws.laws (id, slug) VALUES (10, 'SOME_LAW_SLUG')", []).unwrap();
        c.execute(
            "INSERT INTO contracts (slug, card_slug, superseded_by, status, tier, size, signed_by, signed_at, sandbox_path, worktree_path, dev_microtask_progress, current_attempt, briefing_md, goal_md, tl_briefing_md, tl_evaluation_md, microtasks_json, acceptance_json, files_touched_json, failure_kind, rejection_reason_md, created_at, updated_at) \
             VALUES (?1, ?1, NULL, 'doing', 'T2', 'M', 'dev', '2025-12-01 00:00:00', NULL, NULL, 2, 1, 'briefing', 'goal text', NULL, 'evaluation', '[{\"microtask_id\":\"MT-1\",\"intent\":\"do thing\"}]', '[{\"kind\":\"file-exists\",\"path\":\"x\"}]', '[\"src/x.mjs\"]', NULL, NULL, '2025-12-01 00:00:00', '2025-12-02 00:00:00')",
            rusqlite::params![slug],
        )
        .unwrap();
        c.execute(
            "INSERT INTO brain.driver_ticks (card_slug, started_at, ended_at, decided_agent, decided_action, decided_reason, outcome, error_message, cost_usd, duration_ms) \
             VALUES (?1, '2025-12-05 00:00:00', '2025-12-05 00:05:00', 'dev', 'dev.implement', 'because', 'success', NULL, 0.02, 1200)",
            rusqlite::params![slug],
        )
        .unwrap();
        c.execute(
            "INSERT INTO judgments.judgment_cases (judgment_id, card_slug, scope_kind, verdict, goal_line, n_items, defender_trues, prosecutor_trues) VALUES (77, ?1, 'contract', 'fail', 'goal line text', 3, 1, 2)",
            rusqlite::params![slug],
        )
        .unwrap();
        c.execute(
            "INSERT INTO judgments.judgment_items (judgment_id, item_id, statement, judge_why, judge_accuracy, side, judge_value) VALUES (77, 'IT-1', 'bug found', 'explanation', 0.9, 'prosecutor', 1)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO judgments.judgment_items (judgment_id, item_id, statement, judge_why, judge_accuracy, side, judge_value) VALUES (77, 'IT-2', 'non-upheld', 'n/a', 0.2, 'prosecutor', 0)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO judgments.judgment_items (judgment_id, item_id, statement, judge_why, judge_accuracy, side, judge_value) VALUES (77, 'IT-3', 'defender ok', 'n/a', 0.5, 'defender', 1)",
            [],
        )
        .unwrap();

        let r = cardMetrics(&json!({ "slug": slug }), &c).unwrap();
        assert_eq!(r["slug"], json!(slug));
        assert_eq!(r["cost_usd_actual"], json!(0.1234));
        assert_eq!(r["cost_usd_estimated_baseline"], json!(0.0105));
        assert_eq!(r["tokens_in"], json!(200));
        assert_eq!(r["tokens_out"], json!(100));
        assert_eq!(r["tokens_total"], json!(300));
        assert_eq!(r["ai_calls_count"], json!(1));
        assert_eq!(r["cached_calls"], json!(0));
        assert_eq!(r["commit_count"], json!(0)); // cold cache — see module doc
        assert_eq!(r["driver_intent"], json!("auto"));
        assert_eq!(r["lock_state"], json!("open"));
        assert_eq!(r["lock_age_seconds"], Value::Null);
        assert_eq!(r["drift_count"], json!(1));
        assert_eq!(
            r["drift_recent"],
            json!([{
                "id": 1, "ts": "2025-12-20 00:00:00", "actor": "dev-agent", "agent": "dev",
                "action": "dev.contract.load", "outcome": "blocked", "law_slug": "SOME_LAW_SLUG",
                "reason": format!("needs {slug} fix"),
            }])
        );
        assert_eq!(r["contract"]["status"], json!("doing"));
        assert_eq!(r["contract"]["body_md"], json!("goal text"));
        assert_eq!(r["contract"]["microtask_progress"], json!({ "done": 2, "total": 1, "tasks": [{"microtask_id":"MT-1","intent":"do thing"}] }));
        assert_eq!(r["contract"]["acceptance"], json!([{"kind":"file-exists","path":"x"}]));
        assert_eq!(r["contract"]["files_touched"], json!(["src/x.mjs"]));
        assert_eq!(r["last_tick"]["decided_action"], json!("dev.implement"));
        assert_eq!(r["failure_scope"], json!("contract"));
        assert_eq!(r["failure_verdict"], json!("fail"));
        assert_eq!(r["failure_goal"], json!("goal line text"));
        assert_eq!(r["failure_counts"], json!({ "defects_upheld": 2, "sufficiency_upheld": 1, "items": 3 }));
        assert_eq!(
            r["failure_findings"],
            json!([{ "item_id": "IT-1", "problem": "bug found", "why": "explanation", "confidence": 0.9 }])
        );
    }

    #[test]
    fn minimal_card_defaults_every_optional_block() {
        let c = db();
        c.execute("INSERT INTO cards (slug, driver_intent, openrouter_session_id) VALUES ('ZTEST-CARD-METRICS-MIN', NULL, NULL)", []).unwrap();
        let r = cardMetrics(&json!({ "slug": "ZTEST-CARD-METRICS-MIN" }), &c).unwrap();
        assert_eq!(r["cost_usd_actual"], json!(0));
        assert_eq!(r["cost_usd_estimated_baseline"], json!(0));
        assert_eq!(r["tokens_in"], json!(0));
        assert_eq!(r["ai_calls_count"], json!(0));
        assert_eq!(r["driver_intent"], Value::Null);
        assert_eq!(r["lock_state"], json!("open"));
        assert_eq!(r["drift_count"], json!(0));
        assert_eq!(r["drift_recent"], json!([]));
        assert_eq!(r["contract"], Value::Null);
        assert_eq!(r["last_tick"], Value::Null);
        assert_eq!(r["failure_findings"], json!([]));
        assert_eq!(r["failure_scope"], Value::Null);
        assert_eq!(r["failure_counts"], Value::Null);
    }

    // ── real fs: lock_state, disclosed-untested-by-fixture branch — see module doc ──────────
    // shared crate-wide lock (test_env_lock.rs) — a local mutex would not prevent a race with
    // OTHER files' tests over the same real env var.
    fn with_substrate_root<T>(root: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", root);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-card-metrics-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lock_state_reports_locked_for_a_real_sandbox_dir() {
        let dir = temp_dir("lock");
        let slug = "ZTEST-LOCK-CARD";
        fs::create_dir_all(dir.join(".antiloki").join("sandbox").join(slug)).unwrap();

        let c = db();
        c.execute("INSERT INTO cards (slug, driver_intent, openrouter_session_id) VALUES (?1, NULL, NULL)", rusqlite::params![slug]).unwrap();

        let r = with_substrate_root(dir.to_str().unwrap(), || cardMetrics(&json!({ "slug": slug }), &c).unwrap());
        assert_eq!(r["lock_state"], json!("locked"));
        assert!(r["lock_age_seconds"].is_number(), "{r}");

        let _ = fs::remove_dir_all(&dir);
    }

    // ── real subprocess: commit_count_for, disclosed-untested-by-fixture — see module doc ──────
    fn git(args: &[&str], cwd: &Path) {
        let status = crate::proc::cmd("git").args(args).current_dir(cwd).status().unwrap();
        assert!(status.success(), "git {args:?} failed in {cwd:?}");
    }

    #[test]
    fn commit_count_for_learns_from_a_background_git_probe() {
        let dir = temp_dir("gitprobe");
        git(&["init", "-q"], &dir);
        git(&["config", "user.email", "test@example.com"], &dir);
        git(&["config", "user.name", "Test"], &dir);
        let slug = "ZCOMMITCOUNTTEST-1";
        fs::write(dir.join("a.txt"), "1").unwrap();
        git(&["add", "-A"], &dir);
        let status = crate::proc::cmd("git")
            .arg("commit")
            .arg("-m")
            .arg(format!("{slug}: initial commit"))
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(status.success());

        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_SUBSTRATE_ROOT", dir.to_str().unwrap());
        }
        assert_eq!(commit_count_for(slug), 0); // cold cache — synchronous return never blocks
        let mut got = 0;
        for _ in 0..50 {
            got = commit_count_for(slug);
            if got != 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        unsafe {
            std::env::remove_var("ANTILOKI_SUBSTRATE_ROOT");
        }
        drop(_guard);

        assert_eq!(got, 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
