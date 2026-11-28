//! STRICT YAML (RFC 0015, operator 2026-11-17: branches are YAML files a leader or a person can read, write, share and import; the product keeps JSON as its canonical form).
//! A SUBSET of YAML 1.2, read by `yaml-rust2`'s event parser and checked as it is read, so the dangers of YAML are refused, not hoped away:
//!   no anchors, aliases, tags or merge keys · no duplicate keys · keys are always text · one document · YAML 1.2 core types only (so `no`, `on`, `yes` stay TEXT and `1.10` is read as a number only when written as one)
//!   · quoted scalars are always text. The writer is deterministic (sorted keys, one layout), so a file diffs cleanly in git and the same value always gives the same text.
//! `parse_strict(text) -> Value` and `to_yaml(&Value) -> text`; `to_yaml` then `parse_strict` gives back the same value (tested). Models never write YAML: they answer in schema-constrained JSON and code writes the file.
use serde_json::{json, Map, Number, Value};
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::{Marker, TScalarStyle};

enum Frame { Map(Map<String, Value>, Option<String>), Seq(Vec<Value>) }
struct Rx { stack: Vec<Frame>, root: Option<Value>, err: Option<String>, docs: u32 }
impl Rx {
    fn fail(&mut self, line: usize, m: &str) { if self.err.is_none() { self.err = Some(format!("line {line}: {m}")); } }
    fn put(&mut self, v: Value, line: usize) {
        match self.stack.last_mut() {
            None => { if self.root.is_some() { self.fail(line, "only one YAML document is allowed"); } else { self.root = Some(v); } }
            Some(Frame::Seq(a)) => a.push(v),
            Some(Frame::Map(m, key)) => match key.take() { Some(k) => { m.insert(k, v); } None => self.fail(line, "a key must be plain text") },
        }
    }
}
/// A plain (unquoted) scalar read by the YAML 1.2 core schema: null, true/false, a decimal integer, a finite float, else text. Pure.
pub fn plain_value(s: &str) -> Value {
    match s { "" | "~" | "null" | "Null" | "NULL" => return Value::Null, "true" | "True" | "TRUE" => return json!(true), "false" | "False" | "FALSE" => return json!(false), _ => {} }
    let b = s.as_bytes(); let digits = |t: &[u8]| !t.is_empty() && t.iter().all(u8::is_ascii_digit);
    let body = if matches!(b.first(), Some(b'-') | Some(b'+')) { &b[1..] } else { b };
    if digits(body) { if let Ok(i) = s.parse::<i64>() { return json!(i); } }
    // a float: digits, one point, an optional exponent; `.inf` and `.nan` stay text
    let (mant, exp) = match body.iter().position(|c| *c == b'e' || *c == b'E') { Some(i) => (&body[..i], Some(&body[i + 1..])), None => (body, None) };
    let (ip, fp) = match mant.iter().position(|c| *c == b'.') { Some(i) => (&mant[..i], Some(&mant[i + 1..])), None => (mant, None) };
    let exp_ok = exp.map_or(true, |e| { let e = if matches!(e.first(), Some(b'-') | Some(b'+')) { &e[1..] } else { e }; digits(e) });
    let shape = (digits(ip) || ip.is_empty()) && fp.map_or(false, |f| digits(f) || f.is_empty()) && !(ip.is_empty() && fp.map_or(true, |f| f.is_empty()));
    if shape && exp_ok { if let Ok(f) = s.parse::<f64>() { if let Some(n) = Number::from_f64(f) { return Value::Number(n); } } }
    if digits(ip) && fp.is_none() && exp.is_some() && exp_ok { if let Ok(f) = s.parse::<f64>() { if let Some(n) = Number::from_f64(f) { return Value::Number(n); } } }
    Value::String(s.to_string())
}
impl MarkedEventReceiver for Rx {
    fn on_event(&mut self, ev: Event, mark: Marker) {
        if self.err.is_some() { return; } let line = mark.line();
        match ev {
            Event::DocumentStart => { self.docs += 1; if self.docs > 1 { self.fail(line, "only one YAML document is allowed"); } }
            Event::Alias(_) => self.fail(line, "aliases are not allowed (write the value out)"),
            Event::Scalar(text, style, anchor, tag) => {
                if anchor > 0 { return self.fail(line, "anchors are not allowed"); } if tag.is_some() { return self.fail(line, "tags are not allowed"); }
                if let Some(Frame::Map(m, key)) = self.stack.last_mut() { if key.is_none() { if m.contains_key(&text) { return self.fail(line, &format!("the key `{text}` appears twice")); } if text == "<<" { return self.fail(line, "merge keys are not allowed"); } *key = Some(text); return; } }
                let v = if style == TScalarStyle::Plain { plain_value(&text) } else { Value::String(text) }; self.put(v, line);
            }
            Event::SequenceStart(..) | Event::MappingStart(..) if self.stack.len() >= MAX_DEPTH => self.fail(line, &format!("nested deeper than {MAX_DEPTH} levels")),
            Event::SequenceStart(a, t) => { if a > 0 { return self.fail(line, "anchors are not allowed"); } if t.is_some() { return self.fail(line, "tags are not allowed"); } if matches!(self.stack.last(), Some(Frame::Map(_, None))) { return self.fail(line, "a key must be plain text"); } self.stack.push(Frame::Seq(vec![])); }
            Event::MappingStart(a, t) => { if a > 0 { return self.fail(line, "anchors are not allowed"); } if t.is_some() { return self.fail(line, "tags are not allowed"); } if matches!(self.stack.last(), Some(Frame::Map(_, None))) { return self.fail(line, "a key must be plain text"); } self.stack.push(Frame::Map(Map::new(), None)); }
            Event::SequenceEnd => { if let Some(Frame::Seq(a)) = self.stack.pop() { self.put(Value::Array(a), line); } }
            Event::MappingEnd => { if let Some(Frame::Map(m, _)) = self.stack.pop() { self.put(Value::Object(m), line); } }
            _ => {}
        }
    }
}
/// A file larger than this, or nested deeper than `MAX_DEPTH`, is refused before it can exhaust memory or the stack.
pub const MAX_BYTES: usize = 2_000_000; pub const MAX_DEPTH: usize = 64;
/// Read a YAML text as a value, under the strict rules above. Pure.
pub fn parse_strict(text: &str) -> Result<Value, String> {
    if text.len() > MAX_BYTES { return Err(format!("the YAML is larger than {} bytes", MAX_BYTES)); }
    let mut rx = Rx { stack: vec![], root: None, err: None, docs: 0 };
    Parser::new_from_str(text).load(&mut rx, true).map_err(|e| format!("not valid YAML: {e}"))?;
    if let Some(e) = rx.err { return Err(e); } rx.root.ok_or_else(|| "the YAML is empty".to_string())
}

// ── the writer ───────────────────────────────────────────────────────────────────────────────────────────────────────────────────
fn plain_safe(s: &str) -> bool {
    let mut it = s.chars(); let Some(f) = it.next() else { return false };
    if !(f.is_ascii_alphabetic() || f == '_') || s.ends_with(' ') || s.contains("  ") { return false; }
    if !s.chars().all(|c| c.is_ascii_alphanumeric() || "_./-: ".contains(c)) || s.ends_with(':') || s.contains(": ") { return false; }   // a colon inside a word is plain text in YAML; a colon before a space (or at the end) would be a key
    !matches!(s.to_ascii_lowercase().as_str(), "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "y" | "n" | "inf" | "nan")
}
fn quoted(s: &str) -> String { serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into()) }
fn scalar(v: &Value) -> String {
    match v { Value::Null => "null".into(), Value::Bool(b) => b.to_string(), Value::Number(n) => n.to_string(), Value::String(s) => if plain_safe(s) { s.clone() } else { quoted(s) }, _ => unreachable!() }
}
/// A multi-line string can be a block scalar when nothing in it needs an escape. Pure.
fn block_ok(s: &str) -> bool {
    s.contains('\n') && !s.contains('\r') && !s.contains('\t') && !s.ends_with("\n\n") && !s.starts_with(|c: char| c == ' ' || c == '\n') && s.chars().all(|c| c == '\n' || !c.is_control())
        && s.trim_end_matches('\n').lines().all(|l| !l.ends_with(' ')) && !s.trim_end_matches('\n').lines().next().map_or(true, |l| l.is_empty())
}
fn key_text(k: &str) -> String { if plain_safe(k) { k.to_string() } else { quoted(k) } }
fn emit_block(s: &str, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent); let body = s.strip_suffix('\n').unwrap_or(s);
    for l in body.split('\n') { if l.is_empty() { out.push('\n'); } else { out.push_str(&pad); out.push_str(l); out.push('\n'); } }
}
fn header_of(s: &str) -> &'static str { if s.ends_with('\n') { "|" } else { "|-" } }
fn emit_value(v: &Value, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    match v {
        Value::Object(m) => { let mut keys: Vec<&String> = m.keys().collect(); keys.sort();
            for k in keys { let x = &m[k]; out.push_str(&pad); out.push_str(&key_text(k)); out.push(':'); emit_after_key(x, indent, out); } }
        Value::Array(a) => { for x in a { out.push_str(&pad); out.push('-'); emit_after_dash(x, indent, out); } }
        other => { out.push_str(&pad); out.push_str(&scalar(other)); out.push('\n'); }
    }
}
fn emit_after_key(x: &Value, indent: usize, out: &mut String) {
    match x {
        Value::Object(m) if m.is_empty() => out.push_str(" {}\n"), Value::Array(a) if a.is_empty() => out.push_str(" []\n"),
        Value::Object(_) | Value::Array(_) => { out.push('\n'); emit_value(x, indent + 2, out); }
        Value::String(s) if block_ok(s) => { out.push(' '); out.push_str(header_of(s)); out.push('\n'); emit_block(s, indent + 2, out); }
        other => { out.push(' '); out.push_str(&scalar(other)); out.push('\n'); }
    }
}
fn emit_after_dash(x: &Value, indent: usize, out: &mut String) {
    match x {
        Value::Object(m) if m.is_empty() => out.push_str(" {}\n"), Value::Array(a) if a.is_empty() => out.push_str(" []\n"),
        Value::Object(_) => { let mut tmp = String::new(); emit_value(x, indent + 2, &mut tmp); out.push(' '); out.push_str(tmp.trim_start_matches(' ')); }
        Value::Array(_) => { out.push('\n'); emit_value(x, indent + 2, out); }
        Value::String(s) if block_ok(s) => { out.push(' '); out.push_str(header_of(s)); out.push('\n'); emit_block(s, indent + 2, out); }
        other => { out.push(' '); out.push_str(&scalar(other)); out.push('\n'); }
    }
}
/// A value as YAML text in the one stable layout (sorted keys, two-space indent, block scalars for multi-line text). Pure.
pub fn to_yaml(v: &Value) -> String { let mut out = String::new(); emit_value(v, 0, &mut out); out }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_strict_reader_refuses_what_makes_yaml_dangerous() {
        for (bad, why) in [("a: &x 1\nb: *x\n", "anchors"), ("a: !!str 1\n", "tags"), ("a: 1\na: 2\n", "twice"), ("<<: {a: 1}\n", "merge"), ("a: 1\n---\nb: 2\n", "one YAML document"), ("? [a, b]\n: 1\n", "plain text"), ("", "empty")] {
            let e = parse_strict(bad).unwrap_err(); assert!(e.contains(why), "{bad:?} -> {e}");
        }
        assert!(parse_strict("a: [1, 2\n").unwrap_err().contains("not valid YAML"));
    }
    #[test]
    fn plain_scalars_follow_the_yaml_1_2_core_schema_and_quotes_always_mean_text() {
        let v = parse_strict("a: no\nb: on\nc: yes\nd: 1.10\ne: \"1.10\"\nf: '5'\ng: 007\nh: -3\ni: 2e3\nj: .inf\nk: ~\nl: true\nm: 'true'\nn: 0x1F\no:\n").unwrap();
        assert_eq!((v["a"].clone(), v["b"].clone(), v["c"].clone()), (json!("no"), json!("on"), json!("yes")), "1.2: these are text, not booleans");
        assert_eq!(v["d"], json!(1.1)); assert_eq!(v["e"], json!("1.10")); assert_eq!(v["f"], json!("5")); assert_eq!(v["g"], json!(7)); assert_eq!(v["h"], json!(-3)); assert_eq!(v["i"], json!(2000.0));
        assert_eq!(v["j"], json!(".inf")); assert_eq!(v["k"], Value::Null); assert_eq!(v["l"], json!(true)); assert_eq!(v["m"], json!("true")); assert_eq!(v["n"], json!("0x1F")); assert_eq!(v["o"], Value::Null);
    }
    fn round(v: &Value) { let y = to_yaml(v); let back = parse_strict(&y).unwrap_or_else(|e| panic!("{e}\n---\n{y}")); assert_eq!(&back, v, "round trip\n{y}"); assert_eq!(to_yaml(&back), y, "the writer is deterministic"); }
    #[test]
    fn the_writer_round_trips_hard_values_in_one_stable_layout() {
        round(&json!({ "name": "sound branch", "version": "0.1.0", "n": 5, "f": 0.5, "g": 1.0, "ok": true, "none": null, "empty_map": {}, "empty_list": [] }));
        round(&json!({ "tricky": ["no", "on", "yes", "true", "null", "1.10", "5", "007", "a: b", "# not a comment", "- dash", "it's", "say \"hi\"", "tab\there", "trailing ", " leading", "", "é ü 日本", "back\\slash"] }));
        round(&json!({ "code": "import { test } from 'node:test';\ntest('a', () => {\n  assert.equal(f(1), 2);\n});\n", "no_newline": "line one\nline two", "blank_lines": "a\n\nb\n", "two_newlines": "a\n\n", "starts_space": " a\nb\n", "trailing_space": "a \nb\n", "cr": "a\r\nb" }));
        round(&json!({ "nodes": { "build": { "node": "use_tool", "tool": { "type": "skill", "name": "run-build", "args": {} }, "allow": { "effects": ["read", "write", "exec"], "paths": [] } } }, "edges": [{ "from": "a", "port": "out", "to": "b" }, { "from": "b", "port": "then", "to": "c" }], "nested": [[1, 2], [3, [4, 5]], [], {}] }));
        round(&json!([{ "a": 1 }, "x", 2, { "b": { "c": [true, null] } }]));
        let y = to_yaml(&json!({ "z": 1, "a": { "m": 2, "b": 3 } })); assert_eq!(y, "a:\n  b: 3\n  m: 2\nz: 1\n", "keys are sorted");
        assert_eq!(to_yaml(&json!({ "ref": "values:rules.output.subtitle", "h": "sha256:ab12" })), "h: sha256:ab12\nref: values:rules.output.subtitle\n", "a colon inside a word stays plain");
        round(&json!({ "a": ["x:", "x: y", "x:y", ":x", "a::b", "http://x.dev/a:b"] }));
    }

    #[test]
    fn hostile_size_and_depth_are_refused() {
        let deep = format!("{}1{}", "[".repeat(200), "]".repeat(200)); assert!(parse_strict(&format!("a: {deep}\n")).unwrap_err().contains("nested deeper"));
        let ok = format!("a: {}1{}\n", "[".repeat(30), "]".repeat(30)); assert!(parse_strict(&ok).is_ok());
        let big = format!("a: \"{}\"\n", "x".repeat(MAX_BYTES + 1)); assert!(parse_strict(&big).unwrap_err().contains("larger than"));
    }
}
