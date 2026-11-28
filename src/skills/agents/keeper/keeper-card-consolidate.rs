//! keeper-card-consolidate.rs — twin of skills/agents/keeper/keeper-card-consolidate.mjs (db,
//! cross-db memory.*/brain.* WRITE lane). NAMES ARE CONTRACT: `keeperCardConsolidate` (default) +
//! `PROMPT_NAME` (named, `"keeper.card.consolidate.v1"`). Post-QA pipeline step 2: reads the
//! man.card.review phase event's evidence (verdict + findings) plus deterministic card facts
//! (timeline count, runtime errors, AI cost/duration, QA retries), then asks the KEEPER faculty
//! (mid+warm) for a short reflection. Inserts a `memory.memories` type='lesson' row and fires a
//! `card_phase_events` phase='keeper_consolidated' row UNCONDITIONALLY — even when the AI call
//! fails, a deterministic degraded reflection takes its place (FIX-KEEPER-WEDGE, per the JS's own
//! comment): the post-QA chain must never wedge on a provider hiccup.
//!
//! PARITY-DEFERRED: the `aiCall` leg (`crate::ai_ai_call::aiCall`) — see ai-call.rs's module doc,
//! always `Err`. Because JS wraps THIS SPECIFIC call in a try/catch with a deterministic degraded
//! fallback (not an unconditional propagate, unlike most PARITY-DEFERRED twins), the deferred stub
//! drives the exact same branch a real network failure would: this twin always takes the
//! `degraded=true` path today, matching JS behavior on any aiCall failure byte-for-byte. The
//! output_kv aggregation logic (array_of_string handling for the `lessons` field) is ported
//! faithfully as the pure `aggregate_ai_output` fn and unit-tested directly with canned
//! `ai_call`-shaped response values — mirrors book-run.rs's documented reasoning for why this
//! parsing code is otherwise unreachable by this file's own `keeperCardConsolidate` tests (it only
//! runs after a successful aiCall).
//!
//! `ANTILOKI_KEEPER_CONSOLIDATE_STUB=1` — deterministic canned-output short-circuit, ported
//! byte-for-byte (matches JS's own stub branch): inserts a stub memory row + fires the phase
//! event, skips `ensurePrompt`/`aiCall` entirely.
//!
//! `context.provider_fn` (JS) has no Rust equivalent (opaque JS function reference) — threaded as
//! `null` into the aiCall input, same convention as book-run.rs/interviewer-present.rs.
//! `context.db` missing → JS `err.contextMissing` — unreachable here (`db: &Connection` is a
//! required, non-nullable reference, same convention as every sibling db-lane twin).
//! `context.actor` defaults to `'keeper:auto'` — the db lane only threads `input`+`db` (no context
//! object), so the JS default is the only reachable value (mirrors keeper-journey-append.rs).
// QUALITY: db WRITE lane, aiCall PARITY-DEFERRED with a ported degraded-fallback branch; stub-mode
// ported; deterministic surface (guards, fact assembly, memory insert, phase event) fully tested.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

pub const PROMPT_NAME: &str = "keeper.card.consolidate.v1";

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
fn tmpl(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}
fn first(rows: &Value) -> Option<Value> { rows.as_array().and_then(|a| a.first()).cloned() }

const PROMPT_TEMPLATE_MD: &str = "You are the KEEPER, the substrate's institutional memory. A card just finished its journey through the pipeline. Distill the experience into a SHORT reflection that future agents can learn from.\n\nCARD: {{card_slug}}\nTITLE: {{card_title}}\nVERDICT (from MAN review): {{verdict}}\nFINDINGS (concerns or failures, may be empty):\n{{findings_block}}\n\nMETRICS:\n- timeline events: {{timeline_count}}\n- runtime errors: {{error_count}}\n- AI cost: ${{cost_usd}}\n- wall-clock: {{duration_sec}}s\n- QA retries: {{qa_retry_count}}\n\nOUTPUT — JSON of this exact shape:\n{\n  \"reflection_md\": \"<4-6 sentences. Cover: what was the actual outcome, what was hard or surprising, what would future agents do differently. Concrete. No filler.>\",\n  \"lessons\": [\"<one short lesson per element, 1-4 items, each ≤ 80 chars>\"],\n  \"would_repeat\": <boolean — would future agents use the same approach?>\n}\n\nRules:\n  - Lessons are SHORT and FUTURE-FACING (\"when facing X, prefer Y\"). Not summaries.\n  - No fabrication. If findings is empty, say so directly.\n  - No hedging language (\"perhaps\", \"maybe\"). Speak with confidence the metrics support.";

fn prompt_output_fields() -> Value {
    json!([
        { "field_name": "reflection_md", "field_type": "string", "required": true },
        { "field_name": "lessons",       "field_type": "array_of_string", "required": true },
        { "field_name": "would_repeat",  "field_type": "boolean", "required": true },
    ])
}

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
            "default_temperature": "warm",
            "max_output_tokens": 700,
            "output_fields": prompt_output_fields(),
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!({ "name": PROMPT_NAME }), db)?;
    Ok(())
}

fn load_man_review_evidence(db: &Connection, card_slug: &Value) -> Result<Value, String> {
    let rows = crate::db_json::query_rows(
        db,
        "SELECT evidence_json FROM card_phase_events WHERE card_slug = ? AND phase = 'man_reviewed' ORDER BY id DESC LIMIT 1",
        &[card_slug.clone()],
    )?;
    let Some(row) = first(&rows) else { return Ok(Value::Null) };
    let raw = row.get("evidence_json").and_then(|v| v.as_str()).unwrap_or("{}");
    Ok(serde_json::from_str::<Value>(raw).unwrap_or(Value::Null))
}

struct CardFacts {
    card: Value,
    timeline_count: i64,
    error_count: i64,
    cost_usd: f64,
    duration_sec: i64,
    qa_retry_count: i64,
}

fn load_card_facts(db: &Connection, card_slug: &Value) -> Result<Option<CardFacts>, String> {
    let card_rows = crate::db_json::query_rows(db, "SELECT slug, title, status FROM cards WHERE slug = ?", &[card_slug.clone()])?;
    let Some(card) = first(&card_rows) else { return Ok(None) };

    let tl_rows = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS c FROM sacred_timeline WHERE entity_type='card' AND entity_id=?",
        &[card_slug.clone()],
    )?;
    let timeline_count = first(&tl_rows).and_then(|r| r.get("c").and_then(|v| v.as_i64())).unwrap_or(0);

    let re_rows = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS errs FROM runtime_events r JOIN pl_sessions s ON r.user_goal_id = s.id WHERE s.approved_card_slug = ? AND r.outcome = 'error'",
        &[card_slug.clone()],
    )?;
    let error_count = first(&re_rows).and_then(|r| r.get("errs").and_then(|v| v.as_i64())).unwrap_or(0);

    let ai_rows = crate::db_json::query_rows(
        db,
        "SELECT COALESCE(SUM(a.cost_usd_actual), 0) AS cost, COALESCE(SUM(a.duration_ms), 0) AS dur \
         FROM brain.ai_calls a JOIN runtime_events r ON r.id = a.parent_event_id JOIN pl_sessions s ON r.user_goal_id = s.id \
         WHERE s.approved_card_slug = ?",
        &[card_slug.clone()],
    )?;
    let ai_row = first(&ai_rows);
    let cost_usd = ai_row.as_ref().and_then(|r| r.get("cost")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let dur_ms = ai_row.as_ref().and_then(|r| r.get("dur")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let duration_sec = (dur_ms / 1000.0).round() as i64;

    let qa_rows = crate::db_json::query_rows(
        db,
        "SELECT COUNT(*) AS retries FROM qa_runs q JOIN contracts c ON q.contract_slug = c.slug WHERE c.card_slug = ?",
        &[card_slug.clone()],
    )?;
    let retries = first(&qa_rows).and_then(|r| r.get("retries").and_then(|v| v.as_i64())).unwrap_or(1);
    let qa_retry_count = (retries - 1).max(0);

    Ok(Some(CardFacts { card, timeline_count, error_count, cost_usd, duration_sec, qa_retry_count }))
}

// `(manEvidence?.findings ?? []).map(f => `- ${f.severity}: ${f.kind} (${f.ref}): ${f.snippet}`).join('\n') || '(no findings)'`
fn findings_block(man_evidence: &Value) -> String {
    let findings = man_evidence.get("findings").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    if findings.is_empty() {
        return "(no findings)".to_string();
    }
    let lines: Vec<String> = findings
        .iter()
        .map(|f| {
            format!(
                "- {}: {} ({}): {}",
                tmpl(f.get("severity")),
                tmpl(f.get("kind")),
                tmpl(f.get("ref")),
                tmpl(f.get("snippet")),
            )
        })
        .collect();
    let joined = lines.join("\n");
    if joined.is_empty() { "(no findings)".to_string() } else { joined }
}

/// Reconstructs `{ reflection_md, lessons, would_repeat }` from an `ai_call`-shaped response's
/// `output_kv` rows (array_of_string fields appear as multi-row, ordered by `order_index`). Pure —
/// this is the code path a real (non-deferred) aiCall success would drive; unreachable via
/// `keeperCardConsolidate` itself while aiCall is the always-Err stub, so exercised directly here
/// with canned values (same reasoning as book-run.rs's `kv_out`/`parse_arr`).
fn aggregate_ai_output(ai_result: &Value) -> (String, Vec<String>, bool) {
    let mut out: Map<String, Value> = Map::new();
    let mut lessons_slots: Vec<Option<String>> = Vec::new();
    if let Some(rows) = ai_result.get("output_kv").and_then(|v| v.as_array()) {
        for row in rows {
            let field_name = row.get("field_name").and_then(|v| v.as_str()).unwrap_or("");
            if field_name == "lessons" {
                // `list[order_index ?? list.length] = row.value_text ?? ''` — a present row always
                // writes a string (possibly empty); only unwritten indices (order_index gaps) stay
                // sparse-`undefined`, which JS's later `.filter(typeof === 'string')` drops.
                let idx = row.get("order_index").and_then(|v| v.as_i64()).unwrap_or(lessons_slots.len() as i64) as usize;
                if lessons_slots.len() <= idx { lessons_slots.resize(idx + 1, None); }
                let text = row.get("value_text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                lessons_slots[idx] = Some(text);
            } else if let Some(t) = row.get("value_text").filter(|v| !v.is_null()) {
                out.insert(field_name.to_string(), t.clone());
            } else if let Some(n) = row.get("value_number").filter(|v| !v.is_null()) {
                out.insert(field_name.to_string(), n.clone());
            } else if let Some(b) = row.get("value_boolean").filter(|v| !v.is_null()) {
                out.insert(field_name.to_string(), json!(truthy(Some(b))));
            }
        }
    }
    let lessons: Vec<String> = lessons_slots.into_iter().flatten().collect();
    let reflection_md = out.get("reflection_md").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let would_repeat = out.get("would_repeat").map(|v| truthy(Some(v))).unwrap_or(false);
    (reflection_md, lessons, would_repeat)
}

#[allow(non_snake_case)]
pub fn keeperCardConsolidate(input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db, actor = 'keeper:auto', provider_fn = null } = context ?? {}` — `db` missing
    // is unreachable here (non-nullable reference); the db lane threads only `input`+`db` (no
    // context object), so `actor` is always the JS default and `provider_fn` is always null
    // (mirrors keeper-journey-append.rs).
    let actor = "keeper:auto";
    let provider_fn = Value::Null;

    if !truthy(input.get("card_slug")) {
        return Err("keeper-card-consolidate: card_slug is required".to_string());
    }
    let card_slug = input.get("card_slug").cloned().unwrap_or(Value::Null);

    let Some(facts) = load_card_facts(db, &card_slug)? else {
        return Err(format!("keeper-card-consolidate: card '{}' not found", tmpl(Some(&card_slug))));
    };
    let man_evidence = load_man_review_evidence(db, &card_slug)?;
    let verdict = man_evidence.get("verdict").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();

    // Stub mode — deterministic canned output for tests.
    if std::env::var("ANTILOKI_KEEPER_CONSOLIDATE_STUB").as_deref() == Ok("1") {
        let card_slug_s = tmpl(Some(&card_slug));
        let title = format!("card-reflection: {card_slug_s}");
        let guidance = format!("(stub) Card {card_slug_s} closed with verdict={verdict}.");
        let situation = "(stub) journey through pipeline".to_string();
        let facts_json = json!({
            "card": facts.card, "timeline_count": facts.timeline_count, "error_count": facts.error_count,
            "cost_usd": facts.cost_usd, "duration_sec": facts.duration_sec, "qa_retry_count": facts.qa_retry_count,
        });
        let tags_json = json!({ "verdict": verdict, "would_repeat": true, "lessons": ["stub-lesson"], "metrics": facts_json });
        let (_c, mem_id) = crate::db_json::run_write(
            db,
            "INSERT INTO memory.memories (type, title, guidance, situation, tags_json, source_card, validated, created_by) \
             VALUES ('lesson', ?, ?, ?, ?, ?, 1, ?)",
            &[json!(title), json!(guidance), json!(situation), json!(serde_json::to_string(&tags_json).map_err(|e| e.to_string())?), card_slug.clone(), json!(actor)],
        )?;
        crate::entities_card_phase_event_record::cardPhaseEventRecord(
            &json!({ "card_slug": card_slug, "phase": "keeper_consolidated", "actor": actor, "evidence": { "memory_id": mem_id, "verdict": verdict } }),
            db,
        )?;
        return Ok(json!({
            "card_slug": card_slug, "memory_id": mem_id, "verdict": verdict,
            "lessons": ["stub-lesson"], "would_repeat": true,
            "reflection_md": format!("(stub) Card {card_slug_s} closed."),
            "cached": false, "cost_usd": 0,
        }));
    }

    ensure_prompt(db)?;
    let findings_block_s = findings_block(&man_evidence);

    // FIX-KEEPER-WEDGE: the aiCall MUST be guarded (see module doc) — deterministic write + phase
    // event fire UNCONDITIONALLY below; the AI enrichment is best-effort.
    let mut reflection_md;
    let mut lessons: Vec<String> = Vec::new();
    let mut would_repeat = verdict != "fail" && verdict != "blocked";
    let mut ai_result: Value = Value::Null;
    let mut degraded = false;

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "card_slug",      "value": card_slug },
            { "key": "card_title",     "value": facts.card.get("title").cloned().unwrap_or(json!("")) },
            { "key": "verdict",        "value": verdict },
            { "key": "findings_block", "value": findings_block_s },
            { "key": "timeline_count", "value": facts.timeline_count.to_string() },
            { "key": "error_count",    "value": facts.error_count.to_string() },
            { "key": "cost_usd",       "value": format!("{:.4}", facts.cost_usd) },
            { "key": "duration_sec",   "value": facts.duration_sec.to_string() },
            { "key": "qa_retry_count", "value": facts.qa_retry_count.to_string() },
        ],
        "tier": "mid", "temperature": "warm",
        "provider_fn": provider_fn,
    });
    // PARITY-DEFERRED boundary: crate::ai_ai_call::aiCall is a network stub that always returns
    // Err — see module doc. This mirrors JS's try/catch: on Err the degraded fallback below fires,
    // exactly as a real provider failure would drive the same JS catch block.
    match crate::ai_ai_call::aiCall(&ai_input, db) {
        Ok(res) => {
            let (rmd, ls, wr) = aggregate_ai_output(&res);
            reflection_md = rmd;
            lessons = ls;
            would_repeat = wr;
            ai_result = res;
        }
        Err(e) => {
            degraded = true;
            reflection_md = format!(
                "Card {} closed with verdict={} after {} timeline events, {} runtime errors and {} QA retries (cost ${:.4}, {}s). Reflection AI unavailable ({}) — degraded deterministic record.",
                tmpl(Some(&card_slug)), verdict, facts.timeline_count, facts.error_count, facts.qa_retry_count,
                facts.cost_usd, facts.duration_sec, e.chars().take(80).collect::<String>(),
            );
        }
    }

    let facts_json = json!({
        "card": facts.card, "timeline_count": facts.timeline_count, "error_count": facts.error_count,
        "cost_usd": facts.cost_usd, "duration_sec": facts.duration_sec, "qa_retry_count": facts.qa_retry_count,
    });
    let title = format!("card-reflection: {}", tmpl(Some(&card_slug)));
    let situation = format!(
        "card {} verdict={} cost=${:.4} duration={}s{}",
        tmpl(Some(&card_slug)), verdict, facts.cost_usd, facts.duration_sec,
        if degraded { " [degraded]" } else { "" },
    );
    let tags_json = json!({ "verdict": verdict, "would_repeat": would_repeat, "lessons": lessons, "metrics": facts_json, "degraded": degraded });
    let (_c, memory_id) = crate::db_json::run_write(
        db,
        "INSERT INTO memory.memories (type, title, guidance, situation, tags_json, source_card, validated, created_by) \
         VALUES ('lesson', ?, ?, ?, ?, ?, 1, ?)",
        &[json!(title), json!(reflection_md), json!(situation), json!(serde_json::to_string(&tags_json).map_err(|e| e.to_string())?), card_slug.clone(), json!(actor)],
    )?;

    let ai_call_id = ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null);
    let cost_usd = ai_result.get("cost_usd").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cached = ai_result.get("cached").map(|v| truthy(Some(v))).unwrap_or(false);

    // Phase event — fires UNCONDITIONALLY so the card can never wedge here.
    crate::entities_card_phase_event_record::cardPhaseEventRecord(
        &json!({
            "card_slug": card_slug, "phase": "keeper_consolidated", "actor": actor,
            "evidence": { "memory_id": memory_id, "verdict": verdict, "lessons_count": lessons.len(), "would_repeat": would_repeat, "cost_usd": cost_usd, "degraded": degraded },
        }),
        db,
    )?;

    Ok(json!({
        "card_slug": card_slug, "memory_id": memory_id, "verdict": verdict,
        "reflection_md": reflection_md, "lessons": lessons, "would_repeat": would_repeat,
        "cached": cached, "cost_usd": cost_usd, "ai_call_id": ai_call_id, "degraded": degraded,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env_lock::ENV_LOCK;

    fn setup() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS memory; ATTACH ':memory:' AS brain;").unwrap();
        c.execute_batch(
            "CREATE TABLE cards (slug TEXT PRIMARY KEY, title TEXT, status TEXT);
             CREATE TABLE card_phase_events (id INTEGER PRIMARY KEY, card_slug TEXT, phase TEXT, actor TEXT, evidence_json TEXT);
             CREATE TABLE sacred_timeline (id INTEGER PRIMARY KEY, entity_type TEXT, entity_id TEXT);
             CREATE TABLE pl_sessions (id INTEGER PRIMARY KEY, approved_card_slug TEXT);
             CREATE TABLE runtime_events (id INTEGER PRIMARY KEY, user_goal_id INTEGER, outcome TEXT);
             CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT);
             CREATE TABLE qa_runs (id INTEGER PRIMARY KEY, contract_slug TEXT);
             CREATE TABLE brain.ai_calls (id INTEGER PRIMARY KEY, parent_event_id INTEGER, cost_usd_actual REAL, duration_ms REAL);
             CREATE TABLE memory.memories (id INTEGER PRIMARY KEY AUTOINCREMENT, type TEXT, title TEXT, guidance TEXT, situation TEXT, tags_json TEXT, source_card TEXT, validated INTEGER, created_by TEXT);
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL CHECK (default_tier IN ('low','mid','high')), default_temperature TEXT NOT NULL CHECK (default_temperature IN ('freezed','cold','warm','hot','hell')), max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL CHECK (field_type IN ('string','number','boolean','enum','array_of_string','array_of_object')), enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );",
        ).unwrap();
        c
    }

    fn a_card(c: &Connection, slug: &str, title: &str) {
        c.execute("INSERT INTO cards (slug, title, status) VALUES (?, ?, 'done')", [slug, title]).unwrap();
    }

    #[test]
    fn missing_card_slug_errors() {
        let _g = ENV_LOCK.lock().unwrap();
        let c = setup();
        let e = keeperCardConsolidate(&json!({}), &c).unwrap_err();
        assert_eq!(e, "keeper-card-consolidate: card_slug is required");
    }

    #[test]
    fn unknown_card_errors_not_found() {
        let _g = ENV_LOCK.lock().unwrap();
        let c = setup();
        let e = keeperCardConsolidate(&json!({ "card_slug": "NOPE" }), &c).unwrap_err();
        assert_eq!(e, "keeper-card-consolidate: card 'NOPE' not found");
    }

    #[test]
    fn stub_mode_inserts_stub_memory_and_fires_phase_event() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("ANTILOKI_KEEPER_CONSOLIDATE_STUB", "1");
        let c = setup();
        a_card(&c, "CARD-1", "Do the thing");
        let r = keeperCardConsolidate(&json!({ "card_slug": "CARD-1" }), &c);
        std::env::remove_var("ANTILOKI_KEEPER_CONSOLIDATE_STUB");
        let r = r.unwrap();
        assert_eq!(r["verdict"], json!("unknown"));
        assert_eq!(r["would_repeat"], json!(true));
        assert_eq!(r["lessons"], json!(["stub-lesson"]));
        let title: String = c.query_row("SELECT title FROM memory.memories WHERE id = 1", [], |row| row.get(0)).unwrap();
        assert_eq!(title, "card-reflection: CARD-1");
        let phase: String = c.query_row("SELECT phase FROM card_phase_events WHERE card_slug = 'CARD-1'", [], |row| row.get(0)).unwrap();
        assert_eq!(phase, "keeper_consolidated");
    }

    #[test]
    fn degraded_path_never_wedges_the_card_and_writes_a_deterministic_memory() {
        let _g = ENV_LOCK.lock().unwrap();
        let c = setup();
        a_card(&c, "CARD-2", "Ship the feature");
        c.execute(
            "INSERT INTO card_phase_events (card_slug, phase, actor, evidence_json) VALUES ('CARD-2', 'man_reviewed', 'man:auto', ?)",
            [r#"{"verdict":"pass","findings":[]}"#],
        ).unwrap();
        let r = keeperCardConsolidate(&json!({ "card_slug": "CARD-2" }), &c).unwrap();
        assert_eq!(r["verdict"], json!("pass"));
        assert_eq!(r["degraded"], json!(true));
        assert_eq!(r["would_repeat"], json!(true)); // pass ⇒ default true, unset by degraded path
        assert!(r["reflection_md"].as_str().unwrap().contains("degraded deterministic record"));
        let mem_id = r["memory_id"].as_i64().unwrap();
        let (guidance, situation): (String, String) = c.query_row(
            "SELECT guidance, situation FROM memory.memories WHERE id = ?", [mem_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert!(guidance.contains("degraded deterministic record"));
        assert!(situation.contains("[degraded]"));
        let evidence: String = c.query_row("SELECT evidence_json FROM card_phase_events WHERE card_slug = 'CARD-2' AND phase = 'keeper_consolidated'", [], |row| row.get(0)).unwrap();
        let ev: Value = serde_json::from_str(&evidence).unwrap();
        assert_eq!(ev["degraded"], json!(true));
        assert_eq!(ev["memory_id"], json!(mem_id));
    }

    #[test]
    fn fail_verdict_defaults_would_repeat_false_on_degraded_path() {
        let _g = ENV_LOCK.lock().unwrap();
        let c = setup();
        a_card(&c, "CARD-3", "Broken thing");
        c.execute(
            "INSERT INTO card_phase_events (card_slug, phase, actor, evidence_json) VALUES ('CARD-3', 'man_reviewed', 'man:auto', ?)",
            [r#"{"verdict":"fail","findings":[{"severity":"high","kind":"bug","ref":"file.rs:12","snippet":"panic"}]}"#],
        ).unwrap();
        let r = keeperCardConsolidate(&json!({ "card_slug": "CARD-3" }), &c).unwrap();
        assert_eq!(r["would_repeat"], json!(false));
    }

    #[test]
    fn facts_aggregate_timeline_errors_cost_and_qa_retries() {
        let _g = ENV_LOCK.lock().unwrap();
        let c = setup();
        a_card(&c, "CARD-4", "Metrics card");
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id) VALUES ('card','CARD-4')", []).unwrap();
        c.execute("INSERT INTO sacred_timeline (entity_type, entity_id) VALUES ('card','CARD-4')", []).unwrap();
        c.execute("INSERT INTO pl_sessions (id, approved_card_slug) VALUES (1, 'CARD-4')", []).unwrap();
        c.execute("INSERT INTO runtime_events (id, user_goal_id, outcome) VALUES (10, 1, 'error')", []).unwrap();
        c.execute("INSERT INTO runtime_events (id, user_goal_id, outcome) VALUES (11, 1, 'success')", []).unwrap();
        c.execute("INSERT INTO brain.ai_calls (parent_event_id, cost_usd_actual, duration_ms) VALUES (10, 0.5, 2000)", []).unwrap();
        c.execute("INSERT INTO contracts (slug, card_slug) VALUES ('C-1', 'CARD-4')", []).unwrap();
        c.execute("INSERT INTO qa_runs (contract_slug) VALUES ('C-1')", []).unwrap();
        c.execute("INSERT INTO qa_runs (contract_slug) VALUES ('C-1')", []).unwrap();
        c.execute("INSERT INTO qa_runs (contract_slug) VALUES ('C-1')", []).unwrap();

        let facts = load_card_facts(&c, &json!("CARD-4")).unwrap().unwrap();
        assert_eq!(facts.timeline_count, 2);
        assert_eq!(facts.error_count, 1);
        assert_eq!(facts.cost_usd, 0.5);
        assert_eq!(facts.duration_sec, 2);
        assert_eq!(facts.qa_retry_count, 2); // max(0, 3 - 1)
    }

    #[test]
    fn qa_retry_count_floors_at_zero_with_no_qa_runs() {
        let _g = ENV_LOCK.lock().unwrap();
        let c = setup();
        a_card(&c, "CARD-5", "No QA yet");
        let facts = load_card_facts(&c, &json!("CARD-5")).unwrap().unwrap();
        assert_eq!(facts.qa_retry_count, 0); // max(0, 1 - 1) — COUNT(*) with no rows is 0, but JS ?? 1 default only fires on undefined, count is always a number here
    }

    #[test]
    fn findings_block_formats_lines_and_defaults_when_empty() {
        assert_eq!(findings_block(&json!({})), "(no findings)");
        assert_eq!(findings_block(&json!({ "findings": [] })), "(no findings)");
        let ev = json!({ "findings": [{ "severity": "high", "kind": "bug", "ref": "f.rs:1", "snippet": "boom" }] });
        assert_eq!(findings_block(&ev), "- high: bug (f.rs:1): boom");
    }

    #[test]
    fn aggregate_ai_output_reconstructs_array_and_scalar_fields_in_order() {
        let res = json!({
            "output_kv": [
                { "field_name": "reflection_md", "value_text": "It went fine." },
                { "field_name": "lessons", "value_text": "second lesson", "order_index": 1 },
                { "field_name": "lessons", "value_text": "first lesson", "order_index": 0 },
                { "field_name": "would_repeat", "value_boolean": true },
            ]
        });
        let (reflection_md, lessons, would_repeat) = aggregate_ai_output(&res);
        assert_eq!(reflection_md, "It went fine.");
        assert_eq!(lessons, vec!["first lesson".to_string(), "second lesson".to_string()]);
        assert!(would_repeat);
    }

    #[test]
    fn aggregate_ai_output_defaults_on_empty_output_kv() {
        let (reflection_md, lessons, would_repeat) = aggregate_ai_output(&json!({}));
        assert_eq!(reflection_md, "");
        assert!(lessons.is_empty());
        assert!(!would_repeat);
    }
}
