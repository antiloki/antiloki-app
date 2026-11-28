//! atlas-symbol-explain.rs — twin of skills/codebase/atlas-symbol-explain.mjs (db, cross-db
//! code.*/brain.* attach lane). NAMES ARE CONTRACT: default `atlasSymbolExplain`. A SHORT AI
//! explanation of ONE def (function/class/method) for the atlas "explain" button. Slices the
//! def's own span from the ACTIVE WORKSPACE source (via the already-converted
//! `codebase_codebase_file_read::codebaseFileRead` twin) and assembles the def's FILE CONTEXT
//! from the graph (the file's intent, sibling symbols, and this symbol's callers/callees) so the
//! low-tier model explains the symbol's ROLE IN ITS FILE, not as an isolated snippet. Both the
//! source slice and the file-context gather are best-effort per the JS's own try/catch shape:
//! an unreadable source returns `{ ok:false, error:"source unreadable: ..." }` immediately (no
//! aiCall attempted), while a failed context gather silently degrades to a bare `File: <path>`
//! line and the explain still runs.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, so
//! `atlasSymbolExplain` surfaces that as `{ ok:false, error:<clipped message> }` once the (real,
//! ported) deterministic surface above it has run: guards (db, node_id), the node lookup, the
//! source slice, the best-effort file-context gather, and `ensure_prompt` (reusing the already-
//! converted `ai_prompt_find`/`ai_prompt_add` twins, mirroring atlas-logic-spec.rs's/
//! atlas-file-snapshot.rs's ensure-prompt pattern). The response-parse (`pick`) helper and the
//! `base_name`/`short_id` id-compaction helpers are ported as their own PURE functions and unit-
//! tested directly with canned values — `pick` is unreachable from `atlasSymbolExplain` itself
//! (aiCall's `Err` short-circuits before it runs) but correct by construction, same pattern as
//! atlas-file-snapshot.rs's `pick`/`normalize`.
//!
//! `context.db` missing → JS throws `err.contextMissing('atlas-symbol-explain', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin). `context.provider_fn` (a JS function reference for provider
//! selection/DI) has no Rust equivalent to carry — threaded as `null` into the constructed
//! aiCall input, same as atlas-file-snapshot.rs's/atlas-logic-spec.rs's `provider_fn`.
// QUALITY: mixed unit — deterministic guard/node-lookup/source-slice/context-assembly surface
// (fully ported, fixture-reachable up to the deferred aiCall boundary) + 3 pure helpers (`pick`,
// `base_name`, `short_id`) unit-tested directly + 1 bespoke/deferred aiCall dispatch (no
// fixture/frontier).

use rusqlite::Connection;
use serde_json::{json, Value};

// ── standard helpers (playbook) ─────────────────────────────────────────────────────────────
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
} // = JS String(v ?? '')
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
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
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

const PROMPT_NAME: &str = "atlas.symbol.explain.v2";
const SYSTEM_MD: &str = "You explain a single code symbol IN THE CONTEXT OF THE FILE it lives in, for a developer reading the code. You output only strict JSON. Be concrete about the symbol's ROLE in the file, how it relates to the file's purpose and the other symbols/dependencies given, its inputs/outputs, and any non-obvious behavior or gotcha. Ground everything in the file context + source; invent nothing. No preamble.";
const TEMPLATE_MD: &str = "Explain this {{kind}} for a developer **in the context of the file it lives in** — not as an isolated snippet. Cover: its ROLE in the file, how it relates to the file's purpose and the other symbols / callers / callees listed, its inputs/outputs, and any non-obvious behavior or gotcha. Use the FILE CONTEXT to ground the explanation; do not merely restate the lines.

{{kind}}: {{name}}   ({{path}})
signature: {{signature}}

FILE CONTEXT:
{{file_context}}

THE {{kind}}'S OWN SOURCE:
{{source}}

Return JSON only as exactly: {\"explanation\":\"<3-5 sentences, grounded in the file context>\"}";

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "explanation", "field_type": "string", "required": true, "min_length": 20 },
    ])
}

// mirrors JS ensurePrompt: `existing && existing.template_md === TEMPLATE` short-circuits;
// else promptAdd re-registers (a new version, since name+template differs from the live row).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db) {
        if existing.get("template_md").and_then(Value::as_str) == Some(TEMPLATE_MD) {
            return Ok(());
        }
    }
    let _ = crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": TEMPLATE_MD,
            "system_md": SYSTEM_MD,
            "default_tier": "low",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 440,
            "output_fields": prompt_output_fields(),
        }),
        db,
    );
    Ok(())
}

// `r?.output_kv?.find(o => o.field_name === 'explanation' && o.parent_output_id == null)?.
// value_text ?? null` — pure, unreachable from atlasSymbolExplain itself (aiCall's Err
// short-circuits before this ever runs) but unit-tested directly per the playbook.
fn pick(r: &Value) -> Value {
    let rows = match r.get("output_kv").and_then(Value::as_array) {
        Some(a) => a,
        None => return Value::Null,
    };
    for row in rows {
        let field_name = row.get("field_name").and_then(Value::as_str);
        // `o.parent_output_id == null` — JS loose equality matches BOTH null and undefined.
        let parent_is_null = row.get("parent_output_id").map_or(true, |v| v.is_null());
        if field_name == Some("explanation") && parent_is_null {
            return nz(row.get("value_text")).cloned().unwrap_or(Value::Null);
        }
    }
    Value::Null
}

// `const baseName = (p) => String(p).split(/[\\/]/).pop() || String(p);` — pop() of a
// trailing-empty split (e.g. path ends in a separator) is falsy, so it falls back to the
// full string.
fn base_name(p: &str) -> String {
    let parts: Vec<&str> = p.split(['\\', '/']).collect();
    match parts.last() {
        Some(&last) if !last.is_empty() => last.to_string(),
        _ => p.to_string(),
    }
}

// `const shortId = (id) => { const s = String(id); const h = s.indexOf('#'); return h < 0 ?
// baseName(s) : baseName(s.slice(0,h)) + '#' + s.slice(h+1); }`
fn short_id(id: &str) -> String {
    match id.find('#') {
        None => base_name(id),
        Some(h) => format!("{}#{}", base_name(&id[..h]), &id[h + 1..]),
    }
}

// JS `Array.prototype.slice(start, end)` index clamping — negative counts from the end,
// out-of-range clamps to [0, len], start>=end yields empty.
fn js_slice<'a>(arr: &[&'a str], start: i64, end: i64) -> Vec<&'a str> {
    let len = arr.len() as i64;
    let clamp = |i: i64| if i < 0 { (len + i).max(0) } else { i.min(len) };
    let (s, e) = (clamp(start), clamp(end));
    if s >= e { Vec::new() } else { arr[s as usize..e as usize].to_vec() }
}

// `span?.start_line && span?.end_line ? file.content.split('\n').slice(Number(span.start_line)-1,
// Number(span.end_line)).join('\n') : file.content` then `.slice(0, 6000)`.
fn source_from_span(content: &str, span_json: Option<&str>) -> String {
    let span: Option<Value> = span_json.and_then(|s| serde_json::from_str(s).ok());
    let sliced = match &span {
        Some(sp) if truthy(sp.get("start_line")) && truthy(sp.get("end_line")) => {
            let lines: Vec<&str> = content.split('\n').collect();
            let start = js_number(sp.get("start_line")) as i64 - 1;
            let end = js_number(sp.get("end_line")) as i64;
            js_slice(&lines, start, end).join("\n")
        }
        _ => content.to_string(),
    };
    clip(&sliced, 6000)
}

pub fn atlasSymbolExplain(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw
    // err.contextMissing('atlas-symbol-explain', 'db');` — unreachable here, `db` is a required
    // reference in every db-lane call site (same convention as every sibling twin).
    let origin = nz(input.get("origin")).cloned().unwrap_or(json!("atlas"));
    let node_id_val = input.get("node_id").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&node_id_val)) {
        return Err("atlas-symbol-explain: node_id is required".into());
    }
    let node_id_s = str_qq(Some(&node_id_val));

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, path, type, name, signature, span_json FROM code.code_nodes WHERE origin=? AND type<>'file' AND id=? LIMIT 1",
        &[origin.clone(), json!(node_id_s)],
    )?;
    let node = match rows.as_array().and_then(|a| a.first()).cloned() {
        Some(n) => n,
        None => return Err(format!("atlas-symbol-explain: symbol node '{node_id_s}' not found")),
    };
    let node_id_out = node.get("id").cloned().unwrap_or(Value::Null);
    let node_name = node.get("name").cloned().unwrap_or(Value::Null);
    let node_path = node.get("path").cloned().unwrap_or(Value::Null);

    // slice the def's own lines from the workspace source (span pointer → line range).
    let read_input = json!({ "path": node_path.clone() });
    let file = match crate::codebase_codebase_file_read::codebaseFileRead(&read_input, db) {
        Ok(f) => f,
        Err(e) => {
            return Ok(json!({
                "node_id": node_id_out, "name": node_name, "explanation": Value::Null,
                "ok": false, "error": format!("source unreadable: {}", clip(&e, 140)),
            }));
        }
    };
    let content = str_qq(file.get("content"));
    let span_json_str = node.get("span_json").and_then(Value::as_str);
    let source = source_from_span(&content, span_json_str);

    // EPIC-ATLAS-INSIGHTS (explain-in-file-context): pull the file's PURPOSE, the other symbols
    // it defines, and this symbol's callers/callees from the graph, so the explanation situates
    // the def in its file rather than reading it in isolation. Best-effort — any failure
    // degrades to def-only (the JS wraps this whole block in try/catch).
    let mut file_context = format!("File: {}", str_qq(Some(&node_path)));
    let ctx: Result<String, String> = (|| {
        let file_rows = crate::db_json::query_rows(
            db,
            "SELECT intent FROM code.code_nodes WHERE origin=? AND type='file' AND path=? LIMIT 1",
            &[origin.clone(), node_path.clone()],
        )?;
        let intent = file_rows.as_array().and_then(|a| a.first()).and_then(|r| r.get("intent")).cloned();

        let sib_rows = crate::db_json::query_rows(
            db,
            "SELECT name, type FROM code.code_nodes WHERE origin=? AND path=? AND type<>'file' AND id<>? ORDER BY id LIMIT 40",
            &[origin.clone(), node_path.clone(), json!(node_id_s)],
        )?;
        let siblings = sib_rows.as_array().cloned().unwrap_or_default();

        let callee_rows = crate::db_json::query_rows(
            db,
            "SELECT DISTINCT to_node FROM code.code_edges WHERE origin=? AND from_node=? AND type IN ('calls','uses','dispatches') LIMIT 20",
            &[origin.clone(), node_id_out.clone()],
        )?;
        let callees: Vec<String> = callee_rows.as_array().cloned().unwrap_or_default()
            .into_iter().map(|r| str_qq(r.get("to_node"))).collect();

        let caller_rows = crate::db_json::query_rows(
            db,
            "SELECT DISTINCT from_node FROM code.code_edges WHERE origin=? AND to_node=? AND type IN ('calls','uses','dispatches') LIMIT 20",
            &[origin.clone(), node_id_out.clone()],
        )?;
        let callers: Vec<String> = caller_rows.as_array().cloned().unwrap_or_default()
            .into_iter().map(|r| str_qq(r.get("from_node"))).collect();

        let sib_list = if siblings.is_empty() {
            "(none)".to_string()
        } else {
            siblings.iter().map(|s| {
                let name = s.get("name").and_then(Value::as_str).unwrap_or("?").to_string();
                let ty = s.get("type").and_then(Value::as_str);
                match ty {
                    Some(t) if t != "symbol" => format!("{name} ({t})"),
                    _ => name,
                }
            }).collect::<Vec<_>>().join(", ")
        };

        // `${node.name ?? 'This symbol'}` — nullish coalescing, not truthy.
        let name_for_lines = nz(Some(&node_name)).map(|v| str_qq(Some(v))).unwrap_or_else(|| "This symbol".to_string());
        let mut lines = vec![
            format!("File: {}", str_qq(Some(&node_path))),
            format!("Purpose: {}", if truthy(intent.as_ref()) { clip(&str_qq(intent.as_ref()), 400) } else { "(no recorded intent)".to_string() }),
            format!("Other symbols defined in this file: {sib_list}"),
        ];
        if !callers.is_empty() {
            lines.push(format!("{name_for_lines} is called/used by: {}", callers.iter().map(|c| short_id(c)).collect::<Vec<_>>().join(", ")));
        }
        if !callees.is_empty() {
            lines.push(format!("{name_for_lines} calls/uses: {}", callees.iter().map(|c| short_id(c)).collect::<Vec<_>>().join(", ")));
        }
        Ok(lines.join("\n"))
    })();
    if let Ok(c) = ctx {
        file_context = c;
    }

    ensure_prompt(db)?;

    // JS `??` (nullish coalescing) — only null/undefined trigger the fallback; falsy-but-present
    // values (empty string, 0) pass through as-is. Use `nz` (filters Value::Null only), not
    // `truthy`, to match.
    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "kind", "value": nz(node.get("type")).cloned().unwrap_or(json!("symbol")) },
            { "key": "name", "value": nz(Some(&node_name)).cloned().unwrap_or(node_id_out.clone()) },
            { "key": "path", "value": nz(Some(&node_path)).cloned().unwrap_or(json!("")) },
            { "key": "signature", "value": nz(node.get("signature")).cloned().unwrap_or(json!("(none)")) },
            { "key": "file_context", "value": file_context },
            { "key": "source", "value": source },
        ],
        "tier": "low", "temperature": "cold", "phase_flow": "single_only", "provider_fn": Value::Null,
    });
    // PARITY-DEFERRED boundary — the real JS wraps this in a try/catch and returns
    // `{ ok:false, error:<clipped message> }` on ANY aiCall failure. `crate::ai_ai_call::aiCall`
    // always returns `Err`, so that branch is the only one reachable here.
    let ai_result = match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(r) => r,
        Err(e) => {
            return Ok(json!({
                "node_id": node_id_out, "name": node_name, "explanation": Value::Null,
                "ok": false, "error": clip(&e, 160),
            }));
        }
    };

    let raw = pick(&ai_result);
    let explanation = if truthy(Some(&raw)) && !str_qq(Some(&raw)).trim().is_empty() {
        Some(str_qq(Some(&raw)).trim().to_string())
    } else {
        None
    };
    match explanation {
        None => Ok(json!({
            "node_id": node_id_out, "name": node_name, "explanation": Value::Null,
            "ok": false, "error": "AI returned no explanation",
        })),
        Some(e) => Ok(json!({ "node_id": node_id_out, "name": node_name, "explanation": e, "ok": true })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_schema(c: &Connection) {
        c.execute_batch(
            "ATTACH ':memory:' AS code; ATTACH ':memory:' AS brain; \
             CREATE TABLE workspaces (root_path TEXT, is_active INTEGER); \
             CREATE TABLE code.code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, span_json TEXT, intent TEXT); \
             CREATE TABLE code.code_edges (origin TEXT, from_node TEXT, to_node TEXT, type TEXT); \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
    }

    fn workspace_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("atlas-symbol-explain-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_node_id_errs() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let e = atlasSymbolExplain(&json!({}), &c).unwrap_err();
        assert_eq!(e, "atlas-symbol-explain: node_id is required");
    }

    #[test]
    fn unknown_node_id_errs_not_found() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let e = atlasSymbolExplain(&json!({ "node_id": "nope.ts#f" }), &c).unwrap_err();
        assert_eq!(e, "atlas-symbol-explain: symbol node 'nope.ts#f' not found");
    }

    #[test]
    fn file_type_node_is_excluded_by_type_ne_file() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        c.execute("INSERT INTO code.code_nodes (id,origin,path,type,name,signature,span_json,intent) VALUES ('a.ts@atlas','atlas','a.ts','file','a.ts',NULL,NULL,'purpose')", []).unwrap();
        let e = atlasSymbolExplain(&json!({ "node_id": "a.ts@atlas" }), &c).unwrap_err();
        assert_eq!(e, "atlas-symbol-explain: symbol node 'a.ts@atlas' not found");
    }

    #[test]
    fn cannot_read_source_returns_ok_false_before_aicall() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        c.execute("INSERT INTO code.code_nodes (id,origin,path,type,name,signature,span_json,intent) VALUES ('a.ts#f','atlas','does/not/exist.ts','function','f','()=>void',NULL,NULL)", []).unwrap();
        let r = atlasSymbolExplain(&json!({ "node_id": "a.ts#f" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("source unreadable"));
        // aiCall never dispatched → no prompt row registered.
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.symbol.explain.v2'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_after_full_deterministic_surface() {
        // proves guard/node-lookup/source-slice/context-gather/ensure_prompt all ran cleanly and
        // the aiCall failure is surfaced as ok:false (not an earlier bug).
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let dir = workspace_dir();
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES (?1, 1)", [dir.to_string_lossy().to_string()]).unwrap();
        std::fs::write(dir.join("a.ts"), "line1\nexport function f() {}\nline3\n").unwrap();
        c.execute("INSERT INTO code.code_nodes (id,origin,path,type,name,signature,span_json,intent) VALUES ('a.ts@atlas','atlas','a.ts','file','a.ts',NULL,NULL,'does a thing')", []).unwrap();
        c.execute("INSERT INTO code.code_nodes (id,origin,path,type,name,signature,span_json,intent) VALUES ('a.ts#f','atlas','a.ts','function','f','()=>void','{\"start_line\":2,\"end_line\":2}',NULL)", []).unwrap();
        c.execute("INSERT INTO code.code_nodes (id,origin,path,type,name,signature,span_json,intent) VALUES ('a.ts#g','atlas','a.ts','function','g','()=>void',NULL,NULL)", []).unwrap();
        c.execute("INSERT INTO code.code_edges (origin, from_node, to_node, type) VALUES ('atlas','a.ts#f','a.ts#g','calls')", []).unwrap();
        c.execute("INSERT INTO code.code_edges (origin, from_node, to_node, type) VALUES ('atlas','a.ts#g','a.ts#f','calls')", []).unwrap();

        let r = atlasSymbolExplain(&json!({ "node_id": "a.ts#f" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        let msg = r["error"].as_str().unwrap();
        assert!(msg.contains("ai-call"), "unexpected error: {msg}");
        assert_eq!(r["node_id"], json!("a.ts#f"));
        assert_eq!(r["name"], json!("f"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.symbol.explain.v2'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn context_gather_best_effort_falls_back_when_edges_table_missing() {
        // no code.code_edges table at all → the callers/callees query errors → the whole
        // best-effort closure returns Err → file_context stays the bare `File: <path>` default.
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS code; ATTACH ':memory:' AS brain; \
             CREATE TABLE workspaces (root_path TEXT, is_active INTEGER); \
             CREATE TABLE code.code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, span_json TEXT, intent TEXT); \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        let dir = workspace_dir();
        c.execute("INSERT INTO workspaces (root_path, is_active) VALUES (?1, 1)", [dir.to_string_lossy().to_string()]).unwrap();
        std::fs::write(dir.join("b.ts"), "export function f() {}\n").unwrap();
        c.execute("INSERT INTO code.code_nodes (id,origin,path,type,name,signature,span_json,intent) VALUES ('b.ts#f','atlas','b.ts','function','f','()=>void',NULL,NULL)", []).unwrap();
        let r = atlasSymbolExplain(&json!({ "node_id": "b.ts#f" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert!(r["error"].as_str().unwrap().contains("ai-call"));
    }

    // ── pure helpers, unreachable from atlasSymbolExplain (aiCall's Err short-circuits first) ──

    #[test]
    fn pick_finds_explanation_field_only() {
        let r = json!({ "output_kv": [
            { "field_name": "other", "value_text": "nope" },
            { "field_name": "explanation", "value_text": "  it does X  ", "parent_output_id": null },
        ] });
        assert_eq!(pick(&r), json!("  it does X  "));
        assert_eq!(pick(&json!({})), Value::Null);
        // parent_output_id present but non-null is excluded.
        let r2 = json!({ "output_kv": [
            { "field_name": "explanation", "value_text": "nested", "parent_output_id": 3 },
        ] });
        assert_eq!(pick(&r2), Value::Null);
    }

    #[test]
    fn base_name_strips_directories_both_separators_and_falls_back_on_trailing_sep() {
        assert_eq!(base_name("a/b/c.ts"), "c.ts");
        assert_eq!(base_name("a\\b\\c.ts"), "c.ts");
        assert_eq!(base_name("c.ts"), "c.ts");
        assert_eq!(base_name("a/b/"), "a/b/"); // pop() → "" (falsy) → fallback to full string
    }

    #[test]
    fn short_id_compacts_path_prefix_but_keeps_symbol_suffix() {
        assert_eq!(short_id("src/skills/codebase/atlas-symbol-explain.mjs#atlasSymbolExplain"), "atlas-symbol-explain.mjs#atlasSymbolExplain");
        assert_eq!(short_id("src/skills/codebase/atlas-symbol-explain.mjs"), "atlas-symbol-explain.mjs");
    }

    #[test]
    fn source_from_span_slices_the_given_line_range_or_falls_back_to_full_content() {
        let content = "line1\nline2\nline3\nline4";
        assert_eq!(source_from_span(content, Some("{\"start_line\":2,\"end_line\":3}")), "line2\nline3");
        assert_eq!(source_from_span(content, None), content);
        assert_eq!(source_from_span(content, Some("null")), content);
        assert_eq!(source_from_span(content, Some("{\"start_line\":0,\"end_line\":2}")), content); // start_line 0 is falsy
        assert_eq!(source_from_span(content, Some("not json")), content); // parse failure → span=null
        // 6000-char clip
        let long = "x".repeat(7000);
        assert_eq!(source_from_span(&long, None).chars().count(), 6000);
    }

    #[test]
    fn js_slice_matches_array_prototype_slice_clamping() {
        let arr = ["a", "b", "c", "d"];
        let refs: Vec<&str> = arr.to_vec();
        assert_eq!(js_slice(&refs, 1, 3), vec!["b", "c"]);
        assert_eq!(js_slice(&refs, 0, 100), vec!["a", "b", "c", "d"]);
        assert_eq!(js_slice(&refs, 3, 1), Vec::<&str>::new());
        assert_eq!(js_slice(&refs, -2, 100), vec!["c", "d"]);
    }
}
