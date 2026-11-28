//! pl-session-advance-decompose.rs — twin of skills/agents/pl/pl-session-advance-decompose.mjs
//! (db WRITE lane). NAMES ARE CONTRACT: `plSessionAdvanceDecompose` (default). Runs the high+warm
//! AI decision on whether a `deciding_split` PL session is atomic (single card) or should split
//! into ≤`max_children` child pl_sessions (each XS/S-sized), then advances the session to
//! `tuning` regardless.
//!
//! PARITY-DEFERRED: the AI leg is `crate::ai_ai_call::aiCall`, itself an always-`Err` stub (see
//! ai-call.rs's module doc) — it dispatches a real network call to a BYOK LLM provider, which is
//! genuinely non-deterministic and not fixturable. Everything ELSE is ported faithfully and IS
//! the deterministic surface this file's tests exercise: the session lookup + status guard, the
//! idempotency guard (an existing pl_decompositions row), `ensurePrompt`'s find-or-register
//! bootstrap (reusing the already-converted `ai_prompt_find`/`ai_prompt_add` twins), and the
//! research-findings compaction (`findings_compact`, its own private fn, unit-tested directly
//! since aiCall's Err short-circuits the real function before that string is ever consumed).
//! The post-aiCall shaping (spent_usd bookkeeping, is_atomic/reasoning_md extraction, the
//! pl_decompositions/pl_decomposition_children inserts via the already-converted
//! `agents_pl_pl_session_start::plSessionStart` twin, and the final status→'tuning' update) is
//! ported byte-for-byte from the JS but — like book-run.rs's post-wave staging — is unreachable by
//! this file's own fixture because it runs only after a successful `aiCall`, which the deferred
//! stub never produces; correctness there is by inspection + the same shape as the reachable code.
//!
//! `context.db` missing → JS throws `err.contextMissing('pl-session-advance-decompose', 'db')`.
//! Unreachable here: `db: &Connection` is a required, non-nullable reference (same convention as
//! every sibling db-lane twin). `provider_fn` (JS `input.provider_fn`) has no Rust equivalent — it
//! is forwarded into the deferred `aiCall` input the same way the JS forwards it, but is otherwise
//! inert (the stub ignores its whole input).
// QUALITY: mixed unit — guards/idempotency/prompt-bootstrap/findings-compaction are real,
// fixture-tested surface; the post-aiCall decomposition-persist path is ported but unreachable
// (deferred AI boundary short-circuits it) — same convention as book-run.rs.

use rusqlite::Connection;
use serde_json::{json, Map, Value};

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
}
fn nz(v: Option<&Value>) -> Option<&Value> { v.filter(|x| !x.is_null()) }

const PROMPT_NAME: &str = "pl.decompose.decision.v2";
const PROMPT_TEMPLATE_MD: &str = "You are PL deciding whether a session's intent is atomic (single card) or should split into children.\n\n\u{2550}\u{2550}\u{2550} session raw_intent \u{2550}\u{2550}\u{2550}\n{{raw_intent}}\n\u{2550}\u{2550}\u{2550} end raw_intent \u{2550}\u{2550}\u{2550}\n\nResearch findings (may be empty):\n{{research_findings}}\n\nAtomic if: the work has ONE clear acceptance criterion + fits in <1 day + no orthogonal subtasks.\nSplit if: multiple acceptance criteria, separate concerns, or work exceeds ~1 day.\n\nHARD CONSTRAINT: every emitted child MUST have size_estimate \u{2208} {XS, S}.\nM or L children mean the decomposition is incomplete \u{2014} split them further\ninto ≤-S pieces. If you cannot shrink a child below S (the work is\ngenuinely one cohesive piece > S), prefer is_atomic=true and explain in\nreasoning_md. The downstream pipeline relies on this size cap to route\neach leaf cleanly through DEV.\n\nMax 5 children. Each child needs its own minimal intent_md.\n\nReturn JSON:\n{\n  \"is_atomic\": <bool>,\n  \"reasoning_md\": \"<2-4 sentences; if some child would be > S, say so here>\",\n  \"children\": [\n    { \"title\": \"<short>\", \"intent_md\": \"<concise>\", \"size_estimate\": \"<XS or S only>\", \"why_separate_md\": \"<1 sentence>\" }\n  ]\n}\n\nIf is_atomic=true, children MAY be empty.";

/// `function ensurePrompt(db)` — internal (not exported). Find-or-register the decompose prompt
/// template. Mirrors the JS `try { return promptFind(...) } catch { promptAdd(...); return
/// promptFind(...) }`.
fn ensure_prompt(db: &Connection) -> Result<Value, String> {
    if let Ok(v) = crate::ai_prompt_find::promptFind(&json!(PROMPT_NAME), db) {
        return Ok(v);
    }
    crate::ai_prompt_add::promptAdd(
        &json!({
            "name": PROMPT_NAME,
            "template_md": PROMPT_TEMPLATE_MD,
            "default_tier": "high",
            "default_temperature": "warm",
            "max_output_tokens": 2048,
            "output_fields": [
                { "field_name": "is_atomic", "field_type": "boolean" },
                { "field_name": "reasoning_md", "field_type": "string" },
                { "field_name": "children", "field_type": "array_of_object" },
            ],
        }),
        db,
    )?;
    crate::ai_prompt_find::promptFind(&json!(PROMPT_NAME), db)
}

/// `findingsCompact` assembly — internal (inline in the JS body, extracted here for direct
/// testing). `[${finding_type}] ${target_ref}: ${why_relevant_md ?? ''}` joined by `\n`, then
/// `.slice(0, 1500)` (JS UTF-16 code units — `.chars().take(1500)` for this file's ASCII/BMP
/// fixtures, per the playbook's gotcha #5).
fn findings_compact(rows: &Value) -> String {
    let lines: Vec<String> = rows
        .as_array()
        .map(|a| {
            a.iter()
                .map(|f| {
                    format!(
                        "[{}] {}: {}",
                        str_qq(f.get("finding_type")),
                        str_qq(f.get("target_ref")),
                        str_qq(nz(f.get("why_relevant_md")))
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    lines.join("\n").chars().take(1500).collect()
}

#[allow(non_snake_case)]
pub fn plSessionAdvanceDecompose(input: &Value, db: &Connection) -> Result<Value, String> {
    let session_id = input.get("session_id").filter(|v| !v.is_null() && truthy(Some(v))).cloned();
    let Some(session_id) = session_id else {
        return Err("pl-session-advance-decompose: session_id is required".to_string());
    };
    let provider_fn = input.get("provider_fn").cloned().unwrap_or(Value::Null);
    let max_children = input.get("max_children").and_then(Value::as_i64).unwrap_or(5);

    let rows = crate::db_json::query_rows(db, "SELECT * FROM pl_sessions WHERE id = ?", &[session_id.clone()])?;
    let Some(session) = rows.as_array().and_then(|a| a.first()).cloned() else {
        return Err(format!("pl-session-advance-decompose: session {} not found", str_qq(Some(&session_id))));
    };
    let status = session.get("status").and_then(Value::as_str).unwrap_or("");
    if status != "deciding_split" {
        return Err(format!(
            "pl-session-advance-decompose: session status='{}', expected 'deciding_split'",
            status
        ));
    }

    let existing_rows = crate::db_json::query_rows(
        db,
        "SELECT id FROM pl_decompositions WHERE session_id = ?",
        &[session_id.clone()],
    )?;
    if let Some(existing) = existing_rows.as_array().and_then(|a| a.first()) {
        let _ = existing; // decomposition_id only used in the (unreachable-on-this-path) error context in JS
        return Err(format!(
            "pl-session-advance-decompose: decomposition for session {} already exists",
            str_qq(Some(&session_id))
        ));
    }

    ensure_prompt(db)?;

    let findings_rows = crate::db_json::query_rows(
        db,
        "SELECT target_ref, finding_type, why_relevant_md FROM pl_research_findings WHERE session_id = ? ORDER BY order_index",
        &[session_id.clone()],
    )?;
    let findings_compact_s = findings_compact(&findings_rows);

    let budget_usd = session.get("budget_usd").and_then(Value::as_f64).unwrap_or(0.0);
    let spent_usd = session.get("spent_usd").and_then(Value::as_f64).unwrap_or(0.0);

    let ai_input = json!({
        "prompt_name": PROMPT_NAME,
        "input_kv": [
            { "key": "raw_intent", "value": session.get("raw_intent").cloned().unwrap_or(Value::Null) },
            { "key": "research_findings", "value": findings_compact_s },
        ],
        "budget_remaining_usd": budget_usd - spent_usd,
        "provider_fn": provider_fn,
    });

    // ── PARITY-DEFERRED boundary ─────────────────────────────────────────
    // aiCall is a real network call to a BYOK LLM provider; the twin's stub always returns Err,
    // so everything below is unreachable by this file's fixture but ported by inspection for
    // shape-correctness (mirrors book-run.rs's convention).
    let ai_result = crate::ai_ai_call::aiCall(&ai_input, db)?;

    let cost_usd = ai_result.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
    crate::db_json::run_write(
        db,
        &format!("UPDATE pl_sessions SET spent_usd = spent_usd + ?, updated_at={} WHERE id = ?", crate::clock::now_sql()),
        &[json!(cost_usd), session_id.clone()],
    )?;

    let output_kv = ai_result.get("output_kv").and_then(Value::as_array).cloned().unwrap_or_default();
    let is_atomic = output_kv
        .iter()
        .find(|o| o.get("field_name").and_then(Value::as_str) == Some("is_atomic"))
        .map(|o| truthy(o.get("value_boolean")))
        .unwrap_or(true);
    let reasoning_md = output_kv
        .iter()
        .find(|o| o.get("field_name").and_then(Value::as_str) == Some("reasoning_md"))
        .and_then(|o| o.get("value_text").and_then(Value::as_str))
        .unwrap_or("(no reasoning)")
        .to_string();

    let ai_call_id = ai_result.get("ai_call_id").cloned().unwrap_or(Value::Null);
    let (_changes, decomposition_id) = crate::db_json::run_write(
        db,
        "INSERT INTO pl_decompositions (session_id, is_atomic, reasoning_md, ai_call_id) VALUES (?, ?, ?, ?)",
        &[session_id.clone(), json!(if is_atomic { 1 } else { 0 }), json!(reasoning_md.clone()), ai_call_id.clone()],
    )?;

    let mut children_spawned: Vec<Value> = Vec::new();
    if !is_atomic {
        // JS: `output_kv.filter(o => o.field_name === 'children' && o.value_text == null && o.value_number == null)`
        // then sorted by order_index, sliced to max_children. Each parent's own children (title,
        // intent_md, size_estimate, why_separate_md) live in brain.ai_call_outputs keyed by
        // parent_output_id — an already-persisted nested-row shape from validateAndDecompose's
        // array_of_object branch (ai-call.rs).
        let mut parents: Vec<&Value> = output_kv
            .iter()
            .filter(|o| {
                o.get("field_name").and_then(Value::as_str) == Some("children")
                    && o.get("value_text").map_or(true, Value::is_null)
                    && o.get("value_number").map_or(true, Value::is_null)
            })
            .collect();
        parents.sort_by(|a, b| {
            let ai = a.get("order_index").and_then(Value::as_i64).unwrap_or(0);
            let bi = b.get("order_index").and_then(Value::as_i64).unwrap_or(0);
            ai.cmp(&bi)
        });
        let started_by = session.get("started_by").cloned().unwrap_or(Value::Null);

        let mut idx: i64 = 0;
        for p in parents.into_iter().take(max_children.max(0) as usize) {
            let child_rows = crate::db_json::query_rows(
                db,
                "SELECT field_name, value_text FROM brain.ai_call_outputs WHERE ai_call_id = ? AND parent_output_id = ?",
                &[ai_call_id.clone(), p.get("id").cloned().unwrap_or(Value::Null)],
            )?;
            let children: Vec<Value> = child_rows.as_array().cloned().unwrap_or_default();
            let find_field = |name: &str| -> Option<String> {
                children
                    .iter()
                    .find(|c| c.get("field_name").and_then(Value::as_str) == Some(name))
                    .and_then(|c| c.get("value_text").and_then(Value::as_str))
                    .map(|s| s.to_string())
            };
            let title = find_field("title");
            let intent_md = find_field("intent_md");
            let (Some(title), Some(intent_md)) = (title, intent_md) else { continue };
            let size_estimate = find_field("size_estimate").filter(|s| !s.is_empty()).unwrap_or_else(|| "S".to_string());
            let why_separate_md = find_field("why_separate_md");

            if size_estimate != "XS" && size_estimate != "S" { continue; }

            let child_session_id = match crate::agents_pl_pl_session_start::plSessionStart(
                &json!({
                    "raw_intent": intent_md,
                    "started_by": started_by,
                    "budget_usd": budget_usd,
                    "parent_session_id": session_id,
                }),
                db,
            ) {
                Ok(r) => r.get("session_id").cloned().unwrap_or(Value::Null),
                Err(_) => Value::Null,
            };

            let (_c, child_id) = crate::db_json::run_write(
                db,
                "INSERT INTO pl_decomposition_children (decomposition_id, title, intent_md, size_estimate, why_separate_md, order_index, child_session_id) VALUES (?, ?, ?, ?, ?, ?, ?)",
                &[
                    json!(decomposition_id),
                    json!(title.clone()),
                    json!(intent_md),
                    json!(size_estimate),
                    why_separate_md.map(|s| json!(s)).unwrap_or(Value::Null),
                    json!(idx),
                    child_session_id.clone(),
                ],
            )?;
            idx += 1;

            let mut ch = Map::new();
            ch.insert("id".into(), json!(child_id));
            ch.insert("title".into(), json!(title));
            ch.insert("child_session_id".into(), child_session_id);
            children_spawned.push(Value::Object(ch));
        }
    }

    crate::db_json::run_write(
        db,
        &format!("UPDATE pl_sessions SET status='tuning', updated_at={} WHERE id = ?", crate::clock::now_sql()),
        &[session_id.clone()],
    )?;

    let mut out = Map::new();
    out.insert("session_id".into(), session_id);
    out.insert("ai_call_id".into(), ai_call_id);
    out.insert("decomposition_id".into(), json!(decomposition_id));
    out.insert("is_atomic".into(), json!(is_atomic));
    out.insert("reasoning_md".into(), json!(reasoning_md));
    out.insert("children_spawned".into(), json!(children_spawned.len() as i64));
    out.insert("children".into(), json!(children_spawned));
    out.insert("new_status".into(), json!("tuning"));
    out.insert("cached".into(), ai_result.get("cached").cloned().unwrap_or(Value::Null));
    out.insert("cost_usd".into(), json!(cost_usd));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db0() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("ATTACH ':memory:' AS brain").unwrap();
        c.execute_batch(
            "CREATE TABLE pl_sessions ( id INTEGER PRIMARY KEY, raw_intent TEXT, status TEXT, started_by TEXT, budget_usd REAL, spent_usd REAL NOT NULL DEFAULT 0, parent_session_id INTEGER, approved_card_slug TEXT, updated_at TEXT );
             CREATE TABLE pl_decompositions ( id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL, is_atomic INTEGER, reasoning_md TEXT, ai_call_id INTEGER );
             CREATE TABLE pl_decomposition_children ( id INTEGER PRIMARY KEY, decomposition_id INTEGER NOT NULL, title TEXT, intent_md TEXT, size_estimate TEXT, why_separate_md TEXT, order_index INTEGER, child_session_id INTEGER );
             CREATE TABLE pl_research_findings ( id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL, target_ref TEXT, finding_type TEXT, why_relevant_md TEXT, order_index INTEGER );
             CREATE TABLE cards ( slug TEXT PRIMARY KEY, status TEXT, updated_at TEXT );
             CREATE TABLE brain.prompt_templates ( id INTEGER PRIMARY KEY, name TEXT NOT NULL, version INTEGER NOT NULL, template_md TEXT NOT NULL, default_tier TEXT NOT NULL, default_temperature TEXT NOT NULL, max_output_tokens INTEGER NOT NULL DEFAULT 1024, superseded_by INTEGER, created_at TEXT NOT NULL DEFAULT (datetime('2026-01-01 00:00:00')), cross_field_rules_json TEXT, judging_criteria_json TEXT, system_md TEXT, phase_flow TEXT NOT NULL DEFAULT 'single', UNIQUE (name, version) );
             CREATE TABLE brain.prompt_output_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, grounded_refs_json TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.prompt_input_fields ( id INTEGER PRIMARY KEY, prompt_template_id INTEGER NOT NULL, field_name TEXT NOT NULL, field_type TEXT NOT NULL, enum_values TEXT, required INTEGER NOT NULL DEFAULT 1, description TEXT, order_index INTEGER NOT NULL, min_length INTEGER, max_length INTEGER, required_sections_json TEXT, pattern TEXT, UNIQUE (prompt_template_id, field_name) );
             CREATE TABLE brain.ai_call_outputs ( id INTEGER PRIMARY KEY, ai_call_id INTEGER, field_name TEXT, value_text TEXT, value_number REAL, value_boolean INTEGER, order_index INTEGER, parent_output_id INTEGER );",
        ).unwrap();
        c
    }

    fn seed_session(c: &Connection, id: i64, status: &str) {
        c.execute(
            "INSERT INTO pl_sessions (id, raw_intent, status, started_by, budget_usd, spent_usd, updated_at) VALUES (?, 'do the thing', ?, 'human:cli', 0.5, 0.1, '2025-12-01 00:00:00')",
            rusqlite::params![id, status],
        ).unwrap();
    }

    #[test]
    fn missing_session_id_errors() {
        let c = db0();
        let e = plSessionAdvanceDecompose(&json!({}), &c).unwrap_err();
        assert_eq!(e, "pl-session-advance-decompose: session_id is required");
    }

    #[test]
    fn unknown_session_id_errors_not_found() {
        let c = db0();
        let e = plSessionAdvanceDecompose(&json!({ "session_id": 999 }), &c).unwrap_err();
        assert_eq!(e, "pl-session-advance-decompose: session 999 not found");
    }

    #[test]
    fn wrong_status_errors_validation_logic() {
        let c = db0();
        seed_session(&c, 1, "clarifying");
        let e = plSessionAdvanceDecompose(&json!({ "session_id": 1 }), &c).unwrap_err();
        assert_eq!(e, "pl-session-advance-decompose: session status='clarifying', expected 'deciding_split'");
    }

    #[test]
    fn already_decomposed_errors_already_exists() {
        let c = db0();
        seed_session(&c, 1, "deciding_split");
        c.execute("INSERT INTO pl_decompositions (id, session_id, is_atomic, reasoning_md) VALUES (1, 1, 1, 'x')", []).unwrap();
        let e = plSessionAdvanceDecompose(&json!({ "session_id": 1 }), &c).unwrap_err();
        assert_eq!(e, "pl-session-advance-decompose: decomposition for session 1 already exists");
    }

    #[test]
    fn guards_pass_then_hits_the_deferred_ai_boundary() {
        // Everything up to (and including bootstrapping the prompt template + compacting
        // findings) runs for real; the function then hits the deferred aiCall stub and
        // propagates its Err — this IS the expected outcome on this twin (see module doc).
        let c = db0();
        seed_session(&c, 1, "deciding_split");
        c.execute(
            "INSERT INTO pl_research_findings (session_id, target_ref, finding_type, why_relevant_md, order_index) VALUES (1, 'src/foo.mjs', 'file', 'has the auth logic', 0)",
            [],
        ).unwrap();

        let e = plSessionAdvanceDecompose(&json!({ "session_id": 1 }), &c).unwrap_err();
        assert!(e.contains("ai-call"), "{e}");

        // ensurePrompt registered the template as a side effect before the deferred call.
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'pl.decompose.decision.v2'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        // no decomposition/status-change side effects happened past the deferred boundary.
        let dn: i64 = c.query_row("SELECT COUNT(*) FROM pl_decompositions", [], |r| r.get(0)).unwrap();
        assert_eq!(dn, 0);
        let status: String = c.query_row("SELECT status FROM pl_sessions WHERE id=1", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "deciding_split");
    }

    #[test]
    fn ensure_prompt_is_idempotent_on_second_call() {
        let c = db0();
        ensure_prompt(&c).unwrap();
        ensure_prompt(&c).unwrap();
        let n: i64 = c.query_row("SELECT COUNT(*) FROM brain.prompt_templates WHERE name = 'pl.decompose.decision.v2'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn findings_compact_formats_and_truncates() {
        let rows = json!([
            { "target_ref": "a.mjs", "finding_type": "file", "why_relevant_md": "does x" },
            { "target_ref": "b.mjs", "finding_type": "file", "why_relevant_md": null },
        ]);
        assert_eq!(findings_compact(&rows), "[file] a.mjs: does x\n[file] b.mjs: ");
        assert_eq!(findings_compact(&json!([])), "");

        let long_row = json!([{ "target_ref": "x", "finding_type": "y", "why_relevant_md": "z".repeat(2000) }]);
        assert_eq!(findings_compact(&long_row).chars().count(), 1500);
    }
}
