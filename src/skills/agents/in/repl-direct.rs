//! repl-direct.rs — twin of skills/agents/in/repl-direct.mjs, antiloki's direct AI TERMINAL.
//! NAMES ARE CONTRACT: `replDirect` (default export in JS; `pub fn replDirect` here).
//!
//! LLM-PARTIAL / PARITY-DEFERRED: the JS `await aiCall(...)` call inside the bounded ReAct loop
//! maps to `crate::ai_ai_call::aiCall(&input, db)` — the deferred stub documented in ai-call.rs's
//! own module doc. It always returns `Err(...)`, because the real provider dispatch is a live
//! network round-trip to a BYOK LLM, genuinely non-deterministic and unconvertible in principle
//! (same convention as book-run.rs's `fac_call`, episode-consolidate.rs's AI branch, and every
//! other twin downstream of ai-call.rs). The JS `replDirect` does NOT wrap its `await aiCall(...)`
//! in try/catch — an aiCall failure propagates straight out of `replDirect` as a rejected promise
//! — so this twin mirrors that with a plain `?` on the call: real invocations always return `Err`
//! from the very first loop iteration (round 0), which is the exact behavior a real JS call would
//! have on a genuine provider failure. Everything else — guards, sandbox/root resolution, the
//! LAWS/MEMORY/ATLAS context gatherers, the scoped read-only (or sandboxed WRITABLE) file-tool
//! surface (READ/LIST/GREP/WRITE, `parseTools`/`execTool`), and the prompt/history/tool markdown
//! assembly fed into `input_kv` — is ported faithfully below and covered by native tests.
//!
//! `provider_fn` (a JS function value passed straight through to `aiCall`) has no Rust
//! representation and is dropped from the assembled `aiCall` input — inert on this twin since
//! `aiCall` never reaches the branch that would use it (see ai-call.rs's own deferred stub).
//!
//! Cross-module calls (already-converted twins, reused not reimplemented): `crate::ai_prompt_find::
//! promptFind`, `crate::ai_prompt_add::promptAdd` (the `ensurePrompt` find-or-register flow, same
//! pattern as episode-consolidate.rs), `crate::ai_ai_call::aiCall` (deferred), `crate::
//! agents_in_terminal_sandbox::ensure` (the writable sandbox provisioning leg).
//!
//! `context.db` missing → JS throws `err.contextMissing('repl-direct', 'db')`. Not modeled: `db:
//! &Connection` stays a required, non-nullable reference — same convention as every sibling db-lane
//! twin (stack-facts.rs, episode-consolidate.rs, book-run.rs, ...).
//!
//! `atlasContext`'s path resolution (`path.isAbsolute(db_dir)` / `path.resolve(cwd, db_dir,
//! 'code.sqlite')`) is ported via a plain lexical join + `..`/`.`-component normalization rather
//! than a full Node `path.resolve` port — the whole helper is already best-effort in the JS itself
//! (wrapped in try/catch, degrading to `''` on any failure), so an inexact edge case here degrades
//! the same way the JS degrades on a missing file. `inScope`'s traversal check reuses the same
//! lexical-normalize primitive for the identical reason (both callers already tolerate `None` /
//! `''` on ambiguity). Directory-walk order (`codebaseMap`) follows `std::fs::read_dir`'s OS
//! iteration order, matching the JS's dependence on `readdirSync`'s (also OS-dependent) order — no
//! twin in this codebase sorts it either.
// QUALITY: mixed unit — deterministic guards/context-gathering/file-tool surface ported and
// fixture-free-tested (bespoke-native: real fs walk/tool exec); the aiCall network dispatch is the
// disclosed PARITY-DEFERRED boundary inherited from ai-call.rs, not re-litigated here.

use regex::Regex;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

const PROMPT_NAME: &str = "system.repl.direct.v2";
const MAX_TOOL_ROUNDS: usize = 4;
const MAX_TOOLS_PER_ROUND: usize = 6;
const FILE_CAP: usize = 40 * 1024;
const LIST_CAP: usize = 200;
const GREP_CAP: usize = 60;
const MAP_CAP: usize = 400;

const PROMPT_TEMPLATE_MD: &str = "You are antiloki's TERMINAL — a direct, technical coding REPL. You have the substrate's context (laws you MUST honor, the agent's memory, and — when granted — the live codebase), but you operate OUTSIDE the card/contract/pipeline process: answer and help directly, like a senior engineer pairing at a shell. Be concise and technical — no corporate filler.\n\n{{context}}\n\n{{tools}}\n\nPRIOR EXCHANGES (oldest first; may be empty):\n{{history}}\n\nLATEST INPUT:\n{{user_input}}";

fn skip_dirs() -> &'static HashSet<&'static str> {
    static S: std::sync::OnceLock<HashSet<&'static str>> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        [".git", "node_modules", "dist", "build", ".next", "out", "coverage", ".antiloki", ".turbo"]
            .into_iter()
            .collect()
    })
}

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }
fn take_chars(s: &str, n: usize) -> String { s.chars().take(n).collect() }
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// = JS `String(v ?? '')`
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

// ── prompt bootstrap (find-or-register) ────────────────────────────────────
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).is_ok() {
        return Ok(());
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "mid",
            "default_temperature": "warm",
            "max_output_tokens": 2048,
            "output_fields": [ { "field_name": "response", "field_type": "string", "required": true } ],
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db)?;
    Ok(())
}

// ── context gatherers (ATTACHed DBs) ────────────────────────────────────────
fn laws_block(db: &Connection) -> String {
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT slug, title, statement, enforcement FROM laws.laws \
         WHERE superseded_by IS NULL \
         ORDER BY CASE enforcement WHEN 'hard' THEN 0 ELSE 1 END, id LIMIT 40",
        &[],
    ) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let arr = rows.as_array().cloned().unwrap_or_default();
    if arr.is_empty() { return String::new(); }
    let lines: Vec<String> = arr
        .iter()
        .map(|r| {
            format!(
                "- [{}] {}: {} — {}",
                str_qq(r.get("enforcement")),
                str_qq(r.get("slug")),
                str_qq(r.get("title")),
                take_chars(&str_qq(r.get("statement")), 240)
            )
        })
        .collect();
    format!("LAWS (honor these):\n{}", lines.join("\n"))
}

fn memory_block(db: &Connection) -> String {
    let rows = match crate::db_json::query_rows(
        db,
        "SELECT type, title, guidance, situation FROM memory.memories \
         WHERE COALESCE(superseded_by, '') = '' \
           AND title NOT LIKE 'escalation:%' AND title NOT LIKE 'Codebase onboarded:%' \
           AND title NOT LIKE '[rejection]%' AND title NOT LIKE 'sandbox-reset:%' \
         ORDER BY id DESC LIMIT 30",
        &[],
    ) {
        Ok(v) => v,
        Err(_) => return String::new(),
    };
    let arr = rows.as_array().cloned().unwrap_or_default();
    if arr.is_empty() { return String::new(); }
    let lines: Vec<String> = arr
        .iter()
        .map(|r| {
            // `r.guidance || r.situation || ''` — JS-falsy fallthrough
            let body = if truthy(r.get("guidance")) {
                str_qq(r.get("guidance"))
            } else if truthy(r.get("situation")) {
                str_qq(r.get("situation"))
            } else {
                String::new()
            };
            let suffix = if body.is_empty() { String::new() } else { format!(": {}", take_chars(&body, 200)) };
            format!("- ({}) {}{}", str_qq(r.get("type")), str_qq(r.get("title")), suffix)
        })
        .collect();
    format!("MEMORY (curated learnings):\n{}", lines.join("\n"))
}

fn workspace_root(db: &Connection) -> Option<String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT root_path FROM state.workspaces WHERE is_active = 1 LIMIT 1",
        &[],
    ).ok()?;
    let row = rows.as_array().and_then(|a| a.first())?;
    let s = row.get("root_path").and_then(Value::as_str)?;
    if s.is_empty() { None } else { Some(s.to_string()) }
}

// Lexical `.`/`..`-component normalization (no filesystem access) — see module doc for why this
// substitutes for a full `path.resolve` port here.
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                match out.components().next_back() {
                    Some(Component::Normal(_)) => { out.pop(); }
                    _ => { out.push(".."); }
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}
fn resolve_from(base: &Path, rel: &str) -> PathBuf {
    let p = Path::new(rel);
    let combined = if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
    lexical_normalize(&combined)
}

// The active workspace's code.sqlite (the Atlas) is a SEPARATE file (not ATTACHed) — best-effort,
// degrades to '' on any failure (missing dir/file/tables).
fn atlas_context(db: &Connection) -> String {
    let db_dir = {
        let rows = match crate::db_json::query_rows(
            db,
            "SELECT db_dir FROM state.workspaces WHERE is_active = 1 LIMIT 1",
            &[],
        ) {
            Ok(v) => v,
            Err(_) => return String::new(),
        };
        match rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("db_dir")).and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return String::new(),
        }
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let db_dir_path = Path::new(&db_dir);
    let mut cands: Vec<PathBuf> = Vec::new();
    if db_dir_path.is_absolute() {
        cands.push(lexical_normalize(&db_dir_path.join("code.sqlite")));
    }
    cands.push(resolve_from(&cwd, &db_dir).join("code.sqlite"));
    let code_path = match cands.into_iter().find(|p| p.exists()) {
        Some(p) => p,
        None => return String::new(),
    };
    let cdb = match Connection::open_with_flags(&code_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    let mut parts: Vec<String> = Vec::new();
    let files = crate::db_json::query_rows(&cdb, "SELECT COUNT(*) c FROM code_nodes WHERE type='file'", &[])
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first()).and_then(|r| r.get("c")).and_then(Value::as_i64))
        .unwrap_or(0);
    let edges = crate::db_json::query_rows(&cdb, "SELECT COUNT(*) c FROM code_edges", &[])
        .ok()
        .and_then(|v| v.as_array().and_then(|a| a.first()).and_then(|r| r.get("c")).and_then(Value::as_i64))
        .unwrap_or(0);
    parts.push(format!("map: {files} files, {edges} edges"));

    if let Ok(eps) = crate::db_json::query_rows(
        &cdb,
        "SELECT kind, path, label FROM code_entry_points ORDER BY confidence DESC LIMIT 15",
        &[],
    ) {
        let arr = eps.as_array().cloned().unwrap_or_default();
        if !arr.is_empty() {
            let lines: Vec<String> = arr
                .iter()
                .map(|e| {
                    let label = e.get("label").and_then(Value::as_str).unwrap_or("");
                    let suffix = if label.is_empty() { String::new() } else { format!(" — {label}") };
                    format!("  [{}] {}{}", str_qq(e.get("kind")), str_qq(e.get("path")), suffix)
                })
                .collect();
            parts.push(format!("entry points (external surface):\n{}", lines.join("\n")));
        }
    }

    if let Ok(snaps) = crate::db_json::query_rows(
        &cdb,
        "SELECT path, snapshot_json FROM code_file_snapshots ORDER BY id DESC LIMIT 20",
        &[],
    ) {
        static WS_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
        let ws_re = WS_RE.get_or_init(|| Regex::new(r"\s+").unwrap());
        let mut lines: Vec<String> = Vec::new();
        for s in snaps.as_array().cloned().unwrap_or_default() {
            let desc = s
                .get("snapshot_json")
                .and_then(Value::as_str)
                .and_then(|txt| serde_json::from_str::<Value>(txt).ok())
                .and_then(|j| j.get("description").and_then(Value::as_str).map(String::from))
                .unwrap_or_default();
            if !desc.is_empty() {
                let collapsed = ws_re.replace_all(&desc, " ").into_owned();
                lines.push(format!("  {}: {}", str_qq(s.get("path")), take_chars(&collapsed, 140)));
            }
        }
        if !lines.is_empty() {
            parts.push(format!("file snapshots (what each does):\n{}", lines.join("\n")));
        }
    }

    if parts.is_empty() { String::new() } else { format!("CODEBASE ATLAS:\n{}", parts.join("\n")) }
}

// ── scoped, read-only (or sandboxed writable) file access ──────────────────
fn clean_exclude(ex: &str) -> String {
    let trimmed = ex.trim();
    let no_lead = trimmed.trim_start_matches(|c| c == '.' || c == '/');
    let no_trail = no_lead.trim_end_matches(|c| c == '/' || c == '\\');
    no_trail.split(|c| c == '/' || c == '\\').collect::<Vec<_>>().join("/")
}

// A path is allowed iff it resolves UNDER the workspace root AND is not within an excluded folder.
fn in_scope(root: &str, rel: &str, excludes: &[String]) -> Option<PathBuf> {
    let r = lexical_normalize(Path::new(root));
    let abs = resolve_from(&r, rel);
    if abs != r {
        let r_str = r.to_string_lossy().into_owned();
        let abs_str = abs.to_string_lossy().into_owned();
        let prefixed = format!("{r_str}{}", std::path::MAIN_SEPARATOR);
        if !abs_str.starts_with(&prefixed) { return None; }
    }
    let rel_norm = if abs == r {
        String::new()
    } else {
        let r_str = r.to_string_lossy().into_owned();
        let abs_str = abs.to_string_lossy().into_owned();
        abs_str[r_str.len() + 1..].replace('\\', "/")
    };
    for ex in excludes {
        let e = clean_exclude(ex);
        if !e.is_empty() && (rel_norm == e || rel_norm.starts_with(&format!("{e}/"))) {
            return None;
        }
    }
    Some(abs)
}

fn codebase_map(root: &str, excludes: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    fn walk(root: &str, dir: &Path, rel_prefix: &str, depth: u32, excludes: &[String], out: &mut Vec<String>) {
        if out.len() >= MAP_CAP || depth > 5 { return; }
        let entries = match std::fs::read_dir(dir) { Ok(e) => e, Err(_) => return };
        for entry in entries.flatten() {
            if out.len() >= MAP_CAP { return; }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && name != ".env.example" { continue; }
            let rel = if rel_prefix.is_empty() { name.clone() } else { format!("{rel_prefix}/{name}") };
            let abs = dir.join(&name);
            if in_scope(root, &rel, excludes).is_none() { continue; }
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => {
                    if !skip_dirs().contains(name.as_str()) { walk(root, &abs, &rel, depth + 1, excludes, out); }
                }
                _ => out.push(rel),
            }
        }
    }
    let root_norm = lexical_normalize(Path::new(root));
    walk(root, &root_norm, "", 0, excludes, &mut out);
    out
}

struct ToolOp { verb: String, arg: String, content: Option<String> }

// Parse the model's tool requests: single-line READ/LIST/GREP, and (when writable) WRITE <path>
// followed by a fenced ``` block whose body is the full file content.
fn parse_tools(resp: &str, writable: bool) -> Vec<ToolOp> {
    static RLG_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static WRITE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static FENCE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let rlg_re = RLG_RE.get_or_init(|| Regex::new(r"^(READ|LIST|GREP)\s+(.+)$").unwrap());
    let write_re = WRITE_RE.get_or_init(|| Regex::new(r"^WRITE\s+(.+)$").unwrap());
    let fence_re = FENCE_RE.get_or_init(|| Regex::new(r"^```").unwrap());

    let lines: Vec<&str> = resp.split('\n').collect();
    let mut ops: Vec<ToolOp> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() && ops.len() < MAX_TOOLS_PER_ROUND {
        let t = lines[i].trim();
        if let Some(caps) = rlg_re.captures(t) {
            ops.push(ToolOp { verb: caps[1].to_string(), arg: caps[2].trim().to_string(), content: None });
        } else if writable {
            if let Some(caps) = write_re.captures(t) {
                let mut j = i + 1;
                while j < lines.len() && lines[j].trim().is_empty() { j += 1; }
                if j < lines.len() && fence_re.is_match(lines[j].trim()) {
                    let mut body: Vec<&str> = Vec::new();
                    j += 1;
                    while j < lines.len() && !fence_re.is_match(lines[j].trim()) { body.push(lines[j]); j += 1; }
                    ops.push(ToolOp { verb: "WRITE".to_string(), arg: caps[1].trim().to_string(), content: Some(body.join("\n")) });
                    i = j; // skip past the closing fence
                }
            }
        }
        i += 1;
    }
    ops
}

fn exec_tool(root: &str, excludes: &[String], op: &ToolOp, writable: bool) -> String {
    if op.verb == "GREP" {
        let re = Regex::new(&format!("(?i){}", &op.arg)).ok();
        let mut hits: Vec<String> = Vec::new();
        'files: for f in codebase_map(root, excludes) {
            if hits.len() >= GREP_CAP { break; }
            let abs = match in_scope(root, &f, excludes) { Some(a) => a, None => continue };
            let meta = match std::fs::metadata(&abs) { Ok(m) => m, Err(_) => continue };
            if meta.len() as usize > FILE_CAP { continue; }
            let txt = match std::fs::read_to_string(&abs) { Ok(t) => t, Err(_) => continue };
            for (i, line) in txt.split('\n').enumerate() {
                if hits.len() >= GREP_CAP { break 'files; }
                let matched = match &re { Some(r) => r.is_match(line), None => line.contains(&op.arg) };
                if matched {
                    hits.push(format!("{f}:{}: {}", i + 1, take_chars(line.trim(), 160)));
                }
            }
        }
        let body = if hits.is_empty() { "(no matches)".to_string() } else { hits.join("\n") };
        return format!("GREP {}\n{}", op.arg, body);
    }

    let abs = match in_scope(root, &op.arg, excludes) {
        Some(a) => a,
        None => return format!("{} {}\n(denied: out of scope)", op.verb, op.arg),
    };

    if op.verb == "WRITE" {
        if !writable {
            return format!("WRITE {}\n(denied: read-only — a sandbox session is needed to write)", op.arg);
        }
        let content = op.content.clone().unwrap_or_default();
        if let Some(parent) = abs.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return format!("WRITE {}\n(write failed: could not create parent directory)", op.arg);
            }
        }
        return match std::fs::write(&abs, &content) {
            Ok(_) => format!("WRITE {}\n(written {} bytes — isolated in the sandbox until you apply)", op.arg, content.len()),
            Err(e) => format!("WRITE {}\n(write failed: {})", op.arg, e),
        };
    }

    if op.verb == "LIST" {
        let entries = match std::fs::read_dir(&abs) { Ok(e) => e, Err(_) => return format!("LIST {}\n(not a directory / unreadable)", op.arg) };
        let names: Vec<String> = entries
            .flatten()
            .take(LIST_CAP)
            .map(|e| {
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                let n = e.file_name().to_string_lossy().into_owned();
                if is_dir { format!("{n}/") } else { n }
            })
            .collect();
        return format!("LIST {}\n{}", op.arg, names.join("\n"));
    }

    // READ
    let meta = match std::fs::metadata(&abs) { Ok(m) => m, Err(_) => return format!("READ {}\n(not found)", op.arg) };
    if meta.is_dir() { return format!("READ {}\n(is a directory — use LIST)", op.arg); }
    let txt = match std::fs::read_to_string(&abs) { Ok(t) => t, Err(_) => return format!("READ {}\n(unreadable)", op.arg) };
    let body = if txt.chars().count() > FILE_CAP { format!("{}\n…(truncated)", take_chars(&txt, FILE_CAP)) } else { txt };
    format!("READ {}\n{}", op.arg, body)
}

/// `export default async function replDirect(input, context)`.
pub fn replDirect(input: &Value, db: &Connection) -> Result<Value, String> {
    let user_input = match input.get("user_input").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Err("repl-direct: user_input is required".to_string()),
    };
    let history = input.get("history").and_then(Value::as_array).cloned().unwrap_or_default();
    let model_id = nz(input.get("model_id")).cloned().unwrap_or(Value::Null);
    let context_flags = input.get("context_flags").cloned().unwrap_or(json!({}));
    let scope = input.get("scope").cloned().unwrap_or(json!({}));
    let session_id = nz(input.get("session_id")).cloned();

    ensure_prompt(db)?;

    let flags_laws = truthy(context_flags.get("laws"));
    let flags_memory = truthy(context_flags.get("memory"));
    let flags_codebase = truthy(context_flags.get("codebase"));

    let excludes: Vec<String> = scope
        .get("exclude_folders")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let include_folders: Vec<String> = scope
        .get("include_folders")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    // Sandbox mode: codebase + a session_id → provision a sparse-checkout worktree, operate
    // WRITABLY inside it. Without a session (or on provisioning failure) → read-only on root.
    let mut root: Option<String> = None;
    let mut writable = false;
    let mut sandbox_info: Value = Value::Null;
    if flags_codebase {
        if let Some(sid) = session_id.clone() {
            let ensure_input = json!({ "session_id": sid, "allow_folders": include_folders });
            match crate::agents_in_terminal_sandbox::ensure(&ensure_input, db) {
                Ok(info) => {
                    root = info.get("path").and_then(Value::as_str).map(String::from);
                    writable = true;
                    sandbox_info = info;
                }
                Err(e) => {
                    root = workspace_root(db);
                    writable = false;
                    sandbox_info = json!({ "error": take_chars(&e, 200) });
                }
            }
        } else {
            root = workspace_root(db);
        }
    }
    let tools_enabled = flags_codebase && root.is_some();

    // ── build the injected context ──
    let mut ctx_parts: Vec<String> = Vec::new();
    if flags_laws { let b = laws_block(db); if !b.is_empty() { ctx_parts.push(b); } }
    if flags_memory { let b = memory_block(db); if !b.is_empty() { ctx_parts.push(b); } }
    if tools_enabled {
        let root_s = root.clone().unwrap();
        let files = codebase_map(&root_s, &excludes);
        let header = if writable {
            let folders_note = if include_folders.is_empty() { " — whole repo".to_string() } else { format!(" — only: {}", include_folders.join(", ")) };
            format!("SANDBOX ({root_s}){folders_note}, WRITABLE (edits isolated until applied)")
        } else {
            let excl_note = if excludes.is_empty() { String::new() } else { format!(" — excluding {}", excludes.join(", ")) };
            format!("CODEBASE ({root_s}){excl_note}, read-only")
        };
        let shown: Vec<&String> = files.iter().take(300).collect();
        let more = if files.len() > 300 { format!("\n… ({} more)", files.len() - 300) } else { String::new() };
        let joined: Vec<&str> = shown.iter().map(|s| s.as_str()).collect();
        ctx_parts.push(format!("{header}:\n{}{more}", joined.join("\n")));
        let atlas = atlas_context(db);
        if !atlas.is_empty() { ctx_parts.push(atlas); }
    }
    let context_md = if ctx_parts.is_empty() {
        "(no extra context selected)".to_string()
    } else {
        format!("=== SUBSTRATE CONTEXT ===\n{}", ctx_parts.join("\n\n"))
    };

    let tools_md = if tools_enabled {
        let write_line = if writable { "\n  WRITE <relative/path>  ← then a fenced ``` block with the FULL new file content" } else { "" };
        let header_note = if writable { " — sandbox: read + WRITE (edits isolated until applied)" } else { " — read-only, scoped" };
        format!(
            "=== FILE TOOLS{header_note} ===\nReply with ONLY tool lines (up to {MAX_TOOLS_PER_ROUND}):\n  READ <relative/path>\n  LIST <relative/dir>\n  GREP <regex>{write_line}\nI run them and return the results; continue. Your FINAL answer has NO tool lines."
        )
    } else {
        String::new()
    };

    let hist_slice: Vec<&Value> = {
        let n = history.len();
        let start = n.saturating_sub(12);
        history[start..].iter().collect()
    };
    let history_md = if hist_slice.is_empty() {
        "(none — first message)".to_string()
    } else {
        hist_slice
            .iter()
            .map(|m| {
                let role = if m.get("role").and_then(Value::as_str) == Some("assistant") { "AI" } else { "USER" };
                let text = take_chars(&str_qq(m.get("text")), 4000);
                format!("{role}: {text}")
            })
            .collect::<Vec<String>>()
            .join("\n")
    };

    // ── the (bounded) ReAct loop ──
    let mut tool_transcript = String::new();
    let mut final_response = String::new();
    let mut last_result: Value = Value::Null;
    let mut total_cost: f64 = 0.0;
    let mut ai_call_ids: Vec<Value> = Vec::new();

    let max_round = if tools_enabled { MAX_TOOL_ROUNDS } else { 0 };
    for round in 0..=max_round {
        let user_block = if tool_transcript.is_empty() {
            user_input.clone()
        } else {
            format!("{user_input}\n\n=== TOOL RESULTS (from your requests) ===\n{tool_transcript}")
        };
        let ai_input = json!({
            "prompt_name": PROMPT_NAME,
            "input_kv": [
                { "key": "context", "value": context_md },
                { "key": "tools", "value": tools_md },
                { "key": "history", "value": history_md },
                { "key": "user_input", "value": user_block },
            ],
            "tier": "mid", "temperature": "warm", "model_id": model_id, "intent": "terminal.repl",
        });
        // PARITY-DEFERRED boundary: real `aiCall` always Errs here (see module doc). The JS
        // `await`s this uncaught, so a real failure propagates straight out of `replDirect` too.
        let r = crate::ai_ai_call::aiCall(&ai_input, db)?;
        last_result = r.clone();
        if let Some(id) = nz(r.get("ai_call_id")) { ai_call_ids.push(id.clone()); }
        total_cost += r.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);

        let mut out: Map<String, Value> = Map::new();
        for row in r.get("output_kv").and_then(Value::as_array).into_iter().flatten() {
            if let Some(fname) = row.get("field_name").and_then(Value::as_str) {
                if let Some(v) = nz(row.get("value_text")) { out.insert(fname.to_string(), v.clone()); }
            }
        }
        let resp = out.get("response").and_then(Value::as_str).unwrap_or("").to_string();

        let ops = if tools_enabled { parse_tools(&resp, writable) } else { Vec::new() };
        if ops.is_empty() || round == MAX_TOOL_ROUNDS {
            final_response = resp;
            break;
        }
        let root_s = root.clone().unwrap();
        let results: Vec<String> = ops.iter().map(|op| exec_tool(&root_s, &excludes, op, writable)).collect();
        if !tool_transcript.is_empty() { tool_transcript.push_str("\n\n"); }
        tool_transcript.push_str(&results.join("\n\n"));
    }

    let mut out = Map::new();
    out.insert("response".into(), json!(final_response));
    out.insert("ai_call_id".into(), ai_call_ids.last().cloned().unwrap_or(Value::Null));
    out.insert("ai_call_ids".into(), json!(ai_call_ids));
    out.insert("model_id".into(), nz(last_result.get("model_id")).cloned().unwrap_or(Value::Null));
    out.insert("provider".into(), nz(last_result.get("provider")).cloned().unwrap_or(Value::Null));
    out.insert("cost_usd".into(), json!(total_cost));
    out.insert("tools_used".into(), json!(tools_enabled));
    out.insert("writable".into(), json!(writable));
    out.insert("sandbox".into(), sandbox_info);
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── guards ───────────────────────────────────────────────────────────
    #[test]
    fn user_input_required() {
        let db = Connection::open_in_memory().unwrap();
        let e = replDirect(&json!({}), &db).unwrap_err();
        assert_eq!(e, "repl-direct: user_input is required");
        let e2 = replDirect(&json!({ "user_input": "" }), &db).unwrap_err();
        assert_eq!(e2, "repl-direct: user_input is required");
    }

    // ── in_scope / codebase_map traversal ──────────────────────────────────
    #[test]
    fn in_scope_blocks_traversal_and_excluded_folders() {
        let td = std::env::temp_dir().join(format!("repl-direct-scope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&td);
        std::fs::create_dir_all(td.join("src")).unwrap();
        std::fs::create_dir_all(td.join("secret")).unwrap();
        std::fs::write(td.join("src/a.mjs"), "x").unwrap();
        std::fs::write(td.join("secret/b.mjs"), "y").unwrap();
        let root = td.to_string_lossy().into_owned();

        assert!(in_scope(&root, "src/a.mjs", &[]).is_some());
        assert!(in_scope(&root, "../outside.mjs", &[]).is_none());
        assert!(in_scope(&root, "secret/b.mjs", &["secret".to_string()]).is_none());
        assert!(in_scope(&root, "secretfile.mjs", &["secret".to_string()]).is_some());

        let files = codebase_map(&root, &["secret".to_string()]);
        assert!(files.iter().any(|f| f == "src/a.mjs"));
        assert!(!files.iter().any(|f| f.starts_with("secret")));
        let _ = std::fs::remove_dir_all(&td);
    }

    #[test]
    fn codebase_map_skips_dotfiles_and_skip_dirs() {
        let td = std::env::temp_dir().join(format!("repl-direct-map-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&td);
        std::fs::create_dir_all(td.join("node_modules")).unwrap();
        std::fs::write(td.join("node_modules/x.js"), "x").unwrap();
        std::fs::write(td.join(".hidden"), "x").unwrap();
        std::fs::write(td.join(".env.example"), "x").unwrap();
        std::fs::write(td.join("real.mjs"), "x").unwrap();
        let root = td.to_string_lossy().into_owned();
        let files = codebase_map(&root, &[]);
        assert!(files.contains(&"real.mjs".to_string()));
        assert!(files.contains(&".env.example".to_string()));
        assert!(!files.iter().any(|f| f.contains("node_modules")));
        assert!(!files.contains(&".hidden".to_string()));
        let _ = std::fs::remove_dir_all(&td);
    }

    // ── parse_tools ──────────────────────────────────────────────────────
    #[test]
    fn parse_tools_read_list_grep() {
        let resp = "READ src/a.mjs\nLIST src\nGREP foo.*bar\nnot a tool line";
        let ops = parse_tools(resp, false);
        assert_eq!(ops.len(), 3);
        assert_eq!(ops[0].verb, "READ"); assert_eq!(ops[0].arg, "src/a.mjs");
        assert_eq!(ops[1].verb, "LIST"); assert_eq!(ops[1].arg, "src");
        assert_eq!(ops[2].verb, "GREP"); assert_eq!(ops[2].arg, "foo.*bar");
    }

    #[test]
    fn parse_tools_write_requires_fenced_block_and_writable() {
        let resp = "WRITE src/new.mjs\n\n```\nconsole.log(1);\nline2\n```\nsome trailing text";
        let ops_ro = parse_tools(resp, false);
        assert_eq!(ops_ro.len(), 0, "WRITE must be ignored when not writable");

        let ops_w = parse_tools(resp, true);
        assert_eq!(ops_w.len(), 1);
        assert_eq!(ops_w[0].verb, "WRITE");
        assert_eq!(ops_w[0].arg, "src/new.mjs");
        assert_eq!(ops_w[0].content.as_deref(), Some("console.log(1);\nline2"));
    }

    #[test]
    fn parse_tools_caps_at_max_per_round() {
        let mut resp = String::new();
        for i in 0..10 { resp.push_str(&format!("READ f{i}.mjs\n")); }
        let ops = parse_tools(&resp, false);
        assert_eq!(ops.len(), MAX_TOOLS_PER_ROUND);
    }

    // ── exec_tool ────────────────────────────────────────────────────────
    #[test]
    fn exec_tool_read_list_grep_write_roundtrip() {
        let td = std::env::temp_dir().join(format!("repl-direct-exec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&td);
        std::fs::create_dir_all(td.join("dir")).unwrap();
        std::fs::write(td.join("dir/hello.mjs"), "line one\nline two needle\nline three").unwrap();
        let root = td.to_string_lossy().into_owned();

        let read_op = ToolOp { verb: "READ".into(), arg: "dir/hello.mjs".into(), content: None };
        let r = exec_tool(&root, &[], &read_op, false);
        assert!(r.contains("line two needle"));

        let list_op = ToolOp { verb: "LIST".into(), arg: "dir".into(), content: None };
        let l = exec_tool(&root, &[], &list_op, false);
        assert!(l.contains("hello.mjs"));

        let grep_op = ToolOp { verb: "GREP".into(), arg: "needle".into(), content: None };
        let g = exec_tool(&root, &[], &grep_op, false);
        assert!(g.contains("dir/hello.mjs:2:"));

        let write_op = ToolOp { verb: "WRITE".into(), arg: "dir/new.mjs".into(), content: Some("hi".into()) };
        let w_denied = exec_tool(&root, &[], &write_op, false);
        assert!(w_denied.contains("denied: read-only"));
        let w_ok = exec_tool(&root, &[], &write_op, true);
        assert!(w_ok.contains("written 2 bytes"));
        assert_eq!(std::fs::read_to_string(td.join("dir/new.mjs")).unwrap(), "hi");

        let out_of_scope = ToolOp { verb: "READ".into(), arg: "../elsewhere.mjs".into(), content: None };
        let denied = exec_tool(&root, &[], &out_of_scope, false);
        assert!(denied.contains("denied: out of scope"));

        let missing = ToolOp { verb: "READ".into(), arg: "nope.mjs".into(), content: None };
        let nf = exec_tool(&root, &[], &missing, false);
        assert!(nf.contains("(not found)"));

        let _ = std::fs::remove_dir_all(&td);
    }

    // ── deferred aiCall boundary ─────────────────────────────────────────
    #[test]
    fn repl_direct_propagates_deferred_ai_call_error_when_no_extra_context() {
        // `brain` schema so `ensurePrompt`'s find-or-register flow (real, ported) succeeds —
        // proving the deterministic prelude runs all the way to — and propagates — the deferred
        // aiCall boundary, exactly like a real JS network failure would (see module doc).
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        let e = replDirect(&json!({ "user_input": "hello" }), &db).unwrap_err();
        assert!(e.contains("ai-call"), "expected deferred-boundary error, got: {e}");
    }

    #[test]
    fn laws_and_memory_blocks_degrade_to_empty_without_attached_schemas() {
        let db = Connection::open_in_memory().unwrap();
        assert_eq!(laws_block(&db), "");
        assert_eq!(memory_block(&db), "");
        assert_eq!(workspace_root(&db), None);
        assert_eq!(atlas_context(&db), "");
    }
}
