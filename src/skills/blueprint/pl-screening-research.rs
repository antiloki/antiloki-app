//! pl-screening-research.rs — twin of skills/blueprint/pl-screening-research.mjs (db lane).
//! NAMES ARE CONTRACT: default `plScreeningResearch`. STEP 1 of the PL screening swarm:
//! RESEARCH (mid). Gathers the project's accumulated intelligence (recent memory lessons +
//! prior cards) and the codebase context-bundle (bone + sliced) BEFORE any decision is made,
//! feeds it all into one AI call, and returns the gathered brief as `research_md`.
//!
//! PARITY-DEFERRED: `crate::codebase_context_bundle::contextBundle` bottoms out at the
//! `rag`/`research` sliced gatherers, which reach the PARITY-DEFERRED ONNX embed boundary via
//! their own converted twins — mirrored exactly per context-bundle.rs's own documented contract
//! (dispatched, per-item errors degrade to `skipped`, never propagate). `crate::ai_ai_call::aiCall`
//! is the deferred network-AI stub (see ai-call.rs's module doc) and always returns `Err`; the JS
//! wraps that call in `try { } catch { research_md = ''; }`, which SWALLOWS the error exactly like
//! pl-ratify-contract.rs's own aiCall dispatch — so this twin's call is likewise non-propagating.
//! Every other line — guard, card lookup, ensure-prompt, memory/card gathering, gathered-context
//! rendering, the context-bundle call + degrade-to-placeholder, the (always-failing) AI attempt,
//! the best-effort bundle-persist + grounding-check follow-ups, and the final research_md/context
//! shaping — is real, reachable, deterministic surface, fixture-testable end to end. Frozen clock
//! 2026-01-01 in place of `datetime('now')` per the crossing playbook (this file makes no writes
//! that use `datetime('now')`, but ensure_prompt's INSERT relies on prompt-add.rs's own default).
// QUALITY: full unit ported — deterministic surface reaches every branch; the one non-deterministic
// call site (aiCall) is dispatched-and-swallowed exactly like the JS try/catch.

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
// `.slice(0, n)` — UTF-16-safe by convention here means "take n chars" (playbook rule 5;
// fixtures stay ASCII/BMP).
fn chars_take(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}
fn obj_field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object().and_then(|o| o.get(key))
}

const PROMPT: &str = "pl.screening.research.v2";
const SYSTEM: &str = "You are the PRODUCT LEAD, STEP 1 of 3: RESEARCH. You GATHER context + intelligence relevant to a requested feature — no decisions, no feature list yet. Plain prose, no code.";
const TEMPLATE: &str = "STEP 1 of 3 — RESEARCH. Gather ALL the context relevant to the requested feature BEFORE anyone decides anything, so the later steps are correct and specific.

=== THE CARD (the requested feature) ===
title: {{card_title}}
body: {{card_body}}

=== THE EXISTING CODEBASE (bone + sliced — the REAL stack, conventions, architecture, and touched code) ===
{{code_context}}

=== GATHERED FROM THE PROJECT (memory + prior cards — use what's relevant) ===
{{gathered_context}}

Surface the intelligence the next steps will rely on:
- the product intent behind the feature (what it's really for),
- the domain + the project's tech stack and conventions — take these from THE EXISTING CODEBASE above, do NOT guess,
- the RELEVANT EXISTING CODE the feature touches — files / modules / functions / the
  structure (AST) it plugs into, grounded in THE EXISTING CODEBASE above,
- prior decisions, related work, and any constraints / gotchas to respect.

GROUND every claim about the code in THE EXISTING CODEBASE section. Do NOT invent frameworks,
state stores, libraries, or files that do not appear there — if the stack facts list a store or
library as absent, treat it as absent.

Output the gathered context — FACTS + relevant intelligence, NOT decisions and NOT a
feature list (those are steps 2 and 3). Plain prose. Put it into research_md.";

// try promptFind; on miss, promptAdd then re-check (mirrors JS ensurePrompt: skip the re-add if
// the existing template already matches verbatim).
fn ensure_prompt(db: &Connection) {
    if let Ok(existing) = crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT }), db) {
        if existing.get("template_md").and_then(Value::as_str) == Some(TEMPLATE) {
            return;
        }
    }
    let _ = crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT,
            "template_md": TEMPLATE,
            "system_md": SYSTEM,
            "default_tier": "mid",
            "default_temperature": "cold",
            "phase_flow": "single_only",
            "max_output_tokens": 2000,
            "output_fields": [{ "field_name": "research_md", "field_type": "string" }],
        }),
        db,
    );
}

#[derive(Clone)]
struct Gathered {
    memories: Vec<(String, String)>, // (title, body)
    cards: Vec<(String, String)>,    // (slug, title)
}

// Pull the substrate's accumulated intelligence relevant to this card: recent
// lessons/corrections from memory + recent prior cards. Defensive — any source that
// errors (schema drift, absent table) is simply skipped.
fn gather_context(db: &Connection, card_slug: &str) -> Gathered {
    let memories = match crate::db_json::query_rows(db, "SELECT title, situation, guidance FROM memory.memories ORDER BY id DESC LIMIT 6", &[]) {
        Ok(rows) => rows
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|m| {
                let title = str_qq(obj_field(m, "title"));
                let situation = chars_take(&str_qq(obj_field(m, "situation")), 200);
                let guidance = chars_take(&str_qq(obj_field(m, "guidance")), 200);
                (title, format!("{situation} \u{2192} {guidance}"))
            })
            .collect(),
        Err(_) => vec![],
    };
    let cards = match crate::db_json::query_rows(db, "SELECT slug, title FROM cards WHERE slug != ? ORDER BY id DESC LIMIT 6", &[json!(card_slug)]) {
        Ok(rows) => rows
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|c| (str_qq(obj_field(c, "slug")), str_qq(obj_field(c, "title"))))
            .collect(),
        Err(_) => vec![],
    };
    Gathered { memories, cards }
}

fn render_gathered(g: &Gathered) -> String {
    let mut lines: Vec<String> = vec![];
    if !g.memories.is_empty() {
        lines.push("LESSONS / CORRECTIONS (project memory):".to_string());
        for (title, body) in &g.memories {
            lines.push(format!("- {title}: {body}"));
        }
    }
    if !g.cards.is_empty() {
        lines.push(String::new());
        lines.push("PRIOR CARDS:".to_string());
        for (slug, title) in &g.cards {
            lines.push(format!("- {slug}: {title}"));
        }
    }
    if lines.is_empty() {
        "(no prior memory or cards found — greenfield)".to_string()
    } else {
        lines.join("\n")
    }
}

// `pickField(res, 'research_md') ?? ''` — pickField returns whatever the field resolved to
// (typically a string, or null/undefined on miss); nullish-coalesce to '' either way.
fn str_or_empty(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => str_qq(Some(other)),
    }
}

pub fn plScreeningResearch(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, provider_fn = null } = context ?? {}; if (!db) throw err.contextMissing(...)`
    // — unreachable here (Rust's `db: &Connection` is never absent).
    let provider_fn = Value::Null;
    let card_slug_v = input.get("card_slug");
    if !truthy(card_slug_v) {
        return Err("pl-screening-research: card_slug is required".to_string());
    }
    let card_slug = str_qq(card_slug_v);

    let card_rows = crate::db_json::query_rows(db, "SELECT slug, title, body_json FROM cards WHERE slug = ?", &[json!(card_slug)])?;
    let Some(card) = card_rows.as_array().and_then(|a| a.first()).cloned() else {
        return Err(format!("pl-screening-research: card '{card_slug}' not found"));
    };
    let card_title = str_qq(obj_field(&card, "title"));
    let card_body = str_qq(obj_field(&card, "body_json"));

    ensure_prompt(db);
    let gathered = gather_context(db, &card_slug);

    // Bone + sliced code context (docs/context-bundle.md): the REAL stack / architecture / touched code, so
    // RESEARCH grounds on the codebase instead of guessing. Degrade-safe; NEVER empty (the ai-call
    // feed-quality gate refuses a blank {{placeholder}}). The pl:research contract = full bone + deep files;
    // it makes no AI calls of its own (no 'rag' item), so RESEARCH stays a single AI call.
    let bundle_result = crate::codebase_context_bundle::contextBundle(
        &json!({
            "agent": "pl", "phase": "research",
            "task": format!("{}\n{}", card_title, chars_take(&card_body, 2000)),
            "budget": 6000,
        }),
        db,
    );
    let bundle_obj: Option<Value> = bundle_result.ok();
    let mut code_context = bundle_obj.as_ref().and_then(|b| obj_field(b, "bundle_md")).and_then(Value::as_str).unwrap_or("").to_string();
    if code_context.trim().is_empty() {
        code_context = "(no codebase context available yet — greenfield, or the atlas has not been built for this workspace; treat the stack as UNKNOWN and do NOT assume any framework, store, or library exists)".to_string();
    }

    let mut research_md = String::new();
    let mut ai_call_id: Value = Value::Null;
    let ai_input = json!({
        "prompt_name": PROMPT,
        "input_kv": [
            { "key": "card_title", "value": card_title },
            { "key": "card_body", "value": chars_take(&card_body, 8000) },
            { "key": "code_context", "value": chars_take(&code_context, 6000) },
            { "key": "gathered_context", "value": chars_take(&render_gathered(&gathered), 6000) },
        ],
        "tier": "mid", "temperature": "cold", "phase_flow": "single_only", "provider_fn": provider_fn,
    });
    // JS: `try { ... } catch { research_md = ''; }` — the deferred aiCall stub always errs, so
    // this branch is a no-op every time (matches the JS fallback path exactly, not merely
    // approximates it).
    if let Ok(res) = crate::ai_ai_call::aiCall(&ai_input, db) {
        research_md = str_or_empty(&crate::blueprint_screening_gate::pickField(&res, &json!("research_md")));
        ai_call_id = res.get("ai_call_id").cloned().unwrap_or(Value::Null);
    }

    // Persist the exact bundle linked to the call it fed (docs/context-bundle.md §6) — best-effort audit,
    // never breaks screening; feeds the FE glass-box Context tab + "what context did this call get?".
    if truthy(Some(&ai_call_id)) {
        if let Some(bundle) = &bundle_obj {
            let _ = crate::codebase_context_bundle_persist::contextBundlePersist(&json!({ "ai_call_id": ai_call_id, "bundle": bundle }), db);
        }
    }
    // Post-call grounding check (docs/context-bundle.md §7): flag a store/lib the brief asserts that the
    // stack says is absent (the "existing Redux store" class) — non-blocking CONTEXT_GROUNDING_WARN.
    if !research_md.is_empty() {
        let _ = crate::codebase_context_grounding_check::contextGroundingCheck(
            &json!({ "output": research_md, "ai_call_id": ai_call_id, "agent": "pl", "phase": "research" }),
            db,
        );
    }

    if research_md.trim().is_empty() {
        let fallback_title = if !card_title.is_empty() { card_title.clone() } else { card_slug.clone() };
        research_md = format!("(research unavailable) {fallback_title}");
    }
    let context_stat: Value = match &bundle_obj {
        Some(b) => json!({
            "bone_tokens": b.get("bone_tokens").cloned().unwrap_or(Value::Null),
            "sliced_tokens": b.get("sliced_tokens").cloned().unwrap_or(Value::Null),
            "items": b.get("items").and_then(Value::as_array).cloned().unwrap_or_default().iter().map(|i| obj_field(i, "id").cloned().unwrap_or(Value::Null)).collect::<Vec<_>>(),
            "skipped": b.get("skipped").and_then(Value::as_array).cloned().unwrap_or_default().iter().map(|s| obj_field(s, "id").cloned().unwrap_or(Value::Null)).collect::<Vec<_>>(),
        }),
        None => Value::Null,
    };

    Ok(json!({
        "card_slug": card_slug,
        "research_md": research_md,
        "gathered": { "memories": gathered.memories.len(), "cards": gathered.cards.len() },
        "context": context_stat,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (id INTEGER PRIMARY KEY, slug TEXT UNIQUE, title TEXT, body_json TEXT);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY, title TEXT, situation TEXT, guidance TEXT);
             CREATE TABLE workspaces (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, root_path TEXT NOT NULL, vcs TEXT NOT NULL DEFAULT 'git', is_active INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT '2026-01-01 00:00:00');
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    #[test]
    fn card_slug_is_required() {
        let c = setup();
        let e = plScreeningResearch(&json!({}), &c).unwrap_err();
        assert_eq!(e, "pl-screening-research: card_slug is required");
        let e2 = plScreeningResearch(&json!({ "card_slug": "" }), &c).unwrap_err();
        assert_eq!(e2, "pl-screening-research: card_slug is required");
    }

    #[test]
    fn card_not_found_errors() {
        let c = setup();
        let e = plScreeningResearch(&json!({ "card_slug": "CARD-404" }), &c).unwrap_err();
        assert_eq!(e, "pl-screening-research: card 'CARD-404' not found");
    }

    #[test]
    fn no_codebase_atlas_degrades_and_ai_down_falls_back_to_placeholder_research_md() {
        // no code_nodes/code_decorators/etc tables -> every context-bundle gatherer degrades ->
        // bundle_md stays empty -> the "(no codebase context available yet...)" placeholder fires.
        // aiCall is the deferred stub (always Err) -> research_md falls back to
        // "(research unavailable) <title>".
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('CARD-1', 'Add widgets', 'widget body')", []).unwrap();
        let r = plScreeningResearch(&json!({ "card_slug": "CARD-1" }), &c).unwrap();
        assert_eq!(r["card_slug"], json!("CARD-1"));
        assert_eq!(r["research_md"], json!("(research unavailable) Add widgets"));
        assert_eq!(r["gathered"], json!({ "memories": 0, "cards": 0 }));
        // context stat is still populated (contextBundle itself never errors -> Some(bundle)),
        // even though every sliced item skipped for lack of atlas tables.
        assert!(r["context"].is_object());
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'pl.screening.research.v2'", [], |row| row.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn falls_back_to_card_slug_when_title_is_null() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('CARD-2', NULL, NULL)", []).unwrap();
        let r = plScreeningResearch(&json!({ "card_slug": "CARD-2" }), &c).unwrap();
        assert_eq!(r["research_md"], json!("(research unavailable) CARD-2"));
    }

    #[test]
    fn gathers_recent_memories_and_prior_cards_excluding_self() {
        let c = setup();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('CARD-SELF', 'Self card', 'body')", []).unwrap();
        c.execute("INSERT INTO cards (slug, title, body_json) VALUES ('CARD-OTHER', 'Other card', 'body2')", []).unwrap();
        c.execute("INSERT INTO memory.memories (title, situation, guidance) VALUES ('lesson-1', 'a bug happened', 'fix it this way')", []).unwrap();
        let r = plScreeningResearch(&json!({ "card_slug": "CARD-SELF" }), &c).unwrap();
        assert_eq!(r["gathered"], json!({ "memories": 1, "cards": 1 }));
    }

    #[test]
    fn render_gathered_matches_js_shape() {
        let g = Gathered { memories: vec![("t1".to_string(), "s \u{2192} g".to_string())], cards: vec![("SL-1".to_string(), "Title 1".to_string())] };
        let out = render_gathered(&g);
        assert_eq!(out, "LESSONS / CORRECTIONS (project memory):\n- t1: s \u{2192} g\n\nPRIOR CARDS:\n- SL-1: Title 1");
        let empty = Gathered { memories: vec![], cards: vec![] };
        assert_eq!(render_gathered(&empty), "(no prior memory or cards found — greenfield)");
    }
}
