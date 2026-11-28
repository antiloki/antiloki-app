//! THE LENSES (RFC 0018): a reader turns the bytes of one artifact into FACTS: a few named attributes of the file and a list of UNITS (a heading, a paragraph, a link, a row, a cell, a key…), each with a line and attributes.
//! Every reader is a pure function of the bytes: no clock, no locale, no network, no model, no randomness; maps are ordered; numbers are rounded the same way everywhere. A reader that cannot read says why (`unread`), it never guesses.
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

pub const READER_VERSION: u32 = 1;
const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_UNITS: usize = 20_000;
pub const KINDS: [&str; 12] = ["code", "markdown", "text", "html", "json", "yaml", "toml", "csv", "image", "svg", "binary", "any"];

#[derive(Clone, Debug)]
pub struct Unit { pub kind: String, pub parent: Option<usize>, pub path: String, pub line: usize, pub text: String, pub attrs: BTreeMap<String, Value> }
#[derive(Clone, Debug, Default)]
pub struct Facts { pub path: String, pub kind: String, pub bytes: usize, pub attrs: BTreeMap<String, Value>, pub units: Vec<Unit>, pub doc: Option<Value>, pub text: Option<String>, pub unread: Option<String> }

fn round1(x: f64) -> f64 { (x * 10.0).round() / 10.0 }
fn num(v: f64) -> Value { if v.fract() == 0.0 && v.abs() < 9e15 { json!(v as i64) } else { json!(v) } }
fn int(v: usize) -> Value { json!(v as u64) }
fn short(s: &str, n: usize) -> String { s.chars().take(n).collect() }

/// The kind of an artifact: its extension first, then a bounded look at the bytes. Pure.
pub fn kind_of(path: &str, bytes: &[u8]) -> String {
    let ext = path.rsplit('/').next().unwrap_or(path).rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let k = match ext.as_str() {
        "md" | "markdown" | "mdx" => "markdown", "txt" | "text" | "rst" | "adoc" => "text", "html" | "htm" => "html", "json" | "jsonc" => "json", "yaml" | "yml" => "yaml", "toml" => "toml", "csv" | "tsv" => "csv",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => "image", "svg" => "svg",
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "rs" | "py" | "go" | "java" | "kt" | "c" | "h" | "cc" | "cpp" | "hpp" | "cs" | "rb" | "php" | "swift" | "sh" | "bash" | "ps1" | "sql" | "css" | "scss" | "vue" | "svelte" | "lua" | "dart" => "code",
        _ => "",
    };
    if !k.is_empty() { return k.to_string(); }
    if bytes.starts_with(b"\x89PNG") || bytes.starts_with(b"\xff\xd8\xff") || bytes.starts_with(b"GIF8") { return "image".into(); }
    if std::str::from_utf8(&bytes[..bytes.len().min(4096)]).is_ok() && !bytes.iter().take(4096).any(|b| *b == 0) { "text".into() } else { "binary".into() }
}

/// Read one artifact. Never panics on any input: a file that cannot be read becomes facts with `unread` set. Pure.
pub fn read(path: &str, bytes: &[u8]) -> Facts {
    let kind = kind_of(path, bytes); let mut f = Facts { path: path.to_string(), kind: kind.clone(), bytes: bytes.len(), ..Default::default() };
    f.attrs.insert("bytes".into(), int(bytes.len()));
    match kind.as_str() {
        "image" => read_image(&mut f, bytes), "svg" => { if let Ok(t) = text_of(bytes) { read_svg(&mut f, &t); } else { f.unread = Some("not valid UTF-8".into()); } }
        "binary" => f.unread = Some("a binary file: only its size is known".into()),
        _ => {
            if bytes.len() > MAX_TEXT_BYTES { f.unread = Some(format!("larger than {} MB: not read", MAX_TEXT_BYTES / 1_048_576)); return f; }
            let t = match text_of(bytes) { Ok(t) => t, Err(e) => { f.unread = Some(e); return f; } };
            f.attrs.insert("lines".into(), int(t.lines().count())); f.attrs.insert("chars".into(), int(t.chars().count()));
            match kind.as_str() {
                "markdown" => read_prose(&mut f, &t, true), "text" => read_prose(&mut f, &t, false), "html" => read_html(&mut f, &t),
                "json" => read_doc(&mut f, &t, "json"), "yaml" => read_doc(&mut f, &t, "yaml"), "toml" => read_doc(&mut f, &t, "toml"), "csv" => read_csv(&mut f, &t, path.ends_with(".tsv")),
                _ => {}
            }
            f.text = Some(t);
        }
    }
    f
}
/// UTF-8 text, a BOM dropped, line endings made `\n`; UTF-16 with a BOM is decoded; anything else is not text.
fn text_of(bytes: &[u8]) -> Result<String, String> {
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let le = bytes[0] == 0xFF; let u: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) }).collect();
        return String::from_utf16(&u).map(|s| norm(&s)).map_err(|_| "invalid UTF-16".to_string());
    }
    let b = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
    std::str::from_utf8(b).map(norm).map_err(|_| "not valid UTF-8 text".to_string())
}
fn norm(s: &str) -> String { if s.contains('\r') { s.replace("\r\n", "\n").replace('\r', "\n") } else { s.to_string() } }

// ── words and sentences ─────────────────────────────────────────────────────────────────────
pub fn words_of(s: &str) -> Vec<&str> {
    let mut out = vec![]; let mut start: Option<usize> = None; let mut alnum = false;
    for (i, c) in s.char_indices() {
        let part = c.is_alphanumeric() || ((c == '\'' || c == '’' || c == '-') && start.is_some());
        if part { if start.is_none() { start = Some(i); alnum = false; } if c.is_alphanumeric() { alnum = true; } }
        else if let Some(st) = start.take() { if alnum { out.push(s[st..i].trim_end_matches(['\'', '’', '-'])); } }
    }
    if let Some(st) = start { if alnum { out.push(s[st..].trim_end_matches(['\'', '’', '-'])); } }
    out.retain(|w| !w.is_empty()); out
}
pub fn sentences_of(s: &str) -> Vec<String> {
    let mut out = vec![]; let mut cur = String::new(); let cs: Vec<char> = s.chars().collect(); let mut i = 0;
    while i < cs.len() {
        let c = cs[i]; cur.push(c);
        if matches!(c, '.' | '!' | '?' | '…') { while i + 1 < cs.len() && matches!(cs[i + 1], '.' | '!' | '?' | '…' | '"' | '”' | ')') { i += 1; cur.push(cs[i]); } if i + 1 >= cs.len() || cs[i + 1].is_whitespace() { let t = cur.trim().to_string(); if !words_of(&t).is_empty() { out.push(t); } cur.clear(); } }
        i += 1;
    }
    let t = cur.trim().to_string(); if !words_of(&t).is_empty() { out.push(t); } out
}
fn syllables(w: &str) -> usize { let mut n = 0; let mut prev = false; for c in w.to_lowercase().chars() { let v = matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y' | 'á' | 'é' | 'í' | 'ó' | 'ú' | 'à' | 'â' | 'ê' | 'ô' | 'ã' | 'õ'); if v && !prev { n += 1; } prev = v; } n.max(1) }

fn push(f: &mut Facts, kind: &str, parent: Option<usize>, path: String, line: usize, text: String, attrs: BTreeMap<String, Value>) -> usize {
    if f.units.len() >= MAX_UNITS { f.attrs.insert("units_truncated".into(), json!(true)); return f.units.len().saturating_sub(1); }
    f.units.push(Unit { kind: kind.into(), parent, path, line, text, attrs }); f.units.len() - 1
}
fn lit(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> { pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect() }

/// Markdown (`md = true`) and plain text: headings, paragraphs, list items, sentences, links, images, code fences, and the counts the laws of writing need.
fn read_prose(f: &mut Facts, t: &str, md: bool) {
    let lines: Vec<&str> = t.split('\n').collect(); let mut i = 0usize;
    let mut fm_end = 0usize;
    if md && lines.first().map_or(false, |l| l.trim() == "---") { if let Some(e) = lines.iter().skip(1).position(|l| l.trim() == "---") { fm_end = e + 2; for l in &lines[1..e + 1] { if let Some((k, v)) = l.split_once(':') { f.attrs.insert(format!("fm.{}", k.trim()), json!(v.trim().trim_matches('"'))); } } i = fm_end; } }
    let heading = regex::Regex::new(r"^(#{1,6})\s+(.*?)\s*#*\s*$").unwrap(); let item = regex::Regex::new(r"^\s*(?:[-*+]|\d+[.)])\s+(.*)$").unwrap();
    let link = regex::Regex::new(r#"(!?)\[([^\]]*)\]\(\s*([^)\s]+)(?:\s+"[^"]*")?\s*\)"#).unwrap(); let autolink = regex::Regex::new(r"<(https?://[^>\s]+)>").unwrap();
    let mut stack: Vec<(usize, String, usize)> = vec![]; let mut fence: Option<String> = None;
    let (mut paras, mut items, mut heads, mut h1, mut depth, mut fences) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize); let mut tl = Tally::default();
    let mut cur: Vec<(usize, String)> = vec![]; // (line, text) of the running paragraph
    let mut own_paras: Vec<usize> = vec![]; let mut head_units: Vec<usize> = vec![]; let mut sect_words: Vec<usize> = vec![]; let mut has_sub: Vec<bool> = vec![];
    macro_rules! flush { () => { if !cur.is_empty() { let line = cur[0].0; let text = cur.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>().join(" "); cur.clear(); let section = stack.iter().map(|s| s.1.clone()).collect::<Vec<_>>().join(" › ");
        add_para(f, "paragraph", line, &text, &section, &link, &autolink, &mut tl); paras += 1; if let Some(h) = own_paras.last_mut() { *h += 1; } if let Some(w) = sect_words.last_mut() { *w += words_of(&text).len(); } } }; }
    while i < lines.len() {
        let l = lines[i]; let ln = i + 1; let trimmed = l.trim_start();
        if let Some(fc) = &fence { if trimmed.starts_with(fc.as_str()) { fence = None; } i += 1; continue; }
        if md && (trimmed.starts_with("```") || trimmed.starts_with("~~~")) { flush!(); fences += 1; let m = &trimmed[..3]; let lang = trimmed[3..].trim(); push(f, "fence", None, String::new(), ln, short(lang, 40), lit(&[("lang", json!(lang)), ("has_lang", json!(!lang.is_empty()))])); fence = Some(m.to_string()); i += 1; continue; }
        if l.trim().is_empty() { flush!(); i += 1; continue; }
        if md { if let Some(c) = heading.captures(l) { flush!(); let level = c[1].len(); let text = c[2].trim().to_string(); while stack.last().map_or(false, |s| s.0 >= level) { stack.pop(); } if let Some(p) = has_sub.last_mut() { if stack.last().is_some() { *p = true; } } stack.push((level, text.clone(), ln)); heads += 1; if level == 1 { h1 += 1; } depth = depth.max(level);
            let path = stack.iter().map(|s| s.1.clone()).collect::<Vec<_>>().join(" › "); let u = push(f, "heading", None, path, ln, text, lit(&[("level", int(level))])); head_units.push(u); own_paras.push(0); sect_words.push(0); has_sub.push(false); i += 1; continue; } }
        if md { if let Some(c) = item.captures(l) { flush!(); let text = c[1].trim().to_string(); let section = stack.iter().map(|s| s.1.clone()).collect::<Vec<_>>().join(" › "); add_para(f, "item", ln, &text, &section, &link, &autolink, &mut tl); items += 1; if let Some(w) = sect_words.last_mut() { *w += words_of(&text).len(); } if let Some(h) = own_paras.last_mut() { *h += 1; } i += 1; continue; } }
        cur.push((ln, l.trim().to_string())); i += 1;
    }
    flush!();
    for (k, u) in head_units.iter().enumerate() { let next_deeper = head_units.get(k + 1).map_or(false, |n| f.units[*n].attrs["level"].as_u64().unwrap_or(0) > f.units[*u].attrs["level"].as_u64().unwrap_or(0)); let own = own_paras[k]; let a = &mut f.units[*u].attrs; a.insert("paragraphs".into(), int(own)); a.insert("words".into(), int(sect_words[k])); a.insert("empty".into(), json!(own == 0 && !next_deeper)); }
    let _ = (&has_sub, paras, items);
    let (w, s) = (tl.words.max(1) as f64, tl.sents.max(1) as f64);
    let wc = words_of(t).len();
    let todo = regex::Regex::new(r"\b(TODO|FIXME|XXX|TBD)\b").unwrap().find_iter(t).count();
    let ph = regex::Regex::new(r"(?i)lorem ipsum|\[(?:insert|your|add|todo|tbd)[^\]]*\]|\{\{[^}]+\}\}|<placeholder>|your (?:name|company|product) here").unwrap().find_iter(t).count();
    let excl = t.matches('!').count(); let caps = words_of(t).iter().filter(|w| w.chars().count() >= 3 && w.chars().any(|c| c.is_alphabetic()) && w.chars().all(|c| !c.is_lowercase())).count();
    let a = &mut f.attrs; a.insert("words".into(), int(wc)); a.insert("paragraphs".into(), int(paras)); a.insert("items".into(), int(items)); a.insert("sentences".into(), int(tl.sents)); a.insert("headings".into(), int(heads)); a.insert("h1_count".into(), int(h1)); a.insert("max_heading_depth".into(), int(depth));
    a.insert("code_fences".into(), int(fences)); a.insert("links".into(), int(tl.links)); a.insert("internal_links".into(), int(tl.internal)); a.insert("external_links".into(), int(tl.external)); a.insert("images".into(), int(tl.imgs)); a.insert("images_without_alt".into(), int(tl.noalt));
    a.insert("todo_count".into(), int(todo)); a.insert("placeholder_count".into(), int(ph)); a.insert("exclamations".into(), int(excl)); a.insert("caps_words".into(), int(caps));
    a.insert("avg_sentence_words".into(), num(round1(tl.sent_words as f64 / s))); a.insert("avg_word_len".into(), num(round1(tl.word_chars as f64 / w))); a.insert("reading_minutes".into(), int((wc + 219) / 220));
    // Flesch reading ease is defined for English; it is a fact about the text, and a law that uses it says so (`flesch_en`).
    a.insert("flesch_en".into(), num(round1(206.835 - 1.015 * (tl.sent_words as f64 / s) - 84.6 * (tl.syl as f64 / w)).clamp(-50.0, 120.0)));
    let _ = fm_end;
}
#[derive(Default)]
struct Tally { words: usize, sents: usize, sent_words: usize, word_chars: usize, syl: usize, links: usize, internal: usize, external: usize, imgs: usize, noalt: usize }
fn add_para(f: &mut Facts, kind: &str, line: usize, text: &str, section: &str, link: &regex::Regex, auto: &regex::Regex, st: &mut Tally) {
    let ws = words_of(text); let ss = sentences_of(text); st.words += ws.len(); st.sents += ss.len(); for s in &ss { st.sent_words += words_of(s).len(); } for w in &ws { st.word_chars += w.chars().count(); st.syl += syllables(w); }
    let u = push(f, kind, None, section.to_string(), line, text.to_string(), lit(&[("words", int(ws.len())), ("sentences", int(ss.len())), ("chars", int(text.chars().count())), ("longest_sentence", int(ss.iter().map(|s| words_of(s).len()).max().unwrap_or(0)))]));
    for c in link.captures_iter(text) { let is_img = &c[1] == "!"; let target = c[3].to_string(); let ext = target.starts_with("http://") || target.starts_with("https://") || target.starts_with("mailto:") || target.starts_with("//"); let anchor = target.starts_with('#');
        if is_img { st.imgs += 1; if c[2].trim().is_empty() { st.noalt += 1; } push(f, "image", Some(u), section.to_string(), line, c[2].to_string(), lit(&[("src", json!(target)), ("alt", json!(c[2].to_string())), ("has_alt", json!(!c[2].trim().is_empty())), ("external", json!(ext))])); }
        else { st.links += 1; if ext { st.external += 1; } else { st.internal += 1; } push(f, "link", Some(u), section.to_string(), line, c[2].to_string(), lit(&[("target", json!(target)), ("external", json!(ext)), ("anchor", json!(anchor))])); } }
    for c in auto.captures_iter(text) { st.links += 1; st.external += 1; push(f, "link", Some(u), section.to_string(), line, c[1].to_string(), lit(&[("target", json!(c[1].to_string())), ("external", json!(true)), ("anchor", json!(false))])); }
}

// ── html ────────────────────────────────────────────────────────────────────────────────────
fn strip_tags(s: &str) -> String { regex::Regex::new(r"(?s)<[^>]*>").unwrap().replace_all(s, " ").split_whitespace().collect::<Vec<_>>().join(" ").replace("&amp;", "&").replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"") }
fn line_at(t: &str, off: usize) -> usize { t[..off.min(t.len())].matches('\n').count() + 1 }
fn attr_of(tag: &str, name: &str) -> Option<String> { let re = regex::Regex::new(&format!(r#"(?is)\b{}\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))"#, regex::escape(name))).unwrap(); re.captures(tag).map(|c| c.get(1).or(c.get(2)).or(c.get(3)).map(|m| m.as_str().to_string()).unwrap_or_default()) }
fn read_html(f: &mut Facts, t: &str) {
    let clean = regex::Regex::new(r"(?is)<(script|style)\b.*?</(script|style)>").unwrap().replace_all(t, |c: &regex::Captures| "\n".repeat(c[0].matches('\n').count()));
    let clean = clean.to_string();
    let title = regex::Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap().captures(&clean).map(|c| strip_tags(&c[1]));
    let mut meta_desc: Option<String> = None; let mut lang: Option<String> = None; let mut inline_style = 0usize;
    for m in regex::Regex::new(r"(?is)<meta\b[^>]*>").unwrap().find_iter(&clean) { if attr_of(m.as_str(), "name").map_or(false, |n| n.eq_ignore_ascii_case("description")) { meta_desc = attr_of(m.as_str(), "content"); } }
    if let Some(m) = regex::Regex::new(r"(?is)<html\b[^>]*>").unwrap().find(&clean) { lang = attr_of(m.as_str(), "lang"); }
    for m in regex::Regex::new(r"(?is)<[a-z][^>]*\sstyle\s*=").unwrap().find_iter(&clean) { let _ = m; inline_style += 1; }
    let (mut h1, mut imgs, mut noalt, mut links, mut internal, mut external) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    for m in regex::Regex::new(r"(?is)<h([1-6])\b[^>]*>(.*?)</h[1-6]>").unwrap().captures_iter(&clean) { let level: usize = m[1].parse().unwrap_or(1); if level == 1 { h1 += 1; } let text = strip_tags(&m[2]); let line = line_at(&clean, m.get(0).unwrap().start()); push(f, "heading", None, text.clone(), line, text, lit(&[("level", int(level))])); }
    for m in regex::Regex::new(r"(?is)<p\b[^>]*>(.*?)</p>").unwrap().captures_iter(&clean) { let text = strip_tags(&m[1]); let ws = words_of(&text); let ss = sentences_of(&text); let line = line_at(&clean, m.get(0).unwrap().start()); push(f, "paragraph", None, String::new(), line, text.clone(), lit(&[("words", int(ws.len())), ("sentences", int(ss.len())), ("chars", int(text.chars().count()))])); }
    for m in regex::Regex::new(r"(?is)<img\b[^>]*>").unwrap().find_iter(&clean) { imgs += 1; let alt = attr_of(m.as_str(), "alt"); let has = alt.as_deref().map_or(false, |a| !a.trim().is_empty()); if !has { noalt += 1; } let src = attr_of(m.as_str(), "src").unwrap_or_default(); let ext = src.starts_with("http") || src.starts_with("//") || src.starts_with("data:"); push(f, "image", None, String::new(), line_at(&clean, m.start()), alt.clone().unwrap_or_default(), lit(&[("src", json!(src)), ("alt", json!(alt.unwrap_or_default())), ("has_alt", json!(has)), ("external", json!(ext))])); }
    for m in regex::Regex::new(r"(?is)<a\b[^>]*>").unwrap().find_iter(&clean) { if let Some(h) = attr_of(m.as_str(), "href") { links += 1; let ext = h.starts_with("http://") || h.starts_with("https://") || h.starts_with("mailto:") || h.starts_with("tel:") || h.starts_with("//"); if ext { external += 1; } else { internal += 1; } push(f, "link", None, String::new(), line_at(&clean, m.start()), h.clone(), lit(&[("target", json!(h.clone())), ("external", json!(ext)), ("anchor", json!(h.starts_with('#')))])); } }
    let body = regex::Regex::new(r"(?is)<body[^>]*>(.*)</body>").unwrap().captures(&clean).map(|c| c[1].to_string()).unwrap_or_else(|| clean.clone()); let visible = strip_tags(&body); let wc = words_of(&visible).len();
    let a = &mut f.attrs; a.insert("title".into(), json!(title.clone().unwrap_or_default())); a.insert("title_len".into(), int(title.as_deref().map_or(0, |s| s.chars().count()))); a.insert("has_title".into(), json!(title.as_deref().map_or(false, |s| !s.is_empty())));
    a.insert("has_meta_description".into(), json!(meta_desc.is_some())); a.insert("meta_description_len".into(), int(meta_desc.as_deref().map_or(0, |s| s.chars().count()))); a.insert("has_lang".into(), json!(lang.as_deref().map_or(false, |s| !s.is_empty())));
    a.insert("h1_count".into(), int(h1)); a.insert("images".into(), int(imgs)); a.insert("images_without_alt".into(), int(noalt)); a.insert("links".into(), int(links)); a.insert("internal_links".into(), int(internal)); a.insert("external_links".into(), int(external)); a.insert("inline_styles".into(), int(inline_style)); a.insert("words".into(), int(wc));
    a.insert("headings".into(), int(f.units.iter().filter(|u| u.kind == "heading").count())); a.insert("todo_count".into(), int(regex::Regex::new(r"\b(TODO|FIXME|TBD)\b").unwrap().find_iter(&visible).count()));
    a.insert("placeholder_count".into(), int(regex::Regex::new(r"(?i)lorem ipsum|\[(?:insert|your|add)[^\]]*\]|\{\{[^}]+\}\}").unwrap().find_iter(&visible).count())); a.insert("exclamations".into(), int(visible.matches('!').count()));
    a.insert("caps_words".into(), int(words_of(&visible).iter().filter(|w| w.chars().count() >= 3 && w.chars().any(|c| c.is_alphabetic()) && w.chars().all(|c| !c.is_lowercase())).count()));
}

// ── structured documents ────────────────────────────────────────────────────────────────────
fn yaml_to_json(y: &yaml_rust2::Yaml) -> Value {
    use yaml_rust2::Yaml as Y;
    match y { Y::Real(s) => s.parse::<f64>().ok().map(|f| num(f)).unwrap_or(Value::Null), Y::Integer(i) => json!(i), Y::String(s) => json!(s), Y::Boolean(b) => json!(b), Y::Null | Y::BadValue => Value::Null, Y::Alias(_) => Value::Null,
        Y::Array(a) => Value::Array(a.iter().map(yaml_to_json).collect()),
        Y::Hash(h) => { let mut m = Map::new(); for (k, v) in h { let key = match k { Y::String(s) => s.clone(), Y::Integer(i) => i.to_string(), Y::Boolean(b) => b.to_string(), Y::Real(r) => r.clone(), _ => continue }; m.insert(key, yaml_to_json(v)); } Value::Object(m) } }
}
/// A YAML or JSON text as a JSON value (JSON is YAML), or why not. Pure.
pub fn parse_yaml(t: &str) -> Result<Value, String> { let d = yaml_rust2::YamlLoader::load_from_str(t).map_err(|e| e.to_string())?; Ok(match d.len() { 0 => Value::Null, 1 => yaml_to_json(&d[0]), _ => Value::Array(d.iter().map(yaml_to_json).collect()) }) }
fn read_doc(f: &mut Facts, t: &str, fmt: &str) {
    let parsed: Result<Value, String> = match fmt { "json" => serde_json::from_str::<Value>(t).map_err(|e| e.to_string()).or_else(|e| { let stripped: String = t.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n"); serde_json::from_str::<Value>(&stripped).map_err(|_| e) }), "yaml" => parse_yaml(t), _ => toml::from_str::<Value>(t).map_err(|e| e.to_string()) };
    match parsed {
        Err(e) => { f.attrs.insert("valid".into(), json!(false)); f.attrs.insert("parse_error".into(), json!(short(&e, 200))); }
        Ok(v) => { f.attrs.insert("valid".into(), json!(true)); let mut maxd = 0usize; walk_doc(f, &v, String::new(), 0, &mut maxd); f.attrs.insert("depth".into(), int(maxd)); f.attrs.insert("top_keys".into(), int(v.as_object().map_or(0, |o| o.len()))); f.doc = Some(v); }
    }
}
fn walk_doc(f: &mut Facts, v: &Value, path: String, depth: usize, maxd: &mut usize) {
    *maxd = (*maxd).max(depth); if f.units.len() >= 5000 || depth > 8 { return; }
    let (ty, len, text) = match v { Value::Null => ("null", 0, String::new()), Value::Bool(b) => ("bool", 0, b.to_string()), Value::Number(n) => ("number", 0, n.to_string()), Value::String(s) => ("string", s.chars().count(), short(s, 2000)), Value::Array(a) => ("array", a.len(), String::new()), Value::Object(o) => ("object", o.len(), String::new()) };
    if !path.is_empty() { push(f, "value", None, path.clone(), 0, text, lit(&[("type", json!(ty)), ("len", int(len))])); }
    match v { Value::Array(a) => for (i, x) in a.iter().enumerate().take(500) { walk_doc(f, x, format!("{path}[{i}]"), depth + 1, maxd); }, Value::Object(o) => for (k, x) in o.iter() { walk_doc(f, x, if path.is_empty() { k.clone() } else { format!("{path}.{k}") }, depth + 1, maxd); }, _ => {} }
}

// ── csv ─────────────────────────────────────────────────────────────────────────────────────
/// Rows of fields, quotes and doubled quotes honoured, `\n` inside quotes kept. Pure.
pub fn parse_csv(t: &str, delim: char) -> Vec<Vec<String>> {
    let mut rows = vec![]; let mut row: Vec<String> = vec![]; let mut field = String::new(); let mut q = false; let cs: Vec<char> = t.chars().collect(); let mut i = 0;
    while i < cs.len() { let c = cs[i];
        if q { if c == '"' { if i + 1 < cs.len() && cs[i + 1] == '"' { field.push('"'); i += 1; } else { q = false; } } else { field.push(c); } }
        else if c == '"' && field.is_empty() { q = true; } else if c == delim { row.push(std::mem::take(&mut field)); } else if c == '\n' { row.push(std::mem::take(&mut field)); rows.push(std::mem::take(&mut row)); } else { field.push(c); }
        i += 1; }
    if !field.is_empty() || !row.is_empty() { row.push(field); rows.push(row); }
    rows.retain(|r| !(r.len() == 1 && r[0].is_empty())); rows
}
fn read_csv(f: &mut Facts, t: &str, tsv: bool) {
    let rows = parse_csv(t, if tsv { '\t' } else { ',' }); if rows.is_empty() { f.attrs.insert("rows".into(), int(0)); f.attrs.insert("cols".into(), int(0)); f.attrs.insert("valid".into(), json!(false)); f.attrs.insert("parse_error".into(), json!("empty")); return; }
    let header = rows[0].clone(); let cols = header.len(); let body = &rows[1..]; let (mut empty, mut ragged) = (0usize, 0usize); let mut seen: BTreeMap<String, usize> = BTreeMap::new(); let mut dups = 0usize; let mut cells = 0usize;
    for (ri, r) in body.iter().enumerate() {
        if r.len() != cols { ragged += 1; } let key = r.join("\u{1f}"); let c = seen.entry(key).or_insert(0); *c += 1; if *c > 1 { dups += 1; }
        let e = r.iter().filter(|c| c.trim().is_empty()).count(); empty += e;
        push(f, "row", None, format!("row {}", ri + 1), ri + 2, String::new(), lit(&[("cells", int(r.len())), ("empty", int(e)), ("ragged", json!(r.len() != cols))]));
        for (ci, c) in r.iter().enumerate() { if cells >= 40_000 { break; } cells += 1; let name = header.get(ci).cloned().unwrap_or_else(|| format!("col{}", ci + 1)); let numeric = !c.trim().is_empty() && c.trim().replace(',', ".").parse::<f64>().is_ok();
            push(f, "cell", None, format!("row {} › {}", ri + 1, name), ri + 2, short(c, 500), lit(&[("col", json!(name)), ("empty", json!(c.trim().is_empty())), ("numeric", json!(numeric)), ("len", int(c.chars().count()))])); }
    }
    let a = &mut f.attrs; a.insert("valid".into(), json!(true)); a.insert("rows".into(), int(body.len())); a.insert("cols".into(), int(cols)); a.insert("header".into(), json!(header)); a.insert("empty_cells".into(), int(empty)); a.insert("ragged_rows".into(), int(ragged)); a.insert("duplicate_rows".into(), int(dups));
    let dupheaders = { let mut s: BTreeMap<&str, usize> = BTreeMap::new(); for h in &header { *s.entry(h.as_str()).or_insert(0) += 1; } s.values().filter(|c| **c > 1).count() }; a.insert("duplicate_headers".into(), int(dupheaders));
}

// ── images ──────────────────────────────────────────────────────────────────────────────────
fn set_dims(f: &mut Facts, fmt: &str, w: u64, h: u64) {
    let a = &mut f.attrs; a.insert("format".into(), json!(fmt)); a.insert("width".into(), json!(w)); a.insert("height".into(), json!(h));
    a.insert("aspect".into(), if h > 0 { num(((w as f64 / h as f64) * 100.0).round() / 100.0) } else { json!(0) }); a.insert("megapixels".into(), num(round1((w * h) as f64 / 1_000_000.0)));
    a.insert("orientation".into(), json!(if w == h { "square" } else if w > h { "landscape" } else { "portrait" }));
}
fn read_image(f: &mut Facts, b: &[u8]) {
    let be32 = |o: usize| b.get(o..o + 4).map(|x| u32::from_be_bytes([x[0], x[1], x[2], x[3]]) as u64); let le16 = |o: usize| b.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]]) as u64); let le32 = |o: usize| b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]) as u64);
    if b.starts_with(b"\x89PNG\r\n\x1a\n") { if let (Some(w), Some(h)) = (be32(16), be32(20)) { set_dims(f, "png", w, h); let ct = b.get(25).copied().unwrap_or(0); f.attrs.insert("has_alpha".into(), json!(ct == 4 || ct == 6)); return; } }
    else if b.starts_with(b"GIF8") { if let (Some(w), Some(h)) = (le16(6), le16(8)) { set_dims(f, "gif", w, h); return; } }
    else if b.starts_with(b"BM") { if let (Some(w), Some(h)) = (le32(18), le32(22)) { set_dims(f, "bmp", w, (h as i32).unsigned_abs() as u64); return; } }
    else if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        match b.get(12..16) { Some(b"VP8X") => { if let Some(x) = b.get(24..30) { set_dims(f, "webp", 1 + (x[0] as u64 | (x[1] as u64) << 8 | (x[2] as u64) << 16), 1 + (x[3] as u64 | (x[4] as u64) << 8 | (x[5] as u64) << 16)); return; } }
            Some(b"VP8 ") => { if let (Some(w), Some(h)) = (le16(26), le16(28)) { set_dims(f, "webp", w & 0x3fff, h & 0x3fff); return; } }
            Some(b"VP8L") => { if let Some(x) = b.get(21..25) { let v = u32::from_le_bytes([x[0], x[1], x[2], x[3]]) as u64; set_dims(f, "webp", (v & 0x3fff) + 1, ((v >> 14) & 0x3fff) + 1); return; } } _ => {} }
    }
    else if b.starts_with(b"\xff\xd8") { let mut i = 2; while i + 9 < b.len() { if b[i] != 0xFF { i += 1; continue; } let m = b[i + 1]; if m == 0xFF { i += 1; continue; } if (0xC0..=0xCF).contains(&m) && !matches!(m, 0xC4 | 0xC8 | 0xCC) { let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u64; let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u64; set_dims(f, "jpeg", w, h); return; } let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize; i += 2 + len.max(2); } }
    f.unread = Some("an image whose header could not be read".into());
}
fn read_svg(f: &mut Facts, t: &str) {
    let tag = regex::Regex::new(r"(?is)<svg\b[^>]*>").unwrap().find(t).map(|m| m.as_str().to_string()).unwrap_or_default(); if tag.is_empty() { f.unread = Some("not an SVG document".into()); return; }
    let px = |s: Option<String>| s.and_then(|v| v.trim().trim_end_matches("px").parse::<f64>().ok());
    let (mut w, mut h) = (px(attr_of(&tag, "width")), px(attr_of(&tag, "height")));
    if w.is_none() || h.is_none() { if let Some(vb) = attr_of(&tag, "viewBox") { let p: Vec<f64> = vb.split(|c: char| c == ' ' || c == ',').filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect(); if p.len() == 4 { w = w.or(Some(p[2])); h = h.or(Some(p[3])); } } }
    set_dims(f, "svg", w.unwrap_or(0.0).round() as u64, h.unwrap_or(0.0).round() as u64);
    f.attrs.insert("has_script".into(), json!(regex::Regex::new(r"(?i)<script\b").unwrap().is_match(t))); f.attrs.insert("elements".into(), int(regex::Regex::new(r"<[a-zA-Z]").unwrap().find_iter(t).count())); f.attrs.insert("has_title".into(), json!(regex::Regex::new(r"(?i)<title\b").unwrap().is_match(t)));
    f.text = Some(t.to_string());
}
