//! contract-sanity-check.rs — twin of skills/contract/contract-sanity-check.mjs (no-db, pure). NAMES ARE
//! CONTRACT: contractSanityCheck (default) · pathViolation · acceptanceInfeasibility. Structural contract lint.
// QUALITY: 83/100 — files_touched/microtask/acceptance-kind checks + long-running-server feasibility lint
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

const KNOWN_ACCEPTANCE_KINDS: [&str; 14] = ["sql", "test", "cli", "file-exists", "file_exists", "grep-contains", "grep", "behavior", "behavioral", "diff_in_paths", "no_law_violation", "skill_returns", "command", "scope_check"];

fn truthy(v: Option<&Value>) -> bool {
    match v { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()), Some(Value::String(s)) => !s.is_empty(), Some(_) => true }
}
fn js_string(v: &Value) -> String { match v { Value::Null => "null".into(), Value::String(s) => s.clone(), Value::Number(n) => n.to_string(), Value::Bool(b) => b.to_string(), o => o.to_string() } }
fn js_typeof(v: Option<&Value>) -> &'static str {
    match v { None => "undefined", Some(Value::Null) => "object", Some(Value::Bool(_)) => "boolean", Some(Value::Number(_)) => "number", Some(Value::String(_)) => "string", _ => "object" }
}
fn take_n(s: &str, n: usize) -> String { s.chars().take(n).collect() }
fn safe_parse_array(s: &str) -> Option<Value> { serde_json::from_str::<Value>(s).ok().filter(|v| v.is_array()) }

fn path_violation_str(p: &Value) -> Option<String> {
    static PH: OnceLock<Regex> = OnceLock::new();
    static GL: OnceLock<Regex> = OnceLock::new();
    static SH: OnceLock<Regex> = OnceLock::new();
    let ph = PH.get_or_init(|| Regex::new(r"^<[A-Z_]+>$").unwrap());
    let gl = GL.get_or_init(|| Regex::new(r"[*?\[\]]").unwrap());
    let sh = SH.get_or_init(|| Regex::new(r"[`$;&|<>]").unwrap());
    let s = match p.as_str() { Some(x) => x, None => return Some("not a string".into()) };
    if s.is_empty() { return Some("empty".into()); }
    if s.trim() != s { return Some("surrounding whitespace".into()); }
    if ph.is_match(s) { return Some("placeholder pattern (<X>)".into()); }
    if gl.is_match(s) { return Some("glob metachars".into()); }
    if sh.is_match(s) { return Some("shell metachars".into()); }
    None
}
pub fn pathViolation(p: &Value) -> Value { match path_violation_str(p) { Some(r) => json!(r), None => Value::Null } }

// SERVER_CMD_RE has a negative lookahead (vite NOT followed by build) — the regex crate lacks lookaround,
// so the main alternatives run as one regex and `vite(?! build)` is checked by hand.
fn server_cmd(s: &str) -> bool {
    static MAIN: OnceLock<Regex> = OnceLock::new();
    static VITE: OnceLock<Regex> = OnceLock::new();
    static BUILD: OnceLock<Regex> = OnceLock::new();
    let main = MAIN.get_or_init(|| Regex::new(r"\bnpm\s+(?:run\s+)?(?:dev|start|serve|watch)\b|\byarn\s+(?:run\s+)?(?:dev|start|serve)\b|\bpnpm\s+(?:run\s+)?(?:dev|start|serve)\b|\bnodemon\b|--watch\b").unwrap());
    if main.is_match(s) { return true; }
    let vite = VITE.get_or_init(|| Regex::new(r"\bvite\b").unwrap());
    let build = BUILD.get_or_init(|| Regex::new(r"^\s+build").unwrap());
    vite.find_iter(s).any(|m| !build.is_match(&s[m.end()..]))
}
pub fn acceptanceInfeasibility(text: &Value) -> Value {
    let s = match text.as_str() { Some(x) if !x.trim().is_empty() => x, _ => return Value::Null };
    static Q: OnceLock<Regex> = OnceLock::new();
    static PROBE: OnceLock<Regex> = OnceLock::new();
    let q = Q.get_or_init(|| Regex::new(r#"'[^']*'|"[^"]*""#).unwrap());
    let probe = PROBE.get_or_init(|| Regex::new(r"(?i)\bcurl\b|\bwget\b|Invoke-WebRequest|\bfetch\(").unwrap());
    let unquoted = q.replace_all(s, "''");
    if !server_cmd(&unquoted) { return Value::Null; }
    if probe.is_match(s) { return Value::Null; }
    json!(format!("long-running server/watcher command in an exit-code acceptance ('{}') — a dev server never exits 0 on its own (timeout-kill returns 124), so QA can never pass this. Use background-start + curl probe + kill, or assert on build artifacts (e.g. 'npm run build' + file-exists) instead.", take_n(s, 100)))
}

pub fn contractSanityCheck(contract: &Value, _db: &Connection) -> Result<Value, String> {
    let mut violations: Vec<Value> = Vec::new();

    // files_touched
    let mut ft: Option<Value> = contract.get("files_touched").cloned();
    if !ft.as_ref().map_or(false, |v| v.is_array()) {
        if let Some(js) = contract.get("files_touched_json").and_then(|v| v.as_str()) { ft = safe_parse_array(js); }
    }
    if !ft.as_ref().map_or(false, |v| v.is_array()) {
        violations.push(json!({ "kind": "files_touched_not_array", "where": "contract.files_touched", "detail": format!("expected array, got {}", js_typeof(ft.as_ref())) }));
    } else if ft.as_ref().unwrap().as_array().unwrap().is_empty() {
        violations.push(json!({ "kind": "files_touched_empty", "where": "contract.files_touched", "detail": "must contain at least one path" }));
    } else {
        let arr = ft.as_ref().unwrap().as_array().unwrap().clone();
        let mut seen: HashSet<String> = HashSet::new();
        for (i, p) in arr.iter().enumerate() {
            if let Some(reason) = path_violation_str(p) {
                violations.push(json!({ "kind": "path_invalid", "where": format!("contract.files_touched[{}]", i), "detail": format!("{}: '{}'", reason, take_n(&js_string(p), 60)) }));
            } else if seen.contains(&js_string(p)) {
                violations.push(json!({ "kind": "path_duplicate", "where": format!("contract.files_touched[{}]", i), "detail": format!("duplicate path: '{}'", js_string(p)) }));
            } else { seen.insert(js_string(p)); }
        }
    }

    // microtasks
    let mut microtasks: Option<Value> = contract.get("microtasks").cloned();
    if !microtasks.as_ref().map_or(false, |v| v.is_array()) {
        if let Some(js) = contract.get("microtasks_json").and_then(|v| v.as_str()) { microtasks = Some(safe_parse_array(js).unwrap_or(json!([]))); }
    }
    if let Some(mts) = microtasks.as_ref().and_then(|v| v.as_array()) {
        for (i, mt) in mts.iter().enumerate() {
            if !truthy(Some(mt)) || !(mt.is_object() || mt.is_array()) {
                violations.push(json!({ "kind": "microtask_not_object", "where": format!("microtasks[{}]", i), "detail": format!("expected object, got {}", js_typeof(Some(mt))) }));
                continue;
            }
            let mt_ft = mt.get("files_touched");
            if let Some(s) = mt_ft.and_then(|v| v.as_str()) {
                if safe_parse_array(s).is_some() {
                    violations.push(json!({ "kind": "microtask_files_touched_stringified", "where": format!("microtasks[{}].files_touched", i), "detail": format!("value is a stringified JSON array (got: {}); AI should return an actual array. Class: parent_output_id leak.", take_n(s, 80)) }));
                } else {
                    violations.push(json!({ "kind": "microtask_files_touched_not_array", "where": format!("microtasks[{}].files_touched", i), "detail": format!("expected array, got string '{}'", take_n(s, 60)) }));
                }
            } else if !mt_ft.map_or(false, |v| v.is_array()) {
                violations.push(json!({ "kind": "microtask_files_touched_not_array", "where": format!("microtasks[{}].files_touched", i), "detail": format!("expected array, got {}", js_typeof(mt_ft)) }));
            } else {
                for (j, p) in mt_ft.unwrap().as_array().unwrap().iter().enumerate() {
                    if let Some(reason) = path_violation_str(p) {
                        violations.push(json!({ "kind": "microtask_path_invalid", "where": format!("microtasks[{}].files_touched[{}]", i, j), "detail": format!("{}: '{}'", reason, take_n(&js_string(p), 60)) }));
                    }
                }
            }
        }
    }

    // acceptance kinds
    let acc_raw = contract.get("acceptance_json").filter(|v| !v.is_null()).or_else(|| contract.get("acceptance").filter(|v| !v.is_null()));
    let acceptance: Option<Value> = match acc_raw.and_then(|v| v.as_str()) { Some(s) => Some(safe_parse_array(s).unwrap_or(json!([]))), None => acc_raw.cloned() };
    if let Some(accs) = acceptance.as_ref().and_then(|v| v.as_array()) {
        for (i, ac) in accs.iter().enumerate() {
            if let Some(s) = ac.as_str() {
                let inf = acceptanceInfeasibility(&json!(s));
                if !inf.is_null() { violations.push(json!({ "kind": "acceptance_infeasible_long_running", "where": format!("acceptance[{}]", i), "detail": inf })); }
                continue;
            }
            if !truthy(Some(ac)) || !(ac.is_object() || ac.is_array()) {
                violations.push(json!({ "kind": "acceptance_not_object_or_string", "where": format!("acceptance[{}]", i), "detail": format!("expected object or string, got {}", js_typeof(Some(ac))) }));
                continue;
            }
            if ac.get("kind").map_or(false, |k| !k.is_null()) && !KNOWN_ACCEPTANCE_KINDS.contains(&ac.get("kind").and_then(|v| v.as_str()).unwrap_or("\0")) {
                violations.push(json!({ "kind": "acceptance_kind_unknown", "where": format!("acceptance[{}].kind", i), "detail": format!("unknown kind '{}' (allowed: {})", js_string(ac.get("kind").unwrap()), KNOWN_ACCEPTANCE_KINDS.join(", ")) }));
            }
            let cmd_text = ["command", "cmd", "run", "assert", "check"].iter().filter_map(|f| ac.get(*f).and_then(|v| v.as_str())).collect::<Vec<_>>().join(" ");
            let inf = acceptanceInfeasibility(&json!(cmd_text));
            if !inf.is_null() { violations.push(json!({ "kind": "acceptance_infeasible_long_running", "where": format!("acceptance[{}]", i), "detail": inf })); }
        }
    }

    Ok(json!({ "ok": violations.is_empty(), "violations": violations }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(c: Value) -> Value { contractSanityCheck(&c, &Connection::open_in_memory().unwrap()).unwrap() }
    #[test]
    fn checks() {
        assert_eq!(pathViolation(&json!("src/ok.ts")), Value::Null);
        assert_eq!(pathViolation(&json!("<UNKNOWN>")), json!("placeholder pattern (<X>)"));
        assert_eq!(pathViolation(&json!("a/*.ts")), json!("glob metachars"));
        assert!(!acceptanceInfeasibility(&json!("npm run dev")).is_null());
        assert!(acceptanceInfeasibility(&json!("npm run dev with curl probe")).is_null()); // probe exempt
        assert!(acceptanceInfeasibility(&json!("vite build")).is_null()); // build is fine
        assert_eq!(call(json!({ "files_touched": ["a.ts", "b.ts"] }))["ok"], json!(true));
        let bad = call(json!({ "files_touched": ["a.ts", "a.ts", "<X>"], "acceptance": [{ "kind": "bogus_kind" }] }));
        assert_eq!(bad["ok"], json!(false));
        let kinds: Vec<&str> = bad["violations"].as_array().unwrap().iter().map(|v| v["kind"].as_str().unwrap()).collect();
        assert!(kinds.contains(&"path_duplicate") && kinds.contains(&"path_invalid") && kinds.contains(&"acceptance_kind_unknown"));
        assert_eq!(call(json!({ "files_touched": [] }))["violations"][0]["kind"], json!("files_touched_empty"));
    }
}
