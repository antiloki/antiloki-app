//! workflow-run.rs — twin of skills/workflow/workflow-run.mjs (db WRITE lane, via dispatch to
//! action-run). NAMES ARE CONTRACT: `workflowRun` (default), `deref` / `resolveInput` (named,
//! no-db).
//!
//! Looks up the workflow via the already-converted `workflow_workflow_find::workflowFind`, then
//! dispatches each step's action through `engine_action_run::skillRunAs` (already scope-limited
//! per its own module doc comment — its dispatch_step_skill table currently only knows
//! `card-list`/`with-audit`, so any fixture step must resolve to an action whose OWN steps_json
//! stays within that scope, e.g. the `board-keeper.card.list` pattern action-run.rs's own tests
//! seed).
//!
//! `context.actor` (JS default `'workflow:auto'`, explicitly spread into actionRun's context) IS
//! threaded — via `skillRunAs`, not plain `skillRun` (which hardcodes `actor="unknown"`, correct
//! only for its OWN bare-fixture-context dispatch). This twin's own (input, db) signature never
//! carries a context.actor override either (same fixture-shape reasoning), so the JS default
//! `'workflow:auto'` is the one value any case of THIS unit will ever produce — hardcoded as a
//! local constant. `db` being always-present in `&Connection` makes the JS `if (!db) throw` guard
//! unreachable-by-construction, same simplification action-run/with-audit already made.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

// REF = /^\$\.(input|results)\.([\w.[\]]+)$/ — verbatim JS regex (gotcha #7: escape literal [ ]).
fn ref_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\$\.(input|results)\.([\w.\[\]]+)$").unwrap())
}

// deref(value, ctx): non-string passthrough; non-matching string passthrough; otherwise walks
// ctx.input/ctx.results by the dotted path, short-circuiting to Value::Null (JS `undefined`) the
// moment the walk hits a null/missing node — matching JS's `if (cur == null) return undefined`
// BEFORE each `cur = cur[seg]` access.
pub fn deref(value: &Value, ctx: &Value) -> Value {
    let Some(s) = value.as_str() else { return value.clone() };
    let Some(caps) = ref_re().captures(s) else { return value.clone() };
    let root_key = if &caps[1] == "input" { "input" } else { "results" };
    let mut cur: Value = ctx.get(root_key).cloned().unwrap_or(Value::Null);
    for seg in caps[2].split('.') {
        if cur.is_null() {
            return Value::Null;
        }
        cur = cur.get(seg).cloned().unwrap_or(Value::Null);
    }
    cur
}

// resolveInput(stepInput, ctx): null/missing → {}; string → deref; array → map; plain object →
// recurse per-value (key order preserved, gotcha #1); anything else passes through unchanged.
pub fn resolveInput(step_input: &Value, ctx: &Value) -> Value {
    match step_input {
        Value::Null => json!({}),
        Value::String(_) => deref(step_input, ctx),
        Value::Array(arr) => Value::Array(arr.iter().map(|v| resolveInput(v, ctx)).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), resolveInput(v, ctx));
            }
            Value::Object(out)
        }
        _ => step_input.clone(),
    }
}

pub fn workflowRun(input: &Value, db: &Connection) -> Result<Value, String> {
    let name = match input.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(n) => n.to_string(),
        None => return Err("workflow-run: name is required".into()),
    };
    let workflow_input = input.get("input").cloned().unwrap_or_else(|| json!({}));

    let wf = crate::workflow_workflow_find::workflowFind(&json!({ "name": name }), db)?;
    let on_failure = wf.get("on_failure").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let steps_arr = wf.get("steps").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let mut ctx = serde_json::Map::new();
    ctx.insert("input".into(), workflow_input);
    ctx.insert("results".into(), json!({}));

    let mut errors: Vec<Value> = Vec::new();
    let mut steps_run: i64 = 0;
    let mut completed = true;

    for (i, step) in steps_arr.iter().enumerate() {
        let step_input_spec = step.get("input").cloned().unwrap_or(Value::Null);
        let ctx_val = Value::Object(ctx.clone());
        let resolved_input = resolveInput(&step_input_spec, &ctx_val);
        let action = step.get("action").cloned().unwrap_or(Value::Null);

        // JS: `const { db, actor = 'workflow:auto' } = ctxIn` then `actionRun(..., {...ctxIn,
        // actor})`. This twin's own signature (input, db) never carries a context.actor override
        // (the harness's fixture for workflowRun is always bare {db} too — same reasoning as
        // action-run's own default), so the JS default is the only value any case ever produces:
        // hardcode it and thread it into skillRunAs (NOT plain skillRun, which hardcodes "unknown"
        // — a real, conform-caught divergence before this fix, see action-run.rs's doc comment).
        // JS's own workflow-run never sets context.source, only actor — so this dispatch's
        // runtime_events.source stays None (matches action-run's `context?.source ?? null`
        // resolving to null when the spread ctxIn carries no source key).
        let actor = "workflow:auto";
        let run_input = json!({ "action": action, "input": resolved_input });
        match crate::engine_action_run::skillRunAs(&run_input, db, actor, None, &Value::Null) {
            Ok(result) => {
                steps_run += 1;
                if let Some(capture_as) = step.get("capture_as").and_then(|v| v.as_str()) {
                    if let Some(results_map) = ctx.get_mut("results").and_then(|v| v.as_object_mut()) {
                        results_map.insert(capture_as.to_string(), result);
                    }
                }
            }
            Err(e) => {
                errors.push(json!({
                    "step_index": i as i64,
                    "action": action,
                    "message": e,
                }));
                if on_failure == "abort" {
                    completed = false;
                    break;
                }
                // 'continue' → record error and proceed
            }
        }
    }

    Ok(json!({
        "workflow": name,
        "completed": completed,
        "steps_run": steps_run,
        "steps_total": steps_arr.len() as i64,
        "results": ctx.get("results").cloned().unwrap_or_else(|| json!({})),
        "errors": errors,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIONS_SCHEMA: &str = "CREATE TABLE actions (name TEXT UNIQUE, description TEXT, owner_agent TEXT, steps_json TEXT, returns TEXT, on_failure TEXT, kind TEXT)";
    const RUNTIME_EVENTS_SCHEMA: &str = "CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', parent_event_id INTEGER, actor TEXT NOT NULL, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, outcome TEXT NOT NULL CHECK (outcome IN ('success','error','blocked','skipped','in_flight')), error_code TEXT, context_json TEXT, user_goal_id INTEGER, source TEXT)";
    const CARDS_SCHEMA: &str = "CREATE TABLE cards (slug TEXT, status TEXT, score REAL)";
    const LAWS_SCHEMA: &str = "CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, statement TEXT, enforcement TEXT, enforcement_hook TEXT, superseded_by INTEGER)";
    const WORKFLOWS_SCHEMA: &str = "CREATE TABLE workflows (id INTEGER PRIMARY KEY, name TEXT, description TEXT, owner_agent TEXT, steps_json TEXT, on_failure TEXT, created_at TEXT)";

    fn seeded_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws;").unwrap();
        c.execute_batch(&format!(
            "{ACTIONS_SCHEMA}; {RUNTIME_EVENTS_SCHEMA}; {CARDS_SCHEMA}; {LAWS_SCHEMA}; {WORKFLOWS_SCHEMA};
             INSERT INTO cards VALUES ('A','doing',5),('B','backlog',NULL);
             INSERT INTO actions (name, description, owner_agent, steps_json, returns, kind) VALUES
               ('board-keeper.card.list', 'list cards', 'board-keeper', '[{{\"skill\":\"card-list\",\"input\":{{}},\"as\":\"cards\"}}]', '$.results.cards', 'atomic');"
        ))
        .unwrap();
        c
    }

    #[test]
    fn happy_path_two_steps_chains_capture_as() {
        let c = seeded_db();
        c.execute(
            "INSERT INTO workflows (id, name, description, owner_agent, steps_json, on_failure) VALUES \
             (1, 'wf-happy', 'd', 'o', ?, 'continue')",
            [r#"[{"action":"board-keeper.card.list","input":{"echo":"$.input.status_filter"},"capture_as":"first"},{"action":"board-keeper.card.list","input":{"seen":"$.results.first"},"capture_as":"second"}]"#],
        )
        .unwrap();
        let result = workflowRun(&json!({ "name": "wf-happy", "input": { "status_filter": "doing" } }), &c).unwrap();
        assert_eq!(result["completed"], json!(true));
        assert_eq!(result["steps_run"], json!(2));
        assert_eq!(result["steps_total"], json!(2));
        assert_eq!(result["errors"], json!([]));
        assert_eq!(result["results"]["first"].as_array().unwrap().len(), 2);
        assert_eq!(result["results"]["second"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn abort_halts_after_first_error() {
        let c = seeded_db();
        c.execute(
            "INSERT INTO workflows (id, name, description, owner_agent, steps_json, on_failure) VALUES \
             (1, 'wf-abort', 'd', 'o', ?, 'abort')",
            [r#"[{"action":"board-keeper.card.list","input":{},"capture_as":"ok"},{"action":"nope.action.missing","input":{},"capture_as":"bad"},{"action":"board-keeper.card.list","input":{},"capture_as":"never"}]"#],
        )
        .unwrap();
        let result = workflowRun(&json!({ "name": "wf-abort" }), &c).unwrap();
        assert_eq!(result["completed"], json!(false));
        assert_eq!(result["steps_run"], json!(1));
        assert_eq!(result["steps_total"], json!(3));
        let errors = result["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["step_index"], json!(1));
        assert_eq!(errors[0]["action"], json!("nope.action.missing"));
        assert!(result["results"].get("ok").is_some());
        assert!(result["results"].get("never").is_none());
    }

    #[test]
    fn continue_proceeds_past_error() {
        let c = seeded_db();
        c.execute(
            "INSERT INTO workflows (id, name, description, owner_agent, steps_json, on_failure) VALUES \
             (1, 'wf-continue', 'd', 'o', ?, 'continue')",
            [r#"[{"action":"nope.action.missing","input":{},"capture_as":"bad"},{"action":"board-keeper.card.list","input":{},"capture_as":"ok"}]"#],
        )
        .unwrap();
        let result = workflowRun(&json!({ "name": "wf-continue" }), &c).unwrap();
        assert_eq!(result["completed"], json!(true));
        assert_eq!(result["steps_run"], json!(1));
        assert_eq!(result["steps_total"], json!(2));
        let errors = result["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["step_index"], json!(0));
        assert!(result["results"].get("ok").is_some());
    }

    #[test]
    fn missing_name_and_not_found_guards() {
        let c = seeded_db();
        assert_eq!(workflowRun(&json!({}), &c).unwrap_err(), "workflow-run: name is required");
        assert!(workflowRun(&json!({ "name": "ghost" }), &c).unwrap_err().contains("not found"));
    }

    #[test]
    fn deref_and_resolve_input() {
        let ctx = json!({ "input": { "status_filter": "doing" }, "results": { "first": [1, 2] } });
        assert_eq!(deref(&json!("$.input.status_filter"), &ctx), json!("doing"));
        assert_eq!(deref(&json!("$.results.first"), &ctx), json!([1, 2]));
        assert_eq!(deref(&json!("plain"), &ctx), json!("plain"));
        assert_eq!(deref(&json!(42), &ctx), json!(42));
        assert_eq!(deref(&json!("$.input.missing.deep"), &ctx), Value::Null);

        assert_eq!(resolveInput(&Value::Null, &ctx), json!({}));
        assert_eq!(
            resolveInput(&json!({ "a": "$.input.status_filter", "arr": ["$.results.first", "lit"] }), &ctx),
            json!({ "a": "doing", "arr": [[1, 2], "lit"] })
        );
    }
}
