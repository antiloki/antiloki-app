//! law-hook-no-destructive-git.rs — twin of skills/audit/law-hook-no-destructive-git.mjs (no-db, pure).
//! NAMES ARE CONTRACT: `lawHookNoDestructiveGit` (default). Enforces HISTORY_IS_SACRED_NO_REVERT:
//! refuses actions whose name carries a destructive-git token or whose input string values match a
//! destructive-git CLI pattern. input.input_data.human_authorized_revert===true is the escape hatch.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

const SUSPECT_NAME_TOKENS: [&str; 7] = [
    "revert",
    "reset-hard",
    "force-push",
    "rewrite-history",
    "rebase-onto",
    "filter-branch",
    "git-clean-force",
];

// (compiled regex, JS `.source` string). The source is echoed verbatim into the reason, so it must
// match the JS RegExp literal body byte-for-byte; the `(?i)` prefix mirrors the JS `/i` flag.
fn destructive_git_patterns() -> &'static Vec<(Regex, &'static str)> {
    static P: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    P.get_or_init(|| {
        let raw = [
            r"\bgit\s+revert\b",
            r"\bgit\s+reset\s+--hard\b",
            r"\bgit\s+push\s+--force\b",
            r"\bgit\s+push\s+-f\b",
            r"\bgit\s+filter-branch\b",
            r"\bgit\s+filter-repo\b",
            r"\bgit\s+clean\s+-(f|fd|fdx|fdfx)\b",
            r"\bgit\s+branch\s+-D\b",
            r"\bgit\s+rebase\s+--abort\b",
        ];
        raw.iter()
            .map(|s| (Regex::new(&format!("(?i){}", s)).unwrap(), *s))
            .collect()
    })
}

// Returns (matched pattern source, json-path) of the first destructive hit, in JS traversal order.
fn scan_value(value: &Value, path: &str) -> Option<(String, String)> {
    match value {
        Value::String(s) => {
            for (re, src) in destructive_git_patterns() {
                if re.is_match(s) {
                    return Some(((*src).to_string(), path.to_string()));
                }
            }
            None
        }
        Value::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                if let Some(hit) = scan_value(item, &format!("{}[{}]", path, i)) {
                    return Some(hit);
                }
            }
            None
        }
        Value::Object(map) => {
            for (k, v) in map.iter() {
                if let Some(hit) = scan_value(v, &format!("{}.{}", path, k)) {
                    return Some(hit);
                }
            }
            None
        }
        _ => None,
    }
}

pub fn lawHookNoDestructiveGit(input: &Value, _db: &Connection) -> Result<Value, String> {
    let action_name = input.get("action_name");
    let input_data = input.get("input_data");

    if !truthy(action_name) {
        return Ok(json!({ "allowed": true }));
    }

    if truthy(input_data)
        && input_data.and_then(|d| d.get("human_authorized_revert")) == Some(&Value::Bool(true))
    {
        return Ok(json!({ "allowed": true, "escape_hatch": "human_authorized_revert=true" }));
    }

    let lowered = action_name
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    for tok in SUSPECT_NAME_TOKENS.iter() {
        if lowered.contains(tok) {
            return Ok(json!({
                "allowed": false,
                "reason": format!("action name contains destructive-git token '{}'", tok),
                "suggested_path": "fix forward via a NEW commit; if revert truly intended, the human must request it explicitly",
            }));
        }
    }

    if let Some(target) = input_data.filter(|d| d.is_object() || d.is_array()) {
        if let Some((m, at)) = scan_value(target, "$") {
            return Ok(json!({
                "allowed": false,
                "reason": format!("input contains destructive-git pattern ({}) at {}", m, at),
                "suggested_path": "remove the destructive command; create a NEW commit instead",
            }));
        }
    }

    Ok(json!({ "allowed": true }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Value {
        lawHookNoDestructiveGit(&i, &Connection::open_in_memory().unwrap()).unwrap()
    }
    #[test]
    fn gates() {
        // missing / empty action_name → allowed
        assert_eq!(call(json!({})), json!({ "allowed": true }));
        assert_eq!(call(json!({ "action_name": "" })), json!({ "allowed": true }));
        // escape hatch wins even over a suspect name
        assert_eq!(
            call(json!({ "action_name": "git.revert", "input_data": { "human_authorized_revert": true } })),
            json!({ "allowed": true, "escape_hatch": "human_authorized_revert=true" })
        );
        // name token
        let c = call(json!({ "action_name": "git.revert" }));
        assert_eq!(c["allowed"], json!(false));
        assert_eq!(c["reason"], json!("action name contains destructive-git token 'revert'"));
        assert_eq!(call(json!({ "action_name": "repo.filter-branch" }))["reason"],
            json!("action name contains destructive-git token 'filter-branch'"));
        // input scan — nested object path
        let h = call(json!({ "action_name": "shell.run", "input_data": { "cmd": "git reset --hard HEAD~3" } }));
        assert_eq!(h["allowed"], json!(false));
        assert_eq!(h["reason"], json!(r"input contains destructive-git pattern (\bgit\s+reset\s+--hard\b) at $.cmd"));
        // input scan — array index path
        let a = call(json!({ "action_name": "shell.run", "input_data": { "steps": ["ls", "git push --force origin main"] } }));
        assert_eq!(a["reason"], json!(r"input contains destructive-git pattern (\bgit\s+push\s+--force\b) at $.steps[1]"));
        // input_data is a string (typeof guard) → not scanned → allowed
        assert_eq!(call(json!({ "action_name": "shell.run", "input_data": "git reset --hard x" })), json!({ "allowed": true }));
        // human_authorized_revert=false does not bypass, still scans
        let f = call(json!({ "action_name": "shell.run", "input_data": { "human_authorized_revert": false, "cmd": "git filter-repo --path a" } }));
        assert_eq!(f["allowed"], json!(false));
        assert_eq!(f["reason"], json!(r"input contains destructive-git pattern (\bgit\s+filter-repo\b) at $.cmd"));
        // clean everything → allowed
        assert_eq!(call(json!({ "action_name": "shell.run", "input_data": { "cmd": "git status" } })), json!({ "allowed": true }));
    }
}
