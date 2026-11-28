//! book-parse.rs — twin of skills/agents/in/book-parse.mjs. NAMES ARE CONTRACT: `parseEpubBuffer`,
//! `parsePdfBuffer` (named, no-db, pure — dispatched via the frontier's `sig:"no-db"` lane),
//! `bookParse` (default, db lane).
//!
//! `bookParse` reads `{KNOWLEDGE_DIR}/{doc.vault_path}` off the REAL filesystem. KNOWLEDGE_DIR
//! (roots.mjs) is a HARDCODED engine root (`ANTILOKI_DATA_DIR` env override, else
//! `ANTILOKI_SUBSTRATE_ROOT`/.antiloki, else self-derived from the JS module's own location) —
//! NOT a per-row DB value like `workspaces.root_path` — so per the crossing playbook this is
//! **bespoke-native**: proven via `#[cfg(test)]` real-fs tests (temp dir + env-var injection),
//! same convention as `knowledge-document-add.rs`'s `vault_root()` / `god-driver-halt.rs`'s
//! `halt_file()`. No conform-db fixture or frontier entry for `bookParse` itself.
//!
//! `parseEpubBuffer`/`parsePdfBuffer` touch no fs/db — pure buffer→text — so THEY ride the
//! standard no-db fixture lane (`buf` arg carries a `{"__bytes_b64"}` marker, decoded via the
//! `base64` crate already in Cargo.toml). Internally each wrapper delegates to a `_bytes`-suffixed
//! implementation taking idiomatic `&[u8]` / `&mut Vec<String>` so `bookParse` can reuse the SAME
//! parsing logic with real warnings collection (the dispatch-lane wrapper's `warnings` arg is
//! JS-parity-only: the JS callers of parseEpubBuffer/parsePdfBuffer never observe warnings via the
//! return value either — only bookParse threads them into its own output).
//!
//! ZIP: a minimal central-directory reader (stored=0 / deflate=8 via the already-present `flate2`
//! crate — DeflateDecoder for raw deflate entries, ZlibDecoder for PDF `/FlateDecode` streams).
//! `zipOpen` never validates CRC32, so neither does this twin.
//!
//! PDF: `buf.toString('latin1')` maps byte value → Unicode scalar of the SAME value (0..=255,
//! always a valid `char`) — `bytes.iter().map(|&b| b as char).collect::<String>()` is the exact
//! Rust equivalent. Structural scanning (stream/endstream/`<<`/`\bBT\b`/`/Title`) uses the `regex`
//! crate directly on that mapped `String`: byte offsets from `.find()`/`.captures()` are used only
//! to SLICE (never exposed numerically), and every structural marker is plain ASCII, so those
//! slice boundaries are always valid UTF-8 char boundaries regardless of what non-ASCII latin1
//! bytes appear elsewhere in the stream. The character-level text-operator walk
//! (`pdf_extract_from_content`/`pdf_decode_literal`) needs true random-access indexing, so it
//! operates on `Vec<char>` (one element per original byte) to stay index-aligned with the JS
//! `cs[i]` string-indexing it ports.
//!
//! Throws (`err.required`/`err.notFound`/`err.contextMissing`) reproduce the exact `AntilokiError`
//! message text, verified against a live `skills/engine/error.mjs` run. Fixture cases exercise the
//! SUCCESS paths only (playbook gotcha 9); the error strings are covered by native tests instead.
// QUALITY: 70/100 — zero-dependency-in-JS binary format ports (ZIP central directory, PDF
// text-operator walk); the honesty-gate PDF reject path and the EPUB spine/manifest ordering are
// both fixture-covered, but exotic malformed-archive/malformed-PDF inputs are not (guard-throws).

use base64::Engine;
use flate2::read::{DeflateDecoder, ZlibDecoder};
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// = JS Number(v) — used for bookParse's `Number(input?.document_id)`.
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) }
        }
        Some(_) => f64::NAN,
    }
}

// ─────────────────────────────── minimal ZIP reader ───────────────────────────────
struct ZipEntry { method: u16, csize: u32, lho: u32 }
struct Zip { buf: Vec<u8>, names: Vec<String>, map: HashMap<String, ZipEntry> }

fn u32le(buf: &[u8], off: usize) -> Option<u32> {
    if off + 4 > buf.len() { return None; }
    Some(u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]))
}
fn u16le(buf: &[u8], off: usize) -> Option<u16> {
    if off + 2 > buf.len() { return None; }
    Some(u16::from_le_bytes([buf[off], buf[off + 1]]))
}

fn inflate_raw(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut d = DeflateDecoder::new(data);
    let mut out = Vec::new();
    d.read_to_end(&mut out)?;
    Ok(out)
}
fn inflate_zlib(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut d = ZlibDecoder::new(data);
    let mut out = Vec::new();
    d.read_to_end(&mut out)?;
    Ok(out)
}

fn zip_open(buf: &[u8]) -> Option<Zip> {
    let len = buf.len() as i64;
    let lo = (len - 65557).max(0);
    let mut eocd: i64 = -1;
    let mut i = len - 22;
    while i >= lo {
        if u32le(buf, i as usize) == Some(0x06054b50) { eocd = i; break; }
        i -= 1;
    }
    if eocd < 0 { return None; }
    let eocd = eocd as usize;
    let count = u16le(buf, eocd + 10).unwrap_or(0) as u32;
    let mut off = u32le(buf, eocd + 16).unwrap_or(0) as usize;
    let mut names: Vec<String> = Vec::new();
    let mut map: HashMap<String, ZipEntry> = HashMap::new();
    for _ in 0..count {
        if off + 46 > buf.len() || u32le(buf, off) != Some(0x02014b50) { break; }
        let method = u16le(buf, off + 10).unwrap_or(0);
        let csize = u32le(buf, off + 20).unwrap_or(0);
        let name_len = u16le(buf, off + 28).unwrap_or(0) as usize;
        let extra_len = u16le(buf, off + 30).unwrap_or(0) as usize;
        let cmt_len = u16le(buf, off + 32).unwrap_or(0) as usize;
        let lho = u32le(buf, off + 42).unwrap_or(0);
        let name_start = (off + 46).min(buf.len());
        let name_end = (name_start + name_len).min(buf.len());
        let name = String::from_utf8_lossy(&buf[name_start..name_end]).into_owned();
        names.push(name.clone());
        map.insert(name, ZipEntry { method, csize, lho });
        off += 46 + name_len + extra_len + cmt_len;
    }
    Some(Zip { buf: buf.to_vec(), names, map })
}

fn zip_read(zip: &Zip, name: &str) -> Option<Vec<u8>> {
    let e = zip.map.get(name)?;
    let lho = e.lho as usize;
    if lho + 30 > zip.buf.len() { return None; }
    if u32le(&zip.buf, lho) != Some(0x04034b50) { return None; }
    let name_len = u16le(&zip.buf, lho + 26)? as usize;
    let extra_len = u16le(&zip.buf, lho + 28)? as usize;
    let start = (lho + 30 + name_len + extra_len).min(zip.buf.len());
    let end = (start + e.csize as usize).min(zip.buf.len()).max(start);
    let raw = &zip.buf[start..end];
    match e.method {
        0 => Some(raw.to_vec()),
        8 => inflate_raw(raw).ok(),
        _ => None,
    }
}

// ─────────────────────────── HTML → markdown-ish text ───────────────────────────
fn entity_lookup(n: &str) -> Option<&'static str> {
    match n {
        "amp" => Some("&"), "lt" => Some("<"), "gt" => Some(">"), "quot" => Some("\""),
        "apos" => Some("'"), "nbsp" => Some(" "), "mdash" => Some("\u{2014}"), "ndash" => Some("\u{2013}"),
        "hellip" => Some("\u{2026}"), "rsquo" => Some("\u{2019}"), "lsquo" => Some("\u{2018}"),
        "rdquo" => Some("\u{201d}"), "ldquo" => Some("\u{201c}"), "copy" => Some("\u{00a9}"),
        "reg" => Some("\u{00ae}"), "trade" => Some("\u{2122}"),
        _ => None,
    }
}

fn decode_entities(s: &str) -> String {
    static HEX_RE: OnceLock<Regex> = OnceLock::new();
    let hex_re = HEX_RE.get_or_init(|| Regex::new(r"(?i)&#x([0-9a-f]+);").unwrap());
    static DEC_RE: OnceLock<Regex> = OnceLock::new();
    let dec_re = DEC_RE.get_or_init(|| Regex::new(r"&#(\d+);").unwrap());
    static NAMED_RE: OnceLock<Regex> = OnceLock::new();
    let named_re = NAMED_RE.get_or_init(|| Regex::new(r"(?i)&([a-z]+);").unwrap());

    let mut out = hex_re.replace_all(s, |c: &regex::Captures| {
        u32::from_str_radix(&c[1], 16).ok().and_then(char::from_u32).map(|ch| ch.to_string()).unwrap_or_default()
    }).into_owned();
    out = dec_re.replace_all(&out, |c: &regex::Captures| {
        c[1].parse::<u32>().ok().and_then(char::from_u32).map(|ch| ch.to_string()).unwrap_or_default()
    }).into_owned();
    out = named_re.replace_all(&out, |c: &regex::Captures| {
        entity_lookup(&c[1].to_lowercase()).map(|s| s.to_string()).unwrap_or_else(|| c[0].to_string())
    }).into_owned();
    out
}

fn strip_inner_tags(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    re.replace_all(s, " ").trim().to_string()
}

// `/<(script|style)\b[\s\S]*?<\/\1>/gi` — the regex crate has no backreferences, so the matching
// tag name is walked by hand: find the next open tag, then require a SAME-NAME close tag before
// removing the span (JS: if no closing tag exists, the whole pattern fails to match at all, so the
// open tag survives untouched and gets stripped later by the generic `<[^>]+>` rule instead).
fn strip_script_style(input: &str) -> String {
    static OPEN_RE: OnceLock<Regex> = OnceLock::new();
    let open_re = OPEN_RE.get_or_init(|| Regex::new(r"(?i)<(script|style)\b[^>]*>").unwrap());
    let mut out = String::new();
    let mut pos = 0usize;
    loop {
        let caps = match open_re.captures_at(input, pos) { Some(c) => c, None => break };
        let m = caps.get(0).unwrap();
        let tag_name = caps.get(1).unwrap().as_str().to_lowercase();
        let close_pat = format!(r"(?i)</{}>", regex::escape(&tag_name));
        let close_re = Regex::new(&close_pat).unwrap();
        match close_re.find_at(input, m.end()) {
            Some(cm) => {
                out.push_str(&input[pos..m.start()]);
                out.push(' ');
                pos = cm.end();
            }
            None => break,
        }
    }
    out.push_str(&input[pos..]);
    out
}

fn html_to_text(html: &str) -> String {
    let mut t = strip_script_style(html);

    static COMMENT_RE: OnceLock<Regex> = OnceLock::new();
    t = COMMENT_RE.get_or_init(|| Regex::new(r"<!--[\s\S]*?-->").unwrap()).replace_all(&t, " ").into_owned();

    static H1_RE: OnceLock<Regex> = OnceLock::new();
    t = H1_RE.get_or_init(|| Regex::new(r"(?i)<h1[^>]*>([\s\S]*?)</h1>").unwrap())
        .replace_all(&t, |c: &regex::Captures| format!("\n\n# {}\n\n", strip_inner_tags(&c[1]))).into_owned();
    static H2_RE: OnceLock<Regex> = OnceLock::new();
    t = H2_RE.get_or_init(|| Regex::new(r"(?i)<h2[^>]*>([\s\S]*?)</h2>").unwrap())
        .replace_all(&t, |c: &regex::Captures| format!("\n\n## {}\n\n", strip_inner_tags(&c[1]))).into_owned();
    static H3_RE: OnceLock<Regex> = OnceLock::new();
    t = H3_RE.get_or_init(|| Regex::new(r"(?i)<h3[^>]*>([\s\S]*?)</h3>").unwrap())
        .replace_all(&t, |c: &regex::Captures| format!("\n\n### {}\n\n", strip_inner_tags(&c[1]))).into_owned();

    static LI_RE: OnceLock<Regex> = OnceLock::new();
    t = LI_RE.get_or_init(|| Regex::new(r"(?i)<li[^>]*>").unwrap()).replace_all(&t, "\n- ").into_owned();

    static BLOCK_OPEN_RE: OnceLock<Regex> = OnceLock::new();
    t = BLOCK_OPEN_RE.get_or_init(|| Regex::new(r"(?i)<(?:p|div|section|article|br|tr|blockquote|figcaption)\b[^>]*/?>").unwrap())
        .replace_all(&t, "\n").into_owned();
    static BLOCK_CLOSE_RE: OnceLock<Regex> = OnceLock::new();
    t = BLOCK_CLOSE_RE.get_or_init(|| Regex::new(r"(?i)</(?:p|div|section|article|tr|blockquote|li|ul|ol|table|figure)>").unwrap())
        .replace_all(&t, "\n").into_owned();

    static TAG_RE: OnceLock<Regex> = OnceLock::new();
    t = TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap()).replace_all(&t, " ").into_owned();

    t = decode_entities(&t);

    static SP_RE: OnceLock<Regex> = OnceLock::new();
    t = SP_RE.get_or_init(|| Regex::new(r"[ \t]+").unwrap()).replace_all(&t, " ").into_owned();
    static NLPAD_RE: OnceLock<Regex> = OnceLock::new();
    t = NLPAD_RE.get_or_init(|| Regex::new(r" ?\n ?").unwrap()).replace_all(&t, "\n").into_owned();
    static NL3_RE: OnceLock<Regex> = OnceLock::new();
    t = NL3_RE.get_or_init(|| Regex::new(r"\n{3,}").unwrap()).replace_all(&t, "\n\n").into_owned();

    t.trim().to_string()
}

// = JS decodeURIComponent for well-formed input (percent-decode UTF-8 byte sequences). JS throws
// URIError on malformed sequences (uncaught in book-parse.mjs); this lossily degrades instead —
// a guard-throw path never exercised by the fixture (playbook gotcha 9).
fn decode_uri_component(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ─────────────────────────────────── EPUB ───────────────────────────────────
struct ManifestItem { href: String, mt: String }

fn parse_epub_bytes(buf: &[u8], warnings: &mut Vec<String>) -> Result<(Option<String>, Option<String>, String), String> {
    let zip = zip_open(buf).ok_or_else(|| "book-parse: a valid EPUB (no ZIP directory found) is required".to_string())?;

    let container = zip_read(&zip, "META-INF/container.xml")
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    static FULLPATH_RE: OnceLock<Regex> = OnceLock::new();
    let fullpath_re = FULLPATH_RE.get_or_init(|| Regex::new(r#"full-path="([^"]+)""#).unwrap());
    let opf_path = fullpath_re.captures(&container).and_then(|c| c.get(1)).map(|m| m.as_str().to_string())
        .or_else(|| zip.names.iter().find(|n| n.to_lowercase().ends_with(".opf")).cloned())
        .ok_or_else(|| "book-parse: an OPF inside the EPUB is required".to_string())?;
    let opf = zip_read(&zip, &opf_path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();

    let strip = |x: Option<&str>| -> String {
        static TAG_RE: OnceLock<Regex> = OnceLock::new();
        let tag_re = TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
        decode_entities(&tag_re.replace_all(x.unwrap_or(""), "")).trim().to_string()
    };

    static TITLE_RE: OnceLock<Regex> = OnceLock::new();
    let title_re = TITLE_RE.get_or_init(|| Regex::new(r"(?i)<dc:title[^>]*>([\s\S]*?)</dc:title>").unwrap());
    let title_raw = title_re.captures(&opf).and_then(|c| c.get(1)).map(|m| m.as_str().to_string());
    let title = { let t = strip(title_raw.as_deref()); if t.is_empty() { None } else { Some(t) } };

    static CREATOR_RE: OnceLock<Regex> = OnceLock::new();
    let creator_re = CREATOR_RE.get_or_init(|| Regex::new(r"(?i)<dc:creator[^>]*>([\s\S]*?)</dc:creator>").unwrap());
    let author_raw = creator_re.captures(&opf).and_then(|c| c.get(1)).map(|m| m.as_str().to_string());
    let author = { let a = strip(author_raw.as_deref()); if a.is_empty() { None } else { Some(a) } };

    let base = match opf_path.rfind('/') { Some(idx) => opf_path[0..idx + 1].to_string(), None => String::new() };

    static ITEM_RE: OnceLock<Regex> = OnceLock::new();
    let item_re = ITEM_RE.get_or_init(|| Regex::new(r"(?i)<item\b[^>]*>").unwrap());
    static ID_RE: OnceLock<Regex> = OnceLock::new();
    let id_re = ID_RE.get_or_init(|| Regex::new(r#"\bid="([^"]+)""#).unwrap());
    static HREF_RE: OnceLock<Regex> = OnceLock::new();
    let href_re = HREF_RE.get_or_init(|| Regex::new(r#"\bhref="([^"]+)""#).unwrap());
    static MT_RE: OnceLock<Regex> = OnceLock::new();
    let mt_re = MT_RE.get_or_init(|| Regex::new(r#"media-type="([^"]+)""#).unwrap());

    let mut manifest_order: Vec<String> = Vec::new();
    let mut manifest: HashMap<String, ManifestItem> = HashMap::new();
    for m in item_re.find_iter(&opf) {
        let tag = m.as_str();
        let id = id_re.captures(tag).and_then(|c| c.get(1)).map(|x| x.as_str().to_string());
        let href = href_re.captures(tag).and_then(|c| c.get(1)).map(|x| x.as_str().to_string());
        let mt = mt_re.captures(tag).and_then(|c| c.get(1)).map(|x| x.as_str().to_string()).unwrap_or_default();
        if let (Some(id), Some(href)) = (id, href) {
            if !manifest.contains_key(&id) { manifest_order.push(id.clone()); }
            manifest.insert(id, ManifestItem { href, mt });
        }
    }

    static ITEMREF_RE: OnceLock<Regex> = OnceLock::new();
    let itemref_re = ITEMREF_RE.get_or_init(|| Regex::new(r#"(?i)<itemref\b[^>]*\bidref="([^"]+)""#).unwrap());
    let spine: Vec<String> = itemref_re.captures_iter(&opf).filter_map(|c| c.get(1).map(|m| m.as_str().to_string())).collect();
    let order: Vec<String> = if !spine.is_empty() { spine } else { manifest_order };

    static HTMLXML_RE: OnceLock<Regex> = OnceLock::new();
    let htmlxml_re = HTMLXML_RE.get_or_init(|| Regex::new(r"(?i)html|xml").unwrap());

    let mut parts: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for idref in &order {
        let it = match manifest.get(idref) { Some(x) => x, None => continue };
        if !it.mt.is_empty() && !htmlxml_re.is_match(&it.mt) { continue; }
        let href_no_frag = it.href.split('#').next().unwrap_or("");
        let decoded = decode_uri_component(href_no_frag);
        let joined = format!("{base}{decoded}");
        let mut segs: Vec<String> = Vec::new();
        for s in joined.split('/') {
            if s == ".." { segs.pop(); } else if !s.is_empty() && s != "." { segs.push(s.to_string()); }
        }
        let p = segs.join("/");
        if seen.contains(&p) { continue; }
        seen.insert(p.clone());
        let bytes = match zip_read(&zip, &p) {
            Some(b) => b,
            None => { warnings.push(format!("missing spine item: {p}")); continue; }
        };
        let text_owned = String::from_utf8_lossy(&bytes).into_owned();
        let text = html_to_text(&text_owned);
        if !text.is_empty() { parts.push(text); }
    }

    Ok((title, author, parts.join("\n\n")))
}

// ─────────────────────────────────── PDF ───────────────────────────────────
fn pdf_decode_literal(s: &[char]) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    let n = s.len();
    while i < n {
        let c = s[i];
        if c != '\\' { out.push(c); i += 1; continue; }
        let nc = s.get(i + 1).copied();
        i += 1;
        match nc {
            Some('n') => out.push('\n'),
            Some('r') | Some('t') => out.push(' '),
            Some('b') | Some('f') => {}
            Some(ch) if ('0'..='7').contains(&ch) => {
                let mut oct = String::new();
                oct.push(ch);
                while oct.len() < 3 {
                    match s.get(i + 1) {
                        Some(&d) if ('0'..='7').contains(&d) => { oct.push(d); i += 1; }
                        _ => break,
                    }
                }
                let val = u32::from_str_radix(&oct, 8).unwrap_or(0) % 256;
                if let Some(ch2) = char::from_u32(val) { out.push(ch2); }
            }
            Some('\n') | Some('\r') => {}
            Some(ch) => out.push(ch),
            None => {}
        }
        i += 1;
    }
    out
}

fn pdf_extract_from_content(cs: &[char]) -> String {
    let mut out = String::new();
    let mut pending: Vec<String> = Vec::new();
    let mut bracket: i32 = 0;
    let n = cs.len();
    let mut i = 0usize;
    while i < n {
        let c = cs[i];
        if c == '(' {
            let mut depth = 1i32;
            let mut j = i + 1;
            let mut raw: Vec<char> = Vec::new();
            while j < n && depth > 0 {
                let d = cs[j];
                if d == '\\' {
                    raw.push(d);
                    if let Some(&nx) = cs.get(j + 1) { raw.push(nx); }
                    j += 2;
                    continue;
                }
                if d == '(' { depth += 1; } else if d == ')' { depth -= 1; if depth == 0 { break; } }
                raw.push(d);
                j += 1;
            }
            pending.push(pdf_decode_literal(&raw));
            i = j + 1;
            continue;
        }
        if c == '<' && cs.get(i + 1) != Some(&'<') {
            match cs[(i + 1).min(n)..].iter().position(|&x| x == '>') {
                Some(rel) => {
                    let j = i + 1 + rel;
                    let hex: Vec<char> = cs[i + 1..j].iter().copied().filter(|c| c.is_ascii_hexdigit()).collect();
                    let mut s = String::new();
                    let mut k = 0usize;
                    while k + 1 < hex.len() {
                        let pair: String = hex[k..k + 2].iter().collect();
                        if let Ok(v) = u8::from_str_radix(&pair, 16) { s.push(v as char); }
                        k += 2;
                    }
                    pending.push(s);
                    i = j + 1;
                }
                None => { i += 1; }
            }
            continue;
        }
        if c == '[' { bracket += 1; i += 1; continue; }
        if c == ']' { bracket = (bracket - 1).max(0); i += 1; continue; }
        if bracket > 0 && (c == '-' || c.is_ascii_digit()) {
            let mut j = i + 1;
            while j < n && matches!(cs[j], '0'..='9' | '.' | '+' | '-') { j += 1; }
            let numstr: String = cs[i..j].iter().collect();
            if let Ok(num) = numstr.parse::<f64>() {
                let num = num.abs();
                if num.is_finite() && num >= 180.0 { pending.push(" ".to_string()); }
            }
            i = j;
            continue;
        }
        if c.is_ascii_alphabetic() || c == '\'' || c == '"' || c == '*' {
            let mut j = i;
            while j < n && (cs[j].is_ascii_alphabetic() || cs[j] == '\'' || cs[j] == '"' || cs[j] == '*') { j += 1; }
            let op: String = cs[i..j].iter().collect();
            if op == "Tj" || op == "TJ" { out.push_str(&pending.concat()); pending.clear(); }
            else if op == "'" || op == "\"" { out.push('\n'); out.push_str(&pending.concat()); pending.clear(); }
            else if op == "Td" || op == "TD" || op == "T*" || op == "ET" { out.push('\n'); pending.clear(); }
            else if op == "BT" { pending.clear(); }
            i = j;
            continue;
        }
        i += 1;
    }
    out
}

fn parse_pdf_bytes(buf: &[u8], warnings: &mut Vec<String>) -> (Option<String>, String) {
    let r: String = buf.iter().map(|&b| b as char).collect();

    static TITLE_RE: OnceLock<Regex> = OnceLock::new();
    let title_re = TITLE_RE.get_or_init(|| Regex::new(r"/Title\s*\(((?:\\.|[^\\)])*)\)").unwrap());
    let title = title_re.captures(&r).and_then(|c| c.get(1)).map(|m| {
        let chars: Vec<char> = m.as_str().chars().collect();
        pdf_decode_literal(&chars).trim().to_string()
    }).filter(|s| !s.is_empty());

    static STREAM_RE: OnceLock<Regex> = OnceLock::new();
    let stream_re = STREAM_RE.get_or_init(|| Regex::new(r"stream\r?\n").unwrap());
    static IMG_RE: OnceLock<Regex> = OnceLock::new();
    let img_re = IMG_RE.get_or_init(|| Regex::new(r"/(DCTDecode|JPXDecode|CCITTFaxDecode|JBIG2Decode)").unwrap());
    static FLATE_RE: OnceLock<Regex> = OnceLock::new();
    let flate_re = FLATE_RE.get_or_init(|| Regex::new(r"/FlateDecode").unwrap());
    static BT_RE: OnceLock<Regex> = OnceLock::new();
    let bt_re = BT_RE.get_or_init(|| Regex::new(r"\bBT\b").unwrap());

    let mut out = String::new();
    let mut streams = 0usize;
    for m in stream_re.find_iter(&r) {
        let start = m.end();
        let end = match r[start..].find("endstream") { Some(rel) => start + rel, None => continue };
        let bound = (m.start() + 2).min(r.len());
        let dict = match r[..bound].rfind("<<") { Some(ds) => &r[ds..m.start()], None => "" };
        if img_re.is_match(dict) { continue; }

        let mut seg = &r[start..end];
        if seg.ends_with("\r\n") { seg = &seg[..seg.len() - 2]; }
        else if seg.ends_with('\n') || seg.ends_with('\r') { seg = &seg[..seg.len() - 1]; }
        let mut data: Vec<u8> = seg.chars().map(|c| c as u8).collect();
        if flate_re.is_match(dict) {
            match inflate_zlib(&data) {
                Ok(d) => data = d,
                Err(_) => match inflate_raw(&data) {
                    Ok(d) => data = d,
                    Err(_) => continue,
                },
            }
        }
        let cs: String = data.iter().map(|&b| b as char).collect();
        if !bt_re.is_match(&cs) { continue; }
        streams += 1;
        let cs_chars: Vec<char> = cs.chars().collect();
        out.push_str(&pdf_extract_from_content(&cs_chars));
        out.push('\n');
    }

    static SP2_RE: OnceLock<Regex> = OnceLock::new();
    out = SP2_RE.get_or_init(|| Regex::new(r"[ \t]{2,}").unwrap()).replace_all(&out, " ").into_owned();
    static NL3_RE: OnceLock<Regex> = OnceLock::new();
    out = NL3_RE.get_or_init(|| Regex::new(r"\n{3,}").unwrap()).replace_all(&out, "\n\n").into_owned();
    out = out.trim().to_string();

    let mut ascii = 0i64;
    let mut ctrl = 0i64;
    for ch in out.chars() {
        let cp = ch as u32;
        if cp == 10 || cp == 9 || (32..=126).contains(&cp) { ascii += 1; }
        else if cp < 32 || (127..=159).contains(&cp) { ctrl += 1; }
    }
    let total = out.chars().count() as f64;
    let ascii_ratio = if total > 0.0 { ascii as f64 / total } else { 0.0 };
    let ctrl_ratio = if total > 0.0 { ctrl as f64 / total } else { 0.0 };
    if streams == 0 { warnings.push("no text-bearing content streams found".to_string()); }
    if total >= 200.0 && (ctrl_ratio > 0.05 || ascii_ratio < 0.5) {
        warnings.push(format!(
            "extracted text fails the printable gate ({}% ascii, {}% control bytes) \u{2014} CID/embedded-font encoding suspected",
            (ascii_ratio * 100.0).round() as i64,
            (ctrl_ratio * 100.0).round() as i64,
        ));
        return (title, String::new());
    }
    (title, out)
}

// ──────────────────────── dispatch-contract wrappers (no-db lane) ────────────────────────
fn decode_bytes(v: &Value) -> Vec<u8> {
    v.get("__bytes_b64").and_then(|s| s.as_str())
        .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
        .unwrap_or_default()
}

#[allow(non_snake_case)]
pub fn parseEpubBuffer(buf: &Value, _warnings: &Value) -> Result<Value, String> {
    let bytes = decode_bytes(buf);
    let mut warnings: Vec<String> = Vec::new();
    let (title, author, content) = parse_epub_bytes(&bytes, &mut warnings)?;
    let mut o = Map::new();
    o.insert("title".into(), title.map_or(Value::Null, Value::String));
    o.insert("author".into(), author.map_or(Value::Null, Value::String));
    o.insert("content".into(), json!(content));
    Ok(Value::Object(o))
}

#[allow(non_snake_case)]
pub fn parsePdfBuffer(buf: &Value, _warnings: &Value) -> Value {
    let bytes = decode_bytes(buf);
    let mut warnings: Vec<String> = Vec::new();
    let (title, content) = parse_pdf_bytes(&bytes, &mut warnings);
    let mut o = Map::new();
    o.insert("title".into(), title.map_or(Value::Null, Value::String));
    o.insert("author".into(), Value::Null);
    o.insert("content".into(), json!(content));
    Value::Object(o)
}

// ──────────────────────────── bookParse (bespoke-native) ────────────────────────────
fn is_windows_drive_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

// node:path resolve(base, rel) simplification — matches knowledge-document-add.rs's
// resolve_vault_path precedent (no ".."-collapsing; doc.vault_path is a stored, well-formed
// relative path in practice, never exercised with traversal segments).
fn resolve_vault_path(base: &Path, rel: &str) -> PathBuf {
    if rel.starts_with('/') || rel.starts_with('\\') || is_windows_drive_absolute(rel) {
        return PathBuf::from(rel);
    }
    let mut p = base.to_path_buf();
    for part in rel.split(['/', '\\']) {
        if part.is_empty() || part == "." { continue; }
        p.push(part);
    }
    p
}

// roots.mjs: KNOWLEDGE_DIR = join(DATA_DIR, 'knowledge'); DATA_DIR = ANTILOKI_DATA_DIR else
// SUBSTRATE_ROOT/.antiloki; SUBSTRATE_ROOT = ANTILOKI_SUBSTRATE_ROOT else self-derived from
// roots.mjs's own file location — inlined + env-read at call time (knowledge-document-add.rs
// `vault_root()` precedent), not imported from roots.mjs.
fn knowledge_dir() -> PathBuf {
    if let Ok(v) = std::env::var("ANTILOKI_DATA_DIR") {
        if !v.is_empty() { return PathBuf::from(v).join("knowledge"); }
    }
    if let Ok(v) = std::env::var("ANTILOKI_SUBSTRATE_ROOT") {
        if !v.is_empty() { return PathBuf::from(v).join(".antiloki").join("knowledge"); }
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("antiloki-core-rust has a parent dir")
        .join(".antiloki")
        .join("knowledge")
}

#[allow(non_snake_case)]
pub fn bookParse(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `if (!db) throw err.contextMissing('book-parse', 'db');` — unreachable here (`db` is a
    // required, non-nullable reference), same convention as every sibling twin.
    let document_id_num = js_number(input.get("document_id"));
    if !document_id_num.is_finite() || document_id_num.fract() != 0.0 || document_id_num <= 0.0 {
        return Err("book-parse: document_id is required".to_string());
    }
    let document_id = document_id_num as i64;

    let rows = crate::db_json::query_rows(db, "SELECT id, vault_path FROM knowledge.documents WHERE id = ?", &[json!(document_id)])?;
    let doc = match rows.as_array().and_then(|a| a.first()) {
        Some(d) => d.clone(),
        None => return Err(format!("book-parse: document '{document_id}' not found")),
    };
    let vault_path = doc.get("vault_path").and_then(|v| v.as_str()).unwrap_or("").to_string();

    let abs = resolve_vault_path(&knowledge_dir(), &vault_path);
    let buf = std::fs::read(&abs).map_err(|_| format!("book-parse: vault file '{vault_path}' not found"))?;

    let mut warnings: Vec<String> = Vec::new();
    let mut format = "text".to_string();
    let mut title: Option<String> = None;
    let mut author: Option<String> = None;
    let content: String;

    if buf.len() >= 4 && &buf[0..4] == b"%PDF" {
        format = "pdf".to_string();
        let (t, c) = parse_pdf_bytes(&buf, &mut warnings);
        title = t;
        content = c;
    } else if buf.len() >= 2 && buf[0] == 0x50 && buf[1] == 0x4b {
        format = "epub".to_string();
        let (t, a, c) = parse_epub_bytes(&buf, &mut warnings)?;
        title = t;
        author = a;
        content = c;
    } else {
        content = String::from_utf8_lossy(&buf).into_owned();
    }

    let content = content.replace('\u{0}', "").trim().to_string();
    let chars = content.encode_utf16().count() as i64;

    if chars < 400 {
        let error = if format == "pdf" {
            "could not extract a usable text layer \u{2014} this PDF is likely scanned or CID/embedded-font encoded; convert it to EPUB or TXT and re-upload"
        } else {
            "the extracted text is too short to be a book"
        };
        let mut o = Map::new();
        o.insert("ok".into(), json!(false));
        o.insert("format".into(), json!(format));
        o.insert("warnings".into(), json!(warnings));
        o.insert("error".into(), json!(error));
        return Ok(Value::Object(o));
    }

    let mut o = Map::new();
    o.insert("ok".into(), json!(true));
    o.insert("format".into(), json!(format));
    o.insert("title".into(), title.map_or(Value::Null, Value::String));
    o.insert("author".into(), author.map_or(Value::Null, Value::String));
    o.insert("content".into(), json!(content));
    o.insert("chars".into(), json!(chars));
    o.insert("warnings".into(), json!(warnings));
    Ok(Value::Object(o))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn v_bytes(b: &[u8]) -> Value {
        json!({ "__bytes_b64": base64::engine::general_purpose::STANDARD.encode(b) })
    }

    #[test]
    fn decode_entities_handles_named_hex_decimal_and_unknown() {
        assert_eq!(decode_entities("A &amp; B &#8212; C &#x2013; D &zzz;"), "A & B \u{2014} C \u{2013} D &zzz;");
    }

    #[test]
    fn html_to_text_strips_script_style_and_renders_headings_lists() {
        let html = "<html><head><style>.x{color:red}</style></head><body><h1>Title</h1><p>A&amp;B</p><ul><li>one</li><li>two</li></ul><script>evil()</script></body></html>";
        let t = html_to_text(html);
        assert!(t.starts_with("# Title"));
        assert!(t.contains("A&B"));
        assert!(t.contains("- one"));
        assert!(t.contains("- two"));
        assert!(!t.contains("evil()"));
        assert!(!t.contains("color:red"));
    }

    #[test]
    fn pdf_extract_handles_tj_kerning_hex_and_octal() {
        let cs: Vec<char> = "BT\n(Hello) Tj\n[(Wor)-250(ld)] TJ\n<48656C6C6F> Tj\n(Octal \\101\\102) Tj\n(line1) '\n(line2) \"\nET".chars().collect();
        let out = pdf_extract_from_content(&cs);
        assert_eq!(out, "HelloWor ldHelloOctal AB\nline1\nline2\n");
    }

    #[test]
    fn parse_pdf_bytes_extracts_title_and_text() {
        let pdf = b"%PDF-1.4\n1 0 obj\n<< /Title (Hi \\(there\\)) >>\nendobj\n4 0 obj\n<< /Length 20 >>\nstream\nBT\n(Body text) Tj\nET\nendstream\nendobj\n%%EOF";
        let mut warnings = Vec::new();
        let (title, content) = parse_pdf_bytes(pdf, &mut warnings);
        assert_eq!(title.as_deref(), Some("Hi (there)"));
        assert_eq!(content, "Body text");
        assert!(warnings.is_empty());
    }

    #[test]
    fn parse_pdf_bytes_no_bt_warns_and_returns_empty() {
        let pdf = b"%PDF-1.4\n4 0 obj\n<< /Length 5 >>\nstream\nhello\nendstream\nendobj\n%%EOF";
        let mut warnings = Vec::new();
        let (_title, content) = parse_pdf_bytes(pdf, &mut warnings);
        assert_eq!(content, "");
        assert_eq!(warnings, vec!["no text-bearing content streams found".to_string()]);
    }

    #[test]
    fn parse_epub_buffer_dispatch_wrapper_round_trips_via_bytes_marker() {
        // A ZIP too small/invalid to contain an EOCD record — exercises the error path via the
        // dispatch-contract wrapper's Result plumbing (not fixture-exercised per gotcha 9).
        let r = parseEpubBuffer(&v_bytes(b"not a zip"), &json!([]));
        assert_eq!(r, Err("book-parse: a valid EPUB (no ZIP directory found) is required".to_string()));
    }

    #[test]
    fn parse_pdf_buffer_dispatch_wrapper_decodes_bytes_marker() {
        let pdf = b"%PDF-1.4\n4 0 obj\n<< /Length 20 >>\nstream\nBT\n(Wrapped) Tj\nET\nendstream\nendobj\n%%EOF";
        let r = parsePdfBuffer(&v_bytes(pdf), &json!([]));
        assert_eq!(r["content"], json!("Wrapped"));
        assert_eq!(r["author"], Value::Null);
    }

    // ── bookParse: bespoke-native, real fs + real (in-memory) db ──
    fn with_data_dir<T>(data_dir: &str, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env_lock::ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ANTILOKI_DATA_DIR", data_dir); }
        let result = f();
        unsafe { std::env::remove_var("ANTILOKI_DATA_DIR"); }
        result
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-book-parse-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seeded(vault_path: &str) -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS knowledge;
             CREATE TABLE knowledge.documents (id INTEGER PRIMARY KEY AUTOINCREMENT, vault_path TEXT NOT NULL);",
        ).unwrap();
        c.execute("INSERT INTO knowledge.documents (id, vault_path) VALUES (1, ?1)", rusqlite::params![vault_path]).unwrap();
        c
    }

    #[test]
    fn book_parse_missing_document_id_is_required() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS knowledge; CREATE TABLE knowledge.documents (id INTEGER PRIMARY KEY, vault_path TEXT);").unwrap();
        assert_eq!(bookParse(&json!({}), &c), Err("book-parse: document_id is required".to_string()));
        assert_eq!(bookParse(&json!({ "document_id": 0 }), &c), Err("book-parse: document_id is required".to_string()));
    }

    #[test]
    fn book_parse_unknown_document_id_is_not_found() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS knowledge; CREATE TABLE knowledge.documents (id INTEGER PRIMARY KEY, vault_path TEXT);").unwrap();
        assert_eq!(bookParse(&json!({ "document_id": 99 }), &c), Err("book-parse: document '99' not found".to_string()));
    }

    #[test]
    fn book_parse_reads_plain_text_file_and_reports_too_short() {
        let dir = temp_dir("short-text");
        let vault = dir.join("knowledge");
        fs::create_dir_all(&vault).unwrap();
        fs::write(vault.join("notes.txt"), b"too short").unwrap();
        let c = seeded("notes.txt");
        let r = with_data_dir(dir.to_str().unwrap(), || bookParse(&json!({ "document_id": 1 }), &c)).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["format"], json!("text"));
        assert_eq!(r["error"], json!("the extracted text is too short to be a book"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn book_parse_reads_long_plain_text_file_successfully() {
        let dir = temp_dir("long-text");
        let vault = dir.join("knowledge");
        fs::create_dir_all(&vault).unwrap();
        let body = "word ".repeat(100); // 500 chars, clears the 400-char floor
        fs::write(vault.join("book.txt"), body.as_bytes()).unwrap();
        let c = seeded("book.txt");
        let r = with_data_dir(dir.to_str().unwrap(), || bookParse(&json!({ "document_id": 1 }), &c)).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["format"], json!("text"));
        assert_eq!(r["title"], Value::Null);
        assert_eq!(r["chars"], json!(body.trim().len() as i64));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn book_parse_missing_vault_file_is_not_found() {
        let dir = temp_dir("missing-file");
        fs::create_dir_all(dir.join("knowledge")).unwrap();
        let c = seeded("ghost.txt");
        let r = with_data_dir(dir.to_str().unwrap(), || bookParse(&json!({ "document_id": 1 }), &c));
        assert_eq!(r, Err("book-parse: vault file 'ghost.txt' not found".to_string()));
        let _ = fs::remove_dir_all(&dir);
    }
}
