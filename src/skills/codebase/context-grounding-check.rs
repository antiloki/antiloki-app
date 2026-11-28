//! context-grounding-check.rs — twin of skills/codebase/context-grounding-check.mjs (db WRITE
//! lane, standard). NAMES ARE CONTRACT: default export `contextGroundingCheck` (standard lane,
//! `fn(input, context)`); named export `ungroundedStateClaims` (pure, no-db helper lane).
//!
//! `contextGroundingCheck` calls the `stack-facts` twin (SAME BATCH sibling module
//! `codebase_stack_facts`, standard lane sig — `crate::codebase_stack_facts::stackFacts`) to get
//! the project's state-management stack fact, then flags any STATE_LIBS the AI output ASSERTS
//! that the stack fact marks absent. Non-blocking: on a flag it best-effort INSERTs a
//! `runtime_events` warn row (swallowed on failure, matching the JS `try {...} catch {}`) and
//! always returns `ok:true`.
//!
//! JS's `if (!db) throw err.contextMissing('context-grounding-check','db')` guard is unreachable
//! here — the db lane always injects a real `Connection`, same convention as sibling twins.
//!
//! DISCLOSED SCOPE (conform-db fixture): `stackFacts` itself does a REAL fs read of
//! `<workspace root>/package.json` (see stack-facts.mjs / workspace-root.mjs) — not something a
//! conform-db fixture's schema/seed can control. The fixture seeds an ACTIVE `workspaces` row
//! whose `root_path` is a real, stable, package.json-free directory (`C:\Windows`) so BOTH lanes
//! deterministically resolve `state: null` and take the early-return branch (this also sidesteps
//! an unrelated asymmetry: JS's `SUBSTRATE_ROOT` fallback resolves inside antiloki-core, which DOES
//! have its own package.json, while the Rust `substrate_root()` fallback resolves one directory up
//! from antiloki-core-rust, which does not — seeding an active workspace row bypasses that fallback
//! entirely on both sides). The `state != null` grounding-detection branch (and the
//! `runtime_events` INSERT it triggers) is therefore NOT exercised by the conform-db fixture; it is
//! exercised locally by this file's own `#[cfg(test)]` (a bespoke real-fs test using a temp
//! directory) and, independently, the actual detection LOGIC is fully covered by the directly and
//! exhaustively tested pure `ungroundedStateClaims`, which holds all of the interesting behavior.

use regex::{Regex, RegexBuilder};
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

/// JS `String(v || '')` — falls back to `''` for ANY falsy `v` (not just null/undefined, unlike
/// `??`/`str_qq`). Used only by `ungroundedStateClaims`, which is the one call site using `||`.
fn or_empty(v: &Value) -> String { if truthy(Some(v)) { js_string(v) } else { String::new() } }

// State stores are the proven hallucination class (a model invents "the store" a project doesn't
// have). Longest-first so 'redux-toolkit' is considered before the bare 'redux' fragment.
const STATE_LIBS: &[&str] = &[
    "redux-toolkit", "redux", "zustand", "mobx", "recoil", "jotai", "valtio", "xstate", "effector",
    "ngrx", "vuex", "pinia",
];

// A mention inside a NEGATION ("no Redux", "without Zustand", …) is the model HONORING the stack
// fact — look back a short window for a negator with no sentence break in between.
fn neg_before_re() -> Regex {
    RegexBuilder::new(r"(?:\bno\b|\bnot\b|\bwithout\b|\bnever\b|\bnone\b|\bnor\b|\babsent\b|\blacks?\b|\bexcludes?\b|\bexcluding\b|\binstead of\b|\bavoid(?:ing|s)?\b|\bdon'?t\b|\bdo(?:es)? not\b|\bremoved?\b)[^.!?\n]{0,80}$")
        .case_insensitive(true).build().unwrap()
}
// "the existing Redux store" ASSERTS the store exists — overrides a negation window ("rather than
// the existing Redux store" still invents the store).
fn existing_before_re() -> Regex {
    RegexBuilder::new(r"\bexisting\b[^.!?\n]{0,20}$").case_insensitive(true).build().unwrap()
}

/// JS `out.slice(Math.max(0, index-back), index)`. Byte-offset slicing is an ASCII/BMP
/// approximation of JS's UTF-16 code-unit slicing (playbook gotcha #5) — safe for this checker's
/// English-prose fixtures. Snaps inward to the nearest char boundary defensively.
fn window_before(out: &str, start: usize, back: usize) -> String {
    let mut from = start.saturating_sub(back);
    while from > 0 && !out.is_char_boundary(from) { from -= 1; }
    out[from..start].to_string()
}

/// `export function ungroundedStateClaims(outputText, stateFact)` — pure, no-db helper. Which
/// STATE libs does `outputText` ASSERT that `stateFact` marks ABSENT?
pub fn ungroundedStateClaims(output_text: &Value, state_fact: &Value) -> Value {
    let out = or_empty(output_text);
    let state = or_empty(state_fact).to_lowercase();
    let all_absent = state.starts_with("none detected");
    let neg_re = neg_before_re();
    let existing_re = existing_before_re();
    let mut flagged: Vec<String> = Vec::new();
    for lib in STATE_LIBS {
        let pattern = format!(r"\b{}\b", lib.replace('-', "[- ]?"));
        let re = RegexBuilder::new(&pattern).case_insensitive(true).build().unwrap();
        let mut assertive = false;
        for m in re.find_iter(&out) {
            let before = window_before(&out, m.start(), 90);
            if existing_re.is_match(&before) || !neg_re.is_match(&before) {
                assertive = true;
                break;
            }
        }
        if !assertive { continue; } // only negated mentions — the model is denying the lib, correctly
        let stripped = lib.split('-').next().unwrap_or(lib); // JS: lib.replace(/-.*/, '')
        let present = !all_absent && state.contains(stripped);
        if !present { flagged.push((*lib).to_string()); }
    }
    json!(flagged) // JS `[...new Set(flagged)]` — STATE_LIBS entries are unique, dedup is a no-op
}

/// default export `contextGroundingCheck(input, context)` — standard db lane.
pub fn contextGroundingCheck(input: &Value, db: &Connection) -> Result<Value, String> {
    let output = str_qq(input.get("output"));

    let mut state = Value::Null;
    if let Ok(sf) = crate::codebase_stack_facts::stackFacts(&json!({}), db) {
        if sf.get("found").and_then(|v| v.as_bool()) == Some(true) {
            state = sf.get("facts").and_then(|f| f.get("state")).cloned().unwrap_or(Value::Null);
        }
    }
    if state.is_null() || output.is_empty() {
        let mut out = Map::new();
        out.insert("ok".into(), json!(true));
        out.insert("grounded".into(), json!(true));
        out.insert("ungrounded".into(), json!([]));
        out.insert("state".into(), state);
        return Ok(Value::Object(out));
    }

    let ungrounded = ungroundedStateClaims(&json!(output), &state);
    let ungrounded_arr = ungrounded.as_array().cloned().unwrap_or_default();
    if !ungrounded_arr.is_empty() {
        let agent = nz(input.get("agent")).map(|v| str_qq(Some(v))).unwrap_or_else(|| "system".into());
        let mut ctx = Map::new();
        ctx.insert("ai_call_id".into(), nz(input.get("ai_call_id")).cloned().unwrap_or(Value::Null));
        ctx.insert("phase".into(), nz(input.get("phase")).cloned().unwrap_or(Value::Null));
        ctx.insert("ungrounded".into(), ungrounded.clone());
        ctx.insert("state".into(), state.clone());
        let context_json = Value::Object(ctx).to_string();
        // best-effort — an audit warn never breaks the call, matching JS's outer try/catch
        let _ = crate::db_json::run_write(
            db,
            "INSERT INTO runtime_events (actor, agent, action, skill, outcome, error_code, context_json, source) \
             VALUES ('agent:context-bundle', ?, 'context.grounding', 'context-grounding-check', 'success', 'CONTEXT_GROUNDING_WARN', ?, 'context-bundle')",
            &[json!(agent), json!(context_json)],
        );
    }

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("grounded".into(), json!(ungrounded_arr.is_empty()));
    out.insert("ungrounded".into(), ungrounded);
    out.insert("state".into(), state);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, parent_event_id INTEGER, \
             actor TEXT, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, outcome TEXT, error_code TEXT, \
             user_goal_id INTEGER, source TEXT, context_json TEXT); \
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        ).unwrap();
        c
    }
    fn seed_active_root(c: &Connection, root_path: &str) {
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active, created_at) VALUES ('self', ?1, 'git', 1, '2026-01-01 00:00:00')",
            rusqlite::params![root_path],
        ).unwrap();
    }

    // ── ungroundedStateClaims — ports of the real JS test's cases (context-grounding-check.test.mjs) ──
    const ABSENT: &str = "none detected — local component state (no redux/zustand/mobx/recoil/jotai/xstate in dependencies)";

    #[test]
    fn meli_case_asserts_existing_redux_store_flagged() {
        let out = "use local state synchronized with localStorage rather than the existing Redux store";
        let flagged = ungroundedStateClaims(&json!(out), &json!(ABSENT));
        assert!(flagged.as_array().unwrap().contains(&json!("redux")));
    }

    #[test]
    fn clean_output_no_store_mention_nothing_flagged() {
        let flagged = ungroundedStateClaims(&json!("use a local useState hook and a src/middlewares handler"), &json!(ABSENT));
        assert_eq!(flagged, json!([]));
    }

    #[test]
    fn store_present_mentioning_it_is_grounded() {
        let flagged = ungroundedStateClaims(&json!("dispatch through the Redux Toolkit slice"), &json!("Redux Toolkit"));
        let arr = flagged.as_array().unwrap();
        assert!(!arr.contains(&json!("redux")));
        assert!(!arr.contains(&json!("redux-toolkit")));
    }

    #[test]
    fn different_absent_store_flagged_even_when_another_present() {
        let flagged = ungroundedStateClaims(&json!("put it in the zustand store"), &json!("Redux Toolkit"));
        assert!(flagged.as_array().unwrap().contains(&json!("zustand")));
    }

    #[test]
    fn bare_redux_mention_does_not_match_redux_toolkit() {
        let flagged = ungroundedStateClaims(&json!("add it to the redux store"), &json!(ABSENT));
        assert_eq!(flagged, json!(["redux"]));
    }

    #[test]
    fn negated_mentions_not_flagged() {
        let out = "State management is strictly confined to local component state; the dependency tree contains no global state libraries (Redux, Zustand, MobX, Recoil, Jotai, or XState)";
        assert_eq!(ungroundedStateClaims(&json!(out), &json!(ABSENT)), json!([]));
    }

    #[test]
    fn echoed_stack_fact_line_not_flagged() {
        assert_eq!(ungroundedStateClaims(&json!("this project has no redux/zustand/mobx in dependencies"), &json!(ABSENT)), json!([]));
    }

    #[test]
    fn without_and_instead_of_are_negations() {
        assert_eq!(ungroundedStateClaims(&json!("persist the counter without Zustand"), &json!(ABSENT)), json!([]));
        assert_eq!(ungroundedStateClaims(&json!("use useState instead of Redux"), &json!(ABSENT)), json!([]));
    }

    #[test]
    fn existing_lib_store_overrides_nearby_negation() {
        let flagged = ungroundedStateClaims(&json!("do not touch the existing Redux store"), &json!(ABSENT));
        assert!(flagged.as_array().unwrap().contains(&json!("redux")));
    }

    #[test]
    fn negated_mention_plus_separate_real_assertion_still_flags() {
        let out = "there is no MobX here. Dispatch the action through the zustand store on click.";
        let flagged = ungroundedStateClaims(&json!(out), &json!(ABSENT));
        let arr = flagged.as_array().unwrap();
        assert!(!arr.contains(&json!("mobx")));
        assert!(arr.contains(&json!("zustand")));
    }

    // ── contextGroundingCheck — db lane ──
    #[test]
    fn early_returns_when_no_stack_facts_found() {
        let c = db();
        // an active workspace that is an EMPTY folder of this test's own: with no row at all, stackFacts fell back to a real folder of the machine, and passed or failed by what happened to be in it
        let empty = std::env::temp_dir().join(format!("grounding-empty-{}", std::process::id())); std::fs::create_dir_all(&empty).unwrap();
        c.execute("INSERT INTO workspaces (name, root_path, vcs, is_active, created_at) VALUES ('empty', ?1, 'git', 1, '2026-01-01 00:00:00')", [empty.to_string_lossy().to_string()]).unwrap();
        let r = contextGroundingCheck(&json!({ "output": "use the existing Redux store" }), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "grounded": true, "ungrounded": [], "state": null }));
    }

    #[test]
    fn empty_output_early_returns() {
        let c = db();
        seed_active_root(&c, "C:\\Windows"); // real, stable, package.json-free directory
        let r = contextGroundingCheck(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "grounded": true, "ungrounded": [], "state": null }));
        let events: i64 = c.query_row("SELECT COUNT(*) FROM runtime_events", [], |row| row.get(0)).unwrap();
        assert_eq!(events, 0);
    }

    #[test]
    fn flags_and_records_a_warn_event_when_stack_facts_are_known() {
        // Bespoke real-fs test (not part of the conform-db fixture — see module doc): seed an
        // active workspace whose root_path is a real temp dir containing a real package.json with
        // no state-mgmt dep, so stackFacts' real fs read resolves a non-null `state` fact,
        // exercising the grounding branch a conform-db fixture cannot reach.
        let dir = std::env::temp_dir().join(format!("ctx-grounding-check-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("package.json"), r#"{"name":"x","dependencies":{}}"#).unwrap();
        let c = db();
        seed_active_root(&c, &dir.to_string_lossy());

        let r = contextGroundingCheck(&json!({ "output": "add it to the redux store", "agent": "tester" }), &c).unwrap();
        assert_eq!(r["grounded"], json!(false));
        assert!(r["ungrounded"].as_array().unwrap().contains(&json!("redux")));
        let events: i64 = c.query_row("SELECT COUNT(*) FROM runtime_events WHERE error_code = 'CONTEXT_GROUNDING_WARN'", [], |row| row.get(0)).unwrap();
        assert_eq!(events, 1);

        fs::remove_dir_all(&dir).ok();
    }
}
