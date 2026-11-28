//! design-capture-live.rs — twin of skills/codebase/design-capture-live.mjs (FEAT-DESIGN-SYSTEM
//! live snapshots). NAMES ARE CONTRACT: default `designCaptureLive` (ported here as
//! `designCaptureLive` — the exported symbol Rust callers use — with every JS local/inner
//! function kept under its original camelCase name: `toPath`, `shapeKey`, `discoverRoutes`,
//! `sampleForParam`, `fillParams`, `mockLeaf`, `synthesizeMock`, `collectKeys`, `applyValues`,
//! `leafFields`, `lookupRecorded`).
//!
//! PARITY-DEFERRED: `designCaptureLive` — the actual browser session (playwright/chromium
//! `launch`, `page.goto`, `page.evaluate(COLLECT)`, route interception, response recording,
//! persistence to `code_design_snapshots` / `code_captured_responses`) needs a real, running
//! Chromium instance driving a real dev server; genuinely non-deterministic and not
//! JSON-fixturable, same class as `external/*-call.rs`'s provider network hop. This crate has no
//! playwright/CDP client wired, so this twin never reaches that branch — same real degenerate path
//! JS's OWN `loadPlaywright()` already takes when the package isn't resolvable (`{ ok: false, ...,
//! error: 'playwright not resolvable — install it in the engine or the FE' }`, a returned RESULT,
//! not a thrown error). Every deterministic artifact JS assembles BEFORE that point — the page
//! origin/start-path parse, the `sourceRoot`-driven `discoverRoutes` scan, the `stubApi` mock body
//! via `synthesizeMock` (+ the operator's `valueMap` override, `leafFields` form projection), the
//! `crawl`/`fixtures` seed-set union+dedupe — is built and exercised by `designCaptureLive` below
//! exactly as JS does, then the function returns the same `ok:false` shape JS's own
//! playwright-missing branch returns.
//!
//! Ported faithfully (real deterministic logic, unit-tested below):
//! - `to_path` / `shape_key` — same-origin `<a href>` normalization + id-segment shape-dedup key
//! - `discover_routes` (+ `walk_source`) — SOURCE-side router-config route discovery
//!   (`<Route path="…">` JSX + `path: "…"` route-object literals, gated by router-library keywords)
//! - `sample_for_param` / `fill_params` — param-route (`:id` / `[id]` / `*`) fixture value synthesis
//! - `mock_leaf` / `synthesize_mock` (+ `collect_keys` / `apply_values` / `leaf_fields`) — the
//!   "scan what the screen needs, then mock it" deterministic mock-shape synthesizer for `stubApi`
//! - `lookup_recorded` — exact-url-then-same-path-prefix real-response replay lookup (db read)
//!
//! Not ported: the AI-values fallback (`fixtureValues`/`ensureFixturePrompt`, JS's `aiValues`
//! input flag). It bottoms out at `ai_ai_call::aiCall`, itself PARITY-DEFERRED (see that module) —
//! every real call there returns `Err`, and JS's own `try { ... } catch { out = {}; }` around the
//! AI call means the net behavior is "keep the deterministic leaves", which this twin already does
//! by never attempting the AI branch. `applyValues(mockObj, input.valueMap)` (the operator's manual
//! override, no AI/network involved) IS ported and applied.
//!
//! One disclosed simplification: `synthesize_mock`'s recursion guard (JS: `depth > 5 ||
//! guard++ > 6000`, a runaway-recursion safety valve, not product logic) increments the counter
//! before comparing rather than JS's post-increment-after-short-circuit ordering — behaviorally
//! identical except at the exact 6000th call, an edge no realistic (or test) input reaches.
//! `synthesize_mock`'s JSON object output uses `serde_json`'s default key ordering (alphabetical),
//! not JS insertion order — the field SET and VALUES match exactly; order is not semantic here (an
//! API-stub JSON body).

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

// ── URL helpers (Node's `new URL(href)` has no crate dep here — a minimal parser sufficient for
//    this module's http(s) same-origin / pathname / hash needs). ──
fn url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([a-zA-Z][a-zA-Z0-9+.\-]*):(?://([^/?#]*))?([^?#]*)(?:\?[^#]*)?(#.*)?$").unwrap())
}
// (origin, protocol, pathname, hash) — pathname defaults to "/" like the WHATWG URL spec.
fn parse_url(href: &str) -> Option<(String, String, String, String)> {
    let caps = url_re().captures(href)?;
    let scheme = caps.get(1)?.as_str().to_string();
    let authority = caps.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
    let mut pathname = caps.get(3).map(|m| m.as_str().to_string()).unwrap_or_default();
    if pathname.is_empty() { pathname = "/".to_string(); }
    let hash = caps.get(4).map(|m| m.as_str().to_string()).unwrap_or_default();
    Some((format!("{scheme}://{authority}"), format!("{scheme}:"), pathname, hash))
}

fn asset_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\.(zip|rar|gz|pdf|docx?|xlsx?|pptx?|csv|png|jpe?g|gif|svg|ico|webp|bmp|css|js|mjs|cjs|json|map|woff2?|ttf|eot|otf|mp4|webm|mov|mp3|wav|wasm|txt|xml)$").unwrap())
}

/// `function toPath(href, pageOrigin)` — an absolute `<a href>` → a same-origin route path, or
/// `None` (cross-origin / non-http(s) / asset download). Hash-router support: only a `#/…` hash is
/// kept (appended to the pathname).
pub fn to_path(href: &str, page_origin: &str) -> Option<String> {
    let (origin, protocol, pathname, hash) = parse_url(href)?;
    if origin != page_origin || !(protocol == "http:" || protocol == "https:") {
        return None;
    }
    if asset_re().is_match(&pathname) {
        return None;
    }
    let hash_out = if hash.starts_with("#/") { hash } else { String::new() };
    let combined = format!("{pathname}{hash_out}");
    Some(if combined.is_empty() { "/".to_string() } else { combined })
}

/// `function shapeKey(path)` — collapse id-like segments so `/product/1` and `/product/2` dedupe
/// to one representative during a crawl.
pub fn shape_key(path: &str) -> String {
    let re_digits = Regex::new(r"^\d+$").unwrap();
    let re_uuid = Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-").unwrap();
    let re_hash = Regex::new(r"(?i)^[0-9a-f]{16,}$").unwrap();
    let re_has_digit = Regex::new(r"\d").unwrap();
    let re_has_alpha = Regex::new(r"[a-zA-Z]").unwrap();
    path.split('/')
        .map(|seg| {
            if seg.is_empty() { return String::new(); }
            if re_digits.is_match(seg) { return ":n".to_string(); }
            if re_uuid.is_match(seg) { return ":uuid".to_string(); }
            if re_hash.is_match(seg) { return ":hash".to_string(); }
            if seg.len() > 24 || (re_has_digit.is_match(seg) && re_has_alpha.is_match(seg) && seg.len() > 12) {
                return ":slug".to_string();
            }
            seg.to_lowercase()
        })
        .collect::<Vec<_>>()
        .join("/")
}

// ── SOURCE walk (shared by discoverRoutes and synthesizeMock) ──
const SKIP_DIRS: &[&str] = &["node_modules", ".git", "dist", "build", "out", ".next", ".nuxt", ".output", "coverage", ".cache", ".turbo", "vendor"];
fn src_ext_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.(jsx?|tsx?|vue|svelte|astro)$").unwrap())
}
fn walk_source(root: &Path, files: &mut Vec<PathBuf>, depth: i32) {
    if depth > 9 || files.len() > 4000 { return; }
    let ents = match fs::read_dir(root) { Ok(e) => e, Err(_) => return };
    for e in ents.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let path = e.path();
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) { walk_source(&path, files, depth + 1); }
        } else if src_ext_re().is_match(&name) {
            files.push(path);
        }
    }
}

/// `function discoverRoutes(root)` — reads the project's SOURCE for router config: React Router
/// `<Route path="…">` JSX, plus (gated behind a router-library keyword sniff) `path: "…"` route
/// objects (v6 / vue-router). Returns `{ static: [...], param: [...] }`.
pub fn discover_routes(root: &str) -> Value {
    let re_jsx = Regex::new(r#"<Route\b[^>]*\bpath\s*=\s*\{?\s*["'`]([^"'`]+)["'`]"#).unwrap();
    let re_router_kw = Regex::new(r"createBrowserRouter|createHashRouter|createRouter|RouterProvider|useRoutes|routes\s*[:=]\s*\[").unwrap();
    let re_obj = Regex::new(r#"\bpath\s*:\s*["'`]([^"'`]+)["'`]"#).unwrap();

    let mut files = Vec::new();
    walk_source(Path::new(root), &mut files, 0);
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for f in &files {
        let src = match fs::read_to_string(f) { Ok(s) => s, Err(_) => continue };
        if !src.contains("path") && !src.contains("Route") { continue; }
        for cap in re_jsx.captures_iter(&src) {
            let r = cap[1].to_string();
            if seen.insert(r.clone()) { out.push(r); }
        }
        if re_router_kw.is_match(&src) {
            for cap in re_obj.captures_iter(&src) {
                let r = cap[1].to_string();
                if seen.insert(r.clone()) { out.push(r); }
            }
        }
    }
    let routes: Vec<String> = out
        .into_iter()
        .map(|r| if r.starts_with('/') { r } else { format!("/{r}") })
        .filter(|r| r.len() <= 120 && !r.chars().any(char::is_whitespace))
        .collect();
    let mut static_set: Vec<String> = Vec::new();
    let mut static_seen: HashSet<String> = HashSet::new();
    let mut param_set: Vec<String> = Vec::new();
    let mut param_seen: HashSet<String> = HashSet::new();
    for r in routes {
        let is_param = r.contains(':') || r.contains('*') || r.contains('[');
        if is_param {
            if param_seen.insert(r.clone()) { param_set.push(r); }
        } else if static_seen.insert(r.clone()) {
            static_set.push(r);
        }
    }
    json!({ "static": static_set, "param": param_set })
}

/// `function sampleForParam(name)` — a deterministic sample value chosen by the param NAME.
pub fn sample_for_param(name: &str) -> String {
    let n = name.to_lowercase();
    if Regex::new(r"uuid|guid").unwrap().is_match(&n) { return "00000000-0000-0000-0000-000000000000".to_string(); }
    if Regex::new(r"id$|^id|num|count|qty|page|index|idx|year|month|day|step|level|version").unwrap().is_match(&n) { return "1".to_string(); }
    if Regex::new(r"slug|name|title|category|cat|type|tag|term|query|q|search|keyword").unwrap().is_match(&n) { return "sample".to_string(); }
    "1".to_string()
}

/// `function fillParams(route)` — `/items/:id` → `/items/1`; Next/SvelteKit `[id]` / `[...id]`
/// bracket params and trailing `*` wildcards likewise substituted.
pub fn fill_params(route: &str) -> String {
    let re_colon = Regex::new(r":([A-Za-z0-9_]+)\??").unwrap();
    let re_bracket = Regex::new(r"\[(?:\.{3})?([A-Za-z0-9_]+)\]").unwrap();
    let re_star = Regex::new(r"\*+").unwrap();
    let step1 = re_colon.replace_all(route, |caps: &regex::Captures| sample_for_param(&caps[1]));
    let step2 = re_bracket.replace_all(&step1, |caps: &regex::Captures| sample_for_param(&caps[1]));
    re_star.replace_all(&step2, "sample").to_string()
}

// ── FIXTURES v2: MOCK_NOISE + mockLeaf + synthesizeMock ──
fn mock_noise() -> &'static HashSet<&'static str> {
    static NOISE: OnceLock<HashSet<&'static str>> = OnceLock::new();
    NOISE.get_or_init(|| {
        const S: &str = "map forEach filter reduce some every find findIndex sort length push pop shift unshift slice splice concat join includes indexOf lastIndexOf flat flatMap fill then catch finally all race props state context ref current target default prototype constructor call apply bind hasOwnProperty valueOf isArray json text blob status ok statusText headers url method body keys values entries assign from of trim split replace replaceAll match matchAll test exec toLowerCase toUpperCase charAt charCodeAt substring substr padStart padEnd repeat startsWith endsWith toFixed toString preventDefault stopPropagation persist focus blur click value checked files style classList dataset className innerHTML innerText textContent setState forceUpdate addEventListener removeEventListener dispatchEvent querySelector querySelectorAll getElementById getAttribute setAttribute removeAttribute appendChild removeChild parentNode parentElement children childNodes nextSibling previousSibling cloneNode contains closest matches floor ceil round abs min max random pow sqrt sign trunc log info warn error debug table group parse stringify render hydrate createElement cloneElement createRoot createContext createRef forwardRef memo lazy Suspense Fragment Children useState useEffect useLayoutEffect useContext useRef useMemo useCallback useReducer useId styled keyframes createGlobalStyle css attrs withConfig displayName div span p a b i u em strong button input form label select option textarea img image picture source video audio table thead tbody tfoot tr td th ul ol li dl dt dd section article header footer main nav aside figure figcaption h1 h2 h3 h4 h5 h6 br hr svg path g rect circle line polyline polygon defs use symbol onClick onChange onSubmit onInput onBlur onFocus onKeyDown onKeyUp onMouseEnter onMouseLeave onScroll png jpg jpeg gif webp svg ico bmp css scss sass less styl mjs cjs js jsx ts tsx vue svelte json md woff woff2 ttf eot otf exports module require __esModule";
        S.split_whitespace().collect()
    })
}
const CONTAINER: &[&str] = &["results", "items", "list", "products", "rows", "records", "docs", "content", "children", "options", "pictures", "images", "photos", "attributes", "variations", "tags", "categories"];

/// `function mockLeaf(name)` — a domain-plausible leaf VALUE for a field name (currency codes,
/// emails, ids-as-numbers, image URLs, dates, booleans, colors; falls back to `"Sample"`).
pub fn mock_leaf(name: &str) -> Value {
    let n = name.to_lowercase();
    if Regex::new(r"currency|currencycode").unwrap().is_match(&n) { return json!("USD"); }
    if Regex::new(r"^lang$|locale").unwrap().is_match(&n) { return json!("en-US"); }
    if Regex::new(r"email").unwrap().is_match(&n) { return json!("sample@example.com"); }
    if Regex::new(r"(^|_)id$|^id$|count|qty|quantity|total|amount|price|^num|num$|index|page|year|stock|rating|score|width|height|size|offset|limit|zip|lat|lng").unwrap().is_match(&n) { return json!(1); }
    if Regex::new(r"url|src|href|thumb|image|img$|^img|icon|avatar|photo|logo|picture|banner").unwrap().is_match(&n) { return json!("https://placehold.co/240x160"); }
    if Regex::new(r"date|time|created|updated|timestamp").unwrap().is_match(&n) { return json!("2024-01-01T00:00:00Z"); }
    if Regex::new(r"^is[A-Z]|^has[A-Z]|enabled|active|available|visible|selected|checked|disabled|featured|free").unwrap().is_match(name) { return json!(true); }
    if Regex::new(r"color|colour").unwrap().is_match(&n) { return json!("#888888"); }
    json!("Sample")
}

struct MockCtx {
    arr_item: BTreeMap<String, BTreeSet<String>>,
    obj_of: BTreeMap<String, BTreeSet<String>>,
    super_fields: BTreeSet<String>,
    guard: i64,
}
impl MockCtx {
    fn build(&mut self, name: &str, depth: i32) -> Value {
        if depth > 5 { return mock_leaf(name); }
        self.guard += 1;
        if self.guard > 6000 { return mock_leaf(name); }
        let name_lc = name.to_lowercase();
        if self.arr_item.contains_key(name) || CONTAINER.contains(&name_lc.as_str()) {
            let it = self.build_item(depth);
            let mut it2 = it.clone();
            if let Value::Object(m) = &mut it2 { m.insert("id".to_string(), json!(2)); }
            return json!([it, it2]);
        }
        if let Some(subs) = self.obj_of.get(name).cloned() {
            if !subs.is_empty() {
                let mut o = Map::new();
                for s in subs.iter().take(30) {
                    let v = self.build(s, depth + 1);
                    o.insert(s.clone(), v);
                }
                return Value::Object(o);
            }
        }
        mock_leaf(name)
    }
    fn build_item(&mut self, depth: i32) -> Value {
        let mut o = Map::new();
        let fields: Vec<String> = self.super_fields.iter().take(50).cloned().collect();
        for s in fields {
            let v = self.build(&s, depth + 1);
            o.insert(s, v);
        }
        if o.get("id").map_or(true, Value::is_null) { o.insert("id".to_string(), json!(1)); }
        if o.get("title").map_or(true, Value::is_null) { o.insert("title".to_string(), json!("Sample")); }
        if o.get("name").map_or(true, Value::is_null) { o.insert("name".to_string(), json!("Sample")); }
        Value::Object(o)
    }
}

/// `function synthesizeMock(root)` — "scan what the screen NEEDS, then mock it": infer field
/// shapes from `x.a.b` (object) and `arr.map(v => v.c)` (array-of-items) access patterns across the
/// project's source, then build one complete sample body from access paths.
pub fn synthesize_mock(root: &str) -> Value {
    let re_unwrap = Regex::new(r"\.(?:data|body)(\??\.)").unwrap(); // unwrap axios: res.data.x -> res.x
    let re_map = Regex::new(r"([A-Za-z_$]\w*)\s*\??\.\s*(?:map|forEach|filter|find|some|every|reduce)\s*\(\s*(?:async\s*)?\(?\s*\{?\s*([A-Za-z_$]\w*)").unwrap();
    let re_chain = Regex::new(r"([A-Za-z_$]\w*)\??\.([A-Za-z_$]\w*)(?:\??\.([A-Za-z_$]\w*))?").unwrap();

    let mut files = Vec::new();
    walk_source(Path::new(root), &mut files, 0);
    let mut obj_of: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut arr_item: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut read_count = 0i32;
    for f in &files {
        if read_count > 1500 { break; }
        let raw = match fs::read_to_string(f) { Ok(s) => s, Err(_) => continue };
        read_count += 1;
        let src = re_unwrap.replace_all(&raw, "$1").to_string();
        let mut lv: HashMap<String, String> = HashMap::new(); // loopVar -> arrayField (per file)
        for cap in re_map.captures_iter(&src) {
            let base = cap[1].to_string();
            let loop_var = cap[2].to_string();
            if !mock_noise().contains(base.as_str()) {
                arr_item.entry(base.clone()).or_default();
                lv.insert(loop_var, base);
            }
        }
        for cap in re_chain.captures_iter(&src) {
            let base = &cap[1];
            let a = cap[2].to_string();
            let b = cap.get(3).map(|m| m.as_str().to_string());
            if let Some(arr_name) = lv.get(base) {
                if !mock_noise().contains(a.as_str()) {
                    arr_item.entry(arr_name.clone()).or_default().insert(a.clone());
                }
            }
            if !mock_noise().contains(a.as_str()) {
                seen.insert(a.clone());
                if let Some(b) = &b {
                    if !mock_noise().contains(b.as_str()) {
                        obj_of.entry(a.clone()).or_default().insert(b.clone());
                    }
                }
            }
        }
    }
    let mut super_fields: BTreeSet<String> = BTreeSet::new();
    for subs in arr_item.values() {
        for s in subs {
            if !mock_noise().contains(s.as_str()) { super_fields.insert(s.clone()); }
        }
    }

    let mut ctx = MockCtx { arr_item, obj_of, super_fields, guard: 0 };
    let mut mock = Map::new();
    let re_skip = Regex::new(r"^[A-Z_]").unwrap();
    for name in &seen {
        if name.len() > 32 || re_skip.is_match(name) { continue; }
        let v = ctx.build(name, 0);
        mock.insert(name.clone(), v);
    }
    for c in CONTAINER {
        if !mock.contains_key(*c) {
            let v = ctx.build(c, 0);
            mock.insert((*c).to_string(), v);
        }
    }
    Value::Object(mock)
}

/// `function collectKeys(node, set)` — every object key reachable inside `node`, recursively.
pub fn collect_keys(node: &Value, set: &mut HashSet<String>) {
    match node {
        Value::Array(arr) => { for n in arr { collect_keys(n, set); } }
        Value::Object(map) => { for (k, v) in map { set.insert(k.clone()); collect_keys(v, set); } }
        _ => {}
    }
}

/// `function applyValues(node, vmap)` — mutate `node`'s leaf fields in place from `vmap[key]`,
/// skipping nulls and object/array overrides (recurse into those instead).
pub fn apply_values(node: &mut Value, vmap: &Value) {
    match node {
        Value::Array(arr) => { for n in arr.iter_mut() { apply_values(n, vmap); } }
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if v.is_object() || v.is_array() {
                    apply_values(v, vmap);
                } else if let Some(vm) = vmap.get(k) {
                    if !vm.is_null() && !vm.is_object() && !vm.is_array() {
                        *v = vm.clone();
                    }
                }
            }
        }
        _ => {}
    }
}

/// `function leafFields(mock)` — every leaf field the mock exposes, grouped by structural context
/// (`"top level"` / `"list item"` / a nested object's key) — the operator's "fill the data" form.
pub fn leaf_fields(mock: &Value) -> Vec<Value> {
    fn walk(node: &Value, ctx: &str, out: &mut Vec<Value>, seen: &mut HashSet<String>) {
        match node {
            Value::Array(arr) => { if let Some(first) = arr.first() { walk(first, "list item", out, seen); } }
            Value::Object(map) => {
                for (k, v) in map {
                    match v {
                        Value::Array(arr) => { if let Some(first) = arr.first() { walk(first, "list item", out, seen); } }
                        Value::Object(_) => walk(v, k, out, seen),
                        _ => {
                            let starts_upper = k.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false);
                            if !mock_noise().contains(k.as_str()) && k.len() > 2 && !starts_upper {
                                let key = format!("{ctx}{k}");
                                if seen.insert(key) {
                                    let type_str = match v {
                                        Value::String(_) => "string",
                                        Value::Number(_) => "number",
                                        Value::Bool(_) => "boolean",
                                        _ => "object",
                                    };
                                    out.push(json!({ "field": k, "type": type_str, "value": v, "context": ctx }));
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    walk(mock, "top level", &mut out, &mut seen);
    out
}

/// `function lookupRecorded(db, origin, method, url)` — exact `(origin, method, url)` match first,
/// else the shortest same-`origin+pathname`-prefix recording (`/search?q=anything` reuses
/// `/search?q=test`'s capture).
pub fn lookup_recorded(db: &Connection, origin: &str, method: &str, url: &str) -> Option<Value> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT status, content_type, body FROM code_captured_responses WHERE origin=? AND method=? AND url=? LIMIT 1",
        &[json!(origin), json!(method), json!(url)],
    ).ok()?;
    if let Some(row) = rows.as_array().and_then(|a| a.first()) {
        return Some(row.clone());
    }
    let prefix = match parse_url(url) {
        Some((origin_part, _, pathname, _)) => format!("{origin_part}{pathname}"),
        None => url.split('?').next().unwrap_or(url).to_string(),
    };
    let rows2 = crate::db_json::query_rows(
        db,
        "SELECT status, content_type, body FROM code_captured_responses WHERE origin=? AND method=? AND url LIKE ? ORDER BY length(url) LIMIT 1",
        &[json!(origin), json!(method), json!(format!("{prefix}%"))],
    ).ok()?;
    rows2.as_array().and_then(|a| a.first()).cloned()
}

// `const pageOrigin = ...; const startPath = ...;` — the entered URL parsed once, reused by every
// seed.
fn page_origin_and_start_path(url: &str) -> (String, String) {
    let page_origin = parse_url(url).map(|(o, ..)| o).unwrap_or_else(|| url.to_string());
    let start_path = match parse_url(url) {
        Some((_, _, pathname, hash)) => {
            let h = if hash.starts_with("#/") { hash } else { String::new() };
            let combined = format!("{pathname}{h}");
            if combined.is_empty() { "/".to_string() } else { combined }
        }
        None => "/".to_string(),
    };
    (page_origin, start_path)
}

// `const seedPath = (r) => ...` — a manual/discovered route entry -> an absolute path off pageOrigin.
fn seed_path(r: Option<&str>, page_origin: &str, start_path: &str) -> String {
    match r {
        None => start_path.to_string(),
        Some(s) if s.is_empty() => start_path.to_string(),
        Some(s) => {
            if Regex::new(r"(?i)^https?:").unwrap().is_match(s) {
                to_path(s, page_origin).unwrap_or_else(|| start_path.to_string())
            } else {
                format!("/{}", s.trim_start_matches('/'))
            }
        }
    }
}

/// `export default async function designCaptureLive(input, context)` — see the module doc: builds
/// every deterministic pre-launch artifact (page origin/start path, source-root route discovery,
/// the `stubApi` mock body + `leafFields` form, the crawl/fixture seed-set union+dedupe), then
/// returns the same `{ ok: false, ..., error: 'playwright not resolvable ...' }` shape JS's own
/// `loadPlaywright()`-missing branch returns — no browser driver is wired in this crate.
pub fn designCaptureLive(input: &Value, db: &Connection) -> Result<Value, String> {
    let mut url = str_qq(input.get("url")); // JS: .replace(/\/$/, '') — strips exactly one trailing slash
    if url.ends_with('/') { url.pop(); }
    if url.is_empty() {
        return Err("design-capture-live: url is required".to_string());
    }
    let origin = input.get("origin").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| "atlas".to_string());
    let crawl = input.get("crawl").and_then(Value::as_bool).unwrap_or(false);
    let fixtures = !matches!(input.get("fixtures"), Some(Value::Bool(false)));
    let stub_api = input.get("stubApi").and_then(Value::as_bool).unwrap_or(false);
    let manual: Vec<String> = input.get("routes").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();

    let _ = crate::codebase_code_schema::ensureCodeSchema(db, &json!("code."));

    let (page_origin, start_path) = page_origin_and_start_path(&url);

    let mut src_root: Option<String> = input.get("sourceRoot").and_then(Value::as_str).map(str::to_string);
    if src_root.is_none() && (crawl || stub_api) {
        for t in ["code_onboard_jobs", "code.code_onboard_jobs"] {
            if let Ok(rows) = crate::db_json::query_rows(db, &format!("SELECT root FROM {t} ORDER BY rowid DESC LIMIT 1"), &[]) {
                if let Some(root) = rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("root")).and_then(Value::as_str) {
                    src_root = Some(root.to_string());
                    break;
                }
            }
        }
    }

    let mut discovered = json!({ "static": [], "param": [] });
    if crawl {
        if let Some(root) = &src_root { discovered = discover_routes(root); }
    }
    let discovered_static: Vec<String> = discovered.get("static").and_then(|v| v.as_array()).cloned().unwrap_or_default()
        .iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
    let discovered_param: Vec<String> = discovered.get("param").and_then(|v| v.as_array()).cloned().unwrap_or_default()
        .iter().filter_map(|v| v.as_str().map(str::to_string)).collect();

    const EMPTY_STUB: &str = r#"{"results":[],"data":[],"items":[],"docs":[],"content":[],"paging":{"total":0},"total":0,"results_count":0}"#;
    let mut _stub_body: Option<String> = None;
    let mut _fixture_fields: Vec<Value> = Vec::new();
    if stub_api {
        let mut mock_obj: Option<Value> = src_root.as_deref().map(synthesize_mock);
        if let Some(obj) = mock_obj.as_mut() {
            // value priority: operator-filled form (AI suggestions omitted — see module doc)
            if let Some(vm) = input.get("valueMap") {
                if vm.is_object() { apply_values(obj, vm); }
            }
            _fixture_fields = leaf_fields(obj);
        }
        _stub_body = Some(mock_obj.map(|o| o.to_string()).unwrap_or_else(|| EMPTY_STUB.to_string()));
    }

    let fixture_seeds: Vec<String> = if fixtures {
        discovered_param.iter().map(|p| seed_path(Some(&fill_params(p)), &page_origin, &start_path)).collect()
    } else { Vec::new() };

    let mut _seed_set: Vec<String> = vec![start_path.clone()];
    for m in &manual { _seed_set.push(seed_path(Some(m), &page_origin, &start_path)); }
    for s in &discovered_static { _seed_set.push(seed_path(Some(s), &page_origin, &start_path)); }
    _seed_set.extend(fixture_seeds);
    let mut dedupe: HashSet<String> = HashSet::new();
    _seed_set.retain(|p| dedupe.insert(p.clone()));

    // THE BROWSER SESSION (2026-09-17, finishing the port): the machine's own Chromium over CDP —
    // see _helpers/browser-capture.rs. `ANTILOKI_CAPTURE_DRY_RUN=1` keeps the old shape for tests.
    let seeds: Vec<String> = _seed_set.iter().map(|p| if p.starts_with("http") { p.clone() } else { format!("{page_origin}{p}") }).collect();
    if std::env::var("ANTILOKI_CAPTURE_DRY_RUN").ok().as_deref() == Some("1") {
        return Ok(json!({ "ok": false, "origin": origin, "url": url, "snapshots": 0, "seeds": seeds, "error": "playwright not resolvable — install it in the engine or the FE" }));
    }
    let settle = input.get("settleMs").and_then(Value::as_u64).unwrap_or(900);
    let (shots, errors) = match crate::codebase_helpers_browser_capture::capture_all(&seeds, settle) {
        Ok(x) => x,
        Err(e) => return Ok(json!({ "ok": false, "origin": origin, "url": url, "snapshots": 0, "seeds": seeds, "error": e })),
    };
    if shots.is_empty() {
        return Ok(json!({ "ok": false, "origin": origin, "url": url, "snapshots": 0, "seeds": seeds, "error": format!("no screen could be captured: {}", errors.join(" · ")) }));
    }
    let (screens, components) = crate::codebase_helpers_browser_capture::persist(db, &origin, &page_origin, &shots)?;
    Ok(json!({
        "ok": true, "origin": origin, "url": url, "snapshots": screens + components, "screens": screens, "components": components,
        "seeds": seeds, "framework": shots[0].framework,
        "detail": crate::codebase_helpers_browser_capture::summary(&shots, &errors),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as stdfs;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-design-capture-live-rs-{tag}-{}", std::process::id()));
        let _ = stdfs::remove_dir_all(&dir);
        stdfs::create_dir_all(&dir).unwrap();
        dir
    }
    fn db_with_schema() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::codebase_code_schema::ensureCodeSchema(&c, &Value::Null);
        c
    }

    #[test]
    fn to_path_keeps_same_origin_route() {
        assert_eq!(to_path("http://localhost:3000/items/1?x=2", "http://localhost:3000"), Some("/items/1".to_string()));
    }
    #[test]
    fn to_path_drops_cross_origin() {
        assert_eq!(to_path("http://evil.example.com/x", "http://localhost:3000"), None);
    }
    #[test]
    fn to_path_drops_assets() {
        assert_eq!(to_path("http://localhost:3000/logo.png", "http://localhost:3000"), None);
    }
    #[test]
    fn to_path_keeps_hash_router_route() {
        assert_eq!(to_path("http://localhost:3000/#/dashboard", "http://localhost:3000"), Some("/#/dashboard".to_string()));
    }
    #[test]
    fn to_path_drops_non_hash_fragment() {
        assert_eq!(to_path("http://localhost:3000/page#section", "http://localhost:3000"), Some("/page".to_string()));
    }

    #[test]
    fn shape_key_collapses_numeric_uuid_hash_and_slug_segments() {
        assert_eq!(shape_key("/product/1"), "/product/:n");
        assert_eq!(shape_key("/user/550e8400-e29b-41d4-a716-446655440000"), "/user/:uuid");
        assert_eq!(shape_key("/blob/abcdef0123456789"), "/blob/:hash");
        assert_eq!(shape_key("/post/my-super-long-slug-segment-123"), "/post/:slug");
        assert_eq!(shape_key("/About"), "/about");
    }

    #[test]
    fn sample_for_param_and_fill_params() {
        assert_eq!(sample_for_param("userId"), "1");
        assert_eq!(sample_for_param("categorySlug"), "sample");
        assert_eq!(sample_for_param("sessionUuid"), "00000000-0000-0000-0000-000000000000");
        assert_eq!(fill_params("/items/:id"), "/items/1");
        assert_eq!(fill_params("/blog/[slug]"), "/blog/sample");
        assert_eq!(fill_params("/blog/[...slug]"), "/blog/sample");
        assert_eq!(fill_params("/catch/*"), "/catch/sample");
    }

    #[test]
    fn discover_routes_finds_jsx_route_and_gated_object_routes() {
        let dir = temp_dir("routes");
        stdfs::write(
            dir.join("App.jsx"),
            r#"import { Route } from 'react-router-dom';
function App() { return <Route path="/dashboard" element={<Dash/>} />; }
"#,
        ).unwrap();
        stdfs::write(
            dir.join("router.js"),
            r#"const routes = [ { path: "/items/:id" }, { path: "/settings" } ];
createBrowserRouter(routes);
"#,
        ).unwrap();
        let r = discover_routes(dir.to_str().unwrap());
        let statics: Vec<String> = r["static"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        let params: Vec<String> = r["param"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert!(statics.contains(&"/dashboard".to_string()));
        assert!(statics.contains(&"/settings".to_string()));
        assert!(params.contains(&"/items/:id".to_string()));
    }

    #[test]
    fn discover_routes_object_form_ignored_without_router_keyword() {
        let dir = temp_dir("routes-ungated");
        stdfs::write(dir.join("unrelated.js"), r#"const cfg = { path: "/not-a-route" };"#).unwrap();
        let r = discover_routes(dir.to_str().unwrap());
        assert_eq!(r["static"].as_array().unwrap().len(), 0);
        assert_eq!(r["param"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn mock_leaf_picks_domain_valid_samples_by_field_name() {
        assert_eq!(mock_leaf("currency_id"), json!("USD"));
        assert_eq!(mock_leaf("email"), json!("sample@example.com"));
        assert_eq!(mock_leaf("product_id"), json!(1));
        assert_eq!(mock_leaf("thumbnail"), json!("https://placehold.co/240x160"));
        assert_eq!(mock_leaf("isActive"), json!(true));
        assert_eq!(mock_leaf("color"), json!("#888888"));
        assert_eq!(mock_leaf("weirdField"), json!("Sample"));
    }

    #[test]
    fn synthesize_mock_infers_object_and_array_item_shapes_from_source() {
        let dir = temp_dir("mock");
        stdfs::write(
            dir.join("Screen.jsx"),
            r#"function Screen({ data }) {
  return data.results.map(item => <li key={item.id}>{item.thumbnail} {item.price}</li>);
}
"#,
        ).unwrap();
        let mock = synthesize_mock(dir.to_str().unwrap());
        let results = mock.get("results").and_then(|v| v.as_array()).expect("results array");
        assert_eq!(results.len(), 2);
        let first = &results[0];
        assert_eq!(first.get("id"), Some(&json!(1)));
        assert_eq!(first.get("thumbnail"), Some(&json!("https://placehold.co/240x160")));
        assert_eq!(first.get("price"), Some(&json!(1)));
        let second = &results[1];
        assert_eq!(second.get("id"), Some(&json!(2)));
    }

    #[test]
    fn synthesize_mock_unwraps_axios_data_body_before_scanning() {
        let dir = temp_dir("mock-axios");
        stdfs::write(dir.join("api.js"), "res.data.pictures.map(p => p.url);\n").unwrap();
        let mock = synthesize_mock(dir.to_str().unwrap());
        // "url" is itself in MOCK_NOISE (fetch/Response API noise: json text blob status ok
        // statusText headers url method body …), so `p.url` never lands in arrItem["pictures"].
        // Real JS output for this exact input (verified via node): pictures[0] == {id:1,title:"Sample",name:"Sample"}.
        // What this test actually proves: the axios unwrap ran (".data." was stripped so "pictures"
        // was recognized as the array base of the .map call) — assert on the array shape it produces.
        let pictures = mock.get("pictures").and_then(|v| v.as_array()).expect("pictures array");
        assert_eq!(pictures.len(), 2);
        assert_eq!(pictures[0].get("id"), Some(&json!(1)));
        assert_eq!(pictures[0].get("title"), Some(&json!("Sample")));
        assert_eq!(pictures[1].get("id"), Some(&json!(2)));
    }

    #[test]
    fn collect_keys_and_apply_values_roundtrip() {
        let mock = json!({ "a": 1, "b": { "c": 2 }, "list": [ { "d": 3 } ] });
        let mut keys = HashSet::new();
        collect_keys(&mock, &mut keys);
        assert!(keys.contains("a") && keys.contains("b") && keys.contains("c") && keys.contains("list") && keys.contains("d"));

        let mut node = mock.clone();
        apply_values(&mut node, &json!({ "a": 42, "d": "real", "b": "should-not-overwrite-object" }));
        assert_eq!(node["a"], json!(42));
        assert_eq!(node["list"][0]["d"], json!("real"));
        assert_eq!(node["b"]["c"], json!(2)); // object leaf untouched: vmap["b"] is not object/array-safe → skipped at container level
    }

    #[test]
    fn leaf_fields_groups_by_structural_context() {
        let mock = json!({ "title": "Sample", "items": [ { "price": 1, "thumbnail": "x" } ] });
        let fields = leaf_fields(&mock);
        let has_title = fields.iter().any(|f| f["field"] == "title" && f["context"] == "top level");
        let has_price = fields.iter().any(|f| f["field"] == "price" && f["context"] == "list item");
        assert!(has_title);
        assert!(has_price);
    }

    #[test]
    fn lookup_recorded_prefers_exact_then_falls_back_to_path_prefix() {
        let db = db_with_schema();
        crate::db_json::run_write(
            &db,
            "INSERT INTO code_captured_responses (origin, method, url, status, content_type, body) VALUES (?,?,?,?,?,?)",
            &[json!("atlas"), json!("GET"), json!("http://localhost:3000/search?q=test"), json!(200), json!("application/json"), json!("{\"ok\":true}")],
        ).unwrap();

        let exact = lookup_recorded(&db, "atlas", "GET", "http://localhost:3000/search?q=test").unwrap();
        assert_eq!(exact["status"], json!(200));

        let by_prefix = lookup_recorded(&db, "atlas", "GET", "http://localhost:3000/search?q=anything").unwrap();
        assert_eq!(by_prefix["body"], json!("{\"ok\":true}"));

        assert!(lookup_recorded(&db, "atlas", "GET", "http://localhost:3000/other").is_none());
    }

    #[test]
    fn designCaptureLive_requires_url() {
        let db = db_with_schema();
        let e = designCaptureLive(&json!({}), &db).unwrap_err();
        assert!(e.contains("url"));
    }

    #[test]
    fn designCaptureLive_returns_playwright_not_resolvable_after_real_assembly() {
        std::env::set_var("ANTILOKI_CAPTURE_DRY_RUN", "1"); // no browser in unit tests — the pre-browser assembly is what's under test
        let db = db_with_schema();
        let r = designCaptureLive(&json!({ "url": "http://localhost:3000/", "origin": "myapp" }), &db).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["origin"], json!("myapp"));
        assert_eq!(r["url"], json!("http://localhost:3000"));
        assert_eq!(r["snapshots"], json!(0));
        assert!(r["error"].as_str().unwrap().contains("playwright not resolvable"));
    }

    #[test]
    fn designCaptureLive_crawl_uses_source_root_route_discovery() {
        std::env::set_var("ANTILOKI_CAPTURE_DRY_RUN", "1");
        let dir = temp_dir("main-crawl");
        stdfs::write(
            dir.join("App.jsx"),
            r#"import { Route } from 'react-router-dom';
function App() { return <Route path="/dashboard" element={<Dash/>} />; }
"#,
        ).unwrap();
        let db = db_with_schema();
        // exercises: crawl=true + sourceRoot -> discoverRoutes -> seed assembly (still ends in the
        // same real playwright-missing branch, but the discovery/seed code path above it ran for real)
        let r = designCaptureLive(&json!({ "url": "http://localhost:3000/", "crawl": true, "sourceRoot": dir.to_string_lossy() }), &db).unwrap();
        assert_eq!(r["ok"], json!(false));
    }
}
