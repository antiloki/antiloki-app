//! epic-evaluate.rs — twin of skills/entities/epic-evaluate.mjs (db lane). NAMES ARE CONTRACT:
//! default `epicEvaluate`. AI evaluation of an EPIC (parent card + children), expanding on
//! card-evaluate's recommendation set with SPLIT/CONSOLIDATE. Appends to `body.ai_evaluations[]`.
//!
//! PARITY-DEFERRED: the AI call is dispatched through `crate::ai_ai_call::aiCall`, the deferred
//! network-AI stub (see ai-call.rs's module doc) — it always returns `Err`. JS calls `await
//! aiCall(...)` with NO surrounding try/catch, so the twin PROPAGATES the error via `?` — same
//! convention as pl-research.rs/book-run.rs. Everything above that boundary is ported for real:
//! the `epic_slug` guard, `load_epic_context` (EPIC row lookup + `EPIC-` prefix validation +
//! body_json best-effort parse + children/peers queries), the cache-by-`updated_at` short-circuit
//! against `body.ai_evaluations`, and `ensure_prompt` (reusing the already-converted
//! `ai_prompt_find`/`ai_prompt_add` twins, mirroring pl-research.rs's `ensure_prompt`). The
//! response-parse helper (`get_field`) and the evaluation/body shaping below the `aiCall` line are
//! ported as their own PURE functions and unit-tested directly with canned values — unreachable
//! from `epicEvaluate` itself (the `?` on `aiCall` short-circuits before they run) but correct by
//! construction, same pattern as pl-research.rs's `build_research_report`/`build_sources`.
// QUALITY: mixed unit — deterministic guard/load/cache/ensure-prompt surface (fully ported,
// fixture-reachable up to the deferred aiCall boundary) + 1 pure response-shaping helper
// (unreachable from the default export, unit-tested directly) + 1 bespoke/deferred aiCall
// dispatch (no fixture/frontier). withAudit's final card-update write is likewise unreachable
// from the default export (same reason) — not re-ported here since with-audit.rs's dispatch_skill
// already covers card-update end-to-end.
use rusqlite::Connection;
use serde_json::{json, Map, Value};

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

const PROMPT_NAME: &str = "board-keeper.epic.evaluate.v1";
const PROMPT_TEMPLATE_MD: &str = "You are auditing a single EPIC (parent card with children) to decide whether it should still be on the board, be split, be consolidated, or be archived.

EPIC slug:    {{epic_slug}}
EPIC title:   {{epic_title}}
EPIC status:  {{epic_status}}
EPIC age:     created {{epic_created_at}}, updated {{epic_updated_at}}

EPIC body (truncated to 1500 chars):
{{epic_body}}

CHILDREN (slug \u{b7} status, total {{total_children}}, {{done_children}} done):
{{children}}

PEER EPICS (other EPICs at same level for SPLIT/CONSOLIDATE judgement):
{{peer_epics}}

Decide which recommendation fits this EPIC today:

  KEEP        \u{2014} children look right, scope is correct, continue as-is
  MODIFY      \u{2014} keep but rebody (clarify scope, update acceptance criteria)
  SPLIT       \u{2014} too large or mixing concerns; should be split into 2+ smaller EPICs
  CONSOLIDATE \u{2014} overlaps significantly with another EPIC; merge them
  DEFER       \u{2014} right idea, wrong time; park for later
  ARCHIVE     \u{2014} superseded, obsolete, or work happened elsewhere

Output JSON of this exact shape:
{ \"recommendation\": \"<KEEP|MODIFY|SPLIT|CONSOLIDATE|DEFER|ARCHIVE>\", \"reasoning\": \"<one short paragraph, max 350 chars>\", \"suggested_modifications\": \"<for MODIFY/SPLIT/CONSOLIDATE: concrete suggestion; otherwise empty>\" }";

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "recommendation", "field_type": "string", "required": true },
        { "field_name": "reasoning", "field_type": "string", "required": true },
        { "field_name": "suggested_modifications", "field_type": "string", "required": false },
    ])
}

const VALID_RECS: [&str; 6] = ["KEEP", "MODIFY", "SPLIT", "CONSOLIDATE", "DEFER", "ARCHIVE"];

// try promptFind; on miss, promptAdd then promptFind again (mirrors JS ensurePrompt's try/catch).
fn ensure_prompt(db: &Connection) -> Result<(), String> {
    if crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db).is_ok() {
        return Ok(());
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "mid",
            "default_temperature": "cold",
            "max_output_tokens": 800,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db)?;
    Ok(())
}

struct EpicContext {
    epic: Value, // {slug, title, status, body_json, created_at, updated_at}
    body: String,
    children: Vec<Value>, // {slug, status}
    done_count: usize,
    peers: Vec<Value>, // {slug, title}
}

// JS: `try { const b = JSON.parse(epic.body_json ?? '""'); return typeof b === 'string' ? b :
// JSON.stringify(b, null, 2); } catch { return epic.body_json ?? ''; }`
fn body_display(body_json: Option<&Value>) -> String {
    let raw = str_qq(body_json);
    let text = if raw.is_empty() { "\"\"".to_string() } else { raw.clone() };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::String(s)) => s,
        Ok(other) => serde_json::to_string_pretty(&other).unwrap_or_default(),
        Err(_) => raw,
    }
}

fn load_epic_context(db: &Connection, slug: &str) -> Result<EpicContext, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT slug, title, status, body_json, created_at, updated_at FROM cards WHERE slug = ?",
        &[json!(slug)],
    )?;
    let epic = match rows.as_array().and_then(|a| a.first()) {
        Some(r) => r.clone(),
        None => return Err(format!("epic-evaluate: EPIC '{slug}' not found")),
    };
    if !slug.starts_with("EPIC-") {
        return Err(format!(
            "epic-evaluate: card '{slug}' is not an EPIC \u{2014} use board-keeper.card.evaluate for non-EPIC cards"
        ));
    }
    let body = body_display(epic.get("body_json"));

    let children_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, status FROM cards WHERE parent_slug = ? ORDER BY id LIMIT 40",
        &[json!(slug)],
    )?;
    let children: Vec<Value> = children_rows.as_array().cloned().unwrap_or_default();
    let done_count = children.iter().filter(|c| c.get("status").and_then(Value::as_str) == Some("done")).count();

    let peers_rows = crate::db_json::query_rows(
        db,
        "SELECT slug, title FROM cards WHERE slug LIKE 'EPIC-%' AND slug != ? AND status IN ('backlog', 'doing') ORDER BY id LIMIT 10",
        &[json!(slug)],
    )?;
    let peers: Vec<Value> = peers_rows.as_array().cloned().unwrap_or_default();

    Ok(EpicContext { epic, body, children, done_count, peers })
}

fn children_block(children: &[Value]) -> String {
    if children.is_empty() {
        return "(none)".to_string();
    }
    children
        .iter()
        .map(|c| format!("  {} \u{b7} {}", str_qq(c.get("slug")), str_qq(c.get("status"))))
        .collect::<Vec<_>>()
        .join("\n")
}

fn peer_epics_block(peers: &[Value]) -> String {
    if peers.is_empty() {
        return "(none)".to_string();
    }
    peers
        .iter()
        .map(|p| format!("  {}: {}", str_qq(p.get("slug")), str_qq(p.get("title"))))
        .collect::<Vec<_>>()
        .join("\n")
}

// Pure — `ai.output_kv?.find(kv => kv.field_name === name)?.value_text ?? row?.value ?? ''`
fn get_field(ai_result: &Value, name: &str) -> String {
    let row = ai_result
        .get("output_kv")
        .and_then(Value::as_array)
        .and_then(|rows| rows.iter().find(|kv| kv.get("field_name").and_then(Value::as_str) == Some(name)));
    match row {
        None => String::new(),
        Some(r) => {
            if let Some(v) = r.get("value_text").filter(|v| !v.is_null()) {
                str_qq(Some(v))
            } else if let Some(v) = r.get("value").filter(|v| !v.is_null()) {
                str_qq(Some(v))
            } else {
                String::new()
            }
        }
    }
}

// Pure — builds the `evaluation` object appended to `body.ai_evaluations[]`. `now_iso` stands in
// for JS's `new Date().toISOString()` (frozen-clock convention: callers pass the fixed instant).
fn build_evaluation(ai_result: &Value, epic_updated_at: &Value, now_iso: &str) -> Value {
    let mut rec = get_field(ai_result, "recommendation").to_uppercase();
    if !VALID_RECS.contains(&rec.as_str()) {
        rec = "KEEP".to_string();
    }
    let reasoning: String = get_field(ai_result, "reasoning").chars().take(500).collect();
    let suggested = get_field(ai_result, "suggested_modifications");
    json!({
        "recommendation": rec,
        "reasoning": reasoning,
        "suggested_modifications": suggested,
        "ai_call_id": ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null),
        "tier_used": ai_result.get("tier_used").cloned().unwrap_or(Value::Null),
        "cost_usd": ai_result.get("cost_usd").cloned().unwrap_or(Value::Null),
        "epic_updated_at": epic_updated_at.clone(),
        "evaluated_at": now_iso,
    })
}

pub fn epicEvaluate(input: &Value, db: &Connection) -> Result<Value, String> {
    let epic_slug = match input.get("epic_slug").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => return Err("epic-evaluate: epic_slug is required".into()),
    };
    let force_refresh = input.get("force_refresh").and_then(Value::as_bool).unwrap_or(false);

    let ctx = load_epic_context(db, &epic_slug)?;

    // Cache by updated_at like card-evaluate. `JSON.parse(epic.body_json ?? '"{}"')`; non-object
    // parse result (or parse failure) → prevBody = null → prevEvals = [].
    let prev_body: Option<Map<String, Value>> = {
        let raw = str_qq(ctx.epic.get("body_json"));
        let text = if raw.is_empty() { "{}".to_string() } else { raw };
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(m)) => Some(m),
            _ => None,
        }
    };
    let prev_evals: Vec<Value> = prev_body
        .as_ref()
        .and_then(|b| b.get("ai_evaluations"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let last_eval = prev_evals.last();
    let epic_updated_at = ctx.epic.get("updated_at").cloned().unwrap_or(Value::Null);
    if !force_refresh {
        if let Some(le) = last_eval {
            if le.get("epic_updated_at") == Some(&epic_updated_at) {
                let mut out = le.clone();
                let obj = out.as_object_mut().unwrap();
                obj.insert("cached".to_string(), json!(true));
                obj.insert("epic_slug".to_string(), json!(epic_slug));
                return Ok(out);
            }
        }
    }

    ensure_prompt(db)?;

    // ── the AI call: PARITY-DEFERRED from here on (see module doc) — aiCall always returns Err.
    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "epic_slug", "value": ctx.epic.get("slug").cloned().unwrap_or(Value::Null) },
            { "key": "epic_title", "value": ctx.epic.get("title").cloned().unwrap_or(Value::Null) },
            { "key": "epic_status", "value": ctx.epic.get("status").cloned().unwrap_or(Value::Null) },
            { "key": "epic_created_at", "value": ctx.epic.get("created_at").cloned().unwrap_or(Value::Null) },
            { "key": "epic_updated_at", "value": epic_updated_at.clone() },
            { "key": "epic_body", "value": ctx.body.chars().take(1500).collect::<String>() },
            { "key": "total_children", "value": ctx.children.len().to_string() },
            { "key": "done_children", "value": ctx.done_count.to_string() },
            { "key": "children", "value": children_block(&ctx.children) },
            { "key": "peer_epics", "value": peer_epics_block(&ctx.peers) },
        ],
        "provider_fn": Value::Null,
    });
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let now_iso = crate::clock::now_iso(db); // real ISO in prod; pinned under test/conform
    let evaluation = build_evaluation(&ai_result, &epic_updated_at, &now_iso);

    let mut new_body: Map<String, Value> = prev_body.unwrap_or_default();
    let mut evals = prev_evals;
    evals.push(evaluation.clone());
    let start = evals.len().saturating_sub(10);
    let kept: Vec<Value> = evals[start..].to_vec();
    new_body.insert("ai_evaluations".to_string(), json!(kept));

    let _ = crate::engine_with_audit::withAudit(
        &json!({
            "skill": "card-update",
            "wrappedInput": { "slug": epic_slug, "body": Value::Object(new_body) },
            "operation": "epic.evaluate",
            "entity_type": "card",
            "entity_id": epic_slug,
        }),
        db,
    )?;

    let mut out = evaluation.as_object().unwrap().clone();
    out.insert("cached".to_string(), json!(false));
    out.insert("epic_slug".to_string(), json!(epic_slug));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT UNIQUE NOT NULL, title TEXT NOT NULL, status TEXT NOT NULL, parent_slug TEXT, body_json TEXT, created_at TEXT NOT NULL DEFAULT '2026-01-01T00:00:00.000Z', updated_at TEXT NOT NULL DEFAULT '2026-01-01T00:00:00.000Z');
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn epic_slug_is_required() {
        let c = setup();
        let e = epicEvaluate(&json!({}), &c).unwrap_err();
        assert_eq!(e, "epic-evaluate: epic_slug is required");
        let e2 = epicEvaluate(&json!({ "epic_slug": "" }), &c).unwrap_err();
        assert_eq!(e2, "epic-evaluate: epic_slug is required");
    }

    #[test]
    fn not_found_epic() {
        let c = setup();
        let e = epicEvaluate(&json!({ "epic_slug": "EPIC-MISSING" }), &c).unwrap_err();
        assert_eq!(e, "epic-evaluate: EPIC 'EPIC-MISSING' not found");
    }

    #[test]
    fn non_epic_slug_rejected() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('FEAT-X', 'a feature', 'backlog')", []).unwrap();
        let e = epicEvaluate(&json!({ "epic_slug": "FEAT-X" }), &c).unwrap_err();
        assert_eq!(e, "epic-evaluate: card 'FEAT-X' is not an EPIC \u{2014} use board-keeper.card.evaluate for non-EPIC cards");
    }

    #[test]
    fn reaches_the_parity_deferred_aicall_boundary_after_full_deterministic_surface() {
        // proves guard/load/cache-miss/ensure_prompt all ran cleanly and the error is aiCall's
        // deferred stub, not an earlier bug.
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, status, body_json) VALUES ('EPIC-BIG', 'a big epic', 'doing', '{}')", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, status, parent_slug) VALUES ('FEAT-CHILD-1', 'child 1', 'done', 'EPIC-BIG')", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, status, parent_slug) VALUES ('FEAT-CHILD-2', 'child 2', 'backlog', 'EPIC-BIG')", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, status) VALUES ('EPIC-PEER', 'peer epic', 'backlog')", []).unwrap();
        let err = epicEvaluate(&json!({ "epic_slug": "EPIC-BIG" }), &c).unwrap_err();
        assert!(err.contains("ai-call"), "unexpected error: {err}");
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'board-keeper.epic.evaluate.v1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn cache_hit_returns_last_eval_when_updated_at_unchanged_and_not_forced() {
        let c = setup();
        let body = json!({
            "ai_evaluations": [
                { "recommendation": "KEEP", "reasoning": "fine", "suggested_modifications": "", "epic_updated_at": "2026-01-01T00:00:00.000Z", "evaluated_at": "2026-01-01T00:00:00.000Z" }
            ]
        });
        c.execute(
            "INSERT INTO cards (slug, title, status, body_json, updated_at) VALUES ('EPIC-CACHED', 't', 'doing', ?1, '2026-01-01T00:00:00.000Z')",
            [body.to_string()],
        ).unwrap();
        let out = epicEvaluate(&json!({ "epic_slug": "EPIC-CACHED" }), &c).unwrap();
        assert_eq!(out["cached"], json!(true));
        assert_eq!(out["recommendation"], json!("KEEP"));
        assert_eq!(out["epic_slug"], json!("EPIC-CACHED"));
    }

    #[test]
    fn cache_bypassed_when_force_refresh_reaches_aicall_boundary() {
        let c = setup();
        let body = json!({
            "ai_evaluations": [
                { "recommendation": "KEEP", "reasoning": "fine", "suggested_modifications": "", "epic_updated_at": "2026-01-01T00:00:00.000Z", "evaluated_at": "2026-01-01T00:00:00.000Z" }
            ]
        });
        c.execute(
            "INSERT INTO cards (slug, title, status, body_json, updated_at) VALUES ('EPIC-FORCE', 't', 'doing', ?1, '2026-01-01T00:00:00.000Z')",
            [body.to_string()],
        ).unwrap();
        let err = epicEvaluate(&json!({ "epic_slug": "EPIC-FORCE", "force_refresh": true }), &c).unwrap_err();
        assert!(err.contains("ai-call"));
    }

    #[test]
    fn body_display_string_vs_object_vs_unparseable() {
        assert_eq!(body_display(Some(&json!("\"plain text\""))), "plain text");
        assert_eq!(body_display(None), "");
        let obj = body_display(Some(&json!("{\"kind\":\"epic\"}")));
        assert!(obj.contains("\"kind\": \"epic\""));
        // unparseable JSON falls back to the raw string
        assert_eq!(body_display(Some(&json!("not json {"))), "not json {");
    }

    #[test]
    fn get_field_prefers_value_text_then_value_then_empty() {
        let ai = json!({ "output_kv": [
            { "field_name": "recommendation", "value_text": "split" },
            { "field_name": "reasoning", "value": "fallback reasoning" },
            { "field_name": "suggested_modifications", "value_text": null, "value": null },
        ] });
        assert_eq!(get_field(&ai, "recommendation"), "split");
        assert_eq!(get_field(&ai, "reasoning"), "fallback reasoning");
        assert_eq!(get_field(&ai, "suggested_modifications"), "");
        assert_eq!(get_field(&ai, "missing_field"), "");
    }

    #[test]
    fn build_evaluation_defaults_invalid_recommendation_to_keep_and_truncates_reasoning() {
        let ai = json!({ "output_kv": [
            { "field_name": "recommendation", "value_text": "bogus" },
            { "field_name": "reasoning", "value_text": "x".repeat(600) },
        ], "ai_call_id": "call-1", "tier_used": "mid", "cost_usd": 0.01 });
        let ev = build_evaluation(&ai, &json!("2026-01-01T00:00:00.000Z"), "2026-01-02T00:00:00.000Z");
        assert_eq!(ev["recommendation"], json!("KEEP"));
        assert_eq!(ev["reasoning"].as_str().unwrap().chars().count(), 500);
        assert_eq!(ev["ai_call_id"], json!("call-1"));
        assert_eq!(ev["epic_updated_at"], json!("2026-01-01T00:00:00.000Z"));
        assert_eq!(ev["evaluated_at"], json!("2026-01-02T00:00:00.000Z"));
    }

    #[test]
    fn build_evaluation_keeps_valid_recommendation_uppercased() {
        let ai = json!({ "output_kv": [
            { "field_name": "recommendation", "value_text": "split" },
            { "field_name": "reasoning", "value_text": "too big" },
            { "field_name": "suggested_modifications", "value_text": "split into 2" },
        ] });
        let ev = build_evaluation(&ai, &Value::Null, "2026-01-01T00:00:00.000Z");
        assert_eq!(ev["recommendation"], json!("SPLIT"));
        assert_eq!(ev["suggested_modifications"], json!("split into 2"));
    }

    #[test]
    fn children_and_peer_blocks_empty_and_populated() {
        assert_eq!(children_block(&[]), "(none)");
        assert_eq!(peer_epics_block(&[]), "(none)");
        let children = vec![json!({ "slug": "FEAT-A", "status": "done" })];
        assert_eq!(children_block(&children), "  FEAT-A \u{b7} done");
        let peers = vec![json!({ "slug": "EPIC-B", "title": "other epic" })];
        assert_eq!(peer_epics_block(&peers), "  EPIC-B: other epic");
    }
}
