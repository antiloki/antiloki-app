//! THE FE <-> SERVE CONTRACT (PLAN-THE-TESTS phase 4): every `/api/v1/...` path the frontend's source calls must be a route the serve registers.
//! A renamed or removed route otherwise fails only when a user clicks the button. Reads the sibling checkout `../../antiloki-ui/src` (CI checks
//! the two repos out side by side); when it is not there the test says so and passes - it is a guard, not a requirement to have the FE.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        // loki/demo/ is the marketing demo's route MOCKER: its "/api/v1/board" style prefixes are patterns it intercepts, not calls to the serve
        if p.is_dir() { if !matches!(p.file_name().and_then(|n| n.to_str()), Some("node_modules" | "dist" | "coverage" | "demo")) { walk(&p, out); } }
        else if matches!(p.extension().and_then(|x| x.to_str()), Some("ts" | "tsx")) && !p.to_string_lossy().contains(".test.") { out.push(p); }
    }
}

/// `/api/v1/...` string / template literals in a source text, normalised: query string dropped, every `${...}` becomes `{}`
fn fe_paths(src: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let b = src.as_bytes();
    let mut i = 0;
    while let Some(pos) = src[i..].find("/api/v1/") {
        let start = i + pos;
        // the literal must begin at a quote: the char before is ' " or `
        // (a `${BASE}/api/v1/...` template starts with an interpolation, so the char before is `}`)
        let opens = start > 0 && matches!(b[start - 1], b'\'' | b'"' | b'`' | b'}');
        let mut j = start; let mut depth = 0;
        while j < b.len() {
            match b[j] {
                b'$' if j + 1 < b.len() && b[j + 1] == b'{' => { depth += 1; j += 2; continue }
                b'}' if depth > 0 => { depth -= 1; }
                b'\'' | b'"' | b'`' | b'\n' | b' ' | b')' | b',' | b';' if depth == 0 => break,
                _ => {}
            }
            j += 1;
        }
        i = j.max(start + 8);
        if !opens { continue }
        let raw = &src[start..j];
        let mut norm = String::new(); let mut k = 0; let rb = raw.as_bytes();
        while k < rb.len() {
            if rb[k] == b'$' && k + 1 < rb.len() && rb[k + 1] == b'{' { let mut d = 0; while k < rb.len() { if rb[k] == b'{' { d += 1 } if rb[k] == b'}' { d -= 1; if d == 0 { k += 1; break } } k += 1; } norm.push_str("{}"); }
            else { norm.push(rb[k] as char); k += 1; }
        }
        let norm = norm.split(['?', '#']).next().unwrap_or("").trim_end_matches('/').to_string();
        if norm.len() > "/api/v1/".len() { found.insert(norm); }
    }
    found
}

/// every path the serve registers: the generated passthrough table plus the hand-written `.route("...")` calls in lib.rs
fn serve_routes() -> Vec<String> {
    let mut v: Vec<String> = antiloki_serve::routes::ROUTES.iter().map(|r| r.path.to_string()).collect();
    let lib = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs")).expect("lib.rs");
    let mut rest = lib.as_str();
    while let Some(p) = rest.find(".route(\"") { rest = &rest[p + 8..]; if let Some(e) = rest.find('"') { v.push(rest[..e].to_string()); } }
    v
}

fn segs(p: &str) -> Vec<&str> { p.trim_matches('/').split('/').collect() }
/// a route pattern matches a FE path when every segment is equal, or either side is a parameter (`{op}` on the serve, `{}` from a template literal)
fn matches(route: &str, fe: &str) -> bool {
    let (r, f) = (segs(route), segs(fe));
    let is_param = |s: &str| s.starts_with('{') ;
    r.len() == f.len() && r.iter().zip(f.iter()).all(|(a, b)| a == b || is_param(a) || is_param(b) || b.contains("{}") && a.starts_with(&b[..b.find("{}").unwrap()]))
}

#[test]
fn every_path_the_frontend_calls_is_a_route_the_serve_registers() {
    let fe_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../antiloki-ui/src");
    if !fe_src.is_dir() { eprintln!("no sibling antiloki-ui checkout at {} - contract not checked", fe_src.display()); return }
    let mut files = vec![]; walk(&fe_src, &mut files);
    assert!(files.len() > 100, "the FE source walk found only {} files - the walk is broken", files.len());
    let routes = serve_routes();
    assert!(routes.len() > 300, "the serve route list is suspiciously short ({})", routes.len());
    let mut missing: Vec<String> = vec![];
    let mut checked = 0;
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap_or_default();
        for p in fe_paths(&src) {
            checked += 1;
            if !routes.iter().any(|r| matches(r, &p)) { missing.push(format!("{p}   ({})", f.strip_prefix(&fe_src).unwrap_or(f).display())); }
        }
    }
    missing.sort(); missing.dedup();
    eprintln!("checked {checked} FE path literals against {} serve routes", routes.len());
    assert!(checked > 100, "only {checked} FE paths were extracted - the extractor is broken");
    assert!(missing.is_empty(), "the FE calls {} path(s) the serve does not register:\n{}", missing.len(), missing.join("\n"));
}

#[test]
fn the_path_extractor_and_matcher_read_the_shapes_the_fe_uses() {
    let s = r#"apiPost(`/api/v1/cockpit/queue/${op}`, b); apiGet("/api/v1/cockpit/goals?limit=3"); fetch(`${BE}/api/v1/workspaces/add`); x = '/api/v1/a/b/'; "not /api/v1/inside-a-word""#;
    let got = fe_paths(s);
    assert!(got.contains("/api/v1/cockpit/queue/{}"), "{got:?}");
    assert!(got.contains("/api/v1/cockpit/goals"), "{got:?}");
    assert!(got.contains("/api/v1/workspaces/add"), "{got:?}");
    assert!(got.contains("/api/v1/a/b"), "{got:?}");
    assert!(matches("/api/v1/cockpit/queue/{op}", "/api/v1/cockpit/queue/{}"));
    assert!(matches("/api/v1/cockpit/goals", "/api/v1/cockpit/goals"));
    assert!(!matches("/api/v1/cockpit/goals", "/api/v1/cockpit/goal"));
    assert!(!matches("/api/v1/cockpit/goals/{op}", "/api/v1/cockpit/goals"));
}
