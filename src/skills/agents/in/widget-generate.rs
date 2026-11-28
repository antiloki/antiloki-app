//! widget-generate.rs — twin of skills/agents/in/widget-generate.mjs. NAMES ARE CONTRACT:
//! `widgetGenerate` (default). The ONE deterministic path that produces a widget's payload: a
//! typed REGISTRY dispatch (name -> {kind, produce, lint}) shared by every widget-shaped command.
//!
//! Four widgets (memory/cost/board/queue) are pure DB-read producers with internal try/catch —
//! ported as graceful-degrade-on-query-error (mirrors the JS empty `catch {}` blocks exactly).
//! `code-sample` and `diagram` call `widgetScopeResolve` UNWRAPPED (JS does not try/catch that
//! call either — a failure propagates to the caller, so these two producers return
//! `Result<Value, String>` and widgetGenerate propagates via `?`). `improvements` wraps BOTH the
//! scope-resolve and the atlas-code-sample call in one try (falls back to deterministic
//! structural findings on ANY failure — cycles/orphans from atlas-smells + lowest-maintainability
//! files from code-docs-build). `logic-spec` wraps only the AI call.
//!
//! `context.db` is always injected by the db lane in production, so the JS `if (!db) throw
//! err.contextMissing(...)` guard is unreachable here (mirrors every sibling twin's convention).
//!
//! PARITY-DEFERRED boundary: `improvements`/`diagram`/`logic-spec` bottom out at `aiCall` via
//! `atlasCodeSample`/`atlasArchitectureDiagram`/`atlasLogicSpec` (all pre-existing Rust twins,
//! themselves parity-deferred at the same aiCall boundary — it always Errs). Everything AROUND
//! that boundary — registry dispatch, the four pure DB producers, the structural-findings
//! fallback, the code-sample/diagram scope resolution + result shaping, and every lint — is fully
//! deterministic and is ported + native-tested here.
//!
//! Cross-module calls (pre-existing Rust twins): `crate::agents_in_widget_scope_resolve::widgetScopeResolve`,
//! `crate::codebase_atlas_smells::atlasSmells`, `crate::codebase_code_docs_build::codeDocsBuild`,
//! `crate::codebase_atlas_code_sample::atlasCodeSample`, `crate::codebase_atlas_architecture_diagram::atlasArchitectureDiagram`,
//! `crate::codebase_atlas_logic_spec::atlasLogicSpec`, `crate::system_driver_queue::driverQueue`.

use crate::db_json::query_rows;
use regex::Regex;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

// Standard helper functions (playbook-verbatim) — JS coercion semantics.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map_or(false, |f| f != 0.0 && !f.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) } // JS `??` right operand
fn str_qq(v: Option<&Value>) -> String { match nz(v) { Some(x) => js_string(x), None => String::new() } } // = JS String(v ?? '')
// first non-nullish value among `vals`, JS `a ?? b ?? c` semantics.
fn coalesce<'a>(vals: &[Option<&'a Value>]) -> Option<&'a Value> {
    for v in vals { if let Some(x) = nz(*v) { return Some(x); } }
    None
}

static LANG_BY_EXT: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    HashMap::from([
        ("js", "javascript"), ("jsx", "javascript"), ("mjs", "javascript"), ("cjs", "javascript"),
        ("ts", "typescript"), ("tsx", "typescript"), ("py", "python"), ("go", "go"), ("rb", "ruby"),
        ("java", "java"), ("php", "php"), ("cs", "csharp"), ("rs", "rust"), ("c", "c"), ("h", "c"),
        ("cpp", "cpp"), ("css", "css"), ("scss", "scss"), ("html", "html"), ("json", "json"),
        ("md", "markdown"), ("sql", "sql"),
    ])
});
// `const langOf = (p) => LANG_BY_EXT[String(p).split('.').pop()?.toLowerCase()] ?? 'text';`
fn lang_of(p: &str) -> &'static str {
    let ext = p.rsplit('.').next().unwrap_or("").to_lowercase();
    LANG_BY_EXT.get(ext.as_str()).copied().unwrap_or("text")
}

static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

const AVAILABLE: [&str; 8] = ["memory", "cost", "board", "queue", "improvements", "code-sample", "diagram", "logic-spec"];

// ── memory: pure DB read, graceful-degrade on query error (mirrors JS `catch { rows = []; }`) ──
fn produce_memory(args: &Value, db: &Connection) -> Value {
    let q = if truthy(args.get("query")) { Some(js_string(args.get("query").unwrap()).trim().to_string()) } else { None };
    let rows = match &q {
        Some(q) => {
            let like = format!("%{q}%");
            query_rows(
                db,
                "SELECT type, title, situation, guidance FROM memory.memories WHERE title LIKE ? OR situation LIKE ? OR guidance LIKE ? ORDER BY created_at DESC LIMIT 20",
                &[json!(like.clone()), json!(like.clone()), json!(like)],
            ).unwrap_or(json!([]))
        }
        None => query_rows(db, "SELECT type, title, situation, guidance FROM memory.memories ORDER BY created_at DESC LIMIT 20", &[]).unwrap_or(json!([])),
    };
    let hits: Vec<Value> = rows.as_array().cloned().unwrap_or_default().iter().map(|r| {
        let snippet_src = coalesce(&[r.get("guidance"), r.get("situation")]);
        let raw = match snippet_src { Some(v) => js_string(v), None => String::new() };
        let collapsed = WHITESPACE_RE.replace_all(&raw, " ").into_owned();
        let snippet: String = collapsed.chars().take(220).collect();
        json!({
            "type": r.get("type").cloned().unwrap_or(Value::Null),
            "title": r.get("title").cloned().unwrap_or(Value::Null),
            "snippet": snippet,
        })
    }).collect();
    json!({ "query": q, "hits": hits })
}

// ── cost: pure DB read, both queries share ONE try (JS: second query never runs if first throws) ──
fn produce_cost(db: &Connection) -> Value {
    let attempt: Result<(f64, i64, Value), String> = (|| {
        let r = query_rows(db, "SELECT COALESCE(SUM(cost_usd_actual),0) AS u, COUNT(*) AS c FROM brain.ai_calls WHERE ts >= date('now','start of day')", &[])?;
        let row = r.as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
        let u = row.get("u").and_then(Value::as_f64).unwrap_or(0.0);
        let today_usd = (u * 1e6).round() / 1e6;
        let calls = row.get("c").and_then(Value::as_i64).unwrap_or(0);
        let by_tier = query_rows(db, "SELECT tier, COALESCE(SUM(cost_usd_actual),0) AS usd, COUNT(*) AS n FROM brain.ai_calls WHERE ts >= date('now','start of day') GROUP BY tier ORDER BY usd DESC", &[])?;
        Ok((today_usd, calls, by_tier))
    })();
    let (today_usd, calls, by_tier) = attempt.unwrap_or((0.0, 0, json!([])));
    json!({ "today_usd": today_usd, "calls": calls, "by_tier": by_tier })
}

// ── board: pure DB read, graceful-degrade to empty cards on any query error ──
fn produce_board(db: &Connection) -> Value {
    let cards = query_rows(db, "SELECT slug, title, status, score FROM cards WHERE status NOT IN ('archived','wontfix') ORDER BY COALESCE(score,0) DESC, id DESC LIMIT 40", &[]).unwrap_or(json!([]));
    json!({ "cards": cards })
}

// ── queue: the SAME read the driver loop uses (driver-queue), degrades to an empty queue on error ──
fn produce_queue(db: &Connection) -> Value {
    match crate::system_driver_queue::driverQueue(&json!({}), db) {
        Ok(v) => v,
        Err(_) => json!({ "next_card": null, "next": null, "queue": [], "counts": { "total": 0, "dispatchable": 0, "blocked": 0 } }),
    }
}

// ── improvements: AI-shaped but seeded from deterministic facts; ONE try wraps scope-resolve +
// atlas-code-sample (any Err falls to the structural fallback, mirroring JS's empty catch). ──
fn produce_improvements(args: &Value, db: &Connection) -> Value {
    let origin = if truthy(args.get("origin")) { js_string(args.get("origin").unwrap()) } else { "atlas".to_string() };

    let grounded: Result<Option<Value>, String> = (|| {
        let scope = if args.get("scope").and_then(Value::as_str) == Some("codebase") { "codebase" } else { "conversation" };
        let request = coalesce(&[args.get("request"), args.get("query")]).cloned().unwrap_or(Value::Null);
        let wsr_input = json!({
            "scope": scope,
            "request": request,
            "response": args.get("response").cloned().unwrap_or(Value::Null),
            "path": args.get("path").cloned().unwrap_or(Value::Null),
            "paths": args.get("paths").cloned().unwrap_or(Value::Null),
            "origin": origin,
            "max_files": 4,
            "per_file_bytes": 4000,
        });
        let resolved = crate::agents_in_widget_scope_resolve::widgetScopeResolve(&wsr_input, db)?;
        let files = resolved.get("files").cloned().unwrap_or(json!([]));

        let ask = coalesce(&[args.get("request"), args.get("ask"), args.get("query")])
            .cloned()
            .unwrap_or_else(|| json!("Suggest concrete improvements with before/after samples."));
        let cs_input = json!({
            "ask": ask,
            "path": nz(args.get("path")).cloned().unwrap_or(Value::Null),
            "origin": origin,
            "files": files,
        });
        let cs = crate::codebase_atlas_code_sample::atlasCodeSample(&cs_input, db)?;
        let ok = cs.get("ok").and_then(Value::as_bool) == Some(true);
        let has_improvements = cs.get("improvements").and_then(Value::as_array).map_or(false, |a| !a.is_empty());
        if ok && has_improvements {
            return Ok(Some(json!({
                "summary": nz(cs.get("summary")).cloned().unwrap_or(Value::Null),
                "improvements": cs.get("improvements").cloned().unwrap_or(json!([])),
                "diagram": nz(cs.get("diagram")).cloned().unwrap_or(Value::Null),
            })));
        }
        Ok(None)
    })();

    if let Ok(Some(v)) = grounded { return v; }
    produce_improvements_fallback(&origin, db)
}

// FALLBACK: deterministic structural findings — cycles/orphans/low-maintainability files.
fn produce_improvements_fallback(origin: &str, db: &Connection) -> Value {
    let mut items: Vec<Value> = Vec::new();
    let mut atlas_files_set: HashSet<String> = HashSet::new();
    let mut atlas_files: Vec<String> = Vec::new();

    let site = crate::codebase_code_docs_build::codeDocsBuild(&json!({ "build": true, "origin": origin }), db).ok();

    if let Ok(rows) = query_rows(db, "SELECT DISTINCT path FROM code_nodes WHERE origin = ? AND path IS NOT NULL", &[json!(origin)]) {
        if let Some(arr) = rows.as_array() {
            for r in arr {
                let p = js_string(r.get("path").unwrap_or(&Value::Null));
                if atlas_files_set.insert(p.clone()) { atlas_files.push(p); }
            }
        }
    }

    if let Ok(s) = crate::codebase_atlas_smells::atlasSmells(&json!({ "origin": origin }), db) {
        let smells = s.get("smells").and_then(Value::as_array).cloned().unwrap_or_default();
        for sm in smells.into_iter().take(8) {
            let kind = js_string(sm.get("kind").unwrap_or(&Value::Null));
            let file = match sm.get("nodes") {
                Some(Value::Array(a)) => js_string(a.first().unwrap_or(&Value::Null)),
                other => js_string(other.unwrap_or(&Value::Null)),
            };
            let category = if sm.get("severity").and_then(Value::as_str) == Some("high") { "correctness" } else { "structure" };
            let rationale_raw = nz(sm.get("detail")).map(js_string).unwrap_or_else(|| format!("{kind} detected"));
            let rationale: String = rationale_raw.chars().take(300).collect();
            items.push(json!({ "target": format!("Fix the {kind}"), "file": file, "category": category, "rationale": rationale }));
        }
    }

    if let Some(site) = &site {
        if let Some(pages) = site.get("pages").and_then(Value::as_object) {
            // (path, grade, mi) — collected then sorted by mi ascending, worst-maintainability first.
            let mut worst: Vec<(String, String, Value)> = Vec::new();
            for p in pages.values() {
                if p.get("kind").and_then(Value::as_str) != Some("file") { continue; }
                let grade = match p.get("metrics").and_then(|m| m.get("grade")).and_then(Value::as_str) {
                    Some(g) if !g.is_empty() && "DF".contains(g) => g.to_string(),
                    _ => continue,
                };
                let mi_v = p.get("metrics").and_then(|m| m.get("mi")).cloned().unwrap_or(Value::Null);
                let path = p.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                worst.push((path, grade, mi_v));
            }
            worst.sort_by(|a, b| {
                let am = a.2.as_f64().unwrap_or(f64::NAN);
                let bm = b.2.as_f64().unwrap_or(f64::NAN);
                am.partial_cmp(&bm).unwrap_or(std::cmp::Ordering::Equal)
            });
            for (path, grade, mi_v) in worst.into_iter().take(4) {
                let base = path.rsplit('/').next().unwrap_or(&path).to_string();
                let mi_disp = js_string(&mi_v);
                items.push(json!({
                    "target": format!("Improve maintainability of {base}"),
                    "file": path,
                    "category": "maintainability",
                    "rationale": format!("Grade {grade} (maintainability {mi_disp}/100) — high complexity and/or size."),
                }));
            }
        }
    }

    let summary = if items.is_empty() {
        "No structural smells or low-maintainability files found.".to_string()
    } else {
        format!("{} concrete improvement{} from the codebase's real smells + lowest-maintainability files.", items.len(), if items.len() == 1 { "" } else { "s" })
    };
    json!({ "summary": summary, "improvements": items, "_atlasFiles": atlas_files })
}

// ── code-sample: PURELY deterministic file read; widgetScopeResolve is NOT wrapped in JS — a
// failure propagates to the caller (mirrored via `?`). ──
fn produce_code_sample(args: &Value, db: &Connection) -> Result<Value, String> {
    let scope = if args.get("scope").and_then(Value::as_str) == Some("codebase") { "codebase" } else { "conversation" };
    let request = coalesce(&[args.get("request"), args.get("query")]).cloned().unwrap_or(Value::Null);
    let wsr_input = json!({
        "scope": scope,
        "request": request,
        "response": args.get("response").cloned().unwrap_or(Value::Null),
        "path": args.get("path").cloned().unwrap_or(Value::Null),
        "paths": args.get("paths").cloned().unwrap_or(Value::Null),
        "origin": args.get("origin").cloned().unwrap_or(Value::Null),
        "max_files": 1,
        "per_file_bytes": 12000,
    });
    let resolved = crate::agents_in_widget_scope_resolve::widgetScopeResolve(&wsr_input, db)?;
    let files = resolved.get("files").and_then(Value::as_array).cloned().unwrap_or_default();
    match files.first() {
        None => Ok(json!({ "file": null, "code": "", "language": "text", "note": "No indexed code yet — build the Atlas for this workspace." })),
        Some(f) => {
            let path_v = f.get("path").cloned().unwrap_or(Value::Null);
            let source = f.get("source").and_then(Value::as_str).unwrap_or("").to_string();
            let lang = lang_of(&js_string(&path_v));
            Ok(json!({ "file": path_v, "code": source, "language": lang }))
        }
    }
}

// ── diagram: widgetScopeResolve is NOT wrapped in JS (propagates via `?`); only the AI call is. ──
fn produce_diagram(args: &Value, db: &Connection) -> Result<Value, String> {
    let scope = if args.get("scope").and_then(Value::as_str) == Some("codebase") { "codebase" } else { "conversation" };
    let request = str_qq(coalesce(&[args.get("request"), args.get("query")]));
    let response = str_qq(args.get("response"));
    let wsr_input = json!({
        "scope": scope,
        "request": request,
        "response": response,
        "path": args.get("path").cloned().unwrap_or(Value::Null),
        "paths": args.get("paths").cloned().unwrap_or(Value::Null),
        "origin": args.get("origin").cloned().unwrap_or(Value::Null),
        "max_files": 6,
        "per_file_bytes": 4000,
    });
    let resolved = crate::agents_in_widget_scope_resolve::widgetScopeResolve(&wsr_input, db)?;
    let files = resolved.get("files").and_then(Value::as_array).cloned().unwrap_or_default();
    if files.is_empty() {
        return Ok(json!({ "diagram": null, "note": "No indexed code to diagram — build the Atlas for this workspace." }));
    }
    let ask = if scope == "conversation" && (!request.is_empty() || !response.is_empty()) {
        let resp_slice: String = response.chars().take(1200).collect();
        format!("The user asked: {request}\n\nThe answer so far: {resp_slice}")
    } else if !request.is_empty() {
        request.clone()
    } else {
        "Explain the overall architecture and data flow of these files.".to_string()
    };
    match crate::codebase_atlas_architecture_diagram::atlasArchitectureDiagram(&json!({ "files": files, "ask": ask }), db) {
        Ok(dr) => {
            let ok = dr.get("ok").and_then(Value::as_bool) == Some(true);
            Ok(json!({
                "diagram": if ok { dr.get("diagram").cloned().unwrap_or(Value::Null) } else { Value::Null },
                "error": if ok { Value::Null } else { nz(dr.get("error")).cloned().unwrap_or(Value::Null) },
            }))
        }
        Err(e) => {
            let msg: String = e.chars().take(160).collect();
            Ok(json!({ "diagram": null, "error": msg }))
        }
    }
}

// ── logic-spec: only the AI call is wrapped; no scope resolution involved. ──
fn produce_logic_spec(args: &Value, db: &Connection) -> Value {
    let intent_raw = str_qq(coalesce(&[args.get("request"), args.get("query"), args.get("response")]));
    let intent = intent_raw.trim().to_string();
    if intent.is_empty() {
        return json!({ "spec": null, "note": "No intent to design from — ask what to build." });
    }
    match crate::codebase_atlas_logic_spec::atlasLogicSpec(&json!({ "intent": intent }), db) {
        Ok(r) => {
            let ok = r.get("ok").and_then(Value::as_bool) == Some(true);
            json!({
                "spec": if ok { r.get("spec").cloned().unwrap_or(Value::Null) } else { Value::Null },
                "error": if ok { Value::Null } else { nz(r.get("error")).cloned().unwrap_or(Value::Null) },
            })
        }
        Err(e) => {
            let msg: String = e.chars().take(160).collect();
            json!({ "spec": null, "error": msg })
        }
    }
}

fn lint_improvements(p: &Value) -> Vec<Value> {
    let Some(improvements) = p.get("improvements").and_then(Value::as_array) else {
        return vec![json!("improvements must be an array")];
    };
    let known: HashSet<String> = p.get("_atlasFiles").and_then(Value::as_array).cloned().unwrap_or_default()
        .iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
    let mut errs = Vec::new();
    if !known.is_empty() {
        for it in improvements {
            if let Some(file) = it.get("file").and_then(Value::as_str) {
                if !file.is_empty() && !known.contains(file) {
                    errs.push(json!(format!("improvement cites unknown file: {file}")));
                }
            }
        }
    }
    errs
}

fn lint_for(widget_name: &str, p: &Value) -> Vec<Value> {
    match widget_name {
        "memory" => if p.get("hits").map_or(false, Value::is_array) { vec![] } else { vec![json!("hits must be an array")] },
        "cost" => if p.get("today_usd").map_or(false, Value::is_number) { vec![] } else { vec![json!("today_usd must be a number")] },
        "board" => if p.get("cards").map_or(false, Value::is_array) { vec![] } else { vec![json!("cards must be an array")] },
        "queue" => if p.get("queue").map_or(false, Value::is_array) { vec![] } else { vec![json!("queue must be an array")] },
        "improvements" => lint_improvements(p),
        "code-sample" => if p.get("code").map_or(false, Value::is_string) { vec![] } else { vec![json!("code must be a string")] },
        "diagram" => {
            let d = p.get("diagram");
            let ok = d.map_or(true, Value::is_null) || d.and_then(|v| v.get("nodes")).map_or(false, Value::is_array);
            if ok { vec![] } else { vec![json!("diagram must be null or carry nodes[]")] }
        }
        "logic-spec" => {
            let s = p.get("spec");
            let ok = s.map_or(true, Value::is_null) || s.and_then(|v| v.get("steps")).map_or(false, Value::is_array);
            if ok { vec![] } else { vec![json!("spec must be null or carry steps[]")] }
        }
        _ => vec![],
    }
}

#[allow(non_snake_case)]
pub fn widgetGenerate(input: &Value, db: &Connection) -> Result<Value, String> {
    // `input?.name` — has_key distinguishes JS `undefined` (no key) from an explicit `null`, only
    // relevant for the `String(name)` interpolation in the unknown-widget error message.
    let name_val = input.get("name").cloned().unwrap_or(Value::Null);
    let has_name_key = input.get("name").is_some();
    let widget_name = input.get("name").and_then(Value::as_str).filter(|n| AVAILABLE.contains(n));

    let Some(widget_name) = widget_name else {
        let name_display = if has_name_key { js_string(&name_val) } else { "undefined".to_string() };
        return Ok(json!({
            "ok": false,
            "error": format!("unknown widget '{name_display}'"),
            "available": AVAILABLE,
        }));
    };

    let args = input.get("args").cloned().unwrap_or(json!({}));

    let (kind, payload): (&str, Value) = match widget_name {
        "memory" => ("memory-search", produce_memory(&args, db)),
        "cost" => ("cost", produce_cost(db)),
        "board" => ("board", produce_board(db)),
        "queue" => ("driver-queue", produce_queue(db)),
        "improvements" => ("improvements", produce_improvements(&args, db)),
        "code-sample" => ("code", produce_code_sample(&args, db)?),
        "diagram" => ("diagram", produce_diagram(&args, db)?),
        "logic-spec" => ("logic-spec", produce_logic_spec(&args, db)),
        _ => unreachable!("widget_name filtered against AVAILABLE above"),
    };

    let lints = lint_for(widget_name, &payload);
    let mut payload = payload;
    if let Value::Object(m) = &mut payload { m.remove("_atlasFiles"); } // internal lint scratch, never shipped

    Ok(json!({
        "ok": lints.is_empty(),
        "kind": kind,
        "name": name_val,
        "payload": payload,
        "lints": lints,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory; ATTACH ':memory:' AS brain; ATTACH ':memory:' AS code; ATTACH ':memory:' AS judgments;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL UNIQUE, title TEXT, status TEXT, priority INTEGER, score REAL, body_json TEXT, driver_intent TEXT NOT NULL DEFAULT 'auto', created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT, title TEXT, situation TEXT, guidance TEXT, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')));
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), tier TEXT, cost_usd_actual REAL);
             CREATE TABLE code_nodes (id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, type TEXT, path TEXT);
             CREATE TABLE code_edges (origin TEXT, type TEXT, from_node TEXT, to_node TEXT, kind TEXT);
             CREATE TABLE workspaces (root_path TEXT, is_active INTEGER);
             CREATE TABLE code_decorators (path TEXT, name TEXT, kind TEXT, role TEXT, line INTEGER, end_line INTEGER, signature TEXT, summary TEXT, inputs_json TEXT, output_json TEXT, effects_json TEXT, calls_json TEXT, uses_json TEXT, complexity INTEGER, flags_json TEXT, steps_json TEXT, origin TEXT);
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT NOT NULL DEFAULT '2026-01-01 00:00:00', parent_event_id INTEGER, actor TEXT NOT NULL, agent TEXT, action TEXT, skill TEXT, duration_ms REAL, outcome TEXT NOT NULL, error_code TEXT, context_json TEXT, user_goal_id INTEGER, source TEXT);",
        ).unwrap();
        c
    }

    #[test]
    fn unknown_widget_lists_available_names() {
        let c = setup();
        let out = widgetGenerate(&json!({ "name": "nope" }), &c).unwrap();
        assert_eq!(out["ok"], json!(false));
        assert_eq!(out["error"], json!("unknown widget 'nope'"));
        assert_eq!(out["available"], json!(AVAILABLE));
    }

    #[test]
    fn missing_name_key_renders_undefined_in_error() {
        let c = setup();
        let out = widgetGenerate(&json!({}), &c).unwrap();
        assert_eq!(out["error"], json!("unknown widget 'undefined'"));
    }

    #[test]
    fn explicit_null_name_renders_null_in_error() {
        let c = setup();
        let out = widgetGenerate(&json!({ "name": Value::Null }), &c).unwrap();
        assert_eq!(out["error"], json!("unknown widget 'null'"));
    }

    #[test]
    fn memory_widget_no_query_returns_recent_hits() {
        let c = setup();
        c.execute("INSERT INTO memory.memories (type, title, situation, guidance) VALUES ('decision','T1','sit  one','guide  one')", []).unwrap();
        let out = widgetGenerate(&json!({ "name": "memory" }), &c).unwrap();
        assert_eq!(out["ok"], json!(true));
        assert_eq!(out["kind"], json!("memory-search"));
        assert_eq!(out["payload"]["query"], json!(Value::Null));
        assert_eq!(out["payload"]["hits"][0]["snippet"], json!("guide one"));
        assert_eq!(out["lints"], json!([]));
    }

    #[test]
    fn memory_widget_query_filters_and_trims() {
        let c = setup();
        c.execute("INSERT INTO memory.memories (type, title, situation, guidance) VALUES ('decision','Auth flow','x','y')", []).unwrap();
        c.execute("INSERT INTO memory.memories (type, title, situation, guidance) VALUES ('decision','Unrelated','x','y')", []).unwrap();
        let out = widgetGenerate(&json!({ "name": "memory", "args": { "query": "  Auth  " } }), &c).unwrap();
        assert_eq!(out["payload"]["query"], json!("Auth"));
        assert_eq!(out["payload"]["hits"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn cost_widget_defaults_to_zero_when_empty() {
        let c = setup();
        let out = widgetGenerate(&json!({ "name": "cost" }), &c).unwrap();
        assert_eq!(out["kind"], json!("cost"));
        assert_eq!(out["payload"]["today_usd"], json!(0.0));
        assert_eq!(out["payload"]["calls"], json!(0));
        assert_eq!(out["payload"]["by_tier"], json!([]));
    }

    #[test]
    fn board_widget_excludes_archived_and_wontfix() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, status, score) VALUES ('a','A','doing',5)", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, status, score) VALUES ('b','B','archived',9)", []).unwrap();
        let out = widgetGenerate(&json!({ "name": "board" }), &c).unwrap();
        let cards = out["payload"]["cards"].as_array().unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["slug"], json!("a"));
    }

    #[test]
    fn queue_widget_degrades_to_empty_when_driver_schedule_errors() {
        // no `contracts` table (driver-schedule needs it downstream) -> driverQueue Errs -> defaults.
        let c = setup();
        let out = widgetGenerate(&json!({ "name": "queue" }), &c).unwrap();
        assert_eq!(out["kind"], json!("driver-queue"));
        assert_eq!(out["payload"]["queue"], json!([]));
        assert_eq!(out["payload"]["counts"]["total"], json!(0));
    }

    #[test]
    fn code_sample_widget_no_atlas_returns_note() {
        let c = setup();
        let out = widgetGenerate(&json!({ "name": "code-sample" }), &c).unwrap();
        assert_eq!(out["kind"], json!("code"));
        assert_eq!(out["payload"]["code"], json!(""));
        assert_eq!(out["payload"]["language"], json!("text"));
        assert!(out["payload"]["note"].as_str().unwrap().contains("No indexed code"));
        assert_eq!(out["ok"], json!(true));
    }

    #[test]
    fn diagram_widget_no_atlas_returns_note() {
        let c = setup();
        let out = widgetGenerate(&json!({ "name": "diagram" }), &c).unwrap();
        assert_eq!(out["kind"], json!("diagram"));
        assert_eq!(out["payload"]["diagram"], json!(Value::Null));
        assert!(out["payload"]["note"].as_str().unwrap().contains("No indexed code to diagram"));
    }

    #[test]
    fn logic_spec_widget_no_intent_returns_note() {
        let c = setup();
        let out = widgetGenerate(&json!({ "name": "logic-spec" }), &c).unwrap();
        assert_eq!(out["kind"], json!("logic-spec"));
        assert_eq!(out["payload"]["spec"], json!(Value::Null));
        assert!(out["payload"]["note"].as_str().unwrap().contains("No intent"));
    }

    #[test]
    fn logic_spec_widget_with_intent_hits_parity_deferred_boundary() {
        let c = setup();
        let out = widgetGenerate(&json!({ "name": "logic-spec", "args": { "request": "design a queue" } }), &c).unwrap();
        assert_eq!(out["payload"]["spec"], json!(Value::Null));
        assert!(out["payload"]["error"].as_str().is_some());
    }

    #[test]
    fn improvements_widget_falls_back_to_structural_when_no_atlas() {
        let c = setup();
        let out = widgetGenerate(&json!({ "name": "improvements" }), &c).unwrap();
        assert_eq!(out["kind"], json!("improvements"));
        assert_eq!(out["payload"]["improvements"], json!([]));
        assert_eq!(out["payload"]["summary"], json!("No structural smells or low-maintainability files found."));
        assert!(out["payload"].get("_atlasFiles").is_none(), "internal lint scratch must never ship");
        assert_eq!(out["lints"], json!([]));
        assert_eq!(out["ok"], json!(true));
    }
}
