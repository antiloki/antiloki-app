//! tl-refine-microtasks.rs — twin of skills/agents/tl/tl-refine-microtasks.mjs.
//! NAMES ARE CONTRACT: `tlRefineMicrotasks`, `hasAuthorableDefs`.
//!
//! Back-compat wrapper over the stage-split pair (tl-plan-contract / tl-author-snippets).
//! Plans when the contract has no usable per-file defs, then authors. BESPOKE-NATIVE:
//! dispatches to the two aiCall-driving twin skills below (deferred boundary lives in them).

use rusqlite::Connection;
use serde_json::{json, Value};

fn str_qq(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// JS: `hasAuthorableDefs(defs)` — a def is authorable when it has a real target file
/// + a real spec (the v11 plan contract shape).
pub fn hasAuthorableDefs(defs: &Value) -> bool {
    let arr = match defs.as_array() {
        Some(a) if !a.is_empty() => a,
        _ => return false,
    };
    arr.iter().all(|d| {
        let file = d.get("file").and_then(Value::as_str).unwrap_or("");
        let file_spec_md = d.get("file_spec_md").and_then(Value::as_str).unwrap_or("");
        !file.is_empty() && file_spec_md.len() >= 80
    })
}

pub fn tlRefineMicrotasks(input: &Value, db: &Connection) -> Result<Value, String> {
    let contract_slug = str_qq(input.get("contract_slug"));
    if contract_slug.is_empty() {
        return Err("tl-refine-microtasks: contract_slug is required".to_string());
    }

    let contract = crate::contract_contract_get::contractGet(&json!({ "slug": contract_slug.clone() }), db)?;
    if contract.is_null() {
        return Err(format!("tl-refine-microtasks: contract '{contract_slug}' not found"));
    }

    let defs = crate::engine_json_utils::loadJson(&contract.get("microtasks_json").cloned().unwrap_or(Value::Null), &json!([]));
    let need_plan = !hasAuthorableDefs(&defs);

    let mut plan_result: Option<Value> = None;
    if need_plan {
        let plan = crate::agents_tl_tl_plan_contract::tlPlanContract(input, db)?;
        // Honest refusal: the contract was marked rejected — nothing to author.
        if plan.get("empty_refusal").map_or(false, |v| v.as_bool().unwrap_or(false)) {
            return Ok(plan);
        }
        plan_result = Some(plan);
    }

    let authored = crate::agents_tl_tl_author_snippets::tlAuthorSnippets(input, db)?;

    let authored_scope_expanded = authored.get("scope_expanded").map_or(false, |v| v.as_bool().unwrap_or(false));
    let plan_scope_expanded = plan_result.as_ref()
        .and_then(|p| p.get("scope_expanded"))
        .map_or(false, |v| v.as_bool().unwrap_or(false));

    let plan_new_paths_added = plan_result.as_ref().and_then(|p| p.get("new_paths_added")).cloned();
    let authored_new_paths_added = authored.get("new_paths_added").cloned().unwrap_or(json!([]));
    let new_paths_added = match &plan_new_paths_added {
        Some(v) if v.as_array().map_or(false, |a| !a.is_empty()) => v.clone(),
        _ => authored_new_paths_added,
    };

    let bootstrap_cmd = plan_result.as_ref()
        .and_then(|p| p.get("bootstrap_cmd"))
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| authored.get("bootstrap_cmd").cloned().unwrap_or(Value::Null));

    let generated_paths = plan_result.as_ref()
        .and_then(|p| p.get("generated_paths"))
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| authored.get("generated_paths").cloned().unwrap_or(Value::Null));

    let ai_call_id = authored.get("ai_call_id")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| plan_result.as_ref().and_then(|p| p.get("ai_call_id")).cloned().unwrap_or(Value::Null));

    let plan_ai_call_id = plan_result.as_ref().and_then(|p| p.get("ai_call_id")).cloned().unwrap_or(Value::Null);

    let mut out = match authored {
        Value::Object(m) => m,
        _ => Default::default(),
    };
    out.insert("scope_expanded".to_string(), json!(authored_scope_expanded || plan_scope_expanded));
    out.insert("new_paths_added".to_string(), new_paths_added);
    out.insert("bootstrap_cmd".to_string(), bootstrap_cmd);
    out.insert("generated_paths".to_string(), generated_paths);
    out.insert("ai_call_id".to_string(), ai_call_id);
    out.insert("planned".to_string(), json!(need_plan));
    out.insert("plan_ai_call_id".to_string(), plan_ai_call_id);

    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn has_authorable_defs_empty_is_false() {
        assert!(!hasAuthorableDefs(&json!([])));
        assert!(!hasAuthorableDefs(&json!(null)));
    }

    #[test]
    fn has_authorable_defs_requires_file_and_spec() {
        assert!(!hasAuthorableDefs(&json!([{ "file": "a.mjs" }])));
        assert!(!hasAuthorableDefs(&json!([{ "file": "a.mjs", "file_spec_md": "short" }])));
        let long_spec = "x".repeat(80);
        assert!(hasAuthorableDefs(&json!([{ "file": "a.mjs", "file_spec_md": long_spec }])));
    }

    #[test]
    fn tl_refine_microtasks_requires_contract_slug() {
        let conn = Connection::open_in_memory().unwrap();
        let err = tlRefineMicrotasks(&json!({}), &conn).unwrap_err();
        assert!(err.contains("contract_slug is required"));
    }
}
