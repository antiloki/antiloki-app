//! interviewer-error-resolve.rs — twin of skills/agents/in/interviewer-error-resolve.mjs (no-db, pure).
//! NAMES ARE CONTRACT: `interviewerErrorResolve` (default). Dispatch failure → in-chat resolution forks.
// QUALITY: 80/100 — map an error string into selectable alternatives (git-drift / law-denied / unknown)
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

fn write_myself() -> Value { json!({ "key": "W", "label": "Write it myself", "action_kind": "write_myself" }) }
fn truthy(v: Option<&Value>) -> bool {
    match v { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()), Some(Value::String(s)) => !s.is_empty(), Some(_) => true }
}

pub fn interviewerErrorResolve(input: &Value, _db: &Connection) -> Result<Value, String> {
    // error = typeof input === 'string' ? input : (input?.error ?? '')
    let error = if let Some(s) = input.as_str() { s.to_string() }
        else { input.get("error").and_then(|v| v.as_str()).unwrap_or("").to_string() };
    // action = (typeof input === 'object' && input?.action) || null
    let action: Option<Value> = if input.is_object() { input.get("action").filter(|v| truthy(Some(v))).cloned() } else { None };

    static DRIFT: OnceLock<Regex> = OnceLock::new();
    static PERM: OnceLock<Regex> = OnceLock::new();
    static LAW: OnceLock<Regex> = OnceLock::new();
    let drift = DRIFT.get_or_init(|| Regex::new(r"code drift detected").unwrap());
    let perm = PERM.get_or_init(|| Regex::new(r"(?i)PERMISSION_DENIED|denied").unwrap());
    let law_re = LAW.get_or_init(|| Regex::new(r"law '([A-Z0-9_]+)'").unwrap());

    let finish = |mut o: Map<String, Value>| -> Value {
        if let Some(a) = &action { o.insert("action".into(), a.clone()); }
        Value::Object(o)
    };

    if error.contains("COMMIT_CODE_BEFORE_AUDIT_COMMIT") || drift.is_match(&error) {
        let mut o = Map::new();
        o.insert("kind".into(), json!("git_drift"));
        o.insert("message_md".into(), json!("That action commits to the audit chain, but there are uncommitted code changes in the working tree. The substrate blocks the audit commit until the code lands (so git history never shows progress that isn't really committed). How do you want to resolve it?"));
        o.insert("alternatives".into(), json!([
            { "key": "A", "label": "Commit my pending code, then create it", "action_kind": "commit_pending", "hint": "lists the uncommitted files; you confirm a commit message" },
            { "key": "B", "label": "I'll commit manually first", "action_kind": "guidance", "hint": "commit your code in a terminal, then re-send" },
            write_myself(),
        ]));
        return Ok(finish(o));
    }

    let law = law_re.captures(&error).map(|c| c[1].to_string());
    if law.is_some() || perm.is_match(&error) {
        let mut o = Map::new();
        o.insert("kind".into(), json!("law_denied"));
        o.insert("message_md".into(), json!(match &law {
            Some(l) => format!("That action was refused by the law **{}**. Pick how to proceed:", l),
            None => "That action was refused. Pick how to proceed:".to_string(),
        }));
        o.insert("alternatives".into(), json!([
            { "key": "A", "label": "Rephrase my request", "action_kind": "rephrase", "hint": "restate it so it doesn't trip the rule" },
            { "key": "B", "label": "Retry as-is", "action_kind": "retry", "hint": "try the same action again" },
            write_myself(),
        ]));
        if let Some(l) = &law { o.insert("law".into(), json!(l)); } // law appended before action
        return Ok(finish(o));
    }

    let mut o = Map::new();
    o.insert("kind".into(), json!("unknown"));
    o.insert("message_md".into(), json!("That didn't go through. Pick how to proceed:"));
    o.insert("alternatives".into(), json!([
        { "key": "A", "label": "Retry as-is", "action_kind": "retry" },
        { "key": "B", "label": "Rephrase my request", "action_kind": "rephrase" },
        write_myself(),
    ]));
    Ok(finish(o))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Value { interviewerErrorResolve(&i, &Connection::open_in_memory().unwrap()).unwrap() }
    #[test]
    fn resolves() {
        let g = call(json!({ "error": "law 'COMMIT_CODE_BEFORE_AUDIT_COMMIT' denied: code drift detected (13 files)", "action": "board-keeper.card.add" }));
        assert_eq!(g["kind"], json!("git_drift"));
        assert_eq!(g["alternatives"].as_array().unwrap().len(), 3);
        assert_eq!(g["action"], json!("board-keeper.card.add"));
        assert_eq!(call(json!({ "error": "code drift detected (2 files)" }))["kind"], json!("git_drift"));
        let l = call(json!({ "error": "action-run: law 'LONE_SKILL_DISPATCH_FORBIDDEN' denied" }));
        assert_eq!(l["kind"], json!("law_denied"));
        assert_eq!(l["law"], json!("LONE_SKILL_DISPATCH_FORBIDDEN"));
        assert_eq!(call(json!("something odd happened"))["kind"], json!("unknown")); // string input
        assert_eq!(call(json!({ "error": "totally unknown" }))["kind"], json!("unknown"));
    }
}
