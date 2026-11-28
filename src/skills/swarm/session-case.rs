//! session-case.rs — twin of skills/swarm/session-case.mjs (no-db, pure). NAMES ARE CONTRACT:
//! `sessionCase` (default) · renderSessionCase. Deterministic plea document (cross-module argument-links).
// QUALITY: 81/100 — validate every link, transclude the grounded ones, strike the ungrounded, tally
use rusqlite::Connection;
use serde_json::{json, Value};

fn str_qq(v: Option<&Value>) -> String {
    match v { None | Some(Value::Null) => String::new(), Some(Value::String(s)) => s.clone(), Some(Value::Bool(b)) => b.to_string(), Some(n @ Value::Number(_)) => n.to_string(), Some(o) => o.to_string() }
}
fn tmpl(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(Value::Null) => "null".into(), Some(x) => str_qq(Some(x)) } }
fn truthy(v: Option<&Value>) -> bool {
    match v { None | Some(Value::Null) => false, Some(Value::Bool(b)) => *b, Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()), Some(Value::String(s)) => !s.is_empty(), Some(_) => true }
}

fn blockquote(text: Option<&Value>) -> String { str_qq(text).split('\n').map(|l| format!("> {}", l)).collect::<Vec<_>>().join("\n") }

fn render_argument(arg: &Value, n: usize, tag: &str) -> String {
    let claim = if truthy(arg.get("claim_md")) { str_qq(arg.get("claim_md")) } else { "(no claim)".to_string() };
    let mut lines = vec![format!("### {}{}. {}", tag, n, claim)];
    let empty = vec![];
    let links = arg.get("links").and_then(|v| v.as_array()).unwrap_or(&empty);
    if links.is_empty() {
        lines.push(String::new());
        lines.push("_no links — ungrounded, the judge must ignore this argument._".into());
        return lines.join("\n");
    }
    for l in links {
        if truthy(l.get("valid")) {
            lines.push(String::new());
            lines.push(format!("**[[{}]]**", tmpl(l.get("resolved").and_then(|r| r.get("locator")))));
            lines.push(blockquote(l.get("resolved").and_then(|r| r.get("content"))));
        } else {
            lines.push(String::new());
            lines.push(format!("~~[[{}:{}]]~~ — \u{26a0} UNGROUNDED: {}", tmpl(l.get("kind")), tmpl(l.get("ref")), tmpl(l.get("reason"))));
        }
    }
    lines.join("\n")
}

fn render_side(title: &str, subtitle: &str, args: &[Value], tag: &str) -> String {
    let mut out = vec![format!("## {} — {}", title, subtitle)];
    if args.is_empty() { out.push(String::new()); out.push("_no arguments submitted._".into()); return out.join("\n"); }
    for (i, a) in args.iter().enumerate() { out.push(String::new()); out.push(render_argument(a, i + 1, tag)); }
    out.join("\n")
}

pub fn renderSessionCase(input: &Value, corpus: &Value) -> Value {
    let validated = crate::swarm_argument_links::validateCase(input, corpus);
    let empty = vec![];
    let prosecution = validated.get("prosecution").and_then(|v| v.as_array()).unwrap_or(&empty).clone();
    let defense = validated.get("defense").and_then(|v| v.as_array()).unwrap_or(&empty).clone();
    let summary = validated.get("summary").cloned().unwrap_or(json!({}));
    let scope = if truthy(input.get("scope")) { format!(" — {}", str_qq(input.get("scope"))) } else { String::new() };
    let sp = |side: &str, k: &str| tmpl(summary.get(side).and_then(|s| s.get(k)));
    let doc = [
        format!("# Session Case{}", scope),
        String::new(),
        "> The judge weighs only GROUNDED arguments — those with at least one link that".into(),
        "> resolves to a real proof (a contract line, a microtask, or a snippet).".into(),
        "> Struck-through links are ungrounded and are excluded from the weighing.".into(),
        String::new(),
        render_side("Prosecution", "argues the work is INSUFFICIENT", &prosecution, "P"),
        String::new(),
        render_side("Defense", "argues the work SUFFICES", &defense, "D"),
        String::new(),
        "## Grounding summary".into(),
        String::new(),
        "| side | arguments | grounded | links \u{2713} | links \u{2717} |".into(),
        "| --- | --- | --- | --- | --- |".into(),
        format!("| prosecution | {} | {} | {} | {} |", sp("prosecution", "arguments"), sp("prosecution", "grounded"), sp("prosecution", "validLinks"), sp("prosecution", "invalidLinks")),
        format!("| defense | {} | {} | {} | {} |", sp("defense", "arguments"), sp("defense", "grounded"), sp("defense", "validLinks"), sp("defense", "invalidLinks")),
    ].join("\n");
    json!({ "document_md": doc, "summary": summary, "prosecution": prosecution, "defense": defense })
}

pub fn sessionCase(input: &Value, _db: &Connection) -> Result<Value, String> {
    let corpus = match input.get("corpus") { Some(c) if c.is_object() => c, _ => return Err("session-case: 'corpus' is required".into()) };
    if !input.get("prosecution").map_or(false, |v| v.is_array()) && !input.get("defense").map_or(false, |v| v.is_array()) {
        return Err("session-case: 'prosecution | defense' is required".into());
    }
    Ok(renderSessionCase(input, corpus))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders() {
        let corpus = json!({ "contract_md": "goal\naccept: starts\naccept: config", "microtasks": [{ "id": "mt-1", "file_spec_md": "loader" }], "snippets": [{ "path": "src/server.js", "content": "fn start(){}" }] });
        let input = json!({ "scope": "card-X", "prosecution": [{ "id": "p1", "claim_md": "config never validated", "links": [{ "kind": "contract", "ref": "3" }] }], "defense": [{ "id": "d1", "claim_md": "server starts", "links": [{ "kind": "snippet", "ref": "src/server.js" }, { "kind": "microtask", "ref": "ghost" }] }] });
        let r = renderSessionCase(&input, &corpus);
        let doc = r["document_md"].as_str().unwrap();
        assert!(doc.contains("# Session Case — card-X"));
        assert!(doc.contains("## Prosecution — argues the work is INSUFFICIENT"));
        assert!(doc.contains("## Defense — argues the work SUFFICES"));
        assert!(doc.contains("UNGROUNDED")); // the ghost microtask link
        let mut with_corpus = input.as_object().unwrap().clone();
        with_corpus.insert("corpus".into(), corpus.clone());
        assert_eq!(sessionCase(&Value::Object(with_corpus), &Connection::open_in_memory().unwrap()).unwrap()["document_md"], r["document_md"]);
    }
}
