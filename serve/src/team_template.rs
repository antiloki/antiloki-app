//! TEAM TEMPLATES (RFC 0017, operator 2026-11-20): a template is a WHOLE TEAM in the branch file format (team_branch: strict YAML, `antiloki-branch/1`, hash over the canonical JSON) with one added header key,
//! `template: { kind: team, category, status, ask, needs, cost_note, lineage }`. They come from two places: SHIPPED with antiloki (embedded at build time, read-only) and the person's own library
//! (`.antiloki/templates/*.yaml`, made by Share as template). Starting one creates an ordinary team that is the person's own; sharing a team checks it first and never carries a secret, a private path
//! or a standing power (triggers, limits, setpoints, the thread's branch). All of this module is pure except `read_library`/`write_library`.
use crate::team_branch;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// The templates that ship with antiloki: (name, file text). Read-only.
pub const SHIPPED: &[(&str, &str)] = &[
    ("recreate-webapp", include_str!("../../templates/teams/recreate-webapp.yaml")),
    ("seo-audit", include_str!("../../templates/teams/seo-audit.yaml")),
    ("marketing-campaign", include_str!("../../templates/teams/marketing-campaign.yaml")),
    ("lead-capture", include_str!("../../templates/teams/lead-capture.yaml")),
    ("sales-crm", include_str!("../../templates/teams/sales-crm.yaml")),
    ("bug-hunter", include_str!("../../templates/teams/bug-hunter.yaml")),
    ("pane-webapp", include_str!("../../templates/teams/pane-webapp.yaml")),
    ("pane-deterministic", include_str!("../../templates/teams/pane-deterministic.yaml")),
];
pub const CATEGORIES: [&str; 7] = ["recreate", "seo", "marketing", "leads", "coding", "sales", "other"];

pub fn library_dir(root: &str) -> PathBuf { Path::new(root).join(".antiloki").join("templates") }
pub fn library_path(root: &str, name: &str) -> Result<PathBuf, String> { team_branch::library_path(root, name).map(|_| library_dir(root).join(format!("{name}.yaml"))) }

/// What a card needs, from the template header (strings only, bounded). Pure.
fn strings(v: &Value) -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.chars().take(40).collect()).take(12).collect()).unwrap_or_default() }

/// One row of the list for a file's text. A file that does not read (a wrong hash, a bad graph) is a row with `valid: false` and the reason, never a missing row. Pure.
pub fn row(name_hint: &str, text: &str, source: &str) -> Value {
    match team_branch::from_text(text) {
        Ok(v) => { let t = &v["template"]; json!({ "name": v["name"], "version": v["version"], "description": v["description"], "category": t["category"].as_str().filter(|c| CATEGORIES.contains(c)).unwrap_or("other"),
            "status": t["status"].as_str().filter(|c| ["verified", "draft", "community"].contains(c)).unwrap_or("draft"), "source": source, "valid": v["template"]["kind"] == json!("team"), "error": if v["template"]["kind"] == json!("team") { "" } else { "the file is a branch, not a team template (`template.kind` is `team`)" },
            "hash": v["hash"], "needs": strings(&t["needs"]), "cost_note": t["cost_note"].as_str().unwrap_or(""), "ask": t["ask"].as_str().unwrap_or("") }) }
        Err(e) => json!({ "name": name_hint, "version": "", "description": "", "category": "other", "status": "draft", "source": source, "valid": false, "error": e, "hash": "", "needs": [], "cost_note": "", "ask": "" }),
    }
}
/// The library file texts: (name, text). Missing folder = none.
pub fn read_library(root: &str) -> Vec<(String, String)> {
    crate::docstore::import_dir(root, "template", &library_dir(root), &["yaml"]);   // files from before are imported once; after that the row is the template
    crate::docstore::ids(root, "template").into_iter().filter_map(|id| crate::docstore::get_text(root, "template", &id).map(|t| (id, t))).collect()
}
/// Every template, shipped first. A library file with a shipped name is listed as invalid (a shipped template is never replaced). Pure given its inputs.
pub fn list(shipped: &[(&str, &str)], library: &[(String, String)]) -> Vec<Value> {
    let mut rows: Vec<Value> = shipped.iter().map(|(n, t)| row(n, t, "shipped")).collect();
    for (n, t) in library { let mut r = row(n, t, "yours"); if shipped.iter().any(|(s, _)| s == n) { r["valid"] = json!(false); r["error"] = json!(format!("`{n}` is the name of a template that ships with antiloki: rename yours")); } rows.push(r); }
    rows
}
/// The text of a template by name: the library first would let a file replace a shipped one, so the SHIPPED text wins. Pure given its inputs.
pub fn find(shipped: &[(&str, &str)], library: &[(String, String)], name: &str) -> Option<String> {
    shipped.iter().find(|(n, _)| *n == name).map(|(_, t)| t.to_string()).or_else(|| library.iter().find(|(n, _)| n == name).map(|(_, t)| t.clone()))
}

// ── the checks before a team is shared ───────────────────────────────────────────────────────────────────────────────────────────────────────

/// Where a secret-looking string is (a path into the graph and what kind), never the string itself. Pure.
pub fn secrets_in(v: &Value) -> Vec<String> {
    use regex::Regex; use std::sync::OnceLock;
    static RES: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    let res = RES.get_or_init(|| vec![
        ("a GitHub token", Regex::new(r"\b(ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}").unwrap()),
        ("an API key (sk-)", Regex::new(r"\bsk-[A-Za-z0-9_-]{20,}").unwrap()),
        ("a Linear key", Regex::new(r"\blin_(api|oauth)_[A-Za-z0-9]{16,}").unwrap()),
        ("a Slack token", Regex::new(r"\bxox[abprs]-[A-Za-z0-9-]{10,}").unwrap()),
        ("an AWS key", Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap()),
        ("a private key", Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").unwrap()),
        ("a bearer token", Regex::new(r"(?i)\bbearer\s+[A-Za-z0-9._~+/-]{24,}").unwrap()),
        ("a secret next to its name", Regex::new(r#"(?i)(token|secret|password|api[_-]?key)["']?\s*[:=]\s*["']?[A-Za-z0-9+/_-]{24,}"#).unwrap()),
    ]);
    fn walk(v: &Value, path: String, res: &[(&'static str, Regex)], out: &mut Vec<String>) {
        match v { Value::String(s) => { for (kind, re) in res { if re.is_match(s) { out.push(format!("{path}: looks like {kind}")); break; } } }
            Value::Array(a) => a.iter().enumerate().for_each(|(i, x)| walk(x, format!("{path}[{i}]"), res, out)),
            Value::Object(m) => m.iter().for_each(|(k, x)| walk(x, if path.is_empty() { k.clone() } else { format!("{path}.{k}") }, res, out)), _ => {} }
    }
    let mut out = vec![]; walk(v, String::new(), res, &mut out); out
}
/// Where a string names a place on THIS machine or outside the project: an absolute path, `~`, a Windows drive, a `..` step. A URL is not a path. Pure.
pub fn private_paths_in(v: &Value) -> Vec<String> {
    fn bad(s: &str) -> bool { let t = s.trim(); if t.contains("://") || t.contains('\n') { return false; }
        t.starts_with("/home/") || t.starts_with("/Users/") || t.starts_with("/root/") || t.starts_with("~/") || t == "~" || t.starts_with("C:\\") || t.starts_with("c:\\") || t.starts_with("C:/") || t.contains("/../") || t.starts_with("../") || t.ends_with("/..") }
    fn walk(v: &Value, path: String, out: &mut Vec<String>) {
        match v { Value::String(s) => { if bad(s) { out.push(format!("{path}: names a place on this machine or outside the project")); } }
            Value::Array(a) => a.iter().enumerate().for_each(|(i, x)| walk(x, format!("{path}[{i}]"), out)),
            Value::Object(m) => m.iter().for_each(|(k, x)| walk(x, if path.is_empty() { k.clone() } else { format!("{path}.{k}") }, out)), _ => {} }
    }
    let mut out = vec![]; walk(v, String::new(), &mut out); out
}
/// A team's graph JSON without what only the person sets on their own team: the thread's branch, triggers (schedules), limits, setpoints. Returns the cleaned graph and a line for each thing that was reset. Pure.
pub fn strip_powers(g: &Value) -> (Value, Vec<String>) {
    let mut g = g.clone(); let mut reset = vec![]; let Some(m) = g.as_object_mut() else { return (g, reset) };
    if m.remove("branch").map_or(false, |b| !b.as_str().unwrap_or("").is_empty()) { reset.push("the thread's branch (a started team gets its own)".into()); }
    if m.remove("triggers").map_or(false, |t| t.as_array().map_or(false, |a| !a.is_empty())) { reset.push("its triggers (a team never starts on its own until you set that)".into()); }
    if m.remove("limits").map_or(false, |l| !l.is_null()) { reset.push("its daily limits".into()); }
    if m.remove("setpoints").map_or(false, |s| s.as_object().map_or(false, |o| !o.is_empty())) { reset.push("its setpoints".into()); }
    (g, reset)
}
/// The template file value for a team: the branch header + the template header + the cleaned body. `status` is `verified` only when `proven_by` names a successful run. Pure.
pub fn build(g: &Value, name: &str, description: &str, category: &str, proven_by: Option<&str>, lineage_team: &str, needs: &[String], ask: &str, cost_note: &str) -> Value {
    let (clean, _) = strip_powers(g); let nodes = clean["nodes"].clone(); let edges = clean["edges"].clone(); let start = clean["start"].clone();
    let mut t = Map::new(); t.insert("kind".into(), json!("team")); t.insert("category".into(), json!(if CATEGORIES.contains(&category) { category } else { "other" }));
    t.insert("status".into(), json!(if proven_by.is_some() { "verified" } else { "draft" })); if !ask.is_empty() { t.insert("ask".into(), json!(ask)); } if !cost_note.is_empty() { t.insert("cost_note".into(), json!(cost_note)); }
    if !needs.is_empty() { t.insert("needs".into(), json!(needs)); } if let Some(r) = proven_by { t.insert("proven_by_run".into(), json!(r)); } t.insert("lineage".into(), json!({ "team": lineage_team }));
    json!({ "schema": team_branch::SCHEMA, "name": name, "version": "1.0.0", "description": description, "license": "proprietary", "template": Value::Object(t),
        "settings": { "budget_usd": clean["budget_usd"].as_f64().unwrap_or(6.0) }, "start": start, "nodes": nodes, "edges": edges })
}
/// What a team uses, from its graph: the kinds of tool nodes and whether a human is in the loop. Pure.
pub fn needs_of(g: &Value) -> Vec<String> {
    let mut n: Vec<String> = vec![]; let mut add = |x: &str| if !n.iter().any(|y| y == x) { n.push(x.to_string()); };
    if let Some(nodes) = g["nodes"].as_object() { for v in nodes.values() { if v["node"] == json!("human") { add("a human step"); } if v["node"] == json!("use_tool") { if let Some(t) = v["tool"]["type"].as_str() { add(t); } } } }
    n
}

#[cfg(test)]
mod tests {
    /// Re-stamp a shipped template after a hand edit of its name or text: `cargo test -p antiloki-serve --lib restamp_shipped -- --ignored`. Never part of a normal run.
    #[test] #[ignore]
    fn restamp_shipped() {
        for n in ["pane-webapp", "pane-deterministic", "bug-hunter"] {
            let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("templates").join("teams").join(format!("{n}.yaml")); let t = std::fs::read_to_string(&p).unwrap();
            let mut v = crate::team_yaml::parse_strict(&t).unwrap(); let h = team_branch::hash_of(&v); v["hash"] = json!(h); std::fs::write(&p, team_branch::to_text(&v)).unwrap();
        }
    }
    use super::*;
    use crate::team_graph::template as builtin;
    fn graph() -> Value { builtin("feature", "demo", 6.0).unwrap().to_json() }
    fn file(name: &str, status_run: Option<&str>) -> String { let v = build(&graph(), name, "plans, codes, reviews, lands", "coding", status_run, "demo", &["a human step".into()], "what to build", ""); team_branch::to_text(&v) }
    #[test]
    fn a_built_template_round_trips_through_the_strict_reader_and_keeps_its_header() {
        let t = file("my-feature", None); let v = team_branch::from_text(&t).unwrap(); assert_eq!(v["template"]["kind"], json!("team")); assert_eq!(v["template"]["status"], json!("draft")); assert_eq!(v["template"]["category"], json!("coding")); assert_eq!(v["template"]["needs"], json!(["a human step"]));
        let r = row("x", &t, "yours"); assert_eq!(r["valid"], json!(true)); assert_eq!(r["name"], json!("my-feature")); assert_eq!(r["status"], json!("draft")); assert_eq!(r["needs"], json!(["a human step"]));
        assert_eq!(row("x", &file("proven", Some("run-1")), "yours")["status"], json!("verified"));
    }
    #[test]
    fn a_file_edited_by_hand_is_listed_invalid_with_the_reason_and_never_disappears() {
        let t = file("my-feature", None).replace("my-feature", "my-feature2"); let r = row("my-feature", &t, "yours"); assert_eq!(r["valid"], json!(false)); assert!(r["error"].as_str().unwrap().contains("hash"), "{r}"); assert_eq!(r["name"], json!("my-feature"));
    }
    #[test]
    fn a_plain_branch_is_not_a_team_template() {
        let mut v = build(&graph(), "br", "d", "coding", None, "demo", &[], "", ""); v.as_object_mut().unwrap().remove("template"); let r = row("br", &team_branch::to_text(&v), "yours"); assert_eq!(r["valid"], json!(false)); assert!(r["error"].as_str().unwrap().contains("team template"));
    }
    #[test]
    fn a_library_file_cannot_replace_a_shipped_template() {
        let shipped = [("recreate-webapp", "unused")]; let lib = vec![("recreate-webapp".to_string(), file("recreate-webapp", None))]; let rows = list(&shipped, &lib);
        assert_eq!(rows.len(), 2); assert_eq!(rows[1]["valid"], json!(false)); assert!(rows[1]["error"].as_str().unwrap().contains("ships with antiloki")); assert_eq!(find(&shipped, &lib, "recreate-webapp").as_deref(), Some("unused"));
    }
    #[test]
    fn secrets_are_found_by_where_they_are_and_never_repeated() {
        let g = json!({ "nodes": { "a": { "prompt": "use ghp_abcdefghijklmnopqrstuvwxyz0123456789 to log in", "b": ["sk-abcdefghijklmnopqrstuvwxyz", "fine"] }, "c": { "env": "token=AbCdEfGhIjKlMnOpQrStUvWxYz012345" } } });
        let s = secrets_in(&g); assert_eq!(s.len(), 3, "{s:?}"); assert!(s.iter().any(|x| x.starts_with("nodes.a.prompt") && x.contains("GitHub token"))); assert!(s.iter().all(|x| !x.contains("ghp_abc") && !x.contains("sk-abc") && !x.contains("AbCdEf")));
        assert!(secrets_in(&json!({ "n": "an ordinary sentence about tokens and passwords" })).is_empty());
    }
    #[test]
    fn private_paths_are_refused_but_a_url_and_a_relative_path_are_fine() {
        let g = json!({ "a": "/home/user/dev/x", "b": "~/notes", "c": "../outside", "d": "C:\\Users\\me", "e": "https://github.com/a/b", "f": "src/ui/**", "g": ".antiloki/browse-targets/x.json" });
        let p = private_paths_in(&g); assert_eq!(p.len(), 4, "{p:?}"); assert!(p.iter().all(|x| !x.starts_with("e:") && !x.starts_with("f:") && !x.starts_with("g:")));
    }
    #[test]
    fn standing_powers_are_taken_off_and_named() {
        let mut g = graph(); g["branch"] = json!("thread-abc"); g["triggers"] = json!([{ "kind": "schedule", "cron": "0 9 * * *" }]); g["limits"] = json!({ "per_day": 3 }); g["setpoints"] = json!({ "coder": { "x": 1 } });
        let (c, reset) = strip_powers(&g); for k in ["branch", "triggers", "limits", "setpoints"] { assert!(c.get(k).is_none(), "{k}"); } assert_eq!(reset.len(), 4, "{reset:?}");
        let mut plain = graph(); plain["branch"] = json!(""); let (_, none) = strip_powers(&plain); assert!(none.is_empty(), "nothing to reset on a plain team: {none:?}");
    }
    #[test]
    fn a_started_template_validates_as_a_team() {
        let v = team_branch::from_text(&file("my-feature", None)).unwrap(); let gj = json!({ "team": "new", "schema": 1, "budget_usd": 6.0, "start": v["start"], "nodes": v["nodes"], "edges": v["edges"] });
        let g = crate::team_graph::Graph::from_json(&gj).unwrap(); assert!(crate::team_graph::validate(&g).errors.is_empty());
    }
    #[test]
    fn every_shipped_template_reads_validates_and_carries_no_secret_or_private_path() {
        let rows = list(SHIPPED, &[]); assert!(!rows.is_empty()); let mut names = std::collections::BTreeSet::new();
        for (n, t) in SHIPPED { let r = row(n, t, "shipped"); assert_eq!(r["valid"], json!(true), "{n}: {r}"); assert_eq!(r["name"], json!(n), "the file name and the template name agree"); assert!(names.insert(n.to_string()), "{n} twice");
            let v = team_branch::from_text(t).unwrap(); assert!(secrets_in(&v).is_empty(), "{n}: {:?}", secrets_in(&v)); assert!(private_paths_in(&v).is_empty(), "{n}: {:?}", private_paths_in(&v));
            let gj = json!({ "team": "x", "schema": 1, "budget_usd": 6.0, "start": v["start"], "nodes": v["nodes"], "edges": v["edges"] }); assert!(crate::team_graph::validate(&crate::team_graph::Graph::from_json(&gj).unwrap()).errors.is_empty(), "{n} does not validate"); }
    }
}
