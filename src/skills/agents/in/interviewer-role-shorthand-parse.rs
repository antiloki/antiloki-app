//! interviewer-role-shorthand-parse.rs — twin of skills/agents/in/interviewer-role-shorthand-parse.mjs
//! (no-db, pure). NAMES ARE CONTRACT: interviewerRoleShorthandParse (default) · parseRoleShorthand · deriveSlugFromTitle.
// QUALITY: 80/100 — "pl draft X" / "@tl sign S" → canonical { action, input }; whitespace-tolerant
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool {
    match v { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()), Some(Value::String(s)) => !s.is_empty(), Some(_) => true }
}
fn str_qq(v: Option<&Value>) -> String {
    match v { None | Some(Value::Null) => String::new(), Some(Value::String(s)) => s.clone(), Some(Value::Bool(b)) => b.to_string(), Some(n @ Value::Number(_)) => n.to_string(), Some(o) => o.to_string() }
}

pub fn deriveSlugFromTitle(title: &Value) -> Value {
    if !truthy(Some(title)) { return Value::Null; }
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"[^A-Z0-9 -]").unwrap());
    let up = str_qq(Some(title)).to_uppercase();
    let cleaned = re.replace_all(&up, "");
    let parts: Vec<&str> = cleaned.trim().split_whitespace().take(6).collect();
    json!(parts.join("-"))
}

// MAPPING[role][verb] → (action, build(rest)); None if the (role, verb) pair isn't a shorthand.
fn dispatch(r: &str, v: &str, rest: &str) -> Option<(&'static str, Value)> {
    let first = || rest.split_whitespace().next().unwrap_or("").to_string();
    Some(match (r, v) {
        ("pl", "draft") => ("pl.session.start", json!({ "raw_intent": rest })),
        ("tl", "sign") => ("contract.gate.tl.sign", json!({ "slug": first() })),
        ("dev", "impl") => ("git-manager.implement.code", json!({ "card_slug": first() })),
        ("qa", "run") => ("qa.gate.contract.verify", json!({ "slug": first() })),
        ("board", "list") => ("board-keeper.card.list", json!({})),
        ("board", "add") => ("board-keeper.card.add", json!({ "title": rest, "slug": deriveSlugFromTitle(&json!(rest)) })),
        ("board", "score") => ("board-keeper.card.score", json!({ "slug": first() })),
        ("board", "digest") => ("board-keeper.card.digest", json!({})),
        ("keeper", "review") => ("keeper.review", json!({})),
        ("system", "tick") => ("system.driver.tick", json!({})),
        ("system", "run") => ("system.driver.run", json!({})),
        _ => return None,
    })
}

pub fn parseRoleShorthand(text: &Value) -> Value {
    let s = match text.as_str() { Some(x) if !x.is_empty() => x, _ => return json!({ "matched": false }) };
    let t = s.trim();
    let trimmed = t.strip_prefix('@').unwrap_or(t);
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"(?i)^([a-z][a-z0-9_-]*)\s+([a-z][a-z0-9_-]*)\s*(.*)$").unwrap());
    let c = match re.captures(trimmed) { Some(c) => c, None => return json!({ "matched": false }) };
    let r = c[1].to_lowercase();
    let v = c[2].to_lowercase();
    let rest_trimmed = c[3].trim().to_string();
    match dispatch(&r, &v, &rest_trimmed) {
        None => json!({ "matched": false }),
        Some((action, input)) => {
            let mut o = Map::new();
            o.insert("matched".into(), json!(true));
            o.insert("role".into(), json!(r));
            o.insert("verb".into(), json!(v));
            o.insert("action".into(), json!(action));
            o.insert("input".into(), input);
            o.insert("rest".into(), json!(rest_trimmed));
            Value::Object(o)
        }
    }
}

pub fn interviewerRoleShorthandParse(input: &Value, _db: &Connection) -> Result<Value, String> {
    let text = input.get("text");
    if !truthy(text) { return Err("interviewer-role-shorthand-parse: 'text' is required".into()); }
    Ok(parseRoleShorthand(text.unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses() {
        assert_eq!(deriveSlugFromTitle(&json!("Fix the Login Bug!")), json!("FIX-THE-LOGIN-BUG"));
        assert_eq!(deriveSlugFromTitle(&json!("")), Value::Null);
        let d = parseRoleShorthand(&json!("pl draft build a thing"));
        assert_eq!(d, json!({ "matched": true, "role": "pl", "verb": "draft", "action": "pl.session.start", "input": { "raw_intent": "build a thing" }, "rest": "build a thing" }));
        assert_eq!(parseRoleShorthand(&json!("@tl sign C-1 extra"))["input"], json!({ "slug": "C-1" }));
        assert_eq!(parseRoleShorthand(&json!("board add My New Card"))["input"], json!({ "title": "My New Card", "slug": "MY-NEW-CARD" }));
        assert_eq!(parseRoleShorthand(&json!("nope")), json!({ "matched": false }));
        assert_eq!(parseRoleShorthand(&json!("pl unknownverb x")), json!({ "matched": false }));
        assert_eq!(interviewerRoleShorthandParse(&json!({ "text": "board list" }), &Connection::open_in_memory().unwrap()).unwrap()["action"], json!("board-keeper.card.list"));
    }
}
