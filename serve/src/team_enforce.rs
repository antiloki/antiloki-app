//! THE ENFORCE NODE (RFC 0014; operator 2026-11-16: "a specialized NODE for JSON enforcement, a tiny model + code"): a raw text goes in, a typed value comes out or a verdict names what is missing.
//! Order of work: (1) CODE extracts the JSON object and repairs what code can repair (fences, a reasoning wrapper, single quotes, unquoted keys, trailing commas) and coerces types (a number written as text, "yes" for true, one value where a list is wanted, an enum in another case);
//! (2) only if that does not give a valid answer, a TINY model (local, `format` = the schema, so its output cannot be invalid JSON) restructures the text, under the contract that it may only move what the text already says;
//! (3) CODE validates again. A model never decides whether the answer is valid.
use crate::team_llm::{extract_json, validate};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnforceSpec {
    /** where the raw text is: `node.field` of an earlier node's output, or `ask` */ pub from: String,
    /** the typed answer (the same grammar as a model node's `answer`) */ pub schema: BTreeMap<String, Value>,
    /** the tiny model that restructures when code could not (`local:qwen3:0.6b`); empty = code only */ #[serde(default = "tiny")] pub seat: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub timeout_s: Option<u64>,
}
fn tiny() -> String { "local:qwen3:0.6b".into() }
pub fn check(sp: &EnforceSpec) -> Vec<String> {
    let mut e = vec![]; let m: Map<String, Value> = sp.schema.clone().into_iter().collect(); e.extend(crate::team_llm::check_schema(&m));
    if sp.from != "ask" && !sp.from.contains('.') { e.push("`from` is `node.field` (or `ask`)".into()); }
    if !sp.seat.is_empty() { if let Err(x) = crate::team_llm::parse_seat(&sp.seat) { e.push(format!("the seat {x}")); } }
    e
}
/// A reply with its reasoning wrapper and code fence removed. Pure.
fn bare(text: &str) -> String { let t = crate::team_decompose::strip_think(text); let t = t.trim(); if let Some(i) = t.find("```") { let r = &t[i + 3..]; let r = r.split_once('\n').map_or(r, |(_, b)| b); return r.split("```").next().unwrap_or(r).trim().to_string(); } t.to_string() }
/// The JSON a reply holds after the repairs code can make: the object itself, else with single quotes made double, bare keys quoted, trailing commas dropped. Pure.
pub fn repair_json(text: &str) -> Option<Value> {
    let t = bare(text); if let Some(v) = extract_json(&t) { return Some(v); }
    let start = t.find('{')?; let end = t.rfind('}')?; if end < start { return None; } let mut s = t[start..=end].to_string();
    if !s.contains('"') { s = s.replace('\'', "\""); }
    s = regex::Regex::new(r"([{,]\s*)([A-Za-z_][A-Za-z0-9_]*)\s*:").unwrap().replace_all(&s, "$1\"$2\":").to_string();
    // a bare word as a value (`urgent: no`) becomes text; true, false and null stay what they are
    s = regex::Regex::new(r":\s*([A-Za-z_][A-Za-z0-9_]*)\s*([,}])").unwrap().replace_all(&s, |c: &regex::Captures| if ["true", "false", "null"].contains(&&c[1]) { format!(": {}{}", &c[1], &c[2]) } else { format!(": \"{}\"{}", &c[1], &c[2]) }).to_string();
    s = regex::Regex::new(r",\s*([}\]])").unwrap().replace_all(&s, "$1").to_string();
    serde_json::from_str::<Value>(&s).ok().filter(Value::is_object)
}
/// Make each field the type the schema wants when that is unambiguous: "12" -> 12, "yes" -> true, 3 -> "3", one value -> a list of one, an enum in any case. Pure.
pub fn coerce(schema: &Map<String, Value>, v: &Value) -> Map<String, Value> {
    let mut out = v.as_object().cloned().unwrap_or_default();
    for (k, ty) in schema {
        let Some(x) = out.get(k).cloned() else { continue };
        let want = match ty { Value::String(s) => s.trim_end_matches('?').to_string(), Value::Object(_) => "enum".into(), _ => "list".into() };
        let nv = match want.as_str() {
            "number" => x.as_str().and_then(|s| s.trim().parse::<f64>().ok()).map(|n| json!(n)),
            "bool" => x.as_str().and_then(|s| match s.trim().to_lowercase().as_str() { "true" | "yes" | "y" | "1" => Some(json!(true)), "false" | "no" | "n" | "0" => Some(json!(false)), _ => None }),
            "string" => if x.is_number() || x.is_boolean() { Some(json!(x.to_string())) } else { None },
            "enum" => { let opts = ty["enum"].as_array().cloned().unwrap_or_default(); x.as_str().and_then(|s| opts.iter().find(|o| o.as_str().map_or(false, |o| o.eq_ignore_ascii_case(s.trim()))).cloned()) }
            _ => if x.is_array() { None } else { Some(json!([x])) },
        };
        if let Some(n) = nv { out.insert(k.clone(), n); }
    }
    out
}
/// The schema in JSON Schema form, for a model that is held to it (Ollama `format`). Pure.
pub fn json_schema_of(schema: &Map<String, Value>) -> Value {
    let mut props = Map::new(); let mut req = vec![];
    for (k, ty) in schema {
        let (opt, t) = match ty { Value::String(s) => (s.ends_with('?'), s.trim_end_matches('?').to_string()), Value::Object(_) => (false, "enum".into()), _ => (false, "list".into()) };
        let p = match t.as_str() { "string" => json!({ "type": "string" }), "number" => json!({ "type": "number" }), "bool" => json!({ "type": "boolean" }), "enum" => json!({ "type": "string", "enum": ty["enum"] }), _ => json!({ "type": "array", "items": { "type": if ty.as_array().and_then(|a| a.first()).and_then(Value::as_str) == Some("number") { "number" } else { "string" } } }) };
        props.insert(k.clone(), p); if !opt { req.push(json!(k)); }
    }
    json!({ "type": "object", "properties": props, "required": req })
}
pub fn prompt_of(text: &str, schema: &Map<String, Value>) -> String {
    format!("Turn the TEXT into one JSON object that matches the schema. Use ONLY what the text says: never add a fact. If a field is not in the text, use an empty value for it.\\n\\nSCHEMA: {}\\n\\nTEXT:\\n{}", json_schema_of(schema), text.chars().take(6000).collect::<String>()).replace("\\\\n", "\n")
}
/// What the node did.
#[derive(Clone, Debug, PartialEq)]
pub struct Enforced { pub fields: Map<String, Value>, /** "code" or "model" */ pub by: &'static str, pub usd: f64 }
/// Code first; the tiny model only when code could not make a valid answer; code validates the model's. Err names what is missing.
pub async fn run(sp: &EnforceSpec, raw: &str, lctx: Option<crate::ledger::Ctx>) -> Result<Enforced, String> {
    let schema: Map<String, Value> = sp.schema.clone().into_iter().collect();
    let by_code = |t: &str| repair_json(t).map(|v| coerce(&schema, &v)).and_then(|m| validate(&schema, &Value::Object(m)).ok());
    if let Some(f) = by_code(raw) { return Ok(Enforced { fields: f, by: "code", usd: 0.0 }); }
    if sp.seat.is_empty() { return Err(format!("the text has no valid answer and no seat is set to restructure it (needs: {})", schema.keys().cloned().collect::<Vec<_>>().join(", "))); }
    let seat = crate::team_llm::parse_seat(&sp.seat)?;
    let (txt, usd) = crate::team_llm::call_schema(&seat, &prompt_of(raw, &schema), &json_schema_of(&schema), 800, sp.timeout_s.unwrap_or(120), lctx).await.map(|(t, c, _)| (t, c))?;
    by_code(&txt).map(|f| Enforced { fields: f, by: "model", usd }).ok_or_else(|| format!("the model's answer is not valid either: {}", validate(&schema, &repair_json(&txt).map(|v| Value::Object(coerce(&schema, &v))).unwrap_or(Value::Null)).err().unwrap_or_else(|| "no JSON".into())))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn schema() -> Map<String, Value> { [("kind".to_string(), json!({ "enum": ["bug", "feature"] })), ("n".to_string(), json!("number")), ("urgent".to_string(), json!("bool")), ("tags".to_string(), json!(["string"])), ("why".to_string(), json!("string?"))].into_iter().collect() }
    #[test] fn code_repairs_what_code_can_before_any_model() {
        for t in ["{\"kind\":\"bug\",\"n\":2,\"urgent\":true,\"tags\":[\"a\"]}", "Sure! ```json\n{\"kind\":\"bug\",\"n\":2,\"urgent\":true,\"tags\":[\"a\"]}\n```", "<think></think>{'kind': 'bug', 'n': 2, 'urgent': true, 'tags': ['a'],}", "{kind: \"Bug\", n: \"2\", urgent: \"yes\", tags: \"a\"}"] {
            let v = repair_json(t).unwrap_or_else(|| panic!("repair {t}")); let m = coerce(&schema(), &v); let ok = validate(&schema(), &Value::Object(m.clone())).unwrap_or_else(|e| panic!("{t}: {e} {m:?}"));
            assert_eq!(ok["kind"], json!("bug")); assert_eq!(ok["n"], json!(2.0)); assert_eq!(ok["urgent"], json!(true)); assert_eq!(ok["tags"], json!(["a"]));
        }
        assert!(repair_json("no json at all").is_none()); assert!(validate(&schema(), &Value::Object(coerce(&schema(), &json!({ "kind": "bug" })))).is_err(), "a missing field is not repaired by code");
    }
    #[test] fn the_schema_is_given_to_the_model_as_json_schema() {
        let j = json_schema_of(&schema()); assert_eq!(j["properties"]["kind"]["enum"], json!(["bug", "feature"])); assert_eq!(j["properties"]["n"]["type"], json!("number")); assert_eq!(j["properties"]["tags"]["type"], json!("array"));
        let req: Vec<&str> = j["required"].as_array().unwrap().iter().filter_map(Value::as_str).collect(); assert!(req.contains(&"kind") && !req.contains(&"why"), "an optional field is not required: {req:?}");
        assert!(prompt_of("text", &schema()).contains("Use ONLY what the text says"));
    }
    #[tokio::test] async fn code_alone_answers_when_it_can_and_a_code_only_node_says_what_is_missing() {
        let sp = EnforceSpec { from: "ask".into(), schema: schema().into_iter().collect(), seat: String::new(), timeout_s: None };
        let ok = run(&sp, "{kind: 'feature', n: 3, urgent: no, tags: []}", None).await.unwrap(); assert_eq!(ok.by, "code"); assert_eq!(ok.fields["urgent"], json!(false));
        let e = run(&sp, "the answer is feature", None).await.unwrap_err(); assert!(e.contains("no seat is set") && e.contains("kind"), "{e}");
        assert!(check(&sp).is_empty() && !check(&EnforceSpec { from: "x".into(), ..sp.clone() }).is_empty() && !check(&EnforceSpec { seat: "gpt".into(), ..sp.clone() }).is_empty());
    }
}
