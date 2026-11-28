//! THE CHECK NODE (RFC 0010 §4.6): rules, written as data, that judge a piece of text the run made. No model, no cost, the same answer every time. A rule that can be written is
//! better than a reviewer agent whose job was mechanical: it is free, it never has a bad day, and it is the control the team bench stands on. Pure: the text and the list of files go in,
//! a verdict comes out. Nothing here touches the disk, the network or the run.
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const KINDS: [&str; 17] = ["forbid", "require", "words", "chars", "lines", "sections", "json", "html_clean", "readability", "file_exists", "file_size", "pii", "secrets", "urls", "keywords", "links_ok", "diff_size"];
/// Kinds the design names that are not built yet (they need the network, a renderer or the diff).
pub const LATER: [&str; 3] = ["page_overflow", "contrast", "no_console_errors"];   // these need a browser: a tool node runs one, the check does not
const PATTERN_MAX: usize = 400;
const TEXT_MAX: usize = 2_000_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    #[serde(default)] pub id: String, pub kind: String,
    /// what is said when it fails (otherwise a sentence is made from the rule)
    #[serde(default, skip_serializing_if = "String::is_empty")] pub message: String,
    /// a regex (forbid, require)
    #[serde(default, skip_serializing_if = "String::is_empty")] pub pattern: String,
    /// a Markdown heading the count is taken inside (words, chars, lines), or the part of the text a regex looks at
    #[serde(default, skip_serializing_if = "String::is_empty")] pub section: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub max: Option<f64>,
    /// headings that must be there (sections), top-level keys (json), files (file_exists)
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub require: Vec<String>,
    /// headings that must not be there (sections)
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub forbid: Vec<String>,
    /// the hosts a page may reach (html_clean); none means none
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub hosts: Vec<String>,
    /// listed words that must not appear (keywords)
    #[serde(default, skip_serializing_if = "Vec::is_empty")] pub keywords: Vec<String>,
    /// sections must come in the order given
    #[serde(default, skip_serializing_if = "std::ops::Not::not")] pub ordered: bool,
}

/// What is wrong with a rule list before any run, in words. Pure.
pub fn check(rules: &[Rule]) -> Vec<String> {
    let mut out = vec![]; let mut seen: Vec<&str> = vec![];
    if rules.is_empty() { out.push("a check with no rules checks nothing".into()); }
    if rules.len() > 40 { out.push("at most 40 rules".into()); }
    for (i, r) in rules.iter().enumerate() {
        let id = if r.id.is_empty() { format!("rule {}", i + 1) } else { r.id.clone() };
        if !r.id.is_empty() { if seen.contains(&r.id.as_str()) { out.push(format!("two rules are called {}", r.id)); } seen.push(r.id.as_str()); }
        if LATER.contains(&r.kind.as_str()) { out.push(format!("{id}: `{}` is not built yet", r.kind)); continue; }
        if !KINDS.contains(&r.kind.as_str()) { out.push(format!("{id}: `{}` is not a rule kind; they are {}", r.kind, KINDS.join(", "))); continue; }
        match r.kind.as_str() {
            "forbid" | "require" => { if r.pattern.is_empty() { out.push(format!("{id}: say the pattern")); } else if r.pattern.len() > PATTERN_MAX { out.push(format!("{id}: a pattern is at most {PATTERN_MAX} characters")); } else if let Err(e) = Regex::new(&r.pattern) { out.push(format!("{id}: the pattern is not a regular expression: {}", e.to_string().lines().last().unwrap_or(""))); } }
            "words" | "chars" | "lines" | "readability" | "file_size" => { if r.min.is_none() && r.max.is_none() { out.push(format!("{id}: say a `min` or a `max`")); } if matches!((r.min, r.max), (Some(a), Some(b)) if a > b) { out.push(format!("{id}: `min` is above `max`")); } }
            "sections" => { if r.require.is_empty() && r.forbid.is_empty() { out.push(format!("{id}: say which headings are `require`d or `forbid`den")); } }
            "json" | "file_exists" => { if r.kind == "file_exists" && r.require.is_empty() { out.push(format!("{id}: say which files (`require`)")); } }
            "diff_size" => { if r.min.is_none() && r.max.is_none() && r.forbid.is_empty() { out.push(format!("{id}: say a `max` (or `min`) of changed lines, or paths that may not be touched (`forbid`)")); } }
            "urls" => { if r.hosts.is_empty() { out.push(format!("{id}: say which hosts are allowed (`hosts`)")); } }
            "keywords" => { if r.keywords.is_empty() && r.require.is_empty() { out.push(format!("{id}: say which words to flag (`keywords` or `require`)")); } }
            _ => {}
        }
    }
    out
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Verdict { pub pass: bool, pub failed: Vec<Value>, pub passed: Vec<String>, pub score: f64 }
impl Verdict {
    pub fn to_json(&self) -> Value { json!({ "ok": true, "pass": self.pass, "failed": self.failed, "passed": self.passed, "score": self.score }) }
}

// ── the text, in parts ────────────────────────────────────────────────────────────────────────────────────────────────────

fn heading(line: &str) -> Option<(usize, String)> { let t = line.trim_start(); let n = t.chars().take_while(|c| *c == '#').count(); (1..=6).contains(&n).then(|| ()).and_then(|_| t[n..].strip_prefix(' ').map(|r| (n, r.trim().trim_end_matches('#').trim().to_string()))) }
/// The headings of a Markdown text, in order, with their level.
pub fn headings(text: &str) -> Vec<(usize, String)> { let mut inside = false; text.lines().filter_map(|l| { if l.trim_start().starts_with("```") { inside = !inside; return None; } if inside { None } else { heading(l) } }).collect() }
/// The text under a heading (to the next heading of the same or a higher level). `None` when there is no such heading. Case-insensitive.
pub fn section_of(text: &str, name: &str) -> Option<String> {
    let mut lines = text.lines(); let mut level = 0; let mut found = false; let mut out = vec![];
    while let Some(l) = lines.next() {
        if let Some((n, t)) = heading(l) {
            if found && n <= level { break; }
            if !found && t.eq_ignore_ascii_case(name.trim()) { found = true; level = n; continue; }
        }
        if found { out.push(l); }
    }
    found.then(|| out.join("\n"))
}
fn words(t: &str) -> usize { t.split_whitespace().filter(|w| w.chars().any(|c| c.is_alphanumeric())).count() }
fn syllables(w: &str) -> usize { let w = w.to_lowercase(); let mut n = 0; let mut prev = false; for c in w.chars() { let v = "aeiouy".contains(c); if v && !prev { n += 1; } prev = v; } if w.ends_with('e') && n > 1 { n -= 1; } n.max(1) }
/// Flesch reading ease of a text (English). Higher is easier; empty text is 0.
pub fn flesch(t: &str) -> f64 {
    let ws: Vec<&str> = t.split_whitespace().filter(|w| w.chars().any(|c| c.is_alphabetic())).collect(); if ws.is_empty() { return 0.0; }
    let sentences = t.chars().filter(|c| matches!(c, '.' | '!' | '?')).count().max(1) as f64; let syl: usize = ws.iter().map(|w| syllables(w.trim_matches(|c: char| !c.is_alphabetic()))).sum();
    206.835 - 1.015 * (ws.len() as f64 / sentences) - 84.6 * (syl as f64 / ws.len() as f64)
}
fn quote_at(text: &str, at: usize) -> String { let s = text[..at].rfind('\n').map_or(0, |i| i + 1); let e = text[at..].find('\n').map_or(text.len(), |i| at + i); let l: String = text[s..e].trim().chars().take(120).collect(); l }
fn line_of(text: &str, at: usize) -> usize { text[..at].matches('\n').count() + 1 }

/// One rule against the text: `None` when it holds, else what is wrong (`{message, quote?, where?}`).
fn judge(r: &Rule, text: &str, files: &[(String, u64)]) -> Option<Value> {
    let said = |default: String, quote: Option<String>, at: Option<usize>| { let mut v = json!({ "message": if r.message.is_empty() { default } else { r.message.clone() } }); if let Some(q) = quote { v["quote"] = json!(q); } if let Some(w) = at { v["where"] = json!(format!("line {w}")); } v };
    let scope: String = if r.section.is_empty() || !matches!(r.kind.as_str(), "forbid" | "require" | "words" | "chars" | "lines") { text.to_string() } else { match section_of(text, &r.section) { Some(s) => s, None => return Some(said(format!("there is no section called {}", r.section), None, None)) } };
    let n_of = |what: &str, n: f64| -> Option<Value> {
        if let Some(lo) = r.min { if n < lo { return Some(said(format!("{what}{} is {n}, under the least ({lo})", if r.section.is_empty() { String::new() } else { format!(" of {}", r.section) }), None, None)); } }
        if let Some(hi) = r.max { if n > hi { return Some(said(format!("{what}{} is {n}, over the most ({hi})", if r.section.is_empty() { String::new() } else { format!(" of {}", r.section) }), None, None)); } }
        None
    };
    match r.kind.as_str() {
        "forbid" => { let re = Regex::new(&r.pattern).ok()?; let m = re.find(&scope)?; let at = text.find(m.as_str()).unwrap_or(0); Some(said(format!("\"{}\" is not allowed", m.as_str().chars().take(60).collect::<String>()), Some(quote_at(text, at)), Some(line_of(text, at)))) }
        "require" => { let re = Regex::new(&r.pattern).ok()?; let n = re.find_iter(&scope).count(); let (lo, hi) = (r.min.unwrap_or(1.0), r.max); if (n as f64) < lo { Some(said(format!("`{}` is found {n} time(s), it must be at least {lo}", r.pattern), None, None)) } else if hi.map_or(false, |h| n as f64 > h) { Some(said(format!("`{}` is found {n} times, at most {}", r.pattern, hi.unwrap_or(0.0)), None, None)) } else { None } }
        "words" => n_of("the word count", words(&scope) as f64),
        "chars" => n_of("the character count", scope.chars().count() as f64),
        "lines" => n_of("the line count", scope.lines().filter(|l| !l.trim().is_empty()).count() as f64),
        "readability" => n_of("the reading ease", (flesch(&scope) * 10.0).round() / 10.0),
        "sections" => {
            let hs = headings(text); let names: Vec<String> = hs.iter().map(|h| h.1.to_lowercase()).collect();
            for want in &r.require { if !names.iter().any(|n| *n == want.to_lowercase()) { return Some(said(format!("the section \"{want}\" is missing"), None, None)); } }
            for no in &r.forbid { if names.iter().any(|n| *n == no.to_lowercase()) { return Some(said(format!("the section \"{no}\" is not allowed"), None, None)); } }
            if r.ordered { let mut at = 0usize; for want in &r.require { match names.iter().skip(at).position(|n| *n == want.to_lowercase()) { Some(p) => at += p + 1, None => return Some(said(format!("the sections are not in the order {}", r.require.join(", ")), None, None)) } } }
            None
        }
        "json" => match serde_json::from_str::<Value>(text.trim()) { Err(e) => Some(said(format!("it is not valid JSON ({e})"), None, None)), Ok(v) => r.require.iter().find(|k| v.get(k.as_str()).is_none()).map(|k| said(format!("the JSON has no `{k}`"), None, None)) },
        "html_clean" => {
            let low = text.to_lowercase();
            if low.contains("<script") { return Some(said("it has a <script>".into(), None, None)); }
            let host_ok = |u: &str| r.hosts.iter().any(|h| u.contains(h.as_str()));
            for m in Regex::new(r#"(?i)\b(?:src|href)\s*=\s*["']?(https?://[^"'\s>]+)"#).ok()?.captures_iter(text) { let u = &m[1]; if !host_ok(u) { return Some(said(format!("it reaches {u}, which is not an allowed host"), None, None)); } }
            if !Regex::new(r"(?i)<html[^>]*\blang=").ok()?.is_match(text) { return Some(said("the page has no `lang`".into(), None, None)); }
            if !low.contains("<title>") { return Some(said("the page has no <title>".into(), None, None)); }
            if !low.contains("name=\"viewport\"") && !low.contains("name='viewport'") { return Some(said("the page has no viewport meta (it will not fit a phone)".into(), None, None)); }
            None
        }
        "file_exists" => r.require.iter().find(|f| !files.iter().any(|(n, _)| n == *f)).map(|f| said(format!("the run made no document called {f}"), None, None)),
        "file_size" => { let target = r.require.first(); let bytes = match target { Some(f) => match files.iter().find(|(n, _)| n == f) { Some((_, b)) => *b as f64, None => return Some(said(format!("the run made no document called {f}"), None, None)) }, None => text.len() as f64 }; n_of("the size in bytes", bytes) }
        "pii" => {
            let email = Regex::new(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").ok()?;
            if let Some(m) = email.find(&scope) { let at = text.find(m.as_str()).unwrap_or(0); return Some(said("personal data (email) is present".into(), Some(quote_at(text, at)), Some(line_of(text, at)))); }
            if let Some((a, b)) = crate::ledger::card_spans(&scope).first().copied() { let m = &scope[a..b]; let at = text.find(m).unwrap_or(0); return Some(said("personal data (card number) is present".into(), Some(quote_at(text, at)), Some(line_of(text, at)))); }
            None
        }
        "secrets" => {
            let sk = Regex::new(r"(?i)\bsk-[A-Za-z0-9]{20,}\b").ok()?;
            let bearer = Regex::new(r"(?i)bearer\s+[A-Za-z0-9._\-]+").ok()?;
            if let Some(m) = sk.find(&scope) { let at = text.find(m.as_str()).unwrap_or(0); return Some(said("a secret key is present".into(), Some(quote_at(text, at)), Some(line_of(text, at)))); }
            if let Some(m) = bearer.find(&scope) { let at = text.find(m.as_str()).unwrap_or(0); return Some(said("a bearer token is present".into(), Some(quote_at(text, at)), Some(line_of(text, at)))); }
            None
        }
        "urls" => {
            let re = Regex::new(r#"https?://[^\s"'<>]+"#).ok()?;
            for m in re.find_iter(&scope) {
                let u = m.as_str();
                if !url_host_allowed(u, &r.hosts) { return Some(said(format!("a URL reaches {u}, which is not allowed"), None, None)); }
            }
            None
        }
        // LINKS OK (BL-10): every link in a Markdown or HTML text points somewhere that can exist. No network (whether a page answers is a tool node's job): a relative link must name a file of the run (or `require`d here),
        // an address must be well formed and, when `hosts` is given, on an allowed host; an empty link, a `javascript:` link and a `#` that no heading or id answers are broken.
        "links_ok" => {
            let md = Regex::new(r"\]\(([^)\s]*)[^)]*\)").ok()?; let html = Regex::new(r#"(?i)\bhref\s*=\s*["']([^"']*)["']"#).ok()?;
            let mut links: Vec<String> = md.captures_iter(text).map(|c| c[1].to_string()).collect(); links.extend(html.captures_iter(text).map(|c| c[1].to_string()));
            let anchors: Vec<String> = headings(text).into_iter().map(|(_, h)| slugish(&h)).chain(Regex::new(r#"(?i)\bid\s*=\s*["']([^"']+)["']"#).ok()?.captures_iter(text).map(|c| c[1].to_string())).collect();
            for l in links {
                let l = l.trim(); let low = l.to_ascii_lowercase();
                if l.is_empty() { return Some(said("a link has no address".into(), None, None)); }
                if low.starts_with("javascript:") || low.starts_with("data:") { return Some(said(format!("the link `{}` runs code, not a page", l.chars().take(40).collect::<String>()), None, None)); }
                if low.starts_with("mailto:") || low.starts_with("tel:") { continue; }
                if low.starts_with("http://") || low.starts_with("https://") || low.starts_with("//") {
                    let host = url_host(l); if host.is_empty() || !host.contains('.') || l.contains(' ') { return Some(said(format!("the address `{l}` is not well formed"), None, None)); }
                    if !r.hosts.is_empty() && !url_host_allowed(l, &r.hosts) { return Some(said(format!("the link to {host} is not on an allowed host"), None, None)); }
                    continue;
                }
                if let Some(a) = l.strip_prefix('#') { if !anchors.iter().any(|x| x == a || *x == slugish(a)) { return Some(said(format!("the link `#{a}` points at a heading or id that is not there"), None, None)); } continue; }
                let path = l.split(|c| c == '#' || c == '?').next().unwrap_or("").trim_start_matches("./");
                let known = |p: &str| files.iter().any(|(n, _)| n == p || n.ends_with(&format!("/{p}"))) || r.require.iter().any(|x| x.trim_start_matches("./") == p);
                if !known(path) { return Some(said(format!("the link `{l}` points at a file the run did not make"), None, None)); }
            }
            None
        }
        // DIFF SIZE (BL-10): the text is a unified diff. `max`/`min` bound the lines added plus removed; each `forbid` entry is a path (or part of one) the diff may not touch.
        "diff_size" => {
            let (mut changed, mut files_touched) = (0usize, Vec::<String>::new());
            for l in text.lines() {
                if let Some(p) = l.strip_prefix("+++ ") { files_touched.push(p.trim_start_matches("b/").trim().to_string()); continue; }
                if l.starts_with("--- ") || l.starts_with("diff ") || l.starts_with("@@") || l.starts_with("index ") { continue; }
                if l.starts_with('+') || l.starts_with('-') { changed += 1; }
            }
            if let Some(f) = r.forbid.iter().find_map(|x| files_touched.iter().find(|t| t.contains(x.as_str()))) { return Some(said(format!("the change touches {f}, which it may not"), None, None)); }
            n_of("changed lines", changed as f64)
        }
        "keywords" => {
            let words: Vec<&str> = if r.keywords.is_empty() { r.require.iter().map(String::as_str).collect() } else { r.keywords.iter().map(String::as_str).collect() };
            let low = scope.to_lowercase();
            for w in words {
                let needle = w.to_lowercase();
                if let Some(at) = low.find(&needle) {
                    return Some(said(format!("the word \"{w}\" is not allowed"), Some(quote_at(text, at)), Some(line_of(text, at))));
                }
            }
            None
        }
        _ => Some(said(format!("`{}` is not a rule kind", r.kind), None, None)),
    }
}

/// All the rules against the text (and the names and sizes of the run's documents). A rule that cannot be run fails with its reason. The verdict passes only when every rule holds. Pure.
/// The host of a URL (no scheme, user info, port, path, query or fragment), lower-case.
fn url_host(u: &str) -> String {
    let rest = u.split_once("://").map_or(u, |(_, r)| r);
    let auth = rest.split(|c| matches!(c, '/' | '?' | '#')).next().unwrap_or("");
    let host = auth.rsplit('@').next().unwrap_or("");
    host.split(':').next().unwrap_or("").trim_end_matches('.').to_lowercase()
}
/// An allowed host is matched whole: the host itself or a subdomain of it, never a substring (`fonts.googleapis.com.evil.com` and `?next=fonts.googleapis.com` are not `fonts.googleapis.com`).
fn url_host_allowed(u: &str, allowed: &[String]) -> bool {
    let host = url_host(u);
    !host.is_empty() && allowed.iter().any(|a| { let a = a.trim().trim_start_matches("*.").to_lowercase(); !a.is_empty() && (host == a || host.ends_with(&format!(".{a}"))) })
}

/// A heading as a link anchor: lower-case, spaces to dashes, only letters, digits, dashes. Pure.
fn slugish(h: &str) -> String { h.trim().to_lowercase().chars().filter_map(|c| if c.is_alphanumeric() { Some(c) } else if c == ' ' || c == '-' { Some('-') } else { None }).collect() }

pub fn evaluate(rules: &[Rule], text: &str, files: &[(String, u64)]) -> Verdict {
    let text = if text.len() > TEXT_MAX { &text[..text.char_indices().take_while(|(i, _)| *i < TEXT_MAX).last().map_or(0, |(i, _)| i)] } else { text };
    let (mut failed, mut passed) = (vec![], vec![]);
    for (i, r) in rules.iter().enumerate() {
        let id = if r.id.is_empty() { format!("rule-{}", i + 1) } else { r.id.clone() };
        match judge(r, text, files) { None => passed.push(id), Some(mut f) => { f["rule"] = json!(id); failed.push(f); } }
    }
    let total = rules.len().max(1) as f64; Verdict { pass: failed.is_empty(), score: ((passed.len() as f64 / total) * 100.0).round() / 100.0, failed, passed }
}
/// What the next agent is told: exactly what the checks found wrong. Empty when all passed. Pure.
pub fn evidence(v: &Value) -> String {
    let failed = v["failed"].as_array().cloned().unwrap_or_default(); if failed.is_empty() { return String::new(); }
    failed.iter().map(|f| format!("- {}: {}{}{}", f["rule"].as_str().unwrap_or(""), f["message"].as_str().unwrap_or(""), f["where"].as_str().map_or(String::new(), |w| format!(" ({w})")), f["quote"].as_str().map_or(String::new(), |q| format!(" — \"{q}\"")))).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn r(kind: &str) -> Rule { Rule { id: kind.into(), kind: kind.into(), ..Default::default() } }
    const POSTS: &str = "# Posts\n\n## Instagram\nFresh beans, roasted today. Come by.\n\n## Newsletter blurb\nThis week: 20% off all beans! Hurry.\n\n## Facebook\nOpen until six.\n";

    #[test]
    fn a_forbidden_pattern_names_what_it_found_with_the_line_and_the_quote() {
        let v = evaluate(&[Rule { pattern: r"(?i)\b\d+\s?% off\b|\bdiscount\b".into(), message: "a discount or price offer".into(), ..r("forbid") }], POSTS, &[]);
        assert!(!v.pass); let f = &v.failed[0]; assert_eq!(f["message"], "a discount or price offer"); assert_eq!(f["where"], "line 7"); assert!(f["quote"].as_str().unwrap().contains("20% off"));
        assert!(evaluate(&[Rule { pattern: "discount".into(), ..r("forbid") }], POSTS, &[]).pass);
    }
    #[test]
    fn counts_are_whole_or_inside_a_section() {
        let w = |sec: &str, min: Option<f64>, max: Option<f64>| evaluate(&[Rule { section: sec.into(), min, max, ..r("words") }], POSTS, &[]);
        assert!(w("Instagram", None, Some(60.0)).pass); assert!(!w("Instagram", None, Some(3.0)).pass); assert!(!w("Instagram", Some(100.0), None).pass);
        assert!(w("", Some(5.0), None).pass); assert!(w("Nowhere", None, Some(9.0)).failed[0]["message"].as_str().unwrap().contains("no section called Nowhere"));
        assert_eq!(section_of(POSTS, "facebook").unwrap().trim(), "Open until six.", "case does not matter, a section ends at the next heading");
    }
    #[test]
    fn sections_are_required_forbidden_and_in_order() {
        let s = |req: &[&str], forbid: &[&str], ordered| evaluate(&[Rule { require: req.iter().map(|x| x.to_string()).collect(), forbid: forbid.iter().map(|x| x.to_string()).collect(), ordered, ..r("sections") }], POSTS, &[]);
        assert!(s(&["Instagram", "Newsletter blurb", "Facebook"], &[], true).pass); assert!(!s(&["Facebook", "Instagram"], &[], true).pass); assert!(s(&["Facebook", "Instagram"], &[], false).pass);
        assert!(s(&["TikTok"], &[], false).failed[0]["message"].as_str().unwrap().contains("TikTok")); assert!(!s(&[], &["Facebook"], false).pass);
        assert_eq!(headings("# A\n```\n# not a heading\n```\n## B").iter().map(|h| h.1.as_str()).collect::<Vec<_>>(), vec!["A", "B"], "a code block has no headings");
    }
    #[test]
    fn require_counts_matches_and_json_html_files_and_readability_have_their_own() {
        let q = |pat: &str, min: Option<f64>| evaluate(&[Rule { pattern: pat.into(), min, ..r("require") }], "[PLACEHOLDER: a] and [PLACEHOLDER: b]", &[]);
        assert!(q(r"\[PLACEHOLDER: [^\]]+\]", Some(2.0)).pass); assert!(!q(r"\[PLACEHOLDER: [^\]]+\]", Some(3.0)).pass);
        let j = |t: &str, keys: &[&str]| evaluate(&[Rule { require: keys.iter().map(|k| k.to_string()).collect(), ..r("json") }], t, &[]);
        assert!(j(r#"{"a":1,"b":2}"#, &["a"]).pass); assert!(!j(r#"{"a":1}"#, &["b"]).pass); assert!(j("not json", &[]).failed[0]["message"].as_str().unwrap().contains("not valid JSON"));
        let h = |t: &str| evaluate(&[Rule { hosts: vec!["fonts.googleapis.com".into()], ..r("html_clean") }], t, &[]);
        let page = "<!doctype html><html lang=\"en\"><head><title>x</title><meta name=\"viewport\" content=\"width=device-width\"></head><body>hi</body></html>";
        assert!(h(page).pass); assert!(!h(&page.replace("<body>", "<body><script>1</script>")).pass); assert!(!h(&page.replace("<body>", "<body><img src=\"http://tracker.example/x.png\">")).pass);
        assert!(h(&page.replace("<body>", "<body><link href=\"https://fonts.googleapis.com/css\">")).pass, "a listed host is allowed"); assert!(!h(&page.replace(" lang=\"en\"", "")).pass); assert!(!h(&page.replace("<title>x</title>", "")).pass);
        let files = vec![("report.md".to_string(), 2048u64)]; let f = |k: &str, req: &[&str], min: Option<f64>, max: Option<f64>| evaluate(&[Rule { require: req.iter().map(|x| x.to_string()).collect(), min, max, ..r(k) }], "", &files);
        assert!(f("file_exists", &["report.md"], None, None).pass); assert!(!f("file_exists", &["other.md"], None, None).pass); assert!(f("file_size", &["report.md"], None, Some(4096.0)).pass); assert!(!f("file_size", &["report.md"], Some(9999.0), None).pass);
        let easy = "The cat sat. The dog ran. We are glad."; let hard = "Notwithstanding aforementioned considerations, institutional reorganisation necessitates comprehensive deliberation."; assert!(flesch(easy) > 80.0 && flesch(hard) < 20.0, "{} {}", flesch(easy), flesch(hard));
        assert!(evaluate(&[Rule { min: Some(60.0), ..r("readability") }], easy, &[]).pass); assert!(!evaluate(&[Rule { min: Some(60.0), ..r("readability") }], hard, &[]).pass);
    }
    #[test]
    fn rule_pii_flags_emails_and_card_numbers() {
        let t = "Contact me at dan@example.com please";
        assert!(!evaluate(&[Rule { kind: "pii".into(), ..Default::default() }], t, &[]).pass);
        assert!(evaluate(&[Rule { kind: "pii".into(), ..Default::default() }], "no personal data here", &[]).pass);
        assert!(!evaluate(&[Rule { kind: "pii".into(), ..Default::default() }], "card 4111 1111 1111 1111", &[]).pass);
    }
    #[test]
    fn rule_secrets_flags_keys_and_tokens() {
        assert!(!evaluate(&[Rule { kind: "secrets".into(), ..Default::default() }], "use sk-abcdefghijklmnopqrstuvwxyz12", &[]).pass);
        assert!(!evaluate(&[Rule { kind: "secrets".into(), ..Default::default() }], "Authorization: bearer abcdef.ghi", &[]).pass);
        assert!(evaluate(&[Rule { kind: "secrets".into(), ..Default::default() }], "nothing sensitive", &[]).pass);
    }
    #[test]
    fn rule_urls_enforces_an_allow_list() {
        let ok = "see https://fonts.googleapis.com/css";
        let bad = "see https://evil.example/track";
        assert!(evaluate(&[Rule { kind: "urls".into(), hosts: vec!["fonts.googleapis.com".into()], ..Default::default() }], ok, &[]).pass);
        assert!(!evaluate(&[Rule { kind: "urls".into(), hosts: vec!["fonts.googleapis.com".into()], ..Default::default() }], bad, &[]).pass);
    }
    #[test]
    fn the_urls_allow_list_matches_the_whole_host_not_a_substring() {
        let rule = Rule { kind: "urls".into(), hosts: vec!["fonts.googleapis.com".into()], ..Default::default() };
        for bad in ["https://fonts.googleapis.com.evil.com/x", "https://evil.com/?next=fonts.googleapis.com", "https://fonts.googleapis.com@evil.com/x", "https://evilfonts.googleapis.com/x"] {
            assert!(!evaluate(&[rule.clone()], &format!("see {bad}"), &[]).pass, "{bad}");
        }
        for ok in ["https://fonts.googleapis.com/css", "https://FONTS.googleapis.com:443/css?a=1", "https://cdn.fonts.googleapis.com/x"] {
            assert!(evaluate(&[rule.clone()], &format!("see {ok}"), &[]).pass, "{ok}");
        }
    }

    #[test]
    fn pii_does_not_flag_a_long_number_that_is_not_a_card() {
        let r = [Rule { kind: "pii".into(), ..Default::default() }];
        assert!(evaluate(&r, "built at 1731600000000 order 1234567890123456", &[]).pass);
        assert!(!evaluate(&r, "card 4111 1111 1111 1111", &[]).pass);
    }

    #[test]
    fn rule_keywords_flags_listed_words() {
        assert!(!evaluate(&[Rule { kind: "keywords".into(), keywords: vec!["discount".into()], ..Default::default() }], "20% discount today", &[]).pass);
        assert!(evaluate(&[Rule { kind: "keywords".into(), require: vec!["lorem".into()], ..Default::default() }], "clean text", &[]).pass);
    }

    #[test]
    fn a_bad_rule_list_is_said_in_words_and_the_verdict_scores_and_tells_the_next_agent() {
        let bad = check(&[Rule { kind: "forbid".into(), ..Default::default() }, Rule { kind: "require".into(), pattern: "(".into(), ..Default::default() }, Rule { kind: "contrast".into(), ..Default::default() }, Rule { kind: "magic".into(), ..Default::default() }, Rule { kind: "words".into(), min: Some(9.0), max: Some(2.0), ..Default::default() }]).join(" | ");
        for n in ["say the pattern", "not a regular expression", "`contrast` is not built yet", "`magic` is not a rule kind", "`min` is above `max`"] { assert!(bad.contains(n), "{n} in {bad}"); }
        assert!(check(&[]).iter().any(|m| m.contains("checks nothing"))); assert!(check(&[Rule { pattern: "x".repeat(401), ..r("forbid") }]).iter().any(|m| m.contains("at most 400")));
        let v = evaluate(&[Rule { id: "a".into(), pattern: "Fresh".into(), ..r("require") }, Rule { id: "b".into(), pattern: "discount".into(), message: "no offers".into(), ..r("require") }], POSTS, &[]);
        assert!(!v.pass && v.passed == vec!["a"] && v.score == 0.5); let e = evidence(&v.to_json()); assert!(e.starts_with("- b: no offers"), "{e}"); assert_eq!(evidence(&evaluate(&[], "", &[]).to_json()), "");
        assert!(evaluate(&[Rule { pattern: "(".into(), ..r("forbid") }], "x", &[]).pass, "a rule that is not valid is the validator's to refuse; run alone it does not guess");
    }

    fn rule(kind: &str) -> Rule { Rule { kind: kind.into(), ..Default::default() } }
    #[test]
    fn a_link_that_cannot_exist_is_found_without_the_network() {
        let r = |hosts: &[&str]| Rule { hosts: hosts.iter().map(|h| h.to_string()).collect(), ..rule("links_ok") };
        let files = vec![("render/out.mp4".to_string(), 10u64), ("docs/guide.md".to_string(), 5)];
        let ok = "# Intro\n\nSee [the guide](docs/guide.md#top), [the video](out.mp4), [site](https://hyperframes.dev/docs), [mail](mailto:a@b.c) and [up](#intro).\n";
        assert!(evaluate(&[r(&[])], ok, &files).pass, "{:?}", evaluate(&[r(&[])], ok, &files).failed);
        for (bad, why) in [("[x]()", "no address"), ("[x](javascript:alert(1))", "runs code"), ("[x](missing.png)", "a file the run did not make"), ("[x](#nowhere)", "not there"), ("[x](https://nodot/page)", "not well formed"), ("<a href=\"\">x</a>", "no address")] {
            let v = evaluate(&[r(&[])], bad, &files); assert!(!v.pass && v.failed[0]["message"].as_str().unwrap().contains(why), "{bad}: {:?}", v.failed);
        }
        assert!(!evaluate(&[r(&["hyperframes.dev"])], "[x](https://evil.example.com/a)", &files).pass, "an unlisted host"); assert!(evaluate(&[r(&["hyperframes.dev"])], "[x](https://docs.hyperframes.dev/a)", &files).pass, "a subdomain of an allowed host");
        assert!(evaluate(&[r(&[])], "<a href=\"#sec\">x</a><h2 id=\"sec\">S</h2>", &files).pass, "an html id answers an anchor"); assert!(check(&[r(&[])]).is_empty());
    }
    #[test]
    fn a_diff_is_held_to_its_size_and_to_the_paths_it_may_touch() {
        let diff = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,2 +1,3 @@\n-old\n+new\n+newer\n ctx\n";
        let d = |max: f64, forbid: &[&str]| Rule { max: Some(max), forbid: forbid.iter().map(|x| x.to_string()).collect(), ..rule("diff_size") };
        assert!(evaluate(&[d(3.0, &[])], diff, &[]).pass, "3 changed lines are within 3"); let v = evaluate(&[d(2.0, &[])], diff, &[]); assert!(!v.pass && v.failed[0]["message"].as_str().unwrap().contains("changed lines is 3"), "{:?}", v.failed);
        let v = evaluate(&[d(99.0, &["Cargo.toml", "src/a.rs"])], diff, &[]); assert!(!v.pass && v.failed[0]["message"].as_str().unwrap().contains("touches src/a.rs"), "{:?}", v.failed);
        assert!(evaluate(&[d(99.0, &["Cargo.toml"])], diff, &[]).pass); assert!(!check(&[rule("diff_size")]).is_empty(), "a diff_size rule needs a bound"); assert!(check(&[rule("contrast")]).iter().any(|m| m.contains("not built yet")), "the browser checks stay for a tool node");
    }
}
