//! DOCUMENTATION GENERATED FROM THE FILE ITSELF (2026-11-21, the Editor view's "docs" icon): what a file is and what is in it, for ANY kind of file, read by the same deterministic readers the book judges with: no model, the same text for the same bytes.
//! X-ray's docs site documents the symbols of an indexed code base and says "no documentation yet" for a page, a report, a table or a one-file project; this one always has something true to say, and says only what it can point at.
use super::readers::{self, Facts};
use serde_json::{json, Value};

fn row(k: &str, v: Value) -> Value { json!({ "k": k, "v": v }) }
fn sec(title: &str, items: Vec<Value>) -> Value { json!({ "title": title, "items": items }) }
fn at(f: &Facts, k: &str) -> Value { f.attrs.get(k).cloned().unwrap_or(Value::Null) }

fn code_symbols(text: &str, ext: &str) -> (Vec<Value>, Vec<String>) {
    let (sym, imp): (Vec<&str>, Vec<&str>) = match ext {
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => (vec![r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)", r"^\s*(?:export\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)", r"^\s*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=]+)?=\s*(?:async\s*)?(?:\([^)]*\)|[A-Za-z_$][\w$]*)\s*(?::[^=]+)?=>", r"^\s*(?:export\s+)?(?:interface|type|enum)\s+([A-Za-z_$][\w$]*)", r"^\s*(?:module\.)?exports\.([A-Za-z_$][\w$]*)\s*="], vec![r#"^\s*import\s+(?:[^'"]*from\s+)?['"]([^'"]+)['"]"#, r#"require\(\s*['"]([^'"]+)['"]\s*\)"#]),
        "py" => (vec![r"^\s*(?:async\s+)?def\s+([A-Za-z_]\w*)", r"^\s*class\s+([A-Za-z_]\w*)"], vec![r"^\s*import\s+([\w.]+)", r"^\s*from\s+([\w.]+)\s+import"]),
        "rs" => (vec![r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_]\w*)", r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|trait|type)\s+([A-Za-z_]\w*)", r"^\s*impl(?:<[^>]*>)?\s+(?:[\w:<>]+\s+for\s+)?([A-Za-z_]\w*)"], vec![r"^\s*use\s+([\w:]+)"]),
        "go" => (vec![r"^func\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)", r"^type\s+([A-Za-z_]\w*)"], vec![r#"^\s*(?:import\s+)?"([^"]+)"$"#]),
        _ => (vec![r"^\s*(?:public|private|protected|static|\s)*(?:class|interface|struct|enum)\s+([A-Za-z_]\w*)", r"^\s*(?:public|private|protected|static|\s)+[\w<>\[\]]+\s+([A-Za-z_]\w*)\s*\([^)]*\)\s*\{?\s*$"], vec![r"^\s*(?:import|using|#include)\s+[<\x22]?([\w./:]+)"]),
    };
    let sre: Vec<regex::Regex> = sym.iter().filter_map(|p| regex::Regex::new(p).ok()).collect(); let ire: Vec<regex::Regex> = imp.iter().filter_map(|p| regex::Regex::new(p).ok()).collect();
    let (mut syms, mut imps) = (vec![], vec![]);
    for (i, l) in text.lines().enumerate() {
        for r in &sre { if let Some(c) = r.captures(l) { let name = c[1].to_string(); if !syms.iter().any(|s: &Value| s["name"] == json!(name)) && syms.len() < 200 { let doc = text.lines().take(i).collect::<Vec<_>>().iter().rev().take_while(|p| { let t = p.trim_start(); t.starts_with("//") || t.starts_with("///") || t.starts_with('#') || t.starts_with("*") || t.starts_with("/*") || t.ends_with("*/") }).map(|p| p.trim().trim_start_matches(['/', '*', '#', ' ']).trim_end_matches("*/").trim().to_string()).collect::<Vec<_>>().into_iter().rev().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" "); syms.push(json!({ "name": name, "line": i + 1, "signature": l.trim().chars().take(140).collect::<String>(), "doc": doc.chars().take(240).collect::<String>(), "exported": l.contains("export") || l.contains("pub ") || l.contains("module.exports") || l.contains("exports.") })); } break; } }
        for r in &ire { if let Some(c) = r.captures(l) { let m = c[1].to_string(); if !imps.contains(&m) && imps.len() < 100 { imps.push(m); } } }
    }
    (syms, imps)
}
/// The documentation of one file as data: a title, a one-line summary and sections of key/value rows. Pure.
pub fn describe(path: &str, bytes: &[u8]) -> Value {
    let f = readers::read(path, bytes); let name = path.rsplit('/').next().unwrap_or(path); let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let mut secs: Vec<Value> = vec![]; let mut summary = format!("{} file, {} bytes", f.kind, f.bytes);
    secs.push(sec("The file", vec![row("path", json!(path)), row("kind", json!(f.kind)), row("bytes", json!(f.bytes)), row("lines", at(&f, "lines"))]));
    if let Some(why) = &f.unread { secs.push(sec("Could not read", vec![row("why", json!(why))])); return json!({ "ok": true, "path": path, "kind": f.kind, "title": name, "summary": why, "sections": secs }); }
    match f.kind.as_str() {
        "markdown" | "text" => {
            let heads: Vec<Value> = f.units.iter().filter(|u| u.kind == "heading").map(|h| json!({ "k": format!("{}{}", "  ".repeat(h.attrs["level"].as_u64().unwrap_or(1).saturating_sub(1) as usize), h.text), "v": format!("line {} · {} words", h.line, h.attrs.get("words").and_then(Value::as_u64).unwrap_or(0)) })).collect();
            let title = f.units.iter().find(|u| u.kind == "heading").map(|h| h.text.clone()); let first = f.units.iter().find(|u| u.kind == "paragraph").map(|p| p.text.chars().take(240).collect::<String>());
            summary = format!("{} words, about {} min to read, {} section(s)", at(&f, "words"), at(&f, "reading_minutes"), at(&f, "headings")); if let Some(p) = first { summary = format!("{p} — {summary}"); }
            secs.push(sec("Reading", vec![row("words", at(&f, "words")), row("sentences", at(&f, "sentences")), row("paragraphs", at(&f, "paragraphs")), row("average sentence", json!(format!("{} words", at(&f, "avg_sentence_words")))), row("reading time", json!(format!("{} min", at(&f, "reading_minutes"))))]));
            if !heads.is_empty() { secs.push(sec("Outline", heads)); }
            let links: Vec<Value> = f.units.iter().filter(|u| u.kind == "link").take(40).map(|u| row(&u.text, json!(u.attrs["target"].clone()))).collect(); if !links.is_empty() { secs.push(sec("Links", links)); }
            let imgs: Vec<Value> = f.units.iter().filter(|u| u.kind == "image").take(20).map(|u| row(if u.text.is_empty() { "(no alt text)" } else { &u.text }, u.attrs["src"].clone())).collect(); if !imgs.is_empty() { secs.push(sec("Images", imgs)); }
            let fm: Vec<Value> = f.attrs.iter().filter(|(k, _)| k.starts_with("fm.")).map(|(k, v)| row(&k[3..], v.clone())).collect(); if !fm.is_empty() { secs.push(sec("Front matter", fm)); }
            return json!({ "ok": true, "path": path, "kind": f.kind, "title": title.unwrap_or_else(|| name.to_string()), "summary": summary, "sections": secs });
        }
        "html" => {
            let heads: Vec<Value> = f.units.iter().filter(|u| u.kind == "heading").map(|h| row(&format!("{}{}", "  ".repeat(h.attrs["level"].as_u64().unwrap_or(1).saturating_sub(1) as usize), h.text), json!(format!("h{} · line {}", h.attrs["level"], h.line)))).collect();
            summary = format!("a page titled “{}”, {} words, {} image(s), {} link(s)", at(&f, "title").as_str().unwrap_or(""), at(&f, "words"), at(&f, "images"), at(&f, "links"));
            secs.push(sec("Page", vec![row("title", at(&f, "title")), row("language", json!(if f.attrs.get("has_lang") == Some(&json!(true)) { "declared" } else { "not declared" })), row("meta description", json!(format!("{} characters", at(&f, "meta_description_len")))), row("words", at(&f, "words"))]));
            if !heads.is_empty() { secs.push(sec("Outline", heads)); }
            let imgs: Vec<Value> = f.units.iter().filter(|u| u.kind == "image").take(30).map(|u| row(u.attrs["src"].as_str().unwrap_or(""), json!(if u.attrs["has_alt"] == json!(true) { u.text.clone() } else { "no alt text".to_string() }))).collect(); if !imgs.is_empty() { secs.push(sec("Images", imgs)); }
            let links: Vec<Value> = f.units.iter().filter(|u| u.kind == "link").take(40).map(|u| row(&u.text, u.attrs["target"].clone())).collect(); if !links.is_empty() { secs.push(sec("Links", links)); }
        }
        "json" | "yaml" | "toml" => {
            if f.attrs.get("valid") == Some(&json!(false)) { secs.push(sec("Problem", vec![row("does not parse", at(&f, "parse_error"))])); summary = "a document that does not parse".into(); }
            else { let keys: Vec<Value> = f.units.iter().filter(|u| u.kind == "value" && u.path.matches(['.', '[']).count() == 0).take(80).map(|u| row(&u.path, json!(match u.attrs["type"].as_str().unwrap_or("") { "array" => format!("array of {}", u.attrs["len"]), "object" => format!("object with {} key(s)", u.attrs["len"]), "string" => format!("“{}”", u.text.chars().take(60).collect::<String>()), t => format!("{t} {}", u.text) }))).collect(); summary = format!("{} top-level key(s), {} levels deep", at(&f, "top_keys"), at(&f, "depth")); secs.push(sec("Structure", keys)); }
        }
        "csv" => {
            let header: Vec<String> = f.attrs.get("header").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            let cols: Vec<Value> = header.iter().map(|h| { let cells: Vec<&readers::Unit> = f.units.iter().filter(|u| u.kind == "cell" && u.attrs["col"] == json!(h)).collect(); let n = cells.len().max(1); let num = cells.iter().filter(|c| c.attrs["numeric"] == json!(true)).count(); let empty = cells.iter().filter(|c| c.attrs["empty"] == json!(true)).count(); row(h, json!(format!("{}{} empty{}", if num * 2 > n { "numbers" } else { "text" }, format!(" · {}%", empty * 100 / n), cells.iter().find(|c| c.attrs["empty"] != json!(true)).map(|c| format!(" · e.g. {}", c.text.chars().take(30).collect::<String>())).unwrap_or_default()))) }).collect();
            summary = format!("a table of {} row(s) and {} column(s)", at(&f, "rows"), at(&f, "cols")); secs.push(sec("Table", vec![row("rows", at(&f, "rows")), row("columns", at(&f, "cols")), row("empty cells", at(&f, "empty_cells")), row("duplicate rows", at(&f, "duplicate_rows"))])); secs.push(sec("Columns", cols));
        }
        "image" | "svg" => { summary = format!("a {} image, {}×{} px", at(&f, "format").as_str().unwrap_or(""), at(&f, "width"), at(&f, "height")); secs.push(sec("Image", vec![row("format", at(&f, "format")), row("width", at(&f, "width")), row("height", at(&f, "height")), row("aspect ratio", at(&f, "aspect")), row("orientation", at(&f, "orientation")), row("megapixels", at(&f, "megapixels"))])); }
        _ => {
            if let Some(t) = &f.text { let (syms, imps) = code_symbols(t, &ext); summary = format!("{} lines of {}, {} definition(s), {} import(s)", at(&f, "lines"), if ext.is_empty() { "text" } else { &ext }, syms.len(), imps.len());
                let todo = regex::Regex::new(r"\b(TODO|FIXME|XXX)\b").unwrap().find_iter(t).count(); secs.push(sec("Code", vec![row("lines", at(&f, "lines")), row("definitions", json!(syms.len())), row("imports", json!(imps.len())), row("TODO markers", json!(todo))]));
                if !syms.is_empty() { secs.push(sec("Definitions", syms.iter().map(|s| json!({ "k": s["name"], "v": format!("line {}{}{}", s["line"], if s["exported"] == json!(true) { " · exported" } else { "" }, if s["doc"].as_str().unwrap_or("").is_empty() { String::new() } else { format!(" · {}", s["doc"].as_str().unwrap_or("")) }), "signature": s["signature"] })).collect())); }
                if !imps.is_empty() { secs.push(sec("Uses", imps.iter().map(|i| row(i, Value::Null)).collect())); } }
        }
    }
    json!({ "ok": true, "path": path, "kind": f.kind, "title": name, "summary": summary, "sections": secs })
}
