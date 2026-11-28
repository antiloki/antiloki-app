//! BRANCHES (RFC 0015, operator 2026-11-17): a branch is a TEAM with ports. Same body as a team recipe (every node's full settings, edges, start, budget) plus a header: a name, a version, typed parameters,
//! typed input and output ports, what it requires (tool entries, models with defaults, runtimes) and examples that must pass. It is a YAML file (strict subset, see team_yaml) that stands alone: it can be exported
//! from a team, shared, imported into another team, or run on its own. JSON stays the canonical form: the hash is SHA-256 over the canonical JSON of the whole file without its `hash` field, so the surface syntax never matters.
//! Rules for what an imported branch may do (enforced at import, a later slice): it can REQUEST limits but never raise its parent's budget, gates or approvals; package approvals are the person's and are never imported;
//! a model seat is a requirement with a default that the importing team can map (models differ per machine); improving a shared branch makes a COPY under its own hash with its lineage, so one team never silently changes another's.
//! This slice: the format, its checks, the hash, and EXPORT of a set of nodes from a team (ports are derived from what crosses the boundary). All pure except the ops that write the library file.
use crate::team_graph::{validate, Graph};
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub const SCHEMA: &str = "antiloki-branch/1";
/// The order of the top-level keys in a branch file: the header a reader wants first, the body next, the hash last. Pure.
const ORDER: [&str; 16] = ["schema", "name", "version", "description", "license", "template", "params", "requires", "settings", "inputs", "outputs", "start", "nodes", "edges", "examples", "hash"];
fn strip_nulls(v: &Value) -> Value { match v { Value::Object(m) => Value::Object(m.iter().filter(|(_, x)| !x.is_null()).map(|(k, x)| (k.clone(), strip_nulls(x))).collect()), Value::Array(a) => Value::Array(a.iter().map(strip_nulls).collect()), o => o.clone() } }
fn canon(v: &Value) -> Value { match v { Value::Object(m) => { let mut k: Vec<_> = m.iter().collect(); k.sort_by(|a, b| a.0.cmp(b.0)); Value::Object(k.into_iter().map(|(a, b)| (a.clone(), canon(b))).collect()) } Value::Array(a) => Value::Array(a.iter().map(canon).collect()), o => o.clone() } }
/// The hash of a branch: SHA-256 over its canonical JSON without the `hash` field. Pure.
pub fn hash_of(v: &Value) -> String {
    let mut c = v.clone(); if let Some(m) = c.as_object_mut() { m.remove("hash"); }
    format!("sha256:{}", Sha256::digest(serde_json::to_string(&canon(&c)).unwrap_or_default().as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>())
}
fn slug_ok(s: &str) -> bool { !s.is_empty() && s.len() <= 60 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !s.starts_with('-') }
fn version_ok(s: &str) -> bool { let p: Vec<&str> = s.split('.').collect(); p.len() == 3 && p.iter().all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit())) }
/// The library file of a branch by its NAME. A name is lowercase letters, digits and dashes, so a request can never build a path that leaves the library (`../../x`). Pure.
/// A branch of the library as YAML text: from the project's database (files from before are imported once). Blocking.
pub fn library_text(root: &str, name: &str) -> Option<String> { crate::docstore::import_dir(root, "branch", &std::path::Path::new(root).join(".antiloki").join("branches"), &["yaml"]); crate::docstore::get_text(root, "branch", name) }
pub fn library_path(root: &str, name: &str) -> Result<std::path::PathBuf, String> {
    if !slug_ok(name) { return Err("a branch name is lowercase letters, digits and dashes".into()); }
    Ok(std::path::Path::new(root).join(".antiloki").join("branches").join(format!("{name}.yaml")))
}
/// The graph a branch body describes (a branch has no leader of its own: the team that imports it has one). Pure.
pub fn graph_of(v: &Value) -> Result<Graph, String> {
    Graph::from_json(&json!({ "team": v["name"], "schema": 1, "budget_usd": v["settings"]["budget_usd"].as_f64().unwrap_or(1.0), "start": v["start"], "nodes": v["nodes"], "edges": v["edges"] }))
}
/// What is wrong with a branch file, if anything. Pure.
pub fn check(v: &Value) -> Vec<String> {
    let mut e = vec![];
    if v["schema"] != json!(SCHEMA) { e.push(format!("`schema` is {SCHEMA}")); }
    if !slug_ok(v["name"].as_str().unwrap_or("")) { e.push("`name` is lowercase letters, digits and dashes".into()); }
    if !version_ok(v["version"].as_str().unwrap_or("")) { e.push("`version` is three numbers, like 0.1.0".into()); }
    if v["description"].as_str().unwrap_or("").trim().is_empty() { e.push("`description` says what the branch is for".into()); }
    if v["license"].as_str().unwrap_or("").trim().is_empty() { e.push("`license` is required (say `proprietary` or an SPDX id)".into()); }
    let nodes = v["nodes"].as_object().cloned().unwrap_or_default();
    if nodes.is_empty() { e.push("a branch has at least one node".into()); return e; }
    for (kind, key) in [("input", "inputs"), ("output", "outputs")] {
        for (i, p) in v[key].as_array().cloned().unwrap_or_default().iter().enumerate() {
            if p["name"].as_str().unwrap_or("").is_empty() { e.push(format!("{kind} port {i} has no name")); }
            if kind == "output" { if let Some(n) = p["node"].as_str() { if !nodes.contains_key(n) { e.push(format!("output port `{}` names the node `{n}`, which is not in the branch", p["name"].as_str().unwrap_or(""))); } } else { e.push(format!("output port `{}` says which `node` it comes from", p["name"].as_str().unwrap_or(""))); } }
        }
    }
    for (k, p) in v["params"].as_object().cloned().unwrap_or_default() { if !matches!(p["type"].as_str(), Some("string") | Some("number") | Some("bool")) { e.push(format!("param `{k}`: `type` is string, number or bool")); } if p.get("default").is_none() { e.push(format!("param `{k}` has a `default`")); } }
    // every placeholder must be declared; a parameter's default must have its type; a required model has a default seat
    let mut ph = vec![]; placeholders(&v["nodes"], &mut ph); ph.sort(); ph.dedup();
    for (kind, name) in &ph { let ok = if kind == "param" { v["params"].get(name).is_some() } else { v["requires"]["models"].as_array().map_or(false, |a| a.iter().any(|x| x["name"] == json!(name))) }; if !ok { e.push(format!("`${{{kind}.{name}}}` is used but not declared (in `params` / `requires.models`)")); } }
    for (k, d) in v["params"].as_object().cloned().unwrap_or_default() { if !d["default"].is_null() && !type_ok(d["type"].as_str().unwrap_or(""), &d["default"]) { e.push(format!("param `{k}`: the default is not a {}", d["type"].as_str().unwrap_or("?"))); } }
    for x in v["requires"]["models"].as_array().cloned().unwrap_or_default() { if x["name"].as_str().unwrap_or("").is_empty() || x["default"].as_str().is_none() { e.push("each entry of `requires.models` has a `name` and a default `seat` (as `default`)".into()); } }
    match graph_of(&with_defaults(v)) {
        Err(x) => e.push(format!("the nodes and edges do not make a graph: {x}")),
        // a branch is judged as a graph, minus what only a whole team has: a leader, and a failure port that its importer will wire
        Ok(g) => for i in validate(&g).errors {
            // a branch is judged as a graph minus what only a whole team has: a leader and a delivery; a port that leaves it is an OUTPUT, so a dead end is only an error when it is not a declared output
            if i.code == "no_leader" || i.code == "no_delivery" { continue; }
            if i.code == "dead_end" { let port = i.message.split('`').nth(1).unwrap_or(""); let node = i.node.clone().unwrap_or_default();
                if v["outputs"].as_array().map_or(false, |o| o.iter().any(|p| p["node"] == json!(node) && p["port"] == json!(port))) { continue; }
                e.push(format!("{node}: its `{port}` port leads nowhere: wire it inside the branch or declare it as an output port")); continue; }
            e.push(format!("{}{}", i.node.map(|n| format!("{n}: ")).unwrap_or_default(), i.message));
        },
    }
    for (i, x) in v["examples"].as_array().cloned().unwrap_or_default().iter().enumerate() { if x["name"].as_str().unwrap_or("").is_empty() { e.push(format!("example {i} has no name")); } }
    e
}
/// The branch file as text: the value plus its hash, in the strict YAML layout. Pure.
pub fn to_text(v: &Value) -> String {
    let mut c = v.clone(); c["hash"] = json!(hash_of(v)); let m = c.as_object().cloned().unwrap_or_default(); let mut out = String::from("# antiloki branch file (strict YAML subset, see RFC 0015). Edit it, then check it: the hash says whether it was changed by hand.\n");
    let mut keys: Vec<&String> = m.keys().collect(); keys.sort_by_key(|k| (ORDER.iter().position(|o| o == k).unwrap_or(ORDER.len()), (*k).clone()));
    for k in keys { let mut one = Map::new(); one.insert(k.clone(), m[k].clone()); out.push_str(&crate::team_yaml::to_yaml(&Value::Object(one))); }
    out
}
/// Read a branch file: strict YAML, the hash must match, the content must check out. Pure.
pub fn from_text(text: &str) -> Result<Value, String> {
    let v = crate::team_yaml::parse_strict(text)?;
    match v["hash"].as_str() { Some(h) if h == hash_of(&v) => {} Some(_) => return Err("the branch does not match its hash: it was edited after it was written (rewrite it with its new hash)".into()), None => return Err("the branch has no `hash`".into()) }
    let e = check(&v); if e.is_empty() { Ok(v) } else { Err(e.join("; ")) }
}
/// Everything inside a node that points at another node: `values:<node>.<field>` references and `<node>.<field>` sources. Pure.
fn refs_in(v: &Value, out: &mut Vec<(String, String)>) {
    match v {
        Value::String(s) => { if let Some(r) = s.strip_prefix("values:") { let mut p = r.splitn(2, '.'); if let (Some(n), Some(f)) = (p.next(), p.next()) { out.push((n.to_string(), f.to_string())); } else if !r.is_empty() { out.push((r.to_string(), String::new())); } } }
        Value::Array(a) => a.iter().for_each(|x| refs_in(x, out)),
        Value::Object(m) => { for (k, x) in m { if k == "from" || k == "content_from" { if let Some(s) = x.as_str() { if !s.starts_with("values:") && s != "ask" && s.contains('.') { let mut p = s.splitn(2, '.'); out.push((p.next().unwrap_or("").to_string(), p.next().unwrap_or("").to_string())); } } } refs_in(x, out); } }
        _ => {}
    }
}
/// A branch file from some nodes of a team. Ports are DERIVED: an input for every value a node reads from a node outside the set, an output for every edge that leaves it. `meta`: {name, version, description, license, params?, requires?, settings?, examples?}. Pure.
pub fn export(g: &Graph, ids: &[String], meta: &Value) -> Result<Value, String> {
    let all = g.to_json(); let all_nodes = all["nodes"].as_object().cloned().unwrap_or_default();
    for i in ids { if !all_nodes.contains_key(i) { return Err(format!("the team has no node `{i}`")); } }
    if ids.is_empty() { return Err("name the nodes to export".into()); }
    let inside = |n: &str| ids.iter().any(|i| i == n);
    let nodes: Map<String, Value> = ids.iter().map(|i| (i.clone(), strip_nulls(&all_nodes[i]))).collect();
    let edges: Vec<Value> = all["edges"].as_array().cloned().unwrap_or_default().into_iter().filter(|e| inside(e["from"].as_str().unwrap_or("")) && inside(e["to"].as_str().unwrap_or(""))).collect();
    let (mut inputs, mut outputs): (Vec<Value>, Vec<Value>) = (vec![], vec![]);
    for i in ids { let mut r = vec![]; refs_in(&all_nodes[i], &mut r); r.sort(); r.dedup();
        for (n, f) in r { if !inside(&n) && all_nodes.contains_key(&n) { let nm = if f.is_empty() { n.clone() } else { format!("{n}_{}", f.replace('.', "_")) }; if !inputs.iter().any(|p| p["name"] == json!(nm)) { inputs.push(json!({ "name": nm, "type": "string", "ref": if f.is_empty() { n.clone() } else { format!("{n}.{f}") }, "used_by": i })); } } } }
    for e in all["edges"].as_array().cloned().unwrap_or_default() { if inside(e["from"].as_str().unwrap_or("")) && !inside(e["to"].as_str().unwrap_or("")) { outputs.push(json!({ "name": format!("{}_{}", e["from"].as_str().unwrap_or(""), e["port"].as_str().unwrap_or("out")), "node": e["from"], "port": e["port"], "goes_to": e["to"] })); } }
    let mut nodes = nodes; let mut nv = Value::Object(nodes.clone()); let mut params = meta.get("params").cloned().unwrap_or(json!({})); let mut models: Vec<Value> = meta["requires"]["models"].as_array().cloned().unwrap_or_default();
    // PARAMETERS: each `meta.params.<name>` may name `paths` (`node.key.key`) whose current value becomes the default and is replaced by `${param.<name>}`
    if let Some(pm) = params.as_object_mut() { for (name, d) in pm.iter_mut() {
        let paths: Vec<String> = d["paths"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        for pth in &paths { let cur = pth.split('.').fold(Some(&nv), |c, seg| c.and_then(|x| match x { Value::Array(a) => seg.parse::<usize>().ok().and_then(|i| a.get(i)), o => o.get(seg) })).cloned().ok_or_else(|| format!("param `{name}`: nothing at `{pth}`"))?;
            if d.get("default").map_or(true, |x| x.is_null()) { d["default"] = cur.clone(); } else if d["default"] != cur { return Err(format!("param `{name}`: `{pth}` holds {cur} but the default says {}", d["default"])); }
            set_path(&mut nv, pth, json!(format!("${{param.{name}}}")))?; }
        if let Some(o) = d.as_object_mut() { o.remove("paths"); } } }
    // SEATS: `meta.seats` {name: "local:tag"}: every place that seat is written becomes `${model.<name>}`, and the branch requires it with that default
    if let Some(sm) = meta["seats"].as_object() { for (name, lit) in sm { let lit = lit.as_str().ok_or_else(|| format!("seat `{name}`: give the seat as text"))?; replace_literal(&mut nv, lit, &format!("${{model.{name}}}")); if !models.iter().any(|x| x["name"] == json!(name)) { models.push(json!({ "name": name, "default": lit, "for": meta["seat_for"][name].clone() })); } } }
    nodes = nv.as_object().cloned().unwrap_or_default(); let mut requires = meta.get("requires").cloned().unwrap_or(json!({})); if !models.is_empty() { requires["models"] = json!(models); }
    let start: Vec<String> = ids.iter().filter(|i| !edges.iter().any(|e| e["to"] == json!(i))).cloned().collect();
    let v = json!({ "schema": SCHEMA, "name": meta["name"], "version": meta.get("version").cloned().unwrap_or(json!("0.1.0")), "description": meta["description"], "license": meta.get("license").cloned().unwrap_or(json!("proprietary")),
        "params": params, "inputs": inputs, "outputs": outputs, "requires": requires, "settings": meta.get("settings").cloned().unwrap_or(json!({ "budget_usd": g.budget_usd })),
        "start": start, "nodes": nodes, "edges": edges, "examples": meta.get("examples").cloned().unwrap_or(json!([])) });
    // a branch is saved only if it is well formed AND its examples pass (at least one must be runnable): untested structure is not shared
    let mut e = check(&v); if e.is_empty() { e = example_problems(&v); } if e.is_empty() { Ok(v) } else { Err(e.join("; ")) }
}


// ── PARAMETERS AND SEATS (VID-42, "first 1"): a node's settings may hold `${param.name}` and `${model.name}` ─────────────────────────────────────────
// `params`: {name: {type: string|number|bool, default, description}}; `requires.models`: [{name, default: "local:...", for}]. Using a branch supplies values or takes the defaults; a model seat is a
// REQUIREMENT with a default that the importing team maps to what its machine has. A placeholder that is the whole string keeps its type (a number stays a number); inside a longer string it is written as text.
fn placeholder_re() -> regex::Regex { regex::Regex::new(r"\$\{(param|model)\.([A-Za-z0-9_]+)\}").unwrap() }
/// Every placeholder in a value as (kind, name). Pure.
pub fn placeholders(v: &Value, out: &mut Vec<(String, String)>) {
    match v { Value::String(x) => for c in placeholder_re().captures_iter(x) { out.push((c[1].to_string(), c[2].to_string())); }, Value::Array(a) => a.iter().for_each(|y| placeholders(y, out)), Value::Object(m) => m.values().for_each(|y| placeholders(y, out)), _ => {} }
}
fn type_ok(t: &str, v: &Value) -> bool { match t { "string" => v.is_string(), "number" => v.is_number(), "bool" => v.is_boolean(), _ => false } }
/// The parameter values and model seats a use resolves to: what is supplied, else the defaults; unknown names, wrong types and bad seats are refused. Pure.
pub fn resolve(b: &Value, params: &Map<String, Value>, seats: &Map<String, Value>) -> Result<(Map<String, Value>, Map<String, Value>), String> {
    let declared = b["params"].as_object().cloned().unwrap_or_default(); let mut p = Map::new();
    for k in params.keys() { if !declared.contains_key(k) { return Err(format!("the branch has no parameter `{k}` (it has: {})", declared.keys().cloned().collect::<Vec<_>>().join(", "))); } }
    for (k, d) in &declared { let v = params.get(k).cloned().unwrap_or_else(|| d["default"].clone()); if !type_ok(d["type"].as_str().unwrap_or(""), &v) { return Err(format!("parameter `{k}` is a {}", d["type"].as_str().unwrap_or("?"))); } p.insert(k.clone(), v); }
    let models = b["requires"]["models"].as_array().cloned().unwrap_or_default(); let mut m = Map::new();
    for k in seats.keys() { if !models.iter().any(|x| x["name"] == json!(k)) { return Err(format!("the branch requires no model called `{k}` (it requires: {})", models.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(", "))); } }
    for x in &models { let name = x["name"].as_str().unwrap_or(""); let seat = seats.get(name).cloned().unwrap_or_else(|| x["default"].clone());
        let st = seat.as_str().ok_or_else(|| format!("model `{name}`: give a seat like `local:<tag>`"))?; crate::team_llm::parse_seat(st).map_err(|e| format!("model `{name}`: the seat {e}"))?; m.insert(name.to_string(), seat); }
    Ok((p, m))
}
/// A value with its placeholders replaced. Pure.
pub fn substitute(v: &Value, params: &Map<String, Value>, models: &Map<String, Value>) -> Result<Value, String> {
    match v {
        Value::String(x) => { let re = placeholder_re();
            if let Some(c) = re.captures(x) { if c.get(0).map(|m| m.as_str().len()) == Some(x.len()) { let src = if &c[1] == "param" { params } else { models }; return src.get(&c[2]).cloned().ok_or_else(|| format!("`${{{}.{}}}` is not declared", &c[1], &c[2])); } }
            let mut err = None; let out = re.replace_all(x, |c: &regex::Captures| { let src = if &c[1] == "param" { params } else { models }; match src.get(&c[2]) { Some(Value::String(t)) => t.clone(), Some(o) => o.to_string(), None => { err = Some(format!("`${{{}.{}}}` is not declared", &c[1], &c[2])); String::new() } } }).to_string();
            match err { Some(e) => Err(e), None => Ok(json!(out)) } }
        Value::Array(a) => a.iter().map(|y| substitute(y, params, models)).collect::<Result<Vec<_>, _>>().map(Value::Array),
        Value::Object(m) => m.iter().map(|(k, y)| substitute(y, params, models).map(|z| (k.clone(), z))).collect::<Result<Map<_, _>, _>>().map(Value::Object),
        o => Ok(o.clone()),
    }
}
/// The branch with its defaults filled in (how its examples and its checks see it). If the defaults do not resolve, the branch as it is (the checks then say why). Pure.
pub fn with_defaults(v: &Value) -> Value {
    let Ok((p, m)) = resolve(v, &Map::new(), &Map::new()) else { return v.clone() };
    let Ok(n) = substitute(&v["nodes"], &p, &m) else { return v.clone() }; let mut c = v.clone(); c["nodes"] = n; c
}
/// Put `val` at `node.key.key...` (array indexes allowed) inside the nodes. Pure.
fn set_path(nodes: &mut Value, path: &str, val: Value) -> Result<(), String> {
    let mut cur = nodes; for seg in path.split('.') { cur = match cur { Value::Array(a) => { let i: usize = seg.parse().map_err(|_| format!("`{path}`: `{seg}` is not an index"))?; a.get_mut(i).ok_or_else(|| format!("`{path}`: no item {i}"))? } Value::Object(m) => m.get_mut(seg).ok_or_else(|| format!("`{path}`: nothing at `{seg}`"))?, _ => return Err(format!("`{path}` goes below a value")) }; }
    *cur = val; Ok(())
}
fn replace_literal(v: &mut Value, lit: &str, with: &str) { match v { Value::String(x) if x == lit => *x = with.to_string(), Value::Array(a) => a.iter_mut().for_each(|y| replace_literal(y, lit, with)), Value::Object(m) => m.values_mut().for_each(|y| replace_literal(y, lit, with)), _ => {} } }

// ── USING A BRANCH (VID-37): a branch is expanded INTO the team's own graph (a sub-team runs as a separate team with its own tree, so it could not touch the parent's files) ──────────
/// What using a branch adds to a team.
#[derive(Debug, PartialEq)]
pub struct Instance { pub nodes: Map<String, Value>, pub edges: Vec<Value>, pub start: Vec<String> }
fn valid_prefix(p: &str) -> bool { p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') && !p.starts_with(|c: char| c.is_ascii_digit()) }
struct Rw<'a> { ids: &'a std::collections::BTreeMap<String, String>, bind: &'a std::collections::BTreeMap<String, String>, cond: Option<regex::Regex> }
impl Rw<'_> {
    /// `node[.field...]` with the node renamed when it is inside the branch, or the whole reference replaced when an input is bound to it. Pure.
    fn reference(&self, r: &str) -> String {
        if let Some(b) = self.bind.get(r) { return b.clone(); }
        let (n, rest) = match r.split_once('.') { Some((n, x)) => (n, Some(x)), None => (r, None) };
        match self.ids.get(n) { Some(new) => match rest { Some(x) => format!("{new}.{x}"), None => new.clone() }, None => r.to_string() }
    }
    fn walk(&self, v: &Value, key: Option<&str>) -> Value {
        match v {
            Value::String(x) => {
                if let Some(r) = x.strip_prefix("values:") { return json!(format!("values:{}", self.reference(r))); }
                match key {
                    Some("from") | Some("content_from") | Some("text_from") if x != "ask" => json!(self.reference(x)),
                    Some("contract") | Some("review") | Some("tests") | Some("worker") | Some("judge") => json!(self.ids.get(x).cloned().unwrap_or_else(|| x.clone())),
                    Some("cond") => match &self.cond { Some(re) => json!(re.replace_all(x, |c: &regex::Captures| format!("{}.", self.ids.get(&c[1]).cloned().unwrap_or_else(|| c[1].to_string()))).to_string()), None => v.clone() },
                    _ => v.clone(),
                }
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| self.walk(x, key)).collect()),
            Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), self.walk(x, Some(k)))).collect()),
            o => o.clone(),
        }
    }
}
/// The nodes and edges a branch adds to a team: every node id gets `prefix`, every reference to a node of the branch is rewritten (edges, `values:` references, `from`/`content_from`/`text_from`, `if` conditions, `contract`/`review`/`tests`/`worker`/`judge`),
/// and an input bound by name (`bind`: input name -> `node.field`) replaces the outside reference it stands for. References to nodes OUTSIDE the branch are left as they are, so the team must have them (the validator says if it does not). Pure.
pub fn instantiate(b: &Value, prefix: &str, bind: &Map<String, Value>, params: &Map<String, Value>, seats: &Map<String, Value>) -> Result<Instance, String> {
    let (p, m) = resolve(b, params, seats)?; let mut b = b.clone(); b["nodes"] = substitute(&b["nodes"], &p, &m)?; let b = &b;
    if !valid_prefix(prefix) { return Err("`prefix` is lowercase letters, digits and underscores, not starting with a digit".into()); }
    let nodes = b["nodes"].as_object().ok_or("the branch has no nodes")?; let ids: std::collections::BTreeMap<String, String> = nodes.keys().map(|k| (k.clone(), format!("{prefix}{k}"))).collect();
    let mut bound = std::collections::BTreeMap::new();
    for (name, to) in bind { let to = to.as_str().ok_or_else(|| format!("bind `{name}`: give a reference like `rules.output.title`"))?.trim_start_matches("values:").to_string();
        let port = b["inputs"].as_array().and_then(|a| a.iter().find(|p| p["name"] == json!(name))).ok_or_else(|| format!("the branch has no input `{name}`"))?; bound.insert(port["ref"].as_str().unwrap_or("").to_string(), to); }
    let alt = ids.keys().map(|k| regex::escape(k)).collect::<Vec<_>>().join("|"); let cond = regex::Regex::new(&format!(r"\b({alt})\.")).ok();
    let rw = Rw { ids: &ids, bind: &bound, cond };
    let out: Map<String, Value> = nodes.iter().map(|(k, v)| (format!("{prefix}{k}"), rw.walk(v, None))).collect();
    let edges: Vec<Value> = b["edges"].as_array().cloned().unwrap_or_default().iter().map(|e| json!({ "from": ids.get(e["from"].as_str().unwrap_or("")).cloned().unwrap_or_default(), "port": e["port"], "to": ids.get(e["to"].as_str().unwrap_or("")).cloned().unwrap_or_default() })).collect();
    let start = b["start"].as_array().cloned().unwrap_or_default().iter().filter_map(|x| x.as_str().and_then(|n| ids.get(n).cloned())).collect();
    Ok(Instance { nodes: out, edges, start })
}
/// Does a link still equal what its file says? The nodes of the instance in the team against the nodes the file gives with the same prefix and binding. Returns the ids that differ. Pure.
pub fn drift(b: &Value, prefix: &str, bind: &Map<String, Value>, params: &Map<String, Value>, seats: &Map<String, Value>, team_nodes: &Value) -> Vec<String> {
    let Ok(inst) = instantiate(b, prefix, bind, params, seats) else { return vec!["(the branch file no longer instantiates)".into()] };
    inst.nodes.iter().filter(|(id, v)| strip_nulls(&team_nodes[id.as_str()]) != strip_nulls(v)).map(|(id, _)| id.clone()).collect()
}


// ── EXAMPLES (VID-34): a branch must carry examples and they must pass when it is exported or used ───────────────────────────────────────────────
// A branch cannot run a whole graph offline, so an example tests ONE node of it, deterministically, on a given input:
//   transform / code   {name, node, input, expect}            the node's output must contain `expect` (every key given equal; or `expect_error: "text"`)
//   cell (a small-model step)  {name, node, input, reply, expect | expect_error}   the node's GUARD judges a sample model reply for that input: accepted (the value) or refused (the reason)
//   sound cue_sheet    {name, node, files: {"scenes.js": "..."}, expect | expect_issue: "text"}   the cue sheet read from a sample template
// Nodes that need a runtime, a model or a render (`mix`, `match`, `test`) cannot be tested here and say so; a branch needs at least one runnable example.
#[derive(Debug, PartialEq, Serialize)]
pub struct ExampleResult { pub name: String, pub ok: bool, pub detail: String }
/// Does `actual` contain everything `expect` says (objects: every key given equal; anything else: equal; numbers within 1e-9)? Pure.
pub fn contains(expect: &Value, actual: &Value) -> bool {
    match (expect, actual) {
        (Value::Object(e), Value::Object(a)) => e.iter().all(|(k, x)| a.get(k).map_or(false, |y| contains(x, y))),
        (Value::Number(e), Value::Number(a)) => (e.as_f64().unwrap_or(f64::NAN) - a.as_f64().unwrap_or(f64::INFINITY)).abs() < 1e-9,
        (Value::Array(e), Value::Array(a)) => e.len() == a.len() && e.iter().zip(a).all(|(x, y)| contains(x, y)),
        (e, a) => e == a,
    }
}
/// Run one example against its node. Ok(actual) or Err(the node's refusal / why it could not run). Pure.
fn run_one(v: &Value, ex: &Value) -> Result<Value, String> {
    let id = ex["node"].as_str().unwrap_or(""); let node = v["nodes"].get(id).ok_or_else(|| format!("the node `{id}` is not in the branch"))?;
    match node["node"].as_str().unwrap_or("") {
        "transform" => crate::team_transform::apply(node["mode"].as_str().unwrap_or(""), ex["input"].clone(), &node["config"]),
        "code" => crate::team_code::run(node["source"].as_str().unwrap_or(""), &ex["input"], &crate::team_code::Limits::default()),
        "cell" => { let sp: crate::team_cell::CellSpec = serde_json::from_value(node.clone()).map_err(|e| format!("the cell does not read: {e}"))?;
            if sp.task == "match" { return Err("a `match` cell needs an embedding model and cannot be tested offline".into()); }
            crate::team_cell::guard(&sp, ex["input"].as_str().unwrap_or(""), ex["reply"].as_str().unwrap_or("")) }
        "use_tool" if node["tool"]["type"] == json!("sound") && node["tool"]["name"] == json!("cue_sheet") => {
            let file = node["tool"]["args"]["file"].as_str().unwrap_or(""); let js = ex["files"][file].as_str().ok_or_else(|| format!("give `files.{file}`: a sample template"))?;
            let (cues, issues) = crate::team_sound::cues_of(js); Ok(json!({ "count": cues.len(), "cues": cues, "issues": issues })) }
        other => Err(format!("a node of kind `{other}` cannot be tested offline (testable: transform, code, a cell's guard, the sound cue_sheet)")),
    }
}
/// Run every example of a branch. Pure.
pub fn run_examples(v: &Value) -> Vec<ExampleResult> {
    let v = &with_defaults(v);
    v["examples"].as_array().cloned().unwrap_or_default().iter().map(|ex| {
        let name = ex["name"].as_str().unwrap_or("(unnamed)").to_string(); let r = run_one(v, ex);
        let (ok, detail) = if let Some(want) = ex["expect_error"].as_str() { match &r { Err(e) if e.contains(want) => (true, format!("refused as expected: {e}")), Err(e) => (false, format!("refused, but not with \"{want}\": {e}")), Ok(o) => (false, format!("expected a refusal containing \"{want}\", got {o}")) } }
            else if let Some(want) = ex["expect_issue"].as_str() { match &r { Ok(o) if o["issues"].as_array().map_or(false, |a| a.iter().any(|i| i.as_str().map_or(false, |t| t.contains(want)))) => (true, "the issue was found".into()), Ok(o) => (false, format!("expected an issue containing \"{want}\", got {}", o["issues"])), Err(e) => (false, e.clone()) } }
            else { match &r { Ok(o) if contains(&ex["expect"], o) => (true, "as expected".into()), Ok(o) => (false, format!("expected {} but got {o}", ex["expect"])), Err(e) => (false, e.clone()) } };
        ExampleResult { name, ok, detail }
    }).collect()
}
/// What stops a branch from being saved or used because of its examples: none given, one that cannot run, or one that fails. Pure.
pub fn example_problems(v: &Value) -> Vec<String> {
    let r = run_examples(v); if r.is_empty() { return vec!["the branch has no examples: give at least one (a transform, code, a cell's guard, or the cue sheet)".into()]; }
    let mut p: Vec<String> = r.iter().filter(|x| !x.ok).map(|x| format!("example \"{}\" fails: {}", x.name, x.detail)).collect();
    if p.is_empty() && r.iter().all(|x| x.detail.is_empty()) { p.push("no example ran".into()); } p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team_graph::template;
    fn meta() -> Value { json!({ "name": "narrated-shot", "version": "0.1.0", "description": "cue sheet then mix", "license": "proprietary", "examples": [
        { "name": "the cue sheet reads two labelled beats", "node": "cue", "files": { "scenes.js": "tl.addLabel(\"cue:zoom\", 0);\ntl.to(a, {}, 0);\ntl.addLabel(\"cue:title\", 1.6);\ntl.to(b, {}, 1.6);\n" }, "expect": { "count": 2, "issues": [] } },
        { "name": "a label that drifts from its tween is caught", "node": "cue", "files": { "scenes.js": "tl.addLabel(\"cue:title\", 1.6);\ntl.to(b, {}, 1.8);\n" }, "expect_issue": "says 1.6" }] }) }
    fn team() -> Graph {
        let mut g = template("blank", "t", 2.0).unwrap(); let mut v = g.to_json();
        v["nodes"]["rules"] = json!({ "node": "transform", "mode": "rules", "from": "values:fill", "config": { "fields": { "title": { "case": "upper" } } } });
        v["nodes"]["cue"] = json!({ "node": "use_tool", "tool": { "type": "sound", "name": "cue_sheet", "args": { "file": "scenes.js" } }, "allow": { "paths": [], "panes": [], "skills": [], "mcp": [], "effects": ["read"] }, "starts_model": false, "outputs": { "ok": "bool" } });
        v["nodes"]["mix"] = json!({ "node": "use_tool", "tool": { "type": "sound", "name": "mix", "args": { "file": "scenes.js", "video": "out/video.mp4", "out": "out/v.mp4", "text_from": "values:rules.output.subtitle", "spec": { "beats": { "zoom": { "want": "a whoosh", "gain_db": -8 } }, "narration": { "start_s": 1.0, "end_s": 4.0, "wps": 3.0, "gain_db": 0 }, "music": { "gain_db": -22, "duck_db": -12 }, "target_lufs": -14.0, "true_peak_max": -1.0 } } }, "allow": { "paths": [], "panes": [], "skills": [], "mcp": [], "effects": ["read", "write", "exec"] }, "starts_model": false, "outputs": { "ok": "bool" } });
        v["edges"] = json!([{ "from": "rules", "port": "out", "to": "cue" }, { "from": "cue", "port": "out", "to": "mix" }, { "from": "mix", "port": "out", "to": "deliver" }]); v["nodes"]["deliver"] = json!({ "node": "deliver" });
        g = Graph::from_json(&v).unwrap(); g
    }
    #[test]
    fn a_branch_is_exported_from_a_team_with_its_ports_derived_and_it_round_trips_as_a_file() {
        let b = export(&team(), &["cue".into(), "mix".into()], &meta()).unwrap();
        assert_eq!(b["start"], json!(["cue"])); assert_eq!(b["edges"].as_array().unwrap().len(), 1, "only the edge inside the branch");
        let ins: Vec<&str> = b["inputs"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect(); assert_eq!(ins, vec!["rules_output_subtitle"], "the one value read from outside");
        assert_eq!(b["outputs"][0]["goes_to"], json!("deliver")); assert_eq!(b["nodes"]["mix"]["tool"]["args"]["spec"]["target_lufs"], json!(-14.0), "the node's settings travel whole");
        let text = to_text(&b); assert!(text.lines().nth(1).unwrap().starts_with("schema:") && text.lines().nth(2).unwrap().starts_with("name:") && !text.contains("null"), "the header comes first and no empty fields are written"); assert!(text.contains("hash: sha256:") && text.contains("text_from: values:rules.output.subtitle")); let back = from_text(&text).unwrap();
        assert_eq!(hash_of(&back), hash_of(&b)); assert_eq!(to_text(&back), text, "the same branch always gives the same file");
    }
    #[test]
    fn an_edited_or_broken_branch_file_is_refused_with_the_reason() {
        let text = to_text(&export(&team(), &["cue".into(), "mix".into()], &meta()).unwrap());
        assert!(from_text(&text.replace("gain_db: -8", "gain_db: -9")).unwrap_err().contains("hash"), "an edit without a new hash");
        assert!(from_text("name: x\n").unwrap_err().contains("hash")); assert!(export(&team(), &["nope".into()], &meta()).unwrap_err().contains("no node"));
        let mut b = export(&team(), &["cue".into(), "mix".into()], &meta()).unwrap(); b["version"] = json!("1.0"); b["license"] = json!(""); let e = check(&b).join("; "); assert!(e.contains("version") && e.contains("license"), "{e}");
        b["version"] = json!("1.0.0"); b["license"] = json!("MIT"); b["outputs"][0]["node"] = json!("ghost"); assert!(check(&b).iter().any(|m| m.contains("not in the branch")));
    }
    #[test]
    fn the_hash_ignores_the_hash_field_and_the_layout() { let b = export(&team(), &["cue".into(), "mix".into()], &meta()).unwrap(); let h = hash_of(&b); let mut c = b.clone(); c["hash"] = json!("sha256:zzz"); assert_eq!(hash_of(&c), h); c["version"] = json!("0.2.0"); assert_ne!(hash_of(&c), h); }

    #[test]
    fn using_a_branch_prefixes_its_nodes_rewrites_references_inside_it_and_leaves_the_outside_alone() {
        let b = json!({ "name": "x", "inputs": [{ "name": "title", "ref": "rules.output.title" }], "start": ["w"], "edges": [{ "from": "w", "port": "out", "to": "build" }, { "from": "build", "port": "out", "to": "ok" }],
          "nodes": { "w": { "node": "use_tool", "tool": { "type": "apply", "name": "files", "args": { "files": [{ "path": "a.txt", "content_from": "rules.output.title" }, { "path": "b.txt", "content_from": "w2.out" }] } } },
                     "build": { "node": "use_tool", "tool": { "type": "skill", "name": "run-build" } }, "ok": { "node": "if", "cond": "build.exit == 0 and rules.ok" },
                     "dec": { "node": "decompose", "contract": "build", "tests": "w", "prompt": "write the build file; the build is fine" } } });
        let i = instantiate(&b, "snd_", &Map::new(), &Map::new(), &Map::new()).unwrap();
        let mut ks: Vec<String> = i.nodes.keys().cloned().collect(); ks.sort(); assert_eq!(ks, ["snd_build", "snd_dec", "snd_ok", "snd_w"]); assert_eq!(i.start, vec!["snd_w"]); assert_eq!(i.edges[0], json!({ "from": "snd_w", "port": "out", "to": "snd_build" }));
        assert_eq!(i.nodes["snd_ok"]["cond"], json!("snd_build.exit == 0 and rules.ok"), "inside renamed, outside left");
        assert_eq!(i.nodes["snd_dec"]["contract"], json!("snd_build")); assert_eq!(i.nodes["snd_dec"]["tests"], json!("snd_w")); assert_eq!(i.nodes["snd_dec"]["prompt"], json!("write the build file; the build is fine"), "prose is never rewritten");
        assert_eq!(i.nodes["snd_w"]["tool"]["args"]["files"][0]["content_from"], json!("rules.output.title"), "an outside reference stays");
        let mut bind = Map::new(); bind.insert("title".into(), json!("values:mytitle.output.t")); let j = instantiate(&b, "", &bind, &Map::new(), &Map::new()).unwrap(); assert_eq!(j.nodes["w"]["tool"]["args"]["files"][0]["content_from"], json!("mytitle.output.t"), "a bound input replaces the outside reference");
        assert!(instantiate(&b, "9x", &Map::new(), &Map::new(), &Map::new()).is_err() && instantiate(&b, "a", &{ let mut m = Map::new(); m.insert("nope".into(), json!("x.y")); m }, &Map::new(), &Map::new()).unwrap_err().contains("no input"));
    }
    #[test]
    fn a_link_is_checked_against_its_file_and_a_changed_node_is_named() {
        let b = export(&team(), &["cue".into(), "mix".into()], &meta()).unwrap(); let inst = instantiate(&b, "snd_", &Map::new(), &Map::new(), &Map::new()).unwrap();
        let mut nodes = Value::Object(inst.nodes.clone()); assert!(drift(&b, "snd_", &Map::new(), &Map::new(), &Map::new(), &nodes).is_empty(), "unchanged");
        nodes["snd_mix"]["timeout_s"] = json!(999); assert_eq!(drift(&b, "snd_", &Map::new(), &Map::new(), &Map::new(), &nodes), vec!["snd_mix".to_string()]);
    }

    #[test]
    fn examples_run_the_testable_nodes_and_a_branch_without_a_passing_one_is_not_saved() {
        let b = export(&team(), &["cue".into(), "mix".into()], &meta()).unwrap(); let r = run_examples(&b); assert_eq!(r.len(), 2); assert!(r.iter().all(|x| x.ok), "{r:?}");
        // an example that fails, one that cannot run, none at all
        let mut m = meta(); m["examples"][0]["expect"] = json!({ "count": 3 }); assert!(export(&team(), &["cue".into(), "mix".into()], &m).unwrap_err().contains("fails: expected"));
        let mut m = meta(); m["examples"] = json!([{ "name": "mix needs a runtime", "node": "mix", "expect": {} }]); assert!(export(&team(), &["cue".into(), "mix".into()], &m).unwrap_err().contains("cannot be tested offline"));
        let mut m = meta(); m["examples"] = json!([]); assert!(export(&team(), &["cue".into(), "mix".into()], &m).unwrap_err().contains("no examples"));
        // the other testable kinds, on a branch value
        let v = json!({ "nodes": { "rules": { "node": "transform", "mode": "rules", "config": { "fields": { "title": { "case": "upper", "max": 12 } } } },
            "pick": { "node": "cell", "task": "intent", "seat": "local:qwen3:0.6b", "from": "ask", "params": { "labels": ["bug", "feature"] } },
            "twice": { "node": "code", "source": "#{ twice: input.n * 2 }" } },
          "examples": [ { "name": "rules fix the title", "node": "rules", "input": { "title": "  the burning bush of sinai " }, "expect": { "title": "THE BURNING" } },
            { "name": "the guard accepts a label inside chatter", "node": "pick", "input": "x", "reply": "I think it is a Feature.", "expect": "feature" },
            { "name": "the guard refuses a reply with no label", "node": "pick", "input": "x", "reply": "no idea", "expect_error": "no label" },
            { "name": "code runs", "node": "twice", "input": { "n": 4 }, "expect": { "twice": 8 } } ] });
        let r = run_examples(&v); assert!(r.iter().all(|x| x.ok), "{r:?}"); assert!(contains(&json!({ "a": 1.0 }), &json!({ "a": 1, "b": 2 })) && !contains(&json!({ "a": 2 }), &json!({ "a": 1 })));
    }

    #[test]
    fn parameters_and_model_seats_are_exported_resolved_with_defaults_and_refused_when_wrong() {
        let mut m = meta(); m["params"] = json!({ "target_lufs": { "type": "number", "description": "loudness of the mix", "paths": ["mix.tool.args.spec.target_lufs"] }, "voice_start": { "type": "number", "paths": ["mix.tool.args.spec.narration.start_s"] } });
        m["seats"] = json!({ "embedding": "local:nomic-embed-text" }); m["seat_for"] = json!({ "embedding": "choosing sounds by similarity" });
        let mut g = team().to_json(); g["nodes"]["mix"]["tool"]["args"]["spec"]["embed_seat"] = json!("local:nomic-embed-text"); let g = Graph::from_json(&g).unwrap();
        let b = export(&g, &["cue".into(), "mix".into()], &m).unwrap();
        assert_eq!(b["nodes"]["mix"]["tool"]["args"]["spec"]["target_lufs"], json!("${param.target_lufs}"), "the value became a placeholder"); assert_eq!(b["params"]["target_lufs"]["default"], json!(-14.0), "its old value is the default"); assert!(b["params"]["target_lufs"].get("paths").is_none());
        assert_eq!(b["nodes"]["mix"]["tool"]["args"]["spec"]["embed_seat"], json!("${model.embedding}")); assert_eq!(b["requires"]["models"][0], json!({ "name": "embedding", "default": "local:nomic-embed-text", "for": "choosing sounds by similarity" }));
        // the examples and the checks see the defaults; the file round-trips with its placeholders
        assert!(run_examples(&b).iter().all(|x| x.ok)); let back = from_text(&to_text(&b)).unwrap(); assert_eq!(hash_of(&back), hash_of(&b));
        // using it: defaults, then supplied values and a mapped seat
        let d = instantiate(&b, "s_", &Map::new(), &Map::new(), &Map::new()).unwrap(); assert_eq!(d.nodes["s_mix"]["tool"]["args"]["spec"]["target_lufs"], json!(-14.0), "a whole-string placeholder keeps its type"); assert_eq!(d.nodes["s_mix"]["tool"]["args"]["spec"]["embed_seat"], json!("local:nomic-embed-text"));
        let mut pr = Map::new(); pr.insert("target_lufs".into(), json!(-16)); let mut se = Map::new(); se.insert("embedding".into(), json!("local:bge-small"));
        let u = instantiate(&b, "s_", &Map::new(), &pr, &se).unwrap(); assert_eq!(u.nodes["s_mix"]["tool"]["args"]["spec"]["target_lufs"], json!(-16)); assert_eq!(u.nodes["s_mix"]["tool"]["args"]["spec"]["embed_seat"], json!("local:bge-small"));
        assert!(resolve(&b, &{ let mut x = Map::new(); x.insert("target_lufs".into(), json!("loud")); x }, &Map::new()).unwrap_err().contains("is a number")); assert!(resolve(&b, &{ let mut x = Map::new(); x.insert("nope".into(), json!(1)); x }, &Map::new()).unwrap_err().contains("no parameter"));
        assert!(resolve(&b, &Map::new(), &{ let mut x = Map::new(); x.insert("gpu".into(), json!("local:x")); x }).unwrap_err().contains("requires no model")); assert!(resolve(&b, &Map::new(), &{ let mut x = Map::new(); x.insert("embedding".into(), json!("banana")); x }).unwrap_err().contains("the seat"));
        // an undeclared placeholder, a wrong default and a missing path are caught
        let mut bad = b.clone(); bad["nodes"]["cue"]["tool"]["args"]["file"] = json!("${param.ghost}"); assert!(check(&bad).iter().any(|x| x.contains("ghost") && x.contains("not declared")));
        let mut bad = b.clone(); bad["params"]["voice_start"]["default"] = json!("late"); assert!(check(&bad).iter().any(|x| x.contains("the default is not a number")));
        let mut m2 = meta(); m2["params"] = json!({ "x": { "type": "number", "paths": ["mix.nope.nothing"] } }); assert!(export(&team(), &["cue".into(), "mix".into()], &m2).unwrap_err().contains("nothing at"));
        // embedded in a longer string a placeholder is written as text
        assert_eq!(substitute(&json!({ "p": "ask ${param.n} times in ${model.m}" }), &{ let mut x = Map::new(); x.insert("n".into(), json!(3)); x }, &{ let mut x = Map::new(); x.insert("m".into(), json!("local:a")); x }).unwrap(), json!({ "p": "ask 3 times in local:a" }));
    }

    #[test]
    fn a_branch_name_can_never_leave_the_library() {
        assert!(library_path("/w", "sound-narrated-shot").unwrap().ends_with(".antiloki/branches/sound-narrated-shot.yaml"));
        for bad in ["../x", "../../etc/passwd", "a/b", "a\\b", "", "UPPER", ".hidden", "-x", "x.yaml", "a b", "x%2e%2e"] { assert!(library_path("/w", bad).is_err(), "{bad:?}"); }
    }
}
