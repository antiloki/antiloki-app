//! constitution-research.rs — twin of skills/constitution/constitution-research.mjs. NAMES ARE
//! CONTRACT: `fetchUrl`, `searchOnce`, default `constitutionResearch`.
//!
//! Deterministic pipeline (per the JS module doc): queries/urls are taken verbatim from the
//! record/input — never invented — and capped (queries ≤4, urls ≤9); the FETCH is the substrate's
//! cached, SSRF-guarded web reach (`knowledge.web_fetch_cache` via `cachedFetch`); the PARSE is a
//! fixed extractor over the fetched HTML (DuckDuckGo results, or a direct page's `<title>`/body
//! text).
//!
//! PARITY-DEFERRED: the real network hop (`cachedFetch` inside JS's `fetchUrl`/`searchOnce`, or a
//! JS-injected `context.fetch_fn`) is not implemented in this crate (no HTTP client, zero-new-deps
//! per the crossing playbook). JS's own `fetch_fn` injection point (`context.fetch_fn`, its
//! "money-guard pattern, applied to the web") is mirrored here as an optional `Option<&dyn Fn(&str)
//! -> String>` parameter on `fetchUrl`/`searchOnce`/`constitutionResearch` — when a mock body
//! producer is supplied, the FULL deterministic pipeline (request-URL assembly → body → parsed
//! sources → dedupe/cap) runs and is exercised by the `#[cfg(test)]` tests below; when `None` (the
//! real-network path), each call hits the deferred boundary and returns `Err`, exactly mirroring
//! how the JS orchestrator's `try { ... } catch { /* one dead url/query never kills the run */ }`
//! swallows a genuine `cachedFetch` failure — so `constitutionResearch` run with `fetch_fn: None`
//! still returns `ok:true` with an empty `sources` array, never propagating the boundary error.
//!
//! `context.db` missing (JS `err.required('constitution-research', 'context.db')`) is unreachable
//! here — Rust's `db: &Connection` is always a real connection, per this project's standard
//! db-lane twin convention (see e.g. constitution-file.rs).

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::OnceLock;

// JS: `strip(h)` — String(h ?? '') then tag-strip, entity-decode, whitespace-collapse, trim.
fn strip(h: &str) -> String {
    let no_tags = tag_re().replace_all(h, " ");
    let decoded = no_tags
        .replace("&amp;", "&")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    ws_re().replace_all(&decoded, " ").trim().to_string()
}

fn tag_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"<[^>]+>").unwrap())
}
fn ws_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\s+").unwrap())
}
fn title_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").unwrap())
}
fn script_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?is)<script[\s\S]*?</script>").unwrap())
}
fn style_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?is)<style[\s\S]*?</style>").unwrap())
}
fn result_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?s)class="result__a"[^>]*href="([^"]+)"[^>]*>(.*?)</a>.*?class="result__snippet"[^>]*>(.*?)</a>"#).unwrap()
    })
}
fn uddg_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[?&]uddg=([^&]+)").unwrap())
}
fn http_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^https?://").unwrap())
}

// JS: `new URL(url).hostname` (best-effort, try/catch → null on parse failure). Minimal stand-in
// (no IDNA/percent-decode/IPv6-bracket support), matching the same-shape helper already ported in
// codebase/_helpers/web-cache.rs's private `parse_url` (not reused here: that fn is private to its
// module and only extracts scheme+host for the SSRF guard, not exported for cross-module reuse).
fn url_hostname(u: &str) -> Option<String> {
    let idx = u.find("://")?;
    let rest = &u[idx + 3..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host_port = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };
    let host = match host_port.rfind(':') {
        Some(i) => &host_port[..i],
        None => host_port,
    };
    if host.is_empty() { None } else { Some(host.to_lowercase()) }
}

// JS: `encodeURIComponent` — unreserved set A-Z a-z 0-9 - _ . ! ~ * ' ( ), percent-encoding every
// other UTF-8 byte. Same approach as external/gemini-call.rs's `encode_uri_component` (zero-new-
// Cargo-deps per the crossing playbook — no url-encoding crate available).
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// JS: `decodeURIComponent` (percent-decode only — the uddg param is plain UTF-8, no `+`-as-space
// handling needed). Returns None on a malformed escape, mirroring JS's throw → caller's `catch { /*
// keep the wrapper */ }`.
fn decode_uri_component(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// JS: the tail of `fetchUrl` — title-extraction + text-excerpt from a fetched page's body, once the
// body is in hand. Pure/deterministic; the network hop that produces `body` is the deferred edge.
fn parse_fetched_page(url: &str, body: &str) -> Value {
    let domain = url_hostname(url);
    let title_m = title_re().captures(body).map(|c| c[1].to_string()).unwrap_or_default();
    let stripped_title = strip(&title_m);
    let title = if !stripped_title.is_empty() {
        stripped_title
    } else if let Some(d) = &domain {
        d.clone()
    } else {
        url.to_string()
    };
    let title = take_chars(&title, 140);
    let no_scripts = script_re().replace_all(body, " ");
    let no_styles = style_re().replace_all(&no_scripts, " ");
    let text = strip(&no_styles);
    json!({
        "url": take_chars(url, 300),
        "domain": domain,
        "title": title,
        "snippet": take_chars(&text, 300),
        "query": "(direct)",
    })
}

// JS: the DuckDuckGo `class="result__a"` / `class="result__snippet"` regex loop inside
// `searchOnce`, capped at 3 sources per query, once `body` is in hand.
fn parse_search_body(query: &str, body: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for cap in result_re().captures_iter(body) {
        if out.len() >= 3 {
            break;
        }
        let mut u = cap[1].to_string();
        if let Some(m) = uddg_re().captures(&u) {
            if let Some(decoded) = decode_uri_component(&m[1]) {
                u = decoded;
            } // else keep the wrapper, per JS's catch
        }
        if !http_re().is_match(&u) {
            continue;
        }
        let domain = url_hostname(&u);
        out.push(json!({
            "url": take_chars(&u, 300),
            "domain": domain,
            "title": take_chars(&strip(&cap[2]), 140),
            "snippet": take_chars(&strip(&cap[3]), 260),
            "query": query,
        }));
    }
    out
}

// JS: `fetchUrl(db, url, { fetch_fn })`. `fetch_fn`, when supplied, is the DI seam JS itself uses
// (`context.fetch_fn`) — calling it yields the full deterministic parse. `db` is accepted for
// signature parity (JS threads it into `cachedFetch` for the response cache) but unused on this
// twin's paths, since the real cache-backed fetch is the deferred boundary.
pub fn fetchUrl(db: &Connection, url: &str, fetch_fn: Option<&dyn Fn(&str) -> String>) -> Result<Option<Value>, String> {
    let _ = db;
    if !http_re().is_match(url) {
        return Ok(None);
    }
    match fetch_fn {
        Some(f) => Ok(Some(parse_fetched_page(url, &f(url)))),
        None => Err("constitution-research: fetchUrl PARITY-DEFERRED — real network fetch (cachedFetch) is not implemented in this twin".to_string()),
    }
}

// JS: the `url` assembly inside `searchOnce`, up to (not including) the fetch call.
fn build_search_url(query: &str) -> String {
    format!("https://html.duckduckgo.com/html/?q={}", encode_uri_component(query))
}

// JS: `searchOnce(db, query, { fetch_fn })` — returns `{ query, sources }`. Same DI seam as
// `fetchUrl`.
pub fn searchOnce(db: &Connection, query: &str, fetch_fn: Option<&dyn Fn(&str) -> String>) -> Result<Value, String> {
    let _ = db;
    let url = build_search_url(query);
    match fetch_fn {
        Some(f) => {
            let sources = parse_search_body(query, &f(&url));
            Ok(json!({ "query": query, "sources": sources }))
        }
        None => Err("constitution-research: searchOnce PARITY-DEFERRED — real network fetch (cachedFetch) is not implemented in this twin".to_string()),
    }
}

// JS: `export default async function constitutionResearch(input, context)`. `context.db` →
// `db: &Connection` (always present, see module doc); `context.fetch_fn` → `fetch_fn` (the DI
// seam; `None` = the real-network / deferred path).
pub fn constitutionResearch(input: &Value, db: &Connection, fetch_fn: Option<&dyn Fn(&str) -> String>) -> Value {
    crate::codebase__helpers_web_cache::ensureWebCache(db);

    let queries: Vec<String> = input
        .get("queries")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|q| json_to_string(q).trim().to_string())
                .filter(|s| !s.is_empty())
                .take(4)
                .collect()
        })
        .unwrap_or_default();

    let urls: Vec<String> = input
        .get("urls")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|u| json_to_string(u).trim().to_string())
                .filter(|s| http_re().is_match(s))
                .take(9)
                .collect()
        })
        .unwrap_or_default();

    if queries.is_empty() && urls.is_empty() {
        return json!({ "ok": true, "queries": [], "urls": [], "sources": [] });
    }

    let mut all: Vec<Value> = Vec::new();
    for u in &urls {
        // JS: `try { ... } catch { /* one dead url never kills the run */ }`
        if let Ok(Some(s)) = fetchUrl(db, u, fetch_fn) {
            all.push(s);
        }
    }
    for q in &queries {
        // JS: `try { ... } catch { /* one dead query never kills the run */ }`
        if let Ok(r) = searchOnce(db, q, fetch_fn) {
            if let Some(sources) = r.get("sources").and_then(Value::as_array) {
                all.extend(sources.iter().cloned());
            }
        }
    }

    let mut seen = HashSet::new();
    let mut sources = Vec::new();
    for s in all {
        let u = s.get("url").and_then(Value::as_str).unwrap_or("").to_string();
        if seen.contains(&u) {
            continue;
        }
        seen.insert(u);
        sources.push(s);
        if sources.len() >= 9 {
            break;
        }
    }

    json!({
        "ok": true,
        "queries": queries,
        "urls": urls,
        "sources": sources,
        "note": "scraped search results — UNVERIFIED; weigh, corroborate, cite by URL",
    })
}

// JS: `String(v)` for an arbitrary array element.
fn json_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS knowledge").unwrap();
        c
    }

    #[test]
    fn strip_removes_tags_decodes_entities_and_collapses_whitespace() {
        assert_eq!(strip("<b>Hi &amp; bye</b>   there\n\n"), "Hi & bye there");
        assert_eq!(strip("a &#x27;b&#x27; &quot;c&quot; &lt;d&gt;"), "a 'b' \"c\" <d>");
    }

    #[test]
    fn parse_fetched_page_extracts_title_and_snippet() {
        let body = "<html><head><title>  Hello <b>World</b>  </title></head><body><script>evil()</script><style>.x{}</style><p>Some body text.</p></body></html>";
        let out = parse_fetched_page("https://example.com/page", body);
        assert_eq!(out["title"], json!("Hello World"));
        assert_eq!(out["domain"], json!("example.com"));
        // real JS: text is stripped from the whole body after removing only
        // <script>/<style> — <title> content is NOT excluded, so it leaks into
        // the snippet too (verified via node against the real strip()+fetchUrl logic).
        assert_eq!(out["snippet"], json!("Hello World Some body text."));
        assert_eq!(out["query"], json!("(direct)"));
    }

    #[test]
    fn parse_fetched_page_falls_back_to_domain_then_url_when_title_missing() {
        let with_domain = parse_fetched_page("https://x.example/a", "<html><body>no title</body></html>");
        assert_eq!(with_domain["title"], json!("x.example"));

        let no_domain = parse_fetched_page("not-a-real-url", "<html><body>no title</body></html>");
        assert_eq!(no_domain["title"], json!("not-a-real-url"));
        assert_eq!(no_domain["domain"], Value::Null);
    }

    #[test]
    fn parse_search_body_extracts_up_to_three_results_and_decodes_uddg() {
        let body = r##"
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Ffoo.example%2Fa&rut=x">Foo <b>Title</b></a>
          <a class="result__snippet" href="#">Foo snippet &amp; more</a>
          <a class="result__a" href="https://bar.example/b">Bar Title</a>
          <a class="result__snippet" href="#">Bar snippet</a>
          <a class="result__a" href="https://baz.example/c">Baz Title</a>
          <a class="result__snippet" href="#">Baz snippet</a>
          <a class="result__a" href="https://qux.example/d">Qux Title (should be dropped, cap is 3)</a>
          <a class="result__snippet" href="#">Qux snippet</a>
        "##;
        let out = parse_search_body("my query", body);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0]["url"], json!("https://foo.example/a"));
        assert_eq!(out[0]["domain"], json!("foo.example"));
        assert_eq!(out[0]["title"], json!("Foo Title"));
        assert_eq!(out[0]["snippet"], json!("Foo snippet & more"));
        assert_eq!(out[0]["query"], json!("my query"));
        assert_eq!(out[2]["url"], json!("https://baz.example/c"));
    }

    #[test]
    fn parse_search_body_skips_non_http_result_after_undecodable_or_missing_uddg() {
        let body = r##"<a class="result__a" href="javascript:alert(1)">X</a><a class="result__snippet" href="#">Y</a>"##;
        assert_eq!(parse_search_body("q", body).len(), 0);
    }

    #[test]
    fn fetch_url_rejects_non_http_url_without_needing_a_fetcher() {
        let c = db();
        assert_eq!(fetchUrl(&c, "ftp://example.com/x", None).unwrap(), None);
    }

    #[test]
    fn fetch_url_with_injected_fetcher_returns_parsed_page() {
        let c = db();
        let fetcher = |_u: &str| "<html><head><title>Injected</title></head><body>body text</body></html>".to_string();
        let out = fetchUrl(&c, "https://example.com/x", Some(&fetcher)).unwrap().unwrap();
        assert_eq!(out["title"], json!("Injected"));
        // real JS includes the <title> text in the snippet (only script/style are
        // stripped before the generic tag-strip) — verified via node.
        assert_eq!(out["snippet"], json!("Injected body text"));
    }

    #[test]
    fn fetch_url_without_fetcher_is_parity_deferred() {
        let c = db();
        let e = fetchUrl(&c, "https://example.com/x", None).unwrap_err();
        assert!(e.contains("PARITY-DEFERRED"), "{e}");
    }

    #[test]
    fn search_once_builds_ddg_url_and_parses_injected_body() {
        let c = db();
        // the fetcher echoes the URL it was called with into the snippet, proving searchOnce
        // built the DuckDuckGo request URL (with encoded query) before fetching.
        let fetcher = |u: &str| {
            format!(
                "<a class=\"result__a\" href=\"https://hit.example/x\">Hit</a><a class=\"result__snippet\" href=\"#\">Snip for {u}</a>"
            )
        };
        let r = searchOnce(&c, "hello world", Some(&fetcher)).unwrap();
        assert_eq!(r["query"], json!("hello world"));
        let sources = r["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0]["url"], json!("https://hit.example/x"));
        assert!(
            sources[0]["snippet"].as_str().unwrap().contains("html/?q=hello%20world"),
            "{:?}",
            sources[0]["snippet"]
        );
    }

    #[test]
    fn search_once_without_fetcher_is_parity_deferred() {
        let c = db();
        let e = searchOnce(&c, "q", None).unwrap_err();
        assert!(e.contains("PARITY-DEFERRED"), "{e}");
    }

    #[test]
    fn constitution_research_returns_empty_when_no_queries_or_urls() {
        let c = db();
        let out = constitutionResearch(&json!({}), &c, None);
        assert_eq!(out, json!({ "ok": true, "queries": [], "urls": [], "sources": [] }));
    }

    #[test]
    fn constitution_research_caps_queries_and_urls_and_filters_invalid() {
        let c = db();
        let input = json!({
            "queries": ["a", "  ", "b", "c", "d", "e"],
            "urls": ["https://x.example/1", "not-a-url", "https://x.example/2"],
        });
        let out = constitutionResearch(&input, &c, None);
        assert_eq!(out["queries"], json!(["a", "b", "c", "d"]));
        assert_eq!(out["urls"], json!(["https://x.example/1", "https://x.example/2"]));
        // no fetcher injected → deferred boundary swallowed per-item, sources stays empty
        assert_eq!(out["sources"], json!([]));
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["note"], json!("scraped search results — UNVERIFIED; weigh, corroborate, cite by URL"));
    }

    #[test]
    fn constitution_research_with_injected_fetcher_dedupes_and_caps_sources() {
        let c = db();
        let input = json!({ "urls": ["https://a.example/1", "https://a.example/1", "https://b.example/2"] });
        let fetcher = |u: &str| format!("<html><head><title>T</title></head><body>{u}</body></html>");
        let out = constitutionResearch(&input, &c, Some(&fetcher));
        let sources = out["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 2, "duplicate url deduped");
        assert_eq!(out["ok"], json!(true));
    }
}
