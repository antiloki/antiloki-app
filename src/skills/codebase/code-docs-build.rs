//! code-docs-build.rs — twin of skills/codebase/code-docs-build.mjs (db READ lane). NAMES ARE
//! CONTRACT: `codeDocsBuild` (named default). Composes persisted decorators + descriptions +
//! cross-refs into a deterministic doc-site model (folder/file pages) and a Markdown renderer.
// QUALITY: 80/100 — deterministic weaver: role classify, MI score+explain, blast radius, page md.
use rusqlite::Connection;
use serde_json::{json, Map, Number, Value};
use std::collections::{HashMap, VecDeque};

// ── JS coercion helpers ──
fn js_float(f: f64) -> String {
    if f.is_nan() { return "NaN".into(); }
    if f == 0.0 { return "0".into(); }
    if f.is_infinite() { return if f > 0.0 { "Infinity".into() } else { "-Infinity".into() }; }
    format!("{}", f)
}
fn num_str(n: &Number) -> String {
    if let Some(i) = n.as_i64() { i.to_string() }
    else if let Some(u) = n.as_u64() { u.to_string() }
    else { js_float(n.as_f64().unwrap_or(f64::NAN)) }
}
// String(v)
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => num_str(n),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
// `${v}`
fn tmpl(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(x) => js_string(x) } }
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `??` right operand is used when left is null/undefined
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
// object key coercion: m[k]
fn obj_key(v: Option<&Value>) -> String { match v { None => "undefined".into(), Some(x) => js_string(x) } }
// `v || ''` interpolated
fn or_empty(v: Option<&Value>) -> String { if truthy(v) { js_string(v.unwrap()) } else { String::new() } }
// `v ?? fallback` interpolated
fn coalesce_str(v: Option<&Value>, fallback: &str) -> String { match nz(v) { Some(x) => js_string(x), None => fallback.into() } }

// JSON.parse(String(v)) with catch → default (J() in the source)
fn j(v: &Value, default: Value) -> Value {
    let s: String = match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => num_str(n),
        _ => return default,
    };
    serde_json::from_str::<Value>(&s).unwrap_or(default)
}

// ── path helpers ──
fn norm(p: &str) -> String { p.replace('\\', "/") }
fn dir_of(p: &str) -> String { let n = norm(p); match n.rfind('/') { Some(i) => n[..i].to_string(), None => String::new() } }
fn base_of(p: &str) -> String { norm(p).split('/').filter(|s| !s.is_empty()).last().unwrap_or("").to_string() }
fn tail_str(name: &str) -> String { name.rsplit('.').next().unwrap_or("").to_string() }
fn crumbs(p: &str) -> Value {
    let n = norm(p);
    let parts: Vec<&str> = n.split('/').filter(|s| !s.is_empty()).collect();
    let mut out: Vec<Value> = Vec::new();
    for (i, seg) in parts.iter().enumerate() { out.push(json!({ "seg": seg, "path": parts[..=i].join("/") })); }
    json!(out)
}

// ── tally maps (serde Map preserves insertion order like JS objects) ──
fn map_inc(m: &mut Map<String, Value>, key: String) {
    let e = m.entry(key).or_insert(Value::from(0i64));
    let cur = e.as_i64().unwrap_or(0);
    *e = Value::from(cur + 1);
}
fn map_get_i64(m: &Value, key: &str) -> i64 { m.get(key).and_then(|v| v.as_i64()).unwrap_or(0) }
// Object.entries(m).sort((a,b)=>b[1]-a[1])[0]?.[0]
fn top_key_of_map(m: &Value) -> Option<String> {
    let obj = m.as_object()?;
    let mut entries: Vec<(&String, i64)> = obj.iter().map(|(k, v)| (k, v.as_i64().unwrap_or_else(|| v.as_f64().map(|f| f as i64).unwrap_or(0)))).collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1));
    entries.first().map(|(k, _)| (*k).clone())
}

// ── didactic constants ──
fn role_label(role: &str) -> Option<&'static str> {
    match role {
        "ui" => Some("Presentation layer"), "data" => Some("Data / service layer"),
        "state" => Some("State management"), "logic" => Some("Domain logic"),
        "external" => Some("External bindings"), "type" => Some("Type definitions"), _ => None,
    }
}
fn role_label_or(top: Option<&str>, dflt: &'static str) -> String {
    top.and_then(role_label).map(|s| s.to_string()).unwrap_or_else(|| dflt.to_string())
}
fn group_of(kind: &Value) -> &'static str {
    match kind.as_str() {
        Some("component") => "Components", Some("hook") => "Hooks", Some("function") => "Functions",
        Some("class") | Some("method") | Some("getter") | Some("setter") => "Classes & methods",
        Some("interface") | Some("enum") | Some("type") => "Types", Some("const") => "Constants", _ => "Functions",
    }
}
fn entry_label(k: &str) -> Option<&'static str> {
    match k { "route" => Some("Routes"), "data" => Some("Data access"), "ui" => Some("UI surface"), _ => None }
}
fn entry_label_disp(k: &str) -> String { entry_label(k).map(|s| s.to_string()).unwrap_or_else(|| k.to_string()) }
fn entry_label_lower(k: &str) -> String { entry_label_disp(k).to_lowercase() }
fn is_type_val(k: &Value) -> bool { matches!(k.as_str(), Some("interface") | Some("type") | Some("enum")) }
fn arr_inc(v: Option<&Value>, needle: &str) -> bool {
    v.and_then(|x| x.as_array()).map_or(false, |a| a.iter().any(|e| e.as_str() == Some(needle)))
}
fn arr_len(v: Option<&Value>) -> usize { v.and_then(|x| x.as_array()).map_or(0, |a| a.len()) }

fn classify(by_role: &Value, by_kind: &Value, n_entries: usize) -> Value {
    let top = top_key_of_map(by_role);
    let type_count = map_get_i64(by_kind, "interface") + map_get_i64(by_kind, "type") + map_get_i64(by_kind, "enum");
    let counts = |k: &str| -> i64 { if k == "type" { type_count } else { map_get_i64(by_kind, k) } };
    let kp = [("component", "components"), ("hook", "hooks"), ("class", "classes"), ("function", "functions"), ("type", "types"), ("const", "constants")];
    let mut parts: Vec<String> = Vec::new();
    for (k, label) in kp { let c = counts(k); if c != 0 { parts.push(format!("{} {}", c, label)); } }
    parts.truncate(3);
    let mut s = role_label_or(top.as_deref(), "Module");
    if !parts.is_empty() { s.push_str(" \u{2014} "); s.push_str(&parts.join(", ")); }
    if n_entries != 0 { s.push_str(&format!("; exposes {} entry point{}", n_entries, if n_entries == 1 { "" } else { "s" })); }
    s.push('.');
    json!(s)
}
fn concept_of(d: &Value) -> &'static str {
    let kind = d.get("kind").unwrap_or(&Value::Null);
    if is_type_val(kind) { return "Types"; }
    let role = d.get("role");
    let role_is = |r: &str| role.and_then(|v| v.as_str()) == Some(r);
    let effects = d.get("effects");
    if role_is("ui") || kind.as_str() == Some("component") { return "Rendering"; }
    if role_is("state") || arr_inc(effects, "sets state") || arr_inc(effects, "dispatches") { return "State"; }
    if role_is("data") || arr_inc(effects, "fetches") || arr_inc(effects, "awaits") { return "Data / I/O"; }
    if arr_len(effects) > 0 { "Logic" } else { "Pure logic" }
}
fn folder_description(dir: &str, by_role: &Value, deps: &Value, used_by: &Value, entry_kinds: &Value) -> String {
    let top = top_key_of_map(by_role);
    let role = role_label_or(top.as_deref(), "A module").to_lowercase();
    let head = if !dir.is_empty() { format!("{}/", base_of(dir)) } else { "The codebase root".to_string() };
    let mut s = format!("{} is the {}.", head, role);
    if let Some(arr) = deps.as_array() { if !arr.is_empty() {
        let shown: Vec<String> = arr.iter().take(3).map(|d| { let b = base_of(&js_string(d)); if b.is_empty() { "root".into() } else { b } }).collect();
        let extra = if arr.len() > 3 { format!(" and {} more", arr.len() - 3) } else { String::new() };
        s.push_str(&format!(" It builds on {}{}.", shown.join(", "), extra));
    } }
    if let Some(arr) = used_by.as_array() { if !arr.is_empty() {
        let shown: Vec<String> = arr.iter().take(3).map(|d| { let b = base_of(&js_string(d)); if b.is_empty() { "root".into() } else { b } }).collect();
        let extra = if arr.len() > 3 { format!(" and {} more", arr.len() - 3) } else { String::new() };
        s.push_str(&format!(" It is used by {}{}.", shown.join(", "), extra));
    } }
    let surf: Vec<String> = match entry_kinds.as_object() {
        Some(o) => o.iter().map(|(k, n)| format!("{} {}", tmpl(Some(n)), entry_label_lower(k))).collect(),
        None => vec![],
    };
    if !surf.is_empty() { s.push_str(&format!(" Its external surface: {}.", surf.join(", "))); }
    s
}

// ── Maintainability Index ──
fn js_round(x: f64) -> f64 { if x.is_nan() { return f64::NAN; } let f = x.floor(); if x - f >= 0.5 { f + 1.0 } else { f } }
fn to_fixed(x: f64, n: usize) -> String { format!("{:.*}", n, x) }
fn maintain_index(loc: f64, avg_c: f64) -> f64 {
    let ac = if avg_c != 0.0 && !avg_c.is_nan() { avg_c } else { 0.0 };
    let l1 = if loc != 0.0 && !loc.is_nan() { loc } else { 1.0 };
    let expr = (171.0 - 0.23 * ac - 16.2 * l1.max(1.0).ln()) * 100.0 / 171.0;
    js_round(expr.min(100.0).max(0.0))
}
fn grade_of(mi: f64) -> &'static str {
    if mi >= 85.0 { "A" } else if mi >= 70.0 { "B" } else if mi >= 50.0 { "C" } else if mi >= 30.0 { "D" } else { "F" }
}
fn mi_bands() -> Value {
    json!([
        {"grade":"A","min":85,"label":"highly maintainable"},
        {"grade":"B","min":70,"label":"maintainable"},
        {"grade":"C","min":50,"label":"moderate"},
        {"grade":"D","min":30,"label":"hard to maintain"},
        {"grade":"F","min":0,"label":"very hard to maintain"},
    ])
}
fn mi_band_of(mi: f64) -> (i64, &'static str) {
    let bands = [(85i64, "highly maintainable"), (70, "maintainable"), (50, "moderate"), (30, "hard to maintain"), (0, "very hard to maintain")];
    for (min, label) in bands { if mi >= min as f64 { return (min, label); } }
    (0, "very hard to maintain")
}
fn explain_file_mi(loc: f64, avg_c: f64, mi: f64, grade: &str) -> Value {
    let l1 = if loc != 0.0 && !loc.is_nan() { loc } else { 1.0 };
    let ln_loc = l1.max(1.0).ln();
    let ac = if avg_c != 0.0 && !avg_c.is_nan() { avg_c } else { 0.0 };
    let c_pen = 0.23 * ac;
    let s_pen = 16.2 * ln_loc;
    let raw = (171.0 - c_pen - s_pen) * 100.0 / 171.0;
    let (band_min, band_label) = mi_band_of(mi);
    let avg_disp = js_float(ac);
    let loc_num = if loc != 0.0 && !loc.is_nan() { loc } else { 0.0 };
    let loc_disp = js_float(loc_num);
    json!({
        "metric": "Maintainability Index", "scope": "file",
        "criteria": "A standard maintainability score (SEI). Higher avg complexity and more lines both push it down; \u{2265}50 is the target.",
        "formula": "MI = (171 \u{2212} 0.23\u{00B7}avgComplexity \u{2212} 16.2\u{00B7}ln(LOC)) \u{00D7} 100 / 171, clamped to 0\u{2013}100",
        "inputs": { "avgComplexity": ac, "loc": loc_num as i64 },
        "steps": [
            format!("avg complexity = {} \u{00B7} LOC = {}", avg_disp, loc_disp),
            format!("complexity penalty: 0.23 \u{00D7} {} = {}", avg_disp, to_fixed(c_pen, 2)),
            format!("size penalty: 16.2 \u{00D7} ln({}) = 16.2 \u{00D7} {} = {}", loc_disp, to_fixed(ln_loc, 2), to_fixed(s_pen, 2)),
            format!("MI = (171 \u{2212} {} \u{2212} {}) \u{00D7} 100/171 = {} \u{2192} clamp \u{2192} {}", to_fixed(c_pen, 2), to_fixed(s_pen, 2), to_fixed(raw, 1), js_float(mi)),
        ],
        "bands": mi_bands(),
        "verdict": format!("{} \u{2265} {} \u{21D2} grade {} \u{00B7} {}", js_float(mi), band_min, grade, band_label),
    })
}
fn explain_folder_mi(fm: &[Value], fmi: f64, grade: &str) -> Value {
    let (band_min, band_label) = mi_band_of(fmi);
    let mut sorted: Vec<f64> = fm.iter().map(|m| m.get("mi").and_then(|v| v.as_f64()).unwrap_or(0.0)).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut fm_sorted: Vec<&Value> = fm.iter().collect();
    fm_sorted.sort_by(|a, b| {
        let am = a.get("mi").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let bm = b.get("mi").and_then(|v| v.as_f64()).unwrap_or(0.0);
        am.partial_cmp(&bm).unwrap_or(std::cmp::Ordering::Equal)
    });
    let worst: Vec<String> = fm_sorted.iter().take(3).map(|m| {
        let path = m.get("__path").filter(|v| truthy(Some(*v))).map(js_string).unwrap_or_default();
        format!("{} {}", base_of(&path), tmpl(m.get("mi")))
    }).filter(|s| !s.trim().is_empty()).collect();
    let mut steps: Vec<Value> = Vec::new();
    steps.push(json!(format!("{} file{} scored", fm.len(), if fm.len() == 1 { "" } else { "s" })));
    if !fm.is_empty() {
        steps.push(json!(format!("MI range {}\u{2013}{}, mean = {}", js_float(sorted[0]), js_float(sorted[sorted.len() - 1]), js_float(fmi))));
    } else {
        steps.push(json!("no files"));
    }
    if !worst.is_empty() { steps.push(json!(format!("lowest: {}", worst.join(" \u{00B7} ")))); }
    json!({
        "metric": "Maintainability Index", "scope": "folder",
        "criteria": "The folder score is the mean of its files\u{2019} maintainability \u{2014} so one unmaintainable file is diluted; open the files to find it.",
        "formula": "folder MI = round(mean(file MI for every file inside))",
        "inputs": { "files": fm.len() as i64 },
        "steps": steps,
        "bands": mi_bands(),
        "verdict": format!("{} \u{2265} {} \u{21D2} grade {} \u{00B7} {}", js_float(fmi), band_min, grade, band_label),
    })
}

// ── decorator loading ──
struct StoryS { name: Value, description: Value, direction: Value, hops: Vec<Value>, paths: Vec<String> }

fn parse_hops(v: &Value) -> Vec<Value> {
    let s = if truthy(Some(v)) { js_string(v) } else { "[]".to_string() };
    match serde_json::from_str::<Value>(&s) { Ok(Value::Array(a)) => a, _ => vec![] }
}
fn stories_for<F: Fn(&str) -> bool>(stories: &[StoryS], pred: F) -> Value {
    let filtered: Vec<&StoryS> = stories.iter().filter(|s| s.paths.iter().any(|p| pred(p))).take(8).collect();
    json!(filtered.iter().map(|s| json!({
        "name": s.name.clone(), "description": s.description.clone(), "direction": s.direction.clone(),
        "hops": s.hops.iter().take(12).cloned().collect::<Vec<_>>()
    })).collect::<Vec<_>>())
}

fn link(sym: &Value, by_name: &HashMap<String, Value>, by_tail: &HashMap<String, Value>) -> Value {
    let sym_s = js_string(sym);
    let t = tail_str(&sym_s);
    let r = by_name.get(&sym_s).or_else(|| by_name.get(&t)).or_else(|| by_tail.get(&t));
    let target = match r {
        Some(rv) => json!(format!("{}#{}", js_string(rv.get("path").unwrap_or(&Value::Null)), js_string(rv.get("anchor").unwrap_or(&Value::Null)))),
        None => Value::Null,
    };
    json!({ "name": sym.clone(), "target": target })
}

fn strict_eq(x: &Value, y: &Value) -> bool {
    match (x, y) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        _ => false,
    }
}

// free helpers over the file/folder sets
fn child_files_of(dir: &str, by_path_order: &[String]) -> Vec<String> {
    by_path_order.iter().filter(|p| dir_of(p) == dir).cloned().collect()
}
fn subdirs_of(dir: &str, folders: &[String]) -> Vec<String> {
    folders.iter().filter(|f| !f.is_empty() && dir_of(f) == dir).cloned().collect()
}
fn descendants_of(dir: &str, by_path_order: &[String]) -> Vec<String> {
    by_path_order.iter().filter(|p| dir.is_empty() || p.as_str() == dir || p.starts_with(&format!("{}/", dir))).cloned().collect()
}
fn in_deg(f: &str, file_deps: &[(String, Vec<String>)]) -> i64 {
    let mut n = 0i64;
    for (_, ts) in file_deps { if ts.iter().any(|x| x == f) { n += 1; } }
    n
}
fn build_tree(dir: &str, folders: &[String], by_path_order: &[String]) -> Value {
    let mut subs = subdirs_of(dir, folders); subs.sort();
    let mut cf = child_files_of(dir, by_path_order); cf.sort();
    let mut children: Vec<Value> = Vec::new();
    for s in &subs { children.push(build_tree(s, folders, by_path_order)); }
    for f in &cf { children.push(json!({ "type": "file", "name": base_of(f), "path": f })); }
    let name = { let b = base_of(dir); if b.is_empty() { "/".to_string() } else { b } };
    json!({ "type": "folder", "name": name, "path": dir, "children": children })
}

pub fn codeDocsBuild(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").filter(|v| !v.is_null()).cloned().unwrap_or(json!("atlas"));

    let rows = match crate::db_json::query_rows(db, "SELECT path,name,kind,role,line,end_line,signature,summary,inputs_json,output_json,effects_json,calls_json,uses_json,complexity,flags_json,steps_json FROM code_decorators WHERE origin = ? ORDER BY path, line", &[origin.clone()]) {
        Ok(r) => r.as_array().cloned().unwrap_or_default(),
        Err(_) => return Ok(json!({ "ok": true, "found": false, "origin": origin, "error": "code_decorators not present \u{2014} restart the BE, then rebuild", "tree": Value::Null, "pages": {}, "index": [] })),
    };
    let decs: Vec<Value> = rows.iter().map(|r| json!({
        "path": r.get("path").cloned().unwrap_or(Value::Null),
        "name": r.get("name").cloned().unwrap_or(Value::Null),
        "kind": r.get("kind").cloned().unwrap_or(Value::Null),
        "role": r.get("role").cloned().unwrap_or(Value::Null),
        "line": r.get("line").cloned().unwrap_or(Value::Null),
        "end_line": r.get("end_line").cloned().unwrap_or(Value::Null),
        "signature": r.get("signature").cloned().unwrap_or(Value::Null),
        "summary": r.get("summary").cloned().unwrap_or(Value::Null),
        "inputs": j(r.get("inputs_json").unwrap_or(&Value::Null), json!([])),
        "output": j(r.get("output_json").unwrap_or(&Value::Null), Value::Null),
        "effects": j(r.get("effects_json").unwrap_or(&Value::Null), json!([])),
        "calls": j(r.get("calls_json").unwrap_or(&Value::Null), json!([])),
        "uses": j(r.get("uses_json").unwrap_or(&Value::Null), json!([])),
        "complexity": r.get("complexity").cloned().unwrap_or(Value::Null),
        "flags": j(r.get("flags_json").unwrap_or(&Value::Null), json!([])),
        "steps": j(r.get("steps_json").unwrap_or(&Value::Null), json!([])),
    })).collect();

    if decs.is_empty() {
        return Ok(json!({ "ok": true, "found": false, "origin": origin, "tree": { "type": "folder", "name": "/", "path": "", "children": [] }, "pages": {}, "index": [], "totals": { "files": 0, "folders": 0, "blocks": 0 } }));
    }

    // essays
    let mut essay_by_path: HashMap<String, Value> = HashMap::new();
    if let Ok(arr) = crate::db_json::query_rows(db, "SELECT path, summary_md FROM code_descriptions WHERE origin = ?", &[origin.clone()]) {
        if let Some(rows) = arr.as_array() {
            for d in rows {
                let smd = d.get("summary_md").unwrap_or(&Value::Null);
                if truthy(Some(smd)) { essay_by_path.insert(js_string(d.get("path").unwrap_or(&Value::Null)), smd.clone()); }
            }
        }
    }
    // entry-points
    let mut ent_by_path: HashMap<String, Vec<Value>> = HashMap::new();
    if let Ok(arr) = crate::db_json::query_rows(db, "SELECT path, kind, subtype, label, http_method, framework FROM code_entry_points WHERE origin = ?", &[origin.clone()]) {
        if let Some(rows) = arr.as_array() {
            for e in rows {
                let path = js_string(e.get("path").unwrap_or(&Value::Null));
                ent_by_path.entry(path).or_default().push(json!({
                    "kind": e.get("kind").cloned().unwrap_or(Value::Null),
                    "subtype": e.get("subtype").cloned().unwrap_or(Value::Null),
                    "label": e.get("label").cloned().unwrap_or(Value::Null),
                    "http_method": e.get("http_method").cloned().unwrap_or(Value::Null),
                    "framework": e.get("framework").cloned().unwrap_or(Value::Null),
                }));
            }
        }
    }
    // data-stories
    let mut stories: Vec<StoryS> = Vec::new();
    if let Ok(arr) = crate::db_json::query_rows(db, "SELECT name, description, direction, hops_json FROM code_data_stories WHERE origin = ?", &[origin.clone()]) {
        if let Some(rows) = arr.as_array() {
            for s in rows {
                let parsed = parse_hops(s.get("hops_json").unwrap_or(&Value::Null));
                let hops_labels: Vec<Value> = parsed.iter().filter_map(|h| {
                    let chosen = if truthy(h.get("label")) { h.get("label") } else { h.get("node_id") };
                    match chosen { Some(v) if truthy(Some(v)) => Some(v.clone()), _ => None }
                }).collect();
                let mut paths: Vec<String> = Vec::new();
                for h in &parsed {
                    let v = if truthy(h.get("node_id")) { h.get("node_id") } else if truthy(h.get("path")) { h.get("path") } else { None };
                    let raw = match v { Some(x) => js_string(x), None => String::new() };
                    let base = raw.split(|c| c == '@' || c == '#').next().unwrap_or("").to_string();
                    if !base.is_empty() && !paths.contains(&base) { paths.push(base); }
                }
                stories.push(StoryS {
                    name: s.get("name").cloned().unwrap_or(Value::Null),
                    description: s.get("description").cloned().unwrap_or(Value::Null),
                    direction: s.get("direction").cloned().unwrap_or(Value::Null),
                    hops: hops_labels, paths,
                });
            }
        }
    }
    // digest + insights
    let mut digest: Value = Value::Null;
    if let Ok(arr) = crate::db_json::query_rows(db, "SELECT * FROM code_digests WHERE origin = ? ORDER BY id DESC LIMIT 1", &[origin.clone()]) {
        if let Some(d) = arr.as_array().and_then(|a| a.first()) {
            digest = json!({
                "files": d.get("total_files_num").cloned().unwrap_or(Value::Null),
                "symbols": d.get("total_symbols_num").cloned().unwrap_or(Value::Null),
                "edges": d.get("total_edges_num").cloned().unwrap_or(Value::Null),
                "loc": d.get("total_loc_num").cloned().unwrap_or(Value::Null),
                "languages": j(d.get("languages_json").unwrap_or(&Value::Null), Value::Null),
                "health": d.get("health_score_num").cloned().unwrap_or(Value::Null),
                "risk": d.get("risk_score_num").cloned().unwrap_or(Value::Null),
                "coverage_pct": d.get("coverage_pct").cloned().unwrap_or(Value::Null),
                "cycles": d.get("cycles_num").cloned().unwrap_or(Value::Null),
                "max_depth": d.get("max_depth_num").cloned().unwrap_or(Value::Null),
                "avg_fan_in": d.get("avg_fan_in_num").cloned().unwrap_or(Value::Null),
                "avg_fan_out": d.get("avg_fan_out_num").cloned().unwrap_or(Value::Null),
                "hubs": j(d.get("hubs_json").unwrap_or(&Value::Null), json!([])),
                "top_complex": j(d.get("top_complex_json").unwrap_or(&Value::Null), json!([])),
                "top_risky": j(d.get("top_risky_json").unwrap_or(&Value::Null), json!([])),
                "top_central": j(d.get("top_central_json").unwrap_or(&Value::Null), json!([])),
                "largest_files": j(d.get("largest_files_json").unwrap_or(&Value::Null), json!([])),
                "top_churn": j(d.get("top_churn_json").unwrap_or(&Value::Null), json!([])),
                "orphans": j(d.get("orphans_json").unwrap_or(&Value::Null), json!([])),
                "tested_files": d.get("tested_files_num").cloned().unwrap_or(Value::Null),
                "untested_files": d.get("untested_files_num").cloned().unwrap_or(Value::Null),
                "test_files": d.get("test_files_num").cloned().unwrap_or(Value::Null),
                "smells_total": d.get("smells_total_num").cloned().unwrap_or(Value::Null),
                "smells_by_kind": j(d.get("smells_by_kind_json").unwrap_or(&Value::Null), json!({})),
                "narrative": d.get("narrative_md").cloned().unwrap_or(Value::Null),
            });
        }
    }
    let mut insights: Value = Value::Null;
    if let Ok(arr) = crate::db_json::query_rows(db, "SELECT * FROM code_reports WHERE origin = ? ORDER BY id DESC LIMIT 1", &[origin.clone()]) {
        if let Some(r) = arr.as_array().and_then(|a| a.first()) {
            insights = json!({
                "health": r.get("health_score").cloned().unwrap_or(Value::Null),
                "risk": r.get("risk_score").cloned().unwrap_or(Value::Null),
                "smells": j(r.get("smells_json").unwrap_or(&Value::Null), json!([])),
                "vulns": j(r.get("vulns_json").unwrap_or(&Value::Null), json!([])),
                "review": r.get("review_md").cloned().unwrap_or(Value::Null),
            });
        }
    }

    // symbol index + callers
    let mut by_name: HashMap<String, Value> = HashMap::new();
    let mut by_tail: HashMap<String, Value> = HashMap::new();
    for d in &decs {
        let name_v = d.get("name").unwrap_or(&Value::Null);
        let name = js_string(name_v);
        let refv = json!({ "path": d.get("path").cloned().unwrap_or(Value::Null), "anchor": name_v.clone(), "kind": d.get("kind").cloned().unwrap_or(Value::Null) });
        by_name.entry(name.clone()).or_insert_with(|| refv.clone());
        by_tail.entry(tail_str(&name)).or_insert(refv);
    }
    let mut callers: HashMap<String, Vec<Value>> = HashMap::new();
    for d in &decs {
        if let Some(calls) = d.get("calls").and_then(|v| v.as_array()) {
            for c in calls {
                callers.entry(tail_str(&js_string(c))).or_default().push(json!({ "name": d.get("name").cloned().unwrap_or(Value::Null), "path": d.get("path").cloned().unwrap_or(Value::Null) }));
            }
        }
    }

    // group decs by path, preserving first-seen order
    let mut by_path_order: Vec<String> = Vec::new();
    let mut by_path: HashMap<String, Vec<Value>> = HashMap::new();
    for d in &decs {
        let path = js_string(d.get("path").unwrap_or(&Value::Null));
        if !by_path.contains_key(&path) { by_path_order.push(path.clone()); }
        by_path.entry(path).or_default().push(d.clone());
    }

    let mut pages: Map<String, Value> = Map::new();
    let mut file_deps: Vec<(String, Vec<String>)> = Vec::new();

    // ── per-file pages ──
    for path in &by_path_order {
        let blocks = &by_path[path];
        let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
        let mut out: Vec<Value> = Vec::new();
        for d in blocks {
            // uses = new Set([...uses, ...calls]) mapped to links, external only
            let mut merged: Vec<&Value> = Vec::new();
            if let Some(u) = d.get("uses").and_then(|v| v.as_array()) { for x in u { merged.push(x); } }
            if let Some(c) = d.get("calls").and_then(|v| v.as_array()) { for x in c { merged.push(x); } }
            let mut seen_syms: Vec<String> = Vec::new();
            let mut uses_arr: Vec<Value> = Vec::new();
            for sym in merged {
                let key = js_string(sym);
                if seen_syms.contains(&key) { continue; }
                seen_syms.push(key);
                let l = link(sym, &by_name, &by_tail);
                let target = l.get("target").unwrap_or(&Value::Null);
                if truthy(Some(target)) {
                    let tp = js_string(target);
                    let tp0 = tp.split('#').next().unwrap_or("");
                    if tp0 != path.as_str() { uses_arr.push(l); }
                }
            }
            // usedBy
            let name_v = d.get("name").unwrap_or(&Value::Null);
            let name_s = js_string(name_v);
            let empty_vec: Vec<Value> = vec![];
            let callers_list = callers.get(&tail_str(&name_s)).unwrap_or(&empty_vec);
            let used_by_raw: Vec<&Value> = callers_list.iter().filter(|c| {
                let cpath = js_string(c.get("path").unwrap_or(&Value::Null));
                let cname = js_string(c.get("name").unwrap_or(&Value::Null));
                !(cpath == *path && cname == name_s)
            }).collect();
            // group tally
            let g = group_of(d.get("kind").unwrap_or(&Value::Null)).to_string();
            let item = json!({ "name": name_v.clone(), "kind": d.get("kind").cloned().unwrap_or(Value::Null) });
            if let Some(entry) = groups.iter_mut().find(|(k, _)| *k == g) { entry.1.push(item); } else { groups.push((g, vec![item])); }
            // out block = {...d, uses, usedBy, used_by_count}
            let mut ob = d.as_object().unwrap().clone();
            ob.insert("uses".into(), json!(uses_arr));
            let used_by: Vec<Value> = used_by_raw.iter().take(12).map(|c| json!({ "name": c.get("name").cloned().unwrap_or(Value::Null), "target": format!("{}#{}", js_string(c.get("path").unwrap_or(&Value::Null)), js_string(c.get("name").unwrap_or(&Value::Null))) })).collect();
            ob.insert("usedBy".into(), json!(used_by));
            ob.insert("used_by_count".into(), json!(used_by_raw.len() as i64));
            out.push(Value::Object(ob));
        }

        let group_order = ["Components", "Hooks", "Functions", "Classes & methods", "Types", "Constants"];
        let toc: Vec<Value> = group_order.iter().filter_map(|g| groups.iter().find(|(k, _)| k.as_str() == *g).map(|(_, items)| json!({ "group": g, "items": items }))).collect();

        let mut by_kind: Map<String, Value> = Map::new();
        let mut by_role: Map<String, Value> = Map::new();
        for d in &out { map_inc(&mut by_kind, obj_key(d.get("kind"))); map_inc(&mut by_role, obj_key(d.get("role"))); }
        let exported: Vec<Value> = out.iter().filter(|d| arr_inc(d.get("flags"), "exported")).map(|d| d.get("name").cloned().unwrap_or(Value::Null)).collect();
        let mut concepts: Map<String, Value> = Map::new();
        for d in &out { map_inc(&mut concepts, concept_of(d).to_string()); }
        let data_structures: Vec<Value> = out.iter().filter(|d| is_type_val(d.get("kind").unwrap_or(&Value::Null))).map(|d| {
            let shape = { let lbl = d.get("output").and_then(|o| o.get("label")); if truthy(lbl) { js_string(lbl.unwrap()) } else { String::new() } };
            json!({ "name": d.get("name").cloned().unwrap_or(Value::Null), "kind": d.get("kind").cloned().unwrap_or(Value::Null), "shape": shape, "target": format!("{}#{}", path, js_string(d.get("name").unwrap_or(&Value::Null))) })
        }).collect();
        let entries: Vec<Value> = ent_by_path.get(path).cloned().unwrap_or_default();

        // deps + reverse-deps sets (insertion-ordered)
        let mut tset: Vec<String> = Vec::new();
        for b in &out {
            if let Some(u) = b.get("uses").and_then(|v| v.as_array()) {
                for uu in u { let tp = js_string(uu.get("target").unwrap_or(&Value::Null)); let p0 = tp.split('#').next().unwrap_or("").to_string(); if !tset.contains(&p0) { tset.push(p0); } }
            }
        }
        file_deps.push((path.clone(), tset.clone()));
        let mut ub_files: Vec<String> = Vec::new();
        for b in &out {
            if let Some(ub) = b.get("usedBy").and_then(|v| v.as_array()) {
                for uu in ub { let tp = js_string(uu.get("target").unwrap_or(&Value::Null)); let p0 = tp.split('#').next().unwrap_or("").to_string(); if !ub_files.contains(&p0) { ub_files.push(p0); } }
            }
        }
        let fan_out = tset.iter().filter(|p| p.as_str() != path.as_str()).count() as i64;
        let fan_in = ub_files.iter().filter(|p| p.as_str() != path.as_str()).count() as i64;

        let mut loc = 0f64;
        for d in &out {
            let el = d.get("end_line"); let ln = d.get("line");
            let v = if truthy(el) { el.unwrap().as_f64().unwrap_or(0.0) } else if truthy(ln) { ln.unwrap().as_f64().unwrap_or(0.0) } else { 0.0 };
            if v > loc { loc = v; }
        }
        let comps: Vec<f64> = out.iter().map(|d| { let c = d.get("complexity"); if truthy(c) { c.unwrap().as_f64().unwrap_or(1.0) } else { 1.0 } }).collect();
        let avg_c = if !comps.is_empty() { js_round(comps.iter().sum::<f64>() / comps.len() as f64 * 10.0) / 10.0 } else { 0.0 };
        let fmi = maintain_index(loc, avg_c);
        let max_c = if !comps.is_empty() { comps.iter().cloned().fold(f64::NEG_INFINITY, f64::max) } else { 0.0 };
        let grade = grade_of(fmi);
        let metrics = json!({
            "loc": loc as i64, "blocks": out.len() as i64, "avgComplexity": avg_c, "maxComplexity": max_c as i64,
            "exported": exported.len() as i64, "fanIn": fan_in, "fanOut": fan_out, "mi": fmi as i64,
            "grade": grade, "scoreExplain": explain_file_mi(loc, avg_c, fmi, grade)
        });

        let fallback = {
            let n = out.len();
            let mut s = format!("Module {} \u{2014} {} block{}", base_of(path), n, if n == 1 { "" } else { "s" });
            if !exported.is_empty() { s.push_str(&format!(", exports {}", exported.iter().take(8).map(js_string).collect::<Vec<_>>().join(", "))); }
            s.push('.');
            s
        };
        let essay = essay_by_path.get(path).cloned().unwrap_or(Value::Null);
        let entries_val = json!(entries.clone());
        let patterns = crate::codebase_code_patterns::detectFilePatterns(&json!(out.clone()), &entries_val);
        let stories_v = stories_for(&stories, |p| p == path.as_str());
        let depends_on: Vec<Value> = tset.iter().filter(|p| p.as_str() != path.as_str()).take(20).map(|p| json!(p)).collect();
        let used_by_files: Vec<Value> = ub_files.iter().filter(|p| p.as_str() != path.as_str()).take(20).map(|p| json!(p)).collect();
        let intro = classify(&Value::Object(by_role.clone()), &Value::Object(by_kind.clone()), entries.len());
        let stats = json!({ "blocks": out.len() as i64, "byKind": Value::Object(by_kind), "byRole": Value::Object(by_role), "exported": exported.len() as i64 });

        let page = json!({
            "kind": "file", "path": path, "breadcrumb": crumbs(path), "intro": intro,
            "narrative": { "essay": essay, "fallback": fallback },
            "toc": toc, "blocks": out, "concepts": Value::Object(concepts), "dataStructures": data_structures,
            "entryPoints": entries, "dependsOn": depends_on, "usedBy": used_by_files,
            "metrics": metrics, "patterns": patterns, "stories": stories_v, "stats": stats,
        });
        pages.insert(path.clone(), page);
    }

    // ── blast radius (transitive reverse-dependents) ──
    let mut reverse: Vec<(String, Vec<String>)> = Vec::new();
    for (fp, deps) in &file_deps {
        for d in deps {
            if let Some(entry) = reverse.iter_mut().find(|(k, _)| k == d) { if !entry.1.contains(fp) { entry.1.push(fp.clone()); } }
            else { reverse.push((d.clone(), vec![fp.clone()])); }
        }
    }
    let reverse_get = |k: &str| -> Vec<String> { reverse.iter().find(|(kk, _)| kk.as_str() == k).map(|(_, v)| v.clone()).unwrap_or_default() };
    for fp in &by_path_order {
        let mut seen: Vec<String> = Vec::new();
        let mut q: VecDeque<String> = reverse_get(fp).into_iter().collect();
        while !q.is_empty() && seen.len() < 300 {
            let x = q.pop_front().unwrap();
            if x == *fp || seen.contains(&x) { continue; }
            seen.push(x.clone());
            for u in reverse_get(&x) { if !seen.contains(&u) { q.push_back(u); } }
        }
        if let Some(p) = pages.get_mut(fp) {
            let files: Vec<Value> = seen.iter().take(40).map(|s| json!(s)).collect();
            p.as_object_mut().unwrap().insert("blastRadius".into(), json!({ "count": seen.len() as i64, "files": files }));
        }
    }

    // ── folders ──
    let mut folders: Vec<String> = vec![String::new()];
    for p in &by_path_order {
        let mut d = dir_of(p);
        loop {
            if !folders.contains(&d) { folders.push(d.clone()); }
            if d.is_empty() { break; }
            d = dir_of(&d);
        }
    }

    for dir in &folders {
        let files = descendants_of(dir, &by_path_order);
        let mut fblocks: Vec<Value> = Vec::new();
        for f in &files { if let Some(bs) = by_path.get(f) { for b in bs { fblocks.push(b.clone()); } } }
        let mut by_kind: Map<String, Value> = Map::new();
        let mut by_role: Map<String, Value> = Map::new();
        for d in &fblocks { map_inc(&mut by_kind, obj_key(d.get("kind"))); map_inc(&mut by_role, obj_key(d.get("role"))); }
        let surface: Vec<Value> = fblocks.iter().filter(|d| arr_inc(d.get("flags"), "exported")).take(60).map(|d| json!({
            "name": d.get("name").cloned().unwrap_or(Value::Null), "kind": d.get("kind").cloned().unwrap_or(Value::Null),
            "summary": d.get("summary").cloned().unwrap_or(Value::Null),
            "target": format!("{}#{}", js_string(d.get("path").unwrap_or(&Value::Null)), js_string(d.get("name").unwrap_or(&Value::Null)))
        })).collect();
        // deps
        let mut deps: Vec<String> = Vec::new();
        for d in &fblocks {
            let mut merged: Vec<&Value> = Vec::new();
            if let Some(u) = d.get("uses").and_then(|v| v.as_array()) { for x in u { merged.push(x); } }
            if let Some(c) = d.get("calls").and_then(|v| v.as_array()) { for x in c { merged.push(x); } }
            for u in merged {
                let l = link(u, &by_name, &by_tail);
                let target = l.get("target").unwrap_or(&Value::Null);
                if !truthy(Some(target)) { continue; }
                let tp = js_string(target); let tp0 = tp.split('#').next().unwrap_or("");
                let td = dir_of(tp0);
                let cond1 = td != *dir;
                let cond2 = !(!dir.is_empty() && td.starts_with(&format!("{}/", dir)));
                let cond3 = !(!td.is_empty() && dir.starts_with(&format!("{}/", td)));
                if cond1 && cond2 && cond3 {
                    let key = if td.is_empty() { "(root)".to_string() } else { td.clone() };
                    if !deps.contains(&key) { deps.push(key); }
                }
            }
        }
        // entries
        let mut f_entries: Vec<Value> = Vec::new();
        for f in &files { if let Some(es) = ent_by_path.get(f) { for e in es { f_entries.push(e.clone()); } } }
        let mut entry_kinds: Map<String, Value> = Map::new();
        for e in &f_entries { map_inc(&mut entry_kinds, obj_key(e.get("kind"))); }
        let data_structures: Vec<Value> = fblocks.iter().filter(|d| is_type_val(d.get("kind").unwrap_or(&Value::Null))).take(40).map(|d| {
            let shape = { let lbl = d.get("output").and_then(|o| o.get("label")); if truthy(lbl) { js_string(lbl.unwrap()) } else { String::new() } };
            json!({ "name": d.get("name").cloned().unwrap_or(Value::Null), "kind": d.get("kind").cloned().unwrap_or(Value::Null), "shape": shape, "target": format!("{}#{}", js_string(d.get("path").unwrap_or(&Value::Null)), js_string(d.get("name").unwrap_or(&Value::Null))) })
        }).collect();
        // reading order
        let mut cf = child_files_of(dir, &by_path_order);
        cf.sort_by(|a, b| in_deg(b, &file_deps).cmp(&in_deg(a, &file_deps)));
        let reading_order: Vec<Value> = cf.iter().take(30).map(|f| json!({ "path": f, "name": base_of(f), "depended_by": in_deg(f, &file_deps) })).collect();
        // concepts + patternCensus
        let mut concepts: Map<String, Value> = Map::new();
        for d in &fblocks { map_inc(&mut concepts, concept_of(d).to_string()); }
        let mut pattern_census: Map<String, Value> = Map::new();
        for f in &files {
            if let Some(fp) = pages.get(f) {
                if let Some(ps) = fp.get("patterns").and_then(|v| v.as_array()) {
                    for p in ps { map_inc(&mut pattern_census, js_string(p.get("pattern").unwrap_or(&Value::Null))); }
                }
            }
        }
        // folder metrics from file metrics
        let mut fm: Vec<Value> = Vec::new();
        for f in &files {
            if let Some(fp) = pages.get(f) {
                if let Some(m) = fp.get("metrics") { let mut mm = m.as_object().unwrap().clone(); mm.insert("__path".into(), json!(f)); fm.push(Value::Object(mm)); }
            }
        }
        let fmi = if !fm.is_empty() { js_round(fm.iter().map(|m| m.get("mi").and_then(|v| v.as_f64()).unwrap_or(0.0)).sum::<f64>() / fm.len() as f64) } else { 0.0 };
        let g = grade_of(fmi);
        let loc_sum: f64 = fm.iter().map(|m| { let l = m.get("loc"); if truthy(l) { l.unwrap().as_f64().unwrap_or(0.0) } else { 0.0 } }).sum();
        let avg = if !fm.is_empty() { js_round(fm.iter().map(|m| m.get("avgComplexity").and_then(|v| v.as_f64()).unwrap_or(0.0)).sum::<f64>() / fm.len() as f64 * 10.0) / 10.0 } else { 0.0 };
        let maxc = fm.iter().map(|m| m.get("maxComplexity").and_then(|v| v.as_f64()).unwrap_or(0.0)).fold(0.0f64, f64::max);
        let fmetrics = json!({
            "files": files.len() as i64, "loc": loc_sum as i64, "blocks": fblocks.len() as i64,
            "avgComplexity": avg, "maxComplexity": maxc as i64, "exported": surface.len() as i64,
            "mi": fmi as i64, "grade": g, "scoreExplain": explain_folder_mi(&fm, fmi, g)
        });
        let intro = classify(&Value::Object(by_role.clone()), &Value::Object(by_kind.clone()), f_entries.len());
        let dir_owned = dir.clone();
        let stories_v = stories_for(&stories, |p| dir_owned.is_empty() || p == dir_owned.as_str() || p.starts_with(&format!("{}/", dir_owned)));
        let mut sub = subdirs_of(dir, &folders); sub.sort();
        let subfolders: Vec<Value> = sub.iter().map(|s| json!({ "path": s, "name": base_of(s), "files": descendants_of(s, &by_path_order).len() as i64 })).collect();
        let mut cff = child_files_of(dir, &by_path_order); cff.sort();
        let files_list: Vec<Value> = cff.iter().map(|f| json!({ "path": f, "name": base_of(f), "blocks": by_path.get(f).map(|b| b.len()).unwrap_or(0) as i64 })).collect();
        let deps_out: Vec<Value> = deps.iter().take(20).map(|d| json!(d)).collect();
        let stats = json!({ "files": files.len() as i64, "blocks": fblocks.len() as i64, "byKind": Value::Object(by_kind), "byRole": Value::Object(by_role) });
        let f_entries_out: Vec<Value> = f_entries.iter().take(60).cloned().collect();

        let page = json!({
            "kind": "folder", "path": dir, "breadcrumb": crumbs(dir), "intro": intro,
            "concepts": Value::Object(concepts), "patternCensus": Value::Object(pattern_census), "metrics": fmetrics,
            "stats": stats, "publicSurface": surface, "dependsOn": deps_out,
            "entryPoints": f_entries_out, "entryKinds": Value::Object(entry_kinds), "readingOrder": reading_order, "dataStructures": data_structures,
            "stories": stories_v, "subfolders": subfolders, "files": files_list,
        });
        pages.insert(format!("dir:{}", dir), page);
    }

    // reverse folder deps (used-by) + description
    for dir in &folders {
        let mut used_by: Vec<Value> = Vec::new();
        for o in &folders {
            if o == dir { continue; }
            let deps = pages.get(&format!("dir:{}", o)).and_then(|p| p.get("dependsOn")).and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let hit = deps.iter().any(|d| { let ds = js_string(d); let mapped = if ds == "(root)" { "" } else { ds.as_str() }; mapped == dir.as_str() });
            if hit { used_by.push(json!(o)); }
        }
        used_by.truncate(20);
        let (by_role_v, deps_v, entry_kinds_v) = {
            let p = pages.get(&format!("dir:{}", dir)).unwrap();
            (p.get("stats").and_then(|s| s.get("byRole")).cloned().unwrap_or(json!({})),
             p.get("dependsOn").cloned().unwrap_or(json!([])),
             p.get("entryKinds").cloned().unwrap_or(json!({})))
        };
        let desc = folder_description(dir, &by_role_v, &deps_v, &json!(used_by), &entry_kinds_v);
        let p = pages.get_mut(&format!("dir:{}", dir)).unwrap();
        let obj = p.as_object_mut().unwrap();
        obj.insert("usedBy".into(), json!(used_by));
        obj.insert("description".into(), json!(desc));
    }

    // root overview extras
    if pages.contains_key("dir:") {
        let conventions = crate::codebase_code_patterns::mineConventions(&json!(decs.clone()), &json!(by_path_order.clone()));
        let pattern_census_v = pages.get("dir:").and_then(|p| p.get("patternCensus")).cloned().unwrap_or(json!({}));
        let by_role_v = pages.get("dir:").and_then(|p| p.get("stats")).and_then(|s| s.get("byRole")).cloned().unwrap_or(json!({}));
        let languages_v = if digest.is_null() { Value::Null } else { digest.get("languages").cloned().unwrap_or(Value::Null) };
        let identity = crate::codebase_code_patterns::codeIdentity(&languages_v, &pattern_census_v, &conventions, &by_role_v);
        let p = pages.get_mut("dir:").unwrap();
        let obj = p.as_object_mut().unwrap();
        obj.insert("digest".into(), digest.clone());
        obj.insert("insights".into(), insights.clone());
        obj.insert("conventions".into(), conventions);
        obj.insert("identity".into(), identity);
    }

    let tree = build_tree("", &folders, &by_path_order);
    let index: Vec<Value> = decs.iter().map(|d| json!({
        "name": d.get("name").cloned().unwrap_or(Value::Null), "kind": d.get("kind").cloned().unwrap_or(Value::Null),
        "path": d.get("path").cloned().unwrap_or(Value::Null), "anchor": d.get("name").cloned().unwrap_or(Value::Null),
        "signature": d.get("signature").cloned().unwrap_or(Value::Null), "summary": d.get("summary").cloned().unwrap_or(Value::Null),
        "role": d.get("role").cloned().unwrap_or(Value::Null)
    })).collect();
    let mut totals = json!({ "files": by_path_order.len() as i64, "folders": folders.len() as i64, "blocks": decs.len() as i64 });

    // opt-in token estimate
    if truthy(input.get("with_tokens")) {
        let mut doc_tokens = 0i64;
        let keys: Vec<String> = pages.keys().cloned().collect();
        for k in &keys {
            let md = page_to_md(pages.get(k).unwrap());
            let t = crate::system_tokens_count::estimateTokens(&json!(md));
            let is_file = pages.get(k).and_then(|p| p.get("kind")).and_then(|v| v.as_str()) == Some("file");
            if is_file { doc_tokens += t; }
            pages.get_mut(k).unwrap().as_object_mut().unwrap().insert("tokens".into(), json!(t));
        }
        totals.as_object_mut().unwrap().insert("tokens".into(), json!(doc_tokens));
    }

    // single-page / markdown mode
    if let Some(pg_in) = input.get("page") {
        if !pg_in.is_null() {
            let key = js_string(pg_in);
            let dirkey = format!("dir:{}", key);
            match pages.get(&key).or_else(|| pages.get(&dirkey)) {
                None => return Ok(json!({ "ok": false, "error": format!("no doc page for \"{}\"", tmpl(Some(pg_in))), "origin": origin })),
                Some(p) => {
                    if input.get("format").and_then(|v| v.as_str()) == Some("md") {
                        return Ok(json!({ "ok": true, "page": pg_in.clone(), "format": "md", "markdown": page_to_md(p) }));
                    }
                    return Ok(json!({ "ok": true, "page": p.clone() }));
                }
            }
        }
    }
    // context pack
    if let Some(ctx) = input.get("context") {
        if let Some(arr) = ctx.as_array() {
            let mut parts: Vec<String> = Vec::new();
            for key_v in arr.iter().take(50) {
                let key = js_string(key_v);
                let dirkey = format!("dir:{}", key);
                if let Some(p) = pages.get(&key).or_else(|| pages.get(&dirkey)) { parts.push(page_to_md(p)); }
            }
            return Ok(json!({ "ok": true, "context": ctx.clone(), "markdown": parts.join("\n\n---\n\n") }));
        }
    }
    // static export
    if truthy(input.get("export")) {
        let mut file_pages: Vec<&Value> = pages.values().filter(|p| p.get("kind").and_then(|v| v.as_str()) == Some("file")).collect();
        file_pages.sort_by(|a, b| js_string(a.get("path").unwrap_or(&Value::Null)).cmp(&js_string(b.get("path").unwrap_or(&Value::Null))));
        let files: Vec<Value> = file_pages.iter().map(|p| json!({ "path": p.get("path").cloned().unwrap_or(Value::Null), "markdown": page_to_md(p) })).collect();
        let mut combined = format!("# {} \u{2014} codebase documentation\n\n{} files \u{00B7} {} blocks \u{00B7} {} folders\n\n", tmpl(Some(&origin)), by_path_order.len(), decs.len(), folders.len());
        combined.push_str(&files.iter().map(|f| js_string(f.get("markdown").unwrap_or(&Value::Null))).collect::<Vec<_>>().join("\n\n---\n\n"));
        return Ok(json!({ "ok": true, "files": files, "combined_md": combined, "totals": totals }));
    }
    Ok(json!({ "ok": true, "found": true, "origin": origin, "tree": tree, "pages": Value::Object(pages), "index": index, "totals": totals }))
}

// ── deterministic Markdown render ──
fn page_to_md(page: &Value) -> String {
    let mut l: Vec<String> = Vec::new();
    if page.get("kind").and_then(|v| v.as_str()) == Some("file") {
        l.push(format!("# {}", js_string(page.get("path").unwrap_or(&Value::Null))));
        l.push(String::new());
        l.push(format!("*{}*", or_empty(page.get("intro"))));
        l.push(String::new());
        let narr = page.get("narrative");
        let essay = narr.and_then(|n| n.get("essay"));
        let body = if truthy(essay) { js_string(essay.unwrap()) } else { js_string(narr.and_then(|n| n.get("fallback")).unwrap_or(&Value::Null)) };
        l.push(body);
        l.push(String::new());
        if let Some(eps) = page.get("entryPoints").and_then(|v| v.as_array()) { if !eps.is_empty() {
            let parts: Vec<String> = eps.iter().take(12).map(|e| {
                let first = if truthy(e.get("http_method")) { e.get("http_method").unwrap().clone() } else if truthy(e.get("subtype")) { e.get("subtype").unwrap().clone() } else { e.get("kind").cloned().unwrap_or(Value::Null) };
                format!("{} {}", tmpl(Some(&first)), tmpl(e.get("label"))).trim().to_string()
            }).collect();
            l.push(format!("**Entry points:** {}", parts.join(", "))); l.push(String::new());
        } }
        if let Some(ds) = page.get("dataStructures").and_then(|v| v.as_array()) { if !ds.is_empty() {
            let parts: Vec<String> = ds.iter().map(|t| format!("{} {}", tmpl(t.get("name")), tmpl(t.get("shape"))).trim().to_string()).collect();
            l.push(format!("**Data structures:** {}", parts.join("; "))); l.push(String::new());
        } }
        if let Some(ps) = page.get("patterns").and_then(|v| v.as_array()) { if !ps.is_empty() {
            l.push(format!("**Patterns:** {}", ps.iter().map(|p| tmpl(p.get("pattern"))).collect::<Vec<_>>().join(", "))); l.push(String::new());
        } }
        if let Some(m) = page.get("metrics") { if truthy(Some(m)) {
            let blast = page.get("blastRadius").and_then(|b| b.get("count"));
            let blast_str = if truthy(blast) { format!(" \u{00B7} blast {}", tmpl(blast)) } else { String::new() };
            l.push(format!("**Metrics:** grade {} \u{00B7} maintainability {}/100 \u{00B7} {} LOC \u{00B7} complexity avg {}/max {} \u{00B7} fan-in {} / fan-out {}{}",
                tmpl(m.get("grade")), tmpl(m.get("mi")), tmpl(m.get("loc")), tmpl(m.get("avgComplexity")), tmpl(m.get("maxComplexity")), tmpl(m.get("fanIn")), tmpl(m.get("fanOut")), blast_str));
            l.push(String::new());
        } }
        if let Some(toc) = page.get("toc").and_then(|v| v.as_array()) {
            let blocks = page.get("blocks").and_then(|v| v.as_array());
            let empty = json!({});
            for sec in toc {
                l.push(format!("## {}", tmpl(sec.get("group")))); l.push(String::new());
                if let Some(items) = sec.get("items").and_then(|v| v.as_array()) {
                    for it in items {
                        let it_name = it.get("name");
                        let d = blocks.and_then(|bs| bs.iter().find(|b| match (b.get("name"), it_name) { (Some(x), Some(y)) => strict_eq(x, y), _ => false })).unwrap_or(&empty);
                        let sig = d.get("signature");
                        l.push(format!("### `{}`", if truthy(sig) { js_string(sig.unwrap()) } else { tmpl(it_name) }));
                        if truthy(d.get("summary")) { l.push(js_string(d.get("summary").unwrap())); }
                        if let Some(out_label) = d.get("output").and_then(|o| o.get("label")) {
                            let okind = d.get("output").and_then(|o| o.get("kind"));
                            if truthy(Some(out_label)) && out_label.as_str() != Some("void") && okind.and_then(|v| v.as_str()) != Some("methods") {
                                l.push(format!("- **returns** `{}`", js_string(out_label)));
                            }
                        }
                        if let Some(eff) = d.get("effects").and_then(|v| v.as_array()) { if !eff.is_empty() {
                            l.push(format!("- **effects** {}", eff.iter().map(|e| tmpl(Some(e))).collect::<Vec<_>>().join(", ")));
                        } }
                        if let Some(us) = d.get("uses").and_then(|v| v.as_array()) { if !us.is_empty() {
                            l.push(format!("- **uses** {}", us.iter().map(|c| tmpl(c.get("name"))).collect::<Vec<_>>().join(", ")));
                        } }
                        if let Some(ub) = d.get("usedBy").and_then(|v| v.as_array()) { if !ub.is_empty() {
                            let names = ub.iter().map(|c| tmpl(c.get("name"))).collect::<Vec<_>>().join(", ");
                            let ubc = d.get("used_by_count").and_then(|v| v.as_i64()).unwrap_or(0);
                            let extra = if ubc > ub.len() as i64 { format!(" (+{} more)", ubc - ub.len() as i64) } else { String::new() };
                            l.push(format!("- **used by** {}{}", names, extra));
                        } }
                        if let Some(st) = d.get("steps").and_then(|v| v.as_array()) { if !st.is_empty() {
                            l.push(String::new()); l.push("```".to_string());
                            for s in st {
                                let depth = s.get("depth").and_then(|v| v.as_f64()).map(|f| f as usize).unwrap_or(0);
                                l.push(format!("{}{}: {}", "  ".repeat(depth), tmpl(s.get("kind")), tmpl(s.get("text"))));
                            }
                            l.push("```".to_string());
                        } }
                        l.push(String::new());
                    }
                }
            }
        }
    } else {
        let path = page.get("path");
        let path_disp = if truthy(path) { js_string(path.unwrap()) } else { "/".to_string() };
        l.push(format!("# {} \u{2014} overview", path_disp)); l.push(String::new());
        let desc = page.get("description");
        if truthy(desc) { l.push(js_string(desc.unwrap())); } else { l.push(format!("*{}*", or_empty(page.get("intro")))); }
        l.push(String::new());
        let stats = page.get("stats");
        let bk_parts: Vec<String> = match stats.and_then(|s| s.get("byKind")).and_then(|v| v.as_object()) {
            Some(o) => o.iter().map(|(k, n)| format!("{} {}", tmpl(Some(n)), k)).collect(), None => vec![],
        };
        l.push(format!("{} files \u{00B7} {} blocks ({}).", tmpl(stats.and_then(|s| s.get("files"))), tmpl(stats.and_then(|s| s.get("blocks"))), bk_parts.join(", ")));
        l.push(String::new());
        if let Some(m) = page.get("metrics") { if truthy(Some(m)) {
            l.push(format!("**Metrics:** grade {} \u{00B7} avg maintainability {}/100 \u{00B7} {} LOC \u{00B7} complexity avg {}/max {}",
                tmpl(m.get("grade")), tmpl(m.get("mi")), tmpl(m.get("loc")), tmpl(m.get("avgComplexity")), tmpl(m.get("maxComplexity"))));
            l.push(String::new());
        } }
        if let Some(a) = page.get("dependsOn").and_then(|v| v.as_array()) { if !a.is_empty() {
            l.push(format!("**Depends on:** {}", a.iter().map(|d| tmpl(Some(d))).collect::<Vec<_>>().join(", "))); l.push(String::new());
        } }
        if let Some(a) = page.get("usedBy").and_then(|v| v.as_array()) { if !a.is_empty() {
            l.push(format!("**Used by:** {}", a.iter().map(|d| tmpl(Some(d))).collect::<Vec<_>>().join(", "))); l.push(String::new());
        } }
        if truthy(page.get("identity")) { l.push(format!("**Code identity:** {}", js_string(page.get("identity").unwrap()))); l.push(String::new()); }
        if let Some(nm) = page.get("conventions").and_then(|c| c.get("naming")).and_then(|v| v.as_array()) { if !nm.is_empty() {
            let parts: Vec<String> = nm.iter().map(|n| format!("{} {} {}%", tmpl(n.get("subject")), tmpl(n.get("convention")), tmpl(n.get("adherence")))).collect();
            l.push(format!("**Naming conventions:** {}", parts.join(", "))); l.push(String::new());
        } }
        if let Some(pc) = page.get("patternCensus").and_then(|v| v.as_object()) { if !pc.is_empty() {
            let mut entries: Vec<(&String, &Value)> = pc.iter().collect();
            entries.sort_by(|a, b| b.1.as_i64().unwrap_or(0).cmp(&a.1.as_i64().unwrap_or(0)));
            let parts: Vec<String> = entries.iter().map(|(p, n)| format!("{} \u{00D7}{}", p, tmpl(Some(*n)))).collect();
            l.push(format!("**Design patterns:** {}", parts.join(", "))); l.push(String::new());
        } }
        if let Some(a) = page.get("entryPoints").and_then(|v| v.as_array()) { if !a.is_empty() {
            let parts: Vec<String> = match page.get("entryKinds").and_then(|v| v.as_object()) {
                Some(o) => o.iter().map(|(k, n)| format!("{} {}", tmpl(Some(n)), entry_label_disp(k))).collect(), None => vec![],
            };
            l.push(format!("**External surface:** {}", parts.join(", "))); l.push(String::new());
        } }
        if let Some(ro) = page.get("readingOrder").and_then(|v| v.as_array()) { if !ro.is_empty() {
            let parts: Vec<String> = ro.iter().take(8).map(|r| tmpl(r.get("name"))).collect();
            l.push(format!("**Suggested reading order:** {}", parts.join(" \u{2192} "))); l.push(String::new());
        } }
        if let Some(ds) = page.get("dataStructures").and_then(|v| v.as_array()) { if !ds.is_empty() {
            l.push(String::new()); l.push("## Data structures".to_string()); l.push(String::new());
            for t in ds.iter().take(30) { l.push(format!("- `{}` {}", tmpl(t.get("name")), tmpl(t.get("shape"))).trim().to_string()); }
            l.push(String::new());
        } }
        if let Some(st) = page.get("stories").and_then(|v| v.as_array()) { if !st.is_empty() {
            l.push("## Data flows".to_string()); l.push(String::new());
            for s in st {
                let hops = s.get("hops").and_then(|v| v.as_array()).map(|a| a.iter().map(|h| tmpl(Some(h))).collect::<Vec<_>>().join(" \u{2192} ")).unwrap_or_default();
                l.push(format!("- **{}** \u{2014} {}", tmpl(s.get("name")), hops));
            }
            l.push(String::new());
        } }
        if let Some(sf) = page.get("subfolders").and_then(|v| v.as_array()) { if !sf.is_empty() {
            l.push("## Sub-folders".to_string()); l.push(String::new());
            for s in sf { l.push(format!("- `{}/` \u{2014} {} files", tmpl(s.get("name")), tmpl(s.get("files")))); }
            l.push(String::new());
        } }
        if let Some(ps) = page.get("publicSurface").and_then(|v| v.as_array()) { if !ps.is_empty() {
            l.push("## Public surface".to_string()); l.push(String::new());
            for s in ps {
                let sk = if truthy(s.get("summary")) { js_string(s.get("summary").unwrap()) } else { js_string(s.get("kind").unwrap_or(&Value::Null)) };
                l.push(format!("- `{}` \u{2014} {}", tmpl(s.get("name")), sk));
            }
            l.push(String::new());
        } }
        if truthy(page.get("digest")) {
            let g = page.get("digest").unwrap();
            l.push("## Codebase health".to_string()); l.push(String::new());
            l.push(format!("Health {} \u{00B7} Risk {} \u{00B7} {} files \u{00B7} {} LOC \u{00B7} coverage {}% \u{00B7} {} cycles.",
                coalesce_str(g.get("health"), "\u{2014}"), coalesce_str(g.get("risk"), "\u{2014}"),
                coalesce_str(g.get("files"), "?"), coalesce_str(g.get("loc"), "?"),
                coalesce_str(g.get("coverage_pct"), "\u{2014}"), coalesce_str(g.get("cycles"), "0")));
            l.push(String::new());
            if truthy(g.get("narrative")) { l.push(js_string(g.get("narrative").unwrap())); l.push(String::new()); }
        }
        if let Some(ins) = page.get("insights") {
            if truthy(ins.get("review")) { l.push("## Insights".to_string()); l.push(String::new()); l.push(js_string(ins.get("review").unwrap())); l.push(String::new()); }
        }
    }
    l.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn seeded() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE code_decorators (id INTEGER PRIMARY KEY, origin TEXT, path TEXT, name TEXT, kind TEXT, role TEXT, line INTEGER, end_line INTEGER, signature TEXT, summary TEXT, inputs_json TEXT, output_json TEXT, effects_json TEXT, calls_json TEXT, uses_json TEXT, complexity INTEGER, flags_json TEXT, steps_json TEXT);\
             INSERT INTO code_decorators (origin,path,name,kind,role,line,end_line,signature,summary,inputs_json,output_json,effects_json,calls_json,uses_json,complexity,flags_json,steps_json) VALUES\
              ('atlas','src/api/orders.js','saveOrder','function','data',10,40,'saveOrder(o)','Persist an order.','[]','{\"label\":\"void\",\"kind\":\"void\"}','[\"fetches\"]','[\"db.insert\"]','[]',6,'[\"exported\"]','[]'),\
              ('atlas','src/ui/Cart.jsx','Cart','component','ui',1,30,'Cart(props)','Cart view.','[{\"name\":\"props\"}]','{\"label\":\"JSX\",\"kind\":\"jsx\"}','[]','[\"saveOrder\"]','[]',3,'[\"exported\"]','[]');"
        ).unwrap();
        c
    }

    #[test]
    fn builds_tree_and_pages() {
        let c = seeded();
        let r = codeDocsBuild(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["found"], json!(true));
        assert_eq!(r["totals"]["files"], json!(2));
        assert_eq!(r["totals"]["blocks"], json!(2));
        // a file page exists and carries metrics + a grade
        let page = &r["pages"]["src/api/orders.js"];
        assert_eq!(page["kind"], json!("file"));
        assert!(page["metrics"]["grade"].is_string());
        // cross-ref: Cart calls saveOrder → orders.js is used by Cart.jsx
        assert!(page["usedBy"].as_array().unwrap().iter().any(|p| p == &json!("src/ui/Cart.jsx")));
    }

    #[test]
    fn markdown_mode_renders() {
        let c = seeded();
        let r = codeDocsBuild(&json!({ "page": "src/api/orders.js", "format": "md" }), &c).unwrap();
        assert_eq!(r["format"], json!("md"));
        let md = r["markdown"].as_str().unwrap();
        assert!(md.starts_with("# src/api/orders.js"));
        assert!(md.contains("**Metrics:** grade"));
    }

    #[test]
    fn missing_page_errors() {
        let c = seeded();
        let r = codeDocsBuild(&json!({ "page": "does/not/exist.js" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("no doc page"));
    }

    #[test]
    fn empty_decorators_found_false() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_decorators (id INTEGER PRIMARY KEY, origin TEXT, path TEXT, name TEXT, kind TEXT, role TEXT, line INTEGER, end_line INTEGER, signature TEXT, summary TEXT, inputs_json TEXT, output_json TEXT, effects_json TEXT, calls_json TEXT, uses_json TEXT, complexity INTEGER, flags_json TEXT, steps_json TEXT);").unwrap();
        let r = codeDocsBuild(&json!({}), &c).unwrap();
        assert_eq!(r["found"], json!(false));
        assert_eq!(r["totals"]["blocks"], json!(0));
    }
}
