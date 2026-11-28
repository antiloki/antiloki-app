//! THE WEB FOR A TEAM (RFC 0008 §4.4, `useTool` type `web`): a node that searches the web or reads one page, with NO model and NO key. It is deterministic: the same call gives the same shape of answer, and what it returns is plain text the nodes
//! after it are told (a custom agent gets it as "what the nodes before you found"). It only ever reads: `GET`, text pages, a size cap, a timeout, and never an address inside this machine or its network (a team's graph is data an agent wrote, so the
//! node must not be a way to reach a router, a metadata service or the serve itself). `ANTILOKI_WEB_SEARCH_URL` names another search (a template with `{q}`; JSON `{results:[{title,url,snippet}]}` or the HTML of DuckDuckGo's lite page);
//! `ANTILOKI_WEB_ALLOW_LOCAL=1` lifts the address guard, for a test that serves a fixture on this machine.

use regex::Regex;
use serde_json::{json, Map, Value};
use std::net::IpAddr;
use std::sync::OnceLock;

pub const MAX_PAGE_BYTES: usize = 2_000_000;
pub const MAX_TEXT_CHARS: usize = 20_000;
const UA: &str = "Mozilla/5.0 (compatible; antiloki-team/1.0; +https://antiloki.dev)";
/// A search page is served to a browser: the engines answer a bot with a challenge.
const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";
fn re(cell: &'static OnceLock<Regex>, p: &str) -> &'static Regex { cell.get_or_init(|| Regex::new(p).expect("regex")) }

/// One search result.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit { pub title: String, pub url: String, pub snippet: String }

fn numeric_entities(s: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"&#(x[0-9a-fA-F]+|[0-9]+);").replace_all(s, |c: &regex::Captures| { let n = &c[1]; let code = if let Some(h) = n.strip_prefix('x') { u32::from_str_radix(h, 16).ok() } else { n.parse::<u32>().ok() }; code.and_then(char::from_u32).map(|ch| ch.to_string()).unwrap_or_else(|| c[0].to_string()) }).to_string()
}
fn entities(s: &str) -> String { numeric_entities(s).replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&#x27;", "'").replace("&nbsp;", " ").replace("&apos;", "'") }
fn strip_tags(s: &str) -> String { static R: OnceLock<Regex> = OnceLock::new(); let t = re(&R, r"(?s)<[^>]*>").replace_all(s, ""); entities(&t).split_whitespace().collect::<Vec<_>>().join(" ") }

/// The title and the readable text of an HTML page, cut to `max` characters. Pure.
pub fn text_of(html: &str, max: usize) -> (String, String) {
    static TITLE: OnceLock<Regex> = OnceLock::new(); static DROP: OnceLock<Regex> = OnceLock::new(); static COMMENT: OnceLock<Regex> = OnceLock::new(); static BLOCK: OnceLock<Regex> = OnceLock::new(); static TAG: OnceLock<Regex> = OnceLock::new();
    let title = re(&TITLE, r"(?is)<title[^>]*>(.*?)</title>").captures(html).map(|c| strip_tags(&c[1])).unwrap_or_default();
    let h = re(&COMMENT, r"(?s)<!--.*?-->").replace_all(html, " ");
    // each of these is dropped up to ITS OWN closing tag, style and script first: one alternation matched `<style>` up to the first `</svg>` of ANY kind, so a stylesheet that holds an inline svg (a background image) left half its rules in the text (antiloki.com, 2026-11-21)
    let mut h = h.into_owned(); for tag in ["style", "script", "noscript", "svg", "template", "head"] { h = Regex::new(&format!(r"(?is)<{tag}\b[^>]*>.*?</{tag}\s*>")).unwrap().replace_all(&h, " ").into_owned(); }
    let _ = &DROP;
    let h = re(&BLOCK, r"(?i)</?(p|div|br|li|ul|ol|h[1-6]|tr|table|section|article|header|footer|nav|main|pre|blockquote)\b[^>]*>").replace_all(&h, "\n");
    let h = re(&TAG, r"(?s)<[^>]*>").replace_all(&h, ""); let h = entities(&h);
    let text = h.lines().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
    (title, text.chars().take(max).collect())
}

/// The results in the HTML of DuckDuckGo's search page (`result__a` links wrapped in `/l/?uddg=<url>`, `result__snippet`). Pure.
pub fn parse_ddg(html: &str, max: usize) -> Vec<Hit> {
    static A: OnceLock<Regex> = OnceLock::new(); static HREF: OnceLock<Regex> = OnceLock::new(); static SNIP: OnceLock<Regex> = OnceLock::new();
    let a = re(&A, r#"(?is)<a\b[^>]*class="[^"]*result__a[^"]*"[^>]*>(.*?)</a>"#); let tags = re(&HREF, r#"(?is)<a\b[^>]*class="[^"]*result__a[^"]*"[^>]*href="([^"]+)""#); let snip = re(&SNIP, r#"(?is)class="[^"]*result__snippet[^"]*"[^>]*>(.*?)</a>"#);
    let spots: Vec<(usize, usize, String)> = a.captures_iter(html).filter_map(|c| { let m = c.get(0)?; Some((m.start(), m.end(), strip_tags(&c[1]))) }).collect();
    let hrefs: Vec<String> = tags.captures_iter(html).map(|c| entities(&c[1])).collect();
    let mut out = vec![];
    for (i, (_, end, title)) in spots.iter().enumerate() {
        let Some(raw) = hrefs.get(i) else { continue }; let next = spots.get(i + 1).map_or(html.len(), |s| s.0);
        let url = real_url(raw); if url.is_empty() || title.is_empty() { continue; }
        let snippet = snip.captures(&html[*end..next]).map(|c| strip_tags(&c[1])).unwrap_or_default();
        if out.iter().any(|h: &Hit| h.url == url) { continue; }
        out.push(Hit { title: title.clone(), url, snippet }); if out.len() >= max { break; }
    }
    out
}
/// DuckDuckGo wraps a result's address (`//duckduckgo.com/l/?uddg=<encoded>&rut=…`); the address itself is what a node wants. Pure.
pub fn real_url(href: &str) -> String {
    let h = if let Some(r) = href.strip_prefix("//") { format!("https://{r}") } else { href.to_string() };
    if let Ok(u) = reqwest::Url::parse(&h) { if u.host_str().map_or(false, |x| x.ends_with("duckduckgo.com")) && u.path().starts_with("/l/") { if let Some((_, v)) = u.query_pairs().find(|(k, _)| k == "uddg") { return v.to_string(); } return String::new(); } }
    if h.starts_with("http://") || h.starts_with("https://") { h } else { String::new() }
}

/// An address inside this machine or its network: never fetched. Pure.
pub fn ip_blocked(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => { let o = v.octets(); v.is_loopback() || v.is_private() || v.is_link_local() || v.is_unspecified() || v.is_multicast() || v.is_broadcast() || v.is_documentation() || (o[0] == 100 && (64..=127).contains(&o[1])) || o[0] == 0 }
        IpAddr::V6(v) => { let s = v.segments(); v.is_loopback() || v.is_unspecified() || v.is_multicast() || (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80 || v.to_ipv4_mapped().map_or(false, |m| ip_blocked(&IpAddr::V4(m))) }
    }
}
fn allow_local() -> bool { std::env::var("ANTILOKI_WEB_ALLOW_LOCAL").map_or(false, |v| v == "1") }
/// A URL a node may fetch: http or https, a host, no credentials in it. Pure.
pub fn allowed_url(u: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(u.trim()).map_err(|_| format!("`{u}` is not an address"))?;
    if !matches!(url.scheme(), "http" | "https") { return Err("only http and https pages are read".into()); }
    if url.host_str().map_or(true, |h| h.is_empty()) { return Err("an address needs a host".into()); }
    if !url.username().is_empty() || url.password().is_some() { return Err("an address with a user name or password is not read".into()); }
    Ok(url)
}
async fn guard_host(url: &reqwest::Url) -> Result<(), String> {
    if allow_local() { return Ok(()); }
    let host = url.host_str().unwrap_or(""); let port = url.port_or_known_default().unwrap_or(80);
    if let Ok(ip) = host.trim_matches(|c| c == '[' || c == ']').parse::<IpAddr>() { return if ip_blocked(&ip) { Err(format!("{host} is an address inside this machine or its network: not read")) } else { Ok(()) }; }
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") || host.ends_with(".local") || host.ends_with(".internal") { return Err(format!("{host} is a local name: not read")); }
    let addrs: Vec<_> = tokio::net::lookup_host((host, port)).await.map_err(|e| format!("could not find {host}: {e}"))?.collect();
    if addrs.is_empty() { return Err(format!("could not find {host}")); }
    if let Some(a) = addrs.iter().find(|a| ip_blocked(&a.ip())) { return Err(format!("{host} points at {} inside this machine or its network: not read", a.ip())); }
    Ok(())
}
fn client(ua: &str, secs: u64) -> Result<reqwest::Client, String> { reqwest::Client::builder().user_agent(ua.to_string()).timeout(std::time::Duration::from_secs(secs)).redirect(reqwest::redirect::Policy::none()).build().map_err(|e| e.to_string()) }

/// GET one page, following at most four redirects (each one checked like the first), and give back (final url, content type, body).
async fn get(url: &str) -> Result<(String, String, String), String> { get_with(url, UA, 20).await }
async fn get_with(url: &str, ua: &str, secs: u64) -> Result<(String, String, String), String> {
    let c = client(ua, secs)?; let mut cur = allowed_url(url)?;
    for _ in 0..5 {
        guard_host(&cur).await?;
        let r = c.get(cur.clone()).header("accept", "text/html,text/plain,application/json;q=0.9,*/*;q=0.5").send().await.map_err(|e| format!("could not read {cur}: {e}"))?;
        if r.status().is_redirection() { let Some(loc) = r.headers().get("location").and_then(|v| v.to_str().ok()) else { return Err("a redirect with nowhere to go".into()) }; cur = allowed_url(cur.join(loc).map_err(|e| e.to_string())?.as_str())?; continue; }
        if !r.status().is_success() { return Err(format!("{cur} answered {}", r.status().as_u16())); }
        let ct = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
        if r.content_length().map_or(false, |n| n as usize > MAX_PAGE_BYTES * 4) { return Err("that page is too big to read".into()); }
        let bytes = r.bytes().await.map_err(|e| e.to_string())?; let slice = &bytes[..bytes.len().min(MAX_PAGE_BYTES)];
        return Ok((cur.to_string(), ct, String::from_utf8_lossy(slice).to_string()));
    }
    Err("too many redirects".into())
}

/// Read one page as text (html is reduced to its readable text; text, markdown and json are as they are).
pub async fn fetch(url: &str, max_chars: usize) -> Result<Value, String> {
    let (final_url, ct, body) = get(url).await?;
    let (title, text) = if ct.contains("html") || (ct.is_empty() && body.trim_start().starts_with('<')) { text_of(&body, max_chars) } else if ct.contains("text/") || ct.contains("json") || ct.contains("xml") { (String::new(), body.chars().take(max_chars).collect()) } else { return Err(format!("{final_url} is not a text page ({ct})")) };
    Ok(json!({ "ok": true, "url": final_url, "title": title, "text": text, "chars": text.chars().count() }))
}

// ── API (BL-18): a team calls ANY http API the Leader wires, not only an installed app. The method decides the risk (GET reads, POST/PUT/PATCH write, DELETE destroys); the same address guard as `fetch` (nothing inside this machine
// or its network); no redirect is followed (a redirect on a write could carry the body somewhere else); the secret is a NAME in the token store, sent as a bearer header and never written to a transcript or an output.
pub fn api_risk(method: &str) -> &'static str { match method.to_ascii_uppercase().as_str() { "GET" | "HEAD" => "read", "DELETE" => "destructive", _ => "write" } }
/// Headers a node may set: names and values that are plain text; never `authorization`, `cookie` or `host` (the token store and the address own those). Pure.
pub fn api_headers_ok(h: &Value) -> Result<Vec<(String, String)>, String> {
    let Some(o) = h.as_object() else { return if h.is_null() { Ok(vec![]) } else { Err("`headers` is an object of names and values".into()) } }; let mut out = vec![];
    for (k, v) in o { let lk = k.to_ascii_lowercase(); if ["authorization", "cookie", "host", "content-length", "proxy-authorization"].contains(&lk.as_str()) { return Err(format!("the header `{k}` is not set by a node: a secret goes in the token store (`token`)")); }
        if !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') || k.is_empty() { return Err(format!("`{k}` is not a header name")); } let v = v.as_str().ok_or_else(|| format!("the header `{k}` is text"))?; if v.contains('\n') || v.contains('\r') { return Err(format!("the header `{k}` has a line break")); } out.push((k.clone(), v.to_string())); }
    Ok(out)
}
pub async fn api_call(method: &str, url: &str, headers: &[(String, String)], body: Option<String>, bearer: Option<String>) -> Result<Value, String> {
    let m = reqwest::Method::from_bytes(method.to_ascii_uppercase().as_bytes()).map_err(|_| format!("`{method}` is not an http method"))?;
    if !["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"].contains(&m.as_str()) { return Err(format!("{} is not allowed: GET, HEAD, POST, PUT, PATCH or DELETE", m)); }
    let u = allowed_url(url)?; guard_host(&u).await?; let c = client(UA, 30)?;
    let mut rb = c.request(m, u.clone()).header("accept", "application/json, text/plain;q=0.9, */*;q=0.5"); for (k, v) in headers { rb = rb.header(k.as_str(), v.as_str()); }
    if let Some(t) = bearer { rb = rb.bearer_auth(t); }
    if let Some(b) = body { let is_json = b.trim_start().starts_with('{') || b.trim_start().starts_with('['); if is_json && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) { rb = rb.header("content-type", "application/json"); } rb = rb.body(b); }
    let r = rb.send().await.map_err(|e| format!("the call to {} failed: {}", u.host_str().unwrap_or(""), e.without_url()))?;
    let status = r.status().as_u16(); let ct = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_lowercase();
    let bytes = r.bytes().await.map_err(|e| e.to_string())?; let text: String = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_PAGE_BYTES)]).chars().take(MAX_TEXT_CHARS).collect();
    let json_v = if ct.contains("json") { serde_json::from_str::<Value>(&text).ok() } else { None };
    Ok(json!({ "ok": (200..300).contains(&status), "status": status, "text": text, "json": json_v, "content_type": ct }))
}

// ── DOCS (the operator's video team: "it downloaded the docs ... but I think it was the link"). A docs tool READS a documentation site into FILES the team keeps, so every agent after it can read the real pages with Read/Grep
// instead of being handed a link. Same host and same path prefix as the start page only, a page cap, the same address guard as `fetch`. No model.
/// The links of an HTML page resolved against its address, fragments dropped, kept only on the same host and under the same directory as `start`, in order, no repeats. Pure.
pub fn doc_links(html: &str, base: &reqwest::Url, start: &reqwest::Url) -> Vec<reqwest::Url> {
    static R: OnceLock<Regex> = OnceLock::new(); let re = re(&R, r#"(?i)\bhref\s*=\s*["']([^"'#\s]+)(?:#[^"']*)?["']"#);
    let dir = start.path().rsplit_once('/').map(|(d, _)| format!("{d}/")).unwrap_or_else(|| "/".into()); let mut out: Vec<reqwest::Url> = vec![];
    for c in re.captures_iter(html) {
        let Ok(mut u) = base.join(&c[1]) else { continue }; u.set_fragment(None); u.set_query(None);
        if u.scheme() != start.scheme() || u.host_str() != start.host_str() || u.port() != start.port() || !u.path().starts_with(&dir) { continue; }
        let last = u.path().rsplit('/').next().unwrap_or(""); if last.contains('.') && !["html", "htm", "md", "txt"].iter().any(|e| last.to_ascii_lowercase().ends_with(&format!(".{e}"))) { continue; }
        if !out.contains(&u) { out.push(u); }
    }
    out
}
/// A file name for a page: its path with the separators turned into dashes, `index` for the root. Pure.
pub fn doc_file(u: &reqwest::Url) -> String {
    let p = u.path().trim_matches('/'); let p = if p.is_empty() { "index".to_string() } else { p.replace('/', "-") }; let p: String = p.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect();
    let p = p.trim_end_matches(".html").trim_end_matches(".htm").trim_end_matches(".md").to_string(); format!("{}.md", p.chars().take(100).collect::<String>())
}
/// Read up to `max_pages` pages of a documentation site, breadth first from `start`, into `dir` as Markdown files (`# title`, the address, the text). Returns what was saved. The first page failing is an error; a later one is skipped.
pub async fn crawl_docs(start: &str, max_pages: usize, dir: &std::path::Path) -> Result<Value, String> {
    let start_url = allowed_url(start)?; let max_pages = max_pages.clamp(1, 30); std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let (mut queue, mut seen, mut saved) = (std::collections::VecDeque::from([start_url.clone()]), vec![start_url.clone()], vec![]);
    while let Some(u) = queue.pop_front() {
        if saved.len() >= max_pages { break; }
        let got = get(u.as_str()).await; let (final_url, ct, body) = match got { Ok(x) => x, Err(e) => { if saved.is_empty() { return Err(e); } continue } };
        let is_html = ct.contains("html") || (ct.is_empty() && body.trim_start().starts_with('<')); if !is_html && !ct.contains("text/") { continue; }
        let base = reqwest::Url::parse(&final_url).unwrap_or(u.clone());
        let (title, text) = if is_html { text_of(&body, MAX_TEXT_CHARS * 4) } else { (String::new(), body.chars().take(MAX_TEXT_CHARS * 4).collect()) };
        let name = doc_file(&base); let md = format!("# {}\n\n<{}>\n\n{}\n", if title.is_empty() { name.trim_end_matches(".md") } else { title.as_str() }, base, text);
        std::fs::write(dir.join(&name), md).map_err(|e| e.to_string())?; saved.push(json!({ "url": base.as_str(), "title": title, "file": name, "chars": text.chars().count() }));
        if is_html { for l in doc_links(&body, &base, &start_url) { if !seen.contains(&l) { seen.push(l.clone()); queue.push_back(l); } } }
    }
    let index: String = saved.iter().map(|p| format!("- [{}]({}) - {}\n", p["title"].as_str().filter(|t| !t.is_empty()).unwrap_or(p["file"].as_str().unwrap_or("")), p["file"].as_str().unwrap_or(""), p["url"].as_str().unwrap_or(""))).collect();
    let _ = std::fs::write(dir.join("INDEX.md"), format!("# Docs read from {start}\n\n{index}"));
    Ok(json!({ "ok": true, "dir": dir.to_string_lossy(), "pages": saved.len(), "files": saved, "text": format!("Read {} page(s) of the documentation into {}. Start with INDEX.md there; read the files with Read or Grep.", saved.len(), dir.display()) }))
}

/// Brave Search's API (a key from the person: `ANTILOKI_BRAVE_KEY`; the free tier is plenty for a team): the best answer, no model. None when there is no key.
pub async fn brave(query: &str, max: usize) -> Option<Result<Value, String>> {
    let key = std::env::var("ANTILOKI_BRAVE_KEY").ok().filter(|k| !k.trim().is_empty())?;
    Some(async {
        let url = format!("https://api.search.brave.com/res/v1/web/search?q={}&count={}", enc(query), max.clamp(1, 10));
        let r = client(UA, 15)?.get(&url).header("accept", "application/json").header("x-subscription-token", key.trim()).send().await.map_err(|e| format!("brave: {e}"))?;
        if !r.status().is_success() { return Err(format!("brave answered {}", r.status().as_u16())); }
        let v: Value = r.json().await.map_err(|e| format!("brave: {e}"))?;
        let hits: Vec<Hit> = v["web"]["results"].as_array().into_iter().flatten().filter_map(|x| Some(Hit { title: strip_tags(x["title"].as_str()?), url: x["url"].as_str()?.to_string(), snippet: strip_tags(x["description"].as_str().unwrap_or("")) })).take(max).collect();
        Ok(shape(query, "brave", &hits))
    }.await)
}
/// The answer of a search, in the one shape every engine gives: the results and the same as text for the nodes after. Pure.
pub fn shape(query: &str, engine: &str, hits: &[Hit]) -> Value {
    if hits.is_empty() { return json!({ "ok": true, "query": query, "engine": engine, "count": 0, "results": [], "text": format!("No results for \"{query}\".") }); }
    let text = hits.iter().enumerate().map(|(i, h)| format!("{}. {}\n   {}\n   {}", i + 1, h.title, h.url, h.snippet)).collect::<Vec<_>>().join("\n");
    json!({ "ok": true, "query": query, "engine": engine, "count": hits.len(), "results": hits.iter().map(|h| json!({ "title": h.title, "url": h.url, "snippet": h.snippet })).collect::<Vec<_>>(), "text": text })
}
/// Results out of a reply that holds JSON `{"results":[{title,url,snippet}]}` somewhere in its words (a seat that searched and answered). Pure.
pub fn hits_in_reply(reply: &str, max: usize) -> Vec<Hit> {
    let (Some(a), Some(b)) = (reply.find('{'), reply.rfind('}')) else { return vec![] }; if b <= a { return vec![]; }
    let Ok(v) = serde_json::from_str::<Value>(&reply[a..=b]) else { return vec![] };
    v["results"].as_array().into_iter().flatten().filter_map(|r| { let url = r["url"].as_str()?.to_string(); if !(url.starts_with("http://") || url.starts_with("https://")) { return None; } Some(Hit { title: r["title"].as_str()?.to_string(), url, snippet: r["snippet"].as_str().unwrap_or("").to_string() }) }).take(max).collect()
}
/// base64 (standard or url-safe, with or without padding): Bing wraps a result's address in one. Pure.
pub fn b64(s: &str) -> Option<Vec<u8>> {
    let mut out = vec![]; let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.trim_end_matches('=').bytes() { let v = match c { b'A'..=b'Z' => c - b'A', b'a'..=b'z' => c - b'a' + 26, b'0'..=b'9' => c - b'0' + 52, b'+' | b'-' => 62, b'/' | b'_' => 63, _ => return None } as u32; acc = (acc << 6) | v; bits += 6; if bits >= 8 { bits -= 8; out.push((acc >> bits) as u8); acc &= (1 << bits) - 1; } }
    Some(out)
}
/// The results in the HTML of Bing's page (`li.b_algo`: an `h2` link wrapped in `/ck/a?...&u=a1<base64 of the address>`, a `b_lineclamp` snippet). Pure.
pub fn parse_bing(html: &str, max: usize) -> Vec<Hit> {
    static BLOCK: OnceLock<Regex> = OnceLock::new(); static H2: OnceLock<Regex> = OnceLock::new(); static P: OnceLock<Regex> = OnceLock::new();
    let (block, h2, p) = (re(&BLOCK, r#"(?s)<li class="b_algo".*?</li>"#), re(&H2, r#"(?is)<h2[^>]*>\s*<a\b[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#), re(&P, r#"(?is)<p\b[^>]*>(.*?)</p>"#));
    let mut out: Vec<Hit> = vec![];
    for b in block.find_iter(html) {
        let Some(c) = h2.captures(b.as_str()) else { continue }; let url = bing_url(&entities(&c[1])); let title = strip_tags(&c[2]); if url.is_empty() || title.is_empty() || out.iter().any(|h| h.url == url) { continue; }
        out.push(Hit { title, url, snippet: p.captures(b.as_str()).map(|x| strip_tags(&x[1])).unwrap_or_default() }); if out.len() >= max { break; }
    }
    out
}
fn bing_url(href: &str) -> String {
    let Ok(u) = reqwest::Url::parse(href) else { return String::new() };
    if u.host_str().map_or(false, |h| h.ends_with("bing.com")) { if let Some((_, v)) = u.query_pairs().find(|(k, _)| k == "u") { if let Some(rest) = v.strip_prefix("a1") { if let Some(bytes) = b64(rest) { if let Ok(t) = String::from_utf8(bytes) { if t.starts_with("http") { return t; } } } } } return String::new(); }
    if u.scheme().starts_with("http") { href.to_string() } else { String::new() }
}

/// The engine that answered last, tried first the next time: one that is not reachable from this network costs its timeout only once.
static LAST: std::sync::Mutex<Option<&'static str>> = std::sync::Mutex::new(None);
fn hits_of(ct: &str, body: &str, kind: &str, max: usize) -> Result<Vec<Hit>, String> {
    if kind == "json" || ct.contains("json") || body.trim_start().starts_with('{') {
        let v: Value = serde_json::from_str(body).map_err(|e| format!("the search answered something that is not JSON: {e}"))?;
        return Ok(v["results"].as_array().into_iter().flatten().filter_map(|r| Some(Hit { title: r["title"].as_str()?.to_string(), url: r["url"].as_str()?.to_string(), snippet: r["snippet"].as_str().or_else(|| r["content"].as_str()).unwrap_or("").to_string() })).take(max).collect());
    }
    Ok(if kind == "bing" { parse_bing(body, max) } else { parse_ddg(body, max) })
}
pub fn enc(q: &str) -> String { reqwest::Url::parse_with_params("http://x/", &[("q", q)]).ok().and_then(|u| u.query().map(|s| s.trim_start_matches("q=").to_string())).unwrap_or_default() }
/// Search the web: the results (title, address, snippet) and the same as text for the nodes after. `ANTILOKI_WEB_SEARCH_URL` (a template with `{q}`) is the only engine when it is set; otherwise DuckDuckGo and Bing, the one that answered last first.
pub async fn search(query: &str, max: usize) -> Result<Value, String> {
    let q = query.trim(); if q.is_empty() { return Err("a search needs words to search for".into()); } let max = max.clamp(1, 10); let e = enc(q);
    let mut engines: Vec<(&'static str, String)> = match std::env::var("ANTILOKI_WEB_SEARCH_URL").ok().filter(|t| t.contains("{q}")) { Some(t) => vec![("custom", t.replace("{q}", &e))], None => vec![("ddg", format!("https://html.duckduckgo.com/html/?q={e}")), ("bing", format!("https://www.bing.com/search?q={e}&setlang=en"))] };
    if let Some(l) = *LAST.lock().unwrap_or_else(|p| p.into_inner()) { if let Some(i) = engines.iter().position(|x| x.0 == l) { let x = engines.remove(i); engines.insert(0, x); } }
    let mut errors = vec![];
    for (name, url) in engines {
        match get_with(&url, BROWSER_UA, if name == "custom" { 20 } else { 8 }).await {
            Ok((_, ct, body)) => match hits_of(&ct, &body, if name == "custom" { "" } else { name }, max) {
                Ok(hits) if !hits.is_empty() => { *LAST.lock().unwrap_or_else(|p| p.into_inner()) = Some(match name { "ddg" => "ddg", "bing" => "bing", _ => "custom" });
                    let text = hits.iter().enumerate().map(|(i, h)| format!("{}. {}\n   {}\n   {}", i + 1, h.title, h.url, h.snippet)).collect::<Vec<_>>().join("\n");
                    return Ok(json!({ "ok": true, "query": q, "engine": name, "count": hits.len(), "results": hits.iter().map(|h| json!({ "title": h.title, "url": h.url, "snippet": h.snippet })).collect::<Vec<_>>(), "text": text })); }
                Ok(_) => errors.push(format!("{name}: no results")), Err(e) => errors.push(format!("{name}: {e}")) },
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    // every engine answered "nothing" (not an error): say so; every engine failed: say why
    if errors.iter().all(|e| e.ends_with("no results")) { return Ok(json!({ "ok": true, "query": q, "count": 0, "results": [], "text": format!("No results for \"{q}\".") })); }
    Err(format!("no search engine answered: {}", errors.join("; ")))
}

/// A template with `{ask}` and `{node.field}` filled from the run: how a search names what to look for. An unknown name stays as written. Pure.
pub fn fill(template: &str, ask: &str, values: &Map<String, Value>) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"\{([A-Za-z0-9_.-]+)\}").replace_all(template, |c: &regex::Captures| {
        let k = &c[1]; if k == "ask" { return ask.to_string(); }
        let mut it = k.splitn(2, '.'); let (n, f) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        match values.get(n).and_then(|v| v.get(f)) { Some(Value::String(s)) => s.clone(), Some(Value::Null) | None => c[0].to_string(), Some(o) => o.to_string() }
    }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    const DDG: &str = r#"<div class="results"><div class="result"><h2><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fcoffee%3Fa%3D1&amp;rut=abc">Single-origin <b>coffee</b> guide</a></h2><a class="result__snippet" href="x">How to taste &amp; brew single origin.</a></div>
<div class="result"><a class="result__a" href="https://roaster.test/page">A roaster</a><a class="result__snippet" href="y">Fresh beans weekly.</a></div><div class="result"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fcoffee%3Fa%3D1">Same page again</a></div></div>"#;
    #[test]
    fn a_search_page_gives_titles_real_addresses_and_snippets_once_each() {
        let h = parse_ddg(DDG, 5); assert_eq!(h.len(), 2, "{h:?}"); assert_eq!(h[0], Hit { title: "Single-origin coffee guide".into(), url: "https://example.com/coffee?a=1".into(), snippet: "How to taste & brew single origin.".into() });
        assert_eq!(h[1].url, "https://roaster.test/page"); assert_eq!(parse_ddg(DDG, 1).len(), 1); assert!(parse_ddg("<html>nothing</html>", 5).is_empty());
        assert_eq!(real_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.test%2F%3Fq%3D1"), "https://a.test/?q=1"); assert_eq!(real_url("javascript:alert(1)"), ""); assert_eq!(real_url("//duckduckgo.com/l/?rut=x"), "");
    }
    #[test]
    fn bings_page_gives_the_real_address_out_of_its_wrapped_link() {
        let html = r#"<li class="b_algo" data-id><h2><a target="_blank" href="https://www.bing.com/ck/a?!&amp;&amp;p=abc&amp;u=a1aHR0cHM6Ly9leGFtcGxlLmNvbS9jb2ZmZWU&amp;ntb=1" h="x">Coffee <strong>guide</strong></a></h2><div class="b_caption"><p class="b_lineclamp2">How to brew.</p></div></li><li class="b_algo"><h2><a href="https://direct.test/page">Direct</a></h2></li>"#;
        let h = parse_bing(html, 5); assert_eq!(h.len(), 2, "{h:?}"); assert_eq!(h[0], Hit { title: "Coffee guide".into(), url: "https://example.com/coffee".into(), snippet: "How to brew.".into() }); assert_eq!(h[1].url, "https://direct.test/page");
        assert_eq!(b64("aHR0cHM6Ly93d3cuc2luZ2xldGVjaC5jb20uYnIv").map(|b| String::from_utf8(b).unwrap()).as_deref(), Some("https://www.singletech.com.br/")); assert!(b64("a b").is_none());
    }
    #[test]
    fn numeric_entities_are_text_and_a_seats_reply_holds_results_in_one_shape() {
        assert_eq!(entities("Tradu&#231;&#227;o &#x41; &amp; &#99999999;"), "Tradução A & &#99999999;");
        let h = hits_in_reply("Here you go:\n{\"results\": [{\"title\": \"A\", \"url\": \"https://a.test\", \"snippet\": \"s\"}, {\"title\": \"B\", \"url\": \"javascript:x\"}]}", 5); assert_eq!(h.len(), 1, "{h:?}"); assert!(hits_in_reply("no json", 3).is_empty());
        let v = shape("q", "seat", &h); assert_eq!(v["count"], json!(1)); assert!(v["text"].as_str().unwrap().starts_with("1. A\n   https://a.test")); assert_eq!(shape("q", "seat", &[])["count"], json!(0));
    }
    #[test]
    fn a_page_is_reduced_to_its_title_and_readable_text_without_scripts_or_styles() {
        let css = text_of("<style>.a{background:url(\"data:image/svg+xml,<svg><rect/></svg>\");animation:grain 1s}</style><p>Real words</p>", 1000).1; assert_eq!(css, "Real words", "a stylesheet with an inline svg must not leak into the text: {css:?}");
        let (t, x) = text_of("<html><head><title>Roast &amp; Co</title><style>p{color:red}</style></head><body><script>alert(1)</script><h1>Hello</h1><p>One  two<br>three</p><!-- hidden --><ul><li>a</li><li>b</li></ul></body></html>", 1000);
        assert_eq!(t, "Roast & Co"); assert_eq!(x, "Hello\nOne two\nthree\na\nb"); assert!(!x.contains("alert") && !x.contains("color") && !x.contains("hidden"));
        assert_eq!(text_of("<p>abcdefghij</p>", 4).1, "abcd", "cut at the limit");
    }
    #[test]
    fn nothing_inside_this_machine_or_its_network_is_read() {
        for ip in ["127.0.0.1", "10.0.0.5", "192.168.1.107", "172.17.0.1", "169.254.169.254", "100.64.0.1", "0.0.0.0", "::1", "fd00::1", "fe80::1", "::ffff:127.0.0.1"] { assert!(ip_blocked(&ip.parse().unwrap()), "{ip}"); }
        for ip in ["8.8.8.8", "93.184.216.34", "2606:4700:4700::1111"] { assert!(!ip_blocked(&ip.parse().unwrap()), "{ip}"); }
        assert!(allowed_url("https://example.com/a").is_ok()); for bad in ["ftp://x.test/", "file:///etc/passwd", "https://user:pw@x.test/", "not a url", "http://"] { assert!(allowed_url(bad).is_err(), "{bad}"); }
    }
    #[test]
    fn a_search_names_what_to_look_for_from_the_ask_and_from_what_a_node_wrote() {
        let mut v = Map::new(); v.insert("brief".into(), json!({ "report": "single origin", "n": 3 }));
        assert_eq!(fill("{ask} news about {brief.report} ({brief.n}) {nobody.x} {ask}", "coffee", &v), "coffee news about single origin (3) {nobody.x} coffee");
    }

    /// Against the real web (needs a network): `cargo test -p antiloki-serve --lib team_web -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs the network"]
    async fn the_real_web_answers_a_search_and_a_page_and_refuses_this_machine() {
        let r = search("single origin coffee brewing", 5).await.expect("search"); println!("{}", r["text"].as_str().unwrap_or("")); assert!(r["count"].as_i64().unwrap_or(0) >= 1, "{r}");
        let url = r["results"][0]["url"].as_str().unwrap().to_string(); let p = fetch(&url, 600).await; println!("{url}: {:?}", p.as_ref().map(|v| v["title"].clone())); assert!(p.is_ok() || p.unwrap_err().contains("answered"), "a page may refuse a bot, never crash");
        assert!(fetch("http://127.0.0.1:41931/healthz", 100).await.is_err(), "this machine is not read"); assert!(fetch("http://169.254.169.254/latest/meta-data", 100).await.is_err());
    }

    #[test]
    fn a_docs_crawl_stays_on_the_site_and_under_the_start_directory_and_names_its_files() {
        let start = reqwest::Url::parse("https://docs.hyperframes.dev/guide/intro.html").unwrap(); let base = start.clone();
        let html = r##"<a href="/guide/install">i</a><a href="usage.html#top">u</a><a href="../api/x">out of the directory</a><a href="https://evil.com/guide/a">other host</a><a href="/guide/logo.png">img</a><a href="/guide/install?x=1">dup</a><a href="mailto:a@b.c">m</a><a href="/guide/sub/deep.md">d</a>"##;
        let got: Vec<String> = doc_links(html, &base, &start).iter().map(|u| u.path().to_string()).collect();
        assert_eq!(got, vec!["/guide/install", "/guide/usage.html", "/guide/sub/deep.md"], "{got:?}");
        assert_eq!(doc_file(&reqwest::Url::parse("https://x.dev/guide/usage.html").unwrap()), "guide-usage.md"); assert_eq!(doc_file(&reqwest::Url::parse("https://x.dev/").unwrap()), "index.md"); assert_eq!(doc_file(&reqwest::Url::parse("https://x.dev/a b/c").unwrap()), "a_20b-c.md");   // the address spells a space %20
    }

    #[test]
    fn an_api_methods_risk_and_the_headers_a_node_may_set_are_decided_before_any_call() {
        assert_eq!((api_risk("get"), api_risk("HEAD"), api_risk("POST"), api_risk("patch"), api_risk("DELETE")), ("read", "read", "write", "write", "destructive"));
        assert_eq!(api_headers_ok(&json!({ "X-Api-Version": "2", "Accept-Language": "pt" })).unwrap().len(), 2); assert!(api_headers_ok(&Value::Null).unwrap().is_empty());
        for bad in [json!({ "Authorization": "Bearer x" }), json!({ "cookie": "a=b" }), json!({ "Host": "evil" }), json!({ "X-A": "a\nb" }), json!({ "X-A": 1 }), json!({ "bad name": "x" }), json!(["x"])] { assert!(api_headers_ok(&bad).is_err(), "{bad}"); }
    }
    #[tokio::test]
    async fn an_api_call_refuses_this_machines_own_addresses_and_odd_methods_before_sending_anything() {
        let e = api_call("GET", "http://127.0.0.1:9/x", &[], None, None).await.unwrap_err(); assert!(e.contains("inside this machine") || e.contains("not read"), "{e}");
        assert!(api_call("TRACE", "https://example.com/", &[], None, None).await.unwrap_err().contains("not allowed")); assert!(api_call("GET", "ftp://example.com/", &[], None, None).await.is_err());
        assert!(api_call("GET", "http://user:pw@example.com/", &[], None, None).await.is_err(), "no credentials in the address");
    }
}
