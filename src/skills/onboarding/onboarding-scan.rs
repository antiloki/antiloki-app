//! onboarding-scan.rs — twin of skills/onboarding/onboarding-scan.mjs. NAMES ARE CONTRACT:
//! `walk`, `detectStack`, `scanGitHistory`, `harvestDocs` (named exports) + `onboardingScan`
//! (default). `STACK_MARKERS` is a plain data const, not a function — not a census unit.
//!
//! BESPOKE-NATIVE (`scanGitHistory` + `onboardingScan`; code-git-info.rs/code-authors.rs
//! precedent, playbook's "not conditionally avoidable" rule): `scanGitHistory` ALWAYS shells out
//! to `git log` — unlike code-authors.mjs's `input.__log` test seam or constitution-estimate.mjs's
//! `input.base`-gated git-diff branch, there is no input flag that selects a git-free path; even
//! the `available:false` degrade (non-git dir / git missing) still executes the real subprocess,
//! it just reads a nonzero/null exit status. Since `onboardingScan`'s default export
//! UNCONDITIONALLY calls `scanGitHistory` on every invocation, both units are bespoke-native: real,
//! correct `std::process::Command` code below, proven by `#[cfg(test)]` native tests against real
//! temp-dir git repos, NO conform-db fixture/frontier entry for either.
//!
//! `walk`, `detectStack`, `harvestDocs` carry NO subprocess and ARE fixture-tested as no-db
//! helpers: `walk`/`harvestDocs` read the real filesystem over a literal `root` path positional
//! arg pointing at a committed testdata dir (design-css-compile.rs precedent — literal path per
//! case, NOT the shared-single-root FS-injection lane, since these are positional-arg helpers, not
//! `(input, context)` standard-lane functions). `detectStack` is pure data (no fs at all).
//!
//! `walk()`'s directory listing is NEVER re-sorted by the JS source (unlike constitution-estimate's
//! `walk()`, which explicitly `.sort()`s at the end) — output order is the raw OS `readdir`
//! enumeration order. Both lanes call the OS's raw directory enumeration
//! (`readdirSync`/`fs::read_dir`) over the SAME on-disk testdata dir with no language-level
//! reordering on either side; design-css-compile.rs's module doc documents this as empirically
//! confirmed-matching (NTFS's stable enumeration order) on this same dev machine — same
//! precedent/risk profile assumed here, not independently re-verified this batch (self_flag).
//!
//! `path.extname()` (used by `walk`) is ported via the exact upstream Node algorithm specialized
//! to a bare filename (no path separators — `e.name` is always a bare `readdir` entry name, never
//! a path), which sidesteps needing the `startPart`/`matchedSlash` slash-scanning machinery.
//!
//! `path.resolve(repo_path)` (used by `onboardingScan`) is ported as a SIMPLIFIED
//! absolute-path-passthrough / join-with-cwd (no `..`/`.` segment normalization) — acceptable
//! because this whole function is bespoke-native (not conform-db fixtured) and every real caller
//! (and every native test below) passes an already-absolute, already-clean `repo_path`.
// QUALITY: bespoke-native default+scanGitHistory (real git subprocess, code-git-info.rs precedent)
// — no fixture/frontier for those two; walk/detectStack/harvestDocs are fixture-backed (real fs
// for walk/harvestDocs via committed testdata dirs, pure data for detectStack).

use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand

const IGNORE_DIRS: [&str; 14] = [
    "node_modules", ".git", "dist", "build", "out", "target", ".next",
    "__pycache__", ".venv", "venv", ".cache", ".parcel-cache", ".idea", ".vscode",
];
const CONFIG_FILES: [&str; 15] = [
    "package.json", "tsconfig.json", "Cargo.toml", "go.mod", "requirements.txt",
    "Pipfile", "pyproject.toml", "pom.xml", "build.gradle", "Gemfile", "composer.json",
    ".eslintrc", "prettier.config.js", "jest.config.js", "vitest.config.ts",
];
// (name, lang, ecosystem) — ecosystem None ⇒ the JS object literal never defined that key (tsconfig.json).
const STACK_MARKERS: &[(&str, &str, Option<&str>)] = &[
    ("package.json", "javascript", Some("npm")),
    ("tsconfig.json", "typescript", None),
    ("Cargo.toml", "rust", Some("cargo")),
    ("go.mod", "go", Some("gomod")),
    ("requirements.txt", "python", Some("pip")),
    ("Pipfile", "python", Some("pipenv")),
    ("pyproject.toml", "python", Some("poetry")),
    ("Gemfile", "ruby", Some("bundler")),
    ("composer.json", "php", Some("composer")),
];

fn test_hints_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"(?i)(__tests__|\.test\.|\.spec\.|_test\.|tests?/)").unwrap())
}
fn doc_hints_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"(?i)(README|CONTRIBUTING|ARCHITECTURE|ADR|docs?/)").unwrap())
}
fn doc_basename_prefix_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"(?i)^(README|CONTRIBUTING|ARCHITECTURE)").unwrap())
}
fn leading_hash_re() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"^#\s*").unwrap())
}

/// `node:path` win32/posix `basename()` for a `/`- or `\`-joined relative path — splits on either
/// separator, takes the last non-empty segment.
fn basename(p: &str) -> String {
    p.rsplit(['/', '\\']).next().unwrap_or("").to_string()
}

/// `node:path` `extname()` specialized to a bare filename (no separators) — the exact upstream
/// scan-backward algorithm with `startPart` fixed at 0 and slash handling elided (never reachable
/// for a bare `readdir` entry name).
fn extname(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let len = chars.len();
    if len == 0 { return String::new(); }
    let end: i64 = len as i64;
    let mut start_dot: i64 = -1;
    let mut pre_dot_state: i32 = 0;
    for i in (0..len).rev() {
        let c = chars[i];
        if c == '.' {
            if start_dot == -1 { start_dot = i as i64; }
            else if pre_dot_state != 1 { pre_dot_state = 1; }
        } else if start_dot != -1 {
            pre_dot_state = -1;
        }
    }
    if start_dot == -1 || pre_dot_state == 0 || (pre_dot_state == 1 && start_dot == end - 1 && start_dot == 1) {
        return String::new();
    }
    chars[start_dot as usize..end as usize].iter().collect()
}

/// `function walk(root, rel, cap)`'s inner `_w` — bounded DFS via `readdirSync(withFileTypes)`,
/// skipping IGNORE_DIRS/dot-prefixed entries (both dirs AND files), classifying each file by
/// TEST_HINTS → DOC_HINTS → CONFIG_FILES → 'source'. `files`/`prefix` accumulate root-relative
/// forward-slash paths incrementally during the DFS (constitution-estimate.rs precedent) rather
/// than reimplementing `path.relative()`.
fn walk_dir(dir: &Path, prefix: &str, files: &mut Vec<Value>, cap: i64) {
    if files.len() as i64 >= cap { return; }
    let entries = match std::fs::read_dir(dir) { Ok(e) => e, Err(_) => return };
    for entry in entries {
        if files.len() as i64 >= cap { break; }
        let entry = match entry { Ok(e) => e, Err(_) => continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if IGNORE_DIRS.contains(&name.as_str()) || name.starts_with('.') { continue; }
        let child_path = dir.join(&name);
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let child_rel = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        if is_dir {
            walk_dir(&child_path, &child_rel, files, cap);
            continue;
        }
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        if is_file {
            let ext = extname(&name);
            let kind = if test_hints_re().is_match(&child_rel) { "test" }
                else if doc_hints_re().is_match(&child_rel) { "doc" }
                else if CONFIG_FILES.contains(&name.as_str()) { "config" }
                else { "source" };
            let size = std::fs::metadata(&child_path).map(|m| m.len()).unwrap_or(0);
            files.push(json!({ "path": child_rel, "ext": ext, "kind": kind, "size": size as i64 }));
        }
    }
}

/// `export function walk(root, maxFiles = 5000)`. `max_files` non-number (incl. the harness's
/// missing-arg `Value::Null`, matching JS's `undefined ⇒ default`) falls back to 5000; a real JS
/// `null`/other-typed override is not distinguished from "omitted" by the harness's positional-arg
/// dispatch (self_flag: production only ever calls `walk(root)`, so this is not exercised).
pub fn walk(root: &Value, max_files: &Value) -> Value {
    let root_s = root.as_str().unwrap_or_default();
    let cap: i64 = match max_files { Value::Number(n) => n.as_i64().unwrap_or(5000), _ => 5000 };
    let mut files: Vec<Value> = Vec::new();
    walk_dir(Path::new(root_s), "", &mut files, cap);
    Value::Array(files)
}

fn stack_marker(name: &str) -> Option<(&'static str, Option<&'static str>)> {
    STACK_MARKERS.iter().find(|(n, _, _)| *n == name).map(|&(_, l, e)| (l, e))
}

/// `export function detectStack(root, files)` — `root` is unused by the JS body (dead param, kept
/// for signature parity). Insertion-order-preserving language/ecosystem dedup mirrors JS `Set`
/// iteration order (first-seen wins).
pub fn detectStack(root: &Value, files: &Value) -> Value {
    let _ = root;
    let mut languages: Vec<String> = Vec::new();
    let mut ecosystems: Vec<String> = Vec::new();
    let mut markers: Vec<Value> = Vec::new();
    if let Some(arr) = files.as_array() {
        for f in arr {
            let path = f.get("path").and_then(Value::as_str).unwrap_or("");
            let name = basename(path);
            if let Some((lang, eco)) = stack_marker(&name) {
                let mut m = Map::new();
                m.insert("path".into(), json!(path));
                m.insert("lang".into(), json!(lang));
                if let Some(e) = eco { m.insert("ecosystem".into(), json!(e)); }
                markers.push(Value::Object(m));
                if !languages.iter().any(|l| l == lang) { languages.push(lang.to_string()); }
                if let Some(e) = eco {
                    if !ecosystems.iter().any(|x| x == e) { ecosystems.push(e.to_string()); }
                }
            }
        }
    }
    json!({ "languages": languages, "ecosystems": ecosystems, "markers": markers })
}

/// `export function harvestDocs(root, files)` — real fs read over `f.path` joined to `root`.
/// Included iff `f.kind === 'doc'` OR `basename(f.path)` starts with README/CONTRIBUTING/ARCHITECTURE
/// (case-insensitive); a read failure (ENOENT etc) is silently skipped (JS `catch { /* skip */ }`).
pub fn harvestDocs(root: &Value, files: &Value) -> Value {
    let root_s = root.as_str().unwrap_or_default();
    let mut docs: Vec<Value> = Vec::new();
    if let Some(arr) = files.as_array() {
        for f in arr {
            let path = f.get("path").and_then(Value::as_str).unwrap_or("");
            let kind = f.get("kind").and_then(Value::as_str).unwrap_or("");
            let name = basename(path);
            if kind != "doc" && !doc_basename_prefix_re().is_match(&name) { continue; }
            let full = Path::new(root_s).join(path);
            let content = match std::fs::read_to_string(&full) { Ok(c) => c, Err(_) => continue };
            let first_hash_line = content.split('\n').find(|l| l.starts_with("# "));
            let raw_title = first_hash_line.map(|s| s.to_string()).unwrap_or_else(|| name.clone());
            let stripped = leading_hash_re().replace(&raw_title, "").into_owned();
            let title: String = stripped.chars().take(120).collect();
            let snippet: String = content.chars().take(800).collect();
            docs.push(json!({ "path": path, "title": title, "snippet": snippet }));
            if docs.len() >= 10 { break; }
        }
    }
    Value::Array(docs)
}

// Insertion-order-preserving `Map<string, number>` bump/entries (code-authors.rs precedent) —
// mirrors JS `Map` iteration order, which Rust's `HashMap` does not preserve.
#[derive(Default)]
struct OrderedCounts { order: Vec<String>, counts: HashMap<String, i64> }
impl OrderedCounts {
    fn bump(&mut self, key: &str) {
        match self.counts.get_mut(key) {
            Some(c) => { *c += 1; }
            None => { self.counts.insert(key.to_string(), 1); self.order.push(key.to_string()); }
        }
    }
    fn entries(&self) -> Vec<(String, i64)> { self.order.iter().map(|k| (k.clone(), self.counts[k])).collect() }
}
fn top_n(oc: &OrderedCounts, n: usize) -> Vec<(String, i64)> {
    let mut v = oc.entries();
    v.sort_by(|a, b| b.1.cmp(&a.1)); // stable desc, ties keep insertion order (JS Array.sort stability)
    v.truncate(n);
    v
}

/// `git log --name-only --pretty=format:%H|%an|%s` STDOUT parser. A header line
/// (`sha|author|subject`, `rest.join('|')` recombines any `|` inside the subject) starts a commit
/// only when no commit is currently open (`!cur`); a blank line closes the open commit; any other
/// line while a commit IS open is a changed-file path. `cur_open` stands in for JS's `cur` object
/// (only `commits.length` is ever read back from the finished array — the per-commit `sha`/
/// `author`/`subject`/`files` fields are populated but never read again — so a bool is sufficient).
fn parse_git_log(stdout: &str) -> (i64, OrderedCounts, OrderedCounts, OrderedCounts) {
    let mut commits_count: i64 = 0;
    let mut file_counts = OrderedCounts::default();
    let mut authors = OrderedCounts::default();
    let mut subject_tokens = OrderedCounts::default();
    let mut cur_open = false;
    for line in stdout.split('\n') {
        if line.trim().is_empty() {
            if cur_open { commits_count += 1; cur_open = false; }
            continue;
        }
        if line.contains('|') && !cur_open {
            let parts: Vec<&str> = line.split('|').collect();
            let author = parts.get(1).copied().unwrap_or("").to_string();
            let subject = if parts.len() > 2 { parts[2..].join("|") } else { String::new() };
            authors.bump(&author);
            for w in subject.to_lowercase().split_whitespace() {
                if w.chars().count() > 4 { subject_tokens.bump(w); }
            }
            cur_open = true;
        } else if cur_open {
            file_counts.bump(line);
        }
    }
    if cur_open { commits_count += 1; }
    (commits_count, file_counts, authors, subject_tokens)
}

/// `export function scanGitHistory(root, limit = 200)` — BESPOKE-NATIVE, see module doc: always a
/// real `git` subprocess, degrading to `{available:false[, reason]}` on any nonzero/unspawnable
/// exit (`reason` key OMITTED, not `null`, when the process never spawned — JS `log.stderr?.slice`
/// on a null stderr is `undefined`, which JSON.stringify drops per gotcha 3).
pub fn scanGitHistory(root: &Value, limit: &Value) -> Value {
    let root_s = root.as_str().unwrap_or_default();
    let n: i64 = match limit { Value::Number(v) => v.as_i64().unwrap_or(200), _ => 200 };
    let output = crate::proc::cmd("git")
        .arg("log").arg("-n").arg(n.to_string())
        .arg("--name-only").arg("--pretty=format:%H|%an|%s")
        .current_dir(root_s)
        .output();
    let (ok, stdout, stderr): (bool, String, Option<String>) = match output {
        Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned(), Some(String::from_utf8_lossy(&o.stderr).into_owned())),
        Err(_) => (false, String::new(), None),
    };
    if !ok {
        return match stderr {
            Some(s) => json!({ "available": false, "reason": s.chars().take(100).collect::<String>() }),
            None => json!({ "available": false }),
        };
    }
    let (commits_count, file_counts, authors, subject_tokens) = parse_git_log(&stdout);
    let hot_files: Vec<Value> = top_n(&file_counts, 15).into_iter().map(|(f, c)| json!({ "file": f, "change_count": c })).collect();
    let top_authors: Vec<Value> = top_n(&authors, 10).into_iter().map(|(n, c)| json!({ "name": n, "commits": c })).collect();
    let themes: Vec<Value> = top_n(&subject_tokens, 10).into_iter().map(|(w, c)| json!({ "word": w, "count": c })).collect();
    json!({
        "available": true,
        "commits_analyzed": commits_count,
        "hot_files": hot_files,
        "top_authors": top_authors,
        "themes": themes,
    })
}

/// `node:path` `resolve(repo_path)` — SIMPLIFIED (see module doc): absolute paths pass through
/// verbatim, relative paths join onto the process cwd. No `..`/`.` segment normalization.
fn resolve_path(p: &str) -> String {
    let path = Path::new(p);
    if path.is_absolute() {
        p.to_string()
    } else {
        std::env::current_dir().map(|cwd| cwd.join(path).to_string_lossy().into_owned()).unwrap_or_else(|_| p.to_string())
    }
}

/// `export default function onboardingScan(input, _context)` — BESPOKE-NATIVE, see module doc
/// (unconditionally calls `scanGitHistory`). Guard-throws (`repo_path` missing / does not exist)
/// are ported for real but — per the "guard-throws are not exercised" convention every sibling
/// twin uses — not covered by a dedicated case beyond the native tests below.
pub fn onboardingScan(input: &Value, db: &Connection) -> Result<Value, String> {
    let _ = db;
    let repo_path_v = nz(input.get("repo_path"));
    if !truthy(repo_path_v) {
        return Err("onboarding-scan: repo_path is required".to_string());
    }
    let repo_path = repo_path_v.and_then(Value::as_str).unwrap_or_default();
    let root = resolve_path(repo_path);
    if !Path::new(&root).exists() {
        return Err(format!("onboarding-scan: repo_path '{repo_path}' (does not exist) is required"));
    }

    let root_v = json!(root.clone());
    let files_v = walk(&root_v, &Value::Null);
    let files_arr = files_v.as_array().cloned().unwrap_or_default();

    let mut by_kind: Map<String, Value> = Map::new();
    for f in &files_arr {
        let kind = f.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
        let next = by_kind.get(&kind).and_then(Value::as_i64).unwrap_or(0) + 1;
        by_kind.insert(kind, json!(next));
    }

    let stack = detectStack(&root_v, &files_v);
    let git = scanGitHistory(&root_v, &Value::Null);
    let docs = harvestDocs(&root_v, &files_v);
    let sample_files: Vec<Value> = files_arr.iter().take(50).cloned().collect();

    Ok(json!({
        "repo_path": root,
        "file_count": files_arr.len() as i64,
        "by_kind": Value::Object(by_kind),
        "stack": stack,
        "git_history": git,
        "docs": docs,
        "sample_files": sample_files,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-onboarding-scan-rs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn git_ok(args: &[&str], cwd: &Path) {
        let out = crate::proc::cmd("git").args(args).current_dir(cwd).output().unwrap();
        assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }
    fn commit(dir: &Path, files: &[(&str, &str)], subject: &str) {
        for (rel, content) in files {
            let abs = dir.join(rel);
            if let Some(p) = abs.parent() { fs::create_dir_all(p).unwrap(); }
            fs::write(&abs, content).unwrap();
        }
        git_ok(&["add", "-A"], dir);
        git_ok(&["-c", "user.name=antiloki", "-c", "user.email=antiloki@local", "-c", "commit.gpgsign=false", "commit", "-q", "-m", subject], dir);
    }

    #[test]
    fn walk_classifies_and_skips_ignored_dot_entries() {
        let dir = temp_dir("walk");
        fs::write(dir.join("README.md"), "# Root\n").unwrap();
        fs::write(dir.join("package.json"), "{}\n").unwrap();
        fs::write(dir.join(".env"), "SECRET=1\n").unwrap();
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/index.mjs"), "export const x = 1;\n").unwrap();
        fs::write(dir.join("src/util.test.mjs"), "test();\n").unwrap();
        fs::create_dir_all(dir.join("docs")).unwrap();
        fs::write(dir.join("docs/guide.md"), "# Guide\n").unwrap();
        fs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        fs::write(dir.join("node_modules/pkg/index.js"), "module.exports={};\n").unwrap();

        let root = dir.to_string_lossy().into_owned();
        let r = walk(&json!(root), &Value::Null);
        let arr = r.as_array().unwrap();
        let paths: Vec<&str> = arr.iter().map(|f| f["path"].as_str().unwrap()).collect();
        assert!(!paths.iter().any(|p| p.starts_with("node_modules")), "{paths:?}");
        assert!(!paths.contains(&".env"), "{paths:?}");

        let by_path = |p: &str| arr.iter().find(|f| f["path"] == json!(p)).unwrap();
        assert_eq!(by_path("README.md")["kind"], json!("doc"));
        assert_eq!(by_path("package.json")["kind"], json!("config"));
        assert_eq!(by_path("src/index.mjs")["kind"], json!("source"));
        assert_eq!(by_path("src/index.mjs")["ext"], json!(".mjs"));
        assert_eq!(by_path("src/util.test.mjs")["kind"], json!("test"));
        assert_eq!(by_path("docs/guide.md")["kind"], json!("doc"));
        assert_eq!(paths.len(), 5); // README, package.json, index.mjs, util.test.mjs, guide.md

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn walk_caps_at_max_files() {
        let dir = temp_dir("walk-cap");
        for i in 0..5 { fs::write(dir.join(format!("f{i}.txt")), "x").unwrap(); }
        let root = dir.to_string_lossy().into_owned();
        let r = walk(&json!(root), &json!(2));
        assert_eq!(r.as_array().unwrap().len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extname_matches_node_semantics() {
        assert_eq!(extname("a.b.c"), ".c");
        assert_eq!(extname("a."), ".");
        assert_eq!(extname("a"), "");
        assert_eq!(extname(".gitignore"), "");
        assert_eq!(extname(".env.local"), ".local");
        assert_eq!(extname("README.md"), ".md");
    }

    #[test]
    fn detect_stack_dedups_languages_keeps_markers_and_omits_missing_ecosystem_key() {
        let files = json!([
            { "path": "package.json" }, { "path": "backend/requirements.txt" },
            { "path": "backend/Pipfile" }, { "path": "frontend/tsconfig.json" },
            { "path": "notes.txt" },
        ]);
        let r = detectStack(&Value::Null, &files);
        assert_eq!(r["languages"], json!(["javascript", "python", "typescript"]));
        assert_eq!(r["ecosystems"], json!(["npm", "pip", "pipenv"]));
        let markers = r["markers"].as_array().unwrap();
        assert_eq!(markers.len(), 4);
        assert_eq!(markers[3], json!({ "path": "frontend/tsconfig.json", "lang": "typescript" }));
        assert!(markers[3].as_object().unwrap().get("ecosystem").is_none());
    }

    #[test]
    fn harvest_docs_title_fallback_basename_prefix_and_missing_file_skip() {
        let dir = temp_dir("docs");
        fs::write(dir.join("README.md"), "# Project Title\n\nintro\n").unwrap();
        fs::create_dir_all(dir.join("docs")).unwrap();
        fs::write(dir.join("docs/notes.md"), "no heading here\nmore text\n").unwrap();
        fs::write(dir.join("CONTRIBUTING-GUIDE.txt"), "# How to contribute\n\ndetails\n").unwrap();
        let root = dir.to_string_lossy().into_owned();

        let files = json!([
            { "path": "README.md", "kind": "doc" },
            { "path": "docs/notes.md", "kind": "doc" },
            { "path": "CONTRIBUTING-GUIDE.txt", "kind": "source" },
            { "path": "missing.md", "kind": "doc" },
            { "path": "src/code.js", "kind": "source" },
        ]);
        let r = harvestDocs(&json!(root), &files);
        let docs = r.as_array().unwrap();
        assert_eq!(docs.len(), 3);
        assert_eq!(docs[0]["title"], json!("Project Title"));
        assert_eq!(docs[1]["title"], json!("notes.md")); // no "# " line ⇒ basename fallback
        assert_eq!(docs[2]["title"], json!("How to contribute"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_git_history_of_non_git_dir_degrades_without_reason_key_when_unspawnable_is_not_the_case() {
        let dir = temp_dir("scan-non-git");
        let root = dir.to_string_lossy().into_owned();
        let r = scanGitHistory(&json!(root), &Value::Null);
        assert_eq!(r["available"], json!(false));
        assert!(r.get("reason").is_some()); // real git binary ran + failed with a real stderr message
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_git_history_of_real_repo_computes_hot_files_top_authors_and_themes() {
        let dir = temp_dir("scan-real");
        git_ok(&["init", "-q"], &dir);
        commit(&dir, &[("a.txt", "1"), ("shared.txt", "1")], "implement onboarding scanner");
        commit(&dir, &[("shared.txt", "2")], "refactor onboarding scanner internals");
        let root = dir.to_string_lossy().into_owned();

        let r = scanGitHistory(&json!(root), &Value::Null);
        assert_eq!(r["available"], json!(true));
        assert_eq!(r["commits_analyzed"], json!(2));
        let hot = r["hot_files"].as_array().unwrap();
        assert_eq!(hot[0], json!({ "file": "shared.txt", "change_count": 2 }));
        let authors = r["top_authors"].as_array().unwrap();
        assert_eq!(authors[0]["name"], json!("antiloki"));
        assert_eq!(authors[0]["commits"], json!(2));
        let themes = r["themes"].as_array().unwrap();
        assert!(themes.iter().any(|t| t["word"] == json!("onboarding")));
        assert!(themes.iter().find(|t| t["word"] == json!("onboarding")).unwrap()["count"] == json!(2));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn onboarding_scan_integrates_walk_stack_git_and_docs_on_a_real_repo() {
        let dir = temp_dir("full");
        git_ok(&["init", "-q"], &dir);
        commit(&dir, &[
            ("package.json", "{}\n"),
            ("README.md", "# Demo Repo\n\nan intro\n"),
            ("src/index.mjs", "export const x = 1;\n"),
        ], "initial import");

        let root = dir.to_string_lossy().into_owned();
        let r = onboardingScan(&json!({ "repo_path": root }), &Connection::open_in_memory().unwrap()).unwrap();
        assert_eq!(r["repo_path"], json!(root));
        assert_eq!(r["file_count"], json!(3));
        assert_eq!(r["by_kind"]["config"], json!(1));
        assert_eq!(r["by_kind"]["doc"], json!(1));
        assert_eq!(r["by_kind"]["source"], json!(1));
        assert_eq!(r["stack"]["languages"], json!(["javascript"]));
        assert_eq!(r["git_history"]["available"], json!(true));
        assert_eq!(r["git_history"]["commits_analyzed"], json!(1));
        assert_eq!(r["docs"].as_array().unwrap().len(), 1);
        assert_eq!(r["docs"][0]["title"], json!("Demo Repo"));
        assert_eq!(r["sample_files"].as_array().unwrap().len(), 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn onboarding_scan_guard_throws() {
        let c = Connection::open_in_memory().unwrap();
        assert_eq!(onboardingScan(&json!({}), &c).unwrap_err(), "onboarding-scan: repo_path is required");
        let err = onboardingScan(&json!({ "repo_path": "C:/nope/does/not/exist-xyz" }), &c).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
    }
}
