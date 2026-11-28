//! atlas-smell-to-card.rs — twin of skills/codebase/atlas-smell-to-card.mjs (db WRITE lane). NAMES
//! ARE CONTRACT: `atlasSmellToCard` (named default). Turns a structural smell into a backlog card
//! via card-add, so Atlas Insights becomes a work generator for the driver.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }
fn js_string_opt(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(x) => js_string(x) } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }

// = JS Array.prototype.join(sep): null/undefined elements render as '' (not "null").
fn js_array_join(arr: &[Value], sep: &str) -> String {
    arr.iter().map(|v| match v { Value::Null => String::new(), other => js_string(other) }).collect::<Vec<_>>().join(sep)
}

// = JS baseName: String(p ?? '').split(/[/\\]/).filter(Boolean).pop() || String(p)
fn base_name(p: Option<&Value>) -> String {
    let s = str_qq(p);
    let parts: Vec<&str> = s.split(|c: char| c == '/' || c == '\\').filter(|x| !x.is_empty()).collect();
    match parts.last() {
        Some(last) if !last.is_empty() => last.to_string(),
        _ => js_string_opt(p),
    }
}

fn nodes_vec(smell: &Value) -> Vec<Value> {
    smell.get("nodes").and_then(|v| v.as_array()).cloned().unwrap_or_default()
}

struct Template { title: String, intent: String }

fn template(smell: &Value) -> Template {
    let kind = smell.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let nodes = nodes_vec(smell);
    let detail = tmpl(smell.get("detail"));
    match kind.as_str() {
        "cycle" => {
            let head: Vec<String> = nodes.iter().take(3).map(|n| base_name(Some(n))).collect();
            let suffix = if nodes.len() > 3 { " \u{2026}" } else { "" };
            Template {
                title: format!("Break circular dependency: {}{}", head.join(" \u{2194} "), suffix),
                intent: format!(
                    "These files form a circular import dependency ({}). Break the cycle \u{2014} extract the shared piece into a new module, invert one dependency, or merge the tightly-coupled files. Cycles block modularization and make the code hard to reason about.",
                    js_array_join(&nodes, ", ")
                ),
            }
        }
        "test-gap" => {
            let n0 = nodes.first();
            Template {
                title: format!("Add tests for the central file {}", base_name(n0)),
                intent: format!(
                    "{} is a central, depended-on file with no test covering it ({}). Add a focused test for its public behavior so a regression here is caught before it ripples to its dependents.",
                    tmpl(n0), detail
                ),
            }
        }
        "god-file" => {
            let n0 = nodes.first();
            Template {
                title: format!("Split the god-file {}", base_name(n0)),
                intent: format!(
                    "{} is both heavily depended-on and complex ({}) \u{2014} a change magnet. Split it into smaller, single-responsibility modules to shrink its blast radius and complexity.",
                    tmpl(n0), detail
                ),
            }
        }
        "orphan" => {
            let n0 = nodes.first();
            Template {
                title: format!("Remove or wire up the orphan file {}", base_name(n0)),
                intent: format!(
                    "{} has no imports in or out \u{2014} an isolated file. Delete it if it's dead, or wire it into the codebase if it's meant to be used.",
                    tmpl(n0)
                ),
            }
        }
        "unreferenced-file" => {
            let n0 = nodes.first();
            Template {
                title: format!("Remove or use the unreferenced file {}", base_name(n0)),
                intent: format!(
                    "Nothing imports {} \u{2014} its exports look unused ({}). Delete it if it's dead, or wire it in if it should be used.",
                    tmpl(n0), detail
                ),
            }
        }
        _ => {
            // fallback(s): title uses baseName((nodes ?? [])[0] ?? kind); intent lists nodes ?? [].
            let n0 = nodes.first().cloned().unwrap_or_else(|| json!(kind.clone()));
            let n0 = if n0.is_null() { json!(kind.clone()) } else { n0 };
            let affected = js_array_join(&nodes, ", ");
            let affected = if affected.is_empty() { "\u{2014}".to_string() } else { affected };
            Template {
                title: format!("Address {}: {}", kind, base_name(Some(&n0))),
                intent: format!("Atlas Insights flagged a '{}' smell: {}. Affected: {}.", kind, detail, affected),
            }
        }
    }
}

// six hex uppercase chars (mirrors randomUUID().slice(0,6).toUpperCase()); NOT byte-identical to
// node:crypto's RNG (never exercised by conform fixtures — those always pass an explicit slug).
fn random_hex6() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let mut x = (nanos as u64) ^ 0x9E3779B97F4A7C15u64;
    x ^= x << 13; x ^= x >> 7; x ^= x << 17;
    format!("{:06X}", x & 0xFFFFFF)
}

pub fn atlasSmellToCard(input: &Value, db: &Connection) -> Result<Value, String> {
    let smell = input.get("smell").cloned();
    let smell = match &smell {
        Some(s) if !s.is_null() && truthy(s.get("kind")) => s.clone(),
        _ => return Err("atlas-smell-to-card: smell is required".into()),
    };
    let kind = smell.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();

    let t = template(&smell);

    let slug = match input.get("slug") {
        Some(s) if !s.is_null() => str_qq(Some(s)),
        _ => {
            let cleaned: String = kind.to_uppercase().chars().filter(|c| c.is_ascii_uppercase() || c.is_ascii_digit()).collect();
            format!("FIX-ATLAS-{}-{}", cleaned, random_hex6())
        }
    };

    let nodes = nodes_vec(&smell);
    let files: Vec<Value> = nodes.into_iter().filter(|n| n.is_string()).collect();

    let mut kind_by_smell = Map::new();
    kind_by_smell.insert("cycle".into(), json!("fix"));
    kind_by_smell.insert("test-gap".into(), json!("gap"));
    kind_by_smell.insert("god-file".into(), json!("fix"));
    kind_by_smell.insert("orphan".into(), json!("fix"));
    kind_by_smell.insert("unreferenced-file".into(), json!("fix"));
    let body_kind = kind_by_smell.get(&kind).cloned().unwrap_or(json!("fix"));

    let size = if kind == "god-file" || kind == "cycle" { "M" } else { "S" };

    let severity = tmpl(smell.get("severity"));
    let detail = tmpl(smell.get("detail"));
    let mut context_lines: Vec<Value> = Vec::new();
    context_lines.push(json!(format!("Atlas structural smell: {} ({}-severity).", kind, severity)));
    context_lines.push(json!(format!("Detail: {}", detail)));
    if !files.is_empty() {
        context_lines.push(json!(format!("Affected: {}", js_array_join(&files, ", "))));
    }

    let mut body = Map::new();
    body.insert("kind".into(), body_kind);
    body.insert("size".into(), json!(size));
    body.insert("motivation".into(), json!(t.intent));
    body.insert("context".into(), Value::Array(context_lines));
    body.insert("acceptance_criteria".into(), json!([format!("The '{}' smell no longer appears when re-running atlas-smells on this codebase.", kind)]));
    if !files.is_empty() {
        body.insert("files_touched".into(), Value::Array(files));
    }

    let card_input = json!({ "slug": slug, "title": t.title, "status": "backlog", "body": Value::Object(body) });
    let card = crate::entities_card_add::cardAdd(&card_input, db)?;

    Ok(json!({
        "card_slug": card.get("slug").cloned().unwrap_or(Value::Null),
        "title": card.get("title").cloned().unwrap_or(Value::Null),
        "kind": kind,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cards_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT UNIQUE, title TEXT, status TEXT, body_json TEXT, parent_slug TEXT)").unwrap();
        c
    }

    #[test]
    fn cycle_smell_makes_a_fix_card() {
        let c = cards_db();
        let r = atlasSmellToCard(&json!({
            "slug": "FIX-ATLAS-CYCLE-ABC123",
            "smell": { "kind": "cycle", "severity": "high", "nodes": ["a.ts", "b.ts"], "detail": "circular dependency among 2 files" }
        }), &c).unwrap();
        assert_eq!(r["card_slug"], json!("FIX-ATLAS-CYCLE-ABC123"));
        assert_eq!(r["kind"], json!("cycle"));
        assert!(r["title"].as_str().unwrap().contains("a.ts"));

        let row: String = c.query_row("SELECT body_json FROM cards WHERE slug = ?", ["FIX-ATLAS-CYCLE-ABC123"], |r| r.get(0)).unwrap();
        let body: Value = serde_json::from_str(&row).unwrap();
        assert_eq!(body["kind"], json!("fix"));
        assert_eq!(body["size"], json!("M"));
        assert!(body["motivation"].as_str().unwrap().contains("a.ts"));
    }

    #[test]
    fn test_gap_smell_maps_to_gap_kind_and_size_s() {
        let c = cards_db();
        let r = atlasSmellToCard(&json!({
            "slug": "FIX-ATLAS-TESTGAP-XYZ999",
            "smell": { "kind": "test-gap", "severity": "high", "nodes": ["core.ts"], "detail": "central (fan_in 5) but no test" }
        }), &c).unwrap();
        assert_eq!(r["kind"], json!("test-gap"));
        assert!(r["title"].as_str().unwrap().contains("Add tests"));
        let row: String = c.query_row("SELECT body_json FROM cards WHERE slug = ?", ["FIX-ATLAS-TESTGAP-XYZ999"], |r| r.get(0)).unwrap();
        let body: Value = serde_json::from_str(&row).unwrap();
        assert_eq!(body["kind"], json!("gap"));
        assert_eq!(body["size"], json!("S"));
    }

    #[test]
    fn requires_smell() {
        let c = cards_db();
        assert!(atlasSmellToCard(&json!({}), &c).is_err());
    }
}
