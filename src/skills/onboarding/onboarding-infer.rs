//! onboarding-infer.rs — twin of skills/onboarding/onboarding-infer.mjs (no-db, pure). NAMES ARE
//! CONTRACT: onboardingInfer (default) · inferArchitecture · inferPatterns · inferDomain.
// QUALITY: 81/100 — deterministic scan → architecture layers, naming/ext patterns, domain entities
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
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
fn last_sep(p: &str) -> Option<usize> { p.rfind(|c| c == '/' || c == '\\') }
fn basename(p: &str, ext: &str) -> String {
    let trimmed = p.trim_end_matches(|c| c == '/' || c == '\\');
    let base = match last_sep(trimmed) { Some(i) => &trimmed[i + 1..], None => trimmed }.to_string();
    if !ext.is_empty() && base.len() > ext.len() && base.ends_with(ext) { base[..base.len() - ext.len()].to_string() } else { base }
}
fn dirname(p: &str) -> String {
    let trimmed = p.trim_end_matches(|c| c == '/' || c == '\\');
    match last_sep(trimmed) { None => ".".into(), Some(0) => trimmed[..1].into(), Some(i) => trimmed[..i].into() }
}

fn layer_hints() -> &'static Vec<(Regex, &'static str)> {
    static H: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    H.get_or_init(|| vec![
        (Regex::new(r"(?i)^(antiloki-|packages?/)?antiloki-fe|frontend|client|web|app$").unwrap(), "frontend"),
        (Regex::new(r"(?i)^(antiloki-|packages?/)?antiloki-be|backend|server|api$").unwrap(), "backend"),
        (Regex::new(r"(?i)^(packages?/)?shared|common|lib|core$").unwrap(), "shared"),
        (Regex::new(r"(?i)^(packages?/)?infra|deploy|ops|terraform|k8s$").unwrap(), "infra"),
        (Regex::new(r"(?i)^docs?$").unwrap(), "docs"),
        (Regex::new(r"(?i)^scripts?|tools?|bin$").unwrap(), "tooling"),
    ])
}

pub fn inferArchitecture(scan: &Value) -> Value {
    let empty: Vec<Value> = vec![];
    let mut top_dirs: Vec<String> = Vec::new(); // insertion-ordered Set
    for f in scan.get("sample_files").and_then(|v| v.as_array()).unwrap_or(&empty) {
        let path = str_qq(f.get("path"));
        let top = path.split('/').next().unwrap_or("");
        if !top.is_empty() && !top.contains('.') && !top_dirs.contains(&top.to_string()) { top_dirs.push(top.to_string()); }
    }
    let mut layers: Vec<Value> = Vec::new();
    for dir in &top_dirs {
        if let Some((_, kind)) = layer_hints().iter().find(|(re, _)| re.is_match(dir)) {
            layers.push(json!({ "name": dir, "kind": kind, "root_path": dir }));
        }
    }
    let is_monorepo = scan.get("stack").and_then(|s| s.get("markers")).and_then(|v| v.as_array())
        .map_or(false, |ms| ms.iter().any(|m| m.get("path").and_then(|p| p.as_str()) == Some("package.json")));
    let mut sorted = top_dirs.clone(); sorted.sort();
    json!({ "is_monorepo": is_monorepo, "layers": layers, "layer_count": layers.len(), "top_dirs": sorted })
}

pub fn inferPatterns(scan: &Value) -> Value {
    let empty: Vec<Value> = vec![];
    let sample = scan.get("sample_files").and_then(|v| v.as_array()).unwrap_or(&empty);
    let source: Vec<&Value> = sample.iter().filter(|f| f.get("kind").and_then(|v| v.as_str()) == Some("source")).collect();
    let tests: Vec<&Value> = sample.iter().filter(|f| f.get("kind").and_then(|v| v.as_str()) == Some("test")).collect();

    let mut ext_order: Vec<Value> = Vec::new();
    let mut ext_count: Vec<(Value, i64)> = Vec::new();
    for f in &source {
        let ext = f.get("ext").cloned().unwrap_or(Value::Null);
        if let Some(e) = ext_count.iter_mut().find(|(k, _)| *k == ext) { e.1 += 1; } else { ext_order.push(ext.clone()); ext_count.push((ext, 1)); }
    }
    let mut sorted_ext = ext_count.clone();
    sorted_ext.sort_by(|a, b| b.1.cmp(&a.1)); // stable desc
    let dominant_ext = sorted_ext.first().map(|(k, _)| k.clone()).unwrap_or(Value::Null);

    static CAMEL: OnceLock<Regex> = OnceLock::new();
    let camel_re = CAMEL.get_or_init(|| Regex::new(r"[a-z][A-Z]").unwrap());
    let (mut kebab, mut camel, mut snake) = (0i64, 0i64, 0i64);
    for f in &source {
        let base = basename(&str_qq(f.get("path")), &str_qq(f.get("ext")));
        if base.contains('-') { kebab += 1; }
        if camel_re.is_match(&base) { camel += 1; }
        if base.contains('_') { snake += 1; }
    }
    let naming = if kebab >= camel && kebab >= snake { "kebab-case" } else if camel >= snake { "camelCase" } else { "snake_case" };

    let collocated = tests.iter().any(|t| { let td = dirname(&str_qq(t.get("path"))); source.iter().any(|s| dirname(&str_qq(s.get("path"))) == td) });
    let patterns = json!([
        { "kind": "naming", "value": naming, "confidence": 0.8 },
        { "kind": "extension", "value": dominant_ext, "confidence": 0.9 },
        { "kind": "tests_collocated", "value": collocated, "confidence": 0.7 },
    ]);
    let mut anti: Vec<Value> = Vec::new();
    if tests.is_empty() { anti.push(json!({ "kind": "no_tests_found", "severity": "warning" })); }
    json!({ "patterns": patterns, "anti_patterns": anti })
}

pub fn inferDomain(scan: &Value) -> Value {
    let stop: [&str; 13] = ["the","and","for","with","that","this","from","into","have","will","about","should","must"];
    // NOTE: 'your','their' are in the JS STOP set too — keep the full 15
    let stop2: [&str; 2] = ["your", "their"];
    let is_stop = |w: &str| stop.contains(&w) || stop2.contains(&w);
    let empty: Vec<Value> = vec![];
    let mut order: Vec<String> = Vec::new();
    let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut bump = |w: String, by: i64, order: &mut Vec<String>, counts: &mut std::collections::HashMap<String, i64>| {
        if !counts.contains_key(&w) { order.push(w.clone()); }
        *counts.entry(w).or_insert(0) += by;
    };
    for d in scan.get("docs").and_then(|v| v.as_array()).unwrap_or(&empty) {
        for w in str_qq(d.get("title")).to_lowercase().split(|c: char| c.is_whitespace()) {
            if w.chars().count() > 4 && !is_stop(w) { bump(w.to_string(), 2, &mut order, &mut counts); }
        }
    }
    for t in scan.get("git_history").and_then(|g| g.get("themes")).and_then(|v| v.as_array()).unwrap_or(&empty) {
        let word = str_qq(t.get("word"));
        if word.chars().count() > 4 && !is_stop(&word) { bump(word, t.get("count").and_then(|v| v.as_i64()).unwrap_or(0), &mut order, &mut counts); }
    }
    let mut pairs: Vec<(String, i64)> = order.iter().map(|w| (w.clone(), counts[w])).collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1)); // stable desc
    let entities: Vec<Value> = pairs.iter().take(12).map(|(term, score)| json!({ "term": term, "score": score, "evidence": "doc + commit themes" })).collect();
    json!({ "entities": entities.clone(), "entity_count": entities.len() })
}

pub fn onboardingInfer(input: &Value, _db: &Connection) -> Result<Value, String> {
    let scan = match input.get("scan") { Some(s) if !s.is_null() => s, _ => return Err("onboarding-infer: 'scan' is required".into()) };
    Ok(json!({ "architecture": inferArchitecture(scan), "patterns": inferPatterns(scan), "domain": inferDomain(scan) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn infers() {
        let scan = json!({
            "sample_files": [
                { "path": "frontend/App.tsx", "kind": "source", "ext": ".tsx" },
                { "path": "backend/server.ts", "kind": "source", "ext": ".ts" },
                { "path": "backend/server.test.ts", "kind": "test", "ext": ".ts" },
                { "path": "my-thing.ts", "kind": "source", "ext": ".ts" },
            ],
            "stack": { "markers": [{ "path": "package.json" }] },
            "docs": [{ "title": "Payment processing service overview" }],
            "git_history": { "themes": [{ "word": "payment", "count": 7 }, { "word": "refund", "count": 3 }] },
        });
        let arch = inferArchitecture(&scan);
        assert_eq!(arch["is_monorepo"], json!(true));
        assert_eq!(arch["layer_count"], json!(2)); // frontend + backend
        assert_eq!(arch["top_dirs"], json!(["backend", "frontend"]));
        let pat = inferPatterns(&scan);
        assert_eq!(pat["patterns"][1]["value"], json!(".ts")); // dominant ext
        let dom = inferDomain(&scan);
        assert_eq!(dom["entities"][0]["term"], json!("payment")); // score 2+7=9, highest
        let full = onboardingInfer(&json!({ "scan": scan }), &Connection::open_in_memory().unwrap()).unwrap();
        assert!(full["architecture"].is_object() && full["domain"]["entity_count"].as_i64().unwrap() >= 1);
    }
}
