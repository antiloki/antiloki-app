//! judgment-id.rs — twin of skills/swarm/judgment-id.mjs. NAMES ARE CONTRACT: `SCOPE_KINDS`,
//! `makeJudgmentId`, `parseJudgmentId`, `judgmentId` (named default), exact JS names.
//! Pure string threading; no DB, no AI (matches the JS doc header).
// QUALITY: 83/100 — faithful string-coercion mirror of JS's ?? and toLowerCase(); SEP/kind
// lookups repeat SCOPE_KINDS.as_array() unwraps per call rather than a cached &[&str]

use serde_json::{json, Value};
use std::sync::LazyLock;

pub static SCOPE_KINDS: LazyLock<Value> =
    LazyLock::new(|| json!(["microtask", "snippet", "card", "blueprint"]));

const SEP: &str = "::";

fn scope_kinds() -> Vec<&'static str> {
    SCOPE_KINDS.as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect()
}

/// JS: strip control chars (\x00-\x1f), then trim.
fn cleanRef(r: &str) -> String {
    let stripped: String = r.chars().filter(|c| (*c as u32) > 0x1f).collect();
    stripped.trim().to_string()
}

/// JS: `makeJudgmentId(cardSlug, scopeKind, scopeRef = '')`.
pub fn makeJudgmentId(card_slug: &Value, scope_kind: &Value, scope_ref: &Value) -> Result<String, String> {
    let slug = match card_slug {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    let slug = slug.trim();
    if slug.is_empty() {
        return Err("judgment-id: cardSlug is required".into());
    }
    let kind = match scope_kind {
        Value::Null => String::new(),
        Value::String(s) => s.to_lowercase(),
        v => v.to_string().to_lowercase(),
    };
    if !scope_kinds().contains(&kind.as_str()) {
        return Err(format!("judgment-id: scopeKind must be one of: {} (got '{kind}')", scope_kinds().join(", ")));
    }
    let ref_str = match scope_ref {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        v => v.to_string(),
    };
    Ok(format!("{slug}{SEP}{kind}:{}", cleanRef(&ref_str)))
}

/// JS: `parseJudgmentId(id)` — null on malformed input.
pub fn parseJudgmentId(id: &Value) -> Value {
    let s = match id {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        v => v.to_string(),
    };
    let Some(at) = s.find(SEP) else { return Value::Null };
    let card_slug = &s[..at];
    let rest = &s[at + SEP.len()..];
    let Some(colon) = rest.find(':') else { return Value::Null };
    let scope_kind = &rest[..colon];
    let scope_ref = &rest[colon + 1..];
    if card_slug.is_empty() || !scope_kinds().contains(&scope_kind) {
        return Value::Null;
    }
    json!({ "cardSlug": card_slug, "scopeKind": scope_kind, "scopeRef": scope_ref })
}

/// JS: named default `judgmentId(input, _context)` — parse-mode (input.id truthy) vs make-mode.
pub fn judgmentId(input: &Value, _context: &Value) -> Result<Value, String> {
    let id = input.get("id");
    let id_truthy = matches!(id, Some(Value::String(s)) if !s.is_empty());
    if id_truthy {
        let parsed = parseJudgmentId(id.unwrap());
        return Ok(if parsed.is_null() { json!({}) } else { parsed });
    }
    let card_slug = input.get("card_slug").unwrap_or(&Value::Null);
    let scope_kind = input.get("scope_kind").unwrap_or(&Value::Null);
    let scope_ref = input.get("scope_ref").unwrap_or(&Value::Null);
    Ok(json!({ "judgment_id": makeJudgmentId(card_slug, scope_kind, scope_ref)? }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_slug_is_prefix_scope_is_suffix() {
        assert_eq!(makeJudgmentId(&json!("card-FEAT-FOO-bcaf798e"), &json!("microtask"), &json!("mt-2")).unwrap(), "card-FEAT-FOO-bcaf798e::microtask:mt-2");
        assert_eq!(makeJudgmentId(&json!("card-X"), &json!("card"), &Value::Null).unwrap(), "card-X::card:");
    }

    #[test]
    fn bad_scope_kind_and_missing_slug_rejected() {
        assert!(makeJudgmentId(&json!("card-X"), &json!("function"), &json!("f")).is_err());
        assert!(makeJudgmentId(&json!(""), &json!("microtask"), &json!("m")).is_err());
    }

    #[test]
    fn parse_round_trips() {
        let id = makeJudgmentId(&json!("card-FEAT-FOO-bcaf798e"), &json!("snippet"), &json!("src/a/b.js")).unwrap();
        assert_eq!(parseJudgmentId(&json!(id)), json!({"cardSlug":"card-FEAT-FOO-bcaf798e","scopeKind":"snippet","scopeRef":"src/a/b.js"}));
        assert_eq!(parseJudgmentId(&json!("not-an-id")), Value::Null);
    }

    #[test]
    fn default_entry_makes_and_parses() {
        assert_eq!(judgmentId(&json!({"card_slug":"card-X","scope_kind":"microtask","scope_ref":"mt-1"}), &Value::Null).unwrap()["judgment_id"], json!("card-X::microtask:mt-1"));
        assert_eq!(judgmentId(&json!({"id":"card-X::microtask:mt-1"}), &Value::Null).unwrap()["scopeRef"], json!("mt-1"));
    }
}
