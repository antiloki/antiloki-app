//! code-patterns.rs — twin of skills/codebase/code-patterns.mjs (helper module, all no-db). NAMES ARE
//! CONTRACT: detectFilePatterns · mineConventions · codeIdentity. Deterministic pattern/convention mining.
// QUALITY: 82/100 — structural-fingerprint pattern match (evidence+confidence), dominant-case, code identity
use regex::Regex;
use serde_json::{json, Value};

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(), Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(), Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(), Some(o) => o.to_string(),
    }
}
// String(v || '')
fn str_truthy(v: Option<&Value>) -> String { if truthy(v) { tmpl(v) } else { String::new() } }
// short(n) = String(n||'').split('.').pop()
fn short(n: Option<&Value>) -> String { let s = str_truthy(n); s.rsplit('.').next().unwrap_or("").to_string() }
fn arr_includes(v: Option<&Value>, needle: &str) -> bool { v.and_then(|x| x.as_array()).map_or(false, |a| a.iter().any(|e| e.as_str() == Some(needle))) }
fn arr_len(v: Option<&Value>) -> usize { v.and_then(|x| x.as_array()).map_or(0, |a| a.len()) }
// !v?.length  →  true when absent/null/empty
fn empty_or_absent(v: Option<&Value>) -> bool { match v.and_then(|x| x.as_array()) { None => true, Some(a) => a.is_empty() } }
fn calls_hit(calls: Option<&Value>, res: &[&Regex]) -> bool {
    calls.and_then(|v| v.as_array()).map_or(false, |arr| arr.iter().any(|c| { let s = tmpl(Some(c)); res.iter().any(|re| re.is_match(&s)) }))
}
fn input_name(inputs: Option<&Value>, i: usize) -> String {
    inputs.and_then(|v| v.as_array()).and_then(|a| a.get(i)).and_then(|o| o.get("name")).and_then(|n| n.as_str()).unwrap_or("").to_string()
}
fn kind_is(b: &Value, k: &str) -> bool { b.get("kind").and_then(|v| v.as_str()) == Some(k) }
fn output_kind_is(b: &Value, k: &str) -> bool { b.get("output").and_then(|o| o.get("kind")).and_then(|v| v.as_str()) == Some(k) }

pub fn detectFilePatterns(blocks_v: &Value, entries_v: &Value) -> Value {
    let empty: Vec<Value> = vec![];
    let blocks = blocks_v.as_array().unwrap_or(&empty);
    let entries = entries_v.as_array().unwrap_or(&empty);
    let (re_provider, re_state, re_action) = (Regex::new(r"Provider$").unwrap(), Regex::new(r"(?i)^state").unwrap(), Regex::new(r"(?i)action").unwrap());
    let (re_factory, re_req, re_res, re_next) = (Regex::new(r"^(create|make|build)[A-Z]").unwrap(), Regex::new(r"(?i)req").unwrap(), Regex::new(r"(?i)res").unwrap(), Regex::new(r"(?i)next").unwrap());
    let repo_m = Regex::new(r"(?i)^(find|save|get|create|update|delete|insert|query|fetch|load|list|count|exists)").unwrap();
    let re_ctx = Regex::new(r"createContext").unwrap();
    let (re_obs1, re_obs2) = (Regex::new(r"(?i)\.(on|emit|subscribe|publish|addEventListener)$").unwrap(), Regex::new(r"(?i)^(on|emit|subscribe|dispatch)$").unwrap());

    let classes_n = blocks.iter().filter(|b| kind_is(b, "class")).count();
    let methods: Vec<&Value> = blocks.iter().filter(|b| kind_is(b, "method") || kind_is(b, "getter")).collect();
    let ctor = blocks.iter().any(|b| short(b.get("name")) == "constructor" && arr_len(b.get("inputs")) > 0);
    let has_routes = entries.iter().any(|e| e.get("kind").and_then(|v| v.as_str()) == Some("route"));
    let has_db = entries.iter().any(|e| e.get("kind").and_then(|v| v.as_str()) == Some("data"));

    let mut out: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut add = |pattern: &str, confidence: &str, evidence: &str| {
        if seen.insert(pattern.to_string()) { out.push(json!({ "pattern": pattern, "confidence": confidence, "evidence": evidence })); }
    };

    // React / FE
    if blocks.iter().any(|b| arr_includes(b.get("flags"), "memo") || arr_includes(b.get("flags"), "forwardRef")) { add("Higher-Order Component", "high", "memo / forwardRef wrapper"); }
    if blocks.iter().any(|b| kind_is(b, "hook")) { add("Custom Hook", "high", "use* hook returning state/handlers"); }
    if blocks.iter().any(|b| calls_hit(b.get("calls"), &[&re_ctx])) || blocks.iter().any(|b| re_provider.is_match(&short(b.get("name"))) && output_kind_is(b, "jsx")) { add("Provider / Context", "medium", "createContext / *Provider"); }
    if blocks.iter().any(|b| arr_len(b.get("inputs")) >= 2 && re_state.is_match(&input_name(b.get("inputs"), 0)) && re_action.is_match(&input_name(b.get("inputs"), 1))) { add("Reducer", "high", "(state, action) → state"); }

    // OO / backend
    if classes_n > 0 && has_routes { add("Controller (route handler)", "high", "class + HTTP route entry-points"); }
    if classes_n > 0 && ctor { add("Dependency Injection", "medium", "constructor-injected dependencies"); }
    if classes_n > 0 && methods.iter().any(|m| repo_m.is_match(&short(m.get("name")))) && (has_db || blocks.iter().any(|b| b.get("role").and_then(|v| v.as_str()) == Some("data") || arr_includes(b.get("effects"), "fetches"))) { add("Repository / DAO", "medium", "find/save/query methods + data access"); }
    if blocks.iter().any(|b| kind_is(b, "const") && output_kind_is(b, "instance") && arr_includes(b.get("flags"), "exported")) { add("Singleton", "medium", "exported module-level instance"); }
    if blocks.iter().any(|b| re_factory.is_match(&short(b.get("name"))) && (output_kind_is(b, "instance") || output_kind_is(b, "object"))) { add("Factory", "medium", "create*/make* returning an instance/object"); }
    if blocks.iter().any(|b| arr_includes(b.get("effects"), "dispatches")) || blocks.iter().any(|b| calls_hit(b.get("calls"), &[&re_obs1, &re_obs2])) { add("Observer / Event-driven", "medium", "emit / on / subscribe / dispatch"); }
    if blocks.iter().any(|b| arr_len(b.get("inputs")) >= 3 && re_req.is_match(&input_name(b.get("inputs"), 0)) && re_res.is_match(&input_name(b.get("inputs"), 1)) && re_next.is_match(&input_name(b.get("inputs"), 2))) { add("Middleware", "high", "(req, res, next) signature"); }
    if blocks.iter().any(|b| arr_includes(b.get("flags"), "exported")) && !blocks.is_empty() && blocks.iter().all(|b| kind_is(b, "const") && empty_or_absent(b.get("effects")) && empty_or_absent(b.get("calls"))) { add("Barrel / re-export", "low", "only re-exported constants"); }

    drop(add);
    json!(out)
}

fn dominant_case(names: &[String], case_order: &[(&'static str, Regex)]) -> Option<Value> {
    let mut tally: Vec<(&'static str, i64)> = Vec::new();
    for n in names {
        for (c, re) in case_order {
            if re.is_match(n) {
                if let Some(e) = tally.iter_mut().find(|(k, _)| k == c) { e.1 += 1; } else { tally.push((c, 1)); }
                break;
            }
        }
    }
    let mut sorted = tally.clone();
    sorted.sort_by(|a, b| b.1.cmp(&a.1)); // stable → ties keep insertion order
    if sorted.is_empty() { return None; }
    let total = if names.is_empty() { 1 } else { names.len() } as f64;
    let (convention, count) = sorted[0];
    let adherence = ((count as f64 / total) * 100.0).round() as i64;
    let mix: Vec<String> = sorted.iter().take(3).map(|(c, k)| format!("{} {}%", c, ((*k as f64 / total) * 100.0).round() as i64)).collect();
    Some(json!({ "convention": convention, "adherence": adherence, "n": total as i64, "mix": mix }))
}

pub fn mineConventions(all_blocks_v: &Value, all_paths_v: &Value) -> Value {
    let empty: Vec<Value> = vec![];
    let all_blocks = all_blocks_v.as_array().unwrap_or(&empty);
    let all_paths = all_paths_v.as_array().unwrap_or(&empty);
    let case_order: Vec<(&'static str, Regex)> = vec![
        ("UPPER_SNAKE", Regex::new(r"^[A-Z][A-Z0-9]*(_[A-Z0-9]+)+$|^[A-Z0-9]{2,}$").unwrap()),
        ("PascalCase", Regex::new(r"^[A-Z][a-zA-Z0-9]*$").unwrap()),
        ("camelCase", Regex::new(r"^[a-z][a-zA-Z0-9]*$").unwrap()),
        ("kebab-case", Regex::new(r"^[a-z0-9]+(-[a-z0-9]+)+$").unwrap()),
        ("snake_case", Regex::new(r"^[a-z0-9]+(_[a-z0-9]+)+$").unwrap()),
    ];

    let mut by_kind: Vec<(String, Vec<String>)> = Vec::new();
    for b in all_blocks {
        if str_truthy(b.get("name")).contains('.') { continue; }
        let s = short(b.get("name"));
        if s.is_empty() { continue; }
        let key = match b.get("kind") { Some(k) => tmpl(Some(k)), None => "undefined".into() };
        if let Some(e) = by_kind.iter_mut().find(|(k, _)| *k == key) { e.1.push(s); } else { by_kind.push((key, vec![s])); }
    }

    let mut naming: Vec<Value> = Vec::new();
    let file_names: Vec<String> = all_paths.iter().filter_map(|p| p.as_str()).map(|p| {
        let base = p.rsplit(|c| c == '/' || c == '\\').next().unwrap_or("");
        base.split('.').next().unwrap_or("").to_string()
    }).filter(|s| !s.is_empty()).collect();
    if let Some(fd) = dominant_case(&file_names, &case_order) {
        let mut o = serde_json::Map::new(); o.insert("subject".into(), json!("files"));
        if let Value::Object(m) = fd { for (k, v) in m { o.insert(k, v); } }
        naming.push(Value::Object(o));
    }
    for kind in ["component", "hook", "function", "class", "const", "interface", "type"] {
        if let Some((_, names)) = by_kind.iter().find(|(k, _)| k == kind) {
            if names.len() >= 4 {
                if let Some(d) = dominant_case(names, &case_order) {
                    let mut o = serde_json::Map::new(); o.insert("subject".into(), json!(kind));
                    if let Value::Object(m) = d { for (k, v) in m { o.insert(k, v); } }
                    naming.push(Value::Object(o));
                }
            }
        }
    }
    json!({ "naming": naming })
}

pub fn codeIdentity(languages: &Value, census: &Value, conventions: &Value, _role_tally: &Value) -> Value {
    let mut parts: Vec<String> = Vec::new();
    let num = |v: &Value| v.as_f64().unwrap_or(0.0);

    if let Some(langs_obj) = languages.as_object() {
        if !langs_obj.is_empty() {
            let mut entries: Vec<(&String, &Value)> = langs_obj.iter().collect();
            entries.sort_by(|a, b| num(b.1).partial_cmp(&num(a.1)).unwrap_or(std::cmp::Ordering::Equal));
            let langs: Vec<String> = entries.iter().take(3).map(|(k, _)| k.strip_prefix('.').unwrap_or(k).to_string()).collect();
            parts.push(format!("{} codebase", langs.join("/").to_uppercase()));
        }
    }

    let census_obj = census.as_object();
    let mut top: Vec<(&String, &Value)> = census_obj.map_or(vec![], |o| o.iter().collect());
    top.sort_by(|a, b| num(b.1).partial_cmp(&num(a.1)).unwrap_or(std::cmp::Ordering::Equal));
    let has_p = |p: &str| census_obj.and_then(|o| o.get(p)).map_or(false, |v| truthy(Some(v)));
    let mut flavor: Vec<&str> = Vec::new();
    if has_p("Custom Hook") || has_p("Higher-Order Component") || has_p("Provider / Context") { flavor.push("React (hooks + function components)"); }
    if has_p("Controller (route handler)") || has_p("Dependency Injection") { flavor.push("NestJS-style DI services + decorated controllers"); }
    if has_p("Repository / DAO") { flavor.push("Repository-pattern data access"); }
    if has_p("Observer / Event-driven") { flavor.push("event-driven"); }
    if !flavor.is_empty() { parts.push(format!("style: {}", flavor.join("; "))); }

    let naming_join = conventions.get("naming").and_then(|v| v.as_array()).map_or(String::new(), |arr| {
        arr.iter().take(5).map(|n| format!("{} {}", tmpl(n.get("subject")), tmpl(n.get("convention")))).collect::<Vec<_>>().join(", ")
    });
    if !naming_join.is_empty() { parts.push(format!("naming: {}", naming_join)); }
    if !top.is_empty() {
        let tp: Vec<String> = top.iter().take(4).map(|(p, n)| format!("{} ({})", p, tmpl(Some(n)))).collect();
        parts.push(format!("dominant patterns: {}", tp.join(", ")));
    }
    json!(parts.join(". ") + ".")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn patterns_conventions_identity() {
        let blocks = json!([{ "kind": "hook", "name": "useThing" }, { "kind": "function", "name": "x", "inputs": [{ "name": "state" }, { "name": "action" }] }]);
        let p = detectFilePatterns(&blocks, &json!([]));
        let pats: Vec<&str> = p.as_array().unwrap().iter().map(|x| x["pattern"].as_str().unwrap()).collect();
        assert!(pats.contains(&"Custom Hook") && pats.contains(&"Reducer"));

        let mc = mineConventions(&json!([{ "kind": "function", "name": "getX" }, { "kind": "function", "name": "getY" }, { "kind": "function", "name": "getZ" }, { "kind": "function", "name": "getW" }]), &json!(["src/foo-bar.js", "src/baz-qux.js"]));
        let naming = mc["naming"].as_array().unwrap();
        assert!(naming.iter().any(|n| n["subject"] == json!("files") && n["convention"] == json!("kebab-case")));
        assert!(naming.iter().any(|n| n["subject"] == json!("function") && n["convention"] == json!("camelCase")));

        let id = codeIdentity(&json!({ ".ts": 5, ".tsx": 2 }), &json!({ "Custom Hook": 3, "Repository / DAO": 1 }), &json!({ "naming": [{ "subject": "files", "convention": "kebab-case" }] }), &json!({}));
        let s = id.as_str().unwrap();
        assert!(s.starts_with("TS/TSX codebase"));
        assert!(s.contains("style: React (hooks + function components); Repository-pattern data access"));
        assert!(s.ends_with('.'));
    }
}
