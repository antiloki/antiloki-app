//! web-research.rs — twin of skills/codebase/_helpers/web-research.mjs (named-exports-only
//! helper, all no-db). NAMES ARE CONTRACT: hashKey · inChunks · parseSemver · behindScore ·
//! htmlToText · extractArticle · parseFeed · sortDateDesc · stripHtml · extractDossier ·
//! githubRepo. Pure deterministic text/number parsing — no AI, no network despite the filename.
//!
//! `inChunks` is TRANSITIVE: its third JS parameter is a live callback, not a JSON value, so it
//! has no fixture/arm — the `pub fn` here exists only so the crate compiles + names_ok holds,
//! taking a real Rust closure in the callback's place.
//!
//! `js_date_parse` reimplements `Date.parse` for the two date families RSS/Atom feeds actually
//! emit — ISO 8601 (`2024-10-02T15:00:00Z`) and RFC 822/1123 (`Wed, 02 Oct 2024 15:00:00 GMT`).
//! It is not a full ECMA-262 Date.parse (no US zone abbreviations, no "Month DD, YYYY" prose
//! form) — unparseable input returns None, mirrored by the caller as NaN → 0, same as JS.
//! DIVERGENCE (disclosed, deterministic-by-design): an ISO date/time string with no explicit
//! offset (e.g. `2024-10-02T15:00:00`, no trailing `Z`/`±HH:MM`) is treated as UTC. Real
//! `Date.parse` in a JS host treats an offset-less **date-time** as *local* time (only a
//! bare date `2024-10-02` is UTC per spec) — a host-timezone-dependent result, which is exactly
//! what a deterministic no-host-clock Rust twin cannot reproduce. Not exercised by any RSS/Atom
//! fixture in practice (feeds always carry an explicit zone), so this is a reasonable, disclosed
//! simplification rather than a live bug.
//!
//! DIVERGENCE (disclosed, out-of-contract): `sortDateDesc`'s JS twin does `[...items].sort(...)`,
//! and JS spread also accepts a *string* (iterates Unicode scalars) or an array containing
//! non-plain-object elements (`{...it}` on those degrades to indexed-char/empty objects). No real
//! call site in antiloki-core/skills/codebase ever passes anything but a plain `[]`-seeded array
//! of parseFeed-shaped objects into `sortDateDesc` (grep confirms: magazine-observe.mjs always
//! does `let/const all = []; all.push(...parseFeed(...))`), so this Rust twin treats non-array
//! `items` as an error and array elements that aren't plain objects as an empty object — matching
//! JS behavior for every reachable call, diverging only for inputs outside the real contract.
//!
//! Truncation slices (`extractArticle`'s `ARTICLE_CAP`, `parseFeed`'s title/link/content caps,
//! `extractDossier`'s title/description caps) all route through `take_chars`, which walks Unicode
//! *scalar values* (`.chars()`), not UTF-16 code units like JS `.slice()`. For any text containing
//! astral-plane characters (rare in scraped article/feed text) the cut point can differ by a
//! surrogate pair; this is the same disclosed approximation everywhere it's used, called out at
//! each call site rather than only once. Length *checks* (as opposed to slices) are not
//! approximated the same way — see `extract_dossier_str`'s claims filter, which counts UTF-16
//! code units via `encode_utf16().count()` to match `t.length` exactly.

use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

fn str_qq(v: Option<&Value>) -> String {
    // String(v ?? '')
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    // JS `${v}`
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    // JS Number(v)
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } },
        Some(_) => f64::NAN,
    }
}
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() } // JS .slice(0,n) (BMP-equivalent)

const ARTICLE_CAP: usize = 4000;

// ── hashKey: FNV-1a over UTF-16 code units → base36 ─────────────────────────
pub fn hashKey(text: &Value) -> Value {
    let s = str_qq(Some(text));
    let mut h: u32 = 2166136261;
    for u in s.encode_utf16() {
        h ^= u as u32;
        h = h.wrapping_mul(16777619);
    }
    json!(to_base36(h))
}

fn to_base36(mut n: u32) -> String {
    if n == 0 { return "0".to_string(); }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut buf = Vec::new();
    while n > 0 { buf.push(DIGITS[(n % 36) as usize]); n /= 36; }
    buf.reverse();
    String::from_utf8(buf).unwrap()
}

// ── inChunks: TRANSITIVE — see module doc. Preserves the JS chunk-then-await-all order. ────
pub fn inChunks<F: FnMut(&Value) -> Value>(items: &[Value], size: usize, mut f: F) -> Vec<Value> {
    let mut out = Vec::new();
    if size == 0 { return out; } // JS: `i += 0` never advances — an infinite loop, not reproducible
    let mut i = 0;
    while i < items.len() {
        let end = (i + size).min(items.len());
        for it in &items[i..end] { out.push(f(it)); }
        i += size;
    }
    out
}

// ── semver distance ──────────────────────────────────────────────────────────
pub fn parseSemver(v: &Value) -> Value {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(\d+)\.(\d+)\.(\d+)").unwrap());
    let s = str_qq(Some(v));
    match re.captures(&s) {
        Some(c) => json!([
            c[1].parse::<i64>().unwrap_or(0),
            c[2].parse::<i64>().unwrap_or(0),
            c[3].parse::<i64>().unwrap_or(0)
        ]),
        None => Value::Null,
    }
}

pub fn behindScore(cur: &Value, latest: &Value) -> Value {
    let a = parseSemver(cur);
    let b = parseSemver(latest);
    let (a, b) = match (a.as_array(), b.as_array()) {
        (Some(a), Some(b)) => (a, b),
        _ => return json!(0),
    };
    let av: Vec<i64> = a.iter().map(|x| x.as_i64().unwrap_or(0)).collect();
    let bv: Vec<i64> = b.iter().map(|x| x.as_i64().unwrap_or(0)).collect();
    let d_maj = (bv[0] - av[0]).max(0);
    let d_min = (bv[1] - av[1]).max(0);
    let d_pat = (bv[2] - av[2]).max(0);
    if d_maj > 0 { return json!((40 + d_maj * 20).min(100)); }
    if d_min > 0 { return json!((8 + d_min * 4).min(39)); }
    json!(d_pat.min(7))
}

// ── HTML/XML → readable plain text ───────────────────────────────────────────
fn html_to_text_str(input: &str) -> String {
    static CDATA: OnceLock<Regex> = OnceLock::new();
    static APOS: OnceLock<Regex> = OnceLock::new();
    static SCRIPT: OnceLock<Regex> = OnceLock::new();
    static STYLE: OnceLock<Regex> = OnceLock::new();
    static BLOCK: OnceLock<Regex> = OnceLock::new();
    static LI: OnceLock<Regex> = OnceLock::new();
    static TAG: OnceLock<Regex> = OnceLock::new();
    static WS: OnceLock<Regex> = OnceLock::new();
    static NL: OnceLock<Regex> = OnceLock::new();
    static NL3: OnceLock<Regex> = OnceLock::new();
    let cdata = CDATA.get_or_init(|| Regex::new(r"<!\[CDATA\[([\s\S]*?)\]\]>").unwrap());
    let apos = APOS.get_or_init(|| Regex::new(r"&#39;|&apos;").unwrap());
    let script = SCRIPT.get_or_init(|| Regex::new(r"(?i)<script[\s\S]*?</script>").unwrap());
    let style = STYLE.get_or_init(|| Regex::new(r"(?i)<style[\s\S]*?</style>").unwrap());
    let block = BLOCK.get_or_init(|| Regex::new(r"(?i)<(?:br|/p|/div|/li|/h[1-6]|/tr|/blockquote)[^>]*>").unwrap());
    let li = LI.get_or_init(|| Regex::new(r"(?i)<li[^>]*>").unwrap());
    let tag = TAG.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    let ws = WS.get_or_init(|| Regex::new(r"[ \t]+").unwrap());
    let nl = NL.get_or_init(|| Regex::new(r" ?\n ?").unwrap());
    let nl3 = NL3.get_or_init(|| Regex::new(r"\n{3,}").unwrap());

    let s = cdata.replace_all(input, "$1");
    let s = s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"");
    let s = apos.replace_all(&s, "'").into_owned();
    let s = s.replace("&nbsp;", " ").replace("&amp;", "&");
    let s = script.replace_all(&s, " ").into_owned();
    let s = style.replace_all(&s, " ").into_owned();
    let s = block.replace_all(&s, "\n").into_owned();
    let s = li.replace_all(&s, "\n· ").into_owned();
    let s = tag.replace_all(&s, " ").into_owned();
    let s = s.replace("&amp;", "&"); // second amp-only sweep: double-escaped text resolves fully
    let s = ws.replace_all(&s, " ").into_owned();
    let s = nl.replace_all(&s, "\n").into_owned();
    let s = nl3.replace_all(&s, "\n\n").into_owned();
    s.trim().to_string()
}

pub fn htmlToText(html: &Value) -> Value { json!(html_to_text_str(&str_qq(Some(html)))) }

pub fn extractArticle(html: &Value) -> Value {
    static ART: OnceLock<Regex> = OnceLock::new();
    static MAIN: OnceLock<Regex> = OnceLock::new();
    let art = ART.get_or_init(|| Regex::new(r"(?i)<article[\s>][\s\S]*?</article>").unwrap());
    let main = MAIN.get_or_init(|| Regex::new(r"(?i)<main[\s>][\s\S]*?</main>").unwrap());
    let src = str_qq(Some(html));
    let scope: &str = art.find(&src).map(|m| m.as_str())
        .or_else(|| main.find(&src).map(|m| m.as_str()))
        .unwrap_or(&src);
    // take_chars ≈ JS .slice(0, ARTICLE_CAP): chars() vs UTF-16 code units, see module doc.
    json!(take_chars(&html_to_text_str(scope), ARTICLE_CAP))
}

// ── RSS/Atom feed parsing ────────────────────────────────────────────────────
fn unescape_xml(t: Option<&str>) -> String {
    static CDATA: OnceLock<Regex> = OnceLock::new();
    static APOS: OnceLock<Regex> = OnceLock::new();
    static TAG: OnceLock<Regex> = OnceLock::new();
    static WS: OnceLock<Regex> = OnceLock::new();
    let cdata = CDATA.get_or_init(|| Regex::new(r"<!\[CDATA\[([\s\S]*?)\]\]>").unwrap());
    let apos = APOS.get_or_init(|| Regex::new(r"&#39;|&apos;").unwrap());
    let tag = TAG.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    let ws = WS.get_or_init(|| Regex::new(r"\s+").unwrap());

    let s = t.unwrap_or("");
    let s = cdata.replace_all(s, "$1");
    let s = s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"");
    let s = apos.replace_all(&s, "'").into_owned();
    let s = s.replace("&amp;", "&");
    let s = tag.replace_all(&s, " ").into_owned();
    ws.replace_all(&s, " ").trim().to_string()
}

fn cap1(re: &Regex, s: &str) -> Option<String> {
    re.captures(s).and_then(|c| c.get(1)).map(|m| m.as_str().to_string())
}
fn cap1_or(re1: &Regex, re2: &Regex, s: &str) -> Option<String> {
    if let Some(c) = re1.captures(s) { return Some(c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default()); }
    if let Some(c) = re2.captures(s) { return Some(c.get(1).map(|m| m.as_str().to_string()).unwrap_or_default()); }
    None
}

fn feed_item_kind(blk: &str, link: &str) -> &'static str {
    static ENC: OnceLock<Regex> = OnceLock::new();
    static YT_LINK: OnceLock<Regex> = OnceLock::new();
    static YT_ID: OnceLock<Regex> = OnceLock::new();
    let enc = ENC.get_or_init(|| Regex::new(r#"(?i)<enclosure[^>]+type=["']audio/"#).unwrap());
    if enc.is_match(blk) { return "podcast"; }
    let yt_link = YT_LINK.get_or_init(|| Regex::new(r"(?i)youtube\.com/watch|youtu\.be/").unwrap());
    let yt_id = YT_ID.get_or_init(|| Regex::new(r"(?i)<yt:videoId>").unwrap());
    if yt_link.is_match(link) || yt_id.is_match(blk) { return "video"; }
    "post"
}

fn rss_item(blk: &str, source: &Value) -> Option<Value> {
    static TITLE: OnceLock<Regex> = OnceLock::new();
    static LINK: OnceLock<Regex> = OnceLock::new();
    static PUBDATE: OnceLock<Regex> = OnceLock::new();
    static DCDATE: OnceLock<Regex> = OnceLock::new();
    static CENC: OnceLock<Regex> = OnceLock::new();
    static DESC: OnceLock<Regex> = OnceLock::new();
    let title_re = TITLE.get_or_init(|| Regex::new(r"(?i)<title[^>]*>([\s\S]*?)</title>").unwrap());
    let link_re = LINK.get_or_init(|| Regex::new(r"(?i)<link[^>]*>([\s\S]*?)</link>").unwrap());
    let pubdate_re = PUBDATE.get_or_init(|| Regex::new(r"(?i)<pubDate[^>]*>([\s\S]*?)</pubDate>").unwrap());
    let dcdate_re = DCDATE.get_or_init(|| Regex::new(r"(?i)<dc:date[^>]*>([\s\S]*?)</dc:date>").unwrap());
    let cenc_re = CENC.get_or_init(|| Regex::new(r"(?i)<content:encoded[^>]*>([\s\S]*?)</content:encoded>").unwrap());
    let desc_re = DESC.get_or_init(|| Regex::new(r"(?i)<description[^>]*>([\s\S]*?)</description>").unwrap());

    let title = unescape_xml(cap1(title_re, blk).as_deref());
    if title.is_empty() { return None; }
    let link = unescape_xml(cap1(link_re, blk).as_deref());
    let date = unescape_xml(cap1_or(pubdate_re, dcdate_re, blk).as_deref());
    let raw_content = cap1_or(cenc_re, desc_re, blk);
    // take_chars ≈ JS .slice(0, ARTICLE_CAP): chars() vs UTF-16 code units, see module doc.
    let content = take_chars(&html_to_text_str(raw_content.as_deref().unwrap_or("")), ARTICLE_CAP);
    let kind = feed_item_kind(blk, &link);

    let mut obj = serde_json::Map::new();
    // take_chars ≈ JS .slice(0, n): chars() vs UTF-16 code units, see module doc.
    obj.insert("title".into(), json!(take_chars(&title, 200)));
    obj.insert("link".into(), json!(take_chars(&link, 500)));
    obj.insert("date".into(), json!(date));
    obj.insert("source".into(), source.clone());
    obj.insert("content".into(), json!(content));
    obj.insert("kind".into(), json!(kind));
    Some(Value::Object(obj))
}

fn atom_entry(blk: &str, source: &Value) -> Option<Value> {
    static TITLE: OnceLock<Regex> = OnceLock::new();
    static LINKHREF: OnceLock<Regex> = OnceLock::new();
    static UPDATED: OnceLock<Regex> = OnceLock::new();
    static PUBLISHED: OnceLock<Regex> = OnceLock::new();
    static CONTENT: OnceLock<Regex> = OnceLock::new();
    static SUMMARY: OnceLock<Regex> = OnceLock::new();
    let title_re = TITLE.get_or_init(|| Regex::new(r"(?i)<title[^>]*>([\s\S]*?)</title>").unwrap());
    let linkhref_re = LINKHREF.get_or_init(|| Regex::new(r#"(?i)<link[^>]*href="([^"]+)""#).unwrap());
    let updated_re = UPDATED.get_or_init(|| Regex::new(r"(?i)<updated[^>]*>([\s\S]*?)</updated>").unwrap());
    let published_re = PUBLISHED.get_or_init(|| Regex::new(r"(?i)<published[^>]*>([\s\S]*?)</published>").unwrap());
    let content_re = CONTENT.get_or_init(|| Regex::new(r"(?i)<content[^>]*>([\s\S]*?)</content>").unwrap());
    let summary_re = SUMMARY.get_or_init(|| Regex::new(r"(?i)<summary[^>]*>([\s\S]*?)</summary>").unwrap());

    let title = unescape_xml(cap1(title_re, blk).as_deref());
    if title.is_empty() { return None; }
    let link = unescape_xml(cap1(linkhref_re, blk).as_deref());
    let date = unescape_xml(cap1_or(updated_re, published_re, blk).as_deref());
    let raw_content = cap1_or(content_re, summary_re, blk);
    // take_chars ≈ JS .slice(0, ARTICLE_CAP): chars() vs UTF-16 code units, see module doc.
    let content = take_chars(&html_to_text_str(raw_content.as_deref().unwrap_or("")), ARTICLE_CAP);
    let kind = feed_item_kind(blk, &link);

    let mut obj = serde_json::Map::new();
    // take_chars ≈ JS .slice(0, n): chars() vs UTF-16 code units, see module doc.
    obj.insert("title".into(), json!(take_chars(&title, 200)));
    obj.insert("link".into(), json!(take_chars(&link, 500)));
    obj.insert("date".into(), json!(date));
    obj.insert("source".into(), source.clone());
    obj.insert("content".into(), json!(content));
    obj.insert("kind".into(), json!(kind));
    Some(Value::Object(obj))
}

pub fn parseFeed(xml: &Value, source: &Value) -> Value {
    static ITEM: OnceLock<Regex> = OnceLock::new();
    static ENTRY: OnceLock<Regex> = OnceLock::new();
    let item_re = ITEM.get_or_init(|| Regex::new(r"(?i)<item[\s>][\s\S]*?</item>").unwrap());
    let entry_re = ENTRY.get_or_init(|| Regex::new(r"(?i)<entry[\s>][\s\S]*?</entry>").unwrap());
    let xml_s = str_qq(Some(xml));

    let mut items: Vec<Value> = Vec::new();
    for m in item_re.find_iter(&xml_s) {
        if let Some(it) = rss_item(m.as_str(), source) { items.push(it); }
    }
    if !items.is_empty() { return json!(items); }
    for m in entry_re.find_iter(&xml_s) {
        if let Some(it) = atom_entry(m.as_str(), source) { items.push(it); }
    }
    json!(items)
}

// ── date-sorted lists ─────────────────────────────────────────────────────────
fn month_num(name: &str) -> Option<i64> {
    Some(match name.to_ascii_lowercase().as_str() {
        "jan" => 1, "feb" => 2, "mar" => 3, "apr" => 4, "may" => 5, "jun" => 6,
        "jul" => 7, "aug" => 8, "sep" => 9, "oct" => 10, "nov" => 11, "dec" => 12,
        _ => return None,
    })
}

// days since 1970-01-01 for a proleptic-Gregorian y/m/d (m in 1..=12) — Howard Hinnant's algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
// JS `new Date(ms).toISOString().slice(0,10)`
fn iso_date10_from_ms(ms: i64) -> String {
    let day = ms.div_euclid(86_400_000);
    let (y, m, d) = civil_from_days(day);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn tz_offset_minutes(t: &str, zero_words: &[&str]) -> i64 {
    if zero_words.iter().any(|w| w.eq_ignore_ascii_case(t)) { return 0; }
    let sign = if t.starts_with('-') { -1 } else { 1 };
    let digits: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 4 {
        let hh: i64 = digits[0..2].parse().unwrap_or(0);
        let mm: i64 = digits[2..4].parse().unwrap_or(0);
        sign * (hh * 60 + mm)
    } else { 0 }
}

// Reimplements Date.parse for ISO 8601 and RFC 822/1123 — the two families RSS/Atom emit. See
// module doc for scope. Returns None (→ NaN → 0 at the call site) for anything else.
// pub(crate) so magazine-observe's release-cadence window (`Date.parse(it.date) > cutoff`) uses
// the SAME parser sortDateDesc does — two date parsers in one crate would drift.
pub(crate) fn js_date_parse(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() { return None; }

    static ISO: OnceLock<Regex> = OnceLock::new();
    let iso_re = ISO.get_or_init(|| Regex::new(
        r"^(\d{4})-(\d{2})-(\d{2})(?:[T ](\d{2}):(\d{2})(?::(\d{2})(?:\.(\d{1,9}))?)?\s*(Z|[+-]\d{2}:?\d{2})?)?$"
    ).unwrap());
    if let Some(c) = iso_re.captures(s) {
        let y: i64 = c[1].parse().ok()?;
        let mo: i64 = c[2].parse().ok()?;
        let d: i64 = c[3].parse().ok()?;
        if !(1..=12).contains(&mo) || d < 1 || d > 31 { return None; }
        let h: i64 = c.get(4).map_or(0, |m| m.as_str().parse().unwrap_or(0));
        let mi: i64 = c.get(5).map_or(0, |m| m.as_str().parse().unwrap_or(0));
        let se: i64 = c.get(6).map_or(0, |m| m.as_str().parse().unwrap_or(0));
        let frac = c.get(7).map(|m| m.as_str()).unwrap_or("");
        let ms: i64 = if frac.is_empty() { 0 } else {
            let padded = format!("{:0<3}", &frac[..frac.len().min(3)]);
            padded.parse().unwrap_or(0)
        };
        // DIVERGENCE (disclosed, see module doc): group 8 absent (no `Z`/offset) → offset_min=0,
        // i.e. treated as UTC. Real Date.parse treats an offset-less date-*time* as local time;
        // not reproducible deterministically without a host clock, and no RSS/Atom fixture omits
        // the zone in practice.
        let offset_min = c.get(8).map_or(0, |m| tz_offset_minutes(m.as_str(), &["Z", "z"]));
        let day_number = days_from_civil(y, mo, d);
        return Some(day_number * 86_400_000 + h * 3_600_000 + mi * 60_000 + se * 1000 + ms - offset_min * 60_000);
    }

    static RFC: OnceLock<Regex> = OnceLock::new();
    let rfc_re = RFC.get_or_init(|| Regex::new(
        r"(?i)^(?:[a-z]{3},\s*)?(\d{1,2})\s+([a-z]{3})\s+(\d{2,4})\s+(\d{2}):(\d{2})(?::(\d{2}))?\s*(GMT|UTC|UT|Z|[+-]\d{4}|[+-]\d{2}:\d{2})?$"
    ).unwrap());
    if let Some(c) = rfc_re.captures(s) {
        let d: i64 = c[1].parse().ok()?;
        let mo = month_num(&c[2])?;
        let mut y: i64 = c[3].parse().ok()?;
        if y < 100 { y += if y < 70 { 2000 } else { 1900 }; }
        let h: i64 = c[4].parse().ok()?;
        let mi: i64 = c[5].parse().ok()?;
        let se: i64 = c.get(6).map_or(0, |m| m.as_str().parse().unwrap_or(0));
        let offset_min = c.get(7).map_or(0, |m| tz_offset_minutes(m.as_str(), &["GMT", "UTC", "UT", "Z"]));
        let day_number = days_from_civil(y, mo, d);
        return Some(day_number * 86_400_000 + h * 3_600_000 + mi * 60_000 + se * 1000 - offset_min * 60_000);
    }
    None
}

fn slice_end(cap: Option<&Value>, len: usize) -> usize {
    match cap {
        None | Some(Value::Null) => len, // JS .slice(0, undefined) === whole array
        Some(v) => {
            let n = js_number(Some(v));
            if n.is_nan() { return 0; }
            if n.is_infinite() { return if n > 0.0 { len } else { 0 }; }
            let n = n.trunc() as i64;
            if n < 0 { (len as i64 + n).max(0) as usize } else { (n as usize).min(len) }
        }
    }
}

// `items` must be an array — matches JS's `[...items]` throwing for non-iterables, for every
// reachable call site. NOT an exact JS twin: JS spread also iterates a *string* (Unicode scalars,
// each becoming `{0: char}` under `{...it}`) and tolerates non-plain-object array *elements* the
// same degenerate way — out-of-contract divergences, disclosed in the module doc, because no real
// caller (grep -rn "sortDateDesc" antiloki-core/skills) ever passes anything but a plain array of
// parseFeed-shaped objects.
pub fn sortDateDesc(items: &Value, cap: &Value) -> Result<Value, String> {
    let mut out = match items {
        Value::Array(a) => a.clone(),
        _ => return Err("items is not iterable".to_string()),
    };
    out.sort_by(|a, b| {
        let da = js_date_parse(&tmpl(a.get("date"))).unwrap_or(0);
        let db = js_date_parse(&tmpl(b.get("date"))).unwrap_or(0);
        db.cmp(&da)
    });
    let end = slice_end(Some(cap), out.len());
    out.truncate(end);
    let mapped: Vec<Value> = out.into_iter().map(|it| {
        // JS `{...it}` on a non-plain-object element (e.g. a string) spreads indexed-char
        // properties (`{0:'a', 1:'b', ...}`), not `{}`. Same disclosed out-of-contract
        // divergence as the array-only check above — real elements are always parseFeed-shaped
        // plain objects, so this only differs from JS for inputs no caller ever produces.
        let mut obj = it.as_object().cloned().unwrap_or_default();
        let date_v = obj.get("date").cloned();
        let new_date = if truthy(date_v.as_ref()) {
            let ms = js_date_parse(&tmpl(date_v.as_ref())).unwrap_or(0);
            json!(iso_date10_from_ms(ms))
        } else {
            json!("")
        };
        obj.insert("date".to_string(), new_date);
        Value::Object(obj)
    }).collect();
    Ok(json!(mapped))
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

// ── HTML page → dossier ──────────────────────────────────────────────────────
fn strip_html_str(s: &str) -> String {
    static SCRIPT: OnceLock<Regex> = OnceLock::new();
    static STYLE: OnceLock<Regex> = OnceLock::new();
    static TAG: OnceLock<Regex> = OnceLock::new();
    static WS: OnceLock<Regex> = OnceLock::new();
    let script = SCRIPT.get_or_init(|| Regex::new(r"(?i)<script[\s\S]*?</script>").unwrap());
    let style = STYLE.get_or_init(|| Regex::new(r"(?i)<style[\s\S]*?</style>").unwrap());
    let tag = TAG.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    let ws = WS.get_or_init(|| Regex::new(r"\s+").unwrap());

    let out = script.replace_all(s, " ");
    let out = style.replace_all(&out, " ").into_owned();
    let out = tag.replace_all(&out, " ").into_owned();
    let out = out.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">");
    ws.replace_all(&out, " ").trim().to_string()
}

pub fn stripHtml(html: &Value) -> Value { json!(strip_html_str(&str_qq(Some(html)))) }

fn extract_dossier_str(body: &str) -> Value {
    static TITLE: OnceLock<Regex> = OnceLock::new();
    static META1: OnceLock<Regex> = OnceLock::new();
    static META2: OnceLock<Regex> = OnceLock::new();
    static HTAG: OnceLock<Regex> = OnceLock::new();
    let title_re = TITLE.get_or_init(|| Regex::new(r"(?i)<title[^>]*>([\s\S]*?)</title>").unwrap());
    let meta1_re = META1.get_or_init(|| Regex::new(r#"(?i)<meta[^>]+name=["']description["'][^>]+content=["']([^"']+)["']"#).unwrap());
    let meta2_re = META2.get_or_init(|| Regex::new(r#"(?i)<meta[^>]+content=["']([^"']+)["'][^>]+name=["']description["']"#).unwrap());
    let htag_re = HTAG.get_or_init(|| Regex::new(r"(?i)<h[123][^>]*>([\s\S]*?)</h[123]>").unwrap());

    // take_chars ≈ JS .slice(0, n): chars() vs UTF-16 code units, see module doc.
    let title = take_chars(&strip_html_str(cap1(title_re, body).as_deref().unwrap_or("")), 160);
    let description = take_chars(
        &strip_html_str(cap1_or(meta1_re, meta2_re, body).as_deref().unwrap_or("")),
        300,
    );
    let mut claims: Vec<Value> = Vec::new();
    for c in htag_re.captures_iter(body) {
        let raw = c.get(1).map(|m| m.as_str()).unwrap_or("");
        let t = strip_html_str(raw);
        // JS `t.length` counts UTF-16 code units, not Unicode scalars — unlike the take_chars
        // truncation sites above, this is a length CHECK (not a slice), so it must match exactly.
        let len16 = t.encode_utf16().count();
        if len16 > 3 && len16 < 120 { claims.push(json!(t)); }
    }
    claims.truncate(12);
    let words = strip_html_str(body).split(' ').count();

    json!({ "title": title, "description": description, "claims": claims, "words": words as i64 })
}

// JS accesses `body.match(...)` directly (no `String(body ?? '')` guard) — null/undefined/
// non-string input throws in JS, so this returns the V8 error message via Err instead of
// silently coercing.
pub fn extractDossier(body: &Value) -> Result<Value, String> {
    match body {
        Value::String(s) => Ok(extract_dossier_str(s)),
        Value::Null => Err("Cannot read properties of null (reading 'match')".to_string()),
        _ => Err("body.match is not a function".to_string()),
    }
}

// git+https://github.com/foo/bar.git → { owner: 'foo', repo: 'bar' } | null
pub fn githubRepo(repository_url: &Value) -> Value {
    static RE: OnceLock<Regex> = OnceLock::new();
    // DIVERGENCE (disclosed, minor): the `regex` crate's `\w` is Unicode-aware by default (matches
    // any Unicode "word" codepoint), while JS's non-`u`-flagged `\w` is ASCII-only
    // ([A-Za-z0-9_]). A repo owner/name containing non-ASCII word characters would match here but
    // not in JS. GitHub owner/repo names are ASCII-only in practice, so not reachable.
    let re = RE.get_or_init(|| Regex::new(r"(?i)github\.com[/:]([\w.-]+)/([\w.-]+?)(?:\.git)?(?:[/#?].*)?$").unwrap());
    let s = str_qq(Some(repository_url));
    match re.captures(&s) {
        Some(c) => json!({ "owner": c[1].to_string(), "repo": c[2].to_string() }),
        None => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_key_and_semver() {
        assert_eq!(hashKey(&json!("hello world")), json!("1n91413"));
        assert_eq!(hashKey(&Value::Null), json!("ztntfp"));
        assert_eq!(parseSemver(&json!("v1.2.3-beta")), json!([1, 2, 3]));
        assert_eq!(parseSemver(&json!("abc")), Value::Null);
        assert_eq!(behindScore(&json!("1.0.0"), &json!("3.2.1")), json!(80));
        assert_eq!(behindScore(&json!("1.2.0"), &json!("1.5.0")), json!(20));
        assert_eq!(behindScore(&json!("1.2.3"), &json!("1.2.9")), json!(6));
        assert_eq!(behindScore(&json!("1.2.3"), &json!("1.2.3")), json!(0));
        assert_eq!(behindScore(&json!("abc"), &json!("1.0.0")), json!(0));
    }

    #[test]
    fn html_text_and_article() {
        let html = "<p>Hello <b>World</b></p><script>bad()</script><li>item</li>&amp;amp;";
        assert_eq!(htmlToText(&json!(html)), json!("Hello World\n\n· item\n&"));
        let art = "<html><body><article><h1>Title</h1><p>Body text</p></article></body></html>";
        assert_eq!(extractArticle(&json!(art)), json!("Title\nBody text"));
        let no_art = "<html><body><p>Just body</p></body></html>";
        assert_eq!(extractArticle(&json!(no_art)), json!("Just body"));
        let main_fallback = "<main><p>Main content</p></main><article-like>not real</article-like>";
        assert_eq!(extractArticle(&json!(main_fallback)), json!("Main content"));
    }

    #[test]
    fn feeds_and_sort() {
        let rss = "<rss><channel><item><title>Hello &amp; World</title><link>http://a.com/1</link><pubDate>Wed, 02 Oct 2024 15:00:00 GMT</pubDate><description>Some &lt;b&gt;desc&lt;/b&gt;</description></item></channel></rss>";
        let got = parseFeed(&json!(rss), &json!("src1"));
        assert_eq!(got, json!([{ "title": "Hello & World", "link": "http://a.com/1", "date": "Wed, 02 Oct 2024 15:00:00 GMT", "source": "src1", "content": "Some desc", "kind": "post" }]));

        let atom = "<feed><entry><title>Atom Title</title><link href=\"http://b.com/2\"/><updated>2024-10-02T15:00:00Z</updated><summary>Atom summary</summary></entry></feed>";
        let got2 = parseFeed(&json!(atom), &json!("src2"));
        assert_eq!(got2, json!([{ "title": "Atom Title", "link": "http://b.com/2", "date": "2024-10-02T15:00:00Z", "source": "src2", "content": "Atom summary", "kind": "post" }]));

        let items = json!([
            { "date": "2024-01-01T00:00:00Z", "t": "a" },
            { "date": "2024-06-01T00:00:00Z", "t": "b" },
            { "date": "", "t": "c" }
        ]);
        let sorted = sortDateDesc(&items, &json!(2)).unwrap();
        assert_eq!(sorted, json!([
            { "date": "2024-06-01", "t": "b" },
            { "date": "2024-01-01", "t": "a" }
        ]));
        assert!(sortDateDesc(&Value::Null, &Value::Null).is_err());
    }

    #[test]
    fn dossier_and_repo() {
        assert_eq!(stripHtml(&json!("<p>Hi &amp; bye</p>")), json!("Hi & bye"));
        let body = "<html><head><title>My Title</title><meta name=\"description\" content=\"Desc here\"></head><body><h1>Head One</h1><h2>ab</h2><h3>Head Three ok</h3><p>Some words here now</p></body></html>";
        let d = extractDossier(&json!(body)).unwrap();
        assert_eq!(d, json!({ "title": "My Title", "description": "Desc here", "claims": ["Head One", "Head Three ok"], "words": 12 }));
        assert!(extractDossier(&Value::Null).is_err());
        assert!(extractDossier(&json!(5)).is_err());

        assert_eq!(githubRepo(&json!("git+https://github.com/foo/bar.git")), json!({ "owner": "foo", "repo": "bar" }));
        assert_eq!(githubRepo(&json!("not a url")), Value::Null);
    }

    #[test]
    fn in_chunks_preserves_order() {
        let items = vec![json!(1), json!(2), json!(3), json!(4), json!(5)];
        let out = inChunks(&items, 2, |v| json!(v.as_i64().unwrap() * 10));
        assert_eq!(out, vec![json!(10), json!(20), json!(30), json!(40), json!(50)]);
    }
}
