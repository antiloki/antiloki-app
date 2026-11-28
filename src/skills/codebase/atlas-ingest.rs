//! atlas-ingest.rs — twin of skills/codebase/atlas-ingest.mjs, the whole-repo atlas builder:
//! discover files under a root, parse each (ast-parse), resolve import + call + flow edges, write
//! the linked graph (atlas-build, origin='atlas'). NAMES ARE CONTRACT: `globToRe` · `matchGlob` ·
//! `discover` · `hashSource` · `extractCsNamespaces` · `readProjectConfig` · `isFlow` ·
//! `looksFlowTyped` · `partitionFamilies` · `atlasIngest` (default). `SKIP_DIRS` is transcribed as
//! a `pub` const fn/static (Rust has no exported-const-Set literal as clean as JS's `export const`).
//!
//! BESPOKE-NATIVE (real fs walk of a real temp dir + real sqlite writes), not conform-db: this
//! skill shells no subprocess but does real, non-deterministic-shaped I/O (recursive directory
//! walk, per-file reads, a multi-table write burst) that the two-lane JSON-fixture harness cannot
//! stage. Proven below via `#[cfg(test)]` against a real temp dir of small JS/TS fixtures + an
//! in-memory sqlite db seeded via the already-converted `codebase_code_schema::ensureCodeSchema`
//! twin — same class as terminal-sandbox.rs / auth-sign.rs (real fs + real db, no fixture).
//!
//! Cross-module calls (every one an already-converted twin, reused not reimplemented):
//! `codebase_ast_parse::{adapterFor, astParse}`, `codebase_dep_resolve::depResolve`,
//! `codebase_symbol_table_build::symbolTableBuild`, `codebase_binding_resolve::bindingResolve`,
//! `codebase_flow_resolve::{extractFlow, resolveFlowFromExtracts}`,
//! `codebase_atlas_entry_points::detectEntryPoints`, `codebase_design_tokens_extract::{grammarFor,
//! extractTokensFromTree}`, `codebase_design_components_extract::extractComponents`,
//! `codebase_parse_treesitter::parseToTree`, `codebase_code_signals_detect::detectFileSignals`,
//! `codebase_atlas_build::atlasBuild`, `system_code_rebuild_settings::readRebuildSettings`,
//! `engine_workspace_root::getWorkspaceRoot`, `db_json::run_write`.
//!
//! ## PARITY-DEFERRED / disclosed divergences (documented here, not silently dropped)
//! 1. **No `context.control` / `onProgress` channel.** The Rust dispatch signature is
//!    `(input: &Value, db: &Connection)` — there is no FE progress-poll or pause/resume control
//!    object to plumb through. The JS source's `flush()`/`shouldPause()` machinery (rebuild-wizard
//!    UI observability) is entirely omitted; it never affects the returned atlas, only how a
//!    long-running build is *watched*. See terminal-sandbox.rs's module doc for the same class of
//!    "no context object to receive this" omission.
//! 2. **No resume / family checkpoints.** `code_family_checkpoints` persistence and the
//!    `input.resume` fast-path are not ported — every call here is a fresh full build (JS's
//!    `resume: false` path). `families_reused` is therefore always `0`. This is a legitimate
//!    consequence of (1): resume exists to let a *paused* build continue, and pause has no signal
//!    to arrive on in this signature.
//! 3. **`ast-parse.rs`'s own PARITY-DEFERRED scope** (only javascript/typescript/tsx/python have a
//!    native parser; every other bundled tree-sitter grammar returns `Value::Null` — see that
//!    module's doc) surfaces here as: a file in an out-of-scope language gets a bare file node and
//!    a `status: "skipped"` health row (`detail: "language not yet ported to the native parser"`)
//!    instead of the real JS's always-populated symbol/import/call extraction for that language.
//! 4. **Thrown-error reclassification is coarser.** The JS source distinguishes a tree-sitter WASM
//!    engine crash (`e.parser_engine`) from a genuine parse failure by an exception FIELD; Rust's
//!    `astParse` returns a plain `Err(String)` with no such tag (its own scope is already narrower
//!    per (3), so the WASM-crash case cannot arise there). This twin keeps the `looksFlowTyped`
//!    reclassification (pragma-less Flow libdefs) but drops the `parser_engine` branch — any other
//!    `Err` is recorded as a real `fail`.
//! 5. **No per-file/family timing telemetry** (`slowest`, `slow_file` alarms) — FE-observability
//!    only, never part of the atlas the function returns.
//! 6. **Directory walk order.** `std::fs::read_dir` does not guarantee entry order the way a given
//!    OS's `readdirSync` happens to; this twin sorts each directory's entries by name before
//!    recursing, for deterministic + reasonable output (same disclosed-approximation category as
//!    every other twin that ports a filesystem walk).
//! Everything else — file discovery + scope filters, the flow/health/error classification tree,
//! symbol/import/call pooling, the depResolve → symbolTableBuild → bindingResolve → flow-resolve
//! stitch, and the final `code_parse_health` rewrite — is ported faithfully.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

fn truthy(v: Option<&Value>) -> bool { match v { None|Some(Value::Null)=>false, Some(Value::Bool(b))=>*b, Some(Value::Number(n))=>n.as_f64().map_or(false,|f|f!=0.0&&!f.is_nan()), Some(Value::String(s))=>!s.is_empty(), Some(_)=>true } }
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

/// `export const SKIP_DIRS = new Set([...])`.
pub fn skip_dirs() -> &'static HashSet<&'static str> {
    static S: OnceLock<HashSet<&'static str>> = OnceLock::new();
    S.get_or_init(|| ["node_modules", ".git", "dist", "build", "out", "coverage", ".next", ".cache"].into_iter().collect())
}

/// IS THIS DIRECTORY NEVER PROJECT SOURCE? The fixed skip set, any `node_modules*`, and ANTILOKI'S OWN FOLDERS - `.antiloki`, `.antiloki-nm-host-aside` (the copy of node_modules the preview boot parks in the
/// project root when it reinstalls), `.antiloki-tmp`... (operator 2026-10-09: the description plan for ministore-2 proposed 40 files of `.antiloki-nm-host-aside/postcss/` - 477 indexed files, 83 "describable", all of
/// them a parked copy of a dependency). Pure - tested.
pub fn is_skipped_dir(name: &str) -> bool { skip_dirs().contains(name) || name.starts_with("node_modules") || name.starts_with(".antiloki") }

/// `export function globToRe(glob)` — tiny glob → RegExp (`**`=any, `*`=one segment, `?`=one char).
#[allow(non_snake_case)]
pub fn globToRe(glob: &Value) -> String {
    let g = str_qq(Some(glob)).replace('\\', "/");
    let chars: Vec<char> = g.chars().collect();
    let mut re = String::from("^");
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '*' {
            if i + 1 < chars.len() && chars[i + 1] == '*' {
                re.push_str(".*");
                i += 1;
                if i + 1 < chars.len() && chars[i + 1] == '/' { i += 1; }
            } else {
                re.push_str("[^/]*");
            }
        } else if c == '?' {
            re.push_str("[^/]");
        } else if ".+^${}()|[]\\".contains(c) {
            re.push('\\');
            re.push(c);
        } else {
            re.push(c);
        }
        i += 1;
    }
    re.push('$');
    re
}

fn glob_to_regex(glob: &str) -> regex::Regex {
    regex::Regex::new(&globToRe(&json!(glob))).unwrap()
}

/// `export function matchGlob(relPath, globs)`.
#[allow(non_snake_case)]
pub fn matchGlob(rel_path: &str, globs: &[String]) -> bool {
    let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
    for g in globs {
        let re = glob_to_regex(g);
        if re.is_match(rel_path) || (!g.contains('/') && re.is_match(base)) { return true; }
    }
    false
}

fn walk(dir: &Path, rel_dir: &str, include: &[String], exclude: &[String], out: &mut Vec<String>) {
    let mut entries: Vec<_> = match fs::read_dir(dir) { Ok(rd) => rd.filter_map(|e| e.ok()).collect(), Err(_) => return };
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let rel = if rel_dir.is_empty() { name.clone() } else { format!("{rel_dir}/{name}") };
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            if is_skipped_dir(name.as_str()) { continue; }
            if !exclude.is_empty() && matchGlob(&rel, exclude) { continue; }
            walk(&dir.join(&name), &rel, include, exclude, out);
        } else if crate::codebase_ast_parse::adapterFor(&json!(name)) != Value::Null {
            if !exclude.is_empty() && matchGlob(&rel, exclude) { continue; }
            if !include.is_empty() && !matchGlob(&rel, include) { continue; }
            out.push(rel);
        }
    }
}

/// `export function discover(root, opts = {})`.
pub fn discover(root: &str, include: &[String], exclude: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    walk(Path::new(root), "", include, exclude, &mut out);
    out
}

/// `export const hashSource = (source) => createHash('sha256').update(String(source)).digest('hex')`.
#[allow(non_snake_case)]
pub fn hashSource(source: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(source.as_bytes());
    hex::encode(h.finalize())
}

/// `export function extractCsNamespaces(source)`.
#[allow(non_snake_case)]
pub fn extractCsNamespaces(source: &str) -> Vec<String> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?m)^[ \t]*namespace\s+([A-Za-z_][\w.]*)").unwrap());
    re.captures_iter(source).map(|c| c[1].to_string()).collect()
}

/// The `workspaces` globs of a JS monorepo: the root `package.json` (`[...]` or `{packages: [...]}`), else `pnpm-workspace.yaml`'s `packages:` list. A negated glob (`!x`) is kept, with its `!`.
fn workspace_globs(root: &Path) -> Vec<String> {
    if let Ok(raw) = fs::read_to_string(root.join("package.json")) {
        if let Ok(pj) = serde_json::from_str::<Value>(&raw) {
            let ws = match pj.get("workspaces") { Some(Value::Object(o)) => o.get("packages").cloned(), other => other.cloned() };
            if let Some(Value::Array(a)) = ws {
                return a.iter().filter_map(|v| v.as_str()).map(|g| g.trim_start_matches("./").trim_end_matches('/').to_string()).collect();
            }
        }
    }
    let mut out = Vec::new();
    if let Ok(y) = fs::read_to_string(root.join("pnpm-workspace.yaml")) {
        let mut in_packages = false;
        for line in y.lines() {
            let t = line.trim();
            if t.starts_with("packages:") { in_packages = true; continue; }
            if in_packages {
                if let Some(item) = t.strip_prefix("- ") {
                    let g = item.trim().trim_matches(|c| c == '\'' || c == '"');
                    out.push(g.trim_start_matches("./").trim_end_matches('/').to_string());
                } else if !t.is_empty() && !t.starts_with('#') { in_packages = false; }
            }
        }
    }
    out
}

/// Directories (relative to `root`, POSIX) that one workspace glob names: a literal path, `*` for one level, `**` for any depth (capped at 4). Never enters `node_modules` or a dot-directory.
fn expand_workspace_glob(root: &Path, glob: &str) -> Vec<String> {
    fn go(root: &Path, base: &str, segs: &[&str], depth: usize, out: &mut Vec<String>) {
        let Some((seg, rest)) = segs.split_first() else { out.push(base.to_string()); return; };
        let here = if base.is_empty() { root.to_path_buf() } else { root.join(base) };
        let join = |name: &str| if base.is_empty() { name.to_string() } else { format!("{base}/{name}") };
        if *seg == "*" || *seg == "**" {
            let Ok(rd) = fs::read_dir(&here) else { return };
            let mut names: Vec<String> = rd.filter_map(|e| e.ok()).filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| !n.starts_with('.') && n != "node_modules").collect();
            names.sort();
            for n in names {
                go(root, &join(&n), rest, depth, out);
                if *seg == "**" && depth < 4 { go(root, &join(&n), segs, depth + 1, out); }
            }
            if *seg == "**" { go(root, base, rest, depth, out); }
        } else if here.join(seg).is_dir() {
            go(root, &join(seg), rest, depth, out);
        }
    }
    let segs: Vec<&str> = glob.split('/').filter(|s| !s.is_empty()).collect();
    let mut out = Vec::new();
    go(root, "", &segs, 0, &mut out);
    out.sort();
    out.dedup();
    out
}

/// The named packages of a JS monorepo: `[{name, dir, main}]` from each workspace directory's `package.json`. `dep-resolve` turns `import 'shared/x'` into `<dir of shared>/x` with it. Empty for a repo that declares no workspaces.
pub fn jsWorkspacePackages(root: &str) -> Vec<Value> {
    let root = Path::new(root);
    let mut pkgs: Vec<Value> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let (negated, positive): (Vec<String>, Vec<String>) = workspace_globs(root).into_iter().partition(|g| g.starts_with('!'));
    let excluded: std::collections::HashSet<String> = negated.iter().flat_map(|g| expand_workspace_glob(root, &g[1..])).collect();
    for glob in positive {
        for dir in expand_workspace_glob(root, &glob) {
            if excluded.contains(&dir) || !seen.insert(dir.clone()) { continue; }
            let Ok(raw) = fs::read_to_string(root.join(&dir).join("package.json")) else { continue };
            let Ok(pj) = serde_json::from_str::<Value>(&raw) else { continue };
            let Some(name) = pj.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()) else { continue };
            pkgs.push(json!({ "name": name, "dir": dir, "main": pj.get("main").and_then(Value::as_str) }));
            if pkgs.len() >= 2000 { return pkgs; }
        }
    }
    pkgs
}

/// `export function readProjectConfig(root)` — best-effort go.mod / composer.json read, never fatal.
#[allow(non_snake_case)]
pub fn readProjectConfig(root: &str) -> Value {
    let mut config = Map::new();
    if let Ok(go_mod) = fs::read_to_string(Path::new(root).join("go.mod")) {
        static RE: OnceLock<regex::Regex> = OnceLock::new();
        let re = RE.get_or_init(|| regex::Regex::new(r"(?m)^\s*module\s+(\S+)").unwrap());
        if let Some(c) = re.captures(&go_mod) { config.insert("goModule".into(), json!(c[1].to_string())); }
    }
    let ws = jsWorkspacePackages(root);
    if !ws.is_empty() { config.insert("jsWorkspaces".into(), json!(ws)); }
    if let Ok(composer_raw) = fs::read_to_string(Path::new(root).join("composer.json")) {
        if let Ok(composer) = serde_json::from_str::<Value>(&composer_raw) {
            if let Some(psr4) = composer.get("autoload").and_then(|a| a.get("psr-4")).and_then(|v| v.as_object()) {
                let mut php_autoload: Vec<Value> = Vec::new();
                for (prefix, dir) in psr4 {
                    let d = match dir { Value::Array(a) => a.first().cloned(), other => Some(other.clone()) };
                    if let Some(Value::String(ds)) = d { php_autoload.push(json!({ "prefix": prefix, "dir": ds })); }
                }
                if !php_autoload.is_empty() { config.insert("phpAutoload".into(), json!(php_autoload)); }
            }
        }
    }
    Value::Object(config)
}

/// `export function isFlow(relPath, source)`.
#[allow(non_snake_case)]
pub fn isFlow(rel_path: &str, source: &str) -> bool {
    static EXT: OnceLock<regex::Regex> = OnceLock::new();
    static PRAGMA: OnceLock<regex::Regex> = OnceLock::new();
    let ext_re = EXT.get_or_init(|| regex::Regex::new(r"(?i)\.flow\.[cm]?jsx?$").unwrap());
    if ext_re.is_match(rel_path) { return true; }
    let pragma_re = PRAGMA.get_or_init(|| regex::Regex::new(r"@flow\b").unwrap());
    let head: String = source.chars().take(500).collect();
    pragma_re.is_match(&head)
}

/// What the parse-health row of a file whose parser recovered from errors says: for a Flow file the
/// count and the first message (so the next fix to `stripFlow` starts from the real construct).
pub fn degraded_detail(parsed: &Value) -> String {
    match parsed.get("first_error").and_then(Value::as_str) {
        Some(first) if parsed.get("flow").and_then(Value::as_bool) == Some(true) => {
            let n = parsed.get("error_count").and_then(Value::as_i64).unwrap_or(0);
            format!("flow syntax: {n} error(s) after type erasure, first: {first}")
        }
        _ => "parser reported syntax errors".to_string(),
    }
}

/// `export function looksFlowTyped(source)`.
#[allow(non_snake_case)]
pub fn looksFlowTyped(source: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?m)^\s*(?:import\s+type\s|export\s+type\s|type\s+[A-Za-z_$][\w$]*\s*=|opaque\s+type\s|declare\s+(?:class|module|function|var|let|const|type|interface|opaque|export)\b)").unwrap());
    let head: String = source.chars().take(2000).collect();
    re.is_match(&head)
}

/// The largest source file either parse lane reads (the atlas lane's `max_file_bytes` default). The sync lane had no cap at all (audit 2026-10-01, F-06).
pub const DEFAULT_MAX_FILE_BYTES: i64 = 1_048_576;

/// `export function partitionFamilies(files, sizeOf, caps = {})`.
#[allow(non_snake_case)]
pub fn partitionFamilies(files: &[String], size_of: impl Fn(&str) -> i64, max_tokens: i64, max_files: i64) -> Vec<Vec<String>> {
    let max_tokens = max_tokens.max(1);
    let max_files = max_files.max(1);
    let mut families: Vec<Vec<String>> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut cur_tokens: i64 = 0;
    for f in files {
        let tokens = ((size_of(f).max(0)) as f64 / 4.0).ceil() as i64;
        if !cur.is_empty() && (cur_tokens + tokens > max_tokens || (cur.len() as i64 + 1) > max_files) {
            families.push(std::mem::take(&mut cur));
            cur_tokens = 0;
        }
        cur.push(f.clone());
        cur_tokens += tokens;
    }
    if !cur.is_empty() { families.push(cur); }
    families
}

/// `export default async function atlasIngest(input, context)`. See module doc for the
/// PARITY-DEFERRED boundaries (no progress/pause channel, no resume, ast-parse's own language
/// scope). Async-over-sync per the playbook's async lane — every real `await` here is over sync
/// SQLite/fs calls, so this twin is an ordinary sync `pub fn`.
#[allow(non_snake_case)]
pub fn atlasIngest(input: &Value, db: &Connection) -> Result<Value, String> {
    let root_v = match nz(input.get("root")) { Some(r) => r.clone(), None => crate::engine_workspace_root::getWorkspaceRoot(db) };
    let root = str_qq(Some(&root_v));
    let origin = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));
    let origin_str = str_qq(Some(&origin));

    let include: Vec<String> = input.get("include").and_then(|v| v.as_array()).map(|a| a.iter().map(|x| str_qq(Some(x))).collect()).unwrap_or_default();
    let exclude: Vec<String> = input.get("exclude").and_then(|v| v.as_array()).map(|a| a.iter().map(|x| str_qq(Some(x))).collect()).unwrap_or_default();

    let files: Vec<String> = match input.get("files").and_then(|v| v.as_array()) {
        Some(arr) if !arr.is_empty() => arr.iter().map(|f| str_qq(Some(f)).replace('\\', "/")).collect(),
        _ => discover(&root, &include, &exclude),
    };

    let caps = crate::system_code_rebuild_settings::readRebuildSettings(db);
    let max_tokens = caps.get("max_tokens").and_then(Value::as_i64).unwrap_or(250000);
    let max_file_bytes = caps.get("max_file_bytes").and_then(Value::as_i64).unwrap_or(DEFAULT_MAX_FILE_BYTES);
    let max_files = caps.get("max_files").and_then(Value::as_i64).unwrap_or(400);

    let size_of = |rel: &str| -> i64 { fs::metadata(Path::new(&root).join(rel)).map(|m| m.len() as i64).unwrap_or(0) };
    let families = partitionFamilies(&files, size_of, max_tokens, max_files);
    let tokens_total: i64 = files.iter().map(|f| ((size_of(f).max(0)) as f64 / 4.0).ceil() as i64).sum();

    let lanes = truthy(input.get("lanes"));

    let mut node_count: i64 = 0;
    let mut symbol_defs: Vec<Value> = Vec::new();
    let mut all_imports: Vec<Value> = Vec::new();
    let mut all_calls: Vec<Value> = Vec::new();
    let mut flow_extracts: Vec<Value> = Vec::new();
    let mut lane_entries: Vec<Value> = Vec::new();
    let mut lane_tokens: Vec<Value> = Vec::new();
    let mut lane_comps: Vec<Value> = Vec::new();
    let mut lane_signals: Vec<Value> = Vec::new();
    let mut ns_index: Map<String, Value> = Map::new();
    let mut errors: Vec<Value> = Vec::new();
    let mut health: Vec<Value> = Vec::new();

    crate::db_json::run_write(db, "DELETE FROM code_edges WHERE origin = ?", &[origin.clone()])?;
    crate::db_json::run_write(db, "DELETE FROM code_nodes WHERE origin = ?", &[origin.clone()])?;
    if lanes {
        let _ = crate::db_json::run_write(db, "DELETE FROM code_entry_points WHERE origin = ?", &[origin.clone()]);
    }

    // HEARTBEAT. The job row's `updated_at` is the ONLY liveness signal the status poller has, and
    // codebase-onboard only writes it at PHASE boundaries. Ingest is one phase that runs for as
    // long as the repo is big, so a healthy rebuild went >5 minutes without touching the row and
    // the poller's zombie-heal declared it "stale: the BE restarted mid-onboard" — killing a job
    // that was working, and blaming a restart that never happened. Families are the natural tick:
    // bounded by max_files/max_tokens, so this fires often on a big repo and costs one UPDATE.
    let hb_job_id: Option<String> = nz(input.get("job_id")).map(|v| str_qq(Some(v))).filter(|s| !s.is_empty());
    let heartbeat = |db: &Connection| {
        if let Some(id) = &hb_job_id {
            let _ = crate::db_json::run_write(
                db,
                &format!("UPDATE code_onboard_jobs SET updated_at = {} WHERE job_id = ?", crate::clock::now_sql()),
                &[json!(id)],
            );
        }
    };

    for family in &families {
        heartbeat(db);
        let mut family_nodes: Vec<Value> = Vec::new();
        let mut f_imports: Vec<Value> = Vec::new();
        let mut f_calls: Vec<Value> = Vec::new();
        let mut f_flow: Vec<Value> = Vec::new();
        let mut f_health: Vec<Value> = Vec::new();
        let mut f_errors: Vec<Value> = Vec::new();
        let mut f_entries: Vec<Value> = Vec::new();
        let mut f_tokens: Vec<Value> = Vec::new();
        let mut f_comps: Vec<Value> = Vec::new();
        let mut f_signals: Vec<Value> = Vec::new();
        let mut f_ns_index: Map<String, Value> = Map::new();

        for rel_path in family {
            let full = Path::new(&root).join(rel_path);
            let source = match fs::read_to_string(&full) {
                Ok(s) => s,
                Err(e) => {
                    f_errors.push(json!({ "path": rel_path, "error": format!("read: {e}") }));
                    f_health.push(json!({ "path": rel_path, "lang": null, "status": "fail", "detail": "unreadable" }));
                    continue;
                }
            };

            if source.len() as i64 > max_file_bytes {
                f_health.push(json!({ "path": rel_path, "lang": null, "status": "fail", "detail": format!("skipped: {}B > max_file_bytes {}", source.len(), max_file_bytes) }));
                family_nodes.push(json!({ "id": rel_path, "path": rel_path, "type": "file", "content_hash": hashSource(&source) }));
                continue;
            }

            if lanes {
                if let Some(arr) = crate::codebase_atlas_entry_points::detectEntryPoints(&json!(source)).as_array() {
                    for e in arr {
                        let mut m = e.as_object().cloned().unwrap_or_default();
                        m.insert("node_id".into(), json!(rel_path));
                        m.insert("path".into(), json!(rel_path));
                        f_entries.push(Value::Object(m));
                    }
                }
                if let Some(arr) = crate::codebase_code_signals_detect::detectFileSignals(&json!(source), &json!(rel_path)).as_array() {
                    f_signals.extend(arr.iter().cloned());
                }
                if let Some(g) = crate::codebase_design_tokens_extract::grammarFor(rel_path) {
                    let tree = crate::codebase_parse_treesitter::parseToTree(&source, g);
                    f_tokens.extend(crate::codebase_design_tokens_extract::extractTokensFromTree(tree.as_ref(), &source, rel_path));
                    if let Some(t) = &tree {
                        f_comps.extend(crate::codebase_design_components_extract::extractComponents(Some(t.root_node()), rel_path, source.as_bytes()));
                    }
                }
            }

            f_flow.push(crate::codebase_flow_resolve::extractFlow(&json!(rel_path), &json!(source)));

            match crate::codebase_ast_parse::astParse(&json!({ "path": rel_path, "source": source }), db) {
                Ok(Value::Null) => {
                    // A grammar with no walk profile — every ADAPTERS grammar has one today (see parse-treesitter.rs "Scope").
                    f_health.push(json!({ "path": rel_path, "lang": null, "status": "skipped", "detail": "no walk profile for this grammar (see parse-treesitter.rs 'Scope')" }));
                    family_nodes.push(json!({ "id": rel_path, "path": rel_path, "type": "file", "content_hash": hashSource(&source) }));
                }
                Ok(parsed) => {
                    let lang = parsed.get("lang").cloned().unwrap_or(Value::Null);
                    if truthy(parsed.get("has_error")) {
                        f_health.push(json!({ "path": rel_path, "lang": if truthy(parsed.get("flow")) { json!("flow") } else { lang }, "status": "degraded", "detail": degraded_detail(&parsed) }));
                    }
                    let mut nodes: Vec<Value> = parsed.get("nodes").and_then(|v| v.as_array()).cloned().unwrap_or_default();
                    if let Some(file_node) = nodes.iter_mut().find(|n| n.get("type").and_then(Value::as_str) == Some("file")) {
                        if let Some(obj) = file_node.as_object_mut() { obj.insert("content_hash".into(), json!(hashSource(&source))); }
                    }
                    if let Some(imports) = parsed.get("imports").and_then(|v| v.as_array()) {
                        for imp in imports {
                            f_imports.push(json!({ "from_path": rel_path, "specifier": imp.get("specifier").cloned().unwrap_or(Value::Null), "kind": imp.get("kind").cloned().unwrap_or(json!("value")) }));
                        }
                    }
                    if let Some(calls) = parsed.get("calls").and_then(|v| v.as_array()) {
                        for c in calls {
                            f_calls.push(json!({ "from_path": rel_path, "caller": c.get("caller").cloned().unwrap_or(Value::Null), "callee": c.get("callee").cloned().unwrap_or(Value::Null), "via": c.get("via").cloned().unwrap_or(Value::Null) }));
                        }
                    }
                    // definitions passed by name → `uses` edges (binding-resolve drops the ones that are locals)
                    if let Some(refs) = parsed.get("refs").and_then(|v| v.as_array()) {
                        for c in refs {
                            f_calls.push(json!({ "from_path": rel_path, "caller": c.get("caller").cloned().unwrap_or(Value::Null), "callee": c.get("callee").cloned().unwrap_or(Value::Null), "via": "ref" }));
                        }
                    }
                    if rel_path.to_lowercase().ends_with(".cs") {
                        for ns in extractCsNamespaces(&source) {
                            f_ns_index.entry(ns).or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!(rel_path));
                        }
                    }
                    family_nodes.extend(nodes);
                }
                Err(e) => {
                    let lang = if e.starts_with("flow:") { json!("flow") } else { crate::codebase_ast_parse::adapterFor(&json!(rel_path)).get("langs").and_then(|l| l.get(0)).cloned().unwrap_or(Value::Null) };
                    f_errors.push(json!({ "path": rel_path, "error": e }));
                    f_health.push(json!({ "path": rel_path, "lang": lang, "status": "fail", "detail": e }));
                    family_nodes.push(json!({ "id": rel_path, "path": rel_path, "type": "file", "content_hash": hashSource(&source) }));
                }
            }
        }

        crate::codebase_atlas_build::atlasBuild(&json!({ "origin": origin, "nodes": family_nodes, "edges": [] }), db)?;
        if lanes && !f_entries.is_empty() {
            for r in &f_entries {
                let _ = crate::db_json::run_write(
                    db,
                    "INSERT INTO code_entry_points (origin, kind, subtype, node_id, path, label, http_method, detail, confidence, line, framework) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                    &[
                        origin.clone(),
                        r.get("kind").cloned().unwrap_or(Value::Null),
                        r.get("subtype").cloned().unwrap_or(Value::Null),
                        r.get("node_id").cloned().unwrap_or(Value::Null),
                        r.get("path").cloned().unwrap_or(Value::Null),
                        r.get("label").cloned().unwrap_or(Value::Null),
                        nz(r.get("http_method")).cloned().unwrap_or(Value::Null),
                        nz(r.get("detail")).cloned().unwrap_or(Value::Null),
                        r.get("confidence").cloned().unwrap_or(Value::Null),
                        nz(r.get("line")).cloned().unwrap_or(Value::Null),
                        nz(r.get("framework")).cloned().unwrap_or(Value::Null),
                    ],
                );
            }
        }

        node_count += family_nodes.len() as i64;
        for n in &family_nodes {
            symbol_defs.push(json!({
                "id": n.get("id").cloned().unwrap_or(Value::Null),
                "path": n.get("path").cloned().unwrap_or(Value::Null),
                "type": n.get("type").cloned().unwrap_or(Value::Null),
                "name": n.get("name").cloned().unwrap_or(Value::Null),
            }));
        }
        all_imports.extend(f_imports);
        all_calls.extend(f_calls);
        flow_extracts.extend(f_flow);
        for (k, v) in f_ns_index {
            let entry = ns_index.entry(k).or_insert_with(|| json!([]));
            if let (Value::Array(dst), Value::Array(src)) = (entry, v) { dst.extend(src); }
        }
        health.extend(f_health);
        errors.extend(f_errors);
        lane_entries.extend(f_entries);
        lane_tokens.extend(f_tokens);
        lane_comps.extend(f_comps);
        lane_signals.extend(f_signals);
    }

    let file_ids: Vec<Value> = symbol_defs.iter()
        .filter(|n| n.get("type").and_then(Value::as_str) == Some("file"))
        .map(|n| n.get("id").cloned().unwrap_or(Value::Null))
        .collect();

    let mut config = readProjectConfig(&root);
    if let Value::Object(ref mut m) = config { m.insert("nsIndex".into(), Value::Object(ns_index)); }

    let dep_result = crate::codebase_dep_resolve::depResolve(&json!({ "files": file_ids, "imports": all_imports, "config": config }), db)?;
    let import_edges: Vec<Value> = dep_result.get("edges").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let total = dep_result.get("total").and_then(Value::as_i64).unwrap_or(0);
    let resolved = dep_result.get("resolved").and_then(Value::as_i64).unwrap_or(0);

    let symbols = crate::codebase_symbol_table_build::symbolTableBuild(&json!({ "nodes": symbol_defs }), db)?;
    let binding_result = crate::codebase_binding_resolve::bindingResolve(&json!({ "calls": all_calls, "symbols": symbols, "importEdges": import_edges }), db)?;
    let call_edges: Vec<Value> = binding_result.get("edges").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    let flow_edges_v = crate::codebase_flow_resolve::resolveFlowFromExtracts(&json!(flow_extracts));
    let flow_edges: Vec<Value> = flow_edges_v.as_array().cloned().unwrap_or_default();

    let mut edges: Vec<Value> = Vec::new();
    edges.extend(import_edges);
    edges.extend(call_edges.clone());
    edges.extend(flow_edges.clone());
    crate::codebase_atlas_build::atlasBuild(&json!({ "origin": origin, "nodes": [], "edges": edges }), db)?;

    // EPIC-TYPECHECK P2: rewrite this origin's parse-health snapshot. Best-effort (JS: `try {} catch {}`).
    if crate::db_json::run_write(db, "DELETE FROM code_parse_health WHERE origin = ?", &[origin.clone()]).is_ok() {
        for h in &health {
            let detail: String = str_qq(h.get("detail")).chars().take(500).collect();
            let _ = crate::db_json::run_write(
                db,
                "INSERT OR REPLACE INTO code_parse_health (origin, path, lang, status, detail) VALUES (?,?,?,?,?)",
                &[origin.clone(), h.get("path").cloned().unwrap_or(Value::Null), h.get("lang").cloned().unwrap_or(Value::Null), h.get("status").cloned().unwrap_or(Value::Null), json!(detail)],
            );
        }
    }

    let fail = health.iter().filter(|h| h.get("status").and_then(Value::as_str) == Some("fail")).count() as i64;
    let degraded = health.iter().filter(|h| h.get("status").and_then(Value::as_str) == Some("degraded")).count() as i64;
    let skipped = health.iter().filter(|h| h.get("status").and_then(Value::as_str) == Some("skipped")).count() as i64;

    let mut out = Map::new();
    out.insert("root".into(), json!(root.replace('\\', "/")));
    out.insert("files".into(), json!(files.len() as i64));
    out.insert("nodes".into(), json!(node_count));
    out.insert("edges".into(), json!(edges.len() as i64));
    out.insert("call_edges".into(), json!(call_edges.len() as i64));
    out.insert("flow_edges".into(), json!(flow_edges.len() as i64));
    out.insert("unresolved".into(), json!(total - resolved));
    out.insert("families".into(), json!(families.len() as i64));
    out.insert("families_reused".into(), json!(0i64));
    out.insert("tokens_total".into(), json!(tokens_total));
    if lanes {
        out.insert("lanes".into(), json!({ "entries": lane_entries, "design_tokens": lane_tokens, "design_components": lane_comps, "signals": lane_signals }));
    }
    out.insert("parse_health".into(), json!({ "fail": fail, "degraded": degraded, "skipped": skipped }));
    out.insert("errors".into(), json!(errors));

    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as stdfs;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("antiloki-atlas-ingest-rs-{tag}-{}", std::process::id()));
        let _ = stdfs::remove_dir_all(&dir);
        stdfs::create_dir_all(&dir).unwrap();
        dir
    }

    fn db_with_schema() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::codebase_code_schema::ensureCodeSchema(&c, &Value::Null);
        c
    }

    #[test]
    fn glob_to_re_and_match_glob() {
        assert!(matchGlob("src/App.test.tsx", &["**/*.test.tsx".to_string()]));
        assert!(matchGlob("src/x.test.tsx", &["*.test.tsx".to_string()]));
        assert!(!matchGlob("src/App.tsx", &["*.test.tsx".to_string()]));
        assert!(matchGlob("a/b/c.js", &["a/**".to_string()]));
    }

    #[test]
    fn hash_source_is_stable_sha256_hex() {
        let h1 = hashSource("hello");
        let h2 = hashSource("hello");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
        assert_ne!(h1, hashSource("hello world"));
    }

    #[test]
    fn extract_cs_namespaces_finds_declarations() {
        let src = "using System;\nnamespace Foo.Bar {\n  class X {}\n}\nnamespace Baz;\n";
        assert_eq!(extractCsNamespaces(src), vec!["Foo.Bar".to_string(), "Baz".to_string()]);
    }

    #[test]
    fn is_flow_detects_extension_and_pragma() {
        assert!(isFlow("widget.flow.js", ""));
        assert!(isFlow("widget.js", "// @flow\nconst x = 1;"));
        assert!(!isFlow("widget.js", "const x = 1;"));
    }

    #[test]
    fn looks_flow_typed_detects_declare_and_import_type() {
        assert!(looksFlowTyped("declare module 'foo' {}\n"));
        assert!(looksFlowTyped("import type { X } from './x';\n"));
        assert!(!looksFlowTyped("import { X } from './x';\n"));
    }

    #[test]
    fn partition_families_respects_file_and_token_caps() {
        let files: Vec<String> = (0..5).map(|i| format!("f{i}.js")).collect();
        let sizes = [100i64, 100, 100, 100, 100];
        let size_of = |f: &str| -> i64 { let i: usize = f[1..2].parse().unwrap(); sizes[i] };
        let fams = partitionFamilies(&files, size_of, 1_000_000, 2);
        assert_eq!(fams, vec![vec!["f0.js".to_string(), "f1.js".to_string()], vec!["f2.js".to_string(), "f3.js".to_string()], vec!["f4.js".to_string()]]);
    }

    #[test]
    fn discover_walks_real_dir_skipping_node_modules() {
        let dir = temp_dir("discover");
        stdfs::create_dir_all(dir.join("src")).unwrap();
        stdfs::create_dir_all(dir.join("node_modules/pkg")).unwrap();
        stdfs::write(dir.join("src/a.js"), "export const a = 1;\n").unwrap();
        stdfs::write(dir.join("node_modules/pkg/index.js"), "module.exports = {};\n").unwrap();
        stdfs::write(dir.join("README.md"), "# hi\n").unwrap(); // no adapter → excluded
        let files = discover(dir.to_str().unwrap(), &[], &[]);
        assert_eq!(files, vec!["src/a.js".to_string()]);
        let _ = stdfs::remove_dir_all(&dir);
    }

    #[test]
    fn atlas_ingest_builds_a_real_atlas_and_resolves_an_import_edge() {
        let dir = temp_dir("ingest");
        stdfs::write(dir.join("a.mjs"), "import { b } from './b.mjs';\nexport function useA() { return b(); }\n").unwrap();
        stdfs::write(dir.join("b.mjs"), "export function b() { return 1; }\n").unwrap();
        let c = db_with_schema();

        let r = atlasIngest(&json!({ "root": dir.to_str().unwrap() }), &c).unwrap();

        assert_eq!(r["files"], json!(2));
        assert_eq!(r["errors"], json!([]));
        assert!(r["nodes"].as_i64().unwrap() > 2, "file nodes plus function nodes");
        assert!(r["edges"].as_i64().unwrap() >= 1, "at least the a.mjs -> b.mjs import edge");
        assert_eq!(r.get("lanes"), None, "lanes omitted when input.lanes is not true");

        let row: i64 = c.query_row(
            "SELECT COUNT(*) FROM code_edges WHERE origin='atlas' AND from_node='a.mjs' AND to_node='b.mjs' AND type='imports'",
            [], |row| row.get(0),
        ).unwrap();
        assert_eq!(row, 1, "the real import edge landed in code_edges");

        let node_row: i64 = c.query_row("SELECT COUNT(*) FROM code_nodes WHERE origin='atlas' AND id='a.mjs'", [], |row| row.get(0)).unwrap();
        assert_eq!(node_row, 1);

        let _ = stdfs::remove_dir_all(&dir);
    }

    #[test]
    fn atlas_ingest_is_a_full_replace_on_rerun() {
        let dir = temp_dir("replace");
        stdfs::write(dir.join("a.mjs"), "export const a = 1;\n").unwrap();
        let c = db_with_schema();

        let r1 = atlasIngest(&json!({ "root": dir.to_str().unwrap() }), &c).unwrap();
        assert_eq!(r1["files"], json!(1));

        stdfs::write(dir.join("b.mjs"), "export const b = 2;\n").unwrap();
        let r2 = atlasIngest(&json!({ "root": dir.to_str().unwrap() }), &c).unwrap();
        assert_eq!(r2["files"], json!(2));

        let node_count: i64 = c.query_row("SELECT COUNT(*) FROM code_nodes WHERE origin='atlas' AND type='file'", [], |row| row.get(0)).unwrap();
        assert_eq!(node_count, 2, "the stale first-run node set was replaced, not accumulated");

        let _ = stdfs::remove_dir_all(&dir);
    }

    #[test]
    fn atlas_ingest_records_a_read_error_for_a_missing_file_in_the_explicit_list() {
        let dir = temp_dir("missing");
        stdfs::create_dir_all(&dir).unwrap();
        let c = db_with_schema();

        let r = atlasIngest(&json!({ "root": dir.to_str().unwrap(), "files": ["nope.mjs"] }), &c).unwrap();
        assert_eq!(r["files"], json!(1));
        let errs = r["errors"].as_array().unwrap();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0]["path"], json!("nope.mjs"));
        assert!(errs[0]["error"].as_str().unwrap().starts_with("read:"));

        let _ = stdfs::remove_dir_all(&dir);
    }

    /// 2026-10-01: a `@flow` file was recorded `skipped` ("the JS/TS parser has no Flow support") and kept only
    /// its bare file node — the React repo's 1160 core files had no symbols and no edges. Now it is parsed.
    #[test]
    fn atlas_ingest_parses_flow_files_instead_of_skipping_them() {
        let dir = temp_dir("flow");
        stdfs::write(dir.join("legacy.js"), "// @flow\nopaque type X = number;\nexport type P = {| +a: ?X |};\nexport function make(a: ?X): P { return {a}; }\n").unwrap();
        stdfs::write(dir.join("use.js"), "/* @flow */\nimport {make} from './legacy';\nimport type {P} from './legacy';\nexport const p: P = (make(1): any);\n").unwrap();
        let c = db_with_schema();

        let r = atlasIngest(&json!({ "root": dir.to_str().unwrap() }), &c).unwrap();
        assert_eq!(r["errors"], json!([]));
        assert_eq!(r["parse_health"]["skipped"], json!(0), "a Flow file is no longer skipped");
        assert_eq!(r["parse_health"]["fail"], json!(0));
        assert_eq!(r["parse_health"]["degraded"], json!(0), "{r}");

        let sym: i64 = c.query_row("SELECT COUNT(*) FROM code_nodes WHERE origin='atlas' AND id='legacy.js#make' AND type='function'", [], |row| row.get(0)).unwrap();
        assert_eq!(sym, 1, "the function behind the annotations is a symbol");
        let edge: i64 = c.query_row("SELECT COUNT(*) FROM code_edges WHERE origin='atlas' AND from_node='use.js' AND to_node='legacy.js' AND type='imports'", [], |row| row.get(0)).unwrap();
        let kinds: Vec<String> = {
            let mut st = c.prepare("SELECT DISTINCT kind FROM code_edges WHERE origin='atlas' AND from_node='use.js' AND to_node='legacy.js' AND type='imports' ORDER BY kind").unwrap();
            st.query_map([], |row| row.get::<_, String>(0)).unwrap().map(|x| x.unwrap()).collect()
        };
        assert_eq!(edge, 2, "and the import edges between two Flow files exist");
        assert_eq!(kinds, vec!["type", "value"], "`import type` stays a type edge, `import {{make}}` a value edge");

        let _ = stdfs::remove_dir_all(&dir);
    }

    /// A Flow file oxc still cannot read is a `fail` with lang `flow` and the reason — never a silent empty "ok".
    #[test]
    fn atlas_ingest_reports_an_unreadable_flow_file_as_a_failure() {
        let dir = temp_dir("flowfail");
        stdfs::write(dir.join("broken.js"), "// @flow\nfunction (((( {{{ %%%\n").unwrap();
        let c = db_with_schema();

        let r = atlasIngest(&json!({ "root": dir.to_str().unwrap() }), &c).unwrap();
        let counted = r["parse_health"]["fail"].as_i64().unwrap() + r["parse_health"]["degraded"].as_i64().unwrap();
        assert_eq!(counted, 1, "{r}");
        let row: (String, String) = c.query_row("SELECT lang, status FROM code_parse_health WHERE path='broken.js'", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(row.0, "flow");
        assert!(row.1 == "fail" || row.1 == "degraded");

        let _ = stdfs::remove_dir_all(&dir);
    }

    fn fx_flow() -> String { format!("{}/fixtures/fx-flow", env!("CARGO_MANIFEST_DIR")) }

    /// The real React files, in a cut-down monorepo: `getComponentNameFromType` and `ReactChildFiber` have
    /// symbols, and "who imports shared/getComponentNameFromType" lists the bare-`shared/…` importers.
    #[test]
    fn atlas_ingest_indexes_real_react_flow_files_and_resolves_workspace_package_names() {
        let c = db_with_schema();
        let r = atlasIngest(&json!({ "root": fx_flow() }), &c).unwrap();
        assert_eq!(r["parse_health"]["skipped"], json!(0), "{r}");
        assert_eq!(r["parse_health"]["fail"], json!(0), "{r}");

        let syms = |file: &str| -> i64 { c.query_row("SELECT COUNT(*) FROM code_nodes WHERE origin='atlas' AND id LIKE ? AND type!='file'", [format!("{file}#%")], |row| row.get(0)).unwrap() };
        assert!(syms("packages/shared/getComponentNameFromType.js") >= 3, "getComponentNameFromType + its helpers");
        assert!(syms("packages/react-reconciler/src/ReactChildFiber.js") >= 10, "the child reconciler's functions");

        let importers: Vec<String> = {
            let mut st = c.prepare("SELECT from_node FROM code_edges WHERE origin='atlas' AND type='imports' AND to_node='packages/shared/getComponentNameFromType.js' ORDER BY from_node").unwrap();
            st.query_map([], |row| row.get::<_, String>(0)).unwrap().map(|x| x.unwrap()).collect()
        };
        assert_eq!(importers, vec!["packages/react-dom/src/describe.js", "packages/react-dom/src/plain.js", "packages/react-reconciler/src/getComponentNameFromFiber.js"], "the bare `shared/…` alias resolves to the workspace package, from Flow AND plain JS");
        let child_fiber: i64 = c.query_row("SELECT COUNT(*) FROM code_edges WHERE origin='atlas' AND type='imports' AND from_node='packages/react-reconciler/src/ReactChildFiber.js' AND to_node='packages/react-reconciler/src/getComponentNameFromFiber.js'", [], |row| row.get(0)).unwrap();
        assert_eq!(child_fiber, 1, "`react-reconciler/src/…` names the package's own folder");
        let barrel: i64 = c.query_row("SELECT COUNT(*) FROM code_edges WHERE origin='atlas' AND type='imports' AND from_node='packages/react-dom/src/describe.js' AND to_node='packages/react-reconciler/index.js'", [], |row| row.get(0)).unwrap();
        assert_eq!(barrel, 1, "a bare package name is that package's `main`");
    }

    #[test]
    fn js_workspace_packages_read_package_json_globs_and_pnpm_yaml() {
        let dir = temp_dir("ws");
        stdfs::write(dir.join("package.json"), r#"{"name":"root","workspaces":{"packages":["libs/*","tools/cli","!libs/skipme"]}}"#).unwrap();
        for (d, name) in [("libs/a", "@org/a"), ("libs/b", "b"), ("libs/skipme", "skipme"), ("tools/cli", "cli")] {
            stdfs::create_dir_all(dir.join(d)).unwrap();
            stdfs::write(dir.join(d).join("package.json"), format!(r#"{{"name":"{name}","main":"lib/index.js"}}"#)).unwrap();
        }
        stdfs::create_dir_all(dir.join("libs/nopkg")).unwrap();
        let names: Vec<String> = jsWorkspacePackages(dir.to_str().unwrap()).iter().map(|p| p["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, vec!["@org/a", "b", "cli"], "globs expand, `!libs/skipme` removes one, and a dir without package.json is not a package");

        stdfs::remove_file(dir.join("package.json")).unwrap();
        stdfs::write(dir.join("pnpm-workspace.yaml"), "packages:\n  - 'tools/*'\n  - \"libs/a\"\nother: 1\n").unwrap();
        let names: Vec<String> = jsWorkspacePackages(dir.to_str().unwrap()).iter().map(|p| p["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, vec!["cli", "@org/a"]);

        let _ = stdfs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod antiloki_dirs_tests {
    use super::*;
    #[test]
    fn antilokis_own_folders_are_never_indexed_as_project_source() {
        for d in [".antiloki", ".antiloki-nm-host-aside", ".antiloki-nm-docker-aside", ".antiloki-tmp", "node_modules", "node_modules.bak", ".git", "dist"] { assert!(is_skipped_dir(d), "{d}"); }
        for d in ["src", "antiloki", "lib", ".github", ".storybook", "my.antiloki"] { assert!(!is_skipped_dir(d), "{d}"); }
    }
}

