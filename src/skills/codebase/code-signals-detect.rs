//! code-signals-detect.rs — twin of skills/codebase/code-signals-detect.mjs. NAMES ARE CONTRACT:
//! `detectFileSignals` (named, no-db, pure) · `codeSignalsDetect` (default, standard db+fs lane,
//! async-over-sync per the playbook's async lane) · `writeFileSignals` (named, db-first helper).
//!
//! The DETERMINISTIC per-file signal detectors (waves B/C/D): SAFETY (secret, swallowed-error,
//! test-only, debug-leftover, regex-risk, a11y), DEBT (todo, unused-import), I18N
//! (hardcoded-strings — a per-file count, not per-string). Graph-level signals (dead-export,
//! orphan-file, clone-pair, bus-factor, co-change) live in the sibling code-signals-graph, not here.
//!
//! `codeSignalsDetect` reads `{root}/{code_nodes.path}` off the REAL filesystem for every atlas
//! file row — same fixture-controllable shape as `stack-facts.rs`: `root` is DB-derived
//! (`input.root ?? getWorkspaceRoot(db)`, the latter an already-converted twin), so a fixture
//! either seeds an active `workspaces` row or passes `root` directly, both pointing at a real
//! on-disk testdata dir (`antiloki-crossing/fixtures/testdata/code-signals-detect-basic/`)
//! committed alongside this unit's fixtures. A file whose row has no matching on-disk file hits
//! the JS `catch { continue; }` — modeled as `Err(_) => continue`, no error surfaced.
//!
//! `context.db` missing → JS throws `err.contextMissing('code-signals-detect', 'db')`. Not
//! modeled: `db: &Connection` is a required, non-nullable reference here — same "guard-throws are
//! not exercised" convention used by every sibling twin. The `scanned % 40 === 0` progress-yield
//! (`context.onProgress` + `setImmediate`) is a pure side effect with zero influence on the
//! return value or `db_after`, so it is not modeled at all.
//!
//! `regexRisk`'s JS source finds "regex literal" spans with a NEGATIVE LOOKAHEAD
//! (`/\/(?![*/]).../g`, so it never starts a match on `//` or `/*`) that the `regex` crate cannot
//! express (no lookaround, gotcha #7). Ported as a manual byte scan: at every `/` whose next
//! char is not `*`/`/` (the lookahead, done by hand), try the anchored body+trailing-`/`+flags
//! regex at that position — same semantics, since the body's own char class already excludes an
//! unescaped `/`, the greedy `+` can never "eat" the terminating slash (no PCRE-style
//! backtracking needed for that boundary).
// QUALITY: 80/100 — 9 detectors ported regex-for-regex (one hand-rolled for the lookahead gotcha);
// SAVEPOINT-wrapped replace-by-category write, first-seen by_kind ordering (gotcha #1).

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn str_qq(v: Option<&Value>) -> String { match v { None|Some(Value::Null)=>String::new(), Some(Value::String(s))=>s.clone(), Some(Value::Bool(b))=>b.to_string(), Some(n@Value::Number(_))=>n.to_string(), Some(o)=>o.to_string() } } // = JS String(v ?? '')

// `const lineOf = (src, idx) => { let n = 1; for (let i = 0; i < idx && i < src.length; i++) if
// (src[i] === '\n') n++; return n; };` — fixtures are ASCII, so a byte-index walk is exact.
fn line_of(src: &str, idx: usize) -> i64 {
    let bound = idx.min(src.len());
    src.as_bytes()[..bound].iter().filter(|&&b| b == b'\n').count() as i64 + 1
}
// `const clip = (s, n = 120) => String(s ?? '').replace(/\s+/g, ' ').trim().slice(0, n);`
fn ws_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\s+").unwrap()) }
fn clip(s: &str, n: usize) -> String { ws_re().replace_all(s, " ").trim().chars().take(n).collect() }
// `const isTestPath = (p) => /(?:^|[\\/])(?:__tests__|__mocks__)[\\/]|\.(?:test|spec)\.[cm]?[jt]sx?$/i.test(String(p));`
fn is_test_path_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)(?:^|[\\/])(?:__tests__|__mocks__)[\\/]|\.(?:test|spec)\.[cm]?[jt]sx?$").unwrap()) }
fn is_test_path(p: &str) -> bool { is_test_path_re().is_match(p) }

// ── SAFETY: secrets. Shape-based, with a placeholder guard so example/env-template lines don't cry wolf.
fn secret_patterns() -> &'static Vec<(Regex, &'static str)> {
    static R: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    R.get_or_init(|| vec![
        (Regex::new(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b").unwrap(), "AWS access key id"),
        (Regex::new(r"-----BEGIN (?:RSA |EC |OPENSSH |DSA |PGP )?PRIVATE KEY-----").unwrap(), "private key block"),
        (Regex::new(r"\bgh[pousr]_[A-Za-z0-9]{36,}\b").unwrap(), "GitHub token"),
        (Regex::new(r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b").unwrap(), "Slack token"),
        (Regex::new(r"\bsk_(?:live|test)_[A-Za-z0-9]{16,}\b").unwrap(), "Stripe secret key"),
        (Regex::new(r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b").unwrap(), "JWT"),
        // assignment shape: (api_key|secret|password|token) = "…20+ non-space chars…" — the catch-all
        (Regex::new(r#"(?i)\b(?:api[_-]?key|secret|password|passwd|access[_-]?token|auth[_-]?token|private[_-]?key)\s*[:=]\s*['"`]([^'"`\s]{16,})['"`]"#).unwrap(), "hardcoded credential"),
    ])
}
fn placeholder_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)^(?:x{3,}|\*{3,}|<|\$\{|your[_-]|changeme|example|placeholder)").unwrap()) }
fn secrets(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for (re, what) in secret_patterns() {
        for caps in re.captures_iter(source) {
            let m0 = caps.get(0).unwrap();
            let hit = caps.get(1).map(|g| g.as_str()).unwrap_or_else(|| m0.as_str()); // m[1] ?? m[0]
            if placeholder_re().is_match(hit) { continue; }
            out.push(json!({ "category": "safety", "kind": "secret", "severity": "high", "line": line_of(source, m0.start()), "detail": format!("{}: {}\u{2026}", what, clip(hit, 24)) }));
        }
    }
    out
}

// ── SAFETY: swallowed errors — empty catch blocks (comment-only counts as empty).
fn catch_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\bcatch\s*(?:\([^)]*\))?\s*\{([\s\S]*?)\}").unwrap()) }
fn line_comment_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"//[^\n]*").unwrap()) }
fn block_comment_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"/\*[\s\S]*?\*/").unwrap()) }
fn swallowed_errors(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for caps in catch_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let body_raw = caps.get(1).map(|g| g.as_str()).unwrap_or("");
        let no_line = line_comment_re().replace_all(body_raw, "");
        let no_comments = block_comment_re().replace_all(&no_line, "");
        if no_comments.trim().is_empty() {
            out.push(json!({ "category": "safety", "kind": "swallowed-error", "severity": "medium", "line": line_of(source, m0.start()), "detail": "empty catch — errors are silently discarded" }));
        }
    }
    out
}

// ── SAFETY: a committed .only test silently disables the rest of the suite in CI.
fn only_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\b(?:describe|it|test|context)\.only\b").unwrap()) }
fn focused_tests(source: &str, path: &str) -> Vec<Value> {
    if !is_test_path(path) { return Vec::new(); }
    only_re().find_iter(source).map(|m| json!({ "category": "safety", "kind": "test-only", "severity": "high", "line": line_of(source, m.start()), "detail": ".only disables every OTHER test in this file under CI" })).collect()
}

// ── SAFETY: debug leftovers in NON-test source — console.log / debugger.
fn debugger_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\bdebugger\b").unwrap()) }
fn console_log_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\bconsole\.(?:log|debug|dir)\s*\(").unwrap()) }
fn word_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"^\w+").unwrap()) }
fn debug_leftovers(source: &str, path: &str) -> Vec<Value> {
    if is_test_path(path) { return Vec::new(); }
    let mut out = Vec::new();
    for m in debugger_re().find_iter(source) {
        out.push(json!({ "category": "safety", "kind": "debug-leftover", "severity": "medium", "line": line_of(source, m.start()), "detail": "debugger statement" }));
    }
    for m in console_log_re().find_iter(source) {
        // `` `console.${source.slice(m.index + 8).match(/^\w+/)?.[0] ?? 'log'}` `` — "console." is
        // exactly 8 chars, so slicing from m.index+8 lands right after the dot.
        let after = &source[(m.start() + 8).min(source.len())..];
        let method = word_re().find(after).map(|w| w.as_str()).unwrap_or("log");
        out.push(json!({ "category": "safety", "kind": "debug-leftover", "severity": "low", "line": line_of(source, m.start()), "detail": format!("console.{method}") }));
    }
    out
}

// ── SAFETY: regex ReDoS risk — a nested quantifier over a group, e.g. (a+)+ / (\d*)* / (x|y)+*.
// See the module doc for why the regex-literal scan is hand-rolled instead of one JS-literal regex.
fn regex_literal_body_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"^(?:\\.|\[(?:\\.|[^\]])*\]|[^/\\\n])+/[gimsuy]*").unwrap()) }
fn nested_quant_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\([^)]*[+*][^)]*\)\s*[+*]").unwrap()) }
fn find_regex_literals(source: &str) -> Vec<(usize, String)> {
    let bytes = source.as_bytes();
    let n = bytes.len();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < n {
        if bytes[i] == b'/' {
            let next = bytes.get(i + 1).copied();
            if next != Some(b'*') && next != Some(b'/') {
                // `(?![*/])` — the lookahead passed; try the body+trailing-slash+flags regex here.
                if let Some(m) = regex_literal_body_re().find(&source[i + 1..]) {
                    let end = i + 1 + m.end();
                    out.push((i, source[i..end].to_string()));
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}
fn regex_risk(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for (idx, body) in find_regex_literals(source) {
        if nested_quant_re().is_match(&body) {
            out.push(json!({ "category": "safety", "kind": "regex-risk", "severity": "medium", "line": line_of(source, idx), "detail": format!("nested quantifier (backtracking risk): {}", clip(&body, 48)) }));
        }
    }
    out
}

// ── SAFETY: a11y statics — <img> with no alt, a click handler on a non-interactive element.
fn jsx_ext_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\.[jt]sx$").unwrap()) }
fn img_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"<img\b((?:[^>]|\n)*?)>").unwrap()) }
fn alt_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\balt\s*=").unwrap()) }
fn onclick_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"<(div|span)\b((?:[^>]|\n)*?)\bonClick\b").unwrap()) }
fn role_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\brole\s*=").unwrap()) }
fn a11y(source: &str, path: &str) -> Vec<Value> {
    if !jsx_ext_re().is_match(path) { return Vec::new(); }
    let mut out = Vec::new();
    for caps in img_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let attrs = caps.get(1).map(|g| g.as_str()).unwrap_or("");
        if !alt_re().is_match(attrs) {
            out.push(json!({ "category": "safety", "kind": "a11y", "severity": "low", "line": line_of(source, m0.start()), "detail": "<img> without alt" }));
        }
    }
    for caps in onclick_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let tag = caps.get(1).map(|g| g.as_str()).unwrap_or("");
        let attrs = caps.get(2).map(|g| g.as_str()).unwrap_or("");
        if !role_re().is_match(attrs) {
            out.push(json!({ "category": "safety", "kind": "a11y", "severity": "low", "line": line_of(source, m0.start()), "detail": format!("onClick on <{tag}> with no role (keyboard-inaccessible)") }));
        }
    }
    out
}

// ── DEBT: TODO / FIXME / HACK / XXX register.
fn todo_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)(?://|/\*|\*|#)\s*(TODO|FIXME|HACK|XXX)\b:?\s*([^\n*]*)").unwrap()) }
fn todos(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for caps in todo_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let word = caps.get(1).map(|g| g.as_str()).unwrap_or("").to_uppercase();
        let rest = caps.get(2).map(|g| g.as_str()).unwrap_or("");
        let severity = if word == "FIXME" { "medium" } else { "low" };
        out.push(json!({ "category": "debt", "kind": "todo", "severity": severity, "line": line_of(source, m0.start()), "detail": format!("{word}: {}", clip(rest, 80)) }));
    }
    out
}

// ── DEBT: unused imports — an imported binding never referenced again in the file.
fn import_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r#"(?m)^\s*import\s+(?:type\s+)?(?:([A-Za-z_$][\w$]*)\s*,?\s*)?(?:\{([^}]*)\})?\s*(?:\*\s*as\s*([A-Za-z_$][\w$]*))?\s*from\s*['"][^'"]+['"]"#).unwrap()) }
fn type_prefix_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"^type\s+").unwrap()) }
fn split_as_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\s+as\s+").unwrap()) }
fn unused_imports(source: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for caps in import_re().captures_iter(source) {
        let m0 = caps.get(0).unwrap();
        let mut names: Vec<String> = Vec::new();
        if let Some(g1) = caps.get(1) { names.push(g1.as_str().to_string()); }
        if let Some(g3) = caps.get(3) { names.push(g3.as_str().to_string()); }
        if let Some(g2) = caps.get(2) {
            for part in g2.as_str().split(',') {
                let trimmed = part.trim();
                let no_type = type_prefix_re().replace(trimmed, "");
                if let Some(nm) = split_as_re().split(&no_type).last() {
                    let nm = nm.trim();
                    if !nm.is_empty() { names.push(nm.to_string()); }
                }
            }
        }
        let after = &source[m0.end()..];
        for nm in &names {
            let used_re = Regex::new(&format!(r"\b{}\b", regex::escape(nm))).unwrap();
            if !used_re.is_match(after) {
                out.push(json!({ "category": "debt", "kind": "unused-import", "severity": "low", "line": line_of(source, m0.start()), "detail": format!("'{nm}' imported but never used") }));
            }
        }
    }
    out
}

// ── I18N: user-facing string literals directly inside JSX text — a per-file COUNT (not per-string).
fn jsx_text_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r">\s*([A-Z][A-Za-z][^<>{}\n]{2,})<").unwrap()) }
fn blank_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"^\s*$").unwrap()) }
fn lower_space_re() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"[a-z]\s").unwrap()) }
fn i18n_strings(source: &str, path: &str) -> Vec<Value> {
    if !jsx_ext_re().is_match(path) { return Vec::new(); }
    let mut count = 0i64;
    for caps in jsx_text_re().captures_iter(source) {
        let text = caps.get(1).map(|g| g.as_str()).unwrap_or("");
        if !blank_re().is_match(text) && lower_space_re().is_match(text) { count += 1; }
    }
    if count >= 5 {
        vec![json!({ "category": "i18n", "kind": "hardcoded-strings", "severity": "low", "line": 1, "detail": format!("{count} user-facing string literals in JSX (not localized)") })]
    } else {
        Vec::new()
    }
}

/// `export function detectFileSignals(source, path)` — every detector over one file → the
/// combined signal list. `path` is merged onto each finding UNCOERCED (the raw arg value), while
/// the detectors themselves see `String(path ?? '')` — same split the JS source makes.
pub fn detectFileSignals(source: &Value, path: &Value) -> Value {
    let s = str_qq(Some(source));
    let p = str_qq(Some(path));
    let found: Vec<Vec<Value>> = vec![
        secrets(&s), swallowed_errors(&s), focused_tests(&s, &p), debug_leftovers(&s, &p),
        regex_risk(&s), a11y(&s, &p), todos(&s), unused_imports(&s), i18n_strings(&s, &p),
    ];
    let mut out: Vec<Value> = Vec::new();
    for list in found {
        for f in list {
            let mut obj = f.as_object().cloned().unwrap_or_default();
            obj.insert("path".into(), path.clone()); // `{ ...f, path }` — path wasn't a key of f, so it's appended
            out.push(Value::Object(obj));
        }
    }
    json!(out)
}

/// `export default async function codeSignalsDetect(input, context)` — standalone sweep: read
/// every atlas file, detect, write. Async-over-sync (playbook's async lane): every `await` here
/// is over a sync SQLite call or a progress-yield with no return-value effect, so the twin is an
/// ordinary sync `pub fn`.
pub fn codeSignalsDetect(input: &Value, db: &Connection) -> Result<Value, String> {
    let origin = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));
    let root_v = match nz(input.get("root")) {
        Some(r) => r.clone(),
        None => crate::engine_workspace_root::getWorkspaceRoot(db),
    };
    let root = root_v.as_str().unwrap_or("");

    let files = crate::db_json::query_rows(db, "SELECT path FROM code_nodes WHERE origin = ? AND type = 'file'", &[origin.clone()])?;
    let mut rows: Vec<Value> = Vec::new();
    for f in files.as_array().cloned().unwrap_or_default() {
        let path_v = f.get("path").cloned().unwrap_or(Value::Null);
        let path_str = path_v.as_str().unwrap_or("");
        let full = std::path::Path::new(root).join(path_str);
        let src = match std::fs::read_to_string(&full) { Ok(s) => s, Err(_) => continue }; // `catch { continue; }`
        if let Some(arr) = detectFileSignals(&json!(src), &path_v).as_array() {
            for sig in arr { rows.push(sig.clone()); }
        }
    }

    Ok(writeFileSignals(db, &origin, &json!(rows)))
}

/// `export function writeFileSignals(db, origin, rows)` — replace the FILE categories only
/// (safety/debt/i18n) for `origin`, in one SAVEPOINT; a graph-signal pass writing later (or an
/// absent `code_signals` table pre-migration) isn't affected — any write failure is swallowed
/// (JS `catch {}`) and the summary is still computed + returned from the full `rows` list.
pub fn writeFileSignals(db: &Connection, origin: &Value, rows: &Value) -> Value {
    let rows_arr: Vec<Value> = rows.as_array().cloned().unwrap_or_default();

    let write = || -> Result<(), String> {
        db.execute_batch("SAVEPOINT sig_file").map_err(|e| e.to_string())?;
        crate::db_json::run_write(db, "DELETE FROM code_signals WHERE origin = ? AND category IN ('safety','debt','i18n')", &[origin.clone()])?;
        for r in &rows_arr {
            let severity = nz(r.get("severity")).cloned().unwrap_or(json!("low"));
            let line = nz(r.get("line")).cloned().unwrap_or(Value::Null);
            let detail = nz(r.get("detail")).cloned().unwrap_or(Value::Null);
            crate::db_json::run_write(
                db,
                "INSERT INTO code_signals (origin, category, kind, severity, path, line, detail) VALUES (?,?,?,?,?,?,?)",
                &[
                    origin.clone(),
                    r.get("category").cloned().unwrap_or(Value::Null),
                    r.get("kind").cloned().unwrap_or(Value::Null),
                    severity,
                    r.get("path").cloned().unwrap_or(Value::Null),
                    line,
                    detail,
                ],
            )?;
        }
        db.execute_batch("RELEASE sig_file").map_err(|e| e.to_string())?;
        Ok(())
    };
    let _ = write(); // JS `try { ... } catch { /* table absent → signals unavailable, rebuild still fine */ }`

    let mut by_kind = Map::new(); // first-seen kind order (gotcha #1) — same as JS `by_kind[r.kind] = (by_kind[r.kind] ?? 0) + 1`
    for r in &rows_arr {
        let k = str_qq(r.get("kind"));
        let next = by_kind.get(&k).and_then(Value::as_i64).unwrap_or(0) + 1;
        by_kind.insert(k, json!(next));
    }
    json!({ "origin": origin.clone(), "total": rows_arr.len() as i64, "by_kind": Value::Object(by_kind) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_real_shapes_fire_placeholders_do_not() {
        let s = detectFileSignals(
            &json!("const k = 'AKIAIOSFODNN7EXAMPLE';\nconst pk = '-----BEGIN PRIVATE KEY-----';\nconst apiKey = 'sk_live_51H8xYzAbCdEf0123';\nconst ok = 'password = xxxxxxxxxxxxxxxxx';\nconst env = process.env.TOKEN;"),
            &json!("a.js"),
        );
        let arr = s.as_array().unwrap();
        assert!(arr.iter().any(|x| x["kind"] == json!("secret") && x["detail"].as_str().unwrap().contains("private key")));
        assert!(arr.iter().any(|x| x["kind"] == json!("secret") && x["detail"].as_str().unwrap().contains("Stripe")));
        assert!(!arr.iter().any(|x| x["kind"] == json!("secret") && x["detail"].as_str().unwrap().contains("xxxxxxx")));
    }

    #[test]
    fn swallowed_error_empty_and_comment_only_fire_a_real_catch_does_not() {
        let s = detectFileSignals(&json!("try { a(); } catch {}\ntry { b(); } catch (e) { /* ignore */ }\ntry { c(); } catch (e) { log(e); }"), &json!("a.js"));
        let count = s.as_array().unwrap().iter().filter(|x| x["kind"] == json!("swallowed-error")).count();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_only_fires_only_in_test_files_debug_leftovers_only_in_non_test() {
        let a = detectFileSignals(&json!("describe.only(\"x\", () => {})"), &json!("a.test.js"));
        assert!(a.as_array().unwrap().iter().any(|x| x["kind"] == json!("test-only")));
        let b = detectFileSignals(&json!("describe.only(\"x\", () => {})"), &json!("a.js"));
        assert!(!b.as_array().unwrap().iter().any(|x| x["kind"] == json!("test-only")));
        let c = detectFileSignals(&json!("console.log(1); debugger;"), &json!("a.js"));
        assert!(c.as_array().unwrap().iter().any(|x| x["kind"] == json!("debug-leftover")));
        let d = detectFileSignals(&json!("console.log(1)"), &json!("a.test.js"));
        assert!(!d.as_array().unwrap().iter().any(|x| x["kind"] == json!("debug-leftover")));
    }

    #[test]
    fn regex_risk_nested_quantifier_flagged_a_plain_regex_is_not() {
        let a = detectFileSignals(&json!("const re = /(a+)+$/;"), &json!("a.js"));
        assert!(a.as_array().unwrap().iter().any(|x| x["kind"] == json!("regex-risk")));
        let b = detectFileSignals(&json!("const re = /^[a-z]+$/;"), &json!("a.js"));
        assert!(!b.as_array().unwrap().iter().any(|x| x["kind"] == json!("regex-risk")));
    }

    #[test]
    fn a11y_img_without_alt_and_onclick_on_a_div() {
        let s = detectFileSignals(&json!("export const C = () => <div onClick={f}><img src=\"x\" /></div>;"), &json!("c.tsx"));
        let arr = s.as_array().unwrap();
        assert!(arr.iter().any(|x| x["kind"] == json!("a11y") && x["detail"].as_str().unwrap().contains("img") && x["detail"].as_str().unwrap().contains("alt")));
        assert!(arr.iter().any(|x| x["kind"] == json!("a11y") && x["detail"].as_str().unwrap().contains("onClick on <div>")));
    }

    #[test]
    fn todo_unused_import_and_i18n() {
        let a = detectFileSignals(&json!("// TODO: fix this\n// FIXME later"), &json!("a.js"));
        assert!(a.as_array().unwrap().iter().any(|x| x["kind"] == json!("todo")));
        let b = detectFileSignals(&json!("import { used, dead } from './x';\nconsole.log(used);"), &json!("a.js"));
        assert!(b.as_array().unwrap().iter().any(|x| x["kind"] == json!("unused-import") && x["detail"].as_str().unwrap().contains("'dead'")));
        let jsx = format!("export const P = () => <div>{}</div>;", (0..6).map(|i| format!("<span>Hello there number {i} world</span>")).collect::<String>());
        let c = detectFileSignals(&json!(jsx), &json!("p.tsx"));
        assert!(c.as_array().unwrap().iter().any(|x| x["kind"] == json!("hardcoded-strings")));
    }

    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, \
             root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, \
             created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00'); \
             CREATE TABLE code_nodes (id TEXT, origin TEXT NOT NULL, path TEXT NOT NULL, type TEXT NOT NULL); \
             CREATE TABLE code_signals (origin TEXT, category TEXT, kind TEXT, severity TEXT, path TEXT, line INTEGER, detail TEXT);",
        ).unwrap();
        c
    }

    #[test]
    fn code_signals_detect_reads_real_files_skips_missing_and_replaces_file_categories() {
        let c = db();
        const ROOT: &str = concat!(env!("ANTILOKI_FIXTURES"), "/code-signals-detect-basic");
        c.execute("INSERT INTO workspaces (name, root_path, is_active) VALUES ('w', ?1, 1)", rusqlite::params![ROOT]).unwrap();
        for (id, path) in [("n1", "src/mixed.js"), ("n2", "src/mixed.test.js"), ("n3", "src/regex-risk.js"), ("n4", "src/component.tsx"), ("n5", "src/missing.js")] {
            c.execute("INSERT INTO code_nodes (id, origin, path, type) VALUES (?1, 'atlas', ?2, 'file')", rusqlite::params![id, path]).unwrap();
        }
        c.execute("INSERT INTO code_signals (origin, category, kind, severity, path, line, detail) VALUES ('atlas','safety','stale','high','gone.js',1,'old')", []).unwrap();
        c.execute("INSERT INTO code_signals (origin, category, kind, severity, path, line, detail) VALUES ('atlas','history','bus-factor','low','keep.js',1,'graph category')", []).unwrap();

        let r = codeSignalsDetect(&json!({}), &c).unwrap();
        assert_eq!(r["origin"], json!("atlas"));
        assert!(r["total"].as_i64().unwrap() > 0); // real signals found across the 4 readable files
        assert!(r["by_kind"].as_object().unwrap().contains_key("secret"));

        let remaining = crate::db_json::query_rows(&c, "SELECT category, kind FROM code_signals WHERE origin='atlas' AND category='safety' AND kind='stale'", &[]).unwrap();
        assert_eq!(remaining.as_array().unwrap().len(), 0); // stale safety row replaced
        let kept = crate::db_json::query_rows(&c, "SELECT category FROM code_signals WHERE origin='atlas' AND category='history'", &[]).unwrap();
        assert_eq!(kept.as_array().unwrap().len(), 1); // graph category untouched
    }

    #[test]
    fn code_signals_detect_root_and_origin_can_be_passed_directly_no_matches_is_a_clean_zero() {
        let c = db();
        const ROOT: &str = concat!(env!("ANTILOKI_FIXTURES"), "/code-signals-detect-basic");
        c.execute("INSERT INTO code_nodes (id, origin, path, type) VALUES ('n1','custom:1','src/mixed.js','file')", []).unwrap();
        let r = codeSignalsDetect(&json!({ "origin": "custom:1", "root": ROOT }), &c).unwrap();
        assert_eq!(r["origin"], json!("custom:1"));
        assert!(r["total"].as_i64().unwrap() > 0);

        let empty = codeSignalsDetect(&json!({ "origin": "no-such-origin", "root": ROOT }), &c).unwrap();
        assert_eq!(empty, json!({ "origin": "no-such-origin", "total": 0, "by_kind": {} }));
    }

    #[test]
    fn write_file_signals_replaces_by_category_defaults_severity_and_line_first_seen_by_kind_order() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_signals (origin TEXT, category TEXT, kind TEXT, severity TEXT, path TEXT, line INTEGER, detail TEXT)").unwrap();
        c.execute("INSERT INTO code_signals (origin, category, kind, severity, path, line, detail) VALUES ('atlas','safety','old-secret','high','old.js',1,'stale')", []).unwrap();
        c.execute("INSERT INTO code_signals (origin, category, kind, severity, path, line, detail) VALUES ('atlas','history','bus-factor','low','old.js',2,'keep me')", []).unwrap();

        let rows = json!([
            { "category": "safety", "kind": "secret", "severity": "high", "path": "a.js", "line": 3, "detail": "AWS key" },
            { "category": "debt", "kind": "todo", "path": "b.js", "line": 1, "detail": "TODO: x" },
            { "category": "debt", "kind": "todo", "path": "b.js", "line": 5, "detail": "TODO: y" },
            { "category": "safety", "kind": "secret", "path": "c.js", "detail": "no severity or line" },
        ]);
        let r = writeFileSignals(&c, &json!("atlas"), &rows);
        assert_eq!(r, json!({ "origin": "atlas", "total": 4, "by_kind": { "secret": 2, "todo": 2 } }));

        // 1 history row survived (not safety/debt/i18n) + the 4 freshly-inserted rows; the stale
        // safety row was deleted by category, and `history` never touched by this write.
        let survivors = crate::db_json::query_rows(&c, "SELECT * FROM code_signals WHERE origin='atlas'", &[]).unwrap().as_array().unwrap().len();
        assert_eq!(survivors, 5);
        let history_intact = crate::db_json::query_rows(&c, "SELECT * FROM code_signals WHERE origin='atlas' AND category='history'", &[]).unwrap();
        assert_eq!(history_intact.as_array().unwrap().len(), 1);
        let stale_gone = crate::db_json::query_rows(&c, "SELECT * FROM code_signals WHERE origin='atlas' AND kind='old-secret'", &[]).unwrap();
        assert_eq!(stale_gone.as_array().unwrap().len(), 0);
        let c_row = crate::db_json::query_rows(&c, "SELECT severity, line FROM code_signals WHERE path='c.js'", &[]).unwrap();
        let c_row = &c_row.as_array().unwrap()[0];
        assert_eq!(c_row["severity"], json!("low")); // r.severity ?? 'low'
        assert_eq!(c_row["line"], Value::Null); // r.line ?? null
    }

    #[test]
    fn write_file_signals_empty_rows_is_a_clean_zero_and_scoped_by_origin() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE code_signals (origin TEXT, category TEXT, kind TEXT, severity TEXT, path TEXT, line INTEGER, detail TEXT)").unwrap();
        let r = writeFileSignals(&c, &json!("atlas"), &json!([]));
        assert_eq!(r, json!({ "origin": "atlas", "total": 0, "by_kind": {} }));

        c.execute("INSERT INTO code_signals (origin, category, kind, severity, path, line, detail) VALUES ('other','safety','x','low','z.js',1,'different origin')", []).unwrap();
        let r2 = writeFileSignals(&c, &json!("atlas"), &json!([{ "category": "i18n", "kind": "hardcoded-strings", "severity": "low", "path": "p.tsx", "line": 1, "detail": "5 strings" }]));
        assert_eq!(r2["total"], json!(1));
        let other_survives = crate::db_json::query_rows(&c, "SELECT * FROM code_signals WHERE origin='other'", &[]).unwrap();
        assert_eq!(other_survives.as_array().unwrap().len(), 1);
    }
}
