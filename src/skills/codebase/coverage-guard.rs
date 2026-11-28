//! coverage-guard.rs — says WHERE THE INTEL STOPS, before an agent trusts it. NO JS TWIN.
//!
//! WHY (2026-10-01, the X-ray A/B experiments): an agent told to trust antiloki's intel inherits its blind spots. The
//! operator's index had `crates/` excluded, so "who imports / who calls this" could not list the Rust port of the same
//! message, and the "intel only" agent — doing exactly what the pack told it — missed the second implementation and
//! reported the task done. Nothing in the pack said "this index does not look in crates/". The pack read as complete
//! because it never said otherwise: an absence in an answer is indistinguishable from "nothing there" unless the answer
//! states what it could not see.
//!
//! WHAT IT DOES, deterministically and without a model:
//!   1. takes the ASK (identifiers, quoted messages, file names — `area_terms`) and finds the files that mention it with a
//!      plain `git grep` (tracked and untracked, ignore rules honoured) plus the files the ask names outright;
//!   2. checks each of them against the index — excluded by the scope setting or a never-indexed directory, a language the
//!      parser does not read, never ingested (added later, or over the size limit), parse failed / skipped / recovered
//!      with errors, edited since the index was built (content hash) — `classify`;
//!   3. adds the index's standing blind spots for the whole repo (tracked source files with no node, grouped by folder),
//!      because an ask with no distinctive words still deserves the warning;
//!   4. writes the result in plain words — `NOT INDEXED: crates/core/src/extract.rs — excluded from the index by the scope
//!      setting `crates/**` — search it yourself` — or, only when it checked something and found nothing, a sentence that
//!      says what was checked and what was not.
//!
//! IT NEVER CERTIFIES COMPLETENESS. "All N files that mention these words are indexed" is a statement about a text search;
//! a file that never spells the words is not checked, and the sentence says so. `notice_md` only ever says less than it
//! knows.
//!
//! WHERE IT IS HANDED OUT: `INDEX.md` of every baked context pack (`cockpit-pane-context-bake`), the PANE BRIEF line that
//! points at it, and the `smart-context-bundle` the chat and the MCP tools build for a question.
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

pub const MAX_TERMS: usize = 8;
/// the most files one check reads and hashes
pub const MAX_AREA_FILES: usize = 400;
/// how many gaps the notice lists before it counts the rest
pub const SHOWN: usize = 15;
const MAX_HASH_BYTES: u64 = 1_048_576;

/// A file the index does not (fully) cover, and why — in words an agent can act on.
#[derive(Clone, Debug, PartialEq)]
pub struct Gap {
    pub path: String,
    /// `not-indexed` · `not-parsed` · `changed` · `degraded`
    pub kind: &'static str,
    pub why: String,
}

/// What the index holds, read once.
#[derive(Clone, Debug, Default)]
pub struct IndexView {
    /// path -> content hash at index time, for every file node
    pub nodes: HashMap<String, Option<String>>,
    /// path -> (status, detail) for files the parser skipped, recovered on or failed
    pub health: HashMap<String, (String, String)>,
    pub exclude: Vec<String>,
    pub include: Vec<String>,
}

pub fn load_index(db: &Connection, origin: &str) -> IndexView {
    let mut v = IndexView::default();
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT path, content_hash FROM code_nodes WHERE origin = ? AND type = 'file'", &[json!(origin)]) {
        for r in rows.as_array().cloned().unwrap_or_default() {
            if let Some(p) = r.get("path").and_then(Value::as_str) { v.nodes.insert(p.to_string(), r.get("content_hash").and_then(Value::as_str).map(String::from)); }
        }
    }
    if let Ok(rows) = crate::db_json::query_rows(db, "SELECT path, status, detail FROM code_parse_health WHERE origin = ?", &[json!(origin)]) {
        for r in rows.as_array().cloned().unwrap_or_default() {
            if let Some(p) = r.get("path").and_then(Value::as_str) {
                v.health.insert(p.to_string(), (r.get("status").and_then(Value::as_str).unwrap_or("").to_string(), r.get("detail").and_then(Value::as_str).unwrap_or("").to_string()));
            }
        }
    }
    let scope = crate::codebase_code_scope_get::loadScope(db, &json!(origin));
    let list = |k: &str| -> Vec<String> { scope.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
    v.exclude = list("exclude");
    v.include = list("include");
    v
}

/// Extensions that make a quoted word a FILE NAME rather than a member access (`Foo.bar`).
const FILE_EXTS: &[&str] = &["rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go", "java", "kt", "cs", "rb", "php", "swift", "json", "toml", "yaml", "yml", "md", "css", "html", "c", "h", "cpp", "hpp", "sh", "lua", "ex", "exs", "scala"];

/// Words too common to name an area on their own.
const STOP: &[&str] = &["function", "functions", "message", "messages", "string", "should", "return", "returns", "import", "imports", "export", "exports", "default", "because",
    "whether", "without", "different", "something", "anything", "everything", "property", "properties", "variable", "variables", "implement", "implementation", "currently",
    "instead", "similar", "following", "existing", "another", "through", "between", "before", "always", "please", "change", "update", "support", "handle", "handles"];

fn is_distinctive_ident(w: &str) -> bool {
    if w.len() < 6 || w.len() > 60 || STOP.contains(&w.to_ascii_lowercase().as_str()) { return false; }
    let has_under = w.contains('_');
    let has_digit = w.chars().any(|c| c.is_ascii_digit());
    // camelCase / PascalCase: a lower-case letter directly followed by an upper-case one
    let camel = w.as_bytes().windows(2).any(|p| p[0].is_ascii_lowercase() && p[1].is_ascii_uppercase());
    // SCREAMING_CASE with an underscore is covered by `has_under`; a bare ALLCAPS word is an acronym, too common
    (has_under || camel || has_digit) && w.chars().next().map_or(false, |c| c.is_ascii_alphabetic() || c == '_')
}

/// The terms that name the area of an ask: backtick-quoted spans, double-quoted messages (6-80 chars) and distinctive
/// identifiers (snake_case, camelCase, with a digit), in the order they first appear, quoted things first, at most
/// `MAX_TERMS`. A plain lower-case word is never a term: searching for "update" finds the repo. Pure — tested.
pub fn area_terms(ask: &str) -> Vec<String> {
    let mut quoted: Vec<String> = Vec::new();
    let mut idents: Vec<String> = Vec::new();
    let mut push = |v: &mut Vec<String>, t: &str| { let t = t.trim(); if !t.is_empty() && !v.iter().any(|x| x == t) { v.push(t.to_string()); } };
    let b: Vec<char> = ask.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == '`' || c == '"' {
            if let Some(len) = b[i + 1..].iter().position(|x| *x == c) {
                let span: String = b[i + 1..i + 1 + len].iter().collect();
                let n = span.chars().count();
                let one_line = !span.contains('\n');
                // a span that is a PATH is a named file, not a term (`named_files` takes it)
                let pathy = !span.contains(' ') && (span.contains('/') || Path::new(&span).extension().map_or(false, |e| FILE_EXTS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str())));
                if one_line && !pathy && (if c == '`' { n >= 3 && n <= 80 } else { n >= 6 && n <= 80 }) { push(&mut quoted, &span); }
                i += len + 2;
                continue;
            }
        }
        i += 1;
    }
    let mut word = String::new();
    let flush = |w: &mut String, out: &mut Vec<String>, push: &mut dyn FnMut(&mut Vec<String>, &str)| { if is_distinctive_ident(w) { push(out, w); } w.clear(); };
    for ch in ask.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_alphanumeric() || ch == '_' { word.push(ch); } else { flush(&mut word, &mut idents, &mut push); }
    }
    let mut out: Vec<String> = Vec::new();
    for t in quoted.into_iter().chain(idents) {
        // a distinctive identifier that sits inside a quoted span is already covered by that span
        if !out.iter().any(|x| x == &t) { out.push(t); }
        if out.len() == MAX_TERMS { break; }
    }
    out
}

/// Paths the ask names outright (`src/cart.ts`, `crates/core/src/extract.rs`): tokens with a slash or a short extension that
/// `exists` confirms. Pure over the predicate — tested.
pub fn named_files(ask: &str, exists: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in ask.split(|c: char| c.is_whitespace() || matches!(c, '`' | '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '<' | '>')) {
        let t = raw.trim_matches(|c: char| matches!(c, '.' | ':' | '!' | '?')).trim_start_matches("./");
        let looks = t.len() >= 3 && t.len() <= 200 && (t.contains('/') || Path::new(t).extension().map_or(false, |e| FILE_EXTS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str())));
        if looks && !t.contains("://") && !t.starts_with('/') && exists(t) && !out.iter().any(|x| x == t) { out.push(t.to_string()); }
    }
    out
}

/// Source languages the parser does not read — said as a gap so a Vue component or a Dart port is not silently absent.
const UNSUPPORTED_SOURCE: &[&str] = &["vue", "svelte", "dart", "zig", "hs", "ml", "clj", "erl", "jl", "sql", "proto", "graphql", "tf", "nim", "cr", "fs", "groovy", "lisp", "pl", "r"];

fn ext_of(name: &str) -> String { Path::new(name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default() }

/// Why the index leaves a path out by configuration, if it does: a never-indexed directory, the scope's exclude globs, or an
/// include list the path is outside of. Pure — tested.
pub fn excluded_by(path: &str, idx: &IndexView) -> Option<String> {
    let norm = path.replace('\\', "/");
    let segs: Vec<&str> = norm.split('/').collect();
    for d in &segs[..segs.len().saturating_sub(1)] {
        if crate::codebase_atlas_ingest::is_skipped_dir(d) { return Some(format!("the directory `{d}/` is never indexed")); }
    }
    for g in &idx.exclude {
        if crate::codebase_atlas_ingest::matchGlob(&norm, &[g.clone()]) { return Some(format!("excluded from the index by the scope setting `{g}`")); }
    }
    if !idx.include.is_empty() && !crate::codebase_atlas_ingest::matchGlob(&norm, &idx.include) {
        return Some(format!("outside the indexed scope (the scope only includes {})", idx.include.iter().map(|g| format!("`{g}`")).collect::<Vec<_>>().join(", ")));
    }
    None
}

/// Is `path` covered by the index? `None` when it is (or is not source at all), else the gap. `current_hash` is the hash of
/// the file on disk now, when the caller read it. Pure — tested.
pub fn classify(path: &str, idx: &IndexView, current_hash: Option<&str>) -> Option<Gap> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = ext_of(name);
    let supported = crate::codebase_ast_parse::adapterFor(&json!(name)) != Value::Null;
    if !supported {
        return UNSUPPORTED_SOURCE.contains(&ext.as_str()).then(|| Gap { path: path.into(), kind: "not-indexed", why: format!("`.{ext}` is a language the index does not read") });
    }
    if let Some(why) = excluded_by(path, idx) { return Some(Gap { path: path.into(), kind: "not-indexed", why }); }
    let Some(stored) = idx.nodes.get(path) else {
        return Some(Gap { path: path.into(), kind: "not-indexed", why: "not in the index - added after it was built, or over the size limit".into() });
    };
    if let Some((status, detail)) = idx.health.get(path) {
        let detail: String = detail.chars().take(140).collect();
        match status.as_str() {
            "fail" => return Some(Gap { path: path.into(), kind: "not-parsed", why: format!("the parser could not read it ({detail}) - it has no symbols, imports or importers in the index") }),
            "skipped" => return Some(Gap { path: path.into(), kind: "not-parsed", why: format!("the parser skipped it ({detail}) - it has no symbols, imports or importers in the index") }),
            "degraded" => return Some(Gap { path: path.into(), kind: "degraded", why: format!("parsed with errors ({detail}) - its symbols may be incomplete") }),
            _ => {}
        }
    }
    match (stored.as_deref(), current_hash) {
        (Some(was), Some(now)) if was != now => Some(Gap { path: path.into(), kind: "changed", why: "edited after the index was built - its symbols, imports and importers may be out of date".into() }),
        _ => None,
    }
}

/// Tracked files in a language the parser does not read (`UNSUPPORTED_SOURCE`), counted per extension, most first. Pure - tested.
pub fn unread_languages(tracked: &[String]) -> Vec<(String, usize)> {
    let mut by: BTreeMap<String, usize> = BTreeMap::new();
    for p in tracked {
        let name = p.rsplit('/').next().unwrap_or(p);
        let ext = ext_of(name);
        if crate::codebase_ast_parse::adapterFor(&json!(name)) == Value::Null && UNSUPPORTED_SOURCE.contains(&ext.as_str()) { *by.entry(ext).or_insert(0) += 1; }
    }
    let mut v: Vec<(String, usize)> = by.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

/// Tracked source files the index has no node for, grouped by their first folder (`.` for the root), most first. The reason a
/// folder is missing rides along when the scope explains it. Pure over its arguments — tested.
pub fn blind_spots(tracked: &[String], idx: &IndexView) -> Vec<(String, usize, String)> {
    let mut by: BTreeMap<String, (usize, String)> = BTreeMap::new();
    for p in tracked {
        let name = p.rsplit('/').next().unwrap_or(p);
        let supported = crate::codebase_ast_parse::adapterFor(&json!(name)) != Value::Null;
        if !supported || idx.nodes.contains_key(p.as_str()) { continue; }
        let top = p.split('/').next().filter(|_| p.contains('/')).unwrap_or(".").to_string();
        let e = by.entry(top).or_insert((0, String::new()));
        e.0 += 1;
        if e.1.is_empty() { e.1 = excluded_by(p, idx).unwrap_or_else(|| "not in the index".into()); }
    }
    let mut v: Vec<(String, usize, String)> = by.into_iter().map(|(k, (n, why))| (k, n, why)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub terms: Vec<String>,
    /// the files the check looked at (those that mention the terms + the ones the ask names)
    pub checked: usize,
    pub gaps: Vec<Gap>,
    /// the text search ran (a git work tree); false = only the named files could be checked
    pub searched: bool,
    pub blind: Vec<(String, usize, String)>,
    pub index_files: usize,
    /// files the parser could not read or skipped, repo-wide (they have a bare file node and nothing else)
    pub unparsed: usize,
    /// `git ls-files` answered: the repo-wide lines below are a count, not an absence of one (a folder that is not a git work tree lists nothing, and nothing listed is not "nothing missing")
    pub listed: bool,
    /// tracked files in a language the parser does not read (`.vue`, `.sql`, `.dart`...), by extension, most first: they can never have a node, so `blind` (which skips them) never counts them
    pub unread: Vec<(String, usize)>,
}

fn git_lines(root: &Path, args: &[&str]) -> Option<Vec<String>> {
    let o = crate::proc::cmd("git").args(args).current_dir(root).output().ok()?;
    // exit 1 from `git grep` is "no match", which is an answer
    if !o.status.success() && o.status.code() != Some(1) { return None; }
    Some(String::from_utf8_lossy(&o.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
}

fn is_our_own(p: &str) -> bool {
    p.starts_with(".antiloki/") || p.starts_with(".claude/") || p.starts_with(".agents/") || p.starts_with(".cursor/") || p.starts_with(".gemini/")
        || matches!(p, "CLAUDE.md" | "AGENTS.md" | "GEMINI.md" | "opencode.json")
}

/// Run the check for `ask` in the work tree `root` against the index in `db`. `extra_files` are paths the caller already
/// knows are in play (a pack's selected files). Never fails: a root that is not a git tree is searched by named files only.
pub fn check(db: &Connection, root: &Path, origin: &str, ask: &str, extra_files: &[String]) -> Report {
    let idx = load_index(db, origin);
    let terms = area_terms(ask);
    let mut files: Vec<String> = named_files(ask, |t| root.join(t).is_file());
    for f in extra_files { if !files.contains(f) { files.push(f.clone()); } }
    let mut searched = false;
    for t in &terms {
        if let Some(hits) = git_lines(root, &["grep", "--untracked", "-l", "-I", "-F", "-e", t, "--", "."]) {
            searched = true;
            for h in hits.into_iter().take(200) { if !is_our_own(&h) && !files.contains(&h) { files.push(h); } }
        }
        if files.len() >= MAX_AREA_FILES { break; }
    }
    files.truncate(MAX_AREA_FILES);
    let mut gaps: Vec<Gap> = Vec::new();
    let mut checked = 0usize;
    for f in &files {
        let name = f.rsplit('/').next().unwrap_or(f);
        let source_like = crate::codebase_ast_parse::adapterFor(&json!(name)) != Value::Null || UNSUPPORTED_SOURCE.contains(&ext_of(name).as_str());
        if !source_like { continue; }
        checked += 1;
        let full = root.join(f);
        let hash = if idx.nodes.get(f.as_str()).map_or(false, |h| h.is_some()) && full.metadata().map_or(false, |m| m.len() <= MAX_HASH_BYTES) {
            std::fs::read_to_string(&full).ok().map(|s| crate::codebase_atlas_ingest::hashSource(&s))
        } else { None };
        if let Some(g) = classify(f, &idx, hash.as_deref()) { gaps.push(g); }
    }
    let rank = |k: &str| match k { "not-indexed" => 0, "not-parsed" => 1, "changed" => 2, _ => 3 };
    gaps.sort_by(|a, b| rank(a.kind).cmp(&rank(b.kind)).then(a.path.cmp(&b.path)));
    let tracked = git_lines(root, &["ls-files", "--cached", "--others", "--exclude-standard"]).map(|l| l.into_iter().filter(|p| !is_our_own(p)).collect::<Vec<String>>());
    let listed = tracked.is_some();
    let (blind, unread) = tracked.map(|l| (blind_spots(&l, &idx), unread_languages(&l))).unwrap_or_default();
    let unparsed = idx.health.values().filter(|(st, _)| st == "fail" || st == "skipped").count();
    Report { terms, checked, gaps, searched, blind, index_files: idx.nodes.len(), unparsed, listed, unread }
}

fn label(kind: &str) -> &'static str {
    match kind { "not-indexed" => "NOT INDEXED", "not-parsed" => "NOT PARSED", "changed" => "CHANGED SINCE THE INDEX", _ => "PARSED WITH ERRORS" }
}

/// The notice, in plain words, for a pack's `INDEX.md` or a bundle. Empty only when there is nothing honest to say (no area
/// words, no named files and no standing blind spots would still print the last line). Pure — tested.
pub fn notice_md(r: &Report) -> String {
    let mut out: Vec<String> = vec!["## Coverage of this intel - read before you trust it".into(), String::new()];
    let quoted = r.terms.iter().take(5).map(|t| format!("`{t}`")).collect::<Vec<_>>().join(", ");
    if r.terms.is_empty() && r.checked == 0 {
        out.push("No distinctive words or file names in the ask to check, so this pack's reach into the area could not be tested.".into());
    } else if r.gaps.is_empty() {
        out.push(format!("Checked {} source file(s) {}: every one is in the index, parsed, and unchanged since it was built. That is a text search - a file that never spells {} is not checked.",
            r.checked, if r.searched { format!("that mention {quoted} or are named in the ask") } else { "named in the ask (this folder is not a git work tree, so nothing could be searched)".into() }, if r.terms.is_empty() { "the ask's words".to_string() } else { "those words".to_string() }));
    } else {
        out.push(format!("The index does NOT fully cover what this ask touches. {} of {} source file(s) {}:", r.gaps.len(), r.checked,
            if r.searched { format!("that mention {quoted} or are named in the ask") } else { "named in the ask".into() }));
        out.push(String::new());
        for g in r.gaps.iter().take(SHOWN) { out.push(format!("- {}: `{}` - {}", label(g.kind), g.path, g.why)); }
        if r.gaps.len() > SHOWN { out.push(format!("- ...and {} more", r.gaps.len() - SHOWN)); }
        out.push(String::new());
        let invisible = r.gaps.iter().any(|g| matches!(g.kind, "not-indexed" | "not-parsed"));
        out.push(if invisible { "Search those files yourself (grep, read). The symbols, `imported by`, blast-radius and test answers in this pack do NOT include them - a caller, a port or a second implementation there will not be listed anywhere in it.".into() }
                 else { "Re-read those files before relying on the symbols, imports and importers this pack lists for them.".into() });
    }
    let unread_n: usize = r.unread.iter().map(|u| u.1).sum();
    if !r.blind.is_empty() || r.unparsed > 0 || unread_n > 0 {
        out.push(String::new());
        out.push("Standing blind spots of the index, repo-wide:".into());
        for (dir, n, why) in r.blind.iter().take(6) { out.push(format!("- `{dir}{}` - {n} tracked source file(s) with no entry in the index: {why}", if dir == "." { "" } else { "/" })); }
        if r.blind.len() > 6 { out.push(format!("- ...and {} more folder(s)", r.blind.len() - 6)); }
        if r.unparsed > 0 { out.push(format!("- {} file(s) the parser could not read or skipped: they are in the index as bare files, so no symbol, import or importer answer in this pack includes them", r.unparsed)); }
        // a language the parser does not read can never have a node, so it is no folder's "missing entry" - but it is as invisible to every answer in the pack
        if unread_n > 0 { out.push(format!("- {unread_n} tracked file(s) in a language the index does not read ({}): they are in no symbol, import or importer answer in this pack", r.unread.iter().take(6).map(|(e, n)| format!("{n} `.{e}`")).collect::<Vec<_>>().join(", "))); }
    } else if !r.listed {
        // THE LIST FAILED (not a git work tree): nothing listed is not "nothing missing"
        out.push(String::new());
        out.push("The repo-wide check was not run: this folder's tracked files could not be listed (it is not a git work tree), so files the index lacks outside the ones above are unknown.".into());
    } else if r.index_files > 0 {
        // the least that is true: an entry exists for every tracked file whose extension the parser reads. Not that the entry is current (the hash is only compared for the files above), not that a language it has no word for is absent
        out.push(String::new());
        out.push("Every tracked file with an extension the parser reads has an entry in the index. That does not say the entries are current for files this ask does not touch, or that the repo holds no language the index cannot read.".into());
    }
    out.join("\n")
}

/// One line for the PANE BRIEF: how many gaps, and where the detail is. Empty when there is nothing to warn about.
pub fn brief_line(r: &Report) -> String {
    let blind: usize = r.blind.iter().map(|b| b.1).sum();
    let unread: usize = r.unread.iter().map(|u| u.1).sum();
    if r.gaps.is_empty() && blind == 0 && r.unparsed == 0 && unread == 0 { return String::new(); }
    let mut parts: Vec<String> = Vec::new();
    if !r.gaps.is_empty() { parts.push(format!("{} file(s) this ask touches are NOT fully covered by the index (first: {})", r.gaps.len(), r.gaps[0].path)); }
    if blind > 0 { parts.push(format!("{blind} tracked source file(s) in {} folder(s) have no index entry at all", r.blind.len())); }
    if r.unparsed > 0 { parts.push(format!("{} file(s) the parser could not read", r.unparsed)); }
    if unread > 0 { parts.push(format!("{unread} tracked file(s) in a language the index does not read")); }
    format!("COVERAGE WARNING: {} - search them yourself; see the \"Coverage\" section of .antiloki/context/INDEX.md before trusting `imported by` answers.", parts.join("; "))
}

pub fn report_json(r: &Report) -> Value {
    json!({
        "terms": r.terms, "checked": r.checked, "searched": r.searched, "listed": r.listed, "index_files": r.index_files, "unparsed": r.unparsed,
        "unread_languages": r.unread.iter().map(|(e, n)| json!({ "ext": e, "files": n })).collect::<Vec<_>>(),
        "gaps": r.gaps.iter().map(|g| json!({ "path": g.path, "kind": g.kind, "why": g.why })).collect::<Vec<_>>(),
        "blind_spots": r.blind.iter().map(|(d, n, w)| json!({ "dir": d, "files": n, "why": w })).collect::<Vec<_>>(),
        "notice_md": notice_md(r), "brief_line": brief_line(r),
    })
}

/// De-duplicated paths, order kept — for callers that merge several sources of "files in play".
pub fn dedup(paths: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    paths.into_iter().filter(|p| seen.insert(p.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn idx(nodes: &[(&str, &str)], exclude: &[&str], include: &[&str]) -> IndexView {
        IndexView { nodes: nodes.iter().map(|(p, h)| (p.to_string(), Some(h.to_string()))).collect(), health: HashMap::new(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(), include: include.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn the_terms_of_an_ask_are_its_messages_and_distinctive_identifiers_never_plain_words() {
        let ask = "Fix the \"Cannot read property of undefined\" crash in `buildLedgerRow` — see parse_rows and getComponentNameFromType; also update the function please, src/ledger/rows.ts is the file. The version2 flag.";
        let t = area_terms(ask);
        assert_eq!(t[0], "Cannot read property of undefined", "a quoted message comes first: it is what a port of the same code repeats");
        assert!(t.contains(&"buildLedgerRow".to_string()) && t.contains(&"parse_rows".to_string()) && t.contains(&"getComponentNameFromType".to_string()) && t.contains(&"version2".to_string()), "{t:?}");
        for plain in ["update", "function", "please", "crash", "ledger"] { assert!(!t.iter().any(|x| x == plain), "{plain} is a plain word: searching it finds the repo"); }
        assert!(!t.iter().any(|x| x.contains("rows.ts")), "a path is a named file, not a search term");
        assert!(area_terms("make the cart faster").is_empty());
        let many = (0..20).map(|i| format!("helper_number_{i}")).collect::<Vec<_>>().join(" ");
        assert_eq!(area_terms(&many).len(), MAX_TERMS);
    }

    #[test]
    fn files_the_ask_names_are_taken_when_they_exist() {
        let got = named_files("look at src/ledger/rows.ts, `crates/core/src/extract.rs`. And ./README.md or https://x.io/a.js and nothing.txt", |p| p != "nothing.txt" && p != "README.md");
        assert_eq!(got, vec!["src/ledger/rows.ts", "crates/core/src/extract.rs"]);
    }

    #[test]
    fn a_path_the_index_leaves_out_says_why_in_words() {
        let ix = idx(&[("src/a.ts", "h1"), ("src/b.ts", "h2")], &["crates/**"], &[]);
        let g = classify("crates/core/src/extract.rs", &ix, None).unwrap();
        assert_eq!((g.kind, g.why.as_str()), ("not-indexed", "excluded from the index by the scope setting `crates/**`"));
        assert!(classify("web/node_modules/x/i.js", &ix, None).unwrap().why.contains("`node_modules/` is never indexed"));
        assert!(classify("src/new.ts", &ix, None).unwrap().why.contains("added after it was built"));
        assert!(classify("ui/App.vue", &ix, None).unwrap().why.contains("`.vue` is a language the index does not read"));
        let inc = idx(&[("src/a.ts", "h1")], &[], &["src/**"]);
        assert!(classify("tools/gen.py", &inc, None).unwrap().why.contains("outside the indexed scope"));
        assert_eq!(classify("README.md", &ix, None), None, "not source at all");
        assert_eq!(classify("src/a.ts", &ix, Some("h1")), None, "indexed and unchanged: no gap");
        assert_eq!(classify("src/a.ts", &ix, None), None, "nothing to compare: not claimed stale");
        assert_eq!(classify("src/a.ts", &ix, Some("other")).unwrap().kind, "changed");
    }

    #[test]
    fn parse_failures_are_gaps_the_agent_must_hear_about() {
        let mut ix = idx(&[("a.js", "h"), ("b.js", "h"), ("c.js", "h"), ("d.js", "h")], &[], &[]);
        ix.health.insert("a.js".into(), ("fail".into(), "flow: parser gave up (line 3)".into()));
        ix.health.insert("b.js".into(), ("skipped".into(), "no walk profile".into()));
        ix.health.insert("c.js".into(), ("degraded".into(), "2 error(s)".into()));
        assert_eq!(classify("a.js", &ix, Some("h")).unwrap().kind, "not-parsed");
        assert!(classify("a.js", &ix, Some("h")).unwrap().why.contains("no symbols, imports or importers"));
        assert_eq!(classify("b.js", &ix, Some("h")).unwrap().kind, "not-parsed");
        assert_eq!(classify("c.js", &ix, Some("h")).unwrap().kind, "degraded");
        assert_eq!(classify("d.js", &ix, Some("h")), None);
    }

    #[test]
    fn the_standing_blind_spots_group_by_folder_and_carry_the_scope_s_reason() {
        let ix = idx(&[("src/a.ts", "h")], &["crates/**"], &[]);
        let tracked: Vec<String> = ["src/a.ts", "crates/x/a.rs", "crates/x/b.rs", "crates/y/c.rs", "docs/README.md", "tools/new.py", "build.py"].iter().map(|s| s.to_string()).collect();
        let b = blind_spots(&tracked, &ix);
        assert_eq!(b[0], ("crates".to_string(), 3, "excluded from the index by the scope setting `crates/**`".to_string()));
        assert!(b.iter().any(|x| x.0 == "tools" && x.1 == 1 && x.2 == "not in the index"));
        assert!(b.iter().any(|x| x.0 == "." && x.1 == 1), "root files group as `.`");
        assert!(!b.iter().any(|x| x.0 == "docs"), "a markdown file is not source");
    }

    #[test]
    fn the_notice_names_each_file_and_says_what_the_pack_cannot_see() {
        let r = Report { terms: vec!["Cannot read property".into()], checked: 4, searched: true, index_files: 10,
            gaps: vec![Gap { path: "crates/core/src/extract.rs".into(), kind: "not-indexed", why: "excluded from the index by the scope setting `crates/**`".into() },
                       Gap { path: "src/a.ts".into(), kind: "changed", why: "edited after the index was built".into() }],
            blind: vec![("crates".into(), 212, "excluded from the index by the scope setting `crates/**`".into())], unparsed: 0, listed: true, unread: vec![] };
        let md = notice_md(&r);
        assert!(md.contains("2 of 4 source file(s) that mention `Cannot read property`"), "{md}");
        assert!(md.contains("- NOT INDEXED: `crates/core/src/extract.rs` - excluded from the index by the scope setting `crates/**`"), "{md}");
        assert!(md.contains("- CHANGED SINCE THE INDEX: `src/a.ts`"), "{md}");
        assert!(md.contains("Search those files yourself"), "{md}");
        assert!(md.contains("a caller, a port or a second implementation there will not be listed"), "{md}");
        assert!(md.contains("- `crates/` - 212 tracked source file(s) with no entry in the index"), "{md}");
        assert!(brief_line(&r).starts_with("COVERAGE WARNING: 2 file(s) this ask touches"), "{}", brief_line(&r));
        assert!(brief_line(&r).contains("212 tracked source file(s) in 1 folder(s)"));
    }

    #[test]
    fn a_clean_check_says_what_was_checked_and_what_was_not() {
        let r = Report { terms: vec!["parse_rows".into()], checked: 3, searched: true, index_files: 10, listed: true, ..Default::default() };
        let md = notice_md(&r);
        assert!(md.contains("Checked 3 source file(s) that mention `parse_rows`"), "{md}");
        assert!(md.contains("a file that never spells those words is not checked"), "it never certifies completeness: {md}");
        assert!(md.contains("Every tracked file with an extension the parser reads has an entry in the index."), "{md}");
        assert!(!md.contains("was parsed") && md.contains("does not say the entries are current"), "it claims the entry, not the parse nor the freshness: {md}");
        assert_eq!(brief_line(&r), "", "nothing to warn about: no line");
        let none = notice_md(&Report::default());
        assert!(none.contains("could not be tested"), "{none}");
    }

    #[test]
    fn files_the_parser_could_not_read_are_a_standing_blind_spot_even_when_every_file_has_a_node() {
        // found live 2026-10-01 on the React copy: "Every tracked source file the parser reads is in the index" printed under three NOT PARSED lines
        let r = Report { terms: vec![], checked: 0, searched: true, index_files: 4579, unparsed: 25, listed: true, ..Default::default() };
        let md = notice_md(&r);
        assert!(md.contains("25 file(s) the parser could not read or skipped"), "{md}");
        assert!(!md.contains("Every tracked file"), "{md}");
        assert!(brief_line(&r).contains("25 file(s) the parser could not read"), "{}", brief_line(&r));
    }

    /// The experiment, end to end on a real git tree: `crates/` is excluded from the index, the ask quotes a message that
    /// the Rust port repeats, and the check must find the port by plain text search and say so.
    #[test]
    fn a_rust_port_of_the_same_message_in_an_excluded_folder_is_found_and_named() {
        let dir = std::env::temp_dir().join(format!("antiloki-coverage-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::create_dir_all(dir.join("crates/core/src")).unwrap();
        fs::write(dir.join("src/extract.ts"), "export function extractNames(x: string) {\n  throw new Error('cannot extract names from an empty ledger');\n}\n").unwrap();
        fs::write(dir.join("crates/core/src/extract.rs"), "pub fn extract_names(x: &str) -> Result<(), String> {\n    Err(\"cannot extract names from an empty ledger\".into())\n}\n").unwrap();
        fs::write(dir.join("src/other.ts"), "export const unrelated = 1;\n").unwrap();
        let git = |a: &[&str]| { crate::proc::cmd("git").args(a).current_dir(&dir).output().unwrap(); };
        git(&["init", "-q"]); git(&["add", "-A"]);
        let db = Connection::open_in_memory().unwrap();
        crate::codebase_code_schema::ensureCodeSchema(&db, &Value::Null);
        let h = crate::codebase_atlas_ingest::hashSource(&fs::read_to_string(dir.join("src/extract.ts")).unwrap());
        db.execute_batch(&format!(
            "INSERT INTO code_nodes (id,origin,name,path,type,content_hash) VALUES ('src/extract.ts','atlas','extract.ts','src/extract.ts','file','{h}'), ('src/other.ts','atlas','other.ts','src/other.ts','file','x'); \
             INSERT INTO code_scope (origin, exclude_json, include_json) VALUES ('atlas','[\"crates/**\"]','[]');")).unwrap();

        let r = check(&db, &dir, "atlas", "the message \"cannot extract names from an empty ledger\" is wrong, fix it", &[]);
        assert!(r.searched);
        assert_eq!(r.terms, vec!["cannot extract names from an empty ledger"]);
        assert_eq!(r.checked, 2, "both implementations mention the message");
        assert_eq!(r.gaps.len(), 1, "{:?}", r.gaps);
        assert_eq!(r.gaps[0].path, "crates/core/src/extract.rs");
        assert_eq!(r.gaps[0].kind, "not-indexed");
        assert!(notice_md(&r).contains("NOT INDEXED: `crates/core/src/extract.rs` - excluded from the index by the scope setting `crates/**`"));
        assert_eq!(r.blind[0].0, "crates", "and the standing blind spot is reported even for an ask with no words");

        // an edit after the index: the TypeScript file changes, and the check says the pack's view of it is old
        fs::write(dir.join("src/extract.ts"), "export function extractNames(x: string) {\n  throw new Error('cannot extract names from an empty ledger');\n}\n// edited\n").unwrap();
        let r2 = check(&db, &dir, "atlas", "\"cannot extract names from an empty ledger\"", &[]);
        assert!(r2.gaps.iter().any(|g| g.path == "src/extract.ts" && g.kind == "changed"), "{:?}", r2.gaps);

        // no words at all: the standing blind spots are still said
        let r3 = check(&db, &dir, "atlas", "make it faster", &[]);
        assert_eq!(r3.checked, 0);
        assert!(notice_md(&r3).contains("- `crates/` - 1 tracked source file(s) with no entry in the index"), "{}", notice_md(&r3));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_that_is_not_a_git_tree_checks_only_the_files_the_ask_names() {
        let dir = std::env::temp_dir().join(format!("antiloki-coverage-nogit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.ts"), "export const a = 1;\n").unwrap();
        let db = Connection::open_in_memory().unwrap();
        crate::codebase_code_schema::ensureCodeSchema(&db, &Value::Null);
        let r = check(&db, &dir, "atlas", "look at src/a.ts and the `someDistinctiveName`", &[]);
        assert_eq!(r.checked, 1);
        assert_eq!(r.gaps.len(), 1);
        assert!(r.gaps[0].why.contains("not in the index"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// REVIEW 2026-10-01: the standing line still certified completeness in two cases, against this module's own rule. (a) `git ls-files` failed (not a git tree): `blind` was empty,
    /// so "every tracked source file is in the index and was parsed" printed for a list nobody read. (b) `blind_spots` skips the languages the parser does not read, so a repo full of
    /// `.vue` and `.sql` printed the same line while `classify` flagged each of those files whenever an ask named one.
    #[test]
    fn the_standing_line_never_certifies_a_list_it_could_not_read_or_a_language_it_cannot_parse() {
        // (a) not a git tree: say the check was not run
        let r = Report { index_files: 12, listed: false, ..Default::default() };
        let md = notice_md(&r);
        assert!(!md.contains("Every tracked"), "{md}");
        assert!(md.contains("could not be listed") && md.contains("not a git work tree"), "{md}");
        // (b) tracked .vue / .sql files are a blind spot of their own, counted by extension
        let tracked: Vec<String> = ["src/a.ts", "ui/App.vue", "ui/Nav.vue", "db/schema.sql", "README.md"].iter().map(|s| s.to_string()).collect();
        assert_eq!(unread_languages(&tracked), vec![("vue".to_string(), 2), ("sql".to_string(), 1)]);
        let r = Report { index_files: 12, listed: true, unread: unread_languages(&tracked), ..Default::default() };
        let md = notice_md(&r);
        assert!(!md.contains("Every tracked"), "{md}");
        assert!(md.contains("3 tracked file(s) in a language the index does not read (2 `.vue`, 1 `.sql`)"), "{md}");
        assert!(brief_line(&r).contains("3 tracked file(s) in a language the index does not read"), "{}", brief_line(&r));
        assert_eq!(report_json(&r)["unread_languages"][0], json!({ "ext": "vue", "files": 2 }));
    }

    /// The same two cases on a real folder: a git tree with a `.vue` file, and a folder with no git at all.
    #[test]
    fn a_real_folder_with_a_vue_file_or_without_git_never_reads_as_fully_indexed() {
        let dir = std::env::temp_dir().join(format!("antiloki-coverage-vue-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.ts"), "export const a = 1;\n").unwrap();
        fs::write(dir.join("src/App.vue"), "<template><p>hi</p></template>\n").unwrap();
        let db = Connection::open_in_memory().unwrap();
        crate::codebase_code_schema::ensureCodeSchema(&db, &Value::Null);
        db.execute_batch("INSERT INTO code_nodes (id,origin,name,path,type,content_hash) VALUES ('src/a.ts','atlas','a.ts','src/a.ts','file','x');").unwrap();
        // no git yet: the list fails
        let r = check(&db, &dir, "atlas", "make it faster", &[]);
        assert!(!r.listed, "{r:?}");
        assert!(!notice_md(&r).contains("Every tracked"), "{}", notice_md(&r));
        // a git tree: the list works, a.ts is indexed, App.vue can never be
        let git = |a: &[&str]| { crate::proc::cmd("git").args(a).current_dir(&dir).output().unwrap(); };
        git(&["init", "-q"]); git(&["add", "-A"]);
        let r = check(&db, &dir, "atlas", "make it faster", &[]);
        assert!(r.listed && r.blind.is_empty(), "{r:?}");
        assert_eq!(r.unread, vec![("vue".to_string(), 1)]);
        let md = notice_md(&r);
        assert!(!md.contains("Every tracked") && md.contains("1 `.vue`"), "{md}");
        let _ = fs::remove_dir_all(&dir);
    }
}
