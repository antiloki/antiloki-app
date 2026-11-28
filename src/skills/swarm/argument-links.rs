//! argument-links.rs — twin of skills/swarm/argument-links.mjs (no-db, pure). NAMES ARE CONTRACT:
//! argumentLinks (default) · validateLink · validateArgument · validateCase. The evidence grounding floor.
// QUALITY: 83/100 — resolve each cited link (contract/microtask/snippet/card/block/screening/codebase) to content
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn str_qq(v: Option<&Value>) -> String { match v { None | Some(Value::Null) => String::new(), Some(x) => js_string(x) } }
fn tmpl(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(x) => js_string(x) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn is_kind(k: &str) -> bool { matches!(k, "contract" | "microtask" | "snippet" | "card" | "block" | "screening" | "codebase") }

fn line_slice(text: Option<&Value>, start: i64, end: i64) -> Option<String> {
    let s = str_qq(text);
    let lines: Vec<&str> = s.split('\n').collect();
    let n = lines.len() as i64;
    if start < 1 || start > n { return None; }
    let e = end.min(n);
    if e < start { return None; }
    Some(lines[(start - 1) as usize..e as usize].join("\n"))
}
fn parse_range(reff: Option<&Value>) -> Option<(i64, i64)> {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"^(\d+)(?:\s*-\s*(\d+))?$").unwrap());
    let s = str_qq(reff);
    let c = re.captures(&s)?;
    let start: i64 = c[1].parse().ok()?;
    let end: i64 = c.get(2).map(|m| m.as_str().parse().unwrap_or(start)).unwrap_or(start);
    if end >= start { Some((start, end)) } else { None }
}
fn parse_snippet_ref(reff: Option<&Value>) -> (String, Option<(i64, i64)>) {
    static R: OnceLock<Regex> = OnceLock::new();
    let re = R.get_or_init(|| Regex::new(r"^(.*?):(\d+)(?:\s*-\s*(\d+))?$").unwrap());
    let s = str_qq(reff);
    if let Some(c) = re.captures(&s) {
        let p = c[1].to_string();
        if !p.is_empty() {
            let start: i64 = c[2].parse().unwrap_or(0);
            let end: i64 = c.get(3).map(|m| m.as_str().parse().unwrap_or(start)).unwrap_or(start);
            return (p, if end >= start { Some((start, end)) } else { None });
        }
    }
    (s, None)
}
fn mt_id(m: &Value) -> Option<Value> { nz(m.get("id")).or_else(|| nz(m.get("microtask_id"))).cloned() }

fn ok(base: &Value, locator: String, content: Value) -> Value {
    let mut o = base.as_object().unwrap().clone();
    o.insert("valid".into(), json!(true));
    o.insert("resolved".into(), json!({ "locator": locator, "content": content }));
    o.insert("reason".into(), Value::Null);
    Value::Object(o)
}
fn bad(base: &Value, reason: String) -> Value {
    let mut o = base.as_object().unwrap().clone();
    o.insert("valid".into(), json!(false));
    o.insert("resolved".into(), Value::Null);
    o.insert("reason".into(), json!(reason));
    Value::Object(o)
}

pub fn validateLink(link: &Value, corpus: &Value) -> Value {
    let base = json!({ "id": nz(link.get("id")).cloned().unwrap_or(Value::Null), "kind": nz(link.get("kind")).cloned().unwrap_or(Value::Null), "ref": nz(link.get("ref")).cloned().unwrap_or(Value::Null) });
    let kind = str_qq(link.get("kind")).to_lowercase();
    if !is_kind(&kind) { return bad(&base, format!("unknown link kind '{}' (allowed: contract, microtask, snippet)", tmpl(link.get("kind")))); }
    let reff = link.get("ref");
    if reff.map_or(true, |v| v.is_null()) || reff.and_then(|v| v.as_str()) == Some("") { return bad(&base, "link has no ref".into()); }
    let ref_str = str_qq(reff);

    if kind == "contract" {
        let range = match parse_range(reff) { Some(r) => r, None => return bad(&base, format!("contract ref '{}' is not a line number or range (e.g. \"12\" or \"12-15\")", ref_str)) };
        match line_slice(corpus.get("contract_md"), range.0, range.1) {
            None => bad(&base, format!("contract has no line(s) {}-{}", range.0, range.1)),
            Some(content) => ok(&base, format!("contract:{}-{}", range.0, range.1), json!(content)),
        }
    } else if kind == "microtask" {
        let empty = vec![];
        let list = corpus.get("microtasks").and_then(|v| v.as_array()).unwrap_or(&empty);
        match list.iter().find(|m| mt_id(m).map(|v| js_string(&v)).unwrap_or("null".into()) == ref_str) {
            None => bad(&base, format!("no microtask with id '{}'", ref_str)),
            Some(mt) => {
                let content = nz(mt.get("file_spec_md")).or_else(|| nz(mt.get("title"))).or_else(|| nz(mt.get("file"))).cloned().unwrap_or(json!(format!("microtask {}", ref_str)));
                ok(&base, format!("microtask:{}", ref_str), content)
            }
        }
    } else if kind == "card" {
        let card = match nz(corpus.get("card")) { Some(c) => c, None => return bad(&base, "no card in the evidence bundle".into()) };
        if (ref_str == "slug" || ref_str == "title") && card.get(&ref_str).map_or(false, |v| !v.is_null()) {
            return ok(&base, format!("card:{}", ref_str), json!(str_qq(card.get(&ref_str))));
        }
        if card.get("fields").and_then(|f| f.get(&ref_str)).map_or(false, |v| !v.is_null()) {
            return ok(&base, format!("card:{}", ref_str), json!(str_qq(card.get("fields").and_then(|f| f.get(&ref_str)))));
        }
        let empty = vec![];
        let feat = card.get("acceptance").and_then(|v| v.as_array()).unwrap_or(&empty).iter().find(|f| js_string(f.get("id").unwrap_or(&Value::Null)) == ref_str);
        match feat {
            Some(f) => ok(&base, format!("card:{}", ref_str), json!(str_qq(nz(f.get("intent"))))),
            None => bad(&base, format!("card has no field/feature '{}'", ref_str)),
        }
    } else if kind == "block" {
        let flat = crate::blueprint_block_shape::flattenBlocks(corpus.get("blocks").unwrap_or(&Value::Null));
        match flat.get(&ref_str).filter(|v| !v.is_null()) {
            None => bad(&base, format!("no block (design node) '{}' in the blueprint", ref_str)),
            Some(hit) => {
                let b = hit.get("block").cloned().unwrap_or(Value::Null);
                let content = format!("[{}] {}", b.get("type").and_then(|v| v.as_str()).unwrap_or("?"), str_qq(nz(b.get("intent")))).trim().to_string();
                ok(&base, format!("block:{}", ref_str), json!(content))
            }
        }
    } else if kind == "screening" {
        let s = corpus.get("screening");
        match s.and_then(|s| nz(s.get(&ref_str))) {
            None => bad(&base, format!("screening has no key '{}'", ref_str)),
            Some(v) => { let content = if v.is_string() { v.as_str().unwrap().to_string() } else { serde_json::to_string(v).unwrap_or_else(|_| "null".into()) }; ok(&base, format!("screening:{}", ref_str), json!(content)) }
        }
    } else if kind == "codebase" {
        let empty = vec![];
        let list = corpus.get("codebase_context").and_then(|c| c.get("files")).and_then(|v| v.as_array()).unwrap_or(&empty);
        match list.iter().find(|x| js_string(x.get("path").unwrap_or(&Value::Null)) == ref_str) {
            None => bad(&base, format!("no codebase file '{}' in the evidence bundle (atlas may not cover it)", ref_str)),
            Some(hit) => ok(&base, format!("codebase:{}", ref_str), json!(str_qq(nz(hit.get("md"))))),
        }
    } else {
        // snippet
        let (path, range) = parse_snippet_ref(reff);
        let empty = vec![];
        let list = corpus.get("snippets").and_then(|v| v.as_array()).unwrap_or(&empty);
        let snip = match list.iter().find(|s| js_string(s.get("path").unwrap_or(&Value::Null)) == path) { Some(s) => s, None => return bad(&base, format!("no snippet at path '{}'", path)) };
        match range {
            None => ok(&base, format!("snippet:{}", path), json!(str_qq(nz(snip.get("content"))))),
            Some((a, b)) => match line_slice(snip.get("content"), a, b) {
                None => bad(&base, format!("snippet '{}' has no line(s) {}-{}", path, a, b)),
                Some(content) => ok(&base, format!("snippet:{}:{}-{}", path, a, b), json!(content)),
            },
        }
    }
}

pub fn validateArgument(argument: &Value, corpus: &Value) -> Value {
    let empty = vec![];
    let links = argument.get("links").and_then(|v| v.as_array()).unwrap_or(&empty);
    let checked: Vec<Value> = links.iter().map(|l| validateLink(l, corpus)).collect();
    let valid_count = checked.iter().filter(|l| l.get("valid") == Some(&json!(true))).count() as i64;
    json!({
        "id": nz(argument.get("id")).cloned().unwrap_or(Value::Null),
        "side": nz(argument.get("side")).cloned().unwrap_or(Value::Null),
        "claim_md": nz(argument.get("claim_md")).cloned().unwrap_or(json!("")),
        "total": checked.len(),
        "validCount": valid_count,
        "invalidCount": checked.len() as i64 - valid_count,
        "grounded": valid_count > 0,
        "links": checked,
    })
}

pub fn validateCase(input: &Value, corpus: &Value) -> Value {
    let empty = vec![];
    let map_side = |arr: &[Value], default_side: &str| -> Vec<Value> {
        arr.iter().map(|a| {
            let mut m = a.as_object().cloned().unwrap_or_default();
            if nz(a.get("side")).is_none() { m.insert("side".into(), json!(default_side)); }
            validateArgument(&Value::Object(m), corpus)
        }).collect()
    };
    let prosecution = map_side(input.get("prosecution").and_then(|v| v.as_array()).unwrap_or(&empty), "prosecution");
    let defense = map_side(input.get("defense").and_then(|v| v.as_array()).unwrap_or(&empty), "defense");
    let tally = |args: &[Value]| json!({
        "arguments": args.len(),
        "grounded": args.iter().filter(|a| a.get("grounded") == Some(&json!(true))).count(),
        "validLinks": args.iter().map(|a| a.get("validCount").and_then(|v| v.as_i64()).unwrap_or(0)).sum::<i64>(),
        "invalidLinks": args.iter().map(|a| a.get("invalidCount").and_then(|v| v.as_i64()).unwrap_or(0)).sum::<i64>(),
    });
    json!({ "prosecution": prosecution, "defense": defense, "summary": { "prosecution": tally(&prosecution), "defense": tally(&defense) } })
}

pub fn argumentLinks(input: &Value, _db: &Connection) -> Result<Value, String> {
    let corpus = match input.get("corpus") { Some(c) if c.is_object() => c, _ => return Err("argument-links: 'corpus' is required".into()) };
    if let Some(arg) = nz(input.get("argument")) { return Ok(validateArgument(arg, corpus)); }
    if input.get("prosecution").map_or(false, |v| v.is_array()) || input.get("defense").map_or(false, |v| v.is_array()) { return Ok(validateCase(input, corpus)); }
    Err("argument-links: 'argument | prosecution/defense' is required".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates() {
        let corpus = json!({ "contract_md": "line1\nline2\nline3\nline4", "microtasks": [{ "id": "m1", "title": "do a" }], "snippets": [{ "path": "src/a.ts", "content": "aa\nbb\ncc" }] });
        assert_eq!(validateLink(&json!({ "id": "L1", "kind": "contract", "ref": "2-3" }), &corpus)["resolved"]["content"], json!("line2\nline3"));
        assert_eq!(validateLink(&json!({ "kind": "microtask", "ref": "m1" }), &corpus)["resolved"]["content"], json!("do a"));
        assert_eq!(validateLink(&json!({ "kind": "snippet", "ref": "src/a.ts:2" }), &corpus)["resolved"]["content"], json!("bb"));
        assert_eq!(validateLink(&json!({ "kind": "bogus", "ref": "x" }), &corpus)["valid"], json!(false));
        let arg = validateArgument(&json!({ "id": "A1", "links": [{ "kind": "contract", "ref": "1" }, { "kind": "contract", "ref": "999" }] }), &corpus);
        assert_eq!(arg["validCount"], json!(1));
        assert_eq!(arg["grounded"], json!(true));
        let case = validateCase(&json!({ "prosecution": [{ "id": "P1", "links": [{ "kind": "microtask", "ref": "m1" }] }], "defense": [] }), &corpus);
        assert_eq!(case["summary"]["prosecution"]["validLinks"], json!(1));
        assert_eq!(case["prosecution"][0]["side"], json!("prosecution"));
    }
}
