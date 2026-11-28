//! DETERMINISTIC LANES (RFC 0018, 2026-11-21): the constitution for ANY artifact.
//!
//! The operator: "the book constitution shouldn't be just for coding … the leader will make the laws, the system will run that, and that feeds all X-ray widgets … we have deterministic nodes but not deterministic lanes."
//! A **reader** turns an artifact (markdown, copy, HTML, JSON/YAML/TOML, CSV, an image, an SVG, code) into facts; a **law** (data: YAML or JSON) is a detector plus its parameters, a severity and an axis; a **pack** is a named set of laws for a kind of work;
//! the **judge** runs the ratified laws of the packs that apply to each artifact and returns ONE report shape that the team graph (a `law-gate` step: pass, repair loop, human gate), the X-ray widgets and the telemetry all read.
//!
//! Determinism is the whole point and is tested: the same bytes and the same law versions give the same report (and the same `digest`) in any file order, on any machine. No clock, no locale, no network, no model, no randomness.
//! The model proposes, the code judges, a person ratifies: a law gates only when it is `ratified`, and a law cannot be ratified without a passing and a failing example that the engine itself has checked (`test_law`).
pub mod describe;
pub mod detect;
pub mod readers;

use detect::Ctx;
use readers::Facts;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const SEVERITIES: [&str; 4] = ["info", "warn", "error", "critical"];
pub const STATUSES: [&str; 3] = ["candidate", "ratified", "suspended"];
const SEED: [(&str, &str); 9] = [
    ("technical-docs", include_str!("../../../templates/book/technical-docs.yaml")), ("marketing-copy", include_str!("../../../templates/book/marketing-copy.yaml")), ("seo-report", include_str!("../../../templates/book/seo-report.yaml")),
    ("project-plan", include_str!("../../../templates/book/project-plan.yaml")), ("data-csv", include_str!("../../../templates/book/data-csv.yaml")), ("html-page", include_str!("../../../templates/book/html-page.yaml")), ("brand-images", include_str!("../../../templates/book/brand-images.yaml")), ("code-docs", include_str!("../../../templates/book/code-docs.yaml")), ("copy-docs", include_str!("../../../templates/book/copy-docs.yaml")),
];

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Fixture { #[serde(default)] pub name: String, #[serde(default)] pub kind: String, #[serde(default)] pub path: String, #[serde(default)] pub text: String, /** base64 bytes, for a fixture that is not text (an image) */ #[serde(default)] pub b64: String, /** how many hits a failing fixture must give (default 1) */ #[serde(default)] pub expect: Option<usize> }
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Fixtures { #[serde(default)] pub pass: Vec<Fixture>, #[serde(default)] pub fail: Vec<Fixture> }
fn d_warn() -> String { "warn".into() } fn d_five() -> u32 { 5 } fn d_one() -> f64 { 1.0 } fn d_rat() -> String { "ratified".into() } fn d_v() -> u32 { 1 }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Law {
    pub key: String, pub title: String, #[serde(default)] pub domain: String, #[serde(default)] pub kinds: Vec<String>, #[serde(default)] pub paths: Vec<String>, pub detector: String, #[serde(default)] pub config: Value,
    #[serde(default = "d_warn")] pub severity: String, #[serde(default = "d_five")] pub weight: u32, #[serde(default)] pub axis: String, #[serde(default = "d_one")] pub confidence: f64,
    #[serde(default = "d_rat")] pub status: String, /** `check` (the default) judges and may gate; `doc` documents: its hits become sections of the file's generated documentation, never findings */ #[serde(default)] pub purpose: String, #[serde(default)] pub rule: String, #[serde(default)] pub fix: String, #[serde(default = "d_v")] pub version: u32, #[serde(default)] pub fixtures: Fixtures, #[serde(default)] pub created_by: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Pack { pub id: String, #[serde(default)] pub name: String, #[serde(default)] pub description: String, #[serde(default)] pub axes: Vec<String>, #[serde(default)] pub kinds: Vec<String>, #[serde(default)] pub paths: Vec<String>, /** paths this pack never judges (a README's laws are not a landing page's) */ #[serde(default)] pub exclude: Vec<String>, #[serde(default = "d_v")] pub version: u32, #[serde(default)] pub laws: Vec<Law> }

fn slug_ok(k: &str) -> bool { !k.is_empty() && k.len() <= 64 && k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') }
/// Is this law one the engine can run? Says why not, in words. Pure.
pub fn compile(l: &Law) -> Result<(), String> {
    if !slug_ok(&l.key) { return Err(format!("the key `{}` must be 1 to 64 characters of a-z, 0-9 and -", l.key)); }
    if l.title.trim().is_empty() || l.title.chars().count() > 160 { return Err("a law needs a title of 1 to 160 characters".into()); }
    if !SEVERITIES.contains(&l.severity.as_str()) { return Err(format!("severity is one of {}", SEVERITIES.join(", "))); }
    if !STATUSES.contains(&l.status.as_str()) { return Err(format!("status is one of {}", STATUSES.join(", "))); }
    if !matches!(l.purpose.as_str(), "" | "check" | "doc") { return Err("purpose is `check` or `doc`".into()); }
    if l.weight > 10 { return Err("weight is 0 to 10".into()); } if !(0.0..=1.0).contains(&l.confidence) { return Err("confidence is 0 to 1".into()); }
    for k in &l.kinds { if !readers::KINDS.contains(&k.as_str()) { return Err(format!("`{k}` is not an artifact kind ({})", readers::KINDS.join(", "))); } }
    if !detect::DETECTORS.contains(&l.detector.as_str()) { return Err(format!("`{}` is not a detector ({})", l.detector, detect::DETECTORS.join(", "))); }
    let c = &l.config;
    match l.detector.as_str() {
        "pattern" => { detect::pattern_regex(c)?; if !matches!(c["mode"].as_str().unwrap_or("forbid"), "forbid" | "require") { return Err("pattern mode is `forbid` or `require`".into()); } }
        "count" => { if c["attr"].as_str().unwrap_or("").is_empty() { return Err("a count law needs `attr`".into()); } if c.get("min").and_then(Value::as_f64).is_none() && c.get("max").and_then(Value::as_f64).is_none() { return Err("a count law needs `min` or `max`".into()); } }
        "expr" => detect::expr_compiles(c["expr"].as_str().unwrap_or(""))?,
        "structure" => { let f = Facts::default(); detect::run("structure", c, &f, &Ctx { files: &BTreeSet::new(), all: &[] }).map(|_| ())?; }
        "cross" => { if c["for"].as_str().unwrap_or("").is_empty() || c["needs"].as_array().map_or(true, |a| a.is_empty()) { return Err("a cross law needs `for` and `needs`".into()); } }
        _ => {}
    }
    Ok(())
}
/// A pack from its YAML or JSON text, every law checked. Pure.
pub fn parse_pack(text: &str) -> Result<Pack, String> {
    let v = readers::parse_yaml(text)?; let p: Pack = serde_json::from_value(v).map_err(|e| format!("not a pack: {e}"))?;
    if !slug_ok(&p.id) { return Err(format!("the pack id `{}` must be 1 to 64 characters of a-z, 0-9 and -", p.id)); }
    let mut keys = BTreeSet::new(); for l in &p.laws { compile(l).map_err(|e| format!("law `{}`: {e}", l.key))?; if !keys.insert(l.key.clone()) { return Err(format!("the law key `{}` is used twice", l.key)); } }
    Ok(p)
}
/// The packs shipped with antiloki. Pure.
pub fn seed_packs() -> Vec<Pack> { SEED.iter().map(|(id, t)| parse_pack(t).unwrap_or_else(|e| panic!("the shipped pack `{id}` is broken: {e}"))).collect() }
fn book_dir(root: &str) -> std::path::PathBuf { std::path::Path::new(root).join(".antiloki").join("book") }
/// The book of a workspace: the shipped packs, then the person's own (`.antiloki/book/*.yaml|json`); a pack of the same id in the workspace REPLACES the shipped one. A file that does not parse is reported, never silently dropped. Blocking.
pub fn load_book(root: &str) -> (Vec<Pack>, Vec<String>) {
    let mut by: BTreeMap<String, Pack> = seed_packs().into_iter().map(|p| (p.id.clone(), p)).collect(); let mut problems = vec![];
    problems.extend(crate::docstore::import_dir(root, "pack", &book_dir(root), &["yaml", "yml", "json"]));   // files from before become rows once; the row is the pack
    for id in crate::docstore::ids(root, "pack") { match crate::docstore::get_text(root, "pack", &id).ok_or("unreadable".to_string()).and_then(|t| parse_pack(&t)) { Ok(pk) => { by.insert(pk.id.clone(), pk); } Err(e) => problems.push(format!("{id}: {e}")) } }
    (by.into_values().collect(), problems)
}
/// Save a pack into the workspace's book (pretty JSON, which is YAML too). A law that is not ratified stays a candidate. Blocking.
pub fn save_pack(root: &str, p: &Pack) -> Result<String, String> {
    let text = serde_json::to_string(p).map_err(|e| e.to_string())?; parse_pack(&text)?; crate::docstore::put_text(root, "pack", &p.id, &text)?; Ok(format!("{}/antiloki.db (pack {})", std::path::Path::new(root).join(".antiloki").display(), p.id))
}

// ── judging ─────────────────────────────────────────────────────────────────────────────────
#[derive(Clone, Debug)]
pub struct Artifact { pub path: String, pub bytes: Vec<u8> }
#[derive(Clone, Debug)]
pub struct Opts { /** run these pack ids only (empty = every pack whose kinds and paths match) */ pub packs: Vec<String>, /** also run candidate laws, as advice (never scored, never gating) */ pub candidates: bool, /** `info|warn|error|critical`: the lowest severity that fails the gate */ pub fail_on: String }
impl Default for Opts { fn default() -> Self { Opts { packs: vec![], candidates: false, fail_on: "error".into() } } }
fn sev_rank(s: &str) -> usize { SEVERITIES.iter().position(|x| *x == s).unwrap_or(1) }
fn sev_pen(s: &str) -> f64 { match s { "info" => 0.0, "warn" => 4.0, "error" => 12.0, _ => 30.0 } }
fn r2(x: f64) -> f64 { (x * 100.0).round() / 100.0 }
fn matches_scope(kinds: &[String], paths: &[String], kind: &str, path: &str) -> bool { (kinds.is_empty() || kinds.iter().any(|k| k == "any" || k == kind)) && (paths.is_empty() || paths.iter().any(|g| detect::glob_match(g, path))) }

fn sha_of(path: &str, arts: &[&Artifact]) -> String { use sha2::{Digest, Sha256}; arts.iter().find(|a| a.path == path).map(|a| hex::encode(Sha256::digest(&a.bytes))[..16].to_string()).unwrap_or_default() }
/// Judge artifacts against packs. Pure and deterministic: artifacts are taken in path order, every map is ordered, scores are rounded to two places, and the report carries a digest of itself.
pub fn judge(packs: &[Pack], artifacts: &[Artifact], o: &Opts) -> Value {
    let mut arts: Vec<&Artifact> = artifacts.iter().collect(); arts.sort_by(|a, b| a.path.cmp(&b.path)); arts.dedup_by(|a, b| a.path == b.path);
    let facts: Vec<Facts> = arts.iter().map(|a| readers::read(&a.path, &a.bytes)).collect(); let files: BTreeSet<String> = facts.iter().map(|f| f.path.clone()).collect(); let ctx = Ctx { files: &files, all: &facts };
    let mut packs: Vec<&Pack> = packs.iter().filter(|p| o.packs.is_empty() || o.packs.contains(&p.id)).collect(); packs.sort_by(|a, b| a.id.cmp(&b.id));
    let (mut findings, mut art_rows, mut skipped, mut unread): (Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>) = (vec![], vec![], vec![], vec![]); let mut laws_run: BTreeSet<String> = BTreeSet::new();
    let mut axis_sum: BTreeMap<String, (f64, usize)> = BTreeMap::new(); let mut counts: BTreeMap<&str, usize> = SEVERITIES.iter().map(|s| (*s, 0)).collect(); let mut advisory = 0usize; let mut score_sum = 0.0; let mut scored = 0usize;
    for f in &facts {
        if let Some(why) = &f.unread { unread.push(json!({ "path": f.path, "kind": f.kind, "why": why })); art_rows.push(json!({ "path": f.path, "kind": f.kind, "sha": sha_of(&f.path, &arts), "score": Value::Null, "findings": 0, "unread": why })); continue; }
        let mut pen_total = 0.0; let mut axis_pen: BTreeMap<String, f64> = BTreeMap::new(); let mut n_find = 0usize; let mut any_law = false; let mut applied_axes: BTreeSet<String> = BTreeSet::new();
        for p in &packs { if !o.packs.contains(&p.id) && !matches_scope(&p.kinds, &p.paths, &f.kind, &f.path) { continue; } if p.exclude.iter().any(|g| detect::glob_match(g, &f.path)) && !o.packs.contains(&p.id) { continue; }
            for l in &p.laws {
                if l.purpose == "doc" || l.status == "suspended" || (l.status == "candidate" && !o.candidates) { continue; } if !matches_scope(&l.kinds, &l.paths, &f.kind, &f.path) { continue; }
                any_law = true; laws_run.insert(format!("{}/{}@{}", p.id, l.key, l.version)); if l.status == "ratified" { applied_axes.insert(if l.axis.is_empty() { l.domain.clone() } else { l.axis.clone() }); }
                let hits = match detect::run(&l.detector, &l.config, f, &ctx) { Ok(h) => h, Err(e) => { skipped.push(json!({ "pack": p.id, "law": l.key, "path": f.path, "why": e })); continue; } };
                let adv = l.status == "candidate"; let total_hits = hits.len();
                for h in hits.into_iter().take(50) {
                    let ax = if l.axis.is_empty() { l.domain.clone() } else { l.axis.clone() };
                    findings.push(json!({ "law": l.key, "version": l.version, "pack": p.id, "title": l.title, "domain": l.domain, "axis": ax, "severity": l.severity, "advisory": adv, "path": f.path, "kind": f.kind, "line": h.line, "unit": h.unit, "message": h.message, "evidence": { "value": h.value, "expected": h.expected, "excerpt": h.excerpt }, "fix": l.fix }));
                    if adv { advisory += 1; continue; } n_find += 1; *counts.get_mut(l.severity.as_str()).unwrap() += 1; let pen = sev_pen(&l.severity) * (l.weight as f64 / 5.0) * l.confidence; pen_total += pen; *axis_pen.entry(if l.axis.is_empty() { l.domain.clone() } else { l.axis.clone() }).or_insert(0.0) += pen;
                }
                if total_hits > 50 { skipped.push(json!({ "pack": p.id, "law": l.key, "path": f.path, "why": format!("{} more finding(s) not listed", total_hits - 50) })); }
            } }
        if any_law { let sc = r2((100.0 - pen_total).max(0.0)); score_sum += sc; scored += 1;
            for ax in &applied_axes { let e = axis_sum.entry(ax.clone()).or_insert((0.0, 0)); e.0 += (100.0 - axis_pen.get(ax).copied().unwrap_or(0.0)).max(0.0); e.1 += 1; }
            art_rows.push(json!({ "path": f.path, "kind": f.kind, "sha": sha_of(&f.path, &arts), "score": sc, "findings": n_find })); }
        else { art_rows.push(json!({ "path": f.path, "kind": f.kind, "sha": sha_of(&f.path, &arts), "score": Value::Null, "findings": 0, "note": "no law applies to this kind of file" })); }
    }
    findings.sort_by(|a, b| (a["path"].as_str(), a["line"].as_u64(), a["law"].as_str(), a["unit"].as_str(), a["message"].as_str()).cmp(&(b["path"].as_str(), b["line"].as_u64(), b["law"].as_str(), b["unit"].as_str(), b["message"].as_str())));
    let floor = sev_rank(&o.fail_on); let failing = findings.iter().filter(|f| f["advisory"] != json!(true) && sev_rank(f["severity"].as_str().unwrap_or("warn")) >= floor).count();
    let axes: BTreeMap<String, f64> = axis_sum.into_iter().filter(|(_, (_, n))| *n > 0).map(|(k, (s, n))| (k, r2(s / n as f64))).collect();
    let overall = if scored == 0 { Value::Null } else { json!(r2(score_sum / scored as f64)) };
    // "no law applies" and "no violations" are different facts: a judging in which no law ran is SKIPPED, never passed
    let ran = !laws_run.is_empty();
    let laws_digest = { use sha2::{Digest, Sha256}; hex::encode(Sha256::digest(laws_run.iter().cloned().collect::<Vec<_>>().join("\n").as_bytes()))[..16].to_string() };
    let mut rep = json!({ "ok": true, "gate": { "fail_on": o.fail_on, "passed": ran && failing == 0, "skipped": !ran, "failing": failing }, "score": overall, "axes": axes, "counts": counts, "advisory": advisory, "artifacts": art_rows, "findings": findings, "laws_run": laws_run.len(), "laws_digest": laws_digest, "laws_skipped": skipped, "unread": unread, "reader_version": readers::READER_VERSION });
    let digest = { use sha2::{Digest, Sha256}; hex::encode(Sha256::digest(serde_json::to_string(&rep).unwrap_or_default().as_bytes())) }; rep["digest"] = json!(digest); rep
}

// ── testing a law with its own examples ─────────────────────────────────────────────────────
fn ext_of(kind: &str) -> &'static str { match kind { "markdown" => "md", "text" => "txt", "html" => "html", "json" => "json", "yaml" => "yaml", "toml" => "toml", "csv" => "csv", "svg" => "svg", "image" => "png", "code" => "ts", _ => "md" } }
fn fixture_artifact(l: &Law, f: &Fixture, i: usize) -> Artifact {
    let kind = if !f.kind.is_empty() { f.kind.clone() } else { l.kinds.first().cloned().unwrap_or_else(|| "markdown".into()) };
    let path = if f.path.is_empty() { format!("fixture-{i}.{}", ext_of(&kind)) } else { f.path.clone() };
    let bytes = if !f.b64.is_empty() { use base64::Engine; base64::engine::general_purpose::STANDARD.decode(f.b64.trim()).unwrap_or_default() } else { f.text.clone().into_bytes() }; Artifact { path, bytes }
}
/// Run a law against its own examples: every passing example must give no hit, every failing one at least `expect` (default 1). A law is ratifiable only when it has both kinds of example and all of them agree. Pure.
pub fn test_law(l: &Law) -> Value {
    let mut rows = vec![]; let mut ok = true;
    let mut one = |f: &Fixture, i: usize, want_fail: bool| { let a = fixture_artifact(l, f, i); let facts = readers::read(&a.path, &a.bytes); let files: BTreeSet<String> = [a.path.clone()].into_iter().collect(); let all = [facts.clone()]; let ctx = Ctx { files: &files, all: &all };
        let got = if facts.unread.is_some() { Err(facts.unread.clone().unwrap()) } else { detect::run(&l.detector, &l.config, &facts, &ctx) }; let need = f.expect.unwrap_or(1);
        let (passed, n, err) = match &got { Ok(h) => (if want_fail { h.len() >= need } else { h.is_empty() }, h.len(), String::new()), Err(e) => (false, 0, e.clone()) };
        rows.push(json!({ "name": if f.name.is_empty() { format!("{}-{i}", if want_fail { "fail" } else { "pass" }) } else { f.name.clone() }, "expect": if want_fail { "hits" } else { "no hits" }, "hits": n, "passed": passed, "error": err })); if !passed { ok = false; } };
    for (i, f) in l.fixtures.pass.iter().enumerate() { one(f, i, false); } for (i, f) in l.fixtures.fail.iter().enumerate() { one(f, i, true); }
    let complete = !l.fixtures.pass.is_empty() && !l.fixtures.fail.is_empty(); let compiled = compile(l);
    json!({ "ok": ok && complete && compiled.is_ok(), "complete": complete, "compiles": compiled.is_ok(), "compile_error": compiled.err().unwrap_or_default(), "examples": rows, "why": if !complete { "a law needs at least one passing and one failing example before it can be ratified" } else { "" } })
}
/// Ratify a law: only a law whose examples the engine has just checked. Returns the law as ratified (version + 1 when it was already ratified and changed is the caller's job). Pure.
pub fn ratify(l: &Law) -> Result<Law, String> { let t = test_law(l); if t["ok"] != json!(true) { return Err(format!("not ratified: {}", if t["why"].as_str().unwrap_or("").is_empty() { "an example disagrees with the law".to_string() } else { t["why"].as_str().unwrap_or("").to_string() })); } let mut n = l.clone(); n.status = "ratified".into(); Ok(n) }
/// The report as markdown for a repair agent: findings with a place and a fix, worst first, at most `limit`. Pure.
pub fn render_findings(rep: &Value, limit: usize) -> String {
    let mut fs: Vec<&Value> = rep["findings"].as_array().map(|a| a.iter().filter(|f| f["advisory"] != json!(true)).collect()).unwrap_or_default(); fs.sort_by(|a, b| sev_rank(b["severity"].as_str().unwrap_or("")).cmp(&sev_rank(a["severity"].as_str().unwrap_or(""))));
    let mut out = format!("score {} · {} finding(s)\n", rep["score"], fs.len());
    for f in fs.iter().take(limit) { out.push_str(&format!("- [{}] {}:{} {} ({}){}\n", f["severity"].as_str().unwrap_or(""), f["path"].as_str().unwrap_or(""), f["line"], f["message"].as_str().unwrap_or(""), f["law"].as_str().unwrap_or(""), if f["fix"].as_str().unwrap_or("").is_empty() { String::new() } else { format!(" · fix: {}", f["fix"].as_str().unwrap_or("")) })); }
    if fs.len() > limit { out.push_str(&format!("… and {} more\n", fs.len() - limit)); } out
}
/// THE DOCUMENTATION OF A FILE, BY THE BOOK (the operator 2026-11-21: "the docs should be flexible, not just docs for coding: the Leader creates the docs rules, generated deterministically, for the type of project the team produces; all on the book"): the file's own facts (what it is, its outline, its definitions) plus a section for every
/// `doc` law of the packs that apply to it: a law's `section` names the section, its `label` says what a hit means (a design pattern, a route, a call to action, a claim). The same detectors as the checks, the same scoping, no model.
pub fn document(packs: &[Pack], path: &str, bytes: &[u8], files: &BTreeSet<String>) -> Value {
    let mut doc = describe::describe(path, bytes); let f = readers::read(path, bytes); if f.unread.is_some() { return doc; }
    let all = [f.clone()]; let ctx = Ctx { files, all: &all }; let mut sections: BTreeMap<String, Vec<Value>> = BTreeMap::new(); let mut order: Vec<String> = vec![];
    let mut ps: Vec<&Pack> = packs.iter().collect(); ps.sort_by(|a, b| a.id.cmp(&b.id));
    for p in ps { if !matches_scope(&p.kinds, &p.paths, &f.kind, &f.path) || p.exclude.iter().any(|g| detect::glob_match(g, &f.path)) { continue; }
        for l in &p.laws { if l.purpose != "doc" || l.status == "suspended" || !matches_scope(&l.kinds, &l.paths, &f.kind, &f.path) { continue; }
            let Ok(hits) = detect::run(&l.detector, &l.config, &f, &ctx) else { continue }; if hits.is_empty() { continue; }
            let section = { let t = l.config["section"].as_str().unwrap_or(""); if t.is_empty() { if l.axis.is_empty() { "Notes".to_string() } else { l.axis.clone() } } else { t.to_string() } }; let label = { let t = l.config["label"].as_str().unwrap_or(""); if t.is_empty() { l.title.clone() } else { t.to_string() } };
            if !sections.contains_key(&section) { order.push(section.clone()); }
            let rows = sections.entry(section).or_default(); let n = hits.len();
            let places: Vec<String> = hits.iter().take(4).map(|h| format!("line {}", h.line)).collect();
            rows.push(json!({ "k": label, "v": format!("{}{}{}", if n == 1 { String::new() } else { format!("{n} times · ") }, places.join(", "), if l.status == "candidate" { " · proposed, not ratified" } else { "" }), "signature": hits.first().map(|h| h.excerpt.clone()).unwrap_or_default(), "law": l.key }));
        } }
    if !order.is_empty() { if let Some(a) = doc["sections"].as_array_mut() { for t in order { a.push(json!({ "title": t.clone(), "items": sections.remove(&t).unwrap_or_default() })); } } }
    doc
}
/// A pack as the screens and the Leader see it: its laws with status, severity and how many examples each has. Pure.
pub fn summary(p: &Pack, workspace_copy: bool) -> Value {
    json!({ "id": p.id, "name": p.name, "description": p.description, "axes": p.axes, "kinds": p.kinds, "paths": p.paths, "exclude": p.exclude, "version": p.version, "origin": if workspace_copy { "workspace" } else { "shipped" },
        "laws": p.laws.iter().map(|l| json!({ "key": l.key, "title": l.title, "domain": l.domain, "axis": l.axis, "detector": l.detector, "severity": l.severity, "weight": l.weight, "status": l.status, "version": l.version, "kinds": l.kinds, "rule": l.rule, "fix": l.fix, "examples": { "pass": l.fixtures.pass.len(), "fail": l.fixtures.fail.len() }, "created_by": l.created_by })).collect::<Vec<_>>() })
}
/// Which pack ids have a copy in the workspace's own book. Blocking.
pub fn workspace_ids(root: &str) -> BTreeSet<String> { crate::docstore::import_dir(root, "pack", &book_dir(root), &["yaml", "yml", "json"]); crate::docstore::ids(root, "pack").into_iter().filter_map(|id| crate::docstore::get(root, "pack", &id)).filter_map(|v| v["id"].as_str().map(String::from)).collect() }
/// What an AGENT may save: its laws are candidates, whatever they say; a law identical to a ratified one that is already in the book keeps its status. Pure.
pub fn as_agent_proposal(mut incoming: Pack, existing: Option<&Pack>) -> Pack {
    for l in incoming.laws.iter_mut() { let same = existing.and_then(|e| e.laws.iter().find(|x| x.key == l.key)).map_or(false, |x| x.status == "ratified" && { let mut a = x.clone(); let mut b = l.clone(); a.status.clear(); b.status.clear(); a.created_by.clear(); b.created_by.clear(); a == b }); if !same { l.status = "candidate".into(); if l.created_by.is_empty() { l.created_by = "leader".into(); } if let Some(x) = existing.and_then(|e| e.laws.iter().find(|x| x.key == l.key)) { l.version = l.version.max(x.version + 1); } } }
    incoming
}
/// Read the artifacts of a folder for judging: files only, outside `.git`, `node_modules`, build folders and antiloki's own files; at most `max` files; the paths relative with `/`. Blocking.
pub fn collect(root: &std::path::Path, globs: &[String], max: usize) -> Vec<Artifact> {
    let mut out = vec![]; let skip = ["node_modules", "target", "dist", "build", ".git", ".antiloki", "venv", ".venv", "__pycache__", ".next"];
    fn walk(base: &std::path::Path, dir: &std::path::Path, skip: &[&str], globs: &[String], max: usize, out: &mut Vec<Artifact>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return }; let mut es: Vec<_> = rd.flatten().collect(); es.sort_by_key(|e| e.file_name());
        for e in es { if out.len() >= max { return; } let p = e.path(); let n = e.file_name().to_string_lossy().to_string(); let Ok(ft) = e.file_type() else { continue }; if ft.is_symlink() { continue; }
            if ft.is_dir() { if skip.contains(&n.as_str()) { continue; } walk(base, &p, skip, globs, max, out); } else if ft.is_file() { let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().replace('\\', "/"); if !globs.is_empty() && !globs.iter().any(|g| detect::glob_match(g, &rel)) { continue; } if e.metadata().map_or(true, |m| m.len() > 16 * 1024 * 1024) { continue; } if let Ok(b) = std::fs::read(&p) { out.push(Artifact { path: rel, bytes: b }); } } }
    }
    walk(root, root, &skip, globs, max, &mut out); out
}

#[cfg(test)]
mod tests;
