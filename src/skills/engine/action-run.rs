//! action-run.rs — twin of skills/engine/action-run.mjs (db WRITE lane). NAMES ARE CONTRACT:
//! `skillRun` (default), `resolvePath` / `deepResolve` (named, no-db).
//!
//! SCOPE (mirrors the with-audit precedent): the conform-db harness's standard-lane context is
//! always bare `{ db }` — never `{ db, agent }` — so every branch gated on `context?.agent` is
//! unreachable by construction under fixture testing and is NOT ported:
//!   1. The permission/auth block (`agent-permission-check` + `auth-verify` dispatch).
//!   2. The law-surface block (`law-surface-for-context`) — `_soft_laws` stays absent; the real
//!      JS never reads `runCtx._soft_laws` downstream either, so dropping the field changes
//!      nothing observable.
//! Likewise the `db` required-field guard (`if (!db) throw`) is unreachable-by-construction in
//! Rust — `db: &Connection` is never absent — and is dropped for the same reason with-audit
//! dropped its own copy of this exact guard.
//!
//! What IS ported for real:
//!   3. The hard-law ENFORCEMENT loop (`ANTILOKI_SKIP_LAW_ENFORCEMENT` unset → always runs here).
//!      Candidates come from the already-converted `entities_law_check::lawCheck`. Each
//!      candidate's `enforcement_hook` (a skill NAME) is dispatched through a hand-kept table
//!      (`dispatch_hook`, mirroring with-audit's `dispatch_skill` pattern) covering the three
//!      hooks named in the conversion brief. A hook name with no arm fails closed immediately
//!      (stands in for JS's "hook not found" branch — NOT fixture-tested, see below). A hook
//!      that itself errors is treated as JS's `catch(e)` branch: recorded as 'escaped' and the
//!      loop `continue`s (the per-hook error is swallowed, not propagated). The
//!      `ANTILOKI_LAW_FAILOPEN`/`ANTILOKI_SKIP_LAW_ENFORCEMENT` env-gated branches are not
//!      ported (both env vars are unset in the harness, per established crossing convention).
//!   4. The steps loop: `JSON.parse(action.steps_json)` → for each step, `deepResolve(step.input,
//!      runCtx)` then dispatch `step.skill` through a second hand-kept table (`dispatch_step_skill`)
//!      — this round's only arm is `"card-list"` → `entities_card_list::cardList`, proving the
//!      mechanism. `step.as` (when set) stores the result at `runCtx.results[step.as]`.
//!   5. runtime_events logging (`ANTILOKI_SKIP_RUNTIME_LOG` unset → always runs here) via the
//!      already-converted `engine_log_event::logEvent`, for both the parent in-flight row and
//!      each step row. `duration_ms` is hardcoded `0.0` everywhere the JS computes a
//!      `performance.now()` delta — the conform harness freezes `performance.now()` to a
//!      constant, so every JS delta is exactly `0` (playbook's frozen-clock lane). The final
//!      `UPDATE runtime_events SET outcome=…` on the parent row is a raw `db.execute`, matching
//!      JS's own raw `db.prepare(...).run(...)` (not routed through any twin).
//!   6. `action.returns ? resolvePath(action.returns, runCtx) : undefined` → `Value::Null` for
//!      the undefined case (the DB-lane's usual convention).
//!   7. The `actionName` required-field guard and the action-not-found `SKILL_NOT_FOUND` error,
//!      matching the "{skill}: {cause}" message shape every other converted twin uses for
//!      `engine/error.mjs`-sourced errors (AntilokiError's `message = [ "${skill}:", cause
//!      ].join(' ')` when `cause` is set) — e.g. `"action-run: action is required"`,
//!      `"action-run: action 'X' not found"`, `"action-run: law 'SLUG' denied: REASON"`.
//!
//! `run_ctx` also omits JS's `{...context}` spread (JS: `runCtx = {...context, input, results,
//! _soft_laws}`) — no key from that spread besides `actor` (see below) is ever observably read
//! downstream (no `agent`/`user_goal_id` field is read back out of `runCtx` either).
//!
//! ACTOR (correction, post-with-audit-precedent): `skillRun(input, db)` is a thin wrapper over
//! `skillRunAs(input, db, actor)` hardcoding `actor="unknown"` — correct ONLY for the
//! fixture-dispatched unit itself (harness context is bare `{db}`). Real Rust callers that compose
//! this twin as a library function — `workflow_workflow_run::workflowRun` (JS default
//! `actor='workflow:auto'`, explicitly spread into actionRun's context) and
//! `system_action_verify::actionVerify` (JS passes `actor: dispatch_actor`, default `'human:cli'`)
//! — call `skillRunAs` with their real actor value instead. `context?.actor ?? 'unknown'` appears
//! at FOUR JS call sites (the parent + step runtime_events rows, and law-violation-record's
//! `attempted_by` on all three record paths) — all four now thread the `actor` parameter. Caught
//! by conform: both consumers' `runtime_events.actor` diverged from JS before this split existed.
//!
//! NOT fixture-tested (documented, not silently dropped): a step-skill dispatch ERROR (the
//! `error_code = e?.code ?? e?.name ?? 'ERROR'` derivation is approximated as a flat `"ERROR"`
//! string since our `Err(String)` carries no `.code`/`.name` metadata — this only matters for a
//! step that throws, which no fixture case exercises); the law-enforcement "hook not found"
//! fail-closed branch (dispatch-table-miss stands in for it in spirit per the conversion brief,
//! but its exact JS violation-record side effect is not hand-verified); a JSON.parse failure on
//! `steps_json` (fixtures always seed valid JSON).
use rusqlite::{params, Connection};
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
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

/// resolvePath($.foo.bar, ctx) walks ctx.foo.bar; non-`$.`-prefixed values pass through
/// unchanged; a null/missing key mid-walk short-circuits to `Value::Null` (JS `undefined`).
pub fn resolvePath(path: &Value, ctx: &Value) -> Value {
    let Some(s) = path.as_str() else { return path.clone() };
    if !s.starts_with("$.") {
        return path.clone();
    }
    let mut val: &Value = ctx;
    for seg in s[2..].split('.') {
        if val.is_null() {
            return Value::Null;
        }
        match val.get(seg) {
            Some(v) => val = v,
            None => return Value::Null,
        }
    }
    val.clone()
}

/// deepResolve recursively applies resolvePath through strings/arrays/objects, preserving key
/// insertion order for objects (== JS `Object.keys` iteration order).
pub fn deepResolve(value: &Value, ctx: &Value) -> Value {
    match value {
        Value::String(s) => {
            if s.starts_with("$.") {
                resolvePath(value, ctx)
            } else {
                value.clone()
            }
        }
        Value::Array(arr) => Value::Array(arr.iter().map(|v| deepResolve(v, ctx)).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), deepResolve(v, ctx));
            }
            Value::Object(out)
        }
        _ => value.clone(),
    }
}

// The dispatch_hook arm names, duplicated here so the loop can fail closed on an unknown hook
// BEFORE calling dispatch_hook (see the loop's own comment for why that distinction matters).
const KNOWN_HOOKS: [&str; 7] = [
    "law-hook-no-destructive-git",
    "law-hook-no-direct-audit-write",
    "law-hook-mutation-via-with-audit",
    "law-hook-no-secrets-in-commit",
    "law-hook-no-drift-on-audit-commit",
    "law-hook-no-lone-skill-dispatch",
    "auth-verify",
];

// dispatch_hook — stands in for JS's `getHook(name)` (import-cache over a filename-index lookup
// that can resolve ANY skill on disk). The twin can only reach hooks that already have a Rust
// twin; a name with no arm fails closed (see module doc comment: NOT byte-verified against JS's
// exact "hook not found" side effects, matching that branch's SPIRIT only).
// `ctx` mirrors JS's second argument to every hook (`hookFn(input, context ?? {})`). Only the
// lone-skill-dispatch hook reads it today — but it reads source/driver-hmac/god-signature out of
// it, and passing `{}` there is what kept 225 of 581 actions undispatchable.
fn dispatch_hook(name: &str, input: &Value, ctx: &Value, db: &Connection) -> Result<Value, String> {
    match name {
        "law-hook-no-destructive-git" => {
            crate::audit_law_hook_no_destructive_git::lawHookNoDestructiveGit(input, db)
        }
        "law-hook-no-direct-audit-write" => {
            crate::audit_law_hook_no_direct_audit_write::lawHookNoDirectAuditWrite(input, db)
        }
        "law-hook-mutation-via-with-audit" => {
            crate::audit_law_hook_mutation_via_with_audit::lawHookMutationViaWithAudit(input, db)
        }
        "law-hook-no-secrets-in-commit" => {
            crate::audit_law_hook_no_secrets_in_commit::lawHookNoSecretsInCommit(input, db)
        }
        "law-hook-no-drift-on-audit-commit" => {
            crate::audit_law_hook_no_drift_on_audit_commit::lawHookNoDriftOnAuditCommit(input, db)
        }
        "law-hook-no-lone-skill-dispatch" => {
            crate::audit_law_hook_no_lone_skill_dispatch::lawHookNoLoneSkillDispatchWithCtx(input, ctx, db)
        }
        "auth-verify" => crate::external_auth_verify::authVerify(input, db),
        _ => Err(format!(
            "action-run: enforcement_hook dispatch table has no entry for '{name}' (extend as needed)"
        )),
    }
}

// dispatch_step_skill — stands in for JS's `importSkill(step.skill)`. Extend arm-by-arm as more
// twins are needed. `with-audit` is a REAL production step-skill name (canonical actions like
// board-keeper.card.status.change dispatch a step literally named `with-audit`, per
// engine/with-audit.mjs being importSkill()-resolvable like any other skill file) — added here
// (system/action-verify.rs conversion) so a fixture can dispatch a with-audit-composing action
// end-to-end and observe a real sacred_timeline seal, not just a pure read.
// `actor` is threaded to step-skills that care about it (with-audit) — JS calls `skill(resolvedInput,
// runCtx)` where runCtx carries `{...context, ...}`, so EVERY dispatched step-skill sees the SAME
// context.actor action-run itself received (card-list ignores it; with-audit's seal doesn't).
fn dispatch_step_skill(name: &str, input: &Value, db: &Connection, actor: &str) -> Result<Value, String> {
    // Runtime cutover: the generated skill_registry replaces the hand-kept match — action-run now
    // reaches every dispatchable skill twin by name (JS importSkill(step.skill) parity). Unknown
    // names still fail closed with the same message.
    let ctx = crate::skill_registry::RunCtx { db, actor, source: "action-run" };
    match crate::skill_registry::dispatch(name, input, &ctx) {
        Some(r) => r,
        None => Err(format!(
            "action-run: step-skill dispatch table has no entry for '{name}' (extend as needed)"
        )),
    }
}

/// The actions the front end polls to keep a screen current, which only READ state: not recorded in runtime_events (see the note at the logging site). Exact names - an action that changes anything is never in here.
const QUIET_READS: &[&str] = &[
    "system.workspace.list", "system.runtime.events", "system.gate.telemetry", "system.driver.status",
    "system.codebase.onboard.status", "system.codebase.onboard.latest",
];
pub(crate) fn is_quiet_read(action_name: &str) -> bool { QUIET_READS.contains(&action_name) }

fn record_violation_swallowed(db: &Connection, law_id: &Value, action_name: &str, actor: &str, context: Value, outcome: &str) {
    let input = json!({
        "law_id": law_id,
        "attempted_by": actor,
        "agent_name": Value::Null,
        "action_name": action_name,
        "context": context,
        "outcome": outcome,
    });
    let _ = crate::entities_law_violation_record::lawViolationRecord(&input, db);
}

// PUBLIC ENTRY (the fixture-dispatched unit): `context.actor` is always its JS default here,
// because the conform-db harness only ever calls the standard lane with bare context={db} (same
// reasoning as with-audit's own `withAudit`/`withAuditAs` split). Real callers that DO thread a
// custom actor — workflow-run defaults `actor='workflow:auto'` and explicitly spreads it into
// actionRun's context; action-verify passes `actor: dispatch_actor` (default 'human:cli') — call
// `skillRunAs` instead, which this function is a thin default-actor wrapper over. Caught by
// conform: workflow-run/action-verify's runtime_events.actor diverged from JS before this split.
pub fn skillRun(input: &Value, db: &Connection) -> Result<Value, String> {
    skillRunAs(input, db, "unknown", None, &Value::Null)
}

// `source` mirrors `actor`'s split: JS reads `context?.source ?? null` at the SAME 3 call sites
// (the allowed+severity law-violation context, and both runtime_events log rows) — action-verify
// hardcodes `source: 'cli'` in its own call, so this must thread too, not just actor.
// `user_goal_id` (JS: `context?.user_goal_id ?? null`, threaded into both runtime_events log
// rows) is the same kind of passthrough — drift-resolve.rs is the first caller that threads a
// real value (`user_goal_id: turn_id`); action-verify/workflow-run's real JS callers never pass
// one, so `&Value::Null` there is exact, not a narrowing.
// pub(crate) (not pub): this has no JS counterpart — it's internal API for other twins that need
// to thread a real actor/source/user_goal_id (workflow-run, action-verify, drift-resolve), not a
// public surface the names-ok parity gate should compare against action-run.mjs's exports.
// Visible crate-wide via crate::.
pub(crate) fn skillRunAs(input: &Value, db: &Connection, actor: &str, source: Option<&str>, user_goal_id: &Value) -> Result<Value, String> {
    // Empty hook ctx ON PURPOSE. driver-tick calls this with source="driver" but has nowhere to
    // put an HMAC, so handing the hook a real source would newly route it into the driver branch
    // and refuse the dispatch for a missing signature. Callers that can sign must use
    // skillRunWithContext and pass driver_hmac/driver_ts alongside the source.
    run_action(input, db, actor, source, user_goal_id, None, &Value::Null, &json!({}))
}

/// The production dispatch entry point: takes the JS-shaped `context` object whole, so the
/// permission/auth block below has the `agent` + `auth_token` it needs. This is what the napi
/// boundary hands straight through from the BE (`{actor, source, agent, auth_token, user_goal_id}`).
///
/// Deliberately a SIBLING of `skillRunAs` rather than a change to it: `skillRunAs`'s positional
/// form has 13 in-crate call sites across 8 twins, none of which carry an agent identity, and
/// widening its signature would churn all of them to pass `None` twice. Auth enforcement is opt-in
/// on `agent` being present (exactly as JS gates on `context?.agent`), so the conform harness's
/// bare `{db}` lane and every existing caller keep their current behavior.
/// `pub` (unlike `skillRunAs`'s `pub(crate)`) because the `antiloki` bin — and the napi crate that
/// replaces it in M2 — are separate crate targets linking the lib, so crate visibility can't reach
/// them. It is the production dispatch surface, not a parity twin of a JS export.
pub fn skillRunWithContext(input: &Value, db: &Connection, context: &Value) -> Result<Value, String> {
    let actor = context.get("actor").and_then(Value::as_str).unwrap_or("unknown");
    let source = context.get("source").and_then(Value::as_str);
    let user_goal_id = context.get("user_goal_id").cloned().unwrap_or(Value::Null);
    let agent = context.get("agent").and_then(Value::as_str).filter(|s| !s.is_empty());
    let auth_token = context.get("auth_token").cloned().unwrap_or(Value::Null);
    run_action(input, db, actor, source, &user_goal_id, agent, &auth_token, context)
}

#[allow(clippy::too_many_arguments)]
fn run_action(
    input: &Value,
    db: &Connection,
    actor: &str,
    source: Option<&str>,
    user_goal_id: &Value,
    agent: Option<&str>,
    auth_token: &Value,
    hook_ctx: &Value,
) -> Result<Value, String> {
    let action_name: Option<String> = match input {
        Value::String(s) => Some(s.clone()),
        _ => input.get("action").and_then(|v| v.as_str()).map(|s| s.to_string()),
    };
    let skill_input: Option<Value> = match input {
        Value::String(_) => None,
        _ => input.get("input").cloned(),
    };

    if action_name.as_deref().unwrap_or("").is_empty() {
        return Err("action-run: action is required".into());
    }
    let action_name = action_name.unwrap();

    let rows = crate::db_json::query_rows(db, "SELECT * FROM actions WHERE name = ?", &[json!(action_name)])?;
    let action = match rows.as_array().and_then(|a| a.first()) {
        Some(a) => a.clone(),
        None => return Err(format!("action-run: action '{action_name}' not found")),
    };

    // ── Permission + auth enforcement (M4b-1) ────────────────────────────────────────────────
    // Port of action-run.mjs's block at the same position (after the action lookup, before the
    // law loop). Opt-in on `agent` exactly like JS's `if (context?.agent)`, so callers that carry
    // no identity — every skillRunAs call site, and the conform harness's bare `{db}` lane — are
    // unaffected. Errors use the crate's `{skill}: {cause}` shape, matching JS's PERMISSION_DENIED
    // AntilokiError whose message renders as "action-run: <cause>".
    if let Some(agent_name) = agent {
        let check = crate::entities_agent_permission_check::agentPermissionCheck(
            &json!({ "agent_name": agent_name, "action_name": action_name }),
            db,
        )?;
        if !truthy(check.get("allowed")) {
            let reason = check.get("reason").and_then(Value::as_str).unwrap_or("permission denied");
            return Err(format!("action-run: {reason}"));
        }
        if truthy(Some(auth_token)) {
            let token_agent = auth_token.get("agent").and_then(Value::as_str).unwrap_or("");
            if token_agent != agent_name {
                return Err(format!(
                    "action-run: auth token agent '{token_agent}' does not match claimed agent '{agent_name}'"
                ));
            }
            let verified = crate::external_auth_verify::authVerify(auth_token, db)?;
            if !truthy(verified.get("valid")) {
                let reason = verified.get("reason").and_then(Value::as_str).unwrap_or("invalid");
                return Err(format!("action-run: auth token rejected: {reason}"));
            }
        }
    }

    // ── Law enforcement (hard laws with enforcement_hook) — ANTILOKI_SKIP_LAW_ENFORCEMENT unset ──
    let candidates = crate::entities_law_check::lawCheck(&json!({}), db)?;
    for law in candidates.as_array().cloned().unwrap_or_default() {
        let hook_name = law.get("enforcement_hook").and_then(|v| v.as_str()).unwrap_or("");
        let slug = law.get("slug").and_then(|v| v.as_str()).unwrap_or("");

        // Hook-name-not-in-dispatch-table stands in for JS's fail-closed "hook not found" branch
        // (getHook() returning null) — propagate immediately, do NOT swallow-and-continue. This
        // is structurally distinct from the hook ITSELF erroring (handled below), which JS's
        // catch(e) swallows into an 'escaped' record. Not fixture-tested (see module doc comment).
        if !KNOWN_HOOKS.contains(&hook_name) {
            return Err(format!(
                "action-run: enforcement_hook dispatch table has no entry for '{hook_name}' (extend as needed)"
            ));
        }

        let hook_input = json!({
            "action_name": action_name,
            // JS: `agent_name: context?.agent ?? null` — real now that context is threaded.
            "agent_name": agent.map(|a| json!(a)).unwrap_or(Value::Null),
            "input_data": skill_input.clone().unwrap_or(Value::Null),
        });
        let verdict = match dispatch_hook(hook_name, &hook_input, hook_ctx, db) {
            Ok(v) => v,
            Err(e) => {
                record_violation_swallowed(
                    db,
                    law.get("id").unwrap_or(&Value::Null),
                    &action_name,
                    actor,
                    json!({ "hook_error": e, "hook": hook_name }),
                    "escaped",
                );
                continue;
            }
        };

        let allowed_true = verdict.get("allowed") == Some(&Value::Bool(true));
        let allowed_false = verdict.get("allowed") == Some(&Value::Bool(false));

        if allowed_true && truthy(verdict.get("severity")) {
            let severity = verdict.get("severity").and_then(|v| v.as_str()).unwrap_or("");
            let outcome = match severity {
                "warning" => "warned",
                "info" => "allowed_exception",
                _ => "allowed_exception",
            };
            let mut ctxo = serde_json::Map::new();
            if let Some(r) = verdict.get("reason") {
                ctxo.insert("reason".into(), r.clone());
            }
            ctxo.insert("mode".into(), verdict.get("mode").cloned().unwrap_or(Value::Null));
            ctxo.insert("source".into(), json!(source));
            ctxo.insert("hook".into(), json!(hook_name));
            record_violation_swallowed(db, law.get("id").unwrap_or(&Value::Null), &action_name, actor, Value::Object(ctxo), outcome);
        }

        if allowed_false {
            let mut ctxo = serde_json::Map::new();
            if let Some(r) = verdict.get("reason") {
                ctxo.insert("reason".into(), r.clone());
            }
            ctxo.insert("hook".into(), json!(hook_name));
            if truthy(verdict.get("drift")) {
                ctxo.insert("drift".into(), verdict.get("drift").cloned().unwrap());
            }
            record_violation_swallowed(db, law.get("id").unwrap_or(&Value::Null), &action_name, actor, Value::Object(ctxo), "blocked");

            let reason_display = nz(verdict.get("reason"))
                .map(|v| tmpl(Some(v)))
                .unwrap_or_else(|| "no reason given".to_string());
            return Err(format!("action-run: law '{slug}' denied: {reason_display}"));
        }
    }

    let steps_json_str = action.get("steps_json").and_then(|v| v.as_str()).unwrap_or("null");
    let steps: Value = serde_json::from_str(steps_json_str)
        .map_err(|e| format!("action-run: invalid steps_json for '{action_name}': {e}"))?;

    let mut run_ctx = serde_json::Map::new();
    run_ctx.insert("input".into(), skill_input.clone().unwrap_or(Value::Null));
    run_ctx.insert("results".into(), json!({}));

    // ── runtime_events instrumentation — ANTILOKI_SKIP_RUNTIME_LOG unset → always runs ──────────
    // …EXCEPT FOR THE STATUS POLLS (Grokbot QA 2026-10-12, performance: "the Activity feed holds 323,684 events, workspace-list firing about once per millisecond"). Every dispatched action wrote a parent row
    // and one row per step, reads included, so a screen polling a status route all day filled the table (and the Activity screen's own refresh wrote two rows of its own). A pure status read changes
    // nothing, so it is not activity: no rows.
    let quiet = is_quiet_read(&action_name);
    let parent_log_input = json!({
        "parent_event_id": Value::Null,
        "actor": actor,
        "agent": Value::Null,
        "action": action_name,
        "outcome": "in_flight",
        "user_goal_id": user_goal_id,
        "source": json!(source),
    });
    let parent_event_id = if quiet { Value::Null } else { crate::engine_log_event::logEvent(&parent_log_input, db).unwrap_or(Value::Null) };
    run_ctx.insert("parent_event_id".into(), parent_event_id.clone());

    let mut final_outcome = "success";
    let mut final_err_code: Value = Value::Null;
    let mut loop_err: Option<String> = None;

    for step in steps.as_array().cloned().unwrap_or_default() {
        let step_skill = step.get("skill").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let step_input_spec = step.get("input").cloned().unwrap_or(Value::Null);
        let resolved_input = deepResolve(&step_input_spec, &Value::Object(run_ctx.clone()));

        let mut step_outcome = "success";
        let mut step_err_code: Value = Value::Null;
        let dispatch_result = dispatch_step_skill(&step_skill, &resolved_input, db, actor);

        match &dispatch_result {
            Ok(result) => {
                if let Some(as_name) = step.get("as").and_then(|v| v.as_str()) {
                    if let Some(results_map) = run_ctx.get_mut("results").and_then(|v| v.as_object_mut()) {
                        results_map.insert(as_name.to_string(), result.clone());
                    }
                }
            }
            Err(_e) => {
                step_outcome = "error";
                // JS: e?.code ?? e?.name ?? 'ERROR' — our Err(String) carries no .code/.name, so
                // this is a flat fallback (unfixtured branch, see module doc comment).
                step_err_code = json!("ERROR");
            }
        }

        let step_log_input = json!({
            "parent_event_id": parent_event_id,
            "actor": actor,
            "agent": Value::Null,
            "skill": step_skill,
            "duration_ms": 0.0,
            "outcome": step_outcome,
            "error_code": step_err_code,
            "user_goal_id": user_goal_id,
            "source": json!(source),
        });
        if !quiet { let _ = crate::engine_log_event::logEvent(&step_log_input, db); }

        if let Err(e) = dispatch_result {
            loop_err = Some(e);
            break;
        }
    }

    if loop_err.is_some() {
        final_outcome = "error";
        final_err_code = json!("ERROR");
    }

    if !parent_event_id.is_null() {
        let _ = db.execute(
            "UPDATE runtime_events SET outcome = ?, duration_ms = ?, error_code = ? WHERE id = ?",
            params![final_outcome, 0.0, final_err_code.as_str(), parent_event_id.as_i64()],
        );
    }

    if let Some(e) = loop_err {
        return Err(e);
    }

    let returns = action.get("returns").cloned();
    if truthy(returns.as_ref()) {
        Ok(resolvePath(&returns.unwrap(), &Value::Object(run_ctx)))
    } else {
        Ok(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIONS_SCHEMA: &str = "CREATE TABLE actions (name TEXT UNIQUE, description TEXT, owner_agent TEXT, steps_json TEXT, returns TEXT, on_failure TEXT, kind TEXT)";
    const RUNTIME_EVENTS_SCHEMA: &str = "CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', parent_event_id INTEGER, actor TEXT NOT NULL, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, outcome TEXT NOT NULL CHECK (outcome IN ('success','error','blocked','skipped','in_flight')), error_code TEXT, context_json TEXT, user_goal_id INTEGER, source TEXT)";
    const CARDS_SCHEMA: &str = "CREATE TABLE cards (slug TEXT, status TEXT, score REAL)";
    const LAWS_SCHEMA: &str = "CREATE TABLE laws.laws (id INTEGER PRIMARY KEY, slug TEXT, kind TEXT, domain TEXT, statement TEXT, enforcement TEXT, enforcement_hook TEXT, superseded_by INTEGER)";
    const LAW_VIOLATIONS_SCHEMA: &str = "CREATE TABLE laws.law_violations (id INTEGER PRIMARY KEY, law_id INTEGER NOT NULL, attempted_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', attempted_by TEXT NOT NULL, agent_name TEXT, action_name TEXT, context_json TEXT, outcome TEXT NOT NULL, CHECK (outcome IN ('blocked', 'warned', 'escaped', 'allowed_exception')))";

    fn seeded_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws;").unwrap();
        c.execute_batch(&format!(
            "{ACTIONS_SCHEMA}; {RUNTIME_EVENTS_SCHEMA}; {CARDS_SCHEMA}; {LAWS_SCHEMA}; {LAW_VIOLATIONS_SCHEMA};
             INSERT INTO cards VALUES ('A','doing',5),('B','backlog',NULL);
             INSERT INTO actions (name, description, owner_agent, steps_json, returns, kind) VALUES
               ('board-keeper.card.list', 'list cards', 'board-keeper', '[{{\"skill\":\"card-list\",\"input\":{{}},\"as\":\"cards\"}}]', '$.results.cards', 'atomic'),
               ('card-add-raw', 'raw mutator', 'system', '[{{\"skill\":\"card-list\",\"input\":{{}},\"as\":\"cards\"}}]', '$.results.cards', 'atomic');"
        ))
        .unwrap();
        c
    }

    #[test]
    fn happy_path_no_laws_dispatches_and_returns() {
        let c = seeded_db();
        let result = skillRun(&json!({ "action": "board-keeper.card.list" }), &c).unwrap();
        assert_eq!(result.as_array().unwrap().len(), 2);

        let events = crate::db_json::query_rows(&c, "SELECT * FROM runtime_events ORDER BY id", &[]).unwrap();
        let rows = events.as_array().unwrap();
        assert_eq!(rows.len(), 2); // parent (in_flight → success) + one step row
        assert_eq!(rows[0]["outcome"], json!("success"));
        assert_eq!(rows[0]["duration_ms"], json!(0.0));
        assert_eq!(rows[1]["skill"], json!("card-list"));
        assert_eq!(rows[1]["outcome"], json!("success"));
        assert_eq!(rows[1]["parent_event_id"], rows[0]["id"]);
    }

    #[test]
    fn a_status_poll_leaves_no_rows_in_the_activity_table_and_a_real_action_still_does() {
        let c = seeded_db();
        c.execute(
            "INSERT INTO actions (name, description, owner_agent, steps_json, returns, kind) VALUES ('system.workspace.list', 'list', 'system', '[{\"skill\":\"card-list\",\"input\":{},\"as\":\"cards\"}]', '$.results.cards', 'atomic')",
            [],
        ).unwrap();
        let r = skillRun(&json!({ "action": "system.workspace.list" }), &c).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 2, "the poll still answers");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM runtime_events", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 0, "a status read is not activity");
        skillRun(&json!({ "action": "board-keeper.card.list" }), &c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM runtime_events", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 2, "any other action is recorded as before");
        assert!(is_quiet_read("system.runtime.events") && !is_quiet_read("system.driver.tick") && !is_quiet_read("system.cockpit.pane.update"));
    }

    // ── M4b-1: permission/auth enforcement (only reachable via skillRunWithContext) ───────────

    fn with_grants(c: &Connection) {
        c.execute_batch(
            "CREATE TABLE agent_actions (id INTEGER PRIMARY KEY, agent_name TEXT, action_name TEXT, revoked_at TEXT);
             INSERT INTO agent_actions (agent_name, action_name, revoked_at) VALUES
               ('board-keeper','board-keeper.card.list',NULL),
               ('dev','board-keeper.card.list','2026-01-01 00:00:00');",
        )
        .unwrap();
    }

    #[test]
    fn no_agent_in_context_skips_auth_entirely() {
        // the conform lane and all 13 skillRunAs call sites live here — no agent_actions table at
        // all, so any unconditional permission check would error rather than pass.
        let c = seeded_db();
        let r = skillRunWithContext(&json!({ "action": "board-keeper.card.list" }), &c, &json!({})).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 2);
    }

    #[test]
    fn agent_with_active_grant_dispatches() {
        let c = seeded_db();
        with_grants(&c);
        let ctx = json!({ "agent": "board-keeper", "actor": "human:cli", "source": "cli" });
        let r = skillRunWithContext(&json!({ "action": "board-keeper.card.list" }), &c, &ctx).unwrap();
        assert_eq!(r.as_array().unwrap().len(), 2);
        // actor/source threaded from the context, not the skillRunAs defaults
        let ev = crate::db_json::query_rows(&c, "SELECT * FROM runtime_events ORDER BY id", &[]).unwrap();
        assert_eq!(ev[0]["actor"], json!("human:cli"));
        assert_eq!(ev[0]["source"], json!("cli"));
    }

    #[test]
    fn agent_without_grant_is_refused_before_any_step_runs() {
        let c = seeded_db();
        with_grants(&c);
        let ctx = json!({ "agent": "intruder" });
        let err = skillRunWithContext(&json!({ "action": "board-keeper.card.list" }), &c, &ctx).unwrap_err();
        assert!(err.starts_with("action-run: "), "{err}");
        assert!(err.contains("no active grant"), "{err}");
        // refused BEFORE the steps loop — no runtime_events row was written at all
        let ev = crate::db_json::query_rows(&c, "SELECT * FROM runtime_events", &[]).unwrap();
        assert_eq!(ev.as_array().unwrap().len(), 0);
    }

    #[test]
    fn revoked_grant_is_refused() {
        let c = seeded_db();
        with_grants(&c);
        let err = skillRunWithContext(
            &json!({ "action": "board-keeper.card.list" }),
            &c,
            &json!({ "agent": "dev" }),
        )
        .unwrap_err();
        assert!(err.contains("no active grant"), "{err}");
    }

    #[test]
    fn auth_token_claiming_a_different_agent_is_refused() {
        let c = seeded_db();
        with_grants(&c);
        let ctx = json!({
            "agent": "board-keeper",
            "auth_token": { "agent": "dev", "nonce": "n", "ts": 1, "sig": "deadbeef" }
        });
        let err = skillRunWithContext(&json!({ "action": "board-keeper.card.list" }), &c, &ctx).unwrap_err();
        assert!(err.contains("does not match claimed agent 'board-keeper'"), "{err}");
    }

    #[test]
    fn auth_token_with_bad_signature_is_refused() {
        let c = seeded_db();
        with_grants(&c);
        let ctx = json!({
            "agent": "board-keeper",
            "auth_token": { "agent": "board-keeper", "nonce": "n", "ts": 1, "sig": "not-a-real-hmac" }
        });
        let err = skillRunWithContext(&json!({ "action": "board-keeper.card.list" }), &c, &ctx).unwrap_err();
        assert!(err.contains("auth token rejected"), "{err}");
    }

    #[test]
    fn law_hook_allows_then_steps_still_dispatch() {
        let c = seeded_db();
        c.execute_batch(
            "INSERT INTO laws.laws (id,slug,kind,domain,statement,enforcement,enforcement_hook,superseded_by) VALUES
               (1,'NEVER_BYPASS_WITH_AUDIT','constitutional','audit','All mutations go through with-audit','hard','law-hook-mutation-via-with-audit',NULL);",
        )
        .unwrap();
        let result = skillRun(&json!({ "action": "board-keeper.card.list" }), &c).unwrap();
        assert_eq!(result.as_array().unwrap().len(), 2);

        let violations = crate::db_json::query_rows(&c, "SELECT * FROM laws.law_violations", &[]).unwrap();
        assert_eq!(violations.as_array().unwrap().len(), 0); // allowed w/o severity → no record
    }

    #[test]
    fn law_hook_denies_before_any_dispatch_or_logging() {
        let c = seeded_db();
        c.execute_batch(
            "INSERT INTO laws.laws (id,slug,kind,domain,statement,enforcement,enforcement_hook,superseded_by) VALUES
               (1,'NEVER_BYPASS_WITH_AUDIT','constitutional','audit','All mutations go through with-audit','hard','law-hook-mutation-via-with-audit',NULL);",
        )
        .unwrap();
        let err = skillRun(&json!({ "action": "card-add-raw" }), &c).unwrap_err();
        assert_eq!(
            err,
            "action-run: law 'NEVER_BYPASS_WITH_AUDIT' denied: 'card-add-raw' is an internal mutator — must be called through its with-audit-composing wrapper"
        );

        let events = crate::db_json::query_rows(&c, "SELECT * FROM runtime_events", &[]).unwrap();
        assert_eq!(events.as_array().unwrap().len(), 0); // denied before runtime logging ever starts

        let violations = crate::db_json::query_rows(&c, "SELECT * FROM laws.law_violations", &[]).unwrap();
        let rows = violations.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["outcome"], json!("blocked"));
        assert_eq!(rows[0]["law_id"], json!(1));
    }

    #[test]
    fn action_not_found() {
        let c = seeded_db();
        let err = skillRun(&json!({ "action": "nope.action" }), &c).unwrap_err();
        assert_eq!(err, "action-run: action 'nope.action' not found");
    }

    #[test]
    fn missing_action_name_required() {
        let c = seeded_db();
        assert_eq!(skillRun(&json!({}), &c).unwrap_err(), "action-run: action is required");
        assert_eq!(skillRun(&json!({ "action": "" }), &c).unwrap_err(), "action-run: action is required");
    }

    #[test]
    fn resolve_path_and_deep_resolve() {
        let ctx = json!({ "input": { "slug": "A", "title": "T" } });
        assert_eq!(resolvePath(&json!("$.input.slug"), &ctx), json!("A"));
        assert_eq!(resolvePath(&json!("plain"), &json!({})), json!("plain"));
        assert_eq!(resolvePath(&json!(42), &json!({})), json!(42));
        assert_eq!(resolvePath(&json!("$.input.missing.deep"), &ctx), Value::Null);

        let out = deepResolve(
            &json!({ "a": "$.input.slug", "nested": { "b": "$.input.title", "literal": "x" }, "arr": ["$.input.slug", "plain"] }),
            &ctx,
        );
        assert_eq!(
            out,
            json!({ "a": "A", "nested": { "b": "T", "literal": "x" }, "arr": ["A", "plain"] })
        );
    }
}
