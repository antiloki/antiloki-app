//! atlas-review.rs — twin of skills/codebase/atlas-review.mjs (db lane, LLM-partial). NAMES ARE
//! CONTRACT: default `atlasReview`. LAZY, scoped, ADVISORY architecture review (EPIC:
//! codebase-atlas, Wave F) — non-gating (QA is the single gate). Given some files (or, by
//! default, the riskiest files `atlas-metrics` surfaces), gathers real structure
//! (`codebase-context`) + risk signals (`atlas-metrics`) and asks a MID-tier model for a short
//! review. Returns prose; persists nothing.
//!
//! `context.db` missing → JS throws `err.contextMissing('atlas-review', 'db')`. Unreachable
//! here: `db: &Connection` is a required, non-nullable reference (same convention as every
//! sibling db-lane twin). `context.provider_fn` (a JS function reference for provider
//! selection/DI) has no Rust equivalent to carry — threaded as `null` into the constructed
//! aiCall input, same as book-run.rs's/pl-research.rs's `provider_fn`.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`, so `atlasReview`
//! returns the `{review_md:null, ..., ok:false, error}` shape once the (real, ported)
//! deterministic surface above it has run: the JS's own `catch` block around the aiCall await is
//! ported as the `Err` arm below, truncating the message to 200 chars exactly as the JS
//! `.slice(0,200)` does. `ensure_review_prompt` reuses the already-converted `ai_prompt_find`/
//! `ai_prompt_add` twins (mirroring book-run.rs's `ensure_faculty` / pl-research.rs's
//! `ensure_prompt`), including the JS's specific short-circuit: it re-adds the prompt ONLY when
//! no existing row is found OR the existing row's `template_md` differs from the current
//! constant — it does NOT re-`promptFind` after `promptAdd` (unlike `ensure_prompt`'s verify
//! step), ported exactly as written. `pick_review` (the `output_kv` → `review_md` extractor) is
//! ported as its own PURE function and unit-tested directly with a canned `res` value —
//! unreachable from `atlasReview` itself (the `?` on `aiCall` short-circuits first) but correct
//! by construction, per the crossing playbook's coverage rule.
//!
//! `codebaseContext`/`atlasMetrics` reused via the already-converted
//! `codebase_codebase_context::codebaseContext` / `codebase_atlas_metrics::atlasMetrics` twins.
// QUALITY: mixed unit — deterministic assembly/db-read surface (fully ported, fixture-reachable
// up to the deferred aiCall boundary) + 1 pure response-shaping helper (unreachable from the
// default export, unit-tested directly) + 1 bespoke/deferred aiCall dispatch (no fixture/frontier).

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

const PROMPT: &str = "atlas.review.v1";
const SYSTEM: &str = "You are a senior engineer doing an ADVISORY architecture review (non-blocking — you are NOT a gate). Given the real structure of some files + risk signals, you note strengths, risks, and concrete suggestions. You output only strict JSON. Ground every point in the evidence; invent nothing.";
const TEMPLATE: &str = "Do a SHORT advisory review (markdown) of the code below: strengths, risks (tight coupling, missing tests, god-files, fan-in hotspots), and concrete suggestions. Advisory only. Ground every point in the evidence; do not invent files that are not listed.

=== STRUCTURE (the real code) ===
{{context_md}}

=== RISK SIGNALS (atlas-metrics) ===
{{risk_md}}

Return JSON only as exactly: {\"review_md\":\"<short advisory review in markdown>\"}";

fn prompt_output_fields() -> Value {
    json!([{ "field_name": "review_md", "field_type": "string", "required": true, "min_length": 40 }])
}

// mirrors JS ensureReviewPrompt EXACTLY: try promptFind; skip the write only when an existing
// row's template_md matches the current TEMPLATE constant verbatim. No re-find after promptAdd.
fn ensure_review_prompt(db: &Connection) -> Result<(), String> {
    let existing = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db).ok();
    if let Some(e) = &existing {
        if e.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) {
            return Ok(());
        }
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "mid",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 1200,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    Ok(())
}

// Pure — `r?.output_kv?.find(o => o.field_name === 'review_md' && o.parent_output_id == null)
// ?.value_text ?? null`. `== null` catches both undefined (absent key) and JSON null.
fn pick_review(res: &Value) -> Option<String> {
    let rows = res.get("output_kv")?.as_array()?;
    rows.iter()
        .find(|o| {
            o.get("field_name").and_then(Value::as_str) == Some("review_md")
                && o.get("parent_output_id").map_or(true, |v| v.is_null())
        })
        .and_then(|o| o.get("value_text"))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

// `entries.slice(0,5).map(e => \`${e.path} (fan_in ${e.fan_in})\`).join(', ') || '—'`
fn risk_line(entries: &[Value]) -> String {
    let parts: Vec<String> = entries
        .iter()
        .take(5)
        .map(|e| format!("{} (fan_in {})", str_qq(e.get("path")), str_qq(e.get("fan_in"))))
        .collect();
    if parts.is_empty() { "—".to_string() } else { parts.join(", ") }
}

pub fn atlasReview(input: &Value, db: &Connection) -> Result<Value, String> {
    let provider_fn = Value::Null;
    let origin = if input.get("origin").is_some() { str_qq(input.get("origin")) } else { "atlas".to_string() };
    let origin = if origin.is_empty() { "atlas".to_string() } else { origin };

    let metrics = crate::codebase_atlas_metrics::atlasMetrics(&json!({ "origin": origin, "top": 10 }), db)?;
    let empty_vec: Vec<Value> = Vec::new();
    let top_risky = metrics.get("top_risky").and_then(Value::as_array).unwrap_or(&empty_vec);
    let top_central = metrics.get("top_central").and_then(Value::as_array).unwrap_or(&empty_vec);

    let mut paths: Vec<String> = match input.get("paths").and_then(Value::as_array) {
        Some(arr) => arr
            .iter()
            .map(|p| str_qq(Some(p)).replace('\\', "/"))
            .filter(|p| !p.is_empty())
            .collect(),
        None => Vec::new(),
    };
    if paths.is_empty() {
        paths = top_risky.iter().take(8).filter_map(|e| e.get("path").and_then(Value::as_str)).map(String::from).collect();
    }
    if paths.is_empty() {
        paths = top_central.iter().take(8).filter_map(|e| e.get("path").and_then(Value::as_str)).map(String::from).collect();
    }
    if paths.is_empty() {
        return Ok(json!({
            "review_md": Value::Null,
            "files_reviewed": 0,
            "paths": [],
            "ok": false,
            "error": format!("no files to review for origin '{origin}'"),
        }));
    }

    let cc = crate::codebase_codebase_context::codebaseContext(
        &json!({ "paths": paths, "hops": 1, "origin": origin }),
        db,
    )?;
    let context_md = if cc.get("grounded").map_or(false, |v| v.as_bool().unwrap_or(false)) {
        str_qq(cc.get("context_md"))
    } else {
        "(atlas has no structure for these files)".to_string()
    };
    let risk_md = format!(
        "Most depended-on: {}\nRisky (depended-on but untested): {}",
        risk_line(top_central),
        risk_line(top_risky),
    );

    ensure_review_prompt(db)?;

    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "context_md", "value": context_md },
            { "key": "risk_md", "value": risk_md },
        ],
        "tier": "mid",
        "temperature": "cold",
        "phase_flow": "single_only",
        "provider_fn": provider_fn,
    });
    // ── the AI call: PARITY-DEFERRED from here (see module doc) — aiCall always returns Err.
    let ai_result = match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(res) => res,
        Err(e) => {
            let msg: String = e.chars().take(200).collect();
            return Ok(json!({
                "review_md": Value::Null,
                "files_reviewed": paths.len() as i64,
                "paths": paths,
                "ok": false,
                "error": msg,
            }));
        }
    };
    let review = pick_review(&ai_result).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    match review {
        None => Ok(json!({
            "review_md": Value::Null,
            "files_reviewed": paths.len() as i64,
            "paths": paths,
            "ok": false,
            "error": "AI returned no usable review",
        })),
        Some(review_md) => Ok(json!({
            "review_md": review_md,
            "files_reviewed": paths.len() as i64,
            "paths": paths,
            "ok": true,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pick_review_finds_top_level_field() {
        let res = json!({ "output_kv": [
            { "field_name": "other", "value_text": "x", "parent_output_id": null },
            { "field_name": "review_md", "value_text": "**good**", "parent_output_id": null },
        ] });
        assert_eq!(pick_review(&res), Some("**good**".to_string()));
    }

    #[test]
    fn pick_review_ignores_nested_output() {
        let res = json!({ "output_kv": [
            { "field_name": "review_md", "value_text": "nested", "parent_output_id": 3 },
        ] });
        assert_eq!(pick_review(&res), None);
    }

    #[test]
    fn pick_review_missing_output_kv_is_none() {
        assert_eq!(pick_review(&json!({})), None);
    }

    #[test]
    fn risk_line_empty_is_emdash() {
        assert_eq!(risk_line(&[]), "—");
    }

    #[test]
    fn risk_line_formats_and_caps_at_five() {
        let entries: Vec<Value> = (0..7)
            .map(|i| json!({ "path": format!("f{i}.mjs"), "fan_in": i }))
            .collect();
        let out = risk_line(&entries);
        assert_eq!(out, "f0.mjs (fan_in 0), f1.mjs (fan_in 1), f2.mjs (fan_in 2), f3.mjs (fan_in 3), f4.mjs (fan_in 4)");
    }

    #[test]
    fn atlas_review_no_paths_returns_ok_false() {
        let conn = Connection::open_in_memory().unwrap();
        // atlasMetrics (called internally) reads code_nodes/code_edges, not "files"/"edges" —
        // matches the real atlas schema used by every other codebase-atlas twin's fixtures.
        conn.execute_batch(
            "CREATE TABLE code_nodes (
               id TEXT PRIMARY KEY, origin TEXT, path TEXT, type TEXT, name TEXT, signature TEXT,
               intent TEXT, parent_id TEXT, span_json TEXT, content_hash TEXT, version INTEGER, exported INTEGER
             );
             CREATE TABLE code_edges (
               id INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT, from_node TEXT, to_node TEXT,
               type TEXT, output TEXT, input TEXT
             , kind TEXT);",
        )
        .unwrap();
        let out = atlasReview(&json!({}), &conn).unwrap();
        assert_eq!(out["ok"], json!(false));
        assert_eq!(out["files_reviewed"], json!(0));
        assert!(out["error"].as_str().unwrap().contains("no files to review"));
    }
}
