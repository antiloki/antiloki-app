//! dev-bundle-slice.rs — twin of skills/agents/dev/dev-bundle-slice.mjs (db lane). NAMES ARE
//! CONTRACT: `devBundleSlice` (default).
//!
//! Extracts a small, deterministic Markdown context bundle for DEV's implement-code step: per
//! `files_touched` entry — exports (first 12 top-level `export ...` declarations) + either the
//! WHOLE file (≤ `WHOLE_FILE_THRESHOLD` bytes) or a keyword-matched section (brace-walked from a
//! declaration whose name matches a derived keyword, capped at 3500 bytes / 100 lines) — plus up
//! to 2 similarity-scored `memory.skills` reference snippets and the microtask/contract acceptance
//! criteria, joined into one string capped at `TARGET_BUNDLE_BYTES`.
//!
//! Real fs I/O (`existsSync`/`statSync`/`readFileSync` over `files_touched`), not simulated — but
//! BOTH candidate roots are fixture-controllable: the primary root is DB-derived
//! (`workspaces.root_path` via the already-converted `engine_workspace_root::getWorkspaceRoot`
//! twin, reused not reimplemented per the playbook's cross-module rule), and the optional override
//! root (`input.sandbox_root`) is a plain caller-supplied argument. Both point at real on-disk
//! testdata dirs committed alongside this unit's fixture
//! (`antiloki-crossing/fixtures/testdata/dev-bundle-slice-{workspace,sandbox}/`), same technique as
//! `codebase_stack_facts__stack_facts` / `agents_in_skill_library_materialize`.
//!
//! `path.resolve(root, rel)` itself is NOT ported byte-for-byte (no Node win32-resolve STRING ever
//! reaches the return value — `bundleForFile`'s `path` field is the untouched `relPath`; only the
//! bytes found ON DISK at the resolved location are observable) — `Path::new(root).join(rel)`
//! locates the identical file (Rust's `Path::join` already replaces wholesale when `rel` is
//! absolute, matching `path.resolve`'s two-argument behavior for every input this skill's fixtures
//! or real callers exercise: an absolute `root`/`sandbox_root` and a relative `relPath`).
//!
//! `context.db` missing → JS throws `err.contextMissing('dev-bundle-slice', 'db')`. Not modeled:
//! Rust's `db: &Connection` is a required, non-nullable reference, so this guard is unreachable
//! here — same convention used by every sibling twin. `contract` missing/falsy → JS throws
//! `err.required('dev-bundle-slice', 'contract')` (`"dev-bundle-slice: contract is required"`);
//! modeled as `Err(String)` with the matching message, a guard-throw not exercised by the fixture
//! (playbook gotcha 9: fixture only the success path).
//!
//! References (`memory.skills`, cross-db ATTACH lane, fixture `"attach": ["memory"]`): the whole
//! scoring block sits inside a JS `try { … } catch { /* no references is fine */ }` — modeled by
//! matching `query_rows`'s `Result` and collapsing `Err` (e.g. "no such table: memory.skills" when
//! the db doesn't have "memory" attached at all) to an empty `references` Vec, exactly like the JS
//! catch. That fallback path is proven by this file's `#[cfg(test)]` rather than the conform
//! fixture, since the fixture's top-level `attach` list is shared by every case and this unit's
//! happy path needs `memory.skills` seeded to prove the scoring/sort/top-2 logic.
//!
//! Two JS lines are NOT ported — both are dead code with zero observable effect, confirmed by
//! reading every downstream use: `const catalogFindMod = db.prepare('SELECT 1').get();` (a stale
//! "touch db" leftover — its own comment describes a lazy import that was never added) and
//! `const queryText = (contract.title ?? '') + ' ' + (microtask?.intent ?? '');` (computed, then
//! never referenced again — the actual `refRows` SQL below it does not use `queryText`).
// QUALITY: 80/100 — export-line/keyword-section regex + brace-walk + Set-ordered keyword
// derivation ported 1:1; real fs read (DB-derived root + input sandbox override) proven via
// committed testdata; cross-db memory.skills scoring exercised with a seeded fixture case.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::Path;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn tmpl(v: Option<&Value>) -> String { match v { None=>"undefined".into(), Some(Value::Null)=>"null".into(), Some(x)=>str_qq(Some(x)) } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

// = JS `.length` (UTF-16 code units) — used for every `.length`/`.slice(0,n)` byte-budget
// comparison below; identical to a plain byte/char count for the ASCII fixture content this unit
// reads, exact for BMP text generally.
fn js_len(s: &str) -> usize { s.encode_utf16().count() }
fn js_slice(s: &str, n: usize) -> String { s.chars().take(n).collect() }

const WHOLE_FILE_THRESHOLD: usize = 9000;
const TARGET_BUNDLE_BYTES: usize = 20000;
const MAX_FILE_SECTION_BYTES: usize = 3500;
const MAX_REFERENCE_BYTES: usize = 600;

/// `extractExportLines(source)` — first 12 top-level `export ...` declaration lines, each clipped
/// to 100 (UTF-16) chars.
fn extract_export_lines(source: &str) -> Vec<String> {
    let re = Regex::new(r"^\s*(export\s+(?:default\s+)?(?:async\s+)?(?:function|const|let|class)\s+[\w]+[^=]*?)(?:\s*=|\s*\{|\s*\(|\s*;|$)").unwrap();
    let mut out: Vec<String> = Vec::new();
    for line in source.split('\n') {
        if let Some(caps) = re.captures(line) {
            let m = caps.get(1).unwrap().as_str().trim();
            out.push(js_slice(m, 100));
            if out.len() >= 12 { break; }
        }
    }
    out
}

// Accumulate whole lines until the byte budget; never split a line (mirrors JS clipToWholeLines).
fn clip_to_whole_lines(section_lines: &[&str], max_bytes: usize) -> String {
    let mut acc = String::new();
    let mut clipped = false;
    for line in section_lines {
        let next = if acc.is_empty() { (*line).to_string() } else { format!("{acc}\n{line}") };
        if js_len(&next) > max_bytes && !acc.is_empty() { clipped = true; break; }
        acc = next;
    }
    if clipped { format!("{acc}\n/* ... section truncated ... */") } else { acc }
}

/// `extractMatchingSection(source, keywords, maxBytes)` — first function/const/let/class
/// declaration whose name (lowercased) is one of `keywords`, brace-walked to its closing brace
/// (hard cap 100 lines from the declaration), then byte-clipped. `None` if no keyword given or no
/// declaration matches.
fn extract_matching_section(source: &str, keywords: &[String], max_bytes: usize) -> Option<String> {
    if keywords.is_empty() { return None; }
    let kw_set: HashSet<&str> = keywords.iter().map(|k| k.as_str()).collect();
    let lines: Vec<&str> = source.split('\n').collect();
    let decl_re = Regex::new(r"(?:function|const|let|class)\s+([\w]+)").unwrap();
    for i in 0..lines.len() {
        let caps = match decl_re.captures(lines[i]) { Some(c) => c, None => continue };
        let name = caps.get(1).unwrap().as_str().to_lowercase();
        if !kw_set.contains(name.as_str()) { continue; }
        let mut depth: i64 = 0;
        let mut started = false;
        let mut end_line = i;
        for j in i..lines.len() {
            for ch in lines[j].chars() {
                if ch == '{' { depth += 1; started = true; }
                else if ch == '}' { depth -= 1; }
            }
            if started && depth == 0 { end_line = j; break; }
            if (j as i64 - i as i64) > 100 { end_line = j; break; }
        }
        let section_lines = &lines[i..=end_line];
        return Some(clip_to_whole_lines(section_lines, max_bytes));
    }
    None
}

// `microtask?.<k> ?? ''` / `contract?.<k> ?? ''` folded into the haystack — a present non-null,
// non-string value would still be `String()`-coerced by JS's `Array.prototype.join`, so `js_string`
// (not a plain `.as_str()`) is the faithful port.
fn js_string(v: &Value) -> String { match v { Value::Null=>"null".into(), Value::String(s)=>s.clone(), Value::Number(n)=>n.to_string(), Value::Bool(b)=>b.to_string(), Value::Array(a)=>a.iter().map(js_string).collect::<Vec<_>>().join(","), Value::Object(_)=>"[object Object]".into() } }
fn field_str(v: &Value, k: &str) -> String { match v.get(k) { Some(x) if !x.is_null() => js_string(x), _ => String::new() } }

/// `deriveKeywords(microtask, contract)` — camelCase identifiers (≥4 chars) then kebab-case runs
/// (converted to camelCase) scanned out of the joined haystack, deduped in first-seen order (JS
/// `Set` iteration order = insertion order), capped to 20 total.
fn derive_keywords(microtask: &Value, contract: &Value) -> Vec<String> {
    let haystack = format!(
        "{} {} {} {}",
        field_str(microtask, "intent"),
        field_str(microtask, "description"),
        field_str(contract, "title"),
        field_str(contract, "briefing_md"),
    );
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();

    let camel_re = Regex::new(r"\b([a-z][a-zA-Z0-9]{3,})\b").unwrap();
    for caps in camel_re.captures_iter(&haystack) {
        let tok = caps.get(1).unwrap().as_str();
        if tok.chars().count() <= 30 {
            let lower = tok.to_lowercase();
            if seen.insert(lower.clone()) { out.push(lower); }
        }
    }
    let kebab_re = Regex::new(r"\b([a-z]+(?:-[a-z]+){1,4})\b").unwrap();
    for caps in kebab_re.captures_iter(&haystack) {
        let tok = caps.get(1).unwrap().as_str();
        let camel: String = tok.split('-').enumerate().map(|(i, seg)| {
            if i == 0 { seg.to_string() } else {
                let mut c = seg.chars();
                match c.next() { Some(f) => f.to_uppercase().collect::<String>() + c.as_str(), None => String::new() }
            }
        }).collect();
        if camel.chars().count() <= 30 {
            let lower = camel.to_lowercase();
            if seen.insert(lower.clone()) { out.push(lower); }
        }
    }
    out.truncate(20);
    out
}

struct FileBundle {
    path: Value,
    size: i64,
    exports: Option<Vec<String>>,
    target_section: Option<String>,
    whole_file: bool,
    from_sandbox: bool,
}

/// `bundleForFile(absPath, relPath, keywords)` — `Ok(None)` when the file doesn't exist (skip,
/// mirrors JS `existsSync` guard); `Err` mirrors an uncaught `statSync`/`readFileSync` throw (not
/// exercised — every fixture path exists and is readable).
fn bundle_for_file(abs_path: &Path, rel_path_v: &Value, keywords: &[String], from_sandbox: bool) -> Result<Option<FileBundle>, String> {
    if !abs_path.exists() { return Ok(None); }
    let meta = std::fs::metadata(abs_path).map_err(|e| e.to_string())?;
    let size = meta.len() as i64;
    if size > 200_000 {
        return Ok(Some(FileBundle { path: rel_path_v.clone(), size, exports: None, target_section: None, whole_file: false, from_sandbox }));
    }
    let source = std::fs::read_to_string(abs_path).map_err(|e| e.to_string())?;
    let exports = extract_export_lines(&source);
    // JS `if (fb.target_section)` / `!!fb.target_section` is a TRUTHY check — an empty string
    // (a zero-byte file shipped whole) is falsy in JS just like `undefined`, so it must collapse
    // to `None` here too (an `extractMatchingSection` match is never empty: it always starts at
    // the matched declaration line, which contains the match itself).
    let (target_section, whole_file) = if js_len(&source) <= WHOLE_FILE_THRESHOLD {
        (if source.is_empty() { None } else { Some(source) }, true)
    } else {
        (extract_matching_section(&source, keywords, MAX_FILE_SECTION_BYTES), false)
    };
    Ok(Some(FileBundle { path: rel_path_v.clone(), size, exports: Some(exports), target_section, whole_file, from_sandbox }))
}

struct Reference { name: String, snippet: String }

pub fn devBundleSlice(input: &Value, db: &Connection) -> Result<Value, String> {
    let contract = match input.get("contract").filter(|v| truthy(Some(v))) {
        Some(c) => c.clone(),
        None => return Err("dev-bundle-slice: contract is required".to_string()),
    };
    let microtask = input.get("microtask").cloned().unwrap_or(Value::Null);
    let sandbox_root = input.get("sandbox_root").cloned().unwrap_or(Value::Null);

    // filesToSlice = (microtask?.files_touched?.length > 0) ? microtask.files_touched : (contract.files_touched ?? [])
    let microtask_files = microtask.get("files_touched").and_then(Value::as_array);
    let files_to_slice: Vec<Value> = match microtask_files {
        Some(arr) if !arr.is_empty() => arr.clone(),
        _ => contract.get("files_touched").and_then(Value::as_array).cloned().unwrap_or_default(),
    };

    let keywords = derive_keywords(&microtask, &contract);

    let workspace_root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let workspace_root = workspace_root_v.as_str().unwrap_or_default();
    let sandbox_root_str = sandbox_root.as_str().filter(|s| !s.is_empty());

    let mut file_bundles: Vec<FileBundle> = Vec::new();
    for rel_path_v in &files_to_slice {
        let rel_path_str = rel_path_v.as_str().unwrap_or_default();
        let mut abs = Path::new(workspace_root).join(rel_path_str);
        let mut from_sandbox = false;
        if let Some(sroot) = sandbox_root_str {
            let sandbox_abs = Path::new(sroot).join(rel_path_str);
            if sandbox_abs.exists() { abs = sandbox_abs; from_sandbox = true; }
        }
        if let Some(fb) = bundle_for_file(&abs, rel_path_v, &keywords, from_sandbox)? {
            file_bundles.push(fb);
        }
    }

    // References — top 2 `memory.skills` rows by keyword-overlap score, wrapped exactly like the
    // JS try/catch: a query error (e.g. "memory" not attached) collapses to an empty Vec.
    let mut references: Vec<Reference> = Vec::new();
    if let Ok(rows_v) = crate::db_json::query_rows(
        db,
        "SELECT name, description FROM memory.skills WHERE description IS NOT NULL AND description != '' ORDER BY id DESC LIMIT 30",
        &[],
    ) {
        let rows = rows_v.as_array().cloned().unwrap_or_default();
        let mut scored: Vec<(Value, i64)> = rows.into_iter().map(|r| {
            let name_s = r.get("name").and_then(Value::as_str).unwrap_or("");
            let desc_s = r.get("description").and_then(Value::as_str).unwrap_or("");
            let text = format!("{name_s} {desc_s}").to_lowercase();
            let score = keywords.iter().filter(|kw| text.contains(kw.as_str())).count() as i64;
            (r, score)
        }).collect();
        scored.retain(|(_, score)| *score > 0);
        scored.sort_by(|a, b| b.1.cmp(&a.1)); // stable — ties keep the ORDER BY id DESC array order
        scored.truncate(2);
        references = scored.into_iter().map(|(r, _)| {
            let name = r.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let desc = r.get("description").and_then(Value::as_str).unwrap_or("");
            Reference { name, snippet: js_slice(desc, MAX_REFERENCE_BYTES) }
        }).collect();
    }

    // Acceptance — microtask.acceptance ?? contract.acceptance ?? contract.acceptance_json ?? [].
    // A string value is JSON.parse'd (malformed JSON keeps the raw string); the final text is the
    // string as-is, or JSON.stringify(v, null, 2) for anything else.
    let acceptance_raw: Value = nz(microtask.get("acceptance"))
        .or_else(|| nz(contract.get("acceptance")))
        .or_else(|| nz(contract.get("acceptance_json")))
        .cloned()
        .unwrap_or_else(|| json!([]));
    let acceptance_val: Value = match acceptance_raw.as_str() {
        Some(s) => serde_json::from_str::<Value>(s).unwrap_or_else(|_| json!(s)),
        None => acceptance_raw.clone(),
    };
    let acceptance_text: String = match acceptance_val.as_str() {
        Some(s) => s.to_string(),
        None => serde_json::to_string_pretty(&acceptance_val).unwrap_or_default(),
    };

    let mut lines: Vec<String> = Vec::new();
    lines.push("## Files in scope".to_string());
    for fb in &file_bundles {
        lines.push(format!("\n### {} ({} bytes)", tmpl(Some(&fb.path)), fb.size));
        if let Some(exports) = &fb.exports {
            if !exports.is_empty() {
                lines.push("Exports:".to_string());
                for e in exports { lines.push(format!("  - {e}")); }
            }
        }
        if let Some(section) = &fb.target_section {
            lines.push(if fb.whole_file {
                format!("Full file content (file \u{2264} {WHOLE_FILE_THRESHOLD} bytes \u{2014} shipped in full):")
            } else {
                "Relevant section (matched by microtask keywords on a larger file):".to_string()
            });
            lines.push("```".to_string());
            lines.push(section.clone());
            lines.push("```".to_string());
            if !fb.whole_file {
                lines.push("NOTE: this is a partial view. Copy anchor_before/anchor_after VERBATIM from the lines shown above; do not anchor on content past a \"... section truncated ...\" marker or on a file region not shown here.".to_string());
            }
        } else {
            lines.push("(No section matched \u{2014} AI must work from exports + file path alone.)".to_string());
        }
    }
    if !references.is_empty() {
        lines.push("\n## Reference patterns".to_string());
        for r in &references { lines.push(format!("- **{}**: {}", r.name, r.snippet)); }
    }
    lines.push("\n## Acceptance criteria".to_string());
    lines.push(acceptance_text);

    let mut bundle_md = lines.join("\n");
    if js_len(&bundle_md) > TARGET_BUNDLE_BYTES {
        let truncated = js_slice(&bundle_md, TARGET_BUNDLE_BYTES);
        bundle_md = format!("{truncated}\n\n/* ... bundle truncated to fit {TARGET_BUNDLE_BYTES} bytes ... */");
    }

    let files_out: Vec<Value> = file_bundles.iter().map(|fb| json!({
        "path": fb.path,
        "has_section": fb.target_section.is_some(),
        "exports_count": fb.exports.as_ref().map(|e| e.len()).unwrap_or(0) as i64,
        "from_sandbox": fb.from_sandbox,
    })).collect();
    let keywords_used: Vec<String> = keywords.iter().take(10).cloned().collect();

    let total_bytes = js_len(&bundle_md) as i64;
    let mut out = Map::new();
    out.insert("bundle_md".into(), json!(bundle_md));
    out.insert("total_bytes".into(), json!(total_bytes));
    out.insert("files".into(), json!(files_out));
    out.insert("references_count".into(), json!(references.len() as i64));
    out.insert("keywords_used".into(), json!(keywords_used));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WS: &str = concat!(env!("ANTILOKI_FIXTURES"), "/dev-bundle-slice-workspace");
    const SB: &str = concat!(env!("ANTILOKI_FIXTURES"), "/dev-bundle-slice-sandbox");

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')",
        ).unwrap();
        c
    }
    fn seed_active(c: &Connection, root_path: &str) {
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active, created_at) VALUES ('x', ?1, 'git', 1, '2026-01-01 00:00:00')",
            rusqlite::params![root_path],
        ).unwrap();
    }
    fn attach_memory(c: &Connection) {
        c.execute_batch("ATTACH ':memory:' AS memory").unwrap();
        c.execute_batch("CREATE TABLE memory.skills (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, description TEXT)").unwrap();
    }

    #[test]
    fn contract_missing_throws() {
        let c = db();
        seed_active(&c, WS);
        let e = devBundleSlice(&json!({}), &c).unwrap_err();
        assert_eq!(e, "dev-bundle-slice: contract is required");
    }

    #[test]
    fn small_whole_file_ships_full_with_exports_and_malformed_acceptance_string_passes_through() {
        let c = db();
        seed_active(&c, WS);
        attach_memory(&c);
        let r = devBundleSlice(&json!({
            "contract": { "title": "improve buildPlan", "files_touched": ["small.mjs"] },
            "microtask": { "intent": "work on buildPlan edge cases", "files_touched": ["small.mjs"], "acceptance": "file exists and exports buildPlan(input)" }
        }), &c).unwrap();
        assert_eq!(r["files"][0]["has_section"], json!(true));
        assert_eq!(r["files"][0]["exports_count"], json!(3));
        assert_eq!(r["files"][0]["from_sandbox"], json!(false));
        assert_eq!(r["references_count"], json!(0));
        assert!(r["bundle_md"].as_str().unwrap().contains("Full file content"));
        assert!(r["bundle_md"].as_str().unwrap().ends_with("file exists and exports buildPlan(input)"));
    }

    #[test]
    fn big_file_keyword_match_extracts_named_section_and_caps_keywords_used_to_ten() {
        let c = db();
        seed_active(&c, WS);
        attach_memory(&c);
        let r = devBundleSlice(&json!({
            "contract": { "title": "x", "files_touched": ["bigmatch.mjs"] },
            "microtask": { "intent": "work on keywordMatchFn alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar", "files_touched": ["bigmatch.mjs"] }
        }), &c).unwrap();
        assert_eq!(r["files"][0]["has_section"], json!(true));
        assert_eq!(r["files"][0]["exports_count"], json!(12));
        let md = r["bundle_md"].as_str().unwrap();
        assert!(md.contains("Relevant section"));
        assert!(md.contains("export function keywordMatchFn(y) {\n  // target section body\n  const z = y * 2;\n  return z;\n}"));
        assert!(!md.contains("fillerAlpha(x)"), "unrelated filler bodies must not be included");
        assert_eq!(r["keywords_used"].as_array().unwrap().len(), 10);
        assert_eq!(r["keywords_used"][0], json!("work"));
        assert_eq!(r["keywords_used"][1], json!("keywordmatchfn"));
    }

    #[test]
    fn big_file_no_keyword_match_reports_no_section_and_parses_acceptance_json_string() {
        let c = db();
        seed_active(&c, WS);
        attach_memory(&c);
        let r = devBundleSlice(&json!({
            "contract": { "title": "investigate unrelated payment gateway timeout retries", "files_touched": ["bignomatch.mjs"], "acceptance_json": "[{\"count\":3,\"label\":\"retry\"}]" },
            "microtask": { "intent": "debug outbound network queue backlog", "files_touched": ["bignomatch.mjs"] }
        }), &c).unwrap();
        assert_eq!(r["files"][0]["has_section"], json!(false));
        assert_eq!(r["files"][0]["exports_count"], json!(12));
        let md = r["bundle_md"].as_str().unwrap();
        assert!(md.contains("No section matched"));
        assert!(md.ends_with("[\n  {\n    \"count\": 3,\n    \"label\": \"retry\"\n  }\n]"), "integer renders without a decimal point, key order preserved");
    }

    #[test]
    fn missing_file_is_skipped_with_no_throw() {
        let c = db();
        seed_active(&c, WS);
        attach_memory(&c);
        let r = devBundleSlice(&json!({
            "contract": { "title": "t", "files_touched": ["does-not-exist.mjs"] },
            "microtask": null
        }), &c).unwrap();
        assert_eq!(r["files"].as_array().unwrap().len(), 0);
        assert_eq!(r["bundle_md"], json!("## Files in scope\n\n## Acceptance criteria\n[]"));
    }

    #[test]
    fn too_large_file_reports_size_with_no_exports_or_section() {
        let c = db();
        seed_active(&c, WS);
        attach_memory(&c);
        let r = devBundleSlice(&json!({
            "contract": { "title": "t", "files_touched": ["too-large.bin"] },
            "microtask": { "intent": "x", "files_touched": ["too-large.bin"] }
        }), &c).unwrap();
        assert_eq!(r["files"][0]["has_section"], json!(false));
        assert_eq!(r["files"][0]["exports_count"], json!(0));
        assert!(r["bundle_md"].as_str().unwrap().contains("(200005 bytes)"));
    }

    #[test]
    fn sandbox_copy_wins_when_present_else_falls_back_to_workspace_copy() {
        let c = db();
        seed_active(&c, WS);
        attach_memory(&c);
        let over = devBundleSlice(&json!({
            "contract": { "title": "t", "files_touched": ["override.mjs"] },
            "microtask": { "intent": "x", "files_touched": ["override.mjs"] },
            "sandbox_root": SB
        }), &c).unwrap();
        assert_eq!(over["files"][0]["from_sandbox"], json!(true));
        assert!(over["bundle_md"].as_str().unwrap().contains("fromSandbox"));

        let fallback = devBundleSlice(&json!({
            "contract": { "title": "t", "files_touched": ["fallback.mjs"] },
            "microtask": { "intent": "x", "files_touched": ["fallback.mjs"] },
            "sandbox_root": SB
        }), &c).unwrap();
        assert_eq!(fallback["files"][0]["from_sandbox"], json!(false));
        assert!(fallback["bundle_md"].as_str().unwrap().contains("onlyInWorkspace"));
    }

    #[test]
    fn references_scored_sorted_desc_and_capped_to_two_object_acceptance_pretty_printed_in_key_order() {
        let c = db();
        seed_active(&c, WS);
        attach_memory(&c);
        c.execute_batch(
            "INSERT INTO memory.skills (name, description) VALUES \
             ('Auth Flow Helper', 'Handles authFlow token refresh and handler dispatch'), \
             ('Handler Utils', 'generic handler update logic refactor helpers'), \
             ('Logic Refactor Kit', 'pure logic refactor with authflow token cache'), \
             ('Totally Unrelated', 'nothing matches here at all')",
        ).unwrap();
        let r = devBundleSlice(&json!({
            "contract": { "title": "refactor authFlow handler", "files_touched": [], "acceptance": [ { "kind": "test", "command": "npm test", "expect": "exit_code_0" } ] },
            "microtask": { "intent": "update authFlow handler logic", "files_touched": [] }
        }), &c).unwrap();
        assert_eq!(r["references_count"], json!(2));
        let md = r["bundle_md"].as_str().unwrap();
        assert!(md.contains("- **Handler Utils**: generic handler update logic refactor helpers"));
        assert!(md.contains("- **Logic Refactor Kit**: pure logic refactor with authflow token cache"));
        assert!(!md.contains("Auth Flow Helper"), "3rd-highest score must be excluded by the top-2 cap");
        assert!(!md.contains("Totally Unrelated"), "zero-score rows must be filtered out");
        assert!(md.find("Handler Utils").unwrap() < md.find("Logic Refactor Kit").unwrap(), "sorted by score descending");
        assert!(md.ends_with("[\n  {\n    \"kind\": \"test\",\n    \"command\": \"npm test\",\n    \"expect\": \"exit_code_0\"\n  }\n]"));
    }

    #[test]
    fn references_query_error_when_memory_not_attached_is_caught_like_the_js_try_catch() {
        let c = db(); // no ATTACH memory at all — "no such table: memory.skills"
        seed_active(&c, WS);
        let r = devBundleSlice(&json!({
            "contract": { "title": "t", "files_touched": [] },
            "microtask": null
        }), &c).unwrap();
        assert_eq!(r["references_count"], json!(0));
    }

    #[test]
    fn bundle_truncates_at_target_bundle_bytes_with_marker() {
        let dir = std::env::temp_dir().join(format!("antiloki-dev-bundle-slice-rs-trunc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..3 {
            // just under WHOLE_FILE_THRESHOLD so each ships in full; 3 * ~8500 > TARGET_BUNDLE_BYTES
            let body = format!("export function bigFn{i}(x) {{\n  // {}\n  return x;\n}}\n", "a".repeat(8400));
            std::fs::write(dir.join(format!("f{i}.mjs")), body).unwrap();
        }
        let c = db();
        seed_active(&c, dir.to_str().unwrap());
        attach_memory(&c);
        let r = devBundleSlice(&json!({
            "contract": { "title": "t", "files_touched": ["f0.mjs", "f1.mjs", "f2.mjs"] },
            "microtask": null
        }), &c).unwrap();
        let md = r["bundle_md"].as_str().unwrap();
        assert!(md.ends_with(&format!("/* ... bundle truncated to fit {TARGET_BUNDLE_BYTES} bytes ... */")));
        assert_eq!(r["total_bytes"].as_i64().unwrap() as usize, js_len(md));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_export_lines_caps_at_twelve_and_clips_long_lines() {
        let mut src = String::new();
        for i in 0..15 { src.push_str(&format!("export function fn{i}() {{}}\n")); }
        let lines = extract_export_lines(&src);
        assert_eq!(lines.len(), 12);
        assert_eq!(lines[0], "export function fn0");

        let long_name = "a".repeat(150);
        let clipped = extract_export_lines(&format!("export const {long_name} = 1;"));
        assert_eq!(clipped[0].chars().count(), 100);
    }

    #[test]
    fn extract_matching_section_returns_none_without_keywords_or_without_a_match() {
        assert_eq!(extract_matching_section("export function foo() {}", &[], MAX_FILE_SECTION_BYTES), None);
        assert_eq!(extract_matching_section("export function foo() {}", &["bar".to_string()], MAX_FILE_SECTION_BYTES), None);
    }

    #[test]
    fn derive_keywords_dedupes_in_first_seen_order_and_converts_kebab_to_camel() {
        let kws = derive_keywords(
            &json!({ "intent": "rework viewIntent renderer for card-review flow" }),
            &json!({ "title": "add board branch handling" }),
        );
        assert!(kws.contains(&"viewintent".to_string()));
        assert!(kws.contains(&"renderer".to_string()));
        assert!(kws.contains(&"cardreview".to_string()), "card-review -> cardReview -> lowercased");
        assert!(kws.contains(&"board".to_string()));
    }

    #[test]
    fn clip_to_whole_lines_never_splits_a_line_and_appends_marker() {
        let lines = vec!["a".repeat(10), "b".repeat(10), "c".repeat(10)];
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let clipped = clip_to_whole_lines(&refs, 15);
        assert_eq!(clipped, format!("{}\n/* ... section truncated ... */", "a".repeat(10)));
    }
}
