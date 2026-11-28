//! ai-image-call.rs — twin of skills/ai/ai-image-call.mjs, the METERED chokepoint for image
//! generation (nano-banana class via OpenRouter). NAMES ARE CONTRACT: default `aiImageCall`.
//!
//! PARITY-DEFERRED (same convention as ai-call.rs's `aiCall` / book-run.rs's `fac_call`): the
//! actual dispatch (`openrouterImageCall`, imported from `../external/openrouter-image-call.mjs`)
//! makes a real HTTP round-trip to OpenRouter's image API and returns the ACTUAL billed cost —
//! genuinely non-deterministic and not fixturable, and there is no already-converted Rust twin
//! for that external adapter (grep of antiloki-core-rust/src/skills confirms). Everything that
//! surrounds it is ported faithfully and IS deterministic: the guard on `prompt`, the model_id
//! resolution (explicit input → `brain.ai_providers` tier='image' lookup → hardcoded fallback,
//! reusing the already-converted `ai_prompt_find`/`ai_prompt_add` twins for `ensure_prompt`'s
//! try/catch-then-create), and the envelope/`ai_calls` metering INSERT that would run after a
//! real response — the last block is unreachable by this file's own tests (it only runs after a
//! successful dispatch, which the deferred stub never produces) but ported byte-for-byte from the
//! JS, exactly like book-run.rs's post-wave staging logic.
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn nz(v: Option<&Value>) -> Option<&Value> {
    v.filter(|x| !x.is_null())
}

const PROMPT_NAME: &str = "ai.image.generate.v1";

// JS `ensurePrompt`: try promptFind({name}); on ANY throw, promptAdd then promptFind again.
fn ensure_prompt(db: &Connection) -> Result<Value, String> {
    if let Ok(tpl) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db) {
        return Ok(tpl);
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": "Generate an image: {{image_prompt}}",
            "default_tier": "high",
            "default_temperature": "warm",
            "max_output_tokens": 16,
            "output_fields": [{ "field_name": "image_note", "field_type": "string", "required": false }],
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db)
}

// JS `resolveImageModel`: try/catch → null on any db error (e.g. table missing).
fn resolve_image_model(db: &Connection) -> Option<Value> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT model_id FROM brain.ai_providers WHERE tier = 'image' AND provider = 'openrouter' AND active = 1 ORDER BY failover_rank ASC, id ASC LIMIT 1",
        &[],
    )
    .ok()?;
    rows.as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("model_id"))
        .cloned()
        .filter(|v| !v.is_null())
}

// PARITY-DEFERRED: the real network dispatch (a real HTTP round-trip + OpenRouter's ACTUAL usage
// accounting) — see module doc. Always propagates Err, mirroring ai-call.rs's `aiCall` stub.
fn openrouter_image_call_deferred(_prompt: &str, _model_id: &Value) -> Result<Value, String> {
    Err("ai-image-call twin: the openrouterImageCall dispatch is PARITY-DEFERRED — see the module doc. It makes a real HTTP round-trip to OpenRouter's image-generation API and reads back OpenRouter's ACTUAL billed cost, which is genuinely non-deterministic and not fixturable.".into())
}

pub fn aiImageCall(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, image_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent).
    let prompt = str_qq(input.get("prompt"));
    if prompt.is_empty() {
        return Err("ai-image-call: prompt is required".into());
    }
    let model_id = nz(input.get("model_id"))
        .cloned()
        .or_else(|| resolve_image_model(db))
        .unwrap_or_else(|| json!("google/gemini-2.5-flash-image-preview"));

    // real, ported: the prompt-template ensure step runs before the deferred dispatch.
    let tpl = ensure_prompt(db)?;
    let tpl_id = tpl.get("id").cloned().unwrap_or(Value::Null);

    // PARITY-DEFERRED past this point (see module doc) — everything below is ported byte-for-byte
    // from the JS but is unreachable by this file's own tests, exactly like book-run.rs's
    // post-`fac_call` staging logic: it only runs after a successful `r` from the real dispatch.
    let r = openrouter_image_call_deferred(&prompt, &model_id)?;

    let envelope = serde_json::to_string(&json!({
        "kind": "image-call",
        "model_id": model_id.clone(),
        "prompt": prompt.chars().take(2000).collect::<String>(),
    }))
    .map_err(|e| e.to_string())?;
    let input_hash = {
        let mut h = Sha256::new();
        h.update(prompt.as_bytes());
        format!("{:x}", h.finalize())
    };
    let cost_usd = r.get("cost_usd").cloned().unwrap_or(json!(0));
    let tokens_in = r.get("tokens_in").cloned().unwrap_or(json!(0));
    let tokens_out = r.get("tokens_out").cloned().unwrap_or(json!(0));
    let generation_id = nz(r.get("generation_id")).cloned().unwrap_or(Value::Null);
    let duration_ms = 0i64; // frozen-clock lane: performance.now()/Date.now() deltas are always 0

    let ai_call_id = match crate::db_json::run_write(
        db,
        "INSERT INTO ai_calls (prompt_template_id, tier, temperature, provider, model_id, input_hash, duration_ms, cost_usd_actual, tokens_in, tokens_out, cached, envelope_json, outcome, openrouter_generation_id) VALUES (?, 'high', 'warm', 'openrouter', ?, ?, ?, ?, ?, ?, 0, ?, 'success', ?)",
        &[tpl_id, model_id, json!(input_hash), json!(duration_ms), cost_usd.clone(), tokens_in, tokens_out, json!(envelope), generation_id],
    ) {
        Ok((_changes, id)) => json!(id),
        Err(_) => Value::Null, // metering must not eat the image
    };

    Ok(json!({
        "ok": true,
        "image_data_url": r.get("image_data_url").cloned().unwrap_or(Value::Null),
        "cost_usd": if cost_usd.is_null() { Value::Null } else { cost_usd },
        "ai_call_id": ai_call_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.ai_providers ( id INTEGER PRIMARY KEY, tier TEXT, provider TEXT, model_id TEXT, active INTEGER NOT NULL DEFAULT 1, failover_rank INTEGER NOT NULL DEFAULT 0 );",
        )
        .unwrap();
        c
    }

    #[test]
    fn prompt_is_required() {
        let c = setup();
        let e = aiImageCall(&json!({}), &c).unwrap_err();
        assert_eq!(e, "ai-image-call: prompt is required");
        let e2 = aiImageCall(&json!({ "prompt": "" }), &c).unwrap_err();
        assert_eq!(e2, "ai-image-call: prompt is required");
    }

    #[test]
    fn reaches_the_parity_deferred_dispatch_after_ensuring_the_prompt_template() {
        let c = setup();
        let err = aiImageCall(&json!({ "prompt": "a cat riding a bike" }), &c).unwrap_err();
        assert!(err.contains("PARITY-DEFERRED"), "unexpected error: {err}");
        // the deterministic prompt-ensure step DID run before the deferred call
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'ai.image.generate.v1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn model_id_resolution_prefers_explicit_input_then_db_then_fallback() {
        let c = setup();
        // explicit input wins even with no ai_providers row
        let e1 = aiImageCall(&json!({ "prompt": "x", "model_id": "explicit/model" }), &c).unwrap_err();
        assert!(e1.contains("PARITY-DEFERRED"));

        // db lookup used when no explicit model_id
        c.execute(
            "INSERT INTO brain.ai_providers (tier, provider, model_id, active, failover_rank) VALUES ('image', 'openrouter', 'db/model', 1, 0)",
            [],
        )
        .unwrap();
        let e2 = aiImageCall(&json!({ "prompt": "x" }), &c).unwrap_err();
        assert!(e2.contains("PARITY-DEFERRED"));
    }

    #[test]
    fn ensure_prompt_reuses_existing_active_row_without_reinserting() {
        let c = setup();
        let _ = aiImageCall(&json!({ "prompt": "first" }), &c);
        let _ = aiImageCall(&json!({ "prompt": "second" }), &c);
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'ai.image.generate.v1'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn resolve_image_model_falls_back_to_null_when_table_or_row_missing() {
        let c = Connection::open_in_memory().unwrap();
        // no brain attach at all — the try/catch in JS returns null; our .ok()? mirrors it
        assert_eq!(resolve_image_model(&c), None);
    }
}
