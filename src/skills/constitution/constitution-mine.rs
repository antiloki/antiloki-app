//! constitution-mine.rs — twin of skills/constitution/constitution-mine.mjs (db lane, cross-db
//! `laws`). NAMES ARE CONTRACT: `constitutionMine` (default). THE CONSTITUTION REVIEW's D2 miner
//! (docs/constitution-review-plan.html): a deterministic pattern census over a sampled set of
//! source files — for each candidate DIMENSION (file-naming case, shared error-helper adherence),
//! measure the dominant house choice and its adherence; a dimension crossing `threshold` becomes a
//! `candidate` law (never auto-ratified) carrying its evidence (count + exemplar files).
//!
//! REAL FILESYSTEM (fixture-controllable, stack-facts.rs / constitution-estimate.rs precedent):
//! `root` is DB-derived via the already-converted `engine_workspace_root::getWorkspaceRoot` twin
//! (when `input.root` is absent — the common case and the only one this unit's fixture exercises),
//! so a fixture seeds an active `workspaces` row pointing at a real on-disk testdata dir
//! (`antiloki-crossing/fixtures/testdata/constitution-mine-*/`, committed alongside this unit's
//! fixture JSON) and both lanes walk/read the identical bytes. `walk()` mirrors
//! `constitution-estimate.rs`'s (same WALK_SKIP/CODE_RE/bounded-DFS-then-sort) but this module's
//! OWN JS source additionally `.slice(0, cap)`s INSIDE `walk()` itself (constitution-estimate.mjs's
//! walk does not — capping happens in ITS caller instead), so this twin's `walk()` truncates to cap
//! after sorting where the sibling's does not — verified against real `node` output, not assumed.
//!
//! `input.root` truthy branch (`resolve(input.root)`) — DISCLOSED UNTESTED (playbook lesson 5's
//! flag-skippable pattern, same spirit as constitution-estimate.rs's git-diff branch): no fixture
//! case sets `input.root` (every case goes through the DB-derived `workspaces` row instead, which
//! already exercises the exact same downstream walk/read code). `resolve_root` below is a
//! best-effort `node:path resolve()` port (absolute passthrough / join-with-cwd) for production
//! parity, not a byte-exact port of `engine_workspace_root`'s private win32 normalize (that copy
//! stays private there — duplicating the full algorithm for an untested branch isn't worth it).
//!
//! `context.db` missing → JS throws `err.required('constitution-mine', 'context.db')`. Not
//! modeled: `db: &Connection` is a required, non-nullable reference here — same "guard-throws are
//! not exercised" convention used by every sibling twin.
//!
//! DDL EXECUTED, NOT RETURNED (frozen-clock mirror): the upsert's `ON CONFLICT ... SET
//! updated_at=datetime('now')` is EXECUTED SQL, so per the playbook's frozen-clock rule the
//! executed copy pins the base instant (`datetime('2026-01-01 00:00:00')`) — the conform JS lane's
//! db proxy rewrites executed SQL identically.
//!
//! All 7 fixture cases (+ the config/evidence JSON they write into `laws.review_laws`) were cross-
//! checked against a real `node` run of the JS source over the committed testdata dirs before this
//! twin was written, not hand-derived — see the fixture's cases for the exact expected shapes.
// QUALITY: 80/100 — two independent pure-counter dimensions (naming case census, error-helper
// adherence) over a real fs walk+read, fixture-backed via committed testdata dirs; one disclosed-
// untested branch (input.root override, flag-skippable per lesson 5).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } } // = JS String(v)
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
// JS numbers don't distinguish int/float — a whole-valued ratio (adherence/dismiss-style) must
// render "1"/"0", not "1.0" (the 100%-adherence fixture case exercises this).
fn js_num(n: f64) -> Value { if n.is_finite() && n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 { json!(n as i64) } else { json!(n) } }
// JS `Math.round`: floor(x + 0.5) — rounds HALF UP, unlike Rust's half-away-from-zero `.round()`.
fn js_round(x: f64) -> f64 { (x + 0.5).floor() }
fn js_is_integer(v: Option<&Value>) -> bool { matches!(v, Some(Value::Number(n)) if n.as_f64().map_or(false, |f| f.is_finite() && f.fract() == 0.0)) }
fn js_finite_number(v: Option<&Value>) -> Option<f64> { match v { Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite()), _ => None } }

// Delegates to the ONE capability list (ast-parse's adapter table filtered to grammars that
// actually have a walk profile). This used to be a private copy of
// `(mjs|cjs|js|jsx|ts|tsx|py)`; four skills each held one, none ever gained `rs`, and the
// review pass walked straight past every Rust file while reporting success.
fn code_re() -> &'static regex::Regex { crate::codebase_ast_parse::analyzable_ext_regex() }
fn test_path_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"\.(test|spec)\.\w+$|(^|/)(tests?|__tests__)/").unwrap())
}
fn error_module_path_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"(^|/)error(s)?\.(mjs|js|ts)$").unwrap())
}
fn error_export_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"(?i)export\s+(const|function)\s+\w*err").unwrap())
}
fn imports_err_from_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r#"from\s+['"][^'"]*error[^'"]*['"]"#).unwrap())
}
fn imports_err_require_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r#"require\(['"][^'"]*error[^'"]*['"]\)"#).unwrap())
}
fn raw_throw_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"throw\s+new\s+Error\s*\(").unwrap())
}

const WALK_SKIP: [&str; 8] = ["node_modules", ".git", "dist", "build", "coverage", ".antiloki", ".next", "out"];

/// `.slice(0, end)` on a Vec<String> (JS negative `end` counts back from the length).
fn js_slice0(arr: Vec<String>, end: i64) -> Vec<String> {
    let len = arr.len() as i64;
    let e = if end < 0 { (len + end).max(0) } else { end.min(len) };
    arr.into_iter().take(e.max(0) as usize).collect()
}

/// `function walk(root, rel, cap)` — bounded DFS via readdir, skipping WALK_SKIP/dot dirs,
/// collecting CODE_RE-matching file names as root-relative forward-slash paths, sorted THEN
/// capped to `cap` (this module's own JS `walk` does `.slice(0, cap)` at the end — unlike
/// constitution-estimate.mjs's walk, which caps in its caller instead; see module doc).
fn walk(root: &str, rel_folder: &str, cap: i64) -> Vec<String> {
    let (base_dir, init_prefix) = if rel_folder == "." {
        (PathBuf::from(root), String::new())
    } else {
        (PathBuf::from(root).join(rel_folder), rel_folder.to_string())
    };
    let limit: usize = if cap < 0 { 0 } else { cap.saturating_mul(4).max(0) as usize };
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<(PathBuf, String)> = vec![(base_dir, init_prefix)];
    while let Some((dir, prefix)) = stack.pop() {
        if out.len() >= limit { break; }
        let entries = match std::fs::read_dir(&dir) { Ok(e) => e, Err(_) => continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let child_rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
            if is_dir {
                if !WALK_SKIP.contains(&name.as_str()) && !name.starts_with('.') {
                    stack.push((dir.join(&name), child_rel));
                }
            } else if code_re().is_match(&name) {
                out.push(child_rel);
            }
        }
    }
    out.sort();
    js_slice0(out, cap)
}

/// `.replace(/\.\w+$/, '')`: strip a trailing "." + word-chars run (the last extension) — a `.`
/// not immediately followed by an all-word-char run to end of string is left alone.
fn strip_last_ext(name: &str) -> String {
    if let Some(idx) = name.rfind('.') {
        let rest = &name[idx + 1..];
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return name[..idx].to_string();
        }
    }
    name.to_string()
}

/// `const caseOf = (name) => { ... }` — dominant basename case (kebab/camel/snake/other).
fn case_of(name: &str) -> &'static str {
    let s1 = strip_last_ext(name);
    let stem = if let Some(t) = s1.strip_suffix(".test") { t.to_string() }
        else if let Some(t) = s1.strip_suffix(".spec") { t.to_string() }
        else if let Some(t) = s1.strip_suffix(".d") { t.to_string() }
        else { s1 };
    let has_upper = stem.bytes().any(|c| c.is_ascii_uppercase());
    let has_dash = stem.contains('-');
    let has_underscore = stem.contains('_');
    if has_upper && !has_dash && !has_underscore { return "camel"; }
    if has_underscore { return "snake"; }
    if has_dash || (!stem.is_empty() && stem.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())) { return "kebab"; }
    "other"
}

fn basename(path: &str) -> &str { path.rsplit('/').next().unwrap_or(path) }

/// `String(input?.folder ?? '.').trim() || '.'`
fn resolved_folder(input: &Value) -> String {
    let raw = match nz(input.get("folder")) { Some(v) => js_string(v), None => ".".to_string() };
    let trimmed = raw.trim();
    if trimmed.is_empty() { ".".to_string() } else { trimmed.to_string() }
}

/// `resolve(input.root)` — see module doc: disclosed-untested branch, best-effort port.
fn resolve_root(input_root: &str) -> String {
    let p = Path::new(input_root);
    if p.is_absolute() {
        p.to_string_lossy().into_owned()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(p).to_string_lossy().into_owned(),
            Err(_) => input_root.to_string(),
        }
    }
}

#[derive(Default)]
struct CaseCounts { kebab: i64, camel: i64, snake: i64, other: i64 }
impl CaseCounts {
    fn get(&self, k: &str) -> i64 { match k { "kebab" => self.kebab, "camel" => self.camel, "snake" => self.snake, _ => self.other } }
    fn bump(&mut self, k: &str) { match k { "kebab" => self.kebab += 1, "camel" => self.camel += 1, "snake" => self.snake += 1, _ => self.other += 1 } }
}
#[derive(Default)]
struct Offenders { kebab: Vec<String>, camel: Vec<String>, snake: Vec<String> }
impl Offenders {
    fn get(&self, k: &str) -> &Vec<String> { match k { "kebab" => &self.kebab, "camel" => &self.camel, _ => &self.snake } }
    fn push(&mut self, k: &str, p: String) { match k { "kebab" => self.kebab.push(p), "camel" => self.camel.push(p), "snake" => self.snake.push(p), _ => {} } }
}

/// `export default function constitutionMine(input, context)`
pub fn constitutionMine(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(db);
    let root: String = if truthy(input.get("root")) {
        resolve_root(&js_string(input.get("root").unwrap()))
    } else {
        crate::engine_workspace_root::getWorkspaceRoot(db).as_str().unwrap_or_default().to_string()
    };
    let cap: i64 = if js_is_integer(input.get("max_files")) {
        input.get("max_files").and_then(Value::as_f64).unwrap_or(600.0) as i64
    } else {
        600
    };
    let threshold: f64 = js_finite_number(input.get("threshold")).unwrap_or(0.8);
    let folder = resolved_folder(input);
    let paths = walk(&root, &folder, cap);

    let mut files: Vec<(String, String)> = Vec::new();
    for p in &paths {
        if let Ok(src) = std::fs::read_to_string(Path::new(&root).join(p)) {
            files.push((p.clone(), src));
        }
    }
    let test_re = test_path_re();
    let non_test: Vec<(String, String)> = files.into_iter().filter(|(p, _)| !test_re.is_match(p)).collect();

    let mut candidates: Vec<Value> = Vec::new();
    let mut census = Map::new();

    // ── dimension: file-naming case ─────────────────────────────────────────
    let mut counts = CaseCounts::default();
    let mut offenders = Offenders::default();
    for (p, _) in &non_test {
        let c = case_of(basename(p));
        counts.bump(c);
        if c != "other" {
            for other in ["kebab", "camel", "snake"] {
                if other != c { offenders.push(other, p.clone()); }
            }
        }
    }
    let named_total = counts.kebab + counts.camel + counts.snake;
    let mut naming_census = Map::new();
    naming_census.insert("kebab".into(), json!(counts.kebab));
    naming_census.insert("camel".into(), json!(counts.camel));
    naming_census.insert("snake".into(), json!(counts.snake));
    naming_census.insert("other".into(), json!(counts.other));
    naming_census.insert("total".into(), json!(named_total));
    census.insert("naming".into(), Value::Object(naming_census));

    if named_total >= 10 {
        let order = ["kebab", "camel", "snake"];
        let mut sorted = order.to_vec();
        sorted.sort_by(|a, b| counts.get(*b).cmp(&counts.get(*a))); // stable: ties keep kebab,camel,snake order
        let dominant = sorted[0];
        let adherence = counts.get(dominant) as f64 / named_total as f64;
        if adherence >= threshold {
            let not_dominant: Vec<Value> = offenders.get(dominant).iter().take(6).map(|s| json!(s)).collect();
            let pattern_for = |d: &str| match d { "kebab" => "[A-Z_]", "camel" => "-", _ => "[A-Z-]" };

            let mut config = Map::new();
            config.insert("pattern".into(), json!(format!("/[^/]*{}[^/]*\\.(mjs|cjs|js|jsx|ts|tsx)$", pattern_for(dominant))));

            let mut evidence = Map::new();
            evidence.insert("pct".into(), js_num(adherence));
            evidence.insert("count_for".into(), json!(counts.get(dominant)));
            evidence.insert("count_against".into(), json!(named_total - counts.get(dominant)));
            evidence.insert("exemplars".into(), json!(not_dominant));

            let mut cand = Map::new();
            cand.insert("key".into(), json!(format!("code-naming-{dominant}")));
            cand.insert("scope".into(), json!("project"));
            cand.insert("title".into(), json!(format!("File names should be {dominant}-case")));
            cand.insert("rule_md".into(), json!(format!("{}% of source files use {dominant}-case names — the house convention.", js_round(adherence * 100.0) as i64)));
            cand.insert("detector".into(), json!("path"));
            cand.insert("config".into(), Value::Object(config));
            cand.insert("weight".into(), json!(3));
            cand.insert("evidence".into(), Value::Object(evidence));
            candidates.push(Value::Object(cand));
        }
    }

    // ── dimension: shared error-helper adherence ────────────────────────────
    let error_module: Option<(String, String)> = non_test
        .iter()
        .find(|(p, s)| error_module_path_re().is_match(p) && error_export_re().is_match(s))
        .cloned();
    if let Some((mod_path, _mod_src)) = error_module {
        let imports_err = |src: &str| imports_err_from_re().is_match(src) || imports_err_require_re().is_match(src);
        let mut using_helper: i64 = 0;
        let mut raw_sites: Vec<String> = Vec::new();
        for (p, s) in &non_test {
            if p == &mod_path { continue; }
            if imports_err(s) { using_helper += 1; }
            if raw_throw_re().is_match(s) && !imports_err(s) { raw_sites.push(p.clone()); }
        }
        let throwers = using_helper + raw_sites.len() as i64;

        let mut eh_census = Map::new();
        eh_census.insert("module".into(), json!(mod_path));
        eh_census.insert("using_helper".into(), json!(using_helper));
        eh_census.insert("raw_sites".into(), json!(raw_sites.len() as i64));
        census.insert("errorHelper".into(), Value::Object(eh_census));

        if throwers >= 8 && (using_helper as f64 / throwers as f64) >= threshold {
            let ratio = using_helper as f64 / throwers as f64;
            let exemplars: Vec<Value> = raw_sites.iter().take(6).map(|s| json!(s)).collect();

            let mut config = Map::new();
            config.insert("pattern".into(), json!(r"throw\s+new\s+Error\s*\("));

            let mut evidence = Map::new();
            evidence.insert("pct".into(), js_num(ratio));
            evidence.insert("count_for".into(), json!(using_helper));
            evidence.insert("count_against".into(), json!(raw_sites.len() as i64));
            evidence.insert("exemplars".into(), json!(exemplars));

            let mut cand = Map::new();
            cand.insert("key".into(), json!("code-use-error-helper"));
            cand.insert("scope".into(), json!("project"));
            cand.insert("title".into(), json!("Throw via the shared error helper, not raw `new Error`"));
            cand.insert(
                "rule_md".into(),
                json!(format!(
                    "{using_helper} of {throwers} throwing files use the shared error module ({mod_path}); raw `throw new Error` bypasses the audited error taxonomy."
                )),
            );
            cand.insert("detector".into(), json!("regex"));
            cand.insert("config".into(), Value::Object(config));
            cand.insert("weight".into(), json!(4));
            cand.insert("evidence".into(), Value::Object(evidence));
            candidates.push(Value::Object(cand));
        }
    }

    // ── persist candidates (idempotent by key; refresh evidence) ───────────
    let mut proposed: i64 = 0;
    for c in &candidates {
        let scope = c.get("scope").and_then(Value::as_str).unwrap_or("");
        let key = c.get("key").and_then(Value::as_str).unwrap_or("");
        let title = c.get("title").and_then(Value::as_str).unwrap_or("");
        let rule_md = c.get("rule_md").and_then(Value::as_str).unwrap_or("");
        let detector = c.get("detector").and_then(Value::as_str).unwrap_or("");
        let config_json = serde_json::to_string(c.get("config").unwrap_or(&Value::Null)).unwrap_or_default();
        let weight = c.get("weight").and_then(Value::as_i64).unwrap_or(0);
        let evidence_json = serde_json::to_string(c.get("evidence").unwrap_or(&Value::Null)).unwrap_or_default();
        crate::db_json::run_write(
            db,
            &format!("INSERT INTO laws.review_laws (scope, key, title, rule_md, detector, config_json, weight, enforcement, applies_json, status, evidence_json, created_by) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 'score', ?, 'candidate', ?, 'mined') \
             ON CONFLICT(key) DO UPDATE SET evidence_json=excluded.evidence_json, rule_md=excluded.rule_md, updated_at={} \
               WHERE laws.review_laws.status='candidate'", crate::clock::now_sql()),
            &[
                json!(scope),
                json!(key),
                json!(title),
                json!(rule_md),
                json!(detector),
                json!(config_json),
                json!(weight),
                json!(r#"{"languages":["js","ts"]}"#),
                json!(evidence_json),
            ],
        )?;
        proposed += 1;
    }

    let mut out = Map::new();
    out.insert("ok".into(), json!(true));
    out.insert("proposed".into(), json!(proposed));
    out.insert("candidates".into(), json!(candidates));
    out.insert("census".into(), Value::Object(census));
    out.insert("files_sampled".into(), json!(non_test.len() as i64));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = concat!(env!("ANTILOKI_FIXTURES"), "");

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments").unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        )
        .unwrap();
        c
    }
    fn seed_active(c: &Connection, root: &str) {
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('x', ?1, 1)", rusqlite::params![root]).unwrap();
    }

    #[test]
    fn both_dimensions_fire_at_default_threshold() {
        let c = db();
        seed_active(&c, &format!("{BASE}/constitution-mine-both"));
        let r = constitutionMine(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["proposed"], json!(2));
        assert_eq!(r["files_sampled"], json!(11));
        assert_eq!(r["census"]["naming"], json!({ "kebab": 9, "camel": 2, "snake": 0, "other": 0, "total": 11 }));
        assert_eq!(r["census"]["errorHelper"], json!({ "module": "error.mjs", "using_helper": 7, "raw_sites": 1 }));
        let cands = r["candidates"].as_array().unwrap();
        assert_eq!(cands.len(), 2);
        assert_eq!(cands[0]["key"], json!("code-naming-kebab"));
        assert_eq!(cands[0]["rule_md"], json!("82% of source files use kebab-case names — the house convention."));
        assert_eq!(cands[0]["evidence"]["pct"], json!(9.0 / 11.0));
        assert_eq!(cands[0]["evidence"]["exemplars"], json!(["fillerAlpha.mjs", "fillerBeta.mjs"]));
        assert_eq!(cands[1]["key"], json!("code-use-error-helper"));
        assert_eq!(cands[1]["rule_md"], json!("7 of 8 throwing files use the shared error module (error.mjs); raw `throw new Error` bypasses the audited error taxonomy."));
        assert_eq!(cands[1]["evidence"]["pct"], json!(0.875));
        assert_eq!(cands[1]["evidence"]["exemplars"], json!(["raw-one.mjs"]));

        let rows = crate::db_json::query_rows(&c, "SELECT key, status, config_json, evidence_json FROM laws.review_laws ORDER BY key", &[]).unwrap();
        assert_eq!(rows[0]["key"], json!("code-naming-kebab"));
        assert_eq!(rows[0]["status"], json!("candidate"));
        assert_eq!(rows[0]["config_json"], json!("{\"pattern\":\"/[^/]*[A-Z_][^/]*\\\\.(mjs|cjs|js|jsx|ts|tsx)$\"}"));
        assert_eq!(rows[1]["key"], json!("code-use-error-helper"));
    }

    #[test]
    fn max_files_caps_before_either_dimension_crosses_its_threshold() {
        let c = db();
        seed_active(&c, &format!("{BASE}/constitution-mine-both"));
        let r = constitutionMine(&json!({ "max_files": 5 }), &c).unwrap();
        assert_eq!(r["proposed"], json!(0));
        assert_eq!(r["candidates"], json!([]));
        assert_eq!(r["files_sampled"], json!(5));
        assert_eq!(r["census"]["naming"], json!({ "kebab": 3, "camel": 2, "snake": 0, "other": 0, "total": 5 }));
        assert_eq!(r["census"]["errorHelper"], json!({ "module": "error.mjs", "using_helper": 2, "raw_sites": 0 }));
    }

    #[test]
    fn hundred_percent_adherence_renders_pct_as_a_bare_integer() {
        let c = db();
        seed_active(&c, &format!("{BASE}/constitution-mine-100pct"));
        let r = constitutionMine(&json!({}), &c).unwrap();
        assert_eq!(r["proposed"], json!(1));
        let cand = &r["candidates"][0];
        assert_eq!(cand["rule_md"], json!("100% of source files use kebab-case names — the house convention."));
        assert_eq!(cand["evidence"]["pct"], json!(1)); // NOT 1.0
        assert_eq!(cand["evidence"]["count_against"], json!(0));
        assert_eq!(cand["evidence"]["exemplars"], json!([]));
        assert!(r["census"].get("errorHelper").is_none()); // no error.mjs in this dir -> key absent

        let stored = crate::db_json::query_rows(&c, "SELECT evidence_json FROM laws.review_laws WHERE key='code-naming-kebab'", &[]).unwrap();
        assert_eq!(stored[0]["evidence_json"], json!("{\"pct\":1,\"count_for\":10,\"count_against\":0,\"exemplars\":[]}"));
    }

    #[test]
    fn below_threshold_dimensions_populate_census_but_propose_nothing() {
        let c = db();
        seed_active(&c, &format!("{BASE}/constitution-mine-under-threshold"));
        let r = constitutionMine(&json!({}), &c).unwrap();
        assert_eq!(r["proposed"], json!(0));
        assert_eq!(r["candidates"], json!([]));
        assert_eq!(r["census"]["naming"], json!({ "kebab": 10, "camel": 4, "snake": 0, "other": 0, "total": 14 }));
        assert_eq!(r["census"]["errorHelper"], json!({ "module": "error.mjs", "using_helper": 5, "raw_sites": 4 }));
    }

    #[test]
    fn custom_threshold_flips_both_dimensions_to_firing_on_the_same_dir() {
        let c = db();
        seed_active(&c, &format!("{BASE}/constitution-mine-under-threshold"));
        let r = constitutionMine(&json!({ "threshold": 0.5 }), &c).unwrap();
        assert_eq!(r["proposed"], json!(2));
        let cands = r["candidates"].as_array().unwrap();
        assert_eq!(cands[0]["rule_md"], json!("71% of source files use kebab-case names — the house convention."));
        assert_eq!(cands[0]["evidence"]["pct"], json!(10.0 / 14.0));
        assert_eq!(cands[0]["evidence"]["exemplars"], json!(["fillerFour.mjs", "fillerOne.mjs", "fillerThree.mjs", "fillerTwo.mjs"]));
        assert_eq!(cands[1]["evidence"]["pct"], json!(5.0 / 9.0));
        assert_eq!(cands[1]["evidence"]["exemplars"], json!(["raw-four.mjs", "raw-one.mjs", "raw-three.mjs", "raw-two.mjs"]));
    }

    #[test]
    fn walk_skip_dirs_test_file_exclusion_and_other_bucket() {
        let c = db();
        seed_active(&c, &format!("{BASE}/constitution-mine-small"));
        let r = constitutionMine(&json!({}), &c).unwrap();
        assert_eq!(r["proposed"], json!(0));
        assert_eq!(r["files_sampled"], json!(10)); // node_modules/.git/dist/README/foo.test.mjs all excluded
        assert_eq!(r["census"]["naming"], json!({ "kebab": 9, "camel": 0, "snake": 0, "other": 1, "total": 9 }));
        assert_eq!(r["census"]["errorHelper"], json!({ "module": "error.mjs", "using_helper": 2, "raw_sites": 0 })); // throwers=2 < 8, short-circuits before the ratio check
    }

    #[test]
    fn folder_scoping_keeps_root_relative_prefixed_paths() {
        let c = db();
        seed_active(&c, &format!("{BASE}/constitution-mine-folder"));
        let r = constitutionMine(&json!({ "folder": "sub" }), &c).unwrap();
        assert_eq!(r["files_sampled"], json!(3)); // top.mjs (outside "sub") excluded
        assert_eq!(r["census"]["naming"], json!({ "kebab": 2, "camel": 1, "snake": 0, "other": 0, "total": 3 }));
        assert!(r["census"].get("errorHelper").is_none());
    }

    #[test]
    fn upsert_is_idempotent_by_key_and_only_refreshes_candidate_status_rows() {
        let c = db();
        seed_active(&c, &format!("{BASE}/constitution-mine-100pct"));
        constitutionMine(&json!({}), &c).unwrap();
        // ratify the mined law out-of-band (as an operator would) — a second mine run must NOT
        // clobber a ratified row's evidence (the upsert's WHERE status='candidate' guard).
        c.execute("UPDATE laws.review_laws SET status='ratified' WHERE key='code-naming-kebab'", []).unwrap();
        let r = constitutionMine(&json!({}), &c).unwrap();
        assert_eq!(r["proposed"], json!(1)); // still counted as proposed regardless of the guarded UPDATE outcome
        let rows = crate::db_json::query_rows(&c, "SELECT status FROM laws.review_laws WHERE key='code-naming-kebab'", &[]).unwrap();
        assert_eq!(rows[0]["status"], json!("ratified")); // untouched by the guarded ON CONFLICT UPDATE
        let count = crate::db_json::query_rows(&c, "SELECT COUNT(*) AS n FROM laws.review_laws", &[]).unwrap();
        assert_eq!(count[0]["n"], json!(1)); // no duplicate row inserted
    }

    #[test]
    fn case_of_classifies_kebab_camel_snake_and_other() {
        assert_eq!(case_of("foo-bar.mjs"), "kebab");
        assert_eq!(case_of("fooBar.mjs"), "camel");
        assert_eq!(case_of("foo_bar.mjs"), "snake");
        assert_eq!(case_of("index.mjs"), "kebab");
        assert_eq!(case_of("weird.thing.mjs"), "other");
        assert_eq!(case_of("foo.test.mjs"), "kebab");
        assert_eq!(case_of("fooBar.test.mjs"), "camel");
        assert_eq!(case_of("FOO_BAR.mjs"), "snake");
    }
}
