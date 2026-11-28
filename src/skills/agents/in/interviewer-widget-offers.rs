//! interviewer-widget-offers.rs — twin of skills/agents/in/interviewer-widget-offers.mjs (no-db, pure).
//! NAMES ARE CONTRACT: `interviewerWidgetOffers` (default). Deterministic widget offers for a chat turn.
// QUALITY: 82/100 — 3 signals (result-shape > entity-file > intent-keyword), dedup-by-kind, cap 4, default
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;
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
fn tmpl(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(Value::Null) => "null".into(), Some(x) => str_qq(Some(x)) } }
fn is_obj_or_arr(v: &Value) -> bool { v.is_object() || v.is_array() }

fn keyword_rules() -> &'static Vec<(Regex, &'static str, &'static str)> {
    static R: OnceLock<Vec<(Regex, &'static str, &'static str)>> = OnceLock::new();
    R.get_or_init(|| vec![
        (Regex::new(r"(?i)\b(sample|example|snippet|show me\b[^.?!]*\b(code|function|implementation))\b").unwrap(), "code", "Code sample"),
        (Regex::new(r"(?i)\b(diagram|architecture|visuali[sz]e|how\b[^.?!]*\b(connect|relate|fit|work together))\b").unwrap(), "diagram", "Diagram"),
        (Regex::new(r"(?i)\bgraph\b|\bdependenc\w*|\bimports?\b|\bcoupling\b|who (calls|uses|imports)").unwrap(), "graph", "Dependency graph"),
        (Regex::new(r"(?i)\b(improve|improvement|better|refactor|smell|weakness|issue|problem|bad|wrong|messy|concerning|quality|technical debt|not (great|good)|what'?s? (bad|wrong)|could\b[^.?!]*\bhave)\b").unwrap(), "improvements", "Improvements"),
        (Regex::new(r"(?i)\b(compare|diff|before\b[^.?!]*\bafter|\bvs\b|versus)\b").unwrap(), "diff", "Compare"),
        (Regex::new(r"(?i)\b(logic|algorithm|the shape of|design (the )?logic|how (would|should)\b[^.?!]*\bwork)\b").unwrap(), "logic-spec", "Logic design"),
        (Regex::new(r"(?i)\b(as a table|in a table|spreadsheet|list all|rows and columns)\b").unwrap(), "table", "Table"),
        (Regex::new(r"(?i)\b(audit|history|timeline|what happened|when did|trail)\b").unwrap(), "audit-timeline", "Audit timeline"),
        (Regex::new(r"(?i)\b(memory|memories|lesson|remember|recall|learned|in the past)\b").unwrap(), "memory-search", "Memory"),
        (Regex::new(r"(?i)\b(queue|dispatch|what'?s running|driver (queue|status))\b").unwrap(), "driver-queue", "Driver queue"),
        (Regex::new(r"(?i)\b(pipeline|stages?|progress of|where is\b[^.?!]*\bcard)\b").unwrap(), "pipeline", "Pipeline"),
    ])
}
fn file_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\b([\w.-]+/[\w./-]+\.[a-z]{1,5})\b").unwrap()) }

fn shape_offers(dispatch_results: &Value) -> Vec<Value> {
    let empty: Vec<Value> = vec![];
    let mut offers: Vec<Value> = Vec::new();
    for d in dispatch_results.as_array().unwrap_or(&empty) {
        if !truthy(d.get("ok")) { continue; }
        let r = d.get("result").cloned().unwrap_or(Value::Null);
        let cards: Option<Vec<Value>> = r.get("cards").and_then(|v| v.as_array()).cloned().or_else(|| r.as_array().cloned());
        let is_cards = cards.as_ref().map_or(false, |c| c.first().map_or(false, |c0| c0.is_object() && truthy(c0.get("slug")) && truthy(c0.get("status"))));
        if is_cards {
            continue;
        } else if r.as_array().map_or(false, |a| !a.is_empty() && is_obj_or_arr(&a[0])) {
            let n = r.as_array().unwrap().len();
            offers.push(json!({ "kind": "table", "label": format!("Table ({} rows)", n), "args": { "rows": r }, "source": "shape" }));
        } else if { let cs = r.get("card").filter(|v| !v.is_null()).unwrap_or(&r); truthy(cs.get("slug")) } {
            let cs = r.get("card").filter(|v| !v.is_null()).cloned().unwrap_or_else(|| r.clone());
            offers.push(json!({ "kind": "card-detail", "label": format!("Card {}", str_qq(cs.get("slug"))), "args": { "slug": cs.get("slug").cloned().unwrap_or(Value::Null) }, "source": "shape" }));
        } else if r.as_str().map_or(false, |s| s.contains('\n')) {
            offers.push(json!({ "kind": "code", "label": "View output", "args": { "code": r }, "source": "shape" }));
        }
    }
    offers
}

fn entity_offers(text: &str) -> Vec<Value> {
    let mut seen: Vec<String> = Vec::new();
    for cap in file_re().captures_iter(text) {
        let f = cap[1].to_string();
        if !seen.contains(&f) { seen.push(f); }
    }
    seen.truncate(2);
    seen.iter().map(|f| {
        let base = f.rsplit('/').next().unwrap_or("");
        json!({ "kind": "code", "label": format!("Code sample: {}", base), "args": { "path": f }, "source": "entity" })
    }).collect()
}

pub fn interviewerWidgetOffers(input: &Value, _db: &Connection) -> Result<Value, String> {
    let user_text = input.get("user_text").cloned().unwrap_or(json!(""));
    let dispatch_results = input.get("dispatch_results").cloned().unwrap_or(json!([]));
    let answer_md = input.get("answer_md").cloned().unwrap_or(json!(""));

    let mut ranked: Vec<Value> = shape_offers(&dispatch_results);
    ranked.extend(entity_offers(&format!("{} {}", tmpl(Some(&answer_md)), tmpl(Some(&user_text)))));
    let ut = str_qq(Some(&user_text));
    for (re, kind, label) in keyword_rules() {
        if re.is_match(&ut) { ranked.push(json!({ "kind": kind, "label": label, "args": { "query": user_text }, "source": "keyword" })); }
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut offers: Vec<Value> = Vec::new();
    for o in ranked {
        let kind = str_qq(o.get("kind"));
        if seen.contains(&kind) { continue; }
        seen.insert(kind);
        offers.push(o);
        if offers.len() >= 4 { break; }
    }
    if offers.is_empty() { offers.push(json!({ "kind": "diagram", "label": "Diagram this", "args": { "query": user_text }, "source": "default" })); }
    Ok(json!({ "offers": offers }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(i: Value) -> Value { interviewerWidgetOffers(&i, &Connection::open_in_memory().unwrap()).unwrap() }
    fn kinds(r: &Value) -> Vec<String> { r["offers"].as_array().unwrap().iter().map(|o| o["kind"].as_str().unwrap().to_string()).collect() }
    #[test]
    fn offers() {
        assert!(kinds(&call(json!({ "user_text": "show me some code samples" }))).contains(&"code".to_string()));
        assert!(kinds(&call(json!({ "user_text": "how do these connect together?" }))).contains(&"diagram".to_string()));
        // shape signal beats keyword; a table result → table offer
        let shaped = call(json!({ "dispatch_results": [{ "ok": true, "result": [{ "a": 1 }, { "a": 2 }] }], "user_text": "graph it" }));
        assert_eq!(shaped["offers"][0]["kind"], json!("table"));
        assert_eq!(shaped["offers"][0]["label"], json!("Table (2 rows)"));
        // entity file → code offer with path
        let ent = call(json!({ "answer_md": "see src/foo/bar.ts for details", "user_text": "" }));
        assert_eq!(ent["offers"][0]["kind"], json!("code"));
        assert_eq!(ent["offers"][0]["args"]["path"], json!("src/foo/bar.ts"));
        // no signal → default diagram
        assert_eq!(call(json!({ "user_text": "hello there friend" }))["offers"][0]["source"], json!("default"));
        // cap at 4
        assert!(call(json!({ "user_text": "sample diagram graph improve compare logic table audit memory" }))["offers"].as_array().unwrap().len() <= 4);
    }
}
