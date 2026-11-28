//! book-atlas.rs — twin of skills/agents/in/book-atlas.mjs (db, cross-db knowledge.*). NAMES ARE
//! CONTRACT: default `bookAtlas`; named `detectTemplate, sliceFields, findListings,
//! definitionCandidates, termFrequency, normativeCandidates, questionCandidates, xrefCandidates,
//! classifyStructure, figureCandidates, summarySlices, calloutCandidates, procedureCandidates,
//! exerciseSlices, glossaryCandidates, buildAtlas, slotKeyOf, applyAnnotations, loadAnnotations`.
// QUALITY: 74/100 — P1 of the AUTHORITY ATLAS: pure functions over ingested book blocks compute
// receipted, deterministic slots (template/lexicon/normative/questions/tradeoffs/listings/xrefs/
// density/plan) the swarm later fills; the map layer (slotKeyOf/applyAnnotations) merges the human's
// corrections back in by deterministic key, honoring a curated '__order__' sequence.
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
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
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
// a Value known never-null-never-undefined at the point of construction: whole doubles render
// as JSON integers (matches JS Number-vs-JSON.stringify: 1.0 -> "1", not "1.0").
fn num_js(f: f64) -> Value {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 { json!(f as i64) } else { json!(f) }
}
fn defined_non_null(v: Option<&Value>) -> bool {
    matches!(v, Some(x) if !x.is_null())
}
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }

// ── book-atlas's own module-private helpers (mirror the JS top-level consts/fns) ───────────
fn stop() -> &'static HashSet<&'static str> {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| {
        [
            "the", "a", "an", "of", "to", "in", "for", "and", "or", "is", "are", "on", "with", "as", "by", "at", "from",
            "this", "that", "these", "those", "it", "its", "chapter", "part", "appendix", "figure", "table", "section",
            "pattern", "patterns", "example", "examples", "known", "uses", "also", "see", "other", "new", "one", "two",
            "first", "second", "each", "all", "any", "some", "most", "more", "when", "where", "how", "what", "which",
            "class", "classes", "object", "objects", "design", "software", "implementation", "structure", "sample",
            "code", "intent", "participants", "consequences", "motivation", "applicability", "collaborations",
        ]
        .into_iter()
        .collect()
    })
}
fn clean_str(s: &str) -> String {
    static WS: OnceLock<Regex> = OnceLock::new();
    let re = WS.get_or_init(|| Regex::new(r"\s+").unwrap());
    re.replace_all(s, " ").trim().to_string()
}
fn receipt_of(ord: &Value, line: i64, text: &str) -> Value {
    json!({ "block_ord": ord.clone(), "line": line, "quote": take_chars(&clean_str(text), 240) })
}
// String(block.text) / String(block.title) — bare String(), NOT the `?? ''` variant: a missing
// key renders "undefined" (matches JS String(undefined)), present-null renders "null".
fn text_of(block: &Value) -> String { match block.get("text") { None => "undefined".to_string(), Some(v) => js_string(v) } }
fn title_of(block: &Value) -> String { match block.get("title") { None => "undefined".to_string(), Some(v) => js_string(v) } }
fn strip_ord_suffix(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r" · \d+$").unwrap());
    re.replace(s, "").to_string()
}

// a "label line": short, standalone, title-case-ish — the raw material of template detection.
fn label_of(raw_line: &str) -> Option<String> {
    static HASH_RE: OnceLock<Regex> = OnceLock::new();
    let hash_re = HASH_RE.get_or_init(|| Regex::new(r"^#{1,4}\s*").unwrap());
    let t = hash_re.replace(raw_line, "").trim().to_string();
    if t.is_empty() || t.chars().count() > 40 { return None; }
    if let Some(last) = t.chars().last() { if ".:;,".contains(last) { return None; } }
    static TITLE_RE: OnceLock<Regex> = OnceLock::new();
    let title_re = TITLE_RE.get_or_init(|| Regex::new(r"^[A-Z][A-Za-z /-]*$").unwrap());
    if !title_re.is_match(&t) { return None; }
    if t.split_whitespace().count() > 4 { return None; }
    Some(t)
}

// ── template: labels recurring across ≥25% of blocks (min 3) = the book's own schema. ───────
fn detect_template_core(blocks_v: &Value) -> Value {
    let blocks = blocks_v.as_array().cloned().unwrap_or_default();
    let mut counts: Map<String, Value> = Map::new();
    for b in &blocks {
        let mut seen: HashSet<String> = HashSet::new();
        for ln in text_of(b).split('\n') {
            if let Some(lb) = label_of(ln) {
                if !seen.contains(&lb) {
                    seen.insert(lb.clone());
                    let cur = counts.get(&lb).and_then(|v| v.as_i64()).unwrap_or(0);
                    counts.insert(lb, json!(cur + 1));
                }
            }
        }
    }
    let min_blocks = std::cmp::max(3, ((blocks.len() as f64) * 0.25).floor() as i64);
    let mut entries: Vec<(String, i64)> = counts.iter().map(|(k, v)| (k.clone(), v.as_i64().unwrap_or(0))).collect();
    entries.retain(|(_, n)| *n >= min_blocks);
    entries.sort_by(|a, b| b.1.cmp(&a.1)); // stable: ties keep first-seen order
    entries.truncate(16);
    let fields: Vec<Value> = entries.into_iter().map(|(label, count)| json!({ "label": label, "count": count })).collect();
    let detected = fields.len() >= 3;
    json!({ "fields": fields, "detected": detected })
}

// per-block slices of the detected template fields — "the Intent of Observer" is a SLICE, not AI.
fn slice_fields_core(block: &Value, field_labels: &HashSet<String>) -> Vec<Value> {
    let text = text_of(block);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut marks: Vec<(String, i64)> = Vec::new();
    for (i, ln) in lines.iter().enumerate() {
        if let Some(lb) = label_of(ln) {
            if field_labels.contains(&lb) { marks.push((lb, i as i64)); }
        }
    }
    let mut out = Vec::new();
    for m in 0..marks.len() {
        let lo = marks[m].1;
        let hi = if m + 1 < marks.len() { marks[m + 1].1 } else { lines.len() as i64 };
        let seg = lines[(lo + 1) as usize..hi as usize].join("\n");
        let t = seg.trim();
        if !t.is_empty() {
            out.push(json!({ "label": marks[m].0.clone(), "line": lo, "chars": t.chars().count() as i64, "text": take_chars(t, 4000) }));
        }
    }
    out
}

// ── code listings: runs of ≥4 code-ish lines (used both to slot skills and to EXCLUDE prose scans).
fn is_code_line(l: &str) -> bool {
    static R1: OnceLock<Regex> = OnceLock::new();
    static R2: OnceLock<Regex> = OnceLock::new();
    static R3: OnceLock<Regex> = OnceLock::new();
    let r1 = R1.get_or_init(|| Regex::new(r"^\s{4,}\S").unwrap());
    let r2 = R2.get_or_init(|| Regex::new(r"[{};]\s*$").unwrap());
    let r3 = R3.get_or_init(|| Regex::new(r"^\s*(class|void|template|public:|private:|def |function |const |return )").unwrap());
    r1.is_match(l) || r2.is_match(l) || r3.is_match(l)
}
fn find_listings_core(block: &Value) -> (Vec<Value>, f64) {
    let text = text_of(block);
    let lines: Vec<&str> = text.split('\n').collect();
    let ord = block.get("ord").cloned().unwrap_or(Value::Null);
    let mut listings: Vec<Value> = Vec::new();
    let mut start: i64 = -1;
    let mut n: i64 = 0;
    let len = lines.len() as i64;
    for i in 0..=len {
        let cur_is_code = i < len && is_code_line(lines[i as usize]);
        let code = i < len && (cur_is_code || (start >= 0 && lines[i as usize].trim().is_empty()));
        if code {
            if start < 0 { start = i; n = 0; }
            if cur_is_code { n += 1; }
        } else {
            if start >= 0 && n >= 4 {
                let hi = ((start + 2).min(len)) as usize;
                let head_src = lines[start as usize..hi].join(" ");
                listings.push(json!({ "block_ord": ord.clone(), "line": start, "lines": i - start, "head": take_chars(&clean_str(&head_src), 120) }));
            }
            start = -1;
            n = 0;
        }
    }
    let code_line_count: i64 = listings.iter().map(|l| l.get("lines").and_then(|v| v.as_i64()).unwrap_or(0)).sum();
    let ratio = if !lines.is_empty() { code_line_count as f64 / lines.len() as f64 } else { 0.0 };
    (listings, ratio)
}

// SENTENCE_SPLIT = /(?<=[.!?])\s+(?=[A-Z"])/ — the regex crate has no lookaround, so this hand-walks
// the same match: a whitespace run immediately preceded by [.!?] and immediately followed by an
// uppercase letter or a `"`. .split() on that pattern removes the matched whitespace at each split.
fn sentence_split(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < n {
        if chars[i].is_whitespace() && i > 0 && matches!(chars[i - 1], '.' | '!' | '?') {
            let mut j = i;
            while j < n && chars[j].is_whitespace() { j += 1; }
            if j < n && (chars[j].is_ascii_uppercase() || chars[j] == '"') {
                parts.push(chars[start..i].iter().collect::<String>());
                start = j;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    parts.push(chars[start..].iter().collect::<String>());
    parts
}
fn prose_sentences_core(block: &Value) -> Vec<(i64, String)> {
    let (listings, _) = find_listings_core(block);
    let mut code_lines: HashSet<i64> = HashSet::new();
    for l in &listings {
        let line = l.get("line").and_then(|v| v.as_i64()).unwrap_or(0);
        let lines_n = l.get("lines").and_then(|v| v.as_i64()).unwrap_or(0);
        for k in line..line + lines_n { code_lines.insert(k); }
    }
    let text = text_of(block);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<(i64, &str)> = Vec::new();
    for (i, ln) in lines.iter().enumerate() {
        let li = i as i64;
        if !code_lines.contains(&li) && !ln.trim().is_empty() { out.push((li, ln)); }
    }
    let mut sentences: Vec<(i64, String)> = Vec::new();
    let mut buf = String::new();
    let mut anchor: i64 = 0;
    for (line, text_ln) in out {
        if buf.is_empty() { anchor = line; }
        buf = if buf.is_empty() { text_ln.trim().to_string() } else { format!("{} {}", buf, text_ln.trim()) };
        let mut parts = sentence_split(&buf);
        while parts.len() > 1 {
            sentences.push((anchor, parts.remove(0)));
            anchor = line;
        }
        buf = parts.remove(0);
    }
    if !buf.trim().is_empty() { sentences.push((anchor, buf)); }
    sentences
}

// ── lexicon: definition-shaped sentences + aliases + frequent terms. ────────────────────────
fn definition_candidates_core(blocks_v: &Value) -> (Vec<Value>, Vec<Value>) {
    static DEF_RE: OnceLock<Regex> = OnceLock::new();
    let def_re = DEF_RE.get_or_init(|| Regex::new(r"^(?:The\s+)?([A-Z][A-Za-z][\w /-]{1,50}?)(?:\s+pattern)?\s+(?:is|are|means|describes|defines|refers to)\s+(.{15,})$").unwrap());
    static AKA_RE: OnceLock<Regex> = OnceLock::new();
    let aka_re = AKA_RE.get_or_init(|| Regex::new(r"(?i)^Also Known As\s*[:—-]?\s*(.+)$").unwrap());
    static AKA_LABEL_RE: OnceLock<Regex> = OnceLock::new();
    let aka_label_re = AKA_LABEL_RE.get_or_init(|| Regex::new(r"(?i)^#{0,4}\s*Also Known As\s*$").unwrap());

    let blocks = blocks_v.as_array().cloned().unwrap_or_default();
    let mut defs = Vec::new();
    let mut aliases = Vec::new();
    for b in &blocks {
        let ord = b.get("ord").cloned().unwrap_or(Value::Null);
        for (line, text) in prose_sentences_core(b) {
            let t = clean_str(&text);
            if let Some(caps) = def_re.captures(&t) {
                let term = caps.get(1).unwrap().as_str().trim().to_string();
                if !stop().contains(term.to_lowercase().as_str()) {
                    defs.push(json!({ "term": term, "block_ord": ord.clone(), "receipt": receipt_of(&ord, line, &t) }));
                }
            }
            if let Some(caps) = aka_re.captures(&t) {
                let al = take_chars(&clean_str(caps.get(1).unwrap().as_str()), 120);
                aliases.push(json!({ "block_ord": ord.clone(), "aliases": al, "receipt": receipt_of(&ord, line, &t) }));
            }
        }
        let text_full = text_of(b);
        let lines: Vec<&str> = text_full.split('\n').collect();
        if lines.len() >= 2 {
            for i in 0..lines.len() - 1 {
                if aka_label_re.is_match(lines[i].trim()) && !lines[i + 1].trim().is_empty() {
                    let al = take_chars(&clean_str(lines[i + 1]), 120);
                    aliases.push(json!({ "block_ord": ord.clone(), "aliases": al, "receipt": receipt_of(&ord, (i + 1) as i64, lines[i + 1]) }));
                }
            }
        }
    }
    (defs, aliases)
}

fn tf_bump(freq: &mut Map<String, Value>, term: &str, ord: &Value) {
    let k = term.trim();
    if k.chars().count() < 4 { return; }
    if stop().contains(k.to_lowercase().as_str()) { return; }
    let entry = freq.entry(k.to_string()).or_insert_with(|| json!({ "n": 0, "blocks": [] }));
    let obj = entry.as_object_mut().unwrap();
    let n = obj.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
    obj.insert("n".into(), json!(n + 1));
    let already = obj.get("blocks").and_then(|v| v.as_array()).map_or(false, |a| a.iter().any(|x| x == ord));
    if !already {
        if let Some(arr) = obj.get_mut("blocks").and_then(|v| v.as_array_mut()) { arr.push(ord.clone()); }
    }
}
fn term_frequency_core(blocks_v: &Value) -> Vec<Value> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\b([A-Z][a-z]{3,})(?:\s+([A-Z][a-z]{3,}))?\b").unwrap());
    let blocks = blocks_v.as_array().cloned().unwrap_or_default();
    let mut freq: Map<String, Value> = Map::new();
    for b in &blocks {
        let ord = b.get("ord").cloned().unwrap_or(Value::Null);
        let sentences = prose_sentences_core(b);
        let prose: String = sentences.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" ");
        for caps in re.captures_iter(&prose) {
            let one = caps.get(1).unwrap().as_str().to_string();
            let two_group = caps.get(2).map(|m| m.as_str().to_string());
            if let Some(g2) = &two_group {
                if !stop().contains(g2.to_lowercase().as_str()) {
                    let two = format!("{} {}", one, g2);
                    tf_bump(&mut freq, &two, &ord);
                    continue;
                }
            }
            tf_bump(&mut freq, &one, &ord);
        }
    }
    let mut entries: Vec<(String, i64, usize)> = freq
        .iter()
        .map(|(k, v)| {
            let n = v.get("n").and_then(|x| x.as_i64()).unwrap_or(0);
            let blen = v.get("blocks").and_then(|x| x.as_array()).map_or(0, |a| a.len());
            (k.clone(), n, blen)
        })
        .collect();
    entries.retain(|(_, n, blen)| *n >= 4 && *blen >= 2);
    entries.sort_by(|a, b| b.1.cmp(&a.1)); // stable
    entries.truncate(120);
    entries.into_iter().map(|(term, count, blen)| json!({ "term": term, "count": count, "blocks": blen as i64 })).collect()
}

// ── normative sentences → law-draft candidates. ──────────────────────────────────────────────
fn normative_candidates_core(blocks_v: &Value) -> Vec<Value> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)\b(should|should not|never|always|avoid|prefer|must|must not|do not|don't)\b").unwrap());
    let blocks = blocks_v.as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for b in &blocks {
        let ord = b.get("ord").cloned().unwrap_or(Value::Null);
        for (line, text) in prose_sentences_core(b) {
            let t = clean_str(&text);
            let len = t.chars().count();
            if len >= 40 && len <= 320 && re.is_match(&t) {
                out.push(json!({ "block_ord": ord.clone(), "receipt": receipt_of(&ord, line, &t) }));
            }
        }
    }
    out
}

// ── the books' own review prompts. ───────────────────────────────────────────────────────────
fn question_candidates_core(blocks_v: &Value) -> Vec<Value> {
    let blocks = blocks_v.as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for b in &blocks {
        let ord = b.get("ord").cloned().unwrap_or(Value::Null);
        for (line, text) in prose_sentences_core(b) {
            let t = clean_str(&text);
            let len = t.chars().count();
            if t.ends_with('?') && len >= 30 && len <= 220 {
                out.push(json!({ "block_ord": ord.clone(), "receipt": receipt_of(&ord, line, &t) }));
            }
        }
    }
    out
}

// ── xrefs: "Iterator (257)" internal refs + [VL90] bibliography keys. ───────────────────────
fn xref_candidates_core(blocks_v: &Value) -> Value {
    static EDGE_RE: OnceLock<Regex> = OnceLock::new();
    let edge_re = EDGE_RE.get_or_init(|| Regex::new(r"\b([A-Z][a-z]+(?: [A-Z][a-z]+)?)\s*\((\d{2,4})\)").unwrap());
    static BIB_RE: OnceLock<Regex> = OnceLock::new();
    let bib_re = BIB_RE.get_or_init(|| Regex::new(r"\[([A-Z][A-Za-z+]{1,9}\d{2})\]").unwrap());
    let blocks = blocks_v.as_array().cloned().unwrap_or_default();
    let mut edges: Vec<Value> = Vec::new();
    let mut bib: Map<String, Value> = Map::new();
    for b in &blocks {
        let text = text_of(b);
        let ord = b.get("ord").cloned().unwrap_or(Value::Null);
        for caps in edge_re.captures_iter(&text) {
            let mut term = caps.get(1).unwrap().as_str().to_string();
            let page_s = caps.get(2).unwrap().as_str();
            let parts: Vec<&str> = term.split(' ').collect();
            if parts.len() == 2 && stop().contains(parts[0].to_lowercase().as_str()) {
                term = parts[1].to_string();
            }
            if !stop().contains(term.to_lowercase().as_str()) {
                let page: i64 = page_s.parse().unwrap_or(0);
                edges.push(json!({ "block_ord": ord.clone(), "term": term, "page": page }));
            }
        }
        for caps in bib_re.captures_iter(&text) {
            let key = caps.get(1).unwrap().as_str().to_string();
            let cur = bib.get(&key).and_then(|v| v.as_i64()).unwrap_or(0);
            bib.insert(key, json!(cur + 1));
        }
    }
    let edges_out: Vec<Value> = edges.into_iter().take(2000).collect();
    let mut bib_entries: Vec<(String, i64)> = bib.iter().map(|(k, v)| (k.clone(), v.as_i64().unwrap_or(0))).collect();
    bib_entries.sort_by(|a, b| b.1.cmp(&a.1)); // stable
    let bibliography: Vec<Value> = bib_entries.into_iter().map(|(key, n)| json!({ "key": key, "n": n })).collect();
    json!({ "edges": edges_out, "bibliography": bibliography })
}

// ── STRUCTURE: front / body / back matter — agnostic title+content patterns. ────────────────
fn classify_structure_core(blocks_v: &Value) -> Value {
    static FRONT_RE: OnceLock<Regex> = OnceLock::new();
    let front_re = FRONT_RE.get_or_init(|| Regex::new(r"(?i)^(preface|foreword|acknowledg|about the author|copyright|contents|table of contents|introduction to this|front matter|dedication|guide to (the )?reader)").unwrap());
    static BACK_RE: OnceLock<Regex> = OnceLock::new();
    let back_re = BACK_RE.get_or_init(|| Regex::new(r"(?i)^(appendix|index$|bibliography|references|glossary|about the|colophon|afterword|epilogue|notes$)").unwrap());
    let blocks = blocks_v.as_array().cloned().unwrap_or_default();
    let out: Vec<Value> = blocks
        .iter()
        .map(|b| {
            let t = strip_ord_suffix(&title_of(b)).trim().to_string();
            let zone = if front_re.is_match(&t) { "front" } else if back_re.is_match(&t) { "back" } else { "body" };
            json!({ "ord": b.get("ord").cloned().unwrap_or(Value::Null), "title": b.get("title").cloned().unwrap_or(Value::Null), "zone": zone })
        })
        .collect();
    Value::Array(out)
}

// ── FIGURES / TABLES / LISTINGS captions — "Figure 4.2 …", "Table 7-1 …". ───────────────────
fn figure_candidates_core(blocks_v: &Value) -> Vec<Value> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)^(Figure|Fig\.|Table|Listing|Diagram|Chart)\s+([\dA-Z][\d.\-]*)\s*[:.]?\s*(.{4,120})$").unwrap());
    let blocks = blocks_v.as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for b in &blocks {
        let text = text_of(b);
        let ord = b.get("ord").cloned().unwrap_or(Value::Null);
        for (i, ln) in text.split('\n').enumerate() {
            if let Some(caps) = re.captures(ln.trim()) {
                let kind = caps.get(1).unwrap().as_str().to_lowercase().replacen('.', "", 1);
                let label = caps.get(2).unwrap().as_str();
                out.push(json!({ "block_ord": ord.clone(), "kind": kind, "label": label, "receipt": receipt_of(&ord, i as i64, ln) }));
            }
        }
    }
    out
}

// ── SUMMARY fields — the book's own recaps. ──────────────────────────────────────────────────
fn summary_labels() -> &'static HashSet<String> {
    static S: OnceLock<HashSet<String>> = OnceLock::new();
    S.get_or_init(|| ["Summary", "Conclusion", "Conclusions", "Key Points", "Recap", "In This Chapter", "Chapter Summary", "Discussion"].iter().map(|s| s.to_string()).collect())
}
fn summary_slices_core(blocks_v: &Value) -> Vec<Value> {
    let blocks = blocks_v.as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for b in &blocks {
        let ord = b.get("ord").cloned().unwrap_or(Value::Null);
        for f in slice_fields_core(b, summary_labels()) {
            let text = f.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let line = f.get("line").and_then(|v| v.as_i64()).unwrap_or(0);
            out.push(json!({ "block_ord": ord.clone(), "label": f.get("label").cloned().unwrap_or(Value::Null), "line": line, "chars": f.get("chars").cloned().unwrap_or(Value::Null), "receipt": receipt_of(&ord, line, &take_chars(text, 200)) }));
        }
    }
    out
}

// ── CALLOUTS — Note:/Tip:/Warning:/Caution:/Important: lines. ───────────────────────────────
fn callout_candidates_core(blocks_v: &Value) -> Vec<Value> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i)^(Note|Tip|Warning|Caution|Important|Remember)\s*[:—-]\s*(.{12,240})$").unwrap());
    let blocks = blocks_v.as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for b in &blocks {
        let text = text_of(b);
        let ord = b.get("ord").cloned().unwrap_or(Value::Null);
        for (i, ln) in text.split('\n').enumerate() {
            if let Some(caps) = re.captures(ln.trim()) {
                let kind = caps.get(1).unwrap().as_str().to_lowercase();
                out.push(json!({ "block_ord": ord.clone(), "kind": kind, "receipt": receipt_of(&ord, i as i64, ln) }));
            }
        }
    }
    out
}

// ── PROCEDURES — runs of ≥3 numbered steps outside code. ────────────────────────────────────
fn procedure_run(out: &mut Vec<Value>, ord: &Value, start: i64, expect: i64, lines: &[&str]) {
    out.push(json!({ "block_ord": ord.clone(), "line": start, "steps": expect - 1, "receipt": receipt_of(ord, start, lines[start as usize]) }));
}
fn procedure_candidates_core(blocks_v: &Value) -> Vec<Value> {
    static STEP_RE: OnceLock<Regex> = OnceLock::new();
    let step_re = STEP_RE.get_or_init(|| Regex::new(r"^(\d{1,2})[.)]\s+\S").unwrap());
    let blocks = blocks_v.as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for b in &blocks {
        let ord = b.get("ord").cloned().unwrap_or(Value::Null);
        let (listings, _) = find_listings_core(b);
        let mut code_lines: HashSet<i64> = HashSet::new();
        for l in &listings {
            let line = l.get("line").and_then(|v| v.as_i64()).unwrap_or(0);
            let lines_n = l.get("lines").and_then(|v| v.as_i64()).unwrap_or(0);
            for k in line..line + lines_n { code_lines.insert(k); }
        }
        let text = text_of(b);
        let lines: Vec<&str> = text.split('\n').collect();
        let len = lines.len() as i64;
        let mut start: i64 = -1;
        let mut expect: i64 = 1;
        for i in 0..=len {
            let m_num: Option<i64> = if i < len && !code_lines.contains(&i) {
                step_re.captures(lines[i as usize].trim()).and_then(|c| c.get(1)).and_then(|g| g.as_str().parse::<i64>().ok())
            } else {
                None
            };
            match m_num {
                Some(num) if num == expect => {
                    if start < 0 { start = i; }
                    expect += 1;
                }
                Some(num) if num == 1 => {
                    if expect > 3 { procedure_run(&mut out, &ord, start, expect, &lines); }
                    start = i;
                    expect = 2;
                }
                _ => {
                    if start >= 0 && expect > 3 { procedure_run(&mut out, &ord, start, expect, &lines); }
                    start = -1;
                    expect = 1;
                }
            }
        }
    }
    out
}

// ── EXERCISES — Exercises/Problems/Review Questions sections. ───────────────────────────────
fn exercise_labels() -> &'static HashSet<String> {
    static S: OnceLock<HashSet<String>> = OnceLock::new();
    S.get_or_init(|| ["Exercises", "Exercise", "Problems", "Review Questions", "Questions", "Self-Test"].iter().map(|s| s.to_string()).collect())
}
fn exercise_slices_core(blocks_v: &Value) -> Vec<Value> {
    let blocks = blocks_v.as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for b in &blocks {
        let ord = b.get("ord").cloned().unwrap_or(Value::Null);
        for f in slice_fields_core(b, exercise_labels()) {
            let text = f.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let line = f.get("line").and_then(|v| v.as_i64()).unwrap_or(0);
            out.push(json!({ "block_ord": ord.clone(), "label": f.get("label").cloned().unwrap_or(Value::Null), "line": line, "chars": f.get("chars").cloned().unwrap_or(Value::Null), "receipt": receipt_of(&ord, line, &take_chars(text, 200)) }));
        }
    }
    out
}

// ── the GLOSSARY — back-matter "Term — definition" pairs. ───────────────────────────────────
fn glossary_candidates_core(blocks_v: &Value, structure_v: &Value) -> Vec<Value> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^([A-Z][\w /()-]{2,60}?)\s*[—:–-]\s+(.{20,400})$").unwrap());
    static EXCL_RE: OnceLock<Regex> = OnceLock::new();
    let excl_re = EXCL_RE.get_or_init(|| Regex::new(r"(?i)^(Figure|Table|Note|See)\b").unwrap());
    static GLOSSAR_RE: OnceLock<Regex> = OnceLock::new();
    let glossar_re = GLOSSAR_RE.get_or_init(|| Regex::new(r"(?i)glossar").unwrap());

    let blocks = blocks_v.as_array().cloned().unwrap_or_default();
    let structure = structure_v.as_array().cloned().unwrap_or_default();
    let scan: Vec<Value> = structure
        .iter()
        .filter_map(|x| {
            let zone_ok = x.get("zone").and_then(|v| v.as_str()) == Some("back");
            if zone_ok && glossar_re.is_match(&title_of(x)) { x.get("ord").cloned() } else { None }
        })
        .collect();
    let mut out = Vec::new();
    for ord in &scan {
        let Some(b) = blocks.iter().find(|x| x.get("ord").cloned().unwrap_or(Value::Null) == *ord) else { continue };
        let text = text_of(b);
        for (i, ln) in text.split('\n').enumerate() {
            if let Some(caps) = re.captures(ln.trim()) {
                let term = caps.get(1).unwrap().as_str();
                if !excl_re.is_match(term) {
                    let definition = caps.get(2).unwrap().as_str();
                    out.push(json!({ "term": term.trim(), "definition": definition.trim(), "block_ord": ord.clone(), "receipt": receipt_of(ord, i as i64, ln) }));
                }
            }
        }
    }
    out
}

// ── the whole atlas + THE SWARM PLAN. ────────────────────────────────────────────────────────
fn build_atlas_core(blocks_v: &Value) -> Value {
    let blocks = blocks_v.as_array().cloned().unwrap_or_default();
    let template = detect_template_core(blocks_v);
    let detected = truthy(template.get("detected"));
    let field_set: HashSet<String> = template
        .get("fields")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|f| f.get("label").and_then(|l| l.as_str()).map(|s| s.to_string())).collect())
        .unwrap_or_default();
    let tradeoff_labels: HashSet<&str> = ["Consequences", "Benefits", "Costs", "Trade-offs", "Tradeoffs", "Liabilities", "Drawbacks"].into_iter().collect();
    let def_labels: HashSet<&str> = ["Intent", "Definition", "Overview"].into_iter().collect();

    struct PerBlock { ord: Value, fields: Vec<Value>, listings: Vec<Value>, code_ratio: f64 }
    let mut per_block: Vec<PerBlock> = Vec::new();
    for b in &blocks {
        let fields = if detected { slice_fields_core(b, &field_set) } else { Vec::new() };
        let (listings, code_ratio) = find_listings_core(b);
        per_block.push(PerBlock { ord: b.get("ord").cloned().unwrap_or(Value::Null), fields, listings, code_ratio });
    }

    let (mut defs, aliases) = definition_candidates_core(blocks_v);
    static CHAPTER_TERM_RE: OnceLock<Regex> = OnceLock::new();
    let chapter_re = CHAPTER_TERM_RE.get_or_init(|| Regex::new(r"^[A-Z][A-Za-z]+(?: [A-Z][A-Za-z]+){0,2}(?: · \d+)?$").unwrap());
    for (pb, b) in per_block.iter().zip(blocks.iter()) {
        let def_field = pb.fields.iter().find(|f| f.get("label").and_then(|l| l.as_str()).map_or(false, |l| def_labels.contains(l)));
        let title_s = title_of(b);
        let bare_title = strip_ord_suffix(&title_s);
        if let Some(field) = def_field {
            if chapter_re.is_match(&title_s) && !stop().contains(bare_title.to_lowercase().as_str()) {
                let field_line = field.get("line").and_then(|v| v.as_i64()).unwrap_or(0);
                let field_text = field.get("text").and_then(|v| v.as_str()).unwrap_or("");
                defs.push(json!({
                    "term": bare_title, "block_ord": pb.ord.clone(),
                    "receipt": receipt_of(&pb.ord, field_line, &take_chars(field_text, 200)),
                    "from_template": true
                }));
            }
        }
    }
    let mut seen_terms: HashSet<String> = HashSet::new();
    let mut definitions: Vec<Value> = Vec::new();
    for d in defs {
        let term = d.get("term").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let k = term.to_lowercase();
        if !seen_terms.contains(&k) { seen_terms.insert(k); definitions.push(d); }
    }

    let mut tradeoffs: Vec<Value> = Vec::new();
    for pb in &per_block {
        for f in &pb.fields {
            if f.get("label").and_then(|l| l.as_str()).map_or(false, |l| tradeoff_labels.contains(l)) {
                tradeoffs.push(json!({ "block_ord": pb.ord.clone(), "label": f.get("label").cloned().unwrap_or(Value::Null), "line": f.get("line").cloned().unwrap_or(Value::Null), "chars": f.get("chars").cloned().unwrap_or(Value::Null) }));
            }
        }
    }

    let structure = classify_structure_core(blocks_v);
    let figures = figure_candidates_core(blocks_v);
    let summaries = summary_slices_core(blocks_v);
    let callouts = callout_candidates_core(blocks_v);
    let procedures = procedure_candidates_core(blocks_v);
    let exercises = exercise_slices_core(blocks_v);
    let glossary = glossary_candidates_core(blocks_v, &structure);
    let mut normatives = normative_candidates_core(blocks_v);
    for c in &callouts {
        let kind = c.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        if ["warning", "caution", "important", "remember"].contains(&kind) {
            normatives.push(json!({ "block_ord": c.get("block_ord").cloned().unwrap_or(Value::Null), "receipt": c.get("receipt").cloned().unwrap_or(Value::Null), "from_callout": kind }));
        }
    }
    let questions = question_candidates_core(blocks_v);
    let mut listings: Vec<Value> = Vec::new();
    for pb in &per_block { listings.extend(pb.listings.iter().cloned()); }
    let xrefs = xref_candidates_core(blocks_v);
    let lexicon = term_frequency_core(blocks_v);

    let mut density: Vec<Value> = Vec::new();
    for (b, pb) in blocks.iter().zip(per_block.iter()) {
        let b_ord = b.get("ord").cloned().unwrap_or(Value::Null);
        let n_def = definitions.iter().filter(|d| d.get("block_ord").cloned().unwrap_or(Value::Null) == b_ord).count() as i64;
        let n_norm = normatives.iter().filter(|x| x.get("block_ord").cloned().unwrap_or(Value::Null) == b_ord).count() as i64;
        let n_q = questions.iter().filter(|x| x.get("block_ord").cloned().unwrap_or(Value::Null) == b_ord).count() as i64;
        let fields_len = pb.fields.len() as i64;
        let score = n_def * 3 + n_norm * 2 + n_q + fields_len * 2;
        let is_ord_zero = b_ord.as_i64() == Some(0);
        let skip: Value = if pb.code_ratio > 0.75 { json!("listing-dominated") } else if is_ord_zero && score == 0 { json!("front matter") } else { Value::Null };
        density.push(json!({ "ord": b_ord.clone(), "title": b.get("title").cloned().unwrap_or(Value::Null), "score": score, "skip": skip }));
    }

    let n_definitions = definitions.len() as i64;
    let n_normatives = normatives.len() as i64;
    let n_questions = questions.len() as i64;
    let n_tradeoffs = tradeoffs.len() as i64;
    let n_listings = listings.len() as i64;
    let xref_edges_len = xrefs.get("edges").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0) as i64;
    let n_procedures = procedures.len() as i64;
    let n_glossary = glossary.len() as i64;
    let n_summaries = summaries.len() as i64;
    let n_callouts = callouts.len() as i64;
    let n_figures = figures.len() as i64;
    let n_exercises = exercises.len() as i64;

    let staged = n_definitions + n_normatives + n_questions + n_tradeoffs + n_listings + n_procedures;
    let tin = n_definitions * 900 + n_normatives * 750 + n_questions * 520 + n_tradeoffs * 950 + (n_listings + n_procedures) * 1200 + staged * 420;
    let tout = n_definitions * 120 + n_normatives * 200 + n_questions * 90 + n_tradeoffs * 170 + (n_listings + n_procedures) * 280 + staged * 60;
    let cost_low = (tin as f64 / 1e6) * 0.25 + (tout as f64 / 1e6) * 0.85;
    let cost_mid = (tin as f64 / 1e6) * 1.0 + (tout as f64 / 1e6) * 3.0;
    let round4 = |f: f64| (f * 10000.0).round() / 10000.0;

    let plan = json!({
        "slots": {
            "definitions": n_definitions, "normatives": n_normatives, "questions": n_questions, "tradeoffs": n_tradeoffs,
            "listings": n_listings, "xref_edges": xref_edges_len, "procedures": n_procedures, "glossary": n_glossary,
            "summaries": n_summaries, "callouts": n_callouts, "figures": n_figures, "exercises": n_exercises
        },
        "est_calls": staged * 2,
        "est_tokens_in": tin,
        "est_tokens_out": tout,
        "est_cost": { "low_tier": num_js(round4(cost_low)), "mid_tier": num_js(round4(cost_mid)) },
        "assumptions": [
            "per-task budgets (in/out tokens): definer 900/120 · lawwright 750/200 · question 520/90 · tradeoff 950/170 · skillsmith 1200/280",
            "one adversarial warden verify per staged item (420/60)",
            "prices $/1M tok — low 0.25/0.85, mid 1/3"
        ]
    });

    json!({
        "template": template,
        "structure": structure,
        "slots": {
            "definitions": definitions, "normatives": normatives, "questions": questions, "tradeoffs": tradeoffs,
            "listings": listings, "xrefs": xrefs, "aliases": aliases, "procedures": procedures, "glossary": glossary,
            "summaries": summaries, "callouts": callouts, "figures": figures, "exercises": exercises
        },
        "lexicon": lexicon,
        "density": density,
        "plan": plan
    })
}

// ── THE MAP LAYER — deterministic slot identities + the annotation merge. ───────────────────
fn slot_key_of_core(tool: &str, slot: &Value) -> Value {
    if tool == "definitions" && truthy(slot.get("term")) {
        return json!(format!("def:{}", js_string(slot.get("term").unwrap()).to_lowercase()));
    }
    if tool == "glossary" && truthy(slot.get("term")) {
        return json!(format!("glossary:{}", js_string(slot.get("term").unwrap()).to_lowercase()));
    }
    let ord = nz(slot.get("block_ord")).or_else(|| nz(slot.get("receipt").and_then(|r| r.get("block_ord")))).cloned().unwrap_or(json!(0));
    let line = nz(slot.get("line")).or_else(|| nz(slot.get("receipt").and_then(|r| r.get("line")))).cloned().unwrap_or(json!(0));
    json!(format!("{}:{}:{}", tool, tmpl(Some(&ord)), tmpl(Some(&line))))
}

// try { JSON.parse(a.payload_json || '{}') } catch { pl = {} }
fn parse_payload(a: &Value) -> Value {
    let s = a.get("payload_json").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("{}");
    serde_json::from_str(s).unwrap_or(json!({}))
}

fn apply_annotations_core(tool: &str, slots_v: &Value, annotations_v: &Value) -> Value {
    let slots_arr = slots_v.as_array().cloned().unwrap_or_default();
    let all_anns = annotations_v.as_array().cloned().unwrap_or_default();
    let anns: Vec<Value> = all_anns.into_iter().filter(|a| a.get("tool").and_then(|v| v.as_str()) == Some(tool)).collect();
    if anns.is_empty() {
        let out: Vec<Value> = slots_arr
            .iter()
            .map(|sl| {
                let mut m = sl.as_object().cloned().unwrap_or_default();
                m.insert("slot_key".into(), slot_key_of_core(tool, sl));
                Value::Object(m)
            })
            .collect();
        return Value::Array(out);
    }
    // byKey: Map slot_key -> ann (a Map built from pairs keeps the LAST value for a repeated key)
    let mut by_key: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for a in &anns {
        if let Some(k) = a.get("slot_key").and_then(|v| v.as_str()) { by_key.insert(k.to_string(), a.clone()); }
    }
    let mut out: Vec<Value> = Vec::new();
    for sl in &slots_arr {
        let key_v = slot_key_of_core(tool, sl);
        let key = key_v.as_str().unwrap_or("").to_string();
        let Some(a) = by_key.get(&key) else {
            let mut m = sl.as_object().cloned().unwrap_or_default();
            m.insert("slot_key".into(), Value::String(key));
            out.push(Value::Object(m));
            continue;
        };
        if a.get("status").and_then(|v| v.as_str()) == Some("rejected") { continue; }
        let pl = parse_payload(a);
        let moved = a.get("status").and_then(|v| v.as_str()) == Some("moved") && defined_non_null(a.get("block_ord"));
        let mut m = sl.as_object().cloned().unwrap_or_default();
        if truthy(pl.get("term")) { m.insert("term".into(), json!(js_string(pl.get("term").unwrap()))); }
        if truthy(pl.get("label")) { m.insert("label".into(), json!(js_string(pl.get("label").unwrap()))); }
        if moved {
            m.insert("block_ord".into(), a.get("block_ord").cloned().unwrap());
            if let Some(lv) = nz(a.get("line")).or_else(|| nz(sl.get("line"))).cloned() { m.insert("line".into(), lv); }
        }
        if truthy(sl.get("receipt")) {
            let mut rm = sl.get("receipt").unwrap().as_object().cloned().unwrap_or_default();
            if moved {
                rm.insert("block_ord".into(), a.get("block_ord").cloned().unwrap());
                let rline = nz(a.get("line")).or_else(|| nz(sl.get("receipt").and_then(|r| r.get("line")))).cloned();
                if let Some(lv) = rline { rm.insert("line".into(), lv); }
            }
            if truthy(pl.get("quote")) {
                let q = js_string(pl.get("quote").unwrap());
                rm.insert("quote".into(), json!(take_chars(&q, 240)));
            }
            m.insert("receipt".into(), Value::Object(rm));
        }
        m.insert("slot_key".into(), Value::String(key));
        m.insert("human".into(), a.get("status").cloned().unwrap_or(Value::Null));
        out.push(Value::Object(m));
    }
    for a in &anns {
        let status_added = a.get("status").and_then(|v| v.as_str()) == Some("added");
        let is_order = a.get("slot_key").and_then(|v| v.as_str()) == Some("__order__");
        if !status_added || is_order { continue; }
        let pl = parse_payload(a);
        let mut m = Map::new();
        if let Some(t) = pl.get("term") { m.insert("term".into(), t.clone()); }
        if let Some(l) = pl.get("label") { m.insert("label".into(), l.clone()); }
        let bo = nz(a.get("block_ord")).cloned().unwrap_or(json!(0));
        let ln = nz(a.get("line")).cloned().unwrap_or(json!(0));
        m.insert("block_ord".into(), bo.clone());
        m.insert("line".into(), ln.clone());
        let quote = if truthy(pl.get("quote")) { js_string(pl.get("quote").unwrap()) } else { "(human-added)".to_string() };
        m.insert("receipt".into(), json!({ "block_ord": bo, "line": ln, "quote": take_chars(&quote, 240) }));
        m.insert("slot_key".into(), a.get("slot_key").cloned().unwrap_or(Value::Null));
        m.insert("human".into(), json!("added"));
        out.push(Value::Object(m));
    }
    if let Some(order_ann) = anns.iter().find(|a| a.get("slot_key").and_then(|v| v.as_str()) == Some("__order__")) {
        let payload_str = order_ann.get("payload_json").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("{}");
        let keys_value: Value = match serde_json::from_str::<Value>(payload_str) {
            Ok(v) => nz(v.get("keys")).cloned().unwrap_or(json!([])),
            Err(_) => json!([]),
        };
        if let Some(keys_arr) = keys_value.as_array() {
            if !keys_arr.is_empty() {
                let mut pos: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
                for (i, k) in keys_arr.iter().enumerate() { pos.insert(js_string(k), i as i64); }
                let mut nat: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
                for (i, sl) in out.iter().enumerate() {
                    let sk = sl.get("slot_key").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    nat.insert(sk, i as i64);
                }
                let score = |sl: &Value| -> i64 {
                    let sk = sl.get("slot_key").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    match pos.get(&sk) { Some(p) => *p, None => 1_000_000_000 + nat.get(&sk).cloned().unwrap_or(0) }
                };
                out.sort_by(|x, y| score(x).cmp(&score(y))); // stable
            }
        }
    }
    Value::Array(out)
}

// ── exported (dispatchable) units — NAMES ARE CONTRACT, exact JS names. ─────────────────────
pub fn detectTemplate(blocks: &Value) -> Value { detect_template_core(blocks) }

// field_labels is a real JS Set in production (fieldLabels.has(x)) — no JSON Set type, so the
// fixture wraps it as {"__set": [...]} (conform.mjs/conform-db.mjs's reviveArg convention) and
// this unwraps the same shape.
pub fn sliceFields(block: &Value, field_labels: &Value) -> Value {
    let set: HashSet<String> = field_labels
        .get("__set")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default();
    Value::Array(slice_fields_core(block, &set))
}

pub fn findListings(block: &Value) -> Value {
    let (listings, ratio) = find_listings_core(block);
    json!({ "listings": listings, "codeRatio": num_js(ratio) })
}

pub fn definitionCandidates(blocks: &Value) -> Value {
    let (defs, aliases) = definition_candidates_core(blocks);
    json!({ "defs": defs, "aliases": aliases })
}

pub fn termFrequency(blocks: &Value) -> Value { Value::Array(term_frequency_core(blocks)) }

pub fn normativeCandidates(blocks: &Value) -> Value { Value::Array(normative_candidates_core(blocks)) }

pub fn questionCandidates(blocks: &Value) -> Value { Value::Array(question_candidates_core(blocks)) }

pub fn xrefCandidates(blocks: &Value) -> Value { xref_candidates_core(blocks) }

pub fn classifyStructure(blocks: &Value) -> Value { classify_structure_core(blocks) }

pub fn figureCandidates(blocks: &Value) -> Value { Value::Array(figure_candidates_core(blocks)) }

pub fn summarySlices(blocks: &Value) -> Value { Value::Array(summary_slices_core(blocks)) }

pub fn calloutCandidates(blocks: &Value) -> Value { Value::Array(callout_candidates_core(blocks)) }

pub fn procedureCandidates(blocks: &Value) -> Value { Value::Array(procedure_candidates_core(blocks)) }

pub fn exerciseSlices(blocks: &Value) -> Value { Value::Array(exercise_slices_core(blocks)) }

pub fn glossaryCandidates(blocks: &Value, structure: &Value) -> Value { Value::Array(glossary_candidates_core(blocks, structure)) }

pub fn buildAtlas(blocks: &Value) -> Value { build_atlas_core(blocks) }

pub fn slotKeyOf(tool: &Value, slot: &Value) -> Value {
    slot_key_of_core(tool.as_str().unwrap_or(""), slot)
}

pub fn applyAnnotations(tool: &Value, slots: &Value, annotations: &Value) -> Value {
    apply_annotations_core(tool.as_str().unwrap_or(""), slots, annotations)
}

pub fn loadAnnotations(db: &Connection, book_id: &Value) -> Value {
    match crate::db_json::query_rows(db, "SELECT * FROM knowledge.book_annotations WHERE book_id = ?", &[book_id.clone()]) {
        Ok(v) => v,
        Err(_) => json!([]),
    }
}

fn tokens_of(text: Option<&Value>) -> i64 {
    let s = str_qq(text);
    ((s.chars().count() as f64) / 4.0).ceil() as i64
}

pub fn bookAtlas(input: &Value, db: &Connection) -> Result<Value, String> {
    let book_id_num = js_number(input.get("book_id"));
    if !book_id_num.is_finite() || book_id_num.fract() != 0.0 || book_id_num <= 0.0 {
        return Err("book-atlas: book_id is required".to_string());
    }
    let book_id = book_id_num as i64;
    let book_rows = crate::db_json::query_rows(db, "SELECT id, title FROM knowledge.books WHERE id = ?", &[json!(book_id)])?;
    let Some(book) = book_rows.as_array().and_then(|a| a.first()).cloned() else {
        return Err(format!("book-atlas: book '{}' not found", book_id));
    };
    let blocks_v = crate::db_json::query_rows(db, "SELECT ord, title, text FROM knowledge.book_blocks WHERE book_id = ? ORDER BY ord", &[json!(book_id)])?;
    let blocks_arr = blocks_v.as_array().cloned().unwrap_or_default();
    if blocks_arr.is_empty() {
        return Ok(json!({ "ok": false, "error": "the book has no blocks — ingest it first" }));
    }
    let blocks_val = Value::Array(blocks_arr.clone());
    let atlas = build_atlas_core(&blocks_val);
    let annotations_v = crate::db_json::query_rows(db, "SELECT * FROM knowledge.book_annotations WHERE book_id = ?", &[json!(book_id)]).unwrap_or(json!([]));
    let annotations_arr = annotations_v.as_array().cloned().unwrap_or_default();

    if truthy(input.get("tool")) {
        let tool = js_string(input.get("tool").unwrap());
        let raw: Vec<Value> = if tool == "structure" {
            atlas.get("structure").and_then(|v| v.as_array()).cloned().unwrap_or_default()
        } else {
            atlas.get("slots").and_then(|s| s.get(tool.as_str())).and_then(|v| v.as_array()).cloned().unwrap_or_default()
        };
        let raw_keyed: Vec<Value> = if tool == "structure" {
            raw.iter()
                .map(|z| {
                    let mut m = z.as_object().cloned().unwrap_or_default();
                    m.insert("slot_key".into(), json!(format!("structure:{}", tmpl(z.get("ord")))));
                    Value::Object(m)
                })
                .collect()
        } else {
            raw.iter()
                .map(|sl| {
                    let mut m = sl.as_object().cloned().unwrap_or_default();
                    m.insert("slot_key".into(), slot_key_of_core(&tool, sl));
                    Value::Object(m)
                })
                .collect()
        };
        let effective: Value = if tool == "structure" { json!(raw_keyed) } else { apply_annotations_core(&tool, &Value::Array(raw.clone()), &Value::Array(annotations_arr.clone())) };
        let raw_sample: Vec<Value> = raw_keyed.into_iter().take(800).collect();
        let eff_sample: Value = match effective.as_array() {
            Some(a) => json!(a.iter().take(800).cloned().collect::<Vec<_>>()),
            None => effective,
        };
        let anns_filtered: Vec<Value> = annotations_arr.iter().filter(|a| a.get("tool").and_then(|v| v.as_str()) == Some(tool.as_str())).cloned().collect();
        return Ok(json!({ "ok": true, "book_id": book_id, "tool": tool, "raw": raw_sample, "effective": eff_sample, "annotations": anns_filtered }));
    }

    let sample = |arr: Option<&Value>, n: usize| -> Value {
        match arr.and_then(|v| v.as_array()) { Some(a) => json!(a.iter().take(n).cloned().collect::<Vec<_>>()), None => json!([]) }
    };
    let slots = atlas.get("slots").cloned().unwrap_or(json!({}));
    let structure_arr = atlas.get("structure").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let front = structure_arr.iter().filter(|x| x.get("zone").and_then(|v| v.as_str()) == Some("front")).count() as i64;
    let body = structure_arr.iter().filter(|x| x.get("zone").and_then(|v| v.as_str()) == Some("body")).count() as i64;
    let back = structure_arr.iter().filter(|x| x.get("zone").and_then(|v| v.as_str()) == Some("back")).count() as i64;
    let density_arr = atlas.get("density").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let skipped = density_arr.iter().filter(|d| truthy(d.get("skip"))).count() as i64;
    let mut density_sorted = density_arr.clone();
    density_sorted.sort_by(|a, b| {
        let bs = b.get("score").and_then(|v| v.as_i64()).unwrap_or(0);
        let as_ = a.get("score").and_then(|v| v.as_i64()).unwrap_or(0);
        bs.cmp(&as_) // stable
    });
    let top: Vec<Value> = density_sorted.into_iter().take(8).collect();

    let tokens_est: i64 = blocks_arr.iter().map(|b| tokens_of(b.get("text"))).sum();

    Ok(json!({
        "ok": true,
        "book_id": book_id,
        "title": book.get("title").cloned().unwrap_or(Value::Null),
        "template": atlas.get("template").cloned().unwrap_or(Value::Null),
        "plan": atlas.get("plan").cloned().unwrap_or(Value::Null),
        "lexicon_top": sample(atlas.get("lexicon"), 20),
        "samples": {
            "definitions": sample(slots.get("definitions"), 5),
            "normatives": sample(slots.get("normatives"), 5),
            "questions": sample(slots.get("questions"), 5),
            "tradeoffs": sample(slots.get("tradeoffs"), 5),
            "listings": sample(slots.get("listings"), 5),
            "aliases": sample(slots.get("aliases"), 5),
            "procedures": sample(slots.get("procedures"), 5),
            "glossary": sample(slots.get("glossary"), 8),
            "summaries": sample(slots.get("summaries"), 5),
            "callouts": sample(slots.get("callouts"), 5),
            "figures": sample(slots.get("figures"), 5),
            "exercises": sample(slots.get("exercises"), 5),
        },
        "structure": { "front": front, "body": body, "back": back, "zones": structure_arr },
        "density": { "skipped": skipped, "top": top },
        "annotations_count": annotations_arr.len() as i64,
        "tokens_est": tokens_est,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attach_knowledge(conn: &Connection) {
        conn.execute_batch("ATTACH ':memory:' AS knowledge").unwrap();
        conn.execute_batch(
            "CREATE TABLE knowledge.books (id INTEGER PRIMARY KEY, title TEXT);
             CREATE TABLE knowledge.book_blocks (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER, ord INTEGER, title TEXT, text TEXT);
             CREATE TABLE knowledge.book_annotations (id INTEGER PRIMARY KEY AUTOINCREMENT, book_id INTEGER, tool TEXT, slot_key TEXT, status TEXT, block_ord INTEGER, line INTEGER, payload_json TEXT, note TEXT);",
        )
        .unwrap();
    }

    #[test]
    fn detect_template_and_slice_fields() {
        // `detected` requires >=3 DISTINCT labels each meeting min_blocks (max(3, floor(len*0.25))
        // = 3 of 4 here) — Intent+Consequences alone (2 labels) legitimately yields detected=false;
        // a 3rd label (Rationale) in blocks A/B/C makes this fixture a genuine happy-path case.
        let blocks = json!([
            { "ord": 0, "title": "A", "text": "## Intent\nline one\n## Consequences\nc1\n## Rationale\nr1" },
            { "ord": 1, "title": "B", "text": "## Intent\nline two\n## Consequences\nc2\n## Rationale\nr2" },
            { "ord": 2, "title": "C", "text": "## Intent\nline three\n## Consequences\nc3\n## Rationale\nr3" },
            { "ord": 3, "title": "D", "text": "no headings here" },
        ]);
        let t = detectTemplate(&blocks);
        assert_eq!(t["detected"], json!(true));
        let labels: Vec<String> = t["fields"].as_array().unwrap().iter().map(|f| f["label"].as_str().unwrap().to_string()).collect();
        assert!(labels.contains(&"Intent".to_string()));
        assert!(labels.contains(&"Consequences".to_string()));

        let field_labels = json!({ "__set": ["Intent", "Consequences"] });
        let sliced = sliceFields(&blocks[0], &field_labels);
        let arr = sliced.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["label"], json!("Intent"));
        assert_eq!(arr[0]["text"], json!("line one"));
    }

    #[test]
    fn build_atlas_matches_known_shape() {
        // mirrors a hand-verified run against the real JS (node) source.
        let text1 = [
            "Intent",
            "The Observer pattern defines a one-to-many dependency between objects so that when one object changes state, all its dependents are notified automatically.",
            "Also Known As",
            "Dependents, Publish-Subscribe",
            "Consequences",
            "Observers can be reused independently. You should always decouple the subject from the observer.",
            "How do you notify all dependents without tight coupling?",
            "Warning: never call notify() inside a lock.",
            "Figure 4.2 Observer class diagram showing subject and observer roles.",
            "    class Subject {",
            "      void attach(Observer* o) {",
            "        observers_.push_back(o);",
            "      }",
            "    };",
        ].join("\n");
        let text2 = [
            "Intent",
            "The Strategy pattern encapsulates a family of algorithms and makes them interchangeable.",
            "Also Known As",
            "Policy",
            "Consequences",
            "Clients must be aware of different strategies. You must never hardcode the algorithm choice.",
            "Steps to apply this pattern:",
            "1) Identify the algorithm family.",
            "2) Extract a common interface.",
            "3) Implement each variant as a class.",
            "4) Inject the chosen strategy at runtime.",
        ].join("\n");
        let text3 = [
            "Intent",
            "The Mediator pattern defines an object that encapsulates how a set of objects interact, promoting loose coupling.",
            "Also Known As",
            "Controller",
            "See Observer (257) for a related pattern. [GoF95] is the canonical reference. [GoF95] appears twice.",
            "Summary",
            "This chapter covered mediation between colleague objects.",
            "Exercises",
            "Implement a chat room mediator for five users.",
        ].join("\n");
        let blocks = json!([
            { "ord": 0, "title": "Preface", "text": "Welcome.\nThis book is short.\n" },
            { "ord": 1, "title": "Observer · 257", "text": text1 },
            { "ord": 2, "title": "Strategy", "text": text2 },
            { "ord": 3, "title": "Mediator", "text": text3 },
            { "ord": 4, "title": "Appendix A · Notes", "text": "Supplementary material only.\n" },
            { "ord": 5, "title": "Glossary", "text": "Coupling — The degree of interdependence between software modules in a system.\nCohesion — The degree to which the elements of a module belong together.\nFigure 9.1 not a glossary entry" },
        ]);
        let atlas = buildAtlas(&blocks);
        assert_eq!(atlas["template"]["detected"], json!(false)); // only 2 fields clear the min-blocks bar
        assert_eq!(atlas["slots"]["tradeoffs"], json!([])); // template not detected -> no per-block field slices
        assert_eq!(atlas["slots"]["xrefs"]["edges"][0]["term"], json!("Observer"));
        assert_eq!(atlas["slots"]["xrefs"]["edges"][0]["page"], json!(257));
        assert_eq!(atlas["slots"]["xrefs"]["bibliography"][0], json!({ "key": "GoF95", "n": 2 }));
        assert_eq!(atlas["slots"]["glossary"].as_array().unwrap().len(), 2);
        assert_eq!(atlas["slots"]["glossary"][0]["term"], json!("Coupling"));
        assert_eq!(atlas["slots"]["procedures"][0]["steps"], json!(4));
        assert_eq!(atlas["density"][0]["skip"], json!("front matter"));
        assert_eq!(atlas["plan"]["slots"]["definitions"], json!(2));
        assert!(atlas["plan"]["est_cost"]["low_tier"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn apply_annotations_orders_and_merges() {
        // mirrors a hand-verified run against the real JS (node) source (probe2 scenario).
        let slots = json!([
            { "term": "Observer", "block_ord": 1, "receipt": { "block_ord": 1, "line": 0, "quote": "Observer is..." }, "from_template": true },
            { "term": "Mediator", "block_ord": 2, "receipt": { "block_ord": 2, "line": 0, "quote": "Mediator is..." }, "from_template": true },
            { "term": "Memento", "block_ord": 3, "receipt": { "block_ord": 3, "line": 0, "quote": "Memento is..." }, "from_template": true },
        ]);
        let annotations = json!([
            { "tool": "definitions", "slot_key": "def:observer", "status": "confirmed", "block_ord": null, "line": null, "payload_json": "{}" },
            { "tool": "definitions", "slot_key": "def:mediator", "status": "moved", "block_ord": 2, "line": 5, "payload_json": "{\"quote\":\"moved quote here\"}" },
            { "tool": "definitions", "slot_key": "def:memento", "status": "rejected", "block_ord": null, "line": null, "payload_json": "{}" },
            { "tool": "definitions", "slot_key": "def:custom-added", "status": "added", "block_ord": 1, "line": 3, "payload_json": "{\"term\":\"Custom Term\",\"quote\":\"a human quote\"}" },
            { "tool": "definitions", "slot_key": "__order__", "status": "confirmed", "block_ord": null, "line": null, "payload_json": "{\"keys\":[\"def:custom-added\",\"def:observer\"]}" },
        ]);
        let eff = applyAnnotations(&json!("definitions"), &slots, &annotations);
        let arr = eff.as_array().unwrap();
        // rejected (Memento) is dropped entirely
        assert_eq!(arr.len(), 3);
        let keys: Vec<String> = arr.iter().map(|x| x["slot_key"].as_str().unwrap().to_string()).collect();
        assert_eq!(keys, vec!["def:custom-added", "def:observer", "def:mediator"]);
        assert_eq!(arr[0]["term"], json!("Custom Term"));
        assert_eq!(arr[0]["human"], json!("added"));
        assert_eq!(arr[1]["human"], json!("confirmed"));
        assert_eq!(arr[2]["human"], json!("moved"));
        assert_eq!(arr[2]["receipt"]["quote"], json!("moved quote here"));
        assert_eq!(arr[2]["receipt"]["line"], json!(5));
    }

    #[test]
    fn slot_key_of_variants() {
        assert_eq!(slotKeyOf(&json!("definitions"), &json!({ "term": "Observer" })), json!("def:observer"));
        assert_eq!(slotKeyOf(&json!("glossary"), &json!({ "term": "Coupling" })), json!("glossary:coupling"));
        assert_eq!(slotKeyOf(&json!("normatives"), &json!({ "block_ord": 2, "line": 5 })), json!("normatives:2:5"));
        assert_eq!(slotKeyOf(&json!("normatives"), &json!({ "receipt": { "block_ord": 1, "line": 9 } })), json!("normatives:1:9"));
    }

    #[test]
    fn book_atlas_default_and_tool_view() {
        let conn = Connection::open_in_memory().unwrap();
        attach_knowledge(&conn);
        conn.execute("INSERT INTO knowledge.books (id, title) VALUES (1, 'Design Patterns Primer')", []).unwrap();
        let chapter = |name: &str, intent: &str| -> String {
            [
                "## Intent".to_string(),
                format!("{name} is a behavioral pattern that {intent}."),
                "".to_string(),
                "## Also Known As".to_string(),
                "Dependents, Publish-Subscribe".to_string(),
                "".to_string(),
                "## Motivation".to_string(),
                "You should not couple subjects to concrete observers. Always program to the interface.".to_string(),
                format!("Does this {} hide its update protocol from clients?", name.to_lowercase()),
                "See Mediator (273) for a related approach. The design follows [GoF94].".to_string(),
                "".to_string(),
                "## Consequences".to_string(),
                format!("{name} trades broadcast flexibility for unexpected update cascades."),
                "".to_string(),
                "## Implementation".to_string(),
                "    class Subject {".to_string(),
                "    public:".to_string(),
                "        void Attach(Observer*);".to_string(),
                "        void Notify();".to_string(),
                "    };".to_string(),
                "    void Subject::Notify() {".to_string(),
                "        for (auto o : _observers) o->Update(this);".to_string(),
                "    }".to_string(),
            ]
            .join("\n")
        };
        let mut ins = conn.prepare("INSERT INTO knowledge.book_blocks (book_id, ord, title, text) VALUES (1, ?, ?, ?)").unwrap();
        ins.execute(rusqlite::params![0, "front matter", "Preface\n\nThanks to everyone."]).unwrap();
        ins.execute(rusqlite::params![1, "Observer", chapter("Observer", "decouples senders from receivers")]).unwrap();
        ins.execute(rusqlite::params![2, "Mediator", chapter("Mediator", "centralizes complex communications")]).unwrap();
        ins.execute(rusqlite::params![3, "Memento", chapter("Memento", "externalizes state without breaking encapsulation")]).unwrap();
        drop(ins);
        conn.execute("INSERT INTO knowledge.book_annotations (book_id, tool, slot_key, status, block_ord, line, payload_json, note) VALUES (1,'definitions','def:observer','confirmed',NULL,NULL,'{}',NULL)", []).unwrap();
        conn.execute("INSERT INTO knowledge.book_annotations (book_id, tool, slot_key, status, block_ord, line, payload_json, note) VALUES (1,'definitions','def:mediator','moved',2,5,'{\"quote\":\"moved quote here\"}',NULL)", []).unwrap();

        let r = bookAtlas(&json!({ "book_id": 1 }), &conn).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["title"], json!("Design Patterns Primer"));
        assert_eq!(r["template"]["detected"], json!(true));
        assert_eq!(r["samples"]["definitions"].as_array().unwrap().len(), 3);
        assert_eq!(r["annotations_count"], json!(2));

        let rt = bookAtlas(&json!({ "book_id": 1, "tool": "definitions" }), &conn).unwrap();
        assert_eq!(rt["raw"].as_array().unwrap().len(), 3);
        let eff = rt["effective"].as_array().unwrap();
        assert_eq!(eff.len(), 3);
        assert!(eff.iter().any(|x| x["human"] == json!("moved") && x["slot_key"] == json!("def:mediator")));

        let annotations_only = loadAnnotations(&conn, &json!(1));
        assert_eq!(annotations_only.as_array().unwrap().len(), 2);
    }
}
