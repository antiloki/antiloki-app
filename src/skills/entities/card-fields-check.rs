//! card-fields-check.rs — twin of skills/entities/card-fields-check.mjs (db lane). NAMES ARE CONTRACT:
//! `cardFieldsCheck` (default) + `evaluateBody` (named, NO-DB helper). Programmatic field-completeness gate.
// QUALITY: 82/100 — alias resolution, per-kind requirements, text/list thresholds; pure helper via sig lane
use rusqlite::Connection;
use serde_json::{json, Value};

const MIN_TEXT_CHARS: usize = 40;
const MIN_LIST_CHARS: usize = 24;

fn aliases(field: &str) -> Vec<&'static str> {
    match field {
        "motivation" => vec!["motivation", "why", "problem", "summary"],
        "acceptance_criteria" => vec!["acceptance_criteria", "outcome", "acceptance"],
        "context" => vec!["context"],
        "reproduction" => vec!["reproduction"],
        "diagnosis" => vec!["diagnosis"],
        "alternatives_considered" => vec!["alternatives_considered"],
        other => vec![Box::leak(other.to_string().into_boxed_str())],
    }
}
fn js_string(v: &Value) -> String {
    match v { Value::String(s) => s.clone(), Value::Null => "null".into(), o => o.to_string() }
}

// first non-empty alias wins (null / empty-array / blank-string are "absent")
fn resolve<'a>(body: &'a Value, field: &str) -> Option<&'a Value> {
    for key in aliases(field) {
        if let Some(v) = body.get(key) {
            let absent = v.is_null()
                || (v.is_array() && v.as_array().unwrap().is_empty())
                || (v.is_string() && v.as_str().unwrap().trim().is_empty());
            if !absent { return Some(v); }
        }
    }
    None
}

fn check_field(body: &Value, field: &str, ftype: &str, severity: &str) -> Option<Value> {
    match resolve(body, field) {
        None => Some(json!({ "field": field, "severity": severity, "problem": "missing" })),
        Some(v) => {
            if ftype == "text" {
                let len = js_string(v).trim().chars().count();
                if len < MIN_TEXT_CHARS {
                    Some(json!({ "field": field, "severity": severity, "problem": format!("too short ({len}/{MIN_TEXT_CHARS} chars)") }))
                } else { None }
            } else {
                let items: Vec<Value> = if v.is_array() { v.as_array().unwrap().clone() } else { vec![v.clone()] };
                let joined = items.iter().map(|x| js_string(x).trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
                let jlen = joined.chars().count();
                if items.is_empty() || jlen < MIN_LIST_CHARS {
                    Some(json!({ "field": field, "severity": severity, "problem": format!("too thin ({}/{} chars across {} item(s))", jlen, MIN_LIST_CHARS, items.len()) }))
                } else { None }
            }
        }
    }
}

// NO-DB helper: evaluate a parsed body object against the per-kind requirements.
pub fn evaluateBody(body: &Value) -> Value {
    let kind = body.get("kind").cloned().unwrap_or(Value::Null);
    let mut specs: Vec<(&str, &str, &str)> = vec![
        ("motivation", "text", "error"),
        ("acceptance_criteria", "list", "error"),
        ("context", "list", "warn"),
    ];
    match kind.as_str() {
        Some("fix") => specs.push(("reproduction", "list", "error")),
        Some("decision") => specs.push(("alternatives_considered", "list", "warn")),
        _ => {}
    }
    let mut issues: Vec<Value> = Vec::new();
    for (field, ftype, severity) in &specs {
        if let Some(issue) = check_field(body, field, ftype, severity) { issues.push(issue); }
    }
    let ok = !issues.iter().any(|i| i.get("severity") == Some(&json!("error")));
    json!({ "ok": ok, "kind": kind, "issues": issues })
}

fn parse_body(bj: Option<&Value>) -> Value {
    let s = match bj.and_then(|v| v.as_str()) { Some(s) if !s.is_empty() => s.to_string(), _ => return json!({}) };
    match serde_json::from_str::<Value>(&s) {
        Ok(Value::String(inner)) => serde_json::from_str::<Value>(&inner).unwrap_or(json!({})), // double-decoded
        Ok(v) => v,
        Err(_) => json!({}),
    }
}

pub fn cardFieldsCheck(input: &Value, db: &Connection) -> Result<Value, String> {
    let provided = input.get("body").filter(|v| !v.is_null() && v.as_bool() != Some(false) && v.as_str() != Some(""));
    if let Some(b) = provided {
        return Ok(evaluateBody(b));
    }
    // db path
    let slug = match input.get("slug").filter(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false)) {
        Some(v) => v.clone(),
        None => return Err("card-fields-check: slug (or body) is required".into()),
    };
    let card = crate::db_json::query_rows(db, "SELECT body_json FROM cards WHERE slug = ?", &[slug.clone()])?;
    let card0 = match card.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("card-fields-check: card '{}' not found", slug.as_str().unwrap())),
    };
    Ok(evaluateBody(&parse_body(card0.get("body_json"))))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evaluates_completeness() {
        let good = json!({ "kind": "feat", "motivation": "Operators keep playing cards that are not ready, wasting driver cycles.", "acceptance_criteria": ["RUN refuses when accuracy < 70", "Blockers shown inline"], "context": ["Driver picks by priority"] });
        assert_eq!(evaluateBody(&good)["ok"], json!(true));
        let mut bad = good.clone(); bad["motivation"] = json!("too short");
        assert_eq!(evaluateBody(&bad)["ok"], json!(false));
        let fix = json!({ "kind": "fix", "motivation": "A clear and sufficiently long description of the bug being fixed.", "acceptance_criteria": ["the crash no longer happens on submit"] });
        assert_eq!(evaluateBody(&fix)["ok"], json!(false)); // fix needs reproduction
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(cardFieldsCheck(&json!({ "body": good }), &c).unwrap()["ok"], json!(true));
    }
}
