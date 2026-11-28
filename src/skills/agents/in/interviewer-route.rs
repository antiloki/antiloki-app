//! interviewer-route.rs — twin of skills/agents/in/interviewer-route.mjs (no-db, pure). NAMES ARE
//! CONTRACT: `interviewerRoute` (default). classification → risk-tier-aware plan + summary.
// QUALITY: 80/100 — per-action tier w/ kind defaults, destructive→critical escalation, max-severity fold
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(), Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(), Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(), Some(o) => o.to_string(),
    }
}
fn kind_default_risk(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "read" | "meta" | "propose" | "clarify" | "curate" | "investigate" => "low",
        "mutate" | "start_work" => "medium",
        "autonomous" => "high",
        "destructive" => "critical",
        _ => return None,
    })
}
fn tier_order(t: &str) -> i32 { match t { "low" => 1, "medium" => 2, "high" => 3, "critical" => 4, _ => 0 } }

struct PItem { name: Option<Value>, args: Value, risk: String }

fn summarize(items: &[PItem], kind_raw: Option<&Value>) -> String {
    let kind = kind_raw.and_then(|v| v.as_str());
    if items.is_empty() {
        return match kind {
            Some("clarify") => "I need a clearer intent — could you rephrase, or be more specific (e.g., name a card slug or specific action)?".into(),
            Some("propose") | Some("meta") => format!("({}) Discussion only — no actions to dispatch. Tell me when you want to do something concrete.", kind.unwrap()),
            _ => format!("I understood the intent as '{}' but didn't recognize a specific action. Could you be more specific?", tmpl(kind_raw)),
        };
    }
    let n = items.len();
    let risk = if kind == Some("destructive") { "critical".to_string() } else { items[0].risk.clone() };
    let mut lines = vec![format!("Proposed plan ({} action{}, risk={}):", n, if n == 1 { "" } else { "s" }, risk)];
    for p in items {
        let suffix = if !p.risk.is_empty() { format!(" [{}]", p.risk) } else { String::new() };
        lines.push(format!("  \u{2022} {}{}", tmpl(p.name.as_ref()), suffix));
    }
    lines.join("\n")
}

pub fn interviewerRoute(input: &Value, _db: &Connection) -> Result<Value, String> {
    let classification = input.get("classification").filter(|v| !v.is_null());
    let classification = match classification {
        None => return Ok(json!({ "plan": [], "summary_md": "no classification supplied", "requires_confirmation": false, "risk_tier": "low" })),
        Some(c) => c,
    };

    let kind_raw = classification.get("kind");
    let kind = kind_raw.and_then(|v| v.as_str());
    let sas = classification.get("suggested_actions").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let mut items: Vec<PItem> = sas.iter().map(|sa| {
        let risk = sa.get("risk_tier").and_then(|v| v.as_str()).map(String::from)
            .or_else(|| kind_default_risk(kind.unwrap_or("")).map(String::from))
            .unwrap_or_else(|| "medium".into());
        PItem { name: sa.get("name").cloned(), args: sa.get("args").filter(|v| !v.is_null()).cloned().unwrap_or(json!({})), risk }
    }).collect();

    if kind == Some("destructive") { for p in &mut items { p.risk = "critical".into(); } }

    let effective: String = if items.is_empty() {
        kind_default_risk(kind.unwrap_or("")).unwrap_or("low").to_string()
    } else {
        let mut acc = "low".to_string();
        for p in &items { if tier_order(&p.risk) > tier_order(&acc) { acc = p.risk.clone(); } }
        acc
    };
    let requires_confirmation = matches!(effective.as_str(), "medium" | "high" | "critical") && !items.is_empty();

    let plan: Vec<Value> = items.iter().map(|p| {
        let mut m = Map::new();
        if let Some(n) = &p.name { m.insert("action".into(), n.clone()); } // sa.name undefined → JS omits the key
        m.insert("args".into(), p.args.clone());
        m.insert("risk_tier".into(), json!(p.risk));
        Value::Object(m)
    }).collect();

    let mut out = Map::new();
    out.insert("plan".into(), json!(plan));
    out.insert("summary_md".into(), json!(summarize(&items, kind_raw)));
    out.insert("risk_tier".into(), json!(effective));
    out.insert("requires_confirmation".into(), json!(requires_confirmation));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Value { interviewerRoute(&i, &Connection::open_in_memory().unwrap()).unwrap() }
    #[test]
    fn routes() {
        assert_eq!(call(json!({})), json!({ "plan": [], "summary_md": "no classification supplied", "requires_confirmation": false, "risk_tier": "low" }));
        let mutate = call(json!({ "classification": { "kind": "mutate", "suggested_actions": [{ "name": "card.add" }] } }));
        assert_eq!(mutate["risk_tier"], json!("medium"));
        assert_eq!(mutate["requires_confirmation"], json!(true));
        assert_eq!(mutate["plan"][0], json!({ "action": "card.add", "args": {}, "risk_tier": "medium" }));
        let destr = call(json!({ "classification": { "kind": "destructive", "suggested_actions": [{ "name": "card.delete", "risk_tier": "low" }] } }));
        assert_eq!(destr["risk_tier"], json!("critical")); // escalated regardless of per-action tier
        assert!(destr["summary_md"].as_str().unwrap().contains("risk=critical"));
        let clarify = call(json!({ "classification": { "kind": "clarify" } }));
        assert_eq!(clarify["requires_confirmation"], json!(false));
        assert!(clarify["summary_md"].as_str().unwrap().starts_with("I need a clearer intent"));
    }
}
