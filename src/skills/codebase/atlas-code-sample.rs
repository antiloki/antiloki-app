//! atlas-code-sample.rs — twin of skills/codebase/atlas-code-sample.mjs (db lane, cross-db
//! brain.* via the prompt-find/prompt-add twins; FS lane via the codebase-file-read twin). NAMES
//! ARE CONTRACT: default `atlasCodeSample`. Picks target files (explicit path → named-in-ask
//! fuzzy match → most-depended-on spread), reads their real source, and asks a strong-tier model
//! for a LIST of concrete before/after improvements grounded in that source.
//!
//! `context.db` missing → JS returns `{ ok:false, error:'no db' }` (NOT a throw) — unreachable
//! here since `db: &Connection` can't literally be absent in Rust. `context.provider_fn` (a JS
//! function reference for provider selection/DI) has no Rust equivalent to carry — threaded as
//! `null` into the constructed aiCall input, same convention as book-run.rs's/pl-research.rs's/
//! atlas-architecture-diagram.rs's `provider_fn`.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`. Unlike pl-research.rs/
//! atlas-architecture-diagram.rs (which propagate that `Err` with `?`), the JS here wraps the
//! `await aiCall(...)` in its own `try { … } catch (e) { return { ok:false, error: … } }` — so
//! this twin mirrors that EXACT shape: an `aiCall` failure is caught and turned into
//! `Ok(json!({"ok":false,"error":<msg, first 160 chars>}))`, not propagated as `Err`. That means
//! `atlasCodeSample` itself ALWAYS resolves to this ok:false branch (the deterministic surface
//! above it — origin/ask defaulting, file selection via `pick_target_files`/`resolve_from_ask`,
//! `codebase_file_read` reads, `files_block` assembly, `ensure_prompt` — all genuinely runs first
//! and is fixture-reachable up to that point). The response-shaping that runs AFTER a real
//! `aiCall` (`parse_payload`, per-item filter/map incl. `lang_of`/`norm_cat`, deterministic
//! summary synthesis, the diagram-warranted check + `atlas_architecture_diagram` dispatch) is
//! unreachable from this file's own fixture but is ported faithfully anyway per the crossing
//! playbook's coverage rule, as its own pure/unit-tested functions — same pattern as
//! pl-research.rs's `build_research_report`/`build_sources`.
// QUALITY: mixed unit — deterministic guard/selection/assembly/prompt-ensure surface (fully
// ported, fixture-reachable up to the deferred-but-caught aiCall boundary) + 5 pure response-
// shaping helpers (unreachable from the default export, unit-tested directly) + 1 bespoke/
// deferred aiCall dispatch (no fixture/frontier) + 1 best-effort nested-skill dispatch
// (atlas_architecture_diagram, also unreachable, also deferred-downstream).
use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
} // = JS String(v ?? '')

const LANG_EXT_PAIRS: &[(&str, &str)] = &[
    ("js", "javascript"), ("jsx", "javascript"), ("mjs", "javascript"), ("cjs", "javascript"),
    ("ts", "typescript"), ("tsx", "typescript"), ("py", "python"), ("go", "go"), ("rb", "ruby"),
    ("java", "java"), ("php", "php"), ("cs", "csharp"), ("rs", "rust"), ("c", "c"), ("h", "c"),
    ("cpp", "cpp"), ("css", "css"), ("scss", "scss"), ("html", "html"), ("json", "json"),
    ("md", "markdown"), ("sql", "sql"),
];

// `LANG_BY_EXT[p.split('.').pop()?.toLowerCase()] ?? 'text'`
fn lang_of(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    LANG_EXT_PAIRS.iter().find(|(k, _)| *k == ext).map(|(_, v)| v.to_string()).unwrap_or_else(|| "text".to_string())
}

const STOP: &[&str] = &[
    "code", "sample", "samples", "example", "examples", "snippet", "before", "after", "improve",
    "improvement", "improvements", "refactor", "rewrite", "suggest", "show", "would", "like", "what", "this", "that",
    "file", "files", "function", "method", "class", "module", "project", "change", "better", "here", "with", "your",
    "about", "some", "make", "give", "specific", "could", "should", "them", "they", "have",
];

// Mirrors the JS candidate-gathering: file-ish tokens, then path-ish tokens, then bare
// identifiers (STOP-filtered) — de-duped case-insensitively (first occurrence wins), longest first.
fn ask_candidates(ask: &str) -> Vec<String> {
    let mut cands: Vec<String> = Vec::new();

    // [A-Za-z0-9_./-]*[A-Za-z0-9_]\.(ext)\b — greedy file-name-with-extension tokens.
    let exts = ["js", "jsx", "ts", "tsx", "mjs", "cjs", "py", "go", "rb", "java", "php", "cs", "rs", "c", "cpp", "css", "scss", "html", "json", "sql"];
    let chars: Vec<char> = ask.chars().collect();
    let is_tok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-');
    let mut i = 0usize;
    while i < chars.len() {
        if is_tok(chars[i]) {
            let start = i;
            while i < chars.len() && is_tok(chars[i]) { i += 1; }
            let tok: String = chars[start..i].iter().collect();
            let lower = tok.to_lowercase();
            for e in exts {
                let suffix = format!(".{e}");
                if lower.ends_with(&suffix) {
                    let before_ok = tok.chars().nth(tok.chars().count().saturating_sub(suffix.chars().count() + 1))
                        .map(|c| c.is_ascii_alphanumeric() || c == '_').unwrap_or(true);
                    if before_ok { cands.push(tok.clone()); }
                    break;
                }
            }
        } else {
            i += 1;
        }
    }

    // \b[A-Za-z0-9_-]+\/[A-Za-z0-9_./-]+ — path-ish tokens (contain a slash).
    let is_path_ch = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '-');
    let mut j = 0usize;
    while j < chars.len() {
        if is_path_ch(chars[j]) {
            let start = j;
            while j < chars.len() && is_path_ch(chars[j]) { j += 1; }
            let tok: String = chars[start..j].iter().collect();
            if tok.contains('/') { cands.push(tok); }
        } else {
            j += 1;
        }
    }

    // \b[A-Za-z][A-Za-z0-9_-]{3,}\b — bare identifiers, len >= 4, STOP-filtered.
    let is_id_ch = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    let mut k = 0usize;
    while k < chars.len() {
        if chars[k].is_ascii_alphabetic() {
            let start = k;
            let mut end = k + 1;
            while end < chars.len() && is_id_ch(chars[end]) { end += 1; }
            let tok: String = chars[start..end].iter().collect();
            if tok.chars().count() >= 4 && !STOP.contains(&tok.to_lowercase().as_str()) { cands.push(tok); }
            k = end;
        } else {
            k += 1;
        }
    }

    let mut seen = std::collections::HashSet::new();
    let mut ordered: Vec<String> = cands.into_iter().filter(|c| seen.insert(c.to_lowercase())).collect();
    ordered.sort_by(|a, b| b.chars().count().cmp(&a.chars().count()));
    ordered
}

struct FileHit { path: String }

fn resolve_from_ask(db: &Connection, origin: &str, ask: Option<&str>) -> Result<Option<FileHit>, String> {
    let ask = match ask { Some(a) if !a.is_empty() => a, _ => return Ok(None) };
    for c in ask_candidates(ask) {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT n.path, n.intent,
                    (SELECT COUNT(*) FROM code.code_edges e WHERE e.origin=n.origin AND e.type='imports' AND e.to_node = n.path) AS deps
             FROM code.code_nodes n WHERE n.origin=? AND n.type='file' AND n.path LIKE ?
             ORDER BY deps DESC, length(n.path) ASC LIMIT 1",
            &[json!(origin), json!(format!("%{c}%"))],
        )?;
        if let Some(row) = rows.as_array().and_then(|a| a.first()) {
            if let Some(p) = row.get("path").and_then(Value::as_str) {
                return Ok(Some(FileHit { path: p.to_string() }));
            }
        }
    }
    Ok(None)
}

fn pick_target_files(db: &Connection, origin: &str, explicit_path: Option<&str>, ask: Option<&str>) -> Result<Vec<String>, String> {
    if let Some(ep) = explicit_path {
        let rows = crate::db_json::query_rows(
            db,
            "SELECT path FROM code.code_nodes WHERE origin=? AND type='file' AND path=? LIMIT 1",
            &[json!(origin), json!(ep)],
        )?;
        if let Some(row) = rows.as_array().and_then(|a| a.first()) {
            if let Some(p) = row.get("path").and_then(Value::as_str) {
                return Ok(vec![p.to_string()]);
            }
        }
    }
    if let Some(hit) = resolve_from_ask(db, origin, ask)? {
        return Ok(vec![hit.path]);
    }
    let top = crate::db_json::query_rows(
        db,
        "SELECT to_node, COUNT(*) n FROM code.code_edges WHERE origin=? AND type='imports' GROUP BY to_node ORDER BY n DESC LIMIT 12",
        &[json!(origin)],
    )?;
    let mut out: Vec<String> = Vec::new();
    if let Some(rows) = top.as_array() {
        for r in rows {
            let to_node = str_qq(r.get("to_node"));
            let p = to_node.split('#').next().unwrap_or("").to_string();
            if out.contains(&p) { continue; }
            let found = crate::db_json::query_rows(
                db,
                "SELECT path FROM code.code_nodes WHERE origin=? AND type='file' AND path=? LIMIT 1",
                &[json!(origin), json!(p.clone())],
            )?;
            if let Some(row) = found.as_array().and_then(|a| a.first()) {
                if let Some(pp) = row.get("path").and_then(Value::as_str) { out.push(pp.to_string()); }
            }
            if out.len() >= 4 { break; }
        }
    }
    if !out.is_empty() { return Ok(out); }
    let fallback = crate::db_json::query_rows(
        db,
        "SELECT path FROM code.code_nodes WHERE origin=? AND type='file' ORDER BY id LIMIT 4",
        &[json!(origin)],
    )?;
    Ok(fallback.as_array().unwrap_or(&vec![]).iter().filter_map(|r| r.get("path").and_then(Value::as_str).map(|s| s.to_string())).collect())
}

const PROMPT_NAME: &str = "atlas.code.sample.v4";
const PROMPT_SYSTEM_MD: &str = "You are a senior engineer reviewing real files and proposing a thorough LIST of concrete improvements — each shown as a before/after a developer can read at a glance. You output only strict JSON. For each improvement: \"file\" MUST be exactly one of the provided file paths; \"before\" MUST be a contiguous excerpt copied VERBATIM from THAT file's source (the exact lines you will improve, not a paraphrase); \"after\" is your improved version of that same excerpt. Each change must be focused and defensible. CLASSIFY each improvement's \"category\": \"data-flow\" (changes how data moves or is transformed), \"structure\" (restructures modules / responsibilities / state), \"logic\" (changes behaviour or algorithm / fixes a bug), or \"standard\" (style / readability / naming / imports — cosmetic). PRIORITIZE substantive improvements (correctness, error handling, performance, data-flow, structure) and LEAD with them; reserve \"standard\" for genuinely cosmetic changes and do not pad the list with trivial restyling. Ground everything in the real source; invent no APIs. No preamble.";
const PROMPT_TEMPLATE_MD: &str = "Review these real files and propose a LIST of 5-7 concrete, high-value improvements — each as a before/after sample a developer can read at a glance. Lead with the most substantive ones (correctness, error handling, performance, data-flow, structure / responsibilities) and only then cosmetic ones — don't pad the list with trivial restyling. Spread them across the files where it makes sense (more than one per file is fine). Do NOT rewrite whole files — each \"before\" is a small contiguous excerpt.

USER ASK: {{ask}}

THE FILES (improve contiguous excerpts of these — copy them VERBATIM into each \"before\"):
{{files}}

Return JSON only, exactly:
{\"summary\":\"<2-3 sentences framing the set as a whole — the themes and the single highest-impact change>\",\"improvements\":[{\"file\":\"<one of the file paths above>\",\"target\":\"<short name of what you improve>\",\"category\":\"data-flow|structure|logic|standard\",\"before\":\"<exact excerpt copied verbatim from that file>\",\"after\":\"<your improved version of that excerpt>\",\"rationale\":\"<2-4 sentences: what changed, why it's better, and any risk>\"}]}";

fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(PROMPT_TEMPLATE_MD) { return Ok(()); }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "system_md": PROMPT_SYSTEM_MD,
            "default_tier": "mid",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 4800,
            "output_fields": [{ "field_name": "improvements_json", "field_type": "string", "required": true, "min_length": 10 }],
        }),
        db,
    )?;
    Ok(())
}

// improvements_json → (summary, improvements[]). aiCall may hand back a JSON array string, an
// object {summary, improvements:[…]}, or (via _json coercion) the array itself — accept all three.
fn parse_payload(res: &Value) -> (Option<String>, Vec<Value>) {
    let raw = res.get("output_kv").and_then(Value::as_array).and_then(|arr| {
        arr.iter().find(|o| o.get("field_name").and_then(Value::as_str) == Some("improvements_json") && o.get("parent_output_id").map_or(true, Value::is_null))
    }).and_then(|o| o.get("value_text")).and_then(Value::as_str);
    let raw = match raw { Some(r) if !r.is_empty() => r, _ => return (None, Vec::new()) };
    let v: Value = match serde_json::from_str(raw) { Ok(v) => v, Err(_) => return (None, Vec::new()) };
    if let Value::Array(a) = v { return (None, a); }
    let summary = v.get("summary").and_then(Value::as_str).map(|s| s.trim()).filter(|s| !s.is_empty()).map(|s| s.to_string());
    let improvements = v.get("improvements").and_then(Value::as_array).cloned().unwrap_or_default();
    (summary, improvements)
}

const CATEGORIES: &[&str] = &["data-flow", "structure", "logic", "standard"];

// `String(c ?? 'standard').toLowerCase().replace(/[_ ]+/g, '-')`, then 'dataflow' → 'data-flow',
// then fall back to 'standard' if not one of the 4 known categories.
fn norm_cat(c: Option<&Value>) -> String {
    let raw = match c { None | Some(Value::Null) => "standard".to_string(), Some(v) => str_qq(Some(v)) };
    let mut s = String::new();
    let mut prev_sep = false;
    for ch in raw.to_lowercase().chars() {
        if ch == '_' || ch == ' ' {
            if !prev_sep { s.push('-'); prev_sep = true; }
        } else {
            s.push(ch);
            prev_sep = false;
        }
    }
    if s == "dataflow" { s = "data-flow".to_string(); }
    if CATEGORIES.contains(&s.as_str()) { s } else { "standard".to_string() }
}

// Pure — filters raw AI-shaped improvement objects to `before && after` present, remaps `file` to
// a known path (falls back to the first file), shapes `{file, language, target, category, before,
// after, rationale}`, caps at 8. Mirrors the JS `.filter().map().slice(0, 8)` chain exactly.
fn build_improvements(parsed: &[Value], known: &std::collections::HashSet<String>, fallback_file: &str) -> Vec<Value> {
    parsed
        .iter()
        .filter(|it| it.get("before").map_or(false, |v| !v.is_null() && v != "") && it.get("after").map_or(false, |v| !v.is_null() && v != ""))
        .map(|it| {
            let file = it.get("file").and_then(Value::as_str).filter(|f| known.contains(*f)).unwrap_or(fallback_file).to_string();
            let target = it.get("target").and_then(Value::as_str).map(|s| s.trim().to_string());
            json!({
                "file": file.clone(),
                "language": lang_of(&file),
                "target": target,
                "category": norm_cat(it.get("category")),
                "before": str_qq(it.get("before")),
                "after": str_qq(it.get("after")),
                "rationale": it.get("rationale").and_then(Value::as_str).map(|s| s.trim().to_string()),
            })
        })
        .take(8)
        .collect()
}

// Pure — deterministic summary synthesis (count · files · category mix) used when the AI omitted
// (or returned blank) `summary`. Markdown OK.
fn synthesize_summary(improvements: &[Value], touched_count: usize) -> String {
    let mut by_cat: Vec<(String, usize)> = Vec::new();
    for it in improvements {
        let cat = it.get("category").and_then(Value::as_str).unwrap_or("standard").to_string();
        if let Some(entry) = by_cat.iter_mut().find(|(c, _)| *c == cat) { entry.1 += 1; } else { by_cat.push((cat, 1)); }
    }
    by_cat.sort_by(|a, b| b.1.cmp(&a.1));
    let cat_str = by_cat.iter().map(|(c, k)| format!("{k} {c}")).collect::<Vec<_>>().join(" · ");
    let n = improvements.len();
    format!(
        "**{n} concrete improvement{}** across **{touched_count} file{}**{}. Expand any item below for its before/after.",
        if n == 1 { "" } else { "s" },
        if touched_count == 1 { "" } else { "s" },
        if cat_str.is_empty() { String::new() } else { format!(" — {cat_str}") },
    )
}

const TRANSFORMATIONAL: &[&str] = &["data-flow", "structure", "logic"];

// Pure — `touched.length >= 2 && improvements.some(i => TRANSFORMATIONAL.has(i.category))`.
fn diagram_warranted(improvements: &[Value], touched_count: usize) -> bool {
    touched_count >= 2 && improvements.iter().any(|i| {
        i.get("category").and_then(Value::as_str).map_or(false, |c| TRANSFORMATIONAL.contains(&c))
    })
}

pub fn atlasCodeSample(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = input.get("origin").and_then(Value::as_str).unwrap_or("atlas").to_string();
    let ask_raw = input.get("ask").and_then(Value::as_str).map(|s| s.to_string())
        .unwrap_or_else(|| "Suggest concrete improvements with before/after samples.".to_string());
    let ask: String = ask_raw.chars().take(600).collect();

    let mut files: Vec<(String, String)> = Vec::new(); // (path, source)
    let input_files = input.get("files").and_then(Value::as_array);
    if input_files.map_or(false, |a| !a.is_empty()) {
        for f in input_files.unwrap() {
            let path = f.get("path").and_then(Value::as_str);
            let source = f.get("source").and_then(Value::as_str);
            if let (Some(p), Some(s)) = (path, source) {
                if !p.is_empty() && !s.is_empty() {
                    files.push((p.to_string(), s.chars().take(4000).collect()));
                }
            }
        }
    } else {
        let targets = pick_target_files(db, &origin, input.get("path").and_then(Value::as_str), Some(ask.as_str()))?;
        if targets.is_empty() {
            return Ok(json!({ "ok": false, "error": "no_atlas: the codebase is not indexed yet — build the Atlas for this workspace first." }));
        }
        for t in targets {
            if let Ok(r) = crate::codebase_codebase_file_read::codebaseFileRead(&json!({ "path": t }), db) {
                let content = r.get("content").and_then(Value::as_str).unwrap_or("");
                if !content.trim().is_empty() {
                    files.push((t, content.chars().take(4000).collect()));
                }
            }
        }
    }
    if files.is_empty() {
        return Ok(json!({ "ok": false, "error": "target files unreadable" }));
    }
    let files_block = files.iter().map(|(p, s)| format!("=== FILE: {p} ===\n{s}")).collect::<Vec<_>>().join("\n\n");

    ensure_prompt(db)?;

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [{ "key": "ask", "value": ask }, { "key": "files", "value": files_block }],
        "tier": "mid", "temperature": "cold", "phase_flow": "single_only", "provider_fn": Value::Null,
    });
    let res = match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(r) => r,
        Err(e) => {
            let msg: String = e.chars().take(160).collect();
            return Ok(json!({ "ok": false, "error": msg }));
        }
    };

    // ── unreachable below this point while aiCall is PARITY-DEFERRED (it always Errs above) —
    // ported faithfully via the pure helpers `parse_payload`/`build_improvements`/
    // `synthesize_summary`/`diagram_warranted`, unit-tested directly.
    let known: std::collections::HashSet<String> = files.iter().map(|(p, _)| p.clone()).collect();
    let (parsed_summary, parsed) = parse_payload(&res);
    let fallback_file = files[0].0.clone();
    let improvements = build_improvements(&parsed, &known, &fallback_file);
    if improvements.is_empty() {
        return Ok(json!({ "ok": false, "error": "AI returned no improvements" }));
    }
    let mut touched: Vec<String> = Vec::new();
    for it in &improvements {
        if let Some(f) = it.get("file").and_then(Value::as_str) {
            if !touched.contains(&f.to_string()) { touched.push(f.to_string()); }
        }
    }
    let summary = parsed_summary.unwrap_or_else(|| synthesize_summary(&improvements, touched.len()));
    let ai_telemetry = vec![crate::ai__helpers_ai_telemetry::telemetryFromResult(&res, db, Some(&json!("code-sample")), Some(&json!(PROMPT_NAME)))];
    let mut diagram = Value::Null;
    if diagram_warranted(&improvements, touched.len()) {
        let files_json: Vec<Value> = files.iter().map(|(p, s)| json!({ "path": p, "source": s })).collect();
        if let Ok(dr) = crate::codebase_atlas_architecture_diagram::atlasArchitectureDiagram(&json!({ "files": files_json, "ask": ask }), db) {
            if dr.get("ok").and_then(Value::as_bool) == Some(true) {
                diagram = dr.get("diagram").cloned().unwrap_or(Value::Null);
            }
        }
    }
    Ok(json!({ "ok": true, "summary": summary, "improvements": improvements, "diagram": diagram, "files": touched, "ai_telemetry": ai_telemetry }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS code; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE code.code_nodes (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, type TEXT NOT NULL, path TEXT NOT NULL, intent TEXT);
             CREATE TABLE code.code_edges (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, type TEXT NOT NULL, to_node TEXT NOT NULL);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE workspaces (root_path TEXT, is_active INTEGER);",
        ).unwrap();
        c
    }

    #[test]
    fn lang_of_maps_extensions_case_insensitively_and_defaults_to_text() {
        assert_eq!(lang_of("src/foo.MJS"), "javascript");
        assert_eq!(lang_of("a/b.rs"), "rust");
        assert_eq!(lang_of("no_ext"), "text");
        assert_eq!(lang_of("weird.xyz"), "text");
    }

    #[test]
    fn norm_cat_normalizes_and_falls_back_to_standard() {
        assert_eq!(norm_cat(Some(&json!("Data_Flow"))), "data-flow");
        assert_eq!(norm_cat(Some(&json!("dataflow"))), "data-flow");
        assert_eq!(norm_cat(Some(&json!("STRUCTURE"))), "structure");
        assert_eq!(norm_cat(None), "standard");
        assert_eq!(norm_cat(Some(&json!("nonsense"))), "standard");
    }

    #[test]
    fn parse_payload_accepts_array_object_and_bad_json() {
        let arr_res = json!({ "output_kv": [{ "field_name": "improvements_json", "value_text": "[{\"file\":\"a\"}]" }] });
        let (s, imp) = parse_payload(&arr_res);
        assert_eq!(s, None);
        assert_eq!(imp.len(), 1);

        let obj_res = json!({ "output_kv": [{ "field_name": "improvements_json", "value_text": "{\"summary\":\" hi \",\"improvements\":[{\"file\":\"a\"},{\"file\":\"b\"}]}" }] });
        let (s2, imp2) = parse_payload(&obj_res);
        assert_eq!(s2, Some("hi".to_string()));
        assert_eq!(imp2.len(), 2);

        let bad_res = json!({ "output_kv": [{ "field_name": "improvements_json", "value_text": "not json" }] });
        let (s3, imp3) = parse_payload(&bad_res);
        assert_eq!(s3, None);
        assert!(imp3.is_empty());

        assert_eq!(parse_payload(&json!({})), (None, Vec::new()));
    }

    #[test]
    fn build_improvements_filters_remaps_unknown_file_and_caps_at_8() {
        let known: std::collections::HashSet<String> = ["a.js".to_string(), "b.py".to_string()].into_iter().collect();
        let mut parsed = Vec::new();
        for i in 0..10 {
            parsed.push(json!({ "file": if i % 2 == 0 { "a.js" } else { "unknown.rb" }, "before": "x", "after": "y", "category": "logic" }));
        }
        parsed.push(json!({ "file": "a.js", "before": "", "after": "y" })); // filtered: empty before
        parsed.push(json!({ "file": "a.js", "after": "y" })); // filtered: missing before
        let out = build_improvements(&parsed, &known, "a.js");
        assert_eq!(out.len(), 8);
        assert_eq!(out[1]["file"], json!("a.js")); // remapped from unknown.rb
        assert_eq!(out[1]["language"], json!("javascript"));
    }

    #[test]
    fn synthesize_summary_counts_categories() {
        let improvements = vec![
            json!({ "category": "logic" }),
            json!({ "category": "logic" }),
            json!({ "category": "standard" }),
        ];
        let s = synthesize_summary(&improvements, 2);
        assert!(s.starts_with("**3 concrete improvements** across **2 files**"));
        assert!(s.contains("2 logic"));
        assert!(s.contains("1 standard"));
    }

    #[test]
    fn diagram_warranted_needs_2_files_and_a_transformational_category() {
        let logic = vec![json!({ "category": "logic" })];
        let standard = vec![json!({ "category": "standard" })];
        assert!(diagram_warranted(&logic, 2));
        assert!(!diagram_warranted(&logic, 1));
        assert!(!diagram_warranted(&standard, 2));
    }

    #[test]
    fn pick_target_files_explicit_path_wins() {
        let c = setup();
        c.execute("INSERT INTO code.code_nodes (origin, type, path, intent) VALUES ('atlas','file','src/a.js', NULL)", []).unwrap();
        c.execute("INSERT INTO code.code_nodes (origin, type, path, intent) VALUES ('atlas','file','src/b.js', NULL)", []).unwrap();
        let out = pick_target_files(&c, "atlas", Some("src/b.js"), None).unwrap();
        assert_eq!(out, vec!["src/b.js".to_string()]);
    }

    #[test]
    fn pick_target_files_falls_back_to_most_depended_on_then_first_4() {
        let c = setup();
        c.execute("INSERT INTO code.code_nodes (origin, type, path, intent) VALUES ('atlas','file','src/a.js', NULL)", []).unwrap();
        c.execute("INSERT INTO code.code_nodes (origin, type, path, intent) VALUES ('atlas','file','src/b.js', NULL)", []).unwrap();
        c.execute("INSERT INTO code.code_edges (origin, type, to_node) VALUES ('atlas','imports','src/a.js')", []).unwrap();
        c.execute("INSERT INTO code.code_edges (origin, type, to_node) VALUES ('atlas','imports','src/a.js')", []).unwrap();
        let out = pick_target_files(&c, "atlas", None, None).unwrap();
        assert_eq!(out, vec!["src/a.js".to_string()]);
    }

    #[test]
    fn reaches_the_deferred_aicall_boundary_and_returns_ok_false_after_full_deterministic_surface() {
        let c = setup();
        c.execute("INSERT INTO code.code_nodes (origin, type, path, intent) VALUES ('atlas','file','src/a.js', NULL)", []).unwrap();
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES ('.', 1)", []).unwrap();
        let res = atlasCodeSample(&json!({ "files": [{ "path": "a.js", "source": "console.log(1)" }] }), &c).unwrap();
        assert_eq!(res["ok"], json!(false));
        // byte len can exceed 160 (em-dash is 3 bytes/1 char) — JS .slice(0,160) counts UTF-16
        // units, which the Rust port mirrors via .chars().take(160); assert char count, not bytes.
        assert!(res["error"].as_str().unwrap().chars().count() <= 160);
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.code.sample.v4'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn no_atlas_when_no_target_files_found() {
        let c = setup();
        let res = atlasCodeSample(&json!({}), &c).unwrap();
        assert_eq!(res["ok"], json!(false));
        assert!(res["error"].as_str().unwrap().starts_with("no_atlas"));
    }
}
