//! magazine-observe.rs — twin of skills/codebase/magazine-observe.mjs (expeditions v4 faculty
//! magazine OBSERVATION). NAMES ARE CONTRACT for the JS *named exports*: `FACULTIES` ·
//! `pickTechs` · `parseDossier` · `computeDelta`. The internal (non-exported) JS helpers
//! `readDeps` / `readRootManifest` / `buildNotices` are ported too (same names) because they are
//! fully deterministic (real `std::fs`/`rusqlite`, no network) and independently testable.
//!
//! PARITY-DEFERRED (the file's actual purpose): every department builder that assembles its
//! payload via `cachedFetch` — `buildLedger`, `buildHorizon`, `buildWire`, `buildTechPress`,
//! `buildTechStats`, `buildRivals`, `buildDispatches`, `enrichArticles`, `discoverBlogFeed`, the
//! `dossier.site` branch of `buildProductOnPaper`, and the `magazineObserve` orchestrator itself
//! — is NOT ported. `cachedFetch` (skills/codebase/_helpers/web-cache.mjs) has no Rust body by
//! design (see web-cache.rs module doc: a real outbound network fetch is not fixture-portable),
//! so every builder that calls it has nothing faithful to call in its place. This is the
//! "file whose entire purpose is the deferred op" case from the playbook: the deterministic
//! surface that does NOT touch `cachedFetch` — the package census, the manifest reader, the tech
//! picker, the operator desk's dossier parser, the atlas-derived structural notices, and the
//! zero-AI "since the last issue" diff — is ported in full below with real `#[cfg(test)]`
//! coverage; the fetch-shaped builders stay JS-only until a deterministic fixture harness for
//! `cachedFetch` exists.
//!
//! Deps reused (already-converted twins, no duplication): `crate::codebase_feature_inventory`,
//! `crate::codebase__helpers_web_research::hashKey`, `crate::codebase__helpers_magazine_db`,
//! `crate::engine_workspace_root`. None of those are required by the ported surface below except
//! `hashKey` (dossier-drift hashing in `computeDelta`'s `product` branch).

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const FACULTIES: [&str; 2] = ["arch", "product"];
const MAX_DEPS: usize = 60;

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
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() } // JS .slice(0,n) (BMP-equivalent, disclosed approximation — see web-research.rs)

// strip leading [~^>=<\s]+ — same regex JS uses on a version string
fn strip_range_prefix(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^[~^>=<\s]+").unwrap());
    re.replace(s, "").to_string()
}

// ── readDeps: the package census (salvaged from deps-observatory) ──────────────────────────
// JS: reads root's package.json ('(root)'), then every top-level non-hidden, non-node_modules
// directory's package.json (labelled by dir name). dependencies/devDependencies merge into ONE
// name→{current,dev} map, first writer wins (root's own deps beat a workspace member's).
pub fn readDeps(root: &Path) -> Value {
    let mut packages: Vec<String> = Vec::new();
    let mut deps: Map<String, Value> = Map::new(); // insertion-ordered (serde_json preserve_order)

    let mut take = |dir: &Path, label: &str| {
        let pj = dir.join("package.json");
        let text = match std::fs::read_to_string(&pj) {
            Ok(t) => t,
            Err(_) => return, // unreadable / missing — skip, matches existsSync guard + try/catch
        };
        let doc: Value = match serde_json::from_str(&text) {
            Ok(d) => d,
            Err(_) => return, // malformed package.json — skip
        };
        packages.push(label.to_string());
        for (bag_key, dev) in [("dependencies", false), ("devDependencies", true)] {
            let bag = doc.get(bag_key).and_then(|b| b.as_object());
            let Some(bag) = bag else { continue };
            for (name, ver) in bag.iter() {
                if deps.contains_key(name) { continue; }
                let current = strip_range_prefix(&str_qq(Some(ver)));
                deps.insert(name.clone(), json!({ "current": current, "dev": dev }));
            }
        }
    };

    take(root, "(root)");
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if !ft.is_dir() { continue; }
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "node_modules" { continue; }
            take(&e.path(), &name);
        }
    }

    let entries: Vec<Value> = deps.into_iter().take(MAX_DEPS).map(|(k, v)| json!([k, v])).collect();
    json!({ "packages": packages, "deps": entries })
}

pub fn readRootManifest(root: &Path) -> Value {
    match std::fs::read_to_string(root.join("package.json")) {
        Ok(t) => serde_json::from_str(&t).unwrap_or_else(|_| json!({})),
        Err(_) => json!({}),
    }
}

// ── pickTechs (JS named export) ──────────────────────────────────────────────────────────────
// registry_docs: an object map name → registry doc (JS Map stand-in — a plain JSON object here).
pub fn pickTechs(ledger_deps: &Value, registry_docs: &Value, horizon_products: &Value, cap: usize) -> Value {
    let mut techs: Vec<(String, Value)> = Vec::new();
    let mut push = |techs: &mut Vec<(String, Value)>, name: Option<&Value>, homepage: Value| {
        let n = str_qq(name).to_lowercase();
        if !n.is_empty() && !techs.iter().any(|(t, _)| t == &n) {
            techs.push((n, homepage));
        }
    };

    for p in horizon_products.as_array().into_iter().flatten() {
        push(&mut techs, p.get("product"), Value::Null);
    }
    let empty = Map::new();
    let docs = registry_docs.as_object().unwrap_or(&empty);
    for d in ledger_deps.as_array().into_iter().flatten() {
        if d.get("dev").and_then(Value::as_bool).unwrap_or(false) { continue; }
        if techs.len() >= cap { break; }
        let name = str_qq(d.get("name"));
        let last_seg = name.rsplit('/').next().unwrap_or("").to_string();
        let doc = docs.get(&name);
        let homepage = match doc.and_then(|dc| dc.get("homepage")) {
            Some(Value::String(s)) => json!(s),
            _ => Value::Null,
        };
        push(&mut techs, Some(&json!(last_seg)), homepage);
    }

    let out: Vec<Value> = techs.into_iter().take(cap).map(|(name, homepage)| json!({ "name": name, "homepage": homepage })).collect();
    json!(out)
}

// ── parseDossier (JS named export) — the operator desk's "product dossier" doc, parsed
// deterministically: intention as data. ────────────────────────────────────────────────────────
fn dossier_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)product dossier").unwrap())
}
fn rival_line_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\s*(.+?)\s*—\s*(https://\S+)\s*$").unwrap())
}
fn site_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^https://").unwrap())
}

pub fn parseDossier(desk_items: &Value) -> Value {
    let items = desk_items.as_array().cloned().unwrap_or_default();
    let doc = match items.iter().rev().find(|d| dossier_re().is_match(&str_qq(d.get("label")))) {
        Some(d) => d.clone(),
        None => return Value::Null,
    };
    let body = str_qq(doc.get("body"));

    let line = |key: &str| -> String {
        let pat = format!(r"(?im)^{}\s*:\s*(.+)$", key);
        let re = Regex::new(&pat).expect("parseDossier: fixed key set is always a valid regex");
        re.captures(&body).and_then(|c| c.get(1)).map(|m| m.as_str().trim().to_string()).unwrap_or_default()
    };

    let rivals: Vec<Value> = line("rivals").split('|').filter_map(|s| {
        rival_line_re().captures(s).map(|c| json!({
            "name": take_chars(c.get(1).unwrap().as_str(), 60),
            "url": c.get(2).unwrap().as_str(),
        }))
    }).collect();

    let keywords: Vec<Value> = line("keywords").split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| s.chars().count() > 1)
        .take(6)
        .map(|s| json!(s))
        .collect();

    let site_line = line("site");
    let site = if site_re().is_match(&site_line) { json!(site_line) } else { Value::Null };

    let one_liner = take_chars(&line("one-liner"), 200);
    let audience = take_chars(&line("audience"), 200);
    let problem = take_chars(&line("problem"), 300);

    if one_liner.is_empty() && audience.is_empty() && keywords.is_empty() && rivals.is_empty() {
        return Value::Null;
    }
    json!({
        "one_liner": one_liner,
        "audience": audience,
        "problem": problem,
        "site": site,
        "rivals": rivals,
        "keywords": keywords,
    })
}

// ── buildNotices — the atlas-derived structural notices department. Fully deterministic
// (rusqlite + real fs stat), no network. Degrades to Null on any db/schema failure (JS try/catch
// around the whole body). ─────────────────────────────────────────────────────────────────────
pub fn buildNotices(db: &Connection, root: &Path) -> Value {
    let files: Vec<String> = match db.prepare(
        "SELECT path FROM code_nodes WHERE origin = 'atlas' AND type = 'file' AND superseded_by IS NULL",
    ).and_then(|mut st| {
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()
    }) {
        Ok(f) => f,
        Err(_) => return Value::Null,
    };
    if files.is_empty() { return Value::Null; }

    let mut giants: Vec<(String, i64)> = Vec::new();
    for p in files.iter().take(800) {
        if let Ok(meta) = std::fs::metadata(root.join(p)) {
            let tokens = ((meta.len() as f64) / 4.0).ceil() as i64;
            giants.push((p.clone(), tokens));
        }
    }
    giants.sort_by(|a, b| b.1.cmp(&a.1));
    let giants_json: Vec<Value> = giants.into_iter().take(6).map(|(path, tokens)| json!({ "path": path, "tokens": tokens })).collect();

    let hubs: Vec<Value> = match db.prepare(
        "SELECT tn.path AS path, COUNT(*) AS dependents FROM code_edges e \
         JOIN code_nodes tn ON tn.id = e.to_node \
         WHERE e.origin = 'atlas' AND e.type = 'imports' \
         GROUP BY tn.path ORDER BY dependents DESC LIMIT 6",
    ).and_then(|mut st| {
        let rows = st.query_map([], |r| {
            let path: String = r.get(0)?;
            let dependents: i64 = r.get(1)?;
            Ok(json!({ "path": path, "dependents": dependents }))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
    }) {
        Ok(h) => h,
        Err(_) => return Value::Null,
    };

    let edges: i64 = db.query_row(
        "SELECT COUNT(*) AS c FROM code_edges WHERE origin = 'atlas' AND type = 'imports'",
        [],
        |r| r.get(0),
    ).unwrap_or(0);

    json!({ "giants": giants_json, "hubs": hubs, "files": files.len(), "edges": edges })
}

// ── computeDelta (JS named export) — "since the last issue": the mechanical, zero-AI diff. ────
fn dossier_hash(r: &Value) -> String {
    let title = str_qq(r.get("title"));
    let description = str_qq(r.get("description"));
    let claims: Vec<String> = r.get("claims").and_then(Value::as_array).into_iter().flatten().map(|c| str_qq(Some(c))).collect();
    let text = json!(format!("{title}|{description}|{}", claims.join("\u{00B7}")));
    crate::codebase__helpers_web_research::hashKey(&text).as_str().unwrap_or_default().to_string()
}
fn arr_at<'a>(v: &'a Value, path: &[&str]) -> Vec<&'a Value> {
    let mut cur = v;
    for k in path {
        match cur.get(k) { Some(x) => cur = x, None => return vec![] }
    }
    cur.as_array().map(|a| a.iter().collect()).unwrap_or_default()
}

pub fn computeDelta(faculty: &str, prev: &Value, cur: &Value) -> Value {
    if prev.is_null() { return json!([]); }
    let mut out: Vec<String> = Vec::new();

    if faculty == "arch" {
        let mut p_adv: std::collections::HashSet<String> = std::collections::HashSet::new();
        for d in arr_at(prev, &["ledger", "deps"]) {
            let name = str_qq(d.get("name"));
            for a in d.get("advisories").and_then(Value::as_array).into_iter().flatten() {
                p_adv.insert(format!("{}:{}", name, str_qq(a.get("id"))));
            }
        }
        for d in arr_at(cur, &["ledger", "deps"]) {
            let name = str_qq(d.get("name"));
            for a in d.get("advisories").and_then(Value::as_array).into_iter().flatten() {
                let id = str_qq(a.get("id"));
                if !p_adv.contains(&format!("{name}:{id}")) {
                    out.push(format!("new advisory {id} on {name}"));
                }
            }
        }

        let mut p_behind: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        for d in arr_at(prev, &["ledger", "deps"]) {
            p_behind.insert(str_qq(d.get("name")), d.get("behind").and_then(Value::as_f64).unwrap_or(0.0));
        }
        for d in arr_at(cur, &["ledger", "deps"]) {
            let name = str_qq(d.get("name"));
            if let Some(&pb) = p_behind.get(&name) {
                let cb = d.get("behind").and_then(Value::as_f64).unwrap_or(0.0);
                if pb < 40.0 && cb >= 40.0 {
                    out.push(format!("{} fell a major behind ({} → {})", name, str_qq(d.get("current")), str_qq(d.get("latest"))));
                }
            }
        }

        let p_links: std::collections::HashSet<String> = arr_at(prev, &["wire", "items"]).iter().filter_map(|i| i.get("link")).map(|l| str_qq(Some(l))).collect();
        let fresh = arr_at(cur, &["wire", "items"]).iter().filter(|i| {
            let link = str_qq(i.get("link"));
            !link.is_empty() && !p_links.contains(&link)
        }).count();
        if fresh > 0 { out.push(format!("{} new stor{} on the wire", fresh, if fresh == 1 { "y" } else { "ies" })); }

        let p_posts: std::collections::HashSet<String> = arr_at(prev, &["press", "items"]).iter().filter_map(|i| i.get("link")).map(|l| str_qq(Some(l))).collect();
        let fresh_posts = arr_at(cur, &["press", "items"]).iter().filter(|i| {
            let link = str_qq(i.get("link"));
            !link.is_empty() && !p_posts.contains(&link)
        }).count();
        if fresh_posts > 0 { out.push(format!("{} new post{} in the tech press", fresh_posts, if fresh_posts == 1 { "" } else { "s" })); }

        let mut p_eol: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();
        for p in arr_at(prev, &["horizon", "products"]) {
            p_eol.insert(str_qq(p.get("product")), Some(tmpl(p.get("eol"))));
        }
        for p in arr_at(cur, &["horizon", "products"]) {
            let product = str_qq(p.get("product"));
            if let Some(Some(pe)) = p_eol.get(&product) {
                let ce = tmpl(p.get("eol"));
                if pe != &ce { out.push(format!("{product}: EOL moved to {ce}")); }
            }
        }
    } else {
        let mut p_riv: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for r in arr_at(prev, &["rivals", "rivals"]) {
            p_riv.insert(str_qq(r.get("name")), dossier_hash(r));
        }
        for r in arr_at(cur, &["rivals", "rivals"]) {
            let name = str_qq(r.get("name"));
            match p_riv.get(&name) {
                None => out.push(format!("new rival tracked: {name}")),
                Some(ph) => {
                    let ok = r.get("ok").and_then(Value::as_bool).unwrap_or(false);
                    if ok && ph != &dossier_hash(r) { out.push(format!("{name} changed their page")); }
                }
            }
        }

        let p_links: std::collections::HashSet<String> = arr_at(prev, &["dispatches", "items"]).iter().filter_map(|i| i.get("link")).map(|l| str_qq(Some(l))).collect();
        let fresh = arr_at(cur, &["dispatches", "items"]).iter().filter(|i| {
            let link = str_qq(i.get("link"));
            !link.is_empty() && !p_links.contains(&link)
        }).count();
        if fresh > 0 { out.push(format!("{} new field dispatch{}", fresh, if fresh == 1 { "" } else { "es" })); }
    }

    out.truncate(8);
    json!(out)
}

// JS `${v}` — used for the EOL comparison (String(p.eol) where eol may be a JSON bool/string/null)
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}


// ════════════════════════════════════════════════════════════════════════════════════════════
// THE FETCH-SHAPED DEPARTMENTS — ported 2026-08-09, once `cachedFetch` became real.
//
// These were the file's original PARITY-DEFERRED set: every builder that assembles its payload
// through `cachedFetch`, plus the `magazineObserve` orchestrator itself. The deferral's stated
// cause — "cachedFetch has no Rust body" — no longer holds (ureq's `tls` feature unblocked it),
// so the reason to defer is gone and they are ported here.
//
// Two shape differences from JS, both forced and both output-preserving:
//   * `inChunks(items, n, fn)` bounds JS's CONCURRENCY. This crate is synchronous, so the same
//     work runs sequentially. Concurrency is not observable in the return value — inChunks
//     preserves input order — so the assembled payload is identical; only wall-clock differs.
//   * JS holds `registryDocs` in a `Map`; here it is a plain JSON object keyed by package name,
//     matching the convention `pickTechs` already established above.
// ════════════════════════════════════════════════════════════════════════════════════════════

const MAX_RIVALS: usize = 8;
const MAX_WIRE_FEEDS: usize = 10;
const MAX_ITEMS: i64 = 30;
const MAX_ARTICLE_SCRAPES: usize = 8;

fn skill_v() -> Value { json!("magazine-observe") }

// JS truthiness — `undefined`/`null`/`false`/`0`/`""` are falsy, everything else (incl. `[]`/`{}`)
// is truthy. Same definition web-research.rs uses; kept local so this file has no cross-helper
// dependency for a two-line predicate.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0 && !f.is_nan()).unwrap_or(false),
        Some(Value::String(st)) => !st.is_empty(),
        _ => true,
    }
}

// The control block JS threads as `ctl` — {refresh, stats:{cached_hits,fetched}}.
pub struct Ctl {
    pub refresh: bool,
    pub stats: Value,
}
impl Ctl {
    fn new(refresh: bool) -> Self { Ctl { refresh, stats: json!({ "cached_hits": 0, "fetched": 0 }) } }
}

// JS `cachedFetch(...).catch(() => null)` — every builder swallows fetch failures. assertPublicHttps
// is the one Err this can raise (a non-https or private-host URL), and JS's `.catch` swallows that
// too, so a failure here is `Value::Null` exactly as in JS.
fn fetch_or_null(db: &Connection, url: &str, init: Option<&Value>, ctl: &mut Ctl, ttl_hours: f64) -> Value {
    crate::codebase__helpers_web_cache::cachedFetch(
        db, &json!(url), init, Some(&mut ctl.stats), ctl.refresh, ttl_hours, &Value::Null, &skill_v(),
    )
    .unwrap_or(Value::Null)
}

fn json_parse(body: &Value) -> Value {
    match body.as_str() {
        Some(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        None => Value::Null,
    }
}

fn https_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)^https://").unwrap())
}

// JS `new URL(u).hostname` / `.pathname` — the same minimal parse web-cache.rs uses.
fn url_host(u: &str) -> Option<String> {
    let idx = u.find("://")?;
    let rest = &u[idx + 3..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host_port = match authority.rfind('@') { Some(i) => &authority[i + 1..], None => authority };
    let host = match host_port.rfind(':') { Some(i) => &host_port[..i], None => host_port };
    if host.is_empty() { None } else { Some(host.to_lowercase()) }
}
fn url_path(u: &str) -> String {
    match u.find("://") {
        Some(idx) => {
            let rest = &u[idx + 3..];
            match rest.find('/') { Some(i) => rest[i..].split(['?', '#']).next().unwrap_or("").to_string(), None => String::new() }
        }
        None => String::new(),
    }
}
// JS `new URL(href, base).toString()` for the two forms a feed <link href> actually takes.
fn url_resolve(href: &str, base: &str) -> Option<String> {
    if https_re().is_match(href) || href.starts_with("http://") { return Some(href.to_string()); }
    let host = url_host(base)?;
    let scheme = base.split("://").next()?;
    if let Some(stripped) = href.strip_prefix('/') {
        return Some(format!("{scheme}://{host}/{stripped}"));
    }
    let path = url_path(base);
    let dir = match path.rfind('/') { Some(i) => &path[..=i], None => "/" };
    Some(format!("{scheme}://{host}{dir}{href}"))
}

// JS `encodeURIComponent` for the characters these call sites actually produce (package names,
// keywords). Unreserved set per RFC 3986 plus JS's extra !'()*-._~ .
fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || "-_.!~*'()".contains(c) { out.push(c); }
        else { out.push_str(&format!("%{b:02X}")); }
    }
    out
}

fn num_or_null(v: Option<&Value>) -> Value {
    // JS `Number(x) || null` — 0 and NaN both fall to null.
    match v.and_then(Value::as_f64) {
        Some(n) if n != 0.0 && !n.is_nan() => json!(n),
        _ => Value::Null,
    }
}
fn num_or_zero(v: Option<&Value>) -> i64 {
    v.and_then(Value::as_f64).filter(|n| !n.is_nan()).unwrap_or(0.0) as i64
}

// ── enrichArticles: the wire displays ARTICLES, not links ────────────────────────────────────
fn enrichArticles(db: &Connection, items: Value, ctl: &mut Ctl) -> Value {
    let mut list = items.as_array().cloned().unwrap_or_default();
    let mut scrapes = 0usize;
    for it in list.iter_mut() {
        let content_len = it.get("content").and_then(Value::as_str).unwrap_or("").chars().count();
        if content_len > 200 { continue; }
        let link = it.get("link").and_then(Value::as_str).unwrap_or("").to_string();
        if scrapes >= MAX_ARTICLE_SCRAPES || !https_re().is_match(&link) { continue; }
        scrapes += 1;
        let body = fetch_or_null(db, &link, None, ctl, 24.0);
        if body.is_null() { continue; }
        let text = crate::codebase__helpers_web_research::extractArticle(&body);
        let text_len = text.as_str().unwrap_or("").chars().count();
        if text_len > content_len {
            if let Some(o) = it.as_object_mut() { o.insert("content".into(), text); }
        }
    }
    json!(list)
}

// ── buildLedger: package census × npm registry × osv.dev ─────────────────────────────────────
fn buildLedger(db: &Connection, root: &Path, ctl: &mut Ctl) -> (Value, Value) {
    let census = readDeps(root);
    let packages = census.get("packages").cloned().unwrap_or(json!([]));
    let deps = census.get("deps").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut registry_docs = Map::new();
    if deps.is_empty() {
        return (json!({ "deps": [], "packages": packages }), Value::Object(registry_docs));
    }

    let mut with_latest: Vec<Value> = Vec::new();
    for entry in &deps {
        let pair = entry.as_array().cloned().unwrap_or_default();
        let name = str_qq(pair.first());
        let info = pair.get(1).cloned().unwrap_or(json!({}));
        let body = fetch_or_null(
            db,
            &format!("https://registry.npmjs.org/{}/latest", encode_uri_component(&name)),
            None, ctl, 24.0,
        );
        let doc = json_parse(&body);
        if !doc.is_null() { registry_docs.insert(name.clone(), doc.clone()); }
        let latest = match doc.get("version") {
            Some(v) if !v.is_null() => json!(str_qq(Some(v))),
            _ => Value::Null,
        };
        let current = info.get("current").cloned().unwrap_or(Value::Null);
        let behind = if latest.is_null() { json!(0) }
            else { crate::codebase__helpers_web_research::behindScore(&current, &latest) };
        with_latest.push(json!({
            "name": name, "current": current, "dev": info.get("dev").cloned().unwrap_or(Value::Null),
            "latest": latest, "behind": behind,
        }));
    }

    // osv.dev — one batched POST for the CURRENT pinned versions. The cache key carries a hash of
    // the query body as a #fragment (never sent over the wire) so a changed dep set cannot hit a
    // stale batch; web-cache.rs strips the fragment before dialing.
    let queries: Vec<Value> = with_latest.iter().map(|d| json!({
        "package": { "name": d.get("name").cloned().unwrap_or(Value::Null), "ecosystem": "npm" },
        "version": d.get("current").cloned().unwrap_or(Value::Null),
    })).collect();
    let q_body = serde_json::to_string(&json!({ "queries": queries })).unwrap_or_else(|_| "{}".into());
    let key = str_qq(Some(&crate::codebase__helpers_web_research::hashKey(&json!(q_body))));
    let osv_body = fetch_or_null(
        db,
        &format!("https://api.osv.dev/v1/querybatch#{key}"),
        Some(&json!({ "method": "POST", "headers": { "content-type": "application/json" }, "body": q_body })),
        ctl, 24.0,
    );
    let osv = json_parse(&osv_body);
    let results = osv.get("results").and_then(Value::as_array).cloned().unwrap_or_default();

    let mut rows: Vec<Value> = with_latest.into_iter().enumerate().map(|(i, mut d)| {
        let vulns = results.get(i).and_then(|r| r.get("vulns")).and_then(Value::as_array).cloned().unwrap_or_default();
        let advisories: Vec<Value> = vulns.iter().take(6).map(|v| json!({
            "id": str_qq(v.get("id")),
            "summary": take_chars(&str_qq(v.get("summary")), 160),
        })).collect();
        if let Some(o) = d.as_object_mut() { o.insert("advisories".into(), json!(advisories)); }
        d
    }).collect();

    rows.sort_by(|a, b| {
        let av = a.get("advisories").and_then(Value::as_array).map(|x| x.len()).unwrap_or(0);
        let bv = b.get("advisories").and_then(Value::as_array).map(|x| x.len()).unwrap_or(0);
        bv.cmp(&av)
            .then_with(|| num_or_zero(b.get("behind")).cmp(&num_or_zero(a.get("behind"))))
            .then_with(|| str_qq(a.get("name")).cmp(&str_qq(b.get("name"))))
    });
    (json!({ "deps": rows, "packages": packages }), Value::Object(registry_docs))
}

// ── buildHorizon: detected stack products × endoflife.date support windows ───────────────────
fn buildHorizon(db: &Connection, root: &Path, ledger_deps: &Value, ctl: &mut Ctl) -> Value {
    let all_body = fetch_or_null(db, "https://endoflife.date/api/all.json", None, ctl, 24.0 * 7.0);
    let all = json_parse(&all_body);
    let known: std::collections::HashSet<String> = all.as_array().into_iter().flatten()
        .map(|s| str_qq(Some(s)).to_lowercase()).collect();
    if known.is_empty() { return json!({ "products": [] }); }

    let manifest = readRootManifest(root);
    let node_engine = strip_range_prefix(&str_qq(manifest.get("engines").and_then(|e| e.get("node"))));
    let mut candidates: Vec<(String, Value)> = vec![(
        "nodejs".to_string(),
        if node_engine.is_empty() { Value::Null } else { json!(node_engine) },
    )];

    // JS sorts by `Number(a.dev) - Number(b.dev)`: prod deps (dev falsy) come before dev deps.
    let mut by_dev: Vec<Value> = ledger_deps.as_array().cloned().unwrap_or_default();
    by_dev.sort_by_key(|d| if truthy(d.get("dev")) { 1 } else { 0 });
    for d in &by_dev {
        let name = str_qq(d.get("name"));
        let slug = if name.contains('/') { name.rsplit('/').next().unwrap_or("").to_lowercase() } else { name.to_lowercase() };
        if !candidates.iter().any(|(s, _)| s == &slug) {
            candidates.push((slug, d.get("current").cloned().unwrap_or(Value::Null)));
        }
    }

    let matched: Vec<(String, Value)> = candidates.into_iter().filter(|(s, _)| known.contains(s)).take(8).collect();
    let mut products: Vec<Value> = Vec::new();
    for (slug, current) in matched {
        let body = fetch_or_null(db, &format!("https://endoflife.date/api/{}.json", encode_uri_component(&slug)), None, ctl, 24.0 * 7.0);
        let cycles = json_parse(&body);
        let cycles = match cycles.as_array() { Some(a) if !a.is_empty() => a.clone(), _ => continue };

        let major = crate::codebase__helpers_web_research::parseSemver(&current);
        let major = major.as_array().and_then(|a| a.first().cloned());
        let row = major
            .as_ref()
            .filter(|m| !m.is_null())
            .and_then(|m| cycles.iter().find(|cy| str_qq(cy.get("cycle")) == str_qq(Some(m))).cloned())
            .unwrap_or_else(|| cycles[0].clone());

        let eol = row.get("eol").cloned().unwrap_or(Value::Null);
        // eol is a date string | false (never) | true (already dead) — only a string has runway.
        let runway_days = match eol.as_str().and_then(crate::codebase__helpers_web_research::js_date_parse) {
            Some(t) => json!(((t - crate::clock::now_ms()) as f64 / 86_400_000.0).round() as i64),
            None => Value::Null,
        };
        products.push(json!({
            "product": slug,
            "current": current,
            "cycle": str_qq(row.get("cycle")),
            "latest": str_qq(row.get("latest")),
            "eol": eol,
            "support": row.get("support").cloned().unwrap_or(Value::Null),
            "runway_days": runway_days,
        }));
    }
    json!({ "products": products })
}

// ── buildWire: releases.atom feeds derived from the project's own prod deps ──────────────────
fn buildWire(db: &Connection, ledger_deps: &Value, registry_docs: &Value, operator_feeds: &[String], ctl: &mut Ctl) -> Value {
    let mut urls: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut follow = |urls: &mut Vec<String>, seen: &mut std::collections::HashSet<String>, u: &str| {
        if !u.is_empty() && https_re().is_match(u) && !seen.contains(u) {
            seen.insert(u.to_string());
            urls.push(u.to_string());
        }
    };
    for u in operator_feeds { follow(&mut urls, &mut seen, u); }

    let empty = Map::new();
    let docs = registry_docs.as_object().unwrap_or(&empty);
    for d in ledger_deps.as_array().into_iter().flatten() {
        if truthy(d.get("dev")) { continue; }
        if urls.len() >= MAX_WIRE_FEEDS { break; }
        let name = str_qq(d.get("name"));
        let repo = docs.get(&name).and_then(|dc| dc.get("repository"));
        let repo_url = repo.and_then(|r| r.get("url")).cloned().or_else(|| repo.cloned()).unwrap_or(Value::Null);
        let gh = crate::codebase__helpers_web_research::githubRepo(&repo_url);
        if !gh.is_null() {
            let u = format!("https://github.com/{}/{}/releases.atom", str_qq(gh.get("owner")), str_qq(gh.get("repo")));
            follow(&mut urls, &mut seen, &u);
        }
    }

    let capped: Vec<String> = urls.into_iter().take(MAX_WIRE_FEEDS).collect();
    let (mut feeds_ok, mut feeds_failed) = (0i64, 0i64);
    let mut all: Vec<Value> = Vec::new();
    for url in &capped {
        // JS labels a github feed by `owner/repo`, anything else by its bare hostname.
        let source = match url_host(url) {
            Some(h) if h == "github.com" => {
                let p = url_path(url);
                p.split('/').filter(|s| !s.is_empty()).take(2).collect::<Vec<_>>().join("/")
            }
            Some(h) => h.strip_prefix("www.").unwrap_or(&h).to_string(),
            None => url.clone(),
        };
        let body = fetch_or_null(db, url, None, ctl, 2.0);
        if body.is_null() { feeds_failed += 1; continue; }
        let items = crate::codebase__helpers_web_research::parseFeed(&body, &json!(source));
        match items.as_array() {
            Some(a) if !a.is_empty() => { feeds_ok += 1; all.extend(a.iter().cloned()); }
            _ => feeds_failed += 1,
        }
    }
    let sorted = crate::codebase__helpers_web_research::sortDateDesc(&json!(all), &json!(MAX_ITEMS)).unwrap_or(json!([]));
    json!({
        "items": enrichArticles(db, sorted, ctl),
        "feeds": capped, "feeds_ok": feeds_ok, "feeds_failed": feeds_failed,
    })
}

// ── the tech press: curated seeds + <link rel=alternate> autodiscovery ───────────────────────
const BLOG_SEEDS: [(&str, &str); 5] = [
    ("nodejs", "https://nodejs.org/en/feed/blog.xml"),
    ("react", "https://react.dev/rss.xml"),
    ("typescript", "https://devblogs.microsoft.com/typescript/feed/"),
    ("vue", "https://blog.vuejs.org/feed.rss"),
    ("angular", "https://blog.angular.dev/feed"),
];

fn feed_link_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?i)<link[^>]+type=["']application/(?:rss|atom)\+xml["'][^>]*>"#).unwrap())
}
fn feed_link_alt_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?i)<link[^>]+rel=["']alternate["'][^>]+type=["']application/(?:rss|atom)\+xml["'][^>]*>"#).unwrap())
}
fn href_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?i)href=["']([^"']+)["']"#).unwrap())
}

fn discoverBlogFeed(db: &Connection, homepage: &str, ctl: &mut Ctl) -> Value {
    if !https_re().is_match(homepage) { return Value::Null; }
    let html = fetch_or_null(db, homepage, None, ctl, 24.0 * 7.0);
    let html = match html.as_str() { Some(s) => s.to_string(), None => return Value::Null };
    let link = feed_link_re().find(&html).map(|m| m.as_str().to_string())
        .or_else(|| feed_link_alt_re().find(&html).map(|m| m.as_str().to_string()));
    let href = match link.as_deref().and_then(|l| href_re().captures(l)) {
        Some(c) => c[1].to_string(),
        None => return Value::Null,
    };
    match url_resolve(&href, homepage) { Some(u) => json!(u), None => Value::Null }
}

fn buildTechPress(db: &Connection, ledger_deps: &Value, registry_docs: &Value, horizon_products: &Value, ctl: &mut Ctl) -> Value {
    let techs = pickTechs(ledger_deps, registry_docs, horizon_products, 8);
    let mut sources: Vec<(String, String)> = Vec::new();
    for t in techs.as_array().into_iter().flatten() {
        if sources.len() >= 6 { break; }
        let name = str_qq(t.get("name"));
        let mut feed = BLOG_SEEDS.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string());
        if feed.is_none() {
            if let Some(hp) = t.get("homepage").and_then(Value::as_str) {
                let d = discoverBlogFeed(db, hp, ctl);
                if let Some(s) = d.as_str() { feed = Some(s.to_string()); }
            }
        }
        if let Some(f) = feed {
            if !sources.iter().any(|(_, existing)| existing == &f) { sources.push((name, f)); }
        }
    }

    let mut all: Vec<Value> = Vec::new();
    let mut feeds_ok = 0i64;
    for (tech, feed) in &sources {
        let body = fetch_or_null(db, feed, None, ctl, 6.0);
        if body.is_null() { continue; }
        let items = crate::codebase__helpers_web_research::parseFeed(&body, &json!(tech));
        if let Some(a) = items.as_array() {
            if !a.is_empty() { feeds_ok += 1; all.extend(a.iter().take(4).cloned()); }
        }
    }
    let sorted = crate::codebase__helpers_web_research::sortDateDesc(&json!(all), &json!(12)).unwrap_or(json!([]));
    let blogs: Vec<Value> = sources.iter().map(|(t, f)| json!({ "tech": t, "feed": f })).collect();
    json!({ "items": enrichArticles(db, sorted, ctl), "blogs": blogs, "feeds_ok": feeds_ok })
}

// ── the observatory numbers: the WORLD's stats on this stack ─────────────────────────────────
fn buildTechStats(db: &Connection, ledger_deps: &Value, registry_docs: &Value, horizon_products: &Value, ctl: &mut Ctl) -> Value {
    let mut eol_by_product: Map<String, Value> = Map::new();
    for p in horizon_products.as_array().into_iter().flatten() {
        eol_by_product.insert(str_qq(p.get("product")), p.clone());
    }
    let empty = Map::new();
    let docs = registry_docs.as_object().unwrap_or(&empty);

    let mut rows: Vec<Value> = Vec::new();
    for d in ledger_deps.as_array().into_iter().flatten().filter(|d| !truthy(d.get("dev"))).take(8) {
        let name = str_qq(d.get("name"));
        let mut row = json!({
            "name": name, "weekly": Value::Null, "trend": [], "stars": Value::Null, "forks": Value::Null,
            "open_issues": Value::Null, "pushed_at": Value::Null, "releases_90d": Value::Null, "runway_days": Value::Null,
        });

        let point = fetch_or_null(db, &format!("https://api.npmjs.org/downloads/point/last-week/{name}"), None, ctl, 24.0);
        row["weekly"] = num_or_null(json_parse(&point).get("downloads"));

        let range = fetch_or_null(db, &format!("https://api.npmjs.org/downloads/range/last-month/{name}"), None, ctl, 24.0);
        let days = json_parse(&range).get("downloads").and_then(Value::as_array).cloned().unwrap_or_default();
        // last-month dailies → four weekly sums (oldest → newest)
        let tail: Vec<&Value> = days.iter().rev().take(28).rev().collect();
        let mut weeks = [0i64; 4];
        for (i, x) in tail.iter().enumerate() { weeks[std::cmp::min(3, i / 7)] += num_or_zero(x.get("downloads")); }
        row["trend"] = if tail.len() >= 14 { json!(weeks) } else { json!([]) };

        let repo = docs.get(&name).and_then(|dc| dc.get("repository"));
        let repo_url = repo.and_then(|r| r.get("url")).cloned().or_else(|| repo.cloned()).unwrap_or(Value::Null);
        let gh = crate::codebase__helpers_web_research::githubRepo(&repo_url);
        if !gh.is_null() {
            let (owner, repo_name) = (str_qq(gh.get("owner")), str_qq(gh.get("repo")));
            let repo_body = fetch_or_null(db, &format!("https://api.github.com/repos/{owner}/{repo_name}"), None, ctl, 24.0);
            let doc = json_parse(&repo_body);
            if !doc.is_null() {
                row["stars"] = json!(num_or_zero(doc.get("stargazers_count")));
                row["forks"] = json!(num_or_zero(doc.get("forks_count")));
                row["open_issues"] = json!(num_or_zero(doc.get("open_issues_count")));
                let pushed = take_chars(&str_qq(doc.get("pushed_at")), 10);
                row["pushed_at"] = if pushed.is_empty() { Value::Null } else { json!(pushed) };
            }
            // release cadence from the atom feed the wire already cached — refresh:false always,
            // so this never re-dials a feed buildWire just fetched.
            let atom = crate::codebase__helpers_web_cache::cachedFetch(
                db, &json!(format!("https://github.com/{owner}/{repo_name}/releases.atom")), None,
                Some(&mut ctl.stats), false, 2.0, &Value::Null, &skill_v(),
            ).unwrap_or(Value::Null);
            if !atom.is_null() {
                let cutoff = crate::clock::now_ms() - 90 * 86_400_000;
                let items = crate::codebase__helpers_web_research::parseFeed(&atom, &json!(name));
                let n = items.as_array().into_iter().flatten().filter(|it| {
                    it.get("date").and_then(Value::as_str)
                        .and_then(crate::codebase__helpers_web_research::js_date_parse)
                        .unwrap_or(0) > cutoff
                }).count();
                row["releases_90d"] = json!(n);
            }
        }

        let slug = if name.contains('/') { name.rsplit('/').next().unwrap_or("").to_lowercase() } else { name.to_lowercase() };
        row["runway_days"] = eol_by_product.get(&slug).and_then(|p| p.get("runway_days")).cloned().unwrap_or(Value::Null);
        if !row["weekly"].is_null() || !row["stars"].is_null() { rows.push(row); }
    }
    json!({ "techs": rows })
}

// ── the product on paper ─────────────────────────────────────────────────────────────────────
fn buildProductOnPaper(db: &Connection, dossier: &Value, ctl: &mut Ctl) -> Value {
    // JS try/catch: an absent atlas leaves the floor empty rather than failing the department.
    let inv = crate::codebase_feature_inventory::featureInventory(&json!({ "origin": "atlas" }), db)
        .unwrap_or_else(|_| json!({ "ui": [], "nonui": [], "gaps": [], "sources": { "entry_points": 0, "stories": 0 } }));

    let mut self_v = Value::Null;
    if let Some(site) = dossier.get("site").and_then(Value::as_str) {
        let body = fetch_or_null(db, site, None, ctl, 24.0);
        if !body.is_null() {
            if let Ok(d) = crate::codebase__helpers_web_research::extractDossier(&body) {
                let mut o = d.as_object().cloned().unwrap_or_default();
                // JS `{url, ...extractDossier(body)}` — url first, so a dossier `url` key would win.
                let mut merged = Map::new();
                merged.insert("url".into(), json!(site));
                merged.append(&mut o);
                self_v = Value::Object(merged);
            }
        }
    }

    let label_of = |f: &Value| {
        let l = f.get("label").cloned().filter(|v| !v.is_null()).or_else(|| f.get("path").cloned());
        take_chars(&str_qq(l.as_ref()), 90)
    };
    let features: Vec<Value> = inv.get("ui").and_then(Value::as_array).into_iter().flatten().take(14)
        .map(|f| json!({ "label": label_of(f), "stories": f.get("stories").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0) }))
        .collect();
    let nonui: Vec<Value> = inv.get("nonui").and_then(Value::as_array).into_iter().flatten().take(10)
        .map(|f| json!(label_of(f))).collect();

    json!({
        "features": features,
        "nonui": nonui,
        "gaps": inv.get("gaps").and_then(Value::as_array).map(|a| a.len()).unwrap_or(0),
        "entry_points": inv.get("sources").and_then(|s| s.get("entry_points")).cloned().unwrap_or(json!(0)),
        "stories": inv.get("sources").and_then(|s| s.get("stories")).cloned().unwrap_or(json!(0)),
        "self": self_v,
        "dossier": if dossier.is_null() { Value::Null } else { json!({
            "one_liner": dossier.get("one_liner").cloned().unwrap_or(Value::Null),
            "audience": dossier.get("audience").cloned().unwrap_or(Value::Null),
            "problem": dossier.get("problem").cloned().unwrap_or(Value::Null),
        }) },
    })
}

// ── the rival pages ──────────────────────────────────────────────────────────────────────────
fn buildRivals(db: &Connection, rivals: &Value, ctl: &mut Ctl) -> Value {
    let list: Vec<(String, String)> = rivals.as_array().into_iter().flatten()
        .map(|r| (take_chars(&str_qq(r.get("name")), 60), str_qq(r.get("url"))))
        .filter(|(n, u)| !n.is_empty() && https_re().is_match(u))
        .take(MAX_RIVALS)
        .collect();

    let mut out: Vec<Value> = Vec::new();
    for (name, url) in list {
        let body = fetch_or_null(db, &url, None, ctl, 24.0);
        if body.is_null() {
            out.push(json!({ "name": name, "url": url, "ok": false, "title": "", "description": "", "claims": [], "words": 0 }));
            continue;
        }
        let mut o = Map::new();
        o.insert("name".into(), json!(name));
        o.insert("url".into(), json!(url));
        o.insert("ok".into(), json!(true));
        if let Ok(d) = crate::codebase__helpers_web_research::extractDossier(&body) {
            if let Some(dm) = d.as_object() { for (k, v) in dm { o.insert(k.clone(), v.clone()); } }
        }
        out.push(Value::Object(o));
    }
    json!({ "rivals": out })
}

// ── dispatches from the field: HN Algolia on the market's vocabulary ─────────────────────────
fn word_split(s: &str) -> Vec<String> {
    // JS `String.split(/\W+/)` — \w is [A-Za-z0-9_].
    s.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map(str::to_string).collect()
}

fn buildDispatches(db: &Connection, root: &Path, operator_feeds: &[String], ctl: &mut Ctl, dossier: &Value) -> Value {
    let mut kws: Vec<String> = Vec::new();
    let mut push = |kws: &mut Vec<String>, w: &str| {
        let k = w.to_lowercase();
        let k = k.trim().to_string();
        if k.chars().count() > 2 && !kws.contains(&k) { kws.push(k); }
    };
    for k in dossier.get("keywords").and_then(Value::as_array).into_iter().flatten() {
        push(&mut kws, &str_qq(Some(k)));
    }
    if kws.is_empty() {
        let manifest = readRootManifest(root);
        let name = str_qq(manifest.get("name"));
        push(&mut kws, name.rsplit('/').next().unwrap_or(""));
        for k in manifest.get("keywords").and_then(Value::as_array).into_iter().flatten() {
            push(&mut kws, &str_qq(Some(k)));
        }
        if kws.len() < 3 {
            for w in word_split(&str_qq(manifest.get("description"))) {
                if w.chars().count() > 4 { push(&mut kws, &w); }
            }
        }
    }

    let queries: Vec<String> = kws.into_iter().take(3).collect();
    let mut all: Vec<Value> = Vec::new();
    for q in &queries {
        let url = format!("https://hn.algolia.com/api/v1/search?query={}&tags=story&hitsPerPage=10", encode_uri_component(q));
        let body = fetch_or_null(db, &url, None, ctl, 6.0);
        if body.is_null() { continue; }
        for h in json_parse(&body).get("hits").and_then(Value::as_array).into_iter().flatten() {
            let title = take_chars(&str_qq(h.get("title")), 200);
            if title.is_empty() { continue; }
            let link = match h.get("url") {
                Some(u) if !u.is_null() => str_qq(Some(u)),
                _ => format!("https://news.ycombinator.com/item?id={}", str_qq(h.get("objectID"))),
            };
            all.push(json!({
                "title": title, "link": take_chars(&link, 500), "date": str_qq(h.get("created_at")),
                "source": format!("hn:{q}"), "points": num_or_zero(h.get("points")), "kind": "post",
            }));
        }
    }

    for url in operator_feeds.iter().take(6) {
        let host = url_host(url).map(|h| h.strip_prefix("www.").unwrap_or(&h).to_string()).unwrap_or_else(|| url.clone());
        let body = fetch_or_null(db, url, None, ctl, 2.0);
        if !body.is_null() {
            let items = crate::codebase__helpers_web_research::parseFeed(&body, &json!(host));
            all.extend(items.as_array().into_iter().flatten().cloned());
        }
    }

    let sorted = crate::codebase__helpers_web_research::sortDateDesc(&json!(all), &json!(MAX_ITEMS)).unwrap_or(json!([]));
    json!({ "items": enrichArticles(db, sorted, ctl), "keywords": queries })
}

// ── the orchestrator ─────────────────────────────────────────────────────────────────────────
/// `export default async function magazineObserve(input, context)`.
pub fn magazineObserve(input: &Value, db: &Connection, workspace_root: Option<&str>) -> Result<Value, String> {
    let faculty = str_qq(input.get("faculty")).trim().to_string();
    if !FACULTIES.contains(&faculty.as_str()) {
        return Err(format!(
            "magazine-observe: faculty must be one of {} (got {})",
            FACULTIES.join(", "), faculty
        ));
    }
    crate::codebase__helpers_web_cache::ensureWebCache(db);
    crate::codebase__helpers_magazine_db::ensureMagazineIssues(db);

    let mut root = str_qq(input.get("root"));
    if root.trim().is_empty() { root = workspace_root.unwrap_or("").to_string(); }
    if root.trim().is_empty() {
        root = str_qq(Some(&crate::engine_workspace_root::getWorkspaceRoot(db)));
    }
    let root = root.trim().to_string();
    let root_path = PathBuf::from(&root);

    let mut ctl = Ctl::new(input.get("refresh") == Some(&json!(true)));
    let operator_feeds: Vec<String> = input.get("feeds").and_then(Value::as_array).into_iter().flatten()
        .map(|u| str_qq(Some(u)))
        .filter(|u| https_re().is_match(u))
        .take(MAX_WIRE_FEEDS)
        .collect();

    // the desk parses FIRST — the dossier (intention as data) steers the herald
    let desk_items: Vec<Value> = input.get("desk").and_then(Value::as_array).into_iter().flatten()
        .map(|d| json!({
            "label": take_chars(&str_qq(d.get("label")), 80),
            "body": take_chars(&str_qq(d.get("body")), 1500),
        }))
        .filter(|d| !str_qq(d.get("label")).is_empty() && !str_qq(d.get("body")).is_empty())
        .take(8)
        .collect();
    let dossier = parseDossier(&json!(desk_items));

    let mut departments = Map::new();
    if faculty == "arch" {
        // JS `safe()` wraps each department so one dead API never kills the issue. Rust's builders
        // return values rather than throwing, so the only "throw" left is the explicit
        // missing-root guard below — mirrored as the same `{error}` payload.
        let mut ledger_deps = json!([]);
        let mut registry_docs = json!({});
        if root.is_empty() {
            departments.insert("ledger".into(), json!({ "error": "magazine-observe: workspace root not found: (none — onboard a workspace first)" }));
        } else {
            let (dept, docs) = buildLedger(db, &root_path, &mut ctl);
            ledger_deps = dept.get("deps").cloned().unwrap_or(json!([]));
            registry_docs = docs;
            departments.insert("ledger".into(), dept);
        }
        let horizon = buildHorizon(db, &root_path, &ledger_deps, &mut ctl);
        let horizon_products = horizon.get("products").cloned().unwrap_or(json!([]));
        departments.insert("horizon".into(), horizon);
        departments.insert("wire".into(), buildWire(db, &ledger_deps, &registry_docs, &operator_feeds, &mut ctl));
        departments.insert("press".into(), buildTechPress(db, &ledger_deps, &registry_docs, &horizon_products, &mut ctl));
        departments.insert("stats".into(), buildTechStats(db, &ledger_deps, &registry_docs, &horizon_products, &mut ctl));
        departments.insert("notices".into(), buildNotices(db, &root_path));
    } else {
        departments.insert("product".into(), buildProductOnPaper(db, &dossier, &mut ctl));
        // rivals = the operator's list UNION the dossier's declared rivals
        let mut rival_list: Vec<Value> = input.get("rivals").and_then(Value::as_array).cloned().unwrap_or_default();
        for r in dossier.get("rivals").and_then(Value::as_array).into_iter().flatten() {
            if !rival_list.iter().any(|x| x.get("url") == r.get("url")) { rival_list.push(r.clone()); }
        }
        departments.insert("rivals".into(), buildRivals(db, &json!(rival_list), &mut ctl));
        departments.insert("dispatches".into(), buildDispatches(db, &root_path, &operator_feeds, &mut ctl, &dossier));
    }

    if !desk_items.is_empty() {
        departments.insert("desk".into(), json!({ "items": desk_items }));
    }

    let departments = Value::Object(departments);
    let hash = crate::codebase__helpers_web_research::hashKey(&json!(serde_json::to_string(&departments).unwrap_or_default()));
    let last = crate::codebase__helpers_magazine_db::latestIssue(db, &json!(faculty));
    let prev = if last.is_null() { Value::Null } else { last.get("observation").cloned().unwrap_or(Value::Null) };
    let delta = computeDelta(&faculty, &prev, &departments);

    Ok(json!({
        "ok": true,
        "faculty": faculty,
        "departments": departments,
        "hash": hash,
        "delta": delta,
        "last_issue": if last.is_null() { Value::Null } else { json!({
            "issue_no": last.get("issue_no").cloned().unwrap_or(Value::Null),
            "observation_hash": last.get("observation_hash").cloned().unwrap_or(Value::Null),
            "editorial": last.get("editorial").cloned().unwrap_or(Value::Null),
            "pressed_at": last.get("pressed_at").cloned().unwrap_or(Value::Null),
        }) },
        "issues": crate::codebase__helpers_magazine_db::listIssues(db, &json!(faculty), &json!(12)),
        "cached_hits": ctl.stats.get("cached_hits").cloned().unwrap_or(json!(0)),
        "fetched": ctl.stats.get("fetched").cloned().unwrap_or(json!(0)),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-magazine-observe-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ── readDeps ─────────────────────────────────────────────────────────────
    #[test]
    fn read_deps_merges_root_and_workspace_members_root_wins_on_conflict() {
        let dir = temp_dir("read-deps");
        fs::write(dir.join("package.json"), r#"{"dependencies":{"react":"^18.2.0"},"devDependencies":{"vitest":"~1.0.0"}}"#).unwrap();
        let pkg_a = dir.join("pkg-a");
        fs::create_dir_all(&pkg_a).unwrap();
        fs::write(pkg_a.join("package.json"), r#"{"dependencies":{"react":"^17.0.0","lodash":"4.17.21"}}"#).unwrap();
        // hidden + node_modules dirs are skipped
        let hidden = dir.join(".git");
        fs::create_dir_all(&hidden).unwrap();
        fs::write(hidden.join("package.json"), r#"{"dependencies":{"ignored":"1.0.0"}}"#).unwrap();
        let nm = dir.join("node_modules");
        fs::create_dir_all(&nm).unwrap();
        fs::write(nm.join("package.json"), r#"{"dependencies":{"also-ignored":"1.0.0"}}"#).unwrap();

        let r = readDeps(&dir);
        assert_eq!(r["packages"], json!(["(root)", "pkg-a"]));
        let deps = r["deps"].as_array().unwrap();
        let map: std::collections::HashMap<String, Value> = deps.iter().map(|e| {
            let a = e.as_array().unwrap();
            (a[0].as_str().unwrap().to_string(), a[1].clone())
        }).collect();
        assert_eq!(map["react"], json!({ "current": "18.2.0", "dev": false })); // root wins over pkg-a's 17.x
        assert_eq!(map["vitest"], json!({ "current": "1.0.0", "dev": true }));
        assert_eq!(map["lodash"], json!({ "current": "4.17.21", "dev": false }));
        assert!(!map.contains_key("ignored"));
        assert!(!map.contains_key("also-ignored"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_deps_missing_root_manifest_yields_empty_census() {
        let dir = temp_dir("read-deps-empty");
        let r = readDeps(&dir);
        assert_eq!(r["packages"], json!([]));
        assert_eq!(r["deps"], json!([]));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_root_manifest_parses_or_degrades_to_empty_object() {
        let dir = temp_dir("root-manifest");
        fs::write(dir.join("package.json"), r#"{"name":"demo","engines":{"node":">=20"}}"#).unwrap();
        assert_eq!(readRootManifest(&dir)["name"], json!("demo"));

        let dir2 = temp_dir("root-manifest-missing");
        assert_eq!(readRootManifest(&dir2), json!({}));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&dir2);
    }

    // ── pickTechs ────────────────────────────────────────────────────────────
    #[test]
    fn pick_techs_horizon_first_then_prod_deps_scoped_name_and_homepage() {
        let ledger = json!([
            { "name": "react", "dev": false },
            { "name": "@scope/pkg", "dev": false },
            { "name": "jest", "dev": true },
        ]);
        let registry_docs = json!({ "react": { "homepage": "https://react.dev" } });
        let horizon = json!([{ "product": "nodejs" }]);
        let r = pickTechs(&ledger, &registry_docs, &horizon, 8);
        assert_eq!(r, json!([
            { "name": "nodejs", "homepage": Value::Null },
            { "name": "react", "homepage": "https://react.dev" },
            { "name": "pkg", "homepage": Value::Null },
        ]));
    }

    #[test]
    fn pick_techs_respects_cap_and_dedupes() {
        let ledger = json!([
            { "name": "nodejs", "dev": false },
            { "name": "react", "dev": false },
        ]);
        let r = pickTechs(&ledger, &json!({}), &json!([{ "product": "nodejs" }]), 1);
        assert_eq!(r, json!([{ "name": "nodejs", "homepage": Value::Null }]));
    }

    // ── parseDossier ─────────────────────────────────────────────────────────
    #[test]
    fn parse_dossier_extracts_fields_rivals_and_filters_short_keywords() {
        let items = json!([{
            "label": "Product Dossier",
            "body": "one-liner: We do X\nkeywords: a, bb, c,c\nrivals: Foo — https://foo.com | Bar — https://bar.com\nsite: https://us.example.com\n",
        }]);
        let d = parseDossier(&items);
        assert_eq!(d["one_liner"], json!("We do X"));
        assert_eq!(d["audience"], json!(""));
        assert_eq!(d["site"], json!("https://us.example.com"));
        assert_eq!(d["keywords"], json!(["bb"])); // 'a' and 'c' filtered (length <= 1)
        assert_eq!(d["rivals"], json!([
            { "name": "Foo", "url": "https://foo.com" },
            { "name": "Bar", "url": "https://bar.com" },
        ]));
    }

    #[test]
    fn parse_dossier_uses_latest_matching_doc_and_nonhttps_site_is_dropped() {
        let items = json!([
            { "label": "Product Dossier", "body": "one-liner: old" },
            { "label": "Product Dossier v2", "body": "one-liner: new\nsite: ftp://not-https.example" },
        ]);
        let d = parseDossier(&items);
        assert_eq!(d["one_liner"], json!("new")); // reversed search finds the LAST matching doc
        assert_eq!(d["site"], Value::Null);
    }

    #[test]
    fn parse_dossier_no_matching_doc_or_empty_fields_returns_null() {
        assert_eq!(parseDossier(&json!([{ "label": "Random Note", "body": "one-liner: x" }])), Value::Null);
        assert_eq!(parseDossier(&json!([{ "label": "Product Dossier", "body": "problem: only a problem line" }])), Value::Null);
        assert_eq!(parseDossier(&json!([])), Value::Null);
    }

    // ── buildNotices ─────────────────────────────────────────────────────────
    const SCHEMA: &str = "
        CREATE TABLE code_nodes (
          id TEXT PRIMARY KEY, origin TEXT, path TEXT, type TEXT, name TEXT, superseded_by TEXT
        );
        CREATE TABLE code_edges (
          id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, from_node TEXT, to_node TEXT, type TEXT
        , kind TEXT);
    ";
    fn db_with_schema() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        c
    }

    #[test]
    fn build_notices_returns_null_when_no_atlas_files() {
        let c = db_with_schema();
        let dir = temp_dir("notices-empty");
        assert_eq!(buildNotices(&c, &dir), Value::Null);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_notices_returns_null_when_table_missing_db_error_path() {
        let c = Connection::open_in_memory().unwrap(); // no code_nodes table at all
        let dir = temp_dir("notices-no-table");
        assert_eq!(buildNotices(&c, &dir), Value::Null);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_notices_giants_ranked_by_size_and_hubs_by_dependent_count() {
        let c = db_with_schema();
        let dir = temp_dir("notices-full");
        fs::write(dir.join("small.js"), "x".repeat(40)).unwrap(); // 10 tokens
        fs::write(dir.join("big.js"), "x".repeat(400)).unwrap(); // 100 tokens
        c.execute_batch(
            "INSERT INTO code_nodes (id, origin, path, type, name, superseded_by) VALUES \
             ('n1','atlas','small.js','file',NULL,NULL), \
             ('n2','atlas','big.js','file',NULL,NULL), \
             ('n3','atlas','gone.js','file',NULL,NULL), \
             ('old','atlas','superseded.js','file',NULL,'n1'), \
             ('other','blueprint:x@1','other.js','file',NULL,NULL);",
        ).unwrap();
        c.execute_batch(
            "INSERT INTO code_edges (origin, from_node, to_node, type) VALUES \
             ('atlas','n1','n2','imports'), \
             ('atlas','n3','n2','imports'), \
             ('atlas','n1','n3','imports'), \
             ('atlas','n1','n2','calls');", // wrong edge type — excluded from hubs + edges count
        ).unwrap();

        let r = buildNotices(&c, &dir);
        assert_eq!(r["files"], json!(3)); // small/big/gone — not the superseded or blueprint rows
        assert_eq!(r["edges"], json!(3)); // 3 'imports' edges, 'calls' excluded
        let giants = r["giants"].as_array().unwrap();
        assert_eq!(giants[0]["path"], json!("big.js"));
        assert_eq!(giants[0]["tokens"], json!(100));
        assert_eq!(giants[1]["path"], json!("small.js"));
        // gone.js has no file on disk — silently skipped (JS try/catch), never appears in giants
        assert!(giants.iter().all(|g| g["path"] != json!("gone.js")));
        let hubs = r["hubs"].as_array().unwrap();
        assert_eq!(hubs[0]["path"], json!("big.js")); // 2 dependents (n1,n3 → n2)
        assert_eq!(hubs[0]["dependents"], json!(2));

        let _ = fs::remove_dir_all(&dir);
    }

    // ── computeDelta ─────────────────────────────────────────────────────────
    #[test]
    fn compute_delta_no_prev_issue_yields_empty() {
        assert_eq!(computeDelta("arch", &Value::Null, &json!({})), json!([]));
    }

    #[test]
    fn compute_delta_arch_new_advisory_major_behind_wire_press_eol() {
        let prev = json!({
            "ledger": { "deps": [{ "name": "react", "behind": 10, "advisories": [] }] },
            "wire": { "items": [{ "link": "https://a" }] },
            "press": { "items": [{ "link": "https://p1" }] },
            "horizon": { "products": [{ "product": "nodejs", "eol": "2026-01-01" }] },
        });
        let cur = json!({
            "ledger": { "deps": [{ "name": "react", "current": "17.0.0", "latest": "18.0.0", "behind": 50, "advisories": [{ "id": "GHSA-1", "summary": "x" }] }] },
            "wire": { "items": [{ "link": "https://a" }, { "link": "https://b" }] },
            "press": { "items": [{ "link": "https://p1" }, { "link": "https://p2" }, { "link": "https://p3" }] },
            "horizon": { "products": [{ "product": "nodejs", "eol": "2027-01-01" }] },
        });
        let out = computeDelta("arch", &prev, &cur);
        let out: Vec<String> = out.as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert!(out.contains(&"new advisory GHSA-1 on react".to_string()));
        assert!(out.contains(&"react fell a major behind (17.0.0 → 18.0.0)".to_string()));
        assert!(out.contains(&"1 new story on the wire".to_string()));
        assert!(out.contains(&"2 new posts in the tech press".to_string()));
        assert!(out.contains(&"nodejs: EOL moved to 2027-01-01".to_string()));
    }

    #[test]
    fn compute_delta_product_new_rival_and_changed_page_and_dispatches_caps_at_eight() {
        let prev = json!({
            "rivals": { "rivals": [{ "name": "Acme", "title": "old", "description": "d", "claims": [] }] },
            "dispatches": { "items": [] },
        });
        let cur = json!({
            "rivals": { "rivals": [
                { "name": "Acme", "ok": true, "title": "new", "description": "d", "claims": [] },
                { "name": "Beta", "ok": true, "title": "t", "description": "d2", "claims": [] },
            ] },
            "dispatches": { "items": [{ "link": "https://x1" }, { "link": "https://x2" }] },
        });
        let out = computeDelta("product", &prev, &cur);
        let out: Vec<String> = out.as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert!(out.contains(&"new rival tracked: Beta".to_string()));
        assert!(out.contains(&"Acme changed their page".to_string()));
        assert!(out.contains(&"2 new field dispatches".to_string()));
    }

    #[test]
    fn compute_delta_truncates_to_eight_entries() {
        let mut prev_deps = Vec::new();
        let mut cur_deps = Vec::new();
        for i in 0..12 {
            prev_deps.push(json!({ "name": format!("pkg{i}"), "behind": 0, "advisories": [] }));
            cur_deps.push(json!({ "name": format!("pkg{i}"), "current": "1.0.0", "latest": "2.0.0", "behind": 0, "advisories": [{ "id": format!("A{i}"), "summary": "" }] }));
        }
        let prev = json!({ "ledger": { "deps": prev_deps } });
        let cur = json!({ "ledger": { "deps": cur_deps } });
        let out = computeDelta("arch", &prev, &cur);
        assert_eq!(out.as_array().unwrap().len(), 8);
    }
}
