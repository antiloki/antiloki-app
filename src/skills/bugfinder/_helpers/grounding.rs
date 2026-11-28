//! grounding.rs — twin of skills/bugfinder/_helpers/grounding.mjs (helper module, all no-db). NAMES ARE
//! CONTRACT: normalizeSeverity · kebab · parseFindingsJson · locateEvidence · validateLead.
// QUALITY: 83/100 — the deterministic evidence floor: verbatim-quote location re-derives the line, $0 refutation
use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn str_qq(v: Option<&Value>) -> String {
    // String(v ?? '')
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn ws_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\s+").unwrap()) }
fn ws_collapse(s: &str) -> String { ws_re().replace_all(s, " ").trim().to_string() }
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() } // JS .slice(0,n) (BMP-equivalent)

pub fn normalizeSeverity(s: &Value) -> Value {
    let v = str_qq(Some(s)).to_lowercase();
    let v = v.trim();
    json!(if v == "low" || v == "medium" || v == "high" { v } else { "medium" })
}

pub fn kebab(s: &Value) -> Value {
    let lower = str_qq(Some(s)).to_lowercase();
    let lower = lower.trim();
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"[^a-z0-9]+").unwrap());
    let dashed = re.replace_all(lower, "-");
    let trimmed = dashed.trim_matches('-'); // /^-+|-+$/g
    json!(take_chars(trimmed, 40))
}

pub fn parseFindingsJson(text: &Value) -> Value {
    static FENCE: OnceLock<Regex> = OnceLock::new();
    static OBJ: OnceLock<Regex> = OnceLock::new();
    let fence = FENCE.get_or_init(|| Regex::new(r"```[a-z]*\n?|```").unwrap());
    let obj = OBJ.get_or_init(|| Regex::new(r"(?s)\{.*\}").unwrap());
    let raw_text = str_qq(Some(text));
    let t = fence.replace_all(&raw_text, "");
    let t = t.trim();
    let raw: String = if t.starts_with('[') { t.to_string() } else { obj.find(t).map(|m| m.as_str().to_string()).unwrap_or_else(|| "null".to_string()) };
    match serde_json::from_str::<Value>(&raw) {
        Ok(v) => {
            let arr = if v.is_array() { Some(v.clone()) }
                else if v.get("findings").map_or(false, |f| f.is_array()) { Some(v["findings"].clone()) }
                else { None };
            match arr { Some(a) if a.is_array() => a, _ => json!([]) }
        }
        Err(_) => json!([]),
    }
}

// JS Number(v)
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse::<f64>().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}

fn locate_inner(source: &str, evidence: &str, claimed: Option<&Value>) -> Option<i64> {
    let needle = ws_collapse(evidence);
    if needle.chars().count() < 4 { return None; }
    let lines: Vec<&str> = source.split('\n').collect();
    let mut hits: Vec<i64> = Vec::new();
    for (i, ln) in lines.iter().enumerate() {
        if ws_collapse(ln).contains(&needle) { hits.push(i as i64 + 1); }
    }
    if hits.is_empty() {
        let flat = ws_collapse(source);
        if !flat.contains(&needle) { return None; }
        let mut acc = String::new();
        for (i, ln) in lines.iter().enumerate() {
            acc = ws_collapse(&format!("{} {}", acc, ln));
            if acc.contains(&needle) { return Some(i as i64 + 1); }
        }
        return Some(1);
    }
    let claimed = js_number(claimed);
    if claimed.is_finite() && claimed.fract() == 0.0 {
        let c = claimed;
        let mut best = hits[0];
        for &h in &hits { if (h as f64 - c).abs() < (best as f64 - c).abs() { best = h; } }
        return Some(best);
    }
    Some(hits[0])
}

pub fn locateEvidence(source: &Value, evidence: &Value, claimed_line: &Value) -> Value {
    match locate_inner(&str_qq(Some(source)), &str_qq(Some(evidence)), Some(claimed_line)) {
        Some(n) => json!(n), None => Value::Null,
    }
}

pub fn validateLead(raw: &Value, source: &Value) -> Value {
    let detail = take_chars(&ws_collapse(&str_qq(raw.get("detail"))), 500);
    let evidence = take_chars(&str_qq(raw.get("evidence")), 400);
    let severity = normalizeSeverity(raw.get("severity").unwrap_or(&Value::Null));
    let lead = |line: Value| json!({ "line": line, "severity": severity, "detail": detail, "evidence": evidence });
    if detail.is_empty() { return json!({ "ok": false, "reason": "malformed", "lead": lead(Value::Null) }); }
    if ws_collapse(&evidence).chars().count() < 4 { return json!({ "ok": false, "reason": "no-evidence", "lead": lead(Value::Null) }); }
    match locate_inner(&str_qq(Some(source)), &evidence, raw.get("line")) {
        None => json!({ "ok": false, "reason": "ungrounded-evidence", "lead": lead(Value::Null) }),
        Some(n) => json!({ "ok": true, "lead": lead(json!(n)) }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grounds() {
        assert_eq!(normalizeSeverity(&json!("HIGH")), json!("high"));
        assert_eq!(normalizeSeverity(&json!("bogus")), json!("medium"));
        assert_eq!(kebab(&json!("  Hello, World!!  ")), json!("hello-world"));
        assert_eq!(parseFindingsJson(&json!("```json\n{\"findings\":[{\"a\":1}]}\n```")), json!([{ "a": 1 }]));
        assert_eq!(parseFindingsJson(&json!("garbage")), json!([]));
        let src = "line one\nconst x = broken();\nline three";
        assert_eq!(locateEvidence(&json!(src), &json!("const x = broken();"), &json!(2)), json!(2));
        assert_eq!(locateEvidence(&json!(src), &json!("not in file"), &json!(1)), Value::Null);
        let v = validateLead(&json!({ "detail": "bug here", "evidence": "const x = broken();", "severity": "high", "line": 2 }), &json!(src));
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["lead"]["line"], json!(2));
        assert_eq!(validateLead(&json!({ "detail": "", "evidence": "x" }), &json!(src))["reason"], json!("malformed"));
    }
}
