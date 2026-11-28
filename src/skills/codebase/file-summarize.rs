//! file-summarize.rs — twin of skills/codebase/file-summarize.mjs (db lane + best-effort real
//! fs read). NAMES ARE CONTRACT: default `fileSummarize`. LAZY, cached AI prose on top of the
//! deterministic atlas: given a FILE node, ask a LOW-tier model for a one-line statement of the
//! file's role/responsibility and cache it in `code_nodes.intent`.
//!
//! The graph is correct with ZERO AI — this only enriches, on-demand, cached (a file node whose
//! `intent` is already non-null/non-blank returns it verbatim unless `force=true`).
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`. Unlike most
//! LLM-partial twins this session (which propagate that `Err` with `?`), the JS here wraps the
//! `aiCall` await in its own `try { } catch (e) { return {..., ok:false, error} }` — a failed/
//! absent provider degrades GRACEFULLY, it never throws. So this twin mirrors that exactly: on
//! `aiCall` failure it returns `Ok({ok:false, error: <deferred message, truncated to 200 chars>,
//! ...})`, never propagating the deferred stub's `Err` up to the caller. Everything else —
//! guard, node lookup, cache check, source resolution (real fs read, budgeted to 6000 chars),
//! `ensure_summarize_prompt` (reusing the already-converted `ai_prompt_find`/`ai_prompt_add`
//! twins) — is the real, fully ported deterministic surface.
//!
//! REPO_ROOT: JS anchors on its OWN module file's location (3 dirs up from
//! `skills/codebase/x.mjs`), not `process.cwd()` nor the atlas's `workspace-root` skill — this
//! skill reads source text of the repo it itself ships in. The Rust analogue is
//! `CARGO_MANIFEST_DIR` (this crate's own root), same self-referential intent, no `../../..`
//! needed since Cargo's manifest dir already sits at the crate root (unlike the JS file's
//! `skills/codebase/` nesting).
// QUALITY: db-read/write lane w/ best-effort real fs read (degrade-safely tested, not a real repo
// file — host-independent) + 1 bespoke/deferred aiCall dispatch, caught and folded into a
// graceful non-throwing result (matches JS contract: this function never throws once guards pass).

use rusqlite::Connection;
use serde_json::{json, Value};

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

const SOURCE_BUDGET: usize = 6000;
const PROMPT_NAME: &str = "atlas.file.summarize.v1";
const SYSTEM_MD: &str = "You summarize a source file in ONE line. You output only strict JSON — a single sentence naming the file's role/responsibility, no preamble.";
const TEMPLATE_MD: &str = "In ONE line, what is this file's role/responsibility in the codebase? Be concrete (what it does / what it owns), not generic.

FILE: {{path}}

SOURCE (may be truncated):
{{source}}

Return JSON only as exactly: {\"intent\":\"<one line, ≤240 chars>\"}";

fn prompt_output_fields() -> Value {
    json!([{ "field_name": "intent", "field_type": "string", "required": true, "max_length": 240 }])
}

/// `CARGO_MANIFEST_DIR` — this crate's own root, the Rust analogue of the JS file's
/// module-location-derived `REPO_ROOT` (see module doc).
fn repo_root() -> String {
    env!("CARGO_MANIFEST_DIR").to_string()
}

// JS: `try { existing = promptFind(...) } catch { existing = null }; if (existing &&
// existing.template_md === TEMPLATE) return;` — promptAdd only when missing or drifted.
fn ensure_summarize_prompt(db: &Connection) -> Result<(), String> {
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db) {
        let matches = existing.get("template_md").and_then(Value::as_str) == Some(TEMPLATE_MD);
        if matches {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
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
    )?;
    Ok(())
}

// `r?.output_kv?.find(o => o.field_name === 'intent' && o.parent_output_id == null)?.value_text`
fn pick_intent(r: &Value) -> Option<String> {
    r.get("output_kv")?.as_array()?.iter().find_map(|o| {
        let is_intent = o.get("field_name").and_then(Value::as_str) == Some("intent");
        let no_parent = o.get("parent_output_id").map_or(true, |v| v.is_null());
        if is_intent && no_parent {
            o.get("value_text").and_then(Value::as_str).map(|s| s.to_string())
        } else {
            None
        }
    })
}

pub fn fileSummarize(input: &Value, db: &Connection) -> Result<Value, String> {
    let raw_path = input.get("path").cloned().unwrap_or(Value::Null);
    if !truthy(Some(&raw_path)) {
        return Err("file-summarize: path is required".into());
    }
    // Portable graph: ids/paths are stored POSIX-normalized (matches atlas-build).
    let path = str_qq(Some(&raw_path)).replace('\\', "/");
    let force = matches!(input.get("force"), Some(Value::Bool(true)));

    let rows = crate::db_json::query_rows(
        db,
        "SELECT id, intent FROM code_nodes WHERE origin = 'atlas' AND type = 'file' AND (id = ? OR path = ?) LIMIT 1",
        &[json!(path), json!(path)],
    )?;
    let node = match rows.as_array().and_then(|a| a.first()) {
        Some(n) => n.clone(),
        None => return Err(format!("file-summarize: file node '{path}' not found")),
    };
    let node_id = node.get("id").cloned().unwrap_or(Value::Null);
    let node_intent = node.get("intent").and_then(Value::as_str).map(|s| s.to_string());

    // CACHE: a non-blank cached intent returns verbatim, no AI call.
    if !force {
        if let Some(cached) = &node_intent {
            if !cached.trim().is_empty() {
                return Ok(json!({ "path": path, "intent": cached, "cached": true, "ok": true }));
            }
        }
    }

    // Resolve source: explicit input wins (testability); else real fs read under REPO_ROOT.
    // A read failure degrades gracefully — no AI, return a status (mirrors JS's try/catch).
    let source: String = match input.get("source").and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => {
            let full = std::path::Path::new(&repo_root()).join(&path);
            match std::fs::read_to_string(&full) {
                Ok(s) => s,
                Err(e) => {
                    let msg: String = format!("source unreadable: {e}").chars().take(200).collect();
                    return Ok(json!({
                        "path": path,
                        "intent": node_intent.map(Value::String).unwrap_or(Value::Null),
                        "cached": false, "ok": false, "error": msg,
                    }));
                }
            }
        }
    };
    let budgeted: String = source.chars().take(SOURCE_BUDGET).collect();

    ensure_summarize_prompt(db)?;

    // The AI is a CACHE on the deterministic graph: degrade gracefully on any failure (here,
    // ALWAYS — aiCall is the deferred stub) — return a status, never propagate the error.
    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "path", "value": path },
            { "key": "source", "value": budgeted },
        ],
        "tier": "low", "temperature": "cold", "phase_flow": "single_only",
        "provider_fn": Value::Null, "bypass_cache": force,
    });
    let ai_result = match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(r) => r,
        Err(e) => {
            let msg: String = e.chars().take(200).collect();
            return Ok(json!({
                "path": path,
                "intent": node_intent.map(Value::String).unwrap_or(Value::Null),
                "cached": false, "ok": false, "error": msg,
            }));
        }
    };
    let intent = pick_intent(&ai_result).filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_string());
    let intent = match intent {
        Some(i) => i,
        None => {
            return Ok(json!({
                "path": path,
                "intent": node_intent.map(Value::String).unwrap_or(Value::Null),
                "cached": false, "ok": false, "error": "AI returned no usable intent",
            }));
        }
    };

    db.execute("UPDATE code_nodes SET intent = ? WHERE id = ?", rusqlite::params![intent, node_id.as_str().unwrap_or_default()])
        .map_err(|e| e.to_string())?;
    Ok(json!({ "path": path, "intent": intent, "cached": false, "ok": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE code_nodes (id TEXT PRIMARY KEY, origin TEXT NOT NULL, path TEXT, type TEXT NOT NULL, intent TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn path_is_required() {
        let c = setup();
        let e = fileSummarize(&json!({}), &c).unwrap_err();
        assert_eq!(e, "file-summarize: path is required");
        let e2 = fileSummarize(&json!({ "path": "" }), &c).unwrap_err();
        assert_eq!(e2, "file-summarize: path is required");
    }

    #[test]
    fn unknown_file_node_errors() {
        let c = setup();
        let e = fileSummarize(&json!({ "path": "skills/foo.mjs" }), &c).unwrap_err();
        assert_eq!(e, "file-summarize: file node 'skills/foo.mjs' not found");
    }

    #[test]
    fn cached_intent_returns_verbatim_no_ai() {
        let c = setup();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, intent) VALUES ('skills/foo.mjs', 'atlas', 'skills/foo.mjs', 'file', 'does the foo thing')", []).unwrap();
        let out = fileSummarize(&json!({ "path": "skills/foo.mjs" }), &c).unwrap();
        assert_eq!(out, json!({ "path": "skills/foo.mjs", "intent": "does the foo thing", "cached": true, "ok": true }));
        // no prompt was ensured — no AI path taken at all.
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn blank_intent_is_not_treated_as_cached() {
        let c = setup();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, intent) VALUES ('skills/foo.mjs', 'atlas', 'skills/foo.mjs', 'file', '   ')", []).unwrap();
        let out = fileSummarize(&json!({ "path": "skills/foo.mjs", "source": "x" }), &c).unwrap();
        assert_eq!(out["cached"], json!(false));
        assert_eq!(out["ok"], json!(false));
        // deferred aiCall folded gracefully — never propagated as Err.
        assert!(out["error"].as_str().unwrap().len() > 0);
    }

    #[test]
    fn force_true_bypasses_cache_and_reaches_deferred_aicall_boundary() {
        let c = setup();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, intent) VALUES ('skills/foo.mjs', 'atlas', 'skills/foo.mjs', 'file', 'stale cached intent')", []).unwrap();
        let out = fileSummarize(&json!({ "path": "skills/foo.mjs", "source": "export default function foo() {}", "force": true }), &c).unwrap();
        assert_eq!(out["cached"], json!(false));
        assert_eq!(out["ok"], json!(false));
        // stale cached intent is preserved as the fallback value on degrade.
        assert_eq!(out["intent"], json!("stale cached intent"));
        // ensure_summarize_prompt DID run (deterministic surface reached the AI boundary).
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.file.summarize.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn missing_source_and_unreadable_disk_path_degrades_gracefully() {
        let c = setup();
        c.execute("INSERT INTO code_nodes (id, origin, path, type, intent) VALUES ('does/not/exist.mjs', 'atlas', 'does/not/exist.mjs', 'file', NULL)", []).unwrap();
        let out = fileSummarize(&json!({ "path": "does/not/exist.mjs" }), &c).unwrap();
        assert_eq!(out["cached"], json!(false));
        assert_eq!(out["ok"], json!(false));
        assert_eq!(out["intent"], Value::Null);
        assert!(out["error"].as_str().unwrap().starts_with("source unreadable:"));
    }

    #[test]
    fn pick_intent_matches_top_level_field_only() {
        let ai_result = json!({ "output_kv": [
            { "field_name": "intent", "value_text": "child value", "parent_output_id": 7 },
            { "field_name": "intent", "value_text": "top level intent", "parent_output_id": null },
        ] });
        assert_eq!(pick_intent(&ai_result), Some("top level intent".to_string()));
        assert_eq!(pick_intent(&json!({})), None);
    }

    #[test]
    fn ensure_summarize_prompt_is_idempotent_when_template_unchanged() {
        let c = setup();
        ensure_summarize_prompt(&c).unwrap();
        ensure_summarize_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.file.summarize.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
