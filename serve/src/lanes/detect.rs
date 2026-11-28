//! THE DETECTORS (RFC 0018): pure functions from FACTS and a law's config to hits. None calls a model, the clock, the network or anything random; every regex is the linear-time `regex` crate under a size limit; the one small language (`expr`) runs under hard operation limits with loops switched off.
//! A hit names where it is (a line, the unit's path) and what was found against what was expected, so the agent that must fix it is handed a place and a number, not a lecture.
use super::readers::{words_of, Facts};
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub struct Hit { pub line: usize, pub unit: String, pub message: String, pub value: Value, pub expected: Value, pub excerpt: String }
pub struct Ctx<'a> { /** every artifact path of this judging (links and cross-checks resolve against it) */ pub files: &'a BTreeSet<String>, /** the facts of every artifact, in path order (similarity looks across them) */ pub all: &'a [Facts] }
pub const DETECTORS: [&str; 8] = ["pattern", "count", "structure", "schema", "link", "similarity", "expr", "cross"];

fn s(v: &Value) -> String { v.as_str().unwrap_or("").to_string() }
fn strs(v: &Value) -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() }
fn short(t: &str, n: usize) -> String { let one: String = t.split_whitespace().collect::<Vec<_>>().join(" "); one.chars().take(n).collect() }
fn hit(line: usize, unit: &str, message: String, value: Value, expected: Value, excerpt: &str) -> Hit { Hit { line, unit: unit.to_string(), message, value, expected, excerpt: short(excerpt, 140) } }

pub fn regex_of(pat: &str, ci: bool) -> Result<regex::Regex, String> { regex::RegexBuilder::new(pat).case_insensitive(ci).size_limit(1 << 20).dfa_size_limit(1 << 20).build().map_err(|e| format!("the pattern does not compile: {e}")) }
/// The regex of a pattern law: its `regex`, or its lexicon (`any`) escaped and joined, with word edges unless `whole_word` is false. Pure.
pub fn pattern_regex(c: &Value) -> Result<regex::Regex, String> {
    let ci = c.get("ignore_case").and_then(Value::as_bool).unwrap_or(true);
    let raw = s(&c["regex"]); let any = strs(&c["any"]);
    if !raw.is_empty() { return regex_of(&raw, ci); }
    if any.is_empty() { return Err("a pattern law needs `regex` or `any`".into()); }
    let ww = c.get("whole_word").and_then(Value::as_bool).unwrap_or(true);
    let alts: Vec<String> = any.iter().filter(|a| !a.trim().is_empty()).map(|a| regex::escape(a.trim())).collect(); if alts.is_empty() { return Err("`any` is empty".into()); }
    regex_of(&if ww { format!(r"\b(?:{})\b", alts.join("|")) } else { alts.join("|") }, ci)
}
fn offset_line(text: &str, off: usize) -> usize { text[..off.min(text.len())].matches('\n').count() + 1 }

pub fn run(detector: &str, c: &Value, f: &Facts, ctx: &Ctx) -> Result<Vec<Hit>, String> {
    match detector { "pattern" => pattern(c, f), "count" => count(c, f), "structure" => structure(c, f), "schema" => schema(c, f), "link" => link(c, f, ctx), "similarity" => similarity(c, f, ctx), "expr" => expr(c, f), "cross" => cross(c, f, ctx), d => Err(format!("`{d}` is not a detector")) }
}

fn pattern(c: &Value, f: &Facts) -> Result<Vec<Hit>, String> {
    let re = pattern_regex(c)?; let unit = { let u = s(&c["unit"]); if u.is_empty() { "file".to_string() } else { u } }; let require = s(&c["mode"]) == "require"; let msg = s(&c["message"]);
    let mut hits = vec![]; let mut found = false;
    if unit == "file" || unit == "line" {
        let Some(t) = &f.text else { return Ok(vec![]) };
        if unit == "file" { for m in re.find_iter(t) { found = true; if !require { hits.push(hit(offset_line(t, m.start()), "", if msg.is_empty() { format!("found `{}`", short(m.as_str(), 60)) } else { msg.clone() }, json!(m.as_str()), json!("absent"), &t[t[..m.start()].rfind('\n').map_or(0, |i| i + 1)..t[m.start()..].find('\n').map_or(t.len(), |i| m.start() + i)])); if hits.len() >= 200 { break; } } } }
        else { for (i, l) in t.lines().enumerate() { if let Some(m) = re.find(l) { found = true; if !require { hits.push(hit(i + 1, "", if msg.is_empty() { format!("found `{}`", short(m.as_str(), 60)) } else { msg.clone() }, json!(m.as_str()), json!("absent"), l)); if hits.len() >= 200 { break; } } } } }
    } else {
        for u in f.units.iter().filter(|u| u.kind == unit) { if let Some(m) = re.find(&u.text) { found = true; if !require { hits.push(hit(u.line, &u.path, if msg.is_empty() { format!("found `{}`", short(m.as_str(), 60)) } else { msg.clone() }, json!(m.as_str()), json!("absent"), &u.text)); if hits.len() >= 200 { break; } } } }
    }
    if require && !found && (f.text.is_some() || !f.units.is_empty()) { hits.push(hit(1, "", if msg.is_empty() { "the required text is missing".into() } else { msg }, json!("absent"), json!("present"), "")); }
    Ok(hits)
}
fn attr_num(a: &std::collections::BTreeMap<String, Value>, k: &str) -> Option<f64> { a.get(k).and_then(Value::as_f64) }
fn count(c: &Value, f: &Facts) -> Result<Vec<Hit>, String> {
    let attr = s(&c["attr"]); if attr.is_empty() { return Err("a count law needs `attr`".into()); } let unit = { let u = s(&c["unit"]); if u.is_empty() { "file".to_string() } else { u } };
    let (min, max) = (c.get("min").and_then(Value::as_f64), c.get("max").and_then(Value::as_f64)); if min.is_none() && max.is_none() { return Err("a count law needs `min` or `max`".into()); }
    let mut hits = vec![]; let check = |v: f64, line: usize, path: &str, ex: &str, hits: &mut Vec<Hit>| { if let Some(m) = max { if v > m { hits.push(hit(line, path, format!("{attr} is {v}, expected at most {m}"), json!(v), json!({ "max": m }), ex)); } } if let Some(m) = min { if v < m { hits.push(hit(line, path, format!("{attr} is {v}, expected at least {m}"), json!(v), json!({ "min": m }), ex)); } } };
    if unit == "file" { match attr_num(&f.attrs, &attr) { Some(v) => check(v, 1, "", "", &mut hits), None => { if f.unread.is_none() { return Ok(vec![]); } } } }
    else { for u in f.units.iter().filter(|u| u.kind == unit) { if let Some(v) = attr_num(&u.attrs, &attr) { check(v, u.line, &u.path, &u.text, &mut hits); if hits.len() >= 200 { break; } } } }
    Ok(hits)
}
fn slug(t: &str) -> String { let mut o = String::new(); let mut dash = false; for c in t.to_lowercase().chars() { if c.is_alphanumeric() { o.push(c); dash = false; } else if !dash && !o.is_empty() { o.push('-'); dash = true; } } o.trim_matches('-').to_string() }
fn structure(c: &Value, f: &Facts) -> Result<Vec<Hit>, String> {
    let check = s(&c["check"]); let heads: Vec<&super::readers::Unit> = f.units.iter().filter(|u| u.kind == "heading").collect(); let mut hits = vec![];
    match check.as_str() {
        "parses" => { if f.attrs.get("valid") == Some(&json!(false)) { hits.push(hit(1, "", format!("the file does not parse: {}", s(f.attrs.get("parse_error").unwrap_or(&Value::Null))), json!("invalid"), json!("valid"), "")); } }
        "heading_levels" => { let mut prev = 0u64; for h in &heads { let l = h.attrs["level"].as_u64().unwrap_or(1); if prev > 0 && l > prev + 1 { hits.push(hit(h.line, &h.path, format!("heading jumps from level {prev} to {l}"), json!(l), json!({ "max": prev + 1 }), &h.text)); } prev = l; } }
        "single_h1" => { let n = heads.iter().filter(|h| h.attrs["level"] == json!(1)).count(); if n != 1 && (!heads.is_empty() || f.kind == "html") { hits.push(hit(heads.iter().find(|h| h.attrs["level"] == json!(1)).map_or(1, |h| h.line), "", format!("{n} top-level headings, expected exactly 1"), json!(n), json!(1), "")); } }
        "no_empty_sections" => { for h in &heads { if h.attrs.get("empty") == Some(&json!(true)) { hits.push(hit(h.line, &h.path, "a heading with nothing under it".into(), json!("empty"), json!("content"), &h.text)); } } }
        "unique_headings" => { let mut seen: BTreeSet<String> = BTreeSet::new(); for h in &heads { let k = format!("{}|{}", h.attrs["level"], h.text.to_lowercase()); if !seen.insert(k) { hits.push(hit(h.line, &h.path, "this heading repeats an earlier one".into(), json!(h.text), json!("unique"), &h.text)); } } }
        "required_sections" => { let ci = c.get("ignore_case").and_then(Value::as_bool).unwrap_or(true); let have: BTreeSet<String> = heads.iter().map(|h| if ci { h.text.to_lowercase() } else { h.text.clone() }).collect(); for n in strs(&c["names"]) { let k = if ci { n.to_lowercase() } else { n.clone() }; if !have.contains(&k) { hits.push(hit(1, "", format!("the section `{n}` is missing"), json!("absent"), json!(n), "")); } } }
        "section_order" => { let ci = true; let want: Vec<String> = strs(&c["names"]).iter().map(|n| if ci { n.to_lowercase() } else { n.clone() }).collect(); let mut last = -1i64; for h in &heads { if let Some(p) = want.iter().position(|w| *w == h.text.to_lowercase()) { if (p as i64) < last { hits.push(hit(h.line, &h.path, format!("`{}` comes after a section that should follow it", h.text), json!(h.text), json!(want.clone()), &h.text)); } last = last.max(p as i64); } } }
        "alt_text" => { for u in f.units.iter().filter(|u| u.kind == "image") { if u.attrs.get("has_alt") == Some(&json!(false)) { hits.push(hit(u.line, &u.path, "an image with no alt text".into(), json!("missing"), json!("alt"), &s(&u.attrs["src"]))); } } }
        "csv_rectangular" => { for u in f.units.iter().filter(|u| u.kind == "row") { if u.attrs.get("ragged") == Some(&json!(true)) { hits.push(hit(u.line, &u.path, format!("{} cells, the header has {}", u.attrs["cells"], f.attrs.get("cols").unwrap_or(&json!(0))), u.attrs["cells"].clone(), f.attrs.get("cols").cloned().unwrap_or(json!(0)), "")); } } }
        "no_duplicate_rows" => { let n = f.attrs.get("duplicate_rows").and_then(Value::as_u64).unwrap_or(0); if n > 0 { hits.push(hit(1, "", format!("{n} duplicate row(s)"), json!(n), json!(0), "")); } }
        "unique_header" => { let n = f.attrs.get("duplicate_headers").and_then(Value::as_u64).unwrap_or(0); if n > 0 { hits.push(hit(1, "", format!("{n} column name(s) repeat"), json!(n), json!(0), "")); } }
        "fence_language" => { for u in f.units.iter().filter(|u| u.kind == "fence") { if u.attrs.get("has_lang") == Some(&json!(false)) { hits.push(hit(u.line, "", "a code block with no language".into(), json!("none"), json!("language"), "")); } } }
        other => return Err(format!("`{other}` is not a structure check (parses, heading_levels, single_h1, no_empty_sections, unique_headings, required_sections, section_order, alt_text, csv_rectangular, no_duplicate_rows, unique_header, fence_language)")),
    }
    Ok(hits)
}
/// Every value of a path in a document: `a.b` follows keys, `tasks[]` fans out over an array, `tasks[0]` takes one; each comes with its concrete path. Pure.
pub fn resolve(doc: &Value, path: &str) -> Vec<(String, Value)> {
    let mut cur: Vec<(String, Value)> = vec![(String::new(), doc.clone())];
    for seg in path.split('.').filter(|p| !p.is_empty()) {
        let (key, idx) = match seg.find('[') { Some(i) => (&seg[..i], Some(&seg[i..])), None => (seg, None) }; let mut next = vec![];
        for (p, v) in cur { let base = if key.is_empty() { Some(v.clone()) } else { v.get(key).cloned() }; let Some(b) = base else { continue }; let bp = if key.is_empty() { p.clone() } else if p.is_empty() { key.to_string() } else { format!("{p}.{key}") };
            match idx { None => next.push((bp, b)), Some("[]") => { if let Some(a) = b.as_array() { for (i, x) in a.iter().enumerate() { next.push((format!("{bp}[{i}]"), x.clone())); } } } Some(i) => { if let Some(n) = i.trim_matches(|c| c == '[' || c == ']').parse::<usize>().ok() { if let Some(x) = b.get(n) { next.push((format!("{bp}[{n}]"), x.clone())); } } } } }
        cur = next;
    }
    cur
}
fn type_of(v: &Value) -> &'static str { match v { Value::Null => "null", Value::Bool(_) => "bool", Value::Number(_) => "number", Value::String(_) => "string", Value::Array(_) => "array", Value::Object(_) => "object" } }
fn schema(c: &Value, f: &Facts) -> Result<Vec<Hit>, String> {
    let Some(doc) = &f.doc else { return Ok(vec![]) }; let mut hits = vec![];
    let at = |p: &str| doc_line(f, p);
    for p in strs(&c["required"]) { let r = resolve(doc, &p); if r.is_empty() || r.iter().all(|(_, v)| v.is_null() || v.as_str().map_or(false, |x| x.trim().is_empty())) { hits.push(hit(at(&p), &p, format!("`{p}` is missing or empty"), json!("absent"), json!("present"), "")); } }
    // `required_each`: for every element of an array path, these keys must be present and non-empty
    if let Some(o) = c.get("required_each").and_then(Value::as_object) { for (arr, keys) in o { for (ip, item) in resolve(doc, &format!("{arr}[]")) { for k in strs(keys) { let ok = item.get(&k).map_or(false, |v| !v.is_null() && !v.as_str().map_or(false, |x| x.trim().is_empty())); if !ok { hits.push(hit(at(&ip), &ip, format!("`{ip}` has no `{k}`"), json!("absent"), json!(k), "")); } } } } }
    for p in strs(&c["forbidden"]) { for (cp, _) in resolve(doc, &p) { hits.push(hit(at(&cp), &cp, format!("`{cp}` must not be present"), json!("present"), json!("absent"), "")); } }
    if let Some(o) = c.get("types").and_then(Value::as_object) { for (p, t) in o { for (cp, v) in resolve(doc, p) { if type_of(&v) != s(t) { hits.push(hit(at(&cp), &cp, format!("`{cp}` is a {}, expected a {}", type_of(&v), s(t)), json!(type_of(&v)), t.clone(), "")); } } } }
    if let Some(o) = c.get("enums").and_then(Value::as_object) { for (p, allowed) in o { let al = strs(allowed); for (cp, v) in resolve(doc, p) { let sv = v.as_str().map(String::from).unwrap_or_else(|| v.to_string()); if !al.contains(&sv) { hits.push(hit(at(&cp), &cp, format!("`{cp}` is `{}`, expected one of {}", short(&sv, 40), al.join(", ")), json!(sv), json!(al.clone()), "")); } } } }
    if let Some(o) = c.get("min_items").and_then(Value::as_object) { for (p, n) in o { let need = n.as_u64().unwrap_or(1) as usize; for (cp, v) in resolve(doc, p) { let l = v.as_array().map_or(0, |a| a.len()); if l < need { hits.push(hit(at(&cp), &cp, format!("`{cp}` has {l} item(s), expected at least {need}"), json!(l), json!({ "min": need }), "")); } } } }
    for p in strs(&c["unique"]) { let mut seen: BTreeSet<String> = BTreeSet::new(); for (cp, v) in resolve(doc, &p) { let k = v.to_string(); if !seen.insert(k.clone()) { hits.push(hit(at(&cp), &cp, format!("`{cp}` repeats the value {}", short(&k, 40)), json!(k), json!("unique"), "")); } } }
    if let Some(a) = c.get("refs").and_then(Value::as_array) { for r in a { let (from, to) = (s(&r["from"]), s(&r["to"])); let targets: BTreeSet<String> = resolve(doc, &to).into_iter().map(|(_, v)| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).collect(); for (cp, v) in resolve(doc, &from) { if v.is_null() { continue; } let sv = v.as_str().map(String::from).unwrap_or_else(|| v.to_string()); if !targets.contains(&sv) { hits.push(hit(at(&cp), &cp, format!("`{cp}` points at `{}`, which is not in `{to}`", short(&sv, 40)), json!(sv), json!(to), "")); } } } }
    Ok(hits)
}
fn doc_line(f: &Facts, path: &str) -> usize { let Some(t) = &f.text else { return 1 }; let last = path.rsplit(['.', ']']).find(|x| !x.is_empty() && x.parse::<usize>().is_err() && !x.starts_with('[')).unwrap_or(path); t.lines().position(|l| l.contains(last)).map_or(1, |i| i + 1) }
fn norm_path(base: &str, target: &str) -> Option<String> {
    let t = target.split('#').next().unwrap_or("").split('?').next().unwrap_or(""); if t.is_empty() { return None; }
    let mut parts: Vec<&str> = if t.starts_with('/') { vec![] } else { base.rsplit_once('/').map(|(d, _)| d.split('/').collect()).unwrap_or_default() };
    for p in t.trim_start_matches('/').split('/') { match p { "" | "." => {} ".." => { parts.pop()?; } x => parts.push(x) } }
    Some(parts.join("/"))
}
fn link(c: &Value, f: &Facts, ctx: &Ctx) -> Result<Vec<Hit>, String> {
    let check_img = c.get("images").and_then(Value::as_bool).unwrap_or(true); let mut hits = vec![]; let slugs: BTreeSet<String> = f.units.iter().filter(|u| u.kind == "heading").map(|h| slug(&h.text)).collect();
    for u in f.units.iter().filter(|u| u.kind == "link" || (check_img && u.kind == "image")) {
        let target = s(if u.kind == "link" { &u.attrs["target"] } else { &u.attrs["src"] }); if target.is_empty() || u.attrs.get("external") == Some(&json!(true)) || target.starts_with("data:") || target.starts_with("mailto:") || target.starts_with("tel:") { continue; }
        if let Some(a) = target.strip_prefix('#') { if !a.is_empty() && !slugs.contains(&slug(a)) && f.kind != "html" { hits.push(hit(u.line, &u.path, format!("the anchor `#{a}` matches no heading"), json!(a), json!("a heading"), &u.text)); } continue; }
        if let Some(p) = norm_path(&f.path, &target) { if !ctx.files.contains(&p) && !ctx.files.contains(&format!("{p}/index.md")) && !ctx.files.contains(&format!("{p}/README.md")) && !ctx.files.contains(&format!("{p}/index.html")) { hits.push(hit(u.line, &u.path, format!("`{target}` points at a file that is not there"), json!(target), json!("an existing file"), &u.text)); } }
    }
    Ok(hits)
}
// ── near-duplicates: a 64-bit simhash of the words of each paragraph, FNV-1a so it is the same everywhere ──
fn fnv(s: &str) -> u64 { let mut h = 0xcbf29ce484222325u64; for b in s.as_bytes() { h ^= *b as u64; h = h.wrapping_mul(0x100000001b3); } h }
pub fn simhash(text: &str) -> u64 { let ws: Vec<String> = words_of(text).iter().map(|w| w.to_lowercase()).collect(); let mut v = [0i32; 64]; for w in &ws { let h = fnv(w); for (i, slot) in v.iter_mut().enumerate() { if h >> i & 1 == 1 { *slot += 1 } else { *slot -= 1 } } } let mut o = 0u64; for (i, x) in v.iter().enumerate() { if *x > 0 { o |= 1 << i; } } o }
fn similarity(c: &Value, f: &Facts, ctx: &Ctx) -> Result<Vec<Hit>, String> {
    let min_words = c.get("min_words").and_then(Value::as_u64).unwrap_or(8) as usize; let max_d = c.get("max_distance").and_then(Value::as_u64).unwrap_or(3) as u32; let across = c.get("across").and_then(Value::as_bool).unwrap_or(true);
    let units = |x: &Facts| -> Vec<(usize, String, u64)> { x.units.iter().filter(|u| (u.kind == "paragraph" || u.kind == "item") && u.attrs.get("words").and_then(Value::as_u64).unwrap_or(0) as usize >= min_words).map(|u| (u.line, short(&u.text, 100), simhash(&u.text))).collect() };
    let mine = units(f); let mut hits = vec![]; let mut earlier: Vec<(String, usize, u64)> = vec![]; let mut budget = 4000usize;
    if across { for o in ctx.all.iter().filter(|o| o.path < f.path) { for (l, _, h) in units(o) { if budget == 0 { break; } budget -= 1; earlier.push((o.path.clone(), l, h)); } } }
    for (i, (line, ex, h)) in mine.iter().enumerate() {
        for (j, (_, _, h2)) in mine.iter().enumerate().take(i) { if (h ^ h2).count_ones() <= max_d { hits.push(hit(*line, "", format!("repeats an earlier paragraph of this file (distance {})", (h ^ h2).count_ones()), json!((h ^ h2).count_ones()), json!({ "max": max_d }), ex)); let _ = j; break; } }
        for (p, l, h2) in &earlier { if (h ^ h2).count_ones() <= max_d { hits.push(hit(*line, "", format!("near-duplicate of {p}:{l} (distance {})", (h ^ h2).count_ones()), json!((h ^ h2).count_ones()), json!({ "max": max_d }), ex)); break; } }
        if hits.len() >= 100 { break; }
    }
    Ok(hits)
}
// ── expr: a small language, bounded ─────────────────────────────────────────────────────────
pub fn engine() -> rhai::Engine {
    let mut e = rhai::Engine::new(); e.set_max_operations(5000); e.set_max_expr_depths(32, 16); e.set_max_string_size(20_000); e.set_max_array_size(1000); e.set_max_map_size(200); e.set_max_call_levels(8);
    for k in ["while", "loop", "for", "do", "until", "import", "export", "eval", "fn", "private", "throw", "try", "catch"] { e.disable_symbol(k); }
    e
}
/// Does the expression compile (and is it one expression)? Pure.
pub fn expr_compiles(src: &str) -> Result<(), String> { if src.trim().is_empty() { return Err("an expr law needs `expr`".into()); } engine().compile_expression(src).map(|_| ()).map_err(|e| format!("the expression does not compile: {e}")) }
fn scope_of(attrs: &std::collections::BTreeMap<String, Value>, text: &str, line: usize, kind: &str) -> rhai::Scope<'static> {
    let mut sc = rhai::Scope::new();
    for (k, v) in attrs { let name: String = k.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect(); match v { Value::Bool(b) => { sc.push(name, *b); } Value::Number(n) => { if let Some(i) = n.as_i64() { sc.push(name, i); } else if let Some(x) = n.as_f64() { sc.push(name, x); } } Value::String(s) => { sc.push(name, s.chars().take(2000).collect::<String>()); } _ => {} } }
    sc.push("text", text.chars().take(5000).collect::<String>()); sc.push("line", line as i64); sc.push("kind", kind.to_string()); sc
}
fn expr(c: &Value, f: &Facts) -> Result<Vec<Hit>, String> {
    let src = s(&c["expr"]); expr_compiles(&src)?; let e = engine(); let ast = e.compile_expression(&src).map_err(|e| e.to_string())?; let unit = { let u = s(&c["unit"]); if u.is_empty() { "file".to_string() } else { u } }; let mut hits = vec![];
    let msg = { let m = s(&c["message"]); if m.is_empty() { format!("`{src}` is true") } else { m } };
    let mut eval = |attrs: &std::collections::BTreeMap<String, Value>, text: &str, line: usize, path: &str, hits: &mut Vec<Hit>| -> Result<(), String> { let mut sc = scope_of(attrs, text, line, &unit); match e.eval_ast_with_scope::<bool>(&mut sc, &ast) { Ok(true) => hits.push(hit(line, path, msg.clone(), json!(true), json!(false), text)), Ok(false) => {} Err(err) => return Err(format!("the expression failed: {err}")) } Ok(()) };
    if unit == "file" { eval(&f.attrs, "", 1, "", &mut hits)?; } else { for u in f.units.iter().filter(|u| u.kind == unit).take(5000) { eval(&u.attrs, &u.text, u.line, &u.path, &mut hits)?; if hits.len() >= 200 { break; } } }
    Ok(hits)
}
// ── cross: a file here needs a file there ───────────────────────────────────────────────────
pub fn glob_match(pat: &str, path: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool { match p.first() { None => s.is_empty(), Some(b'*') if p.get(1) == Some(&b'*') => { let rest = if p.get(2) == Some(&b'/') { &p[3..] } else { &p[2..] }; (0..=s.len()).any(|i| go(rest, &s[i..])) } Some(b'*') => { let mut i = 0; loop { if go(&p[1..], &s[i..]) { return true; } if i >= s.len() || s[i] == b'/' { return false; } i += 1; } } Some(b'?') => !s.is_empty() && s[0] != b'/' && go(&p[1..], &s[1..]), Some(c) => s.first() == Some(c) && go(&p[1..], &s[1..]) } }
    let pat = pat.trim_start_matches("./"); if !pat.contains('/') && !pat.starts_with("**") { return go(format!("**/{pat}").as_bytes(), path.as_bytes()) || go(pat.as_bytes(), path.as_bytes()); } go(pat.as_bytes(), path.as_bytes())
}
fn cross(c: &Value, f: &Facts, ctx: &Ctx) -> Result<Vec<Hit>, String> {
    let g = s(&c["for"]); let needs = strs(&c["needs"]); if g.is_empty() || needs.is_empty() { return Err("a cross law needs `for` (a glob) and `needs` (paths with {dir} {stem} {name})".into()); }
    if !glob_match(&g, &f.path) { return Ok(vec![]); }
    let (dir, name) = f.path.rsplit_once('/').map(|(d, n)| (d.to_string(), n.to_string())).unwrap_or((String::new(), f.path.clone())); let stem = name.rsplit_once('.').map(|(a, _)| a.to_string()).unwrap_or(name.clone());
    let mut hits = vec![];
    for n in needs { let p = n.replace("{dir}", &dir).replace("{stem}", &stem).replace("{name}", &name); let p = p.trim_start_matches('/').to_string(); if !ctx.files.contains(&p) { hits.push(hit(1, "", format!("{} needs `{p}`, which is not there", f.path), json!("absent"), json!(p), "")); } }
    Ok(hits)
}
