//! context-catalog.rs — EVERY antiIDE feature a code-agent pane can ask for, in one table. NO JS TWIN.
//!
//! Added 2026-09-14 (operator: "it should be able to retrieve any antiIDE feature — folder paper,
//! AST, partial AST, git, file history, authors…"). Each row is a SOURCE: the args it takes, whether
//! it can be baked into the static pack (`packable`) or is live-only, whether it costs an AI call,
//! and how to run it. The table drives three surfaces at once — the pack item kinds, the
//! `antiloki_query` MCP tool (with `antiloki_sources` listing this table), and the FE "add" picker —
//! so a feature added here is reachable everywhere without a second edit.
//!
//! Rendering: the reviews, laws, tests, blast and check keep their hand-written markdown
//! (review-markdown.rs). Everything else is rendered by ONE deterministic JSON→markdown projection
//! (`json_md`): scalars as `key: value` lines, uniform arrays as tables, nested objects as
//! sub-sections, long strings cut with the cut stated. Same input → same bytes.
//!
//! AI-backed sources (`ai: true`) are never in a default pack and are listed so the agent knows
//! they cost a call; the engine's own caches (content-hash keyed descriptions etc.) still apply.
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::path::Path;

pub struct Source {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// (name, description) — `path` is worktree-relative wherever it appears
    pub args: &'static [(&'static str, &'static str)],
    pub packable: bool,
    pub ai: bool,
}

const P: (&str, &str) = ("path", "worktree-relative file path");
const F: (&str, &str) = ("path", "worktree-relative folder path");

pub const CATALOG: &[Source] = &[
    // ── the measured reviews (hand-rendered) ─────────────────────────────────────────────
    Source { key: "laws", label: "the ratified laws", description: "the constitution every edit is judged against: weight, severity, applies-to, exceptions", args: &[], packable: true, ai: false },
    Source { key: "file", label: "file review", description: "health, the seven axes with what drives each weak one and the fix, violated laws with exact lines, importers, tests, regression", args: &[P], packable: true, ai: false },
    Source { key: "folder", label: "folder review", description: "the folder paper: roster, health and logic-only mean, members worst first, violations, trajectory", args: &[F], packable: true, ai: false },
    Source { key: "graph", label: "import graph", description: "runtime import edges touching a folder (type-only excluded), as JSON", args: &[F], packable: true, ai: false },
    Source { key: "tests", label: "test map", description: "which tests reach a folder's files (static-import basis), or 'unmeasured'", args: &[F], packable: true, ai: false },
    Source { key: "blast", label: "blast radius", description: "who breaks transitively if a file changes, and which tests to run first", args: &[P], packable: true, ai: false },
    Source { key: "references", label: "references", description: "who references a file's symbols: the functions that call, use, render or dispatch each one (by file, with the referencing function's first line), and the files that import the file - what a rename or a message change actually needs", args: &[P, ("symbol", "optional symbol name; default every top-level symbol of the file (at most 12)")], packable: true, ai: false },
    Source { key: "check", label: "constitution check", description: "re-judge a file's CURRENT content against the laws vs the last snapshot: fixed / introduced / remaining", args: &[P, ("content", "optional candidate content; defaults to the file on disk")], packable: false, ai: false },
    // ── structure ────────────────────────────────────────────────────────────────────────
    Source { key: "ast", label: "AST", description: "the file's definitions (functions, classes, components, hooks, types) with spans, signatures, exports, complexity; its imports (kind), calls and refs. Partial: `symbol` keeps one definition, `lines` keeps the definitions overlapping a range; `sexp` adds the tree-sitter s-expression of that region", args: &[P, ("symbol", "optional definition name — partial AST"), ("lines", "optional `start-end` line range — partial AST"), ("sexp", "optional true → include the syntax tree s-expression for the range/symbol")], packable: true, ai: false },
    Source { key: "symbols", label: "symbol search", description: "find definitions across the atlas by name substring", args: &[("query", "name or fragment"), ("limit", "max matches (default 40)")], packable: false, ai: false },
    Source { key: "imports", label: "imports", description: "what one file imports and what imports it, resolved through the atlas", args: &[P], packable: true, ai: false },
    Source { key: "subgraph", label: "subgraph", description: "the N-hop neighbourhood of one or more files in the code graph (nodes + edges)", args: &[("paths", "list of worktree-relative paths"), ("hops", "1-5, default 1")], packable: true, ai: false },
    Source { key: "trace", label: "trace path", description: "the shortest import/call path between two files or symbols", args: &[("from", "path or symbol name"), ("to", "path or symbol name")], packable: false, ai: false },
    Source { key: "neighborhood", label: "neighborhood", description: "rings of files around one file with their health and issues (the file review's neighborhood tab)", args: &[P, ("depth", "rings, default 2")], packable: true, ai: false },
    Source { key: "diagram", label: "file diagram", description: "the internal call/flow diagram of one file (mermaid)", args: &[P], packable: true, ai: false },
    Source { key: "tree", label: "file tree", description: "the atlas file tree under a folder", args: &[("path", "optional worktree-relative folder; default the repo root")], packable: true, ai: false },
    Source { key: "context", label: "codebase context", description: "the files and symbols the atlas associates with a set of paths and/or search terms", args: &[("paths", "optional list of paths"), ("terms", "optional list of terms"), ("hops", "default 1")], packable: false, ai: false },
    Source { key: "bundle", label: "smart context bundle", description: "the deterministic context bundle antiIDE builds for a question (strongest sources first, budgeted, dropped ones listed)", args: &[("question", "the question"), ("token_budget", "default 20000")], packable: false, ai: false },
    // ── health & metrics ─────────────────────────────────────────────────────────────────
    Source { key: "metrics", label: "atlas metrics", description: "repo-wide metrics: the top lists (central, risky, churn, complex, gravity) and, under a folder, the per-file fan-in/out, complexity, churn, coverage", args: &[("top", "size of the top lists, default 20"), ("path", "optional folder — per-file metrics under it")], packable: true, ai: false },
    Source { key: "smells", label: "smells", description: "god files, cycles, orphans, hubs — the graph smells across the repo", args: &[], packable: true, ai: false },
    Source { key: "entry_points", label: "entry points", description: "where execution enters (routes, ui, outbound calls, config reads), with counts; per-file under a folder", args: &[("path", "optional folder filter")], packable: true, ai: false },
    Source { key: "signals", label: "code signals", description: "the detected signals (security, perf, correctness…) across the repo", args: &[("category", "optional filter"), ("kind", "optional filter"), ("limit", "default 100")], packable: true, ai: false },
    Source { key: "coverage", label: "coverage", description: "lcov / coverage report for the repo or one path, when a coverage run exists", args: &[("path", "optional worktree-relative path")], packable: true, ai: false },
    Source { key: "report", label: "constitution report", description: "the latest judge run: totals, worst files, laws hit", args: &[], packable: true, ai: false },
    Source { key: "atlas_report", label: "atlas report", description: "the latest atlas report with its trend", args: &[], packable: true, ai: false },
    Source { key: "digest", label: "code digest", description: "the latest deterministic repo digest (shape, sizes, hotspots)", args: &[], packable: true, ai: false },
    Source { key: "decorators", label: "decorators", description: "the atlas decorators of one file (flags, exported, notes)", args: &[P], packable: true, ai: false },
    Source { key: "review_history", label: "review history", description: "one file's judge snapshots over time (health, score, axes)", args: &[P, ("limit", "default 12")], packable: true, ai: false },
    // ── git ──────────────────────────────────────────────────────────────────────────────
    Source { key: "git", label: "git info", description: "branch, HEAD, recent commits, status of the workspace", args: &[("limit", "commits, default 20")], packable: true, ai: false },
    Source { key: "file_history", label: "file history", description: "the commits that touched one file (renames followed): sha, author, date, subject", args: &[P, ("limit", "default 30")], packable: true, ai: false },
    Source { key: "authors", label: "authors", description: "who wrote what: top authors and the files each owns", args: &[("top_authors", "default 10"), ("files_per_author", "default 8")], packable: true, ai: false },
    Source { key: "history", label: "repo history", description: "the recent commit log of the workspace", args: &[("limit", "default 40")], packable: true, ai: false },
    // ── docs & papers ────────────────────────────────────────────────────────────────────
    Source { key: "docs", label: "code docs page", description: "antiIDE's generated documentation page for a file or folder (markdown)", args: &[("path", "worktree-relative file or folder; empty = the root overview")], packable: true, ai: false },
    Source { key: "pack", label: "context pack", description: "a CONTEXT PACK the operator consolidated in the context library (by slug) — the AIs' memories, session docs, papers, skills, pasted notes; or the list of packs when no slug is given", args: &[("slug", "optional pack slug; empty = the list")], packable: true, ai: false },
    Source { key: "paper", label: "saved paper", description: "a paper the operator pressed in antiIDE (by slug), or the list of papers when no slug is given", args: &[("slug", "optional paper slug")], packable: true, ai: false },
    Source { key: "source", label: "file contents", description: "the file as the atlas sees it (bounded)", args: &[P], packable: false, ai: false },
    // ── X-ray, the rest of the page (operator 2026-11-10: "list all features inside x-ray ... add all that to the mcp, so the mcp can read any of that") ───────────────
    Source { key: "rules", label: "the rules", description: "every rule the QA server checks, as the Quality tab shows them: key, title, scope, detector and its config, weight, severity, enforcement, on or off, how many violations it hits now, and its exceptions", args: &[("status", "optional: ratified | suspended | candidate")], packable: false, ai: false },
    Source { key: "system_diagram", label: "system diagram", description: "the Architecture tab's system diagram: boxes (folder at a depth, with files and health) and the labelled arrows between them (import counts); as JSON and a mermaid flowchart", args: &[("path", "optional folder to draw inside (default the root)"), ("depth", "folder depth of a box, 1-4 (default 2)")], packable: false, ai: false },
    Source { key: "design", label: "design system", description: "the Design tab: the palette, categories and components harvested from the real UI", args: &[], packable: false, ai: false },
    Source { key: "design_snapshots", label: "design snapshots", description: "the Design tab's frames: the captured screens of the app", args: &[], packable: false, ai: false },
    Source { key: "ui_stories", label: "UI stories", description: "the recorded UI journeys of the app", args: &[], packable: false, ai: false },
    Source { key: "data_stories", label: "data stories", description: "the Architecture tab's data stories: entry -> every file crossed -> death point; the saved ones (or one, by `id`)", args: &[("id", "optional story id: that story in full")], packable: false, ai: false },
    Source { key: "architecture_history", label: "architecture over time", description: "the History tab's architecture over time: health, cycles, coverage and smells per rebuild, the trend and the latest structural incident", args: &[], packable: false, ai: false },
    Source { key: "review_movers", label: "review history (all files)", description: "the History tab's review history: files scored across snapshots, the movers and the axis drift", args: &[], packable: false, ai: false },
    Source { key: "working_tree", label: "working tree and git graph", description: "the History tab's working-tree diff and git graph: branch, commits, what is changed and not committed, the last commit's files, the repos", args: &[], packable: false, ai: false },
    Source { key: "telemetry", label: "live signals", description: "the History tab's substrate telemetry: AI spend, latency and cache, and the rebuild's phase timings", args: &[], packable: false, ai: false },
    Source { key: "typecheck", label: "type-check status", description: "which type-checkers this workspace has and which languages they cover", args: &[], packable: false, ai: false },
    Source { key: "linters", label: "linter status", description: "which linters this workspace has installed and which it runs", args: &[], packable: false, ai: false },
    Source { key: "deps_audit", label: "dependency audit", description: "known vulnerabilities in the dependencies, with the sources that were read", args: &[], packable: false, ai: false },
    Source { key: "scope", label: "atlas scope", description: "what the atlas indexes: the extensions and the excluded folders", args: &[], packable: false, ai: false },
    // ── AI-backed (live only; the engine caches by content where it can) ─────────────────
    Source { key: "describe", label: "file description", description: "antiIDE's AI essay on one file (cached by content hash)", args: &[P], packable: false, ai: true },
    Source { key: "symbol_explain", label: "symbol explanation", description: "AI explanation of one atlas symbol", args: &[("node_id", "atlas node id (from ast / symbols)")], packable: false, ai: true },
    Source { key: "explain_snippet", label: "explain snippet", description: "AI explanation of a line range of a file", args: &[P, ("lines", "`start-end`")], packable: false, ai: true },
    Source { key: "logic_spec", label: "logic spec", description: "AI-derived logic spec of a file against an intent", args: &[P, ("intent", "what the file is meant to do")], packable: false, ai: true },
    Source { key: "architecture", label: "architecture digest", description: "AI architecture digest of the repo", args: &[("max_files", "default 60")], packable: false, ai: true },
    Source { key: "docs_overview", label: "docs overview", description: "AI overview page for a folder", args: &[F], packable: false, ai: true },
];

pub fn source(key: &str) -> Option<&'static Source> { CATALOG.iter().find(|s| s.key == key) }
pub fn packable_keys() -> Vec<&'static str> { CATALOG.iter().filter(|s| s.packable).map(|s| s.key).collect() }

pub fn catalog_json() -> Value {
    json!(CATALOG.iter().map(|s| json!({
        "key": s.key, "label": s.label, "description": s.description,
        "args": s.args.iter().map(|(n, d)| json!({ "name": n, "description": d })).collect::<Vec<_>>(),
        "packable": s.packable, "ai": s.ai,
    })).collect::<Vec<_>>())
}

pub fn catalog_md() -> String {
    let mut out = vec!["# antiloki sources — what `antiloki_query {source, args}` can fetch".to_string(), String::new()];
    for s in CATALOG {
        let args = if s.args.is_empty() { "no args".to_string() } else { s.args.iter().map(|(n, d)| format!("`{n}` ({d})")).collect::<Vec<_>>().join(", ") };
        out.push(format!("- **{}** `{}`{}{} — {}. args: {}", s.label, s.key, if s.ai { " · AI call" } else { "" }, if s.packable { "" } else { " · live only" }, s.description, args));
    }
    out.join("\n")
}

fn s(v: Option<&Value>) -> String { match v { Some(Value::String(x)) => x.clone(), Some(Value::Null) | None => String::new(), Some(Value::Number(n)) => n.to_string(), Some(Value::Bool(b)) => b.to_string(), Some(o) => o.to_string() } }
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn take(sv: &str, n: usize) -> String { if sv.chars().count() <= n { sv.to_string() } else { format!("{}… (cut at {n} chars)", sv.chars().take(n).collect::<String>()) } }
fn arg_i64(args: &Value, k: &str, d: i64) -> i64 { nz(args.get(k)).and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|x| x.parse().ok()))).unwrap_or(d) }

/// worktree-relative, no escapes — the agent names files the way the atlas does.
pub fn clean_path(raw: &str) -> Result<String, String> {
    let p = raw.replace('\\', "/");
    if p.starts_with('/') || p.split('/').any(|seg| seg == "..") || p.contains(':') { return Err(format!("path `{raw}` must be worktree-relative")); }
    let p = p.strip_prefix("./").unwrap_or(&p).trim_matches('/').to_string();
    Ok(p)
}
fn need_path(args: &Value) -> Result<String, String> {
    let p = clean_path(&s(args.get("path")))?;
    if p.is_empty() { return Err("path is required".into()); }
    Ok(p)
}

// ── the one JSON → markdown projection ──────────────────────────────────────────────────────
const STR_CAP: usize = 400;
const ROWS_CAP: usize = 80;
const COLS_CAP: usize = 8;

fn scalar(v: &Value) -> String {
    match v {
        Value::String(x) => take(&x.replace('\n', " ⏎ ").replace('|', "/"), STR_CAP),
        Value::Null => "—".into(),
        Value::Array(a) if a.is_empty() => "—".into(),
        Value::Array(a) if a.iter().all(|x| !x.is_object() && !x.is_array()) => a.iter().map(scalar).collect::<Vec<_>>().join(", "),
        Value::Array(a) => format!("[{} items]", a.len()),
        // a small flat object reads inline; anything richer is summarised
        Value::Object(o) if o.len() <= 8 && o.values().all(|x| !x.is_object() && !x.is_array()) => o.iter().map(|(k, x)| format!("{k}={}", scalar(x))).collect::<Vec<_>>().join(" "),
        Value::Object(o) => format!("{{{} keys}}", o.len()),
        o => o.to_string(),
    }
}

fn table(rows: &[Value], out: &mut Vec<String>) {
    let mut cols: Vec<String> = Vec::new();
    for r in rows.iter().take(ROWS_CAP) {
        if let Some(o) = r.as_object() { for k in o.keys() { if !cols.contains(k) && cols.len() < COLS_CAP { cols.push(k.clone()); } } }
    }
    if cols.is_empty() { return; }
    out.push(format!("| {} |", cols.join(" | ")));
    out.push(format!("|{}|", cols.iter().map(|_| "---").collect::<Vec<_>>().join("|")));
    for r in rows.iter().take(ROWS_CAP) {
        let o = r.as_object().cloned().unwrap_or_default();
        out.push(format!("| {} |", cols.iter().map(|c| scalar(o.get(c).unwrap_or(&Value::Null))).collect::<Vec<_>>().join(" | ")));
    }
    if rows.len() > ROWS_CAP { out.push(format!("… {} more rows", rows.len() - ROWS_CAP)); }
}

fn render(v: &Value, depth: usize, out: &mut Vec<String>) {
    match v {
        Value::Object(o) => {
            let mut nested: Vec<(&String, &Value)> = Vec::new();
            for (k, x) in o {
                match x {
                    Value::Object(inner) if !inner.is_empty() => nested.push((k, x)),
                    Value::Array(a) if a.iter().any(|e| e.is_object()) && !a.is_empty() => nested.push((k, x)),
                    _ => out.push(format!("- {k}: {}", scalar(x))),
                }
            }
            for (k, x) in nested {
                out.push(String::new());
                out.push(format!("{} {k}", "#".repeat((depth + 2).min(6))));
                render(x, depth + 1, out);
            }
        }
        Value::Array(a) if a.iter().any(|e| e.is_object()) => {
            if a.iter().all(|e| e.is_object()) { table(a, out); } else { for e in a.iter().take(ROWS_CAP) { let mut sub = Vec::new(); render(e, depth + 1, &mut sub); out.extend(sub); } }
        }
        Value::Array(a) => { for e in a.iter().take(ROWS_CAP) { out.push(format!("- {}", scalar(e))); } if a.len() > ROWS_CAP { out.push(format!("… {} more", a.len() - ROWS_CAP)); } }
        other => out.push(scalar(other)),
    }
}

/// Deterministic markdown for any JSON value.
pub fn json_md(title: &str, v: &Value) -> String {
    let mut out: Vec<String> = vec![format!("# {title}"), String::new()];
    render(v, 0, &mut out);
    out.join("\n")
}

fn stamp(key: &str, path: &str, generated: &str) -> String {
    format!("<!-- antiloki context · kind={key}{} · generated={generated} -->\n\n", if path.is_empty() { String::new() } else { format!(" · path={path}") })
}

pub struct Out { pub content: String, pub summary: String, pub json: bool }

fn md_out(key: &str, path: &str, title: &str, v: &Value, summary: String, generated: &str) -> Out {
    Out { content: format!("{}{}", stamp(key, path, generated), json_md(title, v)), summary, json: false }
}

fn count(v: &Value, k: &str) -> usize { v.get(k).and_then(Value::as_array).map(|a| a.len()).unwrap_or(0) }

/// The `references` item as markdown: per symbol, the referencing functions grouped by file (`kind`: calls / uses / renders / dispatches; the number is the line the referencing function starts on - the
/// index stores the function, not the call's own line), then the files that import the file. Returns (markdown, places, files). The index sees only what it resolved: a reference through a dynamic access or
/// an unresolved import is not here, and the item says so. Pure - tested.
pub fn references_md(path: &str, found: &[(String, Vec<(String, String, i64, String)>)], importers: &[String], generated: &str) -> (String, usize, usize) {
    let mut md = format!("{}# References to the symbols of {path}\n\n", stamp("references", path, generated));
    let (mut places, mut files_all) = (0usize, std::collections::BTreeSet::new());
    for (name, refs) in found {
        md.push_str(&format!("## `{name}` - {} place(s)\n", refs.len()));
        let mut by_file: std::collections::BTreeMap<&str, Vec<String>> = std::collections::BTreeMap::new();
        for (file, func, line, kind) in refs { by_file.entry(file.as_str()).or_default().push(format!("{}{} ({kind}{})", if func.is_empty() { "(module level)" } else { func.as_str() }, "", if *line > 0 { format!(", from line {line}") } else { String::new() })); files_all.insert(file.clone()); places += 1; }
        for (file, items) in &by_file { md.push_str(&format!("- `{file}`: {}\n", items.join("; "))); }
        if refs.is_empty() { md.push_str("- (no referencing function found in the index)\n"); }
        md.push('\n');
    }
    md.push_str(&format!("## Files that import {path} ({})\n", importers.len()));
    for f in importers { md.push_str(&format!("- `{f}`\n")); }
    md.push_str("\n_Read this as a lower bound: the index lists what it resolved. A reference through a dynamic access, a string, or an import it could not resolve is not here - search the tree for the name before you rely on a count._\n");
    (md, places, files_all.len())
}

/// Run one source. `worktree` is the pane's box (file reads for `ast`/`check` come from there —
/// the agent's version, not the workspace's).
pub fn run(db: &Connection, worktree: &str, key: &str, args: &Value, generated: &str) -> Result<Out, String> {
    let src = source(key).ok_or_else(|| format!("unknown source `{key}` — ask antiloki_sources for the list"))?;
    let rm = |v: &Value, t: &str, summary: String, path: &str| Ok(md_out(src.key, path, t, v, summary, generated));
    match key {
        "laws" => {
            let composed = crate::constitution_constitution_law::composedConstitution(db);
            let n = composed.get("laws").and_then(Value::as_array).map(|a| a.iter().filter(|l| l.get("status").and_then(Value::as_str) == Some("ratified")).count()).unwrap_or(0);
            Ok(Out { content: crate::constitution__helpers_review_markdown::laws_md(&composed, generated), summary: format!("{n} ratified laws — the ruleset your edits are judged against"), json: false })
        }
        "file" | "folder" => {
            let path = need_path(args)?;
            let r = crate::constitution_constitution_file::constitutionFile(&json!({ "path": path }), db).unwrap_or_else(|e| json!({ "path": path, "found": false, "doc_md": e }));
            let found = r.get("found").and_then(Value::as_bool) == Some(true);
            let is_folder = r.get("is_folder").and_then(Value::as_bool).unwrap_or(false) || (key == "folder" && !found);
            if is_folder {
                let summary = if found { format!("folder review: {} files, health {} (logic-only {}), {} violations", si(r.get("files")), si(r.get("health")), si(r.get("health_logic")), count(&r, "violations")) } else { "folder review: no snapshot covers it yet".into() };
                Ok(Out { content: crate::constitution__helpers_review_markdown::folder_review_md(&r, generated), summary, json: false })
            } else {
                let summary = if found { format!("file review {}/100 (law score {}): weakest {}, {} violations", si(r.get("health")), si(r.get("score")), weakest_axis(r.get("axes").unwrap_or(&Value::Null)), count(&r, "violations")) } else { "file review: no snapshot covers it yet".into() };
                Ok(Out { content: crate::constitution__helpers_review_markdown::file_review_md(&r, generated), summary, json: false })
            }
        }
        "graph" => {
            let path = need_path(args)?;
            let atlas = load_atlas(db);
            let g = crate::constitution__helpers_review_markdown::graph_json(&path, &atlas.nodes, &atlas.edges, generated);
            let c = g.get("counts").cloned().unwrap_or(json!({}));
            Ok(Out { content: serde_json::to_string_pretty(&g).unwrap_or_default(), summary: format!("runtime import graph: {} member files, {} internal / {} outbound / {} inbound edges", si(c.get("members")), si(c.get("internal")), si(c.get("outbound")), si(c.get("inbound"))), json: true })
        }
        "tests" => {
            let path = need_path(args)?;
            let tm = crate::codebase_atlas_test_map::atlasTestMap(&json!({ "origin": "atlas" }), db).unwrap_or_else(|_| json!({ "instrument": { "test_files": 0 }, "coverage_by_source": {} }));
            let prefix = format!("{}/", path.trim_end_matches('/'));
            let covered = tm.get("coverage_by_source").and_then(Value::as_object).map(|m| m.keys().filter(|k| k.starts_with(&prefix)).count()).unwrap_or(0);
            let members = crate::db_json::query_rows(db, "SELECT COUNT(*) AS n FROM code_nodes WHERE origin='atlas' AND type='file' AND path LIKE ?", &[json!(format!("{prefix}%"))]).ok().and_then(|v| v[0].get("n").and_then(Value::as_i64)).unwrap_or(0);
            let summary = if tm.get("instrument").and_then(|i| i.get("test_files")).and_then(Value::as_i64).unwrap_or(0) == 0 { "tests: unmeasured (no test files in the atlas)".to_string() } else { format!("tests reaching the folder: {covered} of {members} files have a test importing them") };
            Ok(Out { content: crate::constitution__helpers_review_markdown::tests_md(&tm, &path, generated), summary, json: false })
        }
        "blast" => {
            let atlas = load_atlas(db);
            let mut paths: Vec<String> = args.get("paths").and_then(Value::as_array).map(|a| a.iter().map(|p| s(Some(p))).collect()).unwrap_or_default();
            if let Some(p) = nz(args.get("path")) { paths.push(s(Some(p))); }
            if paths.is_empty() { return Err("path (or paths[]) is required".into()); }
            let mut parts: Vec<String> = Vec::new(); let mut total = 0i64; let mut tests = 0usize;
            for raw in &paths {
                let p = clean_path(raw)?;
                match atlas.id_of.get(&p) {
                    Some(id) => {
                        let b = crate::codebase_blast_radius::blastRadius(&json!({ "node_ids": [id] }), db)?;
                        total += b.get("count").and_then(Value::as_i64).unwrap_or(0);
                        tests += count(&b, "affected_tests");
                        parts.push(crate::constitution__helpers_review_markdown::blast_md(&b, &p, &atlas.path_of, generated));
                    }
                    None => parts.push(format!("{}# Blast radius of {p}\n\n_not in the atlas — check the path or rebuild the codebase._", stamp("blast", &p, generated))),
                }
            }
            Ok(Out { content: parts.join("\n\n---\n\n"), summary: format!("blast radius: {total} files depend on {}, {tests} tests reach the set", if paths.len() == 1 { "it".to_string() } else { format!("{} files", paths.len()) }), json: false })
        }
        "references" => {
            let path = need_path(args)?;
            let symbol = s(args.get("symbol"));
            let syms = crate::db_json::query_rows(db, "SELECT id, name FROM code_nodes WHERE origin = 'atlas' AND path = ? AND type != 'file' AND name IS NOT NULL AND (? = '' OR name = ?) ORDER BY id LIMIT 12", &[json!(path), json!(symbol), json!(symbol)]).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
            let mut found: Vec<(String, Vec<(String, String, i64, String)>)> = vec![];
            for row in &syms {
                let (id, name) = (s(row.get("id")), s(row.get("name")));
                let refs = crate::db_json::query_rows(db, "SELECT e.type AS kind, e.from_node AS from_node, n.span_json AS span FROM code_edges e LEFT JOIN code_nodes n ON n.id = e.from_node WHERE e.origin = 'atlas' AND e.to_node = ? AND e.type IN ('calls','uses','renders','dispatches') ORDER BY e.from_node", &[json!(id)]).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
                let list: Vec<(String, String, i64, String)> = refs.iter().map(|r| {
                    let from = s(r.get("from_node")); let (file, func) = from.split_once('#').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((from.clone(), String::new()));
                    let line = serde_json::from_str::<Value>(&s(r.get("span"))).ok().and_then(|v| v.get("start_line").and_then(Value::as_i64)).unwrap_or(0);
                    (file, func, line, s(r.get("kind")))
                }).collect();
                found.push((name, list));
            }
            let importers: Vec<String> = crate::db_json::query_rows(db, "SELECT DISTINCT from_node FROM code_edges WHERE origin = 'atlas' AND type = 'imports' AND to_node = ? ORDER BY from_node", &[json!(path)]).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default().iter().map(|r| s(r.get("from_node"))).collect();
            let (md, places, files) = references_md(&path, &found, &importers, generated);
            Ok(Out { content: md, summary: format!("references: {places} place(s) in {files} file(s) reference {} symbol(s) of it; {} file(s) import it", found.len(), importers.len()), json: false })
        }
        "check" => {
            let path = need_path(args)?;
            let content = match nz(args.get("content")) {
                Some(c) => s(Some(c)),
                None => { if worktree.is_empty() { return Err("no worktree for this pane and no content given".into()); } std::fs::read_to_string(Path::new(worktree).join(&path)).map_err(|e| format!("read {path}: {e}"))? }
            };
            let r = crate::constitution_constitution_check::constitutionCheck(&json!({ "path": path, "content": content, "extra_laws": args.get("extra_laws").cloned().unwrap_or(Value::Null) }), db)?;
            Ok(Out { content: crate::constitution__helpers_review_markdown::check_md(&r), summary: format!("check: {} → {} violations", si(r.get("before").and_then(|b| b.get("violations"))), si(r.get("after").and_then(|a| a.get("violations")))), json: false })
        }
        "ast" => {
            let path = need_path(args)?;
            let abs = Path::new(worktree).join(&path);
            let source_text = std::fs::read_to_string(&abs).map_err(|e| format!("read {path}: {e}"))?;
            let parsed = crate::codebase_ast_parse::astParse(&json!({ "path": path, "source": source_text }), db)?;
            if parsed.is_null() { return Err(format!("no parser profile for {path}")); }
            let symbol = s(args.get("symbol"));
            let lines: Option<(i64, i64)> = nz(args.get("lines")).and_then(|l| { let t = s(Some(l)); let (a, b) = t.split_once('-')?; Some((a.trim().parse().ok()?, b.trim().parse().ok()?)) });
            let all_nodes = parsed.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
            let overlaps = |n: &Value| -> bool {
                let sp = n.get("span").cloned().unwrap_or(Value::Null);
                let (a, b) = (sp.get("start_line").and_then(Value::as_i64).unwrap_or(0), sp.get("end_line").and_then(Value::as_i64).unwrap_or(0));
                match lines { Some((la, lb)) => a <= lb && b >= la, None => true }
            };
            let nodes: Vec<Value> = all_nodes.iter().filter(|n| n.get("type").and_then(Value::as_str) != Some("file")).filter(|n| symbol.is_empty() || n.get("name").and_then(Value::as_str) == Some(symbol.as_str())).filter(|n| overlaps(n)).cloned().collect();
            let partial = !symbol.is_empty() || lines.is_some();
            // the range the partial view covers → the s-expression of that subtree
            let range: Option<(i64, i64)> = if let Some(l) = lines { Some(l) } else if !symbol.is_empty() {
                nodes.iter().filter_map(|n| { let sp = n.get("span")?; Some((sp.get("start_line")?.as_i64()?, sp.get("end_line")?.as_i64()?)) }).next()
            } else { None };
            let mut sexp = String::new();
            let want_sexp = args.get("sexp").and_then(Value::as_bool).unwrap_or(false) || (partial && range.is_some());
            if want_sexp {
                if let Some((la, lb)) = range {
                    let grammar = s(Some(&crate::constitution__helpers_ts_query::grammarForPath(&json!(path))));
                    if !grammar.is_empty() {
                        if let Some((tree, _lang)) = crate::constitution__helpers_ts_query::getTree(&grammar, &source_text) {
                            // byte range of lines la..=lb (1-based, inclusive) → the smallest node covering it
                            let mut offsets: Vec<usize> = vec![0];
                            for (i, b) in source_text.bytes().enumerate() { if b == b'\n' { offsets.push(i + 1); } }
                            let sb = offsets.get((la.max(1) - 1) as usize).copied().unwrap_or(0);
                            let eb = offsets.get(lb.max(la) as usize).map(|o| o.saturating_sub(1)).unwrap_or(source_text.len());
                            if let Some(node) = tree.root_node().descendant_for_byte_range(sb, eb.max(sb)) {
                                sexp = take(&node.to_sexp(), 12_000);
                            }
                        }
                    }
                }
            }
            let mut v = Map::new();
            v.insert("path".into(), json!(path));
            v.insert("lang".into(), parsed.get("lang").cloned().unwrap_or(Value::Null));
            v.insert("partial".into(), json!(partial));
            if let Some((a, b)) = range { v.insert("range".into(), json!(format!("L{a}-L{b}"))); }
            v.insert("has_error".into(), parsed.get("has_error").cloned().unwrap_or(json!(false)));
            v.insert("definitions".into(), json!(nodes.iter().map(|n| {
                let sp = n.get("span").cloned().unwrap_or(Value::Null);
                json!({ "type": n.get("type"), "name": n.get("name"), "lines": format!("L{}-L{}", si(sp.get("start_line")), si(sp.get("end_line"))), "exported": n.get("exported").and_then(Value::as_bool).unwrap_or(false), "complexity": n.get("complexity"), "signature": n.get("signature"), "id": n.get("id") })
            }).collect::<Vec<_>>()));
            let flat_call = |c: &Value| json!({ "caller": c.get("caller"), "callee": c.get("callee"), "line": c.get("span").and_then(|sp| sp.get("start_line")).cloned().unwrap_or(Value::Null), "via": c.get("via") });
            let calls_all: Vec<Value> = parsed.get("calls").and_then(Value::as_array).cloned().unwrap_or_default();
            if !partial {
                v.insert("imports".into(), parsed.get("imports").cloned().unwrap_or(json!([])));
                v.insert("calls".into(), json!(calls_all.iter().map(flat_call).collect::<Vec<_>>()));
            } else {
                let names: Vec<String> = nodes.iter().filter_map(|n| n.get("name").and_then(Value::as_str).map(String::from)).collect();
                v.insert("calls".into(), json!(calls_all.iter().filter(|c| names.iter().any(|nm| c.get("caller").and_then(Value::as_str) == Some(nm))).map(flat_call).collect::<Vec<_>>()));
            }
            let mut md = format!("{}{}", stamp("ast", &path, generated), json_md(&format!("AST of {path}{}", if partial { " (partial)" } else { "" }), &Value::Object(v)));
            if !sexp.is_empty() { md.push_str("\n\n## Syntax tree (s-expression)\n```\n"); md.push_str(&sexp); md.push_str("\n```"); }
            Ok(Out { content: md, summary: format!("AST{}: {} definitions", if partial { " (partial)" } else { "" }, nodes.len()), json: false })
        }
        "symbols" => {
            let q = s(args.get("query"));
            if q.is_empty() { return Err("query is required".into()); }
            let r = crate::codebase_symbol_find::symbolFind(&json!({ "query": q, "limit": arg_i64(args, "limit", 40) }), db)?;
            let n = count(&r, "matches");
            rm(&r, &format!("Symbols matching `{q}`"), format!("symbols: {n} matches for `{q}`"), "")
        }
        "imports" => {
            let path = need_path(args)?;
            let r = crate::system_code_imports::codeImports(&json!({ "path": path }), db)?;
            rm(&r, &format!("Imports of {path}"), "imports resolved through the atlas".into(), &path)
        }
        "subgraph" => {
            let paths: Vec<String> = args.get("paths").and_then(Value::as_array).map(|a| a.iter().map(|p| s(Some(p))).collect()).unwrap_or_default();
            let paths: Vec<String> = if paths.is_empty() { vec![need_path(args)?] } else { paths.iter().map(|p| clean_path(p)).collect::<Result<_, _>>()? };
            let r = crate::codebase_atlas_subgraph::atlasSubgraph(&json!({ "paths": paths, "hops": arg_i64(args, "hops", 1) }), db)?;
            rm(&r, &format!("Subgraph around {}", paths.join(", ")), format!("subgraph: {} nodes, {} edges within {} hop(s)", si(r.get("node_count")), si(r.get("edge_count")), si(r.get("hops"))), &paths[0])
        }
        "trace" => {
            let (from, to) = (s(args.get("from")), s(args.get("to")));
            if from.is_empty() || to.is_empty() { return Err("from and to are required".into()); }
            let r = crate::codebase_atlas_trace_path::atlasTracePath(&json!({ "from": from, "to": to }), db)?;
            rm(&r, &format!("Path from {from} to {to}"), if r.get("found") == Some(&json!(true)) { format!("trace: {} hops", si(r.get("hops"))) } else { format!("trace: {}", s(r.get("reason"))) }, "")
        }
        "neighborhood" => {
            let path = need_path(args)?;
            let r = crate::constitution_constitution_neighborhood::constitutionNeighborhood(&json!({ "path": path, "depth": arg_i64(args, "depth", 2) }), db)?;
            rm(&r, &format!("Neighborhood of {path}"), format!("neighborhood: {} files in {} rings, {} issues", count(&r, "nodes"), si(r.get("depth")), count(&r, "issues")), &path)
        }
        "diagram" => {
            let path = need_path(args)?;
            let r = crate::codebase_file_internal_diagram::fileInternalDiagram(&json!({ "path": path }), db)?;
            rm(&r, &format!("Internal diagram of {path}"), "file diagram".into(), &path)
        }
        "tree" => {
            let path = clean_path(&s(args.get("path")))?;
            let r = crate::system_code_tree::codeTree(&json!({}), db)?;
            let filtered = if path.is_empty() { r } else { filter_tree(&r, &path) };
            rm(&filtered, &format!("File tree{}", if path.is_empty() { String::new() } else { format!(" under {path}") }), "atlas file tree".into(), &path)
        }
        "context" => {
            let paths: Vec<String> = args.get("paths").and_then(Value::as_array).map(|a| a.iter().map(|p| s(Some(p))).collect()).unwrap_or_default();
            let terms: Vec<String> = args.get("terms").and_then(Value::as_array).map(|a| a.iter().map(|p| s(Some(p))).collect()).unwrap_or_default();
            if paths.is_empty() && terms.is_empty() { return Err("paths[] or terms[] required".into()); }
            let r = crate::codebase_codebase_context::codebaseContext(&json!({ "paths": paths, "terms": terms, "hops": arg_i64(args, "hops", 1) }), db)?;
            rm(&r, "Codebase context", "codebase context".into(), "")
        }
        "bundle" => {
            let q = s(args.get("question"));
            if q.is_empty() { return Err("question is required".into()); }
            let r = crate::codebase_smart_context_bundle::smartContextBundle(&json!({ "question": q, "token_budget": arg_i64(args, "token_budget", 20000), "coverage": true }), db)?;
            let md = format!("{}# Context bundle for: {q}\n\nscope {} · {} tokens · dropped {}\n\n{}", stamp("bundle", "", generated), s(r.get("scope")), si(r.get("tokens")), count(&r, "dropped"), s(r.get("bundle_md")));
            Ok(Out { content: md, summary: format!("bundle: scope {}, {} tokens", s(r.get("scope")), si(r.get("tokens"))), json: false })
        }
        "metrics" => {
            let mut r = crate::codebase_atlas_metrics::atlasMetrics(&json!({ "top": arg_i64(args, "top", 20) }), db)?;
            // per-file rows for the whole repo are thousands of entries — keep them only under a path prefix
            let prefix = clean_path(&s(args.get("path")))?;
            if let Some(o) = r.as_object_mut() {
                match o.remove("metrics_by_file") {
                    Some(Value::Object(by)) if !prefix.is_empty() => {
                        let kept: Map<String, Value> = by.into_iter().filter(|(k, _)| k == &prefix || k.starts_with(&format!("{prefix}/"))).take(60).collect();
                        o.insert("metrics_by_file".into(), Value::Object(kept));
                    }
                    _ => { o.insert("metrics_by_file".into(), json!("(omitted — pass `path` to see per-file metrics under a folder)")); }
                }
            }
            rm(&r, "Atlas metrics", "repo metrics".into(), &prefix)
        }
        "smells" => { let r = crate::codebase_atlas_smells::atlasSmells(&json!({}), db)?; rm(&r, "Graph smells", "graph smells".into(), "") }
        "entry_points" => {
            let mut r = crate::codebase_atlas_entry_points::atlasEntryPoints(&json!({}), db)?;
            let prefix = clean_path(&s(args.get("path")))?;
            if let Some(Value::Object(by)) = r.get("by_path").cloned() {
                let kept: Map<String, Value> = by.into_iter().filter(|(k, _)| prefix.is_empty() || k == &prefix || k.starts_with(&format!("{prefix}/"))).take(60).collect();
                r["by_path"] = Value::Object(kept);
            }
            rm(&r, "Entry points", "entry points".into(), &prefix)
        }
        "signals" => {
            let mut inp = Map::new();
            inp.insert("limit".into(), json!(arg_i64(args, "limit", 100)));
            if let Some(c) = nz(args.get("category")) { inp.insert("category".into(), c.clone()); }
            if let Some(k) = nz(args.get("kind")) { inp.insert("kind".into(), k.clone()); }
            let r = crate::codebase_code_signals_read::codeSignalsRead(&Value::Object(inp), db)?;
            rm(&r, "Code signals", "code signals".into(), "")
        }
        "coverage" => {
            let mut inp = Map::new();
            if let Some(p) = nz(args.get("path")) { inp.insert("path".into(), json!(clean_path(&s(Some(p)))?)); }
            let r = crate::codebase_code_coverage::codeCoverage(&Value::Object(inp), db)?;
            rm(&r, "Coverage", format!("coverage: {}", if r.get("total_pct").map_or(true, Value::is_null) { "no report".to_string() } else { format!("{}%", si(r.get("total_pct"))) }), "")
        }
        "report" => { let r = crate::constitution_constitution_report::constitutionReport(&json!({}), db)?; rm(&r, "Constitution report", "latest judge report".into(), "") }
        "atlas_report" => { let r = crate::codebase_atlas_report_latest::atlasReportLatest(&json!({}), db)?; rm(&r, "Atlas report", "latest atlas report".into(), "") }
        "digest" => { let r = crate::codebase_code_digest_latest::codeDigestLatest(&json!({}), db)?; rm(&r, "Code digest", "latest code digest".into(), "") }
        "decorators" => { let path = need_path(args)?; let r = crate::codebase_code_decorate::codeDecorate(&json!({ "path": path }), db)?; rm(&r, &format!("Decorators of {path}"), "decorators".into(), &path) }
        "review_history" => { let path = need_path(args)?; let r = crate::codebase_code_file_review_history::codeFileReviewHistory(&json!({ "path": path, "limit": arg_i64(args, "limit", 12) }), db)?; rm(&r, &format!("Review history of {path}"), "review history".into(), &path) }
        "git" => { let r = crate::codebase_code_git_info::codeGitInfo(&json!({ "limit": arg_i64(args, "limit", 20) }), db); rm(&r, "Git", "git info".into(), "") }
        "file_history" => {
            let path = need_path(args)?;
            let mut r = crate::codebase_code_file_history::codeFileHistory(&json!({ "path": path, "limit": arg_i64(args, "limit", 30) }), db)?;
            // each revision carries the FULL file content at that commit — the agent wants the log, not 30 copies of the file
            if let Some(revs) = r.get_mut("revisions").and_then(Value::as_array_mut) {
                for rev in revs.iter_mut() { if let Some(o) = rev.as_object_mut() { o.remove("content"); } }
            }
            rm(&r, &format!("History of {path}"), format!("file history: {} commits", si(r.get("count"))), &path)
        }
        "authors" => { let r = crate::codebase_code_authors::codeAuthors(&json!({ "top_authors": arg_i64(args, "top_authors", 10), "files_per_author": arg_i64(args, "files_per_author", 8) }), db)?; rm(&r, "Authors", "authors".into(), "") }
        "history" => { let r = crate::codebase_code_history::codeHistory(&json!({ "limit": arg_i64(args, "limit", 40) }), db)?; rm(&r, "Repo history", "repo history".into(), "") }
        "docs" => {
            let path = clean_path(&s(args.get("path")))?;
            let r = crate::codebase_code_docs_build::codeDocsBuild(&json!({ "page": if path.is_empty() { Value::Null } else { json!(path) }, "format": "md" }), db)?;
            let md = s(r.get("markdown"));
            if md.is_empty() { return rm(&r, &format!("Docs page {path}"), "docs page".into(), &path); }
            Ok(Out { content: format!("{}{}", stamp("docs", &path, generated), md), summary: format!("docs page {}", if path.is_empty() { "(root overview)".to_string() } else { path.clone() }), json: false })
        }
        "pack" => {
            let slug = s(args.get("slug"));
            if slug.is_empty() {
                let r = crate::context_context_library::contextLibrary(&json!({ "op": "packs" }), db)?;
                return rm(&r, "Context packs", "packs list".into(), "");
            }
            match crate::context_context_library::pack_body(db, &slug) {
                Some(md) if !md.trim().is_empty() => Ok(Out { content: format!("{}{}", stamp("pack", &slug, generated), md), summary: format!("context pack `{slug}`"), json: false }),
                _ => Err(format!("no context pack `{slug}` — the library lists the ones that exist")),
            }
        }
        "paper" => {
            let slug = s(args.get("slug"));
            if slug.is_empty() {
                let r = crate::constitution_constitution_paper::constitutionPaper(&json!({ "op": "list" }), db)?;
                return rm(&r, "Saved papers", "papers list".into(), "");
            }
            let r = crate::constitution_constitution_paper::constitutionPaper(&json!({ "op": "get", "slug": slug }), db)?;
            let md = s(r.get("markdown"));
            if md.is_empty() { return rm(&r, &format!("Paper {slug}"), "paper".into(), ""); }
            Ok(Out { content: format!("{}{}", stamp("paper", &slug, generated), md), summary: format!("paper `{slug}`: {}", s(r.get("title"))), json: false })
        }
        "source" => {
            let path = need_path(args)?;
            let r = crate::system_code_file::codeFile(&json!({ "path": path }), db)?;
            let content = s(r.get("content"));
            Ok(Out { content: format!("{}# {path}\n\n```\n{}\n```", stamp("source", &path, generated), take(&content, 60_000)), summary: format!("source: {} chars", content.chars().count()), json: false })
        }
        "describe" => { let path = need_path(args)?; let r = crate::codebase_code_describe::codeDescribe(&json!({ "path": path }), db)?; rm(&r, &format!("Description of {path}"), "AI description".into(), &path) }
        "symbol_explain" => { let id = s(args.get("node_id")); if id.is_empty() { return Err("node_id is required".into()); } let r = crate::codebase_atlas_symbol_explain::atlasSymbolExplain(&json!({ "node_id": id }), db)?; rm(&r, &format!("Symbol {id}"), "AI symbol explanation".into(), "") }
        "explain_snippet" => { let path = need_path(args)?; let r = crate::codebase_atlas_explain_snippet::atlasExplainSnippet(&json!({ "path": path, "lines": s(args.get("lines")) }), db)?; rm(&r, &format!("Explain {path} {}", s(args.get("lines"))), "AI snippet explanation".into(), &path) }
        "logic_spec" => { let path = need_path(args)?; let r = crate::codebase_atlas_logic_spec::atlasLogicSpec(&json!({ "file": path, "intent": s(args.get("intent")) }), db)?; rm(&r, &format!("Logic spec of {path}"), "AI logic spec".into(), &path) }
        "architecture" => { let r = crate::codebase_architecture_digest::architectureDigest(&json!({ "max_files": arg_i64(args, "max_files", 60) }), db)?; rm(&r, "Architecture digest", "AI architecture digest".into(), "") }
        "docs_overview" => { let path = need_path(args)?; let r = crate::codebase_code_docs_overview::codeDocsOverview(&json!({ "path": path }), db); rm(&r, &format!("Docs overview of {path}"), "AI docs overview".into(), &path) }
        "rules" => {
            let book = crate::constitution_constitution_law::constitutionLaw(&json!({ "op": "list" }), db)?;
            let rep = act(db, "system.constitution.report", json!({})).unwrap_or(Value::Null);
            let hits: std::collections::HashMap<String, i64> = rep.get("by_law").and_then(Value::as_array).into_iter().flatten().map(|l| (s(l.get("law_key")), l.get("n").and_then(Value::as_i64).unwrap_or(0))).collect();
            let want = s(args.get("status"));
            let exceptions = book.get("exceptions").and_then(Value::as_array).cloned().unwrap_or_default();
            let mut rows: Vec<Value> = book.get("laws").and_then(Value::as_array).into_iter().flatten().filter(|l| want.is_empty() || s(l.get("status")) == want).map(|l| {
                let k = s(l.get("key"));
                json!({ "key": k, "title": l.get("title"), "scope": l.get("scope"), "status": l.get("status"), "on": s(l.get("status")) == "ratified", "detector": l.get("detector"), "config": l.get("config"), "weight": l.get("weight"), "severity": l.get("severity"),
                    "enforcement": l.get("enforcement"), "applies": l.get("applies"), "hits": hits.get(&k).copied().unwrap_or(0), "version": l.get("version"), "created_by": l.get("created_by"), "rule": l.get("rule_md"),
                    "exceptions": exceptions.iter().filter(|e| s(e.get("law_key")) == k).cloned().collect::<Vec<_>>() })
            }).collect();
            rows.sort_by(|a, b| b["hits"].as_i64().cmp(&a["hits"].as_i64()).then(s(a.get("key")).cmp(&s(b.get("key")))));
            let on = rows.iter().filter(|r| r["on"] == json!(true)).count();
            let v = json!({ "counts": book.get("counts"), "rules": rows });
            Ok(Out { content: format!("{}{}", stamp("rules", "", generated), serde_json::to_string_pretty(&v).unwrap_or_default()), summary: format!("{} rules ({} on)", rows.len(), on), json: true })
        }
        "system_diagram" => {
            let depth = arg_i64(args, "depth", 2).clamp(1, 4) as usize;
            let under = { let p = s(args.get("path")).replace('\\', "/"); p.trim_matches('/').to_string() };
            let atlas = load_atlas(db);
            let (boxes, arrows) = system_diagram(&atlas, &under, depth);
            let mut mm = vec!["flowchart LR".to_string()];
            let id = |n: &str| format!("n{}", n.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect::<String>());
            for b in &boxes { mm.push(format!("  {}[\"{} · {}\"]", id(&s(b.get("folder"))), s(b.get("folder")), s(b.get("files")))); }
            for a in &arrows { mm.push(format!("  {} -->|{}| {}", id(&s(a.get("from"))), s(a.get("imports")), id(&s(a.get("to"))))); }
            let v = json!({ "path": under, "depth": depth, "boxes": boxes, "arrows": arrows, "mermaid": mm.join("\n") });
            Ok(Out { content: format!("{}{}", stamp("system_diagram", &under, generated), serde_json::to_string_pretty(&v).unwrap_or_default()), summary: format!("system diagram: {} boxes, {} arrows (depth {depth})", boxes.len(), arrows.len()), json: true })
        }
        "design" => { let r = act(db, "system.codebase.design.get", json!({}))?; rm(&r, "Design system", "design system".into(), "") }
        "design_snapshots" => { let r = act(db, "system.codebase.design.snapshots", json!({}))?; rm(&r, "Design snapshots", "design snapshots".into(), "") }
        "ui_stories" => { let r = act(db, "system.codebase.ui.stories", json!({}))?; rm(&r, "UI stories", "UI stories".into(), "") }
        "data_stories" => {
            let id = s(args.get("id"));
            let r = if id.is_empty() { act(db, "system.codebase.story.list", json!({}))? } else { act(db, "system.codebase.story.get", json!({ "id": id }))? };
            rm(&r, "Data stories", if id.is_empty() { "the saved data stories".into() } else { format!("data story {id}") }, "")
        }
        "architecture_history" => { let r = act(db, "system.codebase.history", json!({}))?; rm(&r, "Architecture over time", "architecture over time".into(), "") }
        "review_movers" => { let r = act(db, "system.codebase.review.history", json!({}))?; rm(&r, "Review history", "review history across snapshots".into(), "") }
        "working_tree" => { let r = act(db, "system.code.git", json!({}))?; rm(&r, "Working tree and git graph", "working tree and git graph".into(), "") }
        "telemetry" => { let r = act(db, "system.codebase.telemetry", json!({}))?; rm(&r, "Live signals", "substrate telemetry".into(), "") }
        "typecheck" => { let r = act(db, "system.codebase.typecheck.status", json!({}))?; rm(&r, "Type-check status", "type-check status".into(), "") }
        "linters" => { let r = act(db, "system.codebase.lint.status", json!({}))?; rm(&r, "Linter status", "linter status".into(), "") }
        "deps_audit" => { let r = act(db, "system.codebase.deps.audit", json!({}))?; rm(&r, "Dependency audit", "dependency audit".into(), "") }
        "scope" => { let r = act(db, "system.codebase.scope.get", json!({}))?; rm(&r, "Atlas scope", "atlas scope".into(), "") }
        other => Err(format!("source `{other}` is listed but has no runner")),
    }
}

/// One engine action, run on this connection (the workspace the caller is about), its result unwrapped. Pure over the connection.
fn act(db: &Connection, action: &str, input: Value) -> Result<Value, String> {
    let r = crate::engine_action_run::skillRunWithContext(&json!({ "action": action, "input": input }), db, &json!({ "actor": "mcp", "source": "mcp" }))?;
    Ok(match r { Value::Object(ref o) if o.len() <= 3 && o.contains_key("result") && o.contains_key("ok") => o["result"].clone(), other => other })
}

/// THE SYSTEM DIAGRAM, DETERMINISTIC: one box per folder at `depth` (counted from `under`), one arrow per ordered pair of boxes with at least one import between them, labelled with the count. Files outside `under` are left out.
/// Boxes are worst-connected last: sorted by folder name so the same atlas is always the same bytes. Pure.
pub fn system_diagram(atlas: &Atlas, under: &str, depth: usize) -> (Vec<Value>, Vec<Value>) {
    let prefix = if under.is_empty() { String::new() } else { format!("{}/", under.trim_end_matches('/')) };
    let rel = |p: &str| -> Option<String> { let p = p.replace('\\', "/"); if prefix.is_empty() { Some(p) } else { p.strip_prefix(prefix.as_str()).map(String::from) } };
    let boxed = |p: &str| -> Option<String> { let r = rel(p)?; let parts: Vec<&str> = r.split('/').collect(); let dirs = &parts[..parts.len().saturating_sub(1)]; Some(if dirs.is_empty() { "(root)".to_string() } else { dirs.iter().take(depth).cloned().collect::<Vec<_>>().join("/") }) };
    let mut files: std::collections::BTreeMap<String, i64> = Default::default();
    for n in &atlas.nodes { if let Some(b) = boxed(&s(n.get("path"))) { *files.entry(b).or_insert(0) += 1; } }
    let mut arrows: std::collections::BTreeMap<(String, String), i64> = Default::default();
    for e in &atlas.edges {
        let (Some(a), Some(b)) = (atlas.path_of.get(&s(e.get("from_node"))), atlas.path_of.get(&s(e.get("to_node")))) else { continue };
        let (Some(a), Some(b)) = (boxed(&s(Some(a))), boxed(&s(Some(b)))) else { continue };
        if a != b { *arrows.entry((a, b)).or_insert(0) += 1; }
    }
    (files.into_iter().map(|(f, n)| json!({ "folder": f, "files": n })).collect(), arrows.into_iter().map(|((a, b), n)| json!({ "from": a, "to": b, "imports": n })).collect())
}

fn si(v: Option<&Value>) -> String { nz(v).and_then(Value::as_f64).map(|f| format!("{}", f.round() as i64)).unwrap_or_else(|| "—".into()) }

fn weakest_axis(axes: &Value) -> String {
    let mut best: Option<(&str, f64)> = None;
    for k in ["compliance", "defects", "tests", "structure", "coupling", "cohesion", "change"] {
        if let Some(sc) = nz(axes.get(k)).and_then(|a| a.get("score")).and_then(Value::as_f64) {
            if best.map_or(true, |(_, b)| sc < b) { best = Some((k, sc)); }
        }
    }
    best.map(|(k, sc)| format!("{k} {}", sc.round() as i64)).unwrap_or_default()
}

/// Keep only the tree entries under `prefix` (the tree is `{tree:[{path,…,children?}]}` or a flat list).
fn filter_tree(v: &Value, prefix: &str) -> Value {
    fn keep(n: &Value, prefix: &str) -> Option<Value> {
        let p = n.get("path").and_then(Value::as_str).unwrap_or("");
        // an ancestor (the root has path ""), the folder itself, or something under it
        let inside = p.is_empty() || p == prefix || p.starts_with(&format!("{prefix}/")) || prefix.starts_with(&format!("{p}/"));
        if !inside { return None; }
        let mut m = n.as_object().cloned().unwrap_or_default();
        if let Some(children) = n.get("children").and_then(Value::as_array) {
            m.insert("children".into(), json!(children.iter().filter_map(|c| keep(c, prefix)).collect::<Vec<_>>()));
        }
        Some(Value::Object(m))
    }
    match v {
        Value::Array(a) => json!(a.iter().filter_map(|n| keep(n, prefix)).collect::<Vec<_>>()),
        Value::Object(o) => {
            let mut m = o.clone();
            for k in ["tree", "entries", "nodes", "children"] {
                match o.get(k) {
                    Some(Value::Array(a)) => { m.insert(k.into(), json!(a.iter().filter_map(|n| keep(n, prefix)).collect::<Vec<_>>())); }
                    Some(node @ Value::Object(_)) => { m.insert(k.into(), keep(node, prefix).unwrap_or(Value::Null)); }
                    _ => {}
                }
            }
            Value::Object(m)
        }
        other => other.clone(),
    }
}

pub struct Atlas { pub nodes: Vec<Value>, pub edges: Vec<Value>, pub id_of: Map<String, Value>, pub path_of: Map<String, Value> }

pub fn load_atlas(db: &Connection) -> Atlas {
    let nodes = crate::db_json::query_rows(db, "SELECT id, path FROM code_nodes WHERE origin='atlas' AND type='file'", &[]).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let edges = crate::db_json::query_rows(db, "SELECT from_node, to_node, type, kind FROM code_edges WHERE origin='atlas' AND type='imports'", &[]).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let mut id_of = Map::new();
    let mut path_of = Map::new();
    for n in &nodes {
        let p = s(n.get("path")).replace('\\', "/");
        let id = n.get("id").cloned().unwrap_or(Value::Null);
        path_of.insert(s(Some(&id)), json!(p.clone()));
        id_of.insert(p, id);
    }
    Atlas { nodes, edges, id_of, path_of }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_lists_who_uses_a_symbol_by_file_and_who_imports_the_file() {
        let found = vec![("getName".to_string(), vec![
            ("packages/a/A.js".to_string(), "render".to_string(), 12, "calls".to_string()),
            ("packages/a/A.js".to_string(), "update".to_string(), 40, "calls".to_string()),
            ("packages/b/B.js".to_string(), String::new(), 0, "uses".to_string())]),
            ("unused".to_string(), vec![])];
        let (md, places, files) = references_md("packages/shared/getName.js", &found, &["packages/a/A.js".to_string(), "packages/b/B.js".to_string()], "t");
        assert_eq!((places, files), (3, 2));
        assert!(md.contains("## `getName` - 3 place(s)") && md.contains("- `packages/a/A.js`: render (calls, from line 12); update (calls, from line 40)"), "{md}");
        assert!(md.contains("- `packages/b/B.js`: (module level) (uses)"), "{md}");
        assert!(md.contains("(no referencing function found in the index)") && md.contains("Files that import packages/shared/getName.js (2)") && md.contains("lower bound"), "{md}");
        assert!(source("references").is_some_and(|s| s.packable && !s.ai));
    }

    #[test]
    fn the_system_diagram_is_boxes_by_folder_and_arrows_by_import_count_in_a_stable_order() {
        let mk = |paths: &[&str]| { let nodes: Vec<Value> = paths.iter().enumerate().map(|(i, p)| json!({ "id": i + 1, "path": p })).collect(); let mut path_of = Map::new(); for n in &nodes { path_of.insert(s(n.get("id")), n["path"].clone()); } (nodes, path_of) };
        let (nodes, path_of) = mk(&["src/a/x.ts", "src/a/y.ts", "src/b/z.ts", "lib/q.ts", "main.ts"]);
        let edge = |f: i64, t: i64| json!({ "from_node": f, "to_node": t, "type": "imports" });
        let atlas = Atlas { nodes, edges: vec![edge(1, 3), edge(2, 3), edge(3, 4), edge(1, 2)], id_of: Map::new(), path_of };
        let (boxes, arrows) = system_diagram(&atlas, "", 2);
        assert_eq!(boxes, vec![json!({ "folder": "(root)", "files": 1 }), json!({ "folder": "lib", "files": 1 }), json!({ "folder": "src/a", "files": 2 }), json!({ "folder": "src/b", "files": 1 })]);
        assert_eq!(arrows, vec![json!({ "from": "src/a", "to": "src/b", "imports": 2 }), json!({ "from": "src/b", "to": "lib", "imports": 1 })], "an import inside a box is not an arrow; the count is how many");
        let (b1, a1) = system_diagram(&atlas, "", 1);
        assert_eq!(b1.iter().map(|b| s(b.get("folder"))).collect::<Vec<_>>(), vec!["(root)", "lib", "src"]); assert_eq!(a1, vec![json!({ "from": "src", "to": "lib", "imports": 1 })]);
        let (inside, _) = system_diagram(&atlas, "src", 1);
        assert_eq!(inside.iter().map(|b| s(b.get("folder"))).collect::<Vec<_>>(), vec!["a", "b"], "drawn inside src/: boxes are its folders");
    }

    #[test]
    fn catalog_is_consistent() {
        let mut keys: Vec<&str> = CATALOG.iter().map(|s| s.key).collect();
        let n = keys.len();
        keys.sort(); keys.dedup();
        assert_eq!(keys.len(), n, "keys unique");
        assert!(CATALOG.iter().filter(|s| s.ai).all(|s| !s.packable), "AI sources are never baked by default");
        for k in ["laws", "file", "folder", "graph", "tests", "blast", "ast", "git", "file_history", "authors", "paper", "docs", "rules", "system_diagram", "design", "data_stories", "architecture_history", "working_tree", "telemetry"] { assert!(source(k).is_some(), "{k}"); }
        let md = catalog_md();
        assert!(md.contains("- **AST** `ast`"));
        assert!(md.contains("`describe` · AI call · live only"));
        assert_eq!(catalog_json().as_array().unwrap().len(), CATALOG.len());
    }

    #[test]
    fn json_md_is_deterministic_and_readable() {
        let v = json!({ "count": 2, "note": null, "tags": ["a", "b"], "rows": [ { "sha": "abc", "author": "x", "subject": "one|two" }, { "sha": "def", "author": "y" } ], "nested": { "k": "v" } });
        let md = json_md("T", &v);
        assert_eq!(md, json_md("T", &v));
        assert!(md.contains("- count: 2\n- note: —\n- tags: a, b"), "{md}");
        assert!(md.contains("## rows\n| sha | author | subject |\n|---|---|---|\n| abc | x | one/two |\n| def | y | — |"), "{md}");
        assert!(scalar(&json!({ "a": 1, "b": "x" })) == "a=1 b=x");
        assert!(md.contains("## nested\n- k: v"));
    }

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS laws; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS knowledge").unwrap();
        let _ = crate::constitution__helpers_schema::ensureConstitutionSchema(&c);
        let _ = crate::codebase_code_schema::ensureCodeSchema(&c, &json!({}));
        c
    }

    #[test]
    fn ast_full_and_partial_with_sexp() {
        let c = conn();
        let wt = std::env::temp_dir().join(format!("antiloki-catalog-ast-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&wt);
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(wt.join("src/a.ts"), "import { x } from './x';\nexport function alpha(n: number) {\n  if (n > 1) { return n; }\n  return 0;\n}\nfunction beta() { return alpha(2); }\n").unwrap();
        let full = run(&c, wt.to_str().unwrap(), "ast", &json!({ "path": "src/a.ts" }), "g").unwrap();
        assert!(full.content.contains("| alpha |") || full.content.contains("alpha"), "{}", full.content);
        assert!(full.summary.starts_with("AST: 2 definitions"), "{}", full.summary);
        assert!(full.content.contains("## imports"));
        let part = run(&c, wt.to_str().unwrap(), "ast", &json!({ "path": "src/a.ts", "symbol": "alpha" }), "g").unwrap();
        assert_eq!(part.summary, "AST (partial): 1 definitions");
        assert!(part.content.contains("- range: L2-L5"), "{}", part.content);
        assert!(part.content.contains("## Syntax tree (s-expression)\n```\n(export_statement declaration: (function_declaration"), "{}", part.content);
        assert!(!part.content.contains("beta"));
        let lines = run(&c, wt.to_str().unwrap(), "ast", &json!({ "path": "src/a.ts", "lines": "6-6" }), "g").unwrap();
        assert_eq!(lines.summary, "AST (partial): 1 definitions");
        assert!(lines.content.contains("beta"));
        assert!(run(&c, wt.to_str().unwrap(), "ast", &json!({ "path": "../etc/passwd" }), "g").is_err());
        assert!(run(&c, wt.to_str().unwrap(), "nope", &json!({}), "g").is_err());
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn tree_filter_keeps_ancestors_and_the_folder_only() {
        let tree = json!({ "root": "/r", "tree": { "name": "", "path": "", "type": "dir", "children": [
            { "name": "src", "path": "src", "type": "dir", "children": [ { "name": "ui", "path": "src/ui", "type": "dir", "children": [ { "name": "A.tsx", "path": "src/ui/A.tsx", "type": "file" } ] }, { "name": "lib", "path": "src/lib", "type": "dir", "children": [] } ] },
            { "name": "README.md", "path": "README.md", "type": "file" } ] } });
        let f = filter_tree(&tree, "src/ui");
        let top = f["tree"]["children"].as_array().unwrap();
        assert_eq!(top.len(), 1, "README dropped: {f}");
        let src = top[0]["children"].as_array().unwrap();
        assert_eq!(src.len(), 1, "src/lib dropped");
        assert_eq!(src[0]["children"][0]["path"], json!("src/ui/A.tsx"));
    }

    #[test]
    fn clean_path_rejects_escapes() {
        assert_eq!(clean_path("./src/a.ts/").unwrap(), "src/a.ts");
        assert!(clean_path("../x").is_err());
        assert!(clean_path("/abs").is_err());
        assert!(clean_path("C:\\x").is_err());
    }
}
