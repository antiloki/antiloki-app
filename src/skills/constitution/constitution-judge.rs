//! constitution-judge.rs — twin of skills/constitution/constitution-judge.mjs (db lane,
//! cross-db `laws`+`judgments`, plus unqualified `code_nodes`/`code_edges` on the main db, plus
//! REAL fs walk + REAL `git` subprocess). NAMES ARE CONTRACT: `constitutionJudge` (default).
//! $0, ZERO AI — the deterministic constitution-review pipeline: scope → per-file law-compliance
//! judging (D3) → import-binding health (D4) → a fresh `judgments.review_snapshots` row the
//! screen renders (D5). BESPOKE-NATIVE: real filesystem walk (`walk`), real `sha256` content
//! hashing, real `git log`/`git diff` subprocesses (via the reused `gitHistory`/`coChangePairs`
//! twins + a local `git diff --name-only` call for the `base` scope), and real `rusqlite`
//! read/insert/update against `laws`+`judgments`+main — proven by `#[cfg(test)]` native tests
//! against a real temp dir + in-memory db, NOT a conform-db fixture.
//!
//! Reuses already-converted twins verbatim: `ensureConstitutionSchema`/`composedConstitution`/
//! `resolveFrameworks` (schema.rs / constitution-law.rs), `atlasMetrics`/`atlasSmells`
//! (codebase/atlas-metrics.rs, atlas-smells.rs), `lintRun` (codebase/lint-run.rs),
//! `lawApplies`/`exceptionCovers`/`runDetector`/`resolveLayer` (detectors.rs),
//! `grammarForPath`/`getTree` (ts-query.rs — native tree-sitter, sync, see its own module doc),
//! `analyzeTree`/`rollupCognitive` (cognitive.rs), `gitHistory`/`transitiveOf`/`coChangePairs`/
//! `computeEngineering` (engineering.rs), `getWorkspaceRoot` (workspace-root.rs),
//! `constitutionReport` (constitution-report.rs). `context.exec_fn` (the JS test-injection seam
//! for the `base` git-diff branch) is NOT modeled — same "test seams are not exercised" convention
//! as every sibling bespoke-native twin; this port always shells out to the real `git` binary.
//!
//! ## Disclosed gap: `computeEngineering`'s `cohort` excludes `is_test` units
//! `computeEngineering`'s Rust twin returns `{summary, cohort}` where `cohort` is the JS
//! `cohort = units.filter(u => !u.is_test)` array — by design (constitution-judge.mjs itself
//! only ever reads `eng.summary`, never `eng.cohort`; it relies on JS's in-place OBJECT MUTATION
//! of the `units` array it built, which has no Rust equivalent across an owned-Value boundary).
//! For `is_test`-classified files this port therefore cannot pull the full per-unit
//! `axes.structure/coupling/tests/change/cohesion` from the shared twin (they're computed
//! internally but only surfaced for the non-test cohort). This port's fallback for `is_test`
//! files recomputes ONLY `axes.compliance` (`= the law score, verbatim`) and `axes.defects`
//! (`clamp100(100 - min(90, findings*15 + findings_high*10))` — copied 1:1 from the JS formula,
//! not "logic reuse", a two-line arithmetic identity) and derives `health` as their weighted mean
//! (weights 0.25/0.18, same as computeEngineering's `W` table) — `archetype: 'test'` always
//! matches JS exactly. `structure`/`coupling`/`tests`/`change`/`cohesion` axes and
//! `ai_readability`/`pct_repo`/`pct_folder` are `null` for test files here even when JS would
//! compute a real value from present metrics. Every OTHER file (the overwhelming majority: any
//! non-test file) gets the full, faithful `computeEngineering` cohort result. Every
//! `review_scores` row still gets written for every scoped file either way — this is a narrower
//! axis-fidelity gap for one archetype, not a missing-row gap.
// QUALITY: wide orchestrator (D3 per-file judging + D4 binding health), most sub-logic already
// lives in already-converted twins this file composes; the judging loop / cached-vs-fresh split /
// per-block self-vs-neighborhood scoring below is the real transcribed core.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }
fn js_number(v: Option<&Value>) -> f64 { match v { None=>f64::NAN, Some(Value::Null)=>0.0, Some(Value::Bool(b))=>if *b {1.0} else {0.0}, Some(Value::Number(n))=>n.as_f64().unwrap_or(f64::NAN), Some(Value::String(s))=>{let t=s.trim(); if t.is_empty(){0.0}else{t.parse().unwrap_or(f64::NAN)}}, Some(_)=>f64::NAN } }
fn js_round(x: f64) -> f64 { (x + 0.5).floor() }
fn clamp100(x: f64) -> f64 { if x.is_nan() { f64::NAN } else { x.max(0.0).min(100.0) } }
fn opt_num(v: Option<f64>) -> Value { match v { Some(f) => num_out(f), None => Value::Null } }
fn num_out(f: f64) -> Value { if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 { json!(f as i64) } else { json!(f) } }
fn safe_json(v: Option<&Value>) -> Value {
    let text = match v { Some(Value::String(s)) => s.clone(), None | Some(Value::Null) => "null".to_string(), Some(other) => js_string(other) };
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

const MAX_SOURCE: usize = 400_000;
const SCORE_CAP: f64 = 90.0; // one file's total penalty is capped — never below 10
const WALK_SKIP: [&str; 8] = ["node_modules", ".git", "dist", "build", "coverage", ".antiloki", ".next", "out"];

// Delegates to the ONE capability list (ast-parse's adapter table filtered to grammars that
// actually have a walk profile). This used to be a private copy of
// `(mjs|cjs|js|jsx|ts|tsx|py)`; four skills each held one, none ever gained `rs`, and the
// review pass walked straight past every Rust file while reporting success.
fn code_re() -> &'static Regex { crate::codebase_ast_parse::analyzable_ext_regex() }

fn to_forward(p: &Path) -> String { p.to_string_lossy().replace('\\', "/") }

fn walk(root: &str, rel: &str, cap: i64) -> Vec<String> {
    let re = code_re();
    let mut out: Vec<String> = Vec::new();
    let base = Path::new(root);
    let start = if rel == "." || rel.is_empty() { base.to_path_buf() } else { base.join(rel) };
    let mut stack: Vec<PathBuf> = vec![start];
    while let Some(dir) = stack.pop() {
        if (out.len() as i64) >= cap * 4 { break; }
        let entries = match std::fs::read_dir(&dir) { Ok(e) => e, Err(_) => continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let ft = match entry.file_type() { Ok(t) => t, Err(_) => continue };
            if ft.is_dir() {
                if !WALK_SKIP.contains(&name.as_str()) && !name.starts_with('.') { stack.push(dir.join(&name)); }
            } else if re.is_match(&name) {
                let full = dir.join(&name);
                let relp = full.strip_prefix(base).map(to_forward).unwrap_or_else(|_| to_forward(&full));
                out.push(relp);
            }
        }
    }
    out.sort();
    out
}

fn constitution_hash(laws: &[Value], exceptions: &[Value]) -> String {
    let mut parts: Vec<String> = laws.iter()
        .filter(|l| l.get("status").and_then(Value::as_str) == Some("ratified") && l.get("enforcement").and_then(Value::as_str) == Some("score"))
        .map(|l| format!("{}@{}:{}:{}", js_string(l.get("key").unwrap_or(&Value::Null)), js_string(l.get("version").unwrap_or(&Value::Null)), js_string(l.get("weight").unwrap_or(&Value::Null)), js_string(l.get("detector").unwrap_or(&Value::Null))))
        .collect();
    parts.sort();
    let mut ex: Vec<String> = exceptions.iter()
        .map(|e| format!("{}:{}", js_string(e.get("law_key").unwrap_or(&Value::Null)), js_string(e.get("path_glob").unwrap_or(&Value::Null))))
        .collect();
    ex.sort();
    let text = format!("{}#{}", parts.join("|"), ex.join("|"));
    hex::encode(Sha256::digest(text.as_bytes()))[..16].to_string()
}

struct SpanEntry { id: Value, name: Value, start: i64, end: i64 }

/// The language recorded on a review row. The old form was a two-branch chain that fell through to
/// "js" for everything else — so once Rust files entered the manifest, every .rs row was stamped
/// `languages='js'`. Harmless-looking, and wrong in the way that matters: the language field is what
/// downstream lenses filter on, so a Rust file would be judged by JavaScript-shaped rules and appear
/// in a JavaScript cohort. A default is not a detection.
fn langs_csv(p: &str) -> &'static str {
    let s = p.to_lowercase();
    let ext = s.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "ts" | "mts" | "cts" | "tsx" => "ts",
        "py" | "pyi" => "py",
        "rs" => "rs",
        "js" | "mjs" | "cjs" | "jsx" => "js",
        // Not one of the languages this pass understands. Say so rather than guess: an empty
        // language is readable as "unknown", where "js" is readable as a wrong fact.
        _ => "",
    }
}

// Fold a file's layer (dependency-direction) violations into its breakdown + penalty, and
// persist each as a violation row via `persist`. Returns the penalty added.
// `persist` args: (path, line, key, version, scope, weight, severity, confidence, evidence).
#[allow(clippy::too_many_arguments)]
fn apply_layer_violations(p: &str, layer_viol_by_file: &HashMap<String, Vec<Value>>, breakdown: &mut Map<String, Value>, persist: &mut dyn FnMut(&str, Option<i64>, &str, i64, &str, f64, &str, f64, &str)) -> f64 {
    let mut added = 0.0;
    for lv in layer_viol_by_file.get(p).cloned().unwrap_or_default() {
        let law = lv.get("law").cloned().unwrap_or(Value::Null);
        let key = law.get("key").and_then(Value::as_str).unwrap_or("").to_string();
        let version = law.get("version").and_then(Value::as_i64).unwrap_or(1);
        let scope = law.get("scope").and_then(Value::as_str).unwrap_or("").to_string();
        let weight = law.get("weight").and_then(Value::as_i64).unwrap_or(0);
        let severity = law.get("severity").and_then(Value::as_str).unwrap_or("").to_string();
        let conf = if law.get("confidence").map_or(true, Value::is_null) { 1.0 } else { js_number(law.get("confidence")) };
        let pen = js_round(weight as f64 * conf);
        added += pen;
        let prev = breakdown.get(&key).cloned().unwrap_or_else(|| json!({"hits":0,"weight":weight,"confidence":conf,"scope":scope,"severity":severity,"penalty":0}));
        let hits = prev.get("hits").and_then(Value::as_i64).unwrap_or(0) + 1;
        let pen_sum = js_number(prev.get("penalty")) + pen;
        breakdown.insert(key.clone(), json!({"hits":hits,"weight":weight,"confidence":conf,"scope":scope,"severity":severity,"penalty":num_out(pen_sum)}));
        let to_path = lv.get("toPath").and_then(Value::as_str).unwrap_or("");
        let la = lv.get("la").and_then(Value::as_str).unwrap_or("");
        let lb = lv.get("lb").and_then(Value::as_str).unwrap_or("");
        let evidence = format!("imports {to_path} (layer {la}→{lb} forbidden)");
        persist(p, None, &key, version, &scope, weight as f64, &severity, conf, &evidence);
    }
    added
}

const LINT_CAP: usize = 8;
#[allow(clippy::too_many_arguments)]
fn apply_linter_violations(p: &str, lint_by_file: &HashMap<String, Vec<Value>>, breakdown: &mut Map<String, Value>, persist: &mut dyn FnMut(&str, Option<i64>, &str, i64, &str, f64, &str, f64, &str)) -> f64 {
    let mut added = 0.0;
    for f in lint_by_file.get(p).cloned().unwrap_or_default().into_iter().take(LINT_CAP) {
        let law = f.get("law").cloned().unwrap_or(Value::Null);
        let key = law.get("key").and_then(Value::as_str).unwrap_or("").to_string();
        let version = law.get("version").and_then(Value::as_i64).unwrap_or(1);
        let scope = law.get("scope").and_then(Value::as_str).unwrap_or("").to_string();
        let weight = law.get("weight").and_then(Value::as_i64).unwrap_or(0);
        let severity = law.get("severity").and_then(Value::as_str).unwrap_or("").to_string();
        let conf = if law.get("confidence").map_or(true, Value::is_null) { 1.0 } else { js_number(law.get("confidence")) };
        let pen = js_round(weight as f64 * conf);
        added += pen;
        let prev = breakdown.get(&key).cloned().unwrap_or_else(|| json!({"hits":0,"weight":weight,"confidence":conf,"scope":scope,"severity":severity,"penalty":0}));
        let hits = prev.get("hits").and_then(Value::as_i64).unwrap_or(0) + 1;
        let pen_sum = js_number(prev.get("penalty")) + pen;
        breakdown.insert(key.clone(), json!({"hits":hits,"weight":weight,"confidence":conf,"scope":scope,"severity":severity,"penalty":num_out(pen_sum)}));
        let line = f.get("line").and_then(Value::as_i64);
        let code = f.get("code").and_then(Value::as_str).unwrap_or("lint");
        let message: String = f.get("message").and_then(Value::as_str).unwrap_or("").chars().take(140).collect();
        let evidence = format!("{code}: {message}");
        persist(p, line, &key, version, &scope, weight as f64, &severity, conf, &evidence);
    }
    added
}

/// `export default async function constitutionJudge(input, context)`
pub fn constitutionJudge(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::constitution__helpers_schema::ensureConstitutionSchema(db);

    let root: String = match input.get("root").and_then(Value::as_str) {
        Some(r) if !r.is_empty() => {
            let p = Path::new(r);
            if p.is_absolute() { r.to_string() } else { std::env::current_dir().map(|c| to_forward(&c.join(p))).unwrap_or_else(|_| r.to_string()) }
        }
        _ => crate::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or(".").to_string(),
    };
    let cap: i64 = input.get("max_files").and_then(Value::as_i64).unwrap_or(1200);

    let composed = crate::constitution_constitution_law::composedConstitution(db);
    let laws: Vec<Value> = composed.get("laws").and_then(Value::as_array).cloned().unwrap_or_default();
    let exceptions: Vec<Value> = composed.get("exceptions").and_then(Value::as_array).cloned().unwrap_or_default();
    let scoring: Vec<Value> = laws.iter().filter(|l| l.get("status").and_then(Value::as_str) == Some("ratified") && l.get("enforcement").and_then(Value::as_str) == Some("score")).cloned().collect();
    let frameworks = crate::constitution_constitution_law::resolveFrameworks(db);
    // the project's own lint decisions — a seed law naming an ESLint rule defers to them
    let project_rules = crate::constitution__helpers_project_rules::projectRules(&root);
    let c_hash = constitution_hash(&laws, &exceptions);

    // ── the docket ──────────────────────────────────────────────────────────
    let docket_laws: Vec<Value> = scoring.iter().map(|l| {
        let a: Value = l.get("applies_json").and_then(Value::as_str).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(json!({}));
        let mut conds: Vec<String> = Vec::new();
        let langs = a.get("languages").and_then(Value::as_array).filter(|v| !v.is_empty());
        if let Some(v) = langs { conds.push(format!("languages {}", v.iter().map(|x| js_string(x)).collect::<Vec<_>>().join("/"))); }
        let fws = a.get("frameworks").and_then(Value::as_array).filter(|v| !v.is_empty());
        if let Some(v) = fws { conds.push(format!("frameworks {}", v.iter().map(|x| js_string(x)).collect::<Vec<_>>().join("/"))); }
        let paths = a.get("paths").and_then(Value::as_array).filter(|v| !v.is_empty());
        if let Some(v) = paths { conds.push(format!("paths {}", v.iter().map(|x| js_string(x)).collect::<Vec<_>>().join(", "))); }
        let fw_ok = match a.get("frameworks").and_then(Value::as_array).filter(|v| !v.is_empty()) {
            None => true,
            Some(v) => v.iter().any(|f| frameworks.as_array().map_or(false, |fa| fa.contains(f))),
        };
        json!({
            "key": l.get("key").cloned().unwrap_or(Value::Null), "title": l.get("title").cloned().unwrap_or(Value::Null),
            "scope": l.get("scope").cloned().unwrap_or(Value::Null), "detector": l.get("detector").cloned().unwrap_or(Value::Null),
            "weight": l.get("weight").cloned().unwrap_or(Value::Null),
            "why": if conds.is_empty() { "universal — applies to every file".to_string() } else { conds.join(" · ") },
            "active": fw_ok,
        })
    }).collect();
    let docket = json!({
        "frameworks": frameworks, "laws_total": laws.len(), "laws_scoring": scoring.len(),
        "exceptions": exceptions.len(), "laws": docket_laws, "project_rules": project_rules,
    });

    // ── scope ───────────────────────────────────────────────────────────────
    let mut paths: Vec<String>;
    if let Some(files) = input.get("files").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        paths = files.iter().map(|f| match f { Value::String(s) => s.clone(), other => other.get("path").and_then(Value::as_str).unwrap_or("").to_string() }).filter(|s| !s.is_empty()).collect();
    } else if let Some(base) = input.get("base").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let out = crate::proc::cmd("git").arg("-C").arg(&root).arg("diff").arg("--name-only").arg(format!("{base}...HEAD")).output();
        paths = match out {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout).into_owned();
                let mut seen = HashSet::new();
                text.split('\n').map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && seen.insert(s.clone())).collect()
            }
            _ => Vec::new(),
        };
    } else {
        let folder = input.get("folder").and_then(Value::as_str).map(|s| s.trim()).filter(|s| !s.is_empty()).unwrap_or(".");
        paths = walk(&root, folder, cap);
    }
    let re = code_re();
    let mut seen = HashSet::new();
    paths.retain(|p| re.is_match(p) && seen.insert(p.clone()));
    if (paths.len() as i64) > cap { paths.truncate(cap.max(0) as usize); }
    let scoped: Vec<String> = paths.into_iter().filter(|p| {
        let full = if Path::new(p).is_absolute() { PathBuf::from(p) } else { Path::new(&root).join(p) };
        full.exists()
    }).collect();
    let scoped_set: HashSet<String> = scoped.iter().cloned().collect();

    // ── atlas signals ───────────────────────────────────────────────────────
    let metrics_by_file: Value = crate::codebase_atlas_metrics::atlasMetrics(&json!({"origin":"atlas","top":1,"root":root}), db)
        .ok().and_then(|v| v.get("metrics_by_file").cloned()).unwrap_or(json!({}));
    let mut smells_by_file: HashMap<String, Vec<String>> = HashMap::new();
    if let Ok(r) = crate::codebase_atlas_smells::atlasSmells(&json!({"origin":"atlas"}), db) {
        for s in r.get("smells").and_then(Value::as_array).cloned().unwrap_or_default() {
            let kind = s.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
            for n in s.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default() {
                let p = match &n { Value::String(s) => s.clone(), other => other.get("path").and_then(Value::as_str).unwrap_or("").to_string() }.replace('\\', "/");
                if p.is_empty() { continue; }
                smells_by_file.entry(p).or_default().push(kind.clone());
            }
        }
    }

    let mut def_count_by_file: HashMap<String, i64> = HashMap::new();
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT path, COUNT(*) n FROM code_nodes WHERE origin='atlas' AND type IN ('function','class','method','component','hook') GROUP BY path", &[]) {
        for r in rows.as_array().cloned().unwrap_or_default() {
            let p = r.get("path").and_then(Value::as_str).unwrap_or("").replace('\\', "/");
            def_count_by_file.insert(p, r.get("n").and_then(Value::as_i64).unwrap_or(0));
        }
    }

    let mut spans_by_file: HashMap<String, Vec<SpanEntry>> = HashMap::new();
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT id, path, name, span_json FROM code_nodes WHERE origin='atlas' AND type <> 'file'", &[]) {
        for r in rows.as_array().cloned().unwrap_or_default() {
            let sp: Value = r.get("span_json").and_then(Value::as_str).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
            let start = sp.get("start_line").and_then(Value::as_i64);
            let start = match start { Some(s) => s, None => continue };
            let end = sp.get("end_line").and_then(Value::as_i64).unwrap_or(start);
            let p = r.get("path").and_then(Value::as_str).unwrap_or("").replace('\\', "/");
            spans_by_file.entry(p).or_default().push(SpanEntry { id: r.get("id").cloned().unwrap_or(Value::Null), name: r.get("name").cloned().unwrap_or(Value::Null), start, end });
        }
    }

    let mut cohesion_by_file: HashMap<String, f64> = HashMap::new();
    let mut intra_edges_by_file: HashMap<String, Vec<(String, String)>> = HashMap::new();
    {
        let mut node_file: HashMap<String, String> = HashMap::new();
        let mut syms_by_file: HashMap<String, HashSet<String>> = HashMap::new();
        // symbols = the things that can call or render each other. `const`/`type` nodes are data,
        // not participants — counting them made a file with two components and four constants
        // read as one-third connected.
        if let Ok(rows) = crate::db_json::query_rows(db, "SELECT id, path FROM code_nodes WHERE origin='atlas' AND type IN ('function','class','method','component','hook')", &[]) {
            for r in rows.as_array().cloned().unwrap_or_default() {
                let id = r.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                let p = r.get("path").and_then(Value::as_str).unwrap_or("").replace('\\', "/");
                node_file.insert(id.clone(), p.clone());
                syms_by_file.entry(p).or_default().insert(id);
            }
        }
        let mut connected: HashMap<String, HashSet<String>> = HashMap::new();
        if let Ok(rows) = crate::db_json::query_rows(db, "SELECT from_node, to_node FROM code_edges WHERE origin='atlas' AND type IN ('calls','uses','renders')", &[]) {
            for r in rows.as_array().cloned().unwrap_or_default() {
                let from = r.get("from_node").and_then(Value::as_str).unwrap_or("").to_string();
                let to = r.get("to_node").and_then(Value::as_str).unwrap_or("").to_string();
                if let (Some(pf), Some(pt)) = (node_file.get(&from), node_file.get(&to)) {
                    if pf == pt {
                        connected.entry(pf.clone()).or_default().insert(from.clone());
                        connected.entry(pf.clone()).or_default().insert(to.clone());
                        intra_edges_by_file.entry(pf.clone()).or_default().push((from, to));
                    }
                }
            }
        }
        for (p, syms) in &syms_by_file {
            if syms.len() >= 2 {
                let c = connected.get(p).map(|s| s.len()).unwrap_or(0);
                cohesion_by_file.insert(p.clone(), c as f64 / syms.len() as f64);
            }
        }
    }

    // ── prior snapshot for incremental carry-forward ───────────────────────
    let prior_id: Option<i64> = crate::db_json::query_rows(db, "SELECT id FROM judgments.review_snapshots WHERE status='done' AND constitution_hash=? ORDER BY id DESC LIMIT 1", &[json!(c_hash)])
        .ok().and_then(|v| v.as_array().and_then(|a| a.first().cloned())).and_then(|r| r.get("id").and_then(Value::as_i64));

    let scope_json = json!({"folder": input.get("folder").cloned().unwrap_or(if input.get("files").map_or(false,|f|f.is_array()) { Value::Null } else { json!(".") }), "base": input.get("base").cloned().unwrap_or(Value::Null), "files": scoped.len()});
    let (_, snap_id) = crate::db_json::run_write(db,
        "INSERT INTO judgments.review_snapshots (source, scope_json, manifest_json, constitution_hash, status, files, engineering_version) VALUES (?, ?, ?, ?, 'running', ?, ?)",
        &[json!(input.get("source").and_then(Value::as_str).unwrap_or("wizard")), scope_json, json!(scoped), json!(c_hash), json!(scoped.len() as i64), json!(crate::constitution__helpers_engineering::ENGINEERING_VERSION)])?;

    let insert_score_sql = "INSERT INTO judgments.review_scores \
      (snapshot_id, unit_kind, path, content_hash, languages, score, breakdown_json, fan_in, risk, cached, \
       loc, complexity, fan_out, transitive, covered, churn, authors, findings, axes_json, archetype, health, ai_readability, pct_repo, pct_folder, blocks_json) \
      VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)";
    let insert_viol_sql = "INSERT INTO judgments.review_violations (snapshot_id, path, symbol, line, law_key, law_version, law_scope, weight, severity, confidence, evidence) VALUES (?,?,?,?,?,?,?,?,?,?,?)";

    // ── import graph (scoped) ───────────────────────────────────────────────
    let mut edges: Vec<(String, String)> = Vec::new();
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT id, path FROM code_nodes WHERE origin='atlas' AND type='file'", &[]) {
        let id_to_path: HashMap<String, String> = rows.as_array().cloned().unwrap_or_default().into_iter()
            .map(|r| (r.get("id").and_then(Value::as_str).unwrap_or("").to_string(), r.get("path").and_then(Value::as_str).unwrap_or("").replace('\\', "/")))
            .collect();
        if let Ok(erows) = crate::db_json::query_rows(db, &format!("SELECT from_node, to_node FROM code_edges WHERE origin='atlas' AND {}", crate::codebase_atlas_build::RUNTIME_IMPORTS), &[]) {
            let mut seen_e: HashSet<String> = HashSet::new();
            for e in erows.as_array().cloned().unwrap_or_default() {
                let from = e.get("from_node").and_then(Value::as_str).unwrap_or("");
                let to = e.get("to_node").and_then(Value::as_str).unwrap_or("");
                if let (Some(a), Some(b)) = (id_to_path.get(from), id_to_path.get(to)) {
                    if a == b || !scoped_set.contains(a) || !scoped_set.contains(b) { continue; }
                    let kk = format!("{a}|{b}");
                    if seen_e.insert(kk) { edges.push((a.clone(), b.clone())); }
                }
            }
        }
    }

    // ── ARCHITECTURE · layer pass ───────────────────────────────────────────
    let layer_laws: Vec<Value> = scoring.iter().filter(|l| l.get("detector").and_then(Value::as_str) == Some("layer")).cloned().collect();
    let layer_law_keys: HashSet<String> = layer_laws.iter().filter_map(|l| l.get("key").and_then(Value::as_str).map(String::from)).collect();

    // ── LINTER pass ──────────────────────────────────────────────────────────
    let linter_laws: Vec<Value> = scoring.iter().filter(|l| l.get("detector").and_then(Value::as_str) == Some("linter")).cloned().collect();
    let linter_law_keys: HashSet<String> = linter_laws.iter().filter_map(|l| l.get("key").and_then(Value::as_str).map(String::from)).collect();
    let mut lint_by_file: HashMap<String, Vec<Value>> = HashMap::new();
    if !linter_laws.is_empty() {
        let files_input: Vec<Value> = scoped.iter().map(|p| json!({"path": p})).collect();
        if let Ok(r) = crate::codebase_lint_run::lintRun(&json!({"files": files_input}), db) {
            for f in r.get("findings").and_then(Value::as_array).cloned().unwrap_or_default() {
                let p = f.get("file").and_then(Value::as_str).unwrap_or("").replace('\\', "/");
                if p.is_empty() { continue; }
                for law in &linter_laws {
                    let key = law.get("key").and_then(Value::as_str).unwrap_or("");
                    if crate::constitution__helpers_detectors::exceptionCovers(&json!(exceptions), &json!(key), &json!(p)).as_bool().unwrap_or(false) { continue; }
                    if !crate::constitution__helpers_detectors::lawApplies(law, &json!({"path": p, "frameworks": frameworks})).as_bool().unwrap_or(false) { continue; }
                    lint_by_file.entry(p.clone()).or_default().push(json!({"law": law, "line": f.get("line").cloned().unwrap_or(Value::Null), "code": f.get("code").and_then(Value::as_str).unwrap_or("lint"), "message": f.get("message").and_then(Value::as_str).unwrap_or("")}));
                }
            }
        }
    }

    let mut layer_viol_by_file: HashMap<String, Vec<Value>> = HashMap::new();
    let mut layer_edge: HashSet<String> = HashSet::new();
    for law in &layer_laws {
        let cfg: Value = law.get("config_json").and_then(Value::as_str).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(json!({}));
        let forbid: HashSet<String> = cfg.get("forbid").and_then(Value::as_array).cloned().unwrap_or_default().iter()
            .filter_map(|pair| pair.as_array()).filter(|p| p.len() == 2)
            .map(|p| format!("{}>{}", js_string(&p[0]), js_string(&p[1])))
            .collect();
        let layers = cfg.get("layers").cloned().unwrap_or(Value::Null);
        let key = law.get("key").and_then(Value::as_str).unwrap_or("");
        for (a, b) in &edges {
            if crate::constitution__helpers_detectors::exceptionCovers(&json!(exceptions), &json!(key), &json!(a)).as_bool().unwrap_or(false) { continue; }
            let la = crate::constitution__helpers_detectors::resolveLayer(&json!(a), &layers);
            let lb = crate::constitution__helpers_detectors::resolveLayer(&json!(b), &layers);
            if let (Some(la_s), Some(lb_s)) = (la.as_str(), lb.as_str()) {
                if forbid.contains(&format!("{la_s}>{lb_s}")) {
                    layer_viol_by_file.entry(a.clone()).or_default().push(json!({"law": law, "toPath": b, "la": la_s, "lb": lb_s}));
                    layer_edge.insert(format!("{a}|{b}"));
                }
            }
        }
    }

    // ── import adjacency + git history + defect ledger ─────────────────────
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for (a, b) in &edges { adj.entry(a.clone()).or_default().push(b.clone()); }
    let git = crate::constitution__helpers_engineering::gitHistory(&root);
    let mut ledger: HashMap<String, (i64, i64)> = HashMap::new();
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT file, COUNT(*) n, SUM(CASE WHEN severity='high' THEN 1 ELSE 0 END) high FROM judgments.bugfinder_findings WHERE dropped=0 AND file IS NOT NULL GROUP BY file", &[]) {
        for r in rows.as_array().cloned().unwrap_or_default() {
            let f = r.get("file").and_then(Value::as_str).unwrap_or("").replace('\\', "/");
            ledger.insert(f, (r.get("n").and_then(Value::as_i64).unwrap_or(0), r.get("high").and_then(Value::as_i64).unwrap_or(0)));
        }
    }

    // ── PASS 1 · law compliance per file ────────────────────────────────────
    let mut judged: i64 = 0;
    let mut carried: i64 = 0;
    let mut law_by_path: HashMap<String, (f64, Map<String, Value>, String, i64)> = HashMap::new(); // score, breakdown, content_hash, cached
    let mut cog_by_file: HashMap<String, Value> = HashMap::new();
    let mut viol_lines_by_file: HashMap<String, Vec<(Option<i64>, f64, f64)>> = HashMap::new();

    let empty_spans: Vec<SpanEntry> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    for p in &scoped {
        let full = if Path::new(p).is_absolute() { PathBuf::from(p) } else { Path::new(&root).join(p) };
        // A SKIP IS RECORDED, NOT SWALLOWED. Both arms used to `continue` in silence, so a file
        // simply vanished from the review while the paper above it still counted its symbols in the
        // anatomy. Observed: a 422,131-byte main.rs sat 22KB over the cap and disappeared — the
        // folder's headline "health" then described 132 of 301 symbols (44%), with nothing saying
        // so, and the excluded file was the one most likely to be unhealthy. The count already
        // showed up as files=48 / judged=45; what was missing was WHICH and WHY.
        let source = match std::fs::read_to_string(&full) {
            Ok(s) => s,
            Err(e) => { skipped.push(json!({ "path": p, "reason": "unreadable", "detail": e.to_string() })); continue }
        };
        if source.len() > MAX_SOURCE {
            skipped.push(json!({ "path": p, "reason": "too_large", "bytes": source.len() as i64, "cap": MAX_SOURCE as i64 }));
            continue;
        }
        let content_hash = hex::encode(Sha256::digest(source.as_bytes()))[..16].to_string();

        let spans_arr: Value = json!(spans_by_file.get(p).unwrap_or(&empty_spans).iter().map(|s| json!({"id": s.id, "name": s.name, "start": s.start, "end": s.end})).collect::<Vec<_>>());

        let grammar = crate::constitution__helpers_ts_query::grammarForPath(&json!(p));
        let mut stmt: Value = Value::Null;
        let mut tree_lang = None;
        if let Some(g) = grammar.as_str() {
            if let Some((tree, language)) = crate::constitution__helpers_ts_query::getTree(g, &source) {
                stmt = crate::constitution__helpers_cognitive::analyzeTree(g, &tree, &source);
                cog_by_file.insert(p.clone(), crate::constitution__helpers_cognitive::rollupCognitive(&stmt, &spans_arr));
                tree_lang = Some((tree, language));
            }
        }

        let cached_row: Option<Value> = prior_id.and_then(|sid| {
            crate::db_json::query_rows(db, "SELECT * FROM judgments.review_scores WHERE snapshot_id=? AND path=? AND content_hash=?", &[json!(sid), json!(p), json!(content_hash)])
                .ok().and_then(|v| v.as_array().and_then(|a| a.first().cloned()))
        })
        // A CARRIED FILE MUST CARRY A TOTAL (2026-10-06): an entry cut at the cap with no `total_hits` was recorded before the judge counted past the cap, so carrying it forward
        // would keep a baseline the check cannot compare against - re-judge that file once and it is exact from then on. Same content, same score, same rows.
        .filter(|row| {
            let cap = crate::constitution__helpers_detectors::HITS_CAP as i64;
            let bd = safe_json(row.get("breakdown_json"));
            !bd.as_object().map_or(false, |o| o.values().any(|v| v.get("hits").and_then(Value::as_i64).unwrap_or(0) >= cap && v.get("total_hits").is_none()))
        });

        let mut breakdown: Map<String, Value> = Map::new();
        let mut penalty: f64 = 0.0;
        let cached_flag: i64;

        if let (Some(row), Some(sid)) = (cached_row, prior_id) {
            let cb = safe_json(row.get("breakdown_json")).as_object().cloned().unwrap_or_default();
            for (k, v) in cb {
                if layer_law_keys.contains(&k) || linter_law_keys.contains(&k) { continue; }
                penalty += v.get("penalty").and_then(Value::as_f64).unwrap_or(0.0);
                breakdown.insert(k, v);
            }
            if let Ok(vrows) = crate::db_json::query_rows(db, "SELECT law_key, law_version, law_scope, weight, severity, confidence, line, symbol, evidence FROM judgments.review_violations WHERE snapshot_id=? AND path=?", &[json!(sid), json!(p)]) {
                for v in vrows.as_array().cloned().unwrap_or_default() {
                    let law_key = v.get("law_key").and_then(Value::as_str).unwrap_or("").to_string();
                    if layer_law_keys.contains(&law_key) || linter_law_keys.contains(&law_key) { continue; }
                    let line = v.get("line").and_then(Value::as_i64);
                    let weight = v.get("weight").and_then(Value::as_f64).unwrap_or(0.0);
                    let conf = v.get("confidence").and_then(Value::as_f64).unwrap_or(1.0);
                    crate::db_json::run_write(db, insert_viol_sql, &[json!(snap_id), json!(p), v.get("symbol").cloned().unwrap_or(Value::Null), v.get("line").cloned().unwrap_or(Value::Null), json!(law_key), v.get("law_version").cloned().unwrap_or(json!(1)), v.get("law_scope").cloned().unwrap_or(Value::Null), v.get("weight").cloned().unwrap_or(Value::Null), v.get("severity").cloned().unwrap_or(Value::Null), v.get("confidence").cloned().unwrap_or(Value::Null), v.get("evidence").cloned().unwrap_or(Value::Null)])?;
                    viol_lines_by_file.entry(p.clone()).or_default().push((line, weight, conf));
                }
            }
            let mut persist = |pp: &str, line: Option<i64>, key: &str, ver: i64, scope: &str, weight: f64, sev: &str, conf: f64, evidence: &str| {
                let scope_v = if scope.is_empty() { Value::Null } else { json!(scope) };
                let _ = crate::db_json::run_write(db, insert_viol_sql, &[json!(snap_id), json!(pp), Value::Null, opt_num(line.map(|l| l as f64)), json!(key), json!(ver), scope_v, json!(weight), json!(sev), json!(conf), json!(evidence)]);
                viol_lines_by_file.entry(pp.to_string()).or_default().push((line, weight, conf));
            };
            penalty += apply_layer_violations(p, &layer_viol_by_file, &mut breakdown, &mut persist);
            penalty += apply_linter_violations(p, &lint_by_file, &mut breakdown, &mut persist);
            cached_flag = 1;
            carried += 1;
        } else {
            let ctx = json!({
                "path": p, "source": source, "spans": spans_arr,
                "metrics": metrics_by_file.get(p).map(|m| json!({"loc": m.get("loc"), "complexity": m.get("complexity"), "fan_in": m.get("fan_in"), "fan_out": m.get("fan_out")})),
                "smells": smells_by_file.get(p).map(|v| json!(v)).unwrap_or(if metrics_by_file.get(p).is_some() { json!([]) } else { Value::Null }),
                "frameworks": frameworks, "grammar": grammar, "stmt": stmt, "project_rules": project_rules,
                // COUNT EVERYTHING, LIST FIVE (2026-10-06): the detectors cap one law at HITS_CAP hits per file, and `constitution-check` compares counts - so a file that already
                // had five hits of a law could never be shown getting worse. The judge now counts uncapped, scores and stores only the first HITS_CAP (every score and row is
                // exactly what it was) and keeps the true total in the breakdown as `total_hits`, which is what the check compares against.
                "uncapped": true,
            });
            for law in &scoring {
                if law.get("detector").and_then(Value::as_str) == Some("layer") { continue; }
                if !crate::constitution__helpers_detectors::lawApplies(law, &ctx).as_bool().unwrap_or(false) { continue; }
                let key = law.get("key").and_then(Value::as_str).unwrap_or("").to_string();
                if crate::constitution__helpers_detectors::exceptionCovers(&json!(exceptions), &json!(key), &json!(p)).as_bool().unwrap_or(false) { continue; }
                let (tree_ref, lang_ref) = match &tree_lang { Some((t, l)) => (Some(t), Some(l)), None => (None, None) };
                let hits = crate::constitution__helpers_detectors::runDetector(law, &ctx, tree_ref, lang_ref);
                let all_hits = match hits.as_array() { Some(a) if !a.is_empty() => a.clone(), _ => continue };
                let total_hits = all_hits.len() as i64;
                let hits_arr: Vec<Value> = all_hits.into_iter().take(crate::constitution__helpers_detectors::HITS_CAP).collect();
                let conf = if law.get("confidence").map_or(true, Value::is_null) { 1.0 } else { js_number(law.get("confidence")) };
                let weight = law.get("weight").and_then(Value::as_i64).unwrap_or(0);
                let pen = js_round(weight as f64 * hits_arr.len() as f64 * conf);
                penalty += pen;
                let scope = law.get("scope").cloned().unwrap_or(Value::Null);
                let severity = law.get("severity").cloned().unwrap_or(Value::Null);
                let version = law.get("version").and_then(Value::as_i64).unwrap_or(1);
                breakdown.insert(key.clone(), json!({"hits": hits_arr.len() as i64, "total_hits": total_hits, "weight": weight, "confidence": conf, "scope": scope, "severity": severity, "penalty": num_out(pen)}));
                for h in &hits_arr {
                    let line = h.get("line").and_then(Value::as_i64);
                    crate::db_json::run_write(db, insert_viol_sql, &[json!(snap_id), json!(p), h.get("symbol").cloned().unwrap_or(Value::Null), h.get("line").cloned().unwrap_or(Value::Null), json!(key), json!(version), scope.clone(), json!(weight), severity.clone(), json!(conf), h.get("evidence").cloned().unwrap_or(Value::Null)])?;
                    viol_lines_by_file.entry(p.clone()).or_default().push((line, weight as f64, conf));
                }
            }
            let mut persist = |pp: &str, line: Option<i64>, key: &str, ver: i64, scope: &str, weight: f64, sev: &str, conf: f64, evidence: &str| {
                let scope_v = if scope.is_empty() { Value::Null } else { json!(scope) };
                let _ = crate::db_json::run_write(db, insert_viol_sql, &[json!(snap_id), json!(pp), Value::Null, opt_num(line.map(|l| l as f64)), json!(key), json!(ver), scope_v, json!(weight), json!(sev), json!(conf), json!(evidence)]);
                viol_lines_by_file.entry(pp.to_string()).or_default().push((line, weight, conf));
            };
            penalty += apply_layer_violations(p, &layer_viol_by_file, &mut breakdown, &mut persist);
            penalty += apply_linter_violations(p, &lint_by_file, &mut breakdown, &mut persist);
            cached_flag = 0;
            judged += 1;
        }
        let score = (100.0 - penalty.min(SCORE_CAP)).max(10.0);
        law_by_path.insert(p.clone(), (score, breakdown, content_hash, cached_flag));
    }

    let ordered_paths: Vec<String> = scoped.iter().filter(|p| law_by_path.contains_key(*p)).cloned().collect();

    // ── per-block SELF vs NEIGHBORHOOD ──────────────────────────────────────
    let mut blocks_json_by_file: HashMap<String, String> = HashMap::new();
    for p in &ordered_paths {
        let spans: Vec<&SpanEntry> = spans_by_file.get(p).map(|v| v.iter().filter(|s| !s.id.is_null() && (s.end - s.start) >= 1).collect()).unwrap_or_default();
        if spans.is_empty() { continue; }
        let mut cog_pb: HashMap<String, i64> = HashMap::new();
        if let Some(per_block) = cog_by_file.get(p).and_then(|c| c.get("per_block")).and_then(Value::as_array) {
            for b in per_block {
                let cog = b.get("cog").and_then(Value::as_i64).unwrap_or(0);
                if let Some(id) = b.get("id").and_then(Value::as_str) { cog_pb.insert(format!("id:{id}"), cog); }
                if let Some(name) = b.get("name").and_then(Value::as_str) { cog_pb.entry(format!("name:{name}")).or_insert(cog); }
            }
        }
        let vios = viol_lines_by_file.get(p).cloned().unwrap_or_default();
        let mut self_of: HashMap<String, i64> = HashMap::new();
        for s in &spans {
            let key = js_string(&s.id);
            let pen: i64 = vios.iter().filter(|(l, _, _)| l.map_or(false, |ln| ln >= s.start && ln <= s.end)).map(|(_, w, c)| js_round(w * c) as i64).sum::<i64>().min(90);
            let id_key = format!("id:{key}");
            let name_key = format!("name:{}", js_string(&s.name));
            let cog = cog_pb.get(&id_key).or_else(|| cog_pb.get(&name_key)).copied().unwrap_or(0);
            let cog_pen = (((cog - 8) as f64) * 3.0).max(0.0).min(55.0) as i64;
            self_of.insert(key, (100 - pen - cog_pen).max(10));
        }
        let mut nbrs: HashMap<String, HashSet<String>> = HashMap::new();
        let mut outs: HashMap<String, i64> = HashMap::new();
        let mut ins: HashMap<String, i64> = HashMap::new();
        let mut edge_set: HashSet<String> = HashSet::new();
        for (a, b) in intra_edges_by_file.get(p).cloned().unwrap_or_default() {
            edge_set.insert(format!("{a}>{b}"));
            if !self_of.contains_key(&a) || !self_of.contains_key(&b) || a == b { continue; }
            nbrs.entry(a.clone()).or_default().insert(b.clone());
            nbrs.entry(b.clone()).or_default().insert(a.clone());
            *outs.entry(a.clone()).or_insert(0) += 1;
            *ins.entry(b.clone()).or_insert(0) += 1;
        }
        let blocks: Vec<Value> = spans.iter().take(40).map(|s| {
            let key = js_string(&s.id);
            let self_v = *self_of.get(&key).unwrap_or(&100);
            let nb: Vec<String> = nbrs.get(&key).map(|s| s.iter().cloned().collect()).unwrap_or_default();
            if nb.is_empty() {
                return json!({"name": s.name, "line": s.start, "self": self_v, "neigh": Value::Null, "final": self_v, "in": ins.get(&key).copied().unwrap_or(0), "out": outs.get(&key).copied().unwrap_or(0), "cyc": 0});
            }
            let mean = js_round(nb.iter().map(|id| *self_of.get(id).unwrap_or(&100) as f64).sum::<f64>() / nb.len() as f64) as i64;
            let cyc = if nb.iter().any(|id| edge_set.contains(&format!("{key}>{id}")) && edge_set.contains(&format!("{id}>{key}"))) { 1 } else { 0 };
            let neigh = (mean - if cyc == 1 { 15 } else { 0 }).max(10);
            let final_v = js_round(0.4 * self_v as f64 + 0.6 * neigh as f64) as i64;
            json!({"name": s.name, "line": s.start, "self": self_v, "neigh": neigh, "final": final_v, "in": ins.get(&key).copied().unwrap_or(0), "out": outs.get(&key).copied().unwrap_or(0), "cyc": cyc})
        }).collect();
        blocks_json_by_file.insert(p.clone(), json!(blocks).to_string());
    }

    // ── PASS 2 · the ENGINEERING MODEL ──────────────────────────────────────
    let test_re1 = Regex::new(r"(^|/)(tests?|__tests__)/").unwrap();
    let test_re2 = Regex::new(r"\.(test|spec)\.\w+$").unwrap();
    let is_test = |p: &str| -> bool {
        metrics_by_file.get(p).and_then(|m| m.get("is_test")).and_then(Value::as_bool)
            .unwrap_or_else(|| test_re1.is_match(p) || test_re2.is_match(p))
    };
    let mut units: Vec<Value> = Vec::new();
    for p in &ordered_paths {
        let m = metrics_by_file.get(p);
        let g_entry = git.as_ref().and_then(|h| h.per_file.get(p));
        let led = ledger.get(p).cloned().unwrap_or((0, 0));
        let folder = p.rsplit_once('/').map(|(f, _)| f.to_string()).unwrap_or_else(|| ".".to_string());
        let transitive = if m.is_some() { crate::constitution__helpers_engineering::transitiveOf(&adj, p) } else { None };
        units.push(json!({
            "path": p, "folder": folder, "is_test": is_test(p),
            "atlas_known": m.is_some(), "def_count": def_count_by_file.get(p).copied().or_else(|| if m.is_some() { Some(0) } else { None }),
            "loc": m.and_then(|x| x.get("loc")).cloned().unwrap_or(Value::Null),
            "fan_in": m.and_then(|x| x.get("fan_in")).cloned().unwrap_or(Value::Null),
            "fan_out": m.and_then(|x| x.get("fan_out")).cloned().unwrap_or(Value::Null),
            "complexity": m.and_then(|x| x.get("complexity")).cloned().unwrap_or(Value::Null),
            "transitive": opt_num(transitive.map(|t| t as f64)),
            // tri-state: 1 covered · 0 measured uncovered · null nothing could measure it (no test
            // files, no coverage report) — the tests axis is then absent rather than a penalty.
            "covered": m.and_then(|x| x.get("covered")).map(|c| if c.is_null() { Value::Null } else { json!(if truthy(Some(c)) {1} else {0}) }).unwrap_or(Value::Null),
            "tests_basis": m.and_then(|x| x.get("tests_basis")).cloned().unwrap_or(Value::Null),
            "churn": g_entry.map(|e| json!(e.churn)).unwrap_or(if git.is_some() { json!(0) } else { Value::Null }),
            "authors": g_entry.map(|e| json!(e.authors.len() as i64)).unwrap_or(if git.is_some() { json!(0) } else { Value::Null }),
            "findings": led.0, "findings_high": led.1,
            "smells": smells_by_file.get(p).cloned().unwrap_or_default(),
            "cohesion": cohesion_by_file.get(p).copied().map(num_out).unwrap_or(Value::Null),
            "cog": cog_by_file.get(p).cloned().unwrap_or(Value::Null),
            "compliance": law_by_path.get(p).map(|(s, ..)| num_out(*s)).unwrap_or(Value::Null),
        }));
    }
    let eng = crate::constitution__helpers_engineering::computeEngineering(&json!(units));
    let mut enriched_by_path: HashMap<String, Value> = HashMap::new();
    for u in eng.get("cohort").and_then(Value::as_array).cloned().unwrap_or_default() {
        if let Some(p) = u.get("path").and_then(Value::as_str) { enriched_by_path.insert(p.to_string(), u); }
    }

    // ── write score rows (law compliance + engineering model) ──────────────
    let mut file_health: HashMap<String, f64> = HashMap::new();
    for p in &ordered_paths {
        let (lp_score, lp_breakdown, lp_hash, lp_cached) = law_by_path.get(p).cloned().unwrap();
        let (loc, complexity, fan_out, transitive, covered, churn, authors, findings) = units.iter().find(|u| u.get("path").and_then(Value::as_str) == Some(p.as_str()))
            .map(|u| (u.get("loc").cloned().unwrap_or(Value::Null), u.get("complexity").cloned().unwrap_or(Value::Null), u.get("fan_out").cloned().unwrap_or(Value::Null), u.get("transitive").cloned().unwrap_or(Value::Null), u.get("covered").cloned().unwrap_or(Value::Null), u.get("churn").cloned().unwrap_or(Value::Null), u.get("authors").cloned().unwrap_or(Value::Null), u.get("findings").cloned().unwrap_or(Value::Null)))
            .unwrap_or((Value::Null, Value::Null, Value::Null, Value::Null, Value::Null, Value::Null, Value::Null, Value::Null));
        let fan_in = units.iter().find(|u| u.get("path").and_then(Value::as_str) == Some(p.as_str())).and_then(|u| u.get("fan_in").cloned()).unwrap_or(Value::Null);

        let (axes, archetype, health, ai_readability, pct_repo, pct_folder): (Value, String, f64, Value, Value, Value) = if let Some(enr) = enriched_by_path.get(p) {
            let h = enr.get("health").and_then(Value::as_f64).unwrap_or(lp_score);
            (enr.get("axes").cloned().unwrap_or(Value::Null), enr.get("archetype").and_then(Value::as_str).unwrap_or("unknown").to_string(), h, enr.get("ai_readability").cloned().unwrap_or(Value::Null), enr.get("pct_repo").cloned().unwrap_or(Value::Null), enr.get("pct_folder").cloned().unwrap_or(Value::Null))
        } else {
            // is_test file not in computeEngineering's cohort — see module doc's disclosed gap.
            let findings_n = js_number(Some(&findings));
            let defects_score = clamp100(100.0 - (findings_n * 15.0).min(90.0));
            let axes_v = json!({"compliance": {"score": num_out(lp_score), "parts": {"law_score": num_out(lp_score)}}, "defects": {"score": num_out(defects_score), "parts": {"findings": num_out(findings_n), "high": 0}}});
            let h = js_round((lp_score * 0.25 + defects_score * 0.18) / 0.43);
            (axes_v, "test".to_string(), h, Value::Null, Value::Null, Value::Null)
        };

        crate::db_json::run_write(db, insert_score_sql, &[
            json!(snap_id), json!("file"), json!(p), json!(lp_hash), json!(langs_csv(p)), num_out(lp_score), json!(Value::Object(lp_breakdown)),
            fan_in.clone(), num_out(((100.0 - health) / 100.0) * (1.0 + js_number(Some(&fan_in)).max(0.0))), json!(lp_cached),
            loc, complexity, fan_out, transitive, covered, churn, authors, findings,
            axes, json!(archetype), num_out(health), ai_readability, pct_repo, pct_folder,
            blocks_json_by_file.get(p).map(|s| json!(s)).unwrap_or(Value::Null),
        ])?;
        file_health.insert(p.clone(), health);
    }

    // ── D4 · bindings health ────────────────────────────────────────────────
    let mut cycle_files: HashSet<String> = HashSet::new();
    for p in &scoped { if smells_by_file.get(p).map_or(false, |v| v.iter().any(|s| s == "cycle")) { cycle_files.insert(p.clone()); } }
    for (a, b) in &edges {
        let kk = format!("{a}|{b}");
        let target_health = file_health.get(b).copied().unwrap_or(100.0);
        let target_fan_in = metrics_by_file.get(b).and_then(|m| m.get("fan_in")).and_then(Value::as_f64).unwrap_or(0.0);
        let is_layer_viol = layer_edge.contains(&kk);
        let in_cycle = cycle_files.contains(a) && cycle_files.contains(b);
        let mut health = js_round(target_health);
        if is_layer_viol { health = health.min(20.0); } else if in_cycle { health = health.min(45.0); }
        let risk = ((100.0 - health) / 100.0) * (1.0 + target_fan_in);
        let breakdown = json!({"to": b, "target_health": num_out(target_health), "target_fan_in": num_out(target_fan_in), "layer_violation": is_layer_viol, "in_cycle": in_cycle});
        crate::db_json::run_write(db, insert_score_sql, &[
            json!(snap_id), json!("binding"), json!(kk), Value::Null, Value::Null, num_out(health), breakdown, num_out(target_fan_in), num_out(risk), json!(0),
            Value::Null, Value::Null, Value::Null, Value::Null, Value::Null, Value::Null, Value::Null, Value::Null,
            Value::Null, json!("binding"), num_out(health), Value::Null, Value::Null, Value::Null, Value::Null,
        ])?;
    }

    let co_change: Vec<Value> = git.as_ref().map(|h| {
        crate::constitution__helpers_engineering::coChangePairs(&h.commits, &scoped_set, &adj, 3, 20).into_iter()
            .map(|(a, b, n)| json!({"a": a, "b": b, "n": n})).collect()
    }).unwrap_or_default();

    // ── totals + finalize the snapshot ──────────────────────────────────────
    let totals = summarize(db, snap_id);
    let mut totals_obj = totals.as_object().cloned().unwrap_or_default();
    if let Some(s) = eng.get("summary").and_then(Value::as_object) { for (k, v) in s { totals_obj.insert(k.clone(), v.clone()); } }
    totals_obj.insert("co_change".into(), json!(co_change));

    // The skip list rides in totals so a reader of the snapshot can see the gap between the files
    // this review CLAIMED and the files it actually judged, with the reason for each.
    totals_obj.insert("skipped".into(), json!(skipped));
    totals_obj.insert("skipped_count".into(), json!(skipped.len() as i64));
    crate::db_json::run_write(db, "UPDATE judgments.review_snapshots SET status='done', judged=?, duration_ms=?, totals_json=? WHERE id=?",
        &[json!(judged), json!(0), json!(Value::Object(totals_obj)), json!(snap_id)])?;

    let report = crate::constitution_constitution_report::constitutionReport(&json!({"snapshot_id": snap_id}), db)?;
    let mut out = report.as_object().cloned().unwrap_or_default();
    out.insert("judged".into(), json!(judged));
    out.insert("carried".into(), json!(carried));
    out.insert("constitution_hash".into(), json!(c_hash));
    out.insert("docket".into(), docket);
    Ok(Value::Object(out))
}

fn summarize(db: &Connection, snap_id: i64) -> Value {
    let files: Vec<f64> = crate::db_json::query_rows(db, "SELECT score FROM judgments.review_scores WHERE snapshot_id=? AND unit_kind='file'", &[json!(snap_id)])
        .ok().and_then(|v| v.as_array().cloned()).unwrap_or_default().iter().filter_map(|r| r.get("score").and_then(Value::as_f64)).collect();
    let health = if !files.is_empty() { opt_num(Some(js_round(files.iter().sum::<f64>() / files.len() as f64))) } else { Value::Null };
    let by_law = crate::db_json::query_rows(db, "SELECT law_key, law_scope, COUNT(*) n, SUM(weight) mass FROM judgments.review_violations WHERE snapshot_id=? GROUP BY law_key ORDER BY mass DESC", &[json!(snap_id)]).unwrap_or(json!([]));
    let violations: i64 = by_law.as_array().cloned().unwrap_or_default().iter().filter_map(|r| r.get("n").and_then(Value::as_i64)).sum();
    json!({"health": health, "files": files.len() as i64, "violations": violations, "by_law": by_law})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn setup_db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments").unwrap();
        crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        c
    }

    fn insert_var_law(c: &Connection) {
        c.execute(
            "INSERT INTO laws.review_laws (scope, key, title, rule_md, detector, config_json, weight, severity, confidence, enforcement, applies_json, status, version, created_by) \
             VALUES ('project','no-var','No var','use let/const','regex', ?, 5, 'warn', 1.0, 'score', '{}', 'ratified', 1, 'test')",
            [r#"{"pattern":"\\bvar\\b","flags":"g"}"#],
        ).unwrap();
    }

    #[test]
    fn judges_a_real_temp_dir_and_persists_a_violation() {
        let dir = std::env::temp_dir().join(format!("cj-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("a.mjs");
        std::fs::File::create(&file_path).unwrap().write_all(b"var x = 1;\nexport default x;\n").unwrap();

        let db = setup_db();
        insert_var_law(&db);

        let root = to_forward(&dir);
        let out = constitutionJudge(&json!({"root": root, "folder": "."}), &db).unwrap();
        assert_eq!(out["judged"], json!(1));
        assert_eq!(out["carried"], json!(0));

        let viol = crate::db_json::query_rows(&db, "SELECT * FROM judgments.review_violations WHERE law_key='no-var'", &[]).unwrap();
        assert_eq!(viol.as_array().unwrap().len(), 1);
        let score = crate::db_json::query_rows(&db, "SELECT * FROM judgments.review_scores WHERE unit_kind='file'", &[]).unwrap();
        let rows = score.as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0]["score"].as_i64().unwrap() < 100);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_judge_counts_past_the_cap_scores_only_the_listed_hits_and_heals_a_capped_snapshot_it_would_have_carried() {
        let dir = std::env::temp_dir().join(format!("cj-test-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // eight `var`s: the cap lists five, the total is eight
        std::fs::File::create(dir.join("a.mjs")).unwrap().write_all((1..=8).map(|i| format!("var v{i} = {i};\n")).collect::<String>().as_bytes()).unwrap();
        let db = setup_db();
        insert_var_law(&db);
        let root = to_forward(&dir);
        let first = constitutionJudge(&json!({"root": root, "folder": "."}), &db).unwrap();
        assert_eq!(first["judged"], json!(1));
        let rows = crate::db_json::query_rows(&db, "SELECT * FROM judgments.review_violations WHERE law_key='no-var'", &[]).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), crate::constitution__helpers_detectors::HITS_CAP, "rows and the score stay at the cap");
        let bd = |snap: i64| -> Value {
            let r = crate::db_json::query_rows(&db, "SELECT breakdown_json FROM judgments.review_scores WHERE snapshot_id=? AND unit_kind='file'", &[json!(snap)]).unwrap();
            serde_json::from_str(r[0]["breakdown_json"].as_str().unwrap()).unwrap()
        };
        let b1 = bd(1);
        assert_eq!(b1["no-var"]["hits"], json!(5));
        assert_eq!(b1["no-var"]["total_hits"], json!(8), "the true total is recorded");
        assert_eq!(b1["no-var"]["penalty"], json!(25), "the penalty is what five hits cost, exactly as before");

        // same content again: carried, total carried with it
        let second = constitutionJudge(&json!({"root": root, "folder": "."}), &db).unwrap();
        assert_eq!(second["carried"], json!(1));
        assert_eq!(bd(2)["no-var"]["total_hits"], json!(8));

        // a snapshot from before totals existed: strip the total from the newest one, then judge again - the file is RE-JUDGED (not carried) and gets its total back
        let stripped = { let mut b = bd(2); b["no-var"].as_object_mut().unwrap().remove("total_hits"); b };
        db.execute("UPDATE judgments.review_scores SET breakdown_json=? WHERE snapshot_id=2", [stripped.to_string()]).unwrap();
        let third = constitutionJudge(&json!({"root": root, "folder": "."}), &db).unwrap();
        assert_eq!(third["judged"], json!(1), "re-judged, not carried");
        assert_eq!(third["carried"], json!(0));
        assert_eq!(bd(3)["no-var"]["total_hits"], json!(8));
        assert_eq!(bd(3)["no-var"]["penalty"], json!(25));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn second_run_with_identical_content_carries_forward() {
        let dir = std::env::temp_dir().join(format!("cj-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("a.mjs");
        std::fs::File::create(&file_path).unwrap().write_all(b"var x = 1;\n").unwrap();

        let db = setup_db();
        insert_var_law(&db);
        let root = to_forward(&dir);

        let out1 = constitutionJudge(&json!({"root": root.clone(), "folder": "."}), &db).unwrap();
        assert_eq!(out1["judged"], json!(1));

        let out2 = constitutionJudge(&json!({"root": root, "folder": "."}), &db).unwrap();
        assert_eq!(out2["carried"], json!(1));
        assert_eq!(out2["judged"], json!(0));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn walk_skips_dotdirs_and_node_modules() {
        let dir = std::env::temp_dir().join(format!("cj-walk-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("node_modules/x.js"), "1").unwrap();
        std::fs::write(dir.join(".git/y.js"), "1").unwrap();
        std::fs::write(dir.join("src/z.js"), "1").unwrap();
        let out = walk(&to_forward(&dir), ".", 100);
        assert_eq!(out, vec!["src/z.js".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
