//! interviewer-card-proposal-normalize.rs — twin of skills/agents/in/interviewer-card-proposal-normalize.mjs
//! (no-db, pure). NAMES ARE CONTRACT: interviewerCardProposalNormalize (default) · normalizeCardAddArgs.
// QUALITY: 82/100 — rewrite loose AI card.add into a valid slug + canonical kind/size + schema-valid body
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
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
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

fn is_canonical_kind(k: &str) -> bool { matches!(k, "epic" | "feat" | "fix" | "prevent" | "docs" | "decision" | "gap" | "phase") }
fn kind_map(k: &str) -> Option<&'static str> {
    Some(match k {
        "feature" | "feat" | "enhancement" | "task" | "chore" | "story" => "feat",
        "bug" | "bugfix" | "fix" | "hotfix" => "fix",
        "epic" => "epic",
        "doc" | "docs" | "documentation" => "docs",
        "decision" | "adr" => "decision",
        "gap" => "gap",
        "prevent" | "prevention" | "guard" => "prevent",
        "phase" => "phase",
        _ => return None,
    })
}
fn kind_prefix(k: &str) -> &'static str {
    match k { "epic" => "EPIC", "feat" => "FEAT", "fix" => "FIX", "prevent" => "PREVENT", "docs" => "DOCS", "decision" => "DECISION", "gap" => "GAP", "phase" => "PHASE", _ => "FEAT" }
}
fn size_from_kind(k: &str) -> &'static str {
    match k { "epic" => "L", "feat" | "phase" => "M", "fix" | "prevent" | "docs" | "decision" | "gap" => "S", _ => "M" }
}

fn coerce_kind(raw: &Value) -> String {
    if let Some(s) = raw.as_str() {
        let lower = s.trim().to_lowercase();
        if is_canonical_kind(&lower) { return lower; }
        if let Some(m) = kind_map(&lower) { return m.to_string(); }
    }
    "feat".to_string()
}
fn coerce_size(raw: &Value, kind: &str) -> String {
    if let Some(s) = raw.as_str() {
        let up = s.trim().to_uppercase();
        if up == "L" || up == "M" || up == "S" { return up; }
    }
    size_from_kind(kind).to_string()
}
fn slugify(title: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"[^A-Z0-9]+").unwrap());
    let up = title.trim().to_uppercase();
    let dashed = re.replace_all(&up, "-");
    dashed.trim_matches('-').chars().take(48).collect()
}
fn looks_like_slug(v: &Value) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"^[A-Z][A-Z0-9-]{2,}$").unwrap());
    v.as_str().map_or(false, |s| re.is_match(s.trim()))
}

pub fn normalizeCardAddArgs(args: &Value) -> Value {
    let a = if args.is_object() { args.clone() } else { json!({}) };
    let raw_body = match a.get("body") { Some(b) if b.is_object() => b.clone(), _ => json!({}) };

    let kind = coerce_kind(nz(raw_body.get("kind")).or_else(|| nz(a.get("kind"))).unwrap_or(&Value::Null));
    let size = coerce_size(nz(raw_body.get("size")).or_else(|| nz(a.get("size"))).unwrap_or(&Value::Null), &kind);

    let title = if a.get("title").and_then(|v| v.as_str()).map_or(false, |s| !s.trim().is_empty()) {
        a.get("title").unwrap().as_str().unwrap().trim().to_string()
    } else {
        a.get("name").and_then(|v| v.as_str()).map(|s| s.trim().to_string()).unwrap_or_default()
    };

    let slug = if looks_like_slug(a.get("slug").unwrap_or(&Value::Null)) { a.get("slug").unwrap().as_str().unwrap().trim().to_string() }
        else if looks_like_slug(raw_body.get("slug").unwrap_or(&Value::Null)) { raw_body.get("slug").unwrap().as_str().unwrap().trim().to_string() }
        else {
            let stem = { let s = slugify(&title); if s.is_empty() { "CARD".to_string() } else { s } };
            let prefix = kind_prefix(&kind);
            if stem.starts_with(&format!("{}-", prefix)) { stem } else { format!("{}-{}", prefix, stem) }
        };

    let motivation = [raw_body.get("motivation"), raw_body.get("why"), a.get("motivation"), a.get("why"), a.get("description"), a.get("summary")]
        .into_iter().flatten().find(|v| v.as_str().map_or(false, |s| !s.trim().is_empty()));

    let mut body = Map::new();
    body.insert("kind".into(), json!(kind));
    body.insert("size".into(), json!(size));
    if let Some(m) = motivation { body.insert("motivation".into(), json!(m.as_str().unwrap().trim())); }

    let title_out = if title.is_empty() { slug.clone() } else { title.clone() };
    let status = if truthy(a.get("status")) { a.get("status").cloned().unwrap() } else { json!("backlog") };
    let mut out = Map::new();
    out.insert("slug".into(), json!(slug));
    out.insert("title".into(), json!(title_out));
    out.insert("status".into(), status);
    out.insert("body".into(), Value::Object(body));
    if truthy(a.get("parent_slug")) { out.insert("parent_slug".into(), a.get("parent_slug").cloned().unwrap()); }
    Value::Object(out)
}

fn is_card_add_action(action: Option<&str>) -> bool { matches!(action, Some("board-keeper.card.add") | Some("man.card.add") | Some("git-manager.card.add")) }

pub fn interviewerCardProposalNormalize(input: &Value, _db: &Connection) -> Result<Value, String> {
    let plan = match input.get("plan").and_then(|v| v.as_array()) { Some(a) => a, None => return Ok(json!({ "plan": [] })) };
    let normalized: Vec<Value> = plan.iter().map(|p| {
        if !truthy(Some(p)) || !is_card_add_action(p.get("action").and_then(|v| v.as_str())) { return p.clone(); }
        let mut m = p.as_object().cloned().unwrap_or_default();
        m.insert("args".into(), normalizeCardAddArgs(p.get("args").unwrap_or(&Value::Null)));
        Value::Object(m)
    }).collect();
    Ok(json!({ "plan": normalized }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalizes() {
        // loose { title, kind:'feature' } → valid slug + canonical kind/size + body
        let r = normalizeCardAddArgs(&json!({ "title": "Add a strict filter", "kind": "feature" }));
        assert_eq!(r["slug"], json!("FEAT-ADD-A-STRICT-FILTER"));
        assert_eq!(r["body"]["kind"], json!("feat"));
        assert_eq!(r["body"]["size"], json!("M"));
        assert_eq!(r["status"], json!("backlog"));
        // bug → fix/S, explicit slug honoured, motivation pulled from description
        let b = normalizeCardAddArgs(&json!({ "slug": "BUG-99", "kind": "bug", "description": "  it crashes  ", "name": "fallback" }));
        assert_eq!(b["slug"], json!("BUG-99"));
        assert_eq!(b["body"]["kind"], json!("fix"));
        assert_eq!(b["body"]["size"], json!("S"));
        assert_eq!(b["body"]["motivation"], json!("it crashes"));
        assert_eq!(b["title"], json!("fallback"));
        // plan mapping: only card.add entries normalized, others pass through
        let p = interviewerCardProposalNormalize(&json!({ "plan": [
            { "action": "board-keeper.card.add", "args": { "title": "X", "kind": "epic" } },
            { "action": "system.codebase.find", "args": { "query": "y" } },
        ] }), &Connection::open_in_memory().unwrap()).unwrap();
        assert_eq!(p["plan"][0]["args"]["slug"], json!("EPIC-X"));
        assert_eq!(p["plan"][1]["args"]["query"], json!("y")); // untouched
    }
}
