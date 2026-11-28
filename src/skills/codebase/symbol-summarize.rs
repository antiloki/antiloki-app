//! symbol-summarize.rs — twin of skills/codebase/symbol-summarize.mjs (db lane, `code_nodes`
//! table on the plain `db` connection — no cross-db qualification, matching atlas-get.rs/
//! atlas-metrics.rs's unqualified `code_nodes` convention for this table). NAMES ARE CONTRACT:
//! default `symbolSummarize`. LAZY, cached AI prose for a SYMBOL node (function/class/method):
//! given a node id, ask a LOW-tier model for a one-line statement of what it does and cache it
//! in `code_nodes.intent`. Reads only the symbol's OWN span (via `span_json {file,start_line,
//! end_line}`), sliced straight off disk (not through a workspace-row indirection, unlike
//! atlas-symbol-explain.rs's `codebaseFileRead`) — cheap + scoped. Cached: a non-null `intent`
//! returns verbatim unless `force=true`; degrades gracefully (`ok:false`) on any failure.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, surfaced here as
//! `{ ok:false, error:<clipped message> }` once the (real, ported) deterministic surface above it
//! has run: guards (node_id), the node lookup, the cache short-circuit, the span-read source
//! slice, and `ensure_symbol_prompt` (reusing the already-converted `ai_prompt_find`/
//! `ai_prompt_add` twins, mirroring atlas-symbol-explain.rs's `ensure_prompt` pattern). The
//! response-parse helper (`pick_intent`) and the post-aiCall `UPDATE code_nodes SET intent`
//! write are ported byte-for-byte but are unreachable by this file's own fixture (they run only
//! after a successful `aiCall`, which the deferred stub never produces) — correct by inspection +
//! the same shape as atlas-symbol-explain.rs's `pick`.
//!
//! `context.db` missing → JS throws `err.contextMissing('symbol-summarize', 'db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention as every
//! sibling db-lane twin). `context.provider_fn` (a JS function reference for provider selection/
//! DI) has no Rust equivalent to carry — threaded as `null` into the constructed aiCall input,
//! same as atlas-symbol-explain.rs's `provider_fn`.
//!
//! REPO_ROOT: JS `resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', '..')` from
//! `skills/codebase/symbol-summarize.mjs` → the antiloki-core repo root. Ported the same way as
//! ai-prompt-path-grounding.rs's `repo_root()`: `CARGO_MANIFEST_DIR`'s parent, no env override.
// QUALITY: mixed unit — deterministic guard/cache/node-lookup/span-read/ensure-prompt surface
// (fully ported, fixture-reachable up to the deferred aiCall boundary) + 1 pure helper
// (`pick_intent`) unit-tested directly + 1 bespoke/deferred aiCall dispatch (no fixture/frontier).

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
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

const SOURCE_BUDGET: usize = 4000;
const PROMPT_NAME: &str = "atlas.symbol.summarize.v1";
const SYSTEM_MD: &str = "You summarize a single code symbol (function/class/method) in ONE line. You output only strict JSON — a single sentence naming what it does, no preamble.";
const TEMPLATE_MD: &str = "In ONE line, what does this {{kind}} do? Be concrete (its job + the thing it returns/affects), not generic.

{{kind}}: {{name}}   ({{path}})
signature: {{signature}}

SOURCE (may be truncated):
{{source}}

Return JSON only as exactly: {\"intent\":\"<one line, ≤240 chars>\"}";

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "intent", "field_type": "string", "required": true, "max_length": 240 },
    ])
}

// JS: `const HERE = dirname(fileURLToPath(import.meta.url)); const REPO_ROOT = resolve(HERE,
// '..', '..', '..');` — see module doc. CARGO_MANIFEST_DIR here is antiloki-core-rust; its parent
// is the same outer checkout dir the JS lands on (ai-prompt-path-grounding.rs precedent).
fn repo_root() -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    match std::path::Path::new(manifest_dir).parent() {
        Some(p) => p.to_string_lossy().into_owned(),
        None => manifest_dir.to_string(),
    }
}

// mirrors JS ensureSymbolPrompt: `existing && existing.template_md === TEMPLATE` short-circuits;
// else promptAdd re-registers (a new version, since name+template differs from the live row).
fn ensure_symbol_prompt(db: &Connection) -> Result<(), String> {
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
            "max_output_tokens": 200,
            "output_fields": prompt_output_fields(),
        }),
        db,
    );
    Ok(())
}

// `const pickIntent = (r) => r?.output_kv?.find(o => o.field_name === 'intent' &&
// o.parent_output_id == null)?.value_text ?? null` — pure, unreachable from symbolSummarize
// itself (aiCall's Err short-circuits before this ever runs) but unit-tested directly.
fn pick_intent(r: &Value) -> Value {
    let rows = match r.get("output_kv").and_then(Value::as_array) {
        Some(a) => a,
        None => return Value::Null,
    };
    for row in rows {
        let field_name = row.get("field_name").and_then(Value::as_str);
        // `o.parent_output_id == null` — JS loose equality matches BOTH null and undefined.
        let parent_is_null = row.get("parent_output_id").map_or(true, |v| v.is_null());
        if field_name == Some("intent") && parent_is_null {
            return nz(row.get("value_text")).cloned().unwrap_or(Value::Null);
        }
    }
    Value::Null
}

// `function readSpan(node)` — slices the symbol's own lines out of its file via the span
// pointer; null if no span/file/unreadable. `span?.file ?? node.path` (nullish, not truthy);
// `span?.start_line && span?.end_line` (truthy gate — 0 is falsy) selects the line-range slice,
// else the whole file text is returned (budgeted by the caller).
fn read_span(node: &Value) -> Option<String> {
    let span: Option<Value> = node
        .get("span_json")
        .and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Value>(s).ok());
    let file = span
        .as_ref()
        .and_then(|s| nz(s.get("file")))
        .cloned()
        .or_else(|| nz(node.get("path")).cloned());
    let file = match file {
        Some(f) if truthy(Some(&f)) => str_qq(Some(&f)),
        _ => return None,
    };
    let path = std::path::Path::new(&repo_root()).join(&file);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return None,
    };
    let has_range = span.as_ref().map_or(false, |s| {
        truthy(s.get("start_line")) && truthy(s.get("end_line"))
    });
    if has_range {
        let sp = span.as_ref().unwrap();
        let start = sp.get("start_line").and_then(Value::as_f64).unwrap_or(0.0) as i64 - 1;
        let end = sp.get("end_line").and_then(Value::as_f64).unwrap_or(0.0) as i64;
        let lines: Vec<&str> = text.split('\n').collect();
        let len = lines.len() as i64;
        let clamp = |i: i64| i.clamp(0, len);
        let (s, e) = (clamp(start), clamp(end));
        return Some(if s >= e { String::new() } else { lines[s as usize..e as usize].join("\n") });
    }
    Some(text)
}

pub fn symbolSummarize(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw
    // err.contextMissing('symbol-summarize', 'db');` — unreachable here, `db` is a required
    // reference in every db-lane call site (same convention as every sibling twin).
    let node_id_val = input.get("node_id").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&node_id_val)) {
        return Err("symbol-summarize: node_id is required".into());
    }
    let node_id_s = str_qq(Some(&node_id_val));
    let force = input.get("force") == Some(&Value::Bool(true)); // strict === true

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, path, type, name, signature, intent, span_json FROM code_nodes WHERE origin = 'atlas' AND type <> 'file' AND id = ? LIMIT 1",
        &[json!(node_id_s)],
    )?;
    let node = match rows.as_array().and_then(|a| a.first()).cloned() {
        Some(n) => n,
        None => return Err(format!("symbol-summarize: symbol node '{node_id_s}' not found")),
    };
    let node_id_out = node.get("id").cloned().unwrap_or(Value::Null);
    let node_intent = nz(node.get("intent")).cloned();

    if !force {
        if let Some(intent) = &node_intent {
            if truthy(Some(intent)) && !str_qq(Some(intent)).trim().is_empty() {
                return Ok(json!({ "node_id": node_id_out, "intent": intent, "cached": true, "ok": true }));
            }
        }
    }

    // `let source = input?.source; if (source == null) source = readSpan(node);` — `== null`
    // matches both JS null and undefined, i.e. "key absent or explicitly null".
    let source: Option<String> = match nz(input.get("source")) {
        Some(v) => Some(str_qq(Some(v))),
        None => read_span(&node),
    };
    let source = match source {
        Some(s) => s,
        None => {
            return Ok(json!({
                "node_id": node_id_out, "intent": node_intent.unwrap_or(Value::Null),
                "cached": false, "ok": false,
                "error": "symbol source unreadable (no span / file missing)",
            }));
        }
    };
    let budgeted = clip(&source, SOURCE_BUDGET);

    ensure_symbol_prompt(db)?;

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "kind", "value": nz(node.get("type")).cloned().unwrap_or(json!("symbol")) },
            { "key": "name", "value": nz(node.get("name")).cloned().unwrap_or(node_id_out.clone()) },
            { "key": "path", "value": nz(node.get("path")).cloned().unwrap_or(json!("")) },
            { "key": "signature", "value": nz(node.get("signature")).cloned().unwrap_or(json!("(none)")) },
            { "key": "source", "value": budgeted },
        ],
        "tier": "low", "temperature": "cold", "phase_flow": "single_only",
        "provider_fn": Value::Null, "bypass_cache": force,
    });
    // PARITY-DEFERRED boundary — the real JS wraps this in a try/catch and returns
    // `{ ok:false, error:<clipped message> }` on ANY aiCall failure. `crate::ai_ai_call::aiCall`
    // always returns `Err`, so that branch is the only one reachable here.
    let ai_result = match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(r) => r,
        Err(e) => {
            return Ok(json!({
                "node_id": node_id_out, "intent": node_intent.unwrap_or(Value::Null),
                "cached": false, "ok": false, "error": clip(&e, 200),
            }));
        }
    };

    let raw = pick_intent(&ai_result);
    let intent = if truthy(Some(&raw)) && !str_qq(Some(&raw)).trim().is_empty() {
        Some(str_qq(Some(&raw)).trim().to_string())
    } else {
        None
    };
    let intent = match intent {
        Some(i) => i,
        None => {
            return Ok(json!({
                "node_id": node_id_out, "intent": node_intent.unwrap_or(Value::Null),
                "cached": false, "ok": false, "error": "AI returned no usable intent",
            }));
        }
    };

    crate::db_json::run_write(db, "UPDATE code_nodes SET intent = ? WHERE id = ?", &[json!(intent), node_id_out.clone()])?;
    Ok(json!({ "node_id": node_id_out, "intent": intent, "cached": false, "ok": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_schema(c: &Connection) {
        c.execute_batch(
            "ATTACH ':memory:' AS brain; \
             CREATE TABLE code_nodes (id TEXT, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT, intent TEXT, span_json TEXT); \
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) ); \
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) ); \
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
    }

    #[test]
    fn missing_node_id_errs() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let e = symbolSummarize(&json!({}), &c).unwrap_err();
        assert_eq!(e, "symbol-summarize: node_id is required");
    }

    #[test]
    fn unknown_node_id_errs_not_found() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        let e = symbolSummarize(&json!({ "node_id": "nope.ts#f" }), &c).unwrap_err();
        assert_eq!(e, "symbol-summarize: symbol node 'nope.ts#f' not found");
    }

    #[test]
    fn file_type_node_is_excluded_by_type_ne_file() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        c.execute("INSERT INTO code_nodes (id,origin,path,type,name,signature,intent,span_json) VALUES ('a.ts@atlas','atlas','a.ts','file','a.ts',NULL,'purpose',NULL)", []).unwrap();
        let e = symbolSummarize(&json!({ "node_id": "a.ts@atlas" }), &c).unwrap_err();
        assert_eq!(e, "symbol-summarize: symbol node 'a.ts@atlas' not found");
    }

    #[test]
    fn cached_intent_returns_verbatim_without_touching_prompt_table() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        c.execute("INSERT INTO code_nodes (id,origin,path,type,name,signature,intent,span_json) VALUES ('a.ts#f','atlas','a.ts','function','f','()=>void','already summarized',NULL)", []).unwrap();
        let r = symbolSummarize(&json!({ "node_id": "a.ts#f" }), &c).unwrap();
        assert_eq!(r, json!({ "node_id": "a.ts#f", "intent": "already summarized", "cached": true, "ok": true }));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn force_bypasses_cache_and_reaches_the_source_read() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        c.execute("INSERT INTO code_nodes (id,origin,path,type,name,signature,intent,span_json) VALUES ('a.ts#f','atlas','does/not/exist.ts','function','f','()=>void','cached value',NULL)", []).unwrap();
        let r = symbolSummarize(&json!({ "node_id": "a.ts#f", "force": true }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["cached"], json!(false));
        assert_eq!(r["error"], json!("symbol source unreadable (no span / file missing)"));
        // still cached-value carried through as intent (node.intent ?? null).
        assert_eq!(r["intent"], json!("cached value"));
    }

    #[test]
    fn unreadable_source_short_circuits_before_ensure_prompt() {
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        c.execute("INSERT INTO code_nodes (id,origin,path,type,name,signature,intent,span_json) VALUES ('a.ts#f','atlas','does/not/exist.ts','function','f','()=>void',NULL,NULL)", []).unwrap();
        let r = symbolSummarize(&json!({ "node_id": "a.ts#f" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["error"], json!("symbol source unreadable (no span / file missing)"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.symbol.summarize.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn explicit_source_input_skips_disk_read_and_reaches_the_parity_deferred_boundary() {
        // proves guard/node-lookup/cache-skip/ensure_prompt all ran cleanly and the aiCall
        // failure is surfaced as ok:false (not an earlier bug) — no disk file needed since
        // input.source is supplied directly.
        let c = Connection::open_in_memory().unwrap();
        seed_schema(&c);
        c.execute("INSERT INTO code_nodes (id,origin,path,type,name,signature,intent,span_json) VALUES ('a.ts#f','atlas','a.ts','function','f','()=>void',NULL,NULL)", []).unwrap();
        let r = symbolSummarize(&json!({ "node_id": "a.ts#f", "source": "export function f() {}" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        let msg = r["error"].as_str().unwrap();
        assert!(msg.contains("ai-call"), "unexpected error: {msg}");
        assert_eq!(r["node_id"], json!("a.ts#f"));
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.symbol.summarize.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn read_span_slices_the_given_line_range_from_a_real_file_relative_to_repo_root() {
        // span.file is relative to REPO_ROOT (CARGO_MANIFEST_DIR's parent) — write a scratch
        // file there directly (mirrors ai-prompt-path-grounding.rs's repo_root() precedent) and
        // clean it up regardless of outcome.
        let root = repo_root();
        let rel = format!("symbol-summarize-test-scratch-{}.txt", std::process::id());
        let full = std::path::Path::new(&root).join(&rel);
        std::fs::write(&full, "line1\nline2\nline3\nline4\n").unwrap();
        let node = json!({ "path": "unused.ts", "span_json": format!("{{\"file\":\"{rel}\",\"start_line\":2,\"end_line\":3}}") });
        let got = read_span(&node);
        let _ = std::fs::remove_file(&full);
        assert_eq!(got, Some("line2\nline3".to_string()));
    }

    #[test]
    fn read_span_falls_back_to_node_path_and_whole_text_when_no_line_range() {
        let root = repo_root();
        let rel = format!("symbol-summarize-test-scratch2-{}.txt", std::process::id());
        let full = std::path::Path::new(&root).join(&rel);
        std::fs::write(&full, "whole file content").unwrap();
        let node = json!({ "path": rel, "span_json": Value::Null });
        let got = read_span(&node);
        let _ = std::fs::remove_file(&full);
        assert_eq!(got, Some("whole file content".to_string()));
    }

    #[test]
    fn read_span_returns_none_when_file_missing() {
        let node = json!({ "path": "does/not/exist-anywhere.ts", "span_json": Value::Null });
        assert_eq!(read_span(&node), None);
    }

    #[test]
    fn read_span_zero_start_line_is_falsy_so_whole_text_is_returned() {
        let root = repo_root();
        let rel = format!("symbol-summarize-test-scratch3-{}.txt", std::process::id());
        let full = std::path::Path::new(&root).join(&rel);
        std::fs::write(&full, "abc\ndef").unwrap();
        let node = json!({ "path": rel, "span_json": "{\"start_line\":0,\"end_line\":2}" });
        let got = read_span(&node);
        let _ = std::fs::remove_file(&full);
        assert_eq!(got, Some("abc\ndef".to_string()));
    }

    // ── pure helper, unreachable from symbolSummarize (aiCall's Err short-circuits first) ──
    #[test]
    fn pick_intent_finds_intent_field_only() {
        let r = json!({ "output_kv": [
            { "field_name": "other", "value_text": "nope" },
            { "field_name": "intent", "value_text": "  does X  ", "parent_output_id": null },
        ] });
        assert_eq!(pick_intent(&r), json!("  does X  "));
        assert_eq!(pick_intent(&json!({})), Value::Null);
        // parent_output_id present but non-null is excluded.
        let r2 = json!({ "output_kv": [
            { "field_name": "intent", "value_text": "nested", "parent_output_id": 3 },
        ] });
        assert_eq!(pick_intent(&r2), Value::Null);
    }
}
