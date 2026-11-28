//! interviewer-confirm-gate.rs — twin of skills/agents/in/interviewer-confirm-gate.mjs (no-db, pure).
//! NAMES ARE CONTRACT: `interviewerConfirmGate` (default). Formats the plan for typed confirmation.
// QUALITY: 78/100 — risk-tier gate: low bypass · medium/high 'go' · autonomous & critical hard-gates
use rusqlite::Connection;
use serde_json::{json, Map, Value};

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
// JS `${JSON.stringify(v)}`: absent → "undefined" (JSON.stringify(undefined) is undefined), else compact JSON.
fn stringify(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(x) => serde_json::to_string(x).unwrap_or_else(|_| "null".into()) } }
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn plan_lines(plan: &[Value]) -> String { plan.iter().map(|p| format!("  \u{2022} {}", tmpl(p.get("action")))).collect::<Vec<_>>().join("\n") }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

pub fn interviewerConfirmGate(input: &Value, _db: &Connection) -> Result<Value, String> {
    let risk_tier = input.get("risk_tier").and_then(|v| v.as_str());
    let plan_v = input.get("plan").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let classification = input.get("classification");

    if !truthy(input.get("requires_confirmation")) || risk_tier == Some("low") {
        let rt = input.get("risk_tier").filter(|v| !v.is_null()).cloned().unwrap_or(json!("low"));
        return Ok(json!({ "bypass": true, "risk_tier": rt }));
    }

    if risk_tier == Some("medium") {
        return Ok(json!({ "bypass": false, "hard_gate": false, "risk_tier": risk_tier,
            "prompt_md": format!("Type 'go' to confirm:\n{}", plan_lines(&plan_v)), "expected_confirmation": "go" }));
    }

    if risk_tier == Some("high") {
        if classification.and_then(|c| c.get("kind")).and_then(|k| k.as_str()) == Some("autonomous") {
            return Ok(json!({ "bypass": false, "hard_gate": true, "risk_tier": risk_tier,
                "prompt_md": format!("[AUTONOMOUS LOOP] Type 'start the driver' to confirm hand-off:\n{}\n\nThis surrenders the next N ticks to the substrate's driver. You can halt via the driver-halt file.", plan_lines(&plan_v)),
                "expected_confirmation": "start the driver" }));
        }
        return Ok(json!({ "bypass": false, "hard_gate": false, "risk_tier": risk_tier,
            "prompt_md": format!("[HIGH RISK] Type 'go' to confirm:\n{}\n\nThis touches multiple things or has wider blast radius. Review carefully.", plan_lines(&plan_v)),
            "expected_confirmation": "go" }));
    }

    // critical — destructive: hard gate via literal slug
    let p0 = plan_v.first();
    let p0args = p0.and_then(|p| p.get("args"));
    let target = nz(p0args.and_then(|a| a.get("slug")))
        .or_else(|| nz(p0args.and_then(|a| a.get("card_slug"))))
        .or_else(|| nz(p0args.and_then(|a| a.get("contract_slug"))))
        .or_else(|| nz(classification.and_then(|c| c.get("target"))))
        .or_else(|| nz(p0.and_then(|p| p.get("action"))))
        .cloned();
    let lines = plan_v.iter().map(|p| format!("  \u{2022} {} (args={})", tmpl(p.get("action")), stringify(p.get("args")))).collect::<Vec<_>>().join("\n");
    let prompt = format!("[CRITICAL \u{2014} DESTRUCTIVE] Type the literal slug to confirm DESTRUCTION of '{}':\n{}\n\nTyping 'go' alone is REJECTED. Type the exact slug shown above.", tmpl(target.as_ref()), lines);
    let mut out = Map::new();
    out.insert("bypass".into(), json!(false));
    out.insert("hard_gate".into(), json!(true));
    out.insert("risk_tier".into(), json!("critical"));
    out.insert("prompt_md".into(), json!(prompt));
    if let Some(t) = target { out.insert("expected_confirmation".into(), t); } // undefined target → key omitted (JS drops undefined)
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Value { interviewerConfirmGate(&i, &Connection::open_in_memory().unwrap()).unwrap() }
    #[test]
    fn gates() {
        assert_eq!(call(json!({ "requires_confirmation": false, "risk_tier": "high" })), json!({ "bypass": true, "risk_tier": "high" }));
        assert_eq!(call(json!({ "requires_confirmation": true, "risk_tier": "low" })), json!({ "bypass": true, "risk_tier": "low" }));
        let m = call(json!({ "requires_confirmation": true, "risk_tier": "medium", "plan": [{ "action": "card.add" }] }));
        assert_eq!(m["expected_confirmation"], json!("go"));
        assert!(m["prompt_md"].as_str().unwrap().contains("  \u{2022} card.add"));
        let a = call(json!({ "requires_confirmation": true, "risk_tier": "high", "classification": { "kind": "autonomous" }, "plan": [{ "action": "driver.start" }] }));
        assert_eq!(a["expected_confirmation"], json!("start the driver"));
        assert_eq!(a["hard_gate"], json!(true));
        let c = call(json!({ "requires_confirmation": true, "risk_tier": "critical", "plan": [{ "action": "card.delete", "args": { "slug": "DOOMED" } }] }));
        assert_eq!(c["expected_confirmation"], json!("DOOMED"));
        assert!(c["prompt_md"].as_str().unwrap().contains("DESTRUCTION of 'DOOMED'"));
        assert!(c["prompt_md"].as_str().unwrap().contains("(args={\"slug\":\"DOOMED\"})"));
    }
}
