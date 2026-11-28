//! heimdall-attribute.rs — twin of skills/heimdall/heimdall-attribute.mjs (db WRITE lane,
//! async-over-sync, cross-db: brain + judgments; unqualified `contracts`/`cards` are main-db).
//! NAMES ARE CONTRACT: `heimdallAttribute` (default). Turns recorded outcomes into HEIMDALL
//! EVIDENCE rows (plan §3.2). Idempotent (unique index (ai_call_id, provenance, outcome) +
//! INSERT OR IGNORE — re-running attributes nothing twice). Three levels:
//!   CALL level (provenance 'prod_gate'): every real dispatch (cached=0) with an outcome_class
//!   becomes clean→'clean' / degenerate|truncated|validation_failed→'fail'. provider_error is
//!   excluded on purpose (transport failures are failover territory, not cognition).
//!   TERMINAL level (provenance 'prod_terminal'): calls on the envelope chain of a SHIPPED card
//!   (cards.status='done', joined via ai_calls.contract_slug → contracts.card_slug) earn 'ship'.
//!   STAGE level (provenance 'prod_court', outcome 'burn'): a COURT-failed blueprint debits its
//!   authoring calls — discriminated from gate REDs by goal_line shape (gate verdicts write
//!   parenthesized goal-lines, court verdicts never do); authoring calls join via
//!   ai_calls.contract_slug + the scope's authoring template set, with ts <= the judgment's
//!   round stamp.
// QUALITY: 81/100 — three-level idempotent evidence attribution over a shared memoized cat_for lookup
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;

// Standard helper functions (playbook §Standard helper functions) — copied verbatim (subset used).
fn str_qq(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(n @ Value::Number(_)) => n.to_string(),
        Some(o) => o.to_string(),
    }
}

const CALL_FAIL: [&str; 3] = ["degenerate", "truncated", "validation_failed"];
const DESIGN_AUTHORS: [&str; 2] = ["blueprint.skeleton", "blueprint.stub"];
const CODE_AUTHORS: [&str; 3] = ["blueprint.snippet.author", "tl.author.file", "dev.implement.code"];

// JS: `const cache = new Map(); (base) => { if (!cache.has(base)) cache.set(base,
// mapStmt.get(base)?.category ?? 'general'); return cache.get(base); }` — memoized per base.
fn cat_for(db: &Connection, cache: &mut HashMap<String, Value>, base: &str) -> Value {
    if let Some(v) = cache.get(base) {
        return v.clone();
    }
    let rows = crate::db_json::query_rows(
        db,
        "SELECT category FROM brain.heimdall_template_categories WHERE template_base = ?",
        &[json!(base)],
    )
    .unwrap_or_else(|_| json!([]));
    let cat = rows
        .as_array()
        .and_then(|a| a.first())
        .and_then(|r| r.get("category"))
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| json!("general"));
    cache.insert(base.to_string(), cat.clone());
    cat
}

// `ins.run(model_slug, category, template_base, provenance, outcome, ai_call_id).changes` —
// INSERT OR IGNORE against the unique (ai_call_id, provenance, outcome) index; rusqlite's
// `execute` already returns the row-changed count, same as node:sqlite's `.changes`.
fn ins_evidence(
    db: &Connection,
    model_slug: &Value,
    category: &Value,
    template_base: &Value,
    provenance: &str,
    outcome: &str,
    ai_call_id: &Value,
) -> i64 {
    crate::db_json::run_write(
        db,
        "INSERT OR IGNORE INTO brain.heimdall_evidence
          (model_slug, category, template_base, provenance, outcome, ai_call_id)
        VALUES (?, ?, ?, ?, ?, ?)",
        &[
            model_slug.clone(),
            category.clone(),
            template_base.clone(),
            json!(provenance),
            json!(outcome),
            ai_call_id.clone(),
        ],
    )
    .map(|(changes, _)| changes as i64)
    .unwrap_or(0)
}

// TERMINAL level — isolated behind `?` so a query failure (e.g. contract_slug column absent on
// a legacy brain) mirrors the JS try/catch: `terminal_rows` keeps whatever it already accrued
// (nothing, since this is the block's only query) and the rest of the skill still runs.
fn run_terminal_level(db: &Connection, cache: &mut HashMap<String, Value>, terminal_rows: &mut i64) -> Result<(), String> {
    let shipped = crate::db_json::query_rows(
        db,
        "SELECT a.id, a.model_id, pt.name AS template_name
         FROM brain.ai_calls a
         JOIN brain.prompt_templates pt ON pt.id = a.prompt_template_id
         JOIN contracts ct ON ct.slug = a.contract_slug
         JOIN cards cd ON cd.slug = ct.card_slug
         WHERE a.cached = 0 AND a.model_id IS NOT NULL AND a.outcome = 'success'
           AND cd.status = 'done'",
        &[],
    )?;
    for c in shipped.as_array().cloned().unwrap_or_default() {
        let template_name = c.get("template_name").cloned().unwrap_or(Value::Null);
        let base = crate::heimdall__helpers_casting::templateBase(&template_name);
        let base_str = base.as_str().unwrap_or("").to_string();
        let category = cat_for(db, cache, &base_str);
        let model_id = c.get("model_id").cloned().unwrap_or(Value::Null);
        let ai_call_id = c.get("id").cloned().unwrap_or(Value::Null);
        *terminal_rows += ins_evidence(db, &model_id, &category, &base, "prod_terminal", "ship", &ai_call_id);
    }
    Ok(())
}

// STAGE level (burns) — same `?`-behind-a-mut-accumulator shape: a failure at ANY query (the
// judgment_cases select, a per-judgment contracts lookup, or a per-judgment calls select) stops
// the block right there via early return, but `burn_rows` already reflects everything inserted
// before that point — matching the JS try/catch wrapping the whole loop, which loses no partial
// progress made before the throw.
fn run_stage_level(db: &Connection, cache: &mut HashMap<String, Value>, burn_rows: &mut i64) -> Result<(), String> {
    let failed = crate::db_json::query_rows(
        db,
        "SELECT card_slug, scope_ref, updated_at
         FROM judgments.judgment_cases
         WHERE scope_kind = 'blueprint' AND verdict = 'fail' AND goal_line NOT LIKE '(%'",
        &[],
    )?;
    for j in failed.as_array().cloned().unwrap_or_default() {
        let card_slug = j.get("card_slug").cloned().unwrap_or(Value::Null);
        let contracts_rows = crate::db_json::query_rows(db, "SELECT slug FROM contracts WHERE card_slug = ?", &[card_slug])?;
        let contract_slugs: Vec<Value> = contracts_rows
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|r| r.get("slug").cloned().unwrap_or(Value::Null))
            .collect();
        if contract_slugs.is_empty() {
            continue;
        }
        let scope_ref = j.get("scope_ref").cloned().unwrap_or(Value::Null);
        let bases: &[&str] = if str_qq(Some(&scope_ref)).starts_with("design-") { &DESIGN_AUTHORS } else { &CODE_AUTHORS };

        let placeholders = vec!["?"; contract_slugs.len()].join(",");
        let sql = format!(
            "SELECT a.id, a.model_id, pt.name AS template_name
             FROM brain.ai_calls a JOIN brain.prompt_templates pt ON pt.id = a.prompt_template_id
             WHERE a.cached = 0 AND a.model_id IS NOT NULL
               AND a.contract_slug IN ({placeholders})
               AND a.ts <= ?"
        );
        let mut params = contract_slugs;
        params.push(j.get("updated_at").cloned().unwrap_or(Value::Null));
        let calls = crate::db_json::query_rows(db, &sql, &params)?;
        for c in calls.as_array().cloned().unwrap_or_default() {
            let template_name = c.get("template_name").cloned().unwrap_or(Value::Null);
            let base = crate::heimdall__helpers_casting::templateBase(&template_name);
            let base_str = base.as_str().unwrap_or("").to_string();
            if !bases.contains(&base_str.as_str()) {
                continue;
            }
            let category = cat_for(db, cache, &base_str);
            let model_id = c.get("model_id").cloned().unwrap_or(Value::Null);
            let ai_call_id = c.get("id").cloned().unwrap_or(Value::Null);
            *burn_rows += ins_evidence(db, &model_id, &category, &base, "prod_court", "burn", &ai_call_id);
        }
    }
    Ok(())
}

pub fn heimdallAttribute(_input: &Value, db: &Connection) -> Result<Value, String> {
    // JS: `const { db } = context ?? {}; if (!db) throw err.contextMissing('heimdall-attribute',
    // 'db')` — the db lane always injects a live connection, so this guard is unreachable here.
    // `input` is unused: JS doc says "input: {} (idempotent full pass)".

    let mut cache: HashMap<String, Value> = HashMap::new();

    // CALL level — no try/catch in JS (ai_calls/prompt_templates are always present), so a query
    // failure here propagates via `?` exactly like the JS throw would.
    let mut call_rows: i64 = 0;
    let calls = crate::db_json::query_rows(
        db,
        "SELECT a.id, a.model_id, a.outcome_class, pt.name AS template_name
         FROM brain.ai_calls a JOIN brain.prompt_templates pt ON pt.id = a.prompt_template_id
         WHERE a.cached = 0 AND a.model_id IS NOT NULL AND a.outcome_class IS NOT NULL
           AND a.outcome_class IN ('clean','degenerate','truncated','validation_failed')",
        &[],
    )?;
    for c in calls.as_array().cloned().unwrap_or_default() {
        let template_name = c.get("template_name").cloned().unwrap_or(Value::Null);
        let base = crate::heimdall__helpers_casting::templateBase(&template_name);
        let base_str = base.as_str().unwrap_or("").to_string();
        let outcome_class = c.get("outcome_class").and_then(|v| v.as_str()).unwrap_or("");
        let outcome = if CALL_FAIL.contains(&outcome_class) { "fail" } else { "clean" };
        let category = cat_for(db, &mut cache, &base_str);
        let model_id = c.get("model_id").cloned().unwrap_or(Value::Null);
        let ai_call_id = c.get("id").cloned().unwrap_or(Value::Null);
        call_rows += ins_evidence(db, &model_id, &category, &base, "prod_gate", outcome, &ai_call_id);
    }

    let mut terminal_rows: i64 = 0;
    let _ = run_terminal_level(db, &mut cache, &mut terminal_rows);

    let mut burn_rows: i64 = 0;
    let _ = run_stage_level(db, &mut cache, &mut burn_rows);

    Ok(json!({ "ok": true, "call_rows": call_rows, "terminal_rows": terminal_rows, "burn_rows": burn_rows }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "ATTACH ':memory:' AS brain;
             ATTACH ':memory:' AS judgments;
             CREATE TABLE cards (slug TEXT PRIMARY KEY, status TEXT NOT NULL);
             CREATE TABLE contracts (slug TEXT PRIMARY KEY, card_slug TEXT NOT NULL);
             CREATE TABLE brain.prompt_templates (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
             CREATE TABLE brain.ai_calls (
               id INTEGER PRIMARY KEY, ts TEXT NOT NULL, prompt_template_id INTEGER NOT NULL,
               cached INTEGER NOT NULL DEFAULT 0, model_id TEXT, outcome TEXT NOT NULL,
               outcome_class TEXT, contract_slug TEXT
             );
             CREATE TABLE brain.heimdall_template_categories (template_base TEXT PRIMARY KEY, category TEXT NOT NULL);
             CREATE TABLE brain.heimdall_evidence (
               id INTEGER PRIMARY KEY AUTOINCREMENT, model_slug TEXT NOT NULL, category TEXT NOT NULL,
               template_base TEXT, provenance TEXT NOT NULL, outcome TEXT NOT NULL, ai_call_id INTEGER
             );
             CREATE UNIQUE INDEX brain.uq_heimdall_evidence_call ON heimdall_evidence(ai_call_id, provenance, outcome);
             CREATE TABLE judgments.judgment_cases (
               card_slug TEXT NOT NULL, scope_kind TEXT NOT NULL, scope_ref TEXT, verdict TEXT,
               goal_line TEXT, updated_at TEXT
             );",
        )
        .unwrap();
        c
    }

    fn evidence_rows(c: &Connection) -> Vec<(String, String, Option<String>, String, String, Option<i64>)> {
        let mut stmt = c
            .prepare("SELECT model_slug, category, template_base, provenance, outcome, ai_call_id FROM brain.heimdall_evidence ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    #[test]
    fn call_level_classifies_clean_vs_fail_excludes_cached_and_provider_error() {
        let c = fresh();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id, name) VALUES (1,'chat.v1'), (2,'task.v2'), (3,'ignored.v1');
             INSERT INTO brain.heimdall_template_categories (template_base, category) VALUES ('chat','conversational');
             INSERT INTO brain.ai_calls (id, ts, prompt_template_id, cached, model_id, outcome, outcome_class) VALUES
               (1,'2026-01-05 00:00:00',1,0,'model-a','success','clean'),
               (2,'2026-01-05 00:00:00',1,0,'model-a','error','degenerate'),
               (3,'2026-01-05 00:00:00',2,0,'model-b','error','truncated'),
               (4,'2026-01-05 00:00:00',2,0,'model-b','error','validation_failed'),
               (5,'2026-01-05 00:00:00',3,1,'model-c','success','clean'),
               (6,'2026-01-05 00:00:00',3,0,NULL,'success','clean'),
               (7,'2026-01-05 00:00:00',3,0,'model-c','success',NULL),
               (8,'2026-01-05 00:00:00',3,0,'model-c','error','provider_error');",
        )
        .unwrap();

        let r = heimdallAttribute(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "call_rows": 4, "terminal_rows": 0, "burn_rows": 0 }));

        let rows = evidence_rows(&c);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0], ("model-a".into(), "conversational".into(), Some("chat".into()), "prod_gate".into(), "clean".into(), Some(1)));
        assert_eq!(rows[1], ("model-a".into(), "conversational".into(), Some("chat".into()), "prod_gate".into(), "fail".into(), Some(2)));
        // 'task' is unmapped -> falls back to 'general' via cat_for's memoized miss
        assert_eq!(rows[2], ("model-b".into(), "general".into(), Some("task".into()), "prod_gate".into(), "fail".into(), Some(3)));
        assert_eq!(rows[3], ("model-b".into(), "general".into(), Some("task".into()), "prod_gate".into(), "fail".into(), Some(4)));
    }

    #[test]
    fn terminal_level_ships_only_calls_on_a_done_card() {
        let c = fresh();
        c.execute_batch(
            "INSERT INTO cards (slug, status) VALUES ('CARD-A','done'), ('CARD-B','backlog');
             INSERT INTO contracts (slug, card_slug) VALUES ('CT-A','CARD-A'), ('CT-B','CARD-B');
             INSERT INTO brain.prompt_templates (id, name) VALUES (1,'ship.v1');
             INSERT INTO brain.ai_calls (id, ts, prompt_template_id, cached, model_id, outcome, contract_slug) VALUES
               (10,'2026-01-05 00:00:00',1,0,'model-a','success','CT-A'),
               (11,'2026-01-05 00:00:00',1,0,'model-a','success','CT-B'),
               (12,'2026-01-05 00:00:00',1,0,'model-a','error','CT-A'),
               (13,'2026-01-05 00:00:00',1,1,'model-a','success','CT-A'),
               (14,'2026-01-05 00:00:00',1,0,NULL,'success','CT-A');",
        )
        .unwrap();

        let r = heimdallAttribute(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "call_rows": 0, "terminal_rows": 1, "burn_rows": 0 }));
        let rows = evidence_rows(&c);
        assert_eq!(rows, vec![("model-a".into(), "general".into(), Some("ship".into()), "prod_terminal".into(), "ship".into(), Some(10))]);
    }

    #[test]
    fn idempotent_rerun_inserts_nothing_twice() {
        let c = fresh();
        c.execute_batch(
            "INSERT INTO brain.prompt_templates (id, name) VALUES (1,'chat.v1');
             INSERT INTO brain.heimdall_template_categories (template_base, category) VALUES ('chat','conversational');
             INSERT INTO brain.ai_calls (id, ts, prompt_template_id, cached, model_id, outcome, outcome_class) VALUES
               (1,'2026-01-05 00:00:00',1,0,'model-a','success','clean');
             INSERT INTO brain.heimdall_evidence (model_slug, category, template_base, provenance, outcome, ai_call_id)
               VALUES ('model-a','conversational','chat','prod_gate','clean',1);",
        )
        .unwrap();

        let r = heimdallAttribute(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "call_rows": 0, "terminal_rows": 0, "burn_rows": 0 }));
        assert_eq!(evidence_rows(&c).len(), 1); // still just the pre-seeded row
    }

    #[test]
    fn burn_level_splits_design_vs_code_authors_filters_ts_and_skips_contractless_cards() {
        let c = fresh();
        c.execute_batch(
            "INSERT INTO contracts (slug, card_slug) VALUES ('CT-DESIGN','CARD-X'), ('CT-CODE','CARD-Y');
             INSERT INTO judgments.judgment_cases (card_slug, scope_kind, scope_ref, verdict, goal_line, updated_at) VALUES
               ('CARD-X','blueprint','design-alpha','fail','no parens here','2026-01-10 00:00:00'),
               ('CARD-Y','blueprint','code-beta','fail','plain text','2026-01-10 00:00:00'),
               ('CARD-Z','blueprint','design-gamma','fail','plain','2026-01-10 00:00:00'),
               ('CARD-X','blueprint','design-delta','fail','(GATE) parenthesized','2026-01-10 00:00:00'),
               ('CARD-X','blueprint','design-epsilon','pass','plain','2026-01-10 00:00:00'),
               ('CARD-X','swarm','design-zeta','fail','plain','2026-01-10 00:00:00');
             INSERT INTO brain.prompt_templates (id, name) VALUES
               (1,'blueprint.skeleton.v1'), (2,'blueprint.stub.v2'), (3,'blueprint.snippet.author.v1'),
               (4,'tl.author.file.v3'), (5,'dev.implement.code.v1'), (6,'unrelated.template.v1');
             INSERT INTO brain.ai_calls (id, ts, prompt_template_id, cached, model_id, outcome, contract_slug) VALUES
               (101,'2026-01-05 00:00:00',1,0,'model-d1','error','CT-DESIGN'),
               (102,'2026-01-05 00:00:00',6,0,'model-d2','error','CT-DESIGN'),
               (103,'2026-01-15 00:00:00',1,0,'model-d3','error','CT-DESIGN'),
               (104,'2026-01-05 00:00:00',1,1,'model-d4','error','CT-DESIGN'),
               (105,'2026-01-05 00:00:00',1,0,NULL,'error','CT-DESIGN'),
               (201,'2026-01-05 00:00:00',3,0,'model-c1','error','CT-CODE'),
               (202,'2026-01-05 00:00:00',4,0,'model-c2','error','CT-CODE'),
               (203,'2026-01-05 00:00:00',5,0,'model-c3','error','CT-CODE'),
               (204,'2026-01-05 00:00:00',1,0,'model-c4','error','CT-CODE');",
        )
        .unwrap();

        let r = heimdallAttribute(&json!({}), &c).unwrap();
        assert_eq!(r, json!({ "ok": true, "call_rows": 0, "terminal_rows": 0, "burn_rows": 4 }));
        let rows = evidence_rows(&c);
        let ids: Vec<Option<i64>> = rows.iter().map(|r| r.5).collect();
        assert_eq!(ids, vec![Some(101), Some(201), Some(202), Some(203)]);
        assert!(rows.iter().all(|r| r.3 == "prod_court" && r.4 == "burn"));
    }
}
