//! notebook-corpus.rs — twin of skills/notebooks/notebook-corpus.mjs (db READ lane). NAMES ARE
//! CONTRACT: `gather` (named, db-first — reused by cockpit-context-preview in the JS source),
//! `resolveCorpus` (named, reused by notebook-ask / notebook-generate), `notebookCorpus`
//! (default). Resolves a notebook's FULL grounded corpus: the code scope (atlas) PLUS every
//! ENABLED source. Each gather is best-effort in the JS (try/catch per source) so a missing
//! table/column never aborts the whole build — mirrored here by every per-kind query treating a
//! SQL error as "no rows" (`q()`), which yields the SAME observable outcome as the JS catch (no
//! block gets added for that source either way).
//!
//! RISKY BRANCH DISCLOSURE (`gather`, kind:"git"): a real subprocess (`git log` / `git diff` via
//! `std::process::Command`, mirroring the JS `spawnSync` calls) — reachable only when a
//! notebook_sources row has kind='git', a value the fixture never seeds (git-state/cwd dependent,
//! same "flag-skippable branch" precedent as constitution-estimate.rs's git-diff lane). Real,
//! correct code below; UNTESTED by the conform-db fixture, proven instead by a native
//! `#[cfg(test)]` (see `git_kind_runs_the_real_subprocess`).
//!
//! kind:"docs" also can't be fixture-tested: it forwards to `codeDocsBuild`, which in the JS
//! reads `context.db` — a live connection that has no JSON representation, so the harness cannot
//! synthesize a `context` argument for it. Exercised instead by a native `#[cfg(test)]`.
// QUALITY: 79/100 — a 20-armed match over source kinds, each a small deterministic SELECT+format;
// the two branches noted above are real-but-fixture-untested for the disclosed reasons.

use rusqlite::Connection;
use serde_json::{json, Value};
use std::sync::OnceLock;
use regex::Regex;

const ORIGIN: &str = "atlas";

// ── standard JS-coercion helpers (playbook) ──
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
}
fn js_number(v: Option<&Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => if *b { 1.0 } else { 0.0 },
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => { let t = s.trim(); if t.is_empty() { 0.0 } else { t.parse().unwrap_or(f64::NAN) } }
        Some(_) => f64::NAN,
    }
}
fn num_param(n: f64) -> Value { if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(n) } }
// `String(s ?? '').slice(0, n)` — the module's own `cap` helper, over a raw Value.
fn cap(v: &Value, n: usize) -> String { str_qq(Some(v)).chars().take(n).collect() }
fn cap_str(s: &str, n: usize) -> String { s.chars().take(n).collect() }
// `.replace(/\s+/g, ' ')` — collapse whitespace runs, no trim (matches the two call sites' usage
// pattern here, immediately followed by `.slice(0, n)`).
fn ws_collapse(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+").unwrap()).replace_all(s, " ").to_string()
}
// `Array.prototype.join`: null/undefined elements stringify to '' (NOT "null"/"undefined" like a
// template literal would) — distinct from `tmpl`.
fn arr_join_js(arr: &[Value], sep: &str) -> String {
    arr.iter().map(|v| if v.is_null() { String::new() } else { js_string(v) }).collect::<Vec<_>>().join(sep)
}
// query, swallowing any SQL error into an empty row set — matches the JS caller-level try/catch:
// whether gather() itself throws or degrades to '', the caller adds no block either way, so
// folding the catch INTO the query keeps every branch below a plain expression.
fn q(db: &Connection, sql: &str, params: &[Value]) -> Vec<Value> {
    crate::db_json::query_rows(db, sql, params).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}
// `JSON.parse(v || '[]') || []` guarded exactly like the JS try/catch: parse failure OR a
// truthy-but-non-array parse result (which would throw on the subsequent `.map`/`.join` inside
// the JS try block) both collapse to "no array" — the caller then renders '' either way.
fn parsed_array_or_none(v: Option<&Value>) -> Option<Vec<Value>> {
    let s = if truthy(v) { js_string(v.unwrap()) } else { "[]".to_string() };
    match serde_json::from_str::<Value>(&s) {
        Err(_) => None,
        Ok(Value::Null) => Some(vec![]),
        Ok(Value::Array(a)) => Some(a),
        Ok(_) => None,
    }
}

/// `export function gather(db, s, scope, context)` — gather ONE source's text. `context` carries
/// nothing this twin needs beyond `db` (its one use, kind:"docs", forwards to `codeDocsBuild`
/// which itself takes `db` directly in this crate) — kept as a parameter for JS-arity fidelity
/// with the db-first calling convention, never read.
pub fn gather(db: &Connection, s: &Value, scope: &Value, _context: &Value) -> Value {
    let paths: Vec<Value> = scope.get("paths").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let kind = s.get("kind").and_then(|v| v.as_str()).unwrap_or("");

    let text = match kind {
        // A CONTEXT PACK (2026-09-19): consolidated in the context library, reachable from every
        // selection — the slug rides in `body_md` (or `path`), the body is read live
        "context_pack" => {
            let slug = { let b = if truthy(s.get("body_md")) { js_string(s.get("body_md").unwrap()) } else { String::new() }; if b.trim().is_empty() { if truthy(s.get("path")) { js_string(s.get("path").unwrap()) } else { String::new() } } else { b } };
            crate::context_context_library::pack_body(db, slug.trim()).unwrap_or_default()
        }
        "text" | "file" | "url" | "source" | "conversation" | "focus" => {
            if truthy(s.get("body_md")) { js_string(s.get("body_md").unwrap()) } else { String::new() }
        }
        "descriptions" => {
            if paths.is_empty() { String::new() } else {
                let p: Vec<&Value> = paths.iter().take(60).collect();
                let ph = p.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let mut args: Vec<Value> = vec![json!(ORIGIN)];
                args.extend(p.iter().map(|x| (*x).clone()));
                let sql = format!("SELECT path, summary_md FROM code_descriptions WHERE origin = ? AND summary_md IS NOT NULL AND path IN ({ph})");
                q(db, &sql, &args).iter().map(|r| format!("### {}\n{}", tmpl(r.get("path")), tmpl(r.get("summary_md")))).collect::<Vec<_>>().join("\n\n")
            }
        }
        "data_stories" => {
            q(db, "SELECT name, description, hops_json FROM code_data_stories WHERE origin = ? ORDER BY id DESC LIMIT 40", &[json!(ORIGIN)])
                .iter().map(|r| {
                    let h = match parsed_array_or_none(r.get("hops_json")) {
                        None => String::new(),
                        Some(arr) => {
                            let labels: Vec<String> = arr.iter().filter_map(|x| {
                                let chosen = if truthy(x.get("label")) { x.get("label") } else if truthy(x.get("node_id")) { x.get("node_id") } else { None };
                                chosen.map(js_string)
                            }).collect();
                            labels.join(" \u{2192} ")
                        }
                    };
                    let desc = if truthy(r.get("description")) { js_string(r.get("description").unwrap()) } else { String::new() };
                    let flow = if h.is_empty() { String::new() } else { format!("\n  flow: {h}") };
                    format!("- **{}** \u{2014} {}{}", tmpl(r.get("name")), desc, flow)
                }).collect::<Vec<_>>().join("\n")
        }
        "entrypoints" => {
            q(db, "SELECT * FROM code_entry_points WHERE origin = ? LIMIT 200", &[json!(ORIGIN)])
                .iter().map(|r| {
                    let kind_part = if truthy(r.get("kind")) { js_string(r.get("kind").unwrap()) } else { String::new() };
                    let subtype_part = if truthy(r.get("subtype")) { format!("/{}", js_string(r.get("subtype").unwrap())) } else { String::new() };
                    let path_part = if truthy(r.get("path")) { js_string(r.get("path").unwrap()) } else { String::new() };
                    let detail_part = if truthy(r.get("detail")) { js_string(r.get("detail").unwrap()) } else if truthy(r.get("name")) { js_string(r.get("name").unwrap()) } else { String::new() };
                    format!("- [{kind_part}{subtype_part}] {path_part} {detail_part}")
                }).collect::<Vec<_>>().join("\n")
        }
        "digest" => {
            let rows = q(db, "SELECT * FROM code_digests WHERE origin = ? ORDER BY rowid DESC LIMIT 1", &[json!(ORIGIN)]);
            match rows.first() {
                None => String::new(),
                Some(row) => row.as_object().map(|obj| {
                    obj.iter().filter(|(k, v)| !v.is_null() && k.as_str() != "origin" && !v.is_object())
                        .map(|(k, v)| format!("- {}: {}", k, cap(v, 120)))
                        .collect::<Vec<_>>().join("\n")
                }).unwrap_or_default(),
            }
        }
        "design" => {
            q(db, "SELECT * FROM code_design_tokens WHERE origin = ? LIMIT 120", &[json!(ORIGIN)])
                .iter().map(|r| {
                    let name = if truthy(r.get("name")) { js_string(r.get("name").unwrap()) } else if truthy(r.get("token")) { js_string(r.get("token").unwrap()) } else { String::new() };
                    let value = if truthy(r.get("value")) { js_string(r.get("value").unwrap()) } else { String::new() };
                    format!("- {name} = {value}")
                }).collect::<Vec<_>>().join("\n")
        }
        "tests" => {
            q(db, "SELECT path FROM code_nodes WHERE origin = ? AND type = 'file' AND (path LIKE '%.test.%' OR path LIKE '%.spec.%' OR path LIKE '%/test/%' OR path LIKE '%/tests/%') LIMIT 200", &[json!(ORIGIN)])
                .iter().map(|r| format!("- {}", tmpl(r.get("path")))).collect::<Vec<_>>().join("\n")
        }
        "ast" => {
            if paths.is_empty() { String::new() } else {
                let p: Vec<&Value> = paths.iter().take(40).collect();
                let ph = p.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let mut args: Vec<Value> = vec![json!(ORIGIN)];
                args.extend(p.iter().map(|x| (*x).clone()));
                let sql = format!("SELECT path, type, name, signature FROM code_nodes WHERE origin = ? AND signature IS NOT NULL AND path IN ({ph}) LIMIT 300");
                q(db, &sql, &args).iter().map(|r| {
                    let name = if truthy(r.get("name")) { js_string(r.get("name").unwrap()) } else { String::new() };
                    let sig = if truthy(r.get("signature")) { js_string(r.get("signature").unwrap()) } else { String::new() };
                    format!("- {} :: {} ({}) {}", tmpl(r.get("path")), name, tmpl(r.get("type")), sig)
                }).collect::<Vec<_>>().join("\n")
            }
        }
        // THE X-RAY'S OWN VERDICTS (operator 2026-09-19: "file review and folder paper also on the scope
        // selection of the ultra review"): the deterministic file review of each scoped file, and the
        // folder paper of each scoped file's folder — the same markdown the pane bake writes.
        "file_review" | "folder_paper" => {
            if paths.is_empty() { String::new() } else {
                let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
                let mut blocks: Vec<String> = Vec::new();
                for p in paths.iter().take(8) {
                    let file = js_string(p);
                    let target = if kind == "folder_paper" { file.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_else(|| ".".to_string()) } else { file.clone() };
                    if !seen.insert(target.clone()) { continue; }
                    let r = crate::constitution_constitution_file::constitutionFile(&json!({ "path": target }), db).unwrap_or_else(|e| json!({ "path": target, "found": false, "doc_md": e }));
                    let md = if kind == "folder_paper" { crate::constitution__helpers_review_markdown::folder_review_md(&r, "") } else { crate::constitution__helpers_review_markdown::file_review_md(&r, "") };
                    if !md.trim().is_empty() { blocks.push(md.chars().take(4000).collect()); }
                }
                blocks.join("\n\n")
            }
        }
        "git" => git_kind_text(db),
        "docs" => {
            let r = crate::codebase_code_docs_build::codeDocsBuild(&json!({ "page": "", "format": "md" }), db).unwrap_or(Value::Null);
            let md = r.get("markdown").cloned().unwrap_or(Value::Null);
            cap(&md, 6000)
        }
        "cards" => {
            q(db, "SELECT slug, title, status FROM cards ORDER BY COALESCE(updated_at, created_at) DESC LIMIT 40", &[])
                .iter().map(|r| format!("- [{}] {} \u{2014} {}", tmpl(r.get("status")), tmpl(r.get("slug")), tmpl(r.get("title")))).collect::<Vec<_>>().join("\n")
        }
        "contracts" => {
            q(db, "SELECT slug, card_slug, status, goal_md FROM contracts ORDER BY COALESCE(updated_at, created_at) DESC LIMIT 30", &[])
                .iter().map(|r| {
                    let card_slug = if truthy(r.get("card_slug")) { js_string(r.get("card_slug").unwrap()) } else { "\u{2014}".to_string() };
                    let goal = if truthy(r.get("goal_md")) { format!(" \u{2014} {}", cap_str(&ws_collapse(&js_string(r.get("goal_md").unwrap())), 120)) } else { String::new() };
                    format!("- {} ({}) [{}]{}", tmpl(r.get("slug")), card_slug, tmpl(r.get("status")), goal)
                }).collect::<Vec<_>>().join("\n")
        }
        "failures" => {
            q(db, "SELECT card_slug, classification, root_cause_md, suggested_fix_md FROM brain.driver_diagnoses ORDER BY COALESCE(updated_at, created_at) DESC LIMIT 30", &[])
                .iter().map(|r| {
                    let card_slug = if truthy(r.get("card_slug")) { js_string(r.get("card_slug").unwrap()) } else { "\u{2014}".to_string() };
                    let classification = if truthy(r.get("classification")) { js_string(r.get("classification").unwrap()) } else { "?".to_string() };
                    let root_cause = if truthy(r.get("root_cause_md")) { js_string(r.get("root_cause_md").unwrap()) } else { String::new() };
                    let fix = if truthy(r.get("suggested_fix_md")) { format!(" \u{2192} fix: {}", cap_str(&ws_collapse(&js_string(r.get("suggested_fix_md").unwrap())), 120)) } else { String::new() };
                    format!("- {} [{}] {}{}", card_slug, classification, cap_str(&ws_collapse(&root_cause), 160), fix)
                }).collect::<Vec<_>>().join("\n")
        }
        "court" => {
            q(db, "SELECT card_slug, scope_ref, verdict, goal_line FROM judgments.judgment_cases ORDER BY created_at DESC LIMIT 30", &[])
                .iter().map(|r| {
                    let subj = if truthy(r.get("card_slug")) { js_string(r.get("card_slug").unwrap()) } else if truthy(r.get("scope_ref")) { js_string(r.get("scope_ref").unwrap()) } else { "\u{2014}".to_string() };
                    let verdict = if truthy(r.get("verdict")) { js_string(r.get("verdict").unwrap()) } else { "?".to_string() };
                    let goal = if truthy(r.get("goal_line")) { format!(" ({})", cap_str(&js_string(r.get("goal_line").unwrap()), 100)) } else { String::new() };
                    format!("- {subj} \u{2192} {verdict}{goal}")
                }).collect::<Vec<_>>().join("\n")
        }
        "decisions" => {
            q(db, "SELECT card_slug, decision, rationale, decided_at FROM brain.brain_decisions WHERE decision NOT IN ('rule_match','agent_step','session_start','session_end') ORDER BY id DESC LIMIT 40", &[])
                .iter().map(|r| {
                    let decided_at = if truthy(r.get("decided_at")) { js_string(r.get("decided_at").unwrap()) } else { String::new() };
                    let card_slug = if truthy(r.get("card_slug")) { js_string(r.get("card_slug").unwrap()) } else { "\u{2014}".to_string() };
                    let rationale = if truthy(r.get("rationale")) { js_string(r.get("rationale").unwrap()) } else { String::new() };
                    format!("- {} {} {}: {}", decided_at, card_slug, tmpl(r.get("decision")), cap_str(&ws_collapse(&rationale), 120))
                }).collect::<Vec<_>>().join("\n")
        }
        "timeline" => {
            q(db, "SELECT ts, actor, operation, entity_type, entity_id FROM timeline.sacred_timeline ORDER BY id DESC LIMIT 60", &[])
                .iter().map(|r| {
                    let ts = if truthy(r.get("ts")) { js_string(r.get("ts").unwrap()) } else { String::new() };
                    let actor = if truthy(r.get("actor")) { js_string(r.get("actor").unwrap()) } else { String::new() };
                    let etype = if truthy(r.get("entity_type")) { js_string(r.get("entity_type").unwrap()) } else { String::new() };
                    let eid = if truthy(r.get("entity_id")) { js_string(r.get("entity_id").unwrap()) } else { String::new() };
                    format!("- {} {} {} {}#{}", ts, actor, tmpl(r.get("operation")), etype, eid)
                }).collect::<Vec<_>>().join("\n")
        }
        "runtime" => {
            q(db, "SELECT ts, agent, action, skill, outcome, error_code FROM runtime_events ORDER BY id DESC LIMIT 60", &[])
                .iter().map(|r| {
                    let ts = if truthy(r.get("ts")) { js_string(r.get("ts").unwrap()) } else { String::new() };
                    let agent = if truthy(r.get("agent")) { js_string(r.get("agent").unwrap()) } else { String::new() };
                    let action = if truthy(r.get("action")) { js_string(r.get("action").unwrap()) } else { String::new() };
                    let skill = if truthy(r.get("skill")) { format!("/{}", js_string(r.get("skill").unwrap())) } else { String::new() };
                    let outcome = if truthy(r.get("outcome")) { js_string(r.get("outcome").unwrap()) } else { String::new() };
                    let err_code = if truthy(r.get("error_code")) { format!(" [{}]", js_string(r.get("error_code").unwrap())) } else { String::new() };
                    format!("- {ts} {agent} {action}{skill} \u{2192} {outcome}{err_code}")
                }).collect::<Vec<_>>().join("\n")
        }
        "memory" => {
            q(db, "SELECT type, title, guidance FROM memory.memories ORDER BY id DESC LIMIT 40", &[])
                .iter().map(|r| {
                    let typ = if truthy(r.get("type")) { js_string(r.get("type").unwrap()) } else { String::new() };
                    let title = if truthy(r.get("title")) { js_string(r.get("title").unwrap()) } else { String::new() };
                    let guidance = if truthy(r.get("guidance")) { format!(" \u{2014} {}", cap_str(&ws_collapse(&js_string(r.get("guidance").unwrap())), 140)) } else { String::new() };
                    format!("- [{typ}] {title}{guidance}")
                }).collect::<Vec<_>>().join("\n")
        }
        "blueprint" => {
            q(db, "SELECT path, name, type, signature, intent FROM code_nodes WHERE origin LIKE 'blueprint:%' ORDER BY created_at DESC LIMIT 80", &[])
                .iter().map(|r| {
                    let name = if truthy(r.get("name")) { js_string(r.get("name").unwrap()) } else { String::new() };
                    let intent = if truthy(r.get("intent")) { format!(" \u{2014} {}", cap_str(&js_string(r.get("intent").unwrap()), 100)) } else { String::new() };
                    format!("- {} :: {} ({}){}", tmpl(r.get("path")), name, tmpl(r.get("type")), intent)
                }).collect::<Vec<_>>().join("\n")
        }
        "user_stories" => {
            q(db, "SELECT name, url, routes_json FROM code_user_stories WHERE origin = ? ORDER BY id DESC LIMIT 30", &[json!(ORIGIN)])
                .iter().map(|r| {
                    let routes = parsed_array_or_none(r.get("routes_json")).map(|a| arr_join_js(&a, " \u{2192} ")).unwrap_or_default();
                    let routes_part = if routes.is_empty() { String::new() } else { format!(" [{routes}]") };
                    format!("- {}{}", tmpl(r.get("name")), routes_part)
                }).collect::<Vec<_>>().join("\n")
        }
        "faculty_arch" => return crate::codebase__helpers_magazine_db::facultyBrief(db, &json!("arch")),
        "faculty_product" => return crate::codebase__helpers_magazine_db::facultyBrief(db, &json!("product")),
        "paper" => {
            let refv = if truthy(s.get("ref")) { js_string(s.get("ref").unwrap()) } else { String::new() };
            let refv = refv.trim().to_string();
            let mut row: Option<Value> = None;
            if !refv.is_empty() {
                let n = js_number(Some(&json!(refv.clone())));
                let id_param: Value = if n.is_finite() && n == n.trunc() { json!(n as i64) } else { json!(-1i64) };
                let rows = q(db, "SELECT title, discovery, markdown_md FROM judgments.review_papers WHERE slug = ? OR id = ?", &[json!(refv.clone()), id_param]);
                row = rows.into_iter().next();
            }
            match row {
                Some(r) => {
                    if truthy(r.get("markdown_md")) { js_string(r.get("markdown_md").unwrap()) }
                    else {
                        let discovery = if truthy(r.get("discovery")) { js_string(r.get("discovery").unwrap()) } else { String::new() };
                        format!("# {}\n\n{}", tmpl(r.get("title")), discovery)
                    }
                }
                None => if truthy(s.get("body_md")) { js_string(s.get("body_md").unwrap()) } else { String::new() },
            }
        }
        _ => String::new(),
    };

    json!(text)
}

/// `case 'git'` — real subprocess: `git log` + `git diff --stat` against the resolved workspace
/// root. See module doc: reachable only via a source row's kind='git' (never seeded by the
/// conform-db fixture — proven instead by a native test).
fn git_kind_text(db: &Connection) -> String {
    let root_v = crate::engine_workspace_root::getWorkspaceRoot(db);
    let root = match root_v.as_str() { Some(s) if !s.is_empty() => s.to_string(), _ => return String::new() };
    let log = crate::proc::cmd("git")
        .args(["log", "--no-merges", "--pretty=%h %ad %s", "--date=short", "-30"])
        .current_dir(&root)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let stat = crate::proc::cmd("git")
        .args(["diff", "--stat", "HEAD~5", "HEAD"])
        .current_dir(&root)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    cap_str(&format!("Recent commits:\n{log}\n\nRecent churn:\n{stat}"), 4000)
}

/// `export async function resolveCorpus(nb, context)` — reused by notebook-ask / notebook-generate.
pub fn resolveCorpus(nb: &Value, db: &Connection) -> Result<Value, String> {
    let mut blocks: Vec<Value> = Vec::new();

    let scope_input = if truthy(nb.get("scope")) { nb.get("scope").cloned().unwrap() } else { json!({}) };
    let scope = crate::notebooks_notebook_scope::resolveScope(&scope_input, db)?;

    if truthy(scope.get("code_context_md")) {
        let text = scope.get("code_context_md").cloned().unwrap();
        blocks.push(json!({
            "kind": "code_paths",
            "label": "Code in scope",
            "text": text.clone(),
            "tokens": crate::system_tokens_count::estimateTokens(&text),
        }));
    }

    let nb_id_n = if truthy(nb.get("id")) { js_number(nb.get("id")) } else { 0.0 };
    let sources: Vec<Value> = q(db, "SELECT * FROM nb.notebook_sources WHERE notebook_id = ? AND enabled = 1", &[num_param(nb_id_n)]);

    for s in &sources {
        let t = gather(db, s, &scope, &Value::Null);
        let t_str = t.as_str().unwrap_or("");
        if !t_str.trim().is_empty() {
            let txt: String = t_str.chars().take(6000).collect();
            let label = if truthy(s.get("label")) { s.get("label").cloned().unwrap() } else { s.get("kind").cloned().unwrap_or(Value::Null) };
            blocks.push(json!({
                "kind": s.get("kind").cloned().unwrap_or(Value::Null),
                "label": label,
                "text": txt.clone(),
                "tokens": crate::system_tokens_count::estimateTokens(&json!(txt)),
                "source_id": s.get("id").cloned().unwrap_or(Value::Null),
            }));
        }
    }

    // PRIORITY TIERS: focus(0) < conversation(1) < everything else(2), stable within a tier.
    let prio = |b: &Value| -> i32 {
        match b.get("kind").and_then(|v| v.as_str()) {
            Some("focus") => 0,
            Some("conversation") => 1,
            _ => 2,
        }
    };
    blocks.sort_by_key(prio);

    let total_tokens: i64 = blocks.iter().map(|b| b.get("tokens").and_then(|v| v.as_i64()).unwrap_or(0)).sum();

    Ok(json!({
        "ok": true,
        "blocks": blocks,
        "paths": scope.get("paths").cloned().unwrap_or(json!([])),
        "nodes": scope.get("nodes").cloned().unwrap_or(json!([])),
        "total_tokens": total_tokens,
    }))
}

/// `export default async function notebookCorpus(input, context)`.
pub fn notebookCorpus(input: &Value, db: &Connection) -> Result<Value, String> {
    crate::notebooks_notebook_schema::ensureNotebookSchema(db, &json!("nb."));
    let nb = if truthy(input.get("id")) {
        crate::notebooks_notebook_store::getNotebook(db, input.get("id").unwrap())
    } else {
        let scope = if truthy(input.get("scope")) { input.get("scope").cloned().unwrap() } else { json!({}) };
        json!({ "id": 0, "scope": scope })
    };
    if nb.is_null() {
        return Ok(json!({ "ok": false, "error": format!("notebook {} not found", tmpl(input.get("id"))) }));
    }
    resolveCorpus(&nb, db)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS nb; ATTACH ':memory:' AS brain; ATTACH ':memory:' AS judgments; ATTACH ':memory:' AS timeline; ATTACH ':memory:' AS memory; ATTACH ':memory:' AS knowledge;").unwrap();
        c
    }

    #[test]
    fn gather_plain_text_kinds() {
        let c = fresh();
        let empty = json!({});
        assert_eq!(gather(&c, &json!({ "kind": "text", "body_md": "hi" }), &empty, &Value::Null), json!("hi"));
        assert_eq!(gather(&c, &json!({ "kind": "focus" }), &empty, &Value::Null), json!(""));
        assert_eq!(gather(&c, &json!({ "kind": "unknown_kind" }), &empty, &Value::Null), json!(""));
    }

    #[test]
    fn gather_descriptions_empty_paths_short_circuits() {
        let c = fresh();
        assert_eq!(gather(&c, &json!({ "kind": "descriptions" }), &json!({}), &Value::Null), json!(""));
    }

    #[test]
    fn gather_descriptions_with_paths() {
        let c = fresh();
        c.execute_batch("CREATE TABLE code_descriptions (origin TEXT, path TEXT, summary_md TEXT);").unwrap();
        crate::db_json::run_write(&c, "INSERT INTO code_descriptions (origin, path, summary_md) VALUES ('atlas','a.ts','Summary A')", &[]).unwrap();
        crate::db_json::run_write(&c, "INSERT INTO code_descriptions (origin, path, summary_md) VALUES ('atlas','b.ts',NULL)", &[]).unwrap();
        let scope = json!({ "paths": ["a.ts", "b.ts"] });
        let r = gather(&c, &json!({ "kind": "descriptions" }), &scope, &Value::Null);
        assert_eq!(r, json!("### a.ts\nSummary A"));
    }

    #[test]
    fn gather_data_stories_flow_and_parse_failure() {
        let c = fresh();
        c.execute_batch("CREATE TABLE code_data_stories (id INTEGER PRIMARY KEY, origin TEXT, name TEXT, description TEXT, hops_json TEXT);").unwrap();
        crate::db_json::run_write(&c, "INSERT INTO code_data_stories (origin,name,description,hops_json) VALUES ('atlas','Checkout','flows money','[{\"label\":\"cart\"},{\"node_id\":\"pay@\"}]')", &[]).unwrap();
        crate::db_json::run_write(&c, "INSERT INTO code_data_stories (origin,name,description,hops_json) VALUES ('atlas','Bad','d','not json')", &[]).unwrap();
        let r = gather(&c, &json!({ "kind": "data_stories" }), &json!({}), &Value::Null);
        let s = r.as_str().unwrap();
        assert!(s.contains("- **Bad** \u{2014} d"));
        assert!(!s.contains("Bad** \u{2014} d\n  flow:"));
        assert!(s.contains("- **Checkout** \u{2014} flows money\n  flow: cart \u{2192} pay@"));
    }

    #[test]
    fn gather_digest_filters_origin_and_objects() {
        let c = fresh();
        // no explicit id/rowid column named — SQLite's implicit ROWID still services `ORDER BY
        // rowid`, and `SELECT *` returns only the 3 named columns (keeps the digest filter's
        // expected output exact — a named PK column would legitimately show up too).
        c.execute_batch("CREATE TABLE code_digests (origin TEXT, total_files_num INTEGER, narrative_md TEXT);").unwrap();
        crate::db_json::run_write(&c, "INSERT INTO code_digests (origin, total_files_num, narrative_md) VALUES ('atlas', 12, NULL)", &[]).unwrap();
        let r = gather(&c, &json!({ "kind": "digest" }), &json!({}), &Value::Null);
        assert_eq!(r, json!("- total_files_num: 12"));
    }

    #[test]
    fn gather_cards_and_faculty_and_paper_fallback() {
        let c = fresh();
        c.execute_batch("CREATE TABLE cards (slug TEXT, title TEXT, status TEXT, updated_at TEXT, created_at TEXT);").unwrap();
        crate::db_json::run_write(&c, "INSERT INTO cards (slug,title,status,created_at) VALUES ('c1','Card One','doing','2026-01-01')", &[]).unwrap();
        let r = gather(&c, &json!({ "kind": "cards" }), &json!({}), &Value::Null);
        assert_eq!(r, json!("- [doing] c1 \u{2014} Card One"));

        let brief = gather(&c, &json!({ "kind": "faculty_arch" }), &json!({}), &Value::Null);
        assert!(brief.as_str().unwrap().contains("no issue pressed yet"));

        // 'paper' with a ref that resolves to no row falls back to body_md.
        let paper = gather(&c, &json!({ "kind": "paper", "ref": "missing", "body_md": "fallback text" }), &json!({}), &Value::Null);
        assert_eq!(paper, json!("fallback text"));
    }

    #[test]
    fn gather_paper_found_by_slug() {
        let c = fresh();
        c.execute_batch("CREATE TABLE judgments.review_papers (id INTEGER PRIMARY KEY, slug TEXT, title TEXT, discovery TEXT, markdown_md TEXT);").unwrap();
        crate::db_json::run_write(&c, "INSERT INTO judgments.review_papers (slug,title,discovery,markdown_md) VALUES ('vite-8','Vite 8','the bump','# Vite 8 body')", &[]).unwrap();
        let r = gather(&c, &json!({ "kind": "paper", "ref": " vite-8 " }), &json!({}), &Value::Null);
        assert_eq!(r, json!("# Vite 8 body"));
    }

    #[test]
    fn git_kind_runs_the_real_subprocess() {
        // proves the branch is real code (not a stub) — the workspace has no active row, so
        // getWorkspaceRoot falls back to SUBSTRATE_ROOT, and `git log`/`git diff` run for real.
        let c = fresh();
        let r = gather(&c, &json!({ "kind": "git" }), &json!({}), &Value::Null);
        assert!(r.as_str().unwrap().starts_with("Recent commits:\n"));
    }

    #[test]
    fn docs_kind_calls_code_docs_build_and_extracts_markdown() {
        let c = fresh();
        c.execute_batch("CREATE TABLE code_decorators (id INTEGER PRIMARY KEY, origin TEXT, path TEXT, name TEXT, kind TEXT, role TEXT, line INTEGER, end_line INTEGER, signature TEXT, summary TEXT, inputs_json TEXT, output_json TEXT, effects_json TEXT, calls_json TEXT, uses_json TEXT, complexity INTEGER, flags_json TEXT, steps_json TEXT);").unwrap();
        // empty decorators -> codeDocsBuild's `found:false` early-return has no `markdown` key.
        let r = gather(&c, &json!({ "kind": "docs" }), &json!({}), &Value::Null);
        assert_eq!(r, json!(""));
    }

    #[test]
    fn resolve_corpus_orders_tiers_and_sums_tokens() {
        let c = fresh();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, parent_id TEXT); \
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT); \
             CREATE TABLE nb.notebook_sources (id INTEGER PRIMARY KEY, notebook_id INTEGER, kind TEXT, ref TEXT, label TEXT, body_md TEXT, enabled INTEGER, meta_json TEXT, created_at TEXT);",
        ).unwrap();
        crate::db_json::run_write(&c, "INSERT INTO code_nodes (id,origin,path,type,name) VALUES ('a@','atlas','a.ts','file','a')", &[]).unwrap();
        crate::db_json::run_write(
            &c,
            "INSERT INTO nb.notebook_sources (notebook_id,kind,label,body_md,enabled) VALUES (1,'text','A note','hello world',1)",
            &[],
        ).unwrap();
        crate::db_json::run_write(
            &c,
            "INSERT INTO nb.notebook_sources (notebook_id,kind,label,body_md,enabled) VALUES (1,'focus',NULL,'the focus text',1)",
            &[],
        ).unwrap();
        crate::db_json::run_write(
            &c,
            "INSERT INTO nb.notebook_sources (notebook_id,kind,label,body_md,enabled) VALUES (1,'conversation',NULL,'the convo text',1)",
            &[],
        ).unwrap();
        crate::db_json::run_write(
            &c,
            "INSERT INTO nb.notebook_sources (notebook_id,kind,label,body_md,enabled) VALUES (1,'text',NULL,'   ',1)", // blank -> no block
            &[],
        ).unwrap();
        crate::db_json::run_write(
            &c,
            "INSERT INTO nb.notebook_sources (notebook_id,kind,label,body_md,enabled) VALUES (1,'text','disabled','nope',0)", // disabled -> excluded by WHERE
            &[],
        ).unwrap();

        let nb = json!({ "id": 1, "scope": { "paths": ["a.ts"] } });
        let r = resolveCorpus(&nb, &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        let kinds: Vec<Value> = r["blocks"].as_array().unwrap().iter().map(|b| b["kind"].clone()).collect();
        // code_paths (from the in-scope digest) is a plain-tier(2) block appended first, so it
        // sorts AFTER the focus/conversation tiers but keeps its relative position among tier-2s.
        assert_eq!(kinds, vec![json!("focus"), json!("conversation"), json!("code_paths"), json!("text")]);
        assert_eq!(r["blocks"][3]["label"], json!("A note"));
        assert_eq!(r["paths"], json!(["a.ts"]));
        let total: i64 = r["blocks"].as_array().unwrap().iter().map(|b| b["tokens"].as_i64().unwrap()).sum();
        assert_eq!(r["total_tokens"], json!(total));
    }

    #[test]
    fn notebook_corpus_missing_notebook_returns_error() {
        let c = fresh();
        let r = notebookCorpus(&json!({ "id": 999 }), &c).unwrap();
        assert_eq!(r, json!({ "ok": false, "error": "notebook 999 not found" }));
    }

    #[test]
    fn notebook_corpus_default_scope_when_no_id() {
        let c = fresh();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, parent_id TEXT); \
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);",
        ).unwrap();
        let r = notebookCorpus(&json!({}), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["blocks"], json!([]));
    }

    #[test]
    fn notebook_corpus_by_id_delegates_to_get_notebook_scope() {
        let c = fresh();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, parent_id TEXT); \
             CREATE TABLE code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT, kind TEXT);",
        ).unwrap();
        crate::db_json::run_write(&c, "INSERT INTO code_nodes (id,origin,path,type,name) VALUES ('a@','atlas','a.ts','file','a')", &[]).unwrap();
        c.execute_batch("CREATE TABLE nb.notebooks (id INTEGER PRIMARY KEY, slug TEXT, title TEXT NOT NULL, summary TEXT, scope_json TEXT NOT NULL DEFAULT '{}', created_at TEXT, updated_at TEXT);").unwrap();
        crate::db_json::run_write(
            &c,
            "INSERT INTO nb.notebooks (id, slug, title, scope_json) VALUES (1, 's1', 'T1', ?)",
            &[json!(serde_json::to_string(&json!({ "paths": ["a.ts"] })).unwrap())],
        ).unwrap();
        let r = notebookCorpus(&json!({ "id": 1 }), &c).unwrap();
        assert_eq!(r["ok"], json!(true));
        assert_eq!(r["paths"], json!(["a.ts"]));
    }
}
