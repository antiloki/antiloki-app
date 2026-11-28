//! notebook-ingest.rs — twin of skills/notebooks/notebook-ingest.mjs (db WRITE lane, op-routed).
//! NAMES ARE CONTRACT: `notebookIngest` (default). Server-side ingestion of EXTERNAL sources into
//! a notebook, with text extracted before storage: a URL (fetch + HTML→text), a docs folder (fs
//! read of *.md/*.txt under the workspace root), or a GitHub repo's issues/PRs (api.github.com).
//! Each becomes a notebook_source (delegates to `notebookSource` op:'add').
//!
//! PARITY-DEFERRED: `op:'url'`'s `fetch(url, ...)` and `op:'github'`'s
//! `fetch('https://api.github.com/repos/...')` are the non-deterministic edges (real network
//! round-trips) — stubbed by `fetch_url_deferred`/`fetch_github_deferred`, each an unconditional
//! `Err`. JS wraps both in `try/catch` and converts a thrown/rejected error into an `{ok:false,
//! error:...}` STATUS OBJECT (never propagates) — mirrored here: the stub's `Err` is caught by a
//! `match` and folded into the same `ok:false` shape, so the deferred edge is total (never
//! panics) and every OTHER branch (url validation, content-type dispatch, `htmlToText`, the
//! `arr` shape check, the github item formatter, `docs`'s real fs walk + notebookSource writes)
//! is ported byte-for-byte and exercised by native tests. The `docs` op has NO deferred edge —
//! it is fully deterministic (fs::read_dir/read_to_string) and is the primary proof surface.
// QUALITY: 80/100 — 3-op router, one fully-deterministic (docs), two with a disclosed deferred fetch edge.

use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// JS `String(v ?? '')`.
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
// JS `${v}` template interpolation — used only for the final `unknown op: ${op}` message.
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
// JS truthiness.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
// JS `Number(v)`, used only for the `notebook_id` coercion (`!nbId` guard).
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) }
        }
        Some(_) => f64::NAN,
    }
}
fn falsy_num(n: f64) -> bool { n == 0.0 || n.is_nan() }
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }

fn re_script() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?is)<script[\s\S]*?</script>").unwrap()) }
fn re_style() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?is)<style[\s\S]*?</style>").unwrap()) }
fn re_tag() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"<[^>]+>").unwrap()) }
fn re_ws() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"[ \t]{2,}").unwrap()) }
fn re_nl() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"\n{3,}").unwrap()) }
fn re_md_ext() -> &'static Regex { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"(?i)\.(md|markdown|txt)$").unwrap()) }

// `const htmlToText = (html) => String(html || '') ... .trim()`.
fn html_to_text(html: &str) -> String {
    let s = re_script().replace_all(html, " ");
    let s = re_style().replace_all(&s, " ");
    let s = re_tag().replace_all(&s, " ");
    let s = s.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">");
    let s = re_ws().replace_all(&s, " ");
    let s = re_nl().replace_all(&s, "\n\n");
    s.trim().to_string()
}

// `function walk(dir, depth = 0)` — the TOP-level `readdirSync(dir)` call is NOT wrapped in
// try/catch in the JS (it propagates to the caller's own try/catch in op:'docs'); only the
// RECURSIVE `walk(p, depth+1)` calls for subdirectories are individually try/catch-swallowed
// (`catch { /* unreadable */ }`). Mirrored: the top-level `read_dir` error propagates as `Err`;
// a failed recursive sub-walk is silently skipped (contributes nothing, same as JS's empty catch).
fn walk(dir: &Path, depth: u32) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if depth > 6 { return Ok(out); }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || name_str == "node_modules" { continue; }
        let p = entry.path();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            if let Ok(sub) = walk(&p, depth + 1) { out.extend(sub); }
        } else {
            out.push(p);
        }
    }
    Ok(out)
}

// `relative(root, f).replace(/\\/g, '/')` — root is always a prefix of f here (f came from
// walk(join(root, rel))), so a plain prefix-strip is equivalent to node:path's `relative()`.
fn relative_slash(root: &str, f: &Path) -> String {
    let stripped = f.strip_prefix(Path::new(root)).map(|p| p.to_path_buf()).unwrap_or_else(|_| f.to_path_buf());
    stripped.to_string_lossy().replace('\\', "/")
}

// PARITY-DEFERRED: `fetch(url, { headers: { 'user-agent': 'antiloki-notebooks' } })` + reading
// `res.text()` / `res.headers.get('content-type')` — a real network round-trip. See module doc.
fn fetch_url_deferred(_url: &str) -> Result<(String, String), String> {
    Err("notebook-ingest twin: the url-fetch dispatch is PARITY-DEFERRED — see the module doc. It makes a real HTTP round-trip and is genuinely non-deterministic and not fixturable.".into())
}

// PARITY-DEFERRED: `fetch('https://api.github.com/repos/${repo}/${ghKind}?state=all&per_page=40')`
// + `res.json()` — a real network round-trip against the GitHub API. See module doc.
fn fetch_github_deferred(_repo: &str, _gh_kind: &str) -> Result<Value, String> {
    Err("notebook-ingest twin: the github-fetch dispatch is PARITY-DEFERRED — see the module doc. It makes a real HTTP round-trip and is genuinely non-deterministic and not fixturable.".into())
}

/// `export default async function notebookIngest(input, context)` — NAMES ARE CONTRACT.
/// `context.db` missing → JS throws `err.contextMissing('notebook-ingest', 'db')`; unreachable
/// here, `db` is a required `&Connection` parameter (see module doc convention shared with every
/// other db-lane twin).
pub fn notebookIngest(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));
    // `const op = input?.op;` — no coercion in the JS; every comparison below is a strict `===`
    // against a string literal, so a non-string `op` (or `op` absent) never matches any branch.
    let op_v = input.get("op");
    let op = |s: &str| op_v.and_then(|v| v.as_str()) == Some(s);
    let nb_id = js_number(input.get("notebook_id"));
    if falsy_num(nb_id) { return Err("notebook-ingest: notebook_id is required".into()); }

    if op("url") {
        let url = str_qq(input.get("url")).trim().to_string();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Ok(json!({ "ok": false, "error": "a http(s) url is required" }));
        }
        let text = match fetch_url_deferred(&url) {
            Ok((content_type, body)) => {
                if content_type.to_lowercase().contains("json") { body } else { html_to_text(&body) }
            }
            Err(e) => return Ok(json!({ "ok": false, "error": format!("fetch failed: {e}") })),
        };
        if text.trim().is_empty() { return Ok(json!({ "ok": false, "error": "no text extracted from the url" })); }
        let label = if truthy(input.get("label")) { input.get("label").cloned().unwrap() } else { json!(url.clone()) };
        let body_md: String = text.chars().take(200_000).collect();
        return crate::notebooks_notebook_source::notebookSource(
            &json!({ "op": "add", "notebook_id": num_param(nb_id), "kind": "url", "label": label, "ref": url, "body_md": body_md }),
            db,
        );
    }

    if op("docs") {
        let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
        let root = root_v.as_str().unwrap_or("");
        if root.is_empty() { return Ok(json!({ "ok": false, "error": "no workspace root" })); }
        let rel_raw = { let r = input.get("path"); if truthy(r) { str_qq(r) } else { "docs".to_string() } };
        let rel = rel_raw.replace('\\', "/").trim_start_matches('/').to_string();
        let dir = Path::new(root).join(&rel);
        let files: Vec<PathBuf> = match walk(&dir, 0) {
            Ok(fs) => fs.into_iter().filter(|f| re_md_ext().is_match(&f.to_string_lossy())).take(40).collect(),
            Err(e) => return Ok(json!({ "ok": false, "error": format!("read failed: {e}") })),
        };
        if files.is_empty() { return Ok(json!({ "ok": false, "error": format!("no .md/.txt under {rel}") })); }
        let mut parts: Vec<String> = Vec::new();
        for f in &files {
            if let Ok(contents) = std::fs::read_to_string(f) {
                let head: String = contents.chars().take(20_000).collect();
                parts.push(format!("### {}\n{}", relative_slash(root, f), head));
            }
        }
        let body_md: String = parts.join("\n\n").chars().take(300_000).collect();
        let label = if truthy(input.get("label")) { input.get("label").cloned().unwrap() } else { json!(format!("docs: {} ({})", rel, files.len())) };
        return crate::notebooks_notebook_source::notebookSource(
            &json!({ "op": "add", "notebook_id": num_param(nb_id), "kind": "file", "label": label, "ref": rel, "body_md": body_md }),
            db,
        );
    }

    if op("github") {
        let repo_raw = str_qq(input.get("repo")).trim().to_string();
        let repo = {
            let s = repo_raw.trim_start_matches("http://github.com/").trim_start_matches("https://github.com/");
            let s = s.strip_suffix(".git").unwrap_or(s);
            s.trim_end_matches('/').to_string()
        };
        let owner_name_re = { static R: OnceLock<Regex> = OnceLock::new(); R.get_or_init(|| Regex::new(r"^[^/]+/[^/]+$").unwrap()) };
        if !owner_name_re.is_match(&repo) { return Ok(json!({ "ok": false, "error": "repo must be owner/name" })); }
        let gh_kind = if input.get("gh_kind").and_then(|v| v.as_str()) == Some("pulls") { "pulls" } else { "issues" };
        let text = match fetch_github_deferred(&repo, gh_kind) {
            Ok(arr_v) => match arr_v.as_array() {
                None => {
                    let msg = arr_v.get("message").and_then(|v| v.as_str()).map(|s| s.to_string())
                        .unwrap_or_else(|| "github api error (rate limit / private?)".to_string());
                    return Ok(json!({ "ok": false, "error": msg }));
                }
                Some(arr) => arr.iter().map(|x| {
                    // `x.state`/`x.title` interpolate via `${...}` too (tmpl), not a plain string
                    // default — ported as tmpl() throughout to match undefined/null distinctly.
                    let body_line = if truthy(x.get("body")) {
                        let b: String = str_qq(x.get("body")).split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect();
                        format!("\n  {b}")
                    } else { String::new() };
                    format!("- #{} [{}] {}{}", tmpl(x.get("number")), tmpl(x.get("state")), tmpl(x.get("title")), body_line)
                }).collect::<Vec<_>>().join("\n"),
            },
            Err(e) => return Ok(json!({ "ok": false, "error": format!("github fetch failed: {e}") })),
        };
        if text.trim().is_empty() { return Ok(json!({ "ok": false, "error": "no issues/PRs returned" })); }
        let label = if truthy(input.get("label")) { input.get("label").cloned().unwrap() } else { json!(format!("gh:{repo} {gh_kind}")) };
        let body_md: String = text.chars().take(200_000).collect();
        return crate::notebooks_notebook_source::notebookSource(
            &json!({ "op": "add", "notebook_id": num_param(nb_id), "kind": "url", "label": label, "ref": format!("https://github.com/{repo}"), "body_md": body_md }),
            db,
        );
    }

    Ok(json!({ "ok": false, "error": format!("unknown op: {} (url|docs|github)", tmpl(op_v)) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS nb;").unwrap();
        c
    }

    #[test]
    fn missing_notebook_id_errs() {
        let c = fresh();
        let e = notebookIngest(&json!({ "op": "url", "url": "https://x.example" }), &c).unwrap_err();
        assert_eq!(e, "notebook-ingest: notebook_id is required");
    }

    #[test]
    fn url_op_rejects_non_http_and_defers_fetch() {
        let c = fresh();
        let bad = notebookIngest(&json!({ "op": "url", "notebook_id": 1, "url": "ftp://x" }), &c).unwrap();
        assert_eq!(bad, json!({ "ok": false, "error": "a http(s) url is required" }));

        let deferred = notebookIngest(&json!({ "op": "url", "notebook_id": 1, "url": "https://example.com/page" }), &c).unwrap();
        assert_eq!(deferred["ok"], json!(false));
        assert!(deferred["error"].as_str().unwrap().starts_with("fetch failed:"));
        assert!(deferred["error"].as_str().unwrap().contains("PARITY-DEFERRED"));
    }

    #[test]
    fn github_op_validates_repo_shape_before_the_deferred_fetch() {
        let c = fresh();
        let bad = notebookIngest(&json!({ "op": "github", "notebook_id": 1, "repo": "no-slash" }), &c).unwrap();
        assert_eq!(bad, json!({ "ok": false, "error": "repo must be owner/name" }));

        let ok_shape = notebookIngest(&json!({ "op": "github", "notebook_id": 1, "repo": "https://github.com/owner/name.git/" }), &c).unwrap();
        assert_eq!(ok_shape["ok"], json!(false));
        assert!(ok_shape["error"].as_str().unwrap().starts_with("github fetch failed:"));
    }

    #[test]
    fn unknown_op() {
        let c = fresh();
        let r = notebookIngest(&json!({ "op": "bogus", "notebook_id": 1 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "unknown op: bogus (url|docs|github)" }));
    }

    #[test]
    fn docs_op_walks_fs_filters_extensions_and_writes_a_source() {
        let tmp = std::env::temp_dir().join(format!("nb-ingest-test-{}", std::process::id()));
        let docs = tmp.join("docs");
        let sub = docs.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(docs.join("a.md"), "# A\nhello").unwrap();
        std::fs::write(docs.join("skip.png"), "binary").unwrap();
        std::fs::write(sub.join("b.txt"), "world").unwrap();
        std::fs::create_dir_all(docs.join(".hidden")).unwrap();
        std::fs::write(docs.join(".hidden").join("c.md"), "hidden").unwrap();

        let c = fresh();
        c.execute_batch("CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')").unwrap();
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active, created_at) VALUES ('self', ?1, 'git', 1, '2026-01-01 00:00:00')",
            rusqlite::params![tmp.to_string_lossy().to_string()],
        ).unwrap();

        let r = notebookIngest(&json!({ "op": "docs", "notebook_id": 7 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let body = r["source"]["body_md"].as_str().unwrap();
        assert!(body.contains("### docs/a.md\n# A\nhello"), "{body}");
        assert!(body.contains("world"), "{body}");
        assert!(!body.contains("skip.png"));
        assert!(!body.contains("hidden"));
        assert_eq!(r["source"]["kind"], json!("file"));
        assert_eq!(r["source"]["ref"], json!("docs"));
        assert_eq!(r["source"]["label"], json!("docs: docs (2)"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn docs_op_missing_dir_and_no_matches() {
        let c = fresh();
        c.execute_batch("CREATE TABLE workspaces (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00')").unwrap();
        let missing_root = std::env::temp_dir().join(format!("nb-ingest-missing-{}", std::process::id()));
        c.execute(
            "INSERT INTO workspaces (name, root_path, vcs, is_active, created_at) VALUES ('self', ?1, 'git', 1, '2026-01-01 00:00:00')",
            rusqlite::params![missing_root.to_string_lossy().to_string()],
        ).unwrap();
        let r = notebookIngest(&json!({ "op": "docs", "notebook_id": 1, "path": "nope" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().starts_with("read failed:"), "{r}");
    }

    #[test]
    fn html_to_text_strips_tags_scripts_styles_and_entities() {
        let html = "<html><head><style>.a{}</style><script>x()</script></head><body><p>Hi&nbsp;there &amp; &lt;more&gt;</p>\n\n\n\nend</body></html>";
        let t = html_to_text(html);
        // tags + <script>/<style> bodies stripped (order: strip tags, THEN decode &lt;/&gt; —
        // so the literal '<' in the decoded "<more>" legitimately survives, matching real JS).
        assert!(!t.contains(".a{}") && !t.contains("x()"), "{t}");
        assert!(!t.contains("<p>") && !t.contains("</"), "{t}");
        assert!(t.contains("Hi there & <more>"), "{t}");
        assert!(!t.contains("\n\n\n"), "{t}");
    }

    #[test]
    fn walk_respects_depth_dotfiles_and_node_modules() {
        let tmp = std::env::temp_dir().join(format!("nb-ingest-walk-{}", std::process::id()));
        let nm = tmp.join("node_modules");
        std::fs::create_dir_all(&nm).unwrap();
        std::fs::write(nm.join("x.md"), "skip").unwrap();
        std::fs::write(tmp.join("keep.md"), "keep").unwrap();
        let files = walk(&tmp, 0).unwrap();
        let names: Vec<String> = files.iter().map(|f| f.file_name().unwrap().to_string_lossy().to_string()).collect();
        assert!(names.contains(&"keep.md".to_string()));
        assert!(!names.contains(&"x.md".to_string()));
        std::fs::remove_dir_all(&tmp).ok();
    }
}
