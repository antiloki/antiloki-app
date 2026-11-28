//! atlas-explain-snippet.rs — twin of skills/codebase/atlas-explain-snippet.mjs (db, low-tier
//! advisory). NAMES ARE CONTRACT: default `atlasExplainSnippet`. Requests a SHORT (1-3 sentence)
//! AI explanation of an arbitrary code SNIPPET the caller passes in directly (no node lookup, no
//! persistence) — degrades gracefully to `{ok:false, error}` on any AI failure.
//!
//! PARITY-DEFERRED: the single `crate::ai_ai_call::aiCall` dispatch (see ai-call.rs's module doc —
//! always returns `Err`). Ported faithfully around it: the snippet/where guard, the idempotent
//! `ensure_prompt` bootstrap (reusing the already-converted `ai_prompt_find`/`ai_prompt_add`
//! twins), and the `pick` response-extraction helper. JS catches aiCall's rejection and returns a
//! normal `{ok:false, error}` value rather than propagating — this twin mirrors that: `ensure_prompt`
//! errors DO propagate (unguarded in the JS source), but the aiCall `Err` is caught and folded into
//! the `Ok({ok:false, ...})` return, exactly like the JS try/catch around `await aiCall(...)`.
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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(x) => str_qq(Some(x)),
    }
} // = JS `${v}`

const PROMPT: &str = "atlas.snippet.explain.v1";
const SYSTEM: &str = "You explain a short code snippet for a developer, concisely. You output only strict JSON. Be concrete about what these lines do and any non-obvious behavior. No preamble.";
const TEMPLATE: &str = "Explain what these lines do, concisely (1-3 sentences), for a developer. Ground it strictly in the snippet; do not invent surrounding context.\n\n{{where}}\n```\n{{snippet}}\n```\n\nReturn JSON only as exactly: {\"explanation\":\"<1-3 sentences>\"}";

fn ensure_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(|v| v.as_str()) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "low",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 320,
            "output_fields": [{ "field_name": "explanation", "field_type": "string", "required": true, "min_length": 10 }],
        }),
        db,
    )?;
    Ok(())
}

// `res?.output_kv?.find(o => o.field_name === 'explanation' && o.parent_output_id == null)?.value_text ?? null`
fn pick(res: &Value) -> Value {
    res.get("output_kv")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find(|o| {
                o.get("field_name").and_then(|v| v.as_str()) == Some("explanation")
                    && matches!(o.get("parent_output_id"), None | Some(Value::Null))
            })
        })
        .and_then(|o| o.get("value_text").cloned())
        .unwrap_or(Value::Null)
}

pub fn atlasExplainSnippet(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent).
    let snippet_val = input.get("snippet");
    if !truthy(snippet_val) || str_qq(snippet_val).trim().is_empty() {
        return Err("atlas-explain-snippet: snippet is required".to_string());
    }
    let path = input.get("path");
    let where_str = if truthy(path) {
        let mut s = format!("From {}", tmpl(path));
        if truthy(input.get("lines")) {
            s.push_str(&format!(" (lines {})", tmpl(input.get("lines"))));
        }
        s.push(':');
        s
    } else {
        String::new()
    };

    ensure_prompt(db)?;

    let snippet_trunc: String = str_qq(snippet_val).chars().take(6000).collect();
    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "where", "value": where_str },
            { "key": "snippet", "value": snippet_trunc },
        ],
        "tier": "low", "temperature": "cold", "phase_flow": "single_only", "provider_fn": Value::Null,
    });

    let mut explanation: Option<String> = None;
    match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(res) => {
            let raw = pick(&res);
            if truthy(Some(&raw)) {
                let s = str_qq(Some(&raw)).trim().to_string();
                if !s.is_empty() {
                    explanation = Some(s);
                }
            }
        }
        Err(e) => {
            let msg: String = e.chars().take(160).collect();
            return Ok(json!({ "explanation": Value::Null, "ok": false, "error": msg }));
        }
    }
    match explanation {
        Some(explanation) => Ok(json!({ "explanation": explanation, "ok": true })),
        None => Ok(json!({ "explanation": Value::Null, "ok": false, "error": "AI returned no explanation" })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('now')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        )
        .unwrap();
        c
    }

    #[test]
    fn missing_or_blank_snippet_errors() {
        let c = setup();
        assert!(atlasExplainSnippet(&json!({}), &c).unwrap_err().contains("snippet is required"));
        assert!(atlasExplainSnippet(&json!({ "snippet": "   " }), &c).unwrap_err().contains("snippet is required"));
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_and_degrades_gracefully() {
        // proves the deterministic surface (guard, prompt-ensure, snippet truncation) ran cleanly
        // and the result is a graceful {ok:false} fold of aiCall's deferred stub, not a thrown error.
        let c = setup();
        let r = atlasExplainSnippet(&json!({ "snippet": "const x = 1;", "path": "foo.js", "lines": "3-5" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
        assert_eq!(r["explanation"], Value::Null);
        // JS truncates via `.slice(0, 160)`, which counts UTF-16 code units (JS `.length`), not
        // UTF-8 bytes. The em dash in the deferred-aicall message is 1 UTF-16 unit / 1 Rust `char`
        // but 3 UTF-8 bytes, so `.len()` (byte length) legitimately exceeds 160 here — verified via
        // `node -e` against the real JS: `s.slice(0,160).length === 160` while its UTF-8 byte length
        // is 162. The Rust twin mirrors this with `.chars().take(160)`, so assert on char count.
        assert!(r["error"].as_str().unwrap().chars().count() <= 160);
        // the prompt template WAS ensured (ensure_prompt ran) before the deferred call
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.snippet.explain.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn ensure_prompt_is_idempotent_across_calls() {
        let c = setup();
        atlasExplainSnippet(&json!({ "snippet": "a" }), &c).unwrap();
        atlasExplainSnippet(&json!({ "snippet": "b" }), &c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'atlas.snippet.explain.v1'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn where_clause_omits_lines_when_absent() {
        let c = setup();
        // no path → where_str is "" (exercised indirectly; direct assertion would need a non-deferred
        // aiCall, so this just confirms the guard/ensure path runs without a path/lines input).
        let r = atlasExplainSnippet(&json!({ "snippet": "x" }), &c).unwrap();
        assert_eq!(r["ok"], json!(false));
    }
}
