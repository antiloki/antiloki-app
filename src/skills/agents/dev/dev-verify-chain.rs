//! dev-verify-chain.rs — twin of skills/agents/dev/dev-verify-chain.mjs. NAMES ARE CONTRACT:
//! `devVerifyChain` (default, standard lane) · `checkPatches` (no-db helper). Deterministic patch gate.
// QUALITY: verifies a microtask's patches (scope/shape/op/content + JSON sanity) before promotion.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

// JS `${v}` interpolation for the values that appear (string/number/bool/null/absent).
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
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// no-db helper: checkPatches(microtask, patches) → array of { severity, code, message } findings.
pub fn checkPatches(microtask: &Value, patches: &Value) -> Value {
    let mut findings: Vec<Value> = Vec::new();

    // !Array.isArray(patches)
    let patches_arr = match patches.as_array() {
        None => {
            findings.push(json!({ "severity": "critical", "code": "NO_PATCHES", "message": "patches must be an array" }));
            return json!(findings);
        }
        Some(a) => a,
    };

    if patches_arr.is_empty() {
        findings.push(json!({ "severity": "warning", "code": "EMPTY_PATCHES", "message": "microtask produced zero patches" }));
    }

    // new Set(microtask.files_touched) — string members are all a string p.file can match.
    let allowed: HashSet<String> = microtask
        .get("files_touched")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();

    for (i, p) in patches_arr.iter().enumerate() {
        // !p || typeof p !== 'object' → caught unless p is an object OR an array (arrays are 'object' in JS).
        if !(p.is_object() || p.is_array()) {
            findings.push(json!({ "severity": "critical", "code": "PATCH_SHAPE", "message": format!("patch {} not an object", i) }));
            continue;
        }
        // !p.file || typeof p.file !== 'string' → passes only for a non-empty string file.
        let fname = match p.get("file").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            None => {
                findings.push(json!({ "severity": "critical", "code": "PATCH_FILE", "message": format!("patch {} missing file", i) }));
                continue;
            }
            Some(s) => s,
        };
        if !allowed.contains(fname) {
            findings.push(json!({ "severity": "critical", "code": "SCOPE_DRIFT", "message": format!("patch {} touches '{}' which is NOT in microtask.files_touched", i, fname) }));
        }
        // VALID_OPERATIONS = new Set(['create','replace','edit']); membership is by string value.
        let op_valid = p.get("operation").and_then(|v| v.as_str()).map_or(false, |s| s == "create" || s == "replace" || s == "edit");
        if !op_valid {
            findings.push(json!({ "severity": "critical", "code": "PATCH_OPERATION", "message": format!("patch {} operation '{}' must be one of: create, replace, edit", i, tmpl(p.get("operation"))) }));
        }
        // typeof p.content !== 'string' || p.content.length === 0 → passes only for a non-empty string.
        let content_ok = p.get("content").and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty());
        if !content_ok {
            findings.push(json!({ "severity": "critical", "code": "PATCH_CONTENT", "message": format!("patch {} content must be non-empty string", i) }));
        }
        // Syntax sanity: JSON patches must parse. fname is a non-empty string here; only run when content is a string.
        // NOTE: the failure message embeds V8's JSON.parse error text, which is not reproducible in Rust — the
        // invalid-JSON branch is deliberately NOT fixtured (only the valid-JSON success path is exercised).
        if fname.ends_with(".json") {
            if let Some(content) = p.get("content").and_then(|v| v.as_str()) {
                if serde_json::from_str::<Value>(content).is_err() {
                    findings.push(json!({ "severity": "critical", "code": "JSON_INVALID", "message": format!("patch {} JSON content is invalid: <parse error>", i) }));
                }
            }
        }
    }
    json!(findings)
}

// standard lane: default export devVerifyChain(input, context).
pub fn devVerifyChain(input: &Value, _db: &Connection) -> Result<Value, String> {
    // const { microtask, patches, sample_ai_verify = false } = input ?? {};
    let microtask = input.get("microtask");
    if !truthy(microtask) {
        return Err("dev-verify-chain: microtask is required".into());
    }
    let mt = microtask.cloned().unwrap_or(Value::Null);
    let pt = input.get("patches").cloned().unwrap_or(Value::Null); // undefined/null both → not-an-array in checkPatches

    let findings = checkPatches(&mt, &pt);
    let empty: Vec<Value> = Vec::new();
    let findings_arr = findings.as_array().unwrap_or(&empty);
    let criticals = findings_arr
        .iter()
        .filter(|f| f.get("severity").and_then(|s| s.as_str()) == Some("critical"))
        .count();

    // sample_ai_verify: destructuring default only replaces `undefined` → false; truthy(None)==false covers it.
    let requested = truthy(input.get("sample_ai_verify"));

    let mut out = Map::new();
    out.insert("ok".into(), json!(criticals == 0));
    out.insert("finding_count".into(), json!(findings_arr.len() as i64));
    out.insert("critical_count".into(), json!(criticals as i64));
    out.insert("findings".into(), findings.clone());
    out.insert("sampled_ai_verify_requested".into(), json!(requested));
    out.insert(
        "sampled_ai_verify_recommendation".into(),
        if requested {
            json!("caller should dispatch dev-exec-sandbox with critic prompt for this microtask")
        } else {
            Value::Null
        },
    );
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Connection { Connection::open_in_memory().unwrap() }

    #[test]
    fn check_patches_branches() {
        // not an array
        assert_eq!(
            checkPatches(&json!({ "files_touched": [] }), &json!("nope")),
            json!([{ "severity": "critical", "code": "NO_PATCHES", "message": "patches must be an array" }])
        );
        // empty array → warning
        assert_eq!(
            checkPatches(&json!({ "files_touched": [] }), &json!([])),
            json!([{ "severity": "warning", "code": "EMPTY_PATCHES", "message": "microtask produced zero patches" }])
        );
        // shape (non-object/non-array) then array element (passes shape, fails file)
        let r = checkPatches(&json!({ "files_touched": ["a.js"] }), &json!([123, []]));
        assert_eq!(r[0]["code"], json!("PATCH_SHAPE"));
        assert_eq!(r[0]["message"], json!("patch 0 not an object"));
        assert_eq!(r[1]["code"], json!("PATCH_FILE"));
        assert_eq!(r[1]["message"], json!("patch 1 missing file"));
        // scope drift + bad op + empty content on one patch
        let r2 = checkPatches(&json!({ "files_touched": ["a.js"] }), &json!([{ "file": "z.js", "operation": "delete", "content": "" }]));
        assert_eq!(r2[0]["code"], json!("SCOPE_DRIFT"));
        assert_eq!(r2[0]["message"], json!("patch 0 touches 'z.js' which is NOT in microtask.files_touched"));
        assert_eq!(r2[1]["code"], json!("PATCH_OPERATION"));
        assert_eq!(r2[1]["message"], json!("patch 0 operation 'delete' must be one of: create, replace, edit"));
        assert_eq!(r2[2]["code"], json!("PATCH_CONTENT"));
        // happy: in scope, valid op, non-empty content, valid .json parses
        assert_eq!(
            checkPatches(&json!({ "files_touched": ["a.js", "b.json"] }), &json!([
                { "file": "a.js", "operation": "create", "content": "x" },
                { "file": "b.json", "operation": "edit", "content": "{\"k\":1}" }
            ])),
            json!([])
        );
    }

    #[test]
    fn dev_verify_chain_shape() {
        // ok=true, no findings, sample not requested
        let r = devVerifyChain(&json!({ "microtask": { "files_touched": ["a.js"] }, "patches": [{ "file": "a.js", "operation": "create", "content": "x" }] }), &db()).unwrap();
        assert_eq!(r, json!({
            "ok": true, "finding_count": 0, "critical_count": 0, "findings": [],
            "sampled_ai_verify_requested": false, "sampled_ai_verify_recommendation": null
        }));
        // criticals → ok=false; sample requested via truthy non-bool
        let r2 = devVerifyChain(&json!({ "microtask": { "files_touched": ["a.js"] }, "patches": [{ "file": "z.js", "operation": "nope", "content": "" }], "sample_ai_verify": 1 }), &db()).unwrap();
        assert_eq!(r2["ok"], json!(false));
        assert_eq!(r2["critical_count"], json!(3));
        assert_eq!(r2["finding_count"], json!(3));
        assert_eq!(r2["sampled_ai_verify_requested"], json!(true));
        assert_eq!(r2["sampled_ai_verify_recommendation"], json!("caller should dispatch dev-exec-sandbox with critic prompt for this microtask"));
        // warning only (empty patches) → still ok=true; sample explicitly requested
        let r3 = devVerifyChain(&json!({ "microtask": { "files_touched": [] }, "patches": [], "sample_ai_verify": true }), &db()).unwrap();
        assert_eq!(r3["ok"], json!(true));
        assert_eq!(r3["finding_count"], json!(1));
        assert_eq!(r3["critical_count"], json!(0));
        assert_eq!(r3["sampled_ai_verify_requested"], json!(true));
        // missing microtask → Err (guard)
        assert!(devVerifyChain(&json!({ "patches": [] }), &db()).is_err());
    }
}
