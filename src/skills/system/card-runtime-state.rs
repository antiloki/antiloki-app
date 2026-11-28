//! card-runtime-state.rs — twin of skills/system/card-runtime-state.mjs. NAMES ARE CONTRACT:
//! `cardRuntimeState` (default export). Authoritative, cheap runtime snapshot for one card (the
//! FE waterfall stage ladder's data source).
//!
//! BESPOKE-NATIVE, not conform-db fixture-tested: this single exported function (the census
//! counts one default export) mixes real SQLite reads (cards / contracts / brain.driver_ticks /
//! contract_qa_reviews / swarm_manifests / pl_sessions / pl_questions / pl_answers — normally the
//! "standard" db lane) with ONE real filesystem check against a HARDCODED (non-input-injectable,
//! HOME_DIR-anchored) constant — `HALT_FILE` = `RUN_DIR/driver-halt` (roots.mjs; `RUN_DIR` =
//! `HOME_DIR/run`, not DB-derived) for the `halted` field. Same precedent as
//! skills/system/driver-status.rs (identical HALT_FILE mechanism, identical "one fs field
//! inseparable from the db-heavy rest of the function" shape): bespoke-native, no fixture/
//! frontier entry — proven here via #[cfg(test)] native tests (temp dir + `ANTILOKI_HOME` env-var
//! injection, copying the `with_home()` pattern from driver-status.rs/god-driver-halt.rs exactly).
//!
//! HOME_DIR resolution mirrors those siblings (no `dirs`-crate dependency): `ANTILOKI_HOME` env
//! override (plain path join, no cwd-resolve) else Node `os.homedir()` semantics (POSIX `$HOME`;
//! Windows `%USERPROFILE%` falling back to `%HOMEDRIVE%%HOMEPATH%`).
//!
//! `context.db` is always injected by the db lane in production, so the JS `if (!db) throw
//! err.contextMissing(...)` guard is unreachable here (Rust's `db: &Connection` is never absent) —
//! same convention as feature-inventory.rs / driver-status.rs.

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::PathBuf;

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// = JS `a?.key` optional chaining on a JSON value: Some(value) only if `a` is a plain object AND
// carries `key` (even if that value is null); None otherwise (non-object `a`, or missing key).
fn opt_prop<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v { Value::Object(m) => m.get(key), _ => None }
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
// RUN_DIR = join(HOME_DIR, 'run'); HALT_FILE = join(RUN_DIR, 'driver-halt').
fn halt_file_path() -> PathBuf {
    let home_dir = match std::env::var("ANTILOKI_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => homedir().unwrap_or_default().join(".antiloki"),
    };
    home_dir.join("run").join("driver-halt")
}

// JS: FIX-QA-FINDINGS-FULL-DISPLAY fetchReview(whereExtra) closure — one contract_qa_reviews
// lookup, `{...r, issues}` with `issues_json` deleted (spread-then-delete: `issues` lands at the
// END of the key order, after the deleted key's old slot). Any query failure (legacy DB without
// the table/column) is swallowed — mirrors the JS `catch { return null; }`.
fn fetch_review(db: &Connection, contract_slug: &Value, where_extra: &str) -> Value {
    let sql = format!(
        "SELECT id, ts, passed, verdict, failure_target, \
                COALESCE(review_mode, 'full') AS review_mode, \
                substr(COALESCE(findings_md, ''), 1, 280) AS findings_head, \
                findings_md, issues_json \
         FROM contract_qa_reviews \
         WHERE contract_slug = ? {where_extra} \
         ORDER BY id DESC LIMIT 1"
    );
    let rows = match crate::db_json::query_rows(db, &sql, &[contract_slug.clone()]) {
        Ok(v) => v,
        Err(_) => return Value::Null,
    };
    let r = match rows.as_array().and_then(|a| a.first()).cloned() {
        Some(r) => r,
        None => return Value::Null,
    };
    // JSON.parse(r.issues_json ?? '[]') ?? [] — the trailing `?? []` also catches a successfully
    // parsed literal `null` (r.issues_json === "null"), not just a parse failure.
    let raw = nz(r.get("issues_json")).and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_else(|| "[]".to_string());
    let issues = match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Null) | Err(_) => json!([]),
        Ok(v) => v,
    };
    let mut obj = r.as_object().cloned().unwrap_or_default();
    obj.insert("issues".into(), issues);
    obj.remove("issues_json");
    Value::Object(obj)
}

pub fn cardRuntimeState(input: &Value, db: &Connection) -> Result<Value, String> {
    let slug = input.get("slug").cloned();
    if !truthy(slug.as_ref()) {
        return Err("card-runtime-state: slug is required".to_string());
    }
    let slug = slug.unwrap();

    let card_rows = crate::db_json::query_rows(db, "SELECT status, driver_intent FROM cards WHERE slug = ?", &[slug.clone()])?;
    let card = match card_rows.as_array().and_then(|a| a.first()).cloned() {
        Some(c) => c,
        None => return Err(format!("card-runtime-state: card '{}' not found", tmpl(Some(&slug)))),
    };

    // contract — latest non-superseded row for this card, with last_refine peeled off the
    // append-only refine_history_json log (last entry only).
    let mut contract = Value::Null;
    let mut contract_slug: Option<Value> = None;
    if let Ok(rows) = crate::db_json::query_rows(
        db,
        "SELECT slug, status, current_attempt, dev_microtask_progress, \
                failure_kind, last_failure_kind, last_failure_md, last_failure_at, \
                refine_history_json \
         FROM contracts WHERE card_slug = ? AND superseded_by IS NULL ORDER BY id DESC LIMIT 1",
        &[slug.clone()],
    ) {
        if let Some(c) = rows.as_array().and_then(|a| a.first()).cloned() {
            let hist_raw = nz(c.get("refine_history_json")).and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_else(|| "[]".to_string());
            let last_refine = serde_json::from_str::<Value>(&hist_raw)
                .ok()
                .and_then(|h| h.as_array().filter(|a| !a.is_empty()).map(|a| a.last().unwrap().clone()))
                .unwrap_or(Value::Null);
            let mut obj = Map::new();
            obj.insert("slug".into(), c.get("slug").cloned().unwrap_or(Value::Null));
            obj.insert("status".into(), c.get("status").cloned().unwrap_or(Value::Null));
            obj.insert("current_attempt".into(), c.get("current_attempt").cloned().unwrap_or(Value::Null));
            obj.insert("dev_microtask_progress".into(), c.get("dev_microtask_progress").cloned().unwrap_or(Value::Null));
            obj.insert("failure_kind".into(), c.get("failure_kind").cloned().unwrap_or(Value::Null));
            obj.insert("last_failure_kind".into(), c.get("last_failure_kind").cloned().unwrap_or(Value::Null));
            obj.insert("last_failure_md".into(), c.get("last_failure_md").cloned().unwrap_or(Value::Null));
            obj.insert("last_failure_at".into(), c.get("last_failure_at").cloned().unwrap_or(Value::Null));
            obj.insert("last_refine".into(), last_refine);
            contract_slug = Some(c.get("slug").cloned().unwrap_or(Value::Null));
            contract = Value::Object(obj);
        }
    }

    let last_tick = crate::db_json::query_rows(
        db,
        "SELECT id, started_at, decided_action, decided_reason, outcome \
         FROM brain.driver_ticks WHERE card_slug = ? ORDER BY id DESC LIMIT 1",
        &[slug.clone()],
    )
    .ok()
    .and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
    .unwrap_or(Value::Null);

    // FIX-QA1-TRUTH / FIX-PLAN-QA-SPLIT / EPIC-SWARM: only meaningful once a contract exists.
    // latest_qa_review is the AI review only (plan|full); the deterministic gates (plan-coverage,
    // manifest-lint) each get their own field so they paint their own waterfall rung.
    let mut latest_qa_review = Value::Null;
    let mut latest_plan_coverage = Value::Null;
    let mut latest_manifest_lint = Value::Null;
    let mut swarm_manifest = Value::Null;
    if let Some(cslug) = &contract_slug {
        latest_qa_review = fetch_review(db, cslug, "AND COALESCE(review_mode, 'full') IN ('plan', 'full')");
        latest_plan_coverage = fetch_review(db, cslug, "AND review_mode = 'plan-coverage'");

        let swarm_flag = std::env::var("ANTILOKI_SWARM").unwrap_or_else(|_| "on".to_string());
        if swarm_flag.to_lowercase() != "off" {
            latest_manifest_lint = fetch_review(db, cslug, "AND review_mode = 'manifest-lint'");
            if let Ok(rows) = crate::db_json::query_rows(
                db,
                "SELECT version, goal, manifest_json, lint_passed \
                 FROM swarm_manifests WHERE contract_slug = ? AND superseded_by IS NULL \
                 ORDER BY version DESC, id DESC LIMIT 1",
                &[cslug.clone()],
            ) {
                if let Some(m) = rows.as_array().and_then(|a| a.first()).cloned() {
                    let manifest_raw = nz(m.get("manifest_json")).and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_else(|| "{}".to_string());
                    let parsed = match serde_json::from_str::<Value>(&manifest_raw) {
                        Ok(Value::Null) | Err(_) => json!({}),
                        Ok(v) => v,
                    };

                    let blocks_src: Vec<Value> = match opt_prop(&parsed, "blocks") {
                        Some(Value::Array(a)) => a.clone(),
                        _ => vec![],
                    };
                    let blocks: Vec<Value> = blocks_src
                        .iter()
                        .map(|a| {
                            let mut o = Map::new();
                            if let Some(v) = opt_prop(a, "id") { o.insert("id".into(), v.clone()); }
                            if let Some(v) = opt_prop(a, "file") { o.insert("file".into(), v.clone()); }
                            if let Some(v) = opt_prop(a, "role") { o.insert("role".into(), v.clone()); }
                            o.insert("subagent_id".into(), nz(opt_prop(a, "subagent_id")).cloned().unwrap_or(Value::Null));
                            let produces: Vec<Value> = match opt_prop(a, "produces") {
                                Some(Value::Array(parr)) => parr
                                    .iter()
                                    .map(|p| {
                                        let mut po = Map::new();
                                        if let Some(v) = opt_prop(p, "name") { po.insert("name".into(), v.clone()); }
                                        po.insert("type".into(), nz(opt_prop(p, "type")).cloned().unwrap_or(Value::Null));
                                        Value::Object(po)
                                    })
                                    .collect(),
                                _ => vec![],
                            };
                            o.insert("produces".into(), json!(produces));
                            let consumes: Vec<Value> = match opt_prop(a, "consumes") {
                                Some(Value::Array(carr)) => carr
                                    .iter()
                                    .map(|c| {
                                        let mut co = Map::new();
                                        if let Some(v) = opt_prop(c, "name") { co.insert("name".into(), v.clone()); }
                                        co.insert("type".into(), nz(opt_prop(c, "type")).cloned().unwrap_or(Value::Null));
                                        co.insert("external".into(), json!(truthy(opt_prop(c, "external"))));
                                        Value::Object(co)
                                    })
                                    .collect(),
                                _ => vec![],
                            };
                            o.insert("consumes".into(), json!(consumes));
                            Value::Object(o)
                        })
                        .collect();

                    let bindings_src: Vec<Value> = match opt_prop(&parsed, "bindings") {
                        Some(Value::Array(a)) => a.clone(),
                        _ => vec![],
                    };
                    let bindings: Vec<Value> = bindings_src
                        .iter()
                        .map(|b| {
                            let mut o = Map::new();
                            if let Some(v) = opt_prop(b, "from") { o.insert("from".into(), v.clone()); }
                            if let Some(v) = opt_prop(b, "to") { o.insert("to".into(), v.clone()); }
                            if let Some(v) = opt_prop(b, "type") { o.insert("type".into(), v.clone()); }
                            Value::Object(o)
                        })
                        .collect();

                    let mut sm = Map::new();
                    sm.insert("version".into(), m.get("version").cloned().unwrap_or(Value::Null));
                    sm.insert("goal".into(), m.get("goal").cloned().unwrap_or(Value::Null));
                    sm.insert("lint_passed".into(), m.get("lint_passed").cloned().unwrap_or(Value::Null));
                    sm.insert("blocks".into(), json!(blocks));
                    sm.insert("bindings".into(), json!(bindings));
                    swarm_manifest = Value::Object(sm);
                }
            }
        }
    }

    // awaiting_human: latest pl_session in 'clarifying' with unanswered clarify questions. The
    // card->session link column varies by migration; probe pl_sessions for whichever exists.
    let mut awaiting_human = false;
    let mut pending_questions: i64 = 0;
    if let Ok(cols_rows) = crate::db_json::query_rows(db, "PRAGMA table_info(pl_sessions)", &[]) {
        let cols: Vec<String> = cols_rows
            .as_array()
            .map(|a| a.iter().filter_map(|r| r.get("name").and_then(|v| v.as_str()).map(|s| s.to_string())).collect())
            .unwrap_or_default();
        let link_cols: Vec<&str> = ["approved_card_slug", "card_slug"].into_iter().filter(|c| cols.iter().any(|x| x == c)).collect();
        if !link_cols.is_empty() {
            let where_clause = link_cols.iter().map(|c| format!("{c} = ?")).collect::<Vec<_>>().join(" OR ");
            let params: Vec<Value> = link_cols.iter().map(|_| slug.clone()).collect();
            if let Ok(session_rows) = crate::db_json::query_rows(
                db,
                &format!("SELECT id, status FROM pl_sessions WHERE {where_clause} ORDER BY id DESC LIMIT 1"),
                &params,
            ) {
                if let Some(session) = session_rows.as_array().and_then(|a| a.first()) {
                    if session.get("status").and_then(|v| v.as_str()) == Some("clarifying") {
                        let sid = session.get("id").cloned().unwrap_or(Value::Null);
                        if let Ok(cnt_rows) = crate::db_json::query_rows(
                            db,
                            "SELECT COUNT(*) AS n FROM pl_questions q \
                             LEFT JOIN pl_answers a ON a.question_id = q.id \
                             WHERE q.session_id = ? AND q.step = 'clarify' AND a.question_id IS NULL",
                            &[sid],
                        ) {
                            let n = cnt_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("n")).and_then(Value::as_i64).unwrap_or(0);
                            pending_questions = n;
                            awaiting_human = n > 0;
                        }
                    }
                }
            }
        }
    }

    let mut out = Map::new();
    out.insert("slug".into(), slug);
    out.insert("card_status".into(), card.get("status").cloned().unwrap_or(Value::Null));
    out.insert("driver_intent".into(), card.get("driver_intent").cloned().unwrap_or(Value::Null));
    out.insert("contract".into(), contract);
    out.insert("last_tick".into(), last_tick);
    out.insert("latest_qa_review".into(), latest_qa_review);
    out.insert("latest_plan_coverage".into(), latest_plan_coverage);
    out.insert("latest_manifest_lint".into(), latest_manifest_lint);
    out.insert("swarm_manifest".into(), swarm_manifest);
    out.insert("halted".into(), json!(halt_file_path().exists()));
    out.insert("awaiting_human".into(), json!(awaiting_human));
    out.insert("pending_questions".into(), json!(pending_questions));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn with_home<T>(antiloki_home: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", antiloki_home);
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
        }
        result
    }

    fn with_home_and_swarm<T>(antiloki_home: &str, swarm: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("ANTILOKI_HOME", antiloki_home);
            match swarm {
                Some(v) => std::env::set_var("ANTILOKI_SWARM", v),
                None => std::env::remove_var("ANTILOKI_SWARM"),
            }
        }
        let result = f();
        unsafe {
            std::env::remove_var("ANTILOKI_HOME");
            std::env::remove_var("ANTILOKI_SWARM");
        }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-card-runtime-state-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH DATABASE ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT UNIQUE, title TEXT, status TEXT, driver_intent TEXT);
             CREATE TABLE contracts (id INTEGER PRIMARY KEY, slug TEXT UNIQUE, card_slug TEXT, status TEXT,
               current_attempt INTEGER, dev_microtask_progress INTEGER, failure_kind TEXT, last_failure_kind TEXT,
               last_failure_md TEXT, last_failure_at TEXT, refine_history_json TEXT, superseded_by TEXT);
             CREATE TABLE brain.driver_ticks (id INTEGER PRIMARY KEY, started_at TEXT, card_slug TEXT,
               decided_action TEXT, decided_reason TEXT, outcome TEXT);
             CREATE TABLE contract_qa_reviews (id INTEGER PRIMARY KEY, contract_slug TEXT, ts TEXT, passed INTEGER,
               verdict TEXT, failure_target TEXT, review_mode TEXT, findings_md TEXT, issues_json TEXT);
             CREATE TABLE swarm_manifests (id INTEGER PRIMARY KEY, contract_slug TEXT, version INTEGER, goal TEXT,
               manifest_json TEXT, lint_passed INTEGER, superseded_by INTEGER);
             CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY, status TEXT, approved_card_slug TEXT);
             CREATE TABLE pl_questions (id INTEGER PRIMARY KEY, session_id INTEGER, step TEXT, question_md TEXT);
             CREATE TABLE pl_answers (id INTEGER PRIMARY KEY, question_id INTEGER, selected_option_id INTEGER, writein_md TEXT);",
        )
        .unwrap();
        c
    }

    fn insert_card(c: &Connection, slug: &str, status: &str, driver_intent: Option<&str>) {
        c.execute(
            "INSERT INTO cards (slug, title, status, driver_intent) VALUES (?1, 'card', ?2, ?3)",
            rusqlite::params![slug, status, driver_intent],
        )
        .unwrap();
    }

    #[test]
    fn missing_slug_returns_required_error() {
        let db = make_db();
        let dir = temp_dir("missing-slug");
        let r = with_home(dir.to_str().unwrap(), || cardRuntimeState(&json!({}), &db));
        assert_eq!(r, Err("card-runtime-state: slug is required".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_card_returns_not_found_error() {
        let db = make_db();
        let dir = temp_dir("unknown-card");
        let r = with_home(dir.to_str().unwrap(), || cardRuntimeState(&json!({ "slug": "NOPE" }), &db));
        assert_eq!(r, Err("card-runtime-state: card 'NOPE' not found".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn card_without_contract_or_ticks_returns_nulls_not_a_throw() {
        let db = make_db();
        insert_card(&db, "FEAT-RT-BARE", "backlog", None);
        let dir = temp_dir("bare");
        let r = with_home(dir.to_str().unwrap(), || cardRuntimeState(&json!({ "slug": "FEAT-RT-BARE" }), &db).unwrap());
        assert_eq!(r["contract"], Value::Null);
        assert_eq!(r["last_tick"], Value::Null);
        assert_eq!(r["awaiting_human"], json!(false));
        assert_eq!(r["pending_questions"], json!(0));
        let keys: Vec<&String> = r.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "slug", "card_status", "driver_intent", "contract", "last_tick", "latest_qa_review",
                "latest_plan_coverage", "latest_manifest_lint", "swarm_manifest", "halted", "awaiting_human",
                "pending_questions"
            ]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn returns_flat_snapshot_card_plus_latest_contract_plus_last_tick() {
        let db = make_db();
        insert_card(&db, "FEAT-RUNTIME-STATE-CARD", "doing", Some("auto"));
        db.execute(
            "INSERT INTO contracts (slug, card_slug, status, current_attempt, dev_microtask_progress, failure_kind, refine_history_json)
             VALUES ('CONTRACT-RT-1', 'FEAT-RUNTIME-STATE-CARD', 'impl', 2, 3, NULL,
               '[{\"mode\":\"sliced\",\"kept_ids\":[\"a\"],\"replaced_ids\":[\"b\"]},{\"mode\":\"full\",\"kept_ids\":[],\"replaced_ids\":[\"a\",\"b\"]}]')",
            [],
        ).unwrap();
        db.execute(
            "INSERT INTO brain.driver_ticks (started_at, card_slug, decided_action, decided_reason, outcome)
             VALUES ('2026-06-01 00:00:04', 'FEAT-RUNTIME-STATE-CARD', 'git-manager.implement.code', 'rule-x: impl in progress', 'fired')",
            [],
        ).unwrap();
        let dir = temp_dir("flat-snapshot");
        let r = with_home(dir.to_str().unwrap(), || cardRuntimeState(&json!({ "slug": "FEAT-RUNTIME-STATE-CARD" }), &db).unwrap());
        assert_eq!(r["slug"], json!("FEAT-RUNTIME-STATE-CARD"));
        assert_eq!(r["card_status"], json!("doing"));
        assert_eq!(r["driver_intent"], json!("auto"));
        assert_eq!(r["contract"]["slug"], json!("CONTRACT-RT-1"));
        assert_eq!(r["contract"]["status"], json!("impl"));
        assert_eq!(r["contract"]["current_attempt"], json!(2));
        assert_eq!(r["contract"]["dev_microtask_progress"], json!(3));
        assert_eq!(r["contract"]["failure_kind"], Value::Null);
        // only the LAST refine_history entry surfaces
        assert_eq!(r["contract"]["last_refine"], json!({ "mode": "full", "kept_ids": [], "replaced_ids": ["a", "b"] }));
        assert_eq!(r["last_tick"]["decided_action"], json!("git-manager.implement.code"));
        assert_eq!(r["last_tick"]["outcome"], json!("fired"));
        assert!(r["halted"].is_boolean());
        assert_eq!(r["awaiting_human"], json!(false));
        assert_eq!(r["pending_questions"], json!(0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn superseded_contracts_are_skipped_latest_non_superseded_wins() {
        let db = make_db();
        insert_card(&db, "FEAT-SUPERSEDE", "doing", None);
        db.execute(
            "INSERT INTO contracts (slug, card_slug, status, current_attempt, superseded_by) VALUES ('CONTRACT-RT-1', 'FEAT-SUPERSEDE', 'impl', 2, 'CONTRACT-RT-2')",
            [],
        ).unwrap();
        db.execute(
            "INSERT INTO contracts (slug, card_slug, status, current_attempt) VALUES ('CONTRACT-RT-2', 'FEAT-SUPERSEDE', 'proposed', 3)",
            [],
        ).unwrap();
        let dir = temp_dir("superseded");
        let r = with_home(dir.to_str().unwrap(), || cardRuntimeState(&json!({ "slug": "FEAT-SUPERSEDE" }), &db).unwrap());
        assert_eq!(r["contract"]["slug"], json!("CONTRACT-RT-2"));
        assert_eq!(r["contract"]["status"], json!("proposed"));
        assert_eq!(r["contract"]["last_refine"], Value::Null);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_refine_history_json_yields_null_last_refine() {
        let db = make_db();
        insert_card(&db, "FEAT-BAD-HIST", "doing", None);
        db.execute(
            "INSERT INTO contracts (slug, card_slug, refine_history_json) VALUES ('C-1', 'FEAT-BAD-HIST', 'not json')",
            [],
        ).unwrap();
        let dir = temp_dir("bad-hist");
        let r = with_home(dir.to_str().unwrap(), || cardRuntimeState(&json!({ "slug": "FEAT-BAD-HIST" }), &db).unwrap());
        assert_eq!(r["contract"]["last_refine"], Value::Null);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn latest_qa_review_surfaces_plan_or_full_reviews_only() {
        let db = make_db();
        insert_card(&db, "QR-1", "doing", None);
        db.execute("INSERT INTO contracts (slug, card_slug, status) VALUES ('QR-1-C1', 'QR-1', 'pl_ratified')", []).unwrap();
        db.execute(
            "INSERT INTO contract_qa_reviews (contract_slug, passed, verdict, failure_target, review_mode, findings_md)
             VALUES ('QR-1-C1', 0, 'fail', 'contract', 'plan', 'AC#7 is impossible: timeout-killed dev server returns 124, never 0.')",
            [],
        ).unwrap();
        let dir = temp_dir("qa-review");
        let r = with_home(dir.to_str().unwrap(), || cardRuntimeState(&json!({ "slug": "QR-1" }), &db).unwrap());
        assert_eq!(r["latest_qa_review"]["passed"], json!(0));
        assert_eq!(r["latest_qa_review"]["review_mode"], json!("plan"));
        assert_eq!(r["latest_qa_review"]["failure_target"], json!("contract"));
        assert!(r["latest_qa_review"]["findings_head"].as_str().unwrap().contains("impossible"));
        assert!(r["latest_qa_review"].as_object().unwrap().contains_key("issues"));
        assert!(!r["latest_qa_review"].as_object().unwrap().contains_key("issues_json"));
        assert_eq!(r["latest_qa_review"]["issues"], json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_coverage_review_is_separate_from_qa_review() {
        let db = make_db();
        insert_card(&db, "QR-2", "doing", None);
        db.execute("INSERT INTO contracts (slug, card_slug, status) VALUES ('QR-2-C1', 'QR-2', 'pl_ratified')", []).unwrap();
        db.execute(
            "INSERT INTO contract_qa_reviews (contract_slug, passed, verdict, failure_target, review_mode, issues_json)
             VALUES ('QR-2-C1', 1, 'pass', 'microtasks', 'plan-coverage', '[{\"severity\":\"low\"}]')",
            [],
        ).unwrap();
        let dir = temp_dir("plan-coverage");
        let r = with_home(dir.to_str().unwrap(), || cardRuntimeState(&json!({ "slug": "QR-2" }), &db).unwrap());
        assert_eq!(r["latest_qa_review"], Value::Null);
        assert_eq!(r["latest_plan_coverage"]["review_mode"], json!("plan-coverage"));
        assert_eq!(r["latest_plan_coverage"]["issues"], json!([{ "severity": "low" }]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn swarm_manifest_surfaces_blocks_and_bindings_when_flag_on() {
        let db = make_db();
        insert_card(&db, "SW-1", "doing", None);
        db.execute("INSERT INTO contracts (slug, card_slug, status) VALUES ('SW-1-C1', 'SW-1', 'pl_ratified')", []).unwrap();
        let manifest = json!({
            "blocks": [
                { "id": "b1", "file": "src/a.ts", "role": "author", "subagent_id": "sa-1",
                  "produces": [{ "name": "fn1", "type": "function" }],
                  "consumes": [{ "name": "dep1", "type": "module", "external": true }] },
                { "file": "src/b.ts" }
            ],
            "bindings": [{ "from": "b1", "to": "b2", "type": "data" }]
        });
        db.execute(
            "INSERT INTO swarm_manifests (contract_slug, version, goal, manifest_json, lint_passed) VALUES ('SW-1-C1', 2, 'ship it', ?1, 1)",
            rusqlite::params![manifest.to_string()],
        ).unwrap();
        let dir = temp_dir("swarm-on");
        let r = with_home(dir.to_str().unwrap(), || cardRuntimeState(&json!({ "slug": "SW-1" }), &db).unwrap());
        assert_eq!(r["swarm_manifest"]["version"], json!(2));
        assert_eq!(r["swarm_manifest"]["goal"], json!("ship it"));
        assert_eq!(r["swarm_manifest"]["lint_passed"], json!(1));
        assert_eq!(r["swarm_manifest"]["blocks"][0]["id"], json!("b1"));
        assert_eq!(r["swarm_manifest"]["blocks"][0]["subagent_id"], json!("sa-1"));
        assert_eq!(r["swarm_manifest"]["blocks"][0]["produces"], json!([{ "name": "fn1", "type": "function" }]));
        assert_eq!(r["swarm_manifest"]["blocks"][0]["consumes"], json!([{ "name": "dep1", "type": "module", "external": true }]));
        // second block has no id/role — those keys are OMITTED (undefined, not null)
        let b2 = &r["swarm_manifest"]["blocks"][1];
        assert!(!b2.as_object().unwrap().contains_key("id"));
        assert!(!b2.as_object().unwrap().contains_key("role"));
        assert_eq!(b2["file"], json!("src/b.ts"));
        assert_eq!(b2["subagent_id"], Value::Null);
        assert_eq!(b2["produces"], json!([]));
        assert_eq!(b2["consumes"], json!([]));
        assert_eq!(r["swarm_manifest"]["bindings"], json!([{ "from": "b1", "to": "b2", "type": "data" }]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn swarm_manifest_and_manifest_lint_stay_null_when_flag_off() {
        let db = make_db();
        insert_card(&db, "SW-2", "doing", None);
        db.execute("INSERT INTO contracts (slug, card_slug, status) VALUES ('SW-2-C1', 'SW-2', 'pl_ratified')", []).unwrap();
        db.execute(
            "INSERT INTO swarm_manifests (contract_slug, version, manifest_json) VALUES ('SW-2-C1', 1, '{\"blocks\":[]}')",
            [],
        ).unwrap();
        db.execute(
            "INSERT INTO contract_qa_reviews (contract_slug, passed, verdict, failure_target, review_mode) VALUES ('SW-2-C1', 1, 'pass', 'microtasks', 'manifest-lint')",
            [],
        ).unwrap();
        let dir = temp_dir("swarm-off");
        let r = with_home_and_swarm(dir.to_str().unwrap(), Some("off"), || cardRuntimeState(&json!({ "slug": "SW-2" }), &db).unwrap());
        assert_eq!(r["swarm_manifest"], Value::Null);
        assert_eq!(r["latest_manifest_lint"], Value::Null);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn awaiting_human_true_with_unanswered_clarify_question() {
        let db = make_db();
        insert_card(&db, "FEAT-CLARIFY", "doing", None);
        db.execute("INSERT INTO pl_sessions (id, status, approved_card_slug) VALUES (1, 'clarifying', 'FEAT-CLARIFY')", []).unwrap();
        db.execute("INSERT INTO pl_questions (id, session_id, step, question_md) VALUES (10, 1, 'clarify', 'Where?')", []).unwrap();
        let dir = temp_dir("awaiting-human");
        let r = with_home(dir.to_str().unwrap(), || cardRuntimeState(&json!({ "slug": "FEAT-CLARIFY" }), &db).unwrap());
        assert_eq!(r["awaiting_human"], json!(true));
        assert_eq!(r["pending_questions"], json!(1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn awaiting_human_false_once_question_is_answered() {
        let db = make_db();
        insert_card(&db, "FEAT-ANSWERED", "doing", None);
        db.execute("INSERT INTO pl_sessions (id, status, approved_card_slug) VALUES (1, 'clarifying', 'FEAT-ANSWERED')", []).unwrap();
        db.execute("INSERT INTO pl_questions (id, session_id, step, question_md) VALUES (10, 1, 'clarify', 'Where?')", []).unwrap();
        db.execute("INSERT INTO pl_answers (question_id, selected_option_id) VALUES (10, NULL)", []).unwrap();
        let dir = temp_dir("answered");
        let r = with_home(dir.to_str().unwrap(), || cardRuntimeState(&json!({ "slug": "FEAT-ANSWERED" }), &db).unwrap());
        assert_eq!(r["awaiting_human"], json!(false));
        assert_eq!(r["pending_questions"], json!(0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn awaiting_human_false_when_session_not_clarifying() {
        let db = make_db();
        insert_card(&db, "FEAT-DONE-SESSION", "doing", None);
        db.execute("INSERT INTO pl_sessions (id, status, approved_card_slug) VALUES (1, 'done', 'FEAT-DONE-SESSION')", []).unwrap();
        db.execute("INSERT INTO pl_questions (id, session_id, step, question_md) VALUES (10, 1, 'clarify', 'Where?')", []).unwrap();
        let dir = temp_dir("not-clarifying");
        let r = with_home(dir.to_str().unwrap(), || cardRuntimeState(&json!({ "slug": "FEAT-DONE-SESSION" }), &db).unwrap());
        assert_eq!(r["awaiting_human"], json!(false));
        assert_eq!(r["pending_questions"], json!(0));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn halted_reflects_real_halt_file_presence() {
        let db = make_db();
        insert_card(&db, "FEAT-HALT", "doing", None);
        let dir = temp_dir("halt-absent");
        let r = with_home(dir.to_str().unwrap(), || cardRuntimeState(&json!({ "slug": "FEAT-HALT" }), &db).unwrap());
        assert_eq!(r["halted"], json!(false));
        let _ = fs::remove_dir_all(&dir);

        let dir2 = temp_dir("halt-present");
        let run_dir = dir2.join("run");
        fs::create_dir_all(&run_dir).unwrap();
        fs::write(run_dir.join("driver-halt"), "").unwrap();
        let r2 = with_home(dir2.to_str().unwrap(), || cardRuntimeState(&json!({ "slug": "FEAT-HALT" }), &db).unwrap());
        assert_eq!(r2["halted"], json!(true));
        let _ = fs::remove_dir_all(&dir2);
    }
}
