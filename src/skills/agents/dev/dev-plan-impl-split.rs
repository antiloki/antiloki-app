//! dev-plan-impl-split.rs — twin of skills/agents/dev/dev-plan-impl-split.mjs. NAMES ARE CONTRACT:
//! `devPlanImplSplit` (default, standard lane) · `topoSort` (no-db helper, named export). CHEAP-side
//! orchestrator: materializes a plan (inline or from contract.microtasks_json), topo-sorts the
//! microtasks by depends_on, then iterates them through dev-exec-sandbox + dev-verify-chain
//! (reused twins), bailing on first failure when fail_fast.
//!
//! PARITY-DEFERRED: `current_files_for` is a JS callback param — it cannot cross the JSON fixture
//! boundary, so this twin always takes the `typeof current_files_for === 'function'` false branch
//! (current_files = {}), same convention as every other callback-shaped param in the crossing.
//! `context.provider_fn` is likewise JS-only plumbing threaded to devExecSandbox's context; the
//! Rust devExecSandbox twin has no such parameter (its own aiCall leg is the deferred stub), so it
//! is not represented here either.
// QUALITY: topo_sort ported with Map-insertion-order semantics (first-occurrence key order, dedup
// via a parallel visited set standing in for `order.find`); main loop reuses the exec/verify/
// contract-get twins verbatim, mirroring the try/catch-only-around-exec control flow.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn to_fixed(x: f64, decimals: usize) -> f64 { format!("{:.*}", decimals, x).parse::<f64>().unwrap_or(x) } // JS `+x.toFixed(d)`

/// `function loadPlanFromContract(db, contract_slug)` — materializes a plan from
/// contract.microtasks_json when the caller supplies contract_slug instead of an inline plan.
fn load_plan_from_contract(db: &Connection, contract_slug: &str) -> Result<Value, String> {
    let contract = crate::contract_contract_get::contractGet(&json!({ "slug": contract_slug }), db)?;
    if contract.is_null() {
        return Err(format!("dev-plan-impl-split: contract '{contract_slug}' not found"));
    }
    // contract-get's JSON_COLS does NOT include microtasks_json, so it survives on the row as the
    // raw TEXT column — parse it here, same as loadJson(contract.microtasks_json, []).
    let microtasks = contract
        .get("microtasks_json")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or(json!([]));
    let microtasks_arr = microtasks.as_array().cloned().unwrap_or_default();
    if microtasks_arr.is_empty() {
        return Err(format!(
            "dev-plan-impl-split: contract '{contract_slug}' has no microtasks. Run tl.refine.microtasks before dev.plan.impl.split."
        ));
    }
    let total_max_cost_usd: f64 = microtasks_arr
        .iter()
        .map(|m| { let v = js_number(m.get("max_cost_usd")); if v.is_nan() { 0.0 } else { v } })
        .sum::<f64>()
        + 1e-6;

    let slug = contract.get("slug").cloned().unwrap_or(Value::Null);
    let card_slug = nz(contract.get("card_slug")).or_else(|| nz(contract.get("slug"))).cloned().unwrap_or(Value::Null);
    let plan_id_slug = slug.as_str().unwrap_or("");
    Ok(json!({
        "plan_id": format!("plan-{plan_id_slug}-from-contract"),
        "card_slug": card_slug,
        "total_max_cost_usd": total_max_cost_usd,
        "microtasks": microtasks_arr,
    }))
}

/// no-db helper: `topoSort(microtasks)` — plain Kahn's algorithm over `depends_on`, ported with
/// JS `Map` insertion-order semantics (first-occurrence key order for the `incoming`/`byId` maps).
pub fn topoSort(microtasks: &Value) -> Result<Value, String> {
    let empty: Vec<Value> = Vec::new();
    let mts = microtasks.as_array().unwrap_or(&empty);

    let mut id_order: Vec<String> = Vec::new();
    let mut by_id: HashMap<String, Value> = HashMap::new();
    let mut incoming: HashMap<String, HashSet<String>> = HashMap::new();
    for m in mts {
        let id = m.get("microtask_id").and_then(Value::as_str).unwrap_or("").to_string();
        if !by_id.contains_key(&id) { id_order.push(id.clone()); } // Map.set on existing key keeps original position
        by_id.insert(id.clone(), m.clone());
        let deps: HashSet<String> = m
            .get("depends_on")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|d| d.as_str().map(String::from)).collect())
            .unwrap_or_default();
        incoming.insert(id, deps);
    }

    let mut ready: VecDeque<Value> = mts
        .iter()
        .filter(|m| m.get("depends_on").and_then(Value::as_array).map_or(0, |a| a.len()) == 0)
        .cloned()
        .collect();

    let mut order: Vec<Value> = Vec::new();
    let mut in_order: HashSet<String> = HashSet::new(); // stands in for `order.find`

    while let Some(next) = ready.pop_front() {
        let next_id = next.get("microtask_id").and_then(Value::as_str).unwrap_or("").to_string();
        order.push(next);
        in_order.insert(next_id.clone());

        for other_id in &id_order {
            let ready_to_push = if let Some(deps) = incoming.get_mut(other_id) {
                let deleted = deps.remove(&next_id);
                deleted && deps.is_empty() && other_id != &next_id && !in_order.contains(other_id)
            } else {
                false
            };
            if ready_to_push {
                if let Some(mval) = by_id.get(other_id) { ready.push_back(mval.clone()); }
            }
        }
    }

    if order.len() != mts.len() {
        return Err(format!(
            "dev-plan-impl-split: cycle detected in plan microtasks (sorted {}/{})",
            order.len(),
            mts.len()
        ));
    }
    Ok(json!(order))
}

/// `export default async function devPlanImplSplit(input, context)`.
pub fn devPlanImplSplit(input: &Value, db: &Connection) -> Result<Value, String> {
    let empty_input = json!({});
    let input = input.as_object().map(|_| input).unwrap_or(&empty_input);

    let inline_plan = nz(input.get("plan"));
    let contract_slug = input.get("contract_slug").and_then(Value::as_str).filter(|s| !s.is_empty());
    let tier = input.get("tier").and_then(Value::as_str).unwrap_or("low").to_string();
    let sample_ai_verify_every = js_number(input.get("sample_ai_verify_every"));
    let sample_ai_verify_every = if sample_ai_verify_every.is_nan() { 0.0 } else { sample_ai_verify_every };
    let fail_fast = match input.get("fail_fast") { None => true, Some(v) => truthy(Some(v)) };
    // current_files_for: JS callback param, not JSON-representable — always the non-function branch.

    let plan: Value = if let Some(p) = inline_plan {
        p.clone()
    } else if let Some(slug) = contract_slug {
        load_plan_from_contract(db, slug)?
    } else {
        Value::Null
    };
    if !truthy(Some(&plan)) {
        return Err("dev-plan-impl-split: plan or contract_slug is required".to_string());
    }

    crate::agents_dev_dev_plan_spec::devPlanSpecValidate(&json!({ "plan": plan.clone() }), db)?;

    let microtasks_val = plan.get("microtasks").cloned().unwrap_or(json!([]));
    let ordered_val = topoSort(&microtasks_val)?;
    let ordered = ordered_val.as_array().cloned().unwrap_or_default();

    let mut results: Vec<Value> = Vec::new();
    let mut total_cost_usd = 0.0_f64;
    let mut total_duration_ms = 0.0_f64;
    let mut bailed = false;

    for i in 0..ordered.len() {
        let microtask = &ordered[i];
        let microtask_id = microtask.get("microtask_id").cloned().unwrap_or(Value::Null);
        let current_files = json!({}); // deferred boundary: current_files_for never present in JSON fixtures

        let exec_input = json!({ "microtask": microtask, "current_files": current_files, "tier": tier });
        let exec_result = match crate::agents_dev_dev_exec_sandbox::devExecSandbox(&exec_input, db) {
            Ok(v) => v,
            Err(e) => {
                results.push(json!({
                    "microtask_id": microtask_id,
                    "ok": false,
                    "stage": "exec",
                    "error": e,
                }));
                bailed = fail_fast;
                if fail_fast { break; } else { continue; }
            }
        };
        total_cost_usd += exec_result.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
        total_duration_ms += exec_result.get("duration_ms").and_then(Value::as_f64).unwrap_or(0.0);

        let sample = sample_ai_verify_every > 0.0 && ((i as f64 + 1.0) % sample_ai_verify_every) == 0.0;
        let patches = exec_result.get("patches").cloned().unwrap_or(Value::Null);
        let patches_count = patches.as_array().map(|a| a.len()).unwrap_or(0) as i64;
        let verify_input = json!({ "microtask": microtask, "patches": patches, "sample_ai_verify": sample });
        let verify_result = crate::agents_dev_dev_verify_chain::devVerifyChain(&verify_input, db)?;

        let ok = verify_result.get("ok").and_then(Value::as_bool).unwrap_or(false);
        results.push(json!({
            "microtask_id": microtask_id,
            "ok": ok,
            "patches_count": patches_count,
            "cost_usd": exec_result.get("cost_usd").cloned().unwrap_or(Value::Null),
            "duration_ms": exec_result.get("duration_ms").cloned().unwrap_or(Value::Null),
            "findings": verify_result.get("findings").cloned().unwrap_or(Value::Null),
            "ai_verify_recommended": verify_result.get("sampled_ai_verify_recommendation").cloned().unwrap_or(Value::Null),
        }));

        if !ok && fail_fast { bailed = true; break; }
    }

    let ok_count = results.iter().filter(|r| r.get("ok").and_then(Value::as_bool).unwrap_or(false)).count() as i64;

    Ok(json!({
        "plan_id": plan.get("plan_id").cloned().unwrap_or(Value::Null),
        "card_slug": plan.get("card_slug").cloned().unwrap_or(Value::Null),
        "microtask_count": ordered.len() as i64,
        "completed_count": results.len() as i64,
        "ok_count": ok_count,
        "bailed": bailed,
        "total_cost_usd": to_fixed(total_cost_usd, 6),
        "total_duration_ms": to_fixed(total_duration_ms, 2),
        "results": results,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRAIN_SCHEMA: &str = "
        CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
        CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
        CREATE TABLE brain.ai_call_outputs ( id INTEGER PRIMARY KEY, ai_call_id INTEGER NOT NULL, parent_output_id INTEGER, field_name TEXT NOT NULL, order_index INTEGER NOT NULL DEFAULT 0, value_text TEXT, value_number REAL, value_boolean INTEGER );
    ";

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(BRAIN_SCHEMA).unwrap();
        c.execute_batch("CREATE TABLE contracts (slug TEXT, card_slug TEXT, microtasks_json TEXT);").unwrap();
        c
    }

    fn mt(id: &str, deps: Vec<&str>) -> Value {
        json!({
            "microtask_id": id, "intent": "do", "files_touched": ["a.ts"],
            "acceptance": ["x"], "max_tokens": 512, "max_cost_usd": 0.05,
            "expected_diff_shape": "small_edit", "depends_on": deps,
        })
    }

    #[test]
    fn topo_sort_orders_by_depends_on() {
        let mts = json!([mt("b", vec!["a"]), mt("a", vec![]), mt("c", vec!["b"])]);
        let ordered = topoSort(&mts).unwrap();
        let ids: Vec<&str> = ordered.as_array().unwrap().iter().map(|m| m["microtask_id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn topo_sort_detects_cycle() {
        let mts = json!([mt("a", vec!["b"]), mt("b", vec!["a"])]);
        let e = topoSort(&mts).unwrap_err();
        assert_eq!(e, "dev-plan-impl-split: cycle detected in plan microtasks (sorted 0/2)");
    }

    #[test]
    fn missing_plan_and_contract_slug_is_rejected() {
        let e = devPlanImplSplit(&json!({}), &db()).unwrap_err();
        assert_eq!(e, "dev-plan-impl-split: plan or contract_slug is required");
    }

    #[test]
    fn unknown_contract_slug_is_rejected() {
        let e = devPlanImplSplit(&json!({ "contract_slug": "ghost" }), &db()).unwrap_err();
        assert_eq!(e, "dev-plan-impl-split: contract 'ghost' not found");
    }

    #[test]
    fn contract_with_no_microtasks_is_rejected() {
        let c = db();
        c.execute("INSERT INTO contracts (slug, card_slug, microtasks_json) VALUES ('K', 'CARD-1', '[]')", []).unwrap();
        let e = devPlanImplSplit(&json!({ "contract_slug": "K" }), &c).unwrap_err();
        assert_eq!(e, "dev-plan-impl-split: contract 'K' has no microtasks. Run tl.refine.microtasks before dev.plan.impl.split.");
    }

    #[test]
    fn invalid_inline_plan_is_rejected_by_spec_validate() {
        let plan = json!({ "plan_id": "p1", "card_slug": "C-1", "total_max_cost_usd": 0.1, "microtasks": [] });
        let e = devPlanImplSplit(&json!({ "plan": plan }), &db()).unwrap_err();
        assert!(e.starts_with("plan validation failed: "), "{e}");
    }

    // aiCall is the deferred stub (always Err) — the exec step always fails, so a valid plan
    // exercises the try/catch-around-exec bail path deterministically end-to-end.
    #[test]
    fn valid_plan_bails_on_first_exec_failure_when_fail_fast() {
        let plan = json!({
            "plan_id": "p1", "card_slug": "C-1", "total_max_cost_usd": 1.0,
            "microtasks": [mt("m1", vec![]), mt("m2", vec!["m1"])],
        });
        let r = devPlanImplSplit(&json!({ "plan": plan }), &db()).unwrap();
        assert_eq!(r["plan_id"], json!("p1"));
        assert_eq!(r["card_slug"], json!("C-1"));
        assert_eq!(r["microtask_count"], json!(2));
        assert_eq!(r["completed_count"], json!(1));
        assert_eq!(r["ok_count"], json!(0));
        assert_eq!(r["bailed"], json!(true));
        assert_eq!(r["results"][0]["microtask_id"], json!("m1"));
        assert_eq!(r["results"][0]["ok"], json!(false));
        assert_eq!(r["results"][0]["stage"], json!("exec"));
        assert!(r["results"][0]["error"].as_str().unwrap().starts_with("dev-exec-sandbox: microtask exec failed: "));
    }

    #[test]
    fn valid_plan_continues_past_exec_failures_when_not_fail_fast() {
        let plan = json!({
            "plan_id": "p1", "card_slug": "C-1", "total_max_cost_usd": 1.0,
            "microtasks": [mt("m1", vec![]), mt("m2", vec!["m1"])],
        });
        let r = devPlanImplSplit(&json!({ "plan": plan, "fail_fast": false }), &db()).unwrap();
        assert_eq!(r["completed_count"], json!(2));
        assert_eq!(r["ok_count"], json!(0));
        assert_eq!(r["bailed"], json!(false));
    }

    #[test]
    fn loads_plan_from_contract_and_computes_total_max_cost_usd() {
        let c = db();
        let microtasks = json!([mt("m1", vec![])]);
        c.execute(
            "INSERT INTO contracts (slug, card_slug, microtasks_json) VALUES ('K', 'CARD-1', ?)",
            rusqlite::params![microtasks.to_string()],
        ).unwrap();
        let r = devPlanImplSplit(&json!({ "contract_slug": "K", "fail_fast": true }), &c).unwrap();
        assert_eq!(r["plan_id"], json!("plan-K-from-contract"));
        assert_eq!(r["card_slug"], json!("CARD-1"));
        assert_eq!(r["microtask_count"], json!(1));
    }
}
